//! Keeping Collapse's pinned `velocity-graph` indexer current (SPEC.md F19 "Standalone
//! indexer", #577).
//!
//! Collapse pins Velocity's standalone Linux `velocity-graph` archive and SHA-256 in
//! `ops/Dockerfile`. When a published Velocity release's indexer reports a different
//! parser or schema version than the pinned release, Velocity keeps exactly one open
//! "Bump the velocity-graph pin to `<version>`" issue on Collapse's repository, naming
//! the version, archive URL and SHA-256, and rewrites it in place for newer releases.
//! Everything here is pure decision logic; the GitHub reads and writes live in
//! `velocity-github`.

use std::cmp::Ordering;
use std::collections::BTreeMap;

use crate::release::{
    compare_versions, parse_release_version, release_tag, version_from_release_tag,
};

/// Every Collapse indexer pin-bump issue's title starts with this, which is how Velocity
/// finds them.
pub const PIN_BUMP_TITLE: &str = "Bump the velocity-graph pin to ";
/// Where Collapse pins the `velocity-graph` release and its SHA-256.
pub const DOCKERFILE_PATH: &str = "ops/Dockerfile";
/// Target triple of the standalone Linux indexer archive published on Velocity releases.
pub const ARCHIVE_TARGET: &str = "x86_64-unknown-linux-gnu";
/// Source file holding `PARSER_VERSION` for a release tag.
pub const PARSER_VERSION_PATH: &str = "crates/velocity-graph/src/cache.rs";
/// Source file holding `SCHEMA` for a release tag.
pub const SCHEMA_VERSION_PATH: &str = "crates/velocity-graph/src/command_line.rs";

/// The last line of a pin-bump issue's body names the version it asks for.
const TARGET_MARKER: &str = "<!-- velocity-graph-pin-bump:";

/// Which release host Collapse's `ops/Dockerfile` currently downloads `velocity-graph` from (#593).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PinReleaseHost {
    Collapse,
    GitHub,
}

/// The `velocity-graph` release Collapse's `ops/Dockerfile` pins.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CollapseGraphPin {
    /// Velocity's repository from the download URL, when present (`owner/name`).
    pub repository: Option<String>,
    /// Whether `ops/Dockerfile` downloads the archive from Collapse or GitHub (#593).
    pub host: PinReleaseHost,
    /// The pinned release version without a leading `v`, e.g. `0.1.0-beta.17`.
    pub version: String,
    /// The release tag, e.g. `v0.1.0-beta.17`.
    pub tag: String,
    /// The pinned archive SHA-256 when found in `ops/Dockerfile` (64 lowercase hex digits).
    pub sha256: Option<String>,
}

impl CollapseGraphPin {
    /// Reads the `velocity-graph` pin from Collapse's `ops/Dockerfile`. Supports `ARG`,
    /// `ENV`, shell variables inside `RUN`, heredocs, `ADD` and inline URLs, while
    /// ignoring other pinned tools such as `git`.
    pub fn parse(dockerfile: &str) -> Option<Self> {
        let instructions = parse_instructions(dockerfile);
        let mut variables = BTreeMap::new();
        let mut matched: Option<(PinReleaseHost, Option<String>, String, Option<String>)> = None;

        for instruction in &instructions {
            let Some((keyword, rest)) = split_instruction(instruction) else {
                continue;
            };
            if keyword.eq_ignore_ascii_case("ARG") {
                record_arg(&mut variables, rest);
            } else if keyword.eq_ignore_ascii_case("ENV") {
                record_env(&mut variables, rest);
            } else if keyword.eq_ignore_ascii_case("RUN")
                || keyword.eq_ignore_ascii_case("ADD")
                || keyword.eq_ignore_ascii_case("COPY")
            {
                let mut local = variables.clone();
                if keyword.eq_ignore_ascii_case("RUN") {
                    collect_shell_assignments(&mut local, rest);
                }
                let expanded = expand_variables(rest, &local);
                if let Some((host, repository, version)) = extract_archive_pin(&expanded) {
                    let sha256 =
                        extract_sha256_token(&expanded).or_else(|| named_sha256_variable(&local));
                    matched = Some((host, repository, version, sha256));
                }
            }
        }

        let (host, repository, version, sha256) =
            matched.or_else(|| pin_from_variables(&variables))?;
        let tag = release_tag(&version)?;
        let normalized = version_from_release_tag(&tag)?;
        Some(Self {
            repository,
            host,
            version: normalized,
            tag,
            sha256: sha256.or_else(|| named_sha256_variable(&variables)),
        })
    }
}

/// The parser and output schema versions reported by `velocity-graph version`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IndexerVersion {
    pub parser: u32,
    pub schema: u32,
}

/// Parses `PARSER_VERSION` from `crates/velocity-graph/src/cache.rs`.
pub fn parse_parser_version(source: &str) -> Option<u32> {
    parse_u32_const(source, "PARSER_VERSION")
}

