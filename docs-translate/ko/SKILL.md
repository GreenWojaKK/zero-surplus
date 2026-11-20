# zero-surplus Skill

[English](../../skills/zero-surplus/SKILL.md) | 한국어

`zero-surplus`는 PATH에 있는 실행 파일이다. 측정하고 기록을 읽어 근거를 돌려준다. 파일을 지우거나 옮기지 않는다. 조사 순서와 원인 판단은 이 문서의 범위가 아니다.

## 공통

- 모든 명령은 표준 출력에 JSON 하나를 쓴다. `watch`만 한 줄에 사건 하나씩 쓴다. 진행 메시지는 표준 오류로 나간다. `--pretty`를 붙이면 들여쓴다.
- 종료 코드: `0` 결과를 냄(일부만 읽은 결과 포함), `1` 실행 실패, `2` 입력 오류, `3` 지금 상태로는 할 수 없음. 0이 아니면 출력은 `{"error":{"kind","message","detail"}}`이다.
- `--case <폴더>`는 조사 자료를 저장하는 폴더다. 환경 변수 `ZERO_SURPLUS_CASE`로도 지정한다. 저장한 결과는 `id`와 `stored`(조사 폴더 기준 상대경로)를 돌려준다.
- 시각은 로컬 시간대 오프셋이 붙은 RFC 3339다. 크기는 모두 바이트다.
- 출력은 큰 순서로 `--top <N>`개까지만 보여주고 나머지는 개수와 합계로 줄인다. 저장된 파일에는 모든 행이 있다.

```powershell
zero-surplus scan --case C:\cases\disk --volume C: --root C:\Users\me\AppData\Local
zero-surplus list --case C:\cases\disk
```

## scan — 용량 측정

| 입력 | 뜻 |
|---|---|
| `--volume C:` | 드라이브의 총량, 사용량, 여유 공간을 Windows 통계에서 읽는다. |
| `--root <경로>` | 그 폴더의 직계 폴더별 합계를 잰다. 여러 번 줄 수 있고 루트마다 스냅샷 하나가 생긴다. |
| `--appdata` | `Local`, `LocalLow`, `Roaming` 세 루트를 준 것과 같다. |
| `--top <N>` | 출력할 폴더 행 수(기본 30). |
| `--top-files <N>` | 루트 직속 파일 중 큰 것 몇 개를 남길지(기본 20). |

`volumes[]`의 항목(ID는 `vol-`): `total_bytes`, `used_bytes`, `free_bytes`(볼륨 전체), `free_bytes_caller`(현재 사용자가 쓸 수 있는 양), `volume`, `measured_at`.

`snapshots[]`의 항목(ID는 `scan-`):

| 필드 | 읽는 법 |
|---|---|
| `dir_rows[]` | 직계 폴더 행. `bytes`는 논리적 크기(파일 길이의 합), `allocated_bytes`는 디스크에 할당된 크기다. 순위는 `bytes` 기준이다. |
| `status` | `complete`는 끝까지 읽은 행, `partial`은 일부를 읽지 못한 행이다. `partial` 행의 `bytes`는 읽은 만큼만이다. |
| `failures_sample[]`, `failures_total` | 읽지 못한 위치. `path` 아래 전체가 합계에서 빠져 있다. `code`는 Windows 오류 코드다(5는 접근 거부). |
| `root_files` | 루트에 직접 있는 파일의 합계와 큰 파일 목록(`top`). |
| `links_not_followed[]` | 정션과 심볼릭 링크. 따라가지 않으며 크기에 넣지 않는다. |
| `names_complete` | 루트의 자식 이름을 끝까지 열거했는지. `false`면 빠진 폴더가 있을 수 있다. |
| `dir_rows_not_shown` | 출력에서 생략한 행의 수, 합계, 그중 `partial` 행 수. |
| `excluded[]` | 일부러 건너뛴 경로(루트 아래에 있는 조사 폴더). |
| `free_bytes_start`, `free_bytes_end` | 측정 시작과 끝의 볼륨 여유 공간. |
| `rules` | 집계 규칙. 하드링크는 경로마다 따로 더한다. |

해석할 때 지킬 것:

