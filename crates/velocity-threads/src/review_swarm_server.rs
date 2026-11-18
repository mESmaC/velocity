//! A post-land worker's tools. Completion is a durable report, not an approval. Velocity
//! records coverage only after any named proof or fix-forward change has landed.

use std::path::{Path, PathBuf};

use crate::launch::Location;
use crate::review_swarm::ReviewSwarms;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use velocity_acp::client::{QUEUED_RUN_PORT_ENV, QUEUED_RUN_TOKEN_ENV, request_queued_run};
use velocity_github::mcp_stdio::{ReviewDiffRequest, ToolServer, review_diff_tool};
use velocity_github::{IssueClient, ScoutServer, read_scout_reports};
use velocity_rosen::ProjectServer;
use velocity_rosen::duty::{self, ReviewTrees};
use velocity_rosen::review_swarm::{self, LandedChange};
use velocity_store::duty::ScoutRoster;

pub const REVIEW_SWARM_SWITCH: &str = "--review-swarm-mcp";

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct SwarmLaunch {
    pub app_data: PathBuf,
    pub project: u32,
    pub nickname: String,
    pub target: Option<LandedChange>,
    pub workspace: PathBuf,
    pub change: String,
    pub repository: Option<String>,
    #[serde(default)]
    pub source_issue: Option<velocity_store::issues::ThreadIssue>,
    pub report: PathBuf,
    pub completion: PathBuf,
    #[serde(default)]
    pub scout_base: Option<String>,
}

impl SwarmLaunch {
    pub fn save(&self, path: &Path) -> Result<(), String> {
        write_record(path, self)
    }

    pub fn read(path: &Path) -> Result<Self, String> {
        let bytes = std::fs::read(path).map_err(|error| error.to_string())?;
        serde_json::from_slice(&bytes).map_err(|error| error.to_string())
    }

