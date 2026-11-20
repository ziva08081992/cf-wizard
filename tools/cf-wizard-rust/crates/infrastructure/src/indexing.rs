//! Persistent index lifecycle: read-only readiness and explicit atomic XML-delta updates.
use cf_application::profile;
#[cfg(test)]
#[path = "indexing_tests.rs"]
mod tests;
use crate::{
    canonical::{InputEntry, sha256},
    engine::metadata_listing,
    facts_cache,
    index::{
        IndexReader, Proof, complete, header, read_manifest_verified, validate_owned_semantics,
    },
    registrar_facts,
    source::{no_links, normal_path},
    source_reader::SourceDocument,
};
use cf_application::{Cancellation, MetadataSource};
use cf_domain::{
    Error, Result,
    source_facts::{FactVersions, RULE_PROFILE, SourceProof},
};
use rusqlite::{Connection, params};
use serde::Serialize;
use sha1::Sha1;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    time::SystemTime,
};

pub(crate) const SCHEMA:&str="
PRAGMA application_id=1128679768;
CREATE TABLE IF NOT EXISTS metadata(key TEXT PRIMARY KEY,value TEXT NOT NULL) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS objects(name TEXT PRIMARY KEY,content_hash BLOB NOT NULL,facts BLOB NOT NULL) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS object_semantics(name TEXT PRIMARY KEY,signature TEXT NOT NULL,FOREIGN KEY(name) REFERENCES objects(name) ON DELETE CASCADE) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS documents(file TEXT PRIMARY KEY,records_hash BLOB NOT NULL) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS document_semantics(file TEXT PRIMARY KEY,signature TEXT NOT NULL,FOREIGN KEY(file) REFERENCES documents(file) ON DELETE CASCADE) WITHOUT ROWID;
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
    pub metadata_parses: usize,
    pub registrar_parses: usize,
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
        "{name}-{}.source-facts-v3.sqlite3",
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
fn readonly(path: &Path) -> Result<crate::sqlite_cache::CachedConnection> {
    profile::measure("index.candidate_open", "index.readiness", 0, None, || {
        crate::sqlite_cache::CachedConnection::readonly(path)
    })
}

