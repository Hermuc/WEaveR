//! Steam account commands, split by auth flow.
//!
//! `qr` and `oauth` are re-exported so callers keep using
//! `commands::steam::steam_qr_begin` etc. unchanged.

mod oauth;
mod qr;
mod signout;

pub use oauth::*;
pub use qr::*;
pub use signout::*;

// Explicit re-export: `pub(crate)` items are not reliably carried by a glob
// `pub use`, and other command modules need this shared service-level entry.
pub(crate) use qr::sign_in_with_saved_credentials;

/// Re-enable "stay signed in" after a successful sign-in.
///
/// `steam_sign_out` sets `account.auto_login = false` so the next launch does not
/// silently sign the user back in. Every successful sign-in path (OAuth, QR,
/// saved-credential) must undo that, otherwise one sign-out would permanently
/// disable auto-login. Centralised here so no entry point can forget it.
pub(crate) fn mark_signed_in(state: &crate::commands::AppStateHandle<'_>) {
    if let Err(e) = state
        .settings
        .read()
        .set(
            crate::config::KEY_ACCOUNT_AUTO_LOGIN,
            serde_json::Value::Bool(true),
        )
    {
        log::warn!("[steam-session] could not persist auto_login=true: {e}");
    }
}
