//! Headless F4/F18 `$` menu driver: `cargo run -p velocity-skills --example skill_menu`.
//! Installs a two-skill pack once, then invokes it from two projects with empty always-on lists.

// A console example: its children share its console, so no hidden_command (#232).
#![allow(clippy::disallowed_methods)]

use std::fs;
use std::path::Path;
use std::process::Command;

use velocity_core::layout_state::LayoutState;
use velocity_core::layout_view::{LayoutAction, LayoutData};
use velocity_core::skills::ProjectSkills;
use velocity_skills::SkillLibrary;
use velocity_skills::menu::skill_menu_entries;
use velocity_skills::workspace::{link_workspace_skills, prepare_skill_prompt};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = std::env::temp_dir().join(format!("velocity-skill-menu-{}", std::process::id()));
    let upstream = root.join("upstream pack");
    fs::create_dir_all(&upstream)?;
    write_pack(&upstream)?;

    let library = SkillLibrary::open(&root.join("app data"))?;
    let pack = library.install_pack(
        "example/design-pack",
        Some(&upstream.display().to_string()),
        None,
    )?;
    println!(
        "installed {} at {} with {} skills",
        pack.id,
        pack.short_commit,
        pack.skills.len()
    );
    let snapshot = library.snapshot()?;

    for project in [7, 8] {
        let workspace = root.join(format!("project {project}"));
        fs::create_dir_all(&workspace)?;
        link_workspace_skills(&ProjectSkills::default(), &snapshot, &workspace)?;
        let manifest = workspace
            .join(".velocity/invoked-skills")
            .join("palette-picker")
            .join("SKILL.md");
        assert!(!manifest.exists());
        let prompt = compose_invocation(project, &snapshot);
        let instruction = prepare_skill_prompt(&prompt, &snapshot, &workspace)
            .ok_or("The installed skill did not resolve")?;
        assert!(instruction.contains(&manifest.display().to_string()));
        assert!(manifest.is_file());
        assert!(
            !workspace
                .join(".velocity/invoked-skills/form-patterns")
                .exists()
        );
        assert!(!workspace.join(".agents/skills/palette-picker").exists());
        println!("project {project}: {instruction}");
    }
    library.remove_pack(&pack.id)?;
    assert!(skill_menu_entries(&library.snapshot()?).is_empty());

    let _ = fs::remove_dir_all(&root);
    Ok(())
}

fn compose_invocation(project: u32, snapshot: &velocity_skills::SkillLibrarySnapshot) -> String {
    let mut layout = LayoutState::default();
    layout.replace_data(LayoutData {
        project_identifier: Some(project),
        ..LayoutData::default()
    });
    layout.set_installed_skills(skill_menu_entries(snapshot));
    layout.apply(LayoutAction::ComposerText("Build the login page $".into()));
    let items: Vec<String> = layout
        .snapshot()
        .slash_items
        .iter()
        .map(|item| format!("{} · {}", item.command, item.description))
        .collect();
    println!("project {project}: $ menu [{}]", items.join(" | "));
    assert_eq!(items.len(), 2);
    layout.apply(LayoutAction::ComposerText("Use $palette".into()));
    layout.apply(LayoutAction::SlashComplete);
    let prompt = layout.snapshot().composer_text;
    assert_eq!(prompt, "Use $palette-picker ");
    prompt
}

fn write_pack(upstream: &Path) -> Result<(), Box<dyn std::error::Error>> {
    for (folder, name, description) in [
        (
            "palettes",
            "palette-picker",
            "pick a palette for the design system",
        ),
        ("forms", "form-patterns", "accessible form layouts"),
    ] {
        let directory = upstream.join("skills").join(folder);
        fs::create_dir_all(&directory)?;
        fs::write(
            directory.join("SKILL.md"),
            format!("---\nname: {name}\ndescription: {description}\n---\n\n# {name}\n"),
        )?;
    }
    for arguments in [
        &["init", "-q"][..],
        &[
            "-c",
            "user.name=Velocity",
            "-c",
            "user.email=velocity@example.com",
            "add",
            ".",
        ],
        &[
            "-c",
            "user.name=Velocity",
            "-c",
            "user.email=velocity@example.com",
            "commit",
            "-q",
            "-m",
            "Two skills",
        ],
    ] {
        let status = Command::new("git")
            .args(arguments)
            .current_dir(upstream)
            .status()?;
        if !status.success() {
            return Err(format!("git {arguments:?} failed").into());
        }
    }
    Ok(())
}
