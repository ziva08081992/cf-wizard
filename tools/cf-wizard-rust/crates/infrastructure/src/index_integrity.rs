//! Equivalent FK checks for the exact owned schema, with conservative SQLite fallback.
use cf_application::profile;
use cf_domain::Result;
use rusqlite::{Connection, types::ValueRef};
use std::{
    collections::{BTreeMap, HashSet},
    sync::OnceLock,
};

const MAX_KEYS: usize = 100_000;
const MAX_KEY_BYTES: usize = 4096;
const MAX_SET_BYTES: usize = 16 * 1024 * 1024;
// Includes the Vec, allocator rounding and hash-table capacity/control bytes.
const KEY_OVERHEAD: usize = 128;
type Schema = BTreeMap<String, Option<String>>;
const OWNED_TABLES: [&str; 7] = [
    "metadata",
    "objects",
    "object_semantics",
    "documents",
    "document_semantics",
    "registrar_links",
    "git_inputs",
];
const OBJECT_KEYS: &str = "SELECT name FROM main.objects INDEXED BY sqlite_autoindex_objects_1";
const OBJECT_CHILDREN: &str =
    "SELECT name FROM main.object_semantics INDEXED BY sqlite_autoindex_object_semantics_1";
const DOCUMENT_KEYS: &str =
    "SELECT file FROM main.documents INDEXED BY sqlite_autoindex_documents_1";
const DOCUMENT_CHILDREN: [&str; 2] = [
    "SELECT file FROM main.document_semantics INDEXED BY sqlite_autoindex_document_semantics_1",
    "SELECT file FROM main.registrar_links INDEXED BY sqlite_autoindex_registrar_links_1",
];

fn schema(db: &Connection) -> Result<Schema> {
    let mut q = db
        .prepare("SELECT type||':'||name,sql FROM main.sqlite_schema WHERE type IN ('table','index') ORDER BY type,name")
        .map_err(crate::external)?;
    q.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .map_err(crate::external)?
        .collect::<rusqlite::Result<_>>()
        .map_err(crate::external)
}

fn expected_schema() -> Option<&'static Schema> {
    static EXPECTED: OnceLock<Option<Schema>> = OnceLock::new();
    EXPECTED
        .get_or_init(|| {
            let db = Connection::open_in_memory().ok()?;
            db.execute_batch(crate::indexing::SCHEMA).ok()?;
            schema(&db).ok()
        })
        .as_ref()
}

#[derive(Debug, Eq, PartialEq, Ord, PartialOrd)]
struct ForeignKey {
    child: String,
    id: i64,
    seq: i64,
    parent: String,
    from: String,
    to: String,
    update: String,
    delete: String,
    matching: String,
}

fn supported(db: &Connection) -> Result<bool> {
    let encoding: String = db
        .query_row("PRAGMA main.encoding", [], |r| r.get(0))
        .map_err(crate::external)?;
    if encoding != "UTF-8" {
        return Ok(false);
    }
    let Some(expected) = expected_schema() else {
        return Ok(false);
    };
    let actual_schema = schema(db)?;
    if &actual_schema != expected {
        return Ok(false);
    }
    if actual_schema
        .keys()
        .filter(|key| key.starts_with("table:"))
        .count()
        != OWNED_TABLES.len()
        || OWNED_TABLES
            .iter()
            .any(|name| !actual_schema.contains_key(&format!("table:{name}")))
    {
        return Ok(false);
    }
    let owned: bool = db.query_row(
        "SELECT count(*)=2 FROM main.metadata WHERE (key='schema' AND value='2') OR (key='facts_contract' AND value IN (?1,?2))",
        [crate::facts_cache::CONTRACT, crate::facts_cache::CONTRACT_V3], |r| r.get(0),
    ).map_err(crate::external)?;
    if !owned {
        return Ok(false);
    }
    let mut actual = vec![];
    // These identifiers are compile-time owned names, never input-derived SQL.
    for child in OWNED_TABLES {
        let mut q = db
            .prepare(&format!("PRAGMA main.foreign_key_list('{child}')"))
            .map_err(crate::external)?;
        let rows = q
            .query_map([], |r| {
                Ok(ForeignKey {
                    child: child.into(),
                    id: r.get(0)?,
                    seq: r.get(1)?,
                    parent: r.get(2)?,
                    from: r.get::<_, Option<String>>(3)?.unwrap_or_default(),
                    to: r.get::<_, Option<String>>(4)?.unwrap_or_default(),
                    update: r.get(5)?,
                    delete: r.get(6)?,
                    matching: r.get(7)?,
                })
            })
            .map_err(crate::external)?;
        for row in rows {
            actual.push(row.map_err(crate::external)?);
        }
    }
    let mut required = vec![];
    for (child, parent, key) in [
        ("object_semantics", "objects", "name"),
        ("document_semantics", "documents", "file"),
        ("registrar_links", "documents", "file"),
    ] {
        required.push(ForeignKey {
            child: child.into(),
            id: 0,
            seq: 0,
            parent: parent.into(),
            from: key.into(),
            to: key.into(),
            update: "NO ACTION".into(),
            delete: "CASCADE".into(),
            matching: "NONE".into(),
        });
    }
    actual.sort();
    required.sort();
    if actual != required {
        return Ok(false);
    }
    // Keep this guard independent of SCHEMA: a future schema edit cannot silently
    // broaden the byte-comparison assumptions of this checker.
    for (table, key) in [
        ("objects", "name"),
        ("documents", "file"),
        ("object_semantics", "name"),
        ("document_semantics", "file"),
        ("registrar_links", "file"),
    ] {
        let mut info = db
            .prepare(&format!("PRAGMA main.table_xinfo('{table}')"))
            .map_err(crate::external)?;
        let mut rows = info.query([]).map_err(crate::external)?;
        let mut found = false;
        while let Some(row) = rows.next().map_err(crate::external)? {
            let name: String = row.get(1).map_err(crate::external)?;
            if name == key {
                let kind: String = row.get(2).map_err(crate::external)?;
                let not_null: i64 = row.get(3).map_err(crate::external)?;
                let pk: i64 = row.get(5).map_err(crate::external)?;
                if kind != "TEXT" || not_null != 1 || pk != 1 {
                    return Ok(false);
                }
                found = true;
            }
        }
        if !found {
            return Ok(false);
        }
        let index = format!("sqlite_autoindex_{table}_1");
        let mut info = db
            .prepare(&format!("PRAGMA main.index_xinfo('{index}')"))
            .map_err(crate::external)?;
        let mut rows = info.query([]).map_err(crate::external)?;
        let mut binary = false;
        while let Some(row) = rows.next().map_err(crate::external)? {
            let name: Option<String> = row.get(2).map_err(crate::external)?;
            if name.as_deref() == Some(key) {
                let collation: Option<String> = row.get(4).map_err(crate::external)?;
                let key_column: i64 = row.get(5).map_err(crate::external)?;
                binary = collation.as_deref() == Some("BINARY") && key_column == 1;
            }
        }
        if !binary {
            return Ok(false);
        }
    }
    Ok(true)
}

