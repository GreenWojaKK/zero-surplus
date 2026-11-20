//! Turning an interpreter reference as written in a project file into a path.
//!
//! A reference is resolved inside the file system its project lives in: a project under a WSL
//! share resolves POSIX paths inside that distribution, a project on a Windows drive cannot
//! resolve them. Nothing is executed; a reference whose value is only known at run time stays
//! unresolved.

use std::path::{Path, PathBuf};

/// File system a project lives in.
#[derive(Debug, PartialEq)]
pub enum Flavor {
    Windows,
    /// `\\wsl.localhost\<distro>` or `\\wsl$\<distro>`.
    Wsl { share: String },
}

pub fn flavor_of(project: &Path) -> Flavor {
    let text = project.to_string_lossy().replace('/', "\\");
    let lower = text.to_lowercase();
    for prefix in [r"\\wsl.localhost\", r"\\wsl$\"] {
        if let Some(rest) = lower.strip_prefix(prefix) {
            let distro_len = rest.find('\\').unwrap_or(rest.len());
            return Flavor::Wsl { share: text[..prefix.len() + distro_len].to_owned() };
        }
    }
    Flavor::Windows
}

/// Why a reference could not be turned into a path.
pub const RUNTIME: &str = "path decided at run time";
pub const WSL_FROM_WINDOWS: &str = "WSL path in a Windows project";
pub const BARE_COMMAND: &str = "command name without a path";

/// Variables a file format defines without running anything.
pub struct Known<'a> {
    /// `(placeholder, value)`, e.g. `("${workspaceFolder}", "C:\proj")`.
    pub substitutions: &'a [(&'a str, String)],
    pub home: Option<&'a Path>,
}

fn has_runtime_variable(text: &str) -> bool {
    // ${...}, $env:NAME, $NAME, %NAME%, !NAME!
    let bytes = text.as_bytes();
    let percent_pair = text.matches('%').count() >= 2;
    let bang_pair = text.matches('!').count() >= 2;
    let dollar = bytes.windows(2).any(|pair| pair[0] == b'$' && (pair[1] == b'{' || pair[1].is_ascii_alphabetic()));
    percent_pair || bang_pair || dollar
}

fn is_windows_absolute(text: &str) -> bool {
    let bytes = text.as_bytes();
    (bytes.len() >= 3 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' && (bytes[2] == b'\\' || bytes[2] == b'/'))
        || text.starts_with(r"\\")
}

/// Resolve `raw` against `base` (the folder relative paths start from).
pub fn resolve(raw: &str, base: &Path, flavor: &Flavor, known: &Known) -> Result<PathBuf, &'static str> {
    let mut text = raw.trim().trim_matches(['"', '\'']).to_owned();
    if text.is_empty() {
        return Err(BARE_COMMAND);
    }
    for (placeholder, value) in known.substitutions {
        if let Some(position) = text.to_lowercase().find(&placeholder.to_lowercase()) {
            text.replace_range(position..position + placeholder.len(), value);
        }
    }
    if let Some(rest) = text.strip_prefix('~')
        && (rest.is_empty() || rest.starts_with(['/', '\\']))
    {
        match (flavor, known.home) {
            (Flavor::Windows, Some(home)) => text = format!("{}{rest}", home.to_string_lossy()),
            // The Linux home folder is not known without running the distribution.
            _ => return Err(RUNTIME),
        }
    }
    if has_runtime_variable(&text) {
        return Err(RUNTIME);
    }

    if is_windows_absolute(&text) {
        return Ok(PathBuf::from(text.replace('/', "\\")));
    }
    if let Some(posix) = text.strip_prefix('/') {
        return match flavor {
            Flavor::Windows => Err(WSL_FROM_WINDOWS),
            Flavor::Wsl { share } => {
                // /mnt/c/Users/x is the Windows drive C seen from inside WSL.
                let mut parts = posix.splitn(3, '/');
                if parts.next() == Some("mnt")
                    && let Some(drive) = parts.next().filter(|drive| drive.len() == 1)
                {
                    let rest = parts.next().unwrap_or("").replace('/', "\\");
                    return Ok(PathBuf::from(format!("{}:\\{rest}", drive.to_uppercase())));
                }
                Ok(PathBuf::from(format!("{share}\\{}", posix.replace('/', "\\"))))
            }
        };
    }
    if !text.contains(['/', '\\']) {
        // `python`, `py`, `python3`: found through PATH when the command runs.
        return Err(BARE_COMMAND);
    }
    Ok(base.join(text.replace('/', "\\")))
}

