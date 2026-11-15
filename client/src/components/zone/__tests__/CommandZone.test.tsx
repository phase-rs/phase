import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";

import type { CardPrinting, GameAction, GameObject, GameState, WaitingFor } from "../../../adapter/types.ts";
import { dispatchAction } from "../../../game/dispatch.ts";
import { useGameStore } from "../../../stores/gameStore.ts";
import { useUiStore } from "../../../stores/uiStore.ts";
import { usePreferencesStore } from "../../../stores/preferencesStore.ts";
import { useMultiplayerStore } from "../../../stores/multiplayerStore.ts";
import { loadScryfallData, type PrintingEntry } from "../../../services/scryfall.ts";
import type * as Platform from "../../../services/platform.ts";
import type { ResolvedAsset } from "../../../services/visualPacks/types.ts";
import { assetKey, catalogRoot, installedRevision, packId } from "../../../services/visualPacks/types.ts";
import { encodeCandidateKey } from "../../../services/visualPacks/candidateKeys.ts";
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

const installed = vi.hoisted(() => new Map<string, ResolvedAsset[]>());
vi.mock("../../../services/platform.ts", async (original) => ({
  ...await original<typeof Platform>(),
  loadVisualPackBackend: async () => ({
    resolve: async (keys: { key: string }[]) => ({
      revision: installedRevision("0"),
      entries: keys.map((key, ordinal) => ({ key, ordinal, matches: installed.get(key.key) ?? [] })),
    }),
    subscribeRevision: async () => () => {},
  }),
}));

const ORACLE = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
const OTHER_ORACLE = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb";
const A = "11111111-1111-4111-8111-111111111111";
const B = "22222222-2222-4222-8222-222222222222";
const V = "33333333-3333-4333-8333-333333333333";
const printingA: CardPrinting = { set_code: "own", collector_number: "1" };
const printingB: CardPrinting = { set_code: "own", collector_number: "2" };
let admitCatalog: () => void;

beforeAll(async () => {
  const entry = { oracle_id: ORACLE, name: "Alpha", face_names: ["alpha"], layout: "normal",
    mana_cost: "{1}", cmc: 1, type_line: "Creature", colors: [], color_identity: [], keywords: [],
    faces: [{ normal: "/canonical.jpg", art_crop: "/canonical-crop.jpg" }] };
  const catalog: Record<string, PrintingEntry[]> = { [ORACLE]: [
    [A, "own", "1"], [B, "own", "2"], [V, "pin", "3"],
  ].map(([id, set, collector_number]) => ({
    id, set, collector_number, set_name: set, released_at: "2026-01-01", border_color: "black",
    frame_effects: [], full_art: false, faces: [{ normal: `/${id}.jpg`, art_crop: `/${id}-crop.jpg` }],
  })) };
  catalog[OTHER_ORACLE] = [{ ...catalog[ORACLE][0], id: B,
    faces: [{ normal: `/${B}.jpg`, art_crop: `/${B}-crop.jpg` }] }];
  const gate = new Promise<Response>((resolve) => {
    admitCatalog = () => resolve(new Response(JSON.stringify(catalog)));
  });
  vi.stubGlobal("fetch", vi.fn(async (input: RequestInfo | URL) =>
    new URL(String(input), "http://localhost").pathname === "/scryfall-printings.json"
      ? gate : new Response(JSON.stringify({ [ORACLE]: entry, alpha: entry,
        [OTHER_ORACLE]: { ...entry, oracle_id: OTHER_ORACLE } }))));
  for (const id of [A, B, V]) {
    installed.set(encodeCandidateKey("english_printing", [id, 0, "art_crop", "art_crop"]), [{
      packId: packId("complete"), assetKey: assetKey("asset:v1:exact_printing:QQ"),
      catalogRoot: catalogRoot("a".repeat(64)), url: `/${id}-crop.jpg`, media: "image/jpeg",
    }]);
  }
  await loadScryfallData();
});

beforeEach(() => {
  useMultiplayerStore.setState({ activePlayerId: 0, isSpectator: false });
  usePreferencesStore.setState({ ...usePreferencesStore.getInitialState(), language: "en",
    showOwnerPrintings: true, artChain: [],
    artOverrides: { [ORACLE]: { scryfallId: V, setCode: "pin", collectorNumber: "3" } } });
});
afterEach(() => {
  usePreferencesStore.setState(usePreferencesStore.getInitialState());
});

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

  it("anchors the active representative and every public member of its collapsed emblem group", () => {
    const action = abilityAction(EMBLEM_ID);
    seedGroupedPair({ [String(EMBLEM_ID)]: [action] });

    const chip = screen.getByTestId("emblem-card");
    expect(chip).toHaveAttribute("data-object-id", String(EMBLEM_ID));
    expect(chip).toHaveAttribute("data-grouped-ids", `${EMBLEM_ID} ${SIBLING_EMBLEM_ID}`);
  });

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

