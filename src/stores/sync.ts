import { create } from "zustand";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

import { inTauri, tryInvoke, tryInvokeAction } from "@/lib/tauri";

export interface SyncDevice {
  channel: string;
  name: string;
  storages: string[];
}

export interface SyncProgress {
  name: string;
  done: number;
  total: number;
}

export interface SyncStats {
  uploaded: number;
  skipped: number;
  failed: number;
  filtered: number;
}

export interface SyncStatus {
  running: boolean;
  paused: boolean;
  transfer_paused: boolean;
  busy: boolean;
  device: SyncDevice | null;
  progress: SyncProgress | null;
  stats: SyncStats;
  queue: number;
  logs: string[];
}

const EMPTY_STATS: SyncStats = { uploaded: 0, skipped: 0, failed: 0, filtered: 0 };

export const EMPTY_SYNC_STATUS: SyncStatus = {
  running: false,
  paused: false,
  transfer_paused: false,
  busy: false,
  device: null,
  progress: null,
  stats: EMPTY_STATS,
  queue: 0,
  logs: [],
};

type SyncEvent =
  | { type: "log"; message: string }
  | { type: "device"; device: SyncDevice | null }
  | { type: "busy"; value: boolean }
  | { type: "progress"; name: string; done: number; total: number }
  | { type: "stats"; uploaded: number; skipped: number; failed: number; filtered: number }
  | { type: "queue"; count: number }
  | { type: "transfer_paused"; value: boolean };

interface SyncState extends SyncStatus {
  bind: () => Promise<UnlistenFn | undefined>;
  refresh: () => Promise<void>;
  start: () => Promise<boolean>;
  stop: () => Promise<boolean>;
  now: () => Promise<boolean>;
  pause: (paused: boolean) => Promise<boolean>;
  transferPause: (paused: boolean) => Promise<boolean>;
  applyEvent: (event: SyncEvent) => void;
}

export const useSyncStore = create<SyncState>((set, get) => ({
  ...EMPTY_SYNC_STATUS,

  applyEvent: (event) =>
    set((state) => {
      switch (event.type) {
        case "log":
          return { logs: [...state.logs, event.message].slice(-100) };
        case "device":
          return event.device
            ? { device: event.device }
            : { device: null, progress: null, busy: false };
        case "busy":
          return event.value
            ? { busy: true }
            : { busy: false, progress: null };
        case "progress":
          return { progress: { name: event.name, done: event.done, total: event.total } };
        case "stats":
          return {
            stats: {
              uploaded: event.uploaded,
              skipped: event.skipped,
              failed: event.failed,
              filtered: event.filtered,
            },
          };
        case "queue":
          return { queue: event.count };
        case "transfer_paused":
          return { transfer_paused: event.value };
        default:
          return {};
      }
    }),

  bind: async () => {
    if (!inTauri) return undefined;
    return listen<SyncEvent>("sync://event", (event) => {
      get().applyEvent(event.payload);
    });
  },

  refresh: async () => {
    const status = await tryInvoke<SyncStatus>("sync_status");
    if (status) set({ ...status });
  },

  start: async () => {
    const result = await tryInvokeAction("sync_start");
    if (result.ok) await get().refresh();
    return result.ok;
  },

  stop: async () => {
    const result = await tryInvokeAction("sync_stop");
    if (result.ok) await get().refresh();
    return result.ok;
  },

  now: async () => {
    const result = await tryInvokeAction("sync_now");
    return result.ok;
  },

  pause: async (paused: boolean) => {
    const result = await tryInvokeAction("sync_pause", { paused });
    if (result.ok) set({ paused });
    return result.ok;
  },

  transferPause: async (paused: boolean) => {
    const result = await tryInvokeAction("sync_transfer_pause", { paused });
    if (result.ok) set({ transfer_paused: paused });
    return result.ok;
  },
}));
