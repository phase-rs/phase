import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { GameAction, GameObject, GameState, WaitingFor } from "../../../adapter/types.ts";
import { dispatchAction } from "../../../game/dispatch.ts";
import { useGameStore } from "../../../stores/gameStore.ts";
import { useUiStore } from "../../../stores/uiStore.ts";
import { buildGameObject, buildObjectMap } from "../../../test/factories/gameObjectFactory.ts";
import {
  buildGameState,
  buildPlayers,
  buildPriorityWaitingFor,
} from "../../../test/factories/gameStateFactory.ts";
import { CommandZone } from "../CommandZone.tsx";

/**
 * The emblem chip adopting the shared activation authority (plan §6.5).
 * Rows V20 (timing axis), V20b (seat axis — a LIVE cross-seat bug), and
 * V20c (D5: modal action order on the reachable emblem shape).
 *
 * Before this change the chip decided with `emblemActions.length > 0`: no
 * `WaitingFor` gate and no seat gate. `PlayerArea` renders a `<CommandZone
 * playerId={opponentId}/>`, and in local/AI mode `legalActionsByObject` is
 * computed for the STATE's priority player rather than the viewer — so an
 * opponent's emblem chip was clickable, and dispatched, from this seat.
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
 *   PAIR-ONLY / DROP-ONLY   the row carries ONE side by construction (§7.1).
 *
 * An untagged comment is prose and is evidence for nothing.
 *
 * NAMED UNMEASURED ANCHOR IN THIS FILE: D5's order claim is asserted on the
 * STORE payload (`pendingAbilityChoice.actions`), NOT on rendered option
 * labels. `AbilityChoiceModal` is module-private inside `GamePage.tsx`, so no
 * component-level row here can observe its labels; that consumer is registered
 * as an audited absence (plan §7.3 row V26) and is coverage for nothing.
 * ---------------------------------------------------------------------------
 */

vi.mock("../../../game/dispatch.ts", () => ({
  dispatchAction: vi.fn(),
  dispatchInteraction: vi.fn(),
}));

vi.mock("../../../hooks/useCardImage.ts", () => ({
  useCardImage: () => ({ src: null, isLoading: false, isRotated: false, isFlip: false }),
}));

const EMBLEM_ID = 700;
/** A SECOND emblem that groups with EMBLEM_ID — the non-representative member. */
const SIBLING_EMBLEM_ID = 701;

/** CR 114.4 + CR 602.1: an emblem can carry an activated ability. */
const EMBLEM_ABILITY_0: GameAction = {
  type: "ActivateAbility",
  data: { source_id: EMBLEM_ID, ability_index: 0 },
};
const EMBLEM_ABILITY_1: GameAction = {
  type: "ActivateAbility",
  data: { source_id: EMBLEM_ID, ability_index: 1 },
};

function makeEmblem(
  options: { controller?: number; abilities?: unknown[]; id?: number } = {},
): GameObject {
  return buildGameObject({
    id: options.id ?? EMBLEM_ID,
    card_id: 7000,
    zone: "Command",
    owner: options.controller ?? 0,
    controller: options.controller ?? 0,
    name: "Emblem",
    is_emblem: true,
    entered_battlefield_turn: null,
    card_types: { supertypes: [], core_types: [], subtypes: [] },
    abilities: (options.abilities ?? [
      { description: "{X}, Discard a card: Create a token.", effect: { type: "CreateToken" } },
    ]) as GameObject["abilities"],
  });
}

function makeState(emblem: GameObject, waitingFor: WaitingFor): GameState {
  return buildGameState({
    players: buildPlayers([0, 1]),
    objects: buildObjectMap(emblem),
    battlefield: [],
    exile: [],
    stack: [],
    command_zone: [EMBLEM_ID],
    waiting_for: waitingFor,
  });
}

const DECLARE_BLOCKERS: WaitingFor = {
  type: "DeclareBlockers",
  data: { player: 0, valid_blocker_ids: [], valid_block_targets: {} },
};

