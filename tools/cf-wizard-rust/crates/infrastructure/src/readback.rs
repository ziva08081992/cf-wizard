//! Semantic readback rules: complete XML by default, mandatory data for stub descriptors.
use crate::{
    assets::{SHEET, UI, XSI, picture_properties},
    kinds::{REQUIRED_LISTS, REQUIRED_PROPERTIES, TYPE_FOLDERS},
    reference_lexer::references as scan,
    xml::{Xml, unescape},
};
use cf_domain::{Error, ObjectKey, Result};
use serde_json::{Value, json};
use std::collections::BTreeMap;

fn expanded(name: &str, scope: &BTreeMap<String, String>, attribute: bool) -> Result<String> {
    if let Some((p, n)) = name.split_once(':') {
        let uri = scope
            .get(p)
            .ok_or_else(|| Error::Invalid("Необъявленный XML namespace".into()))?;
        Ok(format!("{{{uri}}}{n}"))
    } else if !attribute && scope.get("").is_some_and(|uri| !uri.is_empty()) {
        Ok(format!("{{{}}}{name}", scope[""]))
    } else {
        Ok(name.into())
    }
}
fn qvalue(
    value: &str,
    scope: &BTreeMap<String, String>,
    preserve: bool,
    known_qname: bool,
) -> Value {
    if !preserve && value.trim_matches([' ', '\t', '\n', '\r']).is_empty() {
        return Value::Null;
    }
    let qname = crate::source_xml::ncname;
    let qualified = if known_qname {
        value.trim_matches([' ', '\t', '\n', '\r'])
    } else {
        value
    };
    if let Some((p, n)) = qualified.split_once(':').filter(|_| known_qname) {
        if qname(p)
            && qname(n)
            && let Some(uri) = scope.get(p)
        {
            return json!(["qname", uri, n]);
        }
    } else if known_qname && qname(qualified) {
        return json!(["qname", scope.get("").map_or("", String::as_str), qualified]);
    }
    json!(["text", value])
}
/// Match ElementTree expanded names, QName values and inherited xml:space.
pub fn xml_signature(bytes: &[u8]) -> Result<Value> {
    let text = std::str::from_utf8(bytes)
        .map_err(crate::external)?
        .replace("\r\n", "\n")
        .replace('\r', "\n");
    let x = crate::source_xml::SourceXml::parse(text.as_bytes())?.xml;
    fn node(
        x: &Xml,
        id: usize,
        parent: &BTreeMap<String, String>,
        inherited: bool,
    ) -> Result<Value> {
        let e = &x.elements[id];
        let mut scope = parent.clone();
        for a in &e.attributes {
            if a.name == "xmlns" {
                scope.insert("".into(), a.value.clone());
            } else if let Some(p) = a.name.strip_prefix("xmlns:") {
                scope.insert(p.into(), a.value.clone());
            }
        }
        let preserve = e
            .attributes
            .iter()
            .find(|a| a.name == "xml:space")
            .map_or(inherited, |a| a.value == "preserve");
        let name = expanded(&e.name, &scope, false)?;
        let xsi_type = |name: &str| {
            name.split_once(':').is_some_and(|(prefix, local)| {
                local == "type" && scope.get(prefix).map(String::as_str) == Some(XSI)
            })
        };
        let picture = name
            .strip_prefix('{')
            .and_then(|n| n.strip_suffix("}picture"))
            == Some(SHEET)
            || e.attributes.iter().any(|a| {
                xsi_type(&a.name)
                    && qvalue(&a.value, &scope, true, true) == json!(["qname", UI, "Picture"])
            });
        let typed_qname = e.attributes.iter().any(|a| {
            xsi_type(&a.name)
                && qvalue(&a.value, &scope, true, true)
                    == json!(["qname", "http://www.w3.org/2001/XMLSchema", "QName"])
        });
        let mut attrs = BTreeMap::new();
        for a in &e.attributes {
            if a.name == "xmlns" || a.name.starts_with("xmlns:") {
                continue;
            }
            let value = unescape(&a.raw.replace(['\t', '\n', '\r'], " "))?;
            attrs.insert(
                expanded(&a.name, &scope, true)?,
                qvalue(
                    &value,
                    &scope,
                    true,
                    xsi_type(&a.name) || picture && a.name == "ref",
                ),
            );
        }
        let mut text_ranges = e.text.iter().peekable();
        let mut segment = |start: usize, end: usize| -> Result<Value> {
            let mut value = String::new();
            while text_ranges.peek().is_some_and(|(r, _)| r.end <= end) {
                let (r, cdata) = text_ranges.next().expect("peeked text");
                if r.start >= start {
                    let t = &x.source[r.clone()];
                    value.push_str(&if *cdata { t.into() } else { unescape(t)? });
                }
            }
            let known_type = typed_qname
                || name == "{http://v8.1c.ru/8.1/data/core}Type"
                || name == "Type" && e.parent.is_some_and(|p| x.elements[p].local == "Type");
            Ok(qvalue(&value, &scope, preserve, known_type))
        };
        let head = segment(
            e.inner.start,
            e.children
                .first()
                .map_or(e.inner.end, |&c| x.elements[c].span.start),
        )?;
        let mut children = vec![];
        for (i, &c) in e.children.iter().enumerate() {
            let tail = segment(
                x.elements[c].span.end,
                e.children
                    .get(i + 1)
                    .map_or(e.inner.end, |&n| x.elements[n].span.start),
            )?;
            children.push(json!([node(x, c, &scope, preserve)?, tail]));
        }
        Ok(json!([name, attrs, head, children]))
    }
    node(
        &x,
        x.root,
        &BTreeMap::from([("xml".into(), "http://www.w3.org/XML/1998/namespace".into())]),
        false,
    )
}

