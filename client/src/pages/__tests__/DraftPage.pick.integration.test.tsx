// @vitest-environment happy-dom

import type { ReactNode } from "react";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { MemoryRouter } from "react-router";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { DraftCardInstance, DraftPlayerView } from "../../adapter/draft-adapter";
import { DRAFT_WORKSPACE_PREFERENCES_KEY } from "../../constants/storage";
import { createDefaultDraftWorkspacePreferences } from "../../components/draft/workspace/workspacePreferences";

const wasm = vi.hoisted(() => ({
  ...(() => {
    Object.assign(globalThis, {
      __DEFAULT_MULTIPLAYER_SERVER_URL__: "wss://lobby.phase-rs.dev/ws",
      __TELEMETRY_URL__: "",
    });
    return {};
  })(),
  default: vi.fn(async () => undefined),
  start_quick_draft: vi.fn(),
  load_card_database: vi.fn(() => 0),
  submit_pick: vi.fn(),
  submit_pick_with_draft_effect: vi.fn(),
  submit_deck: vi.fn(),
  auto_pick: vi.fn(),
  suggest_deck: vi.fn(),
  export_draft_session: vi.fn(() => "session"),
}));

const persistence = vi.hoisted(() => ({
  cleanupQuickDraftLifecycle: vi.fn(async () => undefined),
  drainQuickDraftPersistence: vi.fn(async () => undefined),
  inspectActiveQuickDraftLifecycle: vi.fn(async () => null),
  loadDraftRun: vi.fn(async () => null),
  loadQuickDraftSession: vi.fn(async () => null),
  persistQuickDraftSnapshot: vi.fn(async () => undefined),
  publishInitialDraftMatch: vi.fn(async () => undefined),
  publishStagedDraftMatch: vi.fn(async () => undefined),
  recordDraftMatchResult: vi.fn(async () => null),
  runLimits: vi.fn(() => ({ maxWins: 1, maxLosses: 1 })),
}));

vi.mock("@wasm/draft", () => wasm);
vi.mock("../../services/quickDraftPersistence", () => persistence);
vi.mock("../../services/engineRuntime", () => ({
  ensureCardDatabase: vi.fn(async () => 0),
  ensureCardLocale: vi.fn(async () => new Map()),
  getCardFaceData: vi.fn(async () => null),
  getCardParseDetails: vi.fn(async () => null),
  getCardRulings: vi.fn(async () => []),
}));
vi.mock("../../hooks/useCardImage", () => ({ useCardImage: () => ({ src: null, isLoading: false }) }));
vi.mock("../../components/chrome/ScreenChrome", () => ({ ScreenChrome: () => null }));
vi.mock("../../components/menu/MenuShell", () => ({ MenuShell: ({ children }: { children: ReactNode }) => <>{children}</> }));
vi.mock("../../components/draft/DraftSteps", () => ({ DraftSteps: () => null }));
vi.mock("../../components/draft/DraftProgress", () => ({ DraftProgress: () => null }));
vi.mock("../../components/card/HoverCardPreview", () => ({ HoverCardPreview: () => null }));
vi.mock("../../components/draft/BotDifficultySelector", () => ({ BotDifficultySelector: () => null }));
vi.mock("../../components/draft/CubeSetupPanel", () => ({ CubeSetupPanel: () => null }));
vi.mock("../../components/draft/SetSelector", () => ({ SetSelector: () => null }));
vi.mock("../../components/draft/LimitedDeckBuilder", () => ({ LimitedDeckBuilder: () => null }));
vi.mock("../../components/draft/SealedPackOpening", () => ({ SealedPackOpening: () => null }));
vi.mock("../../components/draft/DraftIntro", () => ({
  DraftIntro: ({ onContinue }: { onContinue(): void }) => <button type="button" onClick={onContinue}>Continue</button>,
}));

import { useDraftStore } from "../../stores/draftStore";
import { DraftPage } from "../DraftPage";

function card(instanceId: string): DraftCardInstance {
  return {
    instance_id: instanceId, name: instanceId, set_code: "TST", collector_number: instanceId,
    rarity: "common", colors: [], cmc: 1, type_line: "Card",
  };
}

