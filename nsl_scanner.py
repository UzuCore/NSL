# nsl_scanner.py — 파일 헤더 파싱 · 캐시 · 디렉터리 스캔 엔진
# NSP/NSZ/XCI/XCZ 파일에서 타이틀 ID, 버전, 콘텐츠 타입, NACP 이름을 추출한다.
import hashlib
import json
import os
import re
import shutil
import struct
import subprocess
import sys
import tempfile
import threading
import xml.etree.ElementTree as ET
from concurrent.futures import ThreadPoolExecutor, as_completed
from datetime import datetime
from pathlib import Path

from nsl_common import get_base_path

# ── JSON 라이브러리 (orjson 우선 — 5~10배 빠른 직렬화) ────────────────
try:
    import orjson as _json_lib

    def _json_dumps(obj):
        return _json_lib.dumps(
            obj, option=_json_lib.OPT_NON_STR_KEYS).decode('utf-8')

    def _json_loads(s):
        return _json_lib.loads(s)

    def _json_dumps_bytes(obj):
        return _json_lib.dumps(obj, option=_json_lib.OPT_NON_STR_KEYS)
except ImportError:
    def _json_dumps(obj):
        return json.dumps(obj, ensure_ascii=False, separators=(',', ':'))

    def _json_loads(s):
        return json.loads(s)

    def _json_dumps_bytes(obj):
        return json.dumps(obj, ensure_ascii=False,
                          separators=(',', ':')).encode('utf-8')


# ── 경로 / 상수 ────────────────────────────────────────────────────────
_BASE_DIR      = get_base_path()
CACHE_BASE_DIR = os.path.join(_BASE_DIR, 'caches')
CACHE_FILE     = 'cache.json'
CACHE_VERSION  = 5
_FAST_CHUNK    = 64 * 1024

# CNMT content type 매핑 (헤더 1바이트 → 의미)
_CNMT_TYPES = {
    0x80: 'base', 0x81: 'update', 0x82: 'dlc',
    0x83: 'delta', 0x84: 'dlc',
}

# 파일명 · 메타데이터 파싱용 정규식
_RE_STD_TID    = re.compile(r'\[([0-9A-Fa-f]{16})\]')
_RE_VERSION    = re.compile(r'\[v(\d+)\]', re.IGNORECASE)
_RE_NSTOOL_TID = re.compile(
    r'(?:TitleId|ProgramId|ProgId|Title\s*Id|ApplicationId|'
    r'Application\s*Id|PatchId|Patch\s*Id|AddOnContentId|'
    r'AddOn\s*Content\s*Id|AddonContentId|Addon\s*Content\s*Id)'
    r'\s*[:\s]+(?:0[xX])?(0[15][0-9A-Fa-f]{14})',
    re.IGNORECASE)
_RE_NSTOOL_CMD_FALLBACK = re.compile(
    r'(0[15][0-9A-Fa-f]{14})', re.IGNORECASE)
_RE_CNMT_TITLE = re.compile(
    r'(?:TitleId|Title\s*Id|ApplicationId|Application\s*Id|'
    r'PatchId|Patch\s*Id|AddOnContentId|AddOn\s*Content\s*Id|'
    r'AddonContentId|Addon\s*Content\s*Id)'
    r'\s*:\s*0?x?(0[15][0-9A-Fa-f]{14})',
    re.IGNORECASE)
_RE_CNMT_VER_V = re.compile(r'\(v(\d+)\)')
_RE_NSTOOL_VER = re.compile(r'Version\s*[:\s]+\s*(\d+)', re.IGNORECASE)

# Windows에서 외부 도구 실행 시 콘솔 창 숨김
_NT_STARTUPINFO   = None
_NT_CREATIONFLAGS = 0
if os.name == 'nt':
    _NT_STARTUPINFO = subprocess.STARTUPINFO()
    _NT_STARTUPINFO.dwFlags    |= subprocess.STARTF_USESHOWWINDOW
    _NT_STARTUPINFO.wShowWindow = 0
    _NT_CREATIONFLAGS = subprocess.CREATE_NO_WINDOW

_NSTOOL_TIMEOUT = 60

# ── hactool 키 파일 경쟁 조건 방지 ─────────────────────────────────────
# 스캔 워커 다수가 동시에 ~/.switch/prod.keys 를 백업/스왑/복원하면
# 사용자 원본 파일이 유실될 수 있어 hactool 호출을 직렬화한다.
_HACTOOL_LOCK = threading.Lock()


# ── 공용 헬퍼 ─────────────────────────────────────────────────────────
def get_content_type(title_id):
    """Title ID의 애플리케이션 하위 13비트로 콘텐츠 타입 추정."""
    if not title_id or len(title_id) < 4:
        return 'unknown'
    try:
        suffix = int(title_id, 16) & 0x1FFF
        if suffix == 0x000:
            return 'base'
        if suffix == 0x800:
            return 'update'
        return 'dlc'
    except ValueError:
        return 'unknown'


def version_to_str(version):
    if version is None:
        return '-'
    v = int(version)
    if v == 0:
        return 'v1.0.0'
    maj   = (v >> 26) & 0x3F
    minor = (v >> 20) & 0x3F
    patch = (v >> 16) & 0xF
    if maj or minor or patch:
        return f'v{maj}.{minor}.{patch}'
    if v >= 65536:
        return f'v0.0.0+{v}'
    return f'v? ({v})'


def format_size(n):
    if n is None:
        return '-'
    n = float(n)
    for u in ('B', 'KB', 'MB', 'GB', 'TB'):
        if n < 1024:
            return f'{n:.1f} {u}'
        n /= 1024
    return f'{n:.1f} PB'


def compute_fast_hash(path):
    """파일 일부(헤더 + 꼬리)와 크기로 빠른 해시 산출."""
    h    = hashlib.md5()
    size = os.path.getsize(path)
    h.update(str(size).encode())
    with open(path, 'rb') as f:
        h.update(f.read(_FAST_CHUNK))
        if size > _FAST_CHUNK * 2:
            f.seek(-_FAST_CHUNK, 2)
            h.update(f.read())
    return h.hexdigest()


# ── 로그 헬퍼 (반복 패턴 통합) ────────────────────────────────────────
def _log_extraction(item, log_cb, prefix=''):
    """추출 결과 한 줄 로그. scanner 내부 6중 반복 패턴을 통합."""
    if not log_cb:
        return
    tid    = item.get('title_id') or 'N/A'
    ct     = item.get('content_type', 'unknown')
    ver    = item.get('version', 0)
    source = item.get('source', '')
    head   = f'{prefix} ' if prefix else ''
    log_cb(f'   ✅ {head}{ct:8s} {tid}  v{ver}  [{source}]')


def _log_truncated(log_cb, exc):
    """잘린 파일 로그. scanner 내부 4중 반복 패턴을 통합."""
    if log_cb:
        log_cb(f'   ❌ 손상/잘린 파일: {exc}')


# ── PFS0 / HFS0 컨테이너 파싱 ─────────────────────────────────────────
_PFS0_OFFSETS_NSP = [0, 0x200, 0x400, 0x1000, 0x4000, 0x8000]
_PFS0_OFFSETS_XCI = [0x8000, 0x4000, 0, 0x200, 0x400, 0x1000]
_MAX_CONTAINER_FILES = 16384
_MAX_STRING_TABLE_SIZE = 64 * 1024 * 1024
_MAX_XML_METADATA_SIZE = 8 * 1024 * 1024
_MAX_TICKET_SIZE = 1024 * 1024
_MAX_META_NCA_SIZE = 32 * 1024 * 1024


class _TruncatedContainer(Exception):
    """컨테이너 엔트리가 파일 크기를 초과 — 다운로드 중 잘린 파일."""
    pass


