//! Pure planning and report formatting for the `preflight` tool (#537).
//!
//! Given a change's diff and workspace metadata, [`plan`] selects only the checks that
//! apply to what changed: `rustfmt --check` on changed Rust files only, UI type-check and
//! lint when `ui/` changed, `cargo check -p` for touched crates, `cargo clippy -p` for
//! touched crates on sensitive paths, and generated-file freshness checks.

use std::collections::{BTreeMap, BTreeSet};

use crate::text_budget::{PROMPT_BYTES, prompt_excerpt};

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
    /// True when the path matches a sensitive pattern in the base's `rosen.toml`.
    pub sensitive: bool,
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

/// The checks a change needs before it is named.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PreflightPlan {
    /// Existing changed `.rs` files grouped by Rust edition (`"2024"`, `"2021"`, ...).
    pub rustfmt_by_edition: BTreeMap<String, Vec<String>>,
    pub ui_check: bool,
    pub ui_lint: bool,
    /// Touched workspace crates to run `cargo check -p <crate>` on.
    pub check_crates: Vec<String>,
    /// Touched workspace crates with a sensitive changed path (`cargo clippy -p <crate>`).
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
        !self.check_crates.is_empty() || !self.clippy_crates.is_empty() || self.check_bindings
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
    check_crates: BTreeSet<String>,
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
            self.check_crates.insert(crate_info.name.clone());
            if file.sensitive {
                self.clippy_crates.insert(crate_info.name.clone());
            }
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
            check_crates: self.check_crates.into_iter().collect(),
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
    let short = change.get(..8).unwrap_or(change);
    let noun = if changed_count == 1 { "file" } else { "files" };
    let mut report = format!(
        "[Velocity] {verdict} · change {short} ({changed_count} changed {noun})\nFull log: {log_path}\n"
    );
    for step in steps {
        let tag = step.status.tag();
        if step.summary.is_empty() {
            report.push_str(&format!("\n- [{tag}] {}", step.name));
        } else {
            report.push_str(&format!("\n- [{tag}] {} — {}", step.name, step.summary));
        }
    }
    for step in steps {
        if let Some(excerpt) = step
            .excerpt
            .as_deref()
            .map(str::trim)
            .filter(|text| !text.is_empty())
        {
            report.push_str(&format!("\n\n### {}\n{excerpt}", step.name));
        }
    }
    let bounded = prompt_excerpt(
        &report,
        PROMPT_BYTES,
        "\n[Preflight report excerpt. Read the full log at the path above.]",
    );
    (passed, bounded)
}
