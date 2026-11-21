# NSL Stage 17 최종 인수인계 — 2026-11-21

> **새 채팅 시작점:** `D:\NSW.dev\NSL`에서 이 문서를 먼저 확인하고 Stage 18을 이어간다.
> **최신 기능 소스 기준:** GitHub `UzuCore/NSL`, `main`, 커밋
> `eab2692241a9088ee550a149903af0baf4809caf` (Stage 17 코드 + QA).
> 이 인수인계 문서만 추가로 커밋하더라도 **실행파일 내장 버전은 계속 `eab2692`**이며 코드에 변화가 있는 것은 아니다.

## 1. 프로젝트 / 검수 원칙

- 프로젝트: **NSL**, Windows용 Nintendo Switch 게임 라이브러리·스토어 카탈로그·다운로드 대기열·업데이트/DLC 병합 프로그램(Rust).
- 소스 루트: `D:\NSW.dev\NSL`; NSRP 코어는 프로젝트 내 vendored Rust 라이브러리.
- 테스트용 격리 디스크: `E:\NSL_QA_20261120`. 게임 라이브러리 `E:\NSW`는 원본을 보존한다.
- 배포 실행파일은 `D:\NSW.dev\NSL\target\release\nsl.exe`로 빌드한다. **기존 공식 `dist` 파일은 덮어쓰지 않는다.**
- 사용자가 요구한 게임 동작 검수는 **에뮬레이터에서 타이틀/메뉴까지 로딩 성공**으로 충분하다. 플레이 완주, DLC 해금 확인, 장시간 플레이 검수는 필수로 하지 않는다.
- 다른 Eden/RetroArch/Flycast 인스턴스와 사용자 세이브·BIOS·키 파일은 건드리지 않는다. 에뮬레이터 확인이 필요하면 사용자 환경과 분리된 임시 프로필·독립 인스턴스로만 실행한다.
- 실제 외부 토렌트 QA는 권한이 확인된 배포 가능 콘텐츠로 한다. 사용자의 상용 게임 파일을 외부에 시딩하거나 제3자 배포본의 권한을 가정하지 않는다.
- 불필요한 디버그·더미 산출물은 정리하되 원본 게임, 기존 릴리스, 공식 `dist`, 복구 기록을 무작정 삭제하지 않는다.

## 2. 최신 Stage 17 — 완료된 코드 변경

**커밋:** `eab2692` — `perf(catalog): conditional ETag refresh and public licensed torrent QA`

| 파일 | 변경 내용 |
| --- | --- |
| `src/catalog.rs` | `CatalogState.etag` / `not_modified`, 카탈로그 JSON에 검증자 원자적 동시 저장, URL 일치·ETag 형식 검증, `If-None-Match` 전송, HTTP 304면 캐시를 유지하고 본문 재다운로드 및 파일 재기록 생략 |
| `src/catalog.rs` | `ureq`에서 304를 `Ok(response)` 또는 `Error::Status(304)`로 돌려주는 두 경우 모두 처리. 이전에는 빈 304 본문을 JSON으로 파싱해 실패하는 문제가 있었음 |
| `src/app/store.rs` | 카탈로그 작업 스레드가 단순 `usize` 대신 파싱이 완료된 `CatalogState`를 전달하고 `카탈로그 이미 최신` 메시지를 구분 |
| `src/app/frame.rs` | 백그라운드에서 받은 `CatalogState`를 UI로 직접 이동. 기존 UI 스레드에서 카탈로그 파일 재읽기·재파싱 제거 |
| `src/main.rs` | 비동기 카탈로그 완료 채널 자료형 변경 |
| `src/runtime_tests.rs` | 캐시 히트 상태가 디스크 재조회 없이 UI에 반영되는 회귀 테스트 |
| `src/torrent_e2e_tests.rs` | 합법적 공개 `Sintel` 파일에 대해 실제 마그넷 메타데이터 미리보기 → 선택 파일만 다운로드 → 원본 HTTPS 바이트 비교 통합 테스트 |

**실제 외부 카탈로그 수치(2026-11-21 검증 시점):**

