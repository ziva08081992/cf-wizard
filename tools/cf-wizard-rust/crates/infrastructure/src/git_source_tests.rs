use super::*;
use crate::git::Commit;
use base64::{Engine, engine::general_purpose::STANDARD};
use sha1::{Digest, Sha1};
use std::sync::atomic::{AtomicUsize, Ordering};
fn hash(bytes: &[u8]) -> String {
    let mut h = Sha1::new();
    h.update(format!("blob {}\0", bytes.len()));
    h.update(bytes);
    hex::encode(h.finalize())
}
struct Memory {
    data: BTreeMap<String, Vec<u8>>,
    dirs: BTreeMap<String, Vec<Entry>>,
    reads: Mutex<BTreeMap<String, usize>>,
    trees: AtomicUsize,
    revision: AtomicUsize,
    cancel_after: AtomicUsize,
    history: Mutex<Vec<Commit>>,
    history_calls: AtomicUsize,
    changed_requests: Mutex<Vec<String>>,
    history_ranges: Mutex<Vec<(String, Option<String>, String)>>,
}
impl Memory {
    fn new() -> Arc<Self> {
        let row = crate::engine::tests::rows();
        let data = row["cases"][0]["source"]
            .as_object()
            .unwrap()
            .iter()
            .map(|(p, b)| {
                (
                    format!("config/{p}"),
                    STANDARD.decode(b.as_str().unwrap()).unwrap(),
                )
            })
            .collect::<BTreeMap<_, _>>();
        Self::from_data(data)
    }
    fn from_data(data: BTreeMap<String, Vec<u8>>) -> Arc<Self> {
        let mut dirs = BTreeMap::<String, BTreeMap<String, Entry>>::new();
        for (path, data) in &data {
            let mut parent = String::new();
            let parts = path.split('/').collect::<Vec<_>>();
            for (i, part) in parts.iter().enumerate() {
                let next = if parent.is_empty() {
                    part.to_string()
                } else {
                    format!("{parent}/{part}")
                };
                let file = i == parts.len() - 1;
                dirs.entry(parent.clone()).or_default().insert(
                    part.to_string(),
                    Entry {
                        name: part.to_string(),
                        oid: if file { hash(data) } else { next.clone() },
                        size: if file { data.len() as u64 } else { 0 },
                        kind: if file {
                            EntryKind::Blob
                        } else {
                            EntryKind::Tree
                        },
                        mode: if file { "100644" } else { "040000" }.into(),
                    },
                );
                parent = next;
            }
        }
        let mut paths: Vec<_> = dirs.keys().cloned().collect();
        paths.sort_by_key(|p| {
            std::cmp::Reverse(p.matches('/').count() + usize::from(!p.is_empty()))
        });
        let mut ids = BTreeMap::new();
        let mut trees = BTreeMap::new();
        for path in paths {
            let mut entries: Vec<_> = dirs.remove(&path).unwrap().into_values().collect();
            for entry in &mut entries {
                if entry.kind == EntryKind::Tree {
                    entry.oid = String::clone(&ids[&entry.oid]);
                }
            }
            let id = if path.is_empty() {
                // The root alias returned by this fake provider is not a tree OID.
                String::new()
            } else {
                super::inventory_tests::fixture_tree_oid(&entries)
            };
            ids.insert(path, id.clone());
            trees.insert(id, entries);
        }
        Arc::new(Self {
            data,
            dirs: trees,
            reads: Mutex::new(BTreeMap::new()),
            trees: AtomicUsize::new(0),
            revision: AtomicUsize::new(1),
            cancel_after: AtomicUsize::new(usize::MAX),
            history: Mutex::new(vec![]),
            history_calls: AtomicUsize::new(0),
            changed_requests: Mutex::new(vec![]),
            history_ranges: Mutex::new(vec![]),
        })
    }
}
impl GitProvider for Memory {
    fn branches(&self, _: &Cancellation) -> Result<Vec<String>> {
        Ok(vec!["main".into(), "other".into()])
    }
    fn head(&self, _: &str, c: &Cancellation) -> Result<String> {
        c.check()?;
        Ok(format!("{:040x}", self.revision.load(Ordering::Relaxed)))
    }
    fn root(&self, _: &str, _: &Cancellation) -> Result<String> {
        Ok(String::new())
    }
    fn tree(&self, id: &str, c: &Cancellation) -> Result<Vec<Entry>> {
        c.check()?;
        let count = self.trees.fetch_add(1, Ordering::Relaxed) + 1;
        if count == self.cancel_after.load(Ordering::Relaxed) {
            c.cancel();
        }
        Ok(self.dirs.get(id).cloned().unwrap_or_default())
    }
    fn file(&self, _: &str, path: &str, e: &Entry, c: &Cancellation) -> Result<Vec<u8>> {
        c.check()?;
        *self.reads.lock().unwrap().entry(path.into()).or_default() += 1;
        let data = self.data.get(path).unwrap().clone();
        verify_blob(&data, &e.oid, e.size)?;
        Ok(data)
    }
    fn commits(
        &self,
        revision: &str,
        since: Option<&str>,
        until: &str,
        cancel: &Cancellation,
        progress: &dyn Fn(usize),
    ) -> Result<Vec<Commit>> {
        cancel.check()?;
        self.history_calls.fetch_add(1, Ordering::Relaxed);
        self.history_ranges.lock().unwrap().push((
            revision.into(),
            since.map(str::to_owned),
            until.into(),
        ));
        let rows = self.history.lock().unwrap().clone();
        progress(rows.len());
        cancel.check()?;
        Ok(rows)
    }
    fn changed(&self, revision: &str, _: &str, cancel: &Cancellation) -> Result<Vec<String>> {
        cancel.check()?;
        self.changed_requests.lock().unwrap().push(revision.into());
        Ok(vec!["config/CommonModules/А/Ext/Module.bsl".into()])
    }
}
fn connection(root: &Path) -> Connection {
    Connection {
        server: "https://example.test".into(),
        repository: "owner/repo".into(),
        branch: "main".into(),
        cache: root.to_string_lossy().into(),
        ..Connection::default()
    }
}
#[test]
fn commit_search_uses_exact_keys_and_literal_fragments_for_both_providers() {
    let messages = [
        "ERP-123 исправление",
        "erp-123\nмногострочный комментарий",
        "ERP-1234",
        "XERP-123",
        "ERP-123_test",
        "Исправление обмена",
        "Дата 13.11.2026",
        "Буквальный .*[?( и \"текст\"",
        "Два  пробела",
        "TASK_X9-42: выпуск",
        "ЖERP-123",
        "ERP-123Я",
        "(ERP-123)",
    ];
    for provider in ["gitea", "github"] {
        let root = tempfile::tempdir().unwrap();
        let api = Memory::new();
        *api.history.lock().unwrap() = messages
            .iter()
            .enumerate()
            .map(|(i, message)| Commit {
                oid: format!("{:040x}", i + 10),
                message: (*message).into(),
                date: "2026-11-01T00:00:00Z".into(),
            })
            .collect();
        let mut conn = connection(root.path());
        conn.provider = provider.into();
        if provider == "github" {
            conn.server = "https://github.com".into();
        }
        let cancel = Cancellation::default();
        let source = GitSource::open(&conn, api.clone(), &cancel).unwrap();
        let cases: &[(&str, &[usize])] = &[
            (" ERP-123 ", &[0, 1, 12]),
            ("правление ОБМЕНА", &[5]),
            ("13.11.2026", &[6]),
            (".*[?(", &[7]),
            ("\"ТЕКСТ\"", &[7]),
            ("два  пробела", &[8]),
            ("task_x9-42", &[9]),
            ("нет совпадений", &[]),
        ];
        for &(query, matching) in cases {
            api.changed_requests.lock().unwrap().clear();
            let result = source.search(query, Some(90), &cancel, &|_| {}).unwrap();
            let expected = matching
                .iter()
                .map(|i| format!("{:040x}", i + 10))
                .collect::<Vec<_>>();
            assert_eq!(result.commits, expected, "{provider}: {query}");
            assert_eq!(*api.changed_requests.lock().unwrap(), expected);
            assert_eq!(result.revision, source.revision());
            assert_eq!(result.scanned, messages.len());
            assert_eq!(result.files, usize::from(!matching.is_empty()));
            if matching.is_empty() {
                assert!(result.objects.is_empty());
            } else {
                assert_eq!(result.objects, ["CommonModule.А"]);
            }
        }
        assert!(
            api.history_ranges
                .lock()
                .unwrap()
                .iter()
                .all(|(revision, since, until)| revision == source.revision()
                    && since.is_some()
                    && !until.is_empty())
        );
        source.search("ERP-123", None, &cancel, &|_| {}).unwrap();
        assert!(
            api.history_ranges
                .lock()
                .unwrap()
                .last()
                .unwrap()
                .1
                .is_none()
        );
        api.changed_requests.lock().unwrap().clear();
        assert!(
            source
                .search("ERP-123", None, &cancel, &|_| cancel.cancel())
                .is_err()
        );
        assert!(api.changed_requests.lock().unwrap().is_empty());
    }
}

