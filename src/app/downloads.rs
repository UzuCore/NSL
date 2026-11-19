//! Render torrent downloads, connection statistics, and deletion actions.

use crate::*;

impl NslApp {
    /// The 중지/재개 · 삭제 button cluster shown on every download row.
    pub(crate) fn download_row_buttons(
        &mut self,
        ui: &mut egui::Ui,
        key: &str,
        progress: &torrent::TorrentProgress,
    ) {
        let is_done = progress.finished || progress.error.is_some();
        if !is_done {
            if progress.paused {
                if ui.small_button("재개").clicked() {
                    if let Some(engine) = &self.torrent_engine {
                        engine.resume(key);
                        self.status = "다운로드 재개 요청됨".into();
                    }
                }
            } else if ui.small_button("중지").clicked() {
                if let Some(engine) = &self.torrent_engine {
                    engine.pause(key);
                    self.status = "다운로드 일시정지 요청됨".into();
                }
            }
        }
        // Always confirm before removing even a finished or failed download.
        let delete = ui.small_button("삭제");
        #[cfg(test)]
        ui.ctx().data_mut(|data| {
            data.insert_temp(
                egui::Id::new(("nsl_test_download_delete_button", key)),
                delete.rect,
            );
        });
        if delete.clicked() {
            self.download_delete_confirm = Some((key.to_owned(), progress.name.clone()));
        }
    }

    pub(crate) fn open_store_from_downloads(&mut self) {
        self.folder_filter = "CATALOG".into();
        self.current_page = 0;
        self.config.settings.insert(
            "last_tab_key".into(),
            serde_json::Value::String("CATALOG".into()),
        );
        let _ = database::save_config(&self.base_dir.join("config.json"), &self.config);
    }

