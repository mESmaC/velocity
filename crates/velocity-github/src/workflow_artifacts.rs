//! A workflow run's artifacts, downloaded beside its report so the agent that asked for the
//! run can read them (#508). GitHub has no API for a job's step summary, so a table a
//! workflow wants an agent to see has to be a text file in an artifact; the small text
//! files found here are the ones the report shows in full.

use std::path::{Path, PathBuf};

use crate::workflow_runs::{Artifact, WorkflowClient};

/// Artifacts past these are linked, not downloaded: a measurement run uploads a few small
/// reports, and a release run's installers are not something an agent reads.
const MAXIMUM_DOWNLOADS: usize = 20;
const MAXIMUM_ARTIFACT_BYTES: u64 = 64 * 1024 * 1024;
const MAXIMUM_RUN_BYTES: u64 = 256 * 1024 * 1024;
/// A text file longer than this is left for the agent to open by path.
const MAXIMUM_TEXT_BYTES: u64 = 16 * 1024;
/// How many files of one artifact are looked at for text.
const MAXIMUM_FILES: usize = 400;
const TEXT_EXTENSIONS: [&str; 10] = [
    "csv", "json", "log", "md", "toml", "tsv", "txt", "xml", "yaml", "yml",
];

pub(crate) struct DownloadedArtifact {
    pub artifact: Artifact,
    /// The folder holding its files, or why it was not downloaded.
    pub files: Result<PathBuf, String>,
    pub texts: Vec<TextFile>,
}

/// A small text file from an artifact, named by its path inside it.
pub(crate) struct TextFile {
    pub name: String,
    pub text: String,
}

/// Downloads `artifacts` into folders under `folder`, within the limits above.
pub(crate) fn download(
    client: &WorkflowClient,
    run: u64,
    artifacts: Vec<Artifact>,
    folder: &Path,
) -> Vec<DownloadedArtifact> {
    let mut downloaded_bytes = 0_u64;
    let mut downloads = 0_usize;
    artifacts
        .into_iter()
        .map(|artifact| {
            let files = match skip_reason(&artifact, downloads, downloaded_bytes) {
                Some(reason) => Err(reason),
                None => {
                    downloads += 1;
                    downloaded_bytes += artifact.size_in_bytes;
                    let target = artifact_folder(folder, &artifact);
                    // `gh` makes the folder too; a failure here shows as its download error.
                    let _ = std::fs::create_dir_all(&target);
                    client
                        .download_artifact(run, &artifact.name, &target)
                        .map(|()| target)
                        .map_err(|error| format!("download failed: {error}"))
                }
            };
            let texts = files.as_deref().map(text_files).unwrap_or_default();
            DownloadedArtifact {
                artifact,
                files,
                texts,
            }
        })
        .collect()
}

fn skip_reason(artifact: &Artifact, downloads: usize, downloaded_bytes: u64) -> Option<String> {
    if artifact.expired {
        Some("expired".into())
    } else if artifact.size_in_bytes > MAXIMUM_ARTIFACT_BYTES {
        Some("not downloaded: larger than 64 MB".into())
    } else if downloads >= MAXIMUM_DOWNLOADS
        || downloaded_bytes + artifact.size_in_bytes > MAXIMUM_RUN_BYTES
    {
        Some("not downloaded: the run's download limit was reached".into())
    } else {
        None
    }
}

/// A folder name from the artifact's own, kept to characters every file system takes. Two
/// names that reduce to the same one are told apart by the artifact's ID.
fn artifact_folder(folder: &Path, artifact: &Artifact) -> PathBuf {
    let name: String = artifact
        .name
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || "-_.".contains(character) {
                character
            } else {
                '_'
            }
        })
        .take(80)
        .collect();
    let name = name.trim_matches('.');
    let target = folder.join(name);
    if name.is_empty() || target.exists() {
        folder.join(format!("{name}-{}", artifact.id))
    } else {
        target
    }
}

/// The small UTF-8 text files under `folder`, in path order.
fn text_files(folder: &Path) -> Vec<TextFile> {
    let mut texts = Vec::new();
    let mut pending = vec![folder.to_path_buf()];
    let mut seen = 0_usize;
    while let Some(directory) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&directory) else {
            continue;
        };
        for entry in entries.flatten() {
            seen += 1;
            if seen > MAXIMUM_FILES {
                break;
            }
            // `file_type` does not follow links, so a link in an artifact is never read.
            match entry.file_type() {
                Ok(kind) if kind.is_dir() => pending.push(entry.path()),
                Ok(kind) if kind.is_file() => texts.extend(text_file(folder, &entry.path())),
                _ => {}
            }
        }
    }
    texts.sort_by(|left, right| left.name.cmp(&right.name));
    texts
}

fn text_file(folder: &Path, path: &Path) -> Option<TextFile> {
    let extension = path.extension()?.to_str()?.to_ascii_lowercase();
    if !TEXT_EXTENSIONS.contains(&extension.as_str()) {
        return None;
    }
    let length = std::fs::metadata(path).ok()?.len();
    if length == 0 || length > MAXIMUM_TEXT_BYTES {
        return None;
    }
    let text = String::from_utf8(std::fs::read(path).ok()?).ok()?;
    let name = path
        .strip_prefix(folder)
        .ok()?
        .to_string_lossy()
        .replace('\\', "/");
    Some(TextFile {
        name,
        // Windows PowerShell's `Out-File -Encoding utf8` starts a file with a byte order mark.
        text: text.trim_start_matches('\u{feff}').trim_end().to_owned(),
    })
}
