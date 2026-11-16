# nsl_theme.py — 색상 팔레트 + 전역 QSS 스타일시트 (PySide6)
import os
import tempfile
import weakref as _weakref

# ── 폰트 크기 ──────────────────────────────────────────────────────
_FS = 10

# ── 테마 정의 ──────────────────────────────────────────────────────
# 각 테마는 동일한 키 집합을 가져야 하며, 'label'은 UI 표시용.
# 'input_border'는 입력 위젯 테두리 강조 색상.
THEMES = {
    'github_dark': {
        'label': 'GitHub Dark (기본)',
        'bg':       '#0d1117', 'surface':  '#161b22', 'surface2': '#21262d',
        'accent':   '#238636', 'accent2':  '#1f6feb', 'border':   '#30363d',
        'text':     '#e6edf3', 'text_dim': '#8b949e',
        'base':     '#58a6ff', 'update':   '#3fb950', 'dlc':      '#d29922',
        'unknown':  '#6e7681', 'ok':       '#3fb950', 'warn':     '#d29922',
        'err':      '#f85149', 'row_even': '#0d1117', 'row_odd':  '#161b22',
        'selected': '#1c4a7e', 'input_border': '#6e7681',
        'input_bg': '#010409', 'on_accent': '#ffffff',
    },
    'github_light': {
        'label': 'GitHub Light',
        'bg':       '#ffffff', 'surface':  '#f6f8fa', 'surface2': '#eaeef2',
        'accent':   '#1f883d', 'accent2':  '#0969da', 'border':   '#d0d7de',
        'text':     '#1f2328', 'text_dim': '#59636e',
        'base':     '#1f2328', 'update':   '#1f2328', 'dlc':      '#6e7781',
        'unknown':  '#6e7781', 'ok':       '#1a7f37', 'warn':     '#9a6700',
        'err':      '#cf222e', 'row_even': '#ffffff', 'row_odd':  '#f6f8fa',
        'selected': '#c8e1ff', 'input_border': '#8c959f',
        'input_bg': '#ffffff', 'on_accent': '#ffffff',
    },
    'catppuccin_mocha': {
        'label': 'Catppuccin Mocha',
        'bg':       '#1e1e2e', 'surface':  '#181825', 'surface2': '#313244',
        'accent':   '#a6e3a1', 'accent2':  '#89b4fa', 'border':   '#45475a',
        'text':     '#cdd6f4', 'text_dim': '#a6adc8',
        'base':     '#89b4fa', 'update':   '#a6e3a1', 'dlc':      '#f9e2af',
        'unknown':  '#6c7086', 'ok':       '#a6e3a1', 'warn':     '#f9e2af',
        'err':      '#f38ba8', 'row_even': '#1e1e2e', 'row_odd':  '#181825',
        'selected': '#45475a', 'input_border': '#7f849c',
        'input_bg': '#11111b', 'on_accent': '#11111b',
    },
    'dracula': {
        'label': 'Dracula',
        'bg':       '#282a36', 'surface':  '#21222c', 'surface2': '#343746',
        'accent':   '#50fa7b', 'accent2':  '#bd93f9', 'border':   '#44475a',
        'text':     '#f8f8f2', 'text_dim': '#a8a8b3',
        'base':     '#bd93f9', 'update':   '#8be9fd', 'dlc':      '#ffb86c',
        'unknown':  '#6272a4', 'ok':       '#50fa7b', 'warn':     '#ffb86c',
        'err':      '#ff5555', 'row_even': '#282a36', 'row_odd':  '#21222c',
        'selected': '#44475a', 'input_border': '#7c83bd',
        'input_bg': '#191a21', 'on_accent': '#282a36',
    },
    'one_dark': {
        'label': 'One Dark',
        'bg':       '#21252b', 'surface':  '#282c34', 'surface2': '#2c313c',
        'accent':   '#98c379', 'accent2':  '#61afef', 'border':   '#3e4451',
        'text':     '#dcdfe4', 'text_dim': '#7f848e',
        'base':     '#61afef', 'update':   '#98c379', 'dlc':      '#e5c07b',
        'unknown':  '#5c6370', 'ok':       '#98c379', 'warn':     '#e5c07b',
        'err':      '#e06c75', 'row_even': '#21252b', 'row_odd':  '#282c34',
        'selected': '#3e4451', 'input_border': '#5c6370',
        'input_bg': '#1b1f25', 'on_accent': '#21252b',
    },
    'tokyo_night': {
        'label': 'Tokyo Night',
        'bg':       '#1a1b26', 'surface':  '#16161e', 'surface2': '#24283b',
        'accent':   '#9ece6a', 'accent2':  '#7aa2f7', 'border':   '#3b4261',
        'text':     '#c0caf5', 'text_dim': '#9aa5ce',
        'base':     '#7aa2f7', 'update':   '#9ece6a', 'dlc':      '#e0af68',
        'unknown':  '#565f89', 'ok':       '#9ece6a', 'warn':     '#e0af68',
        'err':      '#f7768e', 'row_even': '#1a1b26', 'row_odd':  '#16161e',
        'selected': '#283457', 'input_border': '#737aa2',
        'input_bg': '#0f0f17', 'on_accent': '#1a1b26',
    },
    'nord': {
        'label': 'Nord',
        'bg':       '#2e3440', 'surface':  '#3b4252', 'surface2': '#434c5e',
        'accent':   '#a3be8c', 'accent2':  '#88c0d0', 'border':   '#4c566a',
        'text':     '#eceff4', 'text_dim': '#d8dee9',
        'base':     '#88c0d0', 'update':   '#a3be8c', 'dlc':      '#ebcb8b',
        'unknown':  '#616e88', 'ok':       '#a3be8c', 'warn':     '#ebcb8b',
        'err':      '#bf616a', 'row_even': '#2e3440', 'row_odd':  '#3b4252',
        'selected': '#5e81ac', 'input_border': '#7b88a1',
        'input_bg': '#232831', 'on_accent': '#2e3440',
    },
    'solarized_dark': {
        'label': 'Solarized Dark',
        'bg':       '#002b36', 'surface':  '#073642', 'surface2': '#0d3d4e',
        'accent':   '#859900', 'accent2':  '#268bd2', 'border':   '#586e75',
        'text':     '#eee8d5', 'text_dim': '#93a1a1',
        'base':     '#b58900', 'update':   '#268bd2', 'dlc':      '#859900',
        'unknown':  '#586e75', 'ok':       '#859900', 'warn':     '#b58900',
        'err':      '#dc322f', 'row_even': '#002b36', 'row_odd':  '#073642',
        'selected': '#1a5276', 'input_border': '#839496',
        'input_bg': '#001f27', 'on_accent': '#ffffff',
    },
    'gruvbox': {
        'label': 'Gruvbox Dark',
        'bg':       '#282828', 'surface':  '#32302f', 'surface2': '#3c3836',
        'accent':   '#b8bb26', 'accent2':  '#83a598', 'border':   '#504945',
        'text':     '#ebdbb2', 'text_dim': '#bdae93',
        'base':     '#fabd2f', 'update':   '#83a598', 'dlc':      '#b8bb26',
        'unknown':  '#928374', 'ok':       '#b8bb26', 'warn':     '#fabd2f',
        'err':      '#fb4934', 'row_even': '#282828', 'row_odd':  '#32302f',
        'selected': '#458588', 'input_border': '#928374',
        'input_bg': '#1d2021', 'on_accent': '#282828',
    },
    'ayu_dark': {
        'label': 'Ayu Dark',
        'bg':       '#0d1017', 'surface':  '#131721', 'surface2': '#1a2130',
        'accent':   '#91b362', 'accent2':  '#39bae6', 'border':   '#2d3640',
        'text':     '#bfbdb6', 'text_dim': '#8a8986',
        'base':     '#ffb454', 'update':   '#39bae6', 'dlc':      '#91b362',
        'unknown':  '#556068', 'ok':       '#91b362', 'warn':     '#ffb454',
        'err':      '#f07178', 'row_even': '#0d1017', 'row_odd':  '#131721',
        'selected': '#273949', 'input_border': '#565b66',
        'input_bg': '#05080d', 'on_accent': '#0d1017',
    },
    'monokai': {
        'label': 'Monokai',
        'bg':       '#272822', 'surface':  '#1e1f1c', 'surface2': '#3e3d32',
        'accent':   '#a6e22e', 'accent2':  '#66d9ef', 'border':   '#49483e',
        'text':     '#f8f8f2', 'text_dim': '#a59f85',
        'base':     '#f92672', 'update':   '#66d9ef', 'dlc':      '#a6e22e',
        'unknown':  '#75715e', 'ok':       '#a6e22e', 'warn':     '#fd971f',
        'err':      '#f92672', 'row_even': '#272822', 'row_odd':  '#1e1f1c',
        'selected': '#49483e', 'input_border': '#75715e',
        'input_bg': '#16170f', 'on_accent': '#272822',
    },
    'cyberpunk': {
        'label': 'Cyberpunk Neon',
        'bg':       '#0a0e27', 'surface':  '#151933', 'surface2': '#1f2547',
        'accent':   '#ff2cd4', 'accent2':  '#00f0ff', 'border':   '#2d3563',
        'text':     '#e0e3ff', 'text_dim': '#9aa1d4',
        'base':     '#ff2cd4', 'update':   '#00f0ff', 'dlc':      '#fde910',
        'unknown':  '#6b7299', 'ok':       '#0aff9d', 'warn':     '#fde910',
        'err':      '#ff3864', 'row_even': '#0a0e27', 'row_odd':  '#151933',
        'selected': '#2d2d6e', 'input_border': '#5a679e',
        'input_bg': '#04061a', 'on_accent': '#0a0e27',
    },
    'rose_pine': {
        'label': 'Rosé Pine',
        'bg':       '#191724', 'surface':  '#1f1d2e', 'surface2': '#26233a',
        'accent':   '#9ccfd8', 'accent2':  '#c4a7e7', 'border':   '#403d52',
        'text':     '#e0def4', 'text_dim': '#908caa',
        'base':     '#eb6f92', 'update':   '#9ccfd8', 'dlc':      '#f6c177',
        'unknown':  '#6e6a86', 'ok':       '#9ccfd8', 'warn':     '#f6c177',
        'err':      '#eb6f92', 'row_even': '#191724', 'row_odd':  '#1f1d2e',
        'selected': '#403d52', 'input_border': '#6e6a86',
        'input_bg': '#0f0e17', 'on_accent': '#191724',
    },
    'material_ocean': {
        'label': 'Material Ocean',
        'bg':       '#0f111a', 'surface':  '#1a1c25', 'surface2': '#232530',
        'accent':   '#c3e88d', 'accent2':  '#82aaff', 'border':   '#2e3142',
        'text':     '#c0c5ce', 'text_dim': '#8a8f99',
        'base':     '#82aaff', 'update':   '#c3e88d', 'dlc':      '#ffcb6b',
        'unknown':  '#464b5d', 'ok':       '#c3e88d', 'warn':     '#ffcb6b',
        'err':      '#f07178', 'row_even': '#0f111a', 'row_odd':  '#1a1c25',
        'selected': '#1f2233', 'input_border': '#545968',
        'input_bg': '#070810', 'on_accent': '#0f111a',
    },
}

