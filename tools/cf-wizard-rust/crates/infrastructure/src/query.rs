//! Query-source, field-path and expression-alias analysis matching the pinned engine.
use crate::{
    kinds::{QUERY_KEYWORDS, QUERY_KINDS, TYPE_FOLDERS, VIRTUAL_TABLES},
    metadata::is_ident,
};
use cf_domain::fold;
use std::collections::{BTreeMap, BTreeSet};
pub type TypePath = (String, Vec<String>);
pub type Fields = BTreeMap<String, Vec<TypePath>>;
const SPACE: &[char] = &[' ', '\t', '\r', '\n', '\x0b', '\x0c'];
#[derive(Debug)]
struct Word {
    segments: Vec<String>,
    start: usize,
    end: usize,
}
struct Text {
    chars: Vec<char>,
    words: Vec<Word>,
    at: BTreeMap<usize, usize>,
    ends: BTreeMap<usize, usize>,
}
impl Text {
    fn new(source: &str) -> Self {
        let mut text = Self {
            chars: source.chars().collect(),
            words: vec![],
            at: BTreeMap::new(),
            ends: BTreeMap::new(),
        };
        let mut i = 0;
        while i < text.chars.len() {
            let c = text.chars[i];
            if c == '"' {
                i = text.skip_string(i);
                continue;
            }
            if c == '/' && text.chars.get(i + 1) == Some(&'/') {
                i = text.line_end(i);
                continue;
            }
            if c == '&' {
                while i < text.chars.len() && (text.chars[i] == '&' || is_ident(text.chars[i])) {
                    i += 1;
                }
                continue;
            }
            if !is_ident(c) || c.is_ascii_digit() {
                i += 1;
                continue;
            }
            let mut segments = vec![];
            let mut j = i;
            loop {
                let mut end = j;
                while end < text.chars.len() && is_ident(text.chars[end]) {
                    end += 1;
                }
                segments.push(text.chars[j..end].iter().collect());
                if text.chars.get(end) == Some(&'.')
                    && text.chars.get(end + 1).is_some_and(|&c| is_ident(c))
                {
                    j = end + 1;
                } else {
                    j = end;
                    break;
                }
            }
            let index = text.words.len();
            text.at.insert(i, index);
            text.ends.insert(j, index);
            text.words.push(Word {
                segments,
                start: i,
                end: j,
            });
            i = j;
        }
        text
    }
    fn line_end(&self, i: usize) -> usize {
        self.chars[i..]
            .iter()
            .position(|&c| c == '\n')
            .map_or(self.chars.len(), |p| i + p)
    }
    fn skip_string(&self, i: usize) -> usize {
        self.chars[i + 1..]
            .iter()
            .position(|&c| c == '"')
            .map_or(self.chars.len(), |p| i + p + 2)
    }
    fn skip_space(&self, mut i: usize) -> usize {
        while i < self.chars.len() {
            if SPACE.contains(&self.chars[i]) {
                i += 1;
            } else if self.chars[i] == '/' && self.chars.get(i + 1) == Some(&'/') {
                i = self.line_end(i);
            } else {
                break;
            }
        }
        i
    }
    fn skip_parens(&self, mut i: usize) -> usize {
        let mut depth = 0;
        while i < self.chars.len() {
            let c = self.chars[i];
            if c == '"' {
                i = self.skip_string(i);
                continue;
            }
            if c == '(' {
                depth += 1;
            } else if c == ')' {
                depth -= 1;
                if depth == 0 {
                    return i + 1;
                }
            }
            i += 1;
        }
        self.chars.len()
    }
    fn single(&self, index: Option<usize>) -> &str {
        index
            .and_then(|i| self.words.get(i))
            .filter(|w| w.segments.len() == 1)
            .map_or("", |w| w.segments[0].as_str())
    }
    fn word_at(&self, i: usize) -> Option<usize> {
        self.at.get(&i).copied()
    }
    fn named(&self, k: usize) -> String {
        let after = self.word_at(self.skip_space(self.words[k].end));
        if !matches!(fold(self.single(after)).as_str(), "как" | "as") {
            return String::new();
        }
        self.single(after.and_then(|i| self.word_at(self.skip_space(self.words[i].end))))
            .to_owned()
    }
    fn trim_end(&self, mut end: usize) -> usize {
        while end > 0 && SPACE.contains(&self.chars[end - 1]) {
            end -= 1;
        }
        end
    }
    fn word_before(&self, end: usize) -> String {
        let mut start = end;
        while start > 0 && is_ident(self.chars[start - 1]) {
            start -= 1;
        }
        fold(&self.chars[start..end].iter().collect::<String>())
    }
}
fn add(out: &mut Vec<TypePath>, seen: &mut BTreeSet<TypePath>, value: TypePath) {
    if seen.insert(value.clone()) {
        out.push(value);
    }
}
fn bind(fields: &mut Fields, name: &str, value: TypePath) {
    let entries = fields.entry(fold(name)).or_default();
    if !entries.contains(&value) {
        entries.push(value);
    }
}
fn kind(word: &str) -> Option<&'static str> {
    let lower = fold(word);
    QUERY_KINDS
        .iter()
        .find(|&&(label, _)| fold(label) == lower)
        .map(|&(_, kind)| kind)
        .or_else(|| {
            TYPE_FOLDERS
                .iter()
                .find(|&&(kind, _)| kind != "Language" && fold(kind) == lower)
                .map(|&(kind, _)| kind)
        })
}

