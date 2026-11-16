use super::*;
use crate::output::Output;
use cf_application::PackageOutput;
use std::path::PathBuf;

const REL: &str = "Catalogs/Тест.xml";

#[test]
fn retained_stub_picture_loss_saves_exact_xml_pair() {
    let picture = r#"<Picture xmlns:u="http://v8.1c.ru/8.1/data/ui" xmlns:i="http://www.w3.org/2001/XMLSchema-instance" i:type="u:Picture" ref="u:Camera"/>"#;
    let before = String::from_utf8(stub_xml(
        "11111111-1111-4111-8111-111111111111",
        "Поле",
        false,
    ))
    .unwrap()
    .replacen("</Properties>", &format!("{picture}</Properties>"), 1);
    for changed in [String::new(), picture.replace(" ref=\"u:Camera\"", "")] {
        let temp = tempfile::tempdir().unwrap();
        let log = OperationLog::create(&temp.path().join("logs"), "build").unwrap();
        let stage = temp.path().join("stage");
        let back = temp.path().join("back");
        let after = before.replace(picture, &changed);
        files(&stage, &back, REL, before.as_bytes(), after.as_bytes());
        let error = super::verify_readback(
            &stage,
            &back,
            &inventory(REL, before.as_bytes()),
            &stub_contract(),
            &Cancellation::default(),
            &log,
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("Picture"), "{error}");
        let pair = pairs(&log);
        assert_eq!(pair.len(), 1);
        assert_eq!(
            fs::read(pair[0].join("before.xml")).unwrap(),
            before.as_bytes()
        );
        assert_eq!(
            fs::read(pair[0].join("after.xml")).unwrap(),
            after.as_bytes()
        );
    }
}

#[test]
fn missing_or_changed_binary_saves_available_bytes_and_rejects_readback() {
    let relative = "CommonPictures/Probe/Ext/Picture/100.png";
    for missing in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        let log = OperationLog::create(&temp.path().join("logs"), "build").unwrap();
        let stage = temp.path().join("stage");
        let back = temp.path().join("back");
        let before = b"original binary";
        files(&stage, &back, relative, before, b"changed binary");
        if missing {
            fs::remove_file(back.join(relative)).unwrap();
        }
        let error = verify_readback(
            &stage,
            &back,
            &inventory(relative, before),
            &Cancellation::default(),
            &log,
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains(relative));
        let saved = pairs(&log);
        assert_eq!(saved.len(), 1);
        assert_eq!(fs::read(saved[0].join("before.bin")).unwrap(), before);
        if missing {
            assert!(!saved[0].join("after.bin").exists());
            assert!(saved[0].join("after-missing.txt").exists());
        } else {
            assert_eq!(
                fs::read(saved[0].join("after.bin")).unwrap(),
                b"changed binary"
            );
        }
    }
}

#[test]
fn resource_descriptor_keeps_full_comparison_and_preflight_role_guard() {
    let temp = tempfile::tempdir().unwrap();
    let log = OperationLog::create(&temp.path().join("logs"), "build").unwrap();
    let stage = temp.path().join("stage");
    let back = temp.path().join("back");
    let relative = "CommonPictures/Probe.xml";
    let before=b"<MetaDataObject xmlns=\"http://v8.1c.ru/8.3/MDClasses\"><CommonPicture uuid=\"11111111-1111-4111-8111-111111111111\"><Properties><Name>Probe</Name><AvailabilityForChoice>false</AvailabilityForChoice></Properties></CommonPicture></MetaDataObject>";
    let after = String::from_utf8_lossy(before).replace("false", "true");
    files(&stage, &back, relative, before, after.as_bytes());
    let mut contract = ReadbackContract {
        full_dependencies: BTreeMap::from([(
            relative.into(),
            cf_domain::ObjectKey::parse("CommonPicture.Probe").unwrap(),
        )]),
        ..Default::default()
    };
    let entries = inventory(relative, before);
    validate_readback_contract(&stage, &entries, &contract, &Cancellation::default()).unwrap();
    assert!(
        super::verify_readback(
            &stage,
            &back,
            &entries,
            &contract,
            &Cancellation::default(),
            &log
        )
        .is_err()
    );
    assert_eq!(
        fs::read(pairs(&log)[0].join("after.xml")).unwrap(),
        after.as_bytes()
    );
    contract.stub_descriptors = std::mem::take(&mut contract.full_dependencies);
    assert!(
        validate_readback_contract(&stage, &entries, &contract, &Cancellation::default()).is_err()
    );
}

