//! One review swarm turn, including a durable recovery record, its Rosen workspace,
//! evidence feed, and acknowledgement. Unfinished files are never discarded.

use agent_client_protocol::v1::schema::{EnvVariable, McpServer, McpServerStdio};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};
use velocity_acp::client::{AgentEvent, AgentInput, SessionSetup};
use velocity_core::duty::{DutyEventKind, DutyFeedEvent};
use velocity_core::scout_sweep::SweepRecord;
use velocity_github::{ScoutReport, read_scout_reports};
use velocity_rosen::{ProjectServer, Watches, review_swarm};
use velocity_store::duty::ScoutRoster;

use crate::launch::{agent_launch, with_compiler_cache};
use crate::review_swarm::{ReviewSwarms, WorkerKey, message, now, worker_event};
use crate::review_swarm_server::{Completion, REVIEW_SWARM_SWITCH, SwarmLaunch};
use crate::sweep_runner::SweepHost;

type SessionChannels = (UnboundedSender<AgentInput>, UnboundedReceiver<AgentInput>);

pub(crate) struct Job {
    runner: ReviewSwarms,
    roster: ScoutRoster,
    index: usize,
    target: Option<review_swarm::LandedChange>,
    recovered: Option<SwarmLaunch>,
}

impl Job {
    pub fn new(
        runner: ReviewSwarms,
        roster: ScoutRoster,
        index: usize,
        target: Option<review_swarm::LandedChange>,
        recovered: Option<SwarmLaunch>,
    ) -> Self {
        Self {
            runner,
            roster,
            index,
            target,
            recovered,
        }
    }

    pub async fn run<Host: SweepHost>(
        self,
        host: &Host,
        input: UnboundedSender<AgentInput>,
        inputs: UnboundedReceiver<AgentInput>,
        instruction: Option<String>,
    ) -> Result<(), String> {
        let key = (
            self.roster.project.id,
            self.roster.scouts[self.index].nickname.clone(),
        );
        let server = ProjectServer::new(&self.runner.app_data, key.0);
        let member = review_swarm::membership(&server, &key.1)
            .await
            .map_err(|error| error.to_string())?;
        let piece = SwarmLaunch::piece_for(&key.1, self.target.as_ref());
        if !review_swarm::claim(
            &member,
            &piece,
            "Review landed code and verify its proof list",
        )
        .await
        .map_err(|error| error.to_string())?
        {
            return Err("the review is claimed by another agent".into());
        }
        // A crash during a filing must not leave all future findings behind its claim.
        member
            .release_piece(Some("review-swarm:issue-filing"))
            .await
            .map_err(|error| error.to_string())?;
        let result = async {
            let mut launch = match &self.recovered {
                Some(launch) => launch.clone(),
                None => SwarmLaunch::for_job(
                    &self.runner,
                    &self.roster,
                    self.index,
                    self.target.clone(),
                )?,
            };
            launch.save(&self.runner.job_path(&key))?;
            self.work(
                host,
                &key,
                &server,
                &mut launch,
                (input, inputs),
                instruction,
            )
            .await
        }
        .await;
        member
            .release_piece(Some("review-swarm:issue-filing"))
            .await
            .map_err(|error| error.to_string())?;
        member
            .release_piece(Some(&piece))
            .await
            .map_err(|error| error.to_string())?;
        result
    }

    async fn work<Host: SweepHost>(
        &self,
        host: &Host,
        key: &WorkerKey,
        server: &ProjectServer,
        launch: &mut SwarmLaunch,
        channels: SessionChannels,
        instruction: Option<String>,
    ) -> Result<(), String> {
        // The work may have landed between the last turn's completion and a crash. The
        // acknowledgement is idempotent and requires no second provider session.
        if launch.completion.is_file()
            && !launch.change.is_empty()
            && self.finish(host, key, server, launch).await?
        {
            return Ok(());
        }
        if launch.target.is_none() {
            launch.scout_base = Some(
                velocity_rosen::policy::policy_at(server, None)
                    .await
                    .map_err(|error| error.to_string())?
                    .base,
            );
        }
        let workspace = review_swarm::open_workspace(server, &key.1, &launch.workspace)
            .await
            .map_err(|error| error.to_string())?;
        launch.change = workspace.change;
        launch.save(&self.runner.job_path(key))?;
        let _ = std::fs::remove_file(&launch.completion);
        self.prepare_workspace(launch).await?;
        let watches = Watches::default();
        watches
            .start(server, &key.1, &launch.workspace)
            .await
            .map_err(|error| error.to_string())?;
        let result = self
            .session(host, key, server, launch, channels, instruction)
            .await;
        watches.stop(&launch.workspace).await;
        result?;
        if self.finish(host, key, server, launch).await? {
            return Ok(());
        }
        Err("unfinished proof or fix retained; name the work or resolve its merge conflict".into())
    }

