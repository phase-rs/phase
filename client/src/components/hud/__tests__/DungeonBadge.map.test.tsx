import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { DungeonRoomView } from "../../../adapter/types.ts";
import { DungeonBadge } from "../HudBadges.tsx";

// The panel resolves its art through the Scryfall sidecars, which are static
// assets the test environment does not serve. Stubbing the service keeps these
// tests about the badge's behaviour — what opens the panel, and where the
// venture marker lands — rather than about image loading.
//
// The default (see `beforeEach`) is the card-table hit that four of the five
// dungeons take. Two tests below override it to REJECT, which is what the real
// service does for Undercity: its `double_faced_token` layout is excluded from
// `scryfall-data.json`, so only the token table can carry it. Both branches are
// real production paths, not error handling.
const fetchCardImageAssetByOracleId = vi.fn();
const fetchTokenImageByRef = vi.fn();

vi.mock("../../../services/scryfall.ts", () => ({
  fetchCardImageAssetByOracleId: (...args: unknown[]) =>
    fetchCardImageAssetByOracleId(...args),
  fetchTokenImageByRef: (...args: unknown[]) => fetchTokenImageByRef(...args),
  deriveImageUrl: (url: string, size: string) =>
    url.replace("/normal/", `/${size}/`),
}));

/** Lost Mine of Phandelver with the marker in Goblin Lair (room index 1).
 *  Shaped exactly like `DerivedViews.dungeon_rooms` — the engine is the only
 *  source of room names, edges and card geometry. */
function lostMine(overrides: Partial<DungeonRoomView> = {}): DungeonRoomView {
  return {
    dungeon: "LostMineOfPhandelver",
    dungeon_name: "Lost Mine of Phandelver",
    room: {
      index: 1,
      name: "Goblin Lair",
      text: "Create a 1/1 red Goblin creature token.",
    },
    room_count: 7,
    card: {
      oracle_id: "5c446a7f-0301-4343-b0df-146cf2db605b",
      scryfall_id: "59b11ff8-f118-4978-87dd-509dc0c8c932",
      face_name: "Lost Mine of Phandelver",
    },
    rooms: [
      {
        index: 0,
        name: "Cave Entrance",
        text: "Scry 1.",
        next_rooms: [1, 2],
        marker: { x_permille: 500, y_permille: 215 },
      },
      {
        index: 1,
        name: "Goblin Lair",
        text: "Create a 1/1 red Goblin creature token.",
        next_rooms: [3, 4],
        marker: { x_permille: 310, y_permille: 390 },
      },
      {
        index: 2,
        name: "Mine Tunnels",
        text: "Create a Treasure token.",
        next_rooms: [4, 5],
        marker: { x_permille: 690, y_permille: 390 },
      },
      {
        index: 3,
        name: "Storeroom",
        text: "Put a +1/+1 counter on target creature.",
        next_rooms: [6],
        marker: { x_permille: 180, y_permille: 610 },
      },
      {
        index: 4,
        name: "Dark Pool",
        text: "Each opponent loses 1 life and you gain 1 life.",
        next_rooms: [6],
        marker: { x_permille: 500, y_permille: 610 },
      },
    ],
    ...overrides,
  };
}

beforeEach(() => {
  // Default: the card table resolves. Individual tests override this to
  // exercise the Undercity fallback and the no-art path. Set explicitly rather
  // than left as a bare `vi.fn()` so a test that never reaches the image code
  // is not silently relying on `undefined` throwing inside the hook.
  fetchCardImageAssetByOracleId.mockResolvedValue({
    src: "https://cards.scryfall.io/normal/front/5/9/59b11ff8.jpg",
  });
  fetchTokenImageByRef.mockResolvedValue(null);
});

afterEach(() => {
  cleanup();
  vi.resetAllMocks();
});

