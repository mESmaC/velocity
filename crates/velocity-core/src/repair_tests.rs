//! Pure parsing, platform matching, command formatting and commit-gate evaluation for
//! targeted tests required on fix-forward repairs (#741).

use serde::{Deserialize, Serialize};

/// One targeted `cargo test` invocation derived from failing Rust test output (#741).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TargetedTest {
    /// Cargo package (`-p <crate>`), when reported by `to rerun pass`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub package: Option<String>,
    /// Cargo test target flags (for example `["--lib"]`, `["--test", "issue406_build_repair"]`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub target_args: Vec<String>,
    /// Failing test names parsed from `---- <name> stdout ----` and `failures:`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tests: Vec<String>,
}

impl TargetedTest {
    /// Arguments passed to `cargo` (starting with `"test"`).
    pub fn cargo_args(&self) -> Vec<String> {
        let mut args = vec![String::from("test")];
        if let Some(package) = &self.package {
            args.push(String::from("-p"));
            args.push(package.clone());
        }
        args.extend(self.target_args.iter().cloned());
        match self.tests.as_slice() {
            [] => {}
            [single] => args.push(single.clone()),
            many => {
                args.push(String::from("--"));
                args.extend(many.iter().cloned());
            }
        }
        args
    }

    /// Exact `cargo test ...` command string for logs and preflight step labels.
    pub fn cargo_command(&self) -> String {
        format!("cargo {}", self.cargo_args().join(" "))
    }

    /// Exact `rosen run -- cargo test ...` command string shown in repair briefs.
    pub fn rosen_run_command(&self) -> String {
        format!("rosen run -- {}", self.cargo_command())
    }

    /// Stable target identity `(package, target_args)` used for deduplication and keys.
    pub fn target_key(&self) -> String {
        let package = self.package.as_deref().unwrap_or("");
        format!("{package}|{}", self.target_args.join(" "))
    }

    /// Individual required verification keys that must pass on the current snapshot head.
    pub fn required_keys(&self) -> Vec<String> {
        let prefix = self.target_key();
        if self.tests.is_empty() {
            return vec![format!("{prefix}|*")];
        }
        self.tests
            .iter()
            .map(|name| format!("{prefix}|{name}"))
            .collect()
    }
}

/// Persisted attestation that one required test key passed on a specific snapshot head (#741).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PassedTestKey {
    pub key: String,
    pub head: String,
    #[serde(default)]
    pub recorded_at: u64,
}

/// Persisted targeted-test requirements and pass attestations for one Rosen change (#741).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequiredTestsRecord {
    pub change: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tests: Vec<TargetedTest>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub passed: Vec<PassedTestKey>,
}

impl RequiredTestsRecord {
    pub fn new(change: impl Into<String>, tests: Vec<TargetedTest>) -> Self {
        Self {
            change: change.into(),
            tests: merge_targeted_tests(tests),
            passed: Vec::new(),
        }
    }

    /// Targeted tests (narrowed to unpassed test names) that have not passed on `head`.
    pub fn missing_for_head(&self, head: &str) -> Vec<TargetedTest> {
        let mut missing = Vec::new();
        for target in &self.tests {
            let prefix = target.target_key();
            if target.tests.is_empty() {
                let key = format!("{prefix}|*");
                if !self.has_passed_key(&key, head) {
                    missing.push(target.clone());
                }
                continue;
            }
            let remaining: Vec<String> = target
                .tests
                .iter()
                .filter(|name| !self.has_passed_key(&format!("{prefix}|{name}"), head))
                .cloned()
                .collect();
            if !remaining.is_empty() {
                missing.push(TargetedTest {
                    package: target.package.clone(),
                    target_args: target.target_args.clone(),
                    tests: remaining,
                });
            }
        }
        missing
    }

    /// True when every required test has a passing attestation for `head`.
    pub fn all_passed_for_head(&self, head: &str) -> bool {
        self.missing_for_head(head).is_empty()
    }

