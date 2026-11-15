//! Regression for issue #1312: casting a prepared copy of a targeting instant or
//! sorcery must fire SpellCast triggers (e.g. Lecturing Scornmage).
//!
//! https://github.com/phase-rs/phase/issues/1312

use engine::database::CardDatabase;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::{StackEntryKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

use crate::support::shared_card_db as load_db;

struct PreparedSwordsFixture {
    runner: GameRunner,
    emeritus: ObjectId,
    exile_target: ObjectId,
    scornmage: Option<ObjectId>,
    counterspell: Option<ObjectId>,
}

fn build_prepared_swords_fixture(
    db: &CardDatabase,
    with_scornmage: bool,
    with_counterspell: bool,
) -> PreparedSwordsFixture {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let scornmage = with_scornmage
        .then(|| scenario.add_real_card(P0, "Lecturing Scornmage", Zone::Battlefield, db));
    let emeritus = scenario.add_real_card(P0, "Emeritus of Truce", Zone::Battlefield, db);
    let exile_target = scenario.add_creature(P0, "Exile Target", 2, 2).id();
    let counterspell =
        with_counterspell.then(|| scenario.add_real_card(P1, "Counterspell", Zone::Hand, db));
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(ManaType::White, ObjectId(0), false, vec![])],
    );
    if with_counterspell {
        scenario.with_mana_pool(
            P1,
            vec![
                ManaUnit::new(ManaType::Blue, ObjectId(0), false, vec![]),
                ManaUnit::new(ManaType::Blue, ObjectId(0), false, vec![]),
            ],
        );
    }

    let mut runner = scenario.build();
    runner.state_mut().debug_mode = true;
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    let back = runner
        .state()
        .objects
        .get(&emeritus)
        .and_then(|object| object.back_face.clone())
        .expect("Emeritus of Truce must hydrate Swords to Plowshares prepare face");
    assert_eq!(back.name, "Swords to Plowshares");

    PreparedSwordsFixture {
        runner,
        emeritus,
        exile_target,
        scornmage,
        counterspell,
    }
}

fn begin_prepared_cast(runner: &mut GameRunner, emeritus: ObjectId) -> ObjectId {
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

    let copy_id = match &runner.state().waiting_for {
        WaitingFor::TargetSelection { pending_cast, .. } => pending_cast.object_id,
        other => panic!("prepared Swords cast must pause for a target, got {other:?}"),
    };
    let placeholder = runner
        .state()
        .stack
        .iter()
        .find(|entry| entry.id == copy_id)
        .expect("CR 601.2a announcement must create the exact prepared-copy stack entry");
    assert!(matches!(
        &placeholder.kind,
        StackEntryKind::Spell { ability: None, .. }
    ));
    assert!(runner.state().objects.contains_key(&copy_id));

    copy_id
}

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

fn assert_prepared_copy_finalized_on_stack(
    runner: &GameRunner,
    copy_id: ObjectId,
    spell_target: ObjectId,
) {
    let entry = runner
        .state()
        .stack
        .iter()
        .find(|entry| entry.id == copy_id)
        .expect("prepared spell must retain its exact stack entry after targeting");
    let ability = entry
        .ability()
        .expect("prepared spell stack entry must carry its finalized ability");
    assert!(
        engine::game::ability_utils::flatten_targets_in_chain(ability)
            .contains(&TargetRef::Object(spell_target))
    );
    assert_eq!(runner.state().objects[&copy_id].zone, Zone::Stack);
}

#[test]
fn issue_1312_prepared_swords_to_plowshares_triggers_lecturing_scornmage() {
    let Some(db) = load_db() else {
        return;
    };

    let PreparedSwordsFixture {
        mut runner,
        emeritus,
        exile_target,
        scornmage,
        counterspell: _,
    } = build_prepared_swords_fixture(db, true, false);
    let scornmage = scornmage.expect("Scornmage fixture requested");
    let copy_id = begin_prepared_cast(&mut runner, emeritus);
    drive_cast_to_stack(&mut runner, exile_target);
    // CR 601.2a + CR 704.3: Choosing the target completes the cast through
    // `apply`, which reaches the ordinary priority-boundary SBA pipeline. The
    // prepared copy must already have finalized into its real Stack zone there.
    assert_prepared_copy_finalized_on_stack(&runner, copy_id, exile_target);

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
    runner.advance_until_stack_empty();

    assert_eq!(runner.state().objects[&exile_target].zone, Zone::Exile);
    assert!(runner.state().stack.is_empty());
    // CR 608.2n + CR 704.5d + CR 704.5e: Swords resolves normally, then its
    // previously proven-live synthetic copy ceases through the ordinary cleanup route.
    assert!(!runner.state().objects.contains_key(&copy_id));

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

#[test]
fn issue_1312_countered_prepared_copy_ceases_without_resolving() {
    let Some(db) = load_db() else {
        return;
    };

    let PreparedSwordsFixture {
        mut runner,
        emeritus,
        exile_target,
        scornmage: _,
        counterspell,
    } = build_prepared_swords_fixture(db, false, true);
    let counterspell = counterspell.expect("Counterspell fixture requested");
    let copy_id = begin_prepared_cast(&mut runner, emeritus);
    drive_cast_to_stack(&mut runner, exile_target);
    assert_prepared_copy_finalized_on_stack(&runner, copy_id, exile_target);

    runner
        .act(GameAction::PassPriority)
        .expect("P0 should pass priority to the Counterspell controller");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P1 }
    ));

    let outcome = runner.cast(counterspell).target_object(copy_id).resolve();

    // CR 701.6a: Counterspell removes the prepared spell without resolving it.
    outcome.assert_zone(&[exile_target], Zone::Battlefield);
    assert!(outcome.state().stack.is_empty());
    // CR 704.5d + CR 704.5e: Once its own spell entry is gone, the synthetic
    // copy's live stack-residency exemption expires and the next SBA makes it cease.
    assert!(!outcome.state().objects.contains_key(&copy_id));
}
