import { describe, expect, it, beforeEach } from "vitest";
import { useSteamSessionStore, SteamAccountInfo } from "./steam-session";

const account: SteamAccountInfo = {
  persona_name: "Me",
  account_name: "me",
  steamid: "76561198000000000",
  profile_url: "https://steamcommunity.com/id/me",
  avatar_url: "",
};

describe("steam-session store", () => {
  beforeEach(() => {
    useSteamSessionStore.setState({ phase: "idle", account: null });
  });

  it("setLoggedIn marks the session signed in and stores the account", () => {
    useSteamSessionStore.getState().setLoggedIn(account);
    const s = useSteamSessionStore.getState();
    expect(s.phase).toBe("logged-in");
    expect(s.account).toEqual(account);
  });

  it("setSignedOut is neutral, never the error phase", () => {
    useSteamSessionStore.getState().setLoggedIn(account);
    useSteamSessionStore.getState().setSignedOut();
    const s = useSteamSessionStore.getState();
    expect(s.phase).toBe("signed-out");
    expect(s.account).toBeNull();
  });

  it("setError is the only action that yields the error phase", () => {
    useSteamSessionStore.getState().setError();
    expect(useSteamSessionStore.getState().phase).toBe("error");
  });

  it("setUnknownOrKeep turns idle into signed-out and otherwise keeps state", () => {
    useSteamSessionStore.getState().setUnknownOrKeep();
    expect(useSteamSessionStore.getState().phase).toBe("signed-out");

    useSteamSessionStore.getState().setLoggedIn(account);
    useSteamSessionStore.getState().setUnknownOrKeep();
    expect(useSteamSessionStore.getState().phase).toBe("logged-in");
  });
});