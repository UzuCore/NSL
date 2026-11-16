# nsl_database.py — SQLite DB · 설정 JSON · 게임 뷰 조립 (PySide6)
import json
import os
import sqlite3
from pathlib import Path

from nsl_common import get_base_path

# ── 경로 / 상수 ────────────────────────────────────────────────────────
_BASE_DIR   = Path(get_base_path())
DB_PATH     = _BASE_DIR / 'library.db'
CONFIG_PATH = _BASE_DIR / 'config.json'

_DB_CHUNK = 5000   # 타이틀 DB 일괄 삽입 단위 (메모리 피크 억제용)


# ── 연결 ──────────────────────────────────────────────────────────────
def get_connection():
    DB_PATH.parent.mkdir(parents=True, exist_ok=True)
    conn = sqlite3.connect(str(DB_PATH))
    conn.row_factory = sqlite3.Row
    conn.execute('PRAGMA foreign_keys = ON')
    conn.execute('PRAGMA journal_mode = WAL')
    conn.execute('PRAGMA cache_size = -32768')
    conn.execute('PRAGMA temp_store = MEMORY')
    return conn


def init_db():
    conn = get_connection()
    conn.executescript('''
        CREATE TABLE IF NOT EXISTS game_files (
            id          INTEGER PRIMARY KEY AUTOINCREMENT,
            path        TEXT UNIQUE NOT NULL,
            filename    TEXT NOT NULL,
            size        INTEGER DEFAULT 0,
            mtime       REAL,
            fast_hash   TEXT,
            file_type   TEXT,
            last_seen   TEXT,
            scan_folder TEXT,
            nacp_ko     TEXT DEFAULT '',
            nacp_en     TEXT DEFAULT '',
            nacp_ja     TEXT DEFAULT ''
        );

        CREATE TABLE IF NOT EXISTS file_contents (
            id           INTEGER PRIMARY KEY AUTOINCREMENT,
            path         TEXT NOT NULL,
            title_id     TEXT,
            version      INTEGER DEFAULT 0,
            content_type TEXT DEFAULT 'unknown',
            source       TEXT DEFAULT 'filename',
            UNIQUE(path, title_id, content_type)
        );
        CREATE INDEX IF NOT EXISTS idx_fc_path     ON file_contents(path);
        CREATE INDEX IF NOT EXISTS idx_fc_title_id ON file_contents(title_id);

        CREATE TABLE IF NOT EXISTS title_db (
            title_id     TEXT PRIMARY KEY,
            name         TEXT,
            version      INTEGER DEFAULT 0,
            release_date TEXT,
            num_players  TEXT,
            rating       TEXT,
            size         INTEGER DEFAULT 0,
            region       TEXT,
            icon_url     TEXT,
            screenshots  TEXT DEFAULT '[]',
            description  TEXT,
            languages    TEXT
        );
        CREATE INDEX IF NOT EXISTS idx_titledb_base
            ON title_db(UPPER(SUBSTR(title_id,1,13))||'000');
    ''')
    # 구버전 DB 마이그레이션 — nacp 컬럼 추가가 누락된 경우 보강.
    for col in ('nacp_ko', 'nacp_en', 'nacp_ja'):
        try:
            conn.execute(
                f"ALTER TABLE game_files ADD COLUMN {col} TEXT DEFAULT ''")
        except Exception:
            pass
    conn.commit()
    conn.close()


# ── 이름 표시 모드 ────────────────────────────────────────────────────
NAME_MODES = {
    'db':       '타이틀DB',
    'ko_en_db': '한국어 > 영어 > DB',
    'ko_ja_db': '한국어 > 일본어 > DB',
}


def _is_cjk(text):
    """문자열에 CJK(한·중·일) 문자가 포함되어 있는지."""
    if not text:
        return False
    for ch in text:
        cp = ord(ch)
        if (0x3040 <= cp <= 0x30FF      # 히라가나·카타카나
                or 0x4E00 <= cp <= 0x9FFF   # CJK 통합 한자
                or 0xF900 <= cp <= 0xFAFF   # CJK 호환 한자
                or 0xFF65 <= cp <= 0xFF9F): # 반각 카타카나
            return True
    return False


