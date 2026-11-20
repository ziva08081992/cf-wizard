//! Diagnostic journal adapter. Errors here never replace a use-case result.
use cf_application::{
    MetadataSource,
    profile::{self, Context},
};
use cf_domain::Result;
use cf_infrastructure::{
    indexing::{self, IndexStatus},
    journal::OperationLog,
};
use serde_json::{Value, json};
use std::{collections::BTreeSet, path::Path, time::Instant};

pub const BUILD_ID: &str = match option_env!("CF_WIZARD_BUILD_ID") {
    Some(v) => v,
    None => "unconfigured",
};
const SUMMARY_CAP: usize = profile::OUTPUT_CAP - 4096;

/// Three fixed records only: start, bounded summary, end. The 4 KiB reserve
/// includes their JSONL envelopes and all identity fields, bounded at their source.
fn bounded_summary(mut value: Value) -> (Value, usize) {
    let mut omitted = 0;
    loop {
        let size = serde_json::to_vec(&value).map_or(usize::MAX, |v| v.len());
        if size <= SUMMARY_CAP {
            return (value, size);
        }
        let mut removed = false;
        for field in ["metrics", "source_delta"] {
            if let Some(rows) = value.get_mut(field).and_then(Value::as_object_mut)
                && let Some(key) = rows.keys().next_back().cloned()
            {
                rows.remove(&key);
                removed = true;
                break;
            }
        }
        value["truncated"] = json!(true);
        value["truncation_reason"] = json!("output_budget");
        omitted += 1;
        value["omitted_metric_rows"] = json!(omitted);
        if !removed {
            let fallback = json!({"schema":1,"operation_id":value["operation_id"],"operation":value["operation"],"status":value["status"],"truncated":true,"truncation_reason":"output_budget","summary_available":false});
            let size = serde_json::to_vec(&fallback).map_or(0, |v| v.len());
            return (fallback, size);
        }
    }
}
fn summary(context: &Context, status: &'static str, start_write_failed: bool) -> (Value, usize) {
    let Some(report) = context.report(status) else {
        return bounded_summary(
            json!({"schema":1,"operation_id":context.operation_id(),"status":status,"summary_available":false,"diagnostic_error":"collector_unavailable"}),
        );
    };
    let mut value=serde_json::to_value(report).unwrap_or_else(|_|json!({"schema":1,"status":status,"summary_available":false,"diagnostic_error":"serialization_failed"}));
    let metric = |name: &str| value["metrics"][name]["inclusive_ns"].as_u64().unwrap_or(0);
    let accounted = [
        "engine.setup",
        "engine.selected_scan",
        "engine.resolve",
        "engine.finish_inventory",
        "engine.result_prepare",
        "engine.package_compile",
    ]
    .into_iter()
    .fold(0u64, |total, name| total.saturating_add(metric(name)));
    let engine = metric("engine.total");
    if engine > 0 {
        value["engine_unattributed_ns"] = json!(engine.saturating_sub(accounted));
        value["engine_partition_ns"] = json!(accounted);
    }
    value["fingerprints_available"] = json!({"input":value["identities"].get("input_fingerprint").is_some(),"index":value["identities"].get("index_fingerprint").is_some(),"selection":value["identities"].get("selection_fingerprint").is_some()});
    value["sha256_coverage"] =
        json!("engine_source_reader_content_only; manifest_snapshot_package_have_enclosing_spans");
    value["wall_boundary"] = json!("before_summary_serialization_and_write");
    value["start_write_failed"] = json!(start_write_failed);
    if value["truncated"] == true {
        value["truncation_reason"] = json!("collector_limit");
    }
    bounded_summary(value)
}
trait DiagnosticWriter {
    fn value(&self, event: &str, fields: Value) -> Result<()>;
}
impl DiagnosticWriter for OperationLog {
    fn value(&self, event: &str, fields: Value) -> Result<()> {
        OperationLog::profile_value(self, event, fields)
    }
}
struct Completion<'a> {
    context: Context,
    log: Option<&'a dyn DiagnosticWriter>,
    finished: bool,
    start_write_failed: bool,
}
impl Completion<'_> {
    fn finish(&mut self, status: &'static str) {
        if self.finished {
            return;
        }
        self.finished = true;
        if let Some(log) = self.log {
            let started = Instant::now();
            let (value, summary_bytes) = summary(&self.context, status, self.start_write_failed);
            let written = log.value("profile-summary", value).is_ok();
            let _=log.value("profile-end",json!({"schema":1,"operation_id":self.context.operation_id(),"status":status,"summary_write_success":written,"summary_bytes":summary_bytes,"summary_prepare_and_write_ns":started.elapsed().as_nanos().min(u128::from(u64::MAX)) as u64}));
        }
    }
}
impl Drop for Completion<'_> {
    fn drop(&mut self) {
        self.finish("interrupted");
    }
}

