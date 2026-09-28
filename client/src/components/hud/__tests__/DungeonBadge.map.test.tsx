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

/** Baldur's Gate Wilderness with the marker in Grymforge (room index 7).
 *  All 19 rooms with the engine's names, edges and card geometry — the map
 *  panel indexes `rooms` by the marker's room, so a short or misordered graph
 *  misplaces the marker with no other failure. */
function baldursGate(overrides: Partial<DungeonRoomView> = {}): DungeonRoomView {
  return {
    dungeon: "BaldursGateWilderness",
    dungeon_name: "Baldur's Gate Wilderness",
    room: {
      index: 7,
      name: "Grymforge",
      text: "For each opponent, goad up to one target creature that player controls.",
    },
    room_count: 19,
    card: {
      oracle_id: "06b9590d-01bf-4fae-9837-352c9e04267a",
      scryfall_id: "a9d56324-8293-4500-a9ad-fed351ccf966",
      face_name: "Baldur's Gate Wilderness",
    },
    rooms: [
      { index: 0, name: "Crash Landing", text: "Search your library for a basic land card, reveal it, put it into your hand, then shuffle.", next_rooms: [1, 2, 3], marker: { x_permille: 500, y_permille: 170 } },
      { index: 1, name: "Goblin Camp", text: "Create a Treasure token.", next_rooms: [4], marker: { x_permille: 210, y_permille: 253 } },
      { index: 2, name: "Emerald Grove", text: "Create a 2/2 white Knight creature token.", next_rooms: [4, 5], marker: { x_permille: 500, y_permille: 253 } },
      { index: 3, name: "Auntie's Teahouse", text: "Scry 3.", next_rooms: [5], marker: { x_permille: 800, y_permille: 253 } },
      { index: 4, name: "Defiled Temple", text: "You may sacrifice a permanent. If you do, draw a card.", next_rooms: [6, 7], marker: { x_permille: 295, y_permille: 336 } },
      { index: 5, name: "Mountain Pass", text: "You may put a land card from your hand onto the battlefield.", next_rooms: [7, 8], marker: { x_permille: 730, y_permille: 336 } },
      { index: 6, name: "Ebonlake Grotto", text: "Create two 1/1 blue Faerie Dragon creature tokens with flying.", next_rooms: [9], marker: { x_permille: 210, y_permille: 440 } },
      { index: 7, name: "Grymforge", text: "For each opponent, goad up to one target creature that player controls.", next_rooms: [9, 10], marker: { x_permille: 500, y_permille: 440 } },
      { index: 8, name: "Githyanki Crèche", text: "Distribute three +1/+1 counters among up to three target creatures you control.", next_rooms: [10], marker: { x_permille: 800, y_permille: 440 } },
      { index: 9, name: "Last Light Inn", text: "Draw two cards.", next_rooms: [11, 12], marker: { x_permille: 285, y_permille: 545 } },
      { index: 10, name: "Reithwin Tollhouse", text: "Roll 2d4 and create that many Treasure tokens.", next_rooms: [12, 13], marker: { x_permille: 700, y_permille: 545 } },
      { index: 11, name: "Moonrise Towers", text: "Instant and sorcery spells you cast this turn cost {3} less to cast.", next_rooms: [14], marker: { x_permille: 210, y_permille: 622 } },
      { index: 12, name: "Gauntlet of Shar", text: "Each opponent loses 5 life.", next_rooms: [14, 15], marker: { x_permille: 500, y_permille: 622 } },
      { index: 13, name: "Balthazar's Lab", text: "Return up to two target creature cards from your graveyard to your hand.", next_rooms: [15], marker: { x_permille: 800, y_permille: 622 } },
      { index: 14, name: "Circus of the Last Days", text: "Create a token that's a copy of one of your commanders, except it's not legendary.", next_rooms: [16, 17], marker: { x_permille: 300, y_permille: 710 } },
      { index: 15, name: "Undercity Ruins", text: "Create three 4/1 black Skeleton creature tokens with menace.", next_rooms: [17, 18], marker: { x_permille: 715, y_permille: 710 } },
      { index: 16, name: "Steel Watch Foundry", text: 'You get an emblem with "Creatures you control get +2/+2 and have trample."', next_rooms: [], marker: { x_permille: 190, y_permille: 822 } },
      { index: 17, name: "Ansur's Sanctum", text: "Reveal the top four cards of your library and put them into your hand. Each opponent loses life equal to those cards' total mana value.", next_rooms: [], marker: { x_permille: 500, y_permille: 822 } },
      { index: 18, name: "Temple of Bhaal", text: "Creatures your opponents control get -5/-5 until end of turn.", next_rooms: [], marker: { x_permille: 810, y_permille: 822 } },
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

  // Regression: `fetchTokenImageAssetByRef` reaches `resolveImageUrl`, which
  // THROWS when the table holds an entry with no URL for the requested rung.
  // That call used to sit outside the hook's `try`, and the promise chain had
  // no `.catch`, so the rejection escaped: `setIsLoading(false)` never ran and
  // the panel sat on "Loading dungeon card…" forever. Resolving-to-null (the
  // test below) does NOT cover this — it is a different branch, which is how
  // the suite read green over the bug.
  it("shows the unavailable state when the token table REJECTS", async () => {
    fetchCardImageAssetByOracleId.mockRejectedValue(new Error("not in local data"));
    fetchTokenImageByRef.mockRejectedValue(new Error("No normal image for \"Undercity\""));

    render(<DungeonBadge room={lostMine()} />);
    fireEvent.mouseEnter(screen.getByRole("button", { name: /venturing in/i }));

    // Settles into the unavailable message rather than hanging on "Loading".
    expect(await screen.findByText("Dungeon card image unavailable")).toBeInTheDocument();
    expect(screen.queryByText("Loading dungeon card…")).toBeNull();
    // And the marker layer still renders, so the panel stays useful.
    expect(screen.getByTitle("Goblin Lair")).toBeInTheDocument();
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

  // Baldur's Gate Wilderness rides the same badge and panel — no per-dungeon
  // branch anywhere in the chain — but it is the only 19-room card, so the
  // position readout, the lattice geometry and the art lookup each get a pin
  // of their own rather than inheriting the 7-room fixture's.
  it("opens on hover for Baldur's Gate Wilderness and counts room 8 of 19", async () => {
    render(<DungeonBadge room={baldursGate()} />);
    const chip = screen.getByRole("button", {
      name: "Venturing in Baldur's Gate Wilderness, Grymforge, room 8 of 19",
    });
    expect(chip).toHaveTextContent("8/19");

    fireEvent.mouseEnter(chip);
    const dialog = await screen.findByRole("dialog");
    expect(dialog).toHaveTextContent("Baldur's Gate Wilderness");
    expect(dialog).toHaveTextContent("Grymforge — room 8 of 19");
  });

  // CR 309.4a: Grymforge's marker is (500, 440) permille → 50% / 44% of the
  // card face — the middle column of the lattice's widest row.
  it("draws the Wilderness marker at the current room's lattice position", async () => {
    render(<DungeonBadge room={baldursGate()} />);
    fireEvent.mouseEnter(screen.getByRole("button", { name: /venturing in/i }));

    const marker = await screen.findByTitle("Grymforge");
    expect(marker).toHaveStyle({ left: "50%", top: "44%" });
  });

  // CR 309.5a: Grymforge leads to Last Light Inn (9) and Reithwin Tollhouse
  // (10) — and nothing else on the 19-room card is marked.
  it("marks only the Wilderness rooms reachable from Grymforge", async () => {
    render(<DungeonBadge room={baldursGate()} />);
    fireEvent.mouseEnter(screen.getByRole("button", { name: /venturing in/i }));
    await screen.findByRole("dialog");

    expect(screen.getByTitle("Last Light Inn")).toBeInTheDocument();
    expect(screen.getByTitle("Reithwin Tollhouse")).toBeInTheDocument();
    // Behind the marker, a sibling branch, the far end of the card — and
    // Moonrise Towers, a row too far down to reach from here.
    expect(screen.queryByTitle("Ebonlake Grotto")).toBeNull();
    expect(screen.queryByTitle("Githyanki Crèche")).toBeNull();
    expect(screen.queryByTitle("Moonrise Towers")).toBeNull();
    expect(screen.queryByTitle("Temple of Bhaal")).toBeNull();
  });

  // The Wilderness is `layout: "normal"`, so it resolves through the card
  // table by oracle id — the Undercity token-table fallback is not its path.
  it("resolves Wilderness art through the card table by oracle id", async () => {
    fetchCardImageAssetByOracleId.mockResolvedValue({
      src: "https://cards.scryfall.io/normal/front/a/9/a9d56324-8293-4500-a9ad-fed351ccf966.jpg",
    });

    render(<DungeonBadge room={baldursGate()} />);
    fireEvent.mouseEnter(screen.getByRole("button", { name: /venturing in/i }));

    const image = await screen.findByRole("img", { name: "Baldur's Gate Wilderness" });
    expect(fetchCardImageAssetByOracleId).toHaveBeenCalledWith(
      "06b9590d-01bf-4fae-9837-352c9e04267a",
      "Baldur's Gate Wilderness",
      "normal",
    );
    // Upgraded to the `large` rung — the room text has to stay readable.
    expect(image).toHaveAttribute(
      "src",
      "https://cards.scryfall.io/large/front/a/9/a9d56324-8293-4500-a9ad-fed351ccf966.jpg",
    );
  });
});
