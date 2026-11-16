#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod common;
mod database;
mod decompress;
mod library;
mod media;
mod scanner;
mod theme;
mod titledb;
mod translate;
mod updater;
mod xci;

use anyhow::Result;
use eframe::egui;
use rayon::prelude::*;
use std::{
    collections::{HashMap, HashSet, VecDeque},
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc::{self, Receiver, Sender},
        Arc,
    },
};

const ICON_CACHE_MAX: usize = 300;
const ICON_LOAD_BUDGET: usize = 32;

/// Process memory and CPU usage for the status bar meter.
#[derive(Clone, Copy, Debug, Default)]
struct ResourceSample {
    working_set_bytes: u64,
    cpu_percent: f32,
}

#[cfg(windows)]
fn process_resource_sample(previous: Option<(std::time::Instant, u64)>) -> Option<(ResourceSample, (std::time::Instant, u64))> {
    #[repr(C)]
    #[derive(Default)]
    struct MemoryCounters {
        cb: u32,
        page_fault_count: u32,
        peak_working_set_size: usize,
        working_set_size: usize,
        quota_peak_paged_pool_usage: usize,
        quota_paged_pool_usage: usize,
        quota_peak_non_paged_pool_usage: usize,
        quota_non_paged_pool_usage: usize,
        pagefile_usage: usize,
        peak_pagefile_usage: usize,
    }
    #[link(name = "kernel32")]
    extern "system" {
        fn GetCurrentProcess() -> *mut core::ffi::c_void;
        fn GetProcessTimes(
            process: *mut core::ffi::c_void,
            creation: *mut i64,
            exit: *mut i64,
            kernel: *mut i64,
            user: *mut i64,
        ) -> i32;
    }
    #[link(name = "psapi")]
    extern "system" {
        fn GetProcessMemoryInfo(
            process: *mut core::ffi::c_void,
            counters: *mut MemoryCounters,
            cb: u32,
        ) -> i32;
    }

    // SAFETY: the handle is the current pseudo-handle, and both output buffers are
    // fully initialized stack structs of the size we pass to the OS.
    unsafe {
        let handle = GetCurrentProcess();
        let mut counters = MemoryCounters {
            cb: std::mem::size_of::<MemoryCounters>() as u32,
            ..Default::default()
        };
        if GetProcessMemoryInfo(handle, &mut counters, counters.cb) == 0 {
            return None;
        }
        let (mut created, mut exited, mut kernel, mut user) = (0i64, 0i64, 0i64, 0i64);
        if GetProcessTimes(handle, &mut created, &mut exited, &mut kernel, &mut user) == 0 {
            return None;
        }
        let total_cpu_100ns = (kernel.max(0) as u64).saturating_add(user.max(0) as u64);
        let now = std::time::Instant::now();
        let cpu_percent = previous
            .map(|(last_at, last_cpu)| {
                let wall = now.duration_since(last_at).as_secs_f64();
                if wall <= 0.0 {
                    return 0.0;
                }
                let cores = std::thread::available_parallelism()
                    .map(|value| value.get())
                    .unwrap_or(1) as f64;
                let cpu = total_cpu_100ns.saturating_sub(last_cpu) as f64 / 10_000_000.0;
                ((cpu / wall) * 100.0 / cores) as f32
            })
            .unwrap_or(0.0);
        Some((
            ResourceSample {
                working_set_bytes: counters.working_set_size as u64,
                cpu_percent,
            },
            (now, total_cpu_100ns),
        ))
    }
}

#[cfg(not(windows))]
fn process_resource_sample(
    _previous: Option<(std::time::Instant, u64)>,
) -> Option<(ResourceSample, (std::time::Instant, u64))> {
    None
}

struct ScanProgress {
    completed: AtomicUsize,
    total: AtomicUsize,
}

/// Files applied to the database, stale rows removed, and whether the pass was cancelled.
type ScanOutcome = (usize, usize, bool);

fn paint_selectable_cell(
    ui: &egui::Ui,
    rect: egui::Rect,
    response: &egui::Response,
    selected: bool,
) -> egui::Color32 {
    let visuals = ui.style().interact_selectable(response, selected);
    if selected || response.hovered() || response.highlighted() || response.has_focus() {
        ui.painter().rect(
            rect.expand(visuals.expansion),
            visuals.rounding,
            visuals.weak_bg_fill,
            visuals.bg_stroke,
        );
    }
    visuals.text_color()
}

fn selectable_table_text_cell(
    ui: &mut egui::Ui,
    width: f32,
    height: f32,
    text: &str,
    selected: bool,
    align: egui::Align,
) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(width, height), egui::Sense::click());
    let text_color = paint_selectable_cell(ui, rect, &response, selected);
    let (position, anchor) = if align == egui::Align::Min {
        (
            rect.left_center() + egui::vec2(6.0, 0.0),
            egui::Align2::LEFT_CENTER,
        )
    } else {
        (rect.center(), egui::Align2::CENTER_CENTER)
    };
    ui.painter()
        .with_clip_rect(rect.shrink2(egui::vec2(4.0, 0.0)))
        .text(
            position,
            anchor,
            text,
            egui::TextStyle::Button.resolve(ui.style()),
            text_color,
        );
    response
}

fn library_grid_dimensions(available_width: f32) -> (usize, f32, f32) {
    let available_width = available_width.max(1.0);
    let min_card_width = 130.0;
    let max_card_width = 190.0;
    let target_card_width = 165.0;
    let min_columns = (available_width / max_card_width).ceil().max(1.0) as usize;
    let max_columns = (available_width / min_card_width).floor().max(1.0) as usize;
    let target_columns = (available_width / target_card_width).round().max(1.0) as usize;
    let columns = target_columns.clamp(min_columns, max_columns.max(min_columns));
    let card_width = available_width / columns as f32;
    let thumbnail_size = (card_width - 30.0).clamp(88.0, 136.0);
    (columns, card_width, thumbnail_size)
}

fn draw_tab_button(
    ui: &mut egui::Ui,
    label: &str,
    selected: bool,
    width: f32,
    index: usize,
    tab_count: usize,
) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(width, 34.0), egui::Sense::click());
    let visuals = ui.visuals();
    let fill = if selected {
        visuals.panel_fill
    } else if response.hovered() {
        visuals.window_fill
    } else {
        visuals.faint_bg_color
    };
    let border = visuals.widgets.noninteractive.bg_stroke.color;
    let panel_fill = visuals.panel_fill;
    let faint_bg = visuals.faint_bg_color;
    let text_color = visuals.text_color();
    let painter = ui.painter();
    painter.rect_filled(rect, 0.0, fill);
    painter.line_segment(
        [rect.left_top(), rect.right_top()],
        egui::Stroke::new(1.0_f32, border),
    );
    painter.line_segment(
        [
            rect.left_bottom() - egui::vec2(0.0, 1.0),
            rect.right_bottom() - egui::vec2(0.0, 1.0),
        ],
        egui::Stroke::new(1.0_f32, if selected { panel_fill } else { border }),
    );
    painter.line_segment(
        [rect.left_top(), rect.left_bottom()],
        egui::Stroke::new(
            1.0_f32,
            if index == 0 {
                border
            } else if selected {
                panel_fill
            } else {
                faint_bg
            },
        ),
    );
    if index + 1 == tab_count {
        painter.line_segment(
            [rect.right_top(), rect.right_bottom()],
            egui::Stroke::new(1.0_f32, border),
        );
    }
    painter.text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        label,
        egui::TextStyle::Button.resolve(ui.style()),
        text_color,
    );
    response
}

fn contrasting_button_text(background: egui::Color32) -> egui::Color32 {
    fn luminance_channel(channel: u8) -> f32 {
        let value = channel as f32 / 255.0;
        if value <= 0.04045 {
            value / 12.92
        } else {
            ((value + 0.055) / 1.055).powf(2.4)
        }
    }

    let luminance = 0.2126 * luminance_channel(background.r())
        + 0.7152 * luminance_channel(background.g())
        + 0.0722 * luminance_channel(background.b());
    let dark_text = egui::Color32::from_rgb(31, 35, 40);
    let dark_luminance = 0.2126 * luminance_channel(dark_text.r())
        + 0.7152 * luminance_channel(dark_text.g())
        + 0.0722 * luminance_channel(dark_text.b());
    let white_contrast = 1.05 / (luminance + 0.05);
    let dark_contrast = (luminance + 0.05) / (dark_luminance + 0.05);

    if white_contrast >= dark_contrast {
        egui::Color32::WHITE
    } else {
        dark_text
    }
}

struct NslApp {
    db: database::Database,
    base_dir: PathBuf,
    stats: database::LibraryStats,
    rows: Vec<library::GameFile>,
    folders: Vec<String>,
    config: database::Config,
    selected: Option<i64>,
    detail_tab: usize,
    current_page: usize,
    search: String,
    search_focus: bool,
    kind_filter: String,
    folder_filter: String,
    view_mode: String,
    settings_open: bool,
    settings_category: usize,
    settings_folder_selected: Option<usize>,
    reset_panel_width: bool,
    status: String,
    scan_result: Option<Receiver<std::result::Result<ScanOutcome, String>>>,
    scan_cancel: Option<Arc<AtomicBool>>,
    scan_progress: Option<Arc<ScanProgress>>,
    titledb_result: Option<Receiver<std::result::Result<usize, String>>>,
    titledb_info: Option<String>,
    clear_titledb_confirm: bool,
    clear_library_confirm: bool,
    remove_folder_confirm: Option<String>,
    folder_notice: Option<String>,
    orphan_cleanup_confirm: bool,
    orphan_cleanup_result: Option<Receiver<std::result::Result<(usize, usize), String>>>,
    reload_result: Option<Receiver<std::result::Result<Vec<library::GameFile>, String>>>,
    rename_preview: Option<(String, String)>,
    bulk_rename_preview: Option<Vec<(String, String)>>,
    bulk_rename_result: Option<Receiver<std::result::Result<(usize, usize), String>>>,
    sort_column: String,
    sort_desc: bool,
    media_result: Option<Receiver<std::result::Result<(String, usize), String>>>,
    icon_textures: HashMap<String, egui::TextureHandle>,
    icon_lru: VecDeque<String>,
    icon_missing: HashSet<String>,
    screenshot_textures: HashMap<String, egui::TextureHandle>,
    texture_selection: Option<i64>,
    icon_dir: PathBuf,
    screenshot_dir: PathBuf,
    active_theme: String,
    translate_result: Option<Receiver<(i64, std::result::Result<String, String>)>>,
    translated_description: Option<(i64, String)>,
    translation_error: Option<(i64, String)>,
    decompress_result: Option<Receiver<std::result::Result<PathBuf, String>>>,
    converted_file: Option<PathBuf>,
    launch_after_decompress: bool,
    pending_icon_prefetch: bool,
    pending_screenshot_prefetch: bool,
    update_check_result: Option<Receiver<std::result::Result<updater::ReleaseInfo, String>>>,
    update_info: Option<updater::ReleaseInfo>,
    update_prepare_result: Option<Receiver<std::result::Result<PathBuf, String>>>,
    update_ready: Option<PathBuf>,
    cache_info: Option<String>,
    shortcuts_open: bool,
    about_open: bool,
    exit_confirm: bool,
    exit_confirmed: bool,
    log_sender: Sender<String>,
    log_receiver: Receiver<String>,
    log_lines: Vec<String>,
    log_open: bool,
    was_working: bool,
    resource_sample: ResourceSample,
    resource_clock: Option<(std::time::Instant, u64)>,
    resource_refresh_at: Option<std::time::Instant>,
}

fn path_is_within(path: &str, folder: &str) -> bool {
    let normalize = |value: &str| {
        value
            .replace('/', "\\")
            .trim_end_matches('\\')
            .to_ascii_lowercase()
    };
    let path = normalize(path);
    let folder = normalize(folder);
    path == folder || path.starts_with(&(folder + "\\"))
}

fn settings_row_label(ui: &mut egui::Ui, text: &str) {
    ui.add_sized(
        [200.0, ui.spacing().interact_size.y],
        egui::Label::new(text).halign(egui::Align::Min),
    );
}

fn normalize_scan_path_text(path: &str) -> String {
    #[cfg(windows)]
    {
        let mut normalized = path.replace('/', "\\");
        if normalized.as_bytes().get(1) == Some(&b':') {
            if let Some(drive) = normalized.chars().next() {
                normalized.replace_range(0..1, &drive.to_ascii_uppercase().to_string());
            }
        }
        normalized
    }
    #[cfg(not(windows))]
    {
        path.to_owned()
    }
}

fn normalize_scan_path(path: &std::path::Path) -> String {
    normalize_scan_path_text(&path.to_string_lossy())
}

fn cached_record_for_path<'a>(
    records: &'a HashMap<String, database::FileRecord>,
    path: &str,
) -> Option<&'a database::FileRecord> {
    records
        .get(path)
        .filter(|record| !record.fast_hash.is_empty())
}

fn texture_from_image(
    ctx: &egui::Context,
    name: &str,
    image: image::DynamicImage,
    max_edge: u32,
) -> Option<egui::TextureHandle> {
    let thumb = image.thumbnail(max_edge, max_edge).to_rgba8();
    if thumb.width() == 0 || thumb.height() == 0 {
        return None;
    }
    let size = [thumb.width() as usize, thumb.height() as usize];
    let color = egui::ColorImage::from_rgba_unmultiplied(size, thumb.as_raw());
    Some(ctx.load_texture(name, color, Default::default()))
}

