import { describe, it, expect } from "vitest";

import type { EndContinuousEffectOffer } from "../../adapter/types";
import { buildGameState } from "../../test/factories/gameStateFactory";
import {
  WIRE_PROTOCOL_VERSION,
  decodeWireMessage,
  encodeWireMessage,
  legalActionsFromWire,
  validateMessage,
} from "../protocol";
import type { P2PMessage } from "../protocol";

const viewerInteractionWithProducedMana = {
  waitingForKind: { simultaneous: null, terminal: false, code: "choose" },
  authorizedSubmitters: [1],
  canSubmit: true,
  autoPassRecommended: false,
  opportunities: [{
    interactionId: "interaction-1",
    response: {
      type: "exactChoices",
      data: { choices: [{
        id: "choice-1",
        status: { type: "available" },
        surfaces: [
          { type: "action", data: { code: "tapLandForMana", actionId: "action-1" } },
          { type: "mana", data: { role: "producedMana", index: 0, symbols: ["G"], restrictions: [] } },
        ],
      }] },
    },
    surfaces: [],
    progress: { selected: 0, minimum: 1, maximum: 1, aggregate: null, confirmable: false },
  }],
  availability: { type: "inputRequired" },
} as never;

describe("encodeWireMessage / decodeWireMessage", () => {
  it("pins the P2P wire protocol to v24", () => {
    expect(WIRE_PROTOCOL_VERSION).toBe(24);
  });

  it("defaults shortcut actions for a legacy payload created before the additive field", () => {
    expect(legalActionsFromWire({ legalActions: [] }).manaPaymentShortcutActions).toEqual([]);
  });

  it("preserves the engine-authored pay-to-end offer order and display payload", () => {
    const first: EndContinuousEffectOffer = {
      type: "EndContinuousEffect",
      data: {
        group: 8,
        source_name: "Calming Licid",
        cost: { type: "Cost", shards: ["W"], generic: 0 },
      },
    };
    const second: EndContinuousEffectOffer = {
      type: "EndContinuousEffect",
      data: {
        group: 13,
        source_name: "Convulsing Licid",
        cost: { type: "Cost", shards: ["R"], generic: 0 },
      },
    };

    expect(
      legalActionsFromWire({
        legalActions: [first, second],
        endContinuousEffectOffers: [second, first],
      }).endContinuousEffectOffers,
    ).toEqual([second, first]);
  });

  // (a) Round-trip across P2PMessage variants.
  const variants: P2PMessage[] = [
    { type: "ping", timestamp: 12345 },
    { type: "pong", timestamp: 12345 },
    { type: "concede" },
    { type: "match_concede" },
    { type: "disconnect", reason: "Page closed" },
    { type: "kick", reason: "Removed" },
    { type: "host_left", reason: "Host left" },
    { type: "player_kicked", playerId: 2, reason: "Removed" },
    { type: "player_conceded", playerId: 1, reason: "Conceded" },
    { type: "player_disconnected", playerId: 1 },
    { type: "player_reconnected", playerId: 1 },
    { type: "game_paused", reason: "Player disconnected" },
    { type: "game_resumed" },
    { type: "lobby_progress", joined: 1, total: 3 },
    { type: "emote", emote: "🔥" },
    { type: "reconnect", playerToken: "token-123" },
    { type: "reconnect_rejected", reason: "Unknown token" },
    { type: "action_rejected", reason: "Player kicked" },
    { type: "action_noop" },
    { type: "mana_payment_preview", requestId: 4, sourceIds: [12] },
    { type: "mana_payment_preview_rejected", requestId: 4, reason: "Not your turn" },
    {
      type: "action",
      senderPlayerId: 0,
      action: { type: "PassPriority" },
    },
    {
      type: "action",
      senderPlayerId: 0,
      action: {
        type: "SetPriorityPassingMode",
        data: { mode: "SkipLowUseWindows" },
      },
    },
    {
      type: "action",
      senderPlayerId: 0,
      action: { type: "TapForConvoke", data: { object_id: 42, mana_type: "Green" } },
    },
    {
      type: "preview_mana_payment",
      requestId: 4,
      action: { type: "PassPriority" },
    },
    {
      type: "action",
      senderPlayerId: 0,
      action: { type: "ChooseMeldPair", data: { source_id: 42, partner_id: 43 } },
    },
    {
      type: "action",
      senderPlayerId: 0,
      action: {
        type: "ChooseEntryAttackTarget",
        data: { target: { type: "Battle", data: 44 } },
      },
    },
    {
      type: "game_setup",
      wireProtocolVersion: WIRE_PROTOCOL_VERSION,
      assignedPlayerId: 1,
      playerToken: "token-123",
      state: buildGameState({
        priority_passing_modes: { 1: "SkipLowUseWindows" },
        derived: {
          planechase: {
            can_roll: true,
            current_roll_cost: { type: "NoCost" },
            planar_deck_count: 1,
          },
        },
      }),
      events: [],
      legalActions: [{ type: "RollPlanarDie" }],
      manaPaymentShortcutActions: [],
      viewerInteraction: viewerInteractionWithProducedMana,
    },
    {
      type: "state_update",
      state: buildGameState(),
      events: [],
      legalActions: [],
      manaPaymentShortcutActions: [],
      viewerInteraction: viewerInteractionWithProducedMana,
    },
    {
      type: "reconnect_ack",
      wireProtocolVersion: WIRE_PROTOCOL_VERSION,
      assignedPlayerId: 1,
      state: buildGameState({
        derived: {
          planechase: {
            active_plane: 7,
            can_roll: false,
            current_roll_cost: { type: "NoCost" },
            planar_deck_count: 1,
          },
        },
      }),
      legalActions: [{ type: "RollPlanarDie" }],
      manaPaymentShortcutActions: [],
      viewerInteraction: viewerInteractionWithProducedMana,
    },
  ];

  it.each(variants)("round-trips %j", async (msg) => {
    const bytes = await encodeWireMessage(msg);
    const out = await decodeWireMessage(bytes);
    expect(out).toEqual(msg);
  });

  it("round-trips monarch-bounded exile links", async () => {
    const msg: P2PMessage = {
      type: "state_update",
      state: buildGameState({
        exile_links: [
          {
            exiled_id: 12,
            source_id: 34,
            kind: {
              UntilOpponentBecomesMonarch: {
                return_zone: "Battlefield",
                controller: 0,
              },
            },
          },
        ],
      }),
      events: [],
      legalActions: [],
      manaPaymentShortcutActions: [],
      viewerInteraction: viewerInteractionWithProducedMana,
    };
    const bytes = await encodeWireMessage(msg);
    await expect(decodeWireMessage(bytes)).resolves.toEqual(msg);
  });

  // (b) Tiny messages take FORMAT_RAW.
  it("ping uses FORMAT_RAW (0x00) — too small for gzip to win", async () => {
    const bytes = await encodeWireMessage({ type: "ping", timestamp: 1 });
    expect(bytes[0]).toBe(0x00);
  });

  // (c) Large messages take FORMAT_GZIP and produce a smaller-than-raw payload.
  // Don't assert on a specific compression ratio — DEFLATE tuning varies.
  it("large messages take FORMAT_GZIP and shrink relative to raw JSON", async () => {
    const bigPayload = "x".repeat(2000);
    const msg = {
      type: "action",
      senderPlayerId: 0,
      action: { type: "PassPriority", padding: bigPayload },
    } as unknown as P2PMessage;
    const bytes = await encodeWireMessage(msg);
    expect(bytes[0]).toBe(0x01); // FORMAT_GZIP
    const rawSize = new TextEncoder().encode(JSON.stringify(msg)).length;
    expect(bytes.length).toBeLessThan(rawSize);
  });

  // (d) Unknown version byte rejects cleanly.
  it("rejects unknown version byte", async () => {
    const bytes = new Uint8Array([0xff, 0x01, 0x02]);
    await expect(decodeWireMessage(bytes)).rejects.toThrow(/unknown wire format/);
  });

  it("rejects empty payload", async () => {
    await expect(decodeWireMessage(new Uint8Array())).rejects.toThrow(/empty/);
  });

  const setupFrameAt = (wireProtocolVersion: number) => ({
    type: "game_setup",
    wireProtocolVersion,
    assignedPlayerId: 1,
    playerToken: "token-123",
    state: buildGameState(),
    events: [],
    legalActions: [],
    manaPaymentShortcutActions: [],
  });

  it("rejects stale setup wire protocol versions", () => {
    expect(() => validateMessage(setupFrameAt(4))).toThrow(/Wire protocol mismatch/);
  });

  // The ADJACENT-peer pairing, which the far-stale v4 row above cannot exercise:
  // 4 is refused whatever this client speaks, so that row proves the mechanism
  // and nothing about the version. Both halves here stamp LITERALS — a frame
  // built from WIRE_PROTOCOL_VERSION cannot tell a bumped client from an
  // unbumped one, which is why every other handshake fixture in the suite is
  // useless as an instrument for a bump. Revert 24 → 23 and BOTH halves red:
  // the v23 frame stops being refused, and the v24 frame stops being admitted.
  // The admitting half is the reach-guard: without it "refuses v23" is also
  // satisfied by a client that refuses everything.
  it("refuses the previous wire protocol (v23) and admits its own (v24)", () => {
    expect(() => validateMessage(setupFrameAt(23))).toThrow(/Wire protocol mismatch/);
    expect(validateMessage(setupFrameAt(24))).toMatchObject({
      type: "game_setup",
      wireProtocolVersion: 24,
    });
  });

  // (e) Compressed payload still gates through validateMessage so unknown
  // message types are rejected, not silently passed through.
  it("decode runs validateMessage — unknown type rejected", async () => {
    const fake = { type: "definitely_not_a_real_type", x: 1 };
    const json = JSON.stringify(fake);
    const stream = new Blob([new TextEncoder().encode(json)])
      .stream()
      .pipeThrough(new CompressionStream("gzip"));
    const gz = new Uint8Array(await new Response(stream).arrayBuffer());
    const bytes = new Uint8Array(1 + gz.length);
    bytes[0] = 0x01;
    bytes.set(gz, 1);
    await expect(decodeWireMessage(bytes)).rejects.toThrow(/Invalid message type/);
  });
});

describe("validateMessage", () => {
  it("accepts known types", () => {
    expect(validateMessage({ type: "concede" })).toEqual({ type: "concede" });
  });
  it("rejects missing type", () => {
    expect(() => validateMessage({ foo: "bar" })).toThrow(/missing type/);
  });
  it("rejects unknown type", () => {
    expect(() => validateMessage({ type: "nope" })).toThrow(/Invalid message type/);
  });

  it("rejects raw unbound match concessions", () => {
    expect(() => validateMessage({ type: "concede_match" })).toThrow(/Invalid message type/);
  });
});
