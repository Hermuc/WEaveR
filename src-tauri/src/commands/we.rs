use std::path::PathBuf;

use tauri::command;

use crate::commands::{map_err, AppStateHandle};
use crate::we_client::{InstalledWallpaper, WallpaperEngineClient};

#[command]
pub fn we_get_directory(state: AppStateHandle<'_>) -> Option<String> {
    state
        .we_client
        .read()
        .directory
        .as_ref()
        .map(|p| p.to_string_lossy().to_string())
}

#[command]
pub fn we_set_directory(state: AppStateHandle<'_>, path: String) -> Result<(), String> {
    let dir = PathBuf::from(&path);
    if !dir.join("wallpaper64.exe").exists() && !dir.join("wallpaper32.exe").exists() {
        return Err("Not a Wallpaper Engine directory".into());
    }
    state.we_client.write().set_directory(Some(dir));
    state.settings.read().set_directory(&path);
    Ok(())
}

/// List installed wallpapers.
///
/// Scanning walks three directories and parses every project.json, which is
/// slow enough to freeze the UI when run on the main thread (Tauri executes
/// synchronous commands there, so the window cannot even repaint). The scan
/// therefore runs on a blocking worker and the result is returned asynchronously.
#[command]
pub async fn we_list_installed(
    state: AppStateHandle<'_>,
    force: Option<bool>,
) -> Result<Vec<InstalledWallpaper>, String> {
    let client = state.we_client.clone();
    let force = force.unwrap_or(false);
    tauri::async_runtime::spawn_blocking(move || {
        // Copy the scan targets under a short-lived lock, then release it
        // BEFORE touching the disk.
        //
        // The explicit `let` + block is essential: a temporary guard in
        // `client.read().foo()` lives until the end of the *statement*, which
        // silently keeps the lock held for the whole scan. That exact mistake
        // once turned a fix into a regression where every view switch flashed.
        let targets = {
            let guard = client.read();
            if force {
                guard.invalidate_installed();
            }
            guard.scan_targets()
        };

        // Lock-free disk walk. Holds no lock, so the synchronous `we_*`
        // commands that run on the main thread are never blocked by a scan.
        let scanned = WallpaperEngineClient::scan_installed_from(targets);

        // Short lock to publish the result.
        client.read().store_installed(scanned.clone());
        scanned
    })
    .await
    .map_err(|e| e.to_string())
}

