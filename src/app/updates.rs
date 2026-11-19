//! Check and prepare verified application updates.

use crate::*;

impl NslApp {
    pub(crate) fn start_update_check(&mut self, automatic: bool) {
        if self.update_check_result.is_some() {
            if !automatic {
                self.update_check_automatic = false;
                self.status = "업데이트 확인 중".into();
            }
            return;
        }
        self.update_check_automatic = automatic;
        let (tx, rx) = mpsc::channel();
        self.update_check_result = Some(rx);
        if !automatic {
            self.status = "업데이트 확인 중".into();
        }
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
    pub(crate) fn start_update_download(&mut self, url: String, commit: String) {
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
}
