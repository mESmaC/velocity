//! Whether an agent's request to run a GitHub workflow is allowed (#508). Agents have no
//! `gh`, so Velocity dispatches for them, and only workflows the project owner lists under
//! `[ci] dispatchable` in the base's `rosen.toml`. The list names files, so a request is
//! reduced to a file name first and compared exactly.

use std::collections::BTreeMap;
use std::fmt;

/// How a requested run's row in the thread starts. The row stays running for as long as
/// GitHub takes, and the stall check reads this to know the thread is waiting, not silent.
pub const RUN_TITLE: &str = "GitHub workflow ";
/// How a requested Rosen check-set run's row in the thread starts (#591).
pub const ROSEN_CHECK_TITLE: &str = "Rosen check ";
/// Where GitHub reads workflow files, which a request or a list entry may spell out.
const WORKFLOW_FOLDER: &str = ".github/workflows/";
/// GitHub refuses a `workflow_dispatch` with more inputs than this.
const MAXIMUM_INPUTS: usize = 25;
/// Longer than any branch or tag GitHub accepts.
const MAXIMUM_REFERENCE_BYTES: usize = 255;

/// Why a workflow request was not started, worded for the agent that asked.
#[derive(Debug, PartialEq, Eq)]
pub enum WorkflowRefusal {
    NotWorkflowFile(String),
    NotDispatchable {
        workflow: String,
        /// The workflow files the list does allow.
        allowed: Vec<String>,
    },
    InvalidReference(String),
    TooManyInputs,
    InvalidInput(String),
}

impl fmt::Display for WorkflowRefusal {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotWorkflowFile(name) => write!(
                formatter,
                "`{name}` is not a workflow file name. Pass the file under .github/workflows, such as `ci.yml`."
            ),
            Self::NotDispatchable { workflow, allowed } => {
                let allows = if allowed.is_empty() {
                    "allows no workflows".to_owned()
                } else {
                    format!("allows only {}", allowed.join(", "))
                };
                write!(
                    formatter,
                    "`{workflow}` is not on the owner's allowlist: `[ci] dispatchable` in the base's rosen.toml {allows}. Only the project owner changes that list."
                )
            }
            Self::InvalidReference(reference) => {
                write!(formatter, "`{reference}` is not a branch or tag name")
            }
            Self::TooManyInputs => {
                write!(
                    formatter,
                    "A workflow takes at most {MAXIMUM_INPUTS} inputs"
                )
            }
            Self::InvalidInput(name) => write!(formatter, "`{name}` is not a workflow input name"),
        }
    }
}

impl std::error::Error for WorkflowRefusal {}

/// The workflow file to dispatch when `dispatchable` allows the request.
pub fn admit(
    dispatchable: &[String],
    workflow: &str,
    reference: Option<&str>,
    inputs: &BTreeMap<String, String>,
) -> Result<String, WorkflowRefusal> {
    let file = workflow_file(workflow)
        .ok_or_else(|| WorkflowRefusal::NotWorkflowFile(workflow.trim().to_owned()))?;
    let allowed: Vec<String> = dispatchable
        .iter()
        .filter_map(|entry| workflow_file(entry))
        .collect();
    if !allowed.contains(&file) {
        return Err(WorkflowRefusal::NotDispatchable {
            workflow: file,
            allowed,
        });
    }
    if let Some(reference) = reference
        && !is_reference(reference)
    {
        return Err(WorkflowRefusal::InvalidReference(reference.to_owned()));
    }
    if inputs.len() > MAXIMUM_INPUTS {
        return Err(WorkflowRefusal::TooManyInputs);
    }
    if let Some(name) = inputs.keys().find(|name| !is_input_name(name)) {
        return Err(WorkflowRefusal::InvalidInput(name.clone()));
    }
    Ok(file)
}

/// `name` as a file directly under `.github/workflows`, or `None` when it is anything
/// else: a path elsewhere, a numeric workflow ID or a display name.
fn workflow_file(name: &str) -> Option<String> {
    let name = name.trim();
    let file = name.strip_prefix(WORKFLOW_FOLDER).unwrap_or(name);
    let named = file
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || b"-_.".contains(&byte));
    let extension = file.ends_with(".yml") || file.ends_with(".yaml");
    (named && extension && !file.starts_with(['.', '-'])).then(|| file.to_owned())
}

/// A branch or tag by Git's naming rules, so it is never read as an option or a range.
fn is_reference(reference: &str) -> bool {
    !reference.is_empty()
        && reference.len() <= MAXIMUM_REFERENCE_BYTES
        && !reference.starts_with(['-', '/'])
        && !reference.ends_with(['/', '.'])
        && !reference.contains("..")
        && !reference.contains("@{")
        && !reference.chars().any(|character| {
            character.is_whitespace() || character.is_control() || "~^:?*[\\".contains(character)
        })
}

fn is_input_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-_".contains(&byte))
}

/// Which check set in `rosen.toml` a Rosen check-set request targets (#591).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RosenCheckKind {
    PreLand,
    PostLand,
}

impl RosenCheckKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::PreLand => "pre-land",
            Self::PostLand => "post-land",
        }
    }
}

/// Summary of one configured check set in `rosen.toml` for pure admission decisions (#591).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfiguredCheckSet {
    pub kind: RosenCheckKind,
    pub name: String,
    pub platforms: Vec<String>,
    pub has_commands: bool,
}

