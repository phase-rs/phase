import { describe, expect, it } from "vitest";

import {
  DRAFT_PROTOCOL_VERSION,
  encodeDraftWireMessage,
  decodeDraftWireMessage,
  validateDraftMessage,
} from "../draftProtocol";
import type { DraftP2PMessage } from "../draftProtocol";

describe("draftProtocol", () => {
  describe("DRAFT_PROTOCOL_VERSION", () => {
    it("is version 3", () => {
      expect(DRAFT_PROTOCOL_VERSION).toBe(3);
    });
  });

  describe("validateDraftMessage", () => {
    it("accepts valid draft_join message", () => {
      const msg = validateDraftMessage({ type: "draft_join", displayName: "Alice" });
      expect(msg.type).toBe("draft_join");
    });

    it("accepts valid draft_pick message", () => {
      const msg = validateDraftMessage({ type: "draft_pick", cardInstanceId: "card-001" });
      expect(msg.type).toBe("draft_pick");
    });

    it("accepts valid draft_welcome message", () => {
      const msg = validateDraftMessage({
        type: "draft_welcome",
        draftProtocolVersion: 1,
        draftToken: "token-123",
        seatIndex: 3,
        view: {},
        draftCode: "draft-abc",
      });
      expect(msg.type).toBe("draft_welcome");
    });

    it("rejects missing type field", () => {
      expect(() => validateDraftMessage({})).toThrow("missing type field");
    });

    it("rejects null input", () => {
      expect(() => validateDraftMessage(null)).toThrow("missing type field");
    });

    it("rejects unknown message type", () => {
      expect(() => validateDraftMessage({ type: "unknown_type" })).toThrow("Invalid draft message type");
    });

    it("rejects game protocol message types", () => {
      expect(() => validateDraftMessage({ type: "game_setup" })).toThrow("Invalid draft message type");
    });

    it.each([
      "draft_join",
      "draft_reconnect",
      "draft_pick",
      "draft_submit_deck",
      "draft_welcome",
      "draft_reconnect_ack",
      "draft_reconnect_rejected",
      "draft_state_update",
      "draft_pick_ack",
      "draft_error",
      "draft_kicked",
      "draft_pairing",
      "draft_match_result",
      "draft_paused",
      "draft_resumed",
      "draft_lobby_update",
      "draft_host_left",
    ])("accepts message type '%s'", (msgType) => {
      const msg = validateDraftMessage({ type: msgType });
      expect(msg.type).toBe(msgType);
    });
  });

  describe("wire encoding/decoding round-trip", () => {
    it("round-trips a small message (raw path)", async () => {
      const msg: DraftP2PMessage = { type: "draft_join", displayName: "Bob" };
      const encoded = await encodeDraftWireMessage(msg);
      // Small messages use raw format (0x00 prefix)
      expect(encoded[0]).toBe(0x00);

      const decoded = await decodeDraftWireMessage(encoded);
      expect(decoded).toEqual(msg);
    });

    it("round-trips a large message (gzip path)", async () => {
      // Build a message large enough to trigger compression
      const longView = {
        status: "Drafting",
        kind: "Premier",
        current_pack_number: 1,
        pick_number: 3,
        pass_direction: "Left",
        current_pack: Array.from({ length: 14 }, (_, i) => ({
          instance_id: `card-${i}`,
          name: `Test Card With A Very Long Name Number ${i}`,
          set_code: "TST",
          collector_number: String(i + 1),
          rarity: "common",
          colors: ["W", "U"],
          cmc: i % 7,
          type_line: "Creature - Human Wizard",
        })),
        pool: [],
        seats: [],
        cards_per_pack: 14,
        pack_count: 3,
        min_deck_size: 40,
        addable_cards: ["Plains", "Island", "Swamp", "Mountain", "Forest"],
      };
      const msg: DraftP2PMessage = {
        type: "draft_state_update",
        view: longView as unknown as DraftP2PMessage & { type: "draft_state_update" } extends { view: infer V } ? V : never,
      };

      const encoded = await encodeDraftWireMessage(msg);
      // Large messages use gzip format (0x01 prefix)
      expect(encoded[0]).toBe(0x01);

      const decoded = await decodeDraftWireMessage(encoded);
      expect(decoded).toEqual(msg);
    });

    it("rejects empty bytes", async () => {
      await expect(decodeDraftWireMessage(new Uint8Array([]))).rejects.toThrow("empty draft wire message");
    });

    it("rejects unknown format version", async () => {
      await expect(
        decodeDraftWireMessage(new Uint8Array([0x42, 0x00])),
      ).rejects.toThrow("unknown draft wire format version");
    });
  });
});