    async fn prepare_workspace(&self, launch: &SwarmLaunch) -> Result<(), String> {
        let marker = launch.workspace.join(".rosen/velocity-setup");
        if marker.is_file() {
            return Ok(());
        }
        let preferences = &self.roster.worktrees;
        let request = velocity_git::WorktreeSetupRequest::detached(
            self.roster.project.repository_path.clone().into(),
            launch.workspace.clone(),
            String::new(),
            crate::launch::setup_settings(preferences),
            preferences.copy_files.clone(),
            preferences.setup_command.clone(),
            launch.workspace.join("target"),
        );
        let app_data = self.runner.app_data.clone();
        let skills = self.roster.project.skills.clone();
        tokio::task::spawn_blocking(move || {
            velocity_skills::workspace::finish_workspace_setup(&app_data, &skills, &request)
                .map_err(|error| error.to_string())?;
            std::fs::write(marker, b"complete\n").map_err(|error| error.to_string())
        })
        .await
        .map_err(|error| error.to_string())?
    }

    async fn session<Host: SweepHost>(
        &self,
        host: &Host,
        key: &WorkerKey,
        server: &ProjectServer,
        launch: &SwarmLaunch,
        (input, inputs): SessionChannels,
        instruction: Option<String>,
    ) -> Result<(), String> {
        let doing = launch.target.as_ref().map_or_else(
            || "scouting the base".into(),
            |target| format!("reviewing {} · {}", target.change, target.summary),
        );
        if !worker_event(host, key, true, doing, DutyEventKind::Claim) {
            return Err("the duty role was removed".into());
        }
        if !host.rosters().iter().any(|roster| {
            roster.project.id == key.0 && roster.scouts.iter().any(|agent| agent.nickname == key.1)
        }) {
            return Err("the duty agent was removed".into());
        }
        self.save_workspace(host, key, launch);
        let (agent_launch, mut setup) = self.setup(key, server, launch)?;
        crate::launch::add_review_handoff(&mut setup, &self.runner.app_data, key.0, &key.1);
        input
            .send(message(&prompt(
                launch,
                &self.roster,
                instruction.as_deref(),
            )))
            .map_err(|error| error.to_string())?;
        self.runner.start_transcript(key, &launch.workspace);
        let session = crate::spawn_session(agent_launch, setup, inputs);
        self.follow(host, key, session).await
    }

    fn setup(
        &self,
        key: &WorkerKey,
        server: &ProjectServer,
        launch: &SwarmLaunch,
    ) -> Result<(velocity_acp::client::AgentLaunch, SessionSetup), String> {
        let agent = &self.roster.scouts[self.index];
        let (agent_launch, modes) = agent_launch(&self.roster.settings, agent.provider)
            .map_err(|error| error.to_string())?;
        let mut agent_launch =
            with_compiler_cache(agent_launch, self.roster.worktrees.share_build_cache);
        agent_launch.env.extend(velocity_rosen::agent_environment(
            server,
            &key.1,
            &launch.change,
        ));
        let settings = &self.roster.settings;
        let mut setup = SessionSetup::new(launch.workspace.clone(), modes).with_run_settings(
            crate::titles::duty_run_settings(settings, agent, settings.agents.review_effort(false)),
        );
        setup.airc = self
            .runner
            .airc
            .as_ref()
            .map(|airc| airc.duty(key.0, &key.1));
        setup.usage_role = Some(velocity_graph::token_usage::Role::Review);
        setup.preface = Some(velocity_rosen::workspace_note(&launch.change));
        setup.shared_servers.push(McpServer::Stdio(
            McpServerStdio::new("velocity-review-swarm", &self.runner.executable)
                .args(vec![
                    REVIEW_SWARM_SWITCH.into(),
                    self.runner.job_path(key).display().to_string(),
                ])
                .env(vec![EnvVariable::new(
                    velocity_acp::client::QUEUED_RUN_PORT_ENV,
                    "",
                )]),
        ));
        setup.shared_servers.extend(rosen_server(
            &self.runner.executable,
            launch,
            &agent_launch.env,
        ));
        Ok((agent_launch, setup))
    }

