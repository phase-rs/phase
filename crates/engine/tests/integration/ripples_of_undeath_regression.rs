//! Regression: GitHub issue #322 — Ripples of Undeath ("At the beginning of
//! your first main phase, mill three cards. Then you may pay {1} and 3 life.
//! If you do, put a card from among those cards into your hand.").
//!
//! User report (Discord, 2026-05-10): the trigger never fires when the
//! controller's precombat main phase begins. CR 505.1a establishes "first
//! main phase" as a synonym for "precombat main phase" — the parser already
//! emits `phase: PreCombatMain` with `OnlyDuringYourTurn`, so the matcher
//! must drive the trigger onto the stack as `auto_advance` enters
//! `Phase::PreCombatMain` (CR 603.6 / CR 603.2b).
//!
//! These tests pin the end-to-end flow against real parsed card data: load
//! Ripples of Undeath from the database, place it on the battlefield, run
//! `auto_advance` from `Phase::Untap`, and assert the Mill effect lands the
//! trigger on the stack with the expected `Effect::Mill` resolution shape.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, StackEntryKind, WaitingFor};
use engine::types::identifiers::CardId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

use crate::support::shared_card_db as load_db;

/// Stack the controller's library so the Mill effect has cards to move.
fn seed_library(runner: &mut GameRunner, count: usize) {
    let state = runner.state_mut();
    for i in 0..count {
        let card_id = CardId(state.next_object_id);
        let id = engine::game::zones::create_object(
            state,
            card_id,
            P0,
            format!("Library Card {i}"),
            Zone::Library,
        );
        let _ = id;
    }
}

fn add_mana(runner: &mut GameRunner, mana: &[ManaType]) {
    let dummy = engine::types::identifiers::ObjectId(0);
    let pool = &mut runner
        .state_mut()
        .players
        .iter_mut()
        .find(|p| p.id == P0)
        .unwrap()
        .mana_pool;
    for m in mana {
        pool.add(ManaUnit::new(*m, dummy, false, vec![]));
    }
}

/// CR 505.1a + CR 603.6 + CR 603.2b: When P0 controls Ripples of Undeath and
/// `auto_advance` enters their precombat main phase, the trigger must fire
/// and land the `Mill 3` ability on the stack before P0 receives priority.
#[test]
fn ripples_of_undeath_triggers_at_precombat_main_phase() {
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    let _ripples = scenario.add_real_card(P0, "Ripples of Undeath", Zone::Battlefield, db);
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    // Pre-existing battlefield permanent — already past the previous turn.
    runner.state_mut().turn_number = 2;
    runner.state_mut().phase = Phase::Untap;
    runner.state_mut().active_player = P0;
    runner.state_mut().priority_player = P0;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P0 };

    // Seed both libraries: P0 needs cards to mill + draw step, P1 just to be alive.
    seed_library(&mut runner, 5);

    // Drive auto_advance from Untap → Upkeep → Draw → PreCombatMain. The
    // PreCombatMain arm calls `process_phase_triggers`, which must land
    // Ripples's Mill ability on the stack.
    // CR 117.1c: priority opens during Upkeep + Draw — the helper drains
    // them after `auto_advance` to land in PreCombatMain.
    let waiting = runner.auto_advance_to_main_phase();

    assert_eq!(runner.state().phase, Phase::PreCombatMain);
    assert!(
        matches!(waiting, WaitingFor::Priority { player } if player == P0),
        "Expected Priority for P0 after PreCombatMain trigger queued, got {:?}",
        waiting
    );

    // The Mill 3 ability must be on the stack as a TriggeredAbility.
    assert_eq!(
        runner.state().stack.len(),
        1,
        "Ripples trigger must place exactly one ability on the stack at PreCombatMain"
    );
    assert!(
        matches!(
            runner.state().stack[0].kind,
            StackEntryKind::TriggeredAbility { .. }
        ),
        "Stack entry must be a TriggeredAbility, got {:?}",
        runner.state().stack[0].kind
    );
}

