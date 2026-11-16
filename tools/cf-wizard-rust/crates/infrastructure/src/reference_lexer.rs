//! Finite metadata reference grammar shared by source facets.
use crate::kinds::{MEMBER_KEYWORDS, TYPE_FOLDERS};
use cf_domain::source_facts::ReferenceTarget;

fn ident(character: char) -> bool {
    character.is_ascii_alphanumeric() || character == '_' || character as u32 >= 128
}
fn vocabulary(token: &str) -> Option<(&'static str, bool)> {
    if let Some(&(kind, _)) = TYPE_FOLDERS.iter().find(|&&(kind, _)| kind == token) {
        return Some((kind, true));
    }
    if token == "Characteristic" {
        return Some(("ChartOfCharacteristicTypes", false));
    }
    TYPE_FOLDERS
        .iter()
        .filter_map(|&(kind, _)| {
            token
                .strip_prefix(kind)
                .filter(|suffix| suffix.starts_with(|ch: char| ch.is_ascii_uppercase()))
                .map(|_| (kind, false))
        })
        .max_by_key(|(kind, _)| kind.len())
}
/// Finite metadata-path lexer; it does not interpret BSL or execute a query.
pub(crate) fn references(text: &str) -> Vec<ReferenceTarget> {
    let mut result = vec![];
    let mut cursor = 0;
    while cursor < text.len() {
        let next = text[cursor..].chars().next().expect("character boundary");
        if !ident(next) {
            cursor += next.len_utf8();
            continue;
        }
        let start = cursor;
        while cursor < text.len() && text[cursor..].starts_with(ident) {
            cursor += text[cursor..].chars().next().expect("character").len_utf8();
        }
        let word_end = cursor;
        let token = &text[start..word_end];
        if token == "style" && text[cursor..].starts_with(':') {
            let name_start = cursor + 1;
            let mut name_end = name_start;
            while name_end < text.len() && text[name_end..].starts_with(ident) {
                name_end += text[name_end..]
                    .chars()
                    .next()
                    .expect("character")
                    .len_utf8();
            }
            if name_end > name_start {
                result.push(ReferenceTarget {
                    kind: "StyleItem".into(),
                    name: text[name_start..name_end].into(),
                    ..ReferenceTarget::default()
                });
            }
        }
        let mut segments = vec![];
        while text[cursor..].starts_with('.') {
            let part_start = cursor + 1;
            if part_start == text.len() || !text[part_start..].starts_with(ident) {
                break;
            }
            cursor = part_start;
            while cursor < text.len() && text[cursor..].starts_with(ident) {
                cursor += text[cursor..].chars().next().expect("character").len_utf8();
            }
            segments.push(&text[part_start..cursor]);
        }
        if start > 0 && text[..start].ends_with('.') {
            continue;
        }
        if !token.starts_with(|ch: char| ch.is_ascii_uppercase()) || segments.is_empty() {
            continue;
        }
        let Some((kind, exact)) = vocabulary(token) else {
            continue;
        };
        let mut reference = ReferenceTarget {
            kind: kind.into(),
            name: segments[0].into(),
            ..ReferenceTarget::default()
        };
        if exact && segments.len() >= 3 && MEMBER_KEYWORDS.contains(&segments[1]) {
            reference.member = segments[1].into();
            reference.member_name = segments[2].into();
        } else if exact && segments.get(1) == Some(&"StandardCommand") && segments.len() >= 3 {
            reference.member = "StandardCommand".into();
            reference.member_name = segments[2].into();
            if segments[2] == "OpenByValue"
                && let Some(value) = segments.get(3)
            {
                result.push(ReferenceTarget {
                    member: "*".into(),
                    member_name: (*value).into(),
                    ..reference.clone()
                });
            }
        } else if exact
            && kind != "CommonModule"
            && segments.len() == 2
            && !segments[1].starts_with("Standard")
            && !MEMBER_KEYWORDS.contains(&segments[1])
            && segments[1] != "EmptyRef"
        {
            reference.predefined = segments[1].into();
        } else if !exact
            && token.contains("TabularSection")
            && let Some(name) = segments.get(1)
        {
            reference.member = "TabularSection".into();
            reference.member_name = (*name).into();
        }
        result.push(reference);
    }
    result
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exported_module_routine_is_not_a_predefined_data_value() {
        let text = "CommonModule.Events.OnWrite";
        let old = crate::metadata::scan(text);
        assert_eq!(
            old[0].predefined, "OnWrite",
            "document the inherited classification"
        );
        let actual = references(text);
        assert_eq!(actual.len(), 1);
        assert_eq!(actual[0].key(), "CommonModule.Events");
        assert!(
            actual[0].predefined.is_empty(),
            "the routine must not request Ext/Predefined.xml"
        );
        assert_eq!(
            references("Catalog.Entities.PredefinedEntry")[0].predefined,
            "PredefinedEntry"
        );
    }
}
