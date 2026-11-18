import { useCallback, useState } from "react";
import { useTranslation } from "@/i18n/hooks";
import { pushToast } from "@/stores/toasts";
import { getVideoStreamStatus, startVideoStream } from "@/lib/videoStream";
import type { WorkshopItem } from "@/types/workshop";

export interface WatchState {
  /** Item currently being resolved/launched, or null when idle. */
  pendingId: string | null;
  /** Last error message for inline display, cleared on the next attempt. */
  error: string | null;
  /** True while the backend is still resolving/preparing the video. */
  preparing: boolean;
  /** Item id whose player launched, so the hint only shows for that item. */
  startedId: string | null;
}

/**
 * Shared "watch while downloading" handler.
 *
 * Video wallpapers are handed to an external player over a local HTTP range URL,
 * so playback starts immediately and seeking does not wait for the download.
 *
 * Feedback is returned as state rather than toasts: the details panel sets
 * `detailsOpen`, and the toast store deliberately suppresses toasts while a
 * panel is open — so a toast-based spinner would be invisible exactly where
 * this button lives. The caller renders a spinner from `pendingId` instead.
 */
export function useWatchWhileDownloading(): {
  watch: (item: WorkshopItem) => Promise<void>;
  state: WatchState;
} {
  const { t } = useTranslation();
  const [state, setState] = useState<WatchState>({
    pendingId: null,
    error: null,
    preparing: false,
    startedId: null,
  });

  const watch = useCallback(
    async (item: WorkshopItem) => {
      // Collections have no single video to stream.
      if (item.is_collection) return;

      setState({ pendingId: item.pubfileid, error: null, preparing: true, startedId: null });
      try {
        // Surface a prewarm failure immediately instead of waiting for the
        // start command to re-resolve the item and fail with a generic string.
        const status = await getVideoStreamStatus(item.pubfileid).catch(() => null);
        if (status?.state === "failed") {
          setState({
            pendingId: null,
            error: t("messages.streaming_failed", {
              error: status.error || "prewarm failed",
            }),
            preparing: false,
            startedId: null,
          });
          return;
        }

        const started = await startVideoStream(item.pubfileid);
        setState({
          pendingId: null,
          error: null,
          preparing: false,
          startedId: item.pubfileid,
        });
        // Success toast also works when the panel closes (detailsOpen false by
        // then), so the user still gets confirmation.
        pushToast(
          t("messages.streaming_started", { player: started.player }),
          "success",
        );
      } catch (err) {
        const raw = err instanceof Error ? err.message : String(err);
        let msg: string;
        if (raw.includes("NO_PLAYER")) {
          msg = t("messages.streaming_no_player");
        } else if (raw.includes("NO_ACCOUNT")) {
          msg = t("messages.streaming_no_account");
        } else {
          msg = t("messages.streaming_failed", { error: raw });
        }
        setState({ pendingId: null, error: msg, preparing: false, startedId: null });
      }
    },
    [t],
  );

  return { watch, state };
}