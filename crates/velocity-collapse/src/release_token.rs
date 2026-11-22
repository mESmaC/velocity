//! Creating and rotating Collapse `releases`-scoped tokens from the signed-in desktop
//! session (Collapse C1, #607).
//!
//! When no release token is saved in Windows Credential Manager, Velocity mints a
//! `releases`-scoped personal access token named `velocity-releases-<machine>` using the
//! desktop sign-in `access_token`, stores it under `velocity:collapse.sh:releases`, and
//! replaces it at most once on HTTP 401 or 403. A pasted token always wins.

use std::future::Future;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use reqwest::{Client, Method, StatusCode};
use serde::{Deserialize, Serialize};

use crate::CollapseError;
use crate::connect::{current_machine_name, normalize_machine_name};
use crate::credential::{
    CollapseCredentials, ReleaseTokenSource, StoredReleaseToken, load_credentials,
    load_release_token_record, save_sign_in_release_token,
};
use crate::provision::{authorized, failed};
use crate::releases::extract_error_message;

/// Diagnostic message logged whenever Velocity creates a Collapse release token from sign-in.
pub const CREATED_RELEASE_TOKEN_LOG: &str =
    "Created a Collapse release token from the signed-in session";

const PERSONAL_TOKENS_PATH: &str = "/api/tokens/personal";
const RELEASES_SCOPE: &str = "releases";
const TOKEN_NAME_PREFIX: &str = "velocity-releases-";
const MINT_TIMEOUT: Duration = Duration::from_secs(30);

/// Result of resolving or minting the Collapse release token for a publish run (#607).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedReleaseToken {
    pub record: StoredReleaseToken,
    /// True when Velocity just created and saved this token from the signed-in session.
    pub created_from_session: bool,
}

#[derive(Serialize)]
struct CreatePersonalTokenBody<'request> {
    name: &'request str,
    scopes: [&'static str; 1],
}

#[derive(Deserialize)]
struct MintedPersonalTokenReply {
    secret: String,
    #[serde(default)]
    token: Option<MintedTokenMeta>,
}

#[derive(Deserialize)]
struct MintedTokenMeta {
    #[serde(default)]
    created_at: Option<String>,
}

/// Token name Collapse expects from a desktop sign-in (`velocity-releases-<machine>`).
pub fn release_token_name(credentials: &CollapseCredentials) -> String {
    let machine = credentials
        .machine
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(normalize_machine_name)
        .unwrap_or_else(current_machine_name);
    format!("{TOKEN_NAME_PREFIX}{machine}")
}

/// Calls Collapse's C1 token API (`POST /api/tokens/personal`) with the desktop session's
/// `access_token` to mint a `releases`-scoped token for this machine, atomically revoking
/// any prior `velocity-releases-<machine>` token for the account.
pub async fn mint_release_token(
    credentials: &CollapseCredentials,
) -> Result<StoredReleaseToken, CollapseError> {
    let name = release_token_name(credentials);
    let client = Client::builder()
        .timeout(MINT_TIMEOUT)
        .user_agent("velocity-collapse")
        .build()
        .map_err(failed)?;
    let response = authorized(
        &client,
        Method::POST,
        PERSONAL_TOKENS_PATH,
        &credentials.access_token,
    )
    .json(&CreatePersonalTokenBody {
        name: &name,
        scopes: [RELEASES_SCOPE],
    })
    .send()
    .await
    .map_err(failed)?;
    parse_mint_response(response).await
}

async fn parse_mint_response(
    response: reqwest::Response,
) -> Result<StoredReleaseToken, CollapseError> {
    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        let detail = extract_error_message(&body);
        if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
            return Err(CollapseError::Api(format!(
                "Collapse refused to create a release token from the signed-in session ({status}{detail}). Reconnect Collapse or save a personal access token with the `releases` scope in Settings → Collapse → Release credentials."
            )));
        }
        return Err(CollapseError::Api(format!(
            "Could not create a Collapse release token from the signed-in session: {status}{detail}"
        )));
    }
    let reply: MintedPersonalTokenReply = response.json().await.map_err(failed)?;
    let secret = reply.secret.trim();
    if secret.is_empty() {
        return Err(CollapseError::Api(
            "Collapse returned an empty release token".into(),
        ));
    }
    let created_at = reply
        .token
        .and_then(|meta| meta.created_at)
        .map(|text| text.trim().to_owned())
        .filter(|text| !text.is_empty())
        .unwrap_or_else(current_utc_rfc3339);
    Ok(StoredReleaseToken {
        token: secret.to_owned(),
        source: ReleaseTokenSource::SignIn,
        created_at: Some(created_at),
    })
}

