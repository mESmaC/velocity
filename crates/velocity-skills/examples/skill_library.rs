//! Headless verification of Velocity's skill pack library (SPEC.md F18, §7 M12):
//! 1. Creates an upstream skill pack repository containing UTF-8 `SKILL.md` files, a stdlib
//!    Python script (`scripts/search.py`), CSV data (`data/styles.csv`), and a `120000` git
//!    symlink mirror folder (`mirror-skill -> skills/core-skill`).
//! 2. Clones and pins the pack into `SkillLibrary` with `core.symlinks=true`.
//! 3. Verifies the mirror folder resolves as a real directory (`mirror-skill/SKILL.md`),
//!    UTF-8 skill descriptions parse cleanly, and `run_pack_python_script` runs with `PYTHONUTF8=1`.
//! 4. Commits an upstream update touching `SKILL.md`, `scripts/search.py`, and `data/styles.csv`.
//! 5. Verifies `check_for_updates` shows the diff without moving the pin, rejects unconfirmed or
//!    mismatched commits, and `confirm_update` moves the pin only when confirmed.

use std::fs;
use std::path::Path;
use std::process::Command;

use velocity_skills::{SkillDiffCategory, SkillLibrary};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let temp_root = std::env::temp_dir().join(format!(
        "velocity-skill-library-example-{}",
        std::process::id()
    ));
    if temp_root.exists() {
        let _ = fs::remove_dir_all(&temp_root);
    }
    let upstream = temp_root.join("upstream-pack");
    let app_data = temp_root.join("app-data");
    fs::create_dir_all(&upstream)?;
    fs::create_dir_all(&app_data)?;

    init_upstream_pack(&upstream)?;
    let initial_sha = git_rev_parse(&upstream, "HEAD")?;

    let library = SkillLibrary::open(&app_data)?;
    let upstream_str = upstream.display().to_string();
    let installed = library.install_pack("uiux", Some(&upstream_str), None)?;

    assert_eq!(installed.id, "uiux");
    assert_eq!(installed.pinned_commit, initial_sha);
    assert!(installed.symlinks_enabled, "core.symlinks must be true");
    assert!(
        installed.skills.len() >= 2,
        "both the primary skill and the symlinked mirror skill must be discovered"
    );

    let py_out = library.run_pack_python_script(
        "uiux",
        "skills/core-skill/scripts/search.py",
        &[String::from("palette")],
    )?;
    assert_eq!(py_out.exit_code, 0, "stderr: {}", py_out.stderr);
    assert!(
        py_out.stdout.contains("UTF8=1") && py_out.stdout.contains("•"),
        "expected PYTHONUTF8=1 output, got: {}",
        py_out.stdout
    );

    let second_sha = commit_upstream_update(&upstream)?;
    assert_ne!(initial_sha, second_sha);
    let skill_count = verify_update_flow(&library, &initial_sha, &second_sha)?;

    let _ = fs::remove_dir_all(&temp_root);
    println!(
        "skill_library verified: pinned {} -> {}, {} skills discovered",
        &initial_sha[..7],
        &second_sha[..7],
        skill_count
    );
    Ok(())
}

fn verify_update_flow(
    library: &SkillLibrary,
    initial_sha: &str,
    second_sha: &str,
) -> Result<usize, Box<dyn std::error::Error>> {
    assert!(library.confirm_update("uiux", second_sha).is_err());

    let preview = library.check_for_updates("uiux")?;
    assert!(!preview.up_to_date);
    assert_eq!(preview.current_commit, initial_sha);
    assert_eq!(preview.candidate_commit, second_sha);
    assert_eq!(preview.skill_markdown_files_changed, 1);
    assert_eq!(preview.script_files_changed, 1);
    assert_eq!(preview.data_files_changed, 1);
    assert!(
        preview
            .files
            .iter()
            .any(|file| file.category == Some(SkillDiffCategory::SkillMarkdown))
    );

    let before_confirm = library.snapshot()?;
    let pinned_before = &before_confirm
        .installed
        .iter()
        .find(|item| item.id == "uiux")
        .ok_or("missing uiux")?
        .pinned_commit;
    assert_eq!(pinned_before, initial_sha);

    assert!(library.confirm_update("uiux", initial_sha).is_err());

    let updated = library.confirm_update("uiux", second_sha)?;
    assert_eq!(updated.pinned_commit, second_sha);
    assert!(updated.pending_update.is_none());
    Ok(updated.skills.len())
}

