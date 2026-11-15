import { describe, expect, it } from "vitest";
import { withTimeout } from "./async";

describe("withTimeout", () => {
  it("returns the resolved value when the promise wins", async () => {
    await expect(withTimeout(Promise.resolve("ok"), 50, "late")).resolves.toBe(
      "ok",
    );
  });

  it("returns the fallback when the promise never settles", async () => {
    const never = new Promise<string>(() => {});
    await expect(withTimeout(never, 5, "fallback")).resolves.toBe("fallback");
  });
});
