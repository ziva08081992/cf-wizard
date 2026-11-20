//! Compact wire layout for the canonical descriptor subset of SourceFacts 2.
use cf_domain::{
    Error, Result,
    source_facts::{
        Coverage, CoverageNote, ExpandedName, Facet, FactVersions, FragmentKind, MissingPolicy,
        Necessity, NodeStep, Origin, ReferenceFact, ReferenceStrength, ReferenceSurface,
        ReferenceTarget, SourceFacts, SourceFragment, SourceProof,
    },
};
use serde::{
    Deserialize, Deserializer, Serialize,
    de::{self, IgnoredAny, SeqAccess, Visitor},
};
use std::{collections::BTreeMap, fmt, fmt::Write as _, io, marker::PhantomData};

#[cfg(test)]
#[path = "facts_compact_performance.rs"]
mod performance;
#[cfg(test)]
#[path = "facts_compact_tests.rs"]
mod tests;

const LIMIT: usize = super::MAX_PAYLOAD as usize;
const DEPTH: usize = 256; // The XML reader rejects a deeper element stack.
// These lower bounds on each expanded JSON entry also cap DTO allocations
// while deserializing, before any parent-path expansion can take place.
const NODES: usize = LIMIT / 96;
const REFERENCES: usize = LIMIT / 64;
const NOTES: usize = LIMIT / 32;

fn invalid(reason: &str) -> Error {
    Error::Invalid(format!("Некорректный компактный SourceFacts: {reason}"))
}

fn oversized() -> Error {
    Error::Invalid("Слишком большой SourceFacts payload".into())
}

#[derive(Serialize)]
#[serde(transparent)]
struct LimitedVec<T, const MAX: usize>(Vec<T>);

impl<'de, T: Deserialize<'de>, const MAX: usize> Deserialize<'de> for LimitedVec<T, MAX> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        struct Array<T, const MAX: usize>(PhantomData<T>);
        impl<'de, T: Deserialize<'de>, const MAX: usize> Visitor<'de> for Array<T, MAX> {
            type Value = LimitedVec<T, MAX>;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "an array of at most {MAX} SourceFacts records")
            }

            fn visit_seq<S: SeqAccess<'de>>(
                self,
                mut sequence: S,
            ) -> std::result::Result<Self::Value, S::Error> {
                let mut items = Vec::new();
                while items.len() < MAX {
                    let Some(item) = sequence.next_element()? else {
                        return Ok(LimitedVec(items));
                    };
                    items.push(item);
                }
                if sequence.next_element::<IgnoredAny>()?.is_some() {
                    return Err(de::Error::custom("Слишком много записей SourceFacts"));
                }
                Ok(LimitedVec(items))
            }
        }
        deserializer.deserialize_seq(Array::<T, MAX>(PhantomData))
    }
}

impl<T, const MAX: usize> Default for LimitedVec<T, MAX> {
    fn default() -> Self {
        Self(Vec::new())
    }
}