pub fn run<T>(
    kind: &'static str,
    source: &dyn MetadataSource,
    selection: Option<(&BTreeSet<String>, &BTreeSet<String>)>,
    log: Option<&OperationLog>,
    f: impl FnOnce() -> Result<T>,
) -> Result<T> {
    if !Context::enabled() {
        return f();
    }
    run_enabled(
        kind,
        source,
        selection,
        log.map(|v| v as &dyn DiagnosticWriter),
        f,
    )
}
fn run_enabled<T>(
    kind: &'static str,
    source: &dyn MetadataSource,
    selection: Option<(&BTreeSet<String>, &BTreeSet<String>)>,
    log: Option<&dyn DiagnosticWriter>,
    f: impl FnOnce() -> Result<T>,
) -> Result<T> {
    let context = Context::new(kind, true, source.profile_counters());
    let mut completion = Completion {
        context,
        log,
        finished: false,
        start_write_failed: false,
    };
    let _scope = completion.context.enter();
    profile::identity(
        "source_mode",
        if source.is_pinned() { "Git" } else { "Local" },
    );
    if let Some(manifest) = source.manifest() {
        profile::identity("pinned_revision", &manifest.revision);
    }
    if let Some((selected, excluded)) = selection {
        if let Ok(bytes) = serde_json::to_vec(&(selected, excluded)) {
            profile::identity(
                "selection_fingerprint",
                &cf_domain::canonical::sha256(&bytes),
            );
        }
        profile::note("input.selected_count", "operation", selected.len());
        profile::note("input.excluded_count", "operation", excluded.len());
    }
    if let Some(log) = log {
        completion.start_write_failed=log.value("profile-start",json!({"schema":1,"operation_id":completion.context.operation_id(),"operation":kind,"version":env!("CARGO_PKG_VERSION"),"build_id":BUILD_ID,"pid":std::process::id(),"time_unit":"ns","bytes_unit":"logical_payload","inclusive_sums_are_not_elapsed":true})).is_err();
    }
    let result = f();
    let status = match &result {
        Ok(_) => "ok",
        Err(cf_domain::Error::Cancelled) => "cancelled",
        Err(cf_domain::Error::Invalid(_)) => "invalid",
        Err(cf_domain::Error::External(_)) => "external_error",
    };
    completion.finish(status);
    result
}
fn observed_status(source: &dyn MetadataSource, directory: &Path) -> Result<IndexStatus> {
    let result = indexing::status(source, directory);
    if let Ok(status) = &result {
        profile::identity("index_ready", if status.ready { "true" } else { "false" });
        profile::identity("index_found", if status.found { "true" } else { "false" });
    } else {
        profile::identity("index_ready", "unknown");
    }
    result
}
pub fn status(
    source: &dyn MetadataSource,
    directory: &Path,
    log: Option<&OperationLog>,
) -> Result<IndexStatus> {
    run("readiness", source, None, log, || {
        observed_status(source, directory)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use cf_application::Cancellation;
    use cf_domain::{Catalog, Error};
    struct NoSource(Catalog);
    impl MetadataSource for NoSource {
        fn identity(&self) -> &str {
            "private-source"
        }
        fn catalog(&self) -> &Catalog {
            &self.0
        }
        fn read(&self, _: &str, _: &Cancellation) -> Result<Vec<u8>> {
            panic!("diagnostics must not read source")
        }
        fn files_below(&self, _: &str, _: &Cancellation) -> Result<Vec<String>> {
            panic!("no extra inventory")
        }
        fn exists(&self, _: &str, _: &Cancellation) -> Result<bool> {
            panic!("no extra exists")
        }
        fn list_directory(
            &self,
            _: &str,
            _: &Cancellation,
        ) -> Result<Vec<cf_application::SourceEntry>> {
            panic!("no extra listing")
        }
        fn snapshot(&self, _: &Cancellation) -> Result<String> {
            panic!("no extra proof")
        }
    }
    #[test]
    fn output_budget_preserves_terminal_identity() {
        let mut metrics = serde_json::Map::new();
        for n in 0..96 {
            metrics.insert(format!("stage-{n}"), json!({"value":"x".repeat(2000)}));
        }
        let (value, encoded_size) = bounded_summary(
            json!({"schema":1,"operation_id":42,"operation":"preview","status":"invalid","metrics":metrics,"source_delta":{}}),
        );
        assert_eq!(serde_json::to_vec(&value).unwrap().len(), encoded_size);
        assert!(encoded_size <= SUMMARY_CAP);
        assert_eq!(value["operation_id"], 42);
        assert_eq!(value["truncation_reason"], "output_budget");
    }
    #[test]
    fn diagnostics_preserve_errors_and_do_no_source_io() {
        // Existing fixture catalog construction is independent of source I/O.
        let catalog: Catalog = Catalog {
            name: "fixture".into(),
            uuid: String::new(),
            objects: Default::default(),
        };
        let source = NoSource(catalog);
        let folder = tempfile::tempdir().unwrap();
        let log = OperationLog::create(folder.path(), "test").unwrap();
        let result: Result<()> = run_enabled("preview", &source, None, Some(&log), || {
            Err(Error::Invalid("primary".into()))
        });
        assert!(matches!(result,Err(Error::Invalid(v)) if v=="primary"));
        assert!(!Context::current().is_enabled());
    }
    #[test]
    fn failed_diagnostic_writer_keeps_primary_error_and_finishes() {
        struct Failing(std::sync::atomic::AtomicUsize);
        impl DiagnosticWriter for Failing {
            fn value(&self, _: &str, _: Value) -> Result<()> {
                self.0.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                Err(Error::External("writer unavailable".into()))
            }
        }
        let source = NoSource(Catalog {
            name: "fixture".into(),
            uuid: String::new(),
            objects: Default::default(),
        });
        let log = Failing(std::sync::atomic::AtomicUsize::new(0));
        let result: Result<()> = run_enabled("preview", &source, None, Some(&log), || {
            Err(Error::Invalid("primary".into()))
        });
        assert!(matches!(result,Err(Error::Invalid(v)) if v=="primary"));
        assert_eq!(log.0.load(std::sync::atomic::Ordering::Relaxed), 3);
        assert!(!Context::current().is_enabled());
    }
    #[test]
    fn unreadable_candidate_records_not_ready_and_failed_candidate_open() {
        struct Recorded(std::sync::Mutex<Vec<(String, Value)>>);
        impl DiagnosticWriter for Recorded {
            fn value(&self, event: &str, value: Value) -> Result<()> {
                self.0.lock().unwrap().push((event.into(), value));
                Ok(())
            }
        }
        let source = NoSource(Catalog {
            name: "fixture".into(),
            uuid: String::new(),
            objects: Default::default(),
        });
        let directory = tempfile::tempdir().unwrap();
        let log = Recorded(std::sync::Mutex::new(Vec::new()));
        let candidate = indexing::path_for(&source, directory.path()).unwrap();
        std::fs::write(candidate, b"not a sqlite database").unwrap();
        let status = run_enabled("readiness", &source, None, Some(&log), || {
            observed_status(&source, directory.path())
        })
        .unwrap();
        assert!(!status.ready);
        let rows = log.0.lock().unwrap();
        let summary = &rows
            .iter()
            .find(|(event, _)| event == "profile-summary")
            .unwrap()
            .1;
        assert_eq!(summary["identities"]["index_ready"], "false");
        assert_eq!(summary["status"], "ok");
        assert!(
            summary["metrics"]["index.candidate_open"]["errors"]
                .as_u64()
                .unwrap()
                > 0
        );
        assert!(
            !summary["fingerprints_available"]["index"]
                .as_bool()
                .unwrap()
        );
        assert!(summary["metrics"].get("index.open").is_none());
    }
}
