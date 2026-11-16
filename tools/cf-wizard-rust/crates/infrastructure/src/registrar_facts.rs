//! Compact document→register facts with namespace-sensitive extraction proof.
use crate::{
    kinds::REGISTER_KINDS,
    reference_lexer::references,
    source_xml::{READABLE, SourceXml, XSI},
};
use cf_domain::{Error, Result};
use sha2::{Digest, Sha256};

pub(crate) struct RegistrarFacts {
    pub hash: Vec<u8>,
    pub links: Vec<(usize, String)>,
}
pub(crate) fn from_view(view: &SourceXml, object: usize) -> Result<RegistrarFacts> {
    let records = view
        .child(object, "Properties")
        .and_then(|properties| view.child(properties, "RegisterRecords"));
    let mut links = vec![];
    let mut type_context = vec![];
    if let Some(records) = records {
        for (ordinal, &item) in view.xml.elements[records].children.iter().enumerate() {
            if view.names[item].local != "Item"
                || !matches!(view.names[item].uri.as_str(), READABLE | "")
            {
                return Err(Error::Invalid(
                    "Неподдержанный элемент RegisterRecords".into(),
                ));
            }
            let text = view.xml.text(item)?;
            let values = references(text.trim());
            if values.len() != 1
                || !REGISTER_KINDS.contains(&values[0].kind.as_str())
                || !values[0].member.is_empty()
                || !values[0].predefined.is_empty()
            {
                return Err(Error::Invalid("Некорректная ссылка RegisterRecords".into()));
            }
            links.push((ordinal, values[0].key()));
            for attribute in &view.xml.elements[item].attributes {
                if attribute.name == "xmlns" || attribute.name.starts_with("xmlns:") {
                    continue;
                }
                let name = view.resolve(item, &attribute.name, true)?;
                if name.uri == XSI && name.local == "type" {
                    type_context
                        .push((ordinal, Some(view.resolve(item, &attribute.value, false)?)));
                }
            }
        }
    }
    // QName context outside the subtree cannot be omitted from reuse evidence.
    let value = records.map(|records| view.value(records)).transpose()?;
    let payload = serde_json::to_vec(&(value, type_context, &links)).map_err(crate::external)?;
    Ok(RegistrarFacts {
        hash: Sha256::digest(payload).to_vec(),
        links,
    })
}
pub(crate) fn extract(bytes: &[u8]) -> Result<RegistrarFacts> {
    let view = SourceXml::parse(bytes)?;
    let objects = &view.xml.elements[view.xml.root].children;
    if !view.metadata(view.xml.root, "MetaDataObject")
        || objects.len() != 1
        || !view.metadata(objects[0], "Document")
    {
        return Err(Error::Invalid(
            "RegisterRecords требует descriptor документа".into(),
        ));
    }
    from_view(&view, objects[0])
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::source_xml::MD;
    #[test]
    fn exact_links_and_ordinals_come_from_source_namespace_scope() {
        let xml = format!(
            r#"<m:MetaDataObject xmlns:m="{MD}" xmlns:r="{READABLE}"><m:Document><m:Properties><m:RegisterRecords><r:Item>AccumulationRegister.A</r:Item><r:Item>AccumulationRegister.B</r:Item></m:RegisterRecords></m:Properties></m:Document></m:MetaDataObject>"#
        );
        let facts = extract(xml.as_bytes()).unwrap();
        assert_eq!(
            facts.links,
            vec![
                (0, "AccumulationRegister.A".into()),
                (1, "AccumulationRegister.B".into())
            ]
        );
        let aliased = xml
            .replace("xmlns:m=", "xmlns:metadata=")
            .replace("<m:", "<metadata:")
            .replace("</m:", "</metadata:");
        assert_eq!(facts.links, extract(aliased.as_bytes()).unwrap().links);
    }
    #[test]
    fn unchanged_subtree_bytes_do_not_hide_a_changed_namespace_binding() {
        let xml = format!(
            r#"<MetaDataObject xmlns="{MD}" xmlns:r="{READABLE}" xmlns:i="{XSI}"><Document><Properties><RegisterRecords><r:Item i:type="r:MDObjectRef">AccumulationRegister.A</r:Item></RegisterRecords></Properties></Document></MetaDataObject>"#
        );
        let other = xml.replace(
            &format!("xmlns:r=\"{READABLE}\""),
            "xmlns:r=\"urn:unrelated\"",
        );
        assert!(
            extract(other.as_bytes()).is_err(),
            "a foreign Item cannot silently become a registrar link"
        );
        let type_only = xml
            .replace("i:type=\"r:MDObjectRef\"", "i:type=\"t:MDObjectRef\"")
            .replace("<Document>", "<Document xmlns:t=\"urn:different\">");
        assert_ne!(
            extract(xml.as_bytes()).unwrap().hash,
            extract(type_only.as_bytes()).unwrap().hash,
            "different QName bindings cannot reuse the former extraction proof"
        );
        assert!(
            extract(
                xml.replace("i:type=\"r:MDObjectRef\"", "i:type=\"missing:MDObjectRef\"")
                    .as_bytes()
            )
            .is_err()
        );
    }
}
