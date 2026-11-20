use cf_application::{Cancellation, profile};
use cf_domain::{Error, Result};
use rusqlite::{Connection, OpenFlags, OptionalExtension, TransactionBehavior, params};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use crate::source::no_links;

const APPLICATION_ID: i64 = 0x43465452; // CFTR
const SCHEMA: i64 = 1;
const PAGER_KIB: i64 = 8 * 1024;
pub(super) const BATCH_BYTES: usize = 1024 * 1024;
pub(super) const BATCH_ROWS: usize = 1024;
static PAGER_IN_USE: AtomicBool = AtomicBool::new(false);

struct PagerPermit;

impl PagerPermit {
    fn acquire() -> Option<Self> {
        PAGER_IN_USE
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .ok()
            .map(|_| Self)
    }
}

impl Drop for PagerPermit {
    fn drop(&mut self) {
        PAGER_IN_USE.store(false, Ordering::Release);
    }
}

struct Handle {
    db: Connection,
    writable: bool,
    // Drop the database, including its pager, before returning the process permit.
    _pager: Option<PagerPermit>,
}

pub(super) struct Store {
    path: PathBuf,
    identity: String,
    connection: Mutex<Option<Handle>>,
}

fn invalid(message: &str) -> Error {
    Error::Invalid(message.into())
}

