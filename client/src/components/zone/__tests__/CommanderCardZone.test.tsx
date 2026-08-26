import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { GameAction, GameObject, PlayerId, WaitingFor } from "../../../adapter/types.ts";
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
// `vi.hoisted` because the `vi.mock` factory is hoisted above module-scope
// `const`s and would otherwise read them in their TDZ.
const cardImage = vi.hoisted(() => ({
  calls: [] as Array<{ name: string; options?: Record<string, unknown> }>,
  result: { src: null as string | null, isLoading: false },
}));

vi.mock("../../../hooks/useCardImage.ts", () => ({
  useCardImage: (name: string, options?: Record<string, unknown>) => {
    cardImage.calls.push({ name, options });
    return cardImage.result;
  },
}));

const COMMANDER_ID = 101;

function makeState(
  commander: GameObject,
  options: { waitingFor?: WaitingFor; activeSeat?: PlayerId } = {},
) {
  const seat = options.activeSeat ?? 0;
  return buildGameState({
    active_player: seat,
    priority_player: seat,
    // Nobody controls another player's turn decisions here, so
    // `useCanActForWaitingState` reduces to "is the waiting seat my seat" —
    // the escape hatch cannot silently rescue the opponent-seat arm below.
    turn_decision_controller: null,
    players: buildPlayers([0, 1]),
    objects: buildObjectMap(commander),
    command_zone: [commander.id],
    battlefield: [],
    exile: [],
    stack: [],
    waiting_for: options.waitingFor ?? buildPriorityWaitingFor({ data: { player: seat } }),
  });
}

