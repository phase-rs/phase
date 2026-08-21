//! CR 603.3 + CR 117.5 — a triggered ability must not reach the stack while a
//! replacement pause is open, on the GENERIC (non-combat) path.
//!
//! This file exists to isolate ONE production conjunct: the
//! `|| state.pending_replacement.is_some()` disjunct in `engine_priority`
//! ("Half A"). Its sibling exclusion ("Half B") keys on
//! `state.pending_combat_lifelink`, so Half B can only ever act when a
//! combat-damage batch is parked. Every board here raises its CR 616.1 ordering
//! choice from a SPELL's lifelink damage during that spell's own resolution, so
//! `pending_combat_lifelink` is `None` throughout and Half B's exclusion vector is
//! empty — its `!contains(event)` conjunct is vacuously true for every event.
//! Half A is therefore the only thing under test.
//!
//! The lifelink source is a spell rather than a creature deliberately: CR 702.15b
//! attaches lifelink to the SOURCE of the damage, and a spell source keeps the
//! whole event out of the combat-damage machinery.

use super::rules::{GameRunner, GameScenario, Phase, WaitingFor, Zone, P0, P1};
use engine::types::ability::{
    AbilityDefinition, AbilityKind, Effect, QuantityExpr, QuantityRef, ReplacementDefinition,
    TargetFilter, TypedFilter,
};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::player::PlayerId;
use engine::types::replacements::ReplacementEvent;
use engine::types::triggers::TriggerMode;

/// A "whenever this creature is dealt damage" observer whose effect is COUNTABLE
/// and is not itself a life gain — a life-gain observer would meet the very
/// replacements under test and confuse the instrument.
const DAMAGE_OBSERVER: &str =
    "Whenever this creature is dealt damage, put a +1/+1 counter on this creature.";

fn event_amount() -> QuantityExpr {
    QuantityExpr::Ref {
        qty: QuantityRef::EventContextAmount,
    }
}

fn gain_life_replacement(amount: QuantityExpr) -> ReplacementDefinition {
    ReplacementDefinition::new(ReplacementEvent::GainLife).execute(AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::GainLife {
            amount,
            player: TargetFilter::Controller,
        },
    ))
}

/// The non-commuting CR 616.1 pair: `2(n+1) != 2n+1`, so
/// `replacement_ordering_is_material` is true and a real choice is forced.
fn install_competing_life_gain_replacements(scenario: &mut GameScenario, player: PlayerId) {
    scenario
        .add_creature(player, "Rhox Faithmender", 1, 5)
        .with_replacement_definition(gain_life_replacement(QuantityExpr::Multiply {
            factor: 2,
            inner: Box::new(event_amount()),
        }));
    scenario
        .add_creature(player, "Leyline of Hope", 1, 1)
        .with_replacement_definition(gain_life_replacement(QuantityExpr::Offset {
            inner: Box::new(event_amount()),
            offset: 1,
        }));
}

/// A single life-gain replacement — order is immaterial with one applicable
/// effect (CR 616.1 asks for a choice only when two or more apply), so this board
/// raises NO prompt. Used as the no-pause positive control.
fn install_single_life_gain_replacement(scenario: &mut GameScenario, player: PlayerId) {
    scenario
        .add_creature(player, "Rhox Faithmender", 1, 5)
        .with_replacement_definition(gain_life_replacement(QuantityExpr::Multiply {
            factor: 2,
            inner: Box::new(event_amount()),
        }));
}

/// P0 gets a lifelink damage spell; P1 gets the observer creature.
/// Returns `(runner, spell, observer)`.
fn board(competing: bool) -> (GameRunner, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = {
        let mut builder = scenario.add_spell_to_hand(P0, "Lifelink Bolt", true);
        builder.with_ability(Effect::DealDamage {
            amount: QuantityExpr::Fixed { value: 3 },
            target: TargetFilter::Typed(TypedFilter::creature()),
            damage_source: None,
            excess: None,
        });
        builder.with_keyword(Keyword::Lifelink);
        builder.id()
    };
    // 2/6 so it survives the 3 damage and stays on the battlefield for the count.
    let observer = scenario
        .add_creature_from_oracle(P1, "Damage Observer", 2, 6, DAMAGE_OBSERVER)
        .id();
    if competing {
        install_competing_life_gain_replacements(&mut scenario, P0);
    } else {
        install_single_life_gain_replacement(&mut scenario, P0);
    }
    let runner = scenario.build();
    (runner, spell, observer)
}

fn observer_counters(runner: &GameRunner, observer: ObjectId) -> u32 {
    runner
        .state()
        .objects
        .get(&observer)
        .and_then(|obj| obj.counters.get(&CounterType::Plus1Plus1).copied())
        .unwrap_or(0)
}

