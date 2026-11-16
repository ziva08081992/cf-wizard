//! Finite query profile expressed over a token stream and explicit bindings.
//! It deliberately does not claim general SDBL syntax or query execution.
use crate::kinds::{QUERY_KEYWORDS, QUERY_KINDS, TYPE_FOLDERS, VIRTUAL_TABLES};
use cf_domain::fold;
use std::collections::{BTreeMap, BTreeSet};
pub type TypePath = (String, Vec<String>);
pub type Fields = BTreeMap<String, Vec<TypePath>>;

#[derive(Debug)]
enum Token {
    Name(Vec<String>),
    Symbol(char),
}
struct Query {
    tokens: Vec<Token>,
    closing: BTreeMap<usize, usize>,
    opening: BTreeMap<usize, usize>,
}
fn ident(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}
fn lex(source: &str) -> Query {
    let chars: Vec<_> = source.chars().collect();
    let mut tokens = vec![];
    let mut at = 0;
    while at < chars.len() {
        let c = chars[at];
        if c == '"' {
            at += 1;
            while at < chars.len() {
                if chars[at] == '"' {
                    at += 1;
                    if chars.get(at) != Some(&'"') {
                        break;
                    }
                }
                at += 1;
            }
        } else if c == '/' && chars.get(at + 1) == Some(&'/') {
            while at < chars.len() && chars[at] != '\n' {
                at += 1;
            }
        } else if c == '&' {
            at += 1;
            while at < chars.len() && ident(chars[at]) {
                at += 1;
            }
        } else if ident(c) && !c.is_ascii_digit() {
            let mut parts = vec![];
            loop {
                let start = at;
                while at < chars.len() && ident(chars[at]) {
                    at += 1;
                }
                parts.push(chars[start..at].iter().collect());
                if chars.get(at) != Some(&'.') || !chars.get(at + 1).is_some_and(|c| ident(*c)) {
                    break;
                }
                at += 1;
            }
            tokens.push(Token::Name(parts));
        } else {
            if !c.is_whitespace() {
                tokens.push(Token::Symbol(c));
            }
            at += 1;
        }
    }
    let mut openings = vec![];
    let mut closing = BTreeMap::new();
    for (i, token) in tokens.iter().enumerate() {
        match token {
            Token::Symbol('(') => openings.push(i),
            Token::Symbol(')') => {
                if let Some(open) = openings.pop() {
                    closing.insert(open, i);
                }
            }
            _ => {}
        }
    }
    let opening = closing
        .iter()
        .map(|(open, close)| (*close, *open))
        .collect();
    Query {
        tokens,
        closing,
        opening,
    }
}
impl Query {
    fn parts(&self, i: usize) -> Option<&[String]> {
        match self.tokens.get(i) {
            Some(Token::Name(parts)) => Some(parts),
            _ => None,
        }
    }
    fn word(&self, i: usize) -> &str {
        self.parts(i)
            .filter(|p| p.len() == 1)
            .map_or("", |p| p[0].as_str())
    }
    fn is(&self, i: usize, symbol: char) -> bool {
        matches!(self.tokens.get(i), Some(Token::Symbol(c)) if *c == symbol)
    }
    fn alias(&self, i: usize) -> &str {
        if matches!(fold(self.word(i + 1)).as_str(), "как" | "as") {
            self.word(i + 2)
        } else {
            ""
        }
    }
    fn source_positions(&self) -> BTreeSet<usize> {
        // Each parenthesis owns its FROM-list state. Commas in function
        // arguments, SELECT fields and predicates outside FROM are not sources.
        let mut frames = vec![(false, false)];
        let mut output = BTreeSet::new();
        for (i, token) in self.tokens.iter().enumerate() {
            match token {
                Token::Symbol('(') => {
                    frames.last_mut().expect("root frame").1 = false;
                    frames.push((false, false));
                }
                Token::Symbol(')') => {
                    if frames.len() > 1 {
                        frames.pop();
                    }
                }
                Token::Symbol(';') => *frames.last_mut().expect("root frame") = (false, false),
                Token::Symbol(',') => {
                    let frame = frames.last_mut().expect("root frame");
                    frame.1 = frame.0;
                }
                Token::Name(parts) => {
                    let frame = frames.last_mut().expect("root frame");
                    let word = if parts.len() == 1 {
                        fold(&parts[0])
                    } else {
                        String::new()
                    };
                    match word.as_str() {
                        "from" | "из" | "join" | "соединение" => *frame = (true, true),
                        "select"
                        | "выбрать"
                        | "where"
                        | "где"
                        | "group"
                        | "сгруппировать"
                        | "having"
                        | "имеющие"
                        | "order"
                        | "упорядочить"
                        | "totals"
                        | "итоги"
                        | "union"
                        | "объединить"
                        | "into"
                        | "поместить" => *frame = (false, false),
                        _ if frame.1 => {
                            if parts.len() >= 2 && kind(&parts[0]).is_some() {
                                output.insert(i);
                            }
                            frame.1 = false;
                        }
                        _ => {}
                    }
                }
                _ => {}
            }
        }
        output
    }
}
fn kind(label: &str) -> Option<&'static str> {
    let label = fold(label);
    QUERY_KINDS
        .iter()
        .find(|(name, _)| fold(name) == label)
        .map(|(_, kind)| *kind)
        .or_else(|| {
            TYPE_FOLDERS
                .iter()
                .find(|(kind, _)| *kind != "Language" && fold(kind) == label)
                .map(|(kind, _)| *kind)
        })
}
fn insert(output: &mut Vec<TypePath>, value: TypePath) {
    if !output.contains(&value) {
        output.push(value);
    }
}
#[derive(Default)]
struct OrderedPaths {
    values: Vec<TypePath>,
    seen: BTreeSet<TypePath>,
}
impl OrderedPaths {
    fn add(&mut self, value: TypePath) {
        if self.seen.insert(value.clone()) {
            self.values.push(value);
        }
    }
}
fn bind(bindings: &mut Fields, label: &str, value: TypePath) {
    if !label.is_empty() {
        insert(bindings.entry(fold(label)).or_default(), value);
    }
}
pub fn literal(parts: &[String]) -> Vec<TypePath> {
    if parts.len() != 3 {
        return vec![];
    }
    let Some(kind) = kind(&parts[0]) else {
        return vec![];
    };
    let key = format!("{kind}.{}", parts[1]);
    let mut output = vec![(key.clone(), vec![])];
    if !matches!(fold(&parts[2]).as_str(), "пустаяссылка" | "emptyref") {
        output.push((
            key,
            if kind == "Enum" {
                vec![parts[2].clone()]
            } else {
                vec!["@predefined".into(), parts[2].clone()]
            },
        ));
    }
    output
}
pub struct QueryRead {
    pub paths: Vec<TypePath>,
    pub required: BTreeSet<String>,
}
#[cfg(test)]
pub fn query_refs(source: &str, fields: &mut Fields) -> Vec<TypePath> {
    read(source, fields).paths
}
pub fn read(source: &str, fields: &mut Fields) -> QueryRead {
    let query = lex(source);
    let source_positions = query.source_positions();
    let mut output = OrderedPaths::default();
    let mut bindings = Fields::new();
    let mut sources = OrderedPaths::default();
    let mut consumed = BTreeSet::new();
    let mut values = BTreeMap::<usize, Vec<TypePath>>::new();
    let mut required = BTreeSet::new();
    let labels: BTreeSet<_> = (0..query.tokens.len())
        .map(|i| fold(query.alias(i)))
        .filter(|s| !s.is_empty())
        .collect();
    // First bind source tables, typed expressions and literals. Field uses may
    // precede their FROM declarations, so they are resolved in a second pass.
    for i in 0..query.tokens.len() {
        let Some(parts) = query.parts(i).filter(|p| p.len() >= 2) else {
            continue;
        };
        let Some(kind) = kind(&parts[0]) else {
            continue;
        };
        let key = format!("{kind}.{}", parts[1]);
        let previous = if i > 0 {
            fold(query.word(i - 1))
        } else {
            String::new()
        };
        let call = if i >= 2 && query.is(i - 1, '(') {
            fold(query.word(i - 2))
        } else {
            String::new()
        };
        consumed.insert(i);
        if matches!(call.as_str(), "value" | "значение") {
            required.insert(key.clone());
            values.insert(i - 2, vec![(key.clone(), vec![])]);
            output.add((key.clone(), vec![]));
            if parts.len() >= 3 && !matches!(fold(&parts[2]).as_str(), "emptyref" | "пустаяссылка")
            {
                output.add((
                    key.clone(),
                    if kind == "Enum" {
                        vec![parts[2].clone()]
                    } else {
                        vec!["@predefined".into(), parts[2].clone()]
                    },
                ));
            }
            if query.is(i + 1, ')') {
                bind(fields, query.alias(i + 1), (key, vec![]));
            }
        } else if matches!(previous.as_str(), "как" | "as" | "ссылка" | "refs")
            || matches!(call.as_str(), "type" | "тип")
        {
            required.insert(key.clone());
            output.add((key.clone(), vec![]));
            if matches!(previous.as_str(), "как" | "as")
                && query.is(i + 1, ')')
                && query.is(i + 2, '.')
                && let Some(path) = query.parts(i + 3)
            {
                consumed.insert(i + 3);
                let value = (key.clone(), path.to_vec());
                output.add(value.clone());
                bind(fields, query.alias(i + 3), value.clone());
                if let Some(&open) = query.opening.get(&(i + 1))
                    && open > 0
                {
                    values.insert(open - 1, vec![value]);
                }
            }
        } else {
            if labels.contains(&fold(&parts[0])) && !source_positions.contains(&i) {
                consumed.remove(&i);
                continue;
            }
            if source_positions.contains(&i) {
                required.insert(key.clone());
            }
            let section = parts
                .get(2)
                .filter(|s| !VIRTUAL_TABLES.iter().any(|table| fold(table) == fold(s)))
                .map_or(vec![], |s| vec![s.clone()]);
            let value = (key, section);
            sources.add(value.clone());
            output.add(value.clone());
            let mut after = i + 1;
            if query.is(after, '(') {
                after = query
                    .closing
                    .get(&after)
                    .map_or(query.tokens.len(), |end| end + 1);
            }
            let next = query.word(after);
            let label = if matches!(fold(next).as_str(), "как" | "as")
                && !query.word(after + 1).is_empty()
            {
                query.word(after + 1)
            } else if !next.is_empty() && !QUERY_KEYWORDS.contains(&fold(next).as_str()) {
                next
            } else {
                parts.last().expect("nonempty")
            };
            bind(&mut bindings, label, value);
        }
    }
    for i in 0..query.tokens.len() {
        if consumed.contains(&i) {
            continue;
        }
        let Some(parts) = query.parts(i) else {
            continue;
        };
        let head = fold(&parts[0]);
        let mut paths = vec![];
        if let Some(bound) = bindings.get(&head) {
            if parts.len() == 1 && query.is(i + 1, '.') && query.is(i + 2, '*') {
                for (key, base) in bound.iter().filter(|(_, base)| base.is_empty()) {
                    let mut path = base.clone();
                    path.push("@all".into());
                    paths.push((key.clone(), path));
                }
            } else if parts.len() >= 2 {
                for (key, base) in bound {
                    let mut path = base.clone();
                    path.extend_from_slice(&parts[1..]);
                    paths.push((key.clone(), path));
                }
            }
        } else if parts.len() == 1 && QUERY_KEYWORDS.contains(&head.as_str()) {
            continue;
        } else {
            for (key, base) in &sources.values {
                let mut path = base.clone();
                path.extend_from_slice(parts);
                paths.push((key.clone(), path));
            }
            if parts.len() >= 2 {
                for (key, base) in &sources.values {
                    let mut path = base.clone();
                    path.extend_from_slice(&parts[1..]);
                    paths.push((key.clone(), path));
                }
            }
            for offset in 0..2.min(parts.len()) {
                if let Some(bound) = fields.get(&fold(&parts[offset])) {
                    for (key, base) in bound {
                        let mut path = base.clone();
                        path.extend_from_slice(&parts[offset + 1..]);
                        paths.push((key.clone(), path));
                    }
                }
            }
        }
        for value in &paths {
            output.add(value.clone());
        }
        for value in &paths {
            bind(fields, query.alias(i), value.clone());
        }
        values.insert(i, paths);
    }
    let mut cases: Vec<Vec<TypePath>> = vec![];
    for i in 0..query.tokens.len() {
        match fold(query.word(i)).as_str() {
            "выбор" | "case" => cases.push(vec![]),
            "тогда" | "иначе" | "then" | "else" => {
                if let Some(case) = cases.last_mut() {
                    case.extend(values.get(&(i + 1)).cloned().unwrap_or_default());
                }
            }
            "конец" | "end" => {
                if let Some(case) = cases.pop() {
                    for value in case {
                        bind(fields, query.alias(i), value);
                    }
                }
            }
            _ => {}
        }
    }
    QueryRead {
        paths: output.values,
        required,
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pinned_queries_remain_an_independent_compatibility_control() {
        let oracle: serde_json::Value =
            serde_json::from_slice(include_bytes!("../../../tests/fixtures/oracle-0.7.3.json"))
                .unwrap();
        for row in oracle["queries"].as_array().unwrap() {
            let mut fields = Fields::new();
            assert_eq!(
                serde_json::to_value(query_refs(row["query"].as_str().unwrap(), &mut fields))
                    .unwrap(),
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
    #[test]
    fn sources_literals_parameters_and_quotes_have_distinct_roles() {
        let mut fields = Fields::new();
        assert_eq!(
            query_refs(
                "SELECT R.Owner.Name FROM Catalog.Entries AS R // Document.Decoy\n",
                &mut fields
            ),
            vec![
                ("Catalog.Entries".into(), vec![]),
                (
                    "Catalog.Entries".into(),
                    vec!["Owner".into(), "Name".into()]
                )
            ]
        );
        assert_eq!(
            literal(&["Catalog".into(), "Currency".into(), "Rouble".into()]),
            vec![
                ("Catalog.Currency".into(), vec![]),
                (
                    "Catalog.Currency".into(),
                    vec!["@predefined".into(), "Rouble".into()]
                )
            ]
        );
        assert!(
            query_refs(
                "SELECT \"Catalog.Hidden\"\"Document.Hidden\", &Catalog.Parameter",
                &mut Fields::new()
            )
            .is_empty()
        );
    }
    #[test]
    fn case_literal_branches_bind_the_result_type() {
        let mut fields = Fields::new();
        query_refs(
            "SELECT CASE WHEN TRUE THEN VALUE(Catalog.Items.First) ELSE VALUE(Catalog.Items.Second) END AS Item",
            &mut fields,
        );
        assert_eq!(
            fields.get("item"),
            Some(&vec![("Catalog.Items".into(), vec![])])
        );
    }
    #[test]
    fn virtual_table_keywords_follow_the_query_case_contract() {
        let expected = vec![
            ("AccumulationRegister.Stock".into(), vec![]),
            ("AccumulationRegister.Stock".into(), vec!["Amount".into()]),
        ];
        for table in [
            "Balance",
            "balance",
            "BALANCE",
            "Остатки",
            "остатки",
            "ОСТАТКИ",
        ] {
            assert_eq!(
                query_refs(
                    &format!("SELECT R.Amount FROM AccumulationRegister.Stock.{table}() AS R"),
                    &mut Fields::new()
                ),
                expected
            );
        }
    }
    #[test]
    fn only_supported_static_slots_confirm_required_query_endpoints() {
        let result = read(
            "SELECT TYPE(Document.Required), VALUE(Enum.Required.First) FROM Catalog.Missing AS M JOIN Catalog.Other AS O ON TRUE",
            &mut Fields::new(),
        );
        assert_eq!(
            result.required,
            BTreeSet::from([
                "Document.Required".into(),
                "Enum.Required".into(),
                "Catalog.Missing".into(),
                "Catalog.Other".into()
            ])
        );
        assert!(
            read(
                "SELECT \"Catalog.String\", &Document.Parameter // Catalog.Comment",
                &mut Fields::new()
            )
            .required
            .is_empty()
        );
    }

    #[test]
    fn comma_separated_from_sources_are_required_but_other_commas_are_not() {
        for query in [
            "SELECT A.Ref, M.Ref FROM Catalog.Present AS A, Catalog.Missing AS M",
            "SELECT A.Ref FROM Catalog.Present A, Catalog.Missing M",
            "ВЫБРАТЬ A.Ссылка ИЗ Справочник.Present КАК A, Справочник.Missing КАК M",
        ] {
            assert_eq!(
                read(query, &mut Fields::new()).required,
                BTreeSet::from(["Catalog.Present".into(), "Catalog.Missing".into()])
            );
        }
        assert_eq!(
            read(
                "SELECT A.Ref FROM Catalog.FirstMissing A, Catalog.SecondMissing B",
                &mut Fields::new()
            )
            .required,
            BTreeSet::from([
                "Catalog.FirstMissing".into(),
                "Catalog.SecondMissing".into()
            ])
        );
        assert_eq!(read("SELECT Catalog.Weak, Document.Weak FROM Catalog.Present A WHERE A.X = 1, Catalog.Predicate",&mut Fields::new()).required,
            BTreeSet::from(["Catalog.Present".into()]));
        assert_eq!(read("SELECT R.Amount FROM AccumulationRegister.Stock.Balance(, Catalog.Parameter) R, (SELECT X.Ref FROM Catalog.Nested X) N",&mut Fields::new()).required,
            BTreeSet::from(["AccumulationRegister.Stock".into(),"Catalog.Nested".into()]));
    }
}
