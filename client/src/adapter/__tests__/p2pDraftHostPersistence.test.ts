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

function recoveredHost(hostDisplayName: string): P2PDraftHost {
  return new P2PDraftHost(
    { id: hostDisplayName } as never,
    () => () => {},
    { type: "Set", data: { set_pool_json: "{}" } } as never,
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
    { type: "Set", data: { set_pool_json: "{}" } } as never,
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
        armRecoveredGuestGrace: () => void;
        expiredDisconnectedSeats: Set<number>;
        disconnectedSeats: Map<number, unknown>;
      };
      privateHost.draftStarted = true;
      privateHost.seatTokens.set(1, "guest-token");
      privateHost.adapter.setSeatConnected = vi.fn(async () => {});
      privateHost.adapter.exportSession = vi.fn(async () => "{\"status\":\"Drafting\"}");
      privateHost.armRecoveredGuestGrace();

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
