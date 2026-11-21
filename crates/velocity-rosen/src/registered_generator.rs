//! Registered generator commands from `[generated.commands]` in the base's `rosen.toml` (#582).
//!
//! Agents may queue an exact registered generator command through `velocity-run`'s `queue_run`
//! or `rosen run --` in the light run queue. Velocity snapshots the workspace before and after
//! the run and refuses (reverting) any change outside the command's registered path pattern(s).

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use rosen_client::protocol::{DiffRequest, ReadAnswer, ReadRequest};
use rosen_client::snapshot::WorkspaceMarker;
use rosen_client::{Client, Endpoint, ObjectId};
use rosen_core::PathPattern;

use crate::RosenError;
use crate::agent_run::executable_name;

/// Timeout for reading `[generated.commands]` from the local Rosen server.
pub const BASE_CONFIG_TIMEOUT: Duration = Duration::from_secs(5);

/// How a matched `[generated.commands]` command executes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GeneratorExecution {
    /// A single executable with optional leading `KEY=VALUE` environment variables.
    Direct {
        env: Vec<(String, String)>,
        program: String,
        arguments: Vec<String>,
    },
    /// A command using shell operators (`&&`, `|`, `>`, `;`, etc.), run verbatim via shell.
    Shell(String),
}

/// A matched `[generated.commands]` entry from the base's `rosen.toml`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RegisteredGenerator {
    /// Exact command string registered in the base's `[generated.commands]`.
    pub command: String,
    /// Parsed execution form used when spawning the command.
    pub execution: GeneratorExecution,
    /// All `[generated.commands]` path patterns whose registered command equals `command`.
    pub patterns: Vec<PathPattern>,
}

/// Workspace baseline captured right before running a registered generator.
pub struct GeneratorGuard {
    endpoint: String,
    worktree: PathBuf,
    before_tree: ObjectId,
    command: String,
    patterns: Vec<PathPattern>,
}

/// Loads `[generated.commands]` from the `rosen.toml` in force at the server's current base.
pub async fn load_base_commands(
    endpoint: &str,
) -> Result<BTreeMap<PathPattern, String>, RosenError> {
    let client = crate::connection::requests(&Endpoint::new(endpoint)).await?;
    Ok(client.base_config().await?.generated_commands)
}

/// Blocking wrapper for CLI shims (`rosen run --`) to read `[generated.commands]` from base.
pub(crate) fn load_base_commands_blocking(endpoint: &str) -> Option<BTreeMap<PathPattern, String>> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .ok()?;
    runtime
        .block_on(async {
            tokio::time::timeout(BASE_CONFIG_TIMEOUT, load_base_commands(endpoint)).await
        })
        .ok()?
        .ok()
}

/// Returns whether `directory` refers to the same filesystem folder as `workspace_root`.
pub fn is_workspace_root(directory: &Path, workspace_root: &Path) -> bool {
    let left = std::fs::canonicalize(directory)
        .or_else(|_| std::path::absolute(directory))
        .ok();
    let right = std::fs::canonicalize(workspace_root)
        .or_else(|_| std::path::absolute(workspace_root))
        .ok();
    match (left, right) {
        (Some(left), Some(right)) => left == right,
        _ => directory == workspace_root,
    }
}

/// Matches an agent's `(program, arguments)` (plus ambient/explicit env lookup) against exact
/// commands registered under `[generated.commands]` in the base's `rosen.toml`.
pub fn match_registered(
    generated_commands: &BTreeMap<PathPattern, String>,
    program: &str,
    arguments: &[String],
    ambient_env: &dyn Fn(&str) -> Option<String>,
) -> Option<RegisteredGenerator> {
    if generated_commands.is_empty() {
        return None;
    }
    let mut grouped: Vec<(&str, Vec<PathPattern>)> = Vec::new();
    for (pattern, raw) in generated_commands {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Some((_, patterns)) = grouped.iter_mut().find(|(cmd, _)| *cmd == trimmed) {
            patterns.push(pattern.clone());
        } else {
            grouped.push((trimmed, vec![pattern.clone()]));
        }
    }
    for (raw, patterns) in grouped {
        let execution = parse_registered_command(raw)?;
        if matches_invocation(raw, &execution, program, arguments, ambient_env) {
            return Some(RegisteredGenerator {
                command: raw.to_owned(),
                execution,
                patterns,
            });
        }
    }
    None
}

