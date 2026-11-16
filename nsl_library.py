# nsl_library.py — 라이브러리 핵심: 시작·필터·테이블·통계·CSV·도움말 (PySide6)
import csv
import os
import re
import shutil
from pathlib import Path

from PySide6.QtCore import Qt, QTimer, QItemSelectionModel
from PySide6.QtGui import QColor, QStandardItem, QIcon, QPixmap
from PySide6.QtWidgets import QFileDialog, QMessageBox, QListWidgetItem

import nsl_database as db
import nsl_scanner as scanner
import nsl_titledb as titledb_module
from nsl_theme import C, restyle as _restyle


# ── 콘텐츠 타입 표시용 상수 ────────────────────────────────────────────
_TYPE_LBL = {
    'base':    '베이스', 'update':  '업데이트',
    'dlc':     'DLC',    'delta':   '패치',
    'unknown': '미확인',
}
_CT_LABEL_CSV = {
    'base': '베이스', 'update': '업데이트', 'dlc': 'DLC',
    'delta': '패치',  'unknown': '미확인',
}

_NORMALIZED_MERGED_RE = re.compile(
    r'^.+ \[B(?:\+U\d+)?(?:\+\d+DLC)?\]$')
_LEGACY_CONTENT_RE = re.compile(
    r' \[[0-9A-F]{16}\]\[v\d+\]\[(?:Base|Update|DLC)\]$',
    re.IGNORECASE)
_NORMALIZED_SINGLE_RE = re.compile(
    r'^.+ \[[0-9A-F]{16}\]\[v\d+\]\[(?:Base|Update|DLC)\]$',
    re.IGNORECASE)
_GAME_EXTENSIONS = {'.nsp', '.nsz', '.xci', '.xcz'}




