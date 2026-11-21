//! Structured task briefs prepared at dispatch, for fix-forward handoffs, and by `next_work` (#539).

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::time::Duration;

use velocity_airc::Channel;
use velocity_core::text_budget::{PROMPT_BYTES, preview, prompt_excerpt};
use velocity_graph::StaleReferenceWarning;
use velocity_rosen::ProjectServer;
use velocity_rosen::brief::{OpenChangeBrief, PostLandFailureBrief};
use velocity_spec::{ProjectSpec, extract_acceptance_criteria, extract_section_ids};
use velocity_store::{ProjectItem, ThreadItem, VersionControl};

const ROSEN_BRIEF_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_CRITERIA: usize = 8;
const MAX_SPEC_LINKS: usize = 8;
const MAX_DECISIONS: usize = 8;
const MAX_NEARBY_CHANGES: usize = 6;
const MAX_DISCOVERIES: usize = 5;
const MAX_FAILURES: usize = 4;

/// Fix-forward context carried by build repairs, CI repairs, and post-land reverts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FixForwardDetail {
    pub landed_head: String,
    pub culprit_change: Option<String>,
    pub failed_check: String,
}

/// Resolved spec section link (`file:start-end`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpecLink {
    pub id: String,
    pub title: String,
    pub location: String,
}

/// Matching §8 open-question / decision link (`file:line`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DecisionLink {
    pub id: String,
    pub decided: bool,
    pub summary: String,
    pub blocks: String,
    pub location: String,
}

/// An open change touching nearby paths or crates, with its owning thread when known.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NearbyChange {
    pub change_id: String,
    pub owner: String,
    pub thread_id: Option<u32>,
    pub thread_title: Option<String>,
    pub state: String,
    pub paths: Vec<String>,
}

/// A recent AIRC discovery relevant to the same crates or paths.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiscoveryNote {
    pub sequence: u64,
    pub nick: String,
    pub summary: String,
}

/// A recent Rosen post-land failure touching the same crates or paths.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FailureNote {
    pub commit: String,
    pub change: String,
    pub kind: String,
    pub reverted: bool,
    pub summary: String,
}

/// Complete task brief prepared by Velocity before an agent begins work.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TaskBrief {
    pub heading: String,
    pub reference: Option<String>,
    pub fix_forward: Option<FixForwardDetail>,
    pub acceptance_criteria: Vec<String>,
    pub spec_links: Vec<SpecLink>,
    pub open_decisions: Vec<DecisionLink>,
    pub nearby_changes: Vec<NearbyChange>,
    pub signature_warnings: Vec<StaleReferenceWarning>,
    pub dependencies: Vec<String>,
    pub crate_discoveries: Vec<DiscoveryNote>,
    pub post_land_failures: Vec<FailureNote>,
    pub owner_note: String,
}

/// Inputs describing the work item a brief is being built for.
pub struct BriefInput<'a> {
    pub heading: String,
    pub reference: Option<String>,
    pub piece: Option<String>,
    pub evidence_texts: Vec<String>,
    pub dependencies: Vec<String>,
    pub owner_note: String,
    pub exclude_change: Option<String>,
    pub fix_forward: Option<FixForwardDetail>,
    pub checkout: &'a Path,
    pub spec_path: &'a Path,
}

impl TaskBrief {
    /// Renders the brief as concise Markdown bounded to `budget` UTF-8 bytes.
    pub fn render(&self, budget: usize) -> String {
        let mut out = String::new();
        let kind = if self.fix_forward.is_some() {
            "Fix-Forward Task Brief"
        } else {
            "Task Brief"
        };
        out.push_str(&format!("# {kind}: {}\n", preview(&self.heading, 180)));
        if let Some(reference) = &self.reference
            && !reference.trim().is_empty()
        {
            out.push_str(&format!("Source: {reference}\n"));
        }
        self.write_fix_forward(&mut out);
        self.write_acceptance_criteria(&mut out);
        self.write_spec_and_decisions(&mut out);
        self.write_nearby_and_dependencies(&mut out);
        self.write_signature_warnings(&mut out);
        self.write_discoveries_and_failures(&mut out);
        out.push_str("\n## Ownership Through Review\n");
        out.push_str(&format!(
            "- {}\n",
            preview(&single_line(&self.owner_note), 260)
        ));
        prompt_excerpt(
            &out,
            budget,
            "\n[Task brief clipped. Inspect the linked spec sections, AIRC ledger, or Rosen status for full details.]",
        )
    }

    pub fn render_for_session(&self) -> String {
        self.render(PROMPT_BYTES)
    }