# 현재 활성 테마
_CURRENT_THEME = 'github_dark'

# 체크박스/라디오 SVG 경로 캐시 (set_theme 호출 시 갱신)
_SVG_CHK   = ''
_SVG_RADIO = ''

# ── 색상 팔레트 (활성 테마 기반) ──────────────────────────────────
C = dict(THEMES[_CURRENT_THEME])

# ── 인라인 스타일 위젯 레지스트리 ─────────────────────────────────
# 위젯과 "스타일을 만드는 함수"를 매핑. 테마 변경 시 모든 함수를
# 다시 호출해 현재 C[...]로 위젯을 재칠한다.
# WeakSet/WeakKeyDictionary로 GC된 위젯을 자동 정리.
_themed_widgets = _weakref.WeakKeyDictionary()

def register_themed(widget, style_fn):
    """
    위젯에 스타일 함수를 등록하고 즉시 적용한다.

    사용:
        register_themed(label, lambda: f'color: {C["text_dim"]};')

    테마 변경 시 style_fn()이 다시 호출돼 현재 C[...] 값으로 재칠해진다.
    같은 위젯에 다시 등록하면 기존 함수가 교체된다.
    """
    if widget is None or style_fn is None:
        return
    _themed_widgets[widget] = style_fn
    try:
        widget.setStyleSheet(style_fn())
    except Exception:
        pass

