import { readFile } from "node:fs/promises";
import { resolve } from "node:path";
import { gunzipSync } from "node:zlib";

import { act, cleanup, fireEvent, render } from "@testing-library/react";
import type { ReactNode } from "react";
import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import init, {
  export_game_state_json,
  initialize_game,
  load_card_database,
  submit_action,
} from "@wasm/engine";

import type { DebugAction, GameAction, GameState, ObjectId, PlayerId } from "../../../adapter/types.ts";
import { WasmAdapter } from "../../../adapter/wasm-adapter.ts";
import { dispatchAction } from "../../../game/dispatch.ts";
import { clearPromptOverlayState } from "../../../game/sessionCleanup.ts";
import { useGameStore } from "../../../stores/gameStore.ts";
import { usePreferencesStore } from "../../../stores/preferencesStore.ts";
import { useUiStore } from "../../../stores/uiStore.ts";
import { GameBoard } from "../GameBoard.tsx";

/*
 * Issue #6016 — the saved Manual payment preference, end to end against the
 * REAL engine. The board is the real `GameBoard` (whose provider publishes the
 * engine's legal-action affordances) with real `PermanentCard`s; a click goes
 * through `resolveObjectActivation` → `dispatchAction` →
 * `applySpellPaymentPreference` → the real `WasmAdapter` (its supported
 * main-thread fallback, since the test DOM has no Worker) → the compiled
 * `submit_action`. Nothing on that path is mocked: the `ManaPayment` window, the
 * pending activation and every tap below are the compiled engine's own output.
 * Only display-only chrome that is unrelated to the battlefield is stubbed.
 *
 * The cards are the canonical integration export (exact Oracle text and the
 * exported `CardFace` JSON); the board mirrors the engine's own Speaker rows in
 * `crates/engine/tests/integration/activation_manual_mana_payment.rs`.
 *
 * Integration lane: needs `./scripts/build-wasm.sh wasm-dev` first, then
 * `pnpm exec vitest run --config vitest.integration.config.ts <this file>`.
 */

