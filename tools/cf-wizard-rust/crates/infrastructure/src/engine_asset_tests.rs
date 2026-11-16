use super::*;
use crate::{
    canonical::{GitInputs, InputEntry},
    index::{IndexReader, Proof},
    indexing,
};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use cf_application::FactStore;
use sha1::Sha1;
use std::fs;

fn row(generated: bool) -> serde_json::Value {
    let mut row: serde_json::Value =
        serde_json::from_str(include_str!("../../../tests/fixtures/asset-resources.json")).unwrap();
    if generated {
        let source = row["source"].as_object_mut().unwrap();
        let doc = String::from_utf8(STANDARD.decode(source["Documents/AssetDocument.xml"].as_str().unwrap()).unwrap()).unwrap()
            .replace("<Template>Card</Template>", "<Template>Card</Template><Attribute uuid=\"00000000-0000-4000-8000-000000000088\"><Properties><Name>Carrier</Name><Type><v8:Type>cfg:CatalogRef.Carrier</v8:Type></Type></Properties></Attribute>");
        source.insert(
            "Documents/AssetDocument.xml".into(),
            STANDARD.encode(doc).into(),
        );
        let template = "Documents/AssetDocument/Templates/Card/Ext/Template.xml";
        let xml = String::from_utf8(STANDARD.decode(source[template].as_str().unwrap()).unwrap())
            .unwrap()
            .replace("p:probecamera", "p:Change");
        source.insert(template.into(), STANDARD.encode(xml).into());
    }
    let mut entries = vec![];
    for (path, data) in row["source"].as_object().unwrap() {
        let owner = if path == "Configuration.xml" {
            String::new()
        } else if path.split('/').count() == 2 && path.ends_with(".xml") {
            let (folder, file) = path.split_once('/').unwrap();
            let kind = TYPE_FOLDERS.iter().find(|(_, f)| *f == folder).unwrap().0;
            format!("{kind}.{}", file.trim_end_matches(".xml"))
        } else {
            continue;
        };
        let bytes = STANDARD.decode(data.as_str().unwrap()).unwrap();
        let mut hash = Sha1::new();
        hash.update(format!("blob {}\0", bytes.len()).as_bytes());
        hash.update(&bytes);
        entries.push(InputEntry {
            path: path.clone(),
            oid: hex::encode(hash.finalize()),
            size: bytes.len() as u64,
            roles: if path == "Configuration.xml" {
                "configuration"
            } else if owner.starts_with("Document.") {
                "descriptor,registrar"
            } else {
                "descriptor"
            }
            .into(),
            owner,
        });
    }
    row["manifest"] = serde_json::to_value(GitInputs {
        source_key: "git:asset-fixture".into(),
        revision: "1".repeat(40),
        entries,
    })
    .unwrap();
    row
}
fn selected() -> BTreeSet<String> {
    BTreeSet::from([
        "Document.AssetDocument".into(),
        "Subsystem.ResourcePanel".into(),
    ])
}

