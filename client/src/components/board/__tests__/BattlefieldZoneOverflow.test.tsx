import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { GameObject } from "../../../adapter/types.ts";
import { GameCardPreview } from "../../card/GameCardPreview.tsx";
import { useGameStore } from "../../../stores/gameStore.ts";
import { usePreferencesStore } from "../../../stores/preferencesStore.ts";
import { useUiStore } from "../../../stores/uiStore.ts";
import { buildGameObjectWithCoreTypes, buildObjectMap } from "../../../test/factories/gameObjectFactory.ts";
import { buildGameState, buildPlayers, buildPriorityWaitingFor } from "../../../test/factories/gameStateFactory.ts";
import type { GroupedPermanent as GroupedPermanentType } from "../../../viewmodel/battlefieldProps.ts";
import { BattlefieldZoneOverflow } from "../BattlefieldZoneOverflow.tsx";
import { BoardInteractionContext } from "../BoardInteractionContext.tsx";

vi.mock("../../card/CardImage.tsx", () => ({
  CardImage: ({ cardName }: { cardName: string }) => (
    <div aria-label={cardName} style={{ height: "var(--card-h)", width: "var(--card-w)" }} />
  ),
}));

vi.mock("../../../hooks/useCardImage.ts", () => ({
  useCardImage: () => ({ src: "card.png", isLoading: false, isRotated: false, isFlip: false }),
}));

vi.mock("../../../hooks/useEngineCardData.ts", () => ({
  useEngineCardData: () => null,
  useCardParseDetails: () => null,
  useCardRulings: () => [],
}));

function makeObject(id: number, coreTypes: string[] = ["Land"]): GameObject {
  return buildGameObjectWithCoreTypes(coreTypes, {
    id,
    card_id: id,
    zone: "Battlefield",
    name: `Permanent ${id}`,
    timestamp: id,
    entered_battlefield_turn: null,
    available_mana_pips: [{ type: "Color", data: "Green" }],
  });
}

function makeState(objects: Record<string, GameObject>) {
  const permanents = Object.values(objects);
  return buildGameState({
    players: buildPlayers([0]),
    objects,
    battlefield: permanents.map((object) => object.id),
    exile: [],
    stack: [],
    waiting_for: buildPriorityWaitingFor(),
  });
}

function makeGroups(count: number): GroupedPermanentType[] {
  return Array.from({ length: count }, (_, index) => {
    const id = index + 1;
    return {
      name: `Permanent ${id}`,
      ids: [id],
      count: 1,
      representative: {} as GroupedPermanentType["representative"],
    };
  });
}

function makeCreature(id: number, power = 2, toughness = 2): GameObject {
  return { ...makeObject(id, ["Creature"]), power, toughness };
}

function renderOverflow(options: {
  groups?: GroupedPermanentType[];
  includePreview?: boolean;
  objects?: Record<string, GameObject>;
  boardChoiceObjectIds?: Set<number>;
  selectableSacrificeObjectIds?: Set<number>;
  validTargetObjectIds?: Set<number>;
  committedAttackerIds?: Set<number>;
  zone?: "lands" | "support" | "creatures";
} = {}) {
  const groups = options.groups ?? makeGroups(9);
  const objects = options.objects ?? buildObjectMap(
    ...groups.flatMap((group) => group.ids).map((id) => makeObject(id)),
  );
  useGameStore.setState({ gameState: makeState(objects) });
  return render(
    <BoardInteractionContext.Provider
      value={{
        activatableObjectIds: new Set(),
        boardChoiceObjectIds: options.boardChoiceObjectIds ?? new Set(),
        committedAttackerIds: options.committedAttackerIds ?? new Set(),
        incomingAttackerCounts: new Map(),
        manaTappableObjectIds: new Set([1]),
        selectableSacrificeObjectIds: options.selectableSacrificeObjectIds ?? new Set(),
        selectableManaCostCreatureIds: new Set(),
        undoableTapObjectIds: new Set(),
        validAttackerIds: new Set(),
        validTargetObjectIds: options.validTargetObjectIds ?? new Set(),
      }}
    >
      <BattlefieldZoneOverflow
        groups={groups}
        zone={options.zone ?? "lands"}
        side="left"
      />
      {options.includePreview ? <GameCardPreview /> : null}
    </BoardInteractionContext.Provider>,
  );
}

