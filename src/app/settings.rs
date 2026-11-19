//! Render and persist app settings and cache statistics.

use crate::*;

impl NslApp {
    pub(crate) fn cache_stats_text(&self) -> String {
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
    pub(crate) fn settings_panel_ui(&mut self, ui: &mut egui::Ui) {
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
                        if ui.button("X  닫기").on_hover_text("Esc로 닫기").clicked() {
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

                                    settings_row_label(ui, "프로그램 업데이트");
                                    let mut auto_update_check = self
                                        .config
                                        .settings
                                        .get("auto_update_check")
                                        .and_then(|value| value.as_str())
                                        .unwrap_or("1")
                                        == "1";
                                    ui.vertical(|ui| {
                                        if ui
                                            .checkbox(
                                                &mut auto_update_check,
                                                "시작 시 새 버전 자동 확인",
                                            )
                                            .changed()
                                        {
                                            self.config.settings.insert(
                                                "auto_update_check".into(),
                                                serde_json::Value::String(
                                                    if auto_update_check { "1" } else { "0" }
                                                        .into(),
                                                ),
                                            );
                                        }
                                        ui.label(
                                            egui::RichText::new(
                                                "백그라운드에서 버전만 확인합니다. 설치는 승인이 필요합니다.",
                                            )
                                            .small()
                                            .color(ui.visuals().weak_text_color()),
                                        );
                                    });
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
                            egui::Grid::new("settings_database_rows")
                                .num_columns(2)
                                .spacing(egui::vec2(0.0, 4.0))
                                .show(ui, |ui| {
                                    settings_row_label(ui, "타이틀 DB 소스");
                                    ui.label("borntohonk/titledb (GitHub)");
                                    ui.end_row();
                                    settings_row_label(ui, "기본 타이틀 정보");
                                    ui.label("titles.json.zst 다운로드 → titles.json 자동 압축 해제");
                                    ui.end_row();
                                    settings_row_label(ui, "추가 타이틀 정보");
                                    ui.label("versions.json, KR.ko.json");
                                    ui.end_row();
                                    settings_row_label(ui, "저장소");
                                    ui.hyperlink_to(
                                        "github.com/borntohonk/titledb",
                                        "https://github.com/borntohonk/titledb",
                                    );
                                    ui.end_row();
                                });
                            self.config.settings.insert(
                                "titledb_source".into(),
                                serde_json::Value::String(titledb::TITLE_DB_SOURCE.into()),
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
                                    "「삭제」 확인창에서 파일까지 삭제 옵션을 우선 표시",
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
                                    "삭제 버튼은 항상 확인창을 엽니다.\n\
                                     끔(기본): 파일을 유지하는 선택지를 먼저 표시합니다.\n\
                                     켬: 파일까지 삭제하는 선택지를 먼저 표시합니다.\n\
                                     실제 파일 삭제는 확인창에서 직접 선택해야 실행됩니다.",
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
}
