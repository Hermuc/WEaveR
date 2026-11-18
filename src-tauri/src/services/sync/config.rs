//! Map WEaveR settings (settings.json -> "sync" object) to the we-sync
//! AppConfig. The we-sync crate keeps owning the schema; WEaveR only stores
//! and forwards it, so new upstream fields flow through without code changes.

use crate::config::SettingsService;
use we_sync::config::AppConfig;

pub const SETTINGS_KEY: &str = "sync";

pub fn load(settings: &SettingsService) -> AppConfig {
    settings
        .get(SETTINGS_KEY)
        .and_then(|value| serde_json::from_value::<AppConfig>(value).ok())
        .unwrap_or_default()
}

pub fn save(settings: &SettingsService, config: &AppConfig) -> anyhow::Result<()> {
    let value = serde_json::to_value(config)?;
    settings.set(SETTINGS_KEY, value)
}
