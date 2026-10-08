//! Regression for issue #5950 — Sensei Golden-Tail training grant wears off.
//!
//! Oracle: "{1}{W}, {T}: Put a training counter on target creature. That creature
//! gains bushido 1 and becomes a Samurai in addition to its other creature types.
//! Activate only as a sorcery."
//!
//! The parse is faithful, but an unstated `None` duration on the additive type grant
//! defaults to `UntilEndOfTurn` in `effect.rs::resolve` and is swept by
//! `prune_end_of_turn_effects` at cleanup — so the bushido + Samurai grant wrongly
//! "wears off" after one turn. The parser fix stamps `Duration::Permanent` (CR
//! 611.2a); the resolver and cleanup path already honor it.
//!
//! Drives the REAL parse → synthesis → activation → resolution → cleanup pipeline.
//! REVERT-PROBE: revert the `Duration::Permanent` stamp in
//! `build_continuous_clause` and both post-cleanup assertions fail.

use engine::game::keywords::has_keyword;
use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::AbilityKind;
use engine::types::ability::{ContinuousModification, Duration};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;

const SENSEI_ORACLE: &str = "Bushido 1 (Whenever this creature blocks or becomes blocked, it gets \
+1/+1 until end of turn.)\n\
{1}{W}, {T}: Put a training counter on target creature. That creature gains bushido 1 and \
becomes a Samurai in addition to its other creature types. Activate only as a sorcery.";

fn refresh(runner: &mut GameRunner) {
    runner.state_mut().layers_dirty.mark_full();
    evaluate_layers(runner.state_mut());
}

fn has_bushido(runner: &GameRunner, id: ObjectId) -> bool {
    has_keyword(&runner.state().objects[&id], &Keyword::Bushido(1))
}

fn has_samurai_subtype(runner: &GameRunner, id: ObjectId) -> bool {
    runner
        .state()
        .objects
        .get(&id)
        .is_some_and(|obj| obj.card_types.subtypes.iter().any(|s| s == "Samurai"))
}

fn training_ability_index(runner: &GameRunner, sensei: ObjectId) -> usize {
    runner.state().objects[&sensei]
        .abilities
        .iter()
        .position(|a| matches!(a.kind, AbilityKind::Activated))
        .expect("Sensei Golden-Tail must carry an activated training ability")
}

/// CR 611.2a + CR 514.2: activate Sensei Golden-Tail's training ability, cross
/// end-of-turn cleanup, and assert the bushido + Samurai grant persists on the
/// trainee.
#[test]
fn sensei_training_grant_survives_cleanup() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(ManaType::White, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
        ],
    );

    let sensei = scenario
        .add_creature_from_oracle(P0, "Sensei Golden-Tail", 3, 2, SENSEI_ORACLE)
        .id();
    let trainee = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();

    let mut runner = scenario.build();
    let ability_index = training_ability_index(&runner, sensei);

    runner
        .activate(sensei, ability_index)
        .target_object(trainee)
        .resolve();

    refresh(&mut runner);
    assert!(
        has_bushido(&runner, trainee),
        "the training grant must apply bushido 1 immediately after resolution"
    );
    assert!(
        has_samurai_subtype(&runner, trainee),
        "the training grant must add Samurai immediately after resolution"
    );
    assert!(
        runner
            .state()
            .transient_continuous_effects
            .iter()
            .any(|effect| {
                effect.duration == Duration::Permanent
                    && effect
                        .modifications
                        .iter()
                        .any(|m| matches!(m, ContinuousModification::AddKeyword { .. }))
                    && effect.modifications.iter().any(|m| {
                        matches!(
                            m,
                            ContinuousModification::AddSubtype { subtype } if subtype == "Samurai"
                        )
                    })
            }),
        "the training grant must register as a permanent transient continuous effect"
    );

    let activation_turn = runner.state().turn_number;
    runner.advance_to_end_step();
    runner.advance_to_phase(Phase::Upkeep);
    assert!(
        runner.state().turn_number > activation_turn,
        "the scenario must cross end-of-turn cleanup into the next turn"
    );

    refresh(&mut runner);
    assert!(
        has_bushido(&runner, trainee),
        "CR 611.2a: bushido 1 must survive cleanup when stamped Permanent"
    );
    assert!(
        has_samurai_subtype(&runner, trainee),
        "CR 205.1b + CR 611.2a: Samurai subtype must survive cleanup when stamped Permanent"
    );
}
