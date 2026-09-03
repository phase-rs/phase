import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type Peer from "peerjs";
import type { PersistedDraftDeckSubmission } from "../../services/draftPersistence";

const persistence = vi.hoisted(() => ({
  saveDraftGuestSession: vi.fn(async () => {}),
  saveActiveDraftGuest: vi.fn(),
  clearDraftGuestRecovery: vi.fn(async () => {}),
  clearDraftDeckSubmission: vi.fn(async () => {}),
  loadDraftDeckSubmission: vi.fn(async (): Promise<PersistedDraftDeckSubmission | null> => null),
  saveDraftDeckSubmission: vi.fn(async () => {}),
}));

vi.mock("../../services/draftPersistence", () => persistence);

import { EMPTY_DRAFT_POOL_GROUPS, type DraftPlayerView } from "../draft-adapter";
import { P2PDraftGuest, type DraftGuestConnection, type DraftGuestEvent } from "../p2p-draft-guest";
import { DRAFT_PROTOCOL_VERSION, decodeDraftWireMessage, type DraftP2PMessage } from "../../network/draftProtocol";
import * as protocol from "../../network/draftProtocol";
import { FakeDraftDataConnection } from "../../network/__tests__/fakeDraftDataConnection";
import type { DraftWorkspaceState } from "../../components/draft/workspace/types";

function deferred() {
  let resolve!: () => void;
  const promise = new Promise<void>((complete) => { resolve = complete; });
  return { promise, resolve };
}

/** Use the supported raw frame so only persistence, not decompression, yields. */
function rawMessage(message: DraftP2PMessage): Uint8Array {
  const json = new TextEncoder().encode(JSON.stringify(message));
  const bytes = new Uint8Array(json.length + 1);
  bytes.set(json, 1);
  return bytes;
}

function view(pickNumber: number): DraftPlayerView {
  return {
    status: "Drafting",
    kind: "Premier",
    launch_capability: "None",
    current_pack_number: 0,
    pick_number: pickNumber,
    pass_direction: "Left",
    current_pack: [{
      instance_id: `pack-${pickNumber}-card`,
      name: "Island",
      set_code: "TST",
      collector_number: "1",
      rarity: "common",
      colors: [],
      cmc: 0,
      type_line: "Basic Land — Island",
    }],
    required_pick_count: 1,
    pick_selection_mode: "Direct",
    pool: [],
    draft_effects: [],
    pool_groups: EMPTY_DRAFT_POOL_GROUPS,
    seats: Array.from({ length: 4 }, (_, seatIndex) => ({
      seat_index: seatIndex,
      display_name: `Player ${seatIndex}`,
      is_bot: false,
      connected: true,
      has_submitted_deck: false,
      pick_status: "Pending",
      active_pack_count: 1,
      face_up_draft_cards: [],
    })),
    cards_per_pack: 14,
    pick_steps_per_pack: 14,
    pack_count: 3,
    min_deck_size: 40,
    addable_cards: ["Island"],
    timer_remaining_ms: 30_000,
    standings: [],
    current_round: 0,
    next_pairing_round: 1,
    tournament_format: "Swiss",
    pod_policy: "Competitive",
    pairings: [],
    match_config: { match_type: "Bo1" },
  };
}

function welcome(initialView: DraftPlayerView): DraftP2PMessage {
  return {
    type: "draft_welcome",
    draftProtocolVersion: DRAFT_PROTOCOL_VERSION,
    draftToken: "guest-token",
    seatIndex: 2,
    draftCode: "draft-xyz",
    workspaceState: null,
    view: initialView,
  };
}

function firstContact(kind: "new" | "reconnect", initialView: DraftPlayerView): DraftP2PMessage {
  return kind === "new"
    ? welcome(initialView)
    : {
      type: "draft_reconnect_ack",
      draftProtocolVersion: DRAFT_PROTOCOL_VERSION,
      seatIndex: 2,
      draftCode: "draft-xyz",
      workspaceState: null,
      view: initialView,
    };
}

const flushAsync = () => new Promise<void>((resolve) => setTimeout(resolve, 0));

const workspace: DraftWorkspaceState = {
  schemaVersion: 1,
  placements: { "virtual-island": { zone: "deck", row: 0, column: 1, order: 0 } },
  virtualBasics: [{ instanceId: "virtual-island", name: "Island" }],
};

