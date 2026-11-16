# nsl_scan.py — 스캔 + 타이틀DB + 업데이트 체크 (ScanMixin) (PySide6)
import json
import os
import shutil
import subprocess
import sys
import urllib.request
import zipfile
from pathlib import Path

from PySide6.QtWidgets import QMessageBox

import nsl_database as db
import nsl_scanner as scanner
import nsl_titledb as titledb_module
from nsl_common import get_ssl_context
from nsl_theme import restyle as _restyle

_SSL_CTX = get_ssl_context()


class ScanMixin:

    # ── 스캔 토글/상태 ────────────────────────────────────────────────
    def _toggle_scan(self):
        if self._scanning or self._icon_extracting:
            self.btn_scan.setText('⏳ 중지 중...')
            self.btn_scan.setEnabled(False)
            self._log(
                '\n[시스템] 스캔 중지를 요청했습니다 — '
                '진행 중인 파일까지만 완료됩니다.\n', 'warn')
            self._stop_scan()
        else:
            self.btn_scan.setText('스캔 중지')
            _restyle(self.btn_scan, 'Red')
            self._incremental_scan()

    def _reset_scan_btn(self):
        self._scanning = False
        self._icon_extracting = False
        if hasattr(self, 'btn_scan'):
            self.btn_scan.setText('빠른 스캔')
            self.btn_scan.setEnabled(True)
            _restyle(self.btn_scan, 'Blue')

    def _incremental_scan(self):
        self._do_scan(incremental=True)

    def _scan_all(self):
        self._do_scan(incremental=False)

    # ── 스캔 실행 ─────────────────────────────────────────────────────
    def _do_scan(self, incremental: bool = False):
        engine    = scanner.find_nstool()
        keys_path = self.base_dir / 'prod.keys'
        if not keys_path.is_file():
            QMessageBox.critical(
                self, '키 파일 없음',
                '키 파일(prod.keys)을 찾을 수 없습니다.\n\n'
                '게임 분석을 위해 프로그램과 같은 폴더에 '
                'prod.keys 파일을 넣어주세요.')
            self._reset_scan_btn()
            return

        if self._scanning:
            QMessageBox.information(
                self, '진행 중', '이미 스캔이 진행 중입니다.')
            return
        folders = db.get_scan_folders()
        if not folders:
            QMessageBox.warning(
                self, '폴더 없음', '먼저 스캔할 폴더를 추가해 주세요.')
            self._reset_scan_btn()
            return

        self._scanning    = True
        self._scan_hits   = 0
        self._scan_misses = 0
        self._set_progress(0, 100)
        mode_str = '빠른 스캔' if incremental else '전체 스캔'
        self._set_status(f'{mode_str} 준비 중…')
        self._log_clear()
        self._log(f'🚀 {mode_str} 시작', 'info')

        def worker():
            found   = []
            _hits   = [0]
            _misses = [0]
            known   = db.get_known_files() if incremental else {}

            self._invoke_main(lambda:
                self._set_status(f'{mode_str} — 파일 목록 수집 중…'))
            all_folder_files = {}
            for folder in folders:
                try:
                    all_folder_files[folder] = scanner._collect_files(folder)
                except Exception:
                    all_folder_files[folder] = []

            grand_total = sum(len(v) for v in all_folder_files.values())
            global_done = [0]
            _last_ui    = [0]

            self._log(
                f'📋 전체 파일 수: {grand_total:,}개 '
                f'({len(folders)}개 폴더)', 'info')

            for folder in folders:
                if not self._scanning:
                    break
                folder_files = all_folder_files.get(folder, [])
                self._log(
                    f'\n📂 폴더: {folder}  ({len(folder_files):,}개)',
                    'info')

                offset = global_done[0]

                def cb(cur, tot, fp, hit=False, _off=offset):
                    if not self._scanning:
                        return False
                    if hit:
                        _hits[0] += 1
                    else:
                        _misses[0] += 1
                    g = _off + cur
                    if g - _last_ui[0] >= 10 or g == grand_total:
                        _last_ui[0] = g
                        gc, gt = g, grand_total
                        self.progress_signal.emit(gc, gt or 1)
                        self.status_signal.emit(
                            f'{mode_str}  ({gc:,}/{gt:,})  '
                            f'⚡캐시:{_hits[0]:,}  '
                            f'신규:{_misses[0]:,}')
                    return True

                try:
                    results, _, _skipped = scanner.scan_directory(
                        folder, cb, engine, log_cb=self._log,
                        known_files=known, incremental=incremental,
                        pre_collected_files=folder_files)
                    self._log(
                        f'\n💾 DB 저장 중… ({len(results):,}개 파일)',
                        'dim')
                    db.upsert_files_batch(results)
                    found.extend(r['path'] for r in results)
                    if incremental:
                        folder_path   = Path(folder).resolve()
                        scanned_paths = {r['path'] for r in results}
                        for p in known:
                            if p in scanned_paths:
                                continue
                            try:
                                if (Path(p).resolve()
                                        .is_relative_to(folder_path)):
                                    if os.path.isfile(p):
                                        found.append(p)
                            except (ValueError, OSError):
                                pass
                    self._log('✅ 저장 완료', 'ok')
                except Exception as e:
                    self._log(f'❌ 오류: {e}', 'err')
                    err = str(e)
                    self._invoke_main(lambda er=err:
                        QMessageBox.critical(self, '스캔 오류', er))

                global_done[0] += len(folder_files)

            removed = (db.delete_missing_files(found)
                       if self._scanning else 0)
            self._scan_hits   = _hits[0]
            self._scan_misses = _misses[0]
            t = len(found)
            self._invoke_main(lambda: self._scan_done(t, removed))

        self._run_in_thread(worker)

    def _scan_done(self, total: int, removed: int):
        self._scanning = False
        self._scan_just_finished = True
        self._set_progress(100, 100)
        removed_str = f'  |  🗑 {removed:,}개 제거' if removed else ''
        msg = (f'스캔 완료  —  {total:,}개 파일  |   '
               f'캐시 {self._scan_hits:,}  '
               f'신규기록 {self._scan_misses:,}{removed_str}')
        self._set_status(msg)
        self._log(f'\n🏁 {msg}', 'ok')
        self._refresh_library()

    def _stop_scan(self):
        self._scanning = False
        self._icon_extracting = False
        self._set_status('스캔 중지됨')
        self._invoke_main(self._reset_scan_btn)

    # ── 타이틀 DB ──────────────────────────────────────────────────────
    def _run_titledb(self, fetch_fn, confirm_msg: str = None):
        if confirm_msg:
            ans = QMessageBox.question(self, '확인', confirm_msg,
                                       QMessageBox.Yes | QMessageBox.No)
            if ans != QMessageBox.Yes:
                return
        self._set_status('타이틀 DB 처리 중…')
        self._set_progress(0, 100)

        def worker():
            self._titledb_running = True
            try:
                def on_prog(pct, _tot, msg):
                    self.progress_signal.emit(pct, 100)
                    self.status_signal.emit(f'[{pct:3d}%]  {msg}')

                entries = fetch_fn(on_prog)
                if not entries:
                    self._invoke_main(lambda:
                        QMessageBox.warning(
                            self, '오류', '타이틀이 없습니다.'))
                    return
                db.load_title_db_entries(entries)
                cnt = len(entries)

                def _done():
                    self._set_progress(100, 100)
                    self._set_status(f'타이틀 DB 완료  —  {cnt:,}개')
                    self._refresh_library()
                    self._update_db_btn_state()
                    QMessageBox.information(
                        self, '완료',
                        f'타이틀 DB 갱신 완료!\n총 {cnt:,}개 타이틀')

                self._invoke_main(_done)
            except Exception as e:
                err = str(e)

                def _fail():
                    QMessageBox.critical(self, '실패', err)
                    self._set_status('타이틀 DB 갱신 실패')
                self._invoke_main(_fail)
            finally:
                self._titledb_running = False

        self._run_in_thread(worker)

    def _update_titledb(self):
        self._run_titledb(
            titledb_module.download_full_titledb,
            confirm_msg=(
                '타이틀 DB를 다운로드합니다.\n\n'
                '  · 메타 데이터 (titles.json)\n'
                '  · 버전 정보 (versions.json)\n'
                '  · 한국어 이름 (KR.ko.json)\n\n'
                '계속하시겠습니까?'
            ))

    def _load_local_titledb(self):
        if not titledb_module.get_local_file_info().get(
                'titles.json', {}).get('exists'):
            QMessageBox.warning(
                self, '파일 없음',
                '타이틀 DB 파일이 없습니다.\n'
                '먼저 "타이틀 DB 다운로드"를 실행해 주세요.')
            return
        self._run_titledb(titledb_module.load_local_titledb)

    def _show_titledb_files(self):
        info  = titledb_module.get_local_file_info()
        lines = [f'저장 위치: {titledb_module.TITLEDB_DIR}\n']
        for name in ('titles.json', 'versions.json', 'KR.ko.json'):
            fi = info.get(name, {})
            if fi.get('exists'):
                lines.append(f'  📄 {name}')
                lines.append(
                    f"     크기: {scanner.format_size(fi['size'])}  |  "
                    f"수정: {fi['modified']}")
            else:
                lines.append(f'  ❌ {name}   (없음)')
            lines.append('')
        QMessageBox.information(self, '타이틀 DB 파일 상태',
                                '\n'.join(lines))

    def _clear_titledb(self):
        ans = QMessageBox.question(
            self, '확인', '타이틀 DB를 초기화하시겠습니까?',
            QMessageBox.Yes | QMessageBox.No)
        if ans == QMessageBox.Yes:
            db.clear_title_db()
            self._refresh_library()
            self._set_status('타이틀 DB 초기화됨')

    # ── 업데이트 확인 ─────────────────────────────────────────────────
    def _parse_version(self, v_str: str):
        try:
            return tuple(map(int, v_str.lower().lstrip('v').split('.')))
        except Exception:
            return (0, 0, 0)

    def _check_update(self, auto: bool = False):
        API_URL = 'https://api.github.com/repos/UzuCore/NSL/releases/latest'
        self._set_status('업데이트 확인 중...')

        def worker():
            try:
                req = urllib.request.Request(
                    API_URL, headers={'User-Agent': 'Mozilla/5.0'})
                with urllib.request.urlopen(
                        req, context=_SSL_CTX, timeout=10) as resp:
                    data = json.loads(resp.read().decode('utf-8'))

                latest  = data.get('tag_name', '').lstrip('v')
                current = self._app_version.lstrip('v')

                if self._parse_version(latest) > self._parse_version(current):
                    assets    = data.get('assets', [])
                    exe_asset = next(
                        (a for a in assets
                         if a.get('name', '').lower().endswith('.exe')),
                        None)
                    zip_asset = next(
                        (a for a in assets
                         if a.get('name', '').lower().endswith('.zip')),
                        None)
                    asset = exe_asset or zip_asset
                    if asset:
                        notes = data.get('body',
                                         '릴리즈 노트가 없습니다.')
                        url   = asset['browser_download_url']
                        kind  = 'exe' if exe_asset else 'zip'
                        self._invoke_main(lambda:
                            self._prompt_update(latest, url, notes, kind))
                    elif not auto:
                        self._invoke_main(lambda:
                            QMessageBox.information(
                                self, '업데이트',
                                f'새 버전(v{latest})이 출시되었지만 '
                                '다운로드 파일을 찾을 수 없습니다.'))
                else:
                    if not auto:
                        self._invoke_main(lambda:
                            QMessageBox.information(
                                self, '업데이트',
                                f'현재 최신 버전(v{current})을 '
                                '사용 중입니다.'))
                    self._invoke_main(lambda: self._set_status('대기'))
            except Exception as e:
                err = str(e)
                if not auto:
                    self._invoke_main(lambda:
                        QMessageBox.critical(
                            self, '업데이트 오류',
                            f'버전 확인 중 오류가 발생했습니다:\n{err}'))
                self._invoke_main(lambda: self._set_status('대기'))

        self._run_in_thread(worker)

    def _prompt_update(self, latest_version: str, download_url: str,
                       release_notes: str, kind: str = 'zip'):
        kind_str = '단일 EXE 패치' if kind == 'exe' else 'ZIP 전체 패키지'
        ans = QMessageBox.question(
            self, '업데이트 발견',
            f'새로운 버전(v{latest_version})이 출시되었습니다.  '
            f'[{kind_str}]\n\n지금 업데이트 하시겠습니까?\n\n'
            f'[릴리즈 노트]\n{release_notes}',
            QMessageBox.Yes | QMessageBox.No)
        if ans == QMessageBox.Yes:
            self._do_update(download_url, kind)
        else:
            self._set_status('대기')

    def _do_update(self, download_url: str, kind: str = 'zip'):
        self._set_status('업데이트 다운로드 중...')
        self._set_progress(0, 100)

        def worker():
            try:
                exe_path = sys.executable
                if not exe_path.lower().endswith('.exe'):
                    self._invoke_main(lambda: (
                        QMessageBox.critical(
                            self, '오류',
                            '자동 업데이트는 .exe 빌드 환경에서만 '
                            '동작합니다.'),
                        self._set_status('대기'),
                    ))
                    return

                exe_dir          = os.path.dirname(exe_path)
                current_exe_name = os.path.basename(exe_path)
                temp_dir         = os.path.join(exe_dir, 'temp')
                os.makedirs(temp_dir, exist_ok=True)

                ext     = '.exe' if kind == 'exe' else '.zip'
                dl_path = os.path.join(temp_dir, f'update{ext}')
                req = urllib.request.Request(
                    download_url, headers={'User-Agent': 'Mozilla/5.0'})
                with urllib.request.urlopen(
                        req, context=_SSL_CTX, timeout=60) as resp:
                    total      = int(resp.headers.get('Content-Length', 0))
                    downloaded = 0
                    with open(dl_path, 'wb') as out_file:
                        while True:
                            chunk = resp.read(8192)
                            if not chunk:
                                break
                            out_file.write(chunk)
                            downloaded += len(chunk)
                            if total:
                                pct = int((downloaded / total)
                                          * (50 if kind == 'zip' else 90))
                                self.progress_signal.emit(pct, 100)
                                self.status_signal.emit(
                                    f'업데이트 다운로드 중... {pct}%')

                self.progress_signal.emit(90, 100)

                if kind == 'exe':
                    update_tmp_dir = os.path.join(temp_dir, 'update_tmp')
                    os.makedirs(update_tmp_dir, exist_ok=True)
                    shutil.copy2(
                        dl_path,
                        os.path.join(update_tmp_dir, current_exe_name))
                    self.status_signal.emit('EXE 패치 준비 완료')
                else:
                    self.status_signal.emit('압축 해제 중...')
                    update_tmp_dir = os.path.join(temp_dir, 'update_tmp')
                    if os.path.exists(update_tmp_dir):
                        shutil.rmtree(update_tmp_dir)
                    os.makedirs(update_tmp_dir, exist_ok=True)
                    with zipfile.ZipFile(dl_path, 'r') as zip_ref:
                        zip_ref.extractall(update_tmp_dir)
                    extracted = os.listdir(update_tmp_dir)
                    if len(extracted) == 1:
                        single = os.path.join(update_tmp_dir, extracted[0])
                        if os.path.isdir(single):
                            for sub in os.listdir(single):
                                shutil.move(
                                    os.path.join(single, sub),
                                    os.path.join(update_tmp_dir, sub))
                            os.rmdir(single)

                self.progress_signal.emit(100, 100)

                bat_content = (
                    '@echo off\n'
                    'chcp 65001 > nul\n'
                    'title NSL 업데이트 적용 중...\n'
                    'echo 프로그램 종료를 기다리는 중...\n'
                    'timeout /t 3 /nobreak > nul\n'
                    'echo 파일 덮어쓰기 중...\n'
                    'xcopy /Y /E /H /C /I '
                    '"%~dp0temp\\update_tmp\\*" "%~dp0"\n'
                    'echo 임시 파일 정리 중...\n'
                    'rmdir /S /Q "%~dp0temp"\n'
                    'echo 프로그램 재시작 중...\n'
                    f'explorer.exe "%~dp0{current_exe_name}"\n'
                    '(goto) 2>nul & del "%~f0"\n'
                )
                bat_path = os.path.join(exe_dir, 'update.bat')
                with open(bat_path, 'w', encoding='utf-8') as f:
                    f.write(bat_content)

                def restart_and_exit():
                    self._set_status('업데이트 준비 완료')
                    QMessageBox.information(
                        self, '업데이트 준비 완료',
                        '다운로드 완료!\n\n'
                        "'확인'을 누르면 프로그램이 종료되고\n"
                        '업데이트가 적용된 후 자동으로 재시작됩니다.')
                    subprocess.Popen([bat_path],
                                     creationflags=0x00000010,
                                     cwd=exe_dir)
                    self.close()
                    os._exit(0)

                self._invoke_main(restart_and_exit)

            except Exception as e:
                err = str(e)
                self._invoke_main(lambda: (
                    QMessageBox.critical(
                        self, '업데이트 실패', f'오류:\n{err}'),
                    self._set_status('업데이트 실패'),
                ))

        self._run_in_thread(worker)
