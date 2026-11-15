import { useCallback, useEffect, useState } from "react";
import { useTranslation } from "@/i18n/hooks";
import { LogIn, LogOut, UserPlus } from "lucide-react";
import { listen } from "@tauri-apps/api/event";
import { EVENTS } from "@/lib/events";

import { inTauri, invoke, tryInvoke, tryInvokeAction } from "@/lib/tauri";
import { withTimeout } from "@/lib/async";
import { pushToast } from "@/stores/toasts";
import { useAppStore } from "@/stores/app";
import { useConfirm } from "@/hooks/useConfirm";
import { useSteamSessionStore, SteamAccountInfo } from "@/stores/steam-session";
import { triggerGlobalRefresh } from "@/stores/refresh";


/// A healthy sign-out is sub-second; this only fires when the backend is
/// genuinely stuck (held webview lock, hung Steam request) so the button can
/// never spin forever again.
const SIGN_OUT_TIMEOUT_MS = 15_000;

const persist = async (path: string, value: unknown) => {
  if (!inTauri) return;
  await invoke("config_set", { path, value }).catch(() => undefined);
};

/**
 * The account page.
 *
 * One account signs in for browsing and is used for downloads, so this renders
 * exactly one card with exactly one action: sign in when signed out, sign out
 * when signed in.
 *
 * Sign-in opens the SAME global `LoginModal` (OAuth / QR) that the startup
 * flow uses. An earlier version opened `DownloadLoginModal` here, which
 * only verifies the password via DepotDownloader and then tried to build a
 * browsing session by filling Steam's web form; that dead-ends at Steam
 * Guard and leaves the button spinning. One sign-in path, one truth.
 */
export default function AccountsSettingsTab({
  onOpenParser,
}: {
  onOpenParser: () => void;
}) {
  const { t } = useTranslation();

  return (
    <div className="space-y-5 p-4">
      <div className="space-y-2">
        <p className="text-xs font-medium tracking-wide text-subtle uppercase">
          {t("settings.account_section") || "Steam account"}
        </p>
        <p className="text-xs text-muted">
          {t("settings.account_section_description") ||
            "One account signs in for browsing and is used for downloads."}
        </p>
        <AccountCard onOpenParser={onOpenParser} />
      </div>
    </div>
  );
}

