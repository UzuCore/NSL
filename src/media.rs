use image::{imageops::FilterType, DynamicImage, GenericImageView, ImageFormat};
use rayon::prelude::*;
use std::{
    io::{Cursor, Read},
    path::Path,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};

/// Images must be bounded before buffering, not just before decoding.
const MAX_IMAGE_BYTES: u64 = 32 * 1024 * 1024;

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
    let parsed = url::Url::parse(url)?;
    anyhow::ensure!(
        matches!(parsed.scheme(), "https" | "http") && parsed.host_str().is_some(),
        "허용하지 않는 이미지 URL입니다"
    );
    let response = ureq::get(url)
        .set("User-Agent", "Mozilla/5.0")
        .timeout(Duration::from_secs(15))
        .call()?;
    let bytes = read_capped(response.into_reader(), MAX_IMAGE_BYTES)?;
    anyhow::ensure!(bytes.len() > 1000, "image response too small");
    Ok(bytes)
}

fn fetch_with_retry(url: &str, progress: &MediaProgress) -> anyhow::Result<Vec<u8>> {
    let mut last_error = None;
    for attempt in 0..2 {
        anyhow::ensure!(
            !progress.cancelled.load(Ordering::Relaxed),
            "미디어 수집 중지됨"
        );
        match fetch(url) {
            Ok(bytes) => return Ok(bytes),
            Err(error) => {
                let permanent = error
                    .downcast_ref::<ureq::Error>()
                    .is_some_and(|error| matches!(error, ureq::Error::Status(400..=499, _)));
                last_error = Some(error);
                if permanent {
                    break;
                }
                if attempt == 0 {
                    std::thread::sleep(Duration::from_millis(220));
                }
            }
        }
    }
    Err(last_error.expect("at least one fetch attempt"))
}

/// Reject broken cached files without fully decoding every cached image.
fn valid_cached_image(path: &Path) -> bool {
    if !path.is_file()
        || std::fs::metadata(path)
            .map(|m| m.len() < 24)
            .unwrap_or(true)
    {
        return false;
    }
    image::ImageReader::open(path)
        .and_then(|reader| reader.with_guessed_format())
        .ok()
        .and_then(|reader| reader.into_dimensions().ok())
        .is_some_and(|(width, height)| width > 0 && height > 0 && width <= 8192 && height <= 8192)
}

fn valid_title_id(id: &str) -> bool {
    id.len() == 16 && id.bytes().all(|byte| byte.is_ascii_hexdigit())
}
/// Screenshots/icons are small; refuse anything absurd before decoding so a
/// hostile or broken URL cannot exhaust memory inside the image decoder.
fn image_bytes(data: &[u8], width: u32, format: ImageFormat) -> anyhow::Result<Vec<u8>> {
    anyhow::ensure!(
        (data.len() as u64) <= MAX_IMAGE_BYTES,
        "이미지가 너무 큽니다 ({} MB)",
        data.len() / (1024 * 1024)
    );
    // Reject decompression bombs before allocating a decoded pixel buffer.
    let header = image::ImageReader::new(Cursor::new(data)).with_guessed_format()?;
    let (header_width, header_height) = header.into_dimensions()?;
    anyhow::ensure!(
        header_width > 0
            && header_height > 0
            && header_width <= 8192
            && header_height <= 8192
            && u64::from(header_width) * u64::from(header_height) <= 24_000_000,
        "이미지 해상도가 허용 범위를 초과합니다"
    );
    let img = image::load_from_memory(data)?;
    let (w, h) = img.dimensions();
    anyhow::ensure!(w > 0 && h > 0, "invalid image dimensions");
    let target_h = ((h as f64 * width as f64 / w as f64).round() as u32).clamp(1, 4096);
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
    crate::database::atomic_write(path, &output)
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
    pub failed: AtomicUsize,
    pub cancelled: AtomicBool,
    error_samples: Mutex<Vec<String>>,
    /// Which half of a combined media collection pass is running.
    stage: AtomicUsize,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MediaOutcome {
    pub icons: usize,
    pub screenshots: usize,
    pub skipped: usize,
    pub failed: usize,
    pub cancelled: bool,
}

impl MediaOutcome {
    pub fn saved_total(&self) -> usize {
        self.icons + self.screenshots
    }
}

impl MediaProgress {
    pub fn begin(&self, total: usize) {
        self.total.store(total, Ordering::Relaxed);
        self.completed.store(0, Ordering::Relaxed);
        self.saved.store(0, Ordering::Relaxed);
        self.skipped.store(0, Ordering::Relaxed);
        self.failed.store(0, Ordering::Relaxed);
        self.cancelled.store(false, Ordering::Relaxed);
        if let Ok(mut errors) = self.error_samples.lock() {
            errors.clear();
        }
        self.stage.store(0, Ordering::Relaxed);
    }

    fn step(&self, saved: usize) {
        self.completed.fetch_add(1, Ordering::Relaxed);
        if saved > 0 {
            self.saved.fetch_add(saved, Ordering::Relaxed);
        }
    }

    fn fail(&self, message: String) {
        self.failed.fetch_add(1, Ordering::Relaxed);
        if let Ok(mut errors) = self.error_samples.lock() {
            if errors.len() < 25 {
                errors.push(message);
            }
        }
    }

    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Relaxed);
    }

    pub fn error_samples(&self) -> Vec<String> {
        self.error_samples
            .lock()
            .map(|v| v.clone())
            .unwrap_or_default()
    }

    pub fn stage_label(&self) -> &'static str {
        match self.stage.load(Ordering::Relaxed) {
            1 => "스크린샷",
            2 => "완료",
            3 => "중지됨",
            _ => "아이콘",
        }
    }

    pub fn snapshot(&self) -> (usize, usize, usize) {
        (
            self.completed.load(Ordering::Relaxed),
            self.total.load(Ordering::Relaxed),
            self.saved.load(Ordering::Relaxed),
        )
    }

    pub fn failures(&self) -> usize {
        self.failed.load(Ordering::Relaxed)
    }
}