def _refresh_themed_widgets():
    """등록된 모든 위젯의 스타일 함수를 다시 호출 (테마 변경 후 호출됨)."""
    # WeakKeyDictionary는 순회 중 변경되면 안 되므로 list로 스냅샷
    for w, fn in list(_themed_widgets.items()):
        try:
            w.setStyleSheet(fn())
        except RuntimeError:
            # C++ 객체가 이미 삭제된 경우 — WeakKeyDict가 정리해줌
            pass
        except Exception:
            pass

def get_theme_list():
    """[(id, label), …] 반환."""
    return [(k, v['label']) for k, v in THEMES.items()]

def set_theme(theme_id: str):
    """테마 변경 후 전역 C 팔레트 및 SVG 캐시 갱신."""
    global _CURRENT_THEME, _SVG_CHK, _SVG_RADIO
    if theme_id not in THEMES:
        return
    _CURRENT_THEME = theme_id
    C.clear()
    C.update(THEMES[theme_id])
    _SVG_CHK, _SVG_RADIO = _write_indicator_svgs(
        C.get('on_accent', '#ffffff'), C['accent2'])

def get_current_theme() -> str:
    return _CURRENT_THEME

def apply_theme(theme_id: str, app=None):
    """
    테마를 즉시 적용한다 (실시간 전환용).
    1) 전역 C 팔레트 갱신
    2) QApplication 전역 QSS 재적용
    3) 등록된 인라인 스타일 위젯 재칠
    """
    from PySide6.QtWidgets import QApplication
    set_theme(theme_id)
    target = app or QApplication.instance()
    if target is not None:
        target.setStyleSheet(_build_stylesheet())
    _refresh_themed_widgets()

