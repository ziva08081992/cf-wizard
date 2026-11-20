//! Managed-form bindings over the already validated source document.
use crate::{
    source_query::{Fields, read as query_read},
    source_reader::SourceDocument,
};
use cf_application::profile;
use cf_domain::{Result, fold};
use std::collections::{BTreeMap, BTreeSet};
pub type FormPath = (BTreeSet<String>, Vec<String>);

fn type_targets(document: &SourceDocument, id: usize, constants: bool) -> Result<BTreeSet<String>> {
    let view = &document.view;
    let legacy = view.names[view.xml.root].uri.is_empty();
    let mut output = BTreeSet::new();
    let mut pending = vec![id];
    while let Some(node) = pending.pop() {
        pending.extend(view.xml.elements[node].children.iter().copied());
        if view.names[node].local != "Type"
            || !(view.names[node].uri == crate::source_xml::CORE
                || legacy && view.names[node].uri.is_empty())
        {
            continue;
        }
        let text = view.xml.text(node)?;
        let text = text.trim();
        let value = match view.resolve(node, text, false) {
            Ok(q) if q.uri == crate::source_xml::CURRENT || legacy && q.uri.is_empty() => {
                Some(q.local)
            }
            Err(_)
                if legacy
                    && text
                        .strip_prefix("cfg:")
                        .is_some_and(crate::source_xml::ncname) =>
            {
                Some(text[4..].into())
            }
            _ => None,
        };
        if let Some(value) = value {
            if constants && value == "ConstantsSet" {
                output.insert("@constants".into());
            }
            output.extend(
                crate::reference_lexer::references(&value)
                    .iter()
                    .map(|r| r.key()),
            );
        }
    }
    Ok(output)
}

