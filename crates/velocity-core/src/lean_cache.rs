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
    let mut project_dir = None;
    let LeadingScan::Positional(subcommand_index) =
        scan_leading_options(arguments, 0, &mut project_dir)
    else {
        return LakeInvocation {
            project_dir,
            needs_cache: false,
        };
    };
    let Some(subcommand) = arguments.get(subcommand_index) else {
        return LakeInvocation {
            project_dir,
            needs_cache: false,
        };
    };
    let after_subcommand = scan_leading_options(arguments, subcommand_index + 1, &mut project_dir);
    let needs_cache = match after_subcommand {
        LeadingScan::Stopped => false,
        LeadingScan::EndOfOptions => {
            subcommand == "env" || subcommand_parses_trailing_options(subcommand)
        }
        LeadingScan::Positional(rest_index) => match subcommand.as_str() {
            // `lake exe cache ...` IS the cache command; never run it recursively.
            "exe" | "exec" => arguments
                .get(rest_index)
                .is_some_and(|executable| executable != "cache"),
            "env" => true,
            name if subcommand_parses_trailing_options(name) => {
                scan_subcommand_options(arguments, rest_index, &mut project_dir)
            }
            _ => false,
        },
    };
    LakeInvocation {
        project_dir,
        needs_cache,
    }
}

enum LeadingScan {
    Stopped,
    EndOfOptions,
    Positional(usize),
}

enum OptionOutcome {
    Continue(usize),
    EndOfOptions,
    Stop,
}

enum OptionKind {
    Directory,
    TakesValue,
    Flag,
    Info,
}

fn is_option_token(argument: &str) -> bool {
    argument.starts_with('-') && argument.len() > 1
}

/// Mirrors Lake's `processLeadingOptions lakeOption` before and after the subcommand name.
fn scan_leading_options(
    arguments: &[String],
    start: usize,
    project_dir: &mut Option<String>,
) -> LeadingScan {
    let mut index = start;
    while let Some(argument) = arguments.get(index) {
        if argument.is_empty() {
            index += 1;
            continue;
        }
        if !is_option_token(argument) {
            return LeadingScan::Positional(index);
        }
        match parse_option(arguments, index, project_dir) {
            OptionOutcome::Continue(next) => index = next,
            OptionOutcome::EndOfOptions => return LeadingScan::EndOfOptions,
            OptionOutcome::Stop => return LeadingScan::Stopped,
        }
    }
    LeadingScan::EndOfOptions
}

/// Mirrors Lake's `processOptions lakeOption` for build/elaboration subcommands that accept
/// global options anywhere before `--` (for example `lake build Probe -d=proofs`).
fn scan_subcommand_options(
    arguments: &[String],
    start: usize,
    project_dir: &mut Option<String>,
) -> bool {
    let mut index = start;
    while let Some(argument) = arguments.get(index) {
        if argument.is_empty() || !is_option_token(argument) {
            index += 1;
            continue;
        }
        match parse_option(arguments, index, project_dir) {
            OptionOutcome::Continue(next) => index = next,
            OptionOutcome::EndOfOptions => return true,
            OptionOutcome::Stop => return false,
        }
    }
    true
}

fn parse_option(
    arguments: &[String],
    index: usize,
    project_dir: &mut Option<String>,
) -> OptionOutcome {
    let Some(argument) = arguments.get(index) else {
        return OptionOutcome::Stop;
    };
    if argument == "--" {
        return OptionOutcome::EndOfOptions;
    }
    if argument.starts_with("--") {
        parse_long_option(arguments, index, project_dir)
    } else {
        parse_short_option(arguments, index, project_dir)
    }
}

fn parse_long_option(
    arguments: &[String],
    index: usize,
    project_dir: &mut Option<String>,
) -> OptionOutcome {
    let Some(argument) = arguments.get(index) else {
        return OptionOutcome::Stop;
    };
    let (name, inline_value) = split_long_option(argument);
    let Some(kind) = classify_long_option(name) else {
        return OptionOutcome::Stop;
    };
    apply_option_kind(kind, inline_value, arguments, index, project_dir)
}

