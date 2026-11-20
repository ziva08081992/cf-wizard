//! Lazy content-addressed materialization, not a clone or repository ZIP.
use crate::{
    canonical::{GitInputs, InputEntry},
    git::{
        Connection, Entry, EntryKind, GitProvider, RecursiveFallback, RecursiveListing,
        file_read_limit, verify_blob,
    },
    git_recursive::{self, VerifiedTrees},
    git_tree_cache::{TreeCache, TreeEntries, checked},
    source::{canonical_future, catalog, no_links, normal_path, relative_path},
};
use cf_application::profile;
use cf_application::{Cancellation, MetadataSource, PrefetchProgress, SourceEntry};
use cf_domain::{Catalog, Error, Provider, Result};
use serde::Serialize;
#[cfg(test)]
#[path = "git_inventory_tests.rs"]
mod inventory_tests;
#[cfg(test)]
#[path = "git_source_tests.rs"]
mod tests;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::{Read, Write},
    ops::Deref,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
};

fn atomic(path: &Path, data: &[u8], c: &Cancellation) -> Result<()> {
    c.check()?;
    no_links(path)?;
    let parent = path
        .parent()
        .ok_or_else(|| crate::external("Нет папки кэша"))?;
    fs::create_dir_all(parent).map_err(crate::external)?;
    no_links(parent)?;
    let mut file = tempfile::NamedTempFile::new_in(parent).map_err(crate::external)?;
    file.write_all(data).map_err(crate::external)?;
    file.as_file().sync_all().map_err(crate::external)?;
    commit_file(file, path, c)
}

fn commit_file(file: tempfile::NamedTempFile, path: &Path, c: &Cancellation) -> Result<()> {
    c.check()?;
    file.persist(path).map_err(crate::external)?;
    Ok(())
}
const MAX_HINT_BLOB_BYTES: u64 = 16 * 1024 * 1024;

enum CachedBlob {
    Shared(Vec<u8>),
    Legacy(Vec<u8>),
}

type SharedTree = Arc<Tree>;

struct Tree {
    entries: TreeEntries,
    hints: Option<SizeHints>,
}

struct SizeHints {
    oid: String,
    refresh: Mutex<()>,
    live: OnceLock<SharedTree>,
}

impl Tree {
    fn new(entries: TreeEntries, hinted_oid: Option<&str>) -> SharedTree {
        Arc::new(Self {
            entries,
            hints: hinted_oid.map(|oid| SizeHints {
                oid: oid.into(),
                refresh: Mutex::new(()),
                live: OnceLock::new(),
            }),
        })
    }

    fn current(&self) -> &Self {
        self.hints
            .as_ref()
            .and_then(|hints| hints.live.get().map(Arc::as_ref))
            .unwrap_or(self)
    }
}

impl Deref for Tree {
    type Target = TreeEntries;

    fn deref(&self) -> &TreeEntries {
        &self.entries
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum TreeUse {
    Open,
    Source,
    Inventory(usize),
    SizeRefresh,
}

impl TreeUse {
    fn reason(self) -> Option<&'static str> {
        match self {
            Self::Open => Some("provider.tree.bootstrap"),
            Self::Source | Self::Inventory(_) => None,
            Self::SizeRefresh => Some("provider.tree.size_refresh"),
        }
    }
}

type SharedTreeResult = std::result::Result<SharedTree, Arc<Error>>;

/// Only this window shares errors. A later user operation may explicitly retry;
/// no failed or partial directory is inserted into the checked source caches.
#[derive(Default)]
struct InventoryFetches {
    trees: Mutex<BTreeMap<String, Arc<OnceLock<SharedTreeResult>>>>,
}
fn copied_error(error: &Error) -> Error {
    match error {
        Error::Cancelled => Error::Cancelled,
        Error::Invalid(message) => Error::Invalid(message.clone()),
        Error::External(message) => Error::External(message.clone()),
    }
}

struct Files {
    client: Arc<dyn GitProvider>,
    connection: Connection,
    revision: String,
    root: PathBuf,
    blobs: PathBuf,
    counters: profile::SourceCounters,
    trees: Mutex<BTreeMap<String, SharedTree>>,
    oid_trees: Mutex<BTreeMap<String, SharedTree>>,
    tree_cache: Option<TreeCache>,
    recursive_disabled: AtomicBool,
    #[cfg(test)]
    inventory_workers: std::sync::atomic::AtomicUsize,
}
impl Files {
    fn persist_tree(
        &self,
        cache: &TreeCache,
        oid: &str,
        entries: &TreeEntries,
        c: &Cancellation,
    ) -> Result<()> {
        let mut write = profile::source_span(
            Some(&self.counters),
            "cache.tree_disk_write",
            "source",
            0,
            None,
        );
        match cache.write(oid, entries, c) {
            Ok(()) => write.success(),
            Err(Error::Cancelled) => return Err(Error::Cancelled),
            Err(_) => profile::note("cache.tree_disk_write_error", "source", 1),
        }
        Ok(())
    }

