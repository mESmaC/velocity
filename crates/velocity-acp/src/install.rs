//! Installing a built-in provider's ACP agent from Settings → Providers (F15 Install, #221):
//! which commands put each agent's program on your `PATH`. Every route is the provider's
//! own published package, installed for your user, so nothing here pins or bundles an agent.

use crate::Provider;

/// Where Velocity runs, which decides the package manager each agent installs through.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InstallPlatform {
    Windows,
    /// macOS and Linux: both run the providers' own `sh` installers.
    Unix,
}

impl InstallPlatform {
    pub fn current() -> Self {
        if cfg!(windows) {
            Self::Windows
        } else {
            Self::Unix
        }
    }
}

/// One command of an install, run with no console window.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstallStep {
    pub program: String,
    pub args: Vec<String>,
    /// Exit codes besides 0 that still mean the package is in place, e.g. winget's
    /// "already installed".
    pub accepted_exit_codes: Vec<i32>,
}

impl InstallStep {
    fn new(program: &str, args: &[&str]) -> Self {
        Self {
            program: program.to_owned(),
            args: args.iter().map(|argument| (*argument).to_owned()).collect(),
            accepted_exit_codes: Vec::new(),
        }
    }

    /// The step as you would type it, for the card's tooltip and error messages.
    pub fn command_line(&self) -> String {
        std::iter::once(self.program.as_str())
            .chain(self.args.iter().map(String::as_str))
            .map(|word| {
                if word.contains(char::is_whitespace) {
                    format!("\"{word}\"")
                } else {
                    word.to_owned()
                }
            })
            .collect::<Vec<_>>()
            .join(" ")
    }

    pub fn succeeded(&self, exit_code: Option<i32>) -> bool {
        exit_code.is_some_and(|code| code == 0 || self.accepted_exit_codes.contains(&code))
    }
}

/// How one provider's agent is published.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Route {
    /// An npm package whose `bin` is the agent's program.
    Npm(&'static str),
    /// A winget package on Windows and the provider's `sh` installer elsewhere.
    Native {
        winget: &'static str,
        script: &'static str,
    },
}

fn route(provider: Provider) -> Option<Route> {
    match provider {
        Provider::Anthropic => Some(Route::Npm("@agentclientprotocol/claude-agent-acp")),
        Provider::Codex => Some(Route::Npm("@agentclientprotocol/codex-acp")),
        // Kimi Code 2.x (the one with `kimi acp`, #84), not the older Python Kimi CLI.
        Provider::KimiCode => Some(Route::Native {
            winget: "MoonshotAI.KimiCodeCLI",
            script: "https://code.kimi.com/kimi-code/install.sh",
        }),
        Provider::Antigravity => Some(Route::Native {
            winget: "Google.AntigravityCLI",
            script: "https://antigravity.google/cli/install.sh",
        }),
        Provider::Custom(_) => None,
    }
}

/// winget's "no newer version" and "already installed": the package is there, so the
/// install did its job and the agent is checked again. These are the HRESULTs
/// `0x8A15002B` and `0x8A150061` as the signed exit codes Windows reports.
const WINGET_ALREADY_INSTALLED: [i32; 2] = [-1_978_335_189, -1_978_335_135];

/// Node.js LTS from winget, for a machine that has no npm yet. It brings npm and puts its
/// global folder on your `PATH`.
const NODE_PACKAGE: &str = "OpenJS.NodeJS.LTS";

fn winget(package: &str) -> InstallStep {
    let mut step = InstallStep::new(
        "winget",
        &[
            "install",
            "--id",
            package,
            "--exact",
            "--source",
            "winget",
            "--silent",
            "--accept-package-agreements",
            "--accept-source-agreements",
            "--disable-interactivity",
        ],
    );
    step.accepted_exit_codes = WINGET_ALREADY_INSTALLED.to_vec();
    step
}

/// The commands that install `provider`'s built-in agent, in order. `npm_found` says whether
/// `npm` is on your `PATH`: on Windows a missing npm is installed first with Node.js.
/// `None` for an agent you added, which Velocity knows nothing about.
pub fn install_steps(
    provider: Provider,
    platform: InstallPlatform,
    npm_found: bool,
) -> Option<Vec<InstallStep>> {
    let steps = match (route(provider)?, platform) {
        (Route::Npm(package), platform) => {
            let mut steps = Vec::new();
            if !npm_found && platform == InstallPlatform::Windows {
                steps.push(winget(NODE_PACKAGE));
            }
            steps.push(InstallStep::new("npm", &["install", "--global", package]));
            steps
        }
        (
            Route::Native {
                winget: package, ..
            },
            InstallPlatform::Windows,
        ) => vec![winget(package)],
        (Route::Native { script, .. }, InstallPlatform::Unix) => {
            let pipeline = format!("set -o pipefail; curl -fsSL {script} | bash");
            vec![InstallStep::new("bash", &["-c", &pipeline])]
        }
    };
    Some(steps)
}
