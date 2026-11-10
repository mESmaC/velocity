//! F11's native file edge: keep drafts in Rust and commit only AGENTS.md.

use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use velocity_core::agents_editor::{AgentsEditor, OutlineAction, OutlineSnapshot};

#[derive(Debug, thiserror::Error)]
pub enum AgentsFileError {
    #[error("Could not access AGENTS.md: {0}")]
    Io(#[from] io::Error),
    #[error(transparent)]
    Git(#[from] crate::GitError),
    #[error(
        "AGENTS.md changed on disk. Your draft is kept; reopen after reconciling the external edit."
    )]
    Changed,
    #[error("AGENTS.md must be a regular file in the project's checkout")]
    NotRegular,
    #[error("Open AGENTS.md before editing or saving it")]
    NotOpen,
}

struct AgentsFile {
    editor: AgentsEditor,
    disk_source: Option<String>,
}

#[derive(Default)]
pub struct AgentsFiles {
    documents: BTreeMap<PathBuf, AgentsFile>,
}

impl AgentsFiles {
    pub fn read(&mut self, directory: &Path) -> Result<OutlineSnapshot, AgentsFileError> {
        let path = file_path(directory)?;
        let disk_source = read_source(&path)?;
        let document = self.documents.entry(path).or_insert_with(|| AgentsFile {
            editor: AgentsEditor::new(disk_source.clone().unwrap_or_default()),
            disk_source: disk_source.clone(),
        });
        // Reopening refreshes a clean document, but never throws away an unsaved draft.
        if !document.editor.snapshot(None).dirty && disk_source != document.disk_source {
            document.editor = AgentsEditor::new(disk_source.clone().unwrap_or_default());
            document.disk_source = disk_source;
        }
        Ok(document.editor.snapshot(None))
    }

    pub fn edit(
        &mut self,
        directory: &Path,
        action: OutlineAction,
    ) -> Result<OutlineSnapshot, AgentsFileError> {
        let path = file_path(directory)?;
        let document = self
            .documents
            .get_mut(&path)
            .ok_or(AgentsFileError::NotOpen)?;
        Ok(document.editor.apply(action))
    }

    pub fn begin_rewrite(
        &mut self,
        directory: &Path,
        identifier: u32,
    ) -> Result<Option<velocity_core::agents_rewrite::RewriteTicket>, AgentsFileError> {
        let path = file_path(directory)?;
        let document = self
            .documents
            .get_mut(&path)
            .ok_or(AgentsFileError::NotOpen)?;
        Ok(document.editor.begin_rewrite(identifier))
    }

    pub fn finish_rewrite(
        &mut self,
        directory: &Path,
        ticket: &velocity_core::agents_rewrite::RewriteTicket,
        result: Result<Vec<String>, String>,
    ) -> Result<OutlineSnapshot, AgentsFileError> {
        let path = file_path(directory)?;
        let document = self
            .documents
            .get_mut(&path)
            .ok_or(AgentsFileError::NotOpen)?;
        document.editor.finish_rewrite(ticket, result);
        Ok(document.editor.snapshot(None))
    }

    pub fn save(&mut self, directory: &Path) -> Result<OutlineSnapshot, AgentsFileError> {
        let path = file_path(directory)?;
        let document = self
            .documents
            .get_mut(&path)
            .ok_or(AgentsFileError::NotOpen)?;
        if read_source(&path)? != document.disk_source {
            return Err(AgentsFileError::Changed);
        }
        let markdown = document.editor.markdown();
        if document.disk_source.as_deref() != Some(&markdown) {
            replace_source(&path, markdown.as_bytes())?;
            // Retain the written baseline even if a git hook rejects this commit: Save can retry.
            document.disk_source = Some(markdown);
        }
        commit_agents(directory)?;
        document.editor.saved();
        Ok(document.editor.snapshot(None))
    }
}

/// Commit only AGENTS.md; the repository edge handles long Windows paths.
pub fn commit_agents(directory: &Path) -> Result<(), AgentsFileError> {
    crate::commit_file(directory, Path::new("AGENTS.md"), "Update AGENTS.md")?;
    Ok(())
}

fn file_path(directory: &Path) -> Result<PathBuf, AgentsFileError> {
    Ok(fs::canonicalize(directory)?.join("AGENTS.md"))
}

fn read_source(path: &Path) -> Result<Option<String>, AgentsFileError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if !metadata.file_type().is_file() => Err(AgentsFileError::NotRegular),
        Ok(_) => Ok(Some(fs::read_to_string(path)?)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn replace_source(path: &Path, bytes: &[u8]) -> Result<(), AgentsFileError> {
    let temporary = path.with_file_name(format!(".velocity-agents-{}.partial", std::process::id()));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)?;
    let result = (|| -> io::Result<()> {
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temporary, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result.map_err(Into::into)
}