#[derive(Clone, Copy)]
struct Node<'a>(&'a Value);
impl<'a> Node<'a> {
    fn name(self) -> &'a str {
        self.0[0].as_str().expect("canonical XML name")
    }
    fn local(self) -> &'a str {
        self.name().rsplit('}').next().expect("name")
    }
    fn qualified(self, local: &str) -> String {
        let prefix = self.name().rfind('}').map_or("", |at| &self.name()[..=at]);
        format!("{prefix}{local}")
    }
    fn children(self) -> impl Iterator<Item = Node<'a>> {
        self.0[3]
            .as_array()
            .expect("canonical XML children")
            .iter()
            .map(|entry| Node(&entry[0]))
    }
    fn one(self, name: &str) -> Result<Option<Node<'a>>> {
        let mut matching = self.children().filter(|node| node.name() == name);
        let first = matching.next();
        if matching.next().is_some() {
            return Err(Error::Invalid(format!(
                "Повтор обязательного XML-узла {name}"
            )));
        }
        Ok(first)
    }
    fn populated(self) -> bool {
        !self.0[1]
            .as_object()
            .expect("canonical XML attributes")
            .is_empty()
            || !self.0[2].is_null()
            || self.children().next().is_some()
    }
    fn attributes_without_uuid(self) -> impl Iterator<Item = (&'a String, &'a Value)> {
        self.0[1]
            .as_object()
            .expect("canonical XML attributes")
            .iter()
            .filter(|(name, _)| name.as_str() != "uuid")
    }
}
fn invalid(message: impl Into<String>) -> Error {
    Error::Invalid(message.into())
}
fn text(value: &Value) -> Option<&str> {
    (value[0].as_str() == Some("text"))
        .then(|| value[1].as_str())
        .flatten()
}
fn object<'a>(signature: &'a Value, key: &ObjectKey) -> Result<(Node<'a>, Node<'a>, uuid::Uuid)> {
    ObjectKey::parse(&key.0)?;
    if !TYPE_FOLDERS.iter().any(|(kind, _)| *kind == key.kind()) {
        return Err(invalid(format!("Неизвестный тип заглушки {}", key.0)));
    }
    let root = Node(signature);
    let mut objects = root.children();
    let object = objects
        .next()
        .ok_or_else(|| invalid("Нет объекта в descriptor заглушки"))?;
    if root.local() != "MetaDataObject"
        || objects.next().is_some()
        || object.name() != root.qualified(key.kind())
    {
        return Err(invalid(format!(
            "Тип/namespace descriptor не соответствует заглушке {}",
            key.0
        )));
    }
    let props = object
        .one(&object.qualified("Properties"))?
        .ok_or_else(|| invalid("Нет Properties заглушки"))?;
    let name = props
        .one(&object.qualified("Name"))?
        .ok_or_else(|| invalid("Нет имени заглушки"))?;
    if text(&name.0[2]) != Some(key.name()) || name.children().next().is_some() {
        return Err(invalid(format!(
            "Имя descriptor не соответствует заглушке {}",
            key.0
        )));
    }
    let uuid = object.0[1]
        .get("uuid")
        .and_then(text)
        .and_then(|value| uuid::Uuid::parse_str(value).ok())
        .ok_or_else(|| invalid(format!("Нет корректного UUID заглушки {}", key.0)))?;
    Ok((object, props, uuid))
}
pub(crate) fn validate_stub_descriptor(bytes: &[u8], key: &ObjectKey) -> Result<()> {
    object(&xml_signature(bytes)?, key)?;
    Ok(())
}
/// Compare raw XML with its original namespace context; a missing role means full comparison.
pub fn verify_xml(before: &[u8], after: &[u8], stub: Option<&ObjectKey>) -> Result<()> {
    let asset_properties = if stub.is_some() {
        picture_properties(std::str::from_utf8(before).map_err(crate::external)?)?
    } else {
        Default::default()
    };
    let before = xml_signature(before)?;
    let after = xml_signature(after)?;
    let Some(key) = stub else {
        return if before == after {
            Ok(())
        } else {
            Err(invalid("Изменилось полное содержимое XML"))
        };
    };
    let (expected, props, id) = object(&before, key)?;
    let (actual, actual_props, actual_id) = object(&after, key)?;
    if expected.name() != actual.name() || id != actual_id {
        return Err(invalid(format!(
            "Изменился тип/namespace или UUID заглушки {}",
            key.0
        )));
    }
    if !expected
        .attributes_without_uuid()
        .eq(actual.attributes_without_uuid())
    {
        return Err(invalid(format!("Изменились атрибуты заглушки {}", key.0)));
    }
    for section in expected
        .children()
        .filter(|node| node.local() != "Properties" && node.populated())
    {
        if actual
            .one(section.name())?
            .is_none_or(|found| found.0 != section.0)
        {
            return Err(invalid(format!(
                "Изменён или потерян обязательный блок {} заглушки {}",
                section.local(),
                key.0
            )));
        }
    }
    for property in props.children() {
        let required = asset_properties.contains(property.name())
            || ["Name", "Type", "BasedOn", "RegisterRecords"].contains(&property.local())
            || REQUIRED_PROPERTIES
                .iter()
                .chain(REQUIRED_LISTS.iter())
                .any(|(kind, fields)| *kind == key.kind() && fields.contains(&property.local()));
        let references = !required
            && !scan(&serde_json::to_string(property.0).map_err(crate::external)?).is_empty();
        if (required || references)
            && actual_props
                .one(property.name())?
                .is_none_or(|found| found.0 != property.0)
        {
            return Err(invalid(format!(
                "Изменено или потеряно обязательное свойство {} заглушки {}",
                property.local(),
                key.0
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "readback_tests.rs"]
mod tests;
