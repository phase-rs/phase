//! Valakut Fireboar (ZEN) — runtime proof that the self-attack trigger switches
//! the creature's power and toughness via the "switch its power and toughness"
//! pronoun surface form.
//!
//! Oracle: "Whenever this creature attacks, switch its power and toughness until
//! end of turn."
//!
//! The effect parser recognized "switch the power and toughness of <target>" and
//! "switch <target>'s power and toughness", but not the bare pronoun "switch its
//! power and toughness": `parse_target` does not treat a bare "its" as a target,
//! so both existing branches missed it and the trigger effect fell to
//! `Effect::Unimplemented` — the swap never fired. The fix adds a pronoun branch
//! that resolves "its" through the shared `resolve_it_pronoun` anaphor. Here the
//! trigger subject is this creature, so "its" resolves to `SelfRef` and the
//! source's own P/T is switched (CR 613.4d).
//!
//! Discriminating observable: Valakut Fireboar is printed 5/2. After it is
//! declared as an attacker and the trigger resolves, its effective P/T must be
//! 2/5. Reverting the parser branch drops the effect (no swap), so the P/T stays
//! 5/2 and the final assertion flips to red.

use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameRunner, GameScenario};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

const P0: PlayerId = PlayerId(0);
const P1: PlayerId = PlayerId(1);

const VALAKUT_FIREBOAR_ORACLE: &str =
    "Whenever this creature attacks, switch its power and toughness until end of turn.";

/// Effective (post-layer) power/toughness of an object.
fn power_toughness(runner: &GameRunner, id: ObjectId) -> (i32, i32) {
    let obj = runner
        .state()
        .objects
        .get(&id)
        .expect("object still present");
    (obj.power.unwrap_or(0), obj.toughness.unwrap_or(0))
}

#[test]
fn valakut_fireboar_switches_power_and_toughness_when_it_attacks() {
    let mut scenario = GameScenario::new_n_player(2, 7);
    scenario.at_phase(Phase::PreCombatMain);

    // P0 controls Valakut Fireboar, printed 5/2 (ZEN).
    let boar = scenario
        .add_creature_from_oracle(P0, "Valakut Fireboar", 5, 2, VALAKUT_FIREBOAR_ORACLE)
        .id();
    let mut runner = scenario.build();

    // Sanity: printed 5/2 before combat.
    assert_eq!(power_toughness(&runner, boar), (5, 2), "printed P/T");

    // Advance to declare-attackers and swing at P1 — this fires the
    // "Whenever this creature attacks" trigger (CR 508.2).
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(boar, AttackTarget::Player(P1))])
        .expect("declare Valakut Fireboar as attacker");

    // Drive the trigger through the real trigger -> stack -> resolution pipeline
    // until the swap lands (or the loop exhausts).
    for _ in 0..300 {
        if power_toughness(&runner, boar) == (2, 5) {
            break;
        }
        match runner.state().waiting_for.clone() {
            WaitingFor::OrderTriggers { .. } => {
                runner
                    .act(GameAction::OrderTriggers { order: vec![0] })
                    .or_else(|_| runner.act(GameAction::OrderTriggers { order: vec![] }))
                    .expect("order the single attack trigger");
            }
            WaitingFor::Priority { .. } => {
                if runner.act(GameAction::PassPriority).is_err() {
                    break;
                }
            }
            WaitingFor::DeclareBlockers { .. } => {
                runner
                    .act(GameAction::DeclareBlockers {
                        assignments: vec![],
                    })
                    .expect("no blocks");
            }
            _ => break,
        }
    }

    // CR 613.4d: the attack trigger switched power and toughness for the turn.
    assert_eq!(
        power_toughness(&runner, boar),
        (2, 5),
        "Valakut Fireboar's P/T must swap 5/2 -> 2/5 after it attacks"
    );
}
