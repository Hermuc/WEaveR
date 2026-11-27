//! Exchange a Steam refresh token for a fresh access token.
//!
//! Two transports exist for `IAuthenticationService.GenerateAccessTokenForApp`:
//!
//!   1. **CM (protobuf)** — connect to a CM, send
//!      `Authentication.GenerateAccessTokenForApp#1` with
//!      `CAuthentication_AccessToken_GenerateForApp_Request`. This is what
//!      SteamKit does, and it is the primary path here because it needs no
//!      HTTP endpoint and no browser cookie.
//!   2. **HTTP form** — `POST /IAuthenticationService/GenerateAccessTokenForApp/v1/`
//!      with `refresh_token` + `steamid` as `application/x-www-form-urlencoded`.
//!      Watt Toolkit uses this shape; it is kept as a fallback because some
//!      networks reach `api.steampowered.com` more reliably than the CM mesh.
//!
//! P0 field measurement (2026-11-17): the local Steam-client token was rejected
//! by both paths with `AccessDenied` (`x-eresult: 15`), which means that token
//! had been rotated/revoked — not that the endpoint is wrong. The endpoint
//! itself answers, so both transports stay implemented and the caller decides
//! which token to feed them.

use std::time::Duration;

use prost::Message;

use crate::errors::AppError;

const REFRESH_TIMEOUT: Duration = Duration::from_secs(20);
const GENERATE_URL: &str =
    "https://api.steampowered.com/IAuthenticationService/GenerateAccessTokenForApp/v1/";

#[derive(Debug, Clone)]
pub struct TokenPair {
    pub access_token: String,
    /// Present only when Steam decided to renew the refresh token itself.
    pub refresh_token: Option<String>,
}

/// Try CM first, then HTTP. The first transport that yields a non-empty access
/// token wins; both failures are reported together when neither works.
pub async fn refresh_access_token(
    account_name: &str,
    steam_id: &str,
    refresh_token: &str,
    allow_renewal: bool,
) -> Result<TokenPair, AppError> {
    let cm_error = match refresh_via_cm(account_name, steam_id, refresh_token, allow_renewal).await
    {
        Ok(pair) => return Ok(pair),
        Err(e) => e,
    };
    log::warn!("[steam-session] CM refresh failed: {cm_error}");

    match refresh_via_http(steam_id, refresh_token, allow_renewal).await {
        Ok(pair) => Ok(pair),
        Err(http_error) => Err(AppError::network(format!(
            "cm: {cm_error}; http: {http_error}"
        ))),
    }
}

/// Result of checking a refresh token against the HTTP API.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefreshVerify {
    /// The API exchanged the refresh token: the session is alive.
    Valid,
    /// Steam explicitly rejected the token (AccessDenied / 401 / 403).
    Rejected,
    /// Network, timeout or server error: says nothing about the token.
    Unavailable,
}

/// Classify the GenerateAccessTokenForApp response. Pure so the rules are
/// unit-testable without a network.
pub fn classify_http_result(status: u16, eresult: Option<&str>) -> RefreshVerify {
    let eresult = eresult.unwrap_or("-");
    if status == 200 && (eresult == "-" || eresult == "1") {
        RefreshVerify::Valid
    } else if eresult == "15" || status == 401 || status == 403 {
        RefreshVerify::Rejected
    } else {
        RefreshVerify::Unavailable
    }
}

/// Verify a stored refresh token through api.steampowered.com.
///
/// This is the fallback used when steamcommunity.com is unreachable: the
/// API lives on a different host and answers even when the community site is
/// blocked. The returned access token is deliberately discarded - the caller
/// only needs the verdict.
pub async fn verify_refresh_token(steam_id: &str, refresh_token: &str) -> RefreshVerify {
    let client = match reqwest::Client::builder().timeout(REFRESH_TIMEOUT).build() {
        Ok(client) => client,
        Err(e) => {
            log::warn!("[steam-session] api verify client: {e}");
            return RefreshVerify::Unavailable;
        }
    };
    let form = [("refresh_token", refresh_token), ("steamid", steam_id)];
    let response = match client.post(GENERATE_URL).form(&form).send().await {
        Ok(response) => response,
        Err(e) => {
            log::warn!("[steam-session] api verify request failed: {e}");
            return RefreshVerify::Unavailable;
        }
    };
    let status = response.status().as_u16();
    let eresult = response
        .headers()
        .get("x-eresult")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let verdict = classify_http_result(status, eresult.as_deref());
    if verdict == RefreshVerify::Valid {
        let body: serde_json::Value = match response.json().await {
            Ok(body) => body,
            Err(_) => return RefreshVerify::Unavailable,
        };
        let has_token = body
            .pointer("/response/access_token")
            .and_then(|value| value.as_str())
            .map(|value| !value.is_empty())
            .unwrap_or(false);
        if !has_token {
            log::warn!("[steam-session] api verify returned 200 without an access token");
            return RefreshVerify::Rejected;
        }
    }
    log::info!(
        "[steam-session] api verify: status={status} eresult={:?} -> {:?}",
        eresult,
        verdict
    );
    verdict
}

