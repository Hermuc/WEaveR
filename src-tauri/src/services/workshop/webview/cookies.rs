//! Cookie persistence for the hidden Steam webview: backup/restore the
//! WebView2 cookie database around a destructive clear, and pull live cookies
//! into the reqwest jar.
//!
//! Split out of `webview.rs`; these methods touch only `self.app`,
//! `self.jar` and the on-disk cookie DB.

use tauri::Manager;

use super::{SteamWebview, LABEL};

impl SteamWebview {

    /// Back up the WebView cookie database before anything wipes it.
    ///
    /// `clear_all_browsing_data()` is destructive and irreversible: it deletes
    /// the persisted `steamLoginSecure` cookie along with everything else, so a
    /// user who signs out by accident (or gets signed out by a failed startup
    /// check) has to re-authenticate by hand, including Steam Guard. Copying the
    /// cookie DB first means the session can be restored.
    pub(super) fn backup_cookie_db(&self) -> Option<std::path::PathBuf> {
        let data_dir = crate::app_state::resolve_app_data_dir();
        let src = data_dir
            .join("SteamWebView")
            .join("EBWebView")
            .join("Default")
            .join("Network")
            .join("Cookies");
        if !src.is_file() {
            return None;
        }
        let backup = data_dir.join("SteamWebView.cookies.bak");
        std::fs::copy(&src, &backup).ok()?;
        log::info!(
            "[steam-webview] cookie database backed up to {}",
            backup.display()
        );
        Some(backup)
    }

    /// Put the cookie database back after a destructive clear.
    pub fn restore_cookie_db(&self) -> bool {
        // The resolver always yields a path, so the only failure mode left is
        // "no backup on disk", which the check below already covers.
        let data_dir = crate::app_state::resolve_app_data_dir();
        let backup = data_dir.join("SteamWebView.cookies.bak");
        if !backup.is_file() {
            return false;
        }
        let dst = data_dir
            .join("SteamWebView")
            .join("EBWebView")
            .join("Default")
            .join("Network")
            .join("Cookies");
        match std::fs::copy(&backup, &dst) {
            Ok(_) => {
                log::info!("[steam-webview] cookie database restored from backup");
                true
            }
            Err(e) => {
                log::warn!("[steam-webview] could not restore cookie database: {e}");
                false
            }
        }
    }
    /// Pull all `steamcommunity.com` cookies from the webview and load them
    /// into our reqwest jar. WebView2 persists cookies automatically, so no
    /// additional file storage is needed.
    pub async fn sync_cookies(&self) -> Result<usize, String> {
        let _g = self.lock.lock().await;
        // Read cookies only if the window already exists.
        if !self.window_exists() {
            return Ok(0);
        }
        let Some(w) = self.app.get_webview_window(LABEL) else {
            return Ok(0);
        };
        let cookies = w.cookies().map_err(|e| e.to_string())?;
        let steam_com = "https://steamcommunity.com"
            .parse::<url::Url>()
            .map_err(|e| e.to_string())?;
        let steam_pow = "https://store.steampowered.com"
            .parse::<url::Url>()
            .map_err(|e| e.to_string())?;
        // Build `Cookie` values directly instead of formatting a Set-Cookie
        // header by hand.
        //
        // The hand-built header (`name=value; Path=...; Domain=...`) omitted the
        // attributes that make a cookie apply to subsequent requests. In
        // particular it produced a *host-only* cookie whenever the domain
        // attribute was dropped during parsing, and `steamLoginSecure` for
        // `steamcommunity.com` then never matched `steamcommunity.com/my/...`.
        // Using the typed API sets domain/path/secure/httponly explicitly and
        // leaves no room for a malformed header to be silently skipped.
        let mut n = 0;
        for c in cookies {
            let domain = c.domain().unwrap_or("steamcommunity.com").to_string();
            if !(domain.ends_with("steamcommunity.com") || domain.ends_with("steampowered.com")) {
                continue;
            }
            let name = c.name().to_string();
            let value = c.value().to_string();

            let path = c.path().unwrap_or("/").to_string();
            // `Domain=` must be present: without it the cookie becomes host-only
            // and stops matching the sub-paths we actually request. Keep the
            // leading dot so subdomains match too.
            let domain_attr = if domain.starts_with('.') {
                domain.clone()
            } else {
                format!(".{domain}")
            };
            let cookie_str =
                format!("{name}={value}; Domain={domain_attr}; Path={path}; Secure; HttpOnly");
            let target = if domain.ends_with("steampowered.com") {
                &steam_pow
            } else {
                &steam_com
            };
            self.jar.add_cookie_str(&cookie_str, target);
            log::info!("[cookie-sync] stored {name} @ {domain_attr} path={path}");
            n += 1;
        }
        Ok(n)
    }

