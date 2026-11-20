//! Process-wide configured read-cache budget. Pager sizes are targets, not RSS caps.
use cf_application::profile;
use cf_domain::Result;
use rusqlite::{Connection, OpenFlags};
use std::{
    ops::{Deref, DerefMut},
    path::Path,
    sync::atomic::{AtomicUsize, Ordering},
    time::Duration,
};
const MIB: usize = 1024 * 1024;
const INDEX_BUDGET: usize = 1016 * MIB; // separate tree-store pool reserves 8 MiB
const PAGER: usize = 2 * MIB;
static ASSIGNED: AtomicUsize = AtomicUsize::new(0);
struct Lease(usize);
impl Lease {
    fn acquire(wanted: usize) -> Self {
        let mut used = ASSIGNED.load(Ordering::Acquire);
        loop {
            let available = INDEX_BUDGET - used;
            let grant = if available >= PAGER {
                wanted.min(available)
            } else {
                0
            };
            match ASSIGNED.compare_exchange_weak(
                used,
                used + grant,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return Self(grant),
                Err(current) => used = current,
            }
        }
    }
}
impl Drop for Lease {
    fn drop(&mut self) {
        ASSIGNED.fetch_sub(self.0, Ordering::AcqRel);
    }
}
/// Declaration order closes SQLite (and mappings) before releasing the budget.
pub(crate) struct CachedConnection {
    db: Connection,
    _lease: Lease,
}
impl CachedConnection {
    pub(crate) fn readonly(path: &Path) -> Result<Self> {
        crate::source::no_links(path)?;
        let db = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_ONLY
                | OpenFlags::SQLITE_OPEN_NO_MUTEX
                | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )
        .map_err(crate::external)?;
        let result = Self::configure(db, true)?;
        result
            .execute_batch("PRAGMA trusted_schema=OFF; BEGIN DEFERRED;")
            .map_err(crate::external)?;
        Ok(result)
    }
    pub(crate) fn writable(path: &Path) -> Result<Self> {
        Self::configure(Connection::open(path).map_err(crate::external)?, false)
    }
    /// Unlike Connection::backup, this never opens an unbudgeted destination.
    pub(crate) fn backup_to(
        &self,
        path: &Path,
        cancel: &cf_application::Cancellation,
    ) -> Result<()> {
        self.backup_observed(path, cancel, |_| {})
    }
    fn backup_observed(
        &self,
        path: &Path,
        cancel: &cf_application::Cancellation,
        observe: impl FnOnce(&Self),
    ) -> Result<()> {
        use rusqlite::backup::{Backup, StepResult};
        let mut target = Self::writable(path)?;
        observe(&target);
        let backup = Backup::new(self, &mut target).map_err(crate::external)?;
        loop {
            cancel.check()?;
            match backup.step(100).map_err(crate::external)? {
                StepResult::Done => return Ok(()),
                StepResult::More => {}
                state => {
                    let code = if state == StepResult::Busy {
                        rusqlite::ffi::SQLITE_BUSY
                    } else {
                        rusqlite::ffi::SQLITE_LOCKED
                    };
                    return Err(crate::external(rusqlite::Error::SqliteFailure(
                        rusqlite::ffi::Error::new(code),
                        None,
                    )));
                }
            }
        }
    }
    fn configure(db: Connection, read_only: bool) -> Result<Self> {
        let lease = Lease::acquire(if read_only { INDEX_BUDGET } else { PAGER });
        let mmap = if read_only {
            lease.0.saturating_sub(PAGER)
        } else {
            0
        };
        let pager = if lease.0 >= PAGER { -2000i64 } else { 0 };
        let result = Self { db, _lease: lease };
        result
            .busy_timeout(Duration::from_secs(5))
            .map_err(crate::external)?;
        result
            .pragma_update(None, "cache_size", pager)
            .map_err(crate::external)?;
        result
            .pragma_update(None, "mmap_size", mmap as i64)
            .map_err(crate::external)?;
        let effective: i64 = result
            .pragma_query_value(None, "mmap_size", |r| r.get(0))
            .map_err(crate::external)?;
        profile::note(
            "index.cache.configured_bytes",
            "index.open",
            result._lease.0,
        );
        profile::note(
            "index.cache.effective_mmap_bytes",
            "index.open",
            effective.max(0) as usize,
        );
        profile::note(
            "index.cache.mapping_fallback",
            "index.open",
            usize::from(effective < mmap as i64),
        );
        Ok(result)
    }
}
impl Deref for CachedConnection {
    type Target = Connection;
    fn deref(&self) -> &Connection {
        &self.db
    }
}
impl DerefMut for CachedConnection {
    fn deref_mut(&mut self) -> &mut Connection {
        &mut self.db
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn overlapping_leases_share_one_budget() {
        // Arithmetic invariant independent of other tests concurrently holding leases.
        let leases: Vec<_> = (0..16).map(|_| Lease::acquire(INDEX_BUDGET)).collect();
        assert!(ASSIGNED.load(Ordering::Acquire) <= INDEX_BUDGET);
        assert!(leases.iter().map(|v| v.0).sum::<usize>() <= INDEX_BUDGET);
        let extra = Lease::acquire(PAGER);
        assert!(extra.0 <= PAGER);
        assert!(ASSIGNED.load(Ordering::Acquire) <= INDEX_BUDGET);
    }
    #[test]
    fn connections_release_mapping_before_next_open() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cache.sqlite3");
        {
            let db = CachedConnection::writable(&path).unwrap();
            db.execute_batch("CREATE TABLE x(v); INSERT INTO x VALUES (1)")
                .unwrap();
        }
        {
            let db = CachedConnection::readonly(&path).unwrap();
            assert_eq!(
                db.query_row("SELECT v FROM x", [], |r| r.get::<_, i64>(0))
                    .unwrap(),
                1
            );
        }
        std::fs::rename(&path, dir.path().join("moved.sqlite3")).unwrap();
    }
    #[test]
    fn backup_destination_obeys_the_shared_budget_and_cancel_preserves_source() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source.sqlite3");
        let target = dir.path().join("copy.sqlite3");
        {
            let db = CachedConnection::writable(&source).unwrap();
            db.execute_batch("CREATE TABLE x(v BLOB); INSERT INTO x VALUES(zeroblob(2000000))")
                .unwrap();
        }
        let before = std::fs::read(&source).unwrap();
        let db = CachedConnection::readonly(&source).unwrap();
        db.backup_observed(
            &target,
            &cf_application::Cancellation::default(),
            |destination| {
                assert!(ASSIGNED.load(Ordering::Acquire) <= INDEX_BUDGET);
                assert!(db._lease.0 + destination._lease.0 <= INDEX_BUDGET);
                let pager: i64 = destination
                    .pragma_query_value(None, "cache_size", |r| r.get(0))
                    .unwrap();
                assert_eq!(pager, if destination._lease.0 == 0 { 0 } else { -2000 });
                let mmap: i64 = destination
                    .pragma_query_value(None, "mmap_size", |r| r.get(0))
                    .unwrap();
                assert_eq!(mmap, 0);
            },
        )
        .unwrap();
        drop(db);
        assert_eq!(std::fs::read(&source).unwrap(), before);
        let copied = CachedConnection::readonly(&target).unwrap();
        assert_eq!(
            copied
                .query_row("SELECT length(v) FROM x", [], |r| r.get::<_, usize>(0))
                .unwrap(),
            2000000
        );
        drop(copied);
        let cancel = cf_application::Cancellation::default();
        cancel.cancel();
        let db = CachedConnection::readonly(&source).unwrap();
        assert!(matches!(
            db.backup_to(&dir.path().join("cancelled.sqlite3"), &cancel),
            Err(cf_domain::Error::Cancelled)
        ));
        assert_eq!(std::fs::read(&source).unwrap(), before);
    }
}
