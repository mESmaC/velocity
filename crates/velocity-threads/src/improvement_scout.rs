//! Machine-wide improvement scout runner and MCP server (SPEC.md §1, F7 Development speed, #622).
//!
//! Runs at most one bounded, read-only improvement scout session per machine when either:
//! - dispatch slots have been free for 15 minutes with nothing dispatchable, or
//! - 25 lands have happened since the last run.
//!
//! Skips a run when the previous one filed nothing and no new signal has arrived since.
//! Files at most 3 evidenced `self-improvement` issues per run with `Target metric:` lines.

mod digest;

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use agent_client_protocol::v1::schema::{McpServer, McpServerStdio};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::mpsc::unbounded_channel;
use velocity_acp::client::{AgentEvent, AgentInput, AircLaunch, SessionSetup};
use velocity_core::improvement_scout::{
    IMPROVEMENT_SCOUT_NICK, ImprovementRunRecord, ImprovementScheduleInput, ImprovementScoutState,
    ImprovementTrigger, MAX_ISSUES_PER_RUN, SELF_IMPROVEMENT_LABEL, ToolSuiteRepository,
    improvement_run_due, improvement_scout_prompt,
};
use velocity_core::text_budget::{json_text_page, preview};
use velocity_core::worktree_settings::WorktreePreferences;
use velocity_github::mcp_stdio::ToolServer;
use velocity_github::{
    ImprovementIssueDraft, IssueClient, IssueHost, IssueReviewToken, ScoutIssueOutcome,
    ScoutReport, read_scout_reports,
};
use velocity_rosen::{AgentWorkspace, ProjectServer, review_swarm};
use velocity_store::duty::DutyAgentItem;
use velocity_store::{ProjectItem, Settings};

use crate::improvement_scout::digest::collect_digest;
use crate::launch::{Location, agent_launch};
use crate::review_swarm::message;

pub const IMPROVEMENT_SCOUT_SWITCH: &str = "--improvement-scout-mcp";

const STATE_FILE: &str = "improvement-scout.json";
const RUN_LIMIT: Duration = Duration::from_secs(20 * 60);
const STOP_GRACE: Duration = Duration::from_secs(60);
const START_RETRY: Duration = Duration::from_secs(5 * 60);

/// One tool-suite repository the improvement scout can inspect and file issues in.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TargetRepositoryConfig {
    pub repository: String,
    #[serde(default)]
    pub host: IssueHost,
    pub directory: PathBuf,
}

/// Machine-wide snapshot handed to [`ImprovementScouts::tick`].
#[derive(Clone)]
pub struct ImprovementFleetSnapshot {
    pub settings: Settings,
    pub projects: Vec<ProjectItem>,
    /// Workspace preferences for the project hosting the read-only checkout, if any.
    pub host_project: Option<(ProjectItem, WorktreePreferences)>,
    pub repositories: Vec<TargetRepositoryConfig>,
    pub slots_free_and_nothing_dispatchable: bool,
    pub external_idle_since: Option<u64>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct ImprovementScoutLaunch {
    workspace: PathBuf,
    repositories: Vec<TargetRepositoryConfig>,
    report: PathBuf,
    #[serde(default = "default_max_issues")]
    max_issues: usize,
}

fn default_max_issues() -> usize {
    MAX_ISSUES_PER_RUN
}

#[derive(Default)]
struct RunnerState {
    running: bool,
    failed_start: Option<Instant>,
    persisted: ImprovementScoutState,
}

/// Machine-wide improvement scout runner. Cheap to clone; every clone shares one lock so
/// at most one improvement scout runs at a time on the machine (#622).
#[derive(Clone)]
pub struct ImprovementScouts {
    state: Arc<Mutex<RunnerState>>,
    executable: PathBuf,
    app_data: PathBuf,
    airc: Option<AircLaunch>,
    logger: Arc<dyn Fn(&str) + Send + Sync>,
}

impl ImprovementScouts {
    pub fn new(
        executable: PathBuf,
        app_data: PathBuf,
        airc: Option<AircLaunch>,
        logger: impl Fn(&str) + Send + Sync + 'static,
    ) -> Self {
        let persisted = load_state(&app_data).unwrap_or_default();
        Self {
            state: Arc::new(Mutex::new(RunnerState {
                running: false,
                failed_start: None,
                persisted,
            })),
            executable,
            app_data,
            airc,
            logger: Arc::new(logger),
        }
    }

