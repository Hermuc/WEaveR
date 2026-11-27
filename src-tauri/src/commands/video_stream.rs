//! Commands backing the "watch while downloading" feature.
//!
//! The expensive part (Steam login + manifest resolve) is done in the background
//! as soon as the user opens a wallpaper's details (`video_stream_prewarm`), and
//! the resolved video is cached. `video_stream_start` then only starts the local
//! range server and launches the player, so clicking "watch" is near-instant.

use std::sync::Arc;

use parking_lot::Mutex;
use serde::Serialize;
use tauri::State;

use crate::commands::AppStateHandle;
use crate::services::video_player;
use crate::services::video_server::{self, VideoServer};
use crate::services::video_stream::{self, ResolvedVideo};

type ReadyMap = std::collections::HashMap<u64, Arc<ResolvedVideo>>;

/// Background resolution state for one item.
#[derive(Clone)]
enum PrewarmState {
    Pending { started: std::time::Instant },
    Ready,
    Failed { message: String },
}

/// Live streams plus pre-resolved videos, keyed by workshop item id.
#[derive(Default)]
pub struct StreamRegistry {
    servers: Mutex<std::collections::HashMap<u64, VideoServer>>,
    /// External players started for this stream, so shutdown can kill them.
    players: Mutex<std::collections::HashMap<u64, Arc<Mutex<Option<std::process::Child>>>>>,
    ready: Mutex<ReadyMap>,
    /// Prewarm lifecycle per item. This replaces the old "pending set" so the
    /// UI can distinguish "still resolving", "ready" and "failed with a reason"
    /// instead of only learning about failures when the user clicks watch.
    state: Mutex<std::collections::HashMap<u64, PrewarmState>>,
}

impl StreamRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Stop every range server and kill every external player.
    ///
    /// Called from the shutdown coordinator before destroying the windows; it
    /// must be safe to call more than once and on an empty registry.
    pub fn shutdown_all(&self) {
        {
            let mut servers = self.servers.lock();
            for (_, mut server) in servers.drain() {
                server.stop();
            }
        }
        {
            let mut players = self.players.lock();
            for (_, child) in players.drain() {
                if let Some(mut child) = child.lock().take() {
                    let _ = child.kill();
                }
            }
        }
    }
}

/// How long `video_stream_start` waits for an in-flight prewarm before falling
/// back to an inline resolve. Kept generous because the first manifest fetch on
/// a slow network can legitimately take a minute; the point is to avoid a
/// second concurrent resolve, not to cut the wait short.
const PREWARM_WAIT_MAX: std::time::Duration = std::time::Duration::from_secs(120);
const PREWARM_POLL: std::time::Duration = std::time::Duration::from_millis(200);

#[derive(Debug, Serialize)]
pub struct PlayerInfo {
    pub name: String,
    pub path: String,
}

#[derive(Debug, Serialize)]
pub struct StreamStarted {
    pub item_id: u64,
    pub url: String,
    pub filename: String,
    pub total_size: u64,
    pub player: String,
}

/// Report the detected external player, if any. A configured path wins over
/// auto-detection; a stale path falls back to the usual PATH scan.
#[tauri::command]
pub fn video_stream_player(state: AppStateHandle<'_>) -> Option<PlayerInfo> {
    let preferred = state
        .settings
        .read()
        .get("video_stream.player_path")
        .and_then(|value| value.as_str().map(str::to_owned));
    video_player::detect_with(preferred.as_deref()).map(|p| PlayerInfo {
        name: p.kind.name().to_string(),
        path: p.path.to_string_lossy().to_string(),
    })
}

/// Report video-cache usage for the settings dialog.
#[tauri::command]
pub fn video_stream_cache_stats() -> video_stream::VideoCacheStats {
    video_stream::cache_stats()
}

/// Remove every cached chunk (keeps the directory structure).
#[tauri::command]
pub fn video_stream_cache_clear() -> Result<(), String> {
    video_stream::clear_cache().map_err(|e| e.to_string())
}

