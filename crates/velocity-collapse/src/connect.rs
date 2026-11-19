//! Collapse's desktop sign-in for Velocity (Collapse C1, collapse.sh#80).
//!
//! Velocity listens on an ephemeral IPv4 loopback port, opens `https://collapse.sh/connect`
//! with a fresh state and an S256 PKCE challenge, waits for the approval's redirect, checks
//! the state and exchanges the code for the account's two tokens. No token is ever typed
//! or pasted into Velocity.

use std::net::Ipv4Addr;
use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::time::timeout;

use crate::CollapseError;
use crate::credential::{CollapseCredentials, save_credentials};
use crate::url_text::{encode_pairs, query_value};

const CONNECT_PAGE: &str = "https://collapse.sh/connect";
const TOKEN_URL: &str = "https://api.collapse.sh/api/connect/token";
const CLIENT_ID: &str = "velocity";
const CALLBACK_PATH: &str = "/collapse/callback";
/// Long enough to sign in to GitHub first; Collapse's code itself lasts two minutes.
const SIGN_IN_TIMEOUT: Duration = Duration::from_secs(10 * 60);
/// A browser sends its request line at once; a silent connection is dropped.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
const EXCHANGE_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_REQUEST_BYTES: usize = 16 * 1024;
/// Collapse accepts a machine name of at most 88 UTF-8 bytes.
const MAX_MACHINE_BYTES: usize = 88;

/// Runs the whole sign-in: `open_browser` receives the approval page's URL. The tokens are
/// saved to the OS credential store before this returns.
pub async fn connect(
    machine: &str,
    open_browser: impl FnOnce(&str) -> Result<(), String>,
) -> Result<CollapseCredentials, CollapseError> {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await?;
    let port = listener.local_addr()?.port();
    let request = SignInRequest::new(machine, format!("http://127.0.0.1:{port}{CALLBACK_PATH}"));
    open_browser(&request.approval_url()).map_err(CollapseError::Browser)?;
    let code = timeout(SIGN_IN_TIMEOUT, wait_for_code(&listener, &request.state))
        .await
        .map_err(|_| CollapseError::TimedOut)??;
    drop(listener);
    let credentials = exchange(&request, &code).await?;
    let saved = credentials.clone();
    tokio::task::spawn_blocking(move || save_credentials(&saved))
        .await
        .map_err(|error| CollapseError::Credential(error.to_string()))??;
    Ok(credentials)
}

struct SignInRequest {
    machine: String,
    redirect_uri: String,
    state: String,
    verifier: String,
}

impl SignInRequest {
    fn new(machine: &str, redirect_uri: String) -> Self {
        Self {
            machine: machine_name(machine),
            redirect_uri,
            // 32 random hex characters: inside Collapse's 22–128 unreserved characters.
            state: uuid::Uuid::new_v4().simple().to_string(),
            // 64 hex characters from two random UUIDs: inside PKCE's 43–128.
            verifier: format!(
                "{}{}",
                uuid::Uuid::new_v4().simple(),
                uuid::Uuid::new_v4().simple()
            ),
        }
    }

    fn approval_url(&self) -> String {
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(self.verifier.as_bytes()));
        let query = encode_pairs(&[
            ("client_id", CLIENT_ID),
            ("response_type", "code"),
            ("machine", &self.machine),
            ("redirect_uri", &self.redirect_uri),
            ("state", &self.state),
            ("code_challenge", &challenge),
            ("code_challenge_method", "S256"),
        ]);
        format!("{CONNECT_PAGE}?{query}")
    }
}

/// A printable name of at most 88 bytes with no surrounding whitespace, as Collapse requires.
fn machine_name(machine: &str) -> String {
    let printable: String = machine.chars().filter(|item| !item.is_control()).collect();
    let mut name = printable.trim().to_owned();
    while name.len() > MAX_MACHINE_BYTES {
        name.pop();
    }
    let name = name.trim_end();
    if name.is_empty() {
        String::from("this computer")
    } else {
        name.to_owned()
    }
}

