//! External player discovery and launching.
//!
//! The streaming feature deliberately does not embed a player: video wallpapers
//! are handed to a real media player over a local HTTP range URL. This module
//! finds a player that supports range-based streaming (mpv preferred, VLC as a
//! fallback) and launches it detached from WEave.

use std::path::PathBuf;
use std::process::Command;

/// A player we know how to drive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlayerKind {
    Mpv,
    Vlc,
}

impl PlayerKind {
    pub fn name(self) -> &'static str {
        match self {
            PlayerKind::Mpv => "mpv",
            PlayerKind::Vlc => "vlc",
        }
    }
}

/// A discovered player executable.
#[derive(Debug, Clone)]
pub struct Player {
    pub kind: PlayerKind,
    pub path: PathBuf,
}

/// Candidate executables to look for, in preference order.
fn candidates() -> Vec<(PlayerKind, &'static str)> {
    vec![(PlayerKind::Mpv, "mpv.exe"), (PlayerKind::Vlc, "vlc.exe")]
}

/// Directories to scan besides PATH.
fn extra_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    // Scoop shims and common portable layouts are where these usually live.
    for base in [
        std::env::var_os("USERPROFILE").map(PathBuf::from),
        std::env::var_os("ProgramFiles").map(PathBuf::from),
        std::env::var_os("ProgramFiles(x86)").map(PathBuf::from),
        std::env::var_os("LOCALAPPDATA").map(PathBuf::from),
    ]
    .into_iter()
    .flatten()
    {
        dirs.push(base.join("scoop").join("shims"));
        dirs.push(base.join("Scoop").join("shims"));
        dirs.push(base.join("Programs").join("mpv"));
        dirs.push(base.join("mpv"));
        dirs.push(base.join("VideoLAN").join("VLC"));
    }
    dirs
}

/// Find a player, preferring an explicitly configured executable.
///
/// The settings path lets portable installs work without putting mpv/VLC on
/// PATH. When the configured file is missing we fall back to auto-detection so
/// a stale setting cannot make streaming impossible.
pub fn detect_with(preferred: Option<&str>) -> Option<Player> {
    if let Some(raw) = preferred.map(str::trim).filter(|value| !value.is_empty()) {
        let path = std::path::PathBuf::from(raw);
        if path.is_file() {
            let name = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("")
                .to_ascii_lowercase();
            let kind = if name.contains("vlc") {
                PlayerKind::Vlc
            } else {
                PlayerKind::Mpv
            };
            return Some(Player { kind, path });
        }
        log::warn!("[video-stream] configured player does not exist: {raw}");
    }
    detect()
}

/// Find the best available player.
///
/// Order: PATH first (so a user-installed shim wins), then the extra dirs.
pub fn detect() -> Option<Player> {
    let path_var = std::env::var_os("PATH").unwrap_or_default();
    let path_dirs: Vec<PathBuf> = std::env::split_paths(&path_var).collect();

    for (kind, exe) in candidates() {
        // PATH lookup.
        for dir in &path_dirs {
            let candidate = dir.join(exe);
            if candidate.is_file() {
                return Some(Player {
                    kind,
                    path: candidate,
                });
            }
        }
        // Extra directories.
        for dir in extra_dirs() {
            let candidate = dir.join(exe);
            if candidate.is_file() {
                return Some(Player {
                    kind,
                    path: candidate,
                });
            }
        }
    }
    None
}

/// Launch the player against `url`, detached from WEave.
///
/// Returns the child so the caller can wait for the player to exit and then
/// tear the range server down. The previous version discarded it, so closing
/// mpv left the server and its background prefetcher running until the next
/// click or app exit.
pub fn launch(player: &Player, url: &str) -> std::io::Result<std::process::Child> {
    let mut cmd = Command::new(&player.path);
    match player.kind {
        PlayerKind::Mpv => {
            // --force-window: show a window even though this is a stream.
            // --keep-open: do not exit at EOF (the file may still be growing).
            // --no-terminal: no console output.
            // Cache options: the default demuxer cache is far too small for a
            // CDN that can stall for seconds; 256 MiB forward / 64 MiB back keeps
            // playback alive across those gaps and makes seeks cheap.
            cmd.arg("--force-window=yes")
                .arg("--keep-open=yes")
                .arg("--no-terminal")
                .arg("--cache=yes")
                .arg("--demuxer-max-bytes=256MiB")
                .arg("--demuxer-max-back-bytes=64MiB")
                .arg("--cache-pause=yes")
                .arg("--cache-pause-wait=2")
                .arg("--network-timeout=30")
                .arg(url);
        }
        PlayerKind::Vlc => {
            // VLC's default network cache is 1s; raise it for the same reason.
            cmd.arg("--no-video-title-show")
                .arg("--network-caching=3000")
                .arg(url);
        }
    }

    crate::core::platform::hide_console_window(&mut cmd);

    cmd.spawn()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_finds_something_or_none() {
        // Can only assert it does not panic; presence depends on the machine.
        let p = detect();
        if let Some(p) = p {
            assert!(p.path.is_file(), "detected player must exist on disk");
        }
    }

    #[test]
    fn candidates_prefer_mpv() {
        let c = candidates();
        assert_eq!(c[0].0, PlayerKind::Mpv);
    }
}
