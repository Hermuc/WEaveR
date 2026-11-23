import { create } from "zustand";

/**
 * Tracks the state of the hidden Steam webview's automatic login into the
 * dedicated parser account. Mirrors the lifecycle of the `steam_auto_login`
 * call fired during bootstrap so the sidebar can surface a small status
 * section (spinner while logging in, signed-in / error states once it
 * settles).
 *
 *  - `logging-in`  — auto-login is in flight (animated spinner).
 *  - `logged-in`   — signed in to Steam.
 *  - `signed-out`  — no account is signed in, but nothing went wrong. The
 *                    Workshop still works anonymously, so this is a neutral
 *                    state, not an error. It is what the user gets after
 *                    dismissing the login prompt.
 *  - `error`       — a sign-in attempt actually failed (bad password, a Steam
 *                    Guard challenge that never completed, …).
 *  - `idle`        — nothing has happened yet (also the non-Tauri default).
 */
export type SteamSessionPhase =
  | "idle"
  | "logging-in"
  | "logged-in"
  | "signed-out"
  | "error"
  | "offline";

export interface SteamAccountInfo {
  persona_name: string;
  account_name: string;
  steamid: string;
  profile_url: string;
  avatar_url: string;
}

interface SteamSessionStore {
  phase: SteamSessionPhase;
  account: SteamAccountInfo | null;
  setPhase: (phase: SteamSessionPhase) => void;
  setLoggingIn: () => void;
  /// Mark the session as signed in.
  ///
  /// Takes a NON-null account: "signed out" has its own action (`setSignedOut`).
  /// This used to accept `null` and map it to the `error` phase, which is how
  /// "signed out" ended up rendered as the red "login failed" badge. The stricter
  /// signature makes that mistake a compile error instead of a UI bug.
  setLoggedIn: (account: SteamAccountInfo) => void;
  setError: () => void;
  /// No account signed in, but nothing failed.
  setSignedOut: () => void;
  /// The probe could not reach Steam at all (network blocked/unavailable).
  setOffline: () => void;
  /// The probe could not reach Steam; keep whatever we already knew.
  setUnknownOrKeep: () => void;
}

export const useSteamSessionStore = create<SteamSessionStore>((set) => ({
  phase: "idle",
  account: null,
  setPhase: (phase) => set({ phase }),
  setLoggingIn: () => set({ phase: "logging-in" }),
  setLoggedIn: (account) => set({ phase: "logged-in", account }),
  setError: () => set({ phase: "error", account: null }),
  setSignedOut: () => set({ phase: "signed-out", account: null }),
  setOffline: () => set({ phase: "offline", account: null }),
  setUnknownOrKeep: () =>
    set((s) => (s.phase === "idle" ? { phase: "signed-out" } : {})),
}));
