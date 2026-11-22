//! A view of one commit in a git repository, bare or not, read only from git objects (C11:
//! the standalone indexer Collapse runs). No checkout, filter or working-tree file is read.
//! Git blob IDs are the SHA-1 object IDs Rosen and the worktree graph use as file hashes, so
//! the parse cache keys, records and addresses match the MCP tools' for the same tree.

use std::collections::{BTreeMap, HashMap};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::Stdio;

use rosen_core::ObjectId;

use crate::Error;
use crate::files::git_command;

pub struct GitTree {
    repository: PathBuf,
    /// The full commit ID this view shows.
    pub commit: String,
    /// Every regular or executable file in the commit's tree, by repository-relative path.
    pub files: BTreeMap<String, ObjectId>,
}

impl GitTree {
    /// Resolves `revision` to a commit in `repository` and lists its files.
    pub fn open(repository: &Path, revision: &str) -> Result<Self, Error> {
        let commit = resolve(repository, revision)?;
        let files = list(repository, &commit)?;
        Ok(Self {
            repository: repository.to_path_buf(),
            commit,
            files,
        })
    }

    /// The files that differ from `previous`: a new blob ID, or `None` when deleted.
    pub fn changes_since(&self, previous: Option<&Self>) -> BTreeMap<String, Option<ObjectId>> {
        let mut changes: BTreeMap<String, Option<ObjectId>> = self
            .files
            .iter()
            .filter(|(path, id)| previous.and_then(|tree| tree.files.get(*path)) != Some(*id))
            .map(|(path, id)| (path.clone(), Some(*id)))
            .collect();
        for path in previous.into_iter().flat_map(|tree| tree.files.keys()) {
            if !self.files.contains_key(path) {
                changes.insert(path.clone(), None);
            }
        }
        changes
    }

    /// The bytes of each blob in `ids`, each checked against its ID.
    pub fn blobs(&self, ids: Vec<ObjectId>) -> Result<HashMap<ObjectId, Vec<u8>>, Error> {
        read_blobs(&self.repository, ids)
    }

    /// One file of the commit as text, for source lines.
    pub fn read(&self, path: &str) -> Result<String, Error> {
        let id = *self
            .files
            .get(path)
            .ok_or_else(|| Error::Invalid(format!("{path} is not in commit {}", self.commit)))?;
        let bytes = self
            .blobs(vec![id])?
            .remove(&id)
            .ok_or_else(|| Error::Git(format!("blob {id} for {path} is missing")))?;
        String::from_utf8(bytes).map_err(|error| Error::Invalid(error.to_string()))
    }
}

/// The full ID of the commit `revision` names. `--end-of-options` keeps a revision that
/// starts with `-` from being read as an option.
fn resolve(repository: &Path, revision: &str) -> Result<String, Error> {
    let output = git_command(repository)
        .args(["rev-parse", "--verify", "--quiet", "--end-of-options"])
        .arg(format!("{revision}^{{commit}}"))
        .output()?;
    let commit = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if !output.status.success() || commit.is_empty() {
        return Err(Error::Git(format!(
            "{revision} is not a commit in {}",
            repository.display()
        )));
    }
    Ok(commit)
}

/// The commit's regular and executable files. Symlinks and submodules are left out, as
/// they are from a Rosen tree.
fn list(repository: &Path, commit: &str) -> Result<BTreeMap<String, ObjectId>, Error> {
    let output = git_command(repository)
        .args(["ls-tree", "-r", "-z", "--full-tree", commit])
        .output()?;
    if !output.status.success() {
        return Err(Error::Git(
            String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        ));
    }
    let mut files = BTreeMap::new();
    for entry in output.stdout.split(|byte| *byte == 0) {
        if entry.is_empty() {
            continue;
        }
        let entry = String::from_utf8_lossy(entry);
        let (metadata, path) = entry.split_once('\t').ok_or_else(invalid_listing)?;
        let mut fields = metadata.split_whitespace();
        if !matches!(fields.next(), Some("100644" | "100755")) {
            continue;
        }
        let id = fields
            .nth(1)
            .and_then(|id| ObjectId::from_hex(id).ok())
            .ok_or_else(invalid_listing)?;
        files.insert(path.to_owned(), id);
    }
    Ok(files)
}

fn invalid_listing() -> Error {
    Error::Git(
        "Could not read the commit's file list (SHA-256 repositories are unsupported)".into(),
    )
}

/// Reads blobs through one `git cat-file --batch`, one request and answer at a time so
/// neither pipe fills while the other waits.
fn read_blobs(repository: &Path, ids: Vec<ObjectId>) -> Result<HashMap<ObjectId, Vec<u8>>, Error> {
    if ids.is_empty() {
        return Ok(HashMap::new());
    }
    let mut git = git_command(repository)
        .args(["cat-file", "--batch"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let result = match (git.stdin.take(), git.stdout.take()) {
        (Some(input), Some(output)) => answer_batch(&ids, input, BufReader::new(output)),
        _ => Err(Error::Invalid("Could not open the Git blob reader".into())),
    };
    let status = git.wait()?;
    let blobs = result?;
    if !status.success() {
        return Err(Error::Git(format!("Git blob reader exited with {status}")));
    }
    Ok(blobs)
}

fn answer_batch(
    ids: &[ObjectId],
    mut input: impl Write,
    mut output: impl BufRead,
) -> Result<HashMap<ObjectId, Vec<u8>>, Error> {
    let mut blobs = HashMap::new();
    let mut header = String::new();
    for id in ids {
        writeln!(input, "{id}")?;
        input.flush()?;
        header.clear();
        output.read_line(&mut header)?;
        let mut fields = header.split_whitespace();
        let (Some(_), Some("blob"), Some(length)) = (fields.next(), fields.next(), fields.next())
        else {
            return Err(Error::Git(format!(
                "blob {id} is missing: {}",
                header.trim()
            )));
        };
        let length: u64 = length
            .parse()
            .map_err(|_| Error::Git(format!("blob {id} has an unreadable size")))?;
        let mut bytes = Vec::new();
        output.by_ref().take(length).read_to_end(&mut bytes)?;
        let mut terminator = [0];
        output.read_exact(&mut terminator)?;
        // The same check Rosen's object reads make: the bytes must hash to their ID.
        if terminator != *b"\n" || ObjectId::hash_object("blob", &bytes) != *id {
            return Err(Error::Git(format!("blob {id} did not match its ID")));
        }
        blobs.insert(*id, bytes);
    }
    Ok(blobs)
}
