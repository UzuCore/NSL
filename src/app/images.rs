//! Manage lazy image loading, caches, cover prefetch, and resource sampling.

use crate::*;

impl NslApp {
    pub(crate) fn ensure_icon_texture(
        &mut self,
        ctx: &egui::Context,
        title_id: &str,
        budget: &mut usize,
    ) {
        if title_id.is_empty() || self.icon_textures.contains_key(title_id) {
            return;
        }
        if *budget == 0 || self.icon_missing.contains(title_id) {
            return;
        }
        *budget -= 1;
        self.image_loader.request(
            ImageKind::GameIcon,
            title_id,
            self.icon_dir.join(format!("{title_id}.png")),
            160,
            None,
        );
        ctx.request_repaint_after(std::time::Duration::from_millis(45));
    }
    pub(crate) fn trim_icon_cache(&mut self) {
        while self.icon_lru.len() > ICON_CACHE_MAX {
            let Some(oldest) = self.icon_lru.pop_front() else {
                break;
            };
            if self.icon_lru.contains(&oldest) {
                continue;
            }
            self.icon_textures.remove(&oldest);
        }
    }
    /// Install only a few finished thumbnails per frame. File I/O and image
    /// decoding run on two dedicated workers; rendering never waits for disk.
    pub(crate) fn poll_loaded_images(&mut self, ctx: &egui::Context) {
        for _ in 0..4 {
            let Some(done) = self.image_loader.try_recv() else {
                break;
            };
            if done.kind == ImageKind::Screenshot && done.selection != self.selected {
                continue;
            }
            match (done.kind, done.image) {
                (ImageKind::GameIcon, Some(image)) => {
                    let texture = ctx.load_texture("game_icon", image, Default::default());
                    if self
                        .icon_textures
                        .insert(done.key.clone(), texture)
                        .is_none()
                    {
                        self.icon_lru.push_back(done.key);
                        self.trim_icon_cache();
                    }
                }
                (ImageKind::StoreCover, Some(image)) => {
                    let texture = ctx.load_texture("store_cover", image, Default::default());
                    if self
                        .catalog_cover_textures
                        .insert(done.key.clone(), texture)
                        .is_none()
                    {
                        self.catalog_cover_lru.push_back(done.key);
                    }
                    while self.catalog_cover_lru.len() > STORE_COVER_CACHE_MAX {
                        if let Some(oldest) = self.catalog_cover_lru.pop_front() {
                            self.catalog_cover_textures.remove(&oldest);
                        }
                    }
                }
                (ImageKind::Screenshot, Some(image)) => {
                    let texture = ctx.load_texture("screenshot", image, Default::default());
                    self.screenshot_textures.insert(done.key, texture);
                }
                (ImageKind::GameIcon, None) => {
                    self.icon_missing.insert(done.key);
                }
                (ImageKind::StoreCover, None) => {
                    self.catalog_cover_missing.insert(done.key);
                }
                (ImageKind::Screenshot, None) => {
                    self.screenshot_missing.insert(done.key);
                }
            }
        }
        if self.image_loader.has_pending() {
            ctx.request_repaint_after(std::time::Duration::from_millis(45));
        }
    }
    pub(crate) fn touch_icon(&mut self, title_id: &str) {
        if self.icon_textures.contains_key(title_id) {
            self.icon_lru.retain(|id| id != title_id);
            self.icon_lru.push_back(title_id.to_owned());
        }
    }
    pub(crate) fn invalidate_icons(&mut self) {
        self.icon_textures.clear();
        self.icon_lru.clear();
        self.icon_missing.clear();
    }
    pub(crate) fn resource_usage_text(&self) -> String {
        if self.resource_sample.working_set_bytes == 0 {
            return String::new();
        }
        format!(
            "RAM: {:.1} MB  |  CPU: {:.1}%",
            self.resource_sample.working_set_bytes as f64 / (1024.0 * 1024.0),
            self.resource_sample.cpu_percent
        )
    }
    pub(crate) fn refresh_resource_sample(&mut self, ctx: &egui::Context) {
        let now = std::time::Instant::now();
        if self
            .resource_refresh_at
            .is_some_and(|deadline| now < deadline)
        {
            return;
        }
        if let Some((sample, clock)) = process_resource_sample(self.resource_clock) {
            self.resource_sample = sample;
            self.resource_clock = Some(clock);
        }
        self.resource_refresh_at = Some(now + std::time::Duration::from_secs(1));
        ctx.request_repaint_after(std::time::Duration::from_secs(1));
    }

