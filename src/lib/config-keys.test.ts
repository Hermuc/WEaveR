import { describe, expect, it } from "vitest";
import { CONFIG_KEYS, shouldAttemptAutoLogin } from "./config-keys";

describe("CONFIG_KEYS", () => {
  it("matches the backend key byte-for-byte", () => {
    // Drifting here silently disables the sticky sign-out flag.
    expect(CONFIG_KEYS.ACCOUNT_AUTO_LOGIN).toBe("account.auto_login");
  });
});

describe("shouldAttemptAutoLogin", () => {
  it("opts out ONLY on an explicit false", () => {
    expect(shouldAttemptAutoLogin(false)).toBe(false);
  });

  it("keeps auto-login for true, missing, null and unexpected values", () => {
    // A missing key (old settings.json) or a failed config_get must not be
    // mistaken for "the user signed out".
    expect(shouldAttemptAutoLogin(true)).toBe(true);
    expect(shouldAttemptAutoLogin(undefined)).toBe(true);
    expect(shouldAttemptAutoLogin(null)).toBe(true);
    expect(shouldAttemptAutoLogin("unknown")).toBe(true);
  });
});
