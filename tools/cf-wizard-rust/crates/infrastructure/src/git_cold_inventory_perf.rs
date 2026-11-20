// Identical test-only transport in the diagnostics.2 control and candidate.
use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

#[derive(Deserialize)]
pub(crate) struct FixtureFile {
    pub oid: String,
    pub sha256: String,
    pub base64: String,
}

#[derive(Deserialize)]
pub(crate) struct Corpus {
    pub version: u32,
    pub revision: String,
    pub files: BTreeMap<String, FixtureFile>,
    pub owner_files: BTreeMap<String, Vec<String>>,
    pub responses: BTreeMap<String, String>,
}

#[derive(Clone, Serialize)]
pub(crate) struct WireCall {
    pub phase: String,
    pub route: &'static str,
    pub address: String,
    pub recursive: bool,
    pub page: Option<u64>,
    pub bytes: usize,
    pub start_ns: u64,
    pub elapsed_ns: u64,
    pub in_flight: usize,
    pub accepted: bool,
}

pub(crate) struct Transport {
    pub corpus: Corpus,
    file_bytes: BTreeMap<String, Vec<u8>>,
    epoch: Instant,
    delay: Duration,
    phase: Mutex<String>,
    calls: Mutex<Vec<WireCall>>,
    active: AtomicUsize,
    maximum: AtomicUsize,
}

struct Active<'a>(&'a AtomicUsize);

impl Drop for Active<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

impl Transport {
    pub(crate) fn new(corpus: Corpus, delay_ms: u64) -> Result<Arc<Self>> {
        if corpus.version != 1 {
            return Err(invalid("Unsupported frozen fixture version"));
        }
        let file_bytes = corpus
            .files
            .iter()
            .map(|(path, file)| {
                let bytes = STANDARD.decode(&file.base64).map_err(crate::external)?;
                verify_blob(&bytes, &file.oid, bytes.len() as u64)?;
                if cf_domain::canonical::sha256(&bytes) != file.sha256 {
                    return Err(invalid("Frozen payload SHA-256 mismatch"));
                }
                Ok((format!("raw/{}", quote(path)), bytes))
            })
            .collect::<Result<_>>()?;
        Ok(Arc::new(Self {
            corpus,
            file_bytes,
            epoch: Instant::now(),
            delay: Duration::from_millis(delay_ms),
            phase: Mutex::new(String::new()),
            calls: Mutex::new(Vec::new()),
            active: AtomicUsize::new(0),
            maximum: AtomicUsize::new(0),
        }))
    }

    pub(crate) fn provider(self: &Arc<Self>) -> Arc<dyn GitProvider> {
        Arc::new(Gitea {
            http: self.clone(),
            trees: Mutex::new(BTreeMap::new()),
        })
    }

    pub(crate) fn phase(&self, phase: &str) {
        *self.phase.lock().unwrap() = phase.into();
    }

    pub(crate) fn now(&self) -> u64 {
        self.epoch.elapsed().as_nanos().try_into().unwrap()
    }

    pub(crate) fn calls(&self) -> Vec<WireCall> {
        self.calls.lock().unwrap().clone()
    }

    pub(crate) fn in_flight(&self) -> (usize, usize) {
        (
            self.maximum.load(Ordering::SeqCst),
            self.active.load(Ordering::SeqCst),
        )
    }

    fn fixture_response(
        &self,
        path: &str,
        query: &BTreeMap<&str, &str>,
        raw: bool,
    ) -> Result<Vec<u8>> {
        if raw {
            if query.get("ref") != Some(&self.corpus.revision.as_str()) || query.len() != 1 {
                return Err(invalid("Unexpected synthetic raw file ref"));
            }
            return self
                .file_bytes
                .get(path)
                .cloned()
                .ok_or_else(|| invalid("Unknown synthetic raw file"));
        }
        if path == "file-contents" {
            if query.get("ref") != Some(&self.corpus.revision.as_str()) || query.len() != 2 {
                return Err(invalid("Unexpected synthetic batch ref"));
            }
            #[derive(Deserialize)]
            struct Batch {
                files: Vec<String>,
            }
            let batch: Batch = serde_json::from_str(query.get("body").ok_or_else(malformed)?)
                .map_err(crate::external)?;
            let rows = batch
                .files
                .iter()
                .map(|path| {
                    let file = self.corpus.files.get(path).ok_or_else(malformed)?;
                    Ok(json!({"path": path, "encoding": "base64", "content": file.base64}))
                })
                .collect::<Result<Vec<_>>>()?;
            return serde_json::to_vec(&rows).map_err(crate::external);
        }
        let key = if query.is_empty() {
            path.into()
        } else {
            let query = query
                .iter()
                .map(|(name, value)| format!("{name}={value}"))
                .collect::<Vec<_>>()
                .join("&");
            format!("{path}?{query}")
        };
        self.corpus
            .responses
            .get(&key)
            .map(|body| body.as_bytes().to_vec())
            .ok_or_else(|| invalid(format!("Unexpected synthetic request: {key}")))
    }
}

impl Http for Transport {
    fn bytes(
        &self,
        path: &str,
        query: &[(String, String)],
        body: Option<Value>,
        limit: usize,
        raw: bool,
        cancel: &Cancellation,
    ) -> Result<Vec<u8>> {
        cancel.check()?;
        let route = if path.starts_with("git/trees/") {
            "tree"
        } else if path.starts_with("git/refs/heads/") {
            "head"
        } else if path == "file-contents" {
            "batch"
        } else {
            "file"
        };
        let query_count = query.len();
        let query: BTreeMap<_, _> = query
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        let recursive = query.get("recursive") == Some(&"true");
        let page = query.get("page").and_then(|value| value.parse().ok());
        let start_ns = self.now();
        let in_flight = self.active.fetch_add(1, Ordering::SeqCst) + 1;
        self.maximum.fetch_max(in_flight, Ordering::SeqCst);
        let _active = Active(&self.active);
        let phase = self.phase.lock().unwrap().clone();
        let response = (|| {
            if body.is_some() || query.len() != query_count || raw != (route == "file") {
                return Err(invalid("Unexpected synthetic HTTP request shape"));
            }
            if route == "tree" && !self.delay.is_zero() {
                std::thread::sleep(self.delay);
            }
            cancel.check()?;
            let bytes = self.fixture_response(path, &query, raw)?;
            if bytes.len() > limit {
                return Err(invalid("Synthetic response exceeds transport limit"));
            }
            Ok(bytes)
        })();
        self.calls.lock().unwrap().push(WireCall {
            phase,
            route,
            address: path.into(),
            recursive,
            page,
            bytes: response.as_ref().map_or(0, Vec::len),
            start_ns,
            elapsed_ns: self.now() - start_ns,
            in_flight,
            accepted: response.is_ok(),
        });
        response
    }
}
