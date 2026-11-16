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
struct Provider {
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
    cancel_at: AtomicUsize,
}
impl Provider {
    fn new(count: usize, shared: bool, modules: bool) -> Arc<Self> {
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
            entries.sort_by_key(|e| {
                format!(
                    "{}{}",
                    e.name,
                    if e.kind == EntryKind::Tree { "/" } else { "" }
                )
            });
            let mut bytes = vec![];
            for e in &entries {
                bytes.extend_from_slice(
                    format!(
                        "{} {}\0",
                        if e.kind == EntryKind::Tree {
                            "40000"
                        } else {
                            "100644"
                        },
                        e.name
                    )
                    .as_bytes(),
                );
                bytes.extend_from_slice(&hex::decode(&e.oid).unwrap());
            }
            let mut h = Sha1::new();
            h.update(format!("tree {}\0", bytes.len()));
            h.update(&bytes);
            let oid = hex::encode(h.finalize());
            ids.insert(path, oid.clone());
            trees.insert(oid, entries);
        }
        Arc::new(Self {
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
        Ok("1".repeat(40))
    }
    fn root(&self, _: &str, c: &Cancellation) -> Result<String> {
        c.check()?;
        Ok(self.ids[""].clone())
    }
    fn tree(&self, id: &str, c: &Cancellation) -> Result<Vec<Entry>> {
        c.check()?;
        let _active = self.begin(format!("tree:{id}"));
        if self.enabled.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(3));
            let n = self.tree_number.fetch_add(1, Ordering::SeqCst) + 1;
            if self.cancel_at.load(Ordering::SeqCst) == n {
                c.cancel();
            }
            c.check()?;
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
        Ok(self.trees[id].clone())
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
