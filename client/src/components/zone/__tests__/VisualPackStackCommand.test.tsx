import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { GameObject, StackEntryDisplay } from "../../../adapter/types.ts";
import { useCardImage } from "../../../hooks/useCardImage.ts";
import { useGameStore } from "../../../stores/gameStore.ts";
import { buildCommanderGameObject, buildGameObject, buildObjectMap } from "../../../test/factories/gameObjectFactory.ts";
import { buildGameState, buildPlayers, buildStackEntry } from "../../../test/factories/gameStateFactory.ts";
import { StackEntry } from "../../stack/StackEntry.tsx";
import { CommandDock } from "../CommandDock.tsx";
import { CommanderCardZone } from "../CommanderCardZone.tsx";
import { CommandZone } from "../CommandZone.tsx";

const imageMock = vi.hoisted(() => ({
  mode: "compact" as "compact" | "inline",
  results: new Map<string, {
    src: string | null;
    isLoading: boolean;
    isRotated: boolean;
    isFlip: boolean;
    rungs?: { small: string; normal: string };
    advanceFailedSource?: (src: string) => void;
  }>(),
}));

vi.mock("../../../hooks/useCardImage.ts", () => ({
  useCardImage: vi.fn((name: string) => imageMock.results.get(name) ?? {
    src: null,
    isLoading: false,
    isRotated: false,
    isFlip: false,
  }),
}));

vi.mock("../../../hooks/useResolvedCommandZoneDisplay.ts", () => ({
  useResolvedCommandZoneDisplay: () => imageMock.mode,
}));

vi.mock("../../../hooks/usePlayerId.ts", () => ({
  getPlayerId: () => 0,
  useCanActForWaitingState: () => true,
  usePlayerId: () => 0,
}));

vi.mock("../../../hooks/useIsMobile.ts", () => ({ useIsMobile: () => false }));
vi.mock("../../../hooks/useIsCompactHeight.ts", () => ({ useIsCompactHeight: () => false }));
vi.mock("../../../hooks/useSeatColor.ts", () => ({ useSeatColor: () => "#fff" }));
vi.mock("../../../hooks/useCardHover.ts", () => ({
  useCardHover: () => ({ handlers: {}, firedRef: { current: false } }),
}));
vi.mock("../../../hooks/useDragToCast.ts", () => ({ useDragToCast: () => () => false }));

function seed(objects: GameObject[], options: { commandZone?: number[]; stack?: unknown[] } = {}) {
  const gameState = buildGameState({
    objects: buildObjectMap(...objects),
    players: buildPlayers([0, 1]),
    command_zone: options.commandZone ?? [],
    stack: (options.stack ?? []) as never,
  });
  act(() => {
    useGameStore.setState({
      gameState,
      waitingFor: gameState.waiting_for,
      legalActions: [],
      legalActionsByObject: {},
      spellCosts: {},
    });
  });
}

function installedResult(prefix: string, advanceFailedSource = vi.fn()) {
  return {
    src: `${prefix}-normal.png`,
    isLoading: false,
    isRotated: false,
    isFlip: false,
    rungs: { small: `${prefix}-small.png`, normal: `${prefix}-normal.png` },
    advanceFailedSource,
  };
}

beforeEach(() => {
  imageMock.mode = "compact";
  imageMock.results.clear();
});

afterEach(() => {
  cleanup();
  useGameStore.getState().reset();
  vi.clearAllMocks();
});

