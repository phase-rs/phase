import { readFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";
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
import { Draft27IncompatibilityError, type Draft27CompatibilityError } from "../draft-compatibility";
import { PEER_CONNECT_OPTIONS } from "../../network/connection";
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
function rawMessage(message: DraftP2PMessage | Record<string, unknown>): Uint8Array {
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
    distribution: "PickAndPass",
    commanders_required: 0,
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
      drafted_card_count: 0,
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

/** Narrows a captured pre-encode message to the deck-submission variant. */
function asDeckSubmission(message: DraftP2PMessage): Extract<DraftP2PMessage, { type: "draft_submit_deck" }> {
  if (message.type !== "draft_submit_deck") throw new Error("Expected a draft_submit_deck message");
  return message;
}

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

type IncompatibleViewShape = "fifoFlow" | "reviewStatus" | "residualFifo";
type ViewCarrier = "welcome" | "reconnect" | "state" | "pick" | "deck";

function incompatibleView(
  pickNumber: number,
  shape: IncompatibleViewShape,
): DraftPlayerView {
  const legacy = view(pickNumber);
  switch (shape) {
    case "fifoFlow":
      return { ...legacy, flow: "FifoAsync" } as unknown as DraftPlayerView;
    case "reviewStatus":
      return { ...legacy, status: "Review" } as unknown as DraftPlayerView;
    case "residualFifo":
      return { ...legacy, fifo: { session: "fifo-session" } } as unknown as DraftPlayerView;
  }
}

function legacyCarrierMessage(
  carrier: ViewCarrier,
  carrierView: DraftPlayerView,
  submissionId = "submission-1",
): DraftP2PMessage {
  switch (carrier) {
    case "welcome":
      return welcome(carrierView);
    case "reconnect":
      return firstContact("reconnect", carrierView);
    case "state":
      return { type: "draft_state_update", view: carrierView };
    case "pick":
      return { type: "draft_pick_ack", view: carrierView };
    case "deck":
      return { type: "draft_deck_submit_ack", submissionId, view: carrierView };
  }
}

/** A card in the face-up prefix of a shared-stack pile. */
const SHARED_STACK_PILE_CARD: DraftPlayerView["pool"][number] = {
  instance_id: "pile-0-card",
  name: "Ponder",
  set_code: "TST",
  collector_number: "2",
  rarity: "common",
  colors: ["U"],
  cmc: 1,
  type_line: "Sorcery",
};

/**
 * A shared-stack (Winston) player view for a two-seat pod, in the
 * shape the engine publishes it: the pile distribution and the play-first
 * chooser on every status, and the live pile turn only while drafting.
 */
function sharedStackView(
  pickNumber: number,
  status: "Drafting" | "Deckbuilding" = "Drafting",
): DraftPlayerView {
  const base = view(pickNumber);
  const pileTurn: NonNullable<DraftPlayerView["shared_stack"]> = {
    main_stack_remaining: 30,
    total_cards: 45,
    active_seat: 0,
    active_pile: 0,
    piles: [
      {
        index: 0,
        total: 2,
        revealed: [SHARED_STACK_PILE_CARD],
        legality: [
          { decision: "Take", refusal: null },
          { decision: "Decline", refusal: null },
        ],
      },
      { index: 1, total: 1, revealed: [], legality: [{ decision: "Take", refusal: null }] },
      { index: 2, total: 1, revealed: [], legality: [{ decision: "Take", refusal: null }] },
    ],
    decisions: pickNumber,
    history: [{ seat: 1, pile: 0, decision: "Decline", pile_size: 1 }],
    forced_draw: null,
  };
  return {
    ...base,
    status,
    kind: "Winston",
    distribution: { SharedStackPiles: { pile_count: 3 } },
    current_pack: null,
    required_pick_count: 0,
    seats: base.seats.slice(0, 2).map((seat): DraftPlayerView["seats"][number] => ({
      ...seat,
      pick_status: status === "Drafting" ? (seat.seat_index === 0 ? "Pending" : "Waiting") : "NotDrafting",
      active_pack_count: 0,
      drafted_card_count: pickNumber,
    })),
    ...(status === "Drafting" ? { shared_stack: pileTurn } : {}),
    play_first_chooser: 1,
  };
}

/** The five view carriers, addressed to the second seat of the two-seat pod. */
function sharedStackCarrierMessage(
  carrier: ViewCarrier,
  carrierView: DraftPlayerView,
  submissionId = "submission-1",
): DraftP2PMessage {
  const message = legacyCarrierMessage(carrier, carrierView, submissionId);
  return message.type === "draft_welcome" || message.type === "draft_reconnect_ack"
    ? { ...message, seatIndex: 1 }
    : message;
}

function withIncompatibleShape(base: DraftPlayerView, shape: IncompatibleViewShape): DraftPlayerView {
  switch (shape) {
    case "fifoFlow":
      return { ...base, flow: "FifoAsync" } as unknown as DraftPlayerView;
    case "reviewStatus":
      return { ...base, status: "Review" } as unknown as DraftPlayerView;
    case "residualFifo":
      return { ...base, fifo: { session: "fifo-session" } } as unknown as DraftPlayerView;
  }
}

/** The refusal message the guest reports for each incompatible view shape. */
const INCOMPATIBLE_SHAPE_MESSAGES: Record<IncompatibleViewShape, string> = {
  fifoFlow: "This draft uses FIFO delivery, which this connection cannot accept.",
  reviewStatus: "This draft is in Review, which this connection cannot accept.",
  residualFifo: "This draft includes FIFO state, which this connection cannot accept.",
};

/** The classifier code the guest reports for each incompatible view shape. */
const INCOMPATIBLE_SHAPE_CODES: Record<IncompatibleViewShape, Draft27CompatibilityError["code"]> = {
  fifoFlow: "fifo_flow",
  reviewStatus: "review_status",
  residualFifo: "residual_fifo",
};

/**
 * A player view draft-core publishes for a FIFO pod, from the fixture its own
 * tests check against its serialization.
 */
function fifoPlayerView(name: "queued_view" | "review_view"): DraftPlayerView {
  const fixture = JSON.parse(readFileSync(resolve(
    dirname(fileURLToPath(import.meta.url)),
    "../../../../fixtures/draft-contract/fifo_legacy_admission.json",
  ), "utf8")) as Record<typeof name, DraftPlayerView>;
  return fixture[name];
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

  it("does not report a recovered acceptance for a caller-awaited submission", async () => {
    const { guest, conn, events } = createGuest({ kind: "new", roomCode: "ABCDE", displayName: "Alice" });
    const initialized = guest.initialize();
    await conn.receiveRaw(rawMessage(welcome(view(1))));
    await initialized;
    events.length = 0;

    const submitted = guest.submitDeck(["Island"], []);
    await vi.waitFor(() => expect(conn.sentRaw).toHaveLength(2));
    const command = await decodeDraftWireMessage(conn.sentRaw[1]!);
    if (command.type !== "draft_submit_deck") throw new Error("Expected draft deck submission");

    const acknowledgedView = { ...view(2), status: "Deckbuilding" as const };
    await conn.receiveRaw(rawMessage({
      type: "draft_deck_submit_ack", submissionId: command.submissionId, view: acknowledgedView,
    }));
    await submitted;

    expect(events).toContainEqual({
      type: "deckSubmissionAcknowledged", submissionId: command.submissionId, view: acknowledgedView,
    });
    expect(events.some((event) => event.type === "recoveredDeckSubmissionAccepted")).toBe(false);
  });

  it("reports a replayed submission's payload once the host accepts it after reconnect", async () => {
    const { guest, conn, events } = createGuest(
      { kind: "reconnect", roomCode: "ABCDE", displayName: "Alice", draftToken: "guest-token" },
    );
    const stored: PersistedDraftDeckSubmission = {
      hostPeerId: "phase2-ABCDE", roomCode: "ABCDE", draftCode: "draft-xyz", draftToken: "guest-token",
      submissionId: "sub-1", mainDeck: ["Island"], commanders: [], timestamp: Date.now(),
    };
    persistence.loadDraftDeckSubmission.mockResolvedValueOnce(stored);

    const initialized = guest.initialize();
    await conn.receiveRaw(rawMessage(firstContact("reconnect", view(1))));
    await initialized;
    await vi.waitFor(() => expect(conn.sentRaw).toHaveLength(2));
    const replay = await decodeDraftWireMessage(conn.sentRaw[1]!);
    expect(replay).toEqual({
      type: "draft_submit_deck", submissionId: "sub-1", mainDeck: ["Island"], commanders: [],
    });

    const acknowledgedView = { ...view(2), status: "Pairing" as const };
    await conn.receiveRaw(rawMessage({
      type: "draft_deck_submit_ack", submissionId: "sub-1", view: acknowledgedView,
    }));

    expect(events).toContainEqual({
      type: "recoveredDeckSubmissionAccepted", mainDeck: ["Island"], commanders: [], view: acknowledgedView,
    });
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

  it("settles only a matching land-suggestion response from its owning session", async () => {
    const { guest, conn } = createGuest({ kind: "new", roomCode: "ABCDE", displayName: "Alice" });
    const initialized = guest.initialize();
    await conn.receiveRaw(rawMessage(welcome(view(1))));
    await initialized;

    const suggested = guest.suggestLands();
    await vi.waitFor(() => expect(conn.sentRaw).toHaveLength(2));
    const request = await decodeDraftWireMessage(conn.sentRaw[1]!);
    expect(request.type).toBe("draft_suggest_lands");
    if (request.type !== "draft_suggest_lands") throw new Error("Expected land suggestion request");

    await conn.receiveRaw(rawMessage({
      type: "draft_suggest_lands_result", requestId: "unknown", lands: { Island: 17 },
    }));
    await conn.receiveRaw(rawMessage({
      type: "draft_suggest_lands_result", requestId: request.requestId, lands: { Mountain: 17 },
    }));

    await expect(suggested).resolves.toEqual({ Mountain: 17 });
  });

  it("rejects a pending land suggestion when disposed", async () => {
    const { guest, conn } = createGuest({ kind: "new", roomCode: "ABCDE", displayName: "Alice" });
    const initialized = guest.initialize();
    await conn.receiveRaw(rawMessage(welcome(view(1))));
    await initialized;

    const suggested = guest.suggestLands();
    await vi.waitFor(() => expect(conn.sentRaw).toHaveLength(2));
    guest.dispose();

    await expect(suggested).rejects.toThrow("Draft connection disposed");
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
    expect(connect).toHaveBeenCalledExactlyOnceWith("phase2-ABCDE", PEER_CONNECT_OPTIONS);
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
    expect(connect).toHaveBeenNthCalledWith(2, "phase2-ABCDE", PEER_CONNECT_OPTIONS);
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

  it("reports one recovered acceptance for a duplicate ack while a stalled replay attempt is still in flight", async () => {
    const middleConn = new FakeDraftDataConnection();
    middleConn.open = false;
    const connect = vi.fn().mockReturnValueOnce(middleConn);
    const stored: PersistedDraftDeckSubmission = {
      hostPeerId: "phase2-ABCDE", roomCode: "ABCDE", draftCode: "draft-xyz", draftToken: "guest-token",
      submissionId: "sub-1", mainDeck: ["Island"], commanders: [], timestamp: Date.now(),
    };
    const { guest, conn, events } = createGuest(
      { kind: "reconnect", roomCode: "ABCDE", displayName: "Alice", draftToken: "guest-token" },
      { connect } as never,
    );
    const initialized = guest.initialize();
    await vi.waitFor(() => expect(conn.sentRaw).toHaveLength(1));
    persistence.loadDraftDeckSubmission.mockResolvedValueOnce(stored);
    // Arm the pause only after the handshake frame itself is on the wire, so
    // it catches the replay the reconnect ack below triggers, not the frame.
    const encoding = pauseNextEncoding();
    await conn.receiveRaw(rawMessage(firstContact("reconnect", view(1))));
    await initialized;
    await encoding.started;
    expect(encoding.encode).toHaveBeenCalledExactlyOnceWith({
      type: "draft_submit_deck", submissionId: "sub-1", mainDeck: ["Island"], commanders: [],
    });

    // Reconnect a second time while the first session's replay is stalled.
    persistence.loadDraftDeckSubmission.mockResolvedValueOnce(stored);
    vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout"] });
    conn.simulateClose();
    await vi.advanceTimersByTimeAsync(1000);
    expect(connect).toHaveBeenCalledExactlyOnceWith("phase2-ABCDE", PEER_CONNECT_OPTIONS);
    middleConn.simulateOpen();
    await vi.advanceTimersByTimeAsync(0);
    await middleConn.receiveRaw(rawMessage(firstContact("reconnect", view(2))));
    await vi.advanceTimersByTimeAsync(0);
    // sentRaw[0] is the reconnect frame; [1] is this session's own (unstalled) replay.
    expect(middleConn.sentRaw).toHaveLength(2);
    await expect(decodeDraftWireMessage(middleConn.sentRaw[1]!)).resolves.toEqual({
      type: "draft_submit_deck", submissionId: "sub-1", mainDeck: ["Island"], commanders: [],
    });

    const acknowledgedView = { ...view(3), status: "Pairing" as const };
    const ackMessage = rawMessage({
      type: "draft_deck_submit_ack", submissionId: "sub-1", view: acknowledgedView,
    });
    await middleConn.receiveRaw(ackMessage);
    await vi.advanceTimersByTimeAsync(0);
    // The host re-acks duplicate submissions: deliver a second, identical ack.
    await middleConn.receiveRaw(ackMessage);
    await vi.advanceTimersByTimeAsync(0);

    const recovered = events.filter((event) => event.type === "recoveredDeckSubmissionAccepted");
    expect(recovered).toEqual([
      { type: "recoveredDeckSubmissionAccepted", mainDeck: ["Island"], commanders: [], view: acknowledgedView },
    ]);

    // The stalled first session's send now fails against its closed connection,
    // but the waiter is already acknowledged, so it must not surface as an error.
    encoding.resume();
    await encoding.finished;
    await vi.advanceTimersByTimeAsync(0);
    expect(events.some((event) => (
      event.type === "error" && event.message === "Draft connection is not open"
    ))).toBe(false);

    vi.useRealTimers();
  });

  it("resolves the caller with no recovered event when its send fails after the host already acknowledged", async () => {
    const middleConn = new FakeDraftDataConnection();
    middleConn.open = false;
    const connect = vi.fn().mockReturnValueOnce(middleConn);
    const { guest, conn, events } = createGuest(
      { kind: "new", roomCode: "ABCDE", displayName: "Alice" },
      { connect } as never,
    );
    const initialized = guest.initialize();
    await conn.receiveRaw(rawMessage(welcome(view(1))));
    await initialized;

    const encoding = pauseNextEncoding();
    const submitted = guest.submitDeck(["Island"], []);
    const settled = vi.fn();
    void submitted.then(
      () => settled("fulfilled"),
      (error: unknown) => settled(error),
    );
    await encoding.started;
    const submission = asDeckSubmission(encoding.encode.mock.calls[0]![0]);
    persistence.loadDraftDeckSubmission.mockResolvedValueOnce({
      hostPeerId: "phase2-ABCDE", roomCode: "ABCDE", draftCode: "draft-xyz", draftToken: "guest-token",
      submissionId: submission.submissionId, mainDeck: submission.mainDeck, commanders: submission.commanders,
      timestamp: Date.now(),
    });

    vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout"] });
    conn.simulateClose();
    await vi.advanceTimersByTimeAsync(1000);
    middleConn.simulateOpen();
    await vi.advanceTimersByTimeAsync(0);
    await middleConn.receiveRaw(rawMessage(firstContact("reconnect", view(2))));
    await vi.advanceTimersByTimeAsync(0);
    expect(middleConn.sentRaw).toHaveLength(2);
    await expect(decodeDraftWireMessage(middleConn.sentRaw[1]!)).resolves.toEqual(submission);

    const acknowledgedView = { ...view(3), status: "Pairing" as const };
    await middleConn.receiveRaw(rawMessage({
      type: "draft_deck_submit_ack", submissionId: submission.submissionId, view: acknowledgedView,
    }));
    await vi.advanceTimersByTimeAsync(0);
    // Reach guard: the caller's own send is still stalled and unsettled, while
    // the ack already routed to it (deckSubmissionAcknowledged, not the
    // recovered-only event, since this waiter's callerAttempts is nonzero).
    expect(settled).not.toHaveBeenCalled();
    expect(events).toContainEqual({
      type: "deckSubmissionAcknowledged", submissionId: submission.submissionId, view: acknowledgedView,
    });

    encoding.resume();
    await encoding.finished;
    await vi.advanceTimersByTimeAsync(0);

    expect(settled).toHaveBeenCalledExactlyOnceWith("fulfilled");
    expect(events.some((event) => event.type === "recoveredDeckSubmissionAccepted")).toBe(false);

    vi.useRealTimers();
  });

  it("reports acceptance for a replayed submission acknowledged as its session fails", async () => {
    const { guest, conn, events } = createGuest(
      { kind: "reconnect", roomCode: "ABCDE", displayName: "Alice", draftToken: "guest-token" },
    );
    const stored: PersistedDraftDeckSubmission = {
      hostPeerId: "phase2-ABCDE", roomCode: "ABCDE", draftCode: "draft-xyz", draftToken: "guest-token",
      submissionId: "sub-1", mainDeck: ["Island"], commanders: [], timestamp: Date.now(),
    };
    persistence.loadDraftDeckSubmission.mockResolvedValueOnce(stored);
    const initialized = guest.initialize();
    await conn.receiveRaw(rawMessage(firstContact("reconnect", view(1))));
    await initialized;
    events.length = 0;
    await vi.waitFor(() => expect(conn.sentRaw).toHaveLength(2));
    const replay = await decodeDraftWireMessage(conn.sentRaw[1]!);
    if (replay.type !== "draft_submit_deck") throw new Error("Expected a replayed deck submission");

    const outboxCleared = deferred();
    persistence.clearDraftDeckSubmission.mockReturnValueOnce(outboxCleared.promise);
    const acknowledgedView = { ...view(2), status: "Pairing" as const };
    const received = conn.receiveRaw(rawMessage({
      type: "draft_deck_submit_ack", submissionId: replay.submissionId, view: acknowledgedView,
    }));
    await vi.waitFor(() => expect(persistence.clearDraftDeckSubmission).toHaveBeenCalledWith(
      "phase2-ABCDE", replay.submissionId,
    ));

    conn.simulateError(new Error("Transport failed"));
    outboxCleared.resolve();
    await received;

    expect(events).toContainEqual({
      type: "recoveredDeckSubmissionAccepted", mainDeck: ["Island"], commanders: [], view: acknowledgedView,
    });
    expect(events.some((event) => event.type === "viewUpdated")).toBe(false);
  });

  it("leaves acceptance to the replay when the caller's own send fails before any acknowledgement", async () => {
    const middleConn = new FakeDraftDataConnection();
    middleConn.open = false;
    const connect = vi.fn().mockReturnValueOnce(middleConn);
    const { guest, conn, events } = createGuest(
      { kind: "new", roomCode: "ABCDE", displayName: "Alice" },
      { connect } as never,
    );
    const initialized = guest.initialize();
    await conn.receiveRaw(rawMessage(welcome(view(1))));
    await initialized;

    const encoding = pauseNextEncoding();
    const submitted = guest.submitDeck(["Island"], []);
    const settled = vi.fn();
    void submitted.then(
      () => settled("fulfilled"),
      (error: unknown) => settled(error),
    );
    await encoding.started;
    expect(encoding.encode).toHaveBeenCalledExactlyOnceWith({
      type: "draft_submit_deck", submissionId: expect.any(String), mainDeck: ["Island"], commanders: [],
    });
    const submission = asDeckSubmission(encoding.encode.mock.calls[0]![0]);
    persistence.loadDraftDeckSubmission.mockResolvedValueOnce({
      hostPeerId: "phase2-ABCDE", roomCode: "ABCDE", draftCode: "draft-xyz", draftToken: "guest-token",
      submissionId: submission.submissionId, mainDeck: submission.mainDeck, commanders: submission.commanders,
      timestamp: Date.now(),
    });

    vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout"] });
    conn.simulateClose();
    await vi.advanceTimersByTimeAsync(1000);
    expect(connect).toHaveBeenCalledExactlyOnceWith("phase2-ABCDE", PEER_CONNECT_OPTIONS);
    middleConn.simulateOpen();
    await vi.advanceTimersByTimeAsync(0);
    await middleConn.receiveRaw(rawMessage(firstContact("reconnect", view(2))));
    await vi.advanceTimersByTimeAsync(0);
    // sentRaw[0] is the reconnect frame itself; [1] is the replay it triggers.
    expect(middleConn.sentRaw).toHaveLength(2);
    await expect(decodeDraftWireMessage(middleConn.sentRaw[1]!)).resolves.toEqual(submission);

    // Resume the caller's stalled send now, before any acknowledgement: the
    // connection it targets is already closed, so it rejects.
    encoding.resume();
    await encoding.finished;
    await vi.advanceTimersByTimeAsync(0);
    expect(settled).toHaveBeenCalledExactlyOnceWith(new Error("Draft connection is not open"));

    const acknowledgedView = { ...view(3), status: "Pairing" as const };
    await middleConn.receiveRaw(rawMessage({
      type: "draft_deck_submit_ack", submissionId: submission.submissionId, view: acknowledgedView,
    }));
    await vi.advanceTimersByTimeAsync(0);

    const recovered = events.filter((event) => event.type === "recoveredDeckSubmissionAccepted");
    expect(recovered).toEqual([
      { type: "recoveredDeckSubmissionAccepted", mainDeck: ["Island"], commanders: [], view: acknowledgedView },
    ]);

    vi.useRealTimers();
  });

  it.each(["welcome", "reconnect", "state", "pick", "deck"] as const)(
    "keeps legacy draft27 %s delivery reachable after admission",
    async (carrier) => {
      const connection: DraftGuestConnection = carrier === "reconnect"
        ? { kind: "reconnect", roomCode: "ABCDE", displayName: "Alice", draftToken: "guest-token" }
        : { kind: "new", roomCode: "ABCDE", displayName: "Alice" };
      const { guest, conn, events } = createGuest(connection);
      const acceptedView = view(2);

      if (carrier === "welcome" || carrier === "reconnect") {
        const initialized = guest.initialize();
        await conn.receiveRaw(rawMessage(legacyCarrierMessage(carrier, acceptedView)));
        await initialized;
      } else {
        const initialized = guest.initialize();
        await conn.receiveRaw(rawMessage(welcome(view(1))));
        await initialized;
        events.length = 0;

        if (carrier === "deck") {
          const submitted = guest.submitDeck(["Island"], []);
          await vi.waitFor(() => expect(conn.sentRaw).toHaveLength(2));
          const command = await decodeDraftWireMessage(conn.sentRaw[1]!);
          expect(command.type).toBe("draft_submit_deck");
          if (command.type !== "draft_submit_deck") throw new Error("Expected draft deck submission");
          await conn.receiveRaw(rawMessage(legacyCarrierMessage(carrier, acceptedView, command.submissionId)));
          await submitted;
        } else {
          await conn.receiveRaw(rawMessage(legacyCarrierMessage(carrier, acceptedView)));
        }
      }

      expect(guest.view).toEqual(acceptedView);
      switch (carrier) {
        case "welcome":
          expect(events).toContainEqual({ type: "joined", seatIndex: 2, draftCode: "draft-xyz" });
          break;
        case "reconnect":
          expect(events).toContainEqual({ type: "reconnected", seatIndex: 2 });
          break;
        case "state":
          expect(events).toContainEqual({ type: "viewUpdated", view: acceptedView });
          break;
        case "pick":
          expect(events).toContainEqual({ type: "pickAcknowledged", view: acceptedView });
          break;
        case "deck":
          expect(events).toContainEqual({
            type: "deckSubmissionAcknowledged",
            submissionId: expect.any(String),
            view: acceptedView,
          });
          break;
      }
    },
  );

  it.each(
    (["welcome", "reconnect", "state", "pick", "deck"] as const).flatMap((carrier) =>
      (["fifoFlow", "reviewStatus", "residualFifo"] as const).map((shape) => ({ carrier, shape })),
    ),
  )(
    "draft27 rejects raw $shape before $carrier carrier effects",
    async ({ carrier, shape }) => {
      const connection: DraftGuestConnection = carrier === "reconnect"
        ? { kind: "reconnect", roomCode: "ABCDE", displayName: "Alice", draftToken: "guest-token" }
        : { kind: "new", roomCode: "ABCDE", displayName: "Alice" };
      const { guest, conn, events } = createGuest(connection);
      const rejectedView = incompatibleView(2, shape);
      const admittedView = view(1);

      if (carrier === "welcome" || carrier === "reconnect") {
        const initialized = guest.initialize();
        await conn.receiveRaw(rawMessage(legacyCarrierMessage(carrier, rejectedView)));
        await expect(initialized).rejects.toBeInstanceOf(Draft27IncompatibilityError);
        expect(guest.view).toBeNull();
      } else {
        const initialized = guest.initialize();
        await conn.receiveRaw(rawMessage(welcome(admittedView)));
        await initialized;
        events.length = 0;

        if (carrier === "deck") {
          const submitted = guest.submitDeck(["Island"], []);
          await vi.waitFor(() => expect(conn.sentRaw).toHaveLength(2));
          const command = await decodeDraftWireMessage(conn.sentRaw[1]!);
          expect(command.type).toBe("draft_submit_deck");
          if (command.type !== "draft_submit_deck") throw new Error("Expected draft deck submission");
          await conn.receiveRaw(rawMessage(legacyCarrierMessage(carrier, rejectedView, command.submissionId)));
          await expect(submitted).rejects.toBeInstanceOf(Draft27IncompatibilityError);
          expect(persistence.clearDraftDeckSubmission).not.toHaveBeenCalled();
        } else {
          await conn.receiveRaw(rawMessage(legacyCarrierMessage(carrier, rejectedView)));
        }

        expect(guest.view).toEqual(admittedView);
      }

      expect(events.slice(-2)).toEqual([
        {
          type: "reconnectFailed",
          failure: {
            kind: "incompatible",
            message: expect.any(String),
            code: INCOMPATIBLE_SHAPE_CODES[shape],
          },
        },
        { type: "error", message: expect.any(String) },
      ]);
      expect(events.some((event) => event.type === "joined" || event.type === "reconnected" || event.type === "viewUpdated" || event.type === "pickAcknowledged" || event.type === "deckSubmissionAcknowledged"))
        .toBe(false);
      expect(persistence.clearDraftGuestRecovery).not.toHaveBeenCalled();
    },
  );

  it("rejects an exact-session pending leave on raw incompatible state without revoking recovery", async () => {
    const { guest, conn, events } = createGuest({ kind: "new", roomCode: "ABCDE", displayName: "Alice" });
    const initialized = guest.initialize();
    const admittedView = view(1);
    await conn.receiveRaw(rawMessage(welcome(admittedView)));
    await initialized;
    events.length = 0;

    const leaveResult = guest.leave().then(
      () => ({ status: "fulfilled" as const }),
      (error: unknown) => ({ status: "rejected" as const, error }),
    );
    await vi.waitFor(() => expect(conn.sentRaw).toHaveLength(2));
    await expect(decodeDraftWireMessage(conn.sentRaw[1]!)).resolves.toEqual({
      type: "draft_leave", draftProtocolVersion: DRAFT_PROTOCOL_VERSION, draftToken: "guest-token",
    });

    // This reaches the real raw codec/session dispatch after the exact live
    // session owns the leave acknowledgement; it is not a direct handler call.
    await conn.receiveRaw(rawMessage({
      type: "draft_state_update",
      view: incompatibleView(2, "fifoFlow"),
    }));

    await expect(leaveResult).resolves.toEqual({
      status: "rejected",
      error: expect.any(Draft27IncompatibilityError),
    });
    expect(guest.view).toEqual(admittedView);
    expect(guest.seat).toBe(2);
    expect(guest.token).toBe("guest-token");
    expect(guest.isRecoveryRevoked).toBe(false);
    expect(conn.open).toBe(false);
    expect(persistence.clearDraftGuestRecovery).not.toHaveBeenCalled();
    expect(persistence.clearDraftDeckSubmission).not.toHaveBeenCalled();
    expect(events).toEqual([
      {
        type: "reconnectFailed",
        failure: { kind: "incompatible", message: expect.any(String), code: INCOMPATIBLE_SHAPE_CODES.fifoFlow },
      },
      { type: "error", message: expect.any(String) },
    ]);

    // The ordinary legacy acknowledgement sibling still completes a leave and
    // performs its intentional terminal recovery revocation.
    const control = createGuest({ kind: "new", roomCode: "ABCDE", displayName: "Alice" });
    const controlInitialized = control.guest.initialize();
    await control.conn.receiveRaw(rawMessage(welcome(view(3))));
    await controlInitialized;
    control.events.length = 0;

    const acknowledgedLeave = control.guest.leave().then(
      () => ({ status: "fulfilled" as const }),
      (error: unknown) => ({ status: "rejected" as const, error }),
    );
    await vi.waitFor(() => expect(control.conn.sentRaw).toHaveLength(2));
    await expect(decodeDraftWireMessage(control.conn.sentRaw[1]!)).resolves.toEqual({
      type: "draft_leave", draftProtocolVersion: DRAFT_PROTOCOL_VERSION, draftToken: "guest-token",
    });
    await control.conn.receiveRaw(rawMessage({
      type: "draft_leave_ack",
      draftProtocolVersion: DRAFT_PROTOCOL_VERSION,
      draftToken: "guest-token",
    }));

    await expect(acknowledgedLeave).resolves.toEqual({ status: "fulfilled" });
    expect(control.guest.isRecoveryRevoked).toBe(true);
    expect(control.events).toEqual([]);
  });

  it.each(["fifoFlow", "reviewStatus"] as const)(
    "stops a rejected reconnect $shape route before it can redial",
    async (shape) => {
      vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout"] });
      const reconnectConn = new FakeDraftDataConnection();
      reconnectConn.open = false;
      const connect = vi.fn().mockReturnValue(reconnectConn);
      const { guest, conn, events } = createGuest(
        { kind: "new", roomCode: "ABCDE", displayName: "Alice" },
        { connect } as never,
      );
      const initialized = guest.initialize();
      const admittedView = view(1);
      await conn.receiveRaw(rawMessage(welcome(admittedView)));
      await initialized;

      const deckSubmission = guest.submitDeck(["Island"], []);
      await vi.waitFor(() => expect(conn.sentRaw).toHaveLength(2));
      const command = await decodeDraftWireMessage(conn.sentRaw[1]!);
      expect(command.type).toBe("draft_submit_deck");
      if (command.type !== "draft_submit_deck") throw new Error("Expected draft deck submission");
      events.length = 0;
      persistence.clearDraftDeckSubmission.mockClear();

      conn.simulateClose();
      await vi.advanceTimersByTimeAsync(1_000);
      expect(connect).toHaveBeenCalledExactlyOnceWith("phase2-ABCDE", PEER_CONNECT_OPTIONS);
      reconnectConn.simulateOpen();
      await vi.advanceTimersByTimeAsync(0);
      await expect(decodeDraftWireMessage(reconnectConn.sentRaw[0]!)).resolves.toEqual({
        type: "draft_reconnect",
        draftProtocolVersion: DRAFT_PROTOCOL_VERSION,
        draftToken: "guest-token",
      });

      await reconnectConn.receiveRaw(rawMessage(legacyCarrierMessage(
        "reconnect",
        incompatibleView(2, shape),
      )));
      await expect(deckSubmission).rejects.toBeInstanceOf(Draft27IncompatibilityError);

      expect(guest.view).toEqual(admittedView);
      expect(reconnectConn.open).toBe(false);
      expect(persistence.clearDraftDeckSubmission).not.toHaveBeenCalled();
      expect(persistence.clearDraftGuestRecovery).not.toHaveBeenCalled();
      expect(events).toEqual([
        { type: "reconnecting", attempt: 1 },
        {
          type: "reconnectFailed",
          failure: { kind: "incompatible", message: expect.any(String), code: INCOMPATIBLE_SHAPE_CODES[shape] },
        },
        { type: "error", message: expect.any(String) },
      ]);

      await vi.advanceTimersByTimeAsync(60_000 * 3);
      expect(connect).toHaveBeenCalledOnce();
      expect(vi.getTimerCount()).toBe(0);

      // Retained recovery data can seed a new, valid route. A late old frame
      // is ignored while the current successor still updates normally.
      const fresh = createGuest({
        kind: "reconnect",
        roomCode: "ABCDE",
        displayName: "Alice",
        draftToken: "guest-token",
      });
      const recovered = fresh.guest.initialize();
      await fresh.conn.receiveRaw(rawMessage(firstContact("reconnect", view(3))));
      await recovered;
      await reconnectConn.receiveRaw(rawMessage({ type: "draft_state_update", view: view(99) }));
      expect(fresh.guest.view).toEqual(view(3));
      await fresh.conn.receiveRaw(rawMessage({ type: "draft_state_update", view: view(4) }));
      expect(fresh.guest.view).toEqual(view(4));
    },
  );

  it.each(["welcome", "reconnect", "state", "pick", "deck"] as const)(
    "admits a shared-stack %s delivery",
    async (carrier) => {
      const connection: DraftGuestConnection = carrier === "reconnect"
        ? { kind: "reconnect", roomCode: "ABCDE", displayName: "Alice", draftToken: "guest-token" }
        : { kind: "new", roomCode: "ABCDE", displayName: "Alice" };
      const { guest, conn, events } = createGuest(connection);
      const acceptedView = carrier === "deck" ? sharedStackView(2, "Deckbuilding") : sharedStackView(2);

      if (carrier === "welcome" || carrier === "reconnect") {
        const initialized = guest.initialize();
        await conn.receiveRaw(rawMessage(sharedStackCarrierMessage(carrier, acceptedView)));
        await initialized;
      } else {
        const initialized = guest.initialize();
        await conn.receiveRaw(rawMessage(sharedStackCarrierMessage("welcome", sharedStackView(1))));
        await initialized;
        events.length = 0;

        if (carrier === "deck") {
          const submitted = guest.submitDeck(["Island"], []);
          await vi.waitFor(() => expect(conn.sentRaw).toHaveLength(2));
          const command = await decodeDraftWireMessage(conn.sentRaw[1]!);
          expect(command.type).toBe("draft_submit_deck");
          if (command.type !== "draft_submit_deck") throw new Error("Expected draft deck submission");
          await conn.receiveRaw(rawMessage(sharedStackCarrierMessage(carrier, acceptedView, command.submissionId)));
          await submitted;
        } else {
          await conn.receiveRaw(rawMessage(sharedStackCarrierMessage(carrier, acceptedView)));
        }
      }

      expect(guest.view).toEqual(acceptedView);
      expect(guest.view?.distribution).toEqual({ SharedStackPiles: { pile_count: 3 } });
      expect(guest.view?.play_first_chooser).toBe(1);
      switch (carrier) {
        case "welcome":
          expect(events).toContainEqual({ type: "joined", seatIndex: 1, draftCode: "draft-xyz" });
          break;
        case "reconnect":
          expect(events).toContainEqual({ type: "reconnected", seatIndex: 1 });
          break;
        case "state":
          expect(events).toContainEqual({ type: "viewUpdated", view: acceptedView });
          break;
        case "pick":
          expect(events).toContainEqual({ type: "pickAcknowledged", view: acceptedView });
          break;
        case "deck":
          expect(events).toContainEqual({
            type: "deckSubmissionAcknowledged",
            submissionId: expect.any(String),
            view: acceptedView,
          });
          break;
      }
    },
  );

  it.each(
    (["welcome", "reconnect", "state", "pick", "deck"] as const).flatMap((carrier) =>
      (["fifoFlow", "reviewStatus", "residualFifo"] as const).map((shape) => ({ carrier, shape })),
    ),
  )(
    "refuses a shared-stack view carrying $shape before $carrier carrier effects",
    async ({ carrier, shape }) => {
      const connection: DraftGuestConnection = carrier === "reconnect"
        ? { kind: "reconnect", roomCode: "ABCDE", displayName: "Alice", draftToken: "guest-token" }
        : { kind: "new", roomCode: "ABCDE", displayName: "Alice" };
      const { guest, conn, events } = createGuest(connection);
      const rejectedView = withIncompatibleShape(
        carrier === "deck" ? sharedStackView(2, "Deckbuilding") : sharedStackView(2),
        shape,
      );
      const admittedView = sharedStackView(1);

      if (carrier === "welcome" || carrier === "reconnect") {
        const initialized = guest.initialize();
        await conn.receiveRaw(rawMessage(sharedStackCarrierMessage(carrier, rejectedView)));
        await expect(initialized).rejects.toBeInstanceOf(Draft27IncompatibilityError);
        expect(guest.view).toBeNull();
      } else {
        const initialized = guest.initialize();
        await conn.receiveRaw(rawMessage(sharedStackCarrierMessage("welcome", admittedView)));
        await initialized;
        events.length = 0;

        if (carrier === "deck") {
          const submitted = guest.submitDeck(["Island"], []);
          await vi.waitFor(() => expect(conn.sentRaw).toHaveLength(2));
          const command = await decodeDraftWireMessage(conn.sentRaw[1]!);
          expect(command.type).toBe("draft_submit_deck");
          if (command.type !== "draft_submit_deck") throw new Error("Expected draft deck submission");
          await conn.receiveRaw(rawMessage(sharedStackCarrierMessage(carrier, rejectedView, command.submissionId)));
          await expect(submitted).rejects.toBeInstanceOf(Draft27IncompatibilityError);
          expect(persistence.clearDraftDeckSubmission).not.toHaveBeenCalled();
        } else {
          await conn.receiveRaw(rawMessage(sharedStackCarrierMessage(carrier, rejectedView)));
        }

        expect(guest.view).toEqual(admittedView);
      }

      expect(events.slice(-2)).toMatchObject([
        { type: "reconnectFailed", failure: { kind: "incompatible" } },
        { type: "error" },
      ]);
      expect(events.slice(-2)).toEqual([
        {
          type: "reconnectFailed",
          failure: {
            kind: "incompatible",
            message: INCOMPATIBLE_SHAPE_MESSAGES[shape],
            code: INCOMPATIBLE_SHAPE_CODES[shape],
          },
        },
        { type: "error", message: INCOMPATIBLE_SHAPE_MESSAGES[shape] },
      ]);
      expect(events.some((event) => event.type === "joined" || event.type === "reconnected" || event.type === "viewUpdated" || event.type === "pickAcknowledged" || event.type === "deckSubmissionAcknowledged"))
        .toBe(false);
      expect(persistence.clearDraftGuestRecovery).not.toHaveBeenCalled();
    },
  );

  it("still reports an active handshake persistence failure", async () => {
    persistence.saveDraftGuestSession.mockRejectedValueOnce(new Error("IDB unavailable"));
    const { guest, conn, events } = createGuest({ kind: "new", roomCode: "ABCDE", displayName: "Alice" });
    const rejected = expect(guest.initialize()).rejects.toThrow("IDB unavailable");

    await conn.receiveRaw(rawMessage(welcome(view(1))));
    await rejected;

    expect(events).toEqual([{ type: "error", message: "Could not save draft recovery details" }]);
    expect(persistence.saveActiveDraftGuest).not.toHaveBeenCalled();
  });

  it("reports a first-contact protocol mismatch as incompatible without a compatibility code", async () => {
    vi.spyOn(console, "error").mockImplementation(() => {});
    const { guest, conn, events } = createGuest({ kind: "new", roomCode: "ABCDE", displayName: "Alice" });
    const rejected = expect(guest.initialize()).rejects.toThrow("Draft protocol mismatch");
    const mismatch: Record<string, unknown> = { ...welcome(view(1)), draftProtocolVersion: DRAFT_PROTOCOL_VERSION - 1 };

    await conn.receiveRaw(rawMessage(mismatch));
    await rejected;

    const failures = events.flatMap((event) => (event.type === "reconnectFailed" ? [event.failure] : []));
    // Reach: the version check reported its own incompatible failure.
    expect(failures).toEqual([{
      kind: "incompatible",
      message: `Draft protocol mismatch: host v${DRAFT_PROTOCOL_VERSION - 1}, client v${DRAFT_PROTOCOL_VERSION}. Refresh both windows.`,
    }]);
    expect(failures[0]).not.toHaveProperty("code");
  });

  async function expectFifoViewRefused(carrier: ViewCarrier, rejectedView: DraftPlayerView) {
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    const decode = vi.spyOn(protocol, "decodeDraftWireMessage");
    const connection: DraftGuestConnection = carrier === "reconnect"
      ? { kind: "reconnect", roomCode: "ABCDE", displayName: "Alice", draftToken: "guest-token" }
      : { kind: "new", roomCode: "ABCDE", displayName: "Alice" };
    const { guest, conn, events } = createGuest(connection);
    const admittedView = view(1);
    const initialized = guest.initialize();
    // A refused first contact rejects this; the rejection is asserted below.
    initialized.catch(() => {});
    let submitted: Promise<unknown> | null = null;
    let submissionId = "submission-1";
    if (carrier !== "welcome" && carrier !== "reconnect") {
      await conn.receiveRaw(rawMessage(welcome(admittedView)));
      await initialized;
      events.length = 0;
      if (carrier === "deck") {
        submitted = guest.submitDeck(["Island"], []);
        submitted.catch(() => {});
        await vi.waitFor(() => expect(conn.sentRaw).toHaveLength(2));
        const command = await decodeDraftWireMessage(conn.sentRaw[1]!);
        if (command.type !== "draft_submit_deck") throw new Error("Expected draft deck submission");
        submissionId = command.submissionId;
      }
    }
    const decodes = decode.mock.calls.length;

    await conn.receiveRaw(rawMessage(legacyCarrierMessage(carrier, rejectedView, submissionId)));

    // Reach: the frame reached the production decoder.
    expect(decode.mock.calls.length).toBe(decodes + 1);
    // Asserted before any wait on the guest, which a dropped frame leaves pending.
    expect(warn.mock.calls
      .filter(([label]) => label === "[DraftPeerSession] decode error:")
      .map(([, error]) => String(error))).toEqual([]);
    expect(events.slice(-2)).toEqual([
      {
        type: "reconnectFailed",
        failure: { kind: "incompatible", message: INCOMPATIBLE_SHAPE_MESSAGES.fifoFlow, code: INCOMPATIBLE_SHAPE_CODES.fifoFlow },
      },
      { type: "error", message: INCOMPATIBLE_SHAPE_MESSAGES.fifoFlow },
    ]);
    expect(events.some((event) => event.type === "joined" || event.type === "reconnected" || event.type === "viewUpdated" || event.type === "pickAcknowledged" || event.type === "deckSubmissionAcknowledged"))
      .toBe(false);
    expect(persistence.clearDraftGuestRecovery).not.toHaveBeenCalled();
    if (carrier === "welcome" || carrier === "reconnect") {
      await expect(initialized).rejects.toBeInstanceOf(Draft27IncompatibilityError);
      expect(guest.view).toBeNull();
    } else {
      expect(guest.view).toEqual(admittedView);
    }
    if (submitted) {
      await expect(submitted).rejects.toBeInstanceOf(Draft27IncompatibilityError);
      expect(persistence.clearDraftDeckSubmission).not.toHaveBeenCalled();
    }
  }

  it.each(["welcome", "reconnect", "state", "pick", "deck"] as const)(
    "refuses a FIFO view whose seat holds two packs before %s carrier effects",
    (carrier) => expectFifoViewRefused(carrier, fifoPlayerView("queued_view")),
  );

  it.each(["welcome", "reconnect", "state", "pick", "deck"] as const)(
    "refuses a FIFO Review view before %s carrier effects",
    (carrier) => expectFifoViewRefused(carrier, fifoPlayerView("review_view")),
  );
});
