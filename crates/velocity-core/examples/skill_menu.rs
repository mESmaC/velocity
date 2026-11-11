//! Headless F4 `$` skill menu driver: `cargo run -p velocity-core --example skill_menu`.
//! Types into the home composer of a locked project with two enabled skills, prints the menu
//! after each keystroke, picks a skill with `Tab`, and prints the prompt the agent receives.

use velocity_core::layout_state::LayoutState;
use velocity_core::layout_view::{LayoutAction, LayoutData, LayoutIntent};
use velocity_core::skill_menu::{SkillMenuEntry, loaded_skill};

fn print_menu(layout: &LayoutState, typed: &str) {
    let snapshot = layout.snapshot();
    let items: Vec<String> = snapshot
        .slash_items
        .iter()
        .map(|item| format!("{} ({})", item.command, item.description))
        .collect();
    println!("typed {typed:?} -> menu [{}]", items.join(", "));
}

fn main() {
    let mut layout = LayoutState::default();
    // `..Default::default()` keeps this building when `LayoutData` gains a field.
    layout.replace_data(LayoutData {
        project_identifier: Some(1),
        project_name: "demo".into(),
        ..LayoutData::default()
    });
    layout.set_project_spec_locked(Some(true));
    layout.set_project_skills(
        1,
        vec![
            SkillMenuEntry::new("ui-ux-pro-max", "design system, styles, palettes, fonts"),
            SkillMenuEntry::new("senior-frontend", "engineering · frontend"),
        ],
    );

    for typed in [
        "Restyle $",
        "Restyle $u",
        "Restyle $UI",
        "Restyle $x",
        "/ta",
    ] {
        layout.apply(LayoutAction::ComposerText(typed.into()));
        print_menu(&layout, typed);
    }

    layout.apply(LayoutAction::ComposerText(
        "Restyle the home page with $u".into(),
    ));
    layout.apply(LayoutAction::SlashComplete);
    let picked = layout.snapshot().composer_text;
    println!("after Tab -> {picked:?}");

    layout.apply(LayoutAction::ComposerText(format!(
        "{picked}and keep $HOME paths"
    )));
    match layout.apply(LayoutAction::SubmitComposer) {
        Some(LayoutIntent::StartPrompt(prompt)) => println!("agent receives:\n{prompt}"),
        other => println!("unexpected intent: {other:?}"),
    }

    for (title, raw, path) in [
        ("Load skill: ui-ux-pro-max", None, None),
        ("Skill", Some("senior-frontend"), None),
        (
            "Read SKILL.md",
            None,
            Some(r"X:\wt\demo\.agents\skills\ui-ux-pro-max\SKILL.md"),
        ),
        ("Read README.md", None, Some("README.md")),
    ] {
        let step = loaded_skill(title, raw, path).map_or_else(
            || "not a skill load".to_owned(),
            |name| format!("Skill · {name}"),
        );
        println!("tool step {title:?} -> {step}");
    }
}
