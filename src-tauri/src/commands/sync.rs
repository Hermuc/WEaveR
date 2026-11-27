//! Commands for the phone sync feature (we-sync engine).

use std::sync::Arc;

use tauri::State;

use crate::services::sync::state::SyncStatus;
use crate::services::sync::SyncService;

#[tauri::command]
pub fn sync_start(state: State<'_, Arc<SyncService>>) -> Result<(), String> {
    state.start().map_err(String::from)
}

#[tauri::command]
pub fn sync_stop(state: State<'_, Arc<SyncService>>) -> Result<(), String> {
    state.stop();
    Ok(())
}

#[tauri::command]
pub fn sync_status(state: State<'_, Arc<SyncService>>) -> SyncStatus {
    state.status()
}

#[tauri::command]
pub fn sync_now(state: State<'_, Arc<SyncService>>) -> Result<(), String> {
    state.now().map_err(String::from)
}

#[tauri::command]
pub fn sync_pause(state: State<'_, Arc<SyncService>>, paused: bool) -> Result<(), String> {
    state.set_paused(paused).map_err(String::from)
}

#[tauri::command]
pub fn sync_transfer_pause(state: State<'_, Arc<SyncService>>, paused: bool) -> Result<(), String> {
    state.set_transfer_paused(paused).map_err(String::from)
}

#[tauri::command]
pub fn sync_config_get(state: State<'_, Arc<SyncService>>) -> serde_json::Value {
    state.get_config()
}

#[tauri::command]
pub fn sync_config_set(
    state: State<'_, Arc<SyncService>>,
    config: serde_json::Value,
) -> Result<(), String> {
    state.save_config(config).map_err(String::from)
}
