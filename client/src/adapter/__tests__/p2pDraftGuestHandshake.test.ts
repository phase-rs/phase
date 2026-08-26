import { describe, expect, it, vi } from "vitest";

const sessionState = vi.hoisted(() => ({
  sessions: [] as Array<{
    handler: ((message: unknown) => void) | null;
    send: ReturnType<typeof vi.fn>;
    close: ReturnType<typeof vi.fn>;
  }>,
}));

const persistenceState = vi.hoisted(() => ({
  clearDraftGuestRecovery: vi.fn(async () => {}),
  saveActiveDraftGuest: vi.fn(),
  saveDraftGuestSession: vi.fn(async () => {}),
}));

vi.mock("../../network/draftPeerSession", () => ({
  createDraftPeerSession: vi.fn(() => {
    const session = {
      handler: null as ((message: unknown) => void) | null,
      send: vi.fn(async () => {}),
      close: vi.fn(),
    };
    sessionState.sessions.push(session);
    return {
      onMessage: vi.fn((handler: (message: unknown) => void) => {
        session.handler = handler;
        return vi.fn();
      }),
      onDisconnect: vi.fn(() => vi.fn()),
      send: session.send,
      close: session.close,
    };
  }),
}));

vi.mock("../../services/draftPersistence", () => persistenceState);

import { P2PDraftGuest } from "../p2p-draft-guest";
import { DRAFT_PROTOCOL_VERSION, validateDraftMessage } from "../../network/draftProtocol";

const reconnectAck = {
  type: "draft_reconnect_ack",
  draftProtocolVersion: DRAFT_PROTOCOL_VERSION,
  seatIndex: 2,
  draftCode: "draft-xyz",
  view: { status: "Deckbuilding", draft_effects: [], seats: [] },
};

