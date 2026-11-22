use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use serde::{Deserialize, Serialize};

use crate::issue_types::content_hash;
use crate::scout_directory::ReproductionDirectory;
use crate::scout_issues::ScoutIssueError;

/// Backend-only reproduction command. It is deliberately not an IPC command:
/// the frontend must never be able to submit a shell command (§3).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Reproduction {
    pub program: PathBuf,
    #[serde(default)]
    pub arguments: Vec<String>,
    pub script: PathBuf,
    #[serde(default)]
    pub script_arguments: Vec<String>,
}

pub(crate) struct FailureEvidence {
    pub attachment: String,
}

impl Reproduction {
    pub(crate) fn run(&self, directory: &Path) -> Result<FailureEvidence, ScoutIssueError> {
        let directory = std::fs::canonicalize(directory)?;
        let script = std::fs::canonicalize(directory.join(&self.script))?;
        let source = read_source(&script)?;
        let working_directory = ReproductionDirectory::new(&directory)?;
        let mut command = crate::command::hidden_command(&self.program);
        crate::command::clean_toolchain_env(&mut command);
        command
            .current_dir(working_directory.path())
            .args(&self.arguments)
            .arg(working_directory.script_path(&directory, &script))
            .args(&self.script_arguments)
            .stdin(Stdio::null());
        let output = capture(&mut command)?;
        if output.status.success() {
            return Err(ScoutIssueError::NotReproduced);
        }
        if read_source(&script)? != source {
            return Err(ScoutIssueError::Invalid(
                "Evidence script changed while it ran; reproduce again".into(),
            ));
        }
        let stdout = String::from_utf8(output.stdout).map_err(invalid_utf8)?;
        let stderr = String::from_utf8(output.stderr).map_err(invalid_utf8)?;
        if let Some(detail) = detect_setup_failure(&stdout, &stderr) {
            return Err(ScoutIssueError::SetupFailed(detail));
        }
        self.attachment(&source, &output.status.to_string(), &stdout, &stderr)
    }

    fn attachment(
        &self,
        source: &str,
        exit_status: &str,
        stdout: &str,
        stderr: &str,
    ) -> Result<FailureEvidence, ScoutIssueError> {
        let command = serde_json::to_string(self)?;
        let source_hash = content_hash(source.as_bytes())?;
        let mut attachment = format!(
            "## Failure evidence\n\nReproduction arguments (JSON; run without a shell):\n\n{}\n\nScript: {} UTF-8 bytes. SHA-256: {source_hash}.\n\nStatus: {exit_status}.\n\nOutput: {} stdout bytes, {} stderr bytes.\n\n",
            fenced(&command),
            source.len(),
            stdout.len(),
            stderr.len()
        );
        attachment.push_str(&format!(
            "### Reproducer\n\n{}\n\n### Standard output\n\n{}\n\n### Standard error\n\n{}",
            fenced(source),
            fenced(stdout),
            fenced(stderr)
        ));
        if attachment.len() > 56_000 {
            return Err(ScoutIssueError::Invalid(
                "Evidence exceeds the issue body budget; use a smaller reproduction".into(),
            ));
        }
        Ok(FailureEvidence { attachment })
    }
}

fn read_source(path: &Path) -> Result<String, ScoutIssueError> {
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(48_001)
        .read_to_end(&mut bytes)?;
    let source = String::from_utf8(bytes).map_err(invalid_utf8)?;
    if source.trim().is_empty() || source.len() > 48_000 {
        return Err(ScoutIssueError::Invalid(
            "Evidence script must contain 1–48000 UTF-8 bytes".into(),
        ));
    }
    Ok(source)
}

pub(crate) fn capture(command: &mut Command) -> io::Result<Output> {
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| io::Error::other("Missing reproduction stdout"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| io::Error::other("Missing reproduction stderr"))?;
    std::thread::scope(|scope| {
        let stdout = scope.spawn(|| read_output(stdout));
        let stderr = scope.spawn(|| read_output(stderr));
        let status = child.wait()?;
        let stdout = stdout
            .join()
            .map_err(|_| io::Error::other("Could not capture reproduction stdout"))??;
        let stderr = stderr
            .join()
            .map_err(|_| io::Error::other("Could not capture reproduction stderr"))??;
        Ok(Output {
            status,
            stdout,
            stderr,
        })
    })
}

