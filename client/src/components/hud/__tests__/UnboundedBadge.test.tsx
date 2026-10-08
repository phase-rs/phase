/**
 * The ∞ badge and the engine-owned collapse state behind it.
 *
 * DATA SOURCE IS LABELLED PER ROW. Three engine goldens are imported here and each carries
 * exactly one `unbounded_families` row on player 0:
 *   - `unbounded-token-wire.json`    → `tokens`,  `Scheduled(Conditional)` ⇒ `∞→?`
 *   - `unbounded-counter-wire.json`  → `counters`, `Scheduled(Committed)`  ⇒ `∞→N`
 *   - `unbounded-declined-wire.json` → `counters`, `Unscheduled`           ⇒ bare `∞`
 * Every multi-family, cross-player or empty case is COMPOSED against the exported prop contract
 * and says so.
 *
 * The family FOLD is no longer here to test: the engine computes it, on the loop's producing
 * controller key, which does not survive onto the wire. Its join laws live in
 * `derived_views::tests::family_collapse_state_merge_is_a_join` (was U6) and its multi-controller
 * discriminator in `two_controllers_draining_one_victim_do_not_cross_schedule`.
 */
import { act } from "react";
import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it } from "vitest";

import type { DerivedViews, UnboundedFamilyView } from "../../../adapter/types.ts";
import { useGameStore } from "../../../stores/gameStore.ts";
import { useMultiplayerStore } from "../../../stores/multiplayerStore.ts";
import { buildGameState } from "../../../test/factories/gameStateFactory.ts";
import counterWire from "../../../test/fixtures/unbounded-counter-wire.json";
import declinedWire from "../../../test/fixtures/unbounded-declined-wire.json";
import tokenWire from "../../../test/fixtures/unbounded-token-wire.json";
import { UnboundedBadge } from "../HudBadges.tsx";
import { PlayerHud } from "../PlayerHud.tsx";

const PLAIN_TOKENS = "Unbounded tokens (∞)";
// TWO VOICES, and which one renders is an engine fact, not a style choice. The engine publishes
// `state.data.prompted` — the loop's CONTROLLER, the seat that will be asked to name N. The badge
// says "you" only when that seat IS the viewer AND the viewer is not spectating; otherwise it
// keeps the passive voice, because the row is keyed by the ATTRIBUTION player, which for a
// victim-attributed axis is the victim, and the badge also renders on opponent HUDs.
const COMMITTED_COUNTERS =
  "Unbounded counters (∞) — collapse pending; a finite amount will be chosen";
const CONDITIONAL_TOKENS = "Unbounded tokens (∞) — collapse pending; this may stay unbounded";
const COMMITTED_COUNTERS_YOU = "Unbounded counters (∞) — collapse pending; you'll name the count";
const CONDITIONAL_TOKENS_YOU =
  "Unbounded tokens (∞) — collapse pending; this may stay unbounded, and you'll name the count if it doesn't";
const COMMITTED_TOKENS_YOU = "Unbounded tokens (∞) — collapse pending; you'll name the count";
const COMMITTED_TOKENS = "Unbounded tokens (∞) — collapse pending; a finite amount will be chosen";
const MIXED_COUNTERS =
  "Unbounded counters (∞) — part of this group has a pending collapse; part remains unbounded";
const PLAIN_COUNTERS = "Unbounded counters (∞)";

// COMPOSED family rows, for the cases no single golden frame produces.
const fam = (
  family: UnboundedFamilyView["family"],
  state: UnboundedFamilyView["state"],
  player = 0,
): UnboundedFamilyView => ({ player, family, state });

