//! Sign-out command.
//!
//! Kept separate from `qr`/`oauth`: those are two ways *in*, this is the way
//! *out*. Re-exported from the parent module like the others.

use crate::commands::AppStateHandle;

/// Sign the user out of Steam.
///
/// Clears all three places a session can live, so a later start cannot quietly
/// sign back in:
///   1. the Steam cookies in the WebView2 store,
///   2. the copies in the in-memory reqwest jar, and
///   3. the encrypted OAuth tokens in `user_accounts.enc`.
///
/// The username/password stay saved — this is "sign out", not "forget account";
/// removing the credential is a separate, explicit action.
#[tauri::command]
pub async fn steam_sign_out(state: AppStateHandle<'_>) -> Result<(), String> {
    let removed = state.steam_webview.sign_out().await?;

    let account = state.accounts.preferred_parser_credentials().username;
    if !account.is_empty() {
        // Best-effort: the webview cookies are already gone, so a failure here
        // only means the *next* launch might restore a stale session.
        if let Err(e) = state.accounts.clear_session(&account) {
            log::warn!("[steam-signout] could not clear stored session for {account}: {e}");
        }
    }

    state.workshop.clear_caches();

    // Remember that the user *chose* to sign out. Without this the startup
    // flow would sign straight back in with the saved password on the next
    // launch, which is exactly how a "signed out" app came back authenticated.
    // Any successful sign-in flips this back to `true`.
    if let Err(e) = state
        .settings
        .read()
        .set(
            crate::config::KEY_ACCOUNT_AUTO_LOGIN,
            serde_json::Value::Bool(false),
        )
    {
        log::warn!("[steam-signout] could not persist auto_login=false: {e}");
    }

    log::info!("[steam-signout] removed {removed} Steam cookies; cleared stored session; disabled auto-login");
    Ok(())
}