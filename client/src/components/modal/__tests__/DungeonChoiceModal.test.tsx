import { readFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

import { cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { DungeonPreview, GameState, WaitingFor } from "../../../adapter/types.ts";
import { CardChoiceModal } from "../CardChoiceModal.tsx";
import { isWaitingForHandled } from "../../../game/waitingForRegistry.ts";
import { useGameStore } from "../../../stores/gameStore.ts";
import { useMultiplayerStore } from "../../../stores/multiplayerStore.ts";
import { buildObjectMap } from "../../../test/factories/gameObjectFactory.ts";
import { buildGameState, buildPlayers } from "../../../test/factories/gameStateFactory.ts";

const dispatchMock = vi.fn();

vi.mock("../../../hooks/useGameDispatch.ts", () => ({
  useGameDispatch: () => dispatchMock,
}));

// The preview panel resolves its art through the Scryfall sidecars, which are
// static assets the test environment does not serve. The two fetch paths are
// stubbed like the DungeonBadge map tests so these stay about the choice's
// behaviour — what opens the preview, and which dungeon it shows — rather
// than image loading. A partial mock: `CardChoiceModal` transitively imports
// card-image layout that reads other exports of this module.
const fetchCardImageAssetByOracleId = vi.fn();
const fetchTokenImageByRef = vi.fn();

vi.mock("../../../services/scryfall.ts", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../../../services/scryfall.ts")>();
  return {
    ...actual,
    fetchCardImageAssetByOracleId: (...args: unknown[]) =>
      fetchCardImageAssetByOracleId(...args),
    fetchTokenImageByRef: (...args: unknown[]) => fetchTokenImageByRef(...args),
    deriveImageUrl: (url: string, size: string) =>
      url.replace("/normal/", `/${size}/`),
  };
});

// CR 309.4a: each dungeon option carries the topmost room it enters, plus the
// whole dungeon behind the choice (`card` + `rooms` + `room_count`) so the
// prompt can preview each card. Shaped exactly like the engine's
// `DungeonPreview` — room names, edges and card geometry are engine-authored.
const lostMineOption: DungeonPreview = {
  dungeon: "LostMineOfPhandelver",
  name: "Lost Mine of Phandelver",
  entry_room: { index: 0, name: "Cave Entrance", text: "Scry 1." },
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
    {
      index: 5,
      name: "Fungi Cavern",
      text: "Target creature gets -4/-0 until your next turn.",
      next_rooms: [6],
      marker: { x_permille: 810, y_permille: 610 },
    },
    {
      index: 6,
      name: "Temple of Dumathoin",
      text: "Draw a card.",
      next_rooms: [],
      marker: { x_permille: 500, y_permille: 800 },
    },
  ],
};

const tombOption: DungeonPreview = {
  dungeon: "TombOfAnnihilation",
  name: "Tomb of Annihilation",
  entry_room: { index: 0, name: "Trapped Entry", text: "Each player loses 1 life." },
  room_count: 5,
  card: {
    oracle_id: "d2ea2605-0ca0-4782-851d-e706bd0114e4",
    scryfall_id: "70b284bd-7a8f-4b60-8238-f746bdc5b236",
    face_name: "Tomb of Annihilation",
  },
  rooms: [
    {
      index: 0,
      name: "Trapped Entry",
      text: "Each player loses 1 life.",
      next_rooms: [1, 2],
      marker: { x_permille: 500, y_permille: 220 },
    },
    {
      index: 1,
      name: "Veils of Fear",
      text: "Each player loses 2 life unless they discard a card.",
      next_rooms: [3],
      marker: { x_permille: 290, y_permille: 400 },
    },
    {
      index: 2,
      name: "Oubliette",
      text: "Discard a card and sacrifice a creature, an artifact, and a land.",
      next_rooms: [4],
      marker: { x_permille: 720, y_permille: 480 },
    },
    {
      index: 3,
      name: "Sandfall Cell",
      text: "Each player loses 2 life unless they sacrifice a creature, artifact, or land of their choice.",
      next_rooms: [4],
      marker: { x_permille: 290, y_permille: 580 },
    },
    {
      index: 4,
      name: "Cradle of the Death God",
      text: "Create The Atropal, a legendary 4/4 black God Horror creature token with deathtouch.",
      next_rooms: [],
      marker: { x_permille: 500, y_permille: 775 },
    },
  ],
};