    /// Search, transfer-state filters and aggregate progress live next to
    /// the downloads list, not in a toolbar above its first row.
    pub(crate) fn downloads_sidebar_ui(&mut self, ui: &mut egui::Ui) {
        ui.spacing_mut().item_spacing.y = 6.0;
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
                let search = ui.add_sized(
                    [ui.available_width(), 30.0],
                    egui::TextEdit::singleline(&mut self.download_search)
                        .id(egui::Id::new("download_search"))
                        .min_size(egui::vec2(0.0, 30.0))
                        .vertical_align(egui::Align::Center)
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
                    self.download_page = 0;
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
                    self.download_page = 0;
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
                let results = ui.label(format!(
                    "검색 결과 {}개",
                    common::format_count(count as i64)
                ));
                #[cfg(test)]
                ui.ctx().data_mut(|data| {
                    data.insert_temp(
                        egui::Id::new("nsl_test_download_result_count"),
                        results.rect,
                    );
                });
                #[cfg(not(test))]
                let _ = results;
                ui.add_space(3.0);
                let reset = ui.add_sized(
                    [ui.available_width(), 28.0],
                    egui::Button::new("검색·분류 초기화"),
                );
                #[cfg(test)]
                ui.ctx().data_mut(|data| {
                    data.insert_temp(egui::Id::new("nsl_test_download_reset_button"), reset.rect);
                });
                if reset.clicked() {
                    self.download_search.clear();
                    self.download_status_filter = "all".into();
                    self.download_page = 0;
                    self.download_scroll_reset = true;
                    ui.ctx().request_repaint();
                }
                ui.separator();
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
                if ui
                    .add_sized(
                        [ui.available_width(), 29.0],
                        egui::Button::new("스토어로 이동"),
                    )
                    .clicked()
                {
                    self.open_store_from_downloads();
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

    /// Consistent empty states for an idle queue and an unmatched filter.
    /// Center a compact card in the content area instead of leaving a small
    /// single-line panel at the top of an otherwise blank screen.
    pub(crate) fn downloads_empty_state_ui(&mut self, ui: &mut egui::Ui, filtered: bool) {
        let card_width = (ui.available_width() - 16.0).clamp(0.0, 480.0);
        let card_height = 267.0;
        ui.add_space(((ui.available_height() - card_height) * 0.5).max(12.0));
        let side_padding = ((ui.available_width() - card_width) * 0.5).max(0.0);
        ui.horizontal(|ui| {
            ui.add_space(side_padding);
            let card = egui::Frame::none()
                .fill(ui.visuals().faint_bg_color)
                .stroke(egui::Stroke::new(
                    1.0_f32,
                    ui.visuals().widgets.noninteractive.bg_stroke.color,
                ))
                .rounding(egui::Rounding::same(9.0))
                .inner_margin(egui::Margin::symmetric(24.0, 23.0))
                .show(ui, |ui| {
                    ui.set_width((card_width - 48.0).max(0.0));
                    ui.vertical_centered(|ui| {
                        let (icon_rect, _) = ui.allocate_exact_size(
                            egui::vec2(62.0, 62.0),
                            egui::Sense::hover(),
                        );
                        let accent = ui.visuals().hyperlink_color;
                        ui.painter().circle_filled(
                            icon_rect.center(),
                            28.0,
                            ui.visuals().widgets.inactive.weak_bg_fill,
                        );
                        ui.painter().circle_stroke(
                            icon_rect.center(),
                            28.0,
                            egui::Stroke::new(1.0_f32, accent),
                        );
                        ui.painter().text(
                            icon_rect.center(),
                            egui::Align2::CENTER_CENTER,
                            if filtered { "?" } else { "↓" },
                            egui::FontId::proportional(29.0),
                            accent,
                        );
                        ui.add_space(12.0);
                        let title = ui.label(
                            egui::RichText::new(if filtered {
                                "검색 결과가 없습니다"
                            } else {
                                "다운로드 작업이 없습니다"
                            })
                            .size(18.0)
                            .strong(),
                        );
                        #[cfg(test)]
                        ui.ctx().data_mut(|data| {
                            data.insert_temp(
                                egui::Id::new("nsl_test_download_empty_heading"),
                                title.rect,
                            );
                        });
                        #[cfg(not(test))]
                        let _ = title;
                        ui.add_space(7.0);
                        ui.add(
                            egui::Label::new(
                                egui::RichText::new(if filtered {
                                    "검색어 또는 진행 상태 필터를 변경해 다시 찾아보세요."
                                } else {
                                    "스토어에서 게임을 선택하고 다운로드를 시작하면\n진행 상황이 여기에 표시됩니다."
                                })
                                .color(ui.visuals().weak_text_color()),
                            )
                            .halign(egui::Align::Center)
                            .wrap(),
                        );
                        ui.add_space(17.0);
                        let action = ui.add_sized(
                            [195.0_f32.min(ui.available_width()), 34.0],
                            egui::Button::new(if filtered {
                                "검색·분류 초기화"
                            } else {
                                "스토어 둘러보기"
                            }),
                        );
                        #[cfg(test)]
                        ui.ctx().data_mut(|data| {
                            data.insert_temp(
                                egui::Id::new("nsl_test_download_empty_action"),
                                action.rect,
                            );
                        });
                        if action.clicked() {
                            if filtered {
                                self.download_search.clear();
                                self.download_status_filter = "all".into();
                                self.download_page = 0;
                                self.download_scroll_reset = true;
                                ui.ctx().request_repaint();
                            } else {
                                self.open_store_from_downloads();
                            }
                        }
                        ui.add_space(14.0);
                        ui.label(
                            egui::RichText::new(if filtered {
                                "오른쪽 패널에서도 검색 조건을 변경할 수 있습니다."
                            } else {
                                "진행률  ·  전송 속도  ·  일시정지  ·  완료"
                            })
                            .small()
                            .color(ui.visuals().weak_text_color()),
                        );
                    });
                });
            #[cfg(test)]
            ui.ctx().data_mut(|data| {
                data.insert_temp(
                    egui::Id::new("nsl_test_download_empty_card"),
                    card.response.rect,
                );
            });
            #[cfg(not(test))]
            let _ = card;
        });
    }

    /// Dedicated downloads tab: the center is reserved for torrent rows.
    pub(crate) fn downloads_panel_ui(&mut self, ui: &mut egui::Ui) {
        if self.torrent_progress.is_empty() {
            self.downloads_empty_state_ui(ui, false);
            return;
        }
        let query = self.download_search.trim().to_lowercase();
        let matching_count = self
            .torrent_progress
            .iter()
            .filter(|(key, progress)| {
                download_matches_filters(key, progress, &query, &self.download_status_filter)
            })
            .count();
        let (pagination_rect, next_button_rect, page_changed) =
            management_pagination_ui(ui, &mut self.download_page, matching_count);
        if page_changed {
            self.download_scroll_reset = true;
        }
        #[cfg(test)]
        ui.ctx().data_mut(|data| {
            data.insert_temp(
                egui::Id::new("nsl_test_download_pagination_rect"),
                pagination_rect,
            );
            data.insert_temp(
                egui::Id::new("nsl_test_download_pagination_next"),
                next_button_rect,
            );
        });
        #[cfg(not(test))]
        let _ = (pagination_rect, next_button_rect);
        let entries: Vec<_> = self
            .torrent_progress
            .iter()
            .filter(|(key, progress)| {
                download_matches_filters(key, progress, &query, &self.download_status_filter)
            })
            .skip(self.download_page * MANAGEMENT_PAGE_SIZE)
            .take(MANAGEMENT_PAGE_SIZE)
            .cloned()
            .collect();
        #[cfg(test)]
        ui.ctx().data_mut(|data| {
            data.insert_temp(
                egui::Id::new("nsl_test_download_visible_items"),
                entries.len(),
            )
        });
        if entries.is_empty() {
            self.downloads_empty_state_ui(ui, true);
            return;
        }
        ui.add_space(6.0);
        let mut download_scroll = egui::ScrollArea::vertical()
            .id_salt("downloads_rows")
            .auto_shrink([false, false]);
        if std::mem::take(&mut self.download_scroll_reset) {
            download_scroll = download_scroll.vertical_scroll_offset(0.0);
        }
        download_scroll.show(ui, |ui| {
            for (key, progress) in &entries {
                let row_width = ui.available_width();
                let card_id = ui.make_persistent_id(("download-card", key));
                let card = selectable_card_background(ui, card_id, row_width, 130.0);
                let selected = self.download_selected.as_deref() == Some(key.as_str());
                let frame = library_section_frame(ui)
                    .fill(if selected {
                        ui.visuals().selection.bg_fill
                    } else {
                        ui.visuals().faint_bg_color
                    })
                    .stroke(egui::Stroke::new(
                        1.0_f32,
                        if selected {
                            ui.visuals().hyperlink_color
                        } else {
                            ui.visuals().widgets.noninteractive.bg_stroke.color
                        },
                    ));
                let mut clicked_content = false;
                let _row = frame.show(ui, |ui| {
                    ui.set_width((row_width - 20.0).max(200.0));
                    let label = if progress.name.is_empty() {
                        key.as_str()
                    } else {
                        &progress.name
                    };
                    let compact = ui.available_width() < 440.0;
                    if compact {
                        let label_response = ui.add(
                            egui::Label::new(egui::RichText::new(label).size(13.0).strong())
                                .truncate()
                                .sense(egui::Sense::click()),
                        );
                        #[cfg(test)]
                        ui.ctx().data_mut(|data| {
                            data.insert_temp(
                                egui::Id::new(("nsl_test_download_name", key)),
                                label_response.rect,
                            );
                        });
                        clicked_content |= label_response.clicked();
                        label_response.on_hover_text(label);
                        ui.horizontal(|ui| self.download_row_buttons(ui, key, progress));
                    } else {
                        ui.horizontal(|ui| {
                            let label_width = (ui.available_width() - 205.0).max(96.0);
                            // add_sized centers even a short child Label in the
                            // wide name column. Allocate the width but lay out
                            // from the left, as in the torrent file picker.
                            let label_response = ui
                                .allocate_ui_with_layout(
                                    egui::vec2(label_width, 22.0),
                                    egui::Layout::left_to_right(egui::Align::Center),
                                    |ui| {
                                        ui.add(
                                            egui::Label::new(
                                                egui::RichText::new(label).size(13.0).strong(),
                                            )
                                            .truncate()
                                            .sense(egui::Sense::click()),
                                        )
                                    },
                                )
                                .inner;
                            #[cfg(test)]
                            ui.ctx().data_mut(|data| {
                                data.insert_temp(
                                    egui::Id::new(("nsl_test_download_name", key)),
                                    label_response.rect,
                                );
                            });
                            clicked_content |= label_response.clicked();
                            label_response.on_hover_text(label);
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
                            let bar = ui.add(
                                egui::ProgressBar::new(1.0)
                                    .desired_height(17.0)
                                    .desired_width(ui.available_width())
                                    .fill(egui::Color32::from_rgb(46, 160, 67))
                                    .text(format!(
                                        "완료 {}",
                                        common::format_size(progress.total_bytes)
                                    )),
                            );
                            clicked_content |= bar.interact(egui::Sense::click()).clicked();
                        } else {
                            let bar = ui.add(
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
                            clicked_content |= bar.interact(egui::Sense::click()).clicked();
                        }
                    } else {
                        clicked_content |= ui
                            .add(
                                egui::Label::new(
                                    egui::RichText::new("크기 확인 중… (메타데이터 받는 중)")
                                        .small()
                                        .color(ui.visuals().weak_text_color()),
                                )
                                .sense(egui::Sense::click()),
                            )
                            .clicked();
                    }
                    ui.horizontal(|ui| {
                        clicked_content |= ui
                            .add(
                                egui::Label::new(
                                    egui::RichText::new(&progress.state)
                                        .small()
                                        .color(ui.visuals().weak_text_color()),
                                )
                                .sense(egui::Sense::click()),
                            )
                            .clicked();
                        let connections = ui.add(
                            egui::Label::new(
                                egui::RichText::new(download_connection_label(progress))
                                    .small()
                                    .color(ui.visuals().weak_text_color()),
                            )
                            .sense(egui::Sense::click()),
                        );
                        #[cfg(test)]
                        ui.ctx().data_mut(|data| {
                            data.insert_temp(
                                egui::Id::new(("nsl_test_download_connections", key)),
                                connections.rect,
                            );
                        });
                        clicked_content |= connections.clicked();
                        connections.on_hover_text(
                            "현재 연결된 시드와 비시더 피어 수 · 전체 토렌트 네트워크 수 아님 · 연결 정보가 없으면 —",
                        );
                        if let Some(error) = &progress.error {
                            let error_response = ui.add(
                                egui::Label::new(
                                    egui::RichText::new(error)
                                        .small()
                                        .color(egui::Color32::LIGHT_RED),
                                )
                                .truncate()
                                .sense(egui::Sense::click()),
                            );
                            clicked_content |= error_response.clicked();
                            error_response.on_hover_text(error);
                        }
                    });
                });
                remember_card_geometry(ui.ctx(), card_id, card.rect, _row.response.rect);
                if card.clicked() || clicked_content {
                    self.download_selected = Some(key.clone());
                    ui.ctx().request_repaint();
                }
                #[cfg(test)]
                ui.ctx().data_mut(|data| {
                    data.insert_temp(egui::Id::new("nsl_test_download_last_row"), card.rect);
                    if key == &entries[0].0 {
                        data.insert_temp(egui::Id::new("nsl_test_download_first_row"), card.rect);
                    }
                });
            }
        });
    }
}
