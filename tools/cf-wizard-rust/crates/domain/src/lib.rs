//! Metadata and package rules, independent of filesystem, HTTP, SQL and desktop.
pub mod canonical;
pub use canonical::{GitInputs, InputEntry};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Invalid(String),
    #[error("Операция отменена")]
    Cancelled,
    #[error("{0}")]
    External(String),
}
pub type Result<T> = std::result::Result<T, Error>;
/// Matches the pinned engine's restricted folding, not Unicode lowercasing.
pub fn fold(text: &str) -> String {
    text.chars()
        .map(|c| match c {
            'A'..='Z' | 'А'..='Я' => char::from_u32(c as u32 + 32).unwrap_or(c),
            'Ё' => 'ё',
            _ => c,
        })
        .collect()
}
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
pub struct ObjectKey(pub String);
impl std::borrow::Borrow<str> for ObjectKey {
    fn borrow(&self) -> &str {
        &self.0
    }
}
impl ObjectKey {
    pub fn parse(value: &str) -> Result<Self> {
        let (kind, name) = value
            .split_once('.')
            .ok_or_else(|| Error::Invalid(format!("Некорректный объект: {value}")))?;
        if kind.is_empty()
            || name.is_empty()
            || value.chars().any(char::is_control)
            || name.contains(['.', '/', '\\', ':'])
            || !kind.chars().all(|c| c.is_ascii_alphabetic())
        {
            return Err(Error::Invalid(format!("Некорректный объект: {value}")));
        }
        Ok(Self(value.to_owned()))
    }
    pub fn kind(&self) -> &str {
        self.0.split_once('.').map_or("", |v| v.0)
    }
    pub fn name(&self) -> &str {
        self.0.split_once('.').map_or("", |v| v.1)
    }
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Member {
    pub name: String,
    pub tag: String,
    pub types: BTreeSet<String>,
    pub columns: Vec<Member>,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, Default)]
pub struct Facts {
    pub targets: BTreeSet<String>,
    pub members: Vec<Member>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MetadataObject {
    pub key: ObjectKey,
    pub descriptor: String,
    pub uuid: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Catalog {
    pub name: String,
    pub uuid: String,
    pub objects: BTreeMap<ObjectKey, MetadataObject>,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Cause {
    pub owner: String,
    pub required: bool,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Stub {
    pub key: String,
    pub causes: Vec<Cause>,
}
impl Stub {
    pub fn required(&self) -> bool {
        self.causes.iter().any(|v| v.required)
    }
}
/// A frozen preview. Writing a package is a separate application use case.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Plan {
    pub source_fingerprint: String,
    pub selected: BTreeSet<String>,
    pub stubs: BTreeMap<String, Stub>,
    pub members: BTreeMap<String, BTreeSet<String>>,
    pub source_hashes: BTreeMap<String, String>,
    pub warnings: Vec<String>,
}
impl Plan {
    pub fn validate_exclusions(&self, excluded: &BTreeSet<String>) -> Result<()> {
        for key in excluded {
            let stub = self
                .stubs
                .get(key)
                .ok_or_else(|| Error::Invalid(format!("Заглушка отсутствует в плане: {key}")))?;
            if stub.required() {
                return Err(Error::Invalid(format!(
                    "Обязательную заглушку нельзя исключить: {key}"
                )));
            }
        }
        Ok(())
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    Gitea,
    Github,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BuildMode {
    Auto,
    Designer,
    Ibcmd,
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn restricted_fold_preserves_other_alphabets() {
        assert_eq!(fold("ABC АБВ Ё Latinİ Ä Ω"), "abc абв ё latinİ Ä Ω");
    }
    #[test]
    fn required_causes_override_optional() {
        let stub = Stub {
            key: "Catalog.A".into(),
            causes: vec![
                Cause {
                    owner: "CommonModule.A".into(),
                    required: false,
                },
                Cause {
                    owner: "Document.A".into(),
                    required: true,
                },
            ],
        };
        let plan = Plan {
            source_fingerprint: String::new(),
            selected: BTreeSet::new(),
            stubs: BTreeMap::from([(stub.key.clone(), stub)]),
            members: BTreeMap::new(),
            source_hashes: BTreeMap::new(),
            warnings: vec![],
        };
        assert!(
            plan.validate_exclusions(&BTreeSet::from(["Catalog.A".into()]))
                .is_err()
        );
        assert!(
            plan.validate_exclusions(&BTreeSet::from(["Catalog.Unknown".into()]))
                .is_err()
        );
    }
    #[test]
    fn metadata_keys_cannot_be_paths() {
        for value in [
            "Catalog../a",
            "Catalog.\\bad",
            "Catalog.a:b",
            "Catalog.",
            "../bad",
        ] {
            assert!(ObjectKey::parse(value).is_err(), "{value}");
        }
        assert_eq!(
            ObjectKey::parse("Catalog.Номенклатура").unwrap().name(),
            "Номенклатура"
        );
    }
}
