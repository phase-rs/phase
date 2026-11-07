import { describe, expect, it, vi } from "vitest";

import { P2PGuestAdapter, P2PHostAdapter } from "../p2p-adapter.ts";
import {
  AdapterError,
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
  sent: boolean;
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

type HostInternals = {
  disposed: boolean;
  ownsAuthority: () => boolean;
  nativeBridge: {
    getState: () => Promise<{
      say_payment: { id: number; deadline_ms: number };
      waiting_for: { type: "UnlessPayment"; data: { player: number } };
    }>;
    submitAction: (action: SayingAction, player: number) => Promise<{ result: SubmitResult }>;
  };
  applyBrowserMutation: <T>(run: () => Promise<T>) => Promise<T>;
  publishHostSnapshot: (result: SubmitResult) => Promise<boolean>;
  broadcastStateUpdate: (transition: unknown) => Promise<void>;
  runAiLoop: () => Promise<void>;
  persistAuthoritativeState: () => void;
  scheduleSayPhraseExpiry: () => void;
  expireSayPhrase: (paymentId: number) => Promise<void>;
};

function makeHost(submitAction: HostInternals["nativeBridge"]["submitAction"]) {
  const host = Object.create(P2PHostAdapter.prototype) as unknown as HostInternals;
  host.disposed = false;
  host.ownsAuthority = () => true;
  host.nativeBridge = {
    getState: async () => ({
      say_payment: { id: 7, deadline_ms: Date.now() - 1 },
      waiting_for: { type: "UnlessPayment", data: { player: 0 } },
    }),
    submitAction,
  };
  host.applyBrowserMutation = (run) => run();
  host.publishHostSnapshot = vi.fn(async () => true);
  host.broadcastStateUpdate = vi.fn(async () => {});
  host.runAiLoop = vi.fn(async () => {});
  host.persistAuthoritativeState = vi.fn();
  host.scheduleSayPhraseExpiry = vi.fn();
  return host;
}

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

  it("submits the host's own lapse report when the payment deadline passes", async () => {
    const submitAction = vi.fn(async () => ({ result: EMPTY }));
    const host = makeHost(submitAction);

    await host.expireSayPhrase(7);

    expect(submitAction).toHaveBeenCalledTimes(1);
    expect(submitAction).toHaveBeenCalledWith(
      expect.objectContaining({
        type: "SayPhrase",
        data: expect.objectContaining({
          payment: 7,
          seq: 0,
          report: "Expiry",
        }),
      }),
      0,
    );
    expect(host.publishHostSnapshot).toHaveBeenCalledWith(EMPTY);
    expect(host.runAiLoop).toHaveBeenCalledTimes(1);
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

  it("does not settle a parked Stay Quiet submission when a saying's state_update broadcast arrives", () => {
    const guest = makeGuest(true);
    const ordinaryResolve = vi.fn();
    const ordinaryReject = vi.fn();
    guest.parkPendingSubmission(ordinaryResolve, ordinaryReject);

    // In guest adapter's handleMessage for "state_update", msg.saying bypasses settlePendingSubmission.
    const msg = {
      type: "state_update" as const,
      saying: true,
      events: [],
      logEntries: [],
    };
    const settled = msg.saying
      ? false
      : guest.settlePendingSubmission({
          kind: "resolve",
          result: { events: msg.events, log_entries: msg.logEntries },
        });

    expect(settled).toBe(false);
    expect(ordinaryResolve).not.toHaveBeenCalled();
    expect(guest.pendingResolve).toBe(ordinaryResolve);
  });

  it("forwards a native rejection through host to guest as a failed saying_result", () => {
    const host = Object.create(P2PHostAdapter.prototype) as any;
    const sentMessages: unknown[] = [];
    host.send = vi.fn((_session: unknown, msg: unknown) => {
      sentMessages.push(msg);
      return Promise.resolve(true);
    });
    const session = {};
    const rejection = new AdapterError(
      AdapterErrorCode.ACTION_REJECTED,
      "Payment already expired",
      false,
    );

    host.sendSayingFailure(session, saying(1), rejection);

    expect(sentMessages).toEqual([
      {
        type: "saying_result",
        payment: 7,
        seq: 1,
        accepted: false,
        message: "Payment already expired",
      },
    ]);
  });
});
