//! CR 603.2: a "you attack" trigger head triggers only when the declaration
//! matches its whole trigger event; the head's tail is never dropped.
//!
//! Sol, Advocate Eternal (#11044): "Teamwork — Whenever you attack or block
//! with both Sol, Advocate Eternal and its partner, support 4 and investigate
//! four times." The parser used to read the attack leg as a bare "whenever you
//! attack", so Sol attacking alone supported 4. "Its partner" needs Legendary
//! partner (CR 702.124), which the engine does not model, so both legs fail
//! closed as `Unknown` and never fire.
//!
//! Ainok Strike Leader: "Whenever you attack with this creature and/or your
//! commander, ..." used to fire on any attack. It now fires only when Ainok or
//! the commander is declared as an attacker.

use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::parser::oracle::parse_oracle_text;
use engine::types::counter::CounterType;
use engine::types::game_state::WaitingFor;
use engine::types::phase::Phase;
use engine::types::triggers::TriggerMode;

const SOL: &str = "Legendary partner (You can have two commanders if this is one of them. The other one is promoted to legendary.)\nFlying, vigilance\nTeamwork — Whenever you attack or block with both Sol, Advocate Eternal and its partner, support 4 and investigate four times.";
const AINOK_STRIKE_LEADER: &str = "Whenever you attack with this creature and/or your commander, for each opponent, create a 1/1 red Goblin creature token that's tapped and attacking that player.\nSacrifice this creature: Creature tokens you control gain indestructible until end of turn.";

/// CR 603.2 + CR 508.1m: Ainok's trigger is on the stack after the declaration
/// only when Ainok itself attacks (no commander on this board).
#[test]
fn ainok_triggers_only_when_it_attacks() {
    for ainok_attacks in [false, true] {
        let mut s = GameScenario::new();
        s.at_phase(Phase::PreCombatMain);
        let ainok = s
            .add_creature_from_oracle(P0, "Ainok Strike Leader", 2, 2, AINOK_STRIKE_LEADER)
            .id();
        let bears = s.add_creature(P0, "Grizzly Bears", 2, 2).id();
        let mut r = s.build();
        r.advance_to_combat();
        let attacker = if ainok_attacks { ainok } else { bears };
        r.declare_attackers(&[(attacker, AttackTarget::Player(P1))])
            .expect("declare one attacker");
        assert_eq!(
            r.stack_names(),
            if ainok_attacks {
                vec!["Ainok Strike Leader".to_string()]
            } else {
                vec![]
            },
            "ainok_attacks={ainok_attacks}"
        );
    }
}

/// Reach guard: both Teamwork legs parse, and both fail closed.
#[test]
fn sol_teamwork_legs_fail_closed() {
    let parsed = parse_oracle_text(
        SOL,
        "Sol, Advocate Eternal",
        &[],
        &["Creature".to_string()],
        &[],
    );
    let modes: Vec<_> = parsed.triggers.iter().map(|t| &t.mode).collect();
    assert_eq!(modes.len(), 2, "attack leg and block leg: {modes:?}");
    assert!(
        modes.iter().all(|m| matches!(m, TriggerMode::Unknown(_))),
        "both Teamwork legs fail closed: {modes:?}"
    );
}

/// CR 603.2: Sol attacking alone (Grizzly Bears beside it, no partner) does not
/// trigger Teamwork: no support counters, nothing on the stack.
#[test]
fn sol_attacking_alone_does_not_trigger_teamwork() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let sol = s
        .add_creature_from_oracle(P0, "Sol, Advocate Eternal", 4, 4, SOL)
        .id();
    let bears = s.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let mut r = s.build();
    assert_eq!(
        r.state().objects[&sol].trigger_definitions.len(),
        2,
        "reach guard: Sol carries both Teamwork legs"
    );
    r.advance_to_combat();
    assert!(
        matches!(r.state().waiting_for, WaitingFor::DeclareAttackers { .. }),
        "reached declare attackers, got {}",
        r.waiting_for_kind()
    );
    r.declare_attackers(&[(sol, AttackTarget::Player(P1))])
        .expect("Sol attacks alone");

    assert!(
        r.state().stack.is_empty(),
        "Teamwork must not trigger: {:?}",
        r.stack_names()
    );
    assert!(
        matches!(r.state().waiting_for, WaitingFor::Priority { .. }),
        "no trigger prompt after declaration, got {}",
        r.waiting_for_kind()
    );
    assert_eq!(
        r.state().objects[&bears]
            .counters
            .get(&CounterType::Plus1Plus1)
            .copied()
            .unwrap_or(0),
        0,
        "no support counters"
    );
}