    pub fn piece_for(nickname: &str, target: Option<&LandedChange>) -> String {
        target.map_or_else(
            || format!("scout:{nickname}"),
            |target| review_swarm::review_piece(&target.change, &target.commit),
        )
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct Completion {
    pub issues_filed: bool,
    #[serde(default)]
    pub source_issue_satisfied: Option<bool>,
    pub summary: String,
    /// The model must identify proof-list applicability even when the list is empty.
    pub proof_assessment: String,
    pub proof_features: Vec<String>,
    #[serde(default)]
    pub head: String,
}

pub struct ReviewSwarmServer {
    launch_file: PathBuf,
    launch: SwarmLaunch,
    scout: ScoutServer,
    runtime: tokio::runtime::Runtime,
    review_trees: Option<ReviewTrees>,
}

impl ReviewSwarmServer {
    pub fn open(path: &Path) -> Result<Self, String> {
        let launch = SwarmLaunch::read(path)?;
        let client = launch
            .repository
            .as_deref()
            .ok_or_else(|| "This project has no GitHub repository for findings".to_owned())
            .and_then(|repository| {
                IssueClient::for_repository(&launch.workspace, repository)
                    .map_err(|error| error.to_string())
            });
        let scout = ScoutServer::new(client, launch.nickname.clone(), Some(launch.report.clone()));
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|error| error.to_string())?;
        Ok(Self {
            launch_file: path.to_owned(),
            launch,
            scout,
            runtime,
            review_trees: None,
        })
    }

    pub fn serve(
        &mut self,
        input: impl std::io::BufRead,
        output: impl std::io::Write,
    ) -> std::io::Result<()> {
        velocity_github::mcp_stdio::serve(self, input, output)
    }

    fn change(&mut self, arguments: Value) -> Result<Value, String> {
        let request: ReviewDiffRequest =
            serde_json::from_value(arguments).map_err(|error| error.to_string())?;
        let server = ProjectServer::new(&self.launch.app_data, self.launch.project);
        if let Some(path) = request.path {
            return self.runtime.block_on(crate::rosen_duty_tools::file_page(
                &server,
                self.review_trees.as_ref(),
                &path,
                request.offset,
            ));
        }
        let Some(target) = &self.launch.target else {
            return Ok(
                json!({"work": "Scout the current base against SPEC.md and its proof list"}),
            );
        };
        self.runtime.block_on(async {
            let overview = duty::change_overview(&server, &target.change).await
                .map_err(|error| error.to_string())?;
            let diff = review_swarm::landed_diff_record(&server, target).await
                .map_err(|error| error.to_string())?;
            let files = duty::diff_stat(&diff);
            self.review_trees = Some(ReviewTrees::from_diff(&diff));
            let endpoint = server.endpoint().clone();
            let cache = self.launch.app_data.join("graph").join(self.launch.project.to_string());
            let impact = tokio::task::spawn_blocking(move || {
                let impact = |revision: String, before| {
                    velocity_graph::Index::open_revision(endpoint.clone(), &revision, &cache)
                        .map(|index| velocity_graph::coordination::review_impact(&index, &diff, before))
                        .unwrap_or_else(|error| format!("Graph impact unavailable: {error}"))
                };
                json!({"before": impact(diff.from.to_string(), true),
                    "landed": impact(diff.to.to_string(), false)})
            }).await.map_err(|error| error.to_string())?;
            Ok(json!({"change": target.change, "commit": target.commit,
                "summary": target.summary, "named_snapshots": overview.named, "files": files, "graph_impact": impact,
                "source_issue": self.launch.source_issue,
                "instruction": "Read the referenced issue and SPEC section; verify every claim against the code"}))
        })
    }

    /// Serialise filings across the swarm using a channel claim. The ordinary scout
    /// server still executes evidence and rechecks the issue inventory before publishing.
    fn file(&mut self, arguments: Value) -> Result<Value, String> {
        let server = ProjectServer::new(&self.launch.app_data, self.launch.project);
        let member = self
            .runtime
            .block_on(review_swarm::membership(&server, &self.launch.nickname))
            .map_err(|error| error.to_string())?;
        let piece = "review-swarm:issue-filing";
        if !self
            .runtime
            .block_on(review_swarm::claim(
                &member,
                piece,
                "File evidence for a finding",
            ))
            .map_err(|error| error.to_string())?
        {
            return Err(
                "Another agent is filing; retry scout_issues and scout_file shortly".into(),
            );
        }
        let result = self.scout.call(Some("scout_file"), arguments);
        let released = self
            .runtime
            .block_on(member.release_piece(Some(piece)))
            .map_err(|error| error.to_string());
        released?;
        result
    }

    fn completion_request(&self, arguments: Value) -> Result<Value, String> {
        let completion: Completion =
            serde_json::from_value(arguments).map_err(|error| error.to_string())?;
        self.validate_completion(&completion)?;
        let port = std::env::var(QUEUED_RUN_PORT_ENV).unwrap_or_default();
        if !port.is_empty()
            && self.needs_checks(&completion)
            && !self.proof_paths(&completion.proof_features)?.is_empty()
        {
            return self.queue_completion(&port, &completion);
        }
        self.complete(completion)
    }

    fn queue_completion(&self, port: &str, completion: &Completion) -> Result<Value, String> {
        let port = port.parse::<u16>().map_err(|error| error.to_string())?;
        let token = std::env::var(QUEUED_RUN_TOKEN_ENV).map_err(|error| error.to_string())?;
        let program = std::env::current_exe().map_err(|error| error.to_string())?;
        let completion = serde_json::to_string(completion).map_err(|error| error.to_string())?;
        let mut result = request_queued_run(
            port,
            &token,
            json!({
                "program": program,
                "arguments": [REVIEW_SWARM_SWITCH, self.launch_file, "--complete", completion],
                "class": "verify",
            }),
        )?;
        result["next"] = json!(
            "End your turn. On a pass, name any proof or fix work with rosen commit, then end your turn."
        );
        Ok(result)
    }

    /// Called by the queued command, with the same validation as the foreground route.
    pub fn finish_queued(&self, completion: &str) -> Result<Value, String> {
        let completion = serde_json::from_str(completion).map_err(|error| error.to_string())?;
        self.validate_completion(&completion)?;
        self.complete(completion)
    }

    fn validate_completion(&self, completion: &Completion) -> Result<(), String> {
        if completion.summary.trim().is_empty() || completion.proof_assessment.trim().is_empty() {
            return Err("Explain the review and its applicability to SPEC.md's proof list".into());
        }
        if self.launch.source_issue.is_some() && completion.source_issue_satisfied.is_none() {
            return Err(
                "Report whether this land meets the source issue's acceptance criteria".into(),
            );
        }
        let reports = read_scout_reports(&self.launch.report, 0)
            .map(|(reports, _)| reports)
            .unwrap_or_default();
        if completion.issues_filed && reports.is_empty() {
            return Err(
                "File or identify an existing issue through scout_file, with evidence, first"
                    .into(),
            );
        }
        Ok(())
    }

    fn needs_checks(&self, completion: &Completion) -> bool {
        !completion.issues_filed && completion.source_issue_satisfied != Some(false)
    }

    fn complete(&self, mut completion: Completion) -> Result<Value, String> {
        let server = ProjectServer::new(&self.launch.app_data, self.launch.project);
        let head = self
            .runtime
            .block_on(review_swarm::snapshot_head(
                &server,
                &self.launch.nickname,
                &self.launch.change,
                &self.launch.workspace,
            ))
            .map_err(|error| error.to_string())?;
        if self.needs_checks(&completion) {
            self.check_proofs(&completion.proof_features)?;
        }
        completion.head = self
            .runtime
            .block_on(review_swarm::snapshot_head(
                &server,
                &self.launch.nickname,
                &self.launch.change,
                &self.launch.workspace,
            ))
            .map_err(|error| error.to_string())?;
        if completion.head != head {
            return Err("Sources changed during verification; complete again".into());
        }
        self.record_source_objection(&server, &completion)?;
        write_record(&self.launch.completion, &completion)?;
        Ok(
            json!({"recorded": true, "instruction": "Name any proof or fix work with rosen commit, then end your turn. Velocity records coverage after that work lands."}),
        )
    }

    fn record_source_objection(
        &self,
        server: &ProjectServer,
        completion: &Completion,
    ) -> Result<(), String> {
        // A rejected source issue must stay open even if this reviewer's own
        // repair has a conflict or its session is interrupted before finishing.
        if self.launch.source_issue.is_some()
            && completion.source_issue_satisfied == Some(false)
            && let Some(target) = &self.launch.target
        {
            self.runtime
                .block_on(review_swarm::mark(
                    server,
                    &self.launch.nickname,
                    target,
                    true,
                ))
                .map_err(|error| error.to_string())?;
        }
        Ok(())
    }

    fn proof_paths(&self, features: &[String]) -> Result<Vec<String>, String> {
        let server = ProjectServer::new(&self.launch.app_data, self.launch.project);
        let mut paths = self
            .runtime
            .block_on(velocity_rosen::duty::changed_paths(
                &server,
                &self.launch.change,
            ))
            .map_err(|error| error.to_string())?;
        if let Some(target) = &self.launch.target {
            paths.extend(
                self.runtime
                    .block_on(review_swarm::landed_paths(&server, target))
                    .map_err(|error| error.to_string())?,
            );
        }
        for feature in features {
            if feature.is_empty()
                || !feature
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_')
            {
                return Err("A proof feature must be a folder name under proofs/".into());
            }
            let design = format!("proofs/{feature}/Design.lean");
            let fixture = format!(
                "proofs/fixtures/{}.tsv",
                velocity_proofs::fixture_name(feature)
            );
            for path in [&design, &fixture] {
                if !self.launch.workspace.join(path).is_file() {
                    return Err(format!(
                        "Missing {path}; write the proof and fixtures or file the gap"
                    ));
                }
            }
            paths.push(design);
        }
        paths.retain(|path| path.starts_with("proofs/"));
        Ok(paths)
    }

    fn check_proofs(&self, features: &[String]) -> Result<(), String> {
        let paths = self.proof_paths(features)?;
        if paths.is_empty() {
            return Ok(());
        }
        let report = review_swarm::verify(&self.launch.workspace, || {
            velocity_proofs::check_build_tree(&self.launch.workspace, &paths)
        })
        .map_err(|error| error.to_string())?
        .map_err(|error| error.to_string())?;
        if !report.passed() || matches!(&report.build, velocity_proofs::CheckStep::Failed(_)) {
            return Err(report
                .summary()
                .unwrap_or_else(|| "Proof checks failed".into()));
        }
        Ok(())
    }
}

fn write_record(path: &Path, record: &impl Serialize) -> Result<(), String> {
    let partial = path.with_extension("partial");
    let mut file = std::fs::File::create(&partial).map_err(|error| error.to_string())?;
    serde_json::to_writer(&mut file, record).map_err(|error| error.to_string())?;
    file.sync_all().map_err(|error| error.to_string())?;
    drop(file);
    std::fs::rename(partial, path).map_err(|error| error.to_string())
}

impl ToolServer for ReviewSwarmServer {
    fn name(&self) -> &'static str {
        "velocity-review-swarm"
    }