describe("UnboundedBadge + usePlayerDesignations", () => {
  beforeEach(() => {
    useMultiplayerStore.setState({ activePlayerId: 0, isSpectator: false });
    // `gameMode` is reset explicitly because U8/U9/U10 set it, and a leaked `"spectate"` would
    // silently suppress every second-person assertion in the rows that follow.
    useGameStore.setState({ gameMode: null, gameState: buildGameState() });
  });

  afterEach(() => {
    cleanup();
  });

  const seed = (derived: DerivedViews) => {
    act(() => {
      useGameStore.setState({ gameState: buildGameState({ derived }) });
    });
    render(<PlayerHud />);
  };

  // The OFFER-TIME shape: an `UnboundedBadge` with no `state` prop, which is the modal's mount
  // and has no HUD mount site, so it is rendered directly rather than through `seed`.
  const seedOfferBadge = (derived: DerivedViews) => {
    act(() => {
      useGameStore.setState({ gameState: buildGameState({ derived }) });
    });
    render(<UnboundedBadge family="tokens" />);
  };

  const boundedTokens = (bound: number) =>
    `tokens from a detected loop, bounded at ${bound} repetitions`;

  it("U1/M2-d: the token golden's CONDITIONAL collapse renders ∞→?, and the counter golden's COMMITTED one renders ∞→N", () => {
    // GOLDEN-DRIVEN, both halves — the family and its state are read out of regenerated engine
    // goldens, never authored here. The pair IS the discriminator: same badge component, two real
    // engine frames, two different glyphs. A component that mapped every `Scheduled` to the
    // scheduled glyph passes the second half and fails the first; one that never renders `∞→N`
    // fails the second.
    //
    // BOTH LABELS ARE SECOND-PERSON, and the HONEST BOUND for that is worth stating: both goldens
    // carry `prompted: 0`, and the default test viewer is seat 0, so this fixture pins "the viewer
    // really is the seat that will be asked ⇒ address them" and NOTHING about the divergent case.
    // It CANNOT witness a prompted seat that differs from the badge's seat — the token golden's
    // only axis is `TokensCreated`, an aggregate axis that attributes to its own controller, so
    // badge seat == prompted seat == viewer by construction. U8 composes the divergence, and the
    // engine-side pin is `two_controllers_draining_one_victim_do_not_cross_schedule` arms B/C.
    seed(tokenWire as unknown as DerivedViews);
    expect(screen.getAllByLabelText(/Unbounded/)).toHaveLength(1);
    const conditional = screen.getByLabelText(CONDITIONAL_TOKENS_YOU);
    expect(conditional).toBeInTheDocument();
    expect(conditional.textContent).toContain("∞→?");
    expect(conditional.textContent).not.toContain("∞→N");

    // MATCHED POSITIVE, from the REAL kilo dump's `DriveSequence` accept — the only Committed
    // frame in the suite.
    cleanup();
    seed(counterWire as unknown as DerivedViews);
    const committed = screen.getByLabelText(COMMITTED_COUNTERS_YOU);
    expect(committed).toBeInTheDocument();
    expect(committed.textContent).toContain("∞→N");
  });

  it("U2/M2-d: the DECLINED golden renders a bare ∞ — the badge stops promising", () => {
    // GOLDEN-DRIVEN. This frame is the engine's post-decline output: the axis is still ∞ but the
    // stash is gone. It is the regression this whole change exists for — the old badge kept
    // promising `∞→N` here.
    //
    // MUTATION COVERAGE: an engine change that makes `scheduled_display_axes` read
    // `unbounded_resources` instead of the stash regenerates this golden as `Scheduled` and reds
    // the `not.toContain("∞→")` line below.
    seed(declinedWire as unknown as DerivedViews);
    expect(screen.getAllByLabelText(/Unbounded/)).toHaveLength(1);
    const badge = screen.getByLabelText(PLAIN_COUNTERS);
    expect(badge).toBeInTheDocument();
    expect(badge.textContent).toContain("∞");
    expect(badge.textContent).not.toContain("∞→");
  });

  it("U3/families: the engine's rows are rendered one badge per family, unmodified", () => {
    // COMPOSED — no single golden frame carries two families. The point of the row is that the FE
    // performs no fold at all now: two engine rows in, two badges out, each with its own state.
    // `prompted` is deliberately OMITTED on both rows: this row is about family fan-out, not about
    // voice, and an omitted seat renders the third person for everyone (pinned by U9).
    seed({
      unbounded_families: [
        fam("tokens", { type: "Scheduled", data: { certainty: "Conditional" } }),
        fam("counters", { type: "Scheduled", data: { certainty: "Committed" } }),
      ],
    } as DerivedViews);
    expect(screen.getAllByLabelText(/Unbounded/)).toHaveLength(2);
    expect(screen.getByLabelText(CONDITIONAL_TOKENS).textContent).toContain("∞→?");
    expect(screen.getByLabelText(COMMITTED_COUNTERS).textContent).toContain("∞→N");
  });

  it("U3b/absent: no family channel ⇒ no badge, and the same frame with one ⇒ a badge", () => {
    // The dominant case: no loop is active, so the engine omits the field entirely.
    seed({ unbounded_families: [] } as unknown as DerivedViews);
    expect(screen.queryByLabelText(/Unbounded/)).toBeNull();

    // MATCHED POSITIVE in the same `it`: without it a HUD that never renders the badge at all
    // satisfies the assertion above.
    cleanup();
    seed({ unbounded_families: [fam("tokens", { type: "Unscheduled" })] } as DerivedViews);
    expect(screen.getByLabelText(PLAIN_TOKENS)).toBeInTheDocument();
  });

  it("M1-e/Mixed: a mixed family renders a bare ∞ — never ∞→N, never ∞→?", () => {
    // COMPOSED for the `Mixed` frame (its engine reachability is proven by
    // `derived_views::tests::mixed_family_is_not_scheduled` and
    // `two_controllers_draining_one_victim_do_not_cross_schedule`), and GOLDEN-DRIVEN for the
    // matched positive.
    //
    // WHY IT FLIPPED FROM THE OLD TEST. This used to be U3c, which asserted the OPPOSITE: the
    // client folded a family with one scheduled and one unscheduled axis to `scheduled: true` and
    // rendered `∞→N` — an over-report the old comment documented and defended, because a boolean
    // could not say anything else. `Mixed` is representable now, so the honest answer is available
    // and this is it.
    seed({ unbounded_families: [fam("counters", { type: "Mixed" })] } as DerivedViews);
    const mixed = screen.getByLabelText(MIXED_COUNTERS);
    expect(mixed).toBeInTheDocument();
    expect(mixed.textContent).toContain("∞");
    expect(mixed.textContent).not.toContain("∞→");

    // MATCHED POSITIVE from the REAL kilo golden: the badge CAN render `∞→N`, so the negatives
    // above are not satisfied by a component that renders a bare `∞` for everything.
    cleanup();
    seed(counterWire as unknown as DerivedViews);
    expect(screen.getByLabelText(COMMITTED_COUNTERS_YOU).textContent).toContain("∞→N");
  });

  it("U4/viewer: another seat's SCHEDULED family does not schedule this seat's badge", () => {
    // COMPOSED. Exercises the hook's per-player filter, which is why it stays render-level.
    //
    // The hazard is the seat filter itself: seat 1's row genuinely carries a schedule, so if
    // `forPlayer` leaked it, seat 0 would render a bound off another player's collapse.
    seed({
      unbounded_families: [
        fam("tokens", { type: "Unscheduled" }, 0),
        // `prompted` omitted — this row tests the SEAT FILTER, not the voice.
        fam("tokens", { type: "Scheduled", data: { certainty: "Committed" } }, 1),
      ],
    } as DerivedViews);
    expect(screen.getAllByLabelText(/Unbounded/)).toHaveLength(1);
    expect(screen.getByLabelText(PLAIN_TOKENS)).toBeInTheDocument();
    expect(screen.queryByLabelText(/collapse pending/)).toBeNull();

    // MATCHED POSITIVE — same shape, schedule on THIS seat. Without it the assertions above pass
    // against a badge that can never render a bound, and the filter would be untested in the
    // direction that matters.
    cleanup();
    seed({
      unbounded_families: [
        fam("tokens", { type: "Scheduled", data: { certainty: "Committed" } }, 0),
        fam("tokens", { type: "Unscheduled" }, 1),
      ],
    } as DerivedViews);
    expect(screen.getByLabelText(/collapse pending; a finite amount will be chosen/)).toBeInTheDocument();
  });

  it("U8/agency: the badge addresses the prompted seat, and only the prompted seat", () => {
    // COMPOSED, and a MATCHED PAIR inside one `it` so neither half can stand alone. Identical
    // family row; the ONLY thing that changes is `prompted`. Reds from both sides:
    //   - delete the `you` branch          ⇒ the second-person half below fails;
    //   - hardcode the second person       ⇒ the third-person half below fails.
    //
    // Seat 2 is deliberately NOT a seat this viewer can be: the badge sits on seat 0's HUD while
    // the engine says seat 2 will be asked. That is the divergent shape the goldens cannot
    // produce, and it is exactly the case a naive "the badge is on my HUD, so it's my prompt"
    // implementation gets wrong.
    act(() => {
      useGameStore.setState({ gameMode: "online" });
      useMultiplayerStore.setState({ activePlayerId: 0 });
    });
    seed({
      unbounded_families: [fam("tokens", { type: "Scheduled", data: { certainty: "Committed", prompted: 2 } }, 0)],
    } as DerivedViews);
    expect(screen.getByLabelText(COMMITTED_TOKENS)).toBeInTheDocument();
    expect(screen.queryByLabelText(COMMITTED_TOKENS_YOU)).toBeNull();

    // MATCHED POSITIVE: same row, same viewer, prompted seat is now the viewer.
    cleanup();
    seed({
      unbounded_families: [fam("tokens", { type: "Scheduled", data: { certainty: "Committed", prompted: 0 } }, 0)],
    } as DerivedViews);
    expect(screen.getByLabelText(COMMITTED_TOKENS_YOU)).toBeInTheDocument();
    expect(screen.queryByLabelText(COMMITTED_TOKENS)).toBeNull();
  });

  it("U9/ambiguous: an omitted prompted seat reads third person even for the viewer", () => {
    // COMPOSED. `prompted` is omitted when the family's scheduled axes name TWO OR MORE distinct
    // seats (the engine's seat meet fell to ⊥) — never "nobody". One glyph cannot address two
    // players, so the badge must fall back to the seat-neutral voice rather than pick a winner,
    // and it must do so even though the viewer is one of the candidates.
    act(() => {
      useGameStore.setState({ gameMode: "online" });
      useMultiplayerStore.setState({ activePlayerId: 0 });
    });
    seed({
      unbounded_families: [fam("tokens", { type: "Scheduled", data: { certainty: "Committed" } }, 0)],
    } as DerivedViews);
    expect(screen.getByLabelText(COMMITTED_TOKENS)).toBeInTheDocument();
    expect(screen.queryByLabelText(COMMITTED_TOKENS_YOU)).toBeNull();
  });

  it("U10/spectator: a spectator reads third person even when the prompted seat equals their resolved id", () => {
    // COMPOSED, and this is the FALSE-POSITIVE GATE — the one error class this design must not
    // have. `usePlayerId()` returns `PLAYER_ID` (0) in spectate mode, NOT `SPECTATOR_PLAYER_ID`,
    // so a loop prompted to seat 0 resolves `prompted === viewer` for EVERY spectator watching.
    // Only the `!spectating &&` conjunct stops the badge telling a spectator they will name the
    // count.
    //
    // REVERT-PROBE: drop `!spectating &&` from the `you` predicate ⇒ this reds, because the
    // equality it guards genuinely holds here. U8's second-person half is the matched positive
    // proving the gate does not simply suppress every "you".
    act(() => {
      useGameStore.setState({ gameMode: "spectate" });
    });
    seed({
      unbounded_families: [fam("tokens", { type: "Scheduled", data: { certainty: "Committed", prompted: 0 } }, 0)],
    } as DerivedViews);
    expect(screen.getByLabelText(COMMITTED_TOKENS)).toBeInTheDocument();
    expect(screen.queryByLabelText(COMMITTED_TOKENS_YOU)).toBeNull();
  });

  it("F1/F1b: a badge with no ∞ row behind it states the engine's published repetition bound, and states the one it was given", () => {
    // MATCHED PAIR in one `it`: the only thing that changes between the halves is the published
    // number. A badge that hard-coded a bound, or ignored the channel and kept `∞`, fails one
    // half or the other.
    seedOfferBadge({ bounded_loop_max_repetitions: 18 } as DerivedViews);
    const at18 = screen.getByLabelText(boundedTokens(18));
    expect(at18.textContent).toContain("≤18×");
    expect(at18.textContent).not.toContain("∞");

    cleanup();
    seedOfferBadge({ bounded_loop_max_repetitions: 7 } as DerivedViews);
    const at7 = screen.getByLabelText(boundedTokens(7));
    expect(at7.textContent).toContain("≤7×");
    expect(screen.queryByLabelText(boundedTokens(18))).toBeNull();
  });

  it("F2: no published bound ⇒ the same badge renders today's bare ∞", () => {
    seedOfferBadge({} as DerivedViews);
    const plain = screen.getByLabelText(PLAIN_TOKENS);
    expect(plain.textContent).toContain("∞");
    expect(plain.textContent).not.toContain("≤");

    // MATCHED POSITIVE: the same mount with the channel present does render the bound, so the
    // assertions above are the absent channel's work and not a badge that can never state one.
    cleanup();
    seedOfferBadge({ bounded_loop_max_repetitions: 7 } as DerivedViews);
    expect(screen.getByLabelText(boundedTokens(7)).textContent).toContain("≤7×");
  });

  it("F3: a badge backed by a published ∞ row keeps ∞ even while a bound is published", () => {
    // The HOSTILE co-existence: a live `∞` mark and an open bounded window on one board. The
    // discriminator is the badge's own prop contract — a badge handed a published row is not the
    // offer-time badge, whatever the channel says.
    seed({
      unbounded_families: [fam("tokens", { type: "Unscheduled" })],
      bounded_loop_max_repetitions: 9,
    } as DerivedViews);
    const badge = screen.getByLabelText(PLAIN_TOKENS);
    expect(badge.textContent).toContain("∞");
    expect(badge.textContent).not.toContain("≤9×");
    expect(screen.queryByLabelText(boundedTokens(9))).toBeNull();

    // MATCHED POSITIVE: the same channel value on the same store, on the offer-time mount.
    cleanup();
    seedOfferBadge({
      unbounded_families: [fam("tokens", { type: "Unscheduled" })],
      bounded_loop_max_repetitions: 9,
    } as DerivedViews);
    expect(screen.getByLabelText(boundedTokens(9)).textContent).toContain("≤9×");
  });
});