- 폴더의 `bytes`는 지우면 확보되는 공간이 아니다. 여러 폴더가 같은 데이터를 공유하면 두 값이 다르다. 실제로 늘어난 공간은 정리 전후의 볼륨 여유 공간으로 확인한다.
- 서로 포함 관계인 루트의 행을 더하지 않는다. 논리적 크기의 합을 볼륨 사용량이라고 하지 않는다.
- 디렉터리 항목에 적힌 크기는 파일이 열려 있는 동안 늦게 갱신될 수 있다.

오류 `kind`: `root_is_link`(루트가 링크라 측정하지 않음), `root_unreadable`, `cancelled`(저장하지 않음).

## diff — 저장한 두 결과 비교

```powershell
zero-surplus diff --case C:\cases\disk <이전 ID> <이후 ID>
```

`scan-` ID 둘 또는 `vol-` ID 둘을 받는다. 저장한 두 파일만 읽고 폴더를 다시 측정하지 않는다.

| 필드 | 읽는 법 |
|---|---|
| `rows[].state` | `changed`, `appeared`(새로 생김), `disappeared`(사라짐), `unknown`(계산할 수 없음). 변하지 않은 행은 출력하지 않고 `rows_unchanged`로 센다. |
| `rows[].delta_bytes` | 이후에서 이전을 뺀 값. `null`이면 한쪽을 다 읽지 못했거나 이름의 존재 여부가 불확실해서 계산하지 않은 것이다. 이때도 `before.bytes`와 `after.bytes`는 읽은 값 그대로 있다. |
| `free_space_change` | 여유 공간 변화(`label`: `free space change`). `delta_bytes`는 두 측정 시각(`before_at`, `after_at`) 사이의 다른 쓰기와 삭제도 반영한다. |

두 결과의 측정 조건(볼륨, 루트, 규칙, 제외 목록)이 다르면 종료 코드 3과 `not_comparable`을 내고 `detail.mismatches[]`에 다른 항목을 적는다.

## list — 저장된 결과 찾기

```powershell
zero-surplus list --case C:\cases\disk --kind scan
```

`items[]`: `id`, `kind`, `at`(시각), `target`(대상), `path`. `--kind`는 `scan`, `volume`, `diff`, `usage`, `refs`, `activity`, `watch`, `note`, `diagnosis`, `source` 중 하나다. 끝까지 저장된 결과만 나온다.

## collect — 관리자 권한이 필요한 기록 복사

```powershell
zero-surplus collect --case C:\cases\disk
```

Prefetch 파일과 SRUM DB를 조사 폴더의 `sources/<id>/`에 복사한다. 실행하면 사용자 화면에 UAC 창이 뜨고, 사용자가 승인할 때까지 기다린다. 거부하면 종료 코드 3과 `elevation_declined`를 낸다. 이후 `usage`, `refs`, `activity`는 일반 권한으로 이 사본을 읽는다. 사본은 `collected_at` 시점의 기록이다.

출력: `prefetch.files`(복사한 파일 수), `srum.readable`, `srum.database_state`, `srum.attempts`, `drives[]`.

SRUM은 서비스가 DB에 쓰는 도중에 복사되면 읽을 수 없는 사본(`DirtyShutdown`)이 된다. `collect`는 복사할 때마다 읽어 보고, 읽히지 않으면 5초 뒤 다시 복사한다(최대 6번). 끝까지 읽히지 않으면 `srum.readable`이 `false`이고 `srum.reason`에 이유가 있다. 이 사본은 조회에서 건너뛴다.

## usage — 실행 기록 조회

```powershell
zero-surplus usage --case C:\cases\disk --app python.exe
zero-surplus usage --case C:\cases\disk --app "C:\Users\me\anaconda3\envs\FAI\python.exe"
zero-surplus usage --folder C:\Users\me\Documents
```

`--app`은 파일 이름 또는 전체 경로다. 이름으로 주면 같은 이름의 모든 위치가 경로별로 따로 나온다. `--folder`는 탐색기에서 그 폴더를 연 흔적을 찾는다.

`sources`의 출처마다 `status`가 따로 있다. 한 출처를 읽지 못해도 나머지는 나온다.

