//! A real directory tree for selective torrent downloads.
//! The tree keeps librqbit's original file indices even when folders are
//! expanded, collapsed or sorted. Checking a folder updates every descendant.
use std::{
    collections::{BTreeMap, HashSet},
    path::Component,
};

use eframe::egui;

use crate::{common, torrent::TorrentPreview};

// Match the familiar Explorer tree: a clearly boxed +/- control, 30px per
// directory level, and visible vertical/horizontal branch guides.
const TREE_LEVEL_INDENT: f32 = 30.0;
const TREE_ROW_HEIGHT: f32 = 28.0;
const TREE_TOGGLE_SIZE: f32 = 19.0;
const TREE_SIZE_COLUMN: f32 = 100.0;

#[derive(Debug, Default)]
struct FileNode {
    name: String,
    key: String,
    children: BTreeMap<String, FileNode>,
    file_slot: Option<usize>,
    descendants: Vec<usize>,
    bytes: u64,
}

/// Flattened only for the current expansion state. ScrollArea::show_rows
/// renders just the rows inside the viewport, even for large torrents.
struct VisibleNode<'a> {
    node: &'a FileNode,
    depth: usize,
    // File paths are restricted to at most 64 parts. One bit per tree level
    // avoids a heap allocation for every row in a large torrent.
    ancestor_continues: u64,
    is_last: bool,
}

impl FileNode {
    fn insert(&mut self, parts: &[String], slot: usize, bytes: u64) -> anyhow::Result<()> {
        anyhow::ensure!(!parts.is_empty(), "비어 있는 토렌트 파일 경로");
        self.descendants.push(slot);
        self.bytes = self.bytes.saturating_add(bytes);
        let name = &parts[0];
        let key = if self.key.is_empty() {
            name.clone()
        } else {
            format!("{}/{}", self.key, name)
        };
        let node = self
            .children
            .entry(name.clone())
            .or_insert_with(|| FileNode {
                name: name.clone(),
                key,
                ..Default::default()
            });
        if parts.len() == 1 {
            anyhow::ensure!(
                node.file_slot.is_none() && node.children.is_empty(),
                "중복되거나 디렉터리와 충돌하는 토렌트 경로"
            );
            node.file_slot = Some(slot);
            node.descendants.push(slot);
            node.bytes = bytes;
        } else {
            anyhow::ensure!(
                node.file_slot.is_none(),
                "파일과 하위 디렉터리가 같은 이름입니다"
            );
            node.insert(&parts[1..], slot, bytes)?;
        }
        Ok(())
    }

    fn selected_count(&self, selected: &[bool]) -> usize {
        self.descendants.iter().filter(|&&i| selected[i]).count()
    }

    fn set_selected(&self, selected: &mut [bool], enabled: bool) {
        for &slot in &self.descendants {
            selected[slot] = enabled;
        }
    }

    fn collect_visible<'a>(
        &'a self,
        expanded: &HashSet<String>,
        depth: usize,
        ancestor_continues: u64,
        out: &mut Vec<VisibleNode<'a>>,
    ) {
        // Explorer groups folders before files, without reordering the
        // underlying torrent IDs used for the actual selective transfer.
        let mut children: Vec<_> = self.children.values().collect();
        children.sort_by_cached_key(|node| (node.file_slot.is_some(), node.name.to_lowercase()));
        let count = children.len();
        for (index, child) in children.into_iter().enumerate() {
            let is_last = index + 1 == count;
            out.push(VisibleNode {
                node: child,
                depth,
                ancestor_continues,
                is_last,
            });
            if !child.children.is_empty() && expanded.contains(&child.key) {
                let next_guides = if is_last {
                    ancestor_continues
                } else {
                    ancestor_continues | (1u64 << depth)
                };
                child.collect_visible(expanded, depth + 1, next_guides, out);
            }
        }
    }
}

pub struct TorrentFilePicker {
    pub preview: TorrentPreview,
    tree: FileNode,
    selected: Vec<bool>,
    expanded: HashSet<String>,
}

