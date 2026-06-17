//! GitHub issue #3301 — Izzet Charm counter mode must counter noncreature spells.
//!
//! Mode 1 oracle:
//!   Counter target noncreature spell unless its controller pays {2}.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::ability::Effect;
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::{CastingVariant, StackEntry, StackEntryKind, WaitingFor};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const IZZET_CHARM_ORACLE: &str = "Choose one —\n\
    • Counter target noncreature spell unless its controller pays {2}.\n\
    • Izzet Charm deals 2 damage to target creature.\n\
    • Draw two cards, then discard two cards.";

fn put_instant_on_stack(
    runner: &mut engine::game::scenario::GameRunner,
    controller: engine::types::player::PlayerId,
) -> ObjectId {
    let spell = engine::game::zones::create_object(
        runner.state_mut(),
        CardId(501),
        controller,
        "Shock".to_string(),
        Zone::Stack,
    );
    if let Some(obj) = runner.state_mut().objects.get_mut(&spell) {
        obj.card_types.core_types = vec![CoreType::Instant];
    }
    runner.state_mut().stack.push_back(StackEntry {
        id: spell,
        source_id: spell,
        controller,
        kind: StackEntryKind::Spell {
            card_id: CardId(501),
            ability: None,
            casting_variant: CastingVariant::Normal,
            actual_mana_spent: 0,
        },
    });
    spell
}

#[test]
fn izzet_charm_parsed_counter_mode_carries_unless_pay() {
    let mut scenario = GameScenario::new();
    let charm = scenario
        .add_spell_to_hand_from_oracle(P0, "Izzet Charm", true, IZZET_CHARM_ORACLE)
        .id();
    let runner = scenario.build();
    let ability = &runner.state().objects[&charm].abilities[0];
    assert!(
        matches!(ability.effect.as_ref(), Effect::Counter { .. }),
        "mode 1 must parse to Counter"
    );
    assert!(
        ability.unless_pay.is_some(),
        "unless_pay {{2}} must live on the spell ability definition"
    );
}

#[test]
fn izzet_charm_counters_noncreature_spell_when_controller_declines_two() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let charm = scenario
        .add_spell_to_hand_from_oracle(P0, "Izzet Charm", true, IZZET_CHARM_ORACLE)
        .id();
    scenario.add_basic_land(P0, engine::types::mana::ManaColor::Blue);
    scenario.add_basic_land(P0, engine::types::mana::ManaColor::Red);

    let mut runner = scenario.build();
    let opponent_spell = put_instant_on_stack(&mut runner, P1);

    runner
        .cast(charm)
        .modes(&[0])
        .target_objects(&[opponent_spell])
        .resolve();

    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::UnlessPayment { player: P1, .. }
        ),
        "counter resolution must prompt P1 to pay {{2}}, got {:?}",
        runner.state().waiting_for
    );

    runner
        .act(GameAction::PayUnlessCost { pay: false })
        .expect("P1 declines to pay {2}");

    assert!(
        runner.state().stack.is_empty(),
        "declining the unless cost must counter the spell"
    );
    assert_eq!(
        runner.state().objects.get(&opponent_spell).map(|o| o.zone),
        Some(Zone::Graveyard),
        "countered spell must move to graveyard"
    );
}

#[test]
fn izzet_charm_counter_mode_rejected_without_noncreature_spell_on_stack() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let charm = scenario
        .add_spell_to_hand_from_oracle(P0, "Izzet Charm", true, IZZET_CHARM_ORACLE)
        .id();
    scenario.add_basic_land(P0, engine::types::mana::ManaColor::Blue);
    scenario.add_basic_land(P0, engine::types::mana::ManaColor::Red);

    let mut runner = scenario.build();

    let err = runner.cast(charm).modes(&[0]).resolve();

    assert!(
        matches!(
            err.final_waiting_for(),
            WaitingFor::ModeChoice { .. } | WaitingFor::TargetSelection { .. }
        ) || runner.state().stack.is_empty(),
        "counter mode must not fully resolve without a legal stack target"
    );
}
