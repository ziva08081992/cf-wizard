use super::*;
use crate::engine::RustEngine;
use cf_application::{DependencyEngine, PreparedPlan};
use sha1::{Digest, Sha1};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

fn blob(bytes: &[u8]) -> String {
    let mut h = Sha1::new();
    h.update(format!("blob {}\0", bytes.len()));
    h.update(bytes);
    hex::encode(h.finalize())
}
fn metadata(kind: &str, name: &str, id: usize, children: &str) -> Vec<u8> {
    format!("<MetaDataObject xmlns=\"http://v8.1c.ru/8.3/MDClasses\"><{kind} uuid=\"00000000-0000-0000-0000-{id:012}\"><Properties><Name>{name}</Name></Properties><ChildObjects>{children}</ChildObjects></{kind}></MetaDataObject>").into_bytes()
}
struct Active<'a>(&'a AtomicUsize);
impl Drop for Active<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}
pub(super) fn fixture_tree_oid(entries: &[Entry]) -> String {
    let mut entries = entries.iter().collect::<Vec<_>>();
    entries.sort_by_key(|e| {
        format!(
            "{}{}",
            e.name,
            if e.kind == EntryKind::Tree { "/" } else { "" }
        )
    });
    let mut bytes = vec![];
    for e in entries {
        let mode = e.mode.trim_start_matches('0');
        bytes.extend_from_slice(format!("{mode} {}\0", e.name).as_bytes());
        bytes.extend_from_slice(&hex::decode(&e.oid).unwrap());
    }
    let mut h = Sha1::new();
    h.update(format!("tree {}\0", bytes.len()));
    h.update(&bytes);
    hex::encode(h.finalize())
}

struct Provider {
    revision: String,
    data: BTreeMap<String, Vec<u8>>,
    trees: BTreeMap<String, Vec<Entry>>,
    ids: BTreeMap<String, String>,
    calls: Mutex<BTreeMap<String, usize>>,
    enabled: AtomicBool,
    active: AtomicUsize,
    maximum: AtomicUsize,
    tree_number: AtomicUsize,
    fail_tree: Mutex<String>,
    fail_file: Mutex<String>,
    invalid_tree: Mutex<String>,
    partial_tree: Mutex<String>,
    wrong_size: Mutex<Option<(String, u64)>>,
    assert_unlocked_cache: Mutex<Option<PathBuf>>,
    cancel_at: AtomicUsize,
}
impl Provider {
    fn new(count: usize, shared: bool, modules: bool) -> Arc<Self> {
        Self::with_revision(count, shared, modules, "", "1".repeat(40))
    }

