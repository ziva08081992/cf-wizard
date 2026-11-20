//! Offline fixed-workload comparison. Only the ignored test uses this module.
use super::*;
use crate::{facts_cache, source_reader::SourceDocument};
use cf_application::profile;
use cf_domain::Facts;
use rusqlite::{Connection, OpenFlags, params, types::ValueRef};
use sha2::{Digest, Sha256};
use std::{env, fs, io::Read as _, path::Path, time::Instant};

const OBJECTS: usize = 822;
const STRUCTURAL: usize = 78;
const REPEATED: usize = 4;
const SCHEMA: &str = "CREATE TABLE objects(
    name TEXT PRIMARY KEY, content_hash BLOB NOT NULL, facts BLOB NOT NULL
) WITHOUT ROWID";
const SAVE: &str = "INSERT INTO objects(name,content_hash,facts) VALUES(?,?,?)";
const SELECT: &str = "SELECT content_hash,facts FROM objects WHERE name=?";

#[derive(Deserialize)]
struct HistoricalManifest {
    distinct: usize,
    inflated_bytes: usize,
    xml_sha256: String,
    v2_sha256: String,
}

#[derive(Serialize, Deserialize)]
struct Dataset {
    distinct: usize,
    xml_sha256: String,
    v2_sha256: String,
    v3_sha256: String,
    v2_inflated: usize,
    v3_inflated: usize,
    v2_bodies: usize,
    v3_bodies: usize,
    v2_file_bytes: u64,
    v3_file_bytes: u64,
}

fn basename(index: usize) -> String {
    format!("Fixture{index:04}")
}

fn update_record(state: &mut Sha256, bytes: &[u8]) {
    state.update((bytes.len() as u64).to_le_bytes());
    state.update(bytes);
}

fn prepare(fixtures: &Path, output: &Path) {
    let historical: HistoricalManifest =
        serde_json::from_reader(fs::File::open(fixtures.join("manifest.json")).unwrap()).unwrap();
    assert_eq!(historical.distinct, OBJECTS);
    fs::create_dir(output).unwrap();
    let v2_path = output.join("v2.sqlite3");
    let v3_path = output.join("v3.sqlite3");
    let mut v2 = Connection::open(&v2_path).unwrap();
    let mut v3 = Connection::open(&v3_path).unwrap();
    v2.execute_batch(SCHEMA).unwrap();
    v3.execute_batch(SCHEMA).unwrap();
    let v2_tx = v2.transaction().unwrap();
    let v3_tx = v3.transaction().unwrap();
    let mut xml_digest = Sha256::new();
    let mut v2_digest = Sha256::new();
    let mut v3_digest = Sha256::new();
    let mut v2_bodies = 0;
    let mut v3_bodies = 0;
    let mut v2_inflated = 0;
    let mut v3_inflated = 0;
    let versions = FactVersions::current();
    {
        let mut save_v2 = v2_tx.prepare(SAVE).unwrap();
        let mut save_v3 = v3_tx.prepare(SAVE).unwrap();
        for i in 0..OBJECTS {
            let stem = basename(i);
            let key = format!("Catalog.{stem}");
            let xml = fs::read(fixtures.join(format!("{stem}.xml"))).unwrap();
            let older = fs::read(fixtures.join(format!("{stem}.v2"))).unwrap();
            let hash = Sha256::digest(&xml);
            let facts = facts_cache::decode(&older, &key, &hash, &versions).unwrap();
            let newer = facts_cache::encode_v3(&facts).unwrap();
            assert_eq!(&newer[..5], b"CFSF\x01");
            assert!(facts_cache::decode_v3(&newer, &key, &hash, &versions).unwrap() == facts);
            v2_inflated += serde_json::to_vec(&facts).unwrap().len();
            v3_inflated += std::io::copy(
                &mut flate2::read::ZlibDecoder::new(&newer[5..])
                    .take(super::super::MAX_PAYLOAD + 1),
                &mut io::sink(),
            )
            .unwrap() as usize;
            v2_bodies += older.len();
            v3_bodies += newer.len();
            update_record(&mut xml_digest, &xml);
            update_record(&mut v2_digest, &older);
            update_record(&mut v3_digest, &newer);
            save_v2.execute(params![key, &hash[..], older]).unwrap();
            save_v3.execute(params![key, &hash[..], newer]).unwrap();
        }
    }
    let xml_sha256 = hex::encode(xml_digest.finalize());
    let v2_sha256 = hex::encode(v2_digest.finalize());
    assert_eq!(xml_sha256, historical.xml_sha256);
    assert_eq!(v2_sha256, historical.v2_sha256);
    assert_eq!(v2_inflated, historical.inflated_bytes);
    v2_tx.commit().unwrap();
    v3_tx.commit().unwrap();
    drop((v2, v3));
    let dataset = Dataset {
        distinct: OBJECTS,
        xml_sha256,
        v2_sha256,
        v3_sha256: hex::encode(v3_digest.finalize()),
        v2_inflated,
        v3_inflated,
        v2_bodies,
        v3_bodies,
        v2_file_bytes: fs::metadata(v2_path).unwrap().len(),
        v3_file_bytes: fs::metadata(v3_path).unwrap().len(),
    };
    serde_json::to_writer_pretty(
        fs::File::create(output.join("manifest.json")).unwrap(),
        &dataset,
    )
    .unwrap();
}

