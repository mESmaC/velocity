//! PR duty's MCP server for a Rosen change (F5, F7, Rule 4): the only way a reviewing agent
//! approves, requests changes, records its checks and lands the change it holds.
//!
//! A review by reading (F7 "Checks before merge") only decides: its approval needs no
//! checks from the agent, and Velocity's checker submits the receipt and lands the change.
//!
//! The runner fixes the reviewer and the one change in a launch file, so an agent can neither
//! act on another change nor record a decision as anyone else. Rosen itself holds the gate: it
//! lands nothing without an approval at the change's head and a receipt for its merge result.
//! Every outcome is appended to the review's report file, which the runner reads for the Duty
//! view's feed and to hand requested changes to the author's thread.

use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use velocity_git::{WorktreeSetupRequest, default_settings, finish_worktree_setup};
use velocity_github::PrDutyReport;
use velocity_github::mcp_stdio::{ToolServer, append_report};
use velocity_rosen::ProjectServer;
use velocity_rosen::duty::{self, LandOutcome, MergeCheckout};

/// The command-line switch that makes Velocity's own executable serve this MCP server.
pub const ROSEN_DUTY_SERVER_SWITCH: &str = "--rosen-duty-mcp";

/// Everything one Rosen review's server needs, written by the runner before the session.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RosenDutyLaunch {
    pub app_data: PathBuf,
    pub project: u32,
    /// The PR duty agent's nick; its Rosen identity is [`reviewer_name`] of it.
    pub nickname: String,
    pub change: String,
    /// The review folder the merge result is written to.
    pub folder: PathBuf,
    pub report: PathBuf,
    pub setup: FolderSetup,
    /// The review is by reading: the agent builds nothing in the folder, so it gets no
    /// setup, and the checking and landing tools are Velocity's, not the agent's.
    #[serde(default)]
    pub reading: bool,
}

impl RosenDutyLaunch {
    /// The launch of `plan`'s review of `change`, reporting to `report`.
    pub fn for_review(
        app_data: &Path,
        plan: &crate::review::ReviewPlan,
        change: &str,
        report: &Path,
    ) -> Self {
        Self {
            app_data: app_data.to_path_buf(),
            project: plan.project,
            nickname: plan.nickname.clone(),
            change: change.to_owned(),
            folder: plan.worktree.worktree_path.clone(),
            report: report.to_path_buf(),
            setup: FolderSetup::of(&plan.worktree),
            reading: false,
        }
    }

    /// The same launch for a review by reading, or not.
    pub fn by_reading(mut self, reading: bool) -> Self {
        self.reading = reading;
        self
    }
}

/// The per-agent setup a fresh checkout of the merge result gets, as a worktree does (F5):
/// copied untracked files, the shared build cache and the setup command.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FolderSetup {
    pub repository_root: PathBuf,
    pub untracked_files: String,
    pub setup_command: String,
    pub shared_target: PathBuf,
    pub share_build_cache: bool,
    pub copy_untracked_files: bool,
    pub run_setup_command: bool,
}

impl FolderSetup {
    /// The setup of `request`, a planned review worktree.
    pub fn of(request: &WorktreeSetupRequest) -> Self {
        Self {
            repository_root: request.repository_root.clone(),
            untracked_files: request.untracked_files.clone(),
            setup_command: request.setup_command.clone(),
            shared_target: request.shared_cargo_target_dir.clone(),
            share_build_cache: request.settings.share_build_cache,
            copy_untracked_files: request.settings.copy_untracked_files,
            run_setup_command: request.settings.run_setup_command,
        }
    }

    fn request(&self, folder: &Path) -> WorktreeSetupRequest {
        let mut settings = default_settings();
        settings.share_build_cache = self.share_build_cache;
        settings.copy_untracked_files = self.copy_untracked_files;
        settings.run_setup_command = self.run_setup_command;
        WorktreeSetupRequest::detached(
            self.repository_root.clone(),
            folder.to_path_buf(),
            String::new(),
            settings,
            self.untracked_files.clone(),
            self.setup_command.clone(),
            self.shared_target.clone(),
        )
    }
}

/// The Rosen identity a PR duty agent approves and lands as.
pub fn reviewer_name(nickname: &str) -> String {
    format!("pr-duty-{}", nickname.replace(char::is_whitespace, "-"))
}

