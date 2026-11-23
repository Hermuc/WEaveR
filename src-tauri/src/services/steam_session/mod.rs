//! Steam session lifecycle: token parsing, refresh and verification.
//!
//! This module owns the *token* half of "stay signed in". The WebView/cookie
//! half stays in `workshop::webview`; commands and startup glue call both.
//!
//! The design mirrors Watt Toolkit's `AuthenticatorSavedSteamAccounts` /
//! `SteamAuthenticatorServiceImpl` split:
//!   * [token] — pure JWT claims parsing and expiry checks;
//!   * [refresh] — refresh-token -> access-token exchange (CM first, HTTP fallback);
//!   * [local_source] — optional reuse of the Steam client's cached refresh token;
//!   * [verify] — "is this cookie actually accepted by Steam?" probe.

pub mod local_source;
pub mod refresh;
pub mod token;
pub mod verify;

use std::sync::Arc;

use serde::Serialize;

use crate::accounts::{AccountManager, StoredSteamSession};
use token::TokenKind;

/// Three-state session verdict. `Unknown` exists because a failed network call
/// is not evidence that the user signed out; treating it as such is what made
/// the old implementation clear a perfectly good session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum SessionState {
    SignedIn,
    SignedOut,
    Unknown,
}

/// How long a failed refresh suppresses further automatic attempts for the
/// same account. A rejected refresh token does not heal by itself; retrying it
/// on every probe only burns 10-20 seconds per attempt (CM logon + HTTP
/// fallback) and makes startup feel stalled. Manual actions use
/// `force_refresh` and bypass this.
const REFRESH_FAILURE_COOLDOWN: std::time::Duration = std::time::Duration::from_secs(300);

struct RefreshFailure {
    account: String,
    at: std::time::Instant,
    message: String,
}

pub struct SteamSessionService {
    accounts: Arc<AccountManager>,
    /// Serialises refresh attempts. Two startup paths (the background
    /// `init_cookies` thread and the frontend's restore call) can race; without
    /// this they would each exchange the same refresh token and one would win
    /// with a token the other already replaced.
    refresh_lock: tokio::sync::Mutex<()>,
    verify_client: reqwest::Client,
    last_refresh_failure: std::sync::Mutex<Option<RefreshFailure>>,
}

impl SteamSessionService {
    pub fn new(accounts: Arc<AccountManager>) -> Self {
        Self {
            accounts,
            refresh_lock: tokio::sync::Mutex::new(()),
            verify_client: verify::build_client(),
            last_refresh_failure: std::sync::Mutex::new(None),
        }
    }

    /// The stored session for an account, if any.
    pub fn stored(&self, account: &str) -> Option<StoredSteamSession> {
        self.accounts.session_for(account)
    }

    /// Persist the tokens that a successful sign-in produced.
    pub fn save_login(
        &self,
        account: &str,
        tokens: &steamroom::auth::AuthTokens,
    ) -> Result<StoredSteamSession, String> {
        self.save_tokens(account, &tokens.access_token, &tokens.refresh_token)
    }

    /// Persist a raw access/refresh token pair (used by the QR path, which does
    /// not have an `AuthTokens` value to hand over).
    pub fn save_tokens(
        &self,
        account: &str,
        access_token: &str,
        refresh_token: &str,
    ) -> Result<StoredSteamSession, String> {
        let steam_id = token::parse_claims(access_token)
            .and_then(|c| c.sub)
            .or_else(|| token::parse_claims(refresh_token).and_then(|c| c.sub))
            .ok_or_else(|| "Steam token has no SteamID claim".to_string())?;
        let session = StoredSteamSession {
            steam_id,
            access_token: access_token.to_string(),
            refresh_token: refresh_token.to_string(),
            saved_at: token::unix_now() as u64,
        };
        self.accounts.set_session(account, session.clone())?;
        log::info!(
            "[steam-session] saved login for {account} access_aud={} refresh_aud={}",
            token::describe_aud(&session.access_token),
            token::describe_aud(&session.refresh_token)
        );
        Ok(session)
    }