/// One background task, one non-resetting progress meter, two media types.
/// Cached files are left untouched by the individual collection functions.
pub fn estimated_total(
    icons: &[(String, String, String)],
    screenshots: &[(String, String, String)],
) -> usize {
    icons.len().saturating_add(
        screenshots
            .iter()
            .map(|(_, raw, _)| {
                serde_json::from_str::<Vec<String>>(raw)
                    .map(|urls| urls.len().max(1))
                    .unwrap_or(1)
            })
            .sum::<usize>(),
    )
}

pub fn collect_media(
    icon_dir: &Path,
    screenshot_dir: &Path,
    icons: &[(String, String, String)],
    screenshots: &[(String, String, String)],
    progress: &MediaProgress,
) -> MediaOutcome {
    // The GUI initializes this counter before spawning the worker. Do not
    // reset a cancellation requested during the thread startup window.
    if progress.total.load(Ordering::Relaxed) == 0 {
        progress.begin(estimated_total(icons, screenshots));
    }
    let mut result = MediaOutcome::default();
    if !icons.is_empty() {
        result.icons = download_icons(icon_dir, icons, progress);
    }
    if !screenshots.is_empty() && !progress.cancelled.load(Ordering::Relaxed) {
        progress.stage.store(1, Ordering::Relaxed);
        result.screenshots = download_screenshots(screenshot_dir, screenshots, progress);
    }
    result.skipped = progress.skipped.load(Ordering::Relaxed);
    result.failed = progress.failed.load(Ordering::Relaxed);
    result.cancelled = progress.cancelled.load(Ordering::Relaxed);
    progress
        .stage
        .store(if result.cancelled { 3 } else { 2 }, Ordering::Relaxed);
    result
}

