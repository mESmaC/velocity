//! Pure planning, gate evaluation and report formatting for the `preflight` tool (#537, #560).
//!
//! Given a change's diff and workspace metadata, [`plan`] selects the checks that apply to
//! what changed: `rustfmt --check` on changed Rust files only, UI type-check and lint when
//! `ui/` changed, `cargo clippy -p <crate> --all-targets -- -D warnings` for every touched
//! Rust crate, and generated-file freshness checks.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::text_budget::{PROMPT_BYTES, prompt_excerpt};

/// Maximum size of a persisted preflight failure summary delivered in name/land refusals.
const FAILURE_SUMMARY_BYTES: usize = 4096;

/// One Cargo package discovered in the workspace.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkspaceCrate {
    pub name: String,
    /// Repository-relative directory with forward slashes, or empty for the workspace root.
    pub directory: String,
    pub edition: String,
}

/// One path changed against the change's parent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChangedFile {
    pub path: String,
    /// False when the file was deleted in this change (so `rustfmt --check` skips it).
    pub exists: bool,
    /// True when the file can affect generated IPC bindings (`ui/src/lib/bindings.ts`).
    pub touches_bindings: bool,
}

/// Static workspace capabilities inspected before planning preflight checks.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PreflightContext {
    pub crates: Vec<WorkspaceCrate>,
    pub default_edition: String,
    pub has_cargo_lock: bool,
    pub has_ui_check: bool,
    pub has_ui_lint: bool,
    pub has_pnpm_lock: bool,
    pub has_bindings_exporter: bool,
}

/// The checks a change needs before it is named or landed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PreflightPlan {
    /// Existing changed `.rs` files grouped by Rust edition (`"2024"`, `"2021"`, ...).
    pub rustfmt_by_edition: BTreeMap<String, Vec<String>>,
    pub ui_check: bool,
    pub ui_lint: bool,
    /// Touched workspace crates to run `cargo clippy -p <crate> --all-targets -- -D warnings` on.
    pub clippy_crates: Vec<String>,
    /// Package name owning `src-tauri`, if present in the workspace.
    pub bindings_crate: Option<String>,
    pub check_cargo_lock: bool,
    pub check_pnpm_lock: bool,
    pub check_bindings: bool,
}

impl PreflightPlan {
    /// Whether any planned step needs a queued light Rust build turn.
    pub fn needs_light_rust_turn(&self) -> bool {
        !self.clippy_crates.is_empty() || self.check_bindings
    }
}

/// Computes the minimal preflight check plan from the change's diff and workspace layout.
pub fn plan(changed: &[ChangedFile], context: &PreflightContext) -> PreflightPlan {
    let mut state = PlanBuilder::default();
    let fallback_edition = if context.default_edition.is_empty() {
        "2024"
    } else {
        context.default_edition.as_str()
    };
    for file in changed {
        state.observe(file, context, fallback_edition);
    }
    state.finish(context)
}

#[derive(Default)]
struct PlanBuilder {
    rustfmt_sets: BTreeMap<String, BTreeSet<String>>,
    clippy_crates: BTreeSet<String>,
    ui_touched: bool,
    check_cargo_lock: bool,
    check_pnpm_lock: bool,
    check_bindings: bool,
}

impl PlanBuilder {
    fn observe(&mut self, file: &ChangedFile, context: &PreflightContext, fallback_edition: &str) {
        let path = normalize_path(&file.path);
        let owner = owning_crate(&path, &context.crates);
        if file.exists && path.ends_with(".rs") {
            let edition = owner
                .map(|item| item.edition.as_str())
                .filter(|edition| !edition.is_empty())
                .unwrap_or(fallback_edition);
            self.rustfmt_sets
                .entry(edition.to_owned())
                .or_default()
                .insert(path.clone());
        }
        if path == "ui" || path.starts_with("ui/") {
            self.ui_touched = true;
        }
        if let Some(crate_info) = owner {
            self.clippy_crates.insert(crate_info.name.clone());
        }
        if context.has_cargo_lock && is_cargo_manifest_or_lock(&path) {
            self.check_cargo_lock = true;
        }
        if context.has_pnpm_lock && (path == "ui/package.json" || path == "ui/pnpm-lock.yaml") {
            self.check_pnpm_lock = true;
        }
        if context.has_bindings_exporter && file.touches_bindings {
            self.check_bindings = true;
        }
    }