| `status` | 뜻 |
|---|---|
| `ok` | 기록을 읽었고 대상의 기록이 있다. |
| `no_record` | 기록을 읽었지만 대상의 기록이 없다. 실행한 적이 없다는 뜻은 아니다. |
| `access_denied` | 권한이 없어 읽지 못했다. Prefetch와 SRUM이면 `collect`가 필요하다. |
| `source_absent` | 기록 자체가 없다. |
| `unsupported_format` | 기록은 있지만 읽을 수 없는 형식이나 상태다. `message`에 이유가 있다. |

`origin`은 자료를 어디서 읽었는지다: `live`(실제 시스템), `copy`(`collect` 사본, `collected_at` 포함), `explicit`(`--prefetch-dir`, `--srum-db`로 지정).

SRUM 사본은 최신부터 읽어 보고 처음 읽힌 사본을 쓴다(`origin.selection`: `newest_readable_copy`). 건너뛴 더 최신 사본은 `skipped_copies[]`에 `source`, `collected_at`, `database_state`, `reason`과 함께 나온다. 이때 결과는 `origin.collected_at` 시점까지의 기록이다. 읽히는 사본이 하나도 없으면 `unsupported_format`이고 `skipped_copies[]`에 시도한 사본이 모두 나온다. `refs`의 `activity_sources.srum`도 같은 필드를 쓴다.

| 출처 | 기록 필드 | 읽는 법 |
|---|---|---|
| `userassist` | `last_run`, `run_count`, `focus_count`, `focus_ms`, `group` | 탐색기나 시작 메뉴로 실행한 프로그램만 남는다. `last_run`만 갱신되고 횟수가 0인 경우가 있으므로 포커스 값을 함께 본다. `group`은 실행 파일(`executable`)과 바로가기(`shortcut`)를 구분한다. |
| `prefetch` | `last_run`, `last_runs[]`(최대 8개), `run_count`, `path`, `path_basis`, `path_candidates`, `referenced_count` | 터미널에서 실행한 프로그램도 남는다. 경로는 대문자로 저장된다. `last_runs`는 최신순이다. `path`는 실행 파일 경로를 하나로 정할 수 있을 때만 있다. 참조 파일 중 실행 파일 이름이 하나뿐이면 `path_basis`가 `only_candidate`, 여럿 중 Prefetch 파일 이름의 해시로 골랐으면 `file_name_hash`다. 정할 수 없으면 `path`는 `null`이고 후보가 `path_candidates`에 나온다. 경로로 조회하면 후보 중 하나와 같은 기록도 나온다. `refs`의 활동에는 경로가 정해진 기록만 쓴다. 실행 직후 참조한 파일 목록(`referenced`)은 저장된 결과 파일에만 있다. |
| `srum` | `observed_from`, `observed_to`, `active_days`, `totals`, `days[]` | 약 한 시간 단위로 기록된 CPU(`*_cycles`, CPU cycle 수), 디스크(`*_bytes_read/written`), 네트워크(`bytes_sent/received`) 사용량이다. 마지막 기록 이후의 사용은 아직 들어 있지 않다. |
| `shellbags` | `path`, `key_last_write`, `folder_itself_recorded` | 탐색기로 연 폴더만 남는다. `key_last_write`는 그 폴더 아래 항목이 마지막으로 추가되거나 순서가 바뀐 시각이다. |

## activity — 폴더별 마지막 사용

```powershell
zero-surplus activity --case C:\cases\disk --appdata
zero-surplus activity --case C:\cases\disk --root C:\Users\me --min-bytes 1073741824
```

저장된 스냅샷의 폴더 행마다, 그 폴더와 이어지는 프로그램의 마지막 실행을 붙인다. "크지만 오래 안 쓴 프로그램 폴더"를 찾는 명령이다. 루트는 먼저 `scan --root`(또는 `--appdata`)로 재 둬야 한다. 재지 않은 루트를 주면 종료 코드 3과 `no_snapshot`을 낸다. 실행 기록은 `usage`와 같은 출처(UserAssist, Prefetch, SRUM)와 같은 사본 선택 규칙을 쓴다.

