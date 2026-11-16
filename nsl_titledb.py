# nsl_titledb.py — 타이틀 DB 다운로드/파싱/병합 (PySide6)
# titles.json (메타) + versions.json (최신 버전) + KR.ko.json (한국어 이름)을
# 받아 단일 엔트리 목록으로 병합한다.
import os
import json
import urllib.request
from datetime import datetime
from concurrent.futures import ThreadPoolExecutor, as_completed

from nsl_common import get_base_path, get_ssl_context

# ── 외부 소스 URL ──────────────────────────────────────────────────────
TINFOIL_TITLES_URL = 'https://tinfoil.media/repo/db/titles.json'
VERSIONS_URL       = 'https://raw.githubusercontent.com/blawar/titledb/master/versions.json'
KR_URL             = 'https://raw.githubusercontent.com/blawar/titledb/master/KR.ko.json'

TITLEDB_DIR = os.path.join(get_base_path(), 'titledb')

_HEADERS = {
    'User-Agent':      'Mozilla/5.0 (Windows NT 10.0; Win64; x64) '
                       'AppleWebKit/537.36 (KHTML, like Gecko) '
                       'Chrome/124.0.0.0 Safari/537.36',
    'Accept':          'application/json, text/plain, */*',
    'Accept-Language': 'ko-KR,ko;q=0.9,en-US;q=0.8',
    'Referer':         'https://tinfoil.media/',
}

_SSL_CTX = get_ssl_context()


# ── 작은 유틸 ──────────────────────────────────────────────────────────
def _clean(v, d=''):
    if v is None:
        return d
    return (' '.join(str(v).split())
            .replace('\n', ' ').replace('\r', ' ').replace('\t', ' '))


def _int(v, d=0):
    try:
        return int(v)
    except (TypeError, ValueError):
        return d


def _fmt(n):
    for u in ('B', 'KB', 'MB', 'GB'):
        if n < 1024:
            return f'{n:.0f} {u}'
        n /= 1024
    return f'{n:.1f} GB'


# ── 네트워크 · 파일 IO ────────────────────────────────────────────────
def _fetch(url, progress_cb=None, save_as=None):
    req = urllib.request.Request(url, headers=_HEADERS)
    with urllib.request.urlopen(req, context=_SSL_CTX, timeout=60) as resp:
        total = int(resp.headers.get('Content-Length', 0))
        chunks, n = [], 0
        while True:
            chunk = resp.read(65536)
            if not chunk:
                break
            chunks.append(chunk)
            n += len(chunk)
            if progress_cb:
                progress_cb(n, total)
    raw = b''.join(chunks)
    if save_as:
        try:
            _save_json(save_as, raw)
        except Exception:
            pass
    return json.loads(raw.decode('utf-8'))


def _save_json(filename, raw):
    os.makedirs(TITLEDB_DIR, exist_ok=True)
    path = os.path.join(TITLEDB_DIR, filename)
    tmp  = path + '.tmp'
    with open(tmp, 'wb') as f:
        f.write(raw)
    os.replace(tmp, path)


def _load_local(filename):
    path = os.path.join(TITLEDB_DIR, filename)
    if not os.path.exists(path):
        return None
    with open(path, 'rb') as f:
        return json.loads(f.read().decode('utf-8'))


# ── 파싱 ──────────────────────────────────────────────────────────────
def _parse_versions(data):
    """versions.json → {title_id: 최신_version}."""
    result = {}
    for tid, vd in data.items():
        if not isinstance(vd, dict) or not vd:
            continue
        try:
            result[tid.upper()] = max(int(k) for k in vd.keys())
        except (ValueError, TypeError):
            pass
    return result


def _parse_languages(langs_raw):
    if isinstance(langs_raw, list):
        return ', '.join(dict.fromkeys(
            str(x).upper() for x in langs_raw if x))
    return str(langs_raw).upper() if langs_raw else ''