function view({ pool = [], pack = [card("picked")], effects = [] }: { pool?: DraftCardInstance[]; pack?: DraftCardInstance[]; effects?: DraftCardInstance[] } = {}): DraftPlayerView {
  return {
    status: "Drafting", kind: "Quick", launch_capability: "None", commanders_required: 0, pool, current_pack: pack, draft_effects: effects,
    pool_groups: {
      color_groups: [], type_groups: [], cmc_groups: [], rarity_groups: [],
      type_filter_options: [], color_filter_options: [],
      color_counts: { white: 0, blue: 0, black: 0, red: 0, green: 0 },
      workspace_capabilities: { rarity_group_order: null },
      workspace_row_classification: { creature_instance_ids: [], noncreature_instance_ids: [] },
    },
    seats: [], current_pack_number: 1, pick_number: 1, pass_direction: "Left",
    cards_per_pack: 14, required_pick_count: 1, pick_selection_mode: "Direct", pick_steps_per_pack: 14, pack_count: 3, min_deck_size: 40, addable_cards: [],
    timer_remaining_ms: null, standings: [], current_round: 0, next_pairing_round: 1, tournament_format: "Swiss",
    pod_policy: "Casual", pairings: [], match_config: { match_type: "Bo1" },
  } as DraftPlayerView;
}

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((resolvePromise) => { resolve = resolvePromise; });
  return { promise, resolve };
}

async function renderDraft(initialView = view()) {
  wasm.start_quick_draft.mockReturnValue(initialView);
  await useDraftStore.getState().startDraft("pool", "TST", "Test", 2);
  render(<MemoryRouter><DraftPage /></MemoryRouter>);
  fireEvent.click(screen.getByRole("button", { name: "Continue" }));
  await screen.findByTestId("pack-sequence");
}

function prepareDrag(instanceId = "picked") {
  const source = document.querySelector<HTMLElement>(`[data-instance-id="${instanceId}"]`)!;
  const target = document.querySelector<HTMLElement>('[data-drop-target="collapsed-sideboard"]')!;
  expect(source).not.toBeNull();
  expect(target).not.toBeNull();
  expect(target.querySelector('section[data-zone="sideboard"]')).not.toBeNull();
  source.setPointerCapture = vi.fn();
  source.releasePointerCapture = vi.fn();
  target.getBoundingClientRect = () => ({
    left: 0, top: 0, right: 300, bottom: 300, width: 300, height: 300,
    x: 0, y: 0, toJSON: () => ({}),
  } as DOMRect);
  return { source, target, release: source.releasePointerCapture as ReturnType<typeof vi.fn> };
}

function dragToTarget(source: HTMLElement, target: HTMLElement) {
  fireEvent.pointerDown(source, { button: 0, clientX: 10, clientY: 10, isPrimary: true, pointerId: 12, pointerType: "mouse" });
  expect(source.setPointerCapture).toHaveBeenCalledWith(12);
  fireEvent.pointerMove(source, { clientX: 30, clientY: 30, pointerId: 12, pointerType: "mouse" });
  expect(target).toHaveAttribute("data-drop-state", "active");
  fireEvent.pointerUp(source, { clientX: 30, clientY: 30, pointerId: 12, pointerType: "mouse" });
}