def _try_pfs0_at(f, base_offset, file_size=None):
    """
    PFS0/HFS0 헤더를 base_offset에서 파싱.
    file_size가 주어지면 데이터 영역이 파일을 넘어가는지 검증해
    잘린 파일을 _TruncatedContainer로 신호한다.
    """
    if base_offset < 0:
        return None
    if file_size is not None and base_offset + 16 > file_size:
        return None
    f.seek(base_offset)
    magic = f.read(4)
    if magic not in (b'PFS0', b'HFS0'):
        return None

    file_count     = struct.unpack('<I', f.read(4))[0]
    str_table_size = struct.unpack('<I', f.read(4))[0]
    f.read(4)

    if not (0 < file_count <= _MAX_CONTAINER_FILES):
        return None
    if str_table_size > _MAX_STRING_TABLE_SIZE:
        return None

    # 엔트리 전체를 한 번에 읽고 unpack_from으로 배치 처리
    entry_size = 0x40 if magic == b'HFS0' else 0x18
    hdr_end = base_offset + 16 + file_count * entry_size + str_table_size
    if file_size is not None and hdr_end > file_size:
        raise _TruncatedContainer(
            f'헤더가 파일 끝을 넘음 '
            f'(헤더 끝 0x{hdr_end:X} > 파일 크기 0x{file_size:X})')
    entry_buf  = f.read(entry_size * file_count)
    if len(entry_buf) != entry_size * file_count:
        raise _TruncatedContainer('컨테이너 엔트리 테이블이 잘렸습니다.')
    raw = []
    for i in range(file_count):
        off, size, soff = struct.unpack_from(
            '<QQI', entry_buf, i * entry_size)
        raw.append((off, size, soff))

    str_tbl = f.read(str_table_size)
    if len(str_tbl) != str_table_size:
        raise _TruncatedContainer('컨테이너 문자열 테이블이 잘렸습니다.')

    entries = []
    seen_names = set()
    for off, size, soff in raw:
        if soff >= len(str_tbl):
            raise ValueError('컨테이너 문자열 오프셋이 범위를 벗어났습니다.')
        try:
            nul  = str_tbl.index(b'\x00', soff)
            name = str_tbl[soff:nul].decode('utf-8', errors='ignore')
        except (ValueError, IndexError):
            raise ValueError('컨테이너 파일명이 종료되지 않았습니다.')
        if not name:
            raise ValueError('컨테이너에 빈 파일명이 있습니다.')
        if name in ('.', '..') or '/' in name or '\\' in name:
            raise ValueError(f'안전하지 않은 컨테이너 파일명입니다: {name}')
        name_key = name.casefold()
        if name_key in seen_names:
            raise ValueError(f'컨테이너에 중복 파일명이 있습니다: {name}')
        seen_names.add(name_key)
        if file_size is not None and hdr_end + off + size > file_size:
            overrun = hdr_end + off + size - file_size
            # 일부 정상 trimmed XCI는 마지막 루트 파티션의 0x200 정렬
            # 패딩만 생략한다. 시스템 파티션에 한해 이 패딩을 허용하되,
            # NCA/PFS0 데이터가 잘린 경우에는 기존처럼 오류로 처리한다.
            is_trimmed_xci_padding = (
                magic == b'HFS0'
                and name.lower() in ('secure', 'normal', 'update', 'logo')
                and 0 < overrun <= 0x200
                and hdr_end + off < file_size
            )
            if is_trimmed_xci_padding:
                size -= overrun
            else:
                raise _TruncatedContainer(
                    f"항목 '{name}'이(가) 파일 끝 너머를 가리킴 — "
                    f"다운로드 중 잘린 파일일 수 있음")
        entries.append((off, size, name))
    return entries, hdr_end


def _pfs0_index(f, ext='', file_size=None):
    """
    여러 후보 오프셋에서 PFS0/HFS0 헤더를 탐색.
    잘린 파일을 만나면 _TruncatedContainer를 위로 그대로 던진다.
    """
    offsets = (_PFS0_OFFSETS_XCI if ext in ('.xci', '.xcz')
               else _PFS0_OFFSETS_NSP)
    for base in offsets:
        try:
            result = _try_pfs0_at(f, base, file_size=file_size)
            if result is not None:
                return result
        except _TruncatedContainer:
            raise   # 다른 오프셋 시도가 무의미 → 위로 전달
        except Exception:
            continue
    return None


# ── 티켓 파싱 ─────────────────────────────────────────────────────────
def _tid_from_tik(data):
    """티켓 파일 바이너리에서 타이틀 ID 추출."""
    # 표준 티켓은 signature type에 따라 body 위치가 정해져 있고,
    # rights ID의 앞 8바이트가 Title ID다.
    if len(data) >= 4:
        sig_type = int.from_bytes(data[:4], 'little')
        body = {
            0x10003: 0x240,
            0x10004: 0x140,
            0x10005: 0x080,
        }.get(sig_type)
        if body is not None and len(data) >= body + 0x170:
            tid = data[body + 0x160:body + 0x168].hex().upper()
            if tid.startswith(('0100', '0500')):
                return tid

    # 일부 비표준/구형 티켓을 위한 제한적인 알려진 위치 폴백.
    for off in (0x29E, 0x2A0, 0x39E, 0x3A0, 0x19E, 0x1A0, 0x2B0, 0x1BE):
        if off + 8 > len(data):
            continue
        chunk = data[off:off + 8]
        if chunk[0] == 0x01 and chunk[1] == 0x00:
            tid = chunk.hex().upper()
            if len(tid) == 16:
                return tid

    # 임의의 01 00 바이트열을 TID로 오인하는 전체 데이터 검색은 하지 않는다.
    return None


# ── CNMT 바이너리 파싱 ────────────────────────────────────────────────
def _parse_cnmt(data):
    """CNMT 헤더 13바이트에서 TID/버전/타입 추출."""
    if len(data) < 13:
        return None
    try:
        tid_int = struct.unpack('<Q', data[0:8])[0]
        version = struct.unpack('<I', data[8:12])[0]
        ctype   = data[12]
        if tid_int == 0:
            return None
        tid = f'{tid_int:016X}'
        return {
            'title_id':     tid,
            'version':      version,
            'content_type': _CNMT_TYPES.get(ctype, get_content_type(tid)),
            'source':       'header_cnmt',
        }
    except Exception:
        return None


# ── CNMT XML 파싱 ─────────────────────────────────────────────────────
_CT_MAP_XML = {
    'application': 'base', 'game': 'base',
    'patch': 'update', 'update': 'update',
    'addoncontent': 'dlc', 'dlc': 'dlc',
    'addon': 'dlc', 'delta': 'delta',
}


def _parse_cnmt_xml(data):
    text = data.decode('utf-8', errors='ignore')
    tid, version, type_str = None, 0, ''

    try:
        root   = ET.fromstring(text)
        tid_el = (root.find('Id') or root.find('TitleId')
                  or root.find('titleId') or root.find('.//Id'))
        if tid_el is not None:
            raw = (tid_el.text or '').strip().lstrip('0x').lstrip('0X')
            if len(raw) == 16:
                tid = raw.upper()
        ver_el = root.find('Version') or root.find('version')
        if ver_el is not None and ver_el.text:
            version = int(ver_el.text.strip())
        type_el = (root.find('Type') or root.find('TitleType')
                   or root.find('ContentType'))
        if type_el is not None:
            type_str = (type_el.text or '').strip().lower()
    except Exception:
        pass

    if not tid:
        m = re.search(r'\b(01[0-9A-Fa-f]{14})\b', text)
        if m:
            tid = m.group(1).upper()

    if not tid:
        return None

    ct = _CT_MAP_XML.get(type_str, get_content_type(tid))
    return {
        'title_id':     tid,
        'version':      version,
        'content_type': ct,
        'source':       'header_xml',
    }


# ── NACP 파싱 (언어별 타이틀 이름) ────────────────────────────────────
_NACP_LANG_IDX = {
    0:  'en',  1:  'en',  2:  'ja',  3:  'fr',  4:  'de',
    5:  'es',  6:  'es',  7:  'it',  8:  'nl',  9:  'fr',
    10: 'pt',  11: 'ru',  12: 'ko',  13: 'zh',  14: 'zh',
}
_NACP_ENTRY_SIZE  = 0x300
_NACP_TITLE_SIZE  = 0x200
_NACP_TOTAL_LANGS = 15


def _parse_nacp(data):
    """
    control.nacp (0x4000 바이트)에서 언어별 타이틀 이름 추출.
    반환: {'ko': '이름', 'en': 'Name', 'ja': '名前', ...}
    """
    names = {}
    if len(data) < _NACP_ENTRY_SIZE * _NACP_TOTAL_LANGS:
        return names

    def is_valid_text(text):
        if not text:
            return False
        # 제어문자가 있으면 미복호화 바이너리로 간주 (탭/줄바꿈은 허용)
        for c in text:
            if 0 <= ord(c) < 32 and ord(c) not in (9, 10, 13):
                return False
        return True

    for i in range(_NACP_TOTAL_LANGS):
        off   = i * _NACP_ENTRY_SIZE
        title = data[off:off + _NACP_TITLE_SIZE]
        try:
            name = (title.split(b'\x00', 1)[0]
                    .decode('utf-8', errors='ignore').strip())
        except Exception:
            name = ''

        if is_valid_text(name):
            lang = _NACP_LANG_IDX.get(i)
            if lang and lang not in names:
                names[lang] = name

    return names


