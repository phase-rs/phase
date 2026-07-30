//! GitHub issue #3466 — Counter spells with non-mana "unless" costs must not
//! silently parse as unconditional counters.
//!
//! Dash Hopes ("Counter target spell unless its controller pays 5 life.") and
//! similar cards must carry `unless_pay` with `PayLife` / `Sacrifice` / `Discard`
//! (CR 118.12 / CR 119.4 / CR 608.2c), and deck validation must not mark them
//! fully supported when the unless clause is dropped.

use engine::game::coverage::card_face_gaps;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{AbilityCost, AbilityKind, Effect, QuantityExpr, TargetFilter};
use engine::types::actions::GameAction;
use engine::types::card::CardFace;
use engine::types::card_type::CoreType;
use engine::types::game_state::{StackEntry, StackEntryKind, WaitingFor};
use engine::types::identifiers::CardId;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const DASH_HOPES: &str = "Counter target spell unless its controller pays 5 life.";
const MANA_LEAK: &str = "Counter target spell unless its controller pays {3}.";
const COUNTER_SACRIFICE: &str = "Counter target spell unless its controller sacrifices a creature.";
const COUNTER_DISCARD: &str = "Counter target spell unless its controller discards a card.";

fn card_face(name: &str, oracle: &str) -> CardFace {
    let parsed = parse_oracle_text(oracle, name, &[], &["Instant".to_string()], &[]);
    CardFace {
        name: name.to_string(),
        oracle_text: Some(oracle.to_string()),
        abilities: parsed.abilities,
        triggers: parsed.triggers,
        static_abilities: parsed.statics,
        replacements: parsed.replacements,
        ..Default::default()
    }
}

fn spell_ability(name: &str, oracle: &str) -> engine::types::ability::AbilityDefinition {
    let parsed = parse_oracle_text(oracle, name, &[], &["Instant".to_string()], &[]);
    parsed
        .abilities
        .into_iter()
        .find(|a| a.kind == AbilityKind::Spell)
        .expect("spell ability")
}

fn put_instant_on_stack(
    runner: &mut engine::game::scenario::GameRunner,
    controller: engine::types::player::PlayerId,
) -> engine::types::identifiers::ObjectId {
    let spell = engine::game::zones::create_object(
        runner.state_mut(),
        CardId(901),
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
            card_id: CardId(901),
            ability: None,
            casting_variant: engine::types::game_state::CastingVariant::Normal,
            actual_mana_spent: 0,
        },
    });
    spell
}

#[test]
fn dash_hopes_parses_conditional_counter_with_pay_life_unless() {
    let ability = spell_ability("Dash Hopes", DASH_HOPES);
    assert!(
        matches!(ability.effect.as_ref(), Effect::Counter { .. }),
        "expected Counter, got {:?}",
        ability.effect
    );
    let unless_pay = ability
        .unless_pay
        .as_ref()
        .expect("Dash Hopes must carry unless_pay, not an unconditional counter");
    assert_eq!(unless_pay.payer, TargetFilter::ParentTargetController);
    assert_eq!(
        unless_pay.cost,
        AbilityCost::PayLife {
            amount: QuantityExpr::Fixed { value: 5 }
        }
    );
}

#[test]
fn counter_unless_non_mana_costs_have_no_coverage_gaps() {
    for (name, oracle) in [
        ("Dash Hopes", DASH_HOPES),
        ("Counter-Sacrifice", COUNTER_SACRIFICE),
        ("Counter-Discard", COUNTER_DISCARD),
    ] {
        let gaps = card_face_gaps(&card_face(name, oracle));
        assert!(
            gaps.is_empty(),
            "{name} should report no face gaps when unless_pay is present, got {gaps:?}"
        );
    }
}

#[test]
fn mana_leak_control_has_unless_pay_for_regression() {
    let ability = spell_ability("Mana Leak", MANA_LEAK);
    assert!(
        ability.unless_pay.is_some(),
        "Mana Leak control must keep unless_pay"
    );
}

#[test]
fn dash_hopes_prompts_life_payment_then_counters_on_decline() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_life(P1, 20);
    let mut dash = scenario.add_spell_to_hand_from_oracle(P0, "Dash Hopes", true, DASH_HOPES);
    dash.with_mana_cost(ManaCost::Cost {
        generic: 0,
        shards: vec![ManaCostShard::Black, ManaCostShard::Black],
    });
    let dash_hopes = dash.id();
    scenario.add_basic_land(P0, ManaColor::Black);
    scenario.add_basic_land(P0, ManaColor::Black);

    let mut runner = scenario.build();
    let opponent_spell = put_instant_on_stack(&mut runner, P1);

    runner
        .cast(dash_hopes)
        .target_objects(&[opponent_spell])
        .resolve();

    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::UnlessPayment { player: P1, .. }
        ),
        "Dash Hopes must prompt P1 to pay 5 life, got {:?}",
        runner.state().waiting_for
    );

    runner
        .act(GameAction::PayUnlessCost { pay: false })
        .expect("P1 declines to pay life");

    assert!(
        runner.state().stack.is_empty(),
        "declining life payment must counter the targeted spell"
    );
    assert_eq!(
        runner.state().objects.get(&opponent_spell).map(|o| o.zone),
        Some(Zone::Graveyard)
    );
}

#[test]
fn dash_hopes_paying_life_leaves_target_spell_on_stack() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_life(P1, 20);
    let mut dash = scenario.add_spell_to_hand_from_oracle(P0, "Dash Hopes", true, DASH_HOPES);
    dash.with_mana_cost(ManaCost::Cost {
        generic: 0,
        shards: vec![ManaCostShard::Black, ManaCostShard::Black],
    });
    let dash_hopes = dash.id();
    scenario.add_basic_land(P0, ManaColor::Black);
    scenario.add_basic_land(P0, ManaColor::Black);

    let mut runner = scenario.build();
    let opponent_spell = put_instant_on_stack(&mut runner, P1);
    let life_before = runner.state().players[P1.0 as usize].life;

    runner
        .cast(dash_hopes)
        .target_objects(&[opponent_spell])
        .resolve();

    runner
        .act(GameAction::PayUnlessCost { pay: true })
        .expect("P1 pays 5 life");

    assert_eq!(
        runner.state().players[P1.0 as usize].life,
        life_before - 5,
        "paying the unless cost must deduct 5 life (CR 119.4)"
    );
    assert!(
        runner.state().stack.iter().any(|e| e.id == opponent_spell),
        "target spell must remain on stack when unless cost is paid"
    );
    assert_eq!(
        runner.state().objects.get(&opponent_spell).map(|o| o.zone),
        Some(Zone::Stack)
    );
}
