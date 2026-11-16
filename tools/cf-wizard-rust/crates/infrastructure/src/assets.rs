//! Contextual XML asset references; the indexed metadata lexer stays unchanged.
use crate::xml::Xml;
use cf_domain::Result;
use std::collections::BTreeSet;

pub(crate) const UI: &str = "http://v8.1c.ru/8.1/data/ui";
pub(crate) const SHEET: &str = "http://v8.1c.ru/8.2/data/spreadsheet";
pub(crate) const XSI: &str = "http://www.w3.org/2001/XMLSchema-instance";
const MD: &str = "http://v8.1c.ru/8.3/MDClasses";

fn namespace<'a>(xml: &'a Xml, mut id: usize, prefix: &str) -> Option<&'a str> {
    loop {
        let element = &xml.elements[id];
        for attribute in element.attributes.iter().rev() {
            if (prefix.is_empty() && attribute.name == "xmlns")
                || attribute.name.strip_prefix("xmlns:") == Some(prefix)
            {
                return Some(&attribute.value);
            }
        }
        id = element.parent?;
    }
}
fn qname<'a>(xml: &'a Xml, id: usize, value: &'a str) -> Option<(&'a str, &'a str)> {
    let value = value.trim_matches([' ', '\t', '\r', '\n']);
    let (prefix, local) = value.split_once(':').unwrap_or(("", value));
    if local.is_empty() || local.contains([':', ' ', '\t', '\r', '\n']) {
        return None;
    }
    Some((namespace(xml, id, prefix)?, local))
}

/// A bare QName is not enough: only spreadsheet picture or explicitly typed Picture values.
#[cfg(test)]
pub(crate) fn picture_names(text: &str) -> Result<BTreeSet<String>> {
    let mut result = BTreeSet::new();
    pictures(text, |_, _, name| {
        result.insert(name.into());
    })?;
    Ok(result)
}

/// Mandatory top-level descriptor properties, resolved in their original namespace scope.
pub(crate) fn picture_properties(text: &str) -> Result<BTreeSet<String>> {
    let mut result = BTreeSet::new();
    pictures(text, |xml, mut id, _| {
        while let Some(parent) = xml.elements[id].parent {
            let top_object = xml.elements[parent].parent;
            if qname(xml, parent, &xml.elements[parent].name) == Some((MD, "Properties"))
                && top_object.and_then(|object| xml.elements[object].parent) == Some(xml.root)
                && qname(xml, xml.root, &xml.elements[xml.root].name)
                    == Some((MD, "MetaDataObject"))
            {
                if let Some((uri, local)) = qname(xml, id, &xml.elements[id].name) {
                    result.insert(format!("{{{uri}}}{local}"));
                }
                break;
            }
            id = parent;
        }
    })?;
    Ok(result)
}

fn pictures(text: &str, mut accept: impl FnMut(&Xml, usize, &str)) -> Result<()> {
    if !text.contains("ref") {
        return Ok(());
    }
    let xml = Xml::parse(text.as_bytes())?;
    for (id, element) in xml.elements.iter().enumerate() {
        let Some(reference) = element.attributes.iter().find(|a| a.name == "ref") else {
            continue;
        };
        let sheet_picture = qname(&xml, id, &element.name) == Some((SHEET, "picture"));
        let typed_picture = element.attributes.iter().any(|attribute| {
            attribute.name.contains(':')
                && qname(&xml, id, &attribute.name) == Some((XSI, "type"))
                && qname(&xml, id, &attribute.value) == Some((UI, "Picture"))
        });
        if (sheet_picture || typed_picture)
            && let Some((UI, name)) = qname(&xml, id, &reference.value)
        {
            accept(&xml, id, name);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn picture_context_aliases_entities_and_rebinding() {
        let xml = format!(
            r#"<document xmlns="{SHEET}" xmlns:p="{UI}" xmlns:i="{XSI}">
          <picture ref="p:Cam&#101;ra"/><picture ref="p:Camera"/>
          <picture xmlns:p="urn:other" ref="p:Wrong"/>
          <foreign xmlns="urn:other" ref="p:NotAPicture"/>
          <value i:type="p:Picture" ref="p:Typed"/>
          <font ref="p:NotAPicture"/><picture ref="missing:Unknown"/>
          <picture xmlns="urn:other" ref="p:WrongElement"/>
          <nested xmlns:p="urn:other"><value i:type="p:Picture" ref="p:WrongType"/></nested>
          <picture xmlns:p="{UI}" ref="p:Change"/>
        </document>"#
        );
        assert_eq!(
            picture_names(&xml).unwrap(),
            BTreeSet::from(["Camera".into(), "Typed".into(), "Change".into(),])
        );
    }
    #[test]
    fn default_qname_namespace_and_typed_stub_properties() {
        let xml = format!(
            r#"<m:Metadata xmlns:m="urn:metadata" xmlns="{UI}" xmlns:t="{XSI}">
          <m:Picture t:type="Picture" ref="Camera"/>
          <m:Text>p:MustNotBeScanned</m:Text>
        </m:Metadata>"#
        );
        assert_eq!(
            picture_names(&xml).unwrap(),
            BTreeSet::from(["Camera".into()])
        );
    }

    #[test]
    fn typed_qname_entities_are_decoded_before_classification() {
        let xml = format!(
            r#"<values xmlns:u="{UI}" xmlns:i="{XSI}" xmlns:q="urn:other">
              <value i:type="u:Pict&#117;re" ref="u:Cam&#101;ra"/>
              <value i:type="q:Pict&#117;re" ref="u:WrongNamespace"/>
            </values>"#
        );
        assert!(!xml.contains("icture"));
        assert_eq!(
            picture_names(&xml).unwrap(),
            BTreeSet::from(["Camera".into()])
        );
    }
}
