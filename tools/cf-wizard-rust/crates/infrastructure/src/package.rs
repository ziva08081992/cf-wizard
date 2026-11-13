//! Materialise a frozen dependency recipe; preview never writes staging.
use crate::{
    canonical::sha256,
    engine::{Recipe, metadata_listing, object_files, resolve},
    kinds::{STUB_MODULES, TYPE_FOLDERS},
    metadata::{member_name, object_element, stable_uuid},
    source::{no_links, relative_path},
    stubs::{clean_properties, content_of, merge_settings},
    xml::{Edit, Xml, escape},
};
use cf_application::{Cancellation, MetadataSource, PreparedPlan, StagedPackage};
use cf_domain::{Error, Plan, Result};
use regex::Regex;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::Path,
    sync::OnceLock,
};

impl PreparedPlan for Recipe {
    fn composition(&self) -> &Plan {
        &self.composition
    }
    fn write(
        &self,
        source: &dyn MetadataSource,
        excluded: &BTreeSet<String>,
        staging: &Path,
        settings: &Path,
        cancel: &Cancellation,
    ) -> Result<StagedPackage> {
        self.composition.validate_exclusions(excluded)?;
        let revised = if excluded.is_empty() {
            None
        } else {
            self.verify(source, cancel)?;
            let recipe = resolve(source, None, &self.composition.selected, excluded, cancel)?;
            if recipe.composition.source_fingerprint != self.composition.source_fingerprint
                || recipe
                    .composition
                    .stubs
                    .keys()
                    .any(|k| excluded.contains(k) || !self.composition.stubs.contains_key(k))
            {
                return Err(Error::Invalid(
                    "Изменились обязательные зависимости: выполните новый предпросмотр".into(),
                ));
            }
            Some(recipe)
        };
        revised
            .as_ref()
            .unwrap_or(self)
            .materialize(source, staging, settings, cancel)
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
        for key in &self.composition.selected {
            for relative in &self.below[key] {
                copy(source, self, staging, relative, &mut files, cancel)?;
            }
        }
        let mut rules = BTreeMap::<String, Vec<String>>::new();
        for (key, built) in &self.built {
            let (kind, name) = key.split_once('.').expect("metadata key");
            let folder = TYPE_FOLDERS
                .iter()
                .find(|&&(k, _)| k == kind)
                .map(|&(_, f)| f)
                .expect("known kind");
            let base = format!("{folder}/{name}/");
            put(
                staging,
                &format!("{folder}/{name}.xml"),
                built.output.descriptor.as_bytes(),
                &mut files,
            )?;
            for relative in &built.files {
                copy(source, self, staging, relative, &mut files, cancel)?;
            }
            let mut properties = vec![];
            for module in STUB_MODULES {
                if has_text(source, &format!("{base}Ext/{module}.bsl"), cancel)? {
                    properties.push((*module).into());
                }
            }
            if kind == "FunctionalOption"
                && content_of(&self.descriptors[key]) != content_of(&built.output.descriptor)
            {
                properties.push("Content".into());
            }
            if !properties.is_empty() {
                rules.insert(key.clone(), properties);
            }
            for pattern in &built.output.patterns {
                if pattern.starts_with("Commands/")
                    && has_text(
                        source,
                        &format!("{base}{pattern}Ext/CommandModule.bsl"),
                        cancel,
                    )?
                {
                    rules.insert(
                        format!("{key}.Command.{}", &pattern[9..pattern.len() - 1]),
                        vec!["CommandModule".into()],
                    );
                } else if pattern.starts_with("Forms/")
                    && pattern.ends_with(".xml")
                    && has_text(
                        source,
                        &format!(
                            "{base}{}/Ext/Form/Module.bsl",
                            pattern.trim_end_matches(".xml")
                        ),
                        cancel,
                    )?
                {
                    rules.insert(
                        format!("{key}.Form.{}", &pattern[6..pattern.len() - 4]),
                        vec!["Form".into()],
                    );
                }
            }
        }
        let included: BTreeSet<_> = self
            .composition
            .selected
            .iter()
            .chain(self.composition.stubs.keys())
            .cloned()
            .collect();
        let composition = self.composition.clone();
        let cfg = String::from_utf8(source.read("Configuration.xml", cancel)?)
            .map_err(crate::external)?;
        let xml = Xml::parse(cfg.as_bytes())?;
        let object = object_element(&xml)
            .filter(|&o| xml.elements[o].name == "Configuration")
            .ok_or_else(|| Error::Invalid("Нет Configuration в корневом XML".into()))?;
        let mut edits = vec![];
        let subsystem = "_Перенос";
        let sub_xml = format!("<Subsystem>{subsystem}</Subsystem>");
        for &section in &xml.elements[object].children {
            let node = &xml.elements[section];
            if node.name == "Properties" {
                edits.push(Edit {
                    range: node.span.clone(),
                    replacement: clean_properties(
                        &xml,
                        section,
                        "Configuration",
                        "Language",
                        &BTreeSet::new(),
                        false,
                        &BTreeSet::new(),
                    ),
                });
            } else if node.name == "ChildObjects" {
                let mut body = String::new();
                let mut last = node.inner.start;
                let mut placed = false;
                let mut indent = "";
                for &item in &node.children {
                    let element = &xml.elements[item];
                    let name = &cfg[element.inner.clone()];
                    let gap = &cfg[last..element.span.start];
                    last = element.span.end;
                    if !placed
                        && element.name != "Language"
                        && (element.name != "Subsystem" || name > subsystem)
                    {
                        body.push_str(gap);
                        body.push_str(&sub_xml);
                        placed = true;
                    }
                    if element.name == "Language"
                        || included.contains(&format!("{}.{name}", element.name))
                    {
                        body.push_str(gap);
                        body.push_str(xml.raw(item));
                    }
                    if element.name == "Language" {
                        let relative = format!("Languages/{name}.xml");
                        if source.exists(&relative, cancel)? {
                            copy(source, self, staging, &relative, &mut files, cancel)?;
                        }
                    }
                    indent = gap;
                }
                if !placed {
                    body.push_str(indent);
                    body.push_str(&sub_xml);
                }
                edits.push(Edit {
                    range: node.span.clone(),
                    replacement: format!(
                        "<ChildObjects>{body}{}</ChildObjects>",
                        &cfg[last..node.inner.end]
                    ),
                });
            }
        }
        put(
            staging,
            "Configuration.xml",
            xml.apply(edits)?.as_bytes(),
            &mut files,
        )?;
        let uuid = cfg[xml.elements[object].span.start..]
            .find("uuid=\"")
            .map(|at| {
                let start = xml.elements[object].span.start + at + 6;
                cfg[start..].chars().take(36).collect::<String>()
            })
            .unwrap_or_default();
        let items = composition
            .selected
            .iter()
            .map(|key| {
                format!(
                    "\t\t\t\t<xr:Item xsi:type=\"xr:MDObjectRef\">{}</xr:Item>\r\n",
                    escape(key)
                )
            })
            .collect::<String>();
        let subsystem_xml = format!(
            "{}<Subsystem uuid=\"{}\">\r\n\
            \t\t<Properties>\r\n\t\t\t<Name>{subsystem}</Name>\r\n\t\t\t<Synonym/>\r\n\t\t\t<Comment/>\r\n\
            \t\t\t<IncludeHelpInContents>false</IncludeHelpInContents>\r\n\
            \t\t\t<IncludeInCommandInterface>false</IncludeInCommandInterface>\r\n\
            \t\t\t<UseOneCommand>false</UseOneCommand>\r\n\t\t\t<Explanation/>\r\n\t\t\t<Picture/>\r\n\
            \t\t\t<Content>\r\n{items}\t\t\t</Content>\r\n\t\t</Properties>\r\n\
            \t\t<ChildObjects/>\r\n\t</Subsystem>\r\n</MetaDataObject>",
            &cfg[..xml.elements[object].span.start],
            stable_uuid(&format!("{uuid}/partial-cf/{subsystem}"))
        );
        put(
            staging,
            "Subsystems/_Перенос.xml",
            subsystem_xml.as_bytes(),
            &mut files,
        )?;
        let configuration = member_name(&xml, object);
        let stubs = composition.stubs.keys().cloned().collect();
        let settings_xml = merge_settings(
            &configuration,
            &composition.selected,
            &stubs,
            &rules,
            subsystem,
        );
        if let Some(parent) = settings.parent() {
            fs::create_dir_all(parent).map_err(crate::external)?;
        }
        fs::write(settings, &settings_xml).map_err(crate::external)?;
        self.verify(source, cancel)?;
        metadata_coverage(staging, &included)?;
        Ok(StagedPackage {
            composition,
            files,
            settings: settings.into(),
            settings_hash: sha256(settings_xml.as_bytes()),
        })
    }
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
fn copy(
    source: &dyn MetadataSource,
    recipe: &Recipe,
    root: &Path,
    relative: &str,
    files: &mut BTreeMap<String, String>,
    cancel: &Cancellation,
) -> Result<()> {
    let data = source.read(relative, cancel)?;
    if recipe
        .composition
        .source_hashes
        .get(relative)
        .is_some_and(|expected| *expected != sha256(&data))
    {
        return Err(Error::Invalid(format!(
            "Изменился исходный файл {relative}"
        )));
    }
    put(root, relative, &data, files)
}
fn has_text(source: &dyn MetadataSource, relative: &str, cancel: &Cancellation) -> Result<bool> {
    if !source.exists(relative, cancel)? {
        return Ok(false);
    }
    let data = source.read(relative, cancel)?;
    let text = std::str::from_utf8(&data).map_err(crate::external)?;
    Ok(!text
        .trim_start_matches('\u{feff}')
        .trim_matches([' ', '\t', '\r', '\n', '\x0b', '\x0c'])
        .is_empty())
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