impl NslApp {
    fn ensure_icon_texture(&mut self, ctx: &egui::Context, title_id: &str, budget: &mut usize) {
        if title_id.is_empty() || self.icon_textures.contains_key(title_id) {
            return;
        }
        if *budget == 0 || self.icon_missing.contains(title_id) {
            return;
        }
        let path = self.icon_dir.join(format!("{title_id}.png"));
        let Ok(img) = image::open(&path) else {
            // Remember misses so the grid does not stat/open the same file every frame.
            self.icon_missing.insert(title_id.to_owned());
            return;
        };
        *budget -= 1;
        if let Some(texture) = texture_from_image(ctx, "game_icon", img, 160) {
            self.icon_textures.insert(title_id.to_owned(), texture);
            self.icon_lru.push_back(title_id.to_owned());
            self.trim_icon_cache();
        }
    }
    fn trim_icon_cache(&mut self) {
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
    fn touch_icon(&mut self, title_id: &str) {
        if self.icon_textures.contains_key(title_id) {
            self.icon_lru.retain(|id| id != title_id);
            self.icon_lru.push_back(title_id.to_owned());
        }
    }
    fn invalidate_icons(&mut self) {
        self.icon_textures.clear();
        self.icon_lru.clear();
        self.icon_missing.clear();
    }
    fn resource_usage_text(&self) -> String {
        if self.resource_sample.working_set_bytes == 0 {
            return String::new();
        }
        format!(
            "RAM: {:.1} MB  |  CPU: {:.1}%",
            self.resource_sample.working_set_bytes as f64 / (1024.0 * 1024.0),
            self.resource_sample.cpu_percent
        )
    }
    fn refresh_resource_sample(&mut self, ctx: &egui::Context) {
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
    fn new() -> Result<Self> {
        Self::with_base_dir(common::base_dir())
    }
    fn with_base_dir(base_dir: PathBuf) -> Result<Self> {
        let db = database::Database::open(base_dir.join("library.db"))?;
        db.init()?;
        let stats = db.stats()?;
        let config = database::load_config(&base_dir.join("config.json"))?;
        let (log_sender, log_receiver) = mpsc::channel();
        let view_mode = config
            .settings
            .get("view_mode")
            .and_then(|v| v.as_str())
            .filter(|v| *v == "grid")
            .unwrap_or("list")
            .to_owned();
        let saved_folder_filter = config
            .settings
            .get("last_tab_key")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_owned();
        let folders = config.scan_folders.clone();
        let detail_tab = config
            .settings
            .get("last_selected_tab")
            .and_then(|v| v.as_str())
            .and_then(|v| v.parse::<usize>().ok())
            .filter(|v| *v < 4)
            .unwrap_or(0);
        let folder_filter = if saved_folder_filter == "MISSING_UPDATES"
            || saved_folder_filter == "MISSING_DLCS"
            || folders.contains(&saved_folder_filter)
        {
            saved_folder_filter
        } else {
            String::new()
        };
        let icon_dir = base_dir.join("icons");
        let screenshot_dir = base_dir.join("screenshots");
        std::fs::create_dir_all(&icon_dir)?;
        std::fs::create_dir_all(&screenshot_dir)?;
        let mut app = Self {
            db,
            base_dir: base_dir.clone(),
            stats,
            rows: Vec::new(),
            folders,
            config,
            selected: None,
            detail_tab,
            current_page: 0,
            search: String::new(),
            search_focus: false,
            kind_filter: "base".into(),
            folder_filter,
            view_mode,
            settings_open: false,
            settings_category: 0,
            settings_folder_selected: None,
            reset_panel_width: false,
            status: "대기".into(),
            scan_result: None,
            scan_cancel: None,
            scan_progress: None,
            titledb_result: None,
            titledb_info: None,
            clear_titledb_confirm: false,
            clear_library_confirm: false,
            remove_folder_confirm: None,
            folder_notice: None,
            orphan_cleanup_confirm: false,
            orphan_cleanup_result: None,
            reload_result: None,
            rename_preview: None,
            bulk_rename_preview: None,
            bulk_rename_result: None,
            sort_column: "name".into(),
            sort_desc: false,
            media_result: None,
            icon_textures: HashMap::new(),
            icon_lru: VecDeque::new(),
            icon_missing: HashSet::new(),
            screenshot_textures: HashMap::new(),
            texture_selection: None,
            icon_dir,
            screenshot_dir,
            active_theme: String::new(),
            translate_result: None,
            translated_description: None,
            translation_error: None,
            decompress_result: None,
            converted_file: None,
            launch_after_decompress: false,
            pending_icon_prefetch: false,
            pending_screenshot_prefetch: false,
            update_check_result: None,
            update_info: None,
            update_prepare_result: None,
            update_ready: None,
            cache_info: None,
            shortcuts_open: false,
            about_open: false,
            exit_confirm: false,
            exit_confirmed: false,
            log_sender,
            log_receiver,
            log_lines: Vec::new(),
            log_open: false,
            was_working: false,
            resource_sample: ResourceSample::default(),
            resource_clock: None,
            resource_refresh_at: None,
        };
        app.start_reload();
        // A packaged build ships the title database files, so match them on first
        // launch instead of showing "DB: 0" until the user clicks DB 매칭.
        if app.stats.title_db_count == 0
            && app.base_dir.join("titledb").join("titles.json").is_file()
        {
            app.start_local_titledb_import();
        }
        Ok(app)
    }
    fn start_reload(&mut self) {
        if self.reload_result.is_some() {
            return;
        }
        let mode = self
            .config
            .settings
            .get("name_mode")
            .and_then(|v| v.as_str())
            .unwrap_or("db")
            .to_owned();
        let db = self.db.clone();
        let (tx, rx) = mpsc::channel();
        self.reload_result = Some(rx);
        std::thread::spawn(move || {
            let _ = tx.send(db.files(&mode).map_err(|e| e.to_string()));
        });
    }
    fn start_scan(&mut self, incremental: bool) {
        if self.scan_result.is_some() {
            return;
        }
        if self.folders.is_empty() {
            self.settings_open = true;
            self.status = "먼저 설정에서 스캔 폴더를 추가하세요".into();
            let _ = self
                .log_sender
                .send("⚠ 스캔을 시작하지 못했습니다 · 스캔 폴더 없음".into());
            return;
        }
        if !scanner::has_prod_keys() {
            self.status =
                "prod.keys를 프로그램 폴더 또는 %USERPROFILE%\\.switch 폴더에 넣어주세요".into();
            let _ = self
                .log_sender
                .send("❌ 스캔을 시작하지 못했습니다 · prod.keys 없음".into());
            return;
        }
        let folders = self.folders.clone();
        let db = self.db.clone();
        let known = db
            .known_files()
            .unwrap_or_default()
            .into_iter()
            .map(|(path, record)| (normalize_scan_path_text(&path), record))
            .collect::<HashMap<_, _>>();
        let cached_records = db
            .cached_file_records()
            .unwrap_or_default()
            .into_iter()
            .map(|(path, mut record)| {
                let path = normalize_scan_path_text(&path);
                record.path = path.clone();
                (path, record)
            })
            .collect::<HashMap<_, _>>();
        let cancel = Arc::new(AtomicBool::new(false));
        let progress = Arc::new(ScanProgress {
            completed: AtomicUsize::new(0),
            total: AtomicUsize::new(0),
        });
        self.scan_cancel = Some(cancel.clone());
        self.scan_progress = Some(progress.clone());
        let default_threads = std::thread::available_parallelism()
            .map(|n| n.get().min(16))
            .unwrap_or(8);
        let scan_threads = self
            .config
            .settings
            .get("max_scan_threads")
            .and_then(|v| v.as_str())
            .and_then(|s| s.parse::<usize>().ok())
            .filter(|n| *n <= 20)
            .unwrap_or(default_threads);
        let (tx, rx) = mpsc::channel();
        self.scan_result = Some(rx);
        self.status = "파일 검색 및 분석 중…".into();
        let log = self.log_sender.clone();
        std::thread::spawn(move || {
            let _ = log.send(format!(
                "🚀 {} 시작 · 폴더 {}개",
                if incremental {
                    "빠른 스캔"
                } else {
                    "전체 스캔"
                },
                folders.len()
            ));
            let result = (|| -> Result<ScanOutcome> {
                let mut discovered = Vec::new();
                let mut existing = Vec::new();
                let mut skipped = 0usize;
                let mut candidates = Vec::new();
                let mut scanned_roots: Vec<String> = Vec::new();
                'folders: for folder in &folders {
                    if cancel.load(Ordering::Relaxed) {
                        break;
                    }
                    let root = PathBuf::from(folder);
                    if !root.exists() {
                        let _ = log.send(format!(
                            "⚠ 폴더에 접근할 수 없습니다 · {folder} (기록은 유지됩니다)"
                        ));
                        continue;
                    }
                    scanned_roots.push(normalize_scan_path(&root));
                    let ignored = [
                        "saves",
                        "save",
                        "mods",
                        "mod",
                        "atmosphere",
                        "sxos",
                        "hekate",
                        "bootloader",
                        ".git",
                        "node_modules",
                        "__pycache__",
                        "temp",
                        "tmp",
                    ];
                    for entry in walkdir::WalkDir::new(&root)
                        .follow_links(false)
                        .into_iter()
                        .filter_entry(|e| {
                            e.depth() == 0
                                || !e.file_type().is_dir()
                                || !ignored.contains(
                                    &e.file_name()
                                        .to_string_lossy()
                                        .to_ascii_lowercase()
                                        .as_str(),
                                )
                        })
                        .filter_map(Result::ok)
                    {
                        if cancel.load(Ordering::Relaxed) {
                            break 'folders;
                        }
                        if !entry.file_type().is_file() {
                            continue;
                        }
                        let ext = entry
                            .path()
                            .extension()
                            .and_then(|s| s.to_str())
                            .unwrap_or("")
                            .to_ascii_lowercase();
                        if !["nsp", "nsz", "xci", "xcz"].contains(&ext.as_str()) {
                            continue;
                        }
                        let Ok(meta) = entry.metadata() else { continue };
                        let path = normalize_scan_path(entry.path());
                        let mtime = meta
                            .modified()
                            .ok()
                            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                            .map(|d| d.as_secs_f64());
                        existing.push(path.clone());
                        if incremental {
                            if let Some((old_mtime, old_size, _)) = known.get(&path) {
                                if *old_size == meta.len() as i64
                                    && old_mtime
                                        .zip(mtime)
                                        .map(|(a, b)| (a - b).abs() < 1.0)
                                        .unwrap_or(false)
                                {
                                    skipped += 1;
                                    continue;
                                }
                            }
                        }
                        let filename = entry.file_name().to_string_lossy().into_owned();
                        candidates.push((
                            PathBuf::from(&path),
                            path,
                            filename,
                            meta.len() as i64,
                            mtime,
                            ext,
                            normalize_scan_path(&root),
                        ));
                    }
                }
                progress
                    .total
                    .store(candidates.len() + skipped, Ordering::Relaxed);
                progress.completed.store(skipped, Ordering::Relaxed);
                let _ = log.send(format!(
                    "📋 대상 {}개 · 캐시 재사용 {}개",
                    candidates.len() + skipped,
                    skipped
                ));
                if !candidates.is_empty() {
                    let threads = if scan_threads == 0 {
                        default_threads.max(1)
                    } else {
                        scan_threads.max(1)
                    };
                    let pool = rayon::ThreadPoolBuilder::new()
                        .num_threads(threads)
                        .build()?;
                    let results = pool.install(|| {
                        candidates
                            .par_iter()
                            .map(|(source, path, filename, size, mtime, ext, folder)| {
                                let cached_record = cached_record_for_path(&cached_records, path);
                                let same_metadata =
                                    known.get(path).map(|(old_mtime, old_size, _)| {
                                        *old_size == *size
                                            && old_mtime
                                                .zip(*mtime)
                                                .map(|(old, current)| (old - current).abs() < 1.0)
                                                .unwrap_or(false)
                                    }) == Some(true);
                                let result = if cancel.load(Ordering::Relaxed) {
                                    (None, false)
                                } else if cached_record.is_some() && same_metadata {
                                    (None, true)
                                } else if let Ok(hash) = scanner::fast_hash(source) {
                                    if let (Some(old), Some((_, _, old_hash))) =
                                        (cached_record, known.get(path))
                                    {
                                        if !old_hash.is_empty() && *old_hash == hash {
                                            let mut record = old.clone();
                                            record.path = path.clone();
                                            record.filename = filename.clone();
                                            record.size = *size;
                                            record.mtime = *mtime;
                                            record.file_type = ext.clone();
                                            record.scan_folder = folder.clone();
                                            record.fast_hash = hash;
                                            (Some(record), true)
                                        } else if let Ok(scanned) =
                                            scanner::scan_file_hashed(source, filename, hash)
                                        {
                                            (
                                                Some(database::FileRecord {
                                                    path: path.clone(),
                                                    filename: filename.clone(),
                                                    size: *size,
                                                    mtime: *mtime,
                                                    file_type: ext.clone(),
                                                    scan_folder: folder.clone(),
                                                    fast_hash: scanned.fast_hash,
                                                    nacp_ko: scanned.nacp_ko,
                                                    nacp_en: scanned.nacp_en,
                                                    nacp_ja: scanned.nacp_ja,
                                                    nacp_version: scanned.nacp_version,
                                                    contents: scanned.contents,
                                                }),
                                                false,
                                            )
                                        } else {
                                            (None, false)
                                        }
                                    } else if let Ok(scanned) =
                                        scanner::scan_file_hashed(source, filename, hash)
                                    {
                                        (
                                            Some(database::FileRecord {
                                                path: path.clone(),
                                                filename: filename.clone(),
                                                size: *size,
                                                mtime: *mtime,
                                                file_type: ext.clone(),
                                                scan_folder: folder.clone(),
                                                fast_hash: scanned.fast_hash,
                                                nacp_ko: scanned.nacp_ko,
                                                nacp_en: scanned.nacp_en,
                                                nacp_ja: scanned.nacp_ja,
                                                nacp_version: scanned.nacp_version,
                                                contents: scanned.contents,
                                            }),
                                            false,
                                        )
                                    } else {
                                        (None, false)
                                    }
                                } else {
                                    (None, false)
                                };
                                match &result {
                                    (Some(record), was_cached) => {
                                        let content_ids = record
                                            .contents
                                            .iter()
                                            .filter_map(|content| content.title_id.as_deref())
                                            .collect::<Vec<_>>()
                                            .join(", ");
                                        let _ = log.send(format!(
                                            "{} {}{}",
                                            if *was_cached { "⚡" } else { "✅" },
                                            filename,
                                            if content_ids.is_empty() {
                                                String::new()
                                            } else {
                                                format!(" · {content_ids}")
                                            }
                                        ));
                                    }
                                    (None, true) => {
                                        let _ = log.send(format!("⚡ 캐시 사용 · {filename}"));
                                    }
                                    (None, false) if !cancel.load(Ordering::Relaxed) => {
                                        let _ = log.send(format!("⚠ 분석 실패 · {filename}"));
                                    }
                                    _ => {}
                                }
                                progress.completed.fetch_add(1, Ordering::Relaxed);
                                result
                            })
                            .collect::<Vec<_>>()
                    });
                    for (record, was_skipped) in results {
                        skipped += usize::from(was_skipped);
                        if let Some(record) = record {
                            discovered.push(record);
                        }
                    }
                }
                let count = discovered.len();
                if folders.is_empty() {
                    return Ok((0, 0, false));
                }
                db.upsert_files(&discovered)?;
                let was_cancelled = cancel.load(Ordering::Relaxed);
                let removed = if was_cancelled {
                    0
                } else {
                    db.delete_missing_files_within(&existing, &scanned_roots)?
                };
                let _ = log.send(format!(
                    "💾 DB 반영 완료 · 새 기록 {}개 · 누락 제거 {}개{}",
                    count,
                    removed,
                    if was_cancelled {
                        " · 취소된 스캔"
                    } else {
                        ""
                    }
                ));
                Ok((count + skipped, removed, was_cancelled))
            })()
            .map_err(|e| e.to_string());
            if let Err(error) = &result {
                let _ = log.send(format!("❌ 스캔 오류 · {error}"));
            }
            let _ = tx.send(result);
        });
    }
    fn start_titledb_download(&mut self) {
        if self.titledb_result.is_some() {
            return;
        }
        let dir = self.base_dir.join("titledb");
        let db = self.db.clone();
        let source = self
            .config
            .settings
            .get("titledb_source")
            .and_then(|v| v.as_str())
            .unwrap_or("custom")
            .to_owned();
        let (tx, rx) = mpsc::channel();
        self.titledb_result = Some(rx);
        self.status = "타이틀 DB 다운로드 중…".into();
        let log = self.log_sender.clone();
        std::thread::spawn(move || {
            let result = titledb::download_full(&dir, &source)
                .and_then(|entries| {
                    db.load_titles(&entries)?;
                    Ok(entries.len())
                })
                .map_err(|e| e.to_string());
            let _ = log.send(match &result {
                Ok(count) => format!("✅ 타이틀 DB 다운로드 및 반영 · {count}개 항목"),
                Err(error) => format!("❌ 타이틀 DB 다운로드 실패 · {error}"),
            });
            let _ = tx.send(result);
        });
    }
    fn start_local_titledb_import(&mut self) {
        if self.titledb_result.is_some() {
            return;
        }
        let dir = self.base_dir.join("titledb");
        let db = self.db.clone();
        let (tx, rx) = mpsc::channel();
        self.titledb_result = Some(rx);
        self.status = "로컬 타이틀 DB 읽는 중…".into();
        let log = self.log_sender.clone();
        std::thread::spawn(move || {
            let result = titledb::load_local(&dir)
                .and_then(|entries| {
                    db.load_titles(&entries)?;
                    Ok(entries.len())
                })
                .map_err(|e| e.to_string());
            let _ = log.send(match &result {
                Ok(count) => format!("✅ 로컬 타이틀 DB 매칭 · {count}개 항목"),
                Err(error) => format!("❌ 로컬 타이틀 DB 매칭 실패 · {error}"),
            });
            let _ = tx.send(result);
        });
    }
    fn start_media_download(&mut self, screenshots: bool, selected: Option<&library::GameFile>) {
        if self.media_result.is_some() {
            return;
        }
        let items: Vec<(String, String, String)> = if let Some(row) = selected {
            vec![(
                row.title_id.clone(),
                if screenshots {
                    row.screenshots.clone()
                } else {
                    row.icon_url.clone()
                },
                row.path.clone(),
            )]
        } else {
            let mut map = std::collections::BTreeMap::new();
            for r in &self.rows {
                if r.title_id.is_empty() {
                    continue;
                }
                let value = if screenshots {
                    &r.screenshots
                } else {
                    &r.icon_url
                };
                if screenshots && (value.is_empty() || value == "[]") {
                    continue;
                }
                if !screenshots && value.is_empty() && r.path.is_empty() {
                    continue;
                }
                map.entry(r.title_id.clone())
                    .or_insert_with(|| (value.clone(), r.path.clone()));
            }
            map.into_iter()
                .map(|(tid, (url, path))| (tid, url, path))
                .collect()
        };
        if items.is_empty() {
            self.status = if screenshots {
                "다운로드할 스크린샷이 없습니다"
            } else {
                "다운로드할 아이콘이 없습니다"
            }
            .into();
            return;
        }
        let cache = if screenshots {
            self.screenshot_dir.clone()
        } else {
            self.icon_dir.clone()
        };
        let label = if screenshots {
            "스크린샷"
        } else {
            "아이콘"
        }
        .to_owned();
        let log = self.log_sender.clone();
        let (tx, rx) = mpsc::channel();
        self.media_result = Some(rx);
        self.status = format!("{label} 다운로드 중…");
        std::thread::spawn(move || {
            let n = if screenshots {
                media::download_screenshots(&cache, &items)
            } else {
                media::download_icons(&cache, &items)
            };
            let _ = log.send(format!("✅ {label} 작업 완료 · {n}개 저장"));
            let _ = tx.send(Ok((label, n)));
        });
    }
    fn start_pending_media(&mut self) {
        if self.media_result.is_some() {
            return;
        }
        if self.pending_icon_prefetch {
            self.pending_icon_prefetch = false;
            self.start_media_download(false, None);
        } else if self.pending_screenshot_prefetch {
            self.pending_screenshot_prefetch = false;
            self.start_media_download(true, None);
        }
    }
    fn start_decompress(&mut self, src: PathBuf) {
        if self.decompress_result.is_some() {
            return;
        }
        let root = self.base_dir.join("workspaces");
        let log = self.log_sender.clone();
        let (tx, rx) = mpsc::channel();
        self.decompress_result = Some(rx);
        self.status = format!(
            "압축 해제 중: {}",
            src.file_name().unwrap_or_default().to_string_lossy()
        );
        std::thread::spawn(move || {
            let result = decompress::decompress(&src, &root).map_err(|e| e.to_string());
            let _ = log.send(match &result {
                Ok(path) => format!("✅ 압축 해제 완료 · {}", path.display()),
                Err(error) => format!("❌ 압축 해제 실패 · {error}"),
            });
            let _ = tx.send(result);
        });
    }
    fn launch_path(&mut self, path: &PathBuf) -> Result<()> {
        anyhow::ensure!(path.is_file(), "실행할 파일을 찾지 못했습니다");
        let enabled = self
            .config
            .settings
            .get("launcher_enabled")
            .and_then(|v| v.as_str())
            .unwrap_or("0")
            == "1";
        let launcher = self
            .config
            .settings
            .get("launcher_path")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_owned();
        if enabled && !launcher.is_empty() {
            let raw = self
                .config
                .settings
                .get("launcher_args")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let has_path = raw.contains("{path}");
            let expanded = raw.replace("{path}", &path.to_string_lossy());
            let args = shlex::split(&expanded)
                .ok_or_else(|| anyhow::anyhow!("실행 인자를 해석할 수 없습니다"))?;
            let mut cmd = std::process::Command::new(&launcher);
            cmd.args(args);
            if !has_path {
                cmd.arg(path);
            }
            cmd.spawn()?;
        } else {
            #[cfg(windows)]
            {
                std::process::Command::new("explorer").arg(path).spawn()?;
            }
            #[cfg(target_os = "macos")]
            {
                std::process::Command::new("open").arg(path).spawn()?;
            }
            #[cfg(all(unix, not(target_os = "macos")))]
            {
                std::process::Command::new("xdg-open").arg(path).spawn()?;
            }
        }
        self.status = format!(
            "실행: {}",
            path.file_name().unwrap_or_default().to_string_lossy()
        );
        Ok(())
    }
    fn run_file(&mut self, row: &library::GameFile) {
        let ext = row.file_type.to_ascii_lowercase();
        let auto = self
            .config
            .settings
            .get("decompress_auto")
            .and_then(|v| v.as_str())
            .unwrap_or("0")
            == "1";
        if auto && (ext == "nsz" || ext == "xcz") {
            self.launch_after_decompress = true;
            self.start_decompress(PathBuf::from(&row.path));
            return;
        }
        if let Err(e) = self.launch_path(&PathBuf::from(&row.path)) {
            self.status = format!("실행 실패: {e}")
        }
    }
    fn row_context_menu(&mut self, ui: &mut egui::Ui, row: &library::GameFile) {
        self.selected = Some(row.id);
        if ui.button("🎮  실행").clicked() {
            self.run_file(row);
            ui.close_menu();
        }
        if ui.button("📂  폴더에서 열기").clicked() {
            let _ = std::process::Command::new("explorer")
                .arg("/select,")
                .arg(&row.path)
                .spawn();
            ui.close_menu();
        }
        if ui.button("📄  경로 복사").clicked() {
            ui.ctx().copy_text(row.path.clone());
            self.status = "경로를 클립보드에 복사했습니다".into();
            ui.close_menu();
        }
        ui.separator();
        if ui.button("✏️  파일명 정규화").clicked() {
            match self.rename_target(row) {
                Ok(target) => {
                    self.rename_preview =
                        Some((row.path.clone(), target.to_string_lossy().into_owned()));
                }
                Err(e) => self.status = e.to_string(),
            }
            ui.close_menu();
        }
        ui.separator();
        if ui.button("📸  스크린샷 다운로드").clicked() {
            self.start_media_download(true, Some(row));
            ui.close_menu();
        }
        ui.separator();
        if ui.button("🖼  아이콘 스캔").clicked() {
            self.start_media_download(false, Some(row));
            ui.close_menu();
        }
    }
    fn start_translation(&mut self, row: &library::GameFile) {
        if self.translate_result.is_some() {
            return;
        }
        let id = row.id;
        let desc = row.description.clone();
        if desc.is_empty() {
            self.status = "번역할 설명이 없습니다".into();
            return;
        }
        let (tx, rx) = mpsc::channel();
        self.translate_result = Some(rx);
        self.translation_error = None;
        self.status = "게임 설명 번역 중…".into();
        std::thread::spawn(move || {
            let _ = tx.send((id, translate::to_korean(&desc).map_err(|e| e.to_string())));
        });
    }
    fn rename_target(&self, row: &library::GameFile) -> Result<PathBuf> {
        let old = PathBuf::from(&row.path);
        if !old.is_file() {
            anyhow::bail!("게임 파일을 찾을 수 없습니다")
        }
        let ext = old
            .extension()
            .and_then(|x| x.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        if !["nsp", "nsz", "xci", "xcz"].contains(&ext.as_str()) {
            anyhow::bail!("지원하지 않는 게임 파일 형식입니다")
        }
        let contents = self.db.file_contents(&row.path)?;
        let base = contents
            .iter()
            .find(|c| c.content_type == "base")
            .ok_or_else(|| anyhow::anyhow!("베이스 게임 정보를 찾지 못했습니다"))?;
        let tid = base.title_id.clone().unwrap_or_default();
        if tid.len() != 16 || !tid.chars().all(|c| c.is_ascii_hexdigit()) {
            anyhow::bail!("베이스 타이틀 ID를 찾지 못했습니다")
        }
        let updates: Vec<i64> = contents
            .iter()
            .filter(|c| c.content_type == "update")
            .map(|c| c.version)
            .collect();
        let dlcs: std::collections::HashSet<String> = contents
            .iter()
            .filter(|c| c.content_type == "dlc")
            .filter_map(|c| c.title_id.clone())
            .collect();
        let mut title = row.title_name.clone();
        for bad in ['\\', '/', '*', '?', ':', '"', '<', '>', '|'] {
            title = title.replace(bad, "");
        }
        title = title.trim().trim_end_matches([' ', '.']).to_owned();
        if title.is_empty() {
            anyhow::bail!("게임 이름을 결정할 수 없습니다")
        }
        let name = if updates.is_empty() && dlcs.is_empty() {
            format!(
                "{title} [{}][v{}][Base].{ext}",
                tid.to_ascii_uppercase(),
                base.version
            )
        } else {
            let mut tags = vec!["B".to_owned()];
            if let Some(v) = updates.iter().max() {
                tags.push(format!("U{v}"));
            }
            if !dlcs.is_empty() {
                tags.push(format!("{}DLC", dlcs.len()));
            }
            format!("{title} [{}].{ext}", tags.join("+"))
        };
        Ok(old.with_file_name(name))
    }
    fn row_matches_current_filter(&self, row: &library::GameFile) -> bool {
        let exception_update = self
            .config
            .settings
            .get("exception_update")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let exception_dlc = self
            .config
            .settings
            .get("exception_dlc")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if self.folder_filter == "MISSING_UPDATES" {
            return row.has_base
                && row.latest_version > 0
                && row.version.max(row.update_version) < row.latest_version
                && !exception_update
                    .split(',')
                    .any(|id| id.trim().eq_ignore_ascii_case(&row.title_id));
        }
        if self.folder_filter == "MISSING_DLCS" {
            return row.has_base
                && row.missing_dlcs.iter().any(|(id, _)| {
                    !exception_dlc
                        .split(',')
                        .any(|excluded| excluded.trim().eq_ignore_ascii_case(id))
                });
        }
        if !self.folder_filter.is_empty() {
            return row.has_base && path_is_within(&row.path, &self.folder_filter);
        }
        match self.kind_filter.as_str() {
            "base" => row.has_base,
            "update" => row.has_update,
            "dlc" => row.dlc_count > 0,
            "unknown" => !row.has_base && !row.has_update && row.dlc_count == 0,
            "missing_updates" => {
                row.has_base
                    && row.latest_version > 0
                    && row.version.max(row.update_version) < row.latest_version
                    && !exception_update
                        .split(',')
                        .any(|id| id.trim().eq_ignore_ascii_case(&row.title_id))
            }
            "missing_dlcs" => {
                row.has_base
                    && row.missing_dlcs.iter().any(|(id, _)| {
                        !exception_dlc
                            .split(',')
                            .any(|excluded| excluded.trim().eq_ignore_ascii_case(id))
                    })
            }
            _ => true,
        }
    }
    fn missing_update_count(&self) -> usize {
        self.rows
            .iter()
            .filter(|row| row.has_base && self.row_is_missing_update(row))
            .count()
    }
    fn row_is_missing_update(&self, row: &library::GameFile) -> bool {
        let excluded = self
            .config
            .settings
            .get("exception_update")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        row.latest_version > 0
            && row.version.max(row.update_version) < row.latest_version
            && !excluded
                .split(',')
                .any(|id| id.trim().eq_ignore_ascii_case(&row.title_id))
    }
    fn row_has_missing_dlc(&self, row: &library::GameFile) -> bool {
        let excluded = self
            .config
            .settings
            .get("exception_dlc")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        row.missing_dlcs.iter().any(|(id, _)| {
            !excluded
                .split(',')
                .any(|value| value.trim().eq_ignore_ascii_case(id))
        })
    }
    fn missing_dlc_count(&self) -> usize {
        self.rows
            .iter()
            .filter(|row| row.has_base && self.row_has_missing_dlc(row))
            .count()
    }
    fn duplicate_title_groups(&self) -> Vec<(String, String, Vec<String>)> {
        let mut grouped: HashMap<String, Vec<&library::GameFile>> = HashMap::new();
        for row in self.rows.iter().filter(|row| row.has_base) {
            if row.title_id.len() == 16 {
                grouped
                    .entry(row.title_id.to_ascii_uppercase())
                    .or_default()
                    .push(row);
            }
        }
        let mut duplicates = grouped
            .into_iter()
            .filter_map(|(title_id, rows)| {
                let mut unique = HashMap::<String, String>::new();
                for row in rows {
                    unique
                        .entry(row.path.to_ascii_lowercase())
                        .or_insert_with(|| row.path.clone());
                }
                if unique.len() < 2 {
                    return None;
                }
                let mut paths = unique.into_values().collect::<Vec<_>>();
                paths.sort_by_key(|path| path.to_ascii_lowercase());
                let name = self
                    .rows
                    .iter()
                    .find(|row| row.title_id.eq_ignore_ascii_case(&title_id))
                    .map(|row| row.title_name.clone())
                    .unwrap_or_default();
                Some((title_id, name, paths))
            })
            .collect::<Vec<_>>();
        duplicates.sort_by_key(|(_, name, id_paths)| {
            name.to_lowercase() + id_paths.first().map(String::as_str).unwrap_or("")
        });
        duplicates
    }
    fn preview_bulk_rename(&mut self) {
        if self.bulk_rename_result.is_some() || self.bulk_rename_preview.is_some() {
            return;
        }
        let mut changes = Vec::new();
        let mut unprocessable = 0usize;
        for row in library::visible(&self.rows, &self.search) {
            if !self.row_matches_current_filter(row) {
                continue;
            }
            match self.rename_target(row) {
                Ok(path) if path.to_string_lossy() != row.path => {
                    changes.push((row.path.clone(), path.to_string_lossy().into_owned()));
                }
                Ok(_) => {}
                Err(_) => unprocessable += 1,
            }
        }
        if changes.is_empty() {
            self.status = format!("정규화할 파일이 없습니다 (분석 불가 {unprocessable}개)");
        } else {
            self.status = format!(
                "변경 예정 {}개, 분석 불가 {}개",
                changes.len(),
                unprocessable
            );
            self.bulk_rename_preview = Some(changes);
        }
    }
    fn start_bulk_rename(&mut self, changes: Vec<(String, String)>) {
        if self.bulk_rename_result.is_some() {
            return;
        }
        let db = self.db.clone();
        let (tx, rx) = mpsc::channel();
        self.bulk_rename_result = Some(rx);
        self.status = format!("파일명 일괄 정규화 중: {}개", changes.len());
        let log = self.log_sender.clone();
        std::thread::spawn(move || {
            let mut renamed = 0usize;
            let mut failed = 0usize;
            for (from, to) in changes {
                if PathBuf::from(&to).exists() || std::fs::rename(&from, &to).is_err() {
                    failed += 1;
                    continue;
                }
                if db.rename_file_path(&from, &to).is_ok() {
                    renamed += 1;
                } else {
                    let _ = std::fs::rename(&to, &from);
                    failed += 1;
                }
            }
            let _ = log.send(format!(
                "✏️ 파일명 일괄 정규화 완료 · 변경 {renamed}개 · 실패 {failed}개"
            ));
            let _ = tx.send(Ok((renamed, failed)));
        });
    }
    fn start_update_check(&mut self) {
        if self.update_check_result.is_some() {
            return;
        }
        let (tx, rx) = mpsc::channel();
        self.update_check_result = Some(rx);
        self.status = "업데이트 확인 중".into();
        let log = self.log_sender.clone();
        std::thread::spawn(move || {
            let result = updater::check_latest().map_err(|e| e.to_string());
            let _ = log.send(match &result {
                Ok(info) => format!("ℹ️ 릴리스 확인 완료 · 최신 v{}", info.latest_version),
                Err(error) => format!("❌ 업데이트 확인 실패 · {error}"),
            });
            let _ = tx.send(result);
        });
    }
    fn start_update_download(&mut self, url: String, version: String) {
        if self.update_prepare_result.is_some() {
            return;
        }
        let base_dir = self.base_dir.clone();
        let (tx, rx) = mpsc::channel();
        self.update_prepare_result = Some(rx);
        self.status = format!("Rust 업데이트 v{version} 다운로드 및 검증 중");
        let log = self.log_sender.clone();
        std::thread::spawn(move || {
            let result =
                updater::download_and_prepare(&url, &version, &base_dir).map_err(|e| e.to_string());
            let _ = log.send(match &result {
                Ok(_) => format!("✅ Rust 업데이트 v{version} 다운로드 및 검증 완료"),
                Err(error) => format!("❌ Rust 업데이트 준비 실패 · {error}"),
            });
            let _ = tx.send(result);
        });
    }
    fn start_orphan_cleanup(&mut self) {
        if self.orphan_cleanup_result.is_some() {
            return;
        }
        let db = self.db.clone();
        let icon_dir = self.icon_dir.clone();
        let screenshot_dir = self.screenshot_dir.clone();
        let (tx, rx) = mpsc::channel();
        self.orphan_cleanup_result = Some(rx);
        self.status = "사용하지 않는 아이콘/스크린샷 정리 중".into();
        let log = self.log_sender.clone();
        std::thread::spawn(move || {
            let result = (|| -> Result<(usize, usize)> {
                let active = db
                    .content_title_ids()?
                    .into_iter()
                    .map(|id| id.to_ascii_uppercase())
                    .collect::<std::collections::HashSet<_>>();
                let mut removed_icons = 0;
                let mut removed_screenshots = 0;
                if let Ok(root) = icon_dir.canonicalize() {
                    if let Ok(entries) = std::fs::read_dir(&root) {
                        for entry in entries.filter_map(Result::ok) {
                            let path = entry.path();
                            if path.extension().and_then(|v| v.to_str()) != Some("png") {
                                continue;
                            }
                            let id = path
                                .file_stem()
                                .and_then(|v| v.to_str())
                                .unwrap_or("")
                                .to_ascii_uppercase();
                            if !active.contains(&id)
                                && path
                                    .canonicalize()
                                    .ok()
                                    .and_then(|p| p.parent().map(PathBuf::from))
                                    == Some(root.clone())
                                && std::fs::remove_file(&path).is_ok()
                            {
                                removed_icons += 1;
                            }
                        }
                    }
                }
                if let Ok(root) = screenshot_dir.canonicalize() {
                    if let Ok(entries) = std::fs::read_dir(&root) {
                        for entry in entries.filter_map(Result::ok) {
                            let path = entry.path();
                            if !path.is_dir() {
                                continue;
                            }
                            let id = path
                                .file_name()
                                .and_then(|v| v.to_str())
                                .unwrap_or("")
                                .to_ascii_uppercase();
                            let safe_child = path
                                .canonicalize()
                                .ok()
                                .map(|target| target.parent() == Some(root.as_path()))
                                .unwrap_or(false);
                            if !active.contains(&id)
                                && safe_child
                                && std::fs::remove_dir_all(&path).is_ok()
                            {
                                removed_screenshots += 1;
                            }
                        }
                    }
                }
                Ok((removed_icons, removed_screenshots))
            })()
            .map_err(|e| e.to_string());
            let _ = log.send(match &result {
                Ok((icons, screenshots)) => {
                    format!(
                        "🧹 미디어 캐시 정리 완료 · 아이콘 {icons}개 · 스크린샷 {screenshots}개"
                    )
                }
                Err(error) => format!("❌ 미디어 캐시 정리 실패 · {error}"),
            });
            let _ = tx.send(result);
        });
    }
    fn export_csv(&mut self) {
        let Some(path) = rfd::FileDialog::new()
            .set_file_name("nsl-library.csv")
            .save_file()
        else {
            return;
        };
        let result = (|| -> Result<()> {
            let mut file = std::fs::File::create(path)?;
            std::io::Write::write_all(&mut file, &[0xEF, 0xBB, 0xBF])?;
            let mut w = csv::WriterBuilder::new()
                .has_headers(false)
                .from_writer(file);
            w.write_record([
                "게임 이름",
                "타이틀 (KO)",
                "타이틀 (EN)",
                "타이틀 (JA)",
                "타이틀 ID",
                "종류",
                "보유 버전",
                "업데이트 버전",
                "최신 버전",
                "상태",
                "크기",
                "형식",
                "DLC 보유",
                "DB DLC 수",
                "언어",
                "지역",
                "파일명",
                "파일 경로",
            ])?;
            for r in &self.rows {
                let ver = if r.update_version != 0 {
                    r.update_version
                } else {
                    r.version
                };
                let status = if r.latest_version == 0 {
                    "미확인"
                } else if ver >= r.latest_version {
                    "최신"
                } else {
                    "업데이트 가능"
                };
                w.write_record([
                    r.title_name.as_str(),
                    r.nacp_ko.as_str(),
                    r.nacp_en.as_str(),
                    r.nacp_ja.as_str(),
                    r.title_id.as_str(),
                    r.content_type.as_str(),
                    &common::version_to_str(Some(r.version)),
                    &if r.update_version == 0 {
                        String::new()
                    } else {
                        common::version_to_str(Some(r.update_version))
                    },
                    &if r.latest_version == 0 {
                        String::new()
                    } else {
                        common::version_to_str(Some(r.latest_version))
                    },
                    status,
                    &common::format_size(r.size.max(0) as u64),
                    &r.file_type.to_ascii_uppercase(),
                    &r.dlc_count.to_string(),
                    &r.db_dlc_count.to_string(),
                    r.languages.as_str(),
                    r.region.as_str(),
                    r.filename.as_str(),
                    r.path.as_str(),
                ])?;
            }
            w.flush()?;
            Ok(())
        })();
        self.status = match result {
            Ok(()) => "CSV 내보내기 완료".into(),
            Err(e) => e.to_string(),
        };
    }
    fn cache_stats_text(&self) -> String {
        let known = match self.db.known_files() {
            Ok(files) => files,
            Err(e) => return format!("스캔 결과 캐시를 읽지 못했습니다: {e}"),
        };
        let mut lines = vec![
            "SQLite 라이브러리 기록을 스캔 결과 캐시로 재사용합니다.".to_owned(),
            format!("캐시된 파일: {}개", known.len()),
            String::new(),
        ];
        for folder in &self.folders {
            let count = known
                .keys()
                .filter(|path| path_is_within(path, folder))
                .count();
            lines.push(format!("{} · {count}개", folder));
        }
        lines.join("\n")
    }
    fn settings_panel_ui(&mut self, ui: &mut egui::Ui) {
        let categories = [
            (
                "스캔 폴더",
                "게임 파일을 스캔할 폴더를 지정합니다. NSP, NSZ, XCI, XCZ 형식을 지원합니다.",
            ),
            ("일반", "앱 동작과 라이브러리 표시 방식을 설정합니다."),
            (
                "테마",
                "앱 전체의 색상 테마를 선택합니다. 선택 즉시 모든 화면에 반영됩니다.",
            ),
            (
                "실행 프로그램",
                "더블클릭 또는 실행 버튼으로 파일을 열 때 사용할 프로그램을 지정합니다. 비어 있으면 OS 기본 연결 프로그램으로 실행됩니다.",
            ),
            (
                "데이터베이스",
                "타이틀 정보를 가져올 원본 소스를 선택합니다.",
            ),
            (
                "제외 설정",
                "미보유 목록(누락된 업데이트 / DLC)에서 제외할 ID를 입력합니다. 여러 개는 줄바꿈으로 구분하세요.",
            ),
        ];
        egui::Frame::none()
            .fill(ui.visuals().faint_bg_color)
            .inner_margin(egui::Margin {
                left: 12.0,
                right: 6.0,
                top: 10.0,
                bottom: 10.0,
            })
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.heading("⚙  설정");
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("X  닫기").clicked() {
                            self.settings_open = false;
                        }
                    });
                });
            });
        ui.separator();
        let body_size = ui.available_size();
        ui.allocate_ui_with_layout(
            body_size,
            egui::Layout::left_to_right(egui::Align::Min),
            |ui| {
            egui::Frame::none()
                .fill(ui.visuals().faint_bg_color)
                .show(ui, |ui| {
                    ui.allocate_ui_with_layout(
                        egui::vec2(200.0, ui.available_height()),
                        egui::Layout::top_down(egui::Align::Min),
                        |ui| {
                            for (index, (label, _)) in categories.iter().enumerate() {
                                let selected = self.settings_category == index;
                                let (rect, response) = ui.allocate_exact_size(
                                    egui::vec2(ui.available_width(), 38.0),
                                    egui::Sense::click(),
                                );
                                let fill = if selected {
                                    ui.visuals().hyperlink_color
                                } else if response.hovered() {
                                    ui.visuals().widgets.hovered.bg_fill
                                } else {
                                    ui.visuals().faint_bg_color
                                };
                                ui.painter().rect_filled(rect, 4.0, fill);
                                ui.painter().text(
                                    rect.left_center() + egui::vec2(18.0, 0.0),
                                    egui::Align2::LEFT_CENTER,
                                    *label,
                                    egui::FontId::proportional(14.0),
                                    if selected {
                                        egui::Color32::WHITE
                                    } else {
                                        ui.visuals().text_color()
                                    },
                                );
                                if response.clicked() {
                                    self.settings_category = index;
                                }
                            }
                        },
                    );
                });
            ui.separator();
            let content_size = ui.available_size();
            ui.allocate_ui_with_layout(
                content_size,
                egui::Layout::top_down(egui::Align::Min),
                |ui| {
                    ui.add_space(22.0);
                    let category = self.settings_category.min(categories.len() - 1);
                    let (title, subtitle) = categories[category];
                    ui.heading(title);
                    ui.label(egui::RichText::new(subtitle).color(egui::Color32::GRAY));
                    ui.add_space(18.0);
                    ui.separator();
                    ui.add_space(12.0);
                    match category {
                        0 => {
                            let available = ui.available_size();
                            let list_size = egui::vec2(available.x, (available.y - 58.0).max(120.0));
                            ui.allocate_ui_with_layout(
                                list_size,
                                egui::Layout::top_down(egui::Align::Min),
                                |list_ui| {
                                            list_ui.set_min_size(list_size);
                                    if self.folders.is_empty() {
                                                list_ui.label("검색 폴더가 없습니다.");
                                    }
                                    for (index, folder) in self.folders.iter().enumerate() {
                                                if list_ui
                                            .selectable_label(
                                                self.settings_folder_selected == Some(index),
                                                folder,
                                            )
                                            .clicked()
                                        {
                                            self.settings_folder_selected = Some(index);
                                        }
                                    }
                                },
                            );
                            if let Some(folder) = self
                                .settings_folder_selected
                                .and_then(|index| self.folders.get(index))
                            {
                                ui.label(
                                    egui::RichText::new(folder)
                                        .small()
                                        .color(egui::Color32::GRAY),
                                );
                            }
                            ui.add_space(8.0);
                            ui.horizontal(|ui| {
                                let label = format!(
                                    "➕  폴더 추가 ({}/{})",
                                    self.folders.len(),
                                    5
                                );
                                let add_button = egui::Button::new(
                                    egui::RichText::new(label)
                                        .color(ui.visuals().hyperlink_color),
                                )
                                .fill(ui.visuals().faint_bg_color)
                                .stroke(egui::Stroke::new(
                                    1.0_f32,
                                    ui.visuals().hyperlink_color,
                                ));
                                if ui
                                    .add_enabled(self.folders.len() < 5, add_button)
                                    .clicked()
                                {
                                    if self.folders.len() >= 5 {
                                        self.folder_notice = Some(
                                            "스캔 폴더는 최대 5개까지 추가할 수 있습니다.\n기존 폴더를 삭제한 후 추가해 주세요.".into(),
                                        );
                                    } else if let Some(path) = rfd::FileDialog::new().pick_folder() {
                                        let value = path.to_string_lossy().into_owned();
                                        if !self.folders.contains(&value) {
                                            self.folders.push(value);
                                        }
                                    }
                                }
                                let selected_folder = self
                                    .settings_folder_selected
                                    .and_then(|index| self.folders.get(index).cloned());
                                if ui.button("🗑  선택 삭제").clicked() {
                                    if let Some(folder) = selected_folder {
                                        self.remove_folder_confirm = Some(folder);
                                    } else {
                                        self.folder_notice = Some("삭제할 폴더를 선택하세요.".into());
                                    }
                                }
                            });
                        }
                        1 => {
                            egui::Grid::new("settings_general_rows")
                                .num_columns(2)
                                .spacing(egui::vec2(0.0, 12.0))
                                .show(ui, |ui| {
                                    settings_row_label(ui, "이름 표시 우선순위");
                                    let mut mode = self
                                        .config
                                        .settings
                                        .get("name_mode")
                                        .and_then(|v| v.as_str())
                                        .unwrap_or("db")
                                        .to_owned();
                                    ui.vertical(|ui| {
                                        ui.radio_value(&mut mode, "db".into(), "타이틀DB");
                                        ui.radio_value(
                                            &mut mode,
                                            "ko_en_db".into(),
                                            "한국어 > 영어 > DB",
                                        );
                                        ui.radio_value(
                                            &mut mode,
                                            "ko_ja_db".into(),
                                            "한국어 > 일본어 > DB",
                                        );
                                    });
                                    self.config.settings.insert(
                                        "name_mode".into(),
                                        serde_json::Value::String(mode),
                                    );
                                    ui.end_row();

                                    settings_row_label(ui, "좌우 패널 크기");
                                    let mut remember = self
                                        .config
                                        .settings
                                        .get("sash_remember")
                                        .and_then(|v| v.as_str())
                                        .unwrap_or("1")
                                        == "1";
                                    ui.vertical(|ui| {
                                        if ui.checkbox(&mut remember, "위치 기억").changed() {
                                            self.config.settings.insert(
                                                "sash_remember".into(),
                                                serde_json::Value::String(
                                                    if remember { "1" } else { "0" }.into(),
                                                ),
                                            );
                                        }
                                        if ui.button("위치 초기화").clicked() {
                                            self.config.settings.insert(
                                                "sash_pos".into(),
                                                serde_json::Value::String("440".into()),
                                            );
                                            self.reset_panel_width = true;
                                        }
                                    });
                                    ui.end_row();

                                    settings_row_label(ui, "시작 화면 (재시작 필요)");
                                    let mut start_maximized = self
                                        .config
                                        .settings
                                        .get("start_maximized")
                                        .and_then(|v| v.as_str())
                                        .unwrap_or("0")
                                        == "1";
                                    if ui
                                        .checkbox(&mut start_maximized, "시작 시 최대화")
                                        .changed()
                                    {
                                        self.config.settings.insert(
                                            "start_maximized".into(),
                                            serde_json::Value::String(
                                                if start_maximized { "1" } else { "0" }.into(),
                                            ),
                                        );
                                    }
                                    ui.end_row();

                                    settings_row_label(ui, "스캔 항목");
                                    ui.horizontal(|ui| {
                                        for (key, label) in [
                                            ("scan_game", "게임"),
                                            ("scan_icon", "아이콘"),
                                            ("scan_screenshot", "스크린샷"),
                                        ] {
                                            let mut enabled = self
                                                .config
                                                .settings
                                                .get(key)
                                                .and_then(|v| v.as_str())
                                                .unwrap_or("1")
                                                == "1";
                                            if ui
                                                .add_enabled_ui(key != "scan_game", |ui| {
                                                    ui.checkbox(&mut enabled, label)
                                                })
                                                .inner
                                                .changed()
                                            {
                                                self.config.settings.insert(
                                                    key.into(),
                                                    serde_json::Value::String(
                                                        if enabled { "1" } else { "0" }.into(),
                                                    ),
                                                );
                                            }
                                        }
                                    });
                                    ui.end_row();

                                    settings_row_label(ui, "페이징");
                                    let mut paging = self
                                        .config
                                        .settings
                                        .get("paging_on")
                                        .and_then(|v| v.as_str())
                                        .unwrap_or("0")
                                        == "1";
                                    let mut page_size = self
                                        .config
                                        .settings
                                        .get("page_size")
                                        .and_then(|v| v.as_str())
                                        .unwrap_or("500")
                                        .to_owned();
                                    ui.vertical(|ui| {
                                        let paging_changed =
                                            ui.checkbox(&mut paging, "사용").changed();
                                        if paging_changed {
                                            self.config.settings.insert(
                                                "paging_on".into(),
                                                serde_json::Value::String(
                                                    if paging { "1" } else { "0" }.into(),
                                                ),
                                            );
                                            self.current_page = 0;
                                        }
                                        ui.horizontal(|ui| {
                                            ui.label("타이틀 개수:");
                                            let response = ui.add_enabled(
                                                paging,
                                                egui::TextEdit::singleline(&mut page_size)
                                                    .desired_width(70.0),
                                            );
                                            if response.changed()
                                                && page_size
                                                    .parse::<usize>()
                                                    .is_ok_and(|value| value > 0 && value <= 99_999)
                                            {
                                                self.config.settings.insert(
                                                    "page_size".into(),
                                                    serde_json::Value::String(page_size.clone()),
                                                );
                                                self.current_page = 0;
                                            }
                                        });
                                    });
                                    ui.end_row();

                                    settings_row_label(ui, "스캔 스레드");
                                    let cpu = std::thread::available_parallelism()
                                        .map(|n| n.get())
                                        .unwrap_or(8);
                                    let threads = self
                                        .config
                                        .settings
                                        .get("max_scan_threads")
                                        .and_then(|v| v.as_str())
                                        .and_then(|v| v.parse::<usize>().ok())
                                        .unwrap_or_else(|| cpu.min(16))
                                        .min(20);
                                    ui.horizontal(|ui| {
                                        ui.label("스레드 개수:");
                                        let mut threads_text = threads.to_string();
                                        let response = ui.add_sized(
                                            [60.0, ui.spacing().interact_size.y],
                                            egui::TextEdit::singleline(&mut threads_text),
                                        );
                                        if response.changed()
                                            && threads_text
                                                .parse::<usize>()
                                                .is_ok_and(|value| value <= 20)
                                        {
                                            self.config.settings.insert(
                                                "max_scan_threads".into(),
                                                serde_json::Value::String(threads_text),
                                            );
                                        }
                                        let min_reco = (cpu / 2).max(2);
                                        let max_reco = cpu.min(16);
                                        ui.label(
                                            egui::RichText::new(format!(
                                                "({cpu}코어 감지 / {min_reco}~{max_reco} 추천)"
                                            ))
                                            .color(egui::Color32::GRAY),
                                        );
                                    });
                                    ui.end_row();
                                });
                        }
                        2 => {
                            let mut theme_id = self
                                .config
                                .settings
                                .get("theme")
                                .or_else(|| self.config.settings.get("ui_theme"))
                                .and_then(|v| v.as_str())
                                .unwrap_or("github_dark")
                                .to_owned();
                            let entries = theme::entries().collect::<Vec<_>>();
                            let card_width = ((ui.available_width() - 12.0) / 2.0).max(280.0);
                            egui::Grid::new("settings_theme_cards")
                                .num_columns(2)
                                .spacing(egui::vec2(10.0, 10.0))
                                .show(ui, |ui| {
                                    for (index, (id, label)) in entries.into_iter().enumerate() {
                                        let selected = theme_id == id;
                                        let (surface, accent) = theme::swatches(id);
                                        let (rect, response) = ui.allocate_exact_size(
                                            egui::vec2(card_width, 74.0),
                                            egui::Sense::click(),
                                        );
                                        let border = if selected {
                                            accent
                                        } else {
                                            ui.visuals().widgets.noninteractive.bg_stroke.color
                                        };
                                        ui.painter().rect_filled(rect, 6.0, border);
                                        ui.painter().rect_filled(
                                            rect.shrink(if selected { 2.0 } else { 1.0 }),
                                            5.0,
                                            ui.visuals().faint_bg_color,
                                        );
                                        let swatch_size = egui::vec2(16.0, 36.0);
                                        ui.painter().rect_filled(
                                            egui::Rect::from_min_size(
                                                rect.min + egui::vec2(14.0, 19.0),
                                                swatch_size,
                                            ),
                                            4.0,
                                            surface,
                                        );
                                        ui.painter().rect_filled(
                                            egui::Rect::from_min_size(
                                                rect.min + egui::vec2(33.0, 19.0),
                                                swatch_size,
                                            ),
                                            4.0,
                                            accent,
                                        );
                                        let radio_center =
                                            rect.min + egui::vec2(73.0, rect.height() * 0.5);
                                        let radio_color = if selected {
                                            accent
                                        } else {
                                            ui.visuals().widgets.inactive.fg_stroke.color
                                        };
                                        ui.painter().circle_stroke(
                                            radio_center,
                                            7.0,
                                            egui::Stroke::new(1.5_f32, radio_color),
                                        );
                                        if selected {
                                            ui.painter().circle_filled(radio_center, 3.5, radio_color);
                                        }
                                        ui.painter().text(
                                            rect.min + egui::vec2(88.0, rect.height() * 0.5),
                                            egui::Align2::LEFT_CENTER,
                                            label,
                                            egui::FontId::proportional(14.0),
                                            ui.visuals().text_color(),
                                        );
                                        if response.clicked() {
                                            theme_id = id.to_owned();
                                        }
                                        if index % 2 == 1 {
                                            ui.end_row();
                                        }
                                    }
                                });
                            self.config.settings.insert(
                                "theme".into(),
                                serde_json::Value::String(theme_id.clone()),
                            );
                            self.config
                                .settings
                                .insert("ui_theme".into(), serde_json::Value::String(theme_id));
                            ui.add_space(8.0);
                            ui.label(
                                egui::RichText::new(
                                    "💡 변경 사항은 즉시 모든 화면에 반영됩니다. 일부 위젯(이미 표시된 게임 설명 본문 등)은 다른 게임을 선택하면 새 테마 색으로 갱신됩니다.",
                                )
                                .color(egui::Color32::GRAY),
                            );
                        }
                        3 => {
                            let mut enabled = self
                                .config
                                .settings
                                .get("launcher_enabled")
                                .and_then(|v| v.as_str())
                                .unwrap_or("0")
                                == "1";
                            egui::Grid::new("settings_launcher_rows")
                                .num_columns(2)
                                .spacing(egui::vec2(0.0, 10.0))
                                .show(ui, |ui| {
                                    settings_row_label(ui, "실행 방식");
                                    if ui
                                        .checkbox(&mut enabled, "지정한 프로그램으로 실행")
                                        .changed()
                                    {
                                        self.config.settings.insert(
                                            "launcher_enabled".into(),
                                            serde_json::Value::String(
                                                if enabled { "1" } else { "0" }.into(),
                                            ),
                                        );
                                    }
                                    ui.end_row();

                                    settings_row_label(ui, "프로그램 경로");
                                    let mut launcher = self
                                        .config
                                        .settings
                                        .get("launcher_path")
                                        .and_then(|v| v.as_str())
                                        .unwrap_or("")
                                        .to_owned();
                                    ui.horizontal(|ui| {
                                        ui.add_enabled(
                                            enabled,
                                            egui::TextEdit::singleline(&mut launcher)
                                                .desired_width(260.0)
                                                .hint_text(
                                                    r"C:\Program Files\yuzu\yuzu.exe  또는  /usr/bin/ryujinx",
                                                ),
                                        );
                                        if ui
                                            .add_enabled(enabled, egui::Button::new("찾기…"))
                                            .clicked()
                                        {
                                            if let Some(path) = rfd::FileDialog::new().pick_file() {
                                                launcher = path.to_string_lossy().into_owned();
                                            }
                                        }
                                    });
                                    self.config.settings.insert(
                                        "launcher_path".into(),
                                        serde_json::Value::String(launcher),
                                    );
                                    ui.end_row();

                                    settings_row_label(ui, "추가 인자");
                                    let mut args = self
                                        .config
                                        .settings
                                        .get("launcher_args")
                                        .and_then(|v| v.as_str())
                                        .unwrap_or("")
                                        .to_owned();
                                    ui.add_enabled(
                                        enabled,
                                        egui::TextEdit::singleline(&mut args)
                                            .desired_width(260.0)
                                            .hint_text(
                                                "예) --fullscreen   ({path} 는 파일 경로로 자동 치환)",
                                            ),
                                    );
                                    self.config.settings.insert(
                                        "launcher_args".into(),
                                        serde_json::Value::String(args),
                                    );
                                    ui.end_row();

                                    settings_row_label(ui, "");
                                    ui.add(
                                        egui::Label::new(
                                            egui::RichText::new(
                                                "💡 인자에 {path} 를 입력하면 실행 시 파일 경로로 자동 치환됩니다. 없으면 경로가 마지막 인자로 자동 추가됩니다.",
                                            )
                                            .color(egui::Color32::GRAY),
                                        )
                                        .wrap(),
                                    );
                                    ui.end_row();
                                });
                            ui.add_space(10.0);
                            ui.separator();
                            ui.add_space(8.0);
                            let mut decompress_auto = self
                                .config
                                .settings
                                .get("decompress_auto")
                                .and_then(|v| v.as_str())
                                .unwrap_or("0")
                                == "1";
                            egui::Grid::new("settings_decompress_rows")
                                .num_columns(2)
                                .spacing(egui::vec2(0.0, 4.0))
                                .show(ui, |ui| {
                                    settings_row_label(ui, "압축 해제");
                                    if ui
                                        .checkbox(
                                            &mut decompress_auto,
                                            "NSZ / XCZ 자동 압축 해제",
                                        )
                                        .changed()
                                    {
                                        self.config.settings.insert(
                                            "decompress_auto".into(),
                                            serde_json::Value::String(
                                                if decompress_auto { "1" } else { "0" }.into(),
                                            ),
                                        );
                                    }
                                    ui.end_row();
                                    settings_row_label(ui, "");
                                    ui.add(
                                        egui::Label::new(
                                            egui::RichText::new(format!(
                                                "💡 NSZ → NSP, XCZ → XCI 로 변환 후 실행합니다. 변환 파일은 {} 에 생성되며, 다음 변환 전에 이전 작업 폴더가 자동으로 정리됩니다.",
                                                self.base_dir.join("workspaces").display()
                                            ))
                                            .color(egui::Color32::GRAY),
                                        )
                                        .wrap(),
                                    );
                                    ui.end_row();
                                });
                        }
                        4 => {
                            let mut source = self
                                .config
                                .settings
                                .get("titledb_source")
                                .and_then(|v| v.as_str())
                                .unwrap_or("custom")
                                .to_owned();
                            egui::Grid::new("settings_database_rows")
                                .num_columns(2)
                                .spacing(egui::vec2(0.0, 4.0))
                                .show(ui, |ui| {
                                    settings_row_label(ui, "타이틀 DB 소스");
                                    ui.vertical(|ui| {
                                        ui.radio_value(
                                            &mut source,
                                            "custom".into(),
                                            "자체 서버 (기본, 빠름)",
                                        );
                                        ui.radio_value(
                                            &mut source,
                                            "tinfoil".into(),
                                            "Tinfoil 공식 (titles.json)",
                                        );
                                    });
                                    ui.end_row();
                                    settings_row_label(ui, "");
                                    ui.add(
                                        egui::Label::new(
                                            egui::RichText::new(
                                                "자체 서버 URL: https://github.com/UzuCore/NSL/releases/download/titledb/titles.json",
                                            )
                                            .color(egui::Color32::GRAY),
                                        )
                                        .wrap(),
                                    );
                                    ui.end_row();
                                });
                            self.config.settings.insert(
                                "titledb_source".into(),
                                serde_json::Value::String(source),
                            );
                        }
                        _ => {
                            let width = ui.available_width();
                            ui.add_space(8.0);
                            let input_height = ((ui.available_height() - 180.0) / 2.0).max(120.0);
                            for (index, (key, label)) in [
                                ("exception_update", "업데이트 제외 (타이틀 ID)"),
                                ("exception_dlc", "DLC 제외 (DLC ID)"),
                            ]
                            .into_iter()
                            .enumerate()
                            {
                                let raw = self
                                    .config
                                    .settings
                                    .get(key)
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("");
                                let mut lines = raw
                                    .split(',')
                                    .filter(|s| !s.is_empty())
                                    .collect::<Vec<_>>()
                                    .join("\n");
                                ui.label(label);
                                ui.add_space(3.0);
                                let changed = egui::Frame::none()
                                    .fill(ui.visuals().extreme_bg_color)
                                    .stroke(ui.visuals().widgets.noninteractive.bg_stroke)
                                    .inner_margin(egui::Margin::same(4.0))
                                    .show(ui, |ui| {
                                        egui::ScrollArea::vertical()
                                            .id_salt(("settings_exception_input", key))
                                            .max_height((input_height - 8.0).max(80.0))
                                            .auto_shrink([false, false])
                                            .show(ui, |ui| {
                                                ui.set_width((width - 8.0).max(0.0));
                                                ui.add(
                                                    egui::TextEdit::multiline(&mut lines)
                                                        .desired_rows(6)
                                                        .frame(false),
                                                )
                                            })
                                            .inner
                                    })
                                    .inner
                                    .changed();
                                if changed {
                                    let value = lines
                                        .lines()
                                        .map(str::trim)
                                        .filter(|s| !s.is_empty())
                                        .map(str::to_ascii_uppercase)
                                        .collect::<Vec<_>>()
                                        .join(",");
                                    self.config.settings.insert(
                                        key.into(),
                                        serde_json::Value::String(value),
                                    );
                                }
                                if index == 0 {
                                    ui.add_space(20.0);
                                }
                            }
                        }
                    }
                    ui.add_space(24.0);
                },
            );
            },
        );
    }

    fn selected_detail_ui(
        &mut self,
        ui: &mut egui::Ui,
        row: &library::GameFile,
        screenshot_paths: &[PathBuf],
    ) {
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    if let Some(icon) = self.icon_textures.get(&row.title_id) {
                        ui.image((icon.id(), egui::vec2(120.0, 120.0)));
                    }
                    ui.vertical(|ui| {
                        ui.horizontal(|ui| {
                            ui.label(egui::RichText::new(&row.title_name).size(14.0).strong());
                            if ui.small_button("×").clicked() {
                                self.selected = None;
                            }
                        });
                        ui.label(
                            egui::RichText::new(&row.title_id)
                                .size(12.0)
                                .color(egui::Color32::GRAY),
                        );
                        let mut type_label = match row.content_type.as_str() {
                            "base" => "베이스".to_owned(),
                            "update" => "업데이트".to_owned(),
                            "dlc" => "DLC".to_owned(),
                            "delta" => "Delta".to_owned(),
                            _ => "Unknown".to_owned(),
                        };
                        if row.has_update {
                            type_label.push_str("+U");
                        }
                        if row.dlc_count > 0 {
                            type_label.push_str(&format!("+{}DLC", row.dlc_count));
                        }
                        type_label.push_str(&format!("  ({})", row.file_type.to_ascii_uppercase()));
                        ui.colored_label(ui.visuals().hyperlink_color, type_label);
                        let installed = if row.update_version != 0 {
                            row.update_version
                        } else {
                            row.version
                        };
                        ui.label(format!(
                            "보유: {}",
                            common::display_version(
                                Some(row.nacp_version.as_str()),
                                Some(installed)
                            )
                        ));
                        ui.colored_label(
                            if row.latest_version > installed {
                                egui::Color32::from_rgb(224, 108, 117)
                            } else {
                                egui::Color32::GRAY
                            },
                            format!("최신: {}", common::version_to_str(Some(row.latest_version))),
                        );
                    });
                });
                ui.separator();
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 1.0;
                    let width = ((ui.available_width() - 3.0) / 4.0).max(48.0);
                    for (index, label) in ["파일정보", "게임정보", "스크린샷", "DLC"]
                        .into_iter()
                        .enumerate()
                    {
                        if draw_tab_button(ui, label, self.detail_tab == index, width, index, 4)
                            .clicked()
                        {
                            self.detail_tab = index;
                            self.config.settings.insert(
                                "last_selected_tab".into(),
                                serde_json::Value::String(index.to_string()),
                            );
                        }
                    }
                });
                ui.separator();
                match self.detail_tab {
                    0 => {
                        let installed = if row.update_version != 0 {
                            row.update_version
                        } else {
                            row.version
                        };
                        let localized_names = [
                            ("게임 이름 (한국어)", row.nacp_ko.trim()),
                            ("게임 이름 (영어)", row.nacp_en.trim()),
                            ("게임 이름 (일본어)", row.nacp_ja.trim()),
                        ];
                        let names_present =
                            localized_names.iter().any(|(_, name)| !name.is_empty());
                        let mut fields = Vec::new();
                        if names_present {
                            fields.extend(
                                localized_names
                                    .iter()
                                    .filter(|(_, name)| !name.is_empty())
                                    .map(|(label, name)| ((*label).to_owned(), (*name).to_owned())),
                            );
                        } else {
                            fields.push((
                                "게임 이름".to_owned(),
                                if row.title_name.is_empty() {
                                    row.filename.clone()
                                } else {
                                    row.title_name.clone()
                                },
                            ));
                        }
                        let mut content_type = match row.content_type.as_str() {
                            "base" => "베이스".to_owned(),
                            "update" => "업데이트".to_owned(),
                            "dlc" => "DLC".to_owned(),
                            "" | "unknown" => "—".to_owned(),
                            other => other.to_owned(),
                        };
                        if row.has_update {
                            content_type.push_str("+U");
                        }
                        if row.dlc_count > 0 {
                            content_type.push_str(&format!("+{}DLC", row.dlc_count));
                        }
                        if !row.file_type.is_empty() {
                            content_type
                                .push_str(&format!("  ({})", row.file_type.to_ascii_uppercase()));
                        }
                        fields.extend([
                            ("타이틀 ID".to_owned(), row.title_id.clone()),
                            (
                                "보유 버전".to_owned(),
                                common::display_version(
                                    Some(row.nacp_version.as_str()),
                                    Some(installed),
                                ),
                            ),
                            (
                                "최신 버전".to_owned(),
                                common::version_to_str(Some(row.latest_version)),
                            ),
                            ("타입".to_owned(), content_type),
                            (
                                "크기".to_owned(),
                                common::format_size(row.size.max(0) as u64),
                            ),
                            ("파일 경로".to_owned(), row.path.clone()),
                            ("파일명".to_owned(), row.filename.clone()),
                        ]);
                        for (label, value) in fields {
                            ui.label(egui::RichText::new(label).color(egui::Color32::GRAY));
                            ui.label(value);
                            ui.add_space(6.0);
                        }
                    }
                    1 => {
                        for (label, value) in [
                            ("한국어", &row.nacp_ko),
                            ("영어", &row.nacp_en),
                            ("일본어", &row.nacp_ja),
                            ("지역", &row.region),
                            ("언어", &row.languages),
                        ] {
                            if !value.is_empty() {
                                ui.label(format!("{label}: {value}"));
                            }
                        }
                        if !row.description.is_empty() {
                            ui.heading("게임 설명");
                            ui.label(&row.description);
                            let translated = self
                                .translated_description
                                .as_ref()
                                .filter(|(id, _)| *id == row.id)
                                .map(|(_, text)| text.clone());
                            if translated.is_none() {
                                if self.translate_result.is_some() {
                                    ui.add_enabled(false, egui::Button::new("한국어 번역 중…"));
                                } else if ui.button("설명 한국어 번역").clicked() {
                                    self.start_translation(row);
                                }
                            }
                            if let Some(error) = self
                                .translation_error
                                .as_ref()
                                .filter(|(id, _)| *id == row.id)
                                .map(|(_, error)| error.clone())
                            {
                                ui.colored_label(
                                    egui::Color32::LIGHT_RED,
                                    format!("번역 실패: {error}"),
                                );
                            }
                            if let Some(translated) = translated {
                                ui.add_space(8.0);
                                ui.heading("한국어 번역");
                                ui.label(translated);
                            }
                        }
                        if row.latest_version > row.version.max(row.update_version) {
                            ui.colored_label(
                                egui::Color32::LIGHT_RED,
                                format!(
                                    "업데이트 필요: {} → {}",
                                    common::version_to_str(Some(
                                        row.version.max(row.update_version)
                                    )),
                                    common::version_to_str(Some(row.latest_version))
                                ),
                            );
                        }
                    }
                    2 => {
                        if let Ok(urls) = serde_json::from_str::<Vec<String>>(&row.screenshots) {
                            if !urls.is_empty() {
                                ui.collapsing(
                                    format!("원본 스크린샷 링크 ({})", urls.len()),
                                    |ui| {
                                        for (index, url) in urls.iter().enumerate() {
                                            ui.hyperlink_to(format!("스크린샷 {}", index + 1), url);
                                        }
                                    },
                                );
                            }
                        }
                        if !screenshot_paths.is_empty() {
                            ui.label("저장된 스크린샷 (선택하면 열기)");
                            ui.add_space(6.0);
                            ui.vertical(|ui| {
                                for path in screenshot_paths {
                                    let key = path.to_string_lossy().into_owned();
                                    if let Some(tex) = self.screenshot_textures.get(&key) {
                                        let texture_size = tex.size_vec2();
                                        let width = ui.available_width().clamp(1.0, 900.0);
                                        let height =
                                            width * texture_size.y / texture_size.x.max(1.0);
                                        let response = ui.add(
                                            egui::Image::new((tex.id(), egui::vec2(width, height)))
                                                .sense(egui::Sense::click()),
                                        );
                                        if response.clicked() {
                                            let _ = std::process::Command::new("explorer")
                                                .arg(&key)
                                                .spawn();
                                        }
                                        ui.add_space(8.0);
                                    }
                                }
                            });
                        } else {
                            ui.label("저장된 스크린샷이 없습니다.");
                        }
                    }
                    _ => {
                        if !row.missing_dlcs.is_empty() {
                            ui.colored_label(
                                egui::Color32::from_rgb(229, 192, 123),
                                format!("미보유 DLC 목록 (총 {}개)", row.missing_dlcs.len()),
                            );
                            for (id, name) in &row.missing_dlcs {
                                ui.label(format!("  - [{id}] {name}"));
                            }
                            ui.add_space(10.0);
                        }
                        if !row.owned_dlcs.is_empty() {
                            ui.colored_label(
                                egui::Color32::from_rgb(152, 195, 121),
                                format!("설치된 DLC 목록 (총 {}개)", row.owned_dlcs.len()),
                            );
                            for (id, name) in &row.owned_dlcs {
                                ui.label(format!("  + [{id}] {name}"));
                            }
                        }
                        if row.owned_dlcs.is_empty() && row.missing_dlcs.is_empty() {
                            ui.label(format!(
                                "DLC 정보가 없습니다. 보유 DLC: {} / DB 등록 DLC: {}",
                                row.dlc_count, row.db_dlc_count
                            ));
                        }
                    }
                }
            });
    }
}

