use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use serde_json::json;
use velocity_acp::client::{AgentProcess, hidden_command, spawn_owned_command, stop_owned_process};

use crate::{Result, cdp::Cdp, error};

pub const MEMORY_BUDGET: u64 = 1 << 30;
pub const WIDTH: u32 = 1280;
pub const HEIGHT: u32 = 800;

fn on_path(names: &[&str]) -> Option<PathBuf> {
    std::env::var_os("PATH").and_then(|path| {
        std::env::split_paths(&path)
            .flat_map(|folder| names.iter().map(move |name| folder.join(name)))
            .find(|path| path.is_file())
    })
}

#[cfg(windows)]
fn executable() -> Option<PathBuf> {
    ["ProgramFiles(x86)", "ProgramFiles", "LOCALAPPDATA"]
        .into_iter()
        .filter_map(std::env::var_os)
        .map(|folder| PathBuf::from(folder).join("Microsoft/Edge/Application/msedge.exe"))
        .find(|path| path.is_file())
        .or_else(|| on_path(&["msedge.exe"]))
}

#[cfg(not(windows))]
fn executable() -> Option<PathBuf> {
    [
        "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
        "/Applications/Chromium.app/Contents/MacOS/Chromium",
    ]
    .into_iter()
    .map(PathBuf::from)
    .find(|path| path.is_file())
    .or_else(|| on_path(&["google-chrome", "chromium", "chromium-browser", "chrome"]))
}

pub struct Engine {
    process: AgentProcess,
    pid: u32,
    pub cdp: Cdp,
}

impl Engine {
    pub async fn start(profile: &Path) -> Result<Self> {
        let program = executable().ok_or_else(|| {
            error("Install Microsoft Edge on Windows or Chrome or Chromium on this machine")
        })?;
        let endpoint = prepare_profile(profile).await?;
        let mut user_data = std::ffi::OsString::from("--user-data-dir=");
        user_data.push(profile);
        let mut command = hidden_command(program);
        command
            .args([
                "--headless=new",
                "--remote-debugging-port=0",
                "--remote-debugging-address=127.0.0.1",
                "--no-first-run",
                "--no-default-browser-check",
                "--disable-background-networking",
                "--disable-sync",
                "--disable-extensions",
                "--force-device-scale-factor=1",
                "--restore-last-session",
            ])
            .arg(format!("--window-size={WIDTH},{HEIGHT}"))
            .arg(user_data)
            .arg("about:blank")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        let mut process = spawn_owned_command(command)?;
        let (cdp, pid) = match connect(&mut process, endpoint).await {
            Ok(connection) => connection,
            Err(error) => {
                let _ = stop_owned_process(&mut process).await;
                return Err(error);
            }
        };
        Ok(Self { process, cdp, pid })
    }

    pub fn pid(&self) -> u32 {
        self.pid
    }

    pub async fn close(mut self) {
        let _ = self.cdp.browser("Browser.close", json!({})).await;
        let shutdown = async {
            #[cfg(windows)]
            {
                let pid = self.pid;
                tokio::task::spawn_blocking(move || wait_for_browser(pid))
                    .await
                    .map_err(std::io::Error::other)??;
            }
            self.process.wait().await
        };
        if !matches!(
            tokio::time::timeout(Duration::from_secs(5), shutdown).await,
            Ok(Ok(_))
        ) {
            let _ = stop_owned_process(&mut self.process).await;
        }
    }
}

#[cfg(windows)]
fn wait_for_browser(pid: u32) -> std::io::Result<()> {
    use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};

    // The launcher can already be gone. Give Chromium time to flush its profile before
    // dropping the job object and terminating any remaining children.
    let mut system = System::new().map_err(std::io::Error::other)?;
    let pid = Pid::from_u32(pid);
    system.refresh_processes_specifics(
        ProcessesToUpdate::Some(&[pid]),
        true,
        ProcessRefreshKind::nothing(),
    );
    if let Some(browser) = system.process(pid) {
        let _ = browser.wait();
    }
    Ok(())
}

async fn prepare_profile(profile: &Path) -> Result<PathBuf> {
    let default_profile = profile.join("Default");
    tokio::fs::create_dir_all(&default_profile).await?;
    let preferences = default_profile.join("Preferences");
    if !tokio::fs::try_exists(&preferences).await? {
        tokio::fs::write(
            preferences,
            r#"{"credentials_enable_service":false,"profile":{"password_manager_enabled":false},"autofill":{"profile_enabled":false,"credit_card_enabled":false},"session":{"restore_on_startup":1}}"#,
        )
        .await?;
    }
    let endpoint = profile.join("DevToolsActivePort");
    match tokio::fs::remove_file(&endpoint).await {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    Ok(endpoint)
}

async fn connect(process: &mut AgentProcess, endpoint: PathBuf) -> Result<(Cdp, u32)> {
    let until = Instant::now() + Duration::from_secs(10);
    while Instant::now() < until {
        if let Ok(text) = tokio::fs::read_to_string(&endpoint).await {
            let mut lines = text.lines();
            if let (Some(port), Some(path)) = (lines.next(), lines.next())
                && let Ok(port) = port.parse::<u16>()
                && path.starts_with("/devtools/browser/")
                && let Ok(cdp) = Cdp::connect(format!("ws://127.0.0.1:{port}{path}")).await
            {
                let pid = browser_pid(&cdp).await?;
                return Ok((cdp, pid));
            }
        }
        // Edge's launcher can exit successfully while the browser in the same job starts.
        if let Some(status) = process.try_wait()?
            && !status.success()
        {
            return Err(error(format!(
                "The installed browser exited during headless startup ({status})"
            )));
        }
        tokio::time::sleep(Duration::from_millis(80)).await;
    }
    Err(error(
        "The browser did not open its local DevTools connection",
    ))
}

async fn browser_pid(cdp: &Cdp) -> Result<u32> {
    let result = cdp.browser("SystemInfo.getProcessInfo", json!({})).await?;
    result["processInfo"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|process| process["type"] == "browser")
        .and_then(|process| process["id"].as_u64())
        .and_then(|pid| u32::try_from(pid).ok())
        .filter(|&pid| pid != 0)
        .ok_or_else(|| error("DevTools did not report the browser process ID"))
}