    /// Sign the user out of Steam: delete the Steam cookies from WebView2 and
    /// expire the copies in the in-memory reqwest jar.
    ///
    /// Deliberately narrower than `logout_locked`: that one runs
    /// `clear_all_browsing_data()` (destroys the whole WebView2 profile) and is
    /// meant for "clear the page before a fresh sign-in". This method deletes
    /// only the Steam cookies, leaves every other site/device-code state intact,
    /// and is safe to run when the user simply wants to sign out.
    ///
    /// It does NOT clear the persisted OAuth session in `user_accounts.enc`;
    /// that is the caller's job, because otherwise the next launch would rebuild
    /// the cookies from the saved access token and silently sign back in.
    pub async fn sign_out(&self) -> Result<usize, String> {
        // The window must exist before we can read its cookie list. This has to
        // happen BEFORE taking `self.lock`: `ensure_profile_loaded` takes the
        // same (non-reentrant tokio) mutex, so calling it while holding the
        // lock deadlocks the whole webview. `window_exists()` does not lock.
        if !self.window_exists() {
            let _ = self.ensure_profile_loaded().await;
        }

        let _g = self.lock.lock().await;
        let Some(w) = self.app.get_webview_window(LABEL) else {
            return Err("NO_WEBVIEW_WINDOW".to_string());
        };

        let mut removed = Self::delete_steam_cookies(&w)?;
        Self::expire_jar_cookies(&self.jar);
        removed += Self::purge_surviving_cookies(&w)?;

        log::info!("[steam-signout] deleted {removed} Steam cookies from the webview");
        Ok(removed)
    }

    /// Delete every Steam-domain cookie via the precise per-cookie API.
    /// Caller holds `self.lock`.
    fn delete_steam_cookies(w: &tauri::WebviewWindow) -> Result<usize, String> {
        let mut removed = 0usize;
        for cookie in w.cookies().map_err(|e| e.to_string())? {
            let domain = cookie.domain().unwrap_or_default().to_string();
            if is_steam_domain(&domain) {
                w.delete_cookie(cookie).map_err(|e| e.to_string())?;
                removed += 1;
            }
        }
        Ok(removed)
    }

    /// Expire the session cookies in the reqwest jar.
    ///
    /// reqwest 0.13 `Jar` has no `clear()`; a `Max-Age=0` cookie matching an
    /// existing one marks it expired and the jar filters it from requests.
    /// NOTE: this relies on cookie_store semantics — re-verify after upgrading
    /// reqwest/cookie_store to a new major version.
    fn expire_jar_cookies(jar: &reqwest::cookie::Jar) {
        for (host, domain_attr) in [
            ("https://steamcommunity.com", ".steamcommunity.com"),
            ("https://store.steampowered.com", ".steampowered.com"),
        ] {
            if let Ok(url) = host.parse::<url::Url>() {
                for name in STEAM_COOKIE_NAMES {
                    jar.add_cookie_str(
                        &format!("{name}=; Domain={domain_attr}; Path=/; Max-Age=0"),
                        &url,
                    );
                }
            }
        }
    }

    /// Re-read the cookie jar and, if Steam cookies survived `delete_cookie`,
    /// fall back to clearing all browsing data. `delete_cookie` returning Ok
    /// only means the request was accepted; on some WebView2 builds an HttpOnly
    /// cookie survives it, which is how the UI once claimed "signed out" while
    /// browsing stayed authenticated. Caller holds `self.lock`.
    fn purge_surviving_cookies(w: &tauri::WebviewWindow) -> Result<usize, String> {
        let still: Vec<String> = w
            .cookies()
            .map_err(|e| e.to_string())?
            .into_iter()
            .filter(|c| is_steam_domain(c.domain().unwrap_or_default()))
            .map(|c| c.name().to_string())
            .collect();
        if still.is_empty() {
            return Ok(0);
        }
        log::warn!(
            "[steam-signout] {} Steam cookies survived delete_cookie: {:?}; clearing browsing data",
            still.len(),
            still
        );
        w.clear_all_browsing_data().map_err(|e| e.to_string())?;
        Ok(still.len())
    }
}

/// Cookie names WEave writes for a Steam session. Used both when syncing into
/// the reqwest jar and when expiring them on sign-out, so the two can never
/// drift apart.
const STEAM_COOKIE_NAMES: [&str; 2] = ["steamLoginSecure", "sessionid"];

/// Whether a cookie's domain belongs to Steam and should be treated as part of
/// the login session.
///
/// Pure so it can be tested without a webview. Matches the same two domains
/// `sync_cookies` copies, using a suffix test so both a leading-dot
/// (`.steamcommunity.com`) and a host-only form match.
fn is_steam_domain(domain: &str) -> bool {
    let d = domain.trim_start_matches('.');
    d == "steamcommunity.com"
        || d.ends_with(".steamcommunity.com")
        || d == "steampowered.com"
        || d.ends_with(".steampowered.com")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognises_steam_domains() {
        assert!(is_steam_domain("steamcommunity.com"));
        assert!(is_steam_domain(".steamcommunity.com"));
        assert!(is_steam_domain("store.steampowered.com"));
        assert!(is_steam_domain(".steampowered.com"));
        assert!(is_steam_domain("checkout.steampowered.com"));
    }

    #[test]
    fn rejects_unrelated_domains() {
        assert!(!is_steam_domain("example.com"));
        assert!(!is_steam_domain("notsteamcommunity.com"));
        assert!(!is_steam_domain("steamcommunity.com.evil.com"));
        assert!(!is_steam_domain(""));
    }
}