/// Parses a registered command string into a direct executable + leading `KEY=VALUE` env pairs,
/// or falls back to a verbatim shell command when shell operators are present.
pub fn parse_registered_command(raw: &str) -> Option<GeneratorExecution> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    if has_unquoted_shell_operator(trimmed) {
        return Some(GeneratorExecution::Shell(trimmed.to_owned()));
    }
    let Some(tokens) = split_command_words(trimmed) else {
        return Some(GeneratorExecution::Shell(trimmed.to_owned()));
    };
    let (env, rest) = peel_env_assignments(&tokens);
    let (program, arguments) = rest.split_first()?;
    Some(GeneratorExecution::Direct {
        env,
        program: program.clone(),
        arguments: arguments.to_vec(),
    })
}

/// Returns paths in `changed` not covered by any pattern in `patterns`.
pub fn disallowed_paths<'a>(changed: &'a [String], patterns: &[PathPattern]) -> Vec<&'a str> {
    changed
        .iter()
        .filter(|path| {
            let normalized = path.replace('\\', "/");
            !patterns.iter().any(|pattern| pattern.matches(&normalized))
        })
        .map(String::as_str)
        .collect()
}

/// A command unwrapped from an `env` or leading `KEY=VALUE` prefix.
pub(crate) struct UnwrappedCommand<'a> {
    pub env: Vec<(&'a str, &'a str)>,
    pub program: &'a str,
    pub arguments: &'a [String],
}

/// Extracts explicit env assignments and the wrapped command when an agent passes leading
/// `KEY=VALUE` assignments or `env KEY=VALUE ...` directly.
pub(crate) fn unwrap_env_prefix<'a>(
    program: &'a str,
    arguments: &'a [String],
) -> Option<UnwrappedCommand<'a>> {
    if executable_name(program) == "env" {
        let mut env = Vec::new();
        let mut index = 0;
        while let Some(token) = arguments.get(index) {
            if token == "--" {
                index += 1;
                break;
            }
            if let Some(pair) = split_env_assignment(token) {
                env.push(pair);
                index += 1;
            } else {
                break;
            }
        }
        let target = arguments.get(index)?.as_str();
        return Some(UnwrappedCommand {
            env,
            program: target,
            arguments: &arguments[index + 1..],
        });
    }
    let first = split_env_assignment(program)?;
    let mut env = vec![first];
    let mut index = 0;
    while let Some(token) = arguments.get(index) {
        if let Some(pair) = split_env_assignment(token) {
            env.push(pair);
            index += 1;
        } else {
            break;
        }
    }
    let target = arguments.get(index)?.as_str();
    Some(UnwrappedCommand {
        env,
        program: target,
        arguments: &arguments[index + 1..],
    })
}

/// Captures the workspace baseline snapshot before running `generator`.
pub async fn begin_guard(
    endpoint: &str,
    worktree: &Path,
    generator: &RegisteredGenerator,
) -> Result<GeneratorGuard, String> {
    let root = WorkspaceMarker::find_root(worktree).ok_or_else(|| {
        format!(
            "Registered generator `{}` requires a Rosen workspace, but {} has no .rosen marker.",
            generator.command,
            worktree.display()
        )
    })?;
    let client = crate::connection::requests(&Endpoint::new(endpoint))
        .await
        .map_err(|error| format!("Could not connect to Rosen before generator run: {error}"))?;
    let report = client
        .snapshot_workspace(&root, None)
        .await
        .map_err(|error| format!("Could not snapshot workspace before generator run: {error}"))?;
    if !report.unreadable.is_empty() {
        return Err(format!(
            "Cannot run registered generator `{}` while files are held open: {}",
            generator.command,
            report.unreadable.join(", ")
        ));
    }
    Ok(GeneratorGuard {
        endpoint: endpoint.to_owned(),
        worktree: root,
        before_tree: report.tree,
        command: generator.command.clone(),
        patterns: generator.patterns.clone(),
    })
}

