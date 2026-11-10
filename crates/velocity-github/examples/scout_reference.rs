//! Headless reference comparison: scout_reference <checkout> <comparison.json>.
//! Drivers read identical UTF-8 cases on stdin and emit the same canonical format.
use std::path::Path;

use velocity_github::ReferenceComparison;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let directory = arguments.first().ok_or("Provide a checkout directory")?;
    let request = arguments.get(1).ok_or("Provide a comparison JSON file")?;
    let comparison: ReferenceComparison = serde_json::from_slice(&std::fs::read(request)?)?;
    let result = comparison.run(Path::new(directory))?;
    println!("{}", serde_json::to_string_pretty(&result)?);
    if !result.matches {
        return Err("Implementation disagrees with the reference model".into());
    }
    Ok(())
}