/// Mints a fresh `releases`-scoped token from the signed-in Collapse session and stores it
/// in Windows Credential Manager (`velocity:collapse.sh:releases`) (#607).
pub async fn create_and_save_release_token() -> Result<StoredReleaseToken, CollapseError> {
    let credentials = tokio::task::spawn_blocking(load_credentials)
        .await
        .map_err(|error| CollapseError::Credential(error.to_string()))??
        .ok_or_else(|| {
            CollapseError::Api(
                "No signed-in Collapse session is available; connect Collapse or save a personal access token with the `releases` scope in Settings → Collapse → Release credentials."
                    .into(),
            )
        })?;
    let minted = mint_release_token(&credentials).await?;
    tokio::task::spawn_blocking(move || {
        save_sign_in_release_token(&minted.token, minted.created_at.as_deref())
    })
    .await
    .map_err(|error| CollapseError::Credential(error.to_string()))?
}

/// Returns the saved Collapse release token if one exists (pasted tokens always win), or
/// creates and saves one from the signed-in Collapse session when none is saved (#607).
pub async fn ensure_release_token() -> Result<ResolvedReleaseToken, CollapseError> {
    if let Some(record) = tokio::task::spawn_blocking(load_release_token_record)
        .await
        .map_err(|error| CollapseError::Credential(error.to_string()))??
    {
        return Ok(ResolvedReleaseToken {
            record,
            created_from_session: false,
        });
    }
    let Some(credentials) = tokio::task::spawn_blocking(load_credentials)
        .await
        .map_err(|error| CollapseError::Credential(error.to_string()))??
    else {
        return Err(CollapseError::Api(
            "No Collapse release token is available; connect Collapse or save one in Settings → Collapse → Release credentials."
                .into(),
        ));
    };
    let minted = mint_release_token(&credentials).await?;
    let saved = tokio::task::spawn_blocking(move || {
        save_sign_in_release_token(&minted.token, minted.created_at.as_deref())
    })
    .await
    .map_err(|error| CollapseError::Credential(error.to_string()))??;
    Ok(ResolvedReleaseToken {
        record: saved,
        created_from_session: true,
    })
}

/// Tracks the active Collapse release token during a publish run and replaces a
/// sign-in-minted token at most once when Collapse answers 401 or 403 (#607).
pub struct ReleaseTokenSession {
    record: StoredReleaseToken,
    replaced_once: bool,
}

impl ReleaseTokenSession {
    pub fn new(record: StoredReleaseToken) -> Self {
        Self {
            record,
            replaced_once: false,
        }
    }

    pub fn token(&self) -> &str {
        &self.record.token
    }

    /// Runs `operation` with the current token. On HTTP 401 or 403 for a sign-in token that
    /// has not yet been replaced in this session, creates a replacement token once, invokes
    /// `on_created`, and retries `operation`. Pasted tokens are never overwritten.
    pub async fn run<T, F, Fut>(
        &mut self,
        on_created: impl FnMut(),
        operation: F,
    ) -> Result<T, CollapseError>
    where
        F: FnMut(String) -> Fut,
        Fut: Future<Output = Result<T, CollapseError>>,
    {
        self.run_with_replacer(create_and_save_release_token, on_created, operation)
            .await
    }

    async fn run_with_replacer<T, R, RFut, F, Fut>(
        &mut self,
        replace: R,
        mut on_created: impl FnMut(),
        mut operation: F,
    ) -> Result<T, CollapseError>
    where
        R: FnOnce() -> RFut,
        RFut: Future<Output = Result<StoredReleaseToken, CollapseError>>,
        F: FnMut(String) -> Fut,
        Fut: Future<Output = Result<T, CollapseError>>,
    {
        match operation(self.record.token.clone()).await {
            Ok(value) => Ok(value),
            Err(error)
                if error.is_release_unauthorized()
                    && self.record.source == ReleaseTokenSource::SignIn
                    && !self.replaced_once =>
            {
                self.replaced_once = true;
                let replacement = replace().await?;
                self.record = replacement;
                on_created();
                operation(self.record.token.clone()).await
            }
            Err(error) => Err(error),
        }
    }
}

