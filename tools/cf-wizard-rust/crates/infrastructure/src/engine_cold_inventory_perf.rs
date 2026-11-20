// Identical test-only worker in the diagnostics.2 control and candidate.
use super::*;
use crate::{
    git::{
        Connection,
        cold_inventory_perf::{Corpus, Transport, WireCall},
    },
    git_source::GitSource,
    index::{IndexReader, Proof},
    indexing,
    source::canonical_future,
};
use cf_application::PreparedPlan;
use cf_domain::{GitInputs, canonical::sha256};
use serde::Serialize;
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::Mutex,
    time::Instant,
};

#[derive(Serialize)]
struct Window {
    phase: String,
    paths: usize,
    start_ns: u64,
    end_ns: u64,
}

struct ObservedSource {
    actual: GitSource,
    transport: Arc<Transport>,
    phase: Mutex<String>,
    windows: Mutex<Vec<Window>>,
}

impl ObservedSource {
    fn phase(&self, phase: &str) {
        self.transport.phase(phase);
        *self.phase.lock().unwrap() = phase.into();
    }
}

impl MetadataSource for ObservedSource {
    fn profile_counters(&self) -> Option<profile::SourceCounters> {
        self.actual.profile_counters()
    }

    fn manifest(&self) -> Option<&GitInputs> {
        self.actual.manifest()
    }

    fn materialized_root(&self) -> Option<&Path> {
        self.actual.materialized_root()
    }

    fn identity(&self) -> &str {
        self.actual.identity()
    }

    fn catalog(&self) -> &cf_domain::Catalog {
        self.actual.catalog()
    }

    fn read(&self, relative: &str, cancel: &Cancellation) -> Result<Vec<u8>> {
        self.actual.read(relative, cancel)
    }

    fn prefetch(&self, paths: &[String], cancel: &Cancellation) -> Result<()> {
        self.actual.prefetch(paths, cancel)
    }

    fn files_below(&self, relative: &str, cancel: &Cancellation) -> Result<Vec<String>> {
        self.actual.files_below(relative, cancel)
    }

    fn inventory_window(
        &self,
        paths: &[String],
        cancel: &Cancellation,
    ) -> Result<Option<Vec<Result<Vec<String>>>>> {
        let start_ns = self.transport.now();
        let result = self.actual.inventory_window(paths, cancel);
        let end_ns = self.transport.now();
        self.windows.lock().unwrap().push(Window {
            phase: self.phase.lock().unwrap().clone(),
            paths: paths.len(),
            start_ns,
            end_ns,
        });
        result
    }

    fn exists(&self, relative: &str, cancel: &Cancellation) -> Result<bool> {
        self.actual.exists(relative, cancel)
    }

    fn list_directory(&self, relative: &str, cancel: &Cancellation) -> Result<Vec<SourceEntry>> {
        self.actual.list_directory(relative, cancel)
    }

    fn snapshot(&self, cancel: &Cancellation) -> Result<String> {
        self.actual.snapshot(cancel)
    }

    fn is_pinned(&self) -> bool {
        self.actual.is_pinned()
    }
}

fn fail(message: &str) -> Error {
    Error::Invalid(message.into())
}

fn path_env(name: &str, root: &Path) -> Result<PathBuf> {
    let path = PathBuf::from(std::env::var_os(name).ok_or_else(|| fail(name))?);
    if !path.is_absolute() {
        return Err(fail("Benchmark path must be absolute"));
    }
    let path = canonical_future(&path)?;
    if path == root || !path.starts_with(root) {
        return Err(fail("Benchmark path escapes its owned root"));
    }
    Ok(path)
}

fn save(path: &Path, value: &impl Serialize) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(value).map_err(crate::external)?;
    fs::write(path, bytes).map_err(crate::external)
}

