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

/// Clear every cache the Workshop browser keeps, WITHOUT touching the session.
///
/// Exists so "I still see content after signing out" can be tested directly:
/// if the same items come back after this runs, the cause is not a cache.
/// Three layers are cleared:
///   1. the parser page/item LRU caches,
///   2. the WebView2 HTTP/disk cache, and
///   3. the frontend preload cache (the caller bumps the refresh counter).
///
/// Cookies are deliberately left alone - this is not a sign-out.
#[tauri::command]
pub async fn steam_clear_browsing_cache(state: AppStateHandle<'_>) -> Result<(), String> {
    state.workshop.clear_caches();
    // `clear_all_browsing_data` would also drop cookies, which we must keep;
    // the WebView2 cache is emptied through the JS API instead.
    if let Some(w) = state
        .app_handle
        .get_webview_window(crate::workshop::webview::LABEL)
    {
        // Best effort: a hidden webview may refuse to evaluate, and that is
        // not a reason to fail the whole command.
        let _ = w.eval("try{caches.keys().then(k=>k.forEach(n=>caches.delete(n)))}catch(e){}");
    }
    log::info!("[steam-cache] cleared parser + webview caches (session untouched)");
    Ok(())
}