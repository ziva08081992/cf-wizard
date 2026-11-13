//! Managed-form paths and BSL VALUE references; not a general BSL call graph.
use crate::{
    metadata::{child, is_ident, scan},
    query::{Fields, TypePath, query_refs},
    xml::Xml,
};
use cf_domain::{Result, fold};
use std::collections::{BTreeMap, BTreeSet};
pub type FormPath = (BTreeSet<String>, Vec<String>);
const SPACE: &[char] = &[' ', '\t', '\r', '\n', '\x0b', '\x0c'];

pub fn module_values(text: &str) -> Vec<TypePath> {
    let chars: Vec<_> = text.chars().collect();
    let mut out = vec![];
    for i in 0..chars.len() {
        if chars[i] != '(' {
            continue;
        }
        let mut b = i;
        while b > 0 && SPACE.contains(&chars[b - 1]) {
            b -= 1;
        }
        let mut a = b;
        while a > 0 && is_ident(chars[a - 1]) {
            a -= 1;
        }
        let call = fold(&chars[a..b].iter().collect::<String>());
        if !matches!(call.as_str(), "значение" | "value") || a > 0 && chars[a - 1] == '.' {
            continue;
        }
        let mut j = i + 1;
        while j < chars.len() && SPACE.contains(&chars[j]) {
            j += 1;
        }
        let mut end = j;
        while end < chars.len() && (is_ident(chars[end]) || chars[end] == '.') {
            end += 1;
        }
        let mut close = end;
        while close < chars.len() && SPACE.contains(&chars[close]) {
            close += 1;
        }
        if chars.get(close) == Some(&')')
            && chars[j..end].iter().filter(|&&c| c == '.').count() == 2
        {
            let expression = chars[j..end].iter().collect::<String>();
            for value in query_refs(&format!("ЗНАЧЕНИЕ({expression})"), &mut Fields::new())
            {
                if !out.contains(&value) {
                    out.push(value);
                }
            }
        }
    }
    out
}
pub fn without_bsl_comments(text: &str) -> String {
    let bytes = text.as_bytes();
    let (mut quoted, mut start, mut i) = (false, 0, 0);
    let mut out = String::new();
    while i < bytes.len() {
        if bytes[i] == b'"' {
            if quoted && bytes.get(i + 1) == Some(&b'"') {
                i += 2;
                continue;
            }
            quoted = !quoted;
        } else if !quoted && bytes[i] == b'/' && bytes.get(i + 1) == Some(&b'/') {
            out.push_str(&text[start..i]);
            let Some(offset) = bytes[i..].iter().position(|&b| b == b'\n') else {
                return out;
            };
            i += offset;
            start = i;
        }
        i += 1;
    }
    out.push_str(&text[start..]);
    out
}
pub fn xml_text(text: &str) -> String {
    let entities = [
        ("&lt;", "<"),
        ("&gt;", ">"),
        ("&amp;", "&"),
        ("&quot;", "\""),
        ("&apos;", "'"),
    ];
    let mut result = String::new();
    let mut i = 0;
    while i < text.len() {
        if text.as_bytes()[i] == b'&'
            && let Some((entity, value)) = entities.iter().find(|(e, _)| text[i..].starts_with(e))
        {
            result.push_str(value);
            i += entity.len();
            continue;
        }
        let c = text[i..].chars().next().expect("valid UTF8 boundary");
        result.push(c);
        i += c.len_utf8();
    }
    result
}
fn raw_attribute(xml: &Xml, id: usize, name: &str) -> String {
    let node = &xml.elements[id];
    let open = &xml.source[node.span.start..node.inner.start];
    let needle = format!(" {name}=\"");
    open.find(&needle)
        .and_then(|start| {
            let tail = &open[start + needle.len()..];
            tail.find('"').map(|end| tail[..end].to_owned())
        })
        .unwrap_or_default()
}
fn types(xml: &Xml, id: usize) -> BTreeSet<String> {
    scan(&xml.source[xml.elements[id].inner.clone()])
        .iter()
        .map(|r| r.key())
        .collect()
}
pub fn form_paths(form: &str) -> Result<Vec<FormPath>> {
    let xml = Xml::parse(form.as_bytes())?;
    let mut out = vec![];
    if xml.elements[xml.root].name != "Form" {
        return Ok(out);
    }
    let (mut attrs, mut columns, mut lists) = (
        BTreeMap::<String, BTreeSet<String>>::new(),
        BTreeMap::<String, BTreeSet<String>>::new(),
        BTreeMap::<String, Fields>::new(),
    );
    if let Some(attributes) = child(&xml, xml.root, "Attributes") {
        for &attr in &xml.elements[attributes].children {
            let name = raw_attribute(&xml, attr, "name");
            let mut kinds = BTreeSet::new();
            if let Some(t) = child(&xml, attr, "Type") {
                kinds.extend(types(&xml, t));
                if xml.source[xml.elements[t].inner.clone()].contains("cfg:ConstantsSet<") {
                    kinds.insert("@constants".into());
                }
            }
            if let Some(cols) = child(&xml, attr, "Columns").filter(|_| !name.is_empty()) {
                for &column in &xml.elements[cols].children {
                    let c = child(&xml, column, "Type")
                        .map(|t| types(&xml, t))
                        .unwrap_or_default();
                    let label = raw_attribute(&xml, column, "name");
                    if !label.is_empty() && !c.is_empty() {
                        columns.insert(format!("{name}.{label}"), c);
                    }
                }
            }
            if let Some(settings) = child(&xml, attr, "Settings") {
                if let Some(main) = child(&xml, settings, "MainTable") {
                    kinds.extend(types(&xml, main));
                }
                if let Some(query) = child(&xml, settings, "QueryText") {
                    let text = xml_text(&xml.source[xml.elements[query].inner.clone()]);
                    let mut unnamed = Fields::new();
                    let named = if name.is_empty() {
                        &mut unnamed
                    } else {
                        lists.entry(name.clone()).or_default()
                    };
                    out.extend(
                        query_refs(&text, named)
                            .into_iter()
                            .map(|(key, path)| (BTreeSet::from([key]), path)),
                    );
                }
            }
            if !name.is_empty() && !kinds.is_empty() {
                attrs.insert(name, kinds);
            }
        }
    }
    let mut item_paths = BTreeMap::new();
    let mut from = 0;
    while let Some(offset) = form[from..].find("<DataPath>") {
        let at = from + offset;
        let end = form[at..].find("</DataPath>").map(|e| at + e);
        let named = form[..(at + 7)].rfind(" name=\"");
        if let (Some(end), Some(named)) = (end, named) {
            let start = named + 7;
            let name_end = form[start..].find('"').map_or(form.len(), |q| start + q);
            item_paths.insert(
                form[start..name_end].to_owned(),
                form[at + 10..end].to_owned(),
            );
        }
        from = at + 1;
    }
    let mut texts = vec![];
    let mut from = 0;
    while let Some(offset) = form[from..].find('>') {
        let gt = from + offset;
        let Some(lt) = form[gt..].find('<').map(|p| gt + p) else {
            break;
        };
        texts.push(&form[gt + 1..lt]);
        from = gt + 1;
    }
    let mut from = 0;
    while let Some(offset) = form[from..].find("=\"") {
        let quote = from + offset;
        let Some(end) = form[quote + 2..].find('"').map(|p| quote + 2 + p) else {
            break;
        };
        texts.push(&form[quote + 2..end]);
        from = quote + 2;
    }
    let mut seen = BTreeSet::new();
    for text in texts {
        if text.len() < 3
            || !text.contains('.')
            || text.starts_with('.')
            || text.ends_with('.')
            || !text.chars().all(|c| is_ident(c) || c == '.')
            || !seen.insert(text)
        {
            continue;
        }
        let mut seg: Vec<String> = text.split('.').map(str::to_owned).collect();
        if seg.len() >= 4
            && seg[0] == "Items"
            && seg[2] == "CurrentData"
            && let Some(path) = item_paths.get(&seg[1])
        {
            let mut rewritten: Vec<_> = path.split('.').map(str::to_owned).collect();
            rewritten.extend_from_slice(&seg[3..]);
            seg = rewritten;
        }
        if seg.len() >= 3 {
            if let Some(fields) = lists.get(&seg[0])
                && let Some(paths) = fields.get(&fold(&seg[1]))
            {
                out.extend(paths.iter().map(|(key, path)| {
                    let mut p = path.clone();
                    p.extend_from_slice(&seg[2..]);
                    (BTreeSet::from([key.clone()]), p)
                }));
            }
            if let Some(types) = columns.get(&format!("{}.{}", seg[0], seg[1])) {
                out.push((types.clone(), seg[2..].to_vec()));
            }
        }
        if seg.len() >= 2
            && let Some(types) = attrs.get(&seg[0])
        {
            out.push((types.clone(), seg[1..].to_vec()));
        }
    }
    Ok(out)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn matches_pinned_bsl_and_form_oracles() {
        let path = std::path::Path::new(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/fixtures/oracle-0.7.3.json"
        ));
        let oracle: serde_json::Value =
            serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        for row in oracle["modules"].as_array().unwrap() {
            let text = row["text"].as_str().unwrap();
            assert_eq!(
                serde_json::to_value(module_values(text)).unwrap(),
                row["refs"],
                "{text}"
            );
            assert_eq!(
                without_bsl_comments(text),
                row["without_comments"].as_str().unwrap()
            );
        }
        for row in oracle["forms"].as_array().unwrap() {
            let text = row["xml"].as_str().unwrap();
            assert_eq!(
                serde_json::to_value(form_paths(text).unwrap()).unwrap(),
                row["paths"],
                "{text}"
            );
        }
    }
}
