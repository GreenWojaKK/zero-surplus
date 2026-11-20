//! Reading interpreter references out of project files and launch scripts.
//!
//! Only the files are read. Scripts are tokenised, never executed.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use quick_xml::Reader;
use quick_xml::events::Event;
use serde_json::Value;

use super::resolve::{self, Flavor, Known};
use crate::win;

pub const VERSION_ONLY: &str = "version only";
pub const SDK_NOT_FOUND: &str = "no path found for this SDK name";
pub const SDK_AMBIGUOUS: &str = "several paths for this SDK name";

pub enum Target {
    /// Path of an interpreter, an activate script's environment, or an environment folder.
    Path(PathBuf),
    /// Name of a conda environment.
    CondaName(String),
    Unresolved(&'static str),
}

pub struct RawRef {
    /// File the reference was read from.
    pub source: PathBuf,
    /// Setting or statement that holds the reference.
    pub key: String,
    /// Value as written.
    pub raw: String,
    pub target: Target,
}

fn home() -> Option<PathBuf> {
    std::env::var_os("USERPROFILE").map(PathBuf::from)
}

fn path_target(raw: &str, base: &Path, flavor: &Flavor, substitutions: &[(&str, String)]) -> Target {
    let home = home();
    let known = Known { substitutions, home: home.as_deref() };
    match resolve::resolve(raw, base, flavor, &known) {
        Ok(path) => Target::Path(resolve::normalize(&path)),
        Err(reason) => Target::Unresolved(reason),
    }
}

// ---------------------------------------------------------------------------------------------
// JetBrains SDK table
// ---------------------------------------------------------------------------------------------

#[derive(Default)]
pub struct JdkTable {
    by_name: HashMap<String, Vec<PathBuf>>,
}

fn attributes(element: &quick_xml::events::BytesStart) -> HashMap<String, String> {
    element
        .attributes()
        .flatten()
        .filter_map(|attribute| {
            let key = attribute.key.as_ref().to_owned();
            #[allow(deprecated)]
            let value = attribute.unescape_value().ok()?.into_owned();
            Some((key, value))
        })
        .collect()
}

impl JdkTable {
    /// Files given explicitly, or every `%APPDATA%\JetBrains\*\options\jdk.table.xml`.
    pub fn load(explicit: &[PathBuf]) -> Self {
        let mut files: Vec<PathBuf> = explicit.to_vec();
        if files.is_empty()
            && let Some(appdata) = std::env::var_os("APPDATA")
            && let Ok(products) = fs::read_dir(Path::new(&appdata).join("JetBrains"))
        {
            files.extend(products.flatten().map(|entry| entry.path().join(r"options\jdk.table.xml")));
        }
        let mut table = Self::default();
        for file in files {
            if let Ok(text) = fs::read_to_string(&file) {
                table.read(&text);
            }
        }
        table
    }

    fn read(&mut self, text: &str) {
        let mut reader = Reader::from_str(text);
        let (mut name, mut home_path, mut inside) = (None, None, false);
        loop {
            match reader.read_event() {
                Ok(Event::Start(element)) | Ok(Event::Empty(element)) => {
                    let tag = element.name().as_ref().to_owned();
                    let attributes = attributes(&element);
                    match tag.as_str() {
                        "jdk" => (name, home_path, inside) = (None, None, true),
                        "name" if inside => name = attributes.get("value").cloned(),
                        "homePath" if inside => home_path = attributes.get("value").cloned(),
                        _ => {}
                    }
                }
                Ok(Event::End(element)) if element.name().as_ref() == "jdk" => {
                    if let (Some(name), Some(path)) = (name.take(), home_path.take()) {
                        let path = match (path.strip_prefix("$USER_HOME$"), home()) {
                            (Some(rest), Some(home)) => format!("{}{rest}", home.to_string_lossy()),
                            _ => path,
                        };
                        // Other IDE macros ($APPLICATION_HOME_DIR$ ...) cannot be expanded here.
                        if !path.contains('$') {
                            let path = PathBuf::from(path.replace('/', "\\"));
                            let paths = self.by_name.entry(name).or_default();
                            if !paths.iter().any(|known| win::path_key(known) == win::path_key(&path)) {
                                paths.push(path);
                            }
                        }
                    }
                    inside = false;
                }
                Ok(Event::Eof) | Err(_) => break,
                _ => {}
            }
        }
    }

