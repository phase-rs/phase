//! Web of Inertia — "At the beginning of combat on each opponent's turn,
//! that player may exile a card from their graveyard. If the player doesn't,
//! creatures they control can't attack you this turn."
//!
//! Row 11b (runtime pin for the guard-test row 11 parse shape): declining
//! installs a `ScopedPlayer` (at parse; lowered to `SpecificPlayer` at
//! creation) / `EndOfTurn` / `Attack { defended: Player }` ban through the
//! bare-`Player` defended arm (`restrictions.rs:2476`) — the arm rows 2/2c/4
//! (subtype-defended, `:2485-2494`) cannot reach.
//!
//! CR 508.1c: attack restrictions checked at declare-attackers. CR 109.5: the
//! defended "you" snapshots the resolving controller. (CR numbers inherited
//! from the in-code authorities cited in the implementation;
//! docs/MagicCompRules.txt is absent in this checkout.)

use engine::game::combat::{declare_attackers, AttackTarget};
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::triggers::process_triggers;
use engine::game::zones::create_object;
use engine::types::ability::{GameRestriction, ProhibitedActivity};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::events::GameEvent;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const P2: PlayerId = PlayerId(2);

/// Verbatim Oracle text (Web of Inertia).
const WEB_OF_INERTIA_ORACLE: &str = "At the beginning of combat on each opponent's turn, that player may exile a card from their graveyard. If the player doesn't, creatures they control can't attack you this turn.";

/// Imperative planeswalker (sibling fixtures whose Oracle text is not under
/// test — mirrors the Jace suite's `add_planeswalker` fixture).
fn add_planeswalker(
    state: &mut GameState,
    controller: PlayerId,
    name: &str,
    subtype: &str,
) -> ObjectId {
    let card = CardId(state.next_object_id);
    let id = create_object(state, card, controller, name.to_string(), Zone::Battlefield);
    let obj = state.objects.get_mut(&id).unwrap();
    obj.card_types.core_types = vec![CoreType::Planeswalker];
    obj.card_types.subtypes = vec![subtype.to_string()];
    obj.base_card_types = obj.card_types.clone();
    id
}

/// Fire the beginning-of-combat trigger for the current active player
/// (production phase event).
fn fire_begin_combat(runner: &mut GameRunner) {
    process_triggers(
        runner.state_mut(),
        &[GameEvent::PhaseChanged {
            phase: Phase::BeginCombat,
        }],
    );
}

/// Pass priority until the exile-choice optional prompt surfaces (or the stack
/// drains, which fails loudly at the call site).
fn drive_to_optional_prompt(runner: &mut GameRunner) {
    for _ in 0..100 {
        if matches!(
            runner.state().waiting_for,
            WaitingFor::OptionalEffectChoice { .. }
        ) {
            return;
        }
        if runner.state().stack.is_empty() {
            return;
        }
        if runner.act(GameAction::PassPriority).is_err() {
            return;
        }
    }
}

/// Drain to an idle empty stack. A further `OptionalEffectChoice` here is
/// UNEXPECTED and fails loudly: every decision under test was already made by
/// the caller.
fn drive_to_idle(runner: &mut GameRunner) {
    for _ in 0..100 {
        match &runner.state().waiting_for {
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => return,
            WaitingFor::OptionalEffectChoice { .. } => {
                panic!(
                    "unexpected optional-effect prompt during drive_to_idle: {:?} — \
                     every decision this test exercises must already be settled by now",
                    runner.state().waiting_for
                );
            }
            _ => {
                if runner.act(GameAction::PassPriority).is_err() {
                    return;
                }
            }
        }
    }
    panic!("resolution did not settle within the budget");
}

/// Assert the exile-choice prompt addresses `player`, answer it, and drain.
fn answer_optional(runner: &mut GameRunner, player: PlayerId, accept: bool) {
    match runner.state().waiting_for.clone() {
        WaitingFor::OptionalEffectChoice {
            player: prompted, ..
        } => {
            assert_eq!(
                prompted, player,
                "the exile choice must be offered to the triggering opponent"
            );
        }
        other => panic!("expected the exile-choice optional prompt, got {other:?}"),
    }
    runner
        .act(GameAction::DecideOptionalEffect { accept })
        .expect("answering the exile choice succeeds");
    drive_to_idle(runner);
    assert!(
        runner.state().stack.is_empty(),
        "resolution must settle with an empty stack"
    );
}