    fn instructions(&self) -> String {
        "Review landed code, scout and verify proof-list features. Start with review_swarm_change for files, diff stats and graph impact. Request patches one path at a time, use scout_issues/scout_file for evidence, then call review_swarm_complete. No review gates a land.".into()
    }

    fn tools(&self) -> Vec<Value> {
        let mut tools = self.scout.tools();
        tools.push(review_diff_tool("review_swarm_change",
            "Landed change overview: file list with diff stats, graph impact, issue references and named snapshots."));
        tools.push(json!({"name": "review_swarm_complete", "description": "Record a finished review or sweep. Proof features require Design.lean and fixtures. If this queues Lean verification, end your turn and continue from the result. Coverage is applied after any named fix or proof lands.",
            "inputSchema": {"type": "object", "properties": {
                "issues_filed": {"type": "boolean"}, "summary": {"type": "string"},
                "source_issue_satisfied": {"type": "boolean", "description": "Required when review_swarm_change identifies a source issue. True only when the landed implementation meets that issue's acceptance criteria."},
                "proof_assessment": {"type": "string", "description": "Which F17 proof-list features this touches, or why none apply"},
                "proof_features": {"type": "array", "items": {"type": "string"}, "description": "Every affected feature's folder under proofs/"}},
                "required": ["issues_filed", "summary", "proof_assessment", "proof_features"], "additionalProperties": false}}));
        tools
    }

