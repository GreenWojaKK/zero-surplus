# zero-surplus Architecture

English | [한국어](docs-translate/ko/architecture.md)

zero-surplus is one Rust executable. Each command measures space or reads Windows records, prints one JSON document and stores the result in a case folder. The skill tells an agent which command answers which question and how to read the result.

## Components and responsibilities

| Component | Responsibility |
|---|---|
| [main.rs](src/main.rs), [cli.rs](src/cli.rs), [output.rs](src/output.rs) | Command dispatch, arguments, JSON output and exit codes |
| [case.rs](src/case.rs), [list.rs](src/list.rs) | Case folder layout, result IDs, atomic writes and listing |
| [win.rs](src/win.rs) | Volume statistics, path resolution, link checks, drive mappings, registry reads, elevation |
| [scan/](src/scan/mod.rs), [diff.rs](src/diff.rs) | Folder totals, snapshots and comparisons |
| [collect.rs](src/collect.rs) | Elevated copies of Prefetch and SRUM |
| [usage/](src/usage/mod.rs) | UserAssist, Prefetch, SRUM and ShellBags readers and source selection |
| [activity.rs](src/activity.rs) | Last use per folder of a snapshot |
| [refs/](src/refs/mod.rs) | Python environments and the projects that reference them |
| [watch.rs](src/watch.rs) | Change records under a path |
| [export.rs](src/export.rs) | Relocatable bundles of notes, results and diagnoses |

## Output and storage

Every command writes one JSON document to standard output; `watch` writes JSON Lines. Exit code 0 means a result, including a partial one; 1 an execution failure; 2 invalid input; 3 a request that cannot be carried out in the current state, such as incomparable snapshots, missing references, declined elevation or a root that is a link.

A stored result gets an ID of the form `<kind>-<UTC time>-<random>` and is written under a temporary name, then renamed in the same directory. `list` shows only results with their final name, so a cancelled measurement never becomes a comparison baseline.

```text
<case>/
  scans/<id>.json          root snapshots (scan-) and volume measurements (vol-)
  diffs/ usage/ refs/ activity/   <id>.json
  watches/<id>.jsonl
  notes/<id>.txt           note text
  diagnoses/<id>.md        diagnosis text, with the cited IDs in <id>.json
  sources/<id>/            collect copies and source.json
```

## Measuring space