fn text(value: ValueRef<'_>) -> Option<&[u8]> {
    let ValueRef::Text(bytes) = value else {
        return None;
    };
    if bytes.len() > MAX_KEY_BYTES || std::str::from_utf8(bytes).is_err() {
        return None;
    }
    Some(bytes)
}

fn parents(db: &Connection, sql: &str) -> Result<Option<HashSet<Vec<u8>>>> {
    let mut q = db.prepare(sql).map_err(crate::external)?;
    let mut rows = q.query([]).map_err(crate::external)?;
    let mut keys = HashSet::new();
    let mut used = 0usize;
    while let Some(row) = rows.next().map_err(crate::external)? {
        let Some(bytes) = text(row.get_ref(0).map_err(crate::external)?) else {
            return Ok(None);
        };
        let Some(next) = used
            .checked_add(bytes.len())
            .and_then(|n| n.checked_add(KEY_OVERHEAD))
        else {
            return Ok(None);
        };
        if keys.len() >= MAX_KEYS || next > MAX_SET_BYTES {
            return Ok(None);
        }
        if !keys.insert(bytes.to_vec()) {
            return Ok(None);
        }
        used = next;
    }
    Ok(Some(keys))
}

fn children(db: &Connection, sql: &str, keys: &HashSet<Vec<u8>>) -> Result<Option<bool>> {
    let mut q = db.prepare(sql).map_err(crate::external)?;
    let mut rows = q.query([]).map_err(crate::external)?;
    let mut valid = true;
    while let Some(row) = rows.next().map_err(crate::external)? {
        let Some(bytes) = text(row.get_ref(0).map_err(crate::external)?) else {
            return Ok(None);
        };
        valid &= keys.contains(bytes);
    }
    Ok(Some(valid))
}

fn owned_valid(db: &Connection) -> Result<Option<bool>> {
    let mut valid = true;
    {
        let Some(keys) = parents(db, OBJECT_KEYS)? else {
            return Ok(None);
        };
        match children(db, OBJECT_CHILDREN, &keys)? {
            Some(result) => valid &= result,
            None => return Ok(None),
        }
    }
    let Some(keys) = parents(db, DOCUMENT_KEYS)? else {
        return Ok(None);
    };
    for sql in DOCUMENT_CHILDREN {
        match children(db, sql, &keys)? {
            Some(result) => valid &= result,
            None => return Ok(None),
        }
    }
    Ok(Some(valid))
}

pub(crate) fn native_valid(db: &Connection) -> Result<bool> {
    let mut q = db
        .prepare("PRAGMA foreign_key_check")
        .map_err(crate::external)?;
    Ok(!q.exists([]).map_err(crate::external)?)
}

pub(crate) fn foreign_keys_valid(db: &Connection) -> Result<bool> {
    profile::measure(
        "index.foreign_key_check",
        "index.integrity",
        0,
        None,
        || check(db),
    )
}

fn check(db: &Connection) -> Result<bool> {
    if profile::measure(
        "index.fk_schema",
        "index.foreign_key_check",
        0,
        None,
        || supported(db),
    )? && let Some(valid) =
        profile::measure("index.fk_fast", "index.foreign_key_check", 0, None, || {
            owned_valid(db)
        })?
    {
        return Ok(valid);
    }
    profile::measure(
        "index.fk_native",
        "index.foreign_key_check",
        0,
        None,
        || native_valid(db),
    )
}

#[cfg(test)]
#[path = "index_integrity_tests.rs"]
mod tests;
