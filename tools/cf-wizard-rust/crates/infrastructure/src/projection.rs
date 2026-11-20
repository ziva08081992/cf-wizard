//! Compile source-node decisions before emit. No XML output is scanned for facts.
use crate::{
    kinds::{OPEN_PROPERTIES, REQUIRED_PROPERTIES},
    source_reader::SourceDocument,
    source_xml::{READABLE, SourceXml},
    stubs::StubSpec,
    xml::escape,
};
use cf_application::profile;
use cf_domain::{
    Error, Result,
    package_plan::{SourceEdit, XmlPart, XmlValue},
    source_facts::{ReferenceFact, ReferenceSurface},
};
use std::collections::{BTreeMap, BTreeSet};
#[cfg(test)]
#[path = "projection_tests.rs"]
mod tests;

#[derive(Clone, Copy)]
enum Decision {
    Drop,
    Empty,
}
pub(crate) struct DescriptorPlan {
    pub edits: Vec<SourceEdit>,
    pub expected: XmlValue,
    pub retained: BTreeSet<String>,
    pub references: Vec<ReferenceFact>,
    pub patterns: Vec<String>,
    pub observations: BTreeMap<String, bool>,
}
pub(crate) fn layout(value: &mut XmlValue, inherited_preserve: bool) {
    let preserve = value
        .attributes
        .iter()
        .find(|(name, _)| {
            name.uri == "http://www.w3.org/XML/1998/namespace" && name.local == "space"
        })
        .map_or(inherited_preserve, |(_, text)| text == "preserve");
    if !preserve
        && value
            .parts
            .iter()
            .any(|part| matches!(part, XmlPart::Element(_)))
    {
        value.parts.retain(|part| !matches!(part, XmlPart::Text(text) if text.trim_matches([' ', '\t', '\r', '\n']).is_empty()));
    }
    for part in &mut value.parts {
        if let XmlPart::Element(child) = part {
            layout(child, preserve);
        }
    }
}
pub(crate) fn semantic_value(view: &SourceXml, id: usize) -> Result<XmlValue> {
    let mut value = view.value(id)?;
    layout(&mut value, false);
    Ok(value)
}
pub(crate) fn top_property<'a>(root: &'a XmlValue, name: &str) -> Option<&'a XmlValue> {
    let object = root.parts.iter().find_map(|part| {
        if let XmlPart::Element(value) = part {
            Some(value)
        } else {
            None
        }
    })?;
    let properties = object.parts.iter().find_map(|part| match part {
        XmlPart::Element(value) if value.name.local == "Properties" => Some(value),
        _ => None,
    })?;
    properties.parts.iter().find_map(|part| match part {
        XmlPart::Element(value) if value.name.local == name => Some(value),
        _ => None,
    })
}
pub(crate) fn empty_tag(view: &SourceXml, id: usize) -> String {
    let node = &view.xml.elements[id];
    let mut text = format!("<{}", node.name);
    for attribute in &node.attributes {
        if attribute.name == "xmlns" || attribute.name.starts_with("xmlns:") {
            text.push_str(&format!(
                " {}=\"{}\"",
                attribute.name,
                escape(&attribute.value)
            ));
        }
    }
    text.push_str("/>");
    text
}
pub(crate) fn compile(
    document: &SourceDocument,
    key: &str,
    selection: &StubSpec,
    present: &BTreeSet<String>,
) -> Result<DescriptorPlan> {
    profile::measure(
        "engine.projection_compile",
        "engine.closure",
        0,
        Some(key),
        || {
            if !selection.finalized {
                return Err(Error::Invalid(
                    "Проекция требует окончательных структурных решений".into(),
                ));
            }
            let kind = key.split_once('.').map_or("", |value| value.0);
            let view = &document.view;
            let mut decisions = BTreeMap::new();
            let mut patterns = vec![];
            let mut observations = BTreeMap::new();
            let mut references_by_node: BTreeMap<usize, Vec<&ReferenceFact>> = BTreeMap::new();
            let positions: BTreeMap<_, _> = document
                .node_ids
                .iter()
                .enumerate()
                .map(|(id, name)| (name.as_str(), id))
                .collect();
            for reference in &document.facts.references {
                references_by_node
                    .entry(positions[reference.fragment.as_str()])
                    .or_default()
                    .push(reference);
            }
            if !cf_domain::full_resource(kind) {
                if let Some(properties) = view.child(document.object, "Properties") {
                    for &property in &view.xml.elements[properties].children {
                        if !view.metadata(property, &view.names[property].local) {
                            continue;
                        }
                        let local = view.names[property].local.as_str();
                        let explicit_items = selection.property_items.get(local).or_else(|| {
                            selection
                                .property_items
                                .get(&view.xml.elements[property].name)
                        });
                        if let Some(keep) = explicit_items {
                            if keep.is_empty() {
                                decisions.insert(property, Decision::Empty);
                            } else {
                                if keep.iter().any(|ordinal| {
                                    *ordinal >= view.xml.elements[property].children.len()
                                }) {
                                    return Err(Error::Invalid(format!(
                                        "Нет запланированного элемента свойства {key}.{local}"
                                    )));
                                }
                                for (ordinal, &child) in
                                    view.xml.elements[property].children.iter().enumerate()
                                {
                                    if !keep.contains(&ordinal) {
                                        decisions.insert(child, Decision::Drop);
                                    }
                                }
                            }
                            continue;
                        }
                        let protected = matches!(local, "Name" | "Synonym" | "Comment" | "Type")
                            || REQUIRED_PROPERTIES
                                .iter()
                                .any(|&(owner, names)| owner == kind && names.contains(&local));
                        if protected {
                            continue;
                        }
                        let property_references: Vec<_> = view
                            .xml
                            .descendants(property)
                            .iter()
                            .flat_map(|node| {
                                references_by_node.get(node).into_iter().flatten().copied()
                            })
                            .filter(|reference| {
                                !matches!(
                                    reference.surface,
                                    ReferenceSurface::ContextualAttribute(_)
                                )
                            })
                            .filter(|reference| {
                                reference.fragment != document.node_ids[property]
                                    || matches!(
                                        reference.surface,
                                        ReferenceSurface::Text | ReferenceSurface::InnerMarkup
                                    )
                            })
                            .collect();
                        let foreign = property_references.iter().any(|reference| {
                            let target = &reference.target;
                            if target.key() == key && target.predefined.is_empty() {
                                if target.member.is_empty() || target.member.starts_with("Standard")
                                {
                                    return false;
                                }
                                if OPEN_PROPERTIES.contains(&local)
                                    && selection.members.contains(&(
                                        target.member.clone(),
                                        target.member_name.clone(),
                                    ))
                                {
                                    return false;
                                }
                            }
                            true
                        });
                        if !foreign {
                            continue;
                        }
                        let children = &view.xml.elements[property].children;
                        let list = !children.is_empty()
                            && children.iter().all(|&child| {
                                view.names[child].local == "Item"
                                    && matches!(view.names[child].uri.as_str(), READABLE | "")
                            });
                        if !list {
                            decisions.insert(property, Decision::Empty);
                            continue;
                        }
                        let mut kept = 0;
                        for &child in children {
                            let mut include = true;
                            for node in view.xml.descendants(child) {
                                for reference in references_by_node.get(&node).into_iter().flatten()
                                {
                                    let target = reference.target.key();
                                    let exists = present.contains(&target);
                                    observations.insert(target, exists);
                                    include &= exists;
                                }
                            }
                            if include {
                                kept += 1;
                            } else {
                                decisions.insert(child, Decision::Drop);
                            }
                        }
                        if kept == 0 {
                            for child in children {
                                decisions.remove(child);
                            }
                            decisions.insert(property, Decision::Empty);
                        }
                    }
                }
                if let Some(children) = view.child(document.object, "ChildObjects") {
                    for &child in &view.xml.elements[children].children {
                        let fragment = &document.facts.fragments[&document.node_ids[child]];
                        let tag = fragment.tag.local.as_str();
                        if !view.metadata(child, tag) {
                            decisions.insert(child, Decision::Drop);
                            continue;
                        }
                        if !selection
                            .members
                            .contains(&(tag.into(), fragment.name.clone()))
                            && !selection
                                .members
                                .contains(&("*".into(), fragment.name.clone()))
                        {
                            decisions.insert(child, Decision::Drop);
                            continue;
                        }
                        match tag {
                            "Form" | "Template" => {
                                let folder = if tag == "Form" { "Forms" } else { "Templates" };
                                patterns.extend([
                                    format!("{folder}/{}.xml", fragment.name),
                                    format!("{folder}/{}/", fragment.name),
                                ]);
                            }
                            "Command" => patterns.push(format!("Commands/{}/", fragment.name)),
                            _ => {}
                        }
                    }
                    if view.xml.elements[children]
                        .children
                        .iter()
                        .all(|child| matches!(decisions.get(child), Some(Decision::Drop)))
                    {
                        for child in &view.xml.elements[children].children {
                            decisions.remove(child);
                        }
                        decisions.insert(children, Decision::Empty);
                    }
                }
                if selection.predefined {
                    patterns.push("Ext/Predefined.xml".into());
                }
            } else {
                patterns.push("Ext/".into());
            }
            let mut visible = vec![true; view.xml.elements.len()];
            for id in 0..view.xml.elements.len() {
                visible[id] = view.xml.elements[id].parent.is_none_or(|parent| {
                    visible[parent] && !matches!(decisions.get(&parent), Some(Decision::Empty))
                }) && !matches!(decisions.get(&id), Some(Decision::Drop));
            }
            let retained: BTreeSet<_> = document
                .node_ids
                .iter()
                .enumerate()
                .filter(|(id, _)| visible[*id])
                .map(|(_, name)| name.clone())
                .collect();
            let references = document
                .facts
                .references
                .iter()
                .filter(|reference| {
                    let id = positions[reference.fragment.as_str()];
                    visible[id] && !matches!(decisions.get(&id), Some(Decision::Empty))
                })
                .cloned()
                .collect();
            let edits = decisions
                .iter()
                .filter(|(id, _)| {
                    visible[**id]
                        || view.xml.elements[**id]
                            .parent
                            .is_some_and(|parent| visible[parent])
                })
                .map(|(&id, action)| {
                    let span = &view.xml.elements[id].span;
                    SourceEdit {
                        start: span.start,
                        end: span.end,
                        replacement: match action {
                            Decision::Drop => String::new(),
                            Decision::Empty => empty_tag(view, id),
                        },
                    }
                })
                .collect();
            let dropped = decisions
                .iter()
                .filter_map(|(&id, decision)| matches!(decision, Decision::Drop).then_some(id))
                .collect();
            let emptied = decisions
                .iter()
                .filter_map(|(&id, decision)| matches!(decision, Decision::Empty).then_some(id))
                .collect();
            let mut expected = view.filtered_value(view.xml.root, &dropped, &emptied)?;
            layout(&mut expected, false);
            Ok(DescriptorPlan {
                edits,
                expected,
                retained,
                references,
                patterns,
                observations,
            })
        },
    )
}