#[derive(Clone, Copy)]
enum Mode {
    V2Owned,
    V2Borrowed,
    V3Borrowed,
}

impl Mode {
    fn label(self) -> &'static str {
        match self {
            Self::V2Owned => "v2-owned",
            Self::V2Borrowed => "v2-borrowed",
            Self::V3Borrowed => "v3-borrowed",
        }
    }

    fn file(self) -> &'static str {
        if matches!(self, Self::V3Borrowed) {
            "v3.sqlite3"
        } else {
            "v2.sqlite3"
        }
    }

    fn lookup(
        self,
        connection: &Connection,
        key: &str,
        versions: &FactVersions,
    ) -> Result<SourceFacts> {
        profile::measure("facts.lookup", "operation", 0, Some(key), || {
            let mut sql = profile::span("facts.sql", "facts.lookup", 0, None);
            let result = match self {
                Self::V2Owned => connection.query_row(SELECT, [key], |row| {
                    let hash: Vec<u8> = row.get(0)?;
                    let bytes: Vec<u8> = row.get(1)?;
                    sql.success();
                    drop(sql);
                    Ok(facts_cache::decode(&bytes, key, &hash, versions))
                }),
                Self::V2Borrowed | Self::V3Borrowed => connection
                    .prepare_cached(SELECT)
                    .map_err(crate::external)?
                    .query_row([key], |row| {
                        let ValueRef::Blob(hash) = row.get_ref(0)? else {
                            panic!("benchmark content hash is not a BLOB");
                        };
                        let ValueRef::Blob(bytes) = row.get_ref(1)? else {
                            panic!("benchmark facts are not a BLOB");
                        };
                        sql.success();
                        drop(sql);
                        Ok(if matches!(self, Self::V3Borrowed) {
                            facts_cache::decode_v3(bytes, key, hash, versions)
                        } else {
                            facts_cache::decode(bytes, key, hash, versions)
                        })
                    }),
            };
            result.map_err(crate::external)?
        })
    }
}

struct HashWriter(Sha256);
impl io::Write for HashWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.update(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(windows)]
fn peak_working_set() -> Option<usize> {
    #[repr(C)]
    struct Counters {
        size: u32,
        faults: u32,
        peak_working_set: usize,
        remaining_counters: [usize; 7],
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetCurrentProcess() -> *mut std::ffi::c_void;
        fn K32GetProcessMemoryInfo(
            process: *mut std::ffi::c_void,
            counters: *mut Counters,
            size: u32,
        ) -> i32;
    }
    let size = size_of::<Counters>() as u32;
    let mut counters = Counters {
        size,
        faults: 0,
        peak_working_set: 0,
        remaining_counters: [0; 7],
    };
    // The pseudo handle needs no close. The buffer matches PROCESS_MEMORY_COUNTERS.
    let ok = unsafe { K32GetProcessMemoryInfo(GetCurrentProcess(), &mut counters, size) };
    (ok != 0).then_some(counters.peak_working_set)
}

#[cfg(not(windows))]
fn peak_working_set() -> Option<usize> {
    None
}

fn structure(facts: &SourceFacts) -> Facts {
    profile::measure("facts.structural", "operation", 0, None, || {
        facts.structural_facts()
    })
    .unwrap()
}