def _write_indicator_svgs(on_accent: str, accent: str):
    """
    체크박스 ✓ 와 라디오 ● SVG를 임시 파일로 저장하고 경로를 반환한다.
    QSS image: url(path) 는 로컬 파일 경로만 안정적으로 지원하기 때문.
    Returns: (chk_path, radio_path) — 슬래시는 Qt가 인식하는 /로 통일
    """
    tmp = tempfile.gettempdir()

    chk_path   = os.path.join(tmp, f'nsl_chk_{on_accent[1:]}.svg')
    radio_path = os.path.join(tmp, f'nsl_radio_{accent[1:]}.svg')

    chk_svg = (
        f"<svg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 16 16'>"
        f"<path d='M3.5 8.5 L6.5 11.5 L12.5 5' "
        f"stroke='{on_accent}' stroke-width='2' stroke-linecap='round' "
        f"stroke-linejoin='round' fill='none'/></svg>"
    )
    radio_svg = (
        f"<svg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 16 16'>"
        f"<circle cx='8' cy='8' r='4' fill='{accent}'/></svg>"
    )

    for path, content in [(chk_path, chk_svg), (radio_path, radio_svg)]:
        try:
            with open(path, 'w', encoding='utf-8') as f:
                f.write(content)
        except Exception:
            pass

    # Qt QSS는 백슬래시를 인식 못하므로 슬래시로 통일
    return chk_path.replace('\\', '/'), radio_path.replace('\\', '/')

