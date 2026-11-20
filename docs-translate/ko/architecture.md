# zero-surplus 아키텍처

[English](../../architecture.md) | 한국어

zero-surplus는 Rust 실행 파일 하나다. 명령마다 공간을 재거나 Windows 기록을 읽고, JSON 문서 하나를 출력하고, 결과를 조사 폴더에 저장한다. Skill은 에이전트에게 어떤 질문에 어떤 명령을 쓰고 결과를 어떻게 읽는지 알려 준다.

## 구성 요소와 역할

| 구성 요소 | 역할 |
|---|---|
| [main.rs](../../src/main.rs), [cli.rs](../../src/cli.rs), [output.rs](../../src/output.rs) | 명령 분기, 인자, JSON 출력과 종료 코드 |
| [case.rs](../../src/case.rs), [list.rs](../../src/list.rs) | 조사 폴더 구조, 결과 ID, 원자적 저장과 목록 |
| [win.rs](../../src/win.rs) | 볼륨 통계, 경로 해석, 링크 판별, 드라이브 대응, 레지스트리 읽기, 승격 |
| [scan/](../../src/scan/mod.rs), [diff.rs](../../src/diff.rs) | 폴더 합계, 스냅샷, 비교 |
| [collect.rs](../../src/collect.rs) | 관리자 권한으로 만드는 Prefetch·SRUM 사본 |
| [usage/](../../src/usage/mod.rs) | UserAssist·Prefetch·SRUM·ShellBags 판독기와 출처 선택 |
| [activity.rs](../../src/activity.rs) | 스냅샷의 폴더별 마지막 사용 |
| [refs/](../../src/refs/mod.rs) | Python 환경과 그 환경을 참조하는 프로젝트 |
| [watch.rs](../../src/watch.rs) | 경로 아래의 변경 기록 |
| [export.rs](../../src/export.rs) | 메모·결과·진단을 옮길 수 있게 묶음 |

## 출력과 저장

모든 명령은 표준 출력에 JSON 문서 하나를 쓰고, `watch`는 JSON Lines를 쓴다. 종료 코드 0은 결과(일부만 읽은 결과 포함), 1은 실행 실패, 2는 입력 오류, 3은 지금 상태로 할 수 없는 요청이다. 비교할 수 없는 스냅샷, 없는 참조, 거부된 승격, 링크인 루트가 3에 해당한다.

저장하는 결과는 `<종류>-<UTC 시각>-<난수>` 형식의 ID를 받고, 임시 이름으로 쓴 뒤 같은 디렉터리에서 최종 이름으로 바뀐다. `list`는 최종 이름을 가진 결과만 보여 주므로, 취소된 측정은 비교 기준이 되지 않는다.

```text
<case>/
  scans/<id>.json          루트 스냅샷(scan-)과 볼륨 측정(vol-)
  diffs/ usage/ refs/ activity/   <id>.json
  watches/<id>.jsonl
  notes/<id>.txt           메모 원문
  diagnoses/<id>.md        진단 원문, 인용한 ID는 <id>.json
  sources/<id>/            collect 사본과 source.json
```

## 공간 측정