def resolve_title_name(d, td_row, mode='db'):
    """
    파일 메타와 타이틀 DB 행으로부터 표시 이름을 결정.

    ko_en_db: 한글 > 영문 > DB 이름(CJK 제외) > 일본어 > 파일명
    ko_ja_db: 한글 > 일본어 > DB 이름 > 파일명
    db      : DB 이름 > 파일명
    """
    nacp_ko = d.get('nacp_ko') or ''
    nacp_en = d.get('nacp_en') or ''
    nacp_ja = d.get('nacp_ja') or ''
    db_name = td_row.get('name') or ''

    def _fallback():
        fn = d.get('filename') or ''
        return os.path.splitext(fn)[0] if fn else 'Unknown'

    if mode == 'ko_en_db':
        if nacp_ko:
            return nacp_ko
        if nacp_en:
            return nacp_en
        # DB 이름이 한자/일본어면 nacp_ja와 큰 차이가 없으므로 후순위.
        if db_name and not _is_cjk(db_name):
            return db_name
        if nacp_ja:
            return nacp_ja
        return db_name or _fallback()

    if mode == 'ko_ja_db':
        if nacp_ko:
            return nacp_ko
        if nacp_ja:
            return nacp_ja
        return db_name or _fallback()

    return db_name or _fallback()


# ── 설정 JSON ─────────────────────────────────────────────────────────
_config_cache = None


def _load_config():
    global _config_cache
    if _config_cache is not None:
        return _config_cache
    if not CONFIG_PATH.exists():
        _config_cache = {"settings": {}, "scan_folders": []}
        return _config_cache
    try:
        with open(CONFIG_PATH, 'r', encoding='utf-8') as f:
            _config_cache = json.load(f)
    except Exception:
        _config_cache = {"settings": {}, "scan_folders": []}
    return _config_cache


def _save_config(config):
    global _config_cache
    _config_cache = config
    with open(CONFIG_PATH, 'w', encoding='utf-8') as f:
        json.dump(config, f, indent=4, ensure_ascii=False)


def get_setting(key, default=None):
    return _load_config().get("settings", {}).get(key, default)


def set_setting(key, value):
    config = _load_config()
    config.setdefault("settings", {})[key] = str(value)
    _save_config(config)


def add_scan_folder(path):
    config  = _load_config()
    folders = config.setdefault("scan_folders", [])
    if path not in folders:
        folders.append(path)
        _save_config(config)


def remove_scan_folder(path):
    config  = _load_config()
    folders = config.get("scan_folders", [])
    if path in folders:
        folders.remove(path)
        _save_config(config)


def get_scan_folders():
    return _load_config().get("scan_folders", [])


# ── 빠른 스캔 지원 ────────────────────────────────────────────────────
def get_known_files():
    """DB에 저장된 {path: {mtime, fast_hash}} 반환."""
    conn = get_connection()
    rows = conn.execute(
        'SELECT path, mtime, fast_hash FROM game_files').fetchall()
    conn.close()
    return {r['path']: {'mtime': r['mtime'], 'fast_hash': r['fast_hash']}
            for r in rows}


def get_file_contents(path):
    """지정 파일에 실제로 들어 있는 콘텐츠 정보만 반환."""
    conn = get_connection()
    rows = conn.execute(
        'SELECT title_id, version, content_type, source '
        'FROM file_contents WHERE path=?',
        (path,)).fetchall()
    conn.close()
    return [dict(row) for row in rows]


def rename_file_path(old_path, new_path):
    """디스크 파일명 변경 후 DB의 경로 참조를 원자적으로 갱신."""
    conn = get_connection()
    try:
        conn.execute('BEGIN')
        exists = conn.execute(
            'SELECT 1 FROM game_files WHERE path=?', (new_path,)).fetchone()
        if exists:
            raise ValueError('새 파일명이 이미 라이브러리에 존재합니다.')
        cur = conn.execute(
            'UPDATE game_files SET path=?, filename=? WHERE path=?',
            (new_path, os.path.basename(new_path), old_path))
        if cur.rowcount != 1:
            raise ValueError('기존 파일의 라이브러리 기록을 찾지 못했습니다.')
        conn.execute(
            'UPDATE file_contents SET path=? WHERE path=?',
            (new_path, old_path))
        conn.commit()
    except Exception:
        conn.rollback()
        raise
    finally:
        conn.close()


