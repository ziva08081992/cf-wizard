use cf_domain::canonical::sha256;
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

const SAMPLE_LIMIT: usize = 128;
const HISTORY_BYTES: u64 = 64 * 1024;
const MIN_SAMPLES: usize = 2;

#[derive(Clone, Deserialize, Serialize)]
struct Sample {
    key: String,
    milliseconds: u64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Range {
    lower_seconds: u64,
    upper_seconds: u64,
    samples: usize,
}

pub(crate) struct History {
    path: PathBuf,
    samples: Vec<Sample>,
    pending: Vec<Sample>,
}

impl History {
    pub(crate) fn load(path: PathBuf) -> Self {
        let samples = Self::read(&path);
        Self {
            path,
            samples,
            pending: Vec::new(),
        }
    }

    fn read(path: &Path) -> Vec<Sample> {
        let mut bytes = Vec::new();
        let result =
            File::open(path).and_then(|file| file.take(HISTORY_BYTES + 1).read_to_end(&mut bytes));
        if result.is_err() || bytes.len() as u64 > HISTORY_BYTES {
            return Vec::new();
        }
        let mut samples: Vec<Sample> = serde_json::from_slice(&bytes).unwrap_or_default();
        Self::trim(&mut samples);
        samples
    }

    fn trim(samples: &mut Vec<Sample>) {
        if samples.len() > SAMPLE_LIMIT {
            samples.drain(..samples.len() - SAMPLE_LIMIT);
        }
    }

    pub(crate) fn estimate(&self, key: &str) -> Option<Range> {
        let mut samples = self
            .samples
            .iter()
            .chain(&self.pending)
            .rev()
            .take(SAMPLE_LIMIT)
            .filter(|sample| sample.key == key);
        let first = samples.next()?.milliseconds;
        let (min, max, count) = samples.fold((first, first, 1), |(min, max, count), sample| {
            (
                min.min(sample.milliseconds),
                max.max(sample.milliseconds),
                count + 1,
            )
        });
        (count >= MIN_SAMPLES).then(|| {
            let lower_seconds = min / 1000;
            Range {
                lower_seconds,
                upper_seconds: max.div_ceil(1000).max(lower_seconds + 1),
                samples: count,
            }
        })
    }

    pub(crate) fn record(&mut self, key: String, elapsed: Duration) {
        self.pending.push(Sample {
            key,
            milliseconds: elapsed.as_millis().try_into().unwrap_or(u64::MAX),
        });
        Self::trim(&mut self.pending);
        // Keep pending observations if the history cannot be written now.
        let _ = self.save();
    }

    fn save(&mut self) -> std::io::Result<()> {
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(self.path.with_extension("lock"))?;
        lock.try_lock().map_err(std::io::Error::from)?;
        let mut samples = Self::read(&self.path);
        samples.extend(self.pending.iter().cloned());
        Self::trim(&mut samples);

        let mut file = tempfile::NamedTempFile::new_in(
            self.path
                .parent()
                .filter(|parent| !parent.as_os_str().is_empty())
                .unwrap_or(Path::new(".")),
        )?;
        serde_json::to_writer(file.as_file_mut(), &samples)?;
        file.flush()?;
        file.persist(&self.path)?;
        self.samples = samples;
        self.pending.clear();
        Ok(())
    }
}

pub(crate) fn key(value: impl Serialize) -> Option<String> {
    serde_json::to_vec(&(env!("CARGO_PKG_VERSION"), value))
        .ok()
        .map(|bytes| sha256(&bytes))
}

pub(crate) fn file_stamp(path: &Path) -> Option<(u64, SystemTime)> {
    let metadata = fs::metadata(path).ok()?;
    if !metadata.is_file() {
        return None;
    }
    Some((metadata.len(), metadata.modified().ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn range_uses_matching_observations_and_survives_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("preview-timings.json");
        let mut history = History::load(path.clone());
        history.record("selection-a".into(), Duration::from_millis(4700));
        history.record("selection-b".into(), Duration::from_secs(95));
        assert!(history.estimate("selection-a").is_none());

        history.record("selection-a".into(), Duration::from_millis(16_350));
        let history = History::load(path);
        let estimate = history.estimate("selection-a").unwrap();
        assert_eq!(
            (
                estimate.lower_seconds,
                estimate.upper_seconds,
                estimate.samples
            ),
            (4, 17, 2)
        );
        assert!(history.estimate("selection-b").is_none());
    }

    #[test]
    fn merges_samples_written_by_other_instances() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("preview-timings.json");
        let mut first = History::load(path.clone());
        let mut second = History::load(path.clone());
        first.record("same-key".into(), Duration::from_secs(4));
        second.record("same-key".into(), Duration::from_secs(8));

        let reopened = History::load(path);
        let estimate = reopened.estimate("same-key").unwrap();
        assert_eq!(
            (
                estimate.lower_seconds,
                estimate.upper_seconds,
                estimate.samples
            ),
            (4, 8, 2)
        );
    }

    #[test]
    fn ignores_corrupt_history_and_bounds_fast_range() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("preview-timings.json");
        fs::write(&path, b"{invalid").unwrap();
        let mut history = History::load(path);
        for _ in 0..2 {
            history.record("fast".into(), Duration::from_millis(3));
        }
        let estimate = history.estimate("fast").unwrap();
        assert_eq!((estimate.lower_seconds, estimate.upper_seconds), (0, 1));
    }
}