fn parse_short_option(
    arguments: &[String],
    index: usize,
    project_dir: &mut Option<String>,
) -> OptionOutcome {
    let Some(argument) = arguments.get(index) else {
        return OptionOutcome::Stop;
    };
    let Some(rest) = argument.strip_prefix('-') else {
        return OptionOutcome::Stop;
    };
    let mut characters = rest.chars();
    let Some(flag) = characters.next() else {
        return OptionOutcome::Stop;
    };
    // Lake's short option parser accepts `-x arg`, `-x=arg`, `"-x arg"` and `-xarg`.
    let suffix = characters.as_str();
    let inline_value = if suffix.is_empty() {
        None
    } else if let Some(value) = suffix.strip_prefix('=') {
        Some(value)
    } else if suffix.starts_with(' ') {
        Some(suffix.trim_start_matches(|character: char| character.is_ascii_whitespace()))
    } else {
        Some(suffix)
    };
    let Some(kind) = classify_short_option(flag) else {
        return OptionOutcome::Stop;
    };
    apply_option_kind(kind, inline_value, arguments, index, project_dir)
}

fn apply_option_kind(
    kind: OptionKind,
    inline_value: Option<&str>,
    arguments: &[String],
    index: usize,
    project_dir: &mut Option<String>,
) -> OptionOutcome {
    match kind {
        OptionKind::Info => OptionOutcome::Stop,
        OptionKind::Flag => {
            if inline_value.is_some() {
                OptionOutcome::Stop
            } else {
                OptionOutcome::Continue(index + 1)
            }
        }
        OptionKind::Directory => {
            let Some((value, next_index)) = option_value(inline_value, arguments, index) else {
                return OptionOutcome::Stop;
            };
            *project_dir = Some(value.to_owned());
            OptionOutcome::Continue(next_index)
        }
        OptionKind::TakesValue => {
            let Some((_, next_index)) = option_value(inline_value, arguments, index) else {
                return OptionOutcome::Stop;
            };
            OptionOutcome::Continue(next_index)
        }
    }
}

fn option_value<'a>(
    inline_value: Option<&'a str>,
    arguments: &'a [String],
    index: usize,
) -> Option<(&'a str, usize)> {
    if let Some(value) = inline_value {
        return Some((value, index + 1));
    }
    arguments
        .get(index + 1)
        .map(|value| (value.as_str(), index + 2))
}

fn split_long_option(argument: &str) -> (&str, Option<&str>) {
    if let Some((name, value)) = argument.split_once('=') {
        return (name, Some(value));
    }
    if let Some((name, value)) = argument.split_once(' ') {
        return (name, Some(value));
    }
    (argument, None)
}

fn classify_short_option(flag: char) -> Option<OptionKind> {
    match flag {
        'd' => Some(OptionKind::Directory),
        'f' | 'o' | 'K' => Some(OptionKind::TakesValue),
        'q' | 'v' | 'U' | 'R' | 'H' | 'J' | 'w' => Some(OptionKind::Flag),
        'h' | 'V' => Some(OptionKind::Info),
        _ => None,
    }
}

const LONG_VALUE_OPTIONS: &[&str] = &[
    "--file",
    "--packages",
    "--log-level",
    "--fail-level",
    "--package",
    "--service",
    "--scope",
    "--repo",
    "--platform",
    "--toolchain",
    "--rev",
    "--max-revs",
    "--linters",
    "--lint-only",
    "--checks",
    "--only",
    "--config",
    "--from-export",
    "--challenge-from-export",
    "--solution-from-export",
];