/// CR 603.2 + `OnlyDuringYourTurn` constraint: the trigger must NOT fire
/// during the opponent's precombat main phase. Same setup as the positive
/// test but the active player is P1.
#[test]
fn ripples_of_undeath_does_not_trigger_on_opponents_turn() {
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    let _ripples = scenario.add_real_card(P0, "Ripples of Undeath", Zone::Battlefield, db);
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    runner.state_mut().turn_number = 2;
    runner.state_mut().phase = Phase::Untap;
    runner.state_mut().active_player = P1;
    runner.state_mut().priority_player = P1;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P1 };

    {
        let state = runner.state_mut();
        for i in 0..5usize {
            let card_id = CardId(state.next_object_id);
            engine::game::zones::create_object(
                state,
                card_id,
                P1,
                format!("Library Card {i}"),
                Zone::Library,
            );
        }
    }

    // CR 117.1c: drain Upkeep + Draw priority via the helper to reach Main.
    let _waiting = runner.auto_advance_to_main_phase();

    assert_eq!(runner.state().phase, Phase::PreCombatMain);
    assert!(
        runner.state().stack.is_empty(),
        "Ripples trigger must NOT fire on opponent's precombat main \
         (OnlyDuringYourTurn constraint); stack={:?}",
        runner.state().stack
    );
}

/// CR 603.2 + CR 603.6: Casting Ripples of Undeath, advancing through end-of-turn,
/// then advancing into the controller's next precombat main phase must fire
/// the trigger. This mirrors the user-reported gameplay flow: resolve the
/// enchantment onto the battlefield and observe that the next "your first
/// main phase" beat queues the Mill 3 onto the stack.
#[test]
fn ripples_of_undeath_triggers_after_being_cast_and_next_turn() {
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let ripples_id = scenario.add_real_card(P0, "Ripples of Undeath", Zone::Hand, db);
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    add_mana(&mut runner, &[ManaType::Black, ManaType::Colorless]);
    seed_library(&mut runner, 10);
    // Seed P1's library so they don't deck out before reaching P0's next turn.
    {
        let state = runner.state_mut();
        for i in 0..10usize {
            let card_id = CardId(state.next_object_id);
            engine::game::zones::create_object(
                state,
                card_id,
                P1,
                format!("P1 Library Card {i}"),
                Zone::Library,
            );
        }
    }

    let card_id = runner.state().objects[&ripples_id].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: ripples_id,
            card_id,
            targets: vec![],

            payment_mode: CastPaymentMode::Auto,
        })
        .expect("Ripples cast should be accepted");

    runner.advance_until_stack_empty();

    // Ripples is now on the battlefield. Confirm before advancing turns.
    assert_eq!(
        runner.state().objects[&ripples_id].zone,
        Zone::Battlefield,
        "Ripples must resolve onto the battlefield before the next-turn flow"
    );

    // Pass priority through the rest of P0's turn and through P1's turn back
    // to P0's NEXT precombat main phase (must be a strictly later turn than the
    // one on which Ripples was cast; same-turn precombat-main is excluded).
    let cast_turn = runner.state().turn_number;
    let mut guard = 0;
    while !(runner.state().phase == Phase::PreCombatMain
        && runner.state().active_player == P0
        && runner.state().turn_number > cast_turn)
    {
        guard += 1;
        if guard > 200 {
            panic!(
                "Failed to reach P0's next precombat main; phase={:?} \
                 active={:?} turn={} waiting={:?}",
                runner.state().phase,
                runner.state().active_player,
                runner.state().turn_number,
                runner.waiting_for_kind(),
            );
        }
        let waiting = runner.state().waiting_for.clone();
        match waiting {
            WaitingFor::Priority { .. } => {
                let _ = runner.act(GameAction::PassPriority);
            }
            WaitingFor::DeclareAttackers { .. } => {
                let _ = runner.act(GameAction::DeclareAttackers {
                    attacks: vec![],
                    bands: vec![],
                });
            }
            WaitingFor::DeclareBlockers { .. } => {
                let _ = runner.act(GameAction::DeclareBlockers {
                    assignments: vec![],
                });
            }
            WaitingFor::DiscardToHandSize {
                count, ref cards, ..
            } => {
                let chosen: Vec<_> = cards.iter().take(count).copied().collect();
                let _ = runner.act(GameAction::SelectCards { cards: chosen });
            }
            WaitingFor::OptionalEffectChoice { .. } => {
                let _ = runner.act(GameAction::DecideOptionalEffect { accept: false });
            }
            ref other => {
                panic!("Unexpected waiting_for during phase advance: {:?}", other);
            }
        }
    }

    // CR 505.1a + CR 603.6 + CR 603.2b: At P0's next precombat main, the
    // Ripples trigger must be on the stack as a TriggeredAbility.
    assert_eq!(runner.state().phase, Phase::PreCombatMain);
    assert_eq!(runner.state().active_player, P0);
    let ripples_obj = &runner.state().objects[&ripples_id];
    assert!(
        !runner.state().stack.is_empty(),
        "Ripples trigger must fire at P0's next precombat main phase; \
         stack={:?} ripples_zone={:?} ripples_trigger_defs={} \
         ripples_etb_turn={:?} turn_number={}",
        runner.state().stack,
        ripples_obj.zone,
        ripples_obj.trigger_definitions.len(),
        ripples_obj.entered_battlefield_turn,
        runner.state().turn_number,
    );
    assert!(
        runner
            .state()
            .stack
            .iter()
            .any(|e| matches!(e.kind, StackEntryKind::TriggeredAbility { .. })),
        "Stack must contain a TriggeredAbility from Ripples; \
         stack kinds={:?}",
        runner
            .state()
            .stack
            .iter()
            .map(|e| &e.kind)
            .collect::<Vec<_>>()
    );
}