/// Writes the change's merge result into the review folder and gives it the per-agent setup.
/// A conflicted merge leaves the folder empty, so it gets no setup; nor does a folder that
/// is only read.
pub async fn check_out_review(
    server: &ProjectServer,
    launch: &RosenDutyLaunch,
) -> Result<MergeCheckout, String> {
    let reviewer = reviewer_name(&launch.nickname);
    let checkout = duty::check_out_merge_result(server, &reviewer, &launch.change, &launch.folder)
        .await
        .map_err(|error| error.to_string())?;
    if checkout.conflicts.is_empty() && !launch.reading {
        let request = launch.setup.request(&launch.folder);
        tokio::task::spawn_blocking(move || finish_worktree_setup(&request))
            .await
            .map_err(|error| error.to_string())?
            .map_err(|error| format!("The review folder's setup failed: {error}"))?;
    }
    Ok(checkout)
}

#[derive(Deserialize)]
struct ReviewArguments {
    action: Choice,
    head: String,
    reason: String,
    #[serde(default)]
    comments: Vec<LineComment>,
    #[serde(default)]
    proof_review: Option<ProofReview>,
}

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Choice {
    Approve,
    RequestChanges,
    Comment,
    Hold,
}

#[derive(Deserialize)]
struct LineComment {
    path: String,
    line: u32,
    body: String,
}

/// The semantic proof checks remain the reviewing agent's job (F17).
#[derive(Deserialize)]
struct ProofReview {
    scope: String,
    statements: String,
    confidence: String,
}

#[derive(Deserialize)]
struct Checked {
    head: String,
    base: String,
}

#[derive(Deserialize)]
struct Land {
    head: String,
}

/// One reviewing agent's connection to its change.
pub struct RosenDutyServer {
    launch: RosenDutyLaunch,
    server: ProjectServer,
    reviewer: String,
    runtime: Result<tokio::runtime::Runtime, String>,
    /// The merge result now in the folder: (head, base).
    prepared: Option<(String, String)>,
    /// The merge result whose receipt this session submitted.
    checked: Option<(String, String)>,
}