pub fn query_refs(query: &str, fields: &mut Fields) -> Vec<TypePath> {
    let text = Text::new(query);
    let n = text.chars.len();
    let (mut out, mut alias, mut sources, mut seen, mut used) = (
        vec![],
        Fields::new(),
        Vec::<TypePath>::new(),
        BTreeSet::new(),
        BTreeSet::new(),
    );
    let mut value_of: BTreeMap<usize, Vec<TypePath>> = BTreeMap::new();
    let named_as: BTreeSet<_> = (0..text.words.len())
        .map(|k| text.named(k))
        .filter(|s| !s.is_empty())
        .map(|s| fold(&s))
        .collect();
    for (k, word) in text.words.iter().enumerate() {
        let seg = &word.segments;
        let Some(kind) = seg.get(1).and_then(|_| kind(&seg[0])) else {
            continue;
        };
        let before_end = text.trim_end(word.start);
        used.insert(k);
        let key = format!("{kind}.{}", seg[1]);
        let value_call = before_end > 0 && text.chars[before_end - 1] == '(' && {
            let e = text.trim_end(before_end - 1);
            let suffix = fold(
                &text.chars[e.saturating_sub(8)..e]
                    .iter()
                    .collect::<String>(),
            );
            suffix.ends_with("значение") || suffix.ends_with("value")
        };
        if value_call {
            add(&mut out, &mut seen, (key.clone(), vec![]));
            if seg.len() >= 3 && !matches!(fold(&seg[2]).as_str(), "пустаяссылка" | "emptyref")
            {
                add(
                    &mut out,
                    &mut seen,
                    (
                        key.clone(),
                        if kind == "Enum" {
                            vec![seg[2].clone()]
                        } else {
                            vec!["@predefined".into(), seg[2].clone()]
                        },
                    ),
                );
            }
            let close = text.skip_space(word.end);
            if text.chars.get(close) == Some(&')') {
                let after = text.word_at(text.skip_space(close + 1));
                if matches!(fold(text.single(after)).as_str(), "как" | "as") {
                    let name = text.single(
                        after.and_then(|i| text.word_at(text.skip_space(text.words[i].end))),
                    );
                    if !name.is_empty() {
                        bind(fields, name, (key, vec![]));
                    }
                }
            }
            continue;
        }
        let mut prev = text.word_before(before_end);
        if prev.is_empty() && before_end > 0 && text.chars[before_end - 1] == '(' {
            prev = format!("({}", text.word_before(text.trim_end(before_end - 1)));
        }
        if matches!(
            prev.as_str(),
            "как" | "as" | "ссылка" | "refs" | "(тип" | "(type"
        ) {
            add(&mut out, &mut seen, (key.clone(), vec![]));
            let close = text.skip_space(word.end);
            if matches!(prev.as_str(), "как" | "as")
                && close + 1 < n
                && text.chars[close] == ')'
                && text.chars[close + 1] == '.'
                && let Some(k2) = text.word_at(close + 2)
            {
                used.insert(k2);
                let path = text.words[k2].segments.clone();
                add(&mut out, &mut seen, (key.clone(), path.clone()));
                let name = text.named(k2);
                if !name.is_empty() {
                    bind(fields, &name, (key.clone(), path.clone()));
                }
                let (mut depth, mut opening) = (0i64, close as isize);
                while opening >= 0 {
                    let c = text.chars[opening as usize];
                    depth += i64::from(c == ')');
                    depth -= i64::from(c == '(');
                    if depth == 0 {
                        break;
                    }
                    opening -= 1;
                }
                if let Some(&first) = text.ends.get(&text.trim_end(opening.max(0) as usize)) {
                    value_of.insert(first, vec![(key, path)]);
                }
            }
            continue;
        }
        if named_as.contains(&fold(&seg[0]))
            && !matches!(prev.as_str(), "из" | "from" | "соединение" | "join")
        {
            used.remove(&k);
            continue;
        }
        let section = if seg.len() >= 3 && !VIRTUAL_TABLES.contains(&seg[2].as_str()) {
            vec![seg[2].clone()]
        } else {
            vec![]
        };
        let source = (key.clone(), section.clone());
        if !sources.contains(&source) {
            sources.push(source.clone());
        }
        add(&mut out, &mut seen, source.clone());
        let mut name = seg.last().cloned().unwrap_or_default();
        let mut pos = text.skip_space(word.end);
        if text.chars.get(pos) == Some(&'(') {
            pos = text.skip_space(text.skip_parens(pos));
        }
        let next = text.word_at(pos);
        let next_word = text.single(next);
        if !next_word.is_empty() && matches!(fold(next_word).as_str(), "как" | "as") {
            let following =
                text.single(next.and_then(|i| text.word_at(text.skip_space(text.words[i].end))));
            if !following.is_empty() {
                name = following.into();
            }
        } else if !next_word.is_empty() && !QUERY_KEYWORDS.contains(&fold(next_word).as_str()) {
            name = next_word.into();
        }
        bind(&mut alias, &name, source);
    }
    for (k, word) in text.words.iter().enumerate() {
        if used.contains(&k) {
            continue;
        }
        let seg = &word.segments;
        let head = fold(&seg[0]);
        let mut paths: Vec<TypePath> = vec![];
        if let Some(bound) = alias.get(&head) {
            if seg.len() == 1
                && text.chars.get(word.end) == Some(&'.')
                && text.chars.get(word.end + 1) == Some(&'*')
            {
                paths.extend(bound.iter().filter(|(_, section)| section.is_empty()).map(
                    |(key, section)| {
                        let mut p = section.clone();
                        p.push("@all".into());
                        (key.clone(), p)
                    },
                ));
            } else if seg.len() >= 2 {
                paths.extend(bound.iter().map(|(key, section)| {
                    let mut p = section.clone();
                    p.extend_from_slice(&seg[1..]);
                    (key.clone(), p)
                }));
            }
        } else if seg.len() == 1 && QUERY_KEYWORDS.contains(&head.as_str()) {
            continue;
        } else {
            paths.extend(sources.iter().map(|(key, section)| {
                let mut p = section.clone();
                p.extend_from_slice(seg);
                (key.clone(), p)
            }));
            if seg.len() >= 2 {
                paths.extend(sources.iter().map(|(key, section)| {
                    let mut p = section.clone();
                    p.extend_from_slice(&seg[1..]);
                    (key.clone(), p)
                }));
            }
            for at in [0, 1] {
                if at < seg.len()
                    && let Some(bound) = fields.get(&fold(&seg[at]))
                {
                    paths.extend(bound.iter().map(|(key, path)| {
                        let mut p = path.clone();
                        p.extend_from_slice(&seg[at + 1..]);
                        (key.clone(), p)
                    }));
                }
            }
        }
        for value in &paths {
            add(&mut out, &mut seen, value.clone());
        }
        value_of.insert(k, paths.clone());
        let name = text.named(k);
        if !name.is_empty() {
            for value in paths {
                bind(fields, &name, value);
            }
        }
    }
    let mut cases: Vec<Vec<TypePath>> = vec![];
    for (k, word) in text.words.iter().enumerate() {
        let w = if word.segments.len() == 1 {
            fold(&word.segments[0])
        } else {
            String::new()
        };
        if matches!(w.as_str(), "выбор" | "case") {
            cases.push(vec![]);
        } else if matches!(w.as_str(), "тогда" | "иначе" | "then" | "else")
            && k + 1 < text.words.len()
        {
            if let Some(case) = cases.last_mut() {
                case.extend(value_of.get(&(k + 1)).cloned().unwrap_or_default());
            }
        } else if matches!(w.as_str(), "конец" | "end")
            && let Some(paths) = cases.pop()
        {
            let name = text.named(k);
            if !name.is_empty() {
                for value in paths {
                    bind(fields, &name, value);
                }
            }
        }
    }
    out
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exact_results_match_python_queries() {
        let path = std::path::Path::new(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/fixtures/oracle-0.7.3.json"
        ));
        let oracle: serde_json::Value =
            serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        for row in oracle["queries"].as_array().unwrap() {
            let mut fields = Fields::new();
            let actual = query_refs(row["query"].as_str().unwrap(), &mut fields);
            assert_eq!(
                serde_json::to_value(actual).unwrap(),
                row["refs"],
                "{}",
                row["query"]
            );
            assert_eq!(
                serde_json::to_value(fields).unwrap(),
                row["fields"],
                "{}",
                row["query"]
            );
        }
    }
}
