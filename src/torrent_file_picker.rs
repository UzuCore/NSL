//! A real directory tree for selective torrent downloads.
//! The tree keeps librqbit's original file indices even when folders are
//! expanded, collapsed or sorted. Checking a folder updates every descendant.
use std::{
    collections::{BTreeMap, HashSet},
    path::Component,
};

use eframe::egui;

use crate::{common, torrent::TorrentPreview};

#[derive(Debug, Default)]
struct FileNode {
    name: String,
    key: String,
    children: BTreeMap<String, FileNode>,
    file_slot: Option<usize>,
    descendants: Vec<usize>,
    bytes: u64,
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
        ui.separator();
        egui::ScrollArea::vertical()
            .id_salt("torrent_file_picker_tree")
            .max_height(340.0)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for node in self.tree.children.values() {
                    Self::draw_node(ui, node, &mut self.selected, &mut self.expanded, 0);
                }
            });
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
        node: &FileNode,
        selected: &mut [bool],
        expanded: &mut HashSet<String>,
        depth: usize,
    ) {
        let folder = !node.children.is_empty();
        let is_expanded = expanded.contains(&node.key);
        let checked_count = node.selected_count(selected);
        let count = node.descendants.len();
        let partially_checked = checked_count > 0 && checked_count < count;
        ui.horizontal(|ui| {
            ui.add_space(depth as f32 * 16.0);
            if folder {
                let arrow = if is_expanded { "▾" } else { "▸" };
                if ui
                    .small_button(arrow)
                    .on_hover_text("하위 폴더 펼치기/접기")
                    .clicked()
                {
                    if is_expanded {
                        expanded.remove(&node.key);
                    } else {
                        expanded.insert(node.key.clone());
                    }
                }
            } else {
                ui.add_space(21.0);
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
            let name = if folder {
                format!("📁 {}  ({}개)", node.name, count)
            } else {
                format!("📄 {}", node.name)
            };
            let name_response = ui.add(
                egui::Label::new(name)
                    .truncate()
                    .sense(egui::Sense::click()),
            );
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
                ui.label(
                    egui::RichText::new(common::format_size(node.bytes))
                        .small()
                        .color(ui.visuals().weak_text_color()),
                );
            });
        });
        if folder && expanded.contains(&node.key) {
            for child in node.children.values() {
                Self::draw_node(ui, child, selected, expanded, depth + 1);
            }
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
