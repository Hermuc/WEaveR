//! Verify that a `steamLoginSecure` token really authenticates a community session.
//!
//! A cookie that merely *exists* proves nothing — Steam happily keeps an
//! expired `steamLoginSecure` in the jar, and `is_logged_in_inner()` used to
//! mistake that for a live session. The authoritative check is Steam's own
//! redirect: `/my/` answers a logged-in browser with 302 to `/id/<vanity>` or
//! `/profiles/<steamid>`, and an anonymous one with 302 to `/login/`.
//!
//! This mirrors Watt Toolkit's `CreateWebSessionAsync` verification, including
//! the "do not follow redirects" part: following it would download the whole
//! profile page just to decide a boolean.

use std::time::Duration;

const VERIFY_TIMEOUT: Duration = Duration::from_secs(15);
const COMMUNITY_MY_URL: &str = "https://steamcommunity.com/my/";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerifyResult {
    SignedIn,
    SignedOut,
    /// The request itself failed (DNS, TLS, timeout). This is *not* evidence
    /// that the session is invalid and must never be reported as such.
    Unknown,
}

/// Build the client used for verification.
///
/// Redirects are disabled because the redirect target *is* the answer.
pub fn build_client() -> reqwest::Client {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(VERIFY_TIMEOUT)
        .build()
        .expect("verify client")
}

/// Check a session by asking Steam who we are.
pub async fn verify_session(
    client: &reqwest::Client,
    steam_id: &str,
    access_token: &str,
) -> VerifyResult {
    let cookie = format!(
        "steamLoginSecure={}%7C%7C{}; sessionid={}",
        steam_id,
        access_token,
        random_session_id()
    );
    let response = client
        .get(COMMUNITY_MY_URL)
        .header(reqwest::header::COOKIE, cookie)
        .send()
        .await;
    match response {
        Ok(resp) => classify(
            resp.status().as_u16(),
            resp.headers()
                .get(reqwest::header::LOCATION)
                .and_then(|v| v.to_str().ok()),
            steam_id,
        ),
        Err(e) => {
            log::warn!("[steam-session] verify request failed: {e}");
            VerifyResult::Unknown
        }
    }
}

/// Turn an HTTP status + optional `Location` into a session verdict.
///
/// Split out so the redirect rules are unit-testable without a network.
pub fn classify(status: u16, location: Option<&str>, steam_id: &str) -> VerifyResult {
    let Some(location) = location else {
        // A 2xx on /my/ without a redirect is not a profile page we can trust;
        // Steam always redirects a logged-in /my/ to the profile URL.
        return if (200..300).contains(&status) {
            VerifyResult::SignedOut
        } else {
            VerifyResult::Unknown
        };
    };
    let Ok(url) = url::Url::parse(location) else {
        return VerifyResult::Unknown;
    };
    if url.scheme() != "https"
        || url.host_str() != Some("steamcommunity.com")
        || url.port().unwrap_or(443) != 443
    {
        return VerifyResult::Unknown;
    }
    let path = url.path();
    let profile_prefix = format!("/profiles/{steam_id}");
    if path.starts_with("/id/")
        || path == profile_prefix
        || path.starts_with(&format!("{profile_prefix}/"))
    {
        VerifyResult::SignedIn
    } else {
        VerifyResult::SignedOut
    }
}

fn random_session_id() -> String {
    use rand::RngExt;
    let mut bytes = [0u8; 16];
    rand::rng().fill(&mut bytes);
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SID: &str = "76561198000000001";

    #[test]
    fn accepts_vanity_and_profile_redirects() {
        assert_eq!(
            classify(302, Some("https://steamcommunity.com/id/someone/"), SID),
            VerifyResult::SignedIn
        );
        assert_eq!(
            classify(
                302,
                Some("https://steamcommunity.com/profiles/76561198000000001"),
                SID
            ),
            VerifyResult::SignedIn
        );
        assert_eq!(
            classify(
                301,
                Some("https://steamcommunity.com/profiles/76561198000000001/home/"),
                SID
            ),
            VerifyResult::SignedIn
        );
    }

    #[test]
    fn rejects_login_redirects() {
        assert_eq!(
            classify(
                302,
                Some("https://steamcommunity.com/login/home/?goto=%2Fmy%2F"),
                SID
            ),
            VerifyResult::SignedOut
        );
        assert_eq!(
            classify(302, Some("https://steamcommunity.com/login/"), SID),
            VerifyResult::SignedOut
        );
    }

    #[test]
    fn rejects_other_accounts_and_hosts() {
        assert_eq!(
            classify(
                302,
                Some("https://steamcommunity.com/profiles/76561198000000002"),
                SID
            ),
            VerifyResult::SignedOut
        );
        assert_eq!(
            classify(302, Some("https://evil.example/id/someone"), SID),
            VerifyResult::Unknown
        );
        assert_eq!(
            classify(302, Some("http://steamcommunity.com/id/someone"), SID),
            VerifyResult::Unknown
        );
    }

    #[test]
    fn treats_2xx_as_signed_out_and_other_statuses_as_unknown() {
        assert_eq!(classify(200, None, SID), VerifyResult::SignedOut);
        assert_eq!(classify(500, None, SID), VerifyResult::Unknown);
    }
}
