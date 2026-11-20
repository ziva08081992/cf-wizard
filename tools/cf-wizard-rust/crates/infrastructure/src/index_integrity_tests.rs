use super::*;
use rusqlite::params;

fn initialize(db: &Connection, schema: &str) {
    db.execute_batch("PRAGMA foreign_keys=OFF").unwrap();
    db.execute_batch(schema).unwrap();
    db.execute("INSERT INTO metadata VALUES('schema','2')", [])
        .unwrap();
    db.execute(
        "INSERT INTO metadata VALUES('facts_contract',?1)",
        [crate::facts_cache::CONTRACT],
    )
    .unwrap();
}
fn owned() -> Connection {
    let db = Connection::open_in_memory().unwrap();
    initialize(&db, crate::indexing::SCHEMA);
    db
}
fn pair(db: &Connection, parent: &str, child: &str) {
    db.execute("INSERT INTO objects VALUES(?1,x'00',x'00')", [parent])
        .unwrap();
    db.execute(
        "INSERT INTO object_semantics VALUES(?1,'signature')",
        [child],
    )
    .unwrap();
}
fn same_as_native(db: &Connection) -> bool {
    let expected = native_valid(db).unwrap();
    assert_eq!(foreign_keys_valid(db).unwrap(), expected);
    expected
}

#[test]
fn owned_all_relations_and_each_orphan_match_sqlite() {
    for orphan in 0..4 {
        let db = owned();
        pair(
            &db,
            "Catalog.Объект",
            if orphan == 1 {
                "Catalog.Missing"
            } else {
                "Catalog.Объект"
            },
        );
        db.execute("INSERT INTO documents VALUES('Documents/A.xml',x'00')", [])
            .unwrap();
        db.execute(
            "INSERT INTO document_semantics VALUES(?1,'s')",
            [if orphan == 2 {
                "missing"
            } else {
                "Documents/A.xml"
            }],
        )
        .unwrap();
        db.execute(
            "INSERT INTO registrar_links VALUES(?1,0,'Register.A')",
            [if orphan == 3 {
                "missing"
            } else {
                "Documents/A.xml"
            }],
        )
        .unwrap();
        assert!(supported(&db).unwrap());
        assert_eq!(owned_valid(&db).unwrap(), Some(orphan == 0));
        assert_eq!(same_as_native(&db), orphan == 0);
    }
}

#[test]
fn binary_unicode_and_embedded_nul_are_not_truncated_or_folded() {
    for (parent, child, valid) in [
        ("A\0x", "A\0x", true),
        ("A\0x", "A\0y", false),
        ("Ёж", "Ёж", true),
        ("Ёж", "ёж", false),
        ("A", "a", false),
        ("\0", "\0", true),
    ] {
        let db = owned();
        pair(&db, parent, child);
        assert_eq!(owned_valid(&db).unwrap(), Some(valid));
        assert_eq!(same_as_native(&db), valid);
    }
}

#[test]
fn nul_does_not_hide_budget_tail() {
    let db = owned();
    let long = format!("A\0{}", "x".repeat(MAX_KEY_BYTES));
    pair(&db, &long, &long);
    assert!(owned_valid(&db).unwrap().is_none());
    assert!(same_as_native(&db));
}

#[test]
fn blob_and_malformed_utf8_values_use_native() {
    for cast in ["?1", "CAST(?1 AS TEXT)"] {
        let db = owned();
        let bytes = vec![0xffu8, 0, 0x80];
        db.execute(
            &format!("INSERT INTO objects VALUES({cast},x'00',x'00')"),
            params![bytes],
        )
        .unwrap();
        db.execute(
            &format!("INSERT INTO object_semantics VALUES({cast},'s')"),
            params![bytes],
        )
        .unwrap();
        assert!(owned_valid(&db).unwrap().is_none());
        assert!(same_as_native(&db));
    }
}

#[test]
fn unsupported_child_is_not_hidden_after_an_orphan() {
    let db = owned();
    pair(&db, "present", "absent");
    db.execute("INSERT INTO document_semantics VALUES(x'ff','s')", [])
        .unwrap();
    assert!(owned_valid(&db).unwrap().is_none());
    assert!(!same_as_native(&db));
}

