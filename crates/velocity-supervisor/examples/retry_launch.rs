//! #395: a failed process launch during resource pressure must retry, not exit.
//! Make the worker executable appear after the first failed spawn; no machine
//! resources are exhausted and no installed application is replaced.
use std::ffi::OsString;
use std::time::{Duration, Instant};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    if std::env::args().nth(1).as_deref() == Some("--worker") {
        return Ok(());
    }
    let directory = std::env::temp_dir().join(format!("velocity retry {}", std::process::id()));
    std::fs::create_dir_all(&directory)?;
    let program = directory.join(format!("worker{}", std::env::consts::EXE_SUFFIX));
    let source = std::env::current_exe()?;
    let target = program.clone();
    let available = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(1));
        std::fs::copy(source, target)
    });
    let started = Instant::now();
    velocity_supervisor::Supervisor::new(&program, [OsString::from("--worker")]).run();
    available.join().map_err(|_| "Worker preparation failed")??;
    if started.elapsed() < Duration::from_secs(5) {
        return Err("The failed launch did not back off".into());
    }
    println!("Failed launch retried automatically; worker exited normally; evidence {}", directory.display());
    Ok(())
}
