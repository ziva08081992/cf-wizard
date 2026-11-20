use super::*;
use std::sync::{
    Barrier,
    atomic::{AtomicUsize, Ordering},
};

const SHA1: &str = "c3bcc39b648e1c38ed7bf8941fc54080b3a404cc";
const SHA256: &str = "37e9947ec1f3b862f389f351413d637794cd68aec14d28c52c6f979f891d17ed";

fn fixture(oid: &str) -> Vec<Entry> {
    [
        ("App0", "100644", "33"),
        ("Я.xml", "100644", "44"),
        ("App", "040000", "22"),
        ("App.check", "100755", "11"),
    ]
    .map(|(name, mode, byte)| Entry {
        name: name.into(),
        oid: byte.repeat(oid.len() / 2),
        kind: if mode == "040000" {
            EntryKind::Tree
        } else {
            EntryKind::Blob
        },
        mode: mode.into(),
        size: if mode == "040000" { 0 } else { 42 },
    })
    .into()
}

fn cache(path: &Path, oid: &str) -> TreeCache {
    TreeCache::new(
        path,
        &Connection {
            server: "https://example.test".into(),
            repository: "test/private".into(),
            ..Connection::default()
        },
        oid,
    )
    .unwrap()
}

#[test]
fn canonical_hash_matches_git_hash_object_for_both_formats() {
    // Git's independent hash-object provided the expected values. The
    // App.check file sorts before the App tree, and App0 sorts after it.
    let root = tempfile::tempdir().unwrap();
    let c = Cancellation::default();
    for oid in [SHA1, SHA256] {
        let cache = cache(root.path(), oid);
        let tree = cache.validate(oid, fixture(oid), &c).unwrap();
        cache.write(oid, &tree, &c).unwrap();
        let reopened = cache.read(oid, &c).unwrap().unwrap();
        assert_eq!(
            reopened.keys().collect::<Vec<_>>(),
            tree.keys().collect::<Vec<_>>()
        );
        assert_eq!(reopened["App.check"].mode, "100755");
        assert_eq!(reopened["Я.xml"].size, 42);
    }
    let empty = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";
    assert!(
        cache(root.path(), empty)
            .validate(empty, vec![], &c)
            .unwrap()
            .is_empty()
    );
    assert!(TreeCache::new(root.path(), &Connection::default(), "deadbeef").is_err());
}

#[test]
fn incomplete_invalid_and_case_colliding_rows_cannot_prove_inventory() {
    let root = tempfile::tempdir().unwrap();
    let c = Cancellation::default();
    let cache = cache(root.path(), SHA1);
    let cases: [fn(&mut Vec<Entry>); 9] = [
        |rows: &mut Vec<Entry>| {
            rows.pop();
        },
        |rows: &mut Vec<Entry>| rows[0].name = "я.XML".into(),
        |rows: &mut Vec<Entry>| rows[0].name = "App/other".into(),
        |rows: &mut Vec<Entry>| rows[0].name = "App0/".into(),
        |rows: &mut Vec<Entry>| rows[0].mode = "120000".into(),
        |rows: &mut Vec<Entry>| rows[0].oid = "a".repeat(64),
        |rows: &mut Vec<Entry>| rows[0].oid = "B".repeat(40),
        |rows: &mut Vec<Entry>| rows[0].oid = "f".repeat(40),
        |rows: &mut Vec<Entry>| rows[0].kind = EntryKind::Tree,
    ];
    for mutate in cases {
        let mut rows = fixture(SHA1);
        mutate(&mut rows);
        assert!(cache.validate(SHA1, rows, &c).is_err());
    }
    assert!(cache.validate(&"f".repeat(40), fixture(SHA1), &c).is_err());
}

