# NSL 인수인계 (Rust 포팅)

작성일: 2026-11-18
대상: `D:\NSW.dev\NSL` (Rust/egui 데스크톱 앱)
현재 버전: **v2.0.0**
최종 커밋: `80769f1`

---

## 1. 한눈에 보는 현재 상태

Python 2.0.0 앱(PySide6)을 Rust/egui로 포팅 완료. 기능 구현과 릴리스 패키징까지 끝났고, 실제 게임 코퍼스(846파일)로 E2E 검증을 마쳤다.

| 항목 | 상태 |
|---|---|
| 코드 규모 | Rust 8,400줄 (12개 모듈) |
| 빌드 | `cargo build --release` 성공, 경고 **0** |
| 린트 | `cargo clippy --all-targets` 경고 **0** |
| 테스트 | 24 통과, 3 ignored(코퍼스 필요) |
| E2E 스캔 | 846파일 / base 800 / update 710 / DLC 3,137 / 실패 0 |
| 배포물 | `dist-rust/nsl.exe` (12.2MB), `dist-rust/nsl-rust-win64.zip` (53.5MB) |
| git | 저장소 초기화 완료, 워킹트리 clean |

### 빌드 & 실행

```powershell
cd D:\NSW.dev\NSL
cargo run                      # 개발 실행 (디버그: 소스 폴더의 DB 사용)
build-rust.bat                 # 릴리스 + dist-rust/ + zip 생성
```

헤드리스 스캔 (검증/자동화용):

```powershell
dist-rust\nsl.exe --scan full   # 전체
dist-rust\nsl.exe --scan fast   # 증분 (캐시 재사용)
```

---

## 2. 이번 작업에서 고친 것 (중요)

### 2-1. 치명적 버그: 타이틀 ID 대량 유실

**증상**: 846파일 중 495개(59%)가 타이틀 ID를 못 얻음. E2E 검수에서 발견.

**원인**: `scanner.rs`의 `hactool_nsp_contents`가 CNMT NCA를 `1 MiB 이상`만 처리하도록 필터링. 실제 CNMT NCA는 **3.5 KB ~ 36 KB**.

**수정**: `is_cnmt_candidate_size()` 도입, `1 KiB ~ 64 MiB`로 확장. 회귀 테스트로 고정.

**결과**: base 217 → **800** (기존 라이브러리와 완전 일치).

### 2-2. 데이터 손실 방지: 스캔 스코프 제한

**증상**: 외장디스크 미연결 상태로 스캔하면 해당 드라이브 기록 전부 삭제.

**수정**: `delete_missing_files_within(paths, roots)` 도입. **실제 접근 가능했던 폴더 하위만** 정리. 접근 불가 폴더는 로그만 남기고 기록 유지.

**검증**: 오프라인 `Z:/OfflineDrive` 추가 후 전체 스캔 → 846건 보존.

### 2-3. 카탈로그 패치 버전 유실

**증상**: `titles.json`의 패치 엔트리(`...0800`) 13,553개가 이름이 비어 있어 전부 버려짐 → 최신 버전 부정확.

**수정**: 이름 없는 행도 **버전 소스로 사용**. `base_title_id()`로 base에 접어 최신값 채택.

**결과**: `title_db` 버전 보유 16,378 → **16,573**, 최신 버전 개선 **195개**, 퇴행 0건. 미탐지 업데이트 92 → 94 (2건 신규 발견).

### 2-4. 버전 표기 (사용자 요청으로 최종 확정)

**최종 표기**: `v524288` — Nintendo 업데이트 번호를 **그대로** 표시.

```rust
pub fn version_to_str(version: Option<i64>) -> String {
    match version {
        None => "-".into(),
        Some(v) => format!("v{v}"),
    }
}
```