fn stub_xml(id: &str, member: &str, materialized: bool) -> Vec<u8> {
    let standard = if materialized {
        "<xr:StandardAttribute name=\"Ref\"><xr:FillChecking>DontCheck</xr:FillChecking></xr:StandardAttribute>"
    } else {
        ""
    };
    format!("<MetaDataObject xmlns=\"http://v8.1c.ru/8.3/MDClasses\" xmlns:xr=\"http://v8.1c.ru/8.3/xcf/readable\"><Catalog uuid=\"{id}\"><Properties><Name>Тест</Name><StandardAttributes>{standard}</StandardAttributes></Properties><ChildObjects>{member}</ChildObjects></Catalog></MetaDataObject>").into_bytes()
}
fn stub_contract() -> ReadbackContract {
    ReadbackContract {
        full_dependencies: BTreeMap::new(),
        stub_descriptors: BTreeMap::from([(
            REL.into(),
            cf_domain::ObjectKey::parse("Catalog.Тест").unwrap(),
        )]),
    }
}

#[test]
fn only_the_stub_descriptor_accepts_materialized_defaults() {
    let temp = tempfile::tempdir().unwrap();
    let log = OperationLog::create(&temp.path().join("logs"), "build").unwrap();
    let stage = temp.path().join("stage");
    let back = temp.path().join("back");
    let before = stub_xml("11111111-1111-4111-8111-111111111111", "", false);
    let after = stub_xml("11111111-1111-4111-8111-111111111111", "", true);
    files(&stage, &back, REL, &before, &after);
    let entries = inventory(REL, &before);
    let cancel = Cancellation::default();
    validate_readback_contract(&stage, &entries, &stub_contract(), &cancel).unwrap();
    super::verify_readback(&stage, &back, &entries, &stub_contract(), &cancel, &log).unwrap();
    // The same bytes are a mismatch when this is a fully selected object.
    assert!(verify_readback(&stage, &back, &entries, &cancel, &log).is_err());
}

#[test]
fn a_stub_identity_or_required_member_failure_still_saves_the_exact_xml_pair() {
    let member = "<Attribute uuid=\"33333333-3333-4333-8333-333333333333\"><Properties><Name>Needed</Name><Type>String</Type></Properties></Attribute>";
    let before = stub_xml("11111111-1111-4111-8111-111111111111", member, false);
    for after in [
        stub_xml("22222222-2222-4222-8222-222222222222", member, true),
        stub_xml("11111111-1111-4111-8111-111111111111", "", true),
    ] {
        let temp = tempfile::tempdir().unwrap();
        let log = OperationLog::create(&temp.path().join("logs"), "build").unwrap();
        let stage = temp.path().join("stage");
        let back = temp.path().join("back");
        files(&stage, &back, REL, &before, &after);
        let error = super::verify_readback(
            &stage,
            &back,
            &inventory(REL, &before),
            &stub_contract(),
            &Cancellation::default(),
            &log,
        )
        .unwrap_err()
        .to_string();
        let saved = pairs(&log);
        assert_eq!(saved.len(), 1);
        assert!(error.contains(&normal_path_display(&saved[0])));
        assert_eq!(fs::read(saved[0].join("before.xml")).unwrap(), before);
        assert_eq!(fs::read(saved[0].join("after.xml")).unwrap(), after);
    }
}

#[test]
fn stub_related_xml_and_bsl_keep_strict_comparison() {
    for rel in [
        "Catalogs/Тест/Ext/Predefined.xml",
        "Catalogs/Тест/Forms/Форма.xml",
        "Catalogs/Тест/Ext/ObjectModule.bsl",
    ] {
        let temp = tempfile::tempdir().unwrap();
        let log = OperationLog::create(&temp.path().join("logs"), "build").unwrap();
        let stage = temp.path().join("stage");
        let back = temp.path().join("back");
        let descriptor = stub_xml("11111111-1111-4111-8111-111111111111", "", false);
        files(&stage, &back, REL, &descriptor, &descriptor);
        let (before, after): (&[u8], &[u8]) = if rel.ends_with(".bsl") {
            (
                b"Procedure A()\nEndProcedure",
                b"Procedure B()\nEndProcedure",
            )
        } else {
            (b"<Root><Required>yes</Required></Root>", b"<Root/>")
        };
        files(&stage, &back, rel, before, after);
        let mut entries = inventory(REL, &descriptor);
        entries.extend(inventory(rel, before));
        let error = super::verify_readback(
            &stage,
            &back,
            &entries,
            &stub_contract(),
            &Cancellation::default(),
            &log,
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains(rel), "{error}");
    }
}

