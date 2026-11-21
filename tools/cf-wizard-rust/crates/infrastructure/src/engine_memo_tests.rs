//! Exercise real Session hits and invalidations, including downstream scans.
use super::tests::{Source, source};
use super::*;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use std::fs;

const ROOT: &str = "CommonModule.Root";
const TARGET: &str = "Catalog.Target";
const SIDE: &str = "Catalogs/Target/Forms/Screen/Ext/Form.xml";

#[test]
fn live_descriptor_rereads_share_text_and_detect_changes() {
    let (_dir, src) = fixture();
    let cancel = Cancellation::default();
    let mut session = Session::new(
        &src,
        None,
        &BTreeSet::from([ROOT.into()]),
        &BTreeSet::new(),
        &cancel,
    )
    .unwrap();
    let path = descriptor(TARGET).unwrap();
    let document = session.document(TARGET).unwrap();
    let reread = session.read(&path).unwrap();
    assert_eq!(src.reads.lock().unwrap()[&path], 2);
    assert!(Arc::ptr_eq(&document.view.xml.source, &reread));
    assert!(Arc::ptr_eq(&session.descriptors[TARGET], &reread));
    assert!(Arc::ptr_eq(&session.text_cache[&path], &reread));

    fs::write(src.local.root.join(&path), "<changed/>").unwrap();
    assert!(session.read(&path).is_err());
    assert_eq!(src.reads.lock().unwrap()[&path], 3);
}

#[test]
fn namespaced_minimum_field_uses_source_order_with_resource_priority() {
    let dir = tempfile::tempdir().unwrap();
    let files = BTreeMap::from([
        (
            "Configuration.xml",
            "<MetaDataObject><Configuration><Properties><Name>Namespaced</Name></Properties><ChildObjects><CommonModule>Root</CommonModule><InformationRegister>Balance</InformationRegister></ChildObjects></Configuration></MetaDataObject>",
        ),
        (
            "CommonModules/Root.xml",
            "<MetaDataObject><CommonModule><Properties><Name>Root</Name></Properties></CommonModule></MetaDataObject>",
        ),
        (
            "InformationRegisters/Balance.xml",
            "<m:MetaDataObject xmlns:m=\"http://v8.1c.ru/8.3/MDClasses\"><m:InformationRegister><m:Properties><m:Name>Balance</m:Name><m:WriteMode>Independent</m:WriteMode></m:Properties><m:ChildObjects><m:Attribute><m:Properties><m:Name>Extra</m:Name></m:Properties></m:Attribute><m:Dimension><m:Properties><m:Name>Key</m:Name></m:Properties></m:Dimension><m:Resource><m:Properties><m:Name>Amount</m:Name></m:Properties></m:Resource><m:Resource><m:Properties><m:Name>Later</m:Name></m:Properties></m:Resource></m:ChildObjects></m:InformationRegister></m:MetaDataObject>",
        ),
    ]);
    let encoded: BTreeMap<_, _> = files
        .into_iter()
        .map(|(path, xml)| (path, STANDARD.encode(xml)))
        .collect();
    let src = source(
        &serde_json::json!({"source": encoded}),
        &dir.path().join("source"),
        false,
    );
    let cancel = Cancellation::default();
    let selected = BTreeSet::from([ROOT.into()]);
    let excluded = BTreeSet::new();
    let mut session = Session::new(&src, None, &selected, &excluded, &cancel).unwrap();
    session
        .note(
            Reference {
                kind: "InformationRegister".into(),
                name: "Balance".into(),
                ..Reference::default()
            },
            ROOT,
        )
        .unwrap();
    session.closure().unwrap();
    while session.choose_structural().unwrap() {
        session.closure().unwrap();
    }
    let choices: Vec<_> = session
        .choices
        .iter()
        .map(|choice| (choice.rule.clone(), choice.chosen.clone(), choice.ordinal))
        .collect();
    assert_eq!(
        choices,
        vec![(
            "minimum_field".to_owned(),
            "InformationRegister.Balance.Resource.Amount".to_owned(),
            2
        )]
    );
    assert_eq!(
        session.states["InformationRegister.Balance"].spec.members,
        BTreeSet::from([("Resource".into(), "Amount".into())])
    );
    assert_eq!(
        src.reads.lock().unwrap()["InformationRegisters/Balance.xml"],
        1
    );
    let emitted = session.built["InformationRegister.Balance"]
        .output()
        .unwrap()
        .descriptor;
    assert!(emitted.contains("<m:Name>Amount</m:Name>"));
    assert!(!emitted.contains("<m:Name>Later</m:Name>"));
    assert!(!emitted.contains("<m:Name>Extra</m:Name>"));
}

