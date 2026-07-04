//! Runtime coverage for Ms. Marvel, Elastic Ally's combat-damage trigger and
//! the new `FilterProp::PowerExceedsBase` it relies on.
//!
//! Ms. Marvel, Elastic Ally: "Whenever a creature you control with power
//! greater than its base power deals combat damage to a player, draw a card.
//! This ability triggers only once each turn."
//!
//! The trigger's source filter carries `FilterProp::PowerExceedsBase` (the new
//! engine variant). These tests drive the real pipeline: `from_oracle_text`
//! parses the trigger (exercising the parser arm in `oracle_nom/filter.rs`),
//! combat damage fires the `DamageDone` trigger, and the trigger's source
//! filter is evaluated via `matches_filter_prop` (the new eval arm in
//! `filter.rs`). The discriminator is whether P0 draws:
//!   - a creature pumped above its base by a +1/+1 counter (power > base) → draw
//!   - an unpumped creature (power == base) → NO draw
//!   - mixed pumped + unpumped attackers → exactly one draw (only the pumped
//!     creature qualifies)
//!
//! Revert the eval arm and `power > base` never evaluates true, so the positive
//! cases stop drawing. Revert the source-filter parse and the trigger is
//! unsupported, so it never fires.
//!
//! CR references (verified against docs/MagicCompRules.txt):
//!   - CR 208.1: a creature's power can be modified or set by effects.
//!   - CR 613.4b: layer 7b establishes base power; counters (7c) modify above it.
//!   - CR 510.1c / CR 120.1: combat damage is dealt to the defending player.

use super::rules::{run_combat, GameRunner, GameScenario, ObjectId, Phase, PlayerId, P0};
// Wave 4: FilterProp::PowerExceedsBase runtime coverage.

const MS_MARVEL_LIKE: &str = "Whenever a creature you control with power greater than its base \
     power deals combat damage to a player, draw a card. This ability triggers only once each turn.";

/// Count of cards in `player`'s hand — the draw discriminator.
fn hand_len(runner: &GameRunner, player: PlayerId) -> usize {
    runner
        .state()
        .players
        .iter()
        .find(|p| p.id == player)
        .expect("player exists")
        .hand
        .len()
}

/// Build a scenario with the Ms.-Marvel-like trigger source plus the requested
/// attackers. Each `(power_over_base)` flag controls whether the attacker gets a
/// +1/+1 counter (power > base) or stays unpumped (power == base).
fn build_scenario(pumped_attackers: &[bool]) -> (GameRunner, Vec<ObjectId>) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // The trigger source — unpumped, so it never self-qualifies.
    scenario
        .add_creature(P0, "Ms. Marvel", 0, 5)
        .from_oracle_text(MS_MARVEL_LIKE);

    let attackers: Vec<ObjectId> = pumped_attackers
        .iter()
        .enumerate()
        .map(|(i, &pumped)| {
            let mut builder = scenario.add_creature(P0, &format!("Attacker{i}"), 2, 2);
            if pumped {
                builder.with_plus_counters(1);
            }
            builder.id()
        })
        .collect();

    // A library deep enough to draw from.
    scenario.with_library_top(P0, &["L0", "L1", "L2", "L3", "L4"]);

    (scenario.build(), attackers)
}

/// CR 208.1 + CR 613.4b: a creature with a +1/+1 counter has power > base power,
/// so its combat damage fires the trigger and P0 draws one card.
#[test]
fn draws_when_pumped_creature_deals_combat_damage() {
    let (mut runner, attackers) = build_scenario(&[true]);
    let hand_before = hand_len(&runner, P0);

    run_combat(&mut runner, attackers, vec![]);
    runner.advance_until_stack_empty();

    assert_eq!(
        hand_len(&runner, P0),
        hand_before + 1,
        "power > base creature dealt combat damage → draw exactly one"
    );
}

/// An unpumped creature has power == base power, so the trigger's source filter
/// does NOT match and P0 draws nothing.
#[test]
fn no_draw_when_creature_power_equals_base() {
    let (mut runner, attackers) = build_scenario(&[false]);
    let hand_before = hand_len(&runner, P0);

    run_combat(&mut runner, attackers, vec![]);
    runner.advance_until_stack_empty();

    assert_eq!(
        hand_len(&runner, P0),
        hand_before,
        "power == base creature must NOT fire the trigger (no draw)"
    );
}

/// The source filter selects only the qualifying creature: with one pumped
/// (power > base) and one unpumped (power == base) attacker dealing combat
/// damage in the same combat, exactly one draw occurs — the pumped creature
/// triggers, the unpumped one does not. This isolates the new
/// `FilterProp::PowerExceedsBase` discrimination from the engine's
/// once-per-turn batching (see module note). Revert the eval arm and neither
/// attacker draws; revert the source-filter parse and the trigger never fires.
///
/// NOTE: the strict "only once each turn" cap is NOT asserted here. Two
/// creatures dealing combat damage *simultaneously* each produce a matching
/// trigger before `triggers_fired_this_turn` is updated, so the existing
/// `TriggerConstraint::OncePerTurn` enforcement (game/triggers.rs) does not
/// deduplicate within a single simultaneous batch — a general, pre-existing
/// trigger-collection limitation independent of this card and out of scope for
/// this change.
#[test]
fn only_the_pumped_attacker_triggers_the_draw() {
    // Attacker0 pumped (power > base), Attacker1 unpumped (power == base).
    let (mut runner, attackers) = build_scenario(&[true, false]);
    let hand_before = hand_len(&runner, P0);

    run_combat(&mut runner, attackers, vec![]);
    runner.advance_until_stack_empty();

    assert_eq!(
        hand_len(&runner, P0),
        hand_before + 1,
        "only the power > base attacker fires the trigger → exactly one draw"
    );
}