fn read_output(mut pipe: impl Read) -> io::Result<Vec<u8>> {
    let mut output = Vec::new();
    pipe.by_ref().take(56_001).read_to_end(&mut output)?;
    // Drain oversized output without retaining it, so a noisy stress script can
    // finish without either exhausting memory or blocking on a full pipe.
    io::copy(&mut pipe, &mut io::sink())?;
    if output.len() > 56_000 {
        return Err(io::Error::other(
            "Evidence output exceeds the issue body budget",
        ));
    }
    Ok(output)
}

pub(crate) fn fenced(value: &str) -> String {
    // Reproducers can contain Markdown themselves. The delimiter must be longer
    // than any run in the attached bytes so no code escapes its evidence block.
    let longest = value
        .split(|character| character != '`')
        .map(str::len)
        .max()
        .unwrap_or(0);
    let fence = "`".repeat(3.max(longest + 1));
    let separator = if value.ends_with('\n') { "" } else { "\n" };
    format!("{fence}\n{value}{separator}{fence}")
}

fn invalid_utf8(error: std::string::FromUtf8Error) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, error)
}

/// Distinguishes package or environment setup failures (such as Go `FAIL <pkg> [setup failed]`
/// or `//go:embed` missing-file errors) from genuine failing bug reproductions (#643).
pub(crate) fn detect_setup_failure(stdout: &str, stderr: &str) -> Option<String> {
    let mut reasons = Vec::new();
    for line in stderr.lines().chain(stdout.lines()) {
        if is_go_setup_failed_line(line) {
            reasons.push(line.trim().to_owned());
        }
    }
    if reasons.is_empty() && !has_go_test_failure(stdout, stderr) {
        for line in stderr.lines().chain(stdout.lines()) {
            if is_go_embed_missing_line(line) || is_go_missing_module_line(line) {
                reasons.push(line.trim().to_owned());
            }
        }
    }
    if reasons.is_empty() {
        return None;
    }
    reasons.dedup();
    let summary = reasons.into_iter().take(4).collect::<Vec<_>>().join(" | ");
    let details = diagnostic_excerpt(stdout, stderr);
    if details.is_empty() {
        Some(summary)
    } else {
        Some(format!("{summary}\n{details}"))
    }
}

fn is_go_setup_failed_line(line: &str) -> bool {
    let trimmed = line.trim();
    let Some(rest) = trimmed.strip_prefix("FAIL") else {
        return false;
    };
    rest.starts_with(char::is_whitespace) && rest.ends_with("[setup failed]")
}

fn is_go_embed_missing_line(line: &str) -> bool {
    let trimmed = line.trim();
    let Some((_, after_go)) = trimmed.split_once(".go:") else {
        return false;
    };
    after_go.contains(": pattern ") && after_go.ends_with(": no matching files found")
}

fn is_go_missing_module_line(line: &str) -> bool {
    let trimmed = line.trim();
    trimmed == "go: go.mod file not found in current directory or any parent directory"
        || trimmed
            .starts_with("go: go.mod file not found in current directory or any parent directory;")
}

fn has_go_test_failure(stdout: &str, stderr: &str) -> bool {
    stdout
        .lines()
        .chain(stderr.lines())
        .any(|line| line.trim_start().starts_with("--- FAIL:"))
}

