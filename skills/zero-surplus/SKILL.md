---
name: zero-surplus
description: Commands and result fields of zero-surplus, a Windows storage diagnosis CLI. Use it to find what takes disk space, why space grew, whether a program or Python environment (venv, conda) was used recently, which projects use which environment, and when files under a path are created or deleted. Run this CLI before writing any scan script of your own.
---

# zero-surplus

`zero-surplus` is an executable on PATH. It measures, reads Windows records and returns the evidence. It never deletes or moves files. Deciding what to investigate and why space grew is up to the agent.

## Common

- Every command writes one JSON document to standard output; `watch` writes one event per line. Progress messages go to standard error. `--pretty` indents the JSON.
- Exit codes: `0` result produced (including partial results), `1` execution failure, `2` invalid input, `3` cannot be done in the current state. On a nonzero code the output is `{"error":{"kind","message","detail"}}`.
- `--case <folder>` is the case folder that stores results; `ZERO_SURPLUS_CASE` sets it too. A stored result returns its `id` and `stored` (path relative to the case folder).
- Times are RFC 3339 with the local UTC offset. Sizes are bytes.
- Output lists the largest `--top <N>` rows and sums up the rest. The stored file keeps every row.

```powershell
zero-surplus scan --case C:\cases\disk --volume C: --root C:\Users\me\AppData\Local
zero-surplus list --case C:\cases\disk
```

## scan — measure space

| Input | Meaning |
|---|---|
| `--volume C:` | Total, used and free space of the drive from Windows volume statistics. |
| `--root <path>` | Totals of each direct child folder of the root. Repeatable; one snapshot per root. |
| `--appdata` | The `Local`, `LocalLow` and `Roaming` roots. |
| `--top <N>` | Folder rows to print (default 30). |
| `--top-files <N>` | Largest files directly in the root to keep (default 20). |

`volumes[]` items (ID prefix `vol-`): `total_bytes`, `used_bytes`, `free_bytes` (whole volume), `free_bytes_caller` (available to the current user), `volume`, `measured_at`.

`snapshots[]` items (ID prefix `scan-`):

| Field | How to read it |
|---|---|
| `dir_rows[]` | Direct child folders. `bytes` is the logical size (sum of file lengths), `allocated_bytes` the space allocated on disk. Rows are ranked by `bytes`. |
| `status` | `complete`: read to the end. `partial`: part of the folder could not be read, and `bytes` covers what was read. |
| `failures_sample[]`, `failures_total` | Locations that could not be read; everything below `path` is missing from the totals. `code` is the Windows error code (5 is access denied). |
| `root_files` | Total and largest files (`top`) directly in the root. |
| `links_not_followed[]` | Junctions and symbolic links. They are not entered and add no size. |
| `names_complete` | Whether the root's child names were listed to the end. `false` means folders may be missing. |
| `dir_rows_not_shown` | Count, total and partial count of the rows left out of the output. |
| `excluded[]` | Paths skipped on purpose (the case folder when it lies below the root). |
| `free_bytes_start`, `free_bytes_end` | Volume free space when the measurement started and ended. |
| `rules` | Aggregation rules. A hard-linked file counts at each of its paths. |

When reading the numbers:

- A folder's `bytes` is the space its files occupy by length, which can differ from the space deleting it frees when folders share data. Check the volume's free space before and after a cleanup for the space actually freed.
- Rows of roots that contain one another do not add up, and the sum of logical sizes is not the volume's used space.
- A directory entry can show a stale size while the file is open.

Error `kind`s: `root_is_link` (the root is a link and is not measured), `root_unreadable`, `cancelled` (nothing stored).

## diff — compare two stored results

```powershell
zero-surplus diff --case C:\cases\disk <earlier ID> <later ID>
```

Takes two `scan-` IDs or two `vol-` IDs. Reads only the two stored files; the folders are not measured again.

| Field | How to read it |
|---|---|
| `rows[].state` | `changed`, `appeared`, `disappeared`, `unknown` (cannot be computed). Unchanged rows are counted in `rows_unchanged` instead of printed. |
| `rows[].delta_bytes` | Later minus earlier. `null` when one side was not read completely or a name's presence is uncertain; `before.bytes` and `after.bytes` still hold the values read. |
| `free_space_change` | Free-space change. `delta_bytes` includes every other write and delete between the two measurement times (`before_at`, `after_at`). |

When the two results were measured under different conditions (volume, root, rules, exclusions), the command exits with code 3 and `not_comparable`, listing the differences in `detail.mismatches[]`.

## list — find stored results

```powershell
zero-surplus list --case C:\cases\disk --kind scan
```

`items[]`: `id`, `kind`, `at`, `target`, `path`. `--kind` is one of `scan`, `volume`, `diff`, `usage`, `refs`, `activity`, `watch`, `note`, `diagnosis`, `source`. Only completely stored results are listed.

## collect — copy the records that need administrator rights

```powershell
zero-surplus collect --case C:\cases\disk
```