    fn call(&mut self, name: Option<&str>, arguments: Value) -> Result<Value, String> {
        match name {
            Some("review_swarm_change") => self.change(arguments),
            Some("review_swarm_complete") => self.completion_request(arguments),
            Some("scout_file") => self.file(arguments),
            _ => self.scout.call(name, arguments),
        }
    }
}

impl SwarmLaunch {
    pub fn for_job(
        runner: &ReviewSwarms,
        roster: &ScoutRoster,
        index: usize,
        target: Option<review_swarm::LandedChange>,
    ) -> Result<Self, String> {
        let nickname = roster.scouts[index].nickname.clone();
        let root = Location::new(&roster.project, &roster.worktrees)
            .project_root()
            .map_err(|error| error.to_string())?;
        let basename = format!("{}-{nickname}", roster.project.id);
        let repository = roster.project.github_remote.clone().or_else(|| {
            velocity_github::IssueClient::discover(std::path::Path::new(
                &roster.project.repository_path,
            ))
            .ok()
            .map(|client| client.repository().to_owned())
        });
        Ok(Self {
            app_data: runner.app_data.clone(),
            project: roster.project.id,
            nickname,
            source_issue: target.as_ref().and_then(|target| {
                roster
                    .project
                    .threads
                    .iter()
                    .find(|thread| thread.rosen_change.as_deref() == Some(&target.change))
                    .and_then(|thread| thread.issue.clone())
            }),
            target,
            workspace: root.join(format!("review-{}", roster.scouts[index].nickname)),
            change: String::new(),
            repository,
            report: runner.reports.join(format!("{basename}-issues.jsonl")),
            completion: runner.reports.join(format!("{basename}-complete.json")),
            scout_base: None,
        })
    }
}