    fn finish(self, context: &PreflightContext) -> PreflightPlan {
        let rustfmt_by_edition = self
            .rustfmt_sets
            .into_iter()
            .map(|(edition, files)| (edition, files.into_iter().collect()))
            .collect();
        let bindings_crate = owning_crate("src-tauri/src/bin/export-bindings.rs", &context.crates)
            .map(|item| item.name.clone());
        PreflightPlan {
            rustfmt_by_edition,
            ui_check: self.ui_touched && context.has_ui_check,
            ui_lint: self.ui_touched && context.has_ui_lint,
            clippy_crates: self.clippy_crates.into_iter().collect(),
            bindings_crate,
            check_cargo_lock: self.check_cargo_lock,
            check_pnpm_lock: self.check_pnpm_lock,
            check_bindings: self.check_bindings,
        }
    }
}

/// Normalizes a repository-relative path to forward slashes without a leading `./`.
pub fn normalize_path(path: &str) -> String {
    let normalized = path.replace('\\', "/");
    normalized
        .strip_prefix("./")
        .unwrap_or(&normalized)
        .to_owned()
}

fn owning_crate<'a>(path: &str, crates: &'a [WorkspaceCrate]) -> Option<&'a WorkspaceCrate> {
    crates
        .iter()
        .filter(|item| path_in_directory(path, &item.directory))
        .max_by_key(|item| item.directory.len())
}

fn path_in_directory(path: &str, directory: &str) -> bool {
    if directory.is_empty() {
        return true;
    }
    path == directory
        || path
            .strip_prefix(directory)
            .is_some_and(|rest| rest.starts_with('/'))
}

fn is_cargo_manifest_or_lock(path: &str) -> bool {
    path == "Cargo.toml" || path.ends_with("/Cargo.toml") || path == "Cargo.lock"
}

/// Outcome of one step in the preflight report.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StepStatus {
    Passed,
    Warned,
    Failed,
    Skipped,
}

impl StepStatus {
    fn tag(self) -> &'static str {
        match self {
            Self::Passed => "PASS",
            Self::Warned => "WARN",
            Self::Failed => "FAIL",
            Self::Skipped => "SKIP",
        }
    }
}

/// One line item (plus optional failure or warning excerpt) in a preflight report.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StepOutcome {
    pub name: String,
    pub status: StepStatus,
    pub summary: String,
    pub excerpt: Option<String>,
}

impl StepOutcome {
    pub fn passed(name: impl Into<String>, summary: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            status: StepStatus::Passed,
            summary: summary.into(),
            excerpt: None,
        }
    }

    pub fn warned(
        name: impl Into<String>,
        summary: impl Into<String>,
        excerpt: Option<String>,
    ) -> Self {
        Self {
            name: name.into(),
            status: StepStatus::Warned,
            summary: summary.into(),
            excerpt,
        }
    }

    pub fn failed(
        name: impl Into<String>,
        summary: impl Into<String>,
        excerpt: Option<String>,
    ) -> Self {
        Self {
            name: name.into(),
            status: StepStatus::Failed,
            summary: summary.into(),
            excerpt,
        }
    }

    pub fn skipped(name: impl Into<String>, summary: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            status: StepStatus::Skipped,
            summary: summary.into(),
            excerpt: None,
        }
    }
}

/// Formats all preflight steps into one pass/fail report within the prompt byte budget.
pub fn format_report(
    change: &str,
    changed_count: usize,
    steps: &[StepOutcome],
    log_path: &str,
) -> (bool, String) {
    let passed = steps.iter().all(|step| step.status != StepStatus::Failed);
    let warned = steps.iter().any(|step| step.status == StepStatus::Warned);
    let verdict = match (passed, warned) {
        (true, false) => "Preflight passed",
        (true, true) => "Preflight passed with warnings",
        (false, _) => "Preflight failed",
    };
    let short = short_id(change);
    let noun = if changed_count == 1 { "file" } else { "files" };
    let mut report = format!(
        "[Velocity] {verdict} · change {short} ({changed_count} changed {noun})\nFull log: {log_path}\n"
    );
    for step in steps {
        append_step_line(&mut report, step);
    }
    for step in steps {
        append_step_excerpt(&mut report, step);
    }
    let bounded = prompt_excerpt(
        &report,
        PROMPT_BYTES,
        "\n[Preflight report excerpt. Read the full log at the path above.]",
    );
    (passed, bounded)
}

