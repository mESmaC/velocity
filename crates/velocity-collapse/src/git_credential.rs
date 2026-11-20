//! Hands a Collapse push token to the machine's git credential helper (Collapse C1, C3).
//!
//! Rosen's mirror push runs `git push` with prompts disabled, so the helper answers it.
//! Collapse issues one token per repository on the same host, so both the approval and
//! the pushes use `credential.useHttpPath=true`, keying each token by its repository path.
//!
//! Rosen's state feed, ledger push and action relay authenticate with the same token, and
//! ask the helper for it under their own endpoints (`<root>/rosen/state`, `<root>/ledgers`).
//! With per-path credentials those are different keys from the mirror URL, so the token is
//! stored under each (#517).

use std::io::Write;
use std::process::{Command, Stdio};

use crate::CollapseError;

/// The URL scope the per-path setting is saved under in the user's git configuration.
const HOST_SCOPE: &str = "https://api.collapse.sh";
/// Collapse accepts any username with a push token; its link panel uses `rosen`.
const USERNAME: &str = "rosen";

/// Saves `secret` in the git credential helper under each of `urls`, keyed by path.
/// Velocity keeps no copy; the helper owns it from here, as `git credential approve` does
/// for a person.
pub fn store_push_secret(urls: &[String], secret: &str) -> Result<(), CollapseError> {
    let Some(first) = urls.first() else {
        return Ok(());
    };
    ensure_helper(first)?;
    ensure_path_specific_credentials()?;
    urls.iter().try_for_each(|url| approve(url, secret, true))
}

/// Gives `urls` the push token the helper already holds for `remote`, the project's
/// mirror. `false` when it holds none. The token passes from the helper back to the
/// helper inside this call and is never kept, logged or returned.
///
/// Both steps run under the user's own git configuration, as Rosen's lookups do. A token
/// Velocity stored is keyed by path, so each URL gets its own entry. A host-wide token a
/// person stored by hand already answers every URL, and approving it again changes nothing.
pub fn share_push_secret(remote: &str, urls: &[String]) -> Result<bool, CollapseError> {
    let Some(secret) = held_secret(remote)? else {
        return Ok(false);
    };
    urls.iter()
        .try_for_each(|url| approve(url, &secret, false))?;
    Ok(true)
}

/// `git credential approve` for one URL. `path_specific` keys the entry by the URL's path
/// whatever the user's configuration says; otherwise their configuration decides.
fn approve(url: &str, secret: &str, path_specific: bool) -> Result<(), CollapseError> {
    if url.contains(['\n', '\r', '\0']) || secret.contains(['\n', '\r', '\0']) {
        return Err(CollapseError::Git(
            "Collapse returned a URL or token git cannot store".into(),
        ));
    }
    let mut command = hidden_git();
    if path_specific {
        command.args(["-c", "credential.useHttpPath=true"]);
    }
    let mut child = command
        .args(["credential", "approve"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| CollapseError::Git(format!("Could not run git: {error}")))?;
    let request = format!("url={url}\nusername={USERNAME}\npassword={secret}\n\n");
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

/// The password the helper holds for `url`, or `None` when no helper answers. Never
/// prompts: with the terminal and askpass programs off, an unanswered `git credential
/// fill` fails instead of opening a window.
fn held_secret(url: &str) -> Result<Option<String>, CollapseError> {
    if url.contains(['\n', '\r', '\0']) {
        return Ok(None);
    }
    let mut child = hidden_git()
        .args(["-c", "core.askPass=", "credential", "fill"])
        .env("GIT_ASKPASS", "")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| CollapseError::Git(format!("Could not run git: {error}")))?;
    if let Some(mut input) = child.stdin.take() {
        input
            .write_all(format!("url={url}\n\n").as_bytes())
            .map_err(|error| CollapseError::Git(error.to_string()))?;
    }
    let output = child
        .wait_with_output()
        .map_err(|error| CollapseError::Git(error.to_string()))?;
    if !output.status.success() {
        return Ok(None);
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .find_map(|line| line.strip_prefix("password="))
        .filter(|secret| !secret.is_empty())
        .map(str::to_owned))
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
/// This is the crate's one process helper, so it alone may call `Command::new` and set
/// creation flags (`clippy.toml`, #232, #276).
#[allow(clippy::disallowed_methods)]
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
