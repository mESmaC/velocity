//! Machine requirements are stable for the app session, independent of project choices.
use std::sync::OnceLock;

use crate::SkillPackCatalogEntry;

// Lazy, separate cells avoid launching unused runtimes and serialize concurrent reads.
static PYTHON: OnceLock<String> = OnceLock::new();
static NODE: OnceLock<String> = OnceLock::new();
static OPTIONAL_TOOLS: OnceLock<Vec<String>> = OnceLock::new();

pub(crate) fn requirements(catalog: &SkillPackCatalogEntry) -> Vec<String> {
    let mut result = Vec::new();
    if catalog.requires_python {
        result.push(
            PYTHON
                .get_or_init(|| probe("python", "Python 3", 3))
                .clone(),
        );
    }
    if catalog.requires_node {
        result.push(NODE.get_or_init(|| probe("node", "Node 22+", 22)).clone());
        result.extend(OPTIONAL_TOOLS.get_or_init(optional_tools).iter().cloned());
    }
    if result.is_empty() {
        result.push("No required runtime".into());
    }
    result
}

fn optional_tools() -> Vec<String> {
    let folders: Vec<_> = std::env::var_os("PATH")
        .map(|paths| std::env::split_paths(&paths).collect())
        .unwrap_or_default();
    let extensions = launcher_extensions();
    [
        ("ghidra", &["ghidraRun", "ghidra"][..]),
        ("hopper", &["hopper", "Hopper"]),
        ("ida", &["idat64", "ida64", "idat", "ida"]),
        ("rizin", &["rizin"]),
        ("objdump", &["objdump"]),
    ]
    .into_iter()
    .map(|(label, launchers)| {
        let found = folders.iter().any(|folder| {
            launchers.iter().any(|launcher| {
                folder.join(launcher).is_file()
                    || extensions
                        .iter()
                        .any(|extension| folder.join(format!("{launcher}{extension}")).is_file())
            })
        });
        let status = if found { "found" } else { "not found" };
        format!("{label} optional \u{00b7} {status}")
    })
    .collect()
}

fn launcher_extensions() -> Vec<String> {
    if !cfg!(windows) {
        return Vec::new();
    }
    std::env::var("PATHEXT")
        .unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".into())
        .split(';')
        .map(str::trim)
        .filter(|extension| extension.starts_with('.') && extension.len() > 1)
        .map(str::to_owned)
        .collect()
}

fn probe(program: &str, label: &str, minimum: u32) -> String {
    let version = command_version(program);
    let found = version
        .as_deref()
        .and_then(|text| {
            text.trim_start_matches(|character: char| !character.is_ascii_digit())
                .split('.')
                .next()?
                .parse::<u32>()
                .ok()
        })
        .is_some_and(|major| major >= minimum);
    format!(
        "{} {label} \u{00b7} {}",
        if found { "\u{2713}" } else { "\u{00b7}" },
        version.as_deref().unwrap_or("not found")
    )
}
fn command_version(program: &str) -> Option<String> {
    let mut command = std::process::Command::new(program);
    command.arg("--version");
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    let output = command.output().ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
}