**주의**: 이 필드는 Nintendo의 `N << 16` 업데이트 ordinal이지 게임 버전이 아니다.
`65536`은 "1번째 업데이트"이며, 비트 디코딩하면 존재하지 않는 `v0.0.1`이 된다.
**과거에 `v0.0.5` 형태로 표시했으나 사용자 요청으로 raw 값 표기(`v524288`)로 확정.**

> 이력: 한때 NACP `0x3060`의 실제 버전 문자열(`1.10`)을 읽어 `v1.10 · 업데이트 5`로 표시하는 구현을 넣었으나, 사용자가 기존 표기를 원해 **모두 제거**했다. 관련 컬럼(`game_files.nacp_version`)과 파싱 코드(`nacp_display_version`, `hactool_nacp_version`)도 삭제됨.

### 2-5. 캐시 무효화 메커니즘

**증상**: 스캔이 캐시 히트 시 레코드를 DB에 다시 쓰지 않아, 새 컬럼이 영원히 빈 값으로 남음.

**수정**: `scan_schema` 설정값 도입.

```rust
const SCAN_SCHEMA_VERSION: &str = "2";
```

- `config.json`의 `scan_schema` ≠ 현재 값이면 → **1회 강제 재분석**
- 완료 후 `scan_schema` 저장 → 이후 캐시 신뢰

**측정**: 강제 재분석 96.9초 → 다음 스캔 0.3초.

**앞으로**: 스캔이 새 메타데이터를 만들면 `SCAN_SCHEMA_VERSION`을 올릴 것.

### 2-6. 릴리스 패키지 안전성

**문제**: 기존 `build-rust.bat`이 `dist-rust` 내용을 그대로 압축 → 개발자 DB/설정이 섞일 위험.

**수정**: `package-rust.ps1` 재작성.
- 깨끗한 스테이징(`build/rust-package`)에서 조립
- **빈 스키마 DB** 자동 생성 (`sqlite3` 필요)
- `prod.keys`, `title.keys`, `config.json` 포함 시 **빌드 중단**
- zip 내용물 검증 후 경고

### 2-7. 기타 수정

| 항목 | 내용 |
|---|---|
| 아이콘 성능 | 무제한 텍스처 + 매 프레임 재시도 → LRU(300개) + 실패 캐시 + 프레임당 32개 제한. 유휴 CPU **0%** |
| 압축 해제 워크스페이스 | `workspaces/NSL_Workspace_*` 무한 누적 → 시작/실행 전 자동 정리 |
| XCI base 유도 | 다중 게임 XCI가 update/DLC만 노출 시 base TID 유도 (Python 동등성) |
| mojibake | `main.rs`의 깨진 문자열 1건 수정 |
| 리소스 미터 | 상태바 RAM/CPU 표시 추가 (Python 동등) |
| 창 아이콘 | `assets/icon.ico` 사용 |
| clippy | 경고 7 → 0 |
| git | `.gitignore` 추가 (키/DB/EXE/캐시 제외) |

---

## 3. 코드 구조

```
src/
  main.rs       (5,300줄)  UI + 앱 상태 + 스캔 오케스트레이션
  scanner.rs    (1,030줄)  파일 파싱, 해시, NSP/XCI/CNMT/NACP
  database.rs     (830줄)  SQLite 스키마 + 쿼리
  xci.rs          (476줄)  XCI/XCZ 파티션 파싱
  titledb.rs      (400줄)  타이틀 DB 로드/병합/다운로드
  theme.rs        (281줄)  테마 14종
  updater.rs      (229줄)  앱 자체 업데이트
  decompress.rs   (130줄)  NSZ/XCZ 변환
  media.rs        (124줄)  아이콘/스크린샷 다운로드
  translate.rs    (123줄)  설명 번역
  common.rs        (70줄)  경로/포맷 유틸
  library.rs       (11줄)  검색 필터
```

### main.rs가 큰 이유