impl RosenDutyServer {
    pub fn new(launch: RosenDutyLaunch) -> Self {
        let server = ProjectServer::new(&launch.app_data, launch.project);
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|error| format!("Could not start the Rosen client: {error}"));
        Self {
            reviewer: reviewer_name(&launch.nickname),
            launch,
            server,
            runtime,
            prepared: None,
            checked: None,
        }
    }

    /// Reads the launch file the runner wrote.
    pub fn from_launch_file(path: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path)
            .map_err(|error| format!("Could not read {}: {error}", path.display()))?;
        let launch = serde_json::from_str(&text)
            .map_err(|error| format!("{} is not a Rosen review launch: {error}", path.display()))?;
        Ok(Self::new(launch))
    }

    /// Answers JSON-RPC requests, one per line, until the agent closes the connection.
    pub fn serve(&mut self, input: impl BufRead, output: impl Write) -> std::io::Result<()> {
        velocity_github::mcp_stdio::serve(self, input, output)
    }

    fn block_on<T>(&self, work: impl Future<Output = Result<T, String>>) -> Result<T, String> {
        self.runtime.as_ref().map_err(Clone::clone)?.block_on(work)
    }

    fn change(&self) -> Result<Value, String> {
        let (server, change) = (&self.server, self.launch.change.as_str());
        let (overview, diff) = self.block_on(async {
            let overview = duty::change_overview(server, change).await;
            let diff = duty::change_diff(server, change).await;
            Ok((
                overview.map_err(|error| error.to_string())?,
                diff.map_err(|error| error.to_string())?,
            ))
        })?;
        let threads: Vec<Value> = overview
            .threads
            .iter()
            .map(|thread| {
                json!({"path": thread.path, "line": thread.line, "resolved": thread.resolved, "comments": thread.comments})
            })
            .collect();
        Ok(json!({
            "change": change,
            "head": overview.head,
            "author": overview.owner,
            "named_snapshots": overview.named,
            "review_threads": threads,
            "diff": diff,
        }))
    }

    /// The change's head now; a decision on any other head is refused.
    fn current_head(&self) -> Result<String, String> {
        let (server, change) = (&self.server, self.launch.change.as_str());
        let standing = self.block_on(async {
            duty::change_standing(server, change)
                .await
                .map_err(|error| error.to_string())
        })?;
        if !standing.open {
            return Err("The change is no longer open in Rosen".into());
        }
        Ok(standing.head)
    }

    fn review(&mut self, arguments: Value) -> Result<Value, String> {
        let arguments: ReviewArguments =
            serde_json::from_value(arguments).map_err(|error| error.to_string())?;
        if arguments.reason.trim().is_empty() {
            return Err("A reason is required".into());
        }
        if self.current_head()? != arguments.head {
            return Err("The change has a new head: read it again and review that head".into());
        }
        self.post_comments(&arguments.comments)?;
        let change = self.launch.change.clone();
        let head = arguments.head.clone();
        let report = match arguments.action {
            Choice::Approve => {
                self.approve(&arguments)?;
                PrDutyReport::ChangeApproved { change, head }
            }
            Choice::RequestChanges => PrDutyReport::ChangeChangesRequested {
                change,
                head,
                message: changes_message(&self.launch.nickname, &arguments),
            },
            Choice::Comment => PrDutyReport::ChangeCommented { change },
            Choice::Hold => PrDutyReport::ChangeHeld {
                change,
                head,
                reason: arguments.reason.clone(),
            },
        };
        // For requested changes and holds the report is the decision itself: the author
        // hears of it only through the report, so a failed write is the tool's failure.
        append_report(&self.launch.report, &report)
            .map_err(|error| format!("Could not record the decision: {error}"))?;
        Ok(json!({"recorded": true, "head": arguments.head}))
    }

    fn approve(&self, arguments: &ReviewArguments) -> Result<(), String> {
        let proof = arguments
            .proof_review
            .as_ref()
            .ok_or("Approval requires proof_review: scope, statements, confidence")?;
        if [&proof.scope, &proof.statements, &proof.confidence]
            .iter()
            .any(|text| text.trim().is_empty())
        {
            return Err("Explain proof scope, statement review and confidence level (or why each is inapplicable)".into());
        }
        // A review by reading approves what it read: Velocity checks the head afterwards.
        if !self.launch.reading
            && self
                .checked
                .as_ref()
                .is_none_or(|(head, _)| *head != arguments.head)
        {
            return Err("Prepare the merge result, run the checks and record them with rosen_duty_checked before approving".into());
        }
        let (server, reviewer) = (&self.server, self.reviewer.as_str());
        let (change, head) = (self.launch.change.as_str(), arguments.head.as_str());
        self.block_on(async {
            duty::approve(server, reviewer, change, head)
                .await
                .map_err(|error| error.to_string())
        })
    }

    fn post_comments(&self, comments: &[LineComment]) -> Result<(), String> {
        let (server, reviewer) = (&self.server, self.reviewer.as_str());
        let change = self.launch.change.as_str();
        self.block_on(async {
            for comment in comments {
                let target = (comment.path.as_str(), comment.line, comment.body.as_str());
                duty::comment_on_line(server, reviewer, change, target)
                    .await
                    .map_err(|error| error.to_string())?;
            }
            Ok(())
        })
    }

    /// Writes the change's current merge result into the folder.
    fn prepare_landing(&mut self) -> Result<Value, String> {
        // A failed refresh must not leave the previous merge result certifiable.
        self.prepared = None;
        let (server, launch) = (&self.server, &self.launch);
        let checkout = self.block_on(async { check_out_review(server, launch).await })?;
        self.prepared = Some((checkout.head.clone(), checkout.base.clone()));
        Ok(json!({
            "head": checkout.head,
            "base": checkout.base,
            "conflicts": checkout.conflicts,
            "folder": self.launch.folder.display().to_string(),
        }))
    }

    fn checked(&mut self, arguments: Value) -> Result<Value, String> {
        let arguments: Checked =
            serde_json::from_value(arguments).map_err(|error| error.to_string())?;
        let wanted = (arguments.head.clone(), arguments.base.clone());
        if self.prepared.as_ref() != Some(&wanted) {
            return Err(
                "That is not the merge result in the folder: call rosen_duty_prepare_landing, check, then record its head and base".into(),
            );
        }
        self.check_proofs()?;
        let (server, reviewer) = (&self.server, self.reviewer.as_str());
        let change = self.launch.change.as_str();
        let pair = (arguments.head.as_str(), arguments.base.as_str());
        self.block_on(async {
            duty::submit_receipt(server, reviewer, change, pair)
                .await
                .map_err(|error| error.to_string())
        })?;
        self.checked = Some(wanted);
        Ok(json!({"checked": true, "head": arguments.head, "base": arguments.base}))
    }

    /// Never land unless the proofs check (F7, F17; locked): a change that touches the Lake
    /// project gets no receipt, so Rosen won't land it, until `lake build` and the audits
    /// pass on its merge result.
    fn check_proofs(&self) -> Result<(), String> {
        let (server, change) = (&self.server, self.launch.change.as_str());
        let paths = self.block_on(async {
            duty::changed_paths(server, change)
                .await
                .map_err(|error| error.to_string())
        })?;
        let report = velocity_proofs::check_build_tree(&self.launch.folder, &paths)
            .map_err(|error| format!("Proof gate could not run: {error}"))?;
        if !report.passed() {
            return Err(report.summary().unwrap_or_default());
        }
        if let velocity_proofs::CheckStep::Ran(audit) = &report.axioms
            && !audit.unbuilt.is_empty()
        {
            return Err(format!(
                "Proof gate: lake build did not check these changed modules: {}",
                audit.unbuilt.join(", ")
            ));
        }
        Ok(())
    }

    fn land(&self, arguments: Value) -> Result<Value, String> {
        let arguments: Land =
            serde_json::from_value(arguments).map_err(|error| error.to_string())?;
        if self
            .checked
            .as_ref()
            .is_none_or(|(head, _)| *head != arguments.head)
        {
            return Err("Record your checks on this head with rosen_duty_checked first".into());
        }
        let (server, reviewer) = (&self.server, self.reviewer.as_str());
        let change = self.launch.change.clone();
        let outcome = self.block_on(async {
            duty::land(server, reviewer, &change)
                .await
                .map_err(|error| error.to_string())
        })?;
        match outcome {
            LandOutcome::Landed { commit } => {
                self.record(&PrDutyReport::ChangeLanded {
                    change,
                    commit: commit.clone(),
                });
                Ok(json!({"landed": true, "commit": commit}))
            }
            LandOutcome::Refused { code, message } => self.refused(change, &code, &message),
        }
    }

    /// A refusal a new check can fix is the agent's to retry; any other ends the review.
    fn refused(&self, change: String, code: &str, message: &str) -> Result<Value, String> {
        if matches!(code, "receipt_stale" | "needs_recheck" | "no_receipt") {
            return Err(format!(
                "Rosen refused ({code}): {message}. Call rosen_duty_prepare_landing, run the checks on the new merge result, record them with rosen_duty_checked, then land again."
            ));
        }
        let reason = format!("{code}: {message}");
        self.record(&PrDutyReport::ChangeLandFailed {
            change,
            reason: reason.clone(),
        });
        Err(format!("Rosen refused to land the change ({reason})"))
    }

    /// The feed is a convenience; a report that can't be written never undoes a decision.
    fn record(&self, report: &PrDutyReport) {
        let _ = append_report(&self.launch.report, report);
    }
}

