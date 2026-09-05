import { describe, it, expect } from "vitest";

import type { EndContinuousEffectOffer } from "../../adapter/types";
import { buildGameState } from "../../test/factories/gameStateFactory";
import {
  WIRE_PROTOCOL_VERSION,
  decodeWireMessage,
  encodeWireMessage,
  legalActionsFromWire,
  legalActionsToWire,
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

/**
 * An allocation whose segments are UNEQUAL and whose `choice_id` order is NOT
 * the candidate publication order, so a sort or a canonicalisation anywhere on
 * the wire path is caught rather than coinciding with the input.
 */
export const HOSTILE_PREVIEW_REQUEST = {
  requestId: "preview-req-1",
  interactionId: "interaction-1",
  response: {
    type: "shortcut",
    data: {
      decision: { type: "fixed", data: { iterations: 6 } },
      pins: [{
        group: 0,
        choiceIds: ["choice-c", "choice-a", "choice-b"],
        amounts: [
          { choiceId: "choice-c", amount: 3 },
          { choiceId: "choice-a", amount: 1 },
          { choiceId: "choice-b", amount: 2 },
        ],
      }],
    },
  },
} as never;

const PREVIEW_ANSWER = {
  requestId: "preview-req-1",
  interactionId: "interaction-1",
  status: { type: "confirmable" },
  progress: { selected: 3, minimum: 1, maximum: 3, aggregate: 6, confirmable: true },
  outcome: "advanced",
  summaries: ["confirmAvailable", "progress"],
} as never;

describe("encodeWireMessage / decodeWireMessage", () => {
  it("pins the P2P wire protocol to v47", () => {
    expect(WIRE_PROTOCOL_VERSION).toBe(47);
  });

  it("defaults shortcut actions for a legacy payload created before the additive field", () => {
    expect(legalActionsFromWire({ legalActions: [] }).manaPaymentShortcutActions).toEqual([]);
  });

  // CR 118.3 — matrix row 20: the acting-player "can't pay this cost right now"
  // read-out must survive the P2P host->guest wire. Omit it from either
  // projection helper and a P2P guest silently loses the read-out; because the
  // field is OPTIONAL, `type-check` stays green, so this round-trip is the only
  // instrument that catches it.
  it("round-trips the CR 118.3 activation-block read-out host->guest", () => {
    const activationBlockReasons = {
      "408": [
        { ability_index: 0, type: "CostNotPayableNow" as const },
        { ability_index: 1, type: "CostNotPayableNow" as const },
      ],
    };

    const wire = legalActionsToWire({
      actions: [],
      autoPassRecommended: false,
      activationBlockReasons,
    });
    // Guard the HOST half separately: a `legalActionsToWire` that dropped the
    // field would still round-trip to `undefined` below and could be read as an
    // absent-field pass.
    expect(wire.activationBlockReasons).toEqual(activationBlockReasons);

    expect(legalActionsFromWire(wire).activationBlockReasons).toEqual(activationBlockReasons);
  });

  // The absent direction: a v45 peer sends no field at all. It must hydrate to
  // `undefined` (the store applies its own `?? {}` default) rather than throwing.
  it("hydrates an absent activation-block read-out without crashing", () => {
    expect(legalActionsFromWire({ legalActions: [] }).activationBlockReasons).toBeUndefined();
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
    { type: "reconnect", playerToken: "token-123", wireProtocolVersion: WIRE_PROTOCOL_VERSION },
    { type: "state_ack", revision: 17 },
    { type: "reconnect_rejected", reason: "Unknown token" },
    {
      type: "action_rejected",
      rejection: {
        code: "action_not_allowed",
        disposition: "unavailable",
        message: "Player kicked",
        related_object_ids: [],
      },
    },
    { type: "action_failed", message: "Host failed to submit action" },
    { type: "action_noop" },
    { type: "mana_payment_preview", requestId: 4, sourceIds: [12] },
    {
      type: "mana_payment_preview_rejected",
      requestId: 4,
      rejection: {
        code: "not_your_priority",
        disposition: "unavailable",
        message: "Not your turn",
        related_object_ids: [],
      },
    },
    { type: "mana_payment_preview_failed", requestId: 4, message: "Preview unavailable" },
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
      type: "preview_interaction",
      request: HOSTILE_PREVIEW_REQUEST,
    },
    {
      type: "interaction_preview",
      requestId: "preview-req-1",
      answer: { type: "preview", preview: PREVIEW_ANSWER },
    },
    {
      type: "interaction_preview",
      requestId: "preview-req-1",
      answer: { type: "failed", message: "Game paused" },
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

  /**
   * Row 6's paired demonstration that the allowlist is load-bearing: the two
   * new types are accepted by `validateMessage` while a neighbouring
   * near-miss name is not, so the round-trips above are not passing through a
   * validator that accepts everything.
   */
  it("gates the two new preview types on VALID_TYPES", () => {
    expect(validateMessage({ type: "preview_interaction", request: HOSTILE_PREVIEW_REQUEST }))
      .toMatchObject({ type: "preview_interaction" });
    expect(validateMessage({
      type: "interaction_preview",
      requestId: "preview-req-1",
      answer: { type: "failed", message: "x" },
    })).toMatchObject({ type: "interaction_preview" });
    expect(() => validateMessage({ type: "preview_interactions" }))
      .toThrow(/Invalid message type/);
    expect(() => validateMessage({ type: "interaction_previewed" }))
      .toThrow(/Invalid message type/);
  });

  /**
   * Row 7, P2P half, at the real wire format: a decoder that does not know the
   * tag throws rather than passing it through. The KNOWN tag on the identical
   * path in the same test is the positive control — without it this could be a
   * decoder that rejects everything.
   */
  it("refuses an unknown preview-shaped tag while the known one decodes", async () => {
    const known: P2PMessage = { type: "preview_interaction", request: HOSTILE_PREVIEW_REQUEST };
    await expect(decodeWireMessage(await encodeWireMessage(known))).resolves.toEqual(known);

    const unknown = { type: "preview_interaction_v2", request: HOSTILE_PREVIEW_REQUEST };
    const json = new TextEncoder().encode(JSON.stringify(unknown));
    const bytes = new Uint8Array(1 + json.length);
    bytes[0] = 0x00;
    bytes.set(json, 1);
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
