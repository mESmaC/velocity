use std::{error::Error, path::Path};
use velocity_store::{ProviderKind, Settings, StateStore};
use velocity_store::threads::NewThread;
fn main() -> Result<(), Box<dyn Error>> {
    let data = std::env::args().nth(1).unwrap();
    let mut store = StateStore::load_or_default(Path::new(&data))?;
    let mut settings = Settings::default();
    settings.providers.default_provider = ProviderKind::Claude;
    settings.providers.default_model = Some("Opus 5.5".into());
    store.set_settings(settings)?;
    store.set_provider_signed_out(ProviderKind::Claude, true)?;
    let settings = store.settings();
    assert!(!settings.provider_usable(ProviderKind::Claude));
    let thread = NewThread::new("Work".into(), "vel/work".into(), "C:\\work".into(), &settings);
    let duty = store.duty_dialog_defaults();
    let project = velocity_new_project::new_project_defaults(Path::new("C:\\"), settings.clone());
    println!("Claude usable after sign out: {}", settings.provider_usable(ProviderKind::Claude));
    println!("New thread still selects: {:?} / {}", thread.provider, thread.model_name);
    println!("Duty dialog still selects: {:?} / {}", duty.provider, duty.model_name);
    println!("New project still selects: {:?} / {:?}", project.provider, project.model_name);
    if [thread.provider, duty.provider, project.provider].contains(&ProviderKind::Claude) {
        return Err("F15: signed-out default remains selected for new work".into());
    }
    Ok(())
}
