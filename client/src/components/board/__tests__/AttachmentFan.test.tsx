import { cleanup, fireEvent, render } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { GameAction, GameObject, GameState, WaitingFor } from "../../../adapter/types.ts";
import type {
  InteractionChoiceId,
  InteractionId,
  ViewerInteraction,
} from "../../../adapter/generated/interaction";
import { dispatchAction, dispatchInteraction } from "../../../game/dispatch.ts";
import { useGameStore } from "../../../stores/gameStore.ts";
import { useUiStore } from "../../../stores/uiStore.ts";
import { buildGameObject, buildObjectMap } from "../../../test/factories/gameObjectFactory.ts";
import {
  buildGameState,
  buildPlayers,
  buildPriorityWaitingFor,
} from "../../../test/factories/gameStateFactory.ts";
import { AttachmentFan } from "../AttachmentFan.tsx";

/**
 * `AttachmentFan` mode 2 — the fan as a reachability surface for an attached
 * permanent's OWN legal actions (plan §6.9). Rows V1–V9.
 *
 * THE BUG THIS MODULE COVERS: with Kilo (401) enchanted by Freed from the Real
 * (408), the engine published `legalActionsByObject["408"] = [#0, #1]` and the
 * fan was inert — no ring, and clicking Freed did nothing. Before this change
 * the fan had exactly one source (an open interaction's projection), so a
 * populated bucket with no prompt open reached nothing.
 *
 * ---------------------------------------------------------------------------
 * EVIDENCE-LABEL CONVENTION (plan §7.2, binding on this module).
 * Every comment in this file that carries a mutant/coverage claim is tagged,
 * and the tag says what KIND of statement it is — an in-file label is
 * otherwise ambiguous between an instruction and a report:
 *
 *   MEASURED   a PAST-TENSE REPORT: this PR ran that arm and the quoted line
 *              is the failing assertion it produced.
 *   QUOTED     a past-tense report copied verbatim from a named harness log.
 *   POINTER    names a sweep row / mutant id whose measurement lives in the
 *              plan's evidence logs, not here.
 *   PAIR-ONLY / DROP-ONLY   the row carries ONE side by construction (§7.1)
 *              and is counted as coverage for that side only.
 *
 * An untagged comment is prose and is evidence for nothing.
 *
 * NAMED UNMEASURED ANCHOR IN THIS FILE: none. Every row below carries at least
 * one measured side; the rows that carry only one are labelled above.
 * ---------------------------------------------------------------------------
 */

vi.mock("../../../game/dispatch.ts", () => ({
  dispatchAction: vi.fn(),
  dispatchInteraction: vi.fn(),
}));

// Deliberately NOT re-emitting the object name: `FanCard` owns the
// `aria-label`, so a mock that also emitted it would label every card twice and
// silently break the per-card queries below.
vi.mock("../../card/CardImage.tsx", () => ({
  CardImage: ({ cardName }: { cardName: string }) => <div data-card-image={cardName} />,
}));

const HOST_ID = 401;
const FREED_ID = 408;
const BOOTS_ID = 409;

/** Freed from the Real — `{U}: Tap enchanted creature.` / `{U}: Untap …`. */
const FREED_TAP: GameAction = {
  type: "ActivateAbility",
  data: { source_id: FREED_ID, ability_index: 0 },
};
const FREED_UNTAP: GameAction = {
  type: "ActivateAbility",
  data: { source_id: FREED_ID, ability_index: 1 },
};

function makeObject(overrides: Partial<GameObject>): GameObject {
  return buildGameObject({
    zone: "Battlefield",
    owner: 0,
    controller: 0,
    entered_battlefield_turn: null,
    ...overrides,
  });
}