    fn write_fix_forward(&self, out: &mut String) {
        let Some(fix) = &self.fix_forward else {
            return;
        };
        out.push_str("\n## Fix-Forward Context\n");
        out.push_str(&format!(
            "- Landed head: `{}`\n",
            preview(&fix.landed_head, 64)
        ));
        if let Some(change) = &fix.culprit_change
            && !change.trim().is_empty()
        {
            out.push_str(&format!("- Culprit change: `{}`\n", preview(change, 64)));
        }
        out.push_str(&format!(
            "- Failed check: {}\n",
            preview(&single_line(&fix.failed_check), 400)
        ));
    }

    fn write_acceptance_criteria(&self, out: &mut String) {
        out.push_str("\n## Acceptance Criteria\n");
        if self.acceptance_criteria.is_empty() {
            out.push_str("- Implement the work item end to end and verify with `preflight`.\n");
            return;
        }
        for item in &self.acceptance_criteria {
            out.push_str(&format!("- {}\n", preview(&single_line(item), 220)));
        }
    }

    fn write_spec_and_decisions(&self, out: &mut String) {
        out.push_str("\n## Spec & §8 Decisions\n");
        if self.spec_links.is_empty() {
            out.push_str(
                "- No explicit spec section referenced; use `graph_find` (kind: section) if needed.\n",
            );
        } else {
            for link in &self.spec_links {
                out.push_str(&format!(
                    "- {} ({}) -> `{}`\n",
                    link.id,
                    preview(&link.title, 80),
                    link.location
                ));
            }
        }
        for decision in &self.open_decisions {
            let status = if decision.decided { "decided" } else { "open" };
            out.push_str(&format!(
                "- §8 {} [{status}] ({}) -> `{}`: {}\n",
                decision.id,
                preview(&decision.blocks, 40),
                decision.location,
                preview(&single_line(&decision.summary), 180),
            ));
        }
    }

    fn write_nearby_and_dependencies(&self, out: &mut String) {
        out.push_str("\n## Nearby Open Changes & Dependencies\n");
        if self.nearby_changes.is_empty() {
            out.push_str("- No open changes currently touch nearby paths or crates.\n");
        } else {
            for change in &self.nearby_changes {
                let thread = match (change.thread_id, &change.thread_title) {
                    (Some(id), Some(title)) => {
                        format!(" · thread-{id} ({})", preview(title, 60))
                    }
                    (Some(id), None) => format!(" · thread-{id}"),
                    _ => String::new(),
                };
                let paths = change
                    .paths
                    .iter()
                    .take(4)
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", ");
                out.push_str(&format!(
                    "- Change `{}` ({}, {}{thread}): {paths}\n",
                    short_id(&change.change_id),
                    change.owner,
                    change.state,
                ));
            }
        }
        for dep in &self.dependencies {
            out.push_str(&format!("- Dependency/Blocker: {}\n", preview(dep, 180)));
        }
    }

    fn write_signature_warnings(&self, out: &mut String) {
        if self.signature_warnings.is_empty() {
            return;
        }
        out.push_str("\n## Signature Warnings\n");
        for warning in &self.signature_warnings {
            let line = velocity_graph::signature_impact::format_warning_line(warning);
            out.push_str(&format!("- {}\n", preview(&single_line(&line), 260)));
        }
    }

    fn write_discoveries_and_failures(&self, out: &mut String) {
        out.push_str("\n## Recent Discoveries & Post-Land Failures\n");
        if self.crate_discoveries.is_empty() && self.post_land_failures.is_empty() {
            out.push_str("- None matching this work item's crates.\n");
            return;
        }
        for note in &self.crate_discoveries {
            out.push_str(&format!(
                "- Discovery [seq {} · {}]: {}\n",
                note.sequence,
                note.nick,
                preview(&single_line(&note.summary), 200)
            ));
        }
        for failure in &self.post_land_failures {
            let state = if failure.reverted {
                "reverted"
            } else {
                "active"
            };
            out.push_str(&format!(
                "- Post-land {} failure on `{}` (change `{}`, {state}): {}\n",
                failure.kind,
                short_commit(&failure.commit),
                short_id(&failure.change),
                preview(&single_line(&failure.summary), 200)
            ));
        }
    }
}

/// Builds a `TaskBrief` asynchronously (used when launching threads and on revert handoffs).
pub async fn build_brief_async(
    app_data: &Path,
    project: &ProjectItem,
    input: BriefInput<'_>,
) -> TaskBrief {
    let (open_changes, failures, signature_warnings) =
        if project.version_control == VersionControl::Rosen {
            let server = ProjectServer::new(app_data, project.id);
            let exclude = input.exclude_change.as_deref();
            let changes = tokio::time::timeout(
                ROSEN_BRIEF_TIMEOUT,
                velocity_rosen::brief::open_changes(&server, exclude),
            )
            .await
            .ok()
            .and_then(Result::ok)
            .unwrap_or_default();
            let failures = tokio::time::timeout(
                ROSEN_BRIEF_TIMEOUT,
                velocity_rosen::brief::recent_post_land_failures(&server),
            )
            .await
            .ok()
            .and_then(Result::ok)
            .unwrap_or_default();
            let warnings = fetch_signature_warnings_async(
                app_data,
                project.id,
                server.endpoint().as_str(),
                &input.evidence_texts,
                exclude,
            )
            .await;
            (changes, failures, warnings)
        } else {
            (Vec::new(), Vec::new(), Vec::new())
        };
    let channel = Channel::open(app_data, &project.id.to_string()).ok();
    let mut brief = assemble_brief(project, channel.as_ref(), &open_changes, &failures, input);
    brief.signature_warnings = signature_warnings;
    brief
}