#[test]
fn complete_resources_match_with_local_git_and_sqlite() {
    for pinned in [false, true] {
        for indexed in [false, true] {
            let temp = tempfile::tempdir().unwrap();
            let fixture = row(false);
            let source = tests::source(&fixture, &temp.path().join("source"), pinned);
            let cancel = Cancellation::default();
            let saved = if indexed {
                let update =
                    indexing::update(&source, &temp.path().join("index"), &cancel, &|_, _| {})
                        .unwrap();
                Some(
                    IndexReader::open(
                        &update.path,
                        if pinned {
                            Proof::Git(source.manifest().unwrap())
                        } else {
                            Proof::Local(source.identity())
                        },
                    )
                    .unwrap(),
                )
            } else {
                None
            };
            source.reads.lock().unwrap().clear();
            let wanted = selected();
            let plan = RustEngine
                .preview(
                    &source,
                    saved.as_ref().map(|s| s as &dyn FactStore),
                    &wanted,
                    &BTreeSet::new(),
                    &cancel,
                )
                .unwrap();
            assert_eq!(plan.composition().selected, wanted);
            for key in ["CommonPicture.ProbeCamera", "Style.ProbeStyle"] {
                assert!(plan.composition().stubs[key].required());
            }
            assert!(
                !plan
                    .composition()
                    .stubs
                    .contains_key("CommonPicture.Change")
            );
            assert!(
                !source
                    .reads
                    .lock()
                    .unwrap()
                    .contains_key("CommonPictures/Unused.xml")
            );
            let stage = temp.path().join("stage");
            let package = plan
                .write(
                    &source,
                    &BTreeSet::new(),
                    &stage,
                    &temp.path().join("merge.xml"),
                    &cancel,
                )
                .unwrap();
            assert_eq!(package.readback.full_dependencies.len(), 2);
            assert!(package.readback.stub_descriptors.is_empty());
            let settings = fs::read_to_string(&package.settings).unwrap();
            for key in ["CommonPicture.ProbeCamera", "Style.ProbeStyle"] {
                assert!(settings.contains(&format!("<Object fullNameInSecondConfiguration=\"{key}\">\r\n\t\t\t<MergeRule>MergePrioritizingMainConfiguration</MergeRule>")));
            }
            package
                .readback
                .validate(&package.composition, source.catalog(), &package.files)
                .unwrap();
            for path in [
                "CommonPictures/ProbeCamera.xml",
                "CommonPictures/ProbeCamera/Ext/Picture.xml",
                "CommonPictures/ProbeCamera/Ext/Picture/Picture.zip",
                "Styles/ProbeStyle.xml",
                "Styles/ProbeStyle/Ext/Style.xml",
            ] {
                let expected = STANDARD
                    .decode(fixture["source"][path].as_str().unwrap())
                    .unwrap();
                assert_eq!(
                    fs::read(stage.join(path)).unwrap(),
                    expected,
                    "{path}, pinned={pinned}, indexed={indexed}"
                );
                assert_eq!(plan.composition().source_hashes[path], sha256(&expected));
            }
        }
    }
}

#[test]
fn retained_generated_descriptor_still_expands_picture_dependencies() {
    let temp = tempfile::tempdir().unwrap();
    let source = tests::source(&row(true), &temp.path().join("source"), false);
    let plan = RustEngine
        .preview(
            &source,
            None,
            &BTreeSet::from(["Document.AssetDocument".into()]),
            &BTreeSet::new(),
            &Cancellation::default(),
        )
        .unwrap();
    assert!(plan.composition().stubs.contains_key("Catalog.Carrier"));
    assert!(plan.composition().stubs["CommonPicture.ProbeCamera"].required());
}

#[test]
fn changed_or_added_resource_requires_a_new_preview() {
    for mutation in 0..3 {
        let temp = tempfile::tempdir().unwrap();
        let source = tests::source(&row(false), &temp.path().join("source"), false);
        let cancel = Cancellation::default();
        let plan = RustEngine
            .preview(&source, None, &selected(), &BTreeSet::new(), &cancel)
            .unwrap();
        let relative = match mutation {
            0 => "CommonPictures/ProbeCamera/Ext/Picture/Picture.zip",
            1 => "CommonPictures/ProbeCamera/Ext/Picture/new.png",
            _ => "Styles/ProbeStyle/Ext/Style.xml",
        };
        fs::write(source.local.root.join(relative), b"changed").unwrap();
        let stage = temp.path().join("stage");
        assert!(
            plan.write(
                &source,
                &BTreeSet::new(),
                &stage,
                &temp.path().join("merge.xml"),
                &cancel
            )
            .is_err()
        );
        assert!(!stage.exists());
    }
}

#[test]
fn a_structural_picture_cannot_be_excluded() {
    let temp = tempfile::tempdir().unwrap();
    let source = tests::source(&row(false), &temp.path().join("source"), false);
    let plan = RustEngine
        .preview(
            &source,
            None,
            &selected(),
            &BTreeSet::new(),
            &Cancellation::default(),
        )
        .unwrap();
    assert!(
        plan.composition()
            .validate_exclusions(&BTreeSet::from(["CommonPicture.ProbeCamera".into()]))
            .is_err()
    );
}
