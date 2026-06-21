//! MSH Wave 1 — `TriggerMode::Connives` ("whenever [subject] connives").
//!
//! Building-block coverage for the connive-payoff class: Glorious Purpose, Iron
//! Monger (Sadistic Tycoon), and Ultron, Unlimited all carry a "whenever a
//! creature you control connives" / self-connive trigger. These cards are not in
//! the local test fixture (MSH is release-gated), so the tests drive the real
//! parser + trigger pipeline through representative Oracle text.
//!
//! CR 701.50b: a permanent "connives" after the connive process completes; the
//! `EffectResolved { kind: Connive }` event must carry the CONNIVER's id (CR
//! 701.50c LKI), not the causing source — otherwise "whenever a creature you
//! control connives" is evaluated against the wrong object.

use engine::game::ability_utils::build_resolved_from_def;
use engine::game::effects::resolve_ability_chain;
use engine::game::scenario::GameScenario;
use engine::game::triggers::process_triggers;
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{Effect, ResolvedAbility, TargetRef};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

const P0: PlayerId = PlayerId(0);
const P1: PlayerId = PlayerId(1);

/// Parse a connive activated ability ("{T}: Target creature you control
/// connives.") and return its definition, for re-targeting at an arbitrary
/// conniver from an arbitrary source.
fn connive_def() -> engine::types::ability::AbilityDefinition {
    let parsed = parse_oracle_text(
        "{T}: Target creature you control connives.",
        "Connive Source",
        &[],
        &["Artifact".to_string()],
        &[],
    );
    parsed
        .abilities
        .iter()
        .find(|a| matches!(a.effect.as_ref(), Effect::Connive { .. }))
        .expect("must parse a Connive activated ability")
        .clone()
}

/// Drive priority until the stack empties (resolving the connive payoff
/// trigger). Stops at the first non-priority wait so the turn does not advance.
fn drain_priority(runner: &mut engine::game::scenario::GameRunner) {
    let mut guard = 0;
    while !runner.state().stack.is_empty() {
        guard += 1;
        assert!(guard < 60, "stack did not drain");
        match &runner.state().waiting_for {
            WaitingFor::Priority { .. } => {
                if runner.act(GameAction::PassPriority).is_err() {
                    break;
                }
            }
            _ => break,
        }
    }
}

/// Glorious Purpose class + the CR 701.50c conviver-id fix (load-bearing).
///
/// A creature you control connives, caused by an EXTERNAL source whose own
/// characteristics do NOT satisfy the watcher's "a creature you control" filter
/// (here: an opponent's creature). The watcher must fire because the event
/// carries the CONNIVER's id, not the source's.
///
/// Revert-failing assertion: with the pre-fix `source_id: ability.source_id`,
/// the `EffectResolved` carries the opponent's source id, the watcher filter
/// "a creature you control" rejects it, and no life is gained.
#[test]
fn connive_watcher_fires_on_external_conniver_via_conniver_id() {
    let mut scenario = GameScenario::new_n_player(2, 7);
    scenario.at_phase(Phase::PreCombatMain);
    // One nonland card so the connive draw + auto-discard completes without a
    // discard prompt (empty hand → draw 1 → discard 1).
    scenario.with_library_top(P0, &["Lib A"]);

    // Watcher: "whenever a creature you control connives, you gain 2 life."
    scenario.add_creature_from_oracle(
        P0,
        "Glorious Watcher",
        2,
        2,
        "Whenever a creature you control connives, you gain 2 life.",
    );
    // The conniver — a plain creature you control.
    let conniver = scenario.add_creature(P0, "Conniver", 2, 2).id();
    // External source: an OPPONENT's creature (a creature, but not yours).
    let external_source = scenario.add_creature(P1, "Opponent Source", 1, 1).id();

    let mut runner = scenario.build();
    runner.state_mut().turn_number = 2;
    runner.state_mut().active_player = P0;
    runner.state_mut().priority_player = P0;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P0 };

    let life_before = runner.life(P0);

    // Resolve a connive on `conniver` whose ability source is the opponent's
    // creature and whose controller is P0 (so P0 draws/discards).
    let def = connive_def();
    let ability = ResolvedAbility {
        targets: vec![TargetRef::Object(conniver)],
        ..build_resolved_from_def(&def, external_source, P0)
    };
    let mut events = Vec::new();
    resolve_ability_chain(runner.state_mut(), &ability, &mut events, 0)
        .expect("connive must resolve");

    process_triggers(runner.state_mut(), &events);
    drain_priority(&mut runner);

    assert_eq!(
        runner.life(P0),
        life_before + 2,
        "watcher must fire on the CONNIVER (a creature you control), gaining 2 life — \
         pre-fix the event carried the opponent source id and no life was gained"
    );
}

/// Negative case: an OPPONENT's creature connives. "a creature you control" must
/// reject it — no life gained.
#[test]
fn connive_watcher_ignores_opponent_conniver() {
    let mut scenario = GameScenario::new_n_player(2, 9);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P1, &["Lib A"]);

    scenario.add_creature_from_oracle(
        P0,
        "Glorious Watcher",
        2,
        2,
        "Whenever a creature you control connives, you gain 2 life.",
    );
    // The conniver is the OPPONENT's creature.
    let opp_conniver = scenario.add_creature(P1, "Opp Conniver", 2, 2).id();

    let mut runner = scenario.build();
    runner.state_mut().turn_number = 2;
    runner.state_mut().active_player = P0;
    runner.state_mut().priority_player = P0;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P0 };

    let life_before = runner.life(P0);

    let def = connive_def();
    // Controller P1 (the opponent) drives the connive on their own creature.
    let ability = ResolvedAbility {
        targets: vec![TargetRef::Object(opp_conniver)],
        ..build_resolved_from_def(&def, opp_conniver, P1)
    };
    let mut events = Vec::new();
    resolve_ability_chain(runner.state_mut(), &ability, &mut events, 0)
        .expect("connive must resolve");

    process_triggers(runner.state_mut(), &events);
    drain_priority(&mut runner);

    assert_eq!(
        runner.life(P0),
        life_before,
        "an opponent's conniver must not trigger 'a creature you control connives'"
    );
}

/// Ultron, Unlimited class — self-connive trigger ("whenever ~ connives"). The
/// no-filter identity branch of `match_connives` (`*conniver_id == source_id`)
/// is exercised when the watcher and the conniver are the same permanent.
#[test]
fn connive_self_trigger_fires_for_the_conniving_source() {
    let mut scenario = GameScenario::new_n_player(2, 11);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Lib A"]);

    // Self-referential connive payoff: "whenever ~ connives, you gain 2 life."
    let ultron = scenario
        .add_creature_from_oracle(
            P0,
            "Ultron Self",
            3,
            3,
            "Whenever this creature connives, you gain 2 life.",
        )
        .id();

    let mut runner = scenario.build();
    runner.state_mut().turn_number = 2;
    runner.state_mut().active_player = P0;
    runner.state_mut().priority_player = P0;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P0 };

    let life_before = runner.life(P0);

    // Ultron connives itself (source == conniver).
    let def = connive_def();
    let ability = ResolvedAbility {
        targets: vec![TargetRef::Object(ultron)],
        ..build_resolved_from_def(&def, ultron, P0)
    };
    let mut events = Vec::new();
    resolve_ability_chain(runner.state_mut(), &ability, &mut events, 0)
        .expect("connive must resolve");

    process_triggers(runner.state_mut(), &events);
    drain_priority(&mut runner);

    assert_eq!(
        runner.life(P0),
        life_before + 2,
        "a permanent's self-connive must fire its own 'whenever ~ connives' trigger"
    );
}