/// Builds a `TaskBrief` from synchronous code (used by the AIRC MCP `next_work` tool).
pub fn build_brief_sync(
    app_data: &Path,
    project: &ProjectItem,
    channel: &Channel,
    input: BriefInput<'_>,
) -> TaskBrief {
    let (open_changes, failures, signature_warnings) =
        if project.version_control == VersionControl::Rosen {
            let (changes, failures) =
                fetch_rosen_sync(app_data, project.id, input.exclude_change.as_deref());
            let server = ProjectServer::new(app_data, project.id);
            let warnings = fetch_signature_warnings_sync(
                app_data,
                project.id,
                server.endpoint().as_str(),
                &input.evidence_texts,
                input.exclude_change.as_deref(),
            );
            (changes, failures, warnings)
        } else {
            (Vec::new(), Vec::new(), Vec::new())
        };
    let mut brief = assemble_brief(project, Some(channel), &open_changes, &failures, input);
    brief.signature_warnings = signature_warnings;
    brief
}

async fn fetch_signature_warnings_async(
    app_data: &Path,
    project_id: u32,
    endpoint: &str,
    texts: &[String],
    exclude_change: Option<&str>,
) -> Vec<StaleReferenceWarning> {
    let Ok(executable) = std::env::current_exe() else {
        return Vec::new();
    };
    let app_data = app_data.to_path_buf();
    let project = project_id.to_string();
    let endpoint = endpoint.to_owned();
    let texts = texts.to_vec();
    let exclude = exclude_change.map(str::to_owned);
    let lookup = tokio::task::spawn_blocking(move || {
        velocity_graph::dispatch_signature_warnings(
            &executable,
            &app_data,
            &project,
            &endpoint,
            &texts,
            exclude.as_deref(),
        )
    });
    tokio::time::timeout(ROSEN_BRIEF_TIMEOUT, lookup)
        .await
        .ok()
        .and_then(Result::ok)
        .unwrap_or_default()
}

fn fetch_signature_warnings_sync(
    app_data: &Path,
    project_id: u32,
    endpoint: &str,
    texts: &[String],
    exclude_change: Option<&str>,
) -> Vec<StaleReferenceWarning> {
    let Ok(executable) = std::env::current_exe() else {
        return Vec::new();
    };
    velocity_graph::dispatch_signature_warnings(
        &executable,
        app_data,
        &project_id.to_string(),
        endpoint,
        texts,
        exclude_change,
    )
}

fn fetch_rosen_sync(
    app_data: &Path,
    project_id: u32,
    exclude_change: Option<&str>,
) -> (Vec<OpenChangeBrief>, Vec<PostLandFailureBrief>) {
    let run = move |app_data: std::path::PathBuf, exclude: Option<String>| {
        let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        else {
            return (Vec::new(), Vec::new());
        };
        runtime.block_on(async move {
            let server = ProjectServer::new(&app_data, project_id);
            let changes = tokio::time::timeout(
                ROSEN_BRIEF_TIMEOUT,
                velocity_rosen::brief::open_changes(&server, exclude.as_deref()),
            )
            .await
            .ok()
            .and_then(Result::ok)
            .unwrap_or_default();
            let failures = tokio::time::timeout(
                ROSEN_BRIEF_TIMEOUT,
                velocity_rosen::brief::recent_post_land_failures(&server),
            )
            .await
            .ok()
            .and_then(Result::ok)
            .unwrap_or_default();
            (changes, failures)
        })
    };
    if tokio::runtime::Handle::try_current().is_ok() {
        let app_data = app_data.to_path_buf();
        let exclude = exclude_change.map(str::to_owned);
        return std::thread::spawn(move || run(app_data, exclude))
            .join()
            .unwrap_or_default();
    }
    run(app_data.to_path_buf(), exclude_change.map(str::to_owned))
}

