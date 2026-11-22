//! Risk (#675): valid Lake global options such as `-d=proofs` and `--try-cache` must not
//! bypass on-demand Mathlib cache preparation before `lake` builds on a cold workspace.
//!
//! Adapted from scout-1's `target/scout-546-lake-dir.py` in mESmaC/velocity#675: the original
//! Python script invoked a live Velocity agent shell's `lake.exe` shim, which is unavailable in
//! standalone release-gate / CI runs and points at the host app rather than the workspace under
//! test. This harness creates the exact same temporary Lake project with the workspace's pinned
//! Mathlib manifest entry and asserts directly against `velocity_rosen::lean_cache::pending_mathlib_cache`
//! (the shared gate called by both the shell `lake` shim and `queue_run`).

use std::error::Error;
use std::fs;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value;
use velocity_rosen::lean_cache::{PendingMathlibCache, pending_mathlib_cache};

fn strings(items: &[&str]) -> Vec<String> {
    items.iter().map(|item| (*item).to_owned()).collect()
}

fn main() -> Result<(), Box<dyn Error>> {
    let workspace = std::env::current_dir()?;
    let unique = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let root = std::env::temp_dir().join(format!("velocity-675-lake-options-{unique}"));
    let project = root.join("proofs");
    fs::create_dir_all(&project)?;

    let result = run_probe(&workspace, &root, &project);
    let _ = fs::remove_dir_all(&root);
    result
}

fn run_probe(workspace: &Path, root: &Path, project: &Path) -> Result<(), Box<dyn Error>> {
    fs::copy(
        workspace.join("proofs/lean-toolchain"),
        project.join("lean-toolchain"),
    )?;
    fs::write(
        project.join("lakefile.toml"),
        "name = \"cache_probe\"\ndefaultTargets = [\"Probe\"]\n[[lean_lib]]\nname = \"Probe\"\n",
    )?;
    fs::write(project.join("Probe.lean"), "def probe : Nat := 1\n")?;

    let mut manifest: Value = serde_json::from_str(&fs::read_to_string(
        workspace.join("proofs/lake-manifest.json"),
    )?)?;
    manifest["name"] = Value::String("cache_probe".into());
    let mathlib_packages: Vec<Value> = manifest["packages"]
        .as_array()
        .ok_or("proofs/lake-manifest.json has no packages array")?
        .iter()
        .filter(|package| package.get("name").and_then(Value::as_str) == Some("mathlib"))
        .cloned()
        .collect();
    let expected_rev = mathlib_packages
        .first()
        .and_then(|package| package.get("rev"))
        .and_then(Value::as_str)
        .ok_or("proofs/lake-manifest.json has no mathlib rev")?
        .to_owned();
    manifest["packages"] = Value::Array(mathlib_packages);
    fs::write(
        project.join("lake-manifest.json"),
        serde_json::to_string(&manifest)?,
    )?;

    let expected = PendingMathlibCache {
        project: project.to_path_buf(),
        rev: expected_rev,
    };

    let control = pending_mathlib_cache(
        root,
        None,
        &strings(&["-d", "proofs", "--no-build", "build"]),
    );
    assert_eq!(
        control.as_ref(),
        Some(&expected),
        "Control (-d proofs) did not request Mathlib cache preparation"
    );

    for case in [
        &["-d=proofs", "--no-build", "build"][..],
        &["--try-cache", "-d", "proofs", "--no-build", "build"][..],
        &["-dproofs", "--no-build", "build"][..],
        &["build", "-d=proofs", "--no-build"][..],
        &["build", "Probe", "-d=proofs", "--no-build"][..],
        &[
            "--rehash",
            "--fail-fast",
            "--allow-empty",
            "--fail-level=warning",
            "-d=proofs",
            "--no-build",
            "build",
        ][..],
        &["exe", "-d=proofs", "export-fixtures"][..],
    ] {
        let actual = pending_mathlib_cache(root, None, &strings(case));
        assert_eq!(
            actual.as_ref(),
            Some(&expected),
            "Managed Lake skipped cache preparation for {case:?}"
        );
    }

    for skipped in [
        &["exe", "-d=proofs", "cache", "get"][..],
        &["-d=proofs", "exe", "cache", "get"][..],
        &["-d=proofs", "build", "--help"][..],
        &["-d=proofs", "clean"][..],
    ] {
        assert_eq!(
            pending_mathlib_cache(root, None, &strings(skipped)),
            None,
            "Managed Lake unexpectedly requested cache preparation for {skipped:?}"
        );
    }

    Ok(())
}
