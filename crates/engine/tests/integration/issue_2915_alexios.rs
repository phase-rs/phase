//! Regression for GitHub issue #2915 — Alexios, Deimos of Kosmos must scope
//! "can't attack its owner" to the owner player (not blanket CantAttack) and
//! upkeep GiveControl must honor `ScopedPlayer`.
//!
//! Follow-up (post-#2915): the upkeep trigger's middle clause — "that player
//! gains control of ~, **untaps it**, and puts a +1/+1 counter on it" — must
//! actually untap Alexios when control moves to the new player. The "untaps it"
//! anaphor previously parsed to `ParentTarget`, which resolves against the
//! parent `GiveControl { target: SelfRef }`'s empty target list, so the untap
//! silently no-op'd. The bug was latent until #2915 made control genuinely
//! move: before that, Alexios reverted to its owner every upkeep and was
//! untapped by that owner's normal untap step (CR 502), masking the dead
//! trigger-untap. CR 608.2k: the bare object pronoun "it" binds to the named
//! source (`SelfRef`), matching the sibling counter clause.

use engine::game::scenario::{GameRunner, GameScenario};
use engine::parser::oracle_static::parse_static_line_multi;
use engine::types::ability::TargetFilter;
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::WaitingFor;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::statics::StaticMode;
use engine::types::triggers::AttackTargetFilter;

const P0: PlayerId = PlayerId(0);
const P1: PlayerId = PlayerId(1);

const ALEXIOS_ATTACK_LINE: &str =
    "This creature attacks each combat if able, can't be sacrificed, and can't attack its owner.";

/// Alexios's full Oracle text (the upkeep trigger is what drives this test).
const ALEXIOS_ORACLE: &str = "Trample\n\
     Alexios attacks each combat if able, can't be sacrificed, and can't attack its owner.\n\
     At the beginning of each player's upkeep, that player gains control of Alexios, untaps it, \
     and puts a +1/+1 counter on it. It gains haste until end of turn.";

#[test]
fn alexios_cant_attack_owner_is_scoped_not_blanket() {
    let defs = parse_static_line_multi(ALEXIOS_ATTACK_LINE);
    assert_eq!(defs.len(), 3);
    assert_eq!(defs[0].mode, StaticMode::MustAttack);
    assert_eq!(
        defs[1].mode,
        StaticMode::Other("CantBeSacrificed".to_string())
    );
    assert_eq!(defs[2].mode, StaticMode::CantAttack);
    assert_eq!(defs[2].attack_defended, Some(AttackTargetFilter::Owner));
    assert!(defs
        .iter()
        .all(|def| def.affected == Some(TargetFilter::SelfRef)));
}

/// Drive the engine forward, passing priority and declaring no
/// attackers/blockers, until the turn rolls from P0's pre-combat main into P1's
/// upkeep and the (non-targeted) Alexios trigger resolves — i.e. control flips
/// to P1 — or a step budget is exhausted.
fn advance_until_alexios_controlled_by_p1(
    runner: &mut GameRunner,
    alexios: engine::types::identifiers::ObjectId,
) {
    for _ in 0..240 {
        if runner.state().objects[&alexios].controller == P1 {
            return;
        }
        match &runner.state().waiting_for {
            WaitingFor::Priority { .. } => {
                if runner.act(GameAction::PassPriority).is_err() {
                    return;
                }
            }
            WaitingFor::DeclareAttackers { .. } => {
                if runner
                    .act(GameAction::DeclareAttackers {
                        attacks: vec![],
                        bands: vec![],
                    })
                    .is_err()
                {
                    return;
                }
            }
            WaitingFor::DeclareBlockers { .. } => {
                if runner
                    .act(GameAction::DeclareBlockers {
                        assignments: vec![],
                    })
                    .is_err()
                {
                    return;
                }
            }
            _ => return,
        }
    }
}

/// CR 608.2k + CR 701.26b (issue #2915 follow-up): on P1's upkeep the trigger
/// gives P1 control of Alexios, untaps it, and adds a +1/+1 counter. A
/// discriminating runtime test: Alexios enters tapped under P0; pre-fix the
/// "untaps it" clause resolves to nothing and Alexios stays tapped after moving
/// to P1; post-fix it is untapped.
#[test]
fn alexios_upkeep_trigger_untaps_when_control_moves() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);

    // Libraries so draw steps never deck anyone out before the assertion.
    for &pid in &[P0, P1] {
        scenario.with_library_top(pid, &["Lib A", "Lib B", "Lib C", "Lib D"]);
    }

    let alexios = scenario
        .add_creature_from_oracle(P0, "Alexios, Deimos of Kosmos", 4, 4, ALEXIOS_ORACLE)
        .id();

    let mut runner = scenario.build();
    // Alexios enters tapped (e.g. it attacked on P0's previous turn). Because it
    // changes control on P1's upkeep, P1's own untap step (CR 502) does NOT
    // untap it — only the trigger's "untaps it" clause can.
    runner.state_mut().objects.get_mut(&alexios).unwrap().tapped = true;

    advance_until_alexios_controlled_by_p1(&mut runner, alexios);

    let obj = &runner.state().objects[&alexios];
    assert_eq!(
        obj.controller, P1,
        "trigger must move control of Alexios to the upkeep player (P1)"
    );
    assert!(
        !obj.tapped,
        "the 'untaps it' clause must untap Alexios when control moves to P1"
    );
    assert_eq!(
        obj.counters
            .get(&CounterType::Plus1Plus1)
            .copied()
            .unwrap_or(0),
        1,
        "the trigger must put one +1/+1 counter on Alexios"
    );
}