#[test]
fn commit_search_rejects_invalid_raw_input_before_requesting_history() {
    let root = tempfile::tempdir().unwrap();
    let api = Memory::new();
    let cancel = Cancellation::default();
    let source = GitSource::open(&connection(root.path()), api.clone(), &cancel).unwrap();
    let invalid = [
        "".into(),
        "   ".into(),
        "\nERP-123".into(),
        "ERP-123\t".into(),
        "text\r\nother".into(),
        "text\0other".into(),
        "\u{007f}text".into(),
        "text\u{009f}".into(),
        "a".repeat(257),
        "Я".repeat(129),
        "😀".repeat(65),
    ];
    for query in invalid {
        assert!(matches!(
            source.search(&query, Some(90), &cancel, &|_| {}),
            Err(Error::Invalid(_))
        ));
    }
    assert_eq!(api.history_calls.load(Ordering::Relaxed), 0);
    assert!(api.changed_requests.lock().unwrap().is_empty());
    for query in ["a".repeat(256), "Я".repeat(128), "😀".repeat(64)] {
        let padded = format!("  {query}  ");
        api.history.lock().unwrap().push(Commit {
            oid: "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".into(),
            message: format!("prefix {query} suffix"),
            date: "2026-11-01T00:00:00Z".into(),
        });
        let result = source.search(&padded, Some(90), &cancel, &|_| {}).unwrap();
        assert_eq!(result.commits.len(), 1);
    }
}
#[test]
fn restart_and_new_sha_reuse_manifest_index_and_shared_blobs() {
    let root = tempfile::tempdir().unwrap();
    let api = Memory::new();
    let c = Cancellation::default();
    let conn = connection(&root.path().join("cache"));
    let first = GitSource::open(&conn, api.clone(), &c).unwrap();
    assert_eq!(
        api.reads.lock().unwrap().len(),
        1,
        "opening reads Configuration only"
    );
    let index_dir = root.path().join("indexes");
    crate::indexing::update(&first, &index_dir, &c, &|_, _| {}).unwrap();
    assert!(crate::indexing::status(&first, &index_dir).unwrap().ready);
    let reads = api.reads.lock().unwrap().clone();
    api.revision.store(2, Ordering::Relaxed);
    let second = GitSource::open(&conn, api.clone(), &c).unwrap();
    assert_eq!(
        first.manifest.fingerprint().unwrap(),
        second.manifest.fingerprint().unwrap()
    );
    assert_ne!(
        first.snapshot(&c).unwrap(),
        second.snapshot(&c).unwrap(),
        "plan remains SHA-bound"
    );
    assert!(crate::indexing::status(&second, &index_dir).unwrap().ready);
    let report = crate::indexing::update(&second, &index_dir, &c, &|_, _| {}).unwrap();
    assert_eq!(report.xml_reads, 0);
    assert_eq!(
        *api.reads.lock().unwrap(),
        reads,
        "new SHA does not redownload unchanged XML"
    );
    let third = GitSource::open(&conn, api.clone(), &c).unwrap();
    assert_eq!(*api.reads.lock().unwrap(), reads);
    let root_config = third.files.root.join("Configuration.xml");
    let entry = third.files.blob("Configuration.xml", &c).unwrap().unwrap();
    assert!(
        !third.files.root.exists(),
        "new reader never materializes commit snapshots"
    );
    fs::create_dir_all(&third.files.root).unwrap(); // Explicit fixture for an older release's copy.
    fs::write(&root_config, b"corrupt").unwrap();
    fs::write(third.files.blobs.join(&entry.oid), b"corrupt").unwrap();
    assert_eq!(
        third.read("Configuration.xml", &c).unwrap(),
        api.data["config/Configuration.xml"]
    );
    assert_eq!(
        api.reads.lock().unwrap()["config/Configuration.xml"],
        reads["config/Configuration.xml"] + 1
    );
}
#[test]
fn directory_traversal_stops_after_cancelled_subtree_request() {
    let root = tempfile::tempdir().unwrap();
    let api = Memory::new();
    let c = Cancellation::default();
    let source = GitSource::open(&connection(root.path()), api.clone(), &c).unwrap();
    let before = api.trees.load(Ordering::Relaxed);
    api.cancel_after.store(before + 1, Ordering::Relaxed);
    assert!(matches!(
        source.files_below("CommonModules/А", &c),
        Err(Error::Cancelled)
    ));
    assert_eq!(api.trees.load(Ordering::Relaxed), before + 1);
}
#[test]
fn wide_directory_is_shared_and_blob_lookups_do_not_clone_the_folder() {
    let root = tempfile::tempdir().unwrap();
    let api = Memory::new();
    let c = Cancellation::default();
    let entries: BTreeMap<String, Entry> = (0..10000)
        .map(|n| {
            let name = format!("Module{n}.xml");
            (
                name.clone(),
                Entry {
                    name,
                    oid: "a".repeat(40),
                    size: 1,
                    kind: EntryKind::Blob,
                    mode: "100644".into(),
                },
            )
        })
        .collect();
    let entries = Tree::new(entries, None);
    let files = Files {
        counters: cf_application::profile::SourceCounters::new(false),
        client: api.clone(),
        connection: connection(root.path()),
        revision: "a".repeat(40),
        root: root.path().join("config"),
        blobs: root.path().join("blobs"),
        trees: Mutex::new(BTreeMap::from([("CommonModules".into(), entries.clone())])),
        oid_trees: Mutex::new(BTreeMap::new()),
        inventory_workers: AtomicUsize::new(0),
        tree_cache: None,
        recursive_disabled: AtomicBool::new(false),
    };
    assert!(Arc::ptr_eq(
        &entries,
        &files.directory("CommonModules", &c).unwrap()
    ));
    for n in 0..10000 {
        assert!(
            files
                .blob(&format!("CommonModules/Module{n}.xml"), &c)
                .unwrap()
                .is_some()
        );
    }
    assert_eq!(api.trees.load(Ordering::Relaxed), 0);
}