fn hashed_staging(root: &Path) -> Result<BTreeMap<String, String>> {
    let mut pending = vec![root.to_owned()];
    let mut result = BTreeMap::new();
    while let Some(directory) = pending.pop() {
        for item in fs::read_dir(directory).map_err(crate::external)? {
            let item = item.map_err(crate::external)?;
            let kind = item.file_type().map_err(crate::external)?;
            let path = item.path();
            crate::source::no_links(&path)?;
            if kind.is_dir() {
                pending.push(path);
            } else if kind.is_file() {
                let name = path
                    .strip_prefix(root)
                    .map_err(crate::external)?
                    .to_str()
                    .ok_or_else(|| fail("Non-UTF-8 staging path"))?
                    .replace('\\', "/");
                result.insert(name, sha256(&fs::read(path).map_err(crate::external)?));
            } else {
                return Err(fail("Unexpected staging entry"));
            }
        }
    }
    Ok(result)
}

fn record_seed(
    source: &ObservedSource,
    index: &Path,
    cache: &Path,
    out: &Path,
    cancel: &Cancellation,
) -> Result<()> {
    source.phase("seed.index");
    let index_started = Instant::now();
    let update = indexing::update(source, index, cancel, &|_, _| {})?;
    let index_ns = index_started.elapsed().as_nanos() as u64;

    source.phase("seed.payload");
    let payload_started = Instant::now();
    let paths = source
        .transport
        .corpus
        .files
        .keys()
        .map(|path| path.strip_prefix("config/").unwrap().to_owned())
        .collect::<Vec<_>>();
    source.prefetch(&paths, cancel)?;
    let payload_ns = payload_started.elapsed().as_nanos() as u64;
    let legacy = source
        .materialized_root()
        .ok_or_else(|| fail("Missing legacy seed path"))?;
    let blobs = cache
        .join(source.actual.connection().repo_key())
        .join("blobs");
    for (path, file) in &source.transport.corpus.files {
        let relative = path.strip_prefix("config/").unwrap();
        for local in [legacy.join(relative), blobs.join(&file.oid)] {
            if sha256(&fs::read(local).map_err(crate::external)?) != file.sha256 {
                return Err(fail("Seed payload failed byte parity"));
            }
        }
    }
    save(
        &out.join("seed.json"),
        &json!({
            "source_identity": source.identity(),
            "index": update,
            "index_ns": index_ns,
            "payload_ns": payload_ns,
            "verified_source_files": paths.len(),
            "legacy_root": legacy,
        }),
    )
}

struct Sample<'a> {
    source: &'a ObservedSource,
    index: &'a Path,
    out: &'a Path,
    cancel: &'a Cancellation,
}