#[test]
fn extra_missing_and_changed_foreign_keys_use_native() {
    let db = owned();
    db.execute_batch("CREATE TABLE extra(key TEXT REFERENCES objects(name)); INSERT INTO extra VALUES('missing');").unwrap();
    assert!(!supported(&db).unwrap());
    assert!(!same_as_native(&db));
    for schema in [
        crate::indexing::SCHEMA.replace(
            ",FOREIGN KEY(file) REFERENCES documents(file) ON DELETE CASCADE",
            "",
        ),
        crate::indexing::SCHEMA.replace("ON DELETE CASCADE", "ON DELETE RESTRICT"),
    ] {
        let db = Connection::open_in_memory().unwrap();
        initialize(&db, &schema);
        db.execute("INSERT INTO registrar_links VALUES('missing',0,'x')", [])
            .unwrap();
        assert!(!supported(&db).unwrap());
        same_as_native(&db);
    }
}

#[test]
fn nocase_schema_and_utf16_use_native_semantics() {
    let db = Connection::open_in_memory().unwrap();
    initialize(
        &db,
        &crate::indexing::SCHEMA.replace(
            "name TEXT PRIMARY KEY",
            "name TEXT COLLATE NOCASE PRIMARY KEY",
        ),
    );
    pair(&db, "A", "a");
    assert!(!supported(&db).unwrap());
    assert!(same_as_native(&db));
    let db = Connection::open_in_memory().unwrap();
    db.execute_batch("PRAGMA encoding='UTF-16le'").unwrap();
    initialize(&db, crate::indexing::SCHEMA);
    pair(&db, "Ёж", "Ёж");
    assert!(!supported(&db).unwrap());
    assert!(same_as_native(&db));
}

#[test]
fn legacy_header_even_with_owned_tables_uses_native() {
    let db = owned();
    db.execute("UPDATE metadata SET value='1' WHERE key='schema'", [])
        .unwrap();
    assert!(!supported(&db).unwrap());
    assert!(same_as_native(&db));
}

#[test]
fn count_budget_falls_back_without_rejecting_valid_index() {
    let mut db = owned();
    let tx = db.transaction().unwrap();
    {
        let mut insert = tx
            .prepare("INSERT INTO objects VALUES(?1,x'00',x'00')")
            .unwrap();
        for n in 0..=MAX_KEYS {
            insert.execute([format!("K{n}")]).unwrap();
        }
    }
    tx.commit().unwrap();
    assert!(owned_valid(&db).unwrap().is_none());
    assert!(same_as_native(&db));
}

#[test]
fn memory_budget_is_counted_before_parent_allocations() {
    let mut db = owned();
    let tx = db.transaction().unwrap();
    {
        let mut insert = tx
            .prepare("INSERT INTO objects VALUES(?1,x'00',x'00')")
            .unwrap();
        for n in 0..4200 {
            let key = format!("{n:08}{}", "x".repeat(4000));
            insert.execute([key]).unwrap();
        }
    }
    tx.commit().unwrap();
    assert!(owned_valid(&db).unwrap().is_none());
    assert!(same_as_native(&db));
}

#[test]
fn reads_share_transaction_but_next_open_sees_wal_changes() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("index.sqlite3");
    let writer = Connection::open(&path).unwrap();
    initialize(&writer, crate::indexing::SCHEMA);
    writer.execute_batch("PRAGMA journal_mode=WAL").unwrap();
    pair(&writer, "A", "A");
    let reader = Connection::open(&path).unwrap();
    reader.execute_batch("BEGIN").unwrap();
    assert!(same_as_native(&reader));
    writer
        .execute("DELETE FROM objects WHERE name='A'", [])
        .unwrap();
    assert!(same_as_native(&reader));
    reader.execute_batch("ROLLBACK").unwrap();
    drop(reader);
    let next = Connection::open(&path).unwrap();
    assert!(!same_as_native(&next));
}