/// Serves the loopback port until the approval's redirect arrives with our state. Requests
/// for other paths, and replies carrying another state, are answered and ignored.
async fn wait_for_code(listener: &TcpListener, state: &str) -> Result<String, CollapseError> {
    loop {
        let (mut stream, _) = listener.accept().await?;
        let Ok(Some(target)) = timeout(REQUEST_TIMEOUT, read_request_target(&mut stream)).await
        else {
            continue;
        };
        let reply = callback_reply(&target, state);
        let (status, message) = match &reply {
            Some(Ok(_)) => (
                "200 OK",
                "Velocity is connecting to Collapse. You can close this tab.",
            ),
            Some(Err(_)) => (
                "200 OK",
                "The Collapse connection was not approved. You can close this tab.",
            ),
            None => ("404 Not Found", "Not found."),
        };
        // The browser page is a courtesy; a failed write must not lose the code.
        let _ = respond(&mut stream, status, message).await;
        if let Some(result) = reply {
            return result;
        }
    }
}

/// The request target of an HTTP request line (`GET <target> HTTP/1.1`), read up to the end
/// of the headers so the browser sees a clean reply.
async fn read_request_target(stream: &mut TcpStream) -> Option<String> {
    let mut received = Vec::new();
    let mut buffer = [0_u8; 2048];
    while !received.windows(4).any(|window| window == b"\r\n\r\n") {
        let read = stream.read(&mut buffer).await.ok()?;
        if read == 0 || received.len() + read > MAX_REQUEST_BYTES {
            break;
        }
        received.extend_from_slice(&buffer[..read]);
    }
    let text = String::from_utf8_lossy(&received);
    let mut parts = text.lines().next()?.split_whitespace();
    let method = parts.next()?;
    let target = parts.next()?;
    (method == "GET").then(|| target.to_owned())
}

/// `None` for anything but the callback with our state; otherwise the code or the refusal.
fn callback_reply(target: &str, state: &str) -> Option<Result<String, CollapseError>> {
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    if path != CALLBACK_PATH || query_value(query, "state").as_deref() != Some(state) {
        return None;
    }
    if let Some(error) = query_value(query, "error") {
        return Some(Err(if error == "access_denied" {
            CollapseError::Denied
        } else {
            CollapseError::Exchange(format!("Collapse answered `{error}`"))
        }));
    }
    query_value(query, "code")
        .filter(|code| !code.is_empty())
        .map(Ok)
}

async fn respond(stream: &mut TcpStream, status: &str, message: &str) -> std::io::Result<()> {
    let body = format!(
        "<!doctype html><meta charset=\"utf-8\"><title>Velocity · Collapse</title><p>{message}</p>"
    );
    let reply = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(reply.as_bytes()).await?;
    stream.shutdown().await
}

#[derive(Deserialize)]
struct TokenReply {
    account: String,
    token_type: String,
    mcp_token: String,
    access_token: String,
}

async fn exchange(
    request: &SignInRequest,
    code: &str,
) -> Result<CollapseCredentials, CollapseError> {
    let body = encode_pairs(&[
        ("grant_type", "authorization_code"),
        ("client_id", CLIENT_ID),
        ("code", code),
        ("code_verifier", &request.verifier),
        ("redirect_uri", &request.redirect_uri),
    ]);
    let failed = |error: reqwest::Error| CollapseError::Exchange(error.without_url().to_string());
    let response = reqwest::Client::builder()
        .timeout(EXCHANGE_TIMEOUT)
        .build()
        .map_err(failed)?
        .post(TOKEN_URL)
        .header(
            reqwest::header::CONTENT_TYPE,
            "application/x-www-form-urlencoded",
        )
        .header(reqwest::header::ACCEPT, "application/json")
        .body(body)
        .send()
        .await
        .map_err(failed)?;
    let status = response.status();
    if !status.is_success() {
        let explanation = response.text().await.unwrap_or_default();
        let explanation: String = explanation.trim().chars().take(200).collect();
        return Err(CollapseError::Exchange(format!("{status} {explanation}")));
    }
    let reply: TokenReply = response.json().await.map_err(failed)?;
    let complete = reply.token_type.eq_ignore_ascii_case("bearer")
        && !reply.account.trim().is_empty()
        && !reply.mcp_token.is_empty()
        && !reply.access_token.is_empty();
    if !complete {
        return Err(CollapseError::Exchange(
            "Collapse returned an incomplete connection".into(),
        ));
    }
    Ok(CollapseCredentials {
        account: reply.account.trim().to_owned(),
        mcp_token: reply.mcp_token,
        access_token: reply.access_token,
    })
}