/// What the author's agent is told when changes are requested.
fn changes_message(nickname: &str, arguments: &ReviewArguments) -> String {
    let mut message = format!(
        "PR duty ({nickname}) requested changes on your Rosen change: {}\n",
        arguments.reason
    );
    for comment in &arguments.comments {
        message.push_str(&format!(
            "- {}:{}: {}\n",
            comment.path, comment.line, comment.body
        ));
    }
    message.push_str("Fix them in your workspace, then name the work again with `rosen commit -m \"<what changed>\"` so PR duty reviews it again.\n");
    message
}

impl ToolServer for RosenDutyServer {
    fn name(&self) -> &'static str {
        "velocity-rosen-duty"
    }

    fn instructions(&self) -> String {
        let landing = if self.launch.reading {
            "Velocity checks and lands the head you approve"
        } else {
            "land an approved head with rosen_duty_land"
        };
        format!(
            "You are {}, holding the review claim on Rosen change {}. Read it with rosen_duty_change, record your decision with rosen_duty_review, and {landing}.",
            self.launch.nickname, self.launch.change
        )
    }

    fn tools(&self) -> Vec<Value> {
        crate::rosen_duty_tools::tool_list(&self.launch.change, self.launch.reading)
    }

    fn call(&mut self, name: Option<&str>, arguments: Value) -> Result<Value, String> {
        match name {
            Some("rosen_duty_change") => self.change(),
            Some("rosen_duty_review") => self.review(arguments),
            Some("rosen_duty_prepare_landing" | "rosen_duty_checked" | "rosen_duty_land")
                if self.launch.reading =>
            {
                Err(format!(
                    "This review is by reading. Velocity runs the pre-land checks on change {} itself, submits the receipt and lands it once you approve. Record your decision with rosen_duty_review, then end your turn.",
                    self.launch.change
                ))
            }
            Some("rosen_duty_prepare_landing") => self.prepare_landing(),
            Some("rosen_duty_checked") => self.checked(arguments),
            Some("rosen_duty_land") => self.land(arguments),
            _ => Err("Unknown Rosen duty tool".into()),
        }
    }
}
