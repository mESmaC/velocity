//! Builds the `stress-harness` binary's modules (#499). Each Rust harness, a `stress/*.rs`
//! script or a source in `stress/harness/sources/` that a `.ps1` script runs, is copied
//! unchanged into its own module file with an entry function appended. A module file keeps
//! the harness's inner attributes and `//!` docs valid, and the copy keeps `cargo fmt` off
//! harness sources, which stay as their authors wrote them. Without the `harnesses`
//! feature this script does nothing, so the workspace build never compiles harness code.

mod spliced;

use std::{
    collections::BTreeMap,
    env,
    error::Error,
    fs,
    path::{Path, PathBuf},
};

type Outcome<T = ()> = Result<T, Box<dyn Error>>;

/// A harness's source, or why it could not be produced. A harness that cannot be produced
/// is built as a stub that prints the reason and fails, so the other harnesses still run.
struct Harness {
    source: Result<String, String>,
    /// Files written beside the module, which the harness includes by relative path.
    companions: Vec<(&'static str, String)>,
}

fn main() -> Outcome {
    println!("cargo::rerun-if-changed=harness/build.rs");
    println!("cargo::rerun-if-changed=harness/spliced.rs");
    if env::var_os("CARGO_FEATURE_HARNESSES").is_none() {
        return Ok(());
    }
    let crate_folder = PathBuf::from(env::var("CARGO_MANIFEST_DIR")?);
    let root = crate_folder
        .parent()
        .ok_or("The stress crate has no parent folder")?
        .to_path_buf();
    // The whole folder, so adding or removing a harness regenerates the binary's table.
    println!("cargo::rerun-if-changed={}", crate_folder.display());
    let harnesses = collect(&crate_folder, &root)?;
    let output = PathBuf::from(env::var("OUT_DIR")?).join("harness");
    write(&output, &harnesses)
}

/// Every harness by name: the stem of its source file, which is the stem of the `.rs`
/// script or of the `.ps1` script that runs it.
fn collect(crate_folder: &Path, root: &Path) -> Outcome<BTreeMap<String, Harness>> {
    let mut harnesses = BTreeMap::new();
    for folder in [
        crate_folder.to_path_buf(),
        crate_folder.join("harness/sources"),
    ] {
        for (name, path) in rust_files(&folder)? {
            let harness = Harness {
                source: Ok(fs::read_to_string(&path)?),
                companions: Vec::new(),
            };
            add(&mut harnesses, name, harness)?;
        }
    }
    let native = spliced::native_functions(root);
    if let Some(harness) = harnesses.get_mut("pr14-deleted-setup") {
        match native {
            Ok(items) => harness.companions.push(("native_functions.rs", items)),
            Err(reason) => harness.source = Err(reason),
        }
    }
    let goal_stack_crash = Harness {
        source: spliced::goal_stack_crash(root),
        companions: Vec::new(),
    };
    add(
        &mut harnesses,
        "pr4-goal-stack-crash".into(),
        goal_stack_crash,
    )?;
    for spliced_input in [
        "src-tauri/src/threads.rs",
        "crates/velocity-threads/examples/rosen_goal_swarm.rs",
    ] {
        println!(
            "cargo::rerun-if-changed={}",
            root.join(spliced_input).display()
        );
    }
    Ok(harnesses)
}

fn add(harnesses: &mut BTreeMap<String, Harness>, name: String, harness: Harness) -> Outcome {
    let module = module_name(&name);
    if let Some(other) = harnesses.keys().find(|other| module_name(other) == module) {
        return Err(format!("Stress harnesses {other} and {name} need distinct names").into());
    }
    harnesses.insert(name, harness);
    Ok(())
}

fn rust_files(folder: &Path) -> Outcome<Vec<(String, PathBuf)>> {
    let mut files = Vec::new();
    for entry in fs::read_dir(folder)? {
        let path = entry?.path();
        if path.is_file() && path.extension().is_some_and(|extension| extension == "rs") {
            let name = path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .ok_or_else(|| format!("{} has no UTF-8 name", path.display()))?
                .to_owned();
            files.push((name, path));
        }
    }
    Ok(files)
}

/// A Rust identifier for a harness name such as `pr43-queued-startup`.
fn module_name(name: &str) -> String {
    let characters: String = name
        .chars()
        .map(|character| match character {
            'a'..='z' | '0'..='9' => character,
            'A'..='Z' => character.to_ascii_lowercase(),
            _ => '_',
        })
        .collect();
    format!("harness_{characters}")
}

/// Writes one folder per harness and `harnesses.rs`, which `harness/main.rs` includes.
fn write(output: &Path, harnesses: &BTreeMap<String, Harness>) -> Outcome {
    let mut declarations = String::new();
    let mut table = String::new();
    for (name, harness) in harnesses {
        let module = module_name(name);
        let folder = output.join(&module);
        fs::create_dir_all(&folder)?;
        let file = folder.join("harness.rs");
        fs::write(&file, module_source(name, harness))?;
        for (companion, contents) in &harness.companions {
            fs::write(folder.join(companion), contents)?;
        }
        let path = file
            .to_str()
            .ok_or_else(|| format!("{} is not UTF-8", file.display()))?;
        declarations.push_str(&format!("#[path = {path:?}]\nmod {module};\n"));
        table.push_str(&format!(
            "    ({name:?}, {module}::stress_harness_entry),\n"
        ));
    }
    let generated = format!(
        "{declarations}\n/// Every harness by name, the stem of its source file.\nconst HARNESSES: &[(&str, fn() -> std::process::ExitCode)] = &[\n{table}];\n"
    );
    fs::write(output.join("harnesses.rs"), generated)?;
    Ok(())
}

/// The harness's source unchanged, with an entry that runs its own `main`, or a stub.
fn module_source(name: &str, harness: &Harness) -> String {
    match &harness.source {
        Ok(source) => format!(
            "{source}\n\n/// Runs this harness's `main` for the stress-harness binary (generated, #499).\npub(crate) fn stress_harness_entry() -> ::std::process::ExitCode {{\n    ::std::process::Termination::report(main())\n}}\n"
        ),
        Err(reason) => format!(
            "/// {name} could not be built from the code it tests (generated, #499).\npub(crate) fn stress_harness_entry() -> ::std::process::ExitCode {{\n    eprintln!(\"{{}}\", {reason:?});\n    ::std::process::ExitCode::FAILURE\n}}\n"
        ),
    }
}