/// Collapse `.` and `..` without touching the file system.
pub fn normalize(path: &Path) -> PathBuf {
    std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn known<'a>(substitutions: &'a [(&'a str, String)]) -> Known<'a> {
        Known { substitutions, home: Some(Path::new(r"C:\Users\me")) }
    }

    #[test]
    fn workspace_folder_is_replaced_by_the_project_root() {
        let root = Path::new(r"C:\proj\a");
        let substitutions = [("${workspaceFolder}", root.to_string_lossy().into_owned())];
        let path =
            resolve("${workspaceFolder}/.venv/Scripts/python.exe", root, &Flavor::Windows, &known(&substitutions));
        assert_eq!(path, Ok(PathBuf::from(r"C:\proj\a\.venv\Scripts\python.exe")));
    }

    #[test]
    fn relative_paths_start_from_the_base_folder() {
        let path = resolve(r".venv\Scripts\python.exe", Path::new(r"C:\proj\a"), &Flavor::Windows, &known(&[]));
        assert_eq!(path, Ok(PathBuf::from(r"C:\proj\a\.venv\Scripts\python.exe")));
    }

    #[test]
    fn home_is_expanded_only_on_windows() {
        let path = resolve("~/envs/x/python.exe", Path::new(r"C:\p"), &Flavor::Windows, &known(&[]));
        assert_eq!(path, Ok(PathBuf::from(r"C:\Users\me\envs\x\python.exe")));
        let wsl = Flavor::Wsl { share: r"\\wsl.localhost\Ubuntu".to_owned() };
        assert_eq!(resolve("~/envs/x/bin/python", Path::new(r"\\wsl.localhost\Ubuntu\p"), &wsl, &known(&[])), Err(RUNTIME));
    }

    #[test]
    fn runtime_variables_stay_unresolved() {
        for raw in [r"%CONDA_PREFIX%\python.exe", "${env:VIRTUAL_ENV}/bin/python", r"$env:VENV\Scripts\python.exe", "$HOME/x/python"] {
            assert_eq!(resolve(raw, Path::new(r"C:\p"), &Flavor::Windows, &known(&[])), Err(RUNTIME), "{raw}");
        }
    }

    #[test]
    fn bare_commands_are_not_paths() {
        assert_eq!(resolve("python", Path::new(r"C:\p"), &Flavor::Windows, &known(&[])), Err(BARE_COMMAND));
    }

    #[test]
    fn posix_paths_resolve_inside_the_projects_distribution() {
        let project = Path::new(r"\\wsl.localhost\Ubuntu\home\me\proj");
        let flavor = flavor_of(project);
        assert_eq!(flavor, Flavor::Wsl { share: r"\\wsl.localhost\Ubuntu".to_owned() });
        assert_eq!(
            resolve("/home/me/envs/x/bin/python", project, &flavor, &known(&[])),
            Ok(PathBuf::from(r"\\wsl.localhost\Ubuntu\home\me\envs\x\bin\python"))
        );
        assert_eq!(
            resolve("/mnt/c/Users/me/envs/x/python.exe", project, &flavor, &known(&[])),
            Ok(PathBuf::from(r"C:\Users\me\envs\x\python.exe"))
        );
    }

    #[test]
    fn posix_paths_cannot_be_resolved_from_a_windows_project() {
        assert_eq!(flavor_of(Path::new(r"C:\proj")), Flavor::Windows);
        assert_eq!(
            resolve("/usr/bin/python3", Path::new(r"C:\proj"), &Flavor::Windows, &known(&[])),
            Err(WSL_FROM_WINDOWS)
        );
    }
}
