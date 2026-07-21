import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { GameAction, GameObject } from "../../../adapter/types.ts";
import { dispatchAction } from "../../../game/dispatch.ts";
import { useGameStore } from "../../../stores/gameStore.ts";
import { useUiStore } from "../../../stores/uiStore.ts";
import {
  buildCommanderGameObject,
  buildObjectMap,
  gameObjectFactory,
} from "../../../test/factories/gameObjectFactory.ts";
import {
  buildGameState,
  buildPlayers,
  buildPriorityWaitingFor,
} from "../../../test/factories/gameStateFactory.ts";
import { CommanderCardZone } from "../CommanderCardZone.tsx";

vi.mock("../../../game/dispatch.ts", () => ({
  dispatchAction: vi.fn(),
}));

// Keep the test hermetic: the card-image hook otherwise fires a dev-server
// asset fetch (localhost:3000) that has nothing to do with the affordance.
vi.mock("../../../hooks/useCardImage.ts", () => ({
  useCardImage: () => ({ src: null }),
}));

const COMMANDER_ID = 101;

function makeState(commander: GameObject) {
  return buildGameState({
    active_player: 0,
    priority_player: 0,
    players: buildPlayers([0, 1]),
    objects: buildObjectMap(commander),
    command_zone: [commander.id],
    battlefield: [],
    exile: [],
    stack: [],
    waiting_for: buildPriorityWaitingFor(),
  });
}

function ninjutsuAction(creatureToReturn: number): GameAction {
  return {
    type: "ActivateNinjutsu",
    data: { ninjutsu_object_id: COMMANDER_ID, creature_to_return: creatureToReturn },
  };
}

function castAction(): GameAction {
  return {
    type: "CastSpell",
    data: { object_id: COMMANDER_ID, card_id: 201, targets: [] },
  };
}

function signatureSpell() {
  return gameObjectFactory
    .signatureSpell()
    .sorcery()
    .withId(COMMANDER_ID)
    .named("Scheming Symmetry")
    .build();
}

/** Seed both stores for a command-zone commander with the given legal actions. */
function seedStores(actions: GameAction[]) {
  const commander = buildCommanderGameObject({ id: COMMANDER_ID });
  const gameState = makeState(commander);
  useGameStore.setState({
    gameState,
    waitingFor: gameState.waiting_for,
    legalActions: actions,
    legalActionsByObject: { [String(COMMANDER_ID)]: actions },
    spellCosts: {},
  });
  useUiStore.setState({
    inspectedObjectId: null,
    pendingAbilityChoice: null,
    debugInteractionMode: false,
  });
}

describe("CommanderCardZone commander ninjutsu (issue #5239)", () => {
  beforeEach(() => {
    vi.mocked(dispatchAction).mockClear();
  });

  afterEach(() => {
    cleanup();
  });

  it("dispatches the lone ActivateNinjutsu action when the commander is clicked", () => {
    const action = ninjutsuAction(9);
    seedStores([action]);

    render(<CommanderCardZone playerId={0} />);
    fireEvent.click(screen.getByRole("button"));

    // A single legal ninjutsu (one unblocked attacker) auto-fires — mirrors
    // hand-zone ninjutsu via resolveSingleActionDispatch.
    expect(dispatchAction).toHaveBeenCalledTimes(1);
    expect(dispatchAction).toHaveBeenCalledWith(action);
    // Auto-dispatch does not open the choice modal, and does not inspect.
    expect(useUiStore.getState().pendingAbilityChoice).toBeNull();
    expect(useUiStore.getState().inspectedObjectId).toBeNull();
  });

  it("opens the ability-choice modal when multiple attackers can be returned", () => {
    const actions = [ninjutsuAction(9), ninjutsuAction(12)];
    seedStores(actions);

    render(<CommanderCardZone playerId={0} />);
    fireEvent.click(screen.getByRole("button"));

    // More than one returnable attacker is a genuine choice — surface the modal
    // rather than auto-firing an arbitrary one.
    expect(dispatchAction).not.toHaveBeenCalled();
    expect(useUiStore.getState().pendingAbilityChoice).toEqual({
      objectId: COMMANDER_ID,
      actions,
    });
  });

  it("inspects (does not cast) on a single click when only a cast action is legal", () => {
    seedStores([castAction()]);

    render(<CommanderCardZone playerId={0} />);
    fireEvent.click(screen.getByRole("button"));

    // Casting a commander is a double-click / drag affordance; a single click
    // must still inspect, not cast — the ninjutsu branch must not hijack it.
    expect(dispatchAction).not.toHaveBeenCalled();
    expect(useUiStore.getState().inspectedObjectId).toBe(COMMANDER_ID);
  });

  it("renders an Oathbreaker signature spell from the command zone", () => {
    const spell = signatureSpell();
    const gameState = makeState(spell);
    useGameStore.setState({
      gameState,
      waitingFor: gameState.waiting_for,
      legalActions: [castAction()],
      legalActionsByObject: { [String(COMMANDER_ID)]: [castAction()] },
      spellCosts: {},
    });

    render(<CommanderCardZone playerId={0} />);

    expect(screen.getByText("Signature Spell")).toBeInTheDocument();
    expect(screen.getByRole("button")).toHaveAttribute(
      "title",
      "Cast Scheming Symmetry — double-click or drag to play",
    );
  });
});