#[test]
fn late_optional_to_required_upgrade_rechecks_missing_retained_files() {
    let (_dir, src) = fixture();
    fs::remove_file(src.local.root.join("Catalogs/Target/Forms/Screen.xml")).unwrap();
    let cancel = Cancellation::default();
    let mut session = Session::new(
        &src,
        None,
        &BTreeSet::from([ROOT.into()]),
        &BTreeSet::new(),
        &cancel,
    )
    .unwrap();
    let reference = Reference {
        kind: "Catalog".into(),
        name: "Target".into(),
        member: "Form".into(),
        member_name: "Screen".into(),
        ..Reference::default()
    };
    session.comment = true;
    session.note(reference.clone(), ROOT).unwrap();
    session.comment = false;
    session.closure().unwrap();
    while session.choose_structural().unwrap() {
        session.closure().unwrap();
    }
    assert!(session.diagnostics.is_empty());
    session.note(reference, ROOT).unwrap();
    session.closure().unwrap();
    assert!(
        session
            .diagnostics
            .iter()
            .any(|d| d.kind == DiagnosticKind::MissingFile
                && d.target == "Catalogs/Target/Forms/Screen.xml"),
        "{:?}",
        session.diagnostics
    );
}

fn fixture() -> (tempfile::TempDir, Source) {
    let dir = tempfile::tempdir().unwrap();
    let mut files = BTreeMap::from([
        ("Configuration.xml".to_owned(), "<MetaDataObject><Configuration><Properties><Name>MemoFixture</Name></Properties><ChildObjects><CommonModule>Root</CommonModule><Catalog>Target</Catalog><Catalog>A</Catalog><Catalog>B</Catalog><Catalog>Unrelated</Catalog></ChildObjects></Configuration></MetaDataObject>".to_owned()),
        ("CommonModules/Root.xml".into(), "<MetaDataObject><CommonModule><Properties><Name>Root</Name></Properties></CommonModule></MetaDataObject>".into()),
        ("Catalogs/Target.xml".into(), "<MetaDataObject xmlns:xr=\"http://v8.1c.ru/8.3/xcf/readable\"><Catalog><Properties><Name>Target</Name><Comment>first</Comment><Items><xr:Item>Catalog.A Catalog.B</xr:Item></Items></Properties><ChildObjects><Attribute><Properties><Name>Extra</Name></Properties></Attribute><Form>Screen</Form></ChildObjects></Catalog></MetaDataObject>".into()),
        ("Catalogs/Target/Forms/Screen.xml".into(), "<MetaDataObject><Form><Properties><Name>Screen</Name></Properties></Form></MetaDataObject>".into()),
        (SIDE.into(), "<Form/>".into()),
        ("Catalogs/Target/Ext/Predefined.xml".into(), "<PredefinedData/>".into()),
    ]);
    for name in ["A", "B", "Unrelated"] {
        files.insert(format!("Catalogs/{name}.xml"), format!("<MetaDataObject><Catalog><Properties><Name>{name}</Name></Properties></Catalog></MetaDataObject>"));
    }
    let encoded: BTreeMap<_, _> = files
        .into_iter()
        .map(|(p, text)| (p, STANDARD.encode(text)))
        .collect();
    let src = source(
        &serde_json::json!({"source":encoded}),
        &dir.path().join("source"),
        false,
    );
    (dir, src)
}

fn add(session: &mut Session<'_>, name: &str) {
    session
        .note(
            Reference {
                kind: "Catalog".into(),
                name: name.into(),
                ..Reference::default()
            },
            ROOT,
        )
        .unwrap();
}

fn prime(session: &mut Session<'_>) {
    session
        .note(
            Reference {
                kind: "Catalog".into(),
                name: "Target".into(),
                member: "Form".into(),
                member_name: "Screen".into(),
                ..Reference::default()
            },
            ROOT,
        )
        .unwrap();
    session.closure().unwrap();
    assert_eq!(session.stub_builds[TARGET], 1);
}

fn replay(session: &mut Session<'_>) {
    session.mark(TARGET);
    session.closure().unwrap();
}

fn assert_fresh_output(session: &Session<'_>) {
    let present = session
        .selected
        .iter()
        .chain(session.states.keys())
        .cloned()
        .collect();
    let fresh = crate::stubs::stub_descriptor(
        &session.descriptors[TARGET],
        TARGET,
        &session.states[TARGET].spec,
        &present,
    )
    .unwrap();
    let actual = session.built[TARGET].output().unwrap();
    assert_eq!(actual.descriptor, fresh.descriptor);
    assert_eq!(actual.patterns, fresh.patterns);
}

