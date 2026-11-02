import { act, cleanup, render, screen, waitFor, within } from "@testing-library/react";
import type { ReactNode } from "react";
import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";

import { useGameStore } from "../../../stores/gameStore.ts";
import { usePreferencesStore } from "../../../stores/preferencesStore.ts";
import { useUiStore } from "../../../stores/uiStore.ts";
import { useMultiplayerStore } from "../../../stores/multiplayerStore.ts";
import { buildGameObjectWithCoreTypes, buildObjectMap } from "../../../test/factories/gameObjectFactory.ts";
import { loadScryfallData, type PrintingEntry } from "../../../services/scryfall.ts";
import type * as Platform from "../../../services/platform.ts";
import type { ResolvedAsset } from "../../../services/visualPacks/types.ts";
import { assetKey, catalogRoot, installedRevision, packId } from "../../../services/visualPacks/types.ts";
import { encodeCandidateKey } from "../../../services/visualPacks/candidateKeys.ts";
import {
  buildGameState,
  buildPlayers,
  buildPriorityWaitingFor,
} from "../../../test/factories/gameStateFactory.ts";
import { GameBoard } from "../GameBoard.tsx";

vi.mock("../ArchenemyPanel.tsx", () => ({
  ArchenemyPanel: () => null,
}));

vi.mock("../CombatLine.tsx", () => ({
  CombatLine: () => <div data-testid="combat-line" />,
}));

vi.mock("../PlanechasePanel.tsx", () => ({
  PlanechasePanel: () => null,
}));


vi.mock("../OpponentSeatHeader.tsx", () => ({
  OpponentSeatHeader: ({ playerId }: { playerId: number }) => (
    <div data-testid={`opponent-seat-header-${playerId}`} data-player-hud={String(playerId)} />
  ),
}));

vi.mock("../../flexlayout/DraggableWidget.tsx", () => ({
  DraggableWidget: ({ children }: { children: ReactNode }) => <>{children}</>,
}));

vi.mock("../../hand/OpponentHand.tsx", () => ({
  OpponentHand: ({ playerId }: { playerId?: number }) => (
    <div data-testid={`opponent-hand-${playerId ?? "focused"}`} />
  ),
}));

vi.mock("../../zone/ExilePile.tsx", () => ({
  ExilePile: () => null,
}));

vi.mock("../../zone/GraveyardPile.tsx", () => ({
  GraveyardPile: () => null,
}));

vi.mock("../../zone/LibraryPile.tsx", () => ({
  LibraryPile: () => null,
}));

const installed = vi.hoisted(() => new Map<string, ResolvedAsset[]>());
vi.mock("../../../services/platform.ts", async (original) => ({
  ...await original<typeof Platform>(),
  loadVisualPackBackend: async () => ({
    resolve: async (keys: { key: string }[]) => ({
      revision: installedRevision("0"),
      entries: keys.map((key, ordinal) => ({ key, ordinal, matches: installed.get(key.key) ?? [] })),
    }),
    subscribeRevision: async () => () => {},
  }),
}));
vi.mock("../../../hooks/useEngineCardData.ts", () => ({
  useEngineCardData: () => null, useLocalizedCardName: (name: string) => name,
}));
vi.mock("../../../hooks/useLocalizedCardContent.ts", () => ({ useLocalizedCardContent: () => null }));
const ORACLE = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
const A = "11111111-1111-4111-8111-111111111111";
const B = "22222222-2222-4222-8222-222222222222";
const V = "33333333-3333-4333-8333-333333333333";

beforeAll(async () => {
  const entry = { oracle_id: ORACLE, name: "Alpha", face_names: ["alpha"], layout: "normal",
    faces: [{ normal: "/canonical.jpg", art_crop: "/canonical-crop.jpg" }],
    mana_cost: "{1}", cmc: 1, type_line: "Creature",
    colors: [], color_identity: [], keywords: [] };
  const catalog: Record<string, PrintingEntry[]> = { [ORACLE]: [
    [A, "own", "1"], [B, "own", "2"], [V, "pin", "3"],
  ].map(([id, set, collector_number]) => ({
    id, set, collector_number, set_name: set, released_at: "2026-01-01", border_color: "black",
    frame_effects: [], full_art: false, faces: [{ normal: `/${id}.jpg`, art_crop: `/${id}-crop.jpg` }],
  })) };
  vi.stubGlobal("fetch", vi.fn(async (input: RequestInfo | URL) =>
    new Response(JSON.stringify(new URL(String(input), "http://localhost").pathname === "/scryfall-printings.json"
      ? catalog : { [ORACLE]: entry, alpha: entry }))));
  for (const id of [A, B, V]) {
    for (const rung of ["small", "normal", "art_crop"] as const) {
      installed.set(encodeCandidateKey("english_printing", [id, 0, rung === "art_crop" ? "art_crop" : "full_card", rung]), [{
        packId: packId("complete"), assetKey: assetKey("asset:v1:exact_printing:QQ"),
        catalogRoot: catalogRoot("a".repeat(64)), url: `/${id}${rung === "art_crop" ? "-crop" : ""}.jpg`, media: "image/jpeg",
      }]);
    }
  }
  await loadScryfallData();
});

function createFourPlayerState() {
  return buildGameState({
    players: buildPlayers([
      { id: 0, life: 40 },
      { id: 1, life: 40 },
      { id: 2, life: 40 },
      { id: 3, life: 40 },
    ]),
    waiting_for: buildPriorityWaitingFor(),
    seat_order: [0, 1, 2, 3],
    eliminated_players: [],
  });
}

