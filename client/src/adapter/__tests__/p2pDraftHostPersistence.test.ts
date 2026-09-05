import { afterEach, describe, expect, it, vi } from "vitest";

const { clearDraftHostSession, saveDraftHostSession } = vi.hoisted(() => ({
  clearDraftHostSession: vi.fn(async () => {}),
  saveDraftHostSession: vi.fn<(id: string, session: unknown) => Promise<void>>(async () => {}),
}));

vi.mock("../draft-adapter", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../draft-adapter")>();
  return {
    ...actual,
    DraftAdapter: vi.fn().mockImplementation(function () {
      return {};
    }),
  };
});

vi.mock("../../services/draftPersistence", () => ({
  clearDraftHostSession,
  saveDraftHostSession,
}));

import { P2PDraftHost } from "../p2p-draft-host";
import type { DraftPlayerView } from "../draft-adapter";
import {
  DRAFT_PROTOCOL_VERSION, DraftPauseReason, decodeDraftWireMessage, encodeDraftWireMessage,
  type DraftMatchBinding, type DraftP2PMessage,
} from "../../network/draftProtocol";
import type { DraftPeerSession } from "../../network/draftPeerSession";
import { FakeDraftDataConnection } from "../../network/__tests__/fakeDraftDataConnection";

type PersistenceHost = {
  adapter: { exportSession: () => Promise<string> };
  draftStarted: boolean;
  persistQueue: Promise<void>;
  persistSession: () => void;
};

type AdmissionHost = PersistenceHost & {
  procedure: { packs_per_player: number; min_deck_size: number };
  handleNewGuest: (session: unknown, displayName: string) => Promise<void>;
  handleReconnect: (session: unknown, draftToken: string) => Promise<void>;
  guestSessions: Map<number, unknown>;
  seatTokens: Map<number, string>;
};

type BackupHost = {
  draftCode: string;
  uploadBackupSnapshot: (snapshot: unknown) => Promise<void>;
};

function recoveredHost(hostDisplayName: string): P2PDraftHost {
  return new P2PDraftHost(
    { id: hostDisplayName } as never,
    () => () => {},
    { type: "Set", data: { pools: [{ code: "TST" }], sequence: ["TST"] } } as never,
    "Premier",
    8,
    hostDisplayName,
    "Swiss",
    "Casual",
    undefined,
    "shared-recovery",
    "ABCDE",
  );
}

function ephemeralHost(hostDisplayName: string): P2PDraftHost {
  return new P2PDraftHost(
    { id: hostDisplayName } as never,
    () => () => {},
    { type: "Set", data: { pools: [{ code: "TST" }], sequence: ["TST"] } } as never,
    "Premier",
    8,
    hostDisplayName,
    "Swiss",
    "Casual",
  );
}