/// Parses `SCHEMA` from `crates/velocity-graph/src/command_line.rs`.
pub fn parse_schema_version(source: &str) -> Option<u32> {
    parse_u32_const(source, "SCHEMA")
}

/// Archive filename published on a Velocity release for `version`.
pub fn archive_name(version: &str) -> String {
    format!("velocity-graph-{version}-{ARCHIVE_TARGET}.tar.gz")
}

/// Checksum filename published alongside [`archive_name`].
pub fn checksum_name(version: &str) -> String {
    format!("{}.sha256", archive_name(version))
}

/// Extracts and validates the SHA-256 digest from a `<archive>.sha256` file. When a
/// filename is present on the line, it must match `expected_archive`.
pub fn parse_sha256(text: &str, expected_archive: &str) -> Option<String> {
    let line = text.lines().map(str::trim).find(|line| !line.is_empty())?;
    let mut parts = line.split_whitespace();
    let digest = parts.next()?;
    if !is_sha256(digest) {
        return None;
    }
    if let Some(name) = parts.next() {
        let filename = name.trim_start_matches('*');
        let basename = filename.rsplit('/').next().unwrap_or(filename);
        if basename != expected_archive {
            return None;
        }
    }
    Some(digest.to_ascii_lowercase())
}

/// One asset on a GitHub release considered when searching for the latest indexer archive.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReleaseAssetCandidate {
    pub id: u64,
    pub name: String,
    pub download_url: String,
}

/// One GitHub release considered when searching for the latest indexer archive.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReleaseCandidate {
    pub tag: String,
    pub draft: bool,
    pub assets: Vec<ReleaseAssetCandidate>,
}

/// A published Velocity release carrying both the Linux `velocity-graph` archive and its
/// `.sha256` checksum asset.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IndexerRelease {
    pub tag: String,
    pub version: String,
    pub archive_name: String,
    pub archive_url: String,
    pub archive_asset_id: u64,
    pub sha256_asset_id: u64,
}

/// Selects the highest SemVer non-draft release that carries both the `velocity-graph`
/// archive and its `.sha256` checksum. Ignores non-version tags such as `updater`.
pub fn latest_indexer_release(releases: &[ReleaseCandidate]) -> Option<IndexerRelease> {
    releases
        .iter()
        .filter(|release| !release.draft)
        .filter_map(|release| {
            let version = version_from_release_tag(&release.tag)?;
            let parsed = parse_release_version(&version)?;
            let expected_archive = archive_name(&version);
            let expected_checksum = checksum_name(&version);
            let archive = release
                .assets
                .iter()
                .find(|asset| asset.name == expected_archive && !asset.download_url.is_empty())?;
            let checksum = release
                .assets
                .iter()
                .find(|asset| asset.name == expected_checksum)?;
            Some((
                parsed,
                IndexerRelease {
                    tag: release.tag.clone(),
                    version,
                    archive_name: expected_archive,
                    archive_url: archive.download_url.clone(),
                    archive_asset_id: archive.id,
                    sha256_asset_id: checksum.id,
                },
            ))
        })
        .max_by(|(left, _), (right, _)| left.cmp(right))
        .map(|(_, release)| release)
}

/// Whether `candidate` has a strictly higher SemVer than `pinned`.
pub fn is_newer_version(candidate: &str, pinned: &str) -> bool {
    compare_versions(candidate, pinned) == Some(Ordering::Greater)
}

/// Whether a published release warrants a Collapse pin-bump issue: the release is strictly
/// newer than Collapse's pin and changes either the parser or schema version.
pub fn should_bump(
    pin_version: &str,
    pin: IndexerVersion,
    release_version: &str,
    release: IndexerVersion,
) -> bool {
    is_newer_version(release_version, pin_version) && pin != release
}

/// Whether `ops/Dockerfile` should be updated to `release_version` on `release_host`:
/// either parser/schema changed on a newer release, or `ops/Dockerfile` still points to
/// GitHub while the release is published on Collapse at the same or newer version (#593).
pub fn should_bump_or_migrate(
    pin: &CollapseGraphPin,
    pin_indexer: IndexerVersion,
    release_version: &str,
    release_indexer: IndexerVersion,
    release_host: PinReleaseHost,
) -> bool {
    if should_bump(&pin.version, pin_indexer, release_version, release_indexer) {
        return true;
    }
    pin.host == PinReleaseHost::GitHub
        && release_host == PinReleaseHost::Collapse
        && compare_versions(release_version, &pin.version)
            .is_some_and(|ordering| ordering != Ordering::Less)
}

/// An open issue read from Collapse's GitHub repository.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpenIssue {
    pub repository: String,
    pub number: u32,
    pub title: String,
    pub body: String,
}

impl OpenIssue {
    pub fn is_pin_bump(&self) -> bool {
        self.title.starts_with(PIN_BUMP_TITLE)
    }

