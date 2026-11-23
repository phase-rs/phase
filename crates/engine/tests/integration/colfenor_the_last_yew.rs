//! Regression for Colfenor, the Last Yew's relative-toughness dies trigger.
//!
//! CR 603.10a + CR 608.2h: the trigger compares graveyard candidates with the
//! dying creature's last-known toughness, not the departed object's reset
//! graveyard characteristics. CR 208.1: the first printed characteristic is
//! power and the second is toughness.

use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, PayCostKind, StackEntryKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const COLFENOR_ORACLE: &str =
    "Reach, vigilance\nWhenever Colfenor or another creature you control dies, return up to one other target creature card with lesser toughness from your graveyard to your hand.";
const MURDER_ORACLE: &str = "Destroy target creature.";
const TORTURED_EXISTENCE_ORACLE: &str =
    "{B}, Discard a creature card: Return target creature card from your graveyard to your hand.";
const DEFLECTING_SWAT_ORACLE: &str =
    "If you control a commander, you may cast this spell without paying its mana cost.\nYou may choose new targets for target spell or ability.";

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

fn activate_tortured_existence(
    runner: &mut GameRunner,
    tortured: ObjectId,
    target: ObjectId,
    discard: ObjectId,
) {
    runner
        .act(GameAction::ActivateAbility {
            source_id: tortured,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("activate Tortured Existence");
    for _ in 0..64 {
        match runner.state().waiting_for.clone() {
            WaitingFor::TargetSelection {
                target_slots,
                selection,
                ..
            } => {
                assert_eq!(selection.current_slot, 0);
                assert!(target_slots[0]
                    .legal_targets
                    .contains(&TargetRef::Object(target)));
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(TargetRef::Object(target)),
                    })
                    .expect("choose Tortured Existence target");
            }
            WaitingFor::PayCost {
                kind: PayCostKind::Discard,
                ..
            } => {
                runner
                    .act(GameAction::SelectCards {
                        cards: vec![discard],
                    })
                    .expect("discard Tortured Existence cost");
            }
            WaitingFor::ManaPayment { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("pay Tortured Existence's {B}");
            }
            WaitingFor::Priority { .. } if runner.state().stack.len() == 2 => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("resolve Tortured Existence");
            }
            WaitingFor::Priority { .. } => return,
            other => panic!("unexpected Tortured Existence state: {other:?}"),
        }
    }
    panic!("Tortured Existence activation did not resolve");
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
    runner.cast(murder).target_objects(&[dying]).commit();

    for _ in 0..8 {
        if matches!(runner.state().waiting_for, WaitingFor::Priority { .. }) {
            runner
                .act(GameAction::PassPriority)
                .expect("pass priority to resolve real Murder");
        } else {
            break;
        }
    }

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

/// CR 400.7 + CR 603.2 + CR 115.7d: the object that caused Colfenor's trigger
/// is excluded by its original graveyard incarnation, but a later incarnation
/// reusing the same storage id is a legal Swat retarget. This drives the public
/// Tortured Existence path twice before Deflecting Swat re-evaluates the trigger.
#[test]
fn colfenor_allows_a_later_graveyard_incarnation_of_the_dying_card() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let colfenor = scenario
        .add_creature_from_oracle(P0, "Colfenor, the Last Yew", 3, 7, COLFENOR_ORACLE)
        .id();
    let dying = scenario
        .add_creature(P0, "Alaborn Trooper", 2, 3)
        .with_plus_counters(1)
        .id();
    let lesser = scenario
        .add_creature_to_graveyard(P0, "Elvish Warrior", 2, 3)
        .id();
    let equal = scenario
        .add_creature_to_graveyard(P0, "Pillarfield Ox", 2, 4)
        .id();
    let discard_fodder = scenario
        .add_creature_to_hand(P0, "Discard Fodder", 1, 1)
        .id();
    let tortured = scenario
        .add_enchantment_from_oracle(P0, "Tortured Existence", TORTURED_EXISTENCE_ORACLE)
        .with_mana_cost(ManaCost::zero())
        .id();
    let murder = scenario
        .add_spell_to_hand_from_oracle(P0, "Murder", true, MURDER_ORACLE)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Black, ManaCostShard::Black],
            generic: 1,
        })
        .id();
    let swat = scenario
        .add_spell_to_hand_from_oracle(P0, "Deflecting Swat", true, DEFLECTING_SWAT_ORACLE)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Red],
            generic: 2,
        })
        .id();
    scenario.with_mana_pool(
        P0,
        (0..8)
            .map(|index| {
                ManaUnit::new(
                    if index == 0 {
                        ManaType::Red
                    } else if index < 5 {
                        ManaType::Black
                    } else {
                        ManaType::Colorless
                    },
                    ObjectId(0),
                    false,
                    vec![],
                )
            })
            .collect(),
    );

    let mut runner = scenario.build();
    runner.cast(murder).target_object(dying).commit();
    let trigger = loop {
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } => {
                if let Some(entry) = runner.state().stack.iter().find(|entry| {
                    entry.source_id == colfenor
                        && matches!(entry.kind, StackEntryKind::TriggeredAbility { .. })
                }) {
                    break entry.id;
                }
                runner
                    .act(GameAction::PassPriority)
                    .expect("resolve Murder toward Colfenor trigger");
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
                assert_eq!(selection.current_slot, 0);
                assert!(target_slots[0]
                    .legal_targets
                    .contains(&TargetRef::Object(lesser)));
                assert!(!target_slots[0]
                    .legal_targets
                    .contains(&TargetRef::Object(dying)));
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(TargetRef::Object(lesser)),
                    })
                    .expect("choose the original strict-lesser card");
            }
            WaitingFor::OrderTriggers { triggers, .. } => {
                runner
                    .act(GameAction::OrderTriggers {
                        order: (0..triggers.len()).collect(),
                    })
                    .expect("order Colfenor trigger");
            }
            other => panic!("unexpected Murder/Colfenor state: {other:?}"),
        }
    };
    assert_eq!(runner.state().objects[&dying].zone, Zone::Graveyard);

    activate_tortured_existence(&mut runner, tortured, dying, discard_fodder);
    activate_tortured_existence(&mut runner, tortured, equal, dying);
    assert_eq!(runner.state().objects[&dying].zone, Zone::Graveyard);

    runner.cast(swat).target_object(trigger).commit();
    for _ in 0..64 {
        match runner.state().waiting_for.clone() {
            WaitingFor::OptionalEffectChoice { .. } => {
                runner
                    .act(GameAction::DecideOptionalEffect { accept: true })
                    .expect("accept Deflecting Swat");
            }
            WaitingFor::RetargetChoice { .. } => break,
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("resolve Deflecting Swat");
            }
            other => panic!("unexpected Swat state: {other:?}"),
        }
    }
    let WaitingFor::RetargetChoice {
        legal_new_targets,
        slot_pools,
        ..
    } = &runner.state().waiting_for
    else {
        panic!("expected Swat's retarget prompt");
    };
    assert!(legal_new_targets.contains(&TargetRef::Object(dying)));
    assert!(slot_pools[0].contains(&TargetRef::Object(dying)));
    assert!(!slot_pools[0].contains(&TargetRef::Object(equal)));
}