fn current_utc_rfc3339() -> String {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs());
    let days = (seconds / 86_400) as i64;
    let time = seconds % 86_400;
    let (year, month, day) = civil_from_days(days);
    let hour = time / 3_600;
    let minute = (time % 3_600) / 60;
    let second = time % 60;
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

/// Howard Hinnant's Euclidean civil-from-days algorithm (days since 1970-01-01).
fn civil_from_days(days_since_epoch: i64) -> (i64, u32, u32) {
    let z = days_since_epoch + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = y + i64::from(month <= 2);
    (year, month as u32, day as u32)
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    // Risk: a revoked sign-in token must be replaced at most once on 401/403, and a pasted
    // token must never be overwritten automatically (#607).
    #[test]
    fn sign_in_token_replaces_once_and_pasted_token_never_replaces()
    -> Result<(), Box<dyn std::error::Error>> {
        let runtime = tokio::runtime::Builder::new_current_thread().build()?;
        runtime.block_on(async {
            let mut session = ReleaseTokenSession::new(StoredReleaseToken {
                token: "clp_pat_stale".into(),
                source: ReleaseTokenSource::SignIn,
                created_at: Some("2026-11-20T00:00:00Z".into()),
            });
            let created_logs = AtomicUsize::new(0);
            let value = session
                .run_with_replacer(
                    || async {
                        Ok(StoredReleaseToken {
                            token: "clp_pat_fresh".into(),
                            source: ReleaseTokenSource::SignIn,
                            created_at: Some("2026-11-22T00:00:00Z".into()),
                        })
                    },
                    || {
                        created_logs.fetch_add(1, Ordering::SeqCst);
                    },
                    |token| async move {
                        if token == "clp_pat_stale" {
                            Err(CollapseError::ReleaseUnauthorized("401".into()))
                        } else {
                            Ok(token)
                        }
                    },
                )
                .await?;
            assert_eq!(value, "clp_pat_fresh");
            assert_eq!(created_logs.load(Ordering::SeqCst), 1);

            // A second 401/403 in the same publish run must NOT replace again.
            let second_replace = AtomicUsize::new(0);
            let second_result: Result<(), CollapseError> = session
                .run_with_replacer(
                    || {
                        second_replace.fetch_add(1, Ordering::SeqCst);
                        async {
                            Ok(StoredReleaseToken {
                                token: "clp_pat_unused".into(),
                                source: ReleaseTokenSource::SignIn,
                                created_at: None,
                            })
                        }
                    },
                    || {},
                    |_| async { Err(CollapseError::ReleaseUnauthorized("403 hint".into())) },
                )
                .await;
            assert!(second_result.is_err());
            assert_eq!(second_replace.load(Ordering::SeqCst), 0);

            // A pasted token always wins and never triggers automatic replacement.
            let mut pasted = ReleaseTokenSession::new(StoredReleaseToken {
                token: "clp_pat_pasted".into(),
                source: ReleaseTokenSource::Pasted,
                created_at: None,
            });
            let pasted_replace = AtomicUsize::new(0);
            let pasted_result: Result<(), CollapseError> = pasted
                .run_with_replacer(
                    || {
                        pasted_replace.fetch_add(1, Ordering::SeqCst);
                        async {
                            Ok(StoredReleaseToken {
                                token: "clp_pat_should_not_replace".into(),
                                source: ReleaseTokenSource::SignIn,
                                created_at: None,
                            })
                        }
                    },
                    || {},
                    |_| async { Err(CollapseError::ReleaseUnauthorized("401 hint".into())) },
                )
                .await;
            assert!(pasted_result.is_err());
            assert_eq!(pasted_replace.load(Ordering::SeqCst), 0);
            assert_eq!(pasted.token(), "clp_pat_pasted");
            Ok(())
        })
    }

    #[test]
    fn release_token_name_uses_saved_machine_name() {
        let credentials = CollapseCredentials {
            account: "mESmaC".into(),
            mcp_token: "clp_pat_m".into(),
            access_token: "clp_desktop_a".into(),
            machine: Some("Office-PC".into()),
        };
        assert_eq!(
            release_token_name(&credentials),
            "velocity-releases-Office-PC"
        );
    }
}
