//! Owned temporary workspace and atomic publication of CF + merge settings.
use crate::{
    canonical::sha256,
    journal::OperationLog,
    source::{canonical_future, no_links},
};
use cf_application::{Cancellation, NativeArtifact, PackageOutput, PackageWorkspace};
use cf_domain::{Error, Result};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};
pub struct Output {
    pub log: Arc<OperationLog>,
}
#[cfg(test)]
#[path = "output_tests.rs"]
mod regression;
struct Workspace {
    temporary: tempfile::TempDir,
    destination: PathBuf,
    label: String,
    log: Arc<OperationLog>,
}
pub fn folder_label(text: &str) -> String {
    let mut result = text
        .trim()
        .chars()
        .map(|c| {
            if c.is_control() || matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*') {
                '_'
            } else {
                c
            }
        })
        .take(100)
        .collect::<String>();
    result = result.trim_end_matches(['.', ' ']).into();
    if result.is_empty() {
        return chrono::Local::now().format("%Y-%m-%d_%H-%M-%S").to_string();
    }
    if crate::source::relative_path(&result).is_err() {
        result.insert(0, '_');
    }
    result
}
impl PackageOutput for Output {
    fn begin(
        &self,
        destination: &Path,
        label: &str,
        source: Option<&Path>,
    ) -> Result<Box<dyn PackageWorkspace>> {
        if destination.as_os_str().is_empty() {
            return Err(Error::Invalid("Выберите папку результата".into()));
        }
        let destination = canonical_future(destination)?;
        let source = source.map(canonical_future).transpose()?;
        if source.as_ref().is_some_and(|s| destination.starts_with(s)) {
            return Err(Error::Invalid(
                "Папка результата должна находиться вне XML-выгрузки".into(),
            ));
        }
        if destination
            .components()
            .any(|p| p.as_os_str().eq_ignore_ascii_case(".git"))
        {
            return Err(Error::Invalid(
                "Результат должен находиться вне .git".into(),
            ));
        }
        fs::create_dir_all(&destination).map_err(crate::external)?;
        no_links(&destination)?;
        // OS-owned temp workspace, with sibling staging/settings/native paths.
        let temporary = tempfile::Builder::new()
            .prefix("cf-wizard-rust-")
            .tempdir()
            .map_err(crate::external)?;
        if source
            .as_ref()
            .is_some_and(|s| canonical_future(temporary.path()).is_ok_and(|p| p.starts_with(s)))
        {
            return Err(Error::Invalid(
                "Временная папка сборки должна находиться вне XML-выгрузки".into(),
            ));
        }
        Ok(Box::new(Workspace {
            temporary,
            destination,
            label: folder_label(label),
            log: self.log.clone(),
        }))
    }
}
impl Drop for Workspace {
    fn drop(&mut self) {
        if let Err(e) = self.log.native_files(&self.native_work()) {
            let _ = self
                .log
                .failure(&format!("Ошибка сохранения native logs: {e}"));
        }
    }
}
impl PackageWorkspace for Workspace {
    fn staging(&self) -> PathBuf {
        self.temporary.path().join("stage")
    }
    fn settings(&self) -> PathBuf {
        self.temporary.path().join("merge-settings.xml")
    }
    fn native_work(&self) -> PathBuf {
        self.temporary.path().join("native")
    }
    fn publish(
        &self,
        artifact: &NativeArtifact,
        settings: &Path,
        settings_hash: &str,
        cancel: &Cancellation,
    ) -> Result<PathBuf> {
        cancel.check()?;
        no_links(&artifact.path)?;
        no_links(settings)?;
        let cf = canonical_future(&artifact.path)?;
        let settings = canonical_future(settings)?;
        let owned_native = canonical_future(&self.native_work())?;
        let owned_settings = canonical_future(&self.settings())?;
        if !cf.starts_with(&owned_native) || settings != owned_settings {
            return Err(Error::Invalid(
                "Публикуются только файлы текущей сборки".into(),
            ));
        }
        let name = cf
            .file_name()
            .ok_or_else(|| Error::Invalid("Нет имени CF".into()))?;
        let cf_bytes = fs::read(&cf).map_err(crate::external)?;
        if cf_bytes.is_empty() || cf.extension().is_none_or(|e| e != "cf") {
            return Err(Error::Invalid("Ожидался непустой файл CF".into()));
        }
        let settings_bytes = fs::read(settings).map_err(crate::external)?;
        if sha256(&cf_bytes) != artifact.sha256 || sha256(&settings_bytes) != settings_hash {
            return Err(Error::Invalid(
                "Проверенные файлы изменились перед публикацией".into(),
            ));
        }
        let pending = tempfile::Builder::new()
            .prefix(".cf-publish-")
            .tempdir_in(&self.destination)
            .map_err(crate::external)?;
        let target_cf = pending.path().join(name);
        fs::write(&target_cf, &cf_bytes).map_err(crate::external)?;
        let merge_name = format!(
            "{}.merge-settings.xml",
            cf.file_stem().unwrap().to_string_lossy()
        );
        let target_settings = pending.path().join(merge_name);
        fs::write(&target_settings, &settings_bytes).map_err(crate::external)?;
        if sha256(&fs::read(&target_settings).map_err(crate::external)?) != settings_hash {
            return Err(Error::Invalid(
                "Настройки объединения изменились при копировании".into(),
            ));
        }
        if sha256(&fs::read(target_cf).map_err(crate::external)?) != sha256(&cf_bytes) {
            return Err(Error::Invalid(
                "Проверка опубликованного CF не пройдена".into(),
            ));
        }
        cancel.check()?;
        let mut target = self.destination.join(&self.label);
        if target.exists() {
            target = self.destination.join(format!(
                "{}-{}",
                self.label,
                chrono::Local::now().format("%H-%M-%S-%3f")
            ));
        }
        no_links(&target)?;
        if target.exists() {
            return Err(Error::Invalid("Папка результата уже существует".into()));
        }
        fs::rename(pending.path(), &target).map_err(crate::external)?;
        Ok(PathBuf::from(crate::native::normal_path_display(&target)))
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn arbitrary_comment_and_date() {
        assert_eq!(folder_label("1235"), "1235");
        assert_eq!(folder_label("08/11/2026: тест"), "08_11_2026_ тест");
        assert!(folder_label("").len() > 15);
        assert_eq!(folder_label("CON"), "_CON");
    }
}
