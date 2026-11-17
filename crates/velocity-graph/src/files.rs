//! The files a graph covers: what git tracks or doesn't ignore in a worktree, minus generated
//! files (F19 "Left out"), with their content hashes.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Component, Path};
use std::process::Stdio;

use crate::Error;
use crate::language::Language;
use crate::text::digest;

/// Files over this size are listed with no symbols: they are data, not code an agent reads.
const PARSE_LIMIT: usize = 1024 * 1024;

/// One file in the worktree.
pub struct SourceFile {
    /// Repository-relative, with `/` separators.
    pub path: String,
    pub language: Option<Language>,
    /// Hash of the file's bytes: the parse cache key and the staleness check.
    pub hash: String,
    /// The text, when the file is parsed (a known language, text, under the size limit).
    pub text: Option<String>,
}

/// Every file git tracks or doesn't ignore under `root`.
pub fn list(root: &Path) -> Result<Vec<String>, Error> {
    if root.join(".rosen/workspace.json").is_file() {
        return walk(root, true);
    }
    let output = hidden_command("git")
        .arg("-C")
        .arg(root)
        .args([
            "ls-files",
            "-z",
            "--cached",
            "--others",
            "--exclude-standard",
        ])
        .output()?;
    if !output.status.success() {
        return Err(Error::Git(
            String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        ));
    }
    let mut paths: Vec<String> = output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
        .map(|path| String::from_utf8_lossy(path).replace('\\', "/"))
        .collect();
    paths.sort();
    paths.dedup();
    Ok(paths)
}

/// Rosen workspaces have no git index. Respect working-tree ignore rules there;
/// a materialized base tree already consists exclusively of tracked files.
pub fn walk(root: &Path, respect_ignores: bool) -> Result<Vec<String>, Error> {
    let mut walker = ignore::WalkBuilder::new(root);
    walker
        .standard_filters(respect_ignores)
        .hidden(false)
        .require_git(false)
        .filter_entry(|entry| !matches!(entry.file_name().to_str(), Some(".git" | ".rosen")));
    let mut paths = Vec::new();
    for entry in walker.build() {
        let entry = entry.map_err(|error| Error::Invalid(error.to_string()))?;
        if !entry.file_type().is_some_and(|kind| kind.is_file()) {
            continue;
        }
        if let Ok(path) = entry.path().strip_prefix(root) {
            paths.push(path.to_string_lossy().replace('\\', "/"));
        }
    }
    paths.sort();
    Ok(paths)
}

