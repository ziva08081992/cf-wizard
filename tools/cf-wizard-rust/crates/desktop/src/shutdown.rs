//! A single ownership transfer. Native windows and WebView never cross this boundary.
use serde_json::json;
use std::{
    fs::{self, File, OpenOptions},
    io::{self, Write},
    panic::{AssertUnwindSafe, catch_unwind},
    path::Path,
    sync::{
        Arc, Mutex,
        mpsc::{self, SyncSender},
    },
    thread,
};

#[derive(Clone, Default)]
pub(crate) struct Trace(Option<Arc<Mutex<File>>>);

impl Trace {
    pub(crate) fn open(folder: &Path, enabled: bool) -> Self {
        if !enabled || cf_infrastructure::source::no_links(folder).is_err() {
            return Self::default();
        }
        let name = format!(
            "{}-shutdown-{}.jsonl",
            chrono::Local::now().format("%Y-%m-%d_%H-%M-%S%.9f"),
            std::process::id()
        );
        let file = fs::create_dir_all(folder).and_then(|()| {
            OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(folder.join(name))
        });
        Self(file.ok().map(|f| Arc::new(Mutex::new(f))))
    }

    pub(crate) fn phase(&self, phase: &'static str) {
        if let Some(file) = &self.0
            && let Ok(mut writer) = file.lock()
        {
            let row = json!({"time":chrono::Utc::now().to_rfc3339(), "phase":phase});
            let _ = writeln!(writer, "{row}");
        }
    }
}

struct NotifyOnDrop<F: FnOnce(bool)> {
    notify: Option<F>,
    success: bool,
}

impl<F: FnOnce(bool)> Drop for NotifyOnDrop<F> {
    fn drop(&mut self) {
        if let Some(notify) = self.notify.take() {
            notify(self.success);
        }
    }
}

pub(crate) fn worker<T: Send + 'static>(
    release: impl FnOnce(T) -> bool + Send + 'static,
    notify: impl FnOnce(bool) + Send + 'static,
) -> io::Result<SyncSender<T>> {
    let (tx, rx) = mpsc::sync_channel(1);
    thread::Builder::new()
        .name("cf-shutdown".into())
        .spawn(move || {
            if let Ok(value) = rx.recv() {
                let mut done = NotifyOnDrop {
                    notify: Some(notify),
                    success: false,
                };
                done.success = catch_unwind(AssertUnwindSafe(|| release(value))).unwrap_or(false);
            }
        })?;
    Ok(tx)
}

pub(crate) enum Handoff<T> {
    Accepted,
    SpawnFailed,
    Returned(T),
}

pub(crate) fn handoff<T>(
    sender: io::Result<SyncSender<T>>,
    take: impl FnOnce() -> T,
) -> Handoff<T> {
    match sender {
        Err(_) => Handoff::SpawnFailed,
        Ok(sender) => match sender.send(take()) {
            Ok(()) => Handoff::Accepted,
            Err(error) => Handoff::Returned(error.0),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn worker_releases_on_another_thread_before_single_completion() {
        struct Block(mpsc::Sender<thread::ThreadId>, mpsc::Receiver<()>);
        impl Drop for Block {
            fn drop(&mut self) {
                self.0.send(thread::current().id()).unwrap();
                self.1.recv_timeout(Duration::from_secs(5)).unwrap();
            }
        }
        let (entered, started) = mpsc::channel();
        let (release, resume) = mpsc::channel();
        let (notify, completion) = mpsc::channel();
        let sender = worker(
            |value| {
                drop(value);
                true
            },
            move |ok| notify.send(ok).unwrap(),
        );
        assert!(matches!(
            handoff(sender, || Block(entered, resume)),
            Handoff::Accepted
        ));
        assert_ne!(
            started.recv_timeout(Duration::from_secs(5)).unwrap(),
            thread::current().id()
        );
        assert!(completion.try_recv().is_err());
        release.send(()).unwrap();
        assert_eq!(completion.recv_timeout(Duration::from_secs(5)), Ok(true));
        assert_eq!(completion.recv(), Err(mpsc::RecvError));
    }

    #[test]
    fn refusal_preserves_state_and_panic_notifies_once() {
        let failed = Err(io::Error::other("spawn"));
        assert!(matches!(
            handoff::<String>(failed, || panic!("taken before spawn")),
            Handoff::SpawnFailed
        ));
        let (tx, rx) = mpsc::sync_channel(1);
        drop(rx);
        assert!(matches!(
            handoff(Ok(tx), || "payload"),
            Handoff::Returned("payload")
        ));
        let (notify, result) = mpsc::channel();
        let sender = worker(
            |()| panic!("controlled cleanup unwind"),
            move |ok| notify.send(ok).unwrap(),
        );
        assert!(matches!(handoff(sender, || ()), Handoff::Accepted));
        assert_eq!(result.recv_timeout(Duration::from_secs(5)), Ok(false));
        assert_eq!(result.recv(), Err(mpsc::RecvError));
    }

    #[test]
    fn diagnostic_errors_are_optional() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("not-a-dir");
        fs::write(&file, b"x").unwrap();
        Trace::open(&file, true).phase("requested");
        Trace::open(dir.path(), false).phase("requested");
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
    }
}
