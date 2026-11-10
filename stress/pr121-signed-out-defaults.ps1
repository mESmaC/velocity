$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$fixture = Join-Path $env:TEMP ('velocity-pr121-signout-' + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path (Join-Path $fixture 'src') -Force | Out-Null
$store = (Join-Path $root 'crates/velocity-store').Replace('\', '/')
$project = (Join-Path $root 'crates/velocity-new-project').Replace('\', '/')
@"
[package]
name = "pr121-signout"
version = "0.0.0"
edition = "2024"
[workspace]
[dependencies]
velocity-store = { path = "$store" }
velocity-new-project = { path = "$project" }
"@ | Set-Content -LiteralPath (Join-Path $fixture 'Cargo.toml')
@'
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
'@ | Set-Content -LiteralPath (Join-Path $fixture 'src/main.rs')
$env:CARGO_TARGET_DIR = Join-Path $root 'target'
& (Join-Path $root 'scripts/build-queue.ps1') cargo run --manifest-path (Join-Path $fixture 'Cargo.toml') -- (Join-Path $fixture 'state')
exit $LASTEXITCODE