    fn provider_tree(
        &self,
        oid: &str,
        c: &Cancellation,
        usage: TreeUse,
        anchor: bool,
    ) -> Result<SharedTree> {
        c.check()?;
        let fetched = {
            let mut request =
                profile::source_span(Some(&self.counters), "provider.tree", "source", 0, None);
            let mut reason = usage
                .reason()
                .map(|name| profile::span(name, "provider.tree", 0, None));
            let fetched = self.client.tree(oid, c);
            if fetched.is_ok() {
                request.success();
                if let Some(reason) = &mut reason {
                    reason.success();
                }
            }
            fetched
        }?;
        c.check()?;
        let entries = match &self.tree_cache {
            Some(cache) if !anchor => {
                let entries = cache.validate(oid, fetched, c)?;
                if usage != TreeUse::Open {
                    self.persist_tree(cache, oid, &entries, c)?;
                }
                entries
            }
            _ => checked(fetched)?,
        };
        c.check()?;
        Ok(Tree::new(entries, None))
    }

    fn disk_tree(&self, oid: &str, c: &Cancellation) -> Result<Option<SharedTree>> {
        let Some(cache) = &self.tree_cache else {
            return Ok(None);
        };
        let mut load = profile::source_span(
            Some(&self.counters),
            "cache.tree_disk_load",
            "source",
            0,
            None,
        );
        let result = cache.read(oid, c);
        c.check()?;
        match result {
            Ok(Some(entries)) => {
                load.success();
                profile::note("cache.tree_disk_hit", "source", 1);
                Ok(Some(Tree::new(entries, Some(oid))))
            }
            Ok(None) => {
                load.success();
                profile::note("cache.tree_disk_miss", "source", 1);
                Ok(None)
            }
            Err(Error::Cancelled) => Err(Error::Cancelled),
            Err(_) => {
                profile::note("cache.tree_disk_rejected", "source", 1);
                Ok(None)
            }
        }
    }

    fn authoritative_tree(
        &self,
        tree: &SharedTree,
        c: &Cancellation,
        usage: TreeUse,
    ) -> Result<SharedTree> {
        c.check()?;
        let Some(hints) = &tree.hints else {
            return Ok(tree.clone());
        };
        if let Some(live) = hints.live.get() {
            return Ok(live.clone());
        }
        let _refresh = hints.refresh.lock().map_err(crate::external)?;
        c.check()?;
        if let Some(live) = hints.live.get() {
            return Ok(live.clone());
        }
        let live = self.provider_tree(&hints.oid, c, usage, false)?;
        c.check()?;
        let _ = hints.live.set(live.clone());
        Ok(live)
    }

    fn publish_subtree(
        &self,
        cache: &TreeCache,
        oid: &str,
        mut trees: VerifiedTrees,
        fetches: &InventoryFetches,
        c: &Cancellation,
    ) -> Result<SharedTree> {
        let root = trees
            .remove(oid)
            .ok_or_else(|| Error::External("В проверенном поддереве Git нет корня".into()))?;
        let root = Tree::new(root, Some(oid));
        let mut added = Vec::new();
        {
            let flights = fetches.trees.lock().map_err(crate::external)?;
            let mut known = self.oid_trees.lock().map_err(crate::external)?;
            c.check()?;
            for (id, entries) in trees {
                if !flights.contains_key(&id) && !known.contains_key(&id) {
                    let tree = Tree::new(entries, Some(&id));
                    known.insert(id.clone(), Arc::clone(&tree));
                    added.push((id, tree));
                }
            }
        }
        // The caller completes the existing root flight. I/O has no map lock
        // and never waits for another subtree's in-flight initializer.
        let result = cache.write_batch(
            added
                .iter()
                .map(|(id, tree)| (id.as_str(), &tree.entries))
                .chain(std::iter::once((oid, &root.entries))),
            c,
        );
        match result {
            Ok(_) => {}
            Err(Error::Cancelled) => return Err(Error::Cancelled),
            Err(_) => profile::note("cache.tree_disk_write_error", "source", 1),
        }
        Ok(root)
    }

