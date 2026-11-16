//! Deterministic dependency closure. All IO arrives through application ports.
#[cfg(test)]
use crate::stubs::StubOutput;
#[cfg(test)]
#[path = "engine_asset_tests.rs"]
mod asset_tests;
#[cfg(test)]
#[path = "engine_contract_tests.rs"]
mod contract_tests;
#[cfg(test)]
#[path = "engine_memo_tests.rs"]
mod memo_tests;
#[path = "engine_normalize.rs"]
mod normalize;
#[cfg(test)]
#[path = "engine_product_tests.rs"]
mod product_tests;
#[cfg(test)]
#[path = "profile_contract_tests.rs"]
mod profile_contract_tests;
#[cfg(test)]
#[path = "engine_tests.rs"]
pub(crate) mod tests;
use crate::{
    canonical::sha256,
    kinds::{
        KEPT_MODULES, OPEN_PROPERTIES, REGISTER_KINDS, RESOURCE_SUFFIXES, SIDE_SUFFIXES,
        TYPE_FOLDERS,
    },
    source_literals::{module_values, without_bsl_comments},
    source_reader::SourceDocument,
    source_schema::descriptor,
    stubs::StubSpec,
};
use cf_application::{
    Cancellation, DependencyEngine, FactStore, MetadataSource, PreparedPlan, SourceEntry,
};
use cf_domain::{
    Cause, Diagnostic, DiagnosticKind, Error, Facts, Member, Plan, Result, StructuralChoice, Stub,
    fold, source_facts::ReferenceTarget as Reference,
};
use sha2::{Digest, Sha256};
use std::{
    cmp::Reverse,
    collections::{BTreeMap, BTreeSet, BinaryHeap},
    sync::Arc,
};

