use std::{
    error::Error,
    fs,
    path::{Path, PathBuf},
    process::Command,
};

type Result<T> = std::result::Result<T, Box<dyn Error>>;

pub fn run(root: &Path, entries: &[PathBuf]) -> bool {
    let mut failures = Vec::new();
    let mut tests = Command::new("cargo");
    tests.args(["test", "--workspace", "--locked"]);
    let mut assets = Command::new("pwsh");
    assets.args([
        "-NoProfile",
        "-Command",
        "& pnpm -C ui build; exit $LASTEXITCODE",
    ]);
    for (label, mut command) in [("Workspace tests", tests), ("Production UI assets", assets)] {
        println!("\n==> {label}");
        let result = status(root, &mut command);
        if !matches!(result, Ok(true)) {
            eprintln!("FAIL: {label}: {result:?}");
            failures.push(label.to_owned());
        }
    }
    for entry in entries {
        let label = entry
            .strip_prefix(root)
            .unwrap_or(entry)
            .display()
            .to_string();
        println!("\n==> {label}");
        match execute(root, entry) {
            Ok(true) => println!("PASS: {label}"),
            result => {
                eprintln!("FAIL: {label}: {result:?}");
                failures.push(label);
            }
        }
    }
    println!(
        "\n{} passed; {} failed",
        entries.len() + 2 - failures.len(),
        failures.len()
    );
    for failure in &failures {
        eprintln!("  {failure}");
    }
    failures.is_empty()
}

fn execute(root: &Path, entry: &Path) -> Result<bool> {
    let mut command = match entry.extension().and_then(|value| value.to_str()) {
        Some("ps1") => {
            let mut command = Command::new("pwsh");
            command.args(["-NoProfile", "-File"]).arg(entry);
            command
        }
        Some("py") => {
            let mut command = Command::new("python");
            command.arg(entry);
            command
        }
        _ => {
            let manifest = if entry.extension().is_some_and(|value| value == "rs") {
                rust_manifest(root, entry)?
            } else {
                entry.to_path_buf()
            };
            let mut command = Command::new("cargo");
            command.args(["run", "--manifest-path"]).arg(manifest);
            command
        }
    };
    status(root, &mut command)
}

fn status(root: &Path, command: &mut Command) -> Result<bool> {
    command.current_dir(root).env("PYTHONUTF8", "1");
    // Vite invokes `net use`; keep Java's net.dll from shadowing Windows net.exe
    // when the aggregate command is run independently of check.ps1 (#44).
    if let (Some(system_root), Some(path)) =
        (std::env::var_os("SystemRoot"), std::env::var_os("PATH"))
    {
        let paths = std::iter::once(PathBuf::from(system_root).join("System32"))
            .chain(std::env::split_paths(&path));
        command.env("PATH", std::env::join_paths(paths)?);
    }
    if std::env::var_os("CARGO_TARGET_DIR").is_none() {
        command.env("CARGO_TARGET_DIR", root.join("target"));
    }
    let status = command.status()?;
    println!("Exit status: {status}");
    Ok(status.success())
}

fn rust_manifest(root: &Path, entry: &Path) -> Result<PathBuf> {
    let name = entry
        .file_stem()
        .ok_or("Rust evidence has no name")?
        .to_string_lossy();
    let directory = root.join(".tools/stress").join(name.as_ref());
    fs::create_dir_all(&directory)?;
    let source = fs::read_to_string(entry)?;
    let mut manifest = format!(
        "[package]\nname = {name:?}\nversion = \"0.0.0\"\nedition = \"2024\"\n[workspace]\n[[bin]]\nname = {name:?}\npath = {:?}\n[dependencies]\n",
        entry.to_string_lossy().replace('\\', "/")
    );
    // Standalone evidence was written against these real libraries. Refer to its
    // original file so its bytes and source-relative paths stay intact.
    for crate_entry in fs::read_dir(root.join("crates"))? {
        let path = crate_entry?.path();
        let name = path
            .file_name()
            .ok_or("Crate has no name")?
            .to_string_lossy();
        if source.contains(&name.replace('-', "_")) {
            manifest.push_str(&format!(
                "{name} = {{ path = {:?} }}\n",
                path.to_string_lossy().replace('\\', "/")
            ));
        }
    }
    let path = directory.join("Cargo.toml");
    fs::write(&path, manifest)?;
    Ok(path)
}