#[test]
fn shared_payload_hit_reads_once_and_prefetch_never_materializes_a_revision() {
    let root = tempfile::tempdir().unwrap();
    let api = Memory::new();
    let cancel = Cancellation::default();
    let mut source = GitSource::open(&connection(root.path()), api.clone(), &cancel).unwrap();
    assert!(!source.files.root.exists());
    source.files.counters = profile::SourceCounters::new(true);
    let before = api.reads.lock().unwrap().clone();
    let context = profile::Context::new("preview", true, source.profile_counters());
    {
        let _scope = context.enter();
        assert_eq!(
            source.read("Configuration.xml", &cancel).unwrap(),
            api.data["config/Configuration.xml"]
        );
        source
            .prefetch(&["Configuration.xml".into()], &cancel)
            .unwrap();
    }
    let report = context.report("ok").unwrap();
    assert_eq!(report.metrics["cache.read"].totals.calls, 2);
    assert!(!report.metrics.contains_key("cache.write"));
    assert_eq!(report.metrics["cache.payload_shared_hit"].totals.items, 2);
    assert_eq!(*api.reads.lock().unwrap(), before);
    assert!(!source.files.root.exists());
}

#[test]
fn legacy_only_payload_is_verified_promoted_and_left_unchanged() {
    let root = tempfile::tempdir().unwrap();
    let api = Memory::new();
    let cancel = Cancellation::default();
    let mut source = GitSource::open(&connection(root.path()), api.clone(), &cancel).unwrap();
    let entry = source
        .files
        .blob("Configuration.xml", &cancel)
        .unwrap()
        .unwrap();
    let shared = source.files.blobs.join(&entry.oid);
    let legacy = source.files.root.join("Configuration.xml");
    let expected = fs::read(&shared).unwrap();
    fs::create_dir_all(legacy.parent().unwrap()).unwrap();
    fs::write(&legacy, &expected).unwrap();
    fs::remove_file(&shared).unwrap();
    let before = api.reads.lock().unwrap().clone();
    source.files.counters = profile::SourceCounters::new(true);
    let context = profile::Context::new("preview", true, source.profile_counters());
    {
        let _scope = context.enter();
        assert_eq!(source.read("Configuration.xml", &cancel).unwrap(), expected);
    }
    let report = context.report("ok").unwrap();
    assert_eq!(
        report.metrics["cache.payload_legacy_promoted"].totals.items,
        1
    );
    assert_eq!(report.metrics["cache.write"].totals.calls, 1);
    assert_eq!(fs::read(shared).unwrap(), expected);
    assert_eq!(fs::read(legacy).unwrap(), expected);
    assert_eq!(*api.reads.lock().unwrap(), before);
}