class LibraryMixin:

    # ── 시작 / 새로고침 ────────────────────────────────────────────────
    def _async_startup(self):
        self._set_status('로드 중…')

        def worker():
            try:
                data = db.get_files_view()
            except Exception as e:
                err = str(e)
                self._invoke_main(
                    lambda: self._set_status(f'오류: {err}'))
                return
            self._invoke_main(lambda: (
                self._on_startup_done(data),
                self._update_db_btn_state(),
            ))

        self._run_in_thread(worker)

    def _update_db_btn_state(self):
        info      = titledb_module.get_local_file_info()
        all_exist = all(v.get('exists') for v in info.values())
        if all_exist:
            self.btn_db.setText('DB 매칭')
            try:
                self.btn_db.clicked.disconnect()
            except (TypeError, RuntimeError):
                pass
            self.btn_db.clicked.connect(self._load_local_titledb)
        else:
            self.btn_db.setText('DB 다운로드')
            try:
                self.btn_db.clicked.disconnect()
            except (TypeError, RuntimeError):
                pass
            self.btn_db.clicked.connect(self._update_titledb)

    def _on_startup_done(self, data):
        # 비동기 시작 로드 완료 → 로딩 화면 / 환영화면 전환 허용
        self._startup_loading = False
        self._library = data
        self._log_startup_duplicates(data)
        self._refresh_dir_tabs()
        self._apply_filters()
        self._update_stats()
        if data:
            self._hide_welcome()
            self._set_status(f'{len(data):,}개 로드됨')
            # 우측 상세 패널 보이기 + 위치 복원
            if not self._detail_frame.isVisible():
                self._detail_frame.show()
            self._restore_sash_pos()
        else:
            self._show_welcome()
            self._detail_frame.hide()
            self._set_status('스캔 폴더를 추가하고 스캔을 시작해 주세요.')

    def _log_startup_duplicates(self, data):
        """시작 시 같은 베이스 타이틀 ID를 가진 게임 파일을 로그에 표시."""
        grouped = {}
        for item in data:
            if item.get('content_type') != 'base':
                continue
            title_id = (item.get('title_id') or '').strip().upper()
            path = item.get('path')
            if len(title_id) != 16 or not path:
                continue
            grouped.setdefault(title_id, []).append(item)

        duplicates = [
            (title_id, items)
            for title_id, items in grouped.items()
            if len({os.path.normcase(os.path.abspath(x['path']))
                    for x in items}) > 1
        ]
        if not duplicates:
            return

        duplicate_files = sum(len(items) for _, items in duplicates)
        self._log(
            f'\n⚠ 중복 게임 감지 — {len(duplicates):,}개 게임 / '
            f'{duplicate_files:,}개 파일', 'warn')
        for title_id, items in sorted(
                duplicates,
                key=lambda pair: (
                    pair[1][0].get('title_name')
                    or pair[1][0].get('filename')
                    or pair[0]).lower()):
            name = (items[0].get('title_name')
                    or items[0].get('filename')
                    or '이름 없음')
            self._log(
                f'   ├ [{title_id}] {name} — {len(items)}개', 'warn')
            for index, item in enumerate(
                    sorted(items, key=lambda x: x['path'].lower())):
                branch = '└' if index == len(items) - 1 else '├'
                self._log(f'   │  {branch} {item["path"]}', 'dim')

    def _restore_sash_pos(self):
        saved = db.get_setting('sash_pos')

        def _apply():
            try:
                total = self.pw.size().width()
                if total > 100:
                    right_width = int(saved) if saved else 440
                    left_width  = max(0, total - right_width)
                    self.pw.setSizes([left_width, right_width])
            except Exception:
                pass

        QTimer.singleShot(50,  _apply)
        QTimer.singleShot(200, _apply)

    def _refresh_library(self):
        self._set_status('목록 불러오는 중…')

        def worker():
            try:
                data = db.get_files_view()
            except Exception as e:
                err = str(e)
                self._invoke_main(
                    lambda: self._set_status(f'갱신 오류: {err}'))
                return
            self._invoke_main(lambda: self._on_refresh_done(data))

        self._run_in_thread(worker)

    def _on_refresh_done(self, data):
        self._library = data
        self._refresh_dir_tabs()
        self._apply_filters()
        self._update_stats()
        if data:
            self._hide_welcome()
            if not self._detail_frame.isVisible():
                self._detail_frame.show()
                self._restore_sash_pos()
        else:
            self._show_welcome()
            self._detail_frame.hide()
        self._set_status(
            f'새로고침 완료  —  현재 {len(self._library):,}개')

        if getattr(self, '_scan_just_finished', False):
            self._scan_just_finished = False
            if db.get_setting('scan_icon', '1') == '1':
                self._prefetch_icons_after_scan()
            else:
                self._reset_scan_btn()
            if db.get_setting('scan_screenshot', '1') == '1':
                QTimer.singleShot(2000, self._auto_prefetch_screenshots)
            else:
                self._log('📸 스크린샷 자동 다운로드 꺼짐 (설정)', 'dim')

    # ── 파일명 정규화 ────────────────────────────────────────────────
    def _normalized_game_path(self, item):
        """NSRP 단독/병합 콘텐츠 규칙에 맞는 파일 경로를 계산."""
        old_path = item.get('path') or ''
        if not old_path or not os.path.isfile(old_path):
            raise ValueError('파일을 찾을 수 없습니다.')
        stem, ext = os.path.splitext(os.path.basename(old_path))
        if ext.lower() not in _GAME_EXTENSIONS:
            raise ValueError('지원하지 않는 게임 파일 형식입니다.')

        # 이미 검증된 NSRP 형식이면 스캐너의 누락 결과로 이름을 훼손하지 않는다.
        # 단독 콘텐츠: 게임명 [TitleID][v버전][Base]
        # 병합 콘텐츠: 게임명 [B+U버전+NDLC]
        if (_NORMALIZED_MERGED_RE.fullmatch(stem)
                or _NORMALIZED_SINGLE_RE.fullmatch(stem)):
            return old_path

        contents = db.get_file_contents(old_path)
        base_contents = [
            c for c in contents if c.get('content_type') == 'base'
        ]
        has_base = bool(base_contents)
        update_versions = [
            int(c.get('version') or 0) for c in contents
            if c.get('content_type') == 'update'
        ]
        dlc_ids = {
            str(c.get('title_id') or '').upper() for c in contents
            if c.get('content_type') == 'dlc' and c.get('title_id')
        }
        if not has_base:
            raise ValueError('베이스 게임 정보를 찾지 못했습니다.')

        # 기존 제목 부분은 유지하고 규칙 태그만 교체한다.
        title = _LEGACY_CONTENT_RE.sub('', stem).strip()
        if title == stem:
            title = (item.get('title_name') or stem).strip()
        title = re.sub(r'[\\/*?:"<>|]', '', title).strip().rstrip(' .')
        if not title:
            raise ValueError('게임 이름을 결정할 수 없습니다.')

        if not update_versions and not dlc_ids:
            base = base_contents[0]
            title_id = str(base.get('title_id') or '').strip().upper()
            if not re.fullmatch(r'[0-9A-F]{16}', title_id):
                raise ValueError('베이스 타이틀 ID를 찾지 못했습니다.')
            version = int(base.get('version') or 0)
            new_name = (
                f'{title} [{title_id}][v{version}][Base]{ext.lower()}')
        else:
            tag_parts = ['B']
            if update_versions:
                tag_parts.append(f'U{max(update_versions)}')
            if dlc_ids:
                tag_parts.append(f'{len(dlc_ids)}DLC')
            new_name = f'{title} [{"+".join(tag_parts)}]{ext.lower()}'
        return os.path.join(os.path.dirname(old_path), new_name)

    def _rename_normalized_path(self, item):
        """파일과 라이브러리 DB를 함께 변경. 실패하면 디스크 이름도 복원."""
        old_path = item.get('path') or ''
        new_path = self._normalized_game_path(item)
        if os.path.normcase(old_path) == os.path.normcase(new_path):
            return 'unchanged', old_path, new_path
        if os.path.exists(new_path):
            raise FileExistsError(
                f'같은 이름의 파일이 이미 있습니다: {os.path.basename(new_path)}')

        os.rename(old_path, new_path)
        try:
            db.rename_file_path(old_path, new_path)
        except Exception:
            try:
                os.rename(new_path, old_path)
            except Exception:
                pass
            raise
        return 'renamed', old_path, new_path

    def _normalize_selected_filename(self):
        item = self._get_item()
        if not item:
            QMessageBox.information(
                self, '파일명 정규화', '정규화할 게임을 선택해 주세요.')
            return
        try:
            new_path = self._normalized_game_path(item)
        except Exception as e:
            QMessageBox.warning(self, '파일명 정규화', str(e))
            return
        old_path = item.get('path') or ''
        if os.path.normcase(old_path) == os.path.normcase(new_path):
            QMessageBox.information(
                self, '파일명 정규화', '이미 정규화된 파일명입니다.')
            return
        reply = QMessageBox.question(
            self, '파일명 정규화',
            f'{os.path.basename(old_path)}\n\n→\n\n'
            f'{os.path.basename(new_path)}\n\n파일명을 변경할까요?',
            QMessageBox.Yes | QMessageBox.No, QMessageBox.No)
        if reply != QMessageBox.Yes:
            return
        try:
            self._rename_normalized_path(item)
        except Exception as e:
            QMessageBox.critical(self, '파일명 정규화 실패', str(e))
            return
        self._log(
            f'✓ 파일명 정규화: {os.path.basename(old_path)} '
            f'→ {os.path.basename(new_path)}', 'ok')
        self._refresh_library()

    def _normalize_current_tab_filenames(self):
        items = list(self._filtered)
        if not items:
            QMessageBox.information(
                self, '파일명 일괄 정규화',
                '현재 탭에 정규화할 파일이 없습니다.')
            return

        changes = []
        preview_errors = []
        for item in items:
            try:
                new_path = self._normalized_game_path(item)
                old_path = item.get('path') or ''
                if os.path.normcase(old_path) != os.path.normcase(new_path):
                    changes.append((item, new_path))
            except Exception as e:
                preview_errors.append((item, str(e)))
        if not changes:
            message = '현재 탭의 파일명이 모두 정규화되어 있습니다.'
            if preview_errors:
                message += f'\n분석할 수 없는 파일: {len(preview_errors)}개'
            QMessageBox.information(self, '파일명 일괄 정규화', message)
            return

        reply = QMessageBox.question(
            self, '파일명 일괄 정규화',
            f'현재 탭에서 {len(changes):,}개 파일의 이름을 변경합니다.\n'
            f'분석 제외: {len(preview_errors):,}개\n\n계속할까요?',
            QMessageBox.Yes | QMessageBox.No, QMessageBox.No)
        if reply != QMessageBox.Yes:
            return

        def worker():
            renamed = unchanged = failed = 0
            self._log(
                f'\n🚀 파일명 일괄 정규화 시작 — 대상 {len(changes):,}개',
                'info')
            for item, _new_path in changes:
                try:
                    state, old_path, new_path = self._rename_normalized_path(
                        item)
                    if state == 'renamed':
                        renamed += 1
                        self._log(
                            f'   ✓ {os.path.basename(old_path)} '
                            f'→ {os.path.basename(new_path)}', 'ok')
                    else:
                        unchanged += 1
                except Exception as e:
                    failed += 1
                    self._log(
                        f'   ❌ {os.path.basename(item.get("path") or "")}: '
                        f'{e}', 'err')
            self._log(
                f'🏁 파일명 일괄 정규화 완료 — 변경 {renamed:,} / '
                f'실패 {failed:,} / 변경 없음 {unchanged:,}', 
                'ok' if failed == 0 else 'warn')
            self._invoke_main(self._refresh_library)

        self._run_in_thread(worker)

    # ── 필터 / 정렬 / 테이블 ───────────────────────────────────────────
    def _apply_filters(self, *_):
        q    = (getattr(self, '_search_text', '') or '').lower().strip()
        data = [x for x in self._library
                if x.get('content_type') == 'base']

        filter_target = getattr(self, '_current_filter_dir', None)

        if filter_target == 'MISSING_UPDATES':
            try:
                exc = db.get_exception_tids('update')
            except Exception:
                exc = set()
            data = [x for x in data
                    if (lat := x.get('db_latest_version')) is not None
                    and lat > 0
                    and max(x.get('version') or 0,
                            x.get('update_version') or 0) < lat
                    and (x.get('title_id') or '').upper() not in exc]

        elif filter_target == 'MISSING_DLCS':
            try:
                exc = db.get_exception_tids('dlc')
            except Exception:
                exc = set()

            def _has_missing_dlc(x):
                missing   = x.get('missing_dlcs', [])
                remaining = [d for d in missing
                             if d.get('title_id', '').upper() not in exc]
                return len(remaining) > 0
            data = [x for x in data if _has_missing_dlc(x)]

        elif filter_target is not None:
            f_dir    = os.path.abspath(filter_target)
            new_data = []
            for x in data:
                p = x.get('path')
                if not p:
                    continue
                try:
                    if os.path.commonpath(
                            [f_dir, os.path.abspath(p)]) == f_dir:
                        new_data.append(x)
                except ValueError:
                    pass
            data = new_data

        if q:
            data = [x for x in data
                    if q in (x.get('title_name') or '').lower()
                    or q in (x.get('filename') or '').lower()
                    or q in (x.get('title_id') or '').lower()]

        self._filtered = data
        self._current_page = 0
        self._populate_table()

    def _page_size(self) -> int:
        if db.get_setting('paging_on', '0') != '1':
            return len(self._filtered) or 9999
        try:
            v = int(db.get_setting('page_size', '500'))
            return v if v > 0 else (len(self._filtered) or 9999)
        except Exception:
            return 500

    def _page_prev(self):
        if self._current_page > 0:
            self._current_page -= 1
            self._populate_table()

    def _page_next(self):
        ps = self._page_size()
        total_pages = max(1, (len(self._filtered) + ps - 1) // ps)
        if self._current_page < total_pages - 1:
            self._current_page += 1
            self._populate_table()

    # ── 테이블 채우기 (청크 단위 점진 삽입) ───────────────────────────
    def _populate_table(self):
        """현재 뷰 모드에 맞는 populate 함수 호출."""
        mode = getattr(self, '_view_mode', 'list')
        if mode == 'grid':
            self._populate_grid()
        else:
            self._populate_list()

    def _get_page_items(self):
        """현재 페이지의 (start, page_items) 반환 + 페이지 바 갱신."""
        ps          = self._page_size()
        total       = len(self._filtered)
        total_pages = max(1, (total + ps - 1) // ps)
        self._current_page = max(
            0, min(self._current_page, total_pages - 1))
        start      = self._current_page * ps
        end        = min(start + ps, total)
        page_items = self._filtered[start:end]
        if hasattr(self, '_pg_bar'):
            paging_on = db.get_setting('paging_on', '0') == '1'
            if paging_on and total_pages > 1:
                self._pg_bar.show()
                self._page_lbl.setText(
                    f'{self._current_page + 1} / {total_pages}')
                self._btn_prev.setEnabled(self._current_page > 0)
                self._btn_next.setEnabled(
                    self._current_page < total_pages - 1)
            else:
                self._pg_bar.hide()
        return start, page_items

    def _populate_list(self):
        """
        세대(generation) 기반 청크 삽입.
        새 _populate_table 호출이 들어오면 generation을 증가시키고,
        타이머 콜백은 자기 generation이 아니면 즉시 종료 →
        stale 데이터로 모델을 건드리지 않음.
        """
        self._populate_gen = getattr(self, '_populate_gen', 0) + 1
        my_gen = self._populate_gen

        if (hasattr(self, '_chunk_timer')
                and self._chunk_timer is not None):
            try:
                self._chunk_timer.stop()
            except Exception:
                pass

        try:
            self.tree.selectionModel().clearSelection()
        except Exception:
            pass

        self.tree_model.removeRows(0, self.tree_model.rowCount())

        start, page_items = self._get_page_items()

        ct_color = {
            'base':    QColor(C['update']),
            'update':  QColor(C['update']),
            'dlc':     QColor(C['dlc']),
            'delta':   QColor(C['update']),
            'unknown': QColor(C['unknown']),
        }

        def _make_row(global_i: int, item: dict) -> list[QStandardItem]:
            ct      = item.get('content_type', 'unknown')
            name    = '  ' + str(item.get('title_name')
                                 or item.get('filename') or '')
            tid     = item.get('title_id') or ''
            _uv     = item.get('update_version')
            ver     = (_uv if (_uv is not None and _uv != 0)
                       else (item.get('version', 0) or 0))
            lat     = item.get('db_latest_version')
            size    = scanner.format_size(item.get('size', 0))
            has_upd = item.get('has_update', 0)
            dlc_cnt = item.get('dlc_count', 0)
            ct_lbl  = _TYPE_LBL.get(ct, ct)
            if has_upd and dlc_cnt:
                ct_lbl = f'{ct_lbl}+U+{dlc_cnt}DLC'
            elif has_upd:
                ct_lbl = f'{ct_lbl}+U'
            elif dlc_cnt:
                ct_lbl = f'{ct_lbl}+{dlc_cnt}DLC'
            status = (
                ('🚀 업데이트 가능' if ver < lat else '✅ 최신')
                if lat is not None else '⚠ 미확인'
            )
            values = [
                name, tid,
                scanner.version_to_str(ver),
                scanner.version_to_str(lat) if lat is not None else '0',
                size, status,
            ]
            color = ct_color.get(ct, ct_color['unknown'])
            row = []
            for v in values:
                it = QStandardItem(str(v))
                it.setEditable(False)
                it.setForeground(color)
                row.append(it)
            row[0].setData(global_i, Qt.UserRole + 1)
            return row

        count = len(page_items)
        self._populate_state = {
            'gen': my_gen, 'idx': 0, 'count': count,
            'start': start, 'items': page_items, 'make_row': _make_row,
        }
        self._insert_chunk(my_gen)

    def _populate_grid(self):
        """그리드 뷰 채우기 — 아이콘 + 이름."""
        gv = self._grid_view
        gv.clear()

        start, page_items = self._get_page_items()

        _PLACEHOLDER = None

        def _placeholder_icon():
            nonlocal _PLACEHOLDER
            if _PLACEHOLDER is None:
                pix = QPixmap(100, 100)
                pix.fill(QColor(C.get('surface2', '#2d2d2d')))
                _PLACEHOLDER = QIcon(pix)
            return _PLACEHOLDER

        ph = _placeholder_icon()
        tids_needing_icon = []

        for i, item in enumerate(page_items):
            global_i = start + i
            name = (item.get('title_name')
                    or item.get('filename') or '').strip()
            tid  = item.get('title_id') or ''

            lw_item = QListWidgetItem(ph, name)
            lw_item.setData(Qt.UserRole, global_i)
            lw_item.setToolTip(
                f"{name}\n{tid}\n"
                f"{scanner.format_size(item.get('size', 0))}")
            lw_item.setTextAlignment(Qt.AlignHCenter | Qt.AlignBottom)
            gv.addItem(lw_item)

            if tid and (self.icons_dir / f'{tid}.png').exists():
                tids_needing_icon.append((tid, gv.count() - 1))

        if tids_needing_icon:
            self._load_icons_for_view(tids_needing_icon, gv)

        # 첫 아이템 자동 선택
        if gv.count() > 0 and gv.currentItem() is None:
            gv.setCurrentRow(0)
            self._on_select_grid(gv.item(0), None)

    def _load_icons_for_view(self, tid_row_pairs, list_widget):
        """
        그리드뷰 아이콘 비동기 배치 로딩.
        메모리 캐시 히트 시 즉시 표시, 미스 시 워커에서 파일 로드.
        """
        icon_size = list_widget.iconSize()
        w = icon_size.width()
        h = icon_size.height()

        still_missing = []
        for tid, row in tid_row_pairs:
            if tid in self._icon_cache:
                self._icon_cache.move_to_end(tid)
                pix = self._icon_cache[tid]
                pix_scaled = pix.scaled(
                    w, h, Qt.KeepAspectRatio,
                    Qt.SmoothTransformation)
                lw_item = list_widget.item(row)
                if lw_item:
                    lw_item.setIcon(QIcon(pix_scaled))
            else:
                still_missing.append((tid, row))

        if not still_missing:
            return

        def worker():
            results = []
            for tid, row in still_missing:
                cache_file = self.icons_dir / f'{tid}.png'
                if cache_file.exists():
                    results.append((tid, row, str(cache_file)))
            if results:
                self._invoke_main(
                    lambda r=results: self._apply_icons_to_view(
                        r, list_widget, w, h))

        self._run_in_thread(worker)

    def _apply_icons_to_view(self, results, list_widget, w, h):
        """메인 스레드: 파일에서 로드한 아이콘을 그리드 뷰에 적용."""
        for tid, row, path in results:
            pix = QPixmap(path)
            if pix.isNull():
                continue
            pix_scaled = pix.scaled(
                w, h, Qt.KeepAspectRatio, Qt.SmoothTransformation)
            if tid not in self._icon_cache:
                self._icon_cache[tid] = pix.scaled(
                    120, 120, Qt.KeepAspectRatio,
                    Qt.SmoothTransformation)
                if len(self._icon_cache) > self._ICON_CACHE_MAX:
                    self._icon_cache.popitem(last=False)
            lw_item = list_widget.item(row)
            if lw_item:
                lw_item.setIcon(QIcon(pix_scaled))

    def _insert_chunk(self, my_gen: int):
        """청크 단위 삽입 — generation이 일치할 때만 동작."""
        # 새 _populate_table 호출이 들어왔으면 즉시 종료
        if my_gen != getattr(self, '_populate_gen', 0):
            return
        st = getattr(self, '_populate_state', None)
        if st is None or st['gen'] != my_gen:
            return

        CHUNK = 300
        idx     = st['idx']
        count   = st['count']
        start   = st['start']
        items   = st['items']
        mk      = st['make_row']
        end_idx = min(idx + CHUNK, count)

        for i in range(idx, end_idx):
            global_i = start + i
            row = mk(global_i, items[i])
            self.tree_model.appendRow(row)
        st['idx'] = end_idx

        if end_idx < count:
            # 다음 청크 예약
            if (not hasattr(self, '_chunk_timer')
                    or self._chunk_timer is None):
                self._chunk_timer = QTimer(self)
                self._chunk_timer.setSingleShot(True)
                self._chunk_timer.timeout.connect(self._on_chunk_timer)
            self._chunk_timer.start(10)
        else:
            # 완료 — 첫 행 자동 선택
            self._populate_state = None
            if (self.tree_model.rowCount() > 0
                    and not self.tree.selectionModel().hasSelection()):
                first_idx = self.tree_model.index(0, 0)
                self.tree.selectionModel().select(
                    first_idx,
                    QItemSelectionModel.ClearAndSelect
                    | QItemSelectionModel.Rows)
                self.tree.scrollTo(first_idx)
                self._on_select()

    def _on_chunk_timer(self):
        """타이머 콜백 — 현재 generation으로 다음 청크 처리."""
        st = getattr(self, '_populate_state', None)
        if st is None:
            return
        self._insert_chunk(st['gen'])

    def _update_stats(self):
        s = db.get_stats()
        self.summary_lbl.setText(
            f"파일: {s.get('file_count',0):,}  |   "
            f"게임: {s.get('base_count',0):,}  "
            f"업데이트: {s.get('update_count',0):,}  "
            f"DLC: {s.get('dlc_count',0):,}  |   "
            f"{scanner.format_size(s.get('total_size',0))}  |   "
            f"DB: {s.get('title_db_count',0):,}")

    def _do_sort(self, col: str):
        if self._sort_key == col:
            self._sort_asc = not self._sort_asc
        else:
            self._sort_key = col
            self._sort_asc = True
        km = {
            'name':       lambda x: (x.get('title_name')
                                     or x.get('filename') or '').lower(),
            'title_id':   lambda x: x.get('title_id') or '',
            'version':    lambda x: (x['update_version']
                                     if (x.get('update_version') is not None
                                         and x.get('update_version') != 0)
                                     else (x.get('version') or 0)),
            'latest_ver': lambda x: x.get('db_latest_version') or 0,
            'size':       lambda x: x.get('size') or 0,
            'source':     lambda x: x.get('source') or '',
            'status':     lambda x: (
                0 if x.get('db_latest_version') is None else
                1 if (x.get('update_version')
                      or x.get('version') or 0)
                     < x.get('db_latest_version') else 2
            ),
        }
        self._filtered.sort(key=km.get(col, lambda x: ''),
                            reverse=not self._sort_asc)
        self._populate_table()

    # ── 캐시 / 라이브러리 관리 ─────────────────────────────────────────
    def _show_cache_stats(self):
        folders = db.get_scan_folders()
        if not folders:
            QMessageBox.information(self, '캐시', '스캔 폴더가 없습니다.')
            return
        lines = ['폴더별 캐시:\n']
        for f in folders:
            s = scanner.CacheManager(f).stats()
            lines += [f' 📁 {f}',
                      f'     활용: {s["total"]:,}개',
                      f'     파일: {s["cache_file"]}', '']
        QMessageBox.information(self, '캐시 현황', '\n'.join(lines))

    def _clear_library(self):
        # 진행 중 작업이 있으면 초기화 차단 (DB 잠금 충돌 방지)
        busy = []
        if getattr(self, '_scanning', False):
            busy.append('스캔')
        if getattr(self, '_icon_extracting', False):
            busy.append('아이콘 추출')
        if getattr(self, '_decompress_running', False):
            busy.append('압축 해제')
        if getattr(self, '_ss_downloading', False):
            busy.append('스크린샷 다운로드')
        if getattr(self, '_cleanup_running', False):
            busy.append('더미 데이터 정리')
        if busy:
            QMessageBox.information(
                self, '진행 중',
                '다음 작업이 진행 중입니다. 완료 후 다시 시도해 주세요.'
                '\n\n· ' + '\n· '.join(busy))
            return

        ans = QMessageBox.question(
            self, '확인',
            '라이브러리를 완전 초기화하시겠습니까?\n\n'
            '· 데이터베이스 파일 삭제 (스캔된 게임 목록 + 타이틀 DB)\n'
            '· 캐시 폴더 내부 전체 삭제 (스캔 캐시)\n\n'
            '※ 게임 파일 자체는 삭제되지 않습니다.\n'
            '※ 아이콘/스크린샷 캐시는 유지됩니다.',
            QMessageBox.Yes | QMessageBox.No)
        if ans != QMessageBox.Yes:
            return

        self._set_status('라이브러리 초기화 중...')
        errors = []

        # 1) 메모리상 데이터 비우기
        try:
            self._library  = []
            self._filtered = []
            if hasattr(self, 'tree_model'):
                self.tree_model.removeRows(
                    0, self.tree_model.rowCount())
        except Exception as e:
            errors.append(f'UI 클리어: {e}')

        # 2) 폴더별 스캔 캐시 정리
        try:
            for f in db.get_scan_folders():
                scanner.clear_cache(f)
        except Exception as e:
            errors.append(f'스캔 캐시: {e}')

        # 3) DB 파일 자체 삭제 (WAL/SHM 부속 파일 포함)
        try:
            deleted, db_errs = db.delete_db_file()
            if db_errs:
                errors.extend(db_errs)
        except Exception as e:
            errors.append(f'DB 파일 삭제: {e}')

        # 4) caches/ 폴더 내부 비우기 (폴더 자체는 보존)
        try:
            cache_root = Path(scanner.CACHE_BASE_DIR)
            if cache_root.exists():
                for entry in cache_root.iterdir():
                    try:
                        if entry.is_dir():
                            shutil.rmtree(entry, ignore_errors=True)
                        else:
                            entry.unlink()
                    except Exception as e:
                        errors.append(f'{entry.name}: {e}')
        except Exception as e:
            errors.append(f'caches 정리: {e}')

        # 5) DB 빈 스키마로 재생성
        try:
            db.init_db()
        except Exception as e:
            errors.append(f'DB 재초기화: {e}')

        # 6) UI 갱신
        try:
            self._refresh_library()
        except Exception as e:
            errors.append(f'라이브러리 갱신: {e}')

        if errors:
            QMessageBox.warning(
                self, '초기화 완료 (일부 경고)',
                '라이브러리 초기화가 완료되었으나 '
                '일부 항목에서 경고가 있었습니다:\n\n'
                + '\n'.join(errors[:10])
                + (f'\n...외 {len(errors)-10}건'
                   if len(errors) > 10 else ''))
            self._set_status('라이브러리 초기화 완료 (경고 있음)')
        else:
            self._set_status('라이브러리 완전 초기화 완료')

    # ── CSV 내보내기 ───────────────────────────────────────────────────
    def _export_csv(self):
        path, _ = QFileDialog.getSaveFileName(
            self, 'CSV 내보내기', '', 'CSV Files (*.csv)')
        if not path:
            return
        if not path.lower().endswith('.csv'):
            path += '.csv'
        try:
            with open(path, 'w', newline='',
                      encoding='utf-8-sig') as f:
                w = csv.writer(f)
                w.writerow([
                    '게임 이름', '타이틀 (KO)', '타이틀 (EN)',
                    '타이틀 (JA)', '타이틀 ID', '종류',
                    '보유 버전', '업데이트 버전', '최신 버전',
                    '상태', '크기', '형식',
                    'DLC 보유', 'DB DLC 수',
                    '언어', '지역',
                    '파일명', '파일 경로',
                ])
                for item in self._library:
                    ct  = item.get('content_type', '')
                    _uv = item.get('update_version')
                    ver = (_uv if (_uv is not None and _uv != 0)
                           else (item.get('version', 0) or 0))
                    lat = item.get('db_latest_version')
                    status = (('최신' if ver >= lat
                               else '업데이트 가능')
                              if lat is not None else '미확인')
                    w.writerow([
                        item.get('title_name')
                        or item.get('filename', ''),
                        item.get('nacp_ko', ''),
                        item.get('nacp_en', ''),
                        item.get('nacp_ja', ''),
                        item.get('title_id', ''),
                        _CT_LABEL_CSV.get(ct, ct),
                        scanner.version_to_str(item.get('version', 0)),
                        scanner.version_to_str(_uv) if _uv else '',
                        (scanner.version_to_str(lat)
                         if lat is not None else ''),
                        status,
                        scanner.format_size(item.get('size', 0)),
                        (item.get('file_type') or '').upper(),
                        item.get('dlc_count', 0),
                        item.get('db_dlc_count', 0),
                        item.get('languages', ''),
                        item.get('region', ''),
                        item.get('filename', ''),
                        item.get('path', ''),
                    ])
            QMessageBox.information(self, '완료', f'저장:\n{path}')
        except Exception as e:
            QMessageBox.critical(self, '오류', str(e))

    # ── 로그 ───────────────────────────────────────────────────────────
    # _log / _flush_log_timer / _show_log / _hide_log은 nsl.py 메인에 있음
    def _log_clear(self):
        if hasattr(self, '_log_text') and self._log_text is not None:
            self._log_text.clear()
        if hasattr(self, '_log_buf'):
            self._log_buf.clear()

    # ── 도움말 / 정보 ──────────────────────────────────────────────────
    def _show_shortcuts(self):
        QMessageBox.information(
            self, '단축키',
            '빠른 스캔 F5\n'
            '설정 열기/닫기 Ctrl+O\n'
            '검색창 포커스 Ctrl+F\n'
            '종료 Alt+F4\n'
            '검색어 초기화 Esc\n'
            '파일 실행 Enter\n'
            '컨텍스트 메뉴 우클릭\n'
        )

    def _about(self):
        QMessageBox.information(
            self, f'{self._app_name} 정보',
            f'{self._app_name} v{self._app_version}\n\n'
            '닌텐도 스위치 게임 라이브러리 관리 툴\n\n'
            '지원 형식: NSP, NSZ, XCI, XCZ\n\n'
            f'DB: {db.DB_PATH}\n'
            f'캐시: {scanner.CACHE_BASE_DIR}')
