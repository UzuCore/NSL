# nsl_detail.py — 우측 상세 패널 빌드 + 갱신 (PySide6)
import sys
from pathlib import Path

from PySide6.QtCore import Qt, QTimer, QUrl, QEvent, QObject
from PySide6.QtGui import (
    QImage, QFont, QColor, QTextCursor, QTextImageFormat,
    QTextCharFormat, QPainter,
)
from PySide6.QtWidgets import (
    QWidget, QFrame, QLabel, QTextEdit, QTabWidget, QTabBar,
    QScrollArea, QHBoxLayout, QVBoxLayout,
)

import nsl_database as db
import nsl_scanner as scanner
from nsl_theme import C, _FS, register_themed


# ── 언어/지역 코드 → 깃발 파일명 매핑 ──────────────────────────────────
_FLAG_FILE_MAP = {
    'KO': 'kr.png', 'KOR': 'kr.png', 'EN': 'us.png', 'ENG': 'us.png',
    'UK': 'gb.png', 'JA': 'jp.png', 'JPN': 'jp.png', 'EUR': 'eu.png',
    'FRE': 'fr.png', 'FRA': 'fr.png', 'GER': 'de.png', 'ITA': 'it.png',
    'SPA': 'es.png', 'DUT': 'nl.png', 'RUS': 'ru.png', 'ZHT': 'tw.png',
    'CHT': 'tw.png', 'ZH':  'cn.png', 'ZHS': 'cn.png', 'CHS': 'cn.png',
    'POR': 'pt.png',
}

_CT_LABEL = {'base': '베이스', 'update': '업데이트', 'dlc': 'DLC'}


# ── 커스텀 위젯 ────────────────────────────────────────────────────────
class _NSLTabBar(QTabBar):
    """
    Windows11 스타일 엔진이 탭 사이에 강제로 그리는 border-left를
    paintEvent 후에 배경색으로 덮어 1px로 만든다.
    """
    def paintEvent(self, ev):
        super().paintEvent(ev)
        cur = self.currentIndex()
        painter = QPainter(self)
        for i in range(self.count() - 1):
            r1 = self.tabRect(i + 1)
            x = r1.left()
            color = C['bg'] if i + 1 == cur else C['surface2']
            painter.setPen(QColor(color))
            painter.drawLine(x, r1.top(), x, r1.bottom() - 1)
        painter.end()


class _TabWidget(QTabWidget):
    """탭바 우측 빈 공간에 border 색 구분선을 직접 그리는 QTabWidget."""
    def __init__(self, parent=None):
        super().__init__(parent)
        self.setTabBar(_NSLTabBar(self))

    def paintEvent(self, ev):
        super().paintEvent(ev)
        tb = self.tabBar()
        if tb.count() == 0:
            return
        last_rect = tb.tabRect(tb.count() - 1)
        offset = tb.mapTo(self, tb.rect().topLeft())
        x = last_rect.right() + 1 + offset.x()
        y = last_rect.bottom() + offset.y()
        if self.width() - x > 0:
            painter = QPainter(self)
            painter.setPen(QColor(C['border']))
            painter.drawLine(x, y, self.width() - 1, y)
            painter.end()


class _ClickLabel(QLabel):
    """mousePressEvent를 콜백으로 노출하는 QLabel."""
    def __init__(self, *args, on_click=None, **kwargs):
        super().__init__(*args, **kwargs)
        self._on_click = on_click
        if on_click is not None:
            self.setCursor(Qt.PointingHandCursor)

    def mousePressEvent(self, ev):
        if ev.button() == Qt.LeftButton and self._on_click is not None:
            try:
                self._on_click()
            except Exception:
                pass
        super().mousePressEvent(ev)


# ── 텍스트 삽입 유틸 (정적) ────────────────────────────────────────────
def _ins_text(cursor: QTextCursor, text: str, color: str = None):
    fmt = QTextCharFormat()
    if color:
        fmt.setForeground(QColor(color))
    cursor.insertText(text, fmt)


def _ins_image(cursor: QTextCursor, url: str):
    img_fmt = QTextImageFormat()
    img_fmt.setName(url)
    img_fmt.setWidth(16)
    img_fmt.setHeight(16)
    cursor.insertImage(img_fmt)
    cursor.insertText(' ')