    /// The version a pin-bump issue asks for, from its body's last line.
    pub fn pin_bump_target(&self) -> Option<&str> {
        self.body
            .lines()
            .next_back()?
            .trim()
            .strip_prefix(TARGET_MARKER)?
            .strip_suffix("-->")
            .map(str::trim)
    }
}

/// What the Collapse indexer pin-bump issue asks for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GraphPinBump {
    pub velocity_repository: String,
    pub pin: CollapseGraphPin,
    pub pin_indexer: IndexerVersion,
    pub target_version: String,
    pub target_tag: String,
    pub target_indexer: IndexerVersion,
    pub archive_url: String,
    pub sha256: String,
}

impl GraphPinBump {
    pub fn title(&self) -> String {
        format!("{PIN_BUMP_TITLE}{}", self.target_version)
    }

    /// The issue's whole text. Identical inputs produce identical text, so an unchanged
    /// issue is recognised by comparing it.
    pub fn body(&self) -> String {
        let velocity = &self.velocity_repository;
        let tag = &self.target_tag;
        let version = &self.target_version;
        let pinned = &self.pin.version;
        let parser_line = version_change(
            "Parser",
            self.pin_indexer.parser,
            self.target_indexer.parser,
        );
        let schema_line = version_change(
            "Schema",
            self.pin_indexer.schema,
            self.target_indexer.schema,
        );
        let release_link = if crate::collapse::is_collapse_download_url(&self.archive_url) {
            crate::collapse::release_page(velocity, tag)
                .unwrap_or_else(|| format!("https://collapse.sh/{velocity}/releases/{tag}"))
        } else {
            format!("https://github.com/{velocity}/releases/tag/{tag}")
        };
        let reason_line = if self.pin_indexer != self.target_indexer {
            format!(
                "Velocity release `{tag}` publishes a `velocity-graph` indexer with a different \
                 parser or schema version than Collapse's pin (`{pinned}`)."
            )
        } else {
            format!(
                "Velocity release `{tag}` publishes the `velocity-graph` indexer archive on \
                 Collapse, replacing the GitHub download URL pinned at `{pinned}`."
            )
        };
        [
            format!(
                "{reason_line}\n\
                 Velocity keeps this issue current: a newer release rewrites it."
            ),
            format!("**What changed.**\n- {parser_line}\n- {schema_line}"),
            format!(
                "**New pin.**\n\
                 - Version: `{version}` (release [`{tag}`]({release_link}))\n\
                 - Archive URL: `{}`\n\
                 - SHA-256: `{}`",
                self.archive_url, self.sha256
            ),
            format!(
                "**Change.**\n\
                 - Update `{DOCKERFILE_PATH}` to pin `velocity-graph` at `{version}` using the \
                 archive URL and SHA-256 above."
            ),
            format!(
                "**Done when** `{DOCKERFILE_PATH}` pins `{version}` with SHA-256 `{}` and the \
                 build passes.",
                self.sha256
            ),
            format!("{TARGET_MARKER}{version} -->"),
        ]
        .join("\n\n")
    }
}

/// What to do with the kept pin-bump issue.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PinBumpStep {
    /// No open unfinished issue asks for a bump: open one.
    Open,
    /// The kept issue says something else: rewrite its title and body.
    Rewrite(u32),
    /// The kept issue is already current.
    Keep(u32),
}

/// How the open pin-bump issues on Collapse become exactly one current issue.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PinBumpPlan {
    pub step: PinBumpStep,
    /// Issues whose target version Collapse already pins (or has already surpassed), so
    /// they are closed instead of being rewritten into a second bump.
    pub finished: Vec<u32>,
    /// Further open unfinished pin-bump issues, closed in favour of the kept one.
    pub duplicates: Vec<u32>,
}

/// Keeps the oldest unfinished pin-bump issue and makes it say `bump`.
pub fn plan(open: &[OpenIssue], bump: &GraphPinBump) -> PinBumpPlan {
    let mut issues: Vec<&OpenIssue> = open.iter().filter(|issue| issue.is_pin_bump()).collect();
    issues.sort_by_key(|issue| issue.number);
    let (finished, unfinished): (Vec<&OpenIssue>, Vec<&OpenIssue>) =
        issues.into_iter().partition(|issue| {
            issue
                .pin_bump_target()
                .is_some_and(|target| is_pinned_or_older(target, &bump.pin.version))
        });
    let step = match unfinished.first() {
        None => PinBumpStep::Open,
        Some(kept)
            if same_text(&kept.title, &bump.title()) && same_text(&kept.body, &bump.body()) =>
        {
            PinBumpStep::Keep(kept.number)
        }
        Some(kept) => PinBumpStep::Rewrite(kept.number),
    };
    PinBumpPlan {
        step,
        finished: finished.iter().map(|issue| issue.number).collect(),
        duplicates: unfinished
            .iter()
            .skip(1)
            .map(|issue| issue.number)
            .collect(),
    }
}