#[test]
fn corrupt_shared_payload_uses_legacy_then_provider_without_rewriting_legacy() {
    let root = tempfile::tempdir().unwrap();
    let api = Memory::new();
    let cancel = Cancellation::default();
    let source = GitSource::open(&connection(root.path()), api.clone(), &cancel).unwrap();
    let entry = source
        .files
        .blob("Configuration.xml", &cancel)
        .unwrap()
        .unwrap();
    let shared = source.files.blobs.join(&entry.oid);
    let legacy = source.files.root.join("Configuration.xml");
    let expected = fs::read(&shared).unwrap();
    let mut bad = expected.clone();
    bad[0] ^= 1;
    fs::create_dir_all(legacy.parent().unwrap()).unwrap();
    fs::write(&legacy, &expected).unwrap();
    fs::write(&shared, &bad).unwrap();
    let before = api.reads.lock().unwrap().clone();
    assert_eq!(source.read("Configuration.xml", &cancel).unwrap(), expected);
    assert_eq!(*api.reads.lock().unwrap(), before);
    assert_eq!(fs::read(&shared).unwrap(), expected);
    fs::write(&shared, &bad).unwrap();
    fs::write(&legacy, &bad).unwrap();
    assert_eq!(source.read("Configuration.xml", &cancel).unwrap(), expected);
    assert_eq!(fs::read(shared).unwrap(), expected);
    assert_eq!(fs::read(legacy).unwrap(), bad);
    assert_eq!(
        api.reads.lock().unwrap()["config/Configuration.xml"],
        before["config/Configuration.xml"] + 1
    );
}