vi.mock("../../card/CardImage.tsx", () => ({
  CardImage: ({ cardName }: { cardName: string }) => <div aria-label={cardName} />,
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

const REPO_ROOT = resolve(__dirname, "../../../../..");
const WASM_PATH = resolve(REPO_ROOT, "client/src/wasm/engine_wasm_bg.wasm");
const CARD_FIXTURE_PATH = resolve(REPO_ROOT, "crates/engine/tests/fixtures/integration_cards.json.gz");
const CARD_NAMES = ["Formidable Speaker", "Grizzly Bears", "Forest", "Mountain", "Sol Ring"];

const HUMAN: PlayerId = 0;
const OPPONENT: PlayerId = 1;
const MANUAL = { type: "Manual" } as const;

type ActivateAbilityAction = Extract<GameAction, { type: "ActivateAbility" }>;

function isActivation(action: GameAction): action is ActivateAbilityAction {
  return action.type === "ActivateAbility";
}

/** The exported faces, verbatim, for exactly the cards this board uses. */
async function loadExportedCards(): Promise<void> {
  const all = JSON.parse(gunzipSync(await readFile(CARD_FIXTURE_PATH)).toString("utf8")) as Record<string, unknown>;
  const subset = Object.fromEntries(CARD_NAMES.map((name) => {
    const face = all[name.toLowerCase()];
    if (!face) throw new Error(`${name} is missing from the integration card export`);
    return [name.toLowerCase(), face];
  }));
  expect(load_card_database(JSON.stringify(subset))).toBe(CARD_NAMES.length);
}

/** Test-setup only: a debug action submitted straight to the engine. */
function setupDebug(action: DebugAction, actor: PlayerId = HUMAN): void {
  const outcome = submit_action(actor, { type: "Debug", data: action }) as { status: string };
  if (outcome.status !== "applied") {
    throw new Error(`setup ${action.type} was refused: ${JSON.stringify(outcome)}`);
  }
}

function engineState(): GameState {
  return (JSON.parse(export_game_state_json()) as { state: GameState }).state;
}

function createPermanent(name: string, owner: PlayerId): ObjectId {
  const before = new Set(engineState().battlefield);
  setupDebug({
    type: "CreateCard",
    data: {
      card_name: name,
      owner,
      zone: "Battlefield",
      run_etb: false,
      nonlegendary: false,
      creation_kind: "Card",
      count: 1,
    },
  });
  const state = engineState();
  const created = state.battlefield.filter((id) => !before.has(id));
  expect(created).toHaveLength(1);
  expect(state.objects[created[0]].name).toBe(name);
  return created[0];
}

interface SeatBoard {
  speaker: ObjectId;
  bears: ObjectId;
  forest: ObjectId;
  mountain: ObjectId;
  solRing: ObjectId;
}

/**
 * Formidable Speaker already on the battlefield (its enters trigger is not
 * replayed), two tapped permanents it can untap, a Mountain to pay with and an
 * unused Sol Ring — for `controller`, during `activePlayer`'s main phase.
 */
function seedBoard(controller: PlayerId, activePlayer: PlayerId): SeatBoard {
  const started = initialize_game(null, 7, null, null, 2, HUMAN) as { error?: boolean; reasons?: string[] };
  expect(started.error, started.reasons?.join("; ")).not.toBe(true);
  setupDebug({ type: "SetPhase", data: { phase: "PreCombatMain", active_player: activePlayer } });
  const board: SeatBoard = {
    speaker: createPermanent("Formidable Speaker", controller),
    bears: createPermanent("Grizzly Bears", controller),
    forest: createPermanent("Forest", controller),
    mountain: createPermanent("Mountain", controller),
    solRing: createPermanent("Sol Ring", controller),
  };
  // Fixture preconditions: the Speaker can {T}, and both permanents it could
  // untap start tapped.
  setupDebug({ type: "SetSummoningSickness", data: { object_id: board.speaker, sick: false } });
  setupDebug({ type: "SetTapped", data: { object_id: board.bears, tapped: true } });
  setupDebug({ type: "SetTapped", data: { object_id: board.forest, tapped: true } });
  const state = engineState();
  expect(state.stack).toHaveLength(0);
  expect(state.waiting_for).toEqual({ type: "Priority", data: { player: activePlayer } });
  return board;
}

/** The real adapter, spied (calls pass through) and committed to the store. */
async function installRealAdapter() {
  const adapter = new WasmAdapter();
  await adapter.initialize();
  const submitAction = vi.spyOn(adapter, "submitAction");
  const snapshot = await adapter.getSnapshot();
  act(() => {
    useGameStore.setState({ adapter, gameMode: "ai" });
    useGameStore.getState().commitEngineSnapshot(snapshot);
  });
  return { adapter, submitAction };
}

/** Persist a saved payment preference and restore it through Zustand persist. */
async function rehydrateSavedPreference(spellPaymentMode: "manual" | "auto"): Promise<void> {
  // Start the live store on the opposite mode, so only the rehydrate can set it.
  // (Any store write persists, so this precedes writing the saved entry.)
  act(() => usePreferencesStore.setState({ spellPaymentMode: spellPaymentMode === "manual" ? "auto" : "manual" }));
  const persisted = { ...usePreferencesStore.getState(), spellPaymentMode };
  localStorage.setItem(
    "phase-preferences",
    JSON.stringify({ state: persisted, version: usePreferencesStore.persist.getOptions().version }),
  );
  await act(async () => {
    await usePreferencesStore.persist.rehydrate();
  });
  expect(usePreferencesStore.getState().spellPaymentMode).toBe(spellPaymentMode);
}

function renderBoard() {
  return render(
    <GameBoard effectiveMultiplayerBoardLayout="focused" oppHud={<div />} playerHud={<div />} />,
  );
}

function permanent(container: HTMLElement, id: ObjectId): HTMLElement {
  const element = container.querySelector(`[data-object-id="${id}"]`);
  if (!(element instanceof HTMLElement)) throw new Error(`permanent ${id} is not on the rendered board`);
  return element;
}

function storeState(): GameState {
  const state = useGameStore.getState().gameState;
  if (!state) throw new Error("no committed game state");
  return state;
}

function isTapped(id: ObjectId): boolean {
  return storeState().objects[id].tapped;
}

function poolSize(player: PlayerId): number {
  return storeState().players[player].mana_pool.mana.length;
}

async function waitForPrompt(type: string, player: PlayerId): Promise<void> {
  await vi.waitFor(() => {
    const waitingFor = useGameStore.getState().waitingFor;
    expect(waitingFor?.type).toBe(type);
    expect((waitingFor as { data: { player: PlayerId } }).data.player).toBe(player);
  });
}

/** The engine's one offered (unstamped) activation for `source` in the current snapshot. */
function offeredActivation(source: ObjectId): ActivateAbilityAction {
  const offered = (useGameStore.getState().legalActionsByObject[String(source)] ?? []).filter(isActivation);
  expect(offered).toHaveLength(1);
  return offered[0];
}

/** Click the Speaker, then its target, through the real board. */
async function activateSpeakerAtBears(container: HTMLElement, board: SeatBoard): Promise<void> {
  fireEvent.click(permanent(container, board.speaker));
  await waitForPrompt("TargetSelection", HUMAN);
  fireEvent.click(permanent(container, board.bears));
}

/** Resolve the top of the stack: the human passes through the store, the AI seat through dispatch. */
async function resolveTop(): Promise<void> {
  const depth = storeState().stack.length;
  await act(async () => {
    await useGameStore.getState().dispatch({ type: "PassPriority" });
  });
  await act(async () => {
    await dispatchAction({ type: "PassPriority" }, OPPONENT);
  });
  await vi.waitFor(() => expect(storeState().stack.length).toBe(depth - 1));
}

describe("PermanentCard activation under a saved Manual preference (real WASM)", () => {
  beforeAll(async () => {
    const module = await WebAssembly.compile(await readFile(WASM_PATH));
    await init({ module_or_path: module });
    await loadExportedCards();
  }, 300_000);

  beforeEach(() => {
    // The test DOM has no Worker; the adapter takes its supported main-thread
    // fallback over the same compiled module initialized above.
    vi.stubGlobal("Worker", undefined);
    vi.spyOn(console, "warn").mockImplementation(() => {});
    window.matchMedia = ((query: string) => ({
      matches: query === "(hover: hover)" || query === "(any-hover: hover)",
      media: query,
      onchange: null,
      addEventListener: vi.fn(),
      removeEventListener: vi.fn(),
      addListener: vi.fn(),
      removeListener: vi.fn(),
      dispatchEvent: vi.fn(),
    })) as unknown as typeof window.matchMedia;
    act(() => {
      usePreferencesStore.setState({
        spellPaymentMode: "auto",
        animationSpeedMultiplier: 0,
        battlefieldCardDisplay: "full_card",
        showKeywordStrip: false,
      });
      useUiStore.setState({ manualManaOverride: false, pendingAbilityChoice: null });
    });
  });

  afterEach(() => {
    cleanup();
    act(() => {
      clearPromptOverlayState();
      useGameStore.getState().reset();
    });
    localStorage.clear();
    vi.unstubAllGlobals();
    vi.restoreAllMocks();
  });

  it("a saved Manual preference opens the Speaker's payment window after its target, and the player's own tap pays it", async () => {
    await rehydrateSavedPreference("manual");
    expect(useUiStore.getState().manualManaOverride).toBe(false);
    const board = seedBoard(HUMAN, HUMAN);
    const { submitAction } = await installRealAdapter();
    const offered = offeredActivation(board.speaker);
    const { container } = renderBoard();

    await activateSpeakerAtBears(container, board);

    // CR 602.2b + CR 601.2c + CR 601.2g: target first, then the manual window.
    await waitForPrompt("ManaPayment", HUMAN);
    // The click reached the real adapter as the offered activation, stamped Manual.
    expect(submitAction).toHaveBeenNthCalledWith(
      1,
      { ...offered, data: { ...offered.data, payment_mode: MANUAL } },
      HUMAN,
    );
    expect(submitAction).toHaveBeenNthCalledWith(
      2,
      { type: "ChooseTarget", data: { target: { Object: board.bears } } },
      HUMAN,
    );
    const pending = storeState().pending_cast;
    expect(pending?.object_id).toBe(board.speaker);
    expect(pending?.activation_ability_index).toBe(offered.data.ability_index);
    expect(storeState().stack).toHaveLength(0);
    expect(isTapped(board.speaker)).toBe(false);
    expect(isTapped(board.mountain)).toBe(false);
    expect(isTapped(board.solRing)).toBe(false);

    // The player pays with the Mountain they choose, on the real board.
    fireEvent.click(permanent(container, board.mountain));
    await vi.waitFor(() => expect(isTapped(board.mountain)).toBe(true));
    expect(useGameStore.getState().waitingFor?.type).toBe("ManaPayment");
    await act(async () => {
      await useGameStore.getState().dispatch({ type: "PassPriority" });
    });

    await waitForPrompt("Priority", HUMAN);
    expect(storeState().stack).toHaveLength(1);
    expect(storeState().stack[0].source_id).toBe(board.speaker);
    expect(storeState().pending_cast ?? null).toBeNull();
    expect(isTapped(board.speaker)).toBe(true);
    expect(isTapped(board.solRing)).toBe(false);

    await resolveTop();
    expect(isTapped(board.bears)).toBe(false);
    expect(isTapped(board.forest)).toBe(true);
  });

  it("the per-game Manual override opens the same window under a saved Auto preference", async () => {
    await rehydrateSavedPreference("auto");
    act(() => useUiStore.setState({ manualManaOverride: true }));
    const board = seedBoard(HUMAN, HUMAN);
    const { submitAction } = await installRealAdapter();
    const offered = offeredActivation(board.speaker);
    const { container } = renderBoard();

    await activateSpeakerAtBears(container, board);

    await waitForPrompt("ManaPayment", HUMAN);
    expect(submitAction).toHaveBeenNthCalledWith(
      1,
      { ...offered, data: { ...offered.data, payment_mode: MANUAL } },
      HUMAN,
    );
    expect(storeState().pending_cast?.object_id).toBe(board.speaker);
    expect(storeState().stack).toHaveLength(0);
    expect(isTapped(board.mountain)).toBe(false);
    expect(isTapped(board.solRing)).toBe(false);
  });

  it("a saved Auto preference with the override off pays the Speaker itself after its target", async () => {
    await rehydrateSavedPreference("auto");
    const board = seedBoard(HUMAN, HUMAN);
    const { submitAction } = await installRealAdapter();
    const offered = offeredActivation(board.speaker);
    const { container } = renderBoard();

    await activateSpeakerAtBears(container, board);

    expect(submitAction).toHaveBeenNthCalledWith(1, offered, HUMAN);
    await waitForPrompt("Priority", HUMAN);
    expect(storeState().stack).toHaveLength(1);
    expect(storeState().stack[0].source_id).toBe(board.speaker);
    expect(isTapped(board.speaker)).toBe(true);
    expect(isTapped(board.mountain) || isTapped(board.solRing)).toBe(true);
    expect(submitAction.mock.calls.map(([action]) => action.type)).toEqual(["ActivateAbility", "ChooseTarget"]);
  });

  it("a mana ability clicked at priority under saved Manual adds mana and opens no window", async () => {
    await rehydrateSavedPreference("manual");
    const board = seedBoard(HUMAN, HUMAN);
    const { submitAction } = await installRealAdapter();
    const offeredMana = offeredActivation(board.solRing);
    const { container } = renderBoard();
    expect(poolSize(HUMAN)).toBe(0);

    fireEvent.click(permanent(container, board.solRing));

    await vi.waitFor(() => expect(isTapped(board.solRing)).toBe(true));
    // The mana ability is stamped Manual like any activation; the engine, which
    // owns the mana-ability classification (CR 605.1a), still just adds mana.
    expect(submitAction).toHaveBeenCalledExactlyOnceWith(
      { ...offeredMana, data: { ...offeredMana.data, payment_mode: MANUAL } },
      HUMAN,
    );
    await waitForPrompt("Priority", HUMAN);
    expect(poolSize(HUMAN)).toBe(2);
    expect(storeState().pending_cast ?? null).toBeNull();
    expect(storeState().stack).toHaveLength(0);
  });

  it("a mana ability clicked inside the Speaker's window pays into it without replacing the pending activation", async () => {
    await rehydrateSavedPreference("manual");
    const board = seedBoard(HUMAN, HUMAN);
    const { submitAction } = await installRealAdapter();
    const { container } = renderBoard();

    await activateSpeakerAtBears(container, board);
    await waitForPrompt("ManaPayment", HUMAN);
    const offeredMana = offeredActivation(board.solRing);
    const callsBeforeMana = submitAction.mock.calls.length;

    fireEvent.click(permanent(container, board.solRing));

    await vi.waitFor(() => expect(isTapped(board.solRing)).toBe(true));
    expect(submitAction).toHaveBeenCalledTimes(callsBeforeMana + 1);
    expect(submitAction).toHaveBeenLastCalledWith(
      { ...offeredMana, data: { ...offeredMana.data, payment_mode: MANUAL } },
      HUMAN,
    );
    // CR 605.3a: a mana ability activated mid-payment pays into that payment —
    // still the Speaker's own window, still its pending activation.
    await waitForPrompt("ManaPayment", HUMAN);
    expect(storeState().pending_cast?.object_id).toBe(board.speaker);
    expect(storeState().stack).toHaveLength(0);
    expect(poolSize(HUMAN)).toBe(2);

    await act(async () => {
      await useGameStore.getState().dispatch({ type: "PassPriority" });
    });
    await waitForPrompt("Priority", HUMAN);
    expect(storeState().stack).toHaveLength(1);
    expect(storeState().stack[0].source_id).toBe(board.speaker);
    expect(isTapped(board.speaker)).toBe(true);
    expect(isTapped(board.mountain)).toBe(false);
    // {1} was paid from Sol Ring's {C}{C}; one {C} still floats.
    expect(poolSize(HUMAN)).toBe(1);
  });

  it("the human's saved Manual preference never stamps the AI seat's activation", async () => {
    await rehydrateSavedPreference("manual");
    const board = seedBoard(OPPONENT, OPPONENT);
    const { adapter, submitAction } = await installRealAdapter();
    const opponentActions = (await adapter.getLegalActionsForViewer(OPPONENT)).actions;
    const offered = opponentActions
      .filter(isActivation)
      .filter((action) => action.data.source_id === board.speaker);
    expect(offered).toHaveLength(1);

    await act(async () => {
      await dispatchAction(offered[0], OPPONENT);
    });
    expect(submitAction).toHaveBeenNthCalledWith(1, offered[0], OPPONENT);
    await waitForPrompt("TargetSelection", OPPONENT);
    await act(async () => {
      await dispatchAction({ type: "ChooseTarget", data: { target: { Object: board.bears } } }, OPPONENT);
    });

    // Unstamped, the engine pays it automatically: no window for either seat.
    await waitForPrompt("Priority", OPPONENT);
    expect(storeState().stack).toHaveLength(1);
    expect(storeState().stack[0].source_id).toBe(board.speaker);
    expect(isTapped(board.speaker)).toBe(true);
    expect(isTapped(board.mountain) || isTapped(board.solRing)).toBe(true);
  });
});
