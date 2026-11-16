//! Byte-preserving minimal descriptors and protective merge rules.
#[cfg(test)]
use crate::{
    kinds::{OPEN_PROPERTIES, REQUIRED_CHILDREN, REQUIRED_LISTS, REQUIRED_PROPERTIES},
    metadata::{member_name, object_element, scan},
    xml::{Edit, Xml, escape},
};
#[cfg(test)]
use cf_domain::{Error, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct StubSpec {
    pub members: BTreeSet<(String, String)>,
    pub predefined: bool,
    pub based_on: bool,
    pub by_value: bool,
    #[serde(default)]
    pub finalized: bool,
    #[serde(default)]
    pub property_items: BTreeMap<String, BTreeSet<usize>>,
}
#[derive(Clone, Debug)]
pub struct StubOutput {
    pub descriptor: String,
    pub patterns: Vec<String>,
}
#[cfg(test)]
pub use legacy::*;
#[cfg(test)]
mod legacy {
    use super::*;
    fn indent_before(xml: &Xml, inner: usize, end: usize) -> &str {
        xml.source[..end + 1]
            .rfind('\n')
            .filter(|&newline| newline > inner)
            .map_or("", |start| &xml.source[start..end])
    }
    pub fn clean_properties(
        xml: &Xml,
        props: usize,
        self_key: &str,
        keep_kind: &str,
        present: &BTreeSet<String>,
        based_on: bool,
        own: &BTreeSet<(String, String)>,
    ) -> String {
        clean_properties_tracked(xml, props, self_key, keep_kind, present, based_on, own)
    }
    pub(crate) fn clean_properties_tracked(
        xml: &Xml,
        props: usize,
        self_key: &str,
        keep_kind: &str,
        present: &dyn Presence,
        based_on: bool,
        own: &BTreeSet<(String, String)>,
    ) -> String {
        clean_projected_properties(
            xml, props, self_key, keep_kind, present, based_on, own, None,
        )
    }
    #[allow(clippy::too_many_arguments)]
    fn clean_projected_properties(
        xml: &Xml,
        props: usize,
        self_key: &str,
        keep_kind: &str,
        present: &dyn Presence,
        based_on: bool,
        own: &BTreeSet<(String, String)>,
        projection: Option<&StubSpec>,
    ) -> String {
        let kind = self_key.split('.').next().unwrap_or_default();
        let node = &xml.elements[props];
        let mut out = xml.source[node.span.start..node.inner.start].to_owned();
        let mut pos = node.inner.start;
        for &property in &node.children {
            let p = &xml.elements[property];
            out.push_str(&xml.source[pos..p.span.start]);
            if let Some(indices) = projection.and_then(|s| s.property_items.get(&p.name)) {
                if indices.is_empty() {
                    out.push_str(&format!("<{}/>", p.name));
                } else {
                    out.push_str(&xml.source[p.span.start..p.inner.start]);
                    for &index in indices {
                        if let Some(&item) = p.children.get(index) {
                            out.push_str(xml.raw(item));
                        }
                    }
                    out.push_str(&format!("</{}>", p.name));
                }
                pos = p.span.end;
                continue;
            }
            let protected = ["Name", "Synonym", "Comment", "Type"].contains(&p.name.as_str())
                || REQUIRED_PROPERTIES
                    .iter()
                    .any(|&(k, props)| k == kind && props.contains(&p.name.as_str()));
            let foreign = !protected
                && scan(&xml.source[p.inner.clone()]).iter().any(|r| {
                    if !keep_kind.is_empty() && r.kind == keep_kind {
                        return false;
                    }
                    if r.key() == self_key
                        && (r.member.is_empty() || r.member.starts_with("Standard"))
                        && r.predefined.is_empty()
                    {
                        return false;
                    }
                    if r.key() == self_key
                        && OPEN_PROPERTIES.contains(&p.name.as_str())
                        && own.contains(&(r.member.clone(), r.member_name.clone()))
                    {
                        return false;
                    }
                    true
                });
            if protected || !foreign {
                out.push_str(xml.raw(property));
                pos = p.span.end;
                continue;
            }
            let mut items = String::new();
            let mut last = p.inner.start;
            let mut is_list = !p.children.is_empty();
            for &item in &p.children {
                let item_node = &xml.elements[item];
                let gap = &xml.source[last..item_node.span.start];
                let body = xml.raw(item);
                last = item_node.span.end;
                if item_node.name != "xr:Item" {
                    is_list = false;
                    break;
                }
                if scan(body).iter().all(|r| present.contains(&r.key())) {
                    items.push_str(gap);
                    items.push_str(body);
                }
            }
            let non_empty = based_on && p.name == "BasedOn"
                || REQUIRED_LISTS
                    .iter()
                    .any(|&(k, props)| k == kind && props.contains(&p.name.as_str()));
            if projection.is_none() && is_list && non_empty && items.is_empty() {
                items.push_str(&xml.source[p.inner.start..xml.elements[p.children[0]].span.end]);
            }
            if !is_list || items.is_empty() {
                out.push_str(&format!("<{}/>", p.name));
            } else {
                out.push_str(&xml.source[p.span.start..p.inner.start]);
                out.push_str(&items);
                out.push_str(indent_before(xml, p.inner.start, p.inner.end));
                out.push_str(&format!("</{}>", p.name));
            }
            pos = p.span.end;
        }
        out.push_str(&xml.source[pos..node.span.end]);
        out
    }
    pub fn stub_descriptor(
        text: &str,
        key: &str,
        stub: &StubSpec,
        present: &BTreeSet<String>,
    ) -> Result<StubOutput> {
        stub_descriptor_tracked(text, key, stub, present)
    }
    pub(crate) fn stub_descriptor_tracked(
        text: &str,
        key: &str,
        stub: &StubSpec,
        present: &dyn Presence,
    ) -> Result<StubOutput> {
        let xml = Xml::parse(text.as_bytes())?;
        let object = object_element(&xml)
            .ok_or_else(|| Error::Invalid(format!("Нет descriptor объекта {key}")))?;
        let kind = key.split('.').next().unwrap_or_default();
        if crate::kinds::full_resource(kind) {
            return Ok(StubOutput {
                descriptor: text.into(),
                patterns: vec!["Ext/".into()],
            });
        }
        let mut edits = vec![];
        let mut patterns = vec![];
        for &section in &xml.elements[object].children {
            let node = &xml.elements[section];
            if node.name == "Properties" {
                edits.push(Edit {
                    range: node.span.clone(),
                    replacement: clean_projected_properties(
                        &xml,
                        section,
                        key,
                        "",
                        present,
                        stub.based_on,
                        &stub.members,
                        stub.finalized.then_some(stub),
                    ),
                });
            } else if node.name == "ChildObjects" {
                let names: Vec<_> = node
                    .children
                    .iter()
                    .map(|&n| member_name(&xml, n))
                    .collect();
                let wanted: Vec<_> = node
                    .children
                    .iter()
                    .zip(&names)
                    .map(|(&n, name)| {
                        let child = &xml.elements[n];
                        stub.members.contains(&(child.name.clone(), name.clone()))
                            || stub.members.contains(&("*".into(), name.clone()))
                            || !stub.finalized
                                && stub.by_value
                                && child.name == "Dimension"
                                && xml.raw(n).contains("<Master>true</Master>")
                    })
                    .collect();
                let required = REQUIRED_CHILDREN
                    .iter()
                    .find(|&&(k, _)| k == kind)
                    .map_or(&[][..], |&(_, r)| r);
                let satisfied =
                    node.children.iter().zip(&wanted).any(|(&n, &want)| {
                        want && required.contains(&xml.elements[n].name.as_str())
                    });
                let extra = if satisfied || stub.finalized {
                    None
                } else {
                    required.iter().find_map(|&tag| {
                        node.children
                            .iter()
                            .position(|&n| xml.elements[n].name == tag)
                    })
                };
                let mut kept = String::new();
                let mut last = node.inner.start;
                for (i, &child) in node.children.iter().enumerate() {
                    let item = &xml.elements[child];
                    let gap = &xml.source[last..item.span.start];
                    last = item.span.end;
                    if !wanted[i] && extra != Some(i) {
                        continue;
                    }
                    kept.push_str(gap);
                    kept.push_str(xml.raw(child));
                    match item.name.as_str() {
                        "Form" => patterns.extend([
                            format!("Forms/{}.xml", names[i]),
                            format!("Forms/{}/", names[i]),
                        ]),
                        "Template" => patterns.extend([
                            format!("Templates/{}.xml", names[i]),
                            format!("Templates/{}/", names[i]),
                        ]),
                        "Command" => patterns.push(format!("Commands/{}/", names[i])),
                        _ => {}
                    }
                }
                edits.push(Edit {
                    range: node.span.clone(),
                    replacement: if kept.is_empty() {
                        "<ChildObjects/>".into()
                    } else {
                        format!(
                            "<ChildObjects>{kept}{}</ChildObjects>",
                            indent_before(&xml, node.inner.start, node.inner.end)
                        )
                    },
                });
            }
        }
        if stub.predefined {
            patterns.push("Ext/Predefined.xml".into());
        }
        Ok(StubOutput {
            descriptor: xml.apply(edits)?,
            patterns,
        })
    }
    pub fn content_of(text: &str) -> &str {
        let mut from = 0;
        while let Some(offset) = text[from..].find("<Content") {
            let start = from + offset;
            let next = text.as_bytes().get(start + 8);
            if !matches!(next, Some(b'>') | Some(b'/')) {
                from = start + 1;
                continue;
            }
            if text[start..].starts_with("<Content/>") {
                return &text[start..start + 10];
            }
            return text[start..]
                .find("</Content>")
                .map_or("", |end| &text[start..start + end + 10]);
        }
        ""
    }
    pub fn merge_settings(
        configuration: &str,
        selected: &BTreeSet<String>,
        stubs: &BTreeSet<String>,
        rules: &BTreeMap<String, Vec<String>>,
        subsystem: &str,
    ) -> String {
        let mut output="\u{feff}<?xml version=\"1.0\" encoding=\"UTF-8\"?>\r\n<Settings xmlns=\"http://v8.1c.ru/8.3/config/merge/settings\" xmlns:xs=\"http://www.w3.org/2001/XMLSchema\" xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\" version=\"1.2\" platformVersion=\"8.3.11\">\r\n\t<Parameters>\r\n\t\t<ConfigurationsRelation>ConfigurationsNotRelated</ConfigurationsRelation>\r\n\t\t<AllowMainConfigurationObjectDeletion>false</AllowMainConfigurationObjectDeletion>\r\n\t</Parameters>\r\n\t<Objects>\r\n".to_owned();
        fn object(output: &mut String, name: &str, rule: &str, props: &[String]) {
            output.push_str(&format!("\t\t<Object fullNameInSecondConfiguration=\"{}\">\r\n\t\t\t<MergeRule>{rule}</MergeRule>\r\n",escape(name)));
            if !props.is_empty() {
                output.push_str("\t\t\t<Properties>\r\n");
                for property in props {
                    output.push_str(&format!("\t\t\t\t<Property name=\"{}\">\r\n\t\t\t\t\t<MergeRule>DoNotMerge</MergeRule>\r\n\t\t\t\t</Property>\r\n",escape(property)));
                }
                output.push_str("\t\t\t</Properties>\r\n");
            }
            output.push_str("\t\t</Object>\r\n");
        }
        if !configuration.is_empty() {
            object(
                &mut output,
                &format!("Configuration.{configuration}"),
                "DoNotMerge",
                &[],
            );
        }
        for key in selected {
            object(&mut output, key, "GetFromSecondConfiguration", &[]);
        }
        for key in stubs {
            object(
                &mut output,
                key,
                "MergePrioritizingMainConfiguration",
                rules.get(key).map_or(&[], Vec::as_slice),
            );
        }
        for (key, props) in rules {
            if !stubs.contains(key) {
                object(
                    &mut output,
                    key,
                    "MergePrioritizingMainConfiguration",
                    props,
                );
            }
        }
        object(
            &mut output,
            &format!("Subsystem.{subsystem}"),
            "DoNotMerge",
            &[],
        );
        output.push_str("\t</Objects>\r\n</Settings>\r\n");
        output
    }
    #[cfg(test)]
    mod tests {
        use super::*;
        #[test]
        fn descriptors_and_rules_match_pinned_oracle() {
            let path = std::path::Path::new(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../tests/fixtures/oracle-0.7.3.json"
            ));
            let oracle: serde_json::Value =
                serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
            for row in oracle["stub_descriptors"].as_array().unwrap() {
                let spec: StubSpec = serde_json::from_value(row["spec"].clone()).unwrap();
                let present = serde_json::from_value(row["present"].clone()).unwrap();
                let out = stub_descriptor(
                    row["xml"].as_str().unwrap(),
                    row["key"].as_str().unwrap(),
                    &spec,
                    &present,
                )
                .unwrap();
                assert_eq!(
                    out.descriptor,
                    row["output"].as_str().unwrap(),
                    "{}",
                    row["key"]
                );
                assert_eq!(serde_json::to_value(out.patterns).unwrap(), row["patterns"]);
            }
            for row in oracle["merge_settings"].as_array().unwrap() {
                let selected = serde_json::from_value(row["selected"].clone()).unwrap();
                let stubs = serde_json::from_value(row["stubs"].clone()).unwrap();
                let rules = serde_json::from_value(row["rules"].clone()).unwrap();
                assert_eq!(
                    merge_settings(
                        row["configuration"].as_str().unwrap(),
                        &selected,
                        &stubs,
                        &rules,
                        "_Перенос"
                    ),
                    row["xml"].as_str().unwrap()
                );
            }
        }
    }

    pub(crate) trait Presence {
        fn contains(&self, key: &str) -> bool;
    }
    impl Presence for BTreeSet<String> {
        fn contains(&self, key: &str) -> bool {
            BTreeSet::contains(self, key)
        }
    }
    pub(crate) struct TrackedPresence<'a> {
        present: &'a BTreeSet<String>,
        observed: std::cell::RefCell<BTreeMap<String, bool>>,
    }
    impl<'a> TrackedPresence<'a> {
        pub(crate) fn new(present: &'a BTreeSet<String>) -> Self {
            Self {
                present,
                observed: Default::default(),
            }
        }
        pub(crate) fn observations(self) -> BTreeMap<String, bool> {
            self.observed.into_inner()
        }
    }
    impl Presence for TrackedPresence<'_> {
        fn contains(&self, key: &str) -> bool {
            let present = self.present.contains(key);
            self.observed.borrow_mut().insert(key.to_owned(), present);
            present
        }
    }

    #[cfg(test)]
    mod presence_memo_tests {
        use super::*;

        #[test]
        fn tracks_short_circuit_presence_and_invalidates_when_observed_answers_change() {
            let text = "<MetaDataObject xmlns:xr=\"urn:test\"><Document><Properties><Name>Test</Name><Items><xr:Item>Catalog.A Catalog.B</xr:Item></Items></Properties><ChildObjects/></Document></MetaDataObject>";
            let spec = StubSpec::default();
            let empty = BTreeSet::new();
            let tracker = TrackedPresence::new(&empty);
            let initial = stub_descriptor_tracked(text, "Document.Test", &spec, &tracker).unwrap();
            let observed = tracker.observations();
            assert_eq!(observed, BTreeMap::from([("Catalog.A".into(), false)]));

            let b_only = BTreeSet::from(["Catalog.B".into(), "Catalog.Unrelated".into()]);
            assert_eq!(
                stub_descriptor(text, "Document.Test", &spec, &b_only)
                    .unwrap()
                    .descriptor,
                initial.descriptor,
            );

            let a_only = BTreeSet::from(["Catalog.A".into()]);
            let tracker = TrackedPresence::new(&a_only);
            let partial = stub_descriptor_tracked(text, "Document.Test", &spec, &tracker).unwrap();
            assert_eq!(
                tracker.observations(),
                BTreeMap::from([("Catalog.A".into(), true), ("Catalog.B".into(), false),])
            );
            assert_eq!(partial.descriptor, initial.descriptor);
            let both = BTreeSet::from(["Catalog.A".into(), "Catalog.B".into()]);
            let complete = stub_descriptor(text, "Document.Test", &spec, &both).unwrap();
            assert_ne!(complete.descriptor, initial.descriptor);
            assert!(complete.descriptor.contains("Catalog.A Catalog.B"));
        }

        #[test]
        fn protected_properties_preserve_references_without_reading_presence() {
            let text = "<MetaDataObject><Catalog><Properties><Name>Test</Name><Synonym>Catalog.A</Synonym><Comment>Document.B</Comment><Type>CatalogRef.C</Type></Properties><ChildObjects/></Catalog></MetaDataObject>";
            let present = BTreeSet::new();
            let tracker = TrackedPresence::new(&present);
            let output =
                stub_descriptor_tracked(text, "Catalog.Test", &StubSpec::default(), &tracker)
                    .unwrap();
            assert!(tracker.observations().is_empty());
            for reference in ["Catalog.A", "Document.B", "CatalogRef.C"] {
                assert!(output.descriptor.contains(reference));
            }
        }
    }
}
