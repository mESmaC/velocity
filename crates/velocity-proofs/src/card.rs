//! Building the proof card (SPEC.md F17 "Proof card") from what the checks found. Pure: the
//! git and Lake edge in `check.rs` gathers the inputs.

use std::collections::BTreeSet;

use velocity_core::proof_card::{
    ProofCard, ProofCheck, ProofCheckState, ProofLevel, ProofLevelKind, ProofTheorem,
    STATEMENT_LOCK_CHECK,
};

use crate::axioms::ALLOWED_AXIOMS;
use crate::lean_source::TheoremStatement;
use crate::levels::{self, FeatureLevels};
use crate::report::{CheckStep, ProofCheckReport};

/// The Lake project folder, as the levels name features inside it.
const PROJECT_FOLDER: &str = "proofs";

/// A fixture table the PR adds or changes (`proofs/fixtures/<feature>.tsv`): the reference
/// level's tie between the Lean model and the code (F17 Confidence proof).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FixtureTable {
    pub path: String,
    pub rows: usize,
}

impl FixtureTable {
    pub fn new(path: &str, rows: usize) -> Self {
        Self {
            path: path.to_owned(),
            rows,
        }
    }
}

/// Where the theorem statements stand against the lock taken when PR duty approved.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LockState {
    /// Not approved yet: the lock is taken at approval.
    NotApproved,
    /// Every statement locked at approval is unchanged at this head.
    Unchanged,
    /// These locked statements changed or went away since approval.
    Changed(Vec<String>),
}

/// Everything the card shows, gathered at one PR head.
#[derive(Clone, Debug)]
pub struct CardInput {
    pub head: String,
    pub report: ProofCheckReport,
    pub theorems: Vec<TheoremStatement>,
    pub fixtures: Vec<FixtureTable>,
    pub lock: LockState,
}

/// The card, or `None` when the PR touches no proof file, fixture table or proof-list
/// feature: a PR off the proof list carries no card.
pub fn build_card(input: &CardInput) -> Option<ProofCard> {
    let report = &input.report;
    if report.changed.is_empty() && input.fixtures.is_empty() && report.levels.is_empty() {
        return None;
    }
    let mut card = ProofCard::new(&input.head, proof_folders(input));
    card.checks = checks(&input.report, &input.lock);
    card.proved = input.report.passed();
    card.theorems = input.theorems.iter().map(theorem_row).collect();
    card.theorem_count = u32::try_from(card.theorems.len()).unwrap_or(u32::MAX);
    card.theorems.extend(input.fixtures.iter().map(fixture_row));
    card.levels = levels(&report.levels);
    Some(card)
}

/// `proofs/ClaimLock/` for `proofs/ClaimLock/Design.lean`; fixture tables name their own folder.
fn proof_folders(input: &CardInput) -> Vec<String> {
    let folders: BTreeSet<String> = input
        .report
        .changed
        .iter()
        .chain(input.fixtures.iter().map(|table| &table.path))
        // Root files (`Velocity.lean`, the lakefile) only register modules; skip them.
        .filter_map(|path| path.rsplit_once('/'))
        .map(|(folder, _)| format!("{folder}/"))
        .filter(|folder| *folder != format!("{PROJECT_FOLDER}/"))
        .chain(
            input
                .report
                .levels
                .iter()
                .map(|feature| format!("{}/{}/", PROJECT_FOLDER, feature.feature)),
        )
        .collect();
    folders.into_iter().collect()
}

fn checks(report: &ProofCheckReport, lock: &LockState) -> Vec<ProofCheck> {
    let no_lean = report.changed.is_empty();
    let mut checks = vec![
        build_check(report, no_lean),
        placeholder_check(report),
        axiom_check(report, no_lean),
        lock_check(lock),
    ];
    if !report.unnamed_sections.is_empty() {
        checks.push(ProofCheck::new(
            "design proof names its SPEC.md section",
            ProofCheckState::Failed,
            &report.unnamed_sections.join(", "),
        ));
    }
    checks
}