/// Pure assembly of a `TaskBrief` from pre-read project, AIRC, and Rosen observations.
pub fn assemble_brief(
    project: &ProjectItem,
    channel: Option<&Channel>,
    open_changes: &[OpenChangeBrief],
    failures: &[PostLandFailureBrief],
    input: BriefInput<'_>,
) -> TaskBrief {
    let text_refs: Vec<&str> = input.evidence_texts.iter().map(String::as_str).collect();
    let sections = extract_section_ids(&text_refs);
    let (spec_links, open_decisions) =
        collect_spec_context(input.checkout, input.spec_path, &sections);
    let mut acceptance_criteria = Vec::new();
    for text in &input.evidence_texts {
        if acceptance_criteria.len() >= MAX_CRITERIA {
            break;
        }
        let remaining = MAX_CRITERIA - acceptance_criteria.len();
        for criterion in extract_acceptance_criteria(text, remaining) {
            if !acceptance_criteria.contains(&criterion) {
                acceptance_criteria.push(criterion);
            }
        }
    }
    if acceptance_criteria.is_empty() && input.fix_forward.is_some() {
        acceptance_criteria.push(
            "Restore the failing check on the landed base without weakening tests or touching unrelated code."
                .into(),
        );
    }
    let (paths, crates) = extract_paths_and_crates(input.checkout, &text_refs);
    let nearby_changes = collect_nearby_changes(project, open_changes, &paths, &crates);
    let mut dependencies = input.dependencies;
    if let Some(channel) = channel {
        append_airc_overlaps(channel, &paths, input.piece.as_deref(), &mut dependencies);
    }
    let crate_discoveries = channel
        .map(|channel| collect_crate_discoveries(channel, &paths, &crates))
        .unwrap_or_default();
    let post_land_failures = collect_crate_failures(failures, &paths, &crates);
    TaskBrief {
        heading: input.heading,
        reference: input.reference,
        fix_forward: input.fix_forward,
        acceptance_criteria,
        spec_links,
        open_decisions,
        nearby_changes,
        signature_warnings: Vec::new(),
        dependencies,
        crate_discoveries,
        post_land_failures,
        owner_note: input.owner_note,
    }
}

/// Builds a `BriefInput` for a thread already recorded in `project` (used at thread startup).
pub fn thread_brief_input<'a>(
    project: &'a ProjectItem,
    thread: &ThreadItem,
    prompt: &str,
    checkout: &'a Path,
    open_blockers: Vec<String>,
) -> Option<BriefInput<'a>> {
    let nick = thread_nick(thread);
    let spec_path = Path::new(&project.spec_path);
    if let Some(input) = build_repair_brief_input(
        project,
        thread,
        &nick,
        prompt,
        checkout,
        spec_path,
        &open_blockers,
    ) {
        return Some(input);
    }
    if let Some(input) = ci_repair_brief_input(
        project,
        thread,
        &nick,
        prompt,
        checkout,
        spec_path,
        &open_blockers,
    ) {
        return Some(input);
    }
    if let Some(input) = issue_brief_input(
        project,
        thread,
        &nick,
        prompt,
        checkout,
        spec_path,
        open_blockers.clone(),
    ) {
        return Some(input);
    }
    spec_or_task_brief_input(
        project,
        thread,
        &nick,
        prompt,
        checkout,
        spec_path,
        open_blockers,
    )
}

/// Builds a `BriefInput` for a reverted change handed back to its author for fix-forward (#539).
pub fn revert_brief_input<'a>(
    project: &'a ProjectItem,
    thread: &ThreadItem,
    change: &str,
    landed_head: &str,
    report: &str,
    checkout: &'a Path,
) -> BriefInput<'a> {
    let nick = thread_nick(thread);
    let owner_note = match &thread.issue {
        Some(issue) => format!(
            "thread-{} ({nick}) owns issue:{} and this post-land revert fix-forward through re-land and review.",
            thread.id, issue.number
        ),
        None => format!(
            "thread-{} ({nick}) owns this post-land revert fix-forward through re-land and verification.",
            thread.id
        ),
    };
    BriefInput {
        heading: format!("Fix reverted change: {}", thread.title),
        reference: Some(format!("Reverted Rosen change {}", short_id(change))),
        piece: thread
            .issue
            .as_ref()
            .map(|issue| format!("issue:{}", issue.number)),
        evidence_texts: vec![thread.title.clone(), report.to_owned()],
        dependencies: Vec::new(),
        owner_note,
        exclude_change: Some(change.to_owned()),
        fix_forward: Some(FixForwardDetail {
            landed_head: landed_head.to_owned(),
            culprit_change: Some(change.to_owned()),
            failed_check: extract_failed_check(report),
        }),
        checkout,
        spec_path: Path::new(&project.spec_path),
    }
}

fn thread_nick(thread: &ThreadItem) -> String {
    thread
        .worktree_path
        .as_deref()
        .and_then(|path| Path::new(path).file_name())
        .and_then(|name| name.to_str())
        .unwrap_or("thread")
        .to_owned()
}

