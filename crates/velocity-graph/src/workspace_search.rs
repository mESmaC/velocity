//! Workspace text and file search (`graph_grep` and `graph_glob`, #538) for harnesses that
//! have no built-in search tool. Respects `.gitignore` (even in Rosen workspaces without
//! `.git`) and caps output within the graph answer budget.

use std::fmt::Write;
use std::io::{BufRead, BufReader};
use std::path::{Component, Path, PathBuf};
use std::process::Stdio;

use serde_json::Value;

use crate::files::hidden_command;
use crate::program::glob as match_glob;
use crate::query::BUDGET;

const DEFAULT_GREP_LIMIT: usize = 50;
const MAX_GREP_LIMIT: usize = 200;
const DEFAULT_GLOB_LIMIT: usize = 100;
const MAX_GLOB_LIMIT: usize = 200;
const MAX_GREP_COLUMNS: usize = 300;
const MAX_FALLBACK_FILE_BYTES: u64 = 1024 * 1024;

/// Runs `graph_grep` over `root` using ripgrep (or an `ignore`-crate fallback when `rg` is
/// absent), respecting `.gitignore` and capping results.
pub fn grep(root: &Path, arguments: &Value) -> Result<String, String> {
    let pattern = required_text(arguments, "pattern")?;
    let sub_path = optional_subpath(root, arguments.get("path").and_then(Value::as_str))?;
    let file_glob = arguments
        .get("glob")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|glob| !glob.is_empty());
    let case_insensitive = arguments["case_insensitive"].as_bool().unwrap_or(false);
    let fixed = arguments["fixed"].as_bool().unwrap_or(false);
    let files_only = arguments["files_only"].as_bool().unwrap_or(false);
    let context = arguments["context"].as_u64().unwrap_or(0).min(5) as usize;
    let limit = arguments["limit"]
        .as_u64()
        .unwrap_or(DEFAULT_GREP_LIMIT as u64)
        .clamp(1, MAX_GREP_LIMIT as u64) as usize;
    let target = sub_path
        .as_ref()
        .map(|relative| root.join(relative))
        .unwrap_or_else(|| root.to_path_buf());
    if !target.exists() {
        return Err(format!(
            "path `{}` does not exist",
            sub_path
                .as_ref()
                .map(|path| path.to_string_lossy())
                .unwrap_or_default()
        ));
    }
    let options = GrepOptions {
        pattern,
        sub_path: sub_path.as_deref(),
        file_glob,
        case_insensitive,
        fixed,
        files_only,
        context,
        limit,
    };
    match run_ripgrep(root, &options) {
        Ok(output) => Ok(output),
        Err(RipgrepError::NotInstalled) => fallback_grep(root, &options),
        Err(RipgrepError::Failed(message)) => Err(message),
    }
}

struct GrepOptions<'a> {
    pattern: &'a str,
    sub_path: Option<&'a Path>,
    file_glob: Option<&'a str>,
    case_insensitive: bool,
    fixed: bool,
    files_only: bool,
    context: usize,
    limit: usize,
}

enum RipgrepError {
    NotInstalled,
    Failed(String),
}

fn ripgrep_command(root: &Path, options: &GrepOptions<'_>) -> std::process::Command {
    let mut command = hidden_command("rg");
    command
        .current_dir(root)
        .args([
            "--color=never",
            "--no-heading",
            "--no-require-git",
            "--glob=!.git/**",
            "--glob=!.rosen/**",
        ])
        .arg(format!("--max-columns={MAX_GREP_COLUMNS}"))
        .arg("--max-columns-preview");
    if options.files_only {
        command.arg("--files-with-matches");
    } else {
        command.arg("--line-number");
        if options.context > 0 {
            command.arg(format!("--context={}", options.context));
        }
    }
    if options.case_insensitive {
        command.arg("--ignore-case");
    }
    if options.fixed {
        command.arg("--fixed-strings");
    }
    if let Some(glob) = options.file_glob {
        command.arg("--glob").arg(glob);
    }
    command.arg("--").arg(options.pattern);
    if let Some(sub_path) = options.sub_path {
        command.arg(sub_path);
    }
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command
}