/// CM transport: log on with the refresh token, then call the unified service.
async fn refresh_via_cm(
    account_name: &str,
    steam_id: &str,
    refresh_token: &str,
    allow_renewal: bool,
) -> Result<TokenPair, AppError> {
    let steam_id_u64: u64 = steam_id
        .parse()
        .map_err(|_| AppError::validation(format!("invalid steam id: {steam_id}")))?;

    let client = steamroom_client::login::LoginBuilder::new()
        .device_name("WEaveR")
        .with_refresh_token(account_name, refresh_token)
        .login()
        .await
        .map_err(|e| AppError::network(format!("cm login: {e}")))?;

    let request = steamroom::generated::CAuthenticationAccessTokenGenerateForAppRequest {
        refresh_token: Some(refresh_token.to_string()),
        steamid: Some(steam_id_u64),
        renewal_type: if allow_renewal { Some(1) } else { None },
    };
    let response = client
        .call_service_method(
            "Authentication.GenerateAccessTokenForApp#1",
            &request.encode_to_vec(),
        )
        .await
        .map_err(|e| AppError::network(format!("cm call: {e}")))?;
    let parsed = steamroom::generated::CAuthenticationAccessTokenGenerateForAppResponse::decode(
        &response.body[..],
    )
    .map_err(|e| AppError::network(format!("cm decode: {e}")))?;
    let access_token = parsed.access_token.unwrap_or_default();
    if access_token.is_empty() {
        return Err(AppError::network("cm call returned an empty access token"));
    }
    Ok(TokenPair {
        access_token,
        refresh_token: parsed.refresh_token,
    })
}

/// HTTP transport: form-encoded POST to the Steam Web API.
async fn refresh_via_http(
    steam_id: &str,
    refresh_token: &str,
    allow_renewal: bool,
) -> Result<TokenPair, AppError> {
    let client = reqwest::Client::builder()
        .timeout(REFRESH_TIMEOUT)
        .build()
        .map_err(|e| AppError::network(e.to_string()))?;
    let mut form: Vec<(&str, &str)> = vec![("refresh_token", refresh_token), ("steamid", steam_id)];
    if allow_renewal {
        form.push(("renewal_type", "1"));
    }
    let response = client
        .post(GENERATE_URL)
        .form(&form)
        .send()
        .await
        .map_err(|e| AppError::network(e.to_string()))?;
    let status = response.status();
    let eresult = response
        .headers()
        .get("x-eresult")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("-")
        .to_string();
    let body: serde_json::Value = response
        .json()
        .await
        .map_err(|e| AppError::network(format!("parse response: {e}")))?;
    let access_token = body
        .pointer("/response/access_token")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    if access_token.is_empty() {
        return Err(AppError::network(format!(
            "empty access token (status={status}, x-eresult={eresult})"
        )));
    }
    Ok(TokenPair {
        access_token,
        refresh_token: body
            .pointer("/response/refresh_token")
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .filter(|s| !s.is_empty()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_http_result_covers_valid_rejected_and_unavailable() {
        assert_eq!(classify_http_result(200, None), RefreshVerify::Valid);
        assert_eq!(classify_http_result(200, Some("1")), RefreshVerify::Valid);
        assert_eq!(
            classify_http_result(403, Some("15")),
            RefreshVerify::Rejected
        );
        assert_eq!(classify_http_result(401, None), RefreshVerify::Rejected);
        assert_eq!(classify_http_result(500, None), RefreshVerify::Unavailable);
        assert_eq!(
            classify_http_result(200, Some("2")),
            RefreshVerify::Unavailable
        );
    }

    #[test]
    fn token_pair_keeps_optional_refresh_token() {
        let pair = TokenPair {
            access_token: "a".into(),
            refresh_token: None,
        };
        assert_eq!(pair.access_token, "a");
        assert!(pair.refresh_token.is_none());
    }
}
