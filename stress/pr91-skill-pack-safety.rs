// Risks (PR #91, F18 skill library):
// 1. Data loss: a custom repo `_/_` (or `-/-`) is a valid slug whose pack id is empty, so the
//    pack's folder is the library's `packs` folder and install clears it, deleting every pack.
// 2. Command injection: without Developer Mode the library makes junctions through
//    `cmd /C mklink /J <link> <target>`, where <link> is a path from the pack's own tree. Rust
//    quotes only arguments with spaces, so `x&rd` reaches cmd.exe as two commands.
// Run against the real velocity-skills library: copy into crates/velocity-skills/examples/ and
// `cargo run -p velocity-skills --example pr91-skill-pack-safety`. Risk 2 replays the exact
// call from library.rs, because a machine with Developer Mode never reaches that fallback.
use std::fs;
use std::path::Path;
use std::process::Command;

use velocity_skills::SkillLibrary;

fn git(directory: &Path, arguments: &[&str]) -> Result<(), Box<dyn std::error::Error>> {
    let status = Command::new("git")
        .current_dir(directory)
        .args(arguments)
        .status()?;
    if !status.success() {
        return Err(format!("git {arguments:?} failed").into());
    }
    Ok(())
}

fn report(name: &str, passed: bool, detail: &str) -> usize {
    println!("{} {name}: {detail}", if passed { "PASS" } else { "FAIL" });
    usize::from(!passed)
}

fn empty_pack_id(root: &Path) -> Result<usize, Box<dyn std::error::Error>> {
    let upstream = root.join("upstream");
    fs::create_dir_all(upstream.join("skills").join("core"))?;
    fs::write(
        upstream.join("skills").join("core").join("SKILL.md"),
        "---\nname: core\ndescription: Core skill\n---\nBody\n",
    )?;
    git(&upstream, &["init", "--quiet", "--initial-branch=main"])?;
    git(&upstream, &["add", "."])?;
    git(
        &upstream,
        &["-c", "user.name=stress", "-c", "user.email=stress@example.invalid", "commit", "--quiet", "-m", "pack"],
    )?;
    let library = SkillLibrary::open(&root.join("app-data"))?;
    let source = upstream.display().to_string();
    let installed = library.install_pack("acme/tools", Some(&source), None)?;
    let kept = Path::new(&installed.clone_path).to_path_buf();
    let outcome = library.install_pack("_/_", Some(&source), None);
    let mut failures = report(
        "`_/_` is refused",
        outcome.is_err(),
        &format!("{:?}", outcome.as_ref().map(|pack| pack.clone_path.clone())),
    );
    failures += report(
        "installed packs survive",
        kept.join("skills").join("core").join("SKILL.md").exists(),
        &kept.display().to_string(),
    );
    Ok(failures)
}

#[cfg(windows)]
fn mklink(link: &Path, target: &Path, quoted: bool) -> Result<(), Box<dyn std::error::Error>> {
    use std::os::windows::process::CommandExt;
    let mut command = Command::new("cmd");
    command.args(["/C", "mklink", "/J"]);
    if quoted {
        command
            .raw_arg(format!("\"{}\"", link.display()))
            .raw_arg(format!("\"{}\"", target.display()));
    } else {
        command.arg(link).arg(target);
    }
    command.output()?;
    Ok(())
}

#[cfg(windows)]
fn injection(root: &Path) -> Result<usize, Box<dyn std::error::Error>> {
    let mut failures = 0;
    for quoted in [false, true] {
        let folder = root.join(if quoted { "quoted" } else { "unquoted" });
        let victim = folder.join("victim");
        fs::create_dir_all(&victim)?;
        // A pack whose symlink is named `x&rd` turns the call into `mklink /J ...\x` and
        // `rd <target>`, removing the (empty) target folder.
        mklink(&folder.join("x&rd"), &victim, quoted)?;
        let name = if quoted { "quoted (fixed)" } else { "unquoted (PR head)" };
        let survived = victim.exists();
        if quoted {
            failures += report(name, survived, "target folder kept, nothing injected");
        } else {
            println!(
                "{} {name}: target folder {}",
                if survived { "INFO" } else { "REPRO" },
                if survived { "kept" } else { "removed by the injected `rd`" }
            );
        }
    }
    Ok(failures)
}

#[cfg(not(windows))]
fn injection(_: &Path) -> Result<usize, Box<dyn std::error::Error>> {
    Ok(0)
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = std::env::current_dir()?
        .join(".tools")
        .join(format!("pr91-skill-pack-safety-{}", std::process::id()));
    fs::create_dir_all(&root)?;
    let failures = empty_pack_id(&root.join("library"))? + injection(&root.join("cmd"))?;
    let _ = fs::remove_dir_all(&root);
    if failures > 0 {
        return Err(format!("{failures} check(s) failed").into());
    }
    println!("All skill pack safety checks passed.");
    Ok(())
}
