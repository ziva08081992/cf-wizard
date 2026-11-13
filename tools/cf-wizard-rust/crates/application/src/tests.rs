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