    fn with_revision(
        count: usize,
        shared: bool,
        modules: bool,
        removed: &str,
        revision: String,
    ) -> Arc<Self> {
        let mut data = BTreeMap::new();
        let mut children = "<CommonModule>Probe</CommonModule>".to_owned();
        let mut module = String::new();
        for i in 0..count {
            let name = format!("C{i:03}");
            children.push_str(&format!("<Catalog>{name}</Catalog>"));
            module.push_str(&format!("x = VALUE(Catalog.{name}.EmptyRef);\n"));
            data.insert(
                format!("config/Catalogs/{name}.xml"),
                metadata("Catalog", &name, i + 100, ""),
            );
            data.insert(
                format!("config/Catalogs/{name}/Templates/Unused/Ext/Template.bin"),
                vec![if shared { 1 } else { i as u8 }; 8],
            );
            if modules {
                data.insert(
                    format!("config/Catalogs/{name}/Ext/ObjectModule.bsl"),
                    b"Procedure Probe()\nEndProcedure".to_vec(),
                );
            }
        }
        data.insert(
            "config/Configuration.xml".into(),
            metadata("Configuration", "Synthetic", 1, &children),
        );
        data.insert(
            "config/CommonModules/Probe.xml".into(),
            metadata("CommonModule", "Probe", 2, ""),
        );
        data.insert(
            "config/CommonModules/Probe/Ext/Module.bsl".into(),
            module.into_bytes(),
        );
        data.remove(removed);
        let mut folders = BTreeMap::<String, BTreeSet<String>>::new();
        for path in data.keys() {
            let mut child = path.as_str();
            while let Some((parent, _)) = child.rsplit_once('/') {
                folders
                    .entry(parent.into())
                    .or_default()
                    .insert(child.into());
                child = parent;
            }
            folders
                .entry(String::new())
                .or_default()
                .insert(child.into());
        }
        let mut paths: Vec<_> = folders.keys().cloned().collect();
        paths.sort_by_key(|p| {
            std::cmp::Reverse(p.matches('/').count() + usize::from(!p.is_empty()))
        });
        let mut ids = BTreeMap::<String, String>::new();
        let mut trees = BTreeMap::new();
        for path in paths {
            let mut entries = vec![];
            for child in &folders[&path] {
                let name = child.rsplit('/').next().unwrap().to_owned();
                let (oid, size, kind, mode) = if let Some(bytes) = data.get(child) {
                    (blob(bytes), bytes.len() as u64, EntryKind::Blob, "100644")
                } else {
                    (ids[child].clone(), 0, EntryKind::Tree, "040000")
                };
                entries.push(Entry {
                    name,
                    oid,
                    size,
                    kind,
                    mode: mode.into(),
                });
            }
            let oid = fixture_tree_oid(&entries);
            ids.insert(path, oid.clone());
            trees.insert(oid, entries);
        }
        Arc::new(Self {
            revision,
            data,
            trees,
            ids,
            calls: Mutex::new(BTreeMap::new()),
            enabled: AtomicBool::new(false),
            active: AtomicUsize::new(0),
            maximum: AtomicUsize::new(0),
            tree_number: AtomicUsize::new(0),
            fail_tree: Mutex::new(String::new()),
            fail_file: Mutex::new(String::new()),
            invalid_tree: Mutex::new(String::new()),
            partial_tree: Mutex::new(String::new()),
            wrong_size: Mutex::new(None),
            assert_unlocked_cache: Mutex::new(None),
            cancel_at: AtomicUsize::new(0),
        })
    }
    fn begin(&self, key: String) -> Active<'_> {
        let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
        self.maximum.fetch_max(active, Ordering::SeqCst);
        *self.calls.lock().unwrap().entry(key).or_default() += 1;
        Active(&self.active)
    }
    fn reset(&self) {
        self.calls.lock().unwrap().clear();
        self.maximum.store(0, Ordering::SeqCst);
        self.tree_number.store(0, Ordering::SeqCst);
        self.enabled.store(true, Ordering::SeqCst);
    }
}
impl GitProvider for Provider {
    fn branches(&self, c: &Cancellation) -> Result<Vec<String>> {
        c.check()?;
        Ok(vec!["main".into()])
    }
    fn head(&self, _: &str, c: &Cancellation) -> Result<String> {
        c.check()?;
        Ok(self.revision.clone())
    }
    fn root(&self, _: &str, c: &Cancellation) -> Result<String> {
        c.check()?;
        Ok(self.revision.clone())
    }
    fn tree(&self, id: &str, c: &Cancellation) -> Result<Vec<Entry>> {
        c.check()?;
        let _active = self.begin(format!("tree:{id}"));
        if let Some(path) = self.assert_unlocked_cache.lock().unwrap().as_ref() {
            let db = rusqlite::Connection::open(path).map_err(crate::external)?;
            db.busy_timeout(std::time::Duration::ZERO)
                .map_err(crate::external)?;
            db.execute_batch("BEGIN EXCLUSIVE; ROLLBACK;")
                .map_err(crate::external)?;
        }
        if self.enabled.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(3));
            let n = self.tree_number.fetch_add(1, Ordering::SeqCst) + 1;
            if self.cancel_at.load(Ordering::SeqCst) == n {
                // Return complete data while canceling before source publication.
                c.cancel();
            } else {
                c.check()?;
            }
            if *self.fail_tree.lock().unwrap() == id {
                return Err(Error::External("later tree failure".into()));
            }
            if *self.invalid_tree.lock().unwrap() == id {
                return Ok(vec![Entry {
                    name: "bad".into(),
                    oid: "2".repeat(40),
                    size: 0,
                    kind: EntryKind::Unsupported,
                    mode: "120000".into(),
                }]);
            }
        }
        let mut entries = self.trees[if id == self.revision {
            &self.ids[""]
        } else {
            id
        }]
        .clone();
        if *self.partial_tree.lock().unwrap() == id {
            entries.pop();
        }
        if let Some((oid, size)) = &*self.wrong_size.lock().unwrap()
            && oid == id
        {
            entries
                .iter_mut()
                .find(|e| e.kind == EntryKind::Blob)
                .unwrap()
                .size = *size;
        }
        Ok(entries)
    }
    fn file(&self, _: &str, path: &str, e: &Entry, c: &Cancellation) -> Result<Vec<u8>> {
        c.check()?;
        let _active = self.begin(format!("file:{path}"));
        if self.enabled.load(Ordering::SeqCst) && *self.fail_file.lock().unwrap() == path {
            return Err(Error::External("early omitted file failure".into()));
        }
        let bytes = self.data[path].clone();
        verify_blob(&bytes, &e.oid, e.size)?;
        Ok(bytes)
    }
    fn commits(
        &self,
        _: &str,
        _: Option<&str>,
        _: &str,
        _: &Cancellation,
        _: &dyn Fn(usize),
    ) -> Result<Vec<crate::git::Commit>> {
        Ok(vec![])
    }
    fn changed(&self, _: &str, _: &str, _: &Cancellation) -> Result<Vec<String>> {
        Ok(vec![])
    }
}
fn source(provider: &Arc<Provider>, root: &Path) -> GitSource {
    GitSource::open(
        &Connection {
            server: "https://example.test".into(),
            repository: "diagnostic/inventory".into(),
            directory: "config".into(),
            branch: "main".into(),
            cache: root.to_string_lossy().into_owned(),
            ..Connection::default()
        },
        provider.clone(),
        &Cancellation::default(),
    )
    .unwrap()
}
fn prefixes() -> Vec<String> {
    vec![
        "Catalogs/C000".into(),
        "Catalogs/C001".into(),
        "Catalogs/C000".into(),
        "Catalogs/C002".into(),
    ]
}
#[test]
fn window_coalesces_shared_paths_and_oids_and_reuses_warm_results_without_workers() {
    let root = tempfile::tempdir().unwrap();
    let provider = Provider::new(3, true, false);
    let source = source(&provider, root.path());
    provider.reset();
    let paths = prefixes();
    let cold = source
        .inventory_window(&paths, &Cancellation::default())
        .unwrap()
        .unwrap();
    let cold: Vec<_> = cold.into_iter().map(Result::unwrap).collect();
    for (path, files) in paths.iter().zip(&cold) {
        assert_eq!(
            files,
            &[format!("{path}/Templates/Unused/Ext/Template.bin")]
        );
    }
    let calls = provider.calls.lock().unwrap().clone();
    assert_eq!(calls.len(), 4);
    assert!(calls.values().all(|&n| n == 1));
    let workers = source.files.inventory_workers.load(Ordering::Relaxed);
    assert!(workers > 0);
    let warm = source
        .inventory_window(&paths, &Cancellation::default())
        .unwrap()
        .unwrap();
    assert_eq!(
        warm.into_iter().map(Result::unwrap).collect::<Vec<_>>(),
        cold
    );
    assert_eq!(
        source.files.inventory_workers.load(Ordering::Relaxed),
        workers
    );
    assert_eq!(*provider.calls.lock().unwrap(), calls);
    assert_eq!(provider.active.load(Ordering::SeqCst), 0);
}
#[test]
fn shared_failure_is_typed_once_per_window_and_never_becomes_a_partial_inventory() {
    for invalid in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let provider = Provider::new(3, true, false);
        let source = source(&provider, root.path());
        let id = provider.ids["config/Catalogs/C000/Templates/Unused"].clone();
        if invalid {
            *provider.invalid_tree.lock().unwrap() = id.clone();
        } else {
            *provider.fail_tree.lock().unwrap() = id.clone();
        }
        provider.reset();
        let cancel = Cancellation::default();
        for expected in 1..=2 {
            let results = source
                .inventory_window(&prefixes(), &cancel)
                .unwrap()
                .unwrap();
            assert!(results.into_iter().all(|r| if invalid {
                matches!(r, Err(Error::Invalid(_)))
            } else {
                matches!(r, Err(Error::External(_)))
            }));
            assert_eq!(
                provider.calls.lock().unwrap()[&format!("tree:{id}")],
                expected
            );
            assert!(cancel.check().is_ok());
            assert_eq!(provider.active.load(Ordering::SeqCst), 0);
            assert!(!source.files.oid_trees.lock().unwrap().contains_key(&id));
            assert!(
                !source
                    .files
                    .trees
                    .lock()
                    .unwrap()
                    .contains_key("Catalogs/C000/Templates/Unused")
            );
        }
    }
}
#[test]
fn cancellation_joins_every_inventory_worker_and_rejects_oversized_windows() {
    let root = tempfile::tempdir().unwrap();
    let provider = Provider::new(4, false, false);
    let source = source(&provider, root.path());
    provider.reset();
    provider.cancel_at.store(2, Ordering::SeqCst);
    let paths = (0..4)
        .map(|i| format!("Catalogs/C{i:03}"))
        .collect::<Vec<_>>();
    assert!(matches!(
        source.inventory_window(&paths, &Cancellation::default()),
        Err(Error::Cancelled)
    ));
    assert_eq!(provider.active.load(Ordering::SeqCst), 0);
    assert!(provider.maximum.load(Ordering::SeqCst) <= 4);
    let calls = provider.calls.lock().unwrap().clone();
    assert!(
        source
            .inventory_window(&vec!["Catalogs/C000".into(); 5], &Cancellation::default())
            .is_err()
    );
    assert_eq!(*provider.calls.lock().unwrap(), calls);
}
struct Serial<'a>(&'a GitSource);
impl MetadataSource for Serial<'_> {
    fn manifest(&self) -> Option<&GitInputs> {
        self.0.manifest()
    }
    fn identity(&self) -> &str {
        self.0.identity()
    }
    fn catalog(&self) -> &Catalog {
        self.0.catalog()
    }
    fn is_pinned(&self) -> bool {
        true
    }
    fn read(&self, p: &str, c: &Cancellation) -> Result<Vec<u8>> {
        self.0.read(p, c)
    }
    fn files_below(&self, p: &str, c: &Cancellation) -> Result<Vec<String>> {
        self.0.files_below(p, c)
    }
    fn exists(&self, p: &str, c: &Cancellation) -> Result<bool> {
        self.0.exists(p, c)
    }
    fn list_directory(&self, p: &str, c: &Cancellation) -> Result<Vec<SourceEntry>> {
        self.0.list_directory(p, c)
    }
    fn snapshot(&self, c: &Cancellation) -> Result<String> {
        self.0.snapshot(c)
    }
}
fn preview(source: &dyn MetadataSource, c: &Cancellation) -> Result<Box<dyn PreparedPlan>> {
    RustEngine.preview(
        source,
        None,
        &BTreeSet::from(["CommonModule.Probe".into()]),
        &BTreeSet::new(),
        c,
    )
}
#[test]
fn window_keeps_early_omitted_file_error_before_later_tree_error() {
    for optimized in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let provider = Provider::new(4, false, true);
        let source = source(&provider, root.path());
        *provider.fail_file.lock().unwrap() = "config/Catalogs/C000/Ext/ObjectModule.bsl".into();
        *provider.fail_tree.lock().unwrap() =
            provider.ids["config/Catalogs/C003/Templates"].clone();
        provider.reset();
        let cancel = Cancellation::default();
        let serial = Serial(&source);
        let chosen: &dyn MetadataSource = if optimized { &source } else { &serial };
        assert!(
            matches!(preview(chosen, &cancel), Err(Error::External(message)) if message == "early omitted file failure")
        );
        assert!(cancel.check().is_ok());
        assert_eq!(provider.active.load(Ordering::SeqCst), 0);
        assert!(provider.maximum.load(Ordering::SeqCst) <= 4);
    }
}
#[test]
fn bounded_windows_preserve_plan_omitted_bytes_settings_roles_and_package() {
    let mut baseline = None;
    for optimized in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let provider = Provider::new(9, false, true);
        let source = source(&provider, &root.path().join("cache"));
        provider.reset();
        let serial = Serial(&source);
        let chosen: &dyn MetadataSource = if optimized { &source } else { &serial };
        let cancel = Cancellation::default();
        let plan = preview(chosen, &cancel).unwrap();
        assert_eq!(plan.composition().stubs.len(), 9);
        assert!(plan.composition().diagnostics.is_empty());
        assert!(
            plan.composition()
                .source_hashes
                .contains_key("Catalogs/C000/Ext/ObjectModule.bsl")
        );
        let mut composition = serde_json::to_value(plan.composition()).unwrap();
        composition.as_object_mut().unwrap().remove("metrics");
        let package = plan
            .write(
                chosen,
                &BTreeSet::new(),
                &root.path().join("stage"),
                &root.path().join("settings.xml"),
                &cancel,
            )
            .unwrap();
        assert!(
            !provider
                .calls
                .lock()
                .unwrap()
                .keys()
                .any(|p| p.ends_with("Template.bin"))
        );
        let result = serde_json::json!({
            "composition":composition, "files":package.files, "settings":package.settings_hash,
            "roles":package.readback.stub_descriptors, "full":package.readback.full_dependencies
        });
        if let Some(expected) = &baseline {
            assert_eq!(&result, expected);
        } else {
            baseline = Some(result);
        }
        assert_eq!(provider.active.load(Ordering::SeqCst), 0);
        assert!(provider.maximum.load(Ordering::SeqCst) <= 4);
        if optimized {
            assert!(provider.maximum.load(Ordering::SeqCst) > 1);
        }
    }
}