/// Apply a wallpaper.
///
/// Async: launching the process, writing project.json and talking to the
/// running Wallpaper Engine instance are all blocking calls.
#[command]
pub async fn we_apply(
    state: AppStateHandle<'_>,
    project_path: String,
    monitor: Option<u32>,
    force: Option<bool>,
) -> Result<(), String> {
    let client = state.we_client.clone();
    tauri::async_runtime::spawn_blocking(move || {
        client
            .read()
            .apply(
                &PathBuf::from(project_path),
                monitor,
                force.unwrap_or(false),
            )
            .map_err(map_err)
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Push preset properties to the running Wallpaper Engine instance.
///
/// Async: every `current_wallpaper_pubfileid` call re-reads config.json from
/// disk, and the loop probes up to eight monitors. Doing that synchronously
/// on the main thread stuttered the preset panel on every slider drag.
#[command]
pub async fn we_apply_properties(
    state: AppStateHandle<'_>,
    pubfileid: String,
    properties: serde_json::Map<String, serde_json::Value>,
) -> Result<(), String> {
    let client = state.we_client.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let we = client.read();

        // Find all monitors where this wallpaper is active
        let mut applied_any = false;
        for i in 0..8 {
            if let Some(active_id) = we.current_wallpaper_pubfileid(i) {
                if active_id == pubfileid {
                    let _ = we.apply_properties(&properties, Some(i));
                    applied_any = true;
                }
            }
        }

        // Fallback if not currently active on any known monitor (though it won't be visible)
        if !applied_any {
            let _ = we.apply_properties(&properties, None);
        }

        Ok(())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Persist preset properties into project.json. Async: read + write.
#[command]
pub async fn we_save_project_properties(
    project_json_path: String,
    properties: serde_json::Map<String, serde_json::Value>,
) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        save_project_properties_blocking(&project_json_path, properties)
    })
    .await
    .map_err(|e| e.to_string())?
}

fn save_project_properties_blocking(
    project_json_path: &str,
    properties: serde_json::Map<String, serde_json::Value>,
) -> Result<(), String> {
    let json_path = std::path::PathBuf::from(&project_json_path);
    let backup_path = json_path.with_file_name("project_backup.json");

    // Create a backup if it doesn't exist
    if !backup_path.exists() && json_path.exists() {
        let _ = std::fs::copy(&json_path, &backup_path);
    }

    if let Ok(raw) = std::fs::read_to_string(&json_path) {
        if let Ok(mut config) = serde_json::from_str::<serde_json::Value>(&raw) {
            if let Some(general) = config.get_mut("general").and_then(|v| v.as_object_mut()) {
                if !general.contains_key("properties") {
                    general.insert("properties".to_string(), serde_json::json!({}));
                }

                if let Some(props) = general
                    .get_mut("properties")
                    .and_then(|v| v.as_object_mut())
                {
                    for (key, value) in properties {
                        if !props.contains_key(&key) {
                            props.insert(key, serde_json::json!({ "value": value }));
                        } else {
                            if let Some(prop_obj) =
                                props.get_mut(&key).and_then(|v| v.as_object_mut())
                            {
                                prop_obj.insert("value".to_string(), value);
                            }
                        }
                    }
                }
            }

            // Write it back
            let _ = std::fs::write(
                &json_path,
                serde_json::to_string_pretty(&config).unwrap_or_default(),
            );
            return Ok(());
        }
    }

    Err("Failed to save to project.json".to_string())
}

/// Restore project.json from its backup. Async: two file operations.
#[command]
pub async fn we_restore_project_properties(project_json_path: String) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        let json_path = std::path::PathBuf::from(&project_json_path);
        let backup_path = json_path.with_file_name("project_backup.json");

        if backup_path.exists() {
            if let Err(e) = std::fs::copy(&backup_path, &json_path) {
                return Err(format!("Failed to restore backup: {}", e));
            }
            Ok(())
        } else {
            Err("No backup found".to_string())
        }
    })
    .await
    .map_err(|e| e.to_string())?
}

#[command]
pub fn we_open(state: AppStateHandle<'_>, show_window: Option<bool>) -> Result<(), String> {
    state
        .we_client
        .read()
        .open_wallpaper_engine(show_window.unwrap_or(true))
        .map_err(map_err)
}

