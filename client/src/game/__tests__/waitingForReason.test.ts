/**
 * Unit tests for `waitingForReason` — maps a pending decision to a localized
 * reason key. Pure function over engine-provided facts (variant type, plus
 * phase/stack for the priority window); it labels state, never infers it.
 *
 * The function reads only `waitingFor.type` (and, for `Priority`, the
 * engine-provided `stack.length` / `phase`), so the fixtures are intentionally
 * minimal and cast to the public types.
 */
import { describe, expect, it } from "vitest";

import type { GameState, Phase, WaitingFor } from "../../adapter/types";
import { waitingForReason } from "../waitingForRegistry";

function wf(type: string, data: Record<string, unknown> = {}): WaitingFor {
  return { type, data } as unknown as WaitingFor;
}

function gs(phase: Phase, stackLen = 0): GameState {
  return {
    phase,
    stack: Array.from({ length: stackLen }, (_, i) => ({ object_id: i })),
  } as unknown as GameState;
}

describe("waitingForReason", () => {
  it("returns null when nothing is pending or the game is over", () => {
    expect(waitingForReason(null, null)).toBeNull();
    expect(waitingForReason(wf("GameOver", { winner: 0 }), gs("End"))).toBeNull();
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