#[test]
fn verified_readonly_shared_payload_does_not_attempt_a_write() {
    let root = tempfile::tempdir().unwrap();
    let api = Memory::new();
    let cancel = Cancellation::default();
    let source = GitSource::open(&connection(root.path()), api.clone(), &cancel).unwrap();
    let entry = source
        .files
        .blob("Configuration.xml", &cancel)
        .unwrap()
        .unwrap();
    let shared = source.files.blobs.join(&entry.oid);
    let permissions = fs::metadata(&shared).unwrap().permissions();
    let mut readonly = permissions.clone();
    readonly.set_readonly(true);
    fs::set_permissions(&shared, readonly).unwrap();
    let result = source.read("Configuration.xml", &cancel);
    let prefetched = source.prefetch(&["Configuration.xml".into()], &cancel);
    fs::set_permissions(&shared, permissions).unwrap();
    assert_eq!(result.unwrap(), api.data["config/Configuration.xml"]);
    prefetched.unwrap();
    assert!(!source.files.root.exists());
}

#[test]
fn cancellation_does_not_create_a_revision_snapshot() {
    let root = tempfile::tempdir().unwrap();
    let api = Memory::new();
    let cancel = Cancellation::default();
    let source = GitSource::open(&connection(root.path()), api.clone(), &cancel).unwrap();
    let before = api.reads.lock().unwrap().clone();
    cancel.cancel();
    assert!(matches!(
        source.read("Configuration.xml", &cancel),
        Err(Error::Cancelled)
    ));
    assert!(matches!(
        source.prefetch(&["Configuration.xml".into()], &cancel),
        Err(Error::Cancelled)
    ));
    assert_eq!(*api.reads.lock().unwrap(), before);
    assert!(!source.files.root.exists());
}