#[test]
fn profiled_warm_payload_reads_once_without_redundant_io_or_provider_calls() {
    let root = tempfile::tempdir().unwrap();
    let provider = Provider::new(2, false, true);
    let mut owned = source(&provider, root.path());
    owned.files.counters = profile::SourceCounters::new(true);
    let cancel = Cancellation::default();
    let path = "Catalogs/C000.xml";
    let expected = owned.read(path, &cancel).unwrap();
    let before = provider.calls.lock().unwrap().clone();
    let context = profile::Context::new("preview", true, owned.profile_counters());
    {
        let _scope = context.enter();
        for _ in 0..2 {
            assert_eq!(owned.read(path, &cancel).unwrap(), expected);
        }
    }
    let report = context.report("ok").unwrap();
    assert_eq!(report.metrics["source.read"].totals.calls, 2);
    assert_eq!(report.metrics["source.read"].unique, 1);
    assert_eq!(report.metrics["cache.read"].totals.calls, 2);
    assert_eq!(report.metrics["hash.git_oid"].totals.calls, 2);
    assert!(!report.metrics.contains_key("cache.write"));
    assert!(!report.metrics.contains_key("provider.file"));
    assert_eq!(*provider.calls.lock().unwrap(), before);
    assert_eq!(report.source_delta["source.read"].calls, 2);
    assert!(!report.shared_source_activity);
}
#[test]
fn profiled_readiness_is_distinct_and_does_not_fetch_or_reindex() {
    let root = tempfile::tempdir().unwrap();
    let provider = Provider::new(2, false, true);
    let owned = source(&provider, &root.path().join("cache"));
    let cancel = Cancellation::default();
    let dir = root.path().join("index");
    crate::indexing::update(&owned, &dir, &cancel, &|_, _| {}).unwrap();
    let calls = provider.calls.lock().unwrap().clone();
    let context = profile::Context::new("readiness", true, owned.profile_counters());
    {
        let _scope = context.enter();
        assert!(crate::indexing::status(&owned, &dir).unwrap().ready);
    }
    let report = context.report("ok").unwrap();
    for name in [
        "index.open",
        "index.quick_check",
        "index.foreign_key_check",
        "index.semantic_objects",
        "index.semantic_documents",
        "index.fingerprint.readback",
        "index.fingerprint.stored_reuse",
        "index.fingerprint.current",
    ] {
        assert_eq!(report.metrics[name].totals.calls, 1, "{name}");
        assert_eq!(report.metrics[name].totals.errors, 0, "{name}");
    }
    assert!(!report.metrics.contains_key("index.fingerprint.stored"));
    assert!(report.identities.contains_key("index_fingerprint"));
    assert_eq!(*provider.calls.lock().unwrap(), calls);
}
#[test]
fn profiled_inventory_failures_preserve_order_and_join() {
    let context = profile::Context::new("preview", true, None);
    {
        let _scope = context.enter();
        window_keeps_early_omitted_file_error_before_later_tree_error();
        cancellation_joins_every_inventory_worker_and_rejects_oversized_windows();
    }
    let report = context.report("error").unwrap();
    assert_eq!(report.final_in_flight, 0);
    assert!(report.maximum_combined_in_flight <= 4);
    assert!(report.metrics["provider.tree"].totals.errors > 0);
}

