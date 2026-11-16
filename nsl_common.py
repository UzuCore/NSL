# nsl_common.py — 공통 유틸리티 (PySide6)
# 모든 모듈이 공유하는 헬퍼: 기본 경로, SSL 컨텍스트, 버전/크기 포매팅.
import os
import sys
import ssl


# ── 기본 경로 ──────────────────────────────────────────────────────────
def get_base_path():
    """애플리케이션 루트 디렉터리. 동결 빌드와 스크립트 실행 모두 지원."""
    if getattr(sys, 'frozen', False) and hasattr(sys, '_MEIPASS'):
        return os.path.dirname(sys.executable)
    try:
        return os.path.dirname(os.path.abspath(__file__))
    except NameError:
        return os.path.abspath('.')


# ── SSL 컨텍스트 ───────────────────────────────────────────────────────
# 일부 미러는 인증서 체인이 불완전하여 검증을 끄고 사용한다.
_SSL_CTX = ssl.create_default_context()
_SSL_CTX.check_hostname = False
_SSL_CTX.verify_mode = ssl.CERT_NONE


def get_ssl_context():
    return _SSL_CTX


# ── 포매팅 ─────────────────────────────────────────────────────────────
def version_to_str(version):
    """버전 정수를 표시용 문자열로. None/0 등 특수값도 처리."""
    if version is None:
        return '-'
    v = int(version)
    if v == 0:
        return 'v1.0.0'
    maj = (v >> 26) & 0x3F
    minor = (v >> 20) & 0x3F
    patch = (v >> 16) & 0xF
    if maj or minor or patch:
        return f'v{maj}.{minor}.{patch}'
    if v >= 65536:
        return f'v0.0.0+{v}'
    return f'v? ({v})'


def format_size(n):
    """바이트 수를 사람이 읽기 쉬운 단위로."""
    if n is None:
        return '-'
    n = float(n)
    for u in ('B', 'KB', 'MB', 'GB', 'TB'):
        if n < 1024:
            return f'{n:.1f} {u}'
        n /= 1024
    return f'{n:.1f} PB'
