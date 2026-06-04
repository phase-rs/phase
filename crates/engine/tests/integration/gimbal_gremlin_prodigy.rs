//! Gimbal, Gremlin Prodigy — end-step trigger that creates a 0/0 Gremlin with
//! X +1/+1 counters, where X is "the number of differently named artifact
//! tokens you control".
//!
//! Bug (pre-fix): the quantity phrase fell through to the
//! `oracle_quantity::parse_quantity_ref_with_context` fallback, which calls
//! `parse_type_phrase_with_ctx` and silently discards the unconsumed
//! remainder. "differently named" was unrecognized, so the result was
//! `QuantityRef::ObjectCount { filter: <empty Typed> }` — a filter that
//! matches every battlefield permanent. With even a handful of permanents in
//! play, the new Gremlin would enter with absurdly oversized stats (the
//! original report: a 24/24 with zero artifact tokens around).
//!
//! Fix: `oracle_nom::quantity::parse_distinct_named_objects` recognizes the
//! "differently named <type-phrase>" template and emits
//! `QuantityRef::ObjectCountDistinct { filter, qualities: [Name] }`. The
//! runtime resolver in `game::quantity` already deduplicates objects by name
//! (CR 201.2 + CR 603.4), so the spawned Gremlin enters with one counter per
//! distinctly-named artifact token its controller controls.
//!
//! CR references (verified against docs/MagicCompRules.txt):
//!   - CR 122.6a: "If an object enters the battlefield with counters on it ...
//!     the object's controller puts those counters on it."
//!   - CR 201.2: a name is an object characteristic.
//!   - CR 513.1a: "At the beginning of [your] end step" triggers fire at the
//!     start of the end step.
//!   - CR 603.4: triggered abilities check their condition on resolution.

use super::rules::{GameRunner, GameScenario, Phase, WaitingFor, P0};
use engine::game::game_object::GameObject;
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::identifiers::ObjectId;

/// Gimbal's printed Oracle text — byte-identical to `client/public/card-data.json`.
const GIMBAL: &str = "Artifact creatures you control have trample.\nAt the \
beginning of your end step, create a 0/0 red Gremlin artifact creature \
token. Put X +1/+1 counters on it, where X is the number of differently \
named artifact tokens you control.";

/// Add an artifact-creature token under P0's control with the given name.
///
/// `add_creature` puts the object on the battlefield; `as_artifact` adds the
/// Artifact core type. `is_token` is then flipped on the live object after
/// `build`, mirroring the pattern used by `ashaya_nontoken_lands` and
/// `wedding_ring_etb_token_copy`.
fn place_artifact_token(scenario: &mut GameScenario, name: &str) -> ObjectId {
    scenario.add_creature(P0, name, 1, 1).as_artifact().id()
}

/// Flip every supplied object's `is_token` flag to `true` after the runner is
/// built. Required because the scenario builder has no token toggle.
fn mark_as_tokens(runner: &mut GameRunner, ids: &[ObjectId]) {
    for &id in ids {
        runner
            .state_mut()
            .objects
            .get_mut(&id)
            .expect("token object present after build")
            .is_token = true;
    }
}

/// Drive the engine until P0's end step trigger has resolved. Returns when
/// priority is in `Phase::End` with the stack empty (i.e., Gimbal's
/// no-target trigger has fully resolved and the new Gremlin is on the
/// battlefield).
///
/// Passes priority on every Priority window, and submits empty
/// attackers/blockers on the combat windows P0 must walk through to reach
/// the end step (CR 508.1 / CR 509.1) — the test setup never attacks.
fn advance_until_end_step_trigger_resolved(runner: &mut GameRunner) {
    for _ in 0..200 {
        let in_end_step = runner.state().phase == Phase::End;
        let stack_empty = runner.state().stack.is_empty();
        if in_end_step && stack_empty {
            // The trigger has been put on the stack and resolved; we are
            // now at P0's first priority window in End with nothing pending.
            if matches!(runner.state().waiting_for, WaitingFor::Priority { .. }) {
                return;
            }
        }
        match &runner.state().waiting_for {
            WaitingFor::Priority { .. } => {
                if runner.act(GameAction::PassPriority).is_err() {
                    return;
                }
            }
            WaitingFor::DeclareAttackers { .. } => {
                runner
                    .act(GameAction::DeclareAttackers {
                        attacks: vec![],
                        bands: vec![],
                    })
                    .expect("declaring no attackers must succeed");
            }
            WaitingFor::DeclareBlockers { .. } => {
                runner
                    .act(GameAction::DeclareBlockers {
                        assignments: vec![],
                    })
                    .expect("declaring no blockers must succeed");
            }
            other => panic!(
                "unexpected waiting state advancing to end step: {other:?} \
                 (phase={:?}, stack_len={})",
                runner.state().phase,
                runner.state().stack.len()
            ),
        }
    }
    panic!(
        "engine did not resolve Gimbal's end-step trigger within 200 steps \
         (phase={:?}, stack_len={})",
        runner.state().phase,
        runner.state().stack.len()
    );
}