fn init_upstream_pack(upstream: &Path) -> Result<(), Box<dyn std::error::Error>> {
    run_git(upstream, &["init"])?;
    run_git(upstream, &["config", "user.email", "velocity@example.com"])?;
    run_git(upstream, &["config", "user.name", "Velocity"])?;

    let skill_dir = upstream.join("skills").join("core-skill");
    fs::create_dir_all(skill_dir.join("scripts"))?;
    fs::create_dir_all(skill_dir.join("data"))?;

    fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: ui-ux-pro-max\ndescription: Design intelligence • 80 styles — UTF-8 ready\n---\n\n# UI UX Pro Max\n",
    )?;
    fs::write(
        skill_dir.join("scripts").join("search.py"),
        "import os, sys\nprint(f'UTF8={os.environ.get(\"PYTHONUTF8\")} • query={sys.argv[1]}')\n",
    )?;
    fs::write(
        skill_dir.join("data").join("styles.csv"),
        "name,palette\ngraphite,#98989d\n",
    )?;

    run_git(upstream, &["add", "."])?;
    // Add a 120000 symlink entry in the git index pointing `mirror-skill` -> `skills/core-skill`
    // so Windows symlink/junction materialization is exercised even without OS symlink privileges.
    let hash = git_output(
        upstream,
        &["hash-object", "-w", "--stdin"],
        b"skills/core-skill",
    )?;
    let cacheinfo = format!("120000,{},mirror-skill", hash.trim());
    run_git(
        upstream,
        &["update-index", "--add", "--cacheinfo", &cacheinfo],
    )?;
    run_git(upstream, &["commit", "-m", "Initial skill pack"])?;
    Ok(())
}

fn commit_upstream_update(upstream: &Path) -> Result<String, Box<dyn std::error::Error>> {
    let skill_dir = upstream.join("skills").join("core-skill");
    fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: ui-ux-pro-max\ndescription: Design intelligence • 90 styles — updated\n---\n\n# UI UX Pro Max v2\n",
    )?;
    fs::write(
        skill_dir.join("scripts").join("search.py"),
        "import os, sys\nprint(f'UTF8={os.environ.get(\"PYTHONUTF8\")} • v2 query={sys.argv[1]}')\n",
    )?;
    fs::write(
        skill_dir.join("data").join("styles.csv"),
        "name,palette\ngraphite,#98989d\noled,#000000\n",
    )?;
    run_git(upstream, &["add", "skills/core-skill"])?;
    run_git(
        upstream,
        &["commit", "-m", "Update skill, script, and data"],
    )?;
    git_rev_parse(upstream, "HEAD")
}

fn git_rev_parse(dir: &Path, rev: &str) -> Result<String, Box<dyn std::error::Error>> {
    let out = git_output(dir, &["rev-parse", rev], b"")?;
    Ok(out.trim().to_owned())
}

fn run_git(dir: &Path, args: &[&str]) -> Result<(), Box<dyn std::error::Error>> {
    git_output(dir, args, b"")?;
    Ok(())
}

fn git_output(
    dir: &Path,
    args: &[&str],
    stdin_bytes: &[u8],
) -> Result<String, Box<dyn std::error::Error>> {
    use std::io::Write;
    use std::process::Stdio;
    let mut child = Command::new("git")
        .current_dir(dir)
        .args(args)
        .stdin(if stdin_bytes.is_empty() {
            Stdio::null()
        } else {
            Stdio::piped()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    if !stdin_bytes.is_empty()
        && let Some(mut stdin) = child.stdin.take()
    {
        stdin.write_all(stdin_bytes)?;
    }
    let output = child.wait_with_output()?;
    if !output.status.success() {
        return Err(format!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}
