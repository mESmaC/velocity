//! Messages for Rosen land and post-land verification events.

use std::time::Duration;

use crate::scout_sweep::feed_line;

/// The most of a failed command's output its author is sent: the end, where the error is.
const OUTPUT_TAIL: usize = 4000;

/// A check command that did not pass.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FailedCheck {
    pub command: String,
    /// `None` when the command was stopped for running too long.
    pub exit_code: Option<i32>,
    /// What it printed, standard output and error together.
    pub output: String,
}

/// What the author's agent is told when its approved change no longer merges onto the base.
pub fn conflict_message(change: &str, base_branch: &str, files: &[String]) -> String {
    let mut message = format!(
        "Your Rosen change `{}` needs repair before merging onto `{base_branch}`: {}. Run `rosen rebase` in your workspace. Combine both sides of text conflict markers; regenerate generated artifacts on the combined inputs using the commands below. Run `rosen resolve <file>` for each remaining conflict, then name the work again with `rosen commit -m \"<what changed>\"` so it lands again under the project's review policy.\n",
        short_change(change),
        files.join(", "),
    );
    message.push_str(&regeneration_instructions(files));
    message
}

/// Generated files need their generators on the combined inputs, never a line merge.
pub fn regeneration_instructions(paths: &[String]) -> String {
    let mut message = String::new();
    for path in paths {
        let command = match path.replace('\\', "/").as_str() {
            "Cargo.lock" => "cargo metadata --format-version 1",
            "ui/pnpm-lock.yaml" => "pnpm -C ui install --lockfile-only",
            "ui/src/lib/bindings.ts" => "pnpm -C ui bindings",
            _ => continue,
        };
        message.push_str(&format!(
            "Regenerate `{path}`: run `{command}` from the project root.\n"
        ));
    }
    message
}

/// The change and commit recorded in Rosen's land history.
pub fn landed_line(change: &str, commit: &str, duration: Duration) -> String {
    format!(
        "Landed change {} · {} · {}",
        short_change(change),
        short_commit(commit),
        seconds(duration)
    )
}

/// A duration as the feed shows it: `0.07 s`, `9.8 s`, `104 s`.
fn seconds(duration: Duration) -> String {
    let seconds = duration.as_secs_f64();
    if seconds < 1.0 {
        format!("{seconds:.2} s")
    } else if seconds < 60.0 {
        format!("{seconds:.1} s")
    } else {
        format!("{seconds:.0} s")
    }
}

/// The feed line for a land Rosen refused for a reason no check or author fixes.
pub fn refused_line(change: &str, reason: &str) -> String {
    feed_line(&format!(
        "Land of change {} refused · {reason}",
        short_change(change)
    ))
}

/// What Rosen is told a failed post-land command printed. Rosen keeps it with the revert and
/// hands it to the reverted change's owner, so it names the command and holds only the end.
pub fn failed_check_report(failed: &FailedCheck) -> String {
    let ended = match failed.exit_code {
        Some(code) => format!("exited with status {code}"),
        None => "ran too long and was stopped".to_owned(),
    };
    format!("`{}` {ended}.\n\n{}", failed.command, tail(&failed.output))
}

/// What the author's agent is told when Rosen reverted its landed change. `report` is what
/// the checker gave Rosen for the failing run ([`failed_check_report`]).
pub fn reverted_message(change: &str, report: &str) -> String {
    // Bound the diagnostic body without dropping the command and status when a full
    // output tail is present. The author needs those to reproduce the failed build.
    let output = match report.split_once("\n\n") {
        Some((header, output)) => format!("{header}\n\n{}", tail(output)),
        None => tail(report).to_owned(),
    };
    format!(
        "Your Rosen change `{}` landed, then failed the project's post-land checks (rosen.toml). It failed twice where the commit before it passed, so Rosen reverted it on the base and reopened it. Your work is kept: this workspace holds it again on the current base. What the failing run printed:\n\n{}\n\nFix it here and run that command yourself until it passes. Then name the work again with `rosen commit -m \"<what changed>\"` so it lands again under the project's review policy. If the failure is the machine's and not your change's (a missing tool, a full disk), say so in your thread instead of changing code.\n",
        short_change(change),
        output,
    )
}

/// The feed line while the post-land checks run on a landed commit.
pub fn verifying_line(commit: &str) -> String {
    format!("Running post-land checks on {}", short_commit(commit))
}

/// The feed line for a landed commit the post-land checks passed on.
pub fn verified_line(commit: &str, took: Duration) -> String {
    format!(
        "Post-land checks passed on {} in {}",
        short_commit(commit),
        seconds(took)
    )
}

/// The feed line for a failed batch, while Rosen's bisection looks for the change.
pub fn verify_failed_line(commit: &str, command: &str) -> String {
    feed_line(&format!(
        "Post-land checks failed on {} · `{command}` · finding the change that broke it",
        short_commit(commit)
    ))
}

/// The feed line for one bisection step.
pub fn bisecting_line(commit: &str) -> String {
    format!("Post-land checks · testing {}", short_commit(commit))
}

/// The feed line for the change the bisection found, which Rosen has reverted.
pub fn reverted_line(change: &str, commit: &str) -> String {
    format!(
        "Change {} ({}) broke the post-land checks · reverted · reopened for its author",
        short_change(change),
        short_commit(commit)
    )
}

/// The feed line for a failing batch no one change explains: nothing is reverted.
pub fn unconfirmed_line(commit: &str) -> String {
    format!(
        "Post-land checks fail on {}, but no one change fails where the commit before it passes · nothing reverted · tried again after the next land",
        short_commit(commit)
    )
}

/// The feed line for post-land checks that could not run or report.
pub fn verify_stopped_line(commit: &str, reason: &str) -> String {
    feed_line(&format!(
        "Post-land checks on {} stopped · {reason}",
        short_commit(commit)
    ))
}

/// The last `OUTPUT_TAIL` characters of `output`, from the start of a line.
fn tail(output: &str) -> &str {
    let output = output.trim_end();
    if output.len() <= OUTPUT_TAIL {
        return output;
    }
    let mut start = output.len() - OUTPUT_TAIL;
    while !output.is_char_boundary(start) {
        start += 1;
    }
    match output[start..].find('\n') {
        Some(line_end) => &output[start + line_end + 1..],
        None => &output[start..],
    }
}

fn short_change(change: &str) -> &str {
    change.get(..8).unwrap_or(change)
}

fn short_commit(commit: &str) -> &str {
    commit.get(..12).unwrap_or(commit)
}
