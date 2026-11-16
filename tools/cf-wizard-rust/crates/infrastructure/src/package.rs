//! Materialise a frozen dependency recipe; preview never writes staging.
use crate::{
    canonical::sha256,
    engine::{Recipe, metadata_listing, object_files, reprepare},
    source::{no_links, relative_path},
};
use cf_application::{Cancellation, MetadataSource, PreparedPlan, ReadbackContract, StagedPackage};
use cf_domain::{
    Error, ObjectKey, Plan, Result,
    package_plan::{ContentMode, FileOperation, SelectionRole},
};
#[cfg(test)]
use regex::Regex;
#[cfg(test)]
use std::sync::OnceLock;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::Path,
};

impl PreparedPlan for Recipe {
    fn composition(&self) -> &Plan {
        &self.composition
    }
    fn validate_exclusions(&self, excluded: &BTreeSet<String>) -> Result<()> {
        self.composition
            .validate_exclusions(&excluded.difference(&self.excluded).cloned().collect())
    }
    fn revised(
        &self,
        source: &dyn MetadataSource,
        excluded: &BTreeSet<String>,
        cancel: &Cancellation,
    ) -> Result<Option<Box<dyn PreparedPlan>>> {
        self.validate_exclusions(excluded)?;
        if excluded.is_subset(&self.excluded) {
            self.composition.require_complete()?;
            return Ok(None);
        }
        self.verify(source, cancel)?;
        let cumulative = self.excluded.union(excluded).cloned().collect();
        let recipe = reprepare(source, self, &cumulative, cancel)?;
        if recipe.composition.source_fingerprint != self.composition.source_fingerprint
            || recipe
                .composition
                .stubs
                .keys()
                .any(|k| cumulative.contains(k) || !self.composition.stubs.contains_key(k))
        {
            return Err(Error::Invalid(
                "Изменились обязательные зависимости: выполните новый предпросмотр".into(),
            ));
        }
        recipe.composition.require_complete()?;
        Ok(Some(Box::new(recipe)))
    }
    fn write(
        &self,
        source: &dyn MetadataSource,
        excluded: &BTreeSet<String>,
        staging: &Path,
        settings: &Path,
        cancel: &Cancellation,
    ) -> Result<StagedPackage> {
        if let Some(revised) = self.revised(source, excluded, cancel)? {
            return revised.write(source, excluded, staging, settings, cancel);
        }
        self.composition.require_complete()?;
        self.materialize(source, staging, settings, cancel)
    }
}
impl Recipe {
    pub(crate) fn verify(&self, source: &dyn MetadataSource, cancel: &Cancellation) -> Result<()> {
        if source.snapshot(cancel)? != self.composition.source_fingerprint {
            return Err(Error::Invalid(
                "Исходники изменились: выполните новый предпросмотр".into(),
            ));
        }
        for (directory, entries) in &self.listings {
            cancel.check()?;
            if &metadata_listing(source, directory, cancel)? != entries {
                return Err(Error::Invalid(format!(
                    "Изменился состав XML в {directory}"
                )));
            }
        }
        for (key, files) in &self.below {
            cancel.check()?;
            if &object_files(source, key, cancel)? != files {
                return Err(Error::Invalid(format!("Изменился состав файлов {key}")));
            }
        }
        // Exact entries in a verified inventory prove presence. Missing exact
        // spelling cannot prove absence: the provider may ignore path case.
        let covered: BTreeSet<_> = self
            .below
            .keys()
            .filter_map(|key| {
                crate::source_schema::descriptor(key)
                    .map(|path| format!("{}/", path.trim_end_matches(".xml")))
            })
            .collect();
        let files: BTreeSet<_> = self.below.values().flatten().map(String::as_str).collect();
        for (path, expected) in &self.presence {
            cancel.check()?;
            let inherited = path
                .match_indices('/')
                .any(|(offset, _)| covered.contains(&path[..=offset]));
            let present =
                inherited && files.contains(path.as_str()) || source.exists(path, cancel)?;
            if present != *expected {
                return Err(Error::Invalid(format!(
                    "Изменилось наличие {path}: выполните новый предпросмотр"
                )));
            }
        }
        for (path, expected) in &self.composition.source_hashes {
            if sha256(&source.read(path, cancel)?) != *expected {
                return Err(Error::Invalid(format!(
                    "Изменился {path}: выполните новый предпросмотр"
                )));
            }
        }
        Ok(())
    }
    fn materialize(
        &self,
        source: &dyn MetadataSource,
        staging: &Path,
        settings: &Path,
        cancel: &Cancellation,
    ) -> Result<StagedPackage> {
        self.verify(source, cancel)?;
        let plan = self
            .compiled
            .as_ref()
            .ok_or_else(|| Error::Invalid("Нет окончательного плана выдачи".into()))?;
        plan.validate()?;
        no_links(staging)?;
        no_links(settings)?;
        if staging.exists()
            && fs::read_dir(staging)
                .map_err(crate::external)?
                .next()
                .is_some()
        {
            return Err(Error::Invalid(
                "Каталог staging должен быть новым или пустым".into(),
            ));
        }
        if settings.exists() {
            return Err(Error::Invalid(
                "Файл настроек объединения уже существует".into(),
            ));
        }
        fs::create_dir_all(staging).map_err(crate::external)?;
        let mut files = BTreeMap::new();
        for (path, file) in &plan.projection.files {
            cancel.check()?;
            let data = match &file.operation {
                FileOperation::Copy { source: path }
                | FileOperation::Project { source: path, .. } => {
                    let data = source.read(path, cancel)?;
                    let expected = &self.composition.source_hashes[path];
                    if sha256(&data) != *expected {
                        return Err(Error::Invalid(format!("Изменился исходный файл {path}")));
                    }
                    if let FileOperation::Project { edits, .. } = &file.operation {
                        crate::source_edits::emit(&data, edits)?
                    } else {
                        data
                    }
                }
                FileOperation::Generate { bytes } => bytes.clone(),
            };
            put(staging, path, &data, &mut files)?;
        }
        let actual = inventory(staging)?;
        crate::emission_guard::package(plan, &actual, |path| {
            fs::read(staging.join(path)).map_err(crate::external)
        })?;
        // Readback roles are assigned only after the independent inventory,
        // byte and source-derived logical expectations passed.
        let mut readback = ReadbackContract::default();
        for (key, object) in &plan.projection.objects {
            if object.role != SelectionRole::Dependency {
                continue;
            }
            let roles = if object.content == ContentMode::Full {
                &mut readback.full_dependencies
            } else {
                &mut readback.stub_descriptors
            };
            roles.insert(object.descriptor.clone(), ObjectKey::parse(key)?);
        }
        let settings_xml = crate::package_compiler::settings(plan)?;
        crate::emission_guard::settings(plan, settings_xml.as_bytes())?;
        if let Some(parent) = settings.parent() {
            fs::create_dir_all(parent).map_err(crate::external)?;
        }
        fs::write(settings, &settings_xml).map_err(crate::external)?;
        self.verify(source, cancel)?;
        let included = self
            .composition
            .selected
            .iter()
            .chain(self.composition.stubs.keys())
            .cloned()
            .collect();
        metadata_coverage(staging, &included)?;
        Ok(StagedPackage {
            composition: self.composition.clone(),
            files,
            readback,
            settings: settings.into(),
            settings_hash: sha256(settings_xml.as_bytes()),
        })
    }
}
fn inventory(root: &Path) -> Result<BTreeSet<String>> {
    let mut directories = vec![root.to_path_buf()];
    let mut files = BTreeSet::new();
    while let Some(directory) = directories.pop() {
        for entry in fs::read_dir(&directory).map_err(crate::external)? {
            let entry = entry.map_err(crate::external)?;
            let path = entry.path();
            no_links(&path)?;
            let kind = entry.file_type().map_err(crate::external)?;
            if kind.is_dir() {
                directories.push(path);
            } else if kind.is_file() {
                files.insert(
                    path.strip_prefix(root)
                        .map_err(crate::external)?
                        .to_string_lossy()
                        .replace('\\', "/"),
                );
            } else {
                return Err(Error::Invalid(
                    "В staging обнаружен неподдерживаемый вид файла".into(),
                ));
            }
        }
    }
    Ok(files)
}
fn put(
    root: &Path,
    relative: &str,
    data: &[u8],
    files: &mut BTreeMap<String, String>,
) -> Result<()> {
    relative_path(relative)?;
    if files.contains_key(relative) {
        return Err(Error::Invalid(format!("Повтор выходного файла {relative}")));
    }
    let path = root.join(relative);
    no_links(&path)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(crate::external)?;
    }
    fs::write(path, data).map_err(crate::external)?;
    files.insert(relative.into(), sha256(data));
    Ok(())
}
pub fn metadata_coverage(root: &Path, expected: &BTreeSet<String>) -> Result<()> {
    let cfg = crate::source::catalog_generated(
        &fs::read(root.join("Configuration.xml")).map_err(crate::external)?,
    )?;
    let found: BTreeSet<_> = cfg
        .objects
        .keys()
        .filter(|k| k.kind() != "Language" && k.0 != "Subsystem._Перенос")
        .map(|k| k.0.clone())
        .collect();
    if &found != expected {
        return Err(Error::Invalid(
            "Состав подготовленной конфигурации не совпадает с планом".into(),
        ));
    }
    for object in cfg.objects.values() {
        if !root.join(&object.descriptor).is_file() {
            return Err(Error::Invalid(format!("Нет файла {}", object.descriptor)));
        }
    }
    Ok(())
}
#[cfg(test)]
pub fn predefined_codes(descriptor: &str, predefined: &str) -> Vec<String> {
    static ZERO: OnceLock<Regex> = OnceLock::new();
    static NUMBER: OnceLock<Regex> = OnceLock::new();
    static CODE: OnceLock<Regex> = OnceLock::new();
    static TYPE: OnceLock<Regex> = OnceLock::new();
    static ZEROS: OnceLock<Regex> = OnceLock::new();
    if !ZERO
        .get_or_init(|| Regex::new(r"<CodeLength>\s*0\s*</CodeLength>").expect("literal regex"))
        .is_match(descriptor)
    {
        return vec![];
    }
    let numeric = NUMBER
        .get_or_init(|| Regex::new(r"<CodeType>\s*Number\s*</CodeType>").expect("literal regex"))
        .is_match(descriptor);
    let mut coded = false;
    let mut names = vec![];
    let type_pattern = TYPE.get_or_init(|| {
        Regex::new(r#"(?:^|\s)xsi:type\s*=\s*(['"])([^'"]*)(['"])"#).expect("literal regex")
    });
    let zeros =
        ZEROS.get_or_init(|| Regex::new(r"^[+-]?(?:0+(?:\.0*)?|\.0+)$").expect("literal regex"));
    for code in CODE
        .get_or_init(|| {
            Regex::new(r"(?:<Name>([^<]*)</Name>\s*)?<Code(\s[^>]*)?>([^<]*)</Code>")
                .expect("literal regex")
        })
        .captures_iter(predefined)
    {
        let value = code[3].trim();
        if value.is_empty() {
            continue;
        }
        let kind = type_pattern.captures(code.get(2).map_or("", |m| m.as_str()));
        let is_number = kind
            .filter(|c| c[1] == c[3])
            .map_or(numeric, |c| c[2].trim() == "xs:decimal");
        if is_number && zeros.is_match(value) {
            continue;
        }
        coded = true;
        if let Some(name) = code.get(1) {
            names.push(name.as_str().into());
        }
    }
    if coded && names.is_empty() {
        names.push(String::new());
    }
    names
}