#[test]
fn fully_warm_inventory_window_updates_source_activity_without_provider_calls() {
    let root = tempfile::tempdir().unwrap();
    let provider = Provider::new(3, false, true);
    let mut owned = legacy_source(&provider, root.path());
    owned.files.counters = profile::SourceCounters::new(true);
    let paths = prefixes();
    let cancel = Cancellation::default();
    owned.inventory_window(&paths, &cancel).unwrap().unwrap();
    let calls = provider.calls.lock().unwrap().clone();
    let context = profile::Context::new("preview", true, owned.profile_counters());
    {
        let _scope = context.enter();
        let result = owned.inventory_window(&paths, &cancel).unwrap().unwrap();
        assert_eq!(result.len(), paths.len());
        assert!(result.into_iter().all(|v| v.is_ok()));
    }
    let report = context.report("ok").unwrap();
    assert_eq!(report.source_delta["source.inventory_window"].calls, 1);
    assert_eq!(report.metrics["source.inventory_window"].totals.calls, 1);
    assert_eq!(report.source_active_after, 0);
    assert!(!report.shared_source_activity);
    assert_eq!(report.maximum_combined_in_flight, 0);
    assert_eq!(*provider.calls.lock().unwrap(), calls);
}

fn legacy_source(provider: &Arc<Provider>, root: &Path) -> GitSource {
    let mut opened = source(provider, root);
    opened
        .files
        .tree_cache
        .as_mut()
        .unwrap()
        .legacy_only_for_test();
    opened
}