const DECLARE_BLOCKERS: WaitingFor = {
  type: "DeclareBlockers",
  data: { player: 0, valid_blocker_ids: [], valid_block_targets: {} },
};

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
function seedStores(
  actions: GameAction[],
  options: { seat?: PlayerId; waitingFor?: WaitingFor } = {},
) {
  const seat = options.seat ?? 0;
  const commander = buildCommanderGameObject({
    id: COMMANDER_ID,
    owner: seat,
    controller: seat,
  });
  const gameState = makeState(commander, { waitingFor: options.waitingFor, activeSeat: seat });
  useGameStore.setState({
    gameMode: "local",
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
      gameMode: "local",
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

/**
 * Sibling sweep of the `CommandZone.EmblemCard` hunk in this PR: the same
 * raw-bucket predicate (`castAction !== null` / `ninjutsuActions.length > 0`,
 * with NEITHER a `WaitingFor` gate NOR a seat gate) survived one file over, in
 * a component `PlayerArea` → `CommandDock` renders for every seat. Pre-existing
 * rather than introduced by this PR, and disclosed as such.
 *
 * EVIDENCE-LABEL CONVENTION (same as `CommandZone.test.tsx`):
 *   MEASURED — past-tense report: this PR ran that arm; the quoted text is the
 *              assertion that flipped.
 * An untagged comment is prose and is evidence for nothing.
 */
describe("CommanderCardZone activation gate", () => {
  beforeEach(() => {
    vi.mocked(dispatchAction).mockClear();
  });

  afterEach(() => {
    cleanup();
    useUiStore.setState({ inspectedObjectId: null, pendingAbilityChoice: null });
  });

  // The SEAT axis. In local/AI mode `legalActionsByObject` is computed for the
  // STATE's priority player, not the viewer, so an opponent's commander chip
  // dispatched from this seat.
  //
  // MEASURED (drop side): restoring `const canNinjutsu = ninjutsuActions.length
  //   > 0` flips the first assertion to `AssertionError: expected "vi.fn()" to
  //   not be called at all, but actually been called 1 times`.
  // MEASURED (always side): forcing both flags false fails this row's control
  //   arm plus three pre-existing rows in the describe above.
  it("does not offer an opponent's commander ninjutsu from this viewer's seat", () => {
    const action = ninjutsuAction(9);
    // The engine is waiting on player 1; the local viewer is player 0.
    seedStores([action], { seat: 1 });

    render(<CommanderCardZone playerId={1} />);
    fireEvent.click(screen.getByRole("button"));

    expect(dispatchAction).not.toHaveBeenCalled();
    // REACH GUARD — without it the negative above is vacuous. The click WAS
    // delivered and DID take a branch: it fell through to inspect.
    expect(useUiStore.getState().inspectedObjectId).toBe(COMMANDER_ID);
    cleanup();

    // Control: the identical bucket on this viewer's own seat IS offered, so
    // the arm above cannot pass by "this fixture never activates".
    vi.mocked(dispatchAction).mockClear();
    seedStores([action]);
    render(<CommanderCardZone playerId={0} />);
    fireEvent.click(screen.getByRole("button"));

    expect(dispatchAction).toHaveBeenCalledTimes(1);
    expect(dispatchAction).toHaveBeenCalledWith(action);
  });

  // The TIMING axis, both flags at once. Identical bucket on both arms; only
  // the engine's `WaitingFor` differs. `title` is the component's own rendering
  // of `canCast`, the click outcome is its rendering of `canNinjutsu`.
  //
  // MEASURED (drop side): restoring the two raw-bucket predicates flips this
  //   row with `expect(element).toHaveAttribute("title", "Commander: Mock
  //   Commander")` — the chip renders the cast title at DeclareBlockers.
  // MEASURED (always side): forcing both flags false fails this row's control
  //   arm (the Priority half).
  it("is inert at DeclareBlockers and live at Priority with the same bucket", () => {
    const bucket = [castAction(), ninjutsuAction(9)];

    seedStores(bucket, { waitingFor: DECLARE_BLOCKERS });
    render(<CommanderCardZone playerId={0} />);
    const inert = screen.getByRole("button");
    expect(inert).toHaveAttribute("title", "Commander: Mock Commander");
    fireEvent.click(inert);
    expect(dispatchAction).not.toHaveBeenCalled();
    // REACH GUARD, as above.
    expect(useUiStore.getState().inspectedObjectId).toBe(COMMANDER_ID);
    cleanup();

    // Control: the same bucket at Priority advertises the cast and activates.
    vi.mocked(dispatchAction).mockClear();
    seedStores(bucket);
    render(<CommanderCardZone playerId={0} />);
    const live = screen.getByRole("button");
    expect(live).toHaveAttribute("title", "Cast Mock Commander — double-click or drag to play");
    fireEvent.click(live);
    expect(dispatchAction).toHaveBeenCalledTimes(1);
  });
});

/**
 * The command dock is the one board surface that used to resolve art from the
 * bare card name and render a single name tile for BOTH "still resolving" and
 * "no art exists". Reported as commander art never rendering: `scryfall-data.json`
 * is tens of MB, and for its whole download the dock showed a name with no
 * picture — indistinguishable from permanently broken art.
 */
describe("CommanderCardZone art resolution", () => {
  const ORACLE_ID = "6d3ca5cb-781d-4608-b7f1-38c020536376";
  const ART_URL = "https://cards.scryfall.io/normal/front/8/f/kefka.jpg";

  function seedWithPrintedRef() {
    const commander = buildCommanderGameObject({
      id: COMMANDER_ID,
      name: "Kefka, Court Mage",
      printed_ref: { oracle_id: ORACLE_ID, face_name: "Kefka, Court Mage" },
    });
    const gameState = makeState(commander);
    useGameStore.setState({
      gameMode: "local",
      gameState,
      waitingFor: gameState.waiting_for,
      legalActions: [],
      legalActionsByObject: {},
      spellCosts: {},
    });
  }

  beforeEach(() => {
    cardImage.calls.length = 0;
    cardImage.result = { src: null, isLoading: false };
    seedWithPrintedRef();
  });

  afterEach(() => {
    cleanup();
  });

  it("resolves art through the engine's printed_ref rather than the card name", () => {
    render(<CommanderCardZone playerId={0} />);

    // The name path is the documented legacy fallback for objects with no
    // printed_ref; a command-zone leader always has one, and only the oracle-id
    // path picks the right face for a leader whose active face is not the front.
    expect(cardImage.calls[0].options).toMatchObject({
      oracleId: ORACLE_ID,
      faceName: "Kefka, Court Mage",
    });
  });

  it("does not show the no-art name tile while resolution is still in flight", () => {
    cardImage.result = { src: null, isLoading: true };
    render(<CommanderCardZone playerId={0} />);

    // The regression: the name tile rendered during the whole scryfall-data
    // fetch, reading as "this commander has no art" rather than "art is
    // loading". Assert on the rendered NAME TEXT — the pre-fix tile was a plain
    // <div>, so a role-based query would have been null under both versions.
    expect(screen.queryByText("Kefka, Court Mage")).toBeNull();
  });

  it("shows the shared name tile once resolution finishes with no art", () => {
    cardImage.result = { src: null, isLoading: false };
    render(<CommanderCardZone playerId={0} />);

    // Named art tile, not bare text: CardArtFallback carries role/aria-label so
    // assistive tech announces an artless commander the way every other board
    // surface does.
    expect(screen.getByRole("img", { name: "Kefka, Court Mage" })).toBeInTheDocument();
    expect(screen.getByText("Kefka, Court Mage")).toBeInTheDocument();
  });

  it("renders the resolved art", () => {
    cardImage.result = { src: ART_URL, isLoading: false };
    render(<CommanderCardZone playerId={0} />);

    expect(screen.getByAltText("Kefka, Court Mage")).toHaveAttribute("src", ART_URL);
  });
});