/// Snapshots the workspace after a registered generator finishes and refuses (reverting all
/// changes from the run) if any path outside `guard.patterns` changed.
pub async fn finish_guard(guard: GeneratorGuard, succeeded: bool) -> Result<(), String> {
    let client = crate::connection::requests(&Endpoint::new(&guard.endpoint))
        .await
        .map_err(|error| format!("Could not connect to Rosen after generator run: {error}"))?;
    let after = client
        .snapshot_workspace(&guard.worktree, None)
        .await
        .map_err(|error| format!("Could not snapshot workspace after generator run: {error}"))?;
    if !after.unreadable.is_empty() {
        return Err(format!(
            "Registered generator `{}` left files held open: {}",
            guard.command,
            after.unreadable.join(", ")
        ));
    }
    if after.tree == guard.before_tree {
        return Ok(());
    }
    let diff = client
        .diff(DiffRequest {
            from: Some(guard.before_tree.to_string()),
            to: Some(after.tree.to_string()),
            context: Some(0),
            ..DiffRequest::default()
        })
        .await
        .map_err(|error| format!("Could not diff workspace after generator run: {error}"))?;
    let changed: Vec<String> = diff.files.iter().map(|file| file.path.clone()).collect();
    let disallowed = disallowed_paths(&changed, &guard.patterns);
    if disallowed.is_empty() {
        return Ok(());
    }
    let disallowed_list = disallowed.join(", ");
    let pattern_list = guard
        .patterns
        .iter()
        .map(PathPattern::as_str)
        .collect::<Vec<_>>()
        .join(", ");
    revert_diff(&client, &guard.worktree, guard.before_tree, &diff.files).await?;
    let _ = client.snapshot_workspace(&guard.worktree, None).await;
    let status_note = if succeeded {
        ""
    } else {
        " (the generator also exited with an error)"
    };
    Err(format!(
        "Refused registered generator `{}`: changed path(s) outside {pattern_list}: \
         {disallowed_list}{status_note}. All changes from the run were reverted.",
        guard.command
    ))
}

/// Blocking wrapper for [`begin_guard`] used by the `rosen run --` CLI shim.
pub(crate) fn begin_guard_blocking(
    endpoint: &str,
    worktree: &Path,
    generator: &RegisteredGenerator,
) -> Result<GeneratorGuard, String> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| error.to_string())?;
    runtime.block_on(begin_guard(endpoint, worktree, generator))
}

/// Blocking wrapper for [`finish_guard`] used by the `rosen run --` CLI shim.
pub(crate) fn finish_guard_blocking(guard: GeneratorGuard, succeeded: bool) -> Result<(), String> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| error.to_string())?;
    runtime.block_on(finish_guard(guard, succeeded))
}

async fn revert_diff(
    client: &Client,
    worktree: &Path,
    before_tree: ObjectId,
    files: &[rosen_client::protocol::FileDiff],
) -> Result<(), String> {
    // Remove added paths first so restoring a file that was replaced by a directory works cleanly.
    for file in files.iter().filter(|file| file.old.is_none()) {
        remove_workspace_path(worktree, &file.path)?;
    }
    let before = before_tree.to_string();
    for file in files.iter().filter(|file| file.old.is_some()) {
        restore_workspace_file(client, worktree, &before, &file.path).await?;
    }
    Ok(())
}

fn remove_workspace_path(worktree: &Path, rel_path: &str) -> Result<(), String> {
    let target = safe_join(worktree, rel_path)?;
    if let Ok(meta) = std::fs::symlink_metadata(&target) {
        if meta.file_type().is_dir() {
            std::fs::remove_dir_all(&target)
                .map_err(|error| format!("Could not remove directory `{rel_path}`: {error}"))?;
        } else {
            std::fs::remove_file(&target)
                .map_err(|error| format!("Could not remove file `{rel_path}`: {error}"))?;
        }
    }
    prune_empty_parents(worktree, target.parent());
    Ok(())
}

