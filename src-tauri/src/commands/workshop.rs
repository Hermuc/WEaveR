use tauri::command;

use crate::commands::{map_err, AppStateHandle};
use crate::workshop::{CollectionContents, WorkshopFilters, WorkshopItem, WorkshopPage};

#[command]
pub async fn workshop_browse(
    state: AppStateHandle<'_>,
    filters: WorkshopFilters,
) -> Result<WorkshopPage, String> {
    state.workshop.browse(&filters).await.map_err(map_err)
}

/// Browse the Workshop with a file-size filter.
///
/// Separate from [`workshop_browse`] on purpose: the unfiltered path must stay
/// a single page fetch, and the filtered path walks several pages. Keeping them
/// apart means no existing caller changes behaviour.
///
/// NOTE for callers: Tauri 2 maps these snake_case arguments to camelCase on
/// the wire, so the frontend must send `minMb` / `maxMb` / `perPage`. Sending
/// `min_mb` silently arrives as `None` — the command runs, the filter just
/// never activates.
#[command]
pub async fn workshop_browse_sized(
    state: AppStateHandle<'_>,
    filters: WorkshopFilters,
    min_mb: Option<f64>,
    max_mb: Option<f64>,
    per_page: Option<u32>,
) -> Result<WorkshopPage, String> {
    let range = crate::services::workshop::size_index::SizeRange::from_mb(min_mb, max_mb);
    // 30 matches Steam's physical page, so a filtered page looks like a normal
    // one and the grid layout does not change.
    let per_page = per_page.unwrap_or(30) as usize;
    state
        .workshop
        .browse_sized(&filters, range, per_page)
        .await
        .map_err(map_err)
}
#[command]
pub async fn workshop_browse_collections(
    state: AppStateHandle<'_>,
    filters: WorkshopFilters,
) -> Result<WorkshopPage, String> {
    state
        .workshop
        .browse_collections(&filters)
        .await
        .map_err(map_err)
}

#[command]
pub async fn workshop_get_item(
    state: AppStateHandle<'_>,
    pubfileid: String,
) -> Result<WorkshopItem, String> {
    let item = state
        .workshop
        .item_details(&pubfileid)
        .await
        .map_err(map_err)?;
    // Persist metadata so that it's available offline next time (mirrors
    // the Python `MetadataService.save_from_workshop_item`).
    if let Ok(value) = serde_json::to_value(&item) {
        state.metadata.read().set_item(&pubfileid, value);
    }
    Ok(item)
}

#[command]
pub async fn workshop_get_collection(
    state: AppStateHandle<'_>,
    collection_id: String,
) -> Result<CollectionContents, String> {
    state
        .workshop
        .collection_contents(&collection_id)
        .await
        .map_err(map_err)
}

#[command]
pub async fn workshop_get_author_items(
    state: AppStateHandle<'_>,
    profile_url: String,
    filters: WorkshopFilters,
) -> Result<WorkshopPage, String> {
    state
        .workshop
        .author_items(&profile_url, &filters)
        .await
        .map_err(map_err)
}

#[command]
pub async fn workshop_get_author_collections(
    state: AppStateHandle<'_>,
    profile_url: String,
    filters: WorkshopFilters,
) -> Result<WorkshopPage, String> {
    state
        .workshop
        .author_collections(&profile_url, &filters)
        .await
        .map_err(map_err)
}

#[command]
pub fn workshop_refresh_cache(state: AppStateHandle<'_>) {
    state.workshop.clear_caches();
}

/// Return the in-memory parser debug log (last N HTTP responses Steam
/// returned). Used by the Settings → Debug → "Parser log" dialog as a
/// replacement for the original Python app's visible-webview debug mode.
#[command]
pub fn workshop_debug_log() -> Vec<crate::workshop::debug::DebugEntry> {
    crate::workshop::debug::snapshot()
}

#[command]
pub fn workshop_debug_clear() {
    crate::workshop::debug::clear();
}
