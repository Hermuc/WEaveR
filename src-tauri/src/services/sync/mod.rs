//! Bridge between WEaveR and the we-sync engine.
//!
//! The engine runs on its own thread and communicates through std mpsc, so
//! this module owns: app-data directory setup, engine lifecycle, event
//! forwarding to the webview, and the settings <-> AppConfig mapping.

pub mod config;
pub mod event;
pub mod state;

use std::sync::Arc;

use parking_lot::Mutex;
use tauri::{AppHandle, Emitter};

use crate::config::SettingsService;
use state::{SyncStatus, SyncStatusCache};

pub struct SyncService {
    app: AppHandle,
    settings: Arc<parking_lot::RwLock<SettingsService>>,
    handle: Mutex<Option<we_sync::engine::EngineHandle>>,
    cache: Arc<Mutex<SyncStatusCache>>,
}

impl SyncService {
    pub fn new(
        app: AppHandle,
        settings: Arc<parking_lot::RwLock<SettingsService>>,
        data_dir: std::path::PathBuf,
    ) -> Self {
        let sync_dir = data_dir.join("sync");
        let _ = std::fs::create_dir_all(&sync_dir);
        // Must run before any other we-sync call, otherwise util::app_dir()
        // latches onto the executable directory.
        let _ = we_sync::util::set_app_dir(sync_dir);
        Self {
            app,
            settings,
            handle: Mutex::new(None),
            cache: Arc::new(Mutex::new(SyncStatusCache::default())),
        }
    }

    /// Start the engine if it is not already running.
    pub fn start(&self) -> Result<(), String> {
        let mut guard = self.handle.lock();
        if guard.is_some() {
            return Ok(());
        }
        let cfg = config::load(&self.settings.read());
        let (tx, rx) = std::sync::mpsc::channel();
        let handle = we_sync::engine::spawn(cfg, tx);
        let app = self.app.clone();
        let cache = self.cache.clone();
        std::thread::Builder::new()
            .name("weave-sync-events".into())
            .spawn(move || {
                while let Ok(event) = rx.recv() {
                    let dto = event::to_dto(event);
                    cache.lock().apply(&dto);
                    let _ = app.emit("sync://event", &dto);
                }
            })
            .map_err(|e| format!("spawn sync event thread: {e}"))?;
        *guard = Some(handle);
        self.cache.lock().set_running(true);
        Ok(())
    }

    /// Stop the engine and let the worker save its state.
    pub fn stop(&self) {
        if let Some(handle) = self.handle.lock().take() {
            handle.quit();
        }
        self.cache.lock().set_running(false);
    }

    pub fn status(&self) -> SyncStatus {
        let mut status = self.cache.lock().snapshot();
        status.running = self.handle.lock().is_some();
        status
    }

    pub fn now(&self) -> Result<(), String> {
        self.send(we_sync::engine::Cmd::SyncNow)
    }

    pub fn set_paused(&self, paused: bool) -> Result<(), String> {
        self.cache.lock().set_paused(paused);
        self.send(we_sync::engine::Cmd::SetPaused(paused))
    }

    pub fn set_transfer_paused(&self, paused: bool) -> Result<(), String> {
        self.send(we_sync::engine::Cmd::SetTransferPaused(paused))
    }

    pub fn get_config(&self) -> serde_json::Value {
        serde_json::to_value(config::load(&self.settings.read())).unwrap_or_default()
    }

    pub fn save_config(&self, value: serde_json::Value) -> Result<(), String> {
        let parsed: we_sync::config::AppConfig =
            serde_json::from_value(value).map_err(|e| format!("invalid sync config: {e}"))?;
        config::save(&self.settings.read(), &parsed).map_err(|e| e.to_string())?;
        self.send(we_sync::engine::Cmd::SetConfig(Box::new(parsed)))
    }

    /// Respect general.sync_on_start when the app boots.
    pub fn auto_start(&self) {
        if config::load(&self.settings.read()).general.sync_on_start {
            if let Err(e) = self.start() {
                log::warn!("[sync] auto start failed: {e}");
            }
        }
    }

    fn send(&self, cmd: we_sync::engine::Cmd) -> Result<(), String> {
        let guard = self.handle.lock();
        let handle = guard.as_ref().ok_or("sync engine is not running")?;
        handle.tx.send(cmd).map_err(|e| e.to_string())
    }
}