function pauseNextEncoding() {
  const started = deferred();
  const resume = deferred();
  const finished = deferred();
  const encodeDraftWireMessage = protocol.encodeDraftWireMessage;
  const encode = vi.spyOn(protocol, "encodeDraftWireMessage").mockImplementationOnce(async (message) => {
    started.resolve();
    await resume.promise;
    try {
      return await encodeDraftWireMessage(message);
    } finally {
      finished.resolve();
    }
  });
  return { started: started.promise, resume: resume.resolve, finished: finished.promise, encode };
}

describe("P2P draft guest receive ordering", () => {
  const guests: P2PDraftGuest[] = [];

  function createGuest(connection: DraftGuestConnection, guestPeer: Peer = {} as never) {
    const conn = new FakeDraftDataConnection();
    // The fake implements only the DataConnection subset used by the session.
    const guest = new P2PDraftGuest(guestPeer, "phase2-ABCDE", conn as never, connection);
    guests.push(guest);
    const events: DraftGuestEvent[] = [];
    guest.onEvent((event) => events.push(event));
    return { guest, conn, events };
  }

  beforeEach(() => {
    vi.clearAllMocks();
  });

  afterEach(() => {
    for (const guest of guests.splice(0)) guest.dispose();
    vi.useRealTimers();
    vi.restoreAllMocks();
  });

  it.each(["new", "reconnect"] as const)(
    "publishes the %s handshake view before a later update when recovery persistence is delayed",
    async (kind) => {
      const recoverySaved = deferred();
      persistence.saveDraftGuestSession.mockReturnValueOnce(recoverySaved.promise);
      const connection: DraftGuestConnection = kind === "new"
        ? { kind, roomCode: "ABCDE", displayName: "Alice" }
        : { kind, roomCode: "ABCDE", displayName: "Alice", draftToken: "guest-token" };
      const { guest, conn, events } = createGuest(connection);
      const initialized = guest.initialize();
      const initialView = view(1);
      const nextView = view(2);
      const firstReceived = conn.receiveRaw(rawMessage(firstContact(kind, initialView)));
      await vi.waitFor(() => expect(persistence.saveDraftGuestSession).toHaveBeenCalledOnce());
      // Reach the real adapter's asynchronous persistence branch before the
      // second frame arrives. No session or codec is mocked in this suite.
      expect(guest.view).toEqual(initialView);
      const secondReceived = conn.receiveRaw(rawMessage({ type: "draft_state_update", view: nextView }));
      await flushAsync();
      recoverySaved.resolve();
      await Promise.all([firstReceived, secondReceived, initialized]);

      expect(events.filter((event) => event.type === "viewUpdated").map((event) => event.view))
        .toEqual([initialView, nextView]);
      expect(guest.view).toEqual(nextView);
      expect(events.some((event) => event.type === (kind === "new" ? "joined" : "reconnected")))
        .toBe(true);
    },
  );

  it.each(["new", "reconnect"] as const)(
    "does not publish the %s acknowledgement if the connection errors during recovery persistence",
    async (kind) => {
      const recoverySaved = deferred();
      persistence.saveDraftGuestSession.mockReturnValueOnce(recoverySaved.promise);
      const connection: DraftGuestConnection = kind === "new"
        ? { kind, roomCode: "ABCDE", displayName: "Alice" }
        : { kind, roomCode: "ABCDE", displayName: "Alice", draftToken: "guest-token" };
      const { guest, conn, events } = createGuest(connection);
      const rejected = expect(guest.initialize(undefined, 1))
        .rejects.toThrow("Draft host disconnected before acknowledging");
      const received = conn.receiveRaw(rawMessage(firstContact(kind, view(1))));
      await vi.waitFor(() => expect(persistence.saveDraftGuestSession).toHaveBeenCalledOnce());

      conn.simulateError(new Error("Transport failed"));
      await rejected;
      recoverySaved.resolve();
      await received;

      expect(events).toEqual([]);
    },
  );

  it("publishes a deck acknowledgement before a later view while clearing the durable outbox", async () => {
    const { guest, conn, events } = createGuest({ kind: "new", roomCode: "ABCDE", displayName: "Alice" });
    const initialized = guest.initialize();
    await conn.receiveRaw(rawMessage(welcome(view(1))));
    await initialized;
    events.length = 0;

    const submitted = guest.submitDeck(["Island"], []);
    await vi.waitFor(() => expect(conn.sentRaw).toHaveLength(2));
    const command = await decodeDraftWireMessage(conn.sentRaw[1]!);
    expect(command.type).toBe("draft_submit_deck");
    if (command.type !== "draft_submit_deck") throw new Error("Expected draft deck submission");

    const outboxCleared = deferred();
    persistence.clearDraftDeckSubmission.mockReturnValueOnce(outboxCleared.promise);
    const acknowledgedView = { ...view(2), status: "Deckbuilding" as const };
    const nextView = { ...view(3), status: "Pairing" as const };
    const ackReceived = conn.receiveRaw(rawMessage({
      type: "draft_deck_submit_ack",
      submissionId: command.submissionId,
      view: acknowledgedView,
    }));
    await vi.waitFor(() => expect(persistence.clearDraftDeckSubmission).toHaveBeenCalledWith(
      "phase2-ABCDE", command.submissionId,
    ));
    const nextReceived = conn.receiveRaw(rawMessage({ type: "draft_state_update", view: nextView }));
    await flushAsync();
    outboxCleared.resolve();
    await Promise.all([ackReceived, nextReceived, submitted]);

    expect(events.filter((event) => event.type === "viewUpdated").map((event) => event.view))
      .toEqual([acknowledgedView, nextView]);
    expect(events).toContainEqual({
      type: "deckSubmissionAcknowledged", submissionId: command.submissionId, view: acknowledgedView,
    });
    expect(guest.view).toEqual(nextView);
  });

  it("settles a durable deck receipt without publishing its view after a connection error", async () => {
    const { guest, conn, events } = createGuest({ kind: "new", roomCode: "ABCDE", displayName: "Alice" });
    const initialized = guest.initialize();
    await conn.receiveRaw(rawMessage(welcome(view(1))));
    await initialized;
    events.length = 0;

    const submitted = guest.submitDeck(["Island"], []);
    await vi.waitFor(() => expect(conn.sentRaw).toHaveLength(2));
    const command = await decodeDraftWireMessage(conn.sentRaw[1]!);
    expect(command.type).toBe("draft_submit_deck");
    if (command.type !== "draft_submit_deck") throw new Error("Expected draft deck submission");

    const outboxCleared = deferred();
    persistence.clearDraftDeckSubmission.mockReturnValueOnce(outboxCleared.promise);
    const received = conn.receiveRaw(rawMessage({
      type: "draft_deck_submit_ack", submissionId: command.submissionId, view: view(2),
    }));
    await vi.waitFor(() => expect(persistence.clearDraftDeckSubmission).toHaveBeenCalledWith(
      "phase2-ABCDE", command.submissionId,
    ));

    conn.simulateError(new Error("Transport failed"));
    outboxCleared.resolve();
    await Promise.all([received, submitted]);

    expect(events).toEqual([{ type: "reconnecting", attempt: 1 }]);
  });

  it.each([
    {
      name: "submitPick",
      send: (guest: P2PDraftGuest) => guest.submitPick(["pack-1-card"]),
      message: { type: "draft_pick", cardInstanceIds: ["pack-1-card"] },
    },
    {
      name: "submitPickWithDraftEffect",
      send: (guest: P2PDraftGuest) => guest.submitPickWithDraftEffect("effect-card", ["pack-1-card", "pack-1-other"]),
      message: {
        type: "draft_pick_with_draft_effect",
        effectCardInstanceId: "effect-card",
        cardInstanceIds: ["pack-1-card", "pack-1-other"],
      },
    },
    {
      name: "updateWorkspace",
      send: (guest: P2PDraftGuest) => guest.updateWorkspace(workspace),
      message: { type: "draft_workspace_update", workspaceState: workspace },
    },
  ])("rejects $name during remote-close draining without discarding accepted receipts or views", async ({ send, message }) => {
    const { guest, conn, events } = createGuest({ kind: "new", roomCode: "ABCDE", displayName: "Alice" });
    const initialized = guest.initialize();
    await conn.receiveRaw(rawMessage(welcome(view(1))));
    await initialized;
    events.length = 0;

    // Prove the same public action and payload reach the real wire while open.
    // In particular, workspace validation must not cause the later rejection.
    await send(guest);
    expect(conn.sentRaw).toHaveLength(2);
    await expect(decodeDraftWireMessage(conn.sentRaw[1]!)).resolves.toEqual(message);

    const submitted = guest.submitDeck(["Island"], []);
    await vi.waitFor(() => expect(conn.sentRaw).toHaveLength(3));
    const submission = await decodeDraftWireMessage(conn.sentRaw[2]!);
    expect(submission.type).toBe("draft_submit_deck");
    if (submission.type !== "draft_submit_deck") throw new Error("Expected draft deck submission");

    const outboxCleared = deferred();
    persistence.clearDraftDeckSubmission.mockReturnValueOnce(outboxCleared.promise);
    const acknowledgedView = { ...view(2), status: "Deckbuilding" as const };
    const nextView = { ...view(3), status: "Pairing" as const };
    const ackReceived = conn.receiveRaw(rawMessage({
      type: "draft_deck_submit_ack", submissionId: submission.submissionId, view: acknowledgedView,
    }));
    await vi.waitFor(() => expect(persistence.clearDraftDeckSubmission).toHaveBeenCalledWith(
      "phase2-ABCDE", submission.submissionId,
    ));
    const nextReceived = conn.receiveRaw(rawMessage({ type: "draft_state_update", view: nextView }));
    conn.simulateClose();

    const sendResult = await send(guest).then(
      () => ({ status: "fulfilled" as const }),
      (error: unknown) => ({ status: "rejected" as const, error }),
    );
    expect(conn.sentRaw).toHaveLength(3);
    expect(events).toEqual([]);

    // Releasing persistence, not disposing the guest, must finish accepted work.
    outboxCleared.resolve();
    await Promise.all([ackReceived, nextReceived, submitted]);
    expect(events).toEqual([
      { type: "deckSubmissionAcknowledged", submissionId: submission.submissionId, view: acknowledgedView },
      { type: "viewUpdated", view: acknowledgedView },
      { type: "viewUpdated", view: nextView },
      { type: "reconnecting", attempt: 1 },
    ]);
    expect(guest.view).toEqual(nextView);
    expect(conn.sentRaw).toHaveLength(3);
    expect(guest.isRecoveryRevoked).toBe(false);
    expect(persistence.clearDraftGuestRecovery).not.toHaveBeenCalled();
    expect(sendResult).toEqual({
      status: "rejected", error: new Error("Draft connection is not open"),
    });
  });

  it("honors a leave acknowledgement queued behind persistence before the host closes", async () => {
    const { guest, conn, events } = createGuest({ kind: "new", roomCode: "ABCDE", displayName: "Alice" });
    const initialized = guest.initialize();
    await conn.receiveRaw(rawMessage(welcome(view(1))));
    await initialized;

    const submitted = guest.submitDeck(["Island"], []);
    await vi.waitFor(() => expect(conn.sentRaw).toHaveLength(2));
    const submission = await decodeDraftWireMessage(conn.sentRaw[1]!);
    expect(submission.type).toBe("draft_submit_deck");
    if (submission.type !== "draft_submit_deck") throw new Error("Expected draft deck submission");

    const outboxCleared = deferred();
    persistence.clearDraftDeckSubmission.mockReturnValueOnce(outboxCleared.promise);
    const deckAckReceived = conn.receiveRaw(rawMessage({
      type: "draft_deck_submit_ack", submissionId: submission.submissionId, view: view(2),
    }));
    await vi.waitFor(() => expect(persistence.clearDraftDeckSubmission).toHaveBeenCalledWith(
      "phase2-ABCDE", submission.submissionId,
    ));

    const leaveResult = guest.leave().then(
      () => ({ status: "fulfilled" as const }),
      (error: unknown) => ({ status: "rejected" as const, error }),
    );
    await vi.waitFor(() => expect(conn.sentRaw).toHaveLength(3));
    await expect(decodeDraftWireMessage(conn.sentRaw[2]!)).resolves.toEqual({
      type: "draft_leave", draftProtocolVersion: DRAFT_PROTOCOL_VERSION, draftToken: "guest-token",
    });
    const leaveAckReceived = conn.receiveRaw(rawMessage({
      type: "draft_leave_ack", draftProtocolVersion: DRAFT_PROTOCOL_VERSION, draftToken: "guest-token",
    }));
    conn.simulateClose();
    outboxCleared.resolve();
    await Promise.all([deckAckReceived, leaveAckReceived, submitted]);

    await expect(leaveResult).resolves.toEqual({ status: "fulfilled" });
    expect(guest.isRecoveryRevoked).toBe(true);
    expect(persistence.clearDraftGuestRecovery).toHaveBeenCalledWith("phase2-ABCDE");
    expect(events.some((event) => event.type === "reconnecting")).toBe(false);
  });

  it.each(["remote close", "error"] as const)(
    "observes a leave rejection when %s interrupts outgoing encoding",
    async (end) => {
      const { guest, conn } = createGuest({ kind: "new", roomCode: "ABCDE", displayName: "Alice" });
      const initialized = guest.initialize();
      await conn.receiveRaw(rawMessage(welcome(view(1))));
      await initialized;

      const encoding = pauseNextEncoding();
      const wireSend = vi.spyOn(conn, "send");
      const leaveResult = guest.leave().then(
        () => ({ status: "fulfilled" as const }),
        (error: unknown) => ({ status: "rejected" as const, error }),
      );
      await encoding.started;
      expect(encoding.encode).toHaveBeenCalledExactlyOnceWith({
        type: "draft_leave", draftProtocolVersion: DRAFT_PROTOCOL_VERSION, draftToken: "guest-token",
      });
      if (end === "remote close") conn.simulateClose();
      else conn.simulateError(new Error("Transport failed"));
      // Give a rejected acknowledgement a full turn to expose an unobserved
      // promise while encoding remains blocked; only public results are caught.
      await flushAsync();
      encoding.resume();
      await encoding.finished;
      await flushAsync();

      await expect(leaveResult).resolves.toEqual({
        status: "rejected", error: new Error("Draft host disconnected before acknowledging leave"),
      });
      expect(wireSend).not.toHaveBeenCalled();
      expect(conn.sentRaw).toHaveLength(1);
      expect(guest.isRecoveryRevoked).toBe(false);
      expect(persistence.clearDraftGuestRecovery).not.toHaveBeenCalled();
    },
  );

  it("observes a deck receipt rejection when disposal interrupts outgoing encoding", async () => {
    const { guest, conn } = createGuest({ kind: "new", roomCode: "ABCDE", displayName: "Alice" });
    const initialized = guest.initialize();
    await conn.receiveRaw(rawMessage(welcome(view(1))));
    await initialized;

    const encoding = pauseNextEncoding();
    const wireSend = vi.spyOn(conn, "send");
    const submissionResult = guest.submitDeck(["Island"], []).then(
      () => ({ status: "fulfilled" as const }),
      (error: unknown) => ({ status: "rejected" as const, error }),
    );
    await encoding.started;
    expect(persistence.saveDraftDeckSubmission).toHaveBeenCalledOnce();
    expect(encoding.encode).toHaveBeenCalledExactlyOnceWith({
      type: "draft_submit_deck", submissionId: expect.any(String), mainDeck: ["Island"], commanders: [],
    });
    guest.dispose();
    await flushAsync();
    encoding.resume();
    await encoding.finished;
    await flushAsync();

    await expect(submissionResult).resolves.toEqual({
      status: "rejected", error: new Error("Draft connection disposed"),
    });
    expect(wireSend).not.toHaveBeenCalled();
    expect(conn.sentRaw).toHaveLength(1);
    expect(guest.isRecoveryRevoked).toBe(false);
    expect(persistence.clearDraftGuestRecovery).not.toHaveBeenCalled();
    expect(persistence.clearDraftDeckSubmission).not.toHaveBeenCalled();
  });

  it("keeps the original deck acknowledgement routed when an older reconnect replay fails", async () => {
    const middleConn = new FakeDraftDataConnection();
    const newestConn = new FakeDraftDataConnection();
    middleConn.open = false;
    newestConn.open = false;
    const connect = vi.fn().mockReturnValueOnce(middleConn).mockReturnValueOnce(newestConn);
    const { guest, conn, events } = createGuest(
      { kind: "new", roomCode: "ABCDE", displayName: "Alice" },
      { connect } as never,
    );
    const initialized = guest.initialize();
    await conn.receiveRaw(rawMessage(welcome(view(1))));
    await initialized;

    const submitted = guest.submitDeck(["Island"], []);
    const settled = vi.fn();
    void submitted.then(
      () => settled("fulfilled"),
      (error: unknown) => settled(error),
    );
    await vi.waitFor(() => expect(conn.sentRaw).toHaveLength(2));
    const submission = await decodeDraftWireMessage(conn.sentRaw[1]!);
    expect(submission.type).toBe("draft_submit_deck");
    if (submission.type !== "draft_submit_deck") throw new Error("Expected draft deck submission");
    expect(persistence.saveDraftDeckSubmission).toHaveBeenCalledExactlyOnceWith("phase2-ABCDE", {
      roomCode: "ABCDE", draftCode: "draft-xyz", draftToken: "guest-token",
      submissionId: submission.submissionId, mainDeck: ["Island"], commanders: [],
    });
    const savedSubmission: PersistedDraftDeckSubmission = {
      hostPeerId: "phase2-ABCDE", roomCode: "ABCDE", draftCode: "draft-xyz", draftToken: "guest-token",
      submissionId: submission.submissionId, mainDeck: ["Island"], commanders: [], timestamp: Date.now(),
    };
    persistence.loadDraftDeckSubmission
      .mockResolvedValueOnce(savedSubmission)
      .mockResolvedValueOnce(savedSubmission);

    // Drive the public reconnect timer and Peer open event into a second real
    // session. Stall only its replay, after its handshake reaches the wire.
    vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout"] });
    conn.simulateClose();
    await vi.advanceTimersByTimeAsync(1000);
    expect(connect).toHaveBeenCalledExactlyOnceWith("phase2-ABCDE");
    middleConn.simulateOpen();
    await vi.advanceTimersByTimeAsync(0);
    expect(middleConn.sentRaw).toHaveLength(1);
    await expect(decodeDraftWireMessage(middleConn.sentRaw[0]!)).resolves.toEqual({
      type: "draft_reconnect", draftProtocolVersion: DRAFT_PROTOCOL_VERSION, draftToken: "guest-token",
    });
    const encoding = pauseNextEncoding();
    await middleConn.receiveRaw(rawMessage(firstContact("reconnect", view(2))));
    await encoding.started;
    expect(encoding.encode).toHaveBeenCalledExactlyOnceWith(submission);
    expect(middleConn.sentRaw).toHaveLength(1);

    // A third session replays the same durable command before the second
    // session's delayed encoder can fail and run its waiter cleanup.
    middleConn.simulateClose();
    await vi.advanceTimersByTimeAsync(1000);
    expect(connect).toHaveBeenCalledTimes(2);
    expect(connect).toHaveBeenNthCalledWith(2, "phase2-ABCDE");
    newestConn.simulateOpen();
    await vi.advanceTimersByTimeAsync(0);
    expect(newestConn.sentRaw).toHaveLength(1);
    await expect(decodeDraftWireMessage(newestConn.sentRaw[0]!)).resolves.toEqual({
      type: "draft_reconnect", draftProtocolVersion: DRAFT_PROTOCOL_VERSION, draftToken: "guest-token",
    });
    await newestConn.receiveRaw(rawMessage(firstContact("reconnect", view(3))));
    await vi.advanceTimersByTimeAsync(0);
    expect(newestConn.sentRaw).toHaveLength(2);
    await expect(decodeDraftWireMessage(newestConn.sentRaw[1]!)).resolves.toEqual(submission);
    expect(guest.submitDeck(["Island"], [])).toBe(submitted);
    expect(settled).not.toHaveBeenCalled();

    encoding.resume();
    await encoding.finished;
    await vi.advanceTimersByTimeAsync(0);
    expect(events).toContainEqual({ type: "error", message: "Draft connection is not open" });
    expect(middleConn.sentRaw).toHaveLength(1);
    expect(settled).not.toHaveBeenCalled();

    const acknowledgedView = { ...view(4), status: "Pairing" as const };
    await newestConn.receiveRaw(rawMessage({
      type: "draft_deck_submit_ack", submissionId: submission.submissionId, view: acknowledgedView,
    }));
    await vi.advanceTimersByTimeAsync(0);
    expect(persistence.clearDraftDeckSubmission).toHaveBeenCalledExactlyOnceWith(
      "phase2-ABCDE", submission.submissionId,
    );
    expect(events).toContainEqual({
      type: "deckSubmissionAcknowledged", submissionId: submission.submissionId, view: acknowledgedView,
    });
    expect(settled).toHaveBeenCalledExactlyOnceWith("fulfilled");
    await expect(submitted).resolves.toBeUndefined();
    expect(persistence.saveDraftDeckSubmission).toHaveBeenCalledOnce();
    expect(newestConn.sentRaw).toHaveLength(2);
    expect(guest.view).toEqual(acknowledgedView);
  });

  it("still reports an active handshake persistence failure", async () => {
    persistence.saveDraftGuestSession.mockRejectedValueOnce(new Error("IDB unavailable"));
    const { guest, conn, events } = createGuest({ kind: "new", roomCode: "ABCDE", displayName: "Alice" });
    const rejected = expect(guest.initialize()).rejects.toThrow("IDB unavailable");

    await conn.receiveRaw(rawMessage(welcome(view(1))));
    await rejected;

    expect(events).toEqual([{ type: "error", message: "Could not save draft recovery details" }]);
    expect(persistence.saveActiveDraftGuest).not.toHaveBeenCalled();
  });
});
