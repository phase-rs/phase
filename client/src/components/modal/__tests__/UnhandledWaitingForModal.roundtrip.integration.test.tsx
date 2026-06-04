// Round-trip integration test for issue #337: drives a curated `WaitingFor`
// payload through the gameStore + UnhandledWaitingForModal boundary — the
// genuine engine→frontend round-trip the #311 safety net defends.
//
// Tests 1–4 mock `isWaitingForHandled` to inject an "unhandled" verdict for an
// otherwise-handled variant (`Priority`); they exercise the modal + store
// round-trip, NOT the registry. Test 5 uses the real registry (no override)
// with a genuinely-unhandled variant (`OrphanEngineChoice`) — the only case
// that exercises real `HANDLED_WAITING_FOR_TYPES` membership end-to-end.

import { readFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { GameState, WaitingFor } from "../../../adapter/types.ts";
import { UnhandledWaitingForModal } from "../UnhandledWaitingForModal.tsx";
import { useGameStore } from "../../../stores/gameStore.ts";
import { useMultiplayerStore } from "../../../stores/multiplayerStore.ts";
import { isWaitingForHandled } from "../../../game/waitingForRegistry.ts";

// S3 + S4: spread the real module, replace only `isWaitingForHandled` with a
// `vi.fn` defaulting to the real implementation. The factory closes over no
// top-level binding and the orphan variant string is hardcoded in each test
// body (not the factory), avoiding the hoisting hazard.
vi.mock("../../../game/waitingForRegistry", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../../../game/waitingForRegistry")>();
  return { ...actual, isWaitingForHandled: vi.fn(actual.isWaitingForHandled) };
});

const FIXTURE_PATH = resolve(
  dirname(fileURLToPath(import.meta.url)),
  "../../../../../fixtures/adapter-contract/waiting_for_priority.json",
);

/** Loads the curated `WaitingFor::Priority` fixture (shape-guarded by the Rust
 *  deserialization-contract test in adapter_contract_fixtures.rs). */
function readWaitingForPriorityFixture(): WaitingFor {
  return JSON.parse(readFileSync(FIXTURE_PATH, "utf-8")) as WaitingFor;
}

function makeState(waitingFor: WaitingFor): GameState {
  return {
    turn_number: 1,
    active_player: 0,
    phase: "PreCombatMain",
    players: [
      { id: 0, life: 20, poison_counters: 0, mana_pool: { mana: [] }, library: [], hand: [], graveyard: [], has_drawn_this_turn: false, lands_played_this_turn: 0, turns_taken: 0 },
      { id: 1, life: 20, poison_counters: 0, mana_pool: { mana: [] }, library: [], hand: [], graveyard: [], has_drawn_this_turn: false, lands_played_this_turn: 0, turns_taken: 0 },
    ],
    priority_player: 0,
    objects: {},
    next_object_id: 100,
    battlefield: [],
    stack: [],
    exile: [],
    rng_seed: 1,
    combat: null,
    waiting_for: waitingFor,
    has_pending_cast: false,
    lands_played_this_turn: 0,
    max_lands_per_turn: 1,
    priority_pass_count: 0,
    pending_replacement: null,
    layers_dirty: false,
    next_timestamp: 2,
    eliminated_players: [],
    turn_decision_controller: 0,
  } as unknown as GameState;
}

/** Adapter→store entry point: feeds the `WaitingFor` into gameStore exactly as
 *  `gameStore.initializeGame`/`dispatch` do. */
function seedStoreFromState(
  waitingFor: WaitingFor,
  { gameMode, activePlayerId }: { gameMode: string; activePlayerId: number },
) {
  const gameState = makeState(waitingFor);
  useMultiplayerStore.setState({ activePlayerId });
  useGameStore.setState({ gameMode, gameState, waitingFor } as never);
}

describe("UnhandledWaitingForModal round-trip (issue #337)", () => {
  let realIsHandled: typeof isWaitingForHandled;

  beforeEach(async () => {
    const actual = await vi.importActual<typeof import("../../../game/waitingForRegistry")>(
      "../../../game/waitingForRegistry",
    );
    realIsHandled = actual.isWaitingForHandled;
    useMultiplayerStore.setState({ activePlayerId: 0 });
    useGameStore.setState({ gameMode: "ai", gameState: null, waitingFor: null } as never);
    vi.mocked(isWaitingForHandled).mockReset();
  });

  afterEach(() => {
    cleanup();
  });

  it("Test 1 — an unhandled WaitingFor verdict surfaces the safety-net diagnostic", () => {
    vi.mocked(isWaitingForHandled).mockReturnValue(false);
    seedStoreFromState(readWaitingForPriorityFixture(), { gameMode: "ai", activePlayerId: 0 });

    const { container } = render(
      <UnhandledWaitingForModal onExit={vi.fn()} exitLabel="Return to menu" />,
    );

    expect(screen.getByText("Action required, but UI is missing")).toBeInTheDocument();
    // `Priority` hardcoded (vi.mock hoisting forbids reading a top-level const).
    expect(screen.getByText("Priority")).toBeInTheDocument();
    expect(
      container.querySelector('[data-unhandled-waiting-for="Priority"]'),
    ).not.toBeNull();
  });

  it("Test 2 — exit action is reachable and invokes the caller's exit handler (AI/local)", () => {
    vi.mocked(isWaitingForHandled).mockReturnValue(false);
    seedStoreFromState(readWaitingForPriorityFixture(), { gameMode: "ai", activePlayerId: 0 });

    const onExit = vi.fn();
    render(<UnhandledWaitingForModal onExit={onExit} exitLabel="Return to menu" />);

    fireEvent.click(screen.getByRole("button", { name: "Return to menu" }));
    expect(onExit).toHaveBeenCalledTimes(1);
  });

  it("Test 3 — exit action is reachable in online mode (concede)", () => {
    vi.mocked(isWaitingForHandled).mockReturnValue(false);
    seedStoreFromState(readWaitingForPriorityFixture(), { gameMode: "online", activePlayerId: 0 });

    const onExit = vi.fn();
    render(<UnhandledWaitingForModal onExit={onExit} exitLabel="Concede game" />);

    fireEvent.click(screen.getByRole("button", { name: "Concede game" }));
    expect(onExit).toHaveBeenCalledTimes(1);
  });

  it("Test 4 — modal returns null when its input predicate reports the variant handled", () => {
    vi.mocked(isWaitingForHandled).mockReturnValue(true);
    seedStoreFromState(readWaitingForPriorityFixture(), { gameMode: "ai", activePlayerId: 0 });

    const { container } = render(
      <UnhandledWaitingForModal onExit={vi.fn()} exitLabel="Return to menu" />,
    );

    expect(container.firstChild).toBeNull();
  });

  it("Test 5 — a genuinely-unhandled real variant fires the modal through the real registry", () => {
    // NON-MOCKED: delegate to the real predicate so this case exercises real
    // HANDLED_WAITING_FOR_TYPES membership end-to-end.
    vi.mocked(isWaitingForHandled).mockImplementation((wf) => realIsHandled(wf));
    const orphan = {
      type: "OrphanEngineChoice",
      data: { player: 0 },
    } as unknown as WaitingFor;
    seedStoreFromState(orphan, { gameMode: "ai", activePlayerId: 0 });

    const { container } = render(
      <UnhandledWaitingForModal onExit={vi.fn()} exitLabel="Return to menu" />,
    );

    expect(screen.getByText("Action required, but UI is missing")).toBeInTheDocument();
    expect(
      container.querySelector('[data-unhandled-waiting-for="OrphanEngineChoice"]'),
    ).not.toBeNull();
  });
});