describe("BattlefieldZoneOverflow", () => {
  beforeEach(() => {
    // BattlefieldRow instantiates a ResizeObserver for the creatures row; jsdom
    // has no implementation, so a no-op stub keeps inline creature renders alive.
    globalThis.ResizeObserver = class {
      observe() {}
      unobserve() {}
      disconnect() {}
    } as unknown as typeof ResizeObserver;
    Object.defineProperty(window, "innerWidth", { configurable: true, value: 1200 });
    Object.defineProperty(window, "innerHeight", { configurable: true, value: 800 });
    window.matchMedia = ((query: string) => ({
      matches: query === "(hover: hover)" || query === "(any-hover: hover)",
      media: query,
      onchange: null,
      addEventListener: vi.fn(),
      removeEventListener: vi.fn(),
      addListener: vi.fn(),
      removeListener: vi.fn(),
      dispatchEvent: vi.fn(),
    })) as unknown as typeof window.matchMedia;
    usePreferencesStore.setState({ cardPreviewMode: "follow", cardPreviewHoverDelayMs: 0 });
  });

  afterEach(() => {
    vi.useRealTimers();
    cleanup();
    useGameStore.setState({ gameState: null, spellCosts: {} });
    useUiStore.setState({ inspectedObjectId: null, inspectedFaceIndex: 0, previewSticky: false });
  });

  it("collapses crowded zones into a summary tile with animation anchor ids", () => {
    const { container } = renderOverflow();

    const summary = screen.getByRole("button", { name: /open lands drawer/i });
    expect(summary).toBeInTheDocument();
    expect(container.querySelector('[data-grouped-ids~="9"]')).toBe(summary);
  });

  it("collapses by distinct stack count, not body count", () => {
    // 7 Forests + 2 duals reads as 2 visible stacks — far under the threshold —
    // so a big-but-uniform land row must NOT collapse into the summary tile even
    // though its body count (9) is high. The crowding metric tracks distinct
    // stacks (what the player actually sees), not raw object count.
    Object.defineProperty(window, "innerWidth", { configurable: true, value: 500 });
    const groups: GroupedPermanentType[] = [
      { name: "Forest", ids: [1, 2, 3, 4, 5, 6, 7], count: 7, representative: {} as GroupedPermanentType["representative"] },
      { name: "Vernal Fen", ids: [8, 9], count: 2, representative: {} as GroupedPermanentType["representative"] },
    ];

    renderOverflow({ groups });

    expect(screen.queryByRole("button", { name: /open lands drawer/i })).toBeNull();
  });

  it("collapses once distinct stacks exceed the threshold", () => {
    // Nine distinct single-card stacks clear the desktop threshold (8), so the
    // row collapses into the summary tile — the aggregate-space signal the
    // stack-count metric is meant to catch.
    Object.defineProperty(window, "innerWidth", { configurable: true, value: 1400 });
    renderOverflow({ groups: makeGroups(9) });

    expect(screen.getByRole("button", { name: /open lands drawer/i })).toBeInTheDocument();
  });

  it("summarizes available land mana with counted pips", () => {
    renderOverflow();

    // Pill shows the untapped count and a "×" multiplier in separate spans.
    expect(screen.getByText("×")).toBeInTheDocument();
    expect(screen.getAllByAltText("G").length).toBeGreaterThanOrEqual(2);
    // Tooltip now reports tapped-vs-untapped availability (all 9 untapped here).
    expect(screen.getByText("9 of 9 untapped")).toBeInTheDocument();
  });

  it("opens and closes the drawer from the summary tile", () => {
    renderOverflow();

    fireEvent.click(screen.getByRole("button", { name: /open lands drawer/i }));
    expect(screen.getByRole("dialog", { name: /lands/i })).toBeInTheDocument();

    fireEvent.click(screen.getAllByRole("button", { name: "Close" })[1]);
    expect(screen.queryByRole("dialog", { name: /lands/i })).not.toBeInTheDocument();
  });

  it("surfaces action badges from board interaction state", () => {
    renderOverflow({ validTargetObjectIds: new Set([2]) });

    expect(screen.getByText(/target 1/i)).toBeInTheDocument();
    expect(screen.getByText(/mana 1/i)).toBeInTheDocument();
  });

  it("surfaces hidden board-choice permanents as interactive", () => {
    renderOverflow({ boardChoiceObjectIds: new Set([2]) });

    const summary = screen.getByRole("button", { name: /open lands drawer/i });
    expect(screen.getByText(/pick 1/i)).toBeInTheDocument();
    expect(summary.className).toContain("border-cyan-300");
  });

  it("uses the shared battlefield hover preview inside the drawer", () => {
    renderOverflow({ includePreview: true });

    fireEvent.click(screen.getByRole("button", { name: /open lands drawer/i }));
    const drawerCard = document.querySelector('[data-object-id="1"]');
    expect(drawerCard).not.toBeNull();

    fireEvent.mouseEnter(drawerCard as Element);

    expect(useUiStore.getState().inspectedObjectId).toBe(1);
    expect(screen.getAllByAltText("Permanent 1").length).toBeGreaterThan(0);
  });

  it("does not add a drawer-only hover preview when battlefield hover is disabled", () => {
    Object.defineProperty(window, "innerWidth", { configurable: true, value: 500 });
    renderOverflow({ includePreview: true });

    fireEvent.click(screen.getByRole("button", { name: /open lands drawer/i }));
    const drawerCard = document.querySelector('[data-object-id="1"]');
    expect(drawerCard).not.toBeNull();

    fireEvent.mouseEnter(drawerCard as Element);
    fireEvent.mouseMove(drawerCard as Element, { clientX: 24, clientY: 24 });

    expect(useUiStore.getState().inspectedObjectId).toBeNull();
    expect(document.querySelector("[data-card-preview]")).not.toBeInTheDocument();
  });

  it("opens the mobile preview from long-press when hover is disabled", () => {
    vi.useFakeTimers();
    Object.defineProperty(window, "innerWidth", { configurable: true, value: 500 });
    renderOverflow({ includePreview: true });

    fireEvent.click(screen.getByRole("button", { name: /open lands drawer/i }));
    const drawerCard = document.querySelector('[data-object-id="1"]');
    expect(drawerCard).not.toBeNull();

    fireEvent.pointerDown(drawerCard as Element, {
      button: 0,
      clientX: 10,
      clientY: 10,
      isPrimary: true,
      pointerId: 1,
      pointerType: "touch",
    });
    act(() => {
      vi.advanceTimersByTime(500);
    });

    expect(useUiStore.getState().inspectedObjectId).toBe(1);
    expect(screen.getAllByAltText("Permanent 1").length).toBeGreaterThan(0);
  });

  describe("creatures zone", () => {
    function renderCreatures(count: number, extra: { power?: number; toughness?: number } = {}) {
      const groups = makeGroups(count);
      const objects = Object.fromEntries(
        groups.flatMap((group) => group.ids).map((id) => [id, makeCreature(id, extra.power, extra.toughness)]),
      );
      return renderOverflow({ groups, objects, zone: "creatures" });
    }

    it("collapses to the scrollable overview only past the higher creature threshold", () => {
      renderCreatures(13);
      expect(screen.getByRole("button", { name: /open creatures drawer/i })).toBeInTheDocument();
    });

    it("keeps a moderate creature count inline (does not reuse the lower land threshold)", () => {
      // 9 groups would collapse a lands zone, but creatures get more room.
      renderCreatures(9);
      expect(screen.queryByRole("button", { name: /open creatures drawer/i })).not.toBeInTheDocument();
    });

    it("summarizes aggregate power/toughness in the overview header", () => {
      renderCreatures(13, { power: 2, toughness: 3 });
      expect(screen.getByText("26/39")).toBeInTheDocument();
    });

    it("floats combat participants to the front for priority visibility", () => {
      const groups = makeGroups(13);
      const objects = Object.fromEntries(
        groups.flatMap((group) => group.ids).map((id) => [id, makeCreature(id)]),
      );
      // Commit the last creature as an attacker; it should sort to the front.
      renderOverflow({ groups, objects, zone: "creatures", committedAttackerIds: new Set([13]) });
      const renderedIds = Array.from(document.querySelectorAll("[data-object-id]"))
        .map((el) => el.getAttribute("data-object-id"));
      expect(renderedIds[0]).toBe("13");
    });
  });
});
