import { useEffect } from "react";
import { ArrowDownToLine, Pause, Play, RefreshCw, Smartphone, WifiOff } from "lucide-react";

import { useTranslation } from "@/i18n/hooks";
import { pushToast } from "@/stores/toasts";
import { useSyncStore } from "@/stores/sync";

function formatBytes(bytes: number): string {
  if (bytes < 1024) return bytes + " B";
  if (bytes < 1024 * 1024) return (bytes / 1024).toFixed(1) + " KB";
  if (bytes < 1024 * 1024 * 1024) return (bytes / 1024 / 1024).toFixed(1) + " MB";
  return (bytes / 1024 / 1024 / 1024).toFixed(2) + " GB";
}

export default function SyncView() {
  const { t } = useTranslation();
  const status = useSyncStore();

  useEffect(() => {
    let unlisten: (() => void) | undefined;
    void (async () => {
      unlisten = await useSyncStore.getState().bind();
      await useSyncStore.getState().refresh();
    })();
    return () => {
      unlisten?.();
    };
  }, []);

  const percent =
    status.progress && status.progress.total > 0
      ? Math.round((status.progress.done / status.progress.total) * 100)
      : 0;

  const run = async (action: () => Promise<boolean>) => {
    const ok = await action();
    if (!ok) pushToast(t("sync.action_failed"), "error");
  };

  const statusLabel = !status.running
    ? t("sync.not_running")
    : status.paused
      ? t("sync.paused")
      : status.device
        ? t("sync.running")
        : t("sync.waiting_device");

  return (
    <div className="flex h-full flex-col gap-4 overflow-y-auto p-6">
      <header className="flex items-start justify-between gap-4">
        <div>
          <h1 className="text-lg font-semibold text-foreground">{t("sync.title")}</h1>
          <p className="mt-1 text-xs text-muted">{t("sync.description")}</p>
        </div>
        <span className="rounded-full bg-primary/10 px-2.5 py-1 text-[11px] font-semibold text-primary">
          {statusLabel}
        </span>
      </header>

      <section className="rounded-xl border border-white/5 bg-surface-raised/60 p-4">
        <div className="flex items-center gap-3">
          {status.device ? (
            <Smartphone className="size-5 text-primary" />
          ) : (
            <WifiOff className="size-5 text-muted" />
          )}
          <div className="min-w-0">
            <div className="text-sm font-medium text-foreground">
              {status.device
                ? t("sync.connected") + " [" + status.device.channel + "] " + status.device.name
                : t("sync.no_device")}
            </div>
            {status.device && status.device.storages.length > 0 && (
              <div className="mt-0.5 text-[11px] text-muted">
                {t("sync.device_storage")}: {status.device.storages.join(" / ")}
              </div>
            )}
          </div>
        </div>
      </section>

      <section className="rounded-xl border border-white/5 bg-surface-raised/60 p-4">
        <div className="flex items-center justify-between text-xs">
          <span className="font-medium text-foreground">
            {status.progress ? t("sync.progress") : status.busy ? t("sync.preparing") : t("sync.idle")}
          </span>
          {status.progress && (
            <span className="text-muted">
              {status.progress.name} · {percent}% ({formatBytes(status.progress.done)} / {formatBytes(status.progress.total)})
            </span>
          )}
        </div>
        <div className="mt-2 h-1.5 overflow-hidden rounded-full bg-white/5">
          <div
            className="h-full rounded-full bg-primary transition-[width] duration-300"
            style={{ width: percent + "%" }}
          />
        </div>
        <div className="mt-3 grid grid-cols-4 gap-2 text-center">
          {[
            [t("sync.copied"), status.stats.uploaded],
            [t("sync.skipped"), status.stats.skipped],
            [t("sync.failed"), status.stats.failed],
            [t("sync.filtered"), status.stats.filtered],
          ].map(([label, value]) => (
            <div key={String(label)} className="rounded-lg bg-white/5 px-2 py-2">
              <div className="text-base font-semibold text-foreground">{value}</div>
              <div className="text-[10px] text-muted">{label}</div>
            </div>
          ))}
        </div>
        <div className="mt-2 text-right text-[11px] text-muted">
          {t("sync.queue")}: {status.queue}
        </div>
      </section>

      <div className="flex flex-wrap items-center gap-2">
        {!status.running ? (
          <button
            onClick={() => void run(() => useSyncStore.getState().start())}
            className="hover-shimmer rounded-md bg-primary px-3 py-1.5 text-xs font-semibold text-white transition-opacity hover:opacity-90"
          >
            {t("sync.start")}
          </button>
        ) : (
          <button
            onClick={() => void run(() => useSyncStore.getState().now())}
            className="hover-shimmer flex items-center gap-1.5 rounded-md bg-primary px-3 py-1.5 text-xs font-semibold text-white transition-opacity hover:opacity-90"
          >
            <RefreshCw className="size-3.5" />
            {t("sync.sync_now")}
          </button>
        )}
        <button
          onClick={() => void run(() => useSyncStore.getState().pause(!status.paused))}
          disabled={!status.running}
          className="flex items-center gap-1.5 rounded-md border border-white/10 bg-white/5 px-3 py-1.5 text-xs font-medium text-foreground transition-colors hover:bg-white/10 disabled:opacity-40"
        >
          {status.paused ? <Play className="size-3.5" /> : <Pause className="size-3.5" />}
          {status.paused ? t("sync.resume") : t("sync.pause")}
        </button>
        <button
          onClick={() => void run(() => useSyncStore.getState().transferPause(!status.transfer_paused))}
          disabled={!status.running || !status.busy}
          className="flex items-center gap-1.5 rounded-md border border-white/10 bg-white/5 px-3 py-1.5 text-xs font-medium text-foreground transition-colors hover:bg-white/10 disabled:opacity-40"
        >
          {status.transfer_paused ? <Play className="size-3.5" /> : <ArrowDownToLine className="size-3.5" />}
          {status.transfer_paused ? t("sync.resume_transfer") : t("sync.pause_transfer")}
        </button>
      </div>

      <section className="min-h-0 flex-1 rounded-xl border border-white/5 bg-surface-sunken/50 p-4">
        <h2 className="mb-2 text-xs font-semibold text-foreground">{t("sync.logs")}</h2>
        <pre className="h-full max-h-64 overflow-auto whitespace-pre-wrap break-all font-mono text-[11px] leading-relaxed text-muted">
          {status.logs.length > 0 ? status.logs.slice(-40).join("\n") : t("sync.empty_logs")}
        </pre>
      </section>

      <p className="text-[11px] text-muted">{t("sync.settings_hint")}</p>
    </div>
  );
}