    pub fn is_running(&self) -> bool {
        self.lock().running
    }

    /// Evaluates machine-level lands, idle slots, signal fingerprint and one-at-a-time gating,
    /// starting one improvement scout session when due.
    pub async fn tick(&self, snapshot: ImprovementFleetSnapshot) {
        self.observe_rosen_lands(&snapshot.projects).await;
        let now = unix_seconds();
        let Some((total_lands, lands_since, log_cursor)) = self.check_threshold(&snapshot, now)
        else {
            return;
        };
        let preview_trigger =
            if lands_since >= velocity_core::improvement_scout::LANDS_TRIGGER_COUNT {
                ImprovementTrigger::LandsReached { lands: lands_since }
            } else {
                ImprovementTrigger::IdleSlots {
                    idle_seconds: velocity_core::improvement_scout::IDLE_TRIGGER_SECONDS,
                }
            };
        let app_data = self.app_data.clone();
        let projects = snapshot.projects.clone();
        let Ok(collected) = tokio::task::spawn_blocking(move || {
            collect_digest(
                &app_data,
                &projects,
                preview_trigger,
                total_lands,
                lands_since,
                log_cursor,
            )
        })
        .await
        else {
            return;
        };
        let Some(trigger) = self.try_reserve(&snapshot, &collected.fingerprint) else {
            return;
        };
        (self.logger)(&format!(
            "Starting improvement scout ({})",
            trigger.summary()
        ));
        let runner = self.clone();
        tokio::spawn(async move {
            runner
                .execute_run(snapshot, trigger, collected, total_lands)
                .await;
        });
    }

    fn check_threshold(
        &self,
        snapshot: &ImprovementFleetSnapshot,
        now: u64,
    ) -> Option<(u64, u64, u64)> {
        let mut state = self.lock();
        state.persisted.observe_slots(
            snapshot.slots_free_and_nothing_dispatchable,
            now,
            snapshot.external_idle_since,
        );
        let _ = save_state(&self.app_data, &state.persisted);
        let in_retry_cooldown = state
            .failed_start
            .is_some_and(|failed| failed.elapsed() < START_RETRY);
        if in_retry_cooldown
            || !state.persisted.threshold_met(
                state.running,
                snapshot.slots_free_and_nothing_dispatchable,
                now,
            )
        {
            return None;
        }
        Some((
            state.persisted.total_lands,
            state.persisted.lands_since_last_run(),
            state.persisted.log_cursor_secs(),
        ))
    }

    fn try_reserve(
        &self,
        snapshot: &ImprovementFleetSnapshot,
        fingerprint: &str,
    ) -> Option<ImprovementTrigger> {
        let mut state = self.lock();
        let trigger = improvement_run_due(
            &state.persisted,
            &ImprovementScheduleInput {
                running: state.running,
                now: unix_seconds(),
                slots_free_and_nothing_dispatchable: snapshot.slots_free_and_nothing_dispatchable,
                signal_fingerprint: fingerprint,
            },
        )?;
        state.running = true;
        Some(trigger)
    }

    async fn observe_rosen_lands(&self, projects: &[ProjectItem]) {
        let mut updates = Vec::new();
        for project in projects.iter().filter(|project| project.rosen_in_use()) {
            let server = ProjectServer::new(&self.app_data, project.id);
            if let Ok((_, entries, _)) =
                velocity_rosen::post_land::recent_land_history(&server, 60).await
            {
                let commits: Vec<String> = entries.into_iter().map(|entry| entry.commit).collect();
                updates.push((project.id, commits));
            }
        }
        if updates.is_empty() {
            return;
        }
        let mut state = self.lock();
        let mut changed = false;
        for (project_id, commits) in updates {
            let previous = state.persisted.last_seen_commit.get(&project_id).cloned();
            let delta = state.persisted.observe_project_lands(project_id, &commits);
            if delta > 0 || previous.is_none() {
                changed = true;
            }
        }
        if changed {
            let _ = save_state(&self.app_data, &state.persisted);
        }
    }