#[test]
fn explicit_projection_memo_keeps_side_checks_and_requires_both_list_targets() {
    let (_dir, src) = fixture();
    let cancel = Cancellation::default();
    let selected = BTreeSet::from([ROOT.into()]);
    let excluded = BTreeSet::new();
    let mut session = Session::new(&src, None, &selected, &excluded, &cancel).unwrap();
    prime(&mut session);
    assert_eq!(src.reads.lock().unwrap()[SIDE], 1);
    assert!(session.states[TARGET].spec.property_items["Items"].is_empty());

    replay(&mut session);
    assert_eq!(session.stub_builds[TARGET], 1, "unchanged inputs must hit");
    assert_eq!(
        src.reads.lock().unwrap()[SIDE],
        2,
        "a hit must rescan side files"
    );

    add(&mut session, "Unrelated");
    replay(&mut session);
    assert_eq!(
        session.stub_builds[TARGET], 1,
        "unrelated membership must hit"
    );
    add(&mut session, "B");
    replay(&mut session);
    assert_eq!(
        session.stub_builds[TARGET], 1,
        "one missing conjunct leaves the explicit projection unchanged"
    );

    add(&mut session, "A");
    replay(&mut session);
    assert_eq!(
        session.stub_builds[TARGET], 2,
        "observed false-to-true must rebuild"
    );
    assert_eq!(
        session.states[TARGET].spec.property_items["Items"],
        BTreeSet::from([0])
    );
    assert_fresh_output(&session);
    for key in ["Catalog.A", "Catalog.B"] {
        assert!(
            session.states[key].causes[TARGET],
            "output scan must retain required causes"
        );
    }
}

#[test]
fn explicit_projection_memo_waits_for_the_second_conjunct() {
    let (_dir, src) = fixture();
    let cancel = Cancellation::default();
    let selected = BTreeSet::from([ROOT.into()]);
    let excluded = BTreeSet::new();
    let mut session = Session::new(&src, None, &selected, &excluded, &cancel).unwrap();
    prime(&mut session);
    add(&mut session, "A");
    replay(&mut session);
    assert_eq!(session.stub_builds[TARGET], 1);
    assert!(session.states[TARGET].spec.property_items["Items"].is_empty());
    assert_fresh_output(&session);
    add(&mut session, "B");
    replay(&mut session);
    assert_eq!(session.stub_builds[TARGET], 2);
    assert!(
        session.built[TARGET]
            .output()
            .unwrap()
            .descriptor
            .contains("Catalog.A Catalog.B")
    );
    assert_fresh_output(&session);
}

#[test]
fn session_memo_invalidates_every_stub_spec_field() {
    for field in ["members", "predefined", "based_on", "by_value"] {
        let (_dir, src) = fixture();
        let cancel = Cancellation::default();
        let selected = BTreeSet::from([ROOT.into()]);
        let excluded = BTreeSet::new();
        let mut session = Session::new(&src, None, &selected, &excluded, &cancel).unwrap();
        prime(&mut session);
        let spec = &mut session.states.get_mut(TARGET).unwrap().spec;
        match field {
            "members" => {
                spec.members.insert(("Attribute".into(), "Extra".into()));
            }
            "predefined" => spec.predefined = true,
            "based_on" => spec.based_on = true,
            "by_value" => spec.by_value = true,
            _ => unreachable!(),
        }
        replay(&mut session);
        assert_eq!(session.stub_builds[TARGET], 2, "{field} must invalidate");
        assert_eq!(session.stub_inputs[TARGET].0, session.states[TARGET].spec);
        assert_fresh_output(&session);
        replay(&mut session);
        assert_eq!(
            session.stub_builds[TARGET], 2,
            "stable {field} must hit again"
        );
    }
}

#[test]
fn session_memo_does_not_leak_into_a_new_session_after_descriptor_changes() {
    let (_dir, src) = fixture();
    let cancel = Cancellation::default();
    let selected = BTreeSet::from([ROOT.into()]);
    let excluded = BTreeSet::new();
    let mut first = Session::new(&src, None, &selected, &excluded, &cancel).unwrap();
    prime(&mut first);
    let descriptor = src.local.root.join("Catalogs/Target.xml");
    let text = fs::read_to_string(&descriptor)
        .unwrap()
        .replace("<Comment>first</Comment>", "<Comment>second</Comment>");
    fs::write(&descriptor, text).unwrap();
    let mut next = Session::new(&src, None, &selected, &excluded, &cancel).unwrap();
    assert!(next.stub_inputs.is_empty());
    prime(&mut next);
    assert!(
        first.built[TARGET]
            .output()
            .unwrap()
            .descriptor
            .contains("<Comment>first</Comment>")
    );
    assert!(
        next.built[TARGET]
            .output()
            .unwrap()
            .descriptor
            .contains("<Comment>second</Comment>")
    );
    assert_fresh_output(&next);
}