async fn restore_workspace_file(
    client: &Client,
    worktree: &Path,
    before_tree: &str,
    rel_path: &str,
) -> Result<(), String> {
    let target = safe_join(worktree, rel_path)?;
    let answer = client
        .read_file(ReadRequest {
            at: Some(before_tree.to_owned()),
            path: rel_path.to_owned(),
        })
        .await
        .map_err(|error| format!("Could not read original `{rel_path}` to revert: {error}"))?;
    let ReadAnswer::File { mode, data, .. } = answer else {
        return Err(format!(
            "Could not restore `{rel_path}`: original entry was not a file"
        ));
    };
    if let Ok(meta) = std::fs::symlink_metadata(&target)
        && meta.file_type().is_dir()
    {
        std::fs::remove_dir_all(&target)
            .map_err(|error| format!("Could not replace directory `{rel_path}`: {error}"))?;
    }
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent).map_err(|error| {
            format!("Could not create parent directory for `{rel_path}`: {error}")
        })?;
    }
    write_restored_bytes(&target, &mode, &data)
        .map_err(|error| format!("Could not restore `{rel_path}`: {error}"))
}

fn write_restored_bytes(target: &Path, mode: &str, data: &[u8]) -> std::io::Result<()> {
    #[cfg(unix)]
    if mode == "120000" {
        if target.symlink_metadata().is_ok() {
            let _ = std::fs::remove_file(target);
        }
        let link = std::ffi::OsString::from(String::from_utf8_lossy(data).into_owned());
        return std::os::unix::fs::symlink(link, target);
    }
    std::fs::write(target, data)?;
    #[cfg(unix)]
    if mode == "100755" {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(target, std::fs::Permissions::from_mode(0o755))?;
    }
    let _ = mode;
    Ok(())
}

fn safe_join(worktree: &Path, rel_path: &str) -> Result<PathBuf, String> {
    let mut joined = worktree.to_path_buf();
    let mut count = 0usize;
    for component in Path::new(rel_path).components() {
        match component {
            Component::Normal(part) => {
                joined.push(part);
                count += 1;
            }
            _ => return Err(format!("Refusing unsafe path `{rel_path}` during revert")),
        }
    }
    if count == 0 {
        return Err(format!("Refusing empty path `{rel_path}` during revert"));
    }
    Ok(joined)
}

fn prune_empty_parents(worktree: &Path, mut current: Option<&Path>) {
    while let Some(dir) = current {
        if dir == worktree {
            break;
        }
        if std::fs::remove_dir(dir).is_err() {
            break;
        }
        current = dir.parent();
    }
}

fn matches_invocation(
    raw: &str,
    execution: &GeneratorExecution,
    program: &str,
    arguments: &[String],
    ambient_env: &dyn Fn(&str) -> Option<String>,
) -> bool {
    if arguments.is_empty() && program.trim() == raw {
        return true;
    }
    if let Some(script) = wrapped_shell_script(program, arguments) {
        return script.trim() == raw;
    }
    let GeneratorExecution::Direct {
        env: reg_env,
        program: reg_prog,
        arguments: reg_args,
    } = execution
    else {
        return false;
    };
    let (explicit_env, invoked_program, invoked_args) = match unwrap_env_prefix(program, arguments)
    {
        Some(unwrapped) => (unwrapped.env, unwrapped.program, unwrapped.arguments),
        None => (Vec::new(), program, arguments),
    };
    if !program_matches(invoked_program, reg_prog) || invoked_args != reg_args.as_slice() {
        return false;
    }
    env_matches(reg_env, &explicit_env, ambient_env)
}

fn wrapped_shell_script<'a>(program: &str, arguments: &'a [String]) -> Option<&'a str> {
    let exe = executable_name(program);
    match (exe.as_str(), arguments) {
        ("sh" | "bash", [flag, script]) if flag == "-c" => Some(script.as_str()),
        ("cmd", [flag, script]) if flag.eq_ignore_ascii_case("/c") => Some(script.as_str()),
        ("pwsh" | "powershell", [flag, script])
            if flag.eq_ignore_ascii_case("-c") || flag.eq_ignore_ascii_case("-command") =>
        {
            Some(script.as_str())
        }
        _ => None,
    }
}

