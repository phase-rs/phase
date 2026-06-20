//! Kid Loki — "Each creature you control that you've put one or more +1/+1
//! counters on this turn has hexproof."
//!
//! Oracle text:
//!   "Each creature you control that you've put one or more +1/+1 counters on
//!    this turn has hexproof.
//!    Whenever you draw your second card each turn, put a +1/+1 counter on Kid
//!    Loki."
//!
//! This drives the REAL parse → layer pipeline: Kid Loki is built from Oracle
//! text via the scenario harness (same synthesis path as production). The
//! conditional static lowers to a `StaticDefinition` whose `affected` filter
//! carries `FilterProp::CountersPutOnThisTurn { actor: Controller, counters:
//! OfType(+1/+1), comparator: GE, count: 1 }` and modifications
//! `[AddKeyword { Hexproof }]`. The counter-placement history (CR 122.6) is
//! populated through `apply_counter_addition`, the single authority the engine
//! uses whenever a +1/+1 counter is put on a permanent.
//!
//! THE BUG this discriminates: the static is a *historical-action* predicate
//! (CR 122.6 "counters being put on an object"), NOT a current-counter query.
//! Assertion (a) — the creature that received a +1/+1 counter this turn HAS
//! hexproof — fails if the static line is left `Effect::Unimplemented` (the
//! pre-fix state) or if the new filter never matches. Assertion (b) — a creature
//! you control that received NO counter this turn does NOT have hexproof —
//! discriminates a degenerate "all creatures you control" misparse. Assertion
//! (c) — an opponent's creature with a +1/+1 counter does NOT get hexproof from
//! Kid Loki — discriminates the `actor: Controller` scope.
//!
//! Counter placement is driven through the public `resolve_ability_chain`
//! production entry with an `Effect::PutCounter` resolved ability — the same
//! seam Kid Loki's own draw trigger uses — so the CR 122.6 placement history is
//! recorded exactly as it is in production.

use engine::game::effects::resolve_ability_chain;
use engine::game::keywords::has_keyword;
use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameRunner, GameScenario};
use engine::types::ability::{Effect, QuantityExpr, ResolvedAbility, TargetFilter, TargetRef};
use engine::types::counter::CounterType;
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

const P0: PlayerId = PlayerId(0);
const P1: PlayerId = PlayerId(1);

const KID_LOKI: &str = "Each creature you control that you've put one or more +1/+1 counters on this turn has hexproof.\n\
Whenever you draw your second card each turn, put a +1/+1 counter on Kid Loki.";

/// True iff `id` currently has `keyword` after a fresh layer evaluation.
fn has_kw(runner: &mut GameRunner, id: ObjectId, keyword: &Keyword) -> bool {
    runner.state_mut().layers_dirty.mark_full();
    evaluate_layers(runner.state_mut());
    has_keyword(&runner.state().objects[&id], keyword)
}

/// CR 122.6: Put one +1/+1 counter on `recipient` as `actor`, driven through the
/// public `resolve_ability_chain` production seam (the same path Kid Loki's draw
/// trigger uses) so the placement is recorded in `counter_added_this_turn`.
fn put_counter(runner: &mut GameRunner, actor: PlayerId, recipient: ObjectId) {
    let ability = ResolvedAbility::new(
        Effect::PutCounter {
            counter_type: CounterType::Plus1Plus1,
            count: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::ParentTarget,
        },
        vec![TargetRef::Object(recipient)],
        recipient,
        actor,
    );
    let mut events = Vec::new();
    resolve_ability_chain(runner.state_mut(), &ability, &mut events, 0)
        .expect("PutCounter must resolve");
}

#[test]
fn kid_loki_grants_hexproof_only_to_creatures_you_put_counters_on_this_turn() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Kid Loki (P0), built from Oracle text through the real parse + synthesis
    // pipeline — carries the conditional hexproof static.
    let _kid_loki = scenario
        .add_creature_from_oracle(P0, "Kid Loki", 1, 4, KID_LOKI)
        .id();

    // P0 creatures: `buffed` will receive a +1/+1 counter this turn; `plain`
    // never does — the negative discriminator.
    let buffed = scenario.add_creature(P0, "Buffed Bear", 2, 2).id();
    let plain = scenario.add_creature(P0, "Plain Bear", 2, 2).id();

    // P1 creature that receives a +1/+1 counter from P1 — the `actor` scope
    // discriminator. Kid Loki's static is scoped to counters YOU (P0) put.
    let opp = scenario.add_creature(P1, "Opponent Bear", 2, 2).id();

    let mut runner = scenario.build();

    // ----- Baseline: no counters placed yet → nobody has hexproof -----
    assert!(
        !has_kw(&mut runner, buffed, &Keyword::Hexproof),
        "before any counter is placed, no creature has Kid Loki's hexproof"
    );

    // P0 puts a +1/+1 counter on `buffed` this turn (CR 122.6 history record).
    put_counter(&mut runner, P0, buffed);
    // P1 puts a +1/+1 counter on their own creature this turn.
    put_counter(&mut runner, P1, opp);

    // (a) The creature P0 put a +1/+1 counter on this turn HAS hexproof. This
    // assertion flips to failure if the Kid Loki static line is reverted to
    // `Effect::Unimplemented` (no static installed) or the new filter never
    // matches the placement record.
    assert!(
        has_kw(&mut runner, buffed, &Keyword::Hexproof),
        "CR 122.6 + CR 702.11: a creature you put a +1/+1 counter on this turn gains hexproof"
    );

    // (b) A creature you control with NO counter this turn does NOT have
    // hexproof — discriminates a degenerate "all creatures you control" misparse.
    assert!(
        !has_kw(&mut runner, plain, &Keyword::Hexproof),
        "a creature with no +1/+1 counter placed this turn must NOT have hexproof"
    );

    // (c) The opponent's creature — buffed by the OPPONENT, not by you — does NOT
    // gain hexproof from Kid Loki. Discriminates the `actor: Controller` scope.
    assert!(
        !has_kw(&mut runner, opp, &Keyword::Hexproof),
        "CR 109.5: Kid Loki's static is scoped to counters YOU put — an opponent's counter does not qualify"
    );
}

