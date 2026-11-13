//! Use-case ports. Desktop composition supplies concrete services.
#[cfg(test)]
mod tests;
use cf_domain::{Catalog, Facts, Plan, Result};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};
#[derive(Clone, Default)]
pub struct Cancellation(Arc<AtomicBool>);
impl Cancellation {
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }
    pub fn check(&self) -> Result<()> {
        if self.0.load(Ordering::Acquire) {
            Err(cf_domain::Error::Cancelled)
        } else {
            Ok(())
        }
    }
}
pub trait MetadataSource: Send + Sync {
    fn manifest(&self) -> Option<&cf_domain::GitInputs> {
        None
    }
    fn materialized_root(&self) -> Option<&std::path::Path> {
        None
    }
    fn identity(&self) -> &str;
    fn catalog(&self) -> &Catalog;
    fn read(&self, relative: &str, cancel: &Cancellation) -> Result<Vec<u8>>;
    fn prefetch(&self, _paths: &[String], cancel: &Cancellation) -> Result<()> {
        cancel.check()
    }
    fn files_below(&self, relative: &str, cancel: &Cancellation) -> Result<Vec<String>>;
    fn exists(&self, relative: &str, cancel: &Cancellation) -> Result<bool>;
    fn list_directory(&self, relative: &str, cancel: &Cancellation) -> Result<Vec<SourceEntry>>;
    fn snapshot(&self, cancel: &Cancellation) -> Result<String>;
    fn is_pinned(&self) -> bool {
        false
    }
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceEntry {
    pub name: String,
    pub directory: bool,
}
pub trait FactStore: Send {
    fn lookup(&self, object: &str, content_hash: Option<&[u8]>) -> Result<Option<Facts>>;
    fn registrar_snapshot<'a>(
        &'a self,
        documents: Option<&BTreeMap<String, Vec<u8>>>,
    ) -> Result<Option<Box<dyn RegistrarLookup + 'a>>>;
    fn registrars(
        &self,
        register: &str,
        documents: Option<&BTreeMap<String, Vec<u8>>>,
    ) -> Result<Option<Vec<String>>> {
        self.registrar_snapshot(documents)?
            .map(|reader| reader.lookup(register))
            .transpose()
    }
}
pub trait RegistrarLookup {
    fn lookup(&self, register: &str) -> Result<Vec<String>>;
}
pub trait PreparedPlan: Send {
    fn composition(&self) -> &Plan;
    fn write(
        &self,
        source: &dyn MetadataSource,
        excluded: &BTreeSet<String>,
        staging: &std::path::Path,
        settings: &std::path::Path,
        cancel: &Cancellation,
    ) -> Result<StagedPackage>;
}
pub struct StagedPackage {
    pub composition: Plan,
    pub files: BTreeMap<String, String>,
    pub settings: PathBuf,
    pub settings_hash: String,
}
pub trait DependencyEngine: Send + Sync {
    fn preview(
        &self,
        source: &dyn MetadataSource,
        facts: Option<&dyn FactStore>,
        selected: &BTreeSet<String>,
        excluded: &BTreeSet<String>,
        cancel: &Cancellation,
    ) -> Result<Box<dyn PreparedPlan>>;
}
#[derive(Clone)]
pub struct NativeRequest {
    pub staging: PathBuf,
    pub work: PathBuf,
    pub executable: PathBuf,
    pub mode: cf_domain::BuildMode,
    pub platform_version: String,
    pub configuration: String,
    pub stamp: String,
    pub inventory: BTreeMap<String, String>,
}
pub trait NativeBuilder: Send + Sync {
    fn build(&self, request: &NativeRequest, cancel: &Cancellation) -> Result<NativeArtifact>;
}
pub struct NativeArtifact {
    pub path: PathBuf,
    pub sha256: String,
}
pub trait Journal: Send + Sync {
    fn event(&self, operation: &str, fields: &BTreeMap<String, String>) -> Result<()>;
}
pub trait PackageWorkspace: Send {
    fn staging(&self) -> PathBuf;
    fn settings(&self) -> PathBuf;
    fn native_work(&self) -> PathBuf;
    fn publish(
        &self,
        cf: &NativeArtifact,
        settings: &std::path::Path,
        settings_hash: &str,
        cancel: &Cancellation,
    ) -> Result<PathBuf>;
}
pub trait PackageOutput: Send + Sync {
    fn begin(
        &self,
        destination: &std::path::Path,
        label: &str,
        source: Option<&std::path::Path>,
    ) -> Result<Box<dyn PackageWorkspace>>;
}
pub struct BuildOptions {
    pub destination: PathBuf,
    pub label: String,
    pub executable: PathBuf,
    pub mode: cf_domain::BuildMode,
    pub version: String,
    pub stamp: String,
}
pub struct BuildServices<'a> {
    pub output: &'a dyn PackageOutput,
    pub builder: &'a dyn NativeBuilder,
    pub log: &'a dyn Journal,
}
/// One actor goal: build the frozen preview, validate it, then publish two files.
pub fn build_package(
    plan: &dyn PreparedPlan,
    source: &dyn MetadataSource,
    excluded: &BTreeSet<String>,
    options: &BuildOptions,
    services: &BuildServices<'_>,
    cancel: &Cancellation,
) -> Result<PathBuf> {
    let BuildServices {
        output,
        builder,
        log,
    } = services;
    cancel.check()?;
    plan.composition().validate_exclusions(excluded)?;
    let workspace = output.begin(
        &options.destination,
        &options.label,
        source.materialized_root(),
    )?;
    let staged = plan.write(
        source,
        excluded,
        &workspace.staging(),
        &workspace.settings(),
        cancel,
    )?;
    log.event(
        "staging-ready",
        &BTreeMap::from([("files".into(), staged.files.len().to_string())]),
    )?;
    let request = NativeRequest {
        staging: workspace.staging(),
        work: workspace.native_work(),
        executable: options.executable.clone(),
        mode: options.mode,
        platform_version: options.version.clone(),
        configuration: source.catalog().name.clone(),
        stamp: options.stamp.clone(),
        inventory: staged.files,
    };
    let cf = builder.build(&request, cancel)?;
    log.event("native-readback-passed", &BTreeMap::new())?;
    let package = workspace.publish(&cf, &staged.settings, &staged.settings_hash, cancel)?;
    // The atomic publication is the commit point. A trailing log failure must not
    // turn a delivered package into an apparent failure and encourage a duplicate.
    let _ = log.event(
        "package-published",
        &BTreeMap::from([("folder".into(), package.to_string_lossy().into())]),
    );
    Ok(package)
}
