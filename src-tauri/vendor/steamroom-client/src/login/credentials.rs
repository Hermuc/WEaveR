use crate::login::BuilderConfig;
use crate::login::STEAM_CLIENT_PLATFORM_TYPE;
use crate::login::TransportConfig;
use crate::login::error::LoginError;
use crate::login::establish_ready_client;
use crate::login::ReconnectHint;
use crate::login::terminal::ApprovedAuth;

use base64::Engine;
use steamroom::auth::GuardType;
use steamroom::client::Ready;
use steamroom::client::SteamClient;
use steamroom::generated::CAuthenticationBeginAuthSessionViaCredentialsRequest;

/// Configured credentials login. Call [`begin()`] to start the auth flow.
///
/// [`begin()`]: CredentialsLogin::begin
pub struct CredentialsLogin {
    pub(crate) config: BuilderConfig,
    pub(crate) transport: TransportConfig,
    pub(crate) account_name: String,
    pub(crate) password: String,
}

/// State of a credentials login after `begin()`.
#[non_exhaustive]
pub enum CredentialsLoginFlow {
    /// No 2FA required. Call `ApprovedAuth::finish()` to complete the logon.
    Approved(ApprovedAuth),
    /// Steam Guard code required. Call `GuardChallenge::submit_code()`.
    NeedsGuardCode(GuardChallenge),
    /// Mobile-app confirmation pending. Call
    /// `MobileChallenge::wait_for_confirmation()`.
    NeedsMobileConfirm(MobileChallenge),
}

pub struct GuardChallenge {
    pub(crate) client: SteamClient<Ready>,
    pub(crate) config: BuilderConfig,
    pub(crate) client_id: u64,
    pub(crate) steam_id: u64,
    pub(crate) request_id: Vec<u8>,
    pub(crate) poll_interval: f32,
    pub(crate) allowed_kinds: Vec<GuardType>,
    /// How to rebuild the CM connection if Steam drops it mid-wait.
    pub(crate) reconnect: Option<ReconnectHint>,
}

pub struct MobileChallenge {
    pub(crate) client: SteamClient<Ready>,
    pub(crate) config: BuilderConfig,
    pub(crate) client_id: u64,
    pub(crate) request_id: Vec<u8>,
    pub(crate) poll_interval: f32,
    /// How to rebuild the CM connection if Steam drops it mid-wait.
    pub(crate) reconnect: Option<ReconnectHint>,
}

