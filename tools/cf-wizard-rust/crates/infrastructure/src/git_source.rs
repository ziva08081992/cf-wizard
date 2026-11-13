//! Lazy content-addressed materialization, not a clone or repository ZIP.
use crate::{
    canonical::{GitInputs, InputEntry},
    git::{Connection, Entry, EntryKind, GitProvider, verify_blob},
    source::{canonical_future, catalog, no_links, normal_path, relative_path},
};
use cf_application::{Cancellation, MetadataSource, SourceEntry};
use cf_domain::{Catalog, Error, Result};
use serde::Serialize;
#[cfg(test)]
#[path = "git_source_tests.rs"]
mod tests;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Write,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};
use unicode_casefold::UnicodeCaseFold;

fn checked(entries: Vec<Entry>) -> Result<BTreeMap<String, Entry>> {
    let mut seen = BTreeSet::new();
    let mut result = BTreeMap::new();
    for e in entries {
        relative_path(&e.name)?;
        if e.kind == EntryKind::Unsupported || !seen.insert(e.name.case_fold().collect::<String>())
        {
            return Err(Error::Invalid(
                "Коллизия путей или неподдерживаемый файл в каталоге Git".into(),
            ));
        }
        result.insert(e.name.clone(), e);
    }
    Ok(result)
}
fn atomic(path: &Path, data: &[u8]) -> Result<()> {
    no_links(path)?;
    let parent = path
        .parent()
        .ok_or_else(|| crate::external("Нет папки кэша"))?;
    fs::create_dir_all(parent).map_err(crate::external)?;
    no_links(parent)?;
    let mut file = tempfile::NamedTempFile::new_in(parent).map_err(crate::external)?;
    file.write_all(data).map_err(crate::external)?;
    file.as_file().sync_all().map_err(crate::external)?;
    file.persist(path).map_err(crate::external)?;
    Ok(())
}
struct Files {
    client: Arc<dyn GitProvider>,
    connection: Connection,
    revision: String,
    root: PathBuf,
    blobs: PathBuf,
    trees: Mutex<BTreeMap<String, Arc<BTreeMap<String, Entry>>>>,
}
impl Files {
    fn directory(&self, path: &str, c: &Cancellation) -> Result<Arc<BTreeMap<String, Entry>>> {
        c.check()?;
        if let Some(v) = self.trees.lock().map_err(crate::external)?.get(path) {
            return Ok(v.clone());
        }
        let (parent, name) = path.rsplit_once('/').unwrap_or(("", path));
        let entries = self.directory(parent, c)?;
        let value = match entries.get(name) {
            Some(e) if e.kind == EntryKind::Tree => checked(self.client.tree(&e.oid, c)?)?,
            _ => BTreeMap::new(),
        };
        let value = Arc::new(value);
        self.trees
            .lock()
            .map_err(crate::external)?
            .insert(path.into(), value.clone());
        Ok(value)
    }
    fn blob(&self, path: &str, c: &Cancellation) -> Result<Option<Entry>> {
        relative_path(path)?;
        let (parent, name) = path.rsplit_once('/').unwrap_or(("", path));
        Ok(self
            .directory(parent, c)?
            .get(name)
            .filter(|e| e.kind == EntryKind::Blob)
            .cloned())
    }
    fn cached(&self, path: &str, e: &Entry) -> Result<Option<Vec<u8>>> {
        for p in [self.root.join(path), self.blobs.join(&e.oid)] {
            no_links(&p)?;
            if p.is_file() && fs::metadata(&p).map_err(crate::external)?.len() == e.size {
                let data = fs::read(&p).map_err(crate::external)?;
                if verify_blob(&data, &e.oid, e.size).is_ok() {
                    return Ok(Some(data));
                }
            }
        }
        Ok(None)
    }
    fn put(&self, path: &str, e: &Entry, data: &[u8]) -> Result<()> {
        verify_blob(data, &e.oid, e.size)?;
        for p in [self.blobs.join(&e.oid), self.root.join(path)] {
            no_links(&p)?;
            let valid = p.is_file()
                && fs::metadata(&p).map_err(crate::external)?.len() == e.size
                && fs::read(&p).is_ok_and(|v| verify_blob(&v, &e.oid, e.size).is_ok());
            if !valid {
                atomic(&p, data)?;
            }
        }
        Ok(())
    }
    fn read(&self, path: &str, c: &Cancellation) -> Result<Vec<u8>> {
        c.check()?;
        let e = self
            .blob(path, c)?
            .ok_or_else(|| Error::Invalid(format!("Файла нет в закреплённой ревизии: {path}")))?;
        let bytes = match self.cached(path, &e)? {
            Some(b) => b,
            None => self.client.file(
                &self.revision,
                &format!("{}/{path}", self.connection.directory),
                &e,
                c,
            )?,
        };
        self.put(path, &e, &bytes)?;
        Ok(bytes)
    }
}
pub struct GitSource {
    files: Files,
    catalog: Catalog,
    manifest: GitInputs,
    identity: String,
}
impl GitSource {
    pub fn open(
        connection: &Connection,
        client: Arc<dyn GitProvider>,
        c: &Cancellation,
    ) -> Result<Self> {
        let connection = connection.checked()?;
        let revision = client.head(&connection.branch, c)?;
        let mut tree = client.root(&revision, c)?;
        for part in connection.directory.split('/') {
            let entries = checked(client.tree(&tree, c)?)?;
            let e = entries
                .get(part)
                .ok_or_else(|| Error::Invalid("Каталог XML-выгрузки не найден в Git".into()))?;
            if e.kind != EntryKind::Tree {
                return Err(Error::Invalid(
                    "Вместо каталога выгрузки указан файл".into(),
                ));
            }
            tree = e.oid.clone();
        }
        let root_tree = checked(client.tree(&tree, c)?)?;
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
        let files = Files {
            client,
            connection: connection.clone(),
            revision: revision.clone(),
            blobs: cache.join(&repo).join("blobs"),
            root: root.clone(),
            trees: Mutex::new(BTreeMap::from([("".into(), Arc::new(root_tree))])),
        };
        let catalog = catalog(&files.read("Configuration.xml", c)?)?;
        let owners = catalog
            .objects
            .values()
            .map(|o| (o.descriptor.clone(), o.key.0.clone()))
            .collect::<BTreeMap<_, _>>();
        let mut paths = owners.keys().cloned().collect::<BTreeSet<_>>();
        paths.insert("Configuration.xml".into());
        for (name, e) in files.directory("Documents", c)?.iter() {
            if e.kind == EntryKind::Blob && name.ends_with(".xml") {
                paths.insert(format!("Documents/{name}"));
            }
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
        let task = task.trim();
        if task.is_empty() || task.len() > 256 || task.chars().any(char::is_control) {
            return Err(Error::Invalid("Укажите номер задачи Git".into()));
        }
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
        let matcher = regex::Regex::new(&format!(
            r"(?i)(?:^|[^\pL\pN_]){}(?:$|[^\pL\pN_])",
            regex::escape(task)
        ))
        .map_err(crate::external)?;
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
    fn manifest(&self) -> Option<&GitInputs> {
        Some(&self.manifest)
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
        let mut missing = vec![];
        for path in paths {
            c.check()?;
            let e = self
                .files
                .blob(path, c)?
                .ok_or_else(|| Error::Invalid(format!("Нет файла {path}")))?;
            if let Some(data) = self.files.cached(path, &e)? {
                self.files.put(path, &e, &data)?;
            } else {
                missing.push((path.clone(), e));
            }
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
            for (path, e) in batch {
                let full = format!("{}/{path}", self.files.connection.directory);
                let bytes = data
                    .remove(&full)
                    .ok_or_else(|| Error::Invalid("Неполная загрузка XML из Git".into()))?;
                self.files.put(path, e, &bytes)?;
            }
        }
        Ok(())
    }
    fn exists(&self, path: &str, c: &Cancellation) -> Result<bool> {
        Ok(self.files.blob(path, c)?.is_some())
    }
    fn list_directory(&self, path: &str, c: &Cancellation) -> Result<Vec<SourceEntry>> {
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
    }
    fn files_below(&self, path: &str, c: &Cancellation) -> Result<Vec<String>> {
        let mut pending = vec![path.to_owned()];
        let mut out = vec![];
        while let Some(dir) = pending.pop() {
            c.check()?;
            if dir.split('/').count() > 128 {
                return Err(Error::Invalid("Слишком глубокое дерево Git".into()));
            }
            for e in self.files.directory(&dir, c)?.values() {
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
    fn snapshot(&self, c: &Cancellation) -> Result<String> {
        c.check()?;
        Ok(format!(
            "{}@{}:{}",
            self.manifest.source_key,
            self.manifest.revision,
            self.manifest.fingerprint()?
        ))
    }
}
