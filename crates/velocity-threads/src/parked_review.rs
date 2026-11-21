//! The bounded review of an issue parked under `no-change` (#506, #526). It needs no staffed
//! review swarm: continuous dispatch starts one light, read-only session per report, which
//! closes the issue with evidence, closes an orphaned spec issue as not planned, returns it
//! to dispatch with retry instructions, marks it `blocked-upstream`, or records a visible
//! person hold with its release action. Whatever the session writes is discarded with its Rosen change.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use agent_client_protocol::v1::schema::{McpServer, McpServerStdio};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::mpsc::unbounded_channel;
use velocity_acp::client::{AgentEvent, AgentInput, SessionSetup};
use velocity_core::worktree_settings::WorktreePreferences;
use velocity_github::mcp_stdio::ToolServer;
use velocity_github::{IssueClient, ParkedOutcome};
use velocity_rosen::{AgentWorkspace, ProjectServer, review_swarm};
use velocity_store::duty::DutyAgentItem;
use velocity_store::{ProjectItem, Settings};

use crate::launch::{Location, agent_launch};
use crate::review_swarm::message;

pub const PARKED_REVIEW_SWITCH: &str = "--parked-review-mcp";

/// The Rosen identity every bounded review works under. One runs per project at a time.
const REVIEWER: &str = "velocity-reviewer";
/// A review reads one issue against the base. A session still going after this is stopped,
/// and the issue returns to dispatch when its report passes the parked limit.
const RUN_LIMIT: Duration = Duration::from_secs(20 * 60);
/// How long a stopped session has to end before its input is closed under it.
const STOP_GRACE: Duration = Duration::from_secs(60);
/// A run that could not start, such as while the Rosen server restarts, is tried again.
const START_RETRY: Duration = Duration::from_secs(5 * 60);
/// The prompt names the first files of a large land; the workspace holds the rest.
const LISTED_LAND_FILES: usize = 40;

/// The land an overlap review checks the issue against.
#[derive(Clone, Debug)]
pub struct OverlappingLand {
    pub source: u32,
    pub commit: String,
    pub files: Vec<String>,
}

/// One parked issue and what its session runs on.
pub struct ParkedReviewRequest {
    pub settings: Settings,
    pub project: ProjectItem,
    pub worktrees: WorktreePreferences,
    pub repository: String,
    pub issue: u32,
    pub title: String,
    /// The report under review. A newer report on the issue gets its own run.
    pub attempt: String,
    /// `None` when an implementation agent ended its turn without a change.
    pub land: Option<OverlappingLand>,
}

type ReviewKey = (u32, u32, String);

#[derive(Default)]
struct State {
    running: BTreeSet<u32>,
    /// Reports that already had their one run since Velocity started.
    reviewed: BTreeSet<ReviewKey>,
    failed_starts: BTreeMap<u32, Instant>,
}

impl State {
    fn free(&self, key: &ReviewKey) -> bool {
        !self.running.contains(&key.0)
            && !self
                .failed_starts
                .get(&key.0)
                .is_some_and(|failed| failed.elapsed() < START_RETRY)
            && !self.reviewed.contains(key)
    }
}

/// Runs bounded reviews. Cheap to clone; every clone shares the same reservations.
#[derive(Clone)]
pub struct ParkedReviews {
    state: Arc<Mutex<State>>,
    executable: PathBuf,
    app_data: PathBuf,
    launches: PathBuf,
}

/// What the session's MCP server reads: the one issue and report it may resolve.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct ParkedLaunch {
    workspace: PathBuf,
    repository: String,
    issue: u32,
    attempt: String,
}

impl ParkedReviews {
    /// `executable` serves the session's MCP server (Velocity itself).
    pub fn new(executable: PathBuf, app_data: PathBuf) -> Self {
        Self {
            state: Arc::default(),
            executable,
            launches: app_data.join("parked-reviews"),
            app_data,
        }
    }

    /// Whether `reserve` would take the project's review slot for this report: no review
    /// runs there, this report has not had its run, and no start failed recently.
    pub fn available(&self, project: u32, issue: u32, attempt: &str) -> bool {
        self.lock().free(&(project, issue, attempt.to_owned()))
    }

    /// Takes the project's single review slot for this report. `false` when it is not
    /// `available`; the caller must then not `run` it.
    pub fn reserve(&self, project: u32, issue: u32, attempt: &str) -> bool {
        let key = (project, issue, attempt.to_owned());
        let mut state = self.lock();
        if !state.free(&key) {
            return false;
        }
        state.running.insert(project);
        state.reviewed.insert(key);
        true
    }

