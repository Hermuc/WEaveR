import { useEffect, useState } from "react";
import { open as openPath } from "@tauri-apps/plugin-dialog";
import { FolderOpen } from "lucide-react";

import { useTranslation } from "@/i18n/hooks";
import { inTauri, tryInvoke, tryInvokeAction } from "@/lib/tauri";
import { pushToast } from "@/stores/toasts";
import { Row, Section } from "./SettingsDialogShared";

interface SyncGeneral {
  sync_on_start: boolean;
  close_to_tray: boolean;
  device_poll_secs: number;
  settle_delay_secs: number;
  max_retries: number;
}

interface SyncConfig {
  general: SyncGeneral;
  source: { workshop_dir: string; mp4_only: boolean };
  filter: { min_mb: number; max_mb: number };
  target: { remote_dir: string; name_template: string; skip_existing: boolean };
  transport: { prefer: string; adb_path: string; mtp_storage_index: number; multipath?: Record<string, unknown> };
}

const DEFAULT_CONFIG: SyncConfig = {
  general: {
    sync_on_start: true,
    close_to_tray: true,
    device_poll_secs: 3,
    settle_delay_secs: 8,
    max_retries: 2,
  },
  source: { workshop_dir: "", mp4_only: true },
  filter: { min_mb: 0, max_mb: 0 },
  target: { remote_dir: "/sdcard/.WE", name_template: "{title} [{id}].{ext}", skip_existing: true },
  transport: {
    prefer: "auto",
    adb_path: "",
    mtp_storage_index: 0,
    multipath: { port: 5740, chunk_kb: 1024, agent_path: "", allow_single_link: false },
  },
};