    /// Load a store cover into the texture cache if it is on disk.
    pub(crate) fn ensure_catalog_cover(
        &mut self,
        ctx: &egui::Context,
        topic_id: &str,
        budget: &mut usize,
    ) {
        if !Self::safe_catalog_topic_id(topic_id) {
            return;
        }
        if self.catalog_cover_textures.contains_key(topic_id) {
            self.catalog_cover_lru.retain(|key| key != topic_id);
            self.catalog_cover_lru.push_back(topic_id.to_owned());
            return;
        }
        if *budget == 0 || self.catalog_cover_missing.contains(topic_id) {
            return;
        }
        *budget -= 1;
        self.image_loader.request(
            ImageKind::StoreCover,
            topic_id,
            self.catalog_cover_dir.join(format!("{topic_id}.jpg")),
            120,
            None,
        );
        ctx.request_repaint_after(std::time::Duration::from_millis(45));
    }

    pub(crate) fn safe_catalog_topic_id(id: &str) -> bool {
        !id.is_empty()
            && id.len() <= 80
            && id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    }

    pub(crate) fn should_prefetch_catalog_cover(
        &self,
        item: &catalog::CatalogItem,
        now: std::time::Instant,
    ) -> bool {
        !item.cover.is_empty()
            && Self::safe_catalog_topic_id(&item.topic_id)
            && self
                .catalog_cover_retry_after
                .get(&item.topic_id)
                .is_none_or(|deadline| *deadline <= now)
            && !self
                .catalog_cover_dir
                .join(format!("{}.jpg", item.topic_id))
                .is_file()
    }

    /// Fetch a small bounded batch; never block later catalog pages behind
    /// dozens of serial network timeouts. Failed URLs are retried only after
    /// a cooldown, not on every GUI repaint.
    pub(crate) fn start_cover_prefetch(&mut self, items: Vec<(String, String)>) {
        if self.catalog_cover_queue.is_some() || items.is_empty() {
            return;
        }
        let dir = self.catalog_cover_dir.clone();
        let (tx, rx) = mpsc::channel();
        self.catalog_cover_queue = Some(rx);
        std::thread::spawn(move || {
            let agent = ureq::AgentBuilder::new()
                .timeout(std::time::Duration::from_secs(12))
                .build();
            let pool = rayon::ThreadPoolBuilder::new()
                .num_threads(STORE_COVER_DOWNLOAD_THREADS)
                .build();
            let results = match pool {
                Ok(pool) => pool.install(|| {
                    items
                        .into_par_iter()
                        .map(|(topic_id, url)| {
                            let target = dir.join(format!("{topic_id}.jpg"));
                            let saved = (|| -> anyhow::Result<bool> {
                                if target.is_file() {
                                    return Ok(false);
                                }
                                let response = agent.get(&url).call()?;
                                let mut bytes = Vec::new();
                                response
                                    .into_reader()
                                    .take(16 * 1024 * 1024 + 1)
                                    .read_to_end(&mut bytes)?;
                                anyhow::ensure!(bytes.len() <= 16 * 1024 * 1024, "커버 파일 초과");
                                let header =
                                    image::ImageReader::new(std::io::Cursor::new(bytes.as_slice()))
                                        .with_guessed_format()?;
                                let (width, height) = header.into_dimensions()?;
                                anyhow::ensure!(
                                    width > 0
                                        && height > 0
                                        && u64::from(width) * u64::from(height) <= 12_000_000,
                                    "커버 해상도 초과"
                                );
                                image::load_from_memory(&bytes)?;
                                database::atomic_write(&target, &bytes)?;
                                Ok(true)
                            })();
                            (topic_id, saved)
                        })
                        .collect::<Vec<_>>()
                }),
                Err(_) => {
                    let failed = items.into_iter().map(|(topic_id, _)| topic_id).collect();
                    let _ = tx.send((0, failed));
                    return;
                }
            };
            let mut downloaded = 0;
            let mut failed = Vec::new();
            for (topic_id, result) in results {
                match result {
                    Ok(true) => downloaded += 1,
                    Ok(false) => {}
                    Err(_) => failed.push(topic_id),
                }
            }
            let _ = tx.send((downloaded, failed));
        });
    }
}
