#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod catalog;
mod common;
mod database;
mod decompress;
mod image_loader;
mod instance;
mod library;
mod media;
mod native_nca;
mod native_package;
mod ncz;
mod progress;
mod scanner;
mod theme;
mod titledb;
mod torrent;
mod translate;
mod updater;
mod xci;

use anyhow::Result;
use eframe::egui;
use image_loader::{ImageLoader, Kind as ImageKind};
use progress::{Snapshot as ProgressSnapshot, TaskProgress};
use rayon::prelude::*;
use std::{
    collections::{HashMap, HashSet, VecDeque},
    io::Read,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc::{self, Receiver, Sender},
        Arc,
    },
};

const ICON_CACHE_MAX: usize = 144;
const ICON_LOAD_BUDGET: usize = 16;
const STORE_COVER_CACHE_MAX: usize = 80;
const STORE_COVER_FETCH_BATCH: usize = 16;
const STORE_COVER_DOWNLOAD_THREADS: usize = 4;
const STORE_COVER_RETRY_DELAY: std::time::Duration = std::time::Duration::from_secs(120);

/// Everything that changes the filtered/sorted library view. When this is
/// unchanged between frames the previous computation is reused.
#[derive(Clone, Debug, PartialEq, Eq)]
struct FilterKey {
    search: String,
    kind: String,
    folder: String,
    sort_column: String,
    sort_desc: bool,
    exception_update: String,
    exception_dlc: String,
    rows_len: usize,
    rows_generation: u64,
}

/// Process memory and CPU usage for the status bar meter.
#[derive(Clone, Copy, Debug, Default)]
struct ResourceSample {
    working_set_bytes: u64,
    cpu_percent: f32,
}

/// The progress shown in the single, persistent bottom status bar.
/// None means busy/indeterminate, not an invented percentage.
#[derive(Clone, Debug)]
struct ProgressView {
    label: String,
    fraction: Option<f32>,
    detail: String,
}

impl ProgressView {
    fn from_task(snapshot: ProgressSnapshot) -> Self {
        let fraction = snapshot
            .total
            .map(|total| (snapshot.completed.min(total) as f64 / total as f64) as f32);
        let detail = if snapshot.bytes {
            match snapshot.total {
                Some(total) => format!(
                    "{} / {}",
                    common::format_size(snapshot.completed),
                    common::format_size(total)
                ),
                None if snapshot.completed > 0 => common::format_size(snapshot.completed),
                None => "진행 중".into(),
            }
        } else {
            match snapshot.total {
                Some(total) => format!("{} / {total}", snapshot.completed.min(total)),
                None => "진행 중".into(),
            }
        };
        Self {
            label: snapshot.label,
            fraction,
            detail,
        }
    }

    fn busy(label: &str) -> Self {
        Self {
            label: label.into(),
            fraction: None,
            detail: "진행 중".into(),
        }
    }

    fn counted(label: &str, completed: u64, total: u64) -> Self {
        if total == 0 {
            return Self::busy(label);
        }
        Self {
            label: label.into(),
            fraction: Some((completed.min(total) as f64 / total as f64) as f32),
            detail: format!("{} / {total}", completed.min(total)),
        }
    }
}

