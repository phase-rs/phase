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
}));

const persistence = vi.hoisted(() => ({
  cleanupQuickDraftLifecycle: vi.fn(async () => undefined),
  drainQuickDraftPersistence: vi.fn(async () => undefined),
  inspectActiveQuickDraftLifecycle: vi.fn<() => Promise<unknown>>(async () => null),
  loadDraftRun: vi.fn<() => Promise<unknown>>(async () => null),
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

vi.mock("@wasm/draft", () => wasm);
vi.mock("../../services/quickDraftPersistence", () => persistence);

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

function view(pool: DraftCardInstance[] = []): DraftPlayerView {
  return {
    status: "Drafting",
    kind: "Quick",
    launch_capability: "None",
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
    seats: [],
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

async function settleTimers(): Promise<void> {
  await vi.runAllTimersAsync();
  await Promise.resolve();
}

describe("draft store workspace authority", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    vi.clearAllMocks();
    persistence.inspectActiveQuickDraftLifecycle.mockResolvedValue(null);
    useDraftStore.getState().reset();
  });

  afterEach(() => {
    vi.useRealTimers();
    vi.restoreAllMocks();
    vi.unstubAllGlobals();
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

  it("routes_resume_suggestions_and_submit_through_workspace_installation", async () => {
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

  it("reuses a durable initial launch stage after publication failure", async () => {
    const draftView = view([card("spell")]);
    draftView.seats = [{
      seat_index: 1,
      display_name: "Bot",
      is_bot: true,
      connected: true,
      has_submitted_deck: true,
      pick_status: "NotDrafting",
      active_pack_count: 0,
      face_up_draft_cards: [],
    }];
    wasm.start_quick_draft.mockReturnValue(draftView);
    await useDraftStore.getState().startDraft("pool", "TST", "Test", 2);
    wasm.get_bot_deck.mockReturnValue({ main_deck: ["Opponent"], lands: {} });
    const randomUuid = vi.spyOn(crypto, "randomUUID")
      .mockReturnValue("00000000-0000-4000-8000-000000000123");
    let stagedRun: unknown;
    persistence.publishInitialDraftMatch.mockImplementationOnce(async (input: { run: unknown }) => {
      stagedRun = input.run;
      throw new Error("metadata failed");
    });
    const navigate = vi.fn();

    await expect(useDraftStore.getState().launchMatch(navigate)).rejects.toThrow("metadata failed");
    persistence.loadDraftRun.mockResolvedValueOnce(stagedRun);
    await useDraftStore.getState().launchMatch(navigate);

    expect(randomUuid).toHaveBeenCalledOnce();
    expect(wasm.get_bot_deck).toHaveBeenCalledOnce();
    expect(wasm.export_draft_session).toHaveBeenCalledOnce();
    expect(persistence.publishStagedDraftMatch).toHaveBeenCalledOnce();
    expect(navigate).toHaveBeenCalledWith(expect.stringContaining("00000000-0000-4000-8000-000000000123"));
  });
});