    async fn execute_run(
        &self,
        snapshot: ImprovementFleetSnapshot,
        trigger: ImprovementTrigger,
        collected: digest::CollectedDigest,
        lands_at_run: u64,
    ) {
        let started_at = unix_seconds();
        let outcome = self.run_bounded_session(&snapshot, &collected).await;
        let finished_at = unix_seconds();
        let mut state = self.lock();
        state.running = false;
        match outcome {
            Ok(filed_count) => {
                state.failed_start = None;
                state.persisted.record_run_finished(ImprovementRunRecord {
                    started_at,
                    finished_at,
                    filed_count,
                    lands_at_run,
                    log_cursor_secs: collected.log_cursor_secs.max(finished_at),
                    signal_fingerprint: collected.fingerprint,
                });
                let _ = save_state(&self.app_data, &state.persisted);
                drop(state);
                (self.logger)(&format!(
                    "Improvement scout finished ({}): {filed_count} issue(s) filed",
                    trigger.summary()
                ));
            }
            Err(error) => {
                state.failed_start = Some(Instant::now());
                drop(state);
                (self.logger)(&format!("Improvement scout did not finish: {error}"));
            }
        }
    }

    async fn run_bounded_session(
        &self,
        snapshot: &ImprovementFleetSnapshot,
        collected: &digest::CollectedDigest,
    ) -> Result<u32, String> {
        let run_dir = self.app_data.join("improvement-scout");
        fs::create_dir_all(&run_dir).map_err(|error| error.to_string())?;
        let launch_path = run_dir.join("launch.json");
        let report_path = run_dir.join("report.jsonl");
        let _ = fs::remove_file(&report_path);

        let (workspace_dir, rosen_handle) = self.prepare_workspace(snapshot, &run_dir).await?;
        let launch = ImprovementScoutLaunch {
            workspace: workspace_dir.clone(),
            repositories: snapshot.repositories.clone(),
            report: report_path.clone(),
            max_issues: MAX_ISSUES_PER_RUN,
        };
        let bytes = serde_json::to_vec_pretty(&launch).map_err(|error| error.to_string())?;
        fs::write(&launch_path, bytes).map_err(|error| error.to_string())?;

        let allowed_names: Vec<&str> = snapshot
            .repositories
            .iter()
            .map(|repo| repo.repository.as_str())
            .collect();
        let prompt = improvement_scout_prompt(&collected.markdown, &allowed_names);
        let session_result = self
            .run_agent_turn(
                &snapshot.settings,
                snapshot
                    .host_project
                    .as_ref()
                    .map(|(project, _)| project.id),
                &workspace_dir,
                rosen_handle.as_ref(),
                &launch_path,
                &prompt,
            )
            .await;

        let filed_count = count_filed_reports(&report_path);
        let _ = fs::remove_file(&launch_path);
        let _ = fs::remove_file(&report_path);
        if let Some((server, workspace)) = rosen_handle {
            let _ = discard_workspace(&server, &workspace).await;
        }
        session_result.map(|()| filed_count)
    }

