//! Faithful metadata reference lexer and facts extraction for the pinned contract.
use crate::{
    kinds::{MEMBER_KEYWORDS, TYPE_FOLDERS},
    xml::Xml,
};
use cf_domain::{Facts, Member, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct Reference {
    pub kind: String,
    pub name: String,
    pub member: String,
    pub member_name: String,
    pub predefined: String,
}
impl Reference {
    pub fn key(&self) -> String {
        format!("{}.{}", self.kind, self.name)
    }
}
pub fn kind_of(token: &str) -> Option<(&'static str, bool)> {
    if let Some(&(kind, _)) = TYPE_FOLDERS.iter().find(|&&(k, _)| k == token) {
        return Some((kind, true));
    }
    if token == "Characteristic" {
        return Some(("ChartOfCharacteristicTypes", false));
    }
    TYPE_FOLDERS
        .iter()
        .filter(|&&(kind, _)| {
            token
                .strip_prefix(kind)
                .is_some_and(|tail| tail.chars().next().is_some_and(|c| c.is_ascii_uppercase()))
        })
        .max_by_key(|&&(kind, _)| kind.len())
        .map(|&(kind, _)| (kind, false))
}
pub fn descriptor(key: &str) -> Option<String> {
    let (kind, name) = key.split_once('.')?;
    let (_, folder) = TYPE_FOLDERS.iter().find(|&&(k, _)| k == kind)?;
    Some(format!("{folder}/{name}.xml"))
}
pub fn is_ident(c: char) -> bool {
    c >= '\u{80}' || c.is_ascii_alphanumeric() || c == '_'
}
fn ident_end(text: &str, start: usize) -> usize {
    for (offset, c) in text[start..].char_indices() {
        if !is_ident(c) {
            return start + offset;
        }
    }
    text.len()
}
pub fn scan(text: &str) -> Vec<Reference> {
    let mut out = vec![];
    let mut previous = None;
    let mut skip_until = 0;
    for (start, ch) in text.char_indices() {
        let boundary = !previous.is_some_and(|c| c == '.' || is_ident(c));
        previous = Some(ch);
        if start < skip_until || !boundary || !is_ident(ch) {
            continue;
        }
        let end = ident_end(text, start);
        if text[..start].ends_with("style:") {
            out.push(Reference {
                kind: "StyleItem".into(),
                name: text[start..end].into(),
                ..Reference::default()
            });
            skip_until = end;
            continue;
        }
        if !ch.is_ascii_uppercase() || text.as_bytes().get(end) != Some(&b'.') {
            continue;
        }
        let token = &text[start..end];
        let Some((kind, exact)) = kind_of(token) else {
            continue;
        };
        let mut segments = vec![];
        let mut cursor = end;
        for _ in 0..5 {
            if text.as_bytes().get(cursor) != Some(&b'.') {
                break;
            }
            let next = cursor + 1;
            if !text[next..].chars().next().is_some_and(is_ident) {
                break;
            }
            cursor = ident_end(text, next);
            segments.push(&text[next..cursor]);
        }
        if segments.is_empty() {
            continue;
        }
        let mut r = Reference {
            kind: kind.into(),
            name: segments[0].into(),
            ..Reference::default()
        };
        if exact && segments.len() >= 3 && MEMBER_KEYWORDS.contains(&segments[1]) {
            r.member = segments[1].into();
            r.member_name = segments[2].into();
        } else if exact
            && segments.len() >= 4
            && segments[1] == "StandardCommand"
            && segments[2] == "OpenByValue"
        {
            r.member = "*".into();
            r.member_name = segments[3].into();
        } else if exact && segments.len() >= 3 && segments[1] == "StandardCommand" {
            r.member = segments[1].into();
            r.member_name = segments[2].into();
        } else if exact
            && segments.len() == 2
            && !segments[1].starts_with("Standard")
            && !MEMBER_KEYWORDS.contains(&segments[1])
            && segments[1] != "EmptyRef"
        {
            r.predefined = segments[1].into();
        } else if !exact && segments.len() >= 2 && token.contains("TabularSection") {
            r.member = "TabularSection".into();
            r.member_name = segments[1].into();
        }
        let by_value = r.member == "*"
            && segments.len() >= 4
            && segments[1] == "StandardCommand"
            && segments[2] == "OpenByValue";
        out.push(r);
        if by_value {
            out.push(Reference {
                kind: kind.into(),
                name: segments[0].into(),
                member: "StandardCommand".into(),
                member_name: "OpenByValue".into(),
                predefined: String::new(),
            });
        }
        skip_until = end;
    }
    out
}
pub fn stable_uuid(seed: &str) -> String {
    let (mut a, mut b) = (
        1_469_598_103_934_665_603u64,
        1_099_511_628_211u64 ^ 0x9e3779b97f4a7c15,
    );
    for byte in seed.bytes() {
        a = (a ^ u64::from(byte)).wrapping_mul(1_099_511_628_211);
        b = (b ^ u64::from(byte))
            .wrapping_mul(0x100000001b3)
            .wrapping_add(0x7f4a7c15);
    }
    format!(
        "{:08x}-{:04x}-4{:03x}-{:04x}-{:012x}",
        a >> 32,
        (a >> 16) & 0xffff,
        a & 0xfff,
        ((b >> 48) & 0x3fff) | 0x8000,
        b & 0xffffffffffff
    )
}
pub fn child(xml: &Xml, id: usize, name: &str) -> Option<usize> {
    xml.elements[id]
        .children
        .iter()
        .copied()
        .find(|&n| xml.elements[n].name == name)
}
pub fn object_element(xml: &Xml) -> Option<usize> {
    if xml.elements[xml.root].name != "MetaDataObject" {
        None
    } else {
        xml.elements[xml.root].children.first().copied()
    }
}
pub fn member_name(xml: &Xml, id: usize) -> String {
    if xml.elements[id].children.is_empty() {
        return xml.source[xml.elements[id].inner.clone()].to_owned();
    }
    child(xml, id, "Properties")
        .and_then(|p| child(xml, p, "Name"))
        .map(|n| xml.source[xml.elements[n].inner.clone()].to_owned())
        .unwrap_or_default()
}
pub fn type_keys(xml: &Xml, id: usize) -> BTreeSet<String> {
    child(xml, id, "Properties")
        .and_then(|p| child(xml, p, "Type"))
        .map(|n| {
            scan(&xml.source[xml.elements[n].inner.clone()])
                .iter()
                .map(Reference::key)
                .collect()
        })
        .unwrap_or_default()
}
pub fn index_members(xml: &Xml, id: usize) -> Vec<Member> {
    let mut result: Vec<Member> = vec![];
    let mut positions: BTreeMap<String, usize> = BTreeMap::new();
    for &node in &xml.elements[id].children {
        if xml.elements[node].children.is_empty() {
            continue;
        }
        let name = member_name(xml, node);
        let member = Member {
            name: name.clone(),
            tag: xml.elements[node].name.clone(),
            types: type_keys(xml, node),
            columns: child(xml, node, "ChildObjects")
                .map(|c| index_members(xml, c))
                .unwrap_or_default(),
        };
        if let Some(&position) = positions.get(&name) {
            result[position] = member;
        } else {
            positions.insert(name, result.len());
            result.push(member);
        }
    }
    result
}
pub fn extract_facts(data: &[u8]) -> Result<Facts> {
    let text = std::str::from_utf8(data).map_err(crate::external)?;
    if !text.contains("<ChildObjects") && !text.contains("<Type") {
        return Ok(Facts::default());
    }
    let xml = Xml::parse(data)?;
    let Some(object) = object_element(&xml) else {
        return Ok(Facts::default());
    };
    Ok(Facts {
        targets: type_keys(&xml, object),
        members: child(&xml, object, "ChildObjects")
            .map(|c| index_members(&xml, c))
            .unwrap_or_default(),
    })
}

#[cfg(test)]
mod member_position_tests {
    use super::*;

    #[test]
    fn duplicate_members_keep_first_position_and_last_value_in_nested_columns() {
        let xml = Xml::parse(
            br#"<ChildObjects>
<Attribute><Properties><Name>B</Name><Type>CatalogRef.Old</Type></Properties></Attribute>
<Attribute><Properties><Name>A</Name></Properties></Attribute>
<Attribute><Properties><Name>B</Name><Type>CatalogRef.New</Type></Properties></Attribute>
<TabularSection><Properties><Name>Lines</Name></Properties><ChildObjects>
<Attribute><Properties><Name>X</Name><Type>CatalogRef.Old</Type></Properties></Attribute>
<Attribute><Properties><Name>Y</Name></Properties></Attribute>
<Attribute><Properties><Name>X</Name><Type>DocumentRef.New</Type></Properties></Attribute>
</ChildObjects></TabularSection></ChildObjects>"#,
        )
        .unwrap();
        let members = index_members(&xml, xml.root);
        assert_eq!(
            members.iter().map(|m| m.name.as_str()).collect::<Vec<_>>(),
            ["B", "A", "Lines"]
        );
        assert_eq!(members[0].types, BTreeSet::from(["Catalog.New".into()]));
        assert_eq!(
            members[2]
                .columns
                .iter()
                .map(|m| m.name.as_str())
                .collect::<Vec<_>>(),
            ["X", "Y"]
        );
        assert_eq!(
            members[2].columns[0].types,
            BTreeSet::from(["Document.New".into()])
        );
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/fixtures/");
    #[test]
    fn facts_match_actual_python_helper() {
        let data = std::fs::read(std::path::Path::new(FIXTURES).join("Catalog.Тест.xml")).unwrap();
        let facts = extract_facts(&data).unwrap();
        let oracle: serde_json::Value = serde_json::from_slice(
            &std::fs::read(std::path::Path::new(FIXTURES).join("oracle-0.7.3.json")).unwrap(),
        )
        .unwrap();
        let blob = crate::index::encode_facts(&facts).unwrap();
        let mut decoded = String::new();
        std::io::Read::read_to_string(
            &mut flate2::read::ZlibDecoder::new(blob.as_slice()),
            &mut decoded,
        )
        .unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&decoded).unwrap(),
            oracle["facts"]
        );
    }
    #[test]
    fn refs_match_pinned_oracle() {
        let oracle: serde_json::Value = serde_json::from_slice(
            &std::fs::read(std::path::Path::new(FIXTURES).join("oracle-0.7.3.json")).unwrap(),
        )
        .unwrap();
        for row in oracle["references"].as_array().unwrap() {
            assert_eq!(
                serde_json::to_value(scan(row["text"].as_str().unwrap())).unwrap(),
                row["refs"],
                "{}",
                row["text"]
            );
        }
        for row in oracle["stable_uuid"].as_array().unwrap() {
            assert_eq!(
                stable_uuid(row["seed"].as_str().unwrap()),
                row["uuid"].as_str().unwrap()
            );
        }
    }
}
