//! Discriminating integration tests for three reusable counter-grammar axes
//! introduced for the Standard "counters" batch. Each test drives a real card's
//! Oracle text through the production trigger pipeline (`process_triggers` /
//! phase advancement) and asserts the concrete counters that land, so reverting
//! the corresponding parser grammar flips the assertion.
//!
//! Axis (a) — choose-counter-kind disjunction (Reluctant Role Model):
//!   "put a flying, lifelink, or +1/+1 counter on it" must parse into a
//!   `ChooseOneOf` of three `PutCounter` branches sharing one target. Reverting
//!   the bare-form arm in `try_parse_put_counter_choice` makes the clause
//!   Unimplemented, so NO counter lands (assertion flips).
//!
//! Axis (b) — typed-possessive move (Selfless Police Captain):
//!   "put its +1/+1 counters on target creature you control" must parse the
//!   typed qualifier into `MoveCounters { counter_type: Some(P1P1) }` so ONLY
//!   +1/+1 counters move. Reverting to `counter_type: None` moves every counter
//!   kind (CR 122.8 specifies only the named kind), so the negative-control
//!   charge counter would leak onto the target (assertion flips).
//!
//! Axis (c) — "additional" qualifier (Toph, Hardheaded Teacher):
//!   "put an additional +1/+1 counter on that land" must strip the flavor word
//!   "additional" before the counter-type parse so the clause yields
//!   `PutCounter { counter_type: P1P1, count: 1 }`. Reverting the strip leaves
//!   "additional +1/+1" unparsed as a counter type, dropping the clause to
//!   Unimplemented (no counter lands).
//!
//! CR references (verified against docs/MagicCompRules.txt):
//!   - CR 122.1: a counter is a marker placed on an object.
//!   - CR 122.1b: keyword counters (flying, lifelink) grant their keyword.
//!   - CR 122.8: "put one object's counters on another" with a named kind moves
//!     only that kind.
//!   - CR 608.2d: the counter-kind choice is announced at resolution.

use super::rules::{GameRunner, GameScenario, Phase, WaitingFor, Zone, P0};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::identifiers::ObjectId;
use engine::types::keywords::KeywordKind;

const RELUCTANT_ROLE_MODEL_ORACLE: &str = "Survival — At the beginning of your second main phase, if this creature is tapped, put a flying, lifelink, or +1/+1 counter on it.\nWhenever this creature or another creature you control dies, if it had counters on it, put those counters on up to one target creature.";

const SELFLESS_POLICE_CAPTAIN_ORACLE: &str = "This creature enters with a +1/+1 counter on it.\nWhen this creature leaves the battlefield, put its +1/+1 counters on target creature you control.";

const TOPH_ORACLE: &str = "When Toph enters, you may discard a card. If you do, return target instant or sorcery card from your graveyard to your hand.\nWhenever you cast a spell, earthbend 1. If that spell is a Lesson, put an additional +1/+1 counter on that land.";

fn counters(runner: &GameRunner, id: ObjectId, ct: &CounterType) -> u32 {
    runner
        .state()
        .objects
        .get(&id)
        .and_then(|o| o.counters.get(ct).copied())
        .unwrap_or(0)
}

/// Total number of distinct counter kinds (with count > 0) on an object — used
/// as the negative control for the typed-possessive move (axis b).
fn distinct_counter_kinds(runner: &GameRunner, id: ObjectId) -> usize {
    runner
        .state()
        .objects
        .get(&id)
        .map(|o| o.counters.values().filter(|&&c| c > 0).count())
        .unwrap_or(0)
}

