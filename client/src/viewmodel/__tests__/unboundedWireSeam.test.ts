/**
 * ∞-channel cross-seam pin. Both JSON files are ENGINE-EMITTED by
 * `combo_infinite_pile::real_4p_object_growth_accept_writes_infinite_pile` and
 * `kilo_live_offer_from_real_dump::kilo_accept_marks_pentad_charge_as_unbounded_display_target`,
 * each driving a REAL 4-player dump through the REAL APNAP accept. Regenerate with
 * `UPDATE_WIRE_GOLDEN=1 cargo test -p phase-engine --test integration <fn>`. Never hand-edit them.
 * Every existing client test that touches these channels hand-writes its own `derived` block, so
 * this file is the only place the engine's wire shape and the client's readers meet.
 * Both goldens are captured AFTER the accept, while a finite collapse is merely SCHEDULED — the
 * engine applies the growth at the next CR 500.5 boundary, while advancing to the shortcut's
 * ending point (CR 732.2c), and the marks stay live through that window, so the ∞ channels are
 * still populated. If the engine went back to hiding them there, both goldens would regenerate
 * empty and every assertion below would red.
 * The `unbounded_pile → Set` hop is performed here rather than by `gameStateView.ts`, because
 * driving that function would require committing a whole `GameState`; the ids, the field name and
 * the value encoding — the parts that actually differ across the language boundary — are
 * engine-authored.
 */
import { renderHook } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import type {
  DerivedViews,
  GameObject,
  ObjectId,
  ResourceAxis,
  ResourceAxisTag,
  UnboundedFamily,
} from "../../adapter/types";
import { familyOf, UNBOUNDED_FAMILY_FOR_TEST } from "../../components/hud/HudBadges";
import { useUnboundedCounterTypes } from "../../hooks/useUnboundedCounterTypes";
import { buildGameObject } from "../../test/factories/gameObjectFactory";
import { buildGameState } from "../../test/factories/gameStateFactory";
import counterWire from "../../test/fixtures/unbounded-counter-wire.json";
import declinedWire from "../../test/fixtures/unbounded-declined-wire.json";
import familyTags from "../../test/fixtures/unbounded-family-tags.json";
import tokenWire from "../../test/fixtures/unbounded-token-wire.json";
import { setGameStoreForTest } from "../../test/helpers/gameStoreHelpers";
import { groupByName } from "../battlefieldProps";

const saproling = (id: ObjectId, tapped: boolean): GameObject =>
  buildGameObject({ id, name: "Saproling", tapped, card_id: 0, controller: 0, owner: 0 });