/// Resolve a video in the background and cache it, so a later "watch" click is
/// instant. Safe to call repeatedly; concurrent calls for the same item dedupe.
#[tauri::command]
pub async fn video_stream_prewarm(
    state: AppStateHandle<'_>,
    registry: State<'_, Arc<StreamRegistry>>,
    item_id: u64,
) -> Result<(), String> {
    if registry.ready.lock().contains_key(&item_id) {
        return Ok(());
    }
    {
        let mut prewarm = registry.state.lock();
        if matches!(prewarm.get(&item_id), Some(PrewarmState::Pending { .. })) {
            return Ok(()); // already resolving
        }
        prewarm.insert(
            item_id,
            PrewarmState::Pending {
                started: std::time::Instant::now(),
            },
        );
    }
    let reg = registry.inner().clone();
    let creds = state.accounts.preferred_parser_credentials();
    let session_refresh_token = state
        .accounts
        .session_for(&creds.username)
        .map(|session| session.refresh_token);
    tauri::async_runtime::spawn(async move {
        let result = resolve_with_retry(&creds, session_refresh_token.as_deref(), item_id).await;
        match result {
            Ok(video) => {
                // Browsing many wallpapers must not pin a resolved video (and
                // its manifest) per item forever. Keep only the most recent
                // few; the rest re-resolve from the on-disk layout cache.
                const MAX_READY: usize = 4;
                let mut ready = reg.ready.lock();
                if ready.len() >= MAX_READY {
                    if let Some(oldest) = ready.keys().next().copied() {
                        ready.remove(&oldest);
                    }
                }
                ready.insert(item_id, Arc::new(video));
                drop(ready);
                reg.state.lock().insert(item_id, PrewarmState::Ready);
                log::info!("[video-stream] prewarm ready for {item_id}");
            }
            Err(e) => {
                log::warn!("[video-stream] prewarm failed for {item_id}: {e}");
                reg.state.lock().insert(
                    item_id,
                    PrewarmState::Failed {
                        message: e.to_string(),
                    },
                );
            }
        }
    });
    Ok(())
}

/// Report the prewarm lifecycle for an item so the UI can show
/// "preparing"/"ready"/"failed" instead of a bare spinner.
#[tauri::command]
pub fn video_stream_status(
    registry: State<'_, Arc<StreamRegistry>>,
    item_id: u64,
) -> VideoStreamStatus {
    if registry.ready.lock().contains_key(&item_id) {
        return VideoStreamStatus {
            state: "ready".into(),
            error: None,
        };
    }
    match registry.state.lock().get(&item_id).cloned() {
        Some(PrewarmState::Pending { .. }) => VideoStreamStatus {
            state: "pending".into(),
            error: None,
        },
        Some(PrewarmState::Failed { message }) => VideoStreamStatus {
            state: "failed".into(),
            error: Some(message),
        },
        Some(PrewarmState::Ready) => VideoStreamStatus {
            state: "ready".into(),
            error: None,
        },
        None => VideoStreamStatus {
            state: "idle".into(),
            error: None,
        },
    }
}

#[derive(Debug, Serialize)]
pub struct VideoStreamStatus {
    pub state: String,
    pub error: Option<String>,
}

async fn resolve_with_retry(
    creds: &crate::services::accounts::AccountCredentials,
    session_refresh_token: Option<&str>,
    item_id: u64,
) -> anyhow::Result<ResolvedVideo> {
    if creds.username.is_empty() {
        return Err(anyhow::anyhow!("NO_ACCOUNT"));
    }
    let client =
        video_stream::shared_login(&creds.username, &creds.password, session_refresh_token).await?;
    match video_stream::resolve_video(&client, item_id).await {
        Ok(v) => Ok(v),
        Err(e) => {
            log::warn!("[video-stream] resolve on cached session failed ({e}); fresh login");
            video_stream::forget_session();
            let fresh =
                video_stream::login(&creds.username, &creds.password, session_refresh_token)
                    .await?;
            video_stream::resolve_video(&fresh, item_id).await
        }
    }
}

