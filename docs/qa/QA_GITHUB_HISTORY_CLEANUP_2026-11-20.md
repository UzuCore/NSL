# NSL GitHub 과거 빌드·태그 정리 — 2026-11-20

- 저장소: https://github.com/UzuCore/NSL
- 기준 날짜: **2026-11-20 00:00:00 KST** (2026-11-19 15:00:00 UTC)
- 삭제: 기준 시각 이전에 게시된 Releases, 오래된 원격 태그, 이전 Actions 실행 기록 및 빌드 아티팩트.
- 보존: 2026-11-20 KST에 생성·게시된 릴리스, 태그, Actions 실행 기록 및 아티팩트.
- 운영 정책: 향후 소스 변경은 Git 커밋·push만. 빌드 실행과 워크플로 수동 트리거는 사용자가 직접 진행.

## GitHub 정리 결과

| 종류 | 삭제 | 보존 | 마지막 재조회 |
| --- | ---: | ---: | --- |
| GitHub Releases | 21개 | 6개 | PASS |
| 원격 Git 태그 | 24개 (릴리스 연결 21 + 단독 3) | 6개 | PASS |
| GitHub Actions 실행 기록 | 15개 | 7개 | PASS |
| GitHub Actions 빌드 아티팩트 | 13개 | 6개 | PASS |

삭제된 릴리스에는 2026-11-19 빌드 12개, 과거 1.x 릴리스 8개, 2026년 5월의 titledb 프리릴리스 1개가 포함된다. titledb 릴리스에 있던 과거 titles.json 아티팩트도 함께 삭제되었으므로 이전 릴리스 URL로 다운로드할 수 없다. 별도 릴리스가 없는 구형 1.3.11, 1.3.17, 1.3.23 원격 태그도 삭제했다.

## 2026-11-20 KST 항목 보존

- 원격 릴리스/태그: build-20261120-004106-KST-r16, build-20261120-015155-KST-r17, build-20261120-031143-KST-r18, build-20261120-053751-KST-r19, build-20261120-064716-KST-r21, build-20261120-072438-KST-r22.
- Actions 실행 기록: 44555659988, 44569356154, 44583569463, 44609422299, 44615223034, 44620288849, 44626588472.
- 최신 r22 릴리스 및 메인 소스 커밋 d09c91ab18955e86a2f1c36c730130faaf69f3f0 보존.
- 오늘의 GitHub Actions 빌드 아티팩트 6개도 보존.

## 안전 검수·감사 자료

- 삭제 이전 대상 및 보존 목록 스냅샷: docs/qa/scratch-gh-cleanup-before-20261120.json (로컬 전용).
- 삭제 결과 및 재검증 기록: docs/qa/scratch-gh-cleanup-result-20261120.json (로컬 전용).
- GitHub API에서 모든 삭제 대상이 사라졌고 모든 보존 항목이 남아 있음을 재조회. 실패 0건.
- .github/workflows/windows-build.yml은 workflow_dispatch 수동 실행 전용이며 Git push 자동 빌드 트리거가 없다.
- 이번 작업에서 cargo/build.bat/GitHub workflow_dispatch 등의 빌드는 **실행하지 않았다**.
- 로컬 NSL 실행파일, 배포 ZIP, DB, 키/설정 및 다운로드 폴더를 변경하지 않았다. 원격 릴리스·태그·빌드 기록만 삭제.
- GitHub main에는 이 검수 기록만 커밋·push하며 새 릴리스는 만들지 않는다.