describe("P2P draft guest handshake attempts", () => {
  it("does not publish a reload locator until the guest token has committed", async () => {
    sessionState.sessions.length = 0;
    persistenceState.saveActiveDraftGuest.mockClear();
    persistenceState.saveDraftGuestSession.mockClear();
    let finishIdbWrite!: () => void;
    persistenceState.saveDraftGuestSession.mockImplementationOnce(() => new Promise<void>((resolve) => {
      finishIdbWrite = resolve;
    }));
    const guest = new P2PDraftGuest(
      {} as never,
      "phase2-ABCDE",
      {} as never,
      { kind: "new", roomCode: "ABCDE", displayName: "Alice" },
    );
    const privateGuest = guest as unknown as {
      handshakeOn: (connection: unknown, signal: AbortSignal | undefined, reconnect: boolean) => Promise<void>;
    };

    let settled = false;
    const handshake = privateGuest.handshakeOn({} as never, undefined, false)
      .then(() => { settled = true; });
    await Promise.resolve();
    sessionState.sessions[0]!.handler!({
      type: "draft_welcome",
      draftProtocolVersion: DRAFT_PROTOCOL_VERSION,
      draftToken: "opaque-token",
      seatIndex: 2,
      draftCode: "draft-xyz",
      view: { status: "Deckbuilding", draft_effects: [], seats: [] },
    });
    await Promise.resolve();

    // A reload in this gap sees no locator and therefore cannot attempt a
    // reconnect that lacks a committed token.
    expect(persistenceState.saveActiveDraftGuest).not.toHaveBeenCalled();
    expect(settled).toBe(false);

    finishIdbWrite();
    await handshake;
    expect(persistenceState.saveDraftGuestSession).toHaveBeenCalledBefore(
      persistenceState.saveActiveDraftGuest,
    );
    expect(persistenceState.saveActiveDraftGuest).toHaveBeenCalledWith({
      roomCode: "ABCDE",
      displayName: "Alice",
      hostPeerId: "phase2-ABCDE",
    });
  });

  it("waits for token persistence before completing a reconnect acknowledgement", async () => {
    sessionState.sessions.length = 0;
    persistenceState.saveActiveDraftGuest.mockClear();
    persistenceState.saveDraftGuestSession.mockClear();
    let finishIdbWrite!: () => void;
    persistenceState.saveDraftGuestSession.mockImplementationOnce(() => new Promise<void>((resolve) => {
      finishIdbWrite = resolve;
    }));
    const guest = new P2PDraftGuest(
      {} as never,
      "phase2-ABCDE",
      {} as never,
      { kind: "reconnect", roomCode: "ABCDE", displayName: "Alice", draftToken: "opaque-token" },
    );
    const privateGuest = guest as unknown as {
      handshakeOn: (connection: unknown, signal: AbortSignal | undefined, reconnect: boolean) => Promise<void>;
    };

    let settled = false;
    const handshake = privateGuest.handshakeOn({} as never, undefined, true)
      .then(() => { settled = true; });
    await Promise.resolve();
    sessionState.sessions[0]!.handler!(reconnectAck);
    await Promise.resolve();

    expect(persistenceState.saveActiveDraftGuest).not.toHaveBeenCalled();
    expect(settled).toBe(false);

    finishIdbWrite();
    await handshake;
    expect(persistenceState.saveDraftGuestSession).toHaveBeenCalledBefore(
      persistenceState.saveActiveDraftGuest,
    );
  });

  it("does not publish a locator when the token write fails", async () => {
    sessionState.sessions.length = 0;
    persistenceState.saveActiveDraftGuest.mockClear();
    persistenceState.saveDraftGuestSession.mockRejectedValueOnce(new Error("IDB unavailable"));
    const guest = new P2PDraftGuest(
      {} as never,
      "phase2-ABCDE",
      {} as never,
      { kind: "new", roomCode: "ABCDE", displayName: "Alice" },
    );
    const privateGuest = guest as unknown as {
      handshakeOn: (connection: unknown, signal: AbortSignal | undefined, reconnect: boolean) => Promise<void>;
    };

    const handshake = privateGuest.handshakeOn({} as never, undefined, false);
    await Promise.resolve();
    sessionState.sessions[0]!.handler!({
      type: "draft_welcome",
      draftProtocolVersion: DRAFT_PROTOCOL_VERSION,
      draftToken: "opaque-token",
      seatIndex: 2,
      draftCode: "draft-xyz",
      view: { status: "Deckbuilding", draft_effects: [], seats: [] },
    });

    await expect(handshake).rejects.toThrow("IDB unavailable");
    expect(persistenceState.saveActiveDraftGuest).not.toHaveBeenCalled();
  });

  it("rejects an incompatible welcome immediately with typed non-retryable recovery", async () => {
    sessionState.sessions.length = 0;
    const events: unknown[] = [];
    const guest = new P2PDraftGuest(
      {} as never,
      "phase2-ABCDE",
      {} as never,
      { kind: "reconnect", roomCode: "ABCDE", displayName: "Alice", draftToken: "opaque-token" },
    );
    guest.onEvent((event) => events.push(event));
    const privateGuest = guest as unknown as {
      handshakeOn: (connection: unknown, signal: AbortSignal | undefined, reconnect: boolean) => Promise<void>;
    };

    const handshake = privateGuest.handshakeOn({} as never, undefined, true);
    await Promise.resolve();
    sessionState.sessions[0]!.handler!({
      ...reconnectAck,
      draftProtocolVersion: DRAFT_PROTOCOL_VERSION - 1,
    });

    await expect(handshake).rejects.toThrow("Draft protocol mismatch");
    expect(events).toContainEqual(expect.objectContaining({
      type: "reconnectFailed",
      failure: expect.objectContaining({ kind: "incompatible" }),
    }));
  });

  it("treats a v14 rejection as terminal without revoking recovery credentials", async () => {
    sessionState.sessions.length = 0;
    persistenceState.clearDraftGuestRecovery.mockClear();
    const guest = new P2PDraftGuest(
      {} as never,
      "phase2-ABCDE",
      {} as never,
      { kind: "reconnect", roomCode: "ABCDE", displayName: "Alice", draftToken: "opaque-token" },
    );
    const privateGuest = guest as unknown as {
      handshakeOn: (connection: unknown, signal: AbortSignal | undefined, reconnect: boolean) => Promise<void>;
      terminated: boolean;
    };

    const attempt = privateGuest.handshakeOn({} as never, undefined, true);
    await Promise.resolve();
    sessionState.sessions[0]!.handler!(validateDraftMessage({
      type: "draft_reconnect_rejected",
      reason: "Refresh to reconnect",
    }));

    await expect(attempt).rejects.toThrow("Refresh to reconnect");
    expect(privateGuest.terminated).toBe(true);
    expect(persistenceState.clearDraftGuestRecovery).not.toHaveBeenCalled();
  });

  it("cannot let a delayed acknowledgement from a retired NoReconnectWindow attempt promote a newer attempt", async () => {
    sessionState.sessions.length = 0;
    const guest = new P2PDraftGuest(
      {} as never,
      "phase2-ABCDE",
      {} as never,
      { kind: "reconnect", roomCode: "ABCDE", displayName: "Alice", draftToken: "opaque-token" },
    );
    const privateGuest = guest as unknown as {
      handshakeOn: (connection: unknown, signal: AbortSignal | undefined, reconnect: boolean) => Promise<void>;
    };

    const first = privateGuest.handshakeOn({} as never, undefined, true);
    await Promise.resolve();
    sessionState.sessions[0]!.handler!({
      type: "draft_reconnect_rejected",
      kind: "NoReconnectWindow",
      reason: "Another connection still owns this seat",
    });
    await expect(first).rejects.toThrow("Another connection");
    expect(sessionState.sessions[0]!.close).toHaveBeenCalled();

    let secondSettled = false;
    const second = privateGuest.handshakeOn({} as never, undefined, true)
      .then(() => { secondSettled = true; });
    await Promise.resolve();

    // A late ack from the retired transport is ignored by the active-session
    // identity gate rather than resolving the newer handshake.
    sessionState.sessions[0]!.handler!(reconnectAck);
    await Promise.resolve();
    expect(secondSettled).toBe(false);

    sessionState.sessions[1]!.handler!(reconnectAck);
    await expect(second).resolves.toBeUndefined();
    expect(secondSettled).toBe(true);
  });
});