impl CredentialsLogin {
    /// Connect (or accept BYO client), fetch the RSA pubkey, RSA-encrypt the
    /// password, and call `BeginAuthSessionViaCredentials`. The returned flow
    /// indicates whether 2FA is needed and which kind.
    pub async fn begin(self) -> Result<CredentialsLoginFlow, LoginError> {
        // Take the reconnect recipe before the transport is consumed: the
        // mobile/QR waits below can run for minutes, which is long enough for
        // Steam to reap the idle CM socket.
        let reconnect = self.transport.reconnect_hint();
        let client = establish_ready_client(self.transport).await?;

        // RSA exchange
        let rsa = client
            .get_password_rsa_public_key(&self.account_name)
            .await?;
        let modulus = rsa
            .publickey_mod
            .ok_or(LoginError::MissingField("publickey_mod"))?;
        let exponent = rsa
            .publickey_exp
            .ok_or(LoginError::MissingField("publickey_exp"))?;
        let timestamp = rsa.timestamp.unwrap_or(0);

        let encrypted_password = steamroom::crypto::rsa::encrypt_with_rsa_public_key(
            self.password.as_bytes(),
            &modulus,
            &exponent,
        )
        .map_err(|e| LoginError::Transport(e.into()))?;
        let encoded_password =
            base64::engine::general_purpose::STANDARD.encode(&encrypted_password);

        // Begin auth session
        let req = CAuthenticationBeginAuthSessionViaCredentialsRequest {
            account_name: Some(self.account_name.clone()),
            encrypted_password: Some(encoded_password),
            encryption_timestamp: Some(timestamp),
            remember_login: Some(true),
            persistence: Some(1),
            platform_type: Some(STEAM_CLIENT_PLATFORM_TYPE),
            device_friendly_name: self.config.device_name.clone(),
            ..Default::default()
        };
        let session = match client.begin_auth_session_via_credentials(req).await {
            Ok(s) => s,
            Err(steamroom::Error::Connection(
                steamroom::error::ConnectionError::ServiceMethodFailed(
                    steamroom::enums::EResultError::InvalidPassword,
                ),
            )) => return Err(LoginError::InvalidPassword),
            Err(e) => return Err(LoginError::Transport(e)),
        };

        let client_id = session
            .client_id
            .ok_or(LoginError::MissingField("client_id"))?;
        let request_id = session
            .request_id
            .ok_or(LoginError::MissingField("request_id"))?;
        let steam_id = session
            .steam_id
            .ok_or(LoginError::MissingField("steamid"))?;
        let poll_interval = session.poll_interval.unwrap_or(5.0);

        // Classify the next step.
        if session.allowed_confirmations.is_empty()
            || session.allowed_confirmations.contains(&GuardType::None)
        {
            // No 2FA needed — poll once for tokens (Steam may have them ready)
            // and produce ApprovedAuth directly.
            let (tokens, client) =
                poll_until_tokens(client, client_id, &request_id, poll_interval).await?;
            return Ok(CredentialsLoginFlow::Approved(ApprovedAuth {
                client,
                config: self.config,
                tokens,
            }));
        }

        let needs_code = session
            .allowed_confirmations
            .iter()
            .any(|g| matches!(g, GuardType::DeviceCode | GuardType::EmailCode));
        let can_confirm_on_mobile = session
            .allowed_confirmations
            .contains(&GuardType::DeviceConfirmation);

        // WEave patch: prefer the mobile-app confirmation when Steam offers
        // BOTH it and a code.
        //
        // Upstream checked `needs_code` first, so a device that accepts either
        // was always routed to the code prompt. The user then approved on their
        // phone anyway, which completed the session server-side; submitting the
        // code afterwards hit `DuplicateRequest` and failed. Waiting for the
        // tap is both the path the user expects and the one that cannot race.
        if can_confirm_on_mobile {
            return Ok(CredentialsLoginFlow::NeedsMobileConfirm(MobileChallenge {
                client,
                reconnect: reconnect.clone(),
                config: self.config,
                client_id,
                request_id,
                poll_interval,
            }));
        }

        if needs_code {
            return Ok(CredentialsLoginFlow::NeedsGuardCode(GuardChallenge {
                client,
                reconnect: reconnect.clone(),
                config: self.config,
                client_id,
                steam_id,
                request_id,
                poll_interval,
                allowed_kinds: session.allowed_confirmations,
            }));
        }

        // Mobile confirmation (DeviceConfirmation only)
        Ok(CredentialsLoginFlow::NeedsMobileConfirm(MobileChallenge {
            client,
            reconnect: reconnect.clone(),
            config: self.config,
            client_id,
            request_id,
            poll_interval,
        }))
    }
}

impl GuardChallenge {
    /// Which kinds of Steam Guard code Steam is willing to accept.
    pub fn allowed_kinds(&self) -> &[GuardType] {
        &self.allowed_kinds
    }

