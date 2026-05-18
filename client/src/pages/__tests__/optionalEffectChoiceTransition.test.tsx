/**
 * Issue #459 — P0 softlock regression test.
 *
 * Repro: Ob Nixilis, the Fallen on the battlefield → play a land → the
 * optional+targeted landfall trigger produces a two-stage WaitingFor prompt
 * sequence `TriggerTargetSelection → Priority → OptionalEffectChoice`. The
 * bug report: after target selection the game hangs on "Waiting" — neither
 * landfall trigger resolves and the `OptionalEffectModal` never appears.
 *
 * A debugger review proved the engine emits a fully-actionable
 * `OptionalEffectChoice` for every reachable state (engine-innocent — see
 * `crates/engine/tests/integration/integration_landfall.rs`). The defect is
 * in the frontend display layer.
 *
 * Root cause: `dispatchAction` (`client/src/game/dispatch.ts`) de-duplicates
 * "rapid double-clicks" by comparing only the `GameAction` payload, ignoring
 * the `actor`. During the intervening priority round the human passes
 * priority (`PassPriority`, actor 0); the AI controller then passes priority
 * for its seat (`PassPriority`, actor 1) while the human's identical action
 * is still in flight. The de-dup wrongly treats the AI's pass as a duplicate
 * of the human's and silently drops it — so the trigger never resolves and
 * the game softlocks at `Priority{player:1}`, showing "Waiting".
 *
 * This drives the REAL `dispatchAction` pipeline with a scripted adapter
 * replaying the exact engine `WaitingFor` sequence, then renders the real
 * `DialogHost` + GamePage modal-mount gate and asserts the actionable
 * `OptionalEffectModal` appears. The class under test is "two seats
 * dispatching the same action type across a multi-stage prompt sequence".
 */