/// V4 + H4 — Half A alone, with Half B provably inert.
///
/// EXPECTED RED (stated before the probe was run): the pause-time
/// `assert!(runner.state().stack.is_empty(), ..)`. Reverting the
/// `|| state.pending_replacement.is_some()` disjunct sends the observer's
/// `DamageReceived` context down the `else` arm to `triggers::process_triggers`,
/// which pushes onto `state.stack`, so the count goes 0 -> 1 while the CR 616.1
/// prompt is still open.
///
/// Half B cannot cover this board: `pending_combat_lifelink` is `None` here (a
/// reach guard asserts it), so its exclusion vector is empty and its conjunct is
/// vacuously true for every event.
#[test]
fn parked_replacement_defers_a_noncombat_observer_off_the_stack() {
    let (mut runner, spell, observer) = board(true);

    // Reach guard: the observer really is wired, so a later count of 0 cannot mean
    // "there was never a trigger".
    assert!(
        runner.state().objects[&observer]
            .trigger_definitions
            .iter_unchecked()
            .any(|entry| entry.definition.mode == TriggerMode::DamageReceived),
        "reach guard: the observer must carry a DamageReceived trigger"
    );

    let _ = runner.cast(spell).target_object(observer).resolve();

    // Reach guards for the negative below.
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ),
        "reach guard: CR 616.1 — the lifelink gain must PARK; got {:?}",
        runner.state().waiting_for
    );
    assert_eq!(
        runner.state().phase,
        Phase::PreCombatMain,
        "reach guard: this is the generic path, nowhere near combat damage"
    );
    assert_eq!(
        runner.life(P0),
        20,
        "reach guard: CR 702.15b — the gain has NOT been applied yet"
    );
    // H4: the axis that makes this test Half-A-only.
    assert!(
        runner.state().pending_combat_lifelink.is_none(),
        "reach guard: no combat-damage batch is parked, so Half B's exclusion is \
         provably inert and Half A is the only conjunct under test"
    );
    assert_eq!(
        observer_counters(&runner, observer),
        0,
        "reach guard: the observer's effect has not landed while the prompt is open"
    );

    // THE ASSERTION. CR 603.3 + CR 117.5: triggered abilities are put on the stack
    // only when a player WOULD receive priority, and no player receives priority
    // for a CR 616.1 choice.
    assert!(
        runner.state().stack.is_empty(),
        "CR 603.3 + CR 117.5: no triggered ability may be put on the stack while a \
         replacement pause is open — the event that triggered it has not finished \
         happening and no player has received priority"
    );

    // Answer, then confirm the deferred context drains exactly once (V5).
    runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("the CR 616.1 ordering choice must be answerable");
    for _ in 0..48 {
        if runner.state().stack.is_empty()
            && matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
        {
            break;
        }
        match runner.state().waiting_for.clone() {
            WaitingFor::ReplacementChoice { .. } => {
                if runner
                    .act(GameAction::ChooseReplacement { index: 0 })
                    .is_err()
                {
                    break;
                }
            }
            WaitingFor::OrderTriggers { triggers, .. } => {
                let order = (0..triggers.len()).collect();
                if runner.act(GameAction::OrderTriggers { order }).is_err() {
                    break;
                }
            }
            WaitingFor::Priority { .. } => {
                if runner.act(GameAction::PassPriority).is_err() {
                    break;
                }
            }
            other => panic!("no further prompt is owed: {other:?}"),
        }
    }

    assert!(
        runner.life(P0) > 20,
        "the lifelink gain lands once the ordering choice is answered"
    );
    assert_eq!(
        observer_counters(&runner, observer),
        1,
        "V5 + CR 603.3b: the deferred observer drains EXACTLY once — `==`, never \
         `>=`, so this fails in both directions"
    );
    assert!(
        runner.state().deferred_triggers.is_empty(),
        "nothing may be left parked in the deferred queue once the answer settles"
    );
    assert_eq!(
        runner.state().objects[&observer].zone,
        Zone::Battlefield,
        "the 2/6 observer survives 3 damage, so the count above reads a live object"
    );
}

/// POSITIVE CONTROL for the row above: the identical board with ONE life-gain
/// replacement raises no CR 616.1 choice, so nothing is ever deferred and the
/// observer reaches the stack on the ordinary path. Without this, "fires exactly
/// once" above could not be distinguished from an observer that is simply
/// incapable of firing more than once on this board.
#[test]
fn unpaused_noncombat_observer_reaches_the_stack_immediately() {
    let (mut runner, spell, observer) = board(false);

    let _ = runner.cast(spell).target_object(observer).resolve();

    assert!(
        !matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ),
        "reach guard: one applicable replacement is not a CR 616.1 choice"
    );
    assert_eq!(
        runner.life(P0),
        26,
        "reach guard: CR 702.15b + the doubler — 3 damage becomes 6 life, applied \
         inline, so a life-gain event really did occur"
    );

    for _ in 0..48 {
        if runner.state().stack.is_empty()
            && matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
        {
            break;
        }
        match runner.state().waiting_for.clone() {
            WaitingFor::OrderTriggers { triggers, .. } => {
                let order = (0..triggers.len()).collect();
                if runner.act(GameAction::OrderTriggers { order }).is_err() {
                    break;
                }
            }
            WaitingFor::Priority { .. } => {
                if runner.act(GameAction::PassPriority).is_err() {
                    break;
                }
            }
            other => panic!("no further prompt is owed: {other:?}"),
        }
    }

    assert_eq!(
        observer_counters(&runner, observer),
        1,
        "the observer fires exactly once on the path that never pauses"
    );
}