/// Read committed blobs without invoking checkout filters or trusting any working-tree file.
pub fn stage_git_base(root: &Path, commit: &str, staging: &Path) -> Result<(), Error> {
    let tree = staging.join("tree");
    std::fs::create_dir(&tree)?;
    let listing = hidden_command("git")
        .arg("-C")
        .arg(root)
        .args(["ls-tree", "-rz", commit])
        .output()?;
    if !listing.status.success() {
        return Err(Error::Git(
            String::from_utf8_lossy(&listing.stderr).trim().to_owned(),
        ));
    }

    let mut git = hidden_command("git")
        .arg("-C")
        .arg(root)
        .args(["cat-file", "--batch"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()?;
    let result = match (git.stdin.take(), git.stdout.take()) {
        (Some(input), Some(output)) => {
            stage_blobs(&tree, &listing.stdout, input, BufReader::new(output))
        }
        _ => Err(Error::Invalid("Could not open the Git blob reader".into())),
    };
    let status = git.wait()?;
    result?;
    if !status.success() {
        return Err(Error::Git(format!("Git blob reader exited with {status}")));
    }
    Ok(())
}

fn stage_blobs(
    tree: &Path,
    listing: &[u8],
    mut input: impl Write,
    mut output: impl BufRead,
) -> Result<(), Error> {
    let mut header = String::new();
    for entry in listing
        .split(|byte| *byte == 0)
        .filter(|entry| !entry.is_empty())
    {
        let entry = String::from_utf8_lossy(entry);
        let (metadata, path) = entry.split_once('\t').ok_or_else(invalid_git_blob)?;
        let mut fields = metadata.split_whitespace();
        let mode = fields.next();
        if !matches!(mode, Some("100644" | "100755")) {
            continue;
        }
        let object = fields.nth(1).ok_or_else(invalid_git_blob)?;
        let path = Path::new(path);
        if !path
            .components()
            .all(|part| matches!(part, Component::Normal(_)))
        {
            return Err(invalid_git_blob());
        }
        let destination = tree.join(path);
        if let Some(parent) = destination.parent() {
            std::fs::create_dir_all(parent)?;
        }

        writeln!(input, "{object}")?;
        header.clear();
        output.read_line(&mut header)?;
        let length: u64 = header
            .split_whitespace()
            .nth(2)
            .and_then(|length| length.parse().ok())
            .ok_or_else(invalid_git_blob)?;
        let mut file = std::fs::File::options()
            .write(true)
            .create_new(true)
            .open(destination)?;
        let written = std::io::copy(&mut output.by_ref().take(length), &mut file)?;
        let mut terminator = [0];
        output.read_exact(&mut terminator)?;
        if written != length || terminator != *b"\n" {
            return Err(invalid_git_blob());
        }
    }
    Ok(())
}

fn invalid_git_blob() -> Error {
    Error::Git("Could not read the committed file list or blob".into())
}

/// Reads one listed file. `None` when it's gone (deleted but still in the index) or generated.
pub fn read(root: &Path, path: &str) -> Option<SourceFile> {
    inspect(root, path).0
}

pub fn inspect(root: &Path, path: &str) -> (Option<SourceFile>, Option<&'static str>) {
    if generated_name(path) {
        return (None, Some("Generated"));
    }
    let Ok(bytes) = std::fs::read(root.join(path)) else {
        return (None, Some("Missing or unreadable"));
    };
    let hash = digest(&bytes);
    let recognized = Language::of(path);
    let mut skip = if bytes.iter().take(8000).any(|byte| *byte == 0) {
        Some("Binary")
    } else if bytes.len() > PARSE_LIMIT {
        Some("Over 1 MiB")
    } else if recognized.is_none() {
        Some("No parser")
    } else {
        None
    };
    let text = if skip.is_none() {
        match String::from_utf8(bytes) {
            Ok(text) if generated_text(&text) => return (None, Some("Generated")),
            Ok(text) => Some(text),
            Err(_) => {
                skip = Some("Invalid UTF-8");
                None
            }
        }
    } else {
        None
    };
    (
        Some(SourceFile {
            path: path.to_owned(),
            language: text.as_ref().and(recognized),
            hash,
            text,
        }),
        skip,
    )
}

/// Lockfiles: generated, and never worth an agent's reading.
fn generated_name(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    matches!(
        name,
        "Cargo.lock"
            | "pnpm-lock.yaml"
            | "package-lock.json"
            | "yarn.lock"
            | "bun.lockb"
            | "poetry.lock"
            | "go.sum"
            | "uv.lock"
    )
}

/// A file that says in its first lines that a tool wrote it (tauri-specta bindings,
/// protobuf output, `@generated` markers).
fn generated_text(text: &str) -> bool {
    let head: String = text
        .chars()
        .take(400)
        .collect::<String>()
        .to_ascii_lowercase();
    head.contains("@generated")
        || head.contains("do not edit")
        || head.contains("this file was generated")
        || head.contains("auto-generated")
        || head.contains("autogenerated")
}

/// A command with no console window, below normal priority: warming is for later, agents'
/// builds are for now. Creation flags replace `hidden_command`'s, so this repeats
/// `CREATE_NO_WINDOW` (#276).
#[allow(clippy::disallowed_methods)]
pub(crate) fn background_command(program: impl AsRef<std::ffi::OsStr>) -> std::process::Command {
    let mut command = std::process::Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        const BELOW_NORMAL_PRIORITY_CLASS: u32 = 0x0000_4000;
        command.creation_flags(CREATE_NO_WINDOW | BELOW_NORMAL_PRIORITY_CLASS);
    }
    command
}

#[allow(clippy::disallowed_methods)]
fn hidden_command(program: impl AsRef<std::ffi::OsStr>) -> std::process::Command {
    let mut command = std::process::Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    command
}