impl eframe::App for NslApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.ui_frame(ctx);
    }
}

impl NslApp {
    fn ui_frame(&mut self, ctx: &egui::Context) {
        self.refresh_resource_sample(ctx);
        let theme_id = self
            .config
            .settings
            .get("theme")
            .or_else(|| self.config.settings.get("ui_theme"))
            .and_then(|v| v.as_str())
            .unwrap_or("github_dark")
            .to_owned();
        if self.active_theme != theme_id {
            theme::apply(ctx, &theme_id);
            self.active_theme = theme_id;
        }
        let mut received_log = false;
        while let Ok(line) = self.log_receiver.try_recv() {
            self.log_lines.push(line);
            received_log = true;
        }
        if received_log {
            self.log_open = true;
        }
        if self.log_lines.len() > 2000 {
            self.log_lines.drain(..self.log_lines.len() - 2000);
        }
        let (ctrl_f, ctrl_o, f5, escape, enter) = ctx.input(|input| {
            (
                input.modifiers.ctrl && input.key_pressed(egui::Key::F),
                input.modifiers.ctrl && input.key_pressed(egui::Key::O),
                input.key_pressed(egui::Key::F5),
                input.key_pressed(egui::Key::Escape),
                input.key_pressed(egui::Key::Enter),
            )
        });
        if ctrl_f {
            self.search_focus = true;
        }
        if ctrl_o {
            self.settings_open = !self.settings_open;
        }
        if f5 && self.scan_result.is_none() {
            self.start_scan(true);
        }
        if escape {
            self.search.clear();
            self.search_focus = false;
        }
        if enter && !ctx.wants_keyboard_input() {
            if let Some(row) = self
                .selected
                .and_then(|id| self.rows.iter().find(|row| row.id == id))
                .cloned()
            {
                self.run_file(&row);
            }
        }
        let active_tasks = [
            (self.scan_result.is_some(), "게임 파일 스캔"),
            (self.titledb_result.is_some(), "타이틀 DB 작업"),
            (self.media_result.is_some(), "아이콘 또는 스크린샷 다운로드"),
            (self.decompress_result.is_some(), "NSZ/XCZ 압축 해제"),
            (self.bulk_rename_result.is_some(), "파일명 일괄 정규화"),
            (self.update_check_result.is_some(), "업데이트 확인"),
            (self.update_prepare_result.is_some(), "업데이트 다운로드"),
            (self.orphan_cleanup_result.is_some(), "미디어 캐시 정리"),
            (self.translate_result.is_some(), "설명 번역"),
        ]
        .into_iter()
        .filter_map(|(active, label)| active.then_some(label))
        .collect::<Vec<_>>();
        let is_working = !active_tasks.is_empty();
        if is_working && !self.was_working {
            self.log_open = true;
        }
        self.was_working = is_working;
        let close_requested = ctx.input(|input| input.viewport().close_requested());
        if close_requested && !self.exit_confirmed && !active_tasks.is_empty() {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.exit_confirm = true;
        }
        let scan_done = self.scan_result.as_ref().and_then(|rx| rx.try_recv().ok());
        if let Some(done) = scan_done {
            self.scan_result = None;
            self.scan_cancel = None;
            self.scan_progress = None;
            match done {
                Ok((n, removed, cancelled)) => {
                    self.status = if cancelled {
                        format!("스캔 중지됨 · {n}개 파일 반영 · 기존 파일 기록은 유지")
                    } else {
                        format!("{n}개 파일 검색 완료 · {removed}개 누락 기록 제거")
                    };
                    self.pending_icon_prefetch = self
                        .config
                        .settings
                        .get("scan_icon")
                        .and_then(|v| v.as_str())
                        .unwrap_or("1")
                        == "1";
                    self.pending_screenshot_prefetch = self
                        .config
                        .settings
                        .get("scan_screenshot")
                        .and_then(|v| v.as_str())
                        .unwrap_or("1")
                        == "1";
                    self.start_reload();
                }
                Err(e) => self.status = e,
            }
        } else if self.scan_result.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
        let titledb_done = self
            .titledb_result
            .as_ref()
            .and_then(|rx| rx.try_recv().ok());
        if let Some(done) = titledb_done {
            self.titledb_result = None;
            match done {
                Ok(n) => {
                    self.status = format!("타이틀 DB {n}개 항목 다운로드 및 반영 완료");
                    self.start_reload();
                }
                Err(e) => self.status = e,
            }
        } else if self.titledb_result.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
        let reload_done = self
            .reload_result
            .as_ref()
            .and_then(|rx| rx.try_recv().ok());
        if let Some(done) = reload_done {
            self.reload_result = None;
            match done {
                Ok(rows) => {
                    self.rows = rows;
                    match self.db.stats() {
                        Ok(stats) => self.stats = stats,
                        Err(error) => self.status = format!("DB 통계를 읽지 못했습니다: {error}"),
                    }
                    if self.view_mode == "grid" && self.selected.is_none() {
                        self.selected = library::visible(&self.rows, &self.search)
                            .filter(|row| self.row_matches_current_filter(row))
                            .min_by(|a, b| {
                                a.title_name
                                    .to_lowercase()
                                    .cmp(&b.title_name.to_lowercase())
                            })
                            .map(|row| row.id);
                    }
                }
                Err(e) => self.status = format!("목록 갱신 실패: {e}"),
            }
        } else if self.reload_result.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
        let bulk_rename_done = self
            .bulk_rename_result
            .as_ref()
            .and_then(|rx| rx.try_recv().ok());
        if let Some(done) = bulk_rename_done {
            self.bulk_rename_result = None;
            match done {
                Ok((renamed, failed)) => {
                    self.status = format!("일괄 정규화 완료: 변경 {renamed}개, 실패 {failed}개");
                    self.start_reload();
                }
                Err(e) => self.status = e,
            }
        } else if self.bulk_rename_result.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
        let update_check_done = self
            .update_check_result
            .as_ref()
            .and_then(|rx| rx.try_recv().ok());
        if let Some(done) = update_check_done {
            self.update_check_result = None;
            match done {
                Ok(info) if info.is_newer && info.download_url.is_some() => {
                    self.update_info = Some(info);
                }
                Ok(info) if info.is_newer => {
                    self.status = format!(
                        "v{}가 있지만 호환되는 Rust 패키지가 릴리스에 없습니다",
                        info.latest_version
                    );
                }
                Ok(info) => {
                    self.status = format!("현재 v{}가 최신입니다", info.current_version);
                }
                Err(e) => self.status = format!("업데이트 확인 실패: {e}"),
            }
        } else if self.update_check_result.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
        let update_prepare_done = self
            .update_prepare_result
            .as_ref()
            .and_then(|rx| rx.try_recv().ok());
        if let Some(done) = update_prepare_done {
            self.update_prepare_result = None;
            match done {
                Ok(path) => {
                    self.status = "업데이트 패키지를 검증했습니다".into();
                    self.update_ready = Some(path);
                }
                Err(e) => self.status = format!("업데이트 준비 실패: {e}"),
            }
        } else if self.update_prepare_result.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
        let orphan_cleanup_done = self
            .orphan_cleanup_result
            .as_ref()
            .and_then(|rx| rx.try_recv().ok());
        if let Some(done) = orphan_cleanup_done {
            self.orphan_cleanup_result = None;
            match done {
                Ok((icons, screenshots)) => {
                    self.status = format!(
                        "캐시 정리 완료 · 아이콘 {icons}개, 스크린샷 폴더 {screenshots}개 제거"
                    );
                }
                Err(e) => self.status = format!("캐시 정리 실패: {e}"),
            }
        } else if self.orphan_cleanup_result.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
        self.start_pending_media();
        let media_done = self.media_result.as_ref().and_then(|rx| rx.try_recv().ok());
        if let Some(done) = media_done {
            self.media_result = None;
            match done {
                Ok((label, n)) => {
                    self.status = format!("{label} 다운로드 완료 · {n}장 저장");
                    self.invalidate_icons();
                    self.screenshot_textures.clear();
                    self.start_pending_media();
                }
                Err(e) => self.status = e,
            }
        } else if self.media_result.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
        let decompress_done = self
            .decompress_result
            .as_ref()
            .and_then(|rx| rx.try_recv().ok());
        if let Some(done) = decompress_done {
            self.decompress_result = None;
            match done {
                Ok(path) => {
                    self.status = format!(
                        "압축 해제 완료: {}",
                        path.file_name().unwrap_or_default().to_string_lossy()
                    );
                    self.converted_file = Some(path);
                    if self.launch_after_decompress {
                        self.launch_after_decompress = false;
                        if let Some(path) = self.converted_file.clone() {
                            if let Err(e) = self.launch_path(&path) {
                                self.status = format!("실행 실패: {e}")
                            }
                        }
                    }
                }
                Err(e) => {
                    self.launch_after_decompress = false;
                    self.status = e
                }
            }
        } else if self.decompress_result.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
        let translate_done = self
            .translate_result
            .as_ref()
            .and_then(|rx| rx.try_recv().ok());
        if let Some(done) = translate_done {
            self.translate_result = None;
            match done {
                (id, Ok(translated)) => {
                    self.status = "설명 번역 완료".into();
                    self.translation_error = None;
                    self.translated_description = Some((id, translated));
                }
                (id, Err(e)) => {
                    self.status = format!("번역 실패: {e}");
                    self.translation_error = Some((id, e));
                }
            }
        } else if self.translate_result.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }

