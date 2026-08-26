import { describe, expect, it, vi } from "vitest";

import { P2PDraftHost } from "../p2p-draft-host";
import { createDraftPeerSession } from "../../network/draftPeerSession";
import type { DraftPeerSession } from "../../network/draftPeerSession";
import { decodeDraftWireMessage } from "../../network/draftProtocol";
import type { DraftReconnectRejectionKind } from "../../network/draftProtocol";

describe("P2P draft host reconnect rejection", () => {
  it.each([
    ["ProtocolMismatch", "Refresh both windows", "Draft protocol mismatch"],
    ["Kicked", "Player kicked", "Kicked"],
    ["UnknownToken", "Unknown token", "Unknown token"],
    ["NoReconnectWindow", "No grace window active for this seat", "Not in grace"],
  ] as const)("flushes %s through the real DraftPeerSession before closing", async (kind, reason, closeReason) => {
    const order: string[] = [];
    const send = vi.fn((_: Uint8Array) => order.push("send"));
    const conn = {
      open: true,
      on: vi.fn(),
      send,
      close: vi.fn(() => order.push("close")),
    };
    const session = createDraftPeerSession(conn as never);
    const host = new P2PDraftHost(
      { id: "phase2-ABCDE" } as never,
      () => () => {},
      { type: "Set", data: { set_pool_json: "{}" } } as never,
      "Premier",
      8,
      "Host",
      "Swiss",
      "Competitive",
    );

    await (host as unknown as {
      rejectAndClose: (session: DraftPeerSession, kind: DraftReconnectRejectionKind, reason: string, closeReason: string) => Promise<void>;
    }).rejectAndClose(session, kind, reason, closeReason);

    expect(send).toHaveBeenCalledOnce();
    expect(conn.close).toHaveBeenCalledOnce();
    expect(order).toEqual(["send", "close"]);
    await expect(decodeDraftWireMessage(send.mock.calls[0]![0])).resolves.toMatchObject({
      type: "draft_reconnect_rejected",
      kind,
      reason,
    });
  });
});
