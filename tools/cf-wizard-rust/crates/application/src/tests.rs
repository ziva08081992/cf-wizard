use super::*;
use std::{path::Path, sync::atomic::AtomicUsize};
struct Source(Catalog);
impl MetadataSource for Source {
    fn identity(&self) -> &str {
        "test"
    }
    fn catalog(&self) -> &Catalog {
        &self.0
    }
    fn read(&self, _: &str, _: &Cancellation) -> Result<Vec<u8>> {
        Ok(vec![])
    }
    fn files_below(&self, _: &str, _: &Cancellation) -> Result<Vec<String>> {
        Ok(vec![])
    }
    fn exists(&self, _: &str, _: &Cancellation) -> Result<bool> {
        Ok(false)
    }
    fn list_directory(&self, _: &str, _: &Cancellation) -> Result<Vec<SourceEntry>> {
        Ok(vec![])
    }
    fn snapshot(&self, _: &Cancellation) -> Result<String> {
        Ok("test".into())
    }
}
struct Preview(Plan);
impl PreparedPlan for Preview {
    fn composition(&self) -> &Plan {
        &self.0
    }
    fn write(
        &self,
        _: &dyn MetadataSource,
        _: &BTreeSet<String>,
        _: &Path,
        settings: &Path,
        _: &Cancellation,
    ) -> Result<StagedPackage> {
        Ok(StagedPackage {
            composition: self.0.clone(),
            files: BTreeMap::new(),
            readback: ReadbackContract::default(),
            settings: settings.into(),
            settings_hash: "settings-proof".into(),
        })
    }
}
struct Workspace(Arc<AtomicUsize>);
impl PackageWorkspace for Workspace {
    fn staging(&self) -> PathBuf {
        "stage".into()
    }
    fn settings(&self) -> PathBuf {
        "settings.xml".into()
    }
    fn native_work(&self) -> PathBuf {
        "native".into()
    }
    fn publish(
        &self,
        cf: &NativeArtifact,
        _: &Path,
        proof: &str,
        _: &Cancellation,
    ) -> Result<PathBuf> {
        assert_eq!(cf.sha256, "cf-proof");
        assert_eq!(proof, "settings-proof");
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok("delivered".into())
    }
}
struct Output(Arc<AtomicUsize>);
impl PackageOutput for Output {
    fn begin(&self, _: &Path, _: &str, _: Option<&Path>) -> Result<Box<dyn PackageWorkspace>> {
        Ok(Box::new(Workspace(self.0.clone())))
    }
}
struct Native;
impl NativeBuilder for Native {
    fn build(&self, _: &NativeRequest, _: &Cancellation) -> Result<NativeArtifact> {
        Ok(NativeArtifact {
            path: "native/test.cf".into(),
            sha256: "cf-proof".into(),
        })
    }
}
struct Log;
struct NoWorkspace;
impl PackageOutput for NoWorkspace {
    fn begin(&self, _: &Path, _: &str, _: Option<&Path>) -> Result<Box<dyn PackageWorkspace>> {
        panic!("incomplete plan reached workspace creation")
    }
}
struct RevisedIncomplete {
    initial: Plan,
    revised: Plan,
}
impl PreparedPlan for RevisedIncomplete {
    fn composition(&self) -> &Plan {
        &self.initial
    }
    fn revised(
        &self,
        _: &dyn MetadataSource,
        _: &BTreeSet<String>,
        _: &Cancellation,
    ) -> Result<Option<Box<dyn PreparedPlan>>> {
        Ok(Some(Box::new(Preview(self.revised.clone()))))
    }
    fn write(
        &self,
        _: &dyn MetadataSource,
        _: &BTreeSet<String>,
        _: &Path,
        _: &Path,
        _: &Cancellation,
    ) -> Result<StagedPackage> {
        panic!("incomplete revised plan reached materialization")
    }
}
#[test]
fn incomplete_original_and_revised_plans_stop_before_creating_any_workspace() {
    let source = Source(Catalog {
        name: "Test".into(),
        uuid: String::new(),
        objects: BTreeMap::new(),
    });
    let mut bad = Plan {
        source_fingerprint: "test".into(),
        selected: BTreeSet::new(),
        stubs: BTreeMap::new(),
        members: BTreeMap::new(),
        source_hashes: BTreeMap::new(),
        warnings: vec![],
        diagnostics: vec![],
        choices: vec![],
        reasons: vec![],
        metrics: Default::default(),
        reader_profile: Default::default(),
    };
    let good = bad.clone();
    bad.diagnostics.push(cf_domain::Diagnostic {
        kind: cf_domain::DiagnosticKind::MissingObject,
        owner: "Catalog.Root".into(),
        source: "Catalogs/Root.xml".into(),
        target: "Catalog.Missing".into(),
    });
    let plans: Vec<Box<dyn PreparedPlan>> = vec![
        Box::new(Preview(bad.clone())),
        Box::new(RevisedIncomplete {
            initial: good.clone(),
            revised: bad.clone(),
        }),
    ];
    let options = BuildOptions {
        destination: "destination".into(),
        label: "test".into(),
        executable: "1cv8.exe".into(),
        mode: cf_domain::BuildMode::Designer,
        version: "8.3.27.2214".into(),
        stamp: "test".into(),
    };
    for plan in plans {
        assert!(
            build_package(
                plan.as_ref(),
                &source,
                &BTreeSet::new(),
                &options,
                &BuildServices {
                    output: &NoWorkspace,
                    builder: &Native,
                    log: &Log
                },
                &Cancellation::default()
            )
            .is_err()
        );
    }
    // A revised complete plan may repair an incomplete original. The final
    // guard still runs before any workspace is created.
    let repaired = RevisedIncomplete {
        initial: bad,
        revised: good,
    };
    let count = Arc::new(AtomicUsize::new(0));
    let path = build_package(
        &repaired,
        &source,
        &BTreeSet::new(),
        &options,
        &BuildServices {
            output: &Output(count.clone()),
            builder: &Native,
            log: &Log,
        },
        &Cancellation::default(),
    )
    .unwrap();
    assert_eq!(path, PathBuf::from("delivered"));
    assert_eq!(count.load(Ordering::SeqCst), 1);
}
impl Journal for Log {
    fn event(&self, operation: &str, _: &BTreeMap<String, String>) -> Result<()> {
        if operation == "package-published" {
            Err(cf_domain::Error::External("full log disk".into()))
        } else {
            Ok(())
        }
    }
}
#[test]
fn a_trailing_journal_error_does_not_hide_an_already_delivered_package() {
    let count = Arc::new(AtomicUsize::new(0));
    let source = Source(Catalog {
        name: "Test".into(),
        uuid: String::new(),
        objects: BTreeMap::new(),
    });
    let plan = Preview(Plan {
        source_fingerprint: "test".into(),
        selected: BTreeSet::new(),
        stubs: BTreeMap::new(),
        members: BTreeMap::new(),
        source_hashes: BTreeMap::new(),
        warnings: vec![],
        diagnostics: vec![],
        choices: vec![],
        reasons: vec![],
        metrics: Default::default(),
        reader_profile: Default::default(),
    });
    let options = BuildOptions {
        destination: "destination".into(),
        label: "label".into(),
        executable: "1cv8.exe".into(),
        mode: cf_domain::BuildMode::Auto,
        version: "8.3.27.2214".into(),
        stamp: "stamp".into(),
    };
    let result = build_package(
        &plan,
        &source,
        &BTreeSet::new(),
        &options,
        &BuildServices {
            output: &Output(count.clone()),
            builder: &Native,
            log: &Log,
        },
        &Cancellation::default(),
    )
    .unwrap();
    assert_eq!(result, PathBuf::from("delivered"));
    assert_eq!(count.load(Ordering::SeqCst), 1);
}

