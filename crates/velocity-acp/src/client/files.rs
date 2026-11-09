//! `fs/read_text_file` and `fs/write_text_file`, rooted at the worktree but not confined to
//! it (§3.2 Tool access).

use std::path::{Path, PathBuf};

use agent_client_protocol::v1::schema::{
    ReadTextFileRequest, ReadTextFileResponse, WriteTextFileRequest, WriteTextFileResponse,
};

use super::ClientError;
use crate::resolve_path;

/// Where a path from an agent points: relative paths against the worktree, absolute paths as
/// given (`paths::resolve_path`).
pub fn resolve_in_worktree(worktree: &Path, requested: &Path) -> Result<PathBuf, ClientError> {
    let worktree_text = worktree
        .to_str()
        .ok_or_else(|| ClientError::NonUtf8Path(worktree.to_path_buf()))?;
    let requested_text = requested
        .to_str()
        .ok_or_else(|| ClientError::NonUtf8Path(requested.to_path_buf()))?;
    let resolved = resolve_path(worktree_text.as_bytes(), requested_text.as_bytes());
    // Every byte `resolve_path` adds is ASCII and it only cuts at ASCII separators, so the
    // result of two UTF-8 inputs is UTF-8; the error branch is unreachable in practice.
    String::from_utf8(resolved)
        .map(PathBuf::from)
        .map_err(|_| ClientError::NonUtf8Path(requested.to_path_buf()))
}

pub async fn read_text_file(
    worktree: &Path,
    request: ReadTextFileRequest,
) -> Result<ReadTextFileResponse, ClientError> {
    let path = resolve_in_worktree(worktree, &request.path)?;
    let content = tokio::fs::read_to_string(&path).await?;
    Ok(ReadTextFileResponse::new(select_lines(
        &content,
        request.line,
        request.limit,
    )))
}

/// ACP's `line` is 1-based; `limit` is a number of lines. Line endings are kept as written.
fn select_lines(content: &str, line: Option<u32>, limit: Option<u32>) -> String {
    if line.is_none() && limit.is_none() {
        return content.to_owned();
    }
    let skip = line.map_or(0, |line| line.saturating_sub(1)) as usize;
    let take = limit.map_or(usize::MAX, |limit| limit as usize);
    content
        .split_inclusive('\n')
        .skip(skip)
        .take(take)
        .collect()
}

pub async fn write_text_file(
    worktree: &Path,
    request: WriteTextFileRequest,
) -> Result<(WriteTextFileResponse, PathBuf), ClientError> {
    let path = resolve_in_worktree(worktree, &request.path)?;
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    tokio::fs::write(&path, request.content).await?;
    Ok((WriteTextFileResponse::new(), path))
}
