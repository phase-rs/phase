import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { GameObject, GameState } from "../../../adapter/types.ts";
import { GameCardPreview } from "../../card/GameCardPreview.tsx";
import { useGameStore } from "../../../stores/gameStore.ts";
import { usePreferencesStore } from "../../../stores/preferencesStore.ts";
import { useUiStore } from "../../../stores/uiStore.ts";
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
  return {
    id,
    card_id: id,
    owner: 0,
    controller: 0,
    zone: "Battlefield",
    tapped: false,
    face_down: false,
    flipped: false,
    transformed: false,
    damage_marked: 0,
    dealt_deathtouch_damage: false,
    attached_to: null,
    attachments: [],
    counters: {},
    name: `Permanent ${id}`,
    power: null,
    toughness: null,
    loyalty: null,
    card_types: { supertypes: [], core_types: coreTypes, subtypes: [] },
    mana_cost: { type: "NoCost" },
    keywords: [],
    abilities: [],
    trigger_definitions: [],
    replacement_definitions: [],
    static_definitions: [],
    color: [],
    base_power: null,
    base_toughness: null,
    base_keywords: [],
    base_color: [],
    timestamp: id,
    entered_battlefield_turn: null,
    available_mana_pips: [{ type: "Color", data: "Green" }],
  };
}

function makeState(objects: Record<number, GameObject>): GameState {
  const ids = Object.keys(objects).map(Number);
  return {
    players: [
      {
        id: 0,
        life: 20,
        poison_counters: 0,
        mana_pool: { mana: [] },
        library: [],
        hand: [],
        graveyard: [],
        has_drawn_this_turn: false,
        lands_played_this_turn: 0,
        turns_taken: 0,
      },
    ],
    objects,
    battlefield: ids,
    exile: [],
    stack: [],
    combat: null,
    waiting_for: { type: "Priority", data: { player: 0 } },
  } as unknown as GameState;
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

function renderOverflow(options: {
  groups?: GroupedPermanentType[];
  includePreview?: boolean;
  objects?: Record<number, GameObject>;
  validTargetObjectIds?: Set<number>;
} = {}) {
  const groups = options.groups ?? makeGroups(9);
  const objects = options.objects ?? Object.fromEntries(
    groups.flatMap((group) => group.ids).map((id) => [id, makeObject(id)]),
  );
  useGameStore.setState({ gameState: makeState(objects) });
  return render(
    <BoardInteractionContext.Provider
      value={{
        activatableObjectIds: new Set(),
        committedAttackerIds: new Set(),
        incomingAttackerCounts: new Map(),
        manaTappableObjectIds: new Set([1]),
        selectableManaCostCreatureIds: new Set(),
        undoableTapObjectIds: new Set(),
        validAttackerIds: new Set(),
        validTargetObjectIds: options.validTargetObjectIds ?? new Set(),
      }}
    >
      <BattlefieldZoneOverflow groups={groups} zone="lands" side="left" />
      {options.includePreview ? <GameCardPreview /> : null}
    </BoardInteractionContext.Provider>,
  );
}

describe("BattlefieldZoneOverflow", () => {
  beforeEach(() => {
    Object.defineProperty(window, "innerWidth", { configurable: true, value: 1200 });
    Object.defineProperty(window, "innerHeight", { configurable: true, value: 800 });
    window.matchMedia = ((query: string) => ({
      matches: query === "(hover: hover)",
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

  it("collapses from actual object count, not visible group count", () => {
    Object.defineProperty(window, "innerWidth", { configurable: true, value: 500 });
    const groups: GroupedPermanentType[] = [
      { name: "Forest", ids: [1, 2], count: 2, representative: {} as GroupedPermanentType["representative"] },
      { name: "Vernal Fen", ids: [3], count: 1, representative: {} as GroupedPermanentType["representative"] },
      { name: "Swamp", ids: [4], count: 1, representative: {} as GroupedPermanentType["representative"] },
      { name: "Exotic Orchard", ids: [5], count: 1, representative: {} as GroupedPermanentType["representative"] },
    ];

    renderOverflow({ groups });

    expect(screen.getByRole("button", { name: /open lands drawer/i })).toBeInTheDocument();
  });

  it("summarizes available land mana with counted pips", () => {
    renderOverflow();

    expect(screen.getByText("9×")).toBeInTheDocument();
    expect(screen.getAllByAltText("G").length).toBeGreaterThanOrEqual(2);
    expect(screen.getByText(/hidden lands can currently produce/i)).toBeInTheDocument();
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
});