Copies the Prefetch files and the SRUM database into `sources/<id>/` in the case folder. A UAC prompt appears on the user's screen and the command waits for approval; when it is declined, the command exits with code 3 and `elevation_declined`. Afterwards `usage`, `refs` and `activity` read the copies without elevation. A copy holds the records as of `collected_at`.

Output: `prefetch.files` (files copied), `srum.readable`, `srum.database_state`, `srum.attempts`, `drives[]`.

A SRUM copy taken while the service writes to the database cannot be read (`DirtyShutdown`). `collect` reads each copy back and copies again after 5 seconds when it cannot be read, up to 6 times. When no attempt succeeds, `srum.readable` is `false` and `srum.reason` says why; lookups pass over that copy.

## usage — program records

```powershell
zero-surplus usage --case C:\cases\disk --app python.exe
zero-surplus usage --case C:\cases\disk --app "C:\Users\me\anaconda3\envs\FAI\python.exe"
zero-surplus usage --folder C:\Users\me\Documents
```

`--app` is a file name or a full path. A name returns every location with that name, one record per path. `--folder` looks up Explorer traces of that folder.

Each source in `sources` has its own `status`; a source that cannot be read leaves the others intact.

| `status` | Meaning |
|---|---|
| `ok` | The records were read and contain the target. |
| `no_record` | The records were read and do not contain the target. The program may still have run before the records begin. |
| `access_denied` | No permission. For Prefetch and SRUM, run `collect`. |
| `source_absent` | The records do not exist. |
| `unsupported_format` | The records exist in a format or state that cannot be read; `message` says why. |

`origin` tells where the data came from: `live` (the running system), `copy` (a `collect` copy, with `collected_at`), `explicit` (`--prefetch-dir`, `--srum-db`).

SRUM copies are tried from newest to oldest and the first readable copy is used (`origin.selection`: `newest_readable_copy`). Newer copies passed over are listed in `skipped_copies[]` with `source`, `collected_at`, `database_state` and `reason`; the result then covers records up to that copy's `origin.collected_at`. When no copy can be read, the status is `unsupported_format` and `skipped_copies[]` lists every copy tried. `activity_sources.srum` in `refs` uses the same fields.

| Source | Record fields | How to read it |
|---|---|---|
| `userassist` | `last_run`, `run_count`, `focus_count`, `focus_ms`, `group` | Programs started from Explorer or the Start menu. `last_run` can be set while the counts are 0, so read the focus values too. `group` separates executables (`executable`) from shortcuts (`shortcut`). |
| `prefetch` | `last_run`, `last_runs[]` (up to 8), `run_count`, `path`, `path_basis`, `path_candidates`, `referenced_count` | Programs started any way, terminals included. Paths are stored in upper case. `last_runs` is newest first. `path` is set when the executable's path can be told: `path_basis` is `only_candidate` when one referenced file has the executable's name, or `file_name_hash` when the Prefetch file name's hash picks one of several. Otherwise `path` is `null` and the candidates are in `path_candidates`; a path query also returns records with that path among their candidates. `refs` uses only records with a `path`. The files referenced at start-up (`referenced`) are in the stored result file. |
| `srum` | `observed_from`, `observed_to`, `active_days`, `totals`, `days[]` | Use recorded about once an hour: CPU (`*_cycles`, CPU cycles), disk (`*_bytes_read/written`) and network (`bytes_sent/received`). Use after the last recording is not in the database yet. |
| `shellbags` | `path`, `key_last_write`, `folder_itself_recorded` | Folders opened in Explorer. `key_last_write` is when an entry below that folder was last added or reordered. |

## activity — last use per folder

```powershell
zero-surplus activity --case C:\cases\disk --appdata
zero-surplus activity --case C:\cases\disk --root C:\Users\me --min-bytes 1073741824
```

Adds, to each folder row of a stored snapshot, the last run of the programs linked to that folder. Use it to find large folders of programs that have not been used lately. Measure the roots with `scan --root` (or `--appdata`) first; a root without a snapshot exits with code 3 and `no_snapshot`. Program records come from the same sources as `usage`, with the same copy selection.

| Input | Meaning |
|---|---|
| `--root <path>`, `--appdata` | Use the newest snapshot of this root. |
| `--min-bytes <N>` | Leave out folders smaller than this (default 100 MiB). |
| `--top <N>` | Rows to print (default 100). The stored file keeps every row. |

`rows[]` is ordered by size.

| Field | How to read it |
|---|---|
| `folder`, `bytes`, `status` | The snapshot's folder row. |
| `last_use` | The latest run within the strongest kind of link the folder has. |
| `evidence` | `program` (executable path or name), `source`, `time`, `link`. |
| `link` | Strongest first: `inside` (the executable lies inside the folder), `exact` (the executable's name is the folder name or one of its words: `Code` and `Code.exe`), `program` (a word of the folder name is in the executable's name: `Kakao` and `KakaoTalk.exe`), `folder` (a word of the folder name is in a parent folder of the executable: `PreSonus` and `...\PreSonus\Studio One 6\Studio One.exe`). `program` and `folder` can link another program of the same vendor, such as an updater, so check `evidence.program`. |
| `programs_linked` | Number of distinct programs linked to the folder. |
| `name_words` | Words of the folder name used for linking. Generic words such as `Microsoft`, `Programs`, `Temp` and `Cache` are left out. |

