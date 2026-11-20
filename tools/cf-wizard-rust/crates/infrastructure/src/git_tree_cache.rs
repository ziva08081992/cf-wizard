use crate::{
    canonical::key,
    git::{Connection, Entry, EntryKind, oid},
    source::{no_links, relative_path},
};
use cf_application::{Cancellation, profile};
use cf_domain::{Error, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    time::Instant,
};
use unicode_casefold::UnicodeCaseFold;

#[cfg(test)]
#[path = "git_tree_cache_tests.rs"]
mod tests;

#[path = "git_tree_sqlite.rs"]
mod sqlite;

#[cfg(test)]
#[path = "git_tree_cache_bench.rs"]
mod bench;

pub(super) type TreeEntries = BTreeMap<String, Entry>;
type EncodedBatch<'a> = Vec<(&'a str, Vec<u8>)>;

const MAX_RECORD_BYTES: usize = 16 * 1024 * 1024;
const CHECKSUM_BYTES: usize = 32;

fn invalid(message: &str) -> Error {
    Error::Invalid(message.into())
}

pub(super) fn checked(entries: Vec<Entry>) -> Result<TreeEntries> {
    let mut seen = BTreeSet::new();
    let mut result = BTreeMap::new();
    for entry in entries {
        relative_path(&entry.name)?;
        if entry.name.contains('/')
            || entry.kind == EntryKind::Unsupported
            || !seen.insert(entry.name.case_fold().collect::<String>())
        {
            return Err(invalid(
                "Коллизия путей или неподдерживаемый файл в каталоге Git",
            ));
        }
        result.insert(entry.name.clone(), entry);
    }
    Ok(result)
}

#[derive(Clone, Copy)]
enum Format {
    Sha1,
    Sha256,
}

impl Format {
    fn validate_tree(
        self,
        id: &str,
        entries: Vec<Entry>,
        cancel: &Cancellation,
    ) -> Result<TreeEntries> {
        cancel.check()?;
        let entries = checked(entries)?;
        if self.git_oid(&entries, cancel)? != id {
            return Err(invalid("Git OID не совпал с полной описью дерева"));
        }
        cancel.check()?;
        Ok(entries)
    }

    fn from_oid(id: &str) -> Result<Self> {
        oid(id, false)?;
        Ok(match id.len() {
            40 => Self::Sha1,
            64 => Self::Sha256,
            _ => unreachable!("validated by oid"),
        })
    }

    fn directory(self) -> &'static str {
        match self {
            Self::Sha1 => "sha1",
            Self::Sha256 => "sha256",
        }
    }

    fn oid_bytes(self) -> usize {
        match self {
            Self::Sha1 => 20,
            Self::Sha256 => 32,
        }
    }

    fn git_oid(self, entries: &TreeEntries, cancel: &Cancellation) -> Result<String> {
        let mut rows = Vec::with_capacity(entries.len());
        let mut length = 0;
        for entry in entries.values() {
            cancel.check()?;
            let mode = match (&entry.kind, entry.mode.as_str()) {
                (EntryKind::Tree, "040000") if entry.size == 0 => "40000",
                (EntryKind::Blob, "100644") => "100644",
                (EntryKind::Blob, "100755") => "100755",
                _ => return Err(invalid("Недопустимый режим записи дерева Git")),
            };
            length += mode.len() + entry.name.len() + self.oid_bytes() + 2;
            // Git compares a virtual slash for a directory and NUL for a blob.
            // A shorter byte slice sorts like the same slice followed by NUL.
            rows.push((mode, entry));
        }
        fn sort_key(entry: &Entry) -> impl Iterator<Item = u8> + '_ {
            entry
                .name
                .bytes()
                .chain((entry.kind == EntryKind::Tree).then_some(b'/'))
        }
        rows.sort_unstable_by(|(_, a), (_, b)| sort_key(a).cmp(sort_key(b)));
        match self {
            Self::Sha1 => Self::hash::<sha1::Sha1>(length, rows, self.oid_bytes(), cancel),
            Self::Sha256 => Self::hash::<Sha256>(length, rows, self.oid_bytes(), cancel),
        }
    }

    fn hash<D: Digest>(
        length: usize,
        rows: Vec<(&str, &Entry)>,
        oid_bytes: usize,
        cancel: &Cancellation,
    ) -> Result<String> {
        let mut digest = D::new();
        digest.update(format!("tree {length}\0").as_bytes());
        let mut raw = [0; 32];
        for (mode, entry) in rows {
            cancel.check()?;
            if entry
                .oid
                .bytes()
                .any(|byte| !byte.is_ascii_digit() && !(b'a'..=b'f').contains(&byte))
                || hex::decode_to_slice(&entry.oid, &mut raw[..oid_bytes]).is_err()
            {
                return Err(invalid("Некорректный OID записи дерева Git"));
            }
            digest.update(mode.as_bytes());
            digest.update(b" ");
            digest.update(entry.name.as_bytes());
            digest.update([0]);
            digest.update(&raw[..oid_bytes]);
        }
        Ok(hex::encode(digest.finalize()))
    }
}