- URL: `https://raw.githubusercontent.com/Langegen/switch-game-collection/main/EN_catalog.json`
- HTTP 200, 원본 응답 **22,926,273 bytes**.
- NSL 프로덕션 다운로드·파싱·개별 QA 캐시 저장 및 재로드 성공: **7,352개 항목**, 기본 타이틀 ID를 찾을 수 있는 항목 **7,004개**, 서로 다른 기본 타이틀 ID **6,915개**, 40자리 유효 BTv1 해시 **7,352개**.
- 캐시 저장 파일 약 **4,498,919 bytes** (ETag 포함). 후속 실제 HTTP 304 시 22.9 MB 재전송 및 캐시 파일 재기록 회피, 저장 파일 수정 시각 불변. 마지막 라이브 QA 약 **1.50초**.
- 외부 카탈로그는 변화할 수 있으므로 해당 수치는 **당시 관측값**이며 영구적인 고정 값이 아니다.

**합법적인 공개 인터넷 BitTorrent 검증:**

- 테스트 콘텐츠: WebTorrent 공개 테스트 토렌트 *Sintel* (Creative Commons BY 3.0).
- 마그넷만으로 실제 외부 피어를 통해 **11개 파일 목록 조회**.
- `Sintel.en.srt` 인덱스 1만 선택: **1,514 bytes** 다운로드. 공식 HTTPS 원본과 **바이트 단위 동일**, 완료 통지 및 실제 피어 관측.
- 최종 라이브 검수 **1 PASS / 0 FAIL**, 약 **4.42초**.
- 토렌트 메타데이터 공식 URL `https://webtorrent.io/torrents/sintel.torrent` (20,792 bytes, SHA-256 `4C8FDAD0414B4767546A0F92FE3D660A66EDC32471874E7A1CFA97120317B84A`).
- 외부 호스트 `bt.t-ru.org`, `tracker.opentrackr.org`의 DNS 및 TCP 80 연결 성공. **개별 게임 토렌트의 시더·트래커 announce 성공을 의미하지 않음.**

## 3. Stage 14~16에서 인계받은 기능

### Stage 14 — 실제 게임 로딩 기준 확보

- Suika Game 원본 NSZ + 업데이트 + 누적 **29개 DLC**의 최종 NSZ 생성.
- CNMT **31개**(Base 1, Update 1, DLC 29) 정합 확인.
- 별도 Eden 프로필에서 GPU 렌더링된 **타이틀/게임 시작 메뉴 표시**까지 실제 시각 확인. 플레이 구간 추가 검수는 사용자 요청상 불필요.
- 원본 해시·사용자 인스턴스·프로필 보존.
- 커밋 `d4ce70b`; QA 문서 `docs\qa\QA_NSL_STAGE14_SUIKA_EDEN_REAL_GAME_LOADING_2026-11-21.md`.

### Stage 15 — 대기열 복구 + 최종 병합 파일 관리

- 다운로드 실패→재개, 재시작 후 `tNN` 런타임 키 재사용 충돌 방지(출력 폴더 기준 재연결).
- 동일 토렌트의 여러 DLC를 누적 처리하되 **네이티브 병합 작업은 한 번에 하나씩** 실행.
- 같은 게임의 모든 DLC 작업 성공 후에만 복구 저널을 **먼저 원자적으로 저장**하고 이전 중간 병합 결과만 삭제. 원본 게임·다운로드 NSP·사용자 파일은 제외. 최종 파일만 유지.
- 실제 Horror Tale 2 베이스 NSZ + 업데이트 NSP의 다운로드 완료 이벤트→저널 재시작 복구→자동 병합 확인: 최종 **330,474,468 bytes NSZ**.
- 커밋 `003252a`; QA 문서 `docs\qa\QA_NSL_STAGE15_BATCH_RESUME_FINAL_OUTPUT_2026-11-21.md`.

### Stage 16 — 로컬 실제 BitTorrent 피어→NSZ 병합

