//! Descriptor facts are extracted once in source namespace context, before emit.
pub(crate) use crate::reference_lexer::references;
use crate::{
    canonical::sha256,
    source_xml::{CORE, CURRENT, READABLE, SHEET, SourceXml, UI, XSI},
};
use cf_application::profile;
use cf_domain::{Error, Facts, Result, source_facts::*};
use std::collections::{BTreeMap, BTreeSet};
#[cfg(test)]
#[path = "source_reader_tests.rs"]
mod tests;

pub(crate) struct SourceDocument {
    pub view: SourceXml,
    pub facts: SourceFacts,
    pub node_ids: Vec<String>,
    pub object: usize,
    subtree_targets: Vec<BTreeSet<String>>,
}
fn object_node(view: &SourceXml, object_key: &str, path: &str) -> Result<usize> {
    let nodes = &view.xml.elements[view.xml.root].children;
    let kind = object_key
        .split_once('.')
        .ok_or_else(|| Error::Invalid("Нет вида объекта".into()))?
        .0;
    if !view.metadata(view.xml.root, "MetaDataObject")
        || nodes.len() != 1
        || !view.metadata(nodes[0], kind)
    {
        return Err(Error::Invalid(format!(
            "{path}: namespace/вид descriptor не соответствует {object_key}"
        )));
    }
    Ok(nodes[0])
}
impl SourceDocument {
    /// A compatible cache supplies facts; the current source supplies byte spans.
    pub fn cached(
        object_key: &str,
        path: &str,
        bytes: &[u8],
        mut proof: SourceProof,
        mut facts: SourceFacts,
    ) -> Result<Self> {
        profile::measure(
            "document.cached",
            "engine.document",
            bytes.len(),
            Some(object_key),
            || {
                let view = SourceXml::parse(bytes)?;
                let object = object_node(&view, object_key, path)?;
                let digest =
                    profile::value("hash.sha256.content", "document", bytes.len(), None, || {
                        sha256(bytes)
                    });
                if facts.object != object_key
                    || crate::facts_cache::OBJECT_FACETS
                        .iter()
                        .any(|facet| !facts.versions.compatible(&FactVersions::current(), *facet))
                {
                    return Err(Error::Invalid(
                        "Кэш относится к другому объекту или reader profile".into(),
                    ));
                }
                let node_ids = profile::value(
                    "document.addresses",
                    "document.cached",
                    bytes.len(),
                    None,
                    || view.addresses(object_key),
                );
                for (id, address) in node_ids.iter().enumerate() {
                    let expected_parent = view.xml.elements[id]
                        .parent
                        .map(|parent| node_ids[parent].as_str());
                    let fragment = facts
                        .fragments
                        .get(address)
                        .ok_or_else(|| Error::Invalid("Кэш потерял source fragment".into()))?;
                    let span = &view.xml.elements[id].span;
                    if fragment.origin.path != path
                        || fragment.origin.sha256 != digest
                        || fragment.origin.span != Some((span.start, span.end))
                        || fragment.tag != view.names[id]
                        || fragment.parent.as_deref() != expected_parent
                    {
                        return Err(Error::Invalid(
                            "Кэш не соответствует текущим source spans".into(),
                        ));
                    }
                }
                // Byte-identical facts can be used by a newer Git revision. Their current
                // proof context is rebuilt, never inherited from the cached commit SHA.
                proof.observe(path.into(), InputObservation::Content { sha256: digest })?;
                facts.proof = proof;
                let subtree_targets = Self::reference_targets(&view, &node_ids, &facts)?;
                Ok(Self {
                    view,
                    facts,
                    node_ids,
                    object,
                    subtree_targets,
                })
            },
        )
    }
    pub fn descriptor(
        object_key: &str,
        path: &str,
        bytes: &[u8],
        proof: SourceProof,
    ) -> Result<Self> {
        profile::measure(
            "document.descriptor",
            "engine.document",
            bytes.len(),
            Some(object_key),
            || {
                let view = SourceXml::parse(bytes)
                    .map_err(|error| Error::Invalid(format!("{path}: {error}")))?;
                let object = object_node(&view, object_key, path)?;
                Self::extract(object_key, path, bytes, proof, view, object, true)
            },
        )
    }
    pub fn sidecar(owner: &str, path: &str, bytes: &[u8], proof: SourceProof) -> Result<Self> {
        profile::measure(
            "document.sidecar",
            "engine.document",
            bytes.len(),
            Some(owner),
            || {
                let view = SourceXml::parse(bytes)
                    .map_err(|error| Error::Invalid(format!("{path}: {error}")))?;
                let object = view.xml.root;
                Self::extract(owner, path, bytes, proof, view, object, false)
            },
        )
    }
    fn extract(
        object_key: &str,
        path: &str,
        bytes: &[u8],
        mut proof: SourceProof,
        view: SourceXml,
        object: usize,
        metadata: bool,
    ) -> Result<Self> {
        profile::measure(
            "document.extract",
            "engine.document",
            bytes.len(),
            Some(object_key),
            || {
                let digest =
                    profile::value("hash.sha256.content", "document", bytes.len(), None, || {
                        sha256(bytes)
                    });
                proof.observe(
                    path.into(),
                    InputObservation::Content {
                        sha256: digest.clone(),
                    },
                )?;
                let node_ids = if metadata {
                    view.addresses(object_key)
                } else {
                    view.addresses(&format!("{object_key}/$file:{path}"))
                };
                let mut facts = SourceFacts {
                    coverage_notes: vec![],
                    object: object_key.into(),
                    versions: FactVersions::current(),
                    proof,
                    fragments: BTreeMap::new(),
                    references: vec![],
                    coverage: BTreeMap::from([
                        (Facet::Metadata, Coverage::Supported),
                        (Facet::References, Coverage::Supported),
                        (Facet::Assets, Coverage::Supported),
                    ]),
                };
                if !metadata {
                    facts.coverage.remove(&Facet::Metadata);
                }
                for id in 0..view.xml.elements.len() {
                    let element = &view.xml.elements[id];
                    let parent = element.parent;
                    let local = view.names[id].local.as_str();
                    let parent_local = parent.map(|p| view.names[p].local.as_str());
                    let kind = if id == object {
                        FragmentKind::Object
                    } else if parent.is_some_and(|p| view.metadata(p, "ChildObjects"))
                        && view.metadata(id, local)
                    {
                        if matches!(
                            local,
                            "Form"
                                | "Template"
                                | "Command"
                                | "Method"
                                | "Operation"
                                | "URLTemplate"
                        ) {
                            FragmentKind::Declaration
                        } else if parent
                            .and_then(|p| view.xml.elements[p].parent)
                            .is_some_and(|p| view.metadata(p, "TabularSection"))
                        {
                            FragmentKind::Column
                        } else {
                            FragmentKind::Field
                        }
                    } else if parent.is_some_and(|p| view.metadata(p, "Properties"))
                        && view.metadata(id, local)
                    {
                        FragmentKind::Property
                    } else if local == "Item"
                        && (view.names[id].uri == READABLE || view.names[id].uri.is_empty())
                    {
                        FragmentKind::PropertyItem
                    } else if local == "Type"
                        && (view.names[id].uri == CORE || view.names[id].uri.is_empty())
                    {
                        FragmentKind::Type
                    } else {
                        FragmentKind::Value
                    };
                    let name = view
                        .child(id, "Properties")
                        .map_or_else(|| view.xml.text(id), |p| view.text_value(p, "Name"))?;
                    facts.fragments.insert(
                        node_ids[id].clone(),
                        SourceFragment {
                            id: node_ids[id].clone(),
                            owner: object_key.into(),
                            parent: parent.map(|p| node_ids[p].clone()),
                            kind,
                            tag: view.names[id].clone(),
                            name: name.trim().into(),
                            ordinal: view.ordinals[id],
                            uuid: view.xml.attr(id, "uuid").map(str::to_owned),
                            origin: Origin {
                                path: path.into(),
                                sha256: digest.clone(),
                                node: view.path(id),
                                span: Some((element.span.start, element.span.end)),
                            },
                        },
                    );
                    let text = view.xml.text(id)?;
                    let mut candidates: Vec<(
                        ReferenceTarget,
                        ReferenceStrength,
                        MissingPolicy,
                        ReferenceSurface,
                    )> = vec![];
                    // Type QName and typed MDObjectRef are distinct from arbitrary lexical hints.
                    let core_type = local == "Type"
                        && (view.names[id].uri == CORE
                            || metadata
                                && parent_local == Some("Type")
                                && view.metadata(id, "Type"));
                    if core_type && !text.trim().is_empty() {
                        let qname = match view.resolve(id, text.trim(), false) {
                            Ok(value) => Some(value),
                            Err(_)
                                if !metadata
                                    && view.names[view.xml.root].uri.is_empty()
                                    && view.names[view.xml.root].local == "Form"
                                    && parent.is_some_and(|p| {
                                        view.names[p].uri.is_empty()
                                            && view.names[p].local == "Type"
                                    })
                                    && text.trim().strip_prefix("cfg:").is_some_and(|local| {
                                        crate::source_xml::ncname(local)
                                            && !references(local).is_empty()
                                    }) =>
                            {
                                // The historical unqualified Form profile uses literal cfg:
                                // hints even with a CORE Type element. Never invent a binding;
                                // declared bindings and namespace-aware forms stay strict.
                                facts.coverage_notes.push(CoverageNote {
                                    facet: Facet::References,
                                    fragment: node_ids[id].clone(),
                                    coverage: Coverage::Unsupported(
                                        "Legacy Form: cfg без namespace, только lexical hint"
                                            .into(),
                                    ),
                                });
                                None
                            }
                            Err(error) if !references(&text).is_empty() => {
                                return Err(Error::Invalid(format!("{path}: {error}")));
                            }
                            Err(_) => {
                                // Datatype validation outside the metadata-reference profile
                                // belongs to the platform. Preserve the explicit limitation.
                                facts.coverage_notes.push(CoverageNote {
                                    facet: Facet::Metadata,
                                    fragment: node_ids[id].clone(),
                                    coverage: Coverage::Unsupported(
                                        "QName типа вне профиля ссылок: namespace не объявлен"
                                            .into(),
                                    ),
                                });
                                None
                            }
                        };
                        if let Some(qname) = qname
                            && (qname.uri == CURRENT || qname.uri.is_empty())
                        {
                            candidates.extend(references(&qname.local).into_iter().map(|r| {
                                (
                                    r,
                                    ReferenceStrength::Structured,
                                    MissingPolicy::Diagnose,
                                    ReferenceSurface::Text,
                                )
                            }));
                        }
                    }
                    let mut xsi_type = None;
                    for attribute in &element.attributes {
                        if view
                            .resolve(id, &attribute.name, true)
                            .is_ok_and(|name| name.uri == XSI && name.local == "type")
                        {
                            xsi_type = Some(
                                view.resolve(id, &attribute.value, false)
                                    .map_err(|error| Error::Invalid(format!("{path}: {error}")))?,
                            );
                        }
                    }
                    let typed_ref = xsi_type
                        .as_ref()
                        .is_some_and(|name| name.uri == READABLE && name.local == "MDObjectRef");
                    let required_property = parent.is_some_and(|parent| {
                        view.metadata(parent, "Properties")
                            && view.xml.elements[parent].parent.is_some_and(|owner| {
                                view.metadata(owner, &view.names[owner].local)
                                    && crate::source_schema::structured_scalar(
                                        &view.names[owner].local,
                                        local,
                                    )
                            })
                    });
                    if typed_ref || required_property {
                        candidates.extend(references(text.trim()).into_iter().map(|r| {
                            (
                                r,
                                ReferenceStrength::Structured,
                                MissingPolicy::Diagnose,
                                ReferenceSurface::Text,
                            )
                        }));
                    }
                    candidates.extend(references(&text).into_iter().map(|r| {
                        (
                            r,
                            ReferenceStrength::Lexical,
                            MissingPolicy::Hint,
                            ReferenceSurface::Text,
                        )
                    }));
                    let mut markup = vec![
                        (
                            element.span.start..element.inner.start,
                            ReferenceSurface::OpeningMarkup,
                        ),
                        (
                            element.inner.end..element.span.end,
                            ReferenceSurface::OpeningMarkup,
                        ),
                    ];
                    // Exclude child subtrees and decoded text. The remaining disjoint
                    // source ranges contain only markup comments/PI/CDATA delimiters.
                    let mut covered: Vec<_> = element
                        .children
                        .iter()
                        .map(|&child| view.xml.elements[child].span.clone())
                        .chain(element.text.iter().map(|(range, _)| range.clone()))
                        .collect();
                    covered.sort_by_key(|range| range.start);
                    let mut cursor = element.inner.start;
                    for range in covered {
                        if range.start > cursor {
                            markup.push((cursor..range.start, ReferenceSurface::InnerMarkup));
                        }
                        cursor = cursor.max(range.end);
                    }
                    if cursor < element.inner.end {
                        markup.push((cursor..element.inner.end, ReferenceSurface::InnerMarkup));
                    }
                    if id == view.xml.root {
                        markup.push((0..element.span.start, ReferenceSurface::OpeningMarkup));
                        markup.push((
                            element.span.end..view.xml.source.len(),
                            ReferenceSurface::OpeningMarkup,
                        ));
                    }
                    for (range, surface) in markup {
                        for target in references(&view.xml.source[range]) {
                            candidates.push((
                                target,
                                ReferenceStrength::Lexical,
                                MissingPolicy::Hint,
                                surface.clone(),
                            ));
                        }
                    }
                    for attribute in &element.attributes {
                        if attribute.name == "xmlns" || attribute.name.starts_with("xmlns:") {
                            continue;
                        }
                        let attribute_name = view.resolve(id, &attribute.name, true)?;
                        candidates.extend(references(&attribute.value).into_iter().map(|r| {
                            (
                                r,
                                ReferenceStrength::Lexical,
                                MissingPolicy::Hint,
                                ReferenceSurface::Attribute(attribute_name.clone()),
                            )
                        }));
                    }
                    let attribute_targets: BTreeSet<_> = candidates
                        .iter()
                        .filter_map(|(target, _, _, surface)| {
                            matches!(surface, ReferenceSurface::Attribute(_))
                                .then_some(target.clone())
                        })
                        .collect();
                    candidates.retain(|(target, _, _, surface)| {
                        *surface != ReferenceSurface::OpeningMarkup
                            || !attribute_targets.contains(target)
                    });
                    let mut node_references = Vec::new();
                    let picture = view.names[id].uri == SHEET && local == "picture"
                        || xsi_type
                            .as_ref()
                            .is_some_and(|name| name.uri == UI && name.local == "Picture");
                    if picture && let Some(reference) = view.xml.attr(id, "ref") {
                        let name = view
                            .resolve(id, reference, false)
                            .map_err(|error| Error::Invalid(format!("{path}: {error}")))?;
                        if name.uri == UI {
                            node_references.push(ReferenceFact {
                                fragment: node_ids[id].clone(),
                                surface: ReferenceSurface::ContextualAttribute(ExpandedName {
                                    uri: String::new(),
                                    local: "ref".into(),
                                }),
                                target: ReferenceTarget {
                                    kind: "CommonPicture".into(),
                                    name: name.local,
                                    ..ReferenceTarget::default()
                                },
                                strength: ReferenceStrength::Structured,
                                necessity: Necessity::Required,
                                missing: MissingPolicy::Hint,
                            });
                        }
                    }
                    for (target, strength, missing, surface) in candidates {
                        let reference = ReferenceFact {
                            fragment: node_ids[id].clone(),
                            surface,
                            target,
                            strength,
                            necessity: Necessity::Required,
                            missing,
                        };
                        if !node_references.contains(&reference) {
                            node_references.push(reference);
                        }
                    }
                    facts.references.extend(node_references);
                }
                let subtree_targets = Self::reference_targets(&view, &node_ids, &facts)?;
                Ok(Self {
                    view,
                    facts,
                    node_ids,
                    object,
                    subtree_targets,
                })
            },
        )
    }
    /// The coordinator consumes source facts without reparsing source XML or
    /// rescanning provisional output. Each subtree relation is assembled once.
    fn reference_targets(
        view: &SourceXml,
        node_ids: &[String],
        facts: &SourceFacts,
    ) -> Result<Vec<BTreeSet<String>>> {
        let positions: BTreeMap<_, _> = node_ids
            .iter()
            .enumerate()
            .map(|(id, address)| (address.as_str(), id))
            .collect();
        let mut targets = vec![BTreeSet::new(); node_ids.len()];
        for reference in &facts.references {
            let id = *positions
                .get(reference.fragment.as_str())
                .ok_or_else(|| Error::Invalid("Ссылка не относится к узлу исходного XML".into()))?;
            targets[id].insert(reference.target.key());
        }
        for id in (0..targets.len()).rev() {
            if let Some(parent) = view.xml.elements[id].parent {
                let children = targets[id].clone();
                targets[parent].extend(children);
            }
        }
        Ok(targets)
    }
    pub(crate) fn target_members(&self) -> usize {
        self.subtree_targets.iter().map(BTreeSet::len).sum()
    }

