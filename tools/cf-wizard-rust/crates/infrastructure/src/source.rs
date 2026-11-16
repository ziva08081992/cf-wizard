//! Checked local Designer exports and common source contracts.
use crate::{
    canonical::{ascii_json, sha256},
    kinds::TYPE_FOLDERS,
    source_xml::SourceXml,
};
use cf_application::{Cancellation, MetadataSource, SourceEntry};
use cf_domain::{Catalog, Error, MetadataObject, ObjectKey, Result};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Read,
    path::{Path, PathBuf},
};
use unicode_casefold::UnicodeCaseFold;
pub fn identifier(text: &str) -> bool {
    let mut chars = text.chars();
    chars
        .next()
        .is_some_and(|c| c == '_' || unicode_ident::is_xid_start(c))
        && chars.all(unicode_ident::is_xid_continue)
}
pub fn relative_path(value: &str) -> Result<&str> {
    let name = value.strip_suffix('/').unwrap_or(value);
    if name.is_empty()
        || name.starts_with('/')
        || name.contains('\\')
        || name
            .chars()
            .any(|c| c.is_control() || matches!(c, '<' | '>' | ':' | '"' | '|' | '?' | '*'))
        || name.split('/').any(|p| {
            p.is_empty() || p == "." || p == ".." || p.ends_with([' ', '.']) || reserved(p)
        })
    {
        return Err(Error::Invalid(format!(
            "Недопустимый относительный путь: {value}"
        )));
    }
    Ok(name)
}
fn reserved(part: &str) -> bool {
    let base = part.split('.').next().unwrap_or("").to_ascii_uppercase();
    matches!(base.as_str(), "CON" | "PRN" | "AUX" | "NUL" | "CLOCK$")
        || base
            .strip_prefix("COM")
            .or_else(|| base.strip_prefix("LPT"))
            .is_some_and(|s| s.len() == 1 && matches!(s.as_bytes()[0], b'1'..=b'9'))
}
pub fn no_links(path: &Path) -> Result<()> {
    for part in path.ancestors() {
        match output_kind(part) {
            Ok(LocalKind::Link) => {
                return Err(Error::Invalid(format!(
                    "Ссылки и reparse points не разрешены: {}",
                    part.display()
                )));
            }
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(crate::external(e)),
        }
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LocalKind {
    File,
    Directory,
    Link,
    Other,
}
impl LocalKind {
    #[cfg(windows)]
    fn from_attributes(attributes: u32) -> Self {
        if attributes & 0x400 != 0 {
            Self::Link
        } else if attributes & 0x10 != 0 {
            Self::Directory
        } else {
            Self::File
        }
    }

    fn from_metadata(meta: &fs::Metadata) -> Self {
        #[cfg(windows)]
        let reparse = {
            use std::os::windows::fs::MetadataExt;
            meta.file_attributes() & 0x400 != 0
        };
        #[cfg(not(windows))]
        let reparse = false;
        if meta.file_type().is_symlink() || reparse {
            Self::Link
        } else if meta.is_dir() {
            Self::Directory
        } else if meta.is_file() {
            Self::File
        } else {
            Self::Other
        }
    }
}

fn standard_kind(path: &Path) -> std::io::Result<LocalKind> {
    fs::symlink_metadata(path).map(|meta| LocalKind::from_metadata(&meta))
}

#[cfg(windows)]
mod attributes {
    use super::*;
    use std::{os::windows::ffi::OsStrExt, path::Component, path::Prefix};

    // WIN32_FILE_ATTRIBUTE_DATA: DWORD attributes, three FILETIMEs, two DWORDs.
    #[repr(C)]
    struct AttributeData {
        attributes: u32,
        remaining: [u32; 8],
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetFileAttributesW(path: *const u16) -> u32;
        fn GetFileAttributesExW(path: *const u16, level: i32, data: *mut AttributeData) -> i32;
    }

    fn wide_path(path: &Path) -> std::io::Result<Vec<u16>> {
        let mut name: Vec<u16> = path.as_os_str().encode_wide().collect();
        if name.contains(&0) {
            return Err(std::io::Error::from(std::io::ErrorKind::InvalidInput));
        }
        name.push(0);
        Ok(name)
    }

    pub(super) fn basic(path: &Path) -> std::io::Result<u32> {
        let name = wide_path(path)?;
        // SAFETY: name is live and NUL-terminated. The query observes the link
        // itself. Capture GetLastError immediately, before any other OS call.
        let value = unsafe { GetFileAttributesW(name.as_ptr()) };
        if value == u32::MAX {
            Err(std::io::Error::last_os_error())
        } else {
            Ok(value)
        }
    }

    pub(super) fn extended(path: &Path) -> std::io::Result<u32> {
        let name = wide_path(path)?;
        let mut data = AttributeData {
            attributes: 0,
            remaining: [0; 8],
        };
        // SAFETY: name is live and NUL-terminated; data has the C layout and
        // size required by GetFileExInfoStandard (0), and is writable. The
        // query returns the link's own attributes, including reparse flags.
        if unsafe { GetFileAttributesExW(name.as_ptr(), 0, &mut data) } == 0 {
            Err(std::io::Error::last_os_error())
        } else {
            Ok(data.attributes)
        }
    }

    fn local_disk(path: &Path) -> bool {
        path.is_absolute()
            && matches!(path.components().next(), Some(Component::Prefix(p))
                if matches!(p.kind(), Prefix::Disk(_) | Prefix::VerbatimDisk(_)))
    }

    pub(super) fn output_with(
        path: &Path,
        query: impl FnOnce(&Path) -> std::io::Result<u32>,
        fallback: impl FnOnce(&Path) -> std::io::Result<LocalKind>,
    ) -> std::io::Result<LocalKind> {
        // UNC, device namespaces and relative paths keep the standard query.
        // ExW errors also fall back, preserving aliases such as NUL and the
        // standard error classification. Basic W would accept NUL as a file.
        if local_disk(path)
            && let Ok(value) = query(path)
        {
            return Ok(LocalKind::from_attributes(value));
        }
        fallback(path)
    }

    pub(super) fn source_with(
        path: &Path,
        query: impl FnOnce(&Path) -> std::io::Result<u32>,
        fallback: impl FnOnce(&Path) -> std::io::Result<LocalKind>,
    ) -> std::io::Result<LocalKind> {
        match query(path) {
            Ok(value) => Ok(LocalKind::from_attributes(value)),
            Err(error)
                if matches!(error.raw_os_error(), Some(2 | 3))
                    && path.is_absolute()
                    && matches!(path.components().next(), Some(Component::Prefix(p))
                        if matches!(p.kind(), Prefix::VerbatimDisk(_))) =>
            {
                // LocalSource roots are canonical absolute paths. Only the
                // unambiguous file/path-not-found errors avoid a second query.
                // No result is cached; the caller still checks all ancestors.
                Err(error)
            }
            Err(_) => fallback(path),
        }
    }
}

fn output_kind(path: &Path) -> std::io::Result<LocalKind> {
    #[cfg(windows)]
    {
        attributes::output_with(path, attributes::extended, standard_kind)
    }
    #[cfg(not(windows))]
    standard_kind(path)
}

fn local_kind(path: &Path) -> std::io::Result<LocalKind> {
    #[cfg(windows)]
    {
        attributes::source_with(path, attributes::basic, standard_kind)
    }
    #[cfg(not(windows))]
    standard_kind(path)
}

fn checked_local_kind(path: &Path) -> Result<Option<LocalKind>> {
    let mut leaf = None;
    for (index, part) in path.ancestors().enumerate() {
        match local_kind(part) {
            Ok(LocalKind::Link) => {
                return Err(Error::Invalid(format!(
                    "Ссылки и reparse points не разрешены: {}",
                    part.display()
                )));
            }
            Ok(kind) => {
                if index == 0 {
                    leaf = Some(kind);
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(crate::external(e)),
        }
    }
    Ok(leaf)
}
pub fn normal_path(path: &Path) -> String {
    let value = path.to_string_lossy();
    let value = if let Some(unc) = value.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{unc}")
    } else {
        value.strip_prefix(r"\\?\").unwrap_or(&value).to_owned()
    };
    #[cfg(windows)]
    {
        value.replace('/', "\\").to_lowercase()
    }
    #[cfg(not(windows))]
    {
        value
    }
}
pub fn canonical_future(path: &Path) -> Result<PathBuf> {
    no_links(path)?;
    let mut existing = std::path::absolute(path).map_err(crate::external)?;
    let mut tail = vec![];
    while !existing.exists() {
        tail.push(
            existing
                .file_name()
                .ok_or_else(|| Error::Invalid("Не найден существующий родитель пути".into()))?
                .to_owned(),
        );
        if !existing.pop() {
            return Err(Error::Invalid("Не найден корень пути".into()));
        }
    }
    let mut result = fs::canonicalize(&existing).map_err(crate::external)?;
    for component in tail.into_iter().rev() {
        result.push(component);
    }
    Ok(result)
}
pub fn catalog(data: &[u8]) -> Result<Catalog> {
    catalog_policy(data, false)
}
pub(crate) fn catalog_generated(data: &[u8]) -> Result<Catalog> {
    catalog_policy(data, true)
}
fn catalog_policy(data: &[u8], allow_transfer: bool) -> Result<Catalog> {
    let view = SourceXml::parse(data)?;
    let xml = &view.xml;
    if !view.metadata(xml.root, "MetaDataObject") {
        return Err(Error::Invalid(
            "Configuration.xml: неверный namespace/корень".into(),
        ));
    }
    let config = view
        .child(xml.root, "Configuration")
        .ok_or_else(|| Error::Invalid("Configuration.xml не описывает конфигурацию".into()))?;
    let properties = view
        .child(config, "Properties")
        .ok_or_else(|| Error::Invalid("Нет свойств конфигурации".into()))?;
    if view
        .child(properties, "ConfigurationExtensionPurpose")
        .is_some()
    {
        return Err(Error::Invalid(
            "Это расширение. Мастер собирает только CF основной конфигурации".into(),
        ));
    }
    let name = view
        .child(properties, "Name")
        .map(|id| xml.text(id))
        .transpose()?
        .unwrap_or_default();
    let children = view
        .child(config, "ChildObjects")
        .ok_or_else(|| Error::Invalid("Нет состава конфигурации".into()))?;
    if !identifier(&name) {
        return Err(Error::Invalid("Некорректное имя конфигурации".into()));
    }
    let mut objects = BTreeMap::new();
    let mut folded = BTreeSet::new();
    for &item in &xml.elements[children].children {
        let kind = &xml.elements[item].local;
        if !view.metadata(item, kind) {
            return Err(Error::Invalid(format!(
                "Configuration.xml: чужой namespace объекта {kind}"
            )));
        }
        let name = xml.text(item)?;
        let folder = TYPE_FOLDERS
            .iter()
            .find(|&&(k, _)| k == kind)
            .map(|&(_, folder)| folder)
            .ok_or_else(|| Error::Invalid(format!("Неизвестный тип {kind}")))?;
        if !identifier(&name) {
            return Err(Error::Invalid(format!("Некорректный объект {kind}.{name}")));
        }
        let key = ObjectKey::parse(&format!("{kind}.{name}"))?;
        if key.0 == "Subsystem._Перенос" && !allow_transfer {
            return Err(Error::Invalid(
                "В выгрузке уже есть подсистема _Перенос".into(),
            ));
        }
        if !folded.insert(key.0.case_fold().collect::<String>()) {
            return Err(Error::Invalid(format!("Повтор объекта: {}", key.0)));
        }
        objects.insert(
            key.clone(),
            MetadataObject {
                key,
                descriptor: format!("{folder}/{name}.xml"),
                uuid: String::new(),
            },
        );
    }
    if !objects.keys().any(|k| k.kind() != "Language") {
        return Err(Error::Invalid("Нет объектов для переноса".into()));
    }
    Ok(Catalog {
        name,
        uuid: xml.attr(config, "uuid").unwrap_or_default().into(),
        objects,
    })
}

pub struct LocalSource {
    pub root: PathBuf,
    identity: String,
    catalog: Catalog,
    configuration_hash: String,
}
impl LocalSource {
    pub fn open(path: &Path) -> Result<Self> {
        no_links(path)?;
        let root = if path.is_file()
            && path.file_name().is_some_and(|n| {
                n.to_string_lossy()
                    .eq_ignore_ascii_case("Configuration.xml")
            }) {
            path.parent()
                .ok_or_else(|| Error::Invalid("Нет папки выгрузки".into()))?
        } else {
            path
        };
        if !root.is_dir() {
            return Err(Error::Invalid(
                "Нужна папка Designer XML или Configuration.xml".into(),
            ));
        }
        let root = fs::canonicalize(root).map_err(crate::external)?;
        no_links(&root.join("Configuration.xml"))?;
        let data = fs::read(root.join("Configuration.xml")).map_err(crate::external)?;
        let catalog = catalog(&data)?;
        let identity = normal_path(&root);
        Ok(Self {
            root,
            identity,
            catalog,
            configuration_hash: sha256(&data),
        })
    }
    pub fn path(&self, relative: &str) -> Result<PathBuf> {
        let path = self.root.join(relative_path(relative)?);
        checked_local_kind(&path)?;
        Ok(path)
    }
}
impl MetadataSource for LocalSource {
    fn materialized_root(&self) -> Option<&Path> {
        Some(&self.root)
    }
    fn identity(&self) -> &str {
        &self.identity
    }
    fn catalog(&self) -> &Catalog {
        &self.catalog
    }
    fn read(&self, relative: &str, cancel: &Cancellation) -> Result<Vec<u8>> {
        cancel.check()?;
        let path = self.path(relative)?;
        let mut file = fs::File::open(&path).map_err(crate::external)?;
        if !file.metadata().map_err(crate::external)?.is_file() {
            return Err(Error::Invalid(format!("Ожидался обычный файл: {relative}")));
        }
        let mut data = vec![];
        let mut block = [0u8; 64 * 1024];
        loop {
            cancel.check()?;
            let n = file.read(&mut block).map_err(crate::external)?;
            if n == 0 {
                break;
            }
            data.extend_from_slice(&block[..n]);
        }
        checked_local_kind(&path)?;
        Ok(data)
    }
    fn exists(&self, relative: &str, cancel: &Cancellation) -> Result<bool> {
        cancel.check()?;
        let path = self.root.join(relative_path(relative)?);
        Ok(checked_local_kind(&path)? == Some(LocalKind::File))
    }
    fn list_directory(&self, relative: &str, cancel: &Cancellation) -> Result<Vec<SourceEntry>> {
        cancel.check()?;
        let path = if relative.is_empty() {
            self.root.clone()
        } else {
            self.root.join(relative_path(relative)?)
        };
        checked_local_kind(&path)?;
        let listing = match fs::read_dir(&path) {
            Ok(listing) => listing,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
            Err(e) => return Err(crate::external(e)),
        };
        let mut out = vec![];
        let mut names = BTreeSet::new();
        for entry in listing {
            cancel.check()?;
            let entry = entry.map_err(crate::external)?;
            let kind = checked_local_kind(&entry.path())?.ok_or_else(|| {
                crate::external(std::io::Error::from(std::io::ErrorKind::NotFound))
            })?;
            let name = entry
                .file_name()
                .into_string()
                .map_err(|_| Error::Invalid("Путь не представим в UTF8".into()))?;
            if !names.insert(name.case_fold().collect::<String>()) {
                return Err(Error::Invalid(format!(
                    "Коллизия Windows-путей в {}",
                    path.display()
                )));
            }
            if !matches!(kind, LocalKind::Directory | LocalKind::File) {
                return Err(Error::Invalid("Неподдерживаемый вид файла".into()));
            }
            out.push(SourceEntry {
                name,
                directory: kind == LocalKind::Directory,
            });
        }
        out.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(out)
    }
    fn files_below(&self, relative: &str, cancel: &Cancellation) -> Result<Vec<String>> {
        let prefix = relative_path(relative)?.trim_end_matches('/');
        let mut out = vec![];
        let mut todo = vec![prefix.to_owned()];
        while let Some(directory) = todo.pop() {
            for entry in self.list_directory(&directory, cancel)? {
                let child = format!("{directory}/{}", entry.name);
                if entry.directory {
                    todo.push(child);
                } else {
                    out.push(child);
                }
            }
        }
        out.sort();
        Ok(out)
    }
    fn snapshot(&self, cancel: &Cancellation) -> Result<String> {
        let config = sha256(&self.read("Configuration.xml", cancel)?);
        if config != self.configuration_hash {
            return Err(Error::Invalid(
                "Configuration.xml изменился: откройте выгрузку заново".into(),
            ));
        }
        let dump = if self.exists("ConfigDumpInfo.xml", cancel)? {
            sha256(&self.read("ConfigDumpInfo.xml", cancel)?)
        } else {
            String::new()
        };
        Ok(sha256(
            ascii_json(&(self.identity(), config, dump))?.as_bytes(),
        ))
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn config(extension: bool) -> String {
        format!(
            "<MetaDataObject><Configuration uuid=\"11111111-1111-4111-8111-111111111111\"><Properties><Name>Fixture</Name>{}</Properties><ChildObjects><Language>Русский</Language><Catalog>Тест</Catalog></ChildObjects></Configuration></MetaDataObject>",
            if extension {
                "<ConfigurationExtensionPurpose>Customization</ConfigurationExtensionPurpose>"
            } else {
                ""
            }
        )
    }
    #[test]
    fn extension_and_unsafe_relative_paths_are_rejected() {
        assert!(catalog(config(true).as_bytes()).is_err());
        for path in [
            "../x",
            "/x",
            "a\\b",
            "a:stream",
            "a//b",
            "CON.xml",
            "a/LPT1.txt",
            "a.",
        ] {
            assert!(relative_path(path).is_err(), "{path}");
        }
        assert!(relative_path("Catalogs/Тест/Ext/Module.bsl").is_ok());
    }
    #[test]
    fn opens_catalog_without_reading_all_descriptors_and_detects_root_change() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("Configuration.xml"), config(false)).unwrap();
        let source = LocalSource::open(&dir.path().join("Configuration.xml")).unwrap();
        assert_eq!(source.catalog().objects.len(), 2);
        assert!(
            !source
                .exists("Catalogs/Тест.xml", &Cancellation::default())
                .unwrap()
        );
        let before = source.snapshot(&Cancellation::default()).unwrap();
        assert!(!before.is_empty());
        fs::write(
            dir.path().join("Configuration.xml"),
            config(false).replace("Fixture", "Changed"),
        )
        .unwrap();
        assert!(source.snapshot(&Cancellation::default()).is_err());
    }

    fn local_fixture() -> (tempfile::TempDir, LocalSource) {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("Configuration.xml"), config(false)).unwrap();
        let source = LocalSource::open(dir.path()).unwrap();
        (dir, source)
    }

    #[test]
    fn local_queries_remain_fresh_and_distinguish_files_from_directories() {
        let (dir, source) = local_fixture();
        let cancel = Cancellation::default();
        fs::create_dir(dir.path().join("Files")).unwrap();
        assert!(!source.exists("Files", &cancel).unwrap());
        assert!(!source.exists("Files/данные.txt", &cancel).unwrap());
        assert!(source.list_directory("Files", &cancel).unwrap().is_empty());
        fs::write(dir.path().join("Files/данные.txt"), b"first").unwrap();
        assert!(source.exists("Files/данные.txt", &cancel).unwrap());
        assert_eq!(source.read("Files/данные.txt", &cancel).unwrap(), b"first");
        assert_eq!(
            source.files_below("Files", &cancel).unwrap(),
            ["Files/данные.txt"]
        );
        fs::write(dir.path().join("Files/данные.txt"), b"changed").unwrap();
        assert_eq!(
            source.read("Files/данные.txt", &cancel).unwrap(),
            b"changed"
        );
        fs::remove_file(dir.path().join("Files/данные.txt")).unwrap();
        assert!(!source.exists("Files/данные.txt", &cancel).unwrap());
        assert!(source.files_below("Files", &cancel).unwrap().is_empty());
        fs::remove_dir(dir.path().join("Files")).unwrap();
        assert!(source.list_directory("Files", &cancel).unwrap().is_empty());
        fs::write(dir.path().join("Files"), b"now a file").unwrap();
        assert!(source.list_directory("Files", &cancel).is_err());
        assert!(source.exists("Files", &cancel).unwrap());
        assert!(source.read("Configuration.xml", &cancel).is_ok());
        assert!(source.read("Files/missing", &cancel).is_err());
        assert!(source.exists("../outside", &cancel).is_err());
    }

    #[test]
    fn local_listing_rejects_full_unicode_casefold_collisions() {
        let (dir, source) = local_fixture();
        fs::create_dir(dir.path().join("Files")).unwrap();
        fs::write(dir.path().join("Files/Straße.xml"), b"a").unwrap();
        fs::write(dir.path().join("Files/STRASSE.xml"), b"b").unwrap();
        assert!(
            source
                .list_directory("Files", &Cancellation::default())
                .unwrap_err()
                .to_string()
                .contains("Коллизия")
        );
    }

    #[test]
    fn local_queries_check_cancellation_before_filesystem_work() {
        let (_dir, source) = local_fixture();
        let cancel = Cancellation::default();
        cancel.cancel();
        assert!(matches!(
            source.exists("missing", &cancel),
            Err(Error::Cancelled)
        ));
        assert!(matches!(
            source.read("missing", &cancel),
            Err(Error::Cancelled)
        ));
        assert!(matches!(
            source.list_directory("", &cancel),
            Err(Error::Cancelled)
        ));
        assert!(matches!(
            source.files_below("missing", &cancel),
            Err(Error::Cancelled)
        ));
    }

    #[cfg(windows)]
    fn junction(link: &Path, target: &Path) {
        use std::{os::windows::process::CommandExt, process::Command};
        let status = Command::new("powershell.exe")
            .args([
                "-NoProfile", "-NonInteractive", "-Command",
                "$ErrorActionPreference='Stop'; New-Item -ItemType Junction -Path $env:CF_TEST_LINK -Target $env:CF_TEST_TARGET | Out-Null",
            ])
            .env("CF_TEST_LINK", link)
            .env("CF_TEST_TARGET", target)
            .creation_flags(0x08000000)
            .status()
            .unwrap();
        assert!(status.success());
    }

    #[cfg(windows)]
    #[test]
    fn local_guard_rechecks_junctions_inserted_after_open() {
        let (dir, source) = local_fixture();
        let outside = tempfile::tempdir().unwrap();
        let cancel = Cancellation::default();
        fs::write(outside.path().join("item.xml"), b"outside").unwrap();
        fs::create_dir(dir.path().join("Files")).unwrap();
        assert!(source.list_directory("Files", &cancel).unwrap().is_empty());
        no_links(&dir.path().join("Files/missing.xml")).unwrap();
        fs::remove_dir(dir.path().join("Files")).unwrap();
        junction(&dir.path().join("Files"), outside.path());
        // A missing leaf must not suppress the fresh ancestor check.
        assert!(
            no_links(&dir.path().join("Files/missing.xml"))
                .unwrap_err()
                .to_string()
                .contains("reparse")
        );
        assert!(
            source
                .exists("Files/missing.xml", &cancel)
                .unwrap_err()
                .to_string()
                .contains("reparse")
        );
        for result in [
            source.exists("Files/item.xml", &cancel).map(|_| ()),
            source.read("Files/item.xml", &cancel).map(|_| ()),
            source.list_directory("Files", &cancel).map(|_| ()),
            source.list_directory("", &cancel).map(|_| ()),
            source.files_below("Files", &cancel).map(|_| ()),
        ] {
            assert!(result.unwrap_err().to_string().contains("reparse"));
        }
        fs::remove_dir(dir.path().join("Files")).unwrap();
        fs::create_dir(dir.path().join("Files")).unwrap();
        no_links(&dir.path().join("Files/missing.xml")).unwrap();
        assert!(source.list_directory("Files", &cancel).unwrap().is_empty());
        assert!(outside.path().join("item.xml").is_file());
    }

    #[cfg(windows)]
    #[test]
    fn local_guard_matches_standard_checks_on_exclusively_open_files() {
        use std::os::windows::fs::OpenOptionsExt;
        let (dir, source) = local_fixture();
        let path = dir.path().join("locked.xml");
        fs::write(&path, b"locked").unwrap();
        let _handle = fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(&path)
            .unwrap();
        assert_eq!(
            local_kind(&path).map_err(|e| e.kind()),
            fs::symlink_metadata(&path)
                .map(|m| LocalKind::from_metadata(&m))
                .map_err(|e| e.kind())
        );
        assert_eq!(checked_local_kind(&path).is_ok(), no_links(&path).is_ok());
        assert!(source.read("locked.xml", &Cancellation::default()).is_err());
    }

    #[cfg(windows)]
    #[test]
    fn local_guard_matches_standard_checks_on_long_and_missing_paths() {
        let (dir, source) = local_fixture();
        let mut path = source.root.clone();
        for _ in 0..9 {
            path.push("длинное_имя_каталога_0123456789");
        }
        fs::create_dir_all(&path).unwrap();
        let file = path.join("данные.xml");
        fs::write(&file, b"long path").unwrap();
        for candidate in [
            dir.path().to_path_buf(),
            path,
            file.clone(),
            file.join("missing"),
        ] {
            assert_eq!(
                local_kind(&candidate).map_err(|e| e.kind()),
                fs::symlink_metadata(&candidate)
                    .map(|m| LocalKind::from_metadata(&m))
                    .map_err(|e| e.kind())
            );
            assert_eq!(
                checked_local_kind(&candidate).is_ok(),
                no_links(&candidate).is_ok()
            );
        }
        assert!(checked_local_kind(&file).unwrap().is_some());
        assert!(local_kind(Path::new("invalid\0path")).is_err());
    }
}

#[cfg(all(test, windows))]
mod attribute_guard_tests {
    use super::*;
    use std::{cell::Cell, io};

    #[test]
    fn output_fast_query_is_scoped_and_all_errors_use_standard_fallback() {
        for name in [r"C:\file", r"\\?\C:\file"] {
            assert_eq!(
                attributes::output_with(Path::new(name), |_| Ok(0x10), |_| panic!("fallback"))
                    .unwrap(),
                LocalKind::Directory
            );
            for code in [2, 3, 5, 32, 53, 87] {
                let called = Cell::new(false);
                let error = attributes::output_with(
                    Path::new(name),
                    |_| Err(io::Error::from_raw_os_error(code)),
                    |_| {
                        called.set(true);
                        Err(io::Error::from_raw_os_error(123))
                    },
                )
                .unwrap_err();
                assert!(called.get());
                assert_eq!(error.raw_os_error(), Some(123));
            }
        }
        for name in [
            "relative",
            r"C:relative",
            r"\rooted",
            r"\\server\share\file",
            r"\\?\UNC\server\share\file",
            r"\\.\NUL",
            r"\\?\Volume{test}\file",
        ] {
            assert_eq!(
                attributes::output_with(
                    Path::new(name),
                    |_| panic!("unsupported fast path: {name}"),
                    |_| Ok(LocalKind::Other),
                )
                .unwrap(),
                LocalKind::Other
            );
        }
    }

    #[test]
    fn source_not_found_shortcut_is_limited_to_absolute_verbatim_disk_paths() {
        for name in [
            r"\\?\C:\missing",
            r"C:\missing",
            "relative",
            r"C:relative",
            r"\\server\share\missing",
            r"\\?\UNC\server\share\missing",
            r"\\.\NUL",
        ] {
            for code in [2, 3, 5, 32, 53, 87] {
                let called = Cell::new(false);
                let result = attributes::source_with(
                    Path::new(name),
                    |_| Err(io::Error::from_raw_os_error(code)),
                    |_| {
                        called.set(true);
                        Ok(LocalKind::Other)
                    },
                );
                if name == r"\\?\C:\missing" && matches!(code, 2 | 3) {
                    let error = result.unwrap_err();
                    assert_eq!(error.raw_os_error(), Some(code));
                    assert_eq!(error.kind(), io::ErrorKind::NotFound);
                    assert!(!called.get());
                } else {
                    assert_eq!(result.unwrap(), LocalKind::Other);
                    assert!(called.get());
                }
            }
        }
    }

    #[test]
    fn file_and_directory_reparse_flags_win_over_other_attributes() {
        let path = Path::new(r"\\?\C:\item");
        for flags in [0x400, 0x410, 0x420, 0x430] {
            assert_eq!(
                attributes::output_with(path, |_| Ok(flags), |_| panic!()).unwrap(),
                LocalKind::Link
            );
            assert_eq!(
                attributes::source_with(path, |_| Ok(flags), |_| panic!()).unwrap(),
                LocalKind::Link
            );
        }
    }

    #[test]
    fn output_query_matches_standard_on_local_paths_and_device_aliases() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("data.xml");
        fs::write(&file, b"data").unwrap();
        let mut cases = vec![
            dir.path().to_path_buf(),
            file.clone(),
            file.join("child"),
            dir.path().join("missing/child"),
            dir.path().join("invalid\0name"),
            dir.path().join("data.xml."),
            dir.path().join("data.xml "),
        ];
        for alias in [
            "NUL", "CON", "PRN", "AUX", "COM1", "LPT1", "CONIN$", "CONOUT$", "CLOCK$",
        ] {
            cases.push(dir.path().join(alias));
        }
        let canonical = fs::canonicalize(dir.path()).unwrap();
        cases.extend([canonical.join("missing"), fs::canonicalize(&file).unwrap()]);
        for candidate in cases {
            assert_eq!(
                output_kind(&candidate).map_err(|e| (e.kind(), e.raw_os_error())),
                standard_kind(&candidate).map_err(|e| (e.kind(), e.raw_os_error())),
                "{}",
                candidate.display()
            );
        }
    }
}

#[cfg(test)]
mod catalog_namespace_tests {
    use super::catalog;
    #[test]
    fn catalog_membership_uses_namespace_identity_and_rejects_foreign_declarations() {
        let md = crate::source_xml::MD;
        let input = format!(
            r#"<m:MetaDataObject xmlns:m="{md}" xmlns:f="urn:foreign"><m:Configuration><m:Properties><m:Name>Probe</m:Name></m:Properties><m:ChildObjects><m:Catalog>Real</m:Catalog></m:ChildObjects></m:Configuration></m:MetaDataObject>"#
        );
        let expected = catalog(input.as_bytes()).unwrap();
        assert!(expected.objects.contains_key("Catalog.Real"));
        let alias = input.replace("m:", "a:").replace("xmlns:m=", "xmlns:a=");
        assert!(
            catalog(alias.as_bytes())
                .unwrap()
                .objects
                .contains_key("Catalog.Real")
        );
        let foreign = input
            .replace("<m:Catalog>", "<f:Catalog>")
            .replace("</m:Catalog>", "</f:Catalog>");
        assert!(catalog(foreign.as_bytes()).is_err());
        for local in [
            "MetaDataObject",
            "Configuration",
            "Properties",
            "Name",
            "ChildObjects",
        ] {
            let foreign = input
                .replace(&format!("<m:{local}"), &format!("<f:{local}"))
                .replace(&format!("</m:{local}>"), &format!("</f:{local}>"));
            assert!(catalog(foreign.as_bytes()).is_err(), "{local}");
        }
    }
}