    /// Whether `arguments` (passed to `cargo`) is an allowed targeted test run for this change.
    pub fn allows_cargo_test(&self, arguments: &[String]) -> bool {
        !self.matched_keys_for_cargo_test(arguments).is_empty()
    }

    /// Required test keys covered by a `cargo` invocation `arguments`.
    pub fn matched_keys_for_cargo_test(&self, arguments: &[String]) -> Vec<String> {
        let Some(parsed) = parse_cargo_test_invocation(arguments) else {
            return Vec::new();
        };
        let mut keys = Vec::new();
        for target in &self.tests {
            if parsed.package != target.package || parsed.target_args != target.target_args {
                continue;
            }
            let prefix = target.target_key();
            if target.tests.is_empty() {
                if parsed.filters.is_empty() {
                    push_unique(&mut keys, format!("{prefix}|*"));
                }
                continue;
            }
            if !parsed.filters.is_empty()
                && parsed
                    .filters
                    .iter()
                    .all(|filter| target.tests.iter().any(|required| required == filter))
            {
                for filter in &parsed.filters {
                    push_unique(&mut keys, format!("{prefix}|{filter}"));
                }
            }
        }
        keys
    }

    /// Records a passing `cargo test` invocation for `head`. Returns true when any key matched.
    pub fn record_pass_for_arguments(
        &mut self,
        head: &str,
        arguments: &[String],
        recorded_at: u64,
    ) -> bool {
        let keys = self.matched_keys_for_cargo_test(arguments);
        if keys.is_empty() {
            return false;
        }
        for key in keys {
            self.upsert_pass(key, head, recorded_at);
        }
        true
    }

    /// Records a passing run of `target` (for example from `preflight`) for `head`.
    pub fn record_pass_for_target(
        &mut self,
        head: &str,
        target: &TargetedTest,
        recorded_at: u64,
    ) -> bool {
        let keys = target.required_keys();
        let allowed: Vec<String> = self
            .tests
            .iter()
            .flat_map(TargetedTest::required_keys)
            .collect();
        let mut recorded = false;
        for key in keys {
            if allowed.iter().any(|candidate| candidate == &key) {
                self.upsert_pass(key, head, recorded_at);
                recorded = true;
            }
        }
        recorded
    }

    fn has_passed_key(&self, key: &str, head: &str) -> bool {
        self.passed
            .iter()
            .any(|entry| entry.key == key && entry.head == head)
    }

    fn upsert_pass(&mut self, key: String, head: &str, recorded_at: u64) {
        if let Some(existing) = self.passed.iter_mut().find(|entry| entry.key == key) {
            existing.head = head.to_owned();
            existing.recorded_at = recorded_at;
        } else {
            self.passed.push(PassedTestKey {
                key,
                head: head.to_owned(),
                recorded_at,
            });
        }
    }
}

/// Parses failing Rust test names (`---- <name> stdout ----`, `failures:` list, and
/// `test <name> ... FAILED`) paired with Cargo's `to rerun pass ...` targets (#741).
pub fn parse_cargo_test_failures(output: &str) -> Vec<TargetedTest> {
    parse_cargo_test_failures_with_hung(output, None)
}

/// Like [`parse_cargo_test_failures`], attaching `hung_test` when a single failing target
/// timed out before printing its `failures:` list (#741).
pub fn parse_cargo_test_failures_with_hung(
    output: &str,
    hung_test: Option<&str>,
) -> Vec<TargetedTest> {
    let mut parser = FailureOutputParser::default();
    for raw_line in output.lines() {
        let clean = strip_ansi(raw_line.trim_end_matches('\r'));
        parser.observe_line(&clean);
    }
    parser.finish(hung_test)
}

/// Merges targeted tests that share the same `(package, target_args)`, preserving order.
pub fn merge_targeted_tests(items: impl IntoIterator<Item = TargetedTest>) -> Vec<TargetedTest> {
    let mut merged: Vec<TargetedTest> = Vec::new();
    for item in items {
        if let Some(existing) = merged
            .iter_mut()
            .find(|entry| entry.package == item.package && entry.target_args == item.target_args)
        {
            if existing.tests.is_empty() || item.tests.is_empty() {
                existing.tests.clear();
            } else {
                for name in item.tests {
                    push_unique(&mut existing.tests, name);
                }
            }
        } else {
            merged.push(item);
        }
    }
    merged
}

