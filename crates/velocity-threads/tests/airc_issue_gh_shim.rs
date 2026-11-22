//! Risk (#581): AIRC issue tools run inside a child process of the coding agent, inheriting
//! the agent's Rosen environment (shim folder first on `PATH`, `VELOCITY_BLOCK_GIT_GH=1`,
//! `ROSEN_CHANGE`, `ROSEN_ENDPOINT`, and a working directory inside a `.rosen` workspace).
//! Velocity's own `gh` calls must reach the real `gh` with shim/Rosen variables stripped,
//! while an agent running `gh issue list` in its shell must still hit the shim and be refused,
//! and local non-GitHub AIRC issue tool failures must be recorded for project health.

use std::ffi::{OsStr, OsString};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use agent_client_protocol::v1::schema::McpServer;
use velocity_acp::client::AircLaunch;
use velocity_airc::{IssueCreate, IssueRead, IssueRequest};
use velocity_github::GH_PROGRAM_ENV;
use velocity_rosen::agent_run::{BLOCK_VCS_ENV, vcs_refusal};
use velocity_store::{PersistedState, ProjectItem, StateStore};
use velocity_threads::issue_activity::{handle_issue_request, read_issue_tool_failure};

const CHILD_APP_DATA_ENV: &str = "VELOCITY_TEST_ISSUE_581_APP_DATA";
const CHILD_EXPECT_FAILURE_ENV: &str = "VELOCITY_TEST_ISSUE_581_EXPECT_FAIL";
const CHILD_CREATE_LOG_ENV: &str = "VELOCITY_TEST_ISSUE_608_CREATE_LOG";
const CHILD_BLOCKED_BY_LOG_ENV: &str = "VELOCITY_TEST_ISSUE_608_BLOCKED_BY_LOG";
const CHILD_TEST_NAME: &str = "child_executes_airc_issue_read";
const CHILD_CROSS_REPO_TEST_NAME: &str = "child_executes_cross_repo_issue_read";
const CHILD_CROSS_REPO_CREATE_TEST_NAME: &str = "child_executes_cross_repo_issue_create";
const CHILD_REFUSAL_TEST_NAME: &str = "child_checks_agent_gh_refusal";

struct TempTree(PathBuf);

