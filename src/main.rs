#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod catalog;
mod common;
mod container_index;
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
mod torrent_file_picker;
mod translate;
mod update_batch;
mod update_merge;
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

struct CatalogTorrentDialog {
    item: catalog::CatalogItem,
    request: Option<torrent::TorrentPreviewRequest>,
    files: Option<torrent_file_picker::TorrentFilePicker>,
    error: Option<String>,
}

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

/// Register a card's hit target before its embedded action buttons so egui
/// routes clicks to child controls instead of the background. Eguis frame
/// layout may shift the next_widget_position() by its margins, so remember
/// the measured bounds to keep the hit region equal to the painted card.
fn selectable_card_background(
    ui: &egui::Ui,
    id: egui::Id,
    width: f32,
    default_height: f32,
) -> egui::Response {
    let (size, offset) = ui.ctx().data_mut(|data| {
        (
            data.get_temp::<egui::Vec2>(id.with("size"))
                .unwrap_or(egui::vec2(width, default_height)),
            data.get_temp::<egui::Vec2>(id.with("offset"))
                .unwrap_or(egui::Vec2::ZERO),
        )
    });
    ui.interact(
        egui::Rect::from_min_size(
            ui.next_widget_position() + offset,
            egui::vec2(width, size.y),
        ),
        id,
        egui::Sense::click(),
    )
}

