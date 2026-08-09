import { cleanup, fireEvent, render } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { GameAction, GameObject, GameState } from "../../../adapter/types.ts";
import { dispatchAction, dispatchInteraction } from "../../../game/dispatch.ts";
import { useGameStore } from "../../../stores/gameStore.ts";
import { useUiStore } from "../../../stores/uiStore.ts";
import { buildGameObject, buildObjectMap } from "../../../test/factories/gameObjectFactory.ts";
import {
  buildGameState,
  buildPlayers,
  buildPriorityWaitingFor,
} from "../../../test/factories/gameStateFactory.ts";
import { DialogHost } from "../../modal/DialogHost.tsx";
import { AttachmentFan } from "../AttachmentFan.tsx";

/**
 * Plan row V25 — THE USER'S BUG, END TO END, THROUGH A REAL CONSUMER.
 *
 * Every other row in this matrix asserts "opens the modal" on the store,
 * because `AbilityChoiceModal` is module-private inside `GamePage.tsx:3074`.
 * `pendingAbilityChoice` IS that modal's input contract, and `DialogHost`
 * (`:118-120` sets `hasUiDialog` from it, `:131-132` turns that into
 * `dialogVisible`, `:199` anchors `fixed inset-0 z-40`) is the consumer that
 * decides whether anything is painted at all. This row drives the PRODUCER
 * (`AttachmentFan` mode 2) and observes the real CONSUMER, so the wiring
 * between them is measured rather than assumed.
 *
 * ---------------------------------------------------------------------------
 * EVIDENCE-LABEL CONVENTION (plan §7.2, binding on this module):
 *   MEASURED  a PAST-TENSE REPORT — this PR ran that mutant arm and the quoted
 *             line is the assertion that flipped.
 *   QUOTED    a past-tense report copied verbatim from a named plan log.
 * An untagged comment is prose and is evidence for nothing.
 *
 * ASSERTION ORDER: none. Both observations use `expect.soft`, so neither can
 * pre-empt the other. That is not a style choice — with hard asserts the
 * payload assert throws first on BOTH mutant arms and the overlay assert (the
 * only assertion in this matrix that observes a `pendingAbilityChoice`
 * CONSUMER) is never reached, i.e. unmeasured.
 * QUOTED (`.plan3r10-v25soft.log`, the three trees the applier built out of §6):
 *   `R10V25[BASE]      FAILINGASSERT| ASSERT=overlay: expected +0 to be 1`
 *   `R10V25[BASE]      FAILINGASSERT| ASSERT=payload: expected null to deeply equal { objectId: 408, actions: [ …(2) `
 *   `R10V25[ADOPTED]   CARDINALITY|      Tests  3 passed (3)`
 *   `R10V25[ALWAYSMUT] FAILINGASSERT| ASSERT=overlay: expected 1 to be +0`
 *   `R10V25[ALWAYSMUT] FAILINGASSERT| ASSERT=payload: expected { objectId: 401, actions: [ …(2)`
 *
 * NAMED UNMEASURED ANCHOR: the `GamePage.tsx:3122` options map. It is the other
 * consumer of `pendingAbilityChoice` and this module does NOT observe it — plan
 * §7.3 row V26 registers it as an audited absence. It is coverage for nothing.
 * ---------------------------------------------------------------------------
 */

vi.mock("../../../game/dispatch.ts", () => ({
  dispatchAction: vi.fn(),
  dispatchInteraction: vi.fn(),
}));

// `FanCard` owns the `aria-label`; a mock that re-emitted it would label every
// card twice and silently break the precondition assert below. That is not
// hypothetical — QUOTED (`.plan3r10-v25fixture.log`), with the mock mutated to
// re-emit it: `R10V25FX[ADOPTED+brokenfixture] AssertionError: expected [ <div
// …(3)>…(1)</div>, …(3) ] to have a length of 2 but got 4`, on EVERY arm of an
// otherwise-passing tree — which is what a broken FIXTURE looks like as
// against a broken FIX.
vi.mock("../../card/CardImage.tsx", () => ({
  CardImage: ({ cardName }: { cardName: string }) => <div data-card-image={cardName} />,
}));

const HOST_ID = 401;
const FREED_ID = 408;