        if self.texture_selection != self.selected {
            self.screenshot_textures.clear();
            self.texture_selection = self.selected;
        }
        if let Some(row) = self
            .selected
            .and_then(|id| self.rows.iter().find(|r| r.id == id))
            .cloned()
        {
            let key = row.title_id.clone();
            let mut budget = ICON_LOAD_BUDGET;
            self.ensure_icon_texture(ctx, &key, &mut budget);
            self.touch_icon(&key);
        }
        let selected_screenshot_paths: Vec<PathBuf> = self
            .selected
            .and_then(|id| self.rows.iter().find(|r| r.id == id))
            .map(|row| {
                let dir = self.screenshot_dir.join(&row.title_id);
                let mut files = std::fs::read_dir(dir)
                    .ok()
                    .into_iter()
                    .flatten()
                    .filter_map(|e| e.ok().map(|e| e.path()))
                    .filter(|p| {
                        p.extension()
                            .and_then(|e| e.to_str())
                            .map(|e| {
                                ["jpg", "jpeg", "png", "webp"]
                                    .contains(&e.to_ascii_lowercase().as_str())
                            })
                            .unwrap_or(false)
                    })
                    .collect::<Vec<_>>();
                files.sort();
                files.into_iter().take(6).collect()
            })
            .unwrap_or_default();
        for path in &selected_screenshot_paths {
            let key = path.to_string_lossy().into_owned();
            if let std::collections::hash_map::Entry::Vacant(entry) =
                self.screenshot_textures.entry(key)
            {
                if let Ok(img) = image::open(path) {
                    if let Some(texture) = texture_from_image(ctx, "screenshot", img, 1200) {
                        entry.insert(texture);
                    }
                }
            }
        }
        let search_before = self.search.clone();
        let filter_before = self.kind_filter.clone();
        let folder_filter_before = self.folder_filter.clone();
        egui::TopBottomPanel::top("top").show(ctx, |ui| {
            top_bar(self, ui, ctx, &active_tasks);
        });
        if self.search != search_before || self.kind_filter != filter_before {
            self.current_page = 0;
        }
        if self.kind_filter != filter_before && !self.folder_filter.is_empty() {
            self.folder_filter.clear();
            self.config.settings.insert(
                "last_tab_key".into(),
                serde_json::Value::String(String::new()),
            );
            let _ = database::save_config(&self.base_dir.join("config.json"), &self.config);
        }
        if self.folder_filter != folder_filter_before {
            self.current_page = 0;
        }
        if self.search != search_before
            || self.kind_filter != filter_before
            || self.folder_filter != folder_filter_before
        {
            self.selected = None;
        }
        let bottom_height = if self.log_open { 205.0 } else { 26.0 };
        let mut close_log = false;
        egui::TopBottomPanel::bottom("status")
            .exact_height(bottom_height)
            .frame(
                egui::Frame::none()
                    .fill(ctx.style().visuals.widgets.inactive.bg_fill)
                    .inner_margin(egui::Margin::symmetric(8.0, 4.0)),
            )
            .show(ctx, |ui| {
                if self.log_open {
                    ui.horizontal(|ui| {
                        ui.label(format!("로그 · {}개 메시지", self.log_lines.len()));
                        if ui.small_button("로그 지우기").clicked() {
                            self.log_lines.clear();
                        }
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            close_log = ui.small_button("X").clicked()
                        });
                    });
                    ui.separator();
                    egui::ScrollArea::vertical()
                        .id_salt("operation_log")
                        .stick_to_bottom(true)
                        .auto_shrink([false, false])
                        .max_height(150.0)
                        .show(ui, |ui| {
                            for line in &self.log_lines {
                                let color = if line.contains("❌") || line.contains("⚠") {
                                    if ui.visuals().dark_mode {
                                        egui::Color32::LIGHT_RED
                                    } else {
                                        egui::Color32::from_rgb(207, 34, 46)
                                    }
                                } else if line.contains("✅") {
                                    if ui.visuals().dark_mode {
                                        egui::Color32::LIGHT_GREEN
                                    } else {
                                        egui::Color32::from_rgb(26, 127, 55)
                                    }
                                } else {
                                    ui.visuals().text_color()
                                };
                                ui.label(egui::RichText::new(line).monospace().color(color));
                            }
                        });
                    ui.separator();
                }
                ui.horizontal(|ui| {
                    ui.label(self.status.as_str());
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(
                            egui::RichText::new(self.resource_usage_text())
                                .monospace()
                                .color(ui.visuals().weak_text_color()),
                        );
                    });
                });
            });
        if close_log {
            self.log_open = false;
        }
        let settings_before = self.config.settings.clone();
        let folders_before = self.folders.clone();
        let name_mode_before = self
            .config
            .settings
            .get("name_mode")
            .and_then(|v| v.as_str())
            .unwrap_or("db")
            .to_owned();
        let remember_sash = self
            .config
            .settings
            .get("sash_remember")
            .and_then(|v| v.as_str())
            .unwrap_or("1")
            == "1";
        let mut saved_detail_width = self
            .config
            .settings
            .get("sash_pos")
            .and_then(|v| v.as_str())
            .and_then(|v| v.parse::<f32>().ok())
            .filter(|v| v.is_finite())
            .unwrap_or(440.0)
            .clamp(320.0, 900.0);
        if self.reset_panel_width {
            ctx.data_mut(|data| {
                data.remove::<egui::containers::panel::PanelState>(egui::Id::new("detail"));
            });
            saved_detail_width = 440.0;
            self.reset_panel_width = false;
        }
        if !self.settings_open {
            if let Some(row) = self
                .selected
                .and_then(|id| self.rows.iter().find(|row| row.id == id))
                .cloned()
            {
                let detail_panel = egui::SidePanel::right("detail")
                    .default_width(saved_detail_width)
                    .min_width(320.0)
                    .max_width(900.0)
                    .resizable(true)
                    .show(ctx, |ui| {
                        self.selected_detail_ui(ui, &row, &selected_screenshot_paths);
                    });
                if remember_sash {
                    let width =
                        egui::containers::panel::PanelState::load(ctx, egui::Id::new("detail"))
                            .map(|state| state.size().x)
                            .unwrap_or_else(|| detail_panel.response.rect.width());
                    if (width - saved_detail_width).abs() > 1.0 {
                        self.config.settings.insert(
                            "sash_pos".into(),
                            serde_json::Value::String(format!("{width:.0}")),
                        );
                    }
                }
            }
        }
        let central_panel = egui::CentralPanel::default();
        let central_panel = if self.settings_open {
            central_panel
                .frame(egui::Frame::none().fill(ctx.style().visuals.widgets.inactive.bg_fill))
        } else {
            central_panel
        };
        central_panel.show(ctx, |ui| {
            if self.settings_open {
                self.settings_panel_ui(ui);
            } else {
                if let Some(progress) = &self.scan_progress {
                    let total = progress.total.load(Ordering::Relaxed);
                    let completed = progress.completed.load(Ordering::Relaxed).min(total);
                    if total == 0 {
                        ui.label("스캔 대상 파일을 찾는 중…");
                    } else {
                        ui.add(
                            egui::ProgressBar::new(completed as f32 / total as f32)
                                .text(format!("스캔 {completed} / {total}")),
                        );
                    }
                }
                let base_count = self.rows.iter().filter(|row| row.has_base).count();
                let mut folder_tabs = vec![(String::new(), format!("전체 ({base_count})"))];
                for folder in &self.folders {
                    let count = self
                        .rows
                        .iter()
                        .filter(|row| row.has_base && path_is_within(&row.path, folder))
                        .count();
                    let name = PathBuf::from(folder)
                        .file_name()
                        .map(|v| v.to_string_lossy().into_owned())
                        .filter(|v| !v.is_empty())
                        .unwrap_or_else(|| folder.clone());
                    folder_tabs.push((folder.clone(), format!("{name} ({count})")));
                }
                folder_tabs.push((
                    "MISSING_UPDATES".into(),
                    format!("미보유 업데이트 ({})", self.missing_update_count()),
                ));
                folder_tabs.push((
                    "MISSING_DLCS".into(),
                    format!("미보유 DLC ({})", self.missing_dlc_count()),
                ));
                egui::ScrollArea::horizontal()
                    .id_salt("directory_tabs")
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 1.0;
                            let tab_count = folder_tabs.len();
                            for (index, (key, label)) in folder_tabs.into_iter().enumerate() {
                                let text_galley = ui.painter().layout_no_wrap(
                                    label.clone(),
                                    egui::TextStyle::Button.resolve(ui.style()),
                                    ui.visuals().text_color(),
                                );
                                let width = text_galley.size().x + 28.0;
                                let selected = self.folder_filter == key;
                                let response =
                                    draw_tab_button(ui, &label, selected, width, index, tab_count);
                                if response.clicked() {
                                    self.folder_filter = key.clone();
                                    self.kind_filter = "base".into();
                                    self.current_page = 0;
                                    self.config.settings.insert(
                                        "last_tab_key".into(),
                                        serde_json::Value::String(key),
                                    );
                                    let _ = database::save_config(
                                        &self.base_dir.join("config.json"),
                                        &self.config,
                                    );
                                }
                            }
                        });
                    });
                ui.separator();
                let library_focus_id = ui.make_persistent_id("library_keyboard_navigation");
                let library_focus_rect = ui.available_rect_before_wrap();
                let _library_focus_anchor = ui.interact(
                    library_focus_rect,
                    library_focus_id,
                    egui::Sense::focusable_noninteractive(),
                );
                let (grid_columns, grid_card_width, grid_thumbnail_size) =
                    library_grid_dimensions(ui.available_width());
                if self.rows.is_empty() {
                    ui.group(|ui| {
                        ui.heading("시작하기");
                        ui.label(
                            "1. prod.keys를 프로그램 폴더 또는 %USERPROFILE%\\.switch에 둡니다.",
                        );
                        ui.label("2. 타이틀 DB를 다운로드하거나 로컬 DB를 매칭합니다.");
                        ui.label("3. 설정에서 스캔 폴더를 추가한 뒤 전체 스캔을 실행합니다.");
                        ui.label(if scanner::has_prod_keys() {
                            "prod.keys 발견됨".to_owned()
                        } else {
                            "prod.keys 없음".to_owned()
                        });
                    });
                }
                let mut matching = library::visible(&self.rows, &self.search)
                    .filter(|row| self.row_matches_current_filter(row))
                    .cloned()
                    .collect::<Vec<_>>();
                matching.sort_by(|a, b| {
                    let ordering = match self.sort_column.as_str() {
                        "id" => a.title_id.cmp(&b.title_id),
                        "type" => a.content_type.cmp(&b.content_type),
                        "version" => a.version.cmp(&b.version),
                        "latest_ver" => a.latest_version.cmp(&b.latest_version),
                        "dlc" => a.dlc_count.cmp(&b.dlc_count),
                        "size" => a.size.cmp(&b.size),
                        "path" => a.path.to_lowercase().cmp(&b.path.to_lowercase()),
                        "status" => {
                            let status = |row: &library::GameFile| {
                                if row.latest_version == 0 {
                                    0
                                } else if row.version.max(row.update_version) < row.latest_version {
                                    1
                                } else {
                                    2
                                }
                            };
                            status(a).cmp(&status(b))
                        }
                        _ => a
                            .title_name
                            .to_lowercase()
                            .cmp(&b.title_name.to_lowercase()),
                    };
                    if self.sort_desc {
                        ordering.reverse()
                    } else {
                        ordering
                    }
                });
                let paging_on = self
                    .config
                    .settings
                    .get("paging_on")
                    .and_then(|v| v.as_str())
                    .unwrap_or("0")
                    == "1";
                let page_size = self
                    .config
                    .settings
                    .get("page_size")
                    .and_then(|v| v.as_str())
                    .and_then(|v| v.parse::<usize>().ok())
                    .filter(|v| *v > 0)
                    .unwrap_or(500)
                    .min(99_999);
                let page_count = matching.len().div_ceil(page_size).max(1);
                self.current_page = self.current_page.min(page_count - 1);
                let (up, down, left, right, home, end, enter_key) = ctx.input(|input| {
                    if input.modifiers.ctrl || input.modifiers.alt || input.modifiers.shift {
                        return (false, false, false, false, false, false, false);
                    }
                    (
                        input.key_pressed(egui::Key::ArrowUp),
                        input.key_pressed(egui::Key::ArrowDown),
                        input.key_pressed(egui::Key::ArrowLeft),
                        input.key_pressed(egui::Key::ArrowRight),
                        input.key_pressed(egui::Key::Home),
                        input.key_pressed(egui::Key::End),
                        input.key_pressed(egui::Key::Enter),
                    )
                });
                let grid_navigation = self.view_mode == "grid";
                let navigation_delta = if grid_navigation {
                    if up {
                        Some(-(grid_columns as isize))
                    } else if down {
                        Some(grid_columns as isize)
                    } else if left {
                        Some(-1)
                    } else if right {
                        Some(1)
                    } else {
                        None
                    }
                } else if up || left {
                    Some(-1)
                } else if down || right {
                    Some(1)
                } else {
                    None
                };
                let library_has_focus = ctx.memory(|memory| memory.has_focus(library_focus_id));
                let search_has_focus =
                    ctx.memory(|memory| memory.has_focus(egui::Id::new("library_search")));
                let keyboard_navigation_enabled = library_has_focus || !search_has_focus;
                let mut keyboard_selection_changed = false;
                if keyboard_navigation_enabled && !matching.is_empty() {
                    let current_index = matching
                        .iter()
                        .position(|row| self.selected == Some(row.id));
                    let target_index = if home {
                        Some(0)
                    } else if end {
                        Some(matching.len() - 1)
                    } else {
                        navigation_delta.map(|delta| {
                            current_index.map_or_else(
                                || {
                                    if delta < 0 {
                                        matching.len() - 1
                                    } else {
                                        0
                                    }
                                },
                                |index| {
                                    (index as isize + delta).clamp(0, matching.len() as isize - 1)
                                        as usize
                                },
                            )
                        })
                    };
                    if let Some(target_index) = target_index {
                        let target = &matching[target_index];
                        keyboard_selection_changed = self.selected != Some(target.id);
                        self.selected = Some(target.id);
                        ctx.memory_mut(|memory| memory.request_focus(library_focus_id));
                        if paging_on {
                            self.current_page = target_index / page_size;
                        }
                    }
                }
                if keyboard_navigation_enabled && enter_key {
                    if let Some(row) = matching
                        .iter()
                        .find(|row| self.selected == Some(row.id))
                        .cloned()
                    {
                        self.run_file(&row);
                    }
                }
                if paging_on {
                    ui.horizontal(|ui| {
                        if ui
                            .add_enabled(self.current_page > 0, egui::Button::new("이전"))
                            .clicked()
                        {
                            self.current_page -= 1;
                        }
                        ui.label(format!(
                            "페이지 {} / {} · {}개 항목",
                            self.current_page + 1,
                            page_count,
                            matching.len()
                        ));
                        if ui
                            .add_enabled(
                                self.current_page + 1 < page_count,
                                egui::Button::new("다음"),
                            )
                            .clicked()
                        {
                            self.current_page += 1;
                        }
                    });
                }
                let page_start = if paging_on {
                    self.current_page * page_size
                } else {
                    0
                };
                let page_items = matching
                    .iter()
                    .skip(page_start)
                    .take(if paging_on { page_size } else { usize::MAX })
                    .cloned()
                    .collect::<Vec<_>>();
                if self.view_mode == "grid" {
                    egui::ScrollArea::vertical()
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            egui::Frame::none()
                                .fill(ui.visuals().faint_bg_color)
                                .inner_margin(egui::Margin::ZERO)
                                .show(ui, |ui| {
                                    egui::Grid::new("library_cards")
                                        .num_columns(grid_columns)
                                        .min_col_width(grid_card_width)
                                        .max_col_width(grid_card_width)
                                        .spacing(egui::vec2(0.0, 0.0))
                                        .show(ui, |ui| {
                                            let mut icon_budget = ICON_LOAD_BUDGET;
                                            for (index, row) in page_items.iter().enumerate() {
                                                self.ensure_icon_texture(
                                                    ctx,
                                                    &row.title_id,
                                                    &mut icon_budget,
                                                );
                                                let selected = self.selected == Some(row.id);
                                                let card_response = egui::Frame::none()
                                                    .fill(if selected {
                                                        ui.visuals().selection.bg_fill
                                                    } else {
                                                        egui::Color32::TRANSPARENT
                                                    })
                                                    .stroke(egui::Stroke::NONE)
                                                    .inner_margin(egui::Margin::same(6.0))
                                                    .show(ui, |ui| {
                                                        ui.set_width(grid_card_width - 12.0);
                                                        ui.set_height(grid_thumbnail_size + 38.0);
                                                        ui.vertical_centered(|ui| {
                                                            if let Some(icon) = self
                                                                .icon_textures
                                                                .get(&row.title_id)
                                                            {
                                                                let response = ui.add(
                                                                    egui::ImageButton::new((
                                                                        icon.id(),
                                                                        egui::vec2(
                                                                            grid_thumbnail_size,
                                                                            grid_thumbnail_size,
                                                                        ),
                                                                    ))
                                                                    .frame(false),
                                                                );
                                                                if response.clicked() {
                                                                    self.selected = Some(row.id);
                                                                    ctx.memory_mut(|memory| {
                                                                        memory.request_focus(
                                                                            library_focus_id,
                                                                        )
                                                                    });
                                                                }
                                                                if response.double_clicked() {
                                                                    self.run_file(row);
                                                                }
                                                                response
                                                                    .on_hover_text(format!(
                                                                        "{}\n{}\n{}",
                                                                        row.title_id,
                                                                        common::format_size(
                                                                            row.size.max(0) as u64
                                                                        ),
                                                                        row.path
                                                                    ))
                                                                    .context_menu(|ui| {
                                                                        self.row_context_menu(
                                                                            ui, row,
                                                                        )
                                                                    });
                                                            } else {
                                                                ui.allocate_ui(
                                                                    egui::vec2(
                                                                        grid_thumbnail_size,
                                                                        grid_thumbnail_size,
                                                                    ),
                                                                    |ui| {
                                                                        ui.centered_and_justified(
                                                                            |ui| {
                                                                                ui.label("🎮");
                                                                            },
                                                                        );
                                                                    },
                                                                );
                                                            }
                                                            let response = ui.selectable_label(
                                                                selected,
                                                                &row.title_name,
                                                            );
                                                            if response.clicked() {
                                                                self.selected = Some(row.id);
                                                                ctx.memory_mut(|memory| {
                                                                    memory.request_focus(
                                                                        library_focus_id,
                                                                    )
                                                                });
                                                            }
                                                            if response.double_clicked() {
                                                                self.run_file(row);
                                                            }
                                                            response
                                                                .on_hover_text(format!(
                                                                    "{}\n{}\n{}",
                                                                    row.title_id,
                                                                    common::format_size(
                                                                        row.size.max(0) as u64
                                                                    ),
                                                                    row.path
                                                                ))
                                                                .context_menu(|ui| {
                                                                    self.row_context_menu(ui, row)
                                                                });
                                                        });
                                                    });
                                                if keyboard_selection_changed && selected {
                                                    ui.scroll_to_rect(
                                                        card_response.response.rect,
                                                        None,
                                                    );
                                                }
                                                if (index + 1) % grid_columns == 0 {
                                                    ui.end_row();
                                                }
                                            }
                                        });
                                });
                        });
                } else {
                    let list_width = ui.available_width();
                    let name_width = (list_width - 552.0).max(240.0);
                    egui::ScrollArea::both().show(ui, |ui| {
                        ui.set_min_width(list_width);
                        egui::Grid::new("library")
                            .striped(true)
                            .spacing(egui::vec2(0.0, 0.0))
                            .show(ui, |ui| {
                                for (label, key, width) in [
                                    ("게임 이름", "name", name_width),
                                    ("타이틀 ID", "id", 120.0),
                                    ("보유 버전", "version", 116.0),
                                    ("최신 버전", "latest_ver", 116.0),
                                    ("크기", "size", 60.0),
                                    ("상태", "status", 140.0),
                                ] {
                                    let marker = if self.sort_column == key {
                                        if self.sort_desc {
                                            " ▼"
                                        } else {
                                            " ▲"
                                        }
                                    } else {
                                        ""
                                    };
                                    let clicked = if key == "name" {
                                        let (rect, response) = ui.allocate_exact_size(
                                            egui::vec2(width, 26.0),
                                            egui::Sense::click(),
                                        );
                                        let visuals = ui.style().interact(&response);
                                        ui.painter().rect(
                                            rect.expand(visuals.expansion),
                                            visuals.rounding,
                                            visuals.weak_bg_fill,
                                            visuals.bg_stroke,
                                        );
                                        ui.painter()
                                            .with_clip_rect(rect.shrink2(egui::vec2(6.0, 0.0)))
                                            .text(
                                                rect.left_center() + egui::vec2(6.0, 0.0),
                                                egui::Align2::LEFT_CENTER,
                                                format!("{label}{marker}"),
                                                egui::TextStyle::Button.resolve(ui.style()),
                                                visuals.text_color(),
                                            );
                                        response.clicked()
                                    } else {
                                        ui.add_sized(
                                            [width, 26.0],
                                            egui::Button::new(format!("{label}{marker}")),
                                        )
                                        .clicked()
                                    };
                                    if clicked {
                                        if self.sort_column == key {
                                            self.sort_desc = !self.sort_desc;
                                        } else {
                                            self.sort_column = key.into();
                                            self.sort_desc = false;
                                        }
                                        self.current_page = 0;
                                    }
                                }
                                ui.end_row();
                                for row in &page_items {
                                    let selected = self.selected == Some(row.id);
                                    let response = selectable_table_text_cell(
                                        ui,
                                        name_width,
                                        24.0,
                                        &row.title_name,
                                        selected,
                                        egui::Align::Min,
                                    );
                                    if response.clicked() {
                                        self.selected = Some(row.id);
                                        ctx.memory_mut(|memory| {
                                            memory.request_focus(library_focus_id)
                                        });
                                    }
                                    if response.double_clicked() {
                                        self.run_file(row);
                                    }
                                    if keyboard_selection_changed && selected {
                                        ui.scroll_to_rect(response.rect, None);
                                    }
                                    response.context_menu(|ui| self.row_context_menu(ui, row));
                                    let response = selectable_table_text_cell(
                                        ui,
                                        120.0,
                                        24.0,
                                        &row.title_id,
                                        selected,
                                        egui::Align::Center,
                                    );
                                    if response.clicked() {
                                        self.selected = Some(row.id);
                                        ctx.memory_mut(|memory| {
                                            memory.request_focus(library_focus_id)
                                        });
                                    }
                                    if response.double_clicked() {
                                        self.run_file(row);
                                    }
                                    response.context_menu(|ui| self.row_context_menu(ui, row));
                                    let installed_version = if row.update_version != 0 {
                                        row.update_version
                                    } else {
                                        row.version
                                    };
                                    let installed_text = common::display_version(
                                        Some(row.nacp_version.as_str()),
                                        Some(installed_version),
                                    );
                                    let response = selectable_table_text_cell(
                                        ui,
                                        116.0,
                                        24.0,
                                        &installed_text,
                                        selected,
                                        egui::Align::Center,
                                    );
                                    if response.clicked() {
                                        self.selected = Some(row.id);
                                        ctx.memory_mut(|memory| {
                                            memory.request_focus(library_focus_id)
                                        });
                                    }
                                    if response.double_clicked() {
                                        self.run_file(row);
                                    }
                                    response.context_menu(|ui| self.row_context_menu(ui, row));
                                    let latest_text = common::version_to_str(
                                        (row.latest_version > 0).then_some(row.latest_version),
                                    );
                                    let response = selectable_table_text_cell(
                                        ui,
                                        116.0,
                                        24.0,
                                        &latest_text,
                                        selected,
                                        egui::Align::Center,
                                    );
                                    if response.clicked() {
                                        self.selected = Some(row.id);
                                        ctx.memory_mut(|memory| {
                                            memory.request_focus(library_focus_id)
                                        });
                                    }
                                    if response.double_clicked() {
                                        self.run_file(row);
                                    }
                                    response.context_menu(|ui| self.row_context_menu(ui, row));
                                    let size_text = common::format_size(row.size.max(0) as u64);
                                    let response = selectable_table_text_cell(
                                        ui,
                                        60.0,
                                        24.0,
                                        &size_text,
                                        selected,
                                        egui::Align::Center,
                                    );
                                    if response.clicked() {
                                        self.selected = Some(row.id);
                                        ctx.memory_mut(|memory| {
                                            memory.request_focus(library_focus_id)
                                        });
                                    }
                                    if response.double_clicked() {
                                        self.run_file(row);
                                    }
                                    response.context_menu(|ui| self.row_context_menu(ui, row));
                                    let dark_mode = ui.visuals().dark_mode;
                                    let unknown_text_color = if dark_mode {
                                        ui.visuals().weak_text_color()
                                    } else {
                                        egui::Color32::from_rgb(110, 119, 129)
                                    };
                                    let update_text_color = if dark_mode {
                                        egui::Color32::from_rgb(244, 174, 94)
                                    } else {
                                        egui::Color32::from_rgb(154, 103, 0)
                                    };
                                    let latest_text_color = if dark_mode {
                                        egui::Color32::from_rgb(100, 208, 139)
                                    } else {
                                        egui::Color32::from_rgb(26, 127, 55)
                                    };
                                    let (status_label, icon_color, text_color, icon) =
                                        if row.latest_version == 0 {
                                            (
                                                "미확인",
                                                egui::Color32::from_rgb(115, 125, 137),
                                                unknown_text_color,
                                                0,
                                            )
                                        } else if installed_version < row.latest_version {
                                            (
                                                "업데이트 가능",
                                                egui::Color32::from_rgb(216, 137, 49),
                                                update_text_color,
                                                1,
                                            )
                                        } else {
                                            (
                                                "최신",
                                                egui::Color32::from_rgb(43, 151, 95),
                                                latest_text_color,
                                                2,
                                            )
                                        };
                                    let (status_rect, status_response) = ui.allocate_exact_size(
                                        egui::vec2(140.0, 24.0),
                                        egui::Sense::click(),
                                    );
                                    paint_selectable_cell(
                                        ui,
                                        status_rect,
                                        &status_response,
                                        selected,
                                    );
                                    let painter = ui.painter();
                                    let icon_center =
                                        status_rect.left_center() + egui::vec2(11.0, 0.0);
                                    painter.circle_filled(icon_center, 7.0, icon_color);
                                    match icon {
                                        1 => {
                                            let stroke =
                                                egui::Stroke::new(1.7_f32, egui::Color32::WHITE);
                                            painter.line_segment(
                                                [
                                                    icon_center + egui::vec2(0.0, 3.5),
                                                    icon_center + egui::vec2(0.0, -3.0),
                                                ],
                                                stroke,
                                            );
                                            painter.line_segment(
                                                [
                                                    icon_center + egui::vec2(-3.0, 0.0),
                                                    icon_center + egui::vec2(0.0, -3.0),
                                                ],
                                                stroke,
                                            );
                                            painter.line_segment(
                                                [
                                                    icon_center + egui::vec2(0.0, -3.0),
                                                    icon_center + egui::vec2(3.0, 0.0),
                                                ],
                                                stroke,
                                            );
                                        }
                                        2 => {
                                            let stroke =
                                                egui::Stroke::new(1.7_f32, egui::Color32::WHITE);
                                            painter.line_segment(
                                                [
                                                    icon_center + egui::vec2(-3.0, 0.0),
                                                    icon_center + egui::vec2(-1.0, 2.0),
                                                ],
                                                stroke,
                                            );
                                            painter.line_segment(
                                                [
                                                    icon_center + egui::vec2(-1.0, 2.0),
                                                    icon_center + egui::vec2(3.0, -2.5),
                                                ],
                                                stroke,
                                            );
                                        }
                                        _ => {
                                            painter.text(
                                                icon_center,
                                                egui::Align2::CENTER_CENTER,
                                                "?",
                                                egui::FontId::proportional(10.0),
                                                egui::Color32::WHITE,
                                            );
                                        }
                                    };
                                    painter.text(
                                        status_rect.left_center() + egui::vec2(23.0, 0.0),
                                        egui::Align2::LEFT_CENTER,
                                        status_label,
                                        egui::TextStyle::Button.resolve(ui.style()),
                                        text_color,
                                    );
                                    if status_response.clicked() {
                                        self.selected = Some(row.id);
                                        ctx.memory_mut(|memory| {
                                            memory.request_focus(library_focus_id)
                                        });
                                    }
                                    if status_response.double_clicked() {
                                        self.run_file(row);
                                    }
                                    status_response
                                        .context_menu(|ui| self.row_context_menu(ui, row));
                                    ui.end_row();
                                }
                            });
                    });
                }
                let duplicate_groups = self.duplicate_title_groups();
                if !duplicate_groups.is_empty() {
                    ui.collapsing(
                        format!("중복 베이스 게임 {}개", duplicate_groups.len()),
                        |ui| {
                            for (title_id, name, paths) in &duplicate_groups {
                                ui.collapsing(
                                    format!("{name} [{title_id}] · {}개 파일", paths.len()),
                                    |ui| {
                                        for path in paths {
                                            ui.label(path);
                                        }
                                    },
                                );
                            }
                        },
                    );
                }
                if let Some(id) = self.selected {
                    if let Some(row) = self.rows.iter().find(|r| r.id == id).cloned() {
                        egui::CollapsingHeader::new("인라인 상세 정보")
                            .default_open(false)
                            .show(ui, |ui| {
                                ui.separator();
                                if let Some(icon) = self.icon_textures.get(&row.title_id) {
                                    ui.image((icon.id(), egui::vec2(120.0, 120.0)));
                                }
                                ui.heading(&row.title_name);
                                ui.label(format!(
                                    "{}  ·  {}  ·  {}",
                                    row.title_id,
                                    common::version_to_str(Some(row.version)),
                                    row.region
                                ));
                                ui.label(&row.path);
                                ui.horizontal(|ui| {
                                    if ui.button("실행").clicked() {
                                        self.run_file(&row);
                                    }
                                    if ui.button("파일명 정규화").clicked() {
                                        match self.rename_target(&row) {
                                            Ok(target) => {
                                                self.rename_preview = Some((
                                                    row.path.clone(),
                                                    target.to_string_lossy().into_owned(),
                                                ))
                                            }
                                            Err(e) => self.status = e.to_string(),
                                        }
                                    }
                                    if ui.button("파일 위치 열기").clicked() {
                                        let _ = std::process::Command::new("explorer")
                                            .arg("/select,")
                                            .arg(&row.path)
                                            .spawn();
                                    }
                                    if ui
                                        .add_enabled(
                                            self.media_result.is_none(),
                                            egui::Button::new("이 게임 스크린샷 다운로드"),
                                        )
                                        .clicked()
                                    {
                                        self.start_media_download(true, Some(&row));
                                    }
                                    if row.has_base
                                        && ui
                                            .add_enabled(
                                                self.media_result.is_none(),
                                                egui::Button::new("이 게임 아이콘 가져오기"),
                                            )
                                            .clicked()
                                    {
                                        self.start_media_download(false, Some(&row));
                                    }
                                    if ["nsz", "xcz"]
                                        .contains(&row.file_type.to_ascii_lowercase().as_str())
                                        && ui
                                            .add_enabled(
                                                self.decompress_result.is_none(),
                                                egui::Button::new("압축 해제"),
                                            )
                                            .clicked()
                                    {
                                        self.start_decompress(PathBuf::from(&row.path));
                                    }
                                    if let Some(converted) = &self.converted_file {
                                        if ui.button("변환 결과 폴더 열기").clicked() {
                                            let _ = std::process::Command::new("explorer")
                                                .arg("/select,")
                                                .arg(converted)
                                                .spawn();
                                        }
                                    }
                                    if ui.button("스크린샷 폴더 열기").clicked() {
                                        let _ = std::process::Command::new("explorer")
                                            .arg(self.screenshot_dir.join(&row.title_id))
                                            .spawn();
                                    }
                                });
                                if row.has_update {
                                    ui.label(format!(
                                        "업데이트 포함: {}",
                                        common::version_to_str(Some(row.update_version))
                                    ));
                                }
                                if row.latest_version > 0 {
                                    ui.label(format!(
                                        "DB 최신 버전: {}",
                                        common::version_to_str(Some(row.latest_version))
                                    ));
                                }
                                if !row.nacp_ko.is_empty() {
                                    ui.label(format!("한국어: {}", row.nacp_ko));
                                }
                                if !row.nacp_en.is_empty() {
                                    ui.label(format!("영어: {}", row.nacp_en));
                                }
                                if !row.nacp_ja.is_empty() {
                                    ui.label(format!("일본어: {}", row.nacp_ja));
                                }
                                if !row.languages.is_empty() {
                                    ui.label(format!("언어: {}", row.languages));
                                }
                                if row.db_dlc_count > 0 {
                                    ui.label(format!(
                                        "보유 DLC: {} / {}",
                                        row.dlc_count, row.db_dlc_count
                                    ));
                                }
                                if !row.owned_dlcs.is_empty() {
                                    ui.collapsing(
                                        format!("보유 DLC 목록 ({})", row.owned_dlcs.len()),
                                        |ui| {
                                            for (id, name) in &row.owned_dlcs {
                                                ui.label(format!("{name} [{id}]"));
                                            }
                                        },
                                    );
                                }
                                if !row.missing_dlcs.is_empty() {
                                    ui.collapsing(
                                        format!("미보유 DLC 목록 ({})", row.missing_dlcs.len()),
                                        |ui| {
                                            for (id, name) in &row.missing_dlcs {
                                                ui.label(format!("{name} [{id}]"));
                                            }
                                        },
                                    );
                                }
                                if let Ok(urls) =
                                    serde_json::from_str::<Vec<String>>(&row.screenshots)
                                {
                                    if let Some(url) = urls.first() {
                                        ui.hyperlink_to("스크린샷 열기", url);
                                    }
                                }
                                if !selected_screenshot_paths.is_empty() {
                                    ui.label("저장된 스크린샷");
                                    ui.horizontal_wrapped(|ui| {
                                        for path in &selected_screenshot_paths {
                                            let key = path.to_string_lossy().into_owned();
                                            if let Some(tex) = self.screenshot_textures.get(&key) {
                                                let texture_size = tex.size_vec2();
                                                let scale = (180.0 / texture_size.x.max(1.0))
                                                    .min(120.0 / texture_size.y.max(1.0));
                                                let response = ui.add(
                                                    egui::Image::new((
                                                        tex.id(),
                                                        texture_size * scale,
                                                    ))
                                                    .sense(egui::Sense::click()),
                                                );
                                                if response.clicked() {
                                                    let _ = std::process::Command::new("explorer")
                                                        .arg(&key)
                                                        .spawn();
                                                }
                                            }
                                        }
                                    });
                                }
                                if !row.description.is_empty() {
                                    ui.label(&row.description);
                                    if ui
                                        .add_enabled(
                                            self.translate_result.is_none(),
                                            egui::Button::new("설명 한국어 번역"),
                                        )
                                        .clicked()
                                    {
                                        self.start_translation(&row);
                                    }
                                }
                                if let Some((id, translated)) = &self.translated_description {
                                    if *id == row.id {
                                        ui.collapsing("한국어 번역", |ui| {
                                            ui.label(translated);
                                        });
                                    }
                                }
                            });
                    }
                }
            }
        });
        if self.config.settings != settings_before || self.folders != folders_before {
            self.config.scan_folders = self.folders.clone();
            if let Err(e) =
                database::save_config(&self.base_dir.join("config.json"), &self.config)
            {
                self.status = format!("설정 저장 실패: {e}");
            }
            let name_mode_after = self
                .config
                .settings
                .get("name_mode")
                .and_then(|v| v.as_str())
                .unwrap_or("db");
            if name_mode_after != name_mode_before || self.folders != folders_before {
                self.start_reload();
            }
        }
        let main_style = ctx.style();
        let mut popup_style = (*main_style).clone();
        popup_style.spacing.window_margin = egui::Margin::symmetric(16.0, 12.0);
        popup_style.spacing.item_spacing = egui::vec2(10.0, 10.0);
        popup_style.spacing.button_padding = egui::vec2(10.0, 6.0);
        popup_style.visuals.window_shadow = egui::Shadow::NONE;
        popup_style.visuals.popup_shadow = egui::Shadow::NONE;
        popup_style.wrap_mode = Some(egui::TextWrapMode::Wrap);
        ctx.set_style(popup_style);
        if let Some((from, to)) = self.rename_preview.clone() {
            show_centered_popup(
                ctx,
                "rename_confirm",
                "파일명 변경 확인",
                None,
                |window| window,
                |ui| {
                    ui.label(
                        std::path::Path::new(&from)
                            .file_name()
                            .unwrap_or_default()
                            .to_string_lossy(),
                    );
                    ui.label("→");
                    ui.label(
                        std::path::Path::new(&to)
                            .file_name()
                            .unwrap_or_default()
                            .to_string_lossy(),
                    );
                    popup_footer(ui, |ui| {
                        ui.horizontal(|ui| {
                            if ui.button("변경").clicked() {
                                self.rename_preview = None;
                                if PathBuf::from(&to).exists() {
                                    self.status = "같은 이름의 파일이 이미 있습니다".into()
                                } else {
                                    match std::fs::rename(&from, &to) {
                                        Ok(()) => match self.db.rename_file_path(&from, &to) {
                                            Ok(()) => {
                                                self.status = "파일명을 정규화했습니다".into();
                                                self.start_reload();
                                            }
                                            Err(e) => {
                                                let _ = std::fs::rename(&to, &from);
                                                self.status = e.to_string()
                                            }
                                        },
                                        Err(e) => self.status = e.to_string(),
                                    }
                                }
                            }
                            if ui.button("취소").clicked() {
                                self.rename_preview = None;
                            }
                        });
                    });
                },
            );
        }
        if let Some(changes) = self.bulk_rename_preview.clone() {
            show_centered_popup(
                ctx,
                "bulk_rename_confirm",
                "파일명 일괄 정규화 확인",
                None,
                |window| window,
                |ui| {
                    ui.label(format!(
                        "검색 및 종류 필터에 맞는 파일 {}개의 이름을 바꿉니다.",
                        changes.len()
                    ));
                    ui.label("새 파일이 이미 있거나 이름 변경에 실패하면 해당 파일은 건너뜁니다.");
                    for (from, to) in changes.iter().take(3) {
                        ui.label(format!(
                            "{} → {}",
                            PathBuf::from(from)
                                .file_name()
                                .unwrap_or_default()
                                .to_string_lossy(),
                            PathBuf::from(to)
                                .file_name()
                                .unwrap_or_default()
                                .to_string_lossy()
                        ));
                    }
                    if changes.len() > 3 {
                        ui.label(format!("외 {}개", changes.len() - 3));
                    }
                    popup_footer(ui, |ui| {
                        ui.horizontal(|ui| {
                            if ui.button("이름 변경").clicked() {
                                self.bulk_rename_preview = None;
                                self.start_bulk_rename(changes);
                            }
                            if ui.button("취소").clicked() {
                                self.bulk_rename_preview = None;
                            }
                        });
                    });
                },
            );
        }
        if let Some(info) = self.titledb_info.clone() {
            let mut open = true;
            show_centered_popup(
                ctx,
                "titledb_info",
                "타이틀 DB 파일 현황",
                Some(&mut open),
                |window| window,
                |ui| {
                    ui.label(info);
                    popup_footer(ui, |ui| {
                        if ui.button("닫기").clicked() {
                            self.titledb_info = None;
                        }
                    });
                },
            );
            if !open {
                self.titledb_info = None;
            }
        }
        if self.clear_titledb_confirm {
            let mut open = self.clear_titledb_confirm;
            show_centered_popup(
                ctx,
                "clear_titledb_confirm",
                "타이틀 DB 초기화 확인",
                Some(&mut open),
                |window| window,
                |ui| {
                    ui.label(
                        "데이터베이스의 타이틀 정보를 비웁니다. 다운로드한 JSON 파일은 유지됩니다.",
                    );
                    popup_footer(ui, |ui| {
                        ui.horizontal(|ui| {
                            if ui
                                .add_enabled(active_tasks.is_empty(), egui::Button::new("초기화"))
                                .clicked()
                            {
                                self.clear_titledb_confirm = false;
                                match self.db.clear_titles() {
                                    Ok(()) => {
                                        self.status = "타이틀 DB를 초기화했습니다".into();
                                        self.start_reload();
                                    }
                                    Err(e) => self.status = e.to_string(),
                                }
                            }
                            if ui.button("취소").clicked() {
                                self.clear_titledb_confirm = false;
                            }
                        });
                    });
                },
            );
            if !open {
                self.clear_titledb_confirm = false;
            }
        }
        if self.clear_library_confirm {
            let mut open = self.clear_library_confirm;
            show_centered_popup(
                ctx,
                "clear_library_confirm",
                "라이브러리 초기화 확인",
                Some(&mut open),
                |window| window,
                |ui| {
                    ui.label("스캔한 게임 목록과 타이틀 DB 정보를 지웁니다. 게임 파일과 아이콘/스크린샷 파일은 유지됩니다.");
                    popup_footer(ui, |ui| {
                        ui.horizontal(|ui| {
                            if ui.button("초기화").clicked() {
                                self.clear_library_confirm = false;
                                match self.db.clear_library_data() {
                                    Ok(()) => {
                                        self.rows.clear();
                                        self.selected = None;
                                        self.status = "라이브러리를 초기화했습니다".into();
                                    }
                                    Err(e) => self.status = e.to_string(),
                                }
                            }
                            if ui.button("취소").clicked() {
                                self.clear_library_confirm = false;
                            }
                        });
                    });
                },
            );
            if !open {
                self.clear_library_confirm = false;
            }
        }
        if let Some(folder) = self.remove_folder_confirm.clone() {
            let mut open = true;
            show_centered_popup(
                ctx,
                "remove_folder_confirm",
                "스캔 폴더 삭제 확인",
                Some(&mut open),
                |window| window,
                |ui| {
                    ui.label(format!("이 폴더를 스캔 목록에서 제거할까요?\n{folder}"));
                    popup_footer(ui, |ui| {
                        ui.horizontal(|ui| {
                            if ui.button("목록에서 제거").clicked() {
                                self.folders.retain(|path| path != &folder);
                                self.settings_folder_selected = None;
                                self.config.scan_folders = self.folders.clone();
                                if self.folder_filter == folder {
                                    self.folder_filter.clear();
                                    self.config.settings.insert(
                                        "last_tab_key".into(),
                                        serde_json::Value::String(String::new()),
                                    );
                                }
                                self.remove_folder_confirm = None;
                                match database::save_config(
                                    &self.base_dir.join("config.json"),
                                    &self.config,
                                ) {
                                    Ok(()) => self.status = format!("폴더 제거: {folder}"),
                                    Err(e) => self.status = e.to_string(),
                                }
                            }
                            if ui.button("취소").clicked() {
                                self.remove_folder_confirm = None;
                            }
                        });
                    });
                },
            );
            if !open {
                self.remove_folder_confirm = None;
            }
        }
        if let Some(message) = self.folder_notice.clone() {
            let mut open = true;
            show_centered_popup(
                ctx,
                "folder_notice",
                "알림",
                Some(&mut open),
                |window| window,
                |ui| {
                    ui.label(message);
                    popup_footer(ui, |ui| {
                        if ui.button("확인").clicked() {
                            self.folder_notice = None;
                        }
                    });
                },
            );
            if !open {
                self.folder_notice = None;
            }
        }
        if self.orphan_cleanup_confirm {
            let mut open = self.orphan_cleanup_confirm;
            show_centered_popup(
                ctx,
                "orphan_cleanup_confirm",
                "사용하지 않는 미디어 캐시 정리",
                Some(&mut open),
                |window| window,
                |ui| {
                    ui.label("현재 라이브러리에 없는 타이틀 ID의 아이콘과 스크린샷 폴더를 삭제합니다. 게임 파일은 삭제하지 않습니다.");
                    popup_footer(ui, |ui| {
                        ui.horizontal(|ui| {
                            if ui
                                .add_enabled(
                                    self.orphan_cleanup_result.is_none(),
                                    egui::Button::new("미디어 캐시 정리"),
                                )
                                .clicked()
                            {
                                self.orphan_cleanup_confirm = false;
                                self.start_orphan_cleanup();
                            }
                            if ui.button("취소").clicked() {
                                self.orphan_cleanup_confirm = false;
                            }
                        });
                    });
                },
            );
            if !open {
                self.orphan_cleanup_confirm = false;
            }
        }
        if let Some(info) = self.update_info.clone() {
            let mut open = true;
            show_centered_popup(
                ctx,
                "rust_update_info",
                format!("Rust 업데이트 v{}", info.latest_version),
                Some(&mut open),
                |window| window.max_width(720.0),
                |ui| {
                    ui.label(format!("현재 버전: v{}", info.current_version));
                    ui.label("Rust 전용 패키지만 설치 후보로 사용합니다.");
                    ui.separator();
                    egui::ScrollArea::vertical()
                        .max_height(300.0)
                        .show(ui, |ui| ui.label(&info.notes));
                    popup_footer(ui, |ui| {
                        ui.horizontal(|ui| {
                            if ui
                                .add_enabled(
                                    self.update_prepare_result.is_none(),
                                    egui::Button::new("Rust 패키지 다운로드 및 확인"),
                                )
                                .clicked()
                            {
                                self.update_info = None;
                                if let Some(url) = info.download_url.clone() {
                                    self.start_update_download(url, info.latest_version.clone());
                                }
                            }
                            if ui.button("나중에").clicked() {
                                self.update_info = None;
                            }
                        });
                    });
                },
            );
            if !open {
                self.update_info = None;
            }
        }
        if let Some(task_dir) = self.update_ready.clone() {
            let mut open = true;
            show_centered_popup(
                ctx,
                "update_ready",
                "업데이트 준비 완료",
                Some(&mut open),
                |window| window,
                |ui| {
                    ui.label("검증된 Rust 패키지를 적용하려면 앱을 종료하고 다시 시작해야 합니다.");
                    popup_footer(ui, |ui| {
                        ui.horizontal(|ui| {
                            if ui.button("적용하고 다시 시작").clicked() {
                                self.update_ready = None;
                                if let Err(e) =
                                    updater::launch_apply_and_exit(&task_dir, &self.base_dir)
                                {
                                    self.status = format!("업데이트 적용 실패: {e}");
                                }
                            }
                            if ui.button("취소").clicked() {
                                self.update_ready = None;
                            }
                        });
                    });
                },
            );
            if !open {
                self.update_ready = None;
            }
        }
        if let Some(info) = self.cache_info.clone() {
            let mut open = true;
            show_centered_popup(
                ctx,
                "cache_info",
                "캐시 현황",
                Some(&mut open),
                |window| window,
                |ui| {
                    ui.monospace(info);
                },
            );
            if !open {
                self.cache_info = None;
            }
        }
        if self.shortcuts_open {
            let mut open = self.shortcuts_open;
            let mut close = false;
            show_centered_popup(
                ctx,
                "shortcuts",
                "단축키",
                Some(&mut open),
                |window| window,
                |ui| {
                    ui.horizontal(|ui| {
                        ui.colored_label(
                            egui::Color32::from_rgb(74, 158, 255),
                            egui::RichText::new("ℹ").size(30.0),
                        );
                        ui.vertical(|ui| {
                            ui.label("빠른 스캔 F5");
                            ui.label("설정 열기/닫기 Ctrl+O");
                            ui.label("검색창 포커스 Ctrl+F");
                            ui.label("종료 Alt+F4");
                            ui.label("검색어 초기화 Esc");
                            ui.label("파일 실행 Enter");
                            ui.label("컨텍스트 메뉴 우클릭");
                        });
                    });
                    popup_footer(ui, |ui| close = ui.button("확인").clicked());
                },
            );
            if close {
                open = false;
            }
            self.shortcuts_open = open;
        }
        if self.about_open {
            let mut open = self.about_open;
            let mut close = false;
            show_centered_popup(
                ctx,
                "about",
                "Nintendo Switch Library 정보",
                Some(&mut open),
                |window| window,
                |ui| {
                    ui.horizontal(|ui| {
                        ui.colored_label(
                            egui::Color32::from_rgb(74, 158, 255),
                            egui::RichText::new("ℹ").size(34.0),
                        );
                        ui.vertical(|ui| {
                            ui.label(format!(
                                "Nintendo Switch Library v{}",
                                env!("CARGO_PKG_VERSION")
                            ));
                            ui.label("닌텐도 스위치 게임 라이브러리 관리 툴");
                            ui.label("지원 형식: NSP, NSZ, XCI, XCZ");
                            ui.label(format!(
                                "DB: {}",
                                self.base_dir.join("library.db").display()
                            ));
                            ui.label(format!(
                                "캐시: {}",
                                self.base_dir.join("caches").display()
                            ));
                        });
                    });
                    popup_footer(ui, |ui| close = ui.button("확인").clicked());
                },
            );
            if close {
                open = false;
            }
            self.about_open = open;
        }
        if self.exit_confirm {
            let mut open = self.exit_confirm;
            show_centered_popup(
                ctx,
                "exit_confirm",
                "진행 중인 작업이 있습니다",
                Some(&mut open),
                |window| window,
                |ui| {
                    ui.label("종료하면 다음 작업이 중단되며 다시 실행해야 할 수 있습니다.");
                    for task in &active_tasks {
                        ui.label(format!("• {task}"));
                    }
                    popup_footer(ui, |ui| {
                        ui.horizontal(|ui| {
                            if ui.button("종료").clicked() {
                                self.exit_confirm = false;
                                self.exit_confirmed = true;
                                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                            }
                            if ui.button("계속 진행").clicked() {
                                self.exit_confirm = false;
                            }
                        });
                    });
                },
            );
            if !open {
                self.exit_confirm = false;
            }
        }
        ctx.set_style(main_style);
    }
}

