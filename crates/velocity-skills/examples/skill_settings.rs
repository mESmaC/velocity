//! Headless settings projection: skill_settings <app-data> <choices.json>.
use velocity_core::skills::ProjectSkills;
use velocity_skills::{SkillLibrary, settings::settings_view};
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut arguments = std::env::args().skip(1);
    let directory = arguments.next().ok_or("Pass an app-data directory")?;
    let choices_file = arguments.next().ok_or("Pass a project choices JSON file")?;
    let choices: ProjectSkills = serde_json::from_slice(&std::fs::read(choices_file)?)?;
    let library = SkillLibrary::open(std::path::Path::new(&directory))?;
    if let Some(source) = arguments.next() {
        let pack = arguments.next().unwrap_or_else(|| "cskills".into());
        library.install_pack(&pack, Some(&source), None)?;
    }
    let view = settings_view(1, "Headless project".into(), &choices, &library.snapshot()?);
    println!("{}", serde_json::to_string_pretty(&view)?);
    Ok(())
}
