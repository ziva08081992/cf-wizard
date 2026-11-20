use super::*;
use crate::{
    canonical::GitInputs,
    index::{IndexReader, Proof},
    source::LocalSource,
};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use cf_application::{DependencyEngine, MetadataSource, SourceEntry};
use sha1::Sha1;
use std::{fs, path::Path, sync::Mutex};

pub(crate) struct Source {
    pub(crate) local: LocalSource,
    pinned: bool,
    expected: BTreeMap<String, Vec<u8>>,
    pub(crate) reads: Mutex<BTreeMap<String, usize>>,
    manifest: Option<GitInputs>,
}
impl MetadataSource for Source {
    fn manifest(&self) -> Option<&GitInputs> {
        self.manifest.as_ref()
    }
    fn materialized_root(&self) -> Option<&Path> {
        Some(&self.local.root)
    }
    fn identity(&self) -> &str {
        self.local.identity()
    }
    fn catalog(&self) -> &cf_domain::Catalog {
        self.local.catalog()
    }
    fn read(&self, path: &str, cancel: &Cancellation) -> Result<Vec<u8>> {
        *self
            .reads
            .lock()
            .expect("read counter")
            .entry(path.into())
            .or_default() += 1;
        let bytes = self.local.read(path, cancel)?;
        if self.pinned {
            let expected = self
                .expected
                .get(path)
                .ok_or_else(|| Error::Invalid(format!("Blob is outside pinned tree: {path}")))?;
            fn blob(bytes: &[u8]) -> Vec<u8> {
                let mut hash = Sha1::new();
                hash.update(format!("blob {}\0", bytes.len()).as_bytes());
                hash.update(bytes);
                hash.finalize().to_vec()
            }
            if blob(&bytes) != blob(expected) {
                return Err(Error::Invalid(format!("Corrupt pinned blob: {path}")));
            }
        }
        Ok(bytes)
    }
    fn exists(&self, path: &str, cancel: &Cancellation) -> Result<bool> {
        if self.pinned {
            Ok(self.expected.contains_key(path))
        } else {
            self.local.exists(path, cancel)
        }
    }
    fn list_directory(&self, path: &str, cancel: &Cancellation) -> Result<Vec<SourceEntry>> {
        let rows = self.local.list_directory(path, cancel)?;
        if !self.pinned {
            return Ok(rows);
        }
        Ok(rows
            .into_iter()
            .filter(|entry| {
                let rel = if path.is_empty() {
                    entry.name.clone()
                } else {
                    format!("{}/{}", path.trim_end_matches('/'), entry.name)
                };
                if entry.directory {
                    self.expected
                        .keys()
                        .any(|k| k.starts_with(&format!("{rel}/")))
                } else {
                    self.expected.contains_key(&rel)
                }
            })
            .collect())
    }
    fn files_below(&self, path: &str, cancel: &Cancellation) -> Result<Vec<String>> {
        if self.pinned {
            let prefix = format!("{}/", path.trim_end_matches('/'));
            Ok(self
                .expected
                .keys()
                .filter(|p| p.starts_with(&prefix))
                .cloned()
                .collect())
        } else {
            self.local.files_below(path, cancel)
        }
    }
    fn snapshot(&self, cancel: &Cancellation) -> Result<String> {
        if self.pinned {
            cancel.check()?;
            let manifest = self.manifest.as_ref().expect("pinned manifest");
            Ok(format!("{}@{}", manifest.source_key, manifest.revision))
        } else {
            self.local.snapshot(cancel)
        }
    }
    fn is_pinned(&self) -> bool {
        self.pinned
    }
}
pub(crate) fn rows() -> serde_json::Value {
    serde_json::from_slice(
        &fs::read(Path::new(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/fixtures/engine-0.7.3.json"
        )))
        .unwrap(),
    )
    .unwrap()
}
pub(crate) fn source(row: &serde_json::Value, root: &Path, pinned: bool) -> Source {
    let mut files = BTreeMap::new();
    for (path, data) in row["source"].as_object().unwrap() {
        let bytes = STANDARD.decode(data.as_str().unwrap()).unwrap();
        let file = root.join(path);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(&file, &bytes).unwrap();
        files.insert(path.clone(), bytes);
    }
    Source {
        local: LocalSource::open(root).unwrap(),
        pinned,
        expected: files,
        reads: Mutex::new(BTreeMap::new()),
        manifest: if pinned {
            Some(serde_json::from_value(row["manifest"].clone()).unwrap())
        } else {
            None
        },
    }
}
fn assert_composition(actual: &Plan, row: &serde_json::Value, context: &str) {
    // Keep the pinned Python oracle immutable. This separate policy delta was
    // independently specified by M10/T05 before the adapter implementation.
    let deltas: serde_json::Value = serde_json::from_slice(include_bytes!(
        "../../../tests/fixtures/datafrog-legacy-deltas.json"
    ))
    .unwrap();
    let mut expected_stubs = row["stubs"].clone();
    let mut expected_members = row["members"].clone();
    if let Some(overrides) = deltas[row["name"].as_str().unwrap()]["additional_members"].as_object()
    {
        for (key, values) in overrides {
            let items = expected_members[key].as_array_mut().unwrap();
            items.extend(values.as_array().unwrap().iter().cloned());
            items.sort_by_key(|v| v.as_str().unwrap().to_owned());
        }
    }
    if let Some(overrides) = deltas[row["name"].as_str().unwrap()]["additional_causes"].as_object()
    {
        for (key, causes) in overrides {
            expected_stubs[key]["causes"]
                .as_array_mut()
                .unwrap()
                .extend(causes.as_array().unwrap().iter().cloned());
        }
    }
    assert_eq!(
        serde_json::to_value(&actual.selected).unwrap(),
        row["selected"],
        "{context}: selected"
    );
    assert_eq!(
        serde_json::to_value(&actual.stubs).unwrap(),
        expected_stubs,
        "{context}: stubs/causes"
    );
    assert_eq!(
        serde_json::to_value(&actual.members).unwrap(),
        expected_members,
        "{context}: members"
    );
    let warnings: Vec<serde_json::Value> = actual
        .warnings
        .iter()
        .map(|w| serde_json::from_str(w).unwrap())
        .collect();
    assert_eq!(
        serde_json::to_value(warnings).unwrap(),
        row["warnings"],
        "{context}: warnings"
    );
}
fn inventory(root: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut result = BTreeMap::new();
    let mut todo = vec![root.to_path_buf()];
    while let Some(dir) = todo.pop() {
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                todo.push(path);
            } else {
                result.insert(
                    path.strip_prefix(root)
                        .unwrap()
                        .to_string_lossy()
                        .replace('\\', "/"),
                    fs::read(path).unwrap(),
                );
            }
        }
    }
    result
}
#[test]
fn complete_packages_match_python_with_and_without_the_saved_index() {
    for row in rows()["cases"].as_array().unwrap() {
        for pinned in [false, true] {
            let label = format!("{} / pinned={pinned}", row["name"].as_str().unwrap());
            let temp = tempfile::tempdir().unwrap();
            let src = source(row, &temp.path().join("source"), pinned);
            let selected = serde_json::from_value(row["selected"].clone()).unwrap();
            let excluded: BTreeSet<String> =
                serde_json::from_value(row["excluded"].clone()).unwrap();
            let inputs: GitInputs = serde_json::from_value(row["manifest"].clone()).unwrap();
            let index = if pinned {
                let path = temp.path().join("legacy.sqlite3");
                fs::write(
                    &path,
                    STANDARD
                        .decode(row["legacy_index"].as_str().unwrap())
                        .unwrap(),
                )
                .unwrap();
                Some(IndexReader::open(&path, Proof::Git(&inputs)).unwrap())
            } else {
                None
            };
            let before = inventory(&src.local.root);
            let plan = RustEngine
                .preview(
                    &src,
                    index.as_ref().map(|v| v as &dyn FactStore),
                    &selected,
                    &BTreeSet::new(),
                    &Cancellation::default(),
                )
                .unwrap_or_else(|e| panic!("{label}: preview: {e}"));
            assert_eq!(
                inventory(&src.local.root),
                before,
                "{label}: preview wrote to source"
            );
            if excluded.is_empty() {
                assert_composition(plan.composition(), row, &label);
            }
            let stage = temp.path().join("stage");
            let settings = temp.path().join("merge-settings.xml");
            let staged = plan
                .write(&src, &excluded, &stage, &settings, &Cancellation::default())
                .unwrap_or_else(|e| panic!("{label}: write: {e}"));
            assert_composition(&staged.composition, row, &label);
            staged
                .readback
                .validate(&staged.composition, src.catalog(), &staged.files)
                .unwrap_or_else(|e| panic!("{label}: readback roles: {e}"));
            for (path, key) in &staged.readback.stub_descriptors {
                assert!(!staged.composition.selected.contains(&key.0), "{label}");
                let bytes = fs::read(stage.join(path)).unwrap();
                crate::readback::verify_xml(&bytes, &bytes, Some(key))
                    .unwrap_or_else(|e| panic!("{label}: stub projection {path}: {e}"));
            }
            let actual = inventory(&stage);
            let mut expected: BTreeMap<String, Vec<u8>> = row["stage"]
                .as_object()
                .unwrap()
                .iter()
                .map(|(path, data)| {
                    (
                        path.clone(),
                        STANDARD.decode(data.as_str().unwrap()).unwrap(),
                    )
                })
                .collect();
            let deltas: serde_json::Value = serde_json::from_slice(include_bytes!(
                "../../../tests/fixtures/datafrog-legacy-deltas.json"
            ))
            .unwrap();
            if let Some(paths) =
                deltas[row["name"].as_str().unwrap()]["stage_from_source"].as_array()
            {
                for path in paths {
                    let path = path.as_str().unwrap();
                    expected.insert(
                        path.into(),
                        STANDARD
                            .decode(row["source"][path].as_str().unwrap())
                            .unwrap(),
                    );
                }
            }
            assert_eq!(
                actual.keys().collect::<Vec<_>>(),
                expected.keys().collect::<Vec<_>>(),
                "{label}: inventory"
            );
            for (path, data) in actual {
                let snippet = |bytes: &[u8]| {
                    let at = data
                        .iter()
                        .zip(&expected[&path])
                        .position(|(left, right)| left != right)
                        .unwrap_or(data.len().min(expected[&path].len()));
                    String::from_utf8_lossy(
                        &bytes[at.saturating_sub(50)..(at + 150).min(bytes.len())],
                    )
                    .into_owned()
                };
                assert!(
                    data == expected[&path],
                    "{label}: changed bytes in {path}; actual={} expected={}; first difference actual={:?} expected={:?}",
                    sha256(&data),
                    sha256(&expected[&path]),
                    snippet(&data),
                    snippet(&expected[&path])
                );
            }
            assert_eq!(
                fs::read(&settings).unwrap(),
                STANDARD.decode(row["settings"].as_str().unwrap()).unwrap(),
                "{label}: merge-settings"
            );
            if pinned && row["name"] == "register-resource-and-first-registrar" {
                assert!(
                    src.reads
                        .lock()
                        .unwrap()
                        .contains_key("Documents/Ясный.xml"),
                    "V1 has no namespace-aware registrar proof: do not relabel it as the new reader"
                );
            }
        }
    }
}
#[test]
fn changed_source_is_rejected_before_creating_staging() {
    let all = rows();
    let row = &all["cases"][0];
    let temp = tempfile::tempdir().unwrap();
    let src = source(row, &temp.path().join("source"), false);
    let selected = serde_json::from_value(row["selected"].clone()).unwrap();
    let plan = RustEngine
        .preview(
            &src,
            None,
            &selected,
            &BTreeSet::new(),
            &Cancellation::default(),
        )
        .unwrap();
    fs::write(
        src.local.root.join("CommonModules/А/Ext/Module.bsl"),
        b"changed after preview",
    )
    .unwrap();
    let stage = temp.path().join("stage");
    assert!(
        plan.write(
            &src,
            &BTreeSet::new(),
            &stage,
            &temp.path().join("settings.xml"),
            &Cancellation::default()
        )
        .is_err()
    );
    assert!(!stage.exists());
}
#[test]
fn changed_binary_resource_is_rejected_before_staging() {
    let all = rows();
    let row = &all["cases"][0];
    let temp = tempfile::tempdir().unwrap();
    let src = source(row, &temp.path().join("source"), false);
    let asset = src.local.root.join("CommonModules/А/Ext/resource.bin");
    fs::write(&asset, [0, 255, 128, 1]).unwrap();
    let selected = serde_json::from_value(row["selected"].clone()).unwrap();
    let plan = RustEngine
        .preview(
            &src,
            None,
            &selected,
            &BTreeSet::new(),
            &Cancellation::default(),
        )
        .unwrap();
    fs::write(&asset, [0, 255, 128, 2]).unwrap();
    let stage = temp.path().join("stage");
    assert!(
        plan.write(
            &src,
            &BTreeSet::new(),
            &stage,
            &temp.path().join("settings.xml"),
            &Cancellation::default()
        )
        .is_err()
    );
    assert!(!stage.exists());
}
struct Changing<'a> {
    source: &'a Source,
    path: String,
    count: std::sync::atomic::AtomicUsize,
}
impl MetadataSource for Changing<'_> {
    fn identity(&self) -> &str {
        self.source.identity()
    }
    fn catalog(&self) -> &cf_domain::Catalog {
        self.source.catalog()
    }
    fn read(&self, p: &str, c: &Cancellation) -> Result<Vec<u8>> {
        let mut bytes = self.source.read(p, c)?;
        if p == self.path
            && self
                .count
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
                > 0
        {
            bytes.push(b' ');
        }
        Ok(bytes)
    }
    fn exists(&self, p: &str, c: &Cancellation) -> Result<bool> {
        self.source.exists(p, c)
    }
    fn list_directory(&self, p: &str, c: &Cancellation) -> Result<Vec<SourceEntry>> {
        self.source.list_directory(p, c)
    }
    fn files_below(&self, p: &str, c: &Cancellation) -> Result<Vec<String>> {
        self.source.files_below(p, c)
    }
    fn snapshot(&self, c: &Cancellation) -> Result<String> {
        self.source.snapshot(c)
    }
}
#[test]
fn changed_repeated_read_aborts_preview_instead_of_replacing_its_digest() {
    let all = rows();
    let mut checked = 0;
    for row in all["cases"].as_array().unwrap() {
        let temp = tempfile::tempdir().unwrap();
        let src = source(row, &temp.path().join("source"), false);
        let selected = serde_json::from_value(row["selected"].clone()).unwrap();
        let cancel = Cancellation::default();
        RustEngine
            .preview(&src, None, &selected, &BTreeSet::new(), &cancel)
            .unwrap();
        let paths = src
            .reads
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, n)| **n > 1)
            .map(|(p, _)| p.clone())
            .collect::<Vec<_>>();
        if let Some(path) = paths.into_iter().next() {
            let changing = Changing {
                source: &src,
                path,
                count: std::sync::atomic::AtomicUsize::new(0),
            };
            let result = RustEngine.preview(&changing, None, &selected, &BTreeSet::new(), &cancel);
            assert!(
                matches!(result,Err(Error::Invalid(message)) if message.contains("изменились во время предпросмотра"))
            );
            checked += 1;
        }
    }
    assert!(checked > 0, "oracle must exercise repeated reads");
}

#[test]
fn profiled_complete_packages_match_the_independent_oracle() {
    let context = cf_application::profile::Context::new("preview", true, None);
    {
        let _scope = context.enter();
        complete_packages_match_python_with_and_without_the_saved_index();
    }
    let report = context.report("ok").unwrap();
    assert!(report.metrics["engine.total"].totals.calls > 0);
    assert_eq!(report.final_in_flight, 0);
}
