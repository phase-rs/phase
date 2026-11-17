import { cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import type { ReactNode } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { GameObject } from "../../../adapter/types.ts";
import { dispatchAction } from "../../../game/dispatch.ts";
import { useCardImage } from "../../../hooks/useCardImage.ts";
import { useGameStore } from "../../../stores/gameStore.ts";
import { usePreferencesStore } from "../../../stores/preferencesStore.ts";
import { useUiStore } from "../../../stores/uiStore.ts";
import { gameObjectFactory } from "../../../test/factories/gameObjectFactory.ts";
import { gameStateFactory } from "../../../test/factories/gameStateFactory.ts";
import { toCardProps } from "../../../viewmodel/cardProps.ts";
import { BoardInteractionContext } from "../BoardInteractionContext.tsx";
import { GroupedPermanentDisplay } from "../GroupedPermanent.tsx";
import { PermanentCard } from "../PermanentCard.tsx";

// Regression tests for #11008: a permanent whose art failed offers its reload
// on the tile on a desktop-sized screen, and the reload's tap never acts on the
// permanent. A phone's tile, and a card showing only a sliver, stay the game's.

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

function sculptor(id: number, overrides: Partial<GameObject> = {}): GameObject {
  return gameObjectFactory
    .creature(1, 2)
    .onBattlefield()
    .named(CARD_NAME)
    .withId(id)
    .build({ printed_ref: { oracle_id: "oracle-etherium-sculptor", face_name: CARD_NAME }, ...overrides });
}

/** Seeds the board and renders `children` with object 1 as a valid target, so
 *  a click on that permanent itself dispatches `ChooseTarget`. */
function renderBoard(objects: GameObject[], children: ReactNode) {
  const gameState = gameStateFactory.withPlayers(0, 1).withObjects(...objects).build();
  useGameStore.setState({
    gameState,
    waitingFor: gameState.waiting_for,
    legalActions: [],
    legalActionsByObject: {},
    manaPaymentPreviewSourceIds: [],
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
      {children}
    </BoardInteractionContext.Provider>,
  );
}

/** The tile of permanent `id`: its root carries the object id. */
function tileOf(container: HTMLElement, id: number): HTMLElement {
  const tile = container.querySelector<HTMLElement>(`[data-object-id="${id}"]`);
  if (tile == null) throw new Error(`permanent ${id} is not on the board`);
  return tile;
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
  vi.mocked(useCardImage).mockReturnValue({
    src: null, isLoading: false, isRotated: false, isFlip: false, reload: vi.fn(),
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
    const { container } = renderBoard([sculptor(1)], <PermanentCard objectId={1} />);

    const button = screen.getByRole("button", { name: RELOAD });
    fireEvent.pointerDown(button);
    fireEvent.click(button);
    expect(reload).toHaveBeenCalledTimes(1);
    expect(dispatchAction).not.toHaveBeenCalled();

    // Reach guard: a click on the permanent itself does target it.
    fireEvent.click(tileOf(container, 1));
    expect(dispatchAction).toHaveBeenCalledWith({ type: "ChooseTarget", data: { target: { Object: 1 } } });
  });

  it("leaves a phone's tile to the game", () => {
    viewport.isMobile = true;
    renderBoard([sculptor(1)], <PermanentCard objectId={1} />);

    // Reach guard: the artless tile is on screen.
    expect(screen.getByRole("img", { name: CARD_NAME })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: RELOAD })).toBeNull();
  });

  it("keeps an attachment's peek free for clicks on the attachment", () => {
    const aura = gameObjectFactory
      .enchantment()
      .onBattlefield()
      .named("Pacifism")
      .withId(2)
      .build({ attached_to: { type: "Object", data: 1 } });
    const { container } = renderBoard([sculptor(1, { attachments: [2] }), aura], <PermanentCard objectId={1} />);

    // Reach guard: the attachment's artless tile renders in its peek.
    const peek = tileOf(container, 2);
    expect(within(peek).getByRole("img", { name: "Pacifism" })).toBeInTheDocument();
    expect(within(peek).queryByRole("button", { name: RELOAD })).toBeNull();
    // The host itself still offers the reload.
    expect(screen.getAllByRole("button", { name: RELOAD })).toHaveLength(1);
  });

  it("offers the reload only on the top card of a staggered stack", () => {
    const stack = [sculptor(1), sculptor(2)];
    const group = {
      name: CARD_NAME,
      ids: [1, 2],
      count: 2,
      representative: toCardProps(stack[0]),
      isUnboundedPile: false,
    };
    const { container } = renderBoard(
      stack,
      <GroupedPermanentDisplay group={group} rowType="creatures" renderMode="staggered" onExpand={vi.fn()} />,
    );

    // Reach guard: the covered card's artless tile renders under the top one.
    const covered = tileOf(container, 1);
    expect(within(covered).getByRole("img", { name: CARD_NAME })).toBeInTheDocument();
    expect(within(covered).queryByRole("button", { name: RELOAD })).toBeNull();
    expect(within(tileOf(container, 2)).getByRole("button", { name: RELOAD })).toBeInTheDocument();
  });
});
