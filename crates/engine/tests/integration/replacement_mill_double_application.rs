//! Bug B — a bare mill replacement must not be applied twice, and must never
//! mill the replacement's controller.
//!
//! `mill_applier` folds a mill replacement's resolved count into the substituted
//! `ProposedEvent::Mill`, so the modified event already carries the whole of the
//! replacement's work. Before the fix the stash derivation ALSO parked a
//! post-replacement continuation for the same definition; because
//! `ProposedEvent::Mill::affected_object_id()` is `None`, that continuation bound
//! to the replacement's source and resolved its `TargetFilter::Controller` to the
//! REPLACEMENT's controller — milling the wrong player a second time.
//!
//! CR references (verified against docs/MagicCompRules.txt):
//!   - CR 614.6: a replaced event never happens; a modified event occurs instead.
//!   - CR 701.17a: to mill N cards is to put N cards from the top of the library
//!     into the graveyard.
//!   - CR 701.17b: a player can't mill more cards than their library holds.
//!   - CR 608.2n: a resolved instant or sorcery is put into its owner's graveyard.

use engine::game::replacement::{replace_event, ReplacementResult};
use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::ability::{
    AbilityDefinition, AbilityKind, Effect, QuantityExpr, ReplacementDefinition,
    ReplacementPlayerScope, TargetFilter,
};
use engine::types::events::GameEvent;
use engine::types::game_state::WaitingFor;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::proposed_event::ProposedEvent;
use engine::types::replacements::ReplacementEvent;
use engine::types::zones::Zone;
use std::collections::HashSet;

/// Bruvac the Grandiloquent — printed Oracle text, verbatim (Scryfall).
const BRUVAC: &str = "If an opponent would mill one or more cards, they mill twice that many \
                      cards instead. (To mill a card, a player puts the top card of their \
                      library into their graveyard.)";

/// Tome Scour — printed Oracle text, verbatim. Deliberately rider-free so
/// `CastOutcome` deltas are unambiguous.
const TOME_SCOUR: &str = "Target player mills five cards.";

/// CR 701.17b: both libraries are staged far deeper than any count any test can
/// mill, so the clamp in `effects/mill.rs` can never mask a delta. Asserted, not
/// assumed.
const LIBRARY_DEPTH: usize = 40;
const BASE_MILL_COUNT: usize = 5; // Tome Scour's printed count

/// CR 614.6: Bruvac doubles P1's mill on the substituted event itself. The
/// modified event has already happened, so no second application may occur —
/// and in particular the replacement's controller (P0) must not be milled.
#[test]
fn mill_replacement_doubles_once_and_mills_only_the_event_player() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let bruvac = scenario
        .add_creature_from_oracle(P0, "Bruvac the Grandiloquent", 1, 4, BRUVAC)
        .id();
    let tome_scour = scenario
        .add_spell_to_hand_from_oracle(P0, "Tome Scour", /* is_instant */ false, TOME_SCOUR)
        .with_mana_cost(ManaCost::zero()) // auto-pay; no ManaPayment window
        .id();
    // CR 701.17b: stage BOTH libraries far deeper than any count either test can mill.
    for pid in [P0, P1] {
        for i in 0..LIBRARY_DEPTH {
            scenario.add_card_to_library_top(pid, &format!("Lib {i}"));
        }
    }
    let mut runner = scenario.build();

    // --- PRECONDITIONS, asserted BEFORE the cast ---
    // (i) The fixture built what this test believes it built. Without this, a
    //     reminder-text or grammar mismatch yields a vanilla 1/4 creature and the
    //     wrong-player assertion below would pass for the wrong reason.
    let repl: Vec<_> = runner
        .state()
        .objects
        .get(&bruvac)
        .expect("Bruvac on battlefield")
        .replacement_definitions
        .iter_unchecked()
        .filter(|r| r.event == ReplacementEvent::Mill)
        .collect();
    assert_eq!(
        repl.len(),
        1,
        "Bruvac's Oracle text must parse to exactly one Mill replacement"
    );
    assert_eq!(
        repl[0].valid_player,
        Some(ReplacementPlayerScope::Opponent),
        "the replacement must be opponent-scoped, or affected player == controller and \
         the wrong-player assertion below cannot discriminate"
    );

    // (ii) Depth guard. P0's library must be NON-EMPTY, so that under the bug a
    //      clamped mill still shows a NON-ZERO delta and `p0 delta == 0` can only
    //      be satisfied by the fix, never by the CR 701.17b clamp. The bound
    //      below is stronger than that (it exceeds one base mill), which is fine
    //      — but note it does NOT exceed the 20 a buggy continuation actually
    //      mills, so do not read it as bounding the buggy count. It does not need
    //      to: at any non-empty depth the clamp cannot produce a zero delta.
    let p0_before = runner
        .state()
        .players
        .iter()
        .find(|p| p.id == P0)
        .expect("P0")
        .library
        .len();
    let p1_before = runner
        .state()
        .players
        .iter()
        .find(|p| p.id == P1)
        .expect("P1")
        .library
        .len();
    assert!(
        p0_before > 2 * BASE_MILL_COUNT,
        "P0 library ({p0_before}) must be deep enough that the clamp cannot mask the defect"
    );
    assert!(p1_before > 2 * BASE_MILL_COUNT);

    let outcome = runner.cast(tome_scour).target_player(P1).resolve();

    // --- POSITIVE CONTROL (non-vacuity) ---
    assert_eq!(
        p1_before - outcome.zone_count(P1, Zone::Library),
        2 * BASE_MILL_COUNT,
        "the replacement must actually have applied (5 -> 10)"
    );

    // --- DISCRIMINATORS ---
    assert_eq!(
        p0_before - outcome.zone_count(P0, Zone::Library),
        0,
        "CR 614.6: the replacement's controller must not be milled"
    );
    // P0's graveyard holds EXACTLY Tome Scour itself. CR 608.2n: "As the final part
    // of an instant or sorcery spell's resolution, the spell is put into its owner's
    // graveyard" — the delivered-resolution move in `stack.rs`. Anything above 1 is a
    // milled card. Asserting `== 0` here would be WRONG and would fail both at base
    // and after the fix, for a reason unrelated to this bug.
    assert_eq!(
        outcome.zone_count(P0, Zone::Graveyard),
        1,
        "CR 614.6: only Tome Scour itself reaches P0's graveyard — no milled cards"
    );

    // --- BOUNDARY ---
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "no phantom pause: the freeze in the filed report"
    );
}