    /// Submit a Steam Guard code, then poll for tokens.
    ///
    /// On `LoginError::InvalidGuardCode`, the challenge is returned unchanged
    /// so the caller can prompt for a new code without restarting the RSA
    /// exchange. On any other error, the session is dead.
    pub async fn submit_code(
        self,
        code: &str,
        kind: GuardType,
    ) -> Result<ApprovedAuth, (GuardChallenge, LoginError)> {
        // Destructure up front. The poll below consumes the client, and on
        // failure the caller needs the challenge back to retype the code —
        // holding the fields separately is what makes that return possible.
        let GuardChallenge {
            client,
            config,
            client_id,
            steam_id,
            request_id,
            poll_interval,
            allowed_kinds,
            reconnect,
        } = self;

        // A rejected code is recoverable: rebuild the challenge and let the
        // caller prompt again. The client is cloned (cheap `Arc`) so it is
        // still available for the rebuild.
        let rebuild = |client: SteamClient<Ready>| GuardChallenge {
            client,
            config: config.clone(),
            client_id,
            steam_id,
            request_id: request_id.clone(),
            poll_interval,
            allowed_kinds: allowed_kinds.clone(),
            reconnect: reconnect.clone(),
        };

        match client
            .submit_steam_guard_code(client_id, steam_id, code, kind)
            .await
        {
            Ok(()) => {}
            Err(steamroom::Error::Connection(
                steamroom::error::ConnectionError::ServiceMethodFailed(
                    steamroom::enums::EResultError::TwoFactorCodeMismatch,
                ),
            )) => return Err((rebuild(client), LoginError::InvalidGuardCode)),
            Err(e) => return Err((rebuild(client), LoginError::Transport(e))),
        }

        // The guard code is already accepted; from here on we are only
        // waiting for Steam to hand back tokens, so a dropped connection is
        // worth reconnecting for.
        //
        // The poll consumes the client, so keep a handle for the error path:
        // the caller needs the challenge back to retype the code. `clone` is a
        // cheap `Arc` bump, and on the reconnect path the poll returns the
        // fresh connection in its `Ok`.
        let client_for_retry = client.clone();
        let reconnect_for_retry = reconnect.clone();
        match poll_until_tokens_with_policy(
            client,
            client_id,
            &request_id,
            PollPolicy::from_interval(poll_interval),
            reconnect,
        )
        .await
        {
            Ok((tokens, client)) => Ok(ApprovedAuth {
                client,
                config,
                tokens,
            }),
            Err(e) => Err((
                GuardChallenge {
                    client: client_for_retry,
                    config,
                    client_id,
                    steam_id,
                    request_id,
                    poll_interval,
                    allowed_kinds,
                    reconnect: reconnect_for_retry,
                },
                e,
            )),
        }
    }
}

impl MobileChallenge {
    /// Poll `PollAuthSessionStatus` until the user approves on their mobile
    /// app and tokens are returned.
    pub async fn wait_for_confirmation(self) -> Result<ApprovedAuth, LoginError> {
        // This is the long wait: the user has to pick up their phone. Steam
        // routinely drops the idle CM socket during it, so reconnect rather
        // than failing the whole sign-in on the first blip.
        let (tokens, client) = poll_until_tokens_with_policy(
            self.client,
            self.client_id,
            &self.request_id,
            PollPolicy::from_interval(self.poll_interval),
            self.reconnect.clone(),
        )
        .await?;
        Ok(ApprovedAuth {
            client,
            config: self.config,
            tokens,
        })
    }
}

/// How long to keep asking Steam whether the user approved the sign-in.
///
/// This used to be an unbounded `loop`, which had two failure modes: a dropped
/// CM connection surfaced as an immediate error, and a connection that was
/// silently black-holed left the UI spinning forever with no way out. The
/// policy below gives the wait an upper bound and a retry budget.
#[derive(Debug, Clone, Copy)]
pub(crate) struct PollPolicy {
    /// How often to ask (Steam supplies this, usually 5s).
    pub interval: std::time::Duration,
    /// Total wall-clock budget for the whole confirmation wait.
    pub deadline: std::time::Duration,
    /// How often to poke the CM so it does not reap an idle connection.
    pub heartbeat: std::time::Duration,
    /// Consecutive transport failures tolerated before giving up.
    pub max_transport_errors: u32,
}

impl PollPolicy {
    /// Budget for a sign-in the user must physically approve on a phone.
    ///
    /// Deliberately generous: the user has to find the device, unlock it and
    /// tap through. Failing earlier turns a slow human into a fake network
    /// error.
    pub(crate) const HUMAN_CONFIRMATION: Self = Self {
        interval: std::time::Duration::from_secs(5),
        deadline: std::time::Duration::from_secs(300),
        heartbeat: std::time::Duration::from_secs(30),
        max_transport_errors: 5,
    };