def _build_stylesheet() -> str:
    """현재 폰트 크기와 색상 팔레트로 전역 QSS 생성."""
    fs       = _FS
    bg, sf   = C['bg'], C['surface']
    sf2, bd  = C['surface2'], C['border']
    tx, dim  = C['text'], C['text_dim']
    acc      = C['accent2']
    sel      = C['selected']
    ibd      = C.get('input_border', C['accent2'])
    # 입력 위젯 배경 — 페이지 배경(surface)보다 한 단계 어두운/밝은 색
    # 으로 분리감을 줘서 빈 입력 영역이 항상 시각적으로 보이게 함
    ibg      = C.get('input_bg', C['bg'])
    # 강조 버튼(Accent/Blue)의 글자색 — 밝은 accent 위에 흰색이면
    # 가독성이 떨어지므로 테마별로 명시
    onacc    = C.get('on_accent', '#ffffff')
    _chk_svg, _radio_svg = _SVG_CHK, _SVG_RADIO

    return f"""
    /* ── 전역 ───────────────────────────────────────────── */
    QWidget {{
        background-color: {bg};
        color: {tx};
        font-family: "Segoe UI", "맑은 고딕", sans-serif;
        font-size: {fs}pt;
    }}
    QMainWindow, QDialog {{
        background-color: {bg};
    }}

    /* ── 표면 프레임 ──────────────────────────────────── */
    QFrame#Surface {{
        background-color: {sf};
    }}
    QFrame#Panel {{
        background-color: {sf2};
    }}
    QLabel#Surface {{
        background-color: {sf};
        color: {tx};
    }}
    QLabel#Panel {{
        background-color: {sf2};
        color: {tx};
    }}
    QLabel#Dim {{
        color: {dim};
    }}

    /* ── 버튼 ─────────────────────────────────────────── */
    /* 일반 버튼: hover 시 글자색은 그대로 유지 (가독성 일관성).
       배경만 한 단계 밝게/어둡게 + 테두리는 accent 색으로 강조. */
    QPushButton {{
        background-color: {sf2};
        color: {tx};
        border: 1px solid {ibd};
        border-radius: 3px;
        padding: 5px 10px;
    }}
    QPushButton:hover {{
        background-color: {sel};
        color: {tx};
        border-color: {acc};
    }}
    QPushButton:pressed {{
        background-color: {acc};
        color: {onacc};
        border-color: {acc};
    }}
    QPushButton:disabled {{
        background-color: {bg};
        color: {dim};
        border-color: {bd};
    }}
    /* Accent/Blue/Red: 강조 버튼은 원래 색 위에 on_accent 글자 (의도된 강조). */
    QPushButton#Accent {{
        background-color: {C['accent']};
        color: {onacc};
        border: 1px solid {C['accent']};
        padding: 6px 12px;
    }}
    QPushButton#Accent:hover {{
        background-color: {acc};
        border-color: {acc};
    }}
    QPushButton#Blue {{
        background-color: {acc};
        color: {onacc};
        border: 1px solid {acc};
        padding: 6px 12px;
    }}
    QPushButton#Blue:hover {{
        background-color: {ibd};
        border-color: {ibd};
    }}
    QPushButton#Red {{
        background-color: {C['err']};
        color: #ffffff;
        border: 1px solid {C['err']};
        padding: 6px 12px;
    }}
    QPushButton#Red:hover {{
        background-color: #d32f2f;
    }}

    /* ── 입력 (테두리 + 어두운 배경으로 영역 명확히) ──── */
    QLineEdit {{
        background-color: {ibg};
        color: {tx};
        border: 1px solid {ibd};
        border-radius: 4px;
        padding: 4px 7px;
        selection-background-color: {sel};
    }}
    QLineEdit:focus {{
        border: 1px solid {acc};
    }}
    QLineEdit:disabled {{
        background-color: {bg};
        color: {dim};
        border: 2px solid {bd};
    }}
    QTextEdit, QPlainTextEdit {{
        background-color: {ibg};
        color: {tx};
        border: 1px solid {ibd};
        border-radius: 4px;
        padding: 6px 8px;
        selection-background-color: {sel};
    }}
    /* QTextEdit/QPlainTextEdit는 내부 viewport(QWidget)가 별도 배경을
       가지므로 명시적으로 input_bg로 칠해야 빈 영역이 보임 */
    QTextEdit > QWidget, QPlainTextEdit > QWidget,
    QAbstractScrollArea > QWidget#qt_scrollarea_viewport {{
        background-color: {ibg};
    }}
    QTextEdit:focus, QPlainTextEdit:focus {{
        border: 1px solid {acc};
        padding: 6px 8px;
    }}
    QSpinBox, QDoubleSpinBox {{
        background-color: {ibg};
        color: {tx};
        border: 1px solid {ibd};
        border-radius: 4px;
        padding: 3px 6px;
    }}
    QSpinBox:focus, QDoubleSpinBox:focus {{
        border: 1px solid {acc};
    }}

    /* ── 트리 / 리스트 ────────────────────────────────── */
    QTreeView, QTableView {{
        background-color: {C['row_even']};
        alternate-background-color: {C['row_odd']};
        color: {tx};
        gridline-color: {bd};
        border: 1px solid {bd};
        selection-background-color: {sel};
        selection-color: {tx};
        outline: none;
    }}
    QTreeView::item, QTableView::item {{
        padding: 4px;
        min-height: {int(fs * 1.8)}px;
    }}
    QTreeView::item:selected, QTableView::item:selected {{
        background-color: {sel};
        color: {tx};
    }}
    QTreeView::item:hover:!selected, QTableView::item:hover:!selected {{
        background-color: {sf};
        color: {tx};
    }}
    QHeaderView::section {{
        background-color: {sf2};
        color: {tx};
        border: none;
        border-right: 1px solid {bd};
        border-bottom: 1px solid {bd};
        padding: 6px;
        font-weight: bold;
    }}
    QHeaderView::section:hover {{
        background-color: {acc};
        color: {onacc};
    }}

    QListWidget {{
        background-color: {sf2};
        color: {tx};
        border: 1px solid {ibd};
        border-radius: 3px;
        outline: 0;
    }}
    QListWidget::item {{
        padding: 6px;
    }}
    QListWidget::item:selected {{
        background-color: {sel};
        color: {tx};
    }}

    /* ── 탭 ───────────────────────────────────────────── */
    QTabWidget::pane {{
        background-color: {bg};
        border: 1px solid {bd};
        top: -1px;
    }}
    QTabBar::scroller {{
        width: 0px;
    }}
    QTabBar::tab {{
        background-color: {sf2};
        color: {tx};
        padding: 8px 14px;
        border-top: 1px solid {bd};
        border-bottom: 1px solid {bd};
        border-left: none;
        border-right: none;
        margin-right: 1px;
    }}
    QTabBar::tab:first-child {{
        border-left: 1px solid {bd};
    }}
    QTabBar::tab:last-child {{
        border-right: 1px solid {bd};
        margin-right: 0px;
    }}
    QTabBar::tab:selected {{
        background-color: {bg};
        color: {tx};
        border-bottom: 1px solid {bg};
        margin-bottom: -1px;
    }}
    QTabBar::tab:hover:!selected {{
        background-color: {sf};
    }}

    /* ── 스크롤바 ─────────────────────────────────────── */
    QScrollBar:vertical {{
        background-color: {bg};
        width: 12px;
        border: none;
    }}
    QScrollBar::handle:vertical {{
        background-color: {sf2};
        border-radius: 4px;
        min-height: 20px;
        margin: 2px;
    }}
    QScrollBar::handle:vertical:hover {{
        background-color: {bd};
    }}
    QScrollBar::add-line:vertical, QScrollBar::sub-line:vertical {{
        height: 0px;
    }}
    QScrollBar:horizontal {{
        background-color: {bg};
        height: 12px;
        border: none;
    }}
    QScrollBar::handle:horizontal {{
        background-color: {sf2};
        border-radius: 4px;
        min-width: 20px;
        margin: 2px;
    }}
    QScrollBar::handle:horizontal:hover {{
        background-color: {bd};
    }}
    QScrollBar::add-line:horizontal, QScrollBar::sub-line:horizontal {{
        width: 0px;
    }}

    /* ── 체크박스 / 라디오 (Windows 표준 스타일) ───────────────
       체크박스: 사각형 + 체크 마크 ✓ (SVG)
       라디오:   원   + 중앙 채워진 점 (SVG)
       두 컨트롤이 시각적으로 명확히 구분되도록 형태 차이를 강조. */
    QCheckBox, QRadioButton {{
        background-color: transparent;
        color: {tx};
        spacing: 8px;
        padding: 2px 0px;
    }}
    QCheckBox::indicator, QRadioButton::indicator {{
        width: 16px;
        height: 16px;
    }}

    /* — 체크박스: 사각형 — */
    QCheckBox::indicator {{
        border-radius: 3px;
    }}
    QCheckBox::indicator:unchecked {{
        background-color: {ibg};
        border: 1px solid {ibd};
    }}
    QCheckBox::indicator:unchecked:hover {{
        border: 1px solid {acc};
        background-color: {ibg};
    }}
    QCheckBox::indicator:checked {{
        background-color: {acc};
        border: 1px solid {acc};
        image: url({_chk_svg});
    }}
    QCheckBox::indicator:checked:hover {{
        background-color: {acc};
        border: 1px solid {acc};
    }}
    QCheckBox::indicator:indeterminate {{
        background-color: {acc};
        border: 1px solid {acc};
        image: url({_chk_svg});
    }}
    QCheckBox::indicator:disabled {{
        background-color: {bg};
        border: 1px solid {bd};
    }}

    /* — 라디오: 원 + 중앙 점 — */
    QRadioButton::indicator {{
        border-radius: 8px;
    }}
    QRadioButton::indicator:unchecked {{
        background-color: {ibg};
        border: 1px solid {ibd};
    }}
    QRadioButton::indicator:unchecked:hover {{
        background-color: {ibg};
        border: 1px solid {acc};
    }}
    QRadioButton::indicator:checked {{
        background-color: {ibg};
        border: 1px solid {acc};
        image: url({_radio_svg});
    }}
    QRadioButton::indicator:checked:hover {{
        background-color: {ibg};
        border: 1px solid {acc};
    }}
    QRadioButton::indicator:disabled {{
        background-color: {bg};
        border: 1px solid {bd};
    }}

    /* ── 진행 바 ──────────────────────────────────────── */
    QProgressBar {{
        background-color: {sf};
        border: 1px solid {bd};
        border-radius: 3px;
        text-align: center;
        color: {tx};
        max-height: 16px;
    }}
    QProgressBar::chunk {{
        background-color: {acc};
        border-radius: 2px;
    }}

    /* ── 메뉴 ─────────────────────────────────────────── */
    QMenuBar {{
        background-color: {bg};
        color: {tx};
        border-bottom: 1px solid {bd};
    }}
    QMenuBar::item {{
        background-color: transparent;
        padding: 6px 10px;
    }}
    QMenuBar::item:selected {{
        background-color: {acc};
        color: {onacc};
    }}
    QMenu {{
        background-color: {sf};
        color: {tx};
        border: 1px solid {bd};
    }}
    QMenu::item {{
        padding: 6px 22px;
    }}
    QMenu::item:selected {{
        background-color: {acc};
        color: {onacc};
    }}
    QMenu::separator {{
        height: 1px;
        background-color: {bd};
        margin: 4px 6px;
    }}

    /* ── 스플리터 ────────────────────────────────────── */
    QSplitter::handle {{
        background-color: {bd};
    }}
    QSplitter::handle:horizontal {{
        width: 3px;
    }}
    QSplitter::handle:vertical {{
        height: 3px;
    }}

    /* ── 상태바 ──────────────────────────────────────── */
    QStatusBar {{
        background-color: {sf2};
        color: {tx};
        border-top: 1px solid {bd};
    }}
    QStatusBar QLabel {{
        background-color: transparent;
        padding: 0px 10px;
    }}
    QStatusBar::item {{
        border: none;
    }}

    /* ── 콤보박스 ────────────────────────────────────── */
    QComboBox {{
        background-color: {ibg};
        color: {tx};
        border: 1px solid {ibd};
        border-radius: 4px;
        padding: 4px 7px;
    }}
    QComboBox:focus {{
        border: 1px solid {acc};
    }}
    QComboBox::drop-down {{
        border: none;
        width: 18px;
    }}
    QComboBox QAbstractItemView {{
        background-color: {sf};
        color: {tx};
        border: 1px solid {bd};
        selection-background-color: {sel};
    }}

    /* ── 툴팁 ────────────────────────────────────────── */
    QToolTip {{
        background-color: {sf2};
        color: {tx};
        border: 1px solid {ibd};
        padding: 4px 8px;
    }}
    """

def apply_stylesheet(app):
    """QApplication 인스턴스에 전역 QSS 적용."""
    app.setStyleSheet(_build_stylesheet())

# 모듈 로드 시 초기 테마 SVG 생성
set_theme(_CURRENT_THEME)


# ── 위젯 스타일 유틸 ──────────────────────────────────────────────
def restyle(widget, new_object_name: str):
    """objectName 변경 후 QSS를 재적용해 색상을 즉시 갱신."""
    widget.setObjectName(new_object_name)
    s = widget.style()
    s.unpolish(widget)
    s.polish(widget)
    widget.update()
