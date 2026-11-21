//! Cutting a release for a landed commit (#431) and comparing published releases against
//! the updater manifest (#521).
//!
//! The release workflow creates the tag itself, so a release needs no tag pushed to
//! GitHub and works with any mirror forwarder. Velocity checks the version here so a
//! refused tag never starts a workflow run.

use std::cmp::Ordering;
use std::collections::BTreeMap;

use serde::Serialize;

/// A parsed SemVer 2.0.0 release version, ordered by SemVer precedence (#521).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReleaseVersion {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
    prerelease: Vec<PrereleaseIdentifier>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum PrereleaseIdentifier {
    Numeric(u64),
    Alpha(String),
}

impl Ord for PrereleaseIdentifier {
    fn cmp(&self, other: &Self) -> Ordering {
        match (self, other) {
            (Self::Numeric(left), Self::Numeric(right)) => left.cmp(right),
            (Self::Numeric(_), Self::Alpha(_)) => Ordering::Less,
            (Self::Alpha(_), Self::Numeric(_)) => Ordering::Greater,
            (Self::Alpha(left), Self::Alpha(right)) => left.cmp(right),
        }
    }
}

impl PartialOrd for PrereleaseIdentifier {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for ReleaseVersion {
    fn cmp(&self, other: &Self) -> Ordering {
        (self.major, self.minor, self.patch)
            .cmp(&(other.major, other.minor, other.patch))
            .then_with(
                || match (self.prerelease.is_empty(), other.prerelease.is_empty()) {
                    (true, true) => Ordering::Equal,
                    (true, false) => Ordering::Greater,
                    (false, true) => Ordering::Less,
                    (false, false) => self.prerelease.cmp(&other.prerelease),
                },
            )
    }
}

impl PartialOrd for ReleaseVersion {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// Summary of a GitHub release checked against `updater/latest.json` (#521).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PublishedRelease {
    pub tag: String,
    pub version: String,
    pub draft: bool,
    /// True when the release carries signed updater assets (`latest.json` and `.sig`).
    pub signed: bool,
}

/// The updater manifest lags behind a newer published GitHub release (#521).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ManifestLag {
    pub manifest_version: String,
    pub latest_release: String,
    pub latest_tag: String,
    pub latest_signed: bool,
}

/// How project health recovers a refused `release.yml` gate once a green integration
/// commit covers it (#549).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RefusedGateRecovery {
    /// Re-dispatch the refused tag on its own commit because that exact commit is green.
    RedispatchRefusedTag { tag: String },
    /// Dispatch the next free beta tag on the newer green commit and leave the refused
    /// tag untouched, so the fast beta path runs on the green tree instead of running the
    /// stress gate on the stale commit.
    ReleaseGreenBeta { tag: String },
    /// Integration passed, but Velocity should not dispatch automatically (no tag on the
    /// refused run, or a non-beta tag whose exact commit is not the green commit).
    ReportReady,
}

/// The `v<semver>` tag a release of `version` is published under, or `None` when the
/// release workflow would refuse it. A leading `v` is accepted. The pre-release part
/// matches the workflow's own check: ASCII letters, digits, dots and hyphens.
pub fn release_tag(version: &str) -> Option<String> {
    let version = version.trim();
    let version = version.strip_prefix('v').unwrap_or(version);
    let (core, prerelease) = match version.split_once('-') {
        Some((core, prerelease)) => (core, Some(prerelease)),
        None => (version, None),
    };
    let numbers: Vec<&str> = core.split('.').collect();
    let numeric = numbers.len() == 3
        && numbers
            .iter()
            .all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()));
    let prerelease_valid = prerelease.is_none_or(|part| {
        !part.is_empty()
            && part
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'.' || byte == b'-')
    });
    (numeric && prerelease_valid).then(|| format!("v{version}"))
}

/// Parses a SemVer string (with optional leading `v` and optional `+build` metadata).
pub fn parse_release_version(text: &str) -> Option<ReleaseVersion> {
    let trimmed = text.trim();
    let without_prefix = trimmed.strip_prefix('v').unwrap_or(trimmed);
    let without_build = without_prefix
        .split_once('+')
        .map_or(without_prefix, |(before, _)| before);
    let (core, prerelease_text) = match without_build.split_once('-') {
        Some((core, prerelease)) => (core, Some(prerelease)),
        None => (without_build, None),
    };
    let mut parts = core.split('.');
    let major = parse_numeric_part(parts.next()?)?;
    let minor = parse_numeric_part(parts.next()?)?;
    let patch = parse_numeric_part(parts.next()?)?;
    if parts.next().is_some() {
        return None;
    }
    let prerelease = match prerelease_text {
        None => Vec::new(),
        Some(text) => parse_prerelease(text)?,
    };
    Some(ReleaseVersion {
        major,
        minor,
        patch,
        prerelease,
    })
}