impl TempTree {
    fn new(prefix: &str) -> std::io::Result<Self> {
        let path = std::env::temp_dir().join(format!("{prefix}-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&path)?;
        Ok(Self(path))
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempTree {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[allow(clippy::disallowed_methods)]
fn hidden_command(program: impl AsRef<OsStr>) -> Command {
    // Only Windows sets creation flags on it.
    #[cfg_attr(not(windows), allow(unused_mut))]
    let mut command = Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    command
}

#[test]
fn airc_issue_read_reaches_real_gh_inside_rosen_agent_environment()
-> Result<(), Box<dyn std::error::Error>> {
    let tree = TempTree::new("velocity-issue-581")?;
    let fixture = setup_fixture(tree.path())?;
    verify_airc_launch_env(&fixture)?;
    verify_issue_read_bypasses_shim(&fixture)?;
    verify_cross_repo_issue_read(&fixture)?;
    verify_cross_repo_issue_create(&fixture)?;
    verify_agent_shell_gh_still_refused(&fixture)?;
    verify_project_health_failure_recording(&fixture)?;
    Ok(())
}

fn verify_cross_repo_issue_read(fixture: &Fixture) -> Result<(), Box<dyn std::error::Error>> {
    // Risk (#598): AIRC issue_read must read configured sibling Velocity projects and
    // repositories referenced in blocked_by dependencies while refusing unrelated repos
    // without creating a local tool health failure.
    let cross_repo = run_child(
        fixture,
        CHILD_CROSS_REPO_TEST_NAME,
        Some(&fixture.real_gh),
        false,
    )?;
    assert_child_ok("cross-repo issue_read", &cross_repo)?;
    assert!(
        read_issue_tool_failure(&fixture.app_data, 1).is_none(),
        "refusing an unallowed cross-repo read must not record a local tool health failure"
    );
    Ok(())
}

fn verify_cross_repo_issue_create(fixture: &Fixture) -> Result<(), Box<dyn std::error::Error>> {
    // Risk (#608): AIRC issue_create must allow registered sibling Velocity projects, refuse
    // unregistered repos and non-existent sibling labels without auto-creating them, check
    // duplicates on the sibling repo first, add the agent/originating issue+thread byline,
    // and link the sibling issue as blocked_by on the caller's held issue.
    let cross_repo = run_child(
        fixture,
        CHILD_CROSS_REPO_CREATE_TEST_NAME,
        Some(&fixture.real_gh),
        false,
    )?;
    assert_child_ok("cross-repo issue_create", &cross_repo)?;
    assert!(
        read_issue_tool_failure(&fixture.app_data, 1).is_none(),
        "refusing an unallowed cross-repo create must not record a local tool health failure"
    );
    Ok(())
}

fn verify_airc_launch_env(fixture: &Fixture) -> Result<(), Box<dyn std::error::Error>> {
    let server = AircLaunch::new(std::env::current_exe()?, fixture.app_data.clone())
        .with_gh_program(Some(fixture.real_gh.clone()))
        .thread(1, 7, &fixture.workspace);
    let McpServer::Stdio(stdio) = server else {
        return Err("expected stdio AIRC MCP server".into());
    };
    assert!(stdio.env.iter().any(|var| {
        var.name == GH_PROGRAM_ENV && var.value == fixture.real_gh.to_string_lossy()
    }));
    Ok(())
}

fn verify_issue_read_bypasses_shim(fixture: &Fixture) -> Result<(), Box<dyn std::error::Error>> {
    // 1. AIRC issue_read with VELOCITY_GH set and exact Rosen agent environment reaches real gh.
    let with_override = run_child(fixture, CHILD_TEST_NAME, Some(&fixture.real_gh), false)?;
    assert_child_ok("issue_read with VELOCITY_GH", &with_override)?;
    assert!(
        !fixture.shim_log.exists(),
        "agent gh shim must not be called"
    );
    verify_real_gh_log(&fixture.real_log, &fixture.shim_dir)?;

    // 2. AIRC issue_read without VELOCITY_GH still skips the shim folder on PATH.
    let _ = fs::remove_file(&fixture.real_log);
    let without_override = run_child(fixture, CHILD_TEST_NAME, None, false)?;
    assert_child_ok("issue_read without VELOCITY_GH", &without_override)?;
    assert!(
        !fixture.shim_log.exists(),
        "agent gh shim must not be called"
    );
    verify_real_gh_log(&fixture.real_log, &fixture.shim_dir)
}

fn verify_agent_shell_gh_still_refused(
    fixture: &Fixture,
) -> Result<(), Box<dyn std::error::Error>> {
    let shell_output = run_agent_shell_gh(fixture)?;
    assert!(
        !shell_output.status.success(),
        "direct agent gh call must be refused"
    );
    let shell_stderr = String::from_utf8_lossy(&shell_output.stderr);
    assert!(
        shell_stderr.contains("Agents do not call gh directly"),
        "expected gh shim refusal, got: {shell_stderr}"
    );
    assert!(
        fixture.shim_log.exists(),
        "direct agent gh must hit the shim"
    );
    let refusal_check = run_child(fixture, CHILD_REFUSAL_TEST_NAME, None, false)?;
    assert_child_ok("vcs_refusal inside Rosen workspace", &refusal_check)
}

fn verify_project_health_failure_recording(
    fixture: &Fixture,
) -> Result<(), Box<dyn std::error::Error>> {
    let github_404 = run_child(fixture, CHILD_TEST_NAME, Some(&fixture.github_404_gh), true)?;
    assert_child_ok("issue_read GitHub 404", &github_404)?;
    assert!(
        read_issue_tool_failure(&fixture.app_data, 1).is_none(),
        "GitHub HTTP 404 must not create a local tool health row"
    );

    let local_fail = run_child(fixture, CHILD_TEST_NAME, Some(&fixture.local_fail_gh), true)?;
    assert_child_ok("issue_read local failure", &local_fail)?;
    let recorded = read_issue_tool_failure(&fixture.app_data, 1)
        .ok_or("expected project health failure after local gh refusal")?;
    assert_eq!(recorded.tool, "issue_read");
    assert_eq!(recorded.count, 1);
    assert!(recorded.message.contains("Agents do not call gh directly"));

    let recovered = run_child(fixture, CHILD_TEST_NAME, Some(&fixture.real_gh), false)?;
    assert_child_ok("issue_read recovery", &recovered)?;
    assert!(
        read_issue_tool_failure(&fixture.app_data, 1).is_none(),
        "successful issue_read must clear the recorded project health failure"
    );
    Ok(())
}

#[test]
#[ignore = "invoked as a child process by airc_issue_read_reaches_real_gh_inside_rosen_agent_environment"]
fn child_executes_airc_issue_read() -> Result<(), Box<dyn std::error::Error>> {
    let app_data =
        PathBuf::from(std::env::var_os(CHILD_APP_DATA_ENV).ok_or("missing child app_data env")?);
    let expect_fail = std::env::var_os(CHILD_EXPECT_FAILURE_ENV).is_some();
    let result = handle_issue_request(
        &app_data,
        "1",
        IssueRequest::Read(IssueRead {
            issue: 42,
            repository: None,
            offset: 0,
            comments: 5,
            member: None,
            held_issues: Vec::new(),
        }),
    );
    if expect_fail {
        assert!(
            result.is_err(),
            "expected issue_read to fail, got {result:?}"
        );
        return Ok(());
    }
    let answer = result?;
    assert_eq!(answer["number"], 42);
    assert_eq!(answer["title"], "Stub issue");
    assert_eq!(answer["body"], "Reached real gh");
    Ok(())
}

#[test]
#[ignore = "invoked as a child process by airc_issue_read_reaches_real_gh_inside_rosen_agent_environment"]
fn child_executes_cross_repo_issue_read() -> Result<(), Box<dyn std::error::Error>> {
    let app_data =
        PathBuf::from(std::env::var_os(CHILD_APP_DATA_ENV).ok_or("missing child app_data env")?);

    // 1. Configured sibling Velocity project (matched case-insensitively).
    let sibling = handle_issue_request(
        &app_data,
        "1",
        IssueRequest::Read(IssueRead {
            issue: 42,
            repository: Some("OWNER/SIBLING".into()),
            offset: 0,
            comments: 5,
            member: None,
            held_issues: Vec::new(),
        }),
    )?;
    assert_eq!(sibling["repository"], "owner/sibling");
    assert_eq!(sibling["number"], 42);

    // 2. Repository referenced in a held issue's blocked_by dependency (even when closed).
    let held_blocker = handle_issue_request(
        &app_data,
        "1",
        IssueRequest::Read(IssueRead {
            issue: 42,
            repository: Some("owner/held-blocker".into()),
            offset: 0,
            comments: 5,
            member: Some("issue-589-2-328".into()),
            held_issues: vec![589],
        }),
    )?;
    assert_eq!(held_blocker["repository"], "owner/held-blocker");

    // 3. Repository referenced in another open project issue's blocked_by dependency.
    let open_blocker = handle_issue_request(
        &app_data,
        "1",
        IssueRequest::Read(IssueRead {
            issue: 42,
            repository: Some("owner/open-blocker".into()),
            offset: 0,
            comments: 5,
            member: None,
            held_issues: Vec::new(),
        }),
    )?;
    assert_eq!(open_blocker["repository"], "owner/open-blocker");

    // 4. Unconfigured, unreferenced repository must be refused cleanly.
    let refused = handle_issue_request(
        &app_data,
        "1",
        IssueRequest::Read(IssueRead {
            issue: 42,
            repository: Some("owner/unrelated".into()),
            offset: 0,
            comments: 5,
            member: None,
            held_issues: Vec::new(),
        }),
    );
    let Err(error) = refused else {
        return Err("expected unrelated repository to be refused".into());
    };
    let message = error.to_string();
    assert!(
        message.contains("not allowed for issue_read"),
        "unexpected refusal message: {message}"
    );
    Ok(())
}

#[test]
#[ignore = "invoked as a child process by airc_issue_read_reaches_real_gh_inside_rosen_agent_environment"]
fn child_executes_cross_repo_issue_create() -> Result<(), Box<dyn std::error::Error>> {
    let app_data =
        PathBuf::from(std::env::var_os(CHILD_APP_DATA_ENV).ok_or("missing child app_data env")?);
    let create_log = PathBuf::from(
        std::env::var_os(CHILD_CREATE_LOG_ENV).ok_or("missing child create_log env")?,
    );
    let blocked_by_log = PathBuf::from(
        std::env::var_os(CHILD_BLOCKED_BY_LOG_ENV).ok_or("missing child blocked_by_log env")?,
    );

    // 1. Unregistered repository is refused cleanly.
    let refused = handle_issue_request(
        &app_data,
        "1",
        IssueRequest::Create(IssueCreate {
            member: "issue-607-345",
            nick: "issue-607-345",
            repository: Some("owner/unrelated".into()),
            held_issues: vec![607],
            title: "Need sibling change",
            body: "Detailed evidence for sibling change.",
            labels: &[],
            blocked_by: &[],
        }),
    );
    let Err(error) = refused else {
        return Err("expected unregistered repository to be refused".into());
    };
    assert!(
        error.to_string().contains("not allowed for issue_create"),
        "unexpected refusal: {error}"
    );

    // 2. Non-existent label on sibling repo (even an auto-label like spec-question) is refused.
    let missing_label = [String::from("spec-question")];
    let refused_label = handle_issue_request(
        &app_data,
        "1",
        IssueRequest::Create(IssueCreate {
            member: "issue-607-345",
            nick: "issue-607-345",
            repository: Some("OWNER/SIBLING".into()),
            held_issues: vec![607],
            title: "Mint releases token from desktop session",
            body: "Desktop credentials cannot mint a releases token yet.",
            labels: &missing_label,
            blocked_by: &[],
        }),
    );
    let Err(label_error) = refused_label else {
        return Err("expected missing sibling label to be refused".into());
    };
    assert!(
        label_error.to_string().contains("Unknown label"),
        "unexpected label refusal: {label_error}"
    );

    // 3. Duplicate search runs against the sibling repo first and links the existing issue.
    let duplicate = handle_issue_request(
        &app_data,
        "1",
        IssueRequest::Create(IssueCreate {
            member: "issue-607-345",
            nick: "issue-607-345",
            repository: Some("owner/sibling".into()),
            held_issues: vec![607],
            title: "Fix mirror webhook signature header",
            body: "Webhook HMAC validation rejects empty payloads with HTTP 400.",
            labels: &[],
            blocked_by: &[],
        }),
    )?;
    assert_eq!(duplicate["outcome"], "duplicate");
    assert_eq!(duplicate["repository"], "owner/sibling");
    assert_eq!(duplicate["issue"]["number"], 215);
    assert_eq!(duplicate["blocked_origin_issues"], serde_json::json!([607]));
    assert_eq!(duplicate["origin_blocker"], "owner/sibling#215");

    // 4. Distinct report files a new sibling issue with byline + originating issue/thread and links blocked_by.
    let existing_label = [String::from("enhancement")];
    let filed = handle_issue_request(
        &app_data,
        "1",
        IssueRequest::Create(IssueCreate {
            member: "issue-607-345",
            nick: "issue-607-345",
            repository: Some("OWNER/SIBLING".into()),
            held_issues: vec![607],
            title: "Allow desktop session to mint releases-scoped token",
            body: "Verified server/internal/tokens/native.go refuses POST /api/tokens/personal.",
            labels: &existing_label,
            blocked_by: &[],
        }),
    )?;
    assert_eq!(filed["outcome"], "filed");
    assert_eq!(filed["repository"], "owner/sibling");
    assert_eq!(filed["cross_repository"], true);
    assert_eq!(filed["number"], 216);
    assert_eq!(filed["blocked_origin_issues"], serde_json::json!([607]));
    assert_eq!(filed["origin_blocker"], "owner/sibling#216");

    let created_payload = fs::read_to_string(&create_log)?;
    assert!(
        created_payload.contains("Reported by an agent (`issue-607-345`)"),
        "expected agent byline in sibling body, got:\n{created_payload}"
    );
    assert!(
        created_payload.contains("Originating issue: owner/repo#607"),
        "expected originating issue in sibling body, got:\n{created_payload}"
    );
    assert!(
        created_payload.contains("velocity://thread/1/345"),
        "expected originating thread link in sibling body, got:\n{created_payload}"
    );
    assert!(
        created_payload.contains("L_ENH"),
        "expected existing sibling label ID in createIssue input, got:\n{created_payload}"
    );

    let blocked_payload = fs::read_to_string(&blocked_by_log)?;
    assert!(
        blocked_payload.contains("I_607") && blocked_payload.contains("I_BLOCKER"),
        "expected addBlockedBy mutation on originating issue, got:\n{blocked_payload}"
    );
    Ok(())
}

#[test]
#[ignore = "invoked as a child process by airc_issue_read_reaches_real_gh_inside_rosen_agent_environment"]
fn child_checks_agent_gh_refusal() -> Result<(), Box<dyn std::error::Error>> {
    let refusal = vcs_refusal("gh").ok_or("expected gh refusal inside Rosen workspace")?;
    assert!(refusal.contains("AIRC"), "unexpected refusal: {refusal}");
    Ok(())
}

struct Fixture {
    app_data: PathBuf,
    workspace: PathBuf,
    shim_dir: PathBuf,
    real_gh: PathBuf,
    local_fail_gh: PathBuf,
    github_404_gh: PathBuf,
    shim_log: PathBuf,
    real_log: PathBuf,
    create_log: PathBuf,
    blocked_by_log: PathBuf,
    agent_path: OsString,
}

fn setup_fixture(root: &Path) -> Result<Fixture, Box<dyn std::error::Error>> {
    let app_data = root.join("app-data");
    let repo = root.join("repo");
    let sibling_repo = root.join("sibling-repo");
    let workspace = root.join("workspace");
    let shim_dir = root.join("velocity-agent-shim");
    let real_dir = root.join("real-gh");
    let local_fail_dir = root.join("local-fail-gh");
    let github_404_dir = root.join("github-404-gh");
    for dir in [
        &app_data,
        &repo,
        &sibling_repo,
        &workspace.join(".rosen"),
        &shim_dir,
        &real_dir,
        &local_fail_dir,
        &github_404_dir,
    ] {
        fs::create_dir_all(dir)?;
    }

    let mut legacy = PersistedState::default();
    let mut project = ProjectItem::new(1, "velocity".into(), repo.display().to_string());
    project.github_remote = Some("owner/repo".into());
    legacy.projects.push(project);
    let mut sibling_project =
        ProjectItem::new(2, "sibling".into(), sibling_repo.display().to_string());
    sibling_project.github_remote = Some("owner/sibling".into());
    legacy.projects.push(sibling_project);
    fs::write(
        app_data.join(velocity_store::STATE_FILE_NAME),
        serde_json::to_vec(&legacy)?,
    )?;
    let _store = StateStore::load_or_default(&app_data)?;

    fs::write(shim_dir.join("velocity-program"), b"velocity")?;
    let shim_log = root.join("shim-called.log");
    let real_log = root.join("real-called.log");
    let create_log = root.join("create-issue.log");
    let blocked_by_log = root.join("add-blocked-by.log");
    let graphql_file = root.join("issue-42.json");
    let held_blockers_file = root.join("held-blockers.json");
    let open_blockers_file = root.join("open-blockers.json");
    let labels_file = root.join("sibling-labels.json");
    let sibling_open_file = root.join("sibling-open-issues.json");
    let repo_id_file = root.join("repo-id.json");
    let issue_id_file = root.join("issue-id.json");
    let created_issue_file = root.join("created-issue.json");
    let add_blocked_by_file = root.join("add-blocked-by.json");
    fs::write(
        &graphql_file,
        r#"{"data":{"repository":{"issue":{"id":"I_42","number":42,"title":"Stub issue","body":"Reached real gh","url":"https://github.com/owner/repo/issues/42","state":"OPEN","author":{"login":"octocat"},"createdAt":"2026-01-01T00:00:00Z","updatedAt":"2026-01-01T00:00:00Z","closedAt":null,"milestone":null,"labels":{"nodes":[],"totalCount":0},"blockedBy":{"nodes":[],"pageInfo":{"hasNextPage":false,"endCursor":null}},"comments":{"totalCount":0,"nodes":[]}}}}}"#,
    )?;
    fs::write(
        &held_blockers_file,
        r#"{"data":{"repository":{"issue":{"id":"I_607","blockedBy":{"nodes":[{"number":191,"repository":{"nameWithOwner":"owner/held-blocker"}}],"pageInfo":{"hasNextPage":false,"endCursor":null}}}}}}"#,
    )?;
    fs::write(
        &open_blockers_file,
        r#"{"data":{"repository":{"issues":{"nodes":[{"blockedBy":{"nodes":[{"repository":{"nameWithOwner":"owner/open-blocker"}}]}}],"pageInfo":{"hasNextPage":false,"endCursor":null}}}}}"#,
    )?;
    fs::write(
        &labels_file,
        r#"{"data":{"repository":{"labels":{"nodes":[{"name":"enhancement","id":"L_ENH"}],"pageInfo":{"hasNextPage":false,"endCursor":null}}}}}"#,
    )?;
    fs::write(
        &sibling_open_file,
        r#"{"data":{"repository":{"defaultBranchRef":null,"issues":{"nodes":[{"id":"I_215","number":215,"title":"Fix mirror webhook signature header","body":"Webhook HMAC validation rejects empty payloads with HTTP 400.","url":"https://github.com/owner/sibling/issues/215","state":"OPEN","author":{"login":"octocat"},"createdAt":"2026-01-01T00:00:00Z","updatedAt":"2026-01-01T00:00:00Z","closedAt":null,"milestone":null,"labels":{"nodes":[{"name":"enhancement"}],"totalCount":1},"issueDependenciesSummary":{"blockedBy":0},"blockedBy":{"nodes":[],"pageInfo":{"hasNextPage":false,"endCursor":null}}}],"pageInfo":{"hasNextPage":false,"endCursor":null}}}}}"#,
    )?;
    fs::write(
        &repo_id_file,
        r#"{"data":{"repository":{"id":"R_SIBLING"}}}"#,
    )?;
    fs::write(
        &issue_id_file,
        r#"{"data":{"repository":{"issue":{"id":"I_BLOCKER"}}}}"#,
    )?;
    fs::write(
        &created_issue_file,
        r#"{"data":{"createIssue":{"issue":{"id":"I_216","number":216,"title":"Allow desktop session to mint releases-scoped token","body":"Created","url":"https://github.com/owner/sibling/issues/216","state":"OPEN","author":{"login":"octocat"},"createdAt":"2026-01-01T00:00:00Z","updatedAt":"2026-01-01T00:00:00Z","closedAt":null,"milestone":null,"labels":{"nodes":[{"name":"enhancement"}],"totalCount":1}}}}}"#,
    )?;
    fs::write(
        &add_blocked_by_file,
        r#"{"data":{"addBlockedBy":{"issue":{"id":"I_607"}}}}"#,
    )?;

    write_refusal_stub(
        &shim_dir,
        Some(&shim_log),
        "gh: Agents do not call gh directly. Use AIRC issue tools (issue_read, issue_create, issue_comment, issue_ready_to_close) or velocity-run's workflow_run instead.",
    )?;
    let real_gh = write_real_stub(
        &real_dir,
        &RealStubFiles {
            log_file: &real_log,
            create_log: &create_log,
            blocked_by_log: &blocked_by_log,
            graphql_file: &graphql_file,
            held_blockers_file: &held_blockers_file,
            open_blockers_file: &open_blockers_file,
            labels_file: &labels_file,
            sibling_open_file: &sibling_open_file,
            repo_id_file: &repo_id_file,
            issue_id_file: &issue_id_file,
            created_issue_file: &created_issue_file,
            add_blocked_by_file: &add_blocked_by_file,
        },
    )?;
    let local_fail_gh = write_refusal_stub(
        &local_fail_dir,
        None,
        "gh: Agents do not call gh directly. Use AIRC issue tools instead.",
    )?;
    let github_404_gh = write_refusal_stub(&github_404_dir, None, "gh: Not Found (HTTP 404)")?;

    let mut path_entries = vec![shim_dir.clone(), real_dir];
    if let Some(existing) = std::env::var_os("PATH") {
        path_entries.extend(std::env::split_paths(&existing).filter(|folder| {
            !folder.join("velocity-program").is_file() && !folder.join("rosen-program").is_file()
        }));
    }
    let agent_path = std::env::join_paths(path_entries)?;

    Ok(Fixture {
        app_data,
        workspace,
        shim_dir,
        real_gh,
        local_fail_gh,
        github_404_gh,
        shim_log,
        real_log,
        create_log,
        blocked_by_log,
        agent_path,
    })
}

struct RealStubFiles<'a> {
    log_file: &'a Path,
    create_log: &'a Path,
    blocked_by_log: &'a Path,
    graphql_file: &'a Path,
    held_blockers_file: &'a Path,
    open_blockers_file: &'a Path,
    labels_file: &'a Path,
    sibling_open_file: &'a Path,
    repo_id_file: &'a Path,
    issue_id_file: &'a Path,
    created_issue_file: &'a Path,
    add_blocked_by_file: &'a Path,
}

fn run_child(
    fixture: &Fixture,
    test_name: &str,
    velocity_gh: Option<&Path>,
    expect_fail: bool,
) -> Result<Output, Box<dyn std::error::Error>> {
    let mut command = hidden_command(std::env::current_exe()?);
    command
        .args(["--exact", test_name, "--ignored", "--nocapture"])
        .current_dir(&fixture.workspace)
        .env("PATH", &fixture.agent_path)
        .env(BLOCK_VCS_ENV, "1")
        .env("ROSEN_CHANGE", "chg-issue-581-test")
        .env("ROSEN_ENDPOINT", "http://127.0.0.1:1")
        .env(CHILD_APP_DATA_ENV, &fixture.app_data)
        .env(CHILD_CREATE_LOG_ENV, &fixture.create_log)
        .env(CHILD_BLOCKED_BY_LOG_ENV, &fixture.blocked_by_log);
    match velocity_gh {
        Some(path) => {
            command.env(GH_PROGRAM_ENV, path);
        }
        None => {
            command.env_remove(GH_PROGRAM_ENV);
        }
    }
    if expect_fail {
        command.env(CHILD_EXPECT_FAILURE_ENV, "1");
    } else {
        command.env_remove(CHILD_EXPECT_FAILURE_ENV);
    }
    Ok(command.output()?)
}

fn run_agent_shell_gh(fixture: &Fixture) -> Result<Output, Box<dyn std::error::Error>> {
    #[cfg(windows)]
    let mut command = {
        let mut cmd = hidden_command("cmd");
        cmd.args(["/C", "gh", "issue", "list"]);
        cmd
    };
    #[cfg(not(windows))]
    let mut command = {
        let mut cmd = hidden_command("sh");
        cmd.args(["-c", "gh issue list"]);
        cmd
    };
    command
        .current_dir(&fixture.workspace)
        .env("PATH", &fixture.agent_path)
        .env(BLOCK_VCS_ENV, "1")
        .env("ROSEN_CHANGE", "chg-issue-581-test")
        .env("ROSEN_ENDPOINT", "http://127.0.0.1:1")
        .env_remove(GH_PROGRAM_ENV);
    Ok(command.output()?)
}

fn verify_real_gh_log(log_path: &Path, shim_dir: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let log = fs::read_to_string(log_path)?;
    assert!(
        log.contains("api"),
        "expected real gh stub to handle GraphQL api call, log:\n{log}"
    );
    assert!(
        log.contains("ENV=[,,]"),
        "expected VELOCITY_BLOCK_GIT_GH, ROSEN_CHANGE and ROSEN_ENDPOINT removed, log:\n{log}"
    );
    let shim_text = shim_dir.to_string_lossy();
    assert!(
        !log.contains(shim_text.as_ref()),
        "expected shim folder {shim_text} removed from gh PATH, log:\n{log}"
    );
    Ok(())
}

fn assert_child_ok(label: &str, output: &Output) -> Result<(), Box<dyn std::error::Error>> {
    if output.status.success() {
        return Ok(());
    }
    Err(format!(
        "{label} failed ({:?}):\nstdout:\n{}\nstderr:\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    )
    .into())
}

#[cfg(windows)]
fn write_real_stub(directory: &Path, files: &RealStubFiles<'_>) -> std::io::Result<PathBuf> {
    let script = directory.join("gh.cmd");
    let stdin_file = directory.join("stdin.json");
    let body = format!(
        "@echo off\r\n\
         echo CMD=%1>> \"{log}\"\r\n\
         echo ENV=[%VELOCITY_BLOCK_GIT_GH%,%ROSEN_CHANGE%,%ROSEN_ENDPOINT%]>> \"{log}\"\r\n\
         echo PATH=%PATH%>> \"{log}\"\r\n\
         if \"%1\"==\"auth\" exit /b 1\r\n\
         if \"%1\"==\"repo\" goto do_repo\r\n\
         if \"%1\"==\"api\" goto do_api\r\n\
         exit /b 1\r\n\
         :do_repo\r\n\
         echo %* | findstr /I /C:\"owner/sibling\" > nul\r\n\
         if not errorlevel 1 goto do_repo_sibling\r\n\
         echo {{\"nameWithOwner\":\"owner/repo\",\"defaultBranchRef\":{{\"name\":\"main\"}}}}\r\n\
         exit /b 0\r\n\
         :do_repo_sibling\r\n\
         echo {{\"nameWithOwner\":\"owner/sibling\",\"defaultBranchRef\":{{\"name\":\"main\"}}}}\r\n\
         exit /b 0\r\n\
         :do_api\r\n\
         findstr \"^\" > \"{stdin}\"\r\n\
         findstr /C:\"createIssue\" \"{stdin}\" > nul\r\n\
         if not errorlevel 1 goto do_create_issue\r\n\
         findstr /C:\"addBlockedBy\" \"{stdin}\" > nul\r\n\
         if not errorlevel 1 goto do_add_blocked_by\r\n\
         findstr /C:\"labels(first:100,after:\" \"{stdin}\" > nul\r\n\
         if not errorlevel 1 goto do_labels\r\n\
         findstr /C:\"issueDependenciesSummary\" \"{stdin}\" > nul\r\n\
         if not errorlevel 1 goto do_sibling_open\r\n\
         findstr /C:\"states:OPEN\" \"{stdin}\" > nul\r\n\
         if not errorlevel 1 goto do_open_blockers\r\n\
         findstr /C:\"id blockedBy\" \"{stdin}\" > nul\r\n\
         if not errorlevel 1 goto do_held_blockers\r\n\
         findstr /C:\"issue(number:$number) {{ id }}\" \"{stdin}\" > nul\r\n\
         if not errorlevel 1 goto do_issue_id\r\n\
         findstr /C:\"repository(owner:$owner,name:$name) {{ id }}\" \"{stdin}\" > nul\r\n\
         if not errorlevel 1 goto do_repo_id\r\n\
         type \"{graphql}\"\r\n\
         exit /b 0\r\n\
         :do_create_issue\r\n\
         type \"{stdin}\" >> \"{create_log}\"\r\n\
         type \"{created_issue}\"\r\n\
         exit /b 0\r\n\
         :do_add_blocked_by\r\n\
         type \"{stdin}\" >> \"{blocked_by_log}\"\r\n\
         type \"{add_blocked_by}\"\r\n\
         exit /b 0\r\n\
         :do_labels\r\n\
         type \"{labels}\"\r\n\
         exit /b 0\r\n\
         :do_sibling_open\r\n\
         type \"{sibling_open}\"\r\n\
         exit /b 0\r\n\
         :do_open_blockers\r\n\
         type \"{open_blockers}\"\r\n\
         exit /b 0\r\n\
         :do_held_blockers\r\n\
         type \"{held_blockers}\"\r\n\
         exit /b 0\r\n\
         :do_issue_id\r\n\
         type \"{issue_id}\"\r\n\
         exit /b 0\r\n\
         :do_repo_id\r\n\
         type \"{repo_id}\"\r\n\
         exit /b 0\r\n",
        log = files.log_file.display(),
        stdin = stdin_file.display(),
        create_log = files.create_log.display(),
        created_issue = files.created_issue_file.display(),
        blocked_by_log = files.blocked_by_log.display(),
        add_blocked_by = files.add_blocked_by_file.display(),
        labels = files.labels_file.display(),
        sibling_open = files.sibling_open_file.display(),
        open_blockers = files.open_blockers_file.display(),
        held_blockers = files.held_blockers_file.display(),
        issue_id = files.issue_id_file.display(),
        repo_id = files.repo_id_file.display(),
        graphql = files.graphql_file.display(),
    );
    fs::write(&script, body)?;
    Ok(script)
}

#[cfg(not(windows))]
fn write_real_stub(directory: &Path, files: &RealStubFiles<'_>) -> std::io::Result<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    let script = directory.join("gh");
    let stdin_file = directory.join("stdin.json");
    let body = format!(
        "#!/bin/sh\n\
         echo \"CMD=$1\" >> \"{log}\"\n\
         echo \"ENV=[$VELOCITY_BLOCK_GIT_GH,$ROSEN_CHANGE,$ROSEN_ENDPOINT]\" >> \"{log}\"\n\
         echo \"PATH=$PATH\" >> \"{log}\"\n\
         if [ \"$1\" = \"auth\" ]; then exit 1; fi\n\
         if [ \"$1\" = \"repo\" ]; then\n\
           if printf '%s\\n' \"$*\" | grep -iq \"owner/sibling\"; then\n\
             echo '{{\"nameWithOwner\":\"owner/sibling\",\"defaultBranchRef\":{{\"name\":\"main\"}}}}'\n\
           else\n\
             echo '{{\"nameWithOwner\":\"owner/repo\",\"defaultBranchRef\":{{\"name\":\"main\"}}}}'\n\
           fi\n\
           exit 0\n\
         fi\n\
         if [ \"$1\" = \"api\" ]; then\n\
           cat > \"{stdin}\"\n\
           if grep -q \"createIssue\" \"{stdin}\"; then\n\
             cat \"{stdin}\" >> \"{create_log}\"\n\
             cat \"{created_issue}\"\n\
             exit 0\n\
           fi\n\
           if grep -q \"addBlockedBy\" \"{stdin}\"; then\n\
             cat \"{stdin}\" >> \"{blocked_by_log}\"\n\
             cat \"{add_blocked_by}\"\n\
             exit 0\n\
           fi\n\
           if grep -q \"labels(first:100,after:\" \"{stdin}\"; then\n\
             cat \"{labels}\"\n\
             exit 0\n\
           fi\n\
           if grep -q \"issueDependenciesSummary\" \"{stdin}\"; then\n\
             cat \"{sibling_open}\"\n\
             exit 0\n\
           fi\n\
           if grep -q \"states:OPEN\" \"{stdin}\"; then\n\
             cat \"{open_blockers}\"\n\
             exit 0\n\
           fi\n\
           if grep -q \"id blockedBy\" \"{stdin}\"; then\n\
             cat \"{held_blockers}\"\n\
             exit 0\n\
           fi\n\
           if grep -q \"issue(number:\\$number) {{ id }}\" \"{stdin}\"; then\n\
             cat \"{issue_id}\"\n\
             exit 0\n\
           fi\n\
           if grep -q \"repository(owner:\\$owner,name:\\$name) {{ id }}\" \"{stdin}\"; then\n\
             cat \"{repo_id}\"\n\
             exit 0\n\
           fi\n\
           cat \"{graphql}\"\n\
           exit 0\n\
         fi\n\
         exit 1\n",
        log = files.log_file.display(),
        stdin = stdin_file.display(),
        create_log = files.create_log.display(),
        created_issue = files.created_issue_file.display(),
        blocked_by_log = files.blocked_by_log.display(),
        add_blocked_by = files.add_blocked_by_file.display(),
        labels = files.labels_file.display(),
        sibling_open = files.sibling_open_file.display(),
        open_blockers = files.open_blockers_file.display(),
        held_blockers = files.held_blockers_file.display(),
        issue_id = files.issue_id_file.display(),
        repo_id = files.repo_id_file.display(),
        graphql = files.graphql_file.display(),
    );
    fs::write(&script, body)?;
    fs::set_permissions(&script, fs::Permissions::from_mode(0o700))?;
    Ok(script)
}

#[cfg(windows)]
fn write_refusal_stub(
    directory: &Path,
    log_file: Option<&Path>,
    message: &str,
) -> std::io::Result<PathBuf> {
    let script = directory.join("gh.cmd");
    let mut body = String::from("@echo off\r\n");
    if let Some(log) = log_file {
        body.push_str(&format!("echo called>> \"{}\"\r\n", log.display()));
    }
    body.push_str(&format!("echo {message} 1>&2\r\nexit /b 1\r\n"));
    fs::write(&script, body)?;
    Ok(script)
}

#[cfg(not(windows))]
fn write_refusal_stub(
    directory: &Path,
    log_file: Option<&Path>,
    message: &str,
) -> std::io::Result<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    let script = directory.join("gh");
    let mut body = String::from("#!/bin/sh\n");
    if let Some(log) = log_file {
        body.push_str(&format!("echo called >> \"{}\"\n", log.display()));
    }
    // The message holds apostrophes (`velocity-run's`), which end a single-quoted word.
    let quoted = message.replace('\'', r"'\''");
    body.push_str(&format!("echo '{quoted}' >&2\nexit 1\n"));
    fs::write(&script, body)?;
    fs::set_permissions(&script, fs::Permissions::from_mode(0o700))?;
    Ok(script)
}