/// The oldest open pin-bump issue and the others, which are closed in its favour while no
/// bump is due.
pub fn oldest_and_surplus(open: &[OpenIssue]) -> Option<(u32, Vec<u32>)> {
    let mut numbers: Vec<u32> = open
        .iter()
        .filter(|issue| issue.is_pin_bump())
        .map(|issue| issue.number)
        .collect();
    numbers.sort_unstable();
    let (oldest, surplus) = numbers.split_first()?;
    Some((*oldest, surplus.to_vec()))
}

fn version_change(label: &str, before: u32, after: u32) -> String {
    if before == after {
        format!("{label} version: `{before}` (unchanged)")
    } else {
        format!("{label} version: `{before}` -> `{after}`")
    }
}

fn is_pinned_or_older(target: &str, pinned: &str) -> bool {
    target == pinned
        || compare_versions(target, pinned).is_some_and(|ordering| ordering != Ordering::Greater)
}

fn same_text(left: &str, right: &str) -> bool {
    left.replace("\r\n", "\n").trim() == right.replace("\r\n", "\n").trim()
}

fn is_sha256(text: &str) -> bool {
    text.len() == 64 && text.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn parse_u32_const(source: &str, name: &str) -> Option<u32> {
    for raw_line in source.lines() {
        let line = raw_line.trim_start();
        if line.starts_with("//") {
            continue;
        }
        let after_vis = strip_visibility(line);
        let Some(after_const) = after_vis.strip_prefix("const") else {
            continue;
        };
        if !after_const.starts_with(char::is_whitespace) {
            continue;
        }
        let Some(after_name) = after_const.trim_start().strip_prefix(name) else {
            continue;
        };
        if after_name
            .chars()
            .next()
            .is_some_and(|character| character.is_ascii_alphanumeric() || character == '_')
        {
            continue;
        }
        let Some((_, rhs)) = after_name.split_once('=') else {
            continue;
        };
        let Some(literal) = rhs.split(';').next().map(str::trim) else {
            continue;
        };
        let digits: String = literal
            .trim_end_matches(|character: char| character.is_ascii_alphabetic())
            .chars()
            .filter(|character| *character != '_')
            .collect();
        if let Ok(value) = digits.parse::<u32>() {
            return Some(value);
        }
    }
    None
}

fn strip_visibility(line: &str) -> &str {
    let Some(rest) = line.strip_prefix("pub") else {
        return line;
    };
    if rest.starts_with(char::is_whitespace) {
        return rest.trim_start();
    }
    if let Some(after_paren) = rest.strip_prefix('(')
        && let Some((_, tail)) = after_paren.split_once(')')
    {
        return tail.trim_start();
    }
    line
}

/// Joins backslash-continued Dockerfile lines and heredoc bodies into logical instructions.
fn parse_instructions(dockerfile: &str) -> Vec<String> {
    let mut instructions = Vec::new();
    let mut current = String::new();
    let mut heredoc_end: Option<String> = None;

    for raw_line in dockerfile.lines() {
        if let Some(delimiter) = heredoc_end.as_deref() {
            current.push('\n');
            current.push_str(raw_line);
            if raw_line.trim() == delimiter {
                heredoc_end = None;
                instructions.push(std::mem::take(&mut current));
            }
            continue;
        }
        let trimmed = raw_line.trim();
        if current.is_empty() && (trimmed.is_empty() || trimmed.starts_with('#')) {
            continue;
        }
        if !current.is_empty() && trimmed.starts_with('#') {
            continue;
        }
        let continued = raw_line.trim_end().strip_suffix('\\');
        let piece = continued.unwrap_or(raw_line).trim_end();
        if !current.is_empty() {
            current.push(' ');
        }
        current.push_str(piece.trim_start());
        if continued.is_none() {
            if let Some(delimiter) = heredoc_delimiter(&current) {
                heredoc_end = Some(delimiter);
            } else {
                instructions.push(std::mem::take(&mut current));
            }
        }
    }
    if !current.is_empty() {
        instructions.push(current);
    }
    instructions
}

fn heredoc_delimiter(line: &str) -> Option<String> {
    let (_, after) = line.split_once("<<")?;
    let after = after.strip_prefix('-').unwrap_or(after).trim_start();
    let quote = after.chars().next().filter(|ch| *ch == '\'' || *ch == '"');
    let rest = quote.map_or(after, |_| &after[1..]);
    let token: String = rest
        .chars()
        .take_while(|ch| ch.is_ascii_alphanumeric() || *ch == '_')
        .collect();
    (!token.is_empty()).then_some(token)
}

fn split_instruction(instruction: &str) -> Option<(&str, &str)> {
    let trimmed = instruction.trim();
    let split = trimmed.find(char::is_whitespace)?;
    Some((&trimmed[..split], trimmed[split..].trim_start()))
}

fn record_arg(variables: &mut BTreeMap<String, String>, rest: &str) {
    if let Some((name, raw_value)) = rest.split_once('=') {
        let key = name.trim();
        if is_identifier(key) {
            let value = expand_variables(unquote(raw_value.trim()), variables);
            variables.insert(key.to_owned(), value);
        }
    }
}

fn record_env(variables: &mut BTreeMap<String, String>, rest: &str) {
    let trimmed = rest.trim();
    if trimmed.contains('=') {
        for token in split_shell_words(trimmed) {
            if let Some((name, raw_value)) = token.split_once('=')
                && is_identifier(name)
            {
                let value = expand_variables(unquote(raw_value), variables);
                variables.insert(name.to_owned(), value);
            }
        }
    } else if let Some(split) = trimmed.find(char::is_whitespace) {
        let key = &trimmed[..split];
        if is_identifier(key) {
            let value = expand_variables(unquote(trimmed[split..].trim()), variables);
            variables.insert(key.to_owned(), value);
        }
    }
}

fn collect_shell_assignments(variables: &mut BTreeMap<String, String>, script: &str) {
    for segment in script.split(['\n', ';']) {
        for part in segment.split("&&") {
            let mut words = split_shell_words(part.trim()).into_iter().peekable();
            while let Some(word) = words.peek() {
                if word.starts_with("--")
                    || matches!(word.as_str(), "export" | "readonly" | "local")
                {
                    words.next();
                } else {
                    break;
                }
            }
            for word in words {
                let Some((name, raw_value)) = word.split_once('=') else {
                    break;
                };
                if !is_identifier(name) {
                    break;
                }
                let value = expand_variables(unquote(raw_value), variables);
                variables.insert(name.to_owned(), value);
            }
        }
    }
}

fn split_shell_words(text: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    for character in text.chars() {
        match quote {
            Some(delimiter) if character == delimiter => {
                quote = None;
                current.push(character);
            }
            Some(_) => current.push(character),
            None if character == '\'' || character == '"' => {
                quote = Some(character);
                current.push(character);
            }
            None if character.is_whitespace() => {
                if !current.is_empty() {
                    words.push(std::mem::take(&mut current));
                }
            }
            None => current.push(character),
        }
    }
    if !current.is_empty() {
        words.push(current);
    }
    words
}

fn expand_variables(input: &str, variables: &BTreeMap<String, String>) -> String {
    let mut current = input.to_owned();
    for _ in 0..4 {
        let next = expand_once(&current, variables);
        if next == current {
            break;
        }
        current = next;
    }
    current
}

fn expand_once(input: &str, variables: &BTreeMap<String, String>) -> String {
    let mut output = String::with_capacity(input.len());
    let bytes = input.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'$' {
            let Some(character) = input[index..].chars().next() else {
                break;
            };
            output.push(character);
            index += character.len_utf8();
            continue;
        }
        if bytes.get(index + 1) == Some(&b'{')
            && let Some(close) = input[index + 2..].find('}')
        {
            let expr = &input[index + 2..index + 2 + close];
            if let Some(value) = resolve_braced_expression(expr, variables) {
                output.push_str(&value);
                index += close + 3;
                continue;
            }
        } else if let Some((name, consumed)) = read_identifier(&input[index + 1..])
            && let Some(value) = variables.get(name)
        {
            output.push_str(value);
            index += 1 + consumed;
            continue;
        }
        output.push('$');
        index += 1;
    }
    output
}