# ── NSP 컨테이너 스캔 ─────────────────────────────────────────────────
def scan_nsp_contents(filepath, log_cb=None):
    """NSP 컨테이너에서 콘텐츠 목록과 NACP 이름을 추출."""
    items      = []
    seen_tid   = set()
    nacp_names = {}
    ext        = Path(filepath).suffix.lower()

    try:
        file_size = os.path.getsize(filepath)
        with open(filepath, 'rb') as f:
            try:
                result = _pfs0_index(f, ext, file_size=file_size)
            except _TruncatedContainer as e:
                if log_cb:
                    log_cb(f'   ❌ 손상/잘린 파일: {e} — '
                           f'다시 다운로드를 권장합니다.')
                return items, nacp_names

            if result is None:
                return items, nacp_names

            entries, hdr_end = result
            nm = {name: (off, sz) for off, sz, name in entries}

            all_names  = sorted(nm.keys())
            nca_files  = [n for n in all_names
                          if n.lower().endswith('.nca')]
            tik_files  = [n for n in all_names
                          if n.lower().endswith('.tik')]
            cnmt_files = [n for n in all_names
                          if (n.lower().endswith('.cnmt')
                              and not n.lower().endswith('.nca'))]
            xml_files  = [n for n in all_names
                          if n.lower().endswith('.xml')]
            cert_files = [n for n in all_names
                          if n.lower().endswith('.cert')]
            nacp_files = [n for n in all_names
                          if n.lower().endswith('.nacp')]

            if log_cb:
                log_cb(
                    f'   📂 컨테이너 ({len(all_names)}개 항목): '
                    + (f'{len(tik_files)}티켓 '
                       if tik_files else '티켓없음 ')
                    + (f'{len(cnmt_files)}메타 '
                       if cnmt_files else '')
                    + (f'{len(xml_files)}xml '
                       if xml_files else '')
                    + (f'{len(nca_files)}콘텐츠 '
                       if nca_files else '')
                    + (f'{len(cert_files)}cert' if cert_files else ''))

            # NACP 직접 파싱
            for name in nacp_files:
                off, sz = nm[name]
                if sz >= 0x300:
                    f.seek(hdr_end + off)
                    parsed = _parse_nacp(f.read(min(sz, 0x4800)))
                    if parsed:
                        nacp_names.update(parsed)
                        break

            # CNMT 바이너리
            for name in cnmt_files:
                off, sz = nm[name]
                f.seek(hdr_end + off)
                item = _parse_cnmt(f.read(min(sz, 64)))
                if item and item['title_id'] not in seen_tid:
                    items.append(item)
                    seen_tid.add(item['title_id'])

            # XML 메타데이터
            for name in xml_files:
                off, sz = nm[name]
                if sz > _MAX_XML_METADATA_SIZE:
                    if log_cb:
                        log_cb(
                            f'   ⚠ 비정상적으로 큰 XML 건너뜀: '
                            f'{name} ({sz:,} bytes)')
                    continue
                f.seek(hdr_end + off)
                item = _parse_cnmt_xml(f.read(sz))
                if item and item['title_id'] not in seen_tid:
                    items.append(item)
                    seen_tid.add(item['title_id'])

            # 티켓에서 TID
            for name in tik_files:
                off, sz = nm[name]
                f.seek(hdr_end + off)
                tik_data = f.read(min(sz, 0x500))
                tid      = _tid_from_tik(tik_data)
                if tid and tid not in seen_tid:
                    items.append({
                        'title_id':     tid,
                        'version':      0,
                        'content_type': get_content_type(tid),
                        'source':       'header_tik',
                    })
                    seen_tid.add(tid)
                elif not tid and log_cb:
                    log_cb(f'   ⚠ 티켓 ID 추출 실패: {name}')

            if not items and nca_files and log_cb:
                log_cb(f'   ⚠ 티켓/메타데이터 없음 — '
                       f'콘텐츠 {len(nca_files)}개만 존재 '
                       f'(키 파일 필요)')

    except Exception as e:
        if log_cb:
            log_cb(f'   ❌ 파싱 예외: {e}')

    return items, nacp_names


# ── 외부 분석 엔진 탐색 ───────────────────────────────────────────────
def find_nstool():
    """분석 엔진 위치 — ./bin/ 우선, 그 다음 PATH."""
    base = (sys._MEIPASS
            if (getattr(sys, 'frozen', False) and hasattr(sys, '_MEIPASS'))
            else _BASE_DIR)
    exe_name = 'nstool.exe' if sys.platform == 'win32' else 'nstool'

    bin_path = os.path.join(base, 'bin', exe_name)
    if os.path.isfile(bin_path):
        return bin_path
    return shutil.which('nstool')


def _find_hactool():
    """보조 분석 엔진 위치 — ./bin/ 우선, 그 다음 PATH."""
    base = (sys._MEIPASS
            if (getattr(sys, 'frozen', False) and hasattr(sys, '_MEIPASS'))
            else _BASE_DIR)
    exe_name = 'hactool.exe' if sys.platform == 'win32' else 'hactool'

    bin_path = os.path.join(base, 'bin', exe_name)
    if os.path.isfile(bin_path):
        return bin_path
    return shutil.which('hactool')


# ── AES 헬퍼 (cryptography 또는 pycryptodome) ─────────────────────────
def _aes128_ecb(key, data, encrypt):
    try:
        from Crypto.Cipher import AES as _AES
        c = _AES.new(key, _AES.MODE_ECB)
        return c.encrypt(data) if encrypt else c.decrypt(data)
    except ImportError:
        pass
    try:
        from cryptography.hazmat.primitives.ciphers import (
            Cipher, algorithms, modes,
        )
        from cryptography.hazmat.backends import default_backend
        c  = Cipher(algorithms.AES(key), modes.ECB(),
                    backend=default_backend())
        op = c.encryptor() if encrypt else c.decryptor()
        return op.update(data) + op.finalize()
    except ImportError:
        pass
    return None


# ── 키 파일 파싱 ──────────────────────────────────────────────────────
def _parse_prod_keys(path):
    result = {}
    try:
        with open(path, 'r', encoding='utf-8', errors='ignore') as f:
            for line in f:
                line = line.strip()
                if not line or line.startswith('#') or '=' not in line:
                    continue
                k, _, v = line.partition('=')
                result[k.strip().lower()] = v.strip().lower()
    except Exception:
        pass
    return result


def _decrypt_title_key(tik_data, prod_keys):
    """티켓 바이너리 → (rights_id, title_key) 복호화."""
    try:
        sig_type = int.from_bytes(tik_data[:4], 'little')
    except Exception:
        return None
    body = {0x10003: 0x240, 0x10004: 0x140, 0x10005: 0x080}.get(sig_type)
    if body is None or len(tik_data) < body + 0x170:
        return None

    enc_title_key = tik_data[body + 0x040: body + 0x050]
    key_rev       = tik_data[body + 0x145]
    rights_id     = tik_data[body + 0x160: body + 0x170]

    titlekek_src_hex = prod_keys.get('titlekek_source', '')
    master_key_hex   = (prod_keys.get(f'master_key_{key_rev:02x}')
                        or prod_keys.get('master_key_00', ''))
    if not titlekek_src_hex or not master_key_hex:
        return None

    try:
        titlekek_src = bytes.fromhex(titlekek_src_hex)
        master_key   = bytes.fromhex(master_key_hex)
    except ValueError:
        return None

    titlekek = _aes128_ecb(master_key, titlekek_src, encrypt=False)
    if not titlekek:
        return None
    title_key = _aes128_ecb(titlekek, enc_title_key, encrypt=False)
    if not title_key:
        return None
    return rights_id.hex().upper(), title_key.hex().upper()


def _extract_tiks_from_pfs0(filepath):
    results = []
    ext     = Path(filepath).suffix.lower()
    try:
        with open(filepath, 'rb') as f:
            file_size = os.path.getsize(filepath)
            result = _pfs0_index(f, ext, file_size=file_size)
            if not result:
                return []
            entries, hdr_end = result
            for off, sz, name in entries:
                if (name.lower().endswith('.tik')
                        and 0 < sz <= _MAX_TICKET_SIZE):
                    f.seek(hdr_end + off)
                    results.append(f.read(sz))
    except Exception:
        pass
    return results


