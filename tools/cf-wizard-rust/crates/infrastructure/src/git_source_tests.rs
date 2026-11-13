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
        Arc::new(Self {
            data,
            dirs: dirs
                .into_iter()
                .map(|(p, e)| (p, e.into_values().collect()))
                .collect(),
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
    let entries = Arc::new(entries);
    let files = Files {
        client: api.clone(),
        connection: connection(root.path()),
        revision: "a".repeat(40),
        root: root.path().join("config"),
        blobs: root.path().join("blobs"),
        trees: Mutex::new(BTreeMap::from([("CommonModules".into(), entries.clone())])),
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