fn resolve_braced_expression(
    expression: &str,
    variables: &BTreeMap<String, String>,
) -> Option<String> {
    if is_identifier(expression) {
        return variables.get(expression).cloned();
    }
    if let Some((name, prefix)) = expression.split_once('#')
        && is_identifier(name)
    {
        let value = variables.get(name)?;
        return Some(value.strip_prefix(prefix).unwrap_or(value).to_owned());
    }
    if let Some((name, default)) = expression.split_once(":-")
        && is_identifier(name)
    {
        return Some(match variables.get(name) {
            Some(value) if !value.is_empty() => value.clone(),
            _ => default.to_owned(),
        });
    }
    None
}

fn read_identifier(text: &str) -> Option<(&str, usize)> {
    let mut chars = text.char_indices();
    let (_, first) = chars.next()?;
    if !(first.is_ascii_alphabetic() || first == '_') {
        return None;
    }
    let end = chars
        .find(|(_, ch)| !(ch.is_ascii_alphanumeric() || *ch == '_'))
        .map_or(text.len(), |(index, _)| index);
    Some((&text[..end], end))
}

fn is_identifier(text: &str) -> bool {
    let mut chars = text.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    (first.is_ascii_alphabetic() || first == '_')
        && chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
}

fn unquote(text: &str) -> &str {
    if text.len() >= 2
        && ((text.starts_with('"') && text.ends_with('"'))
            || (text.starts_with('\'') && text.ends_with('\'')))
    {
        &text[1..text.len() - 1]
    } else {
        text
    }
}

