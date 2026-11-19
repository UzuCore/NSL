//! Disk image decoding off the egui render thread. The library/store can paint
//! text immediately while only the on-screen thumbnails are decoded in two
//! bounded workers. GPU texture upload is performed on the GUI thread.
use eframe::egui;
use std::{
    collections::{HashSet, VecDeque},
    path::PathBuf,
    sync::{
        mpsc::{self, Receiver, TryRecvError},
        Arc, Condvar, Mutex,
    },
};

const QUEUE_LIMIT: usize = 24;
const MAX_SOURCE_BYTES: u64 = 32 * 1024 * 1024;
const MAX_DECODE_PIXELS: u64 = 24_000_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Kind {
    GameIcon,
    StoreCover,
    Screenshot,
}

struct Job {
    kind: Kind,
    key: String,
    path: PathBuf,
    max_edge: u32,
    selection: Option<i64>,
}

/// A full queue may discard thumbnails from an older viewport before they
/// reach a decoder, keeping fast scrolling responsive.
#[derive(Default)]
struct JobQueue {
    waiting: VecDeque<Job>,
    closed: bool,
}

impl JobQueue {
    fn enqueue(&mut self, pending: &mut HashSet<(Kind, String)>, job: Job) -> bool {
        let identity = (job.kind, job.key.clone());
        if self.closed || pending.contains(&identity) {
            return false;
        }
        if pending.len() >= QUEUE_LIMIT {
            // Do not evict jobs already decoding or completed, only waiting.
            let Some(stale) = self.waiting.pop_front() else {
                return false;
            };
            pending.remove(&(stale.kind, stale.key));
        }
        pending.insert(identity);
        self.waiting.push_back(job);
        true
    }

    fn cancel_stale_screenshots(
        &mut self,
        pending: &mut HashSet<(Kind, String)>,
        selection: Option<i64>,
    ) {
        self.waiting.retain(|job| {
            if job.kind == Kind::Screenshot && job.selection != selection {
                pending.remove(&(job.kind, job.key.clone()));
                false
            } else {
                true
            }
        });
    }
}

type SharedQueue = Arc<(Mutex<JobQueue>, Condvar)>;

pub(crate) struct Loaded {
    pub kind: Kind,
    pub key: String,
    pub image: Option<egui::ColorImage>,
    pub selection: Option<i64>,
}

pub(crate) struct ImageLoader {
    queue: SharedQueue,
    receiver: Receiver<Loaded>,
    pending: HashSet<(Kind, String)>,
}

impl ImageLoader {
    pub fn new() -> Self {
        let (finished, receiver) = mpsc::channel::<Loaded>();
        let queue: SharedQueue = Arc::new((Mutex::new(JobQueue::default()), Condvar::new()));
        for worker in 0..2 {
            let jobs = Arc::clone(&queue);
            let finished = finished.clone();
            let _ = std::thread::Builder::new()
                .name(format!("nsl-image-{worker}"))
                .spawn(move || loop {
                    // Release the queue lock before file I/O or decoding.
                    let job = {
                        let (lock, wake) = &*jobs;
                        let mut queue = match lock.lock() {
                            Ok(queue) => queue,
                            Err(_) => break,
                        };
                        loop {
                            if queue.closed {
                                break None;
                            }
                            if let Some(job) = queue.waiting.pop_front() {
                                break Some(job);
                            }
                            queue = match wake.wait(queue) {
                                Ok(queue) => queue,
                                Err(_) => break None,
                            };
                        }
                    };
                    let Some(job) = job else { break };
                    let image = decode(&job.path, job.max_edge);
                    if finished
                        .send(Loaded {
                            kind: job.kind,
                            key: job.key,
                            image,
                            selection: job.selection,
                        })
                        .is_err()
                    {
                        break;
                    }
                });
        }
        Self {
            queue,
            receiver,
            pending: HashSet::new(),
        }
    }

    /// Nonblocking even when full; newly visible images replace obsolete
    /// images which are still waiting from a previous viewport.
    pub fn request(
        &mut self,
        kind: Kind,
        key: &str,
        path: PathBuf,
        max_edge: u32,
        selection: Option<i64>,
    ) {
        if key.is_empty() {
            return;
        }
        let job = Job {
            kind,
            key: key.to_owned(),
            path,
            max_edge,
            selection,
        };
        let (lock, wake) = &*self.queue;
        if let Ok(mut queue) = lock.lock() {
            if queue.enqueue(&mut self.pending, job) {
                wake.notify_one();
            }
        }
    }

    /// Cancel unstarted screenshots for an old selection. In-flight results
    /// are still checked against the selection on the GUI thread.
    pub fn cancel_stale_screenshots(&mut self, selection: Option<i64>) {
        let (lock, _) = &*self.queue;
        if let Ok(mut queue) = lock.lock() {
            queue.cancel_stale_screenshots(&mut self.pending, selection);
        }
    }

    pub fn try_recv(&mut self) -> Option<Loaded> {
        match self.receiver.try_recv() {
            Ok(result) => {
                self.pending.remove(&(result.kind, result.key.clone()));
                Some(result)
            }
            Err(TryRecvError::Empty | TryRecvError::Disconnected) => None,
        }
    }