def _make_temp_title_keys(filepath, prod_keys_path, tmpdir, log_cb=None):
    """파일 내부 티켓들을 복호화해 임시 title.keys로 저장."""
    def _log(msg):
        if log_cb:
            log_cb(msg)

    tiks = _extract_tiks_from_pfs0(filepath)
    if not tiks:
        _log('   ℹ 티켓 없음 (ticketless)')
        return None

    prod_keys = _parse_prod_keys(prod_keys_path)
    if not prod_keys or 'titlekek_source' not in prod_keys:
        _log('   ❌ 키 파일 파싱 실패')
        return None

    entries  = []
    existing = set()
    out_path = os.path.join(tmpdir, 'title.keys')

    if os.path.isfile(out_path):
        with open(out_path, 'r', encoding='utf-8', errors='ignore') as f:
            for line in f:
                if '=' in line:
                    existing.add(line.split('=')[0].strip())

    for i, tik_data in enumerate(tiks):
        try:
            result = _decrypt_title_key(tik_data, prod_keys)
            if result:
                rights_id, title_key = result
                entry = f'{rights_id} = {title_key}'
                if rights_id not in existing:
                    entries.append(entry)
                _log(f'   🔑 티켓 {i+1} 복호화 성공: {rights_id[:16]}…')
            else:
                _log(f'   ❌ 티켓 {i+1} 복호화 실패')
        except Exception as e:
            _log(f'   ❌ 티켓 {i+1} 예외: {e}')

    if not entries:
        return None

    mode = 'a' if os.path.isfile(out_path) else 'w'
    with open(out_path, mode, encoding='utf-8') as f:
        f.write('\n'.join(entries) + '\n')
    return out_path


def _find_key_file(name):
    """키 파일 — ./{name} 우선, 그 다음 ~/.switch/{name}."""
    exe_dir_path = os.path.join(_BASE_DIR, name)
    if os.path.isfile(exe_dir_path):
        return exe_dir_path
    candidates = [os.path.join(os.path.expanduser('~'), '.switch', name)]
    if os.name == 'nt':
        userprofile = os.environ.get('USERPROFILE', '')
        if userprofile:
            candidates.append(os.path.join(userprofile, '.switch', name))
    for p in candidates:
        if os.path.isfile(p):
            return p
    return None


def _find_prod_keys():
    return _find_key_file('prod.keys')


# ── 외부 분석 엔진 실행 헬퍼 ──────────────────────────────────────────
def _run_nstool(nstool_path, filepath, extra_args=None, log_cb=None):
    """분석 엔진 실행 → stdout+stderr 합쳐서 반환."""
    try:
        has_keyset  = extra_args and '--keyset' in extra_args
        keyset_args = []
        if not has_keyset:
            keys = _find_prod_keys()
            if keys:
                keyset_args = ['--keyset', keys]
            else:
                if log_cb:
                    log_cb('   ⚠ 키 파일을 찾지 못했습니다')
                return ''

        cmd = ([nstool_path] + keyset_args
               + (extra_args or []) + [filepath])
        r = subprocess.run(
            cmd, capture_output=True,
            encoding='utf-8', errors='ignore',
            timeout=_NSTOOL_TIMEOUT,
            startupinfo=_NT_STARTUPINFO,
            creationflags=_NT_CREATIONFLAGS)
        return r.stdout + r.stderr
    except FileNotFoundError:
        if log_cb:
            log_cb('   ❌ 분석 엔진 실행 파일을 찾을 수 없습니다')
        return ''
    except Exception as e:
        if log_cb:
            log_cb(f'   ❌ 분석 엔진 오류: {e}')
        return ''


def _label_to_ctype(line):
    """분석 엔진 출력 라벨 → 콘텐츠 타입.
    PatchId/Patch Id          → update
    AddOnContentId/AddonId    → dlc
    ApplicationId/TitleId     → base (호출부에서 처리)
    """
    ll = line.lower()
    flat = ll.replace(' ', '')
    if 'patchid' in flat or 'patch id' in ll:
        return 'update'
    if 'addoncontentid' in flat or 'addonid' in flat:
        return 'dlc'
    if 'applicationid' in flat or 'application id' in ll:
        return 'base'
    return None


def _tids_from_nstool_output(output, filepath=None):
    """분석 엔진 텍스트 출력에서 (TID, 버전, 타입) 항목 목록 추출."""
    items       = []
    seen        = set()
    current_tid = None
    current_ver = 0
    current_ct  = None

    def _flush():
        nonlocal current_tid, current_ver, current_ct
        if current_tid and current_tid not in seen:
            seen.add(current_tid)
            items.append({
                'title_id':     current_tid,
                'version':      current_ver,
                'content_type': (current_ct
                                 or get_content_type(current_tid)),
                'source':       'header_nstool',
            })

    for line in output.splitlines():
        m_tid = _RE_NSTOOL_TID.search(line)
        if m_tid:
            _flush()
            current_tid = m_tid.group(1).upper()
            current_ver = 0
            current_ct  = _label_to_ctype(line)
            if len(current_tid) != 16:
                current_tid = None
                current_ct  = None
            continue

        if current_tid and 'Version' in line and 'Required' not in line:
            stripped = line.strip()
            if stripped.startswith('Version'):
                mv_p = _RE_CNMT_VER_V.search(line)
                if mv_p:
                    current_ver = int(mv_p.group(1))
                else:
                    m_ver = _RE_NSTOOL_VER.search(line)
                    if m_ver:
                        current_ver = int(m_ver.group(1))

        if current_tid and 'Type:' in line:
            tl = line.lower()
            if   'patch' in tl:                          current_ct = 'update'
            elif 'addoncontent' in tl or 'addon' in tl:  current_ct = 'dlc'
            elif 'application' in tl:                    current_ct = 'base'

    _flush()

    # 폴백: 파일 경로에서 TID 추출
    if not items and filepath:
        m_fb = _RE_NSTOOL_CMD_FALLBACK.search(filepath)
        if m_fb:
            tid = m_fb.group(1).upper()
            if tid.startswith('01') or tid.startswith('05'):
                items.append({
                    'title_id':     tid,
                    'version':      0,
                    'content_type': get_content_type(tid),
                    'source':       'nstool_rescue',
                })

    # 폴백: 두 번째 정규식으로 재시도
    if not items:
        _tid, _ver, _type = None, 0, 'unknown'
        for line in output.splitlines():
            mt = _RE_CNMT_TITLE.search(line)
            if mt:
                _tid = mt.group(1).upper()
            stripped = line.strip()
            if (_tid and _ver == 0
                    and stripped.startswith('Version:')
                    and 'Required' not in line):
                mv_paren = _RE_CNMT_VER_V.search(line)
                if mv_paren:
                    _ver = int(mv_paren.group(1))
                else:
                    mv = _RE_NSTOOL_VER.search(line)
                    if mv:
                        _ver = int(mv.group(1))
            if 'Type:' in line and _tid is not None:
                tl = line.lower()
                if   'application' in tl: _type = 'base'
                elif 'patch' in tl:       _type = 'update'
                elif 'addoncontent' in tl or 'addon' in tl: _type = 'dlc'
        if _tid and len(_tid) == 16:
            items.append({
                'title_id':     _tid,
                'version':      _ver,
                'content_type': (_type if _type != 'unknown'
                                 else get_content_type(_tid)),
                'source':       'header_nstool',
            })
    return items


# ── 메타데이터 콘텐츠 추출 ────────────────────────────────────────────
def _extract_cnmt_ncas_from_pfs0(filepath):
    """PFS0에서 메타데이터 콘텐츠 추출. [(name, data), …]."""
    results = []
    ext     = Path(filepath).suffix.lower()
    try:
        with open(filepath, 'rb') as f:
            file_size = os.path.getsize(filepath)
            result = _pfs0_index(f, ext, file_size=file_size)
            if not result:
                return []
            entries, hdr_end = result
            cnmt_entries = [
                (off, sz, name) for off, sz, name in entries
                if name.lower().endswith('.cnmt.nca')
                and 0 < sz <= _MAX_META_NCA_SIZE
            ]
            if not cnmt_entries:
                # 확장자가 잘못된 Meta NCA 폴백. 대형 Program/RomFS NCA를
                # 통째로 메모리에 올리지 않도록 작은 후보만 제한적으로 시도.
                cnmt_entries = sorted(
                    [(off, sz, name) for off, sz, name in entries
                     if name.lower().endswith('.nca')
                     and 0 < sz <= _MAX_META_NCA_SIZE],
                    key=lambda x: x[1])
            for off, sz, name in cnmt_entries:
                f.seek(hdr_end + off)
                results.append((name, f.read(sz)))
    except Exception:
        pass
    return results


