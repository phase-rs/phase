import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { GameObject } from "../../../adapter/types.ts";
import { dispatchAction } from "../../../game/dispatch.ts";
import { useGameStore } from "../../../stores/gameStore.ts";
import { useUiStore } from "../../../stores/uiStore.ts";
import {
  buildGameObject,
  buildObjectMap,
} from "../../../test/factories/gameObjectFactory.ts";
import {
  buildGameState,
  buildPlayers,
  buildPriorityWaitingFor,
} from "../../../test/factories/gameStateFactory.ts";
import { CommandDock } from "../CommandDock.tsx";

vi.mock("../../../game/dispatch.ts", () => ({
  dispatchAction: vi.fn(),
}));

const dockDisplay = vi.hoisted(() => ({ mode: "inline" as "compact" | "inline" }));

vi.mock("../../../hooks/useResolvedCommandZoneDisplay.ts", () => ({
  useResolvedCommandZoneDisplay: () => dockDisplay.mode,
}));

const localizedNames = vi.hoisted(() => new Map<string, string>());

vi.mock("../../../hooks/useEngineCardData.ts", () => ({
  useLocalizedCardName: (name: string | null) => localizedNames.get(name ?? "") ?? name,
}));

// Card art keyed by requested name: each Boss must resolve through the
// token-image path with its OWN art, so a shared stub would hide a mixup.
const KRANG_ART = "https://cards.scryfall.io/normal/front/8/7/krang.jpg";
const SLASH_ART = "https://cards.scryfall.io/normal/front/6/d/slash.jpg";
const cardImage = vi.hoisted(() => ({
  calls: [] as Array<{ name: string; options?: Record<string, unknown> }>,
}));

vi.mock("../../../hooks/useCardImage.ts", () => ({
  useCardImage: (name: string, options?: Record<string, unknown>) => {
    cardImage.calls.push({ name, options });
    const artByName: Record<string, string> = {
      "Krang, Dimension X Overlord": KRANG_ART,
      "Slash, the Dark Mirror": SLASH_ART,
    };
    return { src: artByName[name] ?? null, isLoading: false };
  },
}));

const KRANG_ID = 200;
const SLASH_ID = 201;
const PILE_BOSS_ID = 300;

function bossObject(options: {
  id: number;
  name: string;
  scryfallId: string;
  faceDown?: boolean;
}): GameObject {
  return buildGameObject({
    id: options.id,
    owner: 1,
    controller: 1,
    zone: "Command",
    name: options.name,
    face_down: options.faceDown ?? false,
    is_token: true,
    is_horde_deck_token: true,
    display_source: "Token",
    token_image_ref: {
      preset_id: "boss-preset",
      scryfall_id: options.scryfallId,
      scryfall_oracle_id: "boss-oracle",
    },
    card_types: { core_types: ["Boss"], supertypes: [], subtypes: [] },
  });
}

/** Stage-2 shape: two deployed Bosses plus one still face down in the pile. */
function seedStage2() {
  const krang = bossObject({
    id: KRANG_ID,
    name: "Krang, Dimension X Overlord",
    scryfallId: "8742baf6-2bf9-48ac-af68-6427704b3593",
  });
  const slash = bossObject({
    id: SLASH_ID,
    name: "Slash, the Dark Mirror",
    scryfallId: "6d84f8fc-0da6-4acc-9da2-9993daabd9f4",
  });
  // Hostile shape: the pile Boss sits in the objects map with its real
  // name (production redacts it to "Hidden Card", which would let a
  // pile-rendering bug hide behind the redaction). Only the projection's
  // deployed list decides what renders.
  const pileBoss = bossObject({
    id: PILE_BOSS_ID,
    name: "Chrome Dome, Cyber-Ninja",
    scryfallId: "9a2d23c4-d381-4a92-b4b1-0cf71948f941",
    faceDown: true,
  });
  const gameState = buildGameState({
    active_player: 0,
    priority_player: 0,
    players: buildPlayers([0, 1]),
    objects: buildObjectMap(krang, slash, pileBoss),
    command_zone: [KRANG_ID, SLASH_ID],
    battlefield: [],
    exile: [],
    stack: [],
    waiting_for: buildPriorityWaitingFor({ data: { player: 0 } }),
    derived: {
      horde: {
        horde_seat_ids: [1],
        hero_player_ids: [0],
        deployed_boss_ids: [KRANG_ID, SLASH_ID],
      },
    },
  });
  useGameStore.setState({
    gameMode: "local",
    gameState,
    waitingFor: gameState.waiting_for,
    legalActions: [],
    legalActionsByObject: {},
    spellCosts: {},
  });
  useUiStore.setState({
    inspectedObjectId: null,
    pendingAbilityChoice: null,
    debugInteractionMode: false,
  });
}

describe("CommandDock Horde Bosses", () => {
  beforeEach(() => {
    dockDisplay.mode = "inline";
    localizedNames.clear();
    cardImage.calls.length = 0;
    vi.mocked(dispatchAction).mockClear();
    useGameStore.setState({ gameState: null });
  });

  afterEach(() => {
    cleanup();
  });

  it("shows every deployed Boss with its own art and excludes the hidden pile", () => {
    seedStage2();

    render(<CommandDock playerId={1} isMirrored={false} />);

    const krang = screen.getByRole("img", { name: "Krang, Dimension X Overlord" });
    const slash = screen.getByRole("img", { name: "Slash, the Dark Mirror" });
    expect(krang).toHaveAttribute("src", KRANG_ART);
    expect(slash).toHaveAttribute("src", SLASH_ART);
    expect(screen.getAllByText("Boss")).toHaveLength(2);
    // The face-down pile Boss is neither listed by the projection nor rendered.
    expect(screen.queryByRole("img", { name: "Chrome Dome, Cyber-Ninja" })).toBeNull();
    expect(
      document.querySelector(`[data-object-id="${PILE_BOSS_ID}"]`),
    ).toBeNull();
    // Both cards resolve through the token-image path the engine stamped.
    const tokenCalls = cardImage.calls.filter(
      (call) => (call.options?.tokenImageRef as { scryfall_id?: string } | undefined)?.scryfall_id,
    );
    expect(tokenCalls.map((call) => call.name).sort()).toEqual([
      "Krang, Dimension X Overlord",
      "Slash, the Dark Mirror",
    ]);
  });

  it("inspects (never dispatches) when a Boss is clicked", () => {
    seedStage2();

    render(<CommandDock playerId={1} isMirrored={false} />);
    fireEvent.click(screen.getByRole("button", { name: /Krang/ }));

    expect(dispatchAction).not.toHaveBeenCalled();
    expect(useUiStore.getState().inspectedObjectId).toBe(KRANG_ID);
  });

  it("shows the representative Boss and a count badge in compact mode", () => {
    dockDisplay.mode = "compact";
    seedStage2();

    render(<CommandDock playerId={1} isMirrored={false} />);

    expect(screen.getByRole("img", { name: "Krang, Dimension X Overlord" })).toHaveAttribute(
      "src",
      KRANG_ART,
    );
    expect(screen.getByText("×2")).toBeInTheDocument();
  });

  it("renders nothing for the Horde seat with no deployed Bosses", () => {
    seedStage2();
    const state = useGameStore.getState().gameState;
    if (!state?.derived?.horde) throw new Error("seeded Horde view missing");
    state.derived.horde.deployed_boss_ids = [];
    state.command_zone = [];
    useGameStore.setState({
      gameState: { ...state, objects: {}, command_zone: [] },
    });

    const { container } = render(<CommandDock playerId={1} isMirrored={false} />);

    expect(container.firstChild).toBeNull();
  });
});
