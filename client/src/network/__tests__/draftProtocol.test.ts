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
    it("is version 11", () => {
      expect(DRAFT_PROTOCOL_VERSION).toBe(11);
    });
  });

  describe("pool-group shape upgrade (v10 → v11)", () => {
    const card = {
      instance_id: "adept-1",
      name: "Adept",
      set_code: "TST",
      collector_number: "1",
      rarity: "common",
      colors: ["W"],
      cmc: 2,
      type_line: "Creature",
    };

    it("upgrades a v10 view: fills the rarity axis and the entry instance ids", () => {
      const msg = validateDraftMessage({
        type: "draft_state_update",
        view: {
          status: "Deckbuilding",
          draft_effects: [],
          seats: [],
          pool: [card],
          // v10 shape: no rarity_groups, entry without instance_ids
          pool_groups: {
            color_groups: [],
            type_groups: [{ kind: "creature", total: 1, cards: [{ card, count: 1 }] }],
            cmc_groups: [],
            color_counts: { white: 1, blue: 0, black: 0, red: 0, green: 0 },
          },
        },
      }) as { view: { pool_groups: { rarity_groups: unknown[]; type_groups: Array<{ cards: Array<{ instance_ids: string[] }> }> } } };

      expect(msg.view.pool_groups.rarity_groups).toEqual([]);
      const upgraded = msg.view.pool_groups as unknown as {
        type_filter_options: unknown[];
        color_filter_options: unknown[];
      };
      expect(upgraded.type_filter_options).toEqual([]);
      expect(upgraded.color_filter_options).toEqual([]);
      expect(msg.view.pool_groups.type_groups[0].cards[0].instance_ids).toEqual(["adept-1"]);
    });

    it("passes a v11 view through unchanged", () => {
      const entry = { card, count: 2, instance_ids: ["adept-1", "adept-2"] };
      const msg = validateDraftMessage({
        type: "draft_state_update",
        view: {
          status: "Deckbuilding",
          draft_effects: [],
          seats: [],
          pool: [card],
          pool_groups: {
            color_groups: [],
            type_groups: [{ kind: "creature", total: 2, cards: [entry] }],
            cmc_groups: [],
            rarity_groups: [{ kind: "common", total: 2, cards: [entry] }],
            color_counts: { white: 2, blue: 0, black: 0, red: 0, green: 0 },
          },
        },
      }) as { view: { pool_groups: { rarity_groups: Array<{ cards: Array<{ instance_ids: string[] }> }>; type_groups: Array<{ cards: Array<{ instance_ids: string[] }> }> } } };

      expect(msg.view.pool_groups.type_groups[0].cards[0].instance_ids).toEqual([
        "adept-1",
        "adept-2",
      ]);
      expect(msg.view.pool_groups.rarity_groups[0].cards[0].instance_ids).toEqual([
        "adept-1",
        "adept-2",
      ]);
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

    it("accepts a draft-effect pick message", () => {
      const msg = validateDraftMessage({
        type: "draft_pick_with_draft_effect",
        effectCardInstanceId: "cogwork-1",
        cardInstanceIds: ["card-001", "card-002"],
      });
      expect(msg).toMatchObject({
        type: "draft_pick_with_draft_effect",
        effectCardInstanceId: "cogwork-1",
        cardInstanceIds: ["card-001", "card-002"],
      });
    });

    it.each([
      { effectCardInstanceId: null, cardInstanceIds: ["card-001", "card-002"] },
      { effectCardInstanceId: {}, cardInstanceIds: ["card-001", "card-002"] },
      { effectCardInstanceId: "", cardInstanceIds: ["card-001", "card-002"] },
      { effectCardInstanceId: "x".repeat(257), cardInstanceIds: ["card-001", "card-002"] },
      { effectCardInstanceId: "cogwork-1", cardInstanceIds: null },
      { effectCardInstanceId: "cogwork-1", cardInstanceIds: {} },
      { effectCardInstanceId: "cogwork-1", cardInstanceIds: ["card-001"] },
      { effectCardInstanceId: "cogwork-1", cardInstanceIds: ["card-001", "card-002", "card-003"] },
      { effectCardInstanceId: "cogwork-1", cardInstanceIds: ["card-001", "card-001"] },
      { effectCardInstanceId: "cogwork-1", cardInstanceIds: [null, "card-002"] },
      { effectCardInstanceId: "cogwork-1", cardInstanceIds: ["x".repeat(257), "card-002"] },
    ])("rejects malformed draft-effect pick payloads", (payload) => {
      expect(() => validateDraftMessage({
        type: "draft_pick_with_draft_effect",
        ...payload,
      })).toThrow("Invalid draft-effect pick");
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

    it("normalizes missing face-up draft arrays in received player views", () => {
      const msg = validateDraftMessage({
        type: "draft_state_update",
        view: {
          seats: [{ seat_index: 1, display_name: "Alex" }],
        },
      });

      expect(msg.type).toBe("draft_state_update");
      if (msg.type === "draft_state_update") {
        expect(msg.view.draft_effects).toEqual([]);
        expect(msg.view.seats[0].face_up_draft_cards).toEqual([]);
      }
    });

    it.each([null, {}])("rejects present non-array draft_effects values", (draftEffects) => {
      expect(() => validateDraftMessage({
        type: "draft_state_update",
        view: { draft_effects: draftEffects, seats: [] },
      })).toThrow("draft_effects must be an array");
    });

    it.each([null, {}])("rejects present non-array seats values", (seats) => {
      expect(() => validateDraftMessage({
        type: "draft_state_update",
        view: { draft_effects: [], seats },
      })).toThrow("seats must be an array");
    });

    it.each([null, {}])("rejects present non-array face-up draft cards", (faceUpCards) => {
      expect(() => validateDraftMessage({
        type: "draft_state_update",
        view: {
          draft_effects: [],
          seats: [{ face_up_draft_cards: faceUpCards }],
        },
      })).toThrow("face_up_draft_cards must be an array");
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
      "draft_pick_with_draft_effect",
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
      "draft_match_settlement",
      "draft_match_settlement_ack",
      "draft_paused",
      "draft_resumed",
      "draft_lobby_update",
      "draft_host_left",
      "draft_timer_sync",
      "draft_request_advance",
      "draft_match_start",
      "draft_bo3_sideboard_prompt",
      "draft_bo3_between_games",
      "draft_bo3_sideboard_submit",
      "draft_bo3_intergame_command",
      "draft_bo3_intergame_authorized",
      "draft_bo3_intergame_receipt",
      "draft_bo3_play_draw_prompt",
      "draft_bo3_play_draw_choice",
      "draft_bo3_game_start",
      "draft_bo3_score_update",
      "draft_bo3_match_complete",
    ])("accepts message type '%s'", (msgType) => {
      const msg = validateDraftMessage(
        msgType === "draft_pick_with_draft_effect"
          ? {
              type: msgType,
              effectCardInstanceId: "cogwork-1",
              cardInstanceIds: ["card-001", "card-002"],
            }
          : { type: msgType },
      );
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
        status: "Deckbuilding",
        kind: "Sealed",
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
        pool: [
          {
            instance_id: "pack-1-card-1",
            name: "First Pull",
            set_code: "TST",
            collector_number: "101",
            rarity: "common",
            colors: ["W"],
            cmc: 1,
            type_line: "Creature — Test",
          },
          {
            instance_id: "pack-2-card-1",
            name: "Second Pull",
            set_code: "TST",
            collector_number: "102",
            rarity: "uncommon",
            colors: ["U"],
            cmc: 2,
            type_line: "Instant",
          },
        ],
        draft_effects: [],
        pool_groups: {
          color_groups: [
            { kind: "white", total: 1, cards: [{ card: {
              instance_id: "pack-1-card-1",
              name: "First Pull",
              set_code: "TST",
              collector_number: "101",
              rarity: "common",
              colors: ["W"],
              cmc: 1,
              type_line: "Creature — Test",
            }, count: 1, instance_ids: ["pack-1-card-1"] }] },
            { kind: "blue", total: 1, cards: [{ card: {
              instance_id: "pack-2-card-1",
              name: "Second Pull",
              set_code: "TST",
              collector_number: "102",
              rarity: "uncommon",
              colors: ["U"],
              cmc: 2,
              type_line: "Instant",
            }, count: 1, instance_ids: ["pack-2-card-1"] }] },
          ],
          type_groups: [
            { kind: "creature", total: 1, cards: [{ card: {
              instance_id: "pack-1-card-1",
              name: "First Pull",
              set_code: "TST",
              collector_number: "101",
              rarity: "common",
              colors: ["W"],
              cmc: 1,
              type_line: "Creature — Test",
            }, count: 1, instance_ids: ["pack-1-card-1"] }] },
            { kind: "instant", total: 1, cards: [{ card: {
              instance_id: "pack-2-card-1",
              name: "Second Pull",
              set_code: "TST",
              collector_number: "102",
              rarity: "uncommon",
              colors: ["U"],
              cmc: 2,
              type_line: "Instant",
            }, count: 1, instance_ids: ["pack-2-card-1"] }] },
          ],
          cmc_groups: [
            { kind: "mana_value1", total: 1, cards: [{ card: {
              instance_id: "pack-1-card-1",
              name: "First Pull",
              set_code: "TST",
              collector_number: "101",
              rarity: "common",
              colors: ["W"],
              cmc: 1,
              type_line: "Creature — Test",
            }, count: 1, instance_ids: ["pack-1-card-1"] }] },
            { kind: "mana_value2", total: 1, cards: [{ card: {
              instance_id: "pack-2-card-1",
              name: "Second Pull",
              set_code: "TST",
              collector_number: "102",
              rarity: "uncommon",
              colors: ["U"],
              cmc: 2,
              type_line: "Instant",
            }, count: 1, instance_ids: ["pack-2-card-1"] }] },
          ],
          rarity_groups: [
            { kind: "common", total: 1, cards: [{ card: {
              instance_id: "pack-1-card-1",
              name: "First Pull",
              set_code: "TST",
              collector_number: "101",
              rarity: "common",
              colors: ["W"],
              cmc: 1,
              type_line: "Creature — Test",
            }, count: 1, instance_ids: ["pack-1-card-1"] }] },
            { kind: "uncommon", total: 1, cards: [{ card: {
              instance_id: "pack-2-card-1",
              name: "Second Pull",
              set_code: "TST",
              collector_number: "102",
              rarity: "uncommon",
              colors: ["U"],
              cmc: 2,
              type_line: "Instant",
            }, count: 1, instance_ids: ["pack-2-card-1"] }] },
          ],
          type_filter_options: ["creature", "instant"],
          color_filter_options: ["white", "blue"],
          color_counts: { white: 1, blue: 1, black: 0, red: 0, green: 0 },
        },
        sealed_packs: [
          [{
            instance_id: "pack-1-card-1",
            name: "First Pull",
            set_code: "TST",
            collector_number: "101",
            rarity: "common",
            colors: ["W"],
            cmc: 1,
            type_line: "Creature — Test",
          }],
          [{
            instance_id: "pack-2-card-1",
            name: "Second Pull",
            set_code: "TST",
            collector_number: "102",
            rarity: "uncommon",
            colors: ["U"],
            cmc: 2,
            type_line: "Instant",
          }],
        ],
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
      if (decoded.type === "draft_state_update") {
        expect(decoded.view.sealed_packs).toEqual(longView.sealed_packs);
        expect(decoded.view.pool_groups).toEqual(longView.pool_groups);
      }
    });

    it("round-trips a deck-carrying draft match start message", async () => {
      const deck = {
        main_deck: ["Island"],
        sideboard: [],
        commander: [],
      };
      const msg: DraftP2PMessage = {
        type: "draft_match_start",
        launch: {
          type: "Bot",
          matchId: "round-1-table-1",
          round: 1,
          localSeat: 0,
          botSeat: 1,
          botName: "Bot 2",
          deckPayload: {
            player: deck,
            opponent: { main_deck: ["Mountain"], sideboard: [], commander: [] },
            ai_decks: [],
          },
          matchConfig: { match_type: "Bo1" },
          binding: {
            podId: "draft-1",
            matchId: "round-1-table-1",
            round: 1,
            sessionKey: "session-1",
            lease: "lease-1",
            nonce: "nonce-1",
            revision: 0,
            matchAuthoritySeat: 0,
          },
        },
      };

      const decoded = await decodeDraftWireMessage(await encodeDraftWireMessage(msg));
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
