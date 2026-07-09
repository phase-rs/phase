//! Runtime coverage for Kav Landseeker's delayed sacrifice (EOE).
//!
//! Oracle: "Menace. When this creature enters, create a Lander token. At the
//! beginning of the end step on your next turn, sacrifice that token."
//!
//! The load-bearing rule is CR 513.2: a delayed "at the beginning of the next
//! end step" trigger created OUTSIDE the end step (Kav's ETB resolves in the
//! main phase) would otherwise fire on the CURRENT turn's end step. WotC's
//! wording "the end step ON YOUR NEXT TURN" forces the skip, encoded as
//! `DelayedTriggerCondition::AtNextPhaseForPlayer { gate: TurnGate::After(n) }`
//! (stamped from the parser's symbolic `AfterCreationTurn` at creation).
//!
//! These tests drive the REAL cast + turn machinery (`GameRunner`), never the
//! raw stack resolver.
//!
//! #5072: singleton delayed-trigger scope is deliberate — do not assert
//! multi-trigger dispatch order here.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

/// Kav Landseeker's exact Oracle text (verbatim from card-data.json), so the
/// parser takes the same branch the real card does.
const KAV_ORACLE: &str = "Menace (This creature can't be blocked except by two or more creatures.)\n\
     When this creature enters, create a Lander token. At the beginning of the end step on your next turn, sacrifice that token. (It's an artifact with \"{2}, {T}, Sacrifice this token: Search your library for a basic land card, put it onto the battlefield tapped, then shuffle.\")";

/// Drive the engine through the REAL phase machinery until the active player is
/// in the end step of `target_turn`, exactly as the live driver does: declare
/// no attackers/blockers, drain trigger ordering, answer cleanup discard with
/// no cards, pass priority otherwise. Bounded to guard stalls. Stops at the
/// end-step priority window (any delayed trigger that fired on entry is on the
/// stack, unresolved — the caller resolves it).
fn advance_to_turn_end_step(runner: &mut GameRunner, target_turn: u32) {
    for _ in 0..400 {
        if runner.state().turn_number == target_turn && runner.state().phase == Phase::End {
            return;
        }
        match runner.state().waiting_for.clone() {
            WaitingFor::DeclareAttackers { .. } => {
                runner
                    .act(GameAction::DeclareAttackers {
                        attacks: vec![],
                        bands: vec![],
                    })
                    .expect("declaring no attackers must be accepted");
            }
            WaitingFor::DeclareBlockers { .. } => {
                runner
                    .act(GameAction::DeclareBlockers {
                        assignments: vec![],
                    })
                    .expect("declaring no blockers must be accepted");
            }
            WaitingFor::OrderTriggers { .. } => {
                engine::game::triggers::drain_order_triggers_with_identity(runner.state_mut());
            }
            WaitingFor::DiscardChoice { .. } => {
                runner
                    .act(GameAction::SelectCards { cards: vec![] })
                    .expect("no-op cleanup discard must be accepted");
            }
            WaitingFor::Priority { .. } => {
                if runner.act(GameAction::PassPriority).is_err() {
                    break;
                }
            }
            other => panic!("unexpected WaitingFor while advancing turns: {other:?}"),
        }
    }
    panic!(
        "failed to reach turn {target_turn} end step (stuck at turn {} phase {:?})",
        runner.state().turn_number,
        runner.state().phase
    );
}

/// Find the single Lander token P0 controls on the battlefield.
fn find_lander(runner: &GameRunner, player: PlayerId) -> ObjectId {
    let landers: Vec<ObjectId> = runner
        .state()
        .objects
        .values()
        .filter(|o| {
            o.zone == Zone::Battlefield
                && o.controller == player
                && o.is_token
                && o.name == "Lander"
        })
        .map(|o| o.id)
        .collect();
    assert_eq!(
        landers.len(),
        1,
        "expected exactly one Lander token, got {landers:?}"
    );
    landers[0]
}

/// Whether `obj` is currently a battlefield permanent. A sacrificed token moves
/// to the graveyard and then ceases to exist as a state-based action (CR
/// 704.5d), so it is removed from `state.objects` entirely — hence a plain
/// zone lookup is insufficient; "not on the battlefield" is the sacrifice
/// signal.
fn on_battlefield(runner: &GameRunner, obj: ObjectId) -> bool {
    runner
        .state()
        .objects
        .get(&obj)
        .is_some_and(|o| o.zone == Zone::Battlefield)
}

