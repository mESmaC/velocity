//! Headless project scan (F3, F18 Suggestions): skill_suggestions <project> [branch] [choices.json]
//! Prints what the scan suggests and the project's skill choices after a re-check.
use velocity_core::skills::ProjectSkills;
use velocity_skills::suggestions::{known_tip, scan_project};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut arguments = std::env::args().skip(1);
    let project = arguments.next().ok_or("Pass a project folder")?;
    let root = std::path::Path::new(&project);
    let tip = arguments.next().and_then(|branch| known_tip(root, &branch));
    let mut choices: ProjectSkills = match arguments.next() {
        Some(file) => serde_json::from_slice(&std::fs::read(file)?)?,
        None => ProjectSkills::default(),
    };
    let suggestions = scan_project(root, tip.as_deref());
    let changed = choices.apply_suggestions(suggestions);
    println!("tip: {}", tip.as_deref().unwrap_or("(working tree only)"));
    println!("changed: {changed}");
    println!("{}", serde_json::to_string_pretty(&choices)?);
    Ok(())
}
