# zero-surplus

[English](../../README.md) | 한국어

zero-surplus는 코딩 에이전트용 Skill을 함께 제공하는 Windows 저장공간 진단 CLI다. Claude Code와 Codex는 이 CLI로 디스크 공간이 어디에 쓰이는지, 큰 폴더의 프로그램을 아직 쓰는지, 정리한 뒤에도 공간이 다시 늘어나는 이유가 무엇인지 조사한다. 측정값과 Windows 기록을 근거로 돌려주고 파일은 그대로 둔다.

CLI는 미리 빌드한 Rust 실행 파일이라, 에이전트가 요청마다 스캔 스크립트를 새로 짜지 않고 이 CLI를 실행한다. 폴더 크기와 함께 프로그램을 마지막으로 쓴 시각을 알려 준다.

## 알려 주는 것

| 질문 | 명령 | 근거 |
|---|---|---|
| 공간이 어디에 있는가 | `scan` | 볼륨 통계, 루트의 직계 폴더별 합계 |
| 무엇이 커졌는가 | `diff` | 저장한 두 측정값 |
| 프로그램을 언제 마지막으로 썼는가 | `usage` | Prefetch, SRUM, UserAssist, ShellBags의 탐색기 폴더 흔적 |
| 큰 폴더 중 최근에 쓰지 않은 프로그램의 것은 무엇인가 | `activity` | 저장한 측정값과 프로그램 기록 |
| 어느 프로젝트가 Python 환경을 쓰는가 | `refs` | IDE 설정, 실행 스크립트, 환경 폴더 |
| 어떤 경로에서 무엇이 파일을 만들고 지우는가 | `watch` | Windows 디렉터리 변경 알림 |
| 다른 에이전트가 조사를 어떻게 이어받는가 | `export` | 상대경로로 묶은 메모, 결과, 진단 |

결과는 조사 폴더에 저장하고 `list`로 다시 찾는다. Prefetch와 SRUM은 관리자 권한이 필요하다. `collect`가 UAC 승인을 한 번 받아 사본을 만들고, 이후 조회는 일반 권한으로 사본을 읽는다.

명령, 입력, 결과 필드는 [Skill](SKILL.md)에 있다.

## 설치

zero-surplus는 Windows 10·11 x64에서 실행된다. 이 저장소는 Claude Code와 Codex의 플러그인이자 마켓플레이스이고, `bin/zero-surplus.exe`가 빌드된 CLI다.

### Claude Code

```text
claude plugin marketplace add GreenWojaKK/zero-surplus
claude plugin install zero-surplus@zero-surplus
```

플러그인이 켜져 있는 동안 `bin/`이 셸의 `PATH`에 올라간다. Skill은 요청에 맞을 때 실행되거나 `/zero-surplus:zero-surplus`로 부른다.

### Codex

```text
codex plugin marketplace add GreenWojaKK/zero-surplus
```

플러그인은 ChatGPT 데스크톱 앱의 플러그인 목록에서 설치한다. `bin\zero-surplus.exe`를 `PATH`에 있는 폴더로 복사하거나, 아래처럼 직접 빌드한다.

### 직접 빌드

빌드에는 Rust와 Visual Studio Build Tools 2022(MSVC)가 필요하다.

```text
cargo install --git https://github.com/GreenWojaKK/zero-surplus --locked
```

## 저장소

| 위치 | 역할 |
|---|---|
| `src` | CLI |
| `skills/zero-surplus` | Skill: 명령과 결과 읽는 법 |
| `bin` | 플러그인이 쓰는 CLI release 빌드 |
| `.claude-plugin` | Claude Code 플러그인과 마켓플레이스 매니페스트 |
| `plugin.json`, `.agents/plugins` | Codex 플러그인과 마켓플레이스 매니페스트 |
| `tests` | 통합 테스트와 fixture |

구현은 [아키텍처](architecture.md)에 있다.

## 라이선스

이 저장소의 코드는 [MIT License](../../LICENSE)를 따른다.

의존 라이브러리는 각자의 라이선스를 따른다: MIT, MIT 또는 Apache License 2.0, Unlicense 또는 MIT, Zlib, 그리고 encoding_rs 일부의 BSD-3-Clause. `bin/zero-surplus.exe`에 들어간 crate의 고지는 [THIRD-PARTY-NOTICES](../../THIRD-PARTY-NOTICES)에 있으며, 실행 파일을 배포할 때 함께 둔다.