pub fn download_icons(
    cache_dir: &Path,
    items: &[(String, String, String)],
    progress: &MediaProgress,
) -> usize {
    let pool = match rayon::ThreadPoolBuilder::new().num_threads(6).build() {
        Ok(p) => p,
        Err(error) => {
            progress.fail(format!("아이콘 처리 스레드 시작 실패: {error}"));
            return 0;
        }
    };
    let count = Arc::new(AtomicUsize::new(0));
    pool.install(|| {
        items.par_iter().for_each(|(tid, url, package)| {
            if progress.cancelled.load(Ordering::Relaxed) {
                return;
            }
            if !valid_title_id(tid) {
                progress.fail(format!("아이콘 타이틀 ID 오류: {tid}"));
                progress.step(0);
                return;
            }
            let target = cache_dir.join(format!("{tid}.png"));
            if valid_cached_image(&target) {
                progress.skipped.fetch_add(1, Ordering::Relaxed);
                progress.step(0);
                return;
            }
            let mut last_error = None;
            let downloaded = if !url.is_empty() {
                match fetch_with_retry(url, progress).and_then(|data| {
                    if progress.cancelled.load(Ordering::Relaxed) {
                        anyhow::bail!("미디어 수집 중지됨");
                    }
                    write_image(&target, &data, 120, ImageFormat::Png)
                }) {
                    Ok(()) => true,
                    Err(error) => {
                        last_error = Some(error.to_string());
                        false
                    }
                }
            } else {
                false
            };
            if progress.cancelled.load(Ordering::Relaxed) {
                return;
            }
            let from_package = !downloaded
                && !package.is_empty()
                && Path::new(package).is_file()
                && crate::xci::extract_local_icon(Path::new(package), &target);
            let saved = downloaded || from_package;
            if saved {
                count.fetch_add(1, Ordering::Relaxed);
            } else {
                progress.fail(format!(
                    "{tid} 아이콘 수집 실패: {}",
                    last_error.unwrap_or_else(|| "다운로드 주소나 로컬 이미지가 없습니다".into())
                ));
            }
            progress.step(usize::from(saved));
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
        Err(error) => {
            progress.fail(format!("스크린샷 처리 스레드 시작 실패: {error}"));
            return 0;
        }
    };
    let count = Arc::new(AtomicUsize::new(0));
    pool.install(|| {
        items.par_iter().for_each(|(tid, raw, _package)| {
            if progress.cancelled.load(Ordering::Relaxed) {
                return;
            }
            let Ok(urls) = serde_json::from_str::<Vec<String>>(raw) else {
                progress.fail(format!("{tid} 스크린샷 목록 JSON 오류"));
                progress.step(0);
                return;
            };
            if !valid_title_id(tid) || urls.is_empty() {
                progress.fail(format!("{tid} 스크린샷 타이틀 ID 또는 목록 오류"));
                progress.step(0);
                return;
            }
            for (i, url) in urls.iter().enumerate() {
                if progress.cancelled.load(Ordering::Relaxed) {
                    return;
                }
                let parsed = url::Url::parse(url);
                if !matches!(
                    parsed.as_ref().ok().map(url::Url::scheme),
                    Some("http" | "https")
                ) {
                    progress.fail(format!("{tid} 스크린샷 {} URL 형식 오류", i + 1));
                    progress.step(0);
                    continue;
                }
                let path_ext = url
                    .split(['?', '#'])
                    .next()
                    .and_then(|s| s.rsplit_once('.').map(|(_, ext)| ext))
                    .filter(|ext| !ext.is_empty() && ext.len() <= 4)
                    .unwrap_or("jpg")
                    .to_ascii_lowercase();
                let format = ImageFormat::from_extension(&path_ext).unwrap_or(ImageFormat::Jpeg);
                let ext = match format {
                    ImageFormat::Png => "png",
                    ImageFormat::WebP => "webp",
                    _ => "jpg",
                };
                let filename = format!("{:03}", i + 1);
                let folder = cache_dir.join(tid);
                let target = folder.join(format!("{filename}.{ext}"));
                // A format change upstream must not create a second copy of
                // the same image when a valid file is already cached.
                let cached = ["jpg", "png", "webp"]
                    .iter()
                    .any(|suffix| valid_cached_image(&folder.join(format!("{filename}.{suffix}"))));
                if cached {
                    progress.skipped.fetch_add(1, Ordering::Relaxed);
                    progress.step(0);
                    continue;
                }
                let result = fetch_with_retry(url, progress).and_then(|data| {
                    if progress.cancelled.load(Ordering::Relaxed) {
                        anyhow::bail!("미디어 수집 중지됨");
                    }
                    write_image(&target, &data, 800, format)
                });
                if progress.cancelled.load(Ordering::Relaxed) {
                    return;
                }
                match result {
                    Ok(()) => {
                        count.fetch_add(1, Ordering::Relaxed);
                        progress.step(1);
                    }
                    Err(error) => {
                        progress.fail(format!("{tid} 스크린샷 {} 수집 실패: {error}", i + 1));
                        progress.step(0);
                    }
                }
            }
        })
    });
    count.load(Ordering::Relaxed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_image(format: ImageFormat) -> Vec<u8> {
        let image = DynamicImage::new_rgb8(2, 2);
        let mut cursor = Cursor::new(Vec::new());
        image.write_to(&mut cursor, format).unwrap();
        cursor.into_inner()
    }

    #[test]
    fn corrupt_cache_and_bad_screenshot_json_are_reported_as_failures() {
        let root = tempfile::tempdir().unwrap();
        let id = "0100D2C0115B6000";
        let icon_dir = root.path().join("icons");
        std::fs::create_dir_all(&icon_dir).unwrap();
        let icon = icon_dir.join(format!("{id}.png"));
        std::fs::write(&icon, b"not a PNG").unwrap();
        let icons = vec![(id.into(), String::new(), String::new())];
        let screenshots = vec![(id.into(), "INVALID JSON".into(), String::new())];
        let progress = MediaProgress::default();
        let result = collect_media(&icon_dir, root.path(), &icons, &screenshots, &progress);
        assert_eq!(result.failed, 2);
        assert_eq!(result.skipped, 0);
        assert_eq!(progress.snapshot(), (2, 2, 0));
        assert_eq!(progress.error_samples().len(), 2);
        assert_eq!(std::fs::read(icon).unwrap(), b"not a PNG");
    }

    #[test]
    fn screenshot_progress_counts_each_image_and_reuses_alternate_extension() {
        let root = tempfile::tempdir().unwrap();
        let id = "0100D2C0115B6000";
        let dir = root.path().join(id);
        std::fs::create_dir_all(&dir).unwrap();
        for index in [1, 2] {
            std::fs::write(
                dir.join(format!("{index:03}.jpg")),
                sample_image(ImageFormat::Jpeg),
            )
            .unwrap();
        }
        let screens = vec![(
            id.into(),
            serde_json::to_string(&vec![
                "https://example.test/a.webp",
                "https://example.test/b.png",
            ])
            .unwrap(),
            String::new(),
        )];
        let progress = MediaProgress::default();
        let result = collect_media(root.path(), root.path(), &[], &screens, &progress);
        assert_eq!(result.failed, 0);
        assert_eq!(result.skipped, 2);
        assert_eq!(progress.snapshot(), (2, 2, 0));
        assert!(!dir.join("001.webp").exists());
        assert!(!dir.join("002.png").exists());
    }

    #[test]
    fn cancellation_is_not_reset_by_worker_startup() {
        let root = tempfile::tempdir().unwrap();
        let sources = vec![("0100D2C0115B6000".into(), String::new(), String::new())];
        let progress = MediaProgress::default();
        progress.begin(estimated_total(&sources, &[]));
        progress.cancel();
        let result = collect_media(root.path(), root.path(), &sources, &[], &progress);
        assert!(result.cancelled);
        assert_eq!(progress.stage_label(), "중지됨");
        assert_eq!(progress.snapshot(), (0, 1, 0));
    }

    #[test]
    fn combined_collection_keeps_single_progress_meter_without_network() {
        let root = tempfile::tempdir().unwrap();
        let icon_dir = root.path().join("icons");
        let screenshot_dir = root.path().join("screenshots");
        let tid = "0100D2C0115B6000";
        std::fs::create_dir_all(&icon_dir).unwrap();
        std::fs::create_dir_all(screenshot_dir.join(tid)).unwrap();
        std::fs::write(
            icon_dir.join(format!("{tid}.png")),
            sample_image(ImageFormat::Png),
        )
        .unwrap();
        std::fs::write(
            screenshot_dir.join(tid).join("001.jpg"),
            sample_image(ImageFormat::Jpeg),
        )
        .unwrap();

        let icons = vec![(tid.into(), String::new(), String::new())];
        let screenshots = vec![(
            tid.into(),
            serde_json::to_string(&vec!["https://example.test/one.jpg"]).unwrap(),
            String::new(),
        )];
        let progress = MediaProgress::default();
        let result = collect_media(&icon_dir, &screenshot_dir, &icons, &screenshots, &progress);
        assert_eq!(result.icons, 0);
        assert_eq!(result.screenshots, 0);
        assert_eq!(result.failed, 0);
        assert_eq!(result.skipped, 2);
        assert_eq!(result.saved_total(), 0);
        assert_eq!(progress.snapshot(), (2, 2, 0));
        assert_eq!(progress.stage_label(), "완료");
        assert_eq!(progress.skipped.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn screenshot_only_and_empty_batch_complete_without_resetting_to_icon_step() {
        let root = tempfile::tempdir().unwrap();
        let empty: Vec<(String, String, String)> = Vec::new();
        let progress = MediaProgress::default();
        assert_eq!(
            collect_media(root.path(), root.path(), &empty, &empty, &progress),
            MediaOutcome::default(),
        );
        assert_eq!(progress.snapshot(), (0, 0, 0));
        assert_eq!(progress.stage_label(), "완료");
        progress.begin(3);
        progress.step(2);
        assert_eq!(progress.snapshot(), (1, 3, 2));
    }
}