/// Admitted Rosen check-set request resolved against the base `rosen.toml` (#591).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AdmittedRosenCheck {
    pub kind: RosenCheckKind,
    pub check_set: String,
    pub platform: Option<String>,
    pub reference: Option<String>,
}

/// Why a Rosen check-set request was refused, worded for the agent (#591).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RosenCheckRefusal {
    UnknownCheckSet {
        requested: String,
        available: Vec<String>,
    },
    EmptyCheckSet(String),
    UnknownPlatform {
        requested: String,
        allowed: Vec<String>,
    },
    PlatformRequired {
        check_set: String,
        allowed: Vec<String>,
    },
    InvalidReference(String),
}

impl fmt::Display for RosenCheckRefusal {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownCheckSet {
                requested,
                available,
            } => write!(
                formatter,
                "`{requested}` is not a configured Rosen check set in the base's rosen.toml. Available check sets: {}.",
                available.join(", ")
            ),
            Self::EmptyCheckSet(name) => write!(
                formatter,
                "Check set `{name}` has no commands configured in the base's rosen.toml."
            ),
            Self::UnknownPlatform { requested, allowed } => write!(
                formatter,
                "Platform `{requested}` is not configured for this check set in the base's rosen.toml. Configured platforms: {}.",
                allowed.join(", ")
            ),
            Self::PlatformRequired { check_set, allowed } => write!(
                formatter,
                "Check set `{check_set}` configures multiple platforms ({}); pass `platform` to choose one.",
                allowed.join(", ")
            ),
            Self::InvalidReference(reference) => {
                write!(formatter, "`{reference}` is not a valid commit or ref name")
            }
        }
    }
}

impl std::error::Error for RosenCheckRefusal {}

/// Resolves a requested check set name and optional platform against the base's configured
/// Rosen check sets (#591). `host_os` is `"windows"`, `"macos"`, or `"linux"`.
pub fn admit_rosen_check_set(
    sets: &[ConfiguredCheckSet],
    requested: &str,
    platform: Option<&str>,
    reference: Option<&str>,
    host_os: &str,
) -> Result<AdmittedRosenCheck, RosenCheckRefusal> {
    let trimmed = requested.trim();
    let matched = sets
        .iter()
        .find(|set| check_set_matches(set, trimmed))
        .ok_or_else(|| RosenCheckRefusal::UnknownCheckSet {
            requested: trimmed.to_owned(),
            available: sets.iter().map(|set| set.name.clone()).collect(),
        })?;
    if !matched.has_commands {
        return Err(RosenCheckRefusal::EmptyCheckSet(matched.name.clone()));
    }
    if let Some(reference) = reference
        && !is_reference(reference)
    {
        return Err(RosenCheckRefusal::InvalidReference(reference.to_owned()));
    }
    let resolved_platform = resolve_rosen_platform(matched, platform, host_os)?;
    Ok(AdmittedRosenCheck {
        kind: matched.kind,
        check_set: matched.name.clone(),
        platform: resolved_platform,
        reference: reference.map(str::to_owned),
    })
}

fn check_set_matches(set: &ConfiguredCheckSet, requested: &str) -> bool {
    if requested.is_empty() {
        return false;
    }
    if set.name.eq_ignore_ascii_case(requested) {
        return true;
    }
    match set.kind {
        RosenCheckKind::PreLand => {
            requested.eq_ignore_ascii_case("pre-land") || requested.eq_ignore_ascii_case("pre_land")
        }
        RosenCheckKind::PostLand => {
            requested.eq_ignore_ascii_case("post-land")
                || requested.eq_ignore_ascii_case("post_land")
        }
    }
}

fn resolve_rosen_platform(
    set: &ConfiguredCheckSet,
    requested: Option<&str>,
    host_os: &str,
) -> Result<Option<String>, RosenCheckRefusal> {
    if let Some(raw) = requested.map(str::trim).filter(|value| !value.is_empty()) {
        if set.platforms.is_empty() {
            return Ok(Some(raw.to_ascii_lowercase()));
        }
        let lower = raw.to_ascii_lowercase();
        if let Some(exact) = set
            .platforms
            .iter()
            .find(|configured| configured.eq_ignore_ascii_case(&lower))
        {
            return Ok(Some(exact.clone()));
        }
        let requested_os = lower.split('-').next().unwrap_or(&lower);
        if let Some(by_os) = set
            .platforms
            .iter()
            .find(|configured| configured.eq_ignore_ascii_case(requested_os))
        {
            return Ok(Some(by_os.clone()));
        }
        return Err(RosenCheckRefusal::UnknownPlatform {
            requested: raw.to_owned(),
            allowed: set.platforms.clone(),
        });
    }
    match set.platforms.as_slice() {
        [] => Ok(None),
        [only] => Ok(Some(only.clone())),
        many => {
            if let Some(host_match) = many
                .iter()
                .find(|configured| configured.split('-').next() == Some(host_os))
            {
                Ok(Some(host_match.clone()))
            } else {
                Err(RosenCheckRefusal::PlatformRequired {
                    check_set: set.name.clone(),
                    allowed: set.platforms.clone(),
                })
            }
        }
    }
}
