//! F17 reference stress: identical input for the implementation and compiled Lean model.
use std::io::{Seek, Write};
use std::path::Path;
use std::process::{Output, Stdio};

use serde::{Deserialize, Serialize};

use crate::scout_directory::ReproductionDirectory;
use crate::scout_evidence::capture;
use crate::{Reproduction, ScoutIssueError};

/// Backend-only drivers, never exposed as a frontend shell command. Each script
/// adapts a program's output to the model's format and reads its cases from stdin.
#[derive(Debug, Deserialize)]
pub struct ReferenceComparison {
    pub implementation: Reproduction,
    pub reference: Reproduction,
    pub input: String,
}

#[derive(Debug, Serialize)]
pub struct ReferenceComparisonResult {
    pub matches: bool,
    pub implementation_output: String,
    pub reference_output: String,
    pub implementation_diagnostics: String,
    pub reference_diagnostics: String,
}

impl ReferenceComparison {
    /// Both drivers must succeed: a missing toolchain or crashed model is not a
    /// disagreement. Compare bytes exactly, including trailing newlines.
    pub fn run(&self, directory: &Path) -> Result<ReferenceComparisonResult, ScoutIssueError> {
        if self.input.len() > 48_000 {
            return Err(ScoutIssueError::Invalid(
                "Reference input exceeds 48000 bytes".into(),
            ));
        }
        let directory = std::fs::canonicalize(directory)?;
        let working = ReproductionDirectory::new(&directory)?;
        let implementation = run_driver(&self.implementation, &directory, &working, &self.input)?;
        let reference = run_driver(&self.reference, &directory, &working, &self.input)?;
        Ok(ReferenceComparisonResult {
            matches: implementation.stdout == reference.stdout,
            implementation_output: utf8(implementation.stdout)?,
            reference_output: utf8(reference.stdout)?,
            implementation_diagnostics: utf8(implementation.stderr)?,
            reference_diagnostics: utf8(reference.stderr)?,
        })
    }
}

fn run_driver(
    driver: &Reproduction,
    directory: &Path,
    working: &ReproductionDirectory,
    input: &str,
) -> Result<Output, ScoutIssueError> {
    let script = std::fs::canonicalize(directory.join(&driver.script))?;
    // Independent file handles guarantee both processes start at byte zero;
    // file stdin also avoids a writer deadlock when a driver exits early.
    let mut standard_input = tempfile::tempfile()?;
    standard_input.write_all(input.as_bytes())?;
    standard_input.rewind()?;
    let mut command = crate::command::hidden_command(&driver.program);
    command
        .current_dir(working.path())
        .args(&driver.arguments)
        .arg(working.script_path(directory, &script))
        .args(&driver.script_arguments)
        .stdin(Stdio::from(standard_input));
    let output = capture(&mut command)?;
    if !output.status.success() {
        return Err(ScoutIssueError::Invalid(format!(
            "Reference driver {} failed ({}): {}",
            driver.script.display(),
            output.status,
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    Ok(output)
}

fn utf8(bytes: Vec<u8>) -> Result<String, ScoutIssueError> {
    String::from_utf8(bytes).map_err(|error| ScoutIssueError::Invalid(error.to_string()))
}
