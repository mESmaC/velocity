//! Pure decisions for on-demand Lean / Mathlib cache setup (#546).
//!
//! Workspaces that never touch `proofs/` carry no `proofs/.lake`. The first time a workspace
//! invokes `lake` for a build or elaboration command, Velocity checks whether Mathlib's
//! prebuilt cache is already unpacked for the pinned revision and fetches it with
//! `lake exe cache get` before running the command, so Mathlib is never compiled from source.

/// Stamp written inside `.lake/` after `lake exe cache get` completes for a Mathlib revision.
pub const MATHLIB_CACHE_STAMP: &str = ".lake/.velocity-mathlib-rev";

/// Prebuilt Mathlib top-level `.olean` path relative to the project's packages directory.
pub const MATHLIB_OLEAN_RELATIVE: &str = "mathlib/.lake/build/lib/lean/Mathlib.olean";

/// Lake's default packages directory when `lake-manifest.json` omits `packagesDir`.
pub const DEFAULT_PACKAGES_DIR: &str = ".lake/packages";

/// What an intercepted `lake` command needs before running.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LakeInvocation {
    /// Directory passed via `-d` or `--dir`, if any (relative to the command's working folder).
    pub project_dir: Option<String>,
    /// Whether this invocation builds or elaborates Lean modules and therefore needs Mathlib's
    /// prebuilt cache unpacked first.
    pub needs_cache: bool,
}

/// Whether `program` names the `lake` executable (with or without path or Windows suffix).
pub fn is_lake_program(program: &str) -> bool {
    let name = program.rsplit(['/', '\\']).next().unwrap_or_default();
    let lower = name.to_ascii_lowercase();
    let stem = [".exe", ".cmd", ".bat"]
        .into_iter()
        .find_map(|suffix| lower.strip_suffix(suffix))
        .unwrap_or(&lower);
    stem == "lake"
}

/// Whether `folder` is a safe relative path inside a Lake project: no root, drive or `..`.
pub fn is_relative_folder(folder: &str) -> bool {
    !folder.is_empty()
        && !folder.starts_with(['/', '\\'])
        && !folder.contains(':')
        && folder
            .split(['/', '\\'])
            .all(|step| !step.is_empty() && step != "..")
}

