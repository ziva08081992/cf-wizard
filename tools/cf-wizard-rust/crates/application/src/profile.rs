//! Bounded, opt-in observation. It never controls the observed operation.
use serde::Serialize;
use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet},
    marker::PhantomData,
    rc::Rc,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Instant,
};

pub const UNIQUE_CAP: usize = 32_768;
pub const METRIC_CAP: usize = 128;
pub const PARENT_CAP: usize = 16;
pub const OUTPUT_CAP: usize = 64 * 1024;
static NEXT_ID: AtomicU64 = AtomicU64::new(1);
thread_local! { static CURRENT: RefCell<Context> = RefCell::new(Context::default()); static PARENTS: RefCell<Vec<(u64, &'static str)>> = const { RefCell::new(Vec::new()) }; }

#[derive(Clone, Debug, Default, Serialize, PartialEq)]
pub struct Totals {
    pub calls: u64,
    pub errors: u64,
    pub items: u64,
    /// Logical payload bytes, never physical disk or wire bytes.
    pub bytes: u64,
    /// Inclusive sum. Parallel or nested rows MUST NOT be added to wall time.
    pub inclusive_ns: u64,
}
fn add(a: &mut u64, b: u64, overflow: &mut bool) {
    *overflow |= a.checked_add(b).is_none();
    *a = a.saturating_add(b);
}
impl Totals {
    fn record(&mut self, ns: u64, bytes: u64, ok: bool, overflow: &mut bool) {
        add(&mut self.calls, 1, overflow);
        add(&mut self.errors, u64::from(!ok), overflow);
        add(&mut self.bytes, bytes, overflow);
        add(&mut self.inclusive_ns, ns, overflow);
    }
    fn since(&self, old: &Self) -> Self {
        Self {
            calls: self.calls.saturating_sub(old.calls),
            errors: self.errors.saturating_sub(old.errors),
            items: self.items.saturating_sub(old.items),
            bytes: self.bytes.saturating_sub(old.bytes),
            inclusive_ns: self.inclusive_ns.saturating_sub(old.inclusive_ns),
        }
    }
}
#[derive(Clone, Debug, Serialize)]
pub struct Metric {
    pub parent: &'static str,
    pub group: &'static str,
    pub parents: BTreeSet<&'static str>,
    pub parents_complete: bool,
    pub cpu: Option<Cpu>,
    pub cpu_samples: u64,
    pub inclusive: bool,
    #[serde(flatten)]
    pub totals: Totals,
    pub unique: usize,
    pub unique_lower_bound: bool,
}
struct Row {
    metric: Metric,
    keys: BTreeSet<String>,
}
#[derive(Default)]
struct State {
    rows: BTreeMap<&'static str, Row>,
    identities: BTreeMap<&'static str, String>,
    truncated: bool,
    overflow: bool,
    in_flight: u64,
    maximum: u64,
    source_started: u64,
    source_active: u64,
}
#[derive(Default)]
struct SourceData {
    rows: SourceSnapshot,
    started: u64,
    active: u64,
    overflow: bool,
    truncated: bool,
}
#[derive(Default)]
struct SourceCapture {
    rows: SourceSnapshot,
    started: u64,
    active: u64,
    overflow: bool,
    truncated: bool,
    available: bool,
}
#[derive(Clone, Default)]
pub struct SourceCounters(Option<Arc<Mutex<SourceData>>>);
pub type SourceSnapshot = BTreeMap<&'static str, Totals>;
impl SourceCounters {
    pub fn new(enabled: bool) -> Self {
        Self(enabled.then(|| Arc::new(Mutex::new(SourceData::default()))))
    }
    pub fn available(&self) -> bool {
        self.0.as_ref().is_some_and(|v| v.lock().is_ok())
    }
    pub fn snapshot(&self) -> SourceSnapshot {
        self.capture().rows
    }
    fn capture(&self) -> SourceCapture {
        self.0
            .as_ref()
            .and_then(|v| {
                v.lock().ok().map(|v| SourceCapture {
                    rows: v.rows.clone(),
                    started: v.started,
                    active: v.active,
                    overflow: v.overflow,
                    truncated: v.truncated,
                    available: true,
                })
            })
            .unwrap_or_default()
    }
    fn same(&self, other: &Self) -> bool {
        self.0
            .as_ref()
            .zip(other.0.as_ref())
            .is_some_and(|(a, b)| Arc::ptr_eq(a, b))
    }
    fn begin(&self) -> bool {
        let Some(mut data) = self.0.as_ref().and_then(|v| v.lock().ok()) else {
            return false;
        };
        let mut overflow = false;
        add(&mut data.started, 1, &mut overflow);
        add(&mut data.active, 1, &mut overflow);
        data.overflow |= overflow;
        true
    }
    fn end(&self) {
        if let Some(mut data) = self.0.as_ref().and_then(|v| v.lock().ok()) {
            data.active = data.active.saturating_sub(1);
        }
    }
    #[cfg(test)]
    fn record(&self, name: &'static str, ns: u64, bytes: u64, ok: bool) {
        self.record_items(name, ns, bytes, 0, ok);
    }
    fn record_items(&self, name: &'static str, ns: u64, bytes: u64, items: u64, ok: bool) {
        if let Some(mut data) = self.0.as_ref().and_then(|v| v.lock().ok()) {
            if data.rows.len() < METRIC_CAP || data.rows.contains_key(name) {
                let mut overflow = false;
                let row = data.rows.entry(name).or_default();
                row.record(ns, bytes, ok, &mut overflow);
                add(&mut row.items, items, &mut overflow);
                data.overflow |= overflow;
            } else {
                data.truncated = true;
            }
        }
    }
}
#[derive(Clone, Copy, Debug, Serialize)]
pub struct Cpu {
    pub user_ns: u64,
    pub kernel_ns: u64,
}
#[cfg(windows)]
fn cpu() -> Option<Cpu> {
    #[repr(C)]
    #[derive(Default)]
    struct FileTime {
        low: u32,
        high: u32,
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
    let (mut created, mut exited, mut kernel, mut user) = (
        FileTime::default(),
        FileTime::default(),
        FileTime::default(),
        FileTime::default(),
    );
    // Microsoft processthreadsapi.h: FILETIME is two DWORDs; durations use 100 ns.
    // All out-pointers are valid/aligned; the pseudo handle must not be closed.
    let ok = unsafe {
        GetProcessTimes(
            GetCurrentProcess(),
            &mut created,
            &mut exited,
            &mut kernel,
            &mut user,
        )
    };
    let ns = |t: FileTime| ((u64::from(t.high) << 32) | u64::from(t.low)).saturating_mul(100);
    (ok != 0).then(|| Cpu {
        user_ns: ns(user),
        kernel_ns: ns(kernel),
    })
}
#[cfg(not(windows))]
fn cpu() -> Option<Cpu> {
    None
}
fn cpu_delta(start: Option<Cpu>, end: Option<Cpu>) -> Option<Cpu> {
    start.zip(end).map(|(a, b)| Cpu {
        user_ns: b.user_ns.saturating_sub(a.user_ns),
        kernel_ns: b.kernel_ns.saturating_sub(a.kernel_ns),
    })
}
struct Run {
    id: u64,
    kind: &'static str,
    start: Instant,
    cpu: Option<Cpu>,
    state: Mutex<State>,
    source: Option<SourceCounters>,
    before: SourceCapture,
}
#[derive(Clone, Default)]
pub struct Context(Option<Arc<Run>>, Option<&'static str>);
/// Thread-local lexical scope, deliberately !Send. Worker contexts are explicit clones.
pub struct Scope {
    previous: Context,
    previous_stack: Vec<(u64, &'static str)>,
    _thread: PhantomData<Rc<()>>,
}
impl Drop for Scope {
    fn drop(&mut self) {
        CURRENT.with(|v| *v.borrow_mut() = self.previous.clone());
        PARENTS.with(|v| *v.borrow_mut() = std::mem::take(&mut self.previous_stack));
    }
}
impl Context {
    pub fn enabled() -> bool {
        std::env::var_os("CF_WIZARD_PROFILE").is_some_and(|v| v == "1")
    }
    pub fn current() -> Self {
        {
            let mut context = CURRENT.with(|v| v.borrow().clone());
            context.1 = PARENTS.with(|v| v.borrow().last().map(|(_, name)| *name));
            context
        }
    }
    pub fn new(kind: &'static str, enabled: bool, source: Option<SourceCounters>) -> Self {
        if !enabled {
            return Self::default();
        }
        let before = source
            .as_ref()
            .map(SourceCounters::capture)
            .unwrap_or_default();
        Self(
            Some(Arc::new(Run {
                id: NEXT_ID.fetch_add(1, Ordering::Relaxed),
                kind,
                start: Instant::now(),
                cpu: cpu(),
                state: Mutex::new(State::default()),
                source,
                before,
            })),
            None,
        )
    }
    pub fn enter(&self) -> Scope {
        let previous = CURRENT.with(|v| v.replace(self.clone()));
        let previous_stack =
            PARENTS.with(|v| v.replace(self.1.map(|name| vec![(0, name)]).unwrap_or_default()));
        Scope {
            previous,
            previous_stack,
            _thread: PhantomData,
        }
    }
    pub fn operation_id(&self) -> Option<u64> {
        self.0.as_ref().map(|v| v.id)
    }
    pub fn is_enabled(&self) -> bool {
        self.0.is_some()
    }
    pub fn report(&self, status: &'static str) -> Option<Report> {
        let run = self.0.as_ref()?;
        let state = run.state.lock().ok()?;
        let after = run
            .source
            .as_ref()
            .map(SourceCounters::capture)
            .unwrap_or_default();
        let source_delta: SourceSnapshot = after
            .rows
            .iter()
            .map(|(name, v)| {
                (
                    *name,
                    v.since(run.before.rows.get(name).unwrap_or(&Totals::default())),
                )
            })
            .filter(|(_, v)| v.calls > 0)
            .collect();
        // Unlike a mutable shared observer, lifetime counters are never reset.
        let source_delta_lower_bound =
            after.overflow || run.before.overflow || after.truncated || run.before.truncated;
        let shared_source_activity = source_delta_lower_bound
            || run.before.active > 0
            || after.active > state.source_active
            || after.started.saturating_sub(run.before.started) != state.source_started
            || source_delta.iter().any(|(name, v)| {
                state
                    .rows
                    .get(name)
                    .is_none_or(|row| row.metric.totals != *v)
            });
        let cpu = cpu_delta(run.cpu, cpu());
        Some(Report {
            schema: 1,
            operation_id: run.id,
            operation: run.kind,
            status,
            wall_ns: nanos(run.start),
            cpu,
            cpu_scope: "process_all_threads_excludes_child_processes",
            cpu_unavailable_reason: if cpu.is_none() {
                Some("unsupported_or_api_failed")
            } else {
                None
            },
            identities: state.identities.clone(),
            metrics: state
                .rows
                .iter()
                .map(|(k, v)| (*k, v.metric.clone()))
                .collect(),
            source_counters_available: run.before.available && after.available,
            source_delta_lower_bound,
            source_active_before: run.before.active,
            source_active_after: after.active,
            observed_source_active: state.source_active,
            source_delta,
            shared_source_activity,
            maximum_combined_in_flight: state.maximum,
            final_in_flight: state.in_flight,
            truncated: state.truncated || run.before.truncated || after.truncated,
            counter_overflow: state.overflow || run.before.overflow || after.overflow,
        })
    }
}
#[derive(Serialize)]
pub struct Report {
    pub schema: u32,
    pub operation_id: u64,
    pub operation: &'static str,
    pub status: &'static str,
    pub wall_ns: u64,
    pub cpu: Option<Cpu>,
    pub cpu_scope: &'static str,
    pub cpu_unavailable_reason: Option<&'static str>,
    pub identities: BTreeMap<&'static str, String>,
    pub metrics: BTreeMap<&'static str, Metric>,
    pub source_delta: SourceSnapshot,
    pub source_counters_available: bool,
    pub source_delta_lower_bound: bool,
    pub source_active_before: u64,
    pub source_active_after: u64,
    pub observed_source_active: u64,
    pub shared_source_activity: bool,
    pub maximum_combined_in_flight: u64,
    pub final_in_flight: u64,
    pub truncated: bool,
    pub counter_overflow: bool,
}
fn nanos(start: Instant) -> u64 {
    start.elapsed().as_nanos().min(u128::from(u64::MAX)) as u64
}
pub struct Span {
    context: Context,
    source: Option<SourceCounters>,
    name: &'static str,
    parent: &'static str,
    start: Option<Instant>,
    bytes: u64,
    items: u64,
    key: Option<String>,
    ok: bool,
    provider: bool,
    source_started: bool,
    source_owned: bool,
    group: &'static str,
    token: Option<u64>,
    cpu: Option<Cpu>,
    _thread: PhantomData<Rc<()>>,
}
impl Span {
    pub fn bytes(&mut self, bytes: usize) {
        self.bytes = bytes as u64;
    }
    pub fn success(&mut self) {
        self.ok = true;
    }
    pub fn items(&mut self, items: usize) {
        self.items = items as u64;
    }
}
impl Drop for Span {
    fn drop(&mut self) {
        let Some(start) = self.start else {
            return;
        };
        let ns = nanos(start);
        let cpu = self.cpu.and_then(|v| cpu_delta(Some(v), cpu()));
        if let Some(token) = self.token {
            PARENTS.with(|v| v.borrow_mut().retain(|(id, _)| *id != token));
        }
        if let Some(source) = &self.source {
            source.record_items(self.name, ns, self.bytes, self.items, self.ok);
            if self.source_started {
                source.end();
            }
        }
        let Some(run) = &self.context.0 else {
            return;
        };
        let Ok(mut state) = run.state.lock() else {
            return;
        };
        if self.provider {
            state.in_flight = state.in_flight.saturating_sub(1);
        }
        if self.source_owned {
            state.source_active = state.source_active.saturating_sub(1);
        }
        if !state.rows.contains_key(self.name)
            && (state.rows.len() >= METRIC_CAP || self.name.len() > 64 || self.parent.len() > 64)
        {
            state.truncated = true;
            return;
        }
        let mut overflow = false;
        let row = state.rows.entry(self.name).or_insert_with(|| Row {
            metric: Metric {
                parent: self.parent,
                group: self.group,
                parents: BTreeSet::new(),
                parents_complete: true,
                cpu: None,
                cpu_samples: 0,
                inclusive: true,
                totals: Totals::default(),
                unique: 0,
                unique_lower_bound: false,
            },
            keys: BTreeSet::new(),
        });
        if row.metric.parents.len() < PARENT_CAP || row.metric.parents.contains(self.parent) {
            row.metric.parents.insert(self.parent);
        } else {
            row.metric.parents_complete = false;
        }
        if row.metric.parents.len() > 1 {
            row.metric.parent = "multiple";
        }
        if let Some(delta) = cpu {
            let current = row.metric.cpu.get_or_insert(Cpu {
                user_ns: 0,
                kernel_ns: 0,
            });
            add(&mut current.user_ns, delta.user_ns, &mut overflow);
            add(&mut current.kernel_ns, delta.kernel_ns, &mut overflow);
            row.metric.cpu_samples = row.metric.cpu_samples.saturating_add(1);
        }
        row.metric
            .totals
            .record(ns, self.bytes, self.ok, &mut overflow);
        add(&mut row.metric.totals.items, self.items, &mut overflow);
        if let Some(key) = self.key.take() {
            if row.keys.len() < UNIQUE_CAP || row.keys.contains(&key) {
                row.keys.insert(key);
            } else {
                row.metric.unique_lower_bound = true;
            }
            row.metric.unique = row.keys.len();
        }
        state.truncated |= row.metric.unique_lower_bound || !row.metric.parents_complete;
        state.overflow |= overflow;
    }
}
pub fn source_span(
    source: Option<&SourceCounters>,
    name: &'static str,
    parent: &'static str,
    bytes: usize,
    key: Option<&str>,
) -> Span {
    let context = Context::current();
    let enabled = context.is_enabled() || source.is_some_and(|s| s.0.is_some());
    let group = parent;
    let token = context
        .is_enabled()
        .then(|| NEXT_ID.fetch_add(1, Ordering::Relaxed));
    let parent = PARENTS.with(|v| {
        let mut v = v.borrow_mut();
        let parent = v.last().map(|(_, name)| *name).unwrap_or("operation");
        if let Some(token) = token {
            v.push((token, name));
        }
        parent
    });
    let cpu = if context.is_enabled()
        && matches!(
            name,
            "index.open"
                | "index.readiness"
                | "engine.total"
                | "engine.setup"
                | "engine.selected_scan"
                | "engine.resolve"
                | "engine.finish_inventory"
                | "engine.result_prepare"
                | "engine.package_compile"
        ) {
        cpu()
    } else {
        None
    };
    let provider = name == "provider.tree" || name == "provider.file";
    let source_started = source.is_some_and(SourceCounters::begin);
    let source_owned = source_started
        && context
            .0
            .as_ref()
            .and_then(|run| run.source.as_ref())
            .is_some_and(|owned| source.is_some_and(|source| owned.same(source)));
    if source_owned
        && let Some(run) = &context.0
        && let Ok(mut state) = run.state.lock()
    {
        let mut overflow = false;
        add(&mut state.source_started, 1, &mut overflow);
        add(&mut state.source_active, 1, &mut overflow);
        state.overflow |= overflow;
    }

    if provider
        && let Some(run) = &context.0
        && let Ok(mut state) = run.state.lock()
    {
        state.in_flight = state.in_flight.saturating_add(1);
        state.maximum = state.maximum.max(state.in_flight);
    }
    Span {
        key: if context.is_enabled() {
            key.map(|v| cf_domain::canonical::sha256(v.as_bytes()))
        } else {
            None
        },
        context,
        source: source.cloned(),
        name,
        parent,
        start: enabled.then(Instant::now),
        bytes: bytes as u64,
        items: 0,
        ok: false,
        provider,
        source_started,
        source_owned,
        group,
        token,
        cpu,
        _thread: PhantomData,
    }
}
pub fn span(name: &'static str, parent: &'static str, bytes: usize, key: Option<&str>) -> Span {
    source_span(None, name, parent, bytes, key)
}
pub fn identity(name: &'static str, value: &str) {
    let context = Context::current();
    if let Some(run) = &context.0
        && let Ok(mut state) = run.state.lock()
    {
        if state.identities.len() < 16 && name.len() <= 64 && value.len() <= 128 {
            state.identities.insert(name, value.to_owned());
        } else {
            state.truncated = true;
        }
    }
}
pub fn note(name: &'static str, parent: &'static str, items: usize) {
    let mut s = span(name, parent, 0, None);
    s.items(items);
    s.success();
}
pub fn value<T>(
    name: &'static str,
    parent: &'static str,
    bytes: usize,
    key: Option<&str>,
    f: impl FnOnce() -> T,
) -> T {
    let mut timer = span(name, parent, bytes, key);
    let value = f();
    timer.success();
    value
}
pub fn measure<T, E>(
    name: &'static str,
    parent: &'static str,
    bytes: usize,
    key: Option<&str>,
    f: impl FnOnce() -> Result<T, E>,
) -> Result<T, E> {
    let mut timer = span(name, parent, bytes, key);
    let result = f();
    if result.is_ok() {
        timer.success();
    }
    result
}
pub fn source_measure<T, E>(
    source: Option<&SourceCounters>,
    name: &'static str,
    parent: &'static str,
    bytes: usize,
    key: Option<&str>,
    f: impl FnOnce() -> Result<T, E>,
) -> Result<T, E> {
    let mut timer = source_span(source, name, parent, bytes, key);
    let result = f();
    if result.is_ok() {
        timer.success();
    }
    result
}
pub fn measured_items<T, E>(
    source: Option<&SourceCounters>,
    name: &'static str,
    parent: &'static str,
    key: Option<&str>,
    f: impl FnOnce() -> Result<Vec<T>, E>,
) -> Result<Vec<T>, E> {
    let mut timer = source_span(source, name, parent, 0, key);
    let result = f();
    if let Ok(items) = &result {
        timer.items(items.len());
        timer.success();
    }
    result
}
pub fn measured_bytes<E>(
    source: Option<&SourceCounters>,
    name: &'static str,
    parent: &'static str,
    key: Option<&str>,
    f: impl FnOnce() -> Result<Vec<u8>, E>,
) -> Result<Vec<u8>, E> {
    let mut timer = source_span(source, name, parent, 0, key);
    let result = f();
    if let Ok(bytes) = &result {
        timer.bytes(bytes.len());
        timer.success();
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn scopes_are_lexical_and_off_is_empty() {
        let off = Context::new("test", false, None);
        let _guard = off.enter();
        let _: Result<(), ()> = measure("off", "root", 1, Some("private"), || Ok(()));
        assert!(off.report("ok").is_none());
        let on = Context::new("test", true, None);
        {
            let _scope = on.enter();
            let _: Result<(), ()> = measure("on", "root", 4, Some("private"), || Err(()));
        }
        let r = on.report("error").unwrap();
        assert_eq!(r.metrics["on"].totals.errors, 1);
        assert_eq!(r.metrics["on"].unique, 1);
        assert!(!Context::current().is_enabled());
    }
    #[test]
    fn repeated_keys_are_not_deduplicated_and_workers_are_joined() {
        let on = Context::new("test", true, None);
        let _scope = on.enter();
        for _ in 0..2 {
            let _: Result<(), ()> = measure("scan", "root", 4, Some("same"), || Ok(()));
        }
        std::thread::scope(|s| {
            let c = Context::current();
            s.spawn(move || {
                let _s = c.enter();
                let _: Result<(), ()> = measure("provider.tree", "root", 0, None, || Ok(()));
            });
        });
        let r = on.report("ok").unwrap();
        assert_eq!(r.metrics["scan"].totals.calls, 2);
        assert_eq!(r.metrics["scan"].unique, 1);
        assert_eq!(r.final_in_flight, 0);
        assert_eq!(r.maximum_combined_in_flight, 1);
    }
    #[test]
    fn counters_are_monotonic_and_external_activity_is_marked() {
        let counters = SourceCounters::new(true);
        counters.record("provider.tree", 1, 0, true);
        let c = Context::new("test", true, Some(counters.clone()));
        counters.record("provider.tree", 1, 0, true);
        let r = c.report("ok").unwrap();
        assert_eq!(r.source_delta["provider.tree"].calls, 1);
        assert!(r.shared_source_activity);
        assert_eq!(counters.snapshot()["provider.tree"].calls, 2);
    }
    #[test]
    fn bounded_unique_and_saturating_counters() {
        let c = Context::new("test", true, None);
        let _scope = c.enter();
        for n in 0..UNIQUE_CAP + 1 {
            let _: Result<(), ()> = measure("scan", "root", 0, Some(&n.to_string()), || Ok(()));
        }
        let r = c.report("ok").unwrap();
        assert!(r.truncated);
        assert_eq!(r.metrics["scan"].unique, UNIQUE_CAP);
        assert_eq!(r.metrics["scan"].totals.calls, UNIQUE_CAP as u64 + 1);
        let mut v = u64::MAX;
        let mut overflow = false;
        add(&mut v, 1, &mut overflow);
        assert_eq!(v, u64::MAX);
        assert!(overflow);
    }
    #[test]
    fn cpu_failure_is_optional() {
        assert!(
            cpu_delta(
                None,
                Some(Cpu {
                    user_ns: 0,
                    kernel_ns: 0
                })
            )
            .is_none()
        );
    }
    #[test]
    fn poisoned_observer_cannot_change_the_observed_result() {
        let context = Context::new("test", true, None);
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _lock = context.0.as_ref().unwrap().state.lock().unwrap();
            panic!("observer poisoned");
        }));
        {
            let _scope = context.enter();
            let result: Result<u32, ()> = measure("stage", "root", 0, None, || Ok(7));
            assert_eq!(result, Ok(7));
        }
        assert!(context.report("ok").is_none());
    }
    #[test]
    fn unfinished_external_source_activity_is_visible_at_both_boundaries() {
        let counters = SourceCounters::new(true);
        let other = counters.clone();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let (end_tx, end_rx) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            let mut span = source_span(Some(&other), "source.read", "source", 0, None);
            ready_tx.send(()).unwrap();
            end_rx.recv().unwrap();
            span.success();
        });
        ready_rx.recv().unwrap();
        let context = Context::new("test", true, Some(counters));
        let report = context.report("ok").unwrap();
        assert!(report.shared_source_activity);
        assert_eq!(report.source_active_before, 1);
        assert_eq!(report.source_active_after, 1);
        assert!(report.source_delta.is_empty());
        end_tx.send(()).unwrap();
        worker.join().unwrap();
    }
    #[test]
    fn owned_active_source_span_is_not_misclassified_as_external() {
        let counters = SourceCounters::new(true);
        let context = Context::new("test", true, Some(counters.clone()));
        let _scope = context.enter();
        let mut span = source_span(Some(&counters), "source.read", "source", 0, None);
        let report = context.report("running").unwrap();
        assert!(!report.shared_source_activity);
        assert_eq!(report.observed_source_active, 1);
        span.success();
        drop(span);
        assert_eq!(context.report("ok").unwrap().source_active_after, 0);
    }
    #[test]
    fn source_overflow_is_sticky_and_delta_is_marked_as_lower_bound() {
        let counters = SourceCounters::new(true);
        counters.0.as_ref().unwrap().lock().unwrap().rows.insert(
            "source.read",
            Totals {
                calls: u64::MAX,
                ..Totals::default()
            },
        );
        let context = Context::new("test", true, Some(counters.clone()));
        counters.record("source.read", 1, 1, true);
        let report = context.report("ok").unwrap();
        assert!(report.counter_overflow);
        assert!(report.source_delta_lower_bound);
        assert!(report.shared_source_activity);
        assert_eq!(counters.snapshot()["source.read"].calls, u64::MAX);
    }
}