const chooseDungeon: WaitingFor = {
  type: "ChooseDungeon",
  data: {
    player: 0,
    options: [lostMineOption, tombOption],
  },
};

const CHOOSE_DUNGEON_FIXTURE_PATH = resolve(
  dirname(fileURLToPath(import.meta.url)),
  "../../../../../fixtures/adapter-contract/waiting_for_choose_dungeon.json",
);

/** Loads the engine-emitted `WaitingFor::ChooseDungeon` fixture (shape-guarded
 *  by the Rust deserialization-contract test in
 *  adapter_contract_fixtures.rs). Unlike the hand-shaped options above, these
 *  bytes passed through no test author — they are what the engine sends. */
function readChooseDungeonFixture(): WaitingFor {
  return JSON.parse(readFileSync(CHOOSE_DUNGEON_FIXTURE_PATH, "utf-8")) as WaitingFor;
}

// CR 309.5a: a branch point offers each reachable room with its printed effect.
const chooseRoom: WaitingFor = {
  type: "ChooseDungeonRoom",
  data: {
    player: 0,
    dungeon: "LostMineOfPhandelver",
    dungeon_name: "Lost Mine of Phandelver",
    options: [
      { index: 1, name: "Goblin Lair", text: "Create a 1/1 red Goblin creature token." },
      { index: 2, name: "Mine Tunnels", text: "Create a Treasure token." },
    ],
  },
};

function makeState(waitingFor: WaitingFor): GameState {
  return buildGameState({
    players: buildPlayers([0, 1]),
    objects: buildObjectMap(),
    next_object_id: 100,
    waiting_for: waitingFor,
    next_timestamp: 2,
  });
}

function mount(waitingFor: WaitingFor) {
  useMultiplayerStore.setState({ activePlayerId: 0 });
  useGameStore.setState({
    gameMode: "online",
    gameState: makeState(waitingFor),
    waitingFor,
  });
  render(<CardChoiceModal />);
}

describe("DungeonChoiceModal", () => {
  beforeEach(() => {
    dispatchMock.mockClear();
  });

  afterEach(() => {
    cleanup();
  });

  it("shows each dungeon's entry room and what it does", () => {
    mount(chooseDungeon);

    expect(screen.getByText("Cave Entrance")).toBeInTheDocument();
    expect(screen.getByText("Scry 1.")).toBeInTheDocument();
    expect(screen.getByText("Trapped Entry")).toBeInTheDocument();
    expect(screen.getByText("Each player loses 1 life.")).toBeInTheDocument();
  });

  it("dispatches the chosen dungeon", () => {
    mount(chooseDungeon);

    fireEvent.click(screen.getByText("Scry 1."));
    fireEvent.click(screen.getByRole("button", { name: "Confirm" }));

    expect(dispatchMock).toHaveBeenCalledWith({
      type: "ChooseDungeon",
      data: { dungeon: "LostMineOfPhandelver" },
    });
  });

  it("is registered as a handled waiting-for state", () => {
    expect(isWaitingForHandled(chooseDungeon)).toBe(true);
  });
});

describe("RoomChoiceModal", () => {
  beforeEach(() => {
    dispatchMock.mockClear();
  });

  afterEach(() => {
    cleanup();
  });

  it("shows each reachable room's name and printed effect", () => {
    mount(chooseRoom);

    expect(screen.getByText("Goblin Lair")).toBeInTheDocument();
    expect(screen.getByText("Create a 1/1 red Goblin creature token.")).toBeInTheDocument();
    expect(screen.getByText("Mine Tunnels")).toBeInTheDocument();
    expect(screen.getByText("Create a Treasure token.")).toBeInTheDocument();
  });

  it("dispatches the engine's room index, not the button position", () => {
    mount(chooseRoom);

    fireEvent.click(screen.getByText("Create a Treasure token."));
    fireEvent.click(screen.getByRole("button", { name: "Confirm" }));

    expect(dispatchMock).toHaveBeenCalledWith({
      type: "ChooseDungeonRoom",
      data: { room_index: 2 },
    });
  });

  it("is registered as a handled waiting-for state", () => {
    expect(isWaitingForHandled(chooseRoom)).toBe(true);
  });
});