#[test]
fn independent_open_sees_atomic_replacement_without_certificate() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("index.sqlite3");
    let first = Connection::open(&path).unwrap();
    initialize(&first, crate::indexing::SCHEMA);
    pair(&first, "A", "A");
    assert!(same_as_native(&first));
    drop(first);
    let draft = tempfile::NamedTempFile::new_in(dir.path())
        .unwrap()
        .into_temp_path();
    let next = Connection::open(&draft).unwrap();
    initialize(&next, crate::indexing::SCHEMA);
    pair(&next, "A", "missing");
    drop(next);
    draft.persist(&path).unwrap();
    let next = Connection::open(&path).unwrap();
    assert!(!same_as_native(&next));
}

#[test]
fn extra_secondary_indexes_force_native_and_key_queries_use_primary_roots() {
    let db = owned();
    pair(&db, "A", "A");
    db.execute_batch("INSERT INTO documents VALUES('D',x'00'); INSERT INTO document_semantics VALUES('D','s'); INSERT INTO registrar_links VALUES('D',0,'R');").unwrap();
    let queries = [
        ("objects", "name", OBJECT_KEYS),
        ("object_semantics", "name", OBJECT_CHILDREN),
        ("documents", "file", DOCUMENT_KEYS),
        ("document_semantics", "file", DOCUMENT_CHILDREN[0]),
        ("registrar_links", "file", DOCUMENT_CHILDREN[1]),
    ];
    for (table, key, _) in queries {
        db.execute_batch(&format!("CREATE INDEX cover_{table} ON {table}({key})"))
            .unwrap();
    }
    assert!(!supported(&db).unwrap());
    assert!(same_as_native(&db));
    for (table, _, query) in queries {
        let root: i64 = db
            .query_row(
                "SELECT rootpage FROM main.sqlite_schema WHERE type='table' AND name=?1",
                [table],
                |r| r.get(0),
            )
            .unwrap();
        let mut q = db.prepare(&format!("EXPLAIN {query}")).unwrap();
        let mut rows = q.query([]).unwrap();
        let mut opened = vec![];
        while let Some(row) = rows.next().unwrap() {
            let opcode: String = row.get(1).unwrap();
            if opcode == "OpenRead" {
                opened.push(row.get::<_, i64>(3).unwrap());
            }
        }
        assert_eq!(opened, vec![root], "{query}");
    }
    assert_eq!(owned_valid(&db).unwrap(), Some(true));
}

