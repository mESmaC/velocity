//! Headless F14 wait driver against a caller-supplied disposable git fixture.
//! Arguments: app-data repository linked-worktree branch base outcome.
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::Duration;
use velocity_airc::{Channel, waits::Checkout};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let [data, repository, worktree, branch, base, outcome] = arguments.as_slice() else {
        return Err("Pass app-data repository linked-worktree branch base outcome".into());
    };
    let data = PathBuf::from(data);
    let channel = Channel::open(&data, "wait-demo")?;
    channel.join("thread-1", "first")?;
    channel.join("thread-2", "second")?;
    channel.claim(
        "thread-1",
        "First piece",
        "vel/first",
        vec!["shared.rs".into()],
    )?;
    channel.claim("thread-2", "Second piece", branch, vec!["shared.rs".into()])?;
    channel.register_checkout(
        "thread-2",
        Checkout::new(
            repository.clone(),
            worktree.clone(),
            branch.clone(),
            base.clone(),
        ),
    )?;
    let (sender, receiver) = mpsc::channel();
    let waiting = Channel::open(&data, "wait-demo")?;
    let worker = std::thread::spawn(move || {
        let result = waiting.wait_and_continue("thread-2", "first", "shared.rs");
        let _ = sender.send(result);
    });
    match receiver.recv_timeout(Duration::from_millis(500)) {
        Err(mpsc::RecvTimeoutError::Timeout) => {
            println!("Dependency call remains pending; no continuation ran")
        }
        other => return Err(format!("Wait ended before the dependency: {other:?}").into()),
    }
    if channel.dependency_waits()?.len() != 1 {
        return Err("Wait was not durably recorded".into());
    }
    channel.dependency_ended("thread-1", outcome, Some(42))?;
    let result = receiver.recv_timeout(Duration::from_secs(30))??;
    worker.join().map_err(|_| "Wait worker panicked")?;
    println!("{result}");
    for event in channel.events(0)? {
        println!("{} {} {}", event.kind, event.nick, event.text);
    }
    Ok(())
}