| 입력 | 뜻 |
|---|---|
| `--root <경로>`, `--appdata` | 이 루트의 가장 최근 스냅샷을 쓴다. |
| `--min-bytes <N>` | 이보다 작은 폴더는 뺀다(기본 100 MiB). |
| `--top <N>` | 출력할 행 수(기본 100). 저장 파일에는 모든 행이 있다. |

`rows[]`는 크기순이다.

| 필드 | 읽는 법 |
|---|---|
| `folder`, `bytes`, `status` | 스냅샷의 폴더 행 그대로다. |
| `last_use` | 이어진 프로그램 중 가장 강한 근거 종류에서 가장 최근 실행 시각. |
| `evidence` | `program`(실행 파일 경로나 이름), `source`(출처), `time`, `link`(근거 종류). |
| `link` | 강한 순서로 `inside`(실행 파일이 폴더 안에 있음), `exact`(실행 파일 이름이 폴더 이름이나 그 단어와 같음, `Code`와 `Code.exe`), `program`(폴더 이름의 단어가 실행 파일 이름에 있음, `Kakao`와 `KakaoTalk.exe`), `folder`(폴더 이름의 단어가 실행 파일의 상위 폴더 이름에 있음, `PreSonus`와 `...\PreSonus\Studio One 6\Studio One.exe`). `program`과 `folder`는 같은 회사의 업데이트 프로그램처럼 다른 프로그램에 이어질 수 있으므로 `evidence.program`을 확인한다. |
| `programs_linked` | 이 폴더에 이어진 서로 다른 프로그램 수. |
| `name_words` | 이름으로 잇는 데 쓴 폴더 이름의 단어. `Microsoft`, `Programs`, `Temp`, `Cache` 같은 일반적인 단어는 쓰지 않는다. |

`last_use`가 `null`이면 두 경우 중 하나다. 이 폴더의 프로그램이 기록이 남은 기간 동안 실행되지 않았거나, 폴더 이름으로 프로그램을 알 수 없다(`pip`처럼 `python -m`으로 실행되는 도구, 여러 프로그램이 같이 쓰는 폴더). 기록이 남은 기간은 `coverage`에 있다: `srum.observed_from`(SRUM에 남은 가장 오래된 기록), `prefetch.oldest_run`(Prefetch에 남은 가장 오래된 실행).

## refs — 프로젝트와 Python 환경 연결

```powershell
zero-surplus refs --case C:\cases\disk --projects-in C:\Users\me\Documents --envs-in C:\Users\me\anaconda3\envs --shared C:\Users\me\anaconda3
```

| 입력 | 뜻 |
|---|---|
| `--project <폴더>` | 프로젝트 하나. |
| `--projects-in <폴더>` | 직계 폴더를 각각 프로젝트로 본다. |
| `--envs-in <폴더>` | 직계 폴더 중 환경인 것을 찾는다. |
| `--env <경로>` | 환경 하나. |
| `--shared <경로>` | 사용자가 공용으로 지정한 환경. 정리 후보에서 뺀다. |
| `--script <파일>` | 조사할 실행 스크립트. 읽기만 하고 실행하지 않는다. |
| `--no-activity` | 실행 기록 조회를 건너뛴다. |

각 프로젝트 루트의 `.venv`, `venv`도 환경으로 찾는다. 참조는 VS Code 설정(`python.defaultInterpreterPath`), JetBrains SDK 이름, 지정한 스크립트의 인터프리터·`activate`·`conda activate` 구문, `environment.yml`의 이름에서 읽는다.