`scan --volume` reads total, free and caller-available bytes from Windows. `scan --root` measures the direct children of a root with [dua-core](https://crates.io/crates/dua-core) 6.1.0, whose calls are kept in [scan/walk.rs](src/scan/walk.rs). dua-core enumerates with `GetFileInformationByHandleEx(FileIdBothDirectoryInfo)`, which returns logical and allocated sizes with each entry, and reports junctions and symbolic links without entering them.

The root's direct entries are read first. Files add to a root-files row that keeps only the largest names; each direct child folder becomes one numbered walk root, and entries are summed per row as they arrive. Memory holds per-row totals and failures only.

| Rule | Behavior |
|---|---|
| Size | Logical size (file lengths) ranks rows and drives `diff`; allocated size is stored beside it |
| Hard links | Counted at every path |
| Links | Listed as link rows with no size; a root that is itself a link is refused |
| Failures | Path and Windows error code per failure; a row with any failure is `partial` |
| Names | `names_complete` records whether the root's children were listed to the end |
| Case folder | Skipped and recorded in `excluded` when it lies below the root |

A snapshot also stores the volume GUID path and serial number, the input and resolved root paths, start and end times, free bytes at both times, the rules and run statistics (files, folders, elapsed time, peak memory, threads).

`diff` opens only the two stored files. It first compares volume, root, rules and exclusions and refuses different ones. Rows are then compared as follows.

| State of the two measurements | Result |
|---|---|
| The folder exists in both and both rows are complete | Later minus earlier |
| Both listings are complete and the name exists on one side only | `appeared` or `disappeared`; the change is computed only when that row is complete |
| Part of a folder was not read, or a name's presence is uncertain | Bytes read are kept and `delta_bytes` is `null` |

The free-space change between the two measurement times is reported with both times.

## Windows records

| Source | Reading |
|---|---|
| UserAssist | Current-user registry. Value names are ROT13-decoded and known-folder GUIDs resolved. 72-byte records give run count (offset 4), focus count (8), focus time in ms (12) and last run (60). [userassist.rs](src/usage/userassist.rs) |
| Prefetch | The MAM header (8 bytes, 12 with a CRC) is removed and the data decompressed with `RtlDecompressBufferEx`; [frnsc-prefetch](https://crates.io/crates/frnsc-prefetch) reads the plain SCCA data. Volume serial numbers map referenced paths to drive letters. [prefetch.rs](src/usage/prefetch.rs) |
| SRUM | The copy is opened read-only with the Windows ESE engine (`esent.dll`) with recovery off. `SruDbIdMapTable` supplies app identifiers and user SIDs; the app resource and network tables are summed per app and per local day. Identifiers that differ only in letter case are merged. [srum.rs](src/usage/srum.rs) |
| ShellBags | The two BagMRU trees of the current user. [winparsingtools](https://crates.io/crates/winparsingtools) classifies shell items, and long names are read from the `0xBEEF0004` extension block. [shellbags.rs](src/usage/shellbags.rs) |

A Prefetch record's executable path comes from the referenced files with the executable's name; for a name cut at 29 characters, files that start with it and end in `.EXE`, `.COM` or `.SCR`. One candidate is the path. Among several, the one whose device path hashes to the hash in the Prefetch file name is chosen: start at 314159 and, for every byte of the upper-case UTF-16LE path, take `hash * 37 + byte` modulo 2^32. Programs whose hash also covers the command line (service hosts, packaged and multi-process apps) leave several candidates, which are returned as `path_candidates`.

The Prefetch decompression and the ShellBags long names are local code next to the libraries' own paths. When a library is updated, compare its output on the inputs this code handles.

`collect` restarts itself through a UAC prompt when not elevated and waits for the elevated process. That process copies the Prefetch files and the SRUM database and writes `source.json` with the copy time, method, database state, readability and the drive mappings. SRUM is copied with a shared read, or with `esentutl /y /vss` when the file is locked. Each SRUM copy is opened as the readers would open it; an unreadable copy is taken again after 5 seconds, up to 6 times, and is kept with `readable: false` and the reason when no attempt succeeds.

Each source is selected per lookup.

| Source | Order |
|---|---|
| Prefetch | `--prefetch-dir`, the running system, the newest copy in the case |
| SRUM | `--srum-db` only, otherwise the case copies from newest to oldest until one can be read (passed-over copies are returned with their reasons), otherwise the running system |

`usage`, `refs` and `activity` share this selection. The ESE page size is a process-wide setting, so it is set once per process.

## Last use per folder

`activity` reads every run in UserAssist, Prefetch and SRUM once and links each run to the folder rows of stored snapshots by the executable's path.

| Link | Condition |
|---|---|
| `inside` | The executable lies inside the folder |
| `exact` | The executable's name without extension is the folder name or one of its words |
| `program` | A word of the folder name is a word of the executable's name |
| `folder` | A word of the folder name is a word of one of the executable's parent folders |

A folder's last use is the latest run within its strongest link. Names split into words at separators, at lower-to-upper case changes and between letters and digits; parts shorter than four characters are kept only as whole words. Generic words (`Microsoft`, `Programs`, `Temp`, `Cache` and others in `GENERIC`) do not link. The evidence names the run's program and source, since `program` and `folder` can reach another program of the same vendor.

## Python environments

[refs/envs.rs](src/refs/envs.rs) identifies environments by `pyvenv.cfg` or `conda-meta` in the given folders and in each project's `.venv` and `venv`, and groups paths that resolve to the same folder. [refs/projects.rs](src/refs/projects.rs) reads interpreter references from VS Code settings, JetBrains project files and the SDK table, the given launch scripts (tokenized, never run) and `environment.yml`. [refs/resolve.rs](src/refs/resolve.rs) resolves a reference within its project's file system: `${workspaceFolder}`, `%~dp0` and the home folder are substituted, POSIX paths resolve inside the project's WSL distribution, and values decided at run time stay unresolved.

[refs/mod.rs](src/refs/mod.rs) ties each reference to one environment or records why it cannot: version only, run-time value, several environments with the name, outside the scope. Sizes come from the case folder's snapshots, and activity from the Prefetch and SRUM records of the environment's interpreters. Environments with no reference and no shared designation are listed by size.

## Watching paths

[watch.rs](src/watch.rs) registers `ReadDirectoryChangesW` on the nearest existing parent with the subtree, receives file-name and directory-name changes and keeps those for the target and below. A zero-byte completion or `ERROR_NOTIFY_ENUM_DIR` is reported as a gap from the last received batch to that moment. Events go to standard output and to a temporary log in the case folder. The log stops at its first write error; at the end it is flushed and renamed, and when that fails the temporary log is removed, the `end` line carries `store_error` and the exit code is 1. Windows change notifications carry no process, so `process` is `null`.

## Handing over an investigation

[export.rs](src/export.rs) stores notes unchanged and a new diagnosis under a new ID with the IDs it cites. Before writing anything it gathers the requested IDs and, through each diagnosis, the IDs it cites; unknown IDs and unreadable citation lists stop the export. The bundle is built under a temporary name, gets a `manifest.json` with relative paths and is renamed. An existing bundle of the same name is left untouched.

## Plugin packaging

The repository root is the plugin and the marketplace. Claude Code reads [.claude-plugin/plugin.json](.claude-plugin/plugin.json) and [.claude-plugin/marketplace.json](.claude-plugin/marketplace.json), which lists the plugin at `./`. Codex reads the root [plugin.json](plugin.json) in the Agent Plugins format and [.agents/plugins/marketplace.json](.agents/plugins/marketplace.json). Both find the skill under `skills/`. `bin/zero-surplus.exe` is the release build; Claude Code puts `bin/` on the shell's `PATH` while the plugin is enabled, and Codex users put the executable on `PATH` themselves.

## Verification

Integration tests run the built executable against small fixtures.

| Tests | Checks |
|---|---|
| [scan_diff](tests/scan_diff.rs) | Sizes, junctions, hard links and access-denied folders; comparisons, refused comparisons, cancelled results and the excluded case folder |
| [refs](tests/refs.rs) | Shared environments, aliases, unresolved references, candidate order and per-source activity states |
| [activity](tests/activity.rs) | Each link kind, a stronger link winning over a later weaker one, generic names and a root without a snapshot |
| [watch_export](tests/watch_export.rs) | Delete and recreate, watching a missing target, log write failure and name collision, moved bundles, missing IDs, damaged citation lists |
| [prefetch_reference](tests/prefetch_reference.rs) | Five [Prefetch fixtures](tests/fixtures/prefetch) against PECmd output of the same files |
| [srum_copies](tests/srum_copies.rs) | SRUM-shaped databases built with the ESE engine: passing over a dirty copy, every copy failing, an explicit file, per-app and per-day totals |

Prefetch and SRUM readers were compared with PECmd and SrumECmd on one machine's records: all 435 Prefetch files match in executable name, run count, the eight run times, executable path and referenced files, and 27 sampled SRUM apps match in 12 counters and times. The comparison and its open items are recorded in [working/implementation.md](working/implementation.md).

Run from the repository root:

```text
cargo test
```