fn cache_path(source: &GitSource, id: &str) -> PathBuf {
    source.files.tree_cache.as_ref().unwrap().path(id).unwrap()
}

fn rewrite_cached(path: &Path, change: impl FnOnce(&mut Vec<serde_json::Value>)) {
    let bytes = fs::read(path).unwrap();
    let mut rows: Vec<serde_json::Value> = serde_json::from_slice(&bytes[32..]).unwrap();
    change(&mut rows);
    let body = serde_json::to_vec(&rows).unwrap();
    let mut bytes = sha2::Sha256::digest(&body).to_vec();
    bytes.extend(body);
    fs::write(path, bytes).unwrap();
}

fn package_snapshot(source: &GitSource, stage: &Path, c: &Cancellation) -> serde_json::Value {
    let recipe = crate::engine::prepare(
        source,
        None,
        &BTreeSet::from(["CommonModule.Probe".into()]),
        &BTreeSet::new(),
        c,
    )
    .unwrap();
    let mut composition = serde_json::to_value(&recipe.composition).unwrap();
    composition.as_object_mut().unwrap().remove("metrics");
    let package = recipe
        .write(
            source,
            &BTreeSet::new(),
            &stage.join("files"),
            &stage.join("settings.xml"),
            c,
        )
        .unwrap();
    serde_json::json!({
        "composition": composition,
        "compiled": recipe.compiled.as_deref().unwrap(),
        "files": package.files,
        "settings": package.settings_hash,
        "stubs": package.readback.stub_descriptors,
        "full": package.readback.full_dependencies,
    })
}