    /// Build a policy from the interval Steam returned, keeping our deadline.
    pub(crate) fn from_interval(interval_secs: f32) -> Self {
        let base = Self::HUMAN_CONFIRMATION;
        // Steam has been seen to return 0 or a wild value; clamp so the loop
        // can neither spin nor sleep past the deadline.
        let secs = if interval_secs.is_finite() && interval_secs >= 1.0 {
            interval_secs.min(30.0)
        } else {
            5.0
        };
        Self {
            interval: std::time::Duration::from_secs_f32(secs),
            ..base
        }
    }

    /// Backoff applied after a transport error, capped so a flapping CM is
    /// not hammered and the deadline is still honoured.
    fn backoff(&self, attempt: u32) -> std::time::Duration {
        let millis = 500u64.saturating_mul(1u64 << attempt.min(4));
        std::time::Duration::from_millis(millis.min(8_000))
    }
}

/// Whether an error means "the socket died" (retryable) rather than "Steam
/// said no" (terminal).
///
/// Retrying a business error would either spin forever or mask the real cause,
/// so only genuine transport faults are treated as recoverable.
fn is_retryable_transport(error: &steamroom::Error) -> bool {
    match error {
        steamroom::Error::Connection(inner) => matches!(
            inner,
            steamroom::error::ConnectionError::Disconnected
                | steamroom::error::ConnectionError::Io(_)
                | steamroom::error::ConnectionError::BadMagic(_)
                | steamroom::error::ConnectionError::EncryptionFailed
                | steamroom::error::ConnectionError::PacketTooShort { .. }
        ),
        steamroom::Error::Io(_) => true,
        _ => false,
    }
}

/// Poll `PollAuthSessionStatus` until tokens are returned.
///
/// Convenience wrapper that uses the default human-confirmation budget and no
/// reconnect hook. Callers that can rebuild a connection should use
/// [`poll_until_tokens_with_policy`] directly.
pub(crate) async fn poll_until_tokens(
    client: SteamClient<Ready>,
    client_id: u64,
    request_id: &[u8],
    interval_secs: f32,
) -> Result<(steamroom::auth::AuthTokens, SteamClient<Ready>), LoginError> {
    poll_until_tokens_with_policy(
        client,
        client_id,
        request_id,
        PollPolicy::from_interval(interval_secs),
        None,
    )
    .await
}

