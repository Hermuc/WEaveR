//! Structured error protocol shared by every Tauri command.
//!
//! # Why this exists
//!
//! Commands used to return `Result<T, String>`, so the frontend could only
//! tell error kinds apart by matching message prefixes:
//!
//! ```text
//! if (error.includes('Authentication error:')) { ... }
//! if (error.includes('INVALID_PASSWORD')) { ... }
//! ```
//!
//! That breaks as soon as a message is reworded or translated. `AppError`
//! carries a machine-readable `code` instead, so the frontend switches on data
//! rather than prose.
//!
//! # Wire format
//!
//! `AppError` serialises as JSON, and `From<AppError> for String` keeps the old
//! `Result<T, String>` command signatures working during the migration. A
//! command that has not been migrated yet still emits a plain string; the
//! frontend keeps its prefix matching as a fallback until every command is
//! converted.
//!
//! ```json
//! { "code": "auth", "message": "...", "detail": "invalid_password" }
//! ```
//!
//! # Adding a variant
//!
//! Add it to [`AppError`], give it a stable snake_case `code` in [`ErrorCode`],
//! and add the matching frontend type in `src/lib/errors.ts`. Never rename an
//! existing `code` — the frontend matches on it.

use serde::Serialize;

/// Machine-readable error category. Keep these stable: the frontend switches on
/// them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    Network,
    FileSystem,
    Config,
    Workshop,
    WallpaperEngine,
    Download,
    Auth,
    Validation,
    NotFound,
    Internal,
}

/// Authentication failures the UI reacts to individually.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthReason {
    /// Wrong password.
    InvalidPassword,
    /// Wrong Steam Guard code.
    InvalidGuardCode,
    /// The code expired before it was submitted.
    ExpiredCode,
    /// Rate limited by Steam.
    RateLimited,
    /// The user did not confirm on their phone in time.
    ConfirmationTimeout,
    /// The connection dropped while waiting.
    ConnectionLost,
}

impl AuthReason {
    /// Stable snake_case tag, used as `AppError::detail`.
    pub fn tag(self) -> &'static str {
        match self {
            AuthReason::InvalidPassword => "invalid_password",
            AuthReason::InvalidGuardCode => "invalid_guard_code",
            AuthReason::ExpiredCode => "expired_code",
            AuthReason::RateLimited => "rate_limited",
            AuthReason::ConfirmationTimeout => "confirmation_timeout",
            AuthReason::ConnectionLost => "connection_lost",
        }
    }
}

/// A structured error crossing the command boundary.
#[derive(Debug, Clone, Serialize)]
pub struct AppError {
    pub code: ErrorCode,
    /// Human-readable text. May be translated or reworded; never match on it.
    pub message: String,
    /// Optional machine-readable refinement, e.g. an [`AuthReason`] tag.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl AppError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            detail: None,
        }
    }

    /// Attach a machine-readable refinement (e.g. an [`AuthReason`] tag).
    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }

    pub fn network(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Network, message)
    }

    pub fn file_system(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::FileSystem, message)
    }

    pub fn config(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Config, message)
    }

    pub fn workshop(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Workshop, message)
    }

    pub fn wallpaper_engine(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::WallpaperEngine, message)
    }

    pub fn download(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Download, message)
    }

    pub fn auth(reason: AuthReason, message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Auth, message).with_detail(reason.tag())
    }

    pub fn validation(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Validation, message)
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::NotFound, message)
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Internal, message)
    }

    /// Wrap a legacy `String` error that crossed a not-yet-migrated boundary.
    ///
    /// Used as `.map_err(AppError::from_legacy)` while a callee still returns
    /// `Result<T, String>`. The message is preserved verbatim, so a caller that
    /// converts back to `String` at the command boundary is lossless. Once the
    /// callee is migrated too, this call disappears.
    pub fn from_legacy(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Internal, message)
    }

    /// A validation failure, the most common shape of a legacy message.
    pub fn from_legacy_validation(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Validation, message)
    }
}

/// Display text for the log and for the legacy `String` path.
impl std::fmt::Display for AppError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for AppError {}

/// Bridge for commands that still declare `Result<T, String>`.
///
/// Migrated commands can return `Result<T, AppError>` directly; unmigrated ones
/// call `.map_err(String::from)`. Both reach the frontend, so the fallback prefix
/// matching in `parseCommandError` keeps working either way.
impl From<AppError> for String {
    fn from(err: AppError) -> String {
        err.message
    }
}

/// Common conversions so `?` works inside migrated functions.
impl From<std::io::Error> for AppError {
    fn from(err: std::io::Error) -> Self {
        AppError::file_system(err.to_string())
    }
}

impl From<serde_json::Error> for AppError {
    fn from(err: serde_json::Error) -> Self {
        AppError::internal(format!("JSON error: {err}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_serialise_as_snake_case() {
        let json = serde_json::to_string(&AppError::file_system("boom")).unwrap();
        assert!(json.contains("\"code\":\"file_system\""), "got {json}");
    }

    #[test]
    fn wallpaper_engine_code_is_not_split() {
        let json = serde_json::to_string(&AppError::wallpaper_engine("x")).unwrap();
        assert!(json.contains("\"code\":\"wallpaper_engine\""), "got {json}");
    }

    #[test]
    fn detail_is_omitted_when_absent() {
        let json = serde_json::to_string(&AppError::config("x")).unwrap();
        assert!(!json.contains("detail"), "got {json}");
    }

    #[test]
    fn auth_carries_the_reason_as_detail() {
        let err = AppError::auth(AuthReason::InvalidPassword, "wrong password");
        assert_eq!(err.code, ErrorCode::Auth);
        assert_eq!(err.detail.as_deref(), Some("invalid_password"));
        assert_eq!(err.message, "wrong password");
    }

    #[test]
    fn every_auth_reason_has_a_distinct_tag() {
        let reasons = [
            AuthReason::InvalidPassword,
            AuthReason::InvalidGuardCode,
            AuthReason::ExpiredCode,
            AuthReason::RateLimited,
            AuthReason::ConfirmationTimeout,
            AuthReason::ConnectionLost,
        ];
        let tags: Vec<_> = reasons.iter().map(|r| r.tag()).collect();
        let unique: std::collections::HashSet<_> = tags.iter().collect();
        assert_eq!(tags.len(), unique.len(), "duplicate tag: {tags:?}");
    }

    #[test]
    fn the_string_bridge_preserves_the_message() {
        let legacy: String = AppError::download("disk full").into();
        assert_eq!(legacy, "disk full");
    }

    #[test]
    fn io_errors_become_file_system_errors() {
        let io = std::io::Error::new(std::io::ErrorKind::NotFound, "missing");
        let err: AppError = io.into();
        assert_eq!(err.code, ErrorCode::FileSystem);
        assert!(err.message.contains("missing"));
    }
}
