use std::{fs, path::PathBuf, sync::Mutex};
use velocity_core::layout_state::LayoutState;
use velocity_new_project::{NewProjectRequest, ProjectLocationRequest, create_project};
use velocity_store::StateStore;

fn report(label: &str, passed: bool, failures: &mut u32) {
    println!("{} {label}", if passed { "PASS" } else { "FAIL" });
    if !passed { *failures += 1; }
}

fn main() {
    let fixture = PathBuf::from(std::env::args().nth(1).expect("fixture path"));
    let data = fixture.join("app data");
    let store = StateStore::load_or_default(&data).expect("initial state");
    let before = store.state().clone();
    let disk_before = fs::read(data.join("state.json")).expect("saved baseline");
    let blocker = data.join("state.json.partial");
    fs::create_dir(&blocker).expect("inject failed file creation");
    let settings = before.settings.providers.clone();
    let request = NewProjectRequest {
        location: ProjectLocationRequest::LocalFolder {
            path: fixture.join("new project").display().to_string(),
            create_if_needed: true,
        },
        model_name: settings.default_model,
        effort: settings.default_effort,
        speed: settings.default_speed,
        brief: "PR duty failed-save fixture".into(),
    };
    let store = Mutex::new(store);
    let layout = Mutex::new(LayoutState::default());
    let first = create_project(&request, &store, &layout, |_| Ok(()));
    println!("First Create: {first:?}");
    let unchanged = {
        let guard = store.lock().expect("store lock");
        println!("Projects after failed Create: {}", guard.state().projects.len());
        guard.state() == &before
    };
    let mut failures = 0;
    report("injected save failure is reported", first.is_err(), &mut failures);
    report("failed Create preserves the live project state", unchanged, &mut failures);
    report("failed Create preserves the saved snapshot",
        fs::read(data.join("state.json")).expect("read snapshot") == disk_before, &mut failures);
    fs::remove_dir(&blocker).expect("remove the empty injected blocker");
    let retry = create_project(&request, &store, &layout, |_| Ok(()));
    println!("Retry after repairing state path: {retry:?}");
    report("Create can be retried after the save path is repaired", retry.is_ok(), &mut failures);
    println!("Fixture retained at {}", fixture.display());
    if failures != 0 { std::process::exit(1); }
}