#[test]
fn session_memo_hit_still_rejects_a_changed_side_file() {
    let (_dir, src) = fixture();
    let cancel = Cancellation::default();
    let selected = BTreeSet::from([ROOT.into()]);
    let excluded = BTreeSet::new();
    let mut session = Session::new(&src, None, &selected, &excluded, &cancel).unwrap();
    prime(&mut session);
    fs::write(src.local.root.join(SIDE), "<Form>changed</Form>").unwrap();
    session.mark(TARGET);
    let error = session.closure().unwrap_err();
    assert!(
        error
            .to_string()
            .contains("изменились во время предпросмотра")
    );
    assert_eq!(
        session.stub_builds[TARGET], 1,
        "the error must be detected on a cache hit"
    );
    assert_eq!(src.reads.lock().unwrap()[SIDE], 2);
}

#[test]
fn evicted_documents_preserve_the_frozen_package_and_branch() {
    let inputs = tests::rows();
    let row = &inputs["cases"][1];
    let temp = tempfile::tempdir().unwrap();
    let cancel = Cancellation::default();
    let selected = serde_json::from_value(row["selected"].clone()).unwrap();
    let excluded = BTreeSet::new();
    for pinned in [false, true] {
        let src = tests::source(row, &temp.path().join(format!("source-{pinned}")), pinned);
        let report = crate::indexing::update(
            &src,
            &temp.path().join(format!("index-{pinned}")),
            &cancel,
            &|_, _| {},
        )
        .unwrap();
        let index = if pinned {
            Some(
                crate::index::IndexReader::open(
                    &report.path,
                    crate::index::Proof::Git(src.manifest().unwrap()),
                )
                .unwrap(),
            )
        } else {
            None
        };
        let index = index.as_ref().map(|index| index as &dyn FactStore);
        let baseline = prepare(&src, index, &selected, &excluded, &cancel).unwrap();
        let mut limited = Session::new(&src, index, &selected, &excluded, &cancel).unwrap();
        limited.configure(&RustEngine::with_document_cache_budget(Some(0)));
        let key = selected.first().unwrap();
        let document = limited.document(key).unwrap();
        let expected = document.facts.clone();
        let weak = Arc::downgrade(&document);
        drop(document);
        assert!(weak.upgrade().is_none());
        assert!(limited.source_documents.is_empty());
        assert_eq!(limited.document(key).unwrap().facts, expected);
        assert_eq!(limited.cache_budget.replays, 1);
        let budgeted = limited.finish().unwrap();
        assert!(baseline.compiled.is_some());
        assert_eq!(baseline.compiled, budgeted.compiled);
        let excluded = BTreeSet::from(["Catalog.Тест".into()]);
        let expected = reprepare(&src, &baseline, &excluded, &cancel).unwrap();
        let actual = reprepare(&src, &budgeted, &excluded, &cancel).unwrap();
        assert_eq!(actual.engine.document_cache_bytes, Some(0));
        assert_eq!(actual.compiled, expected.compiled);
    }
}

#[test]
fn measured_headroom_releases_cached_xml_before_the_next_parse() {
    let inputs = tests::rows();
    let row = &inputs["cases"][1];
    let temp = tempfile::tempdir().unwrap();
    let src = tests::source(row, temp.path(), false);
    let selected = serde_json::from_value(row["selected"].clone()).unwrap();
    let cancel = Cancellation::default();
    let mut session = Session::new(&src, None, &selected, &BTreeSet::new(), &cancel).unwrap();
    let budget = 1 << 20;
    let remaining = Arc::new(std::sync::atomic::AtomicU64::new(budget));
    let sample = Arc::clone(&remaining);
    session.configure(
        &RustEngine::with_document_cache_budget(Some(budget)).with_cache_headroom(Arc::new(
            move || Some(sample.load(std::sync::atomic::Ordering::Relaxed)),
        )),
    );
    let first = session.document(selected.first().unwrap()).unwrap();
    let weak = Arc::downgrade(&first);
    drop(first);
    assert!(weak.upgrade().is_some());
    remaining.store(0, std::sync::atomic::Ordering::Relaxed);
    session.cache_budget.resample_on_next_demand();
    session.document("Enum.Статусы").unwrap();
    assert!(weak.upgrade().is_none());
    assert!(session.source_documents.is_empty());
    assert!(session.cache_budget.evictions > 0);
    assert!(session.cache_budget.high_water <= budget);
}