#[test]
fn oversized_and_partial_cache_records_are_rejected() {
    let root = tempfile::tempdir().unwrap();
    let c = Cancellation::default();
    let cache = cache(root.path(), SHA1);
    let path = cache.path(SHA1).unwrap();
    assert!(cache.read(SHA1, &c).unwrap().is_none());
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::File::create(&path)
        .unwrap()
        .set_len(MAX_RECORD_BYTES as u64 + 1)
        .unwrap();
    assert!(cache.read(SHA1, &c).is_err());
    fs::write(&path, b"partial").unwrap();
    assert!(cache.read(SHA1, &c).is_err());
    let entries = cache.validate(SHA1, fixture(SHA1), &c).unwrap();
    cache.write(SHA1, &entries, &c).unwrap();
    assert!(cache.read(SHA1, &c).unwrap().is_some());
}

#[test]
fn namespace_uses_provider_repository_and_hash_format_only() {
    let root = tempfile::tempdir().unwrap();
    let mut connection = Connection {
        server: "https://example.test".into(),
        repository: "owner/private".into(),
        ..Connection::default()
    };
    let old = TreeCache::new(root.path(), &connection, SHA1)
        .unwrap()
        .directory;
    connection.branch = "different".into();
    connection.directory = "config2".into();
    assert_eq!(
        TreeCache::new(root.path(), &connection, &"3".repeat(40))
            .unwrap()
            .directory,
        old,
    );
    assert_ne!(
        TreeCache::new(root.path(), &connection, SHA256)
            .unwrap()
            .directory,
        old
    );
    assert!(!old.to_string_lossy().contains("private"));
    connection.repository = "owner/other".into();
    assert_ne!(
        TreeCache::new(root.path(), &connection, SHA1)
            .unwrap()
            .directory,
        old
    );
}

#[test]
fn concurrent_publish_yields_verified_records_or_a_miss() {
    let root = tempfile::tempdir().unwrap();
    let cache = cache(root.path(), SHA1);
    let c = Cancellation::default();
    let entries = cache.validate(SHA1, fixture(SHA1), &c).unwrap();
    let start = Barrier::new(4);
    let published = AtomicUsize::new(0);
    std::thread::scope(|scope| {
        for _ in 0..2 {
            scope.spawn(|| {
                start.wait();
                for _ in 0..20 {
                    // Windows can reject a replace while a concurrent reader
                    // uses the previous file. Publication is best effort.
                    if cache.write(SHA1, &entries, &c).is_ok() {
                        published.fetch_add(1, Ordering::Relaxed);
                    }
                }
            });
        }
        for _ in 0..2 {
            scope.spawn(|| {
                start.wait();
                for _ in 0..50 {
                    if let Some(read) = cache.read(SHA1, &c).unwrap() {
                        assert_eq!(read.len(), entries.len());
                    }
                }
            });
        }
    });
    assert!(published.load(Ordering::Relaxed) > 0);
    assert_eq!(cache.read(SHA1, &c).unwrap().unwrap().len(), entries.len());
    assert!(cache.sqlite.as_ref().unwrap().path().is_file());
    assert!(!cache.path(SHA1).unwrap().exists());
}

#[test]
fn unsafe_directory_is_not_read_or_written() {
    let root = tempfile::tempdir().unwrap();
    let real = root.path().join("real");
    let alias = root.path().join("alias");
    fs::create_dir(&real).unwrap();
    #[cfg(windows)]
    assert!(
        std::process::Command::new("cmd.exe")
            .args(["/D", "/C", "mklink", "/J", "alias", "real"])
            .current_dir(root.path())
            .output()
            .unwrap()
            .status
            .success()
    );
    #[cfg(unix)]
    std::os::unix::fs::symlink(&real, &alias).unwrap();

    let cache = cache(&alias, SHA1);
    let c = Cancellation::default();
    let entries = cache.validate(SHA1, fixture(SHA1), &c).unwrap();
    assert!(cache.read(SHA1, &c).is_err());
    assert!(cache.write(SHA1, &entries, &c).is_err());
    assert_eq!(fs::read_dir(&real).unwrap().count(), 0);
}

