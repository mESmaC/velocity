//! In-process GitHub HTTPS transport backed by `reqwest` and the user's signed-in `gh`
//! OAuth token (#556). Eliminates spawning a `gh` process per API call, pools TLS
//! connections across the app, and sends conditional `If-None-Match` ETags on REST GETs so
//! unchanged reads answer `304 Not Modified` without spending GitHub's rate limit.
//!
//! When `gh auth token` is unavailable (for example, signed out or a fake `gh` in a smoke
//! test), callers receive [`TransportAttempt::Fallback`] and run `gh` as before.

use std::collections::HashMap;
use std::io::Read;
use std::panic::Location;
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{LazyLock, Mutex, OnceLock, mpsc};
use std::time::{Duration, Instant};

use reqwest::header::{ACCEPT, AUTHORIZATION, ETAG, IF_NONE_MATCH, LINK, USER_AGENT};
use reqwest::{Method, StatusCode};
use serde::de::DeserializeOwned;

use crate::gh_command::GitHubCommandError;

const API_BASE: &str = "https://api.github.com";
const DEFAULT_ACCEPT: &str = "application/vnd.github+json";
const API_VERSION_HEADER: &str = "X-GitHub-Api-Version";
const API_VERSION: &str = "2022-11-28";
const CLIENT_USER_AGENT: &str = "velocity-desktop";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);
const TOKEN_DEADLINE: Duration = Duration::from_secs(15);
const UNAVAILABLE_RETRY: Duration = Duration::from_secs(60);
const MAX_ETAG_ENTRIES: usize = 512;
const MAX_REST_PAGES: usize = 50;

pub(crate) enum TransportAttempt<T> {
    Handled(Result<T, GitHubCommandError>),
    Fallback,
}

enum TokenState {
    Unknown,
    Available(String),
    Unavailable(Instant),
}

struct CachedEtag {
    etag: String,
    body: Vec<u8>,
    last_used: u64,
}

struct HttpJob {
    project: String,
    caller: &'static Location<'static>,
    operation: String,
    method: Method,
    url: String,
    accept: String,
    token: String,
    etag: Option<String>,
    body: Option<Vec<u8>>,
    reply: mpsc::SyncSender<Result<HttpOutcome, GitHubCommandError>>,
}

struct HttpOutcome {
    status: StatusCode,
    etag: Option<String>,
    next_link: Option<String>,
    body: Vec<u8>,
}

static TOKEN: LazyLock<Mutex<TokenState>> = LazyLock::new(|| Mutex::new(TokenState::Unknown));
static ETAG_CACHE: LazyLock<Mutex<HashMap<(String, String), CachedEtag>>> =
    LazyLock::new(Mutex::default);
static ETAG_SEQ: AtomicU64 = AtomicU64::new(1);
static WORKER: OnceLock<Option<tokio::sync::mpsc::UnboundedSender<HttpJob>>> = OnceLock::new();

/// REST GET with `ETag` / `If-None-Match` conditional caching.
pub(crate) fn get(
    project: &str,
    caller: &'static Location<'static>,
    endpoint: &str,
    accept: Option<&str>,
) -> TransportAttempt<Vec<u8>> {
    let url = api_url(endpoint);
    let accept = accept.unwrap_or(DEFAULT_ACCEPT);
    let label = format!("api {}", endpoint.trim_start_matches('/'));
    match execute_get(project, caller, &url, accept, &label) {
        TransportAttempt::Handled(Ok((body, _))) => TransportAttempt::Handled(Ok(body)),
        TransportAttempt::Handled(Err(error)) => TransportAttempt::Handled(Err(error)),
        TransportAttempt::Fallback => TransportAttempt::Fallback,
    }
}