/// Locate the Gremlin token spawned by Gimbal's trigger. Identifies it by:
///   1. `is_token == true` (CR 111.1) AND
///   2. its name is exactly "Gremlin" (the token name produced by the
///      `Effect::Token { name: "Gremlin", .. }` resolver).
///
/// Asserts exactly one matching token exists.
fn find_spawned_gremlin(runner: &GameRunner) -> &GameObject {
    let candidates: Vec<_> = runner
        .state()
        .objects
        .values()
        .filter(|obj| {
            obj.is_token
                && obj.zone == engine::types::zones::Zone::Battlefield
                && obj.name == "Gremlin"
        })
        .collect();
    assert_eq!(
        candidates.len(),
        1,
        "expected exactly one Gremlin token spawned by Gimbal, found {}",
        candidates.len()
    );
    candidates[0]
}

/// Count `+1/+1` counters on an object.
fn p1p1_counters(obj: &GameObject) -> u32 {
    obj.counters
        .get(&CounterType::Plus1Plus1)
        .copied()
        .unwrap_or(0)
}

/// Core regression: three distinct-named artifact tokens (Treasure, Food,
/// Clue) plus a duplicate Treasure → distinct-name count = 3. The spawned
/// Gremlin must enter with exactly 3 +1/+1 counters, not the pre-fix count
/// (which was the total permanent count regardless of artifact-token
/// identity).
#[test]
fn gimbal_spawns_gremlin_with_distinct_named_artifact_token_count() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    scenario.add_creature_from_oracle(P0, "Gimbal, Gremlin Prodigy", 4, 4, GIMBAL);
    let token_ids = vec![
        place_artifact_token(&mut scenario, "Treasure"),
        place_artifact_token(&mut scenario, "Food"),
        place_artifact_token(&mut scenario, "Clue"),
        // Duplicate of the first Treasure — same name, must NOT contribute a
        // second tally under CR 201.2 distinct-name deduplication.
        place_artifact_token(&mut scenario, "Treasure"),
    ];

    let mut runner = scenario.build();
    mark_as_tokens(&mut runner, &token_ids);

    advance_until_end_step_trigger_resolved(&mut runner);

    let gremlin = find_spawned_gremlin(&runner);
    assert_eq!(
        p1p1_counters(gremlin),
        3,
        "Gimbal's Gremlin must enter with exactly 3 +1/+1 counters — one per \
         differently named artifact token P0 controls (Treasure, Food, Clue; \
         the duplicate Treasure does not contribute again per CR 201.2)"
    );
}

/// Single artifact token → exactly 1 counter. Discriminates the fix from any
/// "always 1" fallback regression.
#[test]
fn gimbal_one_artifact_token_gives_one_counter() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    scenario.add_creature_from_oracle(P0, "Gimbal, Gremlin Prodigy", 4, 4, GIMBAL);
    let lone_token = place_artifact_token(&mut scenario, "Treasure");

    let mut runner = scenario.build();
    mark_as_tokens(&mut runner, &[lone_token]);

    advance_until_end_step_trigger_resolved(&mut runner);

    let gremlin = find_spawned_gremlin(&runner);
    assert_eq!(
        p1p1_counters(gremlin),
        1,
        "exactly one artifact token (Treasure) under P0's control must \
         produce exactly one +1/+1 counter on the spawned Gremlin"
    );
}

/// Nontoken artifacts must not contribute. P0 controls Gimbal plus an
/// Artifact creature that is *not* a token; the filter carries
/// `FilterProp::Token` so the nontoken artifact is excluded. The lone
/// artifact token contributes the only tally.
#[test]
fn gimbal_excludes_nontoken_artifacts_and_opponent_tokens() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    scenario.add_creature_from_oracle(P0, "Gimbal, Gremlin Prodigy", 4, 4, GIMBAL);
    // Nontoken artifact under P0's control — must be excluded by FilterProp::Token.
    scenario
        .add_creature(P0, "Permanent Artifact Creature", 2, 2)
        .as_artifact();
    // Opponent-controlled artifact token — must be excluded by ControllerRef::You.
    let opp_token = scenario
        .add_creature(engine::game::scenario::P1, "Opponent Treasure", 1, 1)
        .as_artifact()
        .id();
    let p0_token = place_artifact_token(&mut scenario, "Treasure");

    let mut runner = scenario.build();
    mark_as_tokens(&mut runner, &[p0_token, opp_token]);

    advance_until_end_step_trigger_resolved(&mut runner);

    let gremlin = find_spawned_gremlin(&runner);
    assert_eq!(
        p1p1_counters(gremlin),
        1,
        "only P0's artifact tokens count — the nontoken artifact creature \
         (FilterProp::Token) and the opponent-controlled token \
         (ControllerRef::You) are both excluded"
    );
}
