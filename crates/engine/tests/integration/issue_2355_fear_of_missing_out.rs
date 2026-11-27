//! Regression for issue #2355: Fear of Missing Out's delirium attack trigger must
//! prompt for and untap a target creature, then grant an additional combat phase.
//!
//! https://github.com/phase-rs/phase/issues/2355

use engine::ai_support::legal_actions_full;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{TargetRef, TriggerCondition};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::{StackEntryKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard};
use engine::types::phase::Phase;
use engine::types::triggers::TriggerMode;
use engine::types::zones::Zone;

use super::rules::AttackTarget;

const AURELIA_ORACLE: &str = "Flying, vigilance, haste\n\
Whenever Aurelia attacks for the first time each turn, untap all creatures you control. \
After this phase, there is an additional combat phase.";

const SCOURGE_ORACLE: &str = "Flying\n\
Dethrone (Whenever this creature attacks the player with the most life or tied for most life, put a +1/+1 counter on it.)\n\
Whenever this creature attacks for the first time each turn, if it's attacking the player with \
the most life or tied for most life, untap all attacking creatures. After this phase, there is \
an additional combat phase.";

const FOMO_ORACLE: &str = "When this creature enters, discard a card, then draw a card.\n\
Delirium — Whenever this creature attacks for the first time each turn, if there are four or \
more card types among cards in your graveyard, untap target creature. After this phase, there \
is an additional combat phase.";

fn seed_delirium_graveyard(scenario: &mut GameScenario) -> ObjectId {
    scenario.add_creature_to_graveyard(P0, "Delirium Creature", 1, 1);
    scenario.add_spell_to_graveyard(P0, "Delirium Instant", true);
    scenario.add_spell_to_graveyard(P0, "Delirium Sorcery", false);
    scenario.add_land_to_hand(P0, "Delirium Forest").id()
}

fn resolve_fomo_attack_trigger(runner: &mut engine::game::scenario::GameRunner, target: ObjectId) {
    let mut saw_target_prompt = false;
    for _ in 0..40 {
        match runner.state().waiting_for.clone() {
            WaitingFor::TriggerTargetSelection { .. } | WaitingFor::TargetSelection { .. } => {
                saw_target_prompt = true;
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(TargetRef::Object(target)),
                    })
                    .expect("choose FOMO untap target");
            }
            WaitingFor::Priority { .. } if !runner.state().stack.is_empty() => {
                runner.pass_both_players();
            }
            _ => break,
        }
    }
    assert!(
        saw_target_prompt,
        "FOMO delirium attack trigger must prompt for untap target, got waiting_for = {:?}, stack = {}",
        runner.state().waiting_for,
        runner.state().stack.len()
    );
    runner.advance_until_stack_empty();
}

#[test]
fn fear_of_missing_out_parses_delirium_attack_untap_trigger() {
    let parsed = parse_oracle_text(
        FOMO_ORACLE,
        "Fear of Missing Out",
        &[],
        &["Creature".to_string()],
        &["Human".to_string(), "Warrior".to_string()],
    );
    assert_eq!(
        parsed.triggers.len(),
        2,
        "FOMO must parse ETB + delirium attack triggers, got {:#?}",
        parsed.triggers
    );
    let attack = parsed
        .triggers
        .iter()
        .find(|t| t.mode == TriggerMode::Attacks)
        .expect("FOMO must have delirium attack trigger");
    assert!(matches!(
        attack.execute.as_ref().map(|e| &*e.effect),
        Some(engine::types::ability::Effect::SetTapState { .. })
    ));
}