fn run_ripgrep(root: &Path, options: &GrepOptions<'_>) -> Result<String, RipgrepError> {
    let mut child = ripgrep_command(root, options).spawn().map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            RipgrepError::NotInstalled
        } else {
            RipgrepError::Failed(format!("could not start rg: {error}"))
        }
    })?;
    let Some(stdout) = child.stdout.take() else {
        let _ = child.kill();
        let _ = child.wait();
        return Err(RipgrepError::Failed("could not read rg output".into()));
    };
    let (lines, truncated) = read_bounded_lines(BufReader::new(stdout), options.limit);
    if truncated {
        let _ = child.kill();
    }
    let output = child
        .wait_with_output()
        .map_err(|error| RipgrepError::Failed(format!("rg failed: {error}")))?;
    if !truncated && !output.status.success() {
        // ripgrep exits 1 on no matches and 2 on an invalid regex or I/O error.
        if output.status.code() == Some(1) {
            return Ok(format!("no matches for `{}`", options.pattern));
        }
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        return Err(RipgrepError::Failed(if stderr.is_empty() {
            format!("rg exited with {}", output.status)
        } else {
            stderr
        }));
    }
    Ok(format_grep_output(options.pattern, &lines, truncated))
}

fn read_bounded_lines(reader: impl BufRead, limit: usize) -> (Vec<String>, bool) {
    let mut lines = Vec::new();
    let mut bytes = 0usize;
    let mut truncated = false;
    for line in reader.lines() {
        let Ok(line) = line else { break };
        if lines.len() >= limit || bytes + line.len() + 1 > BUDGET - 300 {
            truncated = true;
            break;
        }
        let normalized = normalize_grep_line(&line);
        bytes += normalized.len() + 1;
        lines.push(normalized);
    }
    (lines, truncated)
}

fn normalize_grep_line(line: &str) -> String {
    if let Some((path, rest)) = line.split_once(':') {
        format!("{}:{rest}", path.replace('\\', "/"))
    } else {
        line.replace('\\', "/")
    }
}

fn format_grep_output(pattern: &str, lines: &[String], truncated: bool) -> String {
    if lines.is_empty() {
        return format!("no matches for `{pattern}`");
    }
    let mut out = String::new();
    if truncated {
        let _ = writeln!(
            out,
            "[showing first {} lines; narrow with path, glob or a tighter pattern]",
            lines.len()
        );
    }
    for line in lines {
        let _ = writeln!(out, "{line}");
    }
    out
}

/// Literal/case-insensitive fallback when `rg` is not on PATH. Uses `ignore::WalkBuilder`
/// so `.gitignore` rules are still honored in Rosen and git workspaces alike.
fn fallback_grep(root: &Path, options: &GrepOptions<'_>) -> Result<String, String> {
    let needle = if options.case_insensitive {
        options.pattern.to_lowercase()
    } else {
        options.pattern.to_owned()
    };
    let start = options
        .sub_path
        .map(|relative| root.join(relative))
        .unwrap_or_else(|| root.to_path_buf());
    let mut lines = Vec::new();
    let mut bytes = 0usize;
    let mut truncated = false;
    for path in walk_files(root, &start)? {
        if let Some(glob) = options.file_glob
            && !glob_matches(glob, &path)
        {
            continue;
        }
        let full = root.join(&path);
        if full
            .metadata()
            .is_ok_and(|metadata| metadata.len() > MAX_FALLBACK_FILE_BYTES)
        {
            continue;
        }
        let Ok(content) = std::fs::read_to_string(&full) else {
            continue;
        };
        for (index, raw_line) in content.lines().enumerate() {
            let matched = if options.case_insensitive {
                raw_line.to_lowercase().contains(&needle)
            } else {
                raw_line.contains(&needle)
            };
            if !matched {
                continue;
            }
            let entry = if options.files_only {
                path.clone()
            } else {
                format!("{path}:{}:{}", index + 1, truncate_columns(raw_line))
            };
            if lines.len() >= options.limit || bytes + entry.len() + 1 > BUDGET - 300 {
                truncated = true;
                break;
            }
            bytes += entry.len() + 1;
            lines.push(entry);
            if options.files_only {
                break;
            }
        }
        if truncated {
            break;
        }
    }
    Ok(format_grep_output(options.pattern, &lines, truncated))
}

fn truncate_columns(line: &str) -> String {
    if line.chars().count() <= MAX_GREP_COLUMNS {
        return line.to_owned();
    }
    let prefix: String = line.chars().take(MAX_GREP_COLUMNS).collect();
    format!("{prefix}…")
}

