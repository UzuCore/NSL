# nsl_decompress.py — NSZ/XCZ 압축 해제 (DecompressMixin) (PySide6)
import os
import platform
import re
import shutil
import subprocess
import tempfile
import threading
import time

from PySide6.QtWidgets import QApplication, QMessageBox


class DecompressMixin:

    def _decompress_file(self, src_path: str, kind: str) -> str | None:
        """
        NSZ → NSP, XCZ → XCI 변환 후 변환된 파일 경로 반환.
        실패하거나 취소 시 None 반환.

        kind            : 'nsz' 또는 'xcz'
        고정 출력 폴더  : C:\\Users\\Public\\NSL_Workspace (Windows)
                          또는 $TMPDIR/NSL_Workspace (기타)
                          (매번 실행 전 자동으로 비움)
        진행률 표시     : 외부 도구 stdout을 실시간 읽어 % 파싱
        """
        # OS별 워크스페이스 경로
        if platform.system() == 'Windows':
            NSL_WORKSPACE = r'C:\Users\Public\NSL_Workspace'
        else:
            NSL_WORKSPACE = os.path.join(
                tempfile.gettempdir(), 'NSL_Workspace')

        # ── 외부 도구가 참조하는 ~/.switch/ 에 키 파일 동기화 ────────
        # 도구는 기본적으로 ~/.switch/prod.keys, ~/.switch/title.keys
        # 를 탐색. 사용자 홈이 한글 경로여도 도구가 직접 읽으므로
        # 복사 자체는 안전 — "Could not load keys file." 방지용.
        try:
            switch_dir = os.path.join(os.path.expanduser('~'), '.switch')
            os.makedirs(switch_dir, exist_ok=True)
            for key_name in ('prod.keys', 'title.keys'):
                src_key = self.base_dir / key_name
                dst_key = os.path.join(switch_dir, key_name)
                if src_key.is_file():
                    if (not os.path.exists(dst_key)
                            or os.path.getmtime(src_key) > os.path.getmtime(dst_key)):
                        shutil.copy2(str(src_key), dst_key)
        except Exception:
            pass  # 동기화 실패해도 계속 진행 (nsz 가 자체 탐색)

        # 워크스페이스 폴더 비우기 (없으면 생성)
        if os.path.exists(NSL_WORKSPACE):
            for entry in os.scandir(NSL_WORKSPACE):
                try:
                    if entry.is_dir(follow_symlinks=False):
                        shutil.rmtree(entry.path, ignore_errors=True)
                    else:
                        os.remove(entry.path)
                except Exception:
                    pass
        else:
            os.makedirs(NSL_WORKSPACE, exist_ok=True)

        # 압축 해제 도구 확인
        nsz_cmd = shutil.which('nsz')
        if not nsz_cmd:
            QMessageBox.warning(
                self, '압축 해제 도구 없음',
                '압축 해제 도구가 설치되어 있지 않습니다.\n'
                '관리자에게 문의하시거나 도구를 설치해 주세요.')
            return None

        cmd = [nsz_cmd, '-D', '-w', '-o', NSL_WORKSPACE, src_path]
        self._set_status(f'압축 해제 중: {os.path.basename(src_path)} …')
        QApplication.processEvents()

        # Windows 콘솔 창 숨김
        si = None
        if platform.system() == 'Windows':
            si = subprocess.STARTUPINFO()
            si.dwFlags |= subprocess.STARTF_USESHOWWINDOW

        pct_re = re.compile(r'(\d{1,3}(?:\.\d+)?)\s*%')

        try:
            proc = subprocess.Popen(
                cmd,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                text=True,
                encoding='utf-8',
                errors='replace',
                startupinfo=si,
            )
        except Exception as e:
            QMessageBox.critical(self, '실행 오류', f'도구 실행 중 오류 발생:\n{e}')
            return None

        # stdout/stderr 동시 수집 (데드락 방지) + 진행률 파싱
        stdout_lines:    list[str] = []
        stderr_lines_out: list[str] = []

        def _read_stdout():
            try:
                for line in iter(proc.stdout.readline, ''):
                    line = line.rstrip()
                    if line:
                        stdout_lines.append(line)
            except Exception:
                pass

        def _read_stderr():
            try:
                for line in iter(proc.stderr.readline, ''):
                    line = line.rstrip()
                    if line:
                        stderr_lines_out.append(line)
            except Exception:
                pass

        t_out = threading.Thread(target=_read_stdout, daemon=True)
        t_err = threading.Thread(target=_read_stderr, daemon=True)
        t_out.start()
        t_err.start()

        # UI 갱신 루프 (스레드가 끝날 때까지)
        while t_out.is_alive() or t_err.is_alive():
            for line in stdout_lines[-5:]:
                m = pct_re.findall(line)
                if m:
                    try:
                        pct = min(99, int(float(m[-1])))
                        self._set_status(
                            f'압축 해제 중: {os.path.basename(src_path)} … {pct}%')
                    except ValueError:
                        pass
            QApplication.processEvents()
            time.sleep(0.05)

        t_out.join()
        t_err.join()
        proc.wait()

        stdout_out = '\n'.join(stdout_lines)
        stderr_out = '\n'.join(stderr_lines_out)

        if proc.returncode != 0:
            QMessageBox.critical(
                self, '압축 해제 실패',
                f'반환 코드: {proc.returncode}\n\n'
                f'[stderr]\n{stderr_out[:600]}\n\n'
                f'[stdout]\n{stdout_out[:400]}')
            return None

        # 변환된 파일 경로 탐색 (NSZ→NSP, XCZ→XCI)
        base     = os.path.splitext(os.path.basename(src_path))[0]
        new_ext  = '.nsp' if kind == 'nsz' else '.xci'
        expected = os.path.join(NSL_WORKSPACE, base + new_ext)
        if os.path.exists(expected):
            self._set_status(f'압축 해제 완료: {os.path.basename(expected)}')
            return expected

        # 폴백: 워크스페이스 전체를 재귀 탐색 (서브디렉토리 포함)
        found_files = [
            os.path.join(dirpath, fname)
            for dirpath, _dirs, filenames in os.walk(NSL_WORKSPACE)
            for fname in filenames
            if fname.lower().endswith(new_ext)
        ]

        if len(found_files) == 1:
            self._set_status(f'압축 해제 완료: {os.path.basename(found_files[0])}')
            return found_files[0]
        elif len(found_files) > 1:
            # 여러 파일이 있을 경우 가장 최근에 수정된 파일 선택
            found_files.sort(key=os.path.getmtime, reverse=True)
            self._set_status(f'압축 해제 완료: {os.path.basename(found_files[0])}')
            return found_files[0]

        QMessageBox.warning(
            self, '파일을 찾을 수 없음',
            f'압축 해제는 성공했으나 출력 파일을 찾지 못했습니다.\n'
            f'예상 경로: {expected}\n\n'
            f'[stderr]\n{stderr_out[:400]}')
        return None