#[test]
fn fear_of_missing_out_untaps_target_creature_on_delirium_attack() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let land = seed_delirium_graveyard(&mut scenario);

    let fomo = scenario
        .add_creature(P0, "Fear of Missing Out", 2, 2)
        .from_oracle_text(FOMO_ORACLE)
        .id();
    let untap_target = scenario.add_creature(P0, "Tapped Ally", 1, 1).id();

    let mut runner = scenario.build();
    let mut events = Vec::new();
    engine::game::zones::move_to_zone(runner.state_mut(), land, Zone::Graveyard, &mut events);
    {
        let obj = runner.state_mut().objects.get_mut(&untap_target).unwrap();
        obj.tapped = true;
    }

    runner.pass_both_players();
    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![(fomo, AttackTarget::Player(P1))],
            bands: vec![],
        })
        .expect("DeclareAttackers should succeed");

    if matches!(runner.state().waiting_for, WaitingFor::Priority { .. }) {
        runner.pass_both_players();
    }
    if matches!(
        runner.state().waiting_for,
        WaitingFor::DeclareBlockers { .. }
    ) {
        runner
            .act(GameAction::DeclareBlockers {
                assignments: vec![],
            })
            .expect("DeclareBlockers should succeed");
        runner.pass_both_players();
    }

    resolve_fomo_attack_trigger(&mut runner, untap_target);

    assert!(
        !runner.state().objects[&untap_target].tapped,
        "FOMO must untap the chosen creature (issue #2355)"
    );
    assert!(
        !runner.state().extra_phases.is_empty(),
        "FOMO must schedule an additional combat phase after the untap"
    );
}

fn advance_to_next_attack_declaration(runner: &mut GameRunner) {
    for _ in 0..64 {
        match runner.state().waiting_for {
            WaitingFor::DeclareAttackers { .. } => return,
            WaitingFor::DeclareBlockers { .. } => {
                runner.declare_blockers(&[]).unwrap();
            }
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).unwrap();
            }
            ref prompt => panic!("unexpected prompt before declaring attackers: {prompt:?}"),
        }
    }
    panic!("did not reach the next attack declaration");
}

fn add_aurelia(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_creature(P0, "Aurelia, the Warleader", 3, 4)
        .from_oracle_text_with_keywords(&["Flying", "Vigilance", "Haste"], AURELIA_ORACLE)
        .id()
}

#[test]
fn first_attack_each_turn_is_independent_of_delirium_and_batch_order() {
    for reverse in [false, true] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let land = seed_delirium_graveyard(&mut scenario);
        let repeat = scenario
            .add_creature(P0, "Fear of Missing Out", 2, 2)
            .from_oracle_text(FOMO_ORACLE)
            .id();
        let first = scenario
            .add_creature(P0, "Fear of Missing Out", 2, 2)
            .from_oracle_text(FOMO_ORACLE)
            .id();
        let aurelia = add_aurelia(&mut scenario);
        let mut runner = scenario.build();
        let attack = |id| (id, AttackTarget::Player(P1));

        runner.advance_to_combat();
        runner
            .declare_attackers(&[attack(repeat), attack(aurelia)])
            .unwrap();
        let stack = &runner.state().stack;
        assert_eq!(stack.len(), 1, "only Aurelia triggers before delirium");
        assert_eq!(stack[0].source_id, aurelia);
        runner.advance_until_stack_empty();
        assert!(runner.state().stack.is_empty());
        assert!(!runner.state().extra_phases.is_empty());

        engine::game::zones::move_to_zone(
            runner.state_mut(),
            land,
            Zone::Graveyard,
            &mut Vec::new(),
        );
        let turn = runner.state().turn_number;
        advance_to_next_attack_declaration(&mut runner);
        assert_eq!(runner.state().turn_number, turn);

        let mut attacks = [attack(repeat), attack(first), attack(aurelia)];
        if reverse {
            attacks.swap(0, 1);
        }
        runner.declare_attackers(&attacks).unwrap();
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::TriggerTargetSelection { .. } | WaitingFor::TargetSelection { .. }
        ));
        runner
            .act(GameAction::ChooseTarget {
                target: Some(TargetRef::Object(first)),
            })
            .unwrap();

        let stack = &runner.state().stack;
        assert_eq!(stack.len(), 1, "only the newly declaring Fear triggers");
        assert_eq!(stack[0].source_id, first);
        runner.advance_until_stack_empty();
        assert!(runner.state().stack.is_empty());
        assert!(!runner.state().objects[&first].tapped);
        assert!(runner.state().objects[&repeat].tapped);
    }
}

