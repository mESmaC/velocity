//! Provisioning entry point. No Tauri window or OAuth login is opened on the worker.
use std::collections::BTreeMap;
use std::path::PathBuf;

pub fn serve() -> anyhow::Result<bool> {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    if arguments.first().map(String::as_str) != Some("machine") {
        return Ok(false);
    }
    let action = arguments.get(1).map(String::as_str).unwrap_or("");
    anyhow::ensure!(
        matches!(action, "join" | "run"),
        "Use machine join --key <key> --data-dir <folder> [--name <name>] [--slots 40], or machine run --data-dir <folder> [--slots 40]"
    );
    let options = options(&arguments[2..])?;
    let directory = PathBuf::from(
        options
            .get("--data-dir")
            .ok_or_else(|| anyhow::anyhow!("--data-dir is required"))?,
    );
    let slots = options
        .get("--slots")
        .map(|value| value.parse::<usize>())
        .transpose()?
        .unwrap_or(40);
    anyhow::ensure!(slots > 0, "--slots must be positive");
    let name = options.get("--name").cloned().unwrap_or_else(|| {
        std::env::var("COMPUTERNAME")
            .or_else(|_| std::env::var("HOSTNAME"))
            .unwrap_or_else(|_| "machine".into())
    });
    let key = if action == "join" {
        Some(
            options
                .get("--key")
                .ok_or_else(|| anyhow::anyhow!("machine join needs --key"))?
                .clone(),
        )
    } else {
        anyhow::ensure!(
            !options.contains_key("--key"),
            "machine run uses its saved credential; omit --key"
        );
        None
    };
    run(directory, slots, key, name)?;
    Ok(true)
}

fn run(directory: PathBuf, slots: usize, key: Option<String>, name: String) -> anyhow::Result<()> {
    // Resolve before spawning the local cache so its upstream reference has one stable
    // credential-store target even when provisioning uses a relative Windows path.
    let directory = velocity_store::extended_windows_path(&std::path::absolute(directory)?);
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(async move {
            if let Some(key) = key {
                velocity_threads::machine_worker::enroll(&directory, &key, &name)
                    .await
                    .map_err(anyhow::Error::msg)?;
            }
            velocity_threads::machine_worker::run(directory, slots)
                .await
                .map_err(anyhow::Error::msg)
        })?;
    Ok(())
}

fn options(arguments: &[String]) -> anyhow::Result<BTreeMap<String, String>> {
    anyhow::ensure!(
        arguments.len().is_multiple_of(2),
        "Every machine option needs a value"
    );
    let mut options = BTreeMap::new();
    for pair in arguments.chunks_exact(2) {
        anyhow::ensure!(
            matches!(
                pair[0].as_str(),
                "--key" | "--name" | "--data-dir" | "--slots"
            ),
            "Unknown machine option: {}",
            pair[0]
        );
        anyhow::ensure!(!pair[1].trim().is_empty(), "{} needs a value", pair[0]);
        anyhow::ensure!(
            options.insert(pair[0].clone(), pair[1].clone()).is_none(),
            "Duplicate machine option: {}",
            pair[0]
        );
    }
    Ok(options)
}