fn parse_numeric_part(part: &str) -> Option<u64> {
    if part.is_empty() || !part.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    part.parse().ok()
}

fn parse_prerelease(text: &str) -> Option<Vec<PrereleaseIdentifier>> {
    let mut identifiers = Vec::new();
    for segment in text.split('.') {
        if segment.is_empty()
            || !segment
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        {
            return None;
        }
        if segment.bytes().all(|byte| byte.is_ascii_digit()) {
            identifiers.push(PrereleaseIdentifier::Numeric(segment.parse().ok()?));
        } else {
            identifiers.push(PrereleaseIdentifier::Alpha(segment.to_owned()));
        }
    }
    Some(identifiers)
}

/// Compares two SemVer strings by SemVer 2.0.0 precedence.
pub fn compare_versions(left: &str, right: &str) -> Option<Ordering> {
    Some(parse_release_version(left)?.cmp(&parse_release_version(right)?))
}

/// Extracts the normalized version from a `v<semver>` release tag, ignoring non-version
/// tags such as `updater`.
pub fn version_from_release_tag(tag: &str) -> Option<String> {
    let trimmed = tag.trim();
    let stripped = trimmed.strip_prefix('v')?;
    parse_release_version(stripped)?;
    Some(stripped.to_owned())
}

/// Returns `Some(ManifestLag)` when any non-draft published release has a strictly higher
/// SemVer than `manifest_version` (#521).
pub fn detect_manifest_lag(
    manifest_version: &str,
    releases: &[PublishedRelease],
) -> Option<ManifestLag> {
    let manifest_parsed = parse_release_version(manifest_version)?;
    let newest = releases
        .iter()
        .filter(|release| !release.draft)
        .filter_map(|release| Some((parse_release_version(&release.version)?, release)))
        .max_by(|(left, _), (right, _)| left.cmp(right))?;
    if newest.0 <= manifest_parsed {
        return None;
    }
    let release = newest.1;
    Some(ManifestLag {
        manifest_version: manifest_version.trim().trim_start_matches('v').to_owned(),
        latest_release: release.version.clone(),
        latest_tag: release.tag.clone(),
        latest_signed: release.signed,
    })
}

