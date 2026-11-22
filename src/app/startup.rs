//! Initialize the application state and persistent user configuration.

use crate::*;

impl NslApp {
    pub(crate) fn new() -> Result<Self> {
        Self::with_base_dir(common::base_dir())
    }
    pub(crate) fn with_base_dir(base_dir: PathBuf) -> Result<Self> {
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
            batch_rescan_pending: false,
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
            update_check_automatic: false,
            update_info: None,
            update_prepare_result: None,
            update_ready: None,
            catalog_state: catalog::CatalogState::default(),
            catalog_load_result: None,
            catalog_result: None,
            catalog_search: String::new(),
            catalog_search_committed: String::new(),
            catalog_search_cache: None,
            catalog_user_filter_cache: None,
            catalog_matching_indices: Vec::new(),
            catalog_search_pending: None,
            catalog_page: 0,
            catalog_genre_filter: "all".into(),
            catalog_format_filter: "all".into(),
            catalog_language_filter: "all".into(),
            catalog_scroll_reset: false,
            catalog_selected: None,
            catalog_owned: HashMap::new(),
            catalog_title_details: HashMap::new(),
            catalog_title_load_result: None,
            catalog_title_metadata_loaded: false,
            catalog_cover_dir: base_dir.join("icons").join("store"),
            catalog_cover_textures: HashMap::new(),
            catalog_cover_lru: VecDeque::new(),
            catalog_cover_missing: HashSet::new(),
            catalog_cover_retry_after: HashMap::new(),
            catalog_cover_queue: None,
            torrent_engine: None,
            torrent_progress: Vec::new(),
            catalog_torrent_dialog: None,
            update_batch: None,
            dlc_batch: None,
            download_selected: None,
            download_search: String::new(),
            download_status_filter: "all".into(),
            download_page: 0,
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
        // Runtime torrent keys are different after restart. Restore our
        // independent, crash-safe post-download manifest; reconnect by the
        // torrent's original output directory once librqbit is ready.
        match update_manifest::load(&app.base_dir) {
            Ok(Some(batch)) => {
                let count = batch.rows.len();
                if batch.kind == update_batch::BatchKind::Dlc {
                    app.dlc_batch = Some(batch);
                } else {
                    app.update_batch = Some(batch);
                }
                let restored_kind = app
                    .dlc_batch
                    .as_ref()
                    .map(|_| update_batch::BatchKind::Dlc)
                    .unwrap_or(update_batch::BatchKind::Update);
                let _ = app.log_sender.send(format!(
                    "ℹ {} 작업 {count}개 복원 · 다운로드 상태 재연결 중",
                    if restored_kind == update_batch::BatchKind::Dlc {
                        "일괄 DLC"
                    } else {
                        "일괄 업데이트"
                    }
                ));
            }
            Ok(None) => {}
            Err(error) => {
                let _ = app.log_sender.send(format!(
                    "⚠ 일괄 업데이트 작업 기록을 읽지 못했습니다: {error} · 원본 유지"
                ));
            }
        }
        // Start-up checks run off the UI thread and only offer an update;
        // installing/restarting always requires the user's explicit approval.
        if !cfg!(debug_assertions)
            && app
                .config
                .settings
                .get("auto_update_check")
                .and_then(|value| value.as_str())
                != Some("0")
        {
            app.start_update_check(true);
        }
        Ok(app)
    }
}
