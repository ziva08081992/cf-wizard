//! Compressed JSON-lines journals, including errors and native output.
use crate::source::no_links;
use cf_application::Journal;
use cf_domain::Result;
use flate2::{Compression, read::MultiGzDecoder, write::GzEncoder};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs::{self, File},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock, RwLock},
};
pub struct OperationLog {
    pub path: PathBuf,
    writer: Mutex<GzEncoder<File>>,
}
static CRASH_FOLDER: OnceLock<RwLock<PathBuf>> = OnceLock::new();
pub fn set_crash_folder(path: &Path) {
    let state = CRASH_FOLDER.get_or_init(|| RwLock::new(path.to_path_buf()));
    *state.write().unwrap_or_else(|e| e.into_inner()) = path.to_path_buf();
}
pub fn crash_current(message: &str) {
    if let Some(state) = CRASH_FOLDER.get() {
        let path = state.read().unwrap_or_else(|e| e.into_inner()).clone();
        crash(&path, message);
    }
}
impl OperationLog {
    pub fn create(folder: &Path, operation: &str) -> Result<Self> {
        no_links(folder)?;
        fs::create_dir_all(folder).map_err(crate::external)?;
        let name = format!(
            "{}-{}-{}.jsonl.gz",
            chrono::Local::now().format("%Y-%m-%d_%H-%M-%S"),
            operation,
            uuid::Uuid::new_v4().simple()
        );
        let path = folder.join(name);
        let file = fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&path)
            .map_err(crate::external)?;
        let log = Self {
            path,
            writer: Mutex::new(GzEncoder::new(file, Compression::fast())),
        };
        log.value(
            "started",
            json!({"operation":operation,"version":env!("CARGO_PKG_VERSION")}),
        )?;
        Ok(log)
    }
    pub fn value(&self, event: &str, fields: Value) -> Result<()> {
        let row = json!({"time":chrono::Utc::now().to_rfc3339(),"event":event,"data":fields});
        let mut writer = self.writer.lock().map_err(crate::external)?;
        serde_json::to_writer(&mut *writer, &row).map_err(crate::external)?;
        writer.write_all(b"\n").map_err(crate::external)?;
        writer.flush().map_err(crate::external)
    }
    pub fn failure(&self, error: &str) -> Result<PathBuf> {
        self.value("failed", json!({"message":error}))?;
        let folder = self.path.parent().unwrap().join("Ошибки");
        no_links(&folder)?;
        fs::create_dir_all(&folder).map_err(crate::external)?;
        let path = folder
            .join(self.path.file_name().unwrap())
            .with_extension("json");
        fs::write(&path,serde_json::to_vec_pretty(&json!({"time":chrono::Utc::now().to_rfc3339(),"message":error,"journal":self.path})).map_err(crate::external)?).map_err(crate::external)?;
        Ok(path)
    }
    pub(crate) fn save_readback_pair(&self, before: &[u8], after: &[u8]) -> Result<PathBuf> {
        let folder = self.path.parent().unwrap().join("Ошибки");
        no_links(&folder)?;
        fs::create_dir_all(&folder).map_err(crate::external)?;
        persist_readback_pair(&folder, before, after, write_new)
    }
    pub(crate) fn save_readback_resource(
        &self,
        before: &[u8],
        after: Option<&[u8]>,
    ) -> Result<PathBuf> {
        let folder = self.path.parent().unwrap().join("Ошибки");
        no_links(&folder)?;
        fs::create_dir_all(&folder).map_err(crate::external)?;
        persist_named_pair(&folder, before, after, false, write_new)
    }
    #[cfg(test)]
    pub(crate) fn poison_writer_for_test(&self) {
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _writer = self.writer.lock().unwrap();
            panic!("simulated journal writer failure");
        }));
    }
    pub fn native_files(&self, work: &Path) -> Result<()> {
        let folder = work.join("logs");
        if !folder.is_dir() {
            return Ok(());
        }
        for item in fs::read_dir(folder).map_err(crate::external)? {
            let path = item.map_err(crate::external)?.path();
            no_links(&path)?;
            if !path.is_file() {
                continue;
            }
            let bytes = fs::read(&path).map_err(crate::external)?;
            self.value("native-log",json!({"name":path.file_name().unwrap().to_string_lossy(),"text":String::from_utf8_lossy(&bytes),"bytes":bytes.len()}))?;
        }
        for name in ["build-progress.json", "build-receipt.json"] {
            let path = work.join(name);
            if let Ok(bytes) = fs::read(path) {
                self.value(
                    name,
                    serde_json::from_slice(&bytes)
                        .unwrap_or_else(|_| json!({"text":String::from_utf8_lossy(&bytes)})),
                )?;
            }
        }
        Ok(())
    }
}
fn write_new(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut file = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(path)
        .map_err(crate::external)?;
    file.write_all(bytes).map_err(crate::external)
}
fn persist_readback_pair(
    folder: &Path,
    before: &[u8],
    after: &[u8],
    write: impl FnMut(&Path, &[u8]) -> Result<()>,
) -> Result<PathBuf> {
    persist_named_pair(folder, before, Some(after), true, write)
}
fn persist_named_pair(
    folder: &Path,
    before: &[u8],
    after: Option<&[u8]>,
    xml: bool,
    mut write: impl FnMut(&Path, &[u8]) -> Result<()>,
) -> Result<PathBuf> {
    let draft = tempfile::Builder::new()
        .prefix("readback-")
        .tempdir_in(folder)
        .map_err(crate::external)?;
    write(
        &draft
            .path()
            .join(if xml { "before.xml" } else { "before.bin" }),
        before,
    )?;
    match after {
        Some(bytes) => write(
            &draft
                .path()
                .join(if xml { "after.xml" } else { "after.bin" }),
            bytes,
        )?,
        None => write(
            &draft.path().join("after-missing.txt"),
            b"The expected readback file is missing or unreadable; see the operation journal.",
        )?,
    }
    Ok(draft.keep())
}
impl Journal for OperationLog {
    fn event(&self, operation: &str, fields: &BTreeMap<String, String>) -> Result<()> {
        self.value(operation, json!(fields))
    }
}
pub fn read(path: &Path) -> Result<String> {
    no_links(path)?;
    let bytes = fs::read(path).map_err(crate::external)?;
    let data = if path.extension().is_some_and(|e| e == "gz") {
        let mut out = vec![];
        let mut decoder = MultiGzDecoder::new(bytes.as_slice()).take(32 * 1024 * 1024);
        let result = decoder.read_to_end(&mut out);
        if let Err(e) = result
            && e.kind() != std::io::ErrorKind::UnexpectedEof
        {
            return Err(crate::external(e));
        }
        out
    } else {
        bytes
    };
    let text = String::from_utf8_lossy(&data);
    let mut out = String::new();
    for row in text.lines() {
        if let Ok(v) = serde_json::from_str::<Value>(row) {
            out.push_str(&format!(
                "{}  {}\n{}\n\n",
                v["time"].as_str().unwrap_or(""),
                v["event"].as_str().unwrap_or(""),
                serde_json::to_string_pretty(&v["data"]).unwrap_or_default()
            ));
        } else {
            out.push_str(row);
            out.push('\n');
        }
    }
    Ok(out)
}
pub fn crash(folder: &Path, message: &str) {
    if let Ok(log) = OperationLog::create(folder, "crash") {
        let _ = log.failure(message);
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn compressed_journal_can_be_viewed() {
        let dir = tempfile::tempdir().unwrap();
        let path;
        {
            let log = OperationLog::create(dir.path(), "test").unwrap();
            path = log.path.clone();
            log.value("example", json!({"текст":"проверка"})).unwrap();
        }
        assert!(read(&path).unwrap().contains("проверка"));
    }
    #[test]
    fn partial_readback_pair_is_removed_if_the_second_write_fails() {
        let temp = tempfile::tempdir().unwrap();
        let result = persist_readback_pair(temp.path(), b"before", b"after", |path, bytes| {
            if path.file_name().unwrap() == "after.xml" {
                write_new(path, b"partial")?;
                return Err(cf_domain::Error::External("simulated disk full".into()));
            }
            write_new(path, bytes)
        });
        assert!(result.is_err());
        assert_eq!(fs::read_dir(temp.path()).unwrap().count(), 0);
    }
}
