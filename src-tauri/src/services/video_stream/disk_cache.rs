//! On-disk cache for decrypted video chunks.
//!
//! Streaming re-fetches every chunk the player asks for; the in-memory LRU in
//! `ResolvedVideo` only covers the current session and is capped at 128 MiB.
//! Persisting chunks turns "watch while downloading" into "watch while it
//! downloads, and keep what was watched": replaying an item hits the disk for
//! everything already seen, and seeking back is free.
//!
//! Layout:
//!   `%LOCALAPPDATA%\\WEave\\video-cache\\<item_id>\\<manifest_id>\\<chunk_id>.bin`
//!
//! Chunks are the *decrypted and decompressed* bytes (what the range server
//! serves), so a hit needs no depot key and no network. Writes are atomic
//! (tmp + rename) so a crash mid-write cannot leave a truncated chunk that
//! would corrupt playback later.

use std::path::PathBuf;

/// Default cap for the whole cache. 4 GiB covers several 4K wallpapers while
/// staying well below a typical disk-pressure threshold; `prune` drops the
/// oldest files first.
pub const MAX_CACHE_BYTES: u64 = 4 * 1024 * 1024 * 1024;

/// Root directory. `WEAVE_VIDEO_CACHE_DIR` exists so tests (and advanced
/// users) can relocate the cache without touching the app data directory.
pub fn cache_root() -> PathBuf {
    if let Some(dir) = std::env::var_os("WEAVE_VIDEO_CACHE_DIR") {
        return PathBuf::from(dir);
    }
    dirs::data_local_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("WEave")
        .join("video-cache")
}

/// Path of one cached chunk. Returns `None` for an empty id (defensive: the
/// caller already validates chunk ids as hex).
pub fn chunk_path(item_id: u64, manifest_id: u64, chunk_id: &str) -> Option<PathBuf> {
    if chunk_id.is_empty() {
        return None;
    }
    Some(
        cache_root()
            .join(item_id.to_string())
            .join(manifest_id.to_string())
            .join(format!("{chunk_id}.bin")),
    )
}

/// Read a cached chunk, if present and non-empty.
pub fn load(item_id: u64, manifest_id: u64, chunk_id: &str) -> Option<Vec<u8>> {
    let path = chunk_path(item_id, manifest_id, chunk_id)?;
    let bytes = std::fs::read(path).ok()?;
    if bytes.is_empty() {
        return None;
    }
    Some(bytes)
}

/// Persist one chunk atomically. Errors are non-fatal for playback (the bytes
/// are already in memory), so callers log and ignore.
pub fn store(item_id: u64, manifest_id: u64, chunk_id: &str, bytes: &[u8]) -> std::io::Result<()> {
    let path = chunk_path(item_id, manifest_id, chunk_id)
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "empty chunk id"))?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("bin.tmp");
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

/// Drop the oldest cached files until the cache fits in `max_bytes`.
///
/// Runs at most once per process (via `prune_once`), in the background: a
/// recursive mtime walk is cheap compared with a manifest download but has no
/// business blocking a resolve.
pub fn prune(max_bytes: u64) {
    let root = cache_root();
    let mut files: Vec<(std::time::SystemTime, u64, PathBuf)> = Vec::new();
    collect_files(&root, &mut files);
    let mut total: u64 = files.iter().map(|(_, size, _)| *size).sum();
    if total <= max_bytes {
        return;
    }
    files.sort_by_key(|(mtime, _, _)| *mtime);
    for (_, size, path) in files {
        if total <= max_bytes {
            break;
        }
        if std::fs::remove_file(&path).is_ok() {
            total = total.saturating_sub(size);
            log::info!("[video-cache] pruned {}", path.display());
        }
    }
}

fn collect_files(dir: &std::path::Path, out: &mut Vec<(std::time::SystemTime, u64, PathBuf)>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        if meta.is_dir() {
            collect_files(&path, out);
        } else if path.extension().and_then(|e| e.to_str()) == Some("bin") {
            let mtime = meta.modified().unwrap_or(std::time::SystemTime::UNIX_EPOCH);
            out.push((mtime, meta.len(), path));
        }
    }
}

/// Run `prune` in a background thread, once per process.
pub fn prune_once(max_bytes: u64) {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        std::thread::spawn(move || prune(max_bytes));
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunk_path_is_scoped_by_item_and_manifest() {
        let path = chunk_path(123, 456, "abcdef").expect("path");
        assert!(path.ends_with(r"123\456\abcdef.bin") || path.ends_with("123/456/abcdef.bin"));
    }

    #[test]
    fn empty_chunk_id_is_rejected() {
        assert!(chunk_path(1, 2, "").is_none());
    }

    #[test]
    fn store_and_load_round_trip() {
        // Isolated directory so a developer's real cache is never touched.
        let dir = std::env::temp_dir().join(format!("weave-disk-cache-test-{}", std::process::id()));
        std::env::set_var("WEAVE_VIDEO_CACHE_DIR", &dir);
        let bytes = b"chunk-bytes";
        store(7, 8, "0011", bytes).expect("store");
        assert_eq!(load(7, 8, "0011").as_deref(), Some(&bytes[..]));
        let _ = std::fs::remove_dir_all(&dir);
    }
}