    /// Return a usable access token for the account, refreshing it when needed.
    ///
    /// `Ok(None)` means "no session material at all" — the caller should ask
    /// the user to sign in. A network failure during refresh is also reported as
    /// `Ok(None)` but logged; verification later turns it into `Unknown`.
    pub async fn ensure_access_token(&self, account: &str) -> Result<Option<String>, String> {
        self.ensure_access_token_inner(account, false).await
    }

    /// Refresh even when the current access token still looks valid.
    ///
    /// Steam can revoke an access token server-side while its JWT `exp` is still
    /// in the future (password change, "sign out everywhere", …). The only way
    /// to notice is a failed verification, so the restore path calls this once
    /// before giving up.
    pub async fn force_refresh(&self, account: &str) -> Result<Option<String>, String> {
        self.ensure_access_token_inner(account, true).await
    }

    async fn ensure_access_token_inner(
        &self,
        account: &str,
        force: bool,
    ) -> Result<Option<String>, String> {
        let _guard = self.refresh_lock.lock().await;

        let Some(mut session) = self.accounts.session_for(account) else {
            return self.try_local_token(account).await;
        };
        if session.steam_id.is_empty() {
            return Ok(None);
        }

        if !force && token::is_usable(&session.access_token, &session.steam_id, TokenKind::Access) {
            log::info!(
                "[steam-session] access token usable for {account} (aud={})",
                token::describe_aud(&session.access_token)
            );
            return Ok(Some(session.access_token));
        }
        log::info!(
            "[steam-session] access token expired/missing for {account} (aud={})",
            token::describe_aud(&session.access_token)
        );

        if !force {
            let guard = self.last_refresh_failure.lock().unwrap();
            if cooldown_active(guard.as_ref(), account, std::time::Instant::now()) {
                if let Some(failure) = guard.as_ref() {
                    log::info!(
                        "[steam-session] refresh cooldown for {account} (last: {})",
                        failure.message
                    );
                }
                return Ok(None);
            }
        }

        let mut refresh_token = session.refresh_token.clone();
        if !token::is_usable(&refresh_token, &session.steam_id, TokenKind::Refresh) {
            if let Some(local) = local_source::extract_refresh_token(account) {
                log::info!(
                    "[steam-session] falling back to local Steam refresh token (aud={})",
                    token::describe_aud(&local)
                );
                refresh_token = local;
            }
        }
        if !token::is_usable(&refresh_token, &session.steam_id, TokenKind::Refresh) {
            log::warn!("[steam-session] no usable refresh token for {account}");
            return Ok(None);
        }

        match refresh_with_timeout(account, &session.steam_id, &refresh_token).await {
            Ok(Some(pair)) => {
                *self.last_refresh_failure.lock().unwrap() = None;
                session.access_token = pair.access_token.clone();
                if let Some(new_refresh) = pair.refresh_token {
                    session.refresh_token = new_refresh;
                }
                session.saved_at = token::unix_now() as u64;
                self.accounts.set_session(account, session)?;
                log::info!(
                    "[steam-session] refreshed access token for {account} (aud={})",
                    token::describe_aud(&pair.access_token)
                );
                Ok(Some(pair.access_token))
            }
            Ok(None) => Ok(None),
            Err(e) => {
                log::warn!("[steam-session] refresh failed for {account}: {e}");
                *self.last_refresh_failure.lock().unwrap() = Some(RefreshFailure {
                    account: account.to_string(),
                    at: std::time::Instant::now(),
                    message: e.clone(),
                });
                Ok(None)
            }
        }
    }

    /// Last-resort path: use the Steam client's cached refresh token when no
    /// session has ever been stored for this account.
    async fn try_local_token(&self, account: &str) -> Result<Option<String>, String> {
        let Some(local) = local_source::extract_refresh_token(account) else {
            return Ok(None);
        };
        let Some(steam_id) = token::parse_claims(&local).and_then(|c| c.sub) else {
            return Ok(None);
        };
        if !token::is_usable(&local, &steam_id, TokenKind::Refresh) {
            return Ok(None);
        }
        let Some(pair) = refresh_with_timeout(account, &steam_id, &local).await? else {
            return Ok(None);
        };
        let session = StoredSteamSession {
            steam_id,
            access_token: pair.access_token.clone(),
            refresh_token: local,
            saved_at: token::unix_now() as u64,
        };
        self.accounts.set_session(account, session)?;
        Ok(Some(pair.access_token))
    }