    fn recursive_tree(
        &self,
        oid: &str,
        prefix_depth: usize,
        fetches: &InventoryFetches,
        c: &Cancellation,
    ) -> Result<Option<SharedTree>> {
        let Some(cache) = self.tree_cache.as_ref() else {
            return Ok(None);
        };
        if self.recursive_disabled.load(Ordering::Acquire) {
            return Ok(None);
        }
        let listing = profile::source_measure(
            Some(&self.counters),
            "provider.tree",
            "source",
            0,
            None,
            || {
                profile::source_measure(
                    Some(&self.counters),
                    "provider.tree_recursive",
                    "provider.tree",
                    0,
                    None,
                    || self.client.recursive_tree(oid, c),
                )
            },
        )?;
        c.check()?;
        let reason = match listing {
            RecursiveListing::Complete(rows) => {
                let row_count = rows.len();
                match git_recursive::validate(oid, rows, prefix_depth, c)? {
                    RecursiveListing::Complete(trees) => {
                        profile::note("provider.tree_recursive_verified_rows", "source", row_count);
                        profile::note(
                            "provider.tree_recursive_verified_trees",
                            "source",
                            trees.len(),
                        );
                        return self
                            .publish_subtree(cache, oid, trees, fetches, c)
                            .map(Some);
                    }
                    RecursiveListing::ProofIncomplete(reason) => Some(reason),
                    RecursiveListing::Unsupported => None,
                }
            }
            RecursiveListing::ProofIncomplete(reason) => Some(reason),
            RecursiveListing::Unsupported => None,
        };
        let disable = if let Some(reason) = reason {
            profile::note(reason.metric(), "source", 1);
            !matches!(
                reason,
                RecursiveFallback::PageLimit
                    | RecursiveFallback::RowLimit
                    | RecursiveFallback::BodyLimit
                    | RecursiveFallback::PathLimit
                    | RecursiveFallback::DepthLimit
            )
        } else {
            profile::note("provider.tree_recursive_fallback_unsupported", "source", 1);
            true
        };
        if disable {
            self.recursive_disabled.store(true, Ordering::Release);
        }
        Ok(None)
    }

    fn tree(
        &self,
        oid: &str,
        c: &Cancellation,
        fetches: Option<&InventoryFetches>,
        usage: TreeUse,
    ) -> Result<SharedTree> {
        let load = || {
            c.check()?;
            let known = self
                .oid_trees
                .lock()
                .map_err(crate::external)?
                .get(oid)
                .cloned();
            if let Some(known) = known {
                profile::note("cache.tree_oid_hit", "source", 1);
                return if usage == TreeUse::Open {
                    self.authoritative_tree(&known, c, usage)
                } else {
                    Ok(known)
                };
            }
            let disk = if usage == TreeUse::Open {
                None
            } else {
                self.disk_tree(oid, c)?
            };
            let value = match disk {
                Some(tree) => tree,
                None => {
                    let recursive = match (usage, fetches) {
                        (TreeUse::Inventory(depth), Some(fetches)) => {
                            self.recursive_tree(oid, depth, fetches, c)?
                        }
                        _ => None,
                    };
                    match recursive {
                        Some(tree) => tree,
                        None => self.provider_tree(oid, c, usage, false)?,
                    }
                }
            };
            c.check()?;
            let mut known = self.oid_trees.lock().map_err(crate::external)?;
            Ok(Arc::clone(known.entry(oid.into()).or_insert(value)))
        };
        let Some(fetches) = fetches else {
            return load();
        };
        let flight = {
            let mut rows = fetches.trees.lock().map_err(crate::external)?;
            Arc::clone(rows.entry(oid.into()).or_default())
        };
        flight
            .get_or_init(|| {
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(load))
                    .unwrap_or_else(|_| {
                        Err(Error::External(
                            "Аварийная ошибка получения каталога Git".into(),
                        ))
                    })
                    .map_err(Arc::new)
            })
            .clone()
            .map_err(|error| copied_error(&error))
    }

    fn directory(&self, path: &str, c: &Cancellation) -> Result<SharedTree> {
        self.directory_with(path, c, None, TreeUse::Source)
    }