    pub fn has_pending(&self) -> bool {
        !self.pending.is_empty()
    }
}

impl Drop for ImageLoader {
    fn drop(&mut self) {
        let (lock, wake) = &*self.queue;
        if let Ok(mut queue) = lock.lock() {
            queue.closed = true;
            queue.waiting.clear();
            wake.notify_all();
        }
    }
}

fn decode(path: &std::path::Path, max_edge: u32) -> Option<egui::ColorImage> {
    if max_edge == 0 || std::fs::metadata(path).ok()?.len() > MAX_SOURCE_BYTES {
        return None;
    }
    let dimensions = image::ImageReader::open(path)
        .ok()?
        .into_dimensions()
        .ok()?;
    if dimensions.0 == 0
        || dimensions.1 == 0
        || dimensions.0 > 8192
        || dimensions.1 > 8192
        || u64::from(dimensions.0) * u64::from(dimensions.1) > MAX_DECODE_PIXELS
    {
        return None;
    }
    let thumbnail = image::open(path)
        .ok()?
        .thumbnail(max_edge, max_edge)
        .to_rgba8();
    let size = [thumbnail.width() as usize, thumbnail.height() as usize];
    Some(egui::ColorImage::from_rgba_unmultiplied(
        size,
        thumbnail.as_raw(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_job(kind: Kind, key: String, selection: Option<i64>) -> Job {
        Job {
            kind,
            key,
            path: PathBuf::new(),
            max_edge: 80,
            selection,
        }
    }

    #[test]
    fn rapid_scroll_replaces_waiting_images_but_preserves_in_flight_work() {
        let mut queue = JobQueue::default();
        let mut pending = HashSet::new();
        for n in 0..QUEUE_LIMIT {
            assert!(queue.enqueue(
                &mut pending,
                test_job(Kind::GameIcon, format!("old-{n}"), None),
            ));
        }
        // Two workers have removed these from the queue and are decoding.
        let active: Vec<_> = (0..2).map(|_| queue.waiting.pop_front().unwrap()).collect();
        for n in 0..16 {
            assert!(queue.enqueue(
                &mut pending,
                test_job(Kind::GameIcon, format!("visible-{n}"), None),
            ));
        }
        assert_eq!(pending.len(), QUEUE_LIMIT);
        assert_eq!(queue.waiting.len(), QUEUE_LIMIT - active.len());
        assert!(pending.contains(&(Kind::GameIcon, active[0].key.clone())));
        assert!(pending.contains(&(Kind::GameIcon, active[1].key.clone())));
        assert!(!pending.contains(&(Kind::GameIcon, "old-2".to_owned())));
        assert!(pending.contains(&(Kind::GameIcon, "visible-15".to_owned())));
        assert!(!queue.enqueue(
            &mut pending,
            test_job(Kind::GameIcon, "visible-15".into(), None)
        ));
    }

    #[test]
    fn selection_change_drops_only_unstarted_old_screenshots() {
        let mut queue = JobQueue::default();
        let mut pending = HashSet::new();
        for n in 0..6 {
            assert!(queue.enqueue(
                &mut pending,
                test_job(Kind::Screenshot, format!("old-screen-{n}"), Some(1)),
            ));
        }
        assert!(queue.enqueue(
            &mut pending,
            test_job(Kind::GameIcon, "keep-icon".into(), None),
        ));
        queue.cancel_stale_screenshots(&mut pending, Some(2));
        assert_eq!(queue.waiting.len(), 1);
        assert_eq!(pending.len(), 1);
        assert_eq!(queue.waiting[0].key, "keep-icon");
        assert!(queue.enqueue(
            &mut pending,
            test_job(Kind::Screenshot, "new-screen".into(), Some(2)),
        ));
    }

    #[test]
    fn lazy_loader_returns_bounded_thumbnail_and_deduplicates_requests() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("sample.png");
        let source = image::RgbaImage::from_pixel(800, 400, image::Rgba([10, 20, 30, 255]));
        source.save(&path).unwrap();
        let mut loader = ImageLoader::new();
        loader.request(Kind::GameIcon, "title", path.clone(), 120, None);
        loader.request(Kind::GameIcon, "title", path, 120, None);
        assert_eq!(loader.pending.len(), 1);
        let result = (0..100)
            .find_map(|_| {
                std::thread::sleep(std::time::Duration::from_millis(10));
                loader.try_recv()
            })
            .unwrap();
        assert_eq!(result.kind, Kind::GameIcon);
        assert_eq!(result.image.unwrap().size, [120, 60]);
        assert!(!loader.has_pending());
    }

    #[test]
    fn missing_images_report_without_retry_loop_on_ui_thread() {
        let temp = tempfile::tempdir().unwrap();
        let mut loader = ImageLoader::new();
        loader.request(
            Kind::Screenshot,
            "not-here",
            temp.path().join("x.png"),
            900,
            Some(4),
        );
        let result = (0..100)
            .find_map(|_| {
                std::thread::sleep(std::time::Duration::from_millis(10));
                loader.try_recv()
            })
            .unwrap();
        assert!(result.image.is_none());
        assert_eq!(result.selection, Some(4));
    }
}
