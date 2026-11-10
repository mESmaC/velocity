//! Print the real goal-swarm inventory without starting agents or touching the desktop.
use velocity_store::StateStore;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut arguments = std::env::args().skip(1);
    let directory = arguments
        .next()
        .ok_or("usage: work_pool <state-directory> [project-id]")?;
    let store = StateStore::load_or_default(directory)?;
    let project = if let Some(identifier) = arguments.next() {
        store.project(identifier.parse()?)
    } else {
        store.active_project()
    }
    .ok_or("Project not found")?;
    let pool = velocity_threads::work_pool::read_work_pool(project)?;
    println!("{}", serde_json::to_string_pretty(&pool)?);
    Ok(())
}