/// Follows GitHub REST `Link: <...>; rel="next"` pagination with per-page ETag caching.
pub(crate) fn get_paginated<T: DeserializeOwned>(
    project: &str,
    caller: &'static Location<'static>,
    endpoint: &str,
) -> TransportAttempt<Vec<T>> {
    let mut url = api_url(endpoint);
    let label = format!("api {}", endpoint.trim_start_matches('/'));
    let mut items = Vec::new();
    for _ in 0..MAX_REST_PAGES {
        let (body, next) = match execute_get(project, caller, &url, DEFAULT_ACCEPT, &label) {
            TransportAttempt::Handled(Ok(page)) => page,
            TransportAttempt::Handled(Err(error)) => return TransportAttempt::Handled(Err(error)),
            TransportAttempt::Fallback => return TransportAttempt::Fallback,
        };
        let page: Vec<T> = match serde_json::from_slice(&body) {
            Ok(page) => page,
            Err(error) => return TransportAttempt::Handled(Err(error.into())),
        };
        items.extend(page);
        match next {
            Some(next_url) if next_url.starts_with(API_BASE) => url = next_url,
            _ => return TransportAttempt::Handled(Ok(items)),
        }
    }
    TransportAttempt::Handled(Ok(items))
}

struct HttpRequest<'a> {
    project: &'a str,
    caller: &'static Location<'static>,
    method: &'a Method,
    url: &'a str,
    accept: &'a str,
    etag: Option<&'a str>,
    body: Option<&'a [u8]>,
    operation: &'a str,
}

/// REST mutation (`POST`/`PATCH`/`DELETE`/`PUT`) or GraphQL (`POST graphql`).
pub(crate) fn send(
    project: &str,
    caller: &'static Location<'static>,
    method: &str,
    endpoint: &str,
    body: Option<&[u8]>,
    graphql_path: Option<&str>,
) -> TransportAttempt<Vec<u8>> {
    let Ok(method) = Method::from_bytes(method.as_bytes()) else {
        return TransportAttempt::Fallback;
    };
    let Some(token) = cached_token(caller) else {
        return TransportAttempt::Fallback;
    };
    let Some(worker) = worker_sender() else {
        return TransportAttempt::Fallback;
    };
    let url = api_url(endpoint);
    let operation = match graphql_path {
        Some(path) => format!("graphql {path}"),
        None => format!("api {method}"),
    };
    let request = HttpRequest {
        project,
        caller,
        method: &method,
        url: &url,
        accept: DEFAULT_ACCEPT,
        etag: None,
        body,
        operation: &operation,
    };
    let outcome = match dispatch_with_auth_retry(worker, &token, &request) {
        Ok(outcome) => outcome,
        Err(error) => return TransportAttempt::Handled(Err(error)),
    };
    if !outcome.status.is_success() {
        return TransportAttempt::Handled(Err(http_status_error(outcome.status, &outcome.body)));
    }
    TransportAttempt::Handled(Ok(outcome.body))
}

fn execute_get(
    project: &str,
    caller: &'static Location<'static>,
    url: &str,
    accept: &str,
    operation: &str,
) -> TransportAttempt<(Vec<u8>, Option<String>)> {
    let Some(token) = cached_token(caller) else {
        return TransportAttempt::Fallback;
    };
    let Some(worker) = worker_sender() else {
        return TransportAttempt::Fallback;
    };
    let cache_key = (url.to_owned(), accept.to_owned());
    let etag = cached_etag(&cache_key);
    let request = HttpRequest {
        project,
        caller,
        method: &Method::GET,
        url,
        accept,
        etag: etag.as_deref(),
        body: None,
        operation,
    };
    let outcome = match dispatch_with_auth_retry(worker, &token, &request) {
        Ok(outcome) => outcome,
        Err(error) => return TransportAttempt::Handled(Err(error)),
    };
    if outcome.status == StatusCode::NOT_MODIFIED {
        if let Some(body) = cached_body(&cache_key) {
            return TransportAttempt::Handled(Ok((body, outcome.next_link)));
        }
        // Evicted between request and response; re-read unconditionally.
        let unconditional = HttpRequest {
            etag: None,
            ..request
        };
        let fresh = match dispatch_with_auth_retry(worker, &token, &unconditional) {
            Ok(outcome) => outcome,
            Err(error) => return TransportAttempt::Handled(Err(error)),
        };
        return finish_get(cache_key, fresh);
    }
    finish_get(cache_key, outcome)
}

