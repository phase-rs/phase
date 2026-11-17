import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { dispatchAction } from "../../../game/dispatch.ts";
import { useCardImage } from "../../../hooks/useCardImage.ts";
import { useGameStore } from "../../../stores/gameStore.ts";
import { usePreferencesStore } from "../../../stores/preferencesStore.ts";
import { useUiStore } from "../../../stores/uiStore.ts";
import { buildGameObject, buildObjectMap } from "../../../test/factories/gameObjectFactory.ts";
import { buildGameState, buildPlayers, buildPriorityWaitingFor } from "../../../test/factories/gameStateFactory.ts";
import { BoardInteractionContext } from "../BoardInteractionContext.tsx";
import { PermanentCard } from "../PermanentCard.tsx";

// Regression tests for #11008: a permanent whose art failed offers its reload
// on the tile on a desktop-sized screen, and the reload's tap never acts on the
// permanent. A phone's tile stays the permanent's tap target.

vi.mock("../../../game/dispatch.ts", () => ({
  dispatchAction: vi.fn(),
  dispatchInteraction: vi.fn(),
}));

vi.mock("../../../hooks/useCardImage.ts", () => ({
  useCardBackImage: vi.fn(() => ({ src: "card-back.png", isLoading: false })),
  useCardImage: vi.fn(),
}));

vi.mock("../../../hooks/useEngineCardData.ts", () => ({
  useEngineCardData: () => null,
  useLocalizedCardName: (name: string | null) => name,
}));

const viewport = vi.hoisted(() => ({ isMobile: false }));

vi.mock("../../../hooks/useIsMobile.ts", () => ({
  useIsMobile: () => viewport.isMobile,
}));

const CARD_NAME = "Etherium Sculptor";
const RELOAD = "Tap to reload art";

/** Renders the sculptor as the one permanent, a valid target, so a click on
 *  the permanent itself dispatches `ChooseTarget`. */
function renderTargetableSculptor() {
  const sculptor = buildGameObject({
    id: 1,
    card_id: 100,
    zone: "Battlefield",
    name: CARD_NAME,
    power: 1,
    toughness: 2,
    card_types: { supertypes: [], core_types: ["Artifact", "Creature"], subtypes: ["Vedalken", "Artificer"] },
    printed_ref: { oracle_id: "oracle-etherium-sculptor", face_name: CARD_NAME },
  });
  const gameState = buildGameState({
    players: buildPlayers([0, 1]),
    objects: buildObjectMap(sculptor),
    battlefield: [1],
    exile: [],
    stack: [],
    waiting_for: buildPriorityWaitingFor(),
  });
  useGameStore.setState({
    gameState,
    waitingFor: gameState.waiting_for,
    legalActions: [],
    legalActionsByObject: {},
    spellCosts: {},
    viewerInteraction: null,
  });
  return render(
    <BoardInteractionContext.Provider
      value={{
        activatableObjectIds: new Set(),
        blockableAttackerIds: new Set(),
        boardChoiceObjectIds: new Set(),
        committedAttackerIds: new Set(),
        incomingAttackerCounts: new Map(),
        manaTappableObjectIds: new Set(),
        selectableSacrificeObjectIds: new Set(),
        selectableManaCostCreatureIds: new Set(),
        undoableTapObjectIds: new Set(),
        validAttackerIds: new Set(),
        validTargetObjectIds: new Set([1]),
      }}
    >
      <PermanentCard objectId={1} />
    </BoardInteractionContext.Provider>,
  );
}

beforeEach(() => {
  viewport.isMobile = false;
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
  useUiStore.setState({
    selectedObjectId: null,
    hoveredObjectId: null,
    inspectedObjectId: null,
    combatMode: null,
    selectedAttackers: [],
    blockerAssignments: new Map(),
    combatClickHandler: null,
    selectedCardIds: [],
    pendingAbilityChoice: null,
  });
});

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
  useGameStore.setState({ gameState: null, waitingFor: null });
});

describe.each(["art_crop", "full_card"] as const)("PermanentCard art reload (%s)", (display) => {
  beforeEach(() => {
    usePreferencesStore.setState({ battlefieldCardDisplay: display, showKeywordStrip: false });
  });

  it("reloads from the tile on a desktop-sized screen without targeting the permanent", () => {
    const reload = vi.fn();
    vi.mocked(useCardImage).mockReturnValue({ src: null, isLoading: false, isRotated: false, isFlip: false, reload });
    const { container } = renderTargetableSculptor();

    const button = screen.getByRole("button", { name: RELOAD });
    fireEvent.pointerDown(button);
    fireEvent.click(button);
    expect(reload).toHaveBeenCalledTimes(1);
    expect(dispatchAction).not.toHaveBeenCalled();

    // Reach guard: a click on the permanent itself does target it.
    fireEvent.click(container.querySelector('[data-object-id="1"]') as HTMLElement);
    expect(dispatchAction).toHaveBeenCalledWith({ type: "ChooseTarget", data: { target: { Object: 1 } } });
  });

  it("leaves a phone's tile to the game", () => {
    viewport.isMobile = true;
    vi.mocked(useCardImage).mockReturnValue({
      src: null, isLoading: false, isRotated: false, isFlip: false, reload: vi.fn(),
    });
    renderTargetableSculptor();

    // Reach guard: the artless tile is on screen.
    expect(screen.getByRole("img", { name: CARD_NAME })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: RELOAD })).toBeNull();
  });
});