#[test]
fn preflight_rejects_invalid_stub_roles_and_malformed_descriptors() {
    let temp = tempfile::tempdir().unwrap();
    let stage = temp.path().join("stage");
    let back = temp.path().join("back");
    let descriptor = stub_xml("11111111-1111-4111-8111-111111111111", "", false);
    files(&stage, &back, REL, &descriptor, &descriptor);
    let entries = inventory(REL, &descriptor);
    for (path, key) in [
        ("../Тест.xml", "Catalog.Тест"),
        ("Catalogs/Missing.xml", "Catalog.Тест"),
        (REL, "Document.Тест"),
        (REL, "Catalog.ДругоеИмя"),
    ] {
        let contract = ReadbackContract {
            full_dependencies: BTreeMap::new(),
            stub_descriptors: BTreeMap::from([(
                path.into(),
                cf_domain::ObjectKey::parse(key).unwrap(),
            )]),
        };
        assert!(
            validate_readback_contract(&stage, &entries, &contract, &Cancellation::default())
                .is_err(),
            "{path} / {key}"
        );
    }
    fs::write(stage.join(REL), b"<Broken").unwrap();
    assert!(
        validate_readback_contract(&stage, &entries, &stub_contract(), &Cancellation::default())
            .is_err()
    );
}

fn verify_readback(
    stage: &Path,
    back: &Path,
    inventory: &BTreeMap<String, String>,
    cancel: &Cancellation,
    log: &OperationLog,
) -> Result<()> {
    super::verify_readback(
        stage,
        back,
        inventory,
        &ReadbackContract::default(),
        cancel,
        log,
    )
}