/// Poll with an explicit policy and an optional reconnect hook.
///
/// `reconnect` is consulted after a retryable transport error. The auth
/// session lives server-side keyed by `client_id` / `request_id`, so a fresh
/// CM connection can keep polling the same session — this is what lets a
/// sign-in survive the CM dropping an idle socket mid-confirmation.
///
/// The client is taken by value (it is a cheap `Arc` clone) and swapped for a
/// freshly connected one on recovery, so the caller always gets back the live
/// connection rather than a stale handle.
pub(crate) async fn poll_until_tokens_with_policy(
    client: SteamClient<Ready>,
    client_id: u64,
    request_id: &[u8],
    policy: PollPolicy,
    reconnect: Option<ReconnectHint>,
) -> Result<(steamroom::auth::AuthTokens, SteamClient<Ready>), LoginError> {
    let started = std::time::Instant::now();
    let mut current = client;
    let mut transport_errors = 0u32;
    let mut last_heartbeat = std::time::Instant::now();

    loop {
        let elapsed = started.elapsed();
        if elapsed >= policy.deadline {
            return Err(LoginError::ConfirmationTimeout { elapsed });
        }

        // Sleep only until the next poll, never past the deadline, so the
        // timeout is honoured even when Steam hands us a long interval.
        let remaining = policy.deadline.saturating_sub(elapsed);
        tokio::time::sleep(policy.interval.min(remaining)).await;

        // Keep the CM socket warm. Steam reaps connections it considers idle,
        // and this wait can last minutes while the user finds their phone. A
        // failed heartbeat is not fatal — the poll below observes the real
        // state of the socket and the retry path handles a dead one.
        if last_heartbeat.elapsed() >= policy.heartbeat {
            let _ = current
                .send_msg(&steamroom::client::msg::ClientMsg::with_body(
                    steamroom::messages::EMsg::CLIENT_HEART_BEAT,
                    &[],
                ))
                .await;
            last_heartbeat = std::time::Instant::now();
        }

        match current.poll_auth_session(client_id, request_id).await {
            Ok(Some(tokens)) => return Ok((tokens, current)),
            // Nobody has approved yet — the normal case, not an error.
            Ok(None) => {
                transport_errors = 0;
            }
            Err(error) if is_retryable_transport(&error) => {
                transport_errors += 1;
                tracing::warn!(
                    "poll_auth_session transport error {}/{}: {error}",
                    transport_errors,
                    policy.max_transport_errors
                );
                if transport_errors >= policy.max_transport_errors {
                    return Err(LoginError::ConnectionLost {
                        last_error: error.to_string(),
                    });
                }
                tokio::time::sleep(policy.backoff(transport_errors)).await;
                let Some(hint) = reconnect.as_ref() else {
                    continue;
                };
                match hint.connect().await {
                    Ok(fresh) => {
                        tracing::info!("reconnected to CM; resuming auth poll");
                        current = fresh;
                        // The new socket starts with a clean slate; carrying
                        // the old count over would let a few blips on a
                        // healthy connection trip the give-up threshold.
                        transport_errors = 0;
                        last_heartbeat = std::time::Instant::now();
                    }
                    Err(reconnect_error) => {
                        tracing::warn!("CM reconnect failed: {reconnect_error}");
                    }
                }
            }
            // Steam answered with a business error (DuplicateRequest, bad code,
            // …). Retrying cannot help and would hide the real reason.
            Err(error) => return Err(LoginError::Transport(error)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn policy_clamps_a_zero_or_wild_interval() {
        // Steam has been observed to send nonsense; a 0 would busy-loop and a
        // huge value would overshoot the deadline.
        assert_eq!(
            PollPolicy::from_interval(0.0).interval,
            std::time::Duration::from_secs(5)
        );
        assert_eq!(
            PollPolicy::from_interval(f32::NAN).interval,
            std::time::Duration::from_secs(5)
        );
        assert_eq!(
            PollPolicy::from_interval(120.0).interval,
            std::time::Duration::from_secs(30)
        );
        assert_eq!(
            PollPolicy::from_interval(6.0).interval,
            std::time::Duration::from_secs(6)
        );
    }

    #[test]
    fn policy_keeps_the_human_confirmation_budget() {
        // Five minutes is deliberate: the user has to find and unlock a phone.
        let policy = PollPolicy::from_interval(5.0);
        assert_eq!(policy.deadline, std::time::Duration::from_secs(300));
        assert_eq!(policy.heartbeat, std::time::Duration::from_secs(30));
        assert_eq!(policy.max_transport_errors, 5);
    }

    #[test]
    fn backoff_grows_then_caps() {
        let policy = PollPolicy::from_interval(5.0);
        let first = policy.backoff(1);
        let second = policy.backoff(2);
        assert!(second > first, "backoff must grow");
        // Must never exceed the cap, otherwise the deadline does the work.
        assert!(policy.backoff(99) <= std::time::Duration::from_secs(8));
    }

    #[test]
    fn only_socket_level_errors_are_retried() {
        // A dead socket is worth another try…
        assert!(is_retryable_transport(&steamroom::Error::Connection(
            steamroom::error::ConnectionError::Disconnected
        )));
        assert!(is_retryable_transport(&steamroom::Error::Connection(
            steamroom::error::ConnectionError::EncryptionFailed
        )));
        // …but Steam saying "no" is terminal: retrying would spin forever or
        // mask the real reason (this is the DuplicateRequest trap).
        assert!(!is_retryable_transport(&steamroom::Error::Connection(
            steamroom::error::ConnectionError::ServiceMethodFailed(
                steamroom::enums::EResultError::DuplicateRequest
            )
        )));
        assert!(!is_retryable_transport(&steamroom::Error::Connection(
            steamroom::error::ConnectionError::LogonFailed(
                steamroom::enums::EResultError::AccessDenied
            )
        )));
    }
}