- 테스트용 실제 게임 업데이트 NSP **346,119,104 bytes**를 `127.0.0.1` 독립 시더에서 실제 BitTorrent 프로토콜로 전송, 원본 NSZ와 네이티브 병합.
- 최종 **330,474,468 bytes NSZ**. 게임 및 패치 원본 SHA-256 불변, 복구 저널 보존. 실피어→병합 E2E **1 PASS**.
- **중요 버그 수정:** 진행률 100%라도 실제 완료 통지가 오고 파일 핸들이 해제되기 전에는 병합 작업을 시작하지 않는다.
- 로컬 시더 중단·복귀, 일시정지·재시작·재개, 선택 다운로드 3종 회귀 PASS. QA 테스트 엔진의 수신 주소는 `127.0.0.1:0`으로 제한.
- 커밋 `73a0180`; QA 문서 `docs\qa\QA_NSL_STAGE16_REAL_LOOPBACK_TORRENT_NATIVE_MERGE_2026-11-21.md`.

## 4. 최종 검수 상태 — Stage 17

| 검수 항목 | 최종 결과 |
| --- | --- |
| `cargo test --locked --all-targets` | **214 PASS / 0 FAIL / 42 ignored** |
| `cargo test --locked -p nsrp-core --lib` | **119 PASS / 0 FAIL / 3 ignored** |
| `cargo fmt --all -- --check` | PASS |
| `cargo clippy --locked --all-targets -- -D warnings` | PASS |
| HTTP ETag 304·URL/저널 분리·악성 ETag·UI 캐시 검수 | PASS |
| 실제 외부 카탈로그 다운로드 + 304 재검수 | 1 PASS / 0 FAIL |
| 실제 공개 피어 마그넷 미리보기 + 자막 선택 다운로드 | 1 PASS / 0 FAIL |
| `cargo build --locked --release` | PASS |
| Windows GUI 980×720 / 1280×800 / 1456×900 | **3/3 PASS** |
| 실제 기존 게임 라이브러리 및 공식 배포본 보호 | PASS |
| GitHub `main` 동기화 / 기능 코드 작업 트리 | PASS / CLEAN |

**현재 테스트 완료 실행파일:**

- `D:\NSW.dev\NSL\target\release\nsl.exe`
- 크기: **22,151,680 bytes**.
- SHA-256: `D2DFE1682483544C0AC92223FC0691C86174355BDF6932E34E77B5447F1FD314`
- 창 타이틀: `NSL  eab2692`.
- 기존 `dist\nsl.exe` SHA-256 `01DB2E1B9039C81F673A71D2C8EFB7B693736691617E462387E76EBCDC2AB423` (**변경 금지**).
- 기존 `dist\nsl-win64.zip` SHA-256 `0C5AD4F89273C7741B14066C400F42B5E890A9E8D7E115D57DC7F9946B6E689B` (**변경 금지**).
- 테스트 후 `stage17-*` 개인 QA 임시 디렉터리 전부 제거. 다른 실행 중인 에뮬레이터에는 개입하지 않음.

## 5. Stage 18 — 아직 남은 작업 / 다음 우선순위

**중요: Stage 17에서 확립한 사실은 외부 네트워크의 정상 동작과 실제 카탈로그 메타데이터 접근성이다. 특정 상용 게임 토렌트의 공개 시더 수, 가용성, 다운로드 권한까지 검증한 것은 아니다.**

