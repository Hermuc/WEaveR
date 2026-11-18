import { useEffect, useState } from "react";
import { open as openPath } from "@tauri-apps/plugin-dialog";
import { useTranslation } from "@/i18n/hooks";
import { inTauri, invoke, tryInvoke, tryInvokeAction } from "@/lib/tauri";
import { pushToast } from "@/stores/toasts";
import { FolderOpen, Trash2 } from "lucide-react";
import { Row, Section } from "./SettingsDialogShared";

interface CacheStats {
  files: number;
  bytes: number;
  root: string;
}

interface PlayerInfo {
  name: string;
  path: string;
}

function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  if (bytes < 1024 * 1024 * 1024) return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
  return `${(bytes / (1024 * 1024 * 1024)).toFixed(2)} GB`;
}

/**
 * Settings for the "watch while downloading" pipeline: external player path
 * and video cache management.
 *
 * Both are plain settings/commands, so this tab has no coupling to the stream
 * runtime — changing the player path takes effect on the next launch, and the
 * cache commands only touch `.bin` files under the video-cache directory.
 */
export default function VideoSettingsTab() {
  const { t } = useTranslation();
  const [playerPath, setPlayerPath] = useState("");
  const [detected, setDetected] = useState<PlayerInfo | null>(null);
  const [stats, setStats] = useState<CacheStats | null>(null);
  const [clearing, setClearing] = useState(false);

  const refreshPlayer = async () => {
    const info = await tryInvoke<PlayerInfo | null>("video_stream_player");
    setDetected(info ?? null);
  };

  const refreshStats = async () => {
    const value = await tryInvoke<CacheStats>("video_stream_cache_stats");
    setStats(value ?? null);
  };

  useEffect(() => {
    if (!inTauri) return;
    void tryInvoke<string>("config_get", { path: "video_stream.player_path" }, "").then(
      (value) => {
        if (typeof value === "string") setPlayerPath(value);
      },
    );
    void tryInvoke<PlayerInfo | null>("video_stream_player").then((info) =>
      setDetected(info ?? null),
    );
    void tryInvoke<CacheStats>("video_stream_cache_stats").then((value) =>
      setStats(value ?? null),
    );
  }, []);

  const savePlayerPath = async (value: string) => {
    if (!inTauri) return;
    await invoke("config_set", { path: "video_stream.player_path", value }).catch(
      () => undefined,
    );
    await refreshPlayer();
  };

  const browsePlayer = async () => {
    const picked = await openPath({
      directory: false,
      multiple: false,
      filters: [{ name: "Player", extensions: ["exe"] }],
    });
    if (!picked || Array.isArray(picked)) return;
    setPlayerPath(picked);
    await savePlayerPath(picked);
  };

  const clearCache = async () => {
    setClearing(true);
    const result = await tryInvokeAction("video_stream_cache_clear");
    setClearing(false);
    if (result.ok) {
      pushToast(t("settings.video_cache_cleared"), "success");
      await refreshStats();
    } else {
      pushToast(
        `${t("settings.video_cache_clear_failed")}: ${result.error}`,
        "error",
      );
    }
  };

  return (
    <div className="space-y-6 p-4">
      <Section title={t("settings.video")}>
        <Row label={t("settings.video_player")} description={t("settings.video_player_hint")}>
          <div className="flex items-center gap-2">
            <input
              value={playerPath}
              onChange={(event) => setPlayerPath(event.target.value)}
              onBlur={() => void savePlayerPath(playerPath)}
              placeholder="C:\Program Files\mpv\mpv.exe"
              className="w-56 rounded-md border border-white/10 bg-surface-sunken/40 px-2.5 py-1.5 text-xs text-foreground placeholder-white/25 focus:ring-1 focus:ring-primary/50 focus:outline-none"
            />
            <button
              onClick={() => void browsePlayer()}
              aria-label={t("settings.video_player")}
              className="hover-shimmer flex size-8 items-center justify-center rounded-md bg-white/5 text-foreground transition-colors hover:bg-white/10"
            >
              <FolderOpen className="size-4" />
            </button>
          </div>
        </Row>
        <Row
          label={t("settings.video_detected")}
          description={detected?.path ?? t("settings.video_not_detected")}
        >
          <span className="rounded-full bg-primary/10 px-2 py-0.5 text-[11px] font-semibold text-primary">
            {detected?.name ?? "—"}
          </span>
        </Row>
        <Row label={t("settings.video_cache")} description={t("settings.video_cache_hint")}>
          <div className="flex items-center gap-2">
            <span className="text-[11px] text-muted">
              {stats
                ? `${stats.files} · ${formatBytes(stats.bytes)}`
                : t("settings.video_cache_empty")}
            </span>
            <button
              onClick={() => void clearCache()}
              disabled={clearing || !stats || stats.files === 0}
              className="hover-shimmer flex items-center gap-1.5 rounded-md border border-danger/25 bg-danger/10 px-2.5 py-1.5 text-[11px] font-semibold text-danger transition-colors hover:bg-danger/20 disabled:opacity-50"
            >
              <Trash2 className="size-3.5" />
              {t("settings.video_cache_clear")}
            </button>
          </div>
        </Row>
      </Section>
    </div>
  );
}