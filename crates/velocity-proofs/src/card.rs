//! Building the proof card (SPEC.md F17 "Proof card") from what the checks found. Pure: the
//! git and Lake edge in `check.rs` gathers the inputs.

use std::collections::BTreeSet;

use velocity_core::proof_card::{
    ProofCard, ProofCheck, ProofCheckState, ProofLevel, ProofLevelKind, ProofTheorem,
    STATEMENT_LOCK_CHECK,
};

use crate::axioms::ALLOWED_AXIOMS;
use crate::lean_source::TheoremStatement;
use crate::report::{CheckStep, ProofCheckReport};

/// A fixture table the PR adds or changes (`proofs/fixtures/<feature>.tsv`): the reference
/// level's tie between the Lean model and the code (F17 Confidence proof).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FixtureTable {
    pub path: String,
    pub rows: usize,
    /// Code files at the PR head that name the table: the tests that check the code against it.
    pub checked_by: Vec<String>,
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
    /// The PR's changed files outside `proofs/`, for the per-language levels.
    pub code_files: Vec<String>,
    /// The PR changes an Aeneas translation of its Rust (the code level).
    pub translated: bool,
    pub lock: LockState,
}

/// The card, or `None` when the PR changes no proof file and no fixture table: a PR off the
/// proof list carries no card.
pub fn build_card(input: &CardInput) -> Option<ProofCard> {
    if input.report.changed.is_empty() && input.fixtures.is_empty() {
        return None;
    }
    let mut card = ProofCard::new(&input.head, proof_folders(input));
    card.checks = checks(&input.report, &input.lock);
    card.proved = input.report.passed();
    card.theorems = input.theorems.iter().map(theorem_row).collect();
    card.theorem_count = u32::try_from(card.theorems.len()).unwrap_or(u32::MAX);
    card.theorems.extend(input.fixtures.iter().map(fixture_row));
    card.levels = levels(input);
    Some(card)
}

/// `proofs/ClaimLock/` for `proofs/ClaimLock/Design.lean`; fixture tables name their own folder.
fn proof_folders(input: &CardInput) -> Vec<String> {
    let folders: BTreeSet<String> = input
        .report
        .changed
        .iter()
        .chain(input.fixtures.iter().map(|table| &table.path))
        .map(|path| match path.rsplit_once('/') {
            Some((folder, _)) => format!("{folder}/"),
            None => path.clone(),
        })
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
    let checked = if table.checked_by.is_empty() {
        "no test names it yet".to_owned()
    } else {
        format!("checked by {}", table.checked_by.join(", "))
    };
    let rows = match table.rows {
        1 => "1 row".to_owned(),
        count => format!("{count} rows"),
    };
    ProofTheorem::new("Fixture", &table.path, &format!("{rows} · {checked}"))
}

/// The level each changed language reaches: code with an Aeneas translation (Rust only),
/// reference when one of its files checks a fixture table, otherwise model, flagged, since
/// the reference level is available to every language (F17 levels).
fn levels(input: &CardInput) -> Vec<ProofLevel> {
    let languages: BTreeSet<&str> = input
        .code_files
        .iter()
        .filter_map(|path| language_of(path))
        .collect();
    languages
        .into_iter()
        .map(|language| level_for(input, language))
        .collect()
}

fn level_for(input: &CardInput, language: &str) -> ProofLevel {
    if language == "Rust" && input.translated {
        return ProofLevel::new(
            language,
            ProofLevelKind::Code,
            "Aeneas translation refines the design model",
            false,
        );
    }
    let checking = input.fixtures.iter().find_map(|table| {
        table
            .checked_by
            .iter()
            .find(|path| language_of(path) == Some(language))
            .map(|test| (table, test))
    });
    match checking {
        Some((table, test)) => ProofLevel::new(
            language,
            ProofLevelKind::Reference,
            &format!("fixture table · {} · checked by {test}", table.path),
            false,
        ),
        None => ProofLevel::new(
            language,
            ProofLevelKind::Model,
            "only the design is proven · reference is available: export a fixture table and check the code against it",
            true,
        ),
    }
}

/// The language of a source file, or `None` for files that carry no logic (docs, config,
/// lockfiles, styles) and for Lean itself.
pub fn language_of(path: &str) -> Option<&'static str> {
    let extension = path.rsplit_once('.')?.1.to_ascii_lowercase();
    Some(match extension.as_str() {
        "rs" => "Rust",
        "ts" | "tsx" | "svelte" => "TypeScript",
        "js" | "jsx" | "mjs" | "cjs" => "JavaScript",
        "py" => "Python",
        "c" => "C",
        "cc" | "cpp" | "cxx" | "h" | "hh" | "hpp" => "C++",
        "go" => "Go",
        "ps1" => "PowerShell",
        _ => return None,
    })
}