    fn target(&self, sdk: &str) -> Target {
        match self.by_name.get(sdk).map(Vec::as_slice) {
            Some([path]) => Target::Path(path.clone()),
            Some([]) | None => Target::Unresolved(SDK_NOT_FOUND),
            Some(_) => Target::Unresolved(SDK_AMBIGUOUS),
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Project files
// ---------------------------------------------------------------------------------------------

fn vscode(file: &Path, settings: &Value, root: &Path, flavor: &Flavor, refs: &mut Vec<RawRef>) {
    let folder = root.to_string_lossy().into_owned();
    let substitutions = [("${workspaceFolder}", folder.clone()), ("${workspaceRoot}", folder)];
    for key in ["python.defaultInterpreterPath", "python.pythonPath"] {
        if let Some(raw) = settings.get(key).and_then(Value::as_str) {
            refs.push(RawRef {
                source: file.to_path_buf(),
                key: key.to_owned(),
                raw: raw.to_owned(),
                target: path_target(raw, root, flavor, &substitutions),
            });
        }
    }
}

fn jsonc(file: &Path) -> Option<Value> {
    let text = fs::read_to_string(file).ok()?;
    jsonc_parser::parse_to_serde_value(&text, &Default::default()).ok().flatten()
}

fn jetbrains(file: &Path, element: &str, table: &JdkTable, refs: &mut Vec<RawRef>) {
    let Ok(text) = fs::read_to_string(file) else {
        return;
    };
    let mut reader = Reader::from_str(&text);
    loop {
        match reader.read_event() {
            Ok(Event::Start(tag)) | Ok(Event::Empty(tag)) if tag.name().as_ref() == element => {
                let attributes = attributes(&tag);
                let (name_key, type_key, matches) = match element {
                    "component" => (
                        "project-jdk-name",
                        "project-jdk-type",
                        attributes.get("name").is_some_and(|name| name == "ProjectRootManager"),
                    ),
                    _ => ("jdkName", "jdkType", attributes.get("type").is_some_and(|kind| kind == "jdk")),
                };
                if matches
                    && attributes.get(type_key).is_some_and(|kind| kind == "Python SDK")
                    && let Some(sdk) = attributes.get(name_key)
                {
                    refs.push(RawRef {
                        source: file.to_path_buf(),
                        key: name_key.to_owned(),
                        raw: sdk.clone(),
                        target: table.target(sdk),
                    });
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
}

/// References found in one project folder. An error means the folder could not be read.
pub fn read_project(root: &Path, table: &JdkTable) -> std::io::Result<Vec<RawRef>> {
    let flavor = resolve::flavor_of(root);
    let mut refs = Vec::new();
    let entries: Vec<PathBuf> = fs::read_dir(root)?.flatten().map(|entry| entry.path()).collect();

    let settings = root.join(r".vscode\settings.json");
    if let Some(value) = jsonc(&settings) {
        vscode(&settings, &value, root, &flavor, &mut refs);
    }
    for file in &entries {
        let name = file.file_name().map(|name| name.to_string_lossy().to_lowercase()).unwrap_or_default();
        if name.ends_with(".code-workspace") {
            if let Some(value) = jsonc(file)
                && let Some(settings) = value.get("settings")
            {
                vscode(file, settings, root, &flavor, &mut refs);
            }
        } else if name == ".python-version" {
            if let Ok(text) = fs::read_to_string(file)
                && let Some(line) = text.lines().map(str::trim).find(|line| !line.is_empty() && !line.starts_with('#'))
            {
                refs.push(RawRef {
                    source: file.clone(),
                    key: "version".to_owned(),
                    raw: line.to_owned(),
                    target: Target::Unresolved(VERSION_ONLY),
                });
            }
        } else if name == "pyproject.toml" {
            if let Ok(text) = fs::read_to_string(file)
                && let Ok(value) = text.parse::<toml::Table>()
                && let Some(requires) =
                    value.get("project").and_then(|project| project.get("requires-python")).and_then(|v| v.as_str())
            {
                refs.push(RawRef {
                    source: file.clone(),
                    key: "project.requires-python".to_owned(),
                    raw: requires.to_owned(),
                    target: Target::Unresolved(VERSION_ONLY),
                });
            }
        } else if name == "environment.yml" || name == "environment.yaml" {
            if let Ok(text) = fs::read_to_string(file)
                && let Some(env_name) = text.lines().find_map(|line| line.strip_prefix("name:"))
            {
                let env_name = env_name.trim().trim_matches(['"', '\'']);
                refs.push(RawRef {
                    source: file.clone(),
                    key: "name".to_owned(),
                    raw: env_name.to_owned(),
                    target: Target::CondaName(env_name.to_owned()),
                });
            }
        }
    }

    let idea = root.join(".idea");
    if idea.is_dir() {
        jetbrains(&idea.join("misc.xml"), "component", table, &mut refs);
        if let Ok(files) = fs::read_dir(&idea) {
            for file in files.flatten().map(|entry| entry.path()) {
                if file.extension().is_some_and(|extension| extension == "iml") {
                    jetbrains(&file, "orderEntry", table, &mut refs);
                }
            }
        }
    }
    Ok(refs)
}

// ---------------------------------------------------------------------------------------------
// Launch scripts
// ---------------------------------------------------------------------------------------------

/// Split a command line on whitespace, keeping quoted parts together.
pub fn tokenize(line: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    for c in line.chars() {
        match quote {
            Some(open) if c == open => quote = None,
            Some(_) => current.push(c),
            None if c == '"' || c == '\'' => quote = Some(c),
            None if c.is_whitespace() => {
                if !current.is_empty() {
                    tokens.push(std::mem::take(&mut current));
                }
            }
            None => current.push(c),
        }
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    tokens
}

fn file_name(token: &str) -> String {
    token.rsplit(['\\', '/']).next().unwrap_or(token).to_lowercase()
}

fn has_separator(token: &str) -> bool {
    token.contains(['\\', '/'])
}

/// References found in one launch script.
pub fn read_script(script: &Path) -> std::io::Result<Vec<RawRef>> {
    let text = String::from_utf8_lossy(&fs::read(script)?).into_owned();
    let directory = script.parent().unwrap_or(Path::new(".")).to_path_buf();
    let flavor = resolve::flavor_of(&directory);
    let folder = directory.to_string_lossy().into_owned();
    // `%~dp0` ends with a backslash; the PowerShell variables do not.
    let substitutions =
        [("%~dp0", format!("{folder}\\")), ("${PSScriptRoot}", folder.clone()), ("$PSScriptRoot", folder)];
    let mut refs = Vec::new();
    let mut push = |key: &str, raw: &str, target: Target| {
        refs.push(RawRef { source: script.to_path_buf(), key: key.to_owned(), raw: raw.to_owned(), target });
    };

    for line in text.lines() {
        let trimmed = line.trim();
        let lower = trimmed.to_lowercase();
        if trimmed.is_empty() || trimmed.starts_with('#') || lower.starts_with("rem ") || trimmed.starts_with("::") {
            continue;
        }
        let tokens = tokenize(trimmed);
        let mut index = 0;
        while index < tokens.len() {
            let token = &tokens[index];
            let name = file_name(token);
            let next = tokens.get(index + 1);
            if name == "conda" || name == "conda.exe" || name == "conda.bat" {
                match next.map(|next| next.to_lowercase()).as_deref() {
                    Some("activate") => {
                        if let Some(target) = tokens.get(index + 2) {
                            let resolved = if has_separator(target) {
                                path_target(target, &directory, &flavor, &substitutions)
                            } else {
                                Target::CondaName(target.clone())
                            };
                            push("conda activate", target, resolved);
                        }
                        index += 3;
                        continue;
                    }
                    Some("run") => {
                        let rest = &tokens[index + 2..];
                        for (offset, flag) in rest.iter().enumerate() {
                            let Some(value) = rest.get(offset + 1) else {
                                break;
                            };
                            match flag.as_str() {
                                "-n" | "--name" => push("conda run -n", value, Target::CondaName(value.clone())),
                                "-p" | "--prefix" => {
                                    push("conda run -p", value, path_target(value, &directory, &flavor, &substitutions));
                                }
                                _ => continue,
                            }
                            break;
                        }
                        index += 2;
                        continue;
                    }
                    _ => {}
                }
            }
            if matches!(name.as_str(), "activate" | "activate.bat" | "activate.ps1") {
                if has_separator(token) {
                    // <env>\Scripts\activate: the environment is two levels up.
                    let environment = match path_target(token, &directory, &flavor, &substitutions) {
                        Target::Path(path) => {
                            Target::Path(path.parent().and_then(Path::parent).map_or(path.clone(), Path::to_path_buf))
                        }
                        other => other,
                    };
                    push("activate", token, environment);
                } else if index > 0
                    && tokens[index - 1].eq_ignore_ascii_case("call")
                    && let Some(env_name) = next.filter(|next| !has_separator(next))
                {
                    push("call activate", env_name, Target::CondaName(env_name.clone()));
                    index += 1;
                }
            } else if matches!(name.as_str(), "python.exe" | "pythonw.exe" | "python" | "python3") && has_separator(token) {
                push("interpreter", token, path_target(token, &directory, &flavor, &substitutions));
            }
            index += 1;
        }
    }
    Ok(refs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_keep_quoted_paths_together() {
        assert_eq!(
            tokenize(r#"call "C:\Program Files\envs\a\Scripts\activate.bat" && python run.py"#),
            vec!["call", r"C:\Program Files\envs\a\Scripts\activate.bat", "&&", "python", "run.py"]
        );
    }

    #[test]
    fn sdk_table_maps_names_to_one_path_or_reports_ambiguity() {
        let mut table = JdkTable::default();
        table.read(
            r#"<application><component name="ProjectJdkTable">
                <jdk version="2"><name value="Python 3.12 (FAI)" /><type value="Python SDK" />
                  <homePath value="C:/envs/FAI/python.exe" /></jdk>
                <jdk version="2"><name value="Twice" /><homePath value="C:/a/python.exe" /></jdk>
                <jdk version="2"><name value="Twice" /><homePath value="D:/b/python.exe" /></jdk>
               </component></application>"#,
        );
        assert!(matches!(table.target("Python 3.12 (FAI)"), Target::Path(path) if path == Path::new(r"C:\envs\FAI\python.exe")));
        assert!(matches!(table.target("Twice"), Target::Unresolved(SDK_AMBIGUOUS)));
        assert!(matches!(table.target("Missing"), Target::Unresolved(SDK_NOT_FOUND)));
    }
}