function seed(options: {
  abilities?: unknown[];
  controller?: number;
  legalActionsByObject?: Record<string, GameAction[]>;
  waitingFor?: WaitingFor;
  viewerSeat?: number;
} = {}) {
  const emblem = makeEmblem({ controller: options.controller, abilities: options.abilities });
  const waitingFor = options.waitingFor ?? buildPriorityWaitingFor();
  useGameStore.setState({
    gameMode: "local",
    gameState: makeState(emblem, waitingFor),
    waitingFor,
    legalActions: [],
    legalActionsByObject: options.legalActionsByObject ?? {},
    viewerInteraction: null,
  });
  useUiStore.setState({ pendingAbilityChoice: null });
  return render(<CommandZone playerId={options.controller ?? 0} />);
}

/** `data-activatable` is the chip's own rendering of the affordance verdict. */
function chipIsActivatable(): boolean {
  const chip = screen.getByTestId("emblem-card");
  return chip.getAttribute("data-activatable") === "true";
}

describe("CommandZone emblem chip activation gate", () => {
  beforeEach(() => {
    vi.mocked(dispatchAction).mockReset();
    vi.mocked(dispatchAction).mockResolvedValue(undefined);
  });

  afterEach(() => {
    cleanup();
    useUiStore.setState({ pendingAbilityChoice: null });
  });

  // V20 — the TIMING axis. Identical bucket on both arms; only the engine's
  // `WaitingFor` differs. Assertion order: the DeclareBlockers arm (the one a
  // restored `emblemActions.length > 0` fails), then the Priority arm, which is
  // the control proving this fixture can be activatable at all.
  //
  // MEASURED (drop side): restoring `isActivatable = emblemActions.length > 0`
  //   flips the first assert to `expected true to be false`.
  it("is inert at DeclareBlockers and activatable at Priority with the same bucket", () => {
    const bucket = { [String(EMBLEM_ID)]: [EMBLEM_ABILITY_0] };

    seed({ legalActionsByObject: bucket, waitingFor: DECLARE_BLOCKERS });
    expect(chipIsActivatable()).toBe(false);
    expect(screen.queryByRole("button")).toBeNull();
    cleanup();

    seed({ legalActionsByObject: bucket });
    expect(chipIsActivatable()).toBe(true);
    fireEvent.click(screen.getByTestId("emblem-card"));
    expect(dispatchAction).toHaveBeenCalledTimes(1);
    expect(dispatchAction).toHaveBeenCalledWith(EMBLEM_ABILITY_0);
  });

  // V20b — the SEAT axis, and a LIVE bug: `PlayerArea` → `CommandDock` renders
  // this component for the opponent's seat too.
  // QUOTED (`.plan3r4-transcript.log:30-31`):
  //   `PF1[opponent emblem @ OPPONENT Priority, viewer=P0] measuredCanAct=false
  //    today data-activatable=true dispatches=1 | adopted activatable=false mana=false`
  //   `PF1b[own emblem @ own Priority] measuredCanAct=true today
  //    data-activatable=true dispatches=1 | adopted activatable=true`
  it("does not offer an opponent's emblem chip from this viewer's seat", () => {
    // The engine is waiting on player 1; the local viewer is player 0. The
    // bucket is populated exactly as in the own-seat arm below.
    seed({
      controller: 1,
      legalActionsByObject: { [String(EMBLEM_ID)]: [EMBLEM_ABILITY_0] },
      waitingFor: { type: "Priority", data: { player: 1 } },
    });

    expect(chipIsActivatable()).toBe(false);
    fireEvent.click(screen.getByTestId("emblem-card"));
    expect(dispatchAction).not.toHaveBeenCalled();
    cleanup();

    // Control: the same chip, on this viewer's own turn, IS offered — so the
    // arm above cannot pass by "the chip is never activatable".
    seed({ legalActionsByObject: { [String(EMBLEM_ID)]: [EMBLEM_ABILITY_0] } });
    expect(chipIsActivatable()).toBe(true);
  });

  // V20c (D5) — action ORDER is preserved on the reachable emblem shape. CR
  // 114.5: an emblem cannot tap for mana, so its bucket never interleaves the
  // mana and non-mana groups and the partitioned list equals the raw one.
  // QUOTED: `PE1[E1 two NON-mana activated abilities (the reachable emblem
  //          shape)] today order=[0,1] adopted order=[0,1] SAME=true`
  // POINTER: sweep row `E1 two NON-mana abilities`, mutants `allReverse` and
  //   `withinGroupReverse`. The expectation is a LITERAL pair, not a list the
  //   test re-derives from `legalActionsByObject`.
  it("opens the chooser with both emblem abilities in engine order", () => {
    seed({
      abilities: [
        { description: "{X}: Create a token.", effect: { type: "CreateToken" } },
        { description: "{2}: Draw a card.", effect: { type: "Draw" } },
      ],
      legalActionsByObject: { [String(EMBLEM_ID)]: [EMBLEM_ABILITY_0, EMBLEM_ABILITY_1] },
    });

    fireEvent.click(screen.getByTestId("emblem-card"));

    expect(useUiStore.getState().pendingAbilityChoice).toEqual({
      objectId: EMBLEM_ID,
      actions: [EMBLEM_ABILITY_0, EMBLEM_ABILITY_1],
    });
    expect(dispatchAction).not.toHaveBeenCalled();
  });
});

