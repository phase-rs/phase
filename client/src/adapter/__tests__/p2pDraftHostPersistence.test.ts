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