    async fn follow<Host: SweepHost>(
        &self,
        host: &Host,
        key: &WorkerKey,
        mut session: crate::RunningSession,
    ) -> Result<(), String> {
        let mut doing = String::new();
        while let Some(event) = session.events.recv().await {
            self.runner.record_transcript(key, &event);
            if let Some(step) = crate::sweep::sweep_doing(&event)
                && step != doing
            {
                doing = step.clone();
                let nickname = key.1.clone();
                host.update(
                    key.0,
                    Box::new(move |entry, _| {
                        if let Some(card) = entry
                            .agents
                            .iter_mut()
                            .find(|card| card.nickname == nickname)
                        {
                            card.doing = step;
                        }
                        Vec::new()
                    }),
                );
            }
            if matches!(event, AgentEvent::TurnEnded(_)) {
                self.runner.end_turn(key);
            }
        }
        session.outcome.await.map_err(|error| error.to_string())?
    }

    async fn finish<Host: SweepHost>(
        &self,
        host: &Host,
        key: &WorkerKey,
        server: &ProjectServer,
        launch: &SwarmLaunch,
    ) -> Result<bool, String> {
        let completion = std::fs::read(&launch.completion)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Completion>(&bytes).ok());
        if !review_swarm::finish_workspace(
            server,
            &key.1,
            &launch.change,
            &launch.workspace,
            completion
                .as_ref()
                .map(|completion| completion.head.as_str()),
        )
        .await
        .map_err(|error| error.to_string())?
        {
            return Ok(false);
        }
        let Some(completion) = completion else {
            self.clear_files(key, launch);
            return Err(
                "session ended without review_swarm_complete; coverage remains unreviewed".into(),
            );
        };
        let objected = completion.issues_filed
            || (launch.source_issue.is_some() && completion.source_issue_satisfied != Some(true));
        if let Some(target) = &launch.target {
            review_swarm::mark(server, &key.1, target, objected)
                .await
                .map_err(|error| error.to_string())?;
        }
        self.publish_reports(host, key, launch)?;
        worker_event(
            host,
            key,
            false,
            format!(
                "{} · {}",
                if objected {
                    "findings remain"
                } else {
                    "reviewed"
                },
                completion.summary
            ),
            DutyEventKind::Task,
        );
        self.clear_files(key, launch);
        Ok(true)
    }

    fn save_workspace<Host: SweepHost>(&self, host: &Host, key: &WorkerKey, launch: &SwarmLaunch) {
        if launch.no_change_only {
            return;
        }
        let nickname = key.1.clone();
        let record = SweepRecord::new(
            now(),
            launch
                .target
                .as_ref()
                .map(|target| target.commit.clone())
                .or_else(|| launch.scout_base.clone()),
            launch.workspace.display().to_string(),
        );
        host.update(
            key.0,
            Box::new(move |entry, _| {
                if let Some(card) = entry
                    .agents
                    .iter_mut()
                    .find(|card| card.nickname == nickname)
                {
                    card.sweep = Some(record);
                }
                Vec::new()
            }),
        );
    }

    fn publish_reports<Host: SweepHost>(
        &self,
        host: &Host,
        key: &WorkerKey,
        launch: &SwarmLaunch,
    ) -> Result<(), String> {
        let reports = match read_scout_reports(&launch.report, 0) {
            Ok((reports, _)) => reports,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(error) => return Err(error.to_string()),
        };
        let nickname = key.1.clone();
        host.update(
            key.0,
            Box::new(move |entry, timestamp| {
                reports
                    .into_iter()
                    .filter_map(|report| {
                        let (message, kind) = match report {
                            ScoutReport::Filed {
                                number,
                                title,
                                evidence,
                            } => (
                                velocity_github::filed_message(number, &title, &evidence),
                                DutyEventKind::File,
                            ),
                            ScoutReport::Duplicate { number, title } => (
                                format!("Known issue #{number} {title}"),
                                DutyEventKind::Skip,
                            ),
                        };
                        if entry.feed.iter().any(|event| event.message == message) {
                            return None;
                        }
                        if kind == DutyEventKind::File {
                            entry.issues_filed += 1;
                        }
                        Some(DutyFeedEvent::new(timestamp, &nickname, kind, message))
                    })
                    .collect()
            }),
        );
        Ok(())
    }

    fn clear_files(&self, key: &WorkerKey, launch: &SwarmLaunch) {
        for path in [
            self.runner.job_path(key),
            launch.completion.clone(),
            launch.report.clone(),
        ] {
            let _ = std::fs::remove_file(path);
        }
    }
}

