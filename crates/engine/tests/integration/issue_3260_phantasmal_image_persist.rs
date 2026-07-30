//! Regression for issue #3260: Phantasmal Image copying Kitchen Finks must gain
//! persist and return to the battlefield with a -1/-1 counter when it dies.
//!
//! https://github.com/phase-rs/phase/issues/3260

use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::game::triggers::process_triggers;
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;
use engine::types::CounterType;

fn issue_3260_db() -> &'static engine::database::card_db::CardDatabase {
    static DB: std::sync::OnceLock<engine::database::card_db::CardDatabase> =
        std::sync::OnceLock::new();
    DB.get_or_init(|| {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/issue_3260_cards.json");
        engine::database::card_db::CardDatabase::from_export(&path)
            .expect("card-data export must load")
    })
}

fn add_mana(runner: &mut GameRunner, mana: &[ManaType]) {
    let dummy = engine::types::identifiers::ObjectId(0);
    let pool = &mut runner.state_mut().players[0].mana_pool;
    for m in mana {
        pool.add(ManaUnit::new(*m, dummy, false, vec![]));
    }
}

fn destroy_with_lethal_damage(runner: &mut GameRunner, object_id: ObjectId) {
    runner
        .state_mut()
        .objects
        .get_mut(&object_id)
        .unwrap()
        .damage_marked = 99;

    let mut events = Vec::new();
    engine::game::sba::check_state_based_actions(runner.state_mut(), &mut events);
    process_triggers(runner.state_mut(), &events);
}

fn drive_until_stack_empty(runner: &mut GameRunner) {
    for _ in 0..128 {
        match runner.state().waiting_for.clone() {
            WaitingFor::ReplacementChoice { .. } => {
                runner
                    .act(GameAction::ChooseReplacement { index: 0 })
                    .expect("accept enter-as-copy replacement");
            }
            WaitingFor::CopyTargetChoice { valid_targets, .. } => {
                let target = valid_targets[0];
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(TargetRef::Object(target)),
                    })
                    .expect("choose copy target");
            }
            WaitingFor::OptionalEffectChoice { .. } => {
                runner
                    .act(GameAction::DecideOptionalEffect { accept: true })
                    .expect("accept optional copy");
            }
            WaitingFor::TargetSelection { .. } | WaitingFor::TriggerTargetSelection { .. } => {
                runner.choose_first_legal_target().expect("choose target");
            }
            WaitingFor::OrderTriggers { .. } => {
                engine::game::triggers::drain_order_triggers_with_identity(runner.state_mut());
            }
            WaitingFor::ManaPayment { .. } => {
                runner.act(GameAction::PassPriority).expect("pay mana");
            }
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => return,
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            other => panic!("unexpected waiting_for while resolving: {other:?}"),
        }
    }
    panic!("resolution loop exhausted");
}

#[test]
fn issue_3260_phantasmal_image_copy_of_kitchen_finks_persists() {
    let db = issue_3260_db();

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let image = scenario.add_real_card(P0, "Phantasmal Image", Zone::Hand, db);
    let finks = scenario.add_real_card(P0, "Kitchen Finks", Zone::Battlefield, db);
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    add_mana(&mut runner, &[ManaType::Blue, ManaType::Colorless]);

    let card_id = runner.state().objects[&image].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: image,
            card_id,
            targets: vec![finks],
            payment_mode: engine::types::game_state::CastPaymentMode::Auto,
        })
        .expect("cast Phantasmal Image");

    drive_until_stack_empty(&mut runner);

    let copy_id = runner
        .state()
        .battlefield
        .iter()
        .copied()
        .find(|id| {
            *id != finks && {
                let obj = &runner.state().objects[id];
                obj.name == "Kitchen Finks" && obj.controller == P0
            }
        })
        .expect("Phantasmal Image should enter as a copy of Kitchen Finks");

    assert!(
        runner
            .state()
            .objects
            .get(&copy_id)
            .unwrap()
            .keywords
            .iter()
            .any(|k| matches!(k, engine::types::keywords::Keyword::Persist)),
        "copy must have persist keyword"
    );
    assert!(
        runner
            .state()
            .objects
            .get(&copy_id)
            .unwrap()
            .trigger_definitions
            .as_slice()
            .iter()
            .any(|trigger| {
                engine::database::synthesis::KeywordTriggerInstaller::trigger_matches_keyword_kind(
                    trigger.definition(),
                    &engine::types::keywords::Keyword::Persist,
                )
            }),
        "copy must carry Persist's synthesized dies trigger"
    );

    destroy_with_lethal_damage(&mut runner, copy_id);

    assert_eq!(
        runner.state().objects[&copy_id].zone,
        Zone::Graveyard,
        "copy must be in graveyard after lethal damage"
    );

    drive_until_stack_empty(&mut runner);

    assert_eq!(
        runner.state().objects[&copy_id].zone,
        Zone::Battlefield,
        "persist must return the same object to the battlefield"
    );

    let persisted_id = copy_id;
    let counters = &runner.state().objects[&persisted_id].counters;
    assert!(
        counters
            .get(&CounterType::Minus1Minus1)
            .copied()
            .unwrap_or(0)
            >= 1,
        "persisted copy must enter with a -1/-1 counter"
    );
}
