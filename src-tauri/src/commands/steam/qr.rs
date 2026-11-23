use tauri::Manager;

use crate::commands::AppStateHandle;
use crate::workshop::SteamAccount;

use tauri::Emitter;

use crate::services::steam_auth::jwt_subject;

/// Let the community page load before reading cookies back.
const COMMUNITY_NAV_SETTLE: std::time::Duration = std::time::Duration::from_millis(2000);
/// Give the webview a moment to apply freshly written cookies.
const COOKIE_APPLY_WAIT: std::time::Duration = std::time::Duration::from_millis(1200);
/// Let an injected eval run before reading its URL-fragment result.
const EVAL_SETTLE: std::time::Duration = std::time::Duration::from_millis(300);


/// Just opens the parser webview window without starting any login-polling loop.
/// Safe to call when already logged in — will not fire "steam-login-success".
#[tauri::command]
pub async fn steam_parser_show(state: AppStateHandle<'_>) -> Result<(), String> {
    state
        .steam_webview
        .show_login()
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn steam_sync_cookies(state: AppStateHandle<'_>) -> Result<usize, String> {
    let n = state.steam_webview.sync_cookies().await?;
    // Clear page cache so that subsequent requests go through with the new
    // cookies and don't return stale anonymous pages.
    state.workshop.clear_caches();
    Ok(n)
}

#[tauri::command]
pub async fn steam_is_logged_in(state: AppStateHandle<'_>) -> Result<bool, String> {
    // Same startup race as `steam_current_account`: `is_logged_in` reports
    // `false` when the window does not exist yet, which is exactly the state
    // during the first moments after launch. Ensure the profile is loaded
    // first so the answer reflects the persisted session, not the timing.
    state.steam_webview.ensure_profile_loaded().await;
    Ok(state.steam_webview.is_logged_in().await)
}

/// Ask Steam itself who the scraper is currently acting as. Returns `None`
/// if no valid Steam session is attached to the reqwest client — which is
/// also the hook the frontend uses to warn the user that the Workshop
/// parser is running anonymously. This is the check the user requested:
/// the account is resolved by hitting steamcommunity.com, not by reading
/// whatever is selected in the Settings dialog.
#[tauri::command]
pub async fn steam_current_account(
    state: AppStateHandle<'_>,
) -> Result<Option<SteamAccount>, String> {
    // Make sure the webview exists BEFORE syncing.
    //
    // `sync_cookies` returns `Ok(0)` and does nothing when the hidden window
    // has not been created yet — and at startup it is created on a background
    // thread by `init_cookies`. The frontend's bootstrap probe fires at the
    // same moment, so whichever won the race decided whether the user looked
    // signed in. Waiting here (idempotent, and free once the window exists)
    // makes the result deterministic instead of a coin flip.
    state.steam_webview.ensure_profile_loaded().await;
    // Make sure whatever cookies are in the webview profile are
    // reflected in the reqwest jar before we query Steam.
    let _ = state.steam_webview.sync_cookies().await;
    Ok(state.workshop.current_account().await)
}

/// Attempt to sign the hidden webview into Steam.
///
/// When `account_index` is supplied that account is used; otherwise the first
/// account the user added is used (there is no bundled fallback). Returns true
/// on success. Safe to call repeatedly — no-ops if already logged in. When
/// `force` is true the current session is terminated first and a fresh login
/// is always attempted.
/// Sign in using the credentials already stored for the given account.
///
/// This is the shared *service-level* entry point. Both the `steam_auto_login`
/// command and `accounts_set_custom` (when it needs to establish a browsing
/// session) go through here, so a command never has to call another
/// `#[tauri::command]` as an internal API.
pub(crate) async fn sign_in_with_saved_credentials(
    state: &AppStateHandle<'_>,
    account_index: Option<usize>,
    force: Option<bool>,
) -> Result<bool, String> {
    if !force.unwrap_or(false) {
        // When not forcing, check for an existing session — but verify it's
        // actually valid by hitting Steam's profile endpoint. A stale
        // `steamLoginSecure` cookie can survive for a long time after the
        // session has expired server-side, so we must not trust it blindly.
        if state.steam_webview.is_logged_in().await {
            let _ = state.steam_webview.sync_cookies().await;
            if state.workshop.current_account().await.is_some() {
                state.workshop.clear_caches();
                return Ok(true);
            }
            // Session cookie exists but is stale — fall through to re-login.
        }
    }
    // Account selection:
    //   Some(i) -> the account at that index in the user's account list
    //   None    -> the first account the user added (there is no bundled pool,
    //              so this is simply that account, or empty if none exists).
    let creds = match account_index {
        Some(i) => state.accounts.credentials(i),
        None => state.accounts.preferred_parser_credentials(),
    };
    if creds.username.is_empty() || creds.password.is_empty() {
        // Surfacing this as Ok(false) made the UI print "Unknown error".
        return Err("No Steam credentials available. Add your account in Settings -> Accounts                     (Add custom account) and try again."
            .to_string());
    }
    // A forced login destroys the current session before retrying. Only the
    // interactive "sign in again" path passes `force`, so refuse to do it when
    // a perfectly good session cookie is still present: wiping it would log the
    // user out for no reason, and that is precisely how a signed-in account used
    // to disappear on restart.
    let force = force.unwrap_or(false);
    let wants_forced_relogin = force && !creds.username.is_empty();
    if wants_forced_relogin && state.steam_webview.is_logged_in().await {
        let _ = state.steam_webview.sync_cookies().await;
        if state.workshop.current_account().await.is_some() {
            state.workshop.clear_caches();
            log::info!("[steam-session] refusing forced re-login: session is valid");
            return Ok(true);
        }
    }

    let ok = state
        .steam_webview
        .auto_login(&creds.username, &creds.password, force)
        .await?;
    if ok {
        let _ = state.steam_webview.sync_cookies().await;
        state.workshop.clear_caches();
        // A sign-in means "keep me signed in" again — re-enable auto-login that
        // a previous sign-out had turned off.
        super::mark_signed_in(state);
    }
    Ok(ok)
}

/// Command wrapper: sign in with the first saved account (or `account_index`).
#[tauri::command]
pub async fn steam_auto_login(
    state: AppStateHandle<'_>,
    account_index: Option<usize>,
    force: Option<bool>,
) -> Result<bool, String> {
    sign_in_with_saved_credentials(&state, account_index, force).await
}

#[tauri::command]
pub async fn steam_login_prepare(state: AppStateHandle<'_>) -> Result<(), String> {
    state
        .steam_webview
        .prepare_login()
        .await
        .map_err(|e| e.to_string())
}

#[derive(serde::Serialize)]
pub struct QrBeginResponse {
    pub client_id: String,
    pub challenge_url: String,
    pub request_id: String,
    /// Ready-to-use `data:image/png;base64,...` rendering of `challenge_url`.
    /// Generated locally so the dialog never depends on an external QR service
    /// (which the CSP blocks and which is often unreachable from CN).
    pub qr_data_url: String,
}

#[tauri::command]
pub async fn steam_qr_begin() -> Result<QrBeginResponse, String> {
    let client = reqwest::Client::new();
    let res = client
        .post("https://api.steampowered.com/IAuthenticationService/BeginAuthSessionViaQR/v1/")
        .form(&[("device_friendly_name", "WEave"), ("platform_type", "2")])
        .send()
        .await
        .map_err(|e| e.to_string())?;

    let json: serde_json::Value = res.json().await.map_err(|e| e.to_string())?;

    let response = json.get("response").ok_or("No response field")?;
    let client_id = response
        .get("client_id")
        .and_then(|v| v.as_str())
        .ok_or("No client_id")?
        .to_string();
    let challenge_url = response
        .get("challenge_url")
        .and_then(|v| v.as_str())
        .ok_or("No challenge_url")?
        .to_string();
    let request_id = response
        .get("request_id")
        .and_then(|v| v.as_str())
        .ok_or("No request_id")?
        .to_string();

    // Render the challenge locally. A failure here must not break the login
    // flow — the dialog can still fall back to the raw challenge URL.
    let qr_data_url = crate::qr::render_qr_data_url(&challenge_url).unwrap_or_default();

    Ok(QrBeginResponse {
        client_id,
        challenge_url,
        request_id,
        qr_data_url,
    })
}

#[tauri::command]
pub async fn steam_qr_poll(
    client_id: String,
    request_id: String,
) -> Result<serde_json::Value, String> {
    let client = reqwest::Client::new();
    let res = client
        .post("https://api.steampowered.com/IAuthenticationService/PollAuthSessionStatus/v1/")
        .form(&[("client_id", &client_id), ("request_id", &request_id)])
        .send()
        .await
        .map_err(|e| e.to_string())?;

    let json: serde_json::Value = res.json().await.map_err(|e| e.to_string())?;
    Ok(json)
}

#[tauri::command]
pub async fn steam_qr_login_finalize(
    state: AppStateHandle<'_>,
    access_token: String,
    refresh_token: String,
) -> Result<(), String> {
    // Same JWT decoding as the OAuth path; one implementation, not two.
    let steamid = jwt_subject(&access_token)
        .ok_or_else(|| "Invalid access token: no SteamID claim".to_string())?;
    let steam_login_secure =
        crate::accounts::StoredSteamSession::steam_login_secure(&steamid, &access_token);

    use rand::RngExt;
    let sessionid: String = rand::rng()
        .sample_iter(&rand::distr::Alphanumeric)
        .take(32)
        .map(char::from)
        .collect::<String>()
        .to_lowercase();

    let Some(w) = state
        .app_handle
        .get_webview_window(crate::workshop::webview::LABEL)
    else {
        return Err("No webview window".into());
    };

    // Write the cookies through WebView2’s own cookie store, not via
    // `document.cookie`.
    //
    // `steamLoginSecure` is HttpOnly — Steam sets it that way precisely so
    // scripts cannot read or forge it. A `document.cookie` assignment is
    // silently ignored for HttpOnly names, so the old code ran without error
    // and produced no session at all. `WebviewWindow::set_cookie` goes through
    // the platform cookie manager and can set HttpOnly values.
    //
    // Both domains matter: the community site serves the pages we scrape, while
    // the store domain is what the login flow itself uses.
    crate::workshop::webview::install_cookies_for_window(
        &w,
        &steam_login_secure,
        &sessionid,
    )?;

    w.eval("window.location.href = 'https://steamcommunity.com/'")
        .map_err(|e| e.to_string())?;

    // Let the new community page finish loading before reading cookies back;
    // the navigation above is asynchronous from the webview's point of view.
    tokio::time::sleep(COMMUNITY_NAV_SETTLE).await;

    let _ = state.steam_webview.sync_cookies().await;
    state.workshop.clear_caches();

    // Persist the session so the next launch can skip the whole QR dance.
    let account = state.accounts.preferred_parser_credentials().username;
    if !account.is_empty() {
        match state
            .steam_session
            .save_tokens(&account, &access_token, &refresh_token)
        {
            Ok(_) => log::info!("[steam-session] saved QR session for {account}"),
            Err(e) => log::warn!("[steam-session] could not save QR session: {e}"),
        }
    }

    super::mark_signed_in(&state);
    let _ = state.app_handle.emit("steam-login-success", ());

    Ok(())
}

/// Probe the current Steam session, distinguishing failure from signed-out.
///
/// Returns `{ state: "signed_in" | "signed_out" | "unknown", account?: {...} }`.
/// `unknown` means the HTTP call itself failed — the caller must not treat that
/// as "the user is signed out".
#[tauri::command]
pub async fn steam_probe_account(state: AppStateHandle<'_>) -> Result<serde_json::Value, String> {
    use crate::services::steam_session::SessionState;

    state.steam_webview.ensure_profile_loaded().await;

    // Prefer the session service's authoritative verification: it builds the
    // `steamLoginSecure` cookie itself and asks Steam directly. The old path
    // went through the reqwest cookie jar, which can lag behind a freshly
    // installed cookie and reported `signed_out` for a valid session — that
    // false negative is what made startup re-run `steam_restore_session`.
    let account_name = state.accounts.preferred_parser_credentials().username;
    if !account_name.is_empty() {
        if let Ok(Some(access)) = state.steam_session.ensure_access_token(&account_name).await {
            if let Some(session) = state.steam_session.stored(&account_name) {
                let verdict = state
                    .steam_session
                    .verify(&session.steam_id, &access, Some(&session.refresh_token))
                    .await;
                log::info!(
                    "[probe] steam_probe_account -> {} (verify)",
                    match verdict {
                        SessionState::SignedIn => "signed_in",
                        SessionState::SignedOut => "signed_out",
                        SessionState::Unknown => "unknown",
                    }
                );
                return match verdict {
                    SessionState::SignedIn => {
                        // Fetch the profile for the sidebar; a failure here is
                        // cosmetic, so fall back to the minimal identity.
                        let profile = state.workshop.current_account().await;
                        Ok(signed_in_json(profile, &session.steam_id, &account_name))
                    }
                    SessionState::SignedOut => Ok(serde_json::json!({ "state": "signed_out" })),
                    SessionState::Unknown => Ok(serde_json::json!({ "state": "unknown" })),
                };
            }
        }
    }

    // No stored session: fall back to the jar-based probe (e.g. a live webview
    // session established by the QR form but not yet persisted).
    let _ = state.steam_webview.sync_cookies().await;
    let result = state.workshop.probe_account().await;
    log::info!(
        "[probe] steam_probe_account -> {} (jar)",
        match &result {
            crate::workshop::AccountProbe::SignedIn(a) => format!("signed_in({})", a.account_name),
            crate::workshop::AccountProbe::SignedOut => "signed_out".to_string(),
            crate::workshop::AccountProbe::Unknown => "unknown".to_string(),
        }
    );
    match result {
        crate::workshop::AccountProbe::SignedIn(a) => Ok(serde_json::json!({
            "state": "signed_in",
            "account": {
                "persona_name": a.persona_name,
                "account_name": a.account_name,
                "steamid": a.steamid,
                "profile_url": a.profile_url,
                "avatar_url": a.avatar_url,
            },
        })),
        crate::workshop::AccountProbe::SignedOut => {
            Ok(serde_json::json!({ "state": "signed_out" }))
        }
        crate::workshop::AccountProbe::Unknown => Ok(serde_json::json!({ "state": "unknown" })),
    }
}

/// Build the `signed_in` payload for `steam_probe_account`.
///
/// The profile fetch is cosmetic (sidebar name/avatar). When it fails we still
/// return a complete identity from the stored session so the UI can render
/// "signed in" instead of degrading to an empty panel.
fn signed_in_json(
    profile: Option<crate::workshop::SteamAccount>,
    steam_id: &str,
    account_name: &str,
) -> serde_json::Value {
    let (persona_name, profile_account, profile_url, avatar_url) = match profile {
        Some(p) => (p.persona_name, p.account_name, p.profile_url, p.avatar_url),
        None => (String::new(), String::new(), String::new(), String::new()),
    };
    let display_account = if profile_account.is_empty() {
        account_name.to_string()
    } else {
        profile_account
    };
    serde_json::json!({
        "state": "signed_in",
        "account": {
            "persona_name": persona_name,
            "account_name": display_account,
            "steamid": steam_id,
            "profile_url": profile_url,
            "avatar_url": avatar_url,
        },
    })
}

/// Try to restore a Steam session from the encrypted account store.
///
/// This is the fast path at startup: if a session was saved after an earlier
/// sign-in, its tokens are enough to rebuild the `steamLoginSecure` cookie and
/// skip the login page (and Steam Guard). Expired access tokens are refreshed
/// with the stored refresh token (falling back to the Steam client's cached
/// token when that exists).
///
/// Returns a three-state verdict:
///   * `signed_in` — Steam accepted the reconstructed session;
///   * `signed_out` — no usable session material, or Steam rejected it;
///   * `unknown` — the verification call failed (network), which must not be
///     reported as a logout.
#[tauri::command]
pub async fn steam_restore_session(
    state: AppStateHandle<'_>,
    username: Option<String>,
) -> Result<crate::services::steam_session::SessionState, String> {
    use crate::services::steam_session::SessionState;

    // Which account? Default to the first one the user added.
    let account = match username {
        Some(u) if !u.trim().is_empty() => u.trim().to_string(),
        _ => state.accounts.preferred_parser_credentials().username,
    };
    if account.is_empty() {
        return Ok(SessionState::SignedOut);
    }

    let Some(mut access) = state.steam_session.ensure_access_token(&account).await? else {
        log::info!("[steam-session] no usable session material for {account}");
        return Ok(SessionState::SignedOut);
    };
    let Some(session) = state.steam_session.stored(&account) else {
        return Ok(SessionState::SignedOut);
    };

    let mut result = install_and_verify(&state, &session.steam_id, &access, Some(&session.refresh_token)).await?;
    if result == SessionState::SignedOut {
        // The token may still look valid locally (JWT exp in the future) while
        // Steam has revoked it. Force one refresh before asking the user to log
        // in again — this is the "signed in yesterday, revoked today" case.
        if let Ok(Some(refreshed)) = state.steam_session.force_refresh(&account).await {
            access = refreshed;
            result = install_and_verify(&state, &session.steam_id, &access, Some(&session.refresh_token)).await?;
        }
    }
    log::info!("[steam-session] restore verify for {account}: {result:?}");
    Ok(result)
}

/// Install a session cookie and ask Steam whether it is accepted.
async fn install_and_verify(
    state: &AppStateHandle<'_>,
    steam_id: &str,
    access_token: &str,
    refresh_token: Option<&str>,
) -> Result<crate::services::steam_session::SessionState, String> {
    // The window has to exist before cookies can be written into it.
    state.steam_webview.ensure_profile_loaded().await;
    state
        .steam_webview
        .install_session_cookies(steam_id, access_token)
        .map_err(|e| format!("install cookies: {e}"))?;
    tokio::time::sleep(COOKIE_APPLY_WAIT).await;
    let _ = state.steam_webview.sync_cookies().await;
    state.workshop.clear_caches();
    Ok(state.steam_session.verify(steam_id, access_token, refresh_token).await)
}

/// Persist the OAuth tokens that came out of a successful sign-in.
///

#[tauri::command]
pub async fn steam_login_poll_error(state: AppStateHandle<'_>) -> Result<Option<String>, String> {
    let Some(w) = state
        .app_handle
        .get_webview_window(crate::workshop::webview::LABEL)
    else {
        return Ok(None);
    };

    let script = r#"
        (function() {
            try {
                var errDiv = Array.from(document.querySelectorAll('form div')).find(d => {
                    var style = window.getComputedStyle(d);
                    if (style.opacity === '0' || style.display === 'none' || style.visibility === 'hidden' || d.offsetHeight === 0) return false;
                    
                    var t = d.innerText || '';
                    if (!t || t.length < 10 || t.length > 200) return false;
                    
                    if (d.querySelector('input') || d.querySelector('button') || d.querySelector('svg') || d.querySelector('a')) return false;

                    var tLower = t.toLowerCase();
                    var isError = false;
                    var errorWords = [
                        'incorrect', 'check', 'try again', 'too many', 'invalid', 'error', 'captcha', 'sorry'
                    ];
                    
                    for (var i = 0; i < errorWords.length; i++) {
                        if (tLower.includes(errorWords[i])) {
                            isError = true;
                            break;
                        }
                    }
                    
                    return isError;
                });
                if (errDiv) {
                    var errStr = errDiv.innerText.trim();
                    history.replaceState(null, '', window.location.pathname + window.location.search + '#WE_ERR:' + encodeURIComponent(errStr));
                } else {
                    history.replaceState(null, '', window.location.pathname + window.location.search);
                }
            } catch(e) {}
        })();
    "#;
    w.eval(script).ok();

    // The injected probe writes its result into location.hash; give the browser
    // a tick to run it before we read the URL back.
    tokio::time::sleep(EVAL_SETTLE).await;

    if let Ok(url) = w.url() {
        if let Some(fragment) = url.fragment() {
            if fragment.starts_with("WE_ERR:") {
                let encoded = fragment.replace("WE_ERR:", "");
                if let Ok(decoded) = urlencoding::decode(&encoded) {
                    return Ok(Some(decoded.to_string()));
                }
            }
        }
    }

    Ok(None)
}

// ───────────────────────── OAuth sign-in ─────────────────────────
//
// The WebView2 form-filling path cannot get past Steam Guard: it fills the
// password and presses the button, Steam answers with a GUARD page, and nothing
// in a headless webview can approve a phone prompt. Steam's own OAuth API is
// designed for this — it reports *which* guard is required, and accepts a code
// typed by the user. `steamroom` wraps the whole flow.
//
// The flow is split across three commands because each step may need something
// from the user (a code, or a tap on their phone), and a Tauri command cannot
// stay suspended waiting for that.
#[cfg(test)]
mod tests {
    use super::signed_in_json;

    #[test]
    fn signed_in_json_uses_profile_when_available() {
        let profile = crate::workshop::SteamAccount {
            persona_name: "Persona".into(),
            account_name: "account".into(),
            steamid: "765".into(),
            profile_url: "https://steamcommunity.com/id/x".into(),
            avatar_url: "data:image/png;base64,x".into(),
        };
        let value = signed_in_json(Some(profile), "765", "fallback");
        assert_eq!(value["state"], "signed_in");
        assert_eq!(value["account"]["persona_name"], "Persona");
        assert_eq!(value["account"]["account_name"], "account");
        assert_eq!(value["account"]["steamid"], "765");
    }

    #[test]
    fn signed_in_json_falls_back_to_account_name() {
        let value = signed_in_json(None, "765", "fallback");
        assert_eq!(value["account"]["account_name"], "fallback");
        assert_eq!(value["account"]["steamid"], "765");
        assert_eq!(value["account"]["persona_name"], "");
    }
}
