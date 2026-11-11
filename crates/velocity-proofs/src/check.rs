//! Running the proof checks in a worktree (SPEC.md F17 "What Velocity checks"): git finds the
//! PR's proof files, Lake builds them, and Lean audits their axioms. The decisions live in the
//! pure modules; this is the process edge.

use std::path::Path;
use std::process::{Command, Output};

use velocity_core::proof_card::ProofCard;

use crate::ProofCheckError;
use crate::axioms::{audit_script, module_name, parse_audit};
use crate::card::{CardInput, FixtureTable, LockState, build_card, language_of};
use crate::lean_source::{ProofFile, names_spec_section, placeholders, theorem_statements};
use crate::report::{CheckStep, ProofCheckReport};
use crate::statement_lock::StatementLock;

/// The folder that holds the Lake project, relative to the repository root.
const PROJECT_FOLDER: &str = "proofs";
/// Where the audit file is written: inside Lake's own ignored build folder.
const AUDIT_FILE: &str = ".lake/velocity-axiom-audit.lean";
/// How many lines of a failed build PR duty sees.
const EXCERPT_LINES: usize = 40;

/// Runs every check on the proof files the PR changed between `base` and `head`, in the
/// worktree as it is checked out (for PR duty, the PR merged onto the base branch).
pub fn check_worktree(
    worktree: &Path,
    base: &str,
    head: &str,
) -> Result<ProofCheckReport, ProofCheckError> {
    let Some(folder) = project_folder(worktree) else {
        return Ok(ProofCheckReport::nothing_changed(None));
    };
    let changed = changed_proof_files(worktree, &folder, base, head)?;
    if changed.is_empty() {
        return Ok(ProofCheckReport::nothing_changed(Some(folder)));
    }
    let files: Vec<ProofFile> = changed
        .iter()
        .map(|path| {
            let text = std::fs::read_to_string(worktree.join(path))?;
            Ok(ProofFile::new(path, &text))
        })
        .collect::<Result<_, std::io::Error>>()?;
    let lake_folder = worktree.join(&folder);
    let build = lake_build(&lake_folder);
    let axioms = match build {
        CheckStep::Ran(()) => audit_axioms(&lake_folder, &folder, &changed),
        _ => CheckStep::Skipped,
    };
    Ok(ProofCheckReport {
        project_folder: Some(folder),
        build,
        placeholders: files.iter().flat_map(placeholders).collect(),
        axioms,
        unnamed_sections: files
            .iter()
            .filter(|file| file.path.ends_with("/Design.lean") && !names_spec_section(&file.text))
            .map(|file| file.path.clone())
            .collect(),
        changed,
    })
}

/// Hashes every theorem statement under the Lake project at `commit`, read from git rather
/// than the checkout, so edits in the worktree never reach the lock.
pub fn statements_at(repository: &Path, commit: &str) -> Result<StatementLock, ProofCheckError> {
    let listing = git(
        repository,
        &[
            "ls-tree",
            "-r",
            "-z",
            "--name-only",
            commit,
            "--",
            PROJECT_FOLDER,
        ],
    )?;
    let mut files = Vec::new();
    for path in listing.split('\0').filter(|path| is_proof_source(path)) {
        let text = git(repository, &["show", &format!("{commit}:{path}")])?;
        files.push(ProofFile::new(path, &text));
    }
    Ok(StatementLock::of_files(commit, &files))
}

fn project_folder(worktree: &Path) -> Option<String> {
    let folder = worktree.join(PROJECT_FOLDER);
    ["lakefile.lean", "lakefile.toml"]
        .iter()
        .any(|name| folder.join(name).is_file())
        .then(|| PROJECT_FOLDER.to_owned())
}

/// The PR's added, copied, modified or renamed `.lean` files under `folder`.
fn changed_proof_files(
    worktree: &Path,
    folder: &str,
    base: &str,
    head: &str,
) -> Result<Vec<String>, ProofCheckError> {
    let range = format!("{base}...{head}");
    let listing = git(
        worktree,
        &[
            "diff",
            "--name-only",
            "-z",
            "--diff-filter=ACMR",
            &range,
            "--",
            folder,
        ],
    )?;
    Ok(listing
        .split('\0')
        .filter(|path| is_proof_source(path))
        .map(str::to_owned)
        .collect())
}

fn is_proof_source(path: &str) -> bool {
    path.ends_with(".lean") && !path.contains("/.lake/")
}