describe("P2PDraftHost persistence disposal", () => {
  afterEach(() => {
    vi.clearAllMocks();
  });

  it("redacts canonical Chaos DraftSource assignments before public backup upload", async () => {
    const originalFetch = globalThis.fetch;
    const fetchMock = vi.fn<typeof fetch>(async () => new Response("", { status: 200 }));
    globalThis.fetch = fetchMock;

    try {
      const host = new P2PDraftHost(
        { id: "host-peer" } as never,
        () => () => {},
        { type: "Set", data: { pools: [{ code: "TST" }], sequence: ["TST"] } } as never,
        "Premier",
        8,
        "Host",
        "Swiss",
        "Casual",
        undefined,
        undefined,
        undefined,
        "https://phase.example",
      );
      const privateHost = host as unknown as BackupHost;
      privateHost.draftCode = "ABC123";
      const snapshot = {
        draftSessionJson: JSON.stringify({
          config: {
            source: {
              type: "Set",
              data: {
                candidate_codes: ["TST", "ALT"],
                assignments: [["TST", "ALT"], ["ALT", "TST"]],
              },
            },
          },
        }),
      };

      await privateHost.uploadBackupSnapshot(snapshot);

      const [, requestInit] = fetchMock.mock.calls[0]!;
      const request = JSON.parse(requestInit?.body as string);
      const publicSnapshot = JSON.parse(request.snapshot_json);
      const publicSession = JSON.parse(publicSnapshot.draftSessionJson);
      expect(publicSession.config.source).toEqual({
        type: "Set",
        data: { candidate_codes: ["TST", "ALT"] },
      });
      expect(JSON.parse(snapshot.draftSessionJson).config.source.data.assignments).toEqual([
        ["TST", "ALT"],
        ["ALT", "TST"],
      ]);
    } finally {
      globalThis.fetch = originalFetch;
    }
  });

  it("uploads a canonical Cube DraftSource unchanged", async () => {
    const originalFetch = globalThis.fetch;
    const fetchMock = vi.fn<typeof fetch>(async () => new Response("", { status: 200 }));
    globalThis.fetch = fetchMock;

    try {
      const host = new P2PDraftHost(
        { id: "host-peer" } as never,
        () => () => {},
        { type: "Set", data: { pools: [{ code: "TST" }], sequence: ["TST"] } } as never,
        "Premier",
        8,
        "Host",
        "Swiss",
        "Casual",
        undefined,
        undefined,
        undefined,
        "https://phase.example",
      );
      const privateHost = host as unknown as BackupHost;
      privateHost.draftCode = "ABC123";
      const snapshot = {
        draftSessionJson: JSON.stringify({
          config: {
            source: {
              type: "Cube",
              data: { id: "my-cube", name: "My Cube" },
            },
          },
        }),
      };

      await privateHost.uploadBackupSnapshot(snapshot);

      const [, requestInit] = fetchMock.mock.calls[0]!;
      const request = JSON.parse(requestInit?.body as string);
      const publicSnapshot = JSON.parse(request.snapshot_json);
      const publicSession = JSON.parse(publicSnapshot.draftSessionJson);
      expect(publicSession.config.source).toEqual({
        type: "Cube",
        data: { id: "my-cube", name: "My Cube" },
      });
    } finally {
      globalThis.fetch = originalFetch;
    }
  });

  it("fences a disposed recovery's queued save before a newer recovery can persist", async () => {
    const stale = recoveredHost("Stale host");
    const stalePrivate = stale as unknown as PersistenceHost;
    stalePrivate.draftStarted = true;
    let resolveStaleExport!: (session: string) => void;
    stalePrivate.adapter.exportSession = vi.fn(() => new Promise<string>((resolve) => {
      resolveStaleExport = resolve;
    }));
    stalePrivate.persistSession();
    await Promise.resolve();
    expect(stalePrivate.adapter.exportSession).toHaveBeenCalledOnce();

    // Route cancellation disposes the stale recovery while its queued snapshot
    // is still loading. The fence must be set before this promise can continue.
    const disposeStale = stale.dispose();

    const current = recoveredHost("Current host");
    const currentPrivate = current as unknown as PersistenceHost;
    currentPrivate.draftStarted = false;
    currentPrivate.persistSession();
    await currentPrivate.persistQueue;

    expect(saveDraftHostSession).toHaveBeenCalledTimes(1);
    expect(saveDraftHostSession).toHaveBeenLastCalledWith(
      "shared-recovery",
      expect.objectContaining({ hostDisplayName: "Current host" }),
    );

    resolveStaleExport("{\"status\":\"Drafting\"}");
    await disposeStale;

    expect(saveDraftHostSession).toHaveBeenCalledTimes(1);
    await current.dispose();
  });

  it("finishes draft termination after a guest notification send fails", async () => {
    const host = recoveredHost("Host");
    const privateHost = host as unknown as PersistenceHost & {
      hostPeer: { destroy: ReturnType<typeof vi.fn> };
      hostConnectionUnsub: ReturnType<typeof vi.fn>;
      guestSessions: Map<number, unknown>;
      persistenceClosed: boolean;
    };
    const failure = new Error("guest transport closed");
    const failedSession = { send: vi.fn(async () => { throw failure; }), close: vi.fn() };
    const liveSession = { send: vi.fn(async () => {}), close: vi.fn() };
    privateHost.guestSessions.set(1, failedSession);
    privateHost.guestSessions.set(2, liveSession);
    privateHost.hostPeer.destroy = vi.fn();
    privateHost.hostConnectionUnsub = vi.fn();
    privateHost.persistSession();
    await privateHost.persistQueue;
    expect(saveDraftHostSession).toHaveBeenCalledWith("shared-recovery", expect.any(Object));

    const dispose = vi.spyOn(host, "dispose");
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    try {
      const termination = host.terminateDraft();
      expect(privateHost.persistenceClosed).toBe(true);
      await expect(termination).resolves.toBeUndefined();

      const notification = { type: "draft_host_left", reason: "Host left the draft" };
      expect(failedSession.send).toHaveBeenCalledWith(notification);
      expect(liveSession.send).toHaveBeenCalledWith(notification);
      expect(warn).toHaveBeenCalledWith("[P2PDraftHost] termination notification failed:", failure);
      expect(clearDraftHostSession).toHaveBeenCalledExactlyOnceWith("shared-recovery");
      expect(liveSession.send).toHaveBeenCalledBefore(clearDraftHostSession);
      expect(clearDraftHostSession).toHaveBeenCalledBefore(dispose);
      expect(dispose).toHaveBeenCalledOnce();
      expect(privateHost.hostConnectionUnsub).toHaveBeenCalledOnce();
      expect(failedSession.close).toHaveBeenCalledOnce();
      expect(liveSession.close).toHaveBeenCalledOnce();
      expect(privateHost.guestSessions.size).toBe(0);
      expect(privateHost.hostPeer.destroy).toHaveBeenCalledOnce();
      expect(liveSession.close).toHaveBeenCalledBefore(privateHost.hostPeer.destroy);
    } finally {
      warn.mockRestore();
      dispose.mockRestore();
      await host.dispose();
    }
  });

  it.each(["fresh", "duplicate"] as const)(
    "retains a %s settlement receipt after acknowledgement send failure and accepts an exact retry once",
    async (attempt) => {
      const host = recoveredHost("Host");
      const privateHost = host as unknown as {
        adapter: {
          reportMatchResult: ReturnType<typeof vi.fn>;
          getViewForSeat: ReturnType<typeof vi.fn>;
          exportSession: ReturnType<typeof vi.fn>;
        };
        draftStarted: boolean;
        guestSessions: Map<number, unknown>;
        matchBindings: Map<string, DraftMatchBinding>;
        settlementReceipts: Map<string, { receiptId: string; revision: number }>;
        settlementOutbox: Map<string, unknown>;
        handleGuestMessage: (seat: number, message: DraftP2PMessage) => Promise<void>;
      };
      const binding: DraftMatchBinding = {
        podId: "draft-1", matchId: "match-12", round: 1,
        sessionKey: "session-1", lease: "lease-1", nonce: "nonce-1", revision: 0,
        matchAuthoritySeat: 1,
      };
      const view = {
        status: "MatchInProgress",
        current_round: 1,
        pairings: [{ match_id: binding.matchId, round: 1, seat_a: 1, seat_b: 2 }],
      };
      privateHost.draftStarted = true;
      privateHost.adapter.reportMatchResult = vi.fn(async () => view);
      privateHost.adapter.getViewForSeat = vi.fn(async () => view);
      privateHost.adapter.exportSession = vi.fn(async () => JSON.stringify(view));
      privateHost.matchBindings.set(binding.matchId, binding);
      const sendAck = vi.fn<(message: DraftP2PMessage) => Promise<void>>(async () => {});
      privateHost.guestSessions.set(1, {
        send: async (message: DraftP2PMessage) => {
          if (message.type === "draft_match_settlement_ack") await sendAck(message);
        },
        close: vi.fn(),
      });
      const settlement: DraftP2PMessage = {
        type: "draft_match_settlement",
        settlement: { binding, receiptId: "receipt-1", winnerSeat: 1 },
      };
      const acknowledgement = {
        type: "draft_match_settlement_ack", matchId: binding.matchId, receiptId: "receipt-1", revision: 0,
      };
      const failure = new Error("acknowledgement transport closed");
      const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
      try {
        if (attempt === "duplicate") {
          await privateHost.handleGuestMessage(1, settlement);
          expect(sendAck).toHaveBeenCalledExactlyOnceWith(acknowledgement);
          sendAck.mockClear();
        }
        sendAck.mockRejectedValueOnce(failure);

        await privateHost.handleGuestMessage(1, settlement);

        await vi.waitFor(() => expect(warn).toHaveBeenCalledWith(
          "[P2PDraftHost] settlement acknowledgement failed:", failure,
        ));
        expect(sendAck).toHaveBeenCalledExactlyOnceWith(acknowledgement);
        expect(privateHost.settlementReceipts.get(binding.matchId)).toEqual({ receiptId: "receipt-1", revision: 0 });
        expect(privateHost.settlementOutbox.size).toBe(0);
        expect(saveDraftHostSession).toHaveBeenLastCalledWith("shared-recovery", expect.objectContaining({
          settlementReceipts: [{ matchId: binding.matchId, receiptId: "receipt-1", revision: 0 }],
          settlementOutbox: [],
        }));
        expect(saveDraftHostSession).toHaveBeenCalledBefore(sendAck);

        await privateHost.handleGuestMessage(1, settlement);

        expect(sendAck).toHaveBeenCalledTimes(2);
        expect(sendAck).toHaveBeenLastCalledWith(acknowledgement);
        expect(privateHost.adapter.reportMatchResult).toHaveBeenCalledExactlyOnceWith(binding.matchId, 1);
        expect(warn).toHaveBeenCalledOnce();
      } finally {
        warn.mockRestore();
        await host.dispose();
      }
    },
  );

  it("releases pause and reconnect state when a disconnected seat becomes a bot or is kicked", async () => {
    const host = ephemeralHost("Host");
    const privateHost = host as unknown as {
      adapter: { replaceSeatWithBot: ReturnType<typeof vi.fn>; getViewForSeat: ReturnType<typeof vi.fn> };
      disconnectedSeats: Map<number, { disconnectedAt: number; timer: ReturnType<typeof setTimeout> | null }>;
      expiredDisconnectedSeats: Set<number>;
      seatTokens: Map<number, string>;
      seatNames: Map<number, string>;
      paused: boolean;
      replaceSeatWithBot: (seat: number) => Promise<void>;
      kickPlayerDurably: (seat: number, reason: string) => Promise<void>;
    };
    privateHost.adapter.replaceSeatWithBot = vi.fn(async () => ({}));
    privateHost.adapter.getViewForSeat = vi.fn(async () => ({ status: "Lobby" }));
    privateHost.disconnectedSeats.set(1, { disconnectedAt: Date.now(), timer: setTimeout(() => {}, 60_000) });
    privateHost.expiredDisconnectedSeats.add(1);
    privateHost.seatTokens.set(1, "guest-token");
    privateHost.seatNames.set(1, "Guest");
    privateHost.paused = true;

    await privateHost.replaceSeatWithBot(1);

    expect(privateHost.disconnectedSeats.has(1)).toBe(false);
    expect(privateHost.expiredDisconnectedSeats.has(1)).toBe(false);
    expect(privateHost.seatTokens.has(1)).toBe(false);
    expect(privateHost.seatNames.has(1)).toBe(false);
    expect(privateHost.paused).toBe(false);

    privateHost.disconnectedSeats.set(2, { disconnectedAt: Date.now(), timer: null });
    privateHost.paused = true;
    await privateHost.kickPlayerDurably(2, "Kicked");
    expect(privateHost.paused).toBe(false);
    await host.dispose();
  });

  it("refuses to start a lobby while a retained guest is reconnecting", async () => {
    const host = recoveredHost("Host");
    const privateHost = host as unknown as {
      adapter: { createMultiplayerDraft: ReturnType<typeof vi.fn> };
      disconnectedSeats: Map<number, { deadlineAt: number; timer: ReturnType<typeof setTimeout> | null }>;
      draftStarted: boolean;
    };
    privateHost.adapter.createMultiplayerDraft = vi.fn(async () => {});
    privateHost.disconnectedSeats.set(1, { deadlineAt: Date.now() + 60_000, timer: null });

    await expect(host.startDraft()).rejects.toThrow("Cannot start draft while a player is reconnecting");

    expect(privateHost.adapter.createMultiplayerDraft).not.toHaveBeenCalled();
    expect(privateHost.draftStarted).toBe(false);
    await host.dispose();
  });

  it("persists a deck receipt before ack and treats its exact retry as idempotent", async () => {
    const host = recoveredHost("Host");
    const privateHost = host as unknown as {
      adapter: {
        submitDeckForSeat: ReturnType<typeof vi.fn>;
        getViewForSeat: ReturnType<typeof vi.fn>;
        exportSession: ReturnType<typeof vi.fn>;
      };
      draftStarted: boolean;
      guestSessions: Map<number, { send: ReturnType<typeof vi.fn> }>;
      handleDeckSubmission: (seat: number, cards: string[], commanders: string[], submissionId: string) => Promise<unknown>;
    };
    const view = {
      status: "Deckbuilding",
      seats: [{ has_submitted_deck: false, is_bot: false }],
    };
    privateHost.draftStarted = true;
    privateHost.adapter.submitDeckForSeat = vi.fn(async () => view);
    privateHost.adapter.getViewForSeat = vi.fn(async () => view);
    privateHost.adapter.exportSession = vi.fn(async () => "{\"status\":\"Deckbuilding\"}");
    const session = { send: vi.fn(async () => {}) };
    privateHost.guestSessions.set(1, session);

    await privateHost.handleDeckSubmission(1, ["Island"], [], "submission-1");
    expect(saveDraftHostSession).toHaveBeenCalledWith("shared-recovery", expect.objectContaining({
      deckSubmissionReceipts: [{ seat: 1, submissionId: "submission-1", payloadFingerprint: expect.any(String) }],
    }));
    expect(saveDraftHostSession).toHaveBeenCalledBefore(session.send);
    expect(session.send).toHaveBeenCalledWith(expect.objectContaining({
      type: "draft_deck_submit_ack",
      submissionId: "submission-1",
    }));

    await privateHost.handleDeckSubmission(1, ["Island"], [], "submission-1");
    expect(privateHost.adapter.submitDeckForSeat).toHaveBeenCalledOnce();
  });

  it("reuses the host deck receipt after a failed immutable snapshot", async () => {
    const host = recoveredHost("Host");
    const privateHost = host as unknown as {
      adapter: {
        submitDeckForSeat: ReturnType<typeof vi.fn>;
        getViewForSeat: ReturnType<typeof vi.fn>;
        exportSession: ReturnType<typeof vi.fn>;
      };
      draftStarted: boolean;
    };
    const view = {
      status: "Deckbuilding",
      seats: [{ has_submitted_deck: false, is_bot: false }],
    };
    privateHost.draftStarted = true;
    privateHost.adapter.submitDeckForSeat = vi.fn(async () => view);
    privateHost.adapter.getViewForSeat = vi.fn(async () => view);
    privateHost.adapter.exportSession = vi.fn(async () => "{\"status\":\"Deckbuilding\"}");
    saveDraftHostSession
      .mockRejectedValueOnce(new Error("IDB unavailable"))
      .mockResolvedValue(undefined);

    await expect(host.submitHostDeck(["Island"], [])).rejects.toThrow("IDB unavailable");
    await host.submitHostDeck(["Island"], []);

    // Retrying invokes the durable receipt path and flushes its captured
    // snapshot; it does not invoke the deck reducer twice.
    expect(privateHost.adapter.submitDeckForSeat).toHaveBeenCalledOnce();
  });

  it("serializes concurrent host deck submits into one reducer command", async () => {
    const host = recoveredHost("Host");
    const privateHost = host as unknown as {
      adapter: {
        submitDeckForSeat: ReturnType<typeof vi.fn>;
        getViewForSeat: ReturnType<typeof vi.fn>;
        exportSession: ReturnType<typeof vi.fn>;
      };
      draftStarted: boolean;
    };
    const view = { status: "Deckbuilding", seats: [{ has_submitted_deck: false, is_bot: false }] };
    privateHost.draftStarted = true;
    privateHost.adapter.submitDeckForSeat = vi.fn(async () => view);
    privateHost.adapter.getViewForSeat = vi.fn(async () => view);
    privateHost.adapter.exportSession = vi.fn(async () => "{\"status\":\"Deckbuilding\"}");

    await Promise.all([host.submitHostDeck(["Island"], []), host.submitHostDeck(["Island"], [])]);

    expect(privateHost.adapter.submitDeckForSeat).toHaveBeenCalledOnce();
    expect(saveDraftHostSession).toHaveBeenCalledWith("shared-recovery", expect.objectContaining({
      deckSubmissionReceipts: [expect.objectContaining({ seat: 0 })],
    }));
  });

  it("rejects a host deck before draft start without invoking the reducer", async () => {
    const host = recoveredHost("Host");
    const privateHost = host as unknown as {
      adapter: { submitDeckForSeat: ReturnType<typeof vi.fn> };
    };
    privateHost.adapter.submitDeckForSeat = vi.fn();

    await expect(host.submitHostDeck(["Island"], [])).rejects.toThrow("Draft not started");

    expect(privateHost.adapter.submitDeckForSeat).not.toHaveBeenCalled();
  });

  it("retains a guest deck command after a failed save and replays it once after host recovery", async () => {
    const host = recoveredHost("Host");
    const privateHost = host as unknown as {
      adapter: {
        submitDeckForSeat: ReturnType<typeof vi.fn>;
        getViewForSeat: ReturnType<typeof vi.fn>;
        exportSession: ReturnType<typeof vi.fn>;
      };
      draftStarted: boolean;
      guestSessions: Map<number, { send: ReturnType<typeof vi.fn> }>;
      generatePairingsInner: ReturnType<typeof vi.fn>;
      handleDeckSubmission: (seat: number, cards: string[], commanders: string[], submissionId: string) => Promise<unknown>;
    };
    const view = { status: "Pairing", seats: [{ has_submitted_deck: true, is_bot: false }] };
    privateHost.draftStarted = true;
    privateHost.adapter.submitDeckForSeat = vi.fn(async () => view);
    privateHost.adapter.getViewForSeat = vi.fn(async () => view);
    privateHost.adapter.exportSession = vi.fn(async () => "{\"status\":\"Deckbuilding\"}");
    privateHost.generatePairingsInner = vi.fn(async () => {});
    const session = { send: vi.fn(async () => {}) };
    privateHost.guestSessions.set(1, session);
    saveDraftHostSession.mockRejectedValueOnce(new Error("IDB unavailable"));

    await expect(privateHost.handleDeckSubmission(1, ["Island"], [], "submission-1"))
      .rejects.toThrow("IDB unavailable");
    expect(session.send).toHaveBeenCalledWith(expect.objectContaining({
      type: "draft_error", submissionId: "submission-1", submissionDisposition: "Retryable",
    }));

    await privateHost.handleDeckSubmission(1, ["Island"], [], "submission-1");
    expect(privateHost.generatePairingsInner).toHaveBeenCalledOnce();
    const recoveredSnapshot = saveDraftHostSession.mock.calls[
      saveDraftHostSession.mock.calls.length - 1
    ]?.[1] as Record<string, unknown>;
    const recovered = recoveredHost("Host");
    await recovered.restoreFromPersisted({ ...recoveredSnapshot, draftSessionJson: null } as never);
    const recoveredPrivate = recovered as unknown as {
      adapter: {
        submitDeckForSeat: ReturnType<typeof vi.fn>;
        getViewForSeat: ReturnType<typeof vi.fn>;
        exportSession: ReturnType<typeof vi.fn>;
      };
      draftStarted: boolean;
      guestSessions: Map<number, { send: ReturnType<typeof vi.fn> }>;
      generatePairingsInner: ReturnType<typeof vi.fn>;
      handleDeckSubmission: (seat: number, cards: string[], commanders: string[], submissionId: string) => Promise<unknown>;
    };
    recoveredPrivate.adapter.submitDeckForSeat = vi.fn(async () => view);
    recoveredPrivate.adapter.getViewForSeat = vi.fn(async () => ({ ...view, status: "MatchInProgress" }));
    recoveredPrivate.adapter.exportSession = vi.fn(async () => "{\"status\":\"Deckbuilding\"}");
    recoveredPrivate.generatePairingsInner = vi.fn(async () => {});
    recoveredPrivate.guestSessions.set(1, { send: vi.fn(async () => {}) });
    await recoveredPrivate.handleDeckSubmission(1, ["Island"], [], "submission-1");

    expect(privateHost.adapter.submitDeckForSeat).toHaveBeenCalledOnce();
    expect(recoveredPrivate.adapter.submitDeckForSeat).not.toHaveBeenCalled();
    expect(recoveredPrivate.generatePairingsInner).not.toHaveBeenCalled();
  });

  it("generates pairings once for an ordinarily durable final deck submission", async () => {
    const host = recoveredHost("Host");
    const privateHost = host as unknown as {
      adapter: {
        submitDeckForSeat: ReturnType<typeof vi.fn>;
        getViewForSeat: ReturnType<typeof vi.fn>;
        exportSession: ReturnType<typeof vi.fn>;
      };
      draftStarted: boolean;
      generatePairingsInner: ReturnType<typeof vi.fn>;
    };
    const view = { status: "Pairing", seats: [{ has_submitted_deck: true, is_bot: false }] };
    privateHost.draftStarted = true;
    privateHost.adapter.submitDeckForSeat = vi.fn(async () => view);
    privateHost.adapter.getViewForSeat = vi.fn(async () => view);
    privateHost.adapter.exportSession = vi.fn(async () => "{\"status\":\"Pairing\"}");
    privateHost.generatePairingsInner = vi.fn(async () => {});

    await host.submitHostDeck(["Island"], []);

    expect(privateHost.adapter.submitDeckForSeat).toHaveBeenCalledOnce();
    expect(privateHost.generatePairingsInner).toHaveBeenCalledOnce();
  });

  it("keeps round advancement invisible until its first durable snapshot commits", async () => {
    const host = recoveredHost("Host");
    const privateHost = host as unknown as {
      adapter: { advanceRound: ReturnType<typeof vi.fn>; exportSession: ReturnType<typeof vi.fn> };
      draftStarted: boolean;
      generatePairingsInner: ReturnType<typeof vi.fn>;
    };
    privateHost.draftStarted = true;
    privateHost.adapter.advanceRound = vi.fn(async () => {});
    privateHost.adapter.exportSession = vi.fn(async () => "{\"status\":\"Pairing\"}");
    privateHost.generatePairingsInner = vi.fn(async () => {});
    const events = vi.fn();
    host.onEvent(events);
    let releaseSave!: () => void;
    saveDraftHostSession.mockImplementationOnce(() => new Promise<void>((resolve) => {
      releaseSave = resolve;
    }));

    const advance = host.advanceRound();
    await vi.waitFor(() => expect(saveDraftHostSession).toHaveBeenCalledOnce());
    expect(events).not.toHaveBeenCalledWith({ type: "roundAdvanced" });
    expect(privateHost.generatePairingsInner).not.toHaveBeenCalled();

    releaseSave();
    await advance;
    expect(events).toHaveBeenCalledWith({ type: "roundAdvanced" });
    expect(privateHost.generatePairingsInner).toHaveBeenCalledOnce();
  });

  it("persists recovered grace expiry once and never rearms an expired seat", async () => {
    vi.useFakeTimers();
    try {
      const host = recoveredHost("Host");
      const privateHost = host as unknown as {
        adapter: { setSeatConnected: ReturnType<typeof vi.fn>; exportSession: ReturnType<typeof vi.fn> };
        draftStarted: boolean;
        seatTokens: Map<number, string>;
        armRecoveredGuestGrace: (deadlines: Record<number, number>) => boolean;
        expiredDisconnectedSeats: Set<number>;
        disconnectedSeats: Map<number, unknown>;
      };
      privateHost.draftStarted = true;
      privateHost.seatTokens.set(1, "guest-token");
      privateHost.adapter.setSeatConnected = vi.fn(async () => {});
      privateHost.adapter.exportSession = vi.fn(async () => "{\"status\":\"Drafting\"}");
      privateHost.armRecoveredGuestGrace({ 1: Date.now() + 5 * 60_000 });

      await vi.advanceTimersByTimeAsync(5 * 60_000);
      expect(privateHost.expiredDisconnectedSeats.has(1)).toBe(true);
      const expiredSnapshot = saveDraftHostSession.mock.calls[
        saveDraftHostSession.mock.calls.length - 1
      ]?.[1] as Record<string, unknown>;
      expect(expiredSnapshot).toMatchObject({ expiredDisconnectedSeats: [1] });

      const recovered = recoveredHost("Host");
      await recovered.restoreFromPersisted({
        ...expiredSnapshot,
        draftSessionJson: null,
      } as never);
      const recoveredPrivate = recovered as unknown as {
        expiredDisconnectedSeats: Set<number>;
        disconnectedSeats: Map<number, unknown>;
        handleReconnect: (session: unknown, token: string) => Promise<void>;
      };
      expect(recoveredPrivate.expiredDisconnectedSeats.has(1)).toBe(true);
      expect(recoveredPrivate.disconnectedSeats.has(1)).toBe(false);
      const reconnectSession = {
        send: vi.fn(async () => {}),
        close: vi.fn(),
      };
      await recoveredPrivate.handleReconnect(reconnectSession, "guest-token");
      expect(reconnectSession.send).toHaveBeenCalledWith(expect.objectContaining({
        type: "draft_reconnect_rejected",
        kind: "NoReconnectWindow",
      }));
      const savesBeforeAdvance = saveDraftHostSession.mock.calls.length;
      await vi.advanceTimersByTimeAsync(5 * 60_000);
      expect(saveDraftHostSession).toHaveBeenCalledTimes(savesBeforeAdvance);
    } finally {
      vi.useRealTimers();
    }
  });

  it("durably revokes a lobby seat before acknowledging its explicit leave", async () => {
    const host = recoveredHost("Host");
    const privateHost = host as unknown as {
      guestSessions: Map<number, unknown>;
      seatTokens: Map<number, string>;
      seatNames: Map<number, string>;
      handleGuestMessage: (seat: number, message: unknown, session: unknown) => Promise<void>;
    };
    const session = { onDisconnect: vi.fn(() => vi.fn()), send: vi.fn(async () => {}), close: vi.fn() };
    privateHost.guestSessions.set(1, session);
    privateHost.seatTokens.set(1, "leave-token");
    privateHost.seatNames.set(1, "Guest");

    await privateHost.handleGuestMessage(1, {
      type: "draft_leave", draftProtocolVersion: DRAFT_PROTOCOL_VERSION, draftToken: "leave-token",
    }, session);

    expect(saveDraftHostSession).toHaveBeenCalledBefore(session.send);
    expect(session.send).toHaveBeenCalledWith({
      type: "draft_leave_ack", draftProtocolVersion: DRAFT_PROTOCOL_VERSION, draftToken: "leave-token",
    });
    expect(privateHost.seatTokens.has(1)).toBe(false);
    expect(privateHost.seatNames.has(1)).toBe(false);
  });

  it.each(["lobby", "started draft"] as const)(
    "publishes a durable %s leave when the departing connection closes before its acknowledgement",
    async (phase) => {
      vi.useFakeTimers();
      let acceptConnection!: Parameters<ConstructorParameters<typeof P2PDraftHost>[1]>[0];
      const host = new P2PDraftHost(
        { id: "host" } as never,
        (handler) => { acceptConnection = handler; return () => {}; },
        { type: "Set", data: { pools: [{ code: "TST" }], sequence: ["TST"] } } as never,
        "Premier", 3, "Host", "Swiss", "Competitive", undefined, "shared-recovery", "ABCDE",
      );
      const privateHost = host as unknown as {
        adapter: {
          draftProcedure: ReturnType<typeof vi.fn>;
          createMultiplayerDraft: ReturnType<typeof vi.fn>;
          setSeatConnected: ReturnType<typeof vi.fn>;
          getViewForSeat: ReturnType<typeof vi.fn>;
          exportSession: ReturnType<typeof vi.fn>;
        };
        guestSessions: Map<number, DraftPeerSession>;
        mutationQueue: Promise<void>;
        persistQueue: Promise<void>;
        timerContext: string | null;
        timerInterval: ReturnType<typeof setInterval> | null;
        frozenTimer: { context: string; remainingMs: number } | null;
        paused: boolean;
      };
      let draftView: DraftPlayerView;
      privateHost.adapter.draftProcedure = vi.fn(async () => ({
        packs_per_player: 3, min_deck_size: 40, launch_capability: "None", commanders_required: 0,
        pick_selection_mode: "Direct", match_config: { match_type: "Bo1" },
      }));
      privateHost.adapter.createMultiplayerDraft = vi.fn(async () => {});
      privateHost.adapter.setSeatConnected = vi.fn(async () => {});
      privateHost.adapter.getViewForSeat = vi.fn(async () => draftView);
      privateHost.adapter.exportSession = vi.fn(async () => JSON.stringify(draftView));
      const departing = new FakeDraftDataConnection();
      const remaining = new FakeDraftDataConnection();
      const events = vi.fn();
      host.onEvent(events);
      const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
      const error = vi.spyOn(console, "error").mockImplementation(() => {});
      let finishSave: (() => void) | undefined;
      try {
        await host.initialize();
        await privateHost.persistQueue;
        for (const [connection, displayName] of [[departing, "Alice"], [remaining, "Bea"]] as const) {
          acceptConnection(connection as never);
          await connection.receiveRaw(await encodeDraftWireMessage({
            type: "draft_join", draftProtocolVersion: DRAFT_PROTOCOL_VERSION, displayName,
          }));
          await privateHost.mutationQueue;
        }
        const welcome = (await Promise.all(departing.sentRaw.map(decodeDraftWireMessage)))
          .find((message) => message.type === "draft_welcome");
        if (!welcome || welcome.type !== "draft_welcome") throw new Error("Guest was not admitted");
        expect(welcome.seatIndex).toBe(1);
        expect(events).toHaveBeenCalledWith({ type: "seatJoined", seatIndex: 2, displayName: "Bea" });
        const session = privateHost.guestSessions.get(1);
        if (!session) throw new Error("Admitted guest has no session");
        const close = vi.spyOn(session, "close");

        if (phase === "started draft") {
          draftView = { ...await host.getHostView(), status: "Drafting" };
          await host.startDraft(false);
          expect(privateHost.adapter.createMultiplayerDraft).toHaveBeenCalledOnce();
          expect(privateHost.timerContext).toBe("pick");
          expect(privateHost.timerInterval).not.toBeNull();
          expect(privateHost.paused).toBe(false);
        }
        await vi.waitFor(async () => {
          const messages = await Promise.all(remaining.sentRaw.map(decodeDraftWireMessage));
          expect(messages).toContainEqual(expect.objectContaining(phase === "lobby"
            ? { type: "draft_lobby_update", joined: 3 }
            : { type: "draft_state_update", view: draftView }));
        });
        remaining.sentRaw.length = 0;
        events.mockClear();

        let snapshot: unknown;
        let saving!: () => void;
        const saveStarted = new Promise<void>((resolve) => { saving = resolve; });
        const saveGate = new Promise<void>((resolve) => { finishSave = resolve; });
        const committed = vi.fn();
        saveDraftHostSession.mockImplementationOnce(async (_id, saved) => {
          snapshot = saved;
          saving();
          await saveGate;
          committed();
        });
        await departing.receiveRaw(await encodeDraftWireMessage({
          type: "draft_leave", draftProtocolVersion: DRAFT_PROTOCOL_VERSION, draftToken: welcome.draftToken,
        }));
        await saveStarted;
        expect(privateHost.guestSessions.has(1)).toBe(false);
        expect(snapshot).toEqual(expect.objectContaining({
          seatTokens: { 2: expect.any(String) },
          kickedTokens: phase === "lobby" ? [] : [welcome.draftToken],
          expiredDisconnectedSeats: phase === "lobby" ? [] : [1],
        }));
        expect(committed).not.toHaveBeenCalled();
        expect(close).not.toHaveBeenCalled();
        departing.simulateClose();
        expect(departing.open).toBe(false);
        if (!finishSave) throw new Error("Persistence gate was not initialized");
        finishSave();
        await privateHost.mutationQueue;

        expect(committed).toHaveBeenCalledOnce();
        expect(close).toHaveBeenCalledExactlyOnceWith("Participant left draft");
        expect(warn).toHaveBeenCalledWith(
          "[P2PDraftHost] leave acknowledgement failed:", new Error("Draft connection is not open"),
        );
        expect(committed).toHaveBeenCalledBefore(close);
        expect(events).toHaveBeenCalledWith({ type: "seatDisconnected", seatIndex: 1 });
        expect(error).not.toHaveBeenCalled();
        await vi.waitFor(async () => {
          const messages = await Promise.all(remaining.sentRaw.map(decodeDraftWireMessage));
          if (phase === "lobby") {
            expect(messages).toContainEqual(expect.objectContaining({ type: "draft_lobby_update", joined: 2 }));
            expect(events).toHaveBeenCalledWith({ type: "lobbyUpdate", seats: expect.any(Array), joined: 2, total: 3 });
            expect((await host.getHostView()).seats[1]?.display_name).not.toBe("Alice");
          } else {
            expect(messages).toContainEqual({ type: "draft_state_update", view: draftView });
            expect(messages).toContainEqual({ type: "draft_paused", reason: DraftPauseReason.PlayerDisconnected });
            expect(events).toHaveBeenCalledWith({ type: "draftPaused", reason: DraftPauseReason.PlayerDisconnected });
            expect(privateHost.adapter.setSeatConnected).toHaveBeenCalledExactlyOnceWith(1, false);
            expect(privateHost.paused).toBe(true);
            expect(privateHost.timerInterval).toBeNull();
            expect(privateHost.timerContext).toBeNull();
            expect(privateHost.frozenTimer).toEqual(expect.objectContaining({ context: "pick", remainingMs: expect.any(Number) }));
            expect(privateHost.frozenTimer!.remainingMs).toBeGreaterThan(0);
          }
        });
      } finally {
        finishSave?.();
        await host.dispose();
        warn.mockRestore();
        error.mockRestore();
        vi.useRealTimers();
      }
    },
  );

  it("restores a leave's live recovery state when its durability fence fails", async () => {
    const host = recoveredHost("Host");
    const privateHost = host as unknown as {
      adapter: { setSeatConnected: ReturnType<typeof vi.fn>; exportSession: ReturnType<typeof vi.fn> };
      draftStarted: boolean;
      guestSessions: Map<number, unknown>;
      seatTokens: Map<number, string>;
      seatNames: Map<number, string>;
      perSeatWorkspaceSnapshots: Map<number, unknown>;
      expiredDisconnectedSeats: Set<number>;
      handleGuestMessage: (seat: number, message: unknown, session: unknown) => Promise<void>;
    };
    const session = { onDisconnect: vi.fn(() => vi.fn()), send: vi.fn(async () => {}), close: vi.fn() };
    const workspace = { schemaVersion: 1, placements: {}, virtualBasics: [] };
    privateHost.draftStarted = true;
    privateHost.adapter.setSeatConnected = vi.fn(async () => {});
    privateHost.adapter.exportSession = vi.fn(async () => "{\"status\":\"Drafting\"}");
    privateHost.guestSessions.set(1, session);
    privateHost.seatTokens.set(1, "leave-token");
    privateHost.seatNames.set(1, "Guest");
    privateHost.perSeatWorkspaceSnapshots.set(1, workspace);
    saveDraftHostSession.mockRejectedValueOnce(new Error("IDB unavailable"));

    await expect(privateHost.handleGuestMessage(1, {
      type: "draft_leave", draftProtocolVersion: DRAFT_PROTOCOL_VERSION, draftToken: "leave-token",
    }, session)).rejects.toThrow("IDB unavailable");

    expect(privateHost.guestSessions.get(1)).toBe(session);
    expect(privateHost.seatTokens.get(1)).toBe("leave-token");
    expect(privateHost.seatNames.get(1)).toBe("Guest");
    expect(privateHost.perSeatWorkspaceSnapshots.get(1)).toBe(workspace);
    expect(privateHost.expiredDisconnectedSeats.has(1)).toBe(false);
    expect(privateHost.adapter.setSeatConnected).toHaveBeenNthCalledWith(1, 1, false);
    expect(privateHost.adapter.setSeatConnected).toHaveBeenNthCalledWith(2, 1, true);
    expect(session.send).not.toHaveBeenCalled();
    expect(session.close).not.toHaveBeenCalled();
  });

  it("opens reconnect grace when the guest closes during a failed leave save", async () => {
    vi.useFakeTimers();
    try {
      const host = recoveredHost("Host");
      const privateHost = host as unknown as {
        adapter: { setSeatConnected: ReturnType<typeof vi.fn>; exportSession: ReturnType<typeof vi.fn> };
        draftStarted: boolean;
        paused: boolean;
        guestSessions: Map<number, unknown>;
        seatTokens: Map<number, string>;
        seatNames: Map<number, string>;
        perSeatWorkspaceSnapshots: Map<number, unknown>;
        disconnectedSeats: Map<number, { deadlineAt: number }>;
        reconnectDeadlines: Map<number, number>;
        handleGuestMessage: (seat: number, message: unknown, session: unknown) => Promise<void>;
      };
      let disconnect!: () => void;
      const session = {
        onDisconnect: vi.fn((handler: () => void) => {
          disconnect = handler;
          return vi.fn();
        }),
        send: vi.fn(async () => {}),
        close: vi.fn(),
      };
      const workspace = { schemaVersion: 1, placements: {}, virtualBasics: [] };
      privateHost.draftStarted = true;
      privateHost.adapter.setSeatConnected = vi.fn(async () => {});
      privateHost.adapter.exportSession = vi.fn(async () => "{\"status\":\"Drafting\"}");
      privateHost.guestSessions.set(1, session);
      privateHost.seatTokens.set(1, "leave-token");
      privateHost.seatNames.set(1, "Guest");
      privateHost.perSeatWorkspaceSnapshots.set(1, workspace);
      saveDraftHostSession.mockImplementationOnce(() => {
        disconnect();
        return Promise.reject(new Error("IDB unavailable"));
      });

      await expect(privateHost.handleGuestMessage(1, {
        type: "draft_leave", draftProtocolVersion: DRAFT_PROTOCOL_VERSION, draftToken: "leave-token",
      }, session)).rejects.toThrow("IDB unavailable");

      const deadlineAt = privateHost.disconnectedSeats.get(1)?.deadlineAt;
      expect(deadlineAt).toBe(Date.now() + 60_000);
      expect(privateHost.reconnectDeadlines.get(1)).toBe(deadlineAt);
      expect(privateHost.guestSessions.has(1)).toBe(false);
      expect(privateHost.seatTokens.get(1)).toBe("leave-token");
      expect(privateHost.seatNames.get(1)).toBe("Guest");
      expect(privateHost.perSeatWorkspaceSnapshots.get(1)).toBe(workspace);
      expect(privateHost.adapter.setSeatConnected).toHaveBeenNthCalledWith(1, 1, false);
      expect(privateHost.adapter.setSeatConnected).toHaveBeenNthCalledWith(2, 1, false);
      expect(privateHost.paused).toBe(true);
      expect(session.send).not.toHaveBeenCalled();
    } finally {
      vi.clearAllTimers();
      vi.useRealTimers();
    }
  });

  it("pauses a started draft when leave recovery's follow-up save also fails", async () => {
    vi.useFakeTimers();
    try {
      const host = recoveredHost("Host");
      const privateHost = host as unknown as {
        adapter: { setSeatConnected: ReturnType<typeof vi.fn>; exportSession: ReturnType<typeof vi.fn> };
        draftStarted: boolean;
        paused: boolean;
        guestSessions: Map<number, unknown>;
        seatTokens: Map<number, string>;
        seatNames: Map<number, string>;
        perSeatWorkspaceSnapshots: Map<number, unknown>;
        disconnectedSeats: Map<number, { deadlineAt: number }>;
        handleGuestMessage: (seat: number, message: unknown, session: unknown) => Promise<void>;
      };
      let disconnect!: () => void;
      const session = {
        onDisconnect: vi.fn((handler: () => void) => {
          disconnect = handler;
          return vi.fn();
        }),
        send: vi.fn(async () => {}),
        close: vi.fn(),
      };
      privateHost.draftStarted = true;
      privateHost.adapter.setSeatConnected = vi.fn(async () => {});
      privateHost.adapter.exportSession = vi.fn(async () => "{\"status\":\"Drafting\"}");
      privateHost.guestSessions.set(1, session);
      privateHost.seatTokens.set(1, "leave-token");
      privateHost.seatNames.set(1, "Guest");
      privateHost.perSeatWorkspaceSnapshots.set(1, { schemaVersion: 1, placements: {}, virtualBasics: [] });
      saveDraftHostSession
        .mockImplementationOnce(() => {
          disconnect();
          return Promise.reject(new Error("IDB unavailable"));
        })
        .mockRejectedValueOnce(new Error("IDB still unavailable"));

      await expect(privateHost.handleGuestMessage(1, {
        type: "draft_leave", draftProtocolVersion: DRAFT_PROTOCOL_VERSION, draftToken: "leave-token",
      }, session)).rejects.toThrow("IDB unavailable");

      expect(privateHost.adapter.setSeatConnected).toHaveBeenNthCalledWith(1, 1, false);
      expect(privateHost.adapter.setSeatConnected).toHaveBeenNthCalledWith(2, 1, false);
      expect(privateHost.paused).toBe(true);
      expect(privateHost.disconnectedSeats.has(1)).toBe(true);
      expect(privateHost.guestSessions.has(1)).toBe(false);
      expect(session.send).not.toHaveBeenCalled();
      expect(session.close).not.toHaveBeenCalled();
    } finally {
      vi.clearAllTimers();
      vi.useRealTimers();
    }
  });

  it("retains an existing reconnect deadline when a failed leave session closes", async () => {
    vi.useFakeTimers();
    try {
      const host = recoveredHost("Host");
      const privateHost = host as unknown as {
        guestSessions: Map<number, unknown>;
        seatTokens: Map<number, string>;
        seatNames: Map<number, string>;
        disconnectedSeats: Map<number, { deadlineAt: number }>;
        reconnectDeadlines: Map<number, number>;
        handleGuestMessage: (seat: number, message: unknown, session: unknown) => Promise<void>;
      };
      let disconnect!: () => void;
      const session = {
        onDisconnect: vi.fn((handler: () => void) => {
          disconnect = handler;
          return vi.fn();
        }),
        send: vi.fn(async () => {}),
        close: vi.fn(),
      };
      const existingDeadline = Date.now() + 15_000;
      privateHost.guestSessions.set(1, session);
      privateHost.seatTokens.set(1, "leave-token");
      privateHost.seatNames.set(1, "Guest");
      privateHost.reconnectDeadlines.set(1, existingDeadline);
      saveDraftHostSession.mockImplementationOnce(() => {
        disconnect();
        return Promise.reject(new Error("IDB unavailable"));
      });

      await expect(privateHost.handleGuestMessage(1, {
        type: "draft_leave", draftProtocolVersion: DRAFT_PROTOCOL_VERSION, draftToken: "leave-token",
      }, session)).rejects.toThrow("IDB unavailable");

      expect(privateHost.disconnectedSeats.get(1)?.deadlineAt).toBe(existingDeadline);
      expect(privateHost.reconnectDeadlines.get(1)).toBe(existingDeadline);
      expect(privateHost.guestSessions.has(1)).toBe(false);
    } finally {
      vi.clearAllTimers();
      vi.useRealTimers();
    }
  });

  it("does not replay a rolled-back leave snapshot on the next save", async () => {
    const host = recoveredHost("Host");
    const privateHost = host as unknown as {
      adapter: { setSeatConnected: ReturnType<typeof vi.fn>; exportSession: ReturnType<typeof vi.fn> };
      draftStarted: boolean;
      guestSessions: Map<number, unknown>;
      seatTokens: Map<number, string>;
      seatNames: Map<number, string>;
      expiredDisconnectedSeats: Set<number>;
      persistQueue: Promise<void>;
      persistSession: () => void;
      handleGuestMessage: (seat: number, message: unknown, session: unknown) => Promise<void>;
    };
    const session = { onDisconnect: vi.fn(() => vi.fn()), send: vi.fn(async () => {}), close: vi.fn() };
    privateHost.draftStarted = true;
    privateHost.adapter.setSeatConnected = vi.fn(async () => {});
    privateHost.adapter.exportSession = vi.fn(async () => "{\"status\":\"Drafting\"}");
    privateHost.guestSessions.set(1, session);
    privateHost.seatTokens.set(1, "leave-token");
    privateHost.seatNames.set(1, "Guest");
    saveDraftHostSession.mockRejectedValueOnce(new Error("IDB unavailable"));

    await expect(privateHost.handleGuestMessage(1, {
      type: "draft_leave", draftProtocolVersion: DRAFT_PROTOCOL_VERSION, draftToken: "leave-token",
    }, session)).rejects.toThrow("IDB unavailable");

    expect(saveDraftHostSession).toHaveBeenCalledWith("shared-recovery", expect.objectContaining({
      seatTokens: {},
      kickedTokens: ["leave-token"],
      expiredDisconnectedSeats: [1],
    }));

    privateHost.persistSession();
    await privateHost.persistQueue;

    // The failed leave was compensated, so the next durable snapshot must be
    // the recovered pre-leave state, not a retry of the revoked capability.
    expect(saveDraftHostSession).toHaveBeenCalledTimes(2);
    expect(saveDraftHostSession).toHaveBeenLastCalledWith("shared-recovery", expect.objectContaining({
      seatTokens: { 1: "leave-token" },
      seatNames: expect.objectContaining({ 1: "Guest" }),
      kickedTokens: [],
      expiredDisconnectedSeats: [],
    }));
  });

  it("compensates an engine disconnect rejection without revoking recovery", async () => {
    const host = recoveredHost("Host");
    const privateHost = host as unknown as {
      adapter: { setSeatConnected: ReturnType<typeof vi.fn> };
      draftStarted: boolean;
      guestSessions: Map<number, unknown>;
      seatTokens: Map<number, string>;
      seatNames: Map<number, string>;
      expiredDisconnectedSeats: Set<number>;
      handleGuestMessage: (seat: number, message: unknown, session: unknown) => Promise<void>;
    };
    const session = { onDisconnect: vi.fn(() => vi.fn()), send: vi.fn(async () => {}), close: vi.fn() };
    privateHost.draftStarted = true;
    privateHost.adapter.setSeatConnected = vi.fn()
      .mockRejectedValueOnce(new Error("disconnect rejected"))
      .mockResolvedValueOnce(undefined);
    privateHost.guestSessions.set(1, session);
    privateHost.seatTokens.set(1, "leave-token");
    privateHost.seatNames.set(1, "Guest");

    await expect(privateHost.handleGuestMessage(1, {
      type: "draft_leave", draftProtocolVersion: DRAFT_PROTOCOL_VERSION, draftToken: "leave-token",
    }, session)).rejects.toThrow("disconnect rejected");

    expect(privateHost.adapter.setSeatConnected).toHaveBeenNthCalledWith(1, 1, false);
    expect(privateHost.adapter.setSeatConnected).toHaveBeenNthCalledWith(2, 1, true);
    expect(privateHost.guestSessions.get(1)).toBe(session);
    expect(privateHost.seatTokens.get(1)).toBe("leave-token");
    expect(privateHost.seatNames.get(1)).toBe("Guest");
    expect(privateHost.expiredDisconnectedSeats.has(1)).toBe(false);
  });

  it("retains recovery state and reports a rejected engine rollback", async () => {
    const host = recoveredHost("Host");
    const privateHost = host as unknown as {
      adapter: { setSeatConnected: ReturnType<typeof vi.fn>; exportSession: ReturnType<typeof vi.fn> };
      draftStarted: boolean;
      guestSessions: Map<number, unknown>;
      seatTokens: Map<number, string>;
      seatNames: Map<number, string>;
      expiredDisconnectedSeats: Set<number>;
      handleGuestMessage: (seat: number, message: unknown, session: unknown) => Promise<void>;
    };
    const session = { onDisconnect: vi.fn(() => vi.fn()), send: vi.fn(async () => {}), close: vi.fn() };
    const events = vi.fn();
    host.onEvent(events);
    privateHost.draftStarted = true;
    privateHost.adapter.setSeatConnected = vi.fn()
      .mockResolvedValueOnce(undefined)
      .mockRejectedValueOnce(new Error("reconnect rejected"));
    privateHost.adapter.exportSession = vi.fn(async () => "{\"status\":\"Drafting\"}");
    privateHost.guestSessions.set(1, session);
    privateHost.seatTokens.set(1, "leave-token");
    privateHost.seatNames.set(1, "Guest");
    saveDraftHostSession.mockRejectedValueOnce(new Error("IDB unavailable"));

    await expect(privateHost.handleGuestMessage(1, {
      type: "draft_leave", draftProtocolVersion: DRAFT_PROTOCOL_VERSION, draftToken: "leave-token",
    }, session)).rejects.toThrow("IDB unavailable");

    expect(privateHost.adapter.setSeatConnected).toHaveBeenNthCalledWith(1, 1, false);
    expect(privateHost.adapter.setSeatConnected).toHaveBeenNthCalledWith(2, 1, true);
    expect(privateHost.guestSessions.get(1)).toBe(session);
    expect(privateHost.seatTokens.get(1)).toBe("leave-token");
    expect(privateHost.seatNames.get(1)).toBe("Guest");
    expect(privateHost.expiredDisconnectedSeats.has(1)).toBe(false);
    expect(events).toHaveBeenCalledWith({
      type: "error",
      message: "leave rollback connectivity failed: reconnect rejected",
    });
  });

  it("does not extend a reconnect deadline across repeated reconnect drops", async () => {
    vi.useFakeTimers();
    try {
      const host = recoveredHost("Host");
      const privateHost = host as unknown as {
        adapter: {
          setSeatConnected: ReturnType<typeof vi.fn>;
          exportSession: ReturnType<typeof vi.fn>;
          getViewForSeat: ReturnType<typeof vi.fn>;
        };
        draftStarted: boolean;
        guestSessions: Map<number, unknown>;
        seatTokens: Map<number, string>;
        disconnectedSeats: Map<number, { deadlineAt: number }>;
        mutationQueue: Promise<void>;
        handleGuestDisconnect: (seat: number) => void;
        handleReconnect: (session: unknown, token: string) => Promise<void>;
      };
      const view = { status: "Drafting", pool: [], seats: [] };
      privateHost.draftStarted = true;
      privateHost.adapter.setSeatConnected = vi.fn(async () => {});
      privateHost.adapter.exportSession = vi.fn(async () => "{\"status\":\"Drafting\"}");
      privateHost.adapter.getViewForSeat = vi.fn(async () => view);
      privateHost.seatTokens.set(1, "guest-token");
      privateHost.guestSessions.set(1, { send: vi.fn(async () => {}), close: vi.fn() });

      privateHost.handleGuestDisconnect(1);
      await privateHost.mutationQueue;
      const deadlineAt = privateHost.disconnectedSeats.get(1)?.deadlineAt;
      expect(deadlineAt).toBeDefined();

      const firstReconnect = { onMessage: vi.fn(), send: vi.fn(async () => {}), close: vi.fn() };
      await privateHost.handleReconnect(firstReconnect, "guest-token");
      await vi.advanceTimersByTimeAsync(5_000);
      privateHost.handleGuestDisconnect(1);
      expect(privateHost.disconnectedSeats.get(1)?.deadlineAt).toBe(deadlineAt);
      await privateHost.mutationQueue;

      const secondReconnect = { onMessage: vi.fn(), send: vi.fn(async () => {}), close: vi.fn() };
      await privateHost.handleReconnect(secondReconnect, "guest-token");
      await vi.advanceTimersByTimeAsync(5_000);
      privateHost.handleGuestDisconnect(1);
      expect(privateHost.disconnectedSeats.get(1)?.deadlineAt).toBe(deadlineAt);
      await host.dispose();
    } finally {
      vi.useRealTimers();
    }
  });

  it("makes a stale prior session inert after the seat reconnects", async () => {
    const host = recoveredHost("Host");
    const privateHost = host as unknown as {
      guestSessions: Map<number, unknown>;
      seatTokens: Map<number, string>;
      handleGuestMessage: (seat: number, message: unknown, session: unknown) => Promise<void>;
    };
    const current = { send: vi.fn(async () => {}), close: vi.fn() };
    const stale = { send: vi.fn(async () => {}), close: vi.fn() };
    privateHost.guestSessions.set(1, current);
    privateHost.seatTokens.set(1, "leave-token");

    await privateHost.handleGuestMessage(1, {
      type: "draft_leave", draftProtocolVersion: DRAFT_PROTOCOL_VERSION, draftToken: "leave-token",
    }, stale);

    expect(privateHost.seatTokens.get(1)).toBe("leave-token");
    expect(current.send).not.toHaveBeenCalled();
  });

  it("turns a post-start leave into a terminal paused human seat without a bot", async () => {
    const host = recoveredHost("Host");
    const privateHost = host as unknown as {
      adapter: {
        setSeatConnected: ReturnType<typeof vi.fn>;
        exportSession: ReturnType<typeof vi.fn>;
        getViewForSeat: ReturnType<typeof vi.fn>;
      };
      draftStarted: boolean;
      guestSessions: Map<number, unknown>;
      seatTokens: Map<number, string>;
      seatNames: Map<number, string>;
      expiredDisconnectedSeats: Set<number>;
      paused: boolean;
      handleGuestMessage: (seat: number, message: unknown, session: unknown) => Promise<void>;
      isBotSeat: (seat: number) => boolean;
    };
    const session = { onDisconnect: vi.fn(() => vi.fn()), send: vi.fn(async () => {}), close: vi.fn() };
    privateHost.draftStarted = true;
    privateHost.adapter.setSeatConnected = vi.fn(async () => {});
    privateHost.adapter.exportSession = vi.fn(async () => "{\"status\":\"Drafting\"}");
    privateHost.adapter.getViewForSeat = vi.fn(async () => ({ status: "Drafting", seats: [] }));
    privateHost.guestSessions.set(1, session);
    privateHost.seatTokens.set(1, "leave-token");
    privateHost.seatNames.set(1, "Guest");

    await privateHost.handleGuestMessage(1, {
      type: "draft_leave", draftProtocolVersion: DRAFT_PROTOCOL_VERSION, draftToken: "leave-token",
    }, session);

    expect(privateHost.adapter.setSeatConnected).toHaveBeenCalledWith(1, false);
    expect(privateHost.expiredDisconnectedSeats.has(1)).toBe(true);
    expect(privateHost.seatTokens.has(1)).toBe(false);
    expect(privateHost.seatNames.get(1)).toBe("Guest");
    expect(privateHost.isBotSeat(1)).toBe(false);
    expect(privateHost.paused).toBe(true);
  });

  it("commits a joining guest's token before welcome and preserves it for recovery", async () => {
    const host = recoveredHost("Host");
    const privateHost = host as unknown as AdmissionHost;
    privateHost.procedure = { packs_per_player: 3, min_deck_size: 40 };
    let initialSnapshot: Record<string, unknown> | undefined;
    let snapshot: Record<string, unknown> | undefined;
    let finishSave!: () => void;
    saveDraftHostSession
      .mockImplementationOnce((_id, value) => {
        initialSnapshot = value as Record<string, unknown>;
        return Promise.resolve();
      })
      .mockImplementationOnce((_id, value) => {
        snapshot = value as Record<string, unknown>;
        return new Promise<void>((resolve) => { finishSave = resolve; });
      });
    // `initialize()` queues this lobby save before accepting connections.
    // It must remain a pre-admission snapshot even if the join reaches the
    // queue before its earlier write gets CPU time.
    privateHost.persistSession();
    const session = {
      onMessage: vi.fn(),
      onDisconnect: vi.fn(() => vi.fn()),
      send: vi.fn(async () => {}),
      close: vi.fn(),
    };

    const admission = privateHost.handleNewGuest(session, "Alice");
    await vi.waitFor(() => expect(saveDraftHostSession).toHaveBeenCalledTimes(2));

    expect(initialSnapshot).toMatchObject({
      seatTokens: {},
      seatNames: { 0: "Host" },
    });

    // A reload while the strict admission write is pending sees neither an
    // acknowledged guest nor a live guest transport. The capability cannot
    // escape this fence.
    expect(session.send).not.toHaveBeenCalled();
    expect(privateHost.guestSessions.size).toBe(0);

    finishSave();
    await admission;

    const token = privateHost.seatTokens.get(1);
    expect(token).toBeDefined();
    expect(snapshot).toMatchObject({
      seatTokens: { 1: token },
      seatNames: { 1: "Alice" },
    });
    expect(session.send).toHaveBeenCalledWith(expect.objectContaining({
      type: "draft_welcome",
      draftToken: token,
    }));

    const recovered = recoveredHost("Host");
    await recovered.restoreFromPersisted(snapshot as never);
    const recoveredPrivate = recovered as unknown as AdmissionHost;
    recoveredPrivate.procedure = { packs_per_player: 3, min_deck_size: 40 };
    const reconnectSession = {
      onMessage: vi.fn(),
      send: vi.fn(async () => {}),
      close: vi.fn(),
    };
    await recoveredPrivate.handleReconnect(reconnectSession, token!);
    await Promise.resolve();
    expect(reconnectSession.send).toHaveBeenCalledWith(expect.objectContaining({
      type: "draft_reconnect_ack",
      seatIndex: 1,
    }));
  });

  it("does not attach or acknowledge a guest when admission persistence fails", async () => {
    vi.spyOn(console, "warn").mockImplementation(() => {});
    saveDraftHostSession.mockRejectedValueOnce(new Error("IDB unavailable"));
    const host = recoveredHost("Host");
    const privateHost = host as unknown as AdmissionHost;
    privateHost.procedure = { packs_per_player: 3, min_deck_size: 40 };
    const session = {
      onMessage: vi.fn(),
      onDisconnect: vi.fn(() => vi.fn()),
      send: vi.fn(async () => {}),
      close: vi.fn(),
    };

    await privateHost.handleNewGuest(session, "Alice");

    expect(privateHost.seatTokens.has(1)).toBe(false);
    expect(privateHost.guestSessions.size).toBe(0);
    expect(session.onMessage).not.toHaveBeenCalled();
    expect(session.send).not.toHaveBeenCalled();
    expect(session.close).toHaveBeenCalledWith("Guest admission persistence failed");
  });

  it("rejects a pre-start deck submission with its command identity", async () => {
    const host = recoveredHost("Host");
    const privateHost = host as unknown as {
      guestSessions: Map<number, { send: ReturnType<typeof vi.fn> }>;
      handleGuestMessage: (seat: number, message: unknown) => Promise<void>;
    };
    const session = { send: vi.fn(async () => {}) };
    privateHost.guestSessions.set(1, session);

    await privateHost.handleGuestMessage(1, {
      type: "draft_submit_deck", submissionId: "submission-1", mainDeck: ["Island"],
    });

    expect(session.send).toHaveBeenCalledWith({
      type: "draft_error",
      reason: "Draft not started",
      submissionId: "submission-1",
      submissionDisposition: "Rejected",
    });
  });

  it("rolls back a first-contact disconnect that happens during durable admission", async () => {
    const host = recoveredHost("Host");
    const privateHost = host as unknown as AdmissionHost;
    privateHost.procedure = { packs_per_player: 3, min_deck_size: 40 };
    let finishAdmissionWrite!: () => void;
    let rollbackSnapshot: Record<string, unknown> | undefined;
    saveDraftHostSession
      .mockImplementationOnce(() => new Promise<void>((resolve) => { finishAdmissionWrite = resolve; }))
      .mockImplementationOnce((_id, snapshot) => {
        rollbackSnapshot = snapshot as Record<string, unknown>;
        return Promise.resolve();
      });
    let disconnect!: () => void;
    const session = {
      onMessage: vi.fn(),
      onDisconnect: vi.fn((handler: () => void) => {
        disconnect = handler;
        return vi.fn();
      }),
      send: vi.fn(async () => {}),
      close: vi.fn(),
    };

    const admission = privateHost.handleNewGuest(session, "Alice");
    await vi.waitFor(() => expect(saveDraftHostSession).toHaveBeenCalledOnce());
    disconnect();
    finishAdmissionWrite();
    await admission;

    expect(privateHost.seatTokens.has(1)).toBe(false);
    expect(privateHost.guestSessions.size).toBe(0);
    expect(session.onMessage).not.toHaveBeenCalled();
    expect(session.send).not.toHaveBeenCalled();
    expect(rollbackSnapshot).toMatchObject({ seatTokens: {}, seatNames: { 0: "Host" } });
  });

  it("does not let a queued admission persist a failed predecessor's token", async () => {
    vi.spyOn(console, "warn").mockImplementation(() => {});
    let secondSnapshot: Record<string, unknown> | undefined;
    saveDraftHostSession
      .mockRejectedValueOnce(new Error("first write failed"))
      .mockImplementationOnce((_id, snapshot) => {
        secondSnapshot = snapshot as Record<string, unknown>;
        return Promise.resolve();
      });
    const host = recoveredHost("Host");
    const privateHost = host as unknown as AdmissionHost;
    privateHost.procedure = { packs_per_player: 3, min_deck_size: 40 };
    const firstSession = {
      onMessage: vi.fn(),
      onDisconnect: vi.fn(() => vi.fn()),
      send: vi.fn(async () => {}),
      close: vi.fn(),
    };
    const secondSession = {
      onMessage: vi.fn(),
      onDisconnect: vi.fn(() => vi.fn()),
      send: vi.fn(async () => {}),
      close: vi.fn(),
    };

    await Promise.all([
      privateHost.handleNewGuest(firstSession, "Alice"),
      privateHost.handleNewGuest(secondSession, "Bea"),
    ]);

    expect(firstSession.send).not.toHaveBeenCalled();
    expect(secondSession.send).toHaveBeenCalledWith(expect.objectContaining({ type: "draft_welcome" }));
    expect(secondSnapshot).toMatchObject({
      seatNames: { 0: "Host", 1: "Bea" },
    });
    expect(Object.keys(secondSnapshot!.seatTokens as Record<string, string>)).toEqual(["1"]);
  });

  it("does not admit a queued guest that disconnects before its transaction begins", async () => {
    const host = recoveredHost("Host");
    const privateHost = host as unknown as AdmissionHost;
    privateHost.procedure = { packs_per_player: 3, min_deck_size: 40 };
    const events = vi.fn();
    host.onEvent(events);
    let finishFirstAdmission!: () => void;
    let snapshot: Record<string, unknown> | undefined;
    saveDraftHostSession.mockImplementationOnce((_id, value) => {
      snapshot = value as Record<string, unknown>;
      return new Promise<void>((resolve) => { finishFirstAdmission = resolve; });
    });
    const firstSession = {
      onMessage: vi.fn(),
      onDisconnect: vi.fn(() => vi.fn()),
      send: vi.fn(async () => {}),
      close: vi.fn(),
    };
    let disconnectSecond!: () => void;
    const secondSession = {
      onMessage: vi.fn(),
      onDisconnect: vi.fn((handler: () => void) => {
        disconnectSecond = handler;
        return vi.fn();
      }),
      send: vi.fn(async () => {}),
      close: vi.fn(),
    };

    const first = privateHost.handleNewGuest(firstSession, "Alice");
    const second = privateHost.handleNewGuest(secondSession, "Bea");
    await vi.waitFor(() => expect(saveDraftHostSession).toHaveBeenCalledOnce());
    disconnectSecond();
    finishFirstAdmission();
    await Promise.all([first, second]);

    expect(snapshot).toMatchObject({
      seatNames: { 0: "Host", 1: "Alice" },
    });
    expect(privateHost.seatTokens.size).toBe(1);
    expect(secondSession.onMessage).not.toHaveBeenCalled();
    expect(secondSession.send).not.toHaveBeenCalled();
    expect(events).toHaveBeenCalledTimes(2);
    expect(events).toHaveBeenNthCalledWith(1, { type: "seatJoined", seatIndex: 1, displayName: "Alice" });
    expect(events).toHaveBeenNthCalledWith(2, expect.objectContaining({ type: "lobbyUpdate" }));
  });

  it("rolls back a close injected during guest-session registration", async () => {
    const host = recoveredHost("Host");
    const privateHost = host as unknown as AdmissionHost;
    privateHost.procedure = { packs_per_player: 3, min_deck_size: 40 };
    let rollbackSnapshot: Record<string, unknown> | undefined;
    saveDraftHostSession
      .mockResolvedValueOnce()
      .mockImplementationOnce((_id, snapshot) => {
        rollbackSnapshot = snapshot as Record<string, unknown>;
        return Promise.resolve();
      });
    let disconnect!: () => void;
    const session = {
      onMessage: vi.fn(),
      onDisconnect: vi.fn((handler: () => void) => {
        disconnect = handler;
        return vi.fn();
      }),
      send: vi.fn(async () => {}),
      close: vi.fn(),
    };
    const originalSet = privateHost.guestSessions.set.bind(privateHost.guestSessions);
    vi.spyOn(privateHost.guestSessions, "set").mockImplementation((seat, guestSession) => {
      const result = originalSet(seat, guestSession);
      disconnect();
      return result;
    });

    await privateHost.handleNewGuest(session, "Alice");

    expect(privateHost.seatTokens.size).toBe(0);
    expect(privateHost.guestSessions.size).toBe(0);
    expect(session.onMessage).not.toHaveBeenCalled();
    expect(session.send).not.toHaveBeenCalled();
    expect(rollbackSnapshot).toMatchObject({ seatTokens: {}, seatNames: { 0: "Host" } });
  });
});
