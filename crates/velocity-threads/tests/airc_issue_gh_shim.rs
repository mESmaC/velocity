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
use velocity_airc::{IssueRead, IssueRequest};
use velocity_github::GH_PROGRAM_ENV;
use velocity_rosen::agent_run::{BLOCK_VCS_ENV, vcs_refusal};
use velocity_store::{PersistedState, ProjectItem, StateStore};
use velocity_threads::issue_activity::{handle_issue_request, read_issue_tool_failure};

const CHILD_APP_DATA_ENV: &str = "VELOCITY_TEST_ISSUE_581_APP_DATA";
const CHILD_EXPECT_FAILURE_ENV: &str = "VELOCITY_TEST_ISSUE_581_EXPECT_FAIL";
const CHILD_TEST_NAME: &str = "child_executes_airc_issue_read";
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
    verify_agent_shell_gh_still_refused(&fixture)?;
    verify_project_health_failure_recording(&fixture)?;
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
            offset: 0,
            comments: 5,
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
    agent_path: OsString,
}

fn setup_fixture(root: &Path) -> Result<Fixture, Box<dyn std::error::Error>> {
    let app_data = root.join("app-data");
    let repo = root.join("repo");
    let workspace = root.join("workspace");
    let shim_dir = root.join("velocity-agent-shim");
    let real_dir = root.join("real-gh");
    let local_fail_dir = root.join("local-fail-gh");
    let github_404_dir = root.join("github-404-gh");
    for dir in [
        &app_data,
        &repo,
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
    fs::write(
        app_data.join(velocity_store::STATE_FILE_NAME),
        serde_json::to_vec(&legacy)?,
    )?;
    let _store = StateStore::load_or_default(&app_data)?;

    fs::write(shim_dir.join("velocity-program"), b"velocity")?;
    let shim_log = root.join("shim-called.log");
    let real_log = root.join("real-called.log");
    let graphql_file = root.join("issue-42.json");
    fs::write(
        &graphql_file,
        r#"{"data":{"repository":{"issue":{"id":"I_42","number":42,"title":"Stub issue","body":"Reached real gh","url":"https://github.com/owner/repo/issues/42","state":"OPEN","author":{"login":"octocat"},"createdAt":"2026-01-01T00:00:00Z","updatedAt":"2026-01-01T00:00:00Z","closedAt":null,"milestone":null,"labels":{"nodes":[],"totalCount":0},"blockedBy":{"nodes":[],"pageInfo":{"hasNextPage":false,"endCursor":null}},"comments":{"totalCount":0,"nodes":[]}}}}}"#,
    )?;

    write_refusal_stub(
        &shim_dir,
        Some(&shim_log),
        "gh: Agents do not call gh directly. Use AIRC issue tools (issue_read, issue_create, issue_comment, issue_ready_to_close) or velocity-run's workflow_run instead.",
    )?;
    let real_gh = write_real_stub(&real_dir, &real_log, &graphql_file)?;
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
        agent_path,
    })
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
        .env(CHILD_APP_DATA_ENV, &fixture.app_data);
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
fn write_real_stub(
    directory: &Path,
    log_file: &Path,
    graphql_file: &Path,
) -> std::io::Result<PathBuf> {
    let script = directory.join("gh.cmd");
    let body = format!(
        "@echo off\r\n\
         echo CMD=%1>> \"{log}\"\r\n\
         echo ENV=[%VELOCITY_BLOCK_GIT_GH%,%ROSEN_CHANGE%,%ROSEN_ENDPOINT%]>> \"{log}\"\r\n\
         echo PATH=%PATH%>> \"{log}\"\r\n\
         if \"%1\"==\"auth\" exit /b 1\r\n\
         if \"%1\"==\"repo\" (\r\n\
         echo {{\"nameWithOwner\":\"owner/repo\",\"defaultBranchRef\":{{\"name\":\"main\"}}}}\r\n\
         exit /b 0\r\n\
         )\r\n\
         if \"%1\"==\"api\" (\r\n\
         findstr \"^\" > nul\r\n\
         type \"{graphql}\"\r\n\
         exit /b 0\r\n\
         )\r\n\
         exit /b 1\r\n",
        log = log_file.display(),
        graphql = graphql_file.display(),
    );
    fs::write(&script, body)?;
    Ok(script)
}

#[cfg(not(windows))]
fn write_real_stub(
    directory: &Path,
    log_file: &Path,
    graphql_file: &Path,
) -> std::io::Result<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    let script = directory.join("gh");
    let body = format!(
        "#!/bin/sh\n\
         echo \"CMD=$1\" >> \"{log}\"\n\
         echo \"ENV=[$VELOCITY_BLOCK_GIT_GH,$ROSEN_CHANGE,$ROSEN_ENDPOINT]\" >> \"{log}\"\n\
         echo \"PATH=$PATH\" >> \"{log}\"\n\
         if [ \"$1\" = \"auth\" ]; then exit 1; fi\n\
         if [ \"$1\" = \"repo\" ]; then\n\
           echo '{{\"nameWithOwner\":\"owner/repo\",\"defaultBranchRef\":{{\"name\":\"main\"}}}}'\n\
           exit 0\n\
         fi\n\
         if [ \"$1\" = \"api\" ]; then\n\
           cat >/dev/null\n\
           cat \"{graphql}\"\n\
           exit 0\n\
         fi\n\
         exit 1\n",
        log = log_file.display(),
        graphql = graphql_file.display(),
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
    body.push_str(&format!("echo '{message}' >&2\nexit 1\n"));
    fs::write(&script, body)?;
    fs::set_permissions(&script, fs::Permissions::from_mode(0o700))?;
    Ok(script)
}