fn diagnostic_excerpt(stdout: &str, stderr: &str) -> String {
    let mut combined = String::new();
    let stderr = stderr.trim();
    let stdout = stdout.trim();
    if !stderr.is_empty() {
        combined.push_str(stderr);
    }
    if !stdout.is_empty() {
        if !combined.is_empty() {
            combined.push('\n');
        }
        combined.push_str(stdout);
    }
    velocity_core::text_budget::preview(&combined, 1_500)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_go_setup_and_embed_failures_while_accepting_real_test_failures() {
        // Risk (#643): scout_file must refuse Go package setup failures (such as missing
        // //go:embed SQL migrations) rather than publishing them as reproduction evidence,
        // while still accepting genuine failing Go tests and script assertions.
        let setup_stdout = "FAIL\tcollapse.sh/server [setup failed]\nFAIL\n";
        let setup_stderr = "# collapse.sh/server/internal/meters\ninternal\\meters\\store.go:12:12: pattern migrations/*.sql: no matching files found\n";
        let detected = detect_setup_failure(setup_stdout, setup_stderr);
        assert!(
            detected
                .as_deref()
                .is_some_and(|message| message.contains("[setup failed]")
                    && message.contains("pattern migrations/*.sql: no matching files found")),
            "expected setup failure detection, got {detected:?}"
        );

        assert!(detect_setup_failure("", setup_stderr).is_some());
        assert!(
            detect_setup_failure(
                "",
                "go: go.mod file not found in current directory or any parent directory; see 'go help modules'\n"
            )
            .is_some()
        );

        let real_go_failure = "=== RUN   TestC9ReadOnlyRouteContract\n\
            --- PASS: TestC9ReadOnlyRouteContract (0.04s)\n\
            === RUN   TestScoutMCPRepositoryListCap\n\
                zz_scout_repository_list_cap_test.go:52: MCP returned 50 of 51 repositories\n\
            --- FAIL: TestScoutMCPRepositoryListCap (0.08s)\n\
            FAIL\n\
            FAIL\tcollapse.sh/server\t0.512s\n";
        assert_eq!(detect_setup_failure(real_go_failure, ""), None);

        let embed_assertion_test = "--- FAIL: TestEmbedPatternError (0.00s)\n\
            \tembed_test.go:18: got main.go:3:12: pattern migrations/*.sql: no matching files found\n\
            FAIL\texample.com/t\t0.02s\n";
        assert_eq!(detect_setup_failure(embed_assertion_test, ""), None);

        assert_eq!(
            detect_setup_failure("", "AssertionError: expected 51 repositories, got 50\n"),
            None
        );
    }

    #[test]
    fn reproduction_run_refuses_setup_failure_output() -> Result<(), Box<dyn std::error::Error>> {
        // Risk (#643): executing a failing script that only reports Go [setup failed] must
        // return ScoutIssueError::SetupFailed instead of producing an issue attachment.
        let workspace = tempfile::tempdir()?;
        #[cfg(windows)]
        let (program, arguments, script_name, contents) = (
            PathBuf::from("cmd"),
            vec!["/D".to_owned(), "/C".to_owned()],
            "setup_fail.cmd",
            "@echo off\r\n>&2 echo main.go:3:12: pattern migrations/*.sql: no matching files found\r\necho FAIL\texample.com/t [setup failed]\r\nexit /b 1\r\n",
        );
        #[cfg(not(windows))]
        let (program, arguments, script_name, contents) = (
            PathBuf::from("sh"),
            Vec::new(),
            "setup_fail.sh",
            "#!/bin/sh\necho 'main.go:3:12: pattern migrations/*.sql: no matching files found' >&2\nprintf 'FAIL\\texample.com/t [setup failed]\\n'\nexit 1\n",
        );
        std::fs::write(workspace.path().join(script_name), contents)?;
        let reproduction = Reproduction {
            program,
            arguments,
            script: PathBuf::from(script_name),
            script_arguments: Vec::new(),
        };
        let outcome = reproduction.run(workspace.path());
        assert!(
            matches!(outcome, Err(ScoutIssueError::SetupFailed(ref detail)) if detail.contains("[setup failed]")),
            "expected SetupFailed, got {:?}",
            outcome.err()
        );
        Ok(())
    }

    #[test]
    fn reproduction_run_uses_non_verbatim_paths_and_accepts_real_failure()
    -> Result<(), Box<dyn std::error::Error>> {
        // Risk (#643): scout_file must pass non-verbatim paths for short Windows checkouts
        // (so Go //go:embed and similar tools see workspace files) and accept real failures.
        let workspace = tempfile::tempdir()?;
        #[cfg(windows)]
        let (program, arguments, script_name, contents) = (
            PathBuf::from("cmd"),
            vec!["/D".to_owned(), "/C".to_owned()],
            "real_fail.cmd",
            "@echo off\r\nif \"%CD:~0,4%\"==\"\\\\?\\\" exit /b 0\r\nset \"SCRIPT=%~f0\"\r\nif \"%SCRIPT:~0,4%\"==\"\\\\?\\\" exit /b 0\r\necho assertion failed: expected 51 repositories, got 50\r\nexit /b 1\r\n",
        );
        #[cfg(not(windows))]
        let (program, arguments, script_name, contents) = (
            PathBuf::from("sh"),
            Vec::new(),
            "real_fail.sh",
            "#!/bin/sh\necho 'assertion failed: expected 51 repositories, got 50'\nexit 1\n",
        );
        std::fs::write(workspace.path().join(script_name), contents)?;
        let reproduction = Reproduction {
            program,
            arguments,
            script: PathBuf::from(script_name),
            script_arguments: Vec::new(),
        };
        let evidence = reproduction.run(workspace.path())?;
        assert!(
            evidence
                .attachment
                .contains("assertion failed: expected 51 repositories, got 50")
        );
        Ok(())
    }
}
