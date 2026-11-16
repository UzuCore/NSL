# nsl_screenshot.py — 스크린샷 캐러셀 표시 (ScreenshotMixin) (PySide6)
from PySide6.QtCore import Qt, QUrl
from PySide6.QtGui import QPixmap, QImage, QDesktopServices

from nsl_theme import C
from nsl_detail import _ClickLabel


class ScreenshotMixin:

    # ── 스크린샷 로딩 ───────────────────────────────────────────────────
    def _load_screenshots(self, title_id: str):
        """
        스크린샷 로딩 — 깜빡임 방지 로직 포함.
        - title_id가 같고 viewport 폭 변화가 작으면 스킵
        - 워커 완료 후에야 기존 위젯을 제거 → 빈 화면 시간 최소화
        """
        if not title_id:
            self._clear_screenshots()
            self._current_ss_tid = None
            self._current_ss_width = 0
            return

        ss_dir = self.screenshots_dir / title_id
        if not ss_dir.exists():
            self._clear_screenshots()
            self._current_ss_tid = title_id
            self._current_ss_width = 0
            return

        image_files = (sorted(ss_dir.glob('*.jpg'))
                       + sorted(ss_dir.glob('*.png'))
                       + sorted(ss_dir.glob('*.jpeg')))
        if not image_files:
            self._clear_screenshots()
            self._current_ss_tid = title_id
            self._current_ss_width = 0
            return

        # 표시 폭 계산
        cw = self._ss_scroll.viewport().width()
        target_w = max(cw - 24, 200) if cw > 50 else 400

        # 같은 title_id + 폭 변화 < 16px이면 리로드 생략
        prev_tid   = getattr(self, '_current_ss_tid', None)
        prev_width = getattr(self, '_current_ss_width', 0)
        if (prev_tid == title_id
                and prev_width > 0
                and abs(target_w - prev_width) < 16
                and self._ss_inner_layout.count() > 0):
            return

        self._current_ss_tid   = title_id
        self._current_ss_width = target_w

        # 워커 시퀀스 — 같은 title_id에 워커가 여러 개 돌면 마지막 것만 반영
        self._ss_load_seq = getattr(self, '_ss_load_seq', 0) + 1
        my_seq = self._ss_load_seq

        def worker():
            try:
                processed = []
                for img_path in image_files:
                    # 다른 title_id로 바뀌었거나 신규 워커가 시작되면 종료
                    if (getattr(self, '_current_ss_tid', None) != title_id
                            or getattr(self, '_ss_load_seq', 0) != my_seq):
                        return
                    img = QImage(str(img_path))
                    if img.isNull():
                        continue
                    if img.width() > 0:
                        new_h = int(img.height() * target_w / img.width())
                        img = img.scaled(target_w, new_h,
                                         Qt.KeepAspectRatio,
                                         Qt.SmoothTransformation)
                    processed.append((img_path, img))
                self._invoke_main(
                    lambda: self._render_screenshots(
                        title_id, processed, my_seq))
            except Exception:
                pass

        self._run_in_thread(worker)

    def _clear_screenshots(self):
        """스크린샷 영역 비우기 (메인 스레드 전용)."""
        while self._ss_inner_layout.count() > 0:
            it = self._ss_inner_layout.takeAt(0)
            w = it.widget()
            if w:
                w.deleteLater()

    def _render_screenshots(self, title_id: str, processed_images,
                            my_seq: int = 0):
        # 다른 title_id로 바뀌었거나 신규 워커가 결과를 냈으면 폐기
        if (getattr(self, '_current_ss_tid', None) != title_id
                or getattr(self, '_ss_load_seq', 0) != my_seq):
            return
        # 새 이미지가 준비된 시점에 기존 위젯 제거
        self._clear_screenshots()
        for img_path, img in processed_images:
            try:
                pix = QPixmap.fromImage(img)
                lbl = _ClickLabel(
                    self._ss_inner,
                    on_click=lambda p=img_path: self._open_image(p))
                lbl.setPixmap(pix)
                lbl.setStyleSheet(
                    f'background-color: {C["surface"]}; '
                    'border: 1px solid #30363d;')
                lbl.setAlignment(Qt.AlignCenter)
                self._ss_inner_layout.addWidget(lbl)
            except Exception:
                pass

    def _open_image(self, path):
        """OS 기본 이미지 뷰어로 열기."""
        try:
            QDesktopServices.openUrl(QUrl.fromLocalFile(str(path)))
        except Exception:
            pass