fn files(stage: &Path, back: &Path, rel: &str, before: &[u8], after: &[u8]) {
    for (root, bytes) in [(stage, before), (back, after)] {
        let path = root.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
    }
}
fn inventory(rel: &str, bytes: &[u8]) -> BTreeMap<String, String> {
    BTreeMap::from([(rel.into(), sha256(bytes))])
}
fn pairs(log: &OperationLog) -> Vec<PathBuf> {
    fs::read_dir(log.path.parent().unwrap().join("Ошибки"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.is_dir())
        .collect()
}

#[test]
fn xml_mismatch_keeps_exact_pair_and_journal_path_after_workspace_cleanup() {
    let temp = tempfile::tempdir().unwrap();
    let log = Arc::new(OperationLog::create(&temp.path().join("logs"), "build").unwrap());
    let output = Output { log: log.clone() };
    let workspace = output
        .begin(&temp.path().join("result"), "test", None)
        .unwrap();
    let stage = workspace.staging();
    let work = workspace.native_work();
    let back = work.join("readback");
    let before = "\u{feff}<?xml version=\"1.0\"?>\r\n<Root><Value>до</Value></Root>\r\n".as_bytes();
    let after = "<Root><Value>после</Value></Root>\n".as_bytes();
    files(&stage, &back, REL, before, after);
    let error = verify_readback(
        &stage,
        &back,
        &inventory(REL, before),
        &Cancellation::default(),
        &log,
    )
    .unwrap_err()
    .to_string();
    assert!(error.contains(&format!("Обратная выгрузка отличается: {REL}")));
    let saved = pairs(&log);
    assert_eq!(saved.len(), 1);
    let saved = &saved[0];
    assert!(error.contains(&normal_path_display(saved)));
    assert_eq!(fs::read(saved.join("before.xml")).unwrap(), before);
    assert_eq!(fs::read(saved.join("after.xml")).unwrap(), after);
    assert_eq!(fs::read_dir(saved).unwrap().count(), 2);
    drop(workspace);
    assert!(!stage.exists());
    assert!(!work.exists());
    assert_eq!(fs::read(saved.join("before.xml")).unwrap(), before);
    assert_eq!(fs::read(saved.join("after.xml")).unwrap(), after);
    let log_path = log.path.clone();
    drop(output);
    drop(log);
    let mut rows = String::new();
    flate2::read::MultiGzDecoder::new(File::open(&log_path).unwrap())
        .read_to_string(&mut rows)
        .unwrap();
    let event = rows
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .find(|row| row["event"] == "readback-mismatch")
        .unwrap();
    assert_eq!(event["data"]["relativePath"], REL);
    assert_eq!(
        Path::new(event["data"]["directory"].as_str().unwrap()),
        saved
    );
    assert_eq!(event["data"]["beforeSha256"], sha256(before));
    assert_eq!(event["data"]["afterSha256"], sha256(after));
}

#[test]
fn equivalent_xml_and_bsl_do_not_save_diagnostics_and_bsl_mismatch_is_unchanged() {
    let temp = tempfile::tempdir().unwrap();
    let log = OperationLog::create(&temp.path().join("logs"), "build").unwrap();
    let stage = temp.path().join("stage");
    let back = temp.path().join("back");
    let before = br#"<Root a="x" b="y"><Value>1</Value></Root>"#;
    let after = b"<Root b=\"y\" a=\"x\">\n<Value>1</Value>\n</Root>";
    files(&stage, &back, REL, before, after);
    let mut entries = inventory(REL, before);
    let module = "CommonModules/Test/Ext/Module.bsl";
    files(&stage, &back, module, b"same module", b"same module");
    entries.extend(inventory(module, b"same module"));
    verify_readback(&stage, &back, &entries, &Cancellation::default(), &log).unwrap();
    let errors = log.path.parent().unwrap().join("Ошибки");
    assert!(!errors.exists());
    fs::write(back.join(module), b"changed module").unwrap();
    let error = verify_readback(&stage, &back, &entries, &Cancellation::default(), &log)
        .unwrap_err()
        .to_string();
    assert_eq!(error, format!("Обратная выгрузка отличается: {module}"));
    assert!(!errors.exists());
}

#[test]
fn failed_diagnostic_destination_preserves_the_original_mismatch() {
    let temp = tempfile::tempdir().unwrap();
    let log = OperationLog::create(&temp.path().join("logs"), "build").unwrap();
    let stage = temp.path().join("stage");
    let back = temp.path().join("back");
    let before = b"<Root>before</Root>";
    files(&stage, &back, REL, before, b"<Root>after</Root>");
    let blocked = log.path.parent().unwrap().join("Ошибки");
    fs::write(&blocked, b"existing file").unwrap();
    let error = verify_readback(
        &stage,
        &back,
        &inventory(REL, before),
        &Cancellation::default(),
        &log,
    )
    .unwrap_err()
    .to_string();
    assert!(error.contains(&format!("Обратная выгрузка отличается: {REL}")));
    assert!(error.contains("Не удалось сохранить XML"));
    assert!(!error.contains("XML сохранены"));
    assert_eq!(fs::read(blocked).unwrap(), b"existing file");
}

#[test]
fn separate_operations_do_not_overwrite_a_saved_pair() {
    let temp = tempfile::tempdir().unwrap();
    let log1 = OperationLog::create(&temp.path().join("logs"), "build").unwrap();
    let log2 = OperationLog::create(&temp.path().join("logs"), "build").unwrap();
    let first = log1
        .save_readback_pair(b"first before", b"first after")
        .unwrap();
    let second = log2
        .save_readback_pair(b"second before", b"second after")
        .unwrap();
    assert_ne!(first, second);
    assert_eq!(fs::read(first.join("before.xml")).unwrap(), b"first before");
    assert_eq!(fs::read(first.join("after.xml")).unwrap(), b"first after");
    assert_eq!(
        fs::read(second.join("before.xml")).unwrap(),
        b"second before"
    );
    assert_eq!(fs::read(second.join("after.xml")).unwrap(), b"second after");
}

#[test]
fn journal_failure_still_preserves_the_pair_and_the_original_mismatch() {
    let temp = tempfile::tempdir().unwrap();
    let log = OperationLog::create(&temp.path().join("logs"), "build").unwrap();
    let stage = temp.path().join("stage");
    let back = temp.path().join("back");
    let before = b"<Root>before</Root>";
    let after = b"<Root>after</Root>";
    files(&stage, &back, REL, before, after);
    log.poison_writer_for_test();
    let error = verify_readback(
        &stage,
        &back,
        &inventory(REL, before),
        &Cancellation::default(),
        &log,
    )
    .unwrap_err()
    .to_string();
    let saved = pairs(&log);
    assert_eq!(saved.len(), 1);
    assert!(error.contains(&format!("Обратная выгрузка отличается: {REL}")));
    assert!(error.contains(&normal_path_display(&saved[0])));
    assert!(error.contains("Не удалось записать сведения в журнал"));
    assert_eq!(fs::read(saved[0].join("before.xml")).unwrap(), before);
    assert_eq!(fs::read(saved[0].join("after.xml")).unwrap(), after);
}

#[cfg(windows)]
#[test]
fn linked_error_folder_is_rejected_without_writing_to_the_target() {
    use std::os::windows::process::CommandExt;
    let temp = tempfile::tempdir().unwrap();
    let log = OperationLog::create(&temp.path().join("logs"), "build").unwrap();
    let target = temp.path().join("outside");
    fs::create_dir(&target).unwrap();
    let link = log.path.parent().unwrap().join("Ошибки");
    let script = format!(
        "$ErrorActionPreference='Stop'; New-Item -ItemType Junction -Path '{}' -Target '{}' | Out-Null",
        link.display().to_string().replace('\'', "''"),
        target.display().to_string().replace('\'', "''")
    );
    let result = Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .creation_flags(0x08000000)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(log.save_readback_pair(b"before", b"after").is_err());
    assert_eq!(fs::read_dir(&target).unwrap().count(), 0);
}