    /// Ask Steam whether the reconstructed session is really accepted.
    ///
    /// steamcommunity.com is the primary signal (it is what the WebView uses),
    /// but it can be blocked while api.steampowered.com is reachable. When the
    /// community probe cannot answer, fall back to exchanging the stored
    /// refresh token through the Web API: a successful exchange means the
    /// session is alive, an explicit rejection means it is not.
    pub async fn verify(
        &self,
        steam_id: &str,
        access_token: &str,
        refresh_token: Option<&str>,
    ) -> SessionState {
        match verify::verify_session(&self.verify_client, steam_id, access_token).await {
            verify::VerifyResult::SignedIn => return SessionState::SignedIn,
            verify::VerifyResult::SignedOut => return SessionState::SignedOut,
            verify::VerifyResult::Unknown => {}
        }
        let Some(refresh_token) = refresh_token else {
            return SessionState::Unknown;
        };
        match refresh::verify_refresh_token(steam_id, refresh_token).await {
            refresh::RefreshVerify::Valid => {
                log::info!("[steam-session] verified via api.steampowered.com");
                SessionState::SignedIn
            }
            refresh::RefreshVerify::Rejected => {
                log::info!("[steam-session] refresh token rejected by api.steampowered.com");
                SessionState::SignedOut
            }
            refresh::RefreshVerify::Unavailable => SessionState::Unknown,
        }
    }
}

/// Whether a recorded refresh failure still suppresses automatic retries.
///
/// Pure so the cooldown policy is testable without a clock or a network.
fn cooldown_active(
    failure: Option<&RefreshFailure>,
    account: &str,
    now: std::time::Instant,
) -> bool {
    match failure {
        Some(f) => {
            f.account == account
                && now.duration_since(f.at) < REFRESH_FAILURE_COOLDOWN
        }
        None => false,
    }
}

/// Run a refresh with a hard deadline.
///
/// A CM logon can take tens of seconds when the network is unhealthy; startup
/// must not be held hostage by it. The timeout covers the whole exchange.
async fn refresh_with_timeout(
    account: &str,
    steam_id: &str,
    refresh_token: &str,
) -> Result<Option<refresh::TokenPair>, String> {
    match tokio::time::timeout(
        std::time::Duration::from_secs(60),
        refresh::refresh_access_token(account, steam_id, refresh_token, false),
    )
    .await
    {
        Ok(Ok(pair)) => Ok(Some(pair)),
        Ok(Err(e)) => Err(e),
        Err(_) => Err("refresh timed out after 60s".to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refresh_cooldown_blocks_only_the_same_account_and_only_for_five_minutes() {
        let now = std::time::Instant::now();
        let recent = RefreshFailure {
            account: "a".into(),
            at: now - std::time::Duration::from_secs(10),
            message: "x".into(),
        };
        assert!(cooldown_active(Some(&recent), "a", now));
        assert!(!cooldown_active(Some(&recent), "b", now));

        let old = RefreshFailure {
            account: "a".into(),
            at: now - std::time::Duration::from_secs(600),
            message: "x".into(),
        };
        assert!(!cooldown_active(Some(&old), "a", now));
        assert!(!cooldown_active(None, "a", now));
    }

    #[test]
    fn session_state_serializes_with_a_state_tag() {
        assert_eq!(
            serde_json::to_string(&SessionState::SignedIn).unwrap(),
            r#"{"state":"signed_in"}"#
        );
        assert_eq!(
            serde_json::to_string(&SessionState::SignedOut).unwrap(),
            r#"{"state":"signed_out"}"#
        );
        assert_eq!(
            serde_json::to_string(&SessionState::Unknown).unwrap(),
            r#"{"state":"unknown"}"#
        );
    }
}