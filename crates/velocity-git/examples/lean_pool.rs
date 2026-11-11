//! Headless check of the warm pool's Lean setup (SPEC.md F5 Warm pool → Lean) on a real
//! repository with a Mathlib project: makes one ready slot at the given commit, prebuilds it
//! (toolchain through elan, Lake packages seeded from the repository's other checkouts,
//! `lake exe cache get`), then asks Lake whether Mathlib is up to date without building, and
//! removes the slot.
//!
//! `cargo run -p velocity-git --example lean_pool -- <repository> [commit]`

// A console example: its children share its console, so no hidden_command (#232).
#![allow(clippy::disallowed_methods)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use velocity_git::{
    WorktreeSetupRequest, create_ready_worktree, default_settings, parse_manifest,
    prebuild_ready_worktree, remove_ready_worktree,
};

type Outcome<Value = ()> = Result<Value, Box<dyn std::error::Error>>;

fn main() -> Outcome {
    let mut arguments = std::env::args().skip(1);
    let repository = PathBuf::from(
        arguments
            .next()
            .ok_or("usage: lean_pool <repository> [commit]")?,
    );
    let commit = match arguments.next() {
        Some(commit) => commit,
        None => output(&repository, "git", &["rev-parse", "origin/master"])?,
    };
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_nanos();
    let project_root = std::env::temp_dir().join(format!("velocity-lean-pool-{stamp}"));
    let slot = project_root.join(".ready-0");
    let result = run(&repository, &commit, &slot);
    if let Err(error) = remove_ready_worktree(&repository, &slot) {
        eprintln!("cleanup: {error}");
    }
    let _ = std::fs::remove_dir_all(&project_root);
    result
}

fn run(repository: &Path, commit: &str, slot: &Path) -> Outcome {
    let request = WorktreeSetupRequest::detached(
        repository.to_path_buf(),
        slot.to_path_buf(),
        commit.to_owned(),
        default_settings(),
        String::new(),
        String::new(),
        slot.join("target"),
    );
    create_ready_worktree(&request)?;
    println!("slot checked out at {}", &commit[..commit.len().min(7)]);

    let started = Instant::now();
    prebuild_ready_worktree(repository, slot, false)?;
    println!("Lean prebuild finished in {:?}", started.elapsed());

    let proofs = slot.join("proofs");
    let manifest = parse_manifest(&std::fs::read_to_string(proofs.join("lake-manifest.json"))?)
        .ok_or("unreadable lake-manifest.json")?;
    for package in &manifest.packages {
        let folder = proofs.join(&manifest.packages_dir).join(&package.name);
        let head = output(&folder, "git", &["rev-parse", "HEAD"])?;
        let url = output(&folder, "git", &["remote", "get-url", "origin"])?;
        if head != package.rev || url != package.url {
            return Err(format!("{} is at {head} from {url}", package.name).into());
        }
    }
    println!(
        "{} packages at their pinned commits",
        manifest.packages.len()
    );

    // `--no-build` fails if any Mathlib module would have to be compiled.
    let started = Instant::now();
    output(&proofs, "lake", &["build", "--no-build", "Mathlib"])?;
    println!(
        "Mathlib is up to date without building ({:?})",
        started.elapsed()
    );
    Ok(())
}

fn output(directory: &Path, program: &str, arguments: &[&str]) -> Outcome<String> {
    let result = Command::new(program)
        .current_dir(directory)
        .args(arguments)
        .output()?;
    if !result.status.success() {
        return Err(format!(
            "{program} {} failed: {}{}",
            arguments.join(" "),
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr)
        )
        .into());
    }
    Ok(String::from_utf8_lossy(&result.stdout).trim().to_owned())
}
