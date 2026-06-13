//! Regression for issue #2866: Professor Onyx magecraft (SpellCastOrCopy) must
//! fire when its controller copies an instant or sorcery spell, not only when
//! they cast one.
//!
//! https://github.com/phase-rs/phase/issues/2866

use engine::game::scenario::{GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

fn issue_2866_db() -> &'static engine::database::card_db::CardDatabase {
    static DB: std::sync::OnceLock<engine::database::card_db::CardDatabase> =
        std::sync::OnceLock::new();
    DB.get_or_init(|| {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/issue_2866_cards.json");
        engine::database::card_db::CardDatabase::from_export(&path)
            .expect("issue_2866_cards.json fixture must load")
    })
}

fn add_mana(runner: &mut engine::game::scenario::GameRunner, player: PlayerId, mana: &[ManaType]) {
    let dummy = engine::types::identifiers::ObjectId(0);
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

fn player_life(runner: &engine::game::scenario::GameRunner, player: PlayerId) -> i32 {
    runner
        .state()
        .players
        .iter()
        .find(|p| p.id == player)
        .unwrap()
        .life
}

fn resolve_discard_choice(runner: &mut engine::game::scenario::GameRunner) -> bool {
    if let WaitingFor::DiscardChoice { count, cards, .. } = &runner.state().waiting_for {
        let chosen: Vec<engine::types::identifiers::ObjectId> =
            cards.iter().take(*count).copied().collect();
        runner
            .act(GameAction::SelectCards { cards: chosen })
            .expect("resolve discard choice");
        return true;
    }
    false
}

fn drive_to_optional_copy(runner: &mut engine::game::scenario::GameRunner) {
    for _ in 0..100 {
        match &runner.state().waiting_for {
            WaitingFor::OptionalEffectChoice { .. } => return,
            WaitingFor::DiscardChoice { .. } => {
                resolve_discard_choice(runner);
            }
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).expect("pass");
            }
            _ => return,
        }
    }
    panic!("did not reach optional copy prompt");
}

/// P0 casts Chain of Smog on themselves, accepts the copy, and magecraft must
/// fire twice (cast + copy): P1 loses 4 life total and P0 gains 4.
#[test]
fn issue_2866_professor_onyx_magecraft_on_chain_of_smog_copy() {
    let db = issue_2866_db();

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let _onyx = scenario.add_real_card(P0, "Professor Onyx", Zone::Battlefield, db);
    let smog = scenario.add_real_card(P0, "Chain of Smog", Zone::Hand, db);
    for _ in 0..6 {
        scenario.add_card_to_hand(P0, "Mountain");
    }

    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    add_mana(&mut runner, P0, &[ManaType::Black, ManaType::Colorless]);

    let p0_life_before = player_life(&runner, P0);
    let p1_life_before = player_life(&runner, P1);

    let card_id = runner.state().objects[&smog].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: smog,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast Chain of Smog");

    match runner.state().waiting_for.clone() {
        WaitingFor::TargetSelection { .. } => {
            runner
                .act(GameAction::SelectTargets {
                    targets: vec![engine::types::ability::TargetRef::Player(P0)],
                })
                .expect("self-target");
        }
        other => panic!("expected target selection, got {other:?}"),
    }

    drive_to_optional_copy(&mut runner);

    match runner.state().waiting_for.clone() {
        WaitingFor::OptionalEffectChoice { player, .. } => {
            assert_eq!(player, P0, "self-targeted copy prompt goes to P0");
        }
        other => panic!("expected optional copy prompt, got {other:?}"),
    }

    runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .expect("accept copy");

    match runner.state().waiting_for.clone() {
        WaitingFor::CopyRetarget { player, .. } => assert_eq!(player, P0),
        other => panic!("expected CopyRetarget after accepting copy, got {other:?}"),
    }

    // Copy observers must not drain until CopyRetarget completes — only cast magecraft so far.
    assert_eq!(
        player_life(&runner, P1),
        p1_life_before - 2,
        "only cast magecraft should have fired before retarget finalizes"
    );
    assert_eq!(
        player_life(&runner, P0),
        p0_life_before + 2,
        "only cast magecraft should have fired before retarget finalizes"
    );

    runner
        .act(GameAction::KeepAllCopyTargets)
        .expect("keep inherited targets");

    for _ in 0..120 {
        if resolve_discard_choice(&mut runner) {
            continue;
        }
        match &runner.state().waiting_for {
            WaitingFor::OptionalEffectChoice { .. } => {
                runner
                    .act(GameAction::DecideOptionalEffect { accept: false })
                    .expect("decline nested copy");
            }
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).expect("pass");
            }
            _ => {
                runner.act(GameAction::PassPriority).expect("pass");
            }
        }
    }

    assert!(
        runner.state().stack.is_empty(),
        "stack should be empty after resolution"
    );

    // Cast: P1 -2, P0 +2. Copy: P1 -2, P0 +2. P0 also discarded 4 cards.
    assert_eq!(
        player_life(&runner, P1),
        p1_life_before - 4,
        "magecraft must fire on cast and copy (4 life total to opponent)"
    );
    assert_eq!(
        player_life(&runner, P0),
        p0_life_before + 4,
        "magecraft must fire on cast and copy (4 life total gained)"
    );
}
