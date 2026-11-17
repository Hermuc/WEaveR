//! Steam JWT claims — parsing and validity checks.
//!
//! Steam issues three flavours of token as JWTs:
//!   * access tokens (audience includes `web`) — what `steamLoginSecure` accepts;
//!   * refresh tokens (audience includes `derive`/`renew`) — what
//!     `GenerateAccessTokenForApp` exchanges for a fresh access token;
//!   * client tokens (audience includes `client`) — what CM logon uses.
//!
//! The only claims this module relies on are `sub` (the SteamID64), `exp`
//! (unix seconds) and `aud` (for diagnostics and the cheap suitability check).
//! Everything is pure so it can be exercised without a network or a WebView.

use base64::Engine as _;
use serde_json::Value;

/// The minimum lifetime a token must still have to be considered usable.
///
/// A token that expires while it is being installed as a cookie is worse than
/// one that is refreshed a minute early, so callers get this skew for free.
pub const CLOCK_SKEW_SECS: i64 = 60;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenKind {
    Access,
    Refresh,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Claims {
    pub sub: Option<String>,
    pub aud: Vec<String>,
    pub exp: Option<i64>,
}

/// Parse the payload of a JWT without verifying its signature.
///
/// Signature verification is Steam's job: the token is only ever presented to
/// Steam, never trusted locally for anything but routing decisions. A malformed
/// token yields `None` instead of an error because every caller treats
/// "cannot parse" the same way: "not usable".
pub fn parse_claims(token: &str) -> Option<Claims> {
    let mut parts = token.split('.');
    let _header = parts.next()?;
    let payload = parts.next()?;
    let _signature = parts.next()?;
    if parts.next().is_some() {
        return None;
    }
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload)
        .ok()?;
    let value: Value = serde_json::from_slice(&bytes).ok()?;

    let sub = value
        .get("sub")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let aud = match value.get("aud") {
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(|v| v.as_str())
            .map(str::to_string)
            .collect(),
        Some(Value::String(s)) => vec![s.clone()],
        _ => Vec::new(),
    };
    let exp = value.get("exp").and_then(|v| v.as_i64());
    Some(Claims { sub, aud, exp })
}

/// Whether a token can be used for the given purpose right now.
///
/// Checks the account binding and expiry, both of which are hard requirements.
/// Audience is *not* enforced here: Steam-issued tokens differ by login
/// platform (client/web/mobile), and rejecting a token locally because of an
/// audience name would turn a working session into a forced re-login. The
/// audience is logged by callers and the final authority is Steam's own
/// response to the cookie / service call.
pub fn is_usable(token: &str, steam_id: &str, _kind: TokenKind) -> bool {
    let Some(claims) = parse_claims(token) else {
        return false;
    };
    if claims.sub.as_deref() != Some(steam_id) {
        return false;
    }
    let Some(exp) = claims.exp else {
        return false;
    };
    let now = unix_now();
    exp > now + CLOCK_SKEW_SECS
}

/// Seconds since the Unix epoch, or 0 if the clock is before 1970.
pub fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Human-readable audience list for logs: `web,derive` or `-` when absent.
pub fn describe_aud(token: &str) -> String {
    match parse_claims(token) {
        Some(c) if !c.aud.is_empty() => c.aud.join(","),
        _ => "-".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn token(payload: &str) -> String {
        let encoded = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(payload.as_bytes());
        format!("header.{encoded}.signature")
    }

    #[test]
    fn parses_sub_exp_and_array_audience() {
        let t = token(
            r#"{"sub":"76561198000000001","aud":["client","web","renew","derive"],"exp":2000000000}"#,
        );
        let c = parse_claims(&t).expect("claims");
        assert_eq!(c.sub.as_deref(), Some("76561198000000001"));
        assert_eq!(c.aud, ["client", "web", "renew", "derive"]);
        assert_eq!(c.exp, Some(2000000000));
    }

    #[test]
    fn parses_string_audience_and_missing_fields() {
        let t = token(r#"{"sub":"1","aud":"web"}"#);
        let c = parse_claims(&t).expect("claims");
        assert_eq!(c.aud, ["web"]);
        assert_eq!(c.exp, None);
    }

    #[test]
    fn rejects_malformed_tokens() {
        assert!(parse_claims("not-a-jwt").is_none());
        assert!(parse_claims("a.b").is_none());
        assert!(parse_claims("a.!!!.c").is_none());
        assert!(parse_claims("").is_none());
    }

    #[test]
    fn usable_requires_matching_subject_and_future_expiry() {
        let good = token(r#"{"sub":"42","aud":["web"],"exp":2000000000}"#);
        assert!(is_usable(&good, "42", TokenKind::Access));
        assert!(!is_usable(&good, "43", TokenKind::Access), "wrong account");

        let expired = token(r#"{"sub":"42","aud":["web"],"exp":1000000000}"#);
        assert!(!is_usable(&expired, "42", TokenKind::Access), "expired");

        let no_exp = token(r#"{"sub":"42","aud":["web"]}"#);
        assert!(!is_usable(&no_exp, "42", TokenKind::Access), "no expiry");
    }

    #[test]
    fn usable_respects_clock_skew() {
        let now = unix_now();
        let soon = token(&format!(r#"{{"sub":"42","exp":{}}}"#, now + 10));
        assert!(
            !is_usable(&soon, "42", TokenKind::Access),
            "expires within skew"
        );
        let later = token(&format!(r#"{{"sub":"42","exp":{}}}"#, now + 3600));
        assert!(is_usable(&later, "42", TokenKind::Access));
    }

    #[test]
    fn describes_audience_for_logs() {
        let t = token(r#"{"sub":"1","aud":["client","web"],"exp":2000000000}"#);
        assert_eq!(describe_aud(&t), "client,web");
        assert_eq!(describe_aud("bad"), "-");
    }
}