describe("unbounded ∞ wire seam (engine-emitted goldens)", () => {
  // RESIDUAL: this closes the ID/shape half only — the TS-side `GameObject`s are factory-built, so
  // the test cannot see an engine/client group-PARTITION mismatch, and `isUnboundedPile`'s
  // `members.every(...)` (`battlefieldProps.ts`) degrades such a mismatch silently to `×N` rather
  // than failing, which is exactly the user's symptom class.

  it("emits populated ∞ channels and omits the empty ones", () => {
    // (1) reach-guard: the engine emitted a populated pile, so the group assertions below are
    // not run against an empty set.
    expect(tokenWire.unbounded_pile).toEqual([402, 403, 404, 407]);
    // (2) reach-guard + the two counter seam facts: the map key is a JSON STRING, and
    // `CounterType` serializes FLAT ("charge", not {"Generic":"charge"}). A regressed Serialize
    // would silently blank every ∞ pill.
    expect(counterWire.unbounded_counters).toEqual({ "405": ["charge"] });
    // (3) omit-when-empty, engine-attested in BOTH directions.
    expect("unbounded_pile" in counterWire).toBe(false);
    expect("unbounded_counters" in tokenWire).toBe(false);
    // (4) the ROW no longer carries a schedule at all — the flag was deleted. Pinning the exact
    // row shape is what catches a partial revert that leaves the field on one side.
    expect(tokenWire.unbounded_resources).toEqual([{ axis: "TokensCreated", player: 0 }]);
    expect(JSON.stringify(tokenWire)).not.toContain("scheduled");
    expect(JSON.stringify(counterWire)).not.toContain("scheduled");
    // (5) the second family — pins the externally-tagged `ResourceAxis` encoding across the
    // language boundary: a data variant is a single-key OBJECT, not a bare string.
    expect(counterWire.unbounded_resources[0].axis).toEqual({ Counter: ["Other", "Other"] });
    // (6) THE COLLAPSE STATE, engine-emitted, in all three of its shapes — and its serde
    // encoding, which is `tag = "type", content = "data"`. Three real engine frames:
    //   - token wire: a BATCHED `Tokens` accept ⇒ Conditional, because its boundary mint can park;
    //   - counter wire: the real kilo `DriveSequence` accept ⇒ Committed, the only one in the suite;
    //   - declined wire: the post-decline frame ⇒ Unscheduled, axis still ∞, promise withdrawn.
    // Certainty is the discriminator here: the first two are both "scheduled", and a projection
    // that collapsed them into one answer reds this row.
    expect(tokenWire.unbounded_families).toEqual([
      { player: 0, family: "tokens", state: { type: "Scheduled", data: "Conditional" } },
    ]);
    expect(counterWire.unbounded_families).toEqual([
      { player: 0, family: "counters", state: { type: "Scheduled", data: "Committed" } },
    ]);
    expect(declinedWire.unbounded_families).toEqual([
      { player: 0, family: "counters", state: { type: "Unscheduled" } },
    ]);
    // (7) the encoding is a NESTED OBJECT, never a flattened string like "Conditional". Asserted
    // on the parsed value rather than as a JSON substring, because `serde_json::Map` is
    // BTreeMap-backed and emits `data` before `type` — a substring would pin key ORDER, which is
    // not the claim. Worth keeping beside (6) because the client switches on `state.type`, and a
    // flattened encoding would silently fall through every case of that switch.
    expect(typeof tokenWire.unbounded_families[0].state).toBe("object");
    //
    // WHAT THIS FILE CANNOT SEE, stated so the coverage is not overread. Each golden holds exactly
    // ONE family row on ONE seat, so:
    // - `Mixed` is invisible here. Pinned engine-side by
    //   `derived_views::tests::mixed_family_is_not_scheduled` and
    //   `two_controllers_draining_one_victim_do_not_cross_schedule`, and client-side by `M1-e`.
    // - the `Mana(_)` scope limit and the two-controllers-one-victim case are invisible here
    //   (single non-mana axis, single seat). Pinned by
    //   `loop_shortcut_mana_engine::scheduled_drive_still_renders_the_already_spendable_mana_badge`
    //   (R4/agree) and
    //   `derived_views::tests::two_controllers_draining_one_victim_do_not_cross_schedule`.
  });

  it("pins EVERY axis tag to the same family in both languages", () => {
    // F4 — the TOTAL cross-language grouping guard. `unbounded-family-tags.json` is emitted by
    // `derived_views::tests::family_tag_table_matches_the_client_golden` from the engine's
    // `family_of` over one representative per `ResourceAxis` tag. Without this, the offer modal
    // (which still maps axes client-side via `familyOf`) and the HUD badge (which now reads the
    // engine's families) could group the same axis differently and nothing would notice.
    //
    // MUTATIONS: rename a Rust variant or drop `rename_all` ⇒ the golden regenerates with
    // different values ⇒ the per-key comparison reds. Change one Rust `family_of` arm ⇒ that key
    // diverges. Rename a TS literal ⇒ the `Record` typecheck reds AND this reds. Add an 18th axis
    // ⇒ TS's exhaustive `Record<ResourceAxisTag, …>` breaks the build, and until the Rust
    // representative list is extended the key sets differ ⇒ this reds.
    const golden = familyTags as Record<string, UnboundedFamily>;
    expect(Object.keys(golden).sort()).toEqual(Object.keys(UNBOUNDED_FAMILY_FOR_TEST).sort());
    for (const [tag, family] of Object.entries(golden)) {
      expect(UNBOUNDED_FAMILY_FOR_TEST[tag as ResourceAxisTag]).toBe(family);
    }
  });

  it("drives the real groupByName pile predicate off engine ids", () => {
    const unboundedPileIds: ReadonlySet<ObjectId> = new Set(tokenWire.unbounded_pile);
    const objects: GameObject[] = [
      ...[402, 403, 404, 407].map((id) => saproling(id, true)),
      ...[406, 408, 409, 410].map((id) => saproling(id, false)),
      buildGameObject({
        id: 401,
        name: "Witherbloom, the Balancer",
        tapped: true,
        card_id: 9001,
        controller: 0,
        owner: 0,
      }),
    ];

    const groups = groupByName(objects, new Set(), unboundedPileIds);
    const groupOf = (id: ObjectId) => {
      const group = groups.find((g) => g.ids.includes(id));
      expect(group, `no group contains ${id}`).toBeDefined();
      return group!;
    };

    // NEGATIVES FIRST, POSITIVE LAST — deliberate. A failing `expect` throws and skips the rest of
    // the `it`, and the regression class this file exists to catch (the engine stops emitting the
    // pile) reds the POSITIVE. Asserting the negatives first keeps them observable as the paired
    // control in that same run instead of being skipped by the positive's throw.
    //
    // (5) paired NEGATIVE from the SAME groupByName call: same name, differs only on `tapped`.
    expect(groupOf(406).ids).toEqual([406, 408, 409, 410]);
    expect(groupOf(406).isUnboundedPile).toBe(false);
    // (6) free third negative: tapped, but not a pile member — so it is not "everything tapped".
    expect(groupOf(401).isUnboundedPile).toBe(false);
    // (4) paired POSITIVE: the tapped Saprolings the engine named.
    expect(groupOf(402).ids).toEqual([402, 403, 404, 407]);
    expect(groupOf(402).isUnboundedPile).toBe(true);
  });

  it("decodes both externally-tagged axis shapes through the real familyOf", () => {
    // (7) unit variant — a bare string on the wire.
    expect(familyOf(tokenWire.unbounded_resources[0].axis as ResourceAxis)).toBe("tokens");
    // (8) data variant — a single-key object on the wire.
    expect(
      familyOf(counterWire.unbounded_resources[0].axis as unknown as ResourceAxis),
    ).toBe("counters");
    // (9) redundant reinforcement, kept as documentation of intent: it cannot fail unless (7) or
    // (8) already has.
    expect(familyOf(tokenWire.unbounded_resources[0].axis as ResourceAxis)).not.toBe(
      familyOf(counterWire.unbounded_resources[0].axis as unknown as ResourceAxis),
    );
  });

  it("feeds the real useUnboundedCounterTypes hook from the engine wire", () => {
    setGameStoreForTest({
      gameState: buildGameState({ derived: counterWire as unknown as DerivedViews }),
    });
    // (10) paired POSITIVE through the real zustand selector.
    expect(renderHook(() => useUnboundedCounterTypes(405)).result.current).toEqual(["charge"]);
    // (11) paired NEGATIVE: 404 is on the same battlefield and carries no ∞ mark.
    expect(renderHook(() => useUnboundedCounterTypes(404)).result.current).toEqual([]);
  });
});