const LONG_FLAG_OPTIONS: &[&str] = &[
    "--quiet",
    "--verbose",
    "--update",
    "--keep-toolchain",
    "--reconfigure",
    "--old",
    "--text",
    "--json",
    "--allow-empty",
    "--no-build",
    "--fail-fast",
    "--no-cache",
    "--try-cache",
    "--rehash",
    "--offline",
    "--wfail",
    "--iofail",
    "--no-overwrite",
    "--force-overwrite",
    "--force-download",
    "--download-arts",
    "--mappings-only",
    "--ansi",
    "--no-ansi",
    "--builtin-lint",
    "--builtin-only",
    "--record-exceptions",
    "--code-quality",
    "--force",
    "--keep-implied",
    "--keep-prefix",
    "--keep-public",
    "--add-public",
    "--gh-style",
    "--explain",
    "--trace",
    "--fix",
    "--paranoid",
    "--inadvisably-no-sandbox",
];

fn classify_long_option(name: &str) -> Option<OptionKind> {
    match name {
        "--dir" => Some(OptionKind::Directory),
        "--help" | "--version" => Some(OptionKind::Info),
        _ if LONG_VALUE_OPTIONS.contains(&name) => Some(OptionKind::TakesValue),
        _ if LONG_FLAG_OPTIONS.contains(&name) => Some(OptionKind::Flag),
        _ => None,
    }
}

fn subcommand_parses_trailing_options(subcommand: &str) -> bool {
    matches!(
        subcommand,
        "build"
            | "lean"
            | "test"
            | "lint"
            | "check-build"
            | "check-test"
            | "check-lint"
            | "setup-file"
            | "query"
            | "query-kind"
            | "pack"
            | "upload"
    )
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

    // Risk (#675): Lake's valid global option forms (`-d=proofs`, `-dproofs`, `--try-cache`,
    // `--rehash`, `--fail-level=...`, options after the subcommand) must keep the project
    // directory and still trigger on-demand Mathlib cache preparation instead of being
    // misclassified as unknown subcommands that skip `lake exe cache get`.
    #[test]
    fn lake_global_option_forms_preserve_project_and_cache_requirement() {
        for arguments in [
            &["-d=proofs", "--no-build", "build"][..],
            &["--try-cache", "-d", "proofs", "--no-build", "build"][..],
            &["-dproofs", "--no-build", "build"][..],
            &["-d proofs", "--no-build", "build"][..],
            &["--dir proofs", "--no-build", "build"][..],
            &["build", "-d=proofs", "--no-build"][..],
            &["build", "Probe", "-d=proofs", "--no-build"][..],
            &[
                "--rehash",
                "-H",
                "--fail-fast",
                "--allow-empty",
                "--fail-level=warning",
                "-Kfoo=bar",
                "-f=lakefile.toml",
                "-o=out.json",
                "-d=proofs",
                "build",
            ][..],
            &["exe", "-d=proofs", "export-fixtures"][..],
            &["exec", "-d=proofs", "export-fixtures"][..],
        ] {
            assert_eq!(
                inspect_lake_invocation(&strings(arguments)),
                LakeInvocation {
                    project_dir: Some("proofs".into()),
                    needs_cache: true,
                },
                "expected proofs build requiring cache for {arguments:?}"
            );
        }
        assert_eq!(
            inspect_lake_invocation(&strings(&["exe", "my-tool", "-d=other"])),
            LakeInvocation {
                project_dir: None,
                needs_cache: true,
            }
        );
        for skipped in [
            &["exe", "-d", "proofs", "cache", "get"][..],
            &["-d=proofs", "exe", "cache", "get"][..],
            &["-d=proofs", "build", "-h"][..],
            &["-h", "build"][..],
            &["-qv", "-d", "proofs", "build"][..],
            &["--", "build"][..],
            &["build", "-d"][..],
        ] {
            assert!(
                !inspect_lake_invocation(&strings(skipped)).needs_cache,
                "expected skip for {skipped:?}"
            );
        }
    }
}
