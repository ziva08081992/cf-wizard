//! Static VALUE/ЗНАЧЕНИЕ profile. Query strings are included intentionally;
//! comment optionality is determined by a separate comment-free observation.
use crate::source_query::{TypePath, literal};
use cf_domain::fold;

fn identifier(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}
fn word(source: &str, start: usize) -> (&str, usize) {
    let end = source[start..]
        .char_indices()
        .find(|(_, c)| !identifier(*c))
        .map_or(source.len(), |(offset, _)| start + offset);
    (&source[start..end], end)
}
fn whitespace(source: &str, start: usize) -> usize {
    source[start..]
        .char_indices()
        .find(|(_, c)| !c.is_ascii_whitespace())
        .map_or(source.len(), |(offset, _)| start + offset)
}
pub fn module_values(source: &str) -> Vec<TypePath> {
    let mut output = vec![];
    let mut seen = std::collections::BTreeSet::new();
    let mut at = 0;
    while at < source.len() {
        let c = source[at..].chars().next().expect("boundary");
        if !identifier(c) {
            at += c.len_utf8();
            continue;
        }
        let (call, end) = word(source, at);
        let member = source[..at].ends_with('.');
        at = end;
        if member || !matches!(fold(call).as_str(), "value" | "значение") {
            continue;
        }
        let opening = whitespace(source, end);
        if source.as_bytes().get(opening) != Some(&b'(') {
            continue;
        }
        let mut cursor = whitespace(source, opening + 1);
        let mut parts = vec![];
        loop {
            let (part, end) = word(source, cursor);
            if part.is_empty() {
                break;
            }
            parts.push(part.to_owned());
            cursor = end;
            if source.as_bytes().get(cursor) != Some(&b'.') {
                break;
            }
            cursor += 1;
        }
        let close = whitespace(source, cursor);
        if source.as_bytes().get(close) == Some(&b')') {
            for value in literal(&parts) {
                if seen.insert(value.clone()) {
                    output.push(value);
                }
            }
        }
    }
    output
}
pub fn without_bsl_comments(source: &str) -> String {
    let mut output = String::with_capacity(source.len());
    let mut chars = source.chars().peekable();
    let mut quoted = false;
    while let Some(c) = chars.next() {
        if c == '"' {
            output.push(c);
            if quoted && chars.peek() == Some(&'"') {
                output.push(chars.next().expect("peek"));
            } else {
                quoted = !quoted;
            }
        } else if !quoted && c == '/' && chars.peek() == Some(&'/') {
            for c in chars.by_ref() {
                if c == '\n' {
                    output.push(c);
                    break;
                }
            }
        } else {
            output.push(c);
        }
    }
    output
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn literal_and_comment_controls_remain_unchanged() {
        let oracle: serde_json::Value =
            serde_json::from_slice(include_bytes!("../../../tests/fixtures/oracle-0.7.3.json"))
                .unwrap();
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
    }
    #[test]
    fn raw_query_strings_and_optional_comments_are_distinct() {
        let text = "x = \"VALUE(Enum.Choice.First) // still a string\"; // VALUE(Catalog.Items.Sample)\nObject.VALUE(Enum.Ignored.Decoy); OtherVALUE(Enum.Ignored.Decoy);";
        assert_eq!(
            module_values(text),
            vec![
                ("Enum.Choice".into(), vec![]),
                ("Enum.Choice".into(), vec!["First".into()]),
                ("Catalog.Items".into(), vec![]),
                (
                    "Catalog.Items".into(),
                    vec!["@predefined".into(), "Sample".into()]
                )
            ]
        );
        assert_eq!(
            module_values(&without_bsl_comments(text)),
            vec![
                ("Enum.Choice".into(), vec![]),
                ("Enum.Choice".into(), vec!["First".into()])
            ]
        );
    }
}