/// Runs `graph_glob` over `root`, respecting `.gitignore` (including in Rosen workspaces)
/// and capping results.
pub fn glob_files(root: &Path, arguments: &Value) -> Result<String, String> {
    let pattern = required_text(arguments, "pattern")?
        .replace('\\', "/")
        .trim_start_matches("./")
        .to_owned();
    let sub_path = optional_subpath(root, arguments.get("path").and_then(Value::as_str))?;
    let limit = arguments["limit"]
        .as_u64()
        .unwrap_or(DEFAULT_GLOB_LIMIT as u64)
        .clamp(1, MAX_GLOB_LIMIT as u64) as usize;
    let start = sub_path
        .as_ref()
        .map(|relative| root.join(relative))
        .unwrap_or_else(|| root.to_path_buf());
    if !start.exists() {
        return Err(format!(
            "path `{}` does not exist",
            sub_path
                .as_ref()
                .map(|path| path.to_string_lossy())
                .unwrap_or_default()
        ));
    }
    let mut matched: Vec<String> = walk_files(root, &start)?
        .into_iter()
        .filter(|path| glob_matches(&pattern, path))
        .collect();
    matched.sort();
    if matched.is_empty() {
        return Ok(format!("no files match `{pattern}`"));
    }
    let total = matched.len();
    let mut out = String::new();
    let mut shown = 0usize;
    let mut body = String::new();
    for path in matched.iter().take(limit) {
        if body.len() + path.len() + 1 > BUDGET - 250 {
            break;
        }
        let _ = writeln!(body, "{path}");
        shown += 1;
    }
    if shown < total {
        let _ = writeln!(
            out,
            "[showing {shown} of {total} files; narrow with path or pattern]"
        );
    }
    out.push_str(&body);
    Ok(out)
}

/// Supports `{a,b}` brace expansion on top of `crate::program::glob`.
pub fn glob_matches(pattern: &str, path: &str) -> bool {
    expand_braces(pattern)
        .iter()
        .any(|candidate| match_glob(candidate, path))
}

const MAX_BRACE_EXPANSIONS: usize = 64;

fn expand_braces(pattern: &str) -> Vec<String> {
    let Some(open) = pattern.find('{') else {
        return vec![pattern.to_owned()];
    };
    let Some(offset) = pattern[open..].find('}') else {
        return vec![pattern.to_owned()];
    };
    let close = open + offset;
    let prefix = &pattern[..open];
    let suffix = &pattern[close + 1..];
    let mut expanded = Vec::new();
    for branch in pattern[open + 1..close].split(',') {
        for tail in expand_braces(suffix) {
            if expanded.len() >= MAX_BRACE_EXPANSIONS {
                return expanded;
            }
            expanded.push(format!("{prefix}{branch}{tail}"));
        }
    }
    expanded
}

fn walk_files(root: &Path, start: &Path) -> Result<Vec<String>, String> {
    let mut walker = ignore::WalkBuilder::new(start);
    walker
        .standard_filters(true)
        .require_git(false)
        .filter_entry(|entry| !matches!(entry.file_name().to_str(), Some(".git" | ".rosen")));
    let mut files = Vec::new();
    for entry in walker.build() {
        let entry = entry.map_err(|error| error.to_string())?;
        if !entry.file_type().is_some_and(|kind| kind.is_file()) {
            continue;
        }
        if let Ok(relative) = entry.path().strip_prefix(root) {
            files.push(relative.to_string_lossy().replace('\\', "/"));
        }
    }
    Ok(files)
}

fn required_text<'a>(arguments: &'a Value, key: &str) -> Result<&'a str, String> {
    arguments[key]
        .as_str()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| format!("`{key}` is required"))
}

/// Validates that an optional search path stays inside `root` and returns it relative to `root`.
fn optional_subpath(root: &Path, raw: Option<&str>) -> Result<Option<PathBuf>, String> {
    let Some(raw) = raw.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    let candidate = Path::new(raw);
    let relative = if candidate.is_absolute() {
        strip_workspace_prefix(root, candidate)
            .ok_or_else(|| format!("path `{raw}` is outside the workspace"))?
    } else {
        PathBuf::from(raw.replace('\\', "/").trim_start_matches("./"))
    };
    let mut normalized = PathBuf::new();
    for component in relative.components() {
        match component {
            Component::Normal(part) => normalized.push(part),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(format!("path `{raw}` must stay inside the workspace"));
            }
        }
    }
    Ok((!normalized.as_os_str().is_empty()).then_some(normalized))
}

fn strip_workspace_prefix(root: &Path, candidate: &Path) -> Option<PathBuf> {
    if let Ok(relative) = candidate.strip_prefix(root) {
        return Some(relative.to_path_buf());
    }
    let canonical_root = std::fs::canonicalize(root).ok()?;
    let canonical_candidate = std::fs::canonicalize(candidate).ok()?;
    canonical_candidate
        .strip_prefix(&canonical_root)
        .ok()
        .map(Path::to_path_buf)
}