#[test]
fn disk_reopen_runs_inventory_without_fetching_and_preserves_proofs_and_package() {
    let root = tempfile::tempdir().unwrap();
    let cache = root.path().join("cache");
    let c = Cancellation::default();
    let provider = Provider::new(9, false, true);
    let cold = source(&provider, &cache);
    provider.reset();
    let expected = package_snapshot(&cold, &root.path().join("cold"), &c);
    let cold_calls = provider.calls.lock().unwrap().len();
    assert!(cold_calls > 0);
    assert!(provider.maximum.load(Ordering::SeqCst) > 1);
    assert!(provider.maximum.load(Ordering::SeqCst) <= 4);
    assert_eq!(provider.active.load(Ordering::SeqCst), 0);
    drop(cold);
    drop(provider);

    let restarted = Provider::new(9, false, true);
    let warm = source(&restarted, &cache);
    assert!(
        restarted
            .calls
            .lock()
            .unwrap()
            .contains_key(&format!("tree:{}", restarted.revision))
    );
    restarted.reset();
    let context = profile::Context::new("preview", true, None);
    let actual = {
        let _scope = context.enter();
        package_snapshot(&warm, &root.path().join("warm"), &c)
    };
    assert_eq!(actual, expected);
    assert!(restarted.calls.lock().unwrap().is_empty());
    assert_eq!(restarted.active.load(Ordering::SeqCst), 0);
    let report = context.report("success").unwrap();
    assert!(report.metrics["cache.tree_disk_hit"].totals.items > 0);
    assert!(!report.metrics.contains_key("provider.tree"));
}

#[test]
fn a_new_revision_reuses_an_unchanged_sibling_and_proves_deletion() {
    let root = tempfile::tempdir().unwrap();
    let c = Cancellation::default();
    let old = Provider::new(2, false, true);
    let paths = vec!["Catalogs/C000".to_owned(), "Catalogs/C001".to_owned()];
    let path = "Catalogs/C000/Templates/Unused/Ext/Template.bin";
    let cold = source(&old, root.path());
    assert!(cold.exists(path, &c).unwrap());
    let initial = cold.inventory_window(&paths, &c).unwrap().unwrap();
    let initial: Vec<_> = initial.into_iter().map(Result::unwrap).collect();
    let sibling = old.ids["config/Catalogs/C001"].clone();
    drop(cold);

    let new = Provider::with_revision(2, false, true, &format!("config/{path}"), "2".repeat(40));
    assert_eq!(new.ids["config/Catalogs/C001"], sibling);
    let warm = source(&new, root.path());
    new.reset();
    let results = warm.inventory_window(&paths, &c).unwrap().unwrap();
    let results: Vec<_> = results.into_iter().map(Result::unwrap).collect();
    assert_eq!(results[0], ["Catalogs/C000/Ext/ObjectModule.bsl"]);
    assert_eq!(results[1], initial[1]);
    assert!(!warm.exists(path, &c).unwrap());
    let calls = new.calls.lock().unwrap();
    assert!(calls.contains_key(&format!("tree:{}", new.ids["config/Catalogs/C000"])));
    assert!(!calls.contains_key(&format!("tree:{sibling}")));
}

#[test]
fn malformed_disk_tree_refetches_and_a_partial_reply_never_publishes() {
    let root = tempfile::tempdir().unwrap();
    let c = Cancellation::default();
    let provider = Provider::new(1, false, false);
    let cold = legacy_source(&provider, root.path());
    let path = "Catalogs/C000/Templates/Unused/Ext";
    let id = &provider.ids[&format!("config/{path}")];
    let expected = cold.files_below(path, &c).unwrap();
    let record = cache_path(&cold, id);
    rewrite_cached(&record, |rows| rows.clear()); // Valid envelope, wrong Git OID.
    drop(cold);

    let fresh = legacy_source(&provider, root.path());
    provider.reset();
    *provider.partial_tree.lock().unwrap() = id.clone();
    let corrupt = fs::read(&record).unwrap();
    assert!(fresh.files_below(path, &c).is_err());
    assert_eq!(fs::read(&record).unwrap(), corrupt);
    assert!(!fresh.files.oid_trees.lock().unwrap().contains_key(id));
    assert!(!fresh.files.trees.lock().unwrap().contains_key(path));
    *provider.partial_tree.lock().unwrap() = String::new();
    assert_eq!(fresh.files_below(path, &c).unwrap(), expected);
    assert_ne!(fs::read(&record).unwrap(), corrupt);
    assert_eq!(provider.calls.lock().unwrap()[&format!("tree:{id}")], 2);
}