    async fn prepare_workspace(
        &self,
        snapshot: &ImprovementFleetSnapshot,
        run_dir: &Path,
    ) -> Result<(PathBuf, Option<(ProjectServer, AgentWorkspace)>), String> {
        if let Some((project, preferences)) = &snapshot.host_project
            && project.rosen_in_use()
        {
            let server = ProjectServer::new(&self.app_data, project.id);
            let root = Location::new(project, preferences)
                .project_root()
                .map_err(|error| error.to_string())?
                .join(IMPROVEMENT_SCOUT_NICK);
            if root.join(".rosen").exists()
                && let Ok(stale) =
                    review_swarm::open_workspace(&server, IMPROVEMENT_SCOUT_NICK, &root).await
            {
                let _ = discard_workspace(&server, &stale).await;
            }
            let workspace =
                velocity_rosen::create_workspace(&server, IMPROVEMENT_SCOUT_NICK, &root, None)
                    .await
                    .map_err(|error| error.to_string())?;
            return Ok((workspace.root.clone(), Some((server, workspace))));
        }
        let fallback = run_dir.join("workspace");
        fs::create_dir_all(&fallback).map_err(|error| error.to_string())?;
        Ok((fallback, None))
    }

    async fn run_agent_turn(
        &self,
        settings: &Settings,
        host_project_id: Option<u32>,
        workspace_dir: &Path,
        rosen_handle: Option<&(ProjectServer, AgentWorkspace)>,
        launch_path: &Path,
        prompt: &str,
    ) -> Result<(), String> {
        let agent = scout_agent(settings);
        let (mut agent_launch, modes) =
            agent_launch(settings, agent.provider).map_err(|error| error.to_string())?;
        if let Some((server, workspace)) = rosen_handle {
            agent_launch.env.extend(velocity_rosen::agent_environment(
                server,
                IMPROVEMENT_SCOUT_NICK,
                &workspace.change,
            ));
        }
        let mut setup = SessionSetup::new(workspace_dir.to_path_buf(), modes).with_run_settings(
            crate::titles::duty_run_settings(settings, &agent, settings.agents.sweep_effort),
        );
        setup.role = velocity_core::tool_policy::ThreadRole::Scout;
        setup.usage_role = Some(velocity_graph::token_usage::Role::Review);
        if let (Some(airc), Some(project_id)) = (self.airc.as_ref(), host_project_id) {
            setup.airc = Some(airc.duty(project_id, IMPROVEMENT_SCOUT_NICK));
        }
        setup.shared_servers.push(McpServer::Stdio(
            McpServerStdio::new("velocity-improvement-scout", &self.executable).args(vec![
                IMPROVEMENT_SCOUT_SWITCH.into(),
                launch_path.display().to_string(),
            ]),
        ));
        let (input, inputs) = unbounded_channel();
        input
            .send(message(prompt))
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
        drop(input);
        session.outcome.await.map_err(|error| error.to_string())?
    }

