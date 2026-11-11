//! Headless run of the built-in pack specifics (SPEC.md F18) against the real upstream repos:
//! `cargo run -p velocity-skills --example built_in_packs -- [library-folder]`.
//!
//! 1. Installs UI UX Pro Max, claude-skills, Game dev skills and REA from GitHub (or reuses a
//!    library folder from an earlier run).
//! 2. Generates a design system in a fresh git project, then prints the rule AGENTS.md gained
//!    and the commits it made.
//! 3. Prints the Settings → Skills cards with all four packs on: real bundle counts, skills
//!    on, the design system row, the engine line and REA's server.
//! 4. Detects engines in a Godot, a Unity, a three.js and a plain project.
//! 5. Starts sessions the way every role does: one in a linked worktree of the project gets
//!    REA's MCP server, one outside it gets none.

// A console example: its children share its console, so no hidden_command (#232).
#![allow(clippy::disallowed_methods)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use velocity_acp::client::{
    AgentLaunch, SharedServer, install_shared_servers, launch_shared_servers,
};
use velocity_core::skills::{ProjectSkills, SkillSettingsEdit};
use velocity_skills::settings::settings_view;
use velocity_skills::{
    SkillLibrary, add_pack_details, generate_design_system, pack_shared_servers,
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
    let project = scratch.join("Serenity Spa");
    design_system(&library, &project)?;
    let choices = choices();
    print_cards(&library, &choices, &project)?;
    engines(&scratch)?;
    shared_with_sessions(library, choices, &project)?;
    Ok(())
}

fn print_cards(library: &SkillLibrary, choices: &ProjectSkills, checkout: &Path) -> Outcome {
    let snapshot = library.snapshot()?;
    let mut view = settings_view(1, String::from("Serenity Spa"), choices, &snapshot);
    add_pack_details(&mut view, checkout, &snapshot);
    for card in &view.cards {
        let chips: Vec<String> = card
            .catalog
            .bundles
            .iter()
            .map(|bundle| format!("{} {}", bundle.id, bundle.skill_count))
            .collect();
        println!(
            "{}: {} skills on · chips: {}",
            card.catalog.name,
            card.enabled_count,
            chips.join(" · ")
        );
        if let Some(design) = &card.details.design_system {
            println!(
                "  design system {} exists={} rule={}",
                design.master_path, design.exists, design.rule_in_agents
            );
        }
        if let Some(server) = &card.details.shared_server {
            println!(
                "  shared MCP server {}: {} {}",
                server.name,
                server.command,
                server.arguments.join(" ")
            );
        }
    }
    println!(
        "{} skills on · ≈{} tokens",
        view.enabled_count, view.description_tokens
    );
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
        // Engine detection is the project scan's (F18 Suggestions, #165).
        for suggestion in velocity_skills::suggestions::scan_project(project, None) {
            println!(
                "{}: {} -> {} {:?}",
                project.display(),
                suggestion.reason,
                suggestion.pack,
                suggestion.bundles
            );
        }
    }
    Ok(())
}

fn choices() -> ProjectSkills {
    let mut choices = ProjectSkills::default();
    for pack in ["uiux", "cskills", "gamedev", "rea"] {
        choices.apply(SkillSettingsEdit::Pack {
            identifier: pack.into(),
            enabled: true,
        });
    }
    choices.apply(SkillSettingsEdit::Bundle {
        identifier: "gamedev".into(),
        bundle: "godot".into(),
        enabled: true,
    });
    choices
}

// The same path every role's `session/new` takes: the app's source, keyed by the session's
// folder. A linked worktree of the project must get REA; a folder outside it must not.
fn shared_with_sessions(library: SkillLibrary, choices: ProjectSkills, project: &Path) -> Outcome {
    let root = fs::canonicalize(project)?;
    install_shared_servers(move |folder| {
        let Ok(checkout) = velocity_git::main_checkout(folder) else {
            return Vec::new();
        };
        if checkout != root {
            return Vec::new();
        }
        pack_shared_servers(&choices, &library)
            .into_iter()
            .map(|server| SharedServer::new(server.name, server.command, server.arguments))
            .collect()
    });
    let worktree = project.with_file_name("serenity-spa-wt");
    git(
        project,
        &[
            "worktree",
            "add",
            "-q",
            "--detach",
            &worktree.display().to_string(),
        ],
    )?;
    for folder in [worktree.clone(), project.with_file_name("plain-cli")] {
        // An added agent (no own config), so nothing is left to an agent's own entry.
        let launch = AgentLaunch::new(PathBuf::from("agent"));
        let servers = launch_shared_servers(&launch, &folder);
        println!(
            "
session/new in {} gets {} shared server(s): {:?}",
            folder.display(),
            servers.len(),
            servers
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
