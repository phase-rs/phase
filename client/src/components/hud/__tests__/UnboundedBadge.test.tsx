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
import { PlayerHud } from "../PlayerHud.tsx";

const PLAIN_TOKENS = "Unbounded tokens (∞)";
// Passive voice on purpose: the badge renders on opponent HUDs, and a victim-attributed axis puts
// it on the victim's seat while the loop's CONTROLLER is the one prompted to name N — so any
// second-person phrasing here is addressed to the wrong player.
const COMMITTED_COUNTERS =
  "Unbounded counters (∞) — collapse pending; a finite amount will be chosen";
const CONDITIONAL_TOKENS = "Unbounded tokens (∞) — collapse pending; this may stay unbounded";
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
    useMultiplayerStore.setState({ activePlayerId: 0 });
    useGameStore.setState({ gameState: buildGameState() });
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

  it("U1/M2-d: the token golden's CONDITIONAL collapse renders ∞→?, and the counter golden's COMMITTED one renders ∞→N", () => {
    // GOLDEN-DRIVEN, both halves — the family and its state are read out of regenerated engine
    // goldens, never authored here. The pair IS the discriminator: same badge component, two real
    // engine frames, two different glyphs. A component that mapped every `Scheduled` to the
    // scheduled glyph passes the second half and fails the first; one that never renders `∞→N`
    // fails the second.
    seed(tokenWire as unknown as DerivedViews);
    expect(screen.getAllByLabelText(/Unbounded/)).toHaveLength(1);
    const conditional = screen.getByLabelText(CONDITIONAL_TOKENS);
    expect(conditional).toBeInTheDocument();
    expect(conditional.textContent).toContain("∞→?");
    expect(conditional.textContent).not.toContain("∞→N");

    // MATCHED POSITIVE, from the REAL kilo dump's `DriveSequence` accept — the only Committed
    // frame in the suite.
    cleanup();
    seed(counterWire as unknown as DerivedViews);
    const committed = screen.getByLabelText(COMMITTED_COUNTERS);
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
    seed({
      unbounded_families: [
        fam("tokens", { type: "Scheduled", data: "Conditional" }),
        fam("counters", { type: "Scheduled", data: "Committed" }),
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
    expect(screen.getByLabelText(COMMITTED_COUNTERS).textContent).toContain("∞→N");
  });

  it("U4/viewer: another seat's SCHEDULED family does not schedule this seat's badge", () => {
    // COMPOSED. Exercises the hook's per-player filter, which is why it stays render-level.
    //
    // The hazard is the seat filter itself: seat 1's row genuinely carries a schedule, so if
    // `forPlayer` leaked it, seat 0 would render a bound off another player's collapse.
    seed({
      unbounded_families: [
        fam("tokens", { type: "Unscheduled" }, 0),
        fam("tokens", { type: "Scheduled", data: "Committed" }, 1),
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
        fam("tokens", { type: "Scheduled", data: "Committed" }, 0),
        fam("tokens", { type: "Unscheduled" }, 1),
      ],
    } as DerivedViews);
    expect(screen.getByLabelText(/collapse pending; a finite amount will be chosen/)).toBeInTheDocument();
  });
});