/// CR 614.6: a bare mill replacement is folded into the event by `mill_applier`,
/// so the stash derivation must not also park a post-replacement continuation.
#[test]
fn bare_mill_replacement_stashes_no_continuation() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let bruvac = scenario
        .add_creature_from_oracle(P0, "Bruvac the Grandiloquent", 1, 4, BRUVAC)
        .id();
    for i in 0..LIBRARY_DEPTH {
        scenario.add_card_to_library_top(P1, &format!("Lib {i}"));
    }
    let mut runner = scenario.build();
    let mut events = Vec::new();

    // The fixture built what this test believes it built. The match arm below is
    // itself a control, but a control that is only redundant given a second control
    // in the same test stops being redundant the moment either one is edited.
    let repl: Vec<_> = runner
        .state()
        .objects
        .get(&bruvac)
        .expect("Bruvac on battlefield")
        .replacement_definitions
        .iter_unchecked()
        .filter(|r| r.event == ReplacementEvent::Mill)
        .collect();
    assert_eq!(
        repl.len(),
        1,
        "Bruvac's Oracle text must parse to exactly one Mill replacement"
    );
    assert_eq!(
        repl[0].valid_player,
        Some(ReplacementPlayerScope::Opponent),
        "the replacement must be opponent-scoped, or it never applies to P1's mill"
    );

    let proposed = ProposedEvent::Mill {
        player_id: P1,
        count: BASE_MILL_COUNT as u32,
        destination: Zone::Graveyard,
        applied: HashSet::new(),
    };
    match replace_event(runner.state_mut(), proposed, &mut events) {
        // POSITIVE CONTROL: the applier folded — proves we reached the seam under test.
        ReplacementResult::Execute(ProposedEvent::Mill {
            count, player_id, ..
        }) => {
            assert_eq!(
                count,
                2 * BASE_MILL_COUNT as u32,
                "Bruvac doubles P1's mill"
            );
            assert_eq!(player_id, P1, "the event still names the affected player");
        }
        other => panic!("expected Execute(Mill {{ count: 10 }}), got {other:?}"),
    }
    // DISCRIMINATOR
    assert!(
        !runner.state().has_post_replacement_drain(),
        "CR 614.6: a folded bare mill must not leave a post-replacement drain"
    );
}