    fn lock(&self) -> MutexGuard<'_, RunnerState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

fn scout_agent(settings: &Settings) -> DutyAgentItem {
    let provider = settings.new_work_choice();
    DutyAgentItem {
        nickname: IMPROVEMENT_SCOUT_NICK.into(),
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

async fn discard_workspace(
    server: &ProjectServer,
    workspace: &AgentWorkspace,
) -> Result<(), String> {
    let _ = review_swarm::snapshot_head(
        server,
        IMPROVEMENT_SCOUT_NICK,
        &workspace.change,
        &workspace.root,
    )
    .await;
    velocity_rosen::dispatch::abandon_finished(server, &workspace.change)
        .await
        .map_err(|error| error.to_string())
}

fn count_filed_reports(path: &Path) -> u32 {
    let Ok((reports, _)) = read_scout_reports(path, 0) else {
        return 0;
    };
    reports
        .into_iter()
        .filter(|report| matches!(report, ScoutReport::Filed { .. }))
        .count() as u32
}

fn load_state(app_data: &Path) -> Result<ImprovementScoutState, String> {
    match fs::read(app_data.join(STATE_FILE)) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(|error| error.to_string()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Ok(ImprovementScoutState::default())
        }
        Err(error) => Err(error.to_string()),
    }
}

fn save_state(app_data: &Path, state: &ImprovementScoutState) -> Result<(), String> {
    fs::create_dir_all(app_data).map_err(|error| error.to_string())?;
    let bytes = serde_json::to_vec_pretty(state).map_err(|error| error.to_string())?;
    let partial = app_data.join(format!("{STATE_FILE}.partial-{}", std::process::id()));
    let mut file = fs::File::create(&partial).map_err(|error| error.to_string())?;
    file.write_all(&bytes).map_err(|error| error.to_string())?;
    file.sync_all().map_err(|error| error.to_string())?;
    fs::rename(&partial, app_data.join(STATE_FILE)).map_err(|error| error.to_string())
}

fn unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Stdio MCP server for one improvement scout run (#622).
///
/// Enforces:
/// - only tool-suite repositories configured in the launch file may be targeted;
/// - duplicate search (`improvement_scout_issues`) is required per repository before filing;
/// - at most `max_issues` (3) issues may be published per run;
/// - every filed issue carries `self-improvement`, evidence, and a `Target metric:` line.
pub struct ImprovementScoutServer {
    launch: ImprovementScoutLaunch,
    reviewed: BTreeMap<String, IssueReviewToken>,
    filed_count: usize,
}

impl ImprovementScoutServer {
    pub fn open(path: &Path) -> Result<Self, String> {
        let bytes = fs::read(path).map_err(|error| error.to_string())?;
        let launch = serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
        Ok(Self {
            launch,
            reviewed: BTreeMap::new(),
            filed_count: 0,
        })
    }

    fn resolve_target(&self, requested: &str) -> Result<&TargetRepositoryConfig, String> {
        let canonical = ToolSuiteRepository::parse(requested)
            .map(ToolSuiteRepository::repository)
            .unwrap_or(requested.trim());
        self.launch
            .repositories
            .iter()
            .find(|config| config.repository.eq_ignore_ascii_case(canonical))
            .ok_or_else(|| {
                let allowed: Vec<&str> = self
                    .launch
                    .repositories
                    .iter()
                    .map(|config| config.repository.as_str())
                    .collect();
                format!(
                    "Repository `{requested}` is not an allowed tool-suite target ({})",
                    allowed.join(", ")
                )
            })
    }

    fn client(&self, target: &TargetRepositoryConfig) -> Result<IssueClient, String> {
        IssueClient::for_host(&target.directory, target.host, &target.repository)
            .map_err(|error| error.to_string())
    }

    fn issues(&mut self, arguments: Value) -> Result<Value, String> {
        #[derive(Deserialize)]
        struct Request {
            repository: String,
            query: String,
            #[serde(default)]
            limit: Option<usize>,
            #[serde(default)]
            offset: usize,
        }
        let request: Request =
            serde_json::from_value(arguments).map_err(|error| error.to_string())?;
        let target = self.resolve_target(&request.repository)?.clone();
        let (page, token) = self
            .client(&target)?
            .search_scout_inventory(&request.query, request.limit, request.offset)
            .map_err(|error| error.to_string())?;
        self.reviewed
            .insert(target.repository.to_ascii_lowercase(), token);
        Ok(page)
    }

    fn issue(&self, arguments: Value) -> Result<Value, String> {
        #[derive(Deserialize)]
        struct Request {
            repository: String,
            number: u32,
            #[serde(default)]
            offset: usize,
        }
        let request: Request =
            serde_json::from_value(arguments).map_err(|error| error.to_string())?;
        let target = self.resolve_target(&request.repository)?;
        let issue = self
            .client(target)?
            .read_issue_detail(request.number, 0)
            .map_err(|error| error.to_string())?;
        let (body, next_offset) = json_text_page(&issue.body, request.offset, 5_000)
            .ok_or_else(|| "Use the returned byte offset".to_owned())?;
        Ok(json!({
            "repository": target.repository,
            "number": issue.number,
            "state": issue.state,
            "title": preview(&issue.title, 300),
            "url": issue.url,
            "body": body,
            "next_offset": next_offset,
        }))
    }

    fn labels(&self, arguments: Value) -> Result<Value, String> {
        #[derive(Deserialize)]
        struct Request {
            repository: String,
            #[serde(default)]
            query: String,
        }
        let request: Request =
            serde_json::from_value(arguments).map_err(|error| error.to_string())?;
        let target = self.resolve_target(&request.repository)?;
        let labels = self
            .client(target)?
            .list_labels_matching(&request.query)
            .map_err(|error| error.to_string())?;
        Ok(json!({
            "repository": target.repository,
            "labels": labels,
            "auto_ensured": SELF_IMPROVEMENT_LABEL,
        }))
    }

    fn file(&mut self, arguments: Value) -> Result<Value, String> {
        #[derive(Deserialize)]
        struct Request {
            repository: String,
            title: String,
            description: String,
            evidence: String,
            target_metric: String,
            duplicate_assessment: String,
            #[serde(default)]
            duplicate_of: Option<u32>,
            #[serde(default)]
            labels: Vec<String>,
            #[serde(default)]
            blocked_by: Vec<String>,
            #[serde(default)]
            publish: bool,
        }
        let request: Request =
            serde_json::from_value(arguments).map_err(|error| error.to_string())?;
        if request.publish && self.filed_count >= self.launch.max_issues {
            return Err(format!(
                "Maximum of {} issues per improvement scout run reached; end your turn.",
                self.launch.max_issues
            ));
        }
        let target = self.resolve_target(&request.repository)?.clone();
        let reviewed = self
            .reviewed
            .get(&target.repository.to_ascii_lowercase())
            .cloned()
            .ok_or_else(|| {
                format!(
                    "Search open and closed issues in {} with improvement_scout_issues before filing",
                    target.repository
                )
            })?;
        let draft = ImprovementIssueDraft {
            title: request.title,
            description: request.description,
            evidence: request.evidence,
            target_metric: request.target_metric,
            labels: request.labels,
            reviewed,
            duplicate_assessment: request.duplicate_assessment,
            duplicate_of: request.duplicate_of,
            blocked_by: request.blocked_by,
        };
        let outcome = self
            .client(&target)?
            .file_improvement_issue(&draft, request.publish)
            .map_err(|error| error.to_string())?;
        if request.publish {
            self.record_outcome(&outcome)?;
        }
        serde_json::to_value(&outcome).map_err(|error| error.to_string())
    }

    fn record_outcome(&mut self, outcome: &ScoutIssueOutcome) -> Result<(), String> {
        let report = match outcome {
            ScoutIssueOutcome::Filed { issue, .. } => {
                self.filed_count = self.filed_count.saturating_add(1);
                ScoutReport::Filed {
                    number: issue.number,
                    title: issue.title.clone(),
                    evidence: String::new(),
                }
            }
            ScoutIssueOutcome::Duplicate { issue, .. } => ScoutReport::Duplicate {
                number: issue.number,
                title: issue.title.clone(),
            },
            ScoutIssueOutcome::Prepared { .. } => return Ok(()),
        };
        velocity_github::mcp_stdio::append_report(&self.launch.report, &report)
            .map_err(|error| error.to_string())
    }
}

impl ToolServer for ImprovementScoutServer {
    fn name(&self) -> &'static str {
        "velocity-improvement-scout"
    }

    fn instructions(&self) -> String {
        format!(
            "Search open and closed issues in the target repository with improvement_scout_issues \
             before filing. File at most {} evidenced self-improvement issues per run with \
             improvement_scout_file, each with a Target metric line naming the fleet metric and \
             direction.",
            self.launch.max_issues
        )
    }

    fn tools(&self) -> Vec<Value> {
        let mut list = read_tools();
        list.push(file_tool());
        list
    }

    fn call(&mut self, name: Option<&str>, arguments: Value) -> Result<Value, String> {
        match name {
            Some("improvement_scout_issues") => self.issues(arguments),
            Some("improvement_scout_issue") => self.issue(arguments),
            Some("improvement_scout_labels") => self.labels(arguments),
            Some("improvement_scout_file") => self.file(arguments),
            _ => Err("Unknown improvement scout tool".into()),
        }
    }
}

fn read_tools() -> Vec<Value> {
    let string = json!({"type": "string"});
    vec![
        json!({
            "name": "improvement_scout_issues",
            "description": "Search open and closed issues in mESmaC/velocity, mESmaC/rosen or mESmaC/collapse.sh for duplicates before filing.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "repository": string,
                    "query": {"type": "string", "minLength": 1},
                    "limit": {"type": "integer", "minimum": 1, "default": 10},
                    "offset": {"type": "integer", "minimum": 0}
                },
                "required": ["repository", "query"],
                "additionalProperties": false
            }
        }),
        json!({
            "name": "improvement_scout_issue",
            "description": "Read one issue body by number in a tool-suite repository.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "repository": string,
                    "number": {"type": "integer", "minimum": 1},
                    "offset": {"type": "integer", "minimum": 0}
                },
                "required": ["repository", "number"],
                "additionalProperties": false
            }
        }),
        json!({
            "name": "improvement_scout_labels",
            "description": "List labels in a tool-suite repository. `self-improvement` is automatically added and created if missing.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "repository": string,
                    "query": string
                },
                "required": ["repository"],
                "additionalProperties": false
            }
        }),
    ]
}

