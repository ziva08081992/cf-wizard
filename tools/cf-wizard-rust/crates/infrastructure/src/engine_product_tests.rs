//! Expectations were frozen before the product adapter was implemented.
use super::*;
use crate::{
    index::{IndexReader, Proof},
    indexing,
};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use std::fs;

fn product_rows() -> serde_json::Value {
    serde_json::from_slice(include_bytes!(
        "../../../tests/fixtures/datafrog-product-v1.json"
    ))
    .unwrap()
}
#[test]
fn independent_minimum_fields_use_a_bounded_structural_agenda() {
    for count in [20, 100] {
        let mut objects = String::new();
        let mut refs = String::new();
        let mut source = serde_json::Map::new();
        for i in 0..count {
            let name = format!("I{i:04}");
            objects += &format!("<InformationRegister>{name}</InformationRegister>");
            refs += &format!("InformationRegister.{name} ");
            source.insert(format!("InformationRegisters/{name}.xml"), serde_json::json!(STANDARD.encode(format!("<MetaDataObject><InformationRegister><Properties><Name>{name}</Name><WriteMode>Independent</WriteMode></Properties><ChildObjects><Resource><Properties><Name>Value</Name></Properties></Resource></ChildObjects></InformationRegister></MetaDataObject>"))));
        }
        source.insert("Configuration.xml".into(), serde_json::json!(STANDARD.encode(format!("<MetaDataObject><Configuration><Properties><Name>Agenda</Name></Properties><ChildObjects><CommonModule>Root</CommonModule>{objects}</ChildObjects></Configuration></MetaDataObject>"))));
        source.insert("CommonModules/Root.xml".into(), serde_json::json!(STANDARD.encode(format!("<MetaDataObject><CommonModule><Properties><Name>Root</Name><Comment>{refs}</Comment></Properties></CommonModule></MetaDataObject>"))));
        let row = serde_json::json!({"source": source});
        let temp = tempfile::tempdir().unwrap();
        let src = tests::source(&row, &temp.path().join("source"), false);
        let recipe = prepare(
            &src,
            None,
            &BTreeSet::from(["CommonModule.Root".into()]),
            &BTreeSet::new(),
            &Cancellation::default(),
        )
        .unwrap();
        assert!(
            recipe.composition.complete(),
            "{:?}",
            recipe.composition.diagnostics
        );
        assert_eq!(recipe.composition.choices.len(), count);
        assert!(
            recipe.projection_visits <= count * 5,
            "{count} independent choices caused {} visits",
            recipe.projection_visits
        );
    }
}
#[test]
fn registrar_proof_does_not_retain_unrelated_raw_documents() {
    let mut row = product_rows()["cases"][4].clone();
    edit_source(&mut row, "Configuration.xml", |s| {
        s.replace(
            "</ChildObjects>",
            "<Document>Unrelated</Document></ChildObjects>",
        )
    });
    row["source"]["Documents/Unrelated.xml"] = serde_json::json!(STANDARD.encode(format!("<MetaDataObject><Document><Properties><Name>Unrelated</Name><Comment>{}</Comment><RegisterRecords/></Properties></Document></MetaDataObject>", "x".repeat(65536))));
    let temp = tempfile::tempdir().unwrap();
    let src = tests::source(&row, &temp.path().join("source"), false);
    let cancel = Cancellation::default();
    let mut session = Session::new(
        &src,
        None,
        &BTreeSet::from(["CommonModule.Root".into()]),
        &BTreeSet::new(),
        &cancel,
    )
    .unwrap();
    assert!(
        session
            .registrars("InformationRegister.Info")
            .unwrap()
            .is_empty()
    );
    assert!(
        session
            .document_proof
            .as_ref()
            .unwrap()
            .contains_key("Unrelated.xml")
    );
    assert!(!session.text_cache.contains_key("Documents/Unrelated.xml"));
    assert!(session.hashes.contains_key("Documents/Unrelated.xml"));
    assert!(session.registrar_rows[0].1.is_empty());
}
#[test]
fn direct_preview_cannot_accept_required_or_unrelated_exclusions() {
    let row = product_rows()["cases"][2].clone();
    let temp = tempfile::tempdir().unwrap();
    let src = tests::source(&row, &temp.path().join("source"), false);
    let selected = serde_json::from_value(row["selected"].clone()).unwrap();
    for key in ["Catalog.Target", "Catalog.Unknown"] {
        let result = RustEngine.preview(
            &src,
            None,
            &selected,
            &BTreeSet::from([key.into()]),
            &Cancellation::default(),
        );
        assert!(result.is_err(), "invalid exclusion accepted: {key}");
    }
}
#[test]
fn successive_previews_preserve_cumulative_optional_exclusions() {
    let mut files = BTreeMap::from([
        ("Configuration.xml".to_owned(), "<MetaDataObject><Configuration><Properties><Name>Exclusions</Name></Properties><ChildObjects><CommonModule>Root</CommonModule><InformationRegister>A</InformationRegister><InformationRegister>B</InformationRegister></ChildObjects></Configuration></MetaDataObject>".to_owned()),
        ("CommonModules/Root.xml".into(), "<MetaDataObject><CommonModule><Properties><Name>Root</Name></Properties></CommonModule></MetaDataObject>".into()),
        ("CommonModules/Root/Ext/Module.bsl".into(), "// VALUE(InformationRegister.A.EmptyRef)\n// VALUE(InformationRegister.B.EmptyRef)".into()),
    ]);
    for name in ["A", "B"] {
        files.insert(format!("InformationRegisters/{name}.xml"), format!("<MetaDataObject><InformationRegister><Properties><Name>{name}</Name></Properties><ChildObjects/></InformationRegister></MetaDataObject>"));
    }
    let files: BTreeMap<_, _> = files
        .into_iter()
        .map(|(p, s)| (p, STANDARD.encode(s)))
        .collect();
    let temp = tempfile::tempdir().unwrap();
    let src = tests::source(
        &serde_json::json!({"source":files}),
        &temp.path().join("source"),
        false,
    );
    let cancel = Cancellation::default();
    let selected = BTreeSet::from(["CommonModule.Root".into()]);
    let mut exclusions = BTreeSet::from(["InformationRegister.A".into()]);
    let first = RustEngine
        .preview(&src, None, &selected, &exclusions, &cancel)
        .unwrap();
    assert!(!first.composition().complete());
    assert!(
        !first
            .composition()
            .stubs
            .contains_key("InformationRegister.A")
    );
    exclusions.insert("InformationRegister.B".into());
    first.validate_exclusions(&exclusions).unwrap();
    for _ in 0..2 {
        let next = RustEngine
            .preview(&src, None, &selected, &exclusions, &cancel)
            .unwrap();
        assert!(next.composition().complete());
        assert!(next.composition().stubs.is_empty());
        assert!(next.revised(&src, &exclusions, &cancel).unwrap().is_none());
    }
}
fn edit_source(row: &mut serde_json::Value, path: &str, edit: impl FnOnce(String) -> String) {
    let text = String::from_utf8(
        STANDARD
            .decode(row["source"][path].as_str().unwrap())
            .unwrap(),
    )
    .unwrap();
    row["source"][path] = serde_json::json!(STANDARD.encode(edit(text)));
}
#[test]
fn declared_forms_and_templates_are_valid_explicit_members() {
    for (tag, folder, name) in [
        ("Form", "Forms", "Screen"),
        ("Template", "Templates", "Schema"),
    ] {
        let mut row = product_rows()["cases"][2].clone();
        edit_source(&mut row, "CommonModules/Root.xml", |s| {
            s.replace("Attribute.Missing", &format!("{tag}.{name}"))
        });
        edit_source(&mut row, "Catalogs/Target.xml", |s| {
            s.replace(
                "</ChildObjects>",
                &format!("<{tag}>{name}</{tag}></ChildObjects>"),
            )
        });
        row["source"][format!("Catalogs/Target/{folder}/{name}.xml")] = serde_json::json!(STANDARD.encode(format!("<MetaDataObject><{tag}><Properties><Name>{name}</Name></Properties></{tag}></MetaDataObject>")));
        let temp = tempfile::tempdir().unwrap();
        let src = tests::source(&row, &temp.path().join("source"), false);
        let selected = serde_json::from_value(row["selected"].clone()).unwrap();
        let plan = RustEngine
            .preview(
                &src,
                None,
                &selected,
                &BTreeSet::new(),
                &Cancellation::default(),
            )
            .unwrap();
        assert!(
            plan.composition().complete(),
            "{tag}: {:?}",
            plan.composition().diagnostics
        );
    }
}
#[test]
fn required_untyped_metadata_properties_report_missing_targets() {
    for (kind, folder, property, value) in [
        (
            "BusinessProcess",
            "BusinessProcesses",
            "Task",
            "Task.Missing",
        ),
        (
            "AccountingRegister",
            "AccountingRegisters",
            "ChartOfAccounts",
            "ChartOfAccounts.Missing",
        ),
    ] {
        let mut row = product_rows()["cases"][2].clone();
        edit_source(&mut row, "CommonModules/Root.xml", |s| {
            s.replace("Catalog.Target.Attribute.Missing", &format!("{kind}.Probe"))
        });
        edit_source(&mut row, "Configuration.xml", |s| {
            s.replace(
                "</ChildObjects>",
                &format!("<{kind}>Probe</{kind}></ChildObjects>"),
            )
        });
        row["source"][format!("{folder}/Probe.xml")] = serde_json::json!(STANDARD.encode(format!("<MetaDataObject><{kind}><Properties><Name>Probe</Name><{property}>{value}</{property}></Properties><ChildObjects/></{kind}></MetaDataObject>")));
        let temp = tempfile::tempdir().unwrap();
        let src = tests::source(&row, &temp.path().join("source"), false);
        let selected = serde_json::from_value(row["selected"].clone()).unwrap();
        let plan = RustEngine
            .preview(
                &src,
                None,
                &selected,
                &BTreeSet::new(),
                &Cancellation::default(),
            )
            .unwrap();
        assert!(
            plan.composition()
                .diagnostics
                .iter()
                .any(|d| d.kind == DiagnosticKind::MissingObject && d.target == value),
            "{property}: {:?}",
            plan.composition().diagnostics
        );
    }
}
#[test]
fn excluding_an_optional_incomplete_stub_repairs_the_frozen_plan() {
    let mut row = product_rows()["cases"][4].clone();
    edit_source(&mut row, "CommonModules/Root.xml", |s| {
        s.replace(
            "<xr:Item xsi:type=\"xr:MDObjectRef\">InformationRegister.Info.Dimension.Key</xr:Item>",
            "",
        )
    });
    edit_source(&mut row, "InformationRegisters/Info.xml", |s| {
        let start = s.find("<ChildObjects>").unwrap();
        let end = s.find("</ChildObjects>").unwrap() + "</ChildObjects>".len();
        format!("{}<ChildObjects/>{}", &s[..start], &s[end..])
    });
    row["source"]["CommonModules/Root/Ext/Module.bsl"] =
        serde_json::json!(STANDARD.encode("// VALUE(InformationRegister.Info.EmptyRef)"));
    let temp = tempfile::tempdir().unwrap();
    let src = tests::source(&row, &temp.path().join("source"), false);
    let selected = serde_json::from_value(row["selected"].clone()).unwrap();
    let cancel = Cancellation::default();
    let plan = RustEngine
        .preview(&src, None, &selected, &BTreeSet::new(), &cancel)
        .unwrap();
    assert!(!plan.composition().complete());
    let excluded = BTreeSet::from(["InformationRegister.Info".into()]);
    plan.composition().validate_exclusions(&excluded).unwrap();
    let repaired = plan
        .revised(&src, &excluded, &cancel)
        .unwrap()
        .expect("new exclusions");
    assert!(repaired.composition().complete());
    assert!(
        !repaired
            .composition()
            .stubs
            .contains_key("InformationRegister.Info")
    );
    let frozen = RustEngine
        .preview(&src, None, &selected, &excluded, &cancel)
        .unwrap();
    assert!(frozen.composition().complete());
    // No additional exclusions must keep the already excluded object removed.
    let stage = temp.path().join("stage");
    frozen
        .write(
            &src,
            &BTreeSet::new(),
            &stage,
            &temp.path().join("merge.xml"),
            &cancel,
        )
        .unwrap();
    assert!(!stage.join("InformationRegisters/Info.xml").exists());
}
#[test]
fn a_supported_missing_object_is_incomplete_even_when_an_orphan_descriptor_exists() {
    let rows = product_rows();
    let mut row = rows["cases"][1].clone();
    edit_source(&mut row, "CommonModules/Root.xml", |s| {
        s.replace("Catalog.Missing", "Catalog.Orphan")
    });
    row["source"]["Catalogs/Orphan.xml"] = row["source"]["Catalogs/Known.xml"].clone();
    let temp = tempfile::tempdir().unwrap();
    let src = tests::source(&row, &temp.path().join("source"), false);
    let selected = serde_json::from_value(row["selected"].clone()).unwrap();
    let plan = RustEngine
        .preview(
            &src,
            None,
            &selected,
            &BTreeSet::new(),
            &Cancellation::default(),
        )
        .unwrap();
    assert!(!plan.composition().complete());
    assert!(plan.composition().stubs.contains_key("Catalog.Known"));
    assert!(!plan.composition().stubs.contains_key("Catalog.Orphan"));
    assert!(
        plan.composition()
            .diagnostics
            .iter()
            .any(|d| d.target == "Catalog.Orphan")
    );
}
#[test]
fn confirmed_required_form_and_predefined_files_cannot_silently_disappear() {
    for (reference, path) in [
        ("Form.Screen", "Catalogs/Target/Forms/Screen.xml"),
        ("Value", "Catalogs/Target/Ext/Predefined.xml"),
    ] {
        let mut row = product_rows()["cases"][2].clone();
        edit_source(&mut row, "CommonModules/Root.xml", |s| {
            s.replace("Attribute.Missing", reference)
        });
        edit_source(&mut row, "Catalogs/Target.xml", |s| {
            s.replace("</ChildObjects>", "<Form>Screen</Form></ChildObjects>")
        });
        let temp = tempfile::tempdir().unwrap();
        let src = tests::source(&row, &temp.path().join("source"), false);
        let selected = serde_json::from_value(row["selected"].clone()).unwrap();
        let cancel = Cancellation::default();
        let plan = RustEngine
            .preview(&src, None, &selected, &BTreeSet::new(), &cancel)
            .unwrap();
        assert!(
            plan.composition()
                .diagnostics
                .iter()
                .any(|d| d.kind == DiagnosticKind::MissingFile && d.target == path),
            "{reference}: {:?}",
            plan.composition().diagnostics
        );
        let stage = temp.path().join("stage");
        assert!(
            plan.write(
                &src,
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
fn structural_minimum_waits_until_other_stub_facets_have_closed() {
    let mut row = product_rows()["cases"][4].clone();
    edit_source(&mut row, "CommonModules/Root.xml", |s| {
        s.replace(
            "InformationRegister.Info.Dimension.Key",
            "InformationRegister.Info",
        )
        .replace(
            "</Content>",
            "<xr:Item xsi:type=\"xr:MDObjectRef\">Report.Delay</xr:Item></Content>",
        )
    });
    edit_source(&mut row, "Configuration.xml", |s| {
        s.replace("</ChildObjects>", "<Report>Delay</Report></ChildObjects>")
    });
    row["source"]["Reports/Delay.xml"]=serde_json::json!(STANDARD.encode("<MetaDataObject xmlns=\"http://v8.1c.ru/8.3/MDClasses\"><Report uuid=\"00000000-0000-4000-8000-000000000004\"><Properties><Name>Delay</Name><Comment>InformationRegister.Info.Dimension.Key</Comment></Properties><ChildObjects/></Report></MetaDataObject>"));
    let temp = tempfile::tempdir().unwrap();
    let src = tests::source(&row, &temp.path().join("source"), false);
    let cancel = Cancellation::default();
    let selected = serde_json::from_value(row["selected"].clone()).unwrap();
    let plan = RustEngine
        .preview(&src, None, &selected, &BTreeSet::new(), &cancel)
        .unwrap();
    assert!(
        plan.composition().complete(),
        "{:?}",
        plan.composition().diagnostics
    );
    let stage = temp.path().join("stage");
    plan.write(
        &src,
        &BTreeSet::new(),
        &stage,
        &temp.path().join("merge.xml"),
        &cancel,
    )
    .unwrap();
    let info = fs::read_to_string(stage.join("InformationRegisters/Info.xml")).unwrap();
    assert!(info.contains("<Name>Key</Name>"));
    assert!(!info.contains("<Name>Amount</Name>"));
    assert!(
        !plan
            .composition()
            .choices
            .iter()
            .any(|c| c.owner == "InformationRegister.Info" && c.rule == "minimum_field")
    );
}
#[test]
fn form_recorder_paths_keep_members_of_a_matching_document_included_later() {
    let old = tests::rows();
    let mut row = old["cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["name"] == "register-recorder-and-common-attribute")
        .unwrap()
        .clone();
    row["selected"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::json!("CommonModule.Late"));
    edit_source(&mut row, "Configuration.xml", |s| {
        s.replace(
            "</ChildObjects>",
            "<CommonModule>Late</CommonModule></ChildObjects>",
        )
    });
    let field = "<Attribute uuid=\"00000000-0000-4000-8000-000000000005\"><Properties><Name>Склад</Name><Type><Type>cfg:CatalogRef.Склады</Type></Type></Properties></Attribute>";
    edit_source(&mut row, "Documents/Альфа.xml", |s| {
        if s.contains("</ChildObjects>") {
            s.replace("</ChildObjects>", &format!("{field}</ChildObjects>"))
        } else {
            assert!(s.contains("<ChildObjects/>"));
            s.replace(
                "<ChildObjects/>",
                &format!("<ChildObjects>{field}</ChildObjects>"),
            )
        }
    });
    row["source"]["CommonModules/Late.xml"]=serde_json::json!(STANDARD.encode("<MetaDataObject xmlns=\"http://v8.1c.ru/8.3/MDClasses\" xmlns:xr=\"http://v8.1c.ru/8.3/xcf/readable\" xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\"><CommonModule uuid=\"00000000-0000-4000-8000-000000000006\"><Properties><Name>Late</Name><Content><xr:Item xsi:type=\"xr:MDObjectRef\">Document.Ясный</xr:Item></Content></Properties><ChildObjects/></CommonModule></MetaDataObject>"));
    let temp = tempfile::tempdir().unwrap();
    let src = tests::source(&row, &temp.path().join("source"), false);
    let selected = serde_json::from_value(row["selected"].clone()).unwrap();
    let cancel = Cancellation::default();
    let plan = RustEngine
        .preview(&src, None, &selected, &BTreeSet::new(), &cancel)
        .unwrap();
    assert!(
        plan.composition().complete(),
        "{:?}",
        plan.composition().diagnostics
    );
    assert!(
        plan.composition().members["Document.Ясный"].contains("*.Склад"),
        "late included matching recorder lost its required member: {:?}",
        plan.composition().members
    );
    let stage = temp.path().join("stage");
    plan.write(
        &src,
        &BTreeSet::new(),
        &stage,
        &temp.path().join("merge.xml"),
        &cancel,
    )
    .unwrap();
    assert!(
        fs::read_to_string(stage.join("Documents/Ясный.xml"))
            .unwrap()
            .contains("<Name>Склад</Name>")
    );
}

#[test]
fn independently_specified_product_cases_with_and_without_sqlite() {
    let rows = product_rows();
    for row in rows["cases"].as_array().unwrap() {
        for indexed in [false, true] {
            let label = format!("{} indexed={indexed}", row["name"].as_str().unwrap());
            let temp = tempfile::tempdir().unwrap();
            let src = tests::source(row, &temp.path().join("source"), false);
            let cancel = Cancellation::default();
            let store = if indexed {
                let built = indexing::update(&src, &temp.path().join("index"), &cancel, &|_, _| {})
                    .unwrap();
                Some(IndexReader::open(&built.path, Proof::Local(src.identity())).unwrap())
            } else {
                None
            };
            let selected = serde_json::from_value(row["selected"].clone()).unwrap();
            let plan = RustEngine
                .preview(
                    &src,
                    store.as_ref().map(|i| i as &dyn FactStore),
                    &selected,
                    &BTreeSet::new(),
                    &cancel,
                )
                .unwrap();
            let expected = &row["expected"];
            assert_eq!(
                plan.composition().complete(),
                expected["complete"].as_bool().unwrap(),
                "{label}: diagnostics {:?}",
                plan.composition().diagnostics
            );
            assert_eq!(
                serde_json::to_value(plan.composition().stubs.keys().collect::<Vec<_>>()).unwrap(),
                expected["stubs"],
                "{label}: composition"
            );
            if let Some(target) = expected["diagnostic_target"].as_str() {
                assert!(
                    plan.composition()
                        .diagnostics
                        .iter()
                        .any(|d| d.target.contains(target)),
                    "{label}: diagnostic {target}"
                );
            }
            if let Some(owner) = expected["required_member_owner"].as_str() {
                assert!(
                    plan.composition().members[owner]
                        .contains(expected["required_member"].as_str().unwrap()),
                    "{label}: long path"
                );
            }
            if let Some(owner) = expected["choice_owner"].as_str() {
                assert!(
                    plan.composition().choices.iter().any(|c| c.owner == owner),
                    "{label}: explicit choice"
                );
            }
            let stage = temp.path().join("stage");
            let result = plan.write(
                &src,
                &BTreeSet::new(),
                &stage,
                &temp.path().join("merge.xml"),
                &cancel,
            );
            if !plan.composition().complete() {
                assert!(result.is_err(), "{label}: incomplete write");
                assert!(!stage.exists(), "{label}: staging created");
                continue;
            }
            let package = result.unwrap_or_else(|e| panic!("{label}: {e}"));
            package
                .readback
                .validate(&package.composition, src.catalog(), &package.files)
                .unwrap();
            if let Some(path) = expected["descriptor"].as_str() {
                let text = fs::read_to_string(stage.join(path)).unwrap();
                if let Some(field) = expected["kept_field"].as_str() {
                    assert!(
                        text.contains(&format!("<Name>{field}</Name>")),
                        "{label}: retained field"
                    );
                }
                if let Some(field) = expected["not_kept_field"].as_str() {
                    assert!(
                        !text.contains(&format!("<Name>{field}</Name>")),
                        "{label}: unnecessary minimum"
                    );
                }
                if let Some(link) = expected["kept_link"].as_str() {
                    assert!(text.contains(link), "{label}: required registrar link");
                }
                if let Some(link) = expected["not_kept_link"].as_str() {
                    assert!(!text.contains(link), "{label}: unrelated registrar link");
                }
            }
        }
    }
}
