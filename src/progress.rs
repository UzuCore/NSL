//! Thread-safe progress reporting shared by background operations and the UI.
//! A missing total means genuinely indeterminate work, never a made-up percent.

use std::io::{self, Read};
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Mutex,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Snapshot {
    pub label: String,
    pub completed: u64,
    pub total: Option<u64>,
    pub bytes: bool,
}

#[derive(Default)]
pub struct TaskProgress {
    completed: AtomicU64,
    total: AtomicU64,
    stage: Mutex<(String, bool)>,
}

impl TaskProgress {
    pub fn begin(&self, label: impl Into<String>, total: Option<u64>, bytes: bool) {
        let mut stage = self.stage.lock().unwrap_or_else(|error| error.into_inner());
        *stage = (label.into(), bytes);
        self.completed.store(0, Ordering::Relaxed);
        self.total.store(total.unwrap_or(0), Ordering::Relaxed);
    }

    pub fn set(&self, completed: u64) {
        self.completed.store(completed, Ordering::Relaxed);
    }

    pub fn advance(&self, amount: u64) {
        self.completed.fetch_add(amount, Ordering::Relaxed);
    }

    pub fn snapshot(&self) -> Snapshot {
        let stage = self.stage.lock().unwrap_or_else(|error| error.into_inner());
        let total = self.total.load(Ordering::Relaxed);
        Snapshot {
            label: stage.0.clone(),
            completed: self.completed.load(Ordering::Relaxed),
            total: (total > 0).then_some(total),
            bytes: stage.1,
        }
    }
}

/// Increment a progress counter for every successful read, including streams
/// without a Content-Length header. The caller still owns size limits.
pub struct MeteredReader<'a, R> {
    inner: R,
    progress: &'a TaskProgress,
}

impl<'a, R: Read> MeteredReader<'a, R> {
    pub fn new(inner: R, progress: &'a TaskProgress) -> Self {
        Self { inner, progress }
    }
}

impl<R: Read> Read for MeteredReader<'_, R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let read = self.inner.read(buf)?;
        self.progress.advance(read as u64);
        Ok(read)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transfer_tracks_real_bytes_and_stage_switches() {
        let progress = TaskProgress::default();
        progress.begin("titles.json", Some(5), true);
        let mut buffer = Vec::new();
        let read = MeteredReader::new(&b"hello"[..], &progress);
        io::copy(&mut read.take(6), &mut buffer).unwrap();
        assert_eq!(buffer, b"hello");
        assert_eq!(
            progress.snapshot(),
            Snapshot {
                label: "titles.json".into(),
                completed: 5,
                total: Some(5),
                bytes: true,
            }
        );
        progress.begin("DB 저장 중", None, false);
        assert_eq!(progress.snapshot().total, None);
        assert_eq!(progress.snapshot().completed, 0);
    }
}
