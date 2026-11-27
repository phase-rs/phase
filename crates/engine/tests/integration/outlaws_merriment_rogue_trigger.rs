//! Runtime regression for Outlaws' Merriment's randomly selected Rogue mode.
//!
//! Oracle: "At the beginning of your upkeep, choose one at random. Create a
//! red and white creature token with those characteristics.
//! • 3/1 Human Warrior with trample and haste.
//! • 2/1 Human Cleric with lifelink and haste.
//! • 1/2 Human Rogue with haste and \"When this token enters, it deals 1 damage
//!   to any target.\""
//!
//! CR 603.2b: the beginning of P0's upkeep triggers Outlaws' Merriment. CR
//! 603.3c + CR 700.2b: its random modal choice is made as the trigger is put on
//! the stack. CR 603.6a: the Rogue's granted enters-the-battlefield trigger
//! then asks for a target through the public trigger-target action path. CR
//! 120.1 + CR 120.3a: choosing P1 makes the Rogue deal one damage to P1 and
//! causes P1 to lose one life.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{ContinuousModification, Effect, TargetRef, TargetSelectionMode};
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

use crate::support::shared_card_db;

const OUTLAWS_MERRIMENT_ORACLE: &str = r#"At the beginning of your upkeep, choose one at random. Create a red and white creature token with those characteristics.
• 3/1 Human Warrior with trample and haste.
• 2/1 Human Cleric with lifelink and haste.
• 1/2 Human Rogue with haste and "When this token enters, it deals 1 damage to any target.""#;

fn assert_merriment_parser_shape() {
    let parsed = parse_oracle_text(
        OUTLAWS_MERRIMENT_ORACLE,
        "Outlaws' Merriment",
        &[],
        &[],
        &[],
    );
    assert_eq!(parsed.triggers.len(), 1, "reach: upkeep trigger");
    let execute = parsed.triggers[0]
        .execute
        .as_deref()
        .expect("reach: trigger body");
    let modal = execute.modal.as_ref().expect("reach: random modal");
    assert_eq!(modal.selection, TargetSelectionMode::Random);
    assert_eq!(execute.mode_abilities.len(), 3);
    let Effect::Token {
        types,
        static_abilities,
        ..
    } = execute.mode_abilities[2].effect.as_ref()
    else {
        panic!("Rogue mode must remain a token effect after live parsing");
    };
    assert!(types.iter().any(|ty| ty == "Rogue"));
    assert!(static_abilities.iter().any(|static_ability| {
        static_ability
            .modifications
            .iter()
            .any(|modification| matches!(modification, ContinuousModification::GrantTrigger { .. }))
    }));
}

fn rogue_tokens(runner: &engine::game::scenario::GameRunner) -> Vec<ObjectId> {
    runner
        .state()
        .battlefield
        .iter()
        .filter(|id| {
            let object = &runner.state().objects[id];
            object.is_token && object.card_types.subtypes.iter().any(|s| s == "Rogue")
        })
        .copied()
        .collect()
}

#[test]
fn outlaws_merriment_rogue_etb_targets_p1_and_deals_one() {
    assert_merriment_parser_shape();
    let db = shared_card_db().expect("the committed integration fixture loads");
    let mut scenario = GameScenario::new_n_player(2, 2);
    let merriment = scenario.add_real_card(P0, "Outlaws' Merriment", Zone::Battlefield, db);
    scenario.at_phase(Phase::Untap);

    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    {
        let state = runner.state_mut();
        state.active_player = P0;
        state.priority_player = P0;
        state.waiting_for = WaitingFor::Priority { player: P0 };
    }
    assert_eq!(
        runner.state().objects[&merriment].zone,
        Zone::Battlefield,
        "Outlaws' Merriment is on P0's battlefield"
    );

    let p0_before = runner.life(P0);
    let p1_before = runner.life(P1);
    runner.advance_to_upkeep();
    assert_eq!(runner.state().phase, Phase::Upkeep);
    assert_eq!(runner.state().active_player, P0);

    let mut events = Vec::new();
    let mut trigger_target_prompts = 0;
    for _ in 0..64 {
        match runner.state().waiting_for.clone() {
            WaitingFor::OrderTriggers { player, triggers } => {
                assert_eq!(player, P0, "P0 owns the upkeep trigger ordering prompt");
                let result = runner
                    .act(GameAction::OrderTriggers {
                        order: (0..triggers.len()).collect(),
                    })
                    .expect("trigger ordering must be accepted");
                events.extend(result.events);
            }
            WaitingFor::TriggerTargetSelection {
                source_id,
                target_slots,
                selection,
                ..
            } => {
                trigger_target_prompts += 1;
                assert_eq!(
                    target_slots.len(),
                    1,
                    "the Rogue trigger has one target slot"
                );
                let rogues = rogue_tokens(&runner);
                assert_eq!(
                    rogues.len(),
                    1,
                    "exactly one newly-created Rogue reaches trigger target selection"
                );
                let rogue = rogues[0];
                assert_eq!(
                    source_id,
                    Some(rogue),
                    "the target prompt's source is the newly-created Rogue"
                );
                let slot = &target_slots[selection.current_slot];
                assert!(
                    slot.legal_targets.contains(&TargetRef::Player(P1)),
                    "P1 is a legal target for the Rogue's enters trigger"
                );
                let result = runner
                    .act(GameAction::SelectTargets {
                        targets: vec![TargetRef::Player(P1)],
                    })
                    .expect("selecting P1 for the Rogue's enters trigger must succeed");
                events.extend(result.events);
            }
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
            WaitingFor::Priority { .. } => {
                let result = runner
                    .act(GameAction::PassPriority)
                    .expect("passing priority must resolve the trigger");
                events.extend(result.events);
            }
            other => panic!("unexpected wait while resolving Outlaws' Merriment: {other:?}"),
        }
    }

    assert_eq!(
        trigger_target_prompts, 1,
        "exactly one newly-created Rogue must reach TriggerTargetSelection"
    );
    let rogue = rogue_tokens(&runner);
    assert_eq!(
        rogue.len(),
        1,
        "the upkeep must create exactly one Rogue token"
    );
    let rogue = rogue[0];
    let rogue_damage: Vec<_> = events
        .iter()
        .filter(|event| {
            matches!(
                event,
                GameEvent::DamageDealt {
                    source_id,
                    target: TargetRef::Player(P1),
                    amount: 1,
                    is_combat: false,
                    ..
                } if *source_id == rogue
            )
        })
        .collect();
    assert_eq!(
        rogue_damage.len(),
        1,
        "the Rogue must deal exactly one noncombat damage to P1"
    );
    assert!(
        !events.iter().any(|event| matches!(
            event,
            GameEvent::DamageDealt {
                target: TargetRef::Player(P0),
                ..
            }
        )),
        "the Rogue's enters trigger must not deal damage to P0"
    );
    assert_eq!(runner.life(P0), p0_before, "P0 takes no damage");
    assert_eq!(runner.life(P1), p1_before - 1, "P1 loses exactly one life");
    assert!(
        runner.state().stack.is_empty(),
        "the stack is empty after resolution"
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "resolution completes at a priority window"
    );
}
