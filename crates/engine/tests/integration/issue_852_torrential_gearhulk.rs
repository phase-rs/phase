//! Issue #852 — Torrential Gearhulk must allow selecting and casting a graveyard instant.
//!
//! https://github.com/phase-rs/phase/issues/852

use engine::game::scenario::{GameScenario, P0};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const GEARHULK_ORACLE: &str = "Flash\n\
When this creature enters, you may cast target instant card from your graveyard without paying its mana cost. If that spell would be put into your graveyard, exile it instead.";

const OPT_ORACLE: &str = "Draw a card.";

fn floating_mana(n: usize, ty: ManaType) -> Vec<ManaUnit> {
    (0..n)
        .map(|_| ManaUnit::new(ty, ObjectId(0), false, vec![]))
        .collect()
}

#[test]
fn torrential_gearhulk_etb_casts_graveyard_instant_without_mana_cost() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(
        P0,
        floating_mana(4, ManaType::Colorless)
            .into_iter()
            .chain(floating_mana(2, ManaType::Blue))
            .collect(),
    );

    let opt = scenario
        .add_spell_to_graveyard(P0, "Opt", true)
        .from_oracle_text(OPT_ORACLE)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Blue],
            generic: 0,
        })
        .id();

    let _dispel = scenario
        .add_spell_to_graveyard(P0, "Dispel", true)
        .from_oracle_text("Counter target spell.")
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Blue],
            generic: 0,
        })
        .id();

    let gearhulk = scenario
        .add_creature_to_hand_from_oracle(P0, "Torrential Gearhulk", 5, 6, GEARHULK_ORACLE)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Blue, ManaCostShard::Blue],
            generic: 4,
        })
        .id();

    let mut runner = scenario.build();

    assert!(
        !runner.state().objects[&gearhulk].trigger_definitions[0]
            .definition
            .optional,
        "Gearhulk ETB must be mandatory on stack when targets exist"
    );

    runner.cast(gearhulk).commit();
    runner.advance_until_stack_empty();

    let legal = match &runner.state().waiting_for {
        WaitingFor::TriggerTargetSelection { target_slots, .. } => target_slots
            .iter()
            .flat_map(|slot| slot.legal_targets.iter())
            .cloned()
            .collect::<Vec<_>>(),
        other => panic!("expected TriggerTargetSelection for Gearhulk ETB, got {other:?}"),
    };
    assert!(
        legal.contains(&TargetRef::Object(opt)),
        "Opt in graveyard must be a legal Gearhulk target; legal={legal:?}"
    );

    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(opt)),
        })
        .expect("select graveyard instant for ETB trigger");

    runner.resolve_top();

    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::OptionalEffectChoice { .. }
        ),
        "after targeting, optional cast prompt must appear; got {:?}",
        runner.state().waiting_for
    );

    runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .expect("accept free cast");

    assert_eq!(
        runner.state().objects[&opt].zone,
        Zone::Stack,
        "accepting must cast the targeted instant during resolution; waiting_for={:?}",
        runner.state().waiting_for
    );
}
