//! Whitelisted preferences only. Secrets never enter this data model.
use crate::{git::Connection, source::no_links};
use cf_domain::Result;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    path::{Path, PathBuf},
};

#[derive(Clone, Serialize, Deserialize)]
pub struct Settings {
    pub values: BTreeMap<String, String>,
    pub last_dirs: BTreeMap<String, String>,
    pub index_directories: BTreeMap<String, String>,
    pub build_mode: String,
    pub source_mode: String,
    pub git_settings: Connection,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            values: BTreeMap::new(),
            last_dirs: BTreeMap::new(),
            index_directories: BTreeMap::new(),
            build_mode: "auto".into(),
            source_mode: "local".into(),
            git_settings: Connection::default(),
        }
    }
}
pub fn base() -> PathBuf {
    if let Some(p) = std::env::var_os("LOCALAPPDATA") {
        PathBuf::from(p).join("CF-Wizard")
    } else {
        PathBuf::from(
            std::env::var_os("USERPROFILE")
                .or_else(|| std::env::var_os("HOME"))
                .unwrap_or_default(),
        )
        .join(".cf-wizard")
    }
}
fn strings(value: &Value, allowed: Option<&[&str]>) -> BTreeMap<String, String> {
    value
        .as_object()
        .into_iter()
        .flat_map(|v| v.iter())
        .filter_map(|(k, v)| {
            if allowed.is_none_or(|keys| keys.contains(&k.as_str())) {
                v.as_str().map(|s| (k.clone(), s.into()))
            } else {
                None
            }
        })
        .collect()
}
impl Settings {
    pub fn load(path: &Path) -> Self {
        let Ok(bytes) = fs::read(path) else {
            return Self::default();
        };
        let Ok(v) = serde_json::from_slice::<Value>(&bytes) else {
            return Self::default();
        };
        let keys = &[
            "export",
            "destination",
            "platform",
            "designer",
            "logs",
            "index_directory",
        ];
        let mut s = Self {
            values: strings(&v["values"], Some(keys)),
            last_dirs: strings(&v["last_dirs"], Some(keys)),
            index_directories: strings(&v["index_directories"], None),
            ..Self::default()
        };
        if let Some(m) = v["build_mode"]
            .as_str()
            .filter(|m| ["auto", "designer", "ibcmd"].contains(m))
        {
            s.build_mode = m.into();
        }
        if v["source_mode"] == "git" {
            s.source_mode = "git".into();
        }
        let g = &v["git_settings"];
        for (key, value) in strings(
            g,
            Some(&[
                "provider",
                "server",
                "repository",
                "directory",
                "branch",
                "cache",
            ]),
        ) {
            match key.as_str() {
                "provider" if ["gitea", "github"].contains(&value.as_str()) => {
                    s.git_settings.provider = value
                }
                "server" => s.git_settings.server = value,
                "repository" => s.git_settings.repository = value,
                "directory" => s.git_settings.directory = value,
                "branch" => s.git_settings.branch = value,
                "cache" => s.git_settings.cache = value,
                _ => {}
            }
        }
        if let Some(n) = g["interval"]
            .as_u64()
            .filter(|&n| n == 0 || (30..=86400).contains(&n))
        {
            s.git_settings.interval = n as u32;
        }
        s
    }
    pub fn save(&self, path: &Path) -> Result<()> {
        no_links(path)?;
        let parent = path
            .parent()
            .ok_or_else(|| crate::external("Нет папки настроек"))?;
        fs::create_dir_all(parent).map_err(crate::external)?;
        let mut temp = tempfile::NamedTempFile::new_in(parent).map_err(crate::external)?;
        let mut bytes = serde_json::to_vec_pretty(self).map_err(crate::external)?;
        bytes.push(b'\n');
        temp.write_all(&bytes).map_err(crate::external)?;
        temp.as_file().sync_all().map_err(crate::external)?;
        temp.persist(path).map_err(crate::external)?;
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn old_settings_are_whitelisted() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("paths.json");
        fs::write(&p,br#"{"values":{"export":"X","token":"secret"},"git_settings":{"server":"https://git.example","token":"secret"},"build_mode":"designer"}"#).unwrap();
        let s = Settings::load(&p);
        assert_eq!(s.git_settings.provider, "gitea");
        s.save(&p).unwrap();
        assert!(
            !String::from_utf8(fs::read(p).unwrap())
                .unwrap()
                .contains("secret")
        );
    }
}
