//! Single, idempotent shutdown path for every way the user can close WEaveR.
//!
//! Before this module there were three different behaviours:
//!   * the custom title bar called app_quit (destroy all windows + hard exit),
//!   * Alt+F4 / the taskbar close button only closed the main window and left
//!     the hidden steam-webview alive, so the process stayed in Task Manager,
//!   * external players started by the video-stream feature survived both.
//!
//! Every entry point now calls request(); it runs once, tears down the video
//! streams and their players, saves the window geometry, destroys every
//! webview, and schedules a hard process exit as a last resort.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use tauri::{AppHandle, Manager};

use crate::commands::video_stream::StreamRegistry;
use crate::config::WindowGeometry;
use crate::core::app_state::AppState;

/// How long the graceful path gets before the process is terminated.
const FORCED_EXIT_DELAY: Duration = Duration::from_millis(800);
/// Watchdog poll interval used to catch closes we did not intercept.
const WATCHDOG_INTERVAL: Duration = Duration::from_secs(2);

/// Claim the one-shot right to run the shutdown sequence.
///
/// Split out so the once-only contract is unit-testable without an AppHandle.
fn claim_once(flag: &AtomicBool) -> bool {
    !flag.swap(true, Ordering::SeqCst)
}

/// Request application shutdown. Safe to call from any thread and any entry
/// point; only the first call performs the teardown.
pub fn request(app: &AppHandle, reason: &str) {
    static CLAIMED: AtomicBool = AtomicBool::new(false);
    if !claim_once(&CLAIMED) {
        return;
    }

    log::info!("[shutdown] requested: {reason}");

    // 1. Stop every video stream and kill the external player processes.
    if let Some(registry) = app.try_state::<std::sync::Arc<StreamRegistry>>() {
        registry.shutdown_all();
    }

    // 1b. Stop the phone sync engine (saves its state before exiting).
    if let Some(sync) = app.try_state::<std::sync::Arc<crate::services::sync::SyncService>>() {
        sync.stop();
    }

    // 2. Persist the window geometry from the backend. The frontend also has a
    //    debounced saver, but a hard exit can land before that debounce fires.
    save_window_geometry(app);

    // 3. Destroy every webview, including the hidden steam-webview.
    for window in app.webview_windows().values() {
        let _ = window.hide();
        let _ = window.destroy();
    }

    // 4. Hard-exit fallback: app.exit(0) is best-effort on Windows while a
    //    hidden WebView2 host is alive, so a background thread guarantees the
    //    process really terminates.
    std::thread::spawn(|| {
        std::thread::sleep(FORCED_EXIT_DELAY);
        std::process::exit(0);
    });
    app.exit(0);
}

/// Install the close watchdog. It catches paths that bypass CloseRequested
/// (for example a window destroyed by the OS) and makes sure the process does
/// not keep running with no main window.
pub fn install_watchdog(app: AppHandle) {
    std::thread::spawn(move || loop {
        std::thread::sleep(WATCHDOG_INTERVAL);
        if app.get_webview_window("main").is_none() {
            request(&app, "watchdog: main window gone");
            break;
        }
    });
}

fn save_window_geometry(app: &AppHandle) {
    let Some(state) = app.try_state::<std::sync::Arc<AppState>>() else {
        return;
    };
    if !state.settings.read().get_save_window_state() {
        return;
    }
    let Some(window) = app.get_webview_window("main") else {
        return;
    };
    let position = window.outer_position().ok();
    let size = window.outer_size().ok();
    let maximized = window.is_maximized().unwrap_or(false);
    if let (Some(position), Some(size)) = (position, size) {
        state.settings.read().set_window_geometry(WindowGeometry {
            x: position.x,
            y: position.y,
            width: size.width as i32,
            height: size.height as i32,
            is_maximized: maximized,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claim_once_returns_true_only_for_the_first_caller() {
        let flag = AtomicBool::new(false);
        assert!(claim_once(&flag));
        assert!(!claim_once(&flag));
        assert!(!claim_once(&flag));
    }

    #[test]
    fn shutdown_all_on_an_empty_registry_is_a_no_op() {
        let registry = StreamRegistry::default();
        registry.shutdown_all(); // must not panic or block
    }
}