fn finish_get(
    cache_key: (String, String),
    outcome: HttpOutcome,
) -> TransportAttempt<(Vec<u8>, Option<String>)> {
    if !outcome.status.is_success() {
        return TransportAttempt::Handled(Err(http_status_error(outcome.status, &outcome.body)));
    }
    if let Some(etag) = outcome.etag {
        store_etag(cache_key, etag, outcome.body.clone());
    }
    TransportAttempt::Handled(Ok((outcome.body, outcome.next_link)))
}

fn dispatch_with_auth_retry(
    worker: &tokio::sync::mpsc::UnboundedSender<HttpJob>,
    initial_token: &str,
    request: &HttpRequest<'_>,
) -> Result<HttpOutcome, GitHubCommandError> {
    dispatch_with_token_refresh(worker, initial_token, request, |stale| {
        refresh_token(request.caller, stale)
    })
}

fn dispatch_with_token_refresh(
    worker: &tokio::sync::mpsc::UnboundedSender<HttpJob>,
    initial_token: &str,
    request: &HttpRequest<'_>,
    refresh: impl FnOnce(&str) -> Option<String>,
) -> Result<HttpOutcome, GitHubCommandError> {
    let outcome = dispatch_once(worker, initial_token, request)?;
    if outcome.status != StatusCode::UNAUTHORIZED {
        return Ok(outcome);
    }
    let Some(refreshed) = refresh(initial_token) else {
        return Ok(outcome);
    };
    dispatch_once(worker, &refreshed, request)
}

fn record_outcome(
    project: &str,
    caller: &'static Location<'static>,
    operation: &str,
    status: Option<StatusCode>,
) {
    let label = if status == Some(StatusCode::NOT_MODIFIED) {
        format!("{operation} (304)")
    } else {
        operation.to_owned()
    };
    crate::request_counter::record_for_project(project, caller, label);
}

/// Counts each dispatched HTTP attempt once, keeping `304` labels when headers arrive while still
/// metering transport timeouts, body read failures, and interrupted worker tasks (#695).
struct AttemptMeter<'a> {
    project: &'a str,
    caller: &'static Location<'static>,
    operation: &'a str,
    recorded: bool,
}

impl<'a> AttemptMeter<'a> {
    fn new(job: &'a HttpJob) -> Self {
        Self {
            project: &job.project,
            caller: job.caller,
            operation: &job.operation,
            recorded: false,
        }
    }

    fn record(&mut self, status: Option<StatusCode>) {
        if !self.recorded {
            self.recorded = true;
            record_outcome(self.project, self.caller, self.operation, status);
        }
    }
}

impl Drop for AttemptMeter<'_> {
    fn drop(&mut self) {
        if !self.recorded {
            self.recorded = true;
            record_outcome(self.project, self.caller, self.operation, None);
        }
    }
}

fn dispatch_once(
    worker: &tokio::sync::mpsc::UnboundedSender<HttpJob>,
    token: &str,
    request: &HttpRequest<'_>,
) -> Result<HttpOutcome, GitHubCommandError> {
    let (reply_tx, reply_rx) = mpsc::sync_channel(1);
    let job = HttpJob {
        project: request.project.to_owned(),
        caller: request.caller,
        operation: request.operation.to_owned(),
        method: request.method.clone(),
        url: request.url.to_owned(),
        accept: request.accept.to_owned(),
        token: token.to_owned(),
        etag: request.etag.map(str::to_owned),
        body: request.body.map(<[u8]>::to_vec),
        reply: reply_tx,
    };
    worker
        .send(job)
        .map_err(|_| GitHubCommandError::Invalid("GitHub HTTP worker stopped".into()))?;
    reply_rx
        .recv()
        .map_err(|_| GitHubCommandError::Invalid("GitHub HTTP worker dropped reply".into()))?
}

