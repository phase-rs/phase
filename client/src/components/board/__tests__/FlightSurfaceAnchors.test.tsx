import { act, cleanup, render, within } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { GameAction } from "../../../adapter/types.ts";
import { useAnimationStore } from "../../../stores/animationStore.ts";
import { useGameStore } from "../../../stores/gameStore.ts";
import { gameObjectFactory } from "../../../test/factories/gameObjectFactory.ts";
import {
  buildPriorityWaitingFor,
  buildStackEntry,
  gameStateFactory,
} from "../../../test/factories/gameStateFactory.ts";
import { objectAnchorSelector } from "../../../utils/objectAnchorSelector.ts";
import { toCardProps } from "../../../viewmodel/cardProps.ts";
import type { GroupedPermanent as GroupedPermanentType } from "../../../viewmodel/battlefieldProps.ts";
import { OpponentHand } from "../../hand/OpponentHand.tsx";
import { PlayerHand } from "../../hand/PlayerHand.tsx";
import { StackEntry } from "../../stack/StackEntry.tsx";
import { GraveyardPile } from "../../zone/GraveyardPile.tsx";
import { LibraryPile } from "../../zone/LibraryPile.tsx";
import { BoardInteractionContext } from "../BoardInteractionContext.tsx";
import { GroupedPermanentDisplay } from "../GroupedPermanent.tsx";
import { PermanentCard } from "../PermanentCard.tsx";

vi.mock("../../../hooks/useCardImage.ts", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../../../hooks/useCardImage.ts")>()),
  useCardImage: () => ({ src: "card.png", isLoading: false }),
  useCardBackImage: () => ({ src: "card-back.png", isLoading: false }),
}));

vi.mock("../../../game/dispatch.ts", () => ({
  dispatchAction: vi.fn(),
  dispatchInteraction: vi.fn(),
}));

/** The one object id placed in every zone at once. */
const X = 50;
const X_NAME = "Flight Card";
/** The collapsed group's representative; X is a covered member. */
const R = 60;
const GROUP_IDS = [R, 61, 62, 63, X];
/** A permanent mounted directly, outside any group. */
const Y = 70;

function castSpell(objectId: number): GameAction {
  return { type: "CastSpell", data: { object_id: objectId, card_id: 1, targets: [] } };
}

/**
 * X sits in player 0's hand, graveyard (top) and library (visible top), in
 * player 1's hand, on the stack and in a collapsed battlefield group. With
 * `castableFromGraveyard` X leaves player 0's hand and gains a cast action, so
 * the graveyard fan renders it instead of the hand.
 */
function setBoardState({ castableFromGraveyard = false } = {}) {
  const flightCard = gameObjectFactory.withId(X).inHand().named(X_NAME).build();
  flightCard.display_visible_to_viewer = true;
  const saprolings = GROUP_IDS.filter((id) => id !== X).map((id) =>
    gameObjectFactory.withId(id).onBattlefield().creature(1, 1).named("Saproling").build(),
  );
  const permanent = gameObjectFactory.withId(Y).onBattlefield().creature().named("Lone Creature").build();
  const gameState = gameStateFactory
    .withPlayers(
      {
        id: 0,
        hand: castableFromGraveyard ? [] : [X],
        graveyard: [X],
        library: [X],
      },
      { id: 1, hand: [X] },
    )
    .withObjects(flightCard, ...saprolings, permanent)
    .build({
      stack: [buildStackEntry({ id: X, source_id: X })],
      waiting_for: buildPriorityWaitingFor(),
    });
  useGameStore.setState({
    gameState,
    waitingFor: gameState.waiting_for,
    spellCosts: {},
    legalActions: [],
    legalActionsByObject: castableFromGraveyard ? { [String(X)]: [castSpell(X)] } : {},
  });
}

function collapsedGroup(): GroupedPermanentType {
  const representative = useGameStore.getState().gameState!.objects[R];
  return {
    name: "Saproling",
    ids: GROUP_IDS,
    count: GROUP_IDS.length,
    representative: toCardProps(representative),
    isUnboundedPile: false,
  };
}