#[command]
pub async fn we_delete_wallpaper(
    state: AppStateHandle<'_>,
    pubfileid: String,
    force: Option<bool>,
) -> Result<(), String> {
    // Async on purpose: this hits the disk (config.json read, folder removal,
    // metadata rewrite). A synchronous command would run on the main thread
    // and freeze the window — and the bulk-delete path fires these
    // concurrently, so N simultaneous deletions used to lock the UI solid.
    let client = state.we_client.clone();
    let metadata = state.metadata.clone();
    tauri::async_runtime::spawn_blocking(move || {
        delete_wallpaper_blocking(&client, &metadata, &pubfileid, force.unwrap_or(false))
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Delete several wallpapers in one call.
///
/// The frontend used to fire `we_delete_wallpaper` N times concurrently via
/// `Promise.all`. Every one of those took its own read lock, re-read
/// config.json, walked three directories looking for the folder and rewrote
/// the entire metadata.json — so a bulk delete of a dozen items meant a dozen
/// simultaneous full-store writes and a solid UI freeze. This does the whole
/// batch behind a single lock acquisition and a single metadata save.
#[command]
pub async fn we_delete_wallpapers_bulk(
    state: AppStateHandle<'_>,
    pubfileids: Vec<String>,
    force: Option<bool>,
) -> Result<Vec<(String, String)>, String> {
    let client = state.we_client.clone();
    let metadata = state.metadata.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let force = force.unwrap_or(false);
        let mut failures: Vec<(String, String)> = Vec::new();
        let mut removed: Vec<String> = Vec::new();

        // Check the active set once, not once per id.
        let active = client.read().active_pubfileids();

        for pubfileid in &pubfileids {
            if active.iter().any(|id| id == pubfileid) {
                failures.push((pubfileid.clone(), format!("ACTIVE:{pubfileid}")));
                continue;
            }
            let Some((folder, source)) = client.read().find_wallpaper_dir(pubfileid) else {
                failures.push((pubfileid.clone(), format!("NOTFOUND:{pubfileid}")));
                continue;
            };
            if source == crate::we_client::SOURCE_WORKSHOP {
                match delete_workshop_folder(&folder, pubfileid, force) {
                    Ok(()) => removed.push(pubfileid.clone()),
                    Err(error) => failures.push((pubfileid.clone(), error)),
                }
                continue;
            }
            match remove_folder(&folder) {
                Ok(()) => removed.push(pubfileid.clone()),
                Err(e) => failures.push((pubfileid.clone(), map_err(e))),
            }
        }

        // One metadata rewrite for the whole batch.
        metadata.read().remove_many(&removed);
        if !removed.is_empty() {
            client.read().invalidate_installed();
        }
        Ok(failures)
    })
    .await
    .map_err(|e| e.to_string())?
}

fn delete_wallpaper_blocking(
    client: &std::sync::Arc<parking_lot::RwLock<WallpaperEngineClient>>,
    metadata: &std::sync::Arc<parking_lot::RwLock<crate::config::MetadataService>>,
    pubfileid: &str,
    force: bool,
) -> Result<(), String> {
    // Refuse to delete a wallpaper that's currently being displayed on any
    // monitor — otherwise Wallpaper Engine keeps a file handle and the
    // delete fails with a confusing OS error halfway through. The caller
    // matches on the "ACTIVE:" prefix to show a friendly prompt.
    let active = client.read().active_pubfileids();
    if active.iter().any(|id| id == pubfileid) {
        return Err(format!("ACTIVE:{pubfileid}"));
    }

    // The Installed view merges myprojects, defaultprojects and the Steam
    // Workshop directory, so the folder can live in any of them. Search all
    // three instead of assuming myprojects.
    let Some((folder, source)) = client.read().find_wallpaper_dir(pubfileid) else {
        return Err(format!("NOTFOUND:{pubfileid}"));
    };

    // Workshop folders belong to the Steam client. A folder is only safe to
    // remove when Steam no longer has a subscription for it (an item can stay
    // in WorkshopItemsInstalled after the subscription is gone). Anything
    // still subscribed needs an explicit force confirmation from the user.
    if source == crate::we_client::SOURCE_WORKSHOP {
        delete_workshop_folder(&folder, pubfileid, force)?;
        metadata.read().remove_item(pubfileid);
        client.read().invalidate_installed();
        return Ok(());
    }

    metadata.read().remove_item(pubfileid);
    let result = remove_folder(&folder).map_err(map_err);
    // The installed list is cached; a deletion changes it.
    client.read().invalidate_installed();
    result
}

/// Read a wallpaper's project.json.
///
/// Async: plain file I/O. This is called every time the preset-settings panel
/// opens, so a main-thread read was directly visible as a hitch.
#[command]
pub async fn we_get_project_json(project_json_path: String) -> Result<serde_json::Value, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let raw = std::fs::read_to_string(&project_json_path).map_err(|e| e.to_string())?;
        serde_json::from_str(&raw).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Active wallpaper ids, one per monitor.
///
/// Async on purpose. Tauri runs *synchronous* commands on the main thread,
/// so the old `pub fn` form blocked the UI on every call — and the details
/// panel polls this every two seconds, which froze the whole window. An
/// `async fn` command is dispatched on the async runtime instead, keeping
/// the config.json read off the event loop.
#[command]
pub async fn we_active_pubfileids(state: AppStateHandle<'_>) -> Result<Vec<String>, String> {
    Ok(state.we_client.read().active_pubfileids())
}

#[command]
pub fn we_play(state: AppStateHandle<'_>, monitor: Option<u32>) -> Result<(), String> {
    state.we_client.read().play(monitor).map_err(map_err)
}

#[command]
pub fn we_pause(state: AppStateHandle<'_>, monitor: Option<u32>) -> Result<(), String> {
    state.we_client.read().pause(monitor).map_err(map_err)
}

#[command]
pub fn we_stop(state: AppStateHandle<'_>, monitor: Option<u32>) -> Result<(), String> {
    state.we_client.read().stop(monitor).map_err(map_err)
}

#[command]
pub fn we_mute(state: AppStateHandle<'_>) -> Result<(), String> {
    state.we_client.read().mute().map_err(map_err)
}

#[command]
pub fn we_unmute(state: AppStateHandle<'_>) -> Result<(), String> {
    state.we_client.read().unmute().map_err(map_err)
}

#[command]
pub fn we_next_wallpaper(state: AppStateHandle<'_>, monitor: Option<u32>) -> Result<(), String> {
    state
        .we_client
        .read()
        .next_wallpaper(monitor)
        .map_err(map_err)
}

#[command]
pub fn we_previous_wallpaper(
    state: AppStateHandle<'_>,
    monitor: Option<u32>,
) -> Result<(), String> {
    state
        .we_client
        .read()
        .previous_wallpaper(monitor)
        .map_err(map_err)
}

#[command]
pub fn we_show_icons(state: AppStateHandle<'_>) -> Result<(), String> {
    state.we_client.read().show_icons().map_err(map_err)
}

#[command]
pub fn we_hide_icons(state: AppStateHandle<'_>) -> Result<(), String> {
    state.we_client.read().hide_icons().map_err(map_err)
}

#[command]
pub fn we_open_playlist(
    state: AppStateHandle<'_>,
    playlist: String,
    monitor: Option<u32>,
) -> Result<(), String> {
    state
        .we_client
        .read()
        .open_playlist(&playlist, monitor)
        .map_err(map_err)
}

#[command]
pub fn we_open_profile(state: AppStateHandle<'_>, profile: String) -> Result<(), String> {
    state
        .we_client
        .read()
        .open_profile(&profile)
        .map_err(map_err)
}

/// Remove a wallpaper folder outright.
///
/// Separate from `delete_wallpaper_folder` because the caller already resolved
/// the absolute path: re-joining a pubfileid would only work for myprojects.
fn remove_folder(folder: &std::path::Path) -> anyhow::Result<()> {
    if folder.exists() {
        std::fs::remove_dir_all(folder)?;
    }
    Ok(())
}

/// Move a Workshop folder to the recycle bin so a mistaken delete is
/// recoverable.
#[cfg(target_os = "windows")]
fn remove_folder_to_trash(folder: &std::path::Path) -> anyhow::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{BOOL, HWND};
    use windows::Win32::UI::Shell::{
        SHFileOperationW, SHFILEOPSTRUCTW, FOF_ALLOWUNDO, FOF_NOCONFIRMATION, FOF_NOERRORUI,
        FOF_SILENT, FO_DELETE,
    };

    if !folder.exists() {
        return Ok(());
    }
    // SHFileOperation expects a double-NUL-terminated path.
    let mut path: Vec<u16> = folder.as_os_str().encode_wide().collect();
    path.push(0);
    path.push(0);
    let mut operation = SHFILEOPSTRUCTW {
        hwnd: HWND(std::ptr::null_mut()),
        wFunc: FO_DELETE,
        pFrom: PCWSTR(path.as_ptr()),
        pTo: PCWSTR::null(),
        fFlags: (FOF_ALLOWUNDO.0 | FOF_NOCONFIRMATION.0 | FOF_SILENT.0 | FOF_NOERRORUI.0) as u16,
        fAnyOperationsAborted: BOOL(0),
        hNameMappings: std::ptr::null_mut(),
        lpszProgressTitle: PCWSTR::null(),
    };
    let result = unsafe { SHFileOperationW(&mut operation) };
    if result != 0 {
        return Err(anyhow::anyhow!("SHFileOperationW failed with code {result}"));
    }
    if operation.fAnyOperationsAborted.as_bool() {
        return Err(anyhow::anyhow!("recycle operation was aborted"));
    }
    Ok(())
}