describe("GameBoard multiplayer layout", () => {
  beforeEach(() => {
    useGameStore.setState({
      gameMode: "local",
      gameState: createFourPlayerState(),
      waitingFor: { type: "Priority", data: { player: 0 } },
      legalActionsByObject: {},
    });
    useUiStore.setState({
      focusedOpponent: 2,
      blockerAssignments: new Map(),
    });
    useMultiplayerStore.setState({ activePlayerId: 0, isSpectator: false });
    usePreferencesStore.setState({ ...usePreferencesStore.getInitialState(), multiplayerBoardLayout: "focused",
      showOwnerPrintings: true, language: "en", artChain: [], battlefieldCardDisplay: "full_card",
      artOverrides: { [ORACLE]: { scryfallId: V, setCode: "pin", collectorNumber: "3" } } });
  });

  afterEach(() => {
    cleanup();
    usePreferencesStore.setState(usePreferencesStore.getInitialState());
    useGameStore.setState({ gameState: null });
    useUiStore.setState(useUiStore.getInitialState());
  });

  it("defaults to the focused opponent plus local player", () => {
    render(<GameBoard effectiveMultiplayerBoardLayout="focused" oppHud={<div />} playerHud={<div />} />);

    expect(screen.getByTestId("player-area-2")).toBeInTheDocument();
    expect(screen.getByTestId("player-area-0")).toBeInTheDocument();
    expect(screen.queryByTestId("player-area-1")).toBeNull();
    expect(screen.queryByTestId("player-area-3")).toBeNull();
  });

  it("renders each live opponent once plus local player in split mode", () => {
    usePreferencesStore.setState({ multiplayerBoardLayout: "split" });

    render(<GameBoard effectiveMultiplayerBoardLayout="split" oppHud={<div data-testid="global-opponent-hud" />} playerHud={<div />} />);

    for (const playerId of [0, 1, 2, 3]) {
      expect(screen.getAllByTestId(`player-area-${playerId}`)).toHaveLength(1);
    }
    expect(screen.queryByTestId("global-opponent-hud")).toBeNull();
    expect(screen.getByTestId("opponent-seat-pane-1")).toHaveClass("group/opponent-seat");
    expect(screen.getByTestId("opponent-seat-header-1")).toBeInTheDocument();
    expect(document.querySelector('[data-player-hud="1"]')).toBeTruthy();
    expect(screen.getByTestId("opponent-seat-pane-2")).toHaveClass("group/opponent-seat");
    expect(screen.getByTestId("opponent-seat-header-2")).toBeInTheDocument();
    expect(document.querySelector('[data-player-hud="2"]')).toBeTruthy();
    expect(screen.getByTestId("opponent-seat-pane-3")).toHaveClass("group/opponent-seat");
    expect(screen.getByTestId("opponent-seat-header-3")).toBeInTheDocument();
    expect(document.querySelector('[data-player-hud="3"]')).toBeTruthy();
  });

  it.each(["focused", "split"] as const)("updates real collapsed creature and land surfaces on unchanged state (%s)", async (layout) => {
    const state = createFourPlayerState();
    const objects = [0, 1, 2, 3].flatMap((controller) =>
      (["Creature", "Land"] as const).flatMap((type, row) =>
        Array.from({ length: 10 }, (_, member) =>
          buildGameObjectWithCoreTypes([type], {
            id: controller * 100 + row * 20 + member + 1,
            name: "Alpha", owner: (controller + 1) % 4, controller, zone: "Battlefield",
            printed_ref: { oracle_id: ORACLE, face_name: "Alpha" },
            display_visible_to_viewer: true,
            printing: { set_code: "own", collector_number: member < 5 ? "1" : "2" },
          }))));
    state.objects = buildObjectMap(...objects);
    state.battlefield = objects.map((object) => object.id);
    useGameStore.setState({ gameState: state });
    useUiStore.setState({ selectedAttackers: [2] });
    const { container } = render(<GameBoard effectiveMultiplayerBoardLayout={layout} />);
    const seats = layout === "split" ? [0, 1, 2, 3] : [0, 2];
    for (const enabled of [true, false, true]) {
      act(() => usePreferencesStore.getState().setShowOwnerPrintings(enabled));
      for (const seat of seats) {
        const area = screen.getByTestId(`player-area-${seat}`);
        const expected = [0, 20].flatMap((row) => enabled
          ? [Array.from({ length: 5 }, (_, i) => seat * 100 + row + i + 1),
            Array.from({ length: 5 }, (_, i) => seat * 100 + row + i + 6)]
          : [Array.from({ length: 10 }, (_, i) => seat * 100 + row + i + 1)]);
        expect(Array.from(area.querySelectorAll("[data-grouped-ids]"),
          (element) => element.getAttribute("data-grouped-ids")!.split(" ").map(Number))
          .sort((a, b) => a[0] - b[0])).toEqual(expected);
        expect(within(area).getAllByText(enabled ? "×5" : "×10")).toHaveLength(enabled ? 4 : 2);
        await waitFor(() => {
          expect(area.querySelectorAll(`img[src="/${enabled ? A : V}.jpg"]`)).toHaveLength(2);
          expect(area.querySelectorAll(`img[src="/${B}.jpg"]`)).toHaveLength(enabled ? 2 : 0);
        });
      }
    }
    expect(container.querySelectorAll("[data-grouped-ids]")).toHaveLength(seats.length * 4);
    expect(useGameStore.getState().gameState).toBe(state);
    expect(useUiStore.getState().selectedAttackers).toEqual([2]);
  });
});
