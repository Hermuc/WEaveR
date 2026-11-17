//! Best-effort access to the local Steam client's cached refresh token.
//!
//! The extraction itself lives in the vendored `steamroom-client` crate
//! (`steam_creds`), which is already used by the video-streaming pipeline:
//! Windows reads `%LOCALAPPDATA%\\Steam\\local.vdf` and decrypts the
//! `ConnectCache` blob with DPAPI (entropy = account name); Linux/macOS use
//! Steam's own AES-CBC layout.
//!
//! Treat this as a *convenience* source only. P0 measurement on 2026-11-17:
//! the cached token on this machine was rejected by Steam with `AccessDenied`
//! (it had been rotated/revoked since the Steam client last ran), so callers
//! must fall through to a real refresh-token exchange or an interactive login
//! instead of treating a local token as guaranteed.

/// Extract the cached refresh token for an account, if Steam is installed and
/// the token is still present. Never fails: absence is normal.
pub fn extract_refresh_token(account_name: &str) -> Option<String> {
    steamroom_client::steam_creds::extract_token(account_name).map(|token| token.refresh_token)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_account_returns_none_without_panicking() {
        // A name no Steam install will have cached; must not panic or block.
        assert!(extract_refresh_token("__weave_no_such_account__").is_none());
    }
}