describe("DraftPage production pick integration", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    useDraftStore.getState().reset();
    localStorage.setItem(DRAFT_WORKSPACE_PREFERENCES_KEY, JSON.stringify({
      ...createDefaultDraftWorkspacePreferences(), explicitView: "board", sideboardCollapsed: true,
    }));
  });

  afterEach(() => {
    cleanup();
    useDraftStore.getState().reset();
    localStorage.clear();
  });

  it("drags_a_rendered_pack_card_through_the_real_store_to_the_collapsed_sideboard_column", async () => {
    const result = deferred<DraftPlayerView>();
    wasm.submit_pick.mockReturnValue(result.promise);
    await renderDraft();
    const { source, target, release } = prepareDrag();

    dragToTarget(source, target);
    await waitFor(() => expect(wasm.submit_pick).toHaveBeenCalledWith("picked"));
    expect(release).toHaveBeenCalledTimes(1);
    expect(useDraftStore.getState()).toMatchObject({ pickInteractionLocked: true });
    expect(useDraftStore.getState().workspaceState?.placements.picked).toBeUndefined();

    await act(async () => result.resolve(view({ pool: [card("picked")], pack: [] })));
    await waitFor(() => expect(useDraftStore.getState().workspaceState?.placements.picked).toMatchObject({ zone: "sideboard", column: 0 }));
    expect(document.querySelector('[data-instance-id="picked"][data-visual-state="leaving"]')).not.toBeNull();
  });

  it("joins_owned_unlock_with_explicit_rejection_without_inserting_or_leaving", async () => {
    const result = deferred<DraftPlayerView>();
    wasm.submit_pick.mockReturnValue(result.promise);
    await renderDraft();
    const { source, target, release } = prepareDrag();
    dragToTarget(source, target);
    expect(useDraftStore.getState().pickInteractionLocked).toBe(true);

    await act(async () => result.resolve(view()));
    await waitFor(() => expect(document.querySelector('[data-instance-id="picked"]')?.getAttribute("data-visual-state")).toBe("failure-restored"));
    expect(release).toHaveBeenCalledTimes(1);
    expect(useDraftStore.getState().workspaceState?.placements.picked).toBeUndefined();
    expect(document.querySelector('[data-visual-state="leaving"]')).toBeNull();
  });

  it("settles_clean_unowned_busy_without_a_lock_edge_or_failure_state", async () => {
    const submission = deferred<DraftPlayerView>();
    wasm.submit_deck.mockReturnValue(submission.promise);
    await renderDraft();
    const privateOperation = useDraftStore.getState().submitDeck();
    await waitFor(() => expect(wasm.submit_deck).toHaveBeenCalledOnce());
    const { source, target, release } = prepareDrag();
    dragToTarget(source, target);

    await waitFor(() => expect(document.querySelector('[data-instance-id="picked"]')?.getAttribute("data-visual-state")).toBe("selected"));
    expect(wasm.submit_pick).not.toHaveBeenCalled();
    expect(release).toHaveBeenCalledTimes(1);
    expect(useDraftStore.getState()).toMatchObject({ pickInteractionLocked: false, pendingPickIntent: null });
    expect(useDraftStore.getState().workspaceState?.placements.picked).toBeUndefined();

    submission.resolve(view());
    await privateOperation;
  });

  it.each([
    ["pre-existing", [card("picked")], [card("picked")], "deck"],
    ["duplicate", [], [card("picked"), card("picked")], undefined],
    ["missing", [], [card("other")], undefined],
    ["unchanged", [card("existing")], [card("existing")], undefined],
  ] as const)("rejects_%s_adapter_acknowledgment_through_the_real_page", async (_label, beforePool, afterPool, existingZone) => {
    wasm.submit_pick.mockReturnValue(view({ pool: [...afterPool] }));
    await renderDraft(view({ pool: [...beforePool] }));
    const workspaceBefore = useDraftStore.getState().workspaceState;
    const intents: unknown[] = [];
    const unsubscribe = useDraftStore.subscribe((state) => {
      if (state.pendingPickIntent !== null) intents.push(state.pendingPickIntent);
    });
    const { source, target, release } = prepareDrag();

    dragToTarget(source, target);
    await waitFor(() => expect(wasm.submit_pick).toHaveBeenCalledWith("picked"));
    await waitFor(() => expect(document.querySelector('[data-instance-id="picked"]')?.getAttribute("data-visual-state")).toBe("failure-restored"));
    unsubscribe();

    expect(intents).toContainEqual({ kind: "pick", instanceIds: ["picked"], destination: "sideboard", placementHint: { column: 0 } });
    expect(release).toHaveBeenCalledTimes(1);
    expect(useDraftStore.getState()).toMatchObject({ pickInteractionLocked: false, pendingPickIntent: null });
    expect(useDraftStore.getState().workspaceState).toBe(workspaceBefore);
    expect(useDraftStore.getState().workspaceState?.placements.picked?.zone).toBe(existingZone);
    expect(document.querySelector('[data-visual-state="leaving"]')).toBeNull();
  });

  it("rejects_partial_two_card_effect_atomically_through_the_real_page", async () => {
    const effect = card("effect");
    const first = card("first");
    const second = card("second");
    const initialView = view({ pool: [effect], pack: [first, second], effects: [effect] });
    wasm.submit_pick_with_draft_effect.mockReturnValue(view({ pool: [effect, first], pack: [first, second], effects: [effect] }));
    await renderDraft(initialView);
    const workspaceBefore = useDraftStore.getState().workspaceState;
    fireEvent.click(screen.getByRole("checkbox", { name: "effect" }));
    fireEvent.click(screen.getByRole("button", { name: "first" }));
    fireEvent.click(screen.getByRole("button", { name: "second" }));
    const { source, target, release } = prepareDrag("first");

    dragToTarget(source, target);
    await waitFor(() => expect(wasm.submit_pick_with_draft_effect).toHaveBeenCalledWith("effect", JSON.stringify(["first", "second"])));
    await waitFor(() => expect(document.querySelector('[data-instance-id="first"]')?.getAttribute("data-visual-state")).toBe("failure-restored"));

    expect(document.querySelector('[data-instance-id="second"]')).toHaveAttribute("data-visual-state", "failure-restored");
    expect(release).toHaveBeenCalledTimes(1);
    expect(useDraftStore.getState()).toMatchObject({ pickInteractionLocked: false, pendingPickIntent: null });
    expect(useDraftStore.getState().workspaceState).toBe(workspaceBefore);
    expect(useDraftStore.getState().workspaceState?.placements.first).toBeUndefined();
    expect(useDraftStore.getState().workspaceState?.placements.second).toBeUndefined();
    expect(document.querySelector('[data-visual-state="leaving"]')).toBeNull();
  });

  it("ignores_a_stale_adapter_result_after_synchronous_lifecycle_replacement", async () => {
    await renderDraft();
    wasm.submit_pick.mockImplementation(() => {
      useDraftStore.getState().reset();
      return view({ pool: [card("picked")], pack: [] });
    });
    const { source, target, release } = prepareDrag();

    dragToTarget(source, target);
    await waitFor(() => expect(wasm.submit_pick).toHaveBeenCalledWith("picked"));
    await waitFor(() => expect(useDraftStore.getState().phase).toBe("setup"));

    expect(release).toHaveBeenCalledTimes(1);
    expect(useDraftStore.getState()).toMatchObject({ workspaceState: null, pickInteractionLocked: false, pendingPickIntent: null });
    expect(document.querySelector('[data-visual-state="failure-restored"]')).toBeNull();
    expect(document.querySelector('[data-visual-state="leaving"]')).toBeNull();
  });

  it("keeps_a_new_generation_authoritative_when_the_old_adapter_result_arrives", async () => {
    const oldResult = deferred<DraftPlayerView>();
    wasm.submit_pick.mockReturnValue(oldResult.promise);
    await renderDraft();
    const { source, target, release } = prepareDrag();
    dragToTarget(source, target);
    await waitFor(() => expect(useDraftStore.getState().pickInteractionLocked).toBe(true));
    const oldGeneration = useDraftStore.getState().interactionGeneration;

    act(() => useDraftStore.getState().reset());
    const replacementGeneration = useDraftStore.getState().interactionGeneration;
    expect(replacementGeneration).toBeGreaterThan(oldGeneration);
    expect(useDraftStore.getState()).toMatchObject({ workspaceState: null, pickInteractionLocked: false, pendingPickIntent: null });

    await act(async () => oldResult.resolve(view({ pool: [card("picked")], pack: [] })));
    expect(release).toHaveBeenCalledTimes(1);
    expect(useDraftStore.getState()).toMatchObject({ interactionGeneration: replacementGeneration, pickInteractionLocked: false, pendingPickIntent: null });
    expect(useDraftStore.getState().workspaceState).toBeNull();

    wasm.start_quick_draft.mockReturnValue(view({ pack: [card("replacement")] }));
    await act(async () => useDraftStore.getState().startDraft("pool", "TST", "Replacement", 2));
    expect(document.querySelector('[data-instance-id="replacement"]')).not.toBeNull();
    expect(useDraftStore.getState().view?.current_pack?.map((entry) => entry.instance_id)).toEqual(["replacement"]);
    expect(useDraftStore.getState().workspaceState?.placements.replacement).toBeUndefined();
    expect(useDraftStore.getState().workspaceState?.placements.picked).toBeUndefined();
    expect(document.querySelector('[data-instance-id="replacement"]')).not.toHaveAttribute("data-visual-state", "failure-restored");
    expect(document.querySelector('[data-visual-state="leaving"]')).toBeNull();
  });
});
