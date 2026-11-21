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
use std::path::Path;
use std::process::{Command, Stdio};

use crate::CollapseError;

/// The URL scope the per-path setting is saved under in the user's git configuration.
const HOST_SCOPE: &str = "https://api.collapse.sh";
/// The URL scope `http.<url>.proactiveAuth` is saved under in global git configuration (#571).
const PROACTIVE_AUTH_SCOPE: &str = "https://api.collapse.sh/";
/// Global git config key enabling proactive Basic auth on private Collapse reads (#571).
pub const PROACTIVE_AUTH_KEY: &str = "http.https://api.collapse.sh/.proactiveAuth";
/// Value for `http.<url>.proactiveAuth` so Git sends credentials without waiting for a 401.
const PROACTIVE_AUTH_VALUE: &str = "basic";
/// Git 2.46 introduced `http.proactiveAuth` (#571).
pub const MIN_PROACTIVE_AUTH_GIT_VERSION: (u32, u32) = (2, 46);
/// Collapse accepts any username with a push token; its link panel uses `rosen`.
const USERNAME: &str = "rosen";

/// State of Git's proactive HTTP auth configuration for Collapse mirror reads (#571).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProactiveAuthStatus {
    /// Git 2.46 or later is installed and `http.proactiveAuth` resolves to `basic` for Collapse.
    Configured,
    /// Git is older than 2.46 (or its version could not be read), so `http.proactiveAuth`
    /// is unsupported.
    UnsupportedGit { reason: String },
    /// Git 2.46 or later is installed, but `http.proactiveAuth` is not configured as `basic`.
    Missing { current: Option<String> },
}