/// `lake build`. A project on Mathlib fetches Mathlib's prebuilt cache first, so a fresh
/// worktree never compiles Mathlib from source.
fn lake_build(folder: &Path) -> CheckStep<()> {
    let manifest = std::fs::read_to_string(folder.join("lake-manifest.json")).unwrap_or_default();
    if manifest.contains("\"mathlib\"") {
        match lake(folder, &["exe", "cache", "get"]) {
            Ok(output) if output.status.success() => {}
            Ok(output) => return CheckStep::Failed(excerpt("lake exe cache get", &output)),
            Err(error) => return CheckStep::Failed(error),
        }
    }
    match lake(folder, &["build"]) {
        Ok(output) if output.status.success() => CheckStep::Ran(()),
        Ok(output) => CheckStep::Failed(excerpt("lake build", &output)),
        Err(error) => CheckStep::Failed(error),
    }
}

/// Elaborates the audit file with `lake lean`, which builds its imports first.
fn audit_axioms(
    lake_folder: &Path,
    folder: &str,
    changed: &[String],
) -> CheckStep<crate::axioms::AxiomAudit> {
    let (modules, unbuilt): (Vec<String>, Vec<String>) = changed
        .iter()
        .filter_map(|path| module_name(folder, path))
        .partition(|module| built(lake_folder, module));
    if modules.is_empty() {
        return CheckStep::Ran(crate::axioms::AxiomAudit {
            unbuilt,
            ..Default::default()
        });
    }
    let script = lake_folder.join(AUDIT_FILE);
    let written = script
        .parent()
        .map_or(Ok(()), std::fs::create_dir_all)
        .and_then(|()| std::fs::write(&script, audit_script(&modules)));
    if let Err(error) = written {
        return CheckStep::Failed(format!("Could not write the audit file: {error}"));
    }
    let output = lake(lake_folder, &["lean", AUDIT_FILE]);
    let _ = std::fs::remove_file(&script);
    match output {
        Ok(output) => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            match parse_audit(&stdout) {
                Some(audit) if output.status.success() => {
                    CheckStep::Ran(crate::axioms::AxiomAudit { unbuilt, ..audit })
                }
                _ => CheckStep::Failed(excerpt("lake lean (axiom audit)", &output)),
            }
        }
        Err(error) => CheckStep::Failed(error),
    }
}

/// Whether `lake build` left an `.olean` for `module`.
fn built(lake_folder: &Path, module: &str) -> bool {
    let mut olean = lake_folder.join(".lake/build/lib/lean");
    olean.extend(module.split('.'));
    olean.set_extension("olean");
    olean.is_file()
}

fn lake(folder: &Path, arguments: &[&str]) -> Result<Output, String> {
    let mut command = Command::new("lake");
    command.args(arguments).current_dir(folder);
    hide_console_window(&mut command);
    command.output().map_err(|error| {
        format!("Could not run lake ({error}). Install the Lean toolchain with elan.")
    })
}

/// The command's last lines, errors first: what PR duty needs to name the failure.
fn excerpt(name: &str, output: &Output) -> String {
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let lines: Vec<&str> = text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect();
    let errors: Vec<&str> = lines
        .iter()
        .copied()
        .filter(|line| line.contains("error"))
        .take(EXCERPT_LINES)
        .collect();
    let shown = if errors.is_empty() {
        lines[lines.len().saturating_sub(EXCERPT_LINES)..].to_vec()
    } else {
        errors
    };
    let status = output
        .status
        .code()
        .map_or("was stopped".to_owned(), |code| {
            format!("exited with {code}")
        });
    format!("{name} {status}:\n{}", shown.join("\n"))
}