fn extract_archive_pin(text: &str) -> Option<(PinReleaseHost, Option<String>, String)> {
    let (host, repository) = extract_release_host_and_repository(text);
    let mut search = 0;
    while let Some(found) = text[search..].find("velocity-graph-") {
        let start = search + found + "velocity-graph-".len();
        search = start;
        let rest = &text[start..];
        let token_end = rest
            .find(|ch: char| {
                ch.is_whitespace() || matches!(ch, '"' | '\'' | '/' | ';' | ')' | '`' | '$')
            })
            .unwrap_or(rest.len());
        let token = &rest[..token_end];
        if let Some(version) = version_from_archive_suffix(token) {
            return Some((host, repository, version));
        }
    }
    None
}

fn version_from_archive_suffix(token: &str) -> Option<String> {
    let target_marker = format!("-{ARCHIVE_TARGET}");
    let candidate = token
        .split_once(&target_marker)
        .map(|(version, _)| version)
        .or_else(|| token.split_once("-x86_64-").map(|(version, _)| version))
        .or_else(|| token.split_once("-aarch64-").map(|(version, _)| version))
        .or_else(|| token.strip_suffix(".tar.gz"))?;
    let trimmed = candidate.strip_prefix('v').unwrap_or(candidate);
    parse_release_version(trimmed)?;
    Some(trimmed.to_owned())
}

fn extract_release_host_and_repository(text: &str) -> (PinReleaseHost, Option<String>) {
    for (prefix, host) in [
        ("https://api.collapse.sh/", PinReleaseHost::Collapse),
        ("https://collapse.sh/", PinReleaseHost::Collapse),
        ("https://github.com/", PinReleaseHost::GitHub),
    ] {
        if let Some(repo) = extract_repository_for_prefix(text, prefix) {
            return (host, Some(repo));
        }
    }
    (PinReleaseHost::GitHub, None)
}

fn extract_repository_for_prefix(text: &str, prefix: &str) -> Option<String> {
    let mut search = 0;
    while let Some(found) = text[search..].find(prefix) {
        let start = search + found + prefix.len();
        search = start;
        let rest = &text[start..];
        let Some((repo, after)) = rest.split_once("/releases/download/") else {
            continue;
        };
        if after.contains("velocity-graph-") && repo.split('/').count() == 2 {
            return Some(repo.to_owned());
        }
    }
    None
}

fn extract_sha256_token(text: &str) -> Option<String> {
    text.split(|ch: char| !ch.is_ascii_alphanumeric())
        .find(|token| is_sha256(token))
        .map(str::to_ascii_lowercase)
}

fn pin_from_variables(
    variables: &BTreeMap<String, String>,
) -> Option<(PinReleaseHost, Option<String>, String, Option<String>)> {
    for value in variables.values() {
        if let Some((host, repository, version)) = extract_archive_pin(value) {
            return Some((host, repository, version, named_sha256_variable(variables)));
        }
    }
    for (name, value) in variables {
        let upper = name.to_ascii_uppercase();
        let names_indexer = upper.contains("VELOCITY_GRAPH")
            || (upper.contains("GRAPH") && upper.contains("INDEXER"));
        let names_version = upper.contains("VERSION") || upper.contains("TAG");
        if names_indexer && names_version {
            let trimmed = value.trim().strip_prefix('v').unwrap_or(value.trim());
            if parse_release_version(trimmed).is_some() {
                return Some((
                    PinReleaseHost::GitHub,
                    None,
                    trimmed.to_owned(),
                    named_sha256_variable(variables),
                ));
            }
        }
    }
    None
}