1. **다운로드·스토어 오류 UX:** 메타데이터 응답 지연, 트래커 타임아웃, 시더 0명, 시드/피어 변동, 연결 끊김의 상태 안내·취소·재시도·중복 클릭 안정성 및 진행률 표시를 검수하고 개선.
2. **미보유 업데이트/DLC의 실제 카탈로그 매칭:** 게임 ID, 업데이트 버전, DLC 소속, 누락 항목, 중복 토렌트, 여러 파일 선택·대기열 묶음이 올바른지 **실제 공개 카탈로그 메타데이터** 기준으로 검사. 불명확한 타이틀은 추측 매칭하지 말 것.
3. **캐시/정합성 예외:** HTTP 304지만 로컬 캐시가 손상된 경우, 잘못된 ETag, HTTP 실패/타임아웃/리다이렉트, 취소 및 재시도 시 기존 정상 카탈로그가 훼손되지 않는지 추가 테스트. UI 프레임과 네트워크 I/O 분리 유지.
4. **대용량 성능 최적화:** 수천 개 스토어 행 검색·스크롤, 텍스트 우선 렌더링과 이미지 지연 로딩, 반복 병합 I/O·메모리·중간 파일 사용량 실제 측정. 감각적인 속도 추정 대신 측정 수치 남길 것.
5. **형식/복구 호환성:** NSP/NSZ 정상 경로는 실제 파일 검수됨. XCI/XCZ 실제 합법적 테스트 샘플이 있는 경우에만 파일 생성 후 에뮬레이터 **로딩까지만** 확인. 파일 부족·디스크 부족·잠금 실패·중단 후 재시작 복구를 함께 검증.
6. **사용자 환경에서의 최종 확인:** GUI 탭 크기·우측 패널 너비 고정·다운로드명 왼쪽 정렬·진행률 및 시드/피어 표시·중복 메뉴/불필요한 파일 여부 확인. 테스트 데이터는 종료 후 정리하고 사용자 `E:\NSW`는 불변.

가장 유용한 다음 단계: **Stage 18 — 다운로드 대기열/스토어 외부 연결 예외·재시도·상태 표시 및 실제 메타데이터 매칭 검수.** 새 기능을 불필요하게 확대하지 말고 기존 자동화 경로의 실패 처리와 체감 성능에 집중한다.

## 6. 새 채팅 시작 명령 (PowerShell)

```powershell
Set-Location 'D:\NSW.dev\NSL'
git status --short --branch
git log -5 --oneline
Get-FileHash '.\target\release\nsl.exe' -Algorithm SHA256
Get-Content '.\docs\qa\QA_NSL_STAGE17_EXTERNAL_CATALOG_PUBLIC_TORRENT_2026-11-21.md'

# 변경 후 일반 회귀 (개발 테스트는 파일 작업 범위에 따라 실행)
cargo fmt --all -- --check
cargo test --locked --all-targets
cargo test --locked -p nsrp-core --lib
cargo clippy --locked --all-targets -- -D warnings

# 아래 둘은 실제 인터넷 접근이 되는 격리 QA 머신에서 명시적으로만 실행
cargo test --locked stage17_live_catalog_download_parse_reload_readonly -- --ignored --nocapture --test-threads=1
cargo test --locked stage17_public_licensed_sintel_selected_subtitle -- --ignored --nocapture --test-threads=1

# 새 Release는 실제 소스 변경 후에만 빌드/검수
cargo build --locked --release
powershell.exe -NoProfile -NonInteractive -ExecutionPolicy Bypass -File '.\scripts\qa-native-gui-window.ps1' -ExpectedCommit (git rev-parse HEAD)
```

검수 로그 (로컬): `D:\NSW.dev\NSL\docs\qa\QA_NSL_STAGE17_*_LOCAL_2026-11-21.txt`.
최종 무결성: `D:\NSW.dev\NSL\docs\qa\QA_NSL_STAGE17_FINAL_INTEGRITY_LOCAL_2026-11-21.txt`.

## 7. 인수인계 직후 유의 사항

- `ignored` 테스트 42개는 전체 회귀에서 자동 실행되지 않는다는 의미이며, **그중 Stage 17 실제 외부 네트워크 E2E 2개는 별도로 실행·PASS**한 상태다. 실행하지 않은 테스트까지 통과했다고 쓰지 않는다.
- 타이틀 화면 로딩은 Stage 14에서 실제 확인했고, Stage 17에서는 재실행하지 않았다. 단계별 검수 범위를 혼동하지 않는다.
- `E:\NSL_QA_20261120`에는 이전 단계 QA 자료가 있을 수 있으므로 **Stage 번호가 다른 폴더를 일괄 삭제하지 않는다**.
- 새 작업에서 코드를 바꾸면 테스트와 빌드를 다시 실행해 SHA·커밋·GUI 버전을 재기록한다. 이 문서 작성만으로는 기존 실행파일의 해시가 바뀌지 않는다.

**Stage 17 완료 / Stage 18 착수 대기.**
