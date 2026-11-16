use super::*;
use crate::engine::tests::{rows, source};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use cf_application::FactStore;
use std::sync::atomic::{AtomicUsize, Ordering};
#[path = "indexing_semantic_tests.rs"]
mod semantic_tests;

fn row(name: &str) -> serde_json::Value {
    rows()["cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["name"] == name)
        .unwrap()
        .clone()
}
fn replace(row: &mut serde_json::Value, path: &str, data: Vec<u8>) {
    row["source"][path] = serde_json::Value::String(STANDARD.encode(&data));
    if let Some(entry) = row["manifest"]["entries"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|e| e["path"] == path)
    {
        let mut hash = Sha1::new();
        hash.update(format!("blob {}\0", data.len()).as_bytes());
        hash.update(&data);
        entry["oid"] = serde_json::Value::String(hex::encode(hash.finalize()));
        entry["size"] = serde_json::json!(data.len());
    }
    row["manifest"]["revision"] = serde_json::Value::String("b".repeat(40));
}
fn replace_text(row: &mut serde_json::Value, path: &str, old: &str, new: &str) {
    let text = String::from_utf8(
        STANDARD
            .decode(row["source"][path].as_str().unwrap())
            .unwrap(),
    )
    .unwrap();
    assert!(text.contains(old));
    replace(row, path, text.replace(old, new).into_bytes());
}
#[test]
fn configuration_version_only_change_updates_manifest_without_reading_object_xml() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("source");
    let folder = temp.path().join("index");
    let mut data = row("required-values");
    replace_text(
        &mut data,
        "Configuration.xml",
        "<Comment/>",
        "<Comment/><Version>1.0.0</Version>",
    );
    let first = source(&data, &root, true);
    let cold = update(&first, &folder, &Cancellation::default(), &|_, _| {}).unwrap();
    let bytes = fs::read(&cold.path).unwrap();

    replace_text(
        &mut data,
        "Configuration.xml",
        "<Version>1.0.0</Version>",
        "<Version>1.0.1</Version>",
    );
    data["manifest"]["revision"] = serde_json::json!("c".repeat(40));
    let changed = source(&data, &root, true);
    let stale = status(&changed, &folder).unwrap();
    assert!(stale.found);
    assert!(!stale.ready);
    assert!(stale.path.is_none());
    assert_eq!(stale.preferred, cold.path);
    assert_eq!(fs::read(&cold.path).unwrap(), bytes);
    assert!(changed.reads.lock().unwrap().is_empty());

    let delta = update(&changed, &folder, &Cancellation::default(), &|_, _| {}).unwrap();
    assert_eq!(delta.path, cold.path);
    assert_eq!(delta.input_changed, 1);
    assert_eq!(delta.xml_reads, 0);
    assert_eq!((delta.added, delta.changed, delta.removed), (0, 0, 0));
    assert_eq!(delta.unchanged, cold.added);
    assert!(changed.reads.lock().unwrap().is_empty());

    let bytes = fs::read(&cold.path).unwrap();
    let modified = cold.path.metadata().unwrap().modified().unwrap();
    let restarted = source(&data, &root, true);
    assert!(status(&restarted, &folder).unwrap().ready);
    let reused = update(&restarted, &folder, &Cancellation::default(), &|_, _| {}).unwrap();
    assert_eq!(reused.xml_reads, 0);
    assert!(restarted.reads.lock().unwrap().is_empty());
    assert_eq!(fs::read(&cold.path).unwrap(), bytes);
    assert_eq!(cold.path.metadata().unwrap().modified().unwrap(), modified);
}
#[test]
fn persistent_git_index_reuses_after_restart_and_only_reads_the_xml_delta() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("source");
    let folder = temp.path().join("index");
    let mut data = row("required-values");
    let first = source(&data, &root, true);
    assert!(!status(&first, &folder).unwrap().found);
    let progress = AtomicUsize::new(0);
    let cold = update(&first, &folder, &Cancellation::default(), &|done, _| {
        progress.store(done, Ordering::Relaxed);
    })
    .unwrap();
    assert_eq!(cold.added, first.catalog().objects.len());
    assert!(cold.xml_reads > 0);
    assert_eq!(progress.load(Ordering::Relaxed), cold.inputs);
    let bytes = fs::read(&cold.path).unwrap();
    let modified = cold.path.metadata().unwrap().modified().unwrap();
    let restarted = source(&data, &root, true);
    assert!(status(&restarted, &folder).unwrap().ready);
    assert!(restarted.reads.lock().unwrap().is_empty());
    let unchanged = update(&restarted, &folder, &Cancellation::default(), &|_, _| {}).unwrap();
    assert_eq!(unchanged.xml_reads, 0);
    assert_eq!(fs::read(&cold.path).unwrap(), bytes);
    assert_eq!(cold.path.metadata().unwrap().modified().unwrap(), modified);
    replace(
        &mut data,
        "CommonModules/А/Ext/Module.bsl",
        b"BSL only change".to_vec(),
    );
    let bsl = source(&data, &root, true);
    assert!(status(&bsl, &folder).unwrap().ready);
    assert_eq!(
        update(&bsl, &folder, &Cancellation::default(), &|_, _| {})
            .unwrap()
            .xml_reads,
        0
    );
    assert_eq!(fs::read(&cold.path).unwrap(), bytes);
    replace_text(&mut data, "Enums/Статусы.xml", "Первый", "Изменённый");
    let changed = source(&data, &root, true);
    assert!(!status(&changed, &folder).unwrap().ready);
    let delta = update(&changed, &folder, &Cancellation::default(), &|_, _| {}).unwrap();
    assert_eq!(delta.xml_reads, 1);
    assert_eq!(delta.changed, 1);
    assert_eq!(delta.input_changed, 1);
    assert!(status(&changed, &folder).unwrap().ready);
    assert_eq!(
        changed
            .reads
            .lock()
            .unwrap()
            .keys()
            .cloned()
            .collect::<Vec<_>>(),
        vec!["Enums/Статусы.xml"]
    );
}
#[test]
fn v1_stays_read_only_and_explicit_update_builds_separate_source_facts() {
    let temp = tempfile::tempdir().unwrap();
    let src = source(
        &row("all-composite-types"),
        &temp.path().join("source"),
        true,
    );
    let folder = temp.path().join("index");
    fs::create_dir(&folder).unwrap();
    let legacy = folder.join("legacy.sqlite3");
    let data = row("all-composite-types");
    fs::write(
        &legacy,
        STANDARD
            .decode(data["legacy_index"].as_str().unwrap())
            .unwrap(),
    )
    .unwrap();
    let bytes = fs::read(&legacy).unwrap();
    let modified = legacy.metadata().unwrap().modified().unwrap();
    let before = status(&src, &folder).unwrap();
    assert!(before.found);
    assert!(
        !before.ready,
        "V1 has no origin/coverage/reader semantics proof"
    );
    assert!(before.path.is_none());
    assert!(!before.preferred.exists());
    let migrated = update(&src, &folder, &Cancellation::default(), &|_, _| {}).unwrap();
    assert!(migrated.migrated);
    assert!(
        migrated.xml_reads > 0,
        "migration must execute the new reader once"
    );
    let reads = src.reads.lock().unwrap();
    for object in src.catalog().objects.values() {
        assert!(
            reads.contains_key(&object.descriptor),
            "new facts must not be relabelled V1: {}",
            object.descriptor
        );
    }
    drop(reads);
    assert_eq!(bytes, fs::read(&legacy).unwrap());
    assert_eq!(modified, legacy.metadata().unwrap().modified().unwrap());
    assert!(status(&src, &folder).unwrap().ready);
}
#[test]
fn registrar_changes_and_removed_objects_are_incremental() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("source");
    let folder = temp.path().join("index");
    let mut data = row("register-resource-and-first-registrar");
    let old = source(&data, &root, true);
    update(&old, &folder, &Cancellation::default(), &|_, _| {}).unwrap();
    replace_text(
        &mut data,
        "Documents/Альфа.xml",
        "AccumulationRegister.Остатки",
        "AccumulationRegister.Чужой",
    );
    let changed = source(&data, &root, true);
    let delta = update(&changed, &folder, &Cancellation::default(), &|_, _| {}).unwrap();
    assert_eq!(delta.xml_reads, 1);
    assert_eq!(delta.registrars_changed, 1);
    let reader = IndexReader::open(&delta.path, Proof::Git(changed.manifest().unwrap())).unwrap();
    assert_eq!(
        reader
            .registrars("AccumulationRegister.Чужой", None)
            .unwrap(),
        Some(vec!["Document.Альфа".into()])
    );
    assert_eq!(
        reader
            .registrars("AccumulationRegister.Остатки", None)
            .unwrap(),
        Some(vec!["Document.Ясный".into()])
    );
    drop(reader);
    replace_text(
        &mut data,
        "Configuration.xml",
        "<Catalog>Организации</Catalog>",
        "",
    );
    data["source"]
        .as_object_mut()
        .unwrap()
        .remove("Catalogs/Организации.xml");
    data["manifest"]["entries"]
        .as_array_mut()
        .unwrap()
        .retain(|e| e["path"] != "Catalogs/Организации.xml");
    let removed = source(&data, &root, true);
    let delta = update(&removed, &folder, &Cancellation::default(), &|_, _| {}).unwrap();
    assert_eq!(delta.removed, 1);
    assert_eq!(delta.xml_reads, 0);
    assert!(status(&removed, &folder).unwrap().ready);
}
#[test]
fn cancellation_corrupt_blob_and_os_lock_preserve_previous_database() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("source");
    let folder = temp.path().join("index");
    let mut data = row("required-values");
    let initial = source(&data, &root, true);
    let first = update(&initial, &folder, &Cancellation::default(), &|_, _| {}).unwrap();
    let bytes = fs::read(&first.path).unwrap();
    replace_text(&mut data, "Enums/Статусы.xml", "Первый", "Изменённый");
    let changed = source(&data, &root, true);
    let cancel = Cancellation::default();
    assert!(update(&changed, &folder, &cancel, &|_, _| cancel.cancel()).is_err());
    assert_eq!(fs::read(&first.path).unwrap(), bytes);
    let lock = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(first.path.with_extension("sqlite3.lock"))
        .unwrap();
    fs2::FileExt::try_lock_exclusive(&lock).unwrap();
    assert!(update(&changed, &folder, &Cancellation::default(), &|_, _| {}).is_err());
    fs2::FileExt::unlock(&lock).unwrap();
    data["manifest"]["entries"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|e| e["path"] == "Enums/Статусы.xml")
        .unwrap()["oid"] = serde_json::json!("0".repeat(40));
    let corrupt = source(&data, &root, true);
    assert!(update(&corrupt, &folder, &Cancellation::default(), &|_, _| {}).is_err());
    assert_eq!(fs::read(&first.path).unwrap(), bytes);
}
#[test]
fn local_cache_is_a_checked_memo_and_index_folder_cannot_be_inside_export() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("source");
    let folder = temp.path().join("index");
    let mut data = row("required-values");
    let initial = source(&data, &root, false);
    assert!(path_for(&initial, &root.join("new-index")).is_err());
    assert!(path_for(&initial, &temp.path().join(".git/new-index")).is_err());
    let first = update(&initial, &folder, &Cancellation::default(), &|_, _| {}).unwrap();
    let reader = IndexReader::open(&first.path, Proof::Local(initial.identity())).unwrap();
    assert!(reader.lookup("Catalog.Тест", None).is_err());
    drop(reader);
    replace_text(&mut data, "Enums/Статусы.xml", "Первый", "Другой");
    let changed = source(&data, &root, false);
    let next = update(&changed, &folder, &Cancellation::default(), &|_, _| {}).unwrap();
    assert_eq!(next.changed, 1);
    assert_eq!(next.added, 0);
    assert_eq!(next.removed, 0);
    assert!(status(&changed, &folder).unwrap().ready);
}
#[test]
fn index_child_crash_probe() {
    let Ok(base) = std::env::var("CF_WIZARD_RUST_CRASH_TEST") else {
        return;
    };
    let base = PathBuf::from(base);
    let data: serde_json::Value =
        serde_json::from_slice(&fs::read(base.join("changed.json")).unwrap()).unwrap();
    let src = source(&data, &base.join("source"), true);
    let _ = update(
        &src,
        &base.join("index"),
        &Cancellation::default(),
        &|_, _| {
            fs::write(base.join("transaction-ready"), b"ready").unwrap();
            std::thread::sleep(std::time::Duration::from_secs(30));
        },
    );
}
#[test]
fn cross_process_lock_and_forced_crash_preserve_the_published_index() {
    use std::process::{Command, Stdio};
    let temp = tempfile::tempdir().unwrap();
    let base = temp.path();
    let root = base.join("source");
    let folder = base.join("index");
    let mut data = row("required-values");
    let original = source(&data, &root, true);
    let result = update(&original, &folder, &Cancellation::default(), &|_, _| {}).unwrap();
    let bytes = fs::read(&result.path).unwrap();
    let modified = result.path.metadata().unwrap().modified().unwrap();
    replace_text(&mut data, "Enums/Статусы.xml", "Первый", "Изменённый");
    fs::write(
        base.join("changed.json"),
        serde_json::to_vec(&data).unwrap(),
    )
    .unwrap();
    let mut cmd = Command::new(std::env::current_exe().unwrap());
    cmd.args([
        "--exact",
        "indexing::tests::index_child_crash_probe",
        "--nocapture",
    ])
    .env("CF_WIZARD_RUST_CRASH_TEST", base)
    .stdin(Stdio::null())
    .stdout(Stdio::null())
    .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x08000000);
    }
    let mut child = cmd.spawn().unwrap();
    struct ChildGuard<'a>(&'a mut std::process::Child);
    impl Drop for ChildGuard<'_> {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let guard = ChildGuard(&mut child);
    let clock = std::time::Instant::now();
    while !base.join("transaction-ready").exists() {
        assert!(
            clock.elapsed() < std::time::Duration::from_secs(10),
            "child did not reach transaction"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let changed = source(&data, &root, true);
    assert!(
        update(&changed, &folder, &Cancellation::default(), &|_, _| {}).is_err(),
        "other process holds writer lock"
    );
    guard.0.kill().unwrap();
    guard.0.wait().unwrap();
    drop(guard);
    assert_eq!(fs::read(&result.path).unwrap(), bytes);
    assert_eq!(
        result.path.metadata().unwrap().modified().unwrap(),
        modified
    );
    assert!(
        status(&original, &folder).unwrap().ready,
        "old committed snapshot survived the killed writer"
    );
    assert_eq!(
        update(&changed, &folder, &Cancellation::default(), &|_, _| {})
            .unwrap()
            .changed,
        1
    );
}
