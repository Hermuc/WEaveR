import { describe, expect, it } from "vitest";
import { isVideoItem } from "./videoStream";

describe("isVideoItem", () => {
  it("recognises object tags from the workshop parser", () => {
    expect(isVideoItem({ tags: [{ tag: "Video", category: "Type" }] })).toBe(true);
    expect(isVideoItem({ tags: [{ tag: "视频", category: "类型" }] })).toBe(true);
  });

  it("recognises plain string tags", () => {
    expect(isVideoItem({ tags: ["Video"] })).toBe(true);
    expect(isVideoItem({ tags: ["动漫"] })).toBe(false);
  });

  it("is case-insensitive and tolerates missing tags", () => {
    expect(isVideoItem({ tags: [{ tag: "VIDEO" }] })).toBe(true);
    expect(isVideoItem({ tags: [] })).toBe(false);
    expect(isVideoItem({} as { tags?: unknown })).toBe(false);
    expect(isVideoItem(null)).toBe(false);
  });
});