/// Whether `cargo test` output shows at least one test actually ran and passed (preventing a
/// deleted or renamed test filter from passing with `0 passed; N filtered out`) (#741).
pub fn cargo_test_output_passed_at_least_one(output: &str) -> bool {
    let mut passed_total = 0u64;
    let mut saw_failure = false;
    for raw_line in output.lines() {
        let clean = strip_ansi(raw_line.trim_end_matches('\r'));
        let trimmed = clean.trim();
        let Some(rest) = trimmed.strip_prefix("test result:") else {
            continue;
        };
        let rest = rest.trim();
        if rest.starts_with("FAILED") {
            saw_failure = true;
            continue;
        }
        let Some(after_ok) = rest.strip_prefix("ok.") else {
            continue;
        };
        if let Some(count) = after_ok
            .split_whitespace()
            .next()
            .and_then(|token| token.parse::<u64>().ok())
        {
            passed_total = passed_total.saturating_add(count);
        }
    }
    !saw_failure && passed_total > 0
}

/// Whether a Rosen post-land platform label matches this host (`host_os`, `host_arch`) (#741).
/// `None`, empty, `"base"`, and `"any"` are treated as any-platform checks runnable locally.
pub fn rosen_platform_matches_host(platform: Option<&str>, host_os: &str, host_arch: &str) -> bool {
    let Some(raw) = platform.map(str::trim).filter(|value| !value.is_empty()) else {
        return true;
    };
    if raw.eq_ignore_ascii_case("base") || raw.eq_ignore_ascii_case("any") {
        return true;
    }
    label_matches_host(raw, host_os, host_arch)
}

/// Whether a GitHub Actions job (judged from its `runner` label, falling back to `job_name`)
/// runs on the same OS as this host (`host_os`, `host_arch`) (#741).
pub fn github_job_matches_host(
    runner: Option<&str>,
    job_name: &str,
    host_os: &str,
    host_arch: &str,
) -> bool {
    if let Some(label) = runner.map(str::trim).filter(|value| !value.is_empty())
        && detect_os(label).is_some()
    {
        return label_matches_host(label, host_os, host_arch);
    }
    label_matches_host(job_name, host_os, host_arch)
}

