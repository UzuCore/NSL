# nsl_icon.py — 아이콘 추출/로딩/캐시 (IconMixin) (PySide6)
import io
import os
import re
import tempfile
import threading
import urllib.request
from concurrent.futures import ThreadPoolExecutor, as_completed
from pathlib import Path

from PySide6.QtCore import Qt
from PySide6.QtGui import QPixmap
from PySide6.QtWidgets import QMessageBox

import nsl_scanner as scanner
from nsl_theme import C, _FS, register_themed
from nsl_common import get_ssl_context

_SSL_CTX = get_ssl_context()


class IconMixin:

    # ── 아이콘 파일 후보 탐색 ──────────────────────────────────────────
    def _find_icon_file(self, files, part0_dir):
        """추출된 디렉터리에서 아이콘으로 쓸만한 이미지 파일을 찾는다."""
        img_exts = ('.jpg', '.jpeg', '.png', '.bmp', '.dat')
        img_keys = ('icon', 'logo', 'banner', 'picture', 'splash')
        for fn in files:
            lower = fn.lower()
            if (any(k in lower for k in img_keys)
                    and lower.endswith(img_exts)):
                return os.path.join(part0_dir, fn)
        for fn in files:
            if fn.lower().endswith(img_exts):
                return os.path.join(part0_dir, fn)
        return None

    # ── 아이콘 추출 ────────────────────────────────────────────────────
    def _extract_icon(self, tid, item):
        """
        아이콘을 추출/다운로드하여 self.icons_dir/{tid}.png 에 저장.
        성공 시 캐시 파일 경로(Path)를 반환, 실패 시 None.

        주의: 워커 스레드에서 호출될 수 있다.
        QPixmap 객체는 여기서 만들지 않고 디스크에 PNG만 저장한다.
        """
        try:
            from PIL import Image as PILImage

            cache_file = self.icons_dir / f'{tid}.png'
            if cache_file.exists():
                return cache_file

            icon_url = item.get('icon_url') or ''

            # 1) URL이 있으면 우선 다운로드 시도 (분석 실패 대비)
            if icon_url:
                try:
                    req = urllib.request.Request(
                        icon_url, headers={'User-Agent': 'Mozilla/5.0'})
                    with urllib.request.urlopen(
                            req, context=_SSL_CTX, timeout=15) as resp:
                        data = resp.read()
                    if len(data) > 1000:
                        img = (PILImage.open(io.BytesIO(data))
                               .resize((120, 120)))
                        img.save(str(cache_file))
                        self._log(
                            f'   ✅ 아이콘 다운로드 성공: {tid}')
                        return cache_file
                except Exception as e:
                    self._log(f'   ⚠ 아이콘 다운로드 실패: {e}', 'warn')
                    # 실패 → 로컬 파일 분석으로 진행

            # 2) 로컬 파일 내부에서 추출 시도
            engine         = scanner.find_nstool()
            game_path      = item.get('path', '')
            prod_keys_path = scanner._find_prod_keys()

            if not os.path.isfile(game_path):
                self._log(
                    f'   ❌ 게임 파일을 찾을 수 없습니다: {game_path}',
                    'err')
                return None
            if not prod_keys_path:
                self._log('   ❌ 키 파일을 찾을 수 없습니다.', 'err')
                return None

            file_ext = os.path.splitext(game_path)[1].lower()
            ks = ['--keyset', prod_keys_path]

            # 2-A) XCI/XCZ — 컨테이너에서 콘텐츠 후보를 추출
            if file_ext in ('.xci', '.xcz'):
                with tempfile.TemporaryDirectory(
                        prefix='slm_xci_icon_') as tmpdir:
                    # cnmt는 무시하고 control 후보만 사용
                    extracted = scanner._extract_cnmt_ncas_from_xci(
                        game_path, tmpdir, self._log, categorized=True)
                    control_ncas = [Path(p)
                                    for p in extracted.get('control', [])]

                    # 폴백: control 후보가 비면 cnmt 외 파일 중 15MB 이하 사용
                    if not control_ncas:
                        all_extracted = (extracted.get('control', [])
                                         + extracted.get('cnmt', []))
                        control_ncas = [
                            Path(p) for p in all_extracted
                            if not p.lower().endswith(('.cnmt.nca',
                                                       '.cnmt.ncz'))
                            and os.path.getsize(p) < 15 * 1024 * 1024
                        ]

                    if not control_ncas:
                        self._log(
                            '   ⚠ 콘텐츠 후보를 찾지 못했습니다.', 'warn')
                        return None

                    # 작은 파일부터 시도 (Control 콘텐츠는 보통 가장 작음)
                    control_ncas.sort(key=lambda p: p.stat().st_size)

                    # base TID 일치 후보를 우선 시도
                    base_tid = (tid or '').upper()
                    if (not base_tid.endswith('000')
                            and len(base_tid) >= 13):
                        base_tid = base_tid[:13] + '000'

                    base_tid_match  = []
                    other_candidates = []

                    nstool_re = re.compile(
                        r'(?:TitleId|ProgramId|ProgId|Title\s*Id|'
                        r'ApplicationId|Application\s*Id)'
                        r'\s*[:\s]+(?:0[xX])?([0-9A-Fa-f]{16})',
                        re.IGNORECASE)

                    for nca_p in control_ncas:
                        out = scanner._run_nstool(
                            engine, str(nca_p), extra_args=ks)
                        if not out:
                            other_candidates.append(nca_p)
                            continue
                        nca_tids = {m.group(1).upper()
                                    for m in nstool_re.finditer(out)}
                        if base_tid and base_tid in nca_tids:
                            base_tid_match.append(nca_p)
                        else:
                            other_candidates.append(nca_p)

                    try_order = base_tid_match + other_candidates
                    if not try_order:
                        try_order = control_ncas

                    # 후보 순회 → 콘텐츠 추출 → 아이콘 검사 (성공 시 즉시 반환)
                    for idx, target in enumerate(try_order, 1):
                        part0 = Path(tmpdir) / f'part0_{idx}_{target.stem}'
                        ctr_key = scanner._extract_aes_key_and_part0(
                            engine, str(target), ks, str(part0))

                        if not ctr_key or not part0.exists():
                            continue

                        try:
                            files_in_part0 = os.listdir(part0)
                        except Exception:
                            continue

                        # 컨트롤 메타가 있어야 진짜 Control 콘텐츠
                        if not any(f.lower().endswith('.nacp')
                                   for f in files_in_part0):
                            continue

                        icon_file = self._find_icon_file(
                            files_in_part0, str(part0))
                        if (icon_file
                                and os.path.getsize(icon_file) > 1000):
                            img = (PILImage.open(icon_file)
                                   .resize((120, 120)))
                            img.save(str(cache_file))
                            self._log(
                                f'   ✅ 아이콘 추출 성공: {tid}')
                            return cache_file

                    self._log(
                        '   ⚠ 모든 후보에서 아이콘을 추출하지 못했습니다 '
                        '(키 파일 누락/구버전 가능)', 'warn')
                return None

            # 2-B) NSP/NSZ — PFS0 인덱스에서 작은 콘텐츠 후보를 차례로 시도
            with open(game_path, 'rb') as f:
                result = scanner._pfs0_index(f, file_ext)
                if not result:
                    return None
                entries, hdr_end = result
                candidates = sorted(
                    [(off, sz, name) for off, sz, name in entries
                     if name.endswith('.nca')
                     and not name.endswith('.cnmt.nca') and sz > 1000],
                    key=lambda x: x[1])

                for off, sz, name in candidates:
                    f.seek(hdr_end + off)
                    nca_data = f.read(sz)
                    with tempfile.TemporaryDirectory(
                            prefix='slm_icon_') as tmpdir:
                        nca_path = os.path.join(tmpdir, name)
                        with open(nca_path, 'wb') as wf:
                            wf.write(nca_data)
                        part0 = os.path.join(tmpdir, 'part0')
                        ctr_key = scanner._extract_aes_key_and_part0(
                            engine, nca_path, ks, part0)
                        if not ctr_key:
                            continue
                        icon_file = self._find_icon_file(
                            os.listdir(part0), part0)
                        if (icon_file
                                and os.path.getsize(icon_file) > 1000):
                            img = (PILImage.open(icon_file)
                                   .resize((120, 120)))
                            img.save(str(cache_file))
                            self._log(f'   ✅ 아이콘 추출 성공: {tid}')
                            return cache_file
            return None

        except ImportError as e:
            self._log(f'❌ 아이콘 처리 모듈 누락: {e}', 'err')
            return None
        except Exception as e:
            self._log(f'❌ 아이콘 처리 중 오류: {e}', 'err')
            return None

    # ── 비동기 로딩 ────────────────────────────────────────────────────
    def _load_icon_async(self, tid, item):
        """
        상세 패널에 아이콘 표시.
        - 메모리 캐시(QPixmap) 히트 → 즉시 표시
        - 디스크 캐시 파일 발견 → 메인 스레드에서 QPixmap 로드
        - 둘 다 없으면 플레이스홀더 유지
        """
        if tid in self._icon_cache:
            self._icon_cache.move_to_end(tid)
            self._set_icon(tid, self._icon_cache[tid])
            return

        def worker():
            cache_file = self.icons_dir / f'{tid}.png'
            if cache_file.exists():
                path_str = str(cache_file)
                self._invoke_main(
                    lambda: self._set_icon_from_file(tid, path_str))
            else:
                self._invoke_main(lambda: self._set_icon(tid, None))

        self._run_in_thread(worker)

    def _set_icon_from_file(self, tid: str, path: str):
        """메인 스레드 전용: 파일 경로 → QPixmap 로드 후 표시."""
        try:
            pix = QPixmap(path)
            if pix.isNull():
                self._set_icon(tid, None)
                return
            pix = pix.scaled(120, 120, Qt.KeepAspectRatio,
                             Qt.SmoothTransformation)
            self._set_icon(tid, pix)
        except Exception:
            self._set_icon(tid, None)

    def _set_icon(self, tid: str, pix):
        """메인 스레드 전용: 아이콘 라벨에 QPixmap 표시."""
        if hasattr(self, '_icon_jobs'):
            self._icon_jobs.discard(tid)
        if pix is not None and not pix.isNull():
            if tid in self._icon_cache:
                self._icon_cache.move_to_end(tid)
            else:
                self._icon_cache[tid] = pix
                if len(self._icon_cache) > self._ICON_CACHE_MAX:
                    self._icon_cache.popitem(last=False)
            self._icon_label.setPixmap(self._icon_cache[tid])
            self._icon_label.setText('')
        else:
            self._icon_label.clear()
            self._icon_label.setText('🎮')
            # 플레이스홀더 스타일 재등록 → 테마 변경에도 자동 추종
            register_themed(self._icon_label, lambda fs=_FS: (
                f'background-color: {C["surface2"]}; '
                f'color: {C["text_dim"]}; font-size: {fs+20}pt;'
            ))
            self._icon_label.setAlignment(Qt.AlignCenter)

    # ── 일괄 추출 (자동) ───────────────────────────────────────────────
    def _prefetch_icons_after_scan(self):
        """스캔 직후 자동 호출 — 캐시에 없는 아이콘만 추출."""
        if not scanner._find_prod_keys():
            self._reset_scan_btn()
            return

        missing = [item for item in self._library
                   if item.get('content_type') == 'base'
                   and item.get('title_id') and item.get('path')
                   and os.path.isfile(item['path'])
                   and not (self.icons_dir
                            / f'{item["title_id"]}.png').exists()]

        if not missing:
            self._set_status('아이콘 자동 추출: 모두 캐시됨')
            self._log('🖼 아이콘 자동 추출: 모두 캐시됨', 'ok')
            self._reset_scan_btn()
            return

        total = len(missing)
        self._icon_extracting = True
        self._set_status(f'아이콘 추출 중… 0/{total}')
        self._log(f'🚀 아이콘 자동 추출 시작 — 대상 {total}개', 'info')

        def worker():
            ok = fail = 0
            done = 0
            lock = threading.Lock()

            def _extract_one(item):
                # 취소 신호 체크
                if not self._icon_extracting:
                    return None
                tid = item['title_id']
                if (self.icons_dir / f'{tid}.png').exists():
                    return True
                return bool(self._extract_icon(tid, item))

            # 4 worker — 각 작업이 nstool subprocess를 띄우므로
            # CPU/디스크가 thrash 되지 않도록 보수적으로.
            with ThreadPoolExecutor(max_workers=4) as pool:
                futures = {pool.submit(_extract_one, it): it
                           for it in missing}
                for future in as_completed(futures):
                    if not self._icon_extracting:
                        # 취소 — 남은 future는 cancel 시도
                        for f in futures:
                            f.cancel()
                        self._invoke_main(
                            lambda: self._set_status('아이콘 추출 중단됨'))
                        break
                    try:
                        res = future.result()
                    except Exception:
                        res = False
                    with lock:
                        done += 1
                        if res:
                            ok += 1
                        elif res is False:
                            fail += 1
                        # res is None: 취소된 작업 — 카운트 안 함
                        d, o, fn = done, ok, fail
                    self._invoke_main(
                        lambda d=d, o=o, f=fn:
                            self._set_status(
                                f'아이콘 추출 중… {d}/{total}  '
                                f'(성공:{o} 실패:{f})'))

            ok_final, fail_final = ok, fail
            self._invoke_main(lambda: self._set_status(
                f'아이콘 추출 완료 (성공 {ok_final}, 실패 {fail_final})'))
            self._log(
                f'🏁 아이콘 자동 추출 완료 — '
                f'성공 {ok_final}개 / 실패 {fail_final}개',
                'ok')
            self._invoke_main(self._refresh_current_selection)
            self._invoke_main(self._reset_scan_btn)

        self._run_in_thread(worker)

    def _refresh_current_selection(self):
        """현재 선택된 행이 있으면 상세 패널 다시 갱신."""
        sel = self.tree.selectionModel().selectedRows()
        if sel:
            self._update_detail_panel(sel)

    # ── 일괄 추출 (수동: 선택 항목) ────────────────────────────────────
    def _extract_icon_selected(self):
        sel = self.tree.selectionModel().selectedRows()
        if not sel:
            return
        idxs  = [s.row() for s in sel]
        items = [self._filtered[i] for i in idxs
                 if i < len(self._filtered)]
        targets = [x for x in items
                   if x.get('content_type') == 'base'
                   and x.get('title_id') and x.get('path')
                   and os.path.isfile(x['path'])]
        if not targets:
            QMessageBox.information(
                self, '아이콘 스캔',
                '선택한 항목에 스캔 가능한 베이스 게임이 없습니다.')
            return

        def worker():
            self._icon_dl_running = True
            try:
                ok = fail = 0
                done = 0
                lock = threading.Lock()

                def _extract_one(item):
                    tid = item['title_id']
                    if (self.icons_dir / f'{tid}.png').exists():
                        return True
                    return bool(self._extract_icon(tid, item))

                with ThreadPoolExecutor(max_workers=4) as pool:
                    futures = {pool.submit(_extract_one, it): it
                               for it in targets}
                    for future in as_completed(futures):
                        try:
                            res = future.result()
                        except Exception:
                            res = False
                        with lock:
                            done += 1
                            if res:
                                ok += 1
                            else:
                                fail += 1
                            d, total, o, fn = done, len(targets), ok, fail
                        self._invoke_main(
                            lambda d=d, t=total, o=o, f=fn:
                                self._set_status(
                                    f'아이콘 스캔 중… {d}/{t}  '
                                    f'(성공:{o} 실패:{f})'))

                ok_f, fail_f = ok, fail
                self._invoke_main(lambda: (
                    self._set_status(
                        f'아이콘 스캔 완료: 성공 {ok_f}, 실패 {fail_f}'),
                    self._refresh_current_selection(),
                ))
            finally:
                self._icon_dl_running = False

        self._run_in_thread(worker)