fn checked_paths(path: &Path) -> Result<()> {
    no_links(path.parent().expect("tree database has a parent"))?;
    for suffix in ["", "-journal", "-shm", "-wal"] {
        let mut entry = path.as_os_str().to_owned();
        entry.push(suffix);
        match fs::symlink_metadata(&entry) {
            Ok(meta) => {
                let linked = meta.file_type().is_symlink();
                #[cfg(windows)]
                let linked = {
                    use std::os::windows::fs::MetadataExt;
                    linked || meta.file_attributes() & 0x400 != 0
                };
                if linked || !meta.is_file() {
                    return Err(invalid(
                        "SQLite-кэш дерева Git содержит ссылку или не является файлом",
                    ));
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(crate::external(error)),
        }
    }
    Ok(())
}

impl Store {
    pub(super) fn new(path: PathBuf, identity: String) -> Self {
        Self {
            path,
            identity,
            connection: Mutex::new(None),
        }
    }

    fn open(&self, write: bool, cancel: &Cancellation) -> Result<Handle> {
        cancel.check()?;
        checked_paths(&self.path)?;
        if write {
            let parent = self.path.parent().expect("tree store has a parent");
            fs::create_dir_all(parent).map_err(crate::external)?;
            checked_paths(&self.path)?;
        }
        let pager = PagerPermit::acquire();
        let mut db = Connection::open_with_flags(
            &self.path,
            OpenFlags::SQLITE_OPEN_NO_MUTEX
                | OpenFlags::SQLITE_OPEN_NOFOLLOW
                | if write {
                    OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE
                } else {
                    OpenFlags::SQLITE_OPEN_READ_ONLY
                },
        )
        .map_err(crate::external)?;
        checked_paths(&self.path)?;
        db.busy_timeout(Duration::ZERO).map_err(crate::external)?;
        db.pragma_update(None, "trusted_schema", false)
            .map_err(crate::external)?;
        db.pragma_update(None, "mmap_size", 0)
            .map_err(crate::external)?;
        let cache_kib = if pager.is_some() { PAGER_KIB } else { 0 };
        db.pragma_update(None, "cache_size", -cache_kib)
            .map_err(crate::external)?;
        let effective_cache: i64 = db
            .pragma_query_value(None, "cache_size", |row| row.get(0))
            .map_err(crate::external)?;
        let effective_mmap: i64 = db
            .pragma_query_value(None, "mmap_size", |row| row.get(0))
            .map_err(crate::external)?;
        if effective_cache != -cache_kib || effective_mmap != 0 {
            return Err(invalid("SQLite не применил ограничения кэша дерева Git"));
        }
        profile::note(
            "cache.tree_sqlite_pager_configured_kib",
            "source",
            cache_kib as usize,
        );
        profile::note(
            "cache.tree_sqlite_pager_effective_kib",
            "source",
            (-effective_cache) as usize,
        );
        profile::note(
            "cache.tree_sqlite_mmap_bytes",
            "source",
            effective_mmap as usize,
        );

        let actual_id: i64 = db
            .pragma_query_value(None, "application_id", |row| row.get(0))
            .map_err(crate::external)?;
        if write && actual_id == 0 {
            // Initialize an empty cache, including an empty file left by a canceled
            // writer. Do not change the schema of an unrelated SQLite file.
            let txn = db
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(crate::external)?;
            let entries: u64 = txn
                .query_row("SELECT count(*) FROM sqlite_schema", [], |row| row.get(0))
                .map_err(crate::external)?;
            if entries != 0 {
                return Err(invalid("Файл относится к другой базе SQLite"));
            }
            txn.execute_batch(
                "CREATE TABLE tree_identity(slot INTEGER PRIMARY KEY CHECK(slot=1), identity TEXT NOT NULL) STRICT;
                 CREATE TABLE trees(oid TEXT PRIMARY KEY, record BLOB NOT NULL)
                   WITHOUT ROWID, STRICT;",
            )
            .map_err(crate::external)?;
            txn.pragma_update(None, "application_id", APPLICATION_ID)
                .map_err(crate::external)?;
            txn.pragma_update(None, "user_version", SCHEMA)
                .map_err(crate::external)?;
            txn.execute("INSERT INTO tree_identity VALUES (1, ?)", [&self.identity])
                .map_err(crate::external)?;
            cancel.check()?;
            txn.commit().map_err(crate::external)?;
        }
        let app: i64 = db
            .pragma_query_value(None, "application_id", |row| row.get(0))
            .map_err(crate::external)?;
        let version: i64 = db
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .map_err(crate::external)?;
        if app != APPLICATION_ID || version != SCHEMA {
            return Err(invalid("Неизвестная схема SQLite-кэша дерева Git"));
        }
        let identity: String = db
            .query_row(
                "SELECT identity FROM tree_identity WHERE slot=1",
                [],
                |row| row.get(0),
            )
            .map_err(crate::external)?;
        if identity != self.identity {
            return Err(invalid(
                "SQLite-кэш дерева Git принадлежит другому источнику",
            ));
        }
        let journal: String = db
            .pragma_query_value(None, "journal_mode", |row| row.get(0))
            .map_err(crate::external)?;
        if journal != "delete" {
            return Err(invalid(
                "Неподдерживаемый режим журнала SQLite-кэша дерева Git",
            ));
        }
        cancel.check()?;
        Ok(Handle {
            db,
            writable: write,
            _pager: pager,
        })
    }

    pub(super) fn read(&self, oid: &str, cancel: &Cancellation) -> Result<Option<Vec<u8>>> {
        cancel.check()?;
        // The mutex serializes only bounded, non-waiting SQLite operations.
        // Spurious misses on ordinary parallel reads would repeat Gitea requests.
        let mut slot = self.connection.lock().map_err(crate::external)?;
        cancel.check()?;
        if slot.is_none() {
            match self.path.symlink_metadata() {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                Err(error) => return Err(crate::external(error)),
                Ok(_) => *slot = Some(self.open(false, cancel)?),
            }
        }
        checked_paths(&self.path)?;
        let handle = slot.as_ref().expect("opened tree cache");
        let result: Option<Option<Vec<u8>>> = handle
            .db
            .prepare_cached(
                "SELECT CASE WHEN typeof(record)='blob' AND length(record) BETWEEN 32 AND ?1
                        THEN record END FROM trees WHERE oid=?2
                 AND (SELECT identity FROM tree_identity WHERE slot=1)=?3",
            )
            .and_then(|mut stmt| {
                stmt.query_row(params![BATCH_BYTES, oid, self.identity], |row| row.get(0))
                    .optional()
            })
            .map_err(crate::external)?;
        drop(slot);
        cancel.check()?;
        match result {
            Some(None) => Err(invalid("Недопустимая запись SQLite-кэша дерева Git")),
            _ => Ok(result.flatten()),
        }
    }

    pub(super) fn write_batch<'a>(
        &self,
        rows: impl IntoIterator<Item = (&'a str, &'a [u8])>,
        cancel: &Cancellation,
    ) -> Result<()> {
        let mut rows = rows.into_iter().peekable();
        if rows.peek().is_none() {
            return Ok(());
        }
        cancel.check()?;
        let mut slot = self.connection.lock().map_err(crate::external)?;
        cancel.check()?;
        checked_paths(&self.path)?;
        if !slot.as_ref().is_some_and(|handle| handle.writable) {
            // Release the old pager before acquiring a new permit on upgrade.
            *slot = None;
            *slot = Some(self.open(true, cancel)?);
        }
        let handle = slot.as_mut().expect("opened writable tree cache");
        let txn = handle
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(crate::external)?;
        let identity: String = txn
            .query_row(
                "SELECT identity FROM tree_identity WHERE slot=1",
                [],
                |row| row.get(0),
            )
            .map_err(crate::external)?;
        if identity != self.identity {
            return Err(invalid(
                "SQLite-кэш дерева Git принадлежит другому источнику",
            ));
        }
        {
            let mut upsert = txn
                .prepare_cached(
                    "INSERT INTO trees VALUES (?1,?2)
                     ON CONFLICT(oid) DO UPDATE SET record=excluded.record",
                )
                .map_err(crate::external)?;
            for (oid, record) in rows {
                cancel.check()?;
                upsert
                    .execute(params![oid, record])
                    .map_err(crate::external)?;
            }
        }
        cancel.check()?;
        txn.commit().map_err(crate::external)
    }

    #[cfg(test)]
    pub(super) fn effective_pager(&self) -> (i64, i64) {
        let slot = self.connection.lock().unwrap();
        let handle = slot.as_ref().unwrap();
        let cache: i64 = handle
            .db
            .pragma_query_value(None, "cache_size", |row| row.get(0))
            .unwrap();
        let mmap: i64 = handle
            .db
            .pragma_query_value(None, "mmap_size", |row| row.get(0))
            .unwrap();
        (-cache, mmap)
    }

    #[cfg(test)]
    pub(super) fn path(&self) -> &Path {
        &self.path
    }
}
