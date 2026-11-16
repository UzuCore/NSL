# nsl_screenshots_dl.py — 스크린샷 다운로드 + 더미 데이터 정리 (PySide6)
import io
import json
import shutil
import threading
import urllib.request
from concurrent.futures import ThreadPoolExecutor, as_completed

from PIL import Image as _PILImage
from PySide6.QtWidgets import QMessageBox

import nsl_database as db
import nsl_titledb as titledb_module
from nsl_common import get_ssl_context

_SSL_CTX = get_ssl_context()


class ScreenshotsDownloadMixin:

    # ── 스크린샷 다운로드 ──────────────────────────────────────────────
    def _get_screenshots(self, title_id):
        conn = db.get_connection()
        row  = conn.execute(
            'SELECT screenshots FROM title_db WHERE title_id=?',
            (title_id,)).fetchone()
        conn.close()
        if not row or not row['screenshots']:
            return []
        try:
            return json.loads(row['screenshots'])
        except Exception:
            return []

    def _download_screenshots(self, title_ids=None):
        if title_ids is None:
            title_ids = [x.get('title_id') for x in self._library
                         if x.get('content_type') == 'base'
                         and x.get('title_id')]

        if not title_ids:
            self._set_status('다운로드할 스크린샷이 없습니다')
            return

        self._set_status(f'스크린샷 다운로드 시작 ({len(title_ids)}개)…')
        self._show_log()
        self._log(
            f'\n🚀 스크린샷 다운로드 시작 — 대상 {len(title_ids)}개', 'info')
        self._log(f'📂 저장 경로: {self.screenshots_dir}', 'dim')

        def download_title(tid):
            urls = self._get_screenshots(tid)
            if not urls:
                return 0
            try:
                tid_dir = self.screenshots_dir / tid
                tid_dir.mkdir(parents=True, exist_ok=True)
            except Exception as e:
                err = str(e); t = tid
                self._invoke_main(lambda:
                    self._log(f'   ❌ [{t}] 저장 폴더 생성 실패: {err}',
                              'err'))
                return 0

            missing_urls = []
            for i, url in enumerate(urls, 1):
                if not url or not isinstance(url, str):
                    continue
                ext  = url.split('.')[-1].split('?')[0]
                ext  = ext if len(ext) <= 4 else 'jpg'
                path = tid_dir / f'{i:03d}.{ext}'
                if not path.exists():
                    missing_urls.append((i, url, path))

            if not missing_urls:
                return 0

            t = tid; n = len(missing_urls)
            self._invoke_main(lambda:
                self._log(f'   ▶ [{t}] 다운로드 중… ({n}장)', 'info'))

            saved = 0
            for i, url, path in missing_urls:
                try:
                    req = urllib.request.Request(
                        url, headers=titledb_module._HEADERS)
                    with urllib.request.urlopen(
                            req, context=_SSL_CTX, timeout=15) as resp:
                        img_data = resp.read()
                    if len(img_data) < 1000:
                        continue
                    img = _PILImage.open(io.BytesIO(img_data))
                    if img.mode in ('RGBA', 'P'):
                        img = img.convert('RGB')
                    w = 800
                    img = img.resize(
                        (w, int(img.height * w / img.width)),
                        _PILImage.Resampling.LANCZOS)
                    img.save(path)
                    saved += 1
                except Exception:
                    pass
            return saved

        def worker():
            self._ss_downloading = True
            try:
                total      = len(title_ids)
                downloaded = 0
                done       = 0
                lock       = threading.Lock()

                with ThreadPoolExecutor(max_workers=6) as pool:
                    futures = {pool.submit(download_title, tid): tid
                               for tid in title_ids if tid}
                    for future in as_completed(futures):
                        try:
                            saved = future.result()
                            with lock:
                                downloaded += saved
                                done += 1
                            d, t, dl = done, total, downloaded
                            self._invoke_main(lambda d=d, t=t, dl=dl:
                                self._set_status(
                                    f'스크린샷 다운로드 중… '
                                    f'{d}/{t} ({dl}장)'))
                        except Exception:
                            with lock:
                                done += 1

                msg = f'🏁 스크린샷 다운로드 완료 ({downloaded}장 저장)'
                self._log(f'\n{msg}', 'ok')
                self._invoke_main(lambda: self._set_status(msg))
            finally:
                self._ss_downloading = False

        self._run_in_thread(worker)

    def _download_selected_screenshots(self):
        sel = self.tree.selectionModel().selectedRows()
        if not sel:
            QMessageBox.information(self, '알림', '게임을 먼저 선택해 주세요.')
            return
        idxs = [s.row() for s in sel]
        title_ids = [self._filtered[i].get('title_id') for i in idxs
                     if i < len(self._filtered)
                     and self._filtered[i].get('title_id')]
        self._download_screenshots(title_ids)

    def _auto_prefetch_screenshots(self):
        """스캔 후 자동 호출 — 캐시에 없는 스크린샷만 다운로드."""
        missing = []
        for x in self._library:
            if x.get('content_type') != 'base':
                continue
            if x.get('screenshots', '[]') == '[]':
                continue
            tid = x.get('title_id')
            if not tid:
                continue
            tid_dir = self.screenshots_dir / tid
            if tid_dir.exists() and any(tid_dir.iterdir()):
                continue
            missing.append(tid)
        if missing:
            self._log(
                f'📸 새 스크린샷 다운로드 대상: {len(missing)}개', 'dim')
            self._download_screenshots(missing)
        else:
            self._log('📸 스크린샷 자동 다운로드: 모두 캐시됨', 'dim')

    # ── 더미 데이터 정리 ───────────────────────────────────────────────
    def _cleanup_orphans(self):
        ans = QMessageBox.question(
            self, '더미 데이터 정리',
            '현재 목록에 없는(보유하지 않은) 게임의\n'
            '아이콘과 스크린샷 캐시를 삭제하시겠습니까?',
            QMessageBox.Yes | QMessageBox.No)
        if ans != QMessageBox.Yes:
            return

        self._set_status('더미 데이터 파일 정리 중…')

        def worker():
            self._cleanup_running = True
            try:
                conn = db.get_connection()
                rows = conn.execute(
                    'SELECT DISTINCT title_id FROM file_contents '
                    'WHERE title_id IS NOT NULL').fetchall()
                conn.close()
                active    = {r['title_id'].upper() for r in rows}
                del_icons = del_ss = 0

                if self.icons_dir.exists():
                    for f in self.icons_dir.glob('*.png'):
                        if f.stem.upper() not in active:
                            try:
                                f.unlink()
                                del_icons += 1
                            except Exception:
                                pass

                if self.screenshots_dir.exists():
                    for d in self.screenshots_dir.iterdir():
                        if d.is_dir() and d.name.upper() not in active:
                            try:
                                shutil.rmtree(d)
                                del_ss += 1
                            except Exception:
                                pass

                di, dss = del_icons, del_ss
                self._invoke_main(lambda: (
                    QMessageBox.information(
                        self, '정리 완료',
                        '더미 데이터 정리가 완료되었습니다.\n\n'
                        f'삭제된 아이콘: {di}개\n'
                        f'삭제된 스크린샷 폴더: {dss}개'),
                    self._set_status(
                        f'정리 완료 (아이콘 {di}개, '
                        f'스크린샷 {dss}폴더 삭제)'),
                ))
            finally:
                self._cleanup_running = False

        self._run_in_thread(worker)