/// Formats required targeted test commands as bullet lines for a repair brief.
pub fn format_required_test_commands(tests: &[TargetedTest]) -> String {
    tests
        .iter()
        .map(|test| format!("- `{}`", test.rosen_run_command()))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Refusal returned when a repair thread tries to name its change before all required tests
/// pass on `current_head` (#741). Returns `None` when `missing` is empty.
pub fn required_tests_name_refusal(
    change: &str,
    current_head: &str,
    missing: &[TargetedTest],
) -> Option<String> {
    if missing.is_empty() {
        return None;
    }
    let short_change = short_id(change);
    let head = short_id(current_head);
    let list = format_required_test_commands(missing);
    Some(format!(
        "Cannot name change {short_change}: required repair test(s) have not passed on the \
         current snapshot ({head}).\n\n\
         Run and pass the required test(s) through `queue_run` (or `preflight` from \
         `velocity-run`) before naming the change:\n\
         {list}\n\n\
         If a test still fails, keep fixing until it passes, then run `preflight` and name the \
         change with `rosen commit -m \"<summary>\"`."
    ))
}

/// Refusal `(reason, instructions)` returned by the landing runner when a repair change's
/// current snapshot lacks passing attestations for its required tests (#741).
pub fn required_tests_land_refusal(
    change: &str,
    current_head: &str,
    missing: &[TargetedTest],
) -> Option<(String, String)> {
    if missing.is_empty() {
        return None;
    }
    let short_change = short_id(change);
    let head = short_id(current_head);
    let list = format_required_test_commands(missing);
    Some((
        "required repair tests have not passed on current snapshot".to_owned(),
        format!(
            "Change {short_change} (snapshot {head}) has required repair test(s) that have not \
             passed on this snapshot:\n\
             {list}\n\n\
             Run those test(s) through `queue_run` (or `preflight` from `velocity-run`) until \
             they pass, then name the change again with `rosen commit -m \"<what changed>\"` so \
             Velocity can land it."
        ),
    ))
}

#[derive(Default)]
struct FailureOutputParser {
    current_group: Vec<String>,
    failed_groups: Vec<Vec<String>>,
    rerun_targets: Vec<(Option<String>, Vec<String>)>,
    in_failures_list: bool,
}

impl FailureOutputParser {
    fn observe_line(&mut self, line: &str) {
        let trimmed = line.trim();
        if self.in_failures_list {
            if let Some(name) = parse_indented_failure_name(line, trimmed) {
                push_unique(&mut self.current_group, name);
                return;
            }
            self.in_failures_list = false;
        }
        if trimmed == "failures:" {
            self.in_failures_list = true;
            return;
        }
        if let Some(name) = parse_stdout_header_name(trimmed) {
            push_unique(&mut self.current_group, name);
            return;
        }
        if let Some(name) = parse_failed_test_line(trimmed) {
            push_unique(&mut self.current_group, name);
            return;
        }
        if trimmed.starts_with("test result: FAILED") {
            let group = std::mem::take(&mut self.current_group);
            self.failed_groups.push(group);
            return;
        }
        if trimmed.starts_with("test result: ok.") {
            self.current_group.clear();
            return;
        }
        if let Some(target) = parse_rerun_target(trimmed) {
            if !self.current_group.is_empty() {
                let group = std::mem::take(&mut self.current_group);
                self.failed_groups.push(group);
            }
            self.rerun_targets.push(target);
        }
    }

    fn finish(mut self, hung_test: Option<&str>) -> Vec<TargetedTest> {
        if self.rerun_targets.is_empty() {
            return Vec::new();
        }
        if !self.current_group.is_empty() {
            let group = std::mem::take(&mut self.current_group);
            self.failed_groups.push(group);
        }
        let target_count = self.rerun_targets.len();
        let mut results = Vec::with_capacity(target_count);
        for (index, (package, target_args)) in self.rerun_targets.into_iter().enumerate() {
            let is_doc = target_args.iter().any(|arg| arg == "--doc");
            let mut tests = if is_doc {
                Vec::new()
            } else if target_count == 1 && self.failed_groups.len() > 1 {
                let mut combined = Vec::new();
                for group in &self.failed_groups {
                    for name in group {
                        push_unique(&mut combined, name.clone());
                    }
                }
                combined
            } else {
                self.failed_groups.get(index).cloned().unwrap_or_default()
            };
            if tests.is_empty()
                && !is_doc
                && target_count == 1
                && let Some(hung) = hung_test
                    .map(str::trim)
                    .filter(|name| is_valid_test_name(name))
            {
                tests.push(hung.to_owned());
            }
            results.push(TargetedTest {
                package,
                target_args,
                tests,
            });
        }
        merge_targeted_tests(results)
    }
}

fn parse_stdout_header_name(trimmed: &str) -> Option<String> {
    let inner = trimmed
        .strip_prefix("---- ")?
        .strip_suffix(" stdout ----")?
        .trim();
    normalize_test_name(inner)
}

fn parse_indented_failure_name(line: &str, trimmed: &str) -> Option<String> {
    if !line.starts_with([' ', '\t']) || trimmed.is_empty() {
        return None;
    }
    if trimmed.starts_with("---- ") || trimmed.starts_with("test result:") {
        return None;
    }
    normalize_test_name(trimmed)
}

fn parse_failed_test_line(trimmed: &str) -> Option<String> {
    let rest = trimmed.strip_prefix("test ")?;
    let name = rest.strip_suffix(" ... FAILED")?.trim();
    normalize_test_name(name)
}

fn normalize_test_name(raw: &str) -> Option<String> {
    if raw.contains(" (line ") {
        return None;
    }
    let stripped = raw
        .strip_suffix(" - should panic")
        .or_else(|| raw.strip_suffix(" - compile"))
        .unwrap_or(raw)
        .trim();
    is_valid_test_name(stripped).then(|| stripped.to_owned())
}

fn parse_rerun_target(trimmed: &str) -> Option<(Option<String>, Vec<String>)> {
    let (_, after) = trimmed.split_once("to rerun pass ")?;
    let cleaned = after
        .trim()
        .trim_end_matches('.')
        .trim()
        .trim_matches(['`', '\'', '"'])
        .trim();
    if cleaned.is_empty() {
        return None;
    }
    let tokens: Vec<&str> = cleaned.split_whitespace().collect();
    parse_target_tokens(&tokens)
}

fn parse_target_tokens(tokens: &[&str]) -> Option<(Option<String>, Vec<String>)> {
    let mut package = None;
    let mut target_args = Vec::new();
    let mut index = 0;
    while let Some(&token) = tokens.get(index) {
        match token {
            "-p" | "--package" => {
                let value = *tokens.get(index + 1)?;
                if !is_valid_package_name(value) {
                    return None;
                }
                package = Some(value.to_owned());
                index += 2;
            }
            value if value.starts_with("--package=") => {
                let name = value.strip_prefix("--package=")?;
                if !is_valid_package_name(name) {
                    return None;
                }
                package = Some(name.to_owned());
                index += 1;
            }
            "--lib" | "--bins" | "--tests" | "--doc" | "--examples" | "--benches"
            | "--all-targets" => {
                target_args.push(token.to_owned());
                index += 1;
            }
            "--bin" | "--test" | "--example" | "--bench" => {
                let value = *tokens.get(index + 1)?;
                if !is_valid_target_name(value) {
                    return None;
                }
                target_args.push(token.to_owned());
                target_args.push(value.to_owned());
                index += 2;
            }
            value => {
                let (flag, name) = parse_equals_target_flag(value)?;
                target_args.push(flag.to_owned());
                target_args.push(name.to_owned());
                index += 1;
            }
        }
    }
    if package.is_none() && target_args.is_empty() {
        return None;
    }
    Some((package, target_args))
}

fn parse_equals_target_flag(value: &str) -> Option<(&'static str, &str)> {
    for flag in ["--bin", "--test", "--example", "--bench"] {
        if let Some(name) = value
            .strip_prefix(flag)
            .and_then(|rest| rest.strip_prefix('='))
            && is_valid_target_name(name)
        {
            return Some((flag, name));
        }
    }
    None
}

struct ParsedCargoTestInvocation {
    package: Option<String>,
    target_args: Vec<String>,
    filters: Vec<String>,
}

fn parse_cargo_test_invocation(arguments: &[String]) -> Option<ParsedCargoTestInvocation> {
    let (command, rest) = arguments.split_first()?;
    if command != "test" {
        return None;
    }
    let mut package = None;
    let mut target_args = Vec::new();
    let mut filters = Vec::new();
    let mut index = 0;
    let mut after_separator = false;
    while let Some(arg) = rest.get(index).map(String::as_str) {
        if !after_separator && arg == "--" {
            after_separator = true;
            index += 1;
            continue;
        }
        if !after_separator {
            index += parse_cargo_test_arg(
                arg,
                rest.get(index + 1).map(String::as_str),
                &mut package,
                &mut target_args,
                &mut filters,
            )?;
        } else {
            index += parse_libtest_arg(arg, rest.get(index + 1).map(String::as_str), &mut filters)?;
        }
    }
    Some(ParsedCargoTestInvocation {
        package,
        target_args,
        filters,
    })
}

fn parse_cargo_test_arg(
    arg: &str,
    next: Option<&str>,
    package: &mut Option<String>,
    target_args: &mut Vec<String>,
    filters: &mut Vec<String>,
) -> Option<usize> {
    match arg {
        "-p" | "--package" => {
            let value = next?;
            if !is_valid_package_name(value) {
                return None;
            }
            *package = Some(value.to_owned());
            Some(2)
        }
        value if value.starts_with("--package=") => {
            let name = value.strip_prefix("--package=")?;
            if !is_valid_package_name(name) {
                return None;
            }
            *package = Some(name.to_owned());
            Some(1)
        }
        "--lib" | "--bins" | "--tests" | "--doc" | "--examples" | "--benches" | "--all-targets" => {
            target_args.push(arg.to_owned());
            Some(1)
        }
        "--bin" | "--test" | "--example" | "--bench" => {
            let value = next?;
            if !is_valid_target_name(value) {
                return None;
            }
            target_args.push(arg.to_owned());
            target_args.push(value.to_owned());
            Some(2)
        }
        "--locked" | "--no-fail-fast" | "--quiet" | "-q" | "--no-capture" => Some(1),
        "--color" => {
            let value = next?;
            matches!(value, "auto" | "always" | "never").then_some(2)
        }
        value if value.starts_with("--color=") => {
            let mode = value.strip_prefix("--color=")?;
            matches!(mode, "auto" | "always" | "never").then_some(1)
        }
        value if value.starts_with('-') => {
            let (flag, name) = parse_equals_target_flag(value)?;
            target_args.push(flag.to_owned());
            target_args.push(name.to_owned());
            Some(1)
        }
        value => {
            if !filters.is_empty() || !is_valid_test_name(value) {
                return None;
            }
            filters.push(value.to_owned());
            Some(1)
        }
    }
}

fn parse_libtest_arg(arg: &str, next: Option<&str>, filters: &mut Vec<String>) -> Option<usize> {
    match arg {
        "--nocapture" | "--exact" | "--show-output" | "--ignored" | "--include-ignored"
        | "--quiet" | "-q" => Some(1),
        "--test-threads" => {
            let value = next?;
            value
                .parse::<usize>()
                .ok()
                .filter(|count| *count > 0)
                .map(|_| 2)
        }
        value if value.starts_with("--test-threads=") => value
            .strip_prefix("--test-threads=")?
            .parse::<usize>()
            .ok()
            .filter(|count| *count > 0)
            .map(|_| 1),
        "--color" => {
            let value = next?;
            matches!(value, "auto" | "always" | "never").then_some(2)
        }
        value if value.starts_with("--color=") => {
            let mode = value.strip_prefix("--color=")?;
            matches!(mode, "auto" | "always" | "never").then_some(1)
        }
        "--format" => {
            let value = next?;
            matches!(value, "pretty" | "terse").then_some(2)
        }
        value if value.starts_with("--format=") => {
            let mode = value.strip_prefix("--format=")?;
            matches!(mode, "pretty" | "terse").then_some(1)
        }
        // Reject `--list`, `--skip`, or any unknown flag so tests cannot be bypassed.
        value if value.starts_with('-') => None,
        value => {
            if !is_valid_test_name(value) {
                return None;
            }
            push_unique(filters, value.to_owned());
            Some(1)
        }
    }
}

fn label_matches_host(label: &str, host_os: &str, host_arch: &str) -> bool {
    let Some(label_os) = detect_os(label) else {
        return false;
    };
    if !label_os.eq_ignore_ascii_case(host_os) {
        return false;
    }
    if let Some(label_arch) = detect_arch(label) {
        return normalize_arch(label_arch) == normalize_arch(host_arch);
    }
    true
}

fn detect_os(label: &str) -> Option<&'static str> {
    let lower = label.to_ascii_lowercase();
    if contains_word_or_prefix(&lower, &["windows", "win32"]) {
        Some("windows")
    } else if contains_word_or_prefix(&lower, &["macos", "darwin", "osx"]) {
        Some("macos")
    } else if contains_word_or_prefix(&lower, &["linux", "ubuntu", "debian", "fedora", "alpine"]) {
        Some("linux")
    } else {
        None
    }
}