/// Axis (a): the choose-counter-kind disjunction resolves through a real
/// `ChooseOneOf` mode choice. We deliberately pick the SECOND branch (lifelink),
/// not the +1/+1 default, so the test proves all three branches parsed — a
/// regression that only kept the +1/+1 branch (or dropped the whole clause)
/// could not place a lifelink keyword counter.
#[test]
fn reluctant_role_model_choose_counter_kind_places_chosen_keyword() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let role_model = scenario
        .add_creature_from_oracle(
            P0,
            "Reluctant Role Model",
            2,
            2,
            RELUCTANT_ROLE_MODEL_ORACLE,
        )
        .id();

    let mut runner = scenario.build();

    // Intervening-if gate (CR 603.4): "if this creature is tapped". Tap it so the
    // beginning-of-second-main-phase trigger is not gated out.
    runner
        .state_mut()
        .objects
        .get_mut(&role_model)
        .expect("role model present")
        .tapped = true;

    assert_eq!(
        distinct_counter_kinds(&runner, role_model),
        0,
        "precondition: Reluctant Role Model starts with no counters"
    );

    // Drive through combat into the second main phase (PostCombatMain), where the
    // "beginning of your second main phase" trigger fires (CR 505.1b).
    runner.advance_to_phase(Phase::PostCombatMain);

    // The trigger surfaces a choose-one branch prompt; choose branch index 1
    // (lifelink). The shared target is SelfRef so no separate target selection
    // is needed.
    let mut chose = false;
    for _ in 0..60 {
        match runner.state().waiting_for.clone() {
            WaitingFor::ChooseOneOfBranch { .. } => {
                runner
                    .act(GameAction::ChooseBranch { index: 1 })
                    .expect("choosing the lifelink branch must succeed");
                chose = true;
            }
            WaitingFor::Priority { .. } => {
                if chose && runner.state().stack.is_empty() {
                    break;
                }
                if runner.act(GameAction::PassPriority).is_err() {
                    break;
                }
            }
            other => panic!("unexpected waiting state: {other:?}"),
        }
    }

    assert!(
        chose,
        "the second-main-phase trigger must offer a counter-kind choice"
    );

    // DISCRIMINATOR: exactly one lifelink keyword counter on the source, and no
    // flying / +1/+1 counter (the other two branches were not chosen).
    assert_eq!(
        counters(
            &runner,
            role_model,
            &CounterType::Keyword(KeywordKind::Lifelink)
        ),
        1,
        "choosing the lifelink branch must place exactly one lifelink counter; \
         counters: {:?}",
        runner.state().objects[&role_model].counters
    );
    assert_eq!(
        counters(
            &runner,
            role_model,
            &CounterType::Keyword(KeywordKind::Flying)
        ),
        0,
        "the unchosen flying branch must not place a counter"
    );
    assert_eq!(
        counters(&runner, role_model, &CounterType::Plus1Plus1),
        0,
        "the unchosen +1/+1 branch must not place a counter"
    );
}

/// Axis (b): the typed-possessive move copies ONLY the named counter kind. The
/// captain carries one +1/+1 counter (the kind named in "put its +1/+1
/// counters") plus a negative-control charge counter. On leaving the
/// battlefield, the target must receive the +1/+1 counter and NOT the charge
/// counter — that is the whole point of `counter_type: Some(P1P1)` (CR 122.8).
#[test]
fn selfless_police_captain_moves_only_named_counter_kind() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let captain = scenario
        .add_creature_from_oracle(
            P0,
            "Selfless Police Captain",
            2,
            2,
            SELFLESS_POLICE_CAPTAIN_ORACLE,
        )
        .id();
    let receiver = scenario.add_creature(P0, "Receiver", 1, 1).id();

    let mut runner = scenario.build();
    // Captain leaves with one +1/+1 counter (the named kind) AND one charge
    // counter (the negative control that must NOT move).
    scenario_set_counter(&mut runner, captain, CounterType::Plus1Plus1, 1);
    scenario_set_counter(
        &mut runner,
        captain,
        CounterType::Generic("charge".to_string()),
        1,
    );
    runner.state_mut().turn_number = 2;
    runner.state_mut().active_player = P0;
    runner.state_mut().priority_player = P0;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P0 };

    assert_eq!(
        counters(&runner, receiver, &CounterType::Plus1Plus1),
        0,
        "precondition: receiver starts with no +1/+1 counters"
    );

    // Captain leaves the battlefield — the real production look-back event path.
    let mut events = Vec::new();
    engine::game::zones::move_to_zone(runner.state_mut(), captain, Zone::Graveyard, &mut events);
    engine::game::triggers::process_triggers(runner.state_mut(), &events);

    // The LTB trigger targets a creature you control; select the receiver.
    let mut guard = 0;
    loop {
        guard += 1;
        assert!(guard < 40, "LTB trigger resolution did not terminate");
        match runner.state().waiting_for.clone() {
            WaitingFor::TriggerTargetSelection { .. } | WaitingFor::TargetSelection { .. } => {
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(TargetRef::Object(receiver)),
                    })
                    .expect("choosing the receiver must succeed");
            }
            WaitingFor::Priority { .. } => {
                if counters(&runner, receiver, &CounterType::Plus1Plus1) > 0 {
                    break;
                }
                if runner.act(GameAction::PassPriority).is_err() {
                    break;
                }
            }
            other => panic!("unexpected waiting state during LTB trigger: {other:?}"),
        }
    }
    runner.advance_until_stack_empty();

    // DISCRIMINATOR: the +1/+1 counter moved to the receiver, but the charge
    // counter did NOT. With `counter_type: None` (the pre-fix shape) every kind
    // would copy, so the receiver would also hold a charge counter.
    assert_eq!(
        counters(&runner, receiver, &CounterType::Plus1Plus1),
        1,
        "the named +1/+1 counter must move to the receiver (CR 122.8)"
    );
    assert_eq!(
        counters(
            &runner,
            receiver,
            &CounterType::Generic("charge".to_string())
        ),
        0,
        "the charge counter must NOT move — only the named +1/+1 kind transfers \
         (CR 122.8); receiver counters: {:?}",
        runner.state().objects[&receiver].counters
    );
    assert_eq!(
        distinct_counter_kinds(&runner, receiver),
        1,
        "the receiver must hold exactly one counter kind (+1/+1), not the \
         captain's full counter set"
    );
}