/// Start the range server and launch the player. Uses the prewarmed video when
/// available; otherwise resolves inline (first-ever click).
#[tauri::command]
pub async fn video_stream_start(
    state: AppStateHandle<'_>,
    registry: State<'_, Arc<StreamRegistry>>,
    item_id: u64,
) -> Result<StreamStarted, String> {
    let player = video_player::detect().ok_or_else(|| "NO_PLAYER".to_string())?;
    let creds = state.accounts.preferred_parser_credentials();
    if creds.username.is_empty() {
        return Err("NO_ACCOUNT".into());
    }

    // Stop an existing stream for this item first (idempotent re-click).
    {
        let mut servers = registry.servers.lock();
        if let Some(mut old) = servers.remove(&item_id) {
            old.stop();
        }
    }

    // Take the prewarmed video under a short-lived lock so the guard is dropped
    // before any await (parking_lot guards are not Send).
    //
    // If a prewarm is still running, WAIT for it instead of resolving again.
    // The old code started a second full login + manifest fetch in parallel,
    // which doubled the network work and often made the click slower than the
    // prewarm it was supposed to benefit from.
    let mut prewarmed = registry.ready.lock().remove(&item_id);
    let wait_started = std::time::Instant::now();
    while prewarmed.is_none() {
        if let Some(v) = registry.ready.lock().remove(&item_id) {
            prewarmed = Some(v);
            break;
        }
        let prewarm = registry.state.lock().get(&item_id).cloned();
        match prewarm {
            Some(PrewarmState::Pending { started }) => {
                if wait_started.elapsed() >= PREWARM_WAIT_MAX
                    || started.elapsed() >= PREWARM_WAIT_MAX
                {
                    log::warn!(
                        "[video-stream] prewarm for {item_id} still pending after {:?}; resolving inline",
                        wait_started.elapsed()
                    );
                    break;
                }
                tokio::time::sleep(PREWARM_POLL).await;
            }
            Some(PrewarmState::Failed { message }) => {
                log::warn!(
                    "[video-stream] prewarm for {item_id} failed: {message}; resolving inline"
                );
                break;
            }
            _ => break,
        }
    }
    let session_refresh_token = state
        .accounts
        .session_for(&creds.username)
        .map(|session| session.refresh_token);
    let video = match prewarmed {
        Some(v) => v,
        None => Arc::new(
            resolve_with_retry(&creds, session_refresh_token.as_deref(), item_id)
                .await
                .map_err(|e| e.to_string())?,
        ),
    };

    let info = video.info.clone();
    let server = video_server::serve_arc(video)
        .await
        .map_err(|e| e.to_string())?;
    let url = server.url();
    let port = server.port;
    let child = Arc::new(Mutex::new(Some(
        video_player::launch(&player, &url).map_err(|e| format!("launch failed: {e}"))?,
    )));
    let player_name = player.kind.name().to_string();
    registry.servers.lock().insert(item_id, server);
    registry.players.lock().insert(item_id, child.clone());

    // The player owns the WebView-like lifecycle now: when mpv/VLC exits, stop
    // the range server so its prefetcher stops downloading the rest of the file
    // in the background. Match on port so a re-click (which replaced the server)
    // cannot stop the newer stream by accident.
    let registry_for_wait = registry.inner().clone();
    let child_for_wait = child.clone();
    let child_for_cleanup = child.clone();
    tauri::async_runtime::spawn(async move {
        let status = tokio::task::spawn_blocking(move || {
            let mut guard = child_for_wait.lock();
            guard.take().map(|mut child| child.wait())
        })
        .await;
        log::info!("[video-stream] player exited for {item_id}: {status:?}");
        {
            let mut players = registry_for_wait.players.lock();
            let same = players
                .get(&item_id)
                .map(|entry| std::sync::Arc::ptr_eq(entry, &child_for_cleanup))
                .unwrap_or(false);
            if same {
                players.remove(&item_id);
            }
        }
        let mut servers = registry_for_wait.servers.lock();
        if servers.get(&item_id).map(|s| s.port) == Some(port) {
            if let Some(mut s) = servers.remove(&item_id) {
                s.stop();
                log::info!("[video-stream] stopped range server for {item_id}");
            }
        }
    });

    Ok(StreamStarted {
        item_id,
        url,
        filename: info.filename,
        total_size: info.total_size,
        player: player_name,
    })
}

/// Stop the stream for one item.
#[tauri::command]
pub fn video_stream_stop(registry: State<'_, Arc<StreamRegistry>>, item_id: u64) -> bool {
    let stopped = {
        let mut servers = registry.servers.lock();
        match servers.remove(&item_id) {
            Some(mut s) => {
                s.stop();
                true
            }
            None => false,
        }
    };
    if let Some(child) = registry.players.lock().remove(&item_id) {
        if let Some(mut child) = child.lock().take() {
            let _ = child.kill();
        }
    }
    stopped
}

/// Stop every running stream (used on shutdown).
#[tauri::command]
pub fn video_stream_stop_all(registry: State<'_, Arc<StreamRegistry>>) -> usize {
    let n = {
        let mut servers = registry.servers.lock();
        let n = servers.len();
        for (_, mut s) in servers.drain() {
            s.stop();
        }
        n
    };
    let mut players = registry.players.lock();
    for (_, child) in players.drain() {
        if let Some(mut child) = child.lock().take() {
            let _ = child.kill();
        }
    }
    n
}