fn build_check(report: &ProofCheckReport, no_lean: bool) -> ProofCheck {
    let (state, detail) = match &report.build {
        CheckStep::Ran(()) => (ProofCheckState::Passed, String::new()),
        CheckStep::Skipped if no_lean => (ProofCheckState::Passed, "no Lean changed".into()),
        CheckStep::Skipped => (ProofCheckState::Failed, "not run".into()),
        CheckStep::Failed(output) => (ProofCheckState::Failed, output.clone()),
    };
    ProofCheck::new("lake build", state, &detail)
}

fn placeholder_check(report: &ProofCheckReport) -> ProofCheck {
    if report.placeholders.is_empty() {
        return ProofCheck::new("no sorry or admit", ProofCheckState::Passed, "");
    }
    let found: Vec<String> = report
        .placeholders
        .iter()
        .map(|found| format!("{} at {}:{}", found.word, found.path, found.line))
        .collect();
    ProofCheck::new(
        "no sorry or admit",
        ProofCheckState::Failed,
        &found.join(", "),
    )
}

fn axiom_check(report: &ProofCheckReport, no_lean: bool) -> ProofCheck {
    let label = format!("axioms: {}", ALLOWED_AXIOMS.join(", "));
    let (state, detail) = match &report.axioms {
        CheckStep::Ran(audit) if audit.disallowed.is_empty() => {
            (ProofCheckState::Passed, String::new())
        }
        CheckStep::Ran(audit) => {
            let found: Vec<String> = audit
                .disallowed
                .iter()
                .map(|found| format!("{} uses {}", found.declaration, found.axiom))
                .collect();
            (ProofCheckState::Failed, found.join(", "))
        }
        CheckStep::Skipped if no_lean => (ProofCheckState::Passed, "no Lean changed".into()),
        CheckStep::Skipped => (ProofCheckState::Failed, "not run: lake build failed".into()),
        CheckStep::Failed(reason) => (ProofCheckState::Failed, reason.clone()),
    };
    ProofCheck::new(&label, state, &detail)
}

fn lock_check(lock: &LockState) -> ProofCheck {
    let label = STATEMENT_LOCK_CHECK;
    match lock {
        LockState::NotApproved => ProofCheck::new(
            label,
            ProofCheckState::Pending,
            "locked when PR duty approves",
        ),
        LockState::Unchanged => ProofCheck::new(label, ProofCheckState::Passed, ""),
        LockState::Changed(keys) => ProofCheck::new(
            label,
            ProofCheckState::Failed,
            &format!(
                "changed since approval, so the review restarts: {}",
                keys.join(", ")
            ),
        ),
    }
}

fn theorem_row(theorem: &TheoremStatement) -> ProofTheorem {
    let (path, name) = theorem
        .key
        .split_once("::")
        .unwrap_or(("", theorem.key.as_str()));
    let file = path.rsplit('/').next().unwrap_or(path);
    let kind = file.strip_suffix(".lean").unwrap_or(file);
    let statement = theorem.statement.trim_start_matches(':').trim();
    ProofTheorem::new(kind, name, statement)
}

fn fixture_row(table: &FixtureTable) -> ProofTheorem {
    let rows = match table.rows {
        1 => "1 row".to_owned(),
        count => format!("{count} rows"),
    };
    ProofTheorem::new(
        "Fixture",
        &table.path,
        &format!("{rows} exported from the model"),
    )
}

/// One row per feature and language, from the levels the checks read (F17 levels).
fn levels(features: &[FeatureLevels]) -> Vec<ProofLevel> {
    features
        .iter()
        .flat_map(|feature| {
            feature.languages.iter().map(move |language| {
                ProofLevel::new(
                    language.language.unwrap_or("No code"),
                    level_kind(language.level),
                    &format!(
                        "{} · {}",
                        feature.feature,
                        language.evidence.replace('`', "")
                    ),
                    language.flagged(),
                )
            })
        })
        .collect()
}

fn level_kind(level: levels::ProofLevel) -> ProofLevelKind {
    match level {
        levels::ProofLevel::Model => ProofLevelKind::Model,
        levels::ProofLevel::Reference => ProofLevelKind::Reference,
        levels::ProofLevel::Code => ProofLevelKind::Code,
    }
}
