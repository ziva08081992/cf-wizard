//! Namespace-scoped source view. Tokenization and byte spans belong to `xml`.
//! A frame is allocated only when xmlns bindings change, not for every node.
use crate::xml::{Xml, unescape};
use cf_domain::{
    Error, Result,
    package_plan::{XmlPart, XmlValue},
    source_facts::{ExpandedName, NodeStep},
};
use std::collections::{BTreeMap, BTreeSet};

pub(crate) const MD: &str = "http://v8.1c.ru/8.3/MDClasses";
pub(crate) const CORE: &str = "http://v8.1c.ru/8.1/data/core";
pub(crate) const CURRENT: &str = "http://v8.1c.ru/8.1/data/enterprise/current-config";
pub(crate) const READABLE: &str = "http://v8.1c.ru/8.3/xcf/readable";
pub(crate) const XSI: &str = "http://www.w3.org/2001/XMLSchema-instance";
pub(crate) const UI: &str = "http://v8.1c.ru/8.1/data/ui";
pub(crate) const SHEET: &str = "http://v8.1c.ru/8.2/data/spreadsheet";
const XML: &str = "http://www.w3.org/XML/1998/namespace";
const XMLNS: &str = "http://www.w3.org/2000/xmlns/";

struct Scope {
    parent: Option<usize>,
    bindings: BTreeMap<String, String>,
}
pub(crate) struct SourceXml {
    pub xml: Xml,
    scopes: Vec<Scope>,
    node_scope: Vec<usize>,
    pub names: Vec<ExpandedName>,
    pub ordinals: Vec<usize>,
}
fn invalid(message: impl Into<String>) -> Error {
    Error::Invalid(message.into())
}
/// XML 1.0 Fifth Edition NameStartChar/NameChar, excluding colon (NCName).
/// https://www.w3.org/TR/xml/#NT-NameStartChar
pub(crate) fn ncname(text: &str) -> bool {
    fn start(c: char) -> bool {
        matches!(c, 'A'..='Z' | '_' | 'a'..='z' | '\u{c0}'..='\u{d6}' | '\u{d8}'..='\u{f6}' |
            '\u{f8}'..='\u{2ff}' | '\u{370}'..='\u{37d}' | '\u{37f}'..='\u{1fff}' | '\u{200c}'..='\u{200d}' |
            '\u{2070}'..='\u{218f}' | '\u{2c00}'..='\u{2fef}' | '\u{3001}'..='\u{d7ff}' |
            '\u{f900}'..='\u{fdcf}' | '\u{fdf0}'..='\u{fffd}' | '\u{10000}'..='\u{effff}')
    }
    let mut chars = text.chars();
    chars.next().is_some_and(start) && chars.all(|c| start(c) || matches!(c, '-' | '.' | '0'..='9' | '\u{b7}' | '\u{300}'..='\u{36f}' | '\u{203f}'..='\u{2040}'))
}
impl SourceXml {
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        let xml = Xml::parse(bytes)?;
        let mut result = Self {
            xml,
            scopes: vec![Scope {
                parent: None,
                bindings: BTreeMap::from([
                    ("xml".into(), XML.into()),
                    (String::new(), String::new()),
                ]),
            }],
            node_scope: vec![],
            names: vec![],
            ordinals: vec![],
        };
        let mut sibling_counts: BTreeMap<(Option<usize>, ExpandedName), usize> = BTreeMap::new();
        for id in 0..result.xml.elements.len() {
            let element = &result.xml.elements[id];
            let parent_scope = element.parent.map_or(0, |p| result.node_scope[p]);
            let mut bindings = BTreeMap::new();
            for attr in &element.attributes {
                let prefix = if attr.name == "xmlns" {
                    Some("")
                } else {
                    attr.name.strip_prefix("xmlns:")
                };
                if let Some(prefix) = prefix {
                    if prefix == "xmlns"
                        || attr.value == XMLNS
                        || attr.value == XML && prefix != "xml"
                        || prefix == "xml" && attr.value != XML
                        || !prefix.is_empty() && attr.value.is_empty()
                    {
                        return Err(invalid("Недопустимое объявление XML namespace"));
                    }
                    bindings.insert(prefix.into(), attr.value.clone());
                }
            }
            let scope = if bindings.is_empty() {
                parent_scope
            } else {
                let index = result.scopes.len();
                result.scopes.push(Scope {
                    parent: Some(parent_scope),
                    bindings,
                });
                index
            };
            result.node_scope.push(scope);
            let name = result.resolve(id, &result.xml.elements[id].name, false)?;
            let count = sibling_counts
                .entry((result.xml.elements[id].parent, name.clone()))
                .or_default();
            result.ordinals.push(*count);
            *count += 1;
            result.names.push(name);
            let mut attributes = BTreeMap::new();
            for attr in &result.xml.elements[id].attributes {
                if attr.name == "xmlns" || attr.name.starts_with("xmlns:") {
                    continue;
                }
                let expanded = result.resolve(id, &attr.name, true)?;
                if attributes.insert(expanded, ()).is_some() {
                    return Err(invalid("Повтор XML-атрибута с одинаковым expanded name"));
                }
            }
        }
        Ok(result)
    }
    fn binding(&self, id: usize, prefix: &str) -> Option<&str> {
        let mut scope = Some(self.node_scope[id]);
        while let Some(index) = scope {
            if let Some(uri) = self.scopes[index].bindings.get(prefix) {
                return Some(uri);
            }
            scope = self.scopes[index].parent;
        }
        None
    }
    pub fn resolve(&self, id: usize, qname: &str, attribute: bool) -> Result<ExpandedName> {
        let text = qname.trim_matches([' ', '\t', '\r', '\n']);
        let qualified = text.split_once(':');
        let (prefix, local) = qualified.unwrap_or(("", text));
        if !ncname(local) || qualified.is_some() && !ncname(prefix) {
            return Err(invalid(format!("Некорректное XML QName: {text}")));
        }
        let uri = if attribute && prefix.is_empty() {
            ""
        } else {
            self.binding(id, prefix)
                .ok_or_else(|| invalid(format!("Необъявленный XML namespace: {prefix}")))?
        };
        Ok(ExpandedName {
            uri: uri.into(),
            local: local.into(),
        })
    }
    pub fn metadata(&self, id: usize, local: &str) -> bool {
        self.names[id].local == local && matches!(self.names[id].uri.as_str(), MD | "")
    }
    pub fn child(&self, id: usize, local: &str) -> Option<usize> {
        self.xml.elements[id]
            .children
            .iter()
            .copied()
            .find(|&n| self.metadata(n, local))
    }
    pub fn text_value(&self, id: usize, local: &str) -> Result<String> {
        self.child(id, local).map_or(Ok(String::new()), |child| {
            self.xml.text(child).map(|s| s.trim().into())
        })
    }
    pub fn path(&self, mut id: usize) -> Vec<NodeStep> {
        let mut steps = vec![];
        loop {
            steps.push(NodeStep {
                name: self.names[id].clone(),
                ordinal: self.ordinals[id],
            });
            let Some(parent) = self.xml.elements[id].parent else {
                break;
            };
            id = parent;
        }
        steps.reverse();
        steps
    }
    /// Stable address separates same-named declarations, fields and nested columns.
    pub fn address(&self, id: usize) -> String {
        self.path(id)
            .iter()
            .map(|step| format!("{{{}}}{}[{}]", step.name.uri, step.name.local, step.ordinal))
            .collect::<Vec<_>>()
            .join("/")
    }
    pub fn value(&self, id: usize) -> Result<XmlValue> {
        self.filtered_value(id, &BTreeSet::new(), &BTreeSet::new())
    }
    /// Build a source-derived expectation in one traversal of retained nodes.
    pub fn filtered_value(
        &self,
        id: usize,
        dropped: &BTreeSet<usize>,
        emptied: &BTreeSet<usize>,
    ) -> Result<XmlValue> {
        if emptied.contains(&id) {
            return Ok(XmlValue {
                name: self.names[id].clone(),
                attributes: vec![],
                parts: vec![],
            });
        }
        let element = &self.xml.elements[id];
        let mut attributes = vec![];
        for attr in &element.attributes {
            if attr.name == "xmlns" || attr.name.starts_with("xmlns:") {
                continue;
            }
            attributes.push((self.resolve(id, &attr.name, true)?, attr.value.clone()));
        }
        attributes.sort_by(|a, b| a.0.cmp(&b.0));
        let mut ordered = vec![];
        for &child in &element.children {
            ordered.push((self.xml.elements[child].span.start, Some(child), None));
        }
        for (span, cdata) in &element.text {
            ordered.push((span.start, None, Some((span, cdata))));
        }
        ordered.sort_by_key(|(offset, _, _)| *offset);
        let mut parts = vec![];
        for (_, child, text) in ordered {
            if let Some(child) = child
                && !dropped.contains(&child)
            {
                parts.push(XmlPart::Element(
                    self.filtered_value(child, dropped, emptied)?,
                ));
            }
            if let Some((span, cdata)) = text {
                let value = if *cdata {
                    self.xml.source[span.clone()].into()
                } else {
                    unescape(&self.xml.source[span.clone()])?
                };
                // Tokenizer may split text/CDATA; their semantic value is adjacent text.
                if let Some(XmlPart::Text(previous)) = parts.last_mut() {
                    previous.push_str(&value);
                } else {
                    parts.push(XmlPart::Text(value));
                }
            }
        }
        Ok(XmlValue {
            name: self.names[id].clone(),
            attributes,
            parts,
        })
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn namespace_frames_are_shared_and_scope_changes_are_local() {
        let input = format!(
            "<r xmlns='urn:default' xmlns:p='urn:outer'>{}<p:n xmlns:p='urn:inner'><p:value/></p:n><p:last/></r>",
            "<item/>".repeat(1000)
        );
        let view = SourceXml::parse(input.as_bytes()).unwrap();
        assert_eq!(view.scopes.len(), 3, "no map clone per unchanged node");
        assert_eq!(view.names[1002].uri, "urn:inner");
        assert_eq!(view.names[1003].uri, "urn:outer");
    }
    #[test]
    fn equivalent_attribute_qnames_and_reserved_rebindings_are_rejected() {
        for input in [
            "<r xmlns:p='urn:same' xmlns:q='urn:same' p:id='1' q:id='2'/>",
            "<r xmlns:xml='urn:wrong'/>",
            "<r xmlns:p='http://www.w3.org/XML/1998/namespace'/>",
            "<r xmlns='http://www.w3.org/2000/xmlns/'/>",
        ] {
            assert!(SourceXml::parse(input.as_bytes()).is_err(), "{input}");
        }
    }
    #[test]
    fn qname_values_validate_both_ncname_parts() {
        let view = SourceXml::parse(b"<r xmlns:cfg='urn:cfg'/>").unwrap();
        for value in [
            ":CatalogRef.Missing",
            "cfg:1Invalid",
            "cfg:CatalogRef.A/garbage",
            "cfg:Has Space",
            "cfg:One:Two",
        ] {
            assert!(
                view.resolve(view.xml.root, value, false).is_err(),
                "{value}"
            );
        }
        for value in ["cfg:CatalogRef.Тест", "cfg:A\u{301}", "cfg:_Type-1"] {
            assert!(view.resolve(view.xml.root, value, false).is_ok());
        }
    }
    #[test]
    fn mixed_text_cdata_and_attribute_names_remain_exact() {
        let view = SourceXml::parse(
            b"<r xmlns='urn:r' xmlns:p='urn:a' id='1' p:id='2'>A<![CDATA[<&>]]><c/>B</r>",
        )
        .unwrap();
        let tree = view.value(view.xml.root).unwrap();
        assert_eq!(
            tree.attributes
                .iter()
                .map(|(name, value)| (name.uri.as_str(), value.as_str()))
                .collect::<Vec<_>>(),
            vec![("", "1"), ("urn:a", "2")]
        );
        assert_eq!(tree.parts[0], XmlPart::Text("A<&>".into()));
        assert_eq!(tree.parts[2], XmlPart::Text("B".into()));
        assert!(
            serde_json::to_vec(&tree).is_ok(),
            "expanded-name attributes have a serializable representation"
        );
    }
}