impl TorrentFilePicker {
    pub fn new(preview: TorrentPreview) -> anyhow::Result<Self> {
        anyhow::ensure!(!preview.files.is_empty(), "토렌트 파일이 없습니다");
        let mut tree = FileNode::default();
        let mut expanded = HashSet::new();
        let mut ids = HashSet::new();
        for (slot, file) in preview.files.iter().enumerate() {
            anyhow::ensure!(ids.insert(file.index), "토렌트 파일 번호가 중복되었습니다");
            let parts = file
                .path
                .components()
                .map(|c| match c {
                    Component::Normal(name) => Some(name.to_string_lossy().into_owned()),
                    _ => None,
                })
                .collect::<Option<Vec<_>>>()
                .ok_or_else(|| anyhow::anyhow!("안전하지 않은 파일 경로"))?;
            anyhow::ensure!(
                !parts.is_empty() && parts.len() <= 64,
                "토렌트 폴더 깊이가 잘못되었습니다"
            );
            tree.insert(&parts, slot, file.size)?;
            // Open top-level directories by default so the user immediately
            // sees their first level of nested folders and files.
            if parts.len() >= 2 {
                expanded.insert(parts[0].clone());
            }
        }
        let selected = vec![true; preview.files.len()];
        Ok(Self {
            preview,
            tree,
            selected,
            expanded,
        })
    }

    pub fn total_files(&self) -> usize {
        self.selected.len()
    }

    #[cfg(test)]
    pub(crate) fn is_expanded(&self, path: &str) -> bool {
        self.expanded.contains(path)
    }

    pub fn selected_count(&self) -> usize {
        self.selected.iter().filter(|&&enabled| enabled).count()
    }

    pub fn selected_bytes(&self) -> u64 {
        self.preview
            .files
            .iter()
            .zip(self.selected.iter())
            .filter(|(_, &checked)| checked)
            .map(|(file, _)| file.size)
            .sum()
    }

    pub fn all_selected(&mut self, selected: bool) {
        self.selected.fill(selected);
    }

    /// The original torrent indices, never the tree's alphabetic positions.
    pub fn selected_ids(&self) -> Vec<usize> {
        self.preview
            .files
            .iter()
            .zip(self.selected.iter())
            .filter_map(|(file, &checked)| checked.then_some(file.index))
            .collect()
    }

    pub fn into_transfer(self) -> anyhow::Result<(Vec<u8>, Option<Vec<usize>>)> {
        let selected = self.selected_ids();
        anyhow::ensure!(!selected.is_empty(), "파일을 하나 이상 선택하세요");
        let only_files = (selected.len() != self.preview.files.len()).then_some(selected);
        Ok((self.preview.torrent_bytes, only_files))
    }