def _extract_aes_key_and_part0(nstool_path, nca_path, ks_args, part0_dir,
                               log_cb=None):
    """콘텐츠에서 AES-CTR 키 추출 후 part0 디렉터리로 복호 추출."""
    sk_out  = _run_nstool(
        nstool_path, nca_path, extra_args=ks_args + ['--showkeys'])
    ctr_key = None
    for line in sk_out.splitlines():
        if 'AES-CTR Key:' in line:
            m = re.search(r'([0-9A-Fa-f]{32})', line)
            if m:
                ctr_key = m.group(1)
                break
    if ctr_key:
        os.makedirs(part0_dir, exist_ok=True)
        _run_nstool(
            nstool_path, nca_path,
            extra_args=ks_args + ['--contentkey', ctr_key,
                                  '--part0', part0_dir])
    return ctr_key


def _extract_nacp_from_part0(part0_dir):
    """part0 폴더에서 .nacp 파일 → 언어별 이름 반환."""
    if not part0_dir or not os.path.exists(part0_dir):
        return {}
    for fn in os.listdir(part0_dir):
        if fn.lower().endswith('.nacp'):
            nacp_path = os.path.join(part0_dir, fn)
            try:
                with open(nacp_path, 'rb') as f:
                    data = f.read(0x4800)
                parsed = _parse_nacp(data)
                if parsed:
                    return parsed
            except Exception:
                pass
    return {}


def _get_ncas_from_pfs0(p_path):
    """PFS0 → [(size, data_offset, name), …]. 큰 파일부터."""
    try:
        with open(p_path, 'rb') as f:
            file_size = os.path.getsize(p_path)
            result = _pfs0_index(
                f, Path(p_path).suffix.lower(), file_size=file_size)
            if not result:
                return []
            entries, data_start = result
            # Control 콘텐츠 상한 15MB (보통 10MB 이내), 큰 파일부터.
            return sorted(
                [(sz, data_start + off, name)
                 for off, sz, name in entries
                 if 0 < sz <= 15 * 1024 * 1024
                 and name.lower().endswith('.nca')],
                reverse=True)
    except Exception:
        return []


# ── 보조 분석 엔진 키 동기화 ──────────────────────────────────────────
def _prepare_hactool_keys():
    """
    보조 엔진이 유니코드 경로를 처리하지 못하는 문제를 우회하기 위해
    키 파일을 기본 검색 경로(~/.switch)에 임시 복사.
    """
    original_keys_path = _find_prod_keys()
    if not original_keys_path:
        return None, None

    switch_dir = Path.home() / '.switch'
    switch_dir.mkdir(exist_ok=True)
    temp_keys_path = switch_dir / 'prod.keys'

    # 기존 임시 파일이 있으면 백업
    backup_path = None
    if temp_keys_path.exists():
        try:
            backup_path = temp_keys_path.with_suffix('.bak')
            temp_keys_path.rename(backup_path)
        except OSError:
            backup_path = None

    try:
        shutil.copy2(original_keys_path, temp_keys_path)
    except Exception:
        if backup_path and backup_path.exists():
            backup_path.rename(temp_keys_path)
        return None, None

    return temp_keys_path, backup_path


def _cleanup_hactool_keys(temp_path, backup_path):
    """_prepare_hactool_keys의 임시 파일과 백업 정리."""
    if not temp_path:
        return
    try:
        if temp_path.exists():
            temp_path.unlink()
        if backup_path and backup_path.exists():
            backup_path.rename(temp_path)
    except Exception:
        pass


def _run_hactool(hactool_path, args):
    """보조 엔진 실행 — 키 동기화 후 -k 없이 호출.

    경쟁 조건 주의: ~/.switch/prod.keys 의 백업/스왑/복원은 동시 실행 불가.
    스캔 워커가 여러 개일 때 동시에 들어오면 직렬화한다.
    """
    with _HACTOOL_LOCK:
        temp_keys, backup_keys = _prepare_hactool_keys()
        cmd = [hactool_path, '--disablekeywarns'] + args

        try:
            result = subprocess.run(
                cmd, capture_output=True,
                encoding='utf-8', errors='ignore',
                startupinfo=_NT_STARTUPINFO,
                creationflags=_NT_CREATIONFLAGS)
        finally:
            _cleanup_hactool_keys(temp_keys, backup_keys)

    return result


def _extract_nacp_with_hactool(hactool_path, filepath, log_cb=None):
    """보조 엔진으로 NACP(국가별 타이틀명) 추출."""
    with tempfile.TemporaryDirectory(prefix='slm_aux_') as td:
        tdp = Path(td)

        def check_nca(np_path):
            rd = tdp / f'r_{np_path.stem}'
            rd.mkdir(exist_ok=True)
            _run_hactool(
                hactool_path, [f'--section0dir={rd}', str(np_path)])
            cp = rd / 'control.nacp'
            if cp.is_file():
                return _parse_nacp(cp.read_bytes())
            return None

        # NSP/NSZ
        if filepath.lower().endswith(('.nsp', '.nsz')):
            merged_nacp = {}
            for sz, off, nm in _get_ncas_from_pfs0(filepath):
                # 메타 콘텐츠는 스킵 (이름 정보 없음)
                if nm.lower().endswith('.cnmt.nca'):
                    continue

                nca_path = tdp / nm
                try:
                    with open(filepath, 'rb') as s, open(nca_path, 'wb') as d:
                        s.seek(off)
                        d.write(s.read(sz))
                    res = check_nca(nca_path)
                    if res:
                        if log_cb:
                            log_cb('   ✅ 국가별 이름 추출 성공')
                        for k, v in res.items():
                            if k not in merged_nacp:
                                merged_nacp[k] = v
                        if 'ko' in merged_nacp and 'en' in merged_nacp:
                            return merged_nacp
                except Exception:
                    pass
            return merged_nacp

        # XCI/XCZ
        if filepath.lower().endswith(('.xci', '.xcz')):
            res = _run_hactool(
                hactool_path,
                ['-t', 'xci', '--listnca', str(filepath)])
            if not res or res.returncode != 0:
                return {}
            target = next(
                (l.split()[-1] for l in res.stdout.splitlines()
                 if 'Control' in l), None)
            if target:
                _run_hactool(
                    hactool_path,
                    ['-t', 'xci', f'--nca={target}',
                     f'--outdir={td}', str(filepath)])
                nca = tdp / target
                if nca.exists():
                    return check_nca(nca) or {}
    return {}


