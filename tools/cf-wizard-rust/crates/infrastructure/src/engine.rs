//! Deterministic dependency closure. All IO arrives through application ports.
#[cfg(test)]
#[path = "engine_memo_tests.rs"]
mod memo_tests;
#[cfg(test)]
#[path = "engine_tests.rs"]
pub(crate) mod tests;
use crate::{
    canonical::sha256,
    forms::{form_paths, module_values, without_bsl_comments},
    kinds::{
        KEPT_MODULES, OPEN_PROPERTIES, REGISTER_KINDS, RESOURCE_SUFFIXES, SIDE_SUFFIXES,
        TYPE_FOLDERS,
    },
    metadata::{Reference, child, descriptor, extract_facts, object_element, scan},
    stubs::{StubOutput, StubSpec, TrackedPresence, stub_descriptor_tracked},
    xml::Xml,
};
use cf_application::{
    Cancellation, DependencyEngine, FactStore, MetadataSource, PreparedPlan, SourceEntry,
};
use cf_domain::{Cause, Error, Facts, Member, Plan, Result, Stub, fold};
use sha2::{Digest, Sha256};
use std::{
    cmp::Reverse,
    collections::{BTreeMap, BTreeSet, BinaryHeap},
};

#[derive(Clone, Debug, Default)]
struct State {
    spec: StubSpec,
    primary: String,
    causes: BTreeMap<String, bool>,
}
#[derive(Clone)]
pub(crate) struct Built {
    pub output: StubOutput,
    pub files: Vec<String>,
}
#[derive(Clone)]
pub(crate) struct Recipe {
    pub composition: Plan,
    pub built: BTreeMap<String, Built>,
    pub below: BTreeMap<String, Vec<String>>,
    pub listings: BTreeMap<String, Vec<SourceEntry>>,
    pub descriptors: BTreeMap<String, String>,
}
pub struct RustEngine;
impl DependencyEngine for RustEngine {
    fn preview(
        &self,
        source: &dyn MetadataSource,
        facts: Option<&dyn FactStore>,
        selected: &BTreeSet<String>,
        excluded: &BTreeSet<String>,
        cancel: &Cancellation,
    ) -> Result<Box<dyn PreparedPlan>> {
        Ok(Box::new(resolve(
            source, facts, selected, excluded, cancel,
        )?))
    }
}
pub(crate) fn object_files(
    source: &dyn MetadataSource,
    key: &str,
    cancel: &Cancellation,
) -> Result<Vec<String>> {
    let Some(rel) = descriptor(key) else {
        return Ok(vec![]);
    };
    if !source.exists(&rel, cancel)? {
        return Ok(vec![]);
    }
    let base = rel.strip_suffix(".xml").expect("descriptor extension");
    let mut files = vec![rel.clone()];
    files.extend(source.files_below(base, cancel)?);
    Ok(files)
}
pub(crate) fn metadata_listing(
    source: &dyn MetadataSource,
    directory: &str,
    cancel: &Cancellation,
) -> Result<Vec<SourceEntry>> {
    let mut entries: Vec<_> = source
        .list_directory(directory, cancel)?
        .into_iter()
        .filter(|v| !v.directory && v.name.ends_with(".xml"))
        .collect();
    entries.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(entries)
}
pub fn records_fragment(data: &[u8]) -> &[u8] {
    let start = data
        .windows(b"<RegisterRecords>".len())
        .position(|w| w == b"<RegisterRecords>");
    let end = data
        .windows(b"</RegisterRecords>".len())
        .position(|w| w == b"</RegisterRecords>");
    match (start, end) {
        (Some(a), Some(b)) if b >= a => &data[a..b],
        _ => b"",
    }
}
type CommonAttributeUsage = BTreeMap<String, (String, String, BTreeMap<String, String>)>;
struct Session<'a> {
    source: &'a dyn MetadataSource,
    index: Option<&'a dyn FactStore>,
    cancel: &'a Cancellation,
    selected: BTreeSet<String>,
    excluded: BTreeSet<String>,
    proof: String,
    states: BTreeMap<String, State>,
    dirty: BTreeSet<String>,
    heap: BinaryHeap<Reverse<String>>,
    descriptors: BTreeMap<String, String>,
    stub_inputs: BTreeMap<String, (StubSpec, BTreeMap<String, bool>)>,
    #[cfg(test)]
    stub_builds: BTreeMap<String, usize>,
    facts: BTreeMap<String, Facts>,
    below: BTreeMap<String, Vec<String>>,
    listings: BTreeMap<String, Vec<SourceEntry>>,
    names: BTreeMap<String, BTreeMap<String, String>>,
    hashes: BTreeMap<String, String>,
    built: BTreeMap<String, Built>,
    comment: bool,
    common: Option<CommonAttributeUsage>,
    registrars: BTreeMap<String, Vec<String>>,
    document_proof: Option<BTreeMap<String, Vec<u8>>>,
    fragments: Vec<(String, String)>,
    fallback_registrars: Option<BTreeMap<String, Vec<String>>>,
    registrar_index: Option<Box<dyn cf_application::RegistrarLookup + 'a>>,
    registrar_index_checked: bool,
}
impl<'a> Session<'a> {
    fn new(
        source: &'a dyn MetadataSource,
        index: Option<&'a dyn FactStore>,
        selected: &BTreeSet<String>,
        excluded: &BTreeSet<String>,
        cancel: &'a Cancellation,
    ) -> Result<Self> {
        if selected.is_empty() {
            return Err(Error::Invalid("Не выбрано ни одного объекта".into()));
        }
        for key in selected {
            let kind = key.split('.').next().unwrap_or_default();
            if matches!(kind, "Language" | "Configuration")
                || !source.catalog().objects.contains_key(key.as_str())
            {
                return Err(Error::Invalid(format!(
                    "Объект отсутствует или не выбирается: {key}"
                )));
            }
            let path = descriptor(key)
                .ok_or_else(|| Error::Invalid(format!("Неподдерживаемый объект {key}")))?;
            if !source.exists(&path, cancel)? {
                return Err(Error::Invalid(format!("Нет descriptor: {key}")));
            }
        }
        if source.exists("Subsystems/_Перенос.xml", cancel)? {
            return Err(Error::Invalid(
                "В исходниках уже есть подсистема _Перенос".into(),
            ));
        }
        Ok(Self {
            source,
            index,
            cancel,
            selected: selected.clone(),
            excluded: excluded.clone(),
            proof: source.snapshot(cancel)?,
            states: BTreeMap::new(),
            dirty: BTreeSet::new(),
            heap: BinaryHeap::new(),
            descriptors: BTreeMap::new(),
            stub_inputs: BTreeMap::new(),
            #[cfg(test)]
            stub_builds: BTreeMap::new(),
            facts: BTreeMap::new(),
            below: BTreeMap::new(),
            listings: BTreeMap::new(),
            names: BTreeMap::new(),
            hashes: BTreeMap::new(),
            built: BTreeMap::new(),
            comment: false,
            common: None,
            registrars: BTreeMap::new(),
            document_proof: None,
            registrar_index: None,
            registrar_index_checked: false,
            fragments: vec![],
            fallback_registrars: None,
        })
    }
    fn read(&mut self, relative: &str) -> Result<String> {
        let data = self.source.read(relative, self.cancel)?;
        self.record_hash(relative, &data)?;
        String::from_utf8(data).map_err(crate::external)
    }
    fn record_hash(&mut self, relative: &str, data: &[u8]) -> Result<()> {
        let digest = sha256(data);
        if self.hashes.get(relative).is_some_and(|old| old != &digest) {
            return Err(Error::Invalid(format!(
                "Исходники изменились во время предпросмотра: {relative}"
            )));
        }
        self.hashes.entry(relative.into()).or_insert(digest);
        Ok(())
    }
    fn descriptor(&mut self, key: &str) -> Result<String> {
        if let Some(text) = self.descriptors.get(key) {
            return Ok(text.clone());
        }
        let rel = descriptor(key).ok_or_else(|| Error::Invalid(format!("Нет папки для {key}")))?;
        let text = self.read(&rel)?;
        self.descriptors.insert(key.into(), text.clone());
        Ok(text)
    }
    fn exists(&self, key: &str) -> Result<bool> {
        descriptor(key).map_or(Ok(false), |rel| self.source.exists(&rel, self.cancel))
    }
    fn list(&mut self, directory: &str) -> Result<Vec<SourceEntry>> {
        if let Some(rows) = self.listings.get(directory) {
            return Ok(rows.clone());
        }
        let rows = metadata_listing(self.source, directory, self.cancel)?;
        self.listings.insert(directory.into(), rows.clone());
        Ok(rows)
    }
    fn below(&mut self, key: &str) -> Result<Vec<String>> {
        if let Some(files) = self.below.get(key) {
            return Ok(files.clone());
        }
        let files = object_files(self.source, key, self.cancel)?;
        self.below.insert(key.into(), files.clone());
        Ok(files)
    }
    fn canonical(&mut self, key: &str) -> Result<String> {
        let Some((kind, name)) = key.split_once('.') else {
            return Ok(key.into());
        };
        let Some(&(_, folder)) = TYPE_FOLDERS.iter().find(|&&(k, _)| k == kind) else {
            return Ok(key.into());
        };
        if !self.names.contains_key(folder) {
            let mut names = BTreeMap::new();
            for entry in self.list(folder)? {
                let real = entry.name.strip_suffix(".xml").expect("filtered XML");
                names.entry(fold(real)).or_insert_with(|| real.to_owned());
            }
            self.names.insert(folder.into(), names);
        }
        Ok(self
            .names
            .get(folder)
            .and_then(|names| names.get(&fold(name)))
            .map_or_else(|| key.to_owned(), |real| format!("{kind}.{real}")))
    }
    fn facts(&mut self, key: &str) -> Result<Facts> {
        if let Some(facts) = self.facts.get(key) {
            return Ok(facts.clone());
        }
        if self.source.is_pinned()
            && let Some(index) = self.index
            && let Some(facts) = index.lookup(key, None)?
        {
            self.facts.insert(key.into(), facts.clone());
            return Ok(facts);
        }
        let text = self.descriptor(key)?;
        let hash = Sha256::digest(text.as_bytes());
        let cached = if let Some(index) = self.index {
            index.lookup(key, Some(&hash))?
        } else {
            None
        };
        let facts = cached.map_or_else(|| extract_facts(text.as_bytes()), Ok)?;
        self.facts.insert(key.into(), facts.clone());
        Ok(facts)
    }
    fn mark(&mut self, key: &str) {
        if self.dirty.insert(key.into()) {
            self.heap.push(Reverse(key.into()));
        }
    }
    fn note(&mut self, r: Reference, owner: &str) -> Result<()> {
        self.cancel.check()?;
        let key = r.key();
        if self.selected.contains(&key) || r.kind == "Language" || !self.exists(&key)? {
            return Ok(());
        }
        let required = !self.comment;
        if !required && self.excluded.contains(&key) {
            return Ok(());
        }
        let fresh = !self.states.contains_key(&key);
        let state = self.states.entry(key.clone()).or_insert_with(|| State {
            primary: owner.into(),
            ..State::default()
        });
        if key != owner {
            *state.causes.entry(owner.into()).or_default() |= required;
        }
        let mut changed = fresh;
        if !r.member.is_empty() && !r.member.starts_with("Standard") {
            changed |= state
                .spec
                .members
                .insert((r.member.clone(), r.member_name.clone()));
        }
        if !r.predefined.is_empty() && !state.spec.predefined {
            state.spec.predefined = true;
            changed = true;
        }
        if r.member == "StandardCommand" {
            if r.member_name == "CreateBasedOn" && !state.spec.based_on {
                state.spec.based_on = true;
                changed = true;
            }
            if r.member_name == "OpenByValue" && !state.spec.by_value {
                state.spec.by_value = true;
                changed = true;
            }
        }
        if changed {
            self.mark(&key);
        }
        if r.member == "StandardCommand"
            && r.member_name == "Open"
            && matches!(r.kind.as_str(), "Report" | "DataProcessor")
        {
            let xml = Xml::parse(self.descriptor(&key)?.as_bytes())?;
            if let Some(props) = object_element(&xml).and_then(|o| child(&xml, o, "Properties")) {
                for tag in OPEN_PROPERTIES {
                    if let Some(value) = child(&xml, props, tag) {
                        for own in scan(&xml.source[xml.elements[value].inner.clone()]) {
                            if own.key() == key
                                && !own.member.is_empty()
                                && self
                                    .states
                                    .get_mut(&key)
                                    .expect("state exists")
                                    .spec
                                    .members
                                    .insert((own.member, own.member_name))
                            {
                                self.mark(&key);
                            }
                        }
                    }
                }
            }
        }
        Ok(())
    }
    fn listed(&mut self, key: &str, tag: &str) -> Result<Vec<String>> {
        let xml = Xml::parse(self.descriptor(key)?.as_bytes())?;
        let node = object_element(&xml)
            .and_then(|o| child(&xml, o, "Properties"))
            .and_then(|p| child(&xml, p, tag));
        let mut out = vec![];
        if let Some(node) = node {
            for reference in scan(&xml.source[xml.elements[node].inner.clone()]) {
                let key = reference.key();
                if !out.contains(&key) {
                    out.push(key);
                }
            }
        }
        Ok(out)
    }
    fn common_attribute(&mut self, field: &str, key: &str) -> Result<String> {
        if self.common.is_none() {
            let mut common = BTreeMap::new();
            for entry in self.list("CommonAttributes")? {
                let name = entry.name.strip_suffix(".xml").expect("XML");
                let xml = Xml::parse(
                    self.descriptor(&format!("CommonAttribute.{name}"))?
                        .as_bytes(),
                )?;
                let Some(props) = object_element(&xml).and_then(|o| child(&xml, o, "Properties"))
                else {
                    continue;
                };
                let auto = child(&xml, props, "AutoUse")
                    .map(|a| xml.source[xml.elements[a].inner.clone()].to_owned())
                    .unwrap_or_default();
                let mut uses = BTreeMap::new();
                if let Some(content) = child(&xml, props, "Content") {
                    for &item in &xml.elements[content].children {
                        if let (Some(md), Some(use_)) = (
                            child(&xml, item, "xr:Metadata"),
                            child(&xml, item, "xr:Use"),
                        ) {
                            uses.insert(
                                xml.source[xml.elements[md].inner.clone()].to_owned(),
                                xml.source[xml.elements[use_].inner.clone()].to_owned(),
                            );
                        }
                    }
                }
                common
                    .entry(fold(name))
                    .or_insert((name.into(), auto, uses));
            }
            self.common = Some(common);
        }
        let Some((name, auto, uses)) = self
            .common
            .as_ref()
            .and_then(|common| common.get(&fold(field)))
        else {
            return Ok(String::new());
        };
        let usage = uses.get(key).map_or("Auto", String::as_str);
        Ok(if usage == "Use" || usage != "DontUse" && auto == "Use" {
            name.clone()
        } else {
            String::new()
        })
    }
    fn registrars(&mut self, key: &str) -> Result<Vec<String>> {
        if let Some(known) = self.registrars.get(key) {
            return Ok(known.clone());
        }
        if self.source.is_pinned() {
            if !self.registrar_index_checked {
                self.registrar_index = self
                    .index
                    .map(|i| i.registrar_snapshot(None))
                    .transpose()?
                    .flatten();
                self.registrar_index_checked = true;
            }
            if let Some(index) = &self.registrar_index {
                let docs = index.lookup(key)?;
                self.registrars.insert(key.into(), docs.clone());
                return Ok(docs);
            }
        }
        if self.document_proof.is_none() {
            let mut proof = BTreeMap::new();
            for entry in self.list("Documents")? {
                let text = self.read(&format!("Documents/{}", entry.name))?;
                let fragment = records_fragment(text.as_bytes());
                proof.insert(entry.name.clone(), Sha256::digest(fragment).to_vec());
                self.fragments.push((
                    entry.name,
                    String::from_utf8(fragment.to_vec()).map_err(crate::external)?,
                ));
            }
            self.document_proof = Some(proof);
        }
        if !self.registrar_index_checked {
            self.registrar_index = self
                .index
                .map(|i| i.registrar_snapshot(self.document_proof.as_ref()))
                .transpose()?
                .flatten();
            self.registrar_index_checked = true;
        }
        if let Some(index) = &self.registrar_index {
            let docs = index.lookup(key)?;
            self.registrars.insert(key.into(), docs.clone());
            return Ok(docs);
        }
        if self.fallback_registrars.is_none() {
            let mut registrars = BTreeMap::<String, Vec<String>>::new();
            for (file, text) in &self.fragments {
                for reference in scan(text) {
                    registrars.entry(reference.key()).or_default().push(format!(
                        "Document.{}",
                        file.strip_suffix(".xml").expect("XML")
                    ));
                }
            }
            self.fallback_registrars = Some(registrars);
        }
        let docs = self
            .fallback_registrars
            .as_ref()
            .and_then(|m| m.get(key))
            .cloned()
            .unwrap_or_default();
        self.registrars.insert(key.into(), docs.clone());
        Ok(docs)
    }
    fn having(&mut self, candidates: Vec<String>, field: &str) -> Result<BTreeSet<String>> {
        let mut matching = vec![];
        for candidate in candidates {
            let canonical = self.canonical(&candidate)?;
            if self.exists(&canonical)?
                && find_member(&self.facts(&canonical)?.members, field)
                    .1
                    .is_some()
            {
                matching.push(candidate);
            }
        }
        let present: BTreeSet<_> = matching
            .iter()
            .filter(|c| self.selected.contains(*c) || self.states.contains_key(*c))
            .cloned()
            .collect();
        Ok(if present.is_empty() {
            matching.into_iter().take(1).collect()
        } else {
            present
        })
    }
    fn walk(
        &mut self,
        types: &BTreeSet<String>,
        path: &[String],
        i: usize,
        owner: &str,
        depth: usize,
    ) -> Result<()> {
        if i >= path.len() || depth > 8 {
            return Ok(());
        }
        for kind0 in types {
            if kind0 == "@constants" {
                let key = self.canonical(&format!("Constant.{}", path[i]))?;
                if self.exists(&key)? {
                    let (_, name) = key.split_once('.').expect("key");
                    self.note(
                        Reference {
                            kind: "Constant".into(),
                            name: name.into(),
                            ..Reference::default()
                        },
                        owner,
                    )?;
                    let types = self.facts(&key)?.targets;
                    self.walk(&types, path, i + 1, owner, depth + 1)?;
                }
                continue;
            }
            let keys = if kind0.starts_with("DefinedType.") && self.exists(kind0)? {
                self.facts(kind0)?.targets
            } else {
                BTreeSet::from([kind0.clone()])
            };
            for key in keys {
                let key = self.canonical(&key)?;
                if !self.exists(&key)? {
                    continue;
                }
                let (kind, name) = key.split_once('.').expect("key");
                let facts = self.facts(&key)?;
                if path[i] == "@all" {
                    if i == 0 {
                        let mut names: Vec<_> =
                            facts.members.iter().map(|m| m.name.clone()).collect();
                        names.sort();
                        for field in names {
                            self.note(
                                Reference {
                                    kind: kind.into(),
                                    name: name.into(),
                                    member: "*".into(),
                                    member_name: field,
                                    ..Reference::default()
                                },
                                owner,
                            )?;
                        }
                    }
                    continue;
                }
                if path[i] == "@predefined" {
                    if let Some(value) = path.get(i + 1) {
                        self.note(
                            Reference {
                                kind: kind.into(),
                                name: name.into(),
                                predefined: value.clone(),
                                ..Reference::default()
                            },
                            owner,
                        )?;
                    }
                    continue;
                }
                let (mut field, mut member) = find_member(&facts.members, &path[i]);
                if member.is_none() && REGISTER_KINDS.contains(&kind) {
                    (field, member) = register_member(&facts.members, &field);
                }
                self.note(
                    Reference {
                        kind: kind.into(),
                        name: name.into(),
                        member: "*".into(),
                        member_name: field.clone(),
                        ..Reference::default()
                    },
                    owner,
                )?;
                if member.is_none() {
                    let word = fold(&field);
                    if matches!(word.as_str(), "ссылка" | "ref")
                        && matches!(kind, "DocumentJournal" | "FilterCriterion")
                    {
                        if let Some(next) = path.get(i + 1) {
                            let candidates = self.listed(
                                &key,
                                if kind == "DocumentJournal" {
                                    "RegisteredDocuments"
                                } else {
                                    "Content"
                                },
                            )?;
                            let types = self.having(candidates, next)?;
                            self.walk(&types, path, i + 1, owner, depth + 1)?;
                        }
                        continue;
                    }
                    if matches!(word.as_str(), "ссылка" | "ref") {
                        self.walk(
                            &BTreeSet::from([key.clone()]),
                            path,
                            i + 1,
                            owner,
                            depth + 1,
                        )?;
                        continue;
                    }
                    if matches!(kind, "Catalog" | "ChartOfCharacteristicTypes")
                        && matches!(word.as_str(), "владелец" | "owner")
                    {
                        let types = self.listed(&key, "Owners")?.into_iter().collect();
                        self.walk(&types, path, i + 1, owner, depth + 1)?;
                        continue;
                    }
                    if REGISTER_KINDS.contains(&kind)
                        && matches!(word.as_str(), "регистратор" | "recorder")
                    {
                        if let Some(next) = path.get(i + 1) {
                            let candidates = self.registrars(&key)?;
                            let types = self.having(candidates, next)?;
                            self.walk(&types, path, i + 1, owner, depth + 1)?;
                        }
                        continue;
                    }
                    let ca = self.common_attribute(&field, &key)?;
                    if !ca.is_empty() {
                        self.note(
                            Reference {
                                kind: "CommonAttribute".into(),
                                name: ca.clone(),
                                ..Reference::default()
                            },
                            owner,
                        )?;
                        let types = self.facts(&format!("CommonAttribute.{ca}"))?.targets;
                        self.walk(&types, path, i + 1, owner, depth + 1)?;
                    }
                    continue;
                }
                let mut j = i + 1;
                while member.as_ref().is_some_and(|m| m.tag == "TabularSection") && j < path.len() {
                    let (column, next) =
                        find_member(&member.as_ref().expect("member").columns, &path[j]);
                    if next.is_none() && matches!(fold(&column).as_str(), "ссылка" | "ref") {
                        self.walk(
                            &BTreeSet::from([key.clone()]),
                            path,
                            j + 1,
                            owner,
                            depth + 1,
                        )?;
                        member = None;
                        break;
                    }
                    member = next;
                    j += 1;
                }
                if let Some(member) = member
                    && j < path.len()
                {
                    self.walk(&member.types, path, j, owner, depth + 1)?;
                }
            }
        }
        Ok(())
    }
    fn scan_file(&mut self, relative: &str, owner: &str) -> Result<()> {
        if relative.ends_with(".bsl") {
            let text = self.read(relative)?;
            let values = module_values(&text);
            let code = if values.is_empty() {
                text.clone()
            } else {
                without_bsl_comments(&text)
            };
            let actual = if code != text {
                Some(module_values(&code).into_iter().collect::<BTreeSet<_>>())
            } else {
                None
            };
            for (key, path) in values {
                self.comment = actual
                    .as_ref()
                    .is_some_and(|a| !a.contains(&(key.clone(), path.clone())));
                let result = if path.is_empty() {
                    let canonical = self.canonical(&key)?;
                    let (kind, name) = canonical.split_once('.').expect("key");
                    self.note(
                        Reference {
                            kind: kind.into(),
                            name: name.into(),
                            ..Reference::default()
                        },
                        owner,
                    )
                } else {
                    self.walk(&BTreeSet::from([key]), &path, 0, owner, 0)
                };
                self.comment = false;
                result?;
            }
            return Ok(());
        }
        if !relative.ends_with(".xml") {
            let bytes = self.source.read(relative, self.cancel)?;
            self.record_hash(relative, &bytes)?;
            return Ok(());
        }
        let text = self.read(relative)?;
        for reference in scan(&text) {
            self.note(reference, owner)?;
        }
        if relative.rsplit('/').next() == Some("Form.xml") {
            for (types, path) in form_paths(&text)? {
                if path.is_empty() {
                    let mut keys = vec![];
                    for key in types {
                        keys.push(self.canonical(&key)?);
                    }
                    keys.sort();
                    for key in keys {
                        let (kind, name) = key.split_once('.').expect("key");
                        self.note(
                            Reference {
                                kind: kind.into(),
                                name: name.into(),
                                ..Reference::default()
                            },
                            owner,
                        )?;
                    }
                } else {
                    self.walk(&types, &path, 0, owner, 0)?;
                }
            }
        }
        Ok(())
    }
    fn closure(&mut self) -> Result<()> {
        let mut present: BTreeSet<_> = self
            .selected
            .iter()
            .chain(self.states.keys())
            .cloned()
            .collect();
        let mut state_count = self.states.len();
        while let Some(Reverse(key)) = self.heap.pop() {
            self.cancel.check()?;
            self.dirty.remove(&key);
            // Closure only adds states. Rebuild the union when its membership changes.
            if state_count != self.states.len() {
                present = self
                    .selected
                    .iter()
                    .chain(self.states.keys())
                    .cloned()
                    .collect();
                state_count = self.states.len();
            }
            let state = self.states[&key].clone();
            // Only memoise the pure builder. The scans below still depend on the
            // current closure, even when this descriptor's output is unchanged.
            let reusable = self.stub_inputs.get(&key).is_some_and(|(spec, observed)| {
                spec == &state.spec
                    && observed
                        .iter()
                        .all(|(name, value)| present.contains(name) == *value)
            });
            let output = if reusable {
                self.built[&key].output.clone()
            } else {
                #[cfg(test)]
                {
                    *self.stub_builds.entry(key.clone()).or_default() += 1;
                }
                let tracker = TrackedPresence::new(&present);
                let output =
                    stub_descriptor_tracked(&self.descriptor(&key)?, &key, &state.spec, &tracker)?;
                self.stub_inputs
                    .insert(key.clone(), (state.spec.clone(), tracker.observations()));
                output
            };
            let base = format!(
                "{}/",
                descriptor(&key).expect("key").trim_end_matches(".xml")
            );
            let candidates = if self.source.is_pinned() && output.patterns.is_empty() {
                vec![]
            } else {
                self.below(&key)?
            };
            let files = candidates
                .into_iter()
                .filter(|rel| {
                    !KEPT_MODULES.iter().any(|s| rel.ends_with(s))
                        && output.patterns.iter().any(|p| {
                            rel == &format!("{base}{p}")
                                || p.ends_with('/') && rel.starts_with(&format!("{base}{p}"))
                        })
                })
                .collect::<Vec<_>>();
            let text = output.descriptor.clone();
            self.built.insert(
                key.clone(),
                Built {
                    output,
                    files: files.clone(),
                },
            );
            for reference in scan(&text) {
                self.note(reference, &key)?;
            }
            for file in files {
                self.scan_file(&file, &key)?;
            }
        }
        Ok(())
    }
    fn finish(mut self) -> Result<Recipe> {
        // Languages are copied unchanged, but must belong to the frozen preview too.
        let languages: Vec<_> = self
            .source
            .catalog()
            .objects
            .keys()
            .filter(|k| k.kind() == "Language")
            .map(|k| k.0.clone())
            .collect();
        for key in languages {
            for path in self.below(&key)? {
                let bytes = self.source.read(&path, self.cancel)?;
                self.record_hash(&path, &bytes)?;
            }
        }
        for key in self.selected.clone() {
            for relative in self.below(&key)? {
                self.scan_file(&relative, &key)?;
            }
        }
        let mut before = None;
        while before != Some(self.states.len()) {
            before = Some(self.states.len());
            for key in self.states.keys().cloned().collect::<Vec<_>>() {
                self.mark(&key);
            }
            self.closure()?;
            let included: BTreeSet<_> = self
                .selected
                .iter()
                .chain(self.states.keys())
                .cloned()
                .collect();
            for key in &included {
                let kind = key.split('.').next().unwrap_or_default();
                let needs = matches!(
                    kind,
                    "AccumulationRegister" | "AccountingRegister" | "CalculationRegister"
                ) || kind == "InformationRegister"
                    && self
                        .descriptor(key)?
                        .contains("<WriteMode>RecorderSubordinate</WriteMode>");
                if needs {
                    let docs = self.registrars(key)?;
                    if !docs.is_empty()
                        && !docs
                            .iter()
                            .any(|d| self.selected.contains(d) || self.states.contains_key(d))
                    {
                        let (_, name) = docs[0].split_once('.').expect("document");
                        self.note(
                            Reference {
                                kind: "Document".into(),
                                name: name.into(),
                                ..Reference::default()
                            },
                            key,
                        )?;
                    }
                }
            }
        }
        let mut warnings = vec![];
        let included: BTreeSet<_> = self
            .selected
            .iter()
            .chain(self.states.keys())
            .cloned()
            .collect();
        for key in included {
            let relative = descriptor(&key)
                .expect("key")
                .trim_end_matches(".xml")
                .to_owned()
                + "/Ext/Predefined.xml";
            let present = if self.source.is_pinned() {
                self.source.exists(&relative, self.cancel)?
            } else {
                self.below(&key)?.contains(&relative)
            };
            if present {
                let items = crate::package::predefined_codes(
                    &self.descriptor(&key)?,
                    &self.read(&relative)?,
                );
                if !items.is_empty() {
                    warnings.push(serde_json::json!({"object":key,"in_cf":if self.selected.contains(&key){"whole"}else{"stub"},"items":items.into_iter().filter(|s|!s.is_empty()).collect::<Vec<_>>()} ).to_string());
                }
            }
        }
        let mut stubs = BTreeMap::new();
        let mut members = BTreeMap::new();
        for (key, state) in &self.states {
            if !self.source.catalog().objects.contains_key(key.as_str()) {
                return Err(Error::Invalid(format!(
                    "Заглушка не включена в Configuration.xml: {key}"
                )));
            }
            let mut causes = vec![];
            if let Some(&required) = state.causes.get(&state.primary) {
                causes.push(Cause {
                    owner: state.primary.clone(),
                    required,
                });
            }
            for (owner, &required) in &state.causes {
                if owner != &state.primary {
                    causes.push(Cause {
                        owner: owner.clone(),
                        required,
                    });
                }
            }
            if causes.is_empty() {
                return Err(Error::Invalid(format!(
                    "Не подтверждена причина заглушки {key}"
                )));
            }
            stubs.insert(
                key.clone(),
                Stub {
                    key: key.clone(),
                    causes,
                },
            );
            members.insert(
                key.clone(),
                state
                    .spec
                    .members
                    .iter()
                    .map(|(tag, name)| format!("{tag}.{name}"))
                    .collect(),
            );
        }
        if self.source.snapshot(self.cancel)? != self.proof {
            return Err(Error::Invalid(
                "Исходники изменились во время предпросмотра".into(),
            ));
        }
        Ok(Recipe {
            composition: Plan {
                source_fingerprint: self.proof,
                selected: self.selected,
                stubs,
                members,
                source_hashes: self.hashes,
                warnings,
            },
            built: self.built,
            below: self.below,
            listings: self.listings,
            descriptors: self.descriptors,
        })
    }
}
pub(crate) fn resolve(
    source: &dyn MetadataSource,
    index: Option<&dyn FactStore>,
    selected: &BTreeSet<String>,
    excluded: &BTreeSet<String>,
    cancel: &Cancellation,
) -> Result<Recipe> {
    Session::new(source, index, selected, excluded, cancel)?.finish()
}
fn find_member(members: &[Member], field: &str) -> (String, Option<Member>) {
    if let Some(member) = members.iter().find(|m| m.name == field) {
        return (member.name.clone(), Some(member.clone()));
    }
    let lower = fold(field);
    members
        .iter()
        .filter(|m| fold(&m.name) == lower)
        .min_by(|a, b| a.name.cmp(&b.name))
        .map_or_else(
            || (field.into(), None),
            |m| (m.name.clone(), Some(m.clone())),
        )
}
fn register_member(members: &[Member], field: &str) -> (String, Option<Member>) {
    let lower = fold(field);
    let mut tries = if SIDE_SUFFIXES.iter().any(|s| lower.ends_with(&fold(s))) {
        vec![
            lower
                .chars()
                .take(lower.chars().count().saturating_sub(2))
                .collect::<String>(),
        ]
    } else {
        vec![]
    };
    let mut inputs = vec![lower.clone()];
    inputs.extend(tries.clone());
    for value in inputs {
        for suffix in RESOURCE_SUFFIXES {
            let suffix = fold(suffix);
            if value.ends_with(&suffix) && value.chars().count() > suffix.chars().count() {
                tries.push(value[..value.len() - suffix.len()].to_owned());
                break;
            }
        }
    }
    for candidate in tries {
        let name = field
            .chars()
            .take(candidate.chars().count())
            .collect::<String>();
        let (found, member) = find_member(members, &name);
        if member.is_some() {
            return (found, member);
        }
    }
    (field.into(), None)
}