impl<T, const MAX: usize> LimitedVec<T, MAX> {
    fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct DescriptorFacts {
    object: String,
    versions: FactVersions,
    proof: SourceProof,
    origin: (String, String),
    names: LimitedVec<ExpandedName, NODES>,
    nodes: LimitedVec<Node, NODES>,
    references: LimitedVec<Reference, REFERENCES>,
    coverage: BTreeMap<Facet, Coverage>,
    #[serde(default, skip_serializing_if = "LimitedVec::is_empty")]
    coverage_notes: LimitedVec<Note, NOTES>,
}

#[derive(Serialize, Deserialize)]
struct Node(
    Option<u32>, // parent node, always earlier in the array
    u32,         // expanded XML name
    usize,       // sibling ordinal
    FragmentKind,
    String,
    Option<String>,
    Option<(usize, usize)>,
);

#[derive(Serialize, Deserialize)]
struct Reference(
    u32,
    ReferenceSurface,
    ReferenceTarget,
    ReferenceStrength,
    Necessity,
    MissingPolicy,
);

#[derive(Serialize, Deserialize)]
struct Note(Facet, u32, Coverage);

impl DescriptorFacts {
    /// The caller has already validated the SourceFacts contract. An older,
    /// valid shape can remain in V2 without normalizing any of its evidence.
    pub(super) fn from_facts(facts: &SourceFacts) -> Result<Option<Self>> {
        let Some(object) = facts
            .fragments
            .values()
            .find(|fragment| fragment.kind == FragmentKind::Object)
        else {
            return Ok(None);
        };
        if facts.fragments.len() > NODES
            || facts.references.len() > REFERENCES
            || facts.coverage_notes.len() > NOTES
        {
            return Err(oversized());
        }
        let mut names = Vec::new();
        let mut name_ids = BTreeMap::new();
        let mut nodes = Vec::with_capacity(facts.fragments.len());
        let mut ids: BTreeMap<&str, u32> = BTreeMap::new();
        for (address, fragment) in &facts.fragments {
            if fragment.origin.path != object.origin.path
                || fragment.origin.sha256 != object.origin.sha256
            {
                return Ok(None);
            }
            let (parent, prefix, parent_steps) = match fragment.parent.as_deref() {
                Some(parent) => {
                    let (Some(&id), Some(fragment)) =
                        (ids.get(parent), facts.fragments.get(parent))
                    else {
                        return Ok(None);
                    };
                    (Some(id), parent, fragment.origin.node.as_slice())
                }
                None => (None, facts.object.as_str(), &[][..]),
            };
            let tag = &fragment.tag;
            let ordinal = fragment.ordinal;
            let Some((step, ancestors)) = fragment.origin.node.split_last() else {
                return Ok(None);
            };
            let suffix = address
                .strip_prefix(prefix)
                .and_then(|rest| rest.strip_prefix('/'));
            if ancestors != parent_steps
                || step.name != *tag
                || step.ordinal != ordinal
                || ancestors.len() >= DEPTH
                || suffix != Some(format!("{{{}}}{}[{ordinal}]", tag.uri, tag.local).as_str())
            {
                return Ok(None);
            }
            let name = *name_ids.entry(tag).or_insert_with(|| {
                let next = names.len() as u32;
                names.push(tag.clone());
                next
            });
            ids.insert(address.as_str(), nodes.len() as u32);
            nodes.push(Node(
                parent,
                name,
                ordinal,
                fragment.kind.clone(),
                fragment.name.clone(),
                fragment.uuid.clone(),
                fragment.origin.span,
            ));
        }
        let id_of = |address: &str| {
            ids.get(address)
                .copied()
                .ok_or_else(|| invalid("адрес ссылки"))
        };
        let references = facts
            .references
            .iter()
            .map(|r| {
                Ok(Reference(
                    id_of(&r.fragment)?,
                    r.surface.clone(),
                    r.target.clone(),
                    r.strength,
                    r.necessity,
                    r.missing,
                ))
            })
            .collect::<Result<_>>()?;
        let coverage_notes = facts
            .coverage_notes
            .iter()
            .map(|note| {
                Ok(Note(
                    note.facet,
                    id_of(&note.fragment)?,
                    note.coverage.clone(),
                ))
            })
            .collect::<Result<_>>()?;
        Ok(Some(Self {
            object: facts.object.clone(),
            versions: facts.versions.clone(),
            proof: facts.proof.clone(),
            origin: (object.origin.path.clone(), object.origin.sha256.clone()),
            names: LimitedVec(names),
            nodes: LimitedVec(nodes),
            references: LimitedVec(references),
            coverage: facts.coverage.clone(),
            coverage_notes: LimitedVec(coverage_notes),
        }))
    }
}

#[derive(Clone, Copy)]
struct Lineage {
    depth: usize,
    address: usize, // exact JSON string bytes, including quotes
    steps: usize,   // exact JSON array bytes
}

struct NameSize {
    json: usize,
    segment: usize, // escaped name bytes and the five ASCII address delimiters
}

struct JsonCount(usize);

impl io::Write for JsonCount {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0 = self
            .0
            .checked_add(bytes.len())
            .ok_or_else(|| io::Error::other("SourceFacts size overflow"))?;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn json_size<T: Serialize + ?Sized>(value: &T) -> Result<usize> {
    let mut counter = JsonCount(0);
    serde_json::to_writer(&mut counter, value).map_err(crate::external)?;
    Ok(counter.0)
}

fn sum(sizes: &[usize]) -> Result<usize> {
    sizes.iter().try_fold(0usize, |total, size| {
        total.checked_add(*size).ok_or_else(oversized)
    })
}

fn object_size(fields: &[(&str, usize)]) -> Result<usize> {
    fields
        .iter()
        .try_fold(2 + fields.len().saturating_sub(1), |total, (name, size)| {
            // All field names below are fixed ASCII identifiers.
            sum(&[total, name.len(), 3, *size])
        })
}

fn at<T>(values: &[T], id: u32) -> Result<&T> {
    values
        .get(id as usize)
        .ok_or_else(|| invalid("индекс за пределами таблицы"))
}

impl DescriptorFacts {
    /// Charge the *current full SourceFacts JSON* exactly. Only integer
    /// prefix lengths are retained, so repeated long ancestors cannot cause
    /// allocation of the expanded graph before the limit is proved.
    fn preflight(&self, limit: usize) -> Result<(usize, Vec<Lineage>)> {
        if self.names.0.len() > self.nodes.0.len() {
            return Err(invalid("больше имён, чем узлов"));
        }
        let owner = json_size(&self.object)?;
        let path = json_size(&self.origin.0)?;
        let sha256 = json_size(&self.origin.1)?;
        let mut expanded = object_size(&[
            ("object", owner),
            ("versions", json_size(&self.versions)?),
            ("proof", json_size(&self.proof)?),
            ("fragments", 2),
            ("references", 2),
            ("coverage", json_size(&self.coverage)?),
        ])?;
        if !self.coverage_notes.is_empty() {
            expanded = sum(&[expanded, 1, "\"coverage_notes\":".len(), 2])?;
        }
        if expanded > limit {
            return Err(oversized());
        }
        let mut charge = |sizes: &[usize]| {
            expanded = sum(&[expanded, sum(sizes)?])?;
            if expanded > limit {
                Err(oversized())
            } else {
                Ok(())
            }
        };
        let names = self
            .names
            .0
            .iter()
            .map(|tag| {
                let uri = json_size(&tag.uri)?;
                let local = json_size(&tag.local)?;
                Ok(NameSize {
                    json: object_size(&[("uri", uri), ("local", local)])?,
                    segment: sum(&[5, uri - 2, local - 2])?,
                })
            })
            .collect::<Result<Vec<_>>>()?;

        let mut nodes = Vec::new();
        for (i, Node(parent, name, ordinal, kind, label, uuid, span)) in
            self.nodes.0.iter().enumerate()
        {
            let name = at(&names, *name)?;
            let ordinal = json_size(ordinal)?;
            let lineage = match parent {
                Some(parent) => *at(&nodes, *parent)?, // also rejects self and forward parents
                None => Lineage {
                    depth: 0,
                    address: owner,
                    steps: 2,
                },
            };
            if lineage.depth >= DEPTH || span.is_some_and(|(start, end)| start > end) {
                return Err(invalid("глубина или диапазон байтов"));
            }
            let address = sum(&[lineage.address, name.segment, ordinal])?;
            let step = object_size(&[("name", name.json), ("ordinal", ordinal)])?;
            let steps = sum(&[lineage.steps, usize::from(parent.is_some()), step])?;
            let origin = object_size(&[
                ("path", path),
                ("sha256", sha256),
                ("node", steps),
                ("span", json_size(span)?),
            ])?;
            let fragment = object_size(&[
                ("id", address),
                ("owner", owner),
                ("parent", if parent.is_some() { lineage.address } else { 4 }),
                ("kind", json_size(kind)?),
                ("tag", name.json),
                ("name", json_size(label)?),
                ("ordinal", ordinal),
                ("uuid", json_size(uuid)?),
                ("origin", origin),
            ])?;
            charge(&[usize::from(i != 0), address, 1, fragment])?; // map key and colon
            nodes.push(Lineage {
                depth: lineage.depth + 1,
                address,
                steps,
            });
        }
        for (i, Reference(node, surface, target, strength, necessity, missing)) in
            self.references.0.iter().enumerate()
        {
            let reference = object_size(&[
                ("fragment", at(&nodes, *node)?.address),
                ("surface", json_size(surface)?),
                ("target", json_size(target)?),
                ("strength", json_size(strength)?),
                ("necessity", json_size(necessity)?),
                ("missing", json_size(missing)?),
            ])?;
            charge(&[usize::from(i != 0), reference])?;
        }
        for (i, Note(facet, node, coverage)) in self.coverage_notes.0.iter().enumerate() {
            let note = object_size(&[
                ("facet", json_size(facet)?),
                ("fragment", at(&nodes, *node)?.address),
                ("coverage", json_size(coverage)?),
            ])?;
            charge(&[usize::from(i != 0), note])?;
        }
        Ok((expanded, nodes))
    }