#[test]
fn wrong_disk_sizes_refetch_once_before_network_and_reuse_valid_local_payload() {
    let path = "Catalogs/C000/Ext/ObjectModule.bsl";
    let folder = "config/Catalogs/C000/Ext";
    let remote = format!("config/{path}");
    for wrong in [1, 1024, u64::MAX] {
        for local_payload in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let provider = Provider::new(2, true, true);
            let c = Cancellation::default();
            let cold = legacy_source(&provider, root.path());
            cold.files_below("Catalogs/C000", &c).unwrap();
            if local_payload {
                cold.read(path, &c).unwrap();
            }
            let record = cache_path(&cold, &provider.ids[folder]);
            rewrite_cached(&record, |rows| rows[0]["size"] = wrong.into());
            drop(cold);
            let warm = legacy_source(&provider, root.path());
            provider.reset();

            assert!(warm.exists(path, &c).unwrap());
            assert!(provider.calls.lock().unwrap().is_empty());
            assert_eq!(warm.read(path, &c).unwrap(), provider.data[&remote]);
            let other = "Catalogs/C001/Ext/ObjectModule.bsl";
            assert_eq!(warm.read(other, &c).unwrap(), provider.data[&remote]);
            let calls = provider.calls.lock().unwrap();
            assert_eq!(calls[&format!("tree:{}", provider.ids[folder])], 1);
            assert_eq!(
                calls.get(&format!("file:{remote}")).copied(),
                (!local_payload).then_some(1)
            );
            assert!(!calls.contains_key(&format!("file:config/{other}")));
            assert_eq!(provider.active.load(Ordering::SeqCst), 0);
        }
    }
}

#[test]
fn unopened_descriptor_size_cannot_change_a_manifest_fingerprint() {
    let root = tempfile::tempdir().unwrap();
    let provider = Provider::new(2, false, true);
    let c = Cancellation::default();
    let cold = legacy_source(&provider, root.path());
    let expected = cold.manifest().unwrap().fingerprint().unwrap();
    let tree_cache = cold.files.tree_cache.as_ref().unwrap();
    let id = &provider.ids["config/Catalogs"];
    let record = cache_path(&cold, id);
    let entries = tree_cache
        .validate(id, provider.trees[id].clone(), &c)
        .unwrap();
    tree_cache.write(id, &entries, &c).unwrap();
    rewrite_cached(&record, |rows| {
        rows.iter_mut()
            .find(|row| row["name"] == "C001.xml")
            .unwrap()["size"] = u64::MAX.into();
    });
    drop(cold);
    provider.reset();
    let warm = legacy_source(&provider, root.path());
    assert_eq!(warm.manifest().unwrap().fingerprint().unwrap(), expected);
    let calls = provider.calls.lock().unwrap();
    assert_eq!(calls[&format!("tree:{id}")], 1);
    assert!(!calls.contains_key("file:config/Catalogs/C001.xml"));
}

#[test]
fn network_error_after_a_size_refresh_is_not_retried() {
    let root = tempfile::tempdir().unwrap();
    let provider = Provider::new(1, false, true);
    let c = Cancellation::default();
    let path = "Catalogs/C000/Ext/ObjectModule.bsl";
    let cold = legacy_source(&provider, root.path());
    cold.files_below("Catalogs/C000", &c).unwrap();
    let id = &provider.ids["config/Catalogs/C000/Ext"];
    let record = cache_path(&cold, id);
    rewrite_cached(&record, |rows| rows[0]["size"] = u64::MAX.into());
    drop(cold);

    let warm = legacy_source(&provider, root.path());
    provider.reset();
    *provider.fail_file.lock().unwrap() = format!("config/{path}");
    assert!(
        matches!(warm.read(path, &c), Err(Error::External(message)) if message == "early omitted file failure")
    );
    let calls = provider.calls.lock().unwrap();
    assert_eq!(calls[&format!("tree:{id}")], 1);
    assert_eq!(calls[&format!("file:config/{path}")], 1);
}

#[test]
fn cancelled_tree_return_does_not_publish_or_start_a_blob_request() {
    let root = tempfile::tempdir().unwrap();
    let provider = Provider::new(1, false, true);
    let c = Cancellation::default();
    let parent = "Catalogs/C000/Ext";
    let path = format!("{parent}/ObjectModule.bsl");
    let cold = legacy_source(&provider, root.path());
    cold.list_directory("Catalogs/C000", &c).unwrap();
    let id = &provider.ids["config/Catalogs/C000/Ext"];
    let record = cache_path(&cold, id);
    provider.reset();
    provider.cancel_at.store(1, Ordering::SeqCst);
    assert!(matches!(
        cold.list_directory(parent, &c),
        Err(Error::Cancelled)
    ));
    assert!(!record.exists());
    assert!(!cold.files.oid_trees.lock().unwrap().contains_key(id));
    assert_eq!(provider.active.load(Ordering::SeqCst), 0);
    provider.cancel_at.store(usize::MAX, Ordering::SeqCst);
    cold.list_directory(parent, &Cancellation::default())
        .unwrap();
    assert!(record.exists());
    drop(cold);

    let warm = legacy_source(&provider, root.path());
    assert!(warm.exists(&path, &Cancellation::default()).unwrap());
    let old_record = fs::read(&record).unwrap();
    provider.reset();
    provider.cancel_at.store(1, Ordering::SeqCst);
    assert!(matches!(
        warm.read(&path, &Cancellation::default()),
        Err(Error::Cancelled)
    ));
    assert_eq!(
        provider.calls.lock().unwrap().keys().collect::<Vec<_>>(),
        [&format!("tree:{id}")]
    );
    assert_eq!(fs::read(&record).unwrap(), old_record);
    assert_eq!(provider.active.load(Ordering::SeqCst), 0);
}