def _is_korean(text: str) -> bool:
    return any('\uAC00' <= ch <= '\uD7A3' for ch in text)


# ── 상세 패널 ──────────────────────────────────────────────────────────
class DetailPanelMixin:

    def _build_detail_panel(self, parent) -> QWidget:
        wrapper = QFrame(parent)
        wrapper.setObjectName('Surface')
        wrapper.setAutoFillBackground(True)

        outer = QVBoxLayout(wrapper)
        outer.setContentsMargins(8, 8, 8, 4)
        outer.setSpacing(4)

        outer.addWidget(self._build_meta_section(wrapper))
        outer.addWidget(self._build_notebook(wrapper), 1)

        self._icon_jobs = set()
        self._flag_pix_cache = {}
        return wrapper

    # ── 상단 메타 섹션 ─────────────────────────────────────────────────
    def _build_meta_section(self, parent: QWidget) -> QFrame:
        top = QFrame(parent)
        top.setObjectName('Surface')
        top_l = QHBoxLayout(top)
        top_l.setContentsMargins(0, 0, 0, 0)
        top_l.setSpacing(8)

        # 아이콘 (120x120)
        self._icon_label = QLabel(top)
        self._icon_label.setFixedSize(120, 120)
        self._icon_label.setAlignment(Qt.AlignCenter)
        register_themed(self._icon_label, lambda fs=_FS: (
            f'background-color: {C["surface2"]}; '
            f'color: {C["text_dim"]}; font-size: {fs+20}pt;'
        ))
        self._icon_label.setText('🎮')
        top_l.addWidget(self._icon_label, 0, Qt.AlignTop)
        top_l.addWidget(self._build_meta_info(top), 1)
        return top

    def _build_meta_info(self, parent: QWidget) -> QFrame:
        meta = QFrame(parent)
        meta.setObjectName('Surface')
        ml = QVBoxLayout(meta)
        ml.setContentsMargins(0, 0, 0, 0)
        ml.setSpacing(2)

        # 제목
        self._detail_title = QLabel('—', meta)
        self._detail_title.setWordWrap(True)
        f = QFont('Segoe UI', _FS)
        f.setBold(True)
        self._detail_title.setFont(f)
        register_themed(self._detail_title, lambda: (
            f'background-color: {C["surface"]}; color: {C["text"]};'
        ))
        self._detail_title.setAlignment(Qt.AlignLeft | Qt.AlignTop)
        ml.addWidget(self._detail_title)

        # 타이틀 ID
        self._detail_tid = QLabel('', meta)
        register_themed(self._detail_tid, lambda fs=_FS: (
            f'background-color: {C["surface"]}; color: {C["text_dim"]}; '
            f'font-family: Consolas; font-size: {max(fs-1, 8)}pt;'
        ))
        ml.addWidget(self._detail_tid)

        # 콘텐츠 타입
        self._detail_type = QLabel('', meta)
        register_themed(self._detail_type, lambda fs=_FS: (
            f'background-color: {C["surface"]}; color: {C["update"]}; '
            f'font-size: {max(fs-1, 8)}pt;'
        ))
        ml.addWidget(self._detail_type)

        # 버전 (보유 / 최신)
        self._detail_version = QLabel('', meta)
        self._detail_version.setTextFormat(Qt.RichText)
        register_themed(self._detail_version, lambda fs=_FS: (
            f'background-color: {C["surface"]}; color: {C["text_dim"]}; '
            f'font-size: {max(fs-1, 8)}pt;'
        ))
        ml.addWidget(self._detail_version)
        ml.addStretch(1)
        return meta

    # ── 노트북 (탭) ────────────────────────────────────────────────────
    def _build_notebook(self, parent: QWidget) -> _TabWidget:
        self.notebook = _TabWidget(parent)
        register_themed(self.notebook, lambda: (
            f'QTabWidget::pane {{ background-color: {C["bg"]}; '
            f'border: 1px solid {C["border"]}; top: -1px; }}'
            f'QTabBar::tab {{ background-color: {C["surface2"]}; '
            f'color: {C["text"]}; padding: 8px 14px; '
            f'border-top: 1px solid {C["border"]}; '
            f'border-bottom: 1px solid {C["border"]}; '
            f'border-left: none; border-right: none; '
            f'margin-right: 1px; }}'
            f'QTabBar::tab:first-child {{ '
            f'border-left: 1px solid {C["border"]}; }}'
            f'QTabBar::tab:last-child {{ '
            f'border-right: 1px solid {C["border"]}; '
            f'margin-right: 0px; }}'
            f'QTabBar::tab:selected {{ background-color: {C["bg"]}; '
            f'border-bottom: 1px solid {C["bg"]}; '
            f'margin-bottom: -1px; }}'
            f'QTabBar::tab:hover:!selected {{ '
            f'background-color: {C["surface"]}; }}'
        ))

        # 파일정보 탭
        tab_fileinfo = QWidget(self.notebook)
        tf_l = QVBoxLayout(tab_fileinfo)
        tf_l.setContentsMargins(4, 8, 0, 8)
        tf_l.setSpacing(0)
        self._detail_fileinfo = QTextEdit(tab_fileinfo)
        self._detail_fileinfo.setReadOnly(True)
        register_themed(self._detail_fileinfo, lambda fs=_FS: (
            f'background-color: {C["bg"]}; color: {C["text"]}; '
            f'font-family: "Malgun Gothic", "Segoe UI"; '
            f'font-size: {max(fs-1, 8)}pt; border: none;'
        ))
        tf_l.addWidget(self._detail_fileinfo, 1)
        self.notebook.addTab(tab_fileinfo, ' 파일정보 ')

        # 게임정보 탭
        tab_info = QWidget(self.notebook)
        ti_l = QVBoxLayout(tab_info)
        ti_l.setContentsMargins(4, 8, 0, 8)
        ti_l.setSpacing(0)

        self._detail_info = QTextEdit(tab_info)
        self._detail_info.setReadOnly(True)
        register_themed(self._detail_info, lambda fs=_FS: (
            f'background-color: {C["bg"]}; color: {C["text"]}; '
            f'font-family: "Malgun Gothic", "Segoe UI"; '
            f'font-size: {max(fs-1, 8)}pt; border: none;'
        ))
        ti_l.addWidget(self._detail_info, 1)

        # 번역 바 (오버레이)
        self._translate_bar = _ClickLabel(
            '한국어로 번역하기', tab_info,
            on_click=lambda: self._translate_description())
        self._translate_bar.setAlignment(Qt.AlignCenter)
        register_themed(self._translate_bar, lambda fs=_FS: (
            f'background-color: {C["selected"]}; color: {C["text"]}; '
            f'font-family: "Malgun Gothic", "Segoe UI"; '
            f'font-size: {max(fs-1, 8)}pt; '
            f'font-weight: bold; padding: 6px;'
        ))
        self._translate_bar.hide()

        def _reposition_translate_bar():
            try:
                w = tab_info.width()
                h = self._translate_bar.sizeHint().height()
                self._translate_bar.setGeometry(
                    0, tab_info.height() - h, w, h)
            except Exception:
                pass

        class _TabInfoResizeFilter(QObject):
            def eventFilter(self_, obj, ev):
                if ev.type() == QEvent.Resize:
                    _reposition_translate_bar()
                return False
        self._tab_info_resize_filter = _TabInfoResizeFilter()
        tab_info.installEventFilter(self._tab_info_resize_filter)
        self._reposition_translate_bar = _reposition_translate_bar
        self.notebook.addTab(tab_info, ' 게임정보 ')

        # 스크린샷 탭
        tab_ss = QWidget(self.notebook)
        ts_l = QVBoxLayout(tab_ss)
        ts_l.setContentsMargins(4, 4, 0, 4)
        ts_l.setSpacing(0)

        self._ss_scroll = QScrollArea(tab_ss)
        self._ss_scroll.setWidgetResizable(True)
        self._ss_scroll.setHorizontalScrollBarPolicy(
            Qt.ScrollBarAlwaysOff)
        self._ss_scroll.setVerticalScrollBarPolicy(
            Qt.ScrollBarAlwaysOn)
        register_themed(self._ss_scroll, lambda: (
            f'QScrollArea {{ '
            f'background-color: {C["surface"]}; '
            f'border: none; }}'
        ))
        self._ss_inner = QWidget()
        register_themed(
            self._ss_inner,
            lambda: f'background-color: {C["bg"]};')
        self._ss_inner_layout = QVBoxLayout(self._ss_inner)
        self._ss_inner_layout.setContentsMargins(8, 6, 8, 6)
        self._ss_inner_layout.setSpacing(8)
        self._ss_inner_layout.setAlignment(Qt.AlignTop)
        self._ss_scroll.setWidget(self._ss_inner)
        ts_l.addWidget(self._ss_scroll, 1)

        self._ss_resize_timer = QTimer(self)
        self._ss_resize_timer.setSingleShot(True)
        self._ss_resize_timer.setInterval(300)
        self._ss_resize_timer.timeout.connect(self._on_ss_resize_timeout)

        class _SsScrollResizeFilter(QObject):
            def eventFilter(self_, obj, ev):
                if ev.type() == QEvent.Resize:
                    self._ss_resize_timer.start()
                return False
        self._ss_scroll_resize_filter = _SsScrollResizeFilter()
        self._ss_scroll.installEventFilter(self._ss_scroll_resize_filter)
        self.notebook.addTab(tab_ss, ' 스크린샷 ')

        # DLC 탭
        tab_dlc = QWidget(self.notebook)
        td_l = QVBoxLayout(tab_dlc)
        td_l.setContentsMargins(4, 8, 0, 8)
        td_l.setSpacing(0)

        self._detail_dlc = QTextEdit(tab_dlc)
        self._detail_dlc.setReadOnly(True)
        register_themed(self._detail_dlc, lambda fs=_FS: (
            f'background-color: {C["bg"]}; color: {C["text"]}; '
            f'font-family: "Malgun Gothic", "Segoe UI"; '
            f'font-size: {max(fs-1, 8)}pt; border: none;'
        ))
        td_l.addWidget(self._detail_dlc, 1)
        self.notebook.addTab(tab_dlc, ' DLC ')

        # 마지막 선택 탭 복원
        try:
            last_tab = int(db.get_setting('last_selected_tab', '0'))
            self.notebook.setCurrentIndex(
                min(last_tab, self.notebook.count() - 1))
        except (ValueError, TypeError):
            pass

        def _save_selected_tab(idx):
            db.set_setting('last_selected_tab', str(idx))
        self.notebook.currentChanged.connect(_save_selected_tab)
        return self.notebook

    def _on_ss_resize_timeout(self):
        tid = getattr(self, '_current_ss_tid', None)
        if tid and self._ss_inner_layout.count() > 0:
            self._load_screenshots(tid)

    # ── 상세 패널 갱신 ─────────────────────────────────────────────────
    def _update_detail_panel(self, sel):
        """sel은 QModelIndex 리스트 (selectedRows)."""
        if not sel:
            return
        idx = sel[0].row() if hasattr(sel[0], 'row') else int(sel[0])
        if idx >= len(self._filtered):
            return
        item = self._filtered[idx]

        # 메타 헤더 갱신
        self._detail_title.setText(
            item.get('title_name') or item.get('filename', '—'))
        self._detail_tid.setText(item.get('title_id') or '—')
        self._detail_type.setText(self._build_type_str(item))
        self._detail_version.setText(self._build_version_str(item))

        # 탭 내용 렌더링
        self._render_game_info(item)
        self._render_file_info(item)
        self._render_dlc_tab(item)

        # 서브탭 자동 전환은 폴더 탭 변경 시에만 수행 (nsl.py 참조)

        tid = item.get('title_id') or ''
        self._load_screenshots(tid)
        if tid and hasattr(self, '_load_icon_async'):
            self._load_icon_async(tid, item)

    @staticmethod
    def _build_type_str(item: dict) -> str:
        ct       = item.get('content_type', '')
        has_u    = item.get('has_update', 0)
        dlc_n    = item.get('dlc_count', 0)
        file_ext = (item.get('file_type') or '').upper()
        ct_str   = _CT_LABEL.get(ct, ct)
        if has_u and dlc_n:
            ct_str += f'+U+{dlc_n}DLC'
        elif has_u:
            ct_str += '+U'
        elif dlc_n:
            ct_str += f'+{dlc_n}DLC'
        if file_ext:
            ct_str += f'  ({file_ext})'
        return ct_str

    @staticmethod
    def _build_version_str(item: dict) -> str:
        """헤더의 '보유 / 최신' 버전 HTML(2줄) 생성.

        - 최신 버전이 보유보다 높으면 err(빨강) 색에 굵게.
        - 보유 == 최신: 회색으로 동일 값 표시.
        - DB에 최신 정보가 없으면 'v0'.
        """
        _uv  = item.get('update_version')
        ver  = (_uv if (_uv is not None and _uv != 0)
                else (item.get('version', 0) or 0))
        lat  = item.get('db_latest_version')
        ver_str = scanner.version_to_str(ver)

        has_newer = lat is not None and lat > 0 and lat > ver
        if lat is not None and lat > 0:
            lat_str = scanner.version_to_str(lat)
        else:
            lat_str = scanner.version_to_str(0)

        line1 = f'보유: v{ver_str}'
        if has_newer:
            line2 = (f'<b style="color:{C["err"]};">'
                     f'최신: v{lat_str}</b>')
        else:
            line2 = (f'<span style="color:{C["text_dim"]};">'
                     f'최신: v{lat_str}</span>')
        return f'{line1}<br>{line2}'

    # ── 파일정보 탭 렌더링 ─────────────────────────────────────────────
    def _render_file_info(self, item: dict):
        if not hasattr(self, '_detail_fileinfo'):
            return
        self._detail_fileinfo.clear()
        cursor = self._detail_fileinfo.textCursor()

        def _row(label: str, value: str):
            _ins_text(cursor, f'{label}\n', C['text_dim'])
            _ins_text(cursor, f'  {value}\n\n', C['text'])

        name = (item.get('title_name')
                or item.get('filename') or '—')
        localized_names = (
            ('한국어', str(item.get('nacp_ko') or '').strip()),
            ('영어',   str(item.get('nacp_en') or '').strip()),
            ('일본어', str(item.get('nacp_ja') or '').strip()),
        )
        if any(localized_name for _, localized_name in localized_names):
            for language, localized_name in localized_names:
                if localized_name:
                    _row(f'게임 이름 ({language})', localized_name)
        else:
            _row('게임 이름', name)

        tid = item.get('title_id') or '—'
        _row('타이틀 ID', tid)

        _uv  = item.get('update_version')
        ver  = (_uv if (_uv is not None and _uv != 0)
                else (item.get('version', 0) or 0))
        lat  = item.get('db_latest_version')
        ver_str = scanner.version_to_str(ver)
        lat_str = (scanner.version_to_str(lat)
                   if lat is not None else '0')
        _row('보유 버전', ver_str)
        _row('최신 버전', lat_str)

        ct       = item.get('content_type', '')
        has_u    = item.get('has_update', 0)
        dlc_n    = item.get('dlc_count', 0)
        file_ext = (item.get('file_type') or '').upper()
        ct_str   = _CT_LABEL.get(ct, ct or '—')
        if has_u and dlc_n:
            ct_str += f'+U+{dlc_n}DLC'
        elif has_u:
            ct_str += '+U'
        elif dlc_n:
            ct_str += f'+{dlc_n}DLC'
        if file_ext:
            ct_str += f'  ({file_ext})'
        _row('타입', ct_str)

        size = scanner.format_size(item.get('size', 0))
        _row('크기', size)

        path = item.get('path') or '—'
        _row('파일 경로', path)

        filename = item.get('filename') or '—'
        _row('파일명', filename)

        self._detail_fileinfo.verticalScrollBar().setValue(0)

    # ── 게임정보 탭 렌더링 ─────────────────────────────────────────────
    def _render_game_info(self, item: dict):
        self._detail_info.clear()
        doc = self._detail_info.document()
        cursor = self._detail_info.textCursor()
        cursor.movePosition(QTextCursor.End)

        assets_dir = self._get_assets_dir()

        def _register_flag(code: str) -> str:
            filename = (_FLAG_FILE_MAP.get(code)
                        or (f'{code.lower()}.png'
                            if len(code) == 2 else None))
            if not filename:
                return None
            path = assets_dir / filename
            if not path.exists():
                return None
            url = f'flag://{code}'
            if url in self._flag_pix_cache:
                doc.addResource(2, QUrl(url), self._flag_pix_cache[url])
                return url
            img = QImage(str(path))
            if img.isNull():
                return None
            img = img.scaled(16, 16, Qt.KeepAspectRatio,
                             Qt.SmoothTransformation)
            doc.addResource(2, QUrl(url), img)
            self._flag_pix_cache[url] = img
            return url

        def _flag_or_text(code: str) -> bool:
            url = _register_flag(code)
            if url:
                _ins_image(cursor, url)
                return True
            _ins_text(cursor, code, C['text_dim'])
            return False

        # 지역 / 언어
        region_raw = str(item.get('region') or '').upper().strip()
        langs_raw  = str(item.get('languages') or '').upper()

        if region_raw:
            _ins_text(cursor, '지역: ', C['text_dim'])
            _flag_or_text(region_raw)
            _ins_text(cursor, '   ', C['text_dim'])
        if langs_raw:
            _ins_text(cursor, '언어: ', C['text_dim'])
            for lang in langs_raw.split(','):
                lang = lang.strip()
                if not _flag_or_text(lang):
                    _ins_text(cursor, lang + ' ', C['text_dim'])
        if region_raw or langs_raw:
            _ins_text(cursor, '\n\n')

        # 게임 설명
        desc = item.get('description') or ''
        self._desc_original = desc
        if desc:
            _ins_text(cursor, '게임 설명\n\n', C['update'])
            self._desc_pos_start = cursor.position()
            _ins_text(cursor, desc.replace('\\n', '\n') + '\n\n')
            self._desc_pos_end = cursor.position()
            if not _is_korean(desc):
                self._translate_bar.show()
                self._reposition_translate_bar()
                self._translate_bar.raise_()
            else:
                self._translate_bar.hide()
        else:
            self._desc_pos_start = self._desc_pos_end = None
            self._translate_bar.hide()

        # 업데이트 알림
        db_lat = item.get('db_latest_version')
        my_ver = max(item.get('version') or 0,
                     item.get('update_version') or 0)
        if (db_lat is not None and db_lat > 0
                and my_ver < db_lat):
            _ins_text(cursor, '누락된 업데이트 알림\n', C['warn'])
            _ins_text(
                cursor,
                f'  [업데이트 필요] 현재 버전: '
                f'{scanner.version_to_str(my_ver)}'
                f'  →  최신 버전: '
                f'{scanner.version_to_str(db_lat)}\n\n',
                C['text_dim'])

        self._detail_info.verticalScrollBar().setValue(0)

    # ── DLC 탭 렌더링 ──────────────────────────────────────────────────
    def _render_dlc_tab(self, item: dict):
        self._detail_dlc.clear()
        cursor = self._detail_dlc.textCursor()

        missing = item.get('missing_dlcs', [])
        owned   = item.get('owned_dlcs', [])

        if missing:
            _ins_text(
                cursor,
                f'누락된 DLC 목록 (총 {len(missing)}개)\n',
                C['warn'])
            for dlc in missing:
                _ins_text(
                    cursor,
                    f"  - [{dlc['title_id']}] {dlc['name']}\n",
                    C['text_dim'])
            _ins_text(cursor, '\n')
        if owned:
            _ins_text(
                cursor,
                f'설치된 DLC 목록 (총 {len(owned)}개)\n',
                C['ok'])
            for dlc in owned:
                _ins_text(
                    cursor,
                    f"  + [{dlc['title_id']}] {dlc['name']}\n",
                    C['text_dim'])

    def _get_assets_dir(self) -> Path:
        if getattr(sys, 'frozen', False) and hasattr(sys, '_MEIPASS'):
            return Path(sys._MEIPASS) / 'assets'
        d = self.base_dir / 'assets'
        d.mkdir(exist_ok=True)
        return d
