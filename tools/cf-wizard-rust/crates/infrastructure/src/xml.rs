//! Source spans permit surgical edits without reserialising Designer XML.
use cf_domain::{Error, Result};
use std::{ops::Range, sync::Arc};
use xmlparser::{ElementEnd, Token, Tokenizer};

#[derive(Clone, Debug)]
pub struct Attribute {
    pub name: String,
    pub value: String,
    pub raw: String,
}
#[derive(Clone, Debug)]
pub struct Element {
    pub name: String,
    pub local: String,
    pub attributes: Vec<Attribute>,
    pub children: Vec<usize>,
    pub span: Range<usize>,
    pub inner: Range<usize>,
    pub parent: Option<usize>,
    pub text: Vec<(Range<usize>, bool)>,
}
#[derive(Clone, Debug)]
pub struct Xml {
    pub source: Arc<String>,
    pub elements: Vec<Element>,
    pub root: usize,
}
pub(crate) enum XmlInput<'a> {
    Bytes(&'a [u8]),
    Shared(Arc<String>),
}

impl XmlInput<'_> {
    pub(crate) fn len(&self) -> usize {
        match self {
            Self::Bytes(bytes) => bytes.len(),
            Self::Shared(text) => text.len(),
        }
    }

    fn into_source(self) -> Result<Arc<String>> {
        match self {
            Self::Shared(text) => Ok(text),
            Self::Bytes(bytes) => {
                let text = std::str::from_utf8(bytes).map_err(crate::external)?;
                Ok(Arc::new(text.to_owned()))
            }
        }
    }
}

