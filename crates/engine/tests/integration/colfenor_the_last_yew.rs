//! Regression for Colfenor, the Last Yew's relative-toughness dies trigger.
//!
//! CR 603.10a + CR 608.2h: the trigger compares graveyard candidates with the
//! dying creature's last-known toughness, not the departed object's reset
//! graveyard characteristics. CR 208.1: the first printed characteristic is
//! power and the second is toughness.

use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const COLFENOR_ORACLE: &str =
    "Reach, vigilance\nWhenever Colfenor or another creature you control dies, return up to one other target creature card with lesser toughness from your graveyard to your hand.";
const MURDER_ORACLE: &str = "Destroy target creature.";

fn resolve_colfenor_trigger(
    runner: &mut GameRunner,
    expected_target: ObjectId,
    forbidden_target: ObjectId,
    equal_target: ObjectId,
) {
    for _ in 0..128 {
        match runner.state().waiting_for.clone() {
            WaitingFor::OrderTriggers { triggers, .. } => {
                runner
                    .act(GameAction::OrderTriggers {
                        order: (0..triggers.len()).collect(),
                    })
                    .expect("order Colfenor's trigger");
            }
            WaitingFor::TriggerTargetSelection {
                target_slots,
                selection,
                ..
            }
            | WaitingFor::TargetSelection {
                target_slots,
                selection,
                ..
            } => {
                let slot = &target_slots[selection.current_slot];
                assert!(
                    slot.legal_targets
                        .contains(&TargetRef::Object(expected_target)),
                    "the strict-lesser card must be a legal target: {slot:?}"
                );
                assert!(
                    !slot
                        .legal_targets
                        .contains(&TargetRef::Object(forbidden_target)),
                    "the creature whose death caused the trigger must be excluded: {slot:?}"
                );
                assert!(
                    !slot
                        .legal_targets
                        .contains(&TargetRef::Object(equal_target)),
                    "equal toughness must not be a legal target: {slot:?}"
                );
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(TargetRef::Object(expected_target)),
                    })
                    .expect("choose Colfenor's graveyard target");
            }
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => return,
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("pass priority while Colfenor resolves");
            }
            other => panic!("unexpected Colfenor waiting state: {other:?}"),
        }
    }
    panic!("Colfenor's trigger did not finish resolving");
}

#[test]
fn colfenor_returns_strictly_lesser_card_and_excludes_dying_trigger_object() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(
        P0,
        (0..3)
            .map(|index| {
                ManaUnit::new(
                    if index == 0 {
                        ManaType::Colorless
                    } else {
                        ManaType::Black
                    },
                    ObjectId(0),
                    false,
                    vec![],
                )
            })
            .collect(),
    );

    let colfenor = scenario
        .add_creature_from_oracle(P0, "Colfenor, the Last Yew", 3, 7, COLFENOR_ORACLE)
        .id();
    let dying = scenario
        .add_creature(P0, "Alaborn Trooper", 2, 3)
        .with_plus_counters(1)
        .id();
    let strict_lesser = scenario
        .add_creature_to_graveyard(P0, "Elvish Warrior", 2, 3)
        .id();
    let equal_toughness = scenario
        .add_creature_to_graveyard(P0, "Pillarfield Ox", 2, 4)
        .id();
    let murder = scenario
        .add_spell_to_hand_from_oracle(P0, "Murder", true, MURDER_ORACLE)
        .id();

    let mut runner = scenario.build();
    let card_id = runner.state().objects[&murder].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: murder,
            card_id,
            targets: vec![dying],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast real Murder through the public action path");

    assert_eq!(runner.state().objects[&colfenor].zone, Zone::Battlefield);
    resolve_colfenor_trigger(&mut runner, strict_lesser, dying, equal_toughness);

    assert_eq!(
        runner.state().objects[&strict_lesser].zone,
        Zone::Hand,
        "a card with toughness strictly less than Colfenor's LKI toughness returns"
    );
    assert_eq!(
        runner.state().objects[&equal_toughness].zone,
        Zone::Graveyard,
        "a card with equal toughness is not legal for 'lesser toughness'"
    );
    assert_eq!(
        runner.state().objects[&dying].zone,
        Zone::Graveyard,
        "the card that caused the trigger cannot be chosen as 'another'"
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
            && runner.state().stack.is_empty(),
        "resolution must finish at an empty-stack priority boundary"
    );
}