/// Build P0 with Kav Landseeker castable for free in its precombat main phase.
/// Returns the runner (already built) and Kav's object id.
fn kav_in_hand() -> (GameRunner, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // Stock both libraries so neither player decks out (CR 104.3c) while we
    // advance several turns to reach the controller's next end step.
    scenario.with_library_top(P0, &["Forest", "Forest", "Forest", "Forest", "Forest"]);
    scenario.with_library_top(P1, &["Forest", "Forest", "Forest", "Forest", "Forest"]);
    let kav = {
        let mut builder = scenario.add_creature_to_hand(P0, "Kav Landseeker", 4, 3);
        builder.from_oracle_text_with_keywords(&["Menace"], KAV_ORACLE);
        // Free to cast so the pipeline never surfaces a mana-payment window.
        builder.with_mana_cost(ManaCost::generic(0));
        builder.id()
    };
    (scenario.build(), kav)
}

/// CR 513.2 (paired timing): the Lander survives the CURRENT turn's end step and
/// is sacrificed on the controller's NEXT turn's end step. Revert-to-red: drop
/// the `TurnGate::After(floor)` guard in `delayed_trigger_event` → the Lander is
/// sacrificed at the current end step and the first assertion fails.
#[test]
fn kav_lander_survives_current_end_step_and_dies_next_turn() {
    let (mut runner, kav) = kav_in_hand();

    // Cast Kav (turn 2). Its ETB creates the Lander and arms the delayed
    // sacrifice gated to the controller's next turn.
    runner.cast(kav).resolve();
    assert_eq!(
        runner.state().objects[&kav].zone,
        Zone::Battlefield,
        "Kav must resolve onto the battlefield"
    );
    let lander = find_lander(&runner, P0);

    // CURRENT turn (turn 2) end step: the gate skips it — no trigger goes on the
    // stack, so resolving the stack here is a no-op and the Lander survives.
    // (Resolving BEFORE the assert is load-bearing: without the gate the sacrifice
    // fires and sits on the stack unresolved, so a pre-resolution check would
    // vacuously see the Lander still present.)
    advance_to_turn_end_step(&mut runner, 2);
    runner.advance_until_stack_empty();
    assert!(
        on_battlefield(&runner, lander),
        "CR 513.2: the Lander must NOT be sacrificed at the current turn's end step"
    );

    // Controller's NEXT turn (turn 4; turn 3 is the opponent's) end step: the
    // gate opens — the delayed sacrifice fires and resolves.
    advance_to_turn_end_step(&mut runner, 4);
    runner.advance_until_stack_empty();
    assert!(
        !on_battlefield(&runner, lander),
        "CR 513.2 + CR 603.7c: the Lander must be sacrificed on the controller's next end step"
    );
}

/// CR 603.7c (specific token): with an unrelated second Lander token already on
/// the battlefield when Kav's delayed trigger fires, ONLY Kav's snapshotted
/// token is sacrificed — the delayed effect targets the `LastCreated` id
/// snapshotted at creation, not "every Lander". Revert-to-red on the snapshot:
/// break the creation-time `LastCreated` snapshot and the wrong token (or both)
/// is sacrificed. Also red if Kav's temporal arm is reverted (nothing fires, so
/// Kav's own Lander survives).
#[test]
fn kav_sacrifices_only_its_own_snapshotted_lander() {
    let (mut runner, kav) = kav_in_hand();

    // A pre-existing, unrelated Lander token P0 controls — NOT created by Kav,
    // so it is not in the creation-time `last_created_token_ids` snapshot.
    let bystander = engine::game::zones::create_object(
        runner.state_mut(),
        engine::types::identifiers::CardId(9999),
        P0,
        "Lander".to_string(),
        Zone::Battlefield,
    );
    {
        let obj = runner.state_mut().objects.get_mut(&bystander).unwrap();
        obj.is_token = true;
        obj.card_types.subtypes.push("Lander".to_string());
    }

    runner.cast(kav).resolve();
    // Kav's own Lander is the one created during the ETB (distinct from the
    // bystander).
    let kav_lander = runner
        .state()
        .objects
        .values()
        .find(|o| {
            o.zone == Zone::Battlefield
                && o.controller == P0
                && o.is_token
                && o.name == "Lander"
                && o.id != bystander
        })
        .map(|o| o.id)
        .expect("Kav's ETB must create its own Lander token");

    advance_to_turn_end_step(&mut runner, 4);
    runner.advance_until_stack_empty();

    assert!(
        !on_battlefield(&runner, kav_lander),
        "Kav's snapshotted Lander must be sacrificed"
    );
    assert!(
        on_battlefield(&runner, bystander),
        "CR 603.7c: the unrelated bystander Lander must NOT be sacrificed"
    );
}