fn supported(meta: &BTreeMap<String, String>) -> bool {
    meta.get("schema").map(String::as_str) == Some("2")
        && meta.get("engine").map(String::as_str) == Some("cf-wizard-rust")
        && crate::index::known_facts_contract(meta)
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
pub fn status(source: &dyn MetadataSource, directory: &Path) -> Result<IndexStatus> {
    profile::measure("index.readiness", "operation", 0, None, || {
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
            match profile::measure("index.open", "index.readiness", 0, None, || {
                IndexReader::from_connection(db, proof)
            }) {
                Ok(_reader) => {
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
fn usable_rows(db: &Connection, descriptor: bool, signature: &str) -> Result<BTreeSet<String>> {
    let sql = if descriptor {
        "SELECT name FROM object_semantics WHERE signature=?"
    } else {
        "SELECT file FROM document_semantics WHERE signature=?"
    };
    let mut query = db.prepare(sql).map_err(crate::external)?;
    query
        .query_map([signature], |row| row.get(0))
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
    update_core(source, directory, cancel, progress, |_| {})
}
#[derive(Clone, Copy)]
enum PublicationPoint {
    BeforeCommit,
    AfterCommit,
}
fn update_core(
    source: &dyn MetadataSource,
    directory: &Path,
    cancel: &Cancellation,
    progress: &dyn Fn(usize, usize),
    publication_hook: impl Fn(PublicationPoint),
) -> Result<IndexUpdate> {
    cancel.check()?;
    source.prepare_index_cache(cancel)?;
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
    let versions = FactVersions::current();
    let object_signature = facts_cache::signature(&versions, facts_cache::OBJECT_FACETS)?;
    let registrar_signature = facts_cache::signature(&versions, facts_cache::REGISTRAR_FACETS)?;
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
            && meta.get("facts_contract").map(String::as_str) == Some(facts_cache::CONTRACT_V3)
            && supported(&meta)
            && validate_owned_semantics(&db, &meta).is_ok()
            && read_manifest_verified(&db, &meta).is_ok_and(|old| {
                inputs
                    .fingerprint()
                    .is_ok_and(|current| old.fingerprint() == current)
            })
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
        if !origin(&meta, source) {
            continue;
        }
        if !supported(&meta) {
            // V1 remains immutable. It can identify the source, not supply V2 extraction.
            if meta.get("schema").map(String::as_str) == Some("1") {
                report.migrated = true;
            }
            continue;
        }
        let manifest = if meta.contains_key("git_manifest") {
            match read_manifest_verified(&db, &meta) {
                Ok(inputs) => Some(inputs.manifest),
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
        if meta.get("facts_contract").map(String::as_str) == Some(facts_cache::CONTRACT) {
            // An old semantic reader requires source re-extraction, never transcode it
            // under today's signature. V3 incremental refresh keeps its existing path.
            if validate_owned_semantics(&db, &meta).is_err() {
                report.migrated = true;
                continue;
            }
            transcode_legacy(&db, &draft, cancel)?;
            profile::note("index.migration.compact", "operation", 1);
        } else {
            db.backup_to(&draft, cancel)?;
        }
        old_inputs = manifest;
        report.migrated = candidate != path;
        break;
    }
    let mut db = crate::sqlite_cache::CachedConnection::writable(&draft)?;
    db.execute_batch("PRAGMA foreign_keys=ON; PRAGMA trusted_schema=OFF;")
        .map_err(crate::external)?;
    db.execute_batch(SCHEMA).map_err(crate::external)?;
    let known = hashes(&db, "objects")?;
    let documents = hashes(&db, "documents")?;
    let usable_objects = usable_rows(&db, true, &object_signature)?;
    let usable_documents = usable_rows(&db, false, &registrar_signature)?;
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
            let complete = (!descriptor || usable_objects.contains(&e.owner))
                && (!registrar || usable_documents.contains(filename));
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
        let complete_row = (!descriptor || usable_objects.contains(&entry.owner))
            && (!registrar || usable_documents.contains(filename));
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
        let descriptor_digest = Sha256::digest(&data).to_vec();
        let descriptor_changed = descriptor
            && (known.get(&entry.owner) != Some(&descriptor_digest)
                || !usable_objects.contains(&entry.owner));
        let parsed = if descriptor_changed {
            report.metadata_parses += 1;
            Some(SourceDocument::descriptor(
                &entry.owner,
                &entry.path,
                &data,
                SourceProof {
                    identity: source.identity().into(),
                    snapshot: proof.clone(),
                    inputs: BTreeMap::new(),
                },
            )?)
        } else {
            None
        };
        if descriptor {
            if !descriptor_changed {
                report.unchanged += 1;
            } else {
                let facts =
                    facts_cache::encode_v3(&parsed.as_ref().expect("parsed descriptor").facts)?;
                transaction.execute("INSERT INTO objects VALUES (?,?,?) ON CONFLICT(name) DO UPDATE SET content_hash=excluded.content_hash,facts=excluded.facts",params![&entry.owner,&descriptor_digest,&facts]).map_err(crate::external)?;
                transaction.execute("INSERT INTO object_semantics VALUES (?,?) ON CONFLICT(name) DO UPDATE SET signature=excluded.signature", params![&entry.owner, &object_signature]).map_err(crate::external)?;
                if known.contains_key(&entry.owner) {
                    report.changed += 1;
                } else {
                    report.added += 1;
                }
            }
        }
        if registrar {
            let registrations = if let Some(document) = parsed.as_ref() {
                registrar_facts::from_view(&document.view, document.object)?
            } else {
                report.registrar_parses += 1;
                registrar_facts::extract(&data)?
            };
            if documents.get(filename) != Some(&registrations.hash)
                || !usable_documents.contains(filename)
            {
                transaction
                    .execute("DELETE FROM registrar_links WHERE file=?", [filename])
                    .map_err(crate::external)?;
                transaction.execute("INSERT INTO documents VALUES (?,?) ON CONFLICT(file) DO UPDATE SET records_hash=excluded.records_hash",params![filename,&registrations.hash]).map_err(crate::external)?;
                for (ordinal, target) in registrations.links {
                    transaction
                        .execute(
                            "INSERT INTO registrar_links VALUES (?,?,?)",
                            params![filename, ordinal as u64, target],
                        )
                        .map_err(crate::external)?;
                }
                transaction.execute("INSERT INTO document_semantics VALUES (?,?) ON CONFLICT(file) DO UPDATE SET signature=excluded.signature", params![filename, &registrar_signature]).map_err(crate::external)?;
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
        ("schema", "2"),
        ("source", source.identity()),
        ("engine", "cf-wizard-rust"),
        ("facts_contract", facts_cache::CONTRACT_V3),
        ("rule_profile", RULE_PROFILE),
    ] {
        transaction
            .execute(
                "INSERT OR REPLACE INTO metadata VALUES (?,?)",
                params![key, value],
            )
            .map_err(crate::external)?;
    }
    transaction
        .execute(
            "INSERT OR REPLACE INTO metadata VALUES (?,?)",
            params![
                "reader_versions",
                serde_json::to_string(&versions).map_err(crate::external)?
            ],
        )
        .map_err(crate::external)?;
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
    validate_owned_semantics(&db, &header(&db)?)?;
    drop(db);
    if stamp(&path)? != before {
        return Err(Error::Invalid(
            "Индекс изменился в другом окне; повторите обновление".into(),
        ));
    }
    report.bytes = fs::metadata(&draft).map_err(crate::external)?.len();
    // Publication starts after this final cancellation checkpoint. A late request
    // cannot retroactively report a successfully replaced index as Cancelled.
    publication_hook(PublicationPoint::BeforeCommit);
    cancel.check()?;
    draft.persist(&path).map_err(crate::external)?;
    drop(lock);
    publication_hook(PublicationPoint::AfterCommit);
    Ok(report)
}
fn foreign_keys_valid(db: &Connection) -> Result<bool> {
    crate::index_integrity::foreign_keys_valid(db)
}

/// Stream a verified V2 snapshot into a fresh compact draft; never mutate the legacy file.
fn transcode_legacy(old: &Connection, draft: &Path, cancel: &Cancellation) -> Result<()> {
    let mut db = crate::sqlite_cache::CachedConnection::writable(draft)?;
    db.execute_batch("PRAGMA foreign_keys=ON; PRAGMA trusted_schema=OFF;")
        .map_err(crate::external)?;
    db.execute_batch(SCHEMA).map_err(crate::external)?;
    let tx = db.transaction().map_err(crate::external)?;
    let mut query = old
        .prepare("SELECT name,content_hash,facts FROM objects ORDER BY name")
        .map_err(crate::external)?;
    let mut rows = query.query([]).map_err(crate::external)?;
    let mut insert = tx
        .prepare("INSERT INTO objects VALUES (?,?,?)")
        .map_err(crate::external)?;
    while let Some(row) = rows.next().map_err(crate::external)? {
        cancel.check()?;
        let name: String = row.get(0).map_err(crate::external)?;
        let hash: Vec<u8> = row.get(1).map_err(crate::external)?;
        let value = row.get_ref(2).map_err(crate::external)?;
        let data = value.as_blob().map_err(crate::external)?;
        let facts = facts_cache::decode(data, &name, &hash, &FactVersions::current())?;
        let compact = facts_cache::encode_v3(&facts)?;
        insert
            .execute(params![name, hash, compact])
            .map_err(crate::external)?;
        profile::note("index.migration.objects", "operation", 1);
    }
    drop(insert);
    drop(rows);
    drop(query);
    // All identifiers are compile-time owned, and each row is released before the next.
    for (table, width) in [
        ("metadata", 2),
        ("object_semantics", 2),
        ("documents", 2),
        ("document_semantics", 2),
        ("registrar_links", 3),
        ("git_inputs", 5),
    ] {
        let mut query = old
            .prepare(&format!("SELECT * FROM {table}"))
            .map_err(crate::external)?;
        let mut rows = query.query([]).map_err(crate::external)?;
        let sql = format!(
            "INSERT INTO {table} VALUES ({})",
            vec!["?"; width].join(",")
        );
        let mut insert = tx.prepare(&sql).map_err(crate::external)?;
        while let Some(row) = rows.next().map_err(crate::external)? {
            cancel.check()?;
            let values = (0..width)
                .map(|i| row.get::<_, rusqlite::types::Value>(i))
                .collect::<rusqlite::Result<Vec<_>>>()
                .map_err(crate::external)?;
            insert
                .execute(rusqlite::params_from_iter(values))
                .map_err(crate::external)?;
        }
    }
    tx.execute(
        "UPDATE metadata SET value=? WHERE key='facts_contract'",
        [facts_cache::CONTRACT_V3],
    )
    .map_err(crate::external)?;
    cancel.check()?;
    tx.commit().map_err(crate::external)?;
    Ok(())
}
