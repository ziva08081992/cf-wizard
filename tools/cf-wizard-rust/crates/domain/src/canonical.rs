//! Compatibility with Python's ensure_ascii compact JSON and pinned Git manifests.
use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::fmt::Write;
use unicode_casefold::UnicodeCaseFold;

pub fn ascii_json(value: &impl Serialize) -> Result<String> {
    let json = serde_json::to_string(value).map_err(|e| Error::Invalid(e.to_string()))?;
    let mut out = String::with_capacity(json.len());
    for c in json.chars() {
        if c.is_ascii() {
            out.push(c);
        } else {
            for unit in c.encode_utf16(&mut [0; 2]) {
                write!(&mut out, "\\u{unit:04x}").expect("formatting into String");
            }
        }
    }
    Ok(out)
}
pub fn sha256(data: &[u8]) -> String {
    hex::encode(Sha256::digest(data))
}
pub fn key(parts: &[&str]) -> String {
    sha256(parts.join("\0").as_bytes())[..24].to_owned()
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
pub struct InputEntry {
    pub path: String,
    pub oid: String,
    pub size: u64,
    pub owner: String,
    pub roles: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GitInputs {
    pub source_key: String,
    pub revision: String,
    pub entries: Vec<InputEntry>,
}
impl GitInputs {
    pub fn validate(&self) -> Result<()> {
        fn oid(value: &str) -> bool {
            matches!(value.len(), 40 | 64)
                && value
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        }
        if !self.source_key.starts_with("git:") || !oid(&self.revision) {
            return Err(Error::Invalid(
                "Не подтверждён источник Git-манифеста".into(),
            ));
        }
        let mut paths = BTreeSet::new();
        let mut configurations = 0;
        for entry in &self.entries {
            let roles: Vec<_> = entry.roles.split(',').collect();
            if !oid(&entry.oid)
                || roles.is_empty()
                || roles
                    .iter()
                    .any(|r| !matches!(*r, "configuration" | "descriptor" | "registrar"))
                || roles.windows(2).any(|v| v[0] >= v[1])
                || entry.path.starts_with('/')
                || entry.path.contains('\\')
                || entry.path.split('/').any(|p| matches!(p, "" | "." | ".."))
            {
                return Err(Error::Invalid("Некорректные входы индекса Git".into()));
            }
            if !paths.insert(entry.path.case_fold().collect::<String>()) {
                return Err(Error::Invalid("Коллизия путей в Git-манифесте".into()));
            }
            if roles.contains(&"configuration") {
                if entry.path != "Configuration.xml" {
                    return Err(Error::Invalid("Некорректный путь Configuration.xml".into()));
                }
                configurations += 1;
            }
        }
        if configurations != 1 {
            return Err(Error::Invalid(
                "Во входах индекса должен быть один Configuration.xml".into(),
            ));
        }
        Ok(())
    }
    pub fn fingerprint(&self) -> Result<String> {
        self.validate()?;
        let mut entries = self.entries.clone();
        entries.sort();
        if entries.windows(2).any(|pair| pair[0].path == pair[1].path) {
            return Err(Error::Invalid("Повтор пути в Git-манифесте".into()));
        }
        let rows: Vec<_> = entries
            .iter()
            .map(|v| (&v.path, &v.oid, v.size, &v.owner, &v.roles))
            .collect();
        Ok(sha256(
            ascii_json(&("1", &self.source_key, rows))?.as_bytes(),
        ))
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ascii_json_is_python_compatible() {
        assert_eq!(
            ascii_json(&vec!["Ёж", "😀", "quote\"\n", "Ä/<>β"]).unwrap(),
            r#"["\u0401\u0436","\ud83d\ude00","quote\"\n","\u00c4/<>\u03b2"]"#
        );
    }
    #[test]
    fn revision_is_provenance_not_metadata_fingerprint() {
        let mut inputs = GitInputs {
            source_key: "git:source".into(),
            revision: "a".repeat(40),
            entries: vec![
                InputEntry {
                    path: "Configuration.xml".into(),
                    oid: "0".repeat(40),
                    size: 0,
                    owner: String::new(),
                    roles: "configuration".into(),
                },
                InputEntry {
                    path: "Languages/Русский.xml".into(),
                    oid: "0".repeat(40),
                    size: 17,
                    owner: "Language.Русский".into(),
                    roles: "descriptor".into(),
                },
            ],
        };
        let before = inputs.fingerprint().unwrap();
        inputs.revision = "b".repeat(40);
        assert_eq!(inputs.fingerprint().unwrap(), before);
        inputs.entries[0].oid = "1".repeat(40);
        assert_ne!(inputs.fingerprint().unwrap(), before);
    }
}
