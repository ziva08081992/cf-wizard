use super::*;
use cf_application::DependencyEngine;
use cf_domain::source_facts::Facet;

fn invalidate_object(db: &Connection, name: &str) {
    let (hash, payload): (Vec<u8>, Vec<u8>) = db
        .query_row(
            "SELECT content_hash,facts FROM objects WHERE name=?",
            [name],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    let mut facts =
        facts_cache::decode_v3(&payload, name, &hash, &FactVersions::current()).unwrap();
    facts
        .versions
        .readers
        .insert(Facet::Metadata, "fixture-previous-metadata-reader".into());
    let signature = facts_cache::signature(&facts.versions, facts_cache::OBJECT_FACETS).unwrap();
    db.execute(
        "UPDATE objects SET facts=? WHERE name=?",
        params![facts_cache::encode_v3(&facts).unwrap(), name],
    )
    .unwrap();
    db.execute(
        "UPDATE object_semantics SET signature=? WHERE name=?",
        params![signature, name],
    )
    .unwrap();
}
#[test]
fn identical_whole_manifest_does_not_upgrade_old_reader_results() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("source");
    let folder = temp.path().join("cache");
    let data = row("required-values");
    let src = source(&data, &root, true);
    let first = update(&src, &folder, &Cancellation::default(), &|_, _| {}).unwrap();
    let db = Connection::open(&first.path).unwrap();
    for name in src.catalog().objects.keys() {
        invalidate_object(&db, &name.0);
    }
    let mut old = FactVersions::current();
    old.readers
        .insert(Facet::Metadata, "fixture-previous-metadata-reader".into());
    db.execute(
        "UPDATE metadata SET value=? WHERE key='reader_versions'",
        [serde_json::to_string(&old).unwrap()],
    )
    .unwrap();
    drop(db);
    let current = source(&data, &root, true);
    assert!(!status(&current, &folder).unwrap().ready);
    assert!(
        current.reads.lock().unwrap().is_empty(),
        "readiness never migrates or fetches XML"
    );
    let refreshed = update(&current, &folder, &Cancellation::default(), &|_, _| {}).unwrap();
    let paths: BTreeSet<_> = current.reads.lock().unwrap().keys().cloned().collect();
    let expected = current
        .catalog()
        .objects
        .values()
        .map(|object| object.descriptor.clone())
        .collect();
    assert_eq!(paths, expected);
    assert_eq!(refreshed.metadata_parses, current.catalog().objects.len());
    assert!(status(&current, &folder).unwrap().ready);
}
#[test]
fn unchanged_oid_and_descriptor_sha_do_not_hide_a_stale_row() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("source");
    let folder = temp.path().join("cache");
    let mut data = row("required-values");
    let src = source(&data, &root, true);
    let first = update(&src, &folder, &Cancellation::default(), &|_, _| {}).unwrap();
    let db = Connection::open(&first.path).unwrap();
    invalidate_object(&db, "Enum.Статусы");
    drop(db);
    replace_text(
        &mut data,
        "Configuration.xml",
        "<Comment/>",
        "<Comment/><Version>2</Version>",
    );
    let changed = source(&data, &root, true);
    let original_entry = src
        .manifest()
        .unwrap()
        .entries
        .iter()
        .find(|e| e.path == "Enums/Статусы.xml")
        .unwrap();
    assert_eq!(
        Some(original_entry),
        changed
            .manifest()
            .unwrap()
            .entries
            .iter()
            .find(|e| e.path == original_entry.path)
    );
    let refreshed = update(&changed, &folder, &Cancellation::default(), &|_, _| {}).unwrap();
    assert_eq!(refreshed.xml_reads, 1);
    assert_eq!(refreshed.metadata_parses, 1);
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
    let reader =
        IndexReader::open(&refreshed.path, Proof::Git(changed.manifest().unwrap())).unwrap();
    assert_eq!(
        reader
            .source_facts("Enum.Статусы", None)
            .unwrap()
            .unwrap()
            .versions,
        FactVersions::current()
    );
}
#[test]
fn local_same_hash_refreshes_only_the_incompatible_extraction() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("source");
    let folder = temp.path().join("cache");
    let data = row("required-values");
    let src = source(&data, &root, false);
    let first = update(&src, &folder, &Cancellation::default(), &|_, _| {}).unwrap();
    let db = Connection::open(&first.path).unwrap();
    invalidate_object(&db, "Enum.Статусы");
    drop(db);
    let current = source(&data, &root, false);
    let refreshed = update(&current, &folder, &Cancellation::default(), &|_, _| {}).unwrap();
    assert_eq!(refreshed.changed, 1);
    assert_eq!(refreshed.metadata_parses, 1);
    assert!(status(&current, &folder).unwrap().ready);
}
#[test]
fn registrar_semantics_invalidates_same_hash_and_preserves_metadata_payload() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("source");
    let folder = temp.path().join("cache");
    let data = row("register-resource-and-first-registrar");
    let src = source(&data, &root, true);
    let first = update(&src, &folder, &Cancellation::default(), &|_, _| {}).unwrap();
    let db = Connection::open(&first.path).unwrap();
    let before: Vec<u8> = db
        .query_row(
            "SELECT facts FROM objects WHERE name='Document.Альфа'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let mut old = FactVersions::current();
    old.readers
        .insert(Facet::Registrars, "fixture-previous-register-reader".into());
    db.execute(
        "UPDATE document_semantics SET signature=? WHERE file='Альфа.xml'",
        [facts_cache::signature(&old, facts_cache::REGISTRAR_FACETS).unwrap()],
    )
    .unwrap();
    db.execute(
        "UPDATE registrar_links SET target='AccumulationRegister.Poisoned' WHERE file='Альфа.xml'",
        [],
    )
    .unwrap();
    drop(db);
    let current = source(&data, &root, true);
    assert!(!status(&current, &folder).unwrap().ready);
    let refreshed = update(&current, &folder, &Cancellation::default(), &|_, _| {}).unwrap();
    assert_eq!(
        (
            refreshed.xml_reads,
            refreshed.metadata_parses,
            refreshed.registrar_parses,
            refreshed.registrars_changed
        ),
        (1, 0, 1, 1)
    );
    assert_eq!(
        current
            .reads
            .lock()
            .unwrap()
            .keys()
            .cloned()
            .collect::<Vec<_>>(),
        vec!["Documents/Альфа.xml"]
    );
    let db = Connection::open(&refreshed.path).unwrap();
    let after: Vec<u8> = db
        .query_row(
            "SELECT facts FROM objects WHERE name='Document.Альфа'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        before, after,
        "unaffected metadata facet stays byte-identical"
    );
    drop(db);
    let reader =
        IndexReader::open(&refreshed.path, Proof::Git(current.manifest().unwrap())).unwrap();
    assert_eq!(
        reader
            .source_registrar_snapshot(None)
            .unwrap()
            .unwrap()
            .lookup("AccumulationRegister.Poisoned")
            .unwrap(),
        Vec::<String>::new()
    );
    assert_eq!(
        reader
            .source_registrar_snapshot(None)
            .unwrap()
            .unwrap()
            .lookup("AccumulationRegister.Остатки")
            .unwrap(),
        vec!["Document.Альфа", "Document.Ясный"]
    );
}
#[test]
fn rule_only_change_keeps_raw_facts_ready_without_xml_or_cache_writes() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("source");
    let folder = temp.path().join("cache");
    let data = row("required-values");
    let src = source(&data, &root, true);
    let first = update(&src, &folder, &Cancellation::default(), &|_, _| {}).unwrap();
    let db = Connection::open(&first.path).unwrap();
    db.execute(
        "UPDATE metadata SET value='rules-before' WHERE key='rule_profile'",
        [],
    )
    .unwrap();
    drop(db);
    let bytes = fs::read(&first.path).unwrap();
    let current = source(&data, &root, true);
    assert!(status(&current, &folder).unwrap().ready);
    let reader = IndexReader::open(&first.path, Proof::Git(current.manifest().unwrap())).unwrap();
    let facts = reader.source_facts("Enum.Статусы", None).unwrap().unwrap();
    assert!(facts.structural_facts().is_ok());
    drop(reader);
    let unchanged = update(&current, &folder, &Cancellation::default(), &|_, _| {}).unwrap();
    assert_eq!(
        (
            unchanged.xml_reads,
            unchanged.metadata_parses,
            unchanged.registrar_parses
        ),
        (0, 0, 0)
    );
    assert!(current.reads.lock().unwrap().is_empty());
    assert_eq!(fs::read(&first.path).unwrap(), bytes);
}
#[test]
fn cancel_immediately_before_publication_preserves_the_published_cache() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("source");
    let folder = temp.path().join("cache");
    let mut data = row("required-values");
    let src = source(&data, &root, true);
    let first = update(&src, &folder, &Cancellation::default(), &|_, _| {}).unwrap();
    let bytes = fs::read(&first.path).unwrap();
    replace_text(&mut data, "Enums/Статусы.xml", "Первый", "Обновлённый");
    let changed = source(&data, &root, true);
    let cancel = Cancellation::default();
    let after = AtomicUsize::new(0);
    let result = update_core(
        &changed,
        &folder,
        &cancel,
        &|_, _| {},
        |point| match point {
            PublicationPoint::BeforeCommit => cancel.cancel(),
            PublicationPoint::AfterCommit => {
                after.fetch_add(1, Ordering::Relaxed);
            }
        },
    );
    assert!(matches!(result, Err(Error::Cancelled)));
    assert_eq!(after.load(Ordering::Relaxed), 0);
    assert_eq!(fs::read(&first.path).unwrap(), bytes);
    assert!(status(&src, &folder).unwrap().ready);
}
#[test]
fn cancel_after_publication_reports_committed_success() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("source");
    let folder = temp.path().join("cache");
    let mut data = row("required-values");
    let src = source(&data, &root, true);
    let first = update(&src, &folder, &Cancellation::default(), &|_, _| {}).unwrap();
    let bytes = fs::read(&first.path).unwrap();
    replace_text(&mut data, "Enums/Статусы.xml", "Первый", "Обновлённый");
    let changed = source(&data, &root, true);
    let cancel = Cancellation::default();
    let result = update_core(&changed, &folder, &cancel, &|_, _| {}, |point| {
        if matches!(point, PublicationPoint::AfterCommit) {
            cancel.cancel();
        }
    });
    assert!(cancel.check().is_err(), "late request actually occurred");
    assert!(
        result.is_ok(),
        "committed publication cannot be retroactively Cancelled"
    );
    assert_ne!(fs::read(&first.path).unwrap(), bytes);
    assert!(status(&changed, &folder).unwrap().ready);
}
#[test]
fn ready_owned_registrar_cache_avoids_unrelated_document_reads() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("source");
    let folder = temp.path().join("cache");
    let data = row("register-resource-and-first-registrar");
    let first = source(&data, &root, true);
    let built = update(&first, &folder, &Cancellation::default(), &|_, _| {}).unwrap();
    let current = source(&data, &root, true);
    let reader = IndexReader::open(&built.path, Proof::Git(current.manifest().unwrap())).unwrap();
    let selected = serde_json::from_value(data["selected"].clone()).unwrap();
    let preview = crate::engine::RustEngine
        .preview(
            &current,
            Some(&reader),
            &selected,
            &BTreeSet::new(),
            &Cancellation::default(),
        )
        .unwrap();
    assert!(preview.composition().complete());
    assert!(
        !current
            .reads
            .lock()
            .unwrap()
            .contains_key("Documents/Ясный.xml"),
        "only V2 may prove unused registrar XML can be skipped"
    );
}