def upsert_files_batch(file_list):
    if not file_list:
        return
    conn = get_connection()
    try:
        conn.execute('BEGIN')
        for fi in file_list:
            conn.execute('''
                INSERT INTO game_files
                    (path, filename, size, mtime, fast_hash, file_type,
                     last_seen, scan_folder,
                     nacp_ko, nacp_en, nacp_ja)
                VALUES
                    (:path, :filename, :size, :mtime, :fast_hash, :file_type,
                     :last_seen, :scan_folder,
                     :nacp_ko, :nacp_en, :nacp_ja)
                ON CONFLICT(path) DO UPDATE SET
                    filename  = excluded.filename,
                    size      = excluded.size,
                    mtime     = excluded.mtime,
                    fast_hash = excluded.fast_hash,
                    file_type = excluded.file_type,
                    last_seen = excluded.last_seen,
                    nacp_ko   = excluded.nacp_ko,
                    nacp_en   = excluded.nacp_en,
                    nacp_ja   = excluded.nacp_ja
            ''', fi)
            conn.execute('DELETE FROM file_contents WHERE path=?',
                         (fi['path'],))
            contents = fi.get('contents') or []
            if not contents:
                conn.execute(
                    'INSERT OR IGNORE INTO file_contents '
                    '(path, title_id, version, content_type, source) '
                    'VALUES (?,?,?,?,?)',
                    (fi['path'], None, 0, 'unknown', 'none'))
            else:
                conn.executemany(
                    'INSERT OR IGNORE INTO file_contents '
                    '(path, title_id, version, content_type, source) '
                    'VALUES (?,?,?,?,?)',
                    [(fi['path'], c.get('title_id'), c.get('version', 0),
                      c.get('content_type', 'unknown'),
                      c.get('source', 'filename'))
                     for c in contents])
        conn.commit()
    except Exception:
        conn.rollback()
        raise
    finally:
        conn.close()


def delete_missing_files(existing_paths):
    existing  = set(existing_paths)
    conn      = get_connection()
    all_paths = {r['path'] for r in
                 conn.execute('SELECT path FROM game_files').fetchall()}
    dead = list(all_paths - existing)
    if dead:
        ph = ','.join('?' * len(dead))
        conn.execute(
            f'DELETE FROM file_contents WHERE path IN ({ph})', dead)
        conn.execute(
            f'DELETE FROM game_files    WHERE path IN ({ph})', dead)
        conn.commit()
    conn.close()
    return len(dead)


def clear_all_files():
    conn = get_connection()
    conn.execute('DELETE FROM file_contents')
    conn.execute('DELETE FROM game_files')
    conn.commit()
    conn.close()


def delete_db_file():
    """
    DB 파일과 WAL/SHM 부속 파일을 완전히 삭제.
    호출 후에는 init_db()로 빈 DB를 다시 만들어야 한다.

    반환: (삭제된 파일 수, 에러 메시지 리스트)
    """
    deleted = 0
    errors  = []
    targets = [
        DB_PATH,
        DB_PATH.with_name(DB_PATH.name + '-wal'),
        DB_PATH.with_name(DB_PATH.name + '-shm'),
        DB_PATH.with_name(DB_PATH.name + '-journal'),
    ]
    for p in targets:
        try:
            if p.exists():
                p.unlink()
                deleted += 1
        except Exception as e:
            errors.append(f'{p.name}: {e}')
    return deleted, errors


def _base_tid(tid):
    """베이스 TID(끝 3자리 000) 도출. 16진수 변환 실패 시 문자열 폴백."""
    try:
        return f"{int(tid, 16) & 0xFFFFFFFFFFFFE000:016X}"
    except ValueError:
        return (tid[:13] + '000').upper() if len(tid) >= 13 else ''


# ── 게임 뷰 조립 ──────────────────────────────────────────────────────
def _build_file_map(rows):
    """1) path를 키로 파일 맵 구성, 각 파일의 콘텐츠 리스트 누적."""
    file_map = {}
    for row in rows:
        p = row['path']
        if p not in file_map:
            file_map[p] = {
                'id': row['id'], 'path': p, 'filename': row['filename'],
                'size': row['size'], 'mtime': row['mtime'],
                'fast_hash': row['fast_hash'], 'file_type': row['file_type'],
                'last_seen': row['last_seen'],
                'scan_folder': row['scan_folder'],
                'nacp_ko': row['nacp_ko'] or '',
                'nacp_en': row['nacp_en'] or '',
                'nacp_ja': row['nacp_ja'] or '',
                '_contents': [],
            }
        if row['content_type']:
            file_map[p]['_contents'].append({
                'title_id':     row['title_id'],
                'version':      row['version'] or 0,
                'content_type': row['content_type'],
                'source':       row['source'],
            })
    return file_map


def _build_group_map(file_map):
    """2) 베이스 키별 집계 — 보유 DLC·최신 버전·업데이트 여부."""
    g_map = {}
    for d in file_map.values():
        for c in d['_contents']:
            tid = c.get('title_id')
            if not tid or len(tid) < 13:
                continue
            bk = _base_tid(tid)
            if not bk:
                continue
            gs = g_map.setdefault(
                bk, {'max_version': 0, 'has_update': 0, 'total_dlc': set()})
            ct  = c['content_type']
            ver = c['version'] or 0
            if ct == 'update':
                gs['has_update'] = 1
                if ver > gs['max_version']:
                    gs['max_version'] = ver
            elif ct == 'dlc':
                gs['total_dlc'].add(tid)
            elif ct == 'base' and ver > gs['max_version']:
                gs['max_version'] = ver
    return g_map