#[test]
fn two_revisions_share_payloads_without_mixing_changed_or_deleted_paths() {
    let root = tempfile::tempdir().unwrap();
    let cancel = Cancellation::default();
    let shared_path = "CommonModules/А/Ext/Shared.bin";
    let changed_path = "CommonModules/А/Ext/Changed.bin";
    let removed_path = "CommonModules/А/Ext/Removed.bin";
    let mut a_data = Memory::new().data.clone();
    a_data.insert(format!("config/{shared_path}"), b"same".to_vec());
    a_data.insert(format!("config/{changed_path}"), b"old-version".to_vec());
    a_data.insert(format!("config/{removed_path}"), b"removed".to_vec());
    let mut b_data = a_data.clone();
    b_data.insert(format!("config/{changed_path}"), b"new-version".to_vec());
    b_data.remove(&format!("config/{removed_path}"));
    let a_api = Memory::from_data(a_data);
    let b_api = Memory::from_data(b_data);
    b_api.revision.store(2, Ordering::Relaxed);
    let a = GitSource::open(&connection(root.path()), a_api.clone(), &cancel).unwrap();
    let mut other = connection(root.path());
    other.branch = "other".into();
    let b = GitSource::open(&other, b_api.clone(), &cancel).unwrap();
    std::thread::scope(|scope| {
        let left = scope.spawn(|| a.read(changed_path, &cancel).unwrap());
        let right = scope.spawn(|| b.read(changed_path, &cancel).unwrap());
        assert_eq!(left.join().unwrap(), b"old-version");
        assert_eq!(right.join().unwrap(), b"new-version");
    });
    assert_eq!(a.read(shared_path, &cancel).unwrap(), b"same");
    assert_eq!(b.read(shared_path, &cancel).unwrap(), b"same");
    assert!(
        !b_api
            .reads
            .lock()
            .unwrap()
            .contains_key(&format!("config/{shared_path}"))
    );
    assert_eq!(a.read(removed_path, &cancel).unwrap(), b"removed");
    assert!(!b.exists(removed_path, &cancel).unwrap());
    assert!(b.read(removed_path, &cancel).is_err());
    assert_eq!(a.read(changed_path, &cancel).unwrap(), b"old-version");
    assert_ne!(a.snapshot(&cancel).unwrap(), b.snapshot(&cancel).unwrap());
    assert!(!a.files.root.exists() && !b.files.root.exists());
}

#[test]
fn concurrent_same_oid_publication_is_atomic_and_leaves_no_revision_copy() {
    let root = tempfile::tempdir().unwrap();
    let api = Memory::new();
    let cancel = Cancellation::default();
    let source = GitSource::open(&connection(root.path()), api, &cancel).unwrap();
    let payload = vec![0x5a; 128 * 1024];
    let entry = Entry {
        name: "Same.bin".into(),
        oid: hash(&payload),
        size: payload.len() as u64,
        kind: EntryKind::Blob,
        mode: "100644".into(),
    };
    let barrier = std::sync::Barrier::new(8);
    std::thread::scope(|scope| {
        let workers = (0..8)
            .map(|_| {
                scope.spawn(|| {
                    barrier.wait();
                    source.files.put("Same.bin", &entry, &payload, &cancel)
                })
            })
            .collect::<Vec<_>>();
        for worker in workers {
            worker.join().unwrap().unwrap();
        }
    });
    assert_eq!(
        fs::read(source.files.blobs.join(&entry.oid)).unwrap(),
        payload
    );
    assert!(!source.files.root.exists());
    assert!(fs::read_dir(&source.files.blobs).unwrap().all(|e| {
        e.unwrap()
            .file_name()
            .to_string_lossy()
            .chars()
            .all(|c| c.is_ascii_hexdigit())
    }));
}

