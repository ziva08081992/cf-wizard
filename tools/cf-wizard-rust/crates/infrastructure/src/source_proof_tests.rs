use crate::engine::RustEngine;
use crate::engine::tests::{rows, source};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use cf_application::{Cancellation, DependencyEngine};
use std::{collections::BTreeSet, fs};

#[test]
fn omitted_module_content_or_absence_changes_invalidate_the_preview() {
    for existed in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        let mut row = rows()["cases"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["name"] == "required-values")
            .unwrap()
            .clone();
        let module = "Catalogs/Тест/Ext/ObjectModule.bsl";
        if existed {
            row["source"][module] =
                serde_json::json!(STANDARD.encode(b"// original omitted module"));
        }
        let src = source(&row, &temp.path().join("source"), false);
        let selected = serde_json::from_value(row["selected"].clone()).unwrap();
        let preview = RustEngine
            .preview(
                &src,
                None,
                &selected,
                &BTreeSet::new(),
                &Cancellation::default(),
            )
            .unwrap();
        assert!(preview.composition().stubs.contains_key("Catalog.Тест"));
        let relative = src.local.root.join(module);
        fs::create_dir_all(relative.parent().unwrap()).unwrap();
        fs::write(relative, b"// changed after preview").unwrap();
        let stage = temp.path().join("stage");
        let result = preview.write(
            &src,
            &BTreeSet::new(),
            &stage,
            &temp.path().join("settings.xml"),
            &Cancellation::default(),
        );
        assert!(
            result.is_err(),
            "existence={existed}: live source must not silently change merge protection"
        );
        assert!(!stage.exists(), "stale proof fails before materialization");
    }
}
