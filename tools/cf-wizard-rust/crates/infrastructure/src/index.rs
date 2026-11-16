//! Read/import the verified v1 facts contract without executing its Python producer.
use crate::canonical::{GitInputs, InputEntry, ascii_json};
use crate::facts_cache;
use cf_application::FactStore;
use cf_domain::source_facts::{FactVersions, SourceFacts};
use cf_domain::{Error, Facts, Member, Result};
use flate2::{Compression, read::ZlibDecoder, write::ZlibEncoder};
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    fs::OpenOptions as FileOptions,
    io::{Read, Write},
    path::Path,
};

pub const APPLICATION_ID: i32 = 0x43464958;
pub const LEGACY_HELPER: &str = "efe67eda5f95475757efefd75c979d8144ffd7480934307db487155c87ea5699";
pub const FACTS_CONTRACT: &str = "partial-cf-facts-v1";
pub enum Proof<'a> {
    Local(&'a str),
    Git(&'a GitInputs),
}
fn invalid(message: &str) -> Error {
    Error::Invalid(message.into())
}
pub fn encode_facts(facts: &Facts) -> Result<Vec<u8>> {
    fn members(items: &[Member]) -> Vec<Value> {
        items
            .iter()
            .map(|m| serde_json::json!([m.name, m.tag, m.types, members(&m.columns)]))
            .collect()
    }
    let payload = ascii_json(&(facts.targets.clone(), members(&facts.members)))?;
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::fast());
    encoder
        .write_all(payload.as_bytes())
        .map_err(crate::external)?;
    encoder.finish().map_err(crate::external)
}
pub fn decode_facts(data: &[u8]) -> Result<Facts> {
    const MAX_FACTS: u64 = 64 * 1024 * 1024;
    let mut bytes = vec![];
    ZlibDecoder::new(data)
        .take(MAX_FACTS + 1)
        .read_to_end(&mut bytes)
        .map_err(crate::external)?;
    if bytes.len() as u64 > MAX_FACTS {
        return Err(invalid("Слишком большой payload фактов SQLite"));
    }
    let value: Value = serde_json::from_slice(&bytes).map_err(crate::external)?;
    fn strings(value: &Value) -> Result<std::collections::BTreeSet<String>> {
        value
            .as_array()
            .ok_or_else(|| invalid("Некорректный список типов facts"))?
            .iter()
            .map(|s| {
                s.as_str()
                    .map(str::to_owned)
                    .ok_or_else(|| invalid("Некорректный тип facts"))
            })
            .collect()
    }
    fn members(value: &Value) -> Result<Vec<Member>> {
        value
            .as_array()
            .ok_or_else(|| invalid("Некорректный список членов facts"))?
            .iter()
            .map(|row| {
                let row = row
                    .as_array()
                    .filter(|v| v.len() == 4)
                    .ok_or_else(|| invalid("Некорректный член facts"))?;
                Ok(Member {
                    name: row[0]
                        .as_str()
                        .ok_or_else(|| invalid("Нет имени члена facts"))?
                        .into(),
                    tag: row[1]
                        .as_str()
                        .ok_or_else(|| invalid("Нет типа члена facts"))?
                        .into(),
                    types: strings(&row[2])?,
                    columns: members(&row[3])?,
                })
            })
            .collect()
    }
    let row = value
        .as_array()
        .filter(|v| v.len() == 2)
        .ok_or_else(|| invalid("Некорректный корень facts"))?;
    Ok(Facts {
        targets: strings(&row[0])?,
        members: members(&row[1])?,
    })
}
pub fn header(db: &Connection) -> Result<BTreeMap<String, String>> {
    let mut query = db
        .prepare("SELECT key,value FROM metadata")
        .map_err(crate::external)?;
    query
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
        .map_err(crate::external)?
        .collect::<rusqlite::Result<_>>()
        .map_err(crate::external)
}
fn integrity(db: &Connection) -> Result<()> {
    let id: i32 = db
        .query_row("PRAGMA application_id", [], |r| r.get(0))
        .map_err(crate::external)?;
    let quick: String = db
        .query_row("PRAGMA quick_check", [], |r| r.get(0))
        .map_err(crate::external)?;
    if id != APPLICATION_ID || quick != "ok" {
        return Err(invalid("Неподходящий или повреждённый индекс SQLite"));
    }
    let mut check = db
        .prepare("PRAGMA foreign_key_check")
        .map_err(crate::external)?;
    if check.exists([]).map_err(crate::external)? {
        return Err(invalid("Нарушена целостность связей SQLite"));
    }
    Ok(())
}
pub fn read_manifest(db: &Connection, meta: &BTreeMap<String, String>) -> Result<GitInputs> {
    if meta.get("git_manifest").map(String::as_str) != Some("1") {
        return Err(invalid("Обновите старый Git-индекс по кнопке"));
    }
    let mut query = db
        .prepare("SELECT path,oid,size,owner,roles FROM git_inputs ORDER BY path")
        .map_err(crate::external)?;
    let entries = query
        .query_map([], |r| {
            Ok(InputEntry {
                path: r.get(0)?,
                oid: r.get(1)?,
                size: r.get(2)?,
                owner: r.get(3)?,
                roles: r.get(4)?,
            })
        })
        .map_err(crate::external)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(crate::external)?;
    let manifest = GitInputs {
        source_key: meta
            .get("git_source")
            .ok_or_else(|| invalid("Нет источника манифеста"))?
            .clone(),
        revision: meta
            .get("indexed_revision")
            .ok_or_else(|| invalid("Нет ревизии манифеста"))?
            .clone(),
        entries,
    };
    if meta.get("git_fingerprint") != Some(&manifest.fingerprint()?) {
        return Err(invalid("Повреждён манифест Git-индекса"));
    }
    Ok(manifest)
}
pub(crate) fn validate_owned_semantics(
    db: &Connection,
    meta: &BTreeMap<String, String>,
) -> Result<()> {
    if meta.get("schema").map(String::as_str) != Some("2")
        || meta.get("engine").map(String::as_str) != Some("cf-wizard-rust")
        || meta.get("facts_contract").map(String::as_str) != Some(facts_cache::CONTRACT)
    {
        return Err(invalid("Неизвестный owned SourceFacts cache contract"));
    }
    let stored: FactVersions = serde_json::from_str(
        meta.get("reader_versions")
            .ok_or_else(|| invalid("Нет версии SourceFacts readers"))?,
    )
    .map_err(crate::external)?;
    let current = FactVersions::current();
    if facts_cache::OBJECT_FACETS
        .iter()
        .chain(facts_cache::REGISTRAR_FACETS)
        .any(|facet| !stored.compatible(&current, *facet))
    {
        return Err(invalid(
            "Изменилась семантика читателя: обновите индекс по кнопке",
        ));
    }
    for (sql, facets) in [
        (
            "SELECT COUNT(*) FROM objects AS o LEFT JOIN object_semantics AS s ON s.name=o.name WHERE s.signature IS NULL OR s.signature<>?",
            facts_cache::OBJECT_FACETS,
        ),
        (
            "SELECT COUNT(*) FROM documents AS d LEFT JOIN document_semantics AS s ON s.file=d.file WHERE s.signature IS NULL OR s.signature<>?",
            facts_cache::REGISTRAR_FACETS,
        ),
    ] {
        let signature = facts_cache::signature(&current, facets)?;
        let stale: u64 = db
            .query_row(sql, [signature], |row| row.get(0))
            .map_err(crate::external)?;
        if stale != 0 {
            return Err(invalid(
                "В SQLite остались устаревшие facets: обновите индекс по кнопке",
            ));
        }
    }
    Ok(())
}
pub struct IndexReader {
    db: Connection,
    pinned: bool,
    owned: bool,
}
impl IndexReader {
    pub fn open(path: &Path, proof: Proof<'_>) -> Result<Self> {
        let db = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(crate::external)?;
        db.execute_batch("PRAGMA trusted_schema=OFF; BEGIN DEFERRED;")
            .map_err(crate::external)?;
        integrity(&db)?;
        let meta = header(&db)?;
        let owned = meta.get("schema").map(String::as_str) == Some("2")
            && meta.get("facts_contract").map(String::as_str) == Some(facts_cache::CONTRACT);
        if owned {
            validate_owned_semantics(&db, &meta)?;
        } else {
            if meta.get("schema").map(String::as_str) != Some("1")
                || meta.get("helper").map(String::as_str) != Some(LEGACY_HELPER)
            {
                return Err(invalid("Неизвестный формат или версия алгоритма SQLite"));
            }
            if meta.get("engine").is_some_and(|s| s != "cf-wizard-rust")
                || meta
                    .get("facts_contract")
                    .is_some_and(|s| s != FACTS_CONTRACT)
            {
                return Err(invalid("Неизвестный контракт Rust facts"));
            }
        }
        let pinned = match proof {
            Proof::Local(identity) => {
                if meta.get("source").map(String::as_str) != Some(identity)
                    || meta.contains_key("git_source")
                {
                    return Err(invalid("Индекс относится к другой XML-выгрузке"));
                }
                false
            }
            Proof::Git(current) => {
                let stored = read_manifest(&db, &meta)?;
                if stored.source_key != current.source_key
                    || stored.fingerprint()? != current.fingerprint()?
                {
                    return Err(invalid("XML изменены: обновите индекс по кнопке"));
                }
                true
            }
        };
        Ok(Self { db, pinned, owned })
    }
}
impl FactStore for IndexReader {
    fn source_registrar_snapshot<'a>(
        &'a self,
        documents: Option<&BTreeMap<String, Vec<u8>>>,
    ) -> Result<Option<Box<dyn cf_application::RegistrarLookup + 'a>>> {
        if self.owned {
            self.registrar_snapshot(documents)
        } else {
            Ok(None)
        }
    }
    fn source_facts(&self, name: &str, content_hash: Option<&[u8]>) -> Result<Option<SourceFacts>> {
        if !self.owned {
            return Ok(None);
        }
        if !self.pinned && content_hash.is_none() {
            return Err(invalid("Локальные SourceFacts требуют hash XML"));
        }
        let row: Option<(Vec<u8>, Vec<u8>)> = self
            .db
            .query_row(
                "SELECT content_hash,facts FROM objects WHERE name=?",
                [name],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .map_err(crate::external)?;
        match row {
            Some((hash, data)) if content_hash.is_none_or(|expected| expected == hash) => {
                match facts_cache::decode(&data, name, &hash, &FactVersions::current()) {
                    Ok(facts) => Ok(Some(facts)),
                    Err(_) if !self.pinned => Ok(None),
                    Err(_) => Err(invalid(
                        "SourceFacts SQLite повреждены или несовместимы. Выберите новую папку индекса и создайте его заново.",
                    )),
                }
            }
            _ => Ok(None),
        }
    }
    fn lookup(&self, name: &str, content_hash: Option<&[u8]>) -> Result<Option<Facts>> {
        if self.owned {
            return self
                .source_facts(name, content_hash)?
                .map(|facts| facts.structural_facts())
                .transpose();
        }
        if !self.pinned && content_hash.is_none() {
            return Err(invalid("Локальные facts требуют проверки hash XML"));
        }
        let row: Option<(Vec<u8>, Vec<u8>)> = self
            .db
            .query_row(
                "SELECT content_hash,facts FROM objects WHERE name=?",
                [name],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .map_err(crate::external)?;
        match row {
            Some((hash, data)) if content_hash.is_none_or(|expected| expected == hash) => {
                match decode_facts(&data) {
                    Ok(facts) => Ok(Some(facts)),
                    Err(_) if !self.pinned => Ok(None),
                    Err(_) => Err(invalid(
                        "Повреждены facts SQLite. Выберите новую папку индекса и создайте его заново.",
                    )),
                }
            }
            _ => Ok(None),
        }
    }
    fn registrar_snapshot<'a>(
        &'a self,
        documents: Option<&BTreeMap<String, Vec<u8>>>,
    ) -> Result<Option<Box<dyn cf_application::RegistrarLookup + 'a>>> {
        if !self.pinned {
            let Some(expected) = documents else {
                return Ok(None);
            };
            let mut statement = self
                .db
                .prepare("SELECT file,records_hash FROM documents ORDER BY file")
                .map_err(crate::external)?;
            let actual = statement
                .query_map([], |r| {
                    Ok((r.get::<_, String>(0)?, r.get::<_, Vec<u8>>(1)?))
                })
                .map_err(crate::external)?
                .collect::<rusqlite::Result<BTreeMap<_, _>>>()
                .map_err(crate::external)?;
            if &actual != expected {
                return Ok(None);
            }
        }
        Ok(Some(Box::new(RegistrarReader { db: &self.db })))
    }
}
struct RegistrarReader<'a> {
    db: &'a Connection,
}
impl cf_application::RegistrarLookup for RegistrarReader<'_> {
    fn lookup(&self, register: &str) -> Result<Vec<String>> {
        let mut query = self
            .db
            .prepare("SELECT file FROM registrar_links WHERE target=? ORDER BY file,ordinal")
            .map_err(crate::external)?;
        query
            .query_map([register], |row| {
                let file: String = row.get(0)?;
                Ok(format!(
                    "Document.{}",
                    file.strip_suffix(".xml").unwrap_or(&file)
                ))
            })
            .map_err(crate::external)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(crate::external)
    }
}
/// Atomic first import only; never overwrites the original or an existing target.
pub fn import_legacy(original: &Path, target: &Path, proof: Proof<'_>) -> Result<()> {
    if original == target || target.exists() {
        return Err(invalid("Для Rust-импорта нужен отдельный новый файл"));
    }
    let parent = target
        .parent()
        .ok_or_else(|| invalid("Нет папки Rust-индекса"))?;
    let lock_path = target.with_extension("sqlite3.lock");
    let lock = FileOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(lock_path)
        .map_err(crate::external)?;
    fs2::FileExt::try_lock_exclusive(&lock)
        .map_err(|_| invalid("Индекс обновляется в другом процессе"))?;
    let original = IndexReader::open(original, proof)?;
    let draft = tempfile::NamedTempFile::new_in(parent)
        .map_err(crate::external)?
        .into_temp_path();
    original
        .db
        .backup("main", &draft, None)
        .map_err(crate::external)?;
    {
        let db = Connection::open(&draft).map_err(crate::external)?;
        db.execute_batch("PRAGMA trusted_schema=OFF; BEGIN IMMEDIATE;")
            .map_err(crate::external)?;
        db.execute(
            "INSERT OR REPLACE INTO metadata VALUES (?,?)",
            params!["engine", "cf-wizard-rust"],
        )
        .map_err(crate::external)?;
        db.execute(
            "INSERT OR REPLACE INTO metadata VALUES (?,?)",
            params!["facts_contract", FACTS_CONTRACT],
        )
        .map_err(crate::external)?;
        db.execute_batch("COMMIT;").map_err(crate::external)?;
        integrity(&db)?;
    }
    draft.persist_noclobber(target).map_err(crate::external)?;
    fs2::FileExt::unlock(&lock).map_err(crate::external)?;
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    use sha2::Digest;
    const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/fixtures/");
    fn fixture(name: &str) -> std::path::PathBuf {
        Path::new(FIXTURES).join(name)
    }
    fn oracle() -> Value {
        serde_json::from_slice(&std::fs::read(fixture("oracle-0.7.3.json")).unwrap()).unwrap()
    }
    #[test]
    fn imports_actual_python_facts_without_modifying_original() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("old.sqlite3");
        std::fs::copy(fixture("legacy-v1-local.sqlite3"), &src).unwrap();
        let before = std::fs::read(&src).unwrap();
        let modified = src.metadata().unwrap().modified().unwrap();
        let dst = tmp.path().join("rust.sqlite3");
        import_legacy(&src, &dst, Proof::Local("fixture-local")).unwrap();
        assert_eq!(before, std::fs::read(&src).unwrap());
        assert_eq!(modified, src.metadata().unwrap().modified().unwrap());
        let index = IndexReader::open(&dst, Proof::Local("fixture-local")).unwrap();
        let hash = hex::decode(oracle()["descriptor_sha256"].as_str().unwrap()).unwrap();
        let facts = index.lookup("Catalog.Тест", Some(&hash)).unwrap().unwrap();
        assert_eq!(facts.members[0].name, "Ссылка");
        assert_eq!(facts.members[1].columns[0].name, "Тип");
        assert_eq!(facts, decode_facts(&encode_facts(&facts).unwrap()).unwrap());
        assert!(
            index
                .lookup("Catalog.Тест", Some(&[0; 32]))
                .unwrap()
                .is_none()
        );
        assert!(index.lookup("Catalog.Тест", None).is_err());
        assert_eq!(
            index
                .registrars(
                    "AccumulationRegister.Остатки",
                    Some(&BTreeMap::from([(
                        "Событие.xml".into(),
                        sha2::Sha256::digest(b"").to_vec()
                    )]))
                )
                .unwrap(),
            Some(vec!["Document.Событие".into()])
        );
    }
    #[test]
    fn python_manifest_matches_and_new_sha_reuses_without_xml() {
        let data = oracle();
        let mut inputs: GitInputs = serde_json::from_value(data["manifest"].clone()).unwrap();
        assert_eq!(
            inputs.fingerprint().unwrap(),
            data["fingerprint"].as_str().unwrap()
        );
        inputs.revision = "b".repeat(40);
        let index =
            IndexReader::open(&fixture("legacy-v1-git.sqlite3"), Proof::Git(&inputs)).unwrap();
        assert!(index.lookup("Catalog.Тест", None).unwrap().is_some());
        inputs.entries[0].oid = "c".repeat(40);
        assert!(IndexReader::open(&fixture("legacy-v1-git.sqlite3"), Proof::Git(&inputs)).is_err());
    }
    #[test]
    fn incompatible_source_and_existing_target_are_not_overwritten() {
        let tmp = tempfile::tempdir().unwrap();
        let dst = tmp.path().join("rust.sqlite3");
        assert!(
            import_legacy(
                &fixture("legacy-v1-local.sqlite3"),
                &dst,
                Proof::Local("wrong")
            )
            .is_err()
        );
        assert!(!dst.exists());
        std::fs::write(&dst, b"keep me").unwrap();
        assert!(
            import_legacy(
                &fixture("legacy-v1-local.sqlite3"),
                &dst,
                Proof::Local("fixture-local")
            )
            .is_err()
        );
        assert_eq!(std::fs::read(dst).unwrap(), b"keep me");
    }
    #[test]
    fn corrupt_facts_are_an_error_not_silent_empty_dependencies() {
        assert!(decode_facts(b"bad zlib").is_err());
    }
    #[test]
    fn malformed_facts_fall_back_locally_and_unknown_contracts_are_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("index.sqlite3");
        std::fs::copy(fixture("legacy-v1-local.sqlite3"), &path).unwrap();
        {
            let db = Connection::open(&path).unwrap();
            db.execute(
                "UPDATE objects SET facts=x'FF' WHERE name='Catalog.Тест'",
                [],
            )
            .unwrap();
        }
        let hash = hex::decode(oracle()["descriptor_sha256"].as_str().unwrap()).unwrap();
        let reader = IndexReader::open(&path, Proof::Local("fixture-local")).unwrap();
        assert!(
            reader
                .lookup("Catalog.Тест", Some(&hash))
                .unwrap()
                .is_none()
        );
        drop(reader);
        for key in ["schema", "helper", "facts_contract"] {
            std::fs::copy(fixture("legacy-v1-local.sqlite3"), &path).unwrap();
            {
                let db = Connection::open(&path).unwrap();
                db.execute(
                    "INSERT OR REPLACE INTO metadata VALUES (?,'unknown')",
                    [key],
                )
                .unwrap();
            }
            assert!(
                IndexReader::open(&path, Proof::Local("fixture-local")).is_err(),
                "{key}"
            );
        }
    }
}
