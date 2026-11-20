//! Identical, test-only child of engine.rs in the diagnostics.5 and candidate checkouts.
use super::*;
use crate::{
    index::{IndexReader, Proof},
    indexing,
    source::LocalSource,
};
use cf_application::PreparedPlan;
use cf_domain::{GitInputs, canonical::sha256};
use serde::Deserialize;
use serde_json::{Value, json};
use sha1::Sha1;
use sha2::{Digest, Sha256};
use std::{
    env, fs,
    io::{self, Write},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

#[derive(Deserialize)]
struct FileProof {
    path: String,
    oid: String,
    sha256: String,
    size: usize,
}

#[derive(Deserialize)]
struct Fixture {
    fixture_version: u32,
    manifest: GitInputs,
    source_files: Vec<FileProof>,
    selection: BTreeSet<String>,
    exclusions: BTreeSet<String>,
    expected_stub_keys: BTreeSet<String>,
    expected_descriptor_count: usize,
}

struct FrozenSource {
    local: LocalSource,
    inputs: GitInputs,
    files: BTreeMap<String, FileProof>,
}

impl MetadataSource for FrozenSource {
    fn manifest(&self) -> Option<&GitInputs> {
        Some(&self.inputs)
    }

    fn materialized_root(&self) -> Option<&Path> {
        Some(&self.local.root)
    }

    fn identity(&self) -> &str {
        &self.inputs.source_key
    }

    fn catalog(&self) -> &cf_domain::Catalog {
        self.local.catalog()
    }

    fn read(&self, path: &str, cancel: &Cancellation) -> Result<Vec<u8>> {
        let expected = self.files.get(path).ok_or_else(|| {
            Error::Invalid(format!(
                "Synthetic pinned file is outside the fixture: {path}"
            ))
        })?;
        let data = self.local.read(path, cancel)?;
        let mut blob = Sha1::new();
        blob.update(format!("blob {}\0", data.len()));
        blob.update(&data);
        if data.len() != expected.size
            || hex::encode(blob.finalize()) != expected.oid
            || sha256(&data) != expected.sha256
        {
            return Err(Error::Invalid(format!(
                "Synthetic pinned bytes changed: {path}"
            )));
        }
        Ok(data)
    }

    fn exists(&self, path: &str, cancel: &Cancellation) -> Result<bool> {
        self.local.exists(path, cancel)
    }

    fn files_below(&self, path: &str, cancel: &Cancellation) -> Result<Vec<String>> {
        self.local.files_below(path, cancel)
    }

    fn list_directory(&self, path: &str, cancel: &Cancellation) -> Result<Vec<SourceEntry>> {
        self.local.list_directory(path, cancel)
    }

    fn snapshot(&self, cancel: &Cancellation) -> Result<String> {
        cancel.check()?;
        Ok(format!(
            "{}@{}",
            self.inputs.source_key, self.inputs.revision
        ))
    }

    fn is_pinned(&self) -> bool {
        true
    }
}

fn config(name: &str) -> PathBuf {
    PathBuf::from(env::var_os(name).unwrap_or_else(|| panic!("Missing {name}")))
}

fn phase(name: &str) {
    println!("MEMORY_PHASE {name}");
    io::stdout().flush().unwrap();
}

fn save(path: &Path, value: &Value) {
    let file = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(path)
        .unwrap();
    serde_json::to_writer(io::BufWriter::new(file), value).unwrap();
}

struct HashWriter(Sha256);

impl Write for HashWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.update(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn digest(value: &impl serde::Serialize) -> String {
    let mut writer = HashWriter(Sha256::new());
    serde_json::to_writer(&mut writer, value).unwrap();
    hex::encode(writer.0.finalize())
}

#[cfg(windows)]
fn cpu_ns() -> Option<u64> {
    #[repr(C)]
    #[derive(Default)]
    struct FileTime {
        low: u32,
        high: u32,
    }
    impl FileTime {
        fn ticks(&self) -> u64 {
            (u64::from(self.high) << 32) | u64::from(self.low)
        }
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetCurrentProcess() -> *mut std::ffi::c_void;
        fn GetProcessTimes(
            process: *mut std::ffi::c_void,
            created: *mut FileTime,
            exited: *mut FileTime,
            kernel: *mut FileTime,
            user: *mut FileTime,
        ) -> i32;
    }
    let mut created = FileTime::default();
    let mut exited = FileTime::default();
    let mut kernel = FileTime::default();
    let mut user = FileTime::default();
    let ok = unsafe {
        GetProcessTimes(
            GetCurrentProcess(),
            &mut created,
            &mut exited,
            &mut kernel,
            &mut user,
        )
    };
    (ok != 0).then(|| (kernel.ticks() + user.ticks()) * 100)
}

#[cfg(not(windows))]
fn cpu_ns() -> Option<u64> {
    None
}

fn validate_parity(
    recipe: &Recipe,
    source: &FrozenSource,
    out: &Path,
    exclusions: &BTreeSet<String>,
    cancel: &Cancellation,
) -> Value {
    let compiled = recipe.compiled.as_deref().unwrap();
    compiled.validate().unwrap();
    let mut composition = serde_json::to_value(recipe.composition()).unwrap();
    composition.as_object_mut().unwrap().remove("metrics");

    let stage = out.join("stage");
    let settings = out.join("merge-settings.xml");
    let package = recipe
        .write(source, exclusions, &stage, &settings, cancel)
        .unwrap();
    package
        .readback
        .validate(&package.composition, source.catalog(), &package.files)
        .unwrap();
    let settings_sha = sha256(&fs::read(settings).unwrap());
    assert_eq!(settings_sha, package.settings_hash);
    for (path, hash) in &package.files {
        let actual = sha256(&fs::read(stage.join(path)).unwrap());
        assert_eq!(&actual, hash, "{path}");
    }
    json!({
        "composition_sha256": digest(&composition),
        "compiled_sha256": digest(compiled),
        "source_proof_sha256": compiled.source.digest().unwrap(),
        "settings_sha256": settings_sha,
        "files_sha256": digest(&package.files),
        "readback_sha256": digest(&(&package.readback.stub_descriptors, &package.readback.full_dependencies)),
    })
}

fn wait_at_checkpoints(preview: usize) {
    let idle = Instant::now();
    for seconds in [5, 30, 60] {
        std::thread::sleep(Duration::from_secs(seconds).saturating_sub(idle.elapsed()));
        phase(&format!("idle{preview}_{seconds}"));
    }
    std::thread::sleep(Duration::from_secs(1));
}

#[test]
#[ignore = "offline frozen memory fixture, run through the external Windows sampler"]
fn frozen_preview_memory() {
    let root = config("CF_MEMORY_FIXTURE");
    let index_dir = config("CF_MEMORY_INDEX");
    let out = config("CF_MEMORY_SAMPLE");
    let mode = env::var("CF_MEMORY_MODE").unwrap();
    assert!(matches!(mode.as_str(), "seed" | "verify" | "sample"));
    let sample = mode == "sample";
    let cancel = Cancellation::default();

    let manifest_bytes = fs::read(root.join("manifest.json")).unwrap();
    let fixture: Fixture = serde_json::from_slice(&manifest_bytes).unwrap();
    assert_eq!(fixture.fixture_version, 1);
    let source = FrozenSource {
        local: LocalSource::open(&root.join("source")).unwrap(),
        inputs: fixture.manifest,
        files: fixture
            .source_files
            .into_iter()
            .map(|file| (file.path.clone(), file))
            .collect(),
    };
    source.inputs.validate().unwrap();
    let index = indexing::path_for(&source, &index_dir).unwrap();
    if mode == "seed" {
        assert!(!index.exists(), "Seed is immutable once generated");
        let update = indexing::update(&source, &index_dir, &cancel, &|_, _| {}).unwrap();
        assert_eq!(update.path, index);
        assert_eq!(update.inputs, source.inputs.entries.len());
        let reader = IndexReader::open(&index, Proof::Git(&source.inputs)).unwrap();
        drop(reader);
        save(
            &out.join("seed.json"),
            &json!({
                "index": index, "inputs": update.inputs, "bytes": update.bytes,
                "metadata_parses": update.metadata_parses,
                "manifest_sha256": sha256(&manifest_bytes),
                "git_inputs_sha256": source.inputs.fingerprint().unwrap(),
            }),
        );
        return;
    }

    let mut ready = None;
    let mut runs = Vec::with_capacity(3);
    let mut parity = None;
    println!();
    phase("initial");
    if sample {
        std::thread::sleep(Duration::from_secs(1));
    }
    for number in 1..=3 {
        phase(&format!("preview{number}"));
        let started = Instant::now();
        let cpu_before = cpu_ns();
        let context = profile::Context::new("preview-memory", true, None);
        let recipe = {
            let _scope = context.enter();
            let reader = IndexReader::open(&index, Proof::Git(&source.inputs)).unwrap();
            prepare(
                &source,
                Some(&reader),
                &fixture.selection,
                &fixture.exclusions,
                &cancel,
            )
            .unwrap()
        };
        let elapsed_ns = started.elapsed().as_nanos();
        let process_cpu_ns = cpu_before
            .zip(cpu_ns())
            .map(|(before, after)| after - before);
        phase(&format!("replace{number}"));
        let previous = ready.replace(recipe);
        drop(previous);
        phase(&format!("held{number}"));
        if sample {
            wait_at_checkpoints(number);
        }

        phase(&format!("assert{number}"));
        let recipe = ready.as_ref().unwrap();
        assert_eq!(recipe.composition.selected, fixture.selection);
        assert!(
            recipe.composition.diagnostics.is_empty(),
            "{:?}",
            recipe.composition.diagnostics
        );
        assert_eq!(
            recipe
                .composition
                .stubs
                .keys()
                .cloned()
                .collect::<BTreeSet<_>>(),
            fixture.expected_stub_keys,
        );
        assert!(
            recipe.compiled.is_some(),
            "Fixture must produce a compiled plan"
        );
        let report = context.report("success").unwrap();
        assert_eq!(
            report.metrics["document.cached"].unique, fixture.expected_descriptor_count,
            "Must inflate all expected descriptor documents"
        );
        if !sample || number == 3 {
            let dir = out.join(format!("parity{number}"));
            fs::create_dir(&dir).unwrap();
            let actual = validate_parity(recipe, &source, &dir, &fixture.exclusions, &cancel);
            if let Some(previous) = &parity {
                assert_eq!(&actual, previous, "Repeated plan semantic mismatch");
            }
            parity = Some(actual);
        }
        runs.push(json!({ "preview": number, "wall_ns": elapsed_ns,
            "process_cpu_ns": process_cpu_ns, "profile": report }));
    }
    phase("drop");
    drop(ready);
    if sample {
        std::thread::sleep(Duration::from_secs(1));
    }
    save(
        &out.join("result.json"),
        &json!({
            "mode": mode, "pid": std::process::id(),
            "manifest_sha256": sha256(&manifest_bytes),
            "git_inputs_sha256": source.inputs.fingerprint().unwrap(),
            "runs": runs, "parity": parity.unwrap(),
        }),
    );
}