def _make_entry(tid, info):
    screenshots_list = info.get('screenshots') or []
    if isinstance(screenshots_list, list):
        screenshots_list = [s for s in screenshots_list
                            if isinstance(s, str) and s.startswith('http')]
    else:
        screenshots_list = []
    return dict(
        title_id     = tid.upper(),
        name         = _clean(info.get('name') or info.get('title')
                              or info.get('Name')),
        version      = _int(info.get('version') or info.get('Version')),
        release_date = _clean(info.get('releaseDate')
                              or info.get('release_date')),
        num_players  = _clean(info.get('numberOfPlayers')
                              or info.get('numPlayers')),
        rating       = _clean(info.get('rating')
                              or info.get('contentRating')),
        size         = _int(info.get('size')),
        region       = _clean(info.get('region')),
        icon_url     = _clean(info.get('iconUrl') or info.get('icon_url')),
        screenshots  = json.dumps(screenshots_list),
        description  = str(info.get('description') or '').strip(),
        languages    = _parse_languages(info.get('languages')),
    )


def _parse_titles(data):
    if isinstance(data, dict) and 'titledb' in data:
        data = data['titledb']

    out = {}
    if isinstance(data, dict):
        for tid, info in data.items():
            if not isinstance(info, dict):
                continue
            entry = _make_entry(tid, info)
            if entry['name']:
                out[entry['title_id']] = entry

    elif isinstance(data, list):
        for info in data:
            if not isinstance(info, dict):
                continue
            tid  = _clean(info.get('id') or info.get('titleId')
                          or info.get('title_id')).upper()
            name = _clean(info.get('name') or info.get('title'))
            if not tid or not name:
                continue
            out[tid] = dict(
                title_id=tid, name=name,
                version=_int(info.get('version')),
                release_date=_clean(info.get('releaseDate')),
                num_players=_clean(info.get('numberOfPlayers')),
                rating=_clean(info.get('rating')),
                size=_int(info.get('size')),
                region=_clean(info.get('region')),
                icon_url='', screenshots='[]',
                description=str(info.get('description') or '').strip(),
                languages=_parse_languages(info.get('languages')),
            )
    return out


def _parse_kr(data):
    """KR.ko.json → {title_id: {name, description}}."""
    if not isinstance(data, dict):
        return {}
    result = {}
    for _, info in data.items():
        if not isinstance(info, dict):
            continue
        tid = _clean(info.get('id') or info.get('titleId')).upper()
        if tid:
            result[tid] = {
                'name':        _clean(info.get('name') or info.get('title')),
                'description': str(info.get('description') or '').strip(),
            }
    return result


def _merge(title_map, ver_map, kr_map):
    """KR 이름/설명 오버라이드 → 최신 버전 채움 → versions에만 있는 항목 추가."""
    for tid, kr_info in kr_map.items():
        if tid in title_map:
            if kr_info['name']:
                title_map[tid]['name'] = kr_info['name']
            if kr_info['description']:
                title_map[tid]['description'] = kr_info['description']
        else:
            title_map[tid] = dict(
                title_id=tid, name=kr_info['name'], version=0,
                release_date='', num_players='', rating='', size=0,
                region='', icon_url='', screenshots='[]',
                description=kr_info['description'], languages='',
            )
    for tid, entry in title_map.items():
        if tid in ver_map:
            entry['version'] = ver_map[tid]
    for tid, latest in ver_map.items():
        if tid not in title_map:
            title_map[tid] = dict(
                title_id=tid, name='', version=latest,
                release_date='', num_players='', rating='', size=0,
                region='', icon_url='', screenshots='[]',
                description='', languages='',
            )