    pub fn ui(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new(format!(
                    "선택 {} / {}개 · {} / {}",
                    self.selected_count(),
                    self.total_files(),
                    common::format_size(self.selected_bytes()),
                    common::format_size(self.tree.bytes),
                ))
                .strong(),
            );
        });
        ui.horizontal(|ui| {
            let select_all = ui.button("전체 선택");
            #[cfg(test)]
            ui.ctx().data_mut(|data| {
                data.insert_temp(
                    egui::Id::new("nsl_test_torrent_select_all"),
                    select_all.rect,
                );
            });
            if select_all.clicked() {
                self.all_selected(true);
            }
            let clear_all = ui.button("전체 해제");
            #[cfg(test)]
            ui.ctx().data_mut(|data| {
                data.insert_temp(egui::Id::new("nsl_test_torrent_clear_all"), clear_all.rect);
            });
            if clear_all.clicked() {
                self.all_selected(false);
            }
            ui.separator();
            if ui.small_button("모두 펼치기").clicked() {
                self.set_all_expanded(true);
            }
            if ui.small_button("모두 접기").clicked() {
                self.set_all_expanded(false);
            }
        });
        // A two-column Explorer-style header keeps file names and sizes
        // distinct, even with multiple nested directories.
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("이름").strong().small());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add_sized(
                    [TREE_SIZE_COLUMN, 18.0],
                    egui::Label::new(egui::RichText::new("크기").strong().small())
                        .halign(egui::Align::RIGHT),
                );
            });
        });
        // Paint only the visible rows. A torrent containing thousands of
        // files should not allocate thousands of egui widgets each frame.
        let mut rows = Vec::new();
        self.tree.collect_visible(&self.expanded, 0, 0, &mut rows);
        let background = ui.visuals().extreme_bg_color;
        let stroke = ui.visuals().widgets.noninteractive.bg_stroke;
        egui::Frame::none()
            .fill(background)
            .stroke(stroke)
            .inner_margin(egui::Margin::same(4.0))
            .show(ui, |ui| {
                egui::ScrollArea::vertical()
                    .id_salt("torrent_file_picker_tree")
                    .max_height(346.0)
                    .min_scrolled_height(188.0)
                    .auto_shrink([false, false])
                    .show_rows(ui, TREE_ROW_HEIGHT, rows.len(), |ui, range| {
                        for (index, row) in rows[range.clone()].iter().enumerate() {
                            Self::draw_node(
                                ui,
                                row,
                                &mut self.selected,
                                &mut self.expanded,
                                range.start + index,
                            );
                        }
                    });
            });
        ui.label(
            egui::RichText::new(
                "+ / − : 하위 폴더 펼치기·접기   |   체크박스 : 다운로드 항목 선택",
            )
            .small()
            .color(ui.visuals().weak_text_color()),
        );
    }

    fn set_all_expanded(&mut self, expand: bool) {
        fn walk(node: &FileNode, expanded: &mut HashSet<String>) {
            for child in node.children.values() {
                if !child.children.is_empty() {
                    expanded.insert(child.key.clone());
                    walk(child, expanded);
                }
            }
        }
        self.expanded.clear();
        if expand {
            walk(&self.tree, &mut self.expanded);
        }
    }

    fn draw_node(
        ui: &mut egui::Ui,
        row: &VisibleNode<'_>,
        selected: &mut [bool],
        expanded: &mut HashSet<String>,
        row_index: usize,
    ) {
        let node = row.node;
        let folder = !node.children.is_empty();
        let is_expanded = expanded.contains(&node.key);
        let checked_count = node.selected_count(selected);
        let count = node.descendants.len();
        let partially_checked = checked_count > 0 && checked_count < count;
        let start = ui.cursor().min;
        let row_rect =
            egui::Rect::from_min_size(start, egui::vec2(ui.available_width(), TREE_ROW_HEIGHT));
        if !row_index.is_multiple_of(2) {
            // Explorer-style alternating row shading. Keep it subtle so
            // folder guides and checked states have the stronger contrast.
            ui.painter().rect_filled(
                row_rect,
                0.0,
                ui.visuals().weak_text_color().linear_multiply(0.045),
            );
        }
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            ui.set_min_height(TREE_ROW_HEIGHT);
            ui.add_space(row.depth as f32 * TREE_LEVEL_INDENT);
            if folder {
                // egui::Button enforces intrinsic text padding (28px even
                // with add_sized(19px)), breaking the tree's indentation.
                // Reserve an exact 19px clickable square and draw the
                // familiar Windows Explorer [+] / [−] glyph ourselves.
                let (rect, toggle) = ui.allocate_exact_size(
                    egui::vec2(TREE_TOGGLE_SIZE, TREE_TOGGLE_SIZE),
                    egui::Sense::click(),
                );
                let border_color = if toggle.hovered() {
                    ui.visuals().hyperlink_color
                } else {
                    ui.visuals().weak_text_color().linear_multiply(0.75)
                };
                ui.painter().rect_filled(
                    rect,
                    egui::Rounding::same(2.0),
                    ui.visuals().extreme_bg_color,
                );
                ui.painter().rect_stroke(
                    rect.shrink(0.5),
                    egui::Rounding::same(2.0),
                    egui::Stroke::new(1.0_f32, border_color),
                );
                ui.painter().text(
                    rect.center(),
                    egui::Align2::CENTER_CENTER,
                    if is_expanded { "−" } else { "+" },
                    egui::FontId::monospace(14.0),
                    ui.visuals().text_color(),
                );
                #[cfg(test)]
                ui.ctx().data_mut(|data| {
                    data.insert_temp(
                        egui::Id::new(("nsl_test_torrent_node_expander", node.key.as_str())),
                        rect,
                    );
                });
                if toggle
                    .on_hover_text(if is_expanded {
                        "− : 하위 폴더 접기"
                    } else {
                        "+ : 하위 폴더 펼치기"
                    })
                    .clicked()
                {
                    if is_expanded {
                        expanded.remove(&node.key);
                    } else {
                        expanded.insert(node.key.clone());
                    }
                }
            } else {
                // No expander for a file, but reserve exactly the same
                // horizontal slot to line up every file and folder checkbox.
                // add_space does not reserve the same inter-widget spacing
                // as a Button, which made file rows drift left by 13px.
                // A real layout slot keeps file and folder checkboxes aligned.
                let _ = ui.allocate_exact_size(
                    egui::vec2(TREE_TOGGLE_SIZE, TREE_TOGGLE_SIZE),
                    egui::Sense::hover(),
                );
            }
            let mut checked = checked_count == count;
            let check =
                ui.add(egui::Checkbox::without_text(&mut checked).indeterminate(partially_checked));
            #[cfg(test)]
            ui.ctx().data_mut(|data| {
                data.insert_temp(
                    egui::Id::new(("nsl_test_torrent_node_checkbox", node.key.as_str())),
                    check.rect,
                );
            });
            if check.clicked() {
                // A partially selected directory becomes fully selected first.
                node.set_selected(selected, checked);
            }
            let icon = if folder {
                if is_expanded {
                    "📂"
                } else {
                    "📁"
                }
            } else {
                "📄"
            };
            let icon_response = ui.label(egui::RichText::new(icon).size(16.0));
            #[cfg(not(test))]
            let _ = icon_response;
            #[cfg(test)]
            ui.ctx().data_mut(|data| {
                data.insert_temp(
                    egui::Id::new(("nsl_test_torrent_node_icon", node.key.as_str())),
                    icon_response.rect,
                );
            });
            let name = if folder {
                egui::RichText::new(format!("{}  ({}개)", node.name, count)).strong()
            } else {
                egui::RichText::new(&node.name)
            };
            let name_width = (ui.available_width() - TREE_SIZE_COLUMN - 8.0).max(18.0);
            // egui::Ui::add_sized uses a centered-and-justified child layout.
            // Even Label::halign(LEFT) cannot move its *widget* to the left,
            // so short file names were centered in the whole name column.
            // Reserve the same column width, but place the text at its START
            // with a non-justified layout, directly beside the file icon.
            let name_response = ui
                .allocate_ui_with_layout(
                    egui::vec2(name_width, TREE_ROW_HEIGHT - 2.0),
                    egui::Layout::left_to_right(egui::Align::Center),
                    |ui| {
                        ui.add(
                            egui::Label::new(name)
                                .truncate()
                                .halign(egui::Align::LEFT)
                                .sense(egui::Sense::click()),
                        )
                    },
                )
                .inner;
            #[cfg(test)]
            ui.ctx().data_mut(|data| {
                data.insert_temp(
                    egui::Id::new(("nsl_test_torrent_node_name", node.key.as_str())),
                    name_response.rect,
                );
            });
            if name_response.clicked() {
                if folder {
                    if is_expanded {
                        expanded.remove(&node.key);
                    } else {
                        expanded.insert(node.key.clone());
                    }
                } else {
                    node.set_selected(selected, checked_count == 0);
                }
            }
            name_response.on_hover_text(&node.key);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add_sized(
                    [TREE_SIZE_COLUMN, TREE_ROW_HEIGHT - 2.0],
                    egui::Label::new(
                        egui::RichText::new(common::format_size(node.bytes))
                            .small()
                            .color(ui.visuals().weak_text_color()),
                    )
                    .halign(egui::Align::RIGHT),
                );
            });
        });
        // Draw Windows-style hierarchy guides in the whitespace BEFORE
        // each nested row. Never paint over its controls or file names.
        if row.depth > 0 {
            let guide = egui::Stroke::new(
                1.0_f32,
                ui.visuals().weak_text_color().linear_multiply(0.38),
            );
            let top = row_rect.top() - 2.0;
            let mid = row_rect.center().y;
            let bottom = row_rect.bottom() + 2.0;
            for level in 0..row.depth - 1 {
                if row.ancestor_continues & (1u64 << level) != 0 {
                    let x = start.x + level as f32 * TREE_LEVEL_INDENT + TREE_TOGGLE_SIZE * 0.5;
                    ui.painter()
                        .line_segment([egui::pos2(x, top), egui::pos2(x, bottom)], guide);
                }
            }
            let branch_x =
                start.x + (row.depth - 1) as f32 * TREE_LEVEL_INDENT + TREE_TOGGLE_SIZE * 0.5;
            ui.painter().line_segment(
                [
                    egui::pos2(branch_x, top),
                    egui::pos2(branch_x, if row.is_last { mid } else { bottom }),
                ],
                guide,
            );
            ui.painter().line_segment(
                [
                    egui::pos2(branch_x, mid),
                    egui::pos2(start.x + row.depth as f32 * TREE_LEVEL_INDENT - 2.0, mid),
                ],
                guide,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::torrent::TorrentFileEntry;
    use std::path::PathBuf;

    fn preview() -> TorrentPreview {
        let paths = [
            ("game/base/game.nsp", 100, 9),
            ("game/base/patch.nsp", 40, 1),
            ("game/dlc/extra/a.nsp", 12, 7),
            ("game/dlc/extra/b.nsp", 20, 5),
            ("readme.txt", 2, 3),
        ];
        TorrentPreview {
            files: paths
                .iter()
                .map(|(path, size, index)| TorrentFileEntry {
                    index: *index,
                    path: PathBuf::from(path),
                    size: *size,
                })
                .collect(),
            torrent_bytes: vec![42, 24],
        }
    }

    #[test]
    fn nested_directories_expand_and_toggle_all_descendants() {
        let mut picker = TorrentFilePicker::new(preview()).unwrap();
        assert_eq!(picker.selected_count(), 5);
        assert_eq!(picker.selected_bytes(), 174);
        assert!(picker.expanded.contains("game"));
        let game = &picker.tree.children["game"];
        let dlc = &game.children["dlc"];
        assert!(dlc.children.contains_key("extra"));
        assert!(!picker.expanded.contains("game/dlc"));
        dlc.set_selected(&mut picker.selected, false);
        assert_eq!(picker.selected_count(), 3);
        assert_eq!(picker.selected_bytes(), 142);
        assert_eq!(game.selected_count(&picker.selected), 2);
        game.set_selected(&mut picker.selected, true);
        assert_eq!(picker.selected_count(), 5);
        picker.set_all_expanded(true);
        assert!(picker.expanded.contains("game/dlc/extra"));
        picker.set_all_expanded(false);
        assert!(picker.expanded.is_empty());
        assert_eq!(picker.selected_count(), 5);
    }

    #[test]
    fn explorer_tree_folders_first_and_indentation_follow_real_depth() {
        let mut preview = preview();
        // File sorts before 'game' alphabetically, but Explorer must show
        // directories first at each level.
        preview.files.push(TorrentFileEntry {
            index: 19,
            path: PathBuf::from("000-notes.txt"),
            size: 3,
        });
        let mut picker = TorrentFilePicker::new(preview).unwrap();
        let mut visible = Vec::new();
        picker
            .tree
            .collect_visible(&picker.expanded, 0, 0, &mut visible);
        let names: Vec<_> = visible.iter().map(|row| row.node.key.as_str()).collect();
        assert_eq!(
            names,
            vec![
                "game",
                "game/base",
                "game/dlc",
                "000-notes.txt",
                "readme.txt"
            ]
        );
        assert_eq!(
            visible.iter().map(|row| row.depth).collect::<Vec<_>>(),
            vec![0, 1, 1, 0, 0]
        );
        assert!(!visible[1].is_last);
        assert!(visible[2].is_last);
        assert_eq!(visible[1].ancestor_continues, 1);
        picker.set_all_expanded(true);
        let mut expanded_rows = Vec::new();
        picker
            .tree
            .collect_visible(&picker.expanded, 0, 0, &mut expanded_rows);
        assert_eq!(
            expanded_rows
                .iter()
                .map(|row| (row.node.key.as_str(), row.depth))
                .collect::<Vec<_>>(),
            vec![
                ("game", 0),
                ("game/base", 1),
                ("game/base/game.nsp", 2),
                ("game/base/patch.nsp", 2),
                ("game/dlc", 1),
                ("game/dlc/extra", 2),
                ("game/dlc/extra/a.nsp", 3),
                ("game/dlc/extra/b.nsp", 3),
                ("000-notes.txt", 0),
                ("readme.txt", 0),
            ]
        );
        // Visual sorting and expansion never change underlying file IDs.
        assert_eq!(picker.selected_ids(), vec![9, 1, 7, 5, 3, 19]);
        picker.set_all_expanded(false);
        let mut collapsed = Vec::new();
        picker
            .tree
            .collect_visible(&picker.expanded, 0, 0, &mut collapsed);
        assert_eq!(collapsed.len(), 3);
        assert_eq!(picker.selected_count(), 6);
    }

    #[test]
    fn explorer_file_names_start_immediately_after_icons_at_every_depth() {
        for screen_width in [720.0_f32, 980.0, 1456.0] {
            let mut picker = TorrentFilePicker::new(preview()).unwrap();
            picker.set_all_expanded(true);
            let ctx = egui::Context::default();
            ctx.begin_pass(egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(screen_width, 800.0),
                )),
                ..Default::default()
            });
            egui::CentralPanel::default().show(&ctx, |ui| {
                ui.set_width(650.0);
                picker.ui(ui);
            });
            let _ = ctx.end_pass();

            for path in [
                "game",
                "game/base",
                "game/base/game.nsp",
                "game/dlc/extra",
                "game/dlc/extra/a.nsp",
                "readme.txt",
            ] {
                let icon = ctx.data_mut(|data| {
                    data.get_temp::<egui::Rect>(egui::Id::new(("nsl_test_torrent_node_icon", path)))
                        .unwrap_or_else(|| panic!("missing icon for {path} at {screen_width}px"))
                });
                let name = ctx.data_mut(|data| {
                    data.get_temp::<egui::Rect>(egui::Id::new(("nsl_test_torrent_node_name", path)))
                        .unwrap_or_else(|| panic!("missing name for {path} at {screen_width}px"))
                });
                assert!(
                    name.left() >= icon.right() - 1.0
                        && name.left() <= icon.right() + 10.0,
                    "name must start immediately after its icon, not centered: screen={screen_width}, path={path}, icon={icon:?}, name={name:?}"
                );
                assert!(
                    name.width() < 260.0,
                    "short filename must not stretch across whole name column: screen={screen_width}, path={path}, name={name:?}"
                );
            }
        }
    }

    #[test]
    fn explorer_expanders_draw_plus_for_closed_and_minus_for_open_folders() {
        let mut picker = TorrentFilePicker::new(preview()).unwrap();
        let collect_glyphs = |picker: &mut TorrentFilePicker| {
            let context = egui::Context::default();
            context.begin_pass(egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(980.0, 720.0),
                )),
                ..Default::default()
            });
            egui::CentralPanel::default().show(&context, |ui| {
                ui.set_width(650.0);
                picker.ui(ui);
            });
            let output = context.end_pass();
            output
                .shapes
                .iter()
                .filter_map(|painted| {
                    if let egui::epaint::Shape::Text(text) = &painted.shape {
                        match text.galley.text() {
                            "+" => Some('+'),
                            "−" => Some('-'),
                            _ => None,
                        }
                    } else {
                        None
                    }
                })
                .collect::<Vec<_>>()
        };
        let default_glyphs = collect_glyphs(&mut picker);
        assert!(
            default_glyphs.contains(&'+') && default_glyphs.contains(&'-'),
            "the default explorer tree must draw both +/-: {default_glyphs:?}"
        );
        picker.set_all_expanded(true);
        let expanded_glyphs = collect_glyphs(&mut picker);
        assert!(!expanded_glyphs.contains(&'+'));
        assert!(expanded_glyphs.contains(&'-'));
        picker.set_all_expanded(false);
        let collapsed_glyphs = collect_glyphs(&mut picker);
        assert!(collapsed_glyphs.contains(&'+'));
        assert!(!collapsed_glyphs.contains(&'-'));
    }

    #[test]
    fn explorer_scroll_area_virtualizes_large_torrent_tree() {
        let files = (0..12_000)
            .map(|index| TorrentFileEntry {
                index,
                path: PathBuf::from(format!("pack/subdir/item_{index:05}.nsp")),
                size: 1024,
            })
            .collect();
        let mut picker = TorrentFilePicker::new(TorrentPreview {
            files,
            torrent_bytes: vec![1],
        })
        .unwrap();
        picker.set_all_expanded(true);
        let mut rows = Vec::new();
        picker
            .tree
            .collect_visible(&picker.expanded, 0, 0, &mut rows);
        assert_eq!(rows.len(), 12_002);
        assert_eq!(picker.selected_count(), 12_000);

        let context = egui::Context::default();
        context.begin_pass(egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(980.0, 680.0),
            )),
            ..Default::default()
        });
        egui::CentralPanel::default().show(&context, |ui| {
            ui.set_width(650.0);
            picker.ui(ui);
        });
        let output = context.end_pass();
        assert!(
            output.shapes.len() < 1_500,
            "large tree rendered more than just visible rows: shapes={}",
            output.shapes.len()
        );
        assert_eq!(picker.selected_ids().len(), 12_000);
    }

    #[test]
    fn selective_transfer_keeps_original_torrent_indices() {
        let mut picker = TorrentFilePicker::new(preview()).unwrap();
        picker.all_selected(false);
        picker.tree.children["game"].children["dlc"].set_selected(&mut picker.selected, true);
        assert_eq!(picker.selected_ids(), vec![7, 5]);
        let (metadata, only) = picker.into_transfer().unwrap();
        assert_eq!(metadata, vec![42, 24]);
        assert_eq!(only, Some(vec![7, 5]));
    }

    #[test]
    fn all_selection_passes_none_and_empty_selection_cannot_start() {
        let picker = TorrentFilePicker::new(preview()).unwrap();
        assert!(picker.into_transfer().unwrap().1.is_none());
        let mut picker = TorrentFilePicker::new(preview()).unwrap();
        picker.all_selected(false);
        assert_eq!(picker.selected_count(), 0);
        assert!(picker.into_transfer().is_err());
    }

    #[test]
    fn rejects_conflicting_nested_paths_and_unsafe_components() {
        let files = vec![
            TorrentFileEntry {
                index: 0,
                path: PathBuf::from("base"),
                size: 1,
            },
            TorrentFileEntry {
                index: 1,
                path: PathBuf::from("base/file.nsp"),
                size: 1,
            },
        ];
        assert!(TorrentFilePicker::new(TorrentPreview {
            files,
            torrent_bytes: vec![],
        })
        .is_err());
        let mut unsafe_file = preview();
        unsafe_file.files[0].path = PathBuf::from("../escape.bin");
        assert!(TorrentFilePicker::new(unsafe_file).is_err());
    }
}