fn show_centered_popup<'open, R>(
    ctx: &egui::Context,
    id_source: &'static str,
    title: impl Into<egui::WidgetText>,
    open: Option<&'open mut bool>,
    configure: impl FnOnce(egui::Window<'open>) -> egui::Window<'open>,
    add_contents: impl FnOnce(&mut egui::Ui) -> R,
) -> Option<egui::InnerResponse<Option<R>>> {
    let popup_id = egui::Id::new(("nsl_popup", id_source));
    let layer_id = egui::LayerId::new(egui::Order::Middle, popup_id);
    let was_visible = ctx.memory(|memory| memory.areas().visible_last_frame(&layer_id));
    let previous_rect = ctx.memory(|memory| memory.area_rect(popup_id));
    let center = ctx.screen_rect().center();
    let pending_center_id = popup_id.with("center_after_measure");
    let pending_position = ctx.data_mut(|data| data.remove_temp::<egui::Pos2>(pending_center_id));
    let should_center = !was_visible;

    let mut window = egui::Window::new(title)
        .id(popup_id)
        .collapsible(false)
        .auto_sized()
        .max_width(560.0)
        .movable(true);
    if let Some(open) = open {
        window = window.open(open);
    }
    window = configure(window);

    if let Some(position) = pending_position {
        window = window.current_pos(position);
    } else if should_center {
        if let Some(rect) = previous_rect {
            window = window.current_pos(center - rect.size() * 0.5);
        } else {
            window = window.default_pos(center - egui::vec2(240.0, 150.0));
        }
    }

    let response = window.show(ctx, |ui| {
        ui.set_min_width(480.0);
        add_contents(ui)
    });
    if should_center {
        if let Some(response) = &response {
            let rect = response.response.rect;
            let centered_position = center - rect.size() * 0.5;
            if (rect.center() - center).length_sq() > 0.5 {
                ctx.data_mut(|data| data.insert_temp(pending_center_id, centered_position));
                ctx.request_repaint();
            }
        }
    }
    response
}