UI(egui) 코드가 단일 `impl NslApp`에 몰려 있다. 분리하려면 `settings`, `detail`, `library` 모듈로 쪼개면 되지만, egui 상태 공유 때문에 리스크가 있다. **동작에는 문제 없음.**

---

## 4. 데이터 흐름

### 스캔

```
start_scan
  → 폴더 순회 (walkdir, 무시 폴더 제외)
  → 캐시 조회: known_files + cached_file_records
  → 캐시 히트 조건: fast_hash 존재 && mtime/size 일치 && !force_reanalyze
  → 미스: fast_hash(MD5) → scan_file_hashed
       ├ container_contents (PFS0/HFS0 헤더, ticket, CNMT, XML)
       ├ container_nacp (직접 NACP)
       ├ nstool_scan (폴백)
       ├ hactool_nsp_contents (CNMT 폴백, **1KiB 하한**)
       └ hactool_nacp_names (NACP 폴백)
  → rayon 병렬 처리
  → db.upsert_files
  → db.delete_missing_files_within(existing, scanned_roots)
  → scan_schema 저장
```

### 라이브러리 표시

```
db.files(name_mode)
  → base_tid()로 그룹화
  → base/update/DLC 집계
  → title_db 조인 (이름, 최신 버전)
  ��� GameFile 생성
```

### 버전 표시

| 컬럼 | 값 | 표시 |
|---|---|---|
| 보유 버전 | update_version (없으면 version) | `v524288` |
| 최신 버전 | title_db.version | `v655360` 또는 `-` |
| 상태 | 위 둘 비교 | 최신 / 업데이트 가능 / 미확인 |

---

## 5. 검증 방법

### 표준 테스트

```powershell
cargo test
```

커버리지:
- PFS0/CNMT/NACP 합성 파싱
- **CNMT 크기 창** (1MiB 버그 회귀 방지)
- XCI base 유도
- 버전 표기 (`v524288`)
- 패치 엔트리 병합
- DB 그룹화/DLC/스코프 프루닝
- **헤드리스 egui 실행**: 라이브러리 로드, 그리드/리스트, 설정 6카테고리, 상세 4탭, 검색, 필터

### 코퍼스 테스트 (수동)

```powershell
# 실제 파일의 해시/TID/버전이 DB와 일치하는지
cargo test scans_small_real_nsp_and_nsz_against_library_records -- --ignored

# 전체 스캔이 기준선(800/710/3137)과 일치하는지
cargo test full_scan_of_real_corpus_matches_existing_library -- --ignored

# 증분 스캔 속도/정합성
cargo test incremental_scan_reuses_cache_after_full_scan -- --ignored
```

### 수동 스모크

```powershell
build-rust.bat
# 빈 폴더에 zip 해제 → nsl.exe 실행 → 첫 실행 화면 확인
# prod.keys 복사 후 --scan full 로 기준선 확인
```

---

## 6. 알려진 제약

### 6-1. 앱 자체 업데이트가 동작하지 않음

```
릴리스 API: https://api.github.com/repos/UzuCore/NSL/releases/latest
  → tag 1.4.8 (2026-07-26), 에셋 nsl.exe 하나뿐
```

**문제 3가지**:
1. `Cargo.toml` 버전이 `2.0.0` → `is_newer("1.4.8","2.0.0")` = false → "최신입니다"
2. 릴리스에 `nsl-rust-win64.zip` 없음
3. v2.0.0 바이너리가 어디에도 없음

**해결**:
```powershell
build-rust.bat
gh release create v2.0.0 dist-rust/nsl-rust-win64.zip `
  --title "NSL v2.0.0 (Rust)" --notes "..."