    fn directory_with(
        &self,
        path: &str,
        c: &Cancellation,
        fetches: Option<&InventoryFetches>,
        usage: TreeUse,
    ) -> Result<SharedTree> {
        c.check()?;
        let known = self
            .trees
            .lock()
            .map_err(crate::external)?
            .get(path)
            .cloned();
        if let Some(known) = known {
            profile::note("cache.tree_path_hit", "source", 1);
            return if usage == TreeUse::Open {
                self.authoritative_tree(&known, c, usage)
            } else {
                Ok(known)
            };
        }
        let (parent, name) = path.rsplit_once('/').unwrap_or(("", path));
        let ancestor_use = match usage {
            TreeUse::Inventory(_) => TreeUse::Source,
            _ => usage,
        };
        let entries = self.directory_with(parent, c, fetches, ancestor_use)?;
        let value = match entries.get(name) {
            Some(e) if e.kind == EntryKind::Tree => self.tree(&e.oid, c, fetches, usage)?,
            _ => Tree::new(BTreeMap::new(), None),
        };
        c.check()?;
        self.trees
            .lock()
            .map_err(crate::external)?
            .insert(path.into(), Arc::clone(&value));
        Ok(value)
    }
    fn below_with(
        &self,
        path: &str,
        c: &Cancellation,
        fetches: Option<&InventoryFetches>,
    ) -> Result<Vec<String>> {
        let mut pending = vec![path.to_owned()];
        let mut out = vec![];
        while let Some(dir) = pending.pop() {
            c.check()?;
            let depth = dir.split('/').count();
            if depth > 128 {
                return Err(Error::Invalid("Слишком глубокое дерево Git".into()));
            }
            let usage = if fetches.is_some() {
                TreeUse::Inventory(depth)
            } else {
                TreeUse::Source
            };
            for e in self.directory_with(&dir, c, fetches, usage)?.values() {
                let rel = format!("{dir}/{}", e.name);
                if e.kind == EntryKind::Tree {
                    pending.push(rel);
                } else {
                    out.push(rel);
                }
            }
        }
        out.sort();
        Ok(out)
    }
    /// A complete warm walk is returned directly, with no worker dispatch or
    /// second traversal. Unknown paths remain unknown, never inferred absent.
    fn known_below(&self, path: &str, c: &Cancellation) -> Result<Option<Vec<String>>> {
        let trees = self.trees.lock().map_err(crate::external)?;
        let mut pending = vec![path.to_owned()];
        let mut out = vec![];
        while let Some(dir) = pending.pop() {
            c.check()?;
            if dir.split('/').count() > 128 {
                return Err(Error::Invalid("Слишком глубокое дерево Git".into()));
            }
            let Some(entries) = trees.get(&dir) else {
                return Ok(None);
            };
            for e in entries.values() {
                let rel = format!("{dir}/{}", e.name);
                if e.kind == EntryKind::Tree {
                    pending.push(rel);
                } else {
                    out.push(rel);
                }
            }
        }
        out.sort();
        Ok(Some(out))
    }
    fn blob(&self, path: &str, c: &Cancellation) -> Result<Option<Entry>> {
        relative_path(path)?;
        let (parent, name) = path.rsplit_once('/').unwrap_or(("", path));
        Ok(self
            .directory(parent, c)?
            .current()
            .get(name)
            .filter(|e| e.kind == EntryKind::Blob)
            .cloned())
    }

    fn cached(&self, path: &str, e: &Entry, size_hint: bool) -> Result<Option<CachedBlob>> {
        if size_hint && e.size > MAX_HINT_BLOB_BYTES {
            return Ok(None);
        }
        for (p, shared) in [
            (self.blobs.join(&e.oid), true),
            (self.root.join(path), false),
        ] {
            no_links(&p)?;
            if p.is_file() && fs::metadata(&p).map_err(crate::external)?.len() == e.size {
                let data = profile::measured_bytes(
                    Some(&self.counters),
                    "cache.read",
                    "source.read",
                    Some(path),
                    || {
                        if !size_hint {
                            return fs::read(&p);
                        }
                        let mut data = Vec::new();
                        fs::File::open(&p)?
                            .take(e.size + 1)
                            .read_to_end(&mut data)?;
                        Ok(data)
                    },
                )
                .map_err(crate::external)?;
                if verify_blob(&data, &e.oid, e.size).is_ok() {
                    profile::note("cache.file_hit", "source.read", 1);
                    profile::note(
                        if shared {
                            "cache.payload_shared_hit"
                        } else {
                            "cache.payload_legacy_hit"
                        },
                        "source.read",
                        1,
                    );
                    return Ok(Some(if shared {
                        CachedBlob::Shared(data)
                    } else {
                        CachedBlob::Legacy(data)
                    }));
                }
            }
        }
        Ok(None)
    }

    fn resolve_blob(&self, path: &str, c: &Cancellation) -> Result<(Entry, Option<CachedBlob>)> {
        relative_path(path)?;
        let (parent, name) = path.rsplit_once('/').unwrap_or(("", path));
        let tree = self.directory(parent, c)?;
        let current = tree.current();
        let missing = || Error::Invalid(format!("Файла нет в закреплённой ревизии: {path}"));
        let entry = current
            .get(name)
            .filter(|e| e.kind == EntryKind::Blob)
            .ok_or_else(missing)?;
        let size_hint = current.hints.is_some();
        let data = self.cached(path, entry, size_hint)?;
        c.check()?;
        if data.is_some() || !size_hint {
            return Ok((entry.clone(), data));
        }
        // The tree establishes presence and OID, but Git does not hash blob sizes
        // in tree objects. Never pass a size hint to a network provider.
        let live = self.authoritative_tree(&tree, c, TreeUse::SizeRefresh)?;
        let entry_live = live
            .get(name)
            .filter(|e| e.kind == EntryKind::Blob && e.oid == entry.oid)
            .ok_or_else(|| Error::Invalid("Изменился файл в закреплённом дереве Git".into()))?;
        let data = if entry.size == entry_live.size && entry.size <= MAX_HINT_BLOB_BYTES {
            None
        } else {
            self.cached(path, entry_live, false)?
        };
        c.check()?;
        Ok((entry_live.clone(), data))
    }

