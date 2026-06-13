//! Regression for issue #2860: Sevinne's Reclamation cast from graveyard via
//! flashback must offer the optional self-copy on resolution.
//!
//! https://github.com/phase-rs/phase/issues/2860

use engine::game::scenario::{GameScenario, P0};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

fn issue_2860_db() -> &'static engine::database::card_db::CardDatabase {
    static DB: std::sync::OnceLock<engine::database::card_db::CardDatabase> =
        std::sync::OnceLock::new();
    DB.get_or_init(|| {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/issue_2860_cards.json");
        engine::database::card_db::CardDatabase::from_export(&path)
            .expect("issue_2860_cards.json fixture must load")
    })
}

fn add_mana(
    runner: &mut engine::game::scenario::GameRunner,
    player: engine::types::player::PlayerId,
    mana: &[ManaType],
) {
    let dummy = ObjectId(0);
    let pool = &mut runner
        .state_mut()
        .players
        .iter_mut()
        .find(|p| p.id == player)
        .unwrap()
        .mana_pool;
    for m in mana {
        pool.add(ManaUnit::new(*m, dummy, false, vec![]));
    }
}

fn drive_to_optional_copy(runner: &mut engine::game::scenario::GameRunner) {
    for _ in 0..64 {
        match runner.state().waiting_for.clone() {
            WaitingFor::CastingVariantChoice { options, .. } => {
                let index = options
                    .iter()
                    .position(|o| o.variant == engine::types::game_state::CastingVariant::Flashback)
                    .expect("flashback option");
                runner
                    .act(GameAction::ChooseCastingVariant { index })
                    .expect("choose flashback");
            }
            WaitingFor::TargetSelection { .. } => {
                runner.choose_first_legal_target().expect("choose target");
            }
            WaitingFor::ManaPayment { .. } => {
                runner.act(GameAction::PassPriority).expect("pay mana");
            }
            WaitingFor::OrderTriggers { .. } => {
                engine::game::triggers::drain_order_triggers_with_identity(runner.state_mut());
            }
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            WaitingFor::OptionalEffectChoice { .. } => return,
            other => panic!("unexpected waiting state while resolving: {other:?}"),
        }
    }
    panic!("resolution loop exhausted without reaching OptionalEffectChoice");
}

#[test]
fn issue_2860_flashback_cast_offers_optional_spell_copy() {
    let db = issue_2860_db();

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let reclamation = scenario.add_real_card(P0, "Sevinne's Reclamation", Zone::Graveyard, db);
    let target_creature = scenario.add_real_card(P0, "Grizzly Bears", Zone::Graveyard, db);
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    add_mana(
        &mut runner,
        P0,
        &[
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::White,
        ],
    );

    let card_id = runner.state().objects[&reclamation].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: reclamation,
            card_id,
            targets: vec![target_creature],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("start flashback cast");

    drive_to_optional_copy(&mut runner);

    assert!(
        runner.state().battlefield.contains(&target_creature),
        "Grizzly Bears should return to the battlefield"
    );
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::OptionalEffectChoice { .. }
        ),
        "flashback cast from graveyard must offer optional spell copy; got {:?}",
        runner.state().waiting_for
    );

    let stack_before = runner.state().stack.len();
    runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .expect("accept optional copy");

    if matches!(runner.state().waiting_for, WaitingFor::CopyRetarget { .. }) {
        runner
            .act(GameAction::KeepAllCopyTargets)
            .expect("keep copy targets");
    }

    assert!(
        runner.state().stack.len() > stack_before,
        "accepting the optional copy must put a copy of Sevinne's Reclamation on the stack"
    );
}
