//! DynQty subgroup D collateral — `FilterProp::Goaded` as an **Attacks-trigger
//! subject** filter, driven end-to-end through `apply()`.
//!
//! PR #6110 (dq-d) adds `FilterProp::Goaded`. Besides the intended Serene Sleuth
//! `ObjectCount` (battlefield-scan) lift, it now also parses the "goaded creature"
//! subject of TRIGGERS — e.g. Vengeful Ancestor's "Whenever a goaded creature
//! attacks, it deals 1 damage to its controller." (verified verbatim against
//! card-data). That is a DISTINCT new wire from the ObjectCount scan: the trigger's
//! `valid_card` Goaded filter is evaluated against the attacking object.
//!
//! The risk the driver flagged: `EventObjectSnapshot` (types/events.rs) carries no
//! goaded field. If the trigger's `valid_card` were evaluated against the fieldless
//! snapshot instead of the LIVE attacker, `Goaded` would always read false, the
//! trigger would NEVER fire, and the card would be FALSE-SUPPORTED (strictly worse
//! than the pre-PR Unknown). This runtime pair proves the eval resolves against the
//! live attacker (which carries `goaded_by`):
//!   - goaded leg   → the goaded attacker's controller loses exactly 1 life.
//!   - ungoaded leg → identical setup minus the goad → controller loses 0 life.
//!
//! The two legs differ ONLY in the Ox's goaded designation, so the delta isolates
//! the `FilterProp::Goaded` evaluation on the trigger subject.
//!
//! CR references:
//!   - CR 701.15b/c: a creature is goaded iff at least one player has goaded it;
//!     it must attack and attack a player other than its goader if able.
//!   - CR 508.1a: a creature attacks only on its controller's turn.
//!   - CR 508.2a + CR 603.2: an attacks-triggered ability triggers at the point a
//!     creature is declared as an attacker; its trigger event is that attacking
//!     creature, so the `valid_card` filter is evaluated against the live attacker,
//!     not the trigger's own source.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{FilterProp, TargetFilter};
use engine::types::actions::GameAction;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::triggers::TriggerMode;

use super::rules::AttackTarget;

const P2: PlayerId = PlayerId(2);

// Vengeful Ancestor's punisher trigger sentence in isolation (verbatim). The
// runtime creature carries ONLY this trigger — not the Flying keyword and not the
// "enters or attacks, goad target creature" sibling — so the life delta measures the
// goaded-attack punish alone.
const VENGEFUL_GOADED_ATTACK_TRIGGER: &str =
    "Whenever a goaded creature attacks, it deals 1 damage to its controller.";

fn life_of(state: &GameState, player: PlayerId) -> i32 {
    state
        .players
        .iter()
        .find(|p| p.id == player)
        .map(|p| p.life)
        .expect("player exists")
}

/// Build a 3-player game (P0 controls Vengeful Ancestor, P1 controls the Ox that
/// will attack, P2 is the goader) in P1's precombat main, ready to advance into
/// P1's declare-attackers step. When `goaded`, the Ox is designated goaded by P2.
///
/// Goader = P2 ≠ the attacked player (P0), so goad's "attack a player other than the
/// goader if able" (CR 701.15b) is satisfied by attacking P0 — the manual attack
/// declaration is legal in both legs.
fn vengeful_runner(goaded: bool) -> (GameRunner, engine::types::identifiers::ObjectId) {
    let mut scenario = GameScenario::new_n_player(3, 20);
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_creature(P0, "Vengeful Ancestor", 3, 2)
        .from_oracle_text(VENGEFUL_GOADED_ATTACK_TRIGGER);
    let ox = scenario.add_creature(P1, "Ornery Ox", 2, 2).id();
    let mut runner = scenario.build();
    if goaded {
        // CR 701.15b/c: designate the Ox as goaded by P2. A nonempty `goaded_by` set is
        // exactly what `FilterProp::Goaded` reads on the LIVE object.
        runner
            .state_mut()
            .objects
            .get_mut(&ox)
            .expect("Ox exists")
            .goaded_by
            .insert(P2);
    }
    // CR 508.1a: a creature attacks only on its controller's turn — hand the turn to
    // P1 and advance to P1's declare-attackers step.
    hand_turn_to(&mut runner, P1);
    (runner, ox)
}