fn popup_footer(ui: &mut egui::Ui, add_buttons: impl FnOnce(&mut egui::Ui)) {
    ui.add_space(12.0);
    let footer_size = egui::vec2(ui.available_width(), ui.spacing().interact_size.y);
    ui.allocate_ui_with_layout(
        footer_size,
        egui::Layout::right_to_left(egui::Align::Center),
        add_buttons,
    );
}

fn top_bar(app: &mut NslApp, ui: &mut egui::Ui, ctx: &egui::Context, active_tasks: &[&str]) {
    ui.vertical(|ui| {
        ui.scope(|ui| {
            ui.style_mut().visuals.widgets.inactive.weak_bg_fill = egui::Color32::TRANSPARENT;
            ui.style_mut().visuals.widgets.inactive.bg_stroke = egui::Stroke::NONE;
            ui.style_mut().visuals.popup_shadow = egui::Shadow::NONE;
            ui.allocate_ui_with_layout(
                egui::vec2(ui.available_width(), 28.0),
                egui::Layout::left_to_right(egui::Align::Center),
                |ui| {
                    ui.menu_button("파일", |ui| {
                        if ui.button("CSV 내보내기...").clicked() {
                            app.export_csv();
                            ui.close_menu();
                        }
                        if ui.button("파일명 일괄 정규화").clicked() {
                            app.preview_bulk_rename();
                            ui.close_menu();
                        }
                        ui.separator();
                        if ui.button("종료").clicked() {
                            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                            ui.close_menu();
                        }
                    });
                    ui.menu_button("라이브러리", |ui| {
                        let can_scan = app.scan_result.is_none() && app.reload_result.is_none();
                        if ui
                            .add_enabled(can_scan, egui::Button::new("전체 스캔"))
                            .clicked()
                        {
                            app.start_scan(false);
                            ui.close_menu();
                        }
                        if ui
                            .add_enabled(can_scan, egui::Button::new("빠른 스캔"))
                            .clicked()
                        {
                            app.start_scan(true);
                            ui.close_menu();
                        }
                        if ui
                            .add_enabled(app.scan_result.is_some(), egui::Button::new("스캔 중지"))
                            .clicked()
                        {
                            if let Some(cancel) = &app.scan_cancel {
                                cancel.store(true, Ordering::Relaxed);
                                app.status =
                                    "스캔 중지 요청됨 · 현재 분석을 마친 뒤 중지합니다".into();
                            }
                            ui.close_menu();
                        }
                        ui.separator();
                        if ui.button("캐시 현황").clicked() {
                            app.cache_info = Some(app.cache_stats_text());
                            ui.close_menu();
                        }
                        ui.separator();
                        if ui
                            .add_enabled(
                                app.orphan_cleanup_result.is_none(),
                                egui::Button::new("더미 데이터 정리"),
                            )
                            .clicked()
                        {
                            app.orphan_cleanup_confirm = true;
                            ui.close_menu();
                        }
                        ui.separator();
                        if ui
                            .add_enabled(
                                active_tasks.is_empty(),
                                egui::Button::new("라이브러리 초기화"),
                            )
                            .clicked()
                        {
                            app.clear_library_confirm = true;
                            ui.close_menu();
                        }
                    });
                    ui.menu_button("데이터베이스", |ui| {
                        if ui
                            .add_enabled(
                                app.titledb_result.is_none(),
                                egui::Button::new("타이틀 DB 다운로드"),
                            )
                            .clicked()
                        {
                            app.start_titledb_download();
                            ui.close_menu();
                        }
                        if ui
                            .add_enabled(
                                app.titledb_result.is_none(),
                                egui::Button::new("타이틀 DB 매칭"),
                            )
                            .clicked()
                        {
                            app.start_local_titledb_import();
                            ui.close_menu();
                        }
                        if ui.button("타이틀 DB 현황").clicked() {
                            app.titledb_info =
                                Some(local_titledb_status(&app.base_dir.join("titledb")));
                            ui.close_menu();
                        }
                        ui.separator();
                        if ui
                            .add_enabled(
                                app.media_result.is_none(),
                                egui::Button::new("전체 아이콘 스캔"),
                            )
                            .clicked()
                        {
                            app.start_media_download(false, None);
                            ui.close_menu();
                        }
                        if ui
                            .add_enabled(
                                app.media_result.is_none(),
                                egui::Button::new("전체 스크린샷 다운로드"),
                            )
                            .clicked()
                        {
                            app.start_media_download(true, None);
                            ui.close_menu();
                        }
                        ui.separator();
                        if ui
                            .add_enabled(
                                app.titledb_result.is_none(),
                                egui::Button::new("타이틀 DB 초기화"),
                            )
                            .clicked()
                        {
                            app.clear_titledb_confirm = true;
                            ui.close_menu();
                        }
                    });
                    ui.menu_button("도움말", |ui| {
                        if ui
                            .add_enabled(
                                !cfg!(debug_assertions)
                                    && app.update_check_result.is_none()
                                    && app.update_prepare_result.is_none(),
                                egui::Button::new("업데이트 확인"),
                            )
                            .clicked()
                        {
                            app.start_update_check();
                            ui.close_menu();
                        }
                        ui.separator();
                        if ui.button("단축키").clicked() {
                            app.shortcuts_open = true;
                            ui.close_menu();
                        }
                        if ui.button("정보").clicked() {
                            app.about_open = true;
                            ui.close_menu();
                        }
                    });
                },
            );
        });
        ui.separator();
        ui.scope(|ui| {
            ui.spacing_mut().button_padding = egui::vec2(10.0, 5.0);
            ui.spacing_mut().interact_size.y = 30.0;
            ui.allocate_ui_with_layout(
                egui::vec2(ui.available_width(), 34.0),
                egui::Layout::left_to_right(egui::Align::Center),
                |ui| {
                    ui.add_space(14.0);
                    ui.label(
                        egui::RichText::new("Nintendo Switch Library")
                            .size(14.0)
                            .strong(),
                    );
                    ui.separator();
                    let can_scan = app.scan_result.is_none() && app.reload_result.is_none();
                    let scan_fill = ui.visuals().hyperlink_color;
                    let scan_text = contrasting_button_text(scan_fill);
                    ui.spacing_mut().button_padding = egui::vec2(12.0, 6.0);
                    if ui
                        .add_enabled(
                            can_scan,
                            egui::Button::new(egui::RichText::new("빠른 스캔").color(scan_text))
                                .fill(scan_fill),
                        )
                        .clicked()
                    {
                        app.start_scan(true);
                    }
                    ui.spacing_mut().button_padding = egui::vec2(10.0, 5.0);
                    if ui.button("DB 매칭").clicked() {
                        app.start_local_titledb_import();
                    }
                    if ui
                        .button(if app.view_mode == "grid" {
                            "그리드뷰"
                        } else {
                            "리스트뷰"
                        })
                        .clicked()
                    {
                        app.view_mode = if app.view_mode == "grid" {
                            "list"
                        } else {
                            "grid"
                        }
                        .into();
                        app.config.settings.insert(
                            "view_mode".into(),
                            serde_json::Value::String(app.view_mode.clone()),
                        );
                        let _ = database::save_config(
                            &app.base_dir.join("config.json"),
                            &app.config,
                        );
                    }
                    if ui.button("설정").clicked() {
                        app.settings_open = !app.settings_open;
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let search = ui.add_sized(
                            [220.0, ui.spacing().interact_size.y],
                            egui::TextEdit::singleline(&mut app.search)
                                .id(egui::Id::new("library_search"))
                                .vertical_align(egui::Align::Center)
                                .hint_text("검색..."),
                        );
                        if app.search_focus {
                            search.request_focus();
                            app.search_focus = false;
                        }
                        ui.label("검색");
                        ui.separator();
                        ui.label(format!(
                            "파일: {}  |  게임: {}  업데이트: {}  DLC: {}  |  {}  |  DB: {}",
                            common::format_count(app.stats.file_count),
                            common::format_count(app.stats.base_count),
                            common::format_count(app.stats.update_count),
                            common::format_count(app.stats.dlc_count),
                            common::format_size(app.stats.total_size.max(0) as u64),
                            common::format_count(app.stats.title_db_count)
                        ));
                    });
                },
            );
        });
        ui.add_space(2.0);
    });
}