fn remember_card_geometry(
    ctx: &egui::Context,
    id: egui::Id,
    hit_rect: egui::Rect,
    painted_rect: egui::Rect,
) {
    ctx.data_mut(|data| {
        let previous = data
            .get_temp::<egui::Vec2>(id.with("offset"))
            .unwrap_or(egui::Vec2::ZERO);
        data.insert_temp(
            id.with("offset"),
            previous + (painted_rect.min - hit_rect.min),
        );
        data.insert_temp(id.with("size"), painted_rect.size());
    });
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

#[derive(Clone, Copy, Debug)]
struct ManagementNavigation {
    step: Option<isize>,
    home: bool,
    end: bool,
    enter: bool,
}

/// Both Store and Downloads are vertical lists: either arrow changes one
/// selection, Page Up/Down follows the visible viewport, Home/End span pages.
/// This only reads raw input; callers reject focused text editors and dialogs.
fn management_navigation_input(
    ctx: &egui::Context,
    enabled: bool,
    visible_rows: usize,
) -> Option<ManagementNavigation> {
    if !enabled {
        return None;
    }
    ctx.input(|input| {
        let plain = !input.modifiers.ctrl && !input.modifiers.alt && !input.modifiers.shift;
        let jump = !input.modifiers.alt && !input.modifiers.shift;
        let step = if plain && input.key_pressed(egui::Key::PageUp) {
            Some(-(visible_rows.max(1) as isize))
        } else if plain && input.key_pressed(egui::Key::PageDown) {
            Some(visible_rows.max(1) as isize)
        } else if plain
            && (input.key_pressed(egui::Key::ArrowUp) || input.key_pressed(egui::Key::ArrowLeft))
        {
            Some(-1)
        } else if plain
            && (input.key_pressed(egui::Key::ArrowDown) || input.key_pressed(egui::Key::ArrowRight))
        {
            Some(1)
        } else {
            None
        };
        let home = jump && input.key_pressed(egui::Key::Home);
        let end = jump && input.key_pressed(egui::Key::End);
        let enter = plain && input.key_pressed(egui::Key::Enter);
        (step.is_some() || home || end || enter).then_some(ManagementNavigation {
            step,
            home,
            end,
            enter,
        })
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
    if selected {
        // Match the theme's visible selection/link accent, not its action
        // button fill (green on GitHub Dark). The selection accent is blue
        // (#1F6FEB) on GitHub Dark and follows Palette.accent2 elsewhere.
        // Keep the 2-point strip inside the existing tab/click rectangle.
        let accent_bar = egui::Rect::from_min_size(rect.min, egui::vec2(rect.width(), 2.0));
        let accent_color = visuals.hyperlink_color;
        painter.rect_filled(accent_bar, 0.0, accent_color);
        #[cfg(test)]
        ui.ctx().data_mut(|data| {
            data.insert_temp(
                egui::Id::new("nsl_test_active_tab_top_bar"),
                (accent_bar, accent_color),
            );
        });
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
    update_check_automatic: bool,
    update_info: Option<updater::ReleaseInfo>,
    update_prepare_result: Option<Receiver<std::result::Result<PathBuf, String>>>,
    update_ready: Option<PathBuf>,
    catalog_state: catalog::CatalogState,
    catalog_load_result: Option<Receiver<Option<catalog::CatalogState>>>,
    catalog_result: Option<Receiver<std::result::Result<usize, String>>>,
    catalog_search: String,
    catalog_search_committed: String,
    catalog_search_cache: Option<String>,
    catalog_user_filter_cache: Option<String>,
    catalog_matching_indices: Vec<usize>,
    catalog_search_pending: Option<std::time::Instant>,
    catalog_page: usize,
    catalog_genre_filter: String,
    catalog_format_filter: String,
    catalog_language_filter: String,
    catalog_scroll_reset: bool,
    catalog_selected: Option<String>,
    catalog_owned: HashMap<String, CatalogOwnedSummary>,
    catalog_title_details: HashMap<String, database::CatalogTitleDetails>,
    catalog_title_load_result: Option<
        Receiver<std::result::Result<HashMap<String, database::CatalogTitleDetails>, String>>,
    >,
    catalog_title_metadata_loaded: bool,
    catalog_cover_dir: PathBuf,
    catalog_cover_textures: HashMap<String, egui::TextureHandle>,
    catalog_cover_lru: VecDeque<String>,
    catalog_cover_missing: HashSet<String>,
    catalog_cover_retry_after: HashMap<String, std::time::Instant>,
    catalog_cover_queue: Option<Receiver<(usize, Vec<String>)>>,
    torrent_engine: Option<Arc<torrent::TorrentEngine>>,
    torrent_progress: Vec<(String, torrent::TorrentProgress)>,
    catalog_torrent_dialog: Option<CatalogTorrentDialog>,
    /// Metadata-only bulk scan and independently verified selective updates.
    update_batch: Option<update_batch::UpdateBatch>,
    download_selected: Option<String>,
    download_search: String,
    download_status_filter: String,
    download_page: usize,
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

fn download_connection_label(progress: &torrent::TorrentProgress) -> String {
    match (progress.active_seeders, progress.active_peers) {
        (Some(seeders), Some(peers)) => format!("시드 {seeders} · 피어 {peers}"),
        _ => "시드 — · 피어 —".into(),
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
const STORE_LANGUAGES: [(&str, &str); 16] = [
    ("all", "전체 언어"),
    ("ko", "한국어"),
    ("en", "영어"),
    ("ja", "일본어"),
    ("zh", "중국어"),
    ("ru", "러시아어"),
    ("es", "스페인어"),
    ("fr", "프랑스어"),
    ("de", "독일어"),
    ("it", "이탈리아어"),
    ("pt", "포르투갈어"),
    ("pl", "폴란드어"),
    ("nl", "네덜란드어"),
    ("tr", "튀르키예어"),
    ("uk", "우크라이나어"),
    ("none", "텍스트 없음"),
];

/// Match only languages explicitly listed by the source. A vague "Multi 10"
/// marker must not be interpreted as Korean (or any other language).
fn catalog_matches_language(source: &str, language: &str) -> bool {
    if language == "all" {
        return true;
    }
    if language == "none" {
        return source.eq_ignore_ascii_case("No text");
    }
    let names: &[&str] = match language {
        "ko" => &["korean", "kor", "ko"],
        "en" => &["english", "eng", "en"],
        "ja" => &["japanese", "jap", "jpn", "ja"],
        "zh" => &["chinese", "chn", "chi", "chs", "cht", "zh"],
        "ru" => &["russian", "rus", "ru"],
        "es" => &["spanish", "esp", "spa", "es"],
        "fr" => &["french", "fra", "fre", "fr"],
        "de" => &["german", "ger", "deu", "de"],
        "it" => &["italian", "ita", "it"],
        "pt" => &["portuguese", "por", "pt"],
        "pl" => &["polish", "pol", "pl"],
        "nl" => &["dutch", "nld", "dut", "nl"],
        "tr" => &["turkish", "tur", "tr"],
        "uk" => &["ukrainian", "ukr", "uk"],
        _ => return false,
    };
    source
        .split(|ch: char| !ch.is_ascii_alphabetic())
        .any(|word| names.iter().any(|name| word.eq_ignore_ascii_case(name)))
}

fn catalog_item_matches_language(
    item: &catalog::CatalogItem,
    metadata: Option<&database::CatalogTitleDetails>,
    language: &str,
) -> bool {
    catalog_matches_language(&item.interface_lang, language)
        || metadata.is_some_and(|details| catalog_matches_language(&details.languages, language))
}

#[derive(Clone, Debug, Default)]
struct CatalogOwnedSummary {
    display_name: String,
    has_base: bool,
    has_update: bool,
    update_version: i64,
    dlc_count: usize,
    file_count: usize,
}

fn catalog_base_id(id: &str) -> Option<String> {
    let parsed = u64::from_str_radix(id, 16).ok()?;
    (id.len() == 16).then(|| format!("{:016X}", parsed & !0x1FFF))
}

fn catalog_owned_summaries(rows: &[library::GameFile]) -> HashMap<String, CatalogOwnedSummary> {
    let mut owned = HashMap::<String, CatalogOwnedSummary>::new();
    for row in rows {
        let Some(base) = catalog_base_id(&row.title_id) else {
            continue;
        };
        let entry = owned.entry(base).or_default();
        if entry.display_name.is_empty() || (row.has_base && !entry.has_base) {
            entry.display_name.clone_from(&row.title_name);
        }
        entry.has_base |= row.has_base;
        entry.has_update |= row.has_update;
        entry.update_version = entry.update_version.max(row.update_version);
        entry.dlc_count = entry.dlc_count.max(row.dlc_count);
        entry.file_count += 1;
    }
    owned
}

fn catalog_preferred_title<'a>(
    item: &'a catalog::CatalogItem,
    metadata: Option<&'a database::CatalogTitleDetails>,
    owned: Option<&'a CatalogOwnedSummary>,
) -> &'a str {
    // Database::files uses the exact Settings > name_mode priority, including
    // Korean/English/Japanese NACP names for owned games.
    if let Some(owned) = owned {
        if !owned.display_name.is_empty() {
            return &owned.display_name;
        }
    }
    metadata
        .map(|details| details.name.as_str())
        .filter(|name| !name.is_empty())
        .unwrap_or(&item.title)
}

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

/// Use the same configured page size for the library and the Store, even
/// when library paging is disabled. Downloads retain their separate limit.
fn configured_game_page_size(settings: &serde_json::Map<String, serde_json::Value>) -> usize {
    settings
        .get("page_size")
        .and_then(|value| value.as_str())
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(500)
        .min(99_999)
}

/// Use the library's compact previous / page / next layout in both management
/// lists. Navigation belongs above the rows, never inside the right sidebar.
const MANAGEMENT_PAGE_SIZE: usize = 100;

fn management_pagination_ui(
    ui: &mut egui::Ui,
    current_page: &mut usize,
    total_items: usize,
    page_size: usize,
) -> (egui::Rect, egui::Rect, bool) {
    let total_pages = total_items.div_ceil(page_size.max(1)).max(1);
    let previous = *current_page;
    *current_page = (*current_page).min(total_pages - 1);
    let mut next_button_rect = egui::Rect::NOTHING;
    let layout = ui.horizontal(|ui| {
        if ui
            .add_enabled(*current_page > 0, egui::Button::new("이전"))
            .clicked()
        {
            *current_page -= 1;
        }
        ui.label(format!(
            "페이지 {} / {} · {}개 항목",
            *current_page + 1,
            total_pages,
            common::format_count(total_items as i64)
        ));
        let next = ui.add_enabled(*current_page + 1 < total_pages, egui::Button::new("다음"));
        next_button_rect = next.rect;
        if next.clicked() {
            *current_page += 1;
        }
    });
    (
        layout.response.rect,
        next_button_rect,
        *current_page != previous,
    )
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

impl eframe::App for NslApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.ui_frame(ctx);
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

fn management_sidebar_frame(style: &egui::Style) -> egui::Frame {
    // Both management tabs share the same comfortable content inset. Using
    // the SidePanel frame keeps the scroll area inside the padded viewport.
    egui::Frame::side_top_panel(style).inner_margin(egui::Margin::symmetric(14.0, 12.0))
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
                            app.start_update_check(false);
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
mod runtime_tests;
