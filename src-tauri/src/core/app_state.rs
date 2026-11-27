use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use parking_lot::RwLock;
use tauri::AppHandle;

use parking_lot::Mutex;

use crate::accounts::AccountManager;
use crate::config::{MetadataService, SettingsService};
use crate::download::DownloadManager;
use crate::extract::ExtractManager;
use crate::i18n::I18nService;
use crate::plugins::plugin_manager;
use crate::services::steam_session::SteamSessionService;
use crate::we_client::WallpaperEngineClient;
use crate::workshop::webview::SteamWebview;
use crate::workshop::WorkshopClient;

pub struct AppState {
    pub app_handle: AppHandle,
    pub app_data_dir: PathBuf,
    pub settings: Arc<RwLock<SettingsService>>,
    pub metadata: Arc<RwLock<MetadataService>>,
    pub i18n: Arc<RwLock<I18nService>>,
    pub accounts: Arc<AccountManager>,
    /// Tracks the most recently completed download so the
    /// `auto_apply_last_downloaded` behavior can pick it up.
    pub last_downloaded: Arc<Mutex<Option<String>>>,
    pub we_client: Arc<RwLock<WallpaperEngineClient>>,
    pub workshop: Arc<WorkshopClient>,
    pub downloads: Arc<DownloadManager>,
    pub extracts: Arc<ExtractManager>,
    pub steam_webview: Arc<SteamWebview>,
    /// Steam token lifecycle (refresh / verify) — see services::steam_session.
    pub steam_session: Arc<SteamSessionService>,
    /// Path to portable .NET Runtime directory (if needed).
    /// None means use system .NET installation.
    pub dotnet_root: Arc<Mutex<Option<PathBuf>>>,
    /// Flag to ensure .NET Runtime check runs only once
    dotnet_initialized: Arc<AtomicBool>,
    /// Flag to ensure plugins check runs only once
    plugins_initialized: Arc<AtomicBool>,
    /// Flag to cancel metadata initialization
    pub metadata_init_cancel: Arc<AtomicBool>,
}

impl AppState {
    pub fn initialize(app_handle: AppHandle) -> anyhow::Result<Self> {
        log::info!("Initializing AppState");
        let app_data_dir = resolve_app_data_dir();
        log::info!("App data directory: {}", app_data_dir.display());
        std::fs::create_dir_all(&app_data_dir).ok();

        let settings_path = app_data_dir.join("settings.json");
        let settings = SettingsService::load(&settings_path)?;

        let metadata_path = app_data_dir.join("metadata.json");
        let metadata = MetadataService::load(&metadata_path)?;

        log::debug!("Initializing i18n service");
        let i18n = I18nService::new();

        log::debug!("Loading account manager");
        let accounts = Arc::new(AccountManager::from_runtime(&app_data_dir));
        let steam_session = Arc::new(SteamSessionService::new(accounts.clone()));

        // Resolve the WE install directory: prefer whatever the user has
        // saved, otherwise auto-detect (Steam library scan + registry).
        // The detected path is persisted so we don't have to re-detect on
        // every launch.
        let mut we_dir: Option<PathBuf> = settings.get_directory().map(PathBuf::from);
        if we_dir.as_ref().map(|p| !p.exists()).unwrap_or(true) {
            log::info!("Detecting Wallpaper Engine installation");
            if let Some(detected) = WallpaperEngineClient::detect_installation() {
                log::info!("Wallpaper Engine detected at: {}", detected.display());
                settings.set_directory(&detected.to_string_lossy());
                we_dir = Some(detected);
            } else {
                log::warn!("Wallpaper Engine installation not found");
            }
        } else {
            log::info!("Using configured Wallpaper Engine directory: {:?}", we_dir);
        }
        let we_client = WallpaperEngineClient::new(we_dir);

        let steam_webview_dir = app_data_dir.join("SteamWebView");
        log::debug!("Initializing Workshop client");
        let workshop = Arc::new(WorkshopClient::new(steam_webview_dir.clone()));

        log::debug!("Initializing download and extract managers");
        let downloads = DownloadManager::new(app_handle.clone());
        let extracts = ExtractManager::new(app_handle.clone());

        log::debug!("Initializing Steam WebView");
        let steam_webview =
            SteamWebview::new(app_handle.clone(), steam_webview_dir, workshop.cookie_jar());

        log::info!("AppState initialized successfully");
        Ok(Self {
            app_handle,
            app_data_dir,
            settings: Arc::new(RwLock::new(settings)),
            metadata: Arc::new(RwLock::new(metadata)),
            i18n: Arc::new(RwLock::new(i18n)),
            accounts,
            last_downloaded: Arc::new(Mutex::new(None)),
            we_client: Arc::new(RwLock::new(we_client)),
            workshop,
            downloads: Arc::new(downloads),
            extracts: Arc::new(extracts),
            steam_webview: Arc::new(steam_webview),
            steam_session,
            dotnet_root: Arc::new(Mutex::new(None)),
            dotnet_initialized: Arc::new(AtomicBool::new(false)),
            plugins_initialized: Arc::new(AtomicBool::new(false)),
            metadata_init_cancel: Arc::new(AtomicBool::new(false)),
        })
    }