fn attack_is_legal(
    runner: &GameRunner,
    controller: PlayerId,
    attacker: ObjectId,
    target: AttackTarget,
) -> bool {
    let mut state = runner.state().clone();
    state.active_player = controller;
    let mut events = Vec::new();
    declare_attackers(&mut state, &[(attacker, target)], &mut events).is_ok()
}

fn has_attack_ban(runner: &GameRunner) -> bool {
    runner.state().restrictions.iter().any(|r| {
        matches!(
            r,
            GameRestriction::ProhibitActivity {
                activity: ProhibitedActivity::Attack { .. },
                ..
            }
        )
    })
}

/// 3-player board: P0 controls Web of Inertia (verbatim Oracle text), P1
/// (Amy) and P2 (Bob) each have an attacker, and P1 has a graveyard card so
/// the decline prompt is offered. Active player is P1 at beginning of combat.
fn inertia_three_player_scenario() -> (GameRunner, ObjectId, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_enchantment_from_oracle(P0, "Web of Inertia", WEB_OF_INERTIA_ORACLE);
    let amy_attacker = scenario.add_creature(P1, "Amy Bear", 2, 2).id();
    let bob_attacker = scenario.add_creature(P2, "Bob Bear", 2, 2).id();
    scenario.with_graveyard(P1, &["Spare Card"]);
    let mut runner = scenario.build();
    let chandra = add_planeswalker(runner.state_mut(), P0, "Chandra", "Chandra");
    runner.state_mut().active_player = P1;
    runner.state_mut().priority_player = P1;
    runner.state_mut().phase = Phase::BeginCombat;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P1 };
    (runner, amy_attacker, bob_attacker, chandra)
}

/// Row 11b — declining Inertia's exile choice bans the declining opponent's
/// creatures from attacking the controller, through the bare-`Player` arm:
/// the controller is illegal, while a planeswalker, another player, and the
/// second opponent stay legal.
///
/// REVERT-FAIL (`restrictions.rs:2476` arm): the controller attack goes
/// through (ban silently lost). Assertion (i) is the install reach-guard;
/// (iii) rules out the attacker-incapacity shape that would make (ii) pass
/// vacuously; (v) is the multi-authority hostile (row-4 pattern).
#[test]
fn web_of_inertia_decline_bans_controller_attacks_only() {
    let (mut runner, amy_attacker, bob_attacker, chandra) = inertia_three_player_scenario();

    fire_begin_combat(&mut runner);
    drive_to_optional_prompt(&mut runner);
    answer_optional(&mut runner, P1, false);

    // (i) the decline installs the prohibition.
    assert!(
        has_attack_ban(&runner),
        "declining installs the attack prohibition"
    );
    // (ii) REVERT-FAIL: without the :2476 arm this attack is legal.
    assert!(
        !attack_is_legal(&runner, P1, amy_attacker, AttackTarget::Player(P0)),
        "CR 508.1c: P1 can't attack P0 after declining"
    );
    // (iii)–(v) scope-matrix negatives — same attacker, sibling targets stay
    // legal; the second opponent is unaffected.
    assert!(
        attack_is_legal(
            &runner,
            P1,
            amy_attacker,
            AttackTarget::Planeswalker(chandra)
        ),
        "the ban defends P0, not P0's planeswalkers"
    );
    assert!(
        attack_is_legal(&runner, P1, amy_attacker, AttackTarget::Player(P2)),
        "P1 can still attack the other opponent"
    );
    assert!(
        attack_is_legal(&runner, P2, bob_attacker, AttackTarget::Player(P0)),
        "P2 attacks P0 freely while P1 is banned"
    );
}

/// Row 11b accept-path probe (kept only if the harness yields it without new
/// machinery): accepting exiles the graveyard card and installs NO ban.
#[test]
fn web_of_inertia_accept_exiles_and_installs_no_ban() {
    let (mut runner, amy_attacker, _, _) = inertia_three_player_scenario();

    fire_begin_combat(&mut runner);
    drive_to_optional_prompt(&mut runner);
    answer_optional(&mut runner, P1, true);

    assert!(
        runner
            .state()
            .objects
            .values()
            .any(|o| o.name == "Spare Card" && o.zone == Zone::Exile),
        "accepting exiles the offered graveyard card"
    );
    assert!(
        !has_attack_ban(&runner),
        "accepting installs no prohibition"
    );
    assert!(
        attack_is_legal(&runner, P1, amy_attacker, AttackTarget::Player(P0)),
        "P1 attacks P0 freely after accepting"
    );
}