fn sample(fixtures: &Path, output: &Path, mode: Mode) {
    let dataset: Dataset =
        serde_json::from_reader(fs::File::open(output.join("manifest.json")).unwrap()).unwrap();
    assert_eq!(dataset.distinct, OBJECTS);
    let mut xml_digest = Sha256::new();
    let xml: Vec<_> = (0..OBJECTS)
        .map(|i| {
            let data = fs::read(fixtures.join(format!("{}.xml", basename(i)))).unwrap();
            update_record(&mut xml_digest, &data);
            data
        })
        .collect();
    assert_eq!(hex::encode(xml_digest.finalize()), dataset.xml_sha256);
    let connection =
        Connection::open_with_flags(output.join(mode.file()), OpenFlags::SQLITE_OPEN_READ_ONLY)
            .unwrap();
    let versions = FactVersions::current();
    let context = profile::Context::new("compact-source-facts-comparison", true, None);
    let started = Instant::now();
    let mut documents = Vec::with_capacity(OBJECTS);
    let mut structural = Vec::with_capacity(STRUCTURAL);
    {
        let _scope = context.enter();
        for position in 0..OBJECTS {
            let index = position * 293 % OBJECTS;
            let stem = basename(index);
            let key = format!("Catalog.{stem}");
            let path = format!("Catalogs/{stem}.xml");
            let facts = mode.lookup(&connection, &key, &versions).unwrap();
            if (REPEATED..STRUCTURAL).contains(&position) {
                structural.push(structure(&facts));
            }
            documents.push(
                SourceDocument::cached(
                    &key,
                    &path,
                    &xml[index],
                    SourceProof {
                        identity: "generated-source-facts".into(),
                        snapshot: "fixture-current".into(),
                        inputs: BTreeMap::new(),
                    },
                    facts,
                )
                .unwrap(),
            );
            if position < REPEATED {
                let facts = mode.lookup(&connection, &key, &versions).unwrap();
                structural.push(structure(&facts));
            }
        }
    }
    let pipeline_ns = started.elapsed().as_nanos() as u64;
    let peak_working_set_bytes = peak_working_set();
    let report = context.report("ok").unwrap();
    assert_eq!(report.metrics["facts.lookup"].unique, OBJECTS);
    assert_eq!(
        report.metrics["facts.lookup"].totals.calls,
        (OBJECTS + REPEATED) as u64
    );
    assert_eq!(
        report.metrics["document.cached"].totals.calls,
        OBJECTS as u64
    );
    assert_eq!(documents.len(), OBJECTS);
    assert_eq!(structural.len(), STRUCTURAL);

    // Exact evidence is produced after the timer and peak-memory snapshot.
    let mut digest = HashWriter(Sha256::new());
    for document in documents {
        serde_json::to_writer(
            &mut digest,
            &(document.object, &document.node_ids, &document.facts),
        )
        .unwrap();
        for i in 0..document.node_ids.len() {
            serde_json::to_writer(&mut digest, document.targets(i)).unwrap();
        }
    }
    serde_json::to_writer(&mut digest, &structural).unwrap();
    let receipt = serde_json::json!({
        "mode": mode.label(), "dataset": dataset, "pipeline_ns": pipeline_ns,
        "peak_working_set_bytes": peak_working_set_bytes,
        "semantic_sha256": hex::encode(digest.0.finalize()), "profile": report,
    });
    let report_path = env::var_os("CF_COMPACT_REPORT").unwrap();
    serde_json::to_writer_pretty(fs::File::create(report_path).unwrap(), &receipt).unwrap();
}

#[test]
#[ignore = "requires frozen offline facts-workload-v1; on-demand comparison"]
fn frozen_822_compact_source_facts_pipeline() {
    let fixtures = env::var_os("CF_COMPACT_FIXTURES").unwrap();
    let output = env::var_os("CF_COMPACT_DATASET").unwrap();
    let (fixtures, output) = (Path::new(&fixtures), Path::new(&output));
    match env::var("CF_COMPACT_MODE").unwrap().as_str() {
        "prepare" => prepare(fixtures, output),
        "v2-owned" => sample(fixtures, output, Mode::V2Owned),
        "v2-borrowed" => sample(fixtures, output, Mode::V2Borrowed),
        "v3-borrowed" => sample(fixtures, output, Mode::V3Borrowed),
        other => panic!("unknown SourceFacts benchmark mode: {other}"),
    }
}