fn program_matches(invoked: &str, registered: &str) -> bool {
    if invoked == registered {
        return true;
    }
    let has_separator = |text: &str| text.contains(['/', '\\']);
    if has_separator(invoked) || has_separator(registered) {
        return false;
    }
    executable_name(invoked) == executable_name(registered)
}

fn env_matches(
    registered: &[(String, String)],
    explicit: &[(&str, &str)],
    ambient: &dyn Fn(&str) -> Option<String>,
) -> bool {
    for &(key, value) in explicit {
        if !registered
            .iter()
            .any(|(reg_key, reg_val)| reg_key == key && reg_val == value)
        {
            return false;
        }
    }
    for (reg_key, reg_val) in registered {
        let explicit_val = explicit
            .iter()
            .rev()
            .find(|(key, _)| *key == reg_key.as_str())
            .map(|(_, value)| *value);
        let satisfied = match explicit_val {
            Some(value) => value == reg_val.as_str(),
            None => ambient(reg_key).as_deref() == Some(reg_val.as_str()),
        };
        if !satisfied {
            return false;
        }
    }
    true
}

fn has_unquoted_shell_operator(text: &str) -> bool {
    let mut in_single = false;
    let mut in_double = false;
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if in_single {
            if ch == '\'' {
                in_single = false;
            }
            continue;
        }
        if in_double {
            if ch == '\\' && matches!(chars.peek(), Some('"' | '\\')) {
                let _ = chars.next();
            } else if ch == '"' {
                in_double = false;
            } else if ch == '`' || (ch == '$' && chars.peek() == Some(&'(')) {
                return true;
            }
            continue;
        }
        match ch {
            '\'' => in_single = true,
            '"' => in_double = true,
            '|' | '&' | ';' | '<' | '>' | '(' | ')' | '`' | '$' | '\n' | '\r' => return true,
            _ => {}
        }
    }
    in_single || in_double
}

fn split_command_words(text: &str) -> Option<Vec<String>> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut in_word = false;
    let mut in_single = false;
    let mut in_double = false;
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if in_single {
            if ch == '\'' {
                in_single = false;
            } else {
                current.push(ch);
            }
            continue;
        }
        if in_double {
            if ch == '\\' && matches!(chars.peek(), Some('"' | '\\')) {
                if let Some(escaped) = chars.next() {
                    current.push(escaped);
                }
            } else if ch == '"' {
                in_double = false;
            } else {
                current.push(ch);
            }
            continue;
        }
        if ch.is_whitespace() {
            if in_word {
                words.push(std::mem::take(&mut current));
                in_word = false;
            }
        } else if ch == '\'' {
            in_single = true;
            in_word = true;
        } else if ch == '"' {
            in_double = true;
            in_word = true;
        } else {
            current.push(ch);
            in_word = true;
        }
    }
    if in_single || in_double {
        return None;
    }
    if in_word {
        words.push(current);
    }
    Some(words)
}

fn peel_env_assignments(tokens: &[String]) -> (Vec<(String, String)>, &[String]) {
    let mut env = Vec::new();
    let mut index = 0;
    while let Some(token) = tokens.get(index) {
        if let Some((key, value)) = split_env_assignment(token) {
            env.push((key.to_owned(), value.to_owned()));
            index += 1;
        } else {
            break;
        }
    }
    (env, &tokens[index..])
}