#[test]
fn github_uses_only_the_existing_memory_cache() {
    let root = tempfile::tempdir().unwrap();
    let provider = Provider::new(1, false, true);
    let c = Cancellation::default();
    let connection = Connection {
        provider: "github".into(),
        server: "https://api.github.com".into(),
        repository: "diagnostic/inventory".into(),
        directory: "config".into(),
        branch: "main".into(),
        cache: root.path().to_string_lossy().into(),
        ..Connection::default()
    };
    let open = || GitSource::open(&connection, provider.clone(), &c).unwrap();
    let paths = ["Catalogs/C000".into()];
    let first = open();
    first.inventory_window(&paths, &c).unwrap().unwrap()[0]
        .as_ref()
        .unwrap();
    assert!(!root.path().join("tree-cache-v1").exists());
    provider.reset();
    first.inventory_window(&paths, &c).unwrap().unwrap()[0]
        .as_ref()
        .unwrap();
    assert!(provider.calls.lock().unwrap().is_empty());
    let second = open();
    provider.reset();
    second.inventory_window(&paths, &c).unwrap().unwrap()[0]
        .as_ref()
        .unwrap();
    assert!(!provider.calls.lock().unwrap().is_empty());
}

#[test]
fn inconsistent_provider_size_is_checked_and_never_retried() {
    let c = Cancellation::default();
    for (size, expected_file_calls) in [(1, 1), (u64::MAX, 0)] {
        let root = tempfile::tempdir().unwrap();
        let provider = Provider::new(1, false, true);
        let id = &provider.ids["config/Catalogs/C000/Ext"];
        let path = "Catalogs/C000/Ext/ObjectModule.bsl";
        let cold = source(&provider, root.path());
        cold.files_below("Catalogs/C000", &c).unwrap();
        drop(cold);
        let warm = source(&provider, root.path());
        provider.reset();
        *provider.wrong_size.lock().unwrap() = Some((id.clone(), size));
        assert!(matches!(warm.read(path, &c), Err(Error::Invalid(_))));
        let calls = provider.calls.lock().unwrap();
        assert_eq!(calls[&format!("tree:{id}")], 1);
        assert_eq!(
            calls
                .get(&format!("file:config/{path}"))
                .copied()
                .unwrap_or(0),
            expected_file_calls
        );
    }
}

#[test]
fn unavailable_disk_cache_preserves_the_verified_provider_result() {
    let root = tempfile::tempdir().unwrap();
    let provider = Provider::new(1, false, true);
    let c = Cancellation::default();
    let owned = legacy_source(&provider, root.path());
    let path = "Catalogs/C000/Ext";
    let id = &provider.ids[&format!("config/{path}")];
    let record = cache_path(&owned, id);
    fs::create_dir_all(&record).unwrap();
    let context = profile::Context::new("preview", true, None);
    let files = {
        let _scope = context.enter();
        provider.reset();
        owned.files_below(path, &c).unwrap()
    };
    assert_eq!(files, [format!("{path}/ObjectModule.bsl")]);
    let report = context.report("ok").unwrap();
    assert_eq!(report.metrics["cache.tree_disk_rejected"].totals.items, 1);
    assert_eq!(
        report.metrics["cache.tree_disk_write_error"].totals.items,
        1
    );
    assert_eq!(provider.calls.lock().unwrap()[&format!("tree:{id}")], 1);
    assert!(record.is_dir());
}

#[test]
fn provider_miss_never_holds_a_tree_sqlite_transaction() {
    let root = tempfile::tempdir().unwrap();
    let provider = Provider::new(2, false, true);
    let cancel = Cancellation::default();
    let owned = source(&provider, root.path());
    assert!(
        !owned
            .files_below("Catalogs/C000", &cancel)
            .unwrap()
            .is_empty()
    );
    let db = owned
        .files
        .tree_cache
        .as_ref()
        .unwrap()
        .sqlite_path_for_test();
    assert!(db.is_file());
    *provider.assert_unlocked_cache.lock().unwrap() = Some(db.to_owned());
    provider.reset();
    assert!(
        !owned
            .files_below("Catalogs/C001", &cancel)
            .unwrap()
            .is_empty()
    );
    assert!(
        provider
            .calls
            .lock()
            .unwrap()
            .keys()
            .any(|key| key.starts_with("tree:"))
    );
}
