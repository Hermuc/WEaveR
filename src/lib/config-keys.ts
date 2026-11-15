/**
 * Config key paths understood by the Rust `config_get` / `config_set` commands.
 *
 * These strings are shared byte-for-byte with the backend (see
 * `src-tauri/src/config.rs::KEY_*`). Keeping them in one place means a typo
 * is a compile/lint-visible edit, not a silently ineffective runtime call —
 * exactly the failure mode that caused the "signed out but still signed in"
 * bug when the auto-login flag was written from three different places.
 */
export const CONFIG_KEYS = {
  ACCOUNT_AUTO_LOGIN: "account.auto_login",
} as const;

/**
 * Whether the startup flow may sign in with the saved credentials.
 *
 * An explicit sign-out writes `false`; every other value (including a
 * missing key, which `config_get` reports as `undefined`/`null`) keeps the
 * default "stay signed in" behaviour. `unknown` probe states must NOT be
 * treated as `false` here — only a deliberate `false` opts out.
 */
export function shouldAttemptAutoLogin(pref: unknown): boolean {
  return pref !== false;
}