fn detect_arch(label: &str) -> Option<&'static str> {
    let lower = label.to_ascii_lowercase();
    for token in
        lower.split(|character: char| !character.is_ascii_alphanumeric() && character != '_')
    {
        match token {
            "x86_64" | "x64" | "amd64" => return Some("x86_64"),
            "aarch64" | "arm64" => return Some("aarch64"),
            _ => {}
        }
    }
    None
}

fn normalize_arch(arch: &str) -> &str {
    match arch.to_ascii_lowercase().as_str() {
        "x64" | "amd64" | "x86_64" => "x86_64",
        "arm64" | "aarch64" => "aarch64",
        _ => arch,
    }
}

fn contains_word_or_prefix(lower: &str, needles: &[&str]) -> bool {
    lower
        .split(|character: char| !character.is_ascii_alphanumeric())
        .any(|token| needles.contains(&token))
}

fn is_valid_package_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('-')
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

fn is_valid_target_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('-')
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
}

fn is_valid_test_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('-')
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b':'))
}

fn strip_ansi(line: &str) -> String {
    let mut output = String::with_capacity(line.len());
    let mut chars = line.chars();
    while let Some(character) = chars.next() {
        if character == '\x1b' && chars.as_str().starts_with('[') {
            chars.next();
            for code in chars.by_ref() {
                if ('@'..='~').contains(&code) {
                    break;
                }
            }
            continue;
        }
        output.push(character);
    }
    output
}