function renderBoard() {
  const stackEntry = useGameStore.getState().gameState!.stack[0];
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
        validTargetObjectIds: new Set(),
      }}
    >
      <PlayerHand />
      <OpponentHand playerId={1} />
      <StackEntry entry={stackEntry} index={0} isTop cardSize={{ width: 120, height: 168 }} />
      <GraveyardPile playerId={0} onClick={vi.fn()} />
      <LibraryPile playerId={0} />
      <GroupedPermanentDisplay
        group={collapsedGroup()}
        rowType="creatures"
        renderMode="collapsed"
        onExpand={vi.fn()}
      />
      <PermanentCard objectId={Y} />
    </BoardInteractionContext.Provider>,
  );
}

/** Zone-scoped anchors for X (and for the directly mounted Y), per surface. */
const ANCHORS = {
  ownHand: `[data-hand-card][data-object-id="${X}"]`,
  opponentHand: `[data-opponent-hand-card="${X}"]`,
  stack: `[data-stack-entry="${X}"]`,
  graveyard: `[data-graveyard-pile][data-grouped-ids~="${X}"]`,
  libraryTop: `[data-library-pile="0"] [data-grouped-ids~="${X}"]`,
  library: `[data-library-pile="0"]`,
  groupRepresentative: `[data-permanent-card][data-grouped-ids~="${X}"]`,
  permanentY: `[data-permanent-card="${Y}"]`,
};
const FAN_ANCHOR = `[data-zone-fan-card="Graveyard"][data-object-id="${X}"]`;

function only(container: HTMLElement, selector: string): HTMLElement {
  const matches = container.querySelectorAll<HTMLElement>(selector);
  expect(matches, selector).toHaveLength(1);
  return matches[0];
}

beforeEach(() => {
  useAnimationStore.getState().clearQueue();
});

afterEach(() => {
  cleanup();
  useAnimationStore.getState().clearQueue();
  useGameStore.setState({ gameState: null, waitingFor: null, spellCosts: {}, legalActionsByObject: {} });
});

describe("flight surface anchors", () => {
  it("addresses one object in every zone by exactly one node per zone, and that node is what the veil hides", () => {
    setBoardState();
    const { container } = renderBoard();

    // Positive control: the generic object anchor really does collide here.
    expect(container.querySelectorAll(objectAnchorSelector(X)).length).toBeGreaterThan(1);

    for (const selector of Object.values(ANCHORS)) only(container, selector);
    const graveyardPile = only(container, '[data-graveyard-pile="0"]');
    const libraryPile = only(container, ANCHORS.library);
    expect(within(graveyardPile).getByRole("img", { name: X_NAME })).toBeInTheDocument();
    expect(within(libraryPile).getByRole("img", { name: X_NAME })).toBeInTheDocument();

    act(() => useAnimationStore.getState().veilFlight(X));

    expect(only(container, ANCHORS.ownHand).style.visibility).toBe("hidden");
    expect(only(container, ANCHORS.opponentHand).style.visibility).toBe("hidden");
    expect(only(container, ANCHORS.stack).style.visibility).toBe("hidden");
    expect(within(graveyardPile).queryByRole("img", { name: X_NAME })).toBeNull();
    expect(within(libraryPile).queryByRole("img", { name: X_NAME })).toBeNull();
    expect(within(libraryPile).getByRole("img", { name: "Card back" })).toBeInTheDocument();
    // A covered member in flight does not blank the group's representative.
    expect(only(container, ANCHORS.groupRepresentative).style.visibility).toBe("");

    act(() => useAnimationStore.getState().veilFlight(R));

    expect(only(container, ANCHORS.groupRepresentative).style.visibility).toBe("hidden");
  });

  it("addresses the castable graveyard fan card by its own zone anchor", () => {
    setBoardState({ castableFromGraveyard: true });
    const { container } = renderBoard();

    const fanCard = only(container, FAN_ANCHOR);
    expect(container.querySelectorAll(ANCHORS.ownHand)).toHaveLength(0);

    act(() => useAnimationStore.getState().veilFlight(X));

    expect(fanCard.style.visibility).toBe("hidden");
  });
});
