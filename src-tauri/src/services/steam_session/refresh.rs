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
) -> Result<TokenPair, String> {
    let cm_error = match refresh_via_cm(account_name, steam_id, refresh_token, allow_renewal).await
    {
        Ok(pair) => return Ok(pair),
        Err(e) => e,
    };
    log::warn!("[steam-session] CM refresh failed: {cm_error}");

    match refresh_via_http(steam_id, refresh_token, allow_renewal).await {
        Ok(pair) => Ok(pair),
        Err(http_error) => Err(format!("cm: {cm_error}; http: {http_error}")),
    }
}

/// CM transport: log on with the refresh token, then call the unified service.
async fn refresh_via_cm(
    account_name: &str,
    steam_id: &str,
    refresh_token: &str,
    allow_renewal: bool,
) -> Result<TokenPair, String> {
    let steam_id_u64: u64 = steam_id
        .parse()
        .map_err(|_| format!("invalid steam id: {steam_id}"))?;

    let client = steamroom_client::login::LoginBuilder::new()
        .device_name("WEaveR")
        .with_refresh_token(account_name, refresh_token)
        .login()
        .await
        .map_err(|e| format!("cm login: {e}"))?;

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
        .map_err(|e| format!("cm call: {e}"))?;
    let parsed = steamroom::generated::CAuthenticationAccessTokenGenerateForAppResponse::decode(
        &response.body[..],
    )
    .map_err(|e| format!("cm decode: {e}"))?;
    let access_token = parsed.access_token.unwrap_or_default();
    if access_token.is_empty() {
        return Err("cm call returned an empty access token".to_string());
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
) -> Result<TokenPair, String> {
    let client = reqwest::Client::builder()
        .timeout(REFRESH_TIMEOUT)
        .build()
        .map_err(|e| e.to_string())?;
    let mut form: Vec<(&str, &str)> = vec![("refresh_token", refresh_token), ("steamid", steam_id)];
    if allow_renewal {
        form.push(("renewal_type", "1"));
    }
    let response = client
        .post(GENERATE_URL)
        .form(&form)
        .send()
        .await
        .map_err(|e| e.to_string())?;
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
        .map_err(|e| format!("parse response: {e}"))?;
    let access_token = body
        .pointer("/response/access_token")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    if access_token.is_empty() {
        return Err(format!(
            "empty access token (status={status}, x-eresult={eresult})"
        ));
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
    fn token_pair_keeps_optional_refresh_token() {
        let pair = TokenPair {
            access_token: "a".into(),
            refresh_token: None,
        };
        assert_eq!(pair.access_token, "a");
        assert!(pair.refresh_token.is_none());
    }
}
