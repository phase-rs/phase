import { cleanup, render, screen } from "@testing-library/react";
import type { ReactNode } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { GameAction, GameObject } from "../../../adapter/types.ts";
import { useGameStore } from "../../../stores/gameStore.ts";
import { usePreferencesStore } from "../../../stores/preferencesStore.ts";
import { useUiStore } from "../../../stores/uiStore.ts";
import { buildGameObject, buildObjectMap } from "../../../test/factories/gameObjectFactory.ts";
import {
  buildGameState,
  buildPlayers,
  buildPriorityWaitingFor,
} from "../../../test/factories/gameStateFactory.ts";
import { GameBoard } from "../GameBoard.tsx";
import { useBoardInteractionState } from "../BoardInteractionContext.tsx";

/**
 * V14w — WIRING, not parity (plan §7.0/§7.3).
 *
 * `deriveActivationAffordances` being correct as a function proves nothing
 * about the board: the value has to REACH the consumer. `GameBoard` publishes
 * it through `BoardInteractionContext`, and this row renders the real
 * `<GameBoard/>` with a child that reads `useBoardInteractionState()` under the
 * real provider, then asserts both sets BY CONTENT.
 *
 * Content, not size, is what makes the always-true side fail: a `.size` assert
 * passes under an "add every object" mutant.
 *
 * ---------------------------------------------------------------------------
 * EVIDENCE-LABEL CONVENTION (plan §7.2, binding on this module). Every comment
 * carrying a mutant/coverage claim is tagged, because an in-file label is
 * otherwise ambiguous between an INSTRUCTION and a PAST-TENSE REPORT:
 *
 *   MEASURED   past-tense report: this PR ran that arm; the quoted text is the
 *              assertion that flipped.
 *   QUOTED     past-tense report copied verbatim from a named harness log.
 *   POINTER    names a row/mutant whose measurement lives in the plan's
 *              evidence logs, not here.
 *
 * An untagged comment is prose and is evidence for nothing.
 *
 * NAMED UNMEASURED ANCHOR IN THIS FILE: this row observes the context boundary
 * only. The two OTHER consumers of the affordance pair are covered elsewhere —
 * `PermanentCard` by the V10–V13 rows in `PermanentCard.test.tsx`, and
 * `BattlefieldZoneOverflow` by the V24 row in its own file (claimed for
 * `activatableObjectIds` only; its `manaTappableObjectIds` read was already
 * guarded).
 * ---------------------------------------------------------------------------
 */

const AURA_ID = 408;
const LAND_ID = 500;

/** A child under the real provider — the consumer whose read is being proved. */
function AffordanceProbe() {
  const { activatableObjectIds, manaTappableObjectIds } = useBoardInteractionState();
  return (
    <div
      data-testid="affordance-probe"
      data-activatable={[...activatableObjectIds].sort((a, b) => a - b).join(",")}
      data-mana={[...manaTappableObjectIds].sort((a, b) => a - b).join(",")}
    />
  );
}

vi.mock("../PlayerArea.tsx", () => ({
  PlayerArea: ({ playerId }: { playerId: number }) =>
    playerId === 0 ? <AffordanceProbe /> : <div data-testid={`player-area-${playerId}`} />,
}));

vi.mock("../ArchenemyPanel.tsx", () => ({ ArchenemyPanel: () => null }));
vi.mock("../CombatLine.tsx", () => ({ CombatLine: () => null }));
vi.mock("../PlanechasePanel.tsx", () => ({ PlanechasePanel: () => null }));
vi.mock("../OpponentSeatHeader.tsx", () => ({ OpponentSeatHeader: () => null }));
vi.mock("../../flexlayout/DraggableWidget.tsx", () => ({
  DraggableWidget: ({ children }: { children: ReactNode }) => <>{children}</>,
}));
vi.mock("../../hand/OpponentHand.tsx", () => ({ OpponentHand: () => null }));
vi.mock("../../zone/ExilePile.tsx", () => ({ ExilePile: () => null }));
vi.mock("../../zone/GraveyardPile.tsx", () => ({ GraveyardPile: () => null }));
vi.mock("../../zone/LibraryPile.tsx", () => ({ LibraryPile: () => null }));

