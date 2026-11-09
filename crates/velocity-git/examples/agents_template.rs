//! Run with an existing checkout path and optional project name; no GUI required.

use std::path::PathBuf;
use velocity_git::agents_template::install_default_agents;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut arguments = std::env::args_os().skip(1);
    let folder = PathBuf::from(arguments.next().ok_or("pass the project checkout path")?);
    let name = arguments
        .next()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();
    let created = install_default_agents(&folder, &name)?;
    println!(
        "{}: {}",
        if created { "Created" } else { "Preserved" },
        folder.join("AGENTS.md").display()
    );
    Ok(())
}
