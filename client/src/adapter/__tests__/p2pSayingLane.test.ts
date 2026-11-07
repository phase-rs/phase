import { describe, expect, it, vi } from "vitest";

import { P2PGuestAdapter } from "../p2p-adapter.ts";
import {
  AdapterErrorCode,
  type GameAction,
  type SubmitResult,
} from "../types.ts";

type SayingAction = Extract<GameAction, { type: "SayPhrase" }>;
type SayingResult = {
  type: "saying_result";
  payment: number;
  seq: number;
  accepted: boolean;
  message?: string;
};
type PendingSaying = {
  resolve: (result: SubmitResult) => void;
  reject: (error: Error) => void;
  timer: ReturnType<typeof setTimeout>;
};

/**
 * The guest adapter's transport and reply slot, without a live PeerJS session.
 * These are exactly the fields `submitSaying` / `settleSaying` touch, so the
 * test exercises the real methods rather than a reimplementation of them.
 */
type GuestInternals = {
  assignedPlayerId: number | null;
  authenticatedSession: unknown;
  pendingSayings: Map<string, PendingSaying>;
  pendingResolve: ((result: SubmitResult) => void) | null;
  pendingReject: ((error: Error) => void) | null;
  pendingSubmissionTimer: ReturnType<typeof setTimeout> | null;
  send: (message: unknown) => Promise<boolean>;
  submitSaying: (action: SayingAction) => Promise<SubmitResult>;
  settleSaying: (result: SayingResult) => void;
  parkPendingSubmission: (
    resolve: (result: SubmitResult) => void,
    reject: (error: Error) => void,
  ) => void;
  settlePendingSubmission: (outcome: {
    kind: "resolve";
    result: SubmitResult;
  }) => boolean;
};

const EMPTY: SubmitResult = { events: [], log_entries: [] };

function makeGuest(sendAccepted: boolean): GuestInternals {
  const adapter = Object.create(
    P2PGuestAdapter.prototype,
  ) as unknown as GuestInternals;
  adapter.assignedPlayerId = 0;
  adapter.authenticatedSession = {};
  adapter.pendingSayings = new Map();
  adapter.pendingResolve = null;
  adapter.pendingReject = null;
  adapter.pendingSubmissionTimer = null;
  adapter.send = vi.fn(async () => sendAccepted);
  return adapter;
}

function saying(seq = 1): SayingAction {
  return {
    type: "SayPhrase",
    data: { payment: 7, seq, at_ms: 1, report: "Saying" },
  };
}

describe("P2P saying reply lane", () => {
  it("rejects a saying whose bytes never left instead of counting it", async () => {
    const guest = makeGuest(false);

    await expect(guest.submitSaying(saying())).rejects.toMatchObject({
      code: AdapterErrorCode.ACTION_NOT_SENT,
    });
    expect(guest.pendingSayings.size).toBe(0);
  });

  it("settles a saying without consuming a parked ordinary command", async () => {
    const guest = makeGuest(true);
    const said = guest.submitSaying(saying());
    expect(guest.pendingSayings.size).toBe(1);

    // Stay Quiet parks an ordinary submission behind the saying.
    const ordinaryResolve = vi.fn();
    const ordinaryReject = vi.fn();
    guest.parkPendingSubmission(ordinaryResolve, ordinaryReject);

    guest.settleSaying({
      type: "saying_result",
      payment: 7,
      seq: 1,
      accepted: true,
    });

    await expect(said).resolves.toEqual(EMPTY);
    expect(ordinaryResolve).not.toHaveBeenCalled();
    expect(ordinaryReject).not.toHaveBeenCalled();

    guest.settlePendingSubmission({ kind: "resolve", result: EMPTY });
    expect(ordinaryResolve).toHaveBeenCalledWith(EMPTY);
  });
});