#[derive(Clone, Debug)]
pub struct Edit {
    pub range: Range<usize>,
    pub replacement: String,
}
fn invalid(message: impl Into<String>) -> Error {
    Error::Invalid(message.into())
}
fn qualified(prefix: &str, local: &str) -> String {
    if prefix.is_empty() {
        local.into()
    } else {
        format!("{prefix}:{local}")
    }
}
pub fn unescape(text: &str) -> Result<String> {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find('&') {
        out.push_str(&rest[..start]);
        let tail = &rest[start + 1..];
        let end = tail
            .find(';')
            .ok_or_else(|| invalid("Незавершённая XML entity"))?;
        let entity = &tail[..end];
        let value = match entity {
            "amp" => '&',
            "lt" => '<',
            "gt" => '>',
            "quot" => '"',
            "apos" => '\'',
            _ => {
                let number = if let Some(hex) = entity.strip_prefix("#x") {
                    u32::from_str_radix(hex, 16).ok()
                } else if let Some(decimal) = entity.strip_prefix('#') {
                    decimal.parse::<u32>().ok()
                } else {
                    None
                };
                let ch = number
                    .and_then(char::from_u32)
                    .ok_or_else(|| invalid("Недопустимая XML entity"))?;
                if !(matches!(ch, '\t' | '\n' | '\r')
                    || ch as u32 >= 0x20 && !matches!(ch, '\u{fffe}' | '\u{ffff}'))
                {
                    return Err(invalid("Недопустимый XML символ"));
                }
                ch
            }
        };
        out.push(value);
        rest = &tail[end + 1..];
    }
    out.push_str(rest);
    Ok(out)
}
pub fn escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}
impl Xml {
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        Self::parse_input(XmlInput::Bytes(bytes))
    }

    pub(crate) fn parse_input(input: XmlInput<'_>) -> Result<Self> {
        let source = input.into_source()?;
        if source.contains("<!DOCTYPE") || source.contains("<!ENTITY") {
            return Err(invalid("DTD и внешние XML entities не поддерживаются"));
        }
        let mut elements: Vec<Element> = vec![];
        let mut stack: Vec<usize> = vec![];
        let mut root = None;
        for token in Tokenizer::from(source.as_str()) {
            match token.map_err(crate::external)? {
                Token::ElementStart {
                    prefix,
                    local,
                    span,
                } => {
                    if stack.len() >= 256 {
                        return Err(invalid("Слишком глубокое XML-дерево"));
                    }
                    let parent = stack.last().copied();
                    let id = elements.len();
                    if let Some(p) = parent {
                        elements[p].children.push(id);
                    } else if root.replace(id).is_some() {
                        return Err(invalid("Несколько корней XML"));
                    }
                    elements.push(Element {
                        name: qualified(prefix.as_str(), local.as_str()),
                        local: local.as_str().into(),
                        attributes: vec![],
                        children: vec![],
                        span: span.start()..span.end(),
                        inner: span.end()..span.end(),
                        parent,
                        text: vec![],
                    });
                    stack.push(id);
                }
                Token::Attribute {
                    prefix,
                    local,
                    value,
                    ..
                } => {
                    let id = *stack
                        .last()
                        .ok_or_else(|| invalid("Атрибут без элемента"))?;
                    let name = qualified(prefix.as_str(), local.as_str());
                    if elements[id].attributes.iter().any(|a| a.name == name) {
                        return Err(invalid("Повтор XML-атрибута"));
                    }
                    elements[id].attributes.push(Attribute {
                        name,
                        value: unescape(value.as_str())?,
                        raw: value.as_str().into(),
                    });
                }
                Token::ElementEnd { end, span } => {
                    let id = *stack
                        .last()
                        .ok_or_else(|| invalid("Закрытие без XML-элемента"))?;
                    match end {
                        ElementEnd::Open => {
                            elements[id].inner.start = span.end();
                        }
                        ElementEnd::Empty => {
                            elements[id].span.end = span.end();
                            elements[id].inner = span.start()..span.start();
                            stack.pop();
                        }
                        ElementEnd::Close(prefix, local) => {
                            if elements[id].name != qualified(prefix.as_str(), local.as_str()) {
                                return Err(invalid("Несогласованные XML-теги"));
                            }
                            elements[id].inner.end = span.start();
                            elements[id].span.end = span.end();
                            stack.pop();
                        }
                    }
                }
                Token::Text { text } => {
                    if let Some(&id) = stack.last() {
                        elements[id].text.push((text.start()..text.end(), false));
                    }
                }
                Token::Cdata { text, .. } => {
                    if let Some(&id) = stack.last() {
                        elements[id].text.push((text.start()..text.end(), true));
                    }
                }
                _ => {}
            }
        }
        if !stack.is_empty() {
            return Err(invalid("Незакрытый XML-элемент"));
        }
        Ok(Self {
            source,
            elements,
            root: root.ok_or_else(|| invalid("Нет корня XML"))?,
        })
    }
    pub fn child(&self, id: usize, local: &str) -> Option<usize> {
        self.elements[id]
            .children
            .iter()
            .copied()
            .find(|&i| self.elements[i].local == local)
    }
    pub fn attr(&self, id: usize, name: &str) -> Option<&str> {
        self.elements[id]
            .attributes
            .iter()
            .find(|a| a.name == name)
            .map(|a| a.value.as_str())
    }
    pub fn text(&self, id: usize) -> Result<String> {
        let mut out = String::new();
        for (range, cdata) in &self.elements[id].text {
            if *cdata {
                out.push_str(&self.source[range.clone()]);
            } else {
                out.push_str(&unescape(&self.source[range.clone()])?);
            }
        }
        Ok(out)
    }
    pub fn value(&self, id: usize, local: &str) -> Result<String> {
        self.child(id, local).map_or(Ok(String::new()), |n| {
            self.text(n).map(|s| s.trim().to_owned())
        })
    }
    pub fn raw(&self, id: usize) -> &str {
        &self.source[self.elements[id].span.clone()]
    }
    pub fn descendants(&self, id: usize) -> Vec<usize> {
        let mut result = vec![];
        let mut todo = vec![id];
        while let Some(node) = todo.pop() {
            result.push(node);
            todo.extend(self.elements[node].children.iter().rev());
        }
        result
    }
    pub fn apply(&self, mut edits: Vec<Edit>) -> Result<String> {
        edits.sort_by_key(|e| (e.range.start, e.range.end));
        let mut result = String::new();
        let mut cursor = 0;
        for edit in edits {
            if edit.range.start < cursor
                || edit.range.end > self.source.len()
                || edit.range.start > edit.range.end
                || !self.source.is_char_boundary(edit.range.start)
                || !self.source.is_char_boundary(edit.range.end)
            {
                return Err(invalid("Пересекающиеся или некорректные XML edits"));
            }
            result.push_str(&self.source[cursor..edit.range.start]);
            result.push_str(&edit.replacement);
            cursor = edit.range.end;
        }
        result.push_str(&self.source[cursor..]);
        Ok(result)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn preserves_original_bytes_outside_edit() {
        let input = "\u{feff}<?xml version=\"1.0\" encoding=\"UTF-8\"?>\r\n<m:Root xmlns:m=\"urn:test\" uuid=\"abc\">\r\n\t<m:Name>Тест &amp; мир</m:Name><Empty />\r\n</m:Root>";
        let shared = Arc::new(input.to_owned());
        let xml = Xml::parse_input(XmlInput::Shared(Arc::clone(&shared))).unwrap();
        assert!(Arc::ptr_eq(&xml.source, &shared));
        let name = xml.child(xml.root, "Name").unwrap();
        assert_eq!(xml.text(name).unwrap(), "Тест & мир");
        let changed = xml
            .apply(vec![Edit {
                range: xml.elements[name].inner.clone(),
                replacement: "Новый".into(),
            }])
            .unwrap();
        assert_eq!(changed, input.replace("Тест &amp; мир", "Новый"));
        assert_eq!(xml.apply(vec![]).unwrap().as_bytes(), input.as_bytes());
    }
    #[test]
    fn rejects_dtd_mismatched_tags_and_overlap() {
        for source in ["<!DOCTYPE a><a/>", "<a><b></a></b>", "<a>"] {
            assert!(Xml::parse(source.as_bytes()).is_err(), "{source}");
        }
        let xml = Xml::parse(b"<a><b/></a>").unwrap();
        assert!(
            xml.apply(vec![
                Edit {
                    range: 0..8,
                    replacement: String::new()
                },
                Edit {
                    range: 3..5,
                    replacement: String::new()
                }
            ])
            .is_err()
        );
    }
    #[test]
    fn cdata_and_numeric_entities() {
        let xml = Xml::parse("<a>А<![CDATA[<&>]]>&#x451;&#1103;</a>".as_bytes()).unwrap();
        assert_eq!(xml.text(xml.root).unwrap(), "А<&>ёя");
    }
}
