import { beforeEach, describe, expect, it, vi } from "vitest";

type SavedDeckSubmission = {
  hostPeerId: string;
  draftCode: string;
  roomCode: string;
  draftToken: string;
  submissionId: string;
  mainDeck: string[];
  commanders: string[];
  timestamp: number;
};

const sessionState = vi.hoisted(() => ({
  sessions: [] as Array<{
    handler: ((message: unknown) => void) | null;
    end: (() => void) | null;
    send: ReturnType<typeof vi.fn>;
    close: ReturnType<typeof vi.fn>;
  }>,
}));

const persistenceState = vi.hoisted(() => ({
  clearDraftGuestRecovery: vi.fn(async () => {}),
  clearDraftDeckSubmission: vi.fn(async () => {}),
  loadDraftDeckSubmission: vi.fn<
    (hostPeerId: string, identity?: { roomCode: string; draftToken: string }) => Promise<SavedDeckSubmission | null>
  >(async () => null),
  saveDraftDeckSubmission: vi.fn(async () => {}),
  saveActiveDraftGuest: vi.fn(),
  saveDraftGuestSession: vi.fn(async () => {}),
}));

vi.mock("../../network/draftPeerSession", () => ({
  createDraftPeerSession: vi.fn((_connection: unknown, options: { onSessionEnd: () => void }) => {
    const session = {
      handler: null as ((message: unknown) => void) | null,
      end: options.onSessionEnd,
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
import { PEER_CONNECT_OPTIONS } from "../../network/connection";
import { DRAFT_PROTOCOL_VERSION, validateDraftMessage } from "../../network/draftProtocol";

const reconnectAck = {
  type: "draft_reconnect_ack",
  draftProtocolVersion: DRAFT_PROTOCOL_VERSION,
  seatIndex: 2,
  draftCode: "draft-xyz",
  view: { launch_capability: "None", status: "Deckbuilding", draft_effects: [], seats: [] },
};

describe("P2P draft guest handshake attempts", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    sessionState.sessions.length = 0;
    persistenceState.loadDraftDeckSubmission.mockResolvedValue(null);
  });

  it("keeps an ordered duplicate commander designation in the participant outbox until its matching receipt", async () => {
    sessionState.sessions.length = 0;
    persistenceState.loadDraftDeckSubmission.mockResolvedValue(null);
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
      view: { launch_capability: "None", status: "Deckbuilding", draft_effects: [], seats: [] },
    });
    await handshake;

    const commanders = ["The Prismatic Piper", "The Prismatic Piper"];
    const submitted = guest.submitDeck(["Island"], commanders);
    await vi.waitFor(() => expect(persistenceState.saveDraftDeckSubmission).toHaveBeenCalledOnce());
    expect(persistenceState.saveDraftDeckSubmission).toHaveBeenCalledWith(
      "phase2-ABCDE",
      expect.objectContaining({ mainDeck: ["Island"], commanders }),
    );
    const sent = sessionState.sessions[0]!.send.mock.calls.find(
      ([message]) => (message as { type?: string }).type === "draft_submit_deck",
    )?.[0] as { submissionId: string; mainDeck: string[]; commanders: string[] };
    expect(sent).toMatchObject({ mainDeck: ["Island"], commanders });
    const sendIndex = sessionState.sessions[0]!.send.mock.calls.findIndex(
      ([message]) => (message as { type?: string }).type === "draft_submit_deck",
    );
    expect(persistenceState.saveDraftDeckSubmission.mock.invocationCallOrder[
      persistenceState.saveDraftDeckSubmission.mock.invocationCallOrder.length - 1
    ]!)
      .toBeLessThan(sessionState.sessions[0]!.send.mock.invocationCallOrder[sendIndex]!);

    sessionState.sessions[0]!.handler!({
      type: "draft_deck_submit_ack",
      submissionId: sent.submissionId,
      view: { launch_capability: "None", status: "Deckbuilding", draft_effects: [], seats: [] },
    });
    await submitted;
    expect(persistenceState.clearDraftDeckSubmission).toHaveBeenCalledWith(
      "phase2-ABCDE",
      sent.submissionId,
    );
  });

  it("serializes two rapid deck submits into one outbox command and acknowledgement", async () => {
    sessionState.sessions.length = 0;
    persistenceState.loadDraftDeckSubmission.mockResolvedValue(null);
    const guest = new P2PDraftGuest(
      {} as never, "phase2-ABCDE", {} as never,
      { kind: "new", roomCode: "ABCDE", displayName: "Alice" },
    );
    const privateGuest = guest as unknown as {
      handshakeOn: (connection: unknown, signal: AbortSignal | undefined, reconnect: boolean) => Promise<void>;
    };
    const handshake = privateGuest.handshakeOn({} as never, undefined, false);
    await Promise.resolve();
    sessionState.sessions[0]!.handler!({
      type: "draft_welcome", draftProtocolVersion: DRAFT_PROTOCOL_VERSION,
      draftToken: "opaque-token", seatIndex: 2, draftCode: "draft-xyz",
      view: { launch_capability: "None", status: "Deckbuilding", draft_effects: [], seats: [] },
    });
    await handshake;
    await Promise.resolve();

    let releaseLoad!: () => void;
    persistenceState.loadDraftDeckSubmission.mockClear();
    persistenceState.loadDraftDeckSubmission.mockImplementationOnce(() => new Promise<null>((resolve) => {
      releaseLoad = () => resolve(null);
    }));

    const first = guest.submitDeck(["Island"], []);
    const second = guest.submitDeck(["Island"], []);
    expect(second).toBe(first);
    expect(persistenceState.loadDraftDeckSubmission).toHaveBeenCalledTimes(1);
    releaseLoad();
    await vi.waitFor(() => expect(persistenceState.saveDraftDeckSubmission).toHaveBeenCalledOnce());
    await vi.waitFor(() => expect(sessionState.sessions[0]!.send.mock.calls.some(
      ([message]) => (message as { type?: string }).type === "draft_submit_deck",
    )).toBe(true));
    const commands = sessionState.sessions[0]!.send.mock.calls.filter(
      ([message]) => (message as { type?: string }).type === "draft_submit_deck",
    );
    expect(commands).toHaveLength(1);
    const command = commands[0]![0] as { submissionId: string };
    sessionState.sessions[0]!.handler!({
      type: "draft_deck_submit_ack", submissionId: command.submissionId,
      view: { launch_capability: "None", status: "Deckbuilding", draft_effects: [], seats: [] },
    });
    await expect(Promise.all([first, second])).resolves.toEqual([undefined, undefined]);
  });

  it("releases only the rejected deck command so a corrected deck gets a new id", async () => {
    sessionState.sessions.length = 0;
    persistenceState.loadDraftDeckSubmission.mockResolvedValue(null);
    const guest = new P2PDraftGuest(
      {} as never, "phase2-ABCDE", {} as never,
      { kind: "new", roomCode: "ABCDE", displayName: "Alice" },
    );
    const privateGuest = guest as unknown as {
      handshakeOn: (connection: unknown, signal: AbortSignal | undefined, reconnect: boolean) => Promise<void>;
    };
    const handshake = privateGuest.handshakeOn({} as never, undefined, false);
    await Promise.resolve();
    sessionState.sessions[0]!.handler!({
      type: "draft_welcome", draftProtocolVersion: DRAFT_PROTOCOL_VERSION,
      draftToken: "opaque-token", seatIndex: 2, draftCode: "draft-xyz",
      view: { launch_capability: "None", status: "Deckbuilding", draft_effects: [], seats: [] },
    });
    await handshake;
    const first = guest.submitDeck(["Island"], []);
    await vi.waitFor(() => expect(sessionState.sessions[0]!.send.mock.calls.some(
      ([message]) => (message as { type?: string }).type === "draft_submit_deck",
    )).toBe(true));
    const firstCommand = sessionState.sessions[0]!.send.mock.calls.find(
      ([message]) => (message as { type?: string }).type === "draft_submit_deck",
    )?.[0] as { submissionId: string };
    sessionState.sessions[0]!.handler!({
      type: "draft_error", submissionId: firstCommand.submissionId, reason: "Deck too small",
      submissionDisposition: "Rejected",
    });
    await expect(first).rejects.toThrow("Deck too small");
    expect(persistenceState.clearDraftDeckSubmission).toHaveBeenCalledWith(
      "phase2-ABCDE", firstCommand.submissionId,
    );
  });

  it("retains a deck outbox when the host reports a retryable durable failure", async () => {
    sessionState.sessions.length = 0;
    persistenceState.loadDraftDeckSubmission.mockResolvedValue(null);
    persistenceState.clearDraftDeckSubmission.mockClear();
    const guest = new P2PDraftGuest(
      {} as never, "phase2-ABCDE", {} as never,
      { kind: "new", roomCode: "ABCDE", displayName: "Alice" },
    );
    const privateGuest = guest as unknown as {
      handshakeOn: (connection: unknown, signal: AbortSignal | undefined, reconnect: boolean) => Promise<void>;
    };
    const handshake = privateGuest.handshakeOn({} as never, undefined, false);
    await Promise.resolve();
    sessionState.sessions[0]!.handler!({
      type: "draft_welcome", draftProtocolVersion: DRAFT_PROTOCOL_VERSION,
      draftToken: "opaque-token", seatIndex: 2, draftCode: "draft-xyz",
      view: { launch_capability: "None", status: "Deckbuilding", draft_effects: [], seats: [] },
    });
    await handshake;
    const submitted = guest.submitDeck(["Island"], []);
    await vi.waitFor(() => expect(sessionState.sessions[0]!.send.mock.calls.some(
      ([message]) => (message as { type?: string }).type === "draft_submit_deck",
    )).toBe(true));
    const command = sessionState.sessions[0]!.send.mock.calls.find(
      ([message]) => (message as { type?: string }).type === "draft_submit_deck",
    )?.[0] as { submissionId: string };
    sessionState.sessions[0]!.handler!({
      type: "draft_error", submissionId: command.submissionId, reason: "IDB unavailable",
      submissionDisposition: "Retryable",
    });
    await expect(submitted).rejects.toThrow("IDB unavailable");
    expect(persistenceState.clearDraftDeckSubmission).not.toHaveBeenCalledWith(
      "phase2-ABCDE", command.submissionId,
    );
  });

  it("reconnect replay resolves the original deck submission promise", async () => {
    sessionState.sessions.length = 0;
    persistenceState.loadDraftDeckSubmission.mockResolvedValue(null);
    const guest = new P2PDraftGuest(
      {} as never, "phase2-ABCDE", {} as never,
      { kind: "new", roomCode: "ABCDE", displayName: "Alice" },
    );
    const privateGuest = guest as unknown as {
      handshakeOn: (connection: unknown, signal: AbortSignal | undefined, reconnect: boolean) => Promise<void>;
    };
    const initialHandshake = privateGuest.handshakeOn({} as never, undefined, false);
    await Promise.resolve();
    sessionState.sessions[0]!.handler!({
      type: "draft_welcome", draftProtocolVersion: DRAFT_PROTOCOL_VERSION,
      draftToken: "opaque-token", seatIndex: 2, draftCode: "draft-xyz",
      view: { launch_capability: "None", status: "Deckbuilding", draft_effects: [], seats: [] },
    });
    await initialHandshake;
    const submitted = guest.submitDeck(["Island"], []);
    await vi.waitFor(() => expect(sessionState.sessions[0]!.send.mock.calls.some(
      ([message]) => (message as { type?: string }).type === "draft_submit_deck",
    )).toBe(true));
    const command = sessionState.sessions[0]!.send.mock.calls.find(
      ([message]) => (message as { type?: string }).type === "draft_submit_deck",
    )?.[0] as { submissionId: string };
    persistenceState.loadDraftDeckSubmission.mockResolvedValue({
      hostPeerId: "phase2-ABCDE", draftCode: "draft-xyz", roomCode: "ABCDE",
      draftToken: "opaque-token", submissionId: command.submissionId,
      mainDeck: ["Island"], commanders: [], timestamp: Date.now(),
    });

    const reconnect = privateGuest.handshakeOn({} as never, undefined, true);
    await Promise.resolve();
    sessionState.sessions[1]!.handler!(reconnectAck);
    await reconnect;
    await vi.waitFor(() => expect(sessionState.sessions[1]!.send).toHaveBeenCalledWith(
      expect.objectContaining({ type: "draft_submit_deck", submissionId: command.submissionId }),
    ));
    sessionState.sessions[1]!.handler!({
      type: "draft_deck_submit_ack", submissionId: command.submissionId,
      view: { launch_capability: "None", status: "Deckbuilding", draft_effects: [], seats: [] },
    });
    await submitted;
  });

  it("does not replay an outbox belonging to a different pod on the same host peer", async () => {
    sessionState.sessions.length = 0;
    persistenceState.loadDraftDeckSubmission.mockImplementation(async (_hostPeerId: string, identity?: {
      roomCode: string;
      draftToken: string;
    }) => {
      if (identity?.roomCode === "OLD12" && identity.draftToken === "old-token") {
        return {
          hostPeerId: "phase2-ABCDE",
          draftCode: "old-pod",
          roomCode: "OLD12",
          draftToken: "old-token",
          submissionId: "old-submission",
          mainDeck: ["Island"],
          commanders: [],
          timestamp: Date.now(),
        };
      }
      return null;
    });
    const guest = new P2PDraftGuest(
      {} as never, "phase2-ABCDE", {} as never,
      { kind: "new", roomCode: "ABCDE", displayName: "Alice" },
    );
    const privateGuest = guest as unknown as {
      handshakeOn: (connection: unknown, signal: AbortSignal | undefined, reconnect: boolean) => Promise<void>;
    };
    const handshake = privateGuest.handshakeOn({} as never, undefined, false);
    await Promise.resolve();
    sessionState.sessions[0]!.handler!({
      type: "draft_welcome", draftProtocolVersion: DRAFT_PROTOCOL_VERSION,
      draftToken: "new-token", seatIndex: 2, draftCode: "new-pod",
      view: { launch_capability: "None", status: "Deckbuilding", draft_effects: [], seats: [] },
    });
    await handshake;
    await vi.waitFor(() => expect(persistenceState.loadDraftDeckSubmission).toHaveBeenCalledWith(
      "phase2-ABCDE", { roomCode: "ABCDE", draftToken: "new-token" },
    ));
    expect(sessionState.sessions[0]!.send).not.toHaveBeenCalledWith(expect.objectContaining({
      type: "draft_submit_deck",
    }));
  });

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
      view: { launch_capability: "None", status: "Deckbuilding", draft_effects: [], seats: [] },
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

  it("clears recovery and the deck outbox only after the host acknowledges leave", async () => {
    const guest = new P2PDraftGuest(
      { destroy: vi.fn() } as never,
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
      type: "draft_welcome", draftProtocolVersion: DRAFT_PROTOCOL_VERSION,
      draftToken: "leave-token", seatIndex: 2, draftCode: "draft-xyz",
      view: { launch_capability: "None", status: "Lobby", draft_effects: [], seats: [] }, workspaceState: null,
    });
    await handshake;

    const leave = guest.leave();
    await vi.waitFor(() => expect(sessionState.sessions[0]!.send).toHaveBeenCalledWith({
      type: "draft_leave", draftProtocolVersion: DRAFT_PROTOCOL_VERSION, draftToken: "leave-token",
    }));
    expect(persistenceState.clearDraftGuestRecovery).not.toHaveBeenCalled();
    expect(persistenceState.clearDraftDeckSubmission).not.toHaveBeenCalled();

    sessionState.sessions[0]!.handler!({
      type: "draft_leave_ack", draftProtocolVersion: DRAFT_PROTOCOL_VERSION, draftToken: "leave-token",
    });
    await leave;
    expect(persistenceState.clearDraftGuestRecovery).toHaveBeenCalledWith("phase2-ABCDE");
    expect(persistenceState.clearDraftDeckSubmission).toHaveBeenCalledWith("phase2-ABCDE");
  });

  it("keeps a dropped leave acknowledgement recoverable and permits a later leave", async () => {
    const events: unknown[] = [];
    const guest = new P2PDraftGuest(
      { destroy: vi.fn() } as never,
      "phase2-ABCDE",
      {} as never,
      { kind: "new", roomCode: "ABCDE", displayName: "Alice" },
    );
    guest.onEvent((event) => events.push(event));
    const privateGuest = guest as unknown as {
      handshakeOn: (connection: unknown, signal: AbortSignal | undefined, reconnect: boolean) => Promise<void>;
    };
    const handshake = privateGuest.handshakeOn({} as never, undefined, false);
    await Promise.resolve();
    sessionState.sessions[0]!.handler!({
      type: "draft_welcome", draftProtocolVersion: DRAFT_PROTOCOL_VERSION,
      draftToken: "leave-token", seatIndex: 2, draftCode: "draft-xyz",
      view: { launch_capability: "None", status: "Lobby", draft_effects: [], seats: [] }, workspaceState: null,
    });
    await handshake;

    const abandonedLeave = guest.leave();
    await vi.waitFor(() => expect(sessionState.sessions[0]!.send).toHaveBeenCalledWith({
      type: "draft_leave", draftProtocolVersion: DRAFT_PROTOCOL_VERSION, draftToken: "leave-token",
    }));
    sessionState.sessions[0]!.end!();

    await expect(abandonedLeave).rejects.toThrow("disconnected before acknowledging leave");
    expect(guest.token).toBe("leave-token");
    expect(events).toContainEqual({ type: "reconnecting", attempt: 1 });
    expect(persistenceState.clearDraftGuestRecovery).not.toHaveBeenCalled();

    const reconnect = privateGuest.handshakeOn({} as never, undefined, true);
    await Promise.resolve();
    sessionState.sessions[1]!.handler!(reconnectAck);
    await reconnect;

    const laterLeave = guest.leave();
    await vi.waitFor(() => expect(sessionState.sessions[1]!.send).toHaveBeenCalledWith({
      type: "draft_leave", draftProtocolVersion: DRAFT_PROTOCOL_VERSION, draftToken: "leave-token",
    }));
    sessionState.sessions[1]!.handler!({
      type: "draft_leave_ack", draftProtocolVersion: DRAFT_PROTOCOL_VERSION, draftToken: "leave-token",
    });
    await laterLeave;
    expect(persistenceState.clearDraftGuestRecovery).toHaveBeenCalledWith("phase2-ABCDE");
  });

  it("clears recovery and the deck outbox when the host ends the draft", async () => {
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
      type: "draft_welcome", draftProtocolVersion: DRAFT_PROTOCOL_VERSION,
      draftToken: "host-left-token", seatIndex: 2, draftCode: "draft-xyz",
      view: { launch_capability: "None", status: "Lobby", draft_effects: [], seats: [] }, workspaceState: null,
    });
    await handshake;
    persistenceState.clearDraftGuestRecovery.mockClear();
    persistenceState.clearDraftDeckSubmission.mockClear();

    sessionState.sessions[0]!.handler!({ type: "draft_host_left", reason: "Host left" });
    await vi.waitFor(() => expect(persistenceState.clearDraftGuestRecovery).toHaveBeenCalledWith("phase2-ABCDE"));
    expect(persistenceState.clearDraftDeckSubmission).toHaveBeenCalledWith("phase2-ABCDE");
    await vi.waitFor(() => expect(guest.isRecoveryRevoked).toBe(true));
  });

  it.each([
    ["draft_kicked", { type: "draft_kicked", reason: "Removed from draft" }, { type: "kicked", reason: "Removed from draft" }],
    ["draft_host_left", { type: "draft_host_left", reason: "Host left" }, { type: "hostLeft", reason: "Host left" }],
  ])("settles a pending leave when the host sends %s", async (_messageType, message, terminalEvent) => {
    const events: unknown[] = [];
    const guest = new P2PDraftGuest(
      {} as never,
      "phase2-ABCDE",
      {} as never,
      { kind: "new", roomCode: "ABCDE", displayName: "Alice" },
    );
    guest.onEvent((event) => events.push(event));
    const privateGuest = guest as unknown as {
      handshakeOn: (connection: unknown, signal: AbortSignal | undefined, reconnect: boolean) => Promise<void>;
    };
    const handshake = privateGuest.handshakeOn({} as never, undefined, false);
    await Promise.resolve();
    sessionState.sessions[0]!.handler!({
      type: "draft_welcome", draftProtocolVersion: DRAFT_PROTOCOL_VERSION,
      draftToken: "leave-token", seatIndex: 2, draftCode: "draft-xyz",
      view: { launch_capability: "None", status: "Lobby", draft_effects: [], seats: [] }, workspaceState: null,
    });
    await handshake;

    const leave = guest.leave();
    await vi.waitFor(() => expect(sessionState.sessions[0]!.send).toHaveBeenCalledWith({
      type: "draft_leave", draftProtocolVersion: DRAFT_PROTOCOL_VERSION, draftToken: "leave-token",
    }));
    sessionState.sessions[0]!.handler!(message);

    await expect(leave).resolves.toBeUndefined();
    expect(events).toContainEqual(terminalEvent);
  });

  it("finishes terminal host handling when recovery cleanup fails", async () => {
    const events: unknown[] = [];
    const guest = new P2PDraftGuest(
      {} as never,
      "phase2-ABCDE",
      {} as never,
      { kind: "new", roomCode: "ABCDE", displayName: "Alice" },
    );
    guest.onEvent((event) => events.push(event));
    const privateGuest = guest as unknown as {
      handshakeOn: (connection: unknown, signal: AbortSignal | undefined, reconnect: boolean) => Promise<void>;
    };
    const handshake = privateGuest.handshakeOn({} as never, undefined, false);
    await Promise.resolve();
    sessionState.sessions[0]!.handler!({
      type: "draft_welcome", draftProtocolVersion: DRAFT_PROTOCOL_VERSION,
      draftToken: "host-left-token", seatIndex: 2, draftCode: "draft-xyz",
      view: { launch_capability: "None", status: "Lobby", draft_effects: [], seats: [] }, workspaceState: null,
    });
    await handshake;
    persistenceState.clearDraftGuestRecovery.mockRejectedValueOnce(new Error("storage unavailable"));
    persistenceState.clearDraftDeckSubmission.mockRejectedValueOnce(new Error("outbox unavailable"));

    sessionState.sessions[0]!.handler!({ type: "draft_host_left", reason: "Host left" });

    await vi.waitFor(() => expect(guest.isRecoveryRevoked).toBe(true));
    expect(events).toContainEqual({ type: "hostLeft", reason: "Host left" });
    expect(persistenceState.clearDraftGuestRecovery).toHaveBeenCalledWith("phase2-ABCDE");
    expect(persistenceState.clearDraftDeckSubmission).toHaveBeenCalledWith("phase2-ABCDE");
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
      view: { launch_capability: "None", status: "Deckbuilding", draft_effects: [], seats: [] },
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

  it("dials the reconnect transport with the shared ordered-channel connect options", async () => {
    // Unlike every other test here, this one needs a real `guestPeer`: the dial
    // is `this.guestPeer.connect(...)`, which the shared `{} as never` peer
    // cannot answer.
    const connHandlers = new Map<string, (arg?: unknown) => void>();
    const reconnectConn = {
      on: vi.fn((event: string, handler: (arg?: unknown) => void) => {
        connHandlers.set(event, handler);
      }),
    };
    const connect = vi.fn(() => reconnectConn);
    const guest = new P2PDraftGuest(
      { connect } as never,
      "phase2-ABCDE",
      {} as never,
      { kind: "reconnect", roomCode: "ABCDE", displayName: "Alice", draftToken: "opaque-token" },
    );
    const privateGuest = guest as unknown as {
      openReconnectConnection: (signal?: AbortSignal) => Promise<unknown>;
    };

    const dial = privateGuest.openReconnectConnection();

    // `reliable: true` is what PeerJS maps to `createDataChannel(…, { ordered
    // }) `; an option-less dial silently yields an unordered channel that a
    // TURN relay will actually reorder.
    expect(connect).toHaveBeenCalledWith("phase2-ABCDE", PEER_CONNECT_OPTIONS);

    connHandlers.get("open")!();
    await expect(dial).resolves.toBe(reconnectConn);
  });
});