#[test]
fn recursive_publication_leaves_active_root_and_descendant_flights_to_their_initializers() {
    use crate::git::RecursiveRow;
    use std::{sync::mpsc, time::Duration};

    const ROOT: &str = "094b9952c5ca2208e85aa347454bcf0545837359";
    const CHILD: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";
    const SIBLING: &str = "8867f3a256f82b8af2ab98e6c7f9b1df8fd4786c";
    let row = |path: &str, id: &str, kind: EntryKind| RecursiveRow {
        path: path.into(),
        entry: Entry {
            name: path.rsplit('/').next().unwrap().into(),
            oid: id.into(),
            size: 0,
            mode: if kind == EntryKind::Tree {
                "040000"
            } else {
                "100644"
            }
            .into(),
            kind,
        },
    };
    let cancel = Cancellation::default();
    let RecursiveListing::Complete(verified) = git_recursive::validate(
        ROOT,
        vec![
            row("child", CHILD, EntryKind::Tree),
            row("new", SIBLING, EntryKind::Tree),
            row("new/n", &"22".repeat(20), EntryKind::Blob),
        ],
        1,
        &cancel,
    )
    .unwrap() else {
        panic!("test vector did not validate");
    };
    let root = tempfile::tempdir().unwrap();
    let source = GitSource::open(&connection(root.path()), Memory::new(), &cancel).unwrap();
    let files = &source.files;
    let cache = files.tree_cache.as_ref().unwrap();
    let root_slot = Arc::new(OnceLock::new());
    let child_slot = Arc::new(OnceLock::new());
    let fetches = InventoryFetches {
        trees: Mutex::new(BTreeMap::from([
            (ROOT.into(), Arc::clone(&root_slot)),
            (CHILD.into(), Arc::clone(&child_slot)),
        ])),
    };
    let fetches = &fetches;
    let cancel = &cancel;
    let root_value = Tree::new(BTreeMap::new(), None);
    let child_value = Tree::new(BTreeMap::new(), None);
    std::thread::scope(|scope| {
        let (started, active) = mpsc::channel();
        let (release_root, wait_root) = mpsc::channel();
        let (release_child, wait_child) = mpsc::channel();
        for (slot, release, value) in [
            (&root_slot, wait_root, &root_value),
            (&child_slot, wait_child, &child_value),
        ] {
            let started = started.clone();
            scope.spawn(move || {
                slot.get_or_init(|| {
                    started.send(()).unwrap();
                    release.recv_timeout(Duration::from_secs(10)).unwrap();
                    Ok(Arc::clone(value))
                });
            });
        }
        let active_before_publication = active.recv_timeout(Duration::from_secs(3)).is_ok()
            && active.recv_timeout(Duration::from_secs(3)).is_ok();
        let (done, publication) = mpsc::channel();
        scope.spawn(move || {
            let result = files.publish_subtree(cache, ROOT, verified, fetches, cancel);
            let _ = done.send(result);
        });
        let result = publication.recv_timeout(Duration::from_secs(3));
        let flights_still_active = root_slot.get().is_none() && child_slot.get().is_none();
        // Release the blockers even when publishing violated the no-wait rule.
        let _ = release_root.send(());
        let _ = release_child.send(());
        assert!(active_before_publication);
        let published = result
            .expect("publication waited for an in-flight OID")
            .unwrap();
        assert!(flights_still_active);
        assert_eq!(published["child"].oid, CHILD);
        assert_eq!(published["new"].oid, SIBLING);
    });
    let known = files.oid_trees.lock().unwrap();
    assert!(!known.contains_key(ROOT));
    assert!(!known.contains_key(CHILD));
    assert_eq!(known[SIBLING]["n"].oid, "22".repeat(20));
    drop(known);
    assert!(Arc::ptr_eq(
        root_slot.get().unwrap().as_ref().unwrap(),
        &root_value
    ));
    assert!(Arc::ptr_eq(
        child_slot.get().unwrap().as_ref().unwrap(),
        &child_value
    ));
    assert!(cache.read(ROOT, cancel).unwrap().is_some());
    assert!(cache.read(CHILD, cancel).unwrap().is_none());
    assert!(cache.read(SIBLING, cancel).unwrap().is_some());
}

#[test]
fn cancellation_at_the_persist_boundary_keeps_existing_payload_and_removes_temporary_file() {
    let root = tempfile::tempdir().unwrap();
    let target = root.path().join("blob");
    fs::write(&target, b"existing verified winner").unwrap();
    let mut temporary = tempfile::NamedTempFile::new_in(root.path()).unwrap();
    temporary.write_all(b"replacement verified bytes").unwrap();
    temporary.as_file().sync_all().unwrap();
    let temporary_path = temporary.path().to_owned();
    let cancel = Cancellation::default();
    cancel.cancel();
    assert!(matches!(
        commit_file(temporary, &target, &cancel),
        Err(Error::Cancelled)
    ));
    assert_eq!(fs::read(&target).unwrap(), b"existing verified winner");
    assert!(!temporary_path.exists());
    assert_eq!(fs::read_dir(root.path()).unwrap().count(), 1);
}
