//! Browse the game catalog, show details, and prepare torrent selections.

use crate::*;

impl NslApp {
    /// Download the torrent catalog snapshot in the background.
    pub(crate) fn start_catalog_download(&mut self) {
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
                .map_err(|e| e.to_string());
            let _ = log.send(match &result {
                Ok(state) if state.not_modified => {
                    format!("✅ 카탈로그 이미 최신 · {}개 항목", state.items.len())
                }
                Ok(state) => format!("✅ 카탈로그 다운로드 · {}개 항목", state.items.len()),
                Err(error) => format!("❌ 카탈로그 다운로드 실패 · {error}"),
            });
            let _ = tx.send(result);
        });
    }

    /// Make a catalog title safe to use as a download sub-folder name.
    pub(crate) fn sanitize_folder_name(title: &str) -> String {
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
    pub(crate) fn start_catalog_torrent(&mut self, item: &catalog::CatalogItem) {
        if self.catalog_torrent_dialog.is_some() {
            self.status = "다운로드 파일 선택 창이 이미 열려 있습니다".into();
            return;
        }
        let Some(engine) = self.torrent_engine.clone() else {
            self.status = "토렌트 엔진을 사용할 수 없습니다".into();
            return;
        };
        if item.magnet.is_empty() {
            self.status = "이 항목에는 마그넷 링크가 없습니다".into();
            return;
        }
        match engine.preview_files(&item.magnet) {
            Ok(request) => {
                self.catalog_torrent_dialog = Some(CatalogTorrentDialog {
                    item: item.clone(),
                    request: Some(request),
                    files: None,
                    error: None,
                });
                // The status bar derives this transient activity from the
                // live preview request. Do not store a permanent 'loading'
                // message that survives success, failure or cancellation.
            }
            Err(error) => self.status = format!("토렌트 파일 목록 조회 실패: {error}"),
        }
    }

    /// Called only after the user confirms the metadata file-picker dialog.
    pub(crate) fn start_selected_catalog_torrent(
        &mut self,
        item: &catalog::CatalogItem,
        picker: torrent_file_picker::TorrentFilePicker,
    ) {
        let Some(engine) = self.torrent_engine.clone() else {
            self.status = "토렌트 엔진을 사용할 수 없습니다".into();
            return;
        };
        let (torrent_bytes, only_files) = match picker.into_transfer() {
            Ok(selection) => selection,
            Err(error) => {
                self.status = error.to_string();
                return;
            }
        };
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
        match engine.start_download_selected(
            &item.title,
            &item.magnet,
            sub_folder,
            Some(torrent_bytes),
            only_files,
        ) {
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

    pub(crate) fn invalidate_catalog_title_details(&mut self) {
        // A title DB import or catalog refresh invalidates all cached rows.
        // Dropping the old receiver also makes stale worker replies harmless.
        self.catalog_title_load_result = None;
        self.catalog_title_metadata_loaded = false;
        self.catalog_title_details.clear();
        self.catalog_search_cache = None;
    }

    pub(crate) fn start_catalog_title_details(&mut self) {
        if self.catalog_title_metadata_loaded
            || self.catalog_title_load_result.is_some()
            || self.catalog_state.items.is_empty()
        {
            return;
        }
        let ids = self
            .catalog_state
            .items
            .iter()
            .filter_map(catalog::CatalogItem::base_title_id)
            .collect::<HashSet<_>>();
        let db = self.db.clone();
        let (tx, rx) = mpsc::channel();
        self.catalog_title_load_result = Some(rx);
        std::thread::spawn(move || {
            let result = db
                .catalog_title_details(&ids)
                .map_err(|error| error.to_string());
            let _ = tx.send(result);
        });
    }

    pub(crate) fn refresh_catalog_matches(&mut self, ctx: &egui::Context) {
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
        let user_filter_key = format!(
            "{query}\u{1f}{}\u{1f}{}\u{1f}{}",
            self.catalog_genre_filter, self.catalog_format_filter, self.catalog_language_filter,
        );
        let filter_key = format!("{user_filter_key}\u{1f}{}", self.rows_generation);
        if self.catalog_search_cache.as_ref() != Some(&filter_key) {
            // A DB/name refresh must re-evaluate localized names, but must
            // not unexpectedly send someone browsing page 22 back to page 1.
            let user_changed = self.catalog_user_filter_cache.as_ref() != Some(&user_filter_key);
            self.catalog_matching_indices = self
                .catalog_state
                .items
                .iter()
                .enumerate()
                .filter(|(_, item)| {
                    let base = item.base_title_id();
                    let metadata = base
                        .as_ref()
                        .and_then(|id| self.catalog_title_details.get(id));
                    if !catalog_item_matches_language(item, metadata, &self.catalog_language_filter)
                    {
                        return false;
                    }
                    let preferred = catalog_preferred_title(
                        item,
                        metadata,
                        base.as_ref().and_then(|id| self.catalog_owned.get(id)),
                    );
                    catalog_matches_filters(
                        item,
                        &query,
                        &self.catalog_genre_filter,
                        &self.catalog_format_filter,
                    ) || (catalog_matches_filters(
                        item,
                        "",
                        &self.catalog_genre_filter,
                        &self.catalog_format_filter,
                    ) && preferred.to_lowercase().contains(&query))
                })
                .map(|(index, _)| index)
                .collect();
            let selection_matches = self.catalog_selected.as_deref().is_some_and(|id| {
                self.catalog_matching_indices
                    .iter()
                    .any(|&index| self.catalog_state.items[index].topic_id == id)
            });
            if !selection_matches {
                self.catalog_selected = self
                    .catalog_matching_indices
                    .first()
                    .map(|&index| self.catalog_state.items[index].topic_id.clone());
            }
            self.catalog_search_cache = Some(filter_key);
            self.catalog_user_filter_cache = Some(user_filter_key);
            if user_changed {
                self.catalog_page = 0;
                self.catalog_scroll_reset = true;
            }
        }
    }

    pub(crate) fn catalog_details_field(ui: &mut egui::Ui, label: &str, value: &str) {
        if value.trim().is_empty() {
            return;
        }
        ui.horizontal(|ui| {
            ui.add_sized(
                [72.0, 20.0],
                egui::Label::new(
                    egui::RichText::new(label)
                        .small()
                        .color(ui.visuals().weak_text_color()),
                ),
            );
            ui.add(egui::Label::new(value).wrap());
        });
    }

    pub(crate) fn catalog_selected_details_ui(&mut self, ui: &mut egui::Ui) {
        let selected = self.catalog_selected.as_deref().and_then(|id| {
            self.catalog_state
                .items
                .iter()
                .find(|item| item.topic_id == id)
                .cloned()
        });
        let Some(item) = selected else {
            ui.label(
                egui::RichText::new("게임 이름을 선택하면 상세 정보가 표시됩니다.")
                    .small()
                    .color(ui.visuals().weak_text_color()),
            );
            return;
        };
        let base = item.base_title_id();
        let metadata = base
            .as_ref()
            .and_then(|id| self.catalog_title_details.get(id))
            .cloned();
        let owned = base
            .as_ref()
            .and_then(|id| self.catalog_owned.get(id))
            .cloned();
        let name = catalog_preferred_title(&item, metadata.as_ref(), owned.as_ref()).to_owned();

        if self.catalog_cover_queue.is_none()
            && self.should_prefetch_catalog_cover(&item, std::time::Instant::now())
        {
            self.start_cover_prefetch(vec![(item.topic_id.clone(), item.cover.clone())]);
        }
        let mut budget = 1;
        self.ensure_catalog_cover(ui.ctx(), &item.topic_id, &mut budget);
        let header_width = ui.available_width();
        let mut title_rect = egui::Rect::NOTHING;
        ui.horizontal(|ui| {
            let cover_size = egui::vec2(78.0, 100.0);
            if let Some(cover) = self.catalog_cover_textures.get(&item.topic_id) {
                let original = cover.size_vec2();
                let scale =
                    (cover_size.x / original.x.max(1.0)).min(cover_size.y / original.y.max(1.0));
                ui.add(egui::Image::new((cover.id(), original * scale)));
            } else {
                ui.allocate_ui_with_layout(
                    cover_size,
                    egui::Layout::centered_and_justified(egui::Direction::TopDown),
                    |ui| {
                        ui.label(egui::RichText::new("🎮").size(23.0));
                    },
                );
            }
            ui.vertical(|ui| {
                ui.set_width((header_width - 90.0).max(95.0));
                let title_label = ui.label(egui::RichText::new(&name).size(15.0).strong());
                title_rect = title_label.rect;
                if name != item.title {
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(&item.title)
                                .small()
                                .color(ui.visuals().weak_text_color()),
                        )
                        .truncate(),
                    )
                    .on_hover_text("카탈로그 원본: ".to_owned() + &item.title);
                }
                ui.add_space(4.0);
                if let Some(owned) = &owned {
                    let message = if owned.has_base {
                        "● 라이브러리에 보유 중"
                    } else {
                        "● 업데이트 / DLC 보유"
                    };
                    ui.colored_label(ui.visuals().hyperlink_color, message);
                } else {
                    ui.label(
                        egui::RichText::new("라이브러리에 없음")
                            .small()
                            .color(ui.visuals().weak_text_color()),
                    );
                }
            });
        });
        #[cfg(test)]
        ui.ctx().data_mut(|data| {
            data.insert_temp(egui::Id::new("nsl_test_store_detail_title"), title_rect);
        });
        #[cfg(not(test))]
        let _ = title_rect;
        ui.add_space(6.0);
        ui.separator();
        Self::catalog_details_field(ui, "타이틀 ID", item.title_id.as_deref().unwrap_or(""));
        Self::catalog_details_field(ui, "출시 시기", &item.year);
        Self::catalog_details_field(ui, "장르", &item.genre);
        Self::catalog_details_field(ui, "개발사", &item.developer);
        Self::catalog_details_field(ui, "배급사", &item.publisher);
        Self::catalog_details_field(ui, "표기 언어", &item.interface_lang);
        Self::catalog_details_field(ui, "파일 형식", &item.image_format);
        Self::catalog_details_field(ui, "다운로드 크기", &item.size);
        if let Some(metadata) = &metadata {
            Self::catalog_details_field(ui, "DB 출시일", &metadata.release_date);
            Self::catalog_details_field(ui, "DB 언어", &metadata.languages);
            Self::catalog_details_field(ui, "지원 인원", &metadata.num_players);
            Self::catalog_details_field(ui, "이용 등급", &metadata.rating);
            Self::catalog_details_field(ui, "지역", &metadata.region);
            if metadata.version > 0 {
                Self::catalog_details_field(ui, "최신 버전", &format!("v{}", metadata.version));
            }
        } else if self.catalog_title_load_result.is_some() {
            ui.label(
                egui::RichText::new("타이틀 DB 상세 정보를 불러오는 중...")
                    .small()
                    .color(ui.visuals().weak_text_color()),
            );
        }
        if let Some(owned) = &owned {
            ui.add_space(5.0);
            ui.label(egui::RichText::new("내 라이브러리").strong());
            Self::catalog_details_field(ui, "보유 파일", &format!("{}개", owned.file_count));
            Self::catalog_details_field(ui, "보유 DLC", &format!("{}개", owned.dlc_count));
            if owned.has_update && owned.update_version > 0 {
                Self::catalog_details_field(ui, "보유 버전", &format!("v{}", owned.update_version));
            }
        }
        if let Some(metadata) = &metadata {
            if let Ok(urls) = serde_json::from_str::<Vec<String>>(&metadata.screenshots) {
                let screenshots = urls
                    .iter()
                    .filter(|url| url.starts_with("https://") || url.starts_with("http://"))
                    .take(4)
                    .collect::<Vec<_>>();
                if !screenshots.is_empty() {
                    ui.add_space(6.0);
                    ui.label(egui::RichText::new("스크린샷").strong());
                    ui.horizontal_wrapped(|ui| {
                        for (index, url) in screenshots.iter().enumerate() {
                            ui.hyperlink_to(format!("사진 {}", index + 1), url.as_str());
                        }
                    });
                }
            }
        }
        if item.url.starts_with("https://") || item.url.starts_with("http://") {
            ui.add_space(8.0);
            let original_button = ui.add_sized(
                [ui.available_width(), 29.0],
                egui::Button::new("원본 게시물 보기"),
            );
            #[cfg(test)]
            ui.ctx().data_mut(|data| {
                data.insert_temp(
                    egui::Id::new("nsl_test_store_original_post_button"),
                    original_button.rect,
                );
            });
            if original_button.clicked() {
                ui.ctx().open_url(egui::OpenUrl::new_tab(item.url));
            }
        }
    }

    /// Store controls live in their own sidebar, independent of the saved
    /// game-detail splitter. The catalog list below gets all remaining space.
    pub(crate) fn catalog_sidebar_ui(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        self.start_catalog_title_details();
        ui.spacing_mut().item_spacing.y = 6.0;
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
                let search = ui.add_sized(
                    [ui.available_width(), 30.0],
                    egui::TextEdit::singleline(&mut self.catalog_search)
                        .id(egui::Id::new("catalog_search"))
                        .min_size(egui::vec2(0.0, 30.0))
                        .vertical_align(egui::Align::Center)
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
                ui.add_space(3.0);
                let genre_label = STORE_GENRES
                    .iter()
                    .find(|(key, _)| *key == self.catalog_genre_filter)
                    .map(|(_, label)| *label)
                    .unwrap_or("전체 장르");
                ui.horizontal(|ui| {
                    ui.add_sized(
                        [65.0, 24.0],
                        egui::Label::new(egui::RichText::new("장르").strong()),
                    );
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
                });
                let format_label = STORE_FORMATS
                    .iter()
                    .find(|(key, _)| *key == self.catalog_format_filter)
                    .map(|(_, label)| *label)
                    .unwrap_or("전체 형식");
                ui.horizontal(|ui| {
                    ui.add_sized(
                        [65.0, 24.0],
                        egui::Label::new(egui::RichText::new("파일 형식").strong()),
                    );
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
                });
                let language_label = STORE_LANGUAGES
                    .iter()
                    .find(|(key, _)| *key == self.catalog_language_filter)
                    .map(|(_, label)| *label)
                    .unwrap_or("전체 언어");
                let mut language_rect = egui::Rect::NOTHING;
                ui.horizontal(|ui| {
                    ui.add_sized(
                        [65.0, 24.0],
                        egui::Label::new(egui::RichText::new("지원 언어").strong()),
                    )
                    .on_hover_text("카탈로그/타이틀 DB에 명시된 언어만 분류합니다. Multi만으로 특정 언어를 추정하지 않습니다.");
                    let language_combo = egui::ComboBox::from_id_salt("catalog_language_filter")
                        .selected_text(language_label)
                        .width(ui.available_width())
                        .show_ui(ui, |ui| {
                            for &(key, label) in &STORE_LANGUAGES {
                                ui.selectable_value(
                                    &mut self.catalog_language_filter,
                                    key.to_owned(),
                                    label,
                                );
                            }
                        });
                    language_rect = language_combo.response.rect;
                });
                #[cfg(test)]
                ui.ctx().data_mut(|data| {
                    data.insert_temp(
                        egui::Id::new("nsl_test_store_language_filter"),
                        language_rect,
                    );
                });
                #[cfg(not(test))]
                let _ = language_rect;
                ui.add_space(5.0);
                self.refresh_catalog_matches(ctx);
                ui.separator();
                let results = ui.label(format!(
                    "검색 결과 {}개",
                    common::format_count(self.catalog_matching_indices.len() as i64)
                ));
                #[cfg(test)]
                ui.ctx().data_mut(|data| {
                    data.insert_temp(egui::Id::new("nsl_test_store_result_count"), results.rect);
                });
                #[cfg(not(test))]
                let _ = results;
                ui.add_space(3.0);
                let reset = ui
                    .add_sized(
                        [ui.available_width(), 28.0],
                        egui::Button::new("검색·분류 초기화"),
                    );
                #[cfg(test)]
                ui.ctx().data_mut(|data| {
                    data.insert_temp(egui::Id::new("nsl_test_store_reset_button"), reset.rect);
                });
                if reset.clicked() {
                    self.catalog_search.clear();
                    self.catalog_search_committed.clear();
                    self.catalog_search_pending = None;
                    self.catalog_genre_filter = "all".into();
                    self.catalog_format_filter = "all".into();
                    self.catalog_language_filter = "all".into();
                    self.refresh_catalog_matches(ctx);
                    ctx.request_repaint();
                }
                // add_enabled() keeps the surrounding left-aligned layout:
                // min_size expands the button, but does NOT center its text.
                // add_sized() supplies a centered-and-justified child layout.
                let refresh = ui
                    .add_enabled_ui(self.catalog_result.is_none(), |ui| {
                        ui.add_sized(
                            [ui.available_width(), 29.0],
                            egui::Button::new("카탈로그 새로고침"),
                        )
                    })
                    .inner;
                #[cfg(test)]
                ui.ctx().data_mut(|data| {
                    data.insert_temp(egui::Id::new("nsl_test_store_refresh_button"), refresh.rect);
                });
                if refresh.clicked() {
                    self.start_catalog_download();
                }
                let can_download = self.catalog_selected.as_deref().is_some_and(|id| {
                    self.catalog_state
                        .items
                        .iter()
                        .any(|item| item.topic_id == id)
                });
                let download = ui
                    .add_enabled_ui(can_download, |ui| {
                        ui.add_sized(
                            [ui.available_width(), 29.0],
                            egui::Button::new("게임 다운로드"),
                        )
                    })
                    .inner;
                #[cfg(test)]
                ui.ctx().data_mut(|data| {
                    data.insert_temp(
                        egui::Id::new("nsl_test_store_game_download_button"),
                        download.rect,
                    );
                });
                if download.clicked() {
                    if let Some(item) = self.catalog_selected.as_deref().and_then(|id| {
                        self.catalog_state
                            .items
                            .iter()
                            .find(|item| item.topic_id == id)
                            .cloned()
                    }) {
                        self.start_catalog_torrent(&item);
                    }
                }
                ui.separator();
                let heading = ui.label(egui::RichText::new("게임 정보").strong());
                #[cfg(test)]
                ui.ctx().data_mut(|data| {
                    data.insert_temp(egui::Id::new("nsl_test_store_game_info_heading"), heading.rect);
                });
                #[cfg(not(test))]
                let _ = heading;
                ui.add_space(3.0);
                self.catalog_selected_details_ui(ui);
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
    pub(crate) fn catalog_panel_ui(&mut self, ui: &mut egui::Ui, _ctx: &egui::Context) {
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
        // Store paging is always on, irrespective of the library's paging_on
        // checkbox, but uses the same user-selected games-per-page setting.
        let page_size = configured_game_page_size(&self.config.settings);
        let (pagination_rect, next_button_rect, page_changed) = management_pagination_ui(
            ui,
            &mut self.catalog_page,
            self.catalog_matching_indices.len(),
            page_size,
        );
        if page_changed {
            self.catalog_scroll_reset = true;
        }
        // Selecting a different page must not highlight a hidden title.
        if page_changed {
            let start = self.catalog_page * page_size;
            let end = start
                .saturating_add(page_size)
                .min(self.catalog_matching_indices.len());
            let is_visible = self.catalog_matching_indices[start..end]
                .iter()
                .any(|&index| {
                    self.catalog_selected.as_deref()
                        == Some(self.catalog_state.items[index].topic_id.as_str())
                });
            if !is_visible {
                self.catalog_selected = self
                    .catalog_matching_indices
                    .get(start)
                    .map(|&index| self.catalog_state.items[index].topic_id.clone());
            }
        }
        let focus_id = ui.make_persistent_id("catalog_keyboard_navigation");
        let _focus_anchor = ui.interact(
            ui.available_rect_before_wrap(),
            focus_id,
            egui::Sense::focusable_noninteractive(),
        );
        let visible_rows = (ui.available_height() / (72.0 + ui.spacing().item_spacing.y))
            .floor()
            .max(1.0) as usize;
        let navigation = management_navigation_input(
            ui.ctx(),
            self.management_navigation_enabled(ui.ctx()),
            visible_rows,
        );
        let mut keyboard_scroll_row = None;
        if let Some(navigation) = navigation {
            let selected_index = self.catalog_matching_indices.iter().position(|&index| {
                self.catalog_selected.as_deref()
                    == Some(self.catalog_state.items[index].topic_id.as_str())
            });
            let page_start = self.catalog_page * page_size;
            let page_end = (page_start + page_size).min(self.catalog_matching_indices.len());
            if let Some(target) = library_navigation_target(
                selected_index,
                self.catalog_matching_indices.len(),
                page_start,
                page_end,
                navigation.step,
                navigation.home,
                navigation.end,
            ) {
                let index = self.catalog_matching_indices[target];
                self.catalog_selected = Some(self.catalog_state.items[index].topic_id.clone());
                let next_page = target / page_size;
                if self.catalog_page != next_page {
                    self.catalog_page = next_page;
                    self.catalog_scroll_reset = true;
                }
                keyboard_scroll_row = Some(target % page_size);
                ui.ctx().memory_mut(|memory| memory.request_focus(focus_id));
                ui.ctx().request_repaint();
            }
            if navigation.enter {
                if let Some(item) = self.catalog_selected.as_deref().and_then(|selected| {
                    self.catalog_matching_indices.iter().find_map(|&index| {
                        let item = &self.catalog_state.items[index];
                        (item.topic_id == selected).then(|| item.clone())
                    })
                }) {
                    self.start_catalog_torrent(&item);
                }
            }
        }
        #[cfg(test)]
        ui.ctx().data_mut(|data| {
            data.insert_temp(
                egui::Id::new("nsl_test_catalog_pagination_rect"),
                pagination_rect,
            );
            data.insert_temp(
                egui::Id::new("nsl_test_catalog_pagination_next"),
                next_button_rect,
            );
        });
        #[cfg(not(test))]
        let _ = (pagination_rect, next_button_rect);
        ui.add_space(6.0);
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
        #[cfg(test)]
        ui.ctx().data_mut(|data| {
            data.insert_temp(
                egui::Id::new("nsl_test_catalog_visible_items"),
                page_indices.len(),
            )
        });
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
        let reset_scroll = std::mem::take(&mut self.catalog_scroll_reset);
        if reset_scroll {
            catalog_scroll = catalog_scroll.vertical_scroll_offset(0.0);
        }
        if let Some(index) = keyboard_scroll_row {
            let old_offset = if reset_scroll {
                0.0
            } else {
                ui.ctx().data_mut(|data| {
                    data.get_temp::<f32>(egui::Id::new("catalog_last_scroll_y"))
                        .unwrap_or(0.0)
                })
            };
            let viewport_height = ui.ctx().data_mut(|data| {
                data.get_temp::<f32>(egui::Id::new("catalog_last_viewport_height"))
                    .unwrap_or_else(|| ui.available_height())
            });
            if let Some(offset) = scroll_offset_for_selected_row(
                old_offset,
                viewport_height,
                index,
                72.0 + ui.spacing().item_spacing.y,
                72.0,
            ) {
                catalog_scroll = catalog_scroll.vertical_scroll_offset(offset);
            }
        }
        let _catalog_scroll =
            catalog_scroll.show_rows(ui, 72.0, page_indices.len(), |ui, row_range| {
                let mut cover_budget = ICON_LOAD_BUDGET;
                for &index in &page_indices[row_range] {
                    let item = self.catalog_state.items[index].clone();
                    let base = item.base_title_id();
                    let display_title = catalog_preferred_title(
                        &item,
                        base.as_ref()
                            .and_then(|id| self.catalog_title_details.get(id)),
                        base.as_ref().and_then(|id| self.catalog_owned.get(id)),
                    )
                    .to_owned();
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
                    let card_id = ui.make_persistent_id(("catalog-card", item.topic_id.as_str()));
                    let card = selectable_card_background(ui, card_id, row_width, 72.0);
                    let frame = egui::Frame::none()
                        .fill(base_fill)
                        .stroke(egui::Stroke::new(1.0_f32, border))
                        .rounding(egui::Rounding::same(4.0))
                        .inner_margin(egui::Margin::same(6.0));
                    // Fold child hit results into a boolean instead of
                    // allocating a Vec for every visible catalog row.
                    let mut clicked_content = false;
                    let _response = frame.show(ui, |ui| {
                        ui.set_width(row_width - 12.0);
                        ui.set_min_height(60.0);
                        ui.horizontal(|ui| {
                            if let Some(cover) = self.catalog_cover_textures.get(&item.topic_id) {
                                clicked_content |= ui
                                    .add(
                                        egui::Image::new((cover.id(), egui::vec2(52.0, 52.0)))
                                            .sense(egui::Sense::click()),
                                    )
                                    .clicked();
                            } else {
                                let (rect, cover_response) = ui.allocate_exact_size(
                                    egui::vec2(52.0, 52.0),
                                    egui::Sense::click(),
                                );
                                ui.painter().text(
                                    rect.center(),
                                    egui::Align2::CENTER_CENTER,
                                    "🎮",
                                    egui::FontId::proportional(16.0),
                                    ui.visuals().text_color(),
                                );
                                clicked_content |= cover_response.clicked();
                            }
                            let label_width = (ui.available_width() - 120.0).max(64.0);
                            ui.allocate_ui_with_layout(
                                egui::vec2(label_width, 54.0),
                                egui::Layout::top_down(egui::Align::Min),
                                |ui| {
                                    let name = ui.add(
                                        egui::Label::new(
                                            egui::RichText::new(&display_title).size(13.0).strong(),
                                        )
                                        .truncate()
                                        .sense(egui::Sense::click()),
                                    );
                                    clicked_content |= name.clicked();
                                    name.on_hover_text(if display_title == item.title {
                                        display_title.clone()
                                    } else {
                                        format!("{display_title}\n원본: {}", item.title)
                                    });
                                    let id = item.title_id.as_deref().unwrap_or("-");
                                    let meta = format!(
                                        "{id}   ·   {}   ·   {}",
                                        item.image_format, item.size
                                    );
                                    let meta_response = ui.add(
                                        egui::Label::new(
                                            egui::RichText::new(meta.clone())
                                                .size(11.0)
                                                .color(ui.visuals().weak_text_color()),
                                        )
                                        .truncate()
                                        .sense(egui::Sense::click()),
                                    );
                                    clicked_content |= meta_response.clicked();
                                    meta_response.on_hover_text(meta);
                                    if display_title != item.title {
                                        let original_title = ui.add(
                                            egui::Label::new(
                                                egui::RichText::new(&item.title)
                                                    .size(10.0)
                                                    .color(ui.visuals().weak_text_color()),
                                            )
                                            .truncate()
                                            .sense(egui::Sense::click()),
                                        );
                                        clicked_content |= original_title.clicked();
                                        original_title.on_hover_text(&item.title);
                                    }
                                },
                            );
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    let download_button = ui.button("다운로드");
                                    #[cfg(test)]
                                    if index == 0 && self.catalog_page == 0 {
                                        ui.ctx().data_mut(|data| {
                                            data.insert_temp(
                                                egui::Id::new(
                                                    "nsl_test_catalog_first_download_button",
                                                ),
                                                download_button.rect,
                                            )
                                        });
                                    }
                                    if download_button.clicked() {
                                        self.start_catalog_torrent(&item);
                                    }
                                },
                            );
                        });
                    });
                    // The background registers before child buttons, so they
                    // keep their own click action while card padding selects.
                    // Labels and cover images also have their own hit regions.
                    // Never turn the action button into a selection click.
                    remember_card_geometry(ui.ctx(), card_id, card.rect, _response.response.rect);
                    #[cfg(test)]
                    if selected {
                        ui.ctx().data_mut(|data| {
                            data.insert_temp(
                                egui::Id::new("nsl_test_catalog_selected_row"),
                                (item.topic_id.clone(), card.rect),
                            );
                        });
                    }
                    if card.clicked() || clicked_content {
                        self.catalog_selected = Some(item.topic_id.clone());
                        ui.ctx().request_repaint();
                    }
                    #[cfg(test)]
                    _ctx.data_mut(|data| {
                        data.insert_temp(egui::Id::new("nsl_test_catalog_last_row"), card.rect);
                        if index == 0 && self.catalog_page == 0 {
                            data.insert_temp(
                                egui::Id::new("nsl_test_catalog_first_row"),
                                card.rect,
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
        ui.ctx().data_mut(|data| {
            data.insert_temp(
                egui::Id::new("catalog_last_scroll_y"),
                _catalog_scroll.state.offset.y,
            );
            data.insert_temp(
                egui::Id::new("catalog_last_viewport_height"),
                _catalog_scroll.inner_rect.height(),
            );
        });
        self.start_cover_prefetch(cover_jobs);
    }
}