# ── 본격 분석 — 외부 엔진 사용 ────────────────────────────────────────
def scan_nstool(filepath, nstool_path, log_cb=None):
    """
    파일을 외부 엔진으로 심층 분석.
    반환값: (contents_list, nacp_names_dict)
    """
    prod_keys_path = _find_prod_keys()
    is_xci = filepath.lower().endswith(('.xci', '.xcz'))

    if not filepath.lower().endswith(('.nsp', '.nsz', '.xci', '.xcz')):
        return [], {}

    all_items  = []
    seen_tids  = set()
    nacp_names = {}

    with tempfile.TemporaryDirectory(prefix='slm_scan_') as tmpdir:
        if not nstool_path:
            nstool_path = find_nstool()
        if not nstool_path:
            return [], nacp_names

        ks_args = (['--keyset', prod_keys_path]
                   if prod_keys_path else [])

        if is_xci:
            if log_cb:
                log_cb('   ℹ XCI 고속 분석 진행 중…')

            xci_extracted = _extract_cnmt_ncas_from_xci(
                filepath, tmpdir, log_cb, categorized=True)
            cnmt_paths    = xci_extracted.get('cnmt', [])
            control_paths = xci_extracted.get('control', [])

            if cnmt_paths:
                hactool_path = _find_hactool()

                # 1) 메타데이터 → part0 추출 → 바이너리 직접 파싱
                # 멀티 게임 XCI의 base/update/dlc를 가장 정확히 잡는 경로.
                for nca_path in cnmt_paths:
                    nca_name  = os.path.basename(nca_path)
                    part0_dir = os.path.join(tmpdir, f'p0_{nca_name}')

                    parsed_via_cnmt = False
                    try:
                        ctr_key = _extract_aes_key_and_part0(
                            nstool_path, nca_path, ks_args, part0_dir,
                            log_cb=log_cb)
                        if ctr_key and os.path.isdir(part0_dir):
                            for cn in os.listdir(part0_dir):
                                if not cn.lower().endswith('.cnmt'):
                                    continue
                                try:
                                    with open(
                                            os.path.join(part0_dir, cn),
                                            'rb') as cf:
                                        cnmt_data = cf.read(64)
                                    item = _parse_cnmt(cnmt_data)
                                    if item:
                                        tid = item['title_id']
                                        if tid and tid not in seen_tids:
                                            seen_tids.add(tid)
                                            all_items.append(item)
                                            parsed_via_cnmt = True
                                            _log_extraction(item, log_cb)
                                        break
                                except Exception:
                                    continue
                    except Exception as e:
                        if log_cb:
                            log_cb(
                                f'   ⚠ 메타 추출 실패: '
                                f'{nca_name}: {e}')

                    # 폴백: 엔진 텍스트 출력에서 첫 TID 1개만 채택
                    # (참조 TID 누설 방지 — cnmt 1개당 콘텐츠 1개)
                    if not parsed_via_cnmt:
                        output = _run_nstool(
                            nstool_path, nca_path, extra_args=ks_args)
                        parsed = _tids_from_nstool_output(
                            output, filepath=nca_path)
                        if parsed:
                            item = parsed[0]
                            tid  = item.get('title_id', '')
                            if tid and tid not in seen_tids:
                                seen_tids.add(tid)
                                all_items.append(item)
                                _log_extraction(item, log_cb)

                # 2) Control 콘텐츠에서 NACP 추출
                if hactool_path:
                    for nca_path in control_paths:
                        nca_name = os.path.basename(nca_path)
                        rd = os.path.join(tmpdir, f'r_{nca_name}')
                        os.makedirs(rd, exist_ok=True)
                        _run_hactool(
                            hactool_path,
                            ['--section0dir=' + rd, nca_path])
                        cp = os.path.join(rd, 'control.nacp')
                        if os.path.isfile(cp):
                            try:
                                with open(cp, 'rb') as nf:
                                    parsed = _parse_nacp(nf.read(0x4800))
                                for k, v in parsed.items():
                                    if k not in nacp_names:
                                        nacp_names[k] = v
                            except Exception:
                                pass

            # 1차 실패 시 폴백 (XCZ 등)
            if not all_items:
                if log_cb:
                    log_cb('   ⚠ 고속 분석 실패 → 기본 분석 시도')
                output = _run_nstool(
                    nstool_path, filepath, extra_args=ks_args,
                    log_cb=log_cb)
                for item in _tids_from_nstool_output(
                        output, filepath=filepath):
                    tid = item.get('title_id', '')
                    if tid and tid not in seen_tids:
                        seen_tids.add(tid)
                        all_items.append(item)

            return all_items, nacp_names

        # ── NSP/NSZ 분기 ───────────────────────────────────────────
        cnmt_ncas = _extract_cnmt_ncas_from_pfs0(filepath)
        if not cnmt_ncas:
            output = _run_nstool(nstool_path, filepath, log_cb=log_cb)
            return (_tids_from_nstool_output(output, filepath=filepath),
                    nacp_names)

        if log_cb:
            log_cb(f'   🔑 메타데이터 {len(cnmt_ncas)}개 분석 중…')

        if prod_keys_path:
            tk_dir = (_BASE_DIR if os.access(_BASE_DIR, os.W_OK)
                      else os.path.dirname(prod_keys_path))
            _make_temp_title_keys(
                filepath, prod_keys_path, tk_dir, log_cb=log_cb)

        # 티켓 맵 미리 구성 (디버그 용도)
        tik_map = {}
        try:
            with open(filepath, 'rb') as _f:
                _r = _pfs0_index(_f, Path(filepath).suffix.lower())
                if _r:
                    _entries, _hdr_end = _r
                    for _off, _sz, _name in _entries:
                        if _name.endswith('.tik') and _sz > 0:
                            _f.seek(_hdr_end + _off)
                            _tp = os.path.join(tmpdir, _name)
                            with open(_tp, 'wb') as _tf:
                                _tf.write(_f.read(_sz))
                            tik_map[_name[:16].upper()] = _tp
        except Exception:
            pass

        for nca_name, nca_data in cnmt_ncas:
            tmp_path = os.path.join(tmpdir, nca_name)
            try:
                with open(tmp_path, 'wb') as f:
                    f.write(nca_data)

                part0_dir = os.path.join(tmpdir, f'{nca_name}_part0')
                ctr_key   = _extract_aes_key_and_part0(
                    nstool_path, tmp_path, ks_args, part0_dir)
                cnmt_path = None
                if ctr_key and os.path.exists(part0_dir):
                    for cn in os.listdir(part0_dir):
                        if cn.endswith('.cnmt'):
                            cnmt_path = os.path.join(part0_dir, cn)
                            break

                # 1순위: 메타 바이너리 헤더 직접 파싱
                # 텍스트 출력에는 'Application Title Id' 같은 참조 TID도
                # 함께 나와 false-positive 발생 가능. 바이너리 헤더는
                # 자기 자신의 (TID, version, type) 1개만 담는다.
                parsed_via_cnmt = False
                if cnmt_path:
                    try:
                        with open(cnmt_path, 'rb') as cf:
                            cnmt_data = cf.read(64)
                        item = _parse_cnmt(cnmt_data)
                        if item:
                            tid = item['title_id']
                            if tid and tid not in seen_tids:
                                seen_tids.add(tid)
                                all_items.append(item)
                                parsed_via_cnmt = True
                                _log_extraction(item, log_cb)
                    except Exception:
                        pass

                # 2순위 폴백: 엔진 텍스트 출력에서 첫 TID 1개만 채택
                if not parsed_via_cnmt:
                    output = _run_nstool(
                        nstool_path, cnmt_path or tmp_path,
                        extra_args=ks_args or None, log_cb=log_cb)
                    parsed = _tids_from_nstool_output(
                        output, filepath=cnmt_path or tmp_path)
                    if parsed:
                        item = parsed[0]
                        tid  = item.get('title_id', '')
                        if tid and tid not in seen_tids:
                            seen_tids.add(tid)
                            all_items.append(item)
                            _log_extraction(item, log_cb)
            except Exception:
                continue

    return all_items, nacp_names


