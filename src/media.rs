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

fn fetch(url: &str) -> anyhow::Result<Vec<u8>> {
    let response = ureq::get(url)
        .set("User-Agent", "Mozilla/5.0")
        .timeout(Duration::from_secs(15))
        .call()?;
    let mut bytes = Vec::new();
    response.into_reader().read_to_end(&mut bytes)?;
    anyhow::ensure!(bytes.len() > 1000, "image response too small");
    Ok(bytes)
}
fn image_bytes(data: &[u8], width: u32, format: ImageFormat) -> anyhow::Result<Vec<u8>> {
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

pub fn download_icons(cache_dir: &Path, items: &[(String, String, String)]) -> usize {
    let pool = match rayon::ThreadPoolBuilder::new().num_threads(6).build() {
        Ok(p) => p,
        Err(_) => return 0,
    };
    let count = Arc::new(AtomicUsize::new(0));
    pool.install(|| {
        items.par_iter().for_each(|(tid, url, package)| {
            if tid.is_empty() {
                return;
            }
            let target = cache_dir.join(format!("{tid}.png"));
            if target.exists() {
                return;
            }
            let downloaded = !url.is_empty()
                && fetch(url)
                    .ok()
                    .map(|data| write_image(&target, &data, 120, ImageFormat::Png).is_ok())
                    .unwrap_or(false);
            if downloaded || crate::xci::extract_local_icon(Path::new(package), &target) {
                count.fetch_add(1, Ordering::Relaxed);
            }
        })
    });
    count.load(Ordering::Relaxed)
}

pub fn download_screenshots(cache_dir: &Path, items: &[(String, String, String)]) -> usize {
    let pool = match rayon::ThreadPoolBuilder::new().num_threads(6).build() {
        Ok(p) => p,
        Err(_) => return 0,
    };
    let count = Arc::new(AtomicUsize::new(0));
    pool.install(|| {
        items.par_iter().for_each(|(tid, raw, _package)| {
            let Ok(urls) = serde_json::from_str::<Vec<String>>(raw) else {
                return;
            };
            if tid.is_empty() {
                return;
            }
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
                    }
                }
            }
        })
    });
    count.load(Ordering::Relaxed)
}