impl Sample<'_> {
    fn run(&self, name: &str, full_start: Instant, source_open_ns: u64) -> Result<Value> {
        let source = self.source;
        let index_phase = format!("{name}.index");
        let preview_phase = format!("{name}.preview");
        source.phase(&index_phase);
        let context = profile::Context::new("preview", true, source.profile_counters());
        let selected = BTreeSet::from(["CommonModule.Probe".into()]);
        let excluded = BTreeSet::new();
        let (recipe, index_ns, preview_ns, full_ns) = {
            let _scope = context.enter();
            let index_started = Instant::now();
            let ready_index = indexing::path_for(source, self.index)?;
            let reader = IndexReader::open(&ready_index, Proof::Git(source.manifest().unwrap()))?;
            let index_ns = index_started.elapsed().as_nanos() as u64;

            source.phase(&preview_phase);
            let started = Instant::now();
            let recipe = profile::measure("engine.total", "operation", 0, None, || {
                prepare(source, Some(&reader), &selected, &excluded, self.cancel)
            })?;
            let preview_ns = started.elapsed().as_nanos() as u64;
            (
                recipe,
                index_ns,
                preview_ns,
                full_start.elapsed().as_nanos() as u64,
            )
        };
        let expected = &source.transport.corpus.owner_files;
        if recipe.composition.stubs.len() != expected.len()
            || !recipe.composition.diagnostics.is_empty()
        {
            return Err(fail(
                "Synthetic workload did not produce its complete expected stub plan",
            ));
        }
        for (key, paths) in expected {
            if recipe.below.get(key) != Some(paths) {
                return Err(Error::Invalid(format!(
                    "Full owner inventory differs for {key}"
                )));
            }
        }

        let mut composition = serde_json::to_value(&recipe.composition).map_err(crate::external)?;
        composition.as_object_mut().unwrap().remove("metrics");
        let compiled = recipe
            .compiled
            .as_deref()
            .ok_or_else(|| fail("Missing compiled plan"))?;
        compiled.validate()?;
        let compiled_sha = sha256(&serde_json::to_vec(compiled).map_err(crate::external)?);
        let composition_sha = sha256(&serde_json::to_vec(&composition).map_err(crate::external)?);
        let phase_stage = self.out.join(format!("{name}-stage"));
        let phase_settings = self.out.join(format!("{name}-settings.xml"));
        source.phase(&format!("{name}.materialize"));
        let started = Instant::now();
        let package = recipe.write(
            source,
            &excluded,
            &phase_stage,
            &phase_settings,
            self.cancel,
        )?;
        let materialize_ns = started.elapsed().as_nanos() as u64;
        let files = hashed_staging(&phase_stage)?;
        let settings_sha = sha256(&fs::read(phase_settings).map_err(crate::external)?);
        if files != package.files || settings_sha != package.settings_hash {
            return Err(fail("Staged bytes do not match package manifest"));
        }
        let parity = json!({
            "compiled_sha256": compiled_sha,
            "composition_sha256": composition_sha,
            "source_proof_sha256": compiled.source.digest()?,
            "identity": compiled.source.identity,
            "manifest_sha256": source.manifest().unwrap().fingerprint()?,
            "files": files,
            "settings_sha256": settings_sha,
            "stub_descriptors": package.readback.stub_descriptors,
            "full_dependencies": package.readback.full_dependencies,
        });
        let parity_sha = sha256(&serde_json::to_vec(&parity).map_err(crate::external)?);
        save(&self.out.join(format!("{name}-compiled.json")), compiled)?;
        save(
            &self.out.join(format!("{name}-composition.json")),
            &composition,
        )?;
        save(&self.out.join(format!("{name}-parity.json")), &parity)?;

        let trace = source.transport.calls();
        let windows = source.windows.lock().unwrap();
        let inventory: Vec<_> = trace
            .iter()
            .filter(|call| {
                call.route == "tree"
                    && call.phase == preview_phase
                    && windows.iter().any(|window| {
                        window.phase == preview_phase
                            && window.start_ns <= call.start_ns
                            && call.start_ns <= window.end_ns
                    })
            })
            .collect();
        let tree_pages = |phase: &str| -> usize {
            trace
                .iter()
                .filter(|call| call.route == "tree" && call.phase == phase)
                .count()
        };
        let inv_bytes: usize = inventory.iter().map(|call| call.bytes).sum();
        Ok(json!({
            "phase": name,
            "full_ns": full_ns,
            "source_open_ns": source_open_ns,
            "index_open_ns": index_ns,
            "preview_ns": preview_ns,
            "materialize_ns": materialize_ns,
            "bootstrap_tree_pages": tree_pages(&format!("{name}.bootstrap")),
            "preview_tree_pages": tree_pages(&preview_phase),
            "inventory_http_pages": inventory.len(),
            "inventory_http_bytes": inv_bytes,
            "recursive_inventory_http_pages": inventory.iter().filter(|call| call.recursive).count(),
            "preview_profile": context.report("ok").unwrap(),
            "parity_sha256": parity_sha,
            "parity": parity,
        }))
    }
}