fn file_tool() -> Value {
    let string = json!({"type": "string"});
    let strings = json!({"type": "array", "items": {"type": "string"}});
    json!({
        "name": "improvement_scout_file",
        "description": "Preview (publish: false) or file (publish: true) one self-improvement issue (max 3 per run). Automatically adds and creates the `self-improvement` label if missing and appends `Target metric:`.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "repository": string,
                "title": string,
                "description": string,
                "evidence": {"type": "string", "description": "Concrete log lines, stats, friction counts or code citations."},
                "target_metric": {"type": "string", "description": "Fleet metric and direction, e.g. `idle slot-minutes per day, down`."},
                "duplicate_assessment": {"type": "string", "description": "How this differs from every similar open or closed issue."},
                "duplicate_of": {"type": "integer"},
                "labels": strings,
                "blocked_by": strings,
                "publish": {"type": "boolean"}
            },
            "required": [
                "repository",
                "title",
                "description",
                "evidence",
                "target_metric",
                "duplicate_assessment",
                "publish"
            ],
            "additionalProperties": false
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // Risk (#622): the MCP server must enforce the 3-issue cap per run even if an agent
    // tries to file a fourth issue.
    #[test]
    fn mcp_server_refuses_fourth_published_issue() -> Result<(), Box<dyn std::error::Error>> {
        let dir = std::env::temp_dir().join(format!(
            "velocity-improvement-scout-cap-{}",
            uuid::Uuid::new_v4()
        ));
        fs::create_dir_all(&dir)?;
        let launch_path = dir.join("launch.json");
        let launch = ImprovementScoutLaunch {
            workspace: dir.clone(),
            repositories: vec![TargetRepositoryConfig {
                repository: "mESmaC/velocity".into(),
                host: IssueHost::GitHub,
                directory: dir.clone(),
            }],
            report: dir.join("report.jsonl"),
            max_issues: MAX_ISSUES_PER_RUN,
        };
        fs::write(&launch_path, serde_json::to_vec(&launch)?)?;
        let mut server = ImprovementScoutServer::open(&launch_path)?;
        server.filed_count = MAX_ISSUES_PER_RUN;
        let outcome = server.call(
            Some("improvement_scout_file"),
            json!({
                "repository": "mESmaC/velocity",
                "title": "Fourth issue should be refused",
                "description": "Remove redundant poll.",
                "evidence": "velocity.log shows 20 idle minutes",
                "target_metric": "idle slot-minutes per day, down",
                "duplicate_assessment": "Checked open and closed issues.",
                "publish": true
            }),
        );
        let Err(error) = outcome else {
            return Err("fourth published issue must be refused".into());
        };
        assert!(error.contains("Maximum of 3 issues"));
        let _ = fs::remove_dir_all(&dir);
        Ok(())
    }
}