pub(super) fn validate_tree(
    id: &str,
    entries: Vec<Entry>,
    cancel: &Cancellation,
) -> Result<TreeEntries> {
    cancel.check()?;
    Format::from_oid(id)?.validate_tree(id, entries, cancel)
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredEntry {
    name: String,
    oid: String,
    mode: String,
    size: u64,
}

impl StoredEntry {
    fn entry(self) -> Result<Entry> {
        let kind = match self.mode.as_str() {
            "040000" => EntryKind::Tree,
            "100644" | "100755" => EntryKind::Blob,
            _ => return Err(invalid("Недопустимый режим записи кэша дерева Git")),
        };
        Ok(Entry {
            name: self.name,
            oid: self.oid,
            mode: self.mode,
            size: self.size,
            kind,
        })
    }
}

#[derive(Default, Serialize)]
pub(crate) struct LegacyImportReport {
    pub scanned: usize,
    pub imported: usize,
    pub rejected: usize,
    pub oversized: usize,
    pub committed_batches: usize,
    pub failed_batches: usize,
    pub elapsed_ns: u64,
}

pub(super) struct TreeCache {
    sqlite: Option<sqlite::Store>,
    directory: PathBuf,
    format: Format,
}

impl TreeCache {
    pub(super) fn new(cache: &Path, connection: &Connection, revision: &str) -> Result<Self> {
        let format = Format::from_oid(revision)?;
        let repository = key(&[
            &connection.provider,
            &connection.server,
            &connection.repository,
        ]);
        let identity = key(&[
            &connection.provider,
            &connection.server,
            &connection.repository,
            &connection.directory,
            format.directory(),
        ]);
        let db = cache
            .join("tree-cache-sqlite-v1")
            .join(&identity)
            .join("trees.sqlite3");
        Ok(Self {
            sqlite: Some(sqlite::Store::new(db, identity)),
            directory: cache
                .join("tree-cache-v1")
                .join(repository)
                .join(format.directory()),
            format,
        })
    }

    pub(super) fn path(&self, id: &str) -> Result<PathBuf> {
        if id.len() != self.format.oid_bytes() * 2 {
            return Err(invalid("Неподдерживаемый формат OID дерева Git"));
        }
        oid(id, false)?;
        Ok(self.directory.join(&id[..2]).join(id))
    }

    pub(super) fn validate(
        &self,
        id: &str,
        entries: Vec<Entry>,
        cancel: &Cancellation,
    ) -> Result<TreeEntries> {
        self.format.validate_tree(id, entries, cancel)
    }

    fn legacy_record(
        &self,
        id: &str,
        limit: usize,
        cancel: &Cancellation,
    ) -> Result<Option<Vec<u8>>> {
        cancel.check()?;
        let path = self.path(id)?;
        no_links(&path)?;
        let mut options = fs::OpenOptions::new();
        options.read(true);
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            // Check the opened entry itself even if its final component was
            // replaced between the initial no_links check and open.
            const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
            options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
        }
        let file = match options.open(&path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(crate::external(error)),
        };
        let meta = file.metadata().map_err(crate::external)?;
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            if meta.file_attributes() & 0x400 != 0 {
                return Err(invalid("Запись кэша дерева Git является reparse point"));
            }
        }
        if !meta.is_file() || meta.len() > limit as u64 {
            return Err(invalid(
                "Недопустимый размер или тип записи кэша дерева Git",
            ));
        }
        let mut bytes = Vec::new();
        file.take(limit as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(crate::external)?;
        cancel.check()?;
        if bytes.len() > limit {
            return Err(invalid("Запись кэша дерева Git выросла при чтении"));
        }
        Ok(Some(bytes))
    }

    fn decode_record(&self, id: &str, bytes: &[u8], cancel: &Cancellation) -> Result<TreeEntries> {
        cancel.check()?;
        if !(CHECKSUM_BYTES..=MAX_RECORD_BYTES).contains(&bytes.len()) {
            return Err(invalid("Недопустимый размер записи кэша дерева Git"));
        }
        let (checksum, body) = bytes.split_at(CHECKSUM_BYTES);
        if &Sha256::digest(body)[..] != checksum {
            return Err(invalid(
                "Контрольная сумма записи кэша дерева Git не совпала",
            ));
        }
        let rows: Vec<StoredEntry> = serde_json::from_slice(body).map_err(crate::external)?;
        let entries = rows
            .into_iter()
            .map(StoredEntry::entry)
            .collect::<Result<Vec<_>>>()?;
        self.validate(id, entries, cancel)
    }

    pub(super) fn read(&self, id: &str, cancel: &Cancellation) -> Result<Option<TreeEntries>> {
        cancel.check()?;
        self.path(id)?;
        if let Some(store) = &self.sqlite {
            let mut load = profile::span("cache.tree_sqlite_read", "source", 0, None);
            let checked = store.read(id, cancel).and_then(|bytes| {
                bytes
                    .as_deref()
                    .map(|record| self.decode_record(id, record, cancel))
                    .transpose()
            });
            match checked {
                Ok(Some(entries)) => {
                    load.success();
                    profile::note("cache.tree_sqlite_hit", "source", 1);
                    return Ok(Some(entries));
                }
                Ok(None) => load.success(),
                Err(Error::Cancelled) => return Err(Error::Cancelled),
                Err(_) => profile::note("cache.tree_sqlite_rejected", "source", 1),
            }
        }
        let legacy = self.legacy_record(id, MAX_RECORD_BYTES, cancel)?;
        let trees = legacy
            .as_deref()
            .map(|record| self.decode_record(id, record, cancel))
            .transpose()?;
        if trees.is_some() {
            profile::note("cache.tree_legacy_hit", "source", 1);
        }
        Ok(trees)
    }

    pub(super) fn write(
        &self,
        id: &str,
        entries: &TreeEntries,
        cancel: &Cancellation,
    ) -> Result<()> {
        self.write_batch([(id, entries)], cancel).map(|_| ())
    }

    /// Every row in the batch is already canonical-verified by its caller.
    /// A true result means SQLite accepted the entire batch. Otherwise, each
    /// completed legacy entry retains the old atomic-file fallback behavior.
    pub(super) fn write_batch<'a>(
        &self,
        trees: impl IntoIterator<Item = (&'a str, &'a TreeEntries)>,
        cancel: &Cancellation,
    ) -> Result<bool> {
        cancel.check()?;
        let trees: Vec<_> = trees.into_iter().collect();
        for (id, _) in &trees {
            self.path(id)?;
        }
        let mut originals = Vec::new();
        let mut records = EncodedBatch::new();
        let mut bytes = 0;
        let mut all_sqlite = self.sqlite.is_some();
        for (id, entries) in trees {
            cancel.check()?;
            let record = if self.sqlite.is_some() {
                Self::encode_record(entries, sqlite::BATCH_BYTES)?
            } else {
                None
            };
            let Some(record) = record else {
                self.write_legacy(id, entries, cancel)?;
                all_sqlite = false;
                continue;
            };
            if !records.is_empty()
                && (records.len() == sqlite::BATCH_ROWS
                    || bytes + record.len() > sqlite::BATCH_BYTES)
            {
                all_sqlite &= self.persist_batch(&originals, &records, cancel)?;
                originals.clear();
                records.clear();
                bytes = 0;
            }
            bytes += record.len();
            originals.push((id, entries));
            records.push((id, record));
        }
        if !records.is_empty() {
            all_sqlite &= self.persist_batch(&originals, &records, cancel)?;
        }
        Ok(all_sqlite)
    }

    fn persist_batch(
        &self,
        trees: &[(&str, &TreeEntries)],
        records: &EncodedBatch<'_>,
        cancel: &Cancellation,
    ) -> Result<bool> {
        if let Some(store) = &self.sqlite {
            let mut write = profile::span("cache.tree_sqlite_write", "source", 0, None);
            match store.write_batch(
                records.iter().map(|(id, value)| (*id, value.as_slice())),
                cancel,
            ) {
                Ok(()) => {
                    write.items(records.len());
                    write.success();
                    return Ok(true);
                }
                Err(Error::Cancelled) => return Err(Error::Cancelled),
                Err(_) => profile::note("cache.tree_sqlite_write_fallback", "source", 1),
            }
        }
        for (id, entries) in trees {
            self.write_legacy(id, entries, cancel)?;
        }
        Ok(false)
    }

    fn encode_record(entries: &TreeEntries, limit: usize) -> Result<Option<Vec<u8>>> {
        // Bound encode allocation before cloning rows and serializing them.
        let max_bytes = entries.values().try_fold(CHECKSUM_BYTES + 2, |total, row| {
            total.checked_add(
                row.name
                    .len()
                    .checked_mul(6)?
                    .checked_add(row.oid.len() + 80)?,
            )
        });
        if max_bytes.is_none_or(|size| size > limit) {
            return Ok(None);
        }
        let rows: Vec<_> = entries
            .values()
            .map(|row| StoredEntry {
                name: row.name.clone(),
                oid: row.oid.clone(),
                mode: row.mode.clone(),
                size: row.size,
            })
            .collect();
        let body = serde_json::to_vec(&rows).map_err(crate::external)?;
        let mut record = Vec::with_capacity(CHECKSUM_BYTES + body.len());
        record.extend_from_slice(&Sha256::digest(&body));
        record.extend_from_slice(&body);
        if record.len() > limit {
            return Ok(None);
        }
        Ok(Some(record))
    }

    fn write_legacy(&self, id: &str, entries: &TreeEntries, cancel: &Cancellation) -> Result<()> {
        cancel.check()?;
        let record = Self::encode_record(entries, MAX_RECORD_BYTES)?
            .ok_or_else(|| invalid("Запись кэша дерева Git превышает допустимый размер"))?;
        let path = self.path(id)?;
        let parent = path.parent().expect("tree cache record has a parent");
        no_links(&path)?;
        fs::create_dir_all(parent).map_err(crate::external)?;
        no_links(&path)?;
        let mut file = tempfile::NamedTempFile::new_in(parent).map_err(crate::external)?;
        file.write_all(&record).map_err(crate::external)?;
        cancel.check()?;
        no_links(&path)?;
        cancel.check()?;
        file.persist(path).map_err(crate::external)?;
        Ok(())
    }

    pub(super) fn import_legacy(&self, cancel: &Cancellation) -> Result<LegacyImportReport> {
        cancel.check()?;
        let start = Instant::now();
        let mut report = LegacyImportReport::default();
        if let Some(store) = &self.sqlite {
            self.import_records(store, &mut report, cancel)?;
        }
        report.elapsed_ns = start.elapsed().as_nanos().min(u64::MAX as u128) as u64;
        Ok(report)
    }

    fn import_records(
        &self,
        store: &sqlite::Store,
        report: &mut LegacyImportReport,
        cancel: &Cancellation,
    ) -> Result<()> {
        if no_links(&self.directory).is_err() {
            report.rejected += 1;
            return Ok(());
        }
        let shards = match fs::read_dir(&self.directory) {
            Ok(shards) => shards,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(_) => {
                report.rejected += 1;
                return Ok(());
            }
        };
        let mut rows = Vec::new();
        let mut bytes = 0;
        for shard in shards {
            cancel.check()?;
            let Ok(shard) = shard else {
                report.rejected += 1;
                continue;
            };
            let shard_name = shard.file_name();
            if !shard_name
                .to_str()
                .is_some_and(|name| name.len() == 2 && name.bytes().all(|b| b.is_ascii_hexdigit()))
            {
                continue;
            }
            let files = match no_links(&shard.path())
                .and_then(|()| fs::read_dir(shard.path()).map_err(crate::external))
            {
                Ok(files) => files,
                Err(_) => {
                    report.rejected += 1;
                    continue;
                }
            };
            for file in files {
                cancel.check()?;
                report.scanned += 1;
                let Ok(file) = file else {
                    report.rejected += 1;
                    continue;
                };
                let file_name = file.file_name();
                let Some(id) = file_name.to_str() else {
                    report.rejected += 1;
                    continue;
                };
                if !self.path(id).is_ok_and(|expected| expected == file.path()) {
                    report.rejected += 1;
                    continue;
                }
                if file
                    .metadata()
                    .is_ok_and(|meta| meta.len() > sqlite::BATCH_BYTES as u64)
                {
                    report.oversized += 1;
                    continue;
                }
                let encoded = self
                    .legacy_record(id, sqlite::BATCH_BYTES, cancel)
                    .and_then(|maybe| {
                        maybe
                            .map(|record| {
                                self.decode_record(id, &record, cancel)?;
                                Ok(record)
                            })
                            .transpose()
                    });
                match encoded {
                    Ok(Some(record)) => {
                        if rows.len() == sqlite::BATCH_ROWS
                            || bytes + record.len() > sqlite::BATCH_BYTES
                        {
                            Self::import_batch(store, &rows, report, cancel)?;
                            rows.clear();
                            bytes = 0;
                        }
                        bytes += record.len();
                        rows.push((id.to_owned(), record));
                    }
                    Err(Error::Cancelled) => return Err(Error::Cancelled),
                    _ => report.rejected += 1,
                }
            }
        }
        Self::import_batch(store, &rows, report, cancel)
    }

    fn import_batch(
        store: &sqlite::Store,
        batch: &[(String, Vec<u8>)],
        report: &mut LegacyImportReport,
        cancel: &Cancellation,
    ) -> Result<()> {
        if batch.is_empty() {
            return cancel.check();
        }
        match store.write_batch(
            batch
                .iter()
                .map(|(id, value)| (id.as_str(), value.as_slice())),
            cancel,
        ) {
            Ok(()) => {
                report.committed_batches += 1;
                report.imported += batch.len();
                Ok(())
            }
            Err(Error::Cancelled) => Err(Error::Cancelled),
            Err(_) => {
                report.failed_batches += 1;
                Ok(())
            }
        }
    }

    #[cfg(test)]
    pub(super) fn legacy_only_for_test(&mut self) {
        self.sqlite = None;
    }

    #[cfg(test)]
    pub(super) fn sqlite_path_for_test(&self) -> &Path {
        self.sqlite.as_ref().unwrap().path()
    }
}