    /// Sync cookies from WebView2 storage into reqwest jar on startup.
    /// This ensures the Workshop client has authentication cookies from
    /// previous sessions without needing to wait for an explicit login.
    pub fn init_cookies(&self) {
        log::info!("Initializing cookies from WebView2");
        let steam_webview = self.steam_webview.clone();
        let workshop = self.workshop.clone();
        let accounts = self.accounts.clone();
        let steam_session = self.steam_session.clone();
        std::thread::spawn(move || {
            // Wait for the main thread to reach the event loop.
            //
            // `init_cookies` is called from `setup()`, which itself runs on the
            // main thread *before* the event loop starts. `create_hidden_window`
            // has to be scheduled onto that thread, so a closure queued from here
            // cannot run until setup returns. Blocking on it (which this used to
            // do, with a 15s `recv_timeout`) therefore deadlocked the caller: the
            // whole startup sync died silently and the app behaved as if the
            // session had never been restored.
            std::thread::sleep(EVENT_LOOP_SETTLE);
            let rt = tokio::runtime::Runtime::new().expect("build tokio runtime");
            rt.block_on(async move {
                // Prepare the window and the token at the same time: they are
                // independent (webview lifecycle vs. token store / refresh), and
                // running them in sequence added the window's rehydration budget
                // to every launch even though neither waits for the other.
                let acct = accounts.preferred_parser_credentials().username;
                let (profile_ready, token_result) =
                    tokio::join!(steam_webview.ensure_profile_loaded(), async {
                        if acct.is_empty() {
                            Ok(None)
                        } else {
                            steam_session.ensure_access_token(&acct).await
                        }
                    });
                log::info!("[startup] webview profile ready: {profile_ready}");

                // Fast path: a stored OAuth session can rebuild the login
                // cookies without any sign-in. The token service checks expiry
                // and refreshes with the refresh token when needed, so a session
                // that sat unused for more than a day still comes back.
                let mut restored = false;
                match token_result {
                    Ok(Some(access)) => {
                        if let Some(session) = steam_session.stored(&acct) {
                            let installed = steam_webview
                                .install_session_cookies(&session.steam_id, &access)
                                .is_ok();
                            if installed {
                                // Poll instead of sleeping a fixed 1.2s: the
                                // cookie is normally visible in well under 300ms.
                                let ready = steam_webview.wait_for_session_cookie(10, 100).await;
                                restored = ready;
                                if ready {
                                    log::info!("[startup] installed session for {acct}");
                                } else {
                                    log::warn!(
                                        "[startup] session cookie did not become visible for {acct}"
                                    );
                                }
                            }
                        }
                    }
                    Ok(None) => {
                        log::info!("[startup] no usable Steam session material for {acct}")
                    }
                    Err(e) => {
                        log::warn!("[startup] could not prepare Steam session for {acct}: {e}")
                    }
                }
                log::info!("[startup] steam session restored: {restored}");
                let synced = match steam_webview.sync_cookies().await {
                    Ok(n) => {
                        if n > 0 {
                            log::info!("Synced {} cookies from WebView2 on startup", n);
                            workshop.clear_caches();
                        } else {
                            log::debug!("No cookies to sync from WebView2");
                        }
                        n
                    }
                    Err(e) => {
                        log::warn!("Failed to sync cookies from WebView2: {}", e);
                        0
                    }
                };

                // Nothing came back but we hold a backup: the profile was wiped
                // (a failed startup check used to do that) or WebView2 rebuilt
                // its store. Restore the copy and re-sync, so a transient failure
                // no longer costs the user their session.
                if synced == 0 && steam_webview.restore_cookie_db() {
                    match steam_webview.sync_cookies().await {
                        Ok(n) if n > 0 => {
                            log::info!("Restored {} cookies from backup on startup", n);
                            workshop.clear_caches();
                        }
                        Ok(_) => log::debug!("Backup restored but no cookies were readable"),
                        Err(e) => log::warn!("Re-sync after restore failed: {}", e),
                    }
                }
            });
        });
    }