`scan --volume`은 Windows에서 총량, 여유 공간, 호출자가 쓸 수 있는 여유 공간을 읽는다. `scan --root`는 [dua-core](https://crates.io/crates/dua-core) 6.1.0으로 루트의 직계 항목을 재고, 그 호출은 [scan/walk.rs](../../src/scan/walk.rs)에 모여 있다. dua-core는 `GetFileInformationByHandleEx(FileIdBothDirectoryInfo)`로 열거해 항목마다 논리적 크기와 할당 크기를 받고, 정션과 심볼릭 링크는 들어가지 않고 보고만 한다.

루트의 직계 항목을 먼저 읽는다. 파일은 가장 큰 이름만 남기는 루트 직속 파일 행에 더하고, 직계 폴더는 각각 번호가 붙은 순회 루트가 되어 항목이 도착하는 대로 행별로 합산된다. 메모리에는 행별 합계와 실패 목록만 둔다.

| 규칙 | 동작 |
|---|---|
| 크기 | 논리적 크기(파일 길이)로 순위를 매기고 `diff`를 계산하며, 할당 크기를 함께 저장 |
| 하드링크 | 경로마다 셈 |
| 링크 | 크기 없는 링크 행으로 남기고, 루트 자체가 링크면 측정하지 않음 |
| 실패 | 실패마다 경로와 Windows 오류 코드를 남기고, 실패가 있는 행은 `partial` |
| 이름 | 루트의 자식을 끝까지 열거했는지를 `names_complete`에 기록 |
| 조사 폴더 | 루트 아래에 있으면 건너뛰고 `excluded`에 기록 |

스냅샷에는 볼륨 GUID 경로와 일련번호, 입력 경로와 해석한 루트 경로, 시작·종료 시각, 두 시각의 여유 바이트, 규칙, 실행 통계(파일 수, 폴더 수, 경과 시간, 최대 메모리, 스레드 수)도 저장한다.

`diff`는 저장한 두 파일만 연다. 먼저 볼륨, 루트, 규칙, 제외 목록을 비교해 다르면 거부한다. 그다음 행을 이렇게 비교한다.

| 두 측정의 상태 | 결과 |
|---|---|
| 양쪽에 폴더가 있고 두 행이 모두 완전함 | 이후에서 이전을 뺌 |
| 양쪽 목록이 완전하고 한쪽에만 이름이 있음 | `appeared` 또는 `disappeared`. 그 행이 완전할 때만 변화량을 계산 |
| 폴더 일부를 읽지 못했거나 이름의 존재가 불확실함 | 읽은 바이트를 남기고 `delta_bytes`는 `null` |

두 측정 시각 사이의 여유 공간 변화를 두 시각과 함께 알려 준다.

## Windows 기록

| 출처 | 판독 |
|---|---|
| UserAssist | 현재 사용자 레지스트리. 값 이름을 ROT13으로 되돌리고 알려진 폴더 GUID를 경로로 바꾼다. 72바이트 레코드에서 실행 횟수(오프셋 4), 포커스 횟수(8), 포커스 시간 ms(12), 마지막 실행(60)을 읽는다. [userassist.rs](../../src/usage/userassist.rs) |
| Prefetch | MAM 헤더(8바이트, CRC가 있으면 12바이트)를 떼고 `RtlDecompressBufferEx`로 푼 뒤 [frnsc-prefetch](https://crates.io/crates/frnsc-prefetch)가 평문 SCCA를 읽는다. 볼륨 일련번호로 참조 경로를 드라이브 문자로 바꾼다. [prefetch.rs](../../src/usage/prefetch.rs) |
| SRUM | 사본을 Windows ESE 엔진(`esent.dll`)으로 복구를 끈 채 읽기 전용으로 연다. `SruDbIdMapTable`에서 앱 식별자와 사용자 SID를 얻고, 앱 자원 사용·네트워크 테이블을 앱별·지역 날짜별로 합산한다. 대소문자만 다른 식별자는 합친다. [srum.rs](../../src/usage/srum.rs) |
| ShellBags | 현재 사용자의 BagMRU 두 계층. [winparsingtools](https://crates.io/crates/winparsingtools)가 shell item을 분류하고, 긴 이름은 `0xBEEF0004` 확장 블록에서 읽는다. [shellbags.rs](../../src/usage/shellbags.rs) |

Prefetch 기록의 실행 파일 경로는 참조 파일 중 실행 파일 이름과 같은 것에서 정한다. 29자에서 잘린 이름이면 그 이름으로 시작하고 `.EXE`, `.COM`, `.SCR`로 끝나는 파일이다. 후보가 하나면 그 경로다. 여럿이면 장치 경로의 해시가 Prefetch 파일 이름의 해시와 같은 것을 고른다. 해시는 314159에서 시작해 대문자 UTF-16LE 경로의 바이트마다 `hash * 37 + byte`(2^32 나머지)를 한다. 실행 인자까지 해시에 넣는 프로그램(서비스 호스트, 패키지 앱, 다중 프로세스 앱)은 후보가 여럿 남고, 이것을 `path_candidates`로 돌려준다.

Prefetch 압축 해제와 ShellBags 긴 이름은 라이브러리의 해당 처리 옆에 둔 자체 코드다. 라이브러리를 올릴 때는 이 코드가 처리하는 입력에서 라이브러리 출력을 대조한다.

`collect`는 승격되지 않았으면 UAC 창을 거쳐 자신을 다시 실행하고 승격된 프로세스를 기다린다. 그 프로세스는 Prefetch 파일과 SRUM DB를 복사하고, 복사 시각, 방법, DB 상태, 판독 가능 여부, 드라이브 대응을 `source.json`에 쓴다. SRUM은 공유 읽기로 복사하고, 파일이 잠겨 있으면 `esentutl /y /vss`로 복사한다. SRUM 사본은 판독기가 여는 방식으로 열어 보고, 읽히지 않으면 5초 뒤 다시 복사하며(최대 6번), 끝까지 실패하면 `readable: false`와 이유를 남긴 채 둔다.

출처는 조회마다 고른다.

| 출처 | 순서 |
|---|---|
| Prefetch | `--prefetch-dir`, 실제 시스템, 조사 폴더의 최신 사본 |
| SRUM | `--srum-db`만, 없으면 조사 폴더의 사본을 최신부터 읽힐 때까지(건너뛴 사본은 이유와 함께 반환), 사본이 없으면 실제 시스템 |

`usage`, `refs`, `activity`가 이 선택을 같이 쓴다. ESE 페이지 크기는 프로세스 전체 설정이라 프로세스마다 한 번 정한다.

## 폴더별 마지막 사용

`activity`는 UserAssist·Prefetch·SRUM의 모든 실행을 한 번 읽고, 실행 파일 경로로 각 실행을 저장된 스냅샷의 폴더 행에 잇는다.

| 연결 | 조건 |
|---|---|
| `inside` | 실행 파일이 폴더 안에 있음 |
| `exact` | 확장자를 뺀 실행 파일 이름이 폴더 이름이나 그 단어와 같음 |
| `program` | 폴더 이름의 단어가 실행 파일 이름의 단어임 |
| `folder` | 폴더 이름의 단어가 실행 파일 상위 폴더 이름의 단어임 |

폴더의 마지막 사용은 가장 강한 연결 안에서 가장 최근 실행이다. 이름은 구분자, 소문자에서 대문자로 바뀌는 곳, 글자와 숫자 사이에서 단어로 나누고, 네 글자보다 짧은 조각은 통째 단어일 때만 쓴다. 일반적인 단어(`Microsoft`, `Programs`, `Temp`, `Cache` 등 `GENERIC`에 있는 것)는 잇는 데 쓰지 않는다. `program`과 `folder`는 같은 회사의 다른 프로그램에 닿을 수 있어 근거에 실행의 프로그램과 출처를 적는다.

## Python 환경

[refs/envs.rs](../../src/refs/envs.rs)는 지정한 폴더와 각 프로젝트의 `.venv`, `venv`에서 `pyvenv.cfg`나 `conda-meta`로 환경을 찾고, 같은 폴더로 이어지는 경로를 묶는다. [refs/projects.rs](../../src/refs/projects.rs)는 VS Code 설정, JetBrains 프로젝트 파일과 SDK 표, 지정한 실행 스크립트(토큰으로 나눌 뿐 실행하지 않음), `environment.yml`에서 인터프리터 참조를 읽는다. [refs/resolve.rs](../../src/refs/resolve.rs)는 참조를 그 프로젝트의 파일시스템 안에서 해석한다. `${workspaceFolder}`, `%~dp0`, 홈 폴더를 치환하고, POSIX 경로는 프로젝트의 WSL 배포판 안에서 해석하며, 실행 시 결정되는 값은 미해결로 둔다.

[refs/mod.rs](../../src/refs/mod.rs)는 참조를 환경 하나에 잇거나, 잇지 못하는 이유(버전만 지정, 실행 시 결정, 같은 이름의 환경이 여럿, 조사 범위 밖)를 남긴다. 크기는 조사 폴더의 스냅샷에서, 활동은 환경 인터프리터의 Prefetch·SRUM 기록에서 가져온다. 참조도 공용 지정도 없는 환경을 크기순으로 낸다.

## 경로 감시

[watch.rs](../../src/watch.rs)는 가장 가까운 기존 상위 폴더에 하위 포함으로 `ReadDirectoryChangesW`를 등록해 파일 이름·디렉터리 이름 변경을 받고, 대상과 그 아래 것만 남긴다. 0바이트 완료나 `ERROR_NOTIFY_ENUM_DIR`은 마지막으로 받은 시각부터 그때까지의 누락 구간으로 알린다. 사건은 표준 출력과 조사 폴더의 임시 로그에 함께 쓴다. 로그는 첫 쓰기 오류에서 멈추고, 끝날 때 디스크에 반영한 뒤 이름을 바꾼다. 이것이 실패하면 임시 로그를 지우고 `end` 줄에 `store_error`를 붙이며 종료 코드는 1이다. Windows 변경 알림에는 프로세스가 없어 `process`는 `null`이다.

## 조사 인계

[export.rs](../../src/export.rs)는 메모를 그대로 저장하고, 새 진단을 인용한 ID와 함께 새 ID로 저장한다. 무엇이든 쓰기 전에 요청한 ID와, 진단마다 인용한 ID를 모은다. 없는 ID나 읽지 못한 인용 목록이 있으면 내보내기를 멈춘다. 묶음은 임시 이름으로 만들어 상대경로 `manifest.json`을 넣은 뒤 이름을 바꾼다. 같은 이름의 묶음은 건드리지 않는다.

## 플러그인 구성

저장소 루트가 플러그인이자 마켓플레이스다. Claude Code는 [.claude-plugin/plugin.json](../../.claude-plugin/plugin.json)과 플러그인 위치를 `./`로 적은 [.claude-plugin/marketplace.json](../../.claude-plugin/marketplace.json)을 읽는다. Codex는 Agent Plugins 형식의 루트 [plugin.json](../../plugin.json)과 [.agents/plugins/marketplace.json](../../.agents/plugins/marketplace.json)을 읽는다. 두 도구 모두 `skills/` 아래의 Skill을 찾는다. `bin/zero-surplus.exe`는 release 빌드다. Claude Code는 플러그인이 켜져 있는 동안 `bin/`을 셸의 `PATH`에 올리고, Codex 사용자는 실행 파일을 직접 `PATH`에 둔다.

## 검증

통합 테스트는 빌드한 실행 파일을 작은 fixture에 실행한다.

| 테스트 | 확인하는 것 |
|---|---|
| [scan_diff](../../tests/scan_diff.rs) | 크기, 정션, 하드링크, 접근 거부 폴더. 비교, 거부되는 비교, 취소된 결과, 제외되는 조사 폴더 |
| [refs](../../tests/refs.rs) | 공용 환경, 별칭, 미해결 참조, 후보 순서, 출처별 활동 상태 |
| [activity](../../tests/activity.rs) | 연결 종류마다, 더 최근의 약한 연결보다 강한 연결이 이김, 일반적인 이름, 스냅샷이 없는 루트 |
| [watch_export](../../tests/watch_export.rs) | 삭제 후 재생성, 없는 대상 감시, 로그 쓰기 실패와 이름 충돌, 옮긴 묶음, 없는 ID, 손상된 인용 목록 |
| [prefetch_reference](../../tests/prefetch_reference.rs) | [Prefetch fixture](../../tests/fixtures/prefetch) 다섯 개와 같은 파일의 PECmd 출력 대조 |
| [srum_copies](../../tests/srum_copies.rs) | ESE 엔진으로 만든 SRUM 형태의 DB: 읽을 수 없는 사본 건너뛰기, 모든 사본 실패, 지정 파일, 앱별·날짜별 합계 |

Prefetch와 SRUM 판독기는 한 PC의 기록으로 PECmd·SrumECmd와 대조했다. Prefetch 435개 파일 전부가 실행 파일 이름, 실행 횟수, 실행 시각 8개, 실행 파일 경로, 참조 파일에서 일치하고, 표본 SRUM 앱 27개가 카운터와 시각 12개 항목에서 일치한다. 대조 내용과 남은 항목은 [working/implementation.md](../../working/implementation.md)에 있다.

저장소 루트에서 실행한다.

```text
cargo test
```