#[derive(Clone, Debug, Default)]
struct State {
    spec: StubSpec,
    primary: String,
    causes: BTreeMap<String, bool>,
    required: bool,
    reported: BTreeSet<(String, String)>,
}
struct FormFacet {
    paths: Vec<crate::source_forms::FormPath>,
    generation: usize,
}
#[derive(Clone, Default)]
pub(crate) struct DemandCache {
    typed_facts: BTreeMap<String, Arc<cf_domain::source_facts::SourceFacts>>,
    source_documents: BTreeMap<String, Arc<SourceDocument>>,
    facts: BTreeMap<String, Facts>,
    text: BTreeMap<String, String>,
    common: Option<CommonAttributeUsage>,
    registrars: BTreeMap<String, Vec<String>>,
    document_proof: Option<BTreeMap<String, Vec<u8>>>,
    registrar_rows: Vec<(String, Vec<String>)>,
    fallback_registrars: Option<BTreeMap<String, Vec<String>>>,
    names: BTreeMap<String, BTreeMap<String, String>>,
}
#[derive(Clone)]
pub(crate) struct Built {
    pub source: Arc<SourceDocument>,
    pub projection: Arc<crate::projection::DescriptorPlan>,
    pub files: Vec<String>,
}
impl Built {
    #[cfg(test)]
    pub fn output(&self) -> Result<StubOutput> {
        let descriptor = self.source.view.xml.apply(
            self.projection
                .edits
                .iter()
                .map(|edit| crate::xml::Edit {
                    range: edit.start..edit.end,
                    replacement: edit.replacement.clone(),
                })
                .collect(),
        )?;
        Ok(StubOutput {
            descriptor,
            patterns: self.projection.patterns.clone(),
        })
    }
}
#[derive(Clone)]
pub(crate) struct Recipe {
    pub compiled: Option<Arc<cf_domain::package_plan::CompiledPackagePlan>>,
    pub merge_rules: BTreeMap<String, Vec<String>>,
    pub presence: BTreeMap<String, bool>,
    #[cfg(test)]
    projection_visits: usize,
    pub composition: Plan,
    pub built: BTreeMap<String, Built>,
    pub below: BTreeMap<String, Vec<String>>,
    pub listings: BTreeMap<String, Vec<SourceEntry>>,
    pub descriptors: BTreeMap<String, String>,
    pub cache: DemandCache,
    pub excluded: BTreeSet<String>,
}
pub struct RustEngine;
impl Recipe {
    pub(crate) fn frozen_text(&self, path: &str) -> Result<&str> {
        self.cache
            .text
            .get(path)
            .map(String::as_str)
            .ok_or_else(|| Error::Invalid(format!("Не зафиксирован исходный текст {path}")))
    }
    pub(crate) fn frozen_document(&self, key: &str) -> Option<&SourceDocument> {
        self.cache.source_documents.get(key).map(Arc::as_ref)
    }
}
impl DependencyEngine for RustEngine {
    fn preview(
        &self,
        source: &dyn MetadataSource,
        facts: Option<&dyn FactStore>,
        selected: &BTreeSet<String>,
        excluded: &BTreeSet<String>,
        cancel: &Cancellation,
    ) -> Result<Box<dyn PreparedPlan>> {
        Ok(Box::new(prepare(
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
    observed_presence: BTreeMap<String, bool>,
    typed_facts: BTreeMap<String, Arc<cf_domain::source_facts::SourceFacts>>,
    source_documents: BTreeMap<String, Arc<SourceDocument>>,
    source: &'a dyn MetadataSource,
    index: Option<&'a dyn FactStore>,
    cancel: &'a Cancellation,
    selected: BTreeSet<String>,
    excluded: BTreeSet<String>,
    excluded_seen: BTreeSet<String>,
    proof: String,
    states: BTreeMap<String, State>,
    dirty: BTreeSet<String>,
    structural_pending: BTreeSet<String>,
    registrar_pending: BTreeSet<String>,
    presence_observers: BTreeMap<String, BTreeSet<String>>,
    present: BTreeSet<String>,
    presence_pending: BTreeSet<String>,
    heap: BinaryHeap<Reverse<String>>,
    descriptors: BTreeMap<String, String>,
    stub_inputs: BTreeMap<String, (StubSpec, BTreeMap<String, bool>)>,
    #[cfg(test)]
    stub_builds: BTreeMap<String, usize>,
    #[cfg(test)]
    projection_visits: usize,
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
    registrar_rows: Vec<(String, Vec<String>)>,
    fallback_registrars: Option<BTreeMap<String, Vec<String>>>,
    registrar_index: Option<Box<dyn cf_application::RegistrarLookup + 'a>>,
    registrar_index_checked: bool,
    requirements: crate::requirements::Requirements,
    diagnostics: BTreeSet<Diagnostic>,
    choices: BTreeSet<StructuralChoice>,
    text_cache: BTreeMap<String, String>,
    cached_reads: bool,
    scanned: BTreeSet<(String, String, String)>,
    source_address: String,
    timer: std::time::Instant,
    setup_ms: f64,
    kernel_ms: f64,
    generation: usize,
    form_facets: BTreeMap<(String, String), FormFacet>,
    source_fragment: Option<(String, String)>,
}
impl<'a> Session<'a> {
    fn new(
        source: &'a dyn MetadataSource,
        index: Option<&'a dyn FactStore>,
        selected: &BTreeSet<String>,
        excluded: &BTreeSet<String>,
        cancel: &'a Cancellation,
    ) -> Result<Self> {
        let started = std::time::Instant::now();
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
            excluded_seen: BTreeSet::new(),
            proof: source.snapshot(cancel)?,
            states: BTreeMap::new(),
            dirty: BTreeSet::new(),
            structural_pending: BTreeSet::new(),
            registrar_pending: BTreeSet::new(),
            presence_observers: BTreeMap::new(),
            present: BTreeSet::new(),
            presence_pending: selected.clone(),
            heap: BinaryHeap::new(),
            descriptors: BTreeMap::new(),
            stub_inputs: BTreeMap::new(),
            #[cfg(test)]
            stub_builds: BTreeMap::new(),
            #[cfg(test)]
            projection_visits: 0,
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
            registrar_rows: vec![],
            typed_facts: BTreeMap::new(),
            source_documents: BTreeMap::new(),
            observed_presence: BTreeMap::new(),
            fallback_registrars: None,
            requirements: crate::requirements::Requirements::new(selected)?,
            diagnostics: BTreeSet::new(),
            choices: BTreeSet::new(),
            text_cache: BTreeMap::new(),
            cached_reads: false,
            scanned: BTreeSet::new(),
            source_address: String::new(),
            timer: started,
            kernel_ms: 0.,
            setup_ms: started.elapsed().as_secs_f64() * 1000.,
            generation: 0,
            form_facets: BTreeMap::new(),
            source_fragment: None,
        })
    }
    fn read(&mut self, relative: &str) -> Result<String> {
        self.read_text(relative, true)
    }
    fn read_text(&mut self, relative: &str, retain: bool) -> Result<String> {
        if self.cached_reads
            && let Some(text) = self.text_cache.get(relative)
        {
            return Ok(text.clone());
        }
        let data = self.source.read(relative, self.cancel)?;
        self.record_hash(relative, &data)?;
        let text = String::from_utf8(data).map_err(crate::external)?;
        if retain {
            self.text_cache.insert(relative.into(), text.clone());
        }
        Ok(text)
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
            && let Some(facts) = index.source_facts(key, None)?
        {
            let structural = facts.structural_facts()?;
            self.typed_facts.insert(key.into(), Arc::new(facts));
            self.facts.insert(key.into(), structural.clone());
            return Ok(structural);
        }
        let facts = self.document(key)?.structural_facts()?;
        self.facts.insert(key.into(), facts.clone());
        Ok(facts)
    }
    fn document(&mut self, key: &str) -> Result<Arc<SourceDocument>> {
        if let Some(document) = self.source_documents.get(key) {
            return Ok(Arc::clone(document));
        }
        let text = self.descriptor(key)?;
        let path =
            descriptor(key).ok_or_else(|| Error::Invalid(format!("Нет descriptor для {key}")))?;
        let proof = cf_domain::source_facts::SourceProof {
            identity: self.source.identity().into(),
            snapshot: self.proof.clone(),
            inputs: BTreeMap::new(),
        };
        let cached = if let Some(facts) = self.typed_facts.get(key) {
            Some(Arc::clone(facts))
        } else if let Some(index) = self.index {
            index
                .source_facts(key, Some(&Sha256::digest(text.as_bytes())))?
                .map(Arc::new)
        } else {
            None
        };
        let document = Arc::new(if let Some(facts) = cached {
            SourceDocument::cached(key, &path, text.as_bytes(), proof, facts.as_ref().clone())?
        } else {
            SourceDocument::descriptor(key, &path, text.as_bytes(), proof)?
        });
        self.source_documents
            .insert(key.into(), Arc::clone(&document));
        Ok(document)
    }
    fn mark(&mut self, key: &str) {
        self.structural_pending.insert(key.into());
        if self.dirty.insert(key.into()) {
            self.heap.push(Reverse(key.into()));
        }
    }
    fn note(&mut self, r: Reference, owner: &str) -> Result<()> {
        self.require(r, owner, true)
    }
    fn source_reference(
        &mut self,
        fact: &cf_domain::source_facts::ReferenceFact,
        owner: &str,
    ) -> Result<()> {
        use cf_domain::source_facts::{MissingPolicy, Necessity, ReferenceSurface};
        let mut target = fact.target.clone();
        if fact.missing == MissingPolicy::Diagnose
            || matches!(fact.surface, ReferenceSurface::ContextualAttribute(_))
        {
            let key = self.canonical(&target.key())?;
            if fact.missing == MissingPolicy::Diagnose
                && (!self.source.catalog().objects.contains_key(key.as_str())
                    || !self.exists(&key)?)
            {
                self.diagnostic(DiagnosticKind::MissingObject, owner, &key);
                return Ok(());
            }
            let (kind, name) = key.split_once('.').expect("metadata target");
            target.kind = kind.into();
            target.name = name.into();
        }
        let label = format!("$source:{}", fact.fragment);
        self.source_fragment = Some((owner.into(), label.clone()));
        self.requirements.reference(owner, owner, Some(&label))?;
        let previous = self.comment;
        self.comment = fact.necessity == Necessity::Optional;
        let result = self.note(
            Reference {
                kind: target.kind.clone(),
                name: target.name.clone(),
                member: target.member.clone(),
                member_name: target.member_name.clone(),
                predefined: target.predefined.clone(),
            },
            owner,
        );
        self.comment = previous;
        result?;
        if fact.missing == MissingPolicy::Diagnose
            && !target.member.is_empty()
            && !target.member.starts_with("Standard")
            && target.member != "*"
        {
            let key = target.key();
            let found = if matches!(target.member.as_str(), "Form" | "Template") {
                self.document(&key)?
                    .facts
                    .fragments
                    .values()
                    .any(|fragment| {
                        fragment.tag.local == target.member && fragment.name == target.member_name
                    })
            } else {
                self.facts(&key)?
                    .members
                    .iter()
                    .any(|member| member.tag == target.member && member.name == target.member_name)
            };
            if !found {
                self.diagnostic(
                    DiagnosticKind::MissingMember,
                    owner,
                    &format!("{key}.{}.{}", target.member, target.member_name),
                );
            }
        }
        Ok(())
    }
    fn require(&mut self, r: Reference, owner: &str, report: bool) -> Result<()> {
        self.cancel.check()?;
        let key = r.key();
        if self.selected.contains(&key)
            || r.kind == "Language"
            || !self.source.catalog().objects.contains_key(key.as_str())
        {
            return Ok(());
        }
        let required = !self.comment;
        if !self.exists(&key)? {
            if required {
                self.diagnostic(DiagnosticKind::MissingObject, owner, &key);
            }
            return Ok(());
        }
        if !required && self.excluded.contains(&key) {
            self.excluded_seen.insert(key);
            return Ok(());
        }
        if !self.states.contains_key(&key) {
            self.presence_pending.insert(key.clone());
        }
        let state = self.states.entry(key.clone()).or_insert_with(|| State {
            primary: owner.into(),
            spec: StubSpec {
                finalized: true,
                ..StubSpec::default()
            },
            ..State::default()
        });
        let became_required = key != owner && required && !state.required;
        if key != owner {
            *state.causes.entry(owner.into()).or_default() |= required;
            state.required |= required;
        }
        if !r.member.is_empty() && !r.member.starts_with("Standard") && report {
            state
                .reported
                .insert((r.member.clone(), r.member_name.clone()));
        }
        if !r.member.is_empty() && !r.member.starts_with("Standard") {
            let origin = self
                .source_fragment
                .as_ref()
                .filter(|(key, _)| report && key == owner)
                .map_or("$identity", |(_, label)| label.as_str());
            self.requirements.reference_from(
                owner,
                origin,
                &key,
                Some(&format!("{}|{}", r.member, r.member_name)),
            )?;
        } else {
            let origin = self
                .source_fragment
                .as_ref()
                .filter(|(key, _)| report && key == owner)
                .map_or("$identity", |(_, label)| label.as_str());
            self.requirements
                .reference_from(owner, origin, &key, None)?;
        }
        if !r.predefined.is_empty() {
            self.requirements
                .reference(owner, &key, Some("$predefined"))?;
        }
        if r.member == "StandardCommand" {
            if r.member_name == "CreateBasedOn" {
                self.requirements
                    .reference(owner, &key, Some("$based_on"))?;
            }
            if r.member_name == "OpenByValue" {
                self.requirements
                    .reference(owner, &key, Some("$by_value"))?;
            }
        }
        if r.member == "StandardCommand"
            && r.member_name == "Open"
            && matches!(r.kind.as_str(), "Report" | "DataProcessor")
        {
            let document = self.document(&key)?;
            if let Some(props) = document.view.child(document.object, "Properties") {
                for tag in OPEN_PROPERTIES {
                    if let Some(value) = document.view.child(props, tag) {
                        for own in document.references_in(value).map(|r| &r.target) {
                            if own.key() == key && !own.member.is_empty() {
                                let declaration =
                                    document.facts.fragments.values().find(|fragment| {
                                        fragment.kind
                                            == cf_domain::source_facts::FragmentKind::Declaration
                                            && fragment.tag.local == own.member
                                            && fold(&fragment.name) == fold(&own.member_name)
                                    });
                                if matches!(own.member.as_str(), "Form" | "Template")
                                    && declaration.is_none()
                                {
                                    self.diagnostic(
                                        DiagnosticKind::MissingMember,
                                        owner,
                                        &format!("{key}.{}.{}", own.member, own.member_name),
                                    );
                                }
                                let member_name = declaration
                                    .map_or(own.member_name.as_str(), |d| d.name.as_str());
                                self.requirements.reference(
                                    owner,
                                    &key,
                                    Some(&format!("{}|{}", own.member, member_name)),
                                )?;
                            }
                        }
                    }
                }
            }
        }
        if became_required {
            // The keep may already be stable, but required-file checks must run
            // again when an optional cause is upgraded to a required cause.
            self.mark(&key);
        }
        Ok(())
    }
    fn activate(&mut self) -> Result<bool> {
        let t = std::time::Instant::now();
        let delta = self.requirements.close(self.cancel)?;
        let objects = delta.objects;
        let keeps = delta.fragments;
        if !objects.is_empty() {
            self.generation += 1;
        }
        self.kernel_ms += t.elapsed().as_secs_f64() * 1000.;
        let changed = !objects.is_empty() || !keeps.is_empty();
        for key in objects {
            self.registrar_pending.insert(key.clone());
            if let Some(observers) = self.presence_observers.get(&key).cloned() {
                for observer in observers {
                    self.mark(&observer);
                }
            }
            if self.states.contains_key(&key) {
                self.mark(&key);
            }
        }
        for (key, label) in keeps {
            if let Some(state) = self.states.get_mut(&key) {
                let dirty = match label.as_str() {
                    _ if label.starts_with("$guard:") => false,
                    "$predefined" => {
                        let old = state.spec.predefined;
                        state.spec.predefined = true;
                        !old
                    }
                    "$based_on" => {
                        let old = state.spec.based_on;
                        state.spec.based_on = true;
                        !old
                    }
                    "$by_value" => {
                        let old = state.spec.by_value;
                        state.spec.by_value = true;
                        !old
                    }
                    _ => {
                        if let Some((tag, name)) = label.split_once('|') {
                            if let Some(prop) = tag.strip_prefix("$property:") {
                                let index = name.parse::<usize>().map_err(crate::external)?;
                                state
                                    .spec
                                    .property_items
                                    .entry(prop.into())
                                    .or_default()
                                    .insert(index)
                            } else {
                                state.spec.members.insert((tag.into(), name.into()))
                            }
                        } else {
                            false
                        }
                    }
                };
                if dirty {
                    self.mark(&key);
                }
            }
        }
        Ok(changed)
    }
    fn take_present(&mut self) -> BTreeSet<String> {
        let mut present = std::mem::take(&mut self.present);
        self.extend_present(&mut present);
        present
    }
    fn extend_present(&mut self, present: &mut BTreeSet<String>) {
        for key in std::mem::take(&mut self.presence_pending) {
            present.insert(key);
        }
    }
    fn diagnostic(&mut self, kind: DiagnosticKind, owner: &str, target: &str) {
        self.diagnostics.insert(Diagnostic {
            kind,
            owner: owner.into(),
            source: self.source_address.clone(),
            target: target.into(),
        });
    }
    fn listed(&mut self, key: &str, tag: &str) -> Result<Vec<String>> {
        let document = self.document(key)?;
        let node = document
            .view
            .child(document.object, "Properties")
            .and_then(|p| document.view.child(p, tag));
        let mut out = vec![];
        if let Some(node) = node {
            for reference in document.ordered_candidates(node) {
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
                let document = self.document(&format!("CommonAttribute.{name}"))?;
                let view = &document.view;
                let xml = &view.xml;
                let Some(props) = view.child(document.object, "Properties") else {
                    continue;
                };
                let auto = view.text_value(props, "AutoUse")?;
                let mut uses = BTreeMap::new();
                if let Some(content) = view.child(props, "Content") {
                    for &item in &xml.elements[content].children {
                        let readable = |local: &str| {
                            xml.elements[item].children.iter().copied().find(|&n| {
                                view.names[n].local == local
                                    && matches!(
                                        view.names[n].uri.as_str(),
                                        crate::source_xml::READABLE | ""
                                    )
                            })
                        };
                        if let (Some(md), Some(use_)) = (readable("Metadata"), readable("Use")) {
                            uses.insert(xml.text(md)?.trim().into(), xml.text(use_)?.trim().into());
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
                    .map(|i| i.source_registrar_snapshot(None))
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
                // Keep compact links and extraction proof, not unrelated raw XML.
                let text = self.read_text(&format!("Documents/{}", entry.name), false)?;
                let facts = crate::registrar_facts::extract(text.as_bytes())?;
                proof.insert(entry.name.clone(), facts.hash);
                self.registrar_rows.push((
                    entry.name,
                    facts.links.into_iter().map(|(_, target)| target).collect(),
                ));
            }
            self.document_proof = Some(proof);
        }
        if !self.registrar_index_checked {
            self.registrar_index = self
                .index
                .map(|i| i.source_registrar_snapshot(self.document_proof.as_ref()))
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
            for (file, targets) in &self.registrar_rows {
                for target in targets {
                    registrars.entry(target.clone()).or_default().push(format!(
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
                matching.push(canonical);
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
        _depth: usize,
    ) -> Result<()> {
        let mut pending = vec![(types.clone(), i)];
        let mut visited = BTreeSet::new();
        while let Some((types, i)) = pending.pop() {
            if i >= path.len() {
                continue;
            }
            for kind0 in &types {
                self.cancel.check()?;
                if !visited.insert((kind0.clone(), i)) {
                    continue;
                }
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
                        pending.push((types, i + 1));
                    }
                    continue;
                }
                if kind0.starts_with("DefinedType.") && self.exists(kind0)? {
                    pending.push((self.facts(kind0)?.targets, i));
                    continue;
                }
                let keys = BTreeSet::from([kind0.clone()]);
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
                                pending.push((types, i + 1));
                            }
                            continue;
                        }
                        if matches!(word.as_str(), "ссылка" | "ref") {
                            pending.push((BTreeSet::from([key.clone()]), i + 1));
                            continue;
                        }
                        if matches!(kind, "Catalog" | "ChartOfCharacteristicTypes")
                            && matches!(word.as_str(), "владелец" | "owner")
                        {
                            let types = self.listed(&key, "Owners")?.into_iter().collect();
                            pending.push((types, i + 1));
                            continue;
                        }
                        if REGISTER_KINDS.contains(&kind)
                            && matches!(word.as_str(), "регистратор" | "recorder")
                        {
                            if let Some(next) = path.get(i + 1) {
                                let candidates = self.registrars(&key)?;
                                let types = self.having(candidates, next)?;
                                pending.push((types, i + 1));
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
                            pending.push((types, i + 1));
                        }
                        continue;
                    }
                    let mut j = i + 1;
                    while member.as_ref().is_some_and(|m| m.tag == "TabularSection")
                        && j < path.len()
                    {
                        let (column, next) =
                            find_member(&member.as_ref().expect("member").columns, &path[j]);
                        if next.is_none() && matches!(fold(&column).as_str(), "ссылка" | "ref")
                        {
                            pending.push((BTreeSet::from([key.clone()]), j + 1));
                            member = None;
                            break;
                        }
                        member = next;
                        j += 1;
                    }
                    if let Some(member) = member
                        && j < path.len()
                    {
                        pending.push((member.types, j));
                    }
                }
            }
        }
        Ok(())
    }
    fn scan_file(&mut self, relative: &str, owner: &str) -> Result<()> {
        self.source_address = relative.into();
        self.source_fragment = Some((owner.into(), format!("$file:{relative}")));
        self.requirements
            .reference(owner, owner, Some(&format!("$file:{relative}")))?;
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
        let first = self
            .scanned
            .insert((owner.into(), relative.into(), sha256(text.as_bytes())));
        let mut form_paths = None;
        if first {
            if descriptor(owner).as_deref() == Some(relative) {
                self.descriptors
                    .entry(owner.into())
                    .or_insert_with(|| text.clone());
                let document = self.document(owner)?;
                for reference in &document.facts.references {
                    self.source_reference(reference, owner)?;
                }
            } else {
                let proof = cf_domain::source_facts::SourceProof {
                    identity: self.source.identity().into(),
                    snapshot: self.proof.clone(),
                    inputs: BTreeMap::new(),
                };
                let document = SourceDocument::sidecar(owner, relative, text.as_bytes(), proof)?;
                if relative.rsplit('/').next() == Some("Form.xml") {
                    let form = crate::source_forms::read(&document)?;
                    for reference in &form.references {
                        self.source_reference(reference, owner)?;
                    }
                    form_paths = Some(form.paths);
                }
                for reference in &document.facts.references {
                    self.source_reference(reference, owner)?;
                }
            }
        }
        if relative.rsplit('/').next() == Some("Form.xml") {
            let key = (owner.into(), relative.into());
            let paths = if first {
                form_paths.ok_or_else(|| {
                    Error::Invalid(format!("Не прочитаны facets формы: {relative}"))
                })?
            } else {
                self.form_facets
                    .get(&key)
                    .map(|f| f.paths.clone())
                    .ok_or_else(|| Error::Invalid(format!("Потеряны facets формы: {relative}")))?
            };
            self.form_facets.insert(
                key,
                FormFacet {
                    paths: paths.clone(),
                    generation: self.generation,
                },
            );
            self.form_requirements(paths, owner)?;
        }
        Ok(())
    }
    fn form_requirements(
        &mut self,
        paths: Vec<crate::source_forms::FormPath>,
        owner: &str,
    ) -> Result<()> {
        for (types, path) in paths {
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
        Ok(())
    }
    fn replay_forms(&mut self) -> Result<bool> {
        let pending: Vec<_> = self
            .form_facets
            .iter()
            .filter(|((owner, _), f)| {
                f.paths.iter().any(|(_, path)| !path.is_empty())
                    && f.generation != self.generation
                    && (self.selected.contains(owner) || self.requirements.active(owner))
            })
            .map(|((owner, path), f)| (owner.clone(), path.clone(), f.paths.clone()))
            .collect();
        let changed = !pending.is_empty();
        for (owner, path, paths) in pending {
            self.cancel.check()?;
            self.form_facets
                .get_mut(&(owner.clone(), path.clone()))
                .expect("facet")
                .generation = self.generation;
            self.source_address = path.clone();
            self.source_fragment = Some((owner.clone(), format!("$file:{path}")));
            self.form_requirements(paths, &owner)?;
        }
        Ok(changed)
    }
    fn closure(&mut self) -> Result<()> {
        self.activate()?;
        let mut present = self.take_present();
        loop {
            self.activate()?;
            self.extend_present(&mut present);
            let Some(Reverse(key)) = self.heap.pop() else {
                if self.replay_forms()? {
                    continue;
                }
                break;
            };
            self.cancel.check()?;
            self.dirty.remove(&key);
            if !self.requirements.active(&key) {
                continue;
            }
            if self.normalise_projection(&key, &present, false)? {
                self.mark(&key);
                continue;
            }
            let spec = self.states[&key].spec.clone();
            let source = self.document(&key)?;
            let reusable = self
                .stub_inputs
                .get(&key)
                .is_some_and(|(previous, observed)| {
                    previous == &spec
                        && observed
                            .iter()
                            .all(|(target, value)| present.contains(target) == *value)
                });
            let projection = if reusable {
                Arc::clone(&self.built[&key].projection)
            } else {
                #[cfg(test)]
                {
                    *self.stub_builds.entry(key.clone()).or_default() += 1;
                }
                let projection =
                    Arc::new(crate::projection::compile(&source, &key, &spec, &present)?);
                for target in projection.observations.keys() {
                    self.presence_observers
                        .entry(target.clone())
                        .or_default()
                        .insert(key.clone());
                }
                self.stub_inputs
                    .insert(key.clone(), (spec, projection.observations.clone()));
                projection
            };
            let descriptor_path = descriptor(&key).expect("metadata key");
            let base = format!("{}/", descriptor_path.trim_end_matches(".xml"));
            let full = crate::kinds::full_resource(key.split('.').next().unwrap_or_default());
            let candidates = if self.source.is_pinned() && projection.patterns.is_empty() && !full {
                vec![]
            } else {
                self.below(&key)?
            };
            let files: Vec<_> = candidates
                .into_iter()
                .filter(|relative| {
                    if full {
                        return relative != &descriptor_path;
                    }
                    !KEPT_MODULES.iter().any(|suffix| relative.ends_with(suffix))
                        && projection.patterns.iter().any(|pattern| {
                            relative == &format!("{base}{pattern}")
                                || pattern.ends_with('/')
                                    && relative.starts_with(&format!("{base}{pattern}"))
                        })
                })
                .collect();
            self.built.insert(
                key.clone(),
                Built {
                    source,
                    projection: Arc::clone(&projection),
                    files: files.clone(),
                },
            );
            self.source_address = descriptor_path;
            // The only dependency input is the retained source fact set. No XML
            // has been emitted, and no writer result can create a requirement.
            for reference in &projection.references {
                self.source_reference(reference, &key)?;
            }
            for file in files {
                self.scan_file(&file, &key)?;
            }
        }
        self.present = present;
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
        loop {
            self.closure()?;
            if !self.choose_structural()? && !self.choose_registrar()? {
                break;
            }
        }
        let mut merge_rules = BTreeMap::new();
        for (key, built) in self.built.clone() {
            let kind = key.split_once('.').expect("metadata key").0;
            let base = format!(
                "{}/",
                descriptor(&key)
                    .expect("metadata key")
                    .trim_end_matches(".xml")
            );
            let mut protected = vec![];
            // The complete owned-file inventory is already a frozen input.
            // Exact inventory entries prove presence. An absent exact spelling
            // still requires the provider lookup (Windows paths ignore case).
            let inventory: BTreeSet<_> = self.below(&key)?.into_iter().collect();
            for module in crate::kinds::STUB_MODULES {
                let path = format!("{base}Ext/{module}.bsl");
                if self.observe_module(&path, &inventory)? {
                    protected.push((*module).into());
                }
            }
            if kind == "FunctionalOption" {
                let source_content = built
                    .source
                    .view
                    .child(built.source.object, "Properties")
                    .and_then(|p| built.source.view.child(p, "Content"))
                    .map(|node| crate::projection::semantic_value(&built.source.view, node))
                    .transpose()?;
                let expected_content =
                    crate::projection::top_property(&built.projection.expected, "Content").cloned();
                if source_content != expected_content {
                    protected.push("Content".into());
                }
            }
            if !protected.is_empty() {
                merge_rules.insert(key.clone(), protected);
            }
            for pattern in &built.projection.patterns {
                let command_path = format!("{base}{pattern}Ext/CommandModule.bsl");
                let form_path = format!(
                    "{base}{}/Ext/Form/Module.bsl",
                    pattern.trim_end_matches(".xml")
                );
                if pattern.starts_with("Commands/")
                    && self.observe_module(&command_path, &inventory)?
                {
                    merge_rules.insert(
                        format!("{key}.Command.{}", &pattern[9..pattern.len() - 1]),
                        vec!["CommandModule".into()],
                    );
                } else if pattern.starts_with("Forms/")
                    && pattern.ends_with(".xml")
                    && self.observe_module(&form_path, &inventory)?
                {
                    merge_rules.insert(
                        format!("{key}.Form.{}", &pattern[6..pattern.len() - 4]),
                        vec!["Form".into()],
                    );
                }
            }
        }
        // Root configuration is also an input to materialization/service identity.
        self.read("Configuration.xml")?;
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
                let items = crate::predefined::codes(
                    self.descriptor(&key)?.as_bytes(),
                    self.read(&relative)?.as_bytes(),
                )?;
                if !items.is_empty() {
                    warnings.push(serde_json::json!({"object":key,"in_cf":if self.selected.contains(&key){"whole"}else{"stub"},"items":items.into_iter().filter(|s|!s.is_empty()).collect::<Vec<_>>()} ).to_string());
                }
            }
        }
        let mut stubs = BTreeMap::new();
        let mut members = BTreeMap::new();
        for (key, state) in &self.states {
            if !self.requirements.active(key) {
                return Err(Error::Invalid(format!(
                    "Внутренняя ошибка: объект не выведен ядром: {key}"
                )));
            }
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
                    .reported
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
        for key in &self.excluded {
            if self
                .states
                .get(key)
                .is_some_and(|s| s.causes.values().any(|&r| r))
            {
                return Err(Error::Invalid(format!(
                    "Обязательную заглушку нельзя исключить: {key}"
                )));
            }
            if !self.excluded_seen.contains(key) {
                return Err(Error::Invalid(format!(
                    "Заглушка отсутствует в плане: {key}"
                )));
            }
        }
        let t = std::time::Instant::now();
        let reasons = self.requirements.reasons();
        let provenance_ms = t.elapsed().as_secs_f64() * 1000.;
        let total_ms = self.timer.elapsed().as_secs_f64() * 1000.;
        let (objects_seen, fragments_seen, relation_rows) = self.requirements.counts();
        let metrics = cf_domain::PlanMetrics {
            setup_ms: self.setup_ms,
            kernel_ms: self.kernel_ms,
            provenance_ms,
            total_ms,
            adapter_ms: (total_ms - self.setup_ms - self.kernel_ms - provenance_ms).max(0.),
            objects_seen,
            fragments_seen,
            relation_rows,
            closure_rounds: self.requirements.rounds,
        };
        let cache = DemandCache {
            typed_facts: self.typed_facts,
            source_documents: self.source_documents,
            facts: self.facts,
            text: self.text_cache,
            common: self.common,
            registrars: self.registrars,
            document_proof: self.document_proof,
            registrar_rows: self.registrar_rows,
            fallback_registrars: self.fallback_registrars,
            names: self.names,
        };
        let source = self.source;
        let mut recipe = Recipe {
            compiled: None,
            merge_rules,
            presence: self.observed_presence,
            #[cfg(test)]
            projection_visits: self.projection_visits,
            composition: Plan {
                source_fingerprint: self.proof,
                selected: self.selected,
                stubs,
                members,
                source_hashes: self.hashes,
                warnings,
                diagnostics: self.diagnostics.into_iter().collect(),
                choices: self.choices.into_iter().collect(),
                reasons,
                metrics,
                reader_profile: cf_domain::ReaderProfile::CfInputV1,
            },
            built: self.built,
            below: self.below,
            listings: self.listings,
            descriptors: self.descriptors,
            cache,
            excluded: self.excluded,
        };
        if recipe.composition.diagnostics.is_empty() {
            recipe.compiled = Some(Arc::new(crate::package_compiler::compile(&recipe, source)?));
        }
        Ok(recipe)
    }
    fn observe_module(&mut self, path: &str, inventory: &BTreeSet<String>) -> Result<bool> {
        self.cancel.check()?;
        let present = inventory.contains(path) || self.source.exists(path, self.cancel)?;
        if self
            .observed_presence
            .get(path)
            .is_some_and(|old| *old != present)
        {
            return Err(Error::Invalid(format!(
                "Изменилось наличие исходного файла {path}"
            )));
        }
        self.observed_presence.insert(path.into(), present);
        if !present {
            return Ok(false);
        }
        let value = self.read_text(path, false)?;
        Ok(!value
            .trim_start_matches('\u{feff}')
            .trim_matches([' ', '\t', '\r', '\n', '\x0b', '\x0c'])
            .is_empty())
    }
}
pub(crate) fn prepare(
    source: &dyn MetadataSource,
    index: Option<&dyn FactStore>,
    selected: &BTreeSet<String>,
    excluded: &BTreeSet<String>,
    cancel: &Cancellation,
) -> Result<Recipe> {
    Session::new(source, index, selected, excluded, cancel)?.finish()
}
pub(crate) fn reprepare(
    source: &dyn MetadataSource,
    previous: &Recipe,
    excluded: &BTreeSet<String>,
    cancel: &Cancellation,
) -> Result<Recipe> {
    let mut query = Session::new(
        source,
        None,
        &previous.composition.selected,
        excluded,
        cancel,
    )?;
    query.facts = previous.cache.facts.clone();
    query.typed_facts = previous.cache.typed_facts.clone();
    query.source_documents = previous.cache.source_documents.clone();
    query.cached_reads = true;
    query.text_cache = previous.cache.text.clone();
    query.descriptors = previous.descriptors.clone();
    query.below = previous.below.clone();
    query.listings = previous.listings.clone();
    query.common = previous.cache.common.clone();
    query.registrars = previous.cache.registrars.clone();
    query.document_proof = previous.cache.document_proof.clone();
    query.registrar_rows = previous.cache.registrar_rows.clone();
    query.fallback_registrars = previous.cache.fallback_registrars.clone();
    query.names = previous.cache.names.clone();
    query.hashes = previous.composition.source_hashes.clone();
    query.finish()
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
