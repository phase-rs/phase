import "fake-indexeddb/auto";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";

import ts from "typescript";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  DraftEngineOperationLease,
  type DraftCardInstance,
  type DraftPlayerView,
} from "../../adapter/draft-adapter";
import {
  MAX_MATERIALIZED_VIRTUAL_BASICS,
  migrateLegacyWorkspace,
} from "../../components/draft/workspace/workspaceMigration";
import type {
  ActiveQuickDraftMeta,
  DraftMatchResult,
  DraftRunState,
} from "../../services/quickDraftPersistence";
import {
  createDraftWorkspaceState,
  makeInteractiveVirtualBasicInstanceId,
} from "../../components/draft/workspace/workspacePlacement";
import {
  createDefaultDraftWorkspacePreferences,
  setArrivingCardBoardPreferences,
} from "../../components/draft/workspace/workspacePreferences";
import { ACTIVE_QUICK_DRAFT_KEY, DRAFT_WORKSPACE_PREFERENCES_KEY } from "../../constants/storage";
import {
  projectWorkspaceLandCounts,
  projectWorkspaceMainDeck,
} from "../../components/draft/workspace/workspaceProjection";

const wasm = vi.hoisted(() => ({
  default: vi.fn(async () => undefined),
  start_quick_draft: vi.fn(),
  start_sealed_draft: vi.fn(),
  start_quick_cube_draft: vi.fn(),
  import_draft_session: vi.fn(),
  load_card_database: vi.fn(() => 0),
  submit_pick: vi.fn(),
  submit_pick_with_draft_effect: vi.fn(),
  auto_pick: vi.fn(),
  submit_deck: vi.fn(),
  suggest_deck: vi.fn(),
  suggest_lands: vi.fn(),
  get_bot_deck: vi.fn(),
  export_draft_session: vi.fn(() => "session"),
  booster_pack_pool_for_game: vi.fn<() => string[] | null | undefined>(() => null),
}));

const persistence = vi.hoisted(() => ({
  cleanupQuickDraftLifecycle: vi.fn(async () => undefined),
  drainQuickDraftPersistence: vi.fn(async () => undefined),
  inspectActiveQuickDraftLifecycle: vi.fn<() => Promise<unknown>>(async () => null),
  loadDraftRun: vi.fn<() => Promise<unknown>>(async () => null),
  saveDraftRun: vi.fn(async (_id: string, _run: DraftRunState) => undefined),
  loadQuickDraftSession: vi.fn<() => Promise<unknown>>(async () => null),
  persistQuickDraftSnapshot: vi.fn<
    (
      id: string,
      sessionJson: string,
      uiState: unknown,
      meta: { runFormat?: string; phase?: string },
    ) => Promise<void>
  >(async () => undefined),
  publishInitialDraftMatch: vi.fn<(input: { run: unknown }) => Promise<void>>(
    async () => undefined,
  ),
  publishStagedDraftMatch: vi.fn(async () => undefined),
  recordDraftMatchResult: vi.fn<
    (input: {
      draftId: string;
      gameId: string;
      result: DraftMatchResult;
      makeMeta: (run: DraftRunState) => ActiveQuickDraftMeta;
    }) => Promise<{ run: DraftRunState; meta: ActiveQuickDraftMeta } | null>
  >(async () => null),
  runLimits: vi.fn((format: string) => (
    format === "run" ? { maxWins: 7, maxLosses: 3 } : { maxWins: 1, maxLosses: 1 }
  )),
}));

const formatGate = vi.hoisted(() => ({
  evaluate: vi.fn(async (_request: unknown) => ({ compatible: true, reasons: [] as string[] })),
}));

vi.mock("@wasm/draft", () => wasm);
vi.mock("../../services/quickDraftPersistence", () => persistence);
vi.mock("../../adapter/wasm-adapter", async (importOriginal) => ({
  ...await importOriginal<typeof import("../../adapter/wasm-adapter")>(),
  getSharedAdapter: () => ({ evaluateDeckFormatGate: formatGate.evaluate }),
}));

import {
  useDraftStore,
  type DraftPickOutcome,
} from "../draftStore";

function card(instanceId: string, name = instanceId): DraftCardInstance {
  return {
    instance_id: instanceId,
    name,
    set_code: "TST",
    collector_number: instanceId,
    rarity: "common",
    colors: [],
    cmc: 1,
    type_line: "Card",
  };
}

/** A card whose mana value decides its column under the default `cmc` board sort.
 *  `manaValueColumn` truncates and clamps to `columnCount - 1`, and the deck board
 *  defaults to 7 columns against the sideboard's 6 — so cmc 6 is the value that
 *  distinguishes "sorted into the deck's last column" from "clamped into the
 *  sideboard's last column", which is the confusion these tests exist to pin. */
function cardWithCmc(instanceId: string, cmc: number): DraftCardInstance {
  return { ...card(instanceId), cmc };
}

function botSeat(seat_index: number): DraftPlayerView["seats"][number] {
  return {
    seat_index, display_name: `Bot ${seat_index}`, is_bot: true,
    connected: true, has_submitted_deck: true, pick_status: "NotDrafting",
    active_pack_count: 0, drafted_card_count: 0, face_up_draft_cards: [],
  };
}

function view(pool: DraftCardInstance[] = []): DraftPlayerView {
  return {
    status: "Drafting",
    kind: "Quick",
    launch_capability: "None",
    distribution: "PickAndPass",
    commanders_required: 0,
    pool,
    current_pack: [],
    draft_effects: [],
    pool_groups: {
      color_groups: [], type_groups: [], cmc_groups: [], rarity_groups: [],
      type_filter_options: [], color_filter_options: [],
      color_counts: { white: 0, blue: 0, black: 0, red: 0, green: 0 },
      workspace_capabilities: { rarity_group_order: null },
      workspace_row_classification: { creature_instance_ids: [], noncreature_instance_ids: [] },
    },
    seats: [botSeat(1)],
    current_pack_number: 1,
    pick_number: 1,
    pass_direction: "Left",
    cards_per_pack: 14,
    required_pick_count: 0,
    pick_selection_mode: "Direct",
    pick_steps_per_pack: 14,
    pack_count: 3,
    min_deck_size: 40,
    addable_cards: [],
    timer_remaining_ms: null,
    standings: [],
    current_round: 0,
    next_pairing_round: 1,
    tournament_format: "Swiss",
    pod_policy: "Casual",
    pairings: [],
    match_config: { match_type: "Bo1" },
  } as DraftPlayerView;
}

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason: unknown) => void;
  const promise = new Promise<T>((resolvePromise, rejectPromise) => {
    resolve = resolvePromise;
    reject = rejectPromise;
  });
  return { promise, resolve, reject };
}

async function start(pool: DraftCardInstance[] = []): Promise<void> {
  wasm.start_quick_draft.mockReturnValue(view(pool));
  await useDraftStore.getState().startDraft("pool", "TST", "Test", 2);
}

async function startLaunchWithBotSeats(indices: number[]): Promise<void> {
  await start([card("human", "Forest")]);
  useDraftStore.setState((state) => ({
    phase: "launching",
    view: { ...state.view!, seats: indices.map(botSeat) },
  }));
  persistence.loadDraftRun.mockResolvedValue(null);
}

async function settleTimers(): Promise<void> {
  await vi.runAllTimersAsync();
  await Promise.resolve();
}