/// CR 701.13a + CR 608.2c: Resolving the Ripples trigger must mill exactly
/// three cards from the controller's library into their graveyard. After the
/// trigger resolves, the optional pay+life sub-ability prompts the controller
/// (modeled as `WaitingFor::OptionalEffectChoice`).
#[test]
fn ripples_of_undeath_resolves_mill_three_cards() {
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    let _ripples = scenario.add_real_card(P0, "Ripples of Undeath", Zone::Battlefield, db);
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    runner.state_mut().turn_number = 2;
    runner.state_mut().phase = Phase::Untap;
    runner.state_mut().active_player = P0;
    runner.state_mut().priority_player = P0;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P0 };

    seed_library(&mut runner, 5);

    // CR 117.1c: drain Upkeep + Draw priority via the helper to reach Main.
    let _waiting = runner.auto_advance_to_main_phase();

    assert_eq!(runner.state().phase, Phase::PreCombatMain);
    assert_eq!(
        runner.state().stack.len(),
        1,
        "Ripples trigger must be on the stack"
    );

    // Snapshot AFTER the draw step has consumed 1 card but BEFORE the trigger
    // resolves — this isolates the Mill 3 delta from any other library reads.
    let library_size_before = runner.state().players[0].library.len();
    let graveyard_size_before = runner.state().players[0].graveyard.len();

    // Resolve the trigger: P0 then P1 pass priority. The Mill 3 effect fires,
    // moving 3 cards from P0's library to graveyard. The optional sub-ability
    // ("Then you may pay {1} and 3 life") prompts next via OptionalEffectChoice.
    let _ = runner.act(GameAction::PassPriority);
    let _ = runner.act(GameAction::PassPriority);

    // CR 701.13a: Mill 3 — exactly three cards moved from library top to
    // graveyard.
    let library_size_after = runner.state().players[0].library.len();
    let graveyard_size_after = runner.state().players[0].graveyard.len();
    assert_eq!(
        library_size_before - library_size_after,
        3,
        "Mill must remove exactly 3 cards from library; \
         before={library_size_before} after={library_size_after}"
    );
    assert_eq!(
        graveyard_size_after - graveyard_size_before,
        3,
        "Mill must add exactly 3 cards to graveyard; \
         before={graveyard_size_before} after={graveyard_size_after}"
    );
}

/// CR 608.2c + CR 700.2: After milling 3 cards, declining the optional
/// "pay {1} and 3 life" must leave the milled cards in the graveyard and
/// NOT move any of them to hand. This is the baseline for the "those cards"
/// tracked-set follow-up — no payment, no hand move.
#[test]
fn ripples_of_undeath_decline_optional_keeps_milled_cards_in_graveyard() {
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    let _ripples = scenario.add_real_card(P0, "Ripples of Undeath", Zone::Battlefield, db);
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    runner.state_mut().turn_number = 2;
    runner.state_mut().phase = Phase::Untap;
    runner.state_mut().active_player = P0;
    runner.state_mut().priority_player = P0;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P0 };

    seed_library(&mut runner, 5);

    // CR 117.1c: drain Upkeep + Draw priority via the helper to reach Main.
    let _waiting = runner.auto_advance_to_main_phase();

    // Resolve the trigger — Mill 3 fires.
    let _ = runner.act(GameAction::PassPriority);
    let _ = runner.act(GameAction::PassPriority);

    // Decline the optional pay+life prompt.
    if matches!(
        runner.state().waiting_for,
        WaitingFor::OptionalEffectChoice { .. }
    ) {
        runner
            .act(GameAction::DecideOptionalEffect { accept: false })
            .expect("decline optional should succeed");
    }

    // After declining, all 3 milled cards must remain in the graveyard.
    // (The 1 drawn card from the draw step is in hand and is NOT part of the
    // milled set, so we do not assert hand emptiness.)
    assert_eq!(
        runner.state().players[0].graveyard.len(),
        3,
        "3 milled cards must remain in graveyard when payment declined"
    );
}