/// CR 122.6 + CR 514.2: the `CountersPutOnThisTurn` predicate is turn-scoped.
/// When the turn ends, the `counter_added_this_turn` ledger is cleared, so a
/// creature that gained hexproof from a +1/+1 counter placed THIS turn must LOSE
/// hexproof on the next turn — the historical-action match no longer holds.
///
/// THE BUG this discriminates (the layer-cache-invalidation gap): the turn
/// boundary (`start_next_turn`) clears the ledger but, before the fix, did NOT
/// mark layers dirty. SBA only recomputes layers when the dirty flag is set
/// (`sba.rs`) and `flush_layers` is a no-op for `LayersDirty::Clean`
/// (`layers.rs`), so the object's keyword cache stayed STALE and the creature
/// incorrectly KEPT hexproof into the next turn.
///
/// This test reads hexproof through the NORMAL cached path — `has_keyword` on the
/// object's cached keyword set — and never calls `mark_full()`/`evaluate_layers`
/// itself (unlike the `has_kw` helper above, which masked the bug by forcing a
/// recompute before every read). The turn boundary is crossed via the real
/// `advance_to_phase` turn machinery (production `auto_advance` + priority
/// passing), so the only thing that can strip the stale hexproof is the fix:
/// the ledger clear routing through `layers_dirty.mark_full()`.
///
/// Pre-fix: the final assertion fails (stale cache keeps hexproof).
/// Post-fix: the ledger clear invalidates layers, the next-turn SBA recomputes,
/// and hexproof is correctly gone.
#[test]
fn kid_loki_hexproof_expires_when_counter_history_clears_next_turn() {
    let mut scenario = GameScenario::new();
    // Start in the post-combat main phase so the turn can advance to End/Cleanup
    // and across the boundary without surfacing the `DeclareAttackers` prompt the
    // priority-pass helpers can't clear (combat is already behind us this turn).
    scenario.at_phase(Phase::PostCombatMain);

    let _kid_loki = scenario
        .add_creature_from_oracle(P0, "Kid Loki", 1, 4, KID_LOKI)
        .id();
    let buffed = scenario.add_creature(P0, "Buffed Bear", 2, 2).id();

    let mut runner = scenario.build();

    // P0 puts a +1/+1 counter on `buffed` this turn (CR 122.6 history record).
    put_counter(&mut runner, P0, buffed);

    // Drive the production turn machinery to the end step. `auto_advance` +
    // priority passing runs SBA, which flushes the (counter-dirtied) layers and
    // installs hexproof in `buffed`'s cached keyword set — exactly as production
    // does. Read through the cache directly (no manual recompute).
    runner.advance_to_end_step();
    assert!(
        has_keyword(&runner.state().objects[&buffed], &Keyword::Hexproof),
        "CR 122.6: while the counter-placement history holds this turn, the creature has Kid Loki's hexproof"
    );

    // Cross the turn boundary via the real turn-advance primitive: End → Cleanup
    // → next turn's Untap → Upkeep. `start_next_turn` clears the
    // `counter_added_this_turn` ledger, so the `CountersPutOnThisTurn` predicate
    // no longer matches (the +1/+1 counter itself remains on the creature, but
    // it was not placed THIS — the new — turn).
    runner.advance_to_upkeep();
    assert_ne!(
        runner.state().active_player,
        P0,
        "sanity: the turn boundary was crossed into the next player's turn"
    );

    // The discriminating assertion: hexproof is gone, read through the NORMAL
    // cached path. Pre-fix the cache is stale (no invalidation at the ledger
    // clear) and this fails; post-fix the boundary invalidation forces the
    // next-turn SBA to recompute and strip hexproof.
    assert!(
        !has_keyword(&runner.state().objects[&buffed], &Keyword::Hexproof),
        "CR 514.2: once the turn ends the counter-this-turn history clears, so hexproof must expire — \
         the keyword cache must not stay stale across the turn boundary"
    );
}
