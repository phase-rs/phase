//! Runtime regression for Another Round — "Exile any number of creatures you
//! control, then return them to the battlefield under their owner's control.
//! Then repeat this process X more times." (CR 608.2c).
//!
//! Another Round is `{X}{X}{W}`, so "X more times" is the mana-{X}. The parser
//! stamps the ROOT ability with `repeat_for = Offset { Ref(Variable X), +1 }` —
//! the TOTAL run count ("once + X more"). The ungated whole-chain driver
//! (`repeated_full_chain`, effects/mod.rs) then loops the full exile→return
//! process X+1 times.
//!
//! The exile "any number of creatures you control" parses to a variable-count
//! `multi_target` (min 0 / max unbounded), so the chosen creatures are the
//! ability's targets (as they would be after the cast-time
//! `MultiTargetSelection`). Each loop iteration re-runs the whole chain against
//! those targets: exile them, then return them (CR 400.7 — each returns as a new
//! object, so they are summoning-sick afterward).
//!
//! Discriminating assertions:
//! - With M creatures and chosen X = N, the exile→return process runs exactly
//!   N+1 times → exactly `M * (N+1)` battlefield→exile transitions. Reverting the
//!   recognizer's `Offset(+1)` → `Offset(+0)` drops it to `M * N`; removing the
//!   "<q> more times" arm drops `repeat_for` to `None` and the chain runs once
//!   (`M` transitions).
//! - With chosen X = 0, the process runs exactly once (`M` transitions).
//!   Reverting to `Offset(+0)` makes X=0 resolve to 0 iterations — the process
//!   never runs (0 transitions).
//! - CR 400.7: the returned creatures are new objects, hence summoning-sick.

use engine::game::ability_utils::build_resolved_from_def_with_targets;
use engine::game::effects::resolve_ability_chain;
use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::game::zones::create_object;
use engine::parser::parse_oracle_text;
use engine::types::ability::{QuantityExpr, QuantityRef, TargetRef};
use engine::types::events::GameEvent;
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const ANOTHER_ROUND_ORACLE: &str = "Exile any number of creatures you control, then return them to the battlefield under their owner's control. Then repeat this process X more times.";

fn expected_repeat_for() -> QuantityExpr {
    QuantityExpr::Offset {
        inner: Box::new(QuantityExpr::Ref {
            qty: QuantityRef::Variable {
                name: "X".to_string(),
            },
        }),
        offset: 1,
    }
}

/// Parse Another Round's spell ability. Asserts the whole-process count landed on
/// the ROOT ability's `repeat_for` as X+1 and that the directive was consumed
/// (no leaked `Unimplemented{name:"repeat"}` sub-ability).
fn another_round_def() -> engine::types::ability::AbilityDefinition {
    let parsed = parse_oracle_text(
        ANOTHER_ROUND_ORACLE,
        "Another Round",
        &[],
        &["Sorcery".to_string()],
        &[],
    );
    let def = parsed
        .abilities
        .first()
        .expect("Another Round parses to a spell ability")
        .clone();
    assert_eq!(
        def.repeat_for,
        Some(expected_repeat_for()),
        "root repeat_for must be Offset(Variable X, +1); got {:?}",
        def.repeat_for
    );
    def
}

/// Add the Another Round spell object to the stack as the ability source.
fn add_source(runner: &mut GameRunner) -> ObjectId {
    let card_id = CardId(runner.state().next_object_id);
    create_object(
        runner.state_mut(),
        card_id,
        P0,
        "Another Round".to_string(),
        Zone::Stack,
    )
}

/// Count battlefield→exile transitions in the emitted event stream — one per
/// (creature, exile→return cycle) pair. Dividing by the creature count yields the
/// number of process runs.
fn exile_transitions(events: &[GameEvent]) -> usize {
    events
        .iter()
        .filter(|e| matches!(e, GameEvent::ZoneChanged { to, .. } if *to == Zone::Exile))
        .count()
}

