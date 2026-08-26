import { describe, expect, it } from "vitest";

import type { SeatKind } from "../../multiplayer/seatTypes";
import {
  buildAssistPaymentWaitingFor,
  buildLoopShortcutWaitingFor,
} from "../../test/factories/gameStateFactory";
import { aiActorFromWaitingFor } from "../p2p-adapter";

// CR 732.2a: the host/P2P AI driver's actor gate must admit LoopShortcut (whose
// data field is `proposer`, not `player`) into the engine-derived authorized-
// submitter path, so an AI-owned proposer seat produces DeclareShortcut instead
// of hanging the game on an unhandled offer.

const seats: SeatKind[] = [
  { type: "HostHuman" },
  { type: "Ai", data: { difficulty: "medium", deck: { type: "Random" } } },
];

const loopShortcut = buildLoopShortcutWaitingFor({ proposer: 1 });

// A data-carrying state that carries neither `player` nor is LoopShortcut
// (AssistPayment routes on `caster`/`chosen`) — must return null. This is the
// non-vacuity control proving the admission is LoopShortcut-specific, not an
// always-return-authorizedSubmitter.
const assistPayment = buildAssistPaymentWaitingFor();

describe("aiActorFromWaitingFor — LoopShortcut routing (T8)", () => {
  it("routes a LoopShortcut offer to the authorized submitter (proposer)", () => {
    // Revert-probe target: delete `|| waitingFor.type === "LoopShortcut"` in
    // aiActorFromWaitingFor and this returns null instead of 1 → this fails.
    expect(aiActorFromWaitingFor(loopShortcut, seats, 1)).toBe(1);
  });

  it("does not blanket-admit non-player, non-LoopShortcut states (control)", () => {
    expect(aiActorFromWaitingFor(assistPayment, seats, 1)).toBeNull();
  });
});