fn run() -> Result<()> {
    let mode = std::env::var("CF_COLD_BENCH_MODE").map_err(crate::external)?;
    if !matches!(mode.as_str(), "seed" | "first" | "restart" | "partial") {
        return Err(fail("Invalid benchmark mode"));
    }
    let root = PathBuf::from(
        std::env::var_os("CF_COLD_BENCH_ROOT").ok_or_else(|| fail("CF_COLD_BENCH_ROOT"))?,
    );
    if !root.is_absolute() {
        return Err(fail("Benchmark root must be absolute"));
    }
    let root = canonical_future(&root)?;
    let cache = path_env("CF_COLD_BENCH_CACHE", &root)?;
    let fixture = path_env("CF_COLD_BENCH_FIXTURE", &root)?;
    let index = path_env("CF_COLD_BENCH_INDEX", &root)?;
    let out = path_env("CF_COLD_BENCH_OUT", &root)?;
    let delay_ms: u64 = std::env::var("CF_COLD_BENCH_DELAY")
        .map_err(crate::external)?
        .parse()
        .map_err(crate::external)?;
    if !matches!(delay_ms, 0 | 20)
        || !out.is_dir()
        || fs::read_dir(&out)
            .map_err(crate::external)?
            .next()
            .is_some()
    {
        return Err(fail("Expected frozen delay and an empty receipt directory"));
    }
    let fixture_bytes = fs::read(fixture).map_err(crate::external)?;
    let fixture_sha = sha256(&fixture_bytes);
    let corpus: Corpus = serde_json::from_slice(&fixture_bytes).map_err(crate::external)?;
    let transport = Transport::new(corpus, delay_ms)?;
    let connection = Connection {
        server: "https://example.test".into(),
        repository: "diagnostic/cold-inventory".into(),
        provider: "gitea".into(),
        branch: "main".into(),
        directory: "config".into(),
        cache: cache
            .to_str()
            .ok_or_else(|| fail("Non-UTF-8 cache path"))?
            .into(),
        ..Connection::default()
    };
    let cancel = Cancellation::default();
    transport.phase(&format!("{mode}.bootstrap"));
    let open = profile::Context::new("source_open", true, None);
    let start = Instant::now();
    let actual = {
        let _scope = open.enter();
        GitSource::open(&connection, transport.provider(), &cancel)?
    };
    let source_open_ns = start.elapsed().as_nanos() as u64;
    let source = ObservedSource {
        actual,
        transport: transport.clone(),
        phase: Mutex::default(),
        windows: Mutex::default(),
    };
    let mut samples = vec![];
    if mode == "seed" {
        record_seed(&source, &index, &cache, &out, &cancel)?;
    } else {
        let worker = Sample {
            source: &source,
            index: &index,
            out: &out,
            cancel: &cancel,
        };
        samples.push(worker.run(&mode, start, source_open_ns)?);
        if mode == "first" {
            samples.push(worker.run("ram", Instant::now(), 0)?);
        }
    }
    let calls: Vec<WireCall> = transport.calls();
    let (max_in_flight, final_in_flight) = transport.in_flight();
    save(&out.join("wire.json"), &calls)?;
    save(
        &out.join("inventory-windows.json"),
        &*source.windows.lock().unwrap(),
    )?;
    save(
        &out.join("report.json"),
        &json!({
            "mode": mode,
            "pid": std::process::id(),
            "fixture_sha256": fixture_sha,
            "revision": transport.corpus.revision,
            "delay_ms": delay_ms,
            "source_open_ns": source_open_ns,
            "source_open_profile": open.report("ok").unwrap(),
            "max_in_flight": max_in_flight,
            "final_in_flight": final_in_flight,
            "http_calls": calls.len(),
            "samples": samples,
        }),
    )?;
    if max_in_flight > 4 || final_in_flight != 0 || calls.iter().any(|call| !call.accepted) {
        return Err(fail("Unexpected synthetic transport outcome"));
    }
    Ok(())
}

#[test]
#[ignore = "requires isolated frozen benchmark fixture and driver"]
fn worker() {
    run().unwrap();
}