/// Non-Windows fallback: there is no portable recycle-bin API wired up yet.
/// WEaveR ships on Windows; this branch only keeps the code compiling.
#[cfg(not(target_os = "windows"))]
fn remove_folder_to_trash(folder: &std::path::Path) -> anyhow::Result<()> {
    if folder.exists() {
        std::fs::remove_dir_all(folder)?;
    }
    Ok(())
}

/// Only allow the exact Steam Workshop layout:
/// <steamapps>/workshop/content/431960/<digits>. This is the path-safety gate
/// for the one place where WEaveR deletes a Steam-owned folder.
fn validate_workshop_folder(folder: &std::path::Path, pubfileid: &str) -> Result<(), String> {
    if pubfileid.is_empty() || !pubfileid.bytes().all(|b| b.is_ascii_digit()) {
        return Err(format!("INVALID_PATH:{pubfileid}"));
    }
    let name = folder
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default();
    if name != pubfileid {
        return Err(format!("INVALID_PATH:{pubfileid}"));
    }
    let parent = folder.parent();
    let app_dir = parent.and_then(|p| p.file_name()).and_then(|n| n.to_str());
    let content = parent
        .and_then(|p| p.parent())
        .and_then(|p| p.file_name())
        .and_then(|n| n.to_str());
    let workshop = parent
        .and_then(|p| p.parent())
        .and_then(|p| p.parent())
        .and_then(|p| p.file_name())
        .and_then(|n| n.to_str());
    if app_dir != Some("431960") || content != Some("content") || workshop != Some("workshop") {
        return Err(format!("INVALID_PATH:{pubfileid}"));
    }
    let meta = std::fs::symlink_metadata(folder).map_err(map_err)?;
    if meta.file_type().is_symlink() {
        return Err(format!("INVALID_PATH:{pubfileid}"));
    }
    Ok(())
}

/// Delete one Workshop folder with the residual/subscription policy:
///   * no subscription record -> residual, move to the recycle bin;
///   * still subscribed, but the caller passed force -> same, with a warning;
///   * still subscribed and no force -> refuse with WORKSHOP_SUBSCRIBED so the
///     UI can ask for an explicit confirmation.
fn delete_workshop_folder(
    folder: &std::path::Path,
    pubfileid: &str,
    force: bool,
) -> Result<(), String> {
    validate_workshop_folder(folder, pubfileid)?;
    let Some(content_dir) = folder.parent() else {
        return Err(format!("INVALID_PATH:{pubfileid}"));
    };
    let acf = crate::services::steam_acf::WorkshopAcf::load_for_content_dir(content_dir);
    let residual = acf.is_residual(pubfileid);
    if !residual && !force {
        return Err(format!("WORKSHOP_SUBSCRIBED:{pubfileid}"));
    }
    if !residual {
        log::warn!("[delete] force-deleting subscribed workshop item {pubfileid}");
    }
    remove_folder_to_trash(folder).map_err(map_err)
}
