/**
 * Unit tests for `waitingForReason` — maps a pending decision to a localized
 * reason key. Pure function over engine-provided facts (variant type, plus
 * phase/stack for the priority window); it labels state, never infers it.
 *
 * The function reads only `waitingFor.type` (and, for `Priority`, the
 * engine-provided `stack.length` / `phase`), so the fixtures keep all other
 * state at factory defaults.
 */
import { describe, expect, it } from "vitest";

import type { GameState, Phase, WaitingFor } from "../../adapter/types";
import {
  buildGameState,
  buildManaPaymentWaitingFor,
  buildPriorityWaitingFor,
  buildStackEntry,
  buildTargetSelectionWaitingFor,
} from "../../test/factories/gameStateFactory";
import { waitingForReason } from "../waitingForRegistry";

function wf(type: WaitingFor["type"]): WaitingFor {
  switch (type) {
    case "DeclareAttackers":
      return { type, data: { player: 0, valid_attacker_ids: [] } };
    case "DeclareBlockers":
      return { type, data: { player: 0, valid_blocker_ids: [], valid_block_targets: {} } };
    case "GameOver":
      return { type, data: { winner: null } };
    case "ManaPayment":
      return buildManaPaymentWaitingFor();
    case "MulliganDecision":
      return { type, data: { pending: [{ player: 0, mulligan_count: 0 }], free_first_mulligan: false } };
    case "OrderTriggers":
      return { type, data: { player: 0, triggers: [] } };
    case "Priority":
      return buildPriorityWaitingFor();
    case "ScryChoice":
      return { type, data: { player: 0, cards: [] } };
    case "TargetSelection":
      return buildTargetSelectionWaitingFor();
    default:
      throw new Error(`Unexpected test WaitingFor variant: ${type}`);
  }
}

function gs(phase: Phase, stackLen = 0): GameState {
  return buildGameState({
    phase,
    stack: Array.from({ length: stackLen }, (_, i) => buildStackEntry({ id: i + 1 })),
  });
}

describe("waitingForReason", () => {
  it("returns null when nothing is pending or the game is over", () => {
    expect(waitingForReason(null, null)).toBeNull();
    expect(waitingForReason(wf("GameOver"), gs("End"))).toBeNull();
  });

  it("maps common decision variants to their reason keys", () => {
    expect(waitingForReason(wf("DeclareAttackers"), gs("DeclareAttackers"))?.key)
      .toBe("status.reason.declareAttackers");
    expect(waitingForReason(wf("DeclareBlockers"), gs("DeclareBlockers"))?.key)
      .toBe("status.reason.declareBlockers");
    expect(waitingForReason(wf("TargetSelection"), gs("PreCombatMain"))?.key)
      .toBe("status.reason.choosingTargets");
    expect(waitingForReason(wf("ManaPayment"), gs("PreCombatMain"))?.key)
      .toBe("status.reason.payingCost");
    expect(waitingForReason(wf("MulliganDecision"), gs("Untap"))?.key)
      .toBe("status.reason.mulligan");
    expect(waitingForReason(wf("OrderTriggers"), gs("Upkeep"))?.key)
      .toBe("status.reason.orderingTriggers");
  });

  it("disambiguates the Priority window by stack depth then phase", () => {
    // Non-empty stack wins regardless of phase.
    expect(waitingForReason(wf("Priority"), gs("PreCombatMain", 1))?.key)
      .toBe("status.reason.respondingToStack");
    // Empty stack: main phases.
    expect(waitingForReason(wf("Priority"), gs("PreCombatMain"))?.key)
      .toBe("status.reason.priorityMain");
    expect(waitingForReason(wf("Priority"), gs("PostCombatMain"))?.key)
      .toBe("status.reason.priorityMain");
    // Empty stack: combat steps.
    expect(waitingForReason(wf("Priority"), gs("DeclareBlockers"))?.key)
      .toBe("status.reason.priorityCombat");
    // Empty stack: other phases (e.g. upkeep) fall to the generic priority key.
    expect(waitingForReason(wf("Priority"), gs("Upkeep"))?.key)
      .toBe("status.reason.priority");
  });

  it("falls back to a generic reason for unmapped variants (graceful degradation)", () => {
    // A real variant with no explicit case must not break — it degrades.
    expect(waitingForReason(wf("ScryChoice"), gs("Upkeep"))?.key)
      .toBe("status.reason.thinking");
  });
});
