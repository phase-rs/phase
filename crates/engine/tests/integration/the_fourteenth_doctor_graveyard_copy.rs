//! Cluster-100 runtime proof: The Fourteenth Doctor (Secret Lair).
//!
//! Exercises the full card end-to-end and proves the Fix A <-> Fix B seam:
//!   1. The cast trigger reveals the top of the library, MILLS all Doctor cards
//!      to the graveyard (CR 701.17a), and puts the rest on the BOTTOM of the
//!      library (CR 401.4) — not the pre-fix inversion that repositioned the
//!      Doctors to the bottom and dropped the mill.
//!   2. On resolution the permanent enters as a COPY of a Doctor card that was
//!      milled from the library to the graveyard THIS turn (CR 614.1c + 707.9),
//!      and — because the optional copy was performed — gains Haste (CR 603.12 +
//!      702.10).
//!
//! Both abilities are coupled at runtime: the copy source predicate
//! (`ZoneChangedThisTurn { Library -> Graveyard }`) can only be satisfied by the
//! Doctor the cast trigger milled, so the two fixes must land together.

use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

fn cluster_100_db() -> &'static engine::database::card_db::CardDatabase {
    static DB: std::sync::OnceLock<engine::database::card_db::CardDatabase> =
        std::sync::OnceLock::new();
    DB.get_or_init(|| {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/integration_cards.json");
        engine::database::card_db::CardDatabase::from_export(&path).expect("fixture must load")
    })
}

fn add_mana(runner: &mut GameRunner, mana: &[ManaType]) {
    let dummy = engine::types::identifiers::ObjectId(0);
    let pool = &mut runner.state_mut().players[0].mana_pool;
    for m in mana {
        pool.add(ManaUnit::new(*m, dummy, false, vec![]));
    }
}

fn drive_until_stack_empty(runner: &mut GameRunner) {
    for _ in 0..256 {
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
fn the_fourteenth_doctor_mills_doctor_then_enters_as_hasty_copy() {
    let db = cluster_100_db();

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let doctor = scenario.add_real_card(P0, "The Fourteenth Doctor", Zone::Hand, db);
    // Library: one Doctor (milled to graveyard, becomes the copy source) plus two
    // non-Doctor lands (the "rest", sent to the library bottom). RevealTop 14
    // clamps to the whole 3-card library, so ordering is irrelevant.
    let fugitive = scenario.add_real_card(P0, "The Fugitive Doctor", Zone::Library, db);
    let mountain = scenario.add_real_card(P0, "Mountain", Zone::Library, db);
    let forest = scenario.add_real_card(P0, "Forest", Zone::Library, db);

    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    // {R/G}{W}{U}
    add_mana(
        &mut runner,
        &[ManaType::Red, ManaType::White, ManaType::Blue],
    );

    let card_id = runner.state().objects[&doctor].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: doctor,
            card_id,
            targets: vec![],
            payment_mode: engine::types::game_state::CastPaymentMode::Auto,
        })
        .expect("cast The Fourteenth Doctor");

    drive_until_stack_empty(&mut runner);

    // Fix A: the Doctor card was MILLED from the library to the graveyard.
    assert_eq!(
        runner.state().objects[&fugitive].zone,
        Zone::Graveyard,
        "The Fugitive Doctor must be milled to the graveyard, not repositioned in the library"
    );
    // Fix A: the non-Doctor "rest" is on the bottom of the library (still in the
    // library, never milled).
    assert_eq!(
        runner.state().objects[&mountain].zone,
        Zone::Library,
        "the non-Doctor rest stays in the library (on the bottom)"
    );
    assert_eq!(runner.state().objects[&forest].zone, Zone::Library);
    let library = &runner.state().players[0].library;
    assert!(
        library.contains(&mountain) && library.contains(&forest),
        "both lands must be on the bottom of the library"
    );

    // Fix B: The Fourteenth Doctor entered as a COPY of the milled Doctor and,
    // because the optional copy was performed, gained Haste.
    let obj = &runner.state().objects[&doctor];
    assert_eq!(
        obj.zone,
        Zone::Battlefield,
        "The Fourteenth Doctor must resolve onto the battlefield"
    );
    assert_eq!(
        obj.name, "The Fugitive Doctor",
        "it must enter as a copy of the milled Doctor"
    );
    assert!(
        obj.keywords.iter().any(|k| matches!(k, Keyword::Haste)),
        "the copy must gain Haste (reflexive 'if you do'), got {:?}",
        obj.keywords
    );
}
