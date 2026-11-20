//! Explicit, isolated native acceptance mode, driven by tests/native_close.py.
use crate::model::Model;
use cf_domain::{Error, Result};
use serde_json::json;
use std::{
    fs,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

pub(crate) struct Probe {
    directory: PathBuf,
    preview_requested: bool,
    ready: bool,
}

impl Probe {
    pub(crate) fn new(directory: PathBuf) -> Self {
        Self {
            directory,
            preview_requested: false,
            ready: false,
        }
    }

    pub(crate) fn tick(&mut self, model: &mut Model) -> Result<()> {
        if self.ready || model.busy {
            return Ok(());
        }
        let Some(source) = &model.source else {
            return Ok(());
        };
        if !self.preview_requested {
            let key = source
                .catalog()
                .objects
                .keys()
                .find(|key| key.kind() == "CommonModule")
                .ok_or_else(|| Error::Invalid("Close fixture requires CommonModule".into()))?
                .0
                .clone();
            self.preview_requested = true;
            model.rpc("markObject", &json!([key, true]), None)?;
            model.rpc("command", &json!(["preview"]), None)?;
        } else if model.plan.is_some() {
            model.rpc(
                "editField",
                &json!(["destination", "close-smoke-destination"]),
                None,
            )?;
            fs::write(self.directory.join("close-ready"), b"retained-preview")
                .map_err(cf_infrastructure::external)?;
            self.ready = true;
        } else {
            return Err(Error::Invalid("Native close preview failed".into()));
        }
        Ok(())
    }
}

// This gate can only be enabled by the standalone test command line. The real
// message loop, ownership transfer, workers and WebView remain unchanged.
pub(crate) fn release_plan<T>(plan: T, gate: Option<&Path>) -> bool {
    struct Gate<'a>(&'a Path, &'a std::cell::Cell<bool>);
    impl Drop for Gate<'_> {
        fn drop(&mut self) {
            self.1.set(wait_for_release(self.0));
        }
    }
    let success = std::cell::Cell::new(true);
    // Tuple fields drop in declaration order. The test can hold this synthetic
    // destructor while the real prepared plan is still owned on this thread.
    drop((gate.map(|path| Gate(path, &success)), plan));
    success.get()
}

fn wait_for_release(directory: &Path) -> bool {
    let release = directory.join("close-release");
    if fs::write(directory.join("close-blocked"), b"worker").is_err() {
        return false;
    }
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(30) {
        if release.is_file() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    false
}