fn rosen_server(
    executable: &std::path::Path,
    launch: &SwarmLaunch,
    environment: &[(String, String)],
) -> Option<McpServer> {
    let command =
        velocity_rosen::mcp_proxy::proxy_mcp_command(executable, &launch.workspace, environment)
            .or_else(|| velocity_rosen::mcp_command(&launch.workspace, environment))?;
    Some(McpServer::Stdio(
        McpServerStdio::new("rosen", command.program)
            .args(command.arguments)
            .env(
                command
                    .environment
                    .into_iter()
                    .map(|(name, value)| EnvVariable::new(name, value))
                    .collect(),
            ),
    ))
}

fn prompt(launch: &SwarmLaunch, roster: &ScoutRoster, instruction: Option<&str>) -> String {
    let dependencies = "When a decision sets an order between issues, or an issue waits on another, use scout_add_blocker to record the dependency on GitHub; never hold an issue with a label for that.";
    let no_change = "Use review_swarm_no_change to list the issues waiting after an empty turn. \
        Read their discussion, following the GitHub link for any clipped evidence, and verify their acceptance criteria against the current base. \
        Resolve an already-fixed issue with verification evidence or requeue it with concrete instructions. \
        Resolve an already-fixed issue with verification evidence, close an orphaned spec issue as not planned with spec_section and removed_commit, mark an upstream wait with blocked_upstream, requeue with concrete instructions (and blocked_by for other issues), or place a visible hold with hold_release_action ONLY for hardware or an explicit scope hold.";
    if launch.no_change_only {
        return format!(
            "You are {}, a Review swarm duty agent. {no_change} {dependencies}\n\n\
             Review the current base without editing it. Report completion through review_swarm_complete \
             with your summary and no proof features, then end the turn.",
            launch.nickname,
        );
    }
    let work = launch.target.as_ref().map_or_else(
        || {
            format!(
                "Scout the base adversarially, covering every top-level folder; focus on {:?}. \
                 If review_swarm_no_change is available, {no_change}",
                roster.rules.focus_areas
            )
        },
        |target| {
            format!(
                "Review landed change {} at commit {}: {}. You hold its channel review claim.",
                target.change,
                target.commit,
                velocity_core::text_budget::preview(&target.summary, 800)
            )
        },
    );
    format!(
        "You are {}, a Review swarm duty agent. {work}\n\n\
        Call review_swarm_change for the file list, diff stats and graph_impact. \
        Use those to choose which files to inspect first. Request the patch by path and page through large files only as needed. \
        Read every referenced issue with its comments and evidence. Use section references from the work or find the relevant headings with graph_find. \
        Read them with graph_read (source: true), or read only their line ranges when the graph is unavailable. \
        Check the implementation end to end against each claim and its SPEC section. \
        Reviews never gate, delay or revert a land. Do not approve, submit receipts or revert by hand; R12 owns build recovery. \
        Search scout_issues before filing, read possible duplicates with scout_issue, and use scout_file with executed failing evidence. \
        {dependencies} \
        Claim issues and every path before editing through Rosen/AIRC. You may write a fix forward in your own workspace.\n\n\
        Identify every affected F17 proof-list feature from this project's SPEC.md. For each, write or update \
        proofs/<Feature>/Design.lean and the exported fixture table, with one implementation fixture check. \
        Prove exactly correct, cost_bound and optimal. Name the spec section, state the cost model and alternatives in the theorem statements, and audit axioms. Use lemmas only for those theorems. File an issue if a design proof exceeds 300 lines. \
        Never weaken reviewed statements, use sorry/admit/axiom/native_decide, or prove a feature outside the proof list. \
        File model/code gaps with evidence. review_swarm_complete queues lake build and the axiom audit for the named features. \
        For other commands that can take more than a minute, use queue_run from velocity-run with class verify. \
        When a tool queues a run, end your turn and continue from Velocity's result. \
        A proof or fix is an ordinary Rosen change, with no review gate.\n\n\
        For a source issue, check its acceptance criteria against the exact landed commit. In review_swarm_complete, report source_issue_satisfied as true when the land meets those criteria and false otherwise. Velocity closes issues on behalf of the review swarm after a positive verdict. A negative verdict keeps the issue open for follow-up. \
        Call review_swarm_complete with your summary, proof assessment and feature folders; set issues_filed when you filed or identified findings through scout_file. \
        Name any proof, stress script or fix with rosen commit -m, then end the turn. \
        Velocity lands that named work through the ordinary policy, then records the reviewed/issues filed state. \
        On a resumed workspace, finish the kept work and resolve any Rosen merge conflicts before completing.\n\n{}",
        launch.nickname,
        velocity_core::text_budget::preview(instruction.unwrap_or(""), 3_000)
    )
}