/// Disconnected worker channels must not leave a UI task stuck as busy.
fn poll_background<T>(
    receiver: &Option<Receiver<std::result::Result<T, String>>>,
    label: &str,
) -> Option<std::result::Result<T, String>> {
    use std::sync::mpsc::TryRecvError;
    match receiver.as_ref()?.try_recv() {
        Ok(value) => Some(value),
        Err(TryRecvError::Empty) => None,
        Err(TryRecvError::Disconnected) => {
            Some(Err(format!("{label} 작업이 비정상 종료되었습니다")))
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MediaSelection {
    Icons,
    Screenshots,
    Both,
}

impl MediaSelection {
    fn includes_icons(self) -> bool {
        matches!(self, Self::Icons | Self::Both)
    }

    fn includes_screenshots(self) -> bool {
        matches!(self, Self::Screenshots | Self::Both)
    }
}

type MediaSource = (String, String, String); // title ID, URL/JSON, package path

#[derive(Default)]
struct MediaCollectionPlan {
    icons: Vec<MediaSource>,
    screenshots: Vec<MediaSource>,
}

impl MediaCollectionPlan {
    fn total(&self) -> usize {
        self.icons.len() + self.screenshots.len()
    }
}

/// The same title ID can occur in several rows with different icon and
/// screenshot metadata. Keep the two source lists independent and deduplicated.
fn media_collection_plan(
    rows: &[library::GameFile],
    selected: Option<&library::GameFile>,
    mode: MediaSelection,
) -> MediaCollectionPlan {
    let candidates: Vec<_> = selected.map_or_else(|| rows.iter().collect(), |row| vec![row]);
    let mut icons = std::collections::BTreeMap::<String, (String, String)>::new();
    let mut screenshots = std::collections::BTreeMap::<String, (String, String)>::new();
    for row in candidates {
        if row.title_id.is_empty() {
            continue;
        }
        if mode.includes_icons() && (!row.icon_url.is_empty() || !row.path.is_empty()) {
            let item = icons
                .entry(row.title_id.clone())
                .or_insert_with(|| (row.icon_url.clone(), row.path.clone()));
            if item.0.is_empty() && !row.icon_url.is_empty() {
                item.0 = row.icon_url.clone();
            }
            if item.1.is_empty() && !row.path.is_empty() {
                item.1 = row.path.clone();
            }
        }
        if mode.includes_screenshots()
            && !row.screenshots.trim().is_empty()
            && row.screenshots.trim() != "[]"
        {
            screenshots
                .entry(row.title_id.clone())
                .or_insert_with(|| (row.screenshots.clone(), row.path.clone()));
        }
    }
    MediaCollectionPlan {
        icons: icons
            .into_iter()
            .map(|(id, (url, path))| (id, url, path))
            .collect(),
        screenshots: screenshots
            .into_iter()
            .map(|(id, (urls, path))| (id, urls, path))
            .collect(),
    }
}

#[cfg(windows)]
fn process_resource_sample(
    previous: Option<(std::time::Instant, u64)>,
) -> Option<(ResourceSample, (std::time::Instant, u64))> {
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

/// Files scanned, stale rows removed, cancellation and recoverable errors.
type ScanOutcome = (usize, usize, bool, usize);

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

/// Keep keyboard selection visible inside a virtualized scroll area. An
/// off-screen card cannot be scrolled to via `ui.scroll_to_rect` because
/// `show_rows` deliberately does not build the card until it is visible.
fn scroll_offset_for_selected_row(
    current_offset: f32,
    viewport_height: f32,
    row_index: usize,
    row_stride: f32,
    row_height: f32,
) -> Option<f32> {
    if !current_offset.is_finite()
        || !viewport_height.is_finite()
        || viewport_height <= 0.0
        || !row_stride.is_finite()
        || row_stride <= 0.0
    {
        return None;
    }
    let row_top = row_index as f32 * row_stride;
    let row_bottom = row_top + row_height;
    let margin = ((viewport_height - row_height) / 2.0).clamp(0.0, 4.0);
    let target = if row_top < current_offset + margin {
        (row_top - margin).max(0.0)
    } else if row_bottom > current_offset + viewport_height - margin {
        (row_bottom - viewport_height + margin).max(0.0)
    } else {
        return None;
    };
    ((target - current_offset).abs() > 0.5).then_some(target)
}

fn library_navigation_target(
    selected_index: Option<usize>,
    total: usize,
    page_start: usize,
    page_end: usize,
    step: Option<isize>,
    home: bool,
    end: bool,
) -> Option<usize> {
    if total == 0 {
        return None;
    }
    if home {
        return Some(0);
    }
    if end {
        return Some(total - 1);
    }
    let step = step?;
    let selected = selected_index.filter(|index| page_start <= *index && *index < page_end);
    Some(match selected {
        None if step < 0 => page_end.saturating_sub(1).min(total - 1),
        None => page_start.min(total - 1),
        Some(index) if step < 0 => index.saturating_sub(step.unsigned_abs()),
        Some(index) => index.saturating_add(step as usize).min(total - 1),
    })
}

/// Fit up to five directory tabs plus the fixed library tabs into the
/// available width. Very narrow windows keep horizontal scrolling as a
/// fallback instead of overlapping the controls.
fn fit_directory_tab_widths(desired: &[f32], available: f32) -> Vec<f32> {
    const MIN_TAB_WIDTH: f32 = 44.0;
    if desired.is_empty() {
        return Vec::new();
    }
    let total: f32 = desired.iter().sum();
    if total <= available {
        return desired.to_vec();
    }
    let minimums: Vec<f32> = desired
        .iter()
        .map(|width| width.min(MIN_TAB_WIDTH))
        .collect();
    let minimum_total: f32 = minimums.iter().sum();
    if available <= minimum_total {
        return minimums;
    }
    let fraction = ((available - minimum_total) / (total - minimum_total)).clamp(0.0, 1.0);
    desired
        .iter()
        .zip(minimums)
        .map(|(preferred, minimum)| minimum + (preferred - minimum) * fraction)
        .collect()
}

/// Shorten at Unicode character boundaries while preserving a trailing count
/// such as `(3137)` when the width allows. The full title remains in a tooltip.
fn elide_tab_label(label: &str, text_width: f32, measure: impl Fn(&str) -> f32) -> String {
    if measure(label) <= text_width {
        return label.to_owned();
    }
    const ELLIPSIS: &str = "…";
    if text_width <= measure(ELLIPSIS) {
        return ELLIPSIS.to_owned();
    }
    let (head, suffix) = label
        .rsplit_once(" (")
        .filter(|(_, digits)| {
            digits.ends_with(')')
                && digits[..digits.len() - 1]
                    .chars()
                    .all(|c| c.is_ascii_digit() || c == ',')
        })
        .and_then(|(head, _)| {
            let suffix = &label[head.len()..];
            let first = head.chars().next()?;
            (measure(&format!("{first}{ELLIPSIS}{suffix}")) <= text_width).then_some((head, suffix))
        })
        .unwrap_or((label, ""));

    let boundaries: Vec<usize> = head
        .char_indices()
        .map(|(position, _)| position)
        .chain(std::iter::once(head.len()))
        .collect();
    let mut low = 0usize;
    let mut high = boundaries.len() - 1;
    while low < high {
        let middle = (low + high).div_ceil(2);
        let candidate = format!("{}{ELLIPSIS}{suffix}", &head[..boundaries[middle]]);
        if measure(&candidate) <= text_width {
            low = middle;
        } else {
            high = middle - 1;
        }
    }
    format!("{}{ELLIPSIS}{suffix}", &head[..boundaries[low]])
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
    let text_font = egui::TextStyle::Button.resolve(ui.style());
    let displayed = elide_tab_label(label, (width - 12.0).max(0.0), |text| {
        painter
            .layout_no_wrap(text.to_owned(), text_font.clone(), text_color)
            .size()
            .x
    });
    painter.with_clip_rect(rect.shrink(4.0)).text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        &displayed,
        text_font,
        text_color,
    );
    if displayed == label {
        response
    } else {
        response.on_hover_text(label)
    }
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
    /// Cached, filtered+sorted view of `rows` keyed by the inputs that produce
    /// it. Recomputing 800 cloned GameFile structs every frame was the main
    /// source of the library stutter, so the result is only rebuilt when a
    /// filter, the sort order, or the row set changes.
    filtered_rows: Arc<Vec<library::GameFile>>,
    filtered_key: Option<FilterKey>,
    /// Cached tab labels (total/folder/missing counts). Recomputing them walked
    /// all rows with two string allocations per row, every frame.
    cached_tabs: Vec<(String, String)>,
    cached_tabs_generation: u64,
    /// Cached duplicate groups and missing-resource counts.
    cached_stats_generation: u64,
    cached_stats_exceptions: (String, String),
    cached_missing_updates: usize,
    cached_missing_dlcs: usize,
    cached_duplicates: Vec<(String, String, Vec<String>)>,
    /// Screenshot file list for the selected row. Reading the directory every
    /// frame was a blocking disk call on the UI thread.
    cached_screenshot_paths: Vec<PathBuf>,
    cached_screenshot_selection: Option<i64>,
    /// Bumped whenever `rows` is replaced, so the cached filter view knows the
    /// underlying set changed even if the length happens to match.
    rows_generation: u64,
    folders: Vec<String>,
    config: database::Config,
    selected: Option<i64>,
    /// Reveal a keyboard-selected row once more on the next frame, after the
    /// right-hand detail panel may have changed the grid column count.
    keyboard_reveal_next_frame: Option<i64>,
    detail_tab: usize,
    current_page: usize,
    search: String,
    search_focus: bool,
    search_committed: String,
    search_pending: Option<std::time::Instant>,
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
    titledb_progress: Option<Arc<TaskProgress>>,
    catalog_progress: Option<Arc<TaskProgress>>,
    update_progress: Option<Arc<TaskProgress>>,
    rename_progress: Option<Arc<TaskProgress>>,
    headless_mode: bool,
    last_scan_errors: usize,
    titledb_result: Option<Receiver<std::result::Result<titledb::UpdateOutcome, String>>>,
    titledb_info: Option<String>,
    clear_titledb_confirm: bool,
    clear_library_confirm: bool,
    remove_folder_confirm: Option<String>,
    folder_notice: Option<String>,
    orphan_cleanup_confirm: bool,
    orphan_cleanup_result: Option<Receiver<std::result::Result<(usize, usize), String>>>,
    workspace_cleanup_confirm: bool,
    workspace_cleanup_result: Option<Receiver<std::result::Result<usize, String>>>,
    reload_result: Option<Receiver<std::result::Result<Vec<library::GameFile>, String>>>,
    reload_dirty: bool,
    rename_preview: Option<(String, String)>,
    bulk_rename_preview: Option<Vec<(String, String)>>,
    bulk_rename_result: Option<Receiver<std::result::Result<(usize, usize), String>>>,
    sort_column: String,
    sort_desc: bool,
    media_result: Option<Receiver<std::result::Result<media::MediaOutcome, String>>>,
    media_progress: Option<Arc<media::MediaProgress>>,
    image_loader: ImageLoader,
    icon_textures: HashMap<String, egui::TextureHandle>,
    icon_lru: VecDeque<String>,
    icon_missing: HashSet<String>,
    screenshot_textures: HashMap<String, egui::TextureHandle>,
    screenshot_missing: HashSet<String>,
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
    catalog_state: catalog::CatalogState,
    catalog_load_result: Option<Receiver<Option<catalog::CatalogState>>>,
    catalog_result: Option<Receiver<std::result::Result<usize, String>>>,
    catalog_search: String,
    catalog_search_committed: String,
    catalog_search_cache: Option<String>,
    catalog_matching_indices: Vec<usize>,
    catalog_search_pending: Option<std::time::Instant>,
    catalog_page: usize,
    catalog_genre_filter: String,
    catalog_format_filter: String,
    catalog_scroll_reset: bool,
    catalog_selected: Option<String>,
    catalog_cover_dir: PathBuf,
    catalog_cover_textures: HashMap<String, egui::TextureHandle>,
    catalog_cover_lru: VecDeque<String>,
    catalog_cover_missing: HashSet<String>,
    catalog_cover_retry_after: HashMap<String, std::time::Instant>,
    catalog_cover_queue: Option<Receiver<(usize, Vec<String>)>>,
    torrent_engine: Option<Arc<torrent::TorrentEngine>>,
    torrent_progress: Vec<(String, torrent::TorrentProgress)>,
    download_search: String,
    download_status_filter: String,
    download_scroll_reset: bool,
    torrent_polled_at: Option<std::time::Instant>,
    download_delete_confirm: Option<(String, String)>,
    cache_info: Option<String>,
    shortcuts_open: bool,
    about_open: bool,
    exit_confirm: bool,
    exit_confirmed: bool,
    log_sender: Sender<String>,
    log_receiver: Receiver<String>,
    log_lines: Vec<String>,
    log_open: bool,
    resource_sample: ResourceSample,
    resource_clock: Option<(std::time::Instant, u64)>,
    resource_refresh_at: Option<std::time::Instant>,
}

fn truncate_label(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        text.to_owned()
    } else {
        let cut: String = text.chars().take(max_chars.saturating_sub(1)).collect();
        format!("{cut}…")
    }
}

/// The source catalog bundles many updates and DLCs, so filter using its
/// actual genre/format metadata rather than guessing a content type.
const STORE_GENRES: [(&str, &str); 12] = [
    ("all", "전체 장르"),
    ("Action", "액션"),
    ("Adventure", "어드벤처"),
    ("Role-Playing", "RPG"),
    ("Simulator", "시뮬레이션"),
    ("Strategy", "전략"),
    ("Puzzle", "퍼즐"),
    ("Racing", "레이싱"),
    ("Shooter", "슈팅"),
    ("Sports", "스포츠"),
    ("Visual Novel", "비주얼 노벨"),
    ("Homebrew", "홈브류"),
];
const STORE_FORMATS: [(&str, &str); 6] = [
    ("all", "전체 형식"),
    ("NSP", "NSP"),
    ("NSZ", "NSZ"),
    ("XCI", "XCI"),
    ("XCZ", "XCZ"),
    ("NRO", "NRO"),
];
const DOWNLOAD_STATUSES: [(&str, &str); 5] = [
    ("all", "전체 작업"),
    ("active", "진행 중"),
    ("paused", "일시정지"),
    ("finished", "완료"),
    ("failed", "실패"),
];

fn catalog_matches_filters(
    item: &catalog::CatalogItem,
    query: &str,
    genre: &str,
    format: &str,
) -> bool {
    (query.is_empty()
        || item.title.to_lowercase().contains(query)
        || item
            .base_title_id()
            .is_some_and(|id| id.to_ascii_lowercase().contains(query)))
        && (genre == "all"
            || item
                .genre
                .to_ascii_lowercase()
                .contains(&genre.to_ascii_lowercase()))
        && (format == "all"
            || item.image_format.to_ascii_uppercase().contains(format)
            || item
                .title
                .to_ascii_uppercase()
                .contains(&format!("[{format}]")))
}

fn download_matches_filters(
    key: &str,
    progress: &torrent::TorrentProgress,
    query: &str,
    status: &str,
) -> bool {
    let matches_status = match status {
        "active" => !progress.finished && !progress.paused && progress.error.is_none(),
        "paused" => !progress.finished && progress.paused && progress.error.is_none(),
        "finished" => progress.finished && progress.error.is_none(),
        "failed" => progress.error.is_some(),
        _ => true,
    };
    matches_status
        && (query.is_empty()
            || progress.name.to_lowercase().contains(query)
            || key.to_lowercase().contains(query))
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

/// Bumped whenever a scan starts producing metadata that older cached records
/// cannot have. A stored version below this forces one full re-analysis so the
/// new data gets populated; afterwards cache hits are trusted again.
const SCAN_SCHEMA_VERSION: &str = "2";

impl NslApp {
    fn ensure_icon_texture(&mut self, ctx: &egui::Context, title_id: &str, budget: &mut usize) {
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
    /// Install only a few finished thumbnails per frame. File I/O and image
    /// decoding run on two dedicated workers; rendering never waits for disk.
    fn poll_loaded_images(&mut self, ctx: &egui::Context) {
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
        titledb::recover_transaction(&base_dir.join("titledb"), &db)?;
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
            || saved_folder_filter == "CATALOG"
            || saved_folder_filter == "DOWNLOADS"
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
        std::fs::create_dir_all(icon_dir.join("store"))?;
        let mut app = Self {
            db,
            base_dir: base_dir.clone(),
            stats,
            rows: Vec::new(),
            filtered_rows: Arc::new(Vec::new()),
            filtered_key: None,
            cached_tabs: Vec::new(),
            cached_tabs_generation: u64::MAX,
            cached_stats_generation: u64::MAX,
            cached_stats_exceptions: (String::new(), String::new()),
            cached_missing_updates: 0,
            cached_missing_dlcs: 0,
            cached_duplicates: Vec::new(),
            cached_screenshot_paths: Vec::new(),
            cached_screenshot_selection: None,
            rows_generation: 0,
            folders,
            config,
            selected: None,
            keyboard_reveal_next_frame: None,
            detail_tab,
            current_page: 0,
            search: String::new(),
            search_focus: false,
            search_committed: String::new(),
            search_pending: None,
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
            titledb_progress: None,
            catalog_progress: None,
            update_progress: None,
            rename_progress: None,
            headless_mode: false,
            last_scan_errors: 0,
            titledb_result: None,
            titledb_info: None,
            clear_titledb_confirm: false,
            clear_library_confirm: false,
            remove_folder_confirm: None,
            folder_notice: None,
            orphan_cleanup_confirm: false,
            orphan_cleanup_result: None,
            workspace_cleanup_confirm: false,
            workspace_cleanup_result: None,
            reload_result: None,
            reload_dirty: false,
            rename_preview: None,
            bulk_rename_preview: None,
            bulk_rename_result: None,
            sort_column: "name".into(),
            sort_desc: false,
            media_result: None,
            media_progress: None,
            image_loader: ImageLoader::new(),
            icon_textures: HashMap::new(),
            icon_lru: VecDeque::new(),
            icon_missing: HashSet::new(),
            screenshot_textures: HashMap::new(),
            screenshot_missing: HashSet::new(),
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
            catalog_state: catalog::CatalogState::default(),
            catalog_load_result: None,
            catalog_result: None,
            catalog_search: String::new(),
            catalog_search_committed: String::new(),
            catalog_search_cache: None,
            catalog_matching_indices: Vec::new(),
            catalog_search_pending: None,
            catalog_page: 0,
            catalog_genre_filter: "all".into(),
            catalog_format_filter: "all".into(),
            catalog_scroll_reset: false,
            catalog_selected: None,
            catalog_cover_dir: base_dir.join("icons").join("store"),
            catalog_cover_textures: HashMap::new(),
            catalog_cover_lru: VecDeque::new(),
            catalog_cover_missing: HashSet::new(),
            catalog_cover_retry_after: HashMap::new(),
            catalog_cover_queue: None,
            torrent_engine: None,
            torrent_progress: Vec::new(),
            download_search: String::new(),
            download_status_filter: "all".into(),
            download_scroll_reset: false,
            torrent_polled_at: None,
            download_delete_confirm: None,
            cache_info: None,
            shortcuts_open: false,
            about_open: false,
            exit_confirm: false,
            exit_confirmed: false,
            log_sender,
            log_receiver,
            log_lines: Vec::new(),
            log_open: false,
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
        // Large store JSON files parse off the startup/UI thread. The game
        // library is immediately usable while the catalog's text arrives.
        let (catalog_sender, catalog_receiver) = mpsc::channel();
        let catalog_dir = app.base_dir.clone();
        app.catalog_load_result = Some(catalog_receiver);
        std::thread::spawn(move || {
            let _ = catalog_sender.send(catalog::load(&catalog_dir));
        });
        // Clearing persisted torrent metadata while rqbit is running races its
        // background writes. Perform an explicitly requested reset BEFORE the
        // engine starts on the next launch; downloaded files are untouched.
        if app
            .config
            .settings
            .get("clear_torrent_session_on_next_launch")
            .and_then(|v| v.as_str())
            == Some("1")
        {
            match torrent::TorrentEngine::clear_session_state(&app.base_dir.join("downloads")) {
                Ok(freed) => {
                    app.config
                        .settings
                        .remove("clear_torrent_session_on_next_launch");
                    if let Err(error) =
                        database::save_config(&app.base_dir.join("config.json"), &app.config)
                    {
                        let _ = app
                            .log_sender
                            .send(format!("세션 초기화 설정 저장 실패: {error}"));
                    }
                    let _ = app
                        .log_sender
                        .send(format!("토렌트 이전 세션 초기화 완료: {} bytes", freed));
                }
                Err(error) => {
                    let _ = app
                        .log_sender
                        .send(format!("토렌트 세션 초기화 실패: {error}"));
                }
            }
        }
        match torrent::TorrentEngine::new(app.base_dir.join("downloads")) {
            Ok(engine) => app.torrent_engine = Some(Arc::new(engine)),
            Err(error) => {
                let _ = app
                    .log_sender
                    .send(format!("⚠ 토렌트 엔진 시작 실패: {error}"));
            }
        }
        Ok(app)
    }
    fn start_reload(&mut self) {
        if self.reload_result.is_some() {
            // Keep a follow-up request if this query predates a DB write.
            self.reload_dirty = true;
            return;
        }
        self.reload_dirty = false;
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
        if self.titledb_result.is_some()
            || self.bulk_rename_result.is_some()
            || self.orphan_cleanup_result.is_some()
            || self.media_result.is_some()
        {
            self.status = "타이틀 DB·파일명 변경·캐시 정리가 끝난 뒤 스캔할 수 있습니다".into();
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
        self.pending_icon_prefetch = false;
        self.pending_screenshot_prefetch = false;
        let db = self.db.clone();
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
        // A one-time full re-analysis after the scan schema changes; otherwise
        // cached records are reused normally.
        let stored_schema = self
            .config
            .settings
            .get("scan_schema")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_owned();
        let force_reanalyze = stored_schema != SCAN_SCHEMA_VERSION;
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
                // One cache read serves metadata comparison AND content reuse.
                // Building two large HashMaps duplicated path strings and DB IO.
                let cached_records = db
                    .cached_file_records()?
                    .into_iter()
                    .map(|(path, mut record)| {
                        let path = normalize_scan_path_text(&path);
                        record.path = path.clone();
                        (path, record)
                    })
                    .collect::<HashMap<_, _>>();
                let mut discovered = Vec::new();
                let mut existing = Vec::new();
                let mut skipped = 0usize;
                let mut candidates = Vec::new();
                let mut scanned_roots: Vec<String> = Vec::new();
                let mut scan_errors = 0usize;
                'folders: for folder in &folders {
                    if cancel.load(Ordering::Relaxed) {
                        break;
                    }
                    let root = PathBuf::from(folder);
                    if !root.is_dir() {
                        let _ = log.send(format!(
                            "⚠ 폴더에 접근할 수 없습니다 · {folder} (기록은 유지됩니다)"
                        ));
                        continue;
                    }
                    // An unreadable child must not be treated as a deleted game.
                    let mut complete = true;
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
                    for item in walkdir::WalkDir::new(&root)
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
                    {
                        let entry = match item {
                            Ok(entry) => entry,
                            Err(error) => {
                                complete = false;
                                scan_errors += 1;
                                let _ =
                                    log.send(format!("스캔 접근 오류 (기존 기록 보존): {error}"));
                                continue;
                            }
                        };
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
                        let meta = match entry.metadata() {
                            Ok(meta) => meta,
                            Err(error) => {
                                complete = false;
                                scan_errors += 1;
                                let _ = log.send(format!(
                                    "스캔 메타데이터 오류 (기존 기록 보존): {error}"
                                ));
                                continue;
                            }
                        };
                        let path = normalize_scan_path(entry.path());
                        let mtime = meta
                            .modified()
                            .ok()
                            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                            .map(|d| d.as_secs_f64());
                        existing.push(path.clone());
                        if incremental && !force_reanalyze {
                            if let Some(old) = cached_records.get(&path) {
                                if old.size == meta.len() as i64
                                    && old
                                        .mtime
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
                    if complete {
                        scanned_roots.push(normalize_scan_path(&root));
                    } else {
                        let _ = log.send(format!(
                            "일부 폴더를 읽지 못해 이전 기록 삭제를 생략: {}",
                            root.display()
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
                    let analysis_errors = AtomicUsize::new(0);
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
                                let same_metadata = cached_records.get(path).map(|old| {
                                    old.size == *size
                                        && old
                                            .mtime
                                            .zip(*mtime)
                                            .map(|(old, current)| (old - current).abs() < 1.0)
                                            .unwrap_or(false)
                                }) == Some(true);
                                // A record written before the NACP version column
                                // existed has an empty version that a plain cache
                                // hit would keep forever, so re-read those once.
                                let cache_complete = cached_record.is_some() && !force_reanalyze;
                                let result = if cancel.load(Ordering::Relaxed) {
                                    (None, false)
                                } else if incremental
                                    && cached_record.is_some()
                                    && same_metadata
                                    && cache_complete
                                {
                                    (None, true)
                                } else if let Ok(hash) = scanner::fast_hash(source) {
                                    if let Some(old) = cached_record {
                                        if incremental && old.fast_hash == hash && cache_complete {
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
                                        analysis_errors.fetch_add(1, Ordering::Relaxed);
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
                    scan_errors += analysis_errors.load(Ordering::Relaxed);
                }
                let count = discovered.len();
                if folders.is_empty() {
                    return Ok((0, 0, false, scan_errors));
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
                Ok((count + skipped, removed, was_cancelled, scan_errors))
            })()
            .map_err(|e| e.to_string());
            if let Err(error) = &result {
                let _ = log.send(format!("❌ 스캔 오류 · {error}"));
            }
            let _ = tx.send(result);
        });
    }
    /// Download the torrent catalog snapshot in the background.
    fn start_catalog_download(&mut self) {
        if self.catalog_result.is_some() {
            return;
        }
        // A fresh network snapshot must not be overwritten by an older
        // local-cache parser that was already running during startup.
        self.catalog_load_result = None;
        let base_dir = self.base_dir.clone();
        let source_url = catalog::DEFAULT_CATALOG_URL.to_owned();
        let (tx, rx) = mpsc::channel();
        self.catalog_result = Some(rx);
        let progress = Arc::new(TaskProgress::default());
        progress.begin("게임 카탈로그 다운로드 준비", None, false);
        self.catalog_progress = Some(progress.clone());
        self.status = "게임 카탈로그 다운로드 중…".into();
        let log = self.log_sender.clone();
        std::thread::spawn(move || {
            let result = catalog::download_with_progress(&base_dir, &source_url, Some(&progress))
                .map(|state| state.items.len())
                .map_err(|e| e.to_string());
            let _ = log.send(match &result {
                Ok(count) => format!("✅ 카탈로그 다운로드 · {count}개 항목"),
                Err(error) => format!("❌ 카탈로그 다운로드 실패 · {error}"),
            });
            let _ = tx.send(result);
        });
    }

    /// Make a catalog title safe to use as a download sub-folder name.
    fn sanitize_folder_name(title: &str) -> String {
        let cleaned: String = title
            .chars()
            .map(|c| match c {
                '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*' => ' ',
                c if (c as u32) < 0x20 => ' ',
                c => c,
            })
            .collect();
        let collapsed = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
        let trimmed = truncate_label(collapsed.trim(), 80);
        let trimmed = trimmed.trim_end_matches(['.', ' ']).to_owned();
        if trimmed.is_empty() {
            "untitled".into()
        } else {
            trimmed
        }
    }

    /// Start a torrent download for one catalog entry into its own sub-folder
    /// under the app's `downloads` directory.
    fn start_catalog_torrent(&mut self, item: &catalog::CatalogItem) {
        let Some(engine) = self.torrent_engine.clone() else {
            self.status = "토렌트 엔진을 사용할 수 없습니다".into();
            return;
        };
        if item.magnet.is_empty() {
            self.status = "이 항목에는 마그넷 링크가 없습니다".into();
            return;
        }
        let folder_name = match item.base_title_id() {
            Some(id) => id,
            None => Self::sanitize_folder_name(&item.title),
        };
        // librqbit's `sub_folder` must be relative to its default download root.
        // A stable torrent suffix prevents update/DLC entries overwriting one another.
        let suffix = item
            .info_hash()
            .map(|hash| {
                hash.chars()
                    .filter(|c| c.is_ascii_alphanumeric())
                    .take(12)
                    .collect::<String>()
            })
            .filter(|hash| !hash.is_empty())
            .unwrap_or_else(|| Self::sanitize_folder_name(&item.topic_id));
        let sub_folder = PathBuf::from(format!("{folder_name}-{suffix}"));
        match engine.start_download(&item.title, &item.magnet, sub_folder) {
            Ok(_) => {
                self.status = format!("다운로드 시작: {}", truncate_label(&item.title, 60));
                let _ = self.log_sender.send(format!(
                    "⬇ 다운로드 시작 · {} ({})",
                    truncate_label(&item.title, 60),
                    item.size
                ));
                self.folder_filter = "DOWNLOADS".into();
                self.current_page = 0;
                self.config.settings.insert(
                    "last_tab_key".into(),
                    serde_json::Value::String("DOWNLOADS".into()),
                );
                let _ = database::save_config(&self.base_dir.join("config.json"), &self.config);
            }
            Err(error) => self.status = format!("다운로드 시작 실패: {error}"),
        }
    }

    fn refresh_catalog_matches(&mut self, ctx: &egui::Context) {
        if let Some(deadline) = self.catalog_search_pending {
            if std::time::Instant::now() >= deadline {
                self.catalog_search_pending = None;
                self.catalog_search_committed
                    .clone_from(&self.catalog_search);
            } else {
                ctx.request_repaint_after(std::time::Duration::from_millis(50));
            }
        }
        let query = self.catalog_search_committed.trim().to_lowercase();
        let filter_key = format!(
            "{query}\u{1f}{}\u{1f}{}",
            self.catalog_genre_filter, self.catalog_format_filter
        );
        if self.catalog_search_cache.as_ref() != Some(&filter_key) {
            self.catalog_matching_indices = self
                .catalog_state
                .items
                .iter()
                .enumerate()
                .filter(|(_, item)| {
                    catalog_matches_filters(
                        item,
                        &query,
                        &self.catalog_genre_filter,
                        &self.catalog_format_filter,
                    )
                })
                .map(|(index, _)| index)
                .collect();
            self.catalog_search_cache = Some(filter_key);
            self.catalog_page = 0;
            self.catalog_scroll_reset = true;
        }
    }

    /// Store controls live in their own sidebar, independent of the saved
    /// game-detail splitter. The catalog list below gets all remaining space.
    fn catalog_sidebar_ui(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        egui::ScrollArea::vertical()
            .id_salt("catalog_sidebar_scroll")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.heading(egui::RichText::new("스토어").size(18.0).strong());
                ui.label(
                    egui::RichText::new(format!(
                        "카탈로그 {}개 항목",
                        common::format_count(self.catalog_state.items.len() as i64)
                    ))
                    .small()
                    .color(ui.visuals().weak_text_color()),
                );
                if let Some(updated_at) = self.catalog_state.updated_at {
                    ui.label(
                        egui::RichText::new(format!(
                            "갱신: {}",
                            chrono::DateTime::<chrono::Local>::from(updated_at)
                                .format("%Y-%m-%d %H:%M")
                        ))
                        .small()
                        .color(ui.visuals().weak_text_color()),
                    );
                }
                ui.separator();
                ui.label(egui::RichText::new("타이틀 검색").strong());
                let search = ui.add_sized(
                    [ui.available_width(), 27.0],
                    egui::TextEdit::singleline(&mut self.catalog_search)
                        .id(egui::Id::new("catalog_search"))
                        .hint_text("게임 이름 또는 타이틀 ID"),
                );
                #[cfg(test)]
                ctx.data_mut(|data| {
                    data.insert_temp(egui::Id::new("nsl_test_store_sidebar_search"), search.rect)
                });
                if search.changed() {
                    self.catalog_search_pending =
                        Some(std::time::Instant::now() + std::time::Duration::from_millis(500));
                }
                ui.add_space(6.0);
                ui.label(egui::RichText::new("장르").strong());
                let genre_label = STORE_GENRES
                    .iter()
                    .find(|(key, _)| *key == self.catalog_genre_filter)
                    .map(|(_, label)| *label)
                    .unwrap_or("전체 장르");
                egui::ComboBox::from_id_salt("catalog_genre_filter")
                    .selected_text(genre_label)
                    .width(ui.available_width())
                    .show_ui(ui, |ui| {
                        for &(key, label) in &STORE_GENRES {
                            ui.selectable_value(
                                &mut self.catalog_genre_filter,
                                key.to_owned(),
                                label,
                            );
                        }
                    });
                ui.add_space(6.0);
                ui.label(egui::RichText::new("파일 형식").strong());
                let format_label = STORE_FORMATS
                    .iter()
                    .find(|(key, _)| *key == self.catalog_format_filter)
                    .map(|(_, label)| *label)
                    .unwrap_or("전체 형식");
                egui::ComboBox::from_id_salt("catalog_format_filter")
                    .selected_text(format_label)
                    .width(ui.available_width())
                    .show_ui(ui, |ui| {
                        for &(key, label) in &STORE_FORMATS {
                            ui.selectable_value(
                                &mut self.catalog_format_filter,
                                key.to_owned(),
                                label,
                            );
                        }
                    });
                ui.add_space(8.0);
                if ui.button("검색·분류 초기화").clicked() {
                    self.catalog_search.clear();
                    self.catalog_search_committed.clear();
                    self.catalog_search_pending = None;
                    self.catalog_genre_filter = "all".into();
                    self.catalog_format_filter = "all".into();
                }
                self.refresh_catalog_matches(ctx);
                let total_pages = self.catalog_matching_indices.len().div_ceil(100).max(1);
                self.catalog_page = self.catalog_page.min(total_pages - 1);
                ui.separator();
                ui.label(format!(
                    "검색 결과 {}개",
                    common::format_count(self.catalog_matching_indices.len() as i64)
                ));
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(self.catalog_page > 0, egui::Button::new("이전"))
                        .clicked()
                    {
                        self.catalog_page -= 1;
                        self.catalog_scroll_reset = true;
                    }
                    ui.label(format!("{} / {}", self.catalog_page + 1, total_pages));
                    if ui
                        .add_enabled(
                            self.catalog_page + 1 < total_pages,
                            egui::Button::new("다음"),
                        )
                        .clicked()
                    {
                        self.catalog_page += 1;
                        self.catalog_scroll_reset = true;
                    }
                });
                ui.separator();
                if ui
                    .add_enabled(
                        self.catalog_result.is_none(),
                        egui::Button::new("카탈로그 새로고침"),
                    )
                    .clicked()
                {
                    self.start_catalog_download();
                }
                if !self.torrent_progress.is_empty() && ui.button("다운로드 보기").clicked() {
                    self.folder_filter = "DOWNLOADS".into();
                    self.current_page = 0;
                    self.config.settings.insert(
                        "last_tab_key".into(),
                        serde_json::Value::String("DOWNLOADS".into()),
                    );
                    let _ = database::save_config(&self.base_dir.join("config.json"), &self.config);
                }
                if let Some(topic_id) = self.catalog_selected.as_deref() {
                    if let Some(item) = self
                        .catalog_state
                        .items
                        .iter()
                        .find(|item| item.topic_id == topic_id)
                        .cloned()
                    {
                        ui.separator();
                        ui.label(egui::RichText::new("선택한 타이틀").strong());
                        ui.label(&item.title);
                        for (label, value) in [
                            ("ID", item.title_id.as_deref().unwrap_or("-")),
                            ("장르", item.genre.as_str()),
                            ("형식", item.image_format.as_str()),
                            ("크기", item.size.as_str()),
                        ] {
                            if !value.is_empty() {
                                ui.label(
                                    egui::RichText::new(format!("{label}: {value}"))
                                        .small()
                                        .color(ui.visuals().weak_text_color()),
                                );
                            }
                        }
                        if ui.button("선택한 항목 다운로드").clicked() {
                            self.start_catalog_torrent(&item);
                        }
                    }
                }
                ui.separator();
                ui.label(
                    egui::RichText::new(format!(
                        "저장 위치: {}\\<게임 폴더>",
                        self.base_dir.join("downloads").display()
                    ))
                    .small()
                    .color(ui.visuals().weak_text_color()),
                );
            });
    }

    /// Store tab: show catalog items only; controls are in the right sidebar.
    fn catalog_panel_ui(&mut self, ui: &mut egui::Ui, _ctx: &egui::Context) {
        if self.catalog_state.items.is_empty() {
            ui.add_space(8.0);
            library_section_frame(ui).show(ui, |ui| {
                ui.label(egui::RichText::new("카탈로그 준비").size(14.0).strong());
                if self.catalog_result.is_some() {
                    ui.label("카탈로그를 다운로드하는 중… (최초 1회 약 20MB)");
                } else if self.catalog_load_result.is_some() {
                    ui.label("저장된 게임 목록을 불러오는 중…");
                } else {
                    ui.label("오른쪽 패널에서 카탈로그를 갱신해 게임 목록을 불러오세요.");
                }
            });
            return;
        }
        ui.add_space(4.0);
        let page_size = 100usize;
        // Keep only lightweight indices for the page. Clone heavyweight
        // catalog entries (magnet links and other strings) only for the rows
        // that the virtualized scroll area will actually display.
        let page_indices: Vec<usize> = self
            .catalog_matching_indices
            .iter()
            .skip(self.catalog_page * page_size)
            .take(page_size)
            .copied()
            .collect();
        if page_indices.is_empty() {
            library_section_frame(ui).show(ui, |ui| {
                ui.label("검색·분류 조건과 일치하는 타이틀이 없습니다.");
            });
            return;
        }
        let selected_topic = self.catalog_selected.clone();
        let mut cover_jobs = Vec::new();
        let now = std::time::Instant::now();
        let mut catalog_scroll = egui::ScrollArea::vertical()
            .id_salt("catalog_rows")
            .auto_shrink([false, false]);
        if std::mem::take(&mut self.catalog_scroll_reset) {
            catalog_scroll = catalog_scroll.vertical_scroll_offset(0.0);
        }
        let _catalog_scroll =
            catalog_scroll.show_rows(ui, 72.0, page_indices.len(), |ui, row_range| {
                let mut cover_budget = ICON_LOAD_BUDGET;
                for &index in &page_indices[row_range] {
                    let item = self.catalog_state.items[index].clone();
                    if self.catalog_cover_queue.is_none()
                        && cover_jobs.len() < STORE_COVER_FETCH_BATCH
                        && self.should_prefetch_catalog_cover(&item, now)
                        && !cover_jobs.iter().any(|(id, _)| id == &item.topic_id)
                    {
                        cover_jobs.push((item.topic_id.clone(), item.cover.clone()));
                    }
                    self.ensure_catalog_cover(ui.ctx(), &item.topic_id, &mut cover_budget);
                    let selected = selected_topic
                        .as_deref()
                        .is_some_and(|id| item.topic_id == id);
                    let base_fill = if selected {
                        ui.visuals().selection.bg_fill
                    } else {
                        ui.visuals().faint_bg_color
                    };
                    let border = if selected {
                        ui.visuals().hyperlink_color
                    } else {
                        ui.visuals().widgets.noninteractive.bg_stroke.color
                    };
                    let row_width = ui.available_width();
                    let frame = egui::Frame::none()
                        .fill(base_fill)
                        .stroke(egui::Stroke::new(1.0_f32, border))
                        .rounding(egui::Rounding::same(4.0))
                        .inner_margin(egui::Margin::same(6.0));
                    let _response = frame.show(ui, |ui| {
                        ui.set_width(row_width - 12.0);
                        ui.set_min_height(60.0);
                        ui.horizontal(|ui| {
                            if let Some(cover) = self.catalog_cover_textures.get(&item.topic_id) {
                                ui.add(egui::Image::new((cover.id(), egui::vec2(52.0, 52.0))));
                            } else {
                                ui.allocate_ui(egui::vec2(52.0, 52.0), |ui| {
                                    ui.centered_and_justified(|ui| {
                                        ui.label("🎮");
                                    });
                                });
                            }
                            let label_width = (ui.available_width() - 120.0).max(64.0);
                            ui.allocate_ui_with_layout(
                                egui::vec2(label_width, 54.0),
                                egui::Layout::top_down(egui::Align::Min),
                                |ui| {
                                    let name = ui.add(
                                        egui::Label::new(
                                            egui::RichText::new(&item.title).size(13.0).strong(),
                                        )
                                        .truncate()
                                        .sense(egui::Sense::click()),
                                    );
                                    if name.clicked() {
                                        self.catalog_selected = Some(item.topic_id.clone());
                                    }
                                    name.on_hover_text(&item.title);
                                    let id = item.title_id.as_deref().unwrap_or("-");
                                    let meta = format!(
                                        "{id}   ·   {}   ·   {}",
                                        item.image_format, item.size
                                    );
                                    ui.add(
                                        egui::Label::new(
                                            egui::RichText::new(meta.clone())
                                                .size(11.0)
                                                .color(ui.visuals().weak_text_color()),
                                        )
                                        .truncate(),
                                    )
                                    .on_hover_text(meta);
                                },
                            );
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if ui.button("다운로드").clicked() {
                                        self.start_catalog_torrent(&item);
                                    }
                                },
                            );
                        });
                    });
                    #[cfg(test)]
                    _ctx.data_mut(|data| {
                        data.insert_temp(
                            egui::Id::new("nsl_test_catalog_last_row"),
                            _response.response.rect,
                        );
                        if index == 0 && self.catalog_page == 0 {
                            data.insert_temp(
                                egui::Id::new("nsl_test_catalog_first_row"),
                                _response.response.rect,
                            );
                        }
                    });
                }
            });
        #[cfg(test)]
        _ctx.data_mut(|data| {
            data.insert_temp(
                egui::Id::new("nsl_test_catalog_viewport"),
                _catalog_scroll.inner_rect,
            )
        });
        self.start_cover_prefetch(cover_jobs);
    }

    /// Load a store cover into the texture cache if it is on disk.
    fn ensure_catalog_cover(&mut self, ctx: &egui::Context, topic_id: &str, budget: &mut usize) {
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

    fn safe_catalog_topic_id(id: &str) -> bool {
        !id.is_empty()
            && id.len() <= 80
            && id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    }

    fn should_prefetch_catalog_cover(
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
    fn start_cover_prefetch(&mut self, items: Vec<(String, String)>) {
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

    /// The 중지/재개 · 삭제 button cluster shown on every download row.
    fn download_row_buttons(
        &mut self,
        ui: &mut egui::Ui,
        key: &str,
        progress: &torrent::TorrentProgress,
    ) {
        let Some(engine) = &self.torrent_engine else {
            return;
        };
        let is_done = progress.finished || progress.error.is_some();
        if !is_done {
            if progress.paused {
                if ui.small_button("재개").clicked() {
                    engine.resume(key);
                    self.status = "다운로드 재개 요청됨".into();
                }
            } else if ui.small_button("중지").clicked() {
                engine.pause(key);
                self.status = "다운로드 일시정지 요청됨".into();
            }
            let delete_files_too = self
                .config
                .settings
                .get("download_delete_files")
                .and_then(|v| v.as_str())
                .map(|v| v == "1")
                .unwrap_or(false);
            let label = if delete_files_too {
                "삭제 (파일 포함)"
            } else {
                "삭제"
            };
            if ui.small_button(label).clicked() {
                if delete_files_too {
                    self.download_delete_confirm = Some((key.to_owned(), progress.name.clone()));
                } else {
                    engine.cancel(key, false);
                    self.status = "다운로드 삭제 요청됨 · 받은 파일은 유지됩니다".into();
                    let _ = self.log_sender.send(format!(
                        "🗑 다운로드 삭제 · {}",
                        truncate_label(&progress.name, 60)
                    ));
                }
            }
        } else if ui.small_button("목록에서 지우기").clicked() {
            engine.dismiss(key);
        }
    }

    /// Search, transfer-state filters and aggregate progress live next to
    /// the downloads list, not in a toolbar above its first row.
    fn downloads_sidebar_ui(&mut self, ui: &mut egui::Ui) {
        egui::ScrollArea::vertical()
            .id_salt("downloads_sidebar_scroll")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.heading(egui::RichText::new("다운로드").size(18.0).strong());
                ui.label(
                    egui::RichText::new(format!(
                        "총 {}개 작업",
                        common::format_count(self.torrent_progress.len() as i64)
                    ))
                    .small()
                    .color(ui.visuals().weak_text_color()),
                );
                ui.separator();
                ui.label(egui::RichText::new("타이틀 검색").strong());
                let search = ui.add_sized(
                    [ui.available_width(), 27.0],
                    egui::TextEdit::singleline(&mut self.download_search)
                        .id(egui::Id::new("download_search"))
                        .hint_text("다운로드 이름 검색"),
                );
                #[cfg(test)]
                ui.ctx().data_mut(|data| {
                    data.insert_temp(
                        egui::Id::new("nsl_test_download_sidebar_search"),
                        search.rect,
                    )
                });
                if search.changed() {
                    self.download_scroll_reset = true;
                }
                ui.add_space(6.0);
                ui.label(egui::RichText::new("진행 상태").strong());
                let status_label = DOWNLOAD_STATUSES
                    .iter()
                    .find(|(key, _)| *key == self.download_status_filter)
                    .map(|(_, label)| *label)
                    .unwrap_or("전체 작업");
                let previous_status = self.download_status_filter.clone();
                egui::ComboBox::from_id_salt("download_status_filter")
                    .selected_text(status_label)
                    .width(ui.available_width())
                    .show_ui(ui, |ui| {
                        for &(key, label) in &DOWNLOAD_STATUSES {
                            ui.selectable_value(
                                &mut self.download_status_filter,
                                key.to_owned(),
                                label,
                            );
                        }
                    });
                if self.download_status_filter != previous_status {
                    self.download_scroll_reset = true;
                }
                ui.add_space(8.0);
                if ui.button("검색·분류 초기화").clicked() {
                    self.download_search.clear();
                    self.download_status_filter = "all".into();
                    self.download_scroll_reset = true;
                }
                let query = self.download_search.trim().to_lowercase();
                let count = self
                    .torrent_progress
                    .iter()
                    .filter(|(key, progress)| {
                        download_matches_filters(
                            key,
                            progress,
                            &query,
                            &self.download_status_filter,
                        )
                    })
                    .count();
                ui.separator();
                ui.label(format!(
                    "검색 결과 {}개",
                    common::format_count(count as i64)
                ));
                let active = self
                    .torrent_progress
                    .iter()
                    .filter(|(_, p)| !p.finished && !p.paused && p.error.is_none())
                    .count();
                let paused = self
                    .torrent_progress
                    .iter()
                    .filter(|(_, p)| !p.finished && p.paused && p.error.is_none())
                    .count();
                let finished = self
                    .torrent_progress
                    .iter()
                    .filter(|(_, p)| p.finished && p.error.is_none())
                    .count();
                let failed = self
                    .torrent_progress
                    .iter()
                    .filter(|(_, p)| p.error.is_some())
                    .count();
                for (label, count) in [
                    ("진행 중", active),
                    ("일시정지", paused),
                    ("완료", finished),
                    ("실패", failed),
                ] {
                    ui.label(
                        egui::RichText::new(format!("{label}  {count}"))
                            .small()
                            .color(ui.visuals().weak_text_color()),
                    );
                }
                let total_speed: u64 = self
                    .torrent_progress
                    .iter()
                    .filter(|(_, p)| !p.finished && !p.paused && p.error.is_none())
                    .map(|(_, p)| p.download_speed_bps)
                    .sum();
                if active > 0 {
                    ui.label(format!("전체 속도  {}/s", common::format_size(total_speed)));
                }
                ui.separator();
                if ui.button("스토어로 이동").clicked() {
                    self.folder_filter = "CATALOG".into();
                    self.current_page = 0;
                    self.config.settings.insert(
                        "last_tab_key".into(),
                        serde_json::Value::String("CATALOG".into()),
                    );
                    let _ = database::save_config(&self.base_dir.join("config.json"), &self.config);
                }
                ui.separator();
                ui.label(
                    egui::RichText::new(format!(
                        "저장 위치: {}\\<게임 폴더>",
                        self.base_dir.join("downloads").display()
                    ))
                    .small()
                    .color(ui.visuals().weak_text_color()),
                );
            });
    }

    /// Dedicated downloads tab: the center is reserved for torrent rows.
    fn downloads_panel_ui(&mut self, ui: &mut egui::Ui) {
        if self.torrent_progress.is_empty() {
            ui.add_space(8.0);
            library_section_frame(ui).show(ui, |ui| {
                ui.label(
                    egui::RichText::new("다운로드 작업이 없습니다")
                        .size(14.0)
                        .strong(),
                );
                ui.label("스토어 탭에서 게임 업데이트나 DLC를 받으면 여기에 표시됩니다.");
            });
            return;
        }
        let query = self.download_search.trim().to_lowercase();
        let entries: Vec<_> = self
            .torrent_progress
            .iter()
            .filter(|(key, progress)| {
                download_matches_filters(key, progress, &query, &self.download_status_filter)
            })
            .cloned()
            .collect();
        if entries.is_empty() {
            ui.add_space(8.0);
            library_section_frame(ui).show(ui, |ui| {
                ui.label("검색·분류 조건과 일치하는 다운로드가 없습니다.");
            });
            return;
        }
        ui.add_space(4.0);
        let mut download_scroll = egui::ScrollArea::vertical()
            .id_salt("downloads_rows")
            .auto_shrink([false, false]);
        if std::mem::take(&mut self.download_scroll_reset) {
            download_scroll = download_scroll.vertical_scroll_offset(0.0);
        }
        download_scroll.show(ui, |ui| {
            for (key, progress) in &entries {
                let row_width = ui.available_width();
                let frame = library_section_frame(ui);
                let _row = frame.show(ui, |ui| {
                    ui.set_width((row_width - 20.0).max(200.0));
                    let label = if progress.name.is_empty() {
                        key.as_str()
                    } else {
                        &progress.name
                    };
                    let compact = ui.available_width() < 440.0;
                    if compact {
                        ui.add(
                            egui::Label::new(egui::RichText::new(label).size(13.0).strong())
                                .truncate(),
                        )
                        .on_hover_text(label);
                        ui.horizontal(|ui| self.download_row_buttons(ui, key, progress));
                    } else {
                        ui.horizontal(|ui| {
                            let label_width = (ui.available_width() - 205.0).max(96.0);
                            ui.add_sized(
                                [label_width, 22.0],
                                egui::Label::new(egui::RichText::new(label).size(13.0).strong())
                                    .truncate(),
                            )
                            .on_hover_text(label);
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| self.download_row_buttons(ui, key, progress),
                            );
                        });
                    }
                    if progress.total_bytes > 0 {
                        let fraction =
                            progress.progress_bytes as f32 / progress.total_bytes.max(1) as f32;
                        if progress.finished {
                            ui.add(
                                egui::ProgressBar::new(1.0)
                                    .desired_height(17.0)
                                    .desired_width(ui.available_width())
                                    .fill(egui::Color32::from_rgb(46, 160, 67))
                                    .text(format!(
                                        "완료 {}",
                                        common::format_size(progress.total_bytes)
                                    )),
                            );
                        } else {
                            ui.add(
                                egui::ProgressBar::new(fraction.clamp(0.0, 1.0))
                                    .desired_height(17.0)
                                    .desired_width(ui.available_width())
                                    .text(format!(
                                        "{} / {} · {}/s",
                                        common::format_size(progress.progress_bytes),
                                        common::format_size(progress.total_bytes),
                                        common::format_size(progress.download_speed_bps),
                                    )),
                            );
                        }
                    } else {
                        ui.label(
                            egui::RichText::new("크기 확인 중… (메타데이터 받는 중)")
                                .small()
                                .color(ui.visuals().weak_text_color()),
                        );
                    }
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new(&progress.state)
                                .small()
                                .color(ui.visuals().weak_text_color()),
                        );
                        if let Some(error) = &progress.error {
                            ui.add(
                                egui::Label::new(
                                    egui::RichText::new(error)
                                        .small()
                                        .color(egui::Color32::LIGHT_RED),
                                )
                                .truncate(),
                            )
                            .on_hover_text(error);
                        }
                    });
                });
                #[cfg(test)]
                ui.ctx().data_mut(|data| {
                    data.insert_temp(
                        egui::Id::new("nsl_test_download_last_row"),
                        _row.response.rect,
                    )
                });
            }
        });
    }

    fn start_titledb_download(&mut self) {
        if self.titledb_result.is_some() {
            return;
        }
        if self.scan_result.is_some()
            || self.bulk_rename_result.is_some()
            || self.media_result.is_some()
        {
            self.status = "스캔 또는 파일명 변경이 끝난 뒤 타이틀 DB를 갱신하세요".into();
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
        let progress = Arc::new(TaskProgress::default());
        progress.begin("타이틀 DB 다운로드 준비", None, false);
        self.titledb_progress = Some(progress.clone());
        self.status = "타이틀 DB 다운로드 중…".into();
        let log = self.log_sender.clone();
        std::thread::spawn(move || {
            let result = titledb::download_and_apply(&dir, &source, &db, Some(&progress))
                .map_err(|e| e.to_string());
            let _ = log.send(match &result {
                Ok(titledb::UpdateOutcome::AlreadyLatest) => {
                    "ℹ️ 타이틀 DB는 이미 최신입니다".into()
                }
                Ok(titledb::UpdateOutcome::Updated(count)) => {
                    format!("✅ 타이틀 DB 다운로드 및 반영 · {count}개 항목")
                }
                Err(error) => format!("❌ 타이틀 DB 다운로드 실패 · {error}"),
            });
            let _ = tx.send(result);
        });
    }
    fn start_local_titledb_import(&mut self) {
        if self.titledb_result.is_some() {
            return;
        }
        if self.scan_result.is_some()
            || self.bulk_rename_result.is_some()
            || self.media_result.is_some()
        {
            self.status = "스캔 또는 파일명 변경이 끝난 뒤 타이틀 DB를 매칭하세요".into();
            return;
        }
        let dir = self.base_dir.join("titledb");
        let db = self.db.clone();
        let (tx, rx) = mpsc::channel();
        self.titledb_result = Some(rx);
        let progress = Arc::new(TaskProgress::default());
        progress.begin("로컬 타이틀 DB 준비", None, false);
        self.titledb_progress = Some(progress.clone());
        self.status = "로컬 타이틀 DB 읽는 중…".into();
        let log = self.log_sender.clone();
        std::thread::spawn(move || {
            let result = titledb::import_local_if_needed(&dir, &db, Some(&progress))
                .map_err(|e| e.to_string());
            let _ = log.send(match &result {
                Ok(titledb::UpdateOutcome::Updated(count)) => {
                    format!("✅ 로컬 타이틀 DB 매칭 · {count}개 항목")
                }
                Ok(titledb::UpdateOutcome::AlreadyLatest) => {
                    "ℹ️ 타이틀 DB는 이미 최신입니다".into()
                }
                Err(error) => format!("❌ 로컬 타이틀 DB 매칭 실패 · {error}"),
            });
            let _ = tx.send(result);
        });
    }
    fn start_media_collection(
        &mut self,
        mode: MediaSelection,
        selected: Option<&library::GameFile>,
    ) {
        if self.media_result.is_some() {
            return;
        }
        if self.scan_result.is_some()
            || self.titledb_result.is_some()
            || self.orphan_cleanup_result.is_some()
            || self.bulk_rename_result.is_some()
        {
            self.status = "스캔·DB 갱신·캐시 정리가 끝난 뒤 미디어를 수집하세요".into();
            return;
        }
        let plan = media_collection_plan(&self.rows, selected, mode);
        if plan.total() == 0 {
            self.status = "수집할 아이콘 또는 스크린샷이 없습니다".into();
            return;
        }
        // Manual collection supersedes pending automatic prefetch, avoiding a
        // redundant second pass over exactly the same game library.
        if selected.is_none() {
            self.pending_icon_prefetch = false;
            self.pending_screenshot_prefetch = false;
        }
        let icon_dir = self.icon_dir.clone();
        let screenshot_dir = self.screenshot_dir.clone();
        let total = media::estimated_total(&plan.icons, &plan.screenshots);
        let log = self.log_sender.clone();
        let (tx, rx) = mpsc::channel();
        self.media_result = Some(rx);
        let progress = Arc::new(media::MediaProgress::default());
        progress.begin(total);
        self.media_progress = Some(progress.clone());
        self.status = format!("미디어 수집 중… 0 / {total}");
        std::thread::spawn(move || {
            let result = media::collect_media(
                &icon_dir,
                &screenshot_dir,
                &plan.icons,
                &plan.screenshots,
                &progress,
            );
            let prefix = if result.cancelled {
                "⚠ 미디어 수집 중지"
            } else if result.failed > 0 {
                "⚠ 미디어 수집 일부 실패"
            } else {
                "✅ 미디어 수집 완료"
            };
            let _ = log.send(format!(
                "{prefix} · 아이콘 {}개 · 스크린샷 {}장 저장 · 캐시 재사용 {}개 · 실패 {}개",
                result.icons, result.screenshots, result.skipped, result.failed
            ));
            for message in progress.error_samples() {
                let _ = log.send(format!("⚠ {message}"));
            }
            let _ = tx.send(Ok(result));
        });
    }

    fn start_pending_media(&mut self) {
        // Scan completion reloads the DB asynchronously. Wait for fresh rows
        // instead of collecting stale/empty metadata during the same frame.
        if self.media_result.is_some()
            || self.reload_result.is_some()
            || self.reload_dirty
            || self.scan_result.is_some()
            || self.titledb_result.is_some()
            || self.orphan_cleanup_result.is_some()
            || self.bulk_rename_result.is_some()
        {
            return;
        }
        let mode = match (self.pending_icon_prefetch, self.pending_screenshot_prefetch) {
            (true, true) => Some(MediaSelection::Both),
            (true, false) => Some(MediaSelection::Icons),
            (false, true) => Some(MediaSelection::Screenshots),
            (false, false) => None,
        };
        if let Some(mode) = mode {
            self.pending_icon_prefetch = false;
            self.pending_screenshot_prefetch = false;
            self.start_media_collection(mode, None);
        }
    }
    fn start_decompress(&mut self, src: PathBuf) {
        if self.decompress_result.is_some() || self.workspace_cleanup_result.is_some() {
            self.status = "변환 작업이나 임시폴더 정리가 진행 중입니다".into();
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
    fn start_workspace_cleanup(&mut self) {
        if self.workspace_cleanup_result.is_some() || self.decompress_result.is_some() {
            self.status = "변환 작업이 끝난 뒤 임시폴더를 정리할 수 있습니다".into();
            return;
        }
        let root = self.base_dir.join("workspaces");
        let (tx, rx) = mpsc::channel();
        self.workspace_cleanup_result = Some(rx);
        self.status = "7일 이상 지난 변환 작업폴더 정리 중".into();
        std::thread::spawn(move || {
            let count =
                decompress::cleanup_workspaces(&root, std::time::Duration::from_secs(7 * 86400));
            let _ = tx.send(Ok(count));
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
            self.start_media_collection(MediaSelection::Screenshots, Some(row));
            ui.close_menu();
        }
        ui.separator();
        if ui.button("🖼  아이콘 스캔").clicked() {
            self.start_media_collection(MediaSelection::Icons, Some(row));
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
        title = title
            .trim()
            .trim_end_matches([' ', '.'])
            .chars()
            .take(100)
            .collect();
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
            format!(
                "{title} [{}][{}].{ext}",
                tid.to_ascii_uppercase(),
                tags.join("+")
            )
        };
        Ok(old.with_file_name(name))
    }
    /// Folder tab labels, rebuilt only when the row set or folders change.
    fn library_tabs_cached(&mut self) -> &[(String, String)] {
        if self.cached_tabs_generation == self.rows_generation && !self.cached_tabs.is_empty() {
            return &self.cached_tabs;
        }
        let base_count = self.rows.iter().filter(|row| row.has_base).count();
        let mut tabs = vec![(String::new(), format!("전체 ({base_count})"))];
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
            tabs.push((folder.clone(), format!("{name} ({count})")));
        }
        tabs.push((
            "MISSING_UPDATES".into(),
            format!("미보유 업데이트 ({})", self.cached_missing_updates),
        ));
        tabs.push((
            "MISSING_DLCS".into(),
            format!("미보유 DLC ({})", self.cached_missing_dlcs),
        ));
        tabs.push(("CATALOG".into(), "스토어".into()));
        tabs.push((
            "DOWNLOADS".into(),
            if self.torrent_progress.is_empty() {
                "다운로드".into()
            } else {
                format!("다운로드 ({})", self.torrent_progress.len())
            },
        ));
        self.cached_tabs = tabs;
        self.cached_tabs_generation = self.rows_generation;
        &self.cached_tabs
    }

    /// Missing-update / missing-DLC counts and duplicate groups, computed once
    /// per row generation instead of every frame.
    fn refresh_library_stats(&mut self) {
        let exceptions = (
            self.config
                .settings
                .get("exception_update")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_owned(),
            self.config
                .settings
                .get("exception_dlc")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_owned(),
        );
        if self.cached_stats_generation == self.rows_generation
            && self.cached_stats_exceptions == exceptions
        {
            return;
        }
        self.cached_stats_exceptions = exceptions;
        self.cached_tabs_generation = u64::MAX;
        self.cached_missing_updates = self
            .rows
            .iter()
            .filter(|row| row.has_base && self.row_is_missing_update(row))
            .count();
        self.cached_missing_dlcs = self
            .rows
            .iter()
            .filter(|row| row.has_base && self.row_has_missing_dlc(row))
            .count();
        self.cached_duplicates = self.duplicate_title_groups();
        self.cached_stats_generation = self.rows_generation;
    }

    /// Screenshot files for the selected game, resolved only when the
    /// selection changes.
    fn selected_screenshot_paths_cached(&mut self) -> Vec<PathBuf> {
        if self.cached_screenshot_selection == self.selected {
            return self.cached_screenshot_paths.clone();
        }
        self.cached_screenshot_selection = self.selected;
        let paths: Vec<PathBuf> = self
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
        self.cached_screenshot_paths = paths.clone();
        paths
    }

    /// Filtered + sorted library view, rebuilt only when an input changes.
    ///
    /// The naive version cloned and sorted all 800+ `GameFile` rows on every
    /// frame, which allocated ~16k strings per frame and made the library view
    /// stutter. This keeps the computation off the hot path.
    fn library_rows_cached(&mut self) -> Arc<Vec<library::GameFile>> {
        let key = FilterKey {
            search: self.search_committed.clone(),
            kind: self.kind_filter.clone(),
            folder: self.folder_filter.clone(),
            sort_column: self.sort_column.clone(),
            sort_desc: self.sort_desc,
            exception_update: self
                .config
                .settings
                .get("exception_update")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_owned(),
            exception_dlc: self
                .config
                .settings
                .get("exception_dlc")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_owned(),
            rows_len: self.rows.len(),
            rows_generation: self.rows_generation,
        };
        if self.filtered_key.as_ref() == Some(&key) {
            return self.filtered_rows.clone();
        }
        let mut matching = library::visible(&self.rows, &self.search_committed)
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
        self.filtered_rows = Arc::new(matching);
        self.filtered_key = Some(key);
        self.filtered_rows.clone()
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
        for row in library::visible(&self.rows, &self.search_committed) {
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
        if self.scan_result.is_some()
            || self.titledb_result.is_some()
            || self.decompress_result.is_some()
            || self.media_result.is_some()
            || self.orphan_cleanup_result.is_some()
        {
            self.status = "스캔·타이틀 DB·압축 해제 작업 중에는 파일명을 변경할 수 없습니다".into();
            return;
        }
        let db = self.db.clone();
        let (tx, rx) = mpsc::channel();
        self.bulk_rename_result = Some(rx);
        let progress = Arc::new(TaskProgress::default());
        progress.begin("파일명 일괄 정규화", Some(changes.len() as u64), false);
        self.rename_progress = Some(progress.clone());
        self.status = format!("파일명 일괄 정규화 중: {}개", changes.len());
        let log = self.log_sender.clone();
        std::thread::spawn(move || {
            let mut renamed = 0usize;
            let mut failed = 0usize;
            for (index, (from, to)) in changes.into_iter().enumerate() {
                if PathBuf::from(&to).exists() || std::fs::rename(&from, &to).is_err() {
                    failed += 1;
                    progress.set(index as u64 + 1);
                    continue;
                }
                if db.rename_file_path(&from, &to).is_ok() {
                    renamed += 1;
                } else {
                    let _ = std::fs::rename(&to, &from);
                    failed += 1;
                }
                progress.set(index as u64 + 1);
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
                Ok(info) => format!("ℹ️ 릴리스 확인 완료 · 커밋 {}", info.latest_version),
                Err(error) => format!("❌ 업데이트 확인 실패 · {error}"),
            });
            let _ = tx.send(result);
        });
    }
    fn start_update_download(&mut self, url: String, commit: String) {
        if self.update_prepare_result.is_some() {
            return;
        }
        let base_dir = self.base_dir.clone();
        let (tx, rx) = mpsc::channel();
        self.update_prepare_result = Some(rx);
        let progress = Arc::new(TaskProgress::default());
        progress.begin("업데이트 다운로드 준비", None, false);
        self.update_progress = Some(progress.clone());
        self.status = format!(
            "커밋 {} 업데이트 다운로드 및 검증 중",
            updater::short_commit(&commit)
        );
        let log = self.log_sender.clone();
        std::thread::spawn(move || {
            let result = updater::download_and_prepare_with_progress(
                &url,
                &commit,
                &base_dir,
                Some(&progress),
            )
            .map_err(|e| e.to_string());
            let _ = log.send(match &result {
                Ok(_) => format!(
                    "✅ 커밋 {} 업데이트 검증 완료",
                    updater::short_commit(&commit)
                ),
                Err(error) => format!("❌ 업데이트 준비 실패 · {error}"),
            });
            let _ = tx.send(result);
        });
    }
    fn start_orphan_cleanup(&mut self) {
        if self.orphan_cleanup_result.is_some() {
            return;
        }
        if self.media_result.is_some()
            || self.scan_result.is_some()
            || self.titledb_result.is_some()
            || self.reload_result.is_some()
            || self.bulk_rename_result.is_some()
        {
            self.status = "미디어 수집·스캔·DB 갱신이 끝난 뒤 캐시를 정리하세요".into();
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
            (
                "다운로드",
                "스토어 토렌트 다운로드의 저장 위치와 삭제 동작을 설정합니다.",
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
                        5 => {
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
                        6 => {
                            let mut delete_files = self
                                .config
                                .settings
                                .get("download_delete_files")
                                .and_then(|v| v.as_str())
                                .map(|v| v == "1")
                                .unwrap_or(false);
                            ui.heading("삭제 동작");
                            ui.add_space(4.0);
                            if ui
                                .checkbox(
                                    &mut delete_files,
                                    "다운로드 탭의 「삭제」에서 받은 파일도 함께 삭제",
                                )
                                .changed()
                            {
                                self.config.settings.insert(
                                    "download_delete_files".into(),
                                    serde_json::Value::String(
                                        if delete_files { "1" } else { "0" }.into(),
                                    ),
                                );
                            }
                            ui.label(
                                egui::RichText::new(
                                    "끔(기본): 토렌트만 제거하고 받은 파일은 그대로 둡니다.\n\
                                     씀: 받은 파일까지 완전히 삭제합니다. 되돌릴 수 없으니 주의하세요.",
                                )
                                .color(egui::Color32::GRAY),
                            );
                            ui.add_space(16.0);
                            ui.heading("저장 위치");
                            ui.add_space(4.0);
                            let save_dir = self.base_dir.join("downloads");
                            ui.label(save_dir.display().to_string());
                            if ui.button("폴더 열기").clicked() {
                                let _ = std::process::Command::new("explorer")
                                    .arg(&save_dir)
                                    .spawn();
                            }
                            ui.label(
                                egui::RichText::new(
                                    "모든 스토어 다운로드는 이 폴더 아래의 게임별 폴더에 저장됩니다.",
                                )
                                .color(egui::Color32::GRAY),
                            );
                            ui.add_space(16.0);
                            ui.heading("세션 데이터");
                            ui.add_space(4.0);
                            let session_dir = torrent::TorrentEngine::session_state_path(
                                &self.base_dir.join("downloads"),
                            );
                            ui.label(session_dir.display().to_string());
                            ui.label(
                                egui::RichText::new(
                                    "이어받기 정보와 토렌트 메타데이터가 저장되는 곳입니다.\n\
                                     지우면 진행 중인 다운로드가 처음부터 다시 받아집니다.",
                                )
                                .color(egui::Color32::GRAY),
                            );
                            ui.add_space(4.0);
                            let reset_pending = self.config.settings
                                .get("clear_torrent_session_on_next_launch")
                                .and_then(|v| v.as_str()) == Some("1");
                            if ui.button(if reset_pending {
                                "다음 시작 세션 초기화 예약 취소"
                            } else {
                                "다음 시작 시 토렌트 세션 초기화"
                            }).clicked() {
                                if reset_pending {
                                    self.config.settings.remove("clear_torrent_session_on_next_launch");
                                    self.status = "토렌트 세션 초기화 예약을 취소했습니다".into();
                                } else {
                                    self.config.settings.insert(
                                        "clear_torrent_session_on_next_launch".into(),
                                        serde_json::Value::String("1".into()),
                                    );
                                    self.status = "다음 실행 전 엔진 시작 시 세션을 안전하게 초기화합니다 (파일 보존)".into();
                                }
                            }
                            if reset_pending {
                                ui.label("다음 실행 시 세션 메타데이터만 초기화됩니다. 다운로드한 파일은 유지됩니다.");
                            }
                            if !self.torrent_progress.is_empty() {
                                ui.label(
                                    egui::RichText::new(
                                        "진행 중인 다운로드가 있어 지금은 삭제할 수 없습니다.",
                                    )
                                    .color(egui::Color32::GRAY),
                                );
                            }
                        }
                        _ => {}
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
        let panel_content_width = ui.available_width();
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                // A long title or absolute path must wrap inside the user's
                // chosen sash width, never expand the SidePanel's layout.
                ui.set_max_width(panel_content_width);
                ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Wrap);
                ui.horizontal(|ui| {
                    if let Some(icon) = self.icon_textures.get(&row.title_id) {
                        let icon_size = if panel_content_width < 365.0 {
                            96.0
                        } else {
                            120.0
                        };
                        ui.image((icon.id(), egui::vec2(icon_size, icon_size)));
                    }
                    ui.vertical(|ui| {
                        ui.set_max_width(ui.available_width());
                        ui.horizontal_wrapped(|ui| {
                            if ui.small_button("×").clicked() {
                                self.selected = None;
                            }
                            ui.label(egui::RichText::new(&row.title_name).size(14.0).strong());
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
                        ui.label(format!("보유: {}", common::version_to_str(Some(installed))));
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
                                common::version_to_str(Some(installed)),
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
    /// Display every running operation in the same footer, with a measured
    /// fraction where available. The first foreground task wins the visible
    /// slot; simultaneous tasks remain discoverable in its hover tooltip.
    fn active_progress_views(&self) -> Vec<ProgressView> {
        let mut views = Vec::new();
        if self.scan_result.is_some() {
            views.push(if let Some(progress) = &self.scan_progress {
                ProgressView::counted(
                    "게임 파일 스캔",
                    progress.completed.load(Ordering::Relaxed) as u64,
                    progress.total.load(Ordering::Relaxed) as u64,
                )
            } else {
                ProgressView::busy("게임 파일 스캔")
            });
        }
        if self.titledb_result.is_some() {
            views.push(self.titledb_progress.as_ref().map_or_else(
                || ProgressView::busy("타이틀 DB 다운로드/매칭"),
                |progress| ProgressView::from_task(progress.snapshot()),
            ));
        }
        if self.media_result.is_some() {
            views.push(if let Some(progress) = &self.media_progress {
                let (done, total, saved) = progress.snapshot();
                let mut view = ProgressView::counted(
                    &format!("미디어 수집 · {}", progress.stage_label()),
                    done as u64,
                    total as u64,
                );
                view.detail = format!(
                    "{} / {total} · 저장 {saved} · 실패 {}",
                    done.min(total),
                    progress.failures()
                );
                view
            } else {
                ProgressView::busy("미디어 수집")
            });
        }
        if self.catalog_result.is_some() {
            views.push(self.catalog_progress.as_ref().map_or_else(
                || ProgressView::busy("게임 카탈로그 다운로드"),
                |progress| ProgressView::from_task(progress.snapshot()),
            ));
        }
        if self.bulk_rename_result.is_some() {
            views.push(self.rename_progress.as_ref().map_or_else(
                || ProgressView::busy("파일명 일괄 정규화"),
                |progress| ProgressView::from_task(progress.snapshot()),
            ));
        }
        if self.update_prepare_result.is_some() {
            views.push(self.update_progress.as_ref().map_or_else(
                || ProgressView::busy("프로그램 업데이트 다운로드/검증"),
                |progress| ProgressView::from_task(progress.snapshot()),
            ));
        }
        for (active, label) in [
            (self.decompress_result.is_some(), "NSZ/XCZ 압축 해제"),
            (self.orphan_cleanup_result.is_some(), "미디어 캐시 정리"),
            (
                self.workspace_cleanup_result.is_some(),
                "오래된 변환 폴더 정리",
            ),
            (self.update_check_result.is_some(), "프로그램 업데이트 확인"),
            (self.translate_result.is_some(), "게임 설명 번역"),
            (self.catalog_cover_queue.is_some(), "스토어 커버 다운로드"),
        ] {
            if active {
                views.push(ProgressView::busy(label));
            }
        }
        let transfers: Vec<_> = self
            .torrent_progress
            .iter()
            .map(|(_, p)| p)
            .filter(|p| !p.finished && p.error.is_none() && !p.paused)
            .collect();
        if !transfers.is_empty() {
            let completed = transfers
                .iter()
                .fold(0_u64, |acc, p| acc.saturating_add(p.progress_bytes));
            let total = transfers
                .iter()
                .fold(0_u64, |acc, p| acc.saturating_add(p.total_bytes));
            let label = format!("토렌트 다운로드 · {}개", transfers.len());
            views.push(if transfers.iter().all(|p| p.total_bytes > 0) {
                let mut view = ProgressView::counted(&label, completed, total);
                let speed = transfers
                    .iter()
                    .fold(0_u64, |acc, p| acc.saturating_add(p.download_speed_bps));
                view.detail = format!(
                    "{} / {} · {}/s",
                    common::format_size(completed),
                    common::format_size(total),
                    common::format_size(speed)
                );
                view
            } else {
                ProgressView::busy(&label)
            });
        }
        if self.reload_result.is_some() {
            views.push(ProgressView::busy("게임 목록 갱신"));
        }
        views
    }

    fn torrent_poll_due(&mut self) -> bool {
        let now = std::time::Instant::now();
        if self.torrent_polled_at.is_some_and(|previous| {
            now.saturating_duration_since(previous) < std::time::Duration::from_millis(300)
        }) {
            return false;
        }
        self.torrent_polled_at = Some(now);
        true
    }

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
        while let Ok(line) = self.log_receiver.try_recv() {
            self.log_lines.push(line);
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
            if !self.settings_open && self.folder_filter == "CATALOG" {
                ctx.memory_mut(|memory| memory.request_focus(egui::Id::new("catalog_search")));
            } else if !self.settings_open && self.folder_filter == "DOWNLOADS" {
                ctx.memory_mut(|memory| memory.request_focus(egui::Id::new("download_search")));
            } else {
                self.search_focus = true;
            }
        }
        if ctrl_o {
            self.settings_open = !self.settings_open;
        }
        if f5 && self.scan_result.is_none() {
            self.start_scan(true);
        }
        if escape {
            // TextEdit changes its buffer during this frame, so clearing only
            // `search` can leave the debounced/committed query filtering the
            // list indefinitely until the user types again.
            self.search.clear();
            self.search_committed.clear();
            self.search_pending = None;
            self.search_focus = false;
            self.current_page = 0;
            self.filtered_key = None;
            if self.folder_filter == "CATALOG" {
                self.catalog_search.clear();
                self.catalog_search_committed.clear();
                self.catalog_search_pending = None;
                self.catalog_search_cache = None;
                self.catalog_page = 0;
                self.catalog_scroll_reset = true;
            } else if self.folder_filter == "DOWNLOADS" {
                self.download_search.clear();
                self.download_scroll_reset = true;
            }
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
            (self.media_result.is_some(), "미디어 수집"),
            (self.decompress_result.is_some(), "NSZ/XCZ 압축 해제"),
            (self.bulk_rename_result.is_some(), "파일명 일괄 정규화"),
            (self.update_check_result.is_some(), "업데이트 확인"),
            (self.update_prepare_result.is_some(), "업데이트 다운로드"),
            (self.orphan_cleanup_result.is_some(), "미디어 캐시 정리"),
            (
                self.workspace_cleanup_result.is_some(),
                "오래된 변환 폴더 정리",
            ),
            (self.translate_result.is_some(), "설명 번역"),
            (self.catalog_result.is_some(), "카탈로그 다운로드"),
            (
                self.torrent_progress
                    .iter()
                    .any(|(_, p)| !p.finished && p.error.is_none() && !p.paused),
                "토렌트 다운로드",
            ),
        ]
        .into_iter()
        .filter_map(|(active, label)| active.then_some(label))
        .collect::<Vec<_>>();
        // Background scans/downloads must not steal focus by opening the log
        // window. The toolbar has a dedicated log toggle for users who want it.
        if self
            .torrent_progress
            .iter()
            .any(|(_, p)| !p.finished && p.error.is_none() && !p.paused)
        {
            ctx.request_repaint_after(std::time::Duration::from_millis(500));
        }
        let close_requested = ctx.input(|input| input.viewport().close_requested());
        if close_requested && !self.exit_confirmed && !active_tasks.is_empty() {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.exit_confirm = true;
        }
        let scan_done = poll_background(&self.scan_result, "scan");
        if let Some(done) = scan_done {
            self.scan_result = None;
            self.scan_cancel = None;
            self.scan_progress = None;
            match done {
                Ok((n, removed, cancelled, scan_errors)) => {
                    self.last_scan_errors = scan_errors;
                    self.status = if cancelled {
                        format!("스캔 중지됨 · {n}개 파일 반영 · 기존 파일 기록은 유지")
                    } else {
                        format!("{n}개 파일 검색 완료 · {removed}개 누락 기록 제거")
                    };
                    // Remember the scan schema that produced these records so the
                    // forced re-analysis happens only once.
                    if !cancelled && scan_errors == 0 {
                        self.config.settings.insert(
                            "scan_schema".into(),
                            serde_json::Value::String(SCAN_SCHEMA_VERSION.into()),
                        );
                        let _ =
                            database::save_config(&self.base_dir.join("config.json"), &self.config);
                    }
                    self.pending_icon_prefetch = !cancelled
                        && scan_errors == 0
                        && !self.headless_mode
                        && self
                            .config
                            .settings
                            .get("scan_icon")
                            .and_then(|v| v.as_str())
                            .unwrap_or("1")
                            == "1";
                    self.pending_screenshot_prefetch = !cancelled
                        && scan_errors == 0
                        && !self.headless_mode
                        && self
                            .config
                            .settings
                            .get("scan_screenshot")
                            .and_then(|v| v.as_str())
                            .unwrap_or("1")
                            == "1";
                    if scan_errors > 0 {
                        self.status =
                            format!("스캔 완료, 읽기/분석 오류 {scan_errors}개 (기존 기록 보존)");
                    }
                    self.start_reload();
                }
                Err(e) => self.status = e,
            }
        } else if self.scan_result.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
        let titledb_done = poll_background(&self.titledb_result, "titledb");
        if let Some(done) = titledb_done {
            self.titledb_result = None;
            self.titledb_progress = None;
            match done {
                Ok(titledb::UpdateOutcome::AlreadyLatest) => {
                    self.status = "타이틀 DB는 이미 최신입니다".into();
                }
                Ok(titledb::UpdateOutcome::Updated(n)) => {
                    self.status = format!("타이틀 DB {n}개 항목 다운로드 및 반영 완료");
                    self.start_reload();
                }
                Err(e) => self.status = e,
            }
        } else if self.titledb_result.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
        let reload_done = poll_background(&self.reload_result, "reload");
        if let Some(done) = reload_done {
            self.reload_result = None;
            match done {
                Ok(rows) => {
                    self.rows = rows;
                    self.cached_screenshot_selection = None;
                    self.rows_generation = self.rows_generation.wrapping_add(1);
                    match self.db.stats() {
                        Ok(stats) => self.stats = stats,
                        Err(error) => self.status = format!("DB 통계를 읽지 못했습니다: {error}"),
                    }
                    if self.view_mode == "grid" && self.selected.is_none() {
                        self.selected = library::visible(&self.rows, &self.search_committed)
                            .filter(|row| self.row_matches_current_filter(row))
                            .min_by(|a, b| {
                                a.title_name
                                    .to_lowercase()
                                    .cmp(&b.title_name.to_lowercase())
                            })
                            .map(|row| row.id);
                    }
                }
                Err(e) => {
                    self.status = format!("목록 갱신 실패: {e}");
                    self.pending_icon_prefetch = false;
                    self.pending_screenshot_prefetch = false;
                }
            }
            if self.reload_dirty {
                self.start_reload();
            }
        } else if self.reload_result.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
        let bulk_rename_done = poll_background(&self.bulk_rename_result, "bulk rename");
        if let Some(done) = bulk_rename_done {
            self.bulk_rename_result = None;
            self.rename_progress = None;
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
        let catalog_loaded = match self.catalog_load_result.as_ref().map(|rx| rx.try_recv()) {
            Some(Ok(result)) => {
                self.catalog_load_result = None;
                result
            }
            Some(Err(mpsc::TryRecvError::Disconnected)) => {
                self.catalog_load_result = None;
                None
            }
            _ => None,
        };
        if let Some(state) = catalog_loaded {
            if self.catalog_state.items.is_empty() && self.catalog_result.is_none() {
                self.catalog_state = state;
                self.catalog_search_cache = None;
                self.catalog_page = 0;
            }
        } else if self.catalog_load_result.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
        let catalog_done = poll_background(&self.catalog_result, "catalog");
        if let Some(done) = catalog_done {
            self.catalog_result = None;
            self.catalog_progress = None;
            match done {
                Ok(count) => {
                    self.status = format!("카탈로그 {count}개 항목 준비 완료");
                    if let Some(state) = catalog::load(&self.base_dir) {
                        self.catalog_state = state;
                        self.catalog_page = 0;
                        self.catalog_search_cache = None;
                        self.catalog_matching_indices.clear();
                        self.catalog_cover_retry_after.clear();
                        self.catalog_cover_missing.clear();
                    }
                }
                Err(e) => self.status = e,
            }
        } else if self.catalog_result.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
        let cover_done = match self.catalog_cover_queue.as_ref().map(|rx| rx.try_recv()) {
            Some(Ok(done)) => Some(done),
            Some(Err(mpsc::TryRecvError::Disconnected)) => {
                self.catalog_cover_queue = None;
                self.status = "스토어 커버 수집 작업이 비정상 종료되었습니다".into();
                None
            }
            _ => None,
        };
        if let Some(done) = cover_done {
            self.catalog_cover_queue = None;
            let (fetched, failed) = done;
            let retry_at = std::time::Instant::now() + STORE_COVER_RETRY_DELAY;
            for topic_id in failed {
                self.catalog_cover_retry_after.insert(topic_id, retry_at);
            }
            if fetched > 0 {
                self.catalog_cover_missing.clear();
                self.status = format!("스토어 커버 {fetched}개 받음");
                ctx.request_repaint();
            }
        } else if self.catalog_cover_queue.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(250));
        }
        if self.torrent_engine.is_some() && self.torrent_poll_due() {
            if let Some(engine) = &self.torrent_engine {
                let was_active = self
                    .torrent_progress
                    .iter()
                    .any(|(_, p)| !p.finished && p.error.is_none() && !p.paused);
                let (snapshot, completions) = engine.poll();
                let now_active = snapshot
                    .iter()
                    .any(|(_, p)| !p.finished && p.error.is_none() && !p.paused);
                if was_active && !now_active {
                    self.start_reload();
                }
                for notice in completions {
                    let (key, result) = match notice {
                        torrent::EngineNotice::Download { key, result } => (key, result),
                        torrent::EngineNotice::Command {
                            key,
                            action,
                            result,
                        } => {
                            if let Err(error) = result {
                                self.status = format!("다운로드 {action} 명령 실패: {error}");
                                let _ = self
                                    .log_sender
                                    .send(format!("다운로드 명령 실패 ({key}): {error}"));
                            }
                            continue;
                        }
                    };
                    let label = snapshot
                        .iter()
                        .find(|(k, _)| *k == key)
                        .map(|(_, p)| p.name.clone())
                        .unwrap_or_else(|| key.clone());
                    match result {
                        Ok(()) => {
                            let _ = self
                                .log_sender
                                .send(format!("✅ 다운로드 완료 · {}", truncate_label(&label, 60)));
                            self.status = format!("다운로드 완료: {}", truncate_label(&label, 60));
                        }
                        Err(error) => {
                            let _ = self.log_sender.send(format!(
                                "❌ 다운로드 실패 · {} · {error}",
                                truncate_label(&label, 60)
                            ));
                        }
                    }
                }
                if self.torrent_progress.len() != snapshot.len() {
                    self.cached_tabs_generation = u64::MAX;
                }
                self.torrent_progress = snapshot;
            }
        }
        let update_check_done = poll_background(&self.update_check_result, "update check");
        if let Some(done) = update_check_done {
            self.update_check_result = None;
            match done {
                Ok(info) if info.is_newer && info.download_url.is_some() => {
                    self.update_info = Some(info);
                }
                Ok(info) if info.is_newer => {
                    self.status = format!(
                        "커밋 {}가 있지만 호환되는 ZIP이 릴리스에 없습니다",
                        info.latest_version
                    );
                }
                Ok(info) => {
                    self.status = format!("현재 커밋 {}가 최신입니다", info.current_version);
                }
                Err(e) => self.status = format!("업데이트 확인 실패: {e}"),
            }
        } else if self.update_check_result.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
        let update_prepare_done = poll_background(&self.update_prepare_result, "update prepare");
        if let Some(done) = update_prepare_done {
            self.update_prepare_result = None;
            self.update_progress = None;
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
        let orphan_cleanup_done = poll_background(&self.orphan_cleanup_result, "orphan cleanup");
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
        let workspace_cleanup_done =
            poll_background(&self.workspace_cleanup_result, "workspace cleanup");
        if let Some(done) = workspace_cleanup_done {
            self.workspace_cleanup_result = None;
            match done {
                Ok(count) => {
                    self.status = format!("오래된 변환 폴더 {count}개 정리 완료");
                    let _ = self.log_sender.send(self.status.clone());
                }
                Err(error) => self.status = format!("변환 폴더 정리 실패: {error}"),
            }
        } else if self.workspace_cleanup_result.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
        self.start_pending_media();
        let media_done = poll_background(&self.media_result, "media");
        if let Some(done) = media_done {
            self.media_result = None;
            self.media_progress = None;
            match done {
                Ok(outcome) => {
                    self.status = if outcome.cancelled {
                        format!("미디어 수집 중지됨 · {}개 저장", outcome.saved_total())
                    } else if outcome.failed > 0 {
                        format!(
                            "미디어 수집 종료 · {}개 저장 · {}개 실패 (로그 확인)",
                            outcome.saved_total(),
                            outcome.failed
                        )
                    } else {
                        format!(
                            "미디어 수집 완료 · 아이콘 {}개 · 스크린샷 {}장 · 캐시 {}개 재사용",
                            outcome.icons, outcome.screenshots, outcome.skipped
                        )
                    };
                    self.invalidate_icons();
                    self.screenshot_textures.clear();
                    self.screenshot_missing.clear();
                    self.cached_screenshot_selection = None;
                    self.start_pending_media();
                }
                Err(e) => self.status = e,
            }
        } else if self.media_result.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
        let decompress_done = poll_background(&self.decompress_result, "decompress");
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
        let translate_done = match self.translate_result.as_ref().map(|rx| rx.try_recv()) {
            Some(Ok(done)) => Some(done),
            Some(Err(mpsc::TryRecvError::Disconnected)) => {
                self.translate_result = None;
                self.status = "설명 번역 작업이 비정상 종료되었습니다".into();
                None
            }
            _ => None,
        };
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
            self.image_loader.cancel_stale_screenshots(self.selected);
            self.screenshot_textures.clear();
            self.screenshot_missing.clear();
            self.texture_selection = self.selected;
        }
        self.poll_loaded_images(ctx);
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
        self.refresh_library_stats();
        let selected_screenshot_paths = self.selected_screenshot_paths_cached();
        // Only decode screenshots when that detail tab is visible. Selecting
        // a row on the text tabs must not synchronously load up to six large
        // images or allocate their full-resolution intermediate buffers.
        if self.detail_tab == 2 && self.selected.is_some() {
            for path in &selected_screenshot_paths {
                let key = path.to_string_lossy().into_owned();
                if !self.screenshot_textures.contains_key(&key)
                    && !self.screenshot_missing.contains(&key)
                {
                    self.image_loader.request(
                        ImageKind::Screenshot,
                        &key,
                        path.clone(),
                        900,
                        self.selected,
                    );
                }
            }
            if self.image_loader.has_pending() {
                ctx.request_repaint_after(std::time::Duration::from_millis(45));
            }
        }
        let search_before = self.search.clone();
        let filter_before = self.kind_filter.clone();
        let folder_filter_before = self.folder_filter.clone();
        egui::TopBottomPanel::top("top").show(ctx, |ui| {
            top_bar(self, ui, ctx, &active_tasks);
        });
        if self.search != search_before {
            self.search_pending =
                Some(std::time::Instant::now() + std::time::Duration::from_millis(500));
        }
        if let Some(deadline) = self.search_pending {
            if std::time::Instant::now() >= deadline {
                self.search_pending = None;
                if self.search_committed != self.search {
                    self.search_committed = self.search.clone();
                }
            }
        }
        if self.search_committed != search_before || self.kind_filter != filter_before {
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
        if self.search_committed != search_before
            || self.kind_filter != filter_before
            || self.folder_filter != folder_filter_before
        {
            self.selected = None;
        }
        if self.search_pending.is_some() || self.catalog_search_pending.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(50));
        }
        // Pin the status bar FIRST, so the optional log panel is always ABOVE it.
        // Match the Python version: status | 180px progress | RAM/CPU.
        let activities = self.active_progress_views();
        let primary = activities.first();
        let status_message = if let Some(task) = primary {
            if activities.len() > 1 {
                format!(
                    "{} · {} · 외 {}개 작업",
                    task.label,
                    task.detail,
                    activities.len() - 1
                )
            } else {
                format!("{} · {}", task.label, task.detail)
            }
        } else {
            self.status.clone()
        };
        let progress_help = if activities.is_empty() {
            "진행 중인 작업 없음".to_owned()
        } else {
            activities
                .iter()
                .map(|a| format!("{} · {}", a.label, a.detail))
                .collect::<Vec<_>>()
                .join("\n")
        };
        let status_panel = egui::TopBottomPanel::bottom("status")
            .exact_height(31.0)
            .frame(
                egui::Frame::none()
                    .fill(ctx.style().visuals.widgets.inactive.bg_fill)
                    .inner_margin(egui::Margin::symmetric(9.0, 4.0)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 8.0;
                    let width = ui.available_width();
                    let show_resource = width >= 580.0;
                    let resource_text = self.resource_usage_text();
                    let resource_width = if show_resource {
                        ui.painter()
                            .layout_no_wrap(
                                resource_text.clone(),
                                egui::TextStyle::Monospace.resolve(ui.style()),
                                ui.visuals().weak_text_color(),
                            )
                            .size()
                            .x
                            + 6.0
                    } else {
                        0.0
                    };
                    let bar_width = if width >= 360.0 { 180.0 } else { 100.0 };
                    let label_width = (width - resource_width - bar_width - 26.0).max(5.0);
                    // Keep the text anchored at the *left edge* of its slot.
                    // egui::Label inside add_sized can center the galley in
                    // this horizontal layout, even with Label::halign(Min).
                    let (message_rect, message_response) =
                        ui.allocate_exact_size(egui::vec2(label_width, 19.0), egui::Sense::hover());
                    ui.painter().with_clip_rect(message_rect).text(
                        message_rect.left_center(),
                        egui::Align2::LEFT_CENTER,
                        &status_message,
                        egui::FontId::proportional(12.0),
                        ui.visuals().text_color(),
                    );
                    message_response.on_hover_text(&status_message);
                    let progress_response =
                        if primary.is_some_and(|task| task.fraction.is_none()) {
                            // An animated segment, not a fabricated 0-100% value,
                            // for network waits, parsing, unpacking, and other work
                            // without a measurable total.
                            let (rect, response) = ui.allocate_exact_size(
                                egui::vec2(bar_width, 15.0),
                                egui::Sense::hover(),
                            );
                            let painter = ui.painter();
                            painter.rect_filled(rect, 7.5, ui.visuals().extreme_bg_color);
                            let segment_width = (bar_width * 0.30).max(26.0);
                            let cycle = (ctx.input(|i| i.time) as f32 * 0.65).fract();
                            let x =
                                rect.left() + (bar_width + segment_width) * cycle - segment_width;
                            let segment = egui::Rect::from_min_size(
                                egui::pos2(x, rect.top()),
                                egui::vec2(segment_width, rect.height()),
                            )
                            .intersect(rect);
                            if segment.is_positive() {
                                painter.rect_filled(segment, 6.0, ui.visuals().hyperlink_color);
                            }
                            painter.text(
                                rect.center(),
                                egui::Align2::CENTER_CENTER,
                                "작업 중",
                                egui::TextStyle::Small.resolve(ui.style()),
                                ui.visuals().text_color(),
                            );
                            ctx.request_repaint_after(std::time::Duration::from_millis(80));
                            response
                        } else if let Some(task) = primary {
                            ui.add(
                                egui::ProgressBar::new(task.fraction.unwrap_or(0.0))
                                    .desired_width(bar_width)
                                    .desired_height(15.0)
                                    .show_percentage(),
                            )
                        } else {
                            // Idle: keep the Python-style empty progress rail
                            // visible without egui's misleading 0%-fill dot.
                            let (rect, response) = ui.allocate_exact_size(
                                egui::vec2(bar_width, 15.0),
                                egui::Sense::hover(),
                            );
                            ui.painter()
                                .rect_filled(rect, 7.5, ui.visuals().extreme_bg_color);
                            response
                        }
                        .on_hover_text(&progress_help);
                    #[cfg(test)]
                    ctx.data_mut(|data| {
                        data.insert_temp(
                            egui::Id::new("nsl_test_status_progress_rect"),
                            progress_response.rect,
                        );
                    });
                    #[cfg(not(test))]
                    let _ = progress_response;
                    if show_resource {
                        ui.separator();
                        ui.label(
                            egui::RichText::new(resource_text)
                                .monospace()
                                .small()
                                .color(ui.visuals().weak_text_color()),
                        );
                    }
                });
            });
        #[cfg(test)]
        ctx.data_mut(|data| {
            data.insert_temp(
                egui::Id::new("nsl_test_status_rect"),
                status_panel.response.rect,
            );
        });
        #[cfg(not(test))]
        let _ = status_panel;

        let mut close_log = false;
        if self.log_open {
            let log_height = (ctx.available_rect().height() * 0.30).clamp(68.0, 174.0);
            let log_panel = egui::TopBottomPanel::bottom("operation_log_panel")
                .exact_height(log_height)
                .frame(
                    egui::Frame::none()
                        .fill(ctx.style().visuals.panel_fill)
                        .stroke(ctx.style().visuals.widgets.noninteractive.bg_stroke)
                        .inner_margin(egui::Margin::symmetric(10.0, 5.0)),
                )
                .show(ctx, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new(format!(
                                "작업 로그  ·  {}개 메시지",
                                self.log_lines.len()
                            ))
                            .strong()
                            .size(12.0),
                        );
                        if ui.small_button("로그 지우기").clicked() {
                            self.log_lines.clear();
                        }
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            close_log = ui.small_button("×").on_hover_text("로그 닫기").clicked();
                        });
                    });
                    ui.separator();
                    let log_row_height = ui.text_style_height(&egui::TextStyle::Monospace)
                        + ui.spacing().item_spacing.y;
                    // ScrollArea::show_rows can request the *full* initial
                    // panel height despite a header above it. Reserve header,
                    // dividers and margins explicitly, or the frame can grow
                    // over the fixed status bar during window resizing.
                    let scroll_height = (log_height - 55.0).max(10.0);
                    let scroll_output = egui::ScrollArea::vertical()
                        .id_salt("operation_log")
                        .stick_to_bottom(true)
                        .auto_shrink([false, false])
                        .min_scrolled_height(0.0)
                        .max_height(scroll_height)
                        .show_rows(ui, log_row_height, self.log_lines.len(), |ui, row_range| {
                            // Virtualized rows: thousands of messages do not cause
                            // full log layout on every repaint.
                            for line in &self.log_lines[row_range] {
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
                                ui.add(
                                    egui::Label::new(
                                        egui::RichText::new(line).monospace().color(color),
                                    )
                                    .truncate(),
                                )
                                .on_hover_text(line);
                            }
                        });
                    #[cfg(test)]
                    ctx.data_mut(|data| {
                        data.insert_temp(
                            egui::Id::new("nsl_test_log_scroll_rect"),
                            scroll_output.inner_rect,
                        );
                    });
                    #[cfg(not(test))]
                    let _ = scroll_output;
                });
            #[cfg(test)]
            ctx.data_mut(|data| {
                data.insert_temp(egui::Id::new("nsl_test_log_rect"), log_panel.response.rect);
            });
            #[cfg(not(test))]
            let _ = log_panel;
        } else {
            #[cfg(test)]
            ctx.data_mut(|data| {
                data.insert_temp(egui::Id::new("nsl_test_log_rect"), egui::Rect::NOTHING);
            });
        }
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
        // Management views get their own right-hand controls; keep the
        // library detail panel and its saved sash width completely separate.
        if !self.settings_open
            && self.folder_filter != "CATALOG"
            && self.folder_filter != "DOWNLOADS"
        {
            if let Some(row) = self
                .selected
                .and_then(|id| self.rows.iter().find(|row| row.id == id))
                .cloned()
            {
                let panel_id = egui::Id::new("detail");
                let resize_id = panel_id.with("__resize");
                // PanelState is egui's layout cache, not the user's preferred
                // sash width. A temporarily smaller window can clamp that
                // cache, so restore the saved preference once there is room.
                // Do not fight a real in-progress splitter drag.
                let dragging_before_layout = ctx
                    .read_response(resize_id)
                    .is_some_and(|response| response.dragged());
                if remember_sash && !dragging_before_layout {
                    if let Some(mut state) =
                        egui::containers::panel::PanelState::load(ctx, panel_id)
                    {
                        if (state.size().x - saved_detail_width).abs() > 1.0 {
                            state.rect.min.x = state.rect.max.x - saved_detail_width;
                            ctx.data_mut(|data| data.insert_persisted(panel_id, state));
                        }
                    }
                }
                let detail_panel = egui::SidePanel::right("detail")
                    .default_width(saved_detail_width)
                    .min_width(320.0)
                    .max_width(900.0)
                    .resizable(true)
                    .show(ctx, |ui| {
                        self.selected_detail_ui(ui, &row, &selected_screenshot_paths);
                    });
                // egui can change PanelState when child widgets need more
                // space or the window becomes narrower. Only a *real splitter
                // drag* should overwrite the user's saved width; otherwise a
                // long game title or a temporary window resize is persisted.
                let user_resizing = ctx
                    .read_response(resize_id)
                    .is_some_and(|response| response.dragged());
                if remember_sash && user_resizing {
                    let width = egui::containers::panel::PanelState::load(ctx, panel_id)
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
        if !self.settings_open && self.folder_filter == "CATALOG" {
            let sidebar = egui::SidePanel::right("catalog_controls")
                .default_width(282.0)
                .min_width(235.0)
                .max_width(365.0)
                .resizable(true)
                .show(ctx, |ui| {
                    ui.scope(|ui| {
                        outlined_action_style(ui);
                        self.catalog_sidebar_ui(ui, ctx);
                    });
                });
            #[cfg(test)]
            ctx.data_mut(|data| {
                data.insert_temp(
                    egui::Id::new("nsl_test_store_sidebar_rect"),
                    sidebar.response.rect,
                );
            });
            #[cfg(not(test))]
            let _ = sidebar;
        } else if !self.settings_open && self.folder_filter == "DOWNLOADS" {
            let sidebar = egui::SidePanel::right("download_controls")
                .default_width(282.0)
                .min_width(235.0)
                .max_width(365.0)
                .resizable(true)
                .show(ctx, |ui| {
                    ui.scope(|ui| {
                        outlined_action_style(ui);
                        self.downloads_sidebar_ui(ui);
                    });
                });
            #[cfg(test)]
            ctx.data_mut(|data| {
                data.insert_temp(
                    egui::Id::new("nsl_test_download_sidebar_rect"),
                    sidebar.response.rect,
                );
            });
            #[cfg(not(test))]
            let _ = sidebar;
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
                let folder_tabs = self.library_tabs_cached().to_vec();
                let tab_font = egui::TextStyle::Button.resolve(ui.style());
                let tab_color = ui.visuals().text_color();
                let preferred_widths: Vec<f32> = folder_tabs
                    .iter()
                    .map(|(_, label)| {
                        ui.painter()
                            .layout_no_wrap(label.clone(), tab_font.clone(), tab_color)
                            .size()
                            .x
                            + 28.0
                    })
                    .collect();
                let tab_count = folder_tabs.len();
                let available_tab_width = ui.available_width();
                let tab_widths = fit_directory_tab_widths(
                    &preferred_widths,
                    (available_tab_width - (tab_count.saturating_sub(1) as f32) - 2.0)
                        .max(0.0),
                );
                #[cfg(test)]
                let mut rendered_tab_rects = Vec::with_capacity(tab_count);
                egui::ScrollArea::horizontal()
                    .id_salt("directory_tabs")
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 1.0;
                            for (index, (key, label)) in folder_tabs.into_iter().enumerate() {
                                let selected = self.folder_filter == key;
                                let response = draw_tab_button(
                                    ui,
                                    &label,
                                    selected,
                                    tab_widths[index],
                                    index,
                                    tab_count,
                                );
                                #[cfg(test)]
                                rendered_tab_rects.push(response.rect);
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
                #[cfg(test)]
                ctx.data_mut(|data| {
                    data.insert_temp(
                        egui::Id::new("nsl_test_directory_tab_layout"),
                        (available_tab_width, tab_widths.clone(), rendered_tab_rects),
                    );
                });
                ui.separator();
                let library_focus_id = ui.make_persistent_id("library_keyboard_navigation");
                let library_focus_rect = ui.available_rect_before_wrap();
                let _library_focus_anchor = ui.interact(
                    library_focus_rect,
                    library_focus_id,
                    egui::Sense::focusable_noninteractive(),
                );
                if self.folder_filter == "CATALOG" {
                    ui.scope(|ui| {
                        outlined_action_style(ui);
                        self.catalog_panel_ui(ui, ctx);
                    });
                    return;
                }
                if self.folder_filter == "DOWNLOADS" {
                    ui.scope(|ui| {
                        outlined_action_style(ui);
                        self.downloads_panel_ui(ui);
                    });
                    return;
                }
                let (grid_columns, grid_card_width, grid_thumbnail_size) =
                    library_grid_dimensions(ui.available_width());
                if self.rows.is_empty() {
                    // Full-screen centered welcome, matching the Python build
                    // instead of a small inline box above an empty list.
                    let full = ui.available_rect_before_wrap();
                    ui.allocate_new_ui(egui::UiBuilder::new().max_rect(full), |ui| {
                        ui.vertical_centered(|ui| {
                            ui.add_space((full.height() * 0.28).max(40.0));
                            ui.heading(
                                egui::RichText::new("Nintendo Switch Library")
                                    .size(28.0)
                                    .strong(),
                            );
                            ui.add_space(10.0);
                            ui.label(
                                egui::RichText::new("라이브러리가 비어 있습니다")
                                    .size(16.0),
                            );
                            ui.add_space(24.0);
                            ui.label(
                                "1. prod.keys를 프로그램 폴더 또는 %USERPROFILE%\\.switch에 둡니다.",
                            );
                            ui.label("2. 타이틀 DB를 다운로드하거나 로컬 DB를 매칭합니다.");
                            ui.label("3. 설정에서 스캔 폴더를 추가한 뒤 전체 스캔을 실행합니다.");
                            ui.add_space(16.0);
                            let has_keys = scanner::has_prod_keys();
                            if has_keys {
                                ui.colored_label(
                                    egui::Color32::from_rgb(46, 160, 67),
                                    "prod.keys 발견됨",
                                );
                            } else {
                                ui.colored_label(
                                    egui::Color32::from_rgb(207, 34, 46),
                                    "prod.keys 없음",
                                );
                            }
                            ui.add_space(20.0);
                            ui.horizontal(|ui| {
                                let total = ui.available_width();
                                ui.add_space((total * 0.5 - 140.0).max(0.0));
                                if ui.button("설정 열기").clicked() {
                                    self.settings_open = true;
                                }
                                if ui.button("타이틀 DB 다운로드").clicked() {
                                    self.start_titledb_download();
                                }
                                if ui.button("빠른 스캔").clicked() {
                                    self.start_scan(true);
                                }
                            });
                        });
                    });
                    return;
                }
                let matching = self.library_rows_cached();
                let paging_on = self
                    .config
                    .settings
                    .get("paging_on")
                    .and_then(|v| v.as_str())
                    .unwrap_or("1")
                    == "1";
                let page_size = self
                    .config
                    .settings
                    .get("page_size")
                    .and_then(|v| v.as_str())
                    .and_then(|v| v.parse::<usize>().ok())
                    .filter(|v| *v > 0)
                    .unwrap_or(200)
                    .min(99_999);
                let page_count = matching.len().div_ceil(page_size).max(1);
                self.current_page = self.current_page.min(page_count - 1);
                let previous_page = self.current_page;
                let (up, down, left, right, page_up, page_down, home, end, enter_key) = ctx.input(|input| {
                    // Leave control-modified arrows and text-entry keys alone,
                    // but also accept the familiar Ctrl+Home / Ctrl+End jumps.
                    let plain = !input.modifiers.ctrl && !input.modifiers.alt && !input.modifiers.shift;
                    let jump = !input.modifiers.alt && !input.modifiers.shift;
                    (
                        plain && input.key_pressed(egui::Key::ArrowUp),
                        plain && input.key_pressed(egui::Key::ArrowDown),
                        plain && input.key_pressed(egui::Key::ArrowLeft),
                        plain && input.key_pressed(egui::Key::ArrowRight),
                        plain && input.key_pressed(egui::Key::PageUp),
                        plain && input.key_pressed(egui::Key::PageDown),
                        jump && input.key_pressed(egui::Key::Home),
                        jump && input.key_pressed(egui::Key::End),
                        plain && input.key_pressed(egui::Key::Enter),
                    )
                });
                let grid_navigation = self.view_mode == "grid";
                // Page Up/Down move by approximately one *visible viewport*,
                // not by an entire 200-item DB page. Keep the stride aligned
                // to grid rows so vertical navigation preserves its column.
                let viewport_height = (ui.available_height() - if paging_on { 34.0 } else { 0.0 })
                    .max(1.0);
                let grid_row_stride = grid_thumbnail_size + 38.0 + 12.0 + ui.spacing().item_spacing.y;
                let visible_grid_rows = (viewport_height / grid_row_stride).floor().max(1.0) as usize;
                let page_jump = if grid_navigation {
                    visible_grid_rows.saturating_mul(grid_columns)
                } else {
                    ((viewport_height - 28.0).max(24.0) / 24.0).floor().max(1.0) as usize
                };
                let navigation_delta = if page_up {
                    Some(-(page_jump as isize))
                } else if page_down {
                    Some(page_jump as isize)
                } else if grid_navigation {
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
                let mut keyboard_scroll_to_selection = self
                    .keyboard_reveal_next_frame.take()
                    .is_some_and(|id| self.selected == Some(id));
                if keyboard_navigation_enabled && !matching.is_empty() {
                    let current_index = matching
                        .iter()
                        .position(|row| self.selected == Some(row.id));
                    let visible_start = if paging_on { self.current_page * page_size } else { 0 };
                    let visible_end = if paging_on {
                        visible_start.saturating_add(page_size).min(matching.len())
                    } else {
                        matching.len()
                    };
                    let target_index = library_navigation_target(
                        current_index,
                        matching.len(),
                        visible_start,
                        visible_end,
                        navigation_delta,
                        home,
                        end,
                    );
                    if let Some(target_index) = target_index {
                        let target = &matching[target_index];
                        keyboard_scroll_to_selection = true;
                        self.selected = Some(target.id);
                        self.keyboard_reveal_next_frame = Some(target.id);
                        ctx.request_repaint();
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
                let library_page_changed = self.current_page != previous_page;
                // `matching` is already filtered+sorted by the cache. Only the
                // visible page is cloned into owned rows for rendering.
                let page_start = if paging_on {
                    self.current_page * page_size
                } else {
                    0
                };
                let page_end = (page_start + if paging_on { page_size } else { matching.len() })
                    .min(matching.len());
                let page_items: &[library::GameFile] = &matching[page_start..page_end];
                if self.view_mode == "grid" {
                    // Virtualized grid: only rows inside the viewport are built.
                    // Building every card (800+ icons) each frame was the main
                    // cause of the library stutter.
                    let row_height = grid_thumbnail_size + 38.0 + 12.0;
                    let total_rows = page_items.len().div_ceil(grid_columns.max(1));
                    let mut grid_scroll = egui::ScrollArea::vertical()
                        .id_salt("library_grid_rows")
                        .animated(false)
                        .auto_shrink([false, false]);
                    if keyboard_scroll_to_selection {
                        if let Some(position) = page_items.iter().position(|row| self.selected == Some(row.id)) {
                            let id = ui.make_persistent_id(egui::Id::new("library_grid_rows"));
                            let old_offset = egui::scroll_area::State::load(ctx, id)
                                .map(|state| state.offset.y).unwrap_or(0.0);
                            let row_stride = row_height + ui.spacing().item_spacing.y;
                            if let Some(new_offset) = scroll_offset_for_selected_row(
                                old_offset, ui.available_height(),
                                position / grid_columns, row_stride, row_height,
                            ) {
                                grid_scroll = grid_scroll.vertical_scroll_offset(new_offset);
                            }
                        }
                    } else if library_page_changed {
                        grid_scroll = grid_scroll.vertical_scroll_offset(0.0);
                    }
                    let _grid_output = grid_scroll.show_rows(ui, row_height, total_rows, |ui, row_range| {
                            #[cfg(test)]
                            ctx.data_mut(|data| {
                                data.insert_temp(
                                    egui::Id::new("nsl_test_visible_grid_rows"), row_range.clone(),
                                );
                                data.insert_temp(egui::Id::new("nsl_test_grid_columns"), grid_columns);
                            });
                            egui::Frame::none()
                                .fill(ui.visuals().faint_bg_color)
                                .inner_margin(egui::Margin::ZERO)
                                .show(ui, |ui| {
                                    let mut icon_budget = ICON_LOAD_BUDGET;
                                    for row_index in row_range {
                                        let start = row_index * grid_columns;
                                        let end = (start + grid_columns).min(page_items.len());
                                        ui.horizontal(|ui| {
                                            ui.spacing_mut().item_spacing.x = 0.0;
                                            for row in page_items.iter().skip(start).take(end - start) {
                                                self.ensure_icon_texture(
                                                    ctx,
                                                    &row.title_id,
                                                    &mut icon_budget,
                                                );
                                                let selected = self.selected == Some(row.id);
                                                let _card_response = egui::Frame::none()
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
                                                #[cfg(test)]
                                                if selected {
                                                    ctx.data_mut(|data| data.insert_temp(
                                                        egui::Id::new("nsl_test_selected_grid_rect"),
                                                        _card_response.response.rect,
                                                    ));
                                                }
                                            }
                                        });
                                    }
                                });
                        });
                    #[cfg(test)]
                    ctx.data_mut(|data| data.insert_temp(
                        egui::Id::new("nsl_test_grid_viewport_rect"), _grid_output.inner_rect,
                    ));
                } else {
                    let list_width = ui.available_width();
                    let name_width = (list_width - 480.0).max(240.0);
                    egui::ScrollArea::horizontal().show(ui, |ui| {
                        ui.set_min_width(list_width);
                        egui::Grid::new("library_header")
                            .striped(true)
                            .spacing(egui::vec2(0.0, 0.0))
                            .show(ui, |ui| {
                                for (label, key, width) in [
                                    ("게임 이름", "name", name_width),
                                    ("타이틀 ID", "id", 120.0),
                                    ("보유 버전", "version", 80.0),
                                    ("최신 버전", "latest_ver", 80.0),
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
                            });
                        // Only create table widgets for visible rows. A disabled
                        // paging setting must not render thousands at once.
                        let mut list_scroll = egui::ScrollArea::vertical()
                            .id_salt("library_list_rows")
                            .animated(false)
                            .auto_shrink([false, false]);
                        ui.spacing_mut().item_spacing.y = 0.0;
                        if keyboard_scroll_to_selection {
                            if let Some(position) = page_items.iter().position(|row| self.selected == Some(row.id)) {
                                let id = ui.make_persistent_id(egui::Id::new("library_list_rows"));
                                let old_offset = egui::scroll_area::State::load(ctx, id)
                                    .map(|state| state.offset.y).unwrap_or(0.0);
                                if let Some(new_offset) = scroll_offset_for_selected_row(
                                    old_offset, ui.available_height(), position, 24.0, 24.0,
                                ) {
                                    list_scroll = list_scroll.vertical_scroll_offset(new_offset);
                                }
                            }
                        } else if library_page_changed {
                            list_scroll = list_scroll.vertical_scroll_offset(0.0);
                        }
                        let _list_output = list_scroll.show_rows(ui, 24.0, page_items.len(), |ui, row_range| {
                            #[cfg(test)]
                            ctx.data_mut(|data| data.insert_temp(
                                egui::Id::new("nsl_test_visible_list_rows"), row_range.clone(),
                            ));
                            egui::Grid::new("library_rows")
                                .striped(true)
                                .spacing(egui::vec2(0.0, 0.0))
                                .show(ui, |ui| {
                                for row in &page_items[row_range] {
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
                                    #[cfg(test)]
                                    if selected {
                                        ctx.data_mut(|data| data.insert_temp(
                                            egui::Id::new("nsl_test_selected_list_rect"), response.rect,
                                        ));
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
                                    let installed_text =
                                        common::version_to_str(Some(installed_version));
                                    let response = selectable_table_text_cell(
                                        ui,
                                        80.0,
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
                                        80.0,
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
                        #[cfg(test)]
                        ctx.data_mut(|data| data.insert_temp(
                            egui::Id::new("nsl_test_list_viewport_rect"), _list_output.inner_rect,
                        ));
                    });
                }
                let duplicate_groups = self.cached_duplicates.clone();
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
                                        self.start_media_collection(MediaSelection::Screenshots, Some(&row));
                                    }
                                    if row.has_base
                                        && ui
                                            .add_enabled(
                                                self.media_result.is_none(),
                                                egui::Button::new("이 게임 아이콘 가져오기"),
                                            )
                                            .clicked()
                                    {
                                        self.start_media_collection(MediaSelection::Icons, Some(&row));
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
                                    for path in &selected_screenshot_paths {
                                        let key = path.to_string_lossy().into_owned();
                                        if !self.screenshot_textures.contains_key(&key)
                                            && !self.screenshot_missing.contains(&key)
                                        {
                                            self.image_loader.request(
                                                ImageKind::Screenshot,
                                                &key,
                                                path.clone(),
                                                900,
                                                self.selected,
                                            );
                                        }
                                    }
                                    if self.image_loader.has_pending() {
                                        ctx.request_repaint_after(std::time::Duration::from_millis(45));
                                    }
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
            if let Err(e) = database::save_config(&self.base_dir.join("config.json"), &self.config)
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
        if let Some((key, name)) = self.download_delete_confirm.clone() {
            show_centered_popup(
                ctx,
                "download_delete_confirm",
                "다운로드 삭제 확인",
                None,
                |window| window,
                |ui| {
                    ui.label(format!("「{}」", truncate_label(&name, 60)));
                    ui.label(
                        "이 다운로드를 삭제하면서 받은 파일까지 완전히 삭제합니다. 되돌릴 수 없습니다.",
                    );
                    popup_footer(ui, |ui| {
                        ui.horizontal(|ui| {
                            if ui.button("파일까지 삭제").clicked() {
                                self.download_delete_confirm = None;
                                if let Some(engine) = &self.torrent_engine {
                                    engine.cancel(&key, true);
                                }
                                self.status = "다운로드 및 파일 삭제 요청됨".into();
                                let _ = self.log_sender.send(format!(
                                    "🗑 다운로드 + 파일 삭제 · {}",
                                    truncate_label(&name, 60)
                                ));
                            }
                            if ui.button("취소").clicked() {
                                self.download_delete_confirm = None;
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
        if self.workspace_cleanup_confirm {
            let mut open = self.workspace_cleanup_confirm;
            show_centered_popup(
                ctx,
                "workspace_cleanup_confirm",
                "변환 작업폴더 정리",
                Some(&mut open),
                |window| window,
                |ui| {
                    ui.label("7일 이상 지난 NSL 변환 작업폴더만 삭제합니다. 최근 변환 파일과 게임 원본은 보존하지만, 오래된 결과물을 다른 프로그램에서 사용 중이라면 정리하지 마세요.");
                    popup_footer(ui, |ui| {
                        ui.horizontal(|ui| {
                            if ui
                                .add_enabled(
                                    self.workspace_cleanup_result.is_none()
                                        && self.decompress_result.is_none(),
                                    egui::Button::new("오래된 폴더 정리"),
                                )
                                .clicked()
                            {
                                self.workspace_cleanup_confirm = false;
                                self.start_workspace_cleanup();
                            }
                            if ui.button("취소").clicked() {
                                self.workspace_cleanup_confirm = false;
                            }
                        });
                    });
                },
            );
            if !open {
                self.workspace_cleanup_confirm = false;
            }
        }
        if let Some(info) = self.update_info.clone() {
            let mut open = true;
            show_centered_popup(
                ctx,
                "update_info",
                format!("업데이트 · 커밋 {}", info.latest_version),
                Some(&mut open),
                |window| window.max_width(720.0),
                |ui| {
                    ui.label(format!("현재 커밋: {}", info.current_version));
                    ui.label(format!("새 커밋: {}", info.latest_version));
                    ui.label(format!("릴리스 태그: {}", info.release_tag));
                    ui.label("공식 NSL 배포 패키지만 설치 후보로 사용합니다.");
                    ui.separator();
                    egui::ScrollArea::vertical()
                        .max_height(300.0)
                        .show(ui, |ui| ui.label(&info.notes));
                    popup_footer(ui, |ui| {
                        ui.horizontal(|ui| {
                            if ui
                                .add_enabled(
                                    self.update_prepare_result.is_none(),
                                    egui::Button::new("업데이트 다운로드 및 확인"),
                                )
                                .clicked()
                            {
                                self.update_info = None;
                                if let Some(url) = info.download_url.clone() {
                                    self.start_update_download(url, info.latest_commit.clone());
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
                    ui.label("검증된 업데이트를 적용하려면 앱을 종료하고 다시 시작해야 합니다.");
                    popup_footer(ui, |ui| {
                        ui.horizontal(|ui| {
                            if ui
                                .add_enabled(
                                    active_tasks.is_empty(),
                                    egui::Button::new("적용하고 다시 시작"),
                                )
                                .on_hover_text("실행 중인 다운로드·스캔·DB 작업이 모두 끝나야 업데이트를 적용할 수 있습니다")
                                .clicked()
                            {
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
                            ui.label("게임 선택 이동 ↑ ↓ ← →");
                            ui.label("한 화면 이동 Page Up / Page Down");
                            ui.label("처음 / 마지막 Home / End (Ctrl+Home / Ctrl+End)");
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
                                "Nintendo Switch Library · {}",
                                updater::build_commit_short()
                            ));
                            ui.label("닌텐도 스위치 게임 라이브러리 관리 툴");
                            ui.label("지원 형식: NSP, NSZ, XCI, XCZ");
                            ui.label(format!(
                                "DB: {}",
                                self.base_dir.join("library.db").display()
                            ));
                            ui.label(format!("캐시: {}", self.base_dir.join("caches").display()));
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

/// Original Qt/Python toolbar styling: subtle fill, 1px border, 3px radius,
/// and an accent border on hover. Call only within a scoped egui Ui so menu
/// and search widgets retain their existing appearance.
fn outlined_action_style(ui: &mut egui::Ui) {
    let visuals = ui.visuals().clone();
    let widgets = &mut ui.style_mut().visuals.widgets;
    widgets.inactive.weak_bg_fill = visuals.widgets.inactive.bg_fill;
    widgets.inactive.bg_stroke =
        egui::Stroke::new(1.0_f32, visuals.widgets.inactive.bg_stroke.color);
    widgets.hovered.weak_bg_fill = visuals.widgets.hovered.bg_fill;
    widgets.hovered.bg_stroke = egui::Stroke::new(1.0_f32, visuals.hyperlink_color);
    widgets.active.weak_bg_fill = visuals.widgets.active.bg_fill;
    widgets.active.bg_stroke = egui::Stroke::new(1.0_f32, visuals.hyperlink_color);
    for widget in [
        &mut widgets.inactive,
        &mut widgets.hovered,
        &mut widgets.active,
    ] {
        widget.rounding = egui::Rounding::same(3.0);
    }
}

fn library_section_frame(ui: &egui::Ui) -> egui::Frame {
    egui::Frame::none()
        .fill(ui.visuals().faint_bg_color)
        .stroke(egui::Stroke::new(
            1.0_f32,
            ui.visuals().widgets.noninteractive.bg_stroke.color,
        ))
        .rounding(egui::Rounding::same(4.0))
        .inner_margin(egui::Margin::same(10.0))
}

fn top_bar(app: &mut NslApp, ui: &mut egui::Ui, ctx: &egui::Context, active_tasks: &[&str]) {
    let statistics = format!(
        "파일: {}  |  게임: {}  업데이트: {}  DLC: {}  |  {}  |  DB: {}",
        common::format_count(app.stats.file_count),
        common::format_count(app.stats.base_count),
        common::format_count(app.stats.update_count),
        common::format_count(app.stats.dlc_count),
        common::format_size(app.stats.total_size.max(0) as u64),
        common::format_count(app.stats.title_db_count)
    );
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
                        let can_scan = app.scan_result.is_none()
                            && app.reload_result.is_none()
                            && app.titledb_result.is_none()
                            && app.bulk_rename_result.is_none()
                            && app.orphan_cleanup_result.is_none()
                            && app.media_result.is_none();
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
                        if ui
                            .add_enabled(
                                app.decompress_result.is_none()
                                    && app.workspace_cleanup_result.is_none(),
                                egui::Button::new("오래된 변환 임시폴더 정리"),
                            )
                            .clicked()
                        {
                            app.workspace_cleanup_confirm = true;
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
                                active_tasks.is_empty() && app.reload_result.is_none(),
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
                                app.titledb_result.is_none()
                                    && app.scan_result.is_none()
                                    && app.bulk_rename_result.is_none()
                                    && app.media_result.is_none(),
                                egui::Button::new("타이틀 DB 다운로드"),
                            )
                            .clicked()
                        {
                            app.start_titledb_download();
                            ui.close_menu();
                        }
                        if ui
                            .add_enabled(
                                app.titledb_result.is_none()
                                    && app.scan_result.is_none()
                                    && app.bulk_rename_result.is_none()
                                    && app.media_result.is_none(),
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
                                app.media_result.is_none()
                                    && app.scan_result.is_none()
                                    && app.titledb_result.is_none()
                                    && app.orphan_cleanup_result.is_none()
                                    && app.bulk_rename_result.is_none(),
                                egui::Button::new("미디어 수집"),
                            )
                            .on_hover_text("전체 게임의 아이콘과 스크린샷을 한 번에 수집합니다")
                            .clicked()
                        {
                            app.start_media_collection(MediaSelection::Both, None);
                            ui.close_menu();
                        }
                        if ui
                            .add_enabled(
                                app.media_result.is_some(),
                                egui::Button::new("미디어 수집 중지"),
                            )
                            .clicked()
                        {
                            if let Some(progress) = &app.media_progress {
                                progress.cancel();
                                app.status = "미디어 수집 중지 요청됨".into();
                            }
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
                    #[cfg(test)]
                    ctx.data_mut(|data| {
                        data.insert_temp(
                            egui::Id::new("nsl_test_top_menus_right"),
                            ui.cursor().min.x,
                        )
                    });
                    // Topmost-right statistics: small text, one line, and
                    // truncated to the space left over after the menus.
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let response = ui.add(
                            egui::Label::new(
                                egui::RichText::new(&statistics)
                                    .size(11.0)
                                    .color(ui.visuals().weak_text_color()),
                            )
                            .truncate(),
                        );
                        #[cfg(test)]
                        ctx.data_mut(|data| {
                            data.insert_temp(
                                egui::Id::new("nsl_test_top_stats_rect"),
                                response.rect,
                            )
                        });
                        response.on_hover_text(&statistics);
                    });
                },
            );
        });
        ui.separator();
        // Statistics already occupy the topmost menu row; the search field
        // still narrows on compact viewports.
        let compact_top_bar = ui.available_width() < 1400.0;
        ui.scope(|ui| {
            outlined_action_style(ui);
            #[cfg(test)]
            ctx.data_mut(|data| {
                data.insert_temp(
                    egui::Id::new("nsl_test_toolbar_button_style"),
                    (
                        ui.visuals().widgets.inactive.bg_stroke.width,
                        ui.visuals().widgets.hovered.bg_stroke.color,
                        ui.visuals().widgets.inactive.weak_bg_fill,
                    ),
                )
            });
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
                    let view_button = ui.button(if app.view_mode == "grid" {
                        "그리드뷰"
                    } else {
                        "리스트뷰"
                    });
                    #[cfg(test)]
                    ctx.data_mut(|data| {
                        data.insert_temp(
                            egui::Id::new("nsl_test_view_button_rect"),
                            view_button.rect,
                        )
                    });
                    if view_button.clicked() {
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
                        let _ =
                            database::save_config(&app.base_dir.join("config.json"), &app.config);
                    }
                    if ui.button("설정").clicked() {
                        app.settings_open = !app.settings_open;
                    }
                    if ui.button("로그").clicked() {
                        app.log_open = !app.log_open;
                    }
                    // Management tabs search inside their right panels. Hide
                    // the unrelated library-search widget to avoid two
                    // competing search fields on those screens.
                    if app.settings_open
                        || (app.folder_filter != "CATALOG" && app.folder_filter != "DOWNLOADS")
                    {
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            let search = ui.add_sized(
                                [
                                    if compact_top_bar { 170.0 } else { 220.0 },
                                    ui.spacing().interact_size.y,
                                ],
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
                        });
                    }
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
    // A headless scan must not alter the user's media download preferences.
    app.headless_mode = true;
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
        if app.scan_result.is_none() && app.reload_result.is_none() && app.scan_progress.is_none() {
            break;
        }
        if started.elapsed() > std::time::Duration::from_secs(6 * 60 * 60) {
            eprintln!("error: scan timed out");
            return 1;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    let failures = app.last_scan_errors;
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

fn load_window_icon() -> Option<egui::IconData> {
    const ICON_BYTES: &[u8] = include_bytes!("../assets/icon.ico");
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
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    // Isolated QA snapshots run before the app initializes or opens any
    // writable user state. The source DB is opened read-only.
    if args.first().map(String::as_str) == Some("--backup-db") {
        anyhow::ensure!(
            args.len() == 3,
            "usage: nsl.exe --backup-db SOURCE DESTINATION"
        );
        database::backup_read_only(
            std::path::Path::new(&args[1]),
            std::path::Path::new(&args[2]),
        )?;
        return Ok(());
    }
    let headless_scan = args.iter().any(|argument| argument == "--scan");
    // Acquire before NslApp::new: initializing SQLite, torrent sessions or
    // startup cleanup must never race another instance using the same data.
    let Some(_instance_guard) = instance::acquire(&common::base_dir())? else {
        if headless_scan {
            eprintln!("error: NSL is already running with this data folder");
            std::process::exit(3);
        }
        instance::warn_duplicate();
        return Ok(());
    };
    let app = NslApp::new()?;

    // Headless scan mode: lets release builds be verified (and scheduled scans be
    // automated) without opening a window.
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
        .with_inner_size([1280.0, 760.0])
        .with_min_inner_size([920.0, 620.0])
        .with_maximized(start_maximized);
    if let Some(icon) = load_window_icon() {
        viewport = viewport.with_icon(icon);
    }
    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };
    let window_title = format!("NSL  {}", updater::build_commit_short());
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
    fn library_store_and_screenshots_paint_text_before_lazy_image_uploads() {
        let (mut app, _dir) = seeded_app();
        app.torrent_engine = None;
        let ctx = run_app_ticks(&mut app, 2);
        wait_for_rows(&mut app, &ctx);
        let base = app.rows.iter().find(|r| r.has_base).unwrap().clone();
        let icon = image::RgbaImage::from_pixel(320, 160, image::Rgba([60, 90, 120, 255]));
        icon.save(app.icon_dir.join(format!("{}.png", base.title_id)))
            .unwrap();
        let screenshot_dir = app.screenshot_dir.join(&base.title_id);
        std::fs::create_dir_all(&screenshot_dir).unwrap();
        icon.save(screenshot_dir.join("first.png")).unwrap();
        std::fs::create_dir_all(&app.catalog_cover_dir).unwrap();
        image::RgbImage::from_pixel(320, 160, image::Rgb([60, 90, 120]))
            .save_with_format(
                app.catalog_cover_dir.join("sample-cover.jpg"),
                image::ImageFormat::Jpeg,
            )
            .unwrap();
        app.catalog_state.items = vec![catalog::CatalogItem {
            title: "Text is visible before the store cover".into(),
            topic_id: "sample-cover".into(),
            ..Default::default()
        }];
        app.selected = Some(base.id);
        app.detail_tab = 0;
        let mut budget = 1;
        app.ensure_icon_texture(&ctx, &base.title_id, &mut budget);
        assert!(
            !app.icon_textures.contains_key(&base.title_id),
            "icon decoded on GUI thread"
        );
        let render = |app: &mut NslApp| {
            ctx.begin_pass(egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1456.0, 900.0),
                )),
                ..Default::default()
            });
            app.ui_frame(&ctx);
            let _ = ctx.end_pass();
        };
        app.folder_filter = "CATALOG".into();
        render(&mut app);
        assert!(
            ctx.data_mut(
                |data| data.get_temp::<egui::Rect>(egui::Id::new("nsl_test_catalog_first_row"))
            )
            .is_some(),
            "store text row failed to render"
        );
        assert!(
            app.screenshot_textures.is_empty(),
            "background tab decoded screenshots"
        );
        for _ in 0..90 {
            app.poll_loaded_images(&ctx);
            if app.icon_textures.contains_key(&base.title_id)
                && app.catalog_cover_textures.contains_key("sample-cover")
            {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(app.icon_textures.contains_key(&base.title_id));
        assert!(app.catalog_cover_textures.contains_key("sample-cover"));
        assert!(app.screenshot_textures.is_empty());
        app.folder_filter.clear();
        app.detail_tab = 2;
        for _ in 0..90 {
            render(&mut app);
            if !app.screenshot_textures.is_empty() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(
            app.screenshot_textures.len(),
            1,
            "visible screenshot not decoded"
        );
        assert!(app.icon_textures.len() <= ICON_CACHE_MAX);
        assert!(app.catalog_cover_textures.len() <= STORE_COVER_CACHE_MAX);
    }

    #[test]
    fn media_plan_collects_both_types_once_and_keeps_valid_duplicate_metadata() {
        let (mut app, _dir) = seeded_app();
        let ctx = run_app_ticks(&mut app, 2);
        wait_for_rows(&mut app, &ctx);
        let mut row = app.rows.iter().find(|r| r.has_base).unwrap().clone();
        row.title_id = "0100D2C0115B6000".into();
        row.icon_url.clear();
        row.screenshots = "[]".into();
        row.path = "E:\\sample.nsp".into();
        let mut duplicate = row.clone();
        duplicate.icon_url = "https://example.test/icon.png".into();
        duplicate.screenshots = "[\"https://example.test/image.jpg\"]".into();
        duplicate.path.clear();
        let rows = [row.clone(), duplicate.clone()];
        let full = media_collection_plan(&rows, None, MediaSelection::Both);
        assert_eq!(full.total(), 2);
        assert_eq!(full.icons.len(), 1);
        assert_eq!(full.screenshots.len(), 1);
        assert_eq!(full.icons[0].1, duplicate.icon_url);
        assert_eq!(full.icons[0].2, row.path);
        assert_eq!(full.screenshots[0].1, duplicate.screenshots);
        assert_eq!(
            media_collection_plan(&rows, None, MediaSelection::Icons).total(),
            1
        );
        assert_eq!(
            media_collection_plan(&rows, None, MediaSelection::Screenshots).total(),
            1
        );
        let one = media_collection_plan(&rows, Some(&row), MediaSelection::Both);
        assert_eq!(one.total(), 1);
        assert_eq!(one.screenshots.len(), 0);
    }

    #[test]
    fn automatic_prefetch_combines_icons_and_screenshots_after_reload() {
        let (mut app, _dir) = seeded_app();
        let ctx = run_app_ticks(&mut app, 2);
        wait_for_rows(&mut app, &ctx);
        let mut row = app.rows.iter().find(|r| r.has_base).unwrap().clone();
        row.screenshots = "[\"https://example.test/one.jpg\"]".into();
        app.rows = vec![row.clone()];

        std::fs::create_dir_all(&app.icon_dir).unwrap();
        std::fs::create_dir_all(app.screenshot_dir.join(&row.title_id)).unwrap();
        let image_data = |format| {
            let image = image::DynamicImage::new_rgb8(2, 2);
            let mut buffer = std::io::Cursor::new(Vec::new());
            image.write_to(&mut buffer, format).unwrap();
            buffer.into_inner()
        };
        std::fs::write(
            app.icon_dir.join(format!("{}.png", row.title_id)),
            image_data(image::ImageFormat::Png),
        )
        .unwrap();
        std::fs::write(
            app.screenshot_dir.join(&row.title_id).join("001.jpg"),
            image_data(image::ImageFormat::Jpeg),
        )
        .unwrap();

        app.pending_icon_prefetch = true;
        app.pending_screenshot_prefetch = true;
        let (_tx, pending_reload) = mpsc::channel();
        app.reload_result = Some(pending_reload);
        app.start_pending_media();
        assert!(app.media_result.is_none(), "must wait for new DB rows");
        assert!(app.pending_icon_prefetch && app.pending_screenshot_prefetch);
        app.reload_result = None;
        app.start_pending_media();
        assert!(!app.pending_icon_prefetch && !app.pending_screenshot_prefetch);
        let outcome = app
            .media_result
            .as_ref()
            .expect("one combined worker")
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap()
            .unwrap();
        assert_eq!(outcome.icons, 0);
        assert_eq!(outcome.screenshots, 0);
        assert_eq!(outcome.failed, 0);
        assert_eq!(outcome.skipped, 2);
        assert_eq!(app.media_progress.as_ref().unwrap().snapshot(), (2, 2, 0));
        assert_eq!(app.media_progress.as_ref().unwrap().stage_label(), "완료");
    }

    #[test]
    fn disconnected_background_worker_returns_error_instead_of_staying_busy() {
        let (tx, rx) = mpsc::channel::<std::result::Result<usize, String>>();
        let worker = Some(rx);
        assert!(poll_background(&worker, "테스트").is_none());
        drop(tx);
        assert!(poll_background(&worker, "테스트").unwrap().is_err());
    }

    #[test]
    fn queued_reload_runs_after_inflight_reload_finishes() {
        let (mut app, _dir) = seeded_app();
        let ctx = run_app_ticks(&mut app, 2);
        wait_for_rows(&mut app, &ctx);
        let (tx, rx) = mpsc::channel::<std::result::Result<Vec<library::GameFile>, String>>();
        app.reload_result = Some(rx);
        app.start_reload();
        assert!(app.reload_dirty);
        tx.send(Ok(app.rows.clone())).unwrap();
        run_app_ticks(&mut app, 1);
        assert!(!app.reload_dirty);
        assert!(app.reload_result.is_some(), "new reload was dropped");
        wait_for_rows(&mut app, &ctx);
    }

    #[test]
    fn cancelled_scan_never_schedules_media_collection() {
        let (mut app, _dir) = seeded_app();
        let ctx = run_app_ticks(&mut app, 2);
        wait_for_rows(&mut app, &ctx);
        app.config.settings.insert("scan_icon".into(), "1".into());
        app.config
            .settings
            .insert("scan_screenshot".into(), "1".into());
        let (tx, rx) = mpsc::channel::<std::result::Result<ScanOutcome, String>>();
        app.scan_result = Some(rx);
        tx.send(Ok((0, 0, true, 0))).unwrap();
        run_app_ticks(&mut app, 1);
        assert!(!app.pending_icon_prefetch);
        assert!(!app.pending_screenshot_prefetch);
        assert!(app.media_result.is_none());
    }

    #[test]
    fn schema_bump_forces_one_rescan() {
        // Records written before the version column cannot be upgraded by a
        // cache hit, so a schema mismatch must force a full re-analysis.
        let mut config = database::Config::default();
        assert_ne!(
            config
                .settings
                .get("scan_schema")
                .and_then(|v| v.as_str())
                .unwrap_or(""),
            SCAN_SCHEMA_VERSION,
            "a fresh config must re-analyze once"
        );
        config.settings.insert(
            "scan_schema".into(),
            serde_json::Value::String(SCAN_SCHEMA_VERSION.into()),
        );
        assert_eq!(
            config
                .settings
                .get("scan_schema")
                .and_then(|v| v.as_str())
                .unwrap_or(""),
            SCAN_SCHEMA_VERSION,
            "after the first scan the cache is trusted again"
        );
    }

    #[test]
    fn incremental_scan_reanalyzes_unchanged_files_on_schema_change() {
        let (mut app, dir) = seeded_app();
        let folder = dir.path().join("synthetic-games");
        std::fs::create_dir_all(&folder).unwrap();
        let filename = "Test [0100D2C0115B6000][v123].nsp";
        let path = folder.join(filename);
        std::fs::write(&path, vec![0u8; 4096]).unwrap();
        let normalized = normalize_scan_path(&path);
        let metadata = std::fs::metadata(&path).unwrap();
        let mtime = metadata
            .modified()
            .ok()
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs_f64());
        app.db
            .upsert_files(&[database::FileRecord {
                path: normalized.clone(),
                filename: filename.into(),
                size: metadata.len() as i64,
                mtime,
                file_type: "nsp".into(),
                scan_folder: normalize_scan_path(&folder),
                fast_hash: scanner::fast_hash(&path).unwrap(),
                nacp_ko: String::new(),
                nacp_en: String::new(),
                nacp_ja: String::new(),
                contents: vec![scanner::Content {
                    title_id: Some("0100D2C0115B6000".into()),
                    version: 999,
                    content_type: "base".into(),
                    source: "old_schema".into(),
                }],
            }])
            .unwrap();
        app.folders = vec![normalize_scan_path(&folder)];
        app.config.settings.insert("scan_schema".into(), "0".into());
        app.headless_mode = true;
        app.start_scan(true);
        let ctx = egui::Context::default();
        assert!(run_until_idle(&mut app, &ctx));
        let contents = app.db.file_contents(&normalized).unwrap();
        assert_eq!(contents.len(), 1);
        assert_eq!(
            contents[0].version, 123,
            "unchanged metadata must not bypass schema upgrade"
        );
    }

    #[test]
    fn missing_dlc_exclusion_invalidates_stats_and_list_cache() {
        let (mut app, _dir) = seeded_app();
        let ctx = run_app_ticks(&mut app, 2);
        wait_for_rows(&mut app, &ctx);
        let id = "0100D2C0115B7001";
        let base = app.rows.iter_mut().find(|row| row.has_base).unwrap();
        base.missing_dlcs = vec![(id.into(), "DLC".into())];
        app.rows_generation += 1;
        app.folder_filter = "MISSING_DLCS".into();
        app.refresh_library_stats();
        assert_eq!(app.cached_missing_dlcs, 1);
        assert_eq!(app.library_rows_cached().len(), 1);
        app.config
            .settings
            .insert("exception_dlc".into(), id.into());
        app.refresh_library_stats();
        assert_eq!(app.cached_missing_dlcs, 0);
        assert_eq!(app.library_rows_cached().len(), 0);
    }

    #[test]
    fn python_style_toolbar_buttons_remain_bordered_and_clickable() {
        let (mut app, _dir) = seeded_app();
        let ctx = run_app_ticks(&mut app, 2);
        wait_for_rows(&mut app, &ctx);
        let (stroke_width, hover_color, background) = ctx.data_mut(|data| {
            data.get_temp::<(f32, egui::Color32, egui::Color32)>(egui::Id::new(
                "nsl_test_toolbar_button_style",
            ))
            .unwrap()
        });
        assert!(stroke_width >= 1.0, "toolbar action border is invisible");
        assert_ne!(
            background,
            egui::Color32::TRANSPARENT,
            "toolbar action background is transparent"
        );
        assert_eq!(
            hover_color,
            ctx.style().visuals.hyperlink_color,
            "toolbar hover border no longer matches the theme accent"
        );

        let rect = ctx.data_mut(|data| {
            data.get_temp::<egui::Rect>(egui::Id::new("nsl_test_view_button_rect"))
                .unwrap()
        });
        let at = rect.center();
        let before = app.view_mode.clone();
        for pressed in [true, false] {
            ctx.begin_pass(egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1456.0, 900.0),
                )),
                events: vec![
                    egui::Event::PointerMoved(at),
                    egui::Event::PointerButton {
                        pos: at,
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
                ..Default::default()
            });
            app.ui_frame(&ctx);
            let _ = ctx.end_pass();
        }
        assert_ne!(
            app.view_mode, before,
            "outlined view button no longer toggles list/grid"
        );
    }

    #[test]
    fn store_sidebar_filters_real_genre_and_format_without_guessing_dlc() {
        let action_nsz = catalog::CatalogItem {
            title: "Alpha [NSZ] + Update + DLC".into(),
            title_id: Some("0100D2C0115B6000".into()),
            genre: "Action, Adventure".into(),
            image_format: ".NSZ (compressed)".into(),
            ..Default::default()
        };
        let puzzle_xci = catalog::CatalogItem {
            title: "Beta [XCI]".into(),
            title_id: Some("0100D2C0115B7000".into()),
            genre: "Puzzle".into(),
            image_format: "XCI".into(),
            ..Default::default()
        };
        assert!(catalog_matches_filters(&action_nsz, "", "all", "all"));
        assert!(catalog_matches_filters(
            &action_nsz,
            "alpha",
            "Action",
            "NSZ"
        ));
        assert!(catalog_matches_filters(
            &action_nsz,
            "0100d2c0115b6",
            "Adventure",
            "NSZ"
        ));
        assert!(!catalog_matches_filters(
            &action_nsz,
            "beta",
            "Action",
            "NSZ"
        ));
        assert!(!catalog_matches_filters(&action_nsz, "", "Puzzle", "NSZ"));
        assert!(!catalog_matches_filters(&action_nsz, "", "Action", "NSP"));
        assert!(catalog_matches_filters(
            &puzzle_xci,
            "beta",
            "Puzzle",
            "XCI"
        ));
        assert!(!catalog_matches_filters(&puzzle_xci, "", "Action", "XCI"));
    }

    #[test]
    fn download_sidebar_filters_search_and_mutually_exclusive_states() {
        let mut progress = torrent::TorrentProgress {
            name: "Alpha Update DLC".into(),
            state: "다운로드 중".into(),
            ..Default::default()
        };
        assert!(download_matches_filters("job-a", &progress, "", "all"));
        assert!(download_matches_filters(
            "job-a", &progress, "update", "active"
        ));
        assert!(download_matches_filters(
            "job-a", &progress, "job-a", "active"
        ));
        assert!(!download_matches_filters("job-a", &progress, "beta", "all"));
        assert!(!download_matches_filters("job-a", &progress, "", "paused"));
        progress.paused = true;
        assert!(download_matches_filters("job-a", &progress, "", "paused"));
        assert!(!download_matches_filters("job-a", &progress, "", "active"));
        progress.error = Some("network error".into());
        assert!(download_matches_filters("job-a", &progress, "", "failed"));
        assert!(!download_matches_filters("job-a", &progress, "", "paused"));
        progress.paused = false;
        progress.error = None;
        progress.finished = true;
        assert!(download_matches_filters("job-a", &progress, "", "finished"));
        assert!(!download_matches_filters("job-a", &progress, "", "active"));
    }

    #[test]
    fn store_sidebar_filter_changes_reset_pagination_and_center_rows() {
        let (mut app, _dir) = seeded_app();
        app.catalog_state.items = vec![
            catalog::CatalogItem {
                title: "Alpha [NSZ]".into(),
                topic_id: "a".into(),
                genre: "Action".into(),
                image_format: "NSZ".into(),
                ..Default::default()
            },
            catalog::CatalogItem {
                title: "Beta [XCI]".into(),
                topic_id: "b".into(),
                genre: "Puzzle".into(),
                image_format: "XCI".into(),
                ..Default::default()
            },
        ];
        app.folder_filter = "CATALOG".into();
        app.catalog_genre_filter = "Action".into();
        app.catalog_format_filter = "NSZ".into();
        app.catalog_page = 99;
        app.catalog_search_cache = None;
        let ctx = run_app_ticks(&mut app, 3);
        assert_eq!(app.catalog_matching_indices, vec![0]);
        assert_eq!(app.catalog_page, 0);
        assert!(ctx
            .data_mut(|data| {
                data.get_temp::<egui::Rect>(egui::Id::new("nsl_test_catalog_first_row"))
            })
            .is_some());
        app.catalog_genre_filter = "Puzzle".into();
        run_app_ticks(&mut app, 2);
        assert_eq!(app.catalog_matching_indices.len(), 0);
        app.catalog_format_filter = "XCI".into();
        run_app_ticks(&mut app, 2);
        assert_eq!(app.catalog_matching_indices, vec![1]);
        app.catalog_search = "alpha".into();
        app.catalog_search_committed = "alpha".into();
        run_app_ticks(&mut app, 2);
        assert!(app.catalog_matching_indices.is_empty());
    }

    #[test]
    fn store_and_download_tabs_have_consistent_compact_rows_at_three_widths() {
        let (mut app, _dir) = seeded_app();
        let ctx = run_app_ticks(&mut app, 2);
        wait_for_rows(&mut app, &ctx);
        // Management views have independent right-hand controls even when
        // the library still has a selected game and a saved detail width.
        let selected_library_game = app.rows.iter().find(|row| row.has_base).unwrap().id;
        app.selected = Some(selected_library_game);
        // The real torrent worker normally supplies the live snapshot each
        // frame. Keep synthetic status cases intact during layout-only QA.
        app.torrent_engine = None;
        app.catalog_state.items = (0..240)
            .map(|i| catalog::CatalogItem {
                title: format!(
                    "Long Switch catalog game number {i:04} with extended release name {}",
                    "X".repeat(80)
                ),
                topic_id: format!("layout-{i:04}"),
                title_id: Some(format!("0100{i:012X}")),
                size: "12.5 GB".into(),
                image_format: "NSP + Update + DLC".into(),
                ..Default::default()
            })
            .collect();
        app.catalog_search_cache = None;
        app.catalog_page = 1;
        app.torrent_progress = vec![
            (
                "downloading".into(),
                torrent::TorrentProgress {
                    name: "A very long game name with an update and multiple DLCs ".repeat(6),
                    total_bytes: 10_000_000,
                    progress_bytes: 3_000_000,
                    download_speed_bps: 400_000,
                    finished: false,
                    paused: false,
                    state: "다운로드 중".into(),
                    error: None,
                },
            ),
            (
                "paused".into(),
                torrent::TorrentProgress {
                    name: "Paused game".into(),
                    total_bytes: 400_000,
                    progress_bytes: 100_000,
                    download_speed_bps: 0,
                    finished: false,
                    paused: true,
                    state: "일시정지".into(),
                    error: None,
                },
            ),
            (
                "finished".into(),
                torrent::TorrentProgress {
                    name: "Completed game".into(),
                    total_bytes: 600_000,
                    progress_bytes: 600_000,
                    download_speed_bps: 0,
                    finished: true,
                    paused: false,
                    state: "완료".into(),
                    error: None,
                },
            ),
            (
                "error".into(),
                torrent::TorrentProgress {
                    name: "Failed torrent".into(),
                    total_bytes: 900_000,
                    progress_bytes: 20_000,
                    download_speed_bps: 0,
                    finished: false,
                    paused: false,
                    state: "오류".into(),
                    error: Some("로컬 테스트 오류".into()),
                },
            ),
        ];
        for (tab, marker) in [
            ("CATALOG", "nsl_test_catalog_last_row"),
            ("DOWNLOADS", "nsl_test_download_last_row"),
        ] {
            app.folder_filter = tab.into();
            let sidebar_marker = if tab == "CATALOG" {
                "nsl_test_store_sidebar_rect"
            } else {
                "nsl_test_download_sidebar_rect"
            };
            let search_marker = if tab == "CATALOG" {
                "nsl_test_store_sidebar_search"
            } else {
                "nsl_test_download_sidebar_search"
            };
            for width in [1456.0, 980.0, 720.0] {
                ctx.begin_pass(egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(width, 750.0),
                    )),
                    ..Default::default()
                });
                app.ui_frame(&ctx);
                let _ = ctx.end_pass();
                let rect = ctx
                    .data_mut(|data| data.get_temp::<egui::Rect>(egui::Id::new(marker)))
                    .unwrap_or_else(|| panic!(
                        "missing {marker} tab={tab} width={width} actual_filter={} catalog_items={} matched={} page={} settings_open={}",
                        app.folder_filter, app.catalog_state.items.len(),
                        app.catalog_matching_indices.len(), app.catalog_page, app.settings_open,
                    ));
                let (sidebar, search) = ctx.data_mut(|data| {
                    (
                        data.get_temp::<egui::Rect>(egui::Id::new(sidebar_marker))
                            .expect("management right sidebar missing"),
                        data.get_temp::<egui::Rect>(egui::Id::new(search_marker))
                            .expect("management sidebar search missing"),
                    )
                });
                assert!(
                    sidebar.width() >= 234.0 && sidebar.width() <= 366.0,
                    "{tab} sidebar width at {width}: {sidebar:?}"
                );
                assert!(
                    (sidebar.right() - width).abs() < 3.0,
                    "{tab} panel does not touch the right edge: {sidebar:?}"
                );
                assert!(
                    search.left() >= sidebar.left() && search.right() <= sidebar.right(),
                    "{tab} search is not inside the sidebar: {search:?}, {sidebar:?}"
                );
                assert!(
                    rect.is_finite() && rect.left() >= -1.0 && rect.right() <= width + 1.0,
                    "{tab} row spills off screen at {width}: {rect:?}"
                );
                assert!(
                    rect.right() <= sidebar.left() + 1.0
                        && rect.right() >= sidebar.left() - 65.0,
                    "{tab} row does not occupy the available center width: row={rect:?}, sidebar={sidebar:?}"
                );
                assert!(
                    rect.height() >= 60.0 && rect.height() < 175.0,
                    "{tab} row height differs from library compact layout: {rect:?}"
                );
                assert!(
                    rect.top() > 80.0,
                    "{tab} row overlaps the library tab/header: {rect:?}"
                );
            }
        }
        assert_eq!(
            app.catalog_state.items.len(),
            240,
            "catalog view unexpectedly modified source content"
        );
        assert_eq!(
            app.torrent_progress.len(),
            4,
            "downloading/paused/completed/failed rows must remain intact"
        );
        assert_eq!(
            app.selected,
            Some(selected_library_game),
            "switching management tabs must not lose the selected library game"
        );
    }

    #[test]
    fn store_first_catalog_item_is_fully_visible_after_opening_tab() {
        let (mut app, _dir) = seeded_app();
        let ctx = run_app_ticks(&mut app, 2);
        wait_for_rows(&mut app, &ctx);
        app.catalog_state.items = (0..25)
            .map(|i| catalog::CatalogItem {
                title: format!("Game {i:03}: update + DLC"),
                topic_id: format!("first-row-{i:04}"),
                magnet: format!("magnet:?xt=urn:btih:{i:040x}"),
                size: "12.5 GB".into(),
                ..Default::default()
            })
            .collect();
        app.catalog_search_cache = None;
        app.catalog_page = 0;
        app.folder_filter = "CATALOG".into();
        app.selected = None;
        for _ in 0..4 {
            ctx.begin_pass(egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1296.0, 799.0),
                )),
                ..Default::default()
            });
            app.ui_frame(&ctx);
            let _ = ctx.end_pass();
        }
        let (first, viewport) = ctx.data_mut(|data| {
            (
                data.get_temp::<egui::Rect>(egui::Id::new("nsl_test_catalog_first_row"))
                    .unwrap(),
                data.get_temp::<egui::Rect>(egui::Id::new("nsl_test_catalog_viewport"))
                    .unwrap(),
            )
        });
        eprintln!("CATALOG_FIRST_ROW={first:?} VIEWPORT={viewport:?}");
        assert!(
            first.top() >= viewport.top() - 1.0,
            "first catalog item clipped above viewport; first={first:?}, viewport={viewport:?}"
        );
        assert!(
            first.bottom() <= viewport.bottom() + 1.0,
            "first catalog item clipped below viewport"
        );
    }

    #[test]
    fn store_catalog_does_not_truncate_at_two_thousand_entries() {
        let (mut app, _dir) = seeded_app();
        app.catalog_state.items = (0..2_101)
            .map(|i| catalog::CatalogItem {
                title: format!("Catalog item {i}"),
                magnet: format!("magnet:?xt=urn:btih:{i}"),
                ..Default::default()
            })
            .collect();
        app.folder_filter = "CATALOG".into();
        run_app_ticks(&mut app, 2);
        assert_eq!(app.catalog_matching_indices.len(), 2_101);
        app.catalog_page = 21;
        run_app_ticks(&mut app, 1);
        assert_eq!(app.catalog_page, 21);
    }

    #[test]
    fn catalog_cover_jobs_respect_disk_cache_and_retry_cooldown() {
        let (mut app, dir) = seeded_app();
        let item = catalog::CatalogItem {
            topic_id: "opt-test-001".into(),
            cover: "https://example.test/cover.jpg".into(),
            ..Default::default()
        };
        let now = std::time::Instant::now();
        assert!(app.should_prefetch_catalog_cover(&item, now));
        app.catalog_cover_retry_after
            .insert(item.topic_id.clone(), now + STORE_COVER_RETRY_DELAY);
        assert!(!app.should_prefetch_catalog_cover(&item, now));
        assert!(app.should_prefetch_catalog_cover(
            &item,
            now + STORE_COVER_RETRY_DELAY + std::time::Duration::from_millis(1)
        ));
        let cover_dir = dir.path().join("icons").join("store");
        std::fs::create_dir_all(&cover_dir).unwrap();
        std::fs::write(cover_dir.join("opt-test-001.jpg"), b"cached").unwrap();
        assert!(!app.should_prefetch_catalog_cover(
            &item,
            now + STORE_COVER_RETRY_DELAY + std::time::Duration::from_secs(1)
        ));
        let invalid = catalog::CatalogItem {
            topic_id: "../escape".into(),
            ..item
        };
        assert!(!app.should_prefetch_catalog_cover(&invalid, now));
    }

    #[test]
    fn background_messages_do_not_force_log_window_open() {
        let (mut app, _dir) = seeded_app();
        assert!(!app.log_open);
        app.log_sender
            .send("background cover update".into())
            .unwrap();
        run_app_ticks(&mut app, 2);
        assert!(app
            .log_lines
            .iter()
            .any(|line| line == "background cover update"));
        assert!(
            !app.log_open,
            "background logs must not obscure the library"
        );
    }

    #[test]
    fn log_panel_renders_only_visible_rows_when_messages_grow() {
        let (mut app, _dir) = seeded_app();
        let ctx = run_app_ticks(&mut app, 2);
        wait_for_rows(&mut app, &ctx);
        app.log_open = true;
        let render = |app: &mut NslApp| -> usize {
            ctx.begin_pass(egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1280.0, 760.0),
                )),
                ..Default::default()
            });
            app.ui_frame(&ctx);
            ctx.end_pass().shapes.len()
        };
        app.log_lines = (0..10).map(|i| format!("log {i:04}")).collect();
        let small = render(&mut app);
        app.log_lines = (0..2_000).map(|i| format!("log {i:04}")).collect();
        let large = render(&mut app);
        assert!(
            large < small + 75,
            "log was laid out in full: {small} -> {large} shapes"
        );
    }

    #[test]
    fn footer_stays_at_screen_bottom_with_logs_open_and_closed() {
        let (mut app, _dir) = seeded_app();
        let ctx = run_app_ticks(&mut app, 2);
        wait_for_rows(&mut app, &ctx);
        app.log_lines = (0..120).map(|i| format!("작업 로그 {i}")).collect();
        for (width, height) in [
            (1456.0, 900.0),
            (980.0, 640.0),
            (680.0, 420.0),
            (500.0, 330.0),
        ] {
            for log_open in [false, true] {
                app.log_open = log_open;
                ctx.begin_pass(egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(width, height),
                    )),
                    ..Default::default()
                });
                app.ui_frame(&ctx);
                let _ = ctx.end_pass();
                let (status, progress, log) = ctx.data_mut(|data| {
                    (
                        data.get_temp::<egui::Rect>(egui::Id::new("nsl_test_status_rect"))
                            .unwrap(),
                        data.get_temp::<egui::Rect>(egui::Id::new("nsl_test_status_progress_rect"))
                            .unwrap(),
                        data.get_temp::<egui::Rect>(egui::Id::new("nsl_test_log_rect"))
                            .unwrap(),
                    )
                });
                assert!(
                    (status.bottom() - height).abs() <= 1.0,
                    "footer left the bottom: {status:?}, h={height}"
                );
                assert!(
                    progress.top() >= status.top() - 1.0
                        && progress.bottom() <= status.bottom() + 1.0,
                    "progress left the fixed status bar: {progress:?} vs {status:?}"
                );
                assert!(
                    progress.right() <= width + 1.0,
                    "progress overflows width {width}"
                );
                if log_open {
                    assert!(log.is_positive(), "log panel was not rendered");
                    let scroll = ctx.data_mut(|data| {
                        data.get_temp::<egui::Rect>(egui::Id::new("nsl_test_log_scroll_rect"))
                            .unwrap()
                    });
                    assert!(
                        scroll.bottom() <= status.top() + 0.5,
                        "log scroll area entered the status bar: {scroll:?} vs {status:?}"
                    );
                    assert!(
                        log.bottom() <= status.top() + 0.5,
                        "log must never cover footer: log={log:?}, status={status:?}"
                    );
                } else {
                    assert!(!log.is_positive(), "closed log consumes screen space");
                }
            }
        }
    }

    #[test]
    fn footer_progress_has_real_percentages_and_indeterminate_stages() {
        let (mut app, _dir) = seeded_app();
        let (_tx, rx) = mpsc::channel::<std::result::Result<titledb::UpdateOutcome, String>>();
        app.titledb_result = Some(rx);
        let progress = Arc::new(TaskProgress::default());
        progress.begin("타이틀 DB 2/3 · versions.json", Some(100), true);
        progress.set(37);
        app.titledb_progress = Some(progress.clone());
        let views = app.active_progress_views();
        let titledb = views
            .iter()
            .find(|view| view.label.contains("versions.json"))
            .unwrap();
        assert_eq!(titledb.fraction, Some(0.37));
        assert!(!titledb.detail.is_empty());
        progress.begin("타이틀 DB 서버 연결 중", None, true);
        let waiting = app.active_progress_views();
        let titledb = waiting
            .iter()
            .find(|view| view.label.contains("연결 중"))
            .unwrap();
        assert!(
            titledb.fraction.is_none(),
            "unknown size must not fabricate a percent"
        );
        app.titledb_result = None;
        app.titledb_progress = None;
        let (_tx, rx) = mpsc::channel::<std::result::Result<(usize, usize), String>>();
        app.bulk_rename_result = Some(rx);
        let rename = Arc::new(TaskProgress::default());
        rename.begin("파일명 일괄 정규화", Some(5), false);
        rename.set(3);
        app.rename_progress = Some(rename);
        assert!(app
            .active_progress_views()
            .iter()
            .any(|view| view.fraction == Some(0.6)));
    }

    #[test]
    fn escape_clears_both_edit_buffer_and_committed_filters() {
        // Regression from real Windows GUI QA: Esc emptied the search edit,
        // while its debounced query still filtered the library indefinitely.
        let (mut app, _dir) = seeded_app();
        app.search = "nonexistent-game".into();
        app.search_committed = app.search.clone();
        app.search_pending = Some(std::time::Instant::now() + std::time::Duration::from_secs(3));
        app.catalog_search = "Mario".into();
        app.catalog_search_committed = "Mario".into();
        app.catalog_search_pending =
            Some(std::time::Instant::now() + std::time::Duration::from_secs(3));
        app.folder_filter = "CATALOG".into();
        app.current_page = 3;
        app.catalog_page = 2;
        let ctx = egui::Context::default();
        ctx.begin_pass(egui::RawInput {
            events: vec![egui::Event::Key {
                key: egui::Key::Escape,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            }],
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1400.0, 860.0),
            )),
            ..Default::default()
        });
        app.ui_frame(&ctx);
        let _ = ctx.end_pass();
        assert_eq!(app.search, "");
        assert_eq!(app.search_committed, "");
        assert!(app.search_pending.is_none());
        assert_eq!(app.catalog_search, "");
        assert_eq!(app.catalog_search_committed, "");
        assert!(app.catalog_search_pending.is_none());
        assert_eq!(app.current_page, 0);
        assert_eq!(app.catalog_page, 0);
    }

    #[test]
    fn top_library_statistics_stay_small_right_aligned_and_on_the_first_row() {
        use egui::containers::panel::PanelState;

        let (mut app, _dir) = seeded_app();
        let ctx = run_app_ticks(&mut app, 2);
        wait_for_rows(&mut app, &ctx);
        let mut top_heights = Vec::new();
        for width in [1456.0, 980.0, 720.0] {
            ctx.begin_pass(egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(width, 800.0),
                )),
                ..Default::default()
            });
            app.ui_frame(&ctx);
            let _ = ctx.end_pass();
            let (stats_rect, menus_right) = ctx.data_mut(|data| {
                (
                    data.get_temp::<egui::Rect>(egui::Id::new("nsl_test_top_stats_rect"))
                        .unwrap(),
                    data.get_temp::<f32>(egui::Id::new("nsl_test_top_menus_right"))
                        .unwrap(),
                )
            });
            assert!(stats_rect.left() >= menus_right - 1.0,
                "stats overlap top menu: width={width}, label={stats_rect:?}, menu_end={menus_right}");
            assert!(
                stats_rect.right() <= width + 1.0,
                "stats overflow right edge: width={width}, label={stats_rect:?}"
            );
            assert!(
                stats_rect.top() < 40.0 && stats_rect.height() < 19.0,
                "stats should be small and on top menu row: width={width}, label={stats_rect:?}"
            );
            let top_height = PanelState::load(&ctx, egui::Id::new("top"))
                .unwrap()
                .size()
                .y;
            top_heights.push(top_height);
            assert!(
                top_height < 95.0,
                "statistics added an unwanted toolbar row: {top_height}"
            );
        }
        assert!(
            (top_heights[0] - top_heights[1]).abs() < 3.0,
            "toolbar grows on compact screens: {top_heights:?}"
        );
    }

    #[test]
    fn detail_panel_keeps_resized_width_when_switching_between_long_game_names() {
        use egui::containers::panel::PanelState;
        let (mut app, _dir) = seeded_app();
        let ctx = run_app_ticks(&mut app, 2);
        wait_for_rows(&mut app, &ctx);

        let template = app.rows.iter().find(|row| row.has_base).unwrap().clone();
        let mut short = template.clone();
        short.id = 10_001;
        short.title_name = "Short Title".into();
        short.filename = "Short.nsp".into();
        let mut long = template.clone();
        long.id = 10_002;
        long.title_name = "An Extremely Long Game Title With Many Words ".repeat(12);
        long.filename = "Some Very Long Filename ".repeat(10);
        long.path = format!("E:\\NSW\\{}", long.filename);
        long.nacp_en = long.title_name.clone();
        long.nacp_ko = "긴 한글 게임 이름과 버전 정보 ".repeat(35);
        long.nacp_ja = "長い日本語ゲームタイトル".repeat(30);
        long.description = "Long game description that must wrap inside the panel. ".repeat(70);
        long.owned_dlcs = vec![("0100D2C0115B7001".into(), "DLC Long Label ".repeat(35))];
        long.missing_dlcs = vec![("0100D2C0115B7002".into(), "Other DLC ".repeat(35))];
        app.rows = vec![short, long];
        app.rows_generation = app.rows_generation.wrapping_add(1);
        app.selected = Some(10_001);
        app.folder_filter.clear();
        app.kind_filter = "base".into();
        app.config
            .settings
            .insert("sash_remember".into(), "1".into());
        let id = egui::Id::new("detail");
        let mut observed = Vec::new();
        for preferred_width in [320.0f32, 380.0, 740.0, 900.0] {
            let expected = format!("{preferred_width:.0}");
            app.config
                .settings
                .insert("sash_pos".into(), expected.clone().into());
            ctx.data_mut(|data| {
                data.insert_persisted(
                    id,
                    PanelState {
                        rect: egui::Rect::from_min_size(
                            egui::pos2(1456.0 - preferred_width, 0.0),
                            egui::vec2(preferred_width, 900.0),
                        ),
                    },
                )
            });
            for detail_tab in 0..4 {
                app.detail_tab = detail_tab;
                for selected in [10_001, 10_002, 10_001, 10_002, 10_001] {
                    app.selected = Some(selected);
                    ctx.begin_pass(egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(
                            egui::Pos2::ZERO,
                            egui::vec2(1_456.0, 900.0),
                        )),
                        ..Default::default()
                    });
                    app.ui_frame(&ctx);
                    let _ = ctx.end_pass();
                    let width = PanelState::load(&ctx, id).unwrap().size().x;
                    observed.push(width);
                    assert!(
                        (width - preferred_width).abs() < 1.0,
                        "detail panel expanded for selected game {selected}, tab {detail_tab}, requested {preferred_width}: {observed:?}"
                    );
                    assert_eq!(
                        app.config.settings.get("sash_pos").and_then(|v| v.as_str()),
                        Some(expected.as_str()),
                        "content unexpectedly overwrote the saved width"
                    );
                }
            }
        }
    }

    #[test]
    fn detail_panel_still_saves_manual_splitter_drags_and_restores_after_reselection() {
        use egui::containers::panel::PanelState;
        let (mut app, _dir) = seeded_app();
        let ctx = run_app_ticks(&mut app, 2);
        wait_for_rows(&mut app, &ctx);
        app.selected = Some(app.rows.iter().find(|row| row.has_base).unwrap().id);
        app.config
            .settings
            .insert("sash_remember".into(), "1".into());
        app.config.settings.insert("sash_pos".into(), "380".into());

        let panel_id = egui::Id::new("detail");
        let resize_id = panel_id.with("__resize");
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1456.0, 900.0));
        let frame = |app: &mut NslApp, events: Vec<egui::Event>| {
            ctx.begin_pass(egui::RawInput {
                screen_rect: Some(screen),
                events,
                ..Default::default()
            });
            app.ui_frame(&ctx);
            let _ = ctx.end_pass();
        };
        ctx.data_mut(|data| {
            data.insert_persisted(
                panel_id,
                PanelState {
                    rect: egui::Rect::from_min_size(
                        egui::pos2(1076.0, 0.0),
                        egui::vec2(380.0, 900.0),
                    ),
                },
            )
        });
        frame(&mut app, vec![]);
        let start = ctx.read_response(resize_id).unwrap().rect.center();
        let target = start + egui::vec2(-75.0, 0.0);
        let click = |pos, pressed| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        frame(&mut app, vec![egui::Event::PointerMoved(start)]);
        frame(&mut app, vec![click(start, true)]);
        frame(&mut app, vec![egui::Event::PointerMoved(target)]);
        frame(&mut app, vec![egui::Event::PointerMoved(target)]);
        frame(&mut app, vec![click(target, false)]);
        frame(&mut app, vec![]);
        let actual = PanelState::load(&ctx, panel_id).unwrap().size().x;
        assert!(
            (actual - 455.0).abs() < 3.0,
            "real splitter drag did not resize: {actual}"
        );
        let stored = app
            .config
            .settings
            .get("sash_pos")
            .and_then(|v| v.as_str())
            .unwrap()
            .parse::<f32>()
            .unwrap();
        assert!(
            (stored - actual).abs() <= 2.0,
            "drag width not persisted: stored={stored}, actual={actual}"
        );

        app.selected = None;
        frame(&mut app, vec![]);
        app.selected = Some(app.rows.iter().find(|row| row.has_base).unwrap().id);
        frame(&mut app, vec![]);
        let reopened = PanelState::load(&ctx, panel_id).unwrap().size().x;
        assert!(
            (reopened - actual).abs() < 1.0,
            "reopening changed sash width"
        );
        let disk_config = database::load_config(&app.base_dir.join("config.json")).unwrap();
        assert_eq!(
            disk_config.settings.get("sash_pos"),
            app.config.settings.get("sash_pos"),
            "manual resize width was not saved across app restarts"
        );

        app.settings_open = true;
        frame(&mut app, vec![]);
        app.settings_open = false;
        frame(&mut app, vec![]);
        let after_settings = PanelState::load(&ctx, panel_id).unwrap().size().x;
        assert!(
            (after_settings - actual).abs() < 1.0,
            "switching settings view changed the user's panel width"
        );

        // A temporary egui layout constraint can shrink its cached panel
        // rectangle without a splitter drag. The saved user width must win
        // when the layout has room again.
        ctx.data_mut(|data| {
            let mut panel = data.get_persisted::<PanelState>(panel_id).unwrap();
            panel.rect.min.x = panel.rect.max.x - 320.0;
            data.insert_persisted(panel_id, panel);
        });
        frame(&mut app, vec![]);
        let after_constraint = PanelState::load(&ctx, panel_id).unwrap().size().x;
        assert!(
            (after_constraint - actual).abs() < 1.0,
            "layout constraint overwrote saved width: {after_constraint} vs {actual}"
        );
    }

    #[test]
    fn virtualized_scroll_offset_reveals_row_without_jumping_within_viewport() {
        assert_eq!(
            scroll_offset_for_selected_row(0.0, 300.0, 0, 110.0, 100.0),
            None
        );
        assert_eq!(
            scroll_offset_for_selected_row(0.0, 300.0, 1, 110.0, 100.0),
            None
        );
        assert_eq!(
            scroll_offset_for_selected_row(0.0, 300.0, 3, 110.0, 100.0),
            Some(134.0)
        );
        assert_eq!(
            scroll_offset_for_selected_row(200.0, 300.0, 3, 110.0, 100.0),
            None
        );
        assert_eq!(
            scroll_offset_for_selected_row(350.0, 300.0, 1, 110.0, 100.0),
            Some(106.0)
        );
        assert_eq!(
            scroll_offset_for_selected_row(0.0, 100.0, 99, 110.0, 100.0),
            Some(10_890.0)
        );
        assert_eq!(
            scroll_offset_for_selected_row(0.0, 0.0, 3, 110.0, 100.0),
            None
        );
    }

    #[test]
    fn keyboard_navigation_page_boundaries_and_unselected_rows() {
        assert_eq!(
            library_navigation_target(Some(0), 350, 0, 200, Some(-4), false, false),
            Some(0)
        );
        assert_eq!(
            library_navigation_target(Some(349), 350, 200, 350, Some(4), false, false),
            Some(349)
        );
        assert_eq!(
            library_navigation_target(Some(5), 350, 0, 200, Some(12), false, false),
            Some(17)
        );
        assert_eq!(
            library_navigation_target(Some(205), 350, 0, 200, Some(1), false, false),
            Some(0)
        );
        assert_eq!(
            library_navigation_target(Some(205), 350, 0, 200, Some(-1), false, false),
            Some(199)
        );
        assert_eq!(
            library_navigation_target(None, 350, 200, 350, Some(1), false, false),
            Some(200)
        );
        assert_eq!(
            library_navigation_target(Some(250), 350, 200, 350, None, true, false),
            Some(0)
        );
        assert_eq!(
            library_navigation_target(Some(0), 350, 0, 200, None, false, true),
            Some(349)
        );
        assert_eq!(
            library_navigation_target(None, 0, 0, 0, Some(1), false, false),
            None
        );
    }

    #[test]
    fn keyboard_navigation_keeps_selection_in_virtualized_grid_and_list() {
        fn key_frame(
            app: &mut NslApp,
            ctx: &egui::Context,
            key: egui::Key,
            modifiers: egui::Modifiers,
        ) {
            for pressed in [true, false] {
                ctx.begin_pass(egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(980.0, 690.0),
                    )),
                    events: vec![egui::Event::Key {
                        key,
                        physical_key: None,
                        pressed,
                        repeat: false,
                        modifiers,
                    }],
                    ..Default::default()
                });
                app.ui_frame(ctx);
                let _ = ctx.end_pass();
            }
        }

        fn assert_visible(app: &mut NslApp, ctx: &egui::Context) -> usize {
            let matching = app.library_rows_cached();
            let index = matching
                .iter()
                .position(|row| app.selected == Some(row.id))
                .unwrap();
            let start = if app
                .config
                .settings
                .get("paging_on")
                .and_then(|v| v.as_str())
                == Some("0")
            {
                0
            } else {
                app.current_page * 200
            };
            assert!(index >= start, "selected item is on a different page");
            if app.view_mode == "grid" {
                let (visible, columns) = ctx.data_mut(|data| {
                    (
                        data.get_temp::<std::ops::Range<usize>>(egui::Id::new(
                            "nsl_test_visible_grid_rows",
                        ))
                        .unwrap(),
                        data.get_temp::<usize>(egui::Id::new("nsl_test_grid_columns"))
                            .unwrap(),
                    )
                });
                let selected_grid_row = (index - start) / columns;
                assert!(visible.contains(&selected_grid_row),
                    "grid selection {index} (row {selected_grid_row}, cols {columns}) not rendered in {visible:?}");
                let (card, viewport) = ctx.data_mut(|data| {
                    (
                        data.get_temp::<egui::Rect>(egui::Id::new("nsl_test_selected_grid_rect"))
                            .unwrap(),
                        data.get_temp::<egui::Rect>(egui::Id::new("nsl_test_grid_viewport_rect"))
                            .unwrap(),
                    )
                });
                assert!(
                    card.top() >= viewport.top() - 2.0 && card.bottom() <= viewport.bottom() + 2.0,
                    "selected grid card {index} is clipped: card={card:?}, viewport={viewport:?}"
                );
            } else {
                let visible = ctx.data_mut(|data| {
                    data.get_temp::<std::ops::Range<usize>>(egui::Id::new(
                        "nsl_test_visible_list_rows",
                    ))
                    .unwrap()
                });
                assert!(
                    visible.contains(&(index - start)),
                    "list selection {index} not rendered in {visible:?}"
                );
                let (cell, viewport) = ctx.data_mut(|data| {
                    (
                        data.get_temp::<egui::Rect>(egui::Id::new("nsl_test_selected_list_rect"))
                            .unwrap(),
                        data.get_temp::<egui::Rect>(egui::Id::new("nsl_test_list_viewport_rect"))
                            .unwrap(),
                    )
                });
                assert!(
                    cell.top() >= viewport.top() - 2.0 && cell.bottom() <= viewport.bottom() + 2.0,
                    "selected list row {index} is clipped: cell={cell:?}, viewport={viewport:?}"
                );
            }
            index
        }

        let (mut app, _dir) = seeded_app();
        let ctx = run_app_ticks(&mut app, 2);
        wait_for_rows(&mut app, &ctx);
        let template = app.rows.iter().find(|row| row.has_base).unwrap().clone();
        app.rows = (0..350)
            .map(|n| {
                let mut row = template.clone();
                row.id = 10_000 + n;
                row.title_name = format!("Synthetic Game {n:04}");
                row.title_id = format!("0100{n:012X}");
                row.path = format!("E:\\NSW\\synthetic_{n:04}.nsp");
                row
            })
            .collect();
        app.rows_generation = app.rows_generation.wrapping_add(1);
        app.folder_filter.clear();
        app.kind_filter = "base".into();
        app.view_mode = "grid".into();
        app.selected = None;
        app.current_page = 0;
        app.config.settings.insert("paging_on".into(), "1".into());
        app.config.settings.insert("page_size".into(), "200".into());
        assert_eq!(app.library_rows_cached().len(), 350);

        // End jumps across the page boundary. The second frame reflows the
        // grid after the detail panel opens; the selection must remain visible.
        key_frame(&mut app, &ctx, egui::Key::End, egui::Modifiers::NONE);
        assert_eq!(assert_visible(&mut app, &ctx), 349);
        assert_eq!(app.current_page, 1);
        key_frame(&mut app, &ctx, egui::Key::Home, egui::Modifiers::CTRL);
        assert_eq!(assert_visible(&mut app, &ctx), 0);
        assert_eq!(app.current_page, 0);
        key_frame(&mut app, &ctx, egui::Key::PageDown, egui::Modifiers::NONE);
        let first_page_down = assert_visible(&mut app, &ctx);
        assert!(first_page_down > 0 && first_page_down < 100);
        for _ in 0..35 {
            key_frame(&mut app, &ctx, egui::Key::ArrowDown, egui::Modifiers::NONE);
            assert_visible(&mut app, &ctx);
        }
        let later = assert_visible(&mut app, &ctx);
        key_frame(&mut app, &ctx, egui::Key::PageUp, egui::Modifiers::NONE);
        assert!(assert_visible(&mut app, &ctx) < later);
        key_frame(&mut app, &ctx, egui::Key::End, egui::Modifiers::CTRL);
        assert_eq!(assert_visible(&mut app, &ctx), 349);
        key_frame(&mut app, &ctx, egui::Key::ArrowDown, egui::Modifiers::NONE);
        assert_eq!(assert_visible(&mut app, &ctx), 349);
        key_frame(&mut app, &ctx, egui::Key::ArrowLeft, egui::Modifiers::NONE);
        assert_eq!(assert_visible(&mut app, &ctx), 348);
        key_frame(&mut app, &ctx, egui::Key::ArrowRight, egui::Modifiers::NONE);
        assert_eq!(assert_visible(&mut app, &ctx), 349);

        // A single 350-item virtual grid must work without the optional
        // 200-item paginator as well.
        app.config.settings.insert("paging_on".into(), "0".into());
        key_frame(&mut app, &ctx, egui::Key::Home, egui::Modifiers::NONE);
        assert_eq!(assert_visible(&mut app, &ctx), 0);
        key_frame(&mut app, &ctx, egui::Key::End, egui::Modifiers::NONE);
        assert_eq!(assert_visible(&mut app, &ctx), 349);

        // List mode must also reveal the last row, then PageUp and Home.
        app.view_mode = "list".into();
        key_frame(&mut app, &ctx, egui::Key::End, egui::Modifiers::NONE);
        assert_eq!(assert_visible(&mut app, &ctx), 349);
        key_frame(&mut app, &ctx, egui::Key::PageUp, egui::Modifiers::NONE);
        assert!(assert_visible(&mut app, &ctx) < 349);
        key_frame(&mut app, &ctx, egui::Key::Home, egui::Modifiers::NONE);
        assert_eq!(assert_visible(&mut app, &ctx), 0);

        // With paging disabled, End must reach the physical last item in one
        // enormous virtual list instead of stopping at item 199.
        key_frame(&mut app, &ctx, egui::Key::End, egui::Modifiers::NONE);
        assert_eq!(assert_visible(&mut app, &ctx), 349);
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
        assert!(
            app.titledb_result.is_none(),
            "title database import stalled"
        );
        assert_eq!(app.db.stats().unwrap().title_db_count, 1);

        // A second construction must not re-import an already populated table.
        let restarted = NslApp::with_base_dir(base).unwrap();
        assert!(
            restarted.titledb_result.is_none(),
            "populated title table must not be re-imported on every launch"
        );
    }

    #[test]
    fn long_directory_tab_names_elide_at_character_boundaries_and_keep_counts() {
        let measure = |value: &str| value.chars().count() as f32 * 10.0;
        let label = "매우길고긴다운로드폴더이름 (3,137)";
        assert_eq!(elide_tab_label(label, 500.0, measure), label);
        let fitted = elide_tab_label(label, 120.0, measure);
        assert!(
            fitted.contains('…') && fitted.ends_with(" (3,137)"),
            "{fitted}"
        );
        assert!(measure(&fitted) <= 120.0, "fitted text overflows: {fitted}");
        assert_eq!(elide_tab_label("스토어", 25.0, measure), "스…");
        assert_eq!(elide_tab_label("스토어", 10.0, measure), "…");
        assert!(measure(&elide_tab_label(label, 35.0, measure)) <= 35.0);
    }

    #[test]
    fn directory_tab_widths_shrink_only_when_needed() {
        // 5 user directories plus All, Missing Updates, Missing DLC, Store,
        // Downloads = a maximum of 10 tab buttons in the usual configuration.
        let preferred = [
            100.0, 170.0, 320.0, 220.0, 190.0, 240.0, 180.0, 150.0, 90.0, 110.0,
        ];
        let full_width: f32 = preferred.iter().sum();
        assert_eq!(
            fit_directory_tab_widths(&preferred, full_width + 10.0),
            preferred
        );
        for available in [1800.0, 1050.0, 820.0, 620.0, 490.0] {
            let fitted = fit_directory_tab_widths(&preferred, available);
            assert_eq!(fitted.len(), preferred.len());
            assert!(fitted.iter().all(|w| *w >= 44.0 && w.is_finite()));
            assert!(fitted
                .iter()
                .zip(preferred)
                .all(|(actual, original)| *actual <= original));
            assert!(
                fitted.iter().sum::<f32>() <= available + 0.05,
                "tabs overflow available width {available}: {fitted:?}"
            );
        }
        let fallback = fit_directory_tab_widths(&preferred, 240.0);
        assert_eq!(
            fallback,
            vec![44.0; preferred.len()],
            "unavoidably narrow windows should retain scrollable tab buttons"
        );
        assert!(fit_directory_tab_widths(&[], 1200.0).is_empty());
    }

    #[test]
    fn five_long_directory_tabs_fit_screen_and_remain_clickable() {
        let (mut app, _dir) = seeded_app();
        let ctx = run_app_ticks(&mut app, 2);
        wait_for_rows(&mut app, &ctx);
        app.selected = None;
        app.folders = (0..5)
            .map(|index| format!(
                "E:\\NSW\\This is an extremely long directory name to fit {index} — 가나다라마바사게임 목록"
            ))
            .collect();
        app.rows_generation = app.rows_generation.wrapping_add(1);
        app.cached_tabs_generation = u64::MAX;
        for width in [1456.0, 1100.0, 900.0, 760.0, 600.0] {
            ctx.begin_pass(egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(width, 850.0),
                )),
                ..Default::default()
            });
            app.ui_frame(&ctx);
            let _ = ctx.end_pass();
            let (available, widths, rects) = ctx.data_mut(|data| {
                data.get_temp::<(f32, Vec<f32>, Vec<egui::Rect>)>(egui::Id::new(
                    "nsl_test_directory_tab_layout",
                ))
                .unwrap()
            });
            assert_eq!(rects.len(), 10, "five folders must produce ten total tabs");
            assert_eq!(widths.len(), rects.len());
            assert!(rects.iter().all(|rect| rect.is_finite()));
            for (rect, fitted) in rects.iter().zip(widths.iter()) {
                assert!(
                    (rect.width() - fitted).abs() < 0.5,
                    "tab width differs from computed allocation: {rect:?} {fitted}"
                );
            }
            if available >= (10.0 * 44.0 + 10.0) {
                assert!(
                    rects.last().unwrap().right() <= width + 1.0,
                    "last tab hidden at window width {width}: {rects:?}"
                );
                assert!(
                    rects.last().unwrap().right() - rects[0].left() <= available + 1.0,
                    "tabs overflow available area {available}: {rects:?}"
                );
            }
        }

        // Clicking an elided tab must still select its full original key.
        ctx.begin_pass(egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1456.0, 850.0),
            )),
            ..Default::default()
        });
        app.ui_frame(&ctx);
        let _ = ctx.end_pass();
        let (_, _, rects) = ctx.data_mut(|data| {
            data.get_temp::<(f32, Vec<f32>, Vec<egui::Rect>)>(egui::Id::new(
                "nsl_test_directory_tab_layout",
            ))
            .unwrap()
        });
        let folder_path = app.folders[4].clone();
        let center = rects[5].center();
        for pressed in [true, false] {
            ctx.begin_pass(egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1456.0, 850.0),
                )),
                events: vec![
                    egui::Event::PointerMoved(center),
                    egui::Event::PointerButton {
                        pos: center,
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
                ..Default::default()
            });
            app.ui_frame(&ctx);
            let _ = ctx.end_pass();
        }
        assert_eq!(
            app.folder_filter, folder_path,
            "truncated tab selected wrong directory"
        );
        assert_eq!(
            app.config
                .settings
                .get("last_tab_key")
                .and_then(|s| s.as_str()),
            Some(folder_path.as_str()),
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
        // Profile the bounded worker count without touching persistent user
        // settings. Set NSL_TEST_SCAN_THREADS only for this ignored QA test.
        if let Ok(threads) = std::env::var("NSL_TEST_SCAN_THREADS") {
            app.config.settings.insert(
                "max_scan_threads".into(),
                serde_json::Value::String(threads),
            );
        }
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
            if app.scan_result.is_none()
                && app.reload_result.is_none()
                && app.scan_progress.is_none()
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
        assert_eq!(
            failures, 0,
            "no file may fail analysis; status: {}",
            app.status
        );
        assert!(
            count > 700,
            "expected the full library, scanned only {count} rows"
        );
        let reference_path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("library.db");
        let reference = rusqlite::Connection::open_with_flags(
            reference_path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        let reference_counts: (i64, i64, i64) = reference
            .query_row(
                "SELECT \
                 (SELECT COUNT(*) FROM file_contents fc JOIN game_files gf ON gf.path=fc.path WHERE gf.scan_folder=?1 AND fc.content_type='base'), \
                 (SELECT COUNT(*) FROM file_contents fc JOIN game_files gf ON gf.path=fc.path WHERE gf.scan_folder=?1 AND fc.content_type='update'), \
                 (SELECT COUNT(*) FROM file_contents fc JOIN game_files gf ON gf.path=fc.path WHERE gf.scan_folder=?1 AND fc.content_type='dlc')",
                [app.folders[0].as_str()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(
            (stats.base_count, stats.update_count, stats.dlc_count),
            reference_counts,
            "scan content totals must match the current reference library"
        );

        // Every discovered file must carry a fast hash so the next pass can reuse it.
        let cached = app.db.cached_file_records().unwrap();
        assert_eq!(cached.len(), count);
        assert!(cached.values().all(|record| !record.fast_hash.is_empty()));
        assert!(cached.values().all(|record| !record.contents.is_empty()));
        // Compare the same scanned root against its read-only existing rows;
        // the complete library may include unrelated download/desktop roots.
        let mut file_stmt = reference.prepare(
            "SELECT path,COALESCE(nacp_ko,''),COALESCE(nacp_en,''),COALESCE(nacp_ja,'') FROM game_files WHERE scan_folder=?1"
        ).unwrap();
        let mut compared_files = 0usize;
        let mut reference_name_differences = 0usize;
        for row in file_stmt
            .query_map([app.folders[0].as_str()], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                ))
            })
            .unwrap()
        {
            let (path, ko, en, ja) = row.unwrap();
            let normalized = normalize_scan_path_text(&path);
            let new = cached
                .get(&normalized)
                .unwrap_or_else(|| panic!("missing reference path"));
            if (
                new.nacp_ko.as_str(),
                new.nacp_en.as_str(),
                new.nacp_ja.as_str(),
            ) != (ko.as_str(), en.as_str(), ja.as_str())
            {
                reference_name_differences += 1;
            }
            compared_files += 1;
        }
        let reference_files: i64 = reference
            .query_row(
                "SELECT COUNT(*) FROM game_files WHERE scan_folder=?1",
                [app.folders[0].as_str()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(compared_files as i64, reference_files);
        let mut content_stmt = reference
            .prepare("SELECT fc.path,fc.title_id,fc.version,fc.content_type FROM file_contents fc JOIN game_files gf ON gf.path=fc.path WHERE gf.scan_folder=?1")
            .unwrap();
        let mut reference_contents: HashMap<String, HashSet<(Option<String>, i64, String)>> =
            HashMap::new();
        for row in content_stmt
            .query_map([app.folders[0].as_str()], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, String>(3)?,
                ))
            })
            .unwrap()
        {
            let (path, id, version, kind) = row.unwrap();
            reference_contents
                .entry(normalize_scan_path_text(&path))
                .or_default()
                .insert((id, version, kind));
        }
        let mut compared_contents = 0usize;
        let mut reference_content_differences = 0usize;
        for (path, expected) in &reference_contents {
            let actual: HashSet<_> = cached
                .get(path)
                .unwrap()
                .contents
                .iter()
                .map(|entry| {
                    (
                        entry.title_id.clone(),
                        entry.version,
                        entry.content_type.clone(),
                    )
                })
                .collect();
            if &actual != expected {
                reference_content_differences += 1;
            }
            compared_contents += actual.len();
        }
        let reference_content_count: i64 = reference.query_row(
            "SELECT COUNT(*) FROM file_contents fc JOIN game_files gf ON gf.path=fc.path WHERE gf.scan_folder=?1",
            [app.folders[0].as_str()], |row| row.get(0)
        ).unwrap();
        assert_eq!(compared_contents as i64, reference_content_count);
        eprintln!("SCAN_REFERENCE_COMPARISON files={compared_files} content_entries={compared_contents} old_names_different={reference_name_differences} old_contents_different={reference_content_differences}");
        let localized = cached
            .values()
            .filter(|record| {
                !record.nacp_ko.is_empty()
                    || !record.nacp_en.is_empty()
                    || !record.nacp_ja.is_empty()
            })
            .count();
        let korean = cached
            .values()
            .filter(|record| !record.nacp_ko.is_empty())
            .count();
        assert!(localized >= 800, "NACP localization was lost: {localized}");
        assert!(korean >= 530, "Korean names were lost: {korean}");
        eprintln!("SCAN_LOCALES available={localized} korean={korean}");
        eprintln!("SCAN_PROFILE {}", scanner::scan_profile::report());
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

        assert!(full_stats.base_count > 0);
        assert_eq!(incremental_stats.base_count, full_stats.base_count);
        assert_eq!(incremental_stats.update_count, full_stats.update_count);
        assert_eq!(incremental_stats.dlc_count, full_stats.dlc_count);
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
            if app.scan_result.is_none()
                && app.reload_result.is_none()
                && app.scan_progress.is_none()
            {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        false
    }
}
