//! Headless run of the built-in pack specifics (SPEC.md F18) against the real upstream repos:
//! `cargo run -p velocity-skills --example built_in_packs -- [library-folder]`.
//!
//! 1. Installs UI UX Pro Max, claude-skills, Game dev skills and REA from GitHub (or reuses a
//!    library folder from an earlier run).
//! 2. Prints each pack's real bundle counts and the skills a bundle selection turns on.
//! 3. Generates a design system in a fresh git project, then prints MASTER.md's path, the rule
//!    AGENTS.md gained and the commits it made.
//! 4. Detects engines in a Godot, a Unity, a three.js and a plain project.
//! 5. Prints the command that starts REA's MCP server.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use velocity_skills::{
    SkillLibrary, bundle_skill_counts, detect_game_engine, generate_design_system, rea_mcp_server,
    skills_in_bundles,
};

type Outcome = Result<(), Box<dyn std::error::Error>>;

fn main() -> Outcome {
    let scratch =
        std::env::temp_dir().join(format!("velocity-built-in-packs-{}", std::process::id()));
    let library_folder = std::env::args()
        .nth(1)
        .map_or_else(|| scratch.join("app-data"), PathBuf::from);
    let library = SkillLibrary::open(&library_folder)?;
    for pack in ["uiux", "cskills", "gamedev", "rea"] {
        if library.installed_pack(pack).is_err() {
            println!("installing {pack} ...");
            library.install_pack(pack, None, None)?;
        }
    }
    print_bundles(&library)?;
    design_system(&library, &scratch.join("Serenity Spa"))?;
    engines(&scratch)?;
    let server = rea_mcp_server(&library.installed_pack("rea")?)?;
    println!(
        "\nREA MCP server `{}`: {} {}",
        server.name,
        server.command,
        server.arguments.join(" ")
    );
    Ok(())
}

fn print_bundles(library: &SkillLibrary) -> Outcome {
    for counts in library.snapshot()?.bundle_counts {
        let chips: Vec<String> = counts
            .bundles
            .iter()
            .map(|bundle| format!("{} {}", bundle.bundle_id, bundle.skill_count))
            .collect();
        println!("{}: {}", counts.pack_id, chips.join(" · "));
    }
    let gamedev = library.installed_pack("gamedev")?;
    let godot = skills_in_bundles(&gamedev, &[String::from("godot")]);
    println!(
        "gamedev [godot] turns on {} skills, router included: {}",
        godot.len(),
        godot.iter().any(|skill| skill.relative_folder == "router")
    );
    let cskills = library.installed_pack("cskills")?;
    let engineering = skills_in_bundles(&cskills, &[String::from("eng")]);
    println!(
        "cskills [eng] turns on {} of {} discovered skills",
        engineering.len(),
        cskills.skills.len()
    );
    let check = bundle_skill_counts(&cskills);
    let total: u32 = check.bundles.iter().map(|bundle| bundle.skill_count).sum();
    println!("cskills bundles cover {total} skills");
    Ok(())
}

fn design_system(library: &SkillLibrary, project: &Path) -> Outcome {
    fs::create_dir_all(project)?;
    git(project, &["init", "-q"])?;
    fs::write(
        project.join("SPEC.md"),
        "# SPEC.md · Serenity Spa\n\nA booking site for a beauty spa and wellness service. Calm, fast and trustworthy.\n",
    )?;
    fs::write(
        project.join("AGENTS.md"),
        "# AGENTS.md\n\n## Code\n- Keep functions short.\n",
    )?;
    git(project, &["add", "-A"])?;
    git(
        project,
        &[
            "-c",
            "user.name=velocity",
            "-c",
            "user.email=velocity@localhost",
            "commit",
            "-q",
            "-m",
            "start",
        ],
    )?;
    // Commits made by the library use the machine's identity; give the scratch repo one.
    git(project, &["config", "user.name", "velocity"])?;
    git(project, &["config", "user.email", "velocity@localhost"])?;
    let status = generate_design_system(library, project, "Serenity Spa", false)?;
    println!(
        "\ndesign system: {} exists={} rule_in_agents={}",
        status.master_path, status.exists, status.rule_in_agents
    );
    println!("{}", fs::read_to_string(project.join("AGENTS.md"))?);
    println!("{}", git(project, &["log", "--oneline", "--name-only"])?);
    let again = generate_design_system(library, project, "Serenity Spa", true)?;
    println!(
        "regenerated: exists={} rule_in_agents={}",
        again.exists, again.rule_in_agents
    );
    println!(
        "git status after regenerate:\n{}",
        git(project, &["status", "--short"])?
    );
    Ok(())
}

fn engines(scratch: &Path) -> Outcome {
    let godot = scratch.join("godot-game");
    fs::create_dir_all(&godot)?;
    fs::write(godot.join("project.godot"), "config_version=5\n")?;
    let unity = scratch.join("unity-game");
    fs::create_dir_all(unity.join("ProjectSettings"))?;
    let three = scratch.join("three-site");
    fs::create_dir_all(three.join("web"))?;
    fs::write(
        three.join("web").join("package.json"),
        r#"{"dependencies":{"three":"^0.180.0","svelte":"^5"}}"#,
    )?;
    let plain = scratch.join("plain-cli");
    fs::create_dir_all(&plain)?;
    fs::write(
        plain.join("Cargo.toml"),
        "[package]\nname = \"plain\"\n\n[dependencies]\nserde = \"1\"\n",
    )?;
    println!();
    for project in [&godot, &unity, &three, &plain] {
        let detection = detect_game_engine(project);
        println!(
            "{}: {} -> {:?}",
            project.display(),
            detection.summary,
            detection.suggested_bundles
        );
    }
    Ok(())
}

fn git(folder: &Path, arguments: &[&str]) -> Result<String, Box<dyn std::error::Error>> {
    let output = Command::new("git")
        .arg("-C")
        .arg(folder)
        .args(arguments)
        .output()?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).into_owned().into());
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}