fn git(repository: &Path, arguments: &[&str]) -> Result<String, ProofCheckError> {
    let mut command = Command::new("git");
    // Git for Windows refuses paths over 260 characters without core.longpaths.
    command
        .args(["-c", "core.longpaths=true"])
        .args(arguments)
        .current_dir(repository);
    hide_console_window(&mut command);
    let output = command.output()?;
    if !output.status.success() {
        return Err(ProofCheckError::Git(format!(
            "git {} failed: {}",
            arguments.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Velocity is a GUI app; without this flag every child process flashes a console window.
#[cfg(windows)]
fn hide_console_window(command: &mut Command) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    command.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(windows))]
fn hide_console_window(_command: &mut Command) {}

/// Whether the worktree is checked out at exactly `head` with no edits under the proofs
/// folder, so what Lake builds there is what the PR holds.
pub fn checkout_at(worktree: &Path, head: &str) -> Result<bool, ProofCheckError> {
    let current = git(worktree, &["rev-parse", "HEAD"])?;
    if current.trim() != head {
        return Ok(false);
    }
    let edits = git(
        worktree,
        &["status", "--porcelain", "-z", "--", PROJECT_FOLDER],
    )?;
    Ok(edits.is_empty())
}

/// Runs the proof checks on the PR's changes between `base` and `head` and builds its proof
/// card (F17), or `None` when the PR changes no proof file or fixture table. `lock` is the
/// statement lock taken at the PR's last approval, if any.
pub fn proof_card(
    worktree: &Path,
    base: &str,
    head: &str,
    lock: Option<&StatementLock>,
) -> Result<Option<ProofCard>, ProofCheckError> {
    let changed = changed_files(worktree, base, head)?;
    let fixtures = fixture_tables(worktree, head, &changed)?;
    let report = check_worktree(worktree, base, head)?;
    if report.changed.is_empty() && fixtures.is_empty() {
        return Ok(None);
    }
    let mut theorems = Vec::new();
    let mut translated = false;
    for path in &report.changed {
        let text = std::fs::read_to_string(worktree.join(path))?;
        translated |= path.contains("/Generated/")
            || (path.ends_with("/Confidence.lean") && text.contains("Aeneas"));
        theorems.extend(theorem_statements(&ProofFile::new(path, &text)));
    }
    let lock = lock_state(worktree, head, lock)?;
    let prefix = format!("{PROJECT_FOLDER}/");
    let code_files = changed
        .into_iter()
        .filter(|path| !path.starts_with(&prefix) && !path.starts_with("stress/"))
        .collect();
    Ok(build_card(&CardInput {
        head: head.to_owned(),
        report,
        theorems,
        fixtures,
        code_files,
        translated,
        lock,
    }))
}

/// Every file the PR adds, copies, modifies or renames.
fn changed_files(worktree: &Path, base: &str, head: &str) -> Result<Vec<String>, ProofCheckError> {
    let range = format!("{base}...{head}");
    let listing = git(
        worktree,
        &["diff", "--name-only", "-z", "--diff-filter=ACMR", &range],
    )?;
    Ok(listing
        .split('\0')
        .filter(|path| !path.is_empty())
        .map(str::to_owned)
        .collect())
}

/// The fixture tables among `changed`, with their row counts and the code files that name
/// them at `head`.
fn fixture_tables(
    worktree: &Path,
    head: &str,
    changed: &[String],
) -> Result<Vec<FixtureTable>, ProofCheckError> {
    let folder = format!("{PROJECT_FOLDER}/fixtures/");
    let mut tables = Vec::new();
    for path in changed
        .iter()
        .filter(|path| path.starts_with(&folder) && path.ends_with(".tsv"))
    {
        let text = git(worktree, &["show", &format!("{head}:{path}")])?;
        let lines = text.lines().filter(|line| !line.trim().is_empty()).count();
        let name = path.rsplit('/').next().unwrap_or(path);
        tables.push(FixtureTable {
            path: path.clone(),
            // The first line is the column header.
            rows: lines.saturating_sub(1),
            checked_by: files_naming(worktree, head, name)?,
        });
    }
    Ok(tables)
}

/// Code files outside the proofs folder that mention `name` at `head`.
fn files_naming(worktree: &Path, head: &str, name: &str) -> Result<Vec<String>, ProofCheckError> {
    let mut command = Command::new("git");
    command
        .args([
            "-c",
            "core.longpaths=true",
            "grep",
            "-l",
            "-z",
            "-F",
            name,
            head,
        ])
        .current_dir(worktree);
    hide_console_window(&mut command);
    let output = command.output()?;
    // git grep exits with 1 when nothing matches.
    if !output.status.success() && output.status.code() != Some(1) {
        return Err(ProofCheckError::Git(format!(
            "git grep failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    let prefix = format!("{head}:");
    let proofs = format!("{PROJECT_FOLDER}/");
    Ok(String::from_utf8_lossy(&output.stdout)
        .split('\0')
        .filter_map(|path| path.strip_prefix(&prefix))
        .filter(|path| !path.starts_with(&proofs) && language_of(path).is_some())
        .map(str::to_owned)
        .collect())
}

fn lock_state(
    worktree: &Path,
    head: &str,
    lock: Option<&StatementLock>,
) -> Result<LockState, ProofCheckError> {
    let Some(lock) = lock else {
        return Ok(LockState::NotApproved);
    };
    if lock.head == head {
        return Ok(LockState::Unchanged);
    }
    let changed = lock.changed_in(&statements_at(worktree, head)?);
    Ok(if changed.is_empty() {
        LockState::Unchanged
    } else {
        LockState::Changed(changed)
    })
}

/// The ref the PR's changes are measured from: the remote-tracking base branch when the
/// worktree has one (what the PR merges into), otherwise the local branch of that name.
pub fn base_reference(worktree: &Path, base_branch: &str) -> String {
    let remote = format!("origin/{base_branch}");
    match git(worktree, &["rev-parse", "--verify", "--quiet", &remote]) {
        Ok(_) => remote,
        Err(_) => base_branch.to_owned(),
    }
}