fn build_repair_brief_input<'a>(
    project: &ProjectItem,
    thread: &ThreadItem,
    nick: &str,
    prompt: &str,
    checkout: &'a Path,
    spec_path: &'a Path,
    open_blockers: &[String],
) -> Option<BriefInput<'a>> {
    let repair = project
        .build_repairs
        .iter()
        .find(|repair| repair.thread == thread.id)?;
    Some(BriefInput {
        heading: thread.title.clone(),
        reference: Some(format!(
            "Build repair for commit {}",
            short_commit(&repair.commit)
        )),
        piece: None,
        evidence_texts: vec![thread.title.clone(), prompt.to_owned()],
        dependencies: open_blockers.to_vec(),
        owner_note: format!(
            "thread-{} ({nick}) owns this fix-forward repair through land and post-land verification.",
            thread.id
        ),
        exclude_change: thread.rosen_change.clone(),
        fix_forward: Some(FixForwardDetail {
            landed_head: repair.commit.clone(),
            culprit_change: extract_field(prompt, "Culprit change:"),
            failed_check: extract_failed_check(prompt),
        }),
        checkout,
        spec_path,
    })
}

fn ci_repair_brief_input<'a>(
    project: &ProjectItem,
    thread: &ThreadItem,
    nick: &str,
    prompt: &str,
    checkout: &'a Path,
    spec_path: &'a Path,
    open_blockers: &[String],
) -> Option<BriefInput<'a>> {
    let ci_repairs: Vec<_> = project
        .ci_repairs
        .iter()
        .filter(|repair| repair.thread == thread.id)
        .collect();
    let first = ci_repairs.first()?;
    let jobs = ci_repairs
        .iter()
        .map(|repair| format!("{} / {}", repair.workflow, repair.job))
        .collect::<Vec<_>>()
        .join(", ");
    Some(BriefInput {
        heading: thread.title.clone(),
        reference: Some(format!(
            "CI repair for commit {}",
            short_commit(&first.commit)
        )),
        piece: None,
        evidence_texts: vec![thread.title.clone(), prompt.to_owned()],
        dependencies: open_blockers.to_vec(),
        owner_note: format!(
            "thread-{} ({nick}) owns this CI fix-forward through land until `{}` passes on the base.",
            thread.id, first.workflow
        ),
        exclude_change: thread.rosen_change.clone(),
        fix_forward: Some(FixForwardDetail {
            landed_head: first.commit.clone(),
            culprit_change: None,
            failed_check: jobs,
        }),
        checkout,
        spec_path,
    })
}

fn issue_brief_input<'a>(
    project: &ProjectItem,
    thread: &ThreadItem,
    nick: &str,
    prompt: &str,
    checkout: &'a Path,
    spec_path: &'a Path,
    mut dependencies: Vec<String>,
) -> Option<BriefInput<'a>> {
    let issue = thread.issue.as_ref()?;
    if let Some(hold) = project.overlap_hold(issue) {
        dependencies.push(format!("Overlap hold: {}", hold.reason()));
    }
    Some(BriefInput {
        heading: format!("{}#{}: {}", issue.repository, issue.number, thread.title),
        reference: Some(format!(
            "https://github.com/{}/issues/{}",
            issue.repository, issue.number
        )),
        piece: Some(format!("issue:{}", issue.number)),
        evidence_texts: vec![thread.title.clone(), prompt.to_owned()],
        dependencies,
        owner_note: format!(
            "thread-{} ({nick}) holds issue:{} through land and after-land review. Leave the issue open; call issue_ready_to_close with evidence when naming the change.",
            thread.id, issue.number
        ),
        exclude_change: thread.rosen_change.clone(),
        fix_forward: None,
        checkout,
        spec_path,
    })
}

fn spec_or_task_brief_input<'a>(
    project: &ProjectItem,
    thread: &ThreadItem,
    nick: &str,
    prompt: &str,
    checkout: &'a Path,
    spec_path: &'a Path,
    dependencies: Vec<String>,
) -> Option<BriefInput<'a>> {
    if let Some(section) = &thread.spec_section {
        return Some(BriefInput {
            heading: thread.title.clone(),
            reference: Some(format!("§7 {section}")),
            piece: Some(format!("spec:{section}")),
            evidence_texts: vec![section.clone(), thread.title.clone(), prompt.to_owned()],
            dependencies,
            owner_note: format!(
                "thread-{} ({nick}) owns §7 item `{section}` through land and after-land review. Velocity ticks §7 on land.",
                thread.id
            ),
            exclude_change: thread.rosen_change.clone(),
            fix_forward: None,
            checkout,
            spec_path,
        });
    }
    let &task_id = thread.task_ids.first()?;
    let task_text = project
        .tasks
        .iter()
        .find(|task| task.id == task_id)
        .map(|task| task.text.clone())
        .unwrap_or_else(|| thread.title.clone());
    Some(BriefInput {
        heading: format!("Task #{task_id}: {}", thread.title),
        reference: Some(format!("Task List #{task_id}")),
        piece: Some(format!("task:{task_id}")),
        evidence_texts: vec![task_text, prompt.to_owned()],
        dependencies,
        owner_note: format!(
            "thread-{} ({nick}) owns task:{task_id} through land and after-land review. Velocity marks the task done on land.",
            thread.id
        ),
        exclude_change: thread.rosen_change.clone(),
        fix_forward: None,
        checkout,
        spec_path,
    })
}