/// Resolve Another Round with `chosen_x` and `num_creatures` P0 creatures on the
/// battlefield, with the creatures pre-selected as the exile targets (the state
/// the cast-time `MultiTargetSelection` produces). Returns
/// `(exile_transitions, initial_ids, final_state)`.
fn run_another_round(
    chosen_x: u32,
    num_creatures: usize,
) -> (usize, Vec<ObjectId>, engine::types::game_state::GameState) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let creatures: Vec<ObjectId> = (0..num_creatures)
        .map(|i| scenario.add_vanilla(P0, 2 + i as i32, 2))
        .collect();

    let mut runner = scenario.build();
    let source = add_source(&mut runner);

    let def = another_round_def();
    let targets: Vec<TargetRef> = creatures.iter().copied().map(TargetRef::Object).collect();
    let mut ability = build_resolved_from_def_with_targets(&def, source, P0, targets);
    ability.chosen_x = Some(chosen_x);

    let mut events = Vec::new();
    resolve_ability_chain(runner.state_mut(), &ability, &mut events, 0).unwrap();

    let transitions = exile_transitions(&events);
    (transitions, creatures, runner.state().clone())
}

/// P0's battlefield creatures as `(id, summoning_sick)` pairs.
fn p0_battlefield_creatures(state: &engine::types::game_state::GameState) -> Vec<(ObjectId, bool)> {
    state
        .objects
        .values()
        .filter(|o| {
            o.zone == Zone::Battlefield
                && o.controller == P0
                && o.card_types
                    .core_types
                    .contains(&engine::types::card_type::CoreType::Creature)
        })
        .map(|o| (o.id, o.summoning_sick))
        .collect()
}

#[test]
fn another_round_runs_process_x_plus_one_times() {
    // chosen X = 2, three creatures → 2 + 1 = 3 exile→return cycles, i.e. exactly
    // 3 * 3 = 9 battlefield→exile transitions. Reverting Offset(+1) → Offset(+0)
    // yields 3 * 2 = 6; removing the "<q> more times" arm yields a single run
    // (3 transitions). Either revert flips this assertion.
    let (transitions, initial_ids, state) = run_another_round(2, 3);
    assert_eq!(
        transitions, 9,
        "X=2, 3 creatures must run the process X+1 = 3 times (3 exiles per run)"
    );

    let final_creatures = p0_battlefield_creatures(&state);
    assert_eq!(
        final_creatures.len(),
        3,
        "all three creatures return to the battlefield"
    );
    // CR 400.7: each returned creature is a new object, so it re-entered this turn
    // and is summoning-sick — proving the exile→return actually cycled (a no-op
    // repeat would leave the pre-existing, non-sick creatures untouched).
    for (id, summoning_sick) in &final_creatures {
        assert!(
            initial_ids.contains(id),
            "same stable object id survives the blink"
        );
        assert!(
            *summoning_sick,
            "creature {id:?} must be summoning-sick after re-entering via blink"
        );
    }
}

#[test]
fn another_round_x_zero_runs_process_exactly_once() {
    // chosen X = 0 → 0 + 1 = exactly one run → 2 exile transitions (2 creatures).
    // Reverting Offset(+1) → Offset(+0) makes X=0 resolve to 0 iterations, so the
    // process never runs (0 transitions) — this assertion flips from 2 to 0.
    let (transitions, _initial_ids, state) = run_another_round(0, 2);
    assert_eq!(
        transitions, 2,
        "X=0 must run the process exactly once (2 creatures → 2 exile transitions)"
    );
    let final_creatures = p0_battlefield_creatures(&state);
    assert_eq!(final_creatures.len(), 2, "both creatures return once");
    for (id, summoning_sick) in &final_creatures {
        assert!(
            *summoning_sick,
            "creature {id:?} must be summoning-sick after the single blink"
        );
    }
}

#[test]
fn another_round_parses_to_root_repeat_for_offset_no_unimplemented() {
    let def = another_round_def();
    // The consumed directive must NOT leak an Unimplemented{name:"repeat"} sub.
    fn has_repeat_unimplemented(def: &engine::types::ability::AbilityDefinition) -> bool {
        let here = matches!(
            &*def.effect,
            engine::types::ability::Effect::Unimplemented { name, .. } if name == "repeat"
        );
        here || def
            .sub_ability
            .as_deref()
            .is_some_and(has_repeat_unimplemented)
    }
    assert!(
        !has_repeat_unimplemented(&def),
        "the 'repeat this process X more times' directive must be consumed, not left as Unimplemented"
    );
}