fn local_titledb_status(directory: &std::path::Path) -> String {
    let mut lines = vec![format!("저장 위치: {}\n", directory.display())];
    for name in ["titles.json", "versions.json", "KR.ko.json"] {
        let path = directory.join(name);
        match std::fs::metadata(&path) {
            Ok(meta) => {
                let modified = meta
                    .modified()
                    .ok()
                    .map(chrono::DateTime::<chrono::Local>::from)
                    .map(|value| value.format("%Y-%m-%d %H:%M").to_string())
                    .unwrap_or_else(|| "시간 정보 없음".into());
                lines.push(format!("  📄 {name}"));
                lines.push(format!(
                    "     크기: {}  |  수정: {modified}",
                    common::format_size(meta.len())
                ));
            }
            Err(_) => lines.push(format!("  ❌ {name}   (없음)")),
        }
        lines.push(String::new());
    }
    lines.join("\n")
}

/// Runs a scan to completion without a window and prints a summary. Returns the
/// process exit code: 0 on success, 1 when the scan could not start or failed.
fn run_headless_scan(mut app: NslApp, incremental: bool) -> i32 {
    if app.folders.is_empty() {
        eprintln!("error: no scan folders configured");
        return 1;
    }
    if !scanner::has_prod_keys() {
        eprintln!("error: prod.keys not found next to the executable or in %USERPROFILE%\\.switch");
        return 1;
    }
    // A CLI scan must not kick off icon/screenshot prefetching afterwards; the
    // scan-completion handler re-reads these settings.
    app.config.settings.insert(
        "scan_icon".into(),
        serde_json::Value::String("0".into()),
    );
    app.config.settings.insert(
        "scan_screenshot".into(),
        serde_json::Value::String("0".into()),
    );
    app.start_scan(incremental);
    app.pending_icon_prefetch = false;
    app.pending_screenshot_prefetch = false;
    let ctx = egui::Context::default();
    let started = std::time::Instant::now();
    loop {
        ctx.begin_pass(egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1456.0, 900.0),
            )),
            ..Default::default()
        });
        app.ui_frame(&ctx);
        let _ = ctx.end_pass();
        if app.scan_result.is_none()
            && app.reload_result.is_none()
            && app.scan_progress.is_none()
        {
            break;
        }
        if started.elapsed() > std::time::Duration::from_secs(6 * 60 * 60) {
            eprintln!("error: scan timed out");
            return 1;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    let failures = app
        .log_lines
        .iter()
        .filter(|line| line.contains("분석 실패") || line.contains("스캔 오류"))
        .count();
    let stats = match app.db.stats() {
        Ok(stats) => stats,
        Err(error) => {
            eprintln!("error: could not read database stats: {error}");
            return 1;
        }
    };
    println!(
        "scan=done mode={} files={} base={} update={} dlc={} title_db={} failures={} elapsed={:.1}s status={}",
        if incremental { "fast" } else { "full" },
        stats.file_count,
        stats.base_count,
        stats.update_count,
        stats.dlc_count,
        stats.title_db_count,
        failures,
        started.elapsed().as_secs_f64(),
        app.status
    );
    if failures > 0 {
        1
    } else {
        0
    }
}