`last_use` is `null` in two cases: the folder's program has not run within the period the records cover, or no program can be told from the folder name (a tool run as `python -m` like `pip`, or a folder many programs share). The covered period is in `coverage`: `srum.observed_from` (oldest SRUM record) and `prefetch.oldest_run` (oldest Prefetch run).

## refs — projects and Python environments

```powershell
zero-surplus refs --case C:\cases\disk --projects-in C:\Users\me\Documents --envs-in C:\Users\me\anaconda3\envs --shared C:\Users\me\anaconda3
```

| Input | Meaning |
|---|---|
| `--project <folder>` | One project. |
| `--projects-in <folder>` | Each direct child folder is a project. |
| `--envs-in <folder>` | Direct child folders that are environments. |
| `--env <path>` | One environment. |
| `--shared <path>` | An environment the user designates as shared; it stays off the candidate list. |
| `--script <file>` | A launch script to read; it is never run. |
| `--no-activity` | Skip the program-record lookup. |

`.venv` and `venv` at each project root are environments too. References are read from VS Code settings (`python.defaultInterpreterPath`), JetBrains SDK names, the interpreter, `activate` and `conda activate` lines of the given scripts, and the `name` in `environment.yml`.

| Field | How to read it |
|---|---|
| `environments[]` | One row per environment. `referenced_by_projects` lists the projects that point to it and `references[]` the evidence (file and key). Other paths to the same folder are grouped in `aliases`. |
| `size`, `size_state` | Size from the case folder's snapshots. `unmeasured` means the environment's parent folder has not been measured with `scan --root`. |
| `activity`, `last_activity` | Prefetch and SRUM records of the environment's Python executables. `activity.prefetch.projects` lists the projects whose files the interpreter opened at start-up. Source states are in `activity_sources`. |
| `unreferenced[]` | Environments no project points to and not designated as shared, largest first; those without a size come last. `located_in_project` names the project folder the environment sits in. |
| `unresolved_references[]` | References that cannot be tied to one environment, with the `reason`: version only, a path decided at run time, several environments with the same name. They are left unresolved. |
| `references_outside_scope[]` | References to interpreters outside the investigated scope. |
| `scope` | The investigated projects and environment folders, and projects that could not be read (`unreadable`). `unreferenced` holds within this scope. |

## watch — record changes under a path

```powershell
zero-surplus watch --case C:\cases\disk --path C:\Users\me\AppData\Local\SomeCache --duration 90s
```

Records creations, deletions and renames of the target and everything below it for the given time (`s`, `m`, `h`), then exits. The target may not exist yet. The command runs for the whole duration, so start it in the background; only changes after the `ready` line are recorded.

| Line (`event`) | Fields |
|---|---|
| `ready` | Registration time `time`, target `path`, the parent folder actually watched `watching`, `target_exists`. |
| `created`, `deleted`, `renamed` | `time` (when the notification arrived), `path`, and `old_path` for renames. `process` is always `null`: Windows change notifications do not name the program that wrote. |
| `gap` | Changes from `from` to `to` were missed. Treat that interval as unknown. |
| `end` | `reason`, `target_exists`, number of events `events`, number of gaps `gaps`, stored location `stored`. |

When the log cannot be stored in the case folder, `end` has `stored: null` and `store_error`, the last line is a `log_not_stored` error and the exit code is 1. The events printed to standard output remain valid. A log that could not be stored is removed.

## export — bundle an investigation

```powershell
zero-surplus export --case C:\cases\disk --out C:\handover --note-file memo.txt --include scan-... --add-diagnosis diagnosis.md --cites scan-... diff-...
zero-surplus export --case C:\cases\disk --out C:\handover --diagnosis diag-...
```

| Input | Meaning |
|---|---|
| `--note-file <file>`, `--note-stdin` | The user's note as UTF-8 text, stored unchanged. |
| `--include <ID>` | Stored results to include. |
| `--add-diagnosis <file>` `--cites <ID>...` | Store a new diagnosis under a new ID together with the IDs it cites as evidence. |
| `--diagnosis <ID>` | An earlier diagnosis; the evidence it cites comes along. |
| `--out <folder>`, `--name <name>` | Where to create the bundle folder and its name. |

The bundle's `manifest.json` lists `items[]` (`kind`, `id`, `path`, and `cites` for diagnoses) with paths relative to the bundle, so the folder opens wherever it is moved. `collect` copies are not included.

When an ID is not in the case folder, no bundle is created and the command exits with code 3 and `missing_references` (`detail.missing[]`). When a diagnosis's cited-ID list (`diagnoses/<id>.json`) cannot be read, it exits with code 3 and `unreadable_citations` (`detail.unreadable[]` with `diagnosis` and `message`). An existing bundle with the same name is left untouched and the command returns `bundle_exists`.