fn worker_sender() -> Option<&'static tokio::sync::mpsc::UnboundedSender<HttpJob>> {
    WORKER
        .get_or_init(|| {
            let client = reqwest::Client::builder()
                .timeout(REQUEST_TIMEOUT)
                .user_agent(CLIENT_USER_AGENT)
                .build()
                .ok()?;
            spawn_worker(client)
        })
        .as_ref()
}

fn spawn_worker(client: reqwest::Client) -> Option<tokio::sync::mpsc::UnboundedSender<HttpJob>> {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<HttpJob>();
    let started = std::thread::Builder::new()
        .name("velocity-github-http".into())
        .spawn(move || {
            let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            else {
                return;
            };
            runtime.block_on(async move {
                while let Some(job) = rx.recv().await {
                    let client = client.clone();
                    tokio::spawn(async move {
                        let result = run_http_job(&client, &job).await;
                        let _ = job.reply.send(result);
                    });
                }
            });
        });
    started.ok().map(|_| tx)
}

async fn run_http_job(
    client: &reqwest::Client,
    job: &HttpJob,
) -> Result<HttpOutcome, GitHubCommandError> {
    let mut request = client
        .request(job.method.clone(), &job.url)
        .header(AUTHORIZATION, format!("Bearer {}", job.token))
        .header(ACCEPT, &job.accept)
        .header(USER_AGENT, CLIENT_USER_AGENT)
        .header(API_VERSION_HEADER, API_VERSION);
    if let Some(etag) = &job.etag {
        request = request.header(IF_NONE_MATCH, etag);
    }
    if let Some(body) = &job.body {
        request = request
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(body.clone());
    }
    let mut meter = AttemptMeter::new(job);
    let response = request.send().await.map_err(|error| {
        meter.record(None);
        if error.is_timeout() {
            GitHubCommandError::TimedOut(REQUEST_TIMEOUT.as_secs())
        } else {
            GitHubCommandError::Invalid(format!("GitHub HTTP request failed: {error}"))
        }
    })?;
    let status = response.status();
    meter.record(Some(status));
    let etag = response
        .headers()
        .get(ETAG)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let next_link = response
        .headers()
        .get(LINK)
        .and_then(|value| value.to_str().ok())
        .and_then(parse_next_link);
    let body = response
        .bytes()
        .await
        .map_err(|error| {
            GitHubCommandError::Invalid(format!("GitHub HTTP body read failed: {error}"))
        })?
        .to_vec();
    Ok(HttpOutcome {
        status,
        etag,
        next_link,
        body,
    })
}

fn cached_token(caller: &'static Location<'static>) -> Option<String> {
    let mut state = TOKEN
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    match &*state {
        TokenState::Available(token) => return Some(token.clone()),
        TokenState::Unavailable(since) if since.elapsed() < UNAVAILABLE_RETRY => return None,
        TokenState::Unknown | TokenState::Unavailable(_) => {}
    }
    match read_gh_token(caller) {
        Some(token) => {
            *state = TokenState::Available(token.clone());
            Some(token)
        }
        None => {
            *state = TokenState::Unavailable(Instant::now());
            None
        }
    }
}

fn refresh_token(caller: &'static Location<'static>, stale: &str) -> Option<String> {
    let mut state = TOKEN
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let TokenState::Available(current) = &*state
        && current != stale
    {
        return Some(current.clone());
    }
    match read_gh_token(caller) {
        Some(token) if token != stale => {
            *state = TokenState::Available(token.clone());
            Some(token)
        }
        Some(token) => {
            *state = TokenState::Available(token);
            None
        }
        None => {
            *state = TokenState::Unavailable(Instant::now());
            None
        }
    }
}