describe("draft store workspace authority", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    vi.clearAllMocks();
    formatGate.evaluate.mockResolvedValue({ compatible: true, reasons: [] });
    persistence.inspectActiveQuickDraftLifecycle.mockResolvedValue(null);
    // Module state, so it survives `reset()` and would otherwise leak the
    // geometry one test publishes into the next.
    setArrivingCardBoardPreferences(createDefaultDraftWorkspacePreferences().deck);
    useDraftStore.getState().reset();
    // `reset()` deliberately keeps the player's chosen bot difficulty, so pin
    // it here to keep tests independent of order.
    useDraftStore.getState().setDifficulty(2);
  });

  afterEach(() => {
    vi.useRealTimers();
    vi.restoreAllMocks();
    vi.unstubAllGlobals();
  });

  it("keeps_the_chosen_bot_difficulty_through_start_and_reset", async () => {
    // The setup screen stays mounted while a start loads, so the lifecycle
    // reset must not snap the selector back to the Medium default.
    useDraftStore.getState().setDifficulty(4);
    wasm.start_quick_draft.mockReturnValue(view([]));
    wasm.load_card_database.mockReturnValue(0);
    vi.stubGlobal("fetch", vi.fn(async () => new Response("{}")));

    const starting = useDraftStore.getState().startDraft("pool", "TST", "Test", 4);
    expect(useDraftStore.getState().difficulty).toBe(4);
    await starting;
    expect(useDraftStore.getState().difficulty).toBe(4);
    expect(wasm.start_quick_draft).toHaveBeenCalledWith("pool", 4, expect.any(Number));

    useDraftStore.getState().reset();
    expect(useDraftStore.getState().difficulty).toBe(4);
  });

  it("applies_pending_destination_only_after_pool_acknowledgement", async () => {
    await start();
    const result = deferred<DraftPlayerView>();
    wasm.submit_pick.mockReturnValue(result.promise);

    const pick = useDraftStore.getState().pickCard("picked", "sideboard");
    expect(useDraftStore.getState().pendingPickIntent).toEqual({
      kind: "pick", instanceIds: ["picked"], destination: "sideboard",
    });
    expect(useDraftStore.getState().workspaceState?.placements.picked).toBeUndefined();

    result.resolve(view([card("picked")]));
    await pick;
    expect(useDraftStore.getState().workspaceState?.placements.picked.zone).toBe("sideboard");
    expect(useDraftStore.getState().pendingPickIntent).toBeNull();
    expect(projectWorkspaceMainDeck(
      useDraftStore.getState().workspaceState!,
      useDraftStore.getState().view!.pool,
    )).toEqual([]);
  });

  it("sorts_a_pool_arriving_on_a_state_install_into_the_boards_columns", async () => {
    // `startDraft` installs the whole pool through a `kind: "state"` operation,
    // which resolves no placement hint for any card — the path a restored Sealed
    // pool and a resumed Quick draft both take. Before `placeArrivingPoolCards`
    // was wired into this store every one of these landed in column 0.
    await start([cardWithCmc("cheap", 1), cardWithCmc("costly", 6)]);

    const placements = useDraftStore.getState().workspaceState!.placements;
    expect(placements.cheap.column).toBe(1);
    expect(placements.costly.column).toBe(6);
  });

  it("sorts_a_hint_less_deck_pick_into_the_column_its_mana_value_means", async () => {
    // `PackDisplay.request` dispatches `pickCard` with no placement hint, so
    // `applyDestination` has only reconcile's column-0 default to fall back on.
    await start();
    wasm.submit_pick.mockReturnValue(view([cardWithCmc("picked", 6)]));

    await useDraftStore.getState().pickCard("picked", "deck");

    expect(useDraftStore.getState().workspaceState!.placements.picked.column).toBe(6);
  });

  it("keeps_the_hint_column_on_a_deck_pick_that_resolved_one", async () => {
    // The arriving pass must not overrule a column someone chose — this is the
    // drag-and-drop target as well as `DraftPage.handleConfirmPick`'s resolution.
    await start();
    wasm.submit_pick.mockReturnValue(view([cardWithCmc("picked", 6)]));

    await useDraftStore.getState().pickCard("picked", "deck", { column: 2 });

    expect(useDraftStore.getState().workspaceState!.placements.picked.column).toBe(2);
  });

  it("places_a_pick_against_the_columns_the_page_published_not_the_module_default", async () => {
    // The seam the whole feature rests on: `installWorkspace` reads the board
    // geometry through `getArrivingCardBoardPreferences`, so a page that has
    // published three columns must get three-column placement. Three is chosen
    // because `manaValueColumn` clamps to `columnCount - 1`: a six-drop lands in
    // column 2 here and column 6 under the seven-column default, so no default
    // can produce this result.
    setArrivingCardBoardPreferences({
      sort: "cmc", columnCount: 3, rows: "one", showHeaders: true,
    });
    await start();
    wasm.submit_pick.mockReturnValue(view([cardWithCmc("picked", 6)]));

    await useDraftStore.getState().pickCard("picked", "deck");

    expect(useDraftStore.getState().workspaceState!.placements.picked.column).toBe(2);
  });

  it("places_an_install_before_any_publish_against_the_stored_preferences", async () => {
    // The OTHER end of that seam: `arrivingCardBoardPreferences` is seeded at
    // module init from `loadDraftWorkspacePreferences()`, and this install
    // reaches it with no `setArrivingCardBoardPreferences` call on the module
    // instance it reads — the `beforeEach` one lands on this file's static
    // instance, not the one imported below. Three stored columns against the
    // seven-column module default: `manaValueColumn` clamps a six-drop to 2
    // here and to 6 under the defaults, so only the stored value produces this,
    // and replacing that initializer with `{ ...DECK_DEFAULTS }` reds this test
    // with `expected 6 to be 2`.
    localStorage.setItem(DRAFT_WORKSPACE_PREFERENCES_KEY, JSON.stringify({
      ...createDefaultDraftWorkspacePreferences(),
      deck: { sort: "cmc", columnCount: 3, rows: "one", showHeaders: true },
    }));
    // A fresh module registry: the initializer runs once per module instance,
    // and the instance this file imported statically is not it — drop
    // `vi.resetModules()` and this install places against the seven-column
    // default instead, `expected 6 to be 2`. `vi.mock` registrations survive
    // `resetModules`: the pool asserted on below is the one
    // `wasm.start_quick_draft` returns, so the re-imported store is still
    // running against the mocked engine.
    vi.resetModules();
    const { useDraftStore: freshStore } = await import("../draftStore");
    wasm.start_quick_draft.mockReturnValue(view([cardWithCmc("costly", 6)]));

    await freshStore.getState().startDraft("pool", "TST", "Test", 2);

    expect(freshStore.getState().workspaceState!.placements.costly.column).toBe(2);
    localStorage.removeItem(DRAFT_WORKSPACE_PREFERENCES_KEY);
  });

  it("leaves_a_hint_less_sideboard_pick_in_the_first_column", async () => {
    // The arriving pass is deck-only. Were it allowed to run for a sideboard
    // pick it would stamp this card with a column from the DECK's geometry —
    // 6, asserted above as the stored value on the deck-pick case — which the
    // sideboard's narrower six columns cannot hold, so
    // `normalizeWorkspaceForBoardGeometry` would clamp it at render to 5, the
    // sideboard's last column, rather than leaving it in the first. A cheaper
    // card would land mid-board instead; the clamp is the overflow case, not
    // the general one.
    await start();
    wasm.submit_pick.mockReturnValue(view([cardWithCmc("picked", 6)]));

    await useDraftStore.getState().pickCard("picked", "sideboard");

    const placement = useDraftStore.getState().workspaceState!.placements.picked;
    expect(placement.zone).toBe("sideboard");
    expect(placement.column).toBe(0);
  });

  it("leaves_a_row_less_hint_on_a_two_row_board_in_the_reconcile_default_row", async () => {
    // `applyDestination` falls back per FIELD, so a hint naming only a column
    // leaves `row` to whatever placement is in the workspace. A drag that hits a
    // column but no row band sends exactly that shape. The arriving pass must
    // therefore skip hinted ids: were it to run, it would resolve this card's
    // row through the engine classification and change where a drag lands on a
    // two-row board, which is not this change's business.
    setArrivingCardBoardPreferences({
      sort: "cmc", columnCount: 7, rows: "two", showHeaders: true,
    });
    await start();
    wasm.submit_pick.mockReturnValue(view([cardWithCmc("picked", 6)]));

    await useDraftStore.getState().pickCard("picked", "deck", { column: 2 });

    const placement = useDraftStore.getState().workspaceState!.placements.picked;
    expect(placement.column).toBe(2);
    expect(placement.row).toBe(0);
  });

  it("has_exactly_one_reconciliation_call_inside_install_workspace", () => {
    const sourcePath = resolve(
      process.cwd(),
      `.${new URL("../draftStore.ts", import.meta.url).pathname}`,
    );
    const source = ts.createSourceFile(
      sourcePath,
      readFileSync(sourcePath, "utf8"),
      ts.ScriptTarget.Latest,
      true,
      ts.ScriptKind.TS,
    );
    const enclosingFunctions: string[] = [];
    const visit = (node: ts.Node, namedFunction: string | null): void => {
      let nextFunction = namedFunction;
      if (ts.isFunctionDeclaration(node) && node.name) nextFunction = node.name.text;
      if (ts.isCallExpression(node)
        && ts.isIdentifier(node.expression)
        && node.expression.text === "reconcileWorkspaceState") {
        enclosingFunctions.push(nextFunction ?? "");
      }
      ts.forEachChild(node, (child) => visit(child, nextFunction));
    };
    visit(source, null);
    expect(enclosingFunctions).toEqual(["installWorkspace"]);
  });

  it("set_workspace_state_reconciles_projects_notifies_once_and_persists_once", async () => {
    await start([card("one", "Shared"), card("two", "Shared")]);
    await settleTimers();
    vi.clearAllMocks();
    const current = useDraftStore.getState().workspaceState!;
    const next = {
      ...current,
      placements: {
        one: { zone: "sideboard" as const, row: 0, column: 3, order: 4 },
        stale: { zone: "deck" as const, row: 0, column: 0, order: 0 },
      },
    };
    const observations: unknown[] = [];
    const unsubscribe = useDraftStore.subscribe((state) => observations.push(state.workspaceState));

    useDraftStore.getState().setWorkspaceState(next);
    unsubscribe();

    const state = useDraftStore.getState();
    expect(observations).toHaveLength(1);
    expect(state.workspaceState?.placements).not.toHaveProperty("stale");
    expect(state.workspaceState?.placements.one).toMatchObject({ zone: "sideboard", column: 3 });
    expect(state.workspaceState?.placements).toHaveProperty("two");
    expect(projectWorkspaceMainDeck(state.workspaceState!, state.view!.pool)).toEqual(["Shared"]);
    expect(vi.getTimerCount()).toBe(1);
    await settleTimers();
    expect(persistence.persistQuickDraftSnapshot).toHaveBeenCalledOnce();
  });

  it("returns_total_outcomes_with_validation_before_contention", async () => {
    await start();
    const result = deferred<DraftPlayerView>();
    wasm.submit_pick.mockReturnValue(result.promise);
    const admitted = useDraftStore.getState().pickCard("owner");
    const effectPick = useDraftStore.getState().pickCardWithDraftEffect as unknown as (
      authority: unknown,
      ids: unknown,
      destination?: unknown,
      hint?: unknown,
    ) => Promise<DraftPickOutcome>;

    await expect(effectPick("effect", ["one"], "deck"))
      .resolves.toEqual({ status: "rejected", reason: "invalid-request" });
    await expect(effectPick("effect", ["one", "two"], "deck"))
      .resolves.toEqual({ status: "ignored", reason: "busy" });
    await expect(effectPick("effect", ["one", "one"], "deck"))
      .resolves.toEqual({ status: "rejected", reason: "invalid-request" });
    await expect(effectPick("effect", ["effect", "two"], "deck"))
      .resolves.toEqual({ status: "rejected", reason: "invalid-request" });
    await expect(effectPick("effect", null, "deck"))
      .resolves.toEqual({ status: "rejected", reason: "invalid-request" });
    await expect(effectPick("effect", ["one", "two", "three"], "deck"))
      .resolves.toEqual({ status: "rejected", reason: "invalid-request" });
    await expect(effectPick("effect", ["", "two"], "deck"))
      .resolves.toEqual({ status: "rejected", reason: "invalid-request" });
    await expect(effectPick("effect", ["one", "two"], "invalid"))
      .resolves.toEqual({ status: "rejected", reason: "invalid-request" });
    await expect(effectPick("effect", ["one", "two"], "deck", { column: Number.NaN }))
      .resolves.toEqual({ status: "rejected", reason: "invalid-request" });
    await expect(effectPick("effect", ["one", "two"], "deck", { column: Number.POSITIVE_INFINITY }))
      .resolves.toEqual({ status: "rejected", reason: "invalid-request" });
    await expect(effectPick("effect", ["one", "two"], "deck", { column: 1.5 }))
      .resolves.toEqual({ status: "rejected", reason: "invalid-request" });
    await expect(effectPick("effect", ["one", "two"], "deck", { column: -1 }))
      .resolves.toEqual({ status: "rejected", reason: "invalid-request" });
    expect(wasm.submit_pick_with_draft_effect).not.toHaveBeenCalled();

    result.resolve(view([card("owner")]));
    await admitted;
  });

  it("rejects_unavailable_store_prerequisites_as_invalid_request", async () => {
    const actions = useDraftStore.getState();
    await expect(actions.pickCard("one"))
      .resolves.toEqual({ status: "rejected", reason: "invalid-request" });
    await expect(actions.confirmPick())
      .resolves.toEqual({ status: "rejected", reason: "invalid-request" });
    await expect(actions.pickCardWithDraftEffect("effect", ["one", "two"]))
      .resolves.toEqual({ status: "rejected", reason: "invalid-request" });
    await expect(actions.autoPickCard())
      .resolves.toEqual({ status: "rejected", reason: "invalid-request" });
    expect(wasm.submit_pick).not.toHaveBeenCalled();
    expect(wasm.submit_pick_with_draft_effect).not.toHaveBeenCalled();
    expect(wasm.auto_pick).not.toHaveBeenCalled();
  });

  it.each(["adapter", "draftId", "view", "workspaceState"] as const)(
    "rejects a missing %s prerequisite without invoking the adapter",
    async (field) => {
      await start([card("existing")]);
      useDraftStore.setState({ [field]: null });
      vi.clearAllMocks();
      await expect(useDraftStore.getState().pickCard("requested"))
        .resolves.toEqual({ status: "rejected", reason: "invalid-request" });
      expect(wasm.submit_pick).not.toHaveBeenCalled();
    },
  );

  it("acknowledges_ordinary_zero_to_one_and_allows_unrelated_additions", async () => {
    await start();
    wasm.submit_pick.mockReturnValue(view([card("requested"), card("unrelated")]));
    await expect(useDraftStore.getState().pickCard("requested", "sideboard", { column: 5, row: 1 }))
      .resolves.toEqual({ status: "acknowledged" });
    expect(useDraftStore.getState().workspaceState?.placements.requested)
      .toMatchObject({ zone: "sideboard", column: 5, row: 1 });
    expect(useDraftStore.getState().workspaceState?.placements.unrelated.zone).toBe("deck");
  });

  it("appends_an_acknowledged_single_pick_to_its_resolved_target_stack", async () => {
    await start([card("target")]);
    useDraftStore.getState().setWorkspacePlacement("target", {
      zone: "deck", row: 0, column: 5, order: 0,
    });
    wasm.submit_pick.mockReturnValue(view([card("target"), card("picked")]));

    await expect(useDraftStore.getState().pickCard("picked", "deck", { column: 5, row: 0 }))
      .resolves.toEqual({ status: "acknowledged" });

    const placements = useDraftStore.getState().workspaceState!.placements;
    expect(placements.target).toEqual({ zone: "deck", column: 5, row: 0, order: 0 });
    expect(placements.picked).toEqual({ zone: "deck", column: 5, row: 0, order: 1 });
  });

  it("acknowledges_a_selected_card_through_confirm_pick", async () => {
    await start();
    useDraftStore.getState().selectCard("selected");
    wasm.submit_pick.mockReturnValue(view([card("selected")]));
    await expect(useDraftStore.getState().confirmPick("sideboard", { column: 2 }))
      .resolves.toEqual({ status: "acknowledged" });
    expect(useDraftStore.getState().workspaceState?.placements.selected)
      .toMatchObject({ zone: "sideboard", column: 2 });
    expect(useDraftStore.getState().selectedCard).toBeNull();
  });

  it("ignores_selection_replacement_while_pick_interaction_is_locked", () => {
    useDraftStore.setState({ selectedCard: "prior", pickInteractionLocked: true });

    useDraftStore.getState().selectCard("replacement");
    expect(useDraftStore.getState().selectedCard).toBe("prior");

    useDraftStore.setState({ pickInteractionLocked: false });
    useDraftStore.getState().selectCard("replacement");
    expect(useDraftStore.getState().selectedCard).toBe("replacement");
  });

  it.each([
    ["unchanged", [], []],
    ["pre-existing", [card("requested")], [card("requested")]],
    ["duplicate", [], [card("requested"), card("requested")]],
  ])("rejects ordinary %s acknowledgment", async (_label, before, after) => {
    await start(before);
    const original = useDraftStore.getState().workspaceState;
    wasm.submit_pick.mockReturnValue(view(after));
    await expect(useDraftStore.getState().pickCard("requested"))
      .resolves.toEqual({ status: "rejected", reason: "unacknowledged" });
    expect(useDraftStore.getState().workspaceState).toBe(original);
    expect(useDraftStore.getState()).toMatchObject({
      pendingPickIntent: null,
      pickInteractionLocked: false,
    });
  });

  it("passes_distinct_mutable_two_element_copy", async () => {
    await start([card("effect")]);
    const tuple = ["first", "second"] as const;
    const leaseSpy = vi.spyOn(DraftEngineOperationLease.prototype, "submitPickWithDraftEffect");
    wasm.submit_pick_with_draft_effect.mockReturnValue(view([
      card("effect"), card("first"), card("second"),
    ]));

    await expect(useDraftStore.getState().pickCardWithDraftEffect("effect", tuple))
      .resolves.toEqual({ status: "acknowledged" });

    const adapterIds = leaseSpy.mock.calls[0]?.[1];
    expect(adapterIds).toEqual(tuple);
    expect(adapterIds).not.toBe(tuple);
  });

  it("appends_a_hint_less_deck_draft_effect_pick_in_request_order", async () => {
    // `appends_acknowledged_draft_effect_cards_in_request_order`, below, sends
    // `"sideboard"` with a `{ column: 4 }` hint, so `operationResolvesOwnPlacement`
    // excludes both ids from the arriving pass and that test stays green whichever
    // side of the switch the pass runs on. THIS case — deck, no hint — is what
    // `PackDisplay.request` dispatches, and it is the one that makes the pass's
    // position relative to the switch observable: run the pass after
    // `applyDestination` instead and these two land in POOL order (`first` then
    // `second`) rather than the requested order.
    await start([card("effect")]);
    wasm.submit_pick_with_draft_effect.mockReturnValue(view([
      { ...cardWithCmc("effect", 3) },
      { ...cardWithCmc("first", 3) },
      { ...cardWithCmc("second", 3) },
    ]));

    await expect(useDraftStore.getState().pickCardWithDraftEffect(
      "effect", ["second", "first"], "deck",
    )).resolves.toEqual({ status: "acknowledged" });

    const placements = useDraftStore.getState().workspaceState!.placements;
    expect(placements.second.order).toBe(0);
    expect(placements.first.order).toBe(1);
  });

  it("appends_acknowledged_draft_effect_cards_in_request_order", async () => {
    await start([card("effect")]);
    wasm.submit_pick_with_draft_effect.mockReturnValue(view([
      card("effect"), card("first"), card("second"),
    ]));
    await expect(useDraftStore.getState().pickCardWithDraftEffect(
      "effect", ["first", "second"], "sideboard", { column: 4 },
    )).resolves.toEqual({ status: "acknowledged" });
    const placements = useDraftStore.getState().workspaceState!.placements;
    expect(placements.effect.zone).toBe("deck");
    expect(placements.first).toEqual({ zone: "sideboard", column: 4, row: 0, order: 0 });
    expect(placements.second).toEqual({ zone: "sideboard", column: 4, row: 0, order: 1 });
  });

  it.each([
    ["partial", [card("effect"), card("first")]],
    ["unchanged", [card("effect")]],
    ["duplicate", [card("effect"), card("first"), card("first"), card("second")]],
  ])("rejects effect %s acknowledgment atomically", async (_label, returnedPool) => {
    await start([card("effect")]);
    const original = useDraftStore.getState().workspaceState;
    wasm.submit_pick_with_draft_effect.mockReturnValue(view(returnedPool));
    await expect(useDraftStore.getState().pickCardWithDraftEffect("effect", ["first", "second"]))
      .resolves.toEqual({ status: "rejected", reason: "unacknowledged" });
    expect(useDraftStore.getState().workspaceState).toBe(original);
  });

  it("sorts_an_auto_picked_card_that_carries_no_hint_of_its_own", async () => {
    // The other direction of the `acknowledged-auto-pick` arm, and the solo twin
    // of the pod store's `sorts an auto-picked card that carries no hint of its
    // own`. `performPick` builds this card's hint as
    // `request.placementHints?.[addedInstanceId]`, so an id the page's
    // pack-keyed map does not cover — or any bare `autoPickCard("deck")` —
    // arrives with no decision of its own and must be sorted, not parked in
    // column 0.
    await start();
    wasm.auto_pick.mockReturnValue(view([cardWithCmc("added", 6)]));

    await expect(useDraftStore.getState().autoPickCard("deck"))
      .resolves.toEqual({ status: "acknowledged" });

    expect(useDraftStore.getState().workspaceState!.placements.added.column).toBe(6);
  });

  it("leaves_a_row_less_auto_pick_hint_on_a_two_row_board_in_the_reconcile_default_row", async () => {
    // The `acknowledged-auto-pick` arm of the same exclusion. `validPlacementHint`
    // admits a hint with no `row`, and `DraftPage.handleAutoPick` builds its hints
    // from `resolveWorkspacePickPlacement`, which omits `row` on a one-row board —
    // a persisted or restored intent can therefore carry that shape into a
    // two-row board. Without the arm the arriving pass decides the row instead.
    setArrivingCardBoardPreferences({
      sort: "cmc", columnCount: 7, rows: "two", showHeaders: true,
    });
    await start();
    wasm.auto_pick.mockReturnValue(view([
      { ...cardWithCmc("added", 6), type_line: "Creature — Bear" },
    ]));

    await expect(useDraftStore.getState().autoPickCard("deck", { added: { column: 2 } }))
      .resolves.toEqual({ status: "acknowledged" });

    const placement = useDraftStore.getState().workspaceState!.placements.added;
    expect(placement.column).toBe(2);
    expect(placement.row).toBe(0);
  });

  it("appends_the_acknowledged_auto_pick_to_its_resolved_target_stack", async () => {
    await start([card("existing"), card("target")]);
    useDraftStore.getState().setWorkspacePlacement("existing", {
      zone: "sideboard", row: 0, column: 2, order: 0,
    });
    useDraftStore.getState().setWorkspacePlacement("target", {
      zone: "deck", row: 1, column: 4, order: 0,
    });
    wasm.auto_pick.mockReturnValue(view([card("existing"), card("target"), card("added")]));
    await expect(useDraftStore.getState().autoPickCard("deck", {
      added: { column: 4, row: 1 },
    }))
      .resolves.toEqual({ status: "acknowledged" });
    expect(useDraftStore.getState().workspaceState?.placements.existing.zone).toBe("sideboard");
    expect(useDraftStore.getState().workspaceState?.placements.target)
      .toEqual({ zone: "deck", column: 4, row: 1, order: 0 });
    expect(useDraftStore.getState().workspaceState?.placements.added)
      .toEqual({ zone: "deck", column: 4, row: 1, order: 1 });
  });

  it.each([
    ["unchanged", [card("existing")]],
    ["duplicate growth", [card("existing"), card("existing")]],
    ["removal", []],
    ["substitution", [card("replacement")]],
    ["multiple additions", [card("existing"), card("one"), card("two")]],
  ])("rejects auto-pick %s acknowledgment", async (_label, returnedPool) => {
    await start([card("existing")]);
    const original = useDraftStore.getState().workspaceState;
    wasm.auto_pick.mockReturnValue(view(returnedPool));
    await expect(useDraftStore.getState().autoPickCard())
      .resolves.toEqual({ status: "rejected", reason: "unacknowledged" });
    expect(useDraftStore.getState().workspaceState).toBe(original);
  });

  it("blocks_all_workspace_mutations_when_each_authority_signal_is_independently_set", async () => {
    const attemptAll = async (): Promise<void> => {
      const state = useDraftStore.getState();
      useDraftStore.getState().setWorkspaceState({
        ...state.workspaceState!,
        placements: {},
      });
      useDraftStore.getState().setWorkspacePlacement("one", {
        zone: "sideboard", row: 0, column: 1, order: 0,
      });
      useDraftStore.getState().addBasicLand("Island");
      useDraftStore.getState().removeBasicLand("Island");
      await useDraftStore.getState().autoSuggestDeck();
      await useDraftStore.getState().autoSuggestLands();
    };

    await start([card("one", "Shared"), card("two", "Shared")]);
    await settleTimers();
    vi.clearAllMocks();
    const baseline = useDraftStore.getState().workspaceState;

    useDraftStore.setState({ pickInteractionLocked: true });
    let notifications = 0;
    let unsubscribe = useDraftStore.subscribe(() => { notifications += 1; });
    await attemptAll();
    unsubscribe();
    expect(notifications).toBe(0);
    expect(useDraftStore.getState().workspaceState).toBe(baseline);

    useDraftStore.getState().reset();
    await start([card("one", "Shared"), card("two", "Shared")]);
    const intentBaseline = useDraftStore.getState().workspaceState;
    useDraftStore.setState({
      pendingPickIntent: { kind: "pick", instanceIds: ["held"], destination: "deck" },
    });
    notifications = 0;
    unsubscribe = useDraftStore.subscribe(() => { notifications += 1; });
    await attemptAll();
    unsubscribe();
    expect(notifications).toBe(0);
    expect(useDraftStore.getState().workspaceState).toBe(intentBaseline);

    useDraftStore.getState().reset();
    await start([card("one", "Shared"), card("two", "Shared")]);
    const tokenBaseline = useDraftStore.getState().workspaceState;
    const submitResult = deferred<DraftPlayerView>();
    wasm.submit_deck.mockReturnValue(submitResult.promise);
    const submit = useDraftStore.getState().submitDeck();
    await Promise.resolve();
    notifications = 0;
    unsubscribe = useDraftStore.subscribe(() => { notifications += 1; });
    await attemptAll();
    unsubscribe();
    expect(notifications).toBe(0);
    expect(useDraftStore.getState().workspaceState).toBe(tokenBaseline);
    expect(wasm.suggest_deck).not.toHaveBeenCalled();
    expect(wasm.suggest_lands).not.toHaveBeenCalled();
    submitResult.resolve(view([card("one", "Shared"), card("two", "Shared")]));
    await submit;

    wasm.suggest_deck.mockReturnValue({ main_deck: ["Shared"], lands: {} });
    wasm.suggest_lands.mockReturnValue({ Island: 1 });
    useDraftStore.getState().setWorkspaceState(useDraftStore.getState().workspaceState!);
    useDraftStore.getState().setWorkspacePlacement("one", {
      zone: "sideboard", row: 0, column: 1, order: 0,
    });
    useDraftStore.getState().addBasicLand("Island");
    useDraftStore.getState().removeBasicLand("Island");
    await useDraftStore.getState().autoSuggestDeck();
    await useDraftStore.getState().autoSuggestLands();
    expect(wasm.suggest_deck).toHaveBeenCalledOnce();
    expect(wasm.suggest_lands).toHaveBeenCalledOnce();
  });

  it.each([
    ["deck fulfillment", "deck", false],
    ["deck rejection", "deck", true],
    ["land fulfillment", "lands", false],
    ["land rejection", "lands", true],
  ] as const)("invalidated deferred %s settles inert after pick admission", async (
    _label,
    family,
    rejectSuggestion,
  ) => {
    await start([card("existing")]);
    await settleTimers();
    vi.clearAllMocks();
    const suggestionResult = deferred<unknown>();
    if (family === "deck") wasm.suggest_deck.mockReturnValue(suggestionResult.promise);
    else wasm.suggest_lands.mockReturnValue(suggestionResult.promise);
    const suggestion = family === "deck"
      ? useDraftStore.getState().autoSuggestDeck()
      : useDraftStore.getState().autoSuggestLands();
    const suggestionMock = family === "deck" ? wasm.suggest_deck : wasm.suggest_lands;
    await vi.waitFor(() => expect(suggestionMock).toHaveBeenCalledOnce());
    const pickResult = deferred<DraftPlayerView>();
    wasm.submit_pick.mockReturnValue(pickResult.promise);
    const pick = useDraftStore.getState().pickCard("picked");
    const before = useDraftStore.getState().workspaceState;

    if (rejectSuggestion) suggestionResult.reject(new Error("late suggestion"));
    else suggestionResult.resolve(family === "deck" ? { main_deck: [], lands: {} } : {});
    await expect(suggestion).resolves.toBeUndefined();
    expect(useDraftStore.getState().workspaceState).toBe(before);

    pickResult.resolve(view([card("existing"), card("picked")]));
    await expect(pick).resolves.toEqual({ status: "acknowledged" });
  });

  it.each([
    ["deck fulfillment", "deck", false],
    ["deck rejection", "deck", true],
    ["land fulfillment", "lands", false],
    ["land rejection", "lands", true],
  ] as const)("invalidated deferred %s settles inert after lifecycle replacement", async (
    _label,
    family,
    rejectSuggestion,
  ) => {
    await start([card("existing")]);
    const suggestionResult = deferred<unknown>();
    if (family === "deck") wasm.suggest_deck.mockReturnValue(suggestionResult.promise);
    else wasm.suggest_lands.mockReturnValue(suggestionResult.promise);
    const suggestion = family === "deck"
      ? useDraftStore.getState().autoSuggestDeck()
      : useDraftStore.getState().autoSuggestLands();
    const suggestionMock = family === "deck" ? wasm.suggest_deck : wasm.suggest_lands;
    await vi.waitFor(() => expect(suggestionMock).toHaveBeenCalledOnce());
    useDraftStore.getState().reset();
    const replacement = useDraftStore.getState();

    if (rejectSuggestion) suggestionResult.reject(new Error("late suggestion"));
    else suggestionResult.resolve(family === "deck" ? { main_deck: [], lands: {} } : {});
    await expect(suggestion).resolves.toBeUndefined();
    expect(useDraftStore.getState()).toMatchObject({
      interactionGeneration: replacement.interactionGeneration,
      workspaceState: null,
    });
  });

  it("preserves_fresh_suggestion_failures", async () => {
    await start([card("existing")]);
    wasm.suggest_deck.mockRejectedValue(new Error("live suggestion"));
    await expect(useDraftStore.getState().autoSuggestDeck()).rejects.toThrow("live suggestion");
  });

  it("recovers a submitted Cube run without its separate session and launches its exact stage at Hard", async () => {
    const run: DraftRunState = {
      format: "run", results: [], playerDeck: ["Player"], opponentDeck: ["Opponent"],
      usedBotSeats: [2], booster_pack_pool: [],
      activeMatch: { draftId: "durable", gameId: "same-game", format: "run",
        resultCountAtLaunch: 0, botSeat: 2, opponentDeck: ["Opponent"] },
    };
    const actualPersistence = await vi.importActual<typeof import("../../services/quickDraftPersistence")>(
      "../../services/quickDraftPersistence",
    );
    localStorage.setItem(ACTIVE_QUICK_DRAFT_KEY, JSON.stringify({
      id: "durable", setCode: "custom-cube", difficulty: 3, kind: "Quick",
      phase: "playing", pickCount: 40, updatedAt: Date.now(),
    }));
    persistence.inspectActiveQuickDraftLifecycle.mockImplementationOnce(() =>
      actualPersistence.inspectActiveQuickDraftLifecycle("inspect"));
    persistence.loadDraftRun.mockResolvedValue(run);
    persistence.loadQuickDraftSession.mockResolvedValue(null);
    const outcome = await useDraftStore.getState().resumeDraft();
    expect(outcome).toEqual({ status: "resumed", draftId: "durable" });
    expect(useDraftStore.getState()).toMatchObject({ draftId: "durable", phase: "playing",
      difficulty: 3, runFormat: "run", runState: run, adapter: null, view: null, workspaceState: null });
    expect(persistence.cleanupQuickDraftLifecycle).not.toHaveBeenCalled();

    const navigate = vi.fn();
    await useDraftStore.getState().launchNextMatch(navigate);
    expect(formatGate.evaluate).toHaveBeenCalledTimes(2);
    expect(formatGate.evaluate.mock.calls.map(([request]) => (request as { main_deck: string[] }).main_deck))
      .toEqual([["Player"], ["Opponent"]]);
    expect(persistence.publishStagedDraftMatch).toHaveBeenCalledWith(expect.objectContaining({
      draftId: "durable", gameId: "same-game", payload: expect.objectContaining({ booster_pack_pool: [] }),
    }));
    expect(navigate).toHaveBeenCalledWith(expect.stringContaining("difficulty=Hard"));
    expect(navigate).toHaveBeenCalledWith(expect.stringContaining("/game/same-game?"));
    localStorage.removeItem(ACTIVE_QUICK_DRAFT_KEY);
  });

  it("uses stored Bo3 format for run-only preflight and refuses unsupported empty sideboards", async () => {
    const run: DraftRunState = { format: "bo3", results: [], playerDeck: ["Player"],
      opponentDeck: ["Opponent"], usedBotSeats: [1], booster_pack_pool: [],
      activeMatch: { draftId: "bo3-run", gameId: "bo3-game", format: "bo3",
        resultCountAtLaunch: 0, botSeat: 1, opponentDeck: ["Opponent"] } };
    persistence.inspectActiveQuickDraftLifecycle.mockResolvedValue({
      id: "bo3-run", setCode: "custom-cube", difficulty: 2, kind: "Quick", phase: "playing",
    });
    persistence.loadDraftRun.mockResolvedValue(run);
    persistence.loadQuickDraftSession.mockResolvedValue(null);
    expect(await useDraftStore.getState().resumeDraft()).toEqual({ status: "resumed", draftId: "bo3-run" });
    formatGate.evaluate.mockImplementation(async (request: unknown) => ({
      compatible: (request as { selected_match_type: string }).selected_match_type !== "Bo3",
      reasons: ["BO3 requires a sideboard"],
    }));
    const navigate = vi.fn();
    await expect(useDraftStore.getState().launchNextMatch(navigate)).rejects.toThrow("BO3 requires a sideboard");
    expect(formatGate.evaluate).toHaveBeenCalledTimes(2);
    for (const [request] of formatGate.evaluate.mock.calls) {
      expect(request).toMatchObject({ selected_format: "Limited", selected_match_type: "Bo3", sideboard: [] });
    }
    expect(persistence.publishStagedDraftMatch).not.toHaveBeenCalled();
    expect(navigate).not.toHaveBeenCalled();
    expect(useDraftStore.getState()).toMatchObject({ phase: "playing", runState: run });
    expect(persistence.cleanupQuickDraftLifecycle).not.toHaveBeenCalled();
  });

  it.each([5, 2.5])("refuses persisted finite invalid difficulty %s while retaining the run ID", async (difficulty) => {
    const run: DraftRunState = { format: "run", results: [], playerDeck: ["Player"],
      opponentDeck: ["Opponent"], usedBotSeats: [1], booster_pack_pool: [] };
    const actualPersistence = await vi.importActual<typeof import("../../services/quickDraftPersistence")>(
      "../../services/quickDraftPersistence",
    );
    localStorage.setItem(ACTIVE_QUICK_DRAFT_KEY, JSON.stringify({
      id: "invalid-difficulty", setCode: "custom-cube", difficulty, kind: "Quick",
      phase: "playing", pickCount: 40, updatedAt: Date.now(),
    }));
    expect((await actualPersistence.inspectActiveQuickDraftLifecycle("inspect"))?.id).toBe("invalid-difficulty");
    persistence.inspectActiveQuickDraftLifecycle.mockImplementationOnce(() =>
      actualPersistence.inspectActiveQuickDraftLifecycle("inspect"));
    persistence.loadDraftRun.mockResolvedValue(run);
    persistence.loadQuickDraftSession.mockResolvedValue(null);
    const outcome = await useDraftStore.getState().resumeDraft();
    expect(outcome).toEqual({ status: "unavailable", draftId: "invalid-difficulty",
      reason: "Saved draft run is unavailable" });
    expect(useDraftStore.getState().draftId).toBe("invalid-difficulty");
    expect(persistence.cleanupQuickDraftLifecycle).not.toHaveBeenCalled();
    expect(formatGate.evaluate).not.toHaveBeenCalled();
    expect(persistence.publishStagedDraftMatch).not.toHaveBeenCalled();
    expect(localStorage.getItem(ACTIVE_QUICK_DRAFT_KEY)).not.toBeNull();
    localStorage.removeItem(ACTIVE_QUICK_DRAFT_KEY);
  });

  it("keeps an unreadable run available for Retry Resume and succeeds after the read recovers", async () => {
    persistence.inspectActiveQuickDraftLifecycle.mockResolvedValue({
      id: "read-retry", setCode: "TST", difficulty: 2, kind: "Quick", phase: "playing",
    });
    const run: DraftRunState = { format: "run", results: [], playerDeck: ["Player"],
      opponentDeck: ["Opponent"], usedBotSeats: [1] };
    persistence.loadDraftRun.mockRejectedValueOnce(new Error("IndexedDB read failed")).mockResolvedValue(run);
    persistence.loadQuickDraftSession.mockResolvedValue(null);
    expect(await useDraftStore.getState().resumeDraft()).toEqual({
      status: "unavailable", draftId: "read-retry", reason: "IndexedDB read failed",
    });
    expect(persistence.cleanupQuickDraftLifecycle).not.toHaveBeenCalled();
    expect(await useDraftStore.getState().resumeDraft()).toEqual({ status: "resumed", draftId: "read-retry" });
    expect(useDraftStore.getState().runState).toEqual(run);
    await useDraftStore.getState().launchNextMatch(vi.fn());
    expect(formatGate.evaluate.mock.calls.map(([request]) => (request as { draft_set_codes: string[] }).draft_set_codes))
      .toEqual([["TST"], ["TST"]]);
  });

  it.each(["session read", "adapter import"])("recovers a complete run when the separate %s fails", async (failure) => {
    const run: DraftRunState = { format: "single", results: [{ gameId: "finished", result: "win" }],
      playerDeck: ["Player"], opponentDeck: ["Opponent"], usedBotSeats: [1], booster_pack_pool: [] };
    persistence.inspectActiveQuickDraftLifecycle.mockResolvedValue({
      id: "complete-run", setCode: "custom-cube", difficulty: 2, kind: "Quick", phase: "complete",
    });
    persistence.loadDraftRun.mockResolvedValue(run);
    if (failure === "session read") {
      persistence.loadQuickDraftSession.mockRejectedValueOnce(new Error("session read failed"));
    } else {
      persistence.loadQuickDraftSession.mockResolvedValueOnce({ sessionJson: "broken session",
        mainDeck: [], landCounts: {}, poolSortMode: "color", poolPanelOpen: true, workspace: null });
      wasm.import_draft_session.mockImplementationOnce(() => { throw new Error("import failed"); });
    }
    expect(await useDraftStore.getState().resumeDraft()).toEqual({ status: "resumed", draftId: "complete-run" });
    expect(useDraftStore.getState()).toMatchObject({ phase: "complete", draftId: "complete-run",
      runState: run, runFormat: "single", adapter: null, view: null, workspaceState: null });
    expect(persistence.cleanupQuickDraftLifecycle).not.toHaveBeenCalled();
  });

  it("mints one run-only stage from the stored opponent and refuses a rejecting engine gate", async () => {
    const run: DraftRunState = { format: "run", results: [{ gameId: "prior", result: "draw" }],
      playerDeck: ["Player"], opponentDeck: ["Opponent"], usedBotSeats: [4], booster_pack_pool: [] };
    persistence.inspectActiveQuickDraftLifecycle.mockResolvedValue({
      id: "new-stage", setCode: "custom-cube", difficulty: 1, kind: "Quick", phase: "playing",
    });
    persistence.loadDraftRun.mockResolvedValue(run);
    persistence.loadQuickDraftSession.mockResolvedValue(null);
    expect((await useDraftStore.getState().resumeDraft()).status).toBe("resumed");
    formatGate.evaluate.mockImplementation(async (request: unknown) => ({
      compatible: !(request as { main_deck: string[] }).main_deck.includes("Opponent"),
      reasons: ["Opponent deck is not legal"],
    }));
    const navigate = vi.fn();
    await expect(useDraftStore.getState().launchNextMatch(navigate)).rejects.toThrow("Opponent deck is not legal");
    expect(formatGate.evaluate).toHaveBeenCalledTimes(2);
    expect(persistence.publishStagedDraftMatch).not.toHaveBeenCalled();
    expect(navigate).not.toHaveBeenCalled();
    expect(wasm.get_bot_deck).not.toHaveBeenCalled();
    expect(useDraftStore.getState().runState).toEqual(run);
    formatGate.evaluate.mockResolvedValue({ compatible: true, reasons: [] });
    await useDraftStore.getState().launchNextMatch(navigate);
    expect(persistence.publishStagedDraftMatch).toHaveBeenCalledWith(expect.objectContaining({
      run: expect.objectContaining({ opponentDeck: ["Opponent"],
        activeMatch: expect.objectContaining({ draftId: "new-stage", botSeat: 4, opponentDeck: ["Opponent"] }) }),
    }));
    expect(wasm.get_bot_deck).not.toHaveBeenCalled();
    expect(navigate).toHaveBeenCalledOnce();
  });

  it.each([null, false])("refuses persisted malformed activeMatch %s before run-only resume and launch", async (activeMatch) => {
    vi.useRealTimers();
    const draftId = `malformed-stage-${String(activeMatch)}`;
    const run: DraftRunState = { format: "run", results: [], playerDeck: ["Player"],
      opponentDeck: ["Opponent"], usedBotSeats: [1], booster_pack_pool: [] };
    const malformedRun = { ...run, activeMatch } as unknown as DraftRunState;
    const actualPersistence = await vi.importActual<typeof import("../../services/quickDraftPersistence")>(
      "../../services/quickDraftPersistence",
    );
    localStorage.setItem(ACTIVE_QUICK_DRAFT_KEY, JSON.stringify({
      id: draftId, setCode: "custom-cube", difficulty: 2, kind: "Quick",
      phase: "playing", pickCount: 40, updatedAt: Date.now(),
    }));
    await actualPersistence.saveDraftRun(draftId, malformedRun);
    persistence.inspectActiveQuickDraftLifecycle.mockImplementation(() =>
      actualPersistence.inspectActiveQuickDraftLifecycle("inspect"));
    persistence.loadDraftRun.mockImplementation(() => actualPersistence.loadDraftRun(draftId));
    persistence.loadQuickDraftSession.mockResolvedValue(null);

    expect(await useDraftStore.getState().resumeDraft()).toEqual({
      status: "unavailable", draftId, reason: "Saved draft run is unavailable",
    });
    expect(persistence.loadDraftRun).toHaveBeenCalledWith(draftId);
    expect(useDraftStore.getState()).toMatchObject({ draftId, runState: null });
    expect(await actualPersistence.loadDraftRun(draftId)).toEqual(malformedRun);
    expect(localStorage.getItem(ACTIVE_QUICK_DRAFT_KEY)).not.toBeNull();
    expect(persistence.cleanupQuickDraftLifecycle).not.toHaveBeenCalled();

    // An actually absent stage may resume and mint a new game from this run.
    await actualPersistence.saveDraftRun(draftId, run);
    expect(await useDraftStore.getState().resumeDraft()).toEqual({ status: "resumed", draftId });
    expect(useDraftStore.getState().runState).toEqual(run);
    await actualPersistence.saveDraftRun(draftId, malformedRun);
    const navigate = vi.fn();
    await expect(useDraftStore.getState().launchNextMatch(navigate)).rejects.toThrow("Saved draft run is unavailable");
    expect(await actualPersistence.loadDraftRun(draftId)).toEqual(malformedRun);
    expect(formatGate.evaluate).not.toHaveBeenCalled();
    expect(persistence.publishStagedDraftMatch).not.toHaveBeenCalled();
    expect(navigate).not.toHaveBeenCalled();
    expect(persistence.cleanupQuickDraftLifecycle).not.toHaveBeenCalled();

    await actualPersistence.saveDraftRun(draftId, run);
    await useDraftStore.getState().launchNextMatch(navigate);
    expect(formatGate.evaluate).toHaveBeenCalledTimes(2);
    expect(persistence.publishStagedDraftMatch).toHaveBeenCalledWith(expect.objectContaining({
      draftId, run: expect.objectContaining({ activeMatch: expect.objectContaining({ draftId }) }),
    }));
    expect(navigate).toHaveBeenCalledOnce();
    await actualPersistence.clearDraftRun(draftId);
    localStorage.removeItem(ACTIVE_QUICK_DRAFT_KEY);
  });

  it.each([null, false])("refuses persisted malformed activeMatch %s with a restored full session", async (activeMatch) => {
    vi.useRealTimers();
    const draftId = `full-malformed-stage-${String(activeMatch)}`;
    const run: DraftRunState = { format: "run", results: [], playerDeck: ["Player"],
      opponentDeck: ["Opponent"], usedBotSeats: [1] };
    const malformedRun = { ...run, activeMatch } as unknown as DraftRunState;
    const actualPersistence = await vi.importActual<typeof import("../../services/quickDraftPersistence")>(
      "../../services/quickDraftPersistence",
    );
    localStorage.setItem(ACTIVE_QUICK_DRAFT_KEY, JSON.stringify({
      id: draftId, setCode: "TST", difficulty: 2, kind: "Quick",
      phase: "playing", pickCount: 40, updatedAt: Date.now(),
    }));
    await actualPersistence.saveDraftRun(draftId, malformedRun);
    persistence.inspectActiveQuickDraftLifecycle.mockImplementation(() =>
      actualPersistence.inspectActiveQuickDraftLifecycle("inspect"));
    persistence.loadDraftRun.mockImplementation(() => actualPersistence.loadDraftRun(draftId));
    persistence.loadQuickDraftSession.mockResolvedValue({
      sessionJson: "saved session", mainDeck: ["Player"], landCounts: {},
      poolSortMode: "color", poolPanelOpen: true, workspace: null,
    });
    wasm.import_draft_session.mockReturnValue(view([card("player", "Player")]));

    expect(await useDraftStore.getState().resumeDraft()).toEqual({
      status: "unavailable", draftId, reason: "Saved draft run is unavailable",
    });
    expect(useDraftStore.getState()).toMatchObject({ draftId, runState: null, adapter: null });
    expect(await actualPersistence.loadDraftRun(draftId)).toEqual(malformedRun);
    expect(persistence.saveDraftRun).not.toHaveBeenCalled();
    expect(persistence.cleanupQuickDraftLifecycle).not.toHaveBeenCalled();

    // A genuinely absent stage in the same full-session path may launch.
    await actualPersistence.saveDraftRun(draftId, run);
    expect(await useDraftStore.getState().resumeDraft()).toEqual({ status: "resumed", draftId });
    expect(wasm.import_draft_session).toHaveBeenCalledWith("saved session", 2);
    expect(useDraftStore.getState()).toMatchObject({ draftId, runState: run, phase: "playing" });
    expect(useDraftStore.getState().adapter).not.toBeNull();
    expect(useDraftStore.getState().workspaceState).not.toBeNull();

    await actualPersistence.saveDraftRun(draftId, malformedRun);
    const navigate = vi.fn();
    await expect(useDraftStore.getState().launchNextMatch(navigate)).rejects.toThrow("Saved draft run is unavailable");
    expect(await actualPersistence.loadDraftRun(draftId)).toEqual(malformedRun);
    expect(wasm.get_bot_deck).not.toHaveBeenCalled();
    expect(formatGate.evaluate).not.toHaveBeenCalled();
    expect(persistence.publishStagedDraftMatch).not.toHaveBeenCalled();
    expect(navigate).not.toHaveBeenCalled();

    await actualPersistence.saveDraftRun(draftId, run);
    wasm.get_bot_deck.mockReturnValue({ main_deck: ["Opponent"], lands: {} });
    await useDraftStore.getState().launchNextMatch(navigate);
    expect(wasm.get_bot_deck).toHaveBeenCalled();
    expect(formatGate.evaluate).toHaveBeenCalledTimes(2);
    expect(persistence.publishStagedDraftMatch).toHaveBeenCalledWith(expect.objectContaining({
      draftId, run: expect.objectContaining({ activeMatch: expect.objectContaining({ draftId }) }),
    }));
    expect(navigate).toHaveBeenCalledOnce();
    await actualPersistence.clearDraftRun(draftId);
    localStorage.removeItem(ACTIVE_QUICK_DRAFT_KEY);
  });

  it("refuses an empty durable player deck in a full session and validates the published deck on retry", async () => {
    vi.useRealTimers();
    const draftId = "full-empty-player";
    const run: DraftRunState = { format: "run", results: [], playerDeck: ["Stored"],
      opponentDeck: ["Opponent"], usedBotSeats: [1] };
    const emptyRun = { ...run, playerDeck: [] };
    const actualPersistence = await vi.importActual<typeof import("../../services/quickDraftPersistence")>(
      "../../services/quickDraftPersistence",
    );
    localStorage.setItem(ACTIVE_QUICK_DRAFT_KEY, JSON.stringify({
      id: draftId, setCode: "custom-cube", difficulty: 2, kind: "Quick",
      phase: "playing", pickCount: 40, updatedAt: Date.now(),
    }));
    await actualPersistence.saveDraftRun(draftId, emptyRun);
    persistence.inspectActiveQuickDraftLifecycle.mockImplementation(() =>
      actualPersistence.inspectActiveQuickDraftLifecycle("inspect"));
    persistence.loadDraftRun.mockImplementation(() => actualPersistence.loadDraftRun(draftId));
    persistence.loadQuickDraftSession.mockResolvedValue({
      sessionJson: "saved session", mainDeck: ["Projected"], landCounts: {},
      poolSortMode: "color", poolPanelOpen: true, workspace: null,
    });
    wasm.import_draft_session.mockReturnValue(view([card("projected", "Projected")]));
    wasm.booster_pack_pool_for_game.mockReturnValue([]);

    expect(await useDraftStore.getState().resumeDraft()).toEqual({
      status: "unavailable", draftId, reason: "Saved draft run is unavailable",
    });
    expect(useDraftStore.getState()).toMatchObject({ draftId, runState: null, adapter: null });
    expect(wasm.import_draft_session).not.toHaveBeenCalled();
    expect(await actualPersistence.loadDraftRun(draftId)).toEqual(emptyRun);
    expect(localStorage.getItem(ACTIVE_QUICK_DRAFT_KEY)).not.toBeNull();
    expect(persistence.cleanupQuickDraftLifecycle).not.toHaveBeenCalled();

    // A complete run without a stage can restore its historical Cube source.
    await actualPersistence.saveDraftRun(draftId, run);
    expect(await useDraftStore.getState().resumeDraft()).toEqual({ status: "resumed", draftId });
    expect(useDraftStore.getState()).toMatchObject({ phase: "playing", runState: {
      playerDeck: ["Stored"], booster_pack_pool: [],
    } });
    expect(useDraftStore.getState().adapter).not.toBeNull();
    wasm.get_bot_deck.mockReturnValue({ main_deck: ["Opponent"], lands: {} });
    const navigate = vi.fn();

    await actualPersistence.saveDraftRun(draftId, emptyRun);
    await expect(useDraftStore.getState().launchNextMatch(navigate)).rejects.toThrow("Saved draft run is unavailable");
    expect(formatGate.evaluate).not.toHaveBeenCalled();
    expect(persistence.publishStagedDraftMatch).not.toHaveBeenCalled();

    await actualPersistence.saveDraftRun(draftId, run);
    formatGate.evaluate.mockImplementation(async (request: unknown) => {
      const deck = request as { main_deck: string[] };
      return {
        compatible: deck.main_deck[0] !== "Stored",
        reasons: deck.main_deck[0] === "Stored" ? ["Stored deck is invalid"] : [],
      };
    });
    await expect(useDraftStore.getState().launchNextMatch(navigate)).rejects.toThrow("Stored deck is invalid");
    expect(formatGate.evaluate).toHaveBeenCalledWith(expect.objectContaining({ main_deck: ["Stored"] }));
    expect(persistence.publishStagedDraftMatch).not.toHaveBeenCalled();

    formatGate.evaluate.mockResolvedValue({ compatible: true, reasons: [] });
    await useDraftStore.getState().launchNextMatch(navigate);
    expect(persistence.publishStagedDraftMatch).toHaveBeenCalledWith(expect.objectContaining({
      draftId, run: expect.objectContaining({ playerDeck: ["Stored"], activeMatch: expect.objectContaining({ draftId }) }),
      payload: expect.objectContaining({ player: expect.objectContaining({ main_deck: ["Stored"] }), booster_pack_pool: [] }),
    }));
    expect(navigate).toHaveBeenCalledOnce();
    expect(persistence.cleanupQuickDraftLifecycle).not.toHaveBeenCalled();
    await actualPersistence.clearDraftRun(draftId);
    localStorage.removeItem(ACTIVE_QUICK_DRAFT_KEY);
  });

  it("refuses an empty persisted stage game ID, then retries a valid stage", async () => {
    vi.useRealTimers();
    const draftId = "empty-stage-game";
    const run: DraftRunState = {
      format: "run", results: [], playerDeck: ["Player"], opponentDeck: ["Opponent"],
      usedBotSeats: [1], booster_pack_pool: [],
      activeMatch: { draftId, gameId: "same-stage", format: "run",
        resultCountAtLaunch: 0, botSeat: 1, opponentDeck: ["Opponent"] },
    };
    const emptyGameRun: DraftRunState = {
      ...run, activeMatch: { ...run.activeMatch!, gameId: "" },
    };
    const actualPersistence = await vi.importActual<typeof import("../../services/quickDraftPersistence")>(
      "../../services/quickDraftPersistence",
    );
    localStorage.setItem(ACTIVE_QUICK_DRAFT_KEY, JSON.stringify({
      id: draftId, setCode: "custom-cube", difficulty: 2, kind: "Quick",
      phase: "playing", pickCount: 40, updatedAt: Date.now(),
    }));
    await actualPersistence.saveDraftRun(draftId, emptyGameRun);
    persistence.inspectActiveQuickDraftLifecycle.mockImplementation(() =>
      actualPersistence.inspectActiveQuickDraftLifecycle("inspect"));
    persistence.loadDraftRun.mockImplementation(() => actualPersistence.loadDraftRun(draftId));
    persistence.loadQuickDraftSession.mockResolvedValue(null);

    expect(await useDraftStore.getState().resumeDraft()).toEqual({
      status: "unavailable", draftId, reason: "Saved draft run is unavailable",
    });
    expect(useDraftStore.getState()).toMatchObject({ draftId, runState: null });
    expect(await actualPersistence.loadDraftRun(draftId)).toEqual(emptyGameRun);
    expect(localStorage.getItem(ACTIVE_QUICK_DRAFT_KEY)).not.toBeNull();
    expect(persistence.cleanupQuickDraftLifecycle).not.toHaveBeenCalled();

    await actualPersistence.saveDraftRun(draftId, run);
    expect(await useDraftStore.getState().resumeDraft()).toEqual({ status: "resumed", draftId });
    expect(useDraftStore.getState()).toMatchObject({ phase: "playing", runState: run });
    await actualPersistence.saveDraftRun(draftId, emptyGameRun);
    const navigate = vi.fn();
    await expect(useDraftStore.getState().launchNextMatch(navigate)).rejects.toThrow("Saved draft run is unavailable");
    expect(await actualPersistence.loadDraftRun(draftId)).toEqual(emptyGameRun);
    expect(formatGate.evaluate).not.toHaveBeenCalled();
    expect(persistence.publishStagedDraftMatch).not.toHaveBeenCalled();
    expect(navigate).not.toHaveBeenCalled();
    expect(persistence.cleanupQuickDraftLifecycle).not.toHaveBeenCalled();

    await actualPersistence.saveDraftRun(draftId, run);
    await useDraftStore.getState().launchNextMatch(navigate);
    expect(formatGate.evaluate).toHaveBeenCalledTimes(2);
    expect(persistence.publishStagedDraftMatch).toHaveBeenCalledWith(expect.objectContaining({
      draftId, gameId: "same-stage",
    }));
    expect(navigate).toHaveBeenCalledWith(expect.stringContaining("/game/same-stage?"));
    await actualPersistence.clearDraftRun(draftId);
    localStorage.removeItem(ACTIVE_QUICK_DRAFT_KEY);
  });

  it("refuses a persisted stage owned by another draft on resume and launch", async () => {
    vi.useRealTimers();
    const draftId = "stage-owner";
    const run: DraftRunState = {
      format: "run", results: [], playerDeck: ["Player"], opponentDeck: ["Opponent"],
      usedBotSeats: [1], booster_pack_pool: [],
      activeMatch: { draftId, gameId: "same-stage", format: "run",
        resultCountAtLaunch: 0, botSeat: 1, opponentDeck: ["Opponent"] },
    };
    const wrongOwnerRun: DraftRunState = {
      ...run, activeMatch: { ...run.activeMatch!, draftId: "another-draft" },
    };
    const actualPersistence = await vi.importActual<typeof import("../../services/quickDraftPersistence")>(
      "../../services/quickDraftPersistence",
    );
    localStorage.setItem(ACTIVE_QUICK_DRAFT_KEY, JSON.stringify({
      id: draftId, setCode: "custom-cube", difficulty: 2, kind: "Quick",
      phase: "playing", pickCount: 40, updatedAt: Date.now(),
    }));
    await actualPersistence.saveDraftRun(draftId, wrongOwnerRun);
    persistence.inspectActiveQuickDraftLifecycle.mockImplementation(() =>
      actualPersistence.inspectActiveQuickDraftLifecycle("inspect"));
    persistence.loadDraftRun.mockImplementation(() => actualPersistence.loadDraftRun(draftId));
    persistence.loadQuickDraftSession.mockResolvedValue(null);

    expect(await useDraftStore.getState().resumeDraft()).toEqual({
      status: "unavailable", draftId, reason: "Saved draft run is unavailable",
    });
    expect(useDraftStore.getState()).toMatchObject({ draftId, runState: null });
    expect(await actualPersistence.loadDraftRun(draftId)).toEqual(wrongOwnerRun);
    expect(localStorage.getItem(ACTIVE_QUICK_DRAFT_KEY)).not.toBeNull();
    expect(persistence.cleanupQuickDraftLifecycle).not.toHaveBeenCalled();

    await actualPersistence.saveDraftRun(draftId, run);
    expect(await useDraftStore.getState().resumeDraft()).toEqual({ status: "resumed", draftId });
    expect(useDraftStore.getState()).toMatchObject({ phase: "playing", runState: run });
    await actualPersistence.saveDraftRun(draftId, wrongOwnerRun);
    const navigate = vi.fn();
    await expect(useDraftStore.getState().launchNextMatch(navigate)).rejects.toThrow("Saved draft run is unavailable");
    expect(await actualPersistence.loadDraftRun(draftId)).toEqual(wrongOwnerRun);
    expect(formatGate.evaluate).not.toHaveBeenCalled();
    expect(persistence.publishStagedDraftMatch).not.toHaveBeenCalled();
    expect(navigate).not.toHaveBeenCalled();
    expect(persistence.cleanupQuickDraftLifecycle).not.toHaveBeenCalled();

    await actualPersistence.saveDraftRun(draftId, run);
    await useDraftStore.getState().launchNextMatch(navigate);
    expect(formatGate.evaluate).toHaveBeenCalledTimes(2);
    expect(persistence.publishStagedDraftMatch).toHaveBeenCalledWith(expect.objectContaining({
      draftId, gameId: "same-stage",
    }));
    expect(navigate).toHaveBeenCalledOnce();
    await actualPersistence.clearDraftRun(draftId);
    localStorage.removeItem(ACTIVE_QUICK_DRAFT_KEY);
  });

  it("refuses a nonfinite in-memory difficulty changed through the store after a valid resume", async () => {
    const run: DraftRunState = { format: "run", results: [], playerDeck: ["Player"],
      opponentDeck: ["Opponent"], usedBotSeats: [1] };
    persistence.inspectActiveQuickDraftLifecycle.mockResolvedValue({
      id: "difficulty-change", setCode: "TST", difficulty: 3, kind: "Quick", phase: "playing",
    });
    persistence.loadDraftRun.mockResolvedValue(run);
    persistence.loadQuickDraftSession.mockResolvedValue(null);
    expect((await useDraftStore.getState().resumeDraft()).status).toBe("resumed");
    useDraftStore.getState().setDifficulty(Number.NaN);
    const navigate = vi.fn();
    await expect(useDraftStore.getState().launchNextMatch(navigate)).rejects.toThrow("Saved draft run is unavailable");
    expect(formatGate.evaluate).not.toHaveBeenCalled();
    expect(persistence.publishStagedDraftMatch).not.toHaveBeenCalled();
    expect(navigate).not.toHaveBeenCalled();
  });

  it("keeps the same unresolved run-only stage after publisher rejection and retries it", async () => {
    const run: DraftRunState = { format: "run", results: [], playerDeck: ["Player"],
      opponentDeck: ["Opponent"], usedBotSeats: [1],
      activeMatch: { draftId: "publish-retry", gameId: "same-stage", format: "run",
        resultCountAtLaunch: 0, botSeat: 1, opponentDeck: ["Opponent"] } };
    persistence.inspectActiveQuickDraftLifecycle.mockResolvedValue({
      id: "publish-retry", setCode: "TST", difficulty: 2, kind: "Quick", phase: "playing",
    });
    persistence.loadDraftRun.mockResolvedValue(run);
    persistence.loadQuickDraftSession.mockResolvedValue(null);
    expect((await useDraftStore.getState().resumeDraft()).status).toBe("resumed");
    persistence.publishStagedDraftMatch.mockRejectedValueOnce(new Error("handoff write failed"));
    const navigate = vi.fn();
    await expect(useDraftStore.getState().launchNextMatch(navigate)).rejects.toThrow("handoff write failed");
    expect(formatGate.evaluate).toHaveBeenCalledTimes(2);
    expect(useDraftStore.getState()).toMatchObject({ phase: "playing", runState: run });
    expect(navigate).not.toHaveBeenCalled();
    await useDraftStore.getState().launchNextMatch(navigate);
    const publishedCalls = persistence.publishStagedDraftMatch.mock.calls as unknown as Array<[{ gameId: string }]>;
    expect(publishedCalls.map(([input]) => input.gameId)).toEqual(["same-stage", "same-stage"]);
    expect(navigate).toHaveBeenCalledOnce();
  });

  it.each(["serialized", "literal"])("keeps nonfinite %s metadata at the parser boundary", async (encoding) => {
    const actualPersistence = await vi.importActual<typeof import("../../services/quickDraftPersistence")>(
      "../../services/quickDraftPersistence",
    );
    const raw = encoding === "serialized"
      ? JSON.stringify({ id: "nonfinite", setCode: "TST", difficulty: Number.NaN,
        phase: "playing", pickCount: 40, updatedAt: Date.now() })
      : `{ "id": "nonfinite", "setCode": "TST", "difficulty": NaN, "phase": "playing", "pickCount": 40, "updatedAt": ${Date.now()} }`;
    localStorage.setItem(ACTIVE_QUICK_DRAFT_KEY, raw);
    expect(await actualPersistence.inspectActiveQuickDraftLifecycle("inspect")).toBeNull();
    expect(localStorage.getItem(ACTIVE_QUICK_DRAFT_KEY)).toBeNull();
    expect(persistence.loadDraftRun).not.toHaveBeenCalled();
  });

  it("routes_resume_suggestions_and_submit_through_workspace_installation", async () => {
    // This drafting-session case has no durable run, regardless of earlier launch fixtures.
    persistence.loadDraftRun.mockReset().mockResolvedValue(null);
    const savedWorkspace = {
      ...createDraftWorkspaceState(),
      placements: {
        stale: { zone: "sideboard" as const, row: 0, column: 0, order: 0 },
      },
    };
    persistence.inspectActiveQuickDraftLifecycle.mockResolvedValue({
      id: "resume-id",
      setCode: "TST",
      setName: "Test",
      difficulty: 2,
      kind: "Quick",
      phase: "drafting",
    });
    persistence.loadQuickDraftSession.mockResolvedValue({
      sessionJson: "session",
      phase: "drafting",
      mainDeck: [],
      landCounts: {},
      poolSortMode: "color",
      poolPanelOpen: true,
      workspace: savedWorkspace,
    });
    wasm.import_draft_session.mockReturnValue(view([card("fresh", "Fresh")]));

    await useDraftStore.getState().resumeDraft();
    expect(useDraftStore.getState().workspaceState?.placements).not.toHaveProperty("stale");
    expect(useDraftStore.getState().workspaceState?.placements.fresh.zone).toBe("deck");
    expect(vi.getTimerCount()).toBe(0);

    wasm.suggest_deck.mockReturnValue({ main_deck: [], lands: { Island: 2 } });
    await useDraftStore.getState().autoSuggestDeck();
    expect(useDraftStore.getState().workspaceState?.placements.fresh.zone).toBe("sideboard");
    expect(projectWorkspaceLandCounts(useDraftStore.getState().workspaceState!)).toEqual({ Island: 2 });

    wasm.suggest_lands.mockReturnValue({ Plains: 1 });
    await useDraftStore.getState().autoSuggestLands();
    expect(projectWorkspaceLandCounts(useDraftStore.getState().workspaceState!)).toEqual({ Plains: 1 });

    const submittedView = { ...view([card("returned", "Returned")]), status: "Pairing" as const };
    wasm.submit_deck.mockReturnValue(submittedView);
    await useDraftStore.getState().submitDeck();
    expect(useDraftStore.getState()).toMatchObject({
      view: submittedView,
      phase: "launching",
    });
    expect(useDraftStore.getState().workspaceState?.placements).not.toHaveProperty("fresh");
    expect(useDraftStore.getState().workspaceState?.placements.returned.zone).toBe("deck");
    expect(projectWorkspaceMainDeck(
      useDraftStore.getState().workspaceState!,
      useDraftStore.getState().view!.pool,
    )).toEqual(["Returned"]);
  });

  it.each([
    {
      label: "Sealed run between games (session Pairing, run in progress)",
      kind: "Sealed",
      persistedPhase: "playing",
      sessionStatus: "Pairing",
      expectedPhase: "playing",
      fetchDatabase: true,
    },
    {
      label: "Sealed run finished (session Pairing, run complete)",
      kind: "Sealed",
      persistedPhase: "complete",
      sessionStatus: "Pairing",
      expectedPhase: "complete",
      fetchDatabase: true,
    },
    {
      label: "Quick run between games (session Complete, run in progress)",
      kind: "Quick",
      persistedPhase: "playing",
      sessionStatus: "Complete",
      expectedPhase: "playing",
      fetchDatabase: false,
    },
    {
      label: "Quick run finished (session Complete, run complete)",
      kind: "Quick",
      persistedPhase: "complete",
      sessionStatus: "Complete",
      expectedPhase: "complete",
      fetchDatabase: false,
    },
  ])("resume keeps the run phase: $label", async ({
    kind,
    persistedPhase,
    sessionStatus,
    expectedPhase,
    fetchDatabase,
  }) => {
    if (fetchDatabase) {
      vi.stubGlobal("fetch", vi.fn(async () => ({ text: async () => "database" })));
    }
    persistence.inspectActiveQuickDraftLifecycle.mockResolvedValue({
      id: "run-id",
      setCode: "TST",
      setName: "Test",
      difficulty: 2,
      kind,
      phase: persistedPhase,
      runFormat: "run",
      runWins: persistedPhase === "complete" ? 7 : 1,
      runLosses: persistedPhase === "complete" ? 3 : 0,
      runDraws: 0,
    });
    persistence.loadQuickDraftSession.mockResolvedValue({
      sessionJson: "session",
      mainDeck: ["C1", "C2"],
      landCounts: {},
      poolSortMode: "color",
      poolPanelOpen: true,
      workspace: null,
    });
    persistence.loadDraftRun.mockResolvedValue({
      format: "run",
      results: persistedPhase === "complete"
        ? [
          { gameId: "g1", result: "win" }, { gameId: "g2", result: "win" },
          { gameId: "g3", result: "win" }, { gameId: "g4", result: "win" },
          { gameId: "g5", result: "win" }, { gameId: "g6", result: "win" },
          { gameId: "g7", result: "win" },
        ]
        : [{ gameId: "g1", result: "win" }],
      playerDeck: ["C1", "C2"],
      opponentDeck: ["O1", "O2"],
      usedBotSeats: [1],
    });
    wasm.import_draft_session.mockReturnValue({
      ...view([card("c1", "C1"), card("c2", "C2")]),
      kind,
      status: sessionStatus,
    });

    await useDraftStore.getState().resumeDraft();

    expect(useDraftStore.getState().phase).toBe(expectedPhase);
    // A Sealed run launched as Full Run must resume as Full Run, not fall
    // back to the event's single-match default — next-match staging compares
    // the store format against the persisted run's format.
    expect(useDraftStore.getState().runFormat).toBe("run");
    expect(useDraftStore.getState().runState?.format).toBe("run");
  });

  it("resume before a Sealed run's first match keeps launching with the remembered format", async () => {
    vi.stubGlobal("fetch", vi.fn(async () => ({ text: async () => "database" })));
    persistence.inspectActiveQuickDraftLifecycle.mockResolvedValue({
      id: "sealed-pre-run-id",
      setCode: "TST",
      setName: "Test",
      difficulty: 2,
      kind: "Sealed",
      phase: "launching",
      runFormat: "run", // user already picked Full Run on the format picker
    });
    persistence.loadQuickDraftSession.mockResolvedValue({
      sessionJson: "session",
      mainDeck: ["C1", "C2"],
      landCounts: { Plains: 2 },
      poolSortMode: "color",
      poolPanelOpen: true,
      workspace: null,
    });
    persistence.loadDraftRun.mockResolvedValue(null);
    wasm.import_draft_session.mockReturnValue({
      ...view([card("c1", "C1"), card("c2", "C2")]),
      kind: "Sealed",
      status: "Pairing",
    });

    await useDraftStore.getState().resumeDraft();

    expect(useDraftStore.getState().phase).toBe("launching");
    expect(useDraftStore.getState().runFormat).toBe("run");
    expect(useDraftStore.getState().runState).toBeNull();
  });

  it("persists a nondefault format picker choice before the first match and restores it on resume", async () => {
    vi.stubGlobal("fetch", vi.fn(async () => ({ text: async () => "database" })));
    const poolView = {
      ...view([card("c1", "C1"), card("c2", "C2")]),
      kind: "Sealed" as const,
      status: "Deckbuilding" as const,
    };
    wasm.start_sealed_draft.mockReturnValue(poolView);
    await useDraftStore.getState().startSealedDraft("pool", "TST", "Test", 2);
    useDraftStore.getState().completeSealedOpening();
    wasm.submit_deck.mockReturnValue({ ...poolView, status: "Pairing" });
    await useDraftStore.getState().submitDeck();
    expect(useDraftStore.getState().phase).toBe("launching");
    expect(useDraftStore.getState().runFormat).toBe("single");
    const metaOf = (): { runFormat?: string; phase?: string } | null => {
      const calls = persistence.persistQuickDraftSnapshot.mock.calls;
      return calls.length > 0 ? calls[calls.length - 1][3] : null;
    };

    await settleTimers();
    // Deck submission persisted the Sealed default (Single Match).
    expect(metaOf()?.runFormat).toBe("single");

    // Choosing Full Run on the picker must persist: the run record only
    // appears at Start Match, so the persisted meta is the sole pre-run
    // resume authority. Without the schedule, the last meta stays "single".
    useDraftStore.getState().setRunFormat("run");
    await settleTimers();
    expect(metaOf()?.runFormat).toBe("run");

    // Reload before a run exists — the resumed picker must show the choice.
    persistence.inspectActiveQuickDraftLifecycle.mockResolvedValue(
      metaOf(),
    );
    persistence.loadQuickDraftSession.mockResolvedValue({
      sessionJson: "session",
      mainDeck: ["C1", "C2"],
      landCounts: {},
      poolSortMode: "color",
      poolPanelOpen: true,
      workspace: null,
    });
    persistence.loadDraftRun.mockResolvedValue(null);
    wasm.import_draft_session.mockReturnValue({ ...poolView, status: "Pairing" });

    await useDraftStore.getState().resumeDraft();

    expect(useDraftStore.getState().phase).toBe("launching");
    expect(useDraftStore.getState().runFormat).toBe("run");
    expect(useDraftStore.getState().runState).toBeNull();
  });

  it("resumes an interrupted first-match launch from the durable run (stale launching meta)", async () => {
    vi.stubGlobal("fetch", vi.fn(async () => ({ text: async () => "database" })));
    // Crash between publishInitialDraftMatch's run write and meta write:
    // meta still says "launching", but the durable run is committed with an
    // active staged match. Resume must show Between Matches, not the picker.
    persistence.inspectActiveQuickDraftLifecycle.mockResolvedValueOnce({
      id: "interrupted-launch-id",
      setCode: "TST",
      setName: "Test",
      difficulty: 2,
      kind: "Sealed",
      phase: "launching",
      runFormat: "run",
    });
    persistence.loadQuickDraftSession.mockResolvedValueOnce({
      sessionJson: "session",
      mainDeck: ["C1", "C2"],
      landCounts: {},
      poolSortMode: "color",
      poolPanelOpen: true,
      workspace: null,
    });
    persistence.loadDraftRun.mockResolvedValueOnce({
      format: "run",
      results: [],
      playerDeck: ["C1", "C2"],
      opponentDeck: ["O1", "O2"],
      usedBotSeats: [1],
      activeMatch: {
        draftId: "interrupted-launch-id",
        gameId: "g1",
        format: "run",
        resultCountAtLaunch: 0,
        botSeat: 1,
        opponentDeck: ["O1", "O2"],
      },
    });
    wasm.import_draft_session.mockReturnValue({
      ...view([card("c1", "C1"), card("c2", "C2")]),
      kind: "Sealed",
      status: "Pairing",
    });

    await useDraftStore.getState().resumeDraft();

    expect(useDraftStore.getState().phase).toBe("playing");
    expect(useDraftStore.getState().runState?.activeMatch?.gameId).toBe("g1");
  });

  it("resumes a terminal run as complete (stale playing meta)", async () => {
    vi.stubGlobal("fetch", vi.fn(async () => ({ text: async () => "database" })));
    // Crash between recordDraftMatchResult's run write and meta write: meta
    // still says "playing", but the durable run has hit the 3-loss limit.
    // Resume must show RunComplete — Between Matches would reject Next Match.
    persistence.inspectActiveQuickDraftLifecycle.mockResolvedValueOnce({
      id: "interrupted-result-id",
      setCode: "TST",
      setName: "Test",
      difficulty: 2,
      kind: "Sealed",
      phase: "playing",
      runFormat: "run",
    });
    persistence.loadQuickDraftSession.mockResolvedValueOnce({
      sessionJson: "session",
      mainDeck: ["C1", "C2"],
      landCounts: {},
      poolSortMode: "color",
      poolPanelOpen: true,
      workspace: null,
    });
    persistence.loadDraftRun.mockResolvedValueOnce({
      format: "run",
      results: [
        { gameId: "g1", result: "loss" },
        { gameId: "g2", result: "loss" },
        { gameId: "g3", result: "loss" },
      ],
      playerDeck: ["C1", "C2"],
      opponentDeck: ["O1", "O2"],
      usedBotSeats: [1],
      activeMatch: undefined,
    });
    wasm.import_draft_session.mockReturnValue({
      ...view([card("c1", "C1"), card("c2", "C2")]),
      kind: "Sealed",
      status: "Pairing",
    });

    await useDraftStore.getState().resumeDraft();

    expect(useDraftStore.getState().phase).toBe("complete");
    expect(useDraftStore.getState().runState?.results).toHaveLength(3);
  });

  it("records a resumed run's result with legacy metadata lacking runFormat", async () => {
    vi.stubGlobal("fetch", vi.fn(async () => ({ text: async () => "database" })));
    // ActiveQuickDraftMeta deliberately permits an absent runFormat (legacy
    // metadata shape). Resume restores the format from the durable run —
    // result recording must not gate out on the absent meta field and drop
    // the match, and the replacement metadata must learn the run's format so
    // a later recording is not dropped the same way.
    persistence.inspectActiveQuickDraftLifecycle.mockResolvedValue({
      id: "legacy-meta-id",
      setCode: "TST",
      setName: "Test",
      difficulty: 2,
      kind: "Sealed",
      phase: "playing",
      // no runFormat field — legacy shape
    });
    persistence.loadQuickDraftSession.mockResolvedValueOnce({
      sessionJson: "session",
      mainDeck: ["C1", "C2"],
      landCounts: {},
      poolSortMode: "color",
      poolPanelOpen: true,
      workspace: null,
    });
    persistence.loadDraftRun.mockResolvedValueOnce({
      format: "run",
      results: [{ gameId: "g1", result: "win" }],
      playerDeck: ["C1", "C2"],
      opponentDeck: ["O1", "O2"],
      usedBotSeats: [1],
      activeMatch: undefined,
    });
    wasm.import_draft_session.mockReturnValue({
      ...view([card("c1", "C1"), card("c2", "C2")]),
      kind: "Sealed",
      status: "Pairing",
    });

    await useDraftStore.getState().resumeDraft();
    expect(useDraftStore.getState().phase).toBe("playing");
    expect(useDraftStore.getState().runFormat).toBe("run");

    const recorded: Array<{
      gameId: string;
      result: DraftMatchResult;
      metaRunFormat?: string;
      metaPhase?: string;
    }> = [];
    persistence.recordDraftMatchResult.mockImplementation(async (input) => {
      const nextRun: DraftRunState = {
        format: "run",
        results: [{ gameId: input.gameId, result: input.result }],
        playerDeck: ["C1", "C2"],
        opponentDeck: ["O1", "O2"],
        usedBotSeats: [1],
      };
      const meta = input.makeMeta(nextRun);
      recorded.push({
        gameId: input.gameId,
        result: input.result,
        metaRunFormat: meta.runFormat,
        metaPhase: meta.phase,
      });
      return { run: nextRun, meta };
    });

    await useDraftStore.getState().recordMatchResult("g2", "win");

    // The result reached the durable writer at all (not gated out), and the
    // replacement metadata carries the run's format for future recordings.
    expect(recorded).toHaveLength(1);
    expect(recorded[0]?.gameId).toBe("g2");
    expect(recorded[0]?.result).toBe("win");
    expect(recorded[0]?.metaRunFormat).toBe("run");
    expect(recorded[0]?.metaPhase).toBe("playing");
    expect(useDraftStore.getState().runState?.results).toHaveLength(1);
    expect(useDraftStore.getState().phase).toBe("playing");
  });

  it.each([
    {
      label: "without virtual lands",
      addLands: [] as string[],
      expected: ["Spell"],
    },
    {
      label: "with virtual lands",
      addLands: ["Plains", "Island"],
      expected: ["Spell", "Plains", "Island"],
    },
  ])("submits the exact deck $label", async ({ addLands, expected }) => {
    await start([card("spell", "Spell")]);
    for (const land of addLands) useDraftStore.getState().addBasicLand(land);

    wasm.submit_deck.mockReturnValue({ ...view(), status: "Pairing" });
    await useDraftStore.getState().submitDeck();

    expect(wasm.submit_deck).toHaveBeenCalledWith(JSON.stringify(expected), JSON.stringify([]));
  });

  it("excludes sideboard virtual lands from the submitted deck", async () => {
    await start([card("spell", "Spell")]);
    useDraftStore.getState().addBasicLand("Plains");
    useDraftStore.getState().addBasicLand("Island");
    const workspace = useDraftStore.getState().workspaceState!;
    const sideboardIsland = workspace.virtualBasics.find((basic) => basic.name === "Island")!;
    useDraftStore.getState().setWorkspacePlacement(sideboardIsland.instanceId, {
      zone: "sideboard",
      row: 0,
      column: 0,
      order: 0,
    });

    wasm.submit_deck.mockReturnValue({ ...view(), status: "Pairing" });
    await useDraftStore.getState().submitDeck();

    expect(wasm.submit_deck).toHaveBeenCalledWith(JSON.stringify(["Spell", "Plains"]), JSON.stringify([]));
  });

  it("projects custom addables as spells for persistence and land suggestions", async () => {
    wasm.start_quick_draft.mockReturnValue({
      ...view([card("spell", "Wind Drake")]),
      addable_cards: ["Plains", "Academy Ruins"],
    });
    await useDraftStore.getState().startDraft("pool", "TST", "Test", 2);
    await settleTimers();
    vi.clearAllMocks();

    useDraftStore.getState().addBasicLand("Academy Ruins");
    useDraftStore.getState().addBasicLand("Plains");
    await settleTimers();

    expect(persistence.persistQuickDraftSnapshot).toHaveBeenCalledWith(
      expect.any(String),
      "session",
      expect.objectContaining({
        mainDeck: ["Wind Drake", "Academy Ruins"],
        landCounts: { Plains: 1 },
      }),
      expect.any(Object),
    );

    wasm.suggest_lands.mockReturnValue({});
    await useDraftStore.getState().autoSuggestLands();
    expect(wasm.suggest_lands).toHaveBeenCalledWith(
      JSON.stringify(["Wind Drake", "Academy Ruins"]),
    );

    wasm.submit_deck.mockReturnValue({ ...view(), status: "Pairing" });
    await useDraftStore.getState().submitDeck();
    expect(wasm.submit_deck).toHaveBeenCalledWith(
      JSON.stringify(["Wind Drake", "Academy Ruins"]),
      JSON.stringify([]),
    );
  });

  it("admits_once_and_uses_exact_terminal_notification_counts", async () => {
    await start();
    await settleTimers();
    vi.clearAllMocks();
    const observations: Array<{ locked: boolean; pending: unknown; pool: string[] }> = [];
    const unsubscribe = useDraftStore.subscribe((state) => observations.push({
      locked: state.pickInteractionLocked,
      pending: state.pendingPickIntent,
      pool: state.view?.pool.map((entry) => entry.instance_id) ?? [],
    }));
    const result = deferred<DraftPlayerView>();
    wasm.submit_pick.mockReturnValue(result.promise);
    const pick = useDraftStore.getState().pickCard("picked", "sideboard");
    expect(observations).toHaveLength(1);
    expect(observations[0]).toMatchObject({ locked: true, pool: [] });
    await expect(useDraftStore.getState().pickCard("busy"))
      .resolves.toEqual({ status: "ignored", reason: "busy" });
    expect(observations).toHaveLength(1);

    result.resolve(view([card("picked")]));
    await expect(pick).resolves.toEqual({ status: "acknowledged" });
    unsubscribe();
    expect(observations).toHaveLength(2);
    expect(observations[1]).toMatchObject({ locked: false, pending: null, pool: ["picked"] });
    expect(vi.getTimerCount()).toBe(1);
  });

  it("unacknowledged_has_admission_and_cleanup_only", async () => {
    await start([card("existing")]);
    await settleTimers();
    vi.clearAllMocks();
    const original = useDraftStore.getState();
    const observations: unknown[] = [];
    const unsubscribe = useDraftStore.subscribe((state) => observations.push(state));
    wasm.submit_pick.mockReturnValue(view([card("existing")]));

    await expect(useDraftStore.getState().pickCard("missing"))
      .resolves.toEqual({ status: "rejected", reason: "unacknowledged" });
    unsubscribe();

    expect(observations).toHaveLength(2);
    expect(useDraftStore.getState()).toMatchObject({
      view: original.view,
      workspaceState: original.workspaceState,
      selectedCard: original.selectedCard,
      pendingPickIntent: null,
      pickInteractionLocked: false,
    });
    expect(vi.getTimerCount()).toBe(0);
  });

  it("stale_pick_settles_without_cleanup_write_before_adapter_invocation", async () => {
    await start();
    const suggestionResult = deferred<unknown>();
    wasm.suggest_deck.mockReturnValue(suggestionResult.promise);
    const suggestion = useDraftStore.getState().autoSuggestDeck();
    await Promise.resolve();
    const pick = useDraftStore.getState().pickCard("late");
    useDraftStore.getState().reset();
    const notificationsAfterReplacement: unknown[] = [];
    const unsubscribe = useDraftStore.subscribe((state) => notificationsAfterReplacement.push(state));
    suggestionResult.resolve({ main_deck: [], lands: {} });
    await suggestion;
    await expect(pick).resolves.toEqual({ status: "ignored", reason: "stale" });
    unsubscribe();
    expect(wasm.submit_pick).not.toHaveBeenCalled();
    expect(notificationsAfterReplacement).toHaveLength(0);
  });

  it("retries_after_live_rejections", async () => {
    await start();
    wasm.submit_pick
      .mockImplementationOnce(() => { throw new Error("adapter failure"); })
      .mockReturnValueOnce(view([]))
      .mockReturnValueOnce(view([card("picked")]));
    await expect(useDraftStore.getState().pickCard("picked"))
      .resolves.toEqual({ status: "rejected", reason: "adapter" });
    await expect(useDraftStore.getState().pickCard("picked"))
      .resolves.toEqual({ status: "rejected", reason: "unacknowledged" });
    await expect(useDraftStore.getState().pickCard("picked"))
      .resolves.toEqual({ status: "acknowledged" });
    expect(wasm.submit_pick).toHaveBeenCalledTimes(3);
  });

  it("publishes_one_monotonic_generation_per_lifecycle_invocation", async () => {
    vi.stubGlobal("fetch", vi.fn(async () => ({ text: async () => "database" })));
    wasm.start_quick_draft.mockReturnValue(view());
    wasm.start_sealed_draft.mockReturnValue({ ...view(), kind: "Sealed", status: "Deckbuilding" });
    wasm.start_quick_cube_draft.mockReturnValue(view());
    const generations: number[] = [];
    let previous = useDraftStore.getState().interactionGeneration;
    const unsubscribe = useDraftStore.subscribe((state) => {
      if (state.interactionGeneration !== previous) {
        previous = state.interactionGeneration;
        generations.push(previous);
      }
    });

    await useDraftStore.getState().startDraft("pool", "TST", "Test", 2);
    await useDraftStore.getState().startSealedDraft("pool", "TST", "Test", 2);
    await useDraftStore.getState().startCubeDraft("cube", "Cube", {
      pod_size: 8,
      pack_count: 3,
      cards_per_pack: 15,
      min_deck_size: 40,
      addable_cards: { policy: "StandardBasics", custom: [] },
    }, 2);
    persistence.inspectActiveQuickDraftLifecycle.mockResolvedValueOnce(null);
    await useDraftStore.getState().resumeDraft();
    await useDraftStore.getState().abandonDraft();
    await useDraftStore.getState().endRun();
    useDraftStore.getState().reset();
    wasm.start_quick_draft.mockImplementationOnce(() => { throw new Error("failed start"); });
    await expect(useDraftStore.getState().startDraft("pool", "TST", "Test", 2))
      .rejects.toThrow("failed start");
    unsubscribe();

    expect(generations).toHaveLength(8);
    expect(generations.every((generation, index) => (
      index === 0 || generation === generations[index - 1]! + 1
    ))).toBe(true);
  });

  it("applies an effect destination independently and excludes the effect card", async () => {
    await start([card("effect")]);
    wasm.submit_pick_with_draft_effect.mockReturnValue(view([
      card("effect"), card("first"), card("second"),
    ]));

    await useDraftStore.getState().pickCardWithDraftEffect(
      "effect", ["first", "second"], "sideboard",
    );
    const placements = useDraftStore.getState().workspaceState!.placements;
    expect(placements.effect.zone).toBe("deck");
    expect(placements.first.zone).toBe("sideboard");
    expect(placements.second.zone).toBe("sideboard");
  });

  it("resolves contended pick families without disturbing the admitted intent", async () => {
    await start();
    const result = deferred<DraftPlayerView>();
    wasm.submit_pick.mockReturnValue(result.promise);

    const first = useDraftStore.getState().pickCard("first");
    const pending = useDraftStore.getState().pendingPickIntent;
    await useDraftStore.getState().autoPickCard();
    await useDraftStore.getState().pickCard("second", "sideboard");
    expect(useDraftStore.getState().pendingPickIntent).toBe(pending);
    expect(wasm.submit_pick).toHaveBeenCalledOnce();
    expect(wasm.auto_pick).not.toHaveBeenCalled();

    result.resolve(view([card("first")]));
    await first;
  });

  it("projects workspace and compatibility fields in one subscriber-visible commit", async () => {
    await start([card("one", "Shared"), card("two", "Shared")]);
    const observations: Array<{ zone?: string; deck: string[] }> = [];
    const unsubscribe = useDraftStore.subscribe((state) => {
      observations.push({
        zone: state.workspaceState?.placements.one.zone,
        deck: state.workspaceState
          ? projectWorkspaceMainDeck(state.workspaceState, state.view!.pool)
          : [],
      });
    });

    useDraftStore.getState().setWorkspacePlacement("one", {
      zone: "sideboard", row: 0, column: 0, order: 0,
    });
    unsubscribe();

    expect(observations).toHaveLength(1);
    expect(observations[0]).toEqual({ zone: "sideboard", deck: ["Shared"] });
    expect(wasm.submit_pick).not.toHaveBeenCalled();
  });

  it("keeps workspace null and exposes no legacy deck facade fields", () => {
    const state = useDraftStore.getState();
    expect(state).toMatchObject({
      workspaceState: null,
      pendingPickIntent: null,
    });
    expect("mainDeck" in state).toBe(false);
    expect("landCounts" in state).toBe(false);
    expect("addToDeck" in state).toBe(false);
    expect("removeFromDeck" in state).toBe(false);
    expect("setLandCount" in state).toBe(false);
  });

  it("bounds virtual materialization and interactive collision fallback", () => {
    const migrated = migrateLegacyWorkspace([], {
      mainDeck: [],
      landCounts: { Island: MAX_MATERIALIZED_VIRTUAL_BASICS + 20, Plains: 2 },
    });
    expect(migrated.virtualBasics).toHaveLength(MAX_MATERIALIZED_VIRTUAL_BASICS);

    vi.spyOn(crypto, "randomUUID")
      .mockReturnValue("00000000-0000-4000-8000-000000000000");
    const workspace = {
      ...createDraftWorkspaceState(),
      placements: {
        "workspace-basic:interactive:00000000-0000-4000-8000-000000000000": {
          zone: "deck" as const, row: 0, column: 0, order: 0,
        },
        "workspace-basic:interactive:fallback:0": {
          zone: "deck" as const, row: 0, column: 0, order: 1,
        },
      },
    };
    expect(makeInteractiveVirtualBasicInstanceId(workspace, []))
      .toBe("workspace-basic:interactive:fallback:1");
  });

  it("adds below the shared virtual basic ceiling but no-ops once the global total is reached", async () => {
    await start();
    const names = ["Island", "Plains", "Mountain"];
    const belowCeiling = {
      ...useDraftStore.getState().workspaceState!,
      virtualBasics: Array.from(
        { length: MAX_MATERIALIZED_VIRTUAL_BASICS - 1 },
        (_, index) => ({ instanceId: `basic-${index}`, name: names[index % names.length] }),
      ),
    };
    useDraftStore.getState().setWorkspaceState(belowCeiling);
    await settleTimers();
    expect(useDraftStore.getState().workspaceState!.virtualBasics)
      .toHaveLength(MAX_MATERIALIZED_VIRTUAL_BASICS - 1);

    useDraftStore.getState().addBasicLand("Island");
    const atCeiling = useDraftStore.getState().workspaceState;
    expect(atCeiling!.virtualBasics).toHaveLength(MAX_MATERIALIZED_VIRTUAL_BASICS);

    useDraftStore.getState().addBasicLand("Plains");
    expect(useDraftStore.getState().workspaceState).toBe(atCeiling);
    expect(useDraftStore.getState().workspaceState!.virtualBasics)
      .toHaveLength(MAX_MATERIALIZED_VIRTUAL_BASICS);
  });

  it("cleans matching start failures and permits a successful retry", async () => {
    wasm.start_quick_draft.mockImplementationOnce(() => {
      throw new Error("initialize failed");
    });
    await expect(useDraftStore.getState().startDraft("pool", "TST", "Test", 2))
      .rejects.toThrow("initialize failed");
    expect(useDraftStore.getState()).toMatchObject({
      draftId: null, workspaceState: null, pendingPickIntent: null,
    });

    await start([card("retry")]);
    expect(useDraftStore.getState().workspaceState?.placements.retry.zone).toBe("deck");
  });

  it("clears a matching rejected pick without changing its workspace", async () => {
    await start([card("existing")]);
    const workspace = useDraftStore.getState().workspaceState;
    wasm.submit_pick.mockImplementation(() => {
      throw new Error("pick rejected");
    });

    await expect(useDraftStore.getState().pickCard("picked", "sideboard"))
      .resolves.toEqual({ status: "rejected", reason: "adapter" });
    expect(useDraftStore.getState().pendingPickIntent).toBeNull();
    expect(useDraftStore.getState().workspaceState).toBe(workspace);
  });

  it.each(["success", "adapter rejection", "invalid acknowledgment"] as const)(
    "ignores stale pick %s after reset",
    async (settlement) => {
    await start();
    const result = deferred<DraftPlayerView>();
    wasm.submit_pick.mockReturnValue(result.promise);
    const pick = useDraftStore.getState().pickCard("late");

    await Promise.resolve();
    await Promise.resolve();
    expect(wasm.submit_pick).toHaveBeenCalledOnce();
    useDraftStore.getState().reset();
    const observations: unknown[] = [];
    const unsubscribe = useDraftStore.subscribe((state) => observations.push(state));
    if (settlement === "adapter rejection") result.reject(new Error("late rejection"));
    else if (settlement === "invalid acknowledgment") result.resolve(view());
    else result.resolve(view([card("late")]));
    await expect(pick).resolves.toEqual({ status: "ignored", reason: "stale" });
    unsubscribe();
    expect(useDraftStore.getState()).toMatchObject({
      workspaceState: null, pendingPickIntent: null,
    });
    expect(observations).toHaveLength(0);
  });

  it("does not queue submit or launch behind an admitted pick", async () => {
    await start();
    const result = deferred<DraftPlayerView>();
    wasm.submit_pick.mockReturnValue(result.promise);
    const pick = useDraftStore.getState().pickCard("first");

    await useDraftStore.getState().submitDeck();
    await useDraftStore.getState().launchMatch(vi.fn());
    expect(wasm.submit_deck).not.toHaveBeenCalled();
    expect(wasm.get_bot_deck).not.toHaveBeenCalled();

    result.resolve(view([card("first")]));
    await pick;
  });

  it.each([
    { durable: undefined, projected: [], expected: [], saves: 1 },
    { durable: undefined, projected: undefined, expected: undefined, saves: 0 },
    { durable: ["Original", "Original"], projected: ["Other"], expected: ["Original", "Original"], saves: 0 },
    { durable: [], projected: ["Other"], expected: [], saves: 0 },
    { durable: null, projected: ["Other"], expected: null, saves: 0 },
  ])("resumes legacy source metadata only from the host accessor: $durable / $projected", async ({ durable, projected, expected, saves }) => {
    persistence.inspectActiveQuickDraftLifecycle.mockResolvedValue({
      id: "legacy", setCode: "custom-cube", setName: "Same label", difficulty: 2,
      kind: "Quick", phase: "playing",
    });
    persistence.loadQuickDraftSession.mockResolvedValue({
      sessionJson: "legacy session", mainDeck: [], landCounts: {},
      poolSortMode: "color", poolPanelOpen: true, workspace: null,
    });
    persistence.loadDraftRun.mockResolvedValueOnce({
      format: "run", results: [], playerDeck: ["Player"], opponentDeck: ["Opponent"],
      usedBotSeats: [1], booster_pack_pool: durable,
    });
    const imported = view([card("picked")]);
    wasm.import_draft_session.mockReturnValue(imported);
    wasm.booster_pack_pool_for_game.mockReturnValue(projected);
    const outcome = await useDraftStore.getState().resumeDraft();
    expect(wasm.import_draft_session).toHaveBeenCalledWith("legacy session", 2);
    if (Array.isArray(expected)) {
      expect(outcome).toEqual({ status: "resumed", draftId: "legacy" });
      expect(useDraftStore.getState().view).toEqual(imported);
      expect(useDraftStore.getState().runState?.booster_pack_pool).toEqual(expected);
    } else {
      expect(outcome).toEqual({ status: "unavailable", draftId: "legacy", reason: "Saved draft run is unavailable" });
      expect(useDraftStore.getState().runState).toBeNull();
    }
    expect(persistence.saveDraftRun).toHaveBeenCalledTimes(saves);
    if (saves) expect(persistence.saveDraftRun).toHaveBeenCalledWith("legacy", expect.objectContaining({ booster_pack_pool: [] }));
  });

  it.each([false, true])("durably upgrades a legacy staged launch from the engine view (next=%s)", async (next) => {
    const projected = view([card("spell")]);
    wasm.start_quick_draft.mockReturnValue(projected);
    wasm.booster_pack_pool_for_game.mockReturnValue([]);
    await useDraftStore.getState().startDraft("pool", "TST", "Test", 2);
    const draftId = useDraftStore.getState().draftId!;
    const run: DraftRunState = {
      format: "run", results: [], playerDeck: ["spell"], opponentDeck: ["Opponent"], usedBotSeats: [1],
      activeMatch: { draftId, gameId: "staged-legacy", format: "run", resultCountAtLaunch: 0, botSeat: 1, opponentDeck: ["Opponent"] },
    };
    persistence.loadDraftRun.mockResolvedValueOnce(run);
    const navigate = vi.fn();
    if (next) await useDraftStore.getState().launchNextMatch(navigate);
    else await useDraftStore.getState().launchMatch(navigate);
    expect(navigate).toHaveBeenCalledOnce();
    expect(persistence.publishStagedDraftMatch).toHaveBeenCalledWith(expect.objectContaining({
      run: expect.objectContaining({ booster_pack_pool: [] }),
      payload: expect.objectContaining({ booster_pack_pool: [] }),
    }));
  });

  it.each([
    { pool: ["Cube A", "Cube A", "Undealt sentinel"] }, { pool: [] },
    { pool: undefined }, { pool: null },
  ])("retains the original cube source through initial publication, retry, and next match: $pool", async ({ pool }) => {
    const draftView = view([card("spell")]);
    draftView.seats = [{
      seat_index: 1,
      display_name: "Bot",
      is_bot: true,
      connected: true,
      has_submitted_deck: true,
      pick_status: "NotDrafting",
      active_pack_count: 0,
      drafted_card_count: 0,
      face_up_draft_cards: [],
    }];
    if (Array.isArray(pool)) {
      vi.stubGlobal("fetch", vi.fn(async () => ({ text: async () => "database" })));
      wasm.start_quick_cube_draft.mockReturnValue(draftView);
      await useDraftStore.getState().startCubeDraft("cube", "Same label", {
        pod_size: 8, pack_count: 3, cards_per_pack: 15, min_deck_size: 40,
        addable_cards: { policy: "StandardBasics", custom: [] },
      }, 2);
    } else {
      wasm.start_quick_draft.mockReturnValue(draftView);
      await useDraftStore.getState().startDraft("pool", "TST", "Test", 2);
    }
    wasm.booster_pack_pool_for_game.mockReturnValue(pool);
    wasm.get_bot_deck.mockReturnValue({ main_deck: ["Opponent"], lands: {} });
    const randomUuid = vi.spyOn(crypto, "randomUUID")
      .mockReturnValue("00000000-0000-4000-8000-000000000123");
    let stagedRun: unknown;
    persistence.loadDraftRun.mockResolvedValueOnce(null);
    persistence.publishInitialDraftMatch.mockImplementationOnce(async (input: { run: unknown }) => {
      stagedRun = input.run;
      // The publication reached the durable run write before metadata failed.
      persistence.loadDraftRun.mockResolvedValue(stagedRun);
      throw new Error("metadata failed");
    });
    const navigate = vi.fn();

    await expect(useDraftStore.getState().launchMatch(navigate)).rejects.toThrow("metadata failed");
    expect(stagedRun).toMatchObject({ booster_pack_pool: pool });
    expect(persistence.publishInitialDraftMatch).toHaveBeenCalledWith(expect.objectContaining({
      run: expect.objectContaining({ booster_pack_pool: pool }),
      payload: expect.objectContaining({ booster_pack_pool: pool }),
    }));
    expect(persistence.persistQuickDraftSnapshot).not.toHaveBeenCalled();
    // A later same-labelled cube must never replace an already bound source.
    if (pool !== undefined) wasm.booster_pack_pool_for_game.mockReturnValue(["Other cube"]);
    await useDraftStore.getState().launchMatch(navigate);

    expect(randomUuid).toHaveBeenCalledOnce();
    expect(wasm.get_bot_deck).toHaveBeenCalledOnce();
    expect(wasm.export_draft_session).toHaveBeenCalledOnce();
    expect(persistence.publishStagedDraftMatch).toHaveBeenCalledOnce();
    expect(navigate).toHaveBeenCalledWith(expect.stringContaining("00000000-0000-4000-8000-000000000123"));
    expect(persistence.publishStagedDraftMatch).toHaveBeenLastCalledWith(expect.objectContaining({
      payload: expect.objectContaining({ booster_pack_pool: pool }),
    }));
    const completed = { ...(stagedRun as DraftRunState), activeMatch: undefined,
      results: [{ gameId: "finished", result: "win" as const }] };
    persistence.loadDraftRun.mockResolvedValueOnce(completed);
    await useDraftStore.getState().launchNextMatch(navigate);
    expect(navigate).toHaveBeenCalledTimes(2);
    expect(persistence.publishStagedDraftMatch).toHaveBeenLastCalledWith(expect.objectContaining({
      run: expect.objectContaining({ booster_pack_pool: pool }),
      payload: expect.objectContaining({ booster_pack_pool: pool }),
    }));
  });

  it("rejects an ante human deck before SubmitDeck while admitting its legal sibling", async () => {
    const pool = [card("ante", "Contract from Below"), card("legal", "Forest")];
    await start(pool);
    const reason = "Can't be in a deck or sideboard unless the game is played for ante: Contract from Below";
    formatGate.evaluate.mockResolvedValueOnce({ compatible: false, reasons: [reason] });
    await expect(useDraftStore.getState().submitDeck()).rejects.toThrow(reason);
    expect(formatGate.evaluate).toHaveBeenCalledWith(expect.objectContaining({
      main_deck: ["Contract from Below", "Forest"], sideboard: [], commander: [],
      draft_set_codes: [], selected_format: "Limited", selected_match_type: "Bo1", player_count: 2,
    }));
    expect(wasm.submit_deck).not.toHaveBeenCalled();
    expect(useDraftStore.getState().phase).toBe("drafting");

    useDraftStore.getState().setWorkspacePlacement("ante", { zone: "sideboard", row: 0, column: 0, order: 0 });
    wasm.submit_deck.mockReturnValue({ ...view(pool), status: "Pairing" });
    await useDraftStore.getState().submitDeck();
    expect(formatGate.evaluate).toHaveBeenLastCalledWith(expect.objectContaining({ main_deck: ["Forest"] }));
    expect(wasm.submit_deck).toHaveBeenCalledOnce();
    expect(useDraftStore.getState().phase).toBe("launching");
  });

  it("fails closed on unavailable submission gates and invalidates an edited pending deck", async () => {
    await start([card("first", "Forest")]);
    formatGate.evaluate.mockRejectedValueOnce(new Error("gate transport failed"));
    await expect(useDraftStore.getState().submitDeck()).rejects.toThrow("gate transport failed");
    expect(wasm.submit_deck).not.toHaveBeenCalled();

    const delayed = deferred<{ compatible: boolean; reasons: string[] }>();
    formatGate.evaluate.mockReturnValueOnce(delayed.promise);
    const submission = useDraftStore.getState().submitDeck();
    await vi.waitFor(() => expect(formatGate.evaluate).toHaveBeenCalledTimes(2));
    useDraftStore.setState({ workspaceState: { ...useDraftStore.getState().workspaceState! } });
    delayed.resolve({ compatible: true, reasons: [] });
    await expect(submission).rejects.toThrow("Stale draft deck submission");
    expect(wasm.submit_deck).not.toHaveBeenCalled();
  });

  it("preflights both exact Bo1 seats before initial publication and rejects an invalid opponent", async () => {
    await start([card("human", "Forest")]);
    useDraftStore.setState({ phase: "launching" });
    wasm.get_bot_deck.mockReturnValue({ main_deck: ["Opponent"], lands: {} });
    persistence.loadDraftRun.mockResolvedValue(null);
    const navigate = vi.fn();
    await useDraftStore.getState().launchMatch(navigate);
    expect(formatGate.evaluate).toHaveBeenCalledTimes(2);
    const published = persistence.publishInitialDraftMatch.mock.calls[0][0] as {
      payload: { player: { main_deck: string[]; sideboard: string[]; commander: string[] }; opponent: { main_deck: string[]; sideboard: string[]; commander: string[] } };
      run: DraftRunState; meta: { runFormat: string };
    };
    expect(published.payload.player.main_deck).toEqual(["Forest"]);
    expect(published.payload.opponent.main_deck).toEqual(["Opponent"]);
    for (const deck of [published.payload.player, published.payload.opponent]) {
      expect(formatGate.evaluate).toHaveBeenCalledWith(expect.objectContaining({
        main_deck: deck.main_deck, sideboard: deck.sideboard, commander: deck.commander,
        selected_format: "Limited", selected_match_type: "Bo1", player_count: 2,
      }));
      expect(deck.sideboard).toEqual([]);
    }
    expect(published.run.format).toBe("run");
    expect(published.run.activeMatch?.format).toBe("run");
    expect(published.meta.runFormat).toBe("run");
    expect(navigate).toHaveBeenCalledWith(expect.stringContaining("match=bo1"));

    useDraftStore.getState().reset();
    await start([card("human", "Forest")]);
    useDraftStore.setState({ phase: "launching" });
    wasm.get_bot_deck.mockReturnValue({ main_deck: ["Opponent"], lands: {} });
    persistence.loadDraftRun.mockResolvedValue(null);
    persistence.publishInitialDraftMatch.mockClear();
    navigate.mockClear();
    formatGate.evaluate.mockClear();
    formatGate.evaluate.mockImplementation(async (request: unknown) => ({
      compatible: (request as { main_deck: string[] }).main_deck[0] !== "Opponent",
      reasons: (request as { main_deck: string[] }).main_deck[0] === "Opponent" ? ["opponent rejected"] : [],
    }));
    await expect(useDraftStore.getState().launchMatch(navigate)).rejects.toThrow("opponent rejected");
    expect(formatGate.evaluate).toHaveBeenCalledTimes(2);
    expect(persistence.publishInitialDraftMatch).not.toHaveBeenCalled();
    expect(navigate).not.toHaveBeenCalled();
    expect(useDraftStore.getState().phase).toBe("launching");
  });

  it("tries a second bot after an exact opponent refusal and publishes only that seat", async () => {
    await startLaunchWithBotSeats([1, 2]);
    vi.spyOn(Math, "random").mockReturnValue(0);
    vi.spyOn(crypto, "randomUUID").mockReturnValue("00000000-0000-4000-8000-000000000222");
    wasm.get_bot_deck.mockImplementation((seat: number) => ({
      main_deck: [seat === 1 ? "Rejected Bot" : "Legal Bot"], lands: { Forest: 39 },
    }));
    formatGate.evaluate.mockImplementation(async (request: unknown) => ({
      compatible: (request as { main_deck: string[] }).main_deck[0] !== "Rejected Bot",
      reasons: (request as { main_deck: string[] }).main_deck[0] === "Rejected Bot"
        ? ["opponent rejected"] : [],
    }));
    const navigate = vi.fn();
    await useDraftStore.getState().launchMatch(navigate);

    expect(wasm.get_bot_deck.mock.calls.map(([seat]) => seat)).toEqual([1, 2]);
    expect(formatGate.evaluate).toHaveBeenCalledTimes(3);
    expect(formatGate.evaluate).toHaveBeenNthCalledWith(1, expect.objectContaining({
      main_deck: ["Forest"], sideboard: [], selected_format: "Limited", selected_match_type: "Bo1",
    }));
    expect(formatGate.evaluate).toHaveBeenNthCalledWith(2, expect.objectContaining({
      main_deck: ["Rejected Bot", ...Array<string>(39).fill("Forest")], sideboard: [],
    }));
    expect(formatGate.evaluate).toHaveBeenNthCalledWith(3, expect.objectContaining({
      main_deck: ["Legal Bot", ...Array<string>(39).fill("Forest")], sideboard: [],
    }));
    expect(persistence.publishInitialDraftMatch).toHaveBeenCalledOnce();
    const published = persistence.publishInitialDraftMatch.mock.calls[0][0] as {
      run: DraftRunState; payload: { opponent: { main_deck: string[] } }; gameId: string;
    };
    expect(published.run.usedBotSeats).toEqual([2]);
    expect(published.run.activeMatch).toMatchObject({ botSeat: 2, gameId: published.gameId });
    expect(published.run.opponentDeck).toEqual(published.payload.opponent.main_deck);
    expect(published.payload.opponent.main_deck[0]).toBe("Legal Bot");
    expect(navigate).toHaveBeenCalledOnce();
  });

  it("keeps first-valid selection finite and leaves an all-invalid run unpublished", async () => {
    await startLaunchWithBotSeats([1, 2]);
    vi.spyOn(Math, "random").mockReturnValue(0);
    wasm.get_bot_deck.mockImplementation((seat: number) => ({ main_deck: [`Bot ${seat}`], lands: {} }));
    const navigate = vi.fn();
    await useDraftStore.getState().launchMatch(navigate);
    expect(wasm.get_bot_deck.mock.calls.map(([seat]) => seat)).toEqual([1]);
    expect(persistence.publishInitialDraftMatch).toHaveBeenCalledOnce();
    expect(navigate).toHaveBeenCalledOnce();

    useDraftStore.getState().reset();
    await startLaunchWithBotSeats([1, 2]);
    wasm.get_bot_deck.mockClear();
    persistence.publishInitialDraftMatch.mockClear();
    navigate.mockClear();
    formatGate.evaluate.mockClear();
    formatGate.evaluate.mockImplementation(async (request: unknown) => ({
      compatible: (request as { main_deck: string[] }).main_deck[0] === "Forest",
      reasons: (request as { main_deck: string[] }).main_deck[0] === "Forest" ? [] : ["opponent rejected"],
    }));
    await expect(useDraftStore.getState().launchMatch(navigate)).rejects.toThrow("opponent rejected");
    expect(wasm.get_bot_deck.mock.calls.map(([seat]) => seat)).toEqual([1, 2]);
    expect(formatGate.evaluate).toHaveBeenCalledTimes(3);
    expect(persistence.publishInitialDraftMatch).not.toHaveBeenCalled();
    expect(useDraftStore.getState().runState).toBeNull();
    expect(useDraftStore.getState().phase).toBe("launching");
    expect(navigate).not.toHaveBeenCalled();
  });

  it("tries the next bot after generation failure but aborts on gate transport or player refusal", async () => {
    await startLaunchWithBotSeats([]);
    vi.spyOn(Math, "random").mockReturnValue(0);
    wasm.get_bot_deck.mockImplementation((seat: number) => {
      if (seat === 1) throw new Error("card database unavailable");
      return { main_deck: ["Legal Bot"], lands: {} };
    });
    await useDraftStore.getState().launchMatch(vi.fn());
    expect(wasm.get_bot_deck.mock.calls.map(([seat]) => seat)).toEqual([1, 2]);
    expect(persistence.publishInitialDraftMatch).toHaveBeenCalledOnce();

    for (const failure of ["transport", "malformed", "player"] as const) {
      useDraftStore.getState().reset();
      await startLaunchWithBotSeats([1, 2]);
      wasm.get_bot_deck.mockReset();
      wasm.get_bot_deck.mockReturnValue({ main_deck: ["Legal Bot"], lands: {} });
      formatGate.evaluate.mockReset();
      formatGate.evaluate.mockImplementation(async (request: unknown) => {
        if (failure === "transport") throw new Error("gate transport failed");
        if (failure === "malformed") return {} as { compatible: boolean; reasons: string[] };
        return (request as { main_deck: string[] }).main_deck[0] === "Forest"
          ? { compatible: false, reasons: ["player rejected"] }
          : { compatible: true, reasons: [] };
      });
      persistence.publishInitialDraftMatch.mockClear();
      await expect(useDraftStore.getState().launchMatch(vi.fn())).rejects.toThrow(
        failure === "transport" ? "gate transport failed"
          : failure === "malformed" ? "Deck format validation is unavailable" : "player rejected",
      );
      expect(wasm.get_bot_deck).toHaveBeenCalledOnce();
      expect(persistence.publishInitialDraftMatch).not.toHaveBeenCalled();
    }
  });

  it("rematches a previously used legal seat after the unused seat fails", async () => {
    await startLaunchWithBotSeats([1, 2]);
    vi.spyOn(Math, "random").mockReturnValue(0);
    useDraftStore.getState().setRunFormat("run");
    const draftId = useDraftStore.getState().draftId!;
    const prior: DraftRunState = {
      format: "run", results: [{ gameId: "finished", result: "win" }],
      playerDeck: ["Forest"], opponentDeck: ["Old Bot"], usedBotSeats: [1],
    };
    useDraftStore.setState({ phase: "playing", runState: prior });
    persistence.loadDraftRun.mockResolvedValue(prior);
    wasm.get_bot_deck.mockImplementation((seat: number) => ({
      main_deck: [seat === 2 ? "Bad Bot" : "Returning Bot"], lands: {},
    }));
    formatGate.evaluate.mockImplementation(async (request: unknown) => ({
      compatible: (request as { main_deck: string[] }).main_deck[0] !== "Bad Bot",
      reasons: (request as { main_deck: string[] }).main_deck[0] === "Bad Bot" ? ["bad bot deck"] : [],
    }));
    const navigate = vi.fn();
    await useDraftStore.getState().launchNextMatch(navigate);
    expect(wasm.get_bot_deck.mock.calls.map(([seat]) => seat)).toEqual([2, 1]);
    expect(formatGate.evaluate).toHaveBeenCalledTimes(3);
    const published = (persistence.publishStagedDraftMatch.mock.calls as unknown as Array<[unknown]>)[0][0] as {
      run: DraftRunState; payload: { opponent: { main_deck: string[] } };
    };
    expect(published.run.activeMatch).toMatchObject({ draftId, botSeat: 1, resultCountAtLaunch: 1 });
    expect(published.run.usedBotSeats).toEqual([1]);
    expect(published.run.results).toEqual(prior.results);
    expect(published.payload.opponent.main_deck).toEqual(["Returning Bot"]);
    expect(navigate).toHaveBeenCalledOnce();
  });

  it("leaves a next-match run unchanged when every distinct bot seat is refused", async () => {
    await startLaunchWithBotSeats([1, 2, 2]);
    vi.spyOn(Math, "random").mockReturnValue(0);
    useDraftStore.getState().setRunFormat("run");
    const prior: DraftRunState = {
      format: "run", results: [{ gameId: "finished", result: "win" }],
      playerDeck: ["Forest"], opponentDeck: ["Old Bot"], usedBotSeats: [1],
    };
    useDraftStore.setState({ phase: "playing", runState: prior });
    persistence.loadDraftRun.mockResolvedValue(prior);
    wasm.get_bot_deck.mockImplementation((seat: number) => ({ main_deck: [`Bad Bot ${seat}`], lands: {} }));
    formatGate.evaluate.mockImplementation(async (request: unknown) => ({
      compatible: (request as { main_deck: string[] }).main_deck[0] === "Forest",
      reasons: (request as { main_deck: string[] }).main_deck[0] === "Forest" ? [] : ["no legal bot deck"],
    }));
    const navigate = vi.fn();
    await expect(useDraftStore.getState().launchNextMatch(navigate)).rejects.toThrow("no legal bot deck");
    expect(wasm.get_bot_deck.mock.calls.map(([seat]) => seat)).toEqual([2, 1]);
    expect(formatGate.evaluate).toHaveBeenCalledTimes(3);
    expect(persistence.publishStagedDraftMatch).not.toHaveBeenCalled();
    expect(useDraftStore.getState().runState).toBe(prior);
    expect(useDraftStore.getState().runState?.usedBotSeats).toEqual([1]);
    expect(useDraftStore.getState().runState?.results).toEqual(prior.results);
    expect(navigate).not.toHaveBeenCalled();
  });

  it("keeps Bo3 unpublished on the player's exact sideboard refusal even with another bot", async () => {
    await startLaunchWithBotSeats([1, 2]);
    useDraftStore.setState((state) => ({
      view: { ...state.view!, match_config: { match_type: "Bo3" } },
    }));
    useDraftStore.getState().setRunFormat("bo3");
    vi.spyOn(Math, "random").mockReturnValue(0);
    wasm.get_bot_deck.mockReturnValue({ main_deck: ["Legal Bot"], lands: {} });
    formatGate.evaluate.mockResolvedValue({ compatible: false, reasons: ["BO3 requires a sideboard"] });
    await expect(useDraftStore.getState().launchMatch(vi.fn())).rejects.toThrow("BO3 requires a sideboard");
    expect(wasm.get_bot_deck).toHaveBeenCalledOnce();
    expect(formatGate.evaluate).toHaveBeenCalledTimes(2);
    for (const [request] of formatGate.evaluate.mock.calls) {
      expect(request).toMatchObject({ selected_format: "Limited", selected_match_type: "Bo3", sideboard: [] });
    }
    expect(persistence.publishInitialDraftMatch).not.toHaveBeenCalled();
    expect(useDraftStore.getState().runFormat).toBe("bo3");
  });

  it("keeps a selected Bo3 first match unpublished and saves the choice before immediate resume", async () => {
    const pool = [card("human", "Forest")];
    wasm.start_quick_draft.mockReturnValue({ ...view(pool), match_config: { match_type: "Bo3" } });
    await useDraftStore.getState().startDraft("pool", "TST", "Test", 2);
    useDraftStore.setState({ phase: "launching" });
    wasm.get_bot_deck.mockReturnValue({ main_deck: ["Opponent"], lands: {} });
    persistence.loadDraftRun.mockResolvedValue(null);
    useDraftStore.getState().setRunFormat("bo3");
    formatGate.evaluate.mockImplementation(async (request: unknown) => ({
      compatible: (request as { selected_match_type: string }).selected_match_type !== "Bo3",
      reasons: (request as { selected_match_type: string }).selected_match_type === "Bo3"
        ? ["BO3 requires a sideboard"] : [],
    }));
    const navigate = vi.fn();
    await expect(useDraftStore.getState().launchMatch(navigate)).rejects.toThrow("BO3 requires a sideboard");
    expect(formatGate.evaluate).toHaveBeenCalledTimes(2);
    for (const [request] of formatGate.evaluate.mock.calls) {
      expect(request).toMatchObject({ selected_format: "Limited", selected_match_type: "Bo3", sideboard: [] });
    }
    expect(persistence.publishInitialDraftMatch).not.toHaveBeenCalled();
    expect(persistence.publishStagedDraftMatch).not.toHaveBeenCalled();
    expect(navigate).not.toHaveBeenCalled();
    expect(useDraftStore.getState().runFormat).toBe("bo3");
    expect(vi.getTimerCount()).toBe(0);
    expect(persistence.persistQuickDraftSnapshot).toHaveBeenCalledOnce();
    const snapshot = persistence.persistQuickDraftSnapshot.mock.calls[0];
    expect(snapshot[3].runFormat).toBe("bo3");
    expect(snapshot[2]).toMatchObject({ workspace: useDraftStore.getState().workspaceState });

    persistence.inspectActiveQuickDraftLifecycle.mockResolvedValue(snapshot[3]);
    persistence.loadQuickDraftSession.mockResolvedValue({
      sessionJson: snapshot[1], mainDeck: ["Forest"], landCounts: {}, poolSortMode: "color",
      poolPanelOpen: true, workspace: (snapshot[2] as { workspace: unknown }).workspace,
    });
    wasm.import_draft_session.mockReturnValue({ ...view(pool), status: "Pairing", match_config: { match_type: "Bo3" } });
    await useDraftStore.getState().resumeDraft();
    expect(useDraftStore.getState().phase).toBe("launching");
    expect(useDraftStore.getState().runFormat).toBe("bo3");
  });

  it("saves an immediate Bo3 choice when initial publication fails before the durable run write", async () => {
    const pool = [card("human", "Forest")];
    wasm.start_quick_draft.mockReturnValue({ ...view(pool), match_config: { match_type: "Bo3" } });
    await useDraftStore.getState().startDraft("pool", "TST", "Test", 2);
    useDraftStore.setState({ phase: "launching" });
    wasm.get_bot_deck.mockReturnValue({ main_deck: ["Opponent"], lands: {} });
    persistence.loadDraftRun.mockResolvedValue(null);
    useDraftStore.getState().setRunFormat("bo3");
    persistence.publishInitialDraftMatch.mockRejectedValueOnce(new Error("session write failed"));
    const snapshotWrite = deferred<void>();
    persistence.persistQuickDraftSnapshot.mockReturnValueOnce(snapshotWrite.promise);
    const navigate = vi.fn();
    let settled = false;
    const launch = useDraftStore.getState().launchMatch(navigate).finally(() => { settled = true; });

    await vi.waitFor(() => expect(persistence.persistQuickDraftSnapshot).toHaveBeenCalledOnce());
    expect(formatGate.evaluate).toHaveBeenCalledTimes(2);
    expect(persistence.publishInitialDraftMatch).toHaveBeenCalledOnce();
    expect(persistence.loadDraftRun).toHaveBeenCalledTimes(2);
    expect(settled).toBe(false);
    expect(vi.getTimerCount()).toBe(0);
    const snapshot = persistence.persistQuickDraftSnapshot.mock.calls[0];
    expect(snapshot[2]).toMatchObject({ phase: "launching", workspace: useDraftStore.getState().workspaceState });
    expect(snapshot[3]).toMatchObject({ phase: "launching", runFormat: "bo3" });
    snapshotWrite.resolve();
    await expect(launch).rejects.toThrow("session write failed");
    expect(navigate).not.toHaveBeenCalled();
    expect(useDraftStore.getState().phase).toBe("launching");
    expect(useDraftStore.getState().runState).toBeNull();

    persistence.inspectActiveQuickDraftLifecycle.mockResolvedValue(snapshot[3]);
    persistence.loadQuickDraftSession.mockResolvedValue({
      sessionJson: snapshot[1], mainDeck: ["Forest"], landCounts: {}, poolSortMode: "color",
      poolPanelOpen: true, workspace: (snapshot[2] as { workspace: unknown }).workspace,
    });
    wasm.import_draft_session.mockReturnValue({ ...view(pool), status: "Pairing", match_config: { match_type: "Bo3" } });
    await useDraftStore.getState().resumeDraft();
    expect(useDraftStore.getState().phase).toBe("launching");
    expect(useDraftStore.getState().runFormat).toBe("bo3");
    expect(useDraftStore.getState().runState).toBeNull();
  });

  it("publishes an immediate nondefault Sealed Full Run choice and reloads it from the durable run", async () => {
    vi.stubGlobal("fetch", vi.fn(async () => ({ text: async () => "database" })));
    const sealedView = { ...view([card("human", "Forest")]), kind: "Sealed" as const, status: "Deckbuilding" as const };
    wasm.start_sealed_draft.mockReturnValue(sealedView);
    await useDraftStore.getState().startSealedDraft("pool", "TST", "Test", 2);
    useDraftStore.getState().completeSealedOpening();
    wasm.submit_deck.mockReturnValue({ ...sealedView, status: "Pairing" });
    await useDraftStore.getState().submitDeck();
    expect(useDraftStore.getState().runFormat).toBe("single");
    wasm.get_bot_deck.mockReturnValue({ main_deck: ["Opponent"], lands: {} });
    persistence.loadDraftRun.mockResolvedValue(null);
    formatGate.evaluate.mockClear();
    useDraftStore.getState().setRunFormat("run");
    const navigate = vi.fn();
    await useDraftStore.getState().launchMatch(navigate);
    expect(formatGate.evaluate).toHaveBeenCalledTimes(2);
    for (const [request] of formatGate.evaluate.mock.calls) {
      expect(request).toMatchObject({ selected_format: "Limited", selected_match_type: "Bo1", sideboard: [] });
    }
    expect(persistence.persistQuickDraftSnapshot).not.toHaveBeenCalled();
    expect(persistence.publishInitialDraftMatch).toHaveBeenCalledOnce();
    const published = persistence.publishInitialDraftMatch.mock.calls[0][0] as unknown as {
      run: DraftRunState; meta: ActiveQuickDraftMeta; sessionJson: string;
      snapshot: { mainDeck: string[]; landCounts: Record<string, number>; poolSortMode: string;
        poolPanelOpen: boolean; workspace: unknown };
    };
    expect(published.run.format).toBe("run");
    expect(published.run.activeMatch?.format).toBe("run");
    expect(published.meta.runFormat).toBe("run");
    expect(navigate).toHaveBeenCalledWith(expect.stringContaining("match=bo1"));

    useDraftStore.getState().reset();
    persistence.inspectActiveQuickDraftLifecycle.mockResolvedValue(published.meta);
    persistence.loadQuickDraftSession.mockResolvedValue({ sessionJson: published.sessionJson, ...published.snapshot });
    persistence.loadDraftRun.mockResolvedValue(published.run);
    wasm.import_draft_session.mockReturnValue({ ...sealedView, status: "Pairing" });
    await useDraftStore.getState().resumeDraft();
    expect(useDraftStore.getState().runFormat).toBe("run");
    expect(useDraftStore.getState().runState?.format).toBe("run");
  });

  it("freezes the first-match choice across a pending gate and permits a new choice after rejection", async () => {
    wasm.start_quick_draft.mockReturnValue({ ...view([card("human", "Forest")]), match_config: { match_type: "Bo3" } });
    await useDraftStore.getState().startDraft("pool", "TST", "Test", 2);
    useDraftStore.setState({ phase: "launching" });
    wasm.get_bot_deck.mockReturnValue({ main_deck: ["Opponent"], lands: {} });
    persistence.loadDraftRun.mockResolvedValue(null);
    const pending = deferred<{ compatible: boolean; reasons: string[] }>();
    formatGate.evaluate.mockReturnValueOnce(pending.promise);
    const navigate = vi.fn();
    const launch = useDraftStore.getState().launchMatch(navigate);
    await vi.waitFor(() => expect(formatGate.evaluate).toHaveBeenCalledTimes(2));
    const timerCount = vi.getTimerCount();
    useDraftStore.getState().setRunFormat("bo3");
    expect(useDraftStore.getState().runFormat).toBe("run");
    expect(vi.getTimerCount()).toBe(timerCount);
    expect(persistence.publishInitialDraftMatch).not.toHaveBeenCalled();
    pending.resolve({ compatible: true, reasons: [] });
    await launch;
    expect(formatGate.evaluate.mock.calls.map(([request]) => (request as { selected_match_type: string }).selected_match_type)).toEqual(["Bo1", "Bo1"]);
    expect(persistence.publishInitialDraftMatch).toHaveBeenCalledOnce();
    const published = persistence.publishInitialDraftMatch.mock.calls[0][0] as unknown as {
      run: DraftRunState; meta: { runFormat: string };
    };
    expect(published.run.format).toBe("run");
    expect(published.run.activeMatch?.format).toBe("run");
    expect(published.meta.runFormat).toBe("run");
    expect(navigate).toHaveBeenCalledWith(expect.stringContaining("match=bo1"));

    useDraftStore.setState({ phase: "launching", runState: null });
    persistence.loadDraftRun.mockResolvedValue(null);
    formatGate.evaluate.mockImplementation(async (request: unknown) => ({
      compatible: (request as { selected_match_type: string }).selected_match_type !== "Bo3",
      reasons: (request as { selected_match_type: string }).selected_match_type === "Bo3" ? ["BO3 requires a sideboard"] : [],
    }));
    useDraftStore.getState().setRunFormat("bo3");
    expect(useDraftStore.getState().runFormat).toBe("bo3");
    await expect(useDraftStore.getState().launchMatch(navigate)).rejects.toThrow("BO3 requires a sideboard");
    expect(formatGate.evaluate).toHaveBeenLastCalledWith(expect.objectContaining({ selected_match_type: "Bo3", sideboard: [] }));
    expect(persistence.publishInitialDraftMatch).toHaveBeenCalledOnce();
  });

  it.each(["staged-retry", "next-match"] as const)("freezes %s through both exact Bo1 seat gates", async (branch) => {
    wasm.start_quick_draft.mockReturnValue({ ...view([card("human", "Forest")]), match_config: { match_type: "Bo3" } });
    await useDraftStore.getState().startDraft("pool", "TST", "Test", 2);
    const draftId = useDraftStore.getState().draftId!;
    const staged: DraftRunState = {
      format: "run", results: [], playerDeck: ["Forest"], opponentDeck: ["Opponent"], usedBotSeats: [1],
      activeMatch: { draftId, gameId: "staged", format: "run", resultCountAtLaunch: 0, botSeat: 1, opponentDeck: ["Opponent"] },
    };
    if (branch === "staged-retry") useDraftStore.setState({ phase: "launching" });
    else {
      staged.results = [{ gameId: "finished", result: "win" }];
      staged.activeMatch = undefined;
      useDraftStore.setState({ phase: "playing", runState: staged });
      wasm.get_bot_deck.mockReturnValue({ main_deck: ["New Opponent"], lands: {} });
    }
    persistence.loadDraftRun.mockResolvedValue(staged);
    const pending = deferred<{ compatible: boolean; reasons: string[] }>();
    formatGate.evaluate.mockReturnValueOnce(pending.promise);
    const navigate = vi.fn();
    const launch = branch === "staged-retry"
      ? useDraftStore.getState().launchMatch(navigate)
      : useDraftStore.getState().launchNextMatch(navigate);
    await vi.waitFor(() => expect(formatGate.evaluate).toHaveBeenCalledTimes(2));
    if (branch === "next-match") {
      await useDraftStore.getState().launchNextMatch(navigate);
      expect(formatGate.evaluate).toHaveBeenCalledTimes(2);
    }
    useDraftStore.getState().setRunFormat("bo3");
    expect(useDraftStore.getState().runFormat).toBe("run");
    expect(persistence.publishStagedDraftMatch).not.toHaveBeenCalled();
    pending.resolve({ compatible: true, reasons: [] });
    await launch;
    expect(formatGate.evaluate.mock.calls.map(([request]) => (request as { selected_match_type: string }).selected_match_type)).toEqual(["Bo1", "Bo1"]);
    const published = (persistence.publishStagedDraftMatch.mock.calls as unknown as Array<[unknown]>)[0][0] as {
      payload: { player: { main_deck: string[] }; opponent: { main_deck: string[] } };
      meta: { runFormat: string }; run?: DraftRunState;
    };
    expect(published.payload.player.main_deck).toEqual(["Forest"]);
    expect(published.payload.opponent.main_deck).toEqual(branch === "staged-retry" ? ["Opponent"] : ["New Opponent"]);
    expect(published.meta.runFormat).toBe("run");
    expect(published.run?.activeMatch?.format ?? staged.activeMatch?.format).toBe("run");
    expect(navigate).toHaveBeenCalledWith(expect.stringContaining("match=bo1"));
    expect(persistence.publishStagedDraftMatch).toHaveBeenCalledOnce();
  });

  it.each(["staged-retry", "next-match"] as const)("refuses empty-sideboard Bo3 %s before stage publication", async (branch) => {
    wasm.start_quick_draft.mockReturnValue({ ...view([card("human", "Forest")]), match_config: { match_type: "Bo3" } });
    await useDraftStore.getState().startDraft("pool", "TST", "Test", 2);
    const draftId = useDraftStore.getState().draftId!;
    const run: DraftRunState = {
      format: "bo3", results: [], playerDeck: ["Forest"], opponentDeck: ["Opponent"], usedBotSeats: [1],
      activeMatch: { draftId, gameId: "staged", format: "bo3", resultCountAtLaunch: 0, botSeat: 1, opponentDeck: ["Opponent"] },
    };
    useDraftStore.getState().setRunFormat("bo3");
    if (branch === "staged-retry") useDraftStore.setState({ phase: "launching" });
    else {
      run.results = [{ gameId: "finished", result: "draw" }];
      run.activeMatch = undefined;
      useDraftStore.setState({ phase: "playing", runState: run });
      wasm.get_bot_deck.mockReturnValue({ main_deck: ["New Opponent"], lands: {} });
    }
    persistence.loadDraftRun.mockResolvedValue(run);
    formatGate.evaluate.mockImplementation(async () => ({ compatible: false, reasons: ["BO3 requires a sideboard"] }));
    const navigate = vi.fn();
    const launch = branch === "staged-retry"
      ? useDraftStore.getState().launchMatch(navigate)
      : useDraftStore.getState().launchNextMatch(navigate);
    await expect(launch).rejects.toThrow("BO3 requires a sideboard");
    expect(formatGate.evaluate).toHaveBeenCalledTimes(2);
    for (const [request] of formatGate.evaluate.mock.calls) {
      expect(request).toMatchObject({ selected_format: "Limited", selected_match_type: "Bo3", sideboard: [] });
    }
    expect(persistence.publishStagedDraftMatch).not.toHaveBeenCalled();
    expect(navigate).not.toHaveBeenCalled();
    expect(useDraftStore.getState().runFormat).toBe("bo3");
  });

  it("rejects a non-action choice mutation after a positive gate without publishing", async () => {
    wasm.start_quick_draft.mockReturnValue({ ...view([card("human", "Forest")]), match_config: { match_type: "Bo3" } });
    await useDraftStore.getState().startDraft("pool", "TST", "Test", 2);
    useDraftStore.setState({ phase: "launching" });
    wasm.get_bot_deck.mockReturnValue({ main_deck: ["Opponent"], lands: {} });
    persistence.loadDraftRun.mockResolvedValue(null);
    const pending = deferred<{ compatible: boolean; reasons: string[] }>();
    formatGate.evaluate.mockReturnValueOnce(pending.promise);
    const navigate = vi.fn();
    const launch = useDraftStore.getState().launchMatch(navigate);
    await vi.waitFor(() => expect(formatGate.evaluate).toHaveBeenCalledTimes(2));
    useDraftStore.setState({ runFormat: "bo3" });
    pending.resolve({ compatible: true, reasons: [] });
    await expect(launch).rejects.toThrow("Stale draft match launch");
    expect(persistence.publishInitialDraftMatch).not.toHaveBeenCalled();
    expect(navigate).not.toHaveBeenCalled();
    expect(useDraftStore.getState().runFormat).toBe("bo3");
  });

  it("saves the current workspace on first-launch rejection and reports a failed choice save", async () => {
    await start([card("human", "Forest")]);
    useDraftStore.setState({ phase: "launching" });
    wasm.get_bot_deck.mockReturnValue({ main_deck: ["Opponent"], lands: {} });
    persistence.loadDraftRun.mockResolvedValue(null);
    const pending = deferred<{ compatible: boolean; reasons: string[] }>();
    formatGate.evaluate.mockReturnValueOnce(pending.promise);
    const launch = useDraftStore.getState().launchMatch(vi.fn());
    await vi.waitFor(() => expect(formatGate.evaluate).toHaveBeenCalledTimes(2));
    const currentWorkspace = { ...useDraftStore.getState().workspaceState! };
    useDraftStore.setState({ workspaceState: currentWorkspace });
    pending.resolve({ compatible: false, reasons: ["engine rejected"] });
    await expect(launch).rejects.toThrow("engine rejected");
    expect(persistence.persistQuickDraftSnapshot).toHaveBeenCalledOnce();
    expect(persistence.persistQuickDraftSnapshot).toHaveBeenCalledWith(
      expect.any(String), expect.any(String), expect.objectContaining({ workspace: currentWorkspace }), expect.any(Object),
    );

    persistence.persistQuickDraftSnapshot.mockClear();
    persistence.persistQuickDraftSnapshot.mockRejectedValueOnce(new Error("disk unavailable"));
    formatGate.evaluate.mockResolvedValueOnce({ compatible: false, reasons: ["engine rejected"] });
    await expect(useDraftStore.getState().launchMatch(vi.fn())).rejects.toThrow(
      "engine rejected; format choice could not be saved: disk unavailable",
    );
    expect(persistence.publishInitialDraftMatch).not.toHaveBeenCalled();
  });

  it("does not write an abandoned first-launch failure snapshot after session replacement", async () => {
    await start([card("human", "Forest")]);
    useDraftStore.setState({ phase: "launching" });
    wasm.get_bot_deck.mockReturnValue({ main_deck: ["Opponent"], lands: {} });
    persistence.loadDraftRun.mockResolvedValue(null);
    const pending = deferred<{ compatible: boolean; reasons: string[] }>();
    formatGate.evaluate.mockReturnValueOnce(pending.promise);
    const launch = useDraftStore.getState().launchMatch(vi.fn());
    await vi.waitFor(() => expect(formatGate.evaluate).toHaveBeenCalledTimes(2));
    useDraftStore.getState().reset();
    pending.resolve({ compatible: false, reasons: ["engine rejected"] });
    await expect(launch).rejects.toThrow("Stale draft match launch");
    expect(persistence.persistQuickDraftSnapshot).not.toHaveBeenCalled();
    expect(persistence.publishInitialDraftMatch).not.toHaveBeenCalled();
  });
});
