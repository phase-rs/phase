//! CR 702.165a — "becomes the target of a backup ability" trigger (Huge Truck class).
//!
//! A creature with "Whenever another creature you control becomes the target of a
//! backup ability, draw a card." must fire when one of its controller's other
//! creatures is targeted by a Backup keyword ability specifically — and must NOT
//! fire when targeted by an unrelated (non-backup) spell or ability.
//!
//! The synthesized Backup ETB ability (`synthesize_backup`) is stamped with
//! `AbilityTag::Backup`; the trigger's `valid_source` is
//! `TargetFilter::StackAbility { tag: Some(AbilityTag::Backup) }`, honored at
//! runtime by `stack_ability_matches_filter` (CR 113.7a — the ability exists on
//! the stack independently of its source, so the tag is read off the resolved
//! ability). The backup ETB targets `target creature` (CR 115.1d), so locking
//! that target emits `GameEvent::BecomesTarget` (CR 603.3d).

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;

/// Huge-Truck-style payoff: cares that another of P0's creatures was targeted by
/// a backup ability.
const HUGE_TRUCK_TRIGGER: &str =
    "Whenever another creature you control becomes the target of a backup ability, draw a card.";

/// Minimal Backup 1 creature — synthesizes the tagged Backup ETB.
const BACKUP_ONE: &str = "Backup 1";

fn generic_pool(count: usize) -> Vec<ManaUnit> {
    (0..count)
        .map(|_| ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]))
        .collect()
}

/// Positive: a Backup 1 creature enters and targets the payoff creature → the
/// becomes-target-of-a-backup-ability trigger fires exactly once (one card drawn),
/// and a `BecomesTarget` event is observed (CR 603.3d, Phase B4 verify-only).
#[test]
fn backup_ability_target_fires_becomes_target_trigger() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // P0's payoff creature on the battlefield (the trigger source). Its id is not
    // needed — the backup targets a different creature so this one's "another
    // creature you control" trigger fires.
    scenario.add_creature_from_oracle(P0, "Huge Truck", 4, 4, HUGE_TRUCK_TRIGGER);

    // P0's Backup 1 creature in hand — its ETB targets a creature with a tagged
    // backup ability.
    let backup = scenario
        .add_creature_to_hand_from_oracle(P0, "Backup Bear", 2, 2, BACKUP_ONE)
        .id();

    // A card to draw when the trigger resolves.
    scenario.with_library_top(P0, &["Forest"]);
    scenario.with_mana_pool(P0, generic_pool(4));

    let mut runner = scenario.build();

    // Cast the backup creature; its ETB targets a creature. The target must be a
    // creature OTHER than the payoff (Huge Truck), because the trigger fires on
    // "ANOTHER creature you control" — Huge Truck targeting itself would not count.
    // The Backup Bear targets itself: it is another creature P0 controls relative
    // to Huge Truck. When that target locks, `GameEvent::BecomesTarget` fires with
    // the backup ability as source, matching the payoff's
    // `valid_source = StackAbility { tag: Backup }`.
    let outcome = runner.cast(backup).target_object(backup).resolve();

    // The becomes-target trigger drew exactly one card. This assertion flips to 0
    // if the `AbilityTag::Backup` stamp, the `tag` filter, or the parser
    // `valid_source` wiring is reverted: the draw only happens when the backup
    // ETB's `GameEvent::BecomesTarget` (CR 603.3d, Phase B4) is routed through the
    // payoff's `valid_source = StackAbility { tag: Backup }`.
    assert_eq!(
        outcome.hand_drawn(P0),
        1,
        "the backup-ability target trigger must draw exactly one card"
    );
}

/// Negative: a plain (non-backup) spell/ability targeting the payoff creature must
/// NOT fire the backup-specific trigger. Lightning Bolt is an instant spell, not a
/// backup ability, so its `BecomesTarget` event does not match
/// `StackAbility { tag: Some(Backup) }`.
#[test]
fn non_backup_source_does_not_fire_backup_trigger() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let payoff = scenario
        .add_creature_from_oracle(P0, "Huge Truck", 4, 4, HUGE_TRUCK_TRIGGER)
        .id();
    // A second P0 creature so "another creature you control" is satisfiable.
    let _other = scenario.add_creature(P0, "Grizzly Bear", 2, 2).id();

    // P1 holds a Lightning Bolt to target the payoff creature.
    let bolt = scenario.add_bolt_to_hand(P1);
    scenario.with_library_top(P0, &["Forest"]);
    scenario.with_mana_pool(
        P1,
        vec![ManaUnit::new(ManaType::Red, ObjectId(0), false, vec![])],
    );

    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.active_player = P1;
        state.priority_player = P1;
        state.waiting_for = WaitingFor::Priority { player: P1 };
    }

    let hand_before = runner.state().players[P0.0 as usize].hand.len();

    let card_id = runner.state().objects[&bolt].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: bolt,
            card_id,
            targets: vec![],
        })
        .expect("casting Lightning Bolt should succeed");

    if matches!(
        runner.state().waiting_for,
        WaitingFor::TargetSelection { .. }
    ) {
        runner
            .act(GameAction::ChooseTarget {
                target: Some(TargetRef::Object(payoff)),
            })
            .expect("targeting the payoff creature should succeed");
    }

    runner.advance_until_stack_empty();

    // A non-backup source must not have drawn a card for P0.
    let hand_after = runner.state().players[P0.0 as usize].hand.len();
    assert_eq!(
        hand_after, hand_before,
        "a non-backup spell targeting the payoff creature must not fire the backup trigger"
    );
}