#[test]
fn sqlite_restart_uses_verified_rows_and_rejects_changed_identity() {
    let root = tempfile::tempdir().unwrap();
    let cancel = Cancellation::default();
    let source = cache(root.path(), SHA1);
    let entries = source.validate(SHA1, fixture(SHA1), &cancel).unwrap();
    assert!(source.write_batch([(SHA1, &entries)], &cancel).unwrap());
    assert!(!source.path(SHA1).unwrap().exists());
    drop(source);

    let reopened = cache(root.path(), SHA1);
    assert_eq!(
        reopened.read(SHA1, &cancel).unwrap().unwrap()["App0"].size,
        42
    );
    let path = reopened.sqlite.as_ref().unwrap().path();
    let foreign = rusqlite::Connection::open(path).unwrap();
    foreign
        .execute("UPDATE tree_identity SET identity='foreign'", [])
        .unwrap();
    assert!(reopened.read(SHA1, &cancel).unwrap().is_none());
    assert!(!reopened.write_batch([(SHA1, &entries)], &cancel).unwrap());
    assert!(reopened.path(SHA1).unwrap().is_file());
    assert_eq!(
        reopened.read(SHA1, &cancel).unwrap().unwrap()["App0"].size,
        42
    );
}

#[test]
fn database_lock_and_corrupt_rows_fall_back_to_unchanged_legacy() {
    let root = tempfile::tempdir().unwrap();
    let cancel = Cancellation::default();
    let source = cache(root.path(), SHA1);
    let entries = source.validate(SHA1, fixture(SHA1), &cancel).unwrap();
    let mut alternate = entries.clone();
    alternate.get_mut("App0").unwrap().size = 99; // Sizes are advisory.
    source.write_legacy(SHA1, &alternate, &cancel).unwrap();
    let legacy = source.path(SHA1).unwrap();
    let exact_legacy = fs::read(&legacy).unwrap();
    assert!(source.write_batch([(SHA1, &entries)], &cancel).unwrap());
    assert_eq!(
        source.read(SHA1, &cancel).unwrap().unwrap()["App0"].size,
        42
    );

    let db = rusqlite::Connection::open(source.sqlite.as_ref().unwrap().path()).unwrap();
    db.execute_batch("BEGIN EXCLUSIVE").unwrap();
    assert_eq!(
        source.read(SHA1, &cancel).unwrap().unwrap()["App0"].size,
        99
    );
    db.execute_batch("ROLLBACK").unwrap();

    let mut altered = entries.clone();
    altered.get_mut("App0").unwrap().mode = "100755".into();
    let valid_checksum = TreeCache::encode_record(&altered, sqlite::BATCH_BYTES)
        .unwrap()
        .unwrap();
    db.execute(
        "UPDATE trees SET record=? WHERE oid=?",
        rusqlite::params![valid_checksum, SHA1],
    )
    .unwrap();
    assert_eq!(
        source.read(SHA1, &cancel).unwrap().unwrap()["App0"].size,
        99
    );
    assert_eq!(fs::read(legacy).unwrap(), exact_legacy);
}

#[test]
fn legacy_import_preserves_bytes_and_rejects_invalid_entries() {
    let root = tempfile::tempdir().unwrap();
    let cancel = Cancellation::default();
    let source = cache(root.path(), SHA1);
    let entries = source.validate(SHA1, fixture(SHA1), &cancel).unwrap();
    source.write_legacy(SHA1, &entries, &cancel).unwrap();
    let legacy = source.path(SHA1).unwrap();
    let exact = fs::read(&legacy).unwrap();
    let wrong_id = "a".repeat(40);
    let wrong_path = source.path(&wrong_id).unwrap();
    fs::create_dir_all(wrong_path.parent().unwrap()).unwrap();
    fs::write(&wrong_path, &exact).unwrap();
    let report = source.import_legacy(&cancel).unwrap();
    assert_eq!(
        (
            report.scanned,
            report.imported,
            report.rejected,
            report.committed_batches
        ),
        (2, 1, 1, 1)
    );
    assert_eq!(fs::read(&legacy).unwrap(), exact);
    assert_eq!(fs::read(wrong_path).unwrap(), exact);
    drop(source);

    let reopened = cache(root.path(), SHA1);
    let db = reopened.sqlite.as_ref().unwrap();
    assert!(db.read(SHA1, &cancel).unwrap().is_some());
    assert!(db.read(&wrong_id, &cancel).unwrap().is_none());
    cancel.cancel();
    assert!(matches!(
        reopened.import_legacy(&cancel),
        Err(Error::Cancelled)
    ));
}

