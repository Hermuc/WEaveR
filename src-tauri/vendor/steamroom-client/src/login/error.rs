use steamroom::enums::EResultError;
use thiserror::Error;

/// Errors produced by the login builder.
///
/// `InvalidPassword` and `InvalidGuardCode` are promoted to top-level variants
/// because they are the expected user-recoverable cases. The CLI's retry loops
/// match them directly without inspecting nested error chains.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum LoginError {
    #[error("transport / connection error: {0}")]
    Transport(#[from] steamroom::Error),

    #[error("logon rejected by Steam: {0}")]
    LogonFailed(EResultError),

    #[error("invalid password")]
    InvalidPassword,

    #[error("two-factor code rejected")]
    InvalidGuardCode,

    #[error("Steam response missing field: {0}")]
    MissingField(&'static str),

    #[error("no CM servers available")]
    NoCmServers,

    /// The user never approved the sign-in (mobile tap / QR scan) before the
    /// caller's deadline expired. Distinct from [`Transport`] so the UI can say
    /// "you did not confirm in time" instead of blaming the network.
    ///
    /// [`Transport`]: LoginError::Transport
    #[error("timed out waiting for confirmation after {elapsed:?}")]
    ConfirmationTimeout { elapsed: std::time::Duration },

    /// The CM connection dropped and could not be re-established within the
    /// retry budget. The auth session itself is still valid server-side, so a
    /// fresh sign-in attempt can succeed immediately.
    #[error("connection lost while waiting for confirmation: {last_error}")]
    ConnectionLost { last_error: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_password_display() {
        let err = LoginError::InvalidPassword;
        assert_eq!(err.to_string(), "invalid password");
    }

    #[test]
    fn missing_field_display() {
        let err = LoginError::MissingField("eresult");
        assert_eq!(err.to_string(), "Steam response missing field: eresult");
    }

    #[test]
    fn transport_from_steamroom_error() {
        let inner =
            steamroom::Error::Connection(steamroom::error::ConnectionError::DnsResolutionFailed);
        let err: LoginError = inner.into();
        assert!(matches!(err, LoginError::Transport(_)));
    }

    #[test]
    fn confirmation_timeout_names_the_elapsed_time() {
        let err = LoginError::ConfirmationTimeout {
            elapsed: std::time::Duration::from_secs(300),
        };
        assert_eq!(
            err.to_string(),
            "timed out waiting for confirmation after 300s"
        );
    }

    #[test]
    fn connection_lost_keeps_the_last_transport_error() {
        let err = LoginError::ConnectionLost {
            last_error: "disconnected".into(),
        };
        assert_eq!(
            err.to_string(),
            "connection lost while waiting for confirmation: disconnected"
        );
    }

    #[test]
    fn logon_failed_display_uses_display_not_debug() {
        // Known variant goes through Display (which writes the Debug form internally for knowns).
        let err = LoginError::LogonFailed(EResultError::InvalidPassword);
        assert_eq!(err.to_string(), "logon rejected by Steam: InvalidPassword");

        // Unknown variant should produce the human-readable Display form,
        // not the Debug form "Unknown(42)".
        let err = LoginError::LogonFailed(EResultError::Unknown(42));
        assert_eq!(
            err.to_string(),
            "logon rejected by Steam: unknown error (code 42)"
        );
    }
}