    fn put(&self, path: &str, e: &Entry, data: &[u8], c: &Cancellation) -> Result<()> {
        c.check()?;
        verify_blob(data, &e.oid, e.size)?;
        c.check()?;
        let p = self.blobs.join(&e.oid);
        no_links(&p)?;
        let valid = p.is_file()
            && fs::metadata(&p).map_err(crate::external)?.len() == e.size
            && profile::measured_bytes(
                Some(&self.counters),
                "cache.read",
                "source.read",
                Some(path),
                || fs::read(&p),
            )
            .is_ok_and(|v| verify_blob(&v, &e.oid, e.size).is_ok());
        if !valid {
            let mut write = profile::source_span(
                Some(&self.counters),
                "cache.write",
                "source.read",
                data.len(),
                Some(path),
            );
            match atomic(&p, data, c) {
                Ok(()) => profile::note("cache.payload_published", "source.read", 1),
                Err(Error::Cancelled) => return Err(Error::Cancelled),
                Err(error) => {
                    c.check()?;
                    // Windows may reject replacement while another verified
                    // publisher/reader holds this OID. Reuse only a proved winner.
                    no_links(&p)?;
                    let winner = p.is_file()
                        && fs::metadata(&p).is_ok_and(|metadata| metadata.len() == e.size)
                        && profile::measured_bytes(
                            Some(&self.counters),
                            "cache.read",
                            "source.read",
                            Some(path),
                            || fs::read(&p),
                        )
                        .is_ok_and(|bytes| verify_blob(&bytes, &e.oid, e.size).is_ok());
                    c.check()?;
                    if !winner {
                        return Err(error);
                    }
                    profile::note("cache.payload_publish_race_reused", "source.read", 1);
                }
            }
            write.success();
        } else {
            c.check()?;
        }
        Ok(())
    }
    fn read(&self, path: &str, c: &Cancellation) -> Result<Vec<u8>> {
        profile::measured_bytes(
            Some(&self.counters),
            "source.read",
            "engine",
            Some(path),
            || {
                c.check()?;
                let (e, data) = self.resolve_blob(path, c)?;
                let legacy = matches!(&data, Some(CachedBlob::Legacy(_)));
                let bytes = match data {
                    Some(CachedBlob::Shared(bytes)) => {
                        c.check()?;
                        return Ok(bytes);
                    }
                    Some(CachedBlob::Legacy(bytes)) => bytes,
                    None => {
                        file_read_limit(e.size)?;
                        profile::note("cache.file_miss", "source.read", 1);
                        profile::measured_bytes(
                            Some(&self.counters),
                            "provider.file",
                            "source.read",
                            None,
                            || {
                                self.client.file(
                                    &self.revision,
                                    &format!("{}/{path}", self.connection.directory),
                                    &e,
                                    c,
                                )
                            },
                        )?
                    }
                };
                c.check()?;
                self.put(path, &e, &bytes, c)?;
                if legacy {
                    profile::note("cache.payload_legacy_promoted", "source.read", 1);
                }
                Ok(bytes)
            },
        )
    }
}
pub struct GitSource {
    files: Files,
    catalog: Catalog,
    manifest: GitInputs,
    identity: String,
}
impl GitSource {
    #[cfg(test)]
    pub(crate) fn set_test_source_counters(&mut self, counters: profile::SourceCounters) {
        self.files.counters = counters;
    }