function AccountCard({ onOpenParser }: { onOpenParser: () => void }) {
  const { t } = useTranslation();
  const { confirm: showConfirm, ConfirmDialog } = useConfirm();
  const state = useAppStore();
  const setAccounts = useAppStore((s) => s.setAccounts);
  const sessionPhase = useSteamSessionStore((s) => s.phase);
  const account = useSteamSessionStore((s) => s.account);
  const setSessionLoggedIn = useSteamSessionStore((s) => s.setLoggedIn);
  const setSessionSignedOut = useSteamSessionStore((s) => s.setSignedOut);

  const [busy, setBusy] = useState(false);


  const refreshSession = useCallback(async () => {
    if (!inTauri) return;

    // One authoritative, three-state probe — the same one the startup path and
    // the login dialog use. `steam_is_logged_in` only reads the WebView2 cookie
    // and `steam_current_account` asks Steam; using them here meant this card
    // could disagree with the sidebar ("logged in" vs the real anonymous
    // session, or the reverse). A single probe keeps every surface consistent.
    const probe = await tryInvoke<{
      state: string;
      account?: SteamAccountInfo;
    }>("steam_probe_account", undefined, { state: "unknown" });

    if (probe?.state === "signed_in" && probe.account) {
      setSessionLoggedIn(probe.account);
    } else if (probe?.state === "signed_out") {
      // Neutral, not an error: nothing failed, the user is just anonymous.
      setSessionSignedOut();
    }
    // `unknown`: leave the current badge alone rather than claiming a sign-out.
  }, [setSessionLoggedIn, setSessionSignedOut]);

  const refreshAccounts = useCallback(async () => {
    if (!inTauri) return;
    const all = await tryInvoke<
      { index: number; username: string; is_custom: boolean }[]
    >("accounts_list", undefined, []);
    if (all) setAccounts(all);
  }, [setAccounts]);

  useEffect(() => {
    void refreshSession();
    void refreshAccounts();
  }, [refreshSession, refreshAccounts]);

  useEffect(() => {
    let unlisten: (() => void) | undefined;
    let active = true;

    if (inTauri) {
      void listen(EVENTS.STEAM_LOGIN_SUCCESS, async () => {
        await invoke<number>("steam_sync_cookies").catch(() => 0);
        pushToast(
          t("messages.signed_in_to_steam") || "Signed in to Steam",
          "success",
        );
        await refreshAccounts();
        await refreshSession();
        triggerGlobalRefresh();
      }).then((fn) => {
        if (active) {
          unlisten = fn;
        } else {
          fn();
        }
      });
    }

    return () => {
      active = false;
      if (unlisten) unlisten();
    };
  }, [t, refreshAccounts, refreshSession]);

  const openParser = async () => {
    if (!inTauri) return;
    await invoke<void>("steam_parser_show").catch(() => undefined);
  };

  const savedAccounts = state.accounts.filter((a) => a.is_custom);
  const savedUsername = savedAccounts[0]?.username ?? "";
  const loggedIn = sessionPhase === "logged-in" && !!account;
  const displayName =
    account?.persona_name?.trim() || account?.account_name?.trim() || savedUsername;

  // One button, one meaning: sign in when signed out, sign out when signed in.
  const primaryAction = async () => {
    if (!inTauri || busy) return;

    if (loggedIn) {
      const confirmed = await showConfirm({
        title: t("settings.sign_out") || "Sign out",
        message:
          t("settings.confirm_sign_out") ||
          "Sign out of Steam? Browsing age-gated items and downloads will stop until you sign in again.",
        confirmLabel: t("settings.sign_out") || "Sign out",
        cancelLabel: t("buttons.cancel") || "Cancel",
        variant: "danger",
      });
      if (!confirmed) return;
      setBusy(true);
      try {
        // Real sign-out: clears the Steam cookies, the in-memory jar and the
        // saved OAuth session. `steam_login_prepare` only navigates to the
        // login page and deliberately keeps the session, so it was a no-op here.
        //
        // Raced with a timeout: if the backend ever stalls (a lock held by a
        // bug, a hung webview call) the button would otherwise spin forever and
        // force a restart. 15s is far longer than a healthy sign-out (sub-second),
        // so this only fires when something is genuinely stuck.
        const res = await withTimeout(
          tryInvokeAction("steam_sign_out"),
          SIGN_OUT_TIMEOUT_MS,
          { ok: false as const, error: "timeout" },
        );
        if (!res.ok) {
          pushToast(
            `${t("messages.error") || "Could not sign out"}: ${res.error}`,
            "error",
          );
          return;
        }
        // The backend owns `account.auto_login`; do NOT write it here too.
        // Two writers for one value is exactly the kind of drift that made
        // "signed out" come back as "signed in" on the next launch.
        // Clear the badge immediately; the probe below confirms it.
        // Signed out is neutral — `setLoggedIn(null)` would paint it as an error.
        setSessionSignedOut();
        pushToast(t("settings.signed_out") || "Signed out of Steam", "success");
        await refreshSession();
        triggerGlobalRefresh();
      } finally {
        setBusy(false);
      }
      return;
    }

    // Signed out: the SAME OAuth/QR dialog the app uses everywhere else.
    useAppStore.getState().setLoginModalOpen(true, "manual");
  };

  // Removing the saved credential is a separate, destructive intent.
  const forgetAccount = async () => {
    if (!inTauri || !savedUsername) return;
    const confirmed = await showConfirm({
      title: t("labels.remove_account") || "Remove account",
      message:
        t("settings.confirm_remove_account") ||
        t("labels.remove_account_question", { user: savedUsername }),
      confirmLabel: t("buttons.remove") || "Remove",
      cancelLabel: t("buttons.cancel") || "Cancel",
      variant: "danger",
    });
    if (!confirmed) return;
    const res = await tryInvokeAction("accounts_remove_custom", {
      username: savedUsername,
    });
    if (res.ok) {
      pushToast(t("messages.removed"), "success");
      state.setAccountIndex(0);
      void persist("settings.account.account.account_number", 0);
      await refreshAccounts();
      await refreshSession();
    } else {
      pushToast(
        `${t("messages.error") || "Could not remove account"}: ${res.error}`,
        "error",
      );
    }
  };

  return (
    <div className="flex flex-col gap-3 rounded-md border border-border bg-surface-sunken p-3">
      <div className="flex items-center gap-3">
        {loggedIn && account?.avatar_url ? (
          <img
            src={account.avatar_url}
            alt=""
            className="size-9 rounded-full object-cover"
          />
        ) : (
          <div className="flex size-9 items-center justify-center rounded-full bg-surface-raised text-subtle">
            <UserPlus className="size-4" />
          </div>
        )}

        <div className="flex min-w-0 flex-1 flex-col">
          <span className="truncate text-sm font-medium">
            {loggedIn
              ? displayName
              : savedUsername || (t("settings.not_signed_in") || "Not signed in")}
          </span>
          <span className="truncate text-[11px] text-subtle">
            {loggedIn
              ? t("settings.signed_in_status") || "Signed in"
              : savedUsername
                ? t("settings.saved_not_signed_in") ||
                  "Saved account — not signed in"
                : t("settings.no_account_hint") ||
                  "Sign in to browse age-gated items and download"}
          </span>
        </div>

        {import.meta.env.DEV && (
          <div className="flex gap-1.5">
            <button className="btn-outline" onClick={openParser} disabled={!inTauri}>
              {t("settings.open_parser") || "Open Parser"}
            </button>
            <button className="btn-outline" onClick={onOpenParser} disabled={!inTauri}>
              {t("settings.parser_logs") || "Parser Logs"}
            </button>
          </div>
        )}
      </div>

      <div className="flex items-center gap-2">
        <button
          className="btn-primary inline-flex items-center gap-1.5"
          onClick={primaryAction}
          disabled={!inTauri || busy}
        >
          {busy ? (
            <span className="inline-block size-4 animate-spin rounded-full border-2 border-current border-t-transparent" />
          ) : loggedIn ? (
            <LogOut className="size-4" />
          ) : (
            <LogIn className="size-4" />
          )}
          {loggedIn
            ? t("settings.sign_out") || "Sign out"
            : t("settings.sign_in") || "Sign in with Steam"}
        </button>

        {/* Only offered once a credential exists, and clearly destructive. */}
        {savedUsername && (
          <button
            className="btn-ghost text-error"
            onClick={forgetAccount}
            disabled={!inTauri}
            title={t("settings.remove_account") || "Remove saved account"}
          >
            {t("settings.forget_account") || "Forget"}
          </button>
        )}
      </div>

      {/* Sign-in is the global LoginModal (App.tsx); no local dialog here. */}
      {ConfirmDialog}
    </div>
  );
}