/// CR 614.6: the `sub_ability` escape runs BEFORE the suppression list, so a mill
/// replacement carrying a rider must still stash its continuation — the rider's
/// own work exists nowhere else in the pipeline. This is the over-reach guard: it
/// is green both at base and after the fix, and goes red only if the new
/// suppression arm is written so as to precede or subsume that escape.
///
/// Deliberately seam-direct. Draining this continuation would execute a
/// wrong-player double mill that this phase does not fix, pinning defective
/// behaviour as expected in a permanent test. The stash's *existence* is the whole
/// of what the suppression arm can affect.
#[test]
fn mill_replacement_with_rider_still_stashes_a_continuation() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Hand-built, because the parser cannot produce a Mill replacement with a rider.
    // Attached via `CardBuilder::with_replacement_definition`, which writes BOTH
    // `replacement_definitions` and `base_replacement_definitions` — a live-only
    // write would be dropped by a layers reset.
    let mut rider_def = ReplacementDefinition::new(ReplacementEvent::Mill).execute(
        AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Mill {
                // Deliberately Fixed, not `Multiply{2, EventContextAmount}`: the
                // escape and the fold guard both key ONLY on `sub_ability`, so the
                // count shape is irrelevant to the axis under test — and a fixed
                // count keeps every expected value exactly derivable.
                count: QuantityExpr::Fixed { value: 3 },
                target: TargetFilter::Controller,
                destination: Zone::Graveyard,
            },
        )
        // THE AXIS UNDER TEST: `sub_ability` present => the escape fires.
        // Goes on the EXECUTE ability, not on `ReplacementDefinition`.
        .sub_ability(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::LoseLife {
                amount: QuantityExpr::Fixed { value: 1 },
                target: None,
            },
        )),
    );
    rider_def.valid_player = Some(ReplacementPlayerScope::Opponent);

    let source = scenario
        .add_creature_from_oracle(P0, "Rider Mill Source", 1, 1, "")
        .with_replacement_definition(rider_def)
        .id();
    for i in 0..LIBRARY_DEPTH {
        scenario.add_card_to_library_top(P1, &format!("Lib {i}"));
    }
    let mut runner = scenario.build();
    let mut events = Vec::new();

    // The hand-built def actually landed on the object.
    let repl_count = runner
        .state()
        .objects
        .get(&source)
        .expect("source on battlefield")
        .replacement_definitions
        .iter_unchecked()
        .filter(|r| r.event == ReplacementEvent::Mill)
        .count();
    assert_eq!(
        repl_count, 1,
        "the hand-built rider replacement must be attached"
    );

    let proposed = ProposedEvent::Mill {
        player_id: P1,
        count: BASE_MILL_COUNT as u32,
        destination: Zone::Graveyard,
        applied: HashSet::new(),
    };
    match replace_event(runner.state_mut(), proposed, &mut events) {
        // RIDER-PATH SIGNATURE — deliberately NOT labelled a positive control. The
        // applier DECLINED to fold (its guard requires `sub_ability.is_none()`), so
        // the count is UNMODIFIED, which is what distinguishes this fixture from the
        // bare one. But it cannot prove ENTRY: an unmodified `Execute(Mill { count:
        // 5, player_id: P1 })` is exactly what `replace_event` returns when NO
        // replacement applied at all. The entry proof is the separate
        // `ReplacementApplied` assertion below.
        ReplacementResult::Execute(ProposedEvent::Mill {
            count, player_id, ..
        }) => {
            assert_eq!(
                count, BASE_MILL_COUNT as u32,
                "a rider-bearing execute is not folded: the event keeps its original count"
            );
            assert_eq!(player_id, P1, "the event still names the affected player");
        }
        other => panic!("expected Execute(Mill {{ count: 5 }}), got {other:?}"),
    }

    // POSITIVE CONTROL — proof of ENTRY, independent of the discriminator.
    // `ReplacementApplied` is pushed by `apply_single_replacement` AFTER the
    // post-effect block closes, so it fires whether or not a continuation is
    // stashed. That orthogonality is the point: it separates "the definition
    // applied" from "a continuation was stashed", where the unmodified count
    // above cannot (an unfolded `Mill { count: 5 }` is also what a NON-applying
    // replacement returns). Without this the discriminator would be its own
    // entry proof.
    assert!(
        events.iter().any(|e| matches!(
            e,
            GameEvent::ReplacementApplied { source_id, .. } if *source_id == source
        )),
        "positive control: the rider definition must actually have APPLIED; got {events:?}"
    );

    // DISCRIMINATOR — the over-reach guard. The escape runs BEFORE the suppression
    // list, so the new arm must not suppress this stash.
    assert!(
        runner.state().has_post_replacement_drain(),
        "CR 614.6: the `sub_ability` escape must still stash a rider continuation"
    );
}