export default function SyncSettingsTab() {
  const { t } = useTranslation();
  const [config, setConfig] = useState<SyncConfig>(DEFAULT_CONFIG);
  const [saving, setSaving] = useState(false);

  useEffect(() => {
    if (!inTauri) return;
    void tryInvoke<SyncConfig>("sync_config_get").then((value) => {
      if (value) setConfig({ ...DEFAULT_CONFIG, ...value });
    });
  }, []);

  const update = (patch: (current: SyncConfig) => SyncConfig) => {
    setConfig((current) => patch(current));
  };

  const save = async () => {
    setSaving(true);
    const result = await tryInvokeAction("sync_config_set", { config });
    setSaving(false);
    if (result.ok) {
      pushToast(t("settings.sync_saved"), "success");
    } else {
      pushToast(t("settings.sync_save_failed") + ": " + result.error, "error");
    }
  };

  const browseWorkshop = async () => {
    const picked = await openPath({ directory: true, multiple: false });
    if (!picked || Array.isArray(picked)) return;
    update((current) => ({ ...current, source: { ...current.source, workshop_dir: picked } }));
  };

  return (
    <div className="space-y-6 p-4">
      <Section title={t("settings.sync")}>
        <Row label={t("settings.sync_workshop_dir")} description={t("settings.sync_workshop_dir_hint")}>
          <div className="flex items-center gap-2">
            <input
              value={config.source.workshop_dir}
              onChange={(event) =>
                update((current) => ({ ...current, source: { ...current.source, workshop_dir: event.target.value } }))
              }
              placeholder="auto"
              className="w-56 rounded-md border border-white/10 bg-surface-sunken/40 px-2.5 py-1.5 text-xs text-foreground placeholder-white/25 focus:ring-1 focus:ring-primary/50 focus:outline-none"
            />
            <button
              onClick={() => void browseWorkshop()}
              aria-label={t("settings.sync_browse")}
              className="hover-shimmer flex size-8 items-center justify-center rounded-md bg-white/5 text-foreground transition-colors hover:bg-white/10"
            >
              <FolderOpen className="size-4" />
            </button>
          </div>
        </Row>
        <Row label={t("settings.sync_remote_dir")} description={t("settings.sync_remote_dir_hint")}>
          <input
            value={config.target.remote_dir}
            onChange={(event) =>
              update((current) => ({ ...current, target: { ...current.target, remote_dir: event.target.value } }))
            }
            placeholder="/sdcard/.WE"
            className="w-56 rounded-md border border-white/10 bg-surface-sunken/40 px-2.5 py-1.5 text-xs text-foreground placeholder-white/25 focus:ring-1 focus:ring-primary/50 focus:outline-none"
          />
        </Row>
        <Row label={t("settings.sync_name_template")} description={t("settings.sync_name_template_hint")}>
          <input
            value={config.target.name_template}
            onChange={(event) =>
              update((current) => ({ ...current, target: { ...current.target, name_template: event.target.value } }))
            }
            className="w-56 rounded-md border border-white/10 bg-surface-sunken/40 px-2.5 py-1.5 text-xs text-foreground focus:ring-1 focus:ring-primary/50 focus:outline-none"
          />
        </Row>
        <Row label={t("settings.sync_min_mb")}>
          <input
            type="number"
            min={0}
            value={config.filter.min_mb}
            onChange={(event) =>
              update((current) => ({ ...current, filter: { ...current.filter, min_mb: Number(event.target.value) || 0 } }))
            }
            className="w-24 rounded-md border border-white/10 bg-surface-sunken/40 px-2.5 py-1.5 text-xs text-foreground focus:ring-1 focus:ring-primary/50 focus:outline-none"
          />
        </Row>
        <Row label={t("settings.sync_max_mb")}>
          <input
            type="number"
            min={0}
            value={config.filter.max_mb}
            onChange={(event) =>
              update((current) => ({ ...current, filter: { ...current.filter, max_mb: Number(event.target.value) || 0 } }))
            }
            className="w-24 rounded-md border border-white/10 bg-surface-sunken/40 px-2.5 py-1.5 text-xs text-foreground focus:ring-1 focus:ring-primary/50 focus:outline-none"
          />
        </Row>
        <Row label={t("settings.sync_prefer")}>
          <select
            value={config.transport.prefer}
            onChange={(event) =>
              update((current) => ({ ...current, transport: { ...current.transport, prefer: event.target.value } }))
            }
            className="rounded-md border border-white/10 bg-surface-sunken/40 px-2.5 py-1.5 text-xs text-foreground focus:ring-1 focus:ring-primary/50 focus:outline-none"
          >
            <option value="auto">{t("settings.sync_prefer_auto")}</option>
            <option value="mtp">{t("settings.sync_prefer_mtp")}</option>
            <option value="adb">{t("settings.sync_prefer_adb")}</option>
          </select>
        </Row>
        <Row label={t("settings.sync_mtp_storage")} description={t("settings.sync_mtp_storage_hint")}>
          <input
            type="number"
            min={0}
            value={config.transport.mtp_storage_index}
            onChange={(event) =>
              update((current) => ({
                ...current,
                transport: { ...current.transport, mtp_storage_index: Number(event.target.value) || 0 },
              }))
            }
            className="w-24 rounded-md border border-white/10 bg-surface-sunken/40 px-2.5 py-1.5 text-xs text-foreground focus:ring-1 focus:ring-primary/50 focus:outline-none"
          />
        </Row>
        <Row label={t("settings.sync_adb_path")} description={t("settings.sync_adb_path_hint")}>
          <input
            value={config.transport.adb_path}
            onChange={(event) =>
              update((current) => ({ ...current, transport: { ...current.transport, adb_path: event.target.value } }))
            }
            placeholder="adb"
            className="w-56 rounded-md border border-white/10 bg-surface-sunken/40 px-2.5 py-1.5 text-xs text-foreground placeholder-white/25 focus:ring-1 focus:ring-primary/50 focus:outline-none"
          />
        </Row>
        <Row label={t("settings.sync_mp4_only")} description={t("settings.sync_mp4_only_hint")}>
          <input
            type="checkbox"
            checked={config.source.mp4_only}
            onChange={(event) =>
              update((current) => ({ ...current, source: { ...current.source, mp4_only: event.target.checked } }))
            }
            className="size-4 accent-primary"
          />
        </Row>
        <Row label={t("settings.sync_skip_existing")}>
          <input
            type="checkbox"
            checked={config.target.skip_existing}
            onChange={(event) =>
              update((current) => ({ ...current, target: { ...current.target, skip_existing: event.target.checked } }))
            }
            className="size-4 accent-primary"
          />
        </Row>
        <Row label={t("settings.sync_on_start")}>
          <input
            type="checkbox"
            checked={config.general.sync_on_start}
            onChange={(event) =>
              update((current) => ({ ...current, general: { ...current.general, sync_on_start: event.target.checked } }))
            }
            className="size-4 accent-primary"
          />
        </Row>
        <Row label={t("settings.sync_device_poll")}>
          <input
            type="number"
            min={1}
            value={config.general.device_poll_secs}
            onChange={(event) =>
              update((current) => ({
                ...current,
                general: { ...current.general, device_poll_secs: Number(event.target.value) || 1 },
              }))
            }
            className="w-24 rounded-md border border-white/10 bg-surface-sunken/40 px-2.5 py-1.5 text-xs text-foreground focus:ring-1 focus:ring-primary/50 focus:outline-none"
          />
        </Row>
        <Row label={t("settings.sync_settle_delay")}>
          <input
            type="number"
            min={0}
            value={config.general.settle_delay_secs}
            onChange={(event) =>
              update((current) => ({
                ...current,
                general: { ...current.general, settle_delay_secs: Number(event.target.value) || 0 },
              }))
            }
            className="w-24 rounded-md border border-white/10 bg-surface-sunken/40 px-2.5 py-1.5 text-xs text-foreground focus:ring-1 focus:ring-primary/50 focus:outline-none"
          />
        </Row>
        <Row label={t("settings.sync_max_retries")}>
          <input
            type="number"
            min={0}
            value={config.general.max_retries}
            onChange={(event) =>
              update((current) => ({
                ...current,
                general: { ...current.general, max_retries: Number(event.target.value) || 0 },
              }))
            }
            className="w-24 rounded-md border border-white/10 bg-surface-sunken/40 px-2.5 py-1.5 text-xs text-foreground focus:ring-1 focus:ring-primary/50 focus:outline-none"
          />
        </Row>
      </Section>
      <div className="flex justify-end">
        <button
          onClick={() => void save()}
          disabled={saving}
          className="hover-shimmer rounded-md bg-primary px-4 py-1.5 text-xs font-semibold text-white transition-opacity hover:opacity-90 disabled:opacity-50"
        >
          {t("settings.sync_save")}
        </button>
      </div>
    </div>
  );
}
