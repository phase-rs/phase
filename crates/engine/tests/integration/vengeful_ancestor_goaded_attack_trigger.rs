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

use engine::game::combat::validate_blockers_for_player;
use engine::game::filter::{matches_target_filter, FilterContext};
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{
    ContinuousModification, Duration, FilterProp, TargetFilter, TypedFilter,
};
use engine::types::actions::GameAction;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::statics::StaticMode;
use engine::types::triggers::TriggerMode;
use engine::types::zones::Zone;

use super::rules::AttackTarget;

const P2: PlayerId = PlayerId(2);

// Vengeful Ancestor's punisher trigger sentence in isolation (verbatim). The
// runtime creature carries ONLY this trigger — not the Flying keyword and not the
// "enters or attacks, goad target creature" sibling — so the life delta measures the
// goaded-attack punish alone.
const VENGEFUL_GOADED_ATTACK_TRIGGER: &str =
    "Whenever a goaded creature attacks, it deals 1 damage to its controller.";
const LIFE_OF_THE_PARTY_ORACLE: &str = "First strike, trample, haste\nWhenever this creature attacks, it gets +X/+0 until end of turn, where X is the number of creatures you control.\nWhen this creature enters, if it's not a token, each opponent creates a token that's a copy of it. The tokens are goaded for the rest of the game. (They attack each combat if able and attack a player other than you if able.)";
const BOTHERSOME_QUASIT_ORACLE: &str = "Menace\nGoaded creatures your opponents control can't block.\nWhenever you cast a noncreature spell, goad target creature an opponent controls. (Until your next turn, that creature attacks each combat if able and attacks a player other than you if able.)";

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

#[test]
fn vengeful_ancestor_reads_real_registered_life_token_until_it_exits() {
    assert_goaded_attacks_trigger_parses();
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let ancestor = scenario
        .add_creature(P0, "Vengeful Ancestor", 3, 2)
        .from_oracle_text(VENGEFUL_GOADED_ATTACK_TRIGGER)
        .id();
    let ox = scenario.add_creature(P1, "Ornery Ox", 2, 2).id();
    let life = scenario
        .add_creature_to_hand(P0, "Life of the Party", 0, 1)
        .with_subtypes(vec!["Elemental"])
        .from_oracle_text_with_keywords(
            &["first strike", "trample", "haste"],
            LIFE_OF_THE_PARTY_ORACLE,
        )
        .id();
    let mut runner = scenario.build();
    runner.cast(life).resolve();
    assert!(runner.state().stack.is_empty());
    assert_eq!(runner.state().objects[&ancestor].controller, P0);
    let token = *runner
        .state()
        .battlefield
        .iter()
        .find(|id| {
            let obj = &runner.state().objects[id];
            obj.name == "Life of the Party" && obj.is_token && obj.controller == P1
        })
        .expect("the actual Life ETB must create a P1 token");
    assert!(runner
        .state()
        .transient_continuous_effects
        .iter()
        .any(|effect| {
            effect.controller == P0
                && effect.duration == Duration::Permanent
                && effect.affected == TargetFilter::SpecificObject { id: token }
                && effect
                    .modifications
                    .contains(&ContinuousModification::AddStaticMode {
                        mode: StaticMode::Goaded,
                    })
        }));
    let goaded = TargetFilter::Typed(TypedFilter::creature().properties(vec![FilterProp::Goaded]));
    assert!(matches_target_filter(
        runner.state(),
        token,
        &goaded,
        &FilterContext::neutral()
    ));

    let mut after_exit = GameRunner::from_state(runner.state().clone());
    engine::game::zones::move_to_zone(
        after_exit.state_mut(),
        token,
        Zone::Graveyard,
        &mut Vec::new(),
    );
    assert!(!after_exit.state().battlefield.contains(&token));
    assert!(!matches_target_filter(
        after_exit.state(),
        ox,
        &goaded,
        &FilterContext::neutral()
    ));

    hand_turn_to(&mut runner, P1);
    let before = life_of(runner.state(), P1);
    runner
        .declare_attackers(&[(token, AttackTarget::Player(P0))])
        .expect("the registered token attacks its only available opponent");
    runner.advance_until_stack_empty();
    assert_eq!(life_of(runner.state(), P1), before - 1);

    hand_turn_to(&mut after_exit, P1);
    let before = life_of(after_exit.state(), P1);
    after_exit
        .declare_attackers(&[(ox, AttackTarget::Player(P0))])
        .expect("the ungoaded sibling can still attack with Ancestor present");
    after_exit.advance_until_stack_empty();
    assert_eq!(after_exit.state().objects[&ancestor].controller, P0);
    assert_eq!(life_of(after_exit.state(), P1), before);
}

#[test]
fn bothersome_quasit_prevents_registered_life_token_from_blocking() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let attacker = scenario.add_creature(P0, "Bear", 2, 2).id();
    let quasit = scenario
        .add_creature(P0, "Bothersome Quasit", 3, 2)
        .from_oracle_text_with_keywords(&["menace"], BOTHERSOME_QUASIT_ORACLE)
        .id();
    let ordinary_blocker = scenario.add_creature(P1, "Ornery Ox", 2, 2).id();
    let life = scenario
        .add_creature_to_hand(P0, "Life of the Party", 0, 1)
        .with_subtypes(vec!["Elemental"])
        .from_oracle_text_with_keywords(
            &["first strike", "trample", "haste"],
            LIFE_OF_THE_PARTY_ORACLE,
        )
        .id();
    let mut runner = scenario.build();
    runner.cast(life).resolve();
    assert!(runner.state().stack.is_empty());
    let token = *runner
        .state()
        .battlefield
        .iter()
        .find(|id| {
            let object = &runner.state().objects[id];
            object.name == "Life of the Party" && object.is_token && object.controller == P1
        })
        .expect("the real Life trigger creates the P1 blocking candidate");
    assert_eq!(runner.state().objects[&quasit].controller, P0);
    assert!(runner
        .state()
        .transient_continuous_effects
        .iter()
        .any(|effect| {
            effect.controller == P0
                && effect.duration == Duration::Permanent
                && effect.affected == TargetFilter::SpecificObject { id: token }
                && effect
                    .modifications
                    .contains(&ContinuousModification::AddStaticMode {
                        mode: StaticMode::Goaded,
                    })
        }));
    let goaded = TargetFilter::Typed(TypedFilter::creature().properties(vec![FilterProp::Goaded]));
    assert!(matches_target_filter(
        runner.state(),
        token,
        &goaded,
        &FilterContext::neutral()
    ));
    assert!(!matches_target_filter(
        runner.state(),
        ordinary_blocker,
        &goaded,
        &FilterContext::neutral()
    ));

    runner.advance_to_combat();
    assert_eq!(runner.waiting_for_kind(), "DeclareAttackers");
    runner
        .declare_attackers(&[(attacker, AttackTarget::Player(P1))])
        .expect("the P0 Bear can attack P1");
    assert!(
        validate_blockers_for_player(runner.state(), P1, &[(ordinary_blocker, attacker)]).is_ok(),
        "the ungoaded P1 creature can block the same attacker"
    );
    assert!(
        validate_blockers_for_player(runner.state(), P1, &[(token, attacker)]).is_err(),
        "the registered P1 token cannot block while P0's Quasit functions"
    );
}