fn load_window_icon() -> Option<egui::IconData> {    const ICON_BYTES: &[u8] = include_bytes!("../assets/icon.ico");
    let image = image::load_from_memory_with_format(ICON_BYTES, image::ImageFormat::Ico).ok()?;
    let rgba = image.to_rgba8();
    if rgba.width() == 0 || rgba.height() == 0 {
        return None;
    }
    Some(egui::IconData {
        rgba: rgba.as_raw().clone(),
        width: rgba.width(),
        height: rgba.height(),
    })
}

fn main() -> Result<()> {
    let app = NslApp::new()?;
    let _ = decompress::cleanup_workspaces(&common::base_dir().join("workspaces"));

    // Headless scan mode: lets release builds be verified (and scheduled scans be
    // automated) without opening a window.
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    if let Some(position) = args.iter().position(|arg| arg == "--scan") {
        let mode = args.get(position + 1).map(String::as_str).unwrap_or("fast");
        let incremental = !matches!(mode, "full" | "all");
        let exit = run_headless_scan(app, incremental);
        std::process::exit(exit);
    }

    let start_maximized = app
        .config
        .settings
        .get("start_maximized")
        .and_then(|v| v.as_str())
        .unwrap_or("0")
        == "1";
    let mut viewport = egui::ViewportBuilder::default()
        .with_inner_size([1400.0, 860.0])
        .with_min_inner_size([1400.0, 860.0])
        .with_maximized(start_maximized);
    if let Some(icon) = load_window_icon() {
        viewport = viewport.with_icon(icon);
    }
    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };
    let window_title = format!("NSL  v{}", env!("CARGO_PKG_VERSION"));
    eframe::run_native(
        &window_title,
        options,
        Box::new(|creation| {
            install_korean_font(&creation.egui_ctx);
            Ok(Box::new(app))
        }),
    )
    .map_err(|e| anyhow::anyhow!(e.to_string()))
}

fn install_korean_font(ctx: &egui::Context) {
    let mut font_dirs = Vec::new();
    if let Some(windows) = std::env::var_os("WINDIR") {
        font_dirs.push(PathBuf::from(windows).join("Fonts"));
    }
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        font_dirs.push(
            PathBuf::from(local)
                .join("Microsoft")
                .join("Windows")
                .join("Fonts"),
        );
    }
    let font = font_dirs.iter().find_map(|dir| {
        ["malgun.ttf", "NanumGothic.ttf", "NotoSansKR-VF.ttf"]
            .iter()
            .map(|name| dir.join(name))
            .find_map(|path| std::fs::read(path).ok())
    });
    let Some(font) = font else {
        return;
    };

    let mut definitions = egui::FontDefinitions::default();
    let name = "korean_fallback".to_owned();
    definitions
        .font_data
        .insert(name.clone(), egui::FontData::from_owned(font));
    for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
        if let Some(fallbacks) = definitions.families.get_mut(&family) {
            fallbacks.push(name.clone());
        }
    }
    ctx.set_fonts(definitions);
}

#[cfg(test)]
mod runtime_tests {
    use super::*;

    #[test]
    fn normalizes_windows_scan_paths_for_existing_database_keys() {
        #[cfg(windows)]
        assert_eq!(
            normalize_scan_path_text(r"e:/NSW\games/sample.nsp"),
            r"E:\NSW\games\sample.nsp"
        );

        #[cfg(not(windows))]
        assert_eq!(
            normalize_scan_path_text("/games/sample.nsp"),
            "/games/sample.nsp"
        );
    }

    #[test]
    fn cached_scan_reuse_does_not_depend_on_nacp_names() {
        let path = normalize_scan_path_text(r"E:\NSW\sample.nsp");
        let mut records = HashMap::new();
        records.insert(
            path.clone(),
            database::FileRecord {
                path: path.clone(),
                filename: "sample.nsp".into(),
                size: 100,
                mtime: Some(1.0),
                file_type: "nsp".into(),
                scan_folder: r"E:\NSW".into(),
                fast_hash: "cached-hash".into(),
                nacp_ko: String::new(),
                nacp_en: String::new(),
                nacp_ja: String::new(),
                nacp_version: String::new(),
                contents: vec![scanner::Content {
                    title_id: Some("0100D2C0115B6000".into()),
                    version: 0,
                    content_type: "base".into(),
                    source: "test".into(),
                }],
            },
        );

        assert!(cached_record_for_path(&records, &path).is_some());
        records.get_mut(&path).unwrap().fast_hash.clear();
        assert!(cached_record_for_path(&records, &path).is_none());
    }

    /// Drives the real egui `update()` loop headlessly so the rendered screens,
    /// filter paths, and tab transitions are exercised without a window.
    fn run_app_ticks(app: &mut NslApp, ticks: usize) -> egui::Context {
        let ctx = egui::Context::default();
        install_korean_font(&ctx);
        let mut input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1456.0, 900.0),
            )),
            ..Default::default()
        };
        for _ in 0..ticks {
            ctx.begin_pass(input.take());
            app.ui_frame(&ctx);
            let _ = ctx.end_pass();
            input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1456.0, 900.0),
                )),
                ..Default::default()
            };
        }
        ctx
    }

    fn seeded_app() -> (NslApp, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().to_path_buf();
        std::fs::create_dir_all(base.join("icons")).unwrap();
        std::fs::create_dir_all(base.join("screenshots")).unwrap();
        let db = database::Database::open(base.join("library.db")).unwrap();
        db.init().unwrap();
        db.upsert_files(&[
            database::FileRecord {
                path: "E:\\NSW\\Alpha [0100D2C0115B6000][v0][Base].nsz".into(),
                filename: "Alpha [0100D2C0115B6000][v0][Base].nsz".into(),
                size: 4_000_000,
                mtime: Some(1.0),
                file_type: "nsz".into(),
                scan_folder: "E:\\NSW".into(),
                fast_hash: "hash-alpha".into(),
                nacp_ko: "알파".into(),
                nacp_en: "Alpha".into(),
                nacp_ja: String::new(),
                nacp_version: "1.2.0".into(),
                contents: vec![scanner::Content {
                    title_id: Some("0100D2C0115B6000".into()),
                    version: 0,
                    content_type: "base".into(),
                    source: "header_cnmt".into(),
                }],
            },
            database::FileRecord {
                path: "E:\\NSW\\Beta [0100D2C0115B7001][v0][DLC].nsz".into(),
                filename: "Beta [0100D2C0115B7001][v0][DLC].nsz".into(),
                size: 1_000_000,
                mtime: Some(2.0),
                file_type: "nsz".into(),
                scan_folder: "E:\\NSW".into(),
                fast_hash: "hash-beta".into(),
                nacp_ko: "베타 DLC".into(),
                nacp_en: String::new(),
                nacp_ja: String::new(),
                nacp_version: String::new(),
                contents: vec![scanner::Content {
                    title_id: Some("0100D2C0115B7001".into()),
                    version: 0,
                    content_type: "dlc".into(),
                    source: "header_cnmt".into(),
                }],
            },
        ])
        .unwrap();
        let mut config = database::Config {
            scan_folders: vec!["E:\\NSW".into()],
            ..Default::default()
        };
        config
            .settings
            .insert("name_mode".into(), serde_json::Value::String("db".into()));
        config.settings.insert(
            "titledb_source".into(),
            serde_json::Value::String("custom".into()),
        );
        database::save_config(&base.join("config.json"), &config).unwrap();

        let app = NslApp::with_base_dir(base).unwrap();
        (app, dir)
    }

    fn wait_for_rows(app: &mut NslApp, ctx: &egui::Context) {
        for _ in 0..200 {
            ctx.begin_pass(egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1456.0, 900.0),
                )),
                ..Default::default()
            });
            app.ui_frame(ctx);
            let _ = ctx.end_pass();
            if !app.rows.is_empty() && app.reload_result.is_none() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        panic!("library rows did not load");
    }

    #[test]
    fn installed_versions_round_trip_through_the_database() {
        let (app, _dir) = seeded_app();
        let rows = app.db.files("ko_en_db").unwrap();
        let alpha = rows
            .iter()
            .find(|row| row.title_name == "알파")
            .expect("alpha row");
        assert_eq!(alpha.nacp_version, "1.2.0");
        // Cached-record reuse must preserve the version column too, otherwise a
        // fast scan would blank every version it did not re-read.
        let cached = app.db.cached_file_records().unwrap();
        let record = cached
            .values()
            .find(|record| record.filename.contains("Alpha"))
            .expect("cached alpha");
        assert_eq!(record.nacp_version, "1.2.0");
    }

    #[test]
    fn headless_app_loads_library_and_renders_every_screen() {
        let (mut app, _dir) = seeded_app();
        let ctx = run_app_ticks(&mut app, 2);
        wait_for_rows(&mut app, &ctx);
        assert_eq!(app.rows.len(), 2, "both records should be visible");

        // Grid, list, settings, and each section of the settings drawer must render.
        for view in ["grid", "list"] {
            app.view_mode = view.into();
            run_app_ticks(&mut app, 2);
        }
        app.settings_open = true;
        for category in 0..6 {
            app.settings_category = category;
            app.status = format!("settings category {category}");
            run_app_ticks(&mut app, 2);
        }
        app.settings_open = false;
        app.settings_category = 0;

        for tab in 0..4 {
            app.detail_tab = tab;
            run_app_ticks(&mut app, 2);
        }

        app.shortcuts_open = true;
        run_app_ticks(&mut app, 1);
        app.shortcuts_open = false;
        app.about_open = true;
        run_app_ticks(&mut app, 1);
        app.about_open = false;
        app.log_open = true;
        run_app_ticks(&mut app, 1);
    }

    #[test]
    fn first_launch_matches_bundled_title_db_only_once() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().to_path_buf();
        std::fs::create_dir_all(base.join("titledb")).unwrap();
        std::fs::write(
            base.join("titledb").join("titles.json"),
            r#"{"titledb":{"0100D2C0115B6000":{"name":"Bundled Title"}}}"#,
        )
        .unwrap();

        let mut app = NslApp::with_base_dir(base.clone()).unwrap();
        assert!(
            app.titledb_result.is_some(),
            "bundled title database must be imported on first launch"
        );
        let ctx = egui::Context::default();
        for _ in 0..600 {
            ctx.begin_pass(egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1456.0, 900.0),
                )),
                ..Default::default()
            });
            app.ui_frame(&ctx);
            let _ = ctx.end_pass();
            if app.titledb_result.is_none() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(app.titledb_result.is_none(), "title database import stalled");
        assert_eq!(app.db.stats().unwrap().title_db_count, 1);

        // A second construction must not re-import an already populated table.
        let restarted = NslApp::with_base_dir(base).unwrap();
        assert!(
            restarted.titledb_result.is_none(),
            "populated title table must not be re-imported on every launch"
        );
    }

    #[test]
    fn headless_app_filters_search_and_folder_tabs() {
        let (mut app, _dir) = seeded_app();
        let ctx = run_app_ticks(&mut app, 2);
        wait_for_rows(&mut app, &ctx);
        assert_eq!(app.rows.len(), 2);

        // The default library view is base-only, matching the Python build: a
        // standalone DLC file never becomes its own card.
        app.kind_filter = "base".into();
        let base_rows = app
            .rows
            .iter()
            .filter(|row| app.row_matches_current_filter(row))
            .count();
        assert_eq!(base_rows, 1);
        assert!(app
            .rows
            .iter()
            .filter(|row| app.row_matches_current_filter(row))
            .all(|row| row.content_type == "base"));

        // Search matches title name, filename, or title id.
        app.kind_filter.clear();
        app.search = "Beta".into();
        let matches: Vec<_> = library::visible(&app.rows, &app.search)
            .filter(|row| app.row_matches_current_filter(row))
            .map(|row| row.size)
            .collect();
        assert_eq!(matches, vec![1_000_000]);

        app.search = "0100D2C0115B6000".into();
        let by_id = library::visible(&app.rows, &app.search).count();
        assert_eq!(by_id, 1, "title id search must hit exactly one file");
        app.search.clear();

        app.kind_filter = "dlc".into();
        let dlc_rows: Vec<_> = app
            .rows
            .iter()
            .filter(|row| app.row_matches_current_filter(row))
            .collect();
        assert_eq!(
            dlc_rows.len(),
            2,
            "DLC filter lists the owning base card and the DLC file itself"
        );
        assert!(dlc_rows.iter().any(|row| row.dlc_count > 0));

        // An unreachable folder tab must not panic or hide everything.
        app.folder_filter = "MISSING_UPDATES".into();
        run_app_ticks(&mut app, 1);
        app.folder_filter = "Z:\\missing".into();
        run_app_ticks(&mut app, 1);
        app.folder_filter.clear();
        app.kind_filter = "base".into();
        run_app_ticks(&mut app, 1);
        assert_eq!(app.rows.len(), 2);
    }

    /// Full scan of the real local corpus into a scratch database. Ignored by
    /// default because it needs E:\NSW and prod.keys present.
    #[test]
    #[ignore = "requires the local game corpus and prod.keys"]
    fn full_scan_of_real_corpus_matches_existing_library() {
        let (mut app, dir) = seeded_app();
        let base = dir.path().to_path_buf();
        app.folders = vec!["E:\\NSW".into()];
        app.config.scan_folders = app.folders.clone();
        database::save_config(&base.join("config.json"), &app.config).unwrap();
        app.db.clear_library_data().unwrap();

        app.start_scan(false);
        let ctx = egui::Context::default();
        let mut finished = false;
        for _ in 0..120_000 {
            ctx.begin_pass(egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1456.0, 900.0),
                )),
                ..Default::default()
            });
            app.ui_frame(&ctx);
            let _ = ctx.end_pass();
            if app.scan_result.is_none() && app.reload_result.is_none() && app.scan_progress.is_none()
            {
                finished = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        assert!(finished, "scan did not finish: {}", app.status);

        let count = app.rows.len();
        let stats = app.db.stats().unwrap();
        let failures = app
            .log_lines
            .iter()
            .filter(|line| line.contains("분석 실패"))
            .count();
        assert_eq!(failures, 0, "no file may fail analysis; status: {}", app.status);
        assert!(
            count > 700,
            "expected the full library, scanned only {count} rows"
        );
        assert_eq!(stats.base_count, 800, "base rows must match the reference library");
        assert_eq!(stats.update_count, 710, "update rows must match the reference library");
        assert_eq!(stats.dlc_count, 3137, "dlc rows must match the reference library");

        // Every discovered file must carry a fast hash so the next pass can reuse it.
        let cached = app.db.cached_file_records().unwrap();
        assert_eq!(cached.len(), count);
        assert!(cached.values().all(|record| !record.fast_hash.is_empty()));
        assert!(cached
            .values()
            .all(|record| !record.contents.is_empty()));
    }

    /// Incremental pass must reuse the cache written by the full pass, keep the
    /// same totals, and stay much faster than a cold scan.
    #[test]
    #[ignore = "requires the local game corpus and prod.keys"]
    fn incremental_scan_reuses_cache_after_full_scan() {
        let (mut app, dir) = seeded_app();
        let base = dir.path().to_path_buf();
        app.folders = vec!["E:\\NSW".into()];
        app.config.scan_folders = app.folders.clone();
        database::save_config(&base.join("config.json"), &app.config).unwrap();
        app.db.clear_library_data().unwrap();
        let ctx = egui::Context::default();

        let full_start = std::time::Instant::now();
        app.start_scan(false);
        assert!(run_until_idle(&mut app, &ctx), "full scan timed out");
        let full_elapsed = full_start.elapsed();
        let full_stats = app.db.stats().unwrap();

        let incremental_start = std::time::Instant::now();
        app.start_scan(true);
        assert!(run_until_idle(&mut app, &ctx), "incremental scan timed out");
        let incremental_elapsed = incremental_start.elapsed();
        let incremental_stats = app.db.stats().unwrap();

        assert_eq!(full_stats.base_count, 800);
        assert_eq!(incremental_stats.base_count, 800);
        assert_eq!(incremental_stats.update_count, 710);
        assert_eq!(incremental_stats.dlc_count, 3137);
        assert_eq!(
            incremental_stats.file_count, full_stats.file_count,
            "incremental pass must not drop records"
        );
        assert!(
            incremental_elapsed < full_elapsed,
            "incremental scan ({incremental_elapsed:?}) should be faster than the cold scan ({full_elapsed:?})"
        );
        eprintln!(
            "full={full_elapsed:?} incremental={incremental_elapsed:?} files={}",
            incremental_stats.file_count
        );
    }

    fn run_until_idle(app: &mut NslApp, ctx: &egui::Context) -> bool {
        for _ in 0..120_000 {
            ctx.begin_pass(egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1456.0, 900.0),
                )),
                ..Default::default()
            });
            app.ui_frame(ctx);
            let _ = ctx.end_pass();
            if app.scan_result.is_none() && app.reload_result.is_none() && app.scan_progress.is_none()
            {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        false
    }
}