    /// Runs a reserved review to its end and frees the project's slot.
    pub async fn run(&self, request: ParkedReviewRequest) -> Result<(), String> {
        let project = request.project.id;
        let server = ProjectServer::new(&self.app_data, project);
        let result = match self.prepare(&request, &server).await {
            Ok((workspace, launch)) => {
                let outcome = self.session(&request, &server, &workspace, &launch).await;
                let _ = std::fs::remove_file(&launch);
                let discarded = discard(&server, &workspace).await;
                outcome.and(discarded)
            }
            Err(error) => {
                let mut state = self.lock();
                state
                    .reviewed
                    .remove(&(project, request.issue, request.attempt.clone()));
                state.failed_starts.insert(project, Instant::now());
                Err(error)
            }
        };
        self.lock().running.remove(&project);
        result.map_err(|error| {
            format!(
                "The review of {}#{} did not finish: {error}",
                request.repository, request.issue
            )
        })
    }

    /// A fresh workspace at the current base and the launch file of the session's server.
    async fn prepare(
        &self,
        request: &ParkedReviewRequest,
        server: &ProjectServer,
    ) -> Result<(AgentWorkspace, PathBuf), String> {
        let root = Location::new(&request.project, &request.worktrees)
            .project_root()
            .map_err(|error| error.to_string())?
            .join(format!("parked-review-{}", request.issue));
        if root.join(".rosen").exists() {
            // An interrupted run left its workspace. Its base may be stale, so it goes.
            let stale = review_swarm::open_workspace(server, REVIEWER, &root)
                .await
                .map_err(|error| error.to_string())?;
            discard(server, &stale).await?;
        }
        let workspace = velocity_rosen::create_workspace(server, REVIEWER, &root, None)
            .await
            .map_err(|error| error.to_string())?;
        std::fs::create_dir_all(&self.launches).map_err(|error| error.to_string())?;
        let launch = self
            .launches
            .join(format!("{}-{}.json", request.project.id, request.issue));
        let record = ParkedLaunch {
            workspace: workspace.root.clone(),
            repository: request.repository.clone(),
            issue: request.issue,
            attempt: request.attempt.clone(),
        };
        let saved = serde_json::to_vec(&record)
            .map_err(|error| error.to_string())
            .and_then(|bytes| std::fs::write(&launch, bytes).map_err(|error| error.to_string()));
        if let Err(error) = saved {
            discard(server, &workspace).await?;
            return Err(error);
        }
        Ok((workspace, launch))
    }

