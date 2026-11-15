//! #395: run the real desktop supervisor around two fatal worker aborts, then
//! a clean exit. The durable attempt file stands in for the retained app state.
//! Usage: recover <new attempt-file>, or seed it with two lines for normal close.
use std::io::Write;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args_os()
        .skip(1)
        .next_back()
        .ok_or("Pass an attempt-file")?;
    if velocity_supervisor::launch_current()? {
        let attempts = std::fs::read_to_string(&path)?.lines().count();
        if attempts != 3 {
            return Err(format!("Expected three durable attempts, got {attempts}").into());
        }
        println!("Recovered fatal exits; retained 3 attempts; normal close stopped the supervisor");
        return Ok(());
    }
    let attempts = std::fs::read_to_string(&path)
        .unwrap_or_default()
        .lines()
        .count()
        + 1;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)?;
    writeln!(file, "attempt {attempts} worker {}", std::process::id())?;
    file.sync_all()?;
    drop(file);
    if attempts < 3 {
        std::process::abort();
    }
    Ok(())
}
