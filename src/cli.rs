//! Command and argument definitions.

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

#[derive(Parser)]
#[command(name = "zero-surplus", version, about = "Windows storage diagnosis for agents")]
pub struct Cli {
    /// Print indented JSON.
    #[arg(long, global = true)]
    pub pretty: bool,
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand)]
pub enum Command {
    /// Measure a volume or the direct children of a root folder and store a snapshot.
    Scan(ScanArgs),
    /// Compare two stored snapshots without touching the measured folders.
    Diff(DiffArgs),
    /// List the results stored in a case folder.
    List(ListArgs),
    /// Copy Prefetch files and the SRUM database into the case folder (needs elevation).
    Collect(CollectArgs),
    /// Look up Windows execution records for an app, or Explorer traces for a folder.
    Usage(UsageArgs),
    /// Connect Python environments to the projects that reference them.
    Refs(RefsArgs),
    /// List the folders of stored snapshots with the last use of the programs linked to each.
    Activity(ActivityArgs),
    /// Record create/delete/rename events under a path for a fixed time.
    Watch(WatchArgs),
    /// Bundle notes, results and diagnoses into a relocatable folder.
    Export(ExportArgs),
}

#[derive(Args)]
pub struct ScanArgs {
    /// Case folder that stores the result.
    #[arg(long, env = "ZERO_SURPLUS_CASE")]
    pub case: PathBuf,
    /// Drive to read volume statistics from, e.g. `C:`.
    #[arg(long)]
    pub volume: Vec<String>,
    /// Root folder whose direct children are measured.
    #[arg(long)]
    pub root: Vec<PathBuf>,
    /// Shortcut for the Local, LocalLow and Roaming AppData roots.
    #[arg(long)]
    pub appdata: bool,
    /// Number of rows to print; every row is stored.
    #[arg(long, default_value_t = 30)]
    pub top: usize,
    /// Number of largest files directly in the root to keep.
    #[arg(long, default_value_t = 20)]
    pub top_files: usize,
    /// Worker threads; defaults to the number of logical processors.
    #[arg(long)]
    pub threads: Option<usize>,
}

#[derive(Args)]
pub struct DiffArgs {
    #[arg(long, env = "ZERO_SURPLUS_CASE")]
    pub case: PathBuf,
    /// Earlier snapshot ID.
    pub before: String,
    /// Later snapshot ID.
    pub after: String,
    #[arg(long, default_value_t = 30)]
    pub top: usize,
}

#[derive(Args)]
pub struct ListArgs {
    #[arg(long, env = "ZERO_SURPLUS_CASE")]
    pub case: PathBuf,
    /// Only this kind: scan, volume, diff, usage, refs, watch, note, diagnosis, source.
    #[arg(long)]
    pub kind: Option<String>,
}

#[derive(Args)]
pub struct CollectArgs {
    #[arg(long, env = "ZERO_SURPLUS_CASE")]
    pub case: PathBuf,
    /// Internal: the elevated child writes its outcome here.
    #[arg(long, hide = true)]
    pub result_file: Option<PathBuf>,
}

#[derive(Args)]
pub struct UsageArgs {
    #[arg(long, env = "ZERO_SURPLUS_CASE")]
    pub case: Option<PathBuf>,
    /// Program name (`python.exe`) or full path.
    #[arg(long)]
    pub app: Option<String>,
    /// Folder whose Explorer traces (ShellBags) are looked up.
    #[arg(long)]
    pub folder: Option<PathBuf>,
    /// Read Prefetch files from this folder instead of the system or the case copy.
    #[arg(long)]
    pub prefetch_dir: Option<PathBuf>,
    /// Read this SRUDB.dat copy instead of the system or the case copy.
    #[arg(long)]
    pub srum_db: Option<PathBuf>,
    #[arg(long, default_value_t = 30)]
    pub top: usize,
}

#[derive(Args)]
pub struct RefsArgs {
    #[arg(long, env = "ZERO_SURPLUS_CASE")]
    pub case: Option<PathBuf>,
    /// A project folder.
    #[arg(long)]
    pub project: Vec<PathBuf>,
    /// A folder whose direct children are projects.
    #[arg(long)]
    pub projects_in: Vec<PathBuf>,
    /// A folder whose direct children are environments.
    #[arg(long)]
    pub envs_in: Vec<PathBuf>,
    /// A single environment folder.
    #[arg(long)]
    pub env: Vec<PathBuf>,
    /// An environment the user designates as shared.
    #[arg(long)]
    pub shared: Vec<PathBuf>,
    /// A launch script to inspect (never executed).
    #[arg(long)]
    pub script: Vec<PathBuf>,
    /// JetBrains SDK table to read instead of %APPDATA%\JetBrains\*\options\jdk.table.xml.
    #[arg(long)]
    pub jdk_table: Vec<PathBuf>,
    /// Skip execution-record lookup for environments.
    #[arg(long)]
    pub no_activity: bool,
    #[arg(long)]
    pub prefetch_dir: Option<PathBuf>,
    #[arg(long)]
    pub srum_db: Option<PathBuf>,
}

#[derive(Args)]
pub struct ActivityArgs {
    #[arg(long, env = "ZERO_SURPLUS_CASE")]
    pub case: PathBuf,
    /// Root whose newest stored snapshot is used.
    #[arg(long)]
    pub root: Vec<PathBuf>,
    /// Shortcut for the Local, Roaming and LocalLow AppData roots.
    #[arg(long)]
    pub appdata: bool,
    /// Folders smaller than this many bytes are left out.
    #[arg(long, default_value_t = 100 * 1024 * 1024)]
    pub min_bytes: u64,
    /// Number of rows to print; every row is stored.
    #[arg(long, default_value_t = 100)]
    pub top: usize,
    #[arg(long)]
    pub prefetch_dir: Option<PathBuf>,
    #[arg(long)]
    pub srum_db: Option<PathBuf>,
}

#[derive(Args)]
pub struct WatchArgs {
    #[arg(long, env = "ZERO_SURPLUS_CASE")]
    pub case: Option<PathBuf>,
    /// File or folder to watch; it does not have to exist yet.
    #[arg(long)]
    pub path: PathBuf,
    /// How long to watch, e.g. `90s`, `5m`, `1h`.
    #[arg(long)]
    pub duration: String,
}

#[derive(Args)]
pub struct ExportArgs {
    #[arg(long, env = "ZERO_SURPLUS_CASE")]
    pub case: PathBuf,
    /// Folder in which the bundle folder is created.
    #[arg(long)]
    pub out: PathBuf,
    /// Bundle folder name; defaults to a generated ID.
    #[arg(long)]
    pub name: Option<String>,
    /// UTF-8 note file to store verbatim.
    #[arg(long, num_args = 1..)]
    pub note_file: Vec<PathBuf>,
    /// Read one UTF-8 note from standard input.
    #[arg(long)]
    pub note_stdin: bool,
    /// Stored result IDs to include.
    #[arg(long, num_args = 1..)]
    pub include: Vec<String>,
    /// Earlier diagnosis IDs to include together with the evidence they cite.
    #[arg(long, num_args = 1..)]
    pub diagnosis: Vec<String>,
    /// New diagnosis text file, stored under a new ID.
    #[arg(long)]
    pub add_diagnosis: Option<PathBuf>,
    /// IDs the new diagnosis cites as evidence.
    #[arg(long, num_args = 1..)]
    pub cites: Vec<String>,
}