/**
 * GROUPED-EMBLEM ACTIVATION IDENTITY.
 *
 * Maintainer review (matthewevans, CHANGES_REQUESTED, 2026-08-04) `[MED]
 * Grouped emblem actions can make legal nonrepresentative emblems unreachable`,
 * independently reported by CodeRabbit as `Preserve activation identity for
 * every grouped emblem`. CR 114.1: each emblem is its own object and
 * `legalActionsByObject` is keyed by exact object id, so a chip that asked the
 * activation authority about `representative.id` alone rendered inert whenever
 * the engine had published the action against a sibling — the command-zone UI
 * could hide a legal action.
 *
 * The grouping itself is unchanged (display stacking by `source | description`,
 * CR 114); what changed is WHICH id the same authority is asked about.
 */
describe("CommandZone grouped-emblem activation identity", () => {
  beforeEach(() => {
    vi.mocked(dispatchAction).mockReset();
    vi.mocked(dispatchAction).mockResolvedValue(undefined);
  });

  afterEach(() => {
    cleanup();
    useUiStore.setState({ pendingAbilityChoice: null });
  });

  function abilityAction(sourceId: number, abilityIndex = 0): GameAction {
    return { type: "ActivateAbility", data: { source_id: sourceId, ability_index: abilityIndex } };
  }

  /**
   * Two emblems that GROUP: no `emblem_source`, identical granted text, so the
   * key `source | description` collides and `CommandZone` renders exactly one
   * chip. `command_zone` order fixes EMBLEM_ID as the representative and
   * SIBLING_EMBLEM_ID as the non-representative member.
   */
  function seedGroupedPair(legalActionsByObject: Record<string, GameAction[]>) {
    const representative = makeEmblem({ id: EMBLEM_ID });
    const sibling = makeEmblem({ id: SIBLING_EMBLEM_ID });
    const waitingFor = buildPriorityWaitingFor();
    useGameStore.setState({
      gameMode: "local",
      gameState: buildGameState({
        players: buildPlayers([0, 1]),
        objects: buildObjectMap(representative, sibling),
        battlefield: [],
        exile: [],
        stack: [],
        command_zone: [EMBLEM_ID, SIBLING_EMBLEM_ID],
        waiting_for: waitingFor,
      }),
      waitingFor,
      legalActions: [],
      legalActionsByObject,
      viewerInteraction: null,
    });
    useUiStore.setState({ pendingAbilityChoice: null });
    return render(<CommandZone playerId={0} />);
  }

  /** Structural precondition every row below depends on: the two emblems really
   *  did collapse into ONE chip carrying a ×2 badge. Without this, a row could
   *  pass because the sibling rendered its own separate chip. */
  function expectOneChipOfTwo() {
    expect(screen.getAllByTestId("emblem-card")).toHaveLength(1);
    expect(screen.getByText("×2")).toBeInTheDocument();
  }

  // THE discriminating row both reviewers asked for.
  //
  // MEASURED (drop side): restoring the representative-only derivation
  //   (`isActivatable` from `emblem.id`, `handleActivate` resolving on
  //   `emblem.id`) flips this row with
  //   `AssertionError: expected false to be true // Object.is equality`
  //   on `chipIsActivatable()` — the group renders inert and the legal action
  //   is unreachable. The chooser row below flips in the same arm with
  //   `AssertionError: expected null to deeply equal { objectId: 701, actions:
  //   [ …(2) ] }`.
  // MEASURED (trivialize side): replacing the affordance-driven `find` with
  //   "always the last member" fails five rows — the two controls below, the
  //   negative control, and the timing/seat rows in the describe above.
  it("reaches an action published against a non-representative group member", () => {
    const action = abilityAction(SIBLING_EMBLEM_ID);
    seedGroupedPair({ [String(SIBLING_EMBLEM_ID)]: [action] });

    expectOneChipOfTwo();
    expect(chipIsActivatable()).toBe(true);

    fireEvent.click(screen.getByTestId("emblem-card"));

    expect(dispatchAction).toHaveBeenCalledTimes(1);
    // The SIBLING's own action, dispatched under the sibling's own id.
    expect(dispatchAction).toHaveBeenCalledWith(action);
  });

  // The dispatch axis is not the only one the maintainer named: the pending
  // choice must also carry the member's EXACT id, or the chooser would resolve
  // against an object the engine never offered.
  it("opens the chooser against the non-representative member's own id", () => {
    const first = abilityAction(SIBLING_EMBLEM_ID, 0);
    const second = abilityAction(SIBLING_EMBLEM_ID, 1);
    seedGroupedPair({ [String(SIBLING_EMBLEM_ID)]: [first, second] });

    expectOneChipOfTwo();
    fireEvent.click(screen.getByTestId("emblem-card"));

    expect(useUiStore.getState().pendingAbilityChoice).toEqual({
      objectId: SIBLING_EMBLEM_ID,
      actions: [first, second],
    });
    expect(dispatchAction).not.toHaveBeenCalled();
  });

  // CONTROL — the representative-live case must keep working, so the row above
  // cannot pass by "the chip now always picks the last member".
  it("still reaches an action published against the representative member", () => {
    const action = abilityAction(EMBLEM_ID);
    seedGroupedPair({ [String(EMBLEM_ID)]: [action] });

    expectOneChipOfTwo();
    expect(chipIsActivatable()).toBe(true);

    fireEvent.click(screen.getByTestId("emblem-card"));

    expect(dispatchAction).toHaveBeenCalledTimes(1);
    expect(dispatchAction).toHaveBeenCalledWith(action);
  });

  // CONTROL — every member live: ONE chip is ONE click, so exactly one dispatch
  // (the first member in command-zone order), and the display count is intact.
  it("dispatches exactly once when every group member is live", () => {
    const representativeAction = abilityAction(EMBLEM_ID);
    const siblingAction = abilityAction(SIBLING_EMBLEM_ID);
    seedGroupedPair({
      [String(EMBLEM_ID)]: [representativeAction],
      [String(SIBLING_EMBLEM_ID)]: [siblingAction],
    });

    expectOneChipOfTwo();
    fireEvent.click(screen.getByTestId("emblem-card"));

    expect(dispatchAction).toHaveBeenCalledTimes(1);
    expect(dispatchAction).toHaveBeenCalledWith(representativeAction);
    expect(useUiStore.getState().pendingAbilityChoice).toBeNull();
  });

  // NEGATIVE CONTROL — no member live: inert. Its reach guard is the first row
  // above, which shows this same fixture DOES light up when a member is offered.
  it("stays inert when no group member has a legal action", () => {
    seedGroupedPair({});

    expectOneChipOfTwo();
    expect(chipIsActivatable()).toBe(false);
    expect(screen.queryByRole("button")).toBeNull();

    fireEvent.click(screen.getByTestId("emblem-card"));
    expect(dispatchAction).not.toHaveBeenCalled();
  });
});
