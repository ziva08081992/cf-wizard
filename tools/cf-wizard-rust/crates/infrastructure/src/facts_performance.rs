//! Offline acceptance fixture for the first V2 decode/document pass.
use super::*;
use crate::{facts_cache, source_xml};
use cf_domain::{
    Catalog, MetadataObject, ObjectKey,
    source_facts::{FactVersions, SourceFacts, SourceProof},
};
use serde::{Deserialize, Serialize};
use std::{
    env,
    fmt::Write as _,
    fs,
    io::{self, Write},
    path::Path,
    time::Instant,
};

const SEED: u64 = 20261119;
const DISTINCT: usize = 822;
const STRUCTURAL: usize = 78;
const REPEATS: usize = 4;
const MIN_INFLATED: usize = 1024 * 1024 * 1024;
const IDENTITY: &str = "generated-source-facts";
const SNAPSHOT: &str = "fixture-current";

#[derive(Serialize, Deserialize)]
struct Manifest {
    seed: u64,
    factor: usize,
    distinct: usize,
    inflated_bytes: usize,
    xml_sha256: String,
    v2_sha256: String,
}

struct HashWriter(Sha256);
impl Write for HashWriter {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        self.0.update(data);
        Ok(data.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
impl HashWriter {
    fn new() -> Self {
        Self(Sha256::new())
    }
    fn value(&mut self, value: &impl Serialize) {
        serde_json::to_writer(&mut *self, value).unwrap();
        self.write_all(b"\n").unwrap();
    }
    fn record(&mut self, data: &[u8]) {
        self.0.update((data.len() as u64).to_le_bytes());
        self.0.update(data);
    }
    fn finish(self) -> String {
        hex::encode(self.0.finalize())
    }
}

fn name(index: usize) -> String {
    format!("Fixture{index:04}")
}

fn attribute(xml: &mut String, number: usize, variant: usize) {
    let ty = match variant % 3 {
        0 => "xs:string",
        1 => "xs:dateTime",
        _ => "cfg:CatalogRef.Target",
    };
    write!(
        xml,
        "<Attribute><Properties><Name>Field{number}</Name><Synonym><v8:item><v8:lang>ru</v8:lang><v8:content>Поле {number}</v8:content></v8:item></Synonym><Type><v8:Type>{ty}</v8:Type></Type></Properties></Attribute>"
    )
    .unwrap();
}

fn descriptor_xml(index: usize, factor: usize) -> String {
    let name = name(index);
    let variant = (SEED
        .wrapping_add(index as u64)
        .wrapping_mul(0x9e3779b97f4a7c15)
        >> 59) as usize;
    let mut xml = format!(
        "<MetaDataObject xmlns='{}' xmlns:v8='{}' xmlns:cfg='{}' xmlns:xs='http://www.w3.org/2001/XMLSchema' xmlns:x='urn:fixture:records'><Catalog uuid='00000000-0000-4000-8000-{index:012}'><Properties><Name>{name}</Name><CodeLength>9</CodeLength><Comment>Record {variant}, 2026-11-19</Comment></Properties><ChildObjects>",
        source_xml::MD,
        source_xml::CORE,
        source_xml::CURRENT
    );
    match index {
        0..600 => {
            for field in 0..(48 + variant) * factor {
                attribute(&mut xml, field, field + variant);
            }
        }
        600..780 => {
            for section in 0..(4 + variant % 5) * factor {
                write!(xml, "<TabularSection><Properties><Name>Rows{section}</Name></Properties><ChildObjects>").unwrap();
                for column in 0..(12 + variant % 7) {
                    attribute(&mut xml, column, column + section);
                }
                xml.push_str("</ChildObjects></TabularSection>");
            }
        }
        _ => {
            for record in 0..(18 + variant % 7) * factor {
                write!(xml, "<x:Record><x:Date>2026-11-{:02}</x:Date><x:Layer xmlns:x='urn:fixture:rows'><x:Layer><x:Layer><x:Layer>", 1 + record % 19).unwrap();
                for column in 0..(8 + variant % 5) {
                    write!(xml, "<x:Item><x:Code>{column:04}</x:Code><x:Value>Catalog.Target</x:Value></x:Item>").unwrap();
                }
                xml.push_str("</x:Layer></x:Layer></x:Layer></x:Layer></x:Record>");
            }
        }
    }
    xml.push_str("</ChildObjects></Catalog></MetaDataObject>");
    xml
}

fn generate(root: &Path) -> Manifest {
    fs::create_dir_all(root).unwrap();
    let mut factor = 1;
    loop {
        let mut xml_digest = HashWriter::new();
        let mut v2_digest = HashWriter::new();
        let mut inflated_bytes = 0;
        for index in 0..DISTINCT {
            let name = name(index);
            let key = format!("Catalog.{name}");
            let path = format!("Catalogs/{name}.xml");
            let xml = descriptor_xml(index, factor);
            let proof = SourceProof {
                identity: IDENTITY.into(),
                snapshot: "fixture-historical".into(),
                inputs: BTreeMap::new(),
            };
            let doc = SourceDocument::descriptor(&key, &path, xml.as_bytes(), proof).unwrap();
            inflated_bytes += serde_json::to_vec(&doc.facts).unwrap().len();
            let payload = facts_cache::encode(&doc.facts).unwrap();
            xml_digest.record(xml.as_bytes());
            v2_digest.record(&payload);
            fs::write(root.join(format!("{name}.xml")), xml).unwrap();
            fs::write(root.join(format!("{name}.v2")), payload).unwrap();
        }
        if inflated_bytes >= MIN_INFLATED {
            let manifest = Manifest {
                seed: SEED,
                factor,
                distinct: DISTINCT,
                inflated_bytes,
                xml_sha256: xml_digest.finish(),
                v2_sha256: v2_digest.finish(),
            };
            let file = fs::File::create(root.join("manifest.json")).unwrap();
            serde_json::to_writer_pretty(file, &manifest).unwrap();
            return manifest;
        }
        factor *= 2;
    }
}

struct Fixture {
    files: BTreeMap<String, Vec<u8>>,
    objects: BTreeMap<String, (Vec<u8>, Vec<u8>)>,
    catalog: Catalog,
}
impl Fixture {
    fn open(root: &Path, manifest: &Manifest) -> Self {
        assert_eq!(manifest.seed, SEED);
        assert_eq!(manifest.distinct, DISTINCT);
        assert!(manifest.inflated_bytes >= MIN_INFLATED);
        let mut files = BTreeMap::new();
        let mut objects = BTreeMap::new();
        let mut catalog = BTreeMap::new();
        let mut xml_digest = HashWriter::new();
        let mut v2_digest = HashWriter::new();
        for index in 0..DISTINCT {
            let name = name(index);
            let key = ObjectKey(format!("Catalog.{name}"));
            let path = format!("Catalogs/{name}.xml");
            let xml = fs::read(root.join(format!("{name}.xml"))).unwrap();
            let payload = fs::read(root.join(format!("{name}.v2"))).unwrap();
            xml_digest.record(&xml);
            v2_digest.record(&payload);
            let hash = Sha256::digest(&xml).to_vec();
            files.insert(path.clone(), xml);
            objects.insert(key.0.clone(), (hash, payload));
            catalog.insert(
                key.clone(),
                MetadataObject {
                    key,
                    descriptor: path,
                    uuid: format!("00000000-0000-4000-8000-{index:012}"),
                },
            );
        }
        assert_eq!(xml_digest.finish(), manifest.xml_sha256);
        assert_eq!(v2_digest.finish(), manifest.v2_sha256);
        Self {
            files,
            objects,
            catalog: Catalog {
                name: "Fixture".into(),
                uuid: "00000000-0000-4000-8000-000000000000".into(),
                objects: catalog,
            },
        }
    }
}
impl MetadataSource for Fixture {
    fn identity(&self) -> &str {
        IDENTITY
    }
    fn catalog(&self) -> &Catalog {
        &self.catalog
    }
    fn read(&self, path: &str, cancel: &Cancellation) -> Result<Vec<u8>> {
        cancel.check()?;
        self.files
            .get(path)
            .cloned()
            .ok_or_else(|| Error::Invalid(format!("Missing fixture descriptor: {path}")))
    }
    fn files_below(&self, _: &str, _: &Cancellation) -> Result<Vec<String>> {
        unreachable!("inventory is outside the SourceFacts benchmark")
    }
    fn exists(&self, path: &str, cancel: &Cancellation) -> Result<bool> {
        cancel.check()?;
        Ok(self.files.contains_key(path))
    }
    fn list_directory(&self, _: &str, _: &Cancellation) -> Result<Vec<SourceEntry>> {
        unreachable!("inventory is outside the SourceFacts benchmark")
    }
    fn snapshot(&self, cancel: &Cancellation) -> Result<String> {
        cancel.check()?;
        Ok(SNAPSHOT.into())
    }
    fn is_pinned(&self) -> bool {
        true
    }
}
impl FactStore for Fixture {
    fn source_facts(&self, key: &str, content_hash: Option<&[u8]>) -> Result<Option<SourceFacts>> {
        profile::measure("facts.lookup", "engine", 0, Some(key), || {
            let (hash, payload) = &self.objects[key];
            assert!(content_hash.is_none_or(|received| received == hash));
            facts_cache::decode(payload, key, hash, &FactVersions::current()).map(Some)
        })
    }
    fn lookup(&self, _: &str, _: Option<&[u8]>) -> Result<Option<Facts>> {
        unreachable!("legacy facts are outside the SourceFacts benchmark")
    }
    fn registrar_snapshot<'a>(
        &'a self,
        _: Option<&BTreeMap<String, Vec<u8>>>,
    ) -> Result<Option<Box<dyn cf_application::RegistrarLookup + 'a>>> {
        unreachable!("registrars are outside the SourceFacts benchmark")
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
    // The pseudo-handle needs no close. The counter buffer is valid for the documented size.
    let ok = unsafe { K32GetProcessMemoryInfo(GetCurrentProcess(), &mut counters, size) };
    (ok != 0).then_some(counters.peak_working_set)
}

#[cfg(not(windows))]
fn peak_working_set() -> Option<usize> {
    None
}

fn sample(root: &Path, report_path: &Path) {
    let manifest: Manifest =
        serde_json::from_reader(fs::File::open(root.join("manifest.json")).unwrap()).unwrap();
    let fixture = Fixture::open(root, &manifest);
    let cancel = Cancellation::default();
    let selected = BTreeSet::from([format!("Catalog.{}", name(0))]);
    let context = profile::Context::new("source-facts-benchmark", true, None);
    let started = Instant::now();
    let session = {
        let _scope = context.enter();
        let mut session = Session::new(
            &fixture,
            Some(&fixture),
            &selected,
            &BTreeSet::new(),
            &cancel,
        )
        .unwrap();
        // The permutation distributes both structural routes across the three XML shapes.
        for position in 0..DISTINCT {
            let key = format!("Catalog.{}", name(position * 293 % DISTINCT));
            if (REPEATS..STRUCTURAL).contains(&position) {
                session.facts(&key).unwrap();
            }
            session.document(&key).unwrap();
            if position < REPEATS {
                session.facts(&key).unwrap();
            }
        }
        session
    };
    let pipeline_ns = started.elapsed().as_nanos() as u64;
    let peak_working_set_bytes = peak_working_set();
    let report = context.report("ok").unwrap();
    assert_eq!(session.source_documents.len(), DISTINCT);
    assert_eq!(session.facts.len(), STRUCTURAL);
    assert_eq!(
        report.metrics["facts.decode"].totals.calls,
        (DISTINCT + REPEATS) as u64
    );
    assert_eq!(report.metrics["facts.lookup"].unique, DISTINCT);

    let encode_started = Instant::now();
    let mut encoded_bytes = 0;
    for document in session.source_documents.values() {
        encoded_bytes += facts_cache::encode(&document.facts).unwrap().len();
    }
    let encode_ns = encode_started.elapsed().as_nanos() as u64;

    let mut digest = HashWriter::new();
    digest.value(&session.facts);
    for (key, document) in &session.source_documents {
        digest.value(&(key, document.object, &document.node_ids, &document.facts));
        for id in 0..document.node_ids.len() {
            digest.value(document.targets(id));
        }
    }
    let receipt = serde_json::json!({
        "fixture": manifest,
        "pipeline_ns": pipeline_ns,
        "peak_working_set_bytes": peak_working_set_bytes,
        "encode_ns": encode_ns,
        "encoded_bytes": encoded_bytes,
        "semantic_sha256": digest.finish(),
        "profile": report,
    });
    serde_json::to_writer_pretty(fs::File::create(report_path).unwrap(), &receipt).unwrap();
}

#[test]
#[ignore = "requires the frozen offline SourceFacts workload"]
fn frozen_source_facts_pipeline() {
    let root = env::var_os("CF_FACTS_BENCH_DIR").unwrap();
    if env::var_os("CF_FACTS_BENCH_GENERATE").is_some() {
        let manifest = generate(Path::new(&root));
        println!("{}", serde_json::to_string(&manifest).unwrap());
    } else {
        let receipt = env::var_os("CF_FACTS_BENCH_RECEIPT").unwrap();
        sample(Path::new(&root), Path::new(&receipt));
    }
}