fn collect_spec_context(
    checkout: &Path,
    spec_path: &Path,
    sections: &[String],
) -> (Vec<SpecLink>, Vec<DecisionLink>) {
    let Ok(spec) = ProjectSpec::read(checkout, spec_path) else {
        return (Vec::new(), Vec::new());
    };
    let mut links = Vec::new();
    for section in sections.iter().take(MAX_SPEC_LINKS) {
        if let Some(span) = spec.locate_section(section) {
            let path = span.path.to_string_lossy().replace('\\', "/");
            links.push(SpecLink {
                id: span.id,
                title: span.title,
                location: format!("{path}:{}-{}", span.start_line, span.end_line),
            });
        }
    }
    let decisions = spec
        .matching_questions(sections, &[])
        .into_iter()
        .take(MAX_DECISIONS)
        .map(|row| {
            let path = row.path.to_string_lossy().replace('\\', "/");
            DecisionLink {
                id: row.id,
                decided: row.decided,
                summary: row.question,
                blocks: row.blocks,
                location: format!("{path}:{}", row.line),
            }
        })
        .collect();
    (links, decisions)
}

/// Extracts candidate repo-relative paths and crate/module names from evidence texts.
pub(crate) fn extract_paths_and_crates(
    checkout: &Path,
    texts: &[&str],
) -> (BTreeSet<String>, BTreeSet<String>) {
    let mut paths = BTreeSet::new();
    let mut crates = BTreeSet::new();
    let known_crates = discover_workspace_crates(checkout);
    for text in texts {
        for file in crate::goal_selection::referenced_files(checkout, text) {
            if let Some(crate_name) = path_crate(&file) {
                crates.insert(crate_name);
            }
            paths.insert(file);
        }
        let lower = text.to_ascii_lowercase();
        for crate_name in &known_crates {
            if contains_identifier(&lower, crate_name) {
                crates.insert(crate_name.clone());
            }
        }
        for top in ["src-tauri", "ui"] {
            if lower.contains(&format!("{top}/")) || contains_identifier(&lower, top) {
                crates.insert(top.to_owned());
            }
        }
    }
    (paths, crates)
}

fn discover_workspace_crates(checkout: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(checkout.join("crates")) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .filter_map(|entry| entry.file_name().to_str().map(str::to_ascii_lowercase))
        .collect()
}

fn path_crate(path: &str) -> Option<String> {
    let normalized = path.replace('\\', "/");
    if let Some(rest) = normalized.strip_prefix("crates/") {
        return rest.split('/').next().map(str::to_ascii_lowercase);
    }
    if normalized.starts_with("src-tauri/") {
        return Some("src-tauri".into());
    }
    if normalized.starts_with("ui/") {
        return Some("ui".into());
    }
    None
}

fn contains_identifier(haystack: &str, needle: &str) -> bool {
    let bytes = haystack.as_bytes();
    let mut start = 0;
    while let Some(offset) = haystack[start..].find(needle) {
        let pos = start + offset;
        let before_ok = pos == 0 || !is_ident_byte(bytes[pos - 1]);
        let after = pos + needle.len();
        let after_ok = after == bytes.len() || !is_ident_byte(bytes[after]);
        if before_ok && after_ok {
            return true;
        }
        start = after;
    }
    false
}

fn is_ident_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_'
}

fn collect_nearby_changes(
    project: &ProjectItem,
    open_changes: &[OpenChangeBrief],
    target_paths: &BTreeSet<String>,
    target_crates: &BTreeSet<String>,
) -> Vec<NearbyChange> {
    let threads_by_change: BTreeMap<&str, &ThreadItem> = project
        .threads
        .iter()
        .filter_map(|thread| {
            thread
                .rosen_change
                .as_deref()
                .map(|change| (change, thread))
        })
        .collect();
    let mut nearby = Vec::new();
    for change in open_changes {
        let all_paths: Vec<String> = change
            .edited_paths
            .iter()
            .chain(change.claimed_paths.iter())
            .cloned()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let matching: Vec<String> = if target_paths.is_empty() && target_crates.is_empty() {
            all_paths.iter().take(4).cloned().collect()
        } else {
            all_paths
                .iter()
                .filter(|path| path_is_nearby(path, target_paths, target_crates))
                .cloned()
                .collect()
        };
        if matching.is_empty() {
            continue;
        }
        let thread = threads_by_change.get(change.id.as_str()).copied();
        nearby.push(NearbyChange {
            change_id: change.id.clone(),
            owner: change.owner.clone(),
            thread_id: thread.map(|thread| thread.id),
            thread_title: thread.map(|thread| thread.title.clone()),
            state: change.state.clone(),
            paths: matching,
        });
        if nearby.len() >= MAX_NEARBY_CHANGES {
            break;
        }
    }
    nearby
}

