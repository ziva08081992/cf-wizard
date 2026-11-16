//! Use-case ports. Desktop composition supplies concrete services.
#[cfg(test)]
mod tests;
use cf_domain::{Catalog, Error, Facts, ObjectKey, Plan, Result};
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
    /// Namespace-sensitive registrar facts are distinct from V1 lexer payloads.
    fn source_registrar_snapshot<'a>(
        &'a self,
        _documents: Option<&BTreeMap<String, Vec<u8>>>,
    ) -> Result<Option<Box<dyn RegistrarLookup + 'a>>> {
        Ok(None)
    }
    /// A legacy V1 adapter cannot claim origin-aware facts from a newer reader.
    fn source_facts(
        &self,
        _object: &str,
        _content_hash: Option<&[u8]>,
    ) -> Result<Option<cf_domain::source_facts::SourceFacts>> {
        Ok(None)
    }
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
    fn validate_exclusions(&self, excluded: &BTreeSet<String>) -> Result<()> {
        self.composition().validate_exclusions(excluded)
    }
    /// Apply additional exclusions to this frozen plan before creating a workspace.
    /// None reuses the plan, including exclusions already baked into its preview.
    fn revised(
        &self,
        _source: &dyn MetadataSource,
        _excluded: &BTreeSet<String>,
        _cancel: &Cancellation,
    ) -> Result<Option<Box<dyn PreparedPlan>>> {
        Ok(None)
    }
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
    pub readback: ReadbackContract,
    pub settings: PathBuf,
    pub settings_hash: String,
}
/// Every other inventory XML/BSL keeps full comparison. Roles come from the final recipe.
#[derive(Clone, Debug, Default)]
pub struct ReadbackContract {
    pub stub_descriptors: BTreeMap<String, ObjectKey>,
    pub full_dependencies: BTreeMap<String, ObjectKey>,
}
impl ReadbackContract {
    pub fn validate(
        &self,
        composition: &Plan,
        catalog: &Catalog,
        files: &BTreeMap<String, String>,
    ) -> Result<()> {
        let entries = || {
            self.stub_descriptors
                .iter()
                .chain(self.full_dependencies.iter())
        };
        let keys: BTreeSet<_> = entries().map(|(_, key)| &key.0).collect();
        let paths: BTreeSet<_> = entries().map(|(path, _)| path).collect();
        let total = self.stub_descriptors.len() + self.full_dependencies.len();
        let expected: BTreeSet<_> = composition.stubs.keys().collect();
        if self
            .stub_descriptors
            .values()
            .any(|key| cf_domain::full_resource(key.kind()))
            || self
                .full_dependencies
                .values()
                .any(|key| !cf_domain::full_resource(key.kind()))
        {
            return Err(Error::Invalid(
                "Неверная роль ресурсной зависимости readback".into(),
            ));
        }
        if keys.len() != total
            || paths.len() != total
            || keys != expected
            || keys.iter().any(|key| composition.selected.contains(*key))
        {
            return Err(Error::Invalid(
                "Роли readback не соответствуют составу заглушек".into(),
            ));
        }
        for (path, key) in entries() {
            let object = catalog.objects.get(key).ok_or_else(|| {
                Error::Invalid(format!("Неизвестная заглушка в readback: {}", key.0))
            })?;
            if object.descriptor != *path || !files.contains_key(path) {
                return Err(Error::Invalid(format!(
                    "Нет descriptor заглушки в составе: {}",
                    key.0
                )));
            }
        }
        Ok(())
    }
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
    pub readback: ReadbackContract,
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
    plan.validate_exclusions(excluded)?;
    let revised = plan.revised(source, excluded, cancel)?;
    let final_plan = revised.as_deref().unwrap_or(plan);
    final_plan.composition().require_complete()?;
    let workspace = output.begin(
        &options.destination,
        &options.label,
        source.materialized_root(),
    )?;
    let staged = final_plan.write(
        source,
        excluded,
        &workspace.staging(),
        &workspace.settings(),
        cancel,
    )?;
    staged.composition.require_complete()?;
    if staged.composition.selected != plan.composition().selected {
        return Err(Error::Invalid(
            "При подготовке изменился выбор полных объектов".into(),
        ));
    }
    staged
        .readback
        .validate(&staged.composition, source.catalog(), &staged.files)?;
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
        readback: staged.readback,
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