function makeState(options: { attachments?: number[]; freedAbilities?: unknown[] } = {}): GameState {
  const attachments = options.attachments ?? [FREED_ID];
  const host = makeObject({
    id: HOST_ID,
    card_id: 4010,
    name: "Kilo, Apogee Mind",
    attachments,
    card_types: { supertypes: ["Legendary"], core_types: ["Creature"], subtypes: ["Robot"] },
  });
  const freed = makeObject({
    id: FREED_ID,
    card_id: 4080,
    name: "Freed from the Real",
    attached_to: { type: "Object", data: HOST_ID },
    card_types: { supertypes: [], core_types: ["Enchantment"], subtypes: ["Aura"] },
    abilities: (options.freedAbilities ?? [
      { effect: { type: "Tap" } },
      { effect: { type: "Untap" } },
    ]) as GameObject["abilities"],
  });
  const boots = makeObject({
    id: BOOTS_ID,
    card_id: 4090,
    name: "Swiftfoot Boots",
    attached_to: { type: "Object", data: HOST_ID },
    card_types: { supertypes: [], core_types: ["Artifact"], subtypes: ["Equipment"] },
  });

  return buildGameState({
    players: buildPlayers([0, 1]),
    objects: buildObjectMap(host, freed, boots),
    battlefield: [HOST_ID, FREED_ID, BOOTS_ID],
    exile: [],
    stack: [],
    waiting_for: buildPriorityWaitingFor(),
  });
}

function seed(options: {
  legalActionsByObject?: Record<string, GameAction[]>;
  waitingFor?: WaitingFor;
  attachments?: number[];
  freedAbilities?: unknown[];
  viewerInteraction?: ViewerInteraction | null;
} = {}) {
  const gameState = makeState({
    attachments: options.attachments,
    freedAbilities: options.freedAbilities,
  });
  const waitingFor = options.waitingFor ?? gameState.waiting_for;
  gameState.waiting_for = waitingFor;
  useGameStore.setState({
    gameMode: "local",
    gameState,
    waitingFor,
    legalActions: [],
    legalActionsByObject: options.legalActionsByObject ?? {},
    viewerInteraction: options.viewerInteraction ?? null,
  });
  useUiStore.setState({ attachmentFanHostId: HOST_ID, pendingAbilityChoice: null });
  return render(<AttachmentFan />);
}

/** The rendered fan card for one object, by the `aria-label` `FanCard` owns. */
function fanCard(name: string): HTMLElement {
  const fan = document.querySelector("[data-attachment-fan]");
  expect(fan).not.toBeNull();
  const card = fan?.querySelector(`[aria-label="${name}"]`);
  expect(card).not.toBeNull();
  return card as HTMLElement;
}

/** Cyan ring + `cursor-pointer` is `FanCard`'s rendering of `selectable`. */
function isSelectable(name: string): boolean {
  const card = fanCard(name);
  const ringed = card.querySelector(".ring-cyan-400") != null;
  const pointer = card.className.includes("cursor-pointer");
  // The two must agree — they are two renderings of one prop, so a disagreement
  // means the query, not the component, is wrong.
  expect(ringed).toBe(pointer);
  return ringed;
}

function interactionOver(objectIds: number[]): ViewerInteraction {
  const interactionId = "fan-interaction" as InteractionId;
  const choiceId = (objectId: number) => `fan-${objectId}` as InteractionChoiceId;
  return {
    waitingForKind: { simultaneous: null, terminal: false, code: "choose" },
    authorizedSubmitters: [0],
    canSubmit: true,
    autoPassRecommended: false,
    opportunities: [{
      interactionId,
      response: {
        type: "exactChoices",
        data: {
          choices: objectIds.map((objectId) => ({
            id: choiceId(objectId),
            status: { type: "available" },
            surfaces: [],
          })),
        },
      },
      surfaces: [],
      progress: { selected: 0, minimum: 1, maximum: 1, aggregate: null, confirmable: false },
    }],
    attachmentFans: {
      [HOST_ID]: {
        hostId: HOST_ID,
        children: objectIds.map((objectId) => ({
          objectId,
          submission: {
            interactionId,
            response: { type: "choose", data: { choiceId: choiceId(objectId) } },
          },
        })),
      },
    },
    availability: { type: "inputRequired" },
  };
}