/// Move the turn to `attacker` and advance to the declare-attackers step, mirroring
/// `total_war_attacking_player_scope::hand_turn_to`. Sets `active_player`,
/// `priority_player`, and `waiting_for` consistently, then passes priority until the
/// engine surfaces the declare-attackers turn-based action (CR 508.1).
fn hand_turn_to(runner: &mut GameRunner, attacker: PlayerId) {
    runner.state_mut().active_player = attacker;
    runner.state_mut().priority_player = attacker;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: attacker };
    for _ in 0..16 {
        if runner.waiting_for_kind() == "DeclareAttackers" {
            return;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("priority pass should advance toward declare attackers");
    }
    panic!("expected DeclareAttackers");
}

/// Reach-guard: the isolated sentence must parse to an `Attacks` trigger whose
/// `valid_card` is a `Typed` filter carrying `FilterProp::Goaded`. If the dq-d parse
/// regressed, this fails first and the runtime legs below are not vacuous.
fn assert_goaded_attacks_trigger_parses() {
    let parsed = parse_oracle_text(
        VENGEFUL_GOADED_ATTACK_TRIGGER,
        "Vengeful Ancestor",
        &[],
        &[],
        &[],
    );
    let attacks = parsed
        .triggers
        .iter()
        .find(|t| t.mode == TriggerMode::Attacks)
        .expect("the sentence parses to an Attacks trigger");
    match attacks.valid_card.as_ref() {
        Some(TargetFilter::Typed(t)) => assert!(
            t.properties.contains(&FilterProp::Goaded),
            "reach-guard: the Attacks trigger's valid_card must carry FilterProp::Goaded, got {:?}",
            t.properties
        ),
        other => panic!("reach-guard: valid_card must be a Typed Goaded filter, got {other:?}"),
    }
}

/// Positive leg — a GOADED opponent creature attacks. Vengeful Ancestor's trigger
/// matches the live attacker's `goaded_by` (CR 701.15b/c), fires, and the attacker
/// deals 1 damage to its controller (P1). P1 loses exactly 1 life.
///
/// Revert-probe: this assertion (P1: 20 → 19) FLIPS to fail if the trigger's Goaded
/// filter is evaluated against the fieldless `EventObjectSnapshot` instead of the
/// live attacker — the trigger would not fire and P1 would stay at 20. It also flips
/// if the whole `FilterProp::Goaded` parse addition is reverted (the reach-guard
/// fails first).
#[test]
fn vengeful_ancestor_goaded_attacker_loses_one_life() {
    assert_goaded_attacks_trigger_parses();

    let (mut runner, ox) = vengeful_runner(true);
    let p1_before = life_of(runner.state(), P1);
    assert_eq!(p1_before, 20, "precondition: P1 starts at 20 life");

    runner
        .declare_attackers(&[(ox, AttackTarget::Player(P0))])
        .expect("declaring the goaded Ox attacking P0 is legal");
    runner.advance_until_stack_empty();

    assert_eq!(
        life_of(runner.state(), P1),
        19,
        "the goaded attacker's controller (P1) must lose exactly 1 life — the trigger fired \
         against the LIVE attacker's goaded_by, not a fieldless snapshot"
    );
}

/// Negative leg (revert-probe pair) — IDENTICAL setup with the goad removed. The
/// trigger's `valid_card` Goaded filter no longer matches the (ungoaded) attacker,
/// so it does NOT fire and P1's life is unchanged. Differs from the positive leg
/// ONLY in the Ox's goaded designation, isolating the `FilterProp::Goaded` eval.
///
/// Reach-guard (non-vacuous): the Ox still attacks and the trigger source (Vengeful
/// Ancestor) is still present — the trigger is genuinely offered and declines only
/// because the subject is not goaded, not because the attack never happened.
#[test]
fn vengeful_ancestor_ungoaded_attacker_loses_no_life() {
    assert_goaded_attacks_trigger_parses();

    let (mut runner, ox) = vengeful_runner(false);
    assert!(
        runner
            .state()
            .objects
            .get(&ox)
            .expect("Ox exists")
            .goaded_by
            .is_empty(),
        "reach-guard: the Ox is genuinely ungoaded in the negative leg"
    );
    assert_eq!(
        life_of(runner.state(), P1),
        20,
        "precondition: P1 starts at 20 life"
    );

    runner
        .declare_attackers(&[(ox, AttackTarget::Player(P0))])
        .expect("declaring the ungoaded Ox attacking P0 is legal");
    runner.advance_until_stack_empty();

    assert_eq!(
        life_of(runner.state(), P1),
        20,
        "an ungoaded attacker must not trigger the goaded-attack punisher — P1 loses no life"
    );
}