| 필드 | 읽는 법 |
|---|---|
| `environments[]` | 환경별 행. `referenced_by_projects`는 이 환경을 가리키는 프로젝트, `references[]`는 근거(파일과 키)다. 같은 폴더를 가리키는 다른 경로는 `aliases`로 한 행에 묶인다. |
| `size`, `size_state` | 크기는 조사 폴더의 스냅샷에서 가져온다. `unmeasured`면 그 환경의 상위 폴더를 `scan --root`로 잰 적이 없다는 뜻이다. |
| `activity`, `last_activity` | 그 환경의 Python 실행 파일로 조회한 Prefetch와 SRUM 기록. `activity.prefetch.projects`는 실행 직후 참조한 파일이 속한 프로젝트다. 출처 상태는 `activity_sources`에 있다. |
| `unreferenced[]` | 어느 프로젝트도 가리키지 않고 공용 지정도 없는 환경. 크기순이며 크기가 없는 것은 뒤에 온다. `located_in_project`가 있으면 그 프로젝트 폴더 안에 있는 환경이다. |
| `unresolved_references[]` | 한 환경으로 확정하지 못한 참조와 이유(`reason`): 버전만 지정됨, 실행 시 결정되는 경로, 같은 이름의 환경이 여러 곳에 있음 등. 추측으로 연결하지 않는다. |
| `references_outside_scope[]` | 조사 범위에 없는 인터프리터를 가리키는 참조. |
| `scope` | 조사한 프로젝트와 환경 폴더, 읽지 못한 프로젝트(`unreadable`). `unreferenced`는 이 범위 안에서만 성립한다. |

## watch — 경로 변경 기록

```powershell
zero-surplus watch --case C:\cases\disk --path C:\Users\me\AppData\Local\SomeCache --duration 90s
```

지정한 시간(`s`, `m`, `h`) 동안 대상과 그 하위의 생성·삭제·이름 변경을 기록하고 끝난다. 대상이 아직 없어도 된다. 시간이 끝날 때까지 돌아가므로 백그라운드로 실행하고, `ready` 줄이 나온 뒤에 일어난 변화만 기록된다.

| 줄(`event`) | 필드 |
|---|---|
| `ready` | 감시가 등록된 시각 `time`, 대상 `path`, 실제로 감시하는 상위 폴더 `watching`, `target_exists`. |
| `created`, `deleted`, `renamed` | `time`(알림을 받은 시각), `path`, 이름 변경이면 `old_path`. `process`는 항상 `null`이다. Windows 변경 알림에는 어느 프로그램이 썼는지가 없다. |
| `gap` | `from`부터 `to`까지의 변화를 놓쳤다. 그 구간에 사건이 없었다고 읽지 않는다. |
| `end` | `reason`, `target_exists`, 사건 수 `events`, 놓친 구간 수 `gaps`, 저장 위치 `stored`. |

로그를 조사 폴더에 저장하지 못하면 `end`의 `stored`가 `null`이고 `store_error`에 이유가 있다. 이어서 마지막 줄에 `log_not_stored` 오류가 나오고 종료 코드는 1이다. 표준 출력으로 나온 사건은 그대로 유효하다. 저장하지 못한 로그는 남기지 않는다.

## export — 조사 자료 묶음

```powershell
zero-surplus export --case C:\cases\disk --out C:\handover --note-file memo.txt --include scan-... --add-diagnosis diagnosis.md --cites scan-... diff-...
zero-surplus export --case C:\cases\disk --out C:\handover --diagnosis diag-...
```

| 입력 | 뜻 |
|---|---|
| `--note-file <파일>`, `--note-stdin` | 사용자 메모 원문(UTF-8). 바꾸지 않고 저장한다. |
| `--include <ID>` | 묶음에 넣을 저장 결과. |
| `--add-diagnosis <파일>` `--cites <ID>...` | 새 진단을 새 ID로 저장하고, 근거로 인용한 ID를 함께 기록한다. |
| `--diagnosis <ID>` | 이전 진단. 그 진단이 인용한 근거도 따라 들어간다. |
| `--out <폴더>`, `--name <이름>` | 묶음 폴더를 만들 위치와 이름. |

묶음 폴더의 `manifest.json`에는 `items[]`(`kind`, `id`, `path`, 진단이면 `cites`)가 있고 경로는 모두 묶음 기준 상대경로다. 폴더째 옮겨도 열린다. `collect`의 원본 사본은 넣지 않는다.

조사 폴더에 없는 ID가 있으면 묶음을 만들지 않고 종료 코드 3과 `missing_references`(`detail.missing[]`)를 낸다. 진단이 인용한 ID 목록(`diagnoses/<id>.json`)을 읽지 못하면 종료 코드 3과 `unreadable_citations`(`detail.unreadable[]`에 `diagnosis`, `message`)를 낸다. 같은 이름의 묶음이 있으면 건드리지 않고 `bundle_exists`를 낸다.
