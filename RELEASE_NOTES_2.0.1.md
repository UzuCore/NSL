# NSL 2.0.1 — Rust Windows x64

NSL의 새로운 **Rust 네이티브 Windows 64비트 버전**입니다. 기존 1.4.8 릴리스는 그대로 유지하며, 이번 Rust 버전은 별도의 소스 브랜치 `rust-v2`에서 배포합니다.

## 설치 및 업데이트

- **Windows 64비트:** `nsl-rust-win64.zip`을 다운로드해 새 폴더에 압축 해제하고 `nsl.exe`를 실행하세요.
- **이전 Rust 2.0.0 사용자:** 프로그램 내 업데이트 확인 기능에서 2.0.1을 감지할 수 있습니다. 프로그램 내 업데이트는 기존 사용자 DB·설정·게임 파일을 유지하도록 설계됐습니다.
- 기존 Python 기반 1.x 설치에서 전환할 경우에는 먼저 **별도 폴더 설치**를 권장합니다. 사용자 설정과 키 파일은 릴리스 ZIP에 포함되지 않습니다.
- ZIP에 들어 있는 `bin/hactool.exe`, `bin/nstool.exe`, `titledb` 파일도 함께 사용합니다. **`nsl.exe` 단독 다운로드만으로는 완전한 신규 설치가 아닙니다.**

## 주요 변경 사항

- **게임 전체 스캔 대폭 가속:** NCA 복호화와 CNMT·NACP 분석을 Rust 프로세스 내부에서 실행해 수천 번의 `hactool` 외부 실행을 제거했습니다. 미지원 암호화 형식에는 기존 `hactool` 폴백을 유지합니다.
- 실제 **846개 패키지** 대상 전체 스캔: 디스크 캐시가 준비된 측정에서 3차 최적화 기준 **47.43초 → 4차 약 0.923초**(테스트 프로필). 최종 Windows 릴리스 실행파일의 캐시된 별도 설치 환경에서는 **0.76초**가 측정됐습니다. 단, **대용량 E: 저장장치의 디스크 캐시가 차가운 재검증에서는 약 55~57초**가 걸렸습니다. 이 수치는 CPU 처리뿐 아니라 저장장치 I/O의 영향을 크게 받으므로 모든 PC에서 동일한 단축 비율을 보장하지 않습니다.
- 게임 파일 846건·콘텐츠 4,647건, 기본 게임 800·업데이트 710·DLC 3,137건을 유지했습니다. 네이티브 분석기와 기존 `hactool`의 콘텐츠·한글/영어/일본어 게임명 비교 결과 차이 **0건**입니다.
- 라이브러리 DB 조회 범위와 메모리 사용량 최적화, 스토어 목록/작업 로그 가상화, 표지 다운로드 병렬 처리와 실패 시 재요청 제한, 증분 스캔 캐시 개선, 안전한 SQLite 일괄 저장이 포함됩니다.
- `hactool` ISC 오픈소스 라이선스 고지를 배포 ZIP의 `licenses/hactool-ISC.txt`에 포함했습니다.

## 검수 결과

- Rust 자동 테스트 **52개 PASS**, 실파일 테스트 **4개 PASS**(846개 파일 전수 비교 포함), Clippy 경고 0개.
- Windows GUI 스토어 7,330건 표시·페이지 이동·로그창·화면 가상화 검수 완료.
- 사용자 `library.db`, `config.json`, `prod.keys`, `title.keys` 및 실제 게임 파일은 릴리스에 포함되지 않으며, 테스트 과정에서도 원본 파일을 변경하지 않았습니다.
- 검수 상세: [`QA_OPTIMIZATION_STAGE4_2026-11-18.md`](https://github.com/UzuCore/NSL/blob/rust-v2/QA_OPTIMIZATION_STAGE4_2026-11-18.md)

**다운로드:** `nsl-rust-win64.zip` (설치 파일) 및 `SHA256SUMS-2.0.1.txt` (무결성 확인).

---

### English summary

NSL 2.0.1 is the first public Rust-native Windows x64 package. It replaces thousands of external `hactool` invocations with an in-process, read-only NCA metadata reader and retains `hactool` for unsupported archives. All 846 sampled local game packages matched the existing analyzer exactly for content IDs, versions, types, and localized names. The release ZIP excludes user data, configuration, and keys. Install the complete ZIP; it is not a standalone EXE. The legacy 1.4.x releases and their source branch remain untouched.

Scan timings vary substantially with disk cache and storage I/O: the warm-cache test was below one second, while the cold-storage read-only validation took approximately 55–57 seconds for the same corpus.
