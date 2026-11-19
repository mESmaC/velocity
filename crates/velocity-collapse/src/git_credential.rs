//! Hands a Collapse push token to the machine's git credential helper (Collapse C1, C3).
//!
//! Rosen's mirror push runs `git push` with prompts disabled, so the helper answers it.
//! Collapse issues one token per repository on the same host, so both the approval and
//! the pushes use `credential.useHttpPath=true`, keying each token by its repository path.

use std::io::Write;
use std::process::{Command, Stdio};

use crate::CollapseError;

/// The URL scope the per-path setting is saved under in the user's git configuration.
const HOST_SCOPE: &str = "https://api.collapse.sh";
/// Collapse accepts any username with a push token; its link panel uses `rosen`.
const USERNAME: &str = "rosen";

/// Saves `secret` for `mirror_url` in the git credential helper. Velocity never reads it
/// back; the helper owns it from here, as `git credential approve` does for a person.
pub fn store_push_secret(mirror_url: &str, secret: &str) -> Result<(), CollapseError> {
    if mirror_url.contains(['\n', '\r', '\0']) || secret.contains(['\n', '\r', '\0']) {
        return Err(CollapseError::Git(
            "Collapse returned a mirror URL or token git cannot store".into(),
        ));
    }
    ensure_helper(mirror_url)?;
    ensure_path_specific_credentials()?;
    let mut child = hidden_git()
        .args(["-c", "credential.useHttpPath=true", "credential", "approve"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| CollapseError::Git(format!("Could not run git: {error}")))?;
    let request = format!("url={mirror_url}\nusername={USERNAME}\npassword={secret}\n\n");
    if let Some(mut input) = child.stdin.take() {
        input
            .write_all(request.as_bytes())
            .map_err(|error| CollapseError::Git(error.to_string()))?;
    }
    let output = child
        .wait_with_output()
        .map_err(|error| CollapseError::Git(error.to_string()))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(CollapseError::Git(format!(
            "git credential approve failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )))
    }
}

/// Without a helper `git credential approve` succeeds and stores nothing, and every
/// mirror push would then fail with 401.
fn ensure_helper(mirror_url: &str) -> Result<(), CollapseError> {
    let helper = git_output(&["config", "--get-urlmatch", "credential.helper", mirror_url])?;
    if helper.trim().is_empty() {
        return Err(CollapseError::Git(
            "Git has no credential helper to keep the Collapse push token. Install Git for Windows with Git Credential Manager, then try again.".into(),
        ));
    }
    Ok(())
}

/// Rosen's pushes read the user's git configuration, so the per-path setting is saved
/// there for Collapse's host only. Other hosts keep their own behaviour.
fn ensure_path_specific_credentials() -> Result<(), CollapseError> {
    let key = format!("credential.{HOST_SCOPE}.useHttpPath");
    if git_output(&["config", "--global", "--get", &key])?.trim() == "true" {
        return Ok(());
    }
    let output = hidden_git()
        .args(["config", "--global", &key, "true"])
        .output()
        .map_err(|error| CollapseError::Git(format!("Could not run git: {error}")))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(CollapseError::Git(format!(
            "Could not save {key} in your git configuration: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )))
    }
}

/// Standard output of a git query; an unset key (exit status 1) reads as empty.
fn git_output(arguments: &[&str]) -> Result<String, CollapseError> {
    let output = hidden_git()
        .args(arguments)
        .output()
        .map_err(|error| CollapseError::Git(format!("Could not run git: {error}")))?;
    match output.status.code() {
        Some(0) => Ok(String::from_utf8_lossy(&output.stdout).into_owned()),
        Some(1) => Ok(String::new()),
        _ => Err(CollapseError::Git(
            String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        )),
    }
}

/// `git` with no console window and no prompt: a missing helper must fail, not wait.
fn hidden_git() -> Command {
    let mut command = Command::new("git");
    command
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GCM_INTERACTIVE", "never");
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    command
}