```

**주의**: `updater.rs`의 매니페스트 검증이 `titledb/titles.json`을 **필수**로 요구한다. 제목 DB를 빼고 패키징하면 업데이터가 거부한다.

### 6-2. XCI/XCZ 실파일 미검증

로컬 코퍼스가 NSP(48)/NSZ(798)뿐. XCI/XCZ 코드 경로는 합성 테스트만 존재. 실물 확보 시 `--scan full`로 확인 권장.

### 6-3. 버전 값의 의미

`v524288`은 게임 버전이 아니라 **Nintendo 업데이트 번호**다. 사용자가 "게임 버전"으로 오해할 수 있으나, 원래 앱부터의 관례를 따른 것이다.

실제 게임 버전이 필요하면 NACP `0x3060`을 읽어야 한다(구현했다가 제거함, 커밋 `e75905f` 참조).

---

## 7. 라이선스 주의

NSL에는 LICENSE 파일이 아직 없다. 배포 전에 추가할 것.

외부 프로젝트의 코드를 참조할 때는 그 프로젝트의 라이선스를 먼저 확인한다. GPL 계열 코드를 이식하면 NSL 전체가 같은 라이선스로 공개되어야 한다.

---

## 8. 환경 메모

| 항목 | 값 |
|---|---|
| 코퍼스 | `E:\NSW` (846파일, 4.0TB) |
| prod.keys | `D:\NSW.dev\NSL\prod.keys` |
| 스캔 도구 | `D:\NSW.dev\NSL\bin\{nstool,hactool}.exe` |
| 타이틀 DB | `D:\NSW.dev\NSL\titledb\*.json` (titles 165MB, KR 56MB, versions 1.8MB) |
| 공개 릴리스 저장소 | `UzuCore/NSL` |

### base_dir 특례 (주의)

`common::base_dir()`는 `dist-rust` 폴더에서 실행하면 **상위 폴더**의 `library.db`/`config.json`을 사용한다. 개발 편의용이며, 배포 환경(빈 폴더)에서는 정상적으로 실행 폴더를 쓴다.

**이 때문에**: `dist-rust\nsl.exe`로 테스트하면 `D:\NSW.dev\NSL\library.db`가 바뀐다. 실제 배포 검증은 **빈 폴더에 zip 해제**해서 할 것.

---

## 9. 다음 작업 후보

| 순위 | 작업 | 비고 |
|---|---|---|
| P0 | GitHub 릴리스에 Rust zip 업로드 | 업데이터 활성화의 전제 |
| P1 | XCI/XCZ 실물 검증 | 코퍼스 확보 필요 |
| P2 | `main.rs` 모듈 분리 | 5,300줄, 리스크 있음 |
| P3 | 폴더 감시 자동 임포트 | 다운로드 폴더 자동 스캔 |
| P4 | 무결성 검증 | CNMT/NCA SHA-256 대조 |
| P5 | LICENSE 파일 추가 | 배포 전 필수 |

---

## 10. 문제 발생 시 확인 순서

1. **스캔 결과가 이상할 때**
   ```
   dist-rust\nsl.exe --scan full
   ```
   `failures=` 값 확인. 0이 아니면 로그창(`로그` 패널) 확인.

2. **버전이 안 채워질 때**
   `config.json`의 `scan_schema` 확인. `SCAN_SCHEMA_VERSION`과 다르면 자동 재분석된다.

3. **타이틀 ID가 비어 있을 때**
   `prod.keys` 위치 확인 (`base_dir` 또는 `%USERPROFILE%\.switch`). `bin/nstool.exe`, `bin/hactool.exe` 존재 확인.

4. **DB가 이상할 때**
   `library.db` 백업 후 삭제 → 재스캔. (게임 파일은 건드리지 않음)

---

## 11. 커밋 이력

```
80769f1 fix: restore raw v<N> version labels and drop unused NACP version column
e75905f feat: use real NACP versions and keep patch-catalog versions
ea72e96 feat: headless scan mode, first-launch title db match, release docs
408fd37 fix: recover title IDs for encrypted CNMT packages and harden release
01f3666 chore: snapshot before release hardening
```

새로 시작할 때는 `80769f1`에서 브랜치를 따면 된다.
