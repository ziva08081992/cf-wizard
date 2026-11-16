//! Source observations are independent of selection, projection and merge policy.
use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

/// Payload shape, extraction semantics and consuming rules have distinct owners.
pub const FACT_SCHEMA: u32 = 2;
pub const RULE_PROFILE: &str = "cf-rules-3";

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Facet {
    Metadata,
    References,
    Forms,
    Query,
    Predefined,
    Assets,
    Registrars,
    Observations,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct FactVersions {
    pub schema: u32,
    pub readers: BTreeMap<Facet, String>,
}
impl FactVersions {
    pub fn current() -> Self {
        use Facet::*;
        Self {
            schema: FACT_SCHEMA,
            readers: [
                Metadata,
                References,
                Forms,
                Query,
                Predefined,
                Assets,
                Registrars,
                Observations,
            ]
            .into_iter()
            .map(|facet| {
                (
                    facet,
                    match facet {
                        References => "source-v2.5",
                        Forms | Query => "source-v2.4",
                        Metadata => "source-v2.3",
                        Registrars => "source-v2.2",
                        _ => "source-v2.1",
                    }
                    .into(),
                )
            })
            .collect(),
        }
    }
    pub fn compatible(&self, required: &Self, facet: Facet) -> bool {
        self.schema == required.schema
            && required.readers.contains_key(&facet)
            && self.readers.get(&facet) == required.readers.get(&facet)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
pub struct ExpandedName {
    pub uri: String,
    pub local: String,
}
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
pub struct NodeStep {
    pub name: ExpandedName,
    /// Position among siblings with the same expanded name, not a global index.
    pub ordinal: usize,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Origin {
    pub path: String,
    pub sha256: String,
    pub node: Vec<NodeStep>,
    /// UTF-8 byte positions are valid only for the proved file bytes.
    pub span: Option<(usize, usize)>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DirectoryEntry {
    pub name: String,
    pub directory: bool,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum InputObservation {
    Content { sha256: String },
    Absent,
    Directory { entries: Vec<DirectoryEntry> },
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SourceProof {
    pub identity: String,
    pub snapshot: String,
    pub inputs: BTreeMap<String, InputObservation>,
}
impl SourceProof {
    pub fn observe(&mut self, path: String, observation: InputObservation) -> Result<()> {
        if self
            .inputs
            .get(&path)
            .is_some_and(|old| old != &observation)
        {
            return Err(Error::Invalid(format!(
                "Источник изменился во время чтения: {path}"
            )));
        }
        self.inputs.insert(path, observation);
        Ok(())
    }
    pub fn digest(&self) -> Result<String> {
        let bytes = serde_json::to_vec(self).map_err(|e| Error::Invalid(e.to_string()))?;
        Ok(hex::encode(Sha256::digest(bytes)))
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FragmentKind {
    Object,
    Field,
    Column,
    Declaration,
    Property,
    PropertyItem,
    Type,
    File,
    Link,
    Value,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SourceFragment {
    pub id: String,
    pub owner: String,
    pub parent: Option<String>,
    pub kind: FragmentKind,
    pub tag: ExpandedName,
    pub name: String,
    pub ordinal: usize,
    pub uuid: Option<String>,
    pub origin: Origin,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReferenceStrength {
    Structured,
    Lexical,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Necessity {
    Required,
    Optional,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MissingPolicy {
    Diagnose,
    Hint,
}
#[derive(Clone, Debug, Default, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
pub struct ReferenceTarget {
    pub kind: String,
    pub name: String,
    pub member: String,
    pub member_name: String,
    pub predefined: String,
}
impl ReferenceTarget {
    pub fn key(&self) -> String {
        format!("{}.{}", self.kind, self.name)
    }
}
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
pub enum ReferenceSurface {
    Text,
    /// Comment/PI between child nodes. It survives exactly with this source node.
    InnerMarkup,
    /// Element/namespace syntax, outside the node's inner property value.
    OpeningMarkup,
    Attribute(ExpandedName),
    /// Decoded binding (for example a UI Picture QName), not a raw metadata path.
    ContextualAttribute(ExpandedName),
}
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
pub struct ReferenceFact {
    pub fragment: String,
    pub surface: ReferenceSurface,
    pub target: ReferenceTarget,
    pub strength: ReferenceStrength,
    pub necessity: Necessity,
    pub missing: MissingPolicy,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", content = "reason", rename_all = "snake_case")]
pub enum Coverage {
    Supported,
    Opaque,
    Unsupported(String),
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CoverageNote {
    pub facet: Facet,
    pub fragment: String,
    pub coverage: Coverage,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SourceFacts {
    pub object: String,
    pub versions: FactVersions,
    pub proof: SourceProof,
    pub fragments: BTreeMap<String, SourceFragment>,
    pub references: Vec<ReferenceFact>,
    pub coverage: BTreeMap<Facet, Coverage>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub coverage_notes: Vec<CoverageNote>,
}
impl SourceFacts {
    /// Cache payload validation never promotes a legacy shape to a new reader.
    pub fn validate(&self) -> Result<()> {
        for (id, fragment) in &self.fragments {
            if id != &fragment.id || fragment.owner != self.object {
                return Err(Error::Invalid(
                    "Некорректный владелец или адрес SourceFacts".into(),
                ));
            }
            if fragment
                .parent
                .as_ref()
                .is_some_and(|p| !self.fragments.contains_key(p))
            {
                return Err(Error::Invalid(format!(
                    "Неизвестный родитель фрагмента: {id}"
                )));
            }
            if self.proof.inputs.get(&fragment.origin.path)
                != Some(&InputObservation::Content {
                    sha256: fragment.origin.sha256.clone(),
                })
                || fragment.origin.span.is_some_and(|(start, end)| start > end)
            {
                return Err(Error::Invalid(format!(
                    "Неподтверждённый источник фрагмента: {id}"
                )));
            }
        }
        for reference in &self.references {
            if !self.fragments.contains_key(&reference.fragment) {
                return Err(Error::Invalid(format!(
                    "Неизвестный origin ссылки: {}",
                    reference.fragment
                )));
            }
        }
        if self
            .coverage_notes
            .iter()
            .any(|note| !self.fragments.contains_key(&note.fragment))
        {
            return Err(Error::Invalid("Неизвестный origin coverage note".into()));
        }
        // Each node is visited once after its ancestors have been proved acyclic.
        let mut finished = BTreeSet::new();
        for id in self.fragments.keys() {
            let mut branch = BTreeSet::new();
            let mut cursor = Some(id.as_str());
            while let Some(node) = cursor {
                if finished.contains(node) {
                    break;
                }
                if !branch.insert(node.to_owned()) {
                    return Err(Error::Invalid(format!(
                        "Цикл родителей SourceFacts: {node}"
                    )));
                }
                cursor = self.fragments[node].parent.as_deref();
            }
            finished.extend(branch);
        }
        Ok(())
    }

    /// Structural adapter for the existing coordinator; no XML or new discovery.
    pub fn structural_facts(&self) -> Result<crate::Facts> {
        self.validate()?;
        let object = self
            .fragments
            .values()
            .find(|f| f.kind == FragmentKind::Object)
            .ok_or_else(|| Error::Invalid("SourceFacts не содержит объект".into()))?;
        let mut targets: BTreeMap<&str, BTreeSet<String>> = BTreeMap::new();
        for reference in &self.references {
            if matches!(reference.surface, ReferenceSurface::ContextualAttribute(_)) {
                continue;
            }
            let mut cursor = Some(reference.fragment.as_str());
            while let Some(id) = cursor {
                let fragment = &self.fragments[id];
                if fragment.kind == FragmentKind::Property && fragment.tag.local == "Type" {
                    let outer_attribute = reference.fragment == fragment.id
                        && matches!(
                            reference.surface,
                            ReferenceSurface::Attribute(_) | ReferenceSurface::OpeningMarkup
                        );
                    if !outer_attribute
                        && (reference.strength == ReferenceStrength::Structured
                            || fragment.tag.uri.is_empty())
                    {
                        let owner = fragment
                            .parent
                            .as_deref()
                            .and_then(|props| self.fragments[props].parent.as_deref());
                        if let Some(owner) = owner {
                            targets
                                .entry(owner)
                                .or_default()
                                .insert(reference.target.key());
                        }
                    }
                    break;
                }
                cursor = fragment.parent.as_deref();
            }
        }
        let mut children: BTreeMap<&str, Vec<&SourceFragment>> = BTreeMap::new();
        for field in self
            .fragments
            .values()
            .filter(|f| matches!(f.kind, FragmentKind::Field | FragmentKind::Column))
        {
            let owner = field
                .parent
                .as_deref()
                .and_then(|container| self.fragments[container].parent.as_deref());
            if let Some(owner) = owner {
                children.entry(owner).or_default().push(field);
            }
        }
        for members in children.values_mut() {
            members.sort_by_key(|f| f.origin.span.map(|span| span.0));
        }
        fn members(
            id: &str,
            children: &BTreeMap<&str, Vec<&SourceFragment>>,
            targets: &BTreeMap<&str, BTreeSet<String>>,
        ) -> Vec<crate::Member> {
            let mut result: Vec<crate::Member> = vec![];
            let mut positions = BTreeMap::new();
            for field in children.get(id).into_iter().flatten() {
                let member = crate::Member {
                    name: field.name.clone(),
                    tag: field.tag.local.clone(),
                    types: targets.get(field.id.as_str()).cloned().unwrap_or_default(),
                    columns: members(&field.id, children, targets),
                };
                if let Some(&position) = positions.get(&field.name) {
                    result[position] = member;
                } else {
                    positions.insert(field.name.clone(), result.len());
                    result.push(member);
                }
            }
            result
        }
        Ok(crate::Facts {
            targets: targets.get(object.id.as_str()).cloned().unwrap_or_default(),
            members: members(&object.id, &children, &targets),
        })
    }
}