fn path_is_nearby(
    path: &str,
    target_paths: &BTreeSet<String>,
    target_crates: &BTreeSet<String>,
) -> bool {
    let normalized = path.replace('\\', "/");
    if target_paths.contains(&normalized) {
        return true;
    }
    path_crate(&normalized).is_some_and(|crate_name| target_crates.contains(&crate_name))
}

fn append_airc_overlaps(
    channel: &Channel,
    target_paths: &BTreeSet<String>,
    own_piece: Option<&str>,
    dependencies: &mut Vec<String>,
) {
    let Ok(claims) = channel.claims() else {
        return;
    };
    for claim in claims {
        if own_piece.is_some_and(|piece| claim.key() == piece) {
            continue;
        }
        let shared: Vec<&String> = claim
            .files
            .iter()
            .filter(|file| {
                let normalized = file
                    .strip_suffix("::registration")
                    .unwrap_or(file)
                    .replace('\\', "/");
                target_paths.contains(&normalized)
            })
            .collect();
        if !shared.is_empty() {
            let files = shared
                .into_iter()
                .take(3)
                .map(String::as_str)
                .collect::<Vec<_>>()
                .join(", ");
            dependencies.push(format!(
                "AIRC claim overlap with `{}` ({}): {files}",
                claim.nick,
                claim.key()
            ));
        }
    }
}

fn collect_crate_discoveries(
    channel: &Channel,
    target_paths: &BTreeSet<String>,
    target_crates: &BTreeSet<String>,
) -> Vec<DiscoveryNote> {
    let Ok(entries) = channel.ledger("discovery", 0) else {
        return Vec::new();
    };
    let mut notes = Vec::new();
    for entry in entries.iter().rev() {
        let lower = entry.text.to_ascii_lowercase();
        let matches = if target_crates.is_empty() && target_paths.is_empty() {
            true
        } else {
            target_crates
                .iter()
                .any(|crate_name| contains_identifier(&lower, crate_name))
                || target_paths
                    .iter()
                    .any(|path| lower.contains(&path.to_ascii_lowercase()))
        };
        if !matches {
            continue;
        }
        notes.push(DiscoveryNote {
            sequence: entry.sequence,
            nick: entry.nick.clone(),
            summary: entry.text.clone(),
        });
        if notes.len() >= MAX_DISCOVERIES {
            break;
        }
    }
    notes
}

fn collect_crate_failures(
    failures: &[PostLandFailureBrief],
    target_paths: &BTreeSet<String>,
    target_crates: &BTreeSet<String>,
) -> Vec<FailureNote> {
    let mut notes = Vec::new();
    for failure in failures {
        let lower = failure.output.to_ascii_lowercase();
        let matches = if target_crates.is_empty() && target_paths.is_empty() {
            !failure.reverted
        } else {
            target_crates
                .iter()
                .any(|crate_name| contains_identifier(&lower, crate_name))
                || target_paths
                    .iter()
                    .any(|path| lower.contains(&path.to_ascii_lowercase()))
        };
        if !matches {
            continue;
        }
        let summary = first_diagnostic_line(&failure.output)
            .unwrap_or_else(|| failure.output.lines().next().unwrap_or_default().to_owned());
        notes.push(FailureNote {
            commit: failure.commit.clone(),
            change: failure.change.clone(),
            kind: failure.kind.clone(),
            reverted: failure.reverted,
            summary,
        });
        if notes.len() >= MAX_FAILURES {
            break;
        }
    }
    notes
}

fn first_diagnostic_line(output: &str) -> Option<String> {
    output
        .lines()
        .find(|line| {
            let lower = line.to_ascii_lowercase();
            ["error:", "error[", "failed", "panicked", "fatal:"]
                .iter()
                .any(|needle| lower.contains(needle))
        })
        .map(|line| line.trim().to_owned())
}

fn extract_field(prompt: &str, prefix: &str) -> Option<String> {
    prompt.lines().find_map(|line| {
        line.trim()
            .strip_prefix(prefix)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
    })
}

fn extract_failed_check(prompt: &str) -> String {
    first_diagnostic_line(prompt).unwrap_or_else(|| {
        prompt
            .lines()
            .find(|line| !line.trim().is_empty())
            .unwrap_or("Post-land check failed")
            .trim()
            .to_owned()
    })
}

fn single_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn short_id(id: &str) -> &str {
    id.get(..8).unwrap_or(id)
}

