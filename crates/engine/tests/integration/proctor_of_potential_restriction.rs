//! Proctor of Potential (FRA): "{W}{U}: Return this card from your graveyard
//! to the battlefield with a finality counter on it. Activate only if you've
//! scried or surveilled this turn." Exercises the `StaticCondition::Or`
//! verb-list parse end to end through `ActivationRestriction::RequiresCondition`
//! (`can_activate_ability_now`), not just that it parses.

use engine::ai_support::legal_actions;
use engine::game::scenario::{GameScenario, P0};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const PROCTOR_ORACLE: &str = "Whenever this creature or another creature you control enters, surveil 1. (Look at the top card of your library. You may put it into your graveyard.)\n{W}{U}: Return this card from your graveyard to the battlefield with a finality counter on it. Activate only if you've scried or surveilled this turn. (If a creature with a finality counter on it would die, exile it instead.)";

fn floating(mana: &[ManaType]) -> Vec<ManaUnit> {
    mana.iter()
        .map(|t| ManaUnit::new(*t, ObjectId(0), false, vec![]))
        .collect()
}

fn offers_activation(state: &engine::types::game_state::GameState, object: ObjectId) -> bool {
    legal_actions(state).iter().any(|action| {
        matches!(action, GameAction::ActivateAbility { source_id, .. } if *source_id == object)
    })
}

fn proctor_ability_index(state: &engine::types::game_state::GameState, proctor: ObjectId) -> usize {
    state.objects[&proctor]
        .abilities
        .iter()
        .position(|ability| {
            matches!(
                *ability.effect,
                engine::types::ability::Effect::ChangeZone {
                    origin: Some(Zone::Graveyard),
                    destination: Zone::Battlefield,
                    ..
                }
            )
        })
        .expect("Proctor must have a Graveyard -> Battlefield return ability")
}

/// With no scry or surveil this turn, the activation must be refused and
/// the card must stay in the graveyard.
#[test]
fn proctor_activation_refused_with_no_scry_or_surveil() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let proctor = scenario
        .add_creature_to_graveyard(P0, "Proctor of Potential", 3, 1)
        .from_oracle_text(PROCTOR_ORACLE)
        .id();
    scenario.with_mana_pool(P0, floating(&[ManaType::White, ManaType::Blue]));
    let mut runner = scenario.build();

    let ability_index = proctor_ability_index(runner.state(), proctor);
    assert!(
        !offers_activation(runner.state(), proctor),
        "the return ability must not be offered with no scry/surveil this turn"
    );
    assert!(
        runner
            .act(GameAction::ActivateAbility {
                source_id: proctor,
                ability_index,
            })
            .is_err(),
        "the submit path must reject the activation too"
    );
    assert_eq!(runner.state().objects[&proctor].zone, Zone::Graveyard);
}

/// After `Surveil 1.` resolves this turn, the activation resolves and the
/// card returns with a finality counter.
#[test]
fn proctor_activation_resolves_after_surveil() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_card_to_library_top(P0, "Library Card");
    let proctor = scenario
        .add_creature_to_graveyard(P0, "Proctor of Potential", 3, 1)
        .from_oracle_text(PROCTOR_ORACLE)
        .id();
    let surveil_spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Test Surveil Spell", true, "Surveil 1.")
        .id();
    scenario.with_mana_pool(P0, floating(&[ManaType::White, ManaType::Blue]));
    let mut runner = scenario.build();
    runner.cast(surveil_spell).resolve();

    let ability_index = proctor_ability_index(runner.state(), proctor);
    assert!(
        offers_activation(runner.state(), proctor),
        "the return ability must be offered after surveilling this turn"
    );
    let outcome = runner.activate(proctor, ability_index).resolve();
    outcome.assert_zone(&[proctor], Zone::Battlefield);
    assert_eq!(
        *outcome.state().objects[&proctor]
            .counters
            .get(&CounterType::Finality)
            .unwrap_or(&0),
        1,
        "the returned card must carry a finality counter"
    );
}

/// The same, but the condition is satisfied by `Scry 1.` instead.
#[test]
fn proctor_activation_resolves_after_scry() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_card_to_library_top(P0, "Library Card");
    let proctor = scenario
        .add_creature_to_graveyard(P0, "Proctor of Potential", 3, 1)
        .from_oracle_text(PROCTOR_ORACLE)
        .id();
    let scry_spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Test Scry Spell", true, "Scry 1.")
        .id();
    scenario.with_mana_pool(P0, floating(&[ManaType::White, ManaType::Blue]));
    let mut runner = scenario.build();
    runner.cast(scry_spell).resolve();

    let ability_index = proctor_ability_index(runner.state(), proctor);
    assert!(
        offers_activation(runner.state(), proctor),
        "the return ability must be offered after scrying this turn"
    );
    let outcome = runner.activate(proctor, ability_index).resolve();
    outcome.assert_zone(&[proctor], Zone::Battlefield);
    assert_eq!(
        *outcome.state().objects[&proctor]
            .counters
            .get(&CounterType::Finality)
            .unwrap_or(&0),
        1,
        "the returned card must carry a finality counter"
    );
}