pub(crate) fn split_env_assignment(token: &str) -> Option<(&str, &str)> {
    let (key, value) = token.split_once('=')?;
    let mut bytes = key.bytes();
    let first = bytes.next()?;
    if !(first.is_ascii_alphabetic() || first == b'_') {
        return None;
    }
    if !bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_') {
        return None;
    }
    Some((key, value))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_commands() -> BTreeMap<PathPattern, String> {
        let mut map = BTreeMap::new();
        if let Ok(pattern) = PathPattern::new("fixtures/collapse/**") {
            map.insert(
                pattern,
                "UPDATE_FIXTURES=1 cargo test -p rosen-server --lib collapse_fixtures".to_owned(),
            );
        }
        map
    }

    // Risk (#582): agents must be able to queue the exact registered `[generated.commands]`
    // command (with its env prefix) while omitting the env var, adding flags, or running
    // plain `cargo test` stays refused.
    #[test]
    fn matches_exact_registered_generator_and_refuses_variants() {
        let commands = fixture_commands();
        let no_env = |_: &str| None;
        let with_update_env = |key: &str| (key == "UPDATE_FIXTURES").then(|| "1".to_owned());
        let with_wrong_env = |key: &str| (key == "UPDATE_FIXTURES").then(|| "0".to_owned());

        let cargo_args: Vec<String> = ["test", "-p", "rosen-server", "--lib", "collapse_fixtures"]
            .into_iter()
            .map(str::to_owned)
            .collect();
        let prefixed_args: Vec<String> = [
            "cargo",
            "test",
            "-p",
            "rosen-server",
            "--lib",
            "collapse_fixtures",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect();
        let env_args: Vec<String> = [
            "UPDATE_FIXTURES=1",
            "cargo",
            "test",
            "-p",
            "rosen-server",
            "--lib",
            "collapse_fixtures",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect();

        // Accepted forms:
        let matched = match_registered(&commands, "UPDATE_FIXTURES=1", &prefixed_args, &no_env);
        assert!(matched.is_some());
        assert_eq!(
            matched.as_ref().map(|item| &item.execution),
            Some(&GeneratorExecution::Direct {
                env: vec![("UPDATE_FIXTURES".to_owned(), "1".to_owned())],
                program: "cargo".to_owned(),
                arguments: cargo_args.clone(),
            })
        );
        assert!(match_registered(&commands, "env", &env_args, &no_env).is_some());
        assert!(match_registered(&commands, "cargo", &cargo_args, &with_update_env).is_some());
        assert!(
            match_registered(
                &commands,
                "sh",
                &[
                    "-c".to_owned(),
                    "UPDATE_FIXTURES=1 cargo test -p rosen-server --lib collapse_fixtures"
                        .to_owned()
                ],
                &no_env,
            )
            .is_some()
        );
        assert!(
            match_registered(
                &commands,
                "UPDATE_FIXTURES=1 cargo test -p rosen-server --lib collapse_fixtures",
                &[],
                &no_env,
            )
            .is_some()
        );

        // Refused forms:
        assert!(match_registered(&commands, "cargo", &cargo_args, &no_env).is_none());
        assert!(match_registered(&commands, "cargo", &cargo_args, &with_wrong_env).is_none());
        assert!(
            match_registered(&commands, "cargo", &["test".to_owned()], &with_update_env).is_none()
        );
        let mut extra_arg = prefixed_args;
        extra_arg.push("--nocapture".to_owned());
        assert!(match_registered(&commands, "UPDATE_FIXTURES=1", &extra_arg, &no_env).is_none());
        assert!(
            match_registered(
                &commands,
                "sh",
                &[
                    "-c".to_owned(),
                    "UPDATE_FIXTURES=1 cargo test -p rosen-server --lib collapse_fixtures && echo hi"
                        .to_owned()
                ],
                &no_env,
            )
            .is_none()
        );
    }

    // Risk (#582): only paths matched by the command's registered pattern(s) may change.
    #[test]
    fn disallowed_paths_catches_changes_outside_registered_pattern() {
        let Ok(pattern) = PathPattern::new("fixtures/collapse/**") else {
            panic!("valid pattern");
        };
        let allowed = vec![
            "fixtures/collapse/events.json".to_owned(),
            "fixtures/collapse/nested/wire.json".to_owned(),
        ];
        assert!(disallowed_paths(&allowed, std::slice::from_ref(&pattern)).is_empty());

        let mixed = vec![
            "fixtures/collapse/events.json".to_owned(),
            "crates/rosen-server/src/lib.rs".to_owned(),
            "Cargo.lock".to_owned(),
        ];
        assert_eq!(
            disallowed_paths(&mixed, &[pattern]),
            vec!["crates/rosen-server/src/lib.rs", "Cargo.lock"]
        );
    }
}