#[test]
#[ignore = "Explicit synthetic 722 MiB fixture generation, outside measured runs"]
fn generate_large_owned_fixture() {
    use cf_domain::source_facts::FactVersions;
    use std::path::PathBuf;
    let dir =
        PathBuf::from(std::env::var("CF_SQLITE_BENCH_DIR").expect("explicit isolated output"));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("owned.sqlite3");
    assert!(!path.exists(), "preserve previous fixture");
    let mut db = Connection::open(&path).unwrap();
    // Fixture generation only; production connection options are unchanged.
    db.execute_batch("PRAGMA page_size=4096; PRAGMA journal_mode=OFF; PRAGMA synchronous=OFF;")
        .unwrap();
    initialize(&db, crate::indexing::SCHEMA);
    let versions = FactVersions::current();
    let object_sig =
        crate::facts_cache::signature(&versions, crate::facts_cache::OBJECT_FACETS).unwrap();
    let document_sig =
        crate::facts_cache::signature(&versions, crate::facts_cache::REGISTRAR_FACETS).unwrap();
    for (key, value) in [
        ("source", "synthetic-sqlite-integrity-v1".to_owned()),
        ("engine", "cf-wizard-rust".to_owned()),
        ("reader_versions", serde_json::to_string(&versions).unwrap()),
    ] {
        db.execute("INSERT INTO metadata VALUES(?1,?2)", params![key, value])
            .unwrap();
    }
    let tx = db.transaction().unwrap();
    {
        let mut objects = tx.prepare("INSERT INTO objects VALUES(?1,?2,?3)").unwrap();
        let mut semantics = tx
            .prepare("INSERT INTO object_semantics VALUES(?1,?2)")
            .unwrap();
        // Realistic per-record overflow is essential: FK probes otherwise avoid
        // the large-record comparison path in SQLite's WITHOUT ROWID tree.
        // The opaque BLOB is only a physical-layout model, never decoded as facts.
        let blob: Vec<u8> = (0..73_728)
            .map(|n| ((n * 131 + n / 17) % 251) as u8)
            .collect();
        let hash = [0u8; 32];
        for n in 0..10_000 {
            let key = format!("Catalog.Модель{n:05}");
            objects
                .execute(params![key, hash.as_slice(), blob.as_slice()])
                .unwrap();
            semantics.execute(params![key, &object_sig]).unwrap();
        }
        let mut documents = tx.prepare("INSERT INTO documents VALUES(?1,?2)").unwrap();
        let mut semantics = tx
            .prepare("INSERT INTO document_semantics VALUES(?1,?2)")
            .unwrap();
        let mut registrars = tx
            .prepare("INSERT INTO registrar_links VALUES(?1,?2,?3)")
            .unwrap();
        for n in 0..2000 {
            let file = format!("Documents/Модель{n:04}.xml");
            documents.execute(params![file, hash.as_slice()]).unwrap();
            semantics.execute(params![file, &document_sig]).unwrap();
            for ordinal in 0..20 {
                registrars
                    .execute(params![
                        file,
                        ordinal,
                        format!("AccumulationRegister.R{ordinal:03}")
                    ])
                    .unwrap();
            }
        }
    }
    tx.commit().unwrap();
    let page_count: u64 = db.query_row("PRAGMA page_count", [], |r| r.get(0)).unwrap();
    let page_size: u64 = db.query_row("PRAGMA page_size", [], |r| r.get(0)).unwrap();
    let mut plans = BTreeMap::new();
    for (name, sql) in [
        ("native", "PRAGMA foreign_key_check"),
        ("object_keys", OBJECT_KEYS),
        ("object_children", OBJECT_CHILDREN),
        ("document_keys", DOCUMENT_KEYS),
    ] {
        let mut q = db.prepare(&format!("EXPLAIN {sql}")).unwrap();
        let mut rows = q.query([]).unwrap();
        let mut plan = vec![];
        while let Some(row) = rows.next().unwrap() {
            let mut values = vec![];
            for col in 0..8 {
                values.push(match row.get_ref(col).unwrap() {
                    ValueRef::Null => serde_json::Value::Null,
                    ValueRef::Integer(v) => serde_json::json!(v),
                    ValueRef::Real(v) => serde_json::json!(v),
                    ValueRef::Text(v) => serde_json::json!(String::from_utf8_lossy(v)),
                    ValueRef::Blob(v) => serde_json::json!(hex::encode(v)),
                });
            }
            plan.push(values);
        }
        plans.insert(name, plan);
    }
    drop(db);
    let bytes = std::fs::metadata(&path).unwrap().len();
    assert!((680.0..780.0).contains(&(bytes as f64 / 1048576.0)));
    let report = serde_json::json!({"seed":20261119,"objects":10000,"documents":2000,"registrar_links":40000,"blob_bytes_per_object":73728,"opaque_blob_physical_model_only":true,"path":"owned.sqlite3","bytes":bytes,"page_count":page_count,"page_size":page_size,"source_identity":"synthetic-sqlite-integrity-v1","expected_valid":true,"native_explain":plans});
    std::fs::write(
        dir.join("manifest.json"),
        serde_json::to_vec_pretty(&report).unwrap(),
    )
    .unwrap();
    println!("fixture bytes={bytes}");
}

#[test]
fn compact_contract_keeps_fast_foreign_key_path_and_unknown_falls_back() {
    let db = owned();
    db.execute(
        "UPDATE metadata SET value=? WHERE key='facts_contract'",
        [crate::facts_cache::CONTRACT_V3],
    )
    .unwrap();
    assert!(supported(&db).unwrap());
    assert!(same_as_native(&db));
    pair(&db, "Catalog.A", "Catalog.Missing");
    assert!(!same_as_native(&db));
    db.execute(
        "UPDATE metadata SET value='unknown' WHERE key='facts_contract'",
        [],
    )
    .unwrap();
    assert!(!supported(&db).unwrap());
    assert!(!same_as_native(&db));
}
