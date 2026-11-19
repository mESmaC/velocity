//! Cutting a release for a landed commit (#431).
//!
//! The release workflow creates the tag itself, so a release needs no tag pushed to
//! GitHub and works with any mirror forwarder. Velocity checks the version here so a
//! refused tag never starts a workflow run.

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

/// Whether `text` is a full 40-character commit ID, the form the workflow checks out.
pub fn is_full_commit(text: &str) -> bool {
    text.len() == 40 && text.bytes().all(|byte| byte.is_ascii_hexdigit())
}
