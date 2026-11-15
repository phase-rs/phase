//! CR733 P2 coverage for the zone delivery tail's enter-tapped mutation.
//!
//! The zone-change hub journals the transition core inside `move_to_zone`, but
//! the delivery tail's entry modifiers run *around* that core and were written
//! raw. This module covers the first of them: CR 614.1 enter-tapped, which now
//! routes through the single object-status authority
//! (`object_state::resolve_and_apply_object_edit`) so the entry tap is recorded
//! as an exact resolved command instead of a bare `obj.tapped = true`.
//!
//! The test drives the REAL pipeline — a tapland played from hand through
//! `GameAction::PlayLand` — so the tap is produced by the production delivery
//! tail, not by a direct call. It goes RED if the wire is reverted to the raw
//! write: the journal would contain no `ObjectStatus` command for the entry.

use engine::game::scenario::{GameScenario, P0};
use engine::types::ability::{
    AbilityDefinition, AbilityKind, Effect, EffectScope, ReplacementDefinition, TapStateChange,
    TargetFilter,
};
use engine::types::actions::GameAction;
use engine::types::phase::Phase;
use engine::types::replacements::ReplacementEvent;
use engine::types::resolved_commands::{ResolvedObjectStatus, ResolvedRulesCommand};
use engine::types::zones::Zone;

/// CR 614.1: "This land enters tapped." A `Moved` -> battlefield self
/// replacement whose execute is a single-target `Tap` — the canonical shape the
/// replacement pipeline folds into the proposed event's `enter_tapped` field,
/// which the delivery tail then applies.
fn enters_tapped_replacement() -> ReplacementDefinition {
    ReplacementDefinition::new(ReplacementEvent::Moved)
        .execute(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::SetTapState {
                target: TargetFilter::SelfRef,
                scope: EffectScope::Single,
                state: TapStateChange::Tap,
            },
        ))
        .valid_card(TargetFilter::SelfRef)
        .destination_zone(Zone::Battlefield)
        .description("This land enters tapped.".to_string())
}

#[test]
fn enter_tapped_delivery_tail_journals_an_exact_resolved_tap() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let mut builder = scenario.add_land_to_hand(P0, "Tapped Fen");
    builder.with_replacement_definition(enters_tapped_replacement());
    let land_id = builder.id();

    let mut runner = scenario.build();
    let journal_start = runner.state().resolved_rules_journal.entries().len();
    let card_id = runner.state().objects[&land_id].card_id;
    runner
        .act(GameAction::PlayLand {
            object_id: land_id,
            card_id,
        })
        .expect("play land should succeed");

    let state = runner.state();

    // CR 614.1: the land entered the battlefield, and the delivery tail applied
    // the enter-tapped modifier.
    let object = &state.objects[&land_id];
    assert_eq!(object.zone, Zone::Battlefield, "the land entered play");
    assert!(
        object.tapped,
        "CR 614.1: the delivery tail must apply the enter-tapped modifier"
    );

    // The discriminating assertion: the entry tap is journaled as an exact
    // object-status command. A raw `obj.tapped = true` records nothing here.
    let taps: Vec<_> = state
        .resolved_rules_journal
        .entries()
        .iter()
        .skip(journal_start)
        .filter_map(|entry| entry.command.clone())
        .filter_map(|command| match command {
            ResolvedRulesCommand::ObjectStatus(command)
                if command.object.object_id == land_id
                    && command.status == ResolvedObjectStatus::Tapped
                    && command.new =>
            {
                Some(command)
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        taps.len(),
        1,
        "the enter-tapped delivery-tail modifier must journal exactly one resolved tap"
    );

    // CR 701.26a: only an untapped permanent can be tapped, so the recorded
    // precondition must be the pre-entry untapped state — replay installs the
    // transition against that exact predecessor rather than re-deriving it.
    let tap = &taps[0];
    assert!(
        !tap.expected_old,
        "CR 701.26a: the entering permanent was untapped before the entry tap"
    );

    // Replay-exactness: the recorded command reinstalls the same transition on
    // a state rewound to the captured predecessor, with no re-derivation.
    let mut replay = state.clone();
    replay
        .objects
        .get_mut(&land_id)
        .expect("the land remains present in the cloned state")
        .tapped = tap.expected_old;
    engine::game::object_state::apply_resolved_object_edit(&mut replay, tap)
        .expect("the recorded entry tap must replay against its captured predecessor");
    assert!(
        replay.objects[&land_id].tapped,
        "replaying the recorded command reinstalls the exact entry tap"
    );
}