    pub fn open(
        connection: &Connection,
        client: Arc<dyn GitProvider>,
        c: &Cancellation,
    ) -> Result<Self> {
        let connection = connection.checked()?;
        let revision = client.head(&connection.branch, c)?;
        let anchor = client.root(&revision, c)?;
        let cache = canonical_future(Path::new(&connection.cache))?;
        if cache
            .components()
            .any(|p| p.as_os_str().eq_ignore_ascii_case(".git"))
        {
            return Err(Error::Invalid("Кэш должен находиться вне .git".into()));
        }
        let repo = connection.repo_key();
        let branch = connection.source_key();
        let branch = branch.trim_start_matches("git:");
        let root = cache
            .join(&repo)
            .join(branch)
            .join(&revision)
            .join("config");
        no_links(&root)?;
        let tree_cache = (connection.provider()? == Provider::Gitea)
            .then(|| TreeCache::new(&cache, &connection, &revision))
            .transpose()?;
        let files = Files {
            counters: profile::SourceCounters::new(profile::Context::enabled()),
            client,
            connection: connection.clone(),
            revision: revision.clone(),
            blobs: cache.join(&repo).join("blobs"),
            root: root.clone(),
            trees: Mutex::new(BTreeMap::new()),
            oid_trees: Mutex::new(BTreeMap::new()),
            tree_cache,
            recursive_disabled: AtomicBool::new(false),
            #[cfg(test)]
            inventory_workers: std::sync::atomic::AtomicUsize::new(0),
        };
        // Gitea's root address is the commit alias, not a Git tree OID.
        let mut root_tree = files.provider_tree(&anchor, c, TreeUse::Open, true)?;
        for part in connection.directory.split('/') {
            let entry = root_tree
                .get(part)
                .ok_or_else(|| Error::Invalid("Каталог XML-выгрузки не найден в Git".into()))?;
            if entry.kind != EntryKind::Tree {
                return Err(Error::Invalid(
                    "Вместо каталога выгрузки указан файл".into(),
                ));
            }
            root_tree = files.tree(&entry.oid, c, None, TreeUse::Open)?;
        }
        files
            .trees
            .lock()
            .map_err(crate::external)?
            .insert("".into(), root_tree);
        let catalog = catalog(&files.read("Configuration.xml", c)?)?;
        let owners = catalog
            .objects
            .values()
            .map(|o| (o.descriptor.clone(), o.key.0.clone()))
            .collect::<BTreeMap<_, _>>();
        let mut paths = owners.keys().cloned().collect::<BTreeSet<_>>();
        paths.insert("Configuration.xml".into());
        for (name, e) in files
            .directory_with("Documents", c, None, TreeUse::Open)?
            .iter()
        {
            if e.kind == EntryKind::Blob && name.ends_with(".xml") {
                paths.insert(format!("Documents/{name}"));
            }
        }
        let parents: BTreeSet<_> = paths
            .iter()
            .filter_map(|path| path.rsplit_once('/').map(|(parent, _)| parent))
            .collect();
        for parent in parents {
            files.directory_with(parent, c, None, TreeUse::Open)?;
        }
        let mut entries = vec![];
        for path in paths {
            let e = files
                .blob(&path, c)?
                .ok_or_else(|| Error::Invalid(format!("Нет XML-описания: {path}")))?;
            let mut roles = vec![];
            if path == "Configuration.xml" {
                roles.push("configuration");
            }
            if owners.contains_key(&path) {
                roles.push("descriptor");
            }
            if path.starts_with("Documents/") && path.matches('/').count() == 1 {
                roles.push("registrar");
            }
            entries.push(InputEntry {
                owner: owners.get(&path).cloned().unwrap_or_default(),
                path,
                oid: e.oid,
                size: e.size,
                roles: roles.join(","),
            });
        }
        let manifest = GitInputs {
            source_key: connection.source_key(),
            revision,
            entries,
        };
        manifest.validate()?;
        let identity = normal_path(&root);
        Ok(Self {
            files,
            catalog,
            manifest,
            identity,
        })
    }
    pub fn revision(&self) -> &str {
        &self.manifest.revision
    }
    pub fn preference_key(&self) -> &str {
        &self.manifest.source_key
    }
    pub fn client(&self) -> Arc<dyn GitProvider> {
        self.files.client.clone()
    }
    pub fn connection(&self) -> &Connection {
        &self.files.connection
    }
    pub fn search(
        &self,
        task: &str,
        days: Option<u32>,
        cancel: &Cancellation,
        progress: &dyn Fn(usize),
    ) -> Result<TaskResult> {
        let task = normalize_commit_query(task)?;
        let matcher = commit_matcher(task)?;
        if days == Some(0) {
            return Err(Error::Invalid(
                "Количество дней должно быть положительным целым числом".into(),
            ));
        }
        let now = chrono::Utc::now();
        let until = now.to_rfc3339();
        let since = days
            .map(|n| {
                now.checked_sub_signed(chrono::Duration::days(n as i64))
                    .ok_or_else(|| {
                        Error::Invalid("Период выходит за поддерживаемый диапазон дат".into())
                    })
            })
            .transpose()?
            .map(|t| t.to_rfc3339());
        let commits = self.files.client.commits(
            self.revision(),
            since.as_deref(),
            &until,
            cancel,
            progress,
        )?;
        let mut selected = BTreeSet::new();
        let mut missing = BTreeSet::new();
        let mut paths = BTreeSet::<String>::new();
        let mut hits = vec![];
        let prefix = format!("{}/", self.files.connection.directory);
        for commit in &commits {
            cancel.check()?;
            if !matcher.is_match(&commit.message) {
                continue;
            }
            hits.push(commit.oid.clone());
            for path in
                self.files
                    .client
                    .changed(&commit.oid, &self.files.connection.directory, cancel)?
            {
                if let Some(rel) = path.strip_prefix(&prefix) {
                    paths.insert(rel.into());
                }
            }
        }
        let owners = self
            .catalog
            .objects
            .values()
            .map(|o| (o.descriptor.as_str(), o))
            .collect::<BTreeMap<_, _>>();
        for path in &paths {
            if path == "Configuration.xml" {
                continue;
            }
            let parent = path.split('/').take(2).collect::<Vec<_>>().join("/") + ".xml";
            let found = owners
                .get(path.as_str())
                .or_else(|| owners.get(parent.as_str()));
            if let Some(o) = found {
                if o.key.kind() != "Language" {
                    selected.insert(o.key.0.clone());
                }
            } else {
                missing.insert(path.clone());
            }
        }
        Ok(TaskResult {
            objects: selected.into_iter().collect(),
            missing: missing.into_iter().collect(),
            commits: hits,
            files: paths.len(),
            scanned: commits.len(),
            revision: self.revision().into(),
        })
    }
}
/// Validate raw input before trimming so edge controls cannot disappear.
pub fn normalize_commit_query(query: &str) -> Result<&str> {
    if query.chars().any(char::is_control) {
        return Err(Error::Invalid(
            "Комментарий для поиска не должен содержать управляющие символы".into(),
        ));
    }
    let query = query.trim();
    if query.is_empty() {
        return Err(Error::Invalid(
            "Укажите ключ задачи или фрагмент комментария коммита Git".into(),
        ));
    }
    if query.len() > 256 {
        return Err(Error::Invalid(
            "Слишком длинный поисковый текст: максимум 256 байт UTF-8".into(),
        ));
    }
    Ok(query)
}
fn commit_matcher(query: &str) -> Result<regex::Regex> {
    let is_key = query.rsplit_once('-').is_some_and(|(prefix, number)| {
        prefix
            .bytes()
            .next()
            .is_some_and(|b| b.is_ascii_alphabetic())
            && prefix
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_')
            && !number.is_empty()
            && number.bytes().all(|b| b.is_ascii_digit())
    });
    let literal = regex::escape(query);
    let pattern = if is_key {
        format!(r"(?i)(?:^|[^\pL\pN_]){literal}(?:$|[^\pL\pN_])")
    } else {
        format!("(?i){literal}")
    };
    regex::Regex::new(&pattern).map_err(crate::external)
}
#[derive(Clone, Serialize)]
pub struct TaskResult {
    pub objects: Vec<String>,
    pub missing: Vec<String>,
    pub commits: Vec<String>,
    pub files: usize,
    pub scanned: usize,
    pub revision: String,
}
impl MetadataSource for GitSource {
    fn profile_counters(&self) -> Option<profile::SourceCounters> {
        Some(self.files.counters.clone())
    }
    fn manifest(&self) -> Option<&GitInputs> {
        Some(&self.manifest)
    }
    fn prepare_index_cache(&self, c: &Cancellation) -> Result<()> {
        if let Some(cache) = &self.files.tree_cache {
            let report =
                profile::measure("cache.tree_legacy_import", "operation", 0, None, || {
                    cache.import_legacy(c)
                })?;
            profile::note(
                "cache.tree_legacy_import.scanned",
                "operation",
                report.scanned,
            );
            profile::note(
                "cache.tree_legacy_import.imported",
                "operation",
                report.imported,
            );
        }
        c.check()
    }
    fn materialized_root(&self) -> Option<&Path> {
        Some(&self.files.root)
    }
    fn identity(&self) -> &str {
        &self.identity
    }
    fn catalog(&self) -> &Catalog {
        &self.catalog
    }
    fn is_pinned(&self) -> bool {
        true
    }
    fn read(&self, path: &str, c: &Cancellation) -> Result<Vec<u8>> {
        self.files.read(path, c)
    }
    fn prefetch(&self, paths: &[String], c: &Cancellation) -> Result<()> {
        self.prefetch_with_progress(paths, c, &|_| {})
    }
    fn prefetch_with_progress(
        &self,
        paths: &[String],
        c: &Cancellation,
        progress: &dyn Fn(PrefetchProgress),
    ) -> Result<()> {
        profile::source_measure(
            Some(&self.files.counters),
            "source.prefetch",
            "source",
            0,
            None,
            || {
                if !paths.is_empty() {
                    progress(PrefetchProgress::CheckingCache);
                }
                let mut missing = vec![];
                for path in paths {
                    c.check()?;
                    let (e, data) = self.files.resolve_blob(path, c)?;
                    match data {
                        Some(CachedBlob::Shared(_)) => c.check()?,
                        Some(CachedBlob::Legacy(data)) => {
                            c.check()?;
                            self.files.put(path, &e, &data, c)?;
                            profile::note("cache.payload_legacy_promoted", "source.read", 1);
                        }
                        None => {
                            file_read_limit(e.size)?;
                            missing.push((path.clone(), e));
                        }
                    }
                }
                let available = paths.len() - missing.len();
                let mut downloaded = 0;
                let report = |downloaded| {
                    progress(PrefetchProgress::Downloading {
                        ready: available + downloaded,
                        total: paths.len(),
                        downloaded,
                    });
                };
                if !missing.is_empty() {
                    report(downloaded);
                }
                for batch in missing.chunks(20) {
                    let names = batch
                        .iter()
                        .map(|(p, e)| {
                            (
                                format!("{}/{p}", self.files.connection.directory),
                                e.clone(),
                            )
                        })
                        .collect::<Vec<_>>();
                    let mut data = self.files.client.files(self.revision(), &names, c)?;
                    let written = (|| -> Result<()> {
                        for (path, e) in batch {
                            let full = format!("{}/{path}", self.files.connection.directory);
                            let bytes = data.remove(&full).ok_or_else(|| {
                                Error::Invalid("Неполная загрузка XML из Git".into())
                            })?;
                            c.check()?;
                            self.files.put(path, e, &bytes, c)?;
                            downloaded += 1;
                        }
                        Ok(())
                    })();
                    report(downloaded);
                    written?;
                }
                Ok(())
            },
        )
    }
    fn exists(&self, path: &str, c: &Cancellation) -> Result<bool> {
        profile::source_measure(
            Some(&self.files.counters),
            "source.exists",
            "source",
            0,
            Some(path),
            || Ok(self.files.blob(path, c)?.is_some()),
        )
    }
    fn list_directory(&self, path: &str, c: &Cancellation) -> Result<Vec<SourceEntry>> {
        profile::measured_items(
            Some(&self.files.counters),
            "source.list_directory",
            "source",
            Some(path),
            || {
                self.files
                    .directory(path, c)?
                    .values()
                    .map(|e| {
                        Ok(SourceEntry {
                            name: e.name.clone(),
                            directory: e.kind == EntryKind::Tree,
                        })
                    })
                    .collect()
            },
        )
    }
    fn files_below(&self, path: &str, c: &Cancellation) -> Result<Vec<String>> {
        profile::measured_items(
            Some(&self.files.counters),
            "source.files_below",
            "source",
            Some(path),
            || self.files.below_with(path, c, None),
        )
    }
    fn inventory_window(
        &self,
        paths: &[String],
        c: &Cancellation,
    ) -> Result<Option<Vec<Result<Vec<String>>>>> {
        profile::source_measure(
            Some(&self.files.counters),
            "source.inventory_window",
            "engine.finish_inventory",
            0,
            None,
            || {
                if paths.len() > cf_application::INVENTORY_WINDOW_LIMIT {
                    return Err(Error::Invalid("Превышено окно Git inventory".into()));
                }
                c.check()?;
                let mut results: Vec<Option<Result<Vec<String>>>> =
                    (0..paths.len()).map(|_| None).collect();
                let mut cold = vec![];
                for (index, path) in paths.iter().enumerate() {
                    match self.files.known_below(path, c) {
                        Ok(Some(files)) => results[index] = Some(Ok(files)),
                        Ok(None) => cold.push((index, path)),
                        Err(error) => results[index] = Some(Err(error)),
                    }
                }
                if !cold.is_empty() {
                    let fetches = InventoryFetches::default();
                    std::thread::scope(|scope| {
                        let mut jobs = vec![];
                        for (index, path) in cold {
                            let fetches = &fetches;
                            let profile_context = profile::Context::current();
                            let job = std::thread::Builder::new()
                                .name("cf-git-inventory".into())
                                .spawn_scoped(scope, move || {
                                    let _profile_scope = profile_context.enter();
                                    self.files.below_with(path, c, Some(fetches))
                                });
                            match job {
                                Ok(job) => {
                                    #[cfg(test)]
                                    self.files
                                        .inventory_workers
                                        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                                    jobs.push((index, job));
                                }
                                // Thread creation has not started the read. The original
                                // synchronous read is a fallback, not a provider retry.
                                Err(_) => {
                                    results[index] =
                                        Some(self.files.below_with(path, c, Some(fetches)));
                                }
                            }
                        }
                        for (index, job) in jobs {
                            results[index] = Some(job.join().unwrap_or_else(|_| {
                                Err(Error::External(
                                    "Аварийная ошибка обхода Git inventory".into(),
                                ))
                            }));
                        }
                    });
                }
                // No serial omitted-file read can overlap these jobs. Each window is
                // fully joined, so the combined preview tree/file budget stays <= 4.
                c.check()?;
                Ok(Some(
                    results
                        .into_iter()
                        .map(|result| result.expect("every inventory job has joined"))
                        .collect(),
                ))
            },
        )
    }
    fn snapshot(&self, c: &Cancellation) -> Result<String> {
        profile::source_measure(
            Some(&self.files.counters),
            "source.snapshot",
            "source",
            0,
            None,
            || {
                c.check()?;
                Ok(format!(
                    "{}@{}:{}",
                    self.manifest.source_key,
                    self.manifest.revision,
                    profile::measure("source.fingerprint", "source.snapshot", 0, None, || {
                        let fingerprint = self.manifest.fingerprint()?;
                        profile::identity("input_fingerprint", &fingerprint);
                        profile::note(
                            "source.manifest_rows",
                            "source.fingerprint",
                            self.manifest.entries.len(),
                        );
                        Ok::<_, Error>(fingerprint)
                    })?
                ))
            },
        )
    }
}
