//! Code diagnostics read XML values in their namespace and item context.
use crate::source_xml::{SourceXml, XSI};
use cf_domain::{Error, Result};

const XSD: &str = "http://www.w3.org/2001/XMLSchema";

fn decimal_zero(value: &str) -> bool {
    let unsigned = value.strip_prefix(['+', '-']).unwrap_or(value);
    let mut digits = 0;
    let mut points = 0;
    for byte in unsigned.bytes() {
        match byte {
            b'0' => digits += 1,
            b'.' => points += 1,
            _ => return false,
        }
    }
    digits > 0 && points <= 1
}

pub(crate) fn codes(descriptor: &[u8], predefined: &[u8]) -> Result<Vec<String>> {
    let metadata = SourceXml::parse(descriptor)?;
    let object = metadata.xml.elements[metadata.xml.root]
        .children
        .first()
        .copied()
        .ok_or_else(|| Error::Invalid("Нет объекта для проверки предопределённых кодов".into()))?;
    let Some(properties) = metadata.child(object, "Properties") else {
        return Ok(vec![]);
    };
    if metadata.text_value(properties, "CodeLength")? != "0" {
        return Ok(vec![]);
    }
    let numeric = metadata.text_value(properties, "CodeType")? == "Number";
    let items = SourceXml::parse(predefined)?;
    let root = items.xml.root;
    if items.names[root].local != "PredefinedData" {
        return Err(Error::Invalid(
            "Неизвестный формат предопределённых данных".into(),
        ));
    }
    let namespace = &items.names[root].uri;
    let mut any_code = false;
    let mut names = vec![];
    for (id, name) in items.names.iter().enumerate() {
        if name.uri != *namespace || name.local != "Code" {
            continue;
        }
        let value = items.xml.text(id)?;
        let value = value.trim();
        if value.is_empty() {
            continue;
        }
        let datatype = items.xml.elements[id].attributes.iter().find(|attribute| {
            items
                .resolve(id, &attribute.name, true)
                .is_ok_and(|name| name.uri == XSI && name.local == "type")
        });
        let number = if let Some(datatype) = datatype {
            items
                .resolve(id, &datatype.value, false)
                .is_ok_and(|name| name.uri == XSD && name.local == "decimal")
        } else {
            numeric
        };
        if number && decimal_zero(value) {
            continue;
        }
        any_code = true;
        if let Some(parent) = items.xml.elements[id].parent
            && let Some(&name_id) = items.xml.elements[parent].children.iter().find(|&&node| {
                items.names[node].uri == *namespace && items.names[node].local == "Name"
            })
        {
            names.push(items.xml.text(name_id)?);
        }
    }
    if any_code && names.is_empty() {
        names.push(String::new());
    }
    Ok(names)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn descriptor(length: usize, kind: &str) -> String {
        format!(
            "<MetaDataObject><Catalog><Properties><Name>Test</Name><CodeLength>{length}</CodeLength><CodeType>{kind}</CodeType></Properties></Catalog></MetaDataObject>"
        )
    }
    #[test]
    fn numeric_string_empty_and_name_cases() {
        let items = "<PredefinedData><Item><Name>Empty</Name><Code> </Code></Item><Item><Name>Zero</Name><Code>-.000</Code></Item><Item><Name>A&amp;B</Name><Code>7</Code></Item></PredefinedData>";
        assert_eq!(
            codes(descriptor(0, "Number").as_bytes(), items.as_bytes()).unwrap(),
            vec!["A&B"]
        );
        assert_eq!(
            codes(descriptor(0, "String").as_bytes(), items.as_bytes()).unwrap(),
            vec!["Zero", "A&B"]
        );
        assert!(
            codes(descriptor(3, "Number").as_bytes(), items.as_bytes())
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            codes(
                descriptor(0, "String").as_bytes(),
                b"<PredefinedData><Item><Code>7</Code></Item></PredefinedData>"
            )
            .unwrap(),
            vec![""]
        );
    }
    #[test]
    fn expanded_type_names_and_foreign_values_are_distinct() {
        let items = format!(
            r#"<p:PredefinedData xmlns:p="urn:predefined" xmlns:i="{XSI}" xmlns:s="{XSD}" xmlns:f="urn:foreign"><p:Item><p:Name>Decimal</p:Name><p:Code i:type="s:decimal">+0.00</p:Code></p:Item><p:Item><p:Name>String</p:Name><p:Code i:type="s:string">0</p:Code></p:Item><p:Item xmlns:s="urn:foreign"><p:Name>Rebound</p:Name><p:Code i:type="s:decimal">0</p:Code></p:Item><f:Item><f:Code>9</f:Code></f:Item></p:PredefinedData>"#
        );
        assert_eq!(
            codes(descriptor(0, "Number").as_bytes(), items.as_bytes()).unwrap(),
            vec!["String", "Rebound"]
        );
    }
    #[test]
    fn zero_test_is_decimal_grammar_not_float_rounding() {
        for value in ["0", "+00", "-0.", ".000", "-0.000"] {
            assert!(decimal_zero(value), "{value}");
        }
        for value in ["", "+", ".", "00.0.0", "0e0", "0.0001", "-1", "NaN"] {
            assert!(!decimal_zero(value), "{value}");
        }
    }
}