# ── XCI 컨테이너 분해 ─────────────────────────────────────────────────
def _extract_cnmt_ncas_from_xci(filepath, tmpdir, log_cb=None,
                                categorized=False):
    """
    XCI 파일에서 메타데이터와 컨트롤 콘텐츠를 동시에 고속 추출.

    categorized=False: 모든 콘텐츠 경로 리스트 반환 (하위 호환)
    categorized=True : {'cnmt': [경로…], 'control': [경로…]} 반환

    XCI 헤더 레이아웃:
      0x0000 .. 0x1000  CardKeyArea (untrimmed XCI에만 존재)
      0x1000 .. 0x1200  CardHeader  (trimmed XCI는 0x0에서 시작)
        +0x100  Magic "HEAD"
        +0x130  PartitionFsHeaderAddress (uint64 LE)
        +0x138  PartitionFsHeaderSize    (uint64 LE)
      0x10000          → root HFS0 일반적 위치
    """
    empty = {'cnmt': [], 'control': []} if categorized else []
    try:
        file_size = os.path.getsize(filepath)
        with open(filepath, 'rb') as f:

            # 1) HFS0 root 오프셋 결정
            root_hfs0_offset = None

            for hdr_base in (0x1000, 0x0):
                f.seek(hdr_base + 0x100)
                if f.read(4) != b'HEAD':
                    continue
                f.seek(hdr_base + 0x130)
                try:
                    addr = struct.unpack('<Q', f.read(8))[0]
                    candidates = []
                    if 0 < addr < 1 << 40:   # 1 TB sanity
                        candidates.append(addr)
                    if 0 < addr < 1 << 32:   # 페이지 단위 해석 시도
                        candidates.append(addr * 0x200)
                    for cand in candidates:
                        try:
                            if _try_pfs0_at(
                                    f, cand, file_size=file_size):
                                root_hfs0_offset = cand
                                break
                        except _TruncatedContainer as e:
                            _log_truncated(log_cb, e)
                            return empty
                except Exception:
                    pass
                if root_hfs0_offset is not None:
                    break

            # 2) 헤더 파싱 실패 시 흔한 위치들 brute-force
            if root_hfs0_offset is None:
                for cand in (0x10000, 0xF000, 0x8000, 0x4000, 0x1000, 0x0):
                    try:
                        res = _try_pfs0_at(
                            f, cand, file_size=file_size)
                    except _TruncatedContainer as e:
                        _log_truncated(log_cb, e)
                        return empty
                    if not res:
                        continue
                    entries, _ = res
                    names = {n.lower() for _, _, n in entries}
                    if ('secure' in names or 'normal' in names
                            or 'update' in names):
                        root_hfs0_offset = cand
                        break

            if root_hfs0_offset is None:
                if log_cb:
                    log_cb('   ⚠ 컨테이너 위치를 찾지 못함')
                return empty

            # 3) Secure 파티션 진입
            try:
                root_res = _try_pfs0_at(
                    f, root_hfs0_offset, file_size=file_size)
            except _TruncatedContainer as e:
                _log_truncated(log_cb, e)
                return empty
            if not root_res:
                return empty
            root_entries, root_hdr_end = root_res

            secure_entry = next(
                ((off, sz) for off, sz, name in root_entries
                 if name.lower() == 'secure'),
                None)
            if secure_entry is None:
                if log_cb:
                    log_cb('   ⚠ 보안 파티션을 찾지 못함')
                return empty
            secure_off, _ = secure_entry
            secure_base   = root_hdr_end + secure_off

            try:
                sec_res = _try_pfs0_at(
                    f, secure_base, file_size=file_size)
            except _TruncatedContainer as e:
                _log_truncated(log_cb, e)
                return empty
            if not sec_res:
                if log_cb:
                    log_cb('   ⚠ 보안 파티션 진입 실패')
                return empty
            sec_entries, sec_hdr_end = sec_res

            # 4) 메타 콘텐츠와 컨트롤 후보 분리
            cnmt_ncas = [
                (off, sz, name) for off, sz, name in sec_entries
                if (name.lower().endswith('.cnmt.nca')
                    or name.lower().endswith('.cnmt.ncz'))
                and sz > 0
            ]
            if not cnmt_ncas:
                # 폴백: 가장 작은 콘텐츠 1개
                ncas = [(off, sz, name) for off, sz, name in sec_entries
                        if (name.lower().endswith('.nca')
                            or name.lower().endswith('.ncz'))
                        and sz > 0]
                if ncas:
                    cnmt_ncas = [min(ncas, key=lambda x: x[1])]

            # Control 콘텐츠 후보 (보통 10KB~15MB, 메타 제외)
            control_candidates = [
                (off, sz, name) for off, sz, name in sec_entries
                if (name.lower().endswith('.nca')
                    or name.lower().endswith('.ncz'))
                and not name.lower().endswith('.cnmt.nca')
                and not name.lower().endswith('.cnmt.ncz')
                and 10 * 1024 < sz < 15 * 1024 * 1024
            ]

            # 5) 임시 폴더에 추출
            cnmt_paths    = []
            control_paths = []
            seen = set()

            for category, entries in (('cnmt', cnmt_ncas),
                                      ('control', control_candidates)):
                for off, sz, name in entries:
                    if (off, sz, name) in seen:
                        continue
                    seen.add((off, sz, name))
                    out_path = os.path.join(tmpdir, name)
                    if not os.path.exists(out_path):
                        f.seek(sec_hdr_end + off)
                        with open(out_path, 'wb') as out_f:
                            out_f.write(f.read(sz))
                    if category == 'cnmt':
                        cnmt_paths.append(out_path)
                    else:
                        control_paths.append(out_path)

            if log_cb:
                log_cb(
                    f'   📦 컨테이너 분해: 메타 {len(cnmt_paths)}개, '
                    f'콘텐츠 {len(control_paths)}개')

            if categorized:
                return {'cnmt': cnmt_paths, 'control': control_paths}
            return cnmt_paths + control_paths
    except Exception as e:
        if log_cb:
            log_cb(f'   ⚠ 부분 추출 예외: {e}')
        return empty


# ── 캐시 매니저 ───────────────────────────────────────────────────────
class CacheManager:
    """폴더별 스캔 결과 캐시. 스레드 안전한 save() 보장."""

    def __init__(self, folder):
        self.folder = folder
        self._lock  = threading.Lock()
        h           = hashlib.sha256(
            os.path.abspath(folder).encode()).hexdigest()[:16]
        self._dir   = os.path.join(CACHE_BASE_DIR, h)
        self._file  = os.path.join(self._dir, CACHE_FILE)
        self._data  = {}
        self._dirty = False
        self._load()

    def _load(self):
        if not os.path.exists(self._file):
            return
        try:
            with open(self._file, 'rb') as f:
                d = _json_loads(f.read())
            if d.get('version') == CACHE_VERSION:
                self._data = d.get('entries', {})
        except Exception:
            self._data = {}

    def save(self):
        with self._lock:
            if not self._dirty:
                return
            data_snapshot = dict(self._data)

        try:
            os.makedirs(self._dir, exist_ok=True)
            with open(os.path.join(self._dir, 'folder.txt'),
                      'w', encoding='utf-8') as f:
                f.write(self.folder)
            tmp = self._file + '.tmp'
            with open(tmp, 'wb') as f:
                f.write(_json_dumps_bytes({
                    'version':   CACHE_VERSION,
                    'generated': datetime.now().isoformat(),
                    'folder':    self.folder,
                    'entries':   data_snapshot,
                }))
            os.replace(tmp, self._file)
            with self._lock:
                self._dirty = False
        except Exception:
            pass

    def get(self, path, mtime, size):
        with self._lock:
            e = self._data.get(path)
        if (e and abs(e.get('mtime', 0) - mtime) < 1.0
                and e.get('size') == size):
            return e
        return None

    def get_by_hash(self, path, fast_hash):
        with self._lock:
            e = self._data.get(path)
        return e if (e and e.get('fast_hash') == fast_hash) else None

    def put(self, path, entry):
        with self._lock:
            self._data[path] = entry
            self._dirty = True

    def touch_mtime(self, path, mtime):
        with self._lock:
            if path in self._data:
                self._data[path]['mtime'] = mtime
                self._dirty = True

    def cleanup(self, existing):
        with self._lock:
            dead = [p for p in self._data if p not in existing]
            for p in dead:
                del self._data[p]
            if dead:
                self._dirty = True
        return len(dead)

    def stats(self):
        with self._lock:
            total = len(self._data)
        return {'total': total, 'cache_dir': self._dir,
                'cache_file': self._file}


