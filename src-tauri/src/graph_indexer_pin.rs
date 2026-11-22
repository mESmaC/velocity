//! Keeping Collapse's pinned `velocity-graph` indexer current (SPEC.md F19 "Standalone
//! indexer", #577). Mirrors `rosen_pin.rs` (#513): in a release build on a machine with a
//! Velocity or Collapse Rosen project whose signed-in account may push to Collapse's
//! repository, this edge checks every ten minutes whether a published Velocity release
//! changes `velocity-graph`'s parser or schema version and keeps exactly one open pin-bump
//! issue on Collapse. No thread, agent session or window is involved.
use std::time::Duration;

use tauri::{AppHandle, Manager};
use velocity_collapse::releases as collapse_releases;
use velocity_core::graph_indexer_pin::{
    ReleaseAssetCandidate, ReleaseCandidate, checksum_name, parse_sha256,
};
use velocity_github::IssueHost;
use velocity_github::graph_indexer_pin::{
    COLLAPSE_REPOSITORIES, GraphIndexerPinWatch, GraphPinCache, IssueLook, PinIssue,
};
use velocity_github::velocity_reports::VELOCITY_REPOSITORY;

use crate::state::ManagedStateStore;

/// Wait until startup work and resumed threads settle, offset slightly from `rosen_pin`.
const FIRST_LOOK: Duration = Duration::from_secs(2 * 60 + 15);
const LOOK_INTERVAL: Duration = Duration::from_secs(10 * 60);

#[derive(Clone, Debug, PartialEq, Eq)]
struct CollapsePinTarget {
    host: IssueHost,
    velocity_repository: String,
    collapse_repository: String,
}

/// What the looks remember between them. Nothing is saved: a restart checks again.
#[derive(Default)]
struct Watch {
    /// Resolved Collapse issue-host target that the signed-in account maintains.
    target: Option<CollapsePinTarget>,
    /// Cached immutable release metadata (parser/schema versions and SHA-256 assets).
    cache: GraphPinCache,
    /// The failure last written to the log, so a failure that repeats is written once.
    last_failure: Option<String>,
}

pub fn initialize(application: &AppHandle) {
    // A development build is not the copy the owner runs: it files nothing.
    if cfg!(debug_assertions) {
        return;
    }
    let application = application.clone();
    std::thread::spawn(move || {
        let mut watch = Watch::default();
        std::thread::sleep(FIRST_LOOK);
        loop {
            let candidates = collapse_candidates(&application);
            if !candidates.is_empty() {
                look(&mut watch, &candidates);
            }
            std::thread::sleep(LOOK_INTERVAL);
        }
    });
}

/// Ordered Collapse repository candidates with their configured issue host when this machine
/// has a Rosen project for Velocity or Collapse. Empty when neither project is present (#590).
fn collapse_candidates(application: &AppHandle) -> Vec<CollapsePinTarget> {
    let Some(store) = application.try_state::<ManagedStateStore>() else {
        return Vec::new();
    };
    let Ok(store) = store.lock() else {
        return Vec::new();
    };
    let mut configured = Vec::new();
    let mut relevant = false;
    let mut default_host = IssueHost::GitHub;
    let mut velocity_repository = VELOCITY_REPOSITORY.to_owned();
    for project in &store.state().projects {
        if !project.rosen_in_use() {
            continue;
        }
        let is_velocity = project
            .collapse_repository
            .as_deref()
            .is_some_and(|repo| repo.eq_ignore_ascii_case(VELOCITY_REPOSITORY))
            || project
                .github_remote
                .as_deref()
                .is_some_and(|repo| repo.eq_ignore_ascii_case(VELOCITY_REPOSITORY));
        if is_velocity {
            relevant = true;
            default_host = project.effective_issue_host();
            if let Some((_, repo)) = project.issue_host_repository() {
                velocity_repository = repo;
            }
        }
        let is_collapse = project
            .collapse_repository
            .as_deref()
            .is_some_and(is_collapse_repository)
            || project
                .github_remote
                .as_deref()
                .is_some_and(is_collapse_repository);
        if is_collapse {
            relevant = true;
            let host = project.effective_issue_host();
            if let Some((_, repo)) = project.issue_host_repository() {
                push_unique(
                    &mut configured,
                    CollapsePinTarget {
                        host,
                        velocity_repository: velocity_repository.clone(),
                        collapse_repository: repo,
                    },
                );
            }
        }
    }
    if !relevant {
        return Vec::new();
    }
    for entry in &mut configured {
        entry.velocity_repository = velocity_repository.clone();
    }
    for default_repo in COLLAPSE_REPOSITORIES {
        push_unique(
            &mut configured,
            CollapsePinTarget {
                host: default_host,
                velocity_repository: velocity_repository.clone(),
                collapse_repository: default_repo.to_owned(),
            },
        );
    }
    configured
}

fn is_collapse_repository(repository: &str) -> bool {
    COLLAPSE_REPOSITORIES
        .iter()
        .any(|known| repository.eq_ignore_ascii_case(known))
}

fn push_unique(list: &mut Vec<CollapsePinTarget>, candidate: CollapsePinTarget) {
    if !list.iter().any(|existing| {
        existing.host == candidate.host
            && existing
                .collapse_repository
                .eq_ignore_ascii_case(&candidate.collapse_repository)
    }) {
        list.push(candidate);
    }
}

