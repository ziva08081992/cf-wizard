//! Headless benchmark of the existing production preview path. No staging/native build.
use cf_application::{Cancellation, DependencyEngine, FactStore, MetadataSource, SourceEntry};
use cf_domain::{Catalog, Plan, Result};
use cf_infrastructure::{
    engine::RustEngine,
    index::{IndexReader, Proof},
    journal::OperationLog,
    source::LocalSource,
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
    time::Instant,
};
struct Metered {
    inner: LocalSource,
    snapshots: AtomicU64,
    reads: AtomicU64,
    listings: AtomicU64,
    existence: AtomicU64,
    owned: AtomicU64,
}
fn timed<T>(counter: &AtomicU64, f: impl FnOnce() -> T) -> T {
    let start = Instant::now();
    let result = f();
    counter.fetch_add(start.elapsed().as_nanos() as u64, Ordering::Relaxed);
    result
}
impl MetadataSource for Metered {
    fn identity(&self) -> &str {
        self.inner.identity()
    }
    fn catalog(&self) -> &Catalog {
        self.inner.catalog()
    }
    fn materialized_root(&self) -> Option<&std::path::Path> {
        self.inner.materialized_root()
    }
    fn read(&self, p: &str, c: &Cancellation) -> Result<Vec<u8>> {
        timed(&self.reads, || self.inner.read(p, c))
    }
    fn exists(&self, p: &str, c: &Cancellation) -> Result<bool> {
        timed(&self.existence, || self.inner.exists(p, c))
    }
    fn list_directory(&self, p: &str, c: &Cancellation) -> Result<Vec<SourceEntry>> {
        timed(&self.listings, || self.inner.list_directory(p, c))
    }
    fn files_below(&self, p: &str, c: &Cancellation) -> Result<Vec<String>> {
        timed(&self.owned, || self.inner.files_below(p, c))
    }
    fn snapshot(&self, c: &Cancellation) -> Result<String> {
        timed(&self.snapshots, || self.inner.snapshot(c))
    }
}

#[derive(Deserialize)]
struct Job {
    source: PathBuf,
    selected: BTreeSet<String>,
    index: Option<PathBuf>,
    logs: PathBuf,
    output: PathBuf,
}
fn normalized(plan: &Plan) -> Value {
    let stubs = plan.stubs.iter().map(|(key, stub)| {
        let causes = stub.causes.iter().map(|c| (c.owner.clone(), c.required)).collect::<BTreeMap<_,_>>();
        (key.clone(), json!({"primary":stub.causes.first().map(|c|c.owner.as_str()).unwrap_or(""),"causes":causes}))
    }).collect::<BTreeMap<_,_>>();
    let mut warnings = plan
        .warnings
        .iter()
        .map(|s| serde_json::from_str::<Value>(s).expect("production warning JSON"))
        .collect::<Vec<_>>();
    warnings.sort_by_key(|v| v["object"].as_str().unwrap_or("").to_owned());
    json!({"selected":plan.selected,"stubs":stubs,"members":plan.members,"warnings":warnings})
}
fn run(job: &Job) -> Result<Value> {
    let init = Instant::now();
    let source = Metered {
        inner: LocalSource::open(&job.source)?,
        snapshots: AtomicU64::new(0),
        reads: AtomicU64::new(0),
        listings: AtomicU64::new(0),
        existence: AtomicU64::new(0),
        owned: AtomicU64::new(0),
    };
    let init_seconds = init.elapsed().as_secs_f64();
    let total = Instant::now();
    let log = OperationLog::create(&job.logs, "rust-preview-benchmark")?;
    let analysis = Instant::now();
    let index = job
        .index
        .as_ref()
        .map(|p| IndexReader::open(p, Proof::Local(source.identity())))
        .transpose()?;
    let plan = RustEngine.preview(
        &source,
        index.as_ref().map(|r| r as &dyn FactStore),
        &job.selected,
        &BTreeSet::new(),
        &Cancellation::default(),
    )?;
    let analysis_seconds = analysis.elapsed().as_secs_f64();
    let snapshot_seconds = source.snapshots.load(Ordering::Relaxed) as f64 / 1e9;
    log.value("dependencies-ready",json!({"selected":plan.composition().selected,"stubs":plan.composition().stubs,"warnings":plan.composition().warnings}))?;
    let preview_seconds = total.elapsed().as_secs_f64();
    let result = json!({
        "engine":"rust","analysis_seconds":analysis_seconds,"preview_seconds":preview_seconds,
        "initialization_seconds":init_seconds,"index_opened":index.is_some(),"source_identity":source.identity(),
        "snapshot_seconds":snapshot_seconds,"analysis_excluding_snapshot_seconds":analysis_seconds-snapshot_seconds,
        "source_io_seconds":{"snapshot":snapshot_seconds,"read":source.reads.load(Ordering::Relaxed) as f64/1e9,
            "listings":source.listings.load(Ordering::Relaxed) as f64/1e9,"exists":source.existence.load(Ordering::Relaxed) as f64/1e9,
            "owned_files":source.owned.load(Ordering::Relaxed) as f64/1e9},
        "normalized":normalized(plan.composition()),"read_hashes":plan.composition().source_hashes,
        "source_snapshot":plan.composition().source_fingerprint,"stub_count":plan.composition().stubs.len(),
        "selected_count":plan.composition().selected.len(),"journal":log.path,
    });
    if source.snapshot(&Cancellation::default())? != plan.composition().source_fingerprint {
        return Err(cf_domain::Error::Invalid(
            "Export markers changed during benchmark".into(),
        ));
    }
    Ok(result)
}
fn main() {
    let config = std::env::args_os().nth(1).expect("job.json");
    let job: Job = serde_json::from_slice(&fs::read(config).expect("read job")).expect("job JSON");
    let result = match run(&job) {
        Ok(value) => value,
        Err(error) => json!({"engine":"rust","error":error.to_string()}),
    };
    fs::write(&job.output, serde_json::to_vec_pretty(&result).unwrap())
        .expect("write benchmark result");
    if result.get("error").is_some() {
        eprintln!("{}", result["error"]);
        std::process::exit(1);
    }
}
