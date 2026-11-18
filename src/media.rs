use image::{imageops::FilterType, DynamicImage, GenericImageView, ImageFormat};
use rayon::prelude::*;
use std::{
    io::{Cursor, Read},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};

/// Streaming download helper with an explicit byte cap.
///
/// `ureq`'s `into_string()` stops at 10 MiB, which silently broke the title
/// database (~165 MiB) and would also break any large asset. Everything that
/// downloads into memory goes through here so the limit is explicit and large
/// enough, while still refusing runaway responses.
pub const MAX_DOWNLOAD_BYTES: u64 = 512 * 1024 * 1024;

fn read_capped(reader: impl Read, cap: u64) -> anyhow::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    let mut limited = reader.take(cap + 1);
    limited.read_to_end(&mut bytes)?;
    anyhow::ensure!(
        (bytes.len() as u64) <= cap,
        "다운로드 응답이 허용 크기({} MB)를 초과했습니다",
        cap / (1024 * 1024)
    );
    Ok(bytes)
}

fn fetch(url: &str) -> anyhow::Result<Vec<u8>> {
    let response = ureq::get(url)
        .set("User-Agent", "Mozilla/5.0")
        .timeout(Duration::from_secs(15))
        .call()?;
    let bytes = read_capped(response.into_reader(), MAX_DOWNLOAD_BYTES)?;
    anyhow::ensure!(bytes.len() > 1000, "image response too small");
    Ok(bytes)
}
/// Screenshots/icons are small; refuse anything absurd before decoding so a
/// hostile or broken URL cannot exhaust memory inside the image decoder.
const MAX_IMAGE_BYTES: u64 = 32 * 1024 * 1024;

fn image_bytes(data: &[u8], width: u32, format: ImageFormat) -> anyhow::Result<Vec<u8>> {
    anyhow::ensure!(
        (data.len() as u64) <= MAX_IMAGE_BYTES,
        "이미지가 너무 큽니다 ({} MB)",
        data.len() / (1024 * 1024)
    );
    let img = image::load_from_memory(data)?;
    let (w, h) = img.dimensions();
    anyhow::ensure!(w > 0 && h > 0, "invalid image dimensions");
    let target_h = ((h as f64 * width as f64 / w as f64).round() as u32).max(1);
    let resized: DynamicImage = img
        .resize_exact(width, target_h, FilterType::Lanczos3)
        .to_rgb8()
        .into();
    let mut cursor = Cursor::new(Vec::new());
    resized.write_to(&mut cursor, format)?;
    Ok(cursor.into_inner())
}
fn write_image(path: &Path, bytes: &[u8], width: u32, format: ImageFormat) -> anyhow::Result<()> {
    let output = image_bytes(bytes, width, format)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut temp = PathBuf::from(path);
    let ext = path.extension().and_then(|s| s.to_str()).unwrap_or("jpg");
    temp.set_extension(format!("{ext}.tmp"));
    std::fs::write(&temp, output)?;
    if path.exists() {
        std::fs::remove_file(path)?;
    }
    std::fs::rename(temp, path)?;
    Ok(())
}

/// Live progress for a media download pass, shared between the worker threads
/// and the UI status bar.
#[derive(Default)]
pub struct MediaProgress {
    pub completed: AtomicUsize,
    pub total: AtomicUsize,
    pub saved: AtomicUsize,
    /// Cached files that did not need a download.
    pub skipped: AtomicUsize,
}

impl MediaProgress {
    pub fn begin(&self, total: usize) {
        self.total.store(total, Ordering::Relaxed);
        self.completed.store(0, Ordering::Relaxed);
        self.saved.store(0, Ordering::Relaxed);
        self.skipped.store(0, Ordering::Relaxed);
    }

    fn step(&self, saved: bool) {
        self.completed.fetch_add(1, Ordering::Relaxed);
        if saved {
            self.saved.fetch_add(1, Ordering::Relaxed);
        }
    }

    pub fn snapshot(&self) -> (usize, usize, usize) {
        (
            self.completed.load(Ordering::Relaxed),
            self.total.load(Ordering::Relaxed),
            self.saved.load(Ordering::Relaxed),
        )
    }
}

pub fn download_icons(
    cache_dir: &Path,
    items: &[(String, String, String)],
    progress: &MediaProgress,
) -> usize {
    let pool = match rayon::ThreadPoolBuilder::new().num_threads(6).build() {
        Ok(p) => p,
        Err(_) => return 0,
    };
    let count = Arc::new(AtomicUsize::new(0));
    progress.begin(items.len());
    pool.install(|| {
        items.par_iter().for_each(|(tid, url, package)| {
            if tid.is_empty() {
                progress.step(false);
                return;
            }
            let target = cache_dir.join(format!("{tid}.png"));
            if target.exists() {
                progress.skipped.fetch_add(1, Ordering::Relaxed);
                progress.step(false);
                return;
            }
            let downloaded = !url.is_empty()
                && fetch(url)
                    .ok()
                    .map(|data| write_image(&target, &data, 120, ImageFormat::Png).is_ok())
                    .unwrap_or(false);
            let saved = downloaded || crate::xci::extract_local_icon(Path::new(package), &target);
            if saved {
                count.fetch_add(1, Ordering::Relaxed);
            }
            progress.step(saved);
        })
    });
    count.load(Ordering::Relaxed)
}

pub fn download_screenshots(
    cache_dir: &Path,
    items: &[(String, String, String)],
    progress: &MediaProgress,
) -> usize {
    let pool = match rayon::ThreadPoolBuilder::new().num_threads(6).build() {
        Ok(p) => p,
        Err(_) => return 0,
    };
    let count = Arc::new(AtomicUsize::new(0));
    progress.begin(items.len());
    pool.install(|| {
        items.par_iter().for_each(|(tid, raw, _package)| {
            let Ok(urls) = serde_json::from_str::<Vec<String>>(raw) else {
                progress.step(false);
                return;
            };
            if tid.is_empty() {
                progress.step(false);
                return;
            }
            let mut saved_any = false;
            for (i, url) in urls.iter().enumerate() {
                if !url.starts_with("http") {
                    continue;
                }
                let path_ext = url
                    .split(['?', '#'])
                    .next()
                    .and_then(|s| s.rsplit_once('.').map(|(_, e)| e))
                    .filter(|e| !e.is_empty() && e.len() <= 4)
                    .unwrap_or("jpg")
                    .to_ascii_lowercase();
                let format = ImageFormat::from_extension(&path_ext).unwrap_or(ImageFormat::Jpeg);
                let ext = match format {
                    ImageFormat::Png => "png",
                    ImageFormat::WebP => "webp",
                    _ => "jpg",
                };
                let target = cache_dir.join(tid).join(format!("{:03}.{ext}", i + 1));
                if target.exists() {
                    continue;
                }
                if let Ok(data) = fetch(url) {
                    if write_image(&target, &data, 800, format).is_ok() {
                        count.fetch_add(1, Ordering::Relaxed);
                        saved_any = true;
                    }
                }
            }
            progress.step(saved_any);
        })
    });
    count.load(Ordering::Relaxed)
}