import { act, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type {
  EngineAdapter,
  GameAction,
  GameState,
  LegalActionsResult,
  SubmitResult,
  WaitingFor,
} from "../../adapter/types";
import { OptionalEffectModalContent } from "../../components/modal/OptionalEffectModal.tsx";
import { DialogHost } from "../../components/modal/DialogHost.tsx";
import { dispatchAction } from "../../game/dispatch.ts";
import { useCanActForWaitingState } from "../../hooks/usePlayerId.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { usePreferencesStore } from "../../stores/preferencesStore.ts";
import { useUiStore } from "../../stores/uiStore.ts";

// ── Engine-shaped fixtures ──────────────────────────────────────────────

const OB_NIXILIS_ID = 100;

function baseState(waitingFor: WaitingFor, stack: GameState["stack"]): GameState {
  return {
    turn_number: 3,
    active_player: 0,
    phase: "PreCombatMain",
    players: [
      { id: 0, life: 20 } as unknown as GameState["players"][number],
      { id: 1, life: 20 } as unknown as GameState["players"][number],
    ],
    priority_player: 0,
    objects: {
      [OB_NIXILIS_ID]: {
        name: "Ob Nixilis, the Fallen",
      } as unknown as GameState["objects"][number],
    },
    next_object_id: 300,
    battlefield: [OB_NIXILIS_ID],
    stack,
    exile: [],
    rng_seed: 42,
    combat: null,
    waiting_for: waitingFor,
    has_pending_cast: false,
    lands_played_this_turn: 1,
    max_lands_per_turn: 1,
    priority_pass_count: 0,
    pending_replacement: null,
    layers_dirty: false,
    next_timestamp: 1,
    turn_decision_controller: 0,
  } as unknown as GameState;
}

const PRIORITY_P0: WaitingFor = { type: "Priority", data: { player: 0 } };
const PRIORITY_P1: WaitingFor = { type: "Priority", data: { player: 1 } };

const OPTIONAL_EFFECT_CHOICE: WaitingFor = {
  type: "OptionalEffectChoice",
  data: {
    player: 0,
    source_id: OB_NIXILIS_ID,
    description: "Ob Nixilis, the Fallen — you may have target player lose 3 life.",
  },
};

const TWO_TRIGGERS_ON_STACK: GameState["stack"] = [
  { id: 1 } as unknown as GameState["stack"][number],
  { id: 2 } as unknown as GameState["stack"][number],
];

const EMPTY_LEGAL: LegalActionsResult = {
  actions: [],
  autoPassRecommended: false,
};

/**
 * Scripted adapter. After the human passes priority (`PassPriority`, actor 0)
 * the engine moves to `Priority{player:1}`; after the AI passes priority
 * (`PassPriority`, actor 1) the batched Ob Nixilis trigger resolves into
 * `OptionalEffectChoice{player:0}`. `submitAction` advances the script and
 * records every (action, actor) pair the dispatch layer actually delivers —
 * so the test can prove the AI's pass was NOT dropped.
 */
function scriptedAdapter(): {
  adapter: EngineAdapter;
  delivered: { action: GameAction; actor: number }[];
} {
  const delivered: { action: GameAction; actor: number }[] = [];
  // Indexed by how many actions have been delivered to the engine so far.
  const sequence: GameState[] = [
    baseState(PRIORITY_P0, TWO_TRIGGERS_ON_STACK), // ← initial (P0 intervening priority)
    baseState(PRIORITY_P1, TWO_TRIGGERS_ON_STACK), // ← after human PassPriority
    baseState(OPTIONAL_EFFECT_CHOICE, TWO_TRIGGERS_ON_STACK), // ← after AI PassPriority
  ];
  let step = 0;

  const adapter: EngineAdapter = {
    initialize: vi.fn().mockResolvedValue(undefined),
    initializeGame: vi.fn().mockResolvedValue({ events: [] } as SubmitResult),
    submitAction: vi.fn(
      async (action: GameAction, actor: number): Promise<SubmitResult> => {
        delivered.push({ action, actor });
        step = Math.min(step + 1, sequence.length - 1);
        return { events: [] };
      },
    ),
    getState: vi.fn(async () => sequence[step]),
    getLegalActions: vi.fn(async () => EMPTY_LEGAL),
    restoreState: vi.fn(),
    getAiAction: vi.fn().mockReturnValue(null),
    estimateBracket: vi.fn().mockResolvedValue(null),
    dispose: vi.fn(),
  };
  return { adapter, delivered };
}

/**
 * Mirrors the GamePage `<DialogHost>` modal-mount gate verbatim
 * (GamePage.tsx:1237-1241). Reproducing the gate keeps the test
 * discriminating — it fails for the same reason the live GamePage hangs.
 */
function GameDialogHarness() {
  const waitingFor = useGameStore((s) => s.waitingFor);
  const objects = useGameStore((s) => s.gameState?.objects);
  const canActForWaitingState = useCanActForWaitingState();

  return (
    <DialogHost>
      {(waitingFor?.type === "OptionalEffectChoice" ||
        waitingFor?.type === "OpponentMayChoice") &&
        canActForWaitingState && (
          <OptionalEffectModalContent
            waitingFor={waitingFor}
            objects={objects}
            dispatch={() => {}}
          />
        )}
    </DialogHost>
  );
}

// ── Test ────────────────────────────────────────────────────────────────

describe("issue #459 — optional + targeted landfall trigger prompt sequence", () => {
  let delivered: { action: GameAction; actor: number }[];

  beforeEach(() => {
    const scripted = scriptedAdapter();
    delivered = scripted.delivered;
    act(() => {
      useGameStore.setState({
        gameId: "issue-459-repro",
        gameMode: "ai",
        adapter: scripted.adapter,
        gameState: baseState(PRIORITY_P0, TWO_TRIGGERS_ON_STACK),
        waitingFor: PRIORITY_P0,
        events: [],
        eventHistory: [],
        logHistory: [],
        nextLogSeq: 0,
        stateHistory: [],
        turnCheckpoints: [],
      });
      useUiStore.setState({ pendingAbilityChoice: null, enchantmentsDialogPlayer: null });
      // Instant animations so the dispatch pipeline does not await timers.
      usePreferencesStore.setState({ animationSpeedMultiplier: 0 });
    });
  });

  afterEach(() => {
    act(() => {
      useGameStore.setState({ gameState: null, waitingFor: null, adapter: null });
    });
  });

  it("delivers both seats' PassPriority across the intervening priority round", async () => {
    render(<GameDialogHarness />);

    // The human passes priority (actor 0), then — before the engine has
    // settled — the AI controller passes priority for its own seat (actor 1).
    // Both are textually `{ type: "PassPriority" }`; the de-dup must not
    // collapse them because they are different seats' decisions.
    await act(async () => {
      await Promise.all([
        dispatchAction({ type: "PassPriority" } as GameAction, 0),
        dispatchAction({ type: "PassPriority" } as GameAction, 1),
      ]);
    });

    // Discriminating precondition: BOTH passes must reach the engine. On the
    // buggy `main` the AI's actor-1 pass is de-duped against the in-flight
    // actor-0 pass and silently dropped — `delivered` then has only one entry
    // and the engine never advances past `Priority{player:1}`.
    expect(delivered).toEqual([
      { action: { type: "PassPriority" }, actor: 0 },
      { action: { type: "PassPriority" }, actor: 1 },
    ]);

    // With both passes delivered the engine resolves the Ob Nixilis trigger.
    expect(useGameStore.getState().waitingFor?.type).toBe("OptionalEffectChoice");

    // The actionable modal must now be mounted — the softlock is cleared.
    expect(screen.getByRole("button", { name: /yes/i })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /no/i })).toBeInTheDocument();
  });
});