describe("visual-pack stack and command surfaces", () => {
  it("binds a live stack source to its current face and advances the exact normal rung", () => {
    const advance = vi.fn();
    const source = buildGameObject({
      id: 42,
      card_id: 42,
      zone: "Stack",
      name: "Current DFC Face",
      transformed: true,
      printed_ref: { oracle_id: "dfc-oracle", face_name: "Current DFC Face" },
    });
    const entry = buildStackEntry({
      id: 77,
      source_id: source.id,
      controller: 0,
      kind: {
        type: "Spell",
        data: { card_id: source.card_id, actual_mana_spent: 0 },
      },
    });
    seed([source], { stack: [entry] });
    imageMock.results.set(source.name, installedResult("stack", advance));

    const { rerender } = render(
      <StackEntry
        entry={entry}
        index={0}
        isTop
        cardSize={{ width: 120, height: 168 }}
      />,
    );
    expect(vi.mocked(useCardImage)).toHaveBeenCalledWith(
      source.name,
      expect.objectContaining({
        faceIndex: 1,
        oracleId: "dfc-oracle",
        faceName: "Current DFC Face",
      }),
    );
    const image = screen.getByAltText(source.name);
    expect(image).toHaveAttribute("srcset", "stack-small.png 146w, stack-normal.png 488w");
    fireEvent.error(image);
    expect(advance).toHaveBeenCalledWith("stack-normal.png");

    imageMock.results.set(source.name, installedResult("stack-next"));
    rerender(
      <StackEntry
        entry={entry}
        index={0}
        isTop
        cardSize={{ width: 120, height: 168 }}
      />,
    );
    expect(screen.getByAltText(source.name)).toHaveAttribute("src", "stack-next-normal.png");

    imageMock.results.set(source.name, {
      src: null,
      isLoading: false,
      isRotated: false,
      isFlip: false,
    });
    rerender(
      <StackEntry
        entry={entry}
        index={0}
        isTop
        cardSize={{ width: 120, height: 168 }}
      />,
    );
    expect(screen.getByRole("img", { name: source.name })).toHaveTextContent(source.name);
  });

  it("uses only the captured token identity for a detached stack source", () => {
    const entry = buildStackEntry({
      id: 78,
      source_id: 900,
      controller: 0,
      kind: {
        type: "TriggeredAbility",
        data: {
          source_id: 900,
          source_name: "Detached Servo",
          ability: { targets: [] },
        },
      },
    });
    const details: StackEntryDisplay = {
      source_name: "Detached Servo",
      kind_label: "Triggered",
      token_image_ref: {
        scryfall_id: "detached-printing",
        scryfall_oracle_id: "detached-oracle",
        face_name: "Servo",
        preset_id: "servo-preset",
      },
    };
    seed([], { stack: [entry] });
    imageMock.results.set("Detached Servo", installedResult("detached"));

    render(
      <StackEntry
        entry={entry}
        details={details}
        index={0}
        isTop
        cardSize={{ width: 120, height: 168 }}
      />,
    );
    expect(vi.mocked(useCardImage)).toHaveBeenCalledWith(
      "Detached Servo",
      expect.objectContaining({
        isToken: true,
        tokenImageRef: expect.objectContaining({ scryfall_id: "detached-printing" }),
        oracleId: undefined,
      }),
    );
  });

  it("keeps compact and full commander requests on the same complete identity", () => {
    const advance = vi.fn();
    const commander = buildCommanderGameObject({
      name: "Commander Current Face",
      transformed: true,
      printed_ref: { oracle_id: "commander-oracle", face_name: "Commander Current Face" },
    });
    seed([commander], { commandZone: [commander.id] });
    imageMock.results.set(commander.name, installedResult("commander", advance));

    const { rerender } = render(<CommandDock playerId={0} isMirrored={false} />);
    const compact = screen.getByAltText(commander.name);
    expect(compact).toHaveAttribute(
      "srcset",
      "commander-small.png 146w, commander-normal.png 488w",
    );
    fireEvent.error(compact);
    expect(advance).toHaveBeenCalledWith("commander-normal.png");
    expect(vi.mocked(useCardImage)).toHaveBeenCalledWith(
      commander.name,
      expect.objectContaining({
        faceIndex: 1,
        oracleId: "commander-oracle",
        faceName: "Commander Current Face",
      }),
    );

    imageMock.results.set(commander.name, {
      src: null,
      isLoading: false,
      isRotated: false,
      isFlip: false,
    });
    rerender(<CommandDock playerId={0} isMirrored={false} />);
    expect(screen.getByRole("img", { name: commander.name })).toHaveTextContent(
      commander.name,
    );

    cleanup();
    vi.mocked(useCardImage).mockClear();
    advance.mockClear();
    imageMock.results.set(commander.name, installedResult("commander", advance));
    const { rerender: rerenderFull } = render(<CommanderCardZone playerId={0} />);
    const full = screen.getByAltText(commander.name);
    expect(full).toHaveAttribute(
      "srcset",
      "commander-small.png 146w, commander-normal.png 488w",
    );
    expect(vi.mocked(useCardImage)).toHaveBeenCalledWith(
      commander.name,
      expect.objectContaining({
        faceIndex: 1,
        oracleId: "commander-oracle",
        faceName: "Commander Current Face",
      }),
    );
    fireEvent.error(full);
    expect(advance).toHaveBeenCalledWith("commander-normal.png");

    imageMock.results.set(commander.name, installedResult("commander-next"));
    rerenderFull(<CommanderCardZone playerId={0} />);
    expect(screen.getByAltText(commander.name)).toHaveAttribute(
      "src",
      "commander-next-normal.png",
    );

    imageMock.results.set(commander.name, {
      src: null,
      isLoading: false,
      isRotated: false,
      isFlip: false,
    });
    rerenderFull(<CommanderCardZone playerId={0} />);
    expect(screen.getByRole("img", { name: commander.name })).toHaveTextContent(
      commander.name,
    );
  });

  it("keeps emblem source provenance on an art crop without a normal ladder", () => {
    const advance = vi.fn();
    const emblem = buildGameObject({
      id: 700,
      zone: "Command",
      owner: 0,
      controller: 0,
      name: "Emblem",
      is_emblem: true,
      emblem_source: {
        name: "Source Walker",
        printed_ref: { oracle_id: "walker-oracle", face_name: "Source Walker" },
      },
      static_definitions: [{ description: "You have no maximum hand size." }],
    });
    seed([emblem], { commandZone: [emblem.id] });
    imageMock.results.set("Source Walker", {
      ...installedResult("emblem", advance),
      rungs: undefined,
    });

    render(<CommandZone playerId={0} />);
    expect(vi.mocked(useCardImage)).toHaveBeenCalledWith(
      "Source Walker",
      expect.objectContaining({
        size: "art_crop",
        oracleId: "walker-oracle",
        faceName: "Source Walker",
      }),
    );
    const image = screen.getByAltText("Source Walker");
    expect(image).not.toHaveAttribute("srcset");
    fireEvent.error(image);
    expect(advance).toHaveBeenCalledWith("emblem-normal.png");
  });
});