#[test]
fn scourge_checks_the_actual_attacked_player_at_detection_and_resolution() {
    for (attack_player, lower_before_declaration, lower_before_resolution) in [
        (false, false, false),
        (true, true, false),
        (true, false, true),
        (true, false, false),
    ] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        if lower_before_declaration {
            scenario.with_life(P1, 19);
        }
        let scourge = scenario
            .add_creature(P0, "Scourge of the Throne", 5, 5)
            .from_oracle_text_with_keywords(&["Flying", "Dethrone"], SCOURGE_ORACLE)
            .id();
        let planeswalker = scenario
            .add_planeswalker_from_oracle(P1, "Test Planeswalker", "Jace", 5, "+1: Draw a card.")
            .id();
        let mut runner = scenario.build();
        runner.advance_to_combat();
        let target = if attack_player {
            AttackTarget::Player(P1)
        } else {
            AttackTarget::Planeswalker(planeswalker)
        };
        runner.declare_attackers(&[(scourge, target)]).unwrap();
        if let WaitingFor::OrderTriggers { triggers, .. } = &runner.state().waiting_for {
            let order = (0..triggers.len()).collect();
            runner.act(GameAction::OrderTriggers { order }).unwrap();
        }
        if attack_player && !lower_before_declaration {
            assert_eq!(runner.state().stack.len(), 2);
            assert!(runner.state().stack.iter().any(|entry| {
                entry.source_id == scourge
                    && matches!(
                        entry.kind,
                        StackEntryKind::TriggeredAbility {
                            condition: Some(TriggerCondition::SourceIsAttacking { .. }),
                            ..
                        }
                    )
            }));
            assert!(runner.state().stack.iter().any(|entry| {
                entry.source_id == scourge
                    && matches!(
                        entry.kind,
                        StackEntryKind::TriggeredAbility {
                            condition: None,
                            ..
                        }
                    )
            }));
        } else {
            assert!(
                runner.state().stack.is_empty(),
                "attacking a planeswalker or a lower-life player triggers neither ability"
            );
        }
        if lower_before_resolution {
            runner
                .state_mut()
                .players
                .iter_mut()
                .find(|player| player.id == P1)
                .unwrap()
                .life -= 1;
        }
        runner.advance_until_stack_empty();
        assert!(runner.state().stack.is_empty());
        let grants_combat = attack_player && !lower_before_declaration && !lower_before_resolution;
        assert_eq!(!runner.state().extra_phases.is_empty(), grants_combat);
        assert_eq!(!runner.state().objects[&scourge].tapped, grants_combat);
        let counters = runner.state().objects[&scourge]
            .counters
            .get(&CounterType::Plus1Plus1)
            .copied()
            .unwrap_or_default();
        assert_eq!(
            counters,
            u32::from(attack_player && !lower_before_declaration)
        );
    }
}

#[test]
fn scourge_attacking_a_planeswalker_spends_its_first_declaration() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let aurelia = add_aurelia(&mut scenario);
    let scourge = scenario
        .add_creature(P0, "Scourge of the Throne", 5, 5)
        .from_oracle_text_with_keywords(&["Flying", "Dethrone"], SCOURGE_ORACLE)
        .id();
    let planeswalker = scenario
        .add_planeswalker_from_oracle(P1, "Test Planeswalker", "Jace", 12, "+1: Draw a card.")
        .id();
    let mut runner = scenario.build();
    runner.advance_to_combat();
    let turn = runner.state().turn_number;
    runner
        .declare_attackers(&[
            (scourge, AttackTarget::Planeswalker(planeswalker)),
            (aurelia, AttackTarget::Planeswalker(planeswalker)),
        ])
        .unwrap();
    assert_eq!(runner.state().stack.len(), 1);
    assert_eq!(runner.state().stack[0].source_id, aurelia);
    runner.advance_until_stack_empty();
    assert!(!runner.state().extra_phases.is_empty());
    advance_to_next_attack_declaration(&mut runner);
    assert_eq!(runner.state().turn_number, turn);

    runner
        .declare_attackers(&[(scourge, AttackTarget::Player(P1))])
        .unwrap();
    assert_eq!(runner.state().stack.len(), 1, "only Dethrone triggers");
    let entry = &runner.state().stack[0];
    assert_eq!(entry.source_id, scourge);
    assert!(matches!(
        entry.kind,
        StackEntryKind::TriggeredAbility {
            condition: None,
            ..
        }
    ));
    runner.advance_until_stack_empty();
    assert!(runner.state().stack.is_empty());
    assert!(runner.state().extra_phases.is_empty());
    assert!(runner.state().objects[&scourge].tapped);
    assert_eq!(
        runner.state().objects[&scourge]
            .counters
            .get(&CounterType::Plus1Plus1),
        Some(&1)
    );
}