#[test]
fn cancellation_rolls_back_the_whole_sql_batch() {
    let root = tempfile::tempdir().unwrap();
    let cancel = Cancellation::default();
    let source = cache(root.path(), SHA1);
    let entries = source.validate(SHA1, fixture(SHA1), &cancel).unwrap();
    let record = TreeCache::encode_record(&entries, sqlite::BATCH_BYTES)
        .unwrap()
        .unwrap();
    let fresh = "f".repeat(40);
    let db = source.sqlite.as_ref().unwrap();
    let rows = [
        (SHA1, record.as_slice()),
        (fresh.as_str(), record.as_slice()),
    ]
    .into_iter()
    .enumerate()
    .map(|(index, row)| {
        if index == 1 {
            cancel.cancel();
        }
        row
    });
    assert!(matches!(
        db.write_batch(rows, &cancel),
        Err(Error::Cancelled)
    ));
    let check = Cancellation::default();
    assert!(db.read(SHA1, &check).unwrap().is_none());
    assert!(db.read(&fresh, &check).unwrap().is_none());
    assert!(!source.path(SHA1).unwrap().exists());
}

#[test]
fn sqlite_pagers_share_one_process_reservation_and_disable_mmap() {
    let roots: Vec<_> = (0..4).map(|_| tempfile::tempdir().unwrap()).collect();
    let cancel = Cancellation::default();
    let stores: Vec<_> = roots.iter().map(|root| cache(root.path(), SHA1)).collect();
    for source in &stores {
        let entries = source.validate(SHA1, fixture(SHA1), &cancel).unwrap();
        assert!(source.write_batch([(SHA1, &entries)], &cancel).unwrap());
    }
    let settings: Vec<_> = stores
        .iter()
        .map(|source| source.sqlite.as_ref().unwrap().effective_pager())
        .collect();
    assert!(
        settings
            .iter()
            .all(|(pager, mmap)| matches!(pager, 0 | 8192) && *mmap == 0)
    );
    assert!(settings.iter().map(|(pager, _)| pager).sum::<i64>() <= 8192);
}

#[test]
fn sqlite_unsafe_parent_uses_legacy_without_writing_outside_cache() {
    let root = tempfile::tempdir().unwrap();
    let cancel = Cancellation::default();
    let source = cache(root.path(), SHA1);
    let entries = source.validate(SHA1, fixture(SHA1), &cancel).unwrap();
    let real = root.path().join("elsewhere");
    #[cfg(unix)]
    let alias = root.path().join("tree-cache-sqlite-v1");
    fs::create_dir(&real).unwrap();
    #[cfg(windows)]
    assert!(
        std::process::Command::new("cmd.exe")
            .args([
                "/D",
                "/C",
                "mklink",
                "/J",
                "tree-cache-sqlite-v1",
                "elsewhere"
            ])
            .current_dir(root.path())
            .output()
            .unwrap()
            .status
            .success()
    );
    #[cfg(unix)]
    std::os::unix::fs::symlink(&real, &alias).unwrap();
    assert!(!source.write_batch([(SHA1, &entries)], &cancel).unwrap());
    assert!(source.read(SHA1, &cancel).unwrap().is_some());
    assert_eq!(fs::read_dir(real).unwrap().count(), 0);
}