# ── 단일 파일 스캔 ────────────────────────────────────────────────────
def scan_file(path, cache, nstool=None, log_cb=None):
    """단일 파일 분석 → DB에 저장할 entry dict 반환."""
    fp    = Path(path)
    stat  = fp.stat()
    mtime = stat.st_mtime
    size  = stat.st_size
    ext   = fp.suffix.lower()

    def _nacp_empty(c):
        return (not c.get('nacp_ko')
                and not c.get('nacp_en')
                and not c.get('nacp_ja'))

    # 캐시 히트 (mtime/size 일치, NACP 채워짐)
    cached = cache.get(path, mtime, size)
    if cached and not _nacp_empty(cached):
        return {**cached, 'cache_hit': True}

    # 해시 기반 캐시 히트 (mtime만 다른 경우)
    fh = compute_fast_hash(path)
    cached = cache.get_by_hash(path, fh)
    if cached and not _nacp_empty(cached):
        cache.touch_mtime(path, mtime)
        return {**cached, 'mtime': mtime, 'cache_hit': True}

    contents   = []
    nacp_names = {}
    if log_cb:
        log_cb(f'🔍 헤더 분석: {fp.name}')

    if ext in ('.nsp', '.nsz', '.xci', '.xcz'):
        if ext in ('.nsz', '.xcz'):
            # 압축 형식은 직접 파싱 거의 불가 → 외부 엔진 직행
            contents, nacp_names = [], {}
        else:
            try:
                contents, nacp_names = scan_nsp_contents(
                    path, log_cb=log_cb)
            except Exception:
                contents, nacp_names = [], {}

        need_engine = (
            not contents
            or not any(c.get('source') == 'header_cnmt'
                       for c in contents)
            or (not nacp_names.get('ko') and not nacp_names.get('en')
                and ext not in ('.xci', '.xcz'))
        )

        if contents and not need_engine:
            if log_cb:
                for item in contents:
                    _log_extraction(item, log_cb)
        else:
            if log_cb:
                log_cb('   ℹ 심층 분석 시작')

            result = scan_nstool(path, nstool, log_cb=log_cb)
            engine_contents = (result[0] if isinstance(result, tuple)
                               else result)
            engine_nacp     = (result[1] if isinstance(result, tuple)
                               else {})
            if engine_contents:
                contents = engine_contents
            for lang, name in engine_nacp.items():
                if name and not nacp_names.get(lang):
                    nacp_names[lang] = name

        # NACP 보조 추출 (XCI/XCZ는 위에서 처리됨)
        if ext not in ('.xci', '.xcz'):
            try:
                hactool_path = _find_hactool()
                if hactool_path and _find_prod_keys():
                    if log_cb:
                        log_cb('   ▶ 국가별 타이틀 추출 시도')
                    hactool_nacp = _extract_nacp_with_hactool(
                        hactool_path, path, log_cb=log_cb) or {}
                    for lang, name in hactool_nacp.items():
                        if name:
                            nacp_names[lang] = name
            except Exception as e:
                if log_cb:
                    log_cb(f'   ⚠ 국가별 이름 추출 실패: {e}')

        # 멀티 게임 XCI에서 update/dlc만 검출되면 base 항목 유도 추가
        # (라이브러리는 base 콘텐츠 기준으로 필터링하므로 필수)
        valid_tids = [c['title_id'] for c in contents
                      if c.get('title_id')
                      and c['title_id'].startswith('01')]
        if (valid_tids
                and not any(c.get('content_type') == 'base'
                            for c in contents)):
            if ext in ('.xci', '.xcz'):
                derived_tid = valid_tids[0][:13] + '000'
                if not any(c.get('title_id') == derived_tid
                           for c in contents):
                    contents.append({
                        'title_id':     derived_tid,
                        'version':      0,
                        'content_type': 'base',
                        'source':       'derived_base',
                    })

        if not contents or all(not c.get('title_id') for c in contents):
            contents = [{
                'title_id':     None,
                'version':      0,
                'content_type': ('base' if ext in ('.xci', '.xcz')
                                 else 'unknown'),
                'source':       'none',
            }]

        if log_cb:
            for item in contents:
                _log_extraction(item, log_cb, prefix='추출 결과:')

    entry = {
        'path':        path,
        'filename':    fp.name,
        'size':        size,
        'mtime':       mtime,
        'fast_hash':   fh,
        'file_type':   ext.lstrip('.'),
        'last_seen':   datetime.now().isoformat(),
        'scan_folder': str(fp.parent),
        'nacp_ko':     nacp_names.get('ko', ''),
        'nacp_en':     nacp_names.get('en', ''),
        'nacp_ja':     nacp_names.get('ja', ''),
        'contents':    contents,
    }
    cache.put(path, entry)
    return {**entry, 'cache_hit': False}


# ── 디렉터리 스캔 ─────────────────────────────────────────────────────
def _collect_files(folder, ignore_dirs=None):
    """os.scandir 기반 빠른 재귀 수집 (os.walk보다 30~50% 빠름)."""
    if not os.path.exists(folder):
        return []

    if ignore_dirs is None:
        ignore_dirs = {
            'saves', 'save', 'mods', 'mod', 'atmosphere', 'sxos',
            'hekate', 'bootloader', '.git', 'node_modules',
            '__pycache__', 'temp', 'tmp',
        }

    found_files = []
    stack = [folder]

    while stack:
        current = stack.pop()
        try:
            with os.scandir(current) as it:
                for entry in it:
                    if entry.is_dir(follow_symlinks=False):
                        if entry.name.lower() not in ignore_dirs:
                            stack.append(entry.path)
                    elif entry.is_file(follow_symlinks=False):
                        if entry.name.lower().endswith(
                                ('.nsp', '.nsz', '.xci', '.xcz')):
                            found_files.append(Path(entry.path))
        except (PermissionError, FileNotFoundError, OSError):
            continue

    return sorted(found_files, key=lambda p: str(p).lower())


def scan_directory(folder, callback=None, nstool=None, log_cb=None,
                   known_files=None, incremental=False,
                   pre_collected_files=None):
    """
    폴더 재귀 스캔.

    known_files       : db.get_known_files() 결과. None이면 전체 스캔.
    incremental       : True면 mtime/hash 변화 없는 파일은 건너뜀.
    pre_collected_files : 이미 수집된 파일 리스트 (중복 수집 방지).

    반환: (results, summary, skipped_paths)
    """
    if pre_collected_files is not None:
        files = pre_collected_files
    else:
        root = Path(folder)
        if not root.exists():
            return ([],
                    {'total': 0, 'cached': 0, 'new': 0,
                     'updated': 0, 'skipped': 0},
                    [])
        files = _collect_files(folder)

    if not files:
        return ([],
                {'total': 0, 'cached': 0, 'new': 0,
                 'updated': 0, 'skipped': 0},
                [])

    mgr            = CacheManager(folder)
    results        = []
    cached_results = []
    uncached_files = []
    skipped_paths  = []

    for fp in files:
        try:
            p     = str(fp)
            stat  = fp.stat()
            mtime = stat.st_mtime
            size  = stat.st_size

            # 빠른 스캔 — mtime이 같으면 완전 스킵
            if incremental and known_files:
                kf = known_files.get(p)
                if kf and abs(kf['mtime'] - mtime) < 1.0:
                    skipped_paths.append(p)
                    continue

            cached = mgr.get(p, mtime, size)
            if cached:
                cached_results.append(
                    (p, {**cached, 'cache_hit': True}))
                continue

            fh = compute_fast_hash(p)
            cached = mgr.get_by_hash(p, fh)
            if cached:
                mgr.touch_mtime(p, mtime)
                cached_results.append(
                    (p, {**cached, 'mtime': mtime, 'cache_hit': True}))
                continue

            uncached_files.append(fp)
        except Exception as e:
            if log_cb:
                log_cb(f'❌ 오류: {fp.name}: {e}')

    if log_cb and cached_results:
        log_cb(f'   ⚡ 캐시 히트: {len(cached_results)}개')
    if log_cb and skipped_paths:
        log_cb(f'   ⏭ 증분 스킵: {len(skipped_paths)}개 (변경 없음)')

    total        = len(files)
    done         = 0
    is_cancelled = False

    # 1) 캐시 히트 처리
    for p, info in cached_results:
        info.pop('cache_hit', False)
        results.append(info)
        done += 1
        if callback and callback(done, total, p, True) is False:
            is_cancelled = True
            break

    # 2) 미캐시 파일 병렬 분석
    if uncached_files and not is_cancelled:
        # 스레드 수 결정 — 사용자 설정 우선, 없으면 자동 계산
        try:
            import nsl_database as _db
            user_setting = int(
                _db.get_setting('max_scan_threads', '0'))
        except Exception:
            user_setting = 0

        cpu     = os.cpu_count() or 8
        has_xci = any(fp.suffix.lower() in ('.xci', '.xcz')
                      for fp in uncached_files)

        if user_setting > 0:
            max_workers = min(20, max(2, user_setting))
        else:
            # 자동 — XCI 많으면 보수적
            if has_xci:
                max_workers = min(10, max(4, cpu))
            else:
                max_workers = min(16, max(6, int(cpu * 1.8)))

        _lock = threading.Lock()
        with ThreadPoolExecutor(max_workers=max_workers) as pool:
            future_map = {
                pool.submit(scan_file, str(fp), mgr, nstool, log_cb): fp
                for fp in uncached_files
            }
            for future in as_completed(future_map):
                fp = future_map[future]
                try:
                    info = future.result()
                    info.pop('cache_hit', False)
                    with _lock:
                        results.append(info)
                        done += 1
                    if (callback
                            and callback(done, total, str(fp), False)
                            is False):
                        is_cancelled = True
                        for f in future_map:
                            f.cancel()
                        break
                except Exception as e:
                    with _lock:
                        done += 1
                    if log_cb:
                        log_cb(f'❌ 오류: {fp.name}: {e}')
                    if (callback
                            and callback(done, total, str(fp), False)
                            is False):
                        is_cancelled = True
                        for f in future_map:
                            f.cancel()
                        break

    if not is_cancelled:
        mgr.cleanup({r['path'] for r in results})
    mgr.save()

    summary = {
        'total':   total,
        'cached':  len(cached_results),
        'new':     len([r for r in results
                        if r['path'] not in (known_files or {})]),
        'updated': len([r for r in results
                        if (known_files or {}).get(r['path'])
                        and abs((known_files or {})
                                .get(r['path'], {}).get('mtime', 0)
                                - r['mtime']) >= 1.0]),
        'skipped': len(skipped_paths),
    }
    return results, summary, skipped_paths


def clear_cache(folder):
    """폴더 캐시 파일 삭제."""
    try:
        cf = CacheManager(folder)._file
        if os.path.exists(cf):
            os.remove(cf)
        return True
    except Exception:
        return False
