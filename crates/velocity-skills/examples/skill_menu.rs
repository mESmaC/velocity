//! Headless F4/F18 `$` menu driver: `cargo run -p velocity-skills --example skill_menu`.
//! Installs a two-skill pack into a temporary library, then shows the composer's `$` menu for a
//! project before and after the pack is switched on in Settings → Skills.

// A console example: its children share its console, so no hidden_command (#232).
#![allow(clippy::disallowed_methods)]

use std::fs;
use std::path::Path;
use std::process::Command;

use velocity_core::layout_state::LayoutState;
use velocity_core::layout_view::{LayoutAction, LayoutData};
use velocity_core::skills::{ProjectSkills, SkillSettingsEdit};
use velocity_skills::SkillLibrary;
use velocity_skills::menu::skill_menu_entries;

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

    let mut choices = ProjectSkills::default();
    print_menu("pack off", &choices, &snapshot);
    choices.apply(SkillSettingsEdit::Pack {
        identifier: pack.id.clone(),
        enabled: true,
    });
    print_menu("pack on", &choices, &snapshot);

    let _ = fs::remove_dir_all(&root);
    Ok(())
}

fn print_menu(
    label: &str,
    choices: &ProjectSkills,
    snapshot: &velocity_skills::SkillLibrarySnapshot,
) {
    let mut layout = LayoutState::default();
    layout.replace_data(LayoutData {
        project_identifier: Some(7),
        ..LayoutData::default()
    });
    layout.set_project_skills(7, skill_menu_entries(choices, snapshot));
    layout.apply(LayoutAction::ComposerText("Build the login page $".into()));
    let items: Vec<String> = layout
        .snapshot()
        .slash_items
        .iter()
        .map(|item| format!("{} · {}", item.command, item.description))
        .collect();
    println!("{label}: $ menu [{}]", items.join(" | "));
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