    pub(super) fn check_budget(&self) -> Result<()> {
        self.preflight(LIMIT).map(|_| ())
    }

    pub(super) fn expand(self) -> Result<SourceFacts> {
        let (_, lengths) =
            cf_application::profile::measure("facts.preflight", "facts.decode", 0, None, || {
                self.preflight(LIMIT)
            })?;
        cf_application::profile::measure("facts.expand", "facts.decode", 0, None, || {
            self.expand_checked(lengths)
        })
    }

    fn expand_checked(self, lengths: Vec<Lineage>) -> Result<SourceFacts> {
        let mut fragments: BTreeMap<String, SourceFragment> = BTreeMap::new();
        let mut addresses = Vec::with_capacity(lengths.len());
        for (Node(parent, name, ordinal, kind, label, uuid, span), length) in
            self.nodes.0.into_iter().zip(lengths)
        {
            let name = at(&self.names.0, name)?;
            let parent = parent.map(|id| at(&addresses, id)).transpose()?;
            let mut address = String::with_capacity(length.address - 2);
            let prefix = parent.map_or(self.object.as_str(), String::as_str);
            write!(
                address,
                "{prefix}/{{{}}}{}[{ordinal}]",
                name.uri, name.local
            )
            .map_err(crate::external)?;
            let mut node = Vec::with_capacity(length.depth);
            if let Some(parent) = parent {
                let fragment = fragments
                    .get(parent)
                    .ok_or_else(|| invalid("отсутствует родитель"))?;
                node.extend_from_slice(&fragment.origin.node);
            }
            node.push(NodeStep {
                name: name.clone(),
                ordinal,
            });
            let fragment = SourceFragment {
                id: address.clone(),
                owner: self.object.clone(),
                parent: parent.cloned(),
                kind,
                tag: name.clone(),
                name: label,
                ordinal,
                uuid,
                origin: Origin {
                    path: self.origin.0.clone(),
                    sha256: self.origin.1.clone(),
                    node,
                    span,
                },
            };
            if fragments.insert(address.clone(), fragment).is_some() {
                return Err(invalid("повтор адреса фрагмента"));
            }
            addresses.push(address);
        }
        let references = self
            .references
            .0
            .into_iter()
            .map(
                |Reference(id, surface, target, strength, necessity, missing)| {
                    Ok(ReferenceFact {
                        fragment: at(&addresses, id)?.clone(),
                        surface,
                        target,
                        strength,
                        necessity,
                        missing,
                    })
                },
            )
            .collect::<Result<_>>()?;
        let coverage_notes = self
            .coverage_notes
            .0
            .into_iter()
            .map(|Note(facet, node, coverage)| {
                Ok(CoverageNote {
                    facet,
                    fragment: at(&addresses, node)?.clone(),
                    coverage,
                })
            })
            .collect::<Result<_>>()?;
        Ok(SourceFacts {
            object: self.object,
            versions: self.versions,
            proof: self.proof,
            fragments,
            references,
            coverage: self.coverage,
            coverage_notes,
        })
    }
}