/** The option button showing an entry-room effect. Resolved BEFORE hovering:
 *  the open preview repeats the entry text, so a lookup after hover would
 *  match twice. */
function optionButton(entryText: string): HTMLElement {
  const button = screen.getByText(entryText).closest("button");
  if (!button) throw new Error(`no dungeon option shows ${entryText}`);
  return button;
}

/** Distinct art per engine identity: the preview must resolve each dungeon's
 *  own card, so the mock answers by oracle id and fails anything else rather
 *  than serving one image for every request. */
const ART_BY_ORACLE_ID: Record<string, string> = {
  "5c446a7f-0301-4343-b0df-146cf2db605b":
    "https://cards.scryfall.io/normal/front/5/9/59b11ff8.jpg",
  "d2ea2605-0ca0-4782-851d-e706bd0114e4":
    "https://cards.scryfall.io/normal/front/7/0/70b284bd.jpg",
};

describe("DungeonChoiceModal preview", () => {
  beforeEach(() => {
    dispatchMock.mockClear();
    fetchCardImageAssetByOracleId.mockImplementation((oracleId: string) => {
      const src = ART_BY_ORACLE_ID[oracleId];
      return src
        ? Promise.resolve({ src })
        : Promise.reject(new Error(`unexpected oracle id ${oracleId}`));
    });
    fetchTokenImageByRef.mockResolvedValue(null);
  });

  afterEach(() => {
    cleanup();
    vi.resetAllMocks();
  });

  it("stays closed until the player hovers an option", () => {
    mount(chooseDungeon);
    expect(screen.queryByRole("dialog")).toBeNull();
  });

  it("previews the hovered dungeon's card with the marker on its entry room", async () => {
    mount(chooseDungeon);
    const lostMine = optionButton("Scry 1.");

    fireEvent.mouseEnter(lostMine);

    // The panel names the hovered dungeon and places it at its entry:
    // CR 309.4a puts the marker on room index 0, counted as room 1.
    const panel = await screen.findByRole("dialog", { name: /Lost Mine of Phandelver/ });
    expect(panel).toHaveAccessibleName(
      "Venturing in Lost Mine of Phandelver, Cave Entrance, room 1 of 7",
    );
    // Cave Entrance sits at (500, 215) permille → 50% / 21.5% of the card.
    expect(screen.getByTitle("Cave Entrance")).toHaveStyle({ left: "50%", top: "21.5%" });
    // The rooms reachable from the entry are dotted; the rest are not drawn.
    expect(screen.getByTitle("Goblin Lair")).toBeInTheDocument();
    expect(screen.getByTitle("Mine Tunnels")).toBeInTheDocument();
    expect(screen.queryByTitle("Dark Pool")).toBeNull();
    // And the printed card resolves through the engine-provided identity,
    // upgraded to the `large` rung so the room text stays readable.
    expect(await screen.findByRole("img", { name: "Lost Mine of Phandelver" })).toHaveAttribute(
      "src",
      "https://cards.scryfall.io/large/front/5/9/59b11ff8.jpg",
    );
  });

  it("closes the preview when the pointer leaves the option", async () => {
    mount(chooseDungeon);
    const lostMine = optionButton("Scry 1.");

    fireEvent.mouseEnter(lostMine);
    expect(await screen.findByRole("dialog")).toBeInTheDocument();

    fireEvent.mouseLeave(lostMine);
    expect(screen.queryByRole("dialog")).toBeNull();
  });

  // The genuine engine→client contract: the committed fixture is bytes the
  // engine itself emitted, and the real payload renders and previews without
  // any hand shaping in between.
  it("renders the engine-emitted fixture and previews it on hover", async () => {
    mount(readChooseDungeonFixture());
    const lostMine = optionButton("Scry 1.");

    fireEvent.mouseEnter(lostMine);

    const panel = await screen.findByRole("dialog", { name: /Lost Mine of Phandelver/ });
    expect(panel).toHaveAccessibleName(
      "Venturing in Lost Mine of Phandelver, Cave Entrance, room 1 of 7",
    );
    expect(screen.getByTitle("Cave Entrance")).toHaveStyle({ left: "50%", top: "21.5%" });
    expect(await screen.findByRole("img", { name: "Lost Mine of Phandelver" })).toHaveAttribute(
      "src",
      "https://cards.scryfall.io/large/front/5/9/59b11ff8.jpg",
    );
  });

  // Selection and preview are independent: clicking pins the choice, hovering
  // previews whichever card the pointer is over.
  it("follows the hovered option rather than the selected one", async () => {
    mount(chooseDungeon);
    const lostMine = optionButton("Scry 1.");
    const tomb = optionButton("Each player loses 1 life.");

    fireEvent.click(tomb);
    fireEvent.mouseEnter(lostMine);

    expect(
      await screen.findByRole("dialog", { name: /Lost Mine of Phandelver/ }),
    ).toBeInTheDocument();
    expect(screen.queryByRole("dialog", { name: /Tomb of Annihilation/ })).toBeNull();
  });

  // Touch taps and keyboard focus both focus the button, which is the only
  // preview gesture those inputs have.
  it("previews on focus and closes on blur", async () => {
    mount(chooseDungeon);
    const tomb = optionButton("Each player loses 1 life.");

    fireEvent.focus(tomb);
    expect(
      await screen.findByRole("dialog", { name: /Tomb of Annihilation/ }),
    ).toBeInTheDocument();

    fireEvent.blur(tomb);
    expect(screen.queryByRole("dialog")).toBeNull();
  });

  // Keyboard focus opens the preview; brushing the pointer across and off the
  // button must not close what focus opened.
  it("keeps a focused option's preview open across mouse leave", async () => {
    mount(chooseDungeon);
    const tomb = optionButton("Each player loses 1 life.");

    fireEvent.focus(tomb);
    expect(
      await screen.findByRole("dialog", { name: /Tomb of Annihilation/ }),
    ).toBeInTheDocument();

    fireEvent.mouseEnter(tomb);
    fireEvent.mouseLeave(tomb);
    expect(screen.getByRole("dialog", { name: /Tomb of Annihilation/ })).toBeInTheDocument();

    fireEvent.blur(tomb);
    expect(screen.queryByRole("dialog")).toBeNull();
  });

  // The preview announces every room, not just the entry: a screen-reader
  // user compares dungeons by their rooms. "Goblin Lair" appears nowhere on
  // the Lost Mine button, so it can only come from the announced list.
  it("announces every room of the previewed dungeon", async () => {
    mount(chooseDungeon);

    fireEvent.mouseEnter(optionButton("Scry 1."));
    expect(
      await screen.findByRole("dialog", { name: /Lost Mine of Phandelver/ }),
    ).toBeInTheDocument();

    const list = screen.getByRole("list");
    expect(within(list).getAllByRole("listitem")).toHaveLength(lostMineOption.room_count);
    expect(within(list).getByText(/Goblin Lair/)).toBeInTheDocument();
    expect(
      within(list).getByText(/Create a 1\/1 red Goblin creature token\./),
    ).toBeInTheDocument();
    expect(within(list).getByText(/Cave Entrance/)).toBeInTheDocument();
  });

  // The preview resolves art by the hovered dungeon's engine-provided
  // identity — hovering another option must swap the image, not reuse it.
  it("resolves each preview's art from that dungeon's own identity", async () => {
    mount(chooseDungeon);
    const lostMine = optionButton("Scry 1.");
    const tomb = optionButton("Each player loses 1 life.");

    fireEvent.mouseEnter(lostMine);
    expect(await screen.findByRole("img", { name: "Lost Mine of Phandelver" })).toHaveAttribute(
      "src",
      "https://cards.scryfall.io/large/front/5/9/59b11ff8.jpg",
    );

    fireEvent.mouseEnter(tomb);
    expect(await screen.findByRole("img", { name: "Tomb of Annihilation" })).toHaveAttribute(
      "src",
      "https://cards.scryfall.io/large/front/7/0/70b284bd.jpg",
    );
    expect(fetchCardImageAssetByOracleId).toHaveBeenCalledWith(
      "d2ea2605-0ca0-4782-851d-e706bd0114e4",
      "Tomb of Annihilation",
      "normal",
    );
  });
});
