//! Independent negative cases from the source/plan contracts, not writer output.
use super::*;
use base64::{Engine as _, engine::general_purpose::STANDARD};

pub(super) fn source(files: &[(&str, String)], root: &std::path::Path) -> tests::Source {
    let rows: BTreeMap<_, _> = files
        .iter()
        .map(|(path, text)| (*path, STANDARD.encode(text)))
        .collect();
    tests::source(&serde_json::json!({"source":rows}), root, false)
}
pub(super) fn configuration(children: &str) -> String {
    format!(
        "<MetaDataObject><Configuration><Properties><Name>Contracts</Name></Properties><ChildObjects>{children}</ChildObjects></Configuration></MetaDataObject>"
    )
}
pub(super) fn root(comment: &str) -> String {
    format!(
        "<MetaDataObject><CommonModule><Properties><Name>Root</Name><Comment>{comment}</Comment></Properties></CommonModule></MetaDataObject>"
    )
}
#[test]
fn first_candidate_policy_prefers_a_present_canonical_key() {
    let dir = tempfile::tempdir().unwrap();
    let catalog = |name| {
        format!(
            "<MetaDataObject><Catalog><Properties><Name>{name}</Name></Properties><ChildObjects><Attribute><Properties><Name>Shared</Name></Properties></Attribute></ChildObjects></Catalog></MetaDataObject>"
        )
    };
    let src = source(
        &[
            (
                "Configuration.xml",
                configuration(
                    "<CommonModule>Root</CommonModule><Catalog>A</Catalog><Catalog>B</Catalog>",
                ),
            ),
            ("CommonModules/Root.xml", root("")),
            ("Catalogs/A.xml", catalog("A")),
            ("Catalogs/B.xml", catalog("B")),
        ],
        &dir.path().join("source"),
    );
    let cancel = Cancellation::default();
    let selected = BTreeSet::from(["CommonModule.Root".into()]);
    let excluded = BTreeSet::new();
    let mut session = Session::new(&src, None, &selected, &excluded, &cancel).unwrap();
    session
        .note(
            Reference {
                kind: "Catalog".into(),
                name: "B".into(),
                ..Default::default()
            },
            "CommonModule.Root",
        )
        .unwrap();
    assert_eq!(
        session
            .having(vec!["Catalog.a".into(), "Catalog.b".into()], "Shared")
            .unwrap(),
        BTreeSet::from(["Catalog.B".into()])
    );
}
#[test]
fn a_known_required_weak_target_with_a_missing_descriptor_blocks_preview() {
    let dir = tempfile::tempdir().unwrap();
    let src = source(&[("Configuration.xml", configuration("<CommonModule>Root</CommonModule><Catalog>Missing</Catalog>")), ("CommonModules/Root.xml", root("Catalog.Missing")), ("Catalogs/Missing.xml", "<MetaDataObject><Catalog><Properties><Name>Missing</Name></Properties></Catalog></MetaDataObject>".into())], &dir.path().join("source"));
    // Preserve the declared catalog entry while the observed file is absent.
    std::fs::remove_file(src.local.root.join("Catalogs/Missing.xml")).unwrap();
    let recipe = prepare(
        &src,
        None,
        &BTreeSet::from(["CommonModule.Root".into()]),
        &BTreeSet::new(),
        &Cancellation::default(),
    )
    .unwrap();
    assert!(!recipe.composition.complete());
    assert!(
        recipe
            .composition
            .diagnostics
            .iter()
            .any(|d| d.kind == DiagnosticKind::MissingObject && d.target == "Catalog.Missing")
    );
}
#[test]
fn static_form_query_missing_source_is_an_addressed_blocking_error() {
    let dir = tempfile::tempdir().unwrap();
    let src = source(&[("Configuration.xml", configuration("<CommonForm>List</CommonForm>")), ("CommonForms/List.xml", "<MetaDataObject><CommonForm><Properties><Name>List</Name></Properties></CommonForm></MetaDataObject>".into()), ("CommonForms/List/Ext/Form.xml", "<Form><Attributes><Attribute name=\"List\"><Settings><QueryText>SELECT M.Ref FROM Catalog.Missing AS M</QueryText></Settings></Attribute></Attributes></Form>".into())], &dir.path().join("source"));
    let recipe = prepare(
        &src,
        None,
        &BTreeSet::from(["CommonForm.List".into()]),
        &BTreeSet::new(),
        &Cancellation::default(),
    )
    .unwrap();
    assert!(!recipe.composition.complete());
    assert!(
        recipe
            .composition
            .diagnostics
            .iter()
            .any(|d| d.target == "Catalog.Missing" && d.source.ends_with("Form.xml"))
    );
}

