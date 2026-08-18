import { describe, expect, it } from "vitest";
import { createTabId, tabLabel } from "./tabs";

describe("createTabId", () => {
  it("returns a non-empty string, different each call", () => {
    const a = createTabId();
    const b = createTabId();
    expect(a).toBeTruthy();
    expect(b).toBeTruthy();
    expect(a).not.toEqual(b);
  });
});

describe("tabLabel", () => {
  it("uses the left path's basename", () => {
    expect(tabLabel("/projects/seam/src/session/compare.ts", "/projects/seam-fork/src/session/compare.ts")).toEqual("compare.ts");
  });

  it("falls back to the right path's basename if the left path is empty", () => {
    expect(tabLabel("", "/projects/seam-fork/src/compare.ts")).toEqual("compare.ts");
  });

  it("handles a bare filename with no directory component", () => {
    expect(tabLabel("compare.ts", "compare.ts")).toEqual("compare.ts");
  });
});