#[test]
fn legacy_only_fixture_places_a_v1_record() {
    let root = tempfile::tempdir().unwrap();
    let cancel = Cancellation::default();
    let mut source = cache(root.path(), SHA1);
    source.legacy_only_for_test();
    let entries = source.validate(SHA1, fixture(SHA1), &cancel).unwrap();
    assert!(!source.write_batch([(SHA1, &entries)], &cancel).unwrap());
    assert!(source.path(SHA1).unwrap().is_file());
}

#[test]
fn sidecar_link_added_after_open_is_rejected_before_read() {
    let root = tempfile::tempdir().unwrap();
    let cancel = Cancellation::default();
    let source = cache(root.path(), SHA1);
    let entries = source.validate(SHA1, fixture(SHA1), &cancel).unwrap();
    let mut legacy = entries.clone();
    legacy.get_mut("App0").unwrap().size = 101;
    source.write_legacy(SHA1, &legacy, &cancel).unwrap();
    assert!(source.write_batch([(SHA1, &entries)], &cancel).unwrap());
    assert_eq!(
        source.read(SHA1, &cancel).unwrap().unwrap()["App0"].size,
        42
    );

    let db = source.sqlite_path_for_test();
    let parent = db.parent().unwrap();
    let target = parent.join("elsewhere");
    fs::create_dir(&target).unwrap();
    #[cfg(windows)]
    assert!(
        std::process::Command::new("cmd.exe")
            .args(["/D", "/C", "mklink", "/J", "trees.sqlite3-wal", "elsewhere"])
            .current_dir(parent)
            .output()
            .unwrap()
            .status
            .success()
    );
    #[cfg(unix)]
    std::os::unix::fs::symlink(&target, parent.join("trees.sqlite3-wal")).unwrap();
    assert_eq!(
        source.read(SHA1, &cancel).unwrap().unwrap()["App0"].size,
        101
    );
    assert_eq!(fs::read_dir(target).unwrap().count(), 0);
}

#[test]
fn production_batch_splits_large_proved_subtrees_by_rows_and_bytes() {
    for (count, entries_per_tree) in [(1100, 1), (80, 200)] {
        let root = tempfile::tempdir().unwrap();
        let cancel = Cancellation::default();
        let storage = cache(root.path(), SHA1);
        let trees: Vec<_> = (0..count)
            .map(|i| {
                let entries = checked(
                    (0..entries_per_tree)
                        .map(|j| Entry {
                            name: format!("entry-{i:04}-{j:04}-padding.xml"),
                            oid: format!("{j:040x}"),
                            kind: EntryKind::Blob,
                            mode: "100644".into(),
                            size: 42,
                        })
                        .collect(),
                )
                .unwrap();
                let oid = Format::Sha1.git_oid(&entries, &cancel).unwrap();
                (oid, entries)
            })
            .collect();
        if entries_per_tree > 1 {
            assert!(
                trees
                    .iter()
                    .map(
                        |(_, entries)| TreeCache::encode_record(entries, MAX_RECORD_BYTES)
                            .unwrap()
                            .unwrap()
                            .len()
                    )
                    .sum::<usize>()
                    > sqlite::BATCH_BYTES
            );
        }
        assert!(
            storage
                .write_batch(
                    trees.iter().map(|(oid, entries)| (oid.as_str(), entries)),
                    &cancel
                )
                .unwrap()
        );
        for (oid, _) in &trees {
            assert!(
                !storage.path(oid).unwrap().exists(),
                "large proved subtree must not revert to per-tree files"
            );
        }
        drop(storage);
        let restarted = cache(root.path(), SHA1);
        for (oid, entries) in &trees {
            let actual = restarted.read(oid, &cancel).unwrap().unwrap();
            assert_eq!(actual.len(), entries.len());
            assert_eq!(Format::Sha1.git_oid(&actual, &cancel).unwrap(), *oid);
        }
    }
}