#[test]
fn every_missing_static_from_list_source_blocks_with_its_xml_origin() {
    for (query, missing) in [
        (
            "SELECT A.Ref, M.Ref FROM Catalog.Present AS A, Catalog.Missing AS M",
            vec!["Catalog.Missing"],
        ),
        (
            "SELECT A.Ref FROM Catalog.FirstMissing A, Catalog.SecondMissing B",
            vec!["Catalog.FirstMissing", "Catalog.SecondMissing"],
        ),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let src=source(&[
            ("Configuration.xml",configuration("<CommonForm>List</CommonForm><Catalog>Present</Catalog>")),
            ("Catalogs/Present.xml","<MetaDataObject><Catalog><Properties><Name>Present</Name></Properties></Catalog></MetaDataObject>".into()),
            ("CommonForms/List.xml","<MetaDataObject><CommonForm><Properties><Name>List</Name></Properties></CommonForm></MetaDataObject>".into()),
            ("CommonForms/List/Ext/Form.xml",format!("<Form><Attributes><Attribute name=\"List\"><Settings><QueryText>{query}</QueryText></Settings></Attribute></Attributes></Form>"))
        ],&dir.path().join("source"));
        let recipe = prepare(
            &src,
            None,
            &BTreeSet::from(["CommonForm.List".into()]),
            &BTreeSet::new(),
            &Cancellation::default(),
        )
        .unwrap();
        assert!(!recipe.composition.complete());
        for target in missing {
            assert!(
                recipe
                    .composition
                    .diagnostics
                    .iter()
                    .any(|d| d.target == target
                        && d.kind == DiagnosticKind::MissingObject
                        && d.source.ends_with("Form.xml")),
                "{target}"
            );
        }
    }
}
#[test]
fn declared_language_cannot_be_omitted_from_the_frozen_package() {
    let dir = tempfile::tempdir().unwrap();
    let src = source(
        &[
            (
                "Configuration.xml",
                configuration("<CommonModule>Root</CommonModule><Language>Russian</Language>"),
            ),
            ("CommonModules/Root.xml", root("")),
        ],
        &dir.path().join("source"),
    );
    let result = prepare(
        &src,
        None,
        &BTreeSet::from(["CommonModule.Root".into()]),
        &BTreeSet::new(),
        &Cancellation::default(),
    );
    assert!(
        matches!(result, Err(Error::Invalid(ref error)) if error.contains("Languages/Russian.xml"))
    );
}
#[test]
fn foreign_resource_cannot_satisfy_the_register_minimum() {
    let dir = tempfile::tempdir().unwrap();
    let src = source(&[("Configuration.xml", configuration("<CommonModule>Root</CommonModule><InformationRegister>Data</InformationRegister>")), ("CommonModules/Root.xml", root("InformationRegister.Data")), ("InformationRegisters/Data.xml", "<MetaDataObject xmlns:f=\"urn:foreign\"><InformationRegister><Properties><Name>Data</Name><WriteMode>Independent</WriteMode></Properties><ChildObjects><f:Resource><Properties><Name>Ghost</Name></Properties></f:Resource><Resource><Properties><Name>Value</Name></Properties></Resource></ChildObjects></InformationRegister></MetaDataObject>".into())], &dir.path().join("source"));
    let recipe = prepare(
        &src,
        None,
        &BTreeSet::from(["CommonModule.Root".into()]),
        &BTreeSet::new(),
        &Cancellation::default(),
    )
    .unwrap();
    assert!(recipe.composition.complete());
    assert_eq!(
        recipe
            .composition
            .choices
            .iter()
            .map(|c| c.chosen.as_str())
            .collect::<Vec<_>>(),
        vec!["InformationRegister.Data.Resource.Value"]
    );
}