/** Freed from the Real — `{U}: Tap enchanted creature.` / `{U}: Untap …`. */
const FREED_TAP: GameAction = {
  type: "ActivateAbility",
  data: { source_id: FREED_ID, ability_index: 0 },
};
const FREED_UNTAP: GameAction = {
  type: "ActivateAbility",
  data: { source_id: FREED_ID, ability_index: 1 },
};
const HOST_ABILITY_A: GameAction = {
  type: "ActivateAbility",
  data: { source_id: HOST_ID, ability_index: 0 },
};
const HOST_ABILITY_B: GameAction = {
  type: "ActivateAbility",
  data: { source_id: HOST_ID, ability_index: 1 },
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

function makeState(): GameState {
  const host = battlefieldObject({
    id: HOST_ID,
    card_id: 4010,
    name: "Kilo, Apogee Mind",
    attachments: [FREED_ID],
    card_types: { supertypes: ["Legendary"], core_types: ["Creature"], subtypes: ["Robot"] },
    abilities: [
      { effect: { type: "Tap" } },
      { effect: { type: "Untap" } },
    ] as GameObject["abilities"],
  });
  const freed = battlefieldObject({
    id: FREED_ID,
    card_id: 4080,
    name: "Freed from the Real",
    attached_to: { type: "Object", data: HOST_ID },
    card_types: { supertypes: [], core_types: ["Enchantment"], subtypes: ["Aura"] },
    abilities: [
      { effect: { type: "Tap" } },
      { effect: { type: "Untap" } },
    ] as GameObject["abilities"],
  });

  return buildGameState({
    players: buildPlayers([0, 1]),
    objects: buildObjectMap(host, freed),
    battlefield: [HOST_ID, FREED_ID],
    exile: [],
    stack: [],
    waiting_for: buildPriorityWaitingFor(),
  });
}

function seed(legalActionsByObject: Record<string, GameAction[]>) {
  const gameState = makeState();
  useGameStore.setState({
    gameMode: "local",
    gameState,
    waitingFor: gameState.waiting_for,
    legalActions: [],
    legalActionsByObject,
    viewerInteraction: null,
  });
  // `enchantmentsDialogPlayer` is the OTHER input to `hasUiDialog`. Left
  // non-null it would raise the overlay on its own and the overlay assert
  // would be vacuous on every arm.
  useUiStore.setState({
    attachmentFanHostId: HOST_ID,
    pendingAbilityChoice: null,
    enchantmentsDialogPlayer: null,
  });
  return render(
    <>
      <DialogHost>
        <div />
      </DialogHost>
      <AttachmentFan />
    </>,
  );
}

/** `DialogHost.tsx:199` — `fixed inset-0 ${GAME_Z_LAYER.dialogHost}`, z-40. */
function hostOverlayCount(): number {
  return document.querySelectorAll(".fixed.inset-0.z-40").length;
}

function fanCards(): HTMLElement[] {
  const fan = document.querySelector("[data-attachment-fan]");
  return Array.from(fan?.querySelectorAll("[aria-label]") ?? []) as HTMLElement[];
}

function clickFanCard(name: string) {
  const card = fanCards().find((el) => el.getAttribute("aria-label") === name);
  expect(card, `fan card ${name} must be rendered`).toBeTruthy();
  fireEvent.click(card as HTMLElement);
}

describe("pendingAbilityChoice reaches its DialogHost consumer", () => {
  beforeEach(() => {
    vi.mocked(dispatchAction).mockReset();
    vi.mocked(dispatchAction).mockResolvedValue(undefined);
    vi.mocked(dispatchInteraction).mockReset();
    vi.mocked(dispatchInteraction).mockResolvedValue(undefined);
  });

  afterEach(() => {
    cleanup();
    useUiStore.setState({ attachmentFanHostId: null, pendingAbilityChoice: null });
  });

  // V25 (a) — the bug itself. Clicking the Aura in the fan must publish BOTH of
  // its abilities, in engine order, AND raise the host overlay that paints the
  // chooser. Before this change the fan had exactly one source (an open
  // interaction's projection), so a populated bucket with no prompt open
  // reached nothing at all.
  // MEASURED (drop side, whole change reverted): both asserts flip —
  //   `ASSERT=payload: expected null to deeply equal { objectId: 408, actions: [ …(2) `
  //   `ASSERT=overlay: expected +0 to be 1`
  it("publishes the clicked attachment's abilities and raises the dialog overlay", () => {
    seed({ [String(FREED_ID)]: [FREED_TAP, FREED_UNTAP] });

    // Non-soft PRECONDITION, and its own control: if the fixture ever stops
    // rendering both cards the arms below would agree for the wrong reason.
    expect(fanCards()).toHaveLength(2);
    expect(hostOverlayCount(), "no overlay before the click").toBe(0);

    clickFanCard("Freed from the Real");

    expect.soft(useUiStore.getState().pendingAbilityChoice, "ASSERT=payload").toEqual({
      objectId: FREED_ID,
      actions: [FREED_TAP, FREED_UNTAP],
    });
    expect.soft(hostOverlayCount(), "ASSERT=overlay").toBe(1);
  });

  // V25 (b) — the fan is opened FROM the host, so the host card is the fan's
  // anchor and never one of its picks. Same bucket as (a); only the clicked
  // card differs.
  it("publishes nothing when the fan's own host is clicked", () => {
    seed({ [String(FREED_ID)]: [FREED_TAP, FREED_UNTAP] });

    expect(fanCards()).toHaveLength(2);

    clickFanCard("Kilo, Apogee Mind");

    expect.soft(useUiStore.getState().pendingAbilityChoice, "ASSERT=payload").toBeNull();
    expect.soft(hostOverlayCount(), "ASSERT=overlay").toBe(0);
  });

  // V25 (c) — THE ARM THAT SEPARATES THE ALWAYS-TRUE MUTANT. With an EMPTY host
  // bucket, (a) and (b) cannot tell a correct implementation from one whose
  // `selectable` predicate is constant-true with the `id !== host.id` host
  // invariant dropped: both agree on every arm. Populating the host's OWN
  // bucket is what makes that mutant visible.
  // MEASURED (always side, on THIS arm only): both asserts flip —
  //   `ASSERT=payload: expected { objectId: 401, actions: [ …(2)`
  //   `ASSERT=overlay: expected 1 to be +0`
  it("still publishes nothing when the host's own bucket is populated too", () => {
    seed({
      [String(HOST_ID)]: [HOST_ABILITY_A, HOST_ABILITY_B],
      [String(FREED_ID)]: [FREED_TAP, FREED_UNTAP],
    });

    expect(fanCards()).toHaveLength(2);

    clickFanCard("Kilo, Apogee Mind");

    expect.soft(useUiStore.getState().pendingAbilityChoice, "ASSERT=payload").toBeNull();
    expect.soft(hostOverlayCount(), "ASSERT=overlay").toBe(0);
  });
});
