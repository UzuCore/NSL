# nsl_settings.py — 설정 패널 (SettingsPanelMixin) (PySide6)
import os
import platform
from PySide6.QtCore import Qt, QTimer
from PySide6.QtGui import QFont, QIntValidator
from PySide6.QtWidgets import (
    QWidget, QFrame, QLabel, QPushButton, QListWidget, QTextEdit,
    QLineEdit, QCheckBox, QRadioButton, QButtonGroup, QScrollArea,
    QHBoxLayout, QVBoxLayout, QGridLayout,
    QFileDialog, QMessageBox, QApplication,
)
import nsl_database as db
import nsl_theme as theme_module
from nsl_theme import C, _FS, register_themed

# ── 통일 디자인 상수 ─────────────────────────────────────────
# 모든 설정 페이지가 같은 여백/간격/색상을 쓰도록 한 곳에 모음.
_PAGE_MARGINS = (24, 22, 24, 20)   # 페이지 본문 contentsMargins
_SECTION_GAP  = 18                 # 섹션 사이 세로 간격
_ROW_GAP      = 8                  # 한 섹션 내 행 사이 세로 간격
_LABEL_WIDTH  = 200                # 행 좌측 라벨 고정 폭

class SettingsPanelMixin:
    """
    SwitchLibraryManager가 상속해서 사용.
    self._table_frame, self._settings_panel, self._settings_visible 가
    메인 윈도우에 존재한다고 가정.
    """

    # ── 설정 패널 토글 ────────────────────────────────────────
    def _toggle_settings(self):
        if self._settings_visible:
            self._settings_panel.hide()
            self._table_frame.show()
            self._settings_visible = False
            self._set_status('설정 닫힘')
        else:
            self._table_frame.hide()
            self._settings_panel.show()
            self._settings_visible = True
            self._refresh_settings_panel()
            self._set_status('설정')

    # ─────────────────────────────────────────────────────────
    # 공통 디자인 헬퍼 — 모든 설정 페이지에서 동일하게 호출.
    # 페이지마다 헤더/구분선/행 스타일이 따로 놀던 부분을
    # 한 군데서 통제하기 위한 메서드들.
    # ─────────────────────────────────────────────────────────
    def _settings_h1(self, parent: QWidget, text: str) -> QLabel:
        """페이지 최상단 제목 라벨."""
        lbl = QLabel(text, parent)
        f = QFont('Segoe UI', _FS + 4)
        f.setBold(True)
        lbl.setFont(f)
        register_themed(lbl, lambda: f'color: {C["text"]};')
        return lbl

    def _settings_subtitle(self, parent: QWidget, text: str) -> QLabel:
        """제목 바로 아래에 들어가는 설명 부제목."""
        lbl = QLabel(text, parent)
        register_themed(lbl, lambda: f'color: {C["text_dim"]};')
        lbl.setWordWrap(True)
        return lbl

    def _settings_separator(self, parent: QWidget) -> QFrame:
        """헤더/섹션 사이 구분선."""
        line = QFrame(parent)
        line.setFrameShape(QFrame.HLine)
        line.setFrameShadow(QFrame.Plain)
        register_themed(line, lambda: (
            f'background-color: {C["border"]}; '
            f'color: {C["border"]}; '
            f'border: none; min-height: 1px; max-height: 1px;'
        ))
        return line

    def _settings_section_label(self, parent: QWidget, text: str) -> QLabel:
        """페이지 내 소제목(섹션 라벨)."""
        lbl = QLabel(text, parent)
        f = QFont('Segoe UI', _FS + 1)
        f.setBold(True)
        lbl.setFont(f)
        register_themed(lbl, lambda: f'color: {C["text"]};')
        return lbl

    def _settings_hint(self, parent: QWidget, text: str) -> QLabel:
        """행 밑에 붙는 작은 도움말."""
        lbl = QLabel(text, parent)
        register_themed(lbl, lambda fs=_FS: (
            f'color: {C["text_dim"]}; '
            f'font-size: {max(fs - 1, 7)}pt;'
        ))
        lbl.setWordWrap(True)
        return lbl

    def _settings_page_header(self, pg: QWidget, v: QVBoxLayout,
                              title: str, subtitle: str = ''):
        """모든 페이지 상단에 들어가는 '제목 + 설명 + 구분선' 묶음."""
        v.addWidget(self._settings_h1(pg, title))
        if subtitle:
            sub = self._settings_subtitle(pg, subtitle)
            sub.setContentsMargins(0, 2, 0, 0)
            v.addWidget(sub)
        sep_wrap = QFrame(pg)
        sep_l = QVBoxLayout(sep_wrap)
        sep_l.setContentsMargins(0, 8, 0, 8)
        sep_l.setSpacing(0)
        sep_l.addWidget(self._settings_separator(pg))
        v.addWidget(sep_wrap)


    @staticmethod
    def _make_page_layout(pg: QWidget):
        """모든 설정 페이지 공통: VBoxLayout + 여백/간격 적용 후 반환."""
        v = QVBoxLayout(pg)
        v.setContentsMargins(*_PAGE_MARGINS)
        v.setSpacing(_ROW_GAP)
        return v

    def _settings_row(self, parent: QWidget, label_text: str,
                      build_fn) -> QFrame:
        """
        좌측 200px 라벨 + 우측 컨트롤(들)로 구성된 한 줄 행.
        build_fn(h_layout, row_widget) 안에서 컨트롤을 추가하면 됨.
        general / launcher / db 등 모든 페이지에서 사용해 통일감 확보.
        """
        row = QFrame(parent)
        row.setStyleSheet('background-color: transparent;')
        h = QHBoxLayout(row)
        h.setContentsMargins(0, 2, 0, 2)
        h.setSpacing(8)
        lbl = QLabel(label_text, row)
        lbl.setFixedWidth(_LABEL_WIDTH)
        lbl.setAlignment(Qt.AlignLeft | Qt.AlignVCenter)
        register_themed(lbl, lambda: f'color: {C["text"]};')
        h.addWidget(lbl)
        build_fn(h, row)
        h.addStretch(1)
        return row

    # ── 입력/체크박스 강제 인라인 스타일 헬퍼 ────────────────
    # 전역 QSS가 부모/형제 스타일에 가려지는 경우가 있어, 설정 페이지의
    # 입력 위젯들은 인라인 스타일로 직접 박아서 항상 적용되게 한다.
    def _style_input(self, widget):
        """QLineEdit 등 텍스트 입력에 인라인 스타일 강제 적용 (테마 변경 추종)."""
        register_themed(widget, lambda: (
            f'QLineEdit, QLineEdit:!focus {{ '
            f'background-color: {C.get("input_bg", C["bg"])}; '
            f'color: {C["text"]}; '
            f'border: 1px solid {C.get("input_border", C["accent2"])}; '
            f'border-radius: 4px; '
            f'padding: 5px 7px; '
            f'selection-background-color: {C["selected"]}; }}'
            f'QLineEdit:focus {{ border: 1px solid {C["accent2"]}; }}'
            f'QLineEdit:disabled {{ '
            f'background-color: {C["bg"]}; color: {C["text_dim"]}; '
            f'border: 1px solid {C["border"]}; }}'
        ))

    def _style_checkbox(self, widget):
        """QCheckBox에 인라인 스타일 강제 적용 (테마 변경 추종)."""
        register_themed(widget, lambda: (
            f'QCheckBox {{ background-color: transparent; '
            f'color: {C["text"]}; spacing: 8px; padding: 2px 0px; }}'
            f'QCheckBox::indicator {{ width: 16px; height: 16px; }}'
            f'QCheckBox::indicator:unchecked {{ '
            f'background-color: {C.get("input_bg", C["bg"])}; '
            f'border: 1px solid {C.get("input_border", C["accent2"])}; '
            f'border-radius: 3px; }}'
            f'QCheckBox::indicator:unchecked:hover {{ '
            f'border: 1px solid {C["accent2"]}; }}'
            f'QCheckBox::indicator:checked {{ '
            f'background-color: {C["accent2"]}; '
            f'border: 1px solid {C["accent2"]}; '
            f'border-radius: 3px; }}'
        ))

    def _style_radio(self, widget):
        """QRadioButton에 인라인 스타일 강제 적용 (테마 변경 추종)."""
        register_themed(widget, lambda: (
            f'QRadioButton {{ background-color: transparent; '
            f'color: {C["text"]}; spacing: 8px; padding: 2px 0px; }}'
            f'QRadioButton::indicator {{ width: 16px; height: 16px; }}'
            f'QRadioButton::indicator:unchecked {{ '
            f'background-color: {C.get("input_bg", C["bg"])}; '
            f'border: 1px solid {C.get("input_border", C["accent2"])}; '
            f'border-radius: 8px; }}'
            f'QRadioButton::indicator:unchecked:hover {{ '
            f'background-color: {C.get("input_bg", C["bg"])}; '
            f'border: 1px solid {C["accent2"]}; '
            f'border-radius: 8px; }}'
            f'QRadioButton::indicator:checked {{ '
            f'background-color: {C.get("input_bg", C["bg"])}; '
            f'border: 1px solid {C["accent2"]}; '
            f'border-radius: 8px; '
            f'image: url({theme_module._SVG_RADIO}); }}'
            f'QRadioButton::indicator:checked:hover {{ '
            f'background-color: {C.get("input_bg", C["bg"])}; '
            f'border: 1px solid {C["accent2"]}; '
            f'border-radius: 8px; '
            f'image: url({theme_module._SVG_RADIO}); }}'
            f'QRadioButton::indicator:disabled {{ '
            f'background-color: {C["bg"]}; '
            f'border: 1px solid {C["border"]}; '
            f'border-radius: 8px; }}'
        ))

    def _style_button(self, widget):
        """일반 QPushButton에 인라인 스타일 강제 적용."""
        register_themed(widget, lambda: (
            f'QPushButton {{ '
            f'background-color: {C["surface2"]}; '
            f'color: {C["text"]}; '
            f'border: 1px solid {C.get("input_border", C["border"])}; '
            f'border-radius: 3px; padding: 5px 10px; }}'
            f'QPushButton:hover {{ '
            f'background-color: {C["selected"]}; '
            f'border: 1px solid {C["accent2"]}; }}'
            f'QPushButton:pressed {{ '
            f'background-color: {C["accent2"]}; '
            f'color: {C.get("on_accent", "#ffffff")}; '
            f'border: 1px solid {C["accent2"]}; }}'
            f'QPushButton:disabled {{ '
            f'background-color: {C["bg"]}; color: {C["text_dim"]}; '
            f'border: 1px solid {C["border"]}; }}'
        ))

    # ── 설정 패널 빌드 ────────────────────────────────────────
    def _build_settings_panel(self, parent: QWidget) -> QWidget:
        panel = QFrame(parent)
        panel.setObjectName('Surface')
        panel.setAutoFillBackground(True)
        outer = QVBoxLayout(panel)
        outer.setContentsMargins(0, 0, 0, 0)
        outer.setSpacing(0)

        # ── 헤더 ─────────────────────────────────────────────
        hdr = QFrame(panel)
        hdr.setObjectName('Panel')
        hdr.setAutoFillBackground(True)
        hl = QHBoxLayout(hdr)
        hl.setContentsMargins(20, 10, 14, 10)

        title = QLabel('⚙  설정', hdr)
        title.setObjectName('Panel')
        f = QFont('Segoe UI', _FS + 4)
        f.setBold(True)
        title.setFont(f)
        hl.addWidget(title)
        hl.addStretch(1)

        close_btn = QPushButton('✕  닫기', hdr)
        close_btn.clicked.connect(self._toggle_settings)
        hl.addWidget(close_btn)
        outer.addWidget(hdr)

        # ── 본문(좌측 사이드바 + 우측 콘텐츠) ───────────────
        body = QFrame(panel)
        body.setObjectName('Surface')
        body.setAutoFillBackground(True)
        bl = QHBoxLayout(body)
        bl.setContentsMargins(0, 0, 0, 0)
        bl.setSpacing(0)

        sidebar = QFrame(body)
        sidebar.setObjectName('Panel')
        sidebar.setAutoFillBackground(True)
        sidebar.setFixedWidth(200)
        sl = QVBoxLayout(sidebar)
        sl.setContentsMargins(0, 8, 0, 0)
        sl.setSpacing(0)

        content_area = QFrame(body)
        content_area.setObjectName('Surface')
        content_area.setAutoFillBackground(True)
        self._settings_content_layout = QVBoxLayout(content_area)
        self._settings_content_layout.setContentsMargins(0, 0, 0, 0)

        bl.addWidget(sidebar)
        bl.addWidget(content_area, 1)
        outer.addWidget(body, 1)

        # 페이지/카테고리 버튼 보관
        self._settings_pages    = {}
        self._settings_cat_btns = {}

        def _make_page(key: str) -> QWidget:
            """페이지 콘텐츠를 스크롤 가능한 영역으로 감싸서 반환."""
            scroll = QScrollArea(content_area)
            scroll.setObjectName('Surface')
            scroll.setWidgetResizable(True)
            scroll.setFrameShape(QFrame.NoFrame)
            register_themed(scroll, lambda: (
                f'background-color: {C["surface"]}; border: none;'
            ))

            page = QWidget(scroll)
            page.setObjectName('Surface')
            register_themed(page, lambda: f'background-color: {C["surface"]};')
            scroll.setWidget(page)

            self._settings_content_layout.addWidget(scroll)
            scroll.hide()
            self._settings_pages[key] = scroll
            return page

        def _switch(key: str):
            for k, pg in self._settings_pages.items():
                pg.setVisible(k == key)
            for k, btn in self._settings_cat_btns.items():
                if k == key:
                    btn.setStyleSheet(
                        f'QLabel {{ background-color: {C["accent2"]}; '
                        f'color: {C.get("on_accent", "#ffffff")}; '
                        f'padding: 10px 16px; '
                        f'font-weight: bold; }}'
                    )
                else:
                    btn.setStyleSheet(
                        f'QLabel {{ background-color: {C["surface2"]}; '
                        f'color: {C["text_dim"]}; '
                        f'padding: 10px 16px; }}'
                    )
            # 현재 활성 카테고리 기억 (테마 변경 시 사이드바 색 갱신용)
            self._settings_active_cat = key

        # ── 카테고리 버튼 ──────────────────────────────────
        cats = [('directories', '스캔 폴더'),
                ('general',     '일반'),
                ('theme',       '테마'),
                ('launcher',    '실행 프로그램'),
                ('db',          '데이터베이스'),
                ('exclude',     '제외 설정')]
        for key, label in cats:
            btn = QLabel(label, sidebar)
            btn.setCursor(Qt.PointingHandCursor)
            # 카테고리 버튼은 두 상태(active/inactive)가 있어 _switch에서
            # 매번 다시 칠함. register_themed에 비활성 상태 스타일을 등록 →
            # 테마 변경 시 비활성 버튼들이 새 색으로 갱신되고,
            # 활성 카테고리는 _refresh_theme()에서 _switch()를 다시 호출.
            register_themed(btn, lambda: (
                f'QLabel {{ background-color: {C["surface2"]}; '
                f'color: {C["text_dim"]}; padding: 10px 16px; }}'
            ))
            # 클릭 이벤트
            btn.mousePressEvent = (lambda e, k=key: _switch(k))
            sl.addWidget(btn)
            self._settings_cat_btns[key] = btn

        sl.addStretch(1)

        # ── 페이지: 스캔 폴더 ──────────────────────────
        self._build_page_directories(_make_page('directories'))
        # ── 페이지: 일반 ───────────────────────────────────
        self._build_page_general(_make_page('general'))
        # ── 페이지: 테마 ───────────────────────────────────
        self._build_page_theme(_make_page('theme'))
        # ── 페이지: 실행 프로그램 ──────────────────────────
        self._build_page_launcher(_make_page('launcher'))
        # ── 페이지: 데이터베이스 ──────────────────────────
        self._build_page_db(_make_page('db'))
        # ── 페이지: 제외 설정 ────────────────────────────
        self._build_page_exclude(_make_page('exclude'))

        # 초기 페이지
        _switch('directories')
        self._switch_settings_page = _switch

        # ── 모든 입력 위젯에 인라인 스타일 강제 적용 ───────────
        # 페이지가 모두 빌드된 뒤 패널 전체를 재귀 순회해서 위젯 타입별로
        # 스타일을 박는다. 전역 QSS가 부모 컨테이너의 transparent 스타일
        # 등에 가려져 적용 안 되는 경우를 우회.
        self._auto_style_setting_widgets(panel)
        return panel

    def _auto_style_setting_widgets(self, root: QWidget):
        """패널 전체를 순회해서 입력 위젯에 인라인 스타일 적용."""
        from PySide6.QtWidgets import (
            QLineEdit, QCheckBox, QRadioButton, QPushButton,
            QTextEdit, QListWidget,
        )
        for w in root.findChildren(QLineEdit):
            self._style_input(w)
        for w in root.findChildren(QCheckBox):
            self._style_checkbox(w)
        for w in root.findChildren(QRadioButton):
            # 테마 페이지의 카드 안 라디오는 _build_page_theme에서 직접 등록됨
            p = w.parent()
            if p is not None and p.objectName() == 'ThemeCard':
                continue
            self._style_radio(w)
        for w in root.findChildren(QPushButton):
            # objectName이 지정된 강조 버튼(Accent/Blue/Red)은 전역 QSS 사용
            if w.objectName() in ('Accent', 'Blue', 'Red'):
                continue
            self._style_button(w)

    # ── 페이지: 스캔 폴더 ───────────────────────────────────
    def _build_page_directories(self, pg: QWidget):
        v = self._make_page_layout(pg)

        self._settings_page_header(
            pg, v, '스캔 폴더',
            '게임 파일을 스캔할 폴더를 지정합니다.  '
            'NSP, NSZ, XCI, XCZ 형식을 지원합니다.')

        # 폴더 리스트
        self._settings_folder_lb = QListWidget(pg)
        self._settings_folder_lb.itemSelectionChanged.connect(
            self._on_settings_folder_select)
        v.addWidget(self._settings_folder_lb, 1)

        # 선택된 경로 표시
        self._settings_sel_lbl = QLabel('', pg)
        register_themed(self._settings_sel_lbl, lambda: (
            f'color: {C["text_dim"]}; font-family: Consolas;'
        ))
        v.addWidget(self._settings_sel_lbl)

        # 버튼 행
        btn_row = QHBoxLayout()
        btn_row.setSpacing(8)
        self._settings_add_btn = QPushButton('➕  폴더 추가', pg)
        self._settings_add_btn.clicked.connect(self._settings_add_folder)
        register_themed(self._settings_add_btn, lambda: (
            f'QPushButton {{ '
            f'background-color: {C["surface2"]}; '
            f'color: {C["accent2"]}; font-weight: bold; '
            f'border: 1px solid {C["accent2"]}; '
            f'border-radius: 3px; padding: 5px 12px; }}'
            f'QPushButton:hover {{ '
            f'background-color: {C["selected"]}; '
            f'border: 1px solid {C["accent2"]}; }}'
            f'QPushButton:pressed {{ '
            f'background-color: {C["accent2"]}; '
            f'color: {C.get("on_accent", "#ffffff")}; }}'
            f'QPushButton:disabled {{ '
            f'background-color: {C["bg"]}; color: {C["text_dim"]}; '
            f'border: 1px solid {C["border"]}; }}'
        ))
        btn_row.addWidget(self._settings_add_btn)

        rm_btn = QPushButton('🗑  선택 삭제', pg)
        rm_btn.clicked.connect(self._settings_remove_folder)
        btn_row.addWidget(rm_btn)
        btn_row.addStretch(1)
        v.addLayout(btn_row)

    # ── 페이지: 일반 ────────────────────────────────────────────
    def _build_page_general(self, pg: QWidget):
        v = self._make_page_layout(pg)

        self._settings_page_header(
            pg, v, '일반',
            '앱 동작과 라이브러리 표시 방식을 설정합니다.')

        # 이름 표시 우선순위 — 세로 배치 (가독성)
        name_sec = QFrame(pg)
        name_sec.setStyleSheet('background-color: transparent;')
        name_h = QHBoxLayout(name_sec)
        name_h.setContentsMargins(0, 2, 0, 2)
        name_h.setSpacing(8)
        name_lbl = QLabel('이름 표시 우선순위', name_sec)
        name_lbl.setFixedWidth(_LABEL_WIDTH)
        name_lbl.setAlignment(Qt.AlignLeft | Qt.AlignTop)
        name_lbl.setContentsMargins(0, 6, 0, 0)
        register_themed(name_lbl, lambda: f'color: {C["text"]};')
        name_h.addWidget(name_lbl)
        name_rb_col = QVBoxLayout()
        name_rb_col.setSpacing(4)
        self._name_mode_group = QButtonGroup(name_sec)
        cur = db.get_setting('name_mode', 'db')
        for key, label in db.NAME_MODES.items():
            rb = QRadioButton(label, name_sec)
            rb.setChecked(key == cur)
            rb.toggled.connect(
                lambda checked, k=key: checked and (
                    db.set_setting('name_mode', k),
                    self._refresh_library()
                ))
            self._name_mode_group.addButton(rb)
            name_rb_col.addWidget(rb)
        name_h.addLayout(name_rb_col)
        name_h.addStretch(1)
        v.addWidget(name_sec)

        # 좌우 패널 크기 — 위치 기억 + 위치 초기화 세로 배치
        panel_sec = QFrame(pg)
        panel_sec.setStyleSheet('background-color: transparent;')
        panel_h = QHBoxLayout(panel_sec)
        panel_h.setContentsMargins(0, 2, 0, 2)
        panel_h.setSpacing(8)
        panel_lbl = QLabel('좌우 패널 크기', panel_sec)
        panel_lbl.setFixedWidth(_LABEL_WIDTH)
        panel_lbl.setAlignment(Qt.AlignLeft | Qt.AlignTop)
        panel_lbl.setContentsMargins(0, 6, 0, 0)
        register_themed(panel_lbl, lambda: f'color: {C["text"]};')
        panel_h.addWidget(panel_lbl)
        panel_col = QVBoxLayout()
        panel_col.setSpacing(6)
        cb_sash = QCheckBox('위치 기억', panel_sec)
        cb_sash.setChecked(db.get_setting('sash_remember', '1') == '1')
        cb_sash.toggled.connect(
            lambda checked: db.set_setting('sash_remember', '1' if checked else '0'))
        panel_col.addWidget(cb_sash)
        reset_btn = QPushButton('위치 초기화', panel_sec)
        def _reset():
            db.set_setting('sash_pos', '')
            if hasattr(self, '_restore_sash_pos'):
                self._restore_sash_pos()
        reset_btn.clicked.connect(_reset)
        panel_col.addWidget(reset_btn)
        panel_h.addLayout(panel_col)
        panel_h.addStretch(1)
        v.addWidget(panel_sec)

        # 시작 시 최대화
        def _maximized_widget(h, row):
            cb = QCheckBox('시작 시 최대화', row)
            cb.setChecked(db.get_setting('start_maximized', '0') == '1')
            cb.toggled.connect(
                lambda checked: db.set_setting(
                    'start_maximized', '1' if checked else '0'))
            h.addWidget(cb)
        v.addWidget(self._settings_row(pg, '시작 화면 (재시작 필요)',
                                       _maximized_widget))

        # 스캔 항목
        def _scan_opt_widget(h, row):
            for label, key in [('게임', 'scan_game'),
                               ('아이콘', 'scan_icon'),
                               ('스크린샷', 'scan_screenshot')]:
                cb = QCheckBox(label, row)
                cb.setChecked(db.get_setting(key, '1') == '1')
                cb.toggled.connect(
                    lambda checked, k=key: db.set_setting(
                        k, '1' if checked else '0'))
                if key == 'scan_game':
                    cb.setEnabled(False)
                h.addWidget(cb)
        v.addWidget(self._settings_row(pg, '스캔 항목', _scan_opt_widget))

        # 페이징
        def _page_size_widget(h, row):
            on = db.get_setting('paging_on', '0') == '1'

            # 세로 레이아웃으로 체크박스 위, 타이틀 수 아래
            v_wrap = QVBoxLayout()
            v_wrap.setContentsMargins(0, 0, 0, 0)
            v_wrap.setSpacing(4)

            cb = QCheckBox('사용', row)
            cb.setChecked(on)
            v_wrap.addWidget(cb)

            sub_row = QHBoxLayout()
            sub_row.setContentsMargins(0, 0, 0, 0)
            sub_row.setSpacing(6)

            lbl_page = QLabel('타이틀 개수:', row)
            register_themed(lbl_page, lambda: f'color: {C["text_dim"]};')
            sub_row.addWidget(lbl_page)

            size_edit = QLineEdit(db.get_setting('page_size', '500'), row)
            size_edit.setFixedWidth(70)
            size_edit.setValidator(QIntValidator(1, 99999, row))
            size_edit.setEnabled(on)
            sub_row.addWidget(size_edit)
            sub_row.addStretch()

            v_wrap.addLayout(sub_row)
            h.addLayout(v_wrap)

            def _on_toggle(checked):
                db.set_setting('paging_on', '1' if checked else '0')
                size_edit.setEnabled(checked)
                self._current_page = 0
                if hasattr(self, '_populate_table'):
                    self._populate_table()

            def _on_size_change(text):
                if text.isdigit() and int(text) > 0:
                    db.set_setting('page_size', text)
                    self._current_page = 0
                    if hasattr(self, '_populate_table'):
                        self._populate_table()

            cb.toggled.connect(_on_toggle)
            size_edit.textChanged.connect(_on_size_change)
        v.addWidget(self._settings_row(pg, '페이징', _page_size_widget))

        # 스캔 스레드
        def _thread_widget(h, row):
            cpu = os.cpu_count() or 8
            default_threads = min(16, cpu)
            min_reco = max(2, cpu // 2)
            max_reco = min(16, cpu)

            te = QLineEdit(
                db.get_setting('max_scan_threads', str(default_threads)), row)
            te.setFixedWidth(60)
            te.setValidator(QIntValidator(0, 20, row))

            def _on_change():
                val = te.text().strip()
                if val.isdigit() and 0 <= int(val) <= 20:
                    db.set_setting('max_scan_threads', val)
            te.editingFinished.connect(_on_change)

            h.addWidget(QLabel('스레드 개수:', row))
            h.addWidget(te)
            reco = QLabel(
                f'({cpu}코어 감지 / {min_reco}~{max_reco} 추천)', row)
            register_themed(reco, lambda: f'color: {C["text_dim"]};')
            h.addWidget(reco)
        v.addWidget(self._settings_row(pg, '스캔 스레드', _thread_widget))

        v.addStretch(1)

    # ── 페이지: 실행 프로그램 ─────────────────────────────────
    def _build_page_launcher(self, pg: QWidget):
        v = self._make_page_layout(pg)

        self._settings_page_header(
            pg, v, '실행 프로그램',
            '더블클릭 또는 실행 버튼으로 파일을 열 때 사용할 프로그램을 '
            '지정합니다. 비어 있으면 OS 기본 연결 프로그램으로 실행됩니다.')

        # ── 자동 저장 헬퍼 (debounce 400ms) ────────────────
        # 텍스트 입력은 타이핑 중 매 키스트로크마다 저장하지 않고
        # 마지막 입력 후 400ms 뒤에 한 번만 저장 → DB 부하 최소화
        def _make_autosave(setting_key: str, line_edit: QLineEdit):
            timer = QTimer(line_edit)
            timer.setSingleShot(True)
            timer.setInterval(400)
            timer.timeout.connect(
                lambda: db.set_setting(setting_key,
                                       line_edit.text().strip()))
            line_edit.textChanged.connect(lambda _t: timer.start())

        # 사용 여부
        def _enabled_widget(h, row):
            self._launcher_use_cb = QCheckBox(
                '지정한 프로그램으로 실행', row)
            self._launcher_use_cb.setChecked(
                db.get_setting('launcher_enabled', '0') == '1')
            h.addWidget(self._launcher_use_cb)
        v.addWidget(self._settings_row(pg, '실행 방식', _enabled_widget))

        # 프로그램 경로
        def _path_widget(h, row):
            self._launcher_path_edit = QLineEdit(
                db.get_setting('launcher_path', ''), row)
            self._launcher_path_edit.setPlaceholderText(
                'C:\\Program Files\\yuzu\\yuzu.exe  '
                '또는  /usr/bin/ryujinx')
            h.addWidget(self._launcher_path_edit, 1)
            browse_btn = QPushButton('찾기…', row)
            h.addWidget(browse_btn)
            self._launcher_browse_btn = browse_btn

            def _browse():
                if platform.system() == 'Windows':
                    filt = '실행 파일 (*.exe *.bat *.cmd);;모든 파일 (*)'
                else:
                    filt = '모든 파일 (*)'
                path, _ = QFileDialog.getOpenFileName(
                    self.window(), '실행 프로그램 선택', '', filt)
                if path:
                    self._launcher_path_edit.setText(path)

            browse_btn.clicked.connect(_browse)
            _make_autosave('launcher_path', self._launcher_path_edit)
        v.addWidget(self._settings_row(pg, '프로그램 경로', _path_widget))

        # 추가 인자
        def _args_widget(h, row):
            self._launcher_args_edit = QLineEdit(
                db.get_setting('launcher_args', ''), row)
            self._launcher_args_edit.setPlaceholderText(
                '예) --fullscreen   ({path} 는 파일 경로로 자동 치환)')
            h.addWidget(self._launcher_args_edit, 1)
            _make_autosave('launcher_args', self._launcher_args_edit)
        v.addWidget(self._settings_row(pg, '추가 인자', _args_widget))

        # 인자 도움말 (왼쪽 라벨 폭만큼 들여쓰기)
        hint_wrap = QFrame(pg)
        hw = QHBoxLayout(hint_wrap)
        hw.setContentsMargins(_LABEL_WIDTH + 8, 0, 0, 4)
        hw.setSpacing(0)
        hw.addWidget(self._settings_hint(
            pg,
            '💡  인자에 {path} 를 입력하면 실행 시 파일 경로로 자동 치환됩니다. '
            '없으면 경로가 마지막 인자로 자동 추가됩니다.'), 1)
        v.addWidget(hint_wrap)

        # ── 자동 압축 해제 섹션 (구분선) ──────────
        gap = QFrame(pg)
        gap_l = QVBoxLayout(gap)
        gap_l.setContentsMargins(0, _SECTION_GAP - _ROW_GAP, 0, 0)
        gap_l.setSpacing(0)
        gap_l.addWidget(self._settings_separator(pg))
        v.addWidget(gap)

        NSL_WORKSPACE = r'C:\Users\Public\NSL_Workspace'

        def _decomp_widget(h, row):
            self._decomp_cb = QCheckBox(
                'NSZ / XCZ 자동 압축 해제', row)
            self._decomp_cb.setChecked(
                db.get_setting('decompress_auto', '0') == '1')
            self._decomp_cb.toggled.connect(
                lambda checked: db.set_setting(
                    'decompress_auto', '1' if checked else '0'))
            h.addWidget(self._decomp_cb)
        v.addWidget(self._settings_row(pg, '압축 해제', _decomp_widget))

        # 도움말
        decomp_hint_wrap = QFrame(pg)
        dhw = QHBoxLayout(decomp_hint_wrap)
        dhw.setContentsMargins(_LABEL_WIDTH + 8, 0, 0, 4)
        dhw.setSpacing(0)
        dhw.addWidget(self._settings_hint(
            pg,
            f'💡  NSZ → NSP, XCZ → XCI 로 변환 후 실행합니다. '
            f'({NSL_WORKSPACE})는 매번 실행 전에 자동으로 비워집니다.'), 1)
        v.addWidget(decomp_hint_wrap)

        v.addStretch(1)

        # ── 활성/비활성 동기화 + 자동 저장 (체크박스 → 즉시 저장) ──
        def _toggle_launcher(checked):
            db.set_setting('launcher_enabled', '1' if checked else '0')
            self._launcher_path_edit.setEnabled(checked)
            self._launcher_args_edit.setEnabled(checked)
            self._launcher_browse_btn.setEnabled(checked)

        # 초기 상태 반영
        _toggle_launcher(self._launcher_use_cb.isChecked())
        self._launcher_use_cb.toggled.connect(_toggle_launcher)

    # ── 페이지: 테마 ──────────────────────────────────────────────────
    def _build_page_theme(self, pg: QWidget):
        """
        테마 선택 페이지 (실시간 적용).
        - 2열 그리드에 각 테마 카드 표시
        - 카드 좌측: 해당 테마의 색상 미리보기 (4색 스와치)
        - 카드 우측: 테마 이름 (현재 활성 테마의 텍스트 색)
        - 선택 시: theme_module.apply_theme() 즉시 호출 → 전역 QSS 재적용 +
          register_themed로 등록된 모든 위젯의 인라인 스타일 재칠
        """
        from PySide6.QtWidgets import QGridLayout, QButtonGroup, QApplication

        v = QVBoxLayout(pg)
        v.setContentsMargins(*_PAGE_MARGINS)
        v.setSpacing(_ROW_GAP)

        self._settings_page_header(
            pg, v, '테마',
            '앱 전체의 색상 테마를 선택합니다. 선택 즉시 모든 화면에 반영됩니다.')

        # 카드 그리드
        grid_wrap = QWidget(pg)
        grid = QGridLayout(grid_wrap)
        grid.setContentsMargins(0, 4, 0, 8)
        grid.setSpacing(10)

        btn_group = QButtonGroup(pg)
        btn_group.setExclusive(True)

        # 카드 위젯 참조 보관 — 테마 변경 시 모든 카드의 active 상태와
        # "현재 사용 중" 뱃지를 함께 갱신해야 함.
        self._theme_cards = {}  # tid → {'card': QFrame, 'badge': QLabel|None}

        theme_list = theme_module.get_theme_list()
        COLS = 2

        for i, (tid, label) in enumerate(theme_list):
            td = theme_module.THEMES[tid]
            row, col = divmod(i, COLS)
            is_active = (tid == theme_module.get_current_theme())

            # ── 카드 컨테이너 ─────────────────────────────
            card = QFrame(grid_wrap)
            card.setObjectName('ThemeCard')
            card.setMinimumHeight(76)
            card.setCursor(Qt.PointingHandCursor)

            # ── 카드 스타일 — 현재 활성 테마 색으로 통일 ───
            # 카드 배경/테두리/텍스트 모두 현재 테마 기준으로 통일.
            # 활성 카드만 accent2 2px 테두리로 강조.
            # register_themed에 등록해 테마 변경 시 자동 재칠.
            def _card_style(active: bool):
                bg_color  = C['surface2']
                bd_color  = C['accent2'] if active else C['border']
                bd_width  = 2 if active else 1
                return (
                    f'QFrame {{ background-color: {bg_color}; '
                    f'border: {bd_width}px solid {bd_color}; '
                    f'border-radius: 6px; }}'
                )
            card.setStyleSheet(_card_style(is_active))
            # 테마 변경 시 모든 카드를 재칠해야 하므로 style_fn은 active=False 기준으로
            # register_themed에 등록하지 않고, _on_select에서 직접 갱신한다.

            cl = QHBoxLayout(card)
            cl.setContentsMargins(14, 12, 14, 12)
            cl.setSpacing(12)

            # ── 색상 스와치 (2색: surface + accent2) ──────
            # 레이아웃 배경 대표색 2개만 표시. 한눈에 테마 톤을 파악.
            swatch_wrap = QFrame(card)
            swatch_wrap.setFixedSize(40, 36)
            swatch_wrap.setStyleSheet(
                'background-color: transparent; border: none;')
            sw_l = QHBoxLayout(swatch_wrap)
            sw_l.setContentsMargins(0, 0, 0, 0)
            sw_l.setSpacing(3)
            for ck in ('surface', 'accent2'):
                chip = QFrame(swatch_wrap)
                chip.setStyleSheet(
                    f'background-color: {td[ck]}; '
                    f'border: 1px solid {td["border"]}; '
                    f'border-radius: 3px;')
                sw_l.addWidget(chip, 1)
            cl.addWidget(swatch_wrap, 0)

            # ── 라디오 + 라벨 — 현재 테마 색으로 통일 ────
            rb = QRadioButton(label, card)
            rb.setChecked(is_active)
            # 라디오는 전역 QSS에서 처리 (Windows 스타일)
            # 라벨 색만 현재 테마의 text 색으로 고정
            rb.setStyleSheet(
                f'QRadioButton {{ background-color: transparent; '
                f'color: {C["text"]}; font-weight: bold; }}'
            )
            btn_group.addButton(rb)
            cl.addWidget(rb, 1)

            self._theme_cards[tid] = {'card': card,
                                      'style_fn': _card_style,
                                      'radio': rb}

            # ── 클릭/선택 로직 ───────────────────────────
            def _on_select(checked, theme_id=tid):
                if not checked:
                    return
                # DB 저장
                db.set_setting('ui_theme', theme_id)
                # 즉시 테마 적용 (전역 QSS + register_themed 등록 위젯들)
                theme_module.apply_theme(theme_id, QApplication.instance())
                # 모든 카드의 active 상태 갱신 (배경+테두리+라디오 색)
                for cid, info in self._theme_cards.items():
                    active = (cid == theme_id)
                    info['card'].setStyleSheet(info['style_fn'](active))
                    if 'radio' in info:
                        info['radio'].setStyleSheet(
                            f'QRadioButton {{ background-color: transparent; '
                            f'color: {C["text"]}; font-weight: bold; }}'
                        )
                # 설정 사이드바의 활성 카테고리 색을 새 테마로 다시 칠해야 함
                # (_switch가 항상 모든 버튼을 다시 칠하므로 한번 호출이면 충분)
                if (hasattr(self, '_switch_settings_page')
                        and hasattr(self, '_settings_active_cat')):
                    self._switch_settings_page(self._settings_active_cat)

                # 게임 리스트(QTreeView) 행 색은 QStandardItem에 박혀있어
                # 테마 변경만으로는 갱신 안 됨. _apply_filters() 호출로
                # 모델 재구성해 새 base/update/dlc 색이 즉시 반영되게 함.
                try:
                    if hasattr(self, '_apply_filters'):
                        self._apply_filters()
                except Exception:
                    pass
                # 현재 선택된 게임의 상세 패널도 색이 박혀있을 수 있으니 재선택
                try:
                    if hasattr(self, '_refresh_current_selection'):
                        self._refresh_current_selection()
                except Exception:
                    pass

            rb.toggled.connect(_on_select)

            # 카드 전체 클릭 → 라디오 선택
            def _make_click_handler(radio):
                def _h(ev):
                    if ev.button() == Qt.LeftButton:
                        radio.setChecked(True)
                return _h
            card.mousePressEvent = _make_click_handler(rb)

            grid.addWidget(card, row, col)

        # 2열 grid가 row 방향으로만 늘어나도록 stretch 부여
        grid.setColumnStretch(0, 1)
        grid.setColumnStretch(1, 1)
        v.addWidget(grid_wrap)

        v.addWidget(self._settings_hint(
            pg,
            '💡  변경 사항은 즉시 모든 화면에 반영됩니다. '
            '일부 위젯(이미 표시된 게임 설명 본문 등)은 다른 게임을 '
            '선택하면 새 테마 색으로 갱신됩니다.'))
        v.addStretch(1)

    # ── 페이지: 데이터베이스 ───────────────────────────────────
    def _build_page_db(self, pg: QWidget):
        v = self._make_page_layout(pg)

        self._settings_page_header(
            pg, v, '데이터베이스',
            '타이틀 정보를 가져올 원본 소스를 선택합니다.')

        # 타이틀 DB 소스
        def _src_widget(h, row):
            col = QVBoxLayout()
            col.setSpacing(4)
            rb_custom  = QRadioButton('자체 서버 (기본, 빠름)', row)
            rb_tinfoil = QRadioButton('Tinfoil 공식 (titles.json)', row)
            cur = db.get_setting('titledb_source', 'custom')
            rb_custom.setChecked(cur == 'custom')
            rb_tinfoil.setChecked(cur == 'tinfoil')

            grp = QButtonGroup(row)
            grp.addButton(rb_custom)
            grp.addButton(rb_tinfoil)

            rb_custom.toggled.connect(
                lambda checked: checked and
                db.set_setting('titledb_source', 'custom'))
            rb_tinfoil.toggled.connect(
                lambda checked: checked and
                db.set_setting('titledb_source', 'tinfoil'))

            col.addWidget(rb_custom)
            col.addWidget(rb_tinfoil)
            h.addLayout(col)
        v.addWidget(self._settings_row(pg, '타이틀 DB 소스', _src_widget))

        # URL 안내 (라벨 폭만큼 들여쓰기)
        info_wrap = QFrame(pg)
        iw = QHBoxLayout(info_wrap)
        iw.setContentsMargins(_LABEL_WIDTH + 8, 0, 0, 0)
        iw.setSpacing(0)
        info = self._settings_hint(
            pg,
            '자체 서버 URL:  '
            'https://github.com/UzuCore/NSL/releases/download/titledb/titles.json')
        iw.addWidget(info, 1)
        v.addWidget(info_wrap)

        v.addStretch(1)

    # ── 페이지: 제외 설정 ───────────────────────────────────────
    def _build_page_exclude(self, pg: QWidget):
        v = self._make_page_layout(pg)

        self._settings_page_header(
            pg, v, '제외 설정',
            '미보유 목록(누락된 업데이트 / DLC)에서 제외할 ID를 입력합니다. '
            '여러 개는 줄바꿈으로 구분하세요.')

        # 업데이트 제외 — stretch=1 부여해서 충분한 높이 확보
        v.addWidget(self._build_exclude_box(
            'update', '업데이트 제외  (타이틀 ID)', pg), 1)
        # DLC 제외
        v.addWidget(self._build_exclude_box(
            'dlc',    'DLC 제외  (DLC ID)',       pg), 1)

    def _build_exclude_box(self, kind: str, title: str, parent) -> QWidget:
        box = QFrame(parent)
        bl = QVBoxLayout(box)
        bl.setContentsMargins(0, 8, 0, 0)
        bl.setSpacing(6)

        lbl = self._settings_section_label(box, title)
        bl.addWidget(lbl)

        te = QTextEdit(box)
        te.setLineWrapMode(QTextEdit.NoWrap)
        te.setMinimumHeight(140)
        # 인라인 스타일로 강제 — 전역 QSS가 가려져도 무조건 적용됨.
        # QTextEdit는 내부 viewport(별도 QWidget)가 별도 배경을 가지므로
        # 셀렉터를 분리해서 둘 다 칠해줘야 빈 영역이 확실히 보임.
        register_themed(te, lambda: (
            f'QTextEdit {{ '
            f'background-color: {C.get("input_bg", C["bg"])}; '
            f'color: {C["text"]}; '
            f'border: 1px solid {C.get("input_border", C["accent2"])}; '
            f'border-radius: 4px; '
            f'padding: 6px 8px; '
            f'selection-background-color: {C["selected"]}; '
            f'}}'
            f'QTextEdit:focus {{ '
            f'border: 1px solid {C["accent2"]}; '
            f'}}'
        ))
        # viewport 자체에도 명시적으로 배경을 지정 (QSS 셀렉터로는
        # QTextEdit > QWidget 매칭이 환경에 따라 불안정한 경우가 있어
        # palette로 직접 설정)
        from PySide6.QtGui import QPalette, QColor
        def _apply_viewport_bg():
            pal = te.viewport().palette()
            pal.setColor(QPalette.Base,
                         QColor(C.get("input_bg", C["bg"])))
            pal.setColor(QPalette.Window,
                         QColor(C.get("input_bg", C["bg"])))
            te.viewport().setPalette(pal)
            te.viewport().setAutoFillBackground(True)
        _apply_viewport_bg()
        # 테마 변경 시 viewport 배경도 다시 칠하도록 등록
        # (별도 위젯이지만 register_themed로 묶기 위해 dummy 함수 등록)
        register_themed(te.viewport(), lambda: (
            f'background-color: {C.get("input_bg", C["bg"])};'
        ))

        try:
            existing = '\n'.join(sorted(db.get_exception_tids(kind)))
        except Exception:
            existing = ''
        te.setPlainText(existing)

        # 자동 저장 (debounce 500ms)
        save_timer = QTimer(box)
        save_timer.setSingleShot(True)
        save_timer.setInterval(500)

        def _save():
            tids = [l.strip().upper()
                    for l in te.toPlainText().splitlines() if l.strip()]
            try:
                db.set_exception_tids(kind, tids)
                if hasattr(self, '_apply_filters'):
                    self._apply_filters()
            except Exception:
                pass

        save_timer.timeout.connect(_save)
        te.textChanged.connect(lambda: save_timer.start())
        bl.addWidget(te, 1)
        return box

    # ── 설정 패널 갱신 / 폴더 처리 ────────────────────────────
    def _refresh_settings_panel(self):
        if not hasattr(self, '_settings_folder_lb'):
            return
        self._settings_folder_lb.clear()
        folders = db.get_scan_folders()
        for f in folders:
            self._settings_folder_lb.addItem(f)
        self._settings_sel_lbl.setText('')

        if hasattr(self, '_settings_add_btn'):
            at_limit = len(folders) >= self._MAX_SCAN_FOLDERS
            self._settings_add_btn.setEnabled(not at_limit)
            self._settings_add_btn.setText(
                f'➕  폴더 추가 ({len(folders)}/{self._MAX_SCAN_FOLDERS})')

    def _on_settings_folder_select(self):
        items = self._settings_folder_lb.selectedItems()
        if items:
            self._settings_sel_lbl.setText(items[0].text())
        else:
            self._settings_sel_lbl.setText('')

    def _settings_add_folder(self):
        folders = db.get_scan_folders()
        if len(folders) >= self._MAX_SCAN_FOLDERS:
            QMessageBox.warning(
                self.window(), '폴더 한도 초과',
                f'스캔 폴더는 최대 {self._MAX_SCAN_FOLDERS}개까지 추가할 수 있습니다.\n'
                '기존 폴더를 삭제한 후 추가해 주세요.')
            return
        path = QFileDialog.getExistingDirectory(
            self.window(), '스캔할 폴더 선택')
        if not path:
            return
        if path not in folders:
            db.add_scan_folder(path)
            self._refresh_settings_panel()
            if hasattr(self, '_refresh_dir_tabs'):
                self._refresh_dir_tabs()
            self._set_status(f'폴더 추가: {path}')

    def _settings_remove_folder(self):
        items = self._settings_folder_lb.selectedItems()
        if not items:
            QMessageBox.information(
                self.window(), '알림', '삭제할 폴더를 선택하세요.')
            return
        path = items[0].text()
        ans = QMessageBox.question(
            self.window(), '삭제 확인',
            f'"{path}"\n목록에서 제거할까요?',
            QMessageBox.Yes | QMessageBox.No)
        if ans == QMessageBox.Yes:
            db.remove_scan_folder(path)
            self._refresh_settings_panel()
            if hasattr(self, '_refresh_dir_tabs'):
                self._refresh_dir_tabs()
            self._set_status(f'폴더 제거: {path}')
