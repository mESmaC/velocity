//! Headless New project setup, using the production create and persistence path.

use std::path::PathBuf;
use std::sync::Mutex;
use velocity_core::layout_state::LayoutState;
use velocity_new_project::{NewProjectRequest, create_project};
use velocity_store::StateStore;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut arguments = std::env::args_os().skip(1);
    let folder = PathBuf::from(arguments.next().ok_or("pass a project folder")?);
    let state_folder = PathBuf::from(arguments.next().ok_or("pass a separate state folder")?);
    let request = NewProjectRequest::local_folder(
        folder.to_string_lossy().into_owned(),
        "Native setup demonstration".into(),
    );
    let store = Mutex::new(StateStore::load_or_default(state_folder)?);
    let layout = Mutex::new(LayoutState::default());
    let created = create_project(&request, &store, &layout, |_| Ok(()))?;
    println!(
        "Registered project {} at {}",
        created.identifier, created.repository_path
    );
    Ok(())
}