#[test]
fn first_attack_ordinal_restarts_after_a_zone_change_or_a_new_turn() {
    const CLOUDSHIFT_ORACLE: &str =
        "Exile target creature you control, then return that card to the battlefield under your control.";

    for blink_in_same_turn in [false, true] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        scenario.add_card_to_library_top(P0, "P0 draw");
        scenario.add_card_to_library_top(P1, "P1 draw");
        scenario.add_creature(P1, "Opponent attacker", 1, 1);
        let plains = scenario.add_basic_land(P0, ManaColor::White);
        let aurelia = add_aurelia(&mut scenario);
        let cloudshift = scenario
            .add_spell_to_hand_from_oracle(P0, "Cloudshift", true, CLOUDSHIFT_ORACLE)
            .with_mana_cost(ManaCost::Cost {
                shards: vec![ManaCostShard::White],
                generic: 0,
            })
            .id();
        let mut runner = scenario.build();
        runner.advance_to_combat();
        let turn = runner.state().turn_number;
        let original_incarnation = runner.state().objects[&aurelia].incarnation;
        let attacks = [(aurelia, AttackTarget::Player(P1))];
        runner.declare_attackers(&attacks).unwrap();
        assert_eq!(runner.state().stack.len(), 1);
        assert_eq!(runner.state().stack[0].source_id, aurelia);
        runner.advance_until_stack_empty();
        assert!(!runner.state().extra_phases.is_empty());

        if blink_in_same_turn {
            let (_, _, actions) = legal_actions_full(runner.state());
            let tap_plains = actions[&plains]
                .iter()
                .find(|action| matches!(action, GameAction::TapLandForMana { .. }))
                .unwrap()
                .clone();
            runner.act(tap_plains).unwrap();
            runner.cast(cloudshift).target_object(aurelia).resolve();
            let returned = &runner.state().objects[&aurelia];
            assert_eq!(returned.zone, Zone::Battlefield);
            assert_ne!(returned.incarnation, original_incarnation);
        }
        advance_to_next_attack_declaration(&mut runner);
        assert_eq!(runner.state().turn_number, turn);
        if !blink_in_same_turn {
            runner.declare_attackers(&[]).unwrap();
            advance_to_next_attack_declaration(&mut runner);
            assert_eq!(runner.state().active_player, P1);
            runner.declare_attackers(&[]).unwrap();
            advance_to_next_attack_declaration(&mut runner);
            assert_eq!(runner.state().active_player, P0);
            assert_eq!(runner.state().turn_number, turn + 2);
            assert_eq!(
                runner.state().objects[&aurelia].incarnation,
                original_incarnation
            );
        }

        runner.declare_attackers(&attacks).unwrap();
        assert_eq!(
            runner.state().stack.len(),
            1,
            "Aurelia's attack triggers again"
        );
        assert_eq!(runner.state().stack[0].source_id, aurelia);
        runner.advance_until_stack_empty();
        assert!(runner.state().stack.is_empty());
        assert!(!runner.state().extra_phases.is_empty());
    }
}
