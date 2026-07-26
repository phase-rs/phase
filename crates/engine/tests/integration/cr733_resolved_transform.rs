//! CR733 P2 coverage for the transform family.
//!
//! `transform::transform_permanent` is the single authority every production
//! transform funnels through (the `Transform` effect, day/night shifts, meld,
//! copy-as-transformed, and the zone delivery tail's `enter_transformed`), but
//! it wrote its mutation raw. A retained-prefix replay therefore had no record
//! that the permanent had turned to its other face.
//!
//! CR 613.7g is why this cannot ride the boolean object-status family: a
//! transformed permanent receives a NEW timestamp drawn from
//! `GameState::next_timestamp`, which orders it against continuous effects. A
//! replay that re-draws that counter installs a different number and silently
//! reorders layer application, so the drawn value must be recorded and
//! reinstalled verbatim.
//!
//! The test drives the REAL pipeline — casting a spell whose effect is
//! `Effect::Transform` at a double-faced permanent — so the transform is
//! produced by the production resolver, not by a direct call to the authority.

use engine::game::printed_cards::snapshot_object_face;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::ability::{Effect, TargetFilter, TypedFilter};
use engine::types::phase::Phase;
use engine::types::resolved_commands::ResolvedRulesCommand;

#[test]
fn transform_journals_an_exact_resolved_transform() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let dfc = scenario.add_creature(P1, "Sleeping Bear", 2, 2).id();
    // The donor supplies the back face. Snapshotting a real object's face is
    // what `transform_permanent` itself stores, so the fixture stays correct as
    // `BackFaceData` gains fields — a hand-written struct literal would break on
    // every new field.
    let donor = scenario.add_creature(P1, "Awakened Bear", 4, 4).id();
    let mut spell = scenario.add_spell_to_hand(P0, "Rouse", true);
    spell.with_ability(Effect::Transform {
        target: TargetFilter::Typed(TypedFilter::creature()),
        scope: engine::types::ability::EffectScope::Single,
    });
    let spell_id = spell.id();

    let mut runner = scenario.build();
    let back_face = snapshot_object_face(&runner.state().objects[&donor]);
    runner
        .state_mut()
        .objects
        .get_mut(&dfc)
        .expect("the double-faced permanent exists")
        .back_face = Some(back_face);

    // Captured before the cast so the recorded command can be replayed against
    // the exact predecessor state it was resolved from.
    let pre_state = runner.state().clone();
    let journal_start = runner.state().resolved_rules_journal.entries().len();

    let outcome = runner.cast(spell_id).target_object(dfc).resolve();
    let state = outcome.state();

    // CR 701.27a: the permanent turned to its other face. Without this reach
    // guard the journal assertion below could pass vacuously on a transform
    // that never happened.
    let object = &state.objects[&dfc];
    assert!(
        object.transformed,
        "CR 701.27a: the Transform effect must turn the permanent to its other face"
    );
    assert_eq!(
        object.name, "Awakened Bear",
        "the back face is now the displayed face"
    );

    // The discriminating assertion: the transform is journaled as an exact
    // resolved command. A raw mutation records nothing here.
    let transforms: Vec<_> = state
        .resolved_rules_journal
        .entries()
        .iter()
        .skip(journal_start)
        .filter_map(|entry| entry.command.clone())
        .filter_map(|command| match command {
            ResolvedRulesCommand::ObjectTransform(command) if command.object.object_id == dfc => {
                Some(command)
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        transforms.len(),
        1,
        "the transform authority must journal exactly one resolved transform"
    );

    let transform = &transforms[0];
    assert!(
        !transform.expected_old_transformed && transform.resulting_transformed,
        "CR 701.27a: the recorded transition is front face -> back face"
    );
    // CR 613.7g: the recorded timestamp is the one the permanent actually
    // received, not a value re-derived at replay time.
    assert_eq!(
        transform.resulting_timestamp, object.timestamp,
        "the journaled timestamp is the timestamp the transform installed"
    );
    assert_eq!(
        transform.resulting_transformation_count, object.transformation_count,
        "CR 701.27f: the journaled transformation count matches the installed count"
    );

    // Replay-exactness: applying the recorded command to the pre-cast state
    // reproduces the same face, timestamp, and count with no re-derivation —
    // in particular without drawing a fresh timestamp from `next_timestamp`.
    let mut replay = pre_state;
    engine::game::transform::apply_resolved_transform(&mut replay, transform)
        .expect("the recorded transform must replay against its captured predecessor");
    let replayed = &replay.objects[&dfc];
    assert!(replayed.transformed, "replay installs the back face");
    assert_eq!(
        replayed.name, "Awakened Bear",
        "replay installs the exact displayed face"
    );
    assert_eq!(
        replayed.timestamp, transform.resulting_timestamp,
        "CR 613.7g: replay installs the recorded timestamp instead of re-drawing one"
    );

    // CR 613.7: installing a recorded timestamp is only half the contract — the
    // allocator must also be carried past it. `next_timestamp` is the draw
    // counter, so an applier that installs 42 while leaving the counter at 5
    // hands 42 out a second time later in the replay, and CR 613.7 orders
    // effects within a layer by timestamp alone, leaving the two unordered.
    // Asserted by DRAWING rather than by reading the counter, so this pins the
    // observable consequence and not the field.
    let next_drawn = replay.next_timestamp();
    assert!(
        next_drawn > transform.resulting_timestamp,
        "CR 613.7: replay installed timestamp {} but the next draw handed out {}; \
         two objects sharing a timestamp are unordered within their layer",
        transform.resulting_timestamp,
        next_drawn
    );
}