fn named_sha256_variable(variables: &BTreeMap<String, String>) -> Option<String> {
    variables.iter().find_map(|(name, value)| {
        let upper = name.to_ascii_uppercase();
        let names_indexer = upper.contains("VELOCITY_GRAPH")
            || (upper.contains("GRAPH") && upper.contains("INDEXER"))
            || upper.contains("INDEXER");
        (names_indexer && upper.contains("SHA") && is_sha256(value.trim()))
            .then(|| value.trim().to_ascii_lowercase())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_SHA_OLD: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const SAMPLE_SHA_NEW: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    const GIT_SHA: &str = "1111111111111111111111111111111111111111111111111111111111111111";

    // Risk (#577): Collapse pins both `git` and `velocity-graph` in `ops/Dockerfile`; parsing
    // must extract the `velocity-graph` version and SHA-256 without grabbing `git`'s pin.
    #[test]
    fn parses_dockerfile_args_without_confusing_git_pin() {
        let dockerfile = format!(
            r#"
FROM debian:bookworm-slim
ARG GIT_VERSION=2.49.0
ARG GIT_SHA256={GIT_SHA}
RUN curl -fsSL "https://mirrors.edge.kernel.org/pub/software/scm/git/git-${{GIT_VERSION}}.tar.xz" -o /tmp/git.tar.xz \
 && echo "${{GIT_SHA256}}  /tmp/git.tar.xz" | sha256sum -c -

ARG VELOCITY_GRAPH_VERSION=0.1.0-beta.17
ARG VELOCITY_GRAPH_SHA256={SAMPLE_SHA_OLD}
RUN curl -fsSL \
      "https://github.com/mESmaC/velocity/releases/download/v${{VELOCITY_GRAPH_VERSION}}/velocity-graph-${{VELOCITY_GRAPH_VERSION}}-x86_64-unknown-linux-gnu.tar.gz" \
      -o /tmp/velocity-graph.tar.gz \
 && echo "${{VELOCITY_GRAPH_SHA256}}  /tmp/velocity-graph.tar.gz" | sha256sum -c - \
 && tar -xzf /tmp/velocity-graph.tar.gz -C /tmp
"#
        );
        let pin = CollapseGraphPin::parse(&dockerfile);
        assert_eq!(
            pin,
            Some(CollapseGraphPin {
                repository: Some("mESmaC/velocity".into()),
                host: PinReleaseHost::GitHub,
                version: "0.1.0-beta.17".into(),
                tag: "v0.1.0-beta.17".into(),
                sha256: Some(SAMPLE_SHA_OLD.into()),
            })
        );
    }

    #[test]
    fn parses_collapse_release_url_in_dockerfile() {
        let dockerfile = format!(
            r#"
ARG VELOCITY_GRAPH_VERSION=0.1.0-beta.21
ARG VELOCITY_GRAPH_SHA256={SAMPLE_SHA_NEW}
RUN curl -fsSL \
      "https://api.collapse.sh/mESmaC/velocity/releases/download/v${{VELOCITY_GRAPH_VERSION}}/velocity-graph-${{VELOCITY_GRAPH_VERSION}}-x86_64-unknown-linux-gnu.tar.gz" \
      -o /tmp/velocity-graph.tar.gz \
 && echo "${{VELOCITY_GRAPH_SHA256}}  /tmp/velocity-graph.tar.gz" | sha256sum -c -
"#
        );
        let pin = CollapseGraphPin::parse(&dockerfile);
        assert_eq!(
            pin,
            Some(CollapseGraphPin {
                repository: Some("mESmaC/velocity".into()),
                host: PinReleaseHost::Collapse,
                version: "0.1.0-beta.21".into(),
                tag: "v0.1.0-beta.21".into(),
                sha256: Some(SAMPLE_SHA_NEW.into()),
            })
        );
    }

    #[test]
    fn parses_shell_variables_and_inline_url_in_run() {
        let dockerfile = format!(
            r#"
RUN set -euo pipefail \
 && version="0.1.0-beta.17" \
 && sha256="{SAMPLE_SHA_OLD}" \
 && archive="velocity-graph-${{version}}-x86_64-unknown-linux-gnu.tar.gz" \
 && curl -fsSL "https://github.com/mESmaC/velocity/releases/download/v${{version}}/${{archive}}" -o "/tmp/${{archive}}" \
 && echo "${{sha256}}  /tmp/${{archive}}" | sha256sum -c -
"#
        );
        let pin = CollapseGraphPin::parse(&dockerfile);
        assert_eq!(
            pin,
            Some(CollapseGraphPin {
                repository: Some("mESmaC/velocity".into()),
                host: PinReleaseHost::GitHub,
                version: "0.1.0-beta.17".into(),
                tag: "v0.1.0-beta.17".into(),
                sha256: Some(SAMPLE_SHA_OLD.into()),
            })
        );
    }

    // Risk (#577): `parse_u32_const` must skip unrelated constants preceding `PARSER_VERSION`
    // or `SCHEMA`, and `parse_sha256` must reject digests that name another archive.
    #[test]
    fn parses_parser_schema_and_sha256_files() {
        assert_eq!(
            parse_parser_version(
                "const OTHER: u32 = 42;\n// pub const PARSER_VERSION: u32 = 1;\npub const PARSER_VERSION: u32 = 9;\n"
            ),
            Some(9)
        );
        assert_eq!(
            parse_schema_version("pub const SCHEMA: u32 = 2;\n"),
            Some(2)
        );
        let archive = archive_name("0.1.0-beta.21");
        assert_eq!(
            parse_sha256(&format!("{SAMPLE_SHA_NEW}  {archive}\n"), &archive),
            Some(SAMPLE_SHA_NEW.into())
        );
        assert_eq!(
            parse_sha256(&format!("{SAMPLE_SHA_NEW}  other.tar.gz\n"), &archive),
            None
        );
    }

    #[test]
    fn selects_highest_semver_release_with_archive_and_checksum() {
        let release = |tag: &str, draft: bool, with_checksum: bool| {
            let version = tag.trim_start_matches('v');
            let archive = archive_name(version);
            let mut assets = vec![ReleaseAssetCandidate {
                id: 1,
                name: archive.clone(),
                download_url: format!(
                    "https://github.com/mESmaC/velocity/releases/download/{tag}/{archive}"
                ),
            }];
            if with_checksum {
                assets.push(ReleaseAssetCandidate {
                    id: 2,
                    name: checksum_name(version),
                    download_url: format!("https://github.com/mESmaC/velocity/releases/download/{tag}/{archive}.sha256"),
                });
            }
            ReleaseCandidate {
                tag: tag.into(),
                draft,
                assets,
            }
        };
        let releases = vec![
            release("updater", false, true),
            release("v0.1.0-beta.22", true, true),
            release("v0.1.0-beta.21", false, false),
            release("v0.1.0-beta.19", false, true),
            release("v0.1.0-beta.20", false, true),
        ];
        let chosen = latest_indexer_release(&releases);
        assert_eq!(
            chosen.as_ref().map(|item| item.version.as_str()),
            Some("0.1.0-beta.20")
        );
    }

    #[test]
    fn triggers_bump_only_on_newer_release_with_parser_or_schema_change() {
        let same = IndexerVersion {
            parser: 7,
            schema: 1,
        };
        let new_parser = IndexerVersion {
            parser: 9,
            schema: 1,
        };
        let new_schema = IndexerVersion {
            parser: 7,
            schema: 2,
        };

        assert!(!should_bump("0.1.0-beta.17", same, "0.1.0-beta.18", same));
        assert!(should_bump(
            "0.1.0-beta.17",
            same,
            "0.1.0-beta.21",
            new_parser
        ));
        assert!(should_bump(
            "0.1.0-beta.17",
            same,
            "0.1.0-beta.21",
            new_schema
        ));
        assert!(!should_bump(
            "0.1.0-beta.21",
            new_parser,
            "0.1.0-beta.21",
            new_parser
        ));
        assert!(!should_bump(
            "0.1.0-beta.21",
            new_parser,
            "0.1.0-beta.17",
            same
        ));
    }

    #[test]
    fn plans_open_keep_rewrite_finished_and_duplicate_closure() {
        let bump = GraphPinBump {
            velocity_repository: "mESmaC/velocity".into(),
            pin: CollapseGraphPin {
                repository: Some("mESmaC/velocity".into()),
                host: PinReleaseHost::GitHub,
                version: "0.1.0-beta.17".into(),
                tag: "v0.1.0-beta.17".into(),
                sha256: Some(SAMPLE_SHA_OLD.into()),
            },
            pin_indexer: IndexerVersion { parser: 7, schema: 1 },
            target_version: "0.1.0-beta.21".into(),
            target_tag: "v0.1.0-beta.21".into(),
            target_indexer: IndexerVersion { parser: 9, schema: 1 },
            archive_url: "https://github.com/mESmaC/velocity/releases/download/v0.1.0-beta.21/velocity-graph-0.1.0-beta.21-x86_64-unknown-linux-gnu.tar.gz".into(),
            sha256: SAMPLE_SHA_NEW.into(),
        };

        assert_eq!(plan(&[], &bump).step, PinBumpStep::Open);

        let current_issue = OpenIssue {
            repository: "mESmaC/collapse.sh".into(),
            number: 12,
            title: bump.title(),
            body: bump.body(),
        };
        assert_eq!(
            plan(std::slice::from_ref(&current_issue), &bump),
            PinBumpPlan {
                step: PinBumpStep::Keep(12),
                finished: vec![],
                duplicates: vec![],
            }
        );

        let finished_issue = OpenIssue {
            repository: "mESmaC/collapse.sh".into(),
            number: 9,
            title: format!("{PIN_BUMP_TITLE}0.1.0-beta.17"),
            body: "Old bump\n\n<!-- velocity-graph-pin-bump:0.1.0-beta.17 -->".into(),
        };
        let stale_issue = OpenIssue {
            repository: "mESmaC/collapse.sh".into(),
            number: 10,
            title: format!("{PIN_BUMP_TITLE}0.1.0-beta.20"),
            body: "Older target\n\n<!-- velocity-graph-pin-bump:0.1.0-beta.20 -->".into(),
        };
        let duplicate_issue = OpenIssue {
            repository: "mESmaC/collapse.sh".into(),
            number: 15,
            title: format!("{PIN_BUMP_TITLE}0.1.0-beta.20"),
            body: "Duplicate\n\n<!-- velocity-graph-pin-bump:0.1.0-beta.20 -->".into(),
        };
        assert_eq!(
            plan(&[duplicate_issue, finished_issue, stale_issue], &bump),
            PinBumpPlan {
                step: PinBumpStep::Rewrite(10),
                finished: vec![9],
                duplicates: vec![15],
            }
        );
    }
}