/// Formats only the failed steps and their excerpts for name/land refusal messages (#560).
pub fn format_failure_summary(steps: &[StepOutcome], log_path: &str) -> String {
    let failed: Vec<&StepOutcome> = steps
        .iter()
        .filter(|step| step.status == StepStatus::Failed)
        .collect();
    if failed.is_empty() {
        return String::new();
    }
    let mut summary = String::new();
    for step in &failed {
        append_step_line(&mut summary, step);
    }
    for step in &failed {
        append_step_excerpt(&mut summary, step);
    }
    if !log_path.is_empty() {
        summary.push_str(&format!("\n\nFull log: {log_path}"));
    }
    prompt_excerpt(
        summary.trim(),
        FAILURE_SUMMARY_BYTES,
        "\n[Failure summary truncated. Read the full preflight log.]",
    )
}

fn append_step_line(output: &mut String, step: &StepOutcome) {
    let tag = step.status.tag();
    if step.summary.is_empty() {
        output.push_str(&format!("\n- [{tag}] {}", step.name));
    } else {
        output.push_str(&format!("\n- [{tag}] {} — {}", step.name, step.summary));
    }
}

fn append_step_excerpt(output: &mut String, step: &StepOutcome) {
    if let Some(excerpt) = step
        .excerpt
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
    {
        output.push_str(&format!("\n\n### {}\n{excerpt}", step.name));
    }
}

/// Persisted result of the most recent `preflight` run for one Rosen change (#560).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreflightRecord {
    pub change: String,
    /// Tree ID (`change.head`) checked by this preflight run.
    pub head: String,
    /// Base commit ID observed when preflight ran.
    #[serde(default)]
    pub base: String,
    pub passed: bool,
    /// Bounded summary of failing steps when `passed` is false.
    #[serde(default)]
    pub failure_summary: String,
    #[serde(default)]
    pub log_path: String,
    #[serde(default)]
    pub recorded_at: u64,
}

/// Result of checking whether a change's current snapshot has a passing preflight (#560).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PreflightGateDecision {
    Passed,
    Missing,
    Failed {
        failure_summary: String,
        log_path: String,
    },
    StaleAfterPass {
        checked_head: String,
    },
    StaleAfterFailure {
        checked_head: String,
        failure_summary: String,
        log_path: String,
    },
}

/// Decides whether `current_head` satisfies the preflight precondition from Rosen's receipt
/// table and the persisted [`PreflightRecord`].
pub fn evaluate_gate(
    current_head: &str,
    has_receipt: bool,
    record: Option<&PreflightRecord>,
) -> PreflightGateDecision {
    if has_receipt {
        return PreflightGateDecision::Passed;
    }
    let Some(record) = record else {
        return PreflightGateDecision::Missing;
    };
    if record.head == current_head {
        if record.passed {
            PreflightGateDecision::Passed
        } else {
            PreflightGateDecision::Failed {
                failure_summary: record.failure_summary.clone(),
                log_path: record.log_path.clone(),
            }
        }
    } else if record.passed {
        PreflightGateDecision::StaleAfterPass {
            checked_head: record.head.clone(),
        }
    } else {
        PreflightGateDecision::StaleAfterFailure {
            checked_head: record.head.clone(),
            failure_summary: record.failure_summary.clone(),
            log_path: record.log_path.clone(),
        }
    }
}