    /// One turn, stopped at its end or at the run limit, whichever comes first.
    async fn session(
        &self,
        request: &ParkedReviewRequest,
        server: &ProjectServer,
        workspace: &AgentWorkspace,
        launch: &Path,
    ) -> Result<(), String> {
        let agent = reviewer(&request.settings);
        let (mut agent_launch, modes) =
            agent_launch(&request.settings, agent.provider).map_err(|error| error.to_string())?;
        agent_launch.env.extend(velocity_rosen::agent_environment(
            server,
            REVIEWER,
            &workspace.change,
        ));
        let settings = &request.settings;
        // A light session: the effort of a trivial reading review.
        let mut setup = SessionSetup::new(workspace.root.clone(), modes).with_run_settings(
            crate::titles::duty_run_settings(settings, &agent, settings.agents.review_effort(true)),
        );
        setup.usage_role = Some(velocity_graph::token_usage::Role::Review);
        setup.shared_servers.push(McpServer::Stdio(
            McpServerStdio::new("velocity-parked-review", &self.executable).args(vec![
                PARKED_REVIEW_SWITCH.into(),
                launch.display().to_string(),
            ]),
        ));
        let (input, inputs) = unbounded_channel();
        input
            .send(message(&prompt(request)))
            .map_err(|error| error.to_string())?;
        let mut session = crate::spawn_session(agent_launch, setup, inputs);
        let turn = async {
            while let Some(event) = session.events.recv().await {
                if matches!(event, AgentEvent::TurnEnded(_)) {
                    let _ = input.send(AgentInput::Stop);
                }
            }
        };
        if tokio::time::timeout(RUN_LIMIT, turn).await.is_err() {
            let _ = input.send(AgentInput::Stop);
            let ended = async { while session.events.recv().await.is_some() {} };
            let _ = tokio::time::timeout(STOP_GRACE, ended).await;
        }
        // A session that ignored the stop ends when its input closes.
        drop(input);
        session.outcome.await.map_err(|error| error.to_string())?
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        // The state holds plain sets; a panic elsewhere leaves them usable.
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// The session's agent: the provider and model new work starts on.
fn reviewer(settings: &Settings) -> DutyAgentItem {
    let provider = settings.new_work_choice();
    DutyAgentItem {
        nickname: REVIEWER.into(),
        provider,
        model_name: settings.provider_model(provider),
        busy: false,
        doing: String::new(),
        sweep: None,
        repeat_unchanged: false,
        effort: None,
        preferred_speed: None,
    }
}

/// The review is read-only: its change is abandoned with anything the session wrote, and
/// never landed. Storing those files first lets Rosen's deletion guard remove the
/// workspace; a change that is already closed has nothing left to store.
async fn discard(server: &ProjectServer, workspace: &AgentWorkspace) -> Result<(), String> {
    let _ = review_swarm::snapshot_head(server, REVIEWER, &workspace.change, &workspace.root).await;
    velocity_rosen::dispatch::abandon_finished(server, &workspace.change)
        .await
        .map_err(|error| error.to_string())
}

fn prompt(request: &ParkedReviewRequest) -> String {
    let reason = request.land.as_ref().map_or_else(
        || "an implementation agent ended its turn on it without a change".to_owned(),
        |land| {
            let mut files: Vec<_> = land
                .files
                .iter()
                .take(LISTED_LAND_FILES)
                .map(|file| format!("`{file}`"))
                .collect();
            if land.files.len() > LISTED_LAND_FILES {
                files.push(format!("and {} more", land.files.len() - LISTED_LAND_FILES));
            }
            format!(
                "it waited for #{}, whose Rosen commit {} landed and changed code this issue is \
                 predicted to touch. Files in that land: {}",
                land.source,
                land.commit,
                files.join(", ")
            )
        },
    );
    format!(
        "You are Velocity's bounded review of {}#{}: {}. The issue is parked under the \
         no-change label because {reason}.\n\n\
         This is one read-only session. Your folder is a Rosen workspace at the current base. \
         Do not edit files, name a change, claim paths or file issues; anything written here \
         is discarded. Call parked_review_read for the issue, its discussion and the report. \
         Check each acceptance criterion against the code in this folder. Then call \
         parked_review_resolve once: already_fixed with verification evidence when the base \
         meets every criterion, return_to_dispatch with concrete instructions for the next \
         agent when work remains, blocked_upstream with upstream (e.g. owner/repo#123 or \
         owner/repo@tag) when waiting on an upstream PR or release, spec_removed with \
         spec_section and removed_commit when the spec section no longer exists, or hold \
         with release_action ONLY when physical hardware or an explicit scope hold requires \
         a person (never for spec defaults per Rule 1). When the issue waits on other \
         issues, use return_to_dispatch and name them in blocked_by; never hold it with a \
         label for that. End your turn after resolving.",
        request.repository,
        request.issue,
        velocity_core::text_budget::preview(&request.title, 300),
    )
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum Decision {
    AlreadyFixed,
    ReturnToDispatch,
    BlockedUpstream,
    SpecRemoved,
    Hold,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Resolution {
    outcome: Decision,
    evidence: String,
    /// Upstream PR or release target when `outcome == blocked_upstream` (#526).
    #[serde(default)]
    upstream: Option<String>,
    /// Removed spec section name when `outcome == spec_removed` (#526).
    #[serde(default)]
    spec_section: Option<String>,
    /// Commit SHA that removed the spec section when `outcome == spec_removed` (#526).
    #[serde(default)]
    removed_commit: Option<String>,
    /// Concrete action that releases the hold on the Work view when `outcome == hold` (#526).
    #[serde(default)]
    release_action: Option<String>,
    /// The issues this one waits on, linked as GitHub dependencies (#511).
    #[serde(default)]
    blocked_by: Vec<String>,
}

/// The session's MCP server: it reads and resolves the one issue of its launch file.
pub struct ParkedReviewServer {
    launch: ParkedLaunch,
}

impl ParkedReviewServer {
    pub fn open(path: &Path) -> Result<Self, String> {
        let bytes = std::fs::read(path).map_err(|error| error.to_string())?;
        let launch = serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
        Ok(Self { launch })
    }

    pub fn serve(
        &mut self,
        input: impl std::io::BufRead,
        output: impl std::io::Write,
    ) -> std::io::Result<()> {
        velocity_github::mcp_stdio::serve(self, input, output)
    }

    fn client(&self) -> Result<IssueClient, String> {
        IssueClient::for_repository(&self.launch.workspace, &self.launch.repository)
            .map_err(|error| error.to_string())
    }

    fn read(&self) -> Result<Value, String> {
        let (issue, comments, attempt) = self
            .client()?
            .no_change_discussion(self.launch.issue)
            .map_err(|error| error.to_string())?;
        let excerpt = |text: &str, limit| {
            velocity_core::text_budget::prompt_excerpt(
                text,
                limit,
                "\n[Read the rest in the GitHub discussion.]",
            )
        };
        let comments: Vec<_> = comments
            .iter()
            .map(|comment| excerpt(comment, 1_500))
            .collect();
        Ok(json!({
            "issue": {"number": issue.number, "title": issue.title, "url": issue.url,
                "body": excerpt(&issue.body, 4_000)},
            "discussion_excerpts": comments,
            "report_current": attempt == self.launch.attempt,
        }))
    }

    fn resolve(&self, arguments: Value) -> Result<Value, String> {
        let resolution: Resolution =
            serde_json::from_value(arguments).map_err(|error| error.to_string())?;
        let outcome = match resolution.outcome {
            Decision::AlreadyFixed => ParkedOutcome::AlreadyFixed,
            Decision::ReturnToDispatch => ParkedOutcome::Returned,
            Decision::BlockedUpstream => {
                let raw = resolution.upstream.as_deref().unwrap_or_default();
                let target = velocity_core::issue_holds::UpstreamTarget::parse(raw).ok_or_else(|| {
                    format!("blocked_upstream requires upstream as owner/repo#123, owner/repo@tag or owner/repo#123 -> release:owner/repo (got {raw:?})")
                })?;
                ParkedOutcome::BlockedUpstream { target }
            }
            Decision::SpecRemoved => ParkedOutcome::SpecRemoved {
                spec_section: resolution
                    .spec_section
                    .filter(|value| !value.trim().is_empty())
                    .ok_or("spec_removed requires spec_section")?,
                removed_commit: resolution
                    .removed_commit
                    .filter(|value| !value.trim().is_empty())
                    .ok_or("spec_removed requires removed_commit")?,
            },
            Decision::Hold => ParkedOutcome::Hold {
                release_action: resolution
                    .release_action
                    .filter(|value| !value.trim().is_empty())
                    .ok_or("hold requires release_action explaining what releases the hold on the Work view")?,
            },
        };
        let blockers = velocity_core::issue_dependencies::parse_blockers(&resolution.blocked_by)?;
        self.client()?
            .resolve_no_change(
                self.launch.issue,
                &self.launch.attempt,
                "Velocity bounded review",
                &resolution.evidence,
                outcome,
                &blockers,
            )
            .map_err(|error| error.to_string())?;
        Ok(json!({"issue": self.launch.issue, "resolved": true,
            "instruction": "End your turn."}))
    }
}

impl ToolServer for ParkedReviewServer {
    fn name(&self) -> &'static str {
        "velocity-parked-review"
    }

    fn instructions(&self) -> String {
        "Review one parked issue against the current base without editing it. Read it with parked_review_read, then resolve it once with parked_review_resolve.".into()
    }

    fn tools(&self) -> Vec<Value> {
        vec![
            json!({
                "name": "parked_review_read",
                "description": "The parked issue, the report that parked it and its latest discussion.",
                "inputSchema": {"type": "object", "properties": {}, "additionalProperties": false}
            }),
            json!({
                "name": "parked_review_resolve",
                "description": "End the review. already_fixed closes the issue with verification evidence. return_to_dispatch removes no-change and gives the next agent instructions (use with blocked_by when waiting on other issues). blocked_upstream records an upstream PR/release checked daily. spec_removed closes an orphaned issue as not planned citing the commit that removed its spec section. hold records a visible Work-view hold with its release_action ONLY for hardware or an explicit scope hold.",
                "inputSchema": {"type": "object", "properties": {
                    "outcome": {"type": "string", "enum": ["already_fixed", "return_to_dispatch", "blocked_upstream", "spec_removed", "hold"]},
                    "evidence": {"type": "string", "minLength": 1, "description": "Verification evidence, instructions for the next agent, or concise hold reason"},
                    "upstream": {"type": "string", "description": "Required for blocked_upstream: owner/repo#123, owner/repo@tag, or owner/repo#123 -> release:owner2/repo2"},
                    "spec_section": {"type": "string", "description": "Required for spec_removed: the removed spec section name"},
                    "removed_commit": {"type": "string", "description": "Required for spec_removed: the commit SHA that removed the spec section"},
                    "release_action": {"type": "string", "description": "Required for hold: the concrete action that releases the hold on the Work view"},
                    "blocked_by": {"type": "array", "items": {"type": "string"}, "description": "Issues this one waits on, as #12 for this repository or owner/name#12 for another"}
                }, "required": ["outcome", "evidence"], "additionalProperties": false}
            }),
        ]
    }

    fn call(&mut self, name: Option<&str>, arguments: Value) -> Result<Value, String> {
        match name {
            Some("parked_review_read") => self.read(),
            Some("parked_review_resolve") => self.resolve(arguments),
            _ => Err("Unknown tool".into()),
        }
    }
}
