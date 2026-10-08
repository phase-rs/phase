import { describe, expect, it, vi } from "vitest";

import { P2PDraftGuest } from "../p2p-draft-guest";
import { P2PDraftHost } from "../p2p-draft-host";
import { DRAFT_PROTOCOL_VERSION } from "../../network/draftProtocol";

const sessionState = vi.hoisted(() => ({
  firstContact: null as ((message: unknown) => void | Promise<void>) | null,
  send: vi.fn(async () => {}),
  close: vi.fn(),
}));

vi.mock("../../network/draftPeerSession", () => ({
  createDraftPeerSession: vi.fn(() => ({
    onMessage: vi.fn((handler: (message: unknown) => void) => {
      sessionState.firstContact = handler;
      return vi.fn();
    }),
    onDisconnect: vi.fn(() => vi.fn()),
    send: sessionState.send,
    close: sessionState.close,
  })),
}));

describe("P2P draft first-contact gate", () => {
  it("puts the exact draft version on both join and reconnect frames", async () => {
    const send = vi.fn(async () => {});
    const newGuest = new P2PDraftGuest(
      {} as never,
      "phase2-ABCDE",
      {} as never,
      { kind: "new", roomCode: "ABCDE", displayName: "Alice" },
    );
    const reconnectingGuest = new P2PDraftGuest(
      {} as never,
      "phase2-ABCDE",
      {} as never,
      { kind: "reconnect", roomCode: "ABCDE", displayName: "Alice", draftToken: "opaque-token" },
    );
    (newGuest as unknown as { session: { send: typeof send } }).session = { send };
    await (newGuest as unknown as { sendFirstContact: (session: unknown, reconnect: boolean) => Promise<void> })
      .sendFirstContact({ send }, false);
    expect(send).toHaveBeenCalledWith({
      type: "draft_join",
      displayName: "Alice",
      draftProtocolVersion: DRAFT_PROTOCOL_VERSION,
    });

    send.mockClear();
    (reconnectingGuest as unknown as { session: { send: typeof send } }).session = { send };
    await (reconnectingGuest as unknown as { sendFirstContact: (session: unknown, reconnect: boolean) => Promise<void> })
      .sendFirstContact({ send }, true);
    expect(send).toHaveBeenCalledWith({
      type: "draft_reconnect",
      draftToken: "opaque-token",
      draftProtocolVersion: DRAFT_PROTOCOL_VERSION,
    });
  });

  it("rejects a v25 peer before it can allocate a seat because v26 requires commanders_required", async () => {
    sessionState.firstContact = null;
    sessionState.send.mockClear();
    sessionState.close.mockClear();
    let releaseSend!: () => void;
    sessionState.send.mockImplementationOnce(() => new Promise<void>((resolve) => {
      releaseSend = resolve;
    }));
    const host = new P2PDraftHost(
      { id: "phase2-ABCDE" } as never,
      () => () => {},
      { type: "Set", data: { pools: [{ code: "TST" }], sequence: ["TST"] } } as never,
      "Premier",
      8,
      "Host",
      "Swiss",
      "Competitive",
    );
    const allocate = vi.fn();
    (host as unknown as { handleNewGuest: typeof allocate }).handleNewGuest = allocate;

    (host as unknown as { handleNewConnection: (connection: unknown) => void })
      .handleNewConnection({} as never);
    const rejected = sessionState.firstContact!({
      type: "draft_join",
      displayName: "Alice",
      // v25 is the launch-capability contract. It lacks this client's exact
      // procedure-owned commander count and must not complete first contact
      // under the same version number.
      draftProtocolVersion: 25,
    } as never);

    expect(allocate).not.toHaveBeenCalled();
    expect(sessionState.send).toHaveBeenCalledWith(expect.objectContaining({
      type: "draft_reconnect_rejected",
      kind: "ProtocolMismatch",
    }));
    // `DraftPeerSession.send()` queues asynchronous encoding. Closing before
    // that promise settles suppresses its delivery, so the host must await it.
    expect(sessionState.close).not.toHaveBeenCalled();
    releaseSend();
    await rejected;
    expect(sessionState.close).toHaveBeenCalledWith("Draft protocol mismatch");
  });
});