/// Refusal returned when an agent attempts to name a change whose latest snapshot has no
/// passing preflight (#560). Returns `None` when the gate passed.
pub fn name_refusal_message(
    change: &str,
    current_head: &str,
    decision: &PreflightGateDecision,
) -> Option<String> {
    let short_change = short_id(change);
    let head = short_id(current_head);
    match decision {
        PreflightGateDecision::Passed => None,
        PreflightGateDecision::Missing => Some(format!(
            "Cannot name change {short_change}: no passing preflight for the current snapshot ({head}).\n\n\
             Run `preflight` from `velocity-run` and end your turn to wait for its pass/fail report, \
             then name the change with `rosen commit -m \"<summary>\"`."
        )),
        PreflightGateDecision::Failed {
            failure_summary,
            log_path,
        } => {
            let detail = format_failure_detail(failure_summary, log_path);
            Some(format!(
                "Cannot name change {short_change}: preflight failed on the current snapshot ({head}).\n\n\
                 {detail}\n\n\
                 Fix the failing checks, run `preflight` from `velocity-run` until it passes, \
                 and then name the change with `rosen commit -m \"<summary>\"`."
            ))
        }
        PreflightGateDecision::StaleAfterPass { checked_head } => Some(format!(
            "Cannot name change {short_change}: the workspace changed after the last passing preflight \
             (checked {}, current {head}).\n\n\
             Run `preflight` from `velocity-run` on the current workspace and wait for it to pass \
             before naming the change with `rosen commit -m \"<summary>\"`.",
            short_id(checked_head)
        )),
        PreflightGateDecision::StaleAfterFailure {
            checked_head,
            failure_summary,
            log_path,
        } => {
            let detail = format_failure_detail(failure_summary, log_path);
            Some(format!(
                "Cannot name change {short_change}: no passing preflight for the current snapshot ({head}); \
                 the last preflight on {} failed.\n\n\
                 {detail}\n\n\
                 Run `preflight` from `velocity-run` on the current workspace and wait for it to pass \
                 before naming the change with `rosen commit -m \"<summary>\"`.",
                short_id(checked_head)
            ))
        }
    }
}

/// Refusal `(reason, instructions)` returned by the landing runner when a named change's
/// latest snapshot has no passing preflight (#560). Returns `None` when the gate passed.
pub fn land_refusal_parts(
    change: &str,
    current_head: &str,
    decision: &PreflightGateDecision,
) -> Option<(String, String)> {
    let short_change = short_id(change);
    let head = short_id(current_head);
    match decision {
        PreflightGateDecision::Passed => None,
        PreflightGateDecision::Missing => Some((
            "no passing preflight for current snapshot".to_owned(),
            format!(
                "Change {short_change} (snapshot {head}) has no passing preflight.\n\n\
                 Run `preflight` from `velocity-run` and wait for it to pass, then name the change \
                 again with `rosen commit -m \"<what changed>\"` so Velocity can land it."
            ),
        )),
        PreflightGateDecision::Failed {
            failure_summary,
            log_path,
        } => {
            let detail = format_failure_detail(failure_summary, log_path);
            Some((
                "preflight failed on current snapshot".to_owned(),
                format!(
                    "Change {short_change} (snapshot {head}) failed preflight.\n\n\
                     {detail}\n\n\
                     Fix the failing checks, run `preflight` from `velocity-run` until it passes, \
                     then name the change again with `rosen commit -m \"<what changed>\"` so Velocity can land it."
                ),
            ))
        }
        PreflightGateDecision::StaleAfterPass { checked_head } => Some((
            "change moved since last passing preflight".to_owned(),
            format!(
                "Change {short_change} moved from snapshot {} to {head} after the last passing preflight \
                 (from new edits or a rebase).\n\n\
                 Run `rosen rebase` if a rebase is waiting, run `preflight` from `velocity-run` on the \
                 current workspace, and name the change again with `rosen commit -m \"<what changed>\"`.",
                short_id(checked_head)
            ),
        )),
        PreflightGateDecision::StaleAfterFailure {
            checked_head,
            failure_summary,
            log_path,
        } => {
            let detail = format_failure_detail(failure_summary, log_path);
            Some((
                "no passing preflight for current snapshot (last preflight failed)".to_owned(),
                format!(
                    "Change {short_change} (snapshot {head}) has no passing preflight; the last preflight \
                     on {} failed.\n\n\
                     {detail}\n\n\
                     Run `preflight` from `velocity-run` until it passes, then name the change again \
                     with `rosen commit -m \"<what changed>\"` so Velocity can land it.",
                    short_id(checked_head)
                ),
            ))
        }
    }
}

