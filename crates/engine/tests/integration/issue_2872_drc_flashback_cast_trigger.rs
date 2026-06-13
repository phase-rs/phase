//! Regression for issue #2872: Dragon's Rage Channeler's "whenever you cast a
//! noncreature spell" surveil must fire when the spell is cast (put on the
//! stack), not when it resolves.
//!
//! https://github.com/phase-rs/phase/issues/2872

use engine::game::scenario::{GameScenario, P0};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::actions::GameAction;
use engine::types::game_state::{
    CastPaymentMode, CastingVariant, StackEntry, StackEntryKind, WaitingFor,
};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

fn issue_2872_db() -> &'static engine::database::card_db::CardDatabase {
    static DB: std::sync::OnceLock<engine::database::card_db::CardDatabase> =
        std::sync::OnceLock::new();
    DB.get_or_init(|| {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/issue_2872_cards.json");
        engine::database::card_db::CardDatabase::from_export(&path)
            .expect("issue_2872_cards.json fixture must load")
    })
}

/// Exact report scenario: Practiced Offense flashback from graveyard with
/// Hardened Academic on the battlefield also triggers on cards leaving the
/// graveyard. The HA trigger pauses on target selection; DRC's cast trigger must
/// still land on the stack above Practiced Offense, not wait until resolution.
#[test]
fn issue_2872_drc_surveil_with_hardened_academic_graveyard_trigger() {
    let db = issue_2872_db();

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let drc = scenario.add_real_card(P0, "Dragon's Rage Channeler", Zone::Battlefield, db);
    let practiced = scenario.add_real_card(P0, "Practiced Offense", Zone::Graveyard, db);
    let _hardened = scenario.add_real_card(P0, "Hardened Academic", Zone::Battlefield, db);
    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::White, ObjectId(0), false, vec![]),
        ],
    );

    let mut runner = scenario.build();
    runner.state_mut().debug_mode = true;
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    let practiced_card_id = runner.state().objects[&practiced].card_id;

    runner.state_mut().resolving_stack_entry = Some(StackEntry {
        id: ObjectId(9999),
        source_id: ObjectId(9999),
        controller: P0,
        kind: StackEntryKind::Spell {
            card_id: CardId(9999),
            ability: None,
            casting_variant: CastingVariant::Normal,
            actual_mana_spent: 0,
        },
    });

    runner
        .act(GameAction::CastSpell {
            object_id: practiced,
            card_id: runner.state().objects[&practiced].card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("start cast");

    for _ in 0..64 {
        match &runner.state().waiting_for {
            WaitingFor::CastingVariantChoice { options, .. } => {
                let index = options
                    .iter()
                    .position(|o| o.variant == CastingVariant::Flashback)
                    .expect("flashback option");
                runner
                    .act(GameAction::ChooseCastingVariant { index })
                    .expect("choose flashback");
            }
            WaitingFor::TargetSelection { .. } => {
                runner.choose_first_legal_target().expect("choose target");
            }
            WaitingFor::ModeChoice { .. } => {
                runner
                    .act(GameAction::SelectModes { indices: vec![0] })
                    .expect("choose mode");
            }
            WaitingFor::ManaPayment { .. } => {
                runner.act(GameAction::PassPriority).expect("pay");
            }
            WaitingFor::TriggerTargetSelection { .. } => {
                runner.choose_first_legal_target().expect("trigger target");
            }
            WaitingFor::OrderTriggers { .. } => {
                engine::game::triggers::drain_order_triggers_with_identity(runner.state_mut());
            }
            WaitingFor::Priority { .. } => break,
            other => panic!("unexpected state: {other:?}"),
        }
    }

    assert_drc_trigger_on_stack_with_practiced(runner.state(), drc, practiced, practiced_card_id);
}

fn assert_drc_trigger_on_stack_with_practiced(
    state: &engine::types::game_state::GameState,
    drc: ObjectId,
    practiced: ObjectId,
    practiced_card_id: engine::types::identifiers::CardId,
) {
    let practiced_on_stack = state.stack.iter().any(|entry| {
        matches!(
            entry.kind,
            StackEntryKind::Spell {
                card_id,
                ..
            } if card_id == practiced_card_id
        )
    });
    assert!(
        practiced_on_stack,
        "Practiced Offense ({practiced:?}) must be on the stack after casting; \
         zone={:?}, stack={:?}",
        state.objects.get(&practiced).map(|o| o.zone),
        state.stack.len()
    );

    let drc_trigger_on_stack = state.stack.iter().any(|entry| {
        matches!(
            entry.kind,
            StackEntryKind::TriggeredAbility {
                source_id,
                ..
            } if source_id == drc
        )
    });
    assert!(
        drc_trigger_on_stack,
        "Dragon's Rage Channeler surveil trigger must be on the stack when \
         Practiced Offense is cast from graveyard, not only after it resolves; \
         stack entries: {:?}",
        state
            .stack
            .iter()
            .map(|e| format!("{:?}", e.kind))
            .collect::<Vec<_>>()
    );
}
