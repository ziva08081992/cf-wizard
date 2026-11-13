//! Persistent index lifecycle: read-only readiness and explicit atomic XML-delta updates.
#[cfg(test)]
#[path = "indexing_tests.rs"]
mod tests;
use crate::{
    canonical::{GitInputs, InputEntry, sha256},
    engine::{metadata_listing, records_fragment},
    index::{
        APPLICATION_ID, FACTS_CONTRACT, IndexReader, LEGACY_HELPER, Proof, encode_facts, header,
        read_manifest,
    },
    metadata::{extract_facts, scan},
    source::{no_links, normal_path},
};
use cf_application::{Cancellation, MetadataSource};
use cf_domain::{Error, Result};
use rusqlite::{Connection, OpenFlags, params};
use serde::Serialize;
use sha1::Sha1;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

const SCHEMA:&str="
PRAGMA application_id=1128679768;
CREATE TABLE IF NOT EXISTS metadata(key TEXT PRIMARY KEY,value TEXT NOT NULL) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS objects(name TEXT PRIMARY KEY,content_hash BLOB NOT NULL,facts BLOB NOT NULL) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS documents(file TEXT PRIMARY KEY,records_hash BLOB NOT NULL) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS registrar_links(file TEXT,ordinal INTEGER,target TEXT NOT NULL,PRIMARY KEY(file,ordinal),FOREIGN KEY(file) REFERENCES documents(file) ON DELETE CASCADE) WITHOUT ROWID;
CREATE INDEX IF NOT EXISTS registrars_by_target ON registrar_links(target,file,ordinal);
CREATE TABLE IF NOT EXISTS git_inputs(path TEXT PRIMARY KEY,oid TEXT NOT NULL,size INTEGER NOT NULL,owner TEXT NOT NULL,roles TEXT NOT NULL) WITHOUT ROWID;
";
#[derive(Clone, Debug, Serialize)]
pub struct IndexStatus {
    pub path: Option<PathBuf>,
    pub preferred: PathBuf,
    pub found: bool,
    pub ready: bool,
    pub info: String,
}
#[derive(Clone, Debug, Default, Serialize)]
pub struct IndexUpdate {
    pub path: PathBuf,
    pub inputs: usize,
    pub added: usize,
    pub changed: usize,
    pub removed: usize,
    pub unchanged: usize,
    pub input_added: usize,
    pub input_removed: usize,
    pub input_changed: usize,
    pub reused_inputs: usize,
    pub xml_reads: usize,
    pub registrars_changed: usize,
    pub migrated: bool,
    pub bytes: u64,
}
pub fn path_for(source: &dyn MetadataSource, directory: &Path) -> Result<PathBuf> {
    if directory.as_os_str().is_empty() {
        return Err(Error::Invalid("Выберите папку индекса".into()));
    }
    no_links(directory)?;
    let folder = crate::source::canonical_future(directory)?;
    if folder
        .components()
        .any(|p| p.as_os_str().to_string_lossy().eq_ignore_ascii_case(".git"))
    {
        return Err(Error::Invalid("Папка индекса должна быть вне .git".into()));
    }
    if let Some(root) = source.materialized_root() {
        let f = normal_path(&folder);
        let r = normal_path(root);
        let separator = if cfg!(windows) { '\\' } else { '/' };
        if f == r || f.starts_with(&format!("{}{separator}", r.trim_end_matches(separator))) {
            return Err(Error::Invalid(
                "Выберите папку индекса вне XML-выгрузки".into(),
            ));
        }
    }
    let identity = source
        .manifest()
        .map_or_else(|| source.identity(), |m| m.source_key.as_str());
    let name = source.catalog().name.chars().take(64).collect::<String>();
    Ok(folder.join(format!(
        "{name}-{}.rust.sqlite3",
        &sha256(identity.as_bytes())[..20]
    )))
}
fn candidates(preferred: &Path) -> Result<Vec<PathBuf>> {
    let mut all = vec![];
    if preferred.exists() {
        all.push(preferred.into());
    }
    let Some(parent) = preferred.parent() else {
        return Ok(all);
    };
    let listing = match fs::read_dir(parent) {
        Ok(rows) => rows,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(all),
        Err(e) => return Err(crate::external(e)),
    };
    let mut others = vec![];
    for entry in listing {
        let entry = entry.map_err(crate::external)?;
        let path = entry.path();
        if path != preferred
            && path.extension().is_some_and(|v| v == "sqlite3")
            && entry.file_type().map_err(crate::external)?.is_file()
        {
            others.push(path);
        }
    }
    others.sort();
    all.extend(others);
    Ok(all)
}
fn readonly(path: &Path) -> Result<Connection> {
    no_links(path)?;
    let db = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(crate::external)?;
    db.busy_timeout(Duration::from_secs(5))
        .map_err(crate::external)?;
    db.execute_batch("PRAGMA trusted_schema=OFF; BEGIN DEFERRED;")
        .map_err(crate::external)?;
    let id: i32 = db
        .query_row("PRAGMA application_id", [], |r| r.get(0))
        .map_err(crate::external)?;
    if id != APPLICATION_ID {
        return Err(Error::Invalid("Это не индекс CF Wizard".into()));
    }
    Ok(db)
}
fn supported(meta: &BTreeMap<String, String>) -> bool {
    meta.get("schema").map(String::as_str) == Some("1")
        && meta.get("helper").map(String::as_str) == Some(LEGACY_HELPER)
        && meta.get("engine").is_none_or(|v| v == "cf-wizard-rust")
        && meta
            .get("facts_contract")
            .is_none_or(|v| v == FACTS_CONTRACT)
}
fn origin(meta: &BTreeMap<String, String>, source: &dyn MetadataSource) -> bool {
    if let Some(inputs) = source.manifest() {
        if let Some(key) = meta.get("git_source") {
            return key == &inputs.source_key;
        }
        let Some(path) = meta.get("source") else {
            return false;
        };
        let path = Path::new(path);
        path.file_name().is_some_and(|n| n == "config")
            && path
                .parent()
                .and_then(Path::parent)
                .and_then(Path::file_name)
                .is_some_and(|n| {
                    n.to_string_lossy() == inputs.source_key.trim_start_matches("git:")
                })
    } else {
        !meta.contains_key("git_source")
            && meta.get("source").map(String::as_str) == Some(source.identity())
    }
}
fn keys(db: &Connection, table: &str, column: &str) -> Result<BTreeSet<String>> {
    let sql = match (table, column) {
        ("objects", "name") => "SELECT name FROM objects",
        ("documents", "file") => "SELECT file FROM documents",
        _ => return Err(Error::Invalid("Недопустимая таблица индекса".into())),
    };
    let mut q = db.prepare(sql).map_err(crate::external)?;
    q.query_map([], |r| r.get(0))
        .map_err(crate::external)?
        .collect::<rusqlite::Result<BTreeSet<_>>>()
        .map_err(crate::external)
}
fn complete(db: &Connection, inputs: &GitInputs) -> Result<bool> {
    let objects = inputs
        .entries
        .iter()
        .filter(|e| e.roles.split(',').any(|r| r == "descriptor"))
        .map(|e| e.owner.clone())
        .collect::<BTreeSet<_>>();
    let docs = inputs
        .entries
        .iter()
        .filter(|e| e.roles.split(',').any(|r| r == "registrar"))
        .map(|e| e.path.rsplit('/').next().expect("path").to_owned())
        .collect::<BTreeSet<_>>();
    Ok(keys(db, "objects", "name")? == objects && keys(db, "documents", "file")? == docs)
}
pub fn status(source: &dyn MetadataSource, directory: &Path) -> Result<IndexStatus> {
    let preferred = path_for(source, directory)?;
    let mut found = false;
    let mut reason = "Индекс ещё не создан".to_owned();
    for path in candidates(&preferred)? {
        let Ok(db) = readonly(&path) else {
            if path == preferred {
                found = true;
                reason = "Индекс повреждён или имеет другой формат".into();
            }
            continue;
        };
        let Ok(meta) = header(&db) else {
            continue;
        };
        if !origin(&meta, source) {
            continue;
        }
        found = true;
        if !supported(&meta) {
            reason = "Изменилась версия фактов: обновите индекс".into();
            continue;
        }
        let proof = source
            .manifest()
            .map_or_else(|| Proof::Local(source.identity()), Proof::Git);
        match IndexReader::open(&path, proof) {
            Ok(_reader) => {
                if let Some(inputs) = source.manifest()
                    && !complete(&db, inputs)?
                {
                    reason = "Индекс неполон: обновите по кнопке".into();
                    continue;
                }
                return Ok(IndexStatus {
                    path: Some(path.clone()),
                    preferred,
                    found: true,
                    ready: true,
                    info: format!(
                        "{} · {}",
                        path.file_name().unwrap_or_default().to_string_lossy(),
                        if source.manifest().is_some() {
                            "соответствует XML ветки"
                        } else {
                            "записи сверяются с XML при использовании"
                        }
                    ),
                });
            }
            Err(e) => reason = e.to_string(),
        }
    }
    Ok(IndexStatus {
        path: None,
        preferred,
        found,
        ready: false,
        info: reason,
    })
}
#[derive(Eq, PartialEq)]
struct Stamp {
    len: u64,
    modified: Option<SystemTime>,
    created: Option<SystemTime>,
}
fn stamp(path: &Path) -> Result<Option<Stamp>> {
    match fs::metadata(path) {
        Ok(m) => Ok(Some(Stamp {
            len: m.len(),
            modified: m.modified().ok(),
            created: m.created().ok(),
        })),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(crate::external(e)),
    }
}
fn inputs(source: &dyn MetadataSource, cancel: &Cancellation) -> Result<Vec<InputEntry>> {
    let descriptors: BTreeMap<_, _> = source
        .catalog()
        .objects
        .values()
        .map(|o| (o.key.0.clone(), o.descriptor.clone()))
        .collect();
    if let Some(inputs) = source.manifest() {
        inputs.validate()?;
        let owners: BTreeMap<_, _> = inputs
            .entries
            .iter()
            .filter(|e| e.roles.split(',').any(|r| r == "descriptor"))
            .map(|e| (e.owner.clone(), e.path.clone()))
            .collect();
        if owners != descriptors {
            return Err(Error::Invalid(
                "Состав XML не соответствует Git-манифесту".into(),
            ));
        }
        return Ok(inputs.entries.clone());
    }
    let mut rows = BTreeMap::<String, InputEntry>::new();
    for (owner, path) in descriptors {
        rows.insert(
            path.clone(),
            InputEntry {
                path,
                oid: String::new(),
                size: 0,
                owner,
                roles: "descriptor".into(),
            },
        );
    }
    for doc in metadata_listing(source, "Documents", cancel)? {
        let path = format!("Documents/{}", doc.name);
        rows.entry(path.clone())
            .and_modify(|r| r.roles = "descriptor,registrar".into())
            .or_insert(InputEntry {
                path,
                oid: String::new(),
                size: 0,
                owner: String::new(),
                roles: "registrar".into(),
            });
    }
    Ok(rows.into_values().collect())
}
fn hashes(db: &Connection, table: &str) -> Result<BTreeMap<String, Vec<u8>>> {
    let sql = if table == "objects" {
        "SELECT name,content_hash FROM objects"
    } else {
        "SELECT file,records_hash FROM documents"
    };
    let mut q = db.prepare(sql).map_err(crate::external)?;
    q.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .map_err(crate::external)?
        .collect::<rusqlite::Result<_>>()
        .map_err(crate::external)
}
fn verify_blob(entry: &InputEntry, bytes: &[u8]) -> Result<()> {
    if bytes.len() as u64 != entry.size {
        return Err(Error::Invalid(format!(
            "Размер Git blob не совпадает: {}",
            entry.path
        )));
    }
    let prefix = format!("blob {}\0", bytes.len());
    let actual = if entry.oid.len() == 40 {
        let mut h = Sha1::new();
        h.update(prefix.as_bytes());
        h.update(bytes);
        hex::encode(h.finalize())
    } else {
        let mut h = Sha256::new();
        h.update(prefix.as_bytes());
        h.update(bytes);
        hex::encode(h.finalize())
    };
    if actual != entry.oid {
        return Err(Error::Invalid(format!(
            "Git blob повреждён: {}",
            entry.path
        )));
    }
    Ok(())
}
pub fn update(
    source: &dyn MetadataSource,
    directory: &Path,
    cancel: &Cancellation,
    progress: &dyn Fn(usize, usize),
) -> Result<IndexUpdate> {
    cancel.check()?;
    let proof = source.snapshot(cancel)?;
    let path = path_for(source, directory)?;
    let parent = path
        .parent()
        .ok_or_else(|| Error::Invalid("Нет папки индекса".into()))?;
    fs::create_dir_all(parent).map_err(crate::external)?;
    no_links(parent)?;
    no_links(&path)?;
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path.with_extension("sqlite3.lock"))
        .map_err(crate::external)?;
    fs2::FileExt::try_lock_exclusive(&lock)
        .map_err(|_| Error::Invalid("Индекс обновляется в другом процессе".into()))?;
    let before = stamp(&path)?;
    let rows = inputs(source, cancel)?;
    let mut report = IndexUpdate {
        path: path.clone(),
        inputs: rows.len(),
        ..IndexUpdate::default()
    };
    if path.exists() {
        let db = readonly(&path)?;
        let meta = header(&db)?;
        if !origin(&meta, source) {
            return Err(Error::Invalid(
                "Файл индекса относится к другому источнику".into(),
            ));
        }
        if let Some(inputs) = source.manifest()
            && supported(&meta)
            && read_manifest(&db, &meta)
                .is_ok_and(|old| old.fingerprint().ok() == inputs.fingerprint().ok())
            && complete(&db, inputs)?
        {
            let check: String = db
                .query_row("PRAGMA quick_check", [], |r| r.get(0))
                .map_err(crate::external)?;
            if check == "ok" && foreign_keys_valid(&db)? {
                report.unchanged = source.catalog().objects.len();
                report.reused_inputs = rows.len();
                report.bytes = fs::metadata(&path).map_err(crate::external)?.len();
                return Ok(report);
            }
        }
    }
    let draft = tempfile::NamedTempFile::new_in(parent)
        .map_err(crate::external)?
        .into_temp_path();
    let mut old_inputs = None;
    for candidate in candidates(&path)? {
        let Ok(db) = readonly(&candidate) else {
            continue;
        };
        let Ok(meta) = header(&db) else {
            continue;
        };
        if !supported(&meta) || !origin(&meta, source) {
            continue;
        }
        let manifest = if meta.contains_key("git_manifest") {
            match read_manifest(&db, &meta) {
                Ok(inputs) => Some(inputs),
                Err(_) => continue,
            }
        } else {
            None
        };
        let quick: String = db
            .query_row("PRAGMA quick_check", [], |r| r.get(0))
            .map_err(crate::external)?;
        if quick != "ok" || !foreign_keys_valid(&db)? {
            continue;
        }
        db.backup("main", &draft, None).map_err(crate::external)?;
        old_inputs = manifest;
        report.migrated = candidate != path;
        break;
    }
    let mut db = Connection::open(&draft).map_err(crate::external)?;
    db.execute_batch("PRAGMA foreign_keys=ON; PRAGMA trusted_schema=OFF;")
        .map_err(crate::external)?;
    db.execute_batch(SCHEMA).map_err(crate::external)?;
    let known = hashes(&db, "objects")?;
    let documents = hashes(&db, "documents")?;
    let old: BTreeMap<_, _> = old_inputs
        .as_ref()
        .map(|i| {
            i.entries
                .iter()
                .map(|e| (e.path.clone(), e.clone()))
                .collect()
        })
        .unwrap_or_default();
    let current: BTreeMap<_, _> = rows.iter().map(|e| (e.path.clone(), e.clone())).collect();
    report.input_added = current.keys().filter(|p| !old.contains_key(*p)).count();
    report.input_removed = old.keys().filter(|p| !current.contains_key(*p)).count();
    report.input_changed = current
        .iter()
        .filter(|(p, e)| old.get(*p).is_some_and(|previous| previous != *e))
        .count();
    let fetch: Vec<_> = rows
        .iter()
        .filter(|e| {
            let descriptor = e.roles.split(',').any(|r| r == "descriptor");
            let registrar = e.roles.split(',').any(|r| r == "registrar");
            let filename = e.path.rsplit('/').next().expect("path");
            let complete = (!descriptor || known.contains_key(&e.owner))
                && (!registrar || documents.contains_key(filename));
            (descriptor || registrar)
                && !(source.manifest().is_some() && old.get(&e.path) == Some(*e) && complete)
        })
        .map(|e| e.path.clone())
        .collect();
    source.prefetch(&fetch, cancel)?;
    let transaction = db.transaction().map_err(crate::external)?;
    let owners: BTreeSet<_> = rows
        .iter()
        .filter(|e| e.roles.split(',').any(|r| r == "descriptor"))
        .map(|e| e.owner.clone())
        .collect();
    for name in known.keys() {
        if !owners.contains(name) {
            transaction
                .execute("DELETE FROM objects WHERE name=?", [name])
                .map_err(crate::external)?;
            report.removed += 1;
        }
    }
    let doc_names: BTreeSet<_> = rows
        .iter()
        .filter(|e| e.roles.split(',').any(|r| r == "registrar"))
        .map(|e| e.path.rsplit('/').next().expect("path").to_owned())
        .collect();
    for name in documents.keys() {
        if !doc_names.contains(name) {
            transaction
                .execute("DELETE FROM documents WHERE file=?", [name])
                .map_err(crate::external)?;
        }
    }
    for (done, entry) in rows.iter().enumerate() {
        cancel.check()?;
        let descriptor = entry.roles.split(',').any(|r| r == "descriptor");
        let registrar = entry.roles.split(',').any(|r| r == "registrar");
        let filename = entry.path.rsplit('/').next().expect("path");
        let complete_row = (!descriptor || known.contains_key(&entry.owner))
            && (!registrar || documents.contains_key(filename));
        if source.manifest().is_some() && old.get(&entry.path) == Some(entry) && complete_row {
            report.reused_inputs += 1;
            if descriptor {
                report.unchanged += 1;
            }
            progress(done + 1, rows.len());
            continue;
        }
        // Configuration was already verified and parsed by the immutable source factory.
        if !descriptor && !registrar {
            progress(done + 1, rows.len());
            continue;
        }
        let data = source.read(&entry.path, cancel)?;
        if source.manifest().is_some() {
            verify_blob(entry, &data)?;
        }
        report.xml_reads += 1;
        if descriptor {
            let digest = Sha256::digest(&data).to_vec();
            if known.get(&entry.owner) == Some(&digest) {
                report.unchanged += 1;
            } else {
                let facts = encode_facts(&extract_facts(&data)?)?;
                transaction.execute("INSERT INTO objects VALUES (?,?,?) ON CONFLICT(name) DO UPDATE SET content_hash=excluded.content_hash,facts=excluded.facts",params![&entry.owner,&digest,&facts]).map_err(crate::external)?;
                if known.contains_key(&entry.owner) {
                    report.changed += 1;
                } else {
                    report.added += 1;
                }
            }
        }
        if registrar {
            let fragment = records_fragment(&data);
            let digest = Sha256::digest(fragment).to_vec();
            if documents.get(filename) != Some(&digest) {
                transaction
                    .execute("DELETE FROM registrar_links WHERE file=?", [filename])
                    .map_err(crate::external)?;
                transaction.execute("INSERT INTO documents VALUES (?,?) ON CONFLICT(file) DO UPDATE SET records_hash=excluded.records_hash",params![filename,digest]).map_err(crate::external)?;
                for (ordinal, reference) in
                    scan(std::str::from_utf8(fragment).map_err(crate::external)?)
                        .into_iter()
                        .enumerate()
                {
                    transaction
                        .execute(
                            "INSERT INTO registrar_links VALUES (?,?,?)",
                            params![filename, ordinal as u64, reference.key()],
                        )
                        .map_err(crate::external)?;
                }
                report.registrars_changed += 1;
            }
        }
        progress(done + 1, rows.len());
    }
    if let Some(inputs) = source.manifest() {
        for removed in old.keys().filter(|p| !current.contains_key(*p)) {
            transaction
                .execute("DELETE FROM git_inputs WHERE path=?", [removed])
                .map_err(crate::external)?;
        }
        for entry in &inputs.entries {
            if old.get(&entry.path) != Some(entry) {
                transaction.execute("INSERT INTO git_inputs VALUES (?,?,?,?,?) ON CONFLICT(path) DO UPDATE SET oid=excluded.oid,size=excluded.size,owner=excluded.owner,roles=excluded.roles",params![entry.path,entry.oid,entry.size,entry.owner,entry.roles]).map_err(crate::external)?;
            }
        }
        for (key, value) in [
            ("git_manifest", "1".to_owned()),
            ("git_source", inputs.source_key.clone()),
            ("git_fingerprint", inputs.fingerprint()?),
            ("indexed_revision", inputs.revision.clone()),
        ] {
            transaction
                .execute(
                    "INSERT OR REPLACE INTO metadata VALUES (?,?)",
                    params![key, value],
                )
                .map_err(crate::external)?;
        }
    }
    for (key, value) in [
        ("schema", "1"),
        ("source", source.identity()),
        ("helper", LEGACY_HELPER),
        ("engine", "cf-wizard-rust"),
        ("facts_contract", FACTS_CONTRACT),
    ] {
        transaction
            .execute(
                "INSERT OR REPLACE INTO metadata VALUES (?,?)",
                params![key, value],
            )
            .map_err(crate::external)?;
    }
    if source.snapshot(cancel)? != proof {
        return Err(Error::Invalid(
            "Исходники изменились во время обновления индекса".into(),
        ));
    }
    cancel.check()?;
    transaction.commit().map_err(crate::external)?;
    let quick: String = db
        .query_row("PRAGMA quick_check", [], |r| r.get(0))
        .map_err(crate::external)?;
    if quick != "ok" || !foreign_keys_valid(&db)? {
        return Err(Error::Invalid("Проверка созданной SQLite не прошла".into()));
    }
    if let Some(inputs) = source.manifest()
        && !complete(&db, inputs)?
    {
        return Err(Error::Invalid("Создан неполный индекс Git".into()));
    }
    drop(db);
    if stamp(&path)? != before {
        return Err(Error::Invalid(
            "Индекс изменился в другом окне; повторите обновление".into(),
        ));
    }
    draft.persist(&path).map_err(crate::external)?;
    report.bytes = fs::metadata(&path).map_err(crate::external)?.len();
    fs2::FileExt::unlock(&lock).map_err(crate::external)?;
    Ok(report)
}
fn foreign_keys_valid(db: &Connection) -> Result<bool> {
    let mut query = db
        .prepare("PRAGMA foreign_key_check")
        .map_err(crate::external)?;
    Ok(!query.exists([]).map_err(crate::external)?)
}