describe("CommandZone owner-printing groups", () => {
  beforeEach(() => {
    vi.mocked(dispatchAction).mockReset();
    vi.mocked(dispatchAction).mockResolvedValue(undefined);
    useUiStore.setState(useUiStore.getInitialState());
  });
  afterEach(() => cleanup());

  function seedPrintings(printings: Array<CardPrinting | null | undefined>, legal = true, holdCatalog = false) {
    if (!holdCatalog) admitCatalog();
    const emblems = printings.map((printing, index) => ({
      ...makeEmblem({ id: 700 + index }),
      // Deliberately contradict the source on the first chip. Own artwork must
      // neither select nor partition a synthetic emblem's source image.
      printing: index === 0 ? printingB : printingA,
      emblem_source: { name: "Alpha", printed_ref: { oracle_id: ORACLE, face_name: "Alpha" }, printing },
    }));
    const waitingFor = buildPriorityWaitingFor();
    const state = buildGameState({ players: buildPlayers([0, 1]), objects: buildObjectMap(...emblems),
      command_zone: emblems.map((emblem) => emblem.id), waiting_for: waitingFor });
    useGameStore.setState({ gameMode: "local", gameState: state, waitingFor,
      legalActionsByObject: legal ? { "701": [{
        type: "ActivateAbility", data: { source_id: 701, ability_index: 0 },
      }] } : {}, viewerInteraction: null });
    return state;
  }

  it("partitions captured source A/A/B before admission and preserves sibling activation through a live cycle", async () => {
    const state = seedPrintings([printingA, { collector_number: "1", set_code: "own" }, printingB], true, true);
    const { container } = render(<CommandZone playerId={0} />);
    expect(screen.getAllByTestId("emblem-card").map((chip) => chip.dataset.groupedIds)).toEqual(["700 701", "702"]);
    await waitFor(() => expect(container.querySelectorAll(`img[src="/${V}-crop.jpg"]`)).toHaveLength(2));
    admitCatalog();
    await waitFor(() => {
      expect(screen.getAllByTestId("emblem-card")[0].querySelector("img")).toHaveAttribute("src", `/${A}-crop.jpg`);
      expect(screen.getAllByTestId("emblem-card")[1].querySelector("img")).toHaveAttribute("src", `/${B}-crop.jpg`);
    });
    for (const enabled of [true, false, true]) {
      act(() => usePreferencesStore.getState().setShowOwnerPrintings(enabled));
      const chips = screen.getAllByTestId("emblem-card");
      expect(chips.map((chip) => chip.dataset.groupedIds)).toEqual(enabled ? ["700 701", "702"] : ["700 701 702"]);
      expect(screen.getByText(enabled ? "×2" : "×3")).toBeInTheDocument();
      expect(chips[0]).toHaveAttribute("data-object-id", "701");
      await waitFor(() => expect(chips[0].querySelector("img")).toHaveAttribute("src", `/${enabled ? A : V}-crop.jpg`));
      vi.mocked(dispatchAction).mockClear();
      fireEvent.click(chips[0]);
      expect(dispatchAction).toHaveBeenCalledExactlyOnceWith({
        type: "ActivateAbility", data: { source_id: 701, ability_index: 0 },
      });
    }
    expect(useGameStore.getState().gameState).toBe(state);
  });

  it("keeps source null and omission together, separates present source art, and preserves the no-source fallback", async () => {
    const state = seedPrintings([null, undefined, printingA], false);
    const noSource = makeEmblem({ id: 703 });
    state.objects[703] = noSource;
    state.command_zone!.push(703);
    const { container } = render(<CommandZone playerId={0} />);
    expect(screen.getAllByTestId("emblem-card").map((chip) => chip.dataset.groupedIds)).toEqual(["700 701", "702", "703"]);
    await waitFor(() => expect(container.querySelector(`img[src="/${A}-crop.jpg"]`)).not.toBeNull());
    const chips = screen.getAllByTestId("emblem-card");
    expect(chips[2].querySelector('img[alt="Emblem"]')).toBeNull();
    expect(chips[2]).toHaveTextContent("Create a token.");
    fireEvent.click(chips[0]);
    expect(dispatchAction).not.toHaveBeenCalled();
    act(() => usePreferencesStore.getState().setShowOwnerPrintings(false));
    expect(screen.getAllByTestId("emblem-card").map((chip) => chip.dataset.groupedIds)).toEqual(["700 701 702", "703"]);
  });

  it("ignores own-printing changes and keeps same-source equal pairs compact", async () => {
    const state = seedPrintings([printingA, { ...printingA }]);
    const { container } = render(<CommandZone playerId={0} />);
    expect(screen.getByTestId("emblem-card")).toHaveAttribute("data-grouped-ids", "700 701");
    await waitFor(() => expect(container.querySelector("img")).toHaveAttribute("src", `/${A}-crop.jpg`));
    act(() => useGameStore.setState({ gameState: {
      ...state, objects: { ...state.objects, "701": { ...state.objects[701],
        printing: { set_code: "unrelated", collector_number: "99" } } },
    } }));
    expect(screen.getByTestId("emblem-card")).toHaveAttribute("data-grouped-ids", "700 701");
    expect(screen.getByTestId("emblem-card").querySelector("img")).toHaveAttribute("src", `/${A}-crop.jpg`);
  });

  it.each(["different", "missing", "equal", "legacy"] as const)(
    "uses selected own emblem authority with equal provenance: %s", async (authority) => {
      for (const reverse of [false, true]) {
        const state = seedPrintings([printingA, printingA]);
        usePreferencesStore.setState({ showOwnerPrintings: true, artOverrides: {
          [ORACLE]: { scryfallId: V, setCode: "pin", collectorNumber: "3" },
          [OTHER_ORACLE]: { scryfallId: V, setCode: "pin", collectorNumber: "3" },
        } });
        for (const id of [700, 701]) {
          state.objects[id].printing = printingA;
          state.objects[id].printed_ref = { oracle_id: authority === "different" && id === 701 ? OTHER_ORACLE : ORACLE, face_name: "Alpha" };
          state.objects[id].emblem_source = { name: "Alpha", printed_ref: null, printing: printingB };
        }
        if (authority === "missing" || authority === "legacy") state.objects[701].printed_ref = null;
        if (authority === "legacy") state.objects[700].printed_ref = null;
        if (reverse) state.command_zone!.reverse();
        act(() => useGameStore.setState({ gameState: { ...state } }));
        const surface = render(<CommandZone playerId={0} />);
        const split = authority === "different" || authority === "missing";
        const order = reverse ? [701, 700] : [700, 701];
        const chips = screen.getAllByTestId("emblem-card");
        expect(chips.map((chip) => chip.dataset.groupedIds)).toEqual(split ? order.map(String) : [order.join(" ")]);
        for (const [index, chip] of chips.entries()) {
          const id = order[index];
          const expected = authority === "legacy" || (authority === "missing" && id === 701)
            ? V : authority === "different" && id === 701 ? B : A;
          await waitFor(() => expect(chip.querySelector("img")).toHaveAttribute("src", `/${expected}-crop.jpg`));
        }
        for (const id of [700, 701]) {
          const action = { type: "ActivateAbility" as const, data: { source_id: id, ability_index: 0 } };
          act(() => useGameStore.setState({ legalActionsByObject: { [id]: [action] } }));
          vi.mocked(dispatchAction).mockClear();
          const chip = screen.getAllByTestId("emblem-card").find((element) => element.dataset.groupedIds!.split(" ").includes(String(id)))!;
          fireEvent.click(chip);
          expect(dispatchAction).toHaveBeenCalledExactlyOnceWith(action);
        }
        act(() => usePreferencesStore.getState().setShowOwnerPrintings(false));
        expect(screen.getByTestId("emblem-card").dataset.groupedIds).toBe(order.join(" "));
        await waitFor(() => expect(screen.getByTestId("emblem-card").querySelector("img")).toHaveAttribute("src", `/${V}-crop.jpg`));
        surface.unmount();
      }
    },
  );

  it.each([
    [printingA, { set_code: "other", collector_number: "1" }],
    [{ set_code: "a|b", collector_number: "c" }, { set_code: "a", collector_number: "b|c" }],
    [{ set_code: "invalid", collector_number: "x" }, { set_code: "foreign", collector_number: "x" }],
  ])("compares source pairs, not admitted artwork (%j versus %j)", async (first, second) => {
    seedPrintings([first, { ...first }, second], false);
    const { container } = render(<CommandZone playerId={0} />);
    expect(screen.getAllByTestId("emblem-card").map((chip) => chip.dataset.groupedIds)).toEqual(["700 701", "702"]);
    await waitFor(() => expect(container.querySelectorAll(`img[src="/${V}-crop.jpg"]`)).toHaveLength(first === printingA ? 1 : 2));
  });

  it("keeps the sibling action inert from another seat, with the own-seat action as a control", () => {
    seedPrintings([printingA, printingA]);
    const { rerender } = render(<CommandZone playerId={0} />);
    act(() => {
      useGameStore.setState({ gameMode: "online" });
      useMultiplayerStore.setState({ activePlayerId: 1 });
    });
    const chip = screen.getByTestId("emblem-card");
    expect(chip).not.toHaveAttribute("data-activatable");
    fireEvent.click(chip);
    expect(dispatchAction).not.toHaveBeenCalled();
    act(() => useMultiplayerStore.setState({ activePlayerId: 0 }));
    rerender(<CommandZone playerId={0} />);
    expect(screen.getByTestId("emblem-card")).toHaveAttribute("data-activatable", "true");
    fireEvent.click(screen.getByTestId("emblem-card"));
    expect(dispatchAction).toHaveBeenCalledExactlyOnceWith({
      type: "ActivateAbility", data: { source_id: 701, ability_index: 0 },
    });
  });
});
