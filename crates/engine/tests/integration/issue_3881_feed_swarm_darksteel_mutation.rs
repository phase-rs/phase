//! Regression for issue #3881: Feed the Swarm must be able to destroy an
//! opponent's Darksteel Mutation aura (even when its host is indestructible).
//!
//! https://github.com/phase-rs/phase/issues/3881

use engine::game::game_object::AttachTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::card_type::CoreType;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const FEED_THE_SWARM_ORACLE: &str =
    "Destroy target creature or enchantment an opponent controls. You lose life equal to that permanent's mana value.";

fn floating_mana(n: usize, ty: ManaType) -> Vec<ManaUnit> {
    (0..n)
        .map(|_| ManaUnit::new(ty, ObjectId(0), false, vec![]))
        .collect()
}

fn attach_mutation(runner: &mut GameRunner, host: ObjectId) -> ObjectId {
    let state = runner.state_mut();
    let aura = engine::game::zones::create_object(
        state,
        engine::types::identifiers::CardId(3881),
        P1,
        "Darksteel Mutation".to_string(),
        Zone::Battlefield,
    );
    {
        let aura_obj = state.objects.get_mut(&aura).unwrap();
        aura_obj.card_types.core_types = vec![CoreType::Enchantment];
        aura_obj.card_types.subtypes = vec!["Aura".to_string()];
        aura_obj.attached_to = Some(AttachTarget::Object(host));
    }
    {
        state.objects.get_mut(&host).unwrap().attachments.push(aura);
    }
    aura
}

#[test]
fn feed_the_swarm_destroys_darksteel_mutation_aura() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let host = scenario.add_creature(P1, "Host Creature", 2, 2).id();
    let feed = scenario
        .add_spell_to_hand_from_oracle(P0, "Feed the Swarm", false, FEED_THE_SWARM_ORACLE)
        .id();
    scenario.with_mana_pool(P0, floating_mana(2, ManaType::Black));

    let mut runner = scenario.build();
    let mutation = attach_mutation(&mut runner, host);

    let outcome = runner.cast(feed).target_objects(&[mutation]).resolve();
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "Feed the Swarm should finish resolving"
    );

    assert_eq!(
        runner.state().objects[&mutation].zone,
        Zone::Graveyard,
        "Feed the Swarm must destroy the aura, not fail on the indestructible host"
    );
}
