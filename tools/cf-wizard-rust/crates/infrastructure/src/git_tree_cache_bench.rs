use super::*;

const TREES: usize = 10_000;
const ANCHOR: &str = "f343f5f678d693782f5288d7373f7d13a9537baa";

fn records(cancel: &Cancellation) -> Vec<(String, TreeEntries)> {
    let empty = Format::Sha1.git_oid(&TreeEntries::new(), cancel).unwrap();
    (0..TREES)
        .map(|tree| {
            let mut entries = vec![Entry {
                name: "Ext".into(),
                oid: empty.clone(),
                kind: EntryKind::Tree,
                mode: "040000".into(),
                size: 0,
            }];
            for file in 0..(2 + tree % 17) {
                let contents = format!("synthetic payload {tree} {file}");
                let mut oid = sha1::Sha1::new();
                oid.update(format!("blob {}\0", contents.len()));
                oid.update(contents.as_bytes());
                entries.push(Entry {
                    name: format!("Элемент{file:02}.xml"),
                    oid: hex::encode(oid.finalize()),
                    kind: EntryKind::Blob,
                    mode: "100644".into(),
                    size: contents.len() as u64,
                });
            }
            let entries = checked(entries).unwrap();
            let oid = Format::Sha1.git_oid(&entries, cancel).unwrap();
            (oid, entries)
        })
        .collect()
}

fn digest(trees: impl Iterator<Item = (impl AsRef<str>, TreeEntries)>) -> (String, u64) {
    let mut hash = Sha256::new();
    let mut bytes = 0;
    for (oid, entries) in trees {
        let record = TreeCache::encode_record(&entries, MAX_RECORD_BYTES)
            .unwrap()
            .unwrap();
        hash.update(oid.as_ref().as_bytes());
        hash.update((record.len() as u64).to_le_bytes());
        hash.update(&record);
        bytes += (record.len() - CHECKSUM_BYTES) as u64;
    }
    (hex::encode(hash.finalize()), bytes)
}

#[test]
#[ignore = "isolated synthetic storage benchmark; launched by the frozen runner"]
fn controlled_tree_storage_worker() {
    let root = PathBuf::from(std::env::var_os("CF_WIZARD_TREE_BENCH_ROOT").unwrap());
    assert!(root.is_absolute() && root.is_dir());
    let arm = std::env::var("CF_WIZARD_TREE_BENCH_ARM").unwrap();
    let phase = std::env::var("CF_WIZARD_TREE_BENCH_PHASE").unwrap();
    let cancel = Cancellation::default();
    let mut cache = TreeCache::new(
        &root,
        &Connection {
            server: "https://benchmark.invalid".into(),
            repository: "fixture/synthetic".into(),
            ..Connection::default()
        },
        ANCHOR,
    )
    .unwrap();
    let sqlite = cache.sqlite_path_for_test().to_owned();
    if arm == "legacy" {
        cache.legacy_only_for_test();
    } else {
        assert_eq!(arm, "sqlite");
    }
    let fixture = records(&cancel);
    let expected = digest(fixture.iter().cloned());
    let mut imported = None;
    let mut found = Vec::with_capacity(TREES);
    let started = Instant::now();
    match phase.as_str() {
        "first" => {
            let stored = cache
                .write_batch(
                    fixture.iter().map(|(oid, rows)| (oid.as_str(), rows)),
                    &cancel,
                )
                .unwrap();
            assert_eq!(stored, arm == "sqlite");
        }
        "restart" => {
            for (oid, _) in &fixture {
                found.push(cache.read(oid, &cancel).unwrap().unwrap());
            }
        }
        "import" => {
            assert_eq!(arm, "sqlite");
            let report = cache.import_legacy(&cancel).unwrap();
            assert_eq!(report.scanned, TREES);
            assert_eq!(report.imported, TREES);
            assert_eq!(
                report.rejected + report.oversized + report.failed_batches,
                0
            );
            imported = Some(report.committed_batches);
        }
        _ => panic!("unexpected benchmark phase {phase}"),
    }
    let elapsed = started.elapsed().as_nanos() as u64;
    assert_eq!(sqlite.is_file(), arm == "sqlite");
    assert_eq!(
        cache.directory.is_dir(),
        arm == "legacy" || phase == "import"
    );
    if phase == "restart" {
        let actual = digest(
            fixture
                .iter()
                .zip(found)
                .map(|((oid, _), rows)| (oid, rows)),
        );
        assert_eq!(actual, expected);
    }
    if phase == "import" {
        let store = cache.sqlite.as_ref().unwrap();
        let actual = digest(fixture.iter().map(|(oid, _)| {
            let legacy = cache
                .legacy_record(oid, MAX_RECORD_BYTES, &cancel)
                .unwrap()
                .unwrap();
            let sqlite = store.read(oid, &cancel).unwrap().unwrap();
            assert_eq!(legacy, sqlite);
            (oid, cache.decode_record(oid, &sqlite, &cancel).unwrap())
        }));
        assert_eq!(actual, expected);
    }
    let (pager, mmap) = cache
        .sqlite
        .as_ref()
        .map_or((0, 0), sqlite::Store::effective_pager);
    println!(
        "TREE_STORAGE_RESULT {}",
        serde_json::json!({
            "pid": std::process::id(),
            "arm": arm,
            "phase": phase,
            "tree_count": TREES,
            "batch_size": sqlite::BATCH_ROWS,
            "wall_ns": elapsed,
            "fixture_sha256": expected.0,
            "expanded_json_bytes": expected.1,
            "configured_pager_bytes": pager * 1024,
            "effective_mmap_bytes": mmap,
            "verified_readback": phase != "first",
            "import_committed_batches": imported
        })
    );
}
