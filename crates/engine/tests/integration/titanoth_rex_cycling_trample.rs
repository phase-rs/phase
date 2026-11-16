//! CR 702.29a/c: cycling discards as a cost; its trigger resolves before the draw.
use engine::game::keywords::object_has_effective_keyword_kind;
use engine::game::scenario::{GameScenario, P0};
use engine::types::ability::{AbilityTag, TargetRef};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::KeywordKind;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const ORACLE: &str = "Trample\nCycling {1}{G} ({1}{G}, Discard this card: Draw a card.)\nWhen you cycle this card, put a trample counter on target creature you control.";

#[test]
fn titanoth_rex_cycling_grants_trample_counter_before_draw() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Cycling Draw"]);
    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(ManaType::Green, ObjectId(9998), false, vec![]),
            ManaUnit::new(ManaType::Colorless, ObjectId(9999), false, vec![]),
        ],
    );
    let target = scenario.add_vanilla(P0, 2, 2);
    let rex = scenario
        .add_spell_to_hand_from_oracle(P0, "Titanoth Rex", false, ORACLE)
        .id();
    let mut runner = scenario.build();
    assert!(!object_has_effective_keyword_kind(
        runner.state(),
        target,
        KeywordKind::Trample
    ));
    let index = runner.state().objects[&rex]
        .abilities
        .iter()
        .position(|ability| ability.ability_tag == Some(AbilityTag::Cycling))
        .expect("cycling ability");
    let hand_before = runner.state().players[0].hand.len();
    runner
        .act(GameAction::ActivateAbility {
            source_id: rex,
            ability_index: index,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("pay cycling cost");
    assert_eq!(runner.state().objects[&rex].zone, Zone::Graveyard);
    // The cycling trigger's only legal target is the creature, so it is chosen
    // without a prompt; the trigger sits above the cycling draw (CR 702.29c).
    assert_eq!(
        runner.state().stack.len(),
        2,
        "draw ability and cycling trigger"
    );
    let trigger = runner.state().stack.back().expect("cycling trigger");
    assert_eq!(
        trigger.ability().map(|ability| ability.targets.clone()),
        Some(vec![TargetRef::Object(target)])
    );
    runner.resolve_top();
    // CR 122.1b: a trample counter grants trample, before cycling's draw resolves.
    assert_eq!(
        runner.state().objects[&target]
            .counters
            .get(&CounterType::Keyword(KeywordKind::Trample)),
        Some(&1)
    );
    assert!(object_has_effective_keyword_kind(
        runner.state(),
        target,
        KeywordKind::Trample
    ));
    assert_eq!(runner.state().players[0].hand.len(), hand_before - 1);
    assert_eq!(
        runner.state().stack.len(),
        1,
        "cycling draw is still pending"
    );
    runner.advance_until_stack_empty();
    assert_eq!(runner.state().players[0].hand.len(), hand_before);
    assert!(runner.state().stack.is_empty());
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
}
