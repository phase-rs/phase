//! Regression for issue #1312: casting a prepared copy of a targeting instant or
//! sorcery must fire SpellCast triggers (e.g. Lecturing Scornmage).
//!
//! https://github.com/phase-rs/phase/issues/1312

use engine::game::scenario::{GameScenario, P0};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

use crate::support::shared_card_db as load_db;

fn drive_cast_to_stack(runner: &mut engine::game::scenario::GameRunner, spell_target: ObjectId) {
    loop {
        match &runner.state().waiting_for {
            WaitingFor::TargetSelection { .. } => {
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(TargetRef::Object(spell_target)),
                    })
                    .expect("spell target selection should succeed");
            }
            WaitingFor::TriggerTargetSelection { .. } => {
                runner
                    .choose_first_legal_target()
                    .expect("trigger target selection should succeed");
            }
            WaitingFor::ManaPayment { .. } => {
                runner.act(GameAction::PassPriority).expect("pay mana");
            }
            WaitingFor::Priority { .. } => break,
            other => panic!("unexpected waiting state during cast: {other:?}"),
        }
    }
}

#[test]
fn issue_1312_prepared_swords_to_plowshares_triggers_lecturing_scornmage() {
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let scornmage = scenario.add_real_card(P0, "Lecturing Scornmage", Zone::Battlefield, db);
    let emeritus = scenario.add_real_card(P0, "Emeritus of Truce", Zone::Battlefield, db);
    let exile_target = scenario.add_creature(P0, "Exile Target", 2, 2).id();
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(ManaType::White, ObjectId(0), false, vec![])],
    );

    let mut runner = scenario.build();
    runner.state_mut().debug_mode = true;
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    let back = runner
        .state()
        .objects
        .get(&emeritus)
        .and_then(|o| o.back_face.clone())
        .expect("Emeritus of Truce must hydrate Swords to Plowshares prepare face");
    assert_eq!(back.name, "Swords to Plowshares");

    runner
        .act(GameAction::Debug(
            engine::types::actions::DebugAction::SetPrepared {
                object_id: emeritus,
                prepared: true,
            },
        ))
        .expect("prepare Emeritus for cast");

    runner
        .act(GameAction::CastPreparedCopy { source: emeritus })
        .expect("CastPreparedCopy should start the prepared spell cast");

    drive_cast_to_stack(&mut runner, exile_target);

    let scornmage_triggers = runner
        .state()
        .objects
        .get(&scornmage)
        .map(|o| o.trigger_definitions.len())
        .unwrap_or(0);
    assert!(
        scornmage_triggers > 0,
        "Lecturing Scornmage must have SpellCast trigger after rehydrate"
    );
    let swords_stack_entry = runner
        .state()
        .stack
        .iter()
        .find(|entry| {
            matches!(
                entry.kind,
                engine::types::game_state::StackEntryKind::Spell { .. }
            )
        })
        .expect("prepared Swords copy must be on the stack after casting");
    let stack_ability = swords_stack_entry
        .ability()
        .expect("prepared spell stack entry must carry finalized ability");
    assert!(
        !engine::game::ability_utils::flatten_targets_in_chain(stack_ability).is_empty(),
        "prepared targeting spell must have targets on stack entry before trigger scan"
    );

    runner.advance_until_stack_empty();

    let counters = runner
        .state()
        .objects
        .get(&scornmage)
        .and_then(|o| o.counters.get(&CounterType::Plus1Plus1))
        .copied()
        .unwrap_or(0);
    assert_eq!(
        counters, 1,
        "Lecturing Scornmage must get a +1/+1 counter when a prepared targeting spell is cast"
    );
}