    pub fn targets(&self, id: usize) -> &BTreeSet<String> {
        &self.subtree_targets[id]
    }
    /// Facts beneath a source fragment; mixed-content occurrence order is
    /// supplied separately by ordered_candidates for first-endpoint policies.
    pub fn references_in(&self, id: usize) -> impl Iterator<Item = &ReferenceFact> {
        let span = self.view.xml.elements[id].span.clone();
        self.facts.references.iter().filter(move |reference| {
            if reference.fragment == self.node_ids[id] {
                return matches!(
                    reference.surface,
                    ReferenceSurface::Text | ReferenceSurface::InnerMarkup
                );
            }
            self.facts
                .fragments
                .get(&reference.fragment)
                .and_then(|f| f.origin.span)
                .is_some_and(|(start, end)| start >= span.start && end <= span.end)
        })
    }
    pub fn ordered_candidates(&self, id: usize) -> Vec<ReferenceTarget> {
        references(&self.view.xml.source[self.view.xml.elements[id].inner.clone()])
    }
    pub fn inner_targets(&self, id: usize) -> BTreeSet<String> {
        let span = &self.view.xml.elements[id].span;
        self.facts
            .references
            .iter()
            .filter(|reference| {
                if matches!(reference.surface, ReferenceSurface::ContextualAttribute(_)) {
                    return false;
                }
                let fragment = &self.facts.fragments[&reference.fragment];
                fragment.origin.span.is_some_and(|(start, end)| {
                    start >= span.start
                        && end <= span.end
                        && (reference.fragment != self.node_ids[id]
                            || matches!(
                                reference.surface,
                                ReferenceSurface::Text | ReferenceSurface::InnerMarkup
                            ))
                })
            })
            .map(|reference| reference.target.key())
            .collect()
    }
    pub fn name(&self, id: usize) -> &str {
        &self.facts.fragments[&self.node_ids[id]].name
    }
    /// Explicit V1-compatible subset for the existing coordinator, not a V2 upgrade.
    pub fn structural_facts(&self) -> Result<Facts> {
        profile::measure("facts.structural", "engine.facts", 0, None, || {
            profile::note("facts.validation_in_structural", "facts.structural", 1);
            self.facts.structural_facts()
        })
    }
}