    /// Initialize .NET Runtime check in background. Call this after state is created.
    pub fn init_dotnet_runtime(&self) {
        log::info!("Initializing .NET Runtime check");
        let app = self.app_handle.clone();
        let dotnet_root = self.dotnet_root.clone();
        let app_data_dir = self.app_data_dir.clone();
        std::thread::spawn(move || {
            let rt = tokio::runtime::Runtime::new().expect("build tokio runtime");
            rt.block_on(async move {
                match crate::runtime::ensure_dotnet_runtime(&app, &app_data_dir).await {
                    Ok(path) => {
                        if let Some(ref p) = path {
                            log::info!(".NET Runtime available at: {}", p.display());
                        } else {
                            log::info!("Using system .NET Runtime");
                        }
                        *dotnet_root.lock() = path;
                    }
                    Err(e) => {
                        log::error!("Failed to ensure .NET Runtime: {}", e);
                    }
                }
            });
        });
    }

    /// Initialize plugins check in background. Downloads any missing plugins.
    pub fn init_plugins(&self) {
        log::info!("Initializing plugins check");
        let app = self.app_handle.clone();
        let plugins_dir = self.app_data_dir.join("plugins");
        std::thread::spawn(move || {
            let rt = tokio::runtime::Runtime::new().expect("build tokio runtime");
            rt.block_on(async move {
                match plugin_manager::ensure_plugins(&app, &plugins_dir).await {
                    Ok(()) => {
                        log::info!("All plugins are ready");
                    }
                    Err(e) => {
                        log::error!("Failed to ensure plugins: {}", e);
                    }
                }
            });
        });
    }

    /// Initialize plugins check manually (for frontend-triggered init)
    pub fn init_plugins_async(&self) {
        // Check if already initialized to prevent duplicate runs
        if self.plugins_initialized.swap(true, Ordering::SeqCst) {
            log::debug!("plugins_init already called, skipping duplicate initialization");
            return;
        }
        self.init_plugins();
    }

    /// Initialize .NET Runtime check manually (for frontend-triggered init)
    pub fn init_dotnet_runtime_async(&self) {
        // Check if already initialized to prevent duplicate runs
        if self.dotnet_initialized.swap(true, Ordering::SeqCst) {
            log::debug!("dotnet_init already called, skipping duplicate initialization");
            return;
        }
        self.init_dotnet_runtime();
    }
}

/// Resolve the app's data directory.
///
/// Uses the same identifier as the Tauri app (WEave) to keep
/// all application data (config, cache, WebView2) in one location.
///
/// This is the single source of truth for the location: the plugin paths and
/// the logger both resolve it through here, so renaming the directory is a
/// one-line change rather than five.
pub fn resolve_app_data_dir() -> PathBuf {
    // Windows: %LOCALAPPDATA%\WEave
    // macOS:   ~/Library/Application Support/WEave
    // Linux:   $XDG_DATA_HOME/WEave or ~/.local/share/WEave
    if let Some(base) = dirs::data_local_dir() {
        return base.join("WEave");
    }

    if let Some(base) = dirs::data_dir() {
        return base.join("WEave");
    }

    if let Some(home) = dirs::home_dir() {
        return home.join(".weave");
    }

    PathBuf::from("./WEave")
}

// ── Timing constants ──────────────────────────────────────────
/// Let the main thread start pumping before posting to it.
const EVENT_LOOP_SETTLE: std::time::Duration = std::time::Duration::from_millis(300);
