//! A post-land worker's tools. Completion is a durable report, not an approval. Velocity
//! records coverage only after any named proof or fix-forward change has landed.

use std::path::{Path, PathBuf};

use crate::launch::Location;
use crate::review_swarm::ReviewSwarms;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use velocity_github::mcp_stdio::ToolServer;
use velocity_github::{IssueClient, ScoutServer, read_scout_reports};
use velocity_rosen::ProjectServer;
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
            |target| format!("review:{}:{}", target.change, target.commit),
        )
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct Completion {
    pub issues_filed: bool,
    pub summary: String,
    /// The model must identify proof-list applicability even when the list is empty.
    pub proof_assessment: String,
    pub proof_features: Vec<String>,
    #[serde(default)]
    pub head: String,
}

pub struct ReviewSwarmServer {
    launch: SwarmLaunch,
    scout: ScoutServer,
    runtime: tokio::runtime::Runtime,
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
            launch,
            scout,
            runtime,
        })
    }

    pub fn serve(
        &mut self,
        input: impl std::io::BufRead,
        output: impl std::io::Write,
    ) -> std::io::Result<()> {
        velocity_github::mcp_stdio::serve(self, input, output)
    }

    fn change(&self) -> Result<Value, String> {
        let Some(target) = &self.launch.target else {
            return Ok(
                json!({"work": "Scout the current base against SPEC.md and its proof list"}),
            );
        };
        let server = ProjectServer::new(&self.launch.app_data, self.launch.project);
        self.runtime.block_on(async {
            let overview = velocity_rosen::duty::change_overview(&server, &target.change).await
                .map_err(|error| error.to_string())?;
            let diff = review_swarm::landed_diff(&server, target).await
                .map_err(|error| error.to_string())?;
            Ok(json!({"change": target.change, "commit": target.commit,
                "summary": target.summary, "named_snapshots": overview.named, "diff": diff,
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

    fn complete(&self, arguments: Value) -> Result<Value, String> {
        let mut completion: Completion =
            serde_json::from_value(arguments).map_err(|error| error.to_string())?;
        if completion.summary.trim().is_empty() || completion.proof_assessment.trim().is_empty() {
            return Err("Explain the review and its applicability to SPEC.md's proof list".into());
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
        if !completion.issues_filed {
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
        write_record(&self.launch.completion, &completion)?;
        Ok(
            json!({"recorded": true, "instruction": "Name any proof or fix work with rosen commit, then end your turn. Velocity records coverage after that work lands."}),
        )
    }

    fn check_proofs(&self, features: &[String]) -> Result<(), String> {
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
        if !paths.iter().any(|path| path.starts_with("proofs/")) {
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
        "Review landed code, scout and verify proof-list features. Call review_swarm_change, use scout_issues/scout_file for evidence, then review_swarm_complete. No review gates a land.".into()
    }

    fn tools(&self) -> Vec<Value> {
        let mut tools = self.scout.tools();
        tools.push(json!({"name": "review_swarm_change", "description": "The exact landed diff, its issue references and author's named snapshots",
            "inputSchema": {"type": "object", "properties": {}, "additionalProperties": false}}));
        tools.push(json!({"name": "review_swarm_complete", "description": "Record a finished review or sweep. Proof features require Design.lean, fixtures, lake build and axiom audit. Coverage is applied after any named fix or proof lands.",
            "inputSchema": {"type": "object", "properties": {
                "issues_filed": {"type": "boolean"}, "summary": {"type": "string"},
                "proof_assessment": {"type": "string", "description": "Which F17 proof-list features this touches, or why none apply"},
                "proof_features": {"type": "array", "items": {"type": "string"}, "description": "Every affected feature's folder under proofs/"}},
                "required": ["issues_filed", "summary", "proof_assessment", "proof_features"], "additionalProperties": false}}));
        tools
    }

    fn call(&mut self, name: Option<&str>, arguments: Value) -> Result<Value, String> {
        match name {
            Some("review_swarm_change") => self.change(),
            Some("review_swarm_complete") => self.complete(arguments),
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