fn short_commit(commit: &str) -> &str {
    commit.get(..12).unwrap_or(commit)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    // Risk (#539): task briefs at dispatch and fix-forward handoffs must include all required
    // sections (acceptance criteria, spec & §8 links, nearby changes, crate discoveries/failures,
    // ownership, and landed head/failed check for fix-forward) within the prompt budget.
    #[test]
    fn assembles_and_renders_dispatch_and_fix_forward_briefs()
    -> Result<(), Box<dyn std::error::Error>> {
        let root =
            std::env::temp_dir().join(format!("velocity-task-brief-test-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(root.join("spec"))?;
        fs::create_dir_all(root.join("crates/velocity-threads/src"))?;
        fs::write(
            root.join("crates/velocity-threads/src/dispatch.rs"),
            "// code\n",
        )?;
        fs::write(
            root.join("SPEC.md"),
            "# SPEC\n\n## Spec file index\n- spec/F9-issues-and-dispatch.md\n",
        )?;
        fs::write(
            root.join("spec/F9-issues-and-dispatch.md"),
            "### F9 · Issues and dispatch\nDispatch rules.\n",
        )?;
        fs::write(
            root.join("spec/8-open-questions.md"),
            "## 8. Open questions\n| # | Question | Blocks |\n|---|---|---|\n| Q28 | Decided (#426): issues stay on GitHub. | F7, F9 |\n",
        )?;

        let project = ProjectItem::new(1, "velocity".into(), root.display().to_string());
        let open = vec![OpenChangeBrief {
            id: "abcd1234efgh5678".into(),
            owner: "issue-538".into(),
            state: "open".into(),
            edited_paths: vec!["crates/velocity-threads/src/dispatch.rs".into()],
            claimed_paths: Vec::new(),
        }];
        let failures = vec![PostLandFailureBrief {
            commit: "1234567890abcdef".into(),
            change: "zzzz111122223333".into(),
            kind: "build".into(),
            reverted: false,
            output: "error[E0432]: unresolved import in velocity-threads".into(),
        }];

        let mut brief = assemble_brief(
            &project,
            None,
            &open,
            &failures,
            BriefInput {
                heading: "Fix dispatch F9".into(),
                reference: Some("https://github.com/mESmaC/velocity/issues/539".into()),
                piece: Some("issue:539".into()),
                evidence_texts: vec![
                    "Fix dispatch in crates/velocity-threads/src/dispatch.rs (F9)\n**Acceptance criteria**\n- Build task brief at dispatch\n- Include §8 decisions".into(),
                ],
                dependencies: vec!["Blocked by #500".into()],
                owner_note: "thread-9 (issue-539) holds issue:539 through review.".into(),
                exclude_change: None,
                fix_forward: Some(FixForwardDetail {
                    landed_head: "deadbeef12345678".into(),
                    culprit_change: Some("qqqq9999".into()),
                    failed_check: "cargo test -p velocity-threads".into(),
                }),
                checkout: &root,
                spec_path: Path::new("SPEC.md"),
            },
        );
        brief.signature_warnings.push(StaleReferenceWarning {
            symbol: "plan_issue_dispatch".into(),
            kind: velocity_graph::SignatureChangeKind::PublicSignature,
            defined_in: "crates/velocity-threads/src/dispatch.rs".into(),
            detail: "signature changed".into(),
            direction: velocity_graph::ReferenceDirection::PeerAlteredWorkTouches,
            peer_change: "abcd1234efgh5678".into(),
            peer_owner: "issue-538".into(),
            peer_status: velocity_graph::ChangeStatus::Open,
            peer_commit: None,
            referencing_paths: vec!["src-tauri/src/issue_panel.rs".into()],
        });
        let rendered = brief.render(5_500);
        assert!(rendered.contains("# Fix-Forward Task Brief: Fix dispatch F9"));
        assert!(rendered.contains("Landed head: `deadbeef12345678`"));
        assert!(rendered.contains("Failed check: cargo test -p velocity-threads"));
        assert!(rendered.contains("Build task brief at dispatch"));
        assert!(rendered.contains("F9"));
        assert!(rendered.contains("spec/F9-issues-and-dispatch.md:1-2"));
        assert!(rendered.contains("§8 Q28 [decided]"));
        assert!(rendered.contains("Change `abcd1234` (issue-538, open)"));
        assert!(rendered.contains("## Signature Warnings"));
        assert!(rendered.contains("plan_issue_dispatch"));
        assert!(rendered.contains("Blocked by #500"));
        assert!(rendered.contains("unresolved import in velocity-threads"));
        assert!(rendered.contains("thread-9 (issue-539) holds issue:539 through review."));
        assert!(rendered.len() <= 5_500);

        let _ = fs::remove_dir_all(&root);
        Ok(())
    }
}