/// Saves `secret` in the git credential helper under each of `urls`, keyed by path.
/// Velocity keeps no copy; the helper owns it from here, as `git credential approve` does
/// for a person.
pub fn store_push_secret(urls: &[String], secret: &str) -> Result<(), CollapseError> {
    let Some(first) = urls.first() else {
        return Ok(());
    };
    ensure_proactive_auth()?;
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
    ensure_proactive_auth()?;
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

/// Reports whether git's credential helper holds a password for `url`, without returning
/// or logging the secret (#576).
pub fn has_push_secret(url: &str) -> Result<bool, CollapseError> {
    held_secret(url).map(|secret| secret.is_some())
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

/// Checks that the installed Git supports `http.proactiveAuth` (Git 2.46 or later, #571).
pub fn check_git_version() -> Result<(u32, u32), CollapseError> {
    let output = hidden_git()
        .arg("--version")
        .output()
        .map_err(|error| CollapseError::Git(format!("Could not run git: {error}")))?;
    let (min_major, min_minor) = MIN_PROACTIVE_AUTH_GIT_VERSION;
    if !output.status.success() {
        return Err(CollapseError::Git(format!(
            "Collapse mirror reads need Git {min_major}.{min_minor} or later for http.proactiveAuth, but `git --version` failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    match parse_git_version(&stdout) {
        Some(version) if version >= MIN_PROACTIVE_AUTH_GIT_VERSION => Ok(version),
        Some((major, minor)) => Err(CollapseError::Git(format!(
            "Collapse mirror reads need Git {min_major}.{min_minor} or later for http.proactiveAuth (found Git {major}.{minor}). Update Git and try again."
        ))),
        None => Err(CollapseError::Git(format!(
            "Collapse mirror reads need Git {min_major}.{min_minor} or later for http.proactiveAuth, but could not parse `git --version` output: {}",
            stdout.trim()
        ))),
    }
}

/// Parses `(major, minor)` from `git --version` output, e.g. `git version 2.46.0.windows.1`.
fn parse_git_version(output: &str) -> Option<(u32, u32)> {
    let token = output.split_whitespace().find(|part| {
        part.chars().next().is_some_and(|ch| ch.is_ascii_digit()) && part.contains('.')
    })?;
    let mut pieces = token.split('.');
    let major = parse_leading_u32(pieces.next()?)?;
    let minor = parse_leading_u32(pieces.next()?)?;
    Some((major, minor))
}

fn parse_leading_u32(segment: &str) -> Option<u32> {
    let digits: String = segment.chars().take_while(char::is_ascii_digit).collect();
    (!digits.is_empty()).then(|| digits.parse().ok()).flatten()
}

/// Sets `http.https://api.collapse.sh/.proactiveAuth basic` in global git config so reads
/// of private Collapse repositories send credentials instead of receiving 404 (#571).
/// Checks for Git 2.46 or later first and refuses when Git is older.
pub fn ensure_proactive_auth() -> Result<(), CollapseError> {
    check_git_version()?;
    if git_output(&["config", "--global", "--get", PROACTIVE_AUTH_KEY])?
        .trim()
        .eq_ignore_ascii_case(PROACTIVE_AUTH_VALUE)
    {
        return Ok(());
    }
    let output = hidden_git()
        .args([
            "config",
            "--global",
            "--replace-all",
            PROACTIVE_AUTH_KEY,
            PROACTIVE_AUTH_VALUE,
        ])
        .output()
        .map_err(|error| CollapseError::Git(format!("Could not run git: {error}")))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(CollapseError::Git(format!(
            "Could not save {PROACTIVE_AUTH_KEY} in your git configuration: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )))
    }
}

/// Checks whether Git supports `http.proactiveAuth` and resolves it to `basic` for Collapse (#571).
pub fn check_proactive_auth(mirror_url: Option<&str>) -> ProactiveAuthStatus {
    if let Err(error) = check_git_version() {
        return ProactiveAuthStatus::UnsupportedGit {
            reason: error.to_string(),
        };
    }
    let target = mirror_url
        .map(str::trim)
        .filter(|url| {
            url.get(..PROACTIVE_AUTH_SCOPE.len())
                .is_some_and(|head| head.eq_ignore_ascii_case(PROACTIVE_AUTH_SCOPE))
        })
        .unwrap_or(PROACTIVE_AUTH_SCOPE);
    match git_output(&["config", "--get-urlmatch", "http.proactiveAuth", target]) {
        Ok(value) if value.trim().eq_ignore_ascii_case(PROACTIVE_AUTH_VALUE) => {
            ProactiveAuthStatus::Configured
        }
        Ok(value) => {
            let trimmed = value.trim();
            ProactiveAuthStatus::Missing {
                current: (!trimmed.is_empty()).then(|| trimmed.to_owned()),
            }
        }
        Err(error) => ProactiveAuthStatus::Missing {
            current: Some(error.to_string()),
        },
    }
}

/// Clones a Collapse repository into `target` using proactive Basic auth, per-path
/// credentials and Windows long-path support (#571, #575).
pub fn clone_repository(mirror_url: &str, target: &Path) -> Result<(), CollapseError> {
    if mirror_url.contains(['\n', '\r', '\0']) {
        return Err(CollapseError::Git(
            "Collapse returned a mirror URL git cannot clone".into(),
        ));
    }
    ensure_proactive_auth()?;
    ensure_path_specific_credentials()?;
    let path_key = format!("credential.{HOST_SCOPE}.useHttpPath=true");
    let auth_key = format!("{PROACTIVE_AUTH_KEY}={PROACTIVE_AUTH_VALUE}");
    let output = hidden_git()
        .args([
            "-c",
            "core.longpaths=true",
            "-c",
            &path_key,
            "-c",
            &auth_key,
            "clone",
            "--quiet",
            "--config",
            "core.longpaths=true",
            "--",
            mirror_url,
        ])
        .arg(target)
        .output()
        .map_err(|error| CollapseError::Git(format!("Could not run git: {error}")))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(CollapseError::Git(format!(
            "Could not clone {mirror_url}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )))
    }
}

/// The Collapse `owner/repository` that `folder`'s `origin` remote points to, or `None`
/// when `folder` is not a Collapse checkout (#575).
pub fn checkout_repository(folder: &Path) -> Option<String> {
    let output = hidden_git()
        .arg("-C")
        .arg(folder)
        .args(["remote", "get-url", "origin"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let remote = String::from_utf8_lossy(&output.stdout);
    let trimmed = remote.trim();
    for host in [
        "https://api.collapse.sh/",
        "https://collapse.sh/",
        "https://git.collapse.sh/",
    ] {
        if let Some(head) = trimmed.get(..host.len())
            && head.eq_ignore_ascii_case(host)
        {
            let rest = trimmed[host.len()..].trim_end_matches('/');
            let rest = rest.strip_suffix(".git").unwrap_or(rest);
            let (owner, name) = rest.split_once('/')?;
            if !owner.is_empty() && !name.is_empty() && !name.contains('/') {
                return Some(format!("{owner}/{name}"));
            }
        }
    }
    None
}

/// Ensures `refs/tags/<tag>` on Collapse mirror `mirror_url` points to `commit` (#593).
/// Collapse creates releases against existing tags; Rosen's mirror forwards only the
/// default branch, so this pushes the tag before creating the release.
/// When `!allow_move`, an existing tag on a different commit is refused.
pub fn ensure_remote_tag(
    mirror_url: &str,
    commit: &str,
    tag: &str,
    allow_move: bool,
    local_repo: Option<&Path>,
) -> Result<(), CollapseError> {
    let commit = commit.trim().to_ascii_lowercase();
    let tag = tag.trim();
    if mirror_url.contains(['\n', '\r', '\0'])
        || tag.is_empty()
        || tag.contains(['\n', '\r', '\0', ' '])
        || commit.len() != 40
        || !commit.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(CollapseError::Git(
            "Invalid Collapse mirror URL, tag or commit ID".into(),
        ));
    }
    ensure_proactive_auth()?;
    ensure_path_specific_credentials()?;
    if let Some(existing) = remote_tag_commit(mirror_url, tag)? {
        if existing.eq_ignore_ascii_case(&commit) {
            return Ok(());
        }
        if !allow_move {
            return Err(CollapseError::Git(format!(
                "Tag {tag} already exists on {existing} on Collapse, not {commit}"
            )));
        }
    }
    push_tag_with_temp_repo(mirror_url, &commit, tag, allow_move, local_repo)
}

fn remote_tag_commit(mirror_url: &str, tag: &str) -> Result<Option<String>, CollapseError> {
    let ref_name = format!("refs/tags/{tag}");
    let peeled_ref = format!("{ref_name}^{{}}");
    let output = collapse_git()
        .args([
            "ls-remote",
            "--tags",
            "--",
            mirror_url,
            &ref_name,
            &peeled_ref,
        ])
        .output()
        .map_err(|error| CollapseError::Git(format!("Could not run git: {error}")))?;
    if !output.status.success() {
        return Err(CollapseError::Git(format!(
            "Could not check tags on {mirror_url}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut direct = None;
    for line in stdout.lines() {
        let mut parts = line.split_whitespace();
        let (Some(sha), Some(reference)) = (parts.next(), parts.next()) else {
            continue;
        };
        if reference == peeled_ref {
            return Ok(Some(sha.to_ascii_lowercase()));
        }
        if reference == ref_name {
            direct = Some(sha.to_ascii_lowercase());
        }
    }
    Ok(direct)
}

fn push_tag_with_temp_repo(
    mirror_url: &str,
    commit: &str,
    tag: &str,
    allow_move: bool,
    local_repo: Option<&Path>,
) -> Result<(), CollapseError> {
    let scratch = std::env::temp_dir().join(format!(
        "velocity-collapse-tag-{}",
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::create_dir_all(&scratch).map_err(|error| {
        CollapseError::Git(format!("Could not create temp git folder: {error}"))
    })?;
    let result = push_tag_inside(&scratch, mirror_url, commit, tag, allow_move, local_repo);
    let _ = std::fs::remove_dir_all(&scratch);
    result
}

fn push_tag_inside(
    scratch: &Path,
    mirror_url: &str,
    commit: &str,
    tag: &str,
    allow_move: bool,
    local_repo: Option<&Path>,
) -> Result<(), CollapseError> {
    let init = collapse_git()
        .arg("-C")
        .arg(scratch)
        .args(["init", "--bare", "--quiet"])
        .output()
        .map_err(|error| CollapseError::Git(format!("Could not run git init: {error}")))?;
    if !init.status.success() {
        return Err(CollapseError::Git(format!(
            "Could not initialize temp repository for tagging: {}",
            String::from_utf8_lossy(&init.stderr).trim()
        )));
    }
    let mut fetched = false;
    if let Some(local) =
        local_repo.filter(|path| path.join(".git").exists() || path.join("HEAD").is_file())
    {
        let local_fetch = collapse_git()
            .arg("-C")
            .arg(scratch)
            .args(["fetch", "--quiet", "--no-tags"])
            .arg(local)
            .arg(commit)
            .output();
        if let Ok(output) = local_fetch
            && output.status.success()
        {
            fetched = true;
        }
    }
    if !fetched {
        let direct = collapse_git()
            .arg("-C")
            .arg(scratch)
            .args([
                "fetch",
                "--quiet",
                "--depth=1",
                "--filter=blob:none",
                "--",
                mirror_url,
                commit,
            ])
            .output()
            .map_err(|error| CollapseError::Git(format!("Could not run git fetch: {error}")))?;
        if direct.status.success() {
            fetched = true;
        }
    }
    if !fetched {
        let head = collapse_git()
            .arg("-C")
            .arg(scratch)
            .args([
                "fetch",
                "--quiet",
                "--filter=blob:none",
                "--",
                mirror_url,
                "HEAD",
            ])
            .output()
            .map_err(|error| CollapseError::Git(format!("Could not run git fetch: {error}")))?;
        if !head.status.success() {
            return Err(CollapseError::Git(format!(
                "Could not fetch {commit} from {mirror_url}: {}",
                String::from_utf8_lossy(&head.stderr).trim()
            )));
        }
    }
    let refspec = if allow_move {
        format!("+{commit}:refs/tags/{tag}")
    } else {
        format!("{commit}:refs/tags/{tag}")
    };
    let push = collapse_git()
        .arg("-C")
        .arg(scratch)
        .args(["push", "--quiet", "--", mirror_url, &refspec])
        .output()
        .map_err(|error| CollapseError::Git(format!("Could not run git push: {error}")))?;
    if push.status.success() {
        Ok(())
    } else {
        Err(CollapseError::Git(format!(
            "Could not push tag {tag} to {mirror_url}: {}",
            String::from_utf8_lossy(&push.stderr).trim()
        )))
    }
}

fn collapse_git() -> Command {
    let mut command = hidden_git();
    command.args([
        "-c",
        &format!("credential.{HOST_SCOPE}.useHttpPath=true"),
        "-c",
        &format!("{PROACTIVE_AUTH_KEY}={PROACTIVE_AUTH_VALUE}"),
    ]);
    command
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
