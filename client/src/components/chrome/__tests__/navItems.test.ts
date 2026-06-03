import { describe, expect, it } from "vitest";

import { activeNavKey, NAV_ITEMS } from "../navItems";

describe("activeNavKey", () => {
  it("matches Home only on the exact root path", () => {
    expect(activeNavKey("/")).toBe("home");
    // A deeper path must not fall back to Home.
    expect(activeNavKey("/setup")).not.toBe("home");
  });

  it("lights the primary destinations on their own routes", () => {
    expect(activeNavKey("/setup")).toBe("play");
    expect(activeNavKey("/multiplayer")).toBe("online");
    expect(activeNavKey("/draft")).toBe("draft");
    expect(activeNavKey("/my-decks")).toBe("decks");
  });

  it("keeps sub-routes under their section", () => {
    // Draft owns the quick-draft and pod sub-routes.
    expect(activeNavKey("/draft/quick")).toBe("draft");
    expect(activeNavKey("/draft-pod")).toBe("draft");
    // The deck builder is a child of Decks.
    expect(activeNavKey("/deck-builder")).toBe("decks");
    expect(activeNavKey("/deck-builder?returnTo=%2Fmy-decks")).toBe("decks");
  });

  it("returns null for routes with no primary nav item (e.g. coverage)", () => {
    expect(activeNavKey("/coverage")).toBeNull();
  });

  it("exposes exactly the five primary destinations", () => {
    expect(NAV_ITEMS.map((n) => n.key)).toEqual([
      "home",
      "play",
      "online",
      "draft",
      "decks",
    ]);
  });
});
