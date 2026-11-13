//! Exercise real Session hits and invalidations, including downstream scans.
use super::tests::{Source, source};
use super::*;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use std::fs;

const ROOT: &str = "CommonModule.Root";
const TARGET: &str = "Catalog.Target";
const SIDE: &str = "Catalogs/Target/Forms/Screen/Ext/Form.xml";

fn fixture() -> (tempfile::TempDir, Source) {
    let dir = tempfile::tempdir().unwrap();
    let mut files = BTreeMap::from([
        ("Configuration.xml".to_owned(), "<MetaDataObject><Configuration><Properties><Name>MemoFixture</Name></Properties><ChildObjects><CommonModule>Root</CommonModule><Catalog>Target</Catalog><Catalog>A</Catalog><Catalog>B</Catalog><Catalog>Unrelated</Catalog></ChildObjects></Configuration></MetaDataObject>".to_owned()),
        ("CommonModules/Root.xml".into(), "<MetaDataObject><CommonModule><Properties><Name>Root</Name></Properties></CommonModule></MetaDataObject>".into()),
        ("Catalogs/Target.xml".into(), "<MetaDataObject xmlns:xr=\"urn:test\"><Catalog><Properties><Name>Target</Name><Comment>first</Comment><Items><xr:Item>Catalog.A Catalog.B</xr:Item></Items></Properties><ChildObjects><Attribute><Properties><Name>Extra</Name></Properties></Attribute><Form>Screen</Form></ChildObjects></Catalog></MetaDataObject>".into()),
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
    assert_eq!(session.built[TARGET].output.descriptor, fresh.descriptor);
    assert_eq!(session.built[TARGET].output.patterns, fresh.patterns);
}

#[test]
fn session_memo_hits_keep_side_scans_and_short_circuited_inputs_are_invalidated() {
    let (_dir, src) = fixture();
    let cancel = Cancellation::default();
    let selected = BTreeSet::from([ROOT.into()]);
    let excluded = BTreeSet::new();
    let mut session = Session::new(&src, None, &selected, &excluded, &cancel).unwrap();
    prime(&mut session);
    assert_eq!(src.reads.lock().unwrap()[SIDE], 1);
    assert_eq!(
        session.stub_inputs[TARGET].1,
        BTreeMap::from([("Catalog.A".into(), false)])
    );

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
        "B was not observed while A was absent"
    );

    add(&mut session, "A");
    replay(&mut session);
    assert_eq!(
        session.stub_builds[TARGET], 2,
        "observed false-to-true must rebuild"
    );
    assert_eq!(
        session.stub_inputs[TARGET].1,
        BTreeMap::from([("Catalog.A".into(), true), ("Catalog.B".into(), true),])
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
fn session_memo_expands_trace_after_a_and_rebuilds_after_observed_b_changes() {
    let (_dir, src) = fixture();
    let cancel = Cancellation::default();
    let selected = BTreeSet::from([ROOT.into()]);
    let excluded = BTreeSet::new();
    let mut session = Session::new(&src, None, &selected, &excluded, &cancel).unwrap();
    prime(&mut session);
    add(&mut session, "A");
    replay(&mut session);
    assert_eq!(session.stub_builds[TARGET], 2);
    assert_eq!(
        session.stub_inputs[TARGET].1,
        BTreeMap::from([("Catalog.A".into(), true), ("Catalog.B".into(), false),])
    );
    assert_fresh_output(&session);
    add(&mut session, "B");
    replay(&mut session);
    assert_eq!(session.stub_builds[TARGET], 3);
    assert!(
        session.built[TARGET]
            .output
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
            .output
            .descriptor
            .contains("<Comment>first</Comment>")
    );
    assert!(
        next.built[TARGET]
            .output
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
