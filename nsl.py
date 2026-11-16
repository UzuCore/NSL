# nsl.py — Nintendo Switch Library Manager (PySide6 / Qt6)
import os
import sys

# ── UTF-8 강제 (한국어 윈도우 cp949 환경에서 인코딩 오류 방지) ──
# 어떤 import 이전에 적용되어야 효과적임
if sys.platform == 'win32':
    os.environ.setdefault('PYTHONUTF8', '1')
    os.environ.setdefault('PYTHONIOENCODING', 'utf-8')
    try:
        # Python 3.7+: 표준 입출력의 인코딩을 UTF-8로 재설정
        sys.stdout.reconfigure(encoding='utf-8', errors='replace')
        sys.stderr.reconfigure(encoding='utf-8', errors='replace')
    except (AttributeError, ValueError):
        pass

import platform
import subprocess
from pathlib import Path
from collections import OrderedDict

from PySide6.QtCore import (
    Qt, QTimer, Signal, QSize, QPoint,
    QItemSelectionModel,
)
from PySide6.QtGui import (
    QFont, QIcon, QKeySequence, QStandardItemModel,
    QGuiApplication, QShortcut,
)
from PySide6.QtWidgets import (
    QApplication, QMainWindow, QWidget, QFrame, QLabel, QPushButton,
    QLineEdit, QTreeView, QHeaderView, QSplitter, QStatusBar,
    QMenu, QHBoxLayout, QVBoxLayout,
    QProgressBar, QMessageBox,
    QPlainTextEdit, QAbstractItemView,
    QListWidget, QStackedWidget,
)

import nsl_database as db
import nsl_scanner as scanner
from nsl_common   import get_base_path

from nsl_theme    import C, _FS, apply_stylesheet, set_theme, register_themed
from nsl_settings import SettingsPanelMixin
from nsl_detail     import DetailPanelMixin
from nsl_translate  import TranslateMixin
from nsl_screenshot import ScreenshotMixin
from nsl_icon import IconMixin
from nsl_screenshots_dl import ScreenshotsDownloadMixin
from nsl_library    import LibraryMixin
from nsl_scan       import ScanMixin
from nsl_decompress import DecompressMixin

# ─────────────────────────────────────────────────────────────────
_MAX_SCAN_FOLDERS = 5
_ICON_CACHE_MAX   = 120

APP_NAME    = 'Nintendo Switch Library'
APP_SNAME   = 'NSL'
APP_VERSION = '2.0.0'


def get_resource_path(relative_path):
    base = sys._MEIPASS if (getattr(sys, 'frozen', False)
                            and hasattr(sys, '_MEIPASS')) else os.path.abspath('.')
    return os.path.join(base, relative_path)


def _force_dark_titlebar(hwnd):
    """
    Windows 타이틀바를 항상 다크 모드로 강제 고정.
    Windows 10 Build 19041(2004) 이상에서 동작.
    윈도우 테마(라이트/다크) 설정에 관계없이 항상 어두운 타이틀바를 유지한다.
    """
    if platform.system() != 'Windows':
        return
    try:
        import ctypes
        import ctypes.wintypes

        DWMWA_USE_IMMERSIVE_DARK_MODE = 20   # Windows 10 20H1+
        DWMWA_USE_IMMERSIVE_DARK_MODE_OLD = 19  # 구버전 폴백

        dwmapi = ctypes.windll.dwmapi
        value = ctypes.c_int(1)  # 1 = 다크 모드 강제

        # 최신 속성값 먼저 시도 (20H1+)
        result = dwmapi.DwmSetWindowAttribute(
            hwnd,
            DWMWA_USE_IMMERSIVE_DARK_MODE,
            ctypes.byref(value),
            ctypes.sizeof(value),
        )
        if result != 0:
            # 구버전 빌드 폴백 (1903/1909)
            dwmapi.DwmSetWindowAttribute(
                hwnd,
                DWMWA_USE_IMMERSIVE_DARK_MODE_OLD,
                ctypes.byref(value),
                ctypes.sizeof(value),
            )
    except Exception:
        pass