def _build_db_dlcs(tdb):
    """3) 타이틀 DB에서 DLC 카탈로그 추출 (끝 4비트 0x1000~0x1FFF = DLC)."""
    db_dlcs = {}
    for tid, row in tdb.items():
        if len(tid) != 16:
            continue
        try:
            val = int(tid, 16)
            if (val & 0x1FFF) >= 0x1000:
                bk = f"{val & 0xFFFFFFFFFFFFE000:016X}"
                db_dlcs.setdefault(bk, []).append({
                    'title_id': tid,
                    'name': row.get('name') or 'Unknown DLC',
                })
        except ValueError:
            pass
    return db_dlcs


def _resolve_entry(d, tdb, g_map, db_dlcs, name_mode):
    """4+5) 단일 파일 엔트리를 완성 — 콘텐츠 선택·그룹 보강·DLC 매칭·이름 결정."""
    contents = d.pop('_contents')

    # 콘텐츠 필드 결정
    if contents:
        base = next((c for c in contents
                     if c['content_type'] == 'base'), None)
        upd  = next((c for c in contents
                     if c['content_type'] == 'update'), None)
        best = base or upd or contents[0]
        d['title_id']       = best['title_id'] or ''
        d['content_type']   = best['content_type'] or 'unknown'
        d['version']        = best['version'] or 0
        d['source']         = best['source'] or 'none'
        d['has_base']       = 1 if any(c['content_type'] == 'base'
                                       for c in contents) else 0
        d['has_update']     = 1 if any(c['content_type'] == 'update'
                                       for c in contents) else 0
        d['dlc_count']      = sum(1 for c in contents
                                  if c['content_type'] == 'dlc')
        d['update_version'] = upd['version'] if upd else 0
    else:
        d['title_id']     = ''
        d['version']      = d['has_base'] = d['has_update'] = 0
        d['dlc_count']    = d['update_version'] = 0
        d['content_type'] = 'unknown'
        d['source']       = 'none'

    tid      = d.get('title_id') or ''
    base_key = _base_tid(tid) if len(tid) >= 13 else ''
    td_row   = tdb.get(base_key, {})

    # 그룹 집계로 보강 (다중 파일이 같은 베이스를 공유할 때)
    owned_dlc_tids = set()
    if base_key in g_map:
        gs      = g_map[base_key]
        max_ver = int(gs['max_version'] or 0)
        cur_ver = int(d.get('version') or 0)
        upd_ver = int(d.get('update_version') or 0)
        if max_ver > cur_ver or max_ver > upd_ver:
            d['update_version'] = max_ver
        if gs.get('has_update'):
            d['has_update'] = 1
        owned_dlc_tids = gs.get('total_dlc', set())
        if len(owned_dlc_tids) > (d.get('dlc_count') or 0):
            d['dlc_count'] = len(owned_dlc_tids)

    # DLC 매칭 (보유/누락)
    all_db_dlcs = db_dlcs.get(base_key, [])
    db_dlc_tids = {dlc['title_id'] for dlc in all_db_dlcs}
    missing_dlcs = sorted(
        [dlc for dlc in all_db_dlcs
         if dlc['title_id'] not in owned_dlc_tids],
        key=lambda x: x['title_id'])
    owned_dlcs = sorted(
        [dlc for dlc in all_db_dlcs
         if dlc['title_id'] in owned_dlc_tids],
        key=lambda x: x['title_id'])
    for dlc_tid in owned_dlc_tids:
        if dlc_tid not in db_dlc_tids:
            owned_dlcs.append(
                {'title_id': dlc_tid, 'name': 'Unknown DLC (DB 미등록)'})
    owned_dlcs.sort(key=lambda x: x['title_id'])

    d['title_name']        = resolve_title_name(d, td_row, name_mode)
    d['db_latest_version'] = td_row.get('version')
    d['db_dlc_count']      = len(all_db_dlcs)
    d['missing_dlcs']      = missing_dlcs
    d['owned_dlcs']        = owned_dlcs
    d['screenshots']       = td_row.get('screenshots', '[]')
    d['description']       = td_row.get('description', '')
    d['languages']         = td_row.get('languages', '')
    d['region']            = td_row.get('region', '')
    return d


