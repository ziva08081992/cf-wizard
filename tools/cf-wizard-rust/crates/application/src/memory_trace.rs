//! Optional memory-observation markers for a separately launched sampler.
use serde_json::json;
use std::{
    cell::Cell,
    fs::{File, OpenOptions},
    io::Write,
    sync::{Mutex, OnceLock},
    time::{SystemTime, UNIX_EPOCH},
};

thread_local! { static PREVIEW: Cell<u64> = const { Cell::new(0) }; }
static OUTPUT: OnceLock<Option<Mutex<File>>> = OnceLock::new();

fn output() -> Option<&'static Mutex<File>> {
    OUTPUT
        .get_or_init(|| {
            let path = std::env::var_os("CF_WIZARD_MEMORY_PHASE_FILE")?;
            let file = OpenOptions::new().append(true).open(path).ok()?;
            file.metadata().ok()?.is_file().then(|| Mutex::new(file))
        })
        .as_ref()
}

pub fn with_preview<T>(ordinal: u64, action: impl FnOnce() -> T) -> T {
    struct Reset(u64);
    impl Drop for Reset {
        fn drop(&mut self) {
            PREVIEW.set(self.0);
        }
    }
    let _reset = Reset(PREVIEW.replace(ordinal));
    action()
}

pub fn current(phase: &str) {
    let ordinal = PREVIEW.get();
    if ordinal != 0 {
        mark(ordinal, phase);
    }
}

pub fn mark(ordinal: u64, phase: &str) {
    let Some(file) = output() else { return };
    let Ok(utc) = SystemTime::now().duration_since(UNIX_EPOCH) else {
        return;
    };
    if let Ok(mut file) = file.lock() {
        let record = json!({
            "phase": phase,
            "pid": std::process::id(),
            "utc_ms": utc.as_millis() as u64,
            "preview_ordinal": ordinal,
        });
        let _ = writeln!(&mut *file, "{record}");
    }
}
