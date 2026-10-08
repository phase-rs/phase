import { describe, expect, it } from "vitest";

import { activeNavKey, navItemsFor, NAV_ITEMS } from "../navItems";

describe("activeNavKey", () => {
  it("matches Home only on the exact root path", () => {
    expect(activeNavKey("/")).toBe("home");
    // A deeper path must not fall back to Home.
    expect(activeNavKey("/setup")).not.toBe("home");
  });

  it("lights the visible primary destinations on their own routes", () => {
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
    // Tournament navigation is experimental and disabled in the default build.
    expect(activeNavKey("/tournament/ABC123")).toBeNull();
  });

  it("returns null for routes with no primary nav item (e.g. coverage)", () => {
    expect(activeNavKey("/coverage")).toBeNull();
  });

  it("hides the experimental tournament destination by default", () => {
    expect(NAV_ITEMS.map((n) => n.key)).toEqual([
      "home",
      "play",
      "online",
      "draft",
      "decks",
    ]);
  });

  it("includes tournaments when the experimental preference is enabled", () => {
    const navItems = navItemsFor(true);

    expect(navItems[navItems.length - 1]?.key).toBe("tournament");
    expect(activeNavKey("/tournament/ABC123", navItems)).toBe("tournament");
  });
});