const AURA_TAP: GameAction = {
  type: "ActivateAbility",
  data: { source_id: AURA_ID, ability_index: 0 },
};
const AURA_UNTAP: GameAction = {
  type: "ActivateAbility",
  data: { source_id: AURA_ID, ability_index: 1 },
};
const TAP_LAND: GameAction = {
  type: "TapLandForMana",
  data: {
    selection: {
      source: { object_id: LAND_ID, incarnation: 1 },
      ability_index: null,
      mana_type: "Blue",
      output: { type: "Concrete", data: "Blue" },
      atomic_combination: null,
      restrictions: [],
      penalty: "None",
      taps_for_mana: [],
    },
  },
};

function battlefieldObject(overrides: Partial<GameObject>): GameObject {
  return buildGameObject({
    zone: "Battlefield",
    owner: 0,
    controller: 0,
    entered_battlefield_turn: null,
    ...overrides,
  });
}

describe("GameBoard publishes the shared activation affordances to its consumers", () => {
  beforeEach(() => {
    const aura = battlefieldObject({
      id: AURA_ID,
      card_id: 4080,
      name: "Freed from the Real",
      card_types: { supertypes: [], core_types: ["Enchantment"], subtypes: ["Aura"] },
      abilities: [
        { effect: { type: "Tap" } },
        { effect: { type: "Untap" } },
      ] as GameObject["abilities"],
    });
    const land = battlefieldObject({
      id: LAND_ID,
      card_id: 5000,
      name: "Island",
      card_types: { supertypes: ["Basic"], core_types: ["Land"], subtypes: ["Island"] },
      abilities: [
        { is_mana_ability: true, effect: { type: "Mana" } },
      ] as GameObject["abilities"],
    });
    const waitingFor = buildPriorityWaitingFor();
    useGameStore.setState({
      gameMode: "local",
      gameState: buildGameState({
        players: buildPlayers([0, 1]),
        objects: buildObjectMap(aura, land),
        battlefield: [AURA_ID, LAND_ID],
        exile: [],
        stack: [],
        seat_order: [0, 1],
        eliminated_players: [],
        waiting_for: waitingFor,
      }),
      waitingFor,
      legalActions: [],
      legalActionsByObject: {
        [String(AURA_ID)]: [AURA_TAP, AURA_UNTAP],
        [String(LAND_ID)]: [TAP_LAND],
      },
      viewerInteraction: null,
    });
    useUiStore.setState({ focusedOpponent: 1, blockerAssignments: new Map() });
    usePreferencesStore.setState({ multiplayerBoardLayout: "focused" });
  });

  afterEach(() => {
    cleanup();
  });

  // V14w — WIRING, not parity: the affordance pair reaches a real child consumer
  // NON-EMPTY, through the real provider.
  // Assertion ORDER: `data-activatable` first, then `data-mana`.
  //
  // QUOTED (drop side — deleting the Priority arm inside the authority):
  //   `REV6WIRE[A2_dropPriorityArm] activatable="" mana="500"` ⇒
  //   `REV6ARM[A2_dropPriorityArm] AssertionError: expected '' to be '408'`
  // QUOTED (always side): `REV6WIRE[A3_alwaysTrue] activatable="408,500"
  //   mana="408,500"` ⇒ `REV6ARM[A3_alwaysTrue] AssertionError: expected
  //   '408,500' to be '408'` — the activatable assert fails first.
  // QUOTED (non-domination of the SECOND assert, so it is not merely dominated
  //   by the first): `REV6WIRE[A7_dropManaArm] activatable="408" mana=""` —
  //   that mutant is invisible to the activatable assert and kills on the mana
  //   assert alone.
  it("reaches a child consumer with both sets populated by content at Priority", () => {
    render(
      <GameBoard effectiveMultiplayerBoardLayout="focused" oppHud={<div />} playerHud={<div />} />,
    );

    const probe = screen.getByTestId("affordance-probe");
    expect(probe.getAttribute("data-activatable")).toBe("408");
    expect(probe.getAttribute("data-mana")).toBe("500");
  });

  // V14w (negative arm) — the board's own timing gate, observed at the same
  // boundary: this proves the assertion above reads a live value, not a constant.
  it("reaches the same consumer with both sets EMPTY when the viewer cannot act", () => {
    const waitingFor = { type: "Priority", data: { player: 1 } } as const;
    useGameStore.setState({
      gameState: { ...useGameStore.getState().gameState!, waiting_for: waitingFor },
      waitingFor,
    });

    render(
      <GameBoard effectiveMultiplayerBoardLayout="focused" oppHud={<div />} playerHud={<div />} />,
    );

    const probe = screen.getByTestId("affordance-probe");
    expect(probe.getAttribute("data-activatable")).toBe("");
    expect(probe.getAttribute("data-mana")).toBe("");
  });
});