/// Whether `text` is a full 40-character commit ID, the form the workflow checks out.
pub fn is_full_commit(text: &str) -> bool {
    text.len() == 40 && text.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// Whether `tag` is a `v<semver>` beta tag that takes `release.yml`'s beta fast path
/// when its exact commit is green (`^v\d+\.\d+\.\d+-beta(\.|$)`) (#549).
pub fn is_beta_release_tag(tag: &str) -> bool {
    let Some(version) = version_from_release_tag(tag).and_then(|v| parse_release_version(&v))
    else {
        return false;
    };
    matches!(
        version.prerelease.first(),
        Some(PrereleaseIdentifier::Alpha(label)) if label == "beta"
    )
}

/// Computes the next free `v<major>.<minor>.<patch>-beta.<n>` tag strictly above
/// `refused_tag` and every existing `v<semver>` tag or release, returning `None` when
/// `refused_tag` is not a beta tag (#549).
pub fn next_free_beta_tag(refused_tag: &str, existing_tags: &[String]) -> Option<String> {
    if !is_beta_release_tag(refused_tag) {
        return None;
    }
    let refused_version = version_from_release_tag(refused_tag)
        .and_then(|version| parse_release_version(&version))?;
    let highest = existing_tags
        .iter()
        .filter_map(|tag| version_from_release_tag(tag))
        .filter_map(|version| parse_release_version(&version))
        .chain(std::iter::once(refused_version))
        .max()?;
    let (major, minor, patch, mut number) = next_beta_coordinates(&highest)?;
    loop {
        let candidate = format!("v{major}.{minor}.{patch}-beta.{number}");
        if !existing_tags.iter().any(|tag| tag == &candidate) {
            return Some(candidate);
        }
        number = number.checked_add(1)?;
    }
}

fn next_beta_coordinates(highest: &ReleaseVersion) -> Option<(u64, u64, u64, u64)> {
    match highest.prerelease.first() {
        None => Some((
            highest.major,
            highest.minor,
            highest.patch.checked_add(1)?,
            1,
        )),
        Some(PrereleaseIdentifier::Alpha(label)) if label == "beta" => {
            match highest.prerelease.get(1) {
                None => Some((highest.major, highest.minor, highest.patch, 1)),
                Some(PrereleaseIdentifier::Numeric(number)) => Some((
                    highest.major,
                    highest.minor,
                    highest.patch,
                    number.checked_add(1)?,
                )),
                Some(PrereleaseIdentifier::Alpha(_)) => Some((
                    highest.major,
                    highest.minor,
                    highest.patch.checked_add(1)?,
                    1,
                )),
            }
        }
        Some(PrereleaseIdentifier::Numeric(_)) => {
            Some((highest.major, highest.minor, highest.patch, 1))
        }
        Some(PrereleaseIdentifier::Alpha(label)) if label.as_str() < "beta" => {
            Some((highest.major, highest.minor, highest.patch, 1))
        }
        Some(PrereleaseIdentifier::Alpha(_)) => Some((
            highest.major,
            highest.minor,
            highest.patch.checked_add(1)?,
            1,
        )),
    }
}

/// Decides how project health should recover a refused `release.yml` gate once
/// `green_commit` carries a green `velocity/integration` status (#549).
pub fn plan_refused_gate_recovery(
    refused_tag: Option<&str>,
    refused_commit: &str,
    green_commit: &str,
    existing_tags: &[String],
) -> RefusedGateRecovery {
    let Some(tag) = refused_tag else {
        return RefusedGateRecovery::ReportReady;
    };
    if green_commit.eq_ignore_ascii_case(refused_commit) {
        return RefusedGateRecovery::RedispatchRefusedTag {
            tag: tag.to_owned(),
        };
    }
    if let Some(next_tag) = next_free_beta_tag(tag, existing_tags) {
        return RefusedGateRecovery::ReleaseGreenBeta { tag: next_tag };
    }
    RefusedGateRecovery::ReportReady
}

/// One signed download entry in Tauri's `latest.json` updater manifest (#593).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct UpdaterPlatform {
    pub url: String,
    pub signature: String,
}

/// Tauri updater manifest (`updater/latest.json`) published alongside signed releases (#593).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct UpdaterManifest {
    pub version: String,
    pub notes: String,
    pub pub_date: String,
    pub platforms: BTreeMap<String, UpdaterPlatform>,
}

/// Builds the Tauri updater manifest (`latest.json`) for a Windows release published to
/// `download_base` (e.g. `https://api.collapse.sh/mESmaC/velocity/releases/download/v0.1.0-beta.21`)
/// matching the exact platform keys Tauri's updater expects (#593).
pub fn build_updater_manifest(
    version: &str,
    notes: &str,
    pub_date: &str,
    download_base: &str,
    setup_signature: &str,
    portable_signature: &str,
    msi_signature: Option<&str>,
) -> UpdaterManifest {
    let base = download_base.trim_end_matches('/');
    let setup = UpdaterPlatform {
        url: format!("{base}/Velocity_{version}_x64-setup.exe"),
        signature: setup_signature.trim().to_owned(),
    };
    let mut platforms = BTreeMap::new();
    platforms.insert("windows-x86_64".to_owned(), setup.clone());
    platforms.insert("windows-x86_64-nsis".to_owned(), setup);
    platforms.insert(
        "windows-x86_64-portable".to_owned(),
        UpdaterPlatform {
            url: format!("{base}/Velocity_{version}_x64-portable.exe"),
            signature: portable_signature.trim().to_owned(),
        },
    );
    if let Some(signature) = msi_signature.map(str::trim).filter(|text| !text.is_empty()) {
        platforms.insert(
            "windows-x86_64-msi".to_owned(),
            UpdaterPlatform {
                url: format!("{base}/Velocity_{version}_x64_en-US.msi"),
                signature: signature.to_owned(),
            },
        );
    }
    UpdaterManifest {
        version: version.trim().trim_start_matches('v').to_owned(),
        notes: notes.trim().to_owned(),
        pub_date: pub_date.trim().to_owned(),
        platforms,
    }
}

/// Whether `tag` names a prerelease (`v0.1.0-beta.1`), as opposed to a numbered release (#593).
pub fn is_prerelease_tag(tag: &str) -> bool {
    version_from_release_tag(tag)
        .and_then(|version| parse_release_version(&version))
        .is_some_and(|parsed| !parsed.prerelease.is_empty())
}
