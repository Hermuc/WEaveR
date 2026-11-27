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
use crate::errors::AppError;
use state::{SyncStatus, SyncStatusCache};

pub struct SyncService {
    app: AppHandle,
    settings: Arc<parking_lot::RwLock<SettingsService>>,
    handle: Mutex<Option<we_sync::engine::EngineHandle>>,
    /// Kept alive while the engine runs; dropping it stops the watch.
    watcher: Mutex<Option<notify::RecommendedWatcher>>,
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
            watcher: Mutex::new(None),
            cache: Arc::new(Mutex::new(SyncStatusCache::default())),
        }
    }

    /// Start the engine if it is not already running.
    pub fn start(&self) -> Result<(), AppError> {
        let mut guard = self.handle.lock();
        if guard.is_some() {
            return Ok(());
        }
        let cfg = config::load(&self.settings.read());
        let (tx, rx) = std::sync::mpsc::channel();
        let handle = we_sync::engine::spawn(cfg.clone(), tx);
        let dirty = handle.dirty.clone();
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
            .map_err(|e| AppError::internal(format!("spawn sync event thread: {e}")))?;
        *guard = Some(handle);
        self.start_watcher(&cfg, dirty);
        self.cache.lock().set_running(true);
        Ok(())
    }

    /// Stop the engine and let the worker save its state.
    pub fn stop(&self) {
        *self.watcher.lock() = None;
        if let Some(handle) = self.handle.lock().take() {
            handle.quit();
        }
        self.cache.lock().set_running(false);
    }

    /// Watch the workshop directory so a new wallpaper triggers a rescan
    /// immediately instead of waiting for the poll interval.
    fn start_watcher(
        &self,
        cfg: &we_sync::config::AppConfig,
        dirty: Arc<std::sync::atomic::AtomicBool>,
    ) {
        let Some(dir) = we_sync::discover::effective_workshop_dir(cfg) else {
            return;
        };
        if let Some(watcher) = we_sync::engine::spawn_watcher(dir, dirty) {
            *self.watcher.lock() = Some(watcher);
        }
    }

    pub fn status(&self) -> SyncStatus {
        let mut status = self.cache.lock().snapshot();
        status.running = self.handle.lock().is_some();
        status
    }

    pub fn now(&self) -> Result<(), AppError> {
        self.send(we_sync::engine::Cmd::SyncNow)
    }

    pub fn set_paused(&self, paused: bool) -> Result<(), AppError> {
        self.cache.lock().set_paused(paused);
        self.send(we_sync::engine::Cmd::SetPaused(paused))
    }

    pub fn set_transfer_paused(&self, paused: bool) -> Result<(), AppError> {
        self.send(we_sync::engine::Cmd::SetTransferPaused(paused))
    }

    pub fn get_config(&self) -> serde_json::Value {
        serde_json::to_value(config::load(&self.settings.read())).unwrap_or_default()
    }

    pub fn save_config(&self, value: serde_json::Value) -> Result<(), AppError> {
        let parsed: we_sync::config::AppConfig = serde_json::from_value(value)
            .map_err(|e| AppError::validation(format!("invalid sync config: {e}")))?;
        config::save(&self.settings.read(), &parsed)
            .map_err(|e| AppError::config(format!("save sync config: {e}")))?;
        let dirty = {
            let guard = self.handle.lock();
            guard.as_ref().map(|handle| handle.dirty.clone())
        };
        self.send(we_sync::engine::Cmd::SetConfig(Box::new(parsed.clone())))?;
        if let Some(dirty) = dirty {
            // The workshop directory may have changed; recreate the watch.
            *self.watcher.lock() = None;
            self.start_watcher(&parsed, dirty);
        }
        Ok(())
    }

    /// Respect general.sync_on_start when the app boots.
    pub fn auto_start(&self) {
        if config::load(&self.settings.read()).general.sync_on_start {
            if let Err(e) = self.start() {
                log::warn!("[sync] auto start failed: {e}");
            }
        }
    }

    fn send(&self, cmd: we_sync::engine::Cmd) -> Result<(), AppError> {
        let guard = self.handle.lock();
        let handle = guard
            .as_ref()
            .ok_or_else(|| AppError::not_found("sync engine is not running"))?;
        handle
            .tx
            .send(cmd)
            .map_err(|e| AppError::internal(e.to_string()))
    }
}
