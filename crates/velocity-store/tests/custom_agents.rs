use velocity_store::{ProviderKind, StateStore};

// Custom agents and edited commands live only in state.json: losing them on a save and load,
// or failing to read a state.json saved before they existed, would drop your providers or
// refuse to start the app.
#[test]
fn custom_agents_and_commands_survive_save_and_load() -> Result<(), Box<dyn std::error::Error>> {
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_nanos();
    let directory =
        std::env::temp_dir().join(format!("velocity-agents-{}-{unique}", std::process::id()));
    std::fs::create_dir(&directory)?;
    // A state.json from before custom agents: providers saved by their old names.
    let mut saved = serde_json::to_value(velocity_store::PersistedState::default())?;
    let providers = &mut saved["settings"]["providers"];
    for field in ["commands", "custom_agents", "next_custom_agent"] {
        providers
            .as_object_mut()
            .ok_or("providers is not an object")?
            .remove(field);
    }
    providers["signed_out"] = serde_json::json!(["kimi"]);
    std::fs::write(directory.join("state.json"), serde_json::to_vec(&saved)?)?;

    let mut store = StateStore::load_or_default(&directory)?;
    assert_eq!(store.settings().providers.signed_out, [ProviderKind::Kimi]);
    let added =
        store.add_custom_agent("Gemini", r#""C:\Program Files\Gemini\gemini.exe" --acp"#)?;
    store.set_provider_command(ProviderKind::Codex, Some("codex-acp-dev --verbose"))?;
    store.set_provider_signed_out(added, true)?;
    drop(store);

    let loaded = StateStore::load_or_default(&directory)?.settings();
    assert_eq!(loaded.provider_name(added), "Gemini");
    let command = loaded.agent_command(added).ok_or("custom command lost")?;
    assert_eq!(command.program, r"C:\Program Files\Gemini\gemini.exe");
    assert_eq!(command.arguments, ["--acp"]);
    let codex = loaded
        .agent_command(ProviderKind::Codex)
        .ok_or("edited command lost")?;
    assert_eq!(codex.display(), "codex-acp-dev --verbose");
    assert!(loaded.providers.signed_out.contains(&added));
    std::fs::remove_dir_all(directory)?;
    Ok(())
}
