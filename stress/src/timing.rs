//! How long each step of a stress run took (#434). The release gate's stress stage was one
//! opaque half hour; the run now ends with its steps listed slowest first, and under GitHub
//! Actions the same list goes to the job summary, so every release records its own profile.

use std::{
    fs::OpenOptions,
    io::Write,
    time::{Duration, Instant},
};

/// GitHub Actions appends Markdown written to this file to the job's summary page.
const SUMMARY_VARIABLE: &str = "GITHUB_STEP_SUMMARY";

struct Step {
    label: String,
    elapsed: Duration,
    passed: bool,
}

#[derive(Default)]
pub struct Timings {
    steps: Vec<Step>,
}

impl Timings {
    /// Records a step that started at `started` and has just finished.
    pub fn record(&mut self, label: &str, started: Instant, passed: bool) {
        self.steps.push(Step {
            label: label.to_owned(),
            elapsed: started.elapsed(),
            passed,
        });
    }

    /// Prints the steps slowest first, and adds them to the job summary when there is one.
    pub fn report(&self) {
        let total: Duration = self.steps.iter().map(|step| step.elapsed).sum();
        let mut slowest: Vec<&Step> = self.steps.iter().collect();
        slowest.sort_by(|left, right| right.elapsed.cmp(&left.elapsed));
        println!("\nTime by step, slowest first ({} in all):", minutes(total));
        for step in &slowest {
            let result = if step.passed { "" } else { "  FAILED" };
            println!("  {:>12}  {}{result}", minutes(step.elapsed), step.label);
        }
        let Some(path) = std::env::var_os(SUMMARY_VARIABLE) else {
            return;
        };
        let written = OpenOptions::new()
            .append(true)
            .create(true)
            .open(path)
            .and_then(|mut file| file.write_all(summary(&slowest, total).as_bytes()));
        // The summary is a record of the run, never a reason to fail it.
        if let Err(error) = written {
            eprintln!("stress: could not write the job summary: {error}");
        }
    }
}

fn summary(slowest: &[&Step], total: Duration) -> String {
    let mut text = format!(
        "### Stress suite: {}\n\n| Step | Time | Result |\n| --- | ---: | --- |\n",
        minutes(total)
    );
    for step in slowest {
        let result = if step.passed { "passed" } else { "failed" };
        // A `|` in a label would end its table cell early.
        let label = step.label.replace('|', "\\|");
        text.push_str(&format!(
            "| `{label}` | {} | {result} |\n",
            minutes(step.elapsed)
        ));
    }
    text.push('\n');
    text
}

/// `4 min 07 s` from a minute up, `12.3 s` below it.
fn minutes(elapsed: Duration) -> String {
    let seconds = elapsed.as_secs();
    if seconds < 60 {
        return format!("{:.1} s", elapsed.as_secs_f64());
    }
    format!("{} min {:02} s", seconds / 60, seconds % 60)
}
