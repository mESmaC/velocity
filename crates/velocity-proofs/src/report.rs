//! What one run of the proof checks found, and how PR duty reads it (SPEC.md F17 "What
//! Velocity checks").

use crate::axioms::{ALLOWED_AXIOMS, AxiomAudit};
use crate::lean_source::Placeholder;
use crate::levels::FeatureLevels;

/// How one check went.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CheckStep<T> {
    /// Not run: nothing it checks changed, or an earlier check failed first.
    Skipped,
    Ran(T),
    /// It could not run, or failed, with this explanation.
    Failed(String),
}

/// The proof checks on one worktree: which proof files the PR changed and what each check
/// found in them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProofCheckReport {
    /// The Lake project folder (`proofs`), when the worktree has one.
    pub project_folder: Option<String>,
    /// The PR's added or modified `.lean` files under the project folder.
    pub changed: Vec<String>,
    pub build: CheckStep<()>,
    pub placeholders: Vec<Placeholder>,
    pub axioms: CheckStep<AxiomAudit>,
    /// Changed `Design.lean` files that never name the SPEC.md section they cover.
    pub unnamed_sections: Vec<String>,
    /// The level each language reaches for every proof-list feature the PR touches (F17
    /// Confidence proof). A model-only level is flagged for the reviewer, never failed.
    pub levels: Vec<FeatureLevels>,
}

impl ProofCheckReport {
    /// A report for a worktree whose PR changes no proof files.
    pub fn nothing_changed(project_folder: Option<String>) -> Self {
        Self {
            project_folder,
            changed: Vec::new(),
            build: CheckStep::Skipped,
            placeholders: Vec::new(),
            axioms: CheckStep::Skipped,
            unnamed_sections: Vec::new(),
            levels: Vec::new(),
        }
    }

    /// Whether every check passed (trivially, when no proof file changed).
    pub fn passed(&self) -> bool {
        if self.changed.is_empty() {
            return true;
        }
        let audit_clean =
            matches!(&self.axioms, CheckStep::Ran(audit) if audit.disallowed.is_empty());
        matches!(self.build, CheckStep::Ran(()))
            && self.placeholders.is_empty()
            && audit_clean
            && self.unnamed_sections.is_empty()
    }

    /// The four checks and the proof levels as lines for PR duty's prompt, or `None` when the
    /// PR touches no proof file and no proof-list feature.
    pub fn summary(&self) -> Option<String> {
        if self.changed.is_empty() {
            return self.levels_summary();
        }
        let verdict = if self.passed() {
            "every check passed"
        } else {
            "FAILED; request changes naming each failure below"
        };
        let lines = [
            format!(
                "Proof checks ({verdict}). Velocity ran them on this worktree, over {} changed proof file(s): {}.",
                self.changed.len(),
                self.changed.join(", ")
            ),
            format!("- lake build: {}", self.build_line()),
            format!("- no sorry or admit: {}", self.placeholder_line()),
            format!(
                "- standard axioms ({}): {}",
                ALLOWED_AXIOMS.join(", "),
                self.axiom_line()
            ),
            format!(
                "- design proofs name their SPEC.md section: {}",
                self.section_line()
            ),
        ];
        let mut summary = lines.join("\n");
        if let Some(levels) = self.levels_summary() {
            summary.push('\n');
            summary.push_str(&levels);
        }
        Some(summary)
    }

    /// One line per feature and language: the level reached, the strongest available, and
    /// the evidence. Model-only levels are flagged; they never fail the checks.
    fn levels_summary(&self) -> Option<String> {
        if self.levels.is_empty() {
            return None;
        }
        let mut lines = Vec::from([
            "Proof levels (a model-only level where a stronger one is available is FLAGGED: point it out and decide, it does not fail the checks; a PR touching a Sensitive path needs the strongest level):".to_owned(),
        ]);
        for feature in &self.levels {
            for entry in &feature.languages {
                let language = entry.language.unwrap_or("any language");
                let mut line = format!(
                    "- {} · {language} · {}: {}",
                    feature.feature,
                    entry.level.name(),
                    entry.evidence
                );
                if entry.flagged() {
                    line.push_str(&format!(
                        ". FLAGGED: {} is available",
                        entry.strongest.name()
                    ));
                } else if entry.below_strongest() {
                    line.push_str(&format!(
                        ". Strongest available: {}, on request",
                        entry.strongest.name()
                    ));
                }
                lines.push(line);
            }
        }
        Some(lines.join("\n"))
    }

    fn build_line(&self) -> String {
        match &self.build {
            CheckStep::Ran(()) => "passed".into(),
            CheckStep::Skipped => "not run".into(),
            CheckStep::Failed(output) => format!("FAILED\n{}", indent(output)),
        }
    }

    fn placeholder_line(&self) -> String {
        if self.placeholders.is_empty() {
            return "passed".into();
        }
        let found: Vec<String> = self
            .placeholders
            .iter()
            .map(|found| format!("`{}` at {}:{}", found.word, found.path, found.line))
            .collect();
        format!("FAILED: {}", found.join(", "))
    }

    fn axiom_line(&self) -> String {
        match &self.axioms {
            CheckStep::Ran(audit) if audit.disallowed.is_empty() => {
                format!(
                    "passed ({} declarations audited){}",
                    audit.audited,
                    unbuilt_note(audit)
                )
            }
            CheckStep::Ran(audit) => {
                let found: Vec<String> = audit
                    .disallowed
                    .iter()
                    .map(|found| format!("`{}` uses `{}`", found.declaration, found.axiom))
                    .collect();
                format!("FAILED: {}", found.join(", "))
            }
            CheckStep::Skipped => "not run (lake build failed)".into(),
            CheckStep::Failed(reason) => {
                format!("FAILED: the audit did not finish\n{}", indent(reason))
            }
        }
    }

    fn section_line(&self) -> String {
        if self.unnamed_sections.is_empty() {
            "passed".into()
        } else {
            format!(
                "FAILED: {} never name a SPEC.md section; use the project's own section label",
                self.unnamed_sections.join(", ")
            )
        }
    }
}

/// Modules no library target builds are not proofs Lean checked; PR duty decides whether
/// that matters (a re-export stub doesn't, a design proof does).
fn unbuilt_note(audit: &AxiomAudit) -> String {
    if audit.unbuilt.is_empty() {
        return String::new();
    }
    format!(
        ". Not built by `lake build`, so Lean never checked them: {}",
        audit.unbuilt.join(", ")
    )
}

fn indent(text: &str) -> String {
    text.lines()
        .map(|line| format!("    {line}"))
        .collect::<Vec<_>>()
        .join("\n")
}