/// Whether `rev` looks like a Git commit hash pinned in `lake-manifest.json`.
pub fn is_commit_hash(rev: &str) -> bool {
    (7..=64).contains(&rev.len()) && rev.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// Whether Mathlib's prebuilt cache is already unpacked for `expected_rev`.
/// Requiring both `Mathlib.olean` and the completion stamp prevents an interrupted unpack or
/// a manifest bump from skipping `lake exe cache get`.
pub fn mathlib_cache_current(
    stamped_rev: Option<&str>,
    expected_rev: &str,
    olean_exists: bool,
) -> bool {
    olean_exists && stamped_rev.is_some_and(|value| value.trim() == expected_rev)
}

/// Classifies a `lake` CLI invocation: extracts any `-d`/`--dir` project override and decides
/// whether Mathlib's cache should be prepared before running it.
pub fn inspect_lake_invocation(arguments: &[String]) -> LakeInvocation {
    let (project_dir, subcommand_index) = scan_global_options(arguments);
    let Some(index) = subcommand_index else {
        return LakeInvocation {
            project_dir,
            needs_cache: false,
        };
    };
    let Some(subcommand) = arguments.get(index) else {
        return LakeInvocation {
            project_dir,
            needs_cache: false,
        };
    };
    let tail = &arguments[index + 1..];
    let asks_info = tail
        .iter()
        .take_while(|argument| argument.as_str() != "--")
        .any(|argument| is_info_flag(argument));
    LakeInvocation {
        project_dir,
        needs_cache: !asks_info && subcommand_needs_cache(subcommand, tail),
    }
}

fn scan_global_options(arguments: &[String]) -> (Option<String>, Option<usize>) {
    let mut project_dir = None;
    let mut index = 0;
    while let Some(argument) = arguments.get(index) {
        if argument == "--" || is_info_flag(argument) {
            return (project_dir, None);
        }
        if matches!(argument.as_str(), "-d" | "--dir") {
            let Some(value) = arguments.get(index + 1) else {
                return (project_dir, None);
            };
            project_dir = Some(value.clone());
            index += 2;
            continue;
        }
        if let Some(value) = argument.strip_prefix("--dir=") {
            project_dir = Some(value.to_owned());
            index += 1;
            continue;
        }
        if let Some(step) = global_option_step(argument) {
            index += step;
            continue;
        }
        return (project_dir, Some(index));
    }
    (project_dir, None)
}

fn is_info_flag(argument: &str) -> bool {
    matches!(argument, "-h" | "--help" | "-V" | "--version")
}

fn global_option_step(argument: &str) -> Option<usize> {
    if matches!(
        argument,
        "-f" | "--file" | "-K" | "--packages" | "--log-level"
    ) {
        return Some(2);
    }
    if argument.starts_with("--file=")
        || argument.starts_with("--packages=")
        || argument.starts_with("--log-level=")
        || matches!(
            argument,
            "-R" | "--reconfigure"
                | "--old"
                | "--no-build"
                | "--no-cache"
                | "-q"
                | "--quiet"
                | "-v"
                | "--verbose"
                | "--ansi"
                | "--no-ansi"
                | "--offline"
                | "-w"
                | "--wfail"
                | "--keep-toolchain"
        )
    {
        return Some(1);
    }
    None
}

fn subcommand_needs_cache(subcommand: &str, tail: &[String]) -> bool {
    match subcommand {
        "build" | "lean" | "env" | "test" | "lint" | "check-build" | "check-test"
        | "check-lint" | "setup-file" | "query" | "query-kind" | "pack" | "upload" => true,
        // `lake exe cache ...` IS the cache command; never run it recursively.
        "exe" => tail
            .iter()
            .find(|argument| !argument.starts_with('-'))
            .is_some_and(|executable| executable != "cache"),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::{LakeInvocation, inspect_lake_invocation, is_lake_program, mathlib_cache_current};

    fn strings(items: &[&str]) -> Vec<String> {
        items.iter().map(|item| (*item).to_owned()).collect()
    }

    // Risk (#546): on-demand Lean setup must trigger for build/elaboration commands (including
    // with `-d`/`--dir` flags) and must skip cache/help/version/clean commands so it never
    // loops recursively or unpacks 7 GB for a metadata probe.
    #[test]
    fn lake_invocation_classification_matches_build_and_skip_cases() {
        assert_eq!(
            inspect_lake_invocation(&strings(&["build"])),
            LakeInvocation {
                project_dir: None,
                needs_cache: true,
            }
        );
        assert_eq!(
            inspect_lake_invocation(&strings(&["-d", "proofs", "build", "Velocity"])),
            LakeInvocation {
                project_dir: Some("proofs".into()),
                needs_cache: true,
            }
        );
        assert_eq!(
            inspect_lake_invocation(&strings(&[
                "--dir=proofs",
                "--no-cache",
                "lean",
                "Foo.lean"
            ])),
            LakeInvocation {
                project_dir: Some("proofs".into()),
                needs_cache: true,
            }
        );
        assert_eq!(
            inspect_lake_invocation(&strings(&["exe", "export-fixtures"])),
            LakeInvocation {
                project_dir: None,
                needs_cache: true,
            }
        );
        for skipped in [
            &["exe", "cache", "get"][..],
            &["--version"][..],
            &["build", "--help"][..],
            &["clean"][..],
            &["update"][..],
            &[][..],
        ] {
            assert!(
                !inspect_lake_invocation(&strings(skipped)).needs_cache,
                "expected skip for {skipped:?}"
            );
        }
        assert!(is_lake_program("lake"));
        assert!(is_lake_program(r"C:\Users\dev\.elan\bin\lake.exe"));
        assert!(!is_lake_program("cargo"));
        assert!(mathlib_cache_current(Some("abc1234\n"), "abc1234", true));
        assert!(!mathlib_cache_current(Some("abc1234"), "abc1234", false));
        assert!(!mathlib_cache_current(Some("oldrev0"), "abc1234", true));
        assert!(!mathlib_cache_current(None, "abc1234", true));
    }
}
