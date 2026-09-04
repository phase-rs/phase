import { describe, expect, it } from "vitest";

import {
  DRAFT_PROTOCOL_VERSION,
  deckSubmissionFingerprint,
  encodeDraftWireMessage,
  decodeDraftWireMessage,
  validateDraftMessage,
} from "../draftProtocol";
import type { DraftP2PMessage } from "../draftProtocol";
import { MAX_DRAFT_WORKSPACE_NETWORK_PLACEMENTS } from "../../components/draft/workspace/types";

const validWorkspace = {
  schemaVersion: 1 as const,
  placements: {
    "pool-1": { zone: "deck" as const, row: 0, column: 2, order: 0 },
    "basic-1": { zone: "sideboard" as const, row: 1, column: 3, order: 1 },
  },
  virtualBasics: [{ instanceId: "basic-1", name: "Island" }],
};

const validDraftView = { launch_capability: "None" as const };

function workspaceWithPlacementCount(count: number) {
  return {
    schemaVersion: 1 as const,
    placements: Object.fromEntries(
      Array.from({ length: count }, (_, index) => [
        `card-${index}`,
        { zone: "deck" as const, row: 0, column: 0, order: index },
      ]),
    ),
    virtualBasics: [],
  };
}

describe("draftProtocol", () => {
  it("uses a locale-independent multiset fingerprint for deck submissions", () => {
    expect(deckSubmissionFingerprint(["Ångler", "Island", "Island"])).toBe(
      deckSubmissionFingerprint(["Island", "Ångler", "Island"]),
    );
  });

  describe("DRAFT_PROTOCOL_VERSION", () => {
    it("is version 25", () => {
      expect(DRAFT_PROTOCOL_VERSION).toBe(25);
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
          ...validDraftView,
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
      }) as { view: { pool_groups: {
        rarity_groups: unknown[];
        type_groups: Array<{ cards: Array<{ instance_ids: string[] }> }>;
        workspace_capabilities: { rarity_group_order: unknown };
        workspace_row_classification: {
          creature_instance_ids: unknown[];
          noncreature_instance_ids: unknown[];
        };
      } } };

      expect(msg.view.pool_groups.rarity_groups).toEqual([]);
      const upgraded = msg.view.pool_groups as unknown as {
        type_filter_options: unknown[];
        color_filter_options: unknown[];
      };
      expect(upgraded.type_filter_options).toEqual([]);
      expect(upgraded.color_filter_options).toEqual([]);
      expect(msg.view.pool_groups.type_groups[0].cards[0].instance_ids).toEqual(["adept-1"]);
      expect(msg.view.pool_groups.workspace_capabilities.rarity_group_order).toBeNull();
      expect(msg.view.pool_groups.workspace_row_classification.creature_instance_ids).toEqual([]);
      expect(msg.view.pool_groups.workspace_row_classification.noncreature_instance_ids).toEqual([]);
    });

    it("passes a v11 view through unchanged", () => {
      const entry = { card, count: 2, instance_ids: ["adept-1", "adept-2"] };
      const msg = validateDraftMessage({
        type: "draft_state_update",
        view: {
          ...validDraftView,
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
            workspace_capabilities: {
              rarity_group_order: ["mythic", "rare", "uncommon", "common", "rarity_other"],
            },
            workspace_row_classification: {
              creature_instance_ids: ["adept-1", "adept-2"],
              noncreature_instance_ids: [],
            },
          },
        },
      }) as { view: { pool_groups: {
        rarity_groups: Array<{ cards: Array<{ instance_ids: string[] }> }>;
        type_groups: Array<{ cards: Array<{ instance_ids: string[] }> }>;
        workspace_capabilities: { rarity_group_order: string[] };
        workspace_row_classification: {
          creature_instance_ids: string[];
          noncreature_instance_ids: string[];
        };
      } } };

      expect(msg.view.pool_groups.type_groups[0].cards[0].instance_ids).toEqual([
        "adept-1",
        "adept-2",
      ]);
      expect(msg.view.pool_groups.rarity_groups[0].cards[0].instance_ids).toEqual([
        "adept-1",
        "adept-2",
      ]);
      expect(msg.view.pool_groups.workspace_capabilities.rarity_group_order).toEqual([
        "mythic",
        "rare",
        "uncommon",
        "common",
        "rarity_other",
      ]);
      expect(msg.view.pool_groups.workspace_row_classification.creature_instance_ids).toEqual([
        "adept-1",
        "adept-2",
      ]);
    });

    const validateNestedMetadata = (
      workspace_capabilities: unknown,
      workspace_row_classification: unknown,
    ) => validateDraftMessage({
      type: "draft_state_update",
      view: {
        ...validDraftView,
        draft_effects: [],
        seats: [],
        pool_groups: {
          color_groups: [],
          type_groups: [],
          cmc_groups: [],
          rarity_groups: [],
          workspace_capabilities,
          workspace_row_classification,
          color_counts: { white: 0, blue: 0, black: 0, red: 0, green: 0 },
        },
      },
    });

    it("preserves valid empty nested metadata", () => {
      const msg = validateNestedMetadata(
        { rarity_group_order: null },
        { creature_instance_ids: [], noncreature_instance_ids: [] },
      );
      expect(msg).toMatchObject({
        view: {
          pool_groups: {
            workspace_capabilities: { rarity_group_order: null },
            workspace_row_classification: {
              creature_instance_ids: [],
              noncreature_instance_ids: [],
            },
          },
        },
      });
    });

    it.each(["workspace_capabilities", "workspace_row_classification"] as const)(
      "defaults an independently absent legacy %s outer field",
      (field) => {
        const poolGroups = {
          color_groups: [],
          type_groups: [],
          cmc_groups: [],
          rarity_groups: [],
          workspace_capabilities: { rarity_group_order: null },
          workspace_row_classification: {
            creature_instance_ids: [],
            noncreature_instance_ids: [],
          },
          color_counts: { white: 0, blue: 0, black: 0, red: 0, green: 0 },
        };
        delete poolGroups[field];

        const msg = validateDraftMessage({
          type: "draft_state_update",
          view: { ...validDraftView, draft_effects: [], seats: [], pool_groups: poolGroups },
        });
        expect(msg).toMatchObject({
          view: {
            pool_groups: {
              workspace_capabilities: { rarity_group_order: null },
              workspace_row_classification: {
                creature_instance_ids: [],
                noncreature_instance_ids: [],
              },
            },
          },
        });
      },
    );

    it.each([
      ["null capabilities", null],
      ["scalar capabilities", "invalid"],
      ["array capabilities", []],
      ["empty capabilities", {}],
      ["missing rarity order", { other: [] }],
      ["non-array rarity order", { rarity_group_order: "common" }],
      ["invalid rarity kind", { rarity_group_order: ["legendary"] }],
      ["non-rarity group kind", { rarity_group_order: ["creature"] }],
      ["non-string rarity kind", { rarity_group_order: [1] }],
    ])("rejects %s", (_label, capabilities) => {
      expect(() => validateNestedMetadata(
        capabilities,
        { creature_instance_ids: [], noncreature_instance_ids: [] },
      )).toThrow();
    });

    it.each([
      ["null row classification", null],
      ["scalar row classification", "invalid"],
      ["array row classification", []],
      ["empty row classification", {}],
      ["missing creature ids", { noncreature_instance_ids: [] }],
      ["missing noncreature ids", { creature_instance_ids: [] }],
      ["non-array creature ids", { creature_instance_ids: "a", noncreature_instance_ids: [] }],
      ["non-array noncreature ids", { creature_instance_ids: [], noncreature_instance_ids: "a" }],
      ["non-string creature id", { creature_instance_ids: [1], noncreature_instance_ids: [] }],
      ["non-string noncreature id", { creature_instance_ids: [], noncreature_instance_ids: [1] }],
    ])("rejects %s", (_label, rows) => {
      expect(() => validateNestedMetadata(
        { rarity_group_order: null },
        rows,
      )).toThrow();
    });
  });

  describe("validateDraftMessage", () => {
    it("accepts only versioned, token-bound draft leave messages", () => {
      expect(validateDraftMessage({
        type: "draft_leave",
        draftProtocolVersion: DRAFT_PROTOCOL_VERSION,
        draftToken: "seat-token",
      })).toMatchObject({ type: "draft_leave", draftToken: "seat-token" });
      expect(validateDraftMessage({
        type: "draft_leave_ack",
        draftProtocolVersion: DRAFT_PROTOCOL_VERSION,
        draftToken: "seat-token",
      })).toMatchObject({ type: "draft_leave_ack", draftToken: "seat-token" });
      expect(() => validateDraftMessage({
        type: "draft_leave",
        draftProtocolVersion: DRAFT_PROTOCOL_VERSION - 1,
        draftToken: "seat-token",
      })).toThrow("Invalid draft leave message");
    });

    it("accepts valid draft_join message", () => {
      const msg = validateDraftMessage({
        type: "draft_join",
        displayName: "Alice",
        draftProtocolVersion: DRAFT_PROTOCOL_VERSION,
      });
      expect(msg.type).toBe("draft_join");
    });

    it("accepts valid draft_pick message", () => {
      const msg = validateDraftMessage({ type: "draft_pick", cardInstanceIds: ["card-001"] });
      expect(msg).toMatchObject({ type: "draft_pick", cardInstanceIds: ["card-001"] });
    });

    // CR 903.13b: a Commander pod's pick step is two cards, and an odd pack's
    // final step is one. The wire bound is therefore a RANGE — a `=== 2` check
    // would reject every CR 905.1a kind, and a `=== 1` check every Commander
    // step.
    it("accepts a two-card draft_pick step", () => {
      const msg = validateDraftMessage({
        type: "draft_pick",
        cardInstanceIds: ["card-001", "card-002"],
      });
      expect(msg).toMatchObject({
        type: "draft_pick",
        cardInstanceIds: ["card-001", "card-002"],
      });
    });

    it.each([
      { cardInstanceIds: undefined },
      { cardInstanceIds: null },
      { cardInstanceIds: {} },
      { cardInstanceIds: "card-001" },
      { cardInstanceIds: [] },
      { cardInstanceIds: ["card-001", "card-002", "card-003"] },
      { cardInstanceIds: ["card-001", "card-001"] },
      { cardInstanceIds: [null] },
      { cardInstanceIds: ["card-001", 7] },
      { cardInstanceIds: [""] },
      { cardInstanceIds: ["x".repeat(257)] },
    ])("rejects malformed draft_pick payloads", (payload) => {
      expect(() => validateDraftMessage({ type: "draft_pick", ...payload })).toThrow(
        "Invalid draft pick",
      );
    });

    // ── draft_submit_deck: the CR 903.3 designation's wire bound ──────
    //
    // This suite is the ONLY one in client/src that runs
    // `validateDraftMessage` at all, so every claim about the new validator's
    // bound is owed here and nowhere else. The P2P host-seam suite invokes
    // `handleGuestMessage` directly and never reaches this code.

    it("accepts a deck submission carrying a designation", () => {
      const msg = validateDraftMessage({
        type: "draft_submit_deck",
        submissionId: "submission-1",
        mainDeck: ["Plains", "Island"],
        commanders: ["Kenrith, the Returned King"],
      });
      expect(msg).toMatchObject({
        type: "draft_submit_deck",
        mainDeck: ["Plains", "Island"],
        commanders: ["Kenrith, the Returned King"],
      });
    });

    // CR 702.124h designates two legendary CARDS, and CR 903.13e's filler case
    // is two copies of ONE name — so a distinctness check would wrongly refuse
    // a legal payload. Neither landed sibling's form (`[0] === [1]`, or
    // `new Set(...).size`) may be copied into `validateSubmitDeck`, and this is
    // the row that pins it.
    it("accepts two designations with the same name", () => {
      const msg = validateDraftMessage({
        type: "draft_submit_deck",
        submissionId: "submission-1",
        mainDeck: ["The Prismatic Piper", "The Prismatic Piper"],
        commanders: ["The Prismatic Piper", "The Prismatic Piper"],
      });
      expect(msg).toMatchObject({
        commanders: ["The Prismatic Piper", "The Prismatic Piper"],
      });
    });

    // THE FLOOR IS 0, and this is the only instrument in the phase that reds
    // on a floor of 1. CR 903.1 scopes the commander designation to the
    // Commander variant, and a P2P host pod is `Exclude<DraftKind, "Quick">` —
    // Premier, Traditional and Sealed all submit `commanders: []`. Copying
    // `validatePick`'s middle disjunct (`length === 0`) here would refuse every
    // one of those submissions, and `draftPeerSession`'s decode `.catch` would
    // drop the refusal silently. Assert on the RETURNED VALUE, not merely that
    // nothing threw: `[]` must be neither refused nor defaulted into a name.
    it("accepts an empty designation and returns it empty", () => {
      const msg = validateDraftMessage({
        type: "draft_submit_deck",
        submissionId: "submission-1",
        mainDeck: ["Plains", "Island"],
        commanders: [],
      });
      expect(msg).toMatchObject({ type: "draft_submit_deck", commanders: [] });
    });

    it.each([
      { commanders: undefined },
      { commanders: null },
      { commanders: {} },
      { commanders: "Kenrith, the Returned King" },
      // Over the bound of 2 (CR 702.124g). Written as a literal three-name
      // array, the way this file's landed `draft_pick` sweep writes every
      // over-bound payload: the bound is module-private in `draftProtocol.ts`
      // and this suite imports no constant. If the bound ever moves, this row
      // goes stale LOUDLY — a third name becomes legal and `toThrow` finds no
      // throw.
      { commanders: ["Kenrith", "Gyruda", "Ludevic"] },
      { commanders: [null] },
      { commanders: ["Kenrith", 7] },
      { commanders: [""] },
      { commanders: ["x".repeat(257)] },
    ])("rejects malformed draft_submit_deck payloads", (payload) => {
      expect(() =>
        validateDraftMessage({
          type: "draft_submit_deck",
          submissionId: "submission-1",
          mainDeck: ["Plains"],
          ...payload,
        }),
      ).toThrow("Invalid deck submission: commanders");
    });

    // v14's `submissionId` and v17's `commanders` are independent required
    // fields on this one message. Each half carries the OTHER field valid, so
    // neither refusal can be satisfied by the other's guard firing first.
    it("requires a stable identifier on a deck submission", () => {
      expect(validateDraftMessage({
        type: "draft_submit_deck",
        submissionId: "submission-1",
        mainDeck: ["Island"],
        commanders: [],
      })).toMatchObject({ type: "draft_submit_deck", submissionId: "submission-1" });
      expect(() => validateDraftMessage({
        type: "draft_submit_deck",
        mainDeck: ["Island"],
        commanders: [],
      })).toThrow("Invalid deck submission: submissionId");
    });

    it("rejects a malformed deck acknowledgement before it can clear an outbox", () => {
      expect(() => validateDraftMessage({
        type: "draft_deck_submit_ack",
        submissionId: "submission-1",
      })).toThrow("Invalid draft deck acknowledgement");
      expect(() => validateDraftMessage({
        type: "draft_deck_submit_ack",
        view: validDraftView,
      })).toThrow("Invalid deck acknowledgement");
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
        draftProtocolVersion: DRAFT_PROTOCOL_VERSION,
        draftToken: "token-123",
        seatIndex: 3,
        view: validDraftView,
        draftCode: "draft-abc",
        workspaceState: validWorkspace,
      });
      expect(msg.type).toBe("draft_welcome");
    });

    it.each([undefined, null, "Commander", "Unknown"])(
      "rejects a missing or unknown launch capability at protocol v25",
      (launch_capability) => {
        expect(() => validateDraftMessage({
          type: "draft_state_update",
          view: { launch_capability },
        })).toThrow("launch_capability must be a known capability");
      },
    );

    it.each(["draft_welcome", "draft_reconnect_ack"])(
      "accepts nullable workspace state for %s",
      (type) => {
        const msg = validateDraftMessage({ type, view: validDraftView, workspaceState: null });
        expect(msg).toMatchObject({ type, workspaceState: null });
      },
    );

    it("accepts a complete workspace update without a seat field", () => {
      const msg = validateDraftMessage({
        type: "draft_workspace_update",
        workspaceState: validWorkspace,
      });
      expect(msg).toEqual({ type: "draft_workspace_update", workspaceState: validWorkspace });
      expect(msg).not.toHaveProperty("seatIndex");
    });

    it("accepts the network placement limit and rejects one more before reading placement values", () => {
      expect(validateDraftMessage({
        type: "draft_workspace_update",
        workspaceState: workspaceWithPlacementCount(MAX_DRAFT_WORKSPACE_NETWORK_PLACEMENTS),
      }).type).toBe("draft_workspace_update");

      const placements = workspaceWithPlacementCount(
        MAX_DRAFT_WORKSPACE_NETWORK_PLACEMENTS,
      ).placements;
      Object.defineProperty(placements, "too-many", {
        enumerable: true,
        get: () => {
          throw new Error("placement value was read");
        },
      });

      expect(() => validateDraftMessage({
        type: "draft_workspace_update",
        workspaceState: { schemaVersion: 1, placements, virtualBasics: [] },
      })).toThrow(`placements cannot exceed ${MAX_DRAFT_WORKSPACE_NETWORK_PLACEMENTS} entries`);
    });

    it.each(["seat", "seatIndex"])("rejects caller-supplied %s authority", (field) => {
      expect(() => validateDraftMessage({
        type: "draft_workspace_update",
        workspaceState: validWorkspace,
        [field]: 4,
      })).toThrow("must not include a seat");
    });

    it.each(["draft_welcome", "draft_reconnect_ack", "draft_workspace_update"])(
      "rejects missing workspace state for %s",
      (type) => {
        expect(() => validateDraftMessage({ type, view: validDraftView })).toThrow("missing workspaceState");
      },
    );

    it("rejects null workspace updates while accepting a valid update", () => {
      expect(validateDraftMessage({
        type: "draft_workspace_update",
        workspaceState: validWorkspace,
      }).type).toBe("draft_workspace_update");
      expect(() => validateDraftMessage({
        type: "draft_workspace_update",
        workspaceState: null,
      })).toThrow("workspace state must be a plain object");
    });

    const malformedWorkspaces = [
      ["row outside the workspace", {
        ...validWorkspace,
        placements: { "pool-1": { zone: "deck", row: 2, column: 0, order: 0 } },
      }],
      ["duplicate virtual ids", {
        ...validWorkspace,
        virtualBasics: [
          { instanceId: "basic-1", name: "Island" },
          { instanceId: "basic-1", name: "Plains" },
        ],
      }],
    ] as const;

    it.each(["draft_welcome", "draft_reconnect_ack", "draft_workspace_update"] as const)(
      "validates complete snapshots for %s",
      (type) => {
        expect(validateDraftMessage({ type, view: validDraftView, workspaceState: validWorkspace }))
          .toMatchObject({ type, workspaceState: validWorkspace });
        for (const [, workspaceState] of malformedWorkspaces) {
          expect(() => validateDraftMessage({ type, view: validDraftView, workspaceState }))
            .toThrow("Invalid draft message");
        }
      },
    );

    it("normalizes missing face-up draft arrays while preserving active-pack presence", () => {
      const msg = validateDraftMessage({
        type: "draft_state_update",
        view: {
          ...validDraftView,
          seats: [{ seat_index: 1, display_name: "Alex", active_pack_count: 1 }],
        },
      });

      expect(msg.type).toBe("draft_state_update");
      if (msg.type === "draft_state_update") {
        expect(msg.view.draft_effects).toEqual([]);
        expect(msg.view.seats[0].active_pack_count).toBe(1);
        expect(msg.view.seats[0].face_up_draft_cards).toEqual([]);
      }
    });

    it("projects a Chaos source view without retaining a host assignment matrix", () => {
      const msg = validateDraftMessage({
        type: "draft_state_update",
        view: {
          ...validDraftView,
          source: {
            type: "Set",
            data: {
              layout: {
                Chaos: {
                  candidate_codes: ["AAA", "BBB"],
                  current_pack_code: "BBB",
                  completed_own_pack_codes: null,
                  actual_set_codes: null,
                  assignments: [["AAA", "BBB"]],
                },
              },
            },
          },
        },
      });

      expect(msg.type).toBe("draft_state_update");
      if (msg.type === "draft_state_update") {
        expect(msg.view.source).toEqual({
          type: "Set",
          data: {
            layout: {
              Chaos: {
                candidate_codes: ["AAA", "BBB"],
                current_pack_code: "BBB",
                completed_own_pack_codes: null,
                actual_set_codes: null,
              },
            },
          },
        });
        expect(JSON.stringify(msg.view.source)).not.toContain("assignments");
      }
    });

    it.each([undefined, null, "1", 0.5, -1, 2])(
      "rejects invalid active-pack presence %j",
      (activePackCount) => {
        expect(() => validateDraftMessage({
          type: "draft_state_update",
          view: {
            ...validDraftView,
            seats: [{ active_pack_count: activePackCount }],
          },
        })).toThrow("active_pack_count must be an integer 0 or 1");
      },
    );

    it.each([0, 1])("accepts active-pack presence %i", (activePackCount) => {
      const msg = validateDraftMessage({
        type: "draft_lobby_update",
        seats: [{ active_pack_count: activePackCount }],
      });

      expect(msg).toMatchObject({
        seats: [{ active_pack_count: activePackCount }],
      });
    });

    it("requires active-pack presence in lobby seats", () => {
      expect(() => validateDraftMessage({
        type: "draft_lobby_update",
        seats: [{}],
      })).toThrow("active_pack_count must be an integer 0 or 1");
    });

    it.each([null, {}])("rejects present non-array draft_effects values", (draftEffects) => {
      expect(() => validateDraftMessage({
        type: "draft_state_update",
        view: { ...validDraftView, draft_effects: draftEffects, seats: [] },
      })).toThrow("draft_effects must be an array");
    });

    it.each([null, {}])("rejects present non-array seats values", (seats) => {
      expect(() => validateDraftMessage({
        type: "draft_state_update",
        view: { ...validDraftView, draft_effects: [], seats },
      })).toThrow("seats must be an array");
    });

    it.each([null, {}])("rejects present non-array face-up draft cards", (faceUpCards) => {
      expect(() => validateDraftMessage({
        type: "draft_state_update",
        view: {
          ...validDraftView,
          draft_effects: [],
          seats: [{ active_pack_count: 0, face_up_draft_cards: faceUpCards }],
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

    it("rejects a malformed typed reconnect rejection", () => {
      expect(() => validateDraftMessage({
        type: "draft_reconnect_rejected",
        kind: "NotARejectionKind",
        reason: "Unknown token",
      })).toThrow("Invalid draft reconnect rejection");
      expect(validateDraftMessage({
        type: "draft_reconnect_rejected",
        kind: "UnknownToken",
        reason: "Unknown token",
      })).toMatchObject({ kind: "UnknownToken" });
    });

    it("normalizes a pre-v13 untyped reconnect rejection to a credential-preserving protocol mismatch", () => {
      expect(validateDraftMessage({
        type: "draft_reconnect_rejected",
        reason: "Unknown token",
      })).toMatchObject({
        type: "draft_reconnect_rejected",
        kind: "ProtocolMismatch",
        reason: "Unknown token",
      });
    });

    it.each([
      "draft_join",
      "draft_reconnect",
      "draft_pick",
      "draft_pick_with_draft_effect",
      "draft_submit_deck",
      "draft_workspace_update",
      "draft_welcome",
      "draft_reconnect_ack",
      "draft_reconnect_rejected",
      "draft_state_update",
      "draft_deck_submit_ack",
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
      const bespokePayloads: Record<string, Record<string, unknown>> = {
        draft_pick: { cardInstanceIds: ["card-001"] },
        draft_pick_with_draft_effect: {
          effectCardInstanceId: "cogwork-1",
          cardInstanceIds: ["card-001", "card-002"],
        },
        draft_submit_deck: {
          submissionId: "submission-1",
          mainDeck: ["Plains"],
          commanders: ["Kenrith, the Returned King"],
        },
        draft_reconnect_rejected: { kind: "NoReconnectWindow", reason: "No grace window" },
        draft_deck_submit_ack: { submissionId: "submission-1", view: validDraftView },
      };
      const msg = validateDraftMessage(
        msgType === "draft_workspace_update"
            ? { type: msgType, workspaceState: validWorkspace }
            : msgType === "draft_welcome" || msgType === "draft_reconnect_ack"
              ? { type: msgType, view: validDraftView, workspaceState: null }
              : msgType === "draft_state_update" || msgType === "draft_pick_ack"
                ? { type: msgType, view: validDraftView }
              : { type: msgType, ...bespokePayloads[msgType] },
      );
      expect(msg.type).toBe(msgType);
    });
  });

  describe("wire encoding/decoding round-trip", () => {
    it.each([
      ["raw", { schemaVersion: 1 as const, placements: {}, virtualBasics: [] }],
      ["gzip", validWorkspace],
    ])("round-trips a non-null workspace update on the %s path", async (format, workspaceState) => {
      const expandedState = format === "gzip"
        ? {
            ...workspaceState,
            virtualBasics: Array.from({ length: 20 }, (_, index) => ({
              instanceId: `basic-${index}`,
              name: `Basic land ${index}`,
            })),
          }
        : workspaceState;
      const msg: DraftP2PMessage = {
        type: "draft_workspace_update",
        workspaceState: expandedState,
      };
      const encoded = await encodeDraftWireMessage(msg);
      expect(encoded[0]).toBe(format === "raw" ? 0x00 : 0x01);
      expect(await decodeDraftWireMessage(encoded)).toEqual(msg);
    });

    it("round-trips a small message (raw path)", async () => {
      const msg: DraftP2PMessage = {
        type: "draft_join",
        displayName: "Bob",
        draftProtocolVersion: DRAFT_PROTOCOL_VERSION,
      };
      const encoded = await encodeDraftWireMessage(msg);
      // Small messages use raw format (0x00 prefix)
      expect(encoded[0]).toBe(0x00);

      const decoded = await decodeDraftWireMessage(encoded);
      expect(decoded).toEqual(msg);
    });

    it("rejects an oversized workspace update decoded from the wire", async () => {
      const encoded = await encodeDraftWireMessage({
        type: "draft_workspace_update",
        workspaceState: workspaceWithPlacementCount(MAX_DRAFT_WORKSPACE_NETWORK_PLACEMENTS + 1),
      });

      await expect(decodeDraftWireMessage(encoded)).rejects
        .toThrow(`placements cannot exceed ${MAX_DRAFT_WORKSPACE_NETWORK_PLACEMENTS} entries`);
    });

    it("round-trips a deck submission carrying its designation", async () => {
      // `decodeDraftWireMessage` runs `validateDraftMessage`, so this covers
      // the production path a guest's deck submission actually takes
      // (CR 903.3).
      const msg: DraftP2PMessage = {
        type: "draft_submit_deck",
        submissionId: "submission-1",
        mainDeck: ["Plains", "Island"],
        commanders: ["Kenrith, the Returned King"],
      };
      const decoded = await decodeDraftWireMessage(await encodeDraftWireMessage(msg));
      expect(decoded).toEqual(msg);
    });

    it("round-trips a whole two-card pick step through the validator", async () => {
      // `decodeDraftWireMessage` runs `validateDraftMessage`, so this covers
      // the production path a guest's pick actually takes (CR 903.13b).
      const msg: DraftP2PMessage = {
        type: "draft_pick",
        cardInstanceIds: ["card-001", "card-002"],
      };
      const decoded = await decodeDraftWireMessage(await encodeDraftWireMessage(msg));
      expect(decoded).toEqual(msg);
    });

    it("round-trips a large message (gzip path)", async () => {
      // Build a message large enough to trigger compression
      const longView = {
        launch_capability: "None" as const,
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
          workspace_capabilities: {
            rarity_group_order: ["mythic", "rare", "uncommon", "common", "rarity_other"],
          },
          workspace_row_classification: {
            creature_instance_ids: ["pack-1-card-1"],
            noncreature_instance_ids: ["pack-2-card-1"],
          },
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