# ── 진입점 ─────────────────────────────────────────────────────────────
def download_full_titledb(progress_cb=None):
    """3개 소스를 모두 받아 병합 후 엔트리 목록 반환."""
    import nsl_database as _db
    source = _db.get_setting('titledb_source', 'custom')

    def cb(pct, msg):
        if progress_cb:
            progress_cb(pct, 100, msg)

    if source == 'custom':
        titles_url = ('https://github.com/UzuCore/NSL/releases/'
                      'download/titledb/titles.json')
        source_name = '기본 서버'
    else:
        titles_url = TINFOIL_TITLES_URL
        source_name = '대체 서버'

    cb(0, f'{source_name} 메타 데이터 다운로드 중…')
    titles_data = _fetch(
        titles_url,
        lambda dl, tot: cb(
            int(dl / tot * 50) if tot else 5,
            f'메타 데이터  {_fmt(dl)} / {_fmt(tot) if tot else "?"}'),
        save_as='titles.json')
    title_map = _parse_titles(titles_data)
    cb(50, f'메타 데이터 완료 ({len(title_map):,}개) — 버전 정보 다운로드 중…')

    ver_data = _fetch(
        VERSIONS_URL,
        lambda dl, tot: cb(
            50 + (int(dl / tot * 35) if tot else 5),
            f'버전 정보  {_fmt(dl)} / {_fmt(tot) if tot else "?"}'),
        save_as='versions.json')
    ver_map = _parse_versions(ver_data)
    cb(85,
       f'버전 정보 완료 ({len(ver_map):,}개) — 한국어 데이터 다운로드 중…')

    kr_map = {}
    try:
        kr_data = _fetch(
            KR_URL,
            lambda dl, tot: cb(
                85 + (int(dl / tot * 10) if tot else 2),
                f'한국어 데이터  {_fmt(dl)} / {_fmt(tot) if tot else "?"}'),
            save_as='KR.ko.json')
        kr_map = _parse_kr(kr_data)
        cb(95, f'한국어 데이터 완료 ({len(kr_map):,}개)')
    except Exception as e:
        cb(95, f'한국어 데이터 생략 ({e})')

    cb(95, '병합 중…')
    _merge(title_map, ver_map, kr_map)
    entries = list(title_map.values())
    cb(100, f'완료 ({len(entries):,}개)')
    return entries


def load_local_titledb(progress_cb=None):
    """이미 받아둔 3개 파일을 병렬 로드 후 병합."""
    def cb(pct, msg):
        if progress_cb:
            progress_cb(pct, 100, msg)

    titles_ref = [None]
    ver_ref    = [None]
    kr_ref     = [None]
    errors     = []

    def load_titles():
        d = _load_local('titles.json')
        if not d:
            raise FileNotFoundError(
                '메타 데이터 파일이 없습니다. '
                '먼저 "타이틀 DB 다운로드"를 실행하세요.')
        titles_ref[0] = d

    def load_versions():
        ver_ref[0] = _load_local('versions.json')

    def load_kr():
        kr_ref[0] = _load_local('KR.ko.json')

    cb(5, '로컬 파일 병렬 로드 중…')

    with ThreadPoolExecutor(max_workers=3) as pool:
        futures = {
            pool.submit(load_titles):   'titles',
            pool.submit(load_versions): 'versions',
            pool.submit(load_kr):       'kr',
        }
        done = 0
        for future in as_completed(futures):
            done += 1
            name = futures[future]
            try:
                future.result()
                cb(10 + done * 25, f'{name} 로드 완료')
            except Exception as e:
                if name == 'titles':
                    raise
                errors.append(f'{name}: {e}')

    cb(85, f'파싱 중… (경고: {", ".join(errors) if errors else "없음"})')

    title_map = _parse_titles(titles_ref[0])
    ver_map   = _parse_versions(ver_ref[0]) if ver_ref[0] else {}
    kr_map    = _parse_kr(kr_ref[0])        if kr_ref[0]  else {}

    cb(95, '병합 중…')
    _merge(title_map, ver_map, kr_map)
    entries = list(title_map.values())
    cb(100, f'완료 ({len(entries):,}개)')
    return entries


def get_local_file_info():
    """타이틀 DB 파일 3종의 존재 여부와 메타 정보."""
    info = {}
    for name in ('titles.json', 'versions.json', 'KR.ko.json'):
        path = os.path.join(TITLEDB_DIR, name)
        if os.path.exists(path):
            info[name] = {
                'exists':   True,
                'size':     os.path.getsize(path),
                'modified': datetime.fromtimestamp(
                    os.path.getmtime(path)).strftime('%Y-%m-%d %H:%M'),
            }
        else:
            info[name] = {'exists': False}
    return info