# ─────────────────────────────────────────────────────────────────
# 메인 윈도우
# ─────────────────────────────────────────────────────────────────
class SwitchLibraryManager(
    QMainWindow,
    # MRO 순서가 중요: 같은 메서드명이 있을 때 앞에 오는 클래스가 우선.
    # ScreenshotsDownloadMixin._cleanup_orphans / _auto_prefetch_screenshots
    # 가 LibraryMixin의 동명 메서드보다 우선되도록 앞에 배치.
    SettingsPanelMixin,
    DetailPanelMixin, TranslateMixin, ScreenshotMixin,
    IconMixin, ScreenshotsDownloadMixin,
    DecompressMixin,
    ScanMixin,
    LibraryMixin,
):
    # ── 시그널 ────────────────────────────────────────────
    # 워커 스레드 → 메인 스레드로 안전하게 호출하기 위한 시그널
    log_signal      = Signal(str, str)              # (msg, level)
    status_signal   = Signal(str)
    progress_signal = Signal(int, int)              # (cur, total)
    refresh_signal  = Signal()                      # 라이브러리 갱신
    invoke_signal   = Signal(object)                # 임의의 콜러블을 메인 스레드에서 실행

    def __init__(self):
        super().__init__()
        self.setWindowTitle(f'{APP_SNAME}  v{APP_VERSION}')

        # 아이콘
        icon_path = get_resource_path(os.path.join('assets', 'icon.ico'))
        if os.path.exists(icon_path):
            self.setWindowIcon(QIcon(icon_path))

        self.resize(1400, 860)
        self.setMinimumSize(1400, 860)

        # ── DB 초기화 ───────────────────────────────────
        db.init_db()

        # ── 상태 변수 ───────────────────────────────────
        self._MAX_SCAN_FOLDERS = _MAX_SCAN_FOLDERS
        self._ICON_CACHE_MAX   = _ICON_CACHE_MAX
        self._app_name         = APP_NAME
        self._app_version      = APP_VERSION

        self._library:  list = []
        self._filtered: list = []
        self._current_page   = 0
        # 시작 시 라이브러리 데이터를 비동기로 로드하는 동안 True.
        # _on_startup_done에서 False로 바뀌며, 그 전까지는 환영/테이블 화면
        # 전환을 막아 깜빡임을 방지.
        self._startup_loading = True
        self.base_dir        = Path(get_base_path())
        self.screenshots_dir = self.base_dir / 'screenshots'
        self.screenshots_dir.mkdir(parents=True, exist_ok=True)
        self.icons_dir = self.base_dir / 'icons'
        self.icons_dir.mkdir(parents=True, exist_ok=True)
        self._sort_key        = 'name'
        self._sort_asc        = True
        self._scanning        = False
        self._icon_extracting = False
        # 종료 확인용 추가 상태 플래그
        self._ss_downloading       = False  # 스크린샷 다운로드 중
        self._icon_dl_running      = False  # 수동 아이콘 추출(다운로드) 중
        self._titledb_running      = False  # 타이틀 DB 다운로드 중
        self._cleanup_running      = False  # 더미데이터 정리 중
        # _decompress_running 은 _run_file 안에서 동적으로 설정됨
        self._scan_hits       = 0
        self._scan_misses     = 0
        self._current_filter_dir = None
        scanner.version_to_str = lambda v: '-' if v is None else str(int(v))

        self._icon_cache = OrderedDict()
        self._icon_jobs  = set()

        # 검색 텍스트 (tk.StringVar 대체)
        self._search_text = ''

        # 코나미

        # 로그 버퍼
        self._log_buf = []

        # 행 선택 디바운스 타이머 (재사용) — 매 선택마다 새로 만들면
        # Qt 부모 트리에 누적되므로 한 번만 생성.
        self._select_timer = QTimer(self)
        self._select_timer.setSingleShot(True)
        self._select_timer.setInterval(100)
        self._select_timer.timeout.connect(self._on_select_timeout)
        self._select_pending_rows: list = []

        # ── UI 빌드 ──────────────────────────────────────
        self._build_ui()

        # 시그널/슬롯 연결
        self.log_signal.connect(self._append_log)
        self.status_signal.connect(self._set_status)
        self.progress_signal.connect(self._set_progress)
        self.refresh_signal.connect(self._on_async_refresh)
        # 워커 → 메인 스레드 임의 콜백 디스패처
        self.invoke_signal.connect(lambda fn: fn() if callable(fn) else None)

        # 시작 시 최대화
        if db.get_setting('start_maximized', '0') == '1':
            self.showMaximized()

        # Windows 타이틀바를 항상 다크 모드로 고정
        # (Windows 테마 설정에 관계없이 어두운 타이틀바 유지)
        QTimer.singleShot(0, self._apply_dark_titlebar)

        # 비동기 시작
        QTimer.singleShot(0, self._async_startup)

        # 로그 플러시 타이머
        self._log_timer = QTimer(self)
        self._log_timer.setInterval(100)
        self._log_timer.timeout.connect(self._flush_log_timer)
        self._log_timer.start()

        # 리소스 사용량 갱신
        self._res_timer = QTimer(self)
        self._res_timer.setInterval(2000)
        self._res_timer.timeout.connect(self._update_resource_usage)
        QTimer.singleShot(1000, self._update_resource_usage)
        self._res_timer.start()

    # ── 스레드 헬퍼 ──────────────────────────────────────────
    def _apply_dark_titlebar(self):
        """창 핸들(HWND)을 가져와 타이틀바를 다크 모드로 고정."""
        try:
            hwnd = int(self.winId())
            _force_dark_titlebar(hwnd)
        except Exception:
            pass

    def _invoke_main(self, fn):
        """워커 스레드에서 메인 스레드로 안전하게 콜러블을 디스패치."""
        self.invoke_signal.emit(fn)

    def _run_in_thread(self, fn):
        """
        워커를 별도 스레드에서 실행. tkinter의 threading.Thread 호환.
        UI 갱신은 반드시 self.invoke_signal/_invoke_main 또는 status/progress
        시그널을 통해 메인 스레드로 디스패치해야 합니다.
        """
        import threading as _th
        t = _th.Thread(target=fn, daemon=True)
        t.start()
        # 스레드 객체 보유 (디버깅 용; 데몬이라 종료 시 자동 정리)
        if not hasattr(self, '_threads'):
            self._threads = []
        self._threads = [x for x in self._threads if x.is_alive()]
        self._threads.append(t)

    # ── UI 빌드 ──────────────────────────────────────────────
    def _build_ui(self):
        # 중앙 위젯
        central = QWidget(self)
        self.setCentralWidget(central)
        cv = QVBoxLayout(central)
        cv.setContentsMargins(6, 0, 6, 4)
        cv.setSpacing(4)

        self._build_menu()
        self._build_toolbar()

        # 본문 (수평 스플리터: 좌측 라이브러리 | 우측 상세)
        self.pw = QSplitter(Qt.Horizontal, central)

        # 좌측 컨테이너 (라이브러리 + 설정 패널이 자리를 공유)
        self._left_container = QWidget(self.pw)
        lc_layout = QVBoxLayout(self._left_container)
        lc_layout.setContentsMargins(0, 0, 0, 0)
        lc_layout.setSpacing(0)

        # 테이블 영역과 설정 패널을 같은 layout에 추가하고 가시성으로 토글
        self._table_frame    = self._build_right(self._left_container)
        self._settings_panel = self._build_settings_panel(self._left_container)

        lc_layout.addWidget(self._table_frame)
        lc_layout.addWidget(self._settings_panel)

        self._settings_panel.hide()
        self._settings_visible = False

        # 우측 상세 패널
        self._detail_frame = self._build_detail_panel(self.pw)
        self._detail_frame.hide()

        self.pw.addWidget(self._left_container)
        self.pw.addWidget(self._detail_frame)
        self.pw.setStretchFactor(0, 1)
        self.pw.setStretchFactor(1, 0)
        self.pw.setSizes([1400, 0])

        cv.addWidget(self.pw, 1)

        # ── 로그 패널 ───────────────────────────────────
        self._log_frame = QFrame(central)
        self._log_frame.setObjectName('Surface')
        self._log_frame.setAutoFillBackground(True)
        log_layout = QVBoxLayout(self._log_frame)
        log_layout.setContentsMargins(8, 2, 8, 4)
        log_layout.setSpacing(2)

        log_header = QHBoxLayout()
        lh_lbl = QLabel('로그', self._log_frame)
        lh_lbl.setObjectName('Dim')
        log_header.addWidget(lh_lbl)
        log_header.addStretch(1)
        log_close = QPushButton('X', self._log_frame)
        log_close.setFixedWidth(28)
        log_close.clicked.connect(self._hide_log)
        log_header.addWidget(log_close)
        log_layout.addLayout(log_header)

        self._log_text = QPlainTextEdit(self._log_frame)
        self._log_text.setReadOnly(True)
        self._log_text.setMaximumHeight(150)
        register_themed(self._log_text, lambda fs=_FS: (
            f'background-color: {C["surface2"]}; color: {C["text"]}; '
            f'font-family: Consolas, monospace; font-size: {max(fs-1, 8)}pt;'
        ))
        log_layout.addWidget(self._log_text)

        cv.addWidget(self._log_frame)
        self._log_visible = True
        self._hide_log()

        # 상태바
        self._build_statusbar()

    # ── 메뉴바 ────────────────────────────────────────────
    def _build_menu(self):
        bar = self.menuBar()

        # 파일
        m_file = bar.addMenu('파일')
        m_file.addAction('CSV 내보내기...', self._export_csv)
        m_file.addSeparator()
        m_file.addAction(
            '파일명 일괄 정규화', self._normalize_current_tab_filenames)
        m_file.addSeparator()
        a_quit = m_file.addAction('종료', self.close)
        a_quit.setShortcut(QKeySequence('Alt+F4'))

        # 라이브러리
        m_lib = bar.addMenu('라이브러리')
        m_lib.addAction('전체 스캔', self._scan_all)
        a_quick = m_lib.addAction('빠른 스캔', self._incremental_scan)
        a_quick.setShortcut(QKeySequence('F5'))
        m_lib.addAction('스캔 중지', self._stop_scan)
        m_lib.addSeparator()
        m_lib.addAction('캐시 현황', self._show_cache_stats)
        m_lib.addSeparator()
        m_lib.addAction('더미 데이터 정리', self._cleanup_orphans)
        m_lib.addSeparator()
        m_lib.addAction('라이브러리 초기화', self._clear_library)

        # 데이터베이스
        m_tdb = bar.addMenu('데이터베이스')
        m_tdb.addAction('타이틀 DB 다운로드', self._update_titledb)
        m_tdb.addAction('타이틀 DB 매칭',     self._load_local_titledb)
        m_tdb.addAction('타이틀 DB 현황',     self._show_titledb_files)
        m_tdb.addSeparator()
        m_tdb.addAction('전체 아이콘 스캔',     self._prefetch_icons_after_scan)
        m_tdb.addAction('전체 스크린샷 다운로드', lambda: self._download_screenshots())
        m_tdb.addSeparator()
        m_tdb.addAction('타이틀 DB 초기화', self._clear_titledb)

        # 도움말
        m_help = bar.addMenu('도움말')
        m_help.addAction('업데이트 확인', self._check_update)
        m_help.addSeparator()
        m_help.addAction('단축키', self._show_shortcuts)
        m_help.addAction('정보',   self._about)

        # 단축키
        QShortcut(QKeySequence('Ctrl+F'), self, self._focus_search)
        QShortcut(QKeySequence('Ctrl+O'), self, self._toggle_settings)

    # ── 툴바 ──────────────────────────────────────────────
    def _build_toolbar(self):
        tb = QFrame(self.centralWidget())
        tb.setObjectName('Surface')
        tb.setAutoFillBackground(True)
        h = QHBoxLayout(tb)
        h.setContentsMargins(14, 7, 12, 6)
        h.setSpacing(6)

        title = QLabel(APP_NAME, tb)
        title.setObjectName('Surface')
        f = QFont('Segoe UI', _FS + 3)
        f.setBold(True)
        title.setFont(f)
        h.addWidget(title)

        sep = QFrame(tb)
        sep.setFrameShape(QFrame.VLine)
        register_themed(sep, lambda: f'color: {C["border"]};')
        h.addWidget(sep)

        self.btn_scan = QPushButton('빠른 스캔', tb)
        self.btn_scan.setObjectName('Blue')
        self.btn_scan.clicked.connect(self._toggle_scan)
        h.addWidget(self.btn_scan)

        self.btn_db = QPushButton('DB 매칭', tb)
        self.btn_db.clicked.connect(self._load_local_titledb)
        h.addWidget(self.btn_db)
        self._update_db_btn_state()

        # 뷰 모드 토글 버튼 (리스트뷰 / 그리드뷰 순환)
        self._view_mode = db.get_setting('view_mode', 'list')  # list / grid
        _VIEW_LABELS = {'list': '리스트뷰', 'grid': '그리드뷰'}
        self.btn_view = QPushButton(_VIEW_LABELS.get(self._view_mode, '리스트뷰'), tb)
        self.btn_view.clicked.connect(self._toggle_view_mode)
        h.addWidget(self.btn_view)

        btn_settings = QPushButton('설정', tb)
        btn_settings.clicked.connect(self._toggle_settings)
        h.addWidget(btn_settings)

        h.addStretch(1)

        self.summary_lbl = QLabel('', tb)
        self.summary_lbl.setObjectName('Surface')
        register_themed(self.summary_lbl, lambda: f'color: {C["text_dim"]};')
        h.addWidget(self.summary_lbl)

        # 검색
        sf = QFrame(tb)
        sf.setObjectName('Surface')
        sl = QHBoxLayout(sf)
        sl.setContentsMargins(0, 0, 0, 0)
        sl.setSpacing(4)
        self._search_entry = QLineEdit(sf)
        self._search_entry.setPlaceholderText('검색...')
        self._search_entry.setFixedWidth(220)
        self._search_entry.textChanged.connect(self._on_search_changed)
        sl.addWidget(self._search_entry)
        h.addWidget(sf)

        # ESC → 검색 클리어
        sc = QShortcut(QKeySequence('Escape'), self._search_entry,
                       activated=lambda: self._search_entry.clear())
        sc.setContext(Qt.WidgetShortcut)

        # 중앙 위젯 layout 맨 위에 추가
        cv = self.centralWidget().layout()
        cv.insertWidget(0, tb)

    def _on_search_changed(self, text: str):
        self._search_text = text
        if hasattr(self, '_apply_filters'):
            self._apply_filters()

    # ── 뷰 모드 전환 ──────────────────────────────────────
    _VIEW_CYCLE  = ['list', 'grid']
    _VIEW_LABELS = {'list': '리스트뷰', 'grid': '그리드뷰'}

    def _toggle_view_mode(self):
        cycle = self._VIEW_CYCLE
        idx   = cycle.index(self._view_mode) if self._view_mode in cycle else 0
        self._view_mode = cycle[(idx + 1) % len(cycle)]
        db.set_setting('view_mode', self._view_mode)
        self.btn_view.setText(self._VIEW_LABELS[self._view_mode])
        # 뷰 스택 전환
        if hasattr(self, '_view_stack'):
            target = {
                'list': self.tree,
                'grid': self._grid_view,
            }.get(self._view_mode, self.tree)
            self._view_stack.setCurrentWidget(target)
        # 데이터 다시 채우기
        if hasattr(self, '_populate_table'):
            self._populate_table()

    # ── 우측: 라이브러리 테이블 영역 ───────────────────────
    def _build_right(self, parent):
        """
        구조:
            right
            ├─ dir_notebook  (얇은 탭바 역할만 — 탭 위젯은 빈 placeholder)
            └─ tbl           (실제 트리뷰; 항상 같은 부모/위치에 고정)

        탭 변경 시 위젯을 reparent하지 않고 _current_filter_dir만 바꾸어
        _apply_filters를 호출. (PySide6에서 동적 reparent는 layout과 충돌
        가능성이 있어 피함)
        """
        right = QWidget(parent)
        rv = QVBoxLayout(right)
        rv.setContentsMargins(0, 0, 0, 0)
        rv.setSpacing(0)

        # 폴더 탭 — 일반 탭에서는 탭바만 보이고 페이지 영역은
        # QTabWidget의 페이지 컨테이너가 빈 placeholder라도 큰 sizeHint를
        # 가져 트리뷰 위에 빈 공간이 생기는 것을 방지.
        from nsl_detail import _TabWidget as _DirTabWidget
        self.dir_notebook = _DirTabWidget(right)
        self.dir_notebook.currentChanged.connect(
            lambda _: self._on_dir_tab_changed())
        rv.addWidget(self.dir_notebook)
        # 일반 탭일 때 적용할 탭바만의 최대 높이는 탭바 sizeHint로
        # 결정 (실제 sizeHint는 위젯이 표시된 후에야 정확해지므로

        # 테이블 영역 — 탭 밖에 고정
        self.tbl = QFrame(right)
        tbl_l = QVBoxLayout(self.tbl)
        tbl_l.setContentsMargins(0, 0, 0, 0)
        tbl_l.setSpacing(0)

        # 트리뷰 + 모델
        cols   = ['name', 'title_id', 'version', 'latest_ver', 'size', 'status']
        heads  = {'name': '  게임 이름', 'title_id': '타이틀 ID',
                  'version': '보유 버전', 'latest_ver': '최신 버전',
                  'size': '크기',         'status': '상태'}
        widths = {'name': 400, 'title_id': 120, 'version': 80,
                  'latest_ver': 80, 'size': 60, 'status': 120}
        self._tree_cols = cols
        self._tree_heads = heads

        self.tree_model = QStandardItemModel(0, len(cols), self.tbl)
        self.tree_model.setHorizontalHeaderLabels([heads[c] for c in cols])

        self.tree = QTreeView(self.tbl)
        self.tree.setModel(self.tree_model)
        self.tree.setRootIsDecorated(False)
        self.tree.setUniformRowHeights(True)
        self.tree.setSelectionMode(QAbstractItemView.SingleSelection)
        self.tree.setSelectionBehavior(QAbstractItemView.SelectRows)
        self.tree.setAlternatingRowColors(True)
        self.tree.setSortingEnabled(True)  # 직접 _do_sort 호출
        self.tree.setEditTriggers(QAbstractItemView.NoEditTriggers)

        hdr = self.tree.header()
        hdr.setStretchLastSection(False)
        # 모든 컬럼은 Interactive(사용자가 끌어 조정), 단 '게임 이름'(첫 컬럼)만
        # Stretch 모드로 — 우측 패널 크기 변경 시 남은 공간을 자동으로 채움
        for i, c in enumerate(cols):
            if c == 'name':
                hdr.setSectionResizeMode(i, QHeaderView.Stretch)
            else:
                hdr.setSectionResizeMode(i, QHeaderView.Interactive)
                self.tree.setColumnWidth(i, widths[c])
        hdr.sectionClicked.connect(
            lambda i: self._do_sort(self._tree_cols[i]))

        self.tree.setContextMenuPolicy(Qt.CustomContextMenu)
        self.tree.customContextMenuRequested.connect(self._on_rclick)
        self.tree.doubleClicked.connect(self._run_file)
        self.tree.selectionModel().selectionChanged.connect(self._on_select)

        # Enter 키 → 실행
        sc_run = QShortcut(QKeySequence(Qt.Key_Return), self.tree,
                           activated=self._run_file)
        sc_run.setContext(Qt.WidgetShortcut)

        # ── 그리드뷰 (QListWidget, IconMode) ──────────────
        self._grid_view = QListWidget(self.tbl)
        self._grid_view.setViewMode(QListWidget.IconMode)
        self._grid_view.setResizeMode(QListWidget.Adjust)
        self._grid_view.setMovement(QListWidget.Static)
        self._grid_view.setIconSize(QSize(100, 100))
        self._grid_view.setGridSize(QSize(130, 145))
        self._grid_view.setWordWrap(True)
        self._grid_view.setEditTriggers(QAbstractItemView.NoEditTriggers)
        self._grid_view.setContextMenuPolicy(Qt.CustomContextMenu)
        self._grid_view.customContextMenuRequested.connect(self._on_rclick_grid)
        self._grid_view.itemDoubleClicked.connect(lambda _: self._run_file())
        self._grid_view.currentItemChanged.connect(self._on_select_grid)

        sc_run_grid = QShortcut(QKeySequence(Qt.Key_Return), self._grid_view,
                                activated=self._run_file)
        sc_run_grid.setContext(Qt.WidgetShortcut)

        # ── 세 뷰를 QStackedWidget으로 묶기 ───────────────
        self._view_stack = QStackedWidget(self.tbl)
        self._view_stack.addWidget(self.tree)
        self._view_stack.addWidget(self._grid_view)

        # 저장된 모드로 초기 뷰 선택
        _initial = {
            'list': self.tree,
            'grid': self._grid_view,
        }.get(getattr(self, '_view_mode', 'list'), self.tree)
        self._view_stack.setCurrentWidget(_initial)

        tbl_l.addWidget(self._view_stack, 1)

        # 페이지네이션 바
        pg_bar = QFrame(self.tbl)
        pg_bar.setObjectName('Panel')
        pg_bar.setAutoFillBackground(True)
        pgl = QHBoxLayout(pg_bar)
        pgl.setContentsMargins(6, 3, 6, 3)
        self._pg_bar = pg_bar
        self._btn_prev = QPushButton('◀', pg_bar)
        self._btn_prev.setFixedWidth(36)
        self._btn_prev.clicked.connect(self._page_prev)
        pgl.addWidget(self._btn_prev)
        self._page_lbl = QLabel('', pg_bar)
        self._page_lbl.setObjectName('Panel')
        register_themed(self._page_lbl, lambda: f'color: {C["text_dim"]};')
        pgl.addWidget(self._page_lbl)
        self._btn_next = QPushButton('▶', pg_bar)
        self._btn_next.setFixedWidth(36)
        self._btn_next.clicked.connect(self._page_next)
        pgl.addWidget(self._btn_next)
        pgl.addStretch(1)
        tbl_l.addWidget(pg_bar)

        # ── 로딩 화면 ─────────────────────────────────
        # 시작 직후 라이브러리 데이터를 비동기로 로드하는 동안
        # 환영 화면 대신 보여주는 페이지 (깜빡임 방지).
        self._loading = QFrame(right)
        ll = QVBoxLayout(self._loading)
        ll.setAlignment(Qt.AlignCenter)
        ll.setSpacing(16)

        _load_title = QLabel(f'🎮  {APP_NAME}', self._loading)
        _f = QFont('Segoe UI', _FS + 8)
        _f.setBold(True)
        _load_title.setFont(_f)
        _load_title.setAlignment(Qt.AlignCenter)
        register_themed(_load_title, lambda: (
            f'color: {C["text"]}; background-color: {C["bg"]};'
        ))
        ll.addWidget(_load_title)

        self._loading_msg = QLabel('라이브러리를 불러오는 중…', self._loading)
        _f2 = QFont('Segoe UI', _FS + 1)
        self._loading_msg.setFont(_f2)
        self._loading_msg.setAlignment(Qt.AlignCenter)
        register_themed(self._loading_msg, lambda: (
            f'color: {C["text_dim"]}; background-color: {C["bg"]};'
        ))
        ll.addWidget(self._loading_msg)

        # 무한(인디터미넷) 진행 바 — range(0,0)이면 좌우로 움직이는 애니메이션
        self._loading_bar = QProgressBar(self._loading)
        self._loading_bar.setRange(0, 0)
        self._loading_bar.setTextVisible(False)
        self._loading_bar.setFixedWidth(280)
        self._loading_bar.setFixedHeight(6)
        _bar_wrap = QHBoxLayout()
        _bar_wrap.setAlignment(Qt.AlignCenter)
        _bar_wrap.addWidget(self._loading_bar)
        ll.addLayout(_bar_wrap)

        # ── 환영 화면 ─────────────────────────────────
        self._welcome = QFrame(right)
        wl = QVBoxLayout(self._welcome)
        wl.setAlignment(Qt.AlignCenter)

        keys_path = self.base_dir / 'prod.keys'
        _keys_ok  = keys_path.is_file()

        # 색상은 키 이름으로 저장 → 테마 변경 시 새 C[key] 색으로 재칠
        welcome_lines = [
            (f'🎮  {APP_NAME}', _FS + 8, True,  'text'),
            ('시작하기',          12,    True,  'text'),
            ('  1.  같은 폴더에 prod.keys 배치',     _FS, False, 'text_dim'),
            ('  2.  타이틀 DB 다운로드 실행',            _FS, False, 'text_dim'),
            ('  3.  설정 > 스캔 폴더 추가',          _FS, False, 'text_dim'),
            ('  4.  🔍 전체 스캔 → 자동으로 게임 분석', _FS, False, 'text_dim'),
            ('필요한 환경',       12,    True,  'text'),
        ]
        if _keys_ok:
            welcome_lines.append(('  ✅  prod.keys       발견됨', _FS, False, 'ok'))
        else:
            welcome_lines.append((
                '  ❌  prod.keys       없음  →  프로그램과 같은 폴더에 prod.keys를 넣어주세요',
                _FS, False, 'err'))

        for text, size, bold, color_key in welcome_lines:
            lbl = QLabel(text, self._welcome)
            f = QFont('Segoe UI', size)
            f.setBold(bold)
            lbl.setFont(f)
            register_themed(lbl, lambda k=color_key: (
                f'color: {C[k]}; background-color: {C["bg"]};'
            ))
            wl.addWidget(lbl)

        # ── 환영 화면 / 테이블 — 같은 자리에 겹쳐서 한 번에 하나만 표시 ──
        # (이전에는 QVBoxLayout에 둘 다 추가했으나, 빈 공간이 발생하는
        # 케이스가 있어 QStackedLayout으로 변경)
        from PySide6.QtWidgets import QStackedLayout

        self._content_stack = QStackedLayout()
        self._content_stack.addWidget(self._loading)
        self._content_stack.addWidget(self._welcome)
        self._content_stack.addWidget(self.tbl)

        stack_container = QWidget(right)
        stack_container.setLayout(self._content_stack)
        rv.addWidget(stack_container, 1)

        # 초기 상태: 로딩 화면 표시 (데이터 로드가 끝나면
        # _on_startup_done에서 테이블/환영화면 중 하나로 전환됨)
        self._content_stack.setCurrentWidget(self._loading)

        # 컨텍스트 메뉴
        self._ctx = QMenu(self)
        self._ctx.addAction('🎮  실행',                self._run_file)
        self._ctx.addAction('📂  폴더에서 열기',   self._open_explorer)
        self._ctx.addAction('📄  경로 복사',           self._copy_path)
        self._ctx.addSeparator()
        self._ctx.addAction(
            '✏️  파일명 정규화', self._normalize_selected_filename)
        self._ctx.addSeparator()
        self._ctx.addAction('📸  스크린샷 다운로드',   self._download_selected_screenshots)
        self._ctx.addSeparator()
        self._ctx.addAction('🖼  아이콘 스캔',         self._extract_icon_selected)

        # 폴더 탭 매핑
        self._dir_tab_map = {}
        self._dir_tab_tooltips = {}

        return right

    # ── 폴더 탭 변경 ──────────────────────────────────
    # Qt의 QWIDGETSIZE_MAX 상수 — PySide6에서 직접 export되지 않아 정의

    def _on_dir_tab_changed(self):
        if not hasattr(self, 'dir_notebook') or self.dir_notebook.count() == 0:
            return
        idx = self.dir_notebook.currentIndex()
        if idx < 0:
            return
        cur_widget = self.dir_notebook.widget(idx)
        tab_bar_h = self.dir_notebook.tabBar().sizeHint().height()
        if tab_bar_h > 0:
            # 탭바 + 약간의 여유(테두리)
            self.dir_notebook.setMaximumHeight(tab_bar_h + 4)

        # stack_container 다시 표시
        if hasattr(self, '_content_stack'):
            stack_widget = self._content_stack.parentWidget()
            if stack_widget:
                stack_widget.show()
            # 라이브러리 상태에 따라 환영화면/테이블 토글
            # (초기 로딩 중에는 로딩 화면을 그대로 유지)
            if getattr(self, '_startup_loading', False):
                pass
            elif self._library:
                self._content_stack.setCurrentWidget(self.tbl)
            else:
                self._content_stack.setCurrentWidget(self._welcome)

        folder = self._dir_tab_map.get(id(cur_widget))
        prev_folder = getattr(self, '_current_filter_dir', -1)
        if prev_folder != folder:
            # 미보유 DLC 탭 → 다른 탭: 서브패널을 파일정보(0)로
            if (prev_folder == 'MISSING_DLCS'
                    and folder != 'MISSING_DLCS'
                    and hasattr(self, 'notebook')):
                try:
                    self.notebook.setCurrentIndex(0)  # 파일정보 탭
                except Exception:
                    pass
            # 다른 탭 → 미보유 DLC 탭: 서브패널을 DLC(3)로
            elif (folder == 'MISSING_DLCS'
                    and prev_folder != 'MISSING_DLCS'
                    and prev_folder != -1
                    and hasattr(self, 'notebook')):
                try:
                    self.notebook.setCurrentIndex(3)  # DLC 탭
                except Exception:
                    pass
            self._current_filter_dir = folder
            if hasattr(self, '_apply_filters'):
                self._apply_filters()

        key = folder if folder is not None else ''
        db.set_setting('last_tab_key', str(key))

    # ── 폴더 탭 갱신 ──────────────────────────────────
    _TAB_MAX = 14

    @staticmethod
    def _trim(text, maxlen=14):
        return text if len(text) <= maxlen else text[:maxlen - 1] + '…'

    def _refresh_dir_tabs(self):
        if not hasattr(self, 'dir_notebook'):
            return

        folders = db.get_scan_folders()

        # 탭 제거/추가 동안 currentChanged 시그널 일시 차단 (스톰 방지)
        self.dir_notebook.blockSignals(True)
        try:
            self._refresh_dir_tabs_impl(folders)
        finally:
            self.dir_notebook.blockSignals(False)

        # 차단 해제 후 한 번만 핸들러 호출
        self._on_dir_tab_changed()

    def _refresh_dir_tabs_impl(self, folders):
        current_idx = max(0, self.dir_notebook.currentIndex())

        # 일반 탭 제거
        while self.dir_notebook.count() > 0:
            w = self.dir_notebook.widget(0)
            self.dir_notebook.removeTab(0)
            w.deleteLater()

        self._dir_tab_map = {}
        self._dir_tab_tooltips = {}

        if not folders:
            # 폴더 없으면 탭 숨기기
            self.dir_notebook.hide()
            self._current_filter_dir = None
            if hasattr(self, '_apply_filters'):
                self._apply_filters()
            return

        self.dir_notebook.show()

        base_items = [x for x in self._library
                      if x.get('content_type') == 'base']
        total_base = len(base_items)

        try:
            exc_upd = db.get_exception_tids('update')
            exc_dlc = db.get_exception_tids('dlc')
        except Exception:
            exc_upd, exc_dlc = set(), set()

        missing_updates_count = sum(
            1 for x in base_items
            if (lat := x.get('db_latest_version')) is not None and lat > 0
            and max(x.get('version') or 0, x.get('update_version') or 0) < lat
            and (x.get('title_id') or '').upper() not in exc_upd
        )
        missing_dlcs_count = sum(
            1 for x in base_items
            if any(d.get('title_id', '').upper() not in exc_dlc
                   for d in x.get('missing_dlcs', []))
        )

        # 전체 탭
        tab_all = QWidget(self.dir_notebook)
        QVBoxLayout(tab_all).setContentsMargins(0, 0, 0, 0)
        self.dir_notebook.addTab(tab_all, f'  전체 ({total_base:,})  ')
        self._dir_tab_map[id(tab_all)] = None

        # 폴더 탭들
        for f in folders:
            tab_f = QWidget(self.dir_notebook)
            QVBoxLayout(tab_f).setContentsMargins(0, 0, 0, 0)
            name  = os.path.basename(os.path.normpath(f)) or str(f)
            f_abs = os.path.abspath(f)
            cnt   = sum(1 for x in base_items
                        if x.get('path') and
                        os.path.abspath(x['path']).startswith(f_abs + os.sep))
            short = self._trim(name, self._TAB_MAX)
            idx = self.dir_notebook.addTab(tab_f, f'  {short} ({cnt:,})  ')
            self._dir_tab_map[id(tab_f)] = f
            if short != name:
                self.dir_notebook.setTabToolTip(idx, f'{name}\n{f}')

        # 미보유 업데이트
        tab_mu = QWidget(self.dir_notebook)
        QVBoxLayout(tab_mu).setContentsMargins(0, 0, 0, 0)
        self.dir_notebook.addTab(
            tab_mu, f'  미보유 업데이트 ({missing_updates_count:,})  ')
        self._dir_tab_map[id(tab_mu)] = 'MISSING_UPDATES'

        # 미보유 DLC
        tab_md = QWidget(self.dir_notebook)
        QVBoxLayout(tab_md).setContentsMargins(0, 0, 0, 0)
        self.dir_notebook.addTab(
            tab_md, f'  미보유 DLC ({missing_dlcs_count:,})  ')
        self._dir_tab_map[id(tab_md)] = 'MISSING_DLCS'

        saved_tab_key = db.get_setting('last_tab_key', '__unset__')
        restored = False
        if saved_tab_key != '__unset__':
            for i in range(self.dir_notebook.count()):
                w = self.dir_notebook.widget(i)
                key = self._dir_tab_map.get(id(w))
                key_str = '' if key is None else str(key)
                if key_str == saved_tab_key:
                    self.dir_notebook.setCurrentIndex(i)
                    restored = True
                    break
        if not restored:
            self.dir_notebook.setCurrentIndex(
                min(current_idx, self.dir_notebook.count() - 1))
        # 해제 후 한 번만 호출하므로 여기서는 생략

    # ── 상태바 ────────────────────────────────────────────
    def _build_statusbar(self):
        sb = QStatusBar(self)
        self.setStatusBar(sb)

        self._status_lbl = QLabel('대기', sb)
        sb.addWidget(self._status_lbl)

        self._progress = QProgressBar(sb)
        self._progress.setFixedWidth(180)
        self._progress.setMaximumHeight(14)
        self._progress.setRange(0, 100)
        self._progress.setValue(0)
        sb.addPermanentWidget(self._progress)

        self._resource_lbl = QLabel(
            'RAM: 측정 대기... | CPU: 측정 대기...', sb)
        self._resource_lbl.setStyleSheet('font-family: Consolas, monospace;')
        sb.addPermanentWidget(self._resource_lbl)

    def _set_progress(self, cur: int, total: int):
        try:
            if total <= 0:
                self._progress.setRange(0, 0)  # busy mode
            else:
                self._progress.setRange(0, total)
                self._progress.setValue(cur)
        except Exception:
            pass

    def _update_resource_usage(self):
        try:
            import psutil
            if not hasattr(self, '_process_obj'):
                self._process_obj = psutil.Process(os.getpid())
                self._process_obj.cpu_percent(interval=None)
            mem_info = self._process_obj.memory_info()
            self._resource_lbl.setText(
                f'RAM: {mem_info.rss/1048576:.1f} MB  |  '
                f'CPU: {self._process_obj.cpu_percent(interval=None):.1f}%')
        except ImportError:
            self._resource_lbl.setText('psutil 미설치 (pip install psutil)')
        except Exception:
            pass

    # ── 선택·이벤트 ──────────────────────────────────────
    def _on_select(self, *_):
        sel = self.tree.selectionModel().selectedRows()
        if not sel:
            return
        rows = [idx.row() for idx in sel if idx.isValid()]
        if not rows:
            return
        self._select_pending_rows = rows
        self._select_timer.start()  # SingleShot: 이미 돌고 있으면 자동 재시작

    def _on_select_timeout(self):
        """선택 디바운스 타이머 만료 → 상세 패널 갱신."""
        rows = self._select_pending_rows
        if rows:
            self._select_pending_rows = []
            self._update_detail_panel_by_rows(rows)

    def _on_select_grid(self, current, _prev):
        """그리드뷰 선택 변경 → 상세 패널 갱신."""
        if current is None:
            return
        global_i = current.data(Qt.UserRole)
        if global_i is None:
            return
        self._update_detail_panel_by_rows([global_i])

    def _update_detail_panel_by_rows(self, rows: list):
        """정수 row 리스트를 받아 _filtered에서 항목을 가져와 상세 패널 갱신."""
        if not rows:
            return
        class _RowProxy:
            def __init__(self, r): self._r = r
            def row(self): return self._r
        proxies = [_RowProxy(r) for r in rows]
        self._update_detail_panel(proxies)

    def _on_rclick(self, pos: QPoint):
        idx = self.tree.indexAt(pos)
        if idx.isValid():
            self.tree.selectionModel().select(
                idx,
                QItemSelectionModel.ClearAndSelect |
                QItemSelectionModel.Rows)
            self._ctx.exec(self.tree.viewport().mapToGlobal(pos))

    def _on_rclick_grid(self, pos: QPoint):
        item = self._grid_view.itemAt(pos)
        if item:
            self._grid_view.setCurrentItem(item)
            self._ctx.exec(self._grid_view.viewport().mapToGlobal(pos))

    def _get_item(self):
        mode = getattr(self, '_view_mode', 'list')
        if mode == 'grid':
            cur = self._grid_view.currentItem()
            if cur is None:
                return None
            global_i = cur.data(Qt.UserRole)
        else:
            sel = self.tree.selectionModel().selectedRows()
            if not sel:
                return None
            global_i = sel[0].row()
        if global_i is None:
            return None
        return self._filtered[global_i] if global_i < len(self._filtered) else None

    def _run_file(self, *_):
        item = self._get_item()
        if not item:
            return
        path = item.get('path', '')
        if not os.path.exists(path):
            QMessageBox.critical(self, '오류', '파일을 찾을 수 없습니다.')
            return

        ext = os.path.splitext(path)[1].lower()

        # ── NSZ / XCZ 자동 압축 해제 처리 ────────────────────
        if ext in ('.nsz', '.xcz') and db.get_setting('decompress_auto', '0') == '1':
            if getattr(self, '_decompress_running', False):
                QMessageBox.information(
                    self, '진행 중',
                    '이미 압축 해제가 진행 중입니다.\n완료 후 다시 시도해 주세요.')
                return
            self._decompress_running = True
            self._start_decompress_anim(path)
            try:
                if hasattr(self, '_view_stack'):
                    self._view_stack.setEnabled(False)
            except Exception:
                pass
            try:
                path = self._decompress_file(path, ext.lstrip('.'))
            finally:
                self._stop_decompress_anim()
                try:
                    if hasattr(self, '_view_stack'):
                        self._view_stack.setEnabled(True)
                except Exception:
                    pass
                self._decompress_running = False
            if path is None:
                return
            ext = os.path.splitext(path)[1].lower()

        # ── 실행 프로그램 처리 ────────────────────────────────
        launcher_enabled = db.get_setting('launcher_enabled', '0') == '1'
        launcher_path    = db.get_setting('launcher_path', '').strip()

        if launcher_enabled and launcher_path:
            launcher_args = db.get_setting('launcher_args', '').strip()
            import shlex
            if '{path}' in launcher_args:
                args_expanded = launcher_args.replace('{path}', path)
                cmd = [launcher_path] + shlex.split(args_expanded)
            else:
                cmd = [launcher_path] + (shlex.split(launcher_args) if launcher_args else []) + [path]
            try:
                subprocess.Popen(cmd)
                self._set_status(f'실행: {os.path.basename(launcher_path)} ← {os.path.basename(path)}')
            except Exception as e:
                QMessageBox.critical(self, '실행 실패', f'프로그램을 실행할 수 없습니다:\n{e}')
            return

        # ── OS 기본 실행 ──────────────────────────────────────
        try:
            if platform.system() == 'Windows':
                os.startfile(os.path.normpath(path))
            elif platform.system() == 'Darwin':
                subprocess.run(['open', path])
            else:
                subprocess.run(['xdg-open', path])
        except Exception as e:
            QMessageBox.critical(self, '실패', f'파일 실행 실패: {e}')

    # ── 압축 해제 중 트리뷰 행 텍스트 애니메이션 ────────────────────
    def _start_decompress_anim(self, src_path: str):
        self._decompress_anim_orig_text = None
        self._decompress_anim_global_idx = None
        try:
            for i, it in enumerate(self._filtered):
                if it.get('path', '') == src_path:
                    self._decompress_anim_global_idx = i
                    break
        except Exception:
            pass
        if self._decompress_anim_global_idx is None:
            return
        model = getattr(self, 'tree_model', None)
        if model is None:
            return
        target_item = None
        for r in range(model.rowCount()):
            it0 = model.item(r, 0)
            if it0 and it0.data(Qt.UserRole + 1) == self._decompress_anim_global_idx:
                target_item = it0
                break
        if target_item is None:
            return
        self._decompress_anim_item = target_item
        self._decompress_anim_orig_text = target_item.text()
        self._decompress_anim_step = 0

        def _tick():
            item = getattr(self, '_decompress_anim_item', None)
            if item is None:
                return
            dots = '.' * (self._decompress_anim_step % 4)
            try:
                item.setText(f'  압축 해제중{dots}')
            except Exception:
                pass
            self._decompress_anim_step += 1

        old = getattr(self, '_decompress_anim_timer', None)
        if old is not None:
            try:
                old.stop(); old.deleteLater()
            except Exception:
                pass
        self._decompress_anim_timer = QTimer(self)
        self._decompress_anim_timer.timeout.connect(_tick)
        self._decompress_anim_timer.start(400)
        _tick()

    def _stop_decompress_anim(self):
        timer = getattr(self, '_decompress_anim_timer', None)
        if timer is not None:
            try:
                timer.stop(); timer.deleteLater()
            except Exception:
                pass
            self._decompress_anim_timer = None
        item = getattr(self, '_decompress_anim_item', None)
        orig = getattr(self, '_decompress_anim_orig_text', None)
        if item is not None and orig is not None:
            try:
                item.setText(orig)
            except Exception:
                pass
        self._decompress_anim_item = None
        self._decompress_anim_orig_text = None
        self._decompress_anim_global_idx = None


    def _open_explorer(self, *_):
        item = self._get_item()
        if not item:
            return
        path = item.get('path', '')
        try:
            if platform.system() == 'Windows':
                subprocess.run(['explorer', '/select,', os.path.normpath(path)])
            elif platform.system() == 'Darwin':
                subprocess.run(['open', '-R', path])
            else:
                subprocess.run(['xdg-open', os.path.dirname(path)])
        except Exception as e:
            QMessageBox.critical(self, '오류', str(e))

    def _copy_path(self):
        item = self._get_item()
        if item:
            QGuiApplication.clipboard().setText(item.get('path', ''))
            self._set_status('경로가 클립보드에 복사되었습니다')

    # ── 유틸 ──────────────────────────────────────────────
    def _focus_search(self):
        if hasattr(self, '_search_entry'):
            self._search_entry.setFocus()
            self._search_entry.selectAll()

    def _set_status(self, msg: str):
        self._status_lbl.setText(str(msg))

    def _hide_welcome(self):
        if hasattr(self, '_content_stack'):
            self._content_stack.setCurrentWidget(self.tbl)

    def _show_welcome(self):
        """라이브러리가 비어있을 때 환영화면 표시."""
        if hasattr(self, '_content_stack'):
            self._content_stack.setCurrentWidget(self._welcome)

    # ── 로그 ──────────────────────────────────────────────
    def _append_log(self, msg: str, level: str = ''):
        """워커 스레드에서 호출되는 로그 추가 — 시그널을 통해 메인 스레드로 전달."""
        self._log_buf.append((msg, level))

    def _flush_log_timer(self):
        if not self._log_buf:
            return
        if not hasattr(self, '_log_text') or self._log_text is None:
            return
        for msg, level in self._log_buf:
            color = {
                'err':  C['err'],  'warn': C['warn'],
                'ok':   C['ok'],   'info': C['accent2'],
            }.get(level, C['text'])
            self._log_text.appendHtml(
                f'<span style="color:{color}">{self._escape_html(msg)}</span>')
        self._log_buf.clear()
        # 로그가 추가되면 자동으로 표시
        if not self._log_visible:
            self._show_log()

    @staticmethod
    def _escape_html(s: str) -> str:
        return (str(s).replace('&', '&amp;').replace('<', '&lt;')
                       .replace('>', '&gt;').replace('"', '&quot;'))

    def _log(self, msg: str, level: str = ''):
        self.log_signal.emit(msg, level)

    def _show_log(self):
        self._log_frame.show()
        self._log_visible = True

    def _hide_log(self):
        self._log_frame.hide()
        self._log_visible = False

    # ── 비동기 갱신 핸들러 ───────────────────────────────
    def _on_async_refresh(self):
        if hasattr(self, '_refresh_library'):
            self._refresh_library()
    def _active_tasks(self) -> list:
        """
        현재 진행 중인 백그라운드 작업 목록 반환.
        반환값: [(라벨, 설명), ...]
        """
        tasks = []
        if getattr(self, '_scanning', False):
            tasks.append(('빠른 스캔',
                          '파일 시스템에서 게임을 검색하고 있습니다.'))
        if getattr(self, '_icon_extracting', False):
            tasks.append(('아이콘 자동 추출',
                          '스캔된 게임의 아이콘을 추출하고 있습니다.'))
        if getattr(self, '_icon_dl_running', False):
            tasks.append(('수동 아이콘 추출',
                          '선택한 게임의 아이콘을 추출하고 있습니다.'))
        if getattr(self, '_ss_downloading', False):
            tasks.append(('스크린샷 다운로드',
                          '게임 스크린샷을 다운로드하고 있습니다.'))
        if getattr(self, '_decompress_running', False):
            tasks.append(('압축 해제',
                          'NSZ/XCZ 파일을 NSP/XCI로 변환하고 있습니다.'))
        if getattr(self, '_titledb_running', False):
            tasks.append(('타이틀 DB 다운로드',
                          '타이틀 데이터베이스를 갱신하고 있습니다.'))
        if getattr(self, '_cleanup_running', False):
            tasks.append(('더미데이터 정리',
                          '사용하지 않는 캐시 파일을 정리하고 있습니다.'))
        return tasks

    def closeEvent(self, event):
        # 진행 중인 작업이 있으면 종료 확인
        active = self._active_tasks()
        if active:
            from PySide6.QtWidgets import QMessageBox
            lines = '\n'.join(f'  •  {label}  —  {desc}'
                              for label, desc in active)
            box = QMessageBox(self)
            box.setIcon(QMessageBox.Warning)
            box.setWindowTitle('진행 중인 작업이 있습니다')
            box.setText(
                f'다음 작업이 아직 진행 중입니다:\n\n{lines}\n\n'
                f'지금 종료하면 작업이 중단되어 데이터가 손실되거나\n'
                f'다음 실행 시 다시 처리해야 할 수 있습니다.')
            box.setInformativeText('정말 종료하시겠습니까?')
            btn_quit   = box.addButton('종료', QMessageBox.DestructiveRole)
            btn_cancel = box.addButton('계속 진행', QMessageBox.RejectRole)
            box.setDefaultButton(btn_cancel)
            box.exec()
            if box.clickedButton() is not btn_quit:
                event.ignore()
                return
            # 종료 선택 시 진행 중 워커들에게 중지 신호 (가능한 것만)
            try:
                self._scanning = False
                self._icon_extracting = False
            except Exception:
                pass

        # 스플리터 위치 저장
        try:
            if db.get_setting('sash_remember', '1') == '1':
                sizes = self.pw.sizes()
                if len(sizes) >= 2:
                    db.set_setting('sash_pos', str(sizes[1]))
        except Exception:
            pass
        super().closeEvent(event)


# ─────────────────────────────────────────────────────────────────
# 진입점
# ─────────────────────────────────────────────────────────────────
def cleanup_temp_zombies():
    import tempfile, shutil
    temp_dir = tempfile.gettempdir()
    for item in os.listdir(temp_dir):
        if item.startswith(('slm_scan_', 'slm_hactool_', 'slm_xci_')):
            try:
                shutil.rmtree(os.path.join(temp_dir, item), ignore_errors=True)
            except Exception:
                pass


def main():
    cleanup_temp_zombies()

    # PySide6는 시작 시 자동으로 DPI를 설정하므로 직접 호출하지 않음.
    # (직접 호출 시 "액세스 거부" 경고 발생)

    app = QApplication.instance() or QApplication(sys.argv)
    app.setApplicationName(APP_NAME)

    # 저장된 테마 로드 후 전역 스타일시트 적용
    db.init_db()
    saved_theme = db.get_setting('ui_theme', 'github_dark')
    set_theme(saved_theme)
    apply_stylesheet(app)

    win = SwitchLibraryManager()
    win.show()

    sys.exit(app.exec())


if __name__ == '__main__':
    main()