struct MaterializedPreview {
    original: Plan,
    actual: Plan,
    roles: ReadbackContract,
    files: BTreeMap<String, String>,
}
impl PreparedPlan for MaterializedPreview {
    fn composition(&self) -> &Plan {
        &self.original
    }
    fn write(
        &self,
        _: &dyn MetadataSource,
        _: &BTreeSet<String>,
        _: &Path,
        settings: &Path,
        _: &Cancellation,
    ) -> Result<StagedPackage> {
        Ok(StagedPackage {
            composition: self.actual.clone(),
            files: self.files.clone(),
            readback: self.roles.clone(),
            settings: settings.into(),
            settings_hash: "settings-proof".into(),
        })
    }
}
struct RecordingNative(std::sync::Mutex<Vec<NativeRequest>>);
impl NativeBuilder for RecordingNative {
    fn build(&self, request: &NativeRequest, cancel: &Cancellation) -> Result<NativeArtifact> {
        self.0.lock().unwrap().push(request.clone());
        Native.build(request, cancel)
    }
}
fn role_fixture() -> (Source, MaterializedPreview) {
    role_fixture_kind(false)
}
fn role_fixture_kind(resource: bool) -> (Source, MaterializedPreview) {
    let full = "Document.Selected";
    let stub = if resource {
        "CommonPicture.Dependency"
    } else {
        "Catalog.Dependency"
    };
    let path = if resource {
        "CommonPictures/Dependency.xml"
    } else {
        "Catalogs/Dependency.xml"
    };
    let mut roles = ReadbackContract::default();
    let role_map = if resource {
        &mut roles.full_dependencies
    } else {
        &mut roles.stub_descriptors
    };
    role_map.insert(path.into(), ObjectKey::parse(stub).unwrap());
    let catalog = Catalog {
        name: "Test".into(),
        uuid: String::new(),
        objects: [(full, "Documents/Selected.xml"), (stub, path)]
            .into_iter()
            .map(|(key, path)| {
                let key = ObjectKey::parse(key).unwrap();
                (
                    key.clone(),
                    cf_domain::MetadataObject {
                        key,
                        descriptor: path.into(),
                        uuid: String::new(),
                    },
                )
            })
            .collect(),
    };
    let actual = Plan {
        source_fingerprint: "test".into(),
        selected: BTreeSet::from([full.into()]),
        stubs: BTreeMap::from([(
            stub.into(),
            cf_domain::Stub {
                key: stub.into(),
                causes: vec![],
            },
        )]),
        members: BTreeMap::new(),
        source_hashes: BTreeMap::new(),
        warnings: vec![],
        diagnostics: vec![],
        choices: vec![],
        reasons: vec![],
        metrics: Default::default(),
        reader_profile: Default::default(),
    };
    let mut original = actual.clone();
    // An optional stub was removed while materialising the revised recipe.
    original.stubs.insert(
        "Catalog.Excluded".into(),
        cf_domain::Stub {
            key: "Catalog.Excluded".into(),
            causes: vec![],
        },
    );
    (
        Source(catalog),
        MaterializedPreview {
            original,
            actual,
            roles,
            files: BTreeMap::from([
                ("Documents/Selected.xml".into(), "full-proof".into()),
                (path.into(), "stub-proof".into()),
            ]),
        },
    )
}
fn build_role_fixture(
    source: &Source,
    plan: &MaterializedPreview,
    native: &RecordingNative,
    count: &Arc<AtomicUsize>,
) -> Result<PathBuf> {
    build_package(
        plan,
        source,
        &BTreeSet::from(["Catalog.Excluded".into()]),
        &BuildOptions {
            destination: "destination".into(),
            label: "label".into(),
            executable: "1cv8.exe".into(),
            mode: cf_domain::BuildMode::Auto,
            version: "8.3.27.2214".into(),
            stamp: "stamp".into(),
        },
        &BuildServices {
            output: &Output(count.clone()),
            builder: native,
            log: &Log,
        },
        &Cancellation::default(),
    )
}
#[test]
fn actual_materialized_roles_reach_the_native_builder() {
    let (source, plan) = role_fixture();
    let native = RecordingNative(std::sync::Mutex::new(vec![]));
    let count = Arc::new(AtomicUsize::new(0));
    build_role_fixture(&source, &plan, &native, &count).unwrap();
    let calls = native.0.lock().unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(
        calls[0].readback.stub_descriptors,
        plan.roles.stub_descriptors
    );
    assert_eq!(calls[0].inventory, plan.files);
    assert_eq!(count.load(Ordering::SeqCst), 1);
}
#[test]
fn invalid_or_downgraded_roles_abort_before_native_build_and_publication() {
    for mutation in 0..7 {
        let (source, mut plan) = role_fixture();
        match mutation {
            0 => plan.roles.stub_descriptors.clear(),
            1 => {
                plan.roles.stub_descriptors.insert(
                    "Documents/Selected.xml".into(),
                    ObjectKey::parse("Document.Selected").unwrap(),
                );
            }
            2 => {
                plan.roles.stub_descriptors.insert(
                    "Catalogs/Duplicate.xml".into(),
                    ObjectKey::parse("Catalog.Dependency").unwrap(),
                );
            }
            3 => {
                plan.files.remove("Catalogs/Dependency.xml");
            }
            4 => {
                let key = plan
                    .roles
                    .stub_descriptors
                    .remove("Catalogs/Dependency.xml")
                    .unwrap();
                plan.roles
                    .stub_descriptors
                    .insert("Documents/Selected.xml".into(), key);
            }
            5 => {
                plan.actual.selected.clear();
                plan.actual.stubs.insert(
                    "Document.Selected".into(),
                    cf_domain::Stub {
                        key: "Document.Selected".into(),
                        causes: vec![],
                    },
                );
            }
            6 => {
                plan.actual.selected.insert("Catalog.Dependency".into());
                plan.original.selected.insert("Catalog.Dependency".into());
            }
            _ => unreachable!(),
        }
        let native = RecordingNative(std::sync::Mutex::new(vec![]));
        let count = Arc::new(AtomicUsize::new(0));
        assert!(
            build_role_fixture(&source, &plan, &native, &count).is_err(),
            "mutation {mutation}"
        );
        assert!(native.0.lock().unwrap().is_empty(), "mutation {mutation}");
        assert_eq!(count.load(Ordering::SeqCst), 0, "mutation {mutation}");
    }
}

#[test]
fn resource_dependency_is_forwarded_as_full_and_cannot_be_downgraded() {
    for downgrade in [false, true] {
        let (source, mut plan) = role_fixture_kind(true);
        if downgrade {
            plan.roles.stub_descriptors = std::mem::take(&mut plan.roles.full_dependencies);
        }
        let native = RecordingNative(std::sync::Mutex::new(vec![]));
        let count = Arc::new(AtomicUsize::new(0));
        let result = build_role_fixture(&source, &plan, &native, &count);
        let calls = native.0.lock().unwrap();
        if downgrade {
            assert!(result.is_err());
            assert!(calls.is_empty());
            assert_eq!(count.load(Ordering::SeqCst), 0);
        } else {
            result.unwrap();
            assert_eq!(
                calls[0].readback.full_dependencies,
                plan.roles.full_dependencies
            );
            assert!(calls[0].readback.stub_descriptors.is_empty());
            assert_eq!(count.load(Ordering::SeqCst), 1);
        }
    }
}
