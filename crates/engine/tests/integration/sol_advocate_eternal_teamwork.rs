//! Sol, Advocate Eternal (#11044): "Teamwork — Whenever you attack or block
//! with both Sol, Advocate Eternal and its partner, support 4 and investigate
//! four times."
//!
//! CR 603.2: the ability triggers only when the event matches the whole
//! trigger event. The parser used to read the attack leg as a bare "whenever
//! you attack", so Sol attacking alone supported 4. "Its partner" needs Legendary
//! partner (CR 702.124), which the engine does not model, so both legs fail
//! closed as `Unknown` and never fire.

use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::parser::oracle::parse_oracle_text;
use engine::types::counter::CounterType;
use engine::types::game_state::WaitingFor;
use engine::types::phase::Phase;
use engine::types::triggers::TriggerMode;

const SOL: &str = "Legendary partner (You can have two commanders if this is one of them. The other one is promoted to legendary.)\nFlying, vigilance\nTeamwork — Whenever you attack or block with both Sol, Advocate Eternal and its partner, support 4 and investigate four times.";

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
