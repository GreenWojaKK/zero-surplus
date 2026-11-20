# zero-surplus

English | [한국어](docs-translate/ko/README.md)

zero-surplus is a Windows storage diagnosis CLI with a skill for coding agents. Claude Code and Codex run it to find where disk space goes, whether the programs behind large folders are still in use, and why space grows again after a cleanup. It returns measurements and Windows records as evidence and leaves every file in place.

The CLI is a prebuilt Rust executable, so an agent calls it instead of writing a scan script for each request. Next to folder sizes it reports how recently programs were used.

## What it reports

| Question | Command | Evidence |
|---|---|---|
| Where is the space? | `scan` | Volume statistics, and totals of each direct child folder of a root |
| What grew? | `diff` | Two stored measurements |
| When was a program last used? | `usage` | Prefetch, SRUM and UserAssist; Explorer folder traces from ShellBags |
| Which large folders belong to programs not used lately? | `activity` | Stored measurements joined with the program records |
| Which projects use a Python environment? | `refs` | IDE settings, launch scripts and environment folders |
| What creates and deletes files under a path? | `watch` | Windows directory change notifications |
| How does another agent continue the investigation? | `export` | Notes, results and diagnoses bundled with relative paths |

Results are stored in a case folder and found again with `list`. Prefetch and SRUM need administrator rights: `collect` copies them once after a UAC prompt, and later lookups read the copies without elevation.

Commands, inputs and result fields are in the [skill](skills/zero-surplus/SKILL.md).

## Install

zero-surplus runs on Windows 10 and 11 x64. This repository is a plugin and a marketplace for Claude Code and Codex, and `bin/zero-surplus.exe` is the built CLI.

### Claude Code

```text
claude plugin marketplace add GreenWojaKK/zero-surplus
claude plugin install zero-surplus@zero-surplus
```

While the plugin is enabled, `bin/` is on the shell's `PATH`. The skill runs when a request matches it, or as `/zero-surplus:zero-surplus`.

### Codex

```text
codex plugin marketplace add GreenWojaKK/zero-surplus
```

Install the plugin from the plugin directory of the ChatGPT desktop app. Copy `bin\zero-surplus.exe` to a folder on `PATH`, or build it as below.

### Build from source

Building requires Rust and Visual Studio Build Tools 2022 (MSVC).

```text
cargo install --git https://github.com/GreenWojaKK/zero-surplus --locked
```

## Repository

| Location | Role |
|---|---|
| `src` | CLI |
| `skills/zero-surplus` | Skill: commands and how to read the results |
| `bin` | Release build of the CLI used by the plugin |
| `.claude-plugin` | Claude Code plugin and marketplace manifests |
| `plugin.json`, `.agents/plugins` | Codex plugin and marketplace manifests |
| `tests` | Integration tests and fixtures |

The implementation is described in [Architecture](architecture.md).

## License

The code in this repository is licensed under the [MIT License](LICENSE).

Dependencies follow their own licenses: MIT, MIT or Apache License 2.0, Unlicense or MIT, Zlib, and BSD-3-Clause for part of encoding_rs. [THIRD-PARTY-NOTICES](THIRD-PARTY-NOTICES) holds the notices of the crates built into `bin/zero-surplus.exe`; keep it with the executable when distributing it.