fn look(watch: &mut Watch, candidates: &[CollapsePinTarget]) {
    let (done, failure) = match observe(watch, candidates) {
        Ok(Some((repository, look))) => (issue_notes(&repository, &look), None),
        Ok(None) => (Vec::new(), None),
        Err(error) => (
            Vec::new(),
            Some(format!("The Collapse indexer pin was not checked: {error}")),
        ),
    };
    for note in &done {
        crate::diagnostics::record("graph indexer pin", note);
    }
    if let Some(failure) = failure.as_deref().filter(|_| failure != watch.last_failure) {
        crate::diagnostics::record("graph indexer pin", failure);
    }
    watch.last_failure = failure;
}

/// One look, or `None` when the signed-in account does not maintain Collapse's repository.
fn observe(
    watch: &mut Watch,
    candidates: &[CollapsePinTarget],
) -> Result<Option<(String, IssueLook)>, velocity_github::GitHubCommandError> {
    if watch.target.as_ref().is_some_and(|saved| {
        !candidates.iter().any(|candidate| {
            candidate.host == saved.host
                && candidate
                    .collapse_repository
                    .eq_ignore_ascii_case(&saved.collapse_repository)
        })
    }) {
        watch.target = None;
    }
    let client = match watch.target.as_ref() {
        Some(target) => GraphIndexerPinWatch::for_host(
            target.host,
            &target.velocity_repository,
            &target.collapse_repository,
        )?,
        None => {
            let Some((target, client)) = resolve_maintainer_client(candidates)? else {
                return Ok(None);
            };
            watch.target = Some(target);
            client
        }
    };
    let repository = client.collapse_repository().to_owned();
    let result = client.look_with_collapse_releases(
        &mut watch.cache,
        read_collapse_releases,
        read_collapse_sha256,
    )?;
    Ok(Some((repository, result)))
}

/// Reads Velocity's published releases from Collapse so `ops/Dockerfile` pin-bump issues
/// point to Collapse's `velocity-graph` archive URL (#593).
fn read_collapse_releases(velocity_repository: &str) -> Option<Vec<ReleaseCandidate>> {
    let (owner, name) = velocity_core::collapse::parse_repository(velocity_repository)?;
    let token = velocity_collapse::load_credentials()
        .ok()
        .flatten()
        .map(|credentials| credentials.mcp_token);
    let page = tauri::async_runtime::block_on(collapse_releases::list_releases(
        token.as_deref(),
        &owner,
        &name,
        None,
    ))
    .ok()?;
    Some(
        page.releases
            .into_iter()
            .map(|release| {
                let tag = release.tag.clone();
                ReleaseCandidate {
                    tag: release.tag,
                    draft: release.draft,
                    assets: release
                        .assets
                        .into_iter()
                        .map(|asset| {
                            let download_url = asset.absolute_download_url(&owner, &name, &tag);
                            ReleaseAssetCandidate {
                                id: asset.id,
                                name: asset.name,
                                download_url,
                            }
                        })
                        .collect(),
                }
            })
            .collect(),
    )
}

fn read_collapse_sha256(release: &ReleaseCandidate, archive_name: &str) -> Option<String> {
    let version = velocity_core::release::version_from_release_tag(&release.tag)?;
    let expected_checksum = checksum_name(&version);
    let checksum_asset = release
        .assets
        .iter()
        .find(|asset| asset.name == expected_checksum)?;
    let token = velocity_collapse::load_credentials()
        .ok()
        .flatten()
        .map(|credentials| credentials.mcp_token);
    let text = tauri::async_runtime::block_on(collapse_releases::download_asset_text(
        token.as_deref(),
        &checksum_asset.download_url,
    ))
    .ok()
    .flatten()?;
    parse_sha256(&text, archive_name)
}

fn resolve_maintainer_client(
    candidates: &[CollapsePinTarget],
) -> Result<Option<(CollapsePinTarget, GraphIndexerPinWatch)>, velocity_github::GitHubCommandError>
{
    for candidate in candidates {
        let client = GraphIndexerPinWatch::for_host(
            candidate.host,
            &candidate.velocity_repository,
            &candidate.collapse_repository,
        )?;
        match client.can_maintain()? {
            Some(true) => return Ok(Some((candidate.clone(), client))),
            Some(false) => return Ok(None),
            None => continue,
        }
    }
    Ok(None)
}

fn issue_notes(repository: &str, look: &IssueLook) -> Vec<String> {
    let mut notes = Vec::new();
    match look.issue {
        PinIssue::Opened(number) => {
            notes.push(format!(
                "Opened {repository}#{number} to bump velocity-graph."
            ));
        }
        PinIssue::Rewritten(number) => {
            notes.push(format!(
                "Rewrote {repository}#{number} for a newer velocity-graph release."
            ));
        }
        PinIssue::Current | PinIssue::Kept(_) => {}
    }
    if !look.closed.is_empty() {
        let closed: Vec<String> = look
            .closed
            .iter()
            .map(|number| format!("{repository}#{number}"))
            .collect();
        notes.push(format!(
            "Closed superseded velocity-graph pin-bump issues {}.",
            closed.join(", ")
        ));
    }
    notes
}