describe("DungeonBadge map panel", () => {
  it("stays closed until the player hovers the dungeon name", () => {
    render(<DungeonBadge room={lostMine()} />);
    expect(screen.queryByRole("dialog")).toBeNull();
  });

  it("opens on hover and closes when the pointer leaves", async () => {
    render(<DungeonBadge room={lostMine()} />);
    const chip = screen.getByRole("button", { name: /venturing in/i });

    fireEvent.mouseEnter(chip);
    expect(await screen.findByRole("dialog")).toBeInTheDocument();

    fireEvent.mouseLeave(chip);
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
  });

  it("stays open after a click, so touch devices and pinning work", async () => {
    render(<DungeonBadge room={lostMine()} />);
    const chip = screen.getByRole("button", { name: /venturing in/i });

    fireEvent.click(chip);
    expect(await screen.findByRole("dialog")).toBeInTheDocument();

    // A pinned panel must survive the hover ending.
    fireEvent.mouseLeave(chip);
    await new Promise((resolve) => setTimeout(resolve, 120));
    expect(screen.getByRole("dialog")).toBeInTheDocument();

    // Escape releases the pin.
    fireEvent.keyDown(window, { key: "Escape" });
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
  });

  // CR 309.4a: the marker sits on the room the engine reports, positioned from
  // that room's own card geometry — never the first room, and never a position
  // the client computed.
  it("draws the venture marker at the current room's printed position", async () => {
    render(<DungeonBadge room={lostMine()} />);
    fireEvent.mouseEnter(screen.getByRole("button", { name: /venturing in/i }));

    const marker = await screen.findByTitle("Goblin Lair");
    // Room 1's marker is (310, 390) permille → 31% / 39% of the card face.
    expect(marker).toHaveStyle({ left: "31%", top: "39%" });
  });

  // CR 309.5a: only the rooms reachable from here are marked; the rest of the
  // card is left alone so the marker reads unambiguously.
  it("marks the rooms the marker can move to next, and no others", async () => {
    render(<DungeonBadge room={lostMine()} />);
    fireEvent.mouseEnter(screen.getByRole("button", { name: /venturing in/i }));
    await screen.findByRole("dialog");

    // Goblin Lair leads to Storeroom (3) and Dark Pool (4).
    expect(screen.getByTitle("Storeroom")).toBeInTheDocument();
    expect(screen.getByTitle("Dark Pool")).toBeInTheDocument();
    // Cave Entrance is behind the marker; Mine Tunnels is a sibling branch.
    expect(screen.queryByTitle("Cave Entrance")).toBeNull();
    expect(screen.queryByTitle("Mine Tunnels")).toBeNull();
  });

  // The Undercity path: absent from the card table, present in the token table.
  it("falls back to the token table when the card table has no entry", async () => {
    fetchCardImageAssetByOracleId.mockRejectedValue(new Error("not in local data"));
    fetchTokenImageByRef.mockResolvedValue(
      "https://cards.scryfall.io/normal/front/2/c/2c65185b.jpg",
    );

    render(<DungeonBadge room={lostMine()} />);
    fireEvent.mouseEnter(screen.getByRole("button", { name: /venturing in/i }));

    const image = await screen.findByRole("img", { name: "Lost Mine of Phandelver" });
    // And the panel upgrades to the `large` rung — these cards are floor plans
    // whose room text has to stay readable.
    expect(image).toHaveAttribute(
      "src",
      "https://cards.scryfall.io/large/front/2/c/2c65185b.jpg",
    );
  });

  it("still shows the map when no art resolves at all", async () => {
    fetchCardImageAssetByOracleId.mockRejectedValue(new Error("not in local data"));
    fetchTokenImageByRef.mockResolvedValue(null);

    render(<DungeonBadge room={lostMine()} />);
    fireEvent.mouseEnter(screen.getByRole("button", { name: /venturing in/i }));

    await screen.findByRole("dialog");
    // The marker layer does not depend on the art having loaded.
    expect(await screen.findByTitle("Goblin Lair")).toBeInTheDocument();
  });
});