pub struct FormRead {
    pub paths: Vec<FormPath>,
    pub references: Vec<cf_domain::source_facts::ReferenceFact>,
}
#[cfg(test)]
pub fn paths(document: &SourceDocument) -> Result<Vec<FormPath>> {
    Ok(read(document)?.paths)
}
pub fn read(document: &SourceDocument) -> Result<FormRead> {
    profile::measure(
        "form.read",
        "engine.scan",
        0,
        Some(&document.facts.object),
        || {
            let view = &document.view;
            let xml = &view.xml;
            let root = xml.root;
            if view.names[root].local != "Form" {
                return Ok(FormRead {
                    paths: vec![],
                    references: vec![],
                });
            }
            let uri = &view.names[root].uri;
            let child = |id: usize, local: &str| {
                xml.elements[id]
                    .children
                    .iter()
                    .copied()
                    .find(|&n| view.names[n].local == local && &view.names[n].uri == uri)
            };
            let name = |id| xml.attr(id, "name").unwrap_or("").to_owned();
            let mut attributes = BTreeMap::new();
            let mut columns = BTreeMap::new();
            let mut queries = BTreeMap::<String, Fields>::new();
            let mut output = vec![];
            let mut references = vec![];
            if let Some(group) = child(root, "Attributes") {
                for &attribute in &xml.elements[group].children {
                    if view.names[attribute].local != "Attribute"
                        || &view.names[attribute].uri != uri
                    {
                        continue;
                    }
                    let label = name(attribute);
                    let mut types = BTreeSet::new();
                    if let Some(id) = child(attribute, "Type") {
                        types.extend(type_targets(document, id, true)?);
                    }
                    if let Some(group) = child(attribute, "Columns").filter(|_| !label.is_empty()) {
                        for &column in &xml.elements[group].children {
                            if &view.names[column].uri != uri {
                                continue;
                            }
                            let column_name = name(column);
                            if let Some(id) =
                                child(column, "Type").filter(|_| !column_name.is_empty())
                            {
                                let targets = type_targets(document, id, false)?;
                                if !targets.is_empty() {
                                    columns.insert((label.clone(), column_name), targets);
                                }
                            }
                        }
                    }
                    if let Some(settings) = child(attribute, "Settings") {
                        if let Some(main) = child(settings, "MainTable") {
                            types.extend(document.targets(main).iter().cloned());
                        }
                        if let Some(query) = child(settings, "QueryText") {
                            let mut unnamed = Fields::new();
                            let fields = if label.is_empty() {
                                &mut unnamed
                            } else {
                                queries.entry(label.clone()).or_default()
                            };
                            let result = query_read(&xml.text(query)?, fields);
                            use cf_domain::source_facts::{
                                MissingPolicy, Necessity, ReferenceFact, ReferenceStrength,
                                ReferenceSurface, ReferenceTarget,
                            };
                            references.extend(result.required.into_iter().map(|key| {
                                let (kind, name) =
                                    key.split_once('.').expect("typed query endpoint");
                                ReferenceFact {
                                    fragment: document.node_ids[query].clone(),
                                    surface: ReferenceSurface::Text,
                                    target: ReferenceTarget {
                                        kind: kind.into(),
                                        name: name.into(),
                                        ..Default::default()
                                    },
                                    strength: ReferenceStrength::Structured,
                                    necessity: Necessity::Required,
                                    missing: MissingPolicy::Diagnose,
                                }
                            }));
                            output.extend(
                                result
                                    .paths
                                    .into_iter()
                                    .map(|(key, path)| (BTreeSet::from([key]), path)),
                            );
                        }
                    }
                    if !label.is_empty() && !types.is_empty() {
                        attributes.insert(label, types);
                    }
                }
            }
            // XML ownership replaces the ambiguous preceding raw name= search.
            let mut item_paths = BTreeMap::new();
            for (id, expanded) in view.names.iter().enumerate() {
                if expanded.local == "DataPath"
                    && &expanded.uri == uri
                    && let Some(owner) = xml.elements[id].parent
                {
                    let label = name(owner);
                    if !label.is_empty() {
                        item_paths.insert(label, xml.text(id)?);
                    }
                }
            }
            // Preserve the compatibility order: source text, then source attributes.
            let texts = (0..xml.elements.len()).map(|id| xml.text(id));
            let attribute_values = xml
                .elements
                .iter()
                .flat_map(|element| &element.attributes)
                .filter(|a| a.name != "xmlns" && !a.name.starts_with("xmlns:"))
                .map(|a| Ok(a.value.clone()));
            let mut seen = BTreeSet::new();
            for value in texts.chain(attribute_values) {
                let value = value?;
                let Some(mut segments) = path_segments(&value) else {
                    continue;
                };
                if segments.len() < 2 {
                    continue;
                }
                if !seen.insert(value) {
                    continue;
                }
                if segments.len() >= 4
                    && segments[0] == "Items"
                    && segments[2] == "CurrentData"
                    && let Some(base) = item_paths.get(&segments[1]).and_then(|s| path_segments(s))
                {
                    let mut rewritten = base;
                    rewritten.extend_from_slice(&segments[3..]);
                    segments = rewritten;
                }
                if segments.len() >= 3 {
                    if let Some(paths) = queries
                        .get(&segments[0])
                        .and_then(|fields| fields.get(&fold(&segments[1])))
                    {
                        output.extend(paths.iter().map(|(key, base)| {
                            let mut path = base.clone();
                            path.extend_from_slice(&segments[2..]);
                            (BTreeSet::from([key.clone()]), path)
                        }));
                    }
                    if let Some(types) = columns.get(&(segments[0].clone(), segments[1].clone())) {
                        output.push((types.clone(), segments[2..].to_vec()));
                    }
                }
                if let Some(types) = attributes.get(&segments[0]) {
                    output.push((types.clone(), segments[1..].to_vec()));
                }
            }
            Ok(FormRead {
                paths: output,
                references,
            })
        },
    )
}
fn path_segments(value: &str) -> Option<Vec<String>> {
    let segments: Vec<_> = value.split('.').collect();
    (segments
        .iter()
        .all(|s| !s.is_empty() && s.chars().all(|c| c.is_alphanumeric() || c == '_')))
    .then(|| segments.into_iter().map(str::to_owned).collect())
}
#[cfg(test)]
mod tests {
    use super::*;
    use cf_domain::source_facts::SourceProof;
    fn read(xml: &str) -> Vec<FormPath> {
        let document = SourceDocument::sidecar(
            "CommonForm.Main",
            "Forms/Main.xml",
            xml.as_bytes(),
            SourceProof {
                identity: "synthetic".into(),
                snapshot: "fixed".into(),
                inputs: BTreeMap::new(),
            },
        )
        .unwrap();
        paths(&document).unwrap()
    }
    #[test]
    fn compatible_forms_keep_pinned_independent_expectations() {
        let oracle: serde_json::Value =
            serde_json::from_slice(include_bytes!("../../../tests/fixtures/oracle-0.7.3.json"))
                .unwrap();
        for row in oracle["forms"].as_array().unwrap() {
            assert_eq!(
                serde_json::to_value(read(row["xml"].as_str().unwrap())).unwrap(),
                row["paths"]
            );
        }
    }
    #[test]
    fn aliases_cdata_single_quotes_and_real_item_owners() {
        let xml = format!(
            r#"<f:Form xmlns:f="urn:form" xmlns:t="{}" xmlns:c="{}"><f:Attributes><f:Attribute name='Object'><f:Type><t:Type>c:CatalogRef.Owner</t:Type></f:Type></f:Attribute></f:Attributes><f:Items><f:Table name='Grid'><f:DataPath><![CDATA[Object.Rows]]></f:DataPath><f:Decoration name='Other'/></f:Table><f:InputField name='Probe'><f:DataPath>Items.Grid.CurrentData.Value</f:DataPath></f:InputField></f:Items></f:Form>"#,
            crate::source_xml::CORE,
            crate::source_xml::CURRENT
        );
        assert_eq!(
            read(&xml),
            vec![
                (
                    BTreeSet::from(["Catalog.Owner".into()]),
                    vec!["Rows".into()]
                ),
                (
                    BTreeSet::from(["Catalog.Owner".into()]),
                    vec!["Rows".into(), "Value".into()]
                ),
            ]
        );
    }
    #[test]
    fn constants_legacy_hint_never_overrides_an_explicit_namespace() {
        let legacy = "<Form><Attributes><Attribute name=\"Values\"><Type><Type>cfg:ConstantsSet</Type></Type></Attribute></Attributes><Items><Input><DataPath>Values.Feature</DataPath></Input></Items></Form>";
        assert_eq!(
            read(legacy),
            vec![(
                BTreeSet::from(["@constants".into()]),
                vec!["Feature".into()]
            )]
        );
        assert!(read(&legacy.replace("<Form>", "<Form xmlns:cfg=\"urn:foreign\">")).is_empty());
        let foreign_element = legacy
            .replace("<Type>cfg:", "<x:Type xmlns:x=\"urn:foreign\">cfg:")
            .replace("ConstantsSet</Type>", "ConstantsSet</x:Type>");
        assert!(read(&foreign_element).is_empty());
        assert_eq!(
            read(&legacy.replace(
                "<Form>",
                &format!("<Form xmlns:cfg=\"{}\">", crate::source_xml::CURRENT)
            )),
            read(legacy)
        );
    }
    #[test]
    fn rebound_qnames_cannot_supply_attribute_or_column_path_types() {
        for root in ["", "urn:form"] {
            let xml = format!(
                r#"<Form xmlns="{root}" xmlns:t="{}" xmlns:c="urn:foreign"><Attributes><Attribute name="Object"><Type><t:Type>c:CatalogRef.Decoy</t:Type></Type></Attribute><Attribute name="Rows"><Columns><Column name="Item"><Type><t:Type>c:CatalogRef.Decoy</t:Type></Type></Column></Columns></Attribute></Attributes><Items><Input><DataPath>Object.Name</DataPath></Input><Input><DataPath>Rows.Item.Name</DataPath></Input></Items></Form>"#,
                crate::source_xml::CORE
            );
            assert!(read(&xml).is_empty());
            let bound = xml.replace("urn:foreign", crate::source_xml::CURRENT);
            assert_eq!(
                read(&bound),
                vec![
                    (
                        BTreeSet::from(["Catalog.Decoy".into()]),
                        vec!["Name".into()]
                    ),
                    (
                        BTreeSet::from(["Catalog.Decoy".into()]),
                        vec!["Name".into()]
                    )
                ]
            );
        }
    }

    #[test]
    fn modern_form_does_not_promote_unqualified_inner_type_to_a_real_binding() {
        for (value, path) in [
            ("CatalogRef.Decoy", "Values.Name"),
            ("ConstantsSet", "Values.Feature"),
        ] {
            let xml = format!(
                r#"<f:Form xmlns:f="http://v8.1c.ru/8.3/xcf/logform" xmlns:c="{}"><f:Attributes><f:Attribute name="Values"><f:Type><Type xmlns="">c:{value}</Type></f:Type></f:Attribute></f:Attributes><f:ChildItems><f:InputField><f:DataPath>{path}</f:DataPath></f:InputField></f:ChildItems></f:Form>"#,
                crate::source_xml::CURRENT
            );
            assert!(read(&xml).is_empty(), "{value}");
            let core = xml.replace(
                "<Type xmlns=\"\">",
                &format!("<Type xmlns=\"{}\">", crate::source_xml::CORE),
            );
            assert!(!read(&core).is_empty(), "CORE positive control: {value}");
        }
    }
}