describe("AttachmentFan mode 2 — the permanent's own legal actions", () => {
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

  // V1 — THE USER'S BUG. Both abilities reach the chooser, in engine order, and
  // the fan is closed FIRST so the modal it opens is neither painted over nor
  // click-swallowed by the fan's `fixed inset-0 z-[120]` backdrop.
  //
  // MEASURED (drop side, whole-change): this row on the un-patched tree reports
  //   `AssertionError: expected null to deeply equal { objectId: 408, actions: [ …(2) }`
  // MEASURED (always side): with `canActivate` forced constant-true and the
  //   `id !== host.id` host invariant dropped, the V5 host arm below flips.
  it("opens the ability chooser with both of the Aura's abilities and closes the fan first", () => {
    seed({ legalActionsByObject: { [String(FREED_ID)]: [FREED_TAP, FREED_UNTAP] } });

    expect(isSelectable("Freed from the Real")).toBe(true);
    fireEvent.click(fanCard("Freed from the Real"));

    expect(useUiStore.getState().pendingAbilityChoice).toEqual({
      objectId: FREED_ID,
      actions: [FREED_TAP, FREED_UNTAP],
    });
    expect(useUiStore.getState().attachmentFanHostId).toBeNull();
    expect(dispatchAction).not.toHaveBeenCalled();
  });

  // V2 — a lone benign action auto-dispatches rather than opening a one-option
  // modal. PAIR-ONLY (§7.1): POINTER sweep row `C4 lone benign ability`; its
  // always-side mutant is `alwaysChoose`.
  it("auto-dispatches a lone benign ability instead of opening a one-option modal", () => {
    seed({ legalActionsByObject: { [String(FREED_ID)]: [FREED_TAP] } });

    fireEvent.click(fanCard("Freed from the Real"));

    expect(dispatchAction).toHaveBeenCalledTimes(1);
    expect(dispatchAction).toHaveBeenCalledWith(FREED_TAP);
    expect(useUiStore.getState().pendingAbilityChoice).toBeNull();
    expect(useUiStore.getState().attachmentFanHostId).toBeNull();
  });

  // V3 (#506) — a lone card-consuming ability must NOT auto-fire from the fan.
  // POINTER: sweep row `C3 lone consumes_source`, mutant `noGate506`; the
  // always side is `alwaysDispatch`. The fan delegates the decision rather than
  // re-implementing it, which is the whole point of the shared authority.
  it("routes a lone card-consuming ability through the modal, never auto-firing it", () => {
    seed({
      legalActionsByObject: { [String(FREED_ID)]: [FREED_TAP] },
      freedAbilities: [{ effect: { type: "Tap" }, consumes_source: true }],
    });

    fireEvent.click(fanCard("Freed from the Real"));

    expect(dispatchAction).not.toHaveBeenCalled();
    expect(useUiStore.getState().pendingAbilityChoice).toEqual({
      objectId: FREED_ID,
      actions: [FREED_TAP],
    });
  });

  // V4 — the timing gate. The bucket is populated exactly as in V1, but the
  // engine is not at Priority, so the fan must offer nothing at all.
  // POINTER: `REV5SWEEP[V4 DeclareBlockers canAct=T] act=[] mana=[]` versus
  // `REV5SWEEP[V4 Priority canAct=T] act=[408] mana=[]` — the same bucket, the
  // two timing arms.
  it("offers nothing at DeclareBlockers even though the bucket is populated", () => {
    seed({
      legalActionsByObject: { [String(FREED_ID)]: [FREED_TAP, FREED_UNTAP] },
      waitingFor: {
        type: "DeclareBlockers",
        data: { player: 0, valid_blocker_ids: [], valid_block_targets: {} },
      },
    });

    expect(isSelectable("Freed from the Real")).toBe(false);
    fireEvent.click(fanCard("Freed from the Real"));

    expect(dispatchAction).not.toHaveBeenCalled();
    expect(useUiStore.getState().pendingAbilityChoice).toBeNull();
    // Still open: an inert click must not dismiss the fan either.
    expect(useUiStore.getState().attachmentFanHostId).toBe(HOST_ID);
  });

  // V5 — the host invariant. The fan is opened FROM the host, so the host card
  // is the fan's anchor and never one of its picks, even when the engine
  // publishes actions for the host too. Assertion order: host first, then
  // attachment — the attachment assert is the control proving the fixture is
  // live rather than uniformly inert.
  //
  // MEASURED (drop side): dropping `id !== host.id` from `selectable` flips the
  //   host assert to `AssertionError: expected true to be false`.
  it("never makes the host itself a pick, while its attachment stays one", () => {
    seed({
      legalActionsByObject: {
        [String(HOST_ID)]: [{ type: "ActivateAbility", data: { source_id: HOST_ID, ability_index: 0 } }],
        [String(FREED_ID)]: [FREED_TAP, FREED_UNTAP],
      },
    });

    expect(isSelectable("Kilo, Apogee Mind")).toBe(false);
    expect(isSelectable("Freed from the Real")).toBe(true);

    fireEvent.click(fanCard("Kilo, Apogee Mind"));
    expect(dispatchAction).not.toHaveBeenCalled();
    expect(useUiStore.getState().pendingAbilityChoice).toBeNull();
  });

  // V7 — the ring is per-object, not per-fan. One attachment has a bucket and
  // the other does not, so exactly one lights up.
  // QUOTED (`.review3r2-probe1.log:115`):
  //   `A6 FIX freed=true boots=false host=false | MUTANT(drop priority arm)
  //    freed=false boots=false | MUTANT(always) freed=true boots=true`
  it("rings only the attachment the engine published actions for", () => {
    seed({
      attachments: [FREED_ID, BOOTS_ID],
      legalActionsByObject: { [String(FREED_ID)]: [FREED_TAP, FREED_UNTAP] },
    });

    expect(isSelectable("Freed from the Real")).toBe(true);
    expect(isSelectable("Swiftfoot Boots")).toBe(false);
    expect(isSelectable("Kilo, Apogee Mind")).toBe(false);

    fireEvent.click(fanCard("Swiftfoot Boots"));
    expect(useUiStore.getState().pendingAbilityChoice).toBeNull();
  });

  // V8 — mode 1 is byte-unchanged: with an interaction open the fan forwards
  // the engine's opaque submission and nothing else. PAIR-ONLY (§7.1): this row
  // claims the change altered NOTHING on its fixture, so no drop-the-fix mutant
  // can be visible on it by construction, and it is counted as always-side
  // coverage only. Its drop-side partner is V9.
  it("still forwards the engine's own submission when an interaction owns the prompt", () => {
    seed({
      legalActionsByObject: {},
      viewerInteraction: interactionOver([FREED_ID]),
    });

    fireEvent.click(fanCard("Freed from the Real"));

    expect(dispatchInteraction).toHaveBeenCalledTimes(1);
    expect(dispatchInteraction).toHaveBeenCalledWith({
      interactionId: "fan-interaction",
      response: { type: "choose", data: { choiceId: "fan-408" } },
    });
    expect(dispatchAction).not.toHaveBeenCalled();
    expect(useUiStore.getState().pendingAbilityChoice).toBeNull();
  });

  // V9 — MULTI-AUTHORITY HOSTILE FIXTURE. Both sources are live at once: an
  // interaction owns the prompt AND the same object has a populated bucket.
  // Exactly one authority may win, and it must be the engine's prompt.
  // Assertion order: interaction count, then action count, then the store.
  // QUOTED: `PD1[interaction + populated bucket] dispatchInteraction calls: 1,
  //          dispatchAction calls: 0, pending=null`
  // MEASURED (drop side): removing mode 1's early `return` makes mode 2 also
  //   fire, flipping `expected "spy" to not be called at all, but it was called
  //   1 times`.
  it("lets the engine interaction win when a prompt and a bucket are both live", () => {
    seed({
      legalActionsByObject: { [String(FREED_ID)]: [FREED_TAP, FREED_UNTAP] },
      viewerInteraction: interactionOver([FREED_ID]),
    });

    fireEvent.click(fanCard("Freed from the Real"));

    expect(dispatchInteraction).toHaveBeenCalledTimes(1);
    expect(dispatchAction).not.toHaveBeenCalled();
    expect(useUiStore.getState().pendingAbilityChoice).toBeNull();
  });
});