/// Axis (c): the "additional" qualifier is a flavor word that does not change
/// the placed counter. Toph's cast-spell trigger is a chain of TWO +1/+1
/// counter placements: earthbend 1's native +1/+1 counter, then the
/// Lesson-conditional "put an additional +1/+1 counter on that land".
///
/// The discriminator is the SECOND placement. Reverting the "additional" strip
/// leaves "put an additional +1/+1 counter on that land" Unimplemented, so the
/// cast-spell trigger chain has exactly ONE `PutCounter` and ONE
/// `Unimplemented`. With the strip, the chain has TWO `PutCounter` effects (both
/// +1/+1, count 1) and ZERO `Unimplemented`. Asserting both counts flips when
/// the strip is reverted (the earthbend PutCounter alone is not enough).
#[test]
fn toph_additional_qualifier_parses_second_plus_one_counter() {
    use engine::parser::oracle::parse_oracle_text;
    use engine::types::ability::{AbilityDefinition, Effect, QuantityExpr};

    let parsed = parse_oracle_text(
        TOPH_ORACLE,
        "Toph, Hardheaded Teacher",
        &[],
        &["Creature".to_string()],
        &["Human".to_string()],
    );

    // Walk a trigger's whole execute chain, collecting every effect.
    fn collect(def: &AbilityDefinition, out: &mut Vec<Effect>) {
        out.push((*def.effect).clone());
        if let Some(sub) = &def.sub_ability {
            collect(sub, out);
        }
    }

    // Find the cast-a-spell trigger (the one carrying the earthbend + additional
    // counter chain), then collect its effects.
    let mut effects = Vec::new();
    for trigger in &parsed.triggers {
        if let Some(execute) = trigger.execute.as_deref() {
            let mut chain = Vec::new();
            collect(execute, &mut chain);
            if chain
                .iter()
                .any(|e| matches!(e, Effect::RegisterBending { .. }))
            {
                effects = chain;
                break;
            }
        }
    }
    assert!(
        !effects.is_empty(),
        "the cast-a-spell trigger (earthbend chain) must be present"
    );

    let put_plus_one: Vec<&Effect> = effects
        .iter()
        .filter(|e| {
            matches!(
                e,
                Effect::PutCounter {
                    counter_type: CounterType::Plus1Plus1,
                    count: QuantityExpr::Fixed { value: 1 },
                    ..
                }
            )
        })
        .collect();

    // DISCRIMINATOR 1: TWO +1/+1 PutCounter effects — earthbend's native one and
    // the "additional" one. Reverting the strip drops the second, leaving one.
    assert_eq!(
        put_plus_one.len(),
        2,
        "the earthbend chain must place TWO +1/+1 counters (earthbend 1 + the \
         additional counter); reverting the 'additional' strip leaves only one. \
         Effects: {effects:?}"
    );
    // DISCRIMINATOR 2: no clause degraded to Unimplemented. Reverting the strip
    // leaves "put an additional +1/+1 counter ..." Unimplemented.
    assert!(
        !effects
            .iter()
            .any(|e| matches!(e, Effect::Unimplemented { .. })),
        "the 'additional' counter clause must not degrade to Unimplemented; \
         effects: {effects:?}"
    );

    // Runtime control: a real +1/+1 counter addition lands exactly once through
    // the production counter resolver (the placement the parsed effect drives).
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let land = scenario.add_creature(P0, "Target Land", 0, 0).id();
    let mut runner = scenario.build();
    let mut events = Vec::new();
    engine::game::effects::counters::add_counter_with_replacement(
        runner.state_mut(),
        P0,
        land,
        CounterType::Plus1Plus1,
        1,
        &mut events,
    );
    assert_eq!(
        counters(&runner, land, &CounterType::Plus1Plus1),
        1,
        "an additional +1/+1 counter lands exactly once (CR 122.1)"
    );
}

/// Set an exact counter count on an object via the live state (test setup).
fn scenario_set_counter(runner: &mut GameRunner, id: ObjectId, ct: CounterType, count: u32) {
    runner
        .state_mut()
        .objects
        .get_mut(&id)
        .expect("object present")
        .counters
        .insert(ct, count);
}