fn read_gh_token(caller: &'static Location<'static>) -> Option<String> {
    crate::request_counter::record(caller, "auth token".into());
    let mut command = crate::command::gh_command();
    command
        .args(["auth", "token", "--hostname", "github.com"])
        .env("GH_PROMPT_DISABLED", "1")
        .env("NO_COLOR", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = command.spawn().ok()?;
    let stdout = child.stdout.take();
    let reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        if let Some(mut pipe) = stdout {
            let _ = pipe.read_to_end(&mut bytes);
        }
        bytes
    });
    let started = Instant::now();
    let status = loop {
        if let Ok(Some(status)) = child.try_wait() {
            break status;
        }
        if started.elapsed() > TOKEN_DEADLINE {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    if !status.success() {
        return None;
    }
    let output = reader.join().ok()?;
    let token = String::from_utf8(output).ok()?.trim().to_owned();
    (!token.is_empty()).then_some(token)
}

fn cached_etag(key: &(String, String)) -> Option<String> {
    let mut cache = ETAG_CACHE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let entry = cache.get_mut(key)?;
    entry.last_used = ETAG_SEQ.fetch_add(1, Ordering::Relaxed);
    Some(entry.etag.clone())
}

fn cached_body(key: &(String, String)) -> Option<Vec<u8>> {
    let mut cache = ETAG_CACHE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let entry = cache.get_mut(key)?;
    entry.last_used = ETAG_SEQ.fetch_add(1, Ordering::Relaxed);
    Some(entry.body.clone())
}

fn store_etag(key: (String, String), etag: String, body: Vec<u8>) {
    let mut cache = ETAG_CACHE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if cache.len() >= MAX_ETAG_ENTRIES && !cache.contains_key(&key) {
        evict_oldest(&mut cache);
    }
    cache.insert(
        key,
        CachedEtag {
            etag,
            body,
            last_used: ETAG_SEQ.fetch_add(1, Ordering::Relaxed),
        },
    );
}

fn evict_oldest(cache: &mut HashMap<(String, String), CachedEtag>) {
    let mut entries: Vec<_> = cache
        .iter()
        .map(|(key, entry)| (entry.last_used, key.clone()))
        .collect();
    entries.sort_unstable_by_key(|(seq, _)| *seq);
    for (_, key) in entries.into_iter().take(cache.len() / 2 + 1) {
        cache.remove(&key);
    }
}

fn api_url(endpoint: &str) -> String {
    format!("{API_BASE}/{}", endpoint.trim_start_matches('/'))
}

fn parse_next_link(header: &str) -> Option<String> {
    header.split(',').find_map(|part| {
        let mut sections = part.split(';');
        let url = sections
            .next()?
            .trim()
            .strip_prefix('<')?
            .strip_suffix('>')?;
        let is_next = sections.any(|section| section.trim() == "rel=\"next\"");
        is_next.then(|| url.to_owned())
    })
}

fn http_status_error(status: StatusCode, body: &[u8]) -> GitHubCommandError {
    let detail = serde_json::from_slice::<serde_json::Value>(body)
        .ok()
        .and_then(|value| {
            value
                .get("message")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
        })
        .unwrap_or_else(|| {
            let text = String::from_utf8_lossy(body).trim().to_owned();
            if text.is_empty() {
                status.canonical_reason().unwrap_or("Error").to_owned()
            } else {
                text
            }
        });
    GitHubCommandError::Command {
        code: Some(i32::from(status.as_u16())),
        message: format!("HTTP {}: {detail}", status.as_u16()),
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::{Shutdown, TcpListener};
    use std::panic::Location;
    use std::time::Duration;

    use reqwest::{Method, StatusCode};

    use super::{DEFAULT_ACCEPT, HttpJob, HttpRequest, dispatch_with_token_refresh, spawn_worker};
    use crate::gh_command::GitHubCommandError;

    enum ScriptedReply {
        Raw(&'static [u8]),
        Stall(Duration),
    }

    fn read_request_headers(stream: &mut std::net::TcpStream) {
        let _ = stream.set_read_timeout(Some(Duration::from_millis(500)));
        let mut received = Vec::new();
        let mut chunk = [0u8; 1024];
        while !received.windows(4).any(|window| window == b"\r\n\r\n") {
            match stream.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(count) => received.extend_from_slice(&chunk[..count]),
            }
        }
    }

    fn spawn_scripted_server(
        replies: Vec<ScriptedReply>,
    ) -> Result<(String, std::thread::JoinHandle<()>), std::io::Error> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let address = listener.local_addr()?;
        let handle = std::thread::spawn(move || {
            for reply in replies {
                let Ok((mut stream, _)) = listener.accept() else {
                    break;
                };
                read_request_headers(&mut stream);
                match reply {
                    ScriptedReply::Raw(bytes) => {
                        let _ = stream.write_all(bytes);
                        let _ = stream.flush();
                        let _ = stream.shutdown(Shutdown::Both);
                    }
                    ScriptedReply::Stall(delay) => {
                        std::thread::spawn(move || {
                            std::thread::sleep(delay);
                            drop(stream);
                        });
                    }
                }
            }
        });
        Ok((format!("http://{address}/repos/test/repo"), handle))
    }

    fn spawn_test_worker(
        timeout: Duration,
    ) -> Result<tokio::sync::mpsc::UnboundedSender<HttpJob>, Box<dyn std::error::Error>> {
        let client = reqwest::Client::builder()
            .timeout(timeout)
            .no_proxy()
            .build()?;
        spawn_worker(client).ok_or_else(|| "failed to start test HTTP worker".into())
    }

    fn project_rate_row(project: &str) -> Option<velocity_core::performance::GhRateRow> {
        let normalized = project.to_ascii_lowercase();
        velocity_core::performance::snapshot()
            .gh_projects
            .into_iter()
            .find(|row| row.project == normalized)
    }

    fn operation_count(operation: &str) -> u64 {
        crate::request_counter::request_totals()
            .into_iter()
            .filter(|row| row.operation == operation)
            .map(|row| row.count)
            .sum()
    }

    // Risk (#695, SPEC.md §3.1/F15): pooled HTTPS attempts that fail after dispatch (body read
    // error, timeout, or failed auth retry) must still be counted once against the project's
    // per-minute rate and totals.
    #[test]
    fn counts_body_read_errors_timeouts_and_failed_auth_retries_once()
    -> Result<(), Box<dyn std::error::Error>> {
        let truncated = b"HTTP/1.1 200 OK\r\nContent-Length: 64\r\nConnection: close\r\n\r\nshort";
        let unauthorized =
            b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}";
        let (url, server) = spawn_scripted_server(vec![
            ScriptedReply::Raw(truncated),
            ScriptedReply::Stall(Duration::from_millis(300)),
            ScriptedReply::Raw(unauthorized),
            ScriptedReply::Raw(truncated),
        ])?;
        let worker = spawn_test_worker(Duration::from_millis(80))?;
        let project = "issue-695/pooled-http-failures";
        let request = |operation| HttpRequest {
            project,
            caller: Location::caller(),
            method: &Method::GET,
            url: &url,
            accept: DEFAULT_ACCEPT,
            etag: None,
            body: None,
            operation,
        };

        let body_err =
            dispatch_with_token_refresh(&worker, "tok", &request("issue-695-body"), |_| None);
        assert!(matches!(
            body_err,
            Err(GitHubCommandError::Invalid(ref msg)) if msg.contains("GitHub HTTP body read failed")
        ));
        let timeout_err =
            dispatch_with_token_refresh(&worker, "tok", &request("issue-695-timeout"), |_| None);
        assert!(matches!(timeout_err, Err(GitHubCommandError::TimedOut(_))));
        let retry_err =
            dispatch_with_token_refresh(&worker, "stale", &request("issue-695-retry-err"), |_| {
                Some("fresh".into())
            });
        assert!(matches!(
            retry_err,
            Err(GitHubCommandError::Invalid(ref msg)) if msg.contains("GitHub HTTP body read failed")
        ));

        let _ = server.join();
        assert_eq!(operation_count("issue-695-body"), 1);
        assert_eq!(operation_count("issue-695-timeout"), 1);
        assert_eq!(operation_count("issue-695-retry-err"), 2);
        let row = project_rate_row(project).ok_or("missing project rate row")?;
        assert_eq!(row.total_calls, 4);
        assert_eq!(row.calls_last_minute, 4);
        Ok(())
    }

    // Risk (#695, SPEC.md §3.1/F15): conditional `304` responses must keep their `(304)` label
    // without counting completion as a second attempt.
    #[test]
    fn keeps_304_detail_without_double_counting() -> Result<(), Box<dyn std::error::Error>> {
        let not_modified =
            b"HTTP/1.1 304 Not Modified\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
        let unauthorized =
            b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}";
        let (url, server) = spawn_scripted_server(vec![
            ScriptedReply::Raw(not_modified),
            ScriptedReply::Raw(unauthorized),
            ScriptedReply::Raw(not_modified),
        ])?;
        let worker = spawn_test_worker(Duration::from_secs(2))?;
        let project = "issue-695/not-modified";
        let make_request = |operation| HttpRequest {
            project,
            caller: Location::caller(),
            method: &Method::GET,
            url: &url,
            accept: DEFAULT_ACCEPT,
            etag: Some("\"v1\""),
            body: None,
            operation,
        };

        let first =
            dispatch_with_token_refresh(&worker, "tok", &make_request("issue-695-304"), |_| None)?;
        assert_eq!(first.status, StatusCode::NOT_MODIFIED);
        let retried = dispatch_with_token_refresh(
            &worker,
            "stale",
            &make_request("issue-695-auth-304"),
            |_| Some("fresh".into()),
        )?;
        assert_eq!(retried.status, StatusCode::NOT_MODIFIED);
        let _ = server.join();

        assert_eq!(operation_count("issue-695-304 (304)"), 1);
        assert_eq!(operation_count("issue-695-304"), 0);
        assert_eq!(operation_count("issue-695-auth-304"), 1);
        assert_eq!(operation_count("issue-695-auth-304 (304)"), 1);
        let row = project_rate_row(project).ok_or("missing project rate row")?;
        assert_eq!(row.total_calls, 3);
        Ok(())
    }

    // Risk (#695, SPEC.md §3.1/F15): a failed worker queue send before dispatch must add zero
    // GitHub requests to the counter or per-project rate tracker.
    #[test]
    fn skips_counting_when_worker_queue_send_fails() {
        let (stopped_tx, stopped_rx) = tokio::sync::mpsc::unbounded_channel::<HttpJob>();
        drop(stopped_rx);
        let project = "issue-695/stopped-worker";
        let request = HttpRequest {
            project,
            caller: Location::caller(),
            method: &Method::GET,
            url: "http://127.0.0.1:1/repos/test/repo",
            accept: DEFAULT_ACCEPT,
            etag: None,
            body: None,
            operation: "issue-695-stopped",
        };
        let stopped = dispatch_with_token_refresh(&stopped_tx, "tok", &request, |_| None);
        assert!(matches!(
            stopped,
            Err(GitHubCommandError::Invalid(ref msg)) if msg == "GitHub HTTP worker stopped"
        ));
        assert_eq!(operation_count("issue-695-stopped"), 0);
        assert!(project_rate_row(project).is_none());
    }
}