fn format_failure_detail(failure_summary: &str, log_path: &str) -> String {
    let trimmed = failure_summary.trim();
    if !trimmed.is_empty() {
        format!("What failed:\n{trimmed}")
    } else if !log_path.is_empty() {
        format!("Full preflight log: {log_path}")
    } else {
        "No failure excerpt was captured.".to_owned()
    }
}

fn short_id(value: &str) -> &str {
    value.get(..8).unwrap_or(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_context() -> PreflightContext {
        PreflightContext {
            crates: vec![
                WorkspaceCrate {
                    name: "velocity-core".into(),
                    directory: "crates/velocity-core".into(),
                    edition: "2024".into(),
                },
                WorkspaceCrate {
                    name: "velocity-graph".into(),
                    directory: "crates/velocity-graph".into(),
                    edition: "2024".into(),
                },
                WorkspaceCrate {
                    name: "velocity-app".into(),
                    directory: "src-tauri".into(),
                    edition: "2024".into(),
                },
            ],
            default_edition: "2024".into(),
            has_cargo_lock: true,
            has_ui_check: true,
            has_ui_lint: true,
            has_pnpm_lock: true,
            has_bindings_exporter: true,
        }
    }

    // Risk (#560): non-sensitive crates such as velocity-graph and velocity-core previously
    // skipped clippy in preflight, letting lint regressions break master repeatedly.
    #[test]
    fn plan_runs_clippy_on_every_touched_rust_crate() {
        let changed = vec![
            ChangedFile {
                path: "crates/velocity-graph/src/markdown.rs".into(),
                exists: true,
                touches_bindings: false,
            },
            ChangedFile {
                path: "crates/velocity-core/src/issue_holds.rs".into(),
                exists: true,
                touches_bindings: false,
            },
        ];
        let result = plan(&changed, &sample_context());
        assert_eq!(
            result.clippy_crates,
            vec!["velocity-core".to_owned(), "velocity-graph".to_owned()]
        );
        assert!(result.needs_light_rust_turn());
    }

    // Risk (#560): a change whose current snapshot failed preflight or moved after preflight
    // must be refused before naming or landing, naming what failed when available.
    #[test]
    fn gate_enforces_matching_head_and_surfaces_failures() {
        let head_a = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let head_b = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
        assert_eq!(
            evaluate_gate(head_a, false, None),
            PreflightGateDecision::Missing
        );
        assert_eq!(
            evaluate_gate(head_a, true, None),
            PreflightGateDecision::Passed
        );

        let failed = PreflightRecord {
            change: "qlxovkwwwxkppttrzxzyxmkulowyknot".into(),
            head: head_a.into(),
            base: "1111111111111111111111111111111111111111".into(),
            passed: false,
            failure_summary: "- [FAIL] cargo clippy -p velocity-graph --all-targets — failed"
                .into(),
            log_path: "C:/tmp/preflight.log".into(),
            recorded_at: 100,
        };
        let decision = evaluate_gate(head_a, false, Some(&failed));
        assert!(matches!(decision, PreflightGateDecision::Failed { .. }));
        let refusal = name_refusal_message(&failed.change, head_a, &decision);
        assert!(
            refusal
                .as_deref()
                .is_some_and(|text| text.contains("velocity-graph"))
        );

        let passed = PreflightRecord {
            passed: true,
            failure_summary: String::new(),
            ..failed
        };
        assert_eq!(
            evaluate_gate(head_a, false, Some(&passed)),
            PreflightGateDecision::Passed
        );
        let stale = evaluate_gate(head_b, false, Some(&passed));
        assert_eq!(
            stale,
            PreflightGateDecision::StaleAfterPass {
                checked_head: head_a.into(),
            }
        );
        let land_refusal = land_refusal_parts(&passed.change, head_b, &stale);
        assert!(land_refusal.is_some());
    }
}