def get_files_view():
    """
    파일·콘텐츠·타이틀 DB를 JOIN하여 UI 표시용 단일 목록으로 조립.
    각 항목에 보유/누락 DLC, 최신 버전, 표시 이름까지 모두 채워서 반환.
    """
    conn = get_connection()
    rows = conn.execute('''
        SELECT
            gf.id, gf.path, gf.filename, gf.size, gf.mtime,
            gf.fast_hash, gf.file_type, gf.last_seen, gf.scan_folder,
            gf.nacp_ko, gf.nacp_en, gf.nacp_ja,
            fc.title_id, fc.version, fc.content_type, fc.source
        FROM game_files gf
        LEFT JOIN file_contents fc ON gf.path = fc.path
        ORDER BY gf.path
    ''').fetchall()
    tdb_rows = conn.execute('''
        SELECT title_id, name, version, screenshots, description,
               languages, region, icon_url
        FROM title_db
    ''').fetchall()
    tdb = {r['title_id']: dict(r) for r in tdb_rows}
    conn.close()

    file_map  = _build_file_map(rows)
    g_map     = _build_group_map(file_map)
    db_dlcs   = _build_db_dlcs(tdb)
    name_mode = get_setting('name_mode', 'db')

    result = [_resolve_entry(d, tdb, g_map, db_dlcs, name_mode)
              for d in file_map.values()]
    result.sort(
        key=lambda x: (x.get('title_name') or x.get('filename') or '').lower())
    return result


def get_stats():
    conn  = get_connection()
    files = conn.execute(
        'SELECT COUNT(*) AS n, SUM(size) AS s FROM game_files').fetchone()
    ct = conn.execute('''
        SELECT
            COUNT(*) AS total,
            SUM(CASE WHEN content_type="base"    THEN 1 ELSE 0 END) AS base_n,
            SUM(CASE WHEN content_type="update"  THEN 1 ELSE 0 END) AS update_n,
            SUM(CASE WHEN content_type="dlc"     THEN 1 ELSE 0 END) AS dlc_n,
            SUM(CASE WHEN content_type="unknown" THEN 1 ELSE 0 END) AS unknown_n
        FROM file_contents
    ''').fetchone()
    tdb = conn.execute(
        'SELECT COUNT(*) AS n FROM title_db').fetchone()
    conn.close()
    return {
        'file_count':     files['n'] or 0,
        'total_size':     files['s'] or 0,
        'total':          ct['total'] or 0,
        'base_count':     ct['base_n'] or 0,
        'update_count':   ct['update_n'] or 0,
        'dlc_count':      ct['dlc_n'] or 0,
        'unknown_count':  ct['unknown_n'] or 0,
        'title_db_count': tdb['n'] or 0,
    }


# ── 타이틀 DB ─────────────────────────────────────────────────────────
def load_title_db_entries(entries):
    """청크 단위 삽입으로 메모리 피크 억제."""
    conn = get_connection()
    conn.execute('DELETE FROM title_db')
    conn.commit()

    SQL = (
        'INSERT OR REPLACE INTO title_db '
        '(title_id, name, version, release_date, num_players, rating, '
        ' size, region, icon_url, screenshots, description, languages) '
        'VALUES (:title_id,:name,:version,:release_date,:num_players,:rating,'
        ':size,:region,:icon_url,:screenshots,:description,:languages)'
    )
    for e in entries:
        e.setdefault('icon_url', '')
        e.setdefault('description', '')
        e.setdefault('languages', '')
        if not e.get('screenshots'):
            e['screenshots'] = '[]'
        elif isinstance(e['screenshots'], list):
            e['screenshots'] = json.dumps(
                [u for u in e['screenshots'] if isinstance(u, str)])

    for i in range(0, len(entries), _DB_CHUNK):
        conn.execute('BEGIN')
        conn.executemany(SQL, entries[i:i + _DB_CHUNK])
        conn.commit()

    conn.close()


def clear_title_db():
    conn = get_connection()
    conn.execute('DELETE FROM title_db')
    conn.commit()
    conn.close()


# ── 제외 ID ────────────────────────────────────────────────────────────
def get_exception_tids(kind):
    """kind: 'update' | 'dlc' — 쉼표 구분 문자열 → set."""
    raw = get_setting(f'exception_{kind}', '')
    return {t.strip().upper() for t in raw.split(',') if t.strip()}


def set_exception_tids(kind, tids):
    """set/list → 쉼표 구분 문자열로 저장."""
    set_setting(f'exception_{kind}',
                ','.join(sorted(t.upper() for t in tids if t.strip())))
