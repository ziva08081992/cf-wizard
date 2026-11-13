use super::*;
#[test]
fn canonical_alias_publication_has_only_two_files_and_rejects_mutation() {
    let dir = tempfile::tempdir().unwrap();
    let log = Arc::new(OperationLog::create(&dir.path().join("logs"), "publication-test").unwrap());
    let output = Output { log };
    let destination = dir.path().join("destination");
    let workspace = output.begin(&destination, "1235", None).unwrap();
    let native = workspace.native_work();
    fs::create_dir_all(native.join("output")).unwrap();
    let cf = native.join("output/Test.cf");
    fs::write(&cf, b"checked CF").unwrap();
    let settings = workspace.settings();
    fs::write(&settings, b"<settings/>").unwrap();
    let artifact = NativeArtifact {
        path: fs::canonicalize(&cf).unwrap(),
        sha256: sha256(b"checked CF"),
    };
    let package = workspace
        .publish(
            &artifact,
            &settings,
            &sha256(b"<settings/>"),
            &Cancellation::default(),
        )
        .unwrap();
    let names = fs::read_dir(&package)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        names,
        std::collections::BTreeSet::from(["Test.cf".into(), "Test.merge-settings.xml".into()])
    );
    fs::write(&cf, b"modified CF").unwrap();
    assert!(
        workspace
            .publish(
                &artifact,
                &settings,
                &sha256(b"<settings/>"),
                &Cancellation::default()
            )
            .is_err()
    );
    assert_eq!(fs::read_dir(&destination).unwrap().count(), 1);
}
#[test]
fn result_cannot_be_the_source_or_a_descendant() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("source");
    fs::create_dir(&root).unwrap();
    let log = Arc::new(OperationLog::create(&dir.path().join("logs"), "scope-test").unwrap());
    let output = Output { log };
    assert!(output.begin(&root, "task", Some(&root)).is_err());
    let child = root.join("result");
    assert!(output.begin(&child, "task", Some(&root)).is_err());
    assert!(!child.exists());
    assert_eq!(fs::read_dir(root).unwrap().count(), 0);
}