fn push_unique(list: &mut Vec<String>, value: String) {
    if !list.iter().any(|existing| existing == &value) {
        list.push(value);
    }
}

fn short_id(value: &str) -> &str {
    value.get(..8).unwrap_or(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Risk (#741): parsing failing test output with `--no-fail-fast` must pair each failing
    // crate's test names with its own `to rerun pass` target and produce exact rerun commands.
    #[test]
    fn parses_single_and_multi_crate_cargo_test_failures() {
        let output = "\
running 15 tests
test dependencies::tests::refresh_wakes_reserved_slug_build_holders_and_skips_non_thread_holders ... FAILED

failures:

---- dependencies::tests::refresh_wakes_reserved_slug_build_holders_and_skips_non_thread_holders stdout ----
Error: Invalid(\"Claim your piece before waiting\")

failures:
    dependencies::tests::refresh_wakes_reserved_slug_build_holders_and_skips_non_thread_holders

test result: FAILED. 14 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.12s

running 4 tests
test deploy::tests::gate_blocks_red_platform ... FAILED
test deploy::tests::gate_allows_green_platform ... FAILED

failures:

---- deploy::tests::gate_blocks_red_platform stdout ----
assertion failed

---- deploy::tests::gate_allows_green_platform stdout ----
assertion failed

failures:
    deploy::tests::gate_blocks_red_platform
    deploy::tests::gate_allows_green_platform

test result: FAILED. 2 passed; 2 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s

error: test failed, to rerun pass `-p velocity-threads --lib`
error: test failed, to rerun pass `-p velocity-core --lib`
";
        let parsed = parse_cargo_test_failures(output);
        assert_eq!(parsed.len(), 2);
        assert_eq!(
            parsed[0].rosen_run_command(),
            "rosen run -- cargo test -p velocity-threads --lib dependencies::tests::refresh_wakes_reserved_slug_build_holders_and_skips_non_thread_holders"
        );
        assert_eq!(
            parsed[1].rosen_run_command(),
            "rosen run -- cargo test -p velocity-core --lib -- deploy::tests::gate_blocks_red_platform deploy::tests::gate_allows_green_platform"
        );
    }

    // Risk (#741): required-test policy must admit the exact failing test(s) (with safe
    // display flags) while refusing bypass flags like `--list`/`--skip` or unrelated crates,
    // and gate evaluation must refuse naming until every required test passes on current head.
    #[test]
    fn required_tests_policy_and_head_gate_enforce_exact_pass() {
        let target = TargetedTest {
            package: Some("velocity-threads".into()),
            target_args: vec!["--lib".into()],
            tests: vec![
                "dependencies::tests::alpha".into(),
                "dependencies::tests::beta".into(),
            ],
        };
        let mut record = RequiredTestsRecord::new("chg12345678", vec![target]);
        let head_a = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let head_b = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

        let run_alpha: Vec<String> = [
            "test",
            "-p",
            "velocity-threads",
            "--lib",
            "dependencies::tests::alpha",
            "--",
            "--nocapture",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect();
        let run_both: Vec<String> = [
            "test",
            "-p",
            "velocity-threads",
            "--lib",
            "--",
            "dependencies::tests::alpha",
            "dependencies::tests::beta",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect();
        let bypass_list: Vec<String> = [
            "test",
            "-p",
            "velocity-threads",
            "--lib",
            "dependencies::tests::alpha",
            "--",
            "--list",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect();
        let other_crate: Vec<String> = ["test", "-p", "velocity-core", "--lib"]
            .into_iter()
            .map(str::to_owned)
            .collect();

        assert!(record.allows_cargo_test(&run_alpha));
        assert!(record.allows_cargo_test(&run_both));
        assert!(!record.allows_cargo_test(&bypass_list));
        assert!(!record.allows_cargo_test(&other_crate));

        assert!(!record.all_passed_for_head(head_a));
        assert!(record.record_pass_for_arguments(head_a, &run_alpha, 10));
        assert_eq!(record.missing_for_head(head_a).len(), 1);
        assert_eq!(
            record.missing_for_head(head_a)[0].tests,
            vec!["dependencies::tests::beta"]
        );
        assert!(record.record_pass_for_arguments(head_a, &run_both, 11));
        assert!(record.all_passed_for_head(head_a));
        // Editing the workspace moves the head and invalidates earlier test passes.
        assert!(!record.all_passed_for_head(head_b));
        let refusal =
            required_tests_name_refusal(&record.change, head_b, &record.missing_for_head(head_b));
        assert!(refusal.is_some_and(|text| text.contains("velocity-threads")));
    }

    // Risk (#741): deleting or renaming a failing test makes `cargo test <old_name>` exit 0
    // with `0 passed`; output verification must reject zero-pass runs.
    #[test]
    fn output_check_rejects_zero_passed_runs() {
        let zero = "running 0 tests\n\ntest result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 12 filtered out; finished in 0.00s\n";
        let one = "running 1 test\ntest foo::bar ... ok\n\ntest result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 11 filtered out; finished in 0.02s\n";
        assert!(!cargo_test_output_passed_at_least_one(zero));
        assert!(cargo_test_output_passed_at_least_one(one));
    }

    // Risk (#741): platform matching must require targeted tests only for same-OS failures
    // while keeping cross-platform failures type-check-only.
    #[test]
    fn platform_matching_distinguishes_same_and_other_os() {
        assert!(rosen_platform_matches_host(
            Some("Windows"),
            "windows",
            "x86_64"
        ));
        assert!(rosen_platform_matches_host(
            Some("Windows (x64)"),
            "windows",
            "x86_64"
        ));
        assert!(rosen_platform_matches_host(None, "windows", "x86_64"));
        assert!(rosen_platform_matches_host(
            Some("base"),
            "windows",
            "x86_64"
        ));
        assert!(!rosen_platform_matches_host(
            Some("macOS"),
            "windows",
            "x86_64"
        ));
        assert!(!rosen_platform_matches_host(
            Some("Linux"),
            "windows",
            "x86_64"
        ));
        assert!(!rosen_platform_matches_host(
            Some("Windows (arm64)"),
            "windows",
            "x86_64"
        ));

        assert!(github_job_matches_host(
            Some("windows-latest"),
            "test (windows-latest)",
            "windows",
            "x86_64"
        ));
        assert!(!github_job_matches_host(
            Some("macos-latest"),
            "test (macos-latest)",
            "windows",
            "x86_64"
        ));
        assert!(github_job_matches_host(
            None,
            "Windows (build, clippy, test)",
            "windows",
            "x86_64"
        ));
        assert!(!github_job_matches_host(
            None,
            "macOS (build, clippy, test)",
            "windows",
            "x86_64"
        ));
    }
}
