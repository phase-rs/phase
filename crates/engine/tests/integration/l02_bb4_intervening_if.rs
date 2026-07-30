//! L02 BB4 — intervening-if conditions on five Standard triggers.
//!
//! CR 603.4 (intervening-if, checked at fire AND resolution) attaches a trigger
//! `condition` for five cards that previously swallowed their "if …" clause:
//!   - Massacre Girl, Known Killer — death P/T-vs-fixed snapshot (CR 603.10a)
//!   - Sharp-Eyed Rookie          — entering P/T-vs-source, live (CR 603.6a)
//!   - Fearless Swashbuckler       — typed attack-declaration conjunction (CR 508.1)
//!   - Rubblebelt Braggart         — source suspected designation (CR 701.60a/d)
//!   - Stalwart Successor          — first-counter-per-object this turn (CR 122.1)
//!
//! Each card has a parse-fidelity row (exact `condition` payload + no swallowed
//! clause) and a discriminating runtime row driven through the real trigger
//! pipeline. Oracle text is verbatim from Scryfall.

use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::parser::oracle::parse_oracle_text;
use engine::parser::oracle_ir::diagnostic::OracleDiagnostic;
use engine::types::ability::{
    AttackersDeclaredCountSubject, Comparator, ControllerRef, Effect, FilterProp, ObjectScope,
    PtStat, PtValueScope, QuantityExpr, QuantityRef, TargetFilter, TriggerCondition, TypeFilter,
};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::counter::CounterType;
use engine::types::game_state::WaitingFor;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::zones::Zone;
use engine::types::ObjectId;
use engine::types::PlayerId;

const PUT_PLUS_ONE: &str = "Put a +1/+1 counter on target creature.";
const PUT_MINUS_ONE: &str = "Put a -1/-1 counter on target creature.";
const DESTROY: &str = "Destroy target creature.";

// ---------------------------------------------------------------------------
// Verbatim Oracle text (Scryfall, 2026-07-11)
// ---------------------------------------------------------------------------

const MASSACRE_GIRL: &str = "Menace\n\
    Creatures you control have wither. (They deal damage to creatures in the form of -1/-1 counters.)\n\
    Whenever a creature an opponent controls dies, if its toughness was less than 1, draw a card.";

const SHARP_EYED_ROOKIE: &str = "Vigilance\n\
    Whenever a creature you control enters, if its power is greater than this creature's power or \
    its toughness is greater than this creature's toughness, put a +1/+1 counter on this creature \
    and investigate. (Create a Clue token. It's an artifact with \"{2}, Sacrifice this token: Draw a card.\")";

const FEARLESS_SWASHBUCKLER: &str = "Haste\n\
    Vehicles you control have haste.\n\
    Whenever you attack, if a Pirate and a Vehicle attacked this combat, draw three cards, then discard two cards.";

const RUBBLEBELT_BRAGGART: &str =
    "Whenever this creature attacks, if it's not suspected, you may suspect it. \
    (A suspected creature has menace and can't block.)";

const STALWART_SUCCESSOR: &str = "Menace (This creature can't be blocked except by two or more creatures.)\n\
    Whenever one or more counters are put on a creature you control, if it's the first time counters \
    have been put on that creature this turn, put a +1/+1 counter on that creature.";

// ---------------------------------------------------------------------------
// Parse helpers
// ---------------------------------------------------------------------------

fn parse_condition(oracle: &str, name: &str, keywords: &[&str]) -> Option<TriggerCondition> {
    let kw: Vec<String> = keywords.iter().map(|s| s.to_string()).collect();
    let parsed = parse_oracle_text(oracle, name, &kw, &["Creature".to_string()], &[]);
    let trigger = parsed
        .triggers
        .into_iter()
        .find(|t| t.condition.is_some())
        .or_else(|| {
            parse_oracle_text(oracle, name, &kw, &["Creature".to_string()], &[])
                .triggers
                .into_iter()
                .next()
        });
    trigger.and_then(|t| t.condition)
}

/// True when the parse produced a `SwallowedClause` diagnostic with `detector`.
fn has_swallowed(oracle: &str, name: &str, keywords: &[&str], detector: &str) -> bool {
    let kw: Vec<String> = keywords.iter().map(|s| s.to_string()).collect();
    let parsed = parse_oracle_text(oracle, name, &kw, &["Creature".to_string()], &[]);
    parsed.parse_warnings.iter().any(|w| {
        matches!(
            w,
            OracleDiagnostic::SwallowedClause { detector: d, .. } if d == detector
        )
    })
}

// ---------------------------------------------------------------------------
// P (parse fidelity) — Massacre Girl
// ---------------------------------------------------------------------------

#[test]
fn massacre_girl_condition_is_death_toughness_snapshot() {
    let cond = parse_condition(MASSACRE_GIRL, "Massacre Girl, Known Killer", &["Menace"])
        .expect("Massacre Girl trigger must carry an intervening-if condition");
    match cond {
        TriggerCondition::ZoneChangeObjectMatchesFilter {
            origin,
            destination,
            filter,
        } => {
            // CR 603.10a: death look-back snapshot — origin battlefield, dest graveyard.
            assert_eq!(origin, Some(Zone::Battlefield));
            assert_eq!(destination, Zone::Graveyard);
            let props = typed_props(&filter);
            assert_eq!(
                props,
                &[FilterProp::PtComparison {
                    stat: PtStat::Toughness,
                    scope: PtValueScope::Current,
                    comparator: Comparator::LT,
                    value: QuantityExpr::Fixed { value: 1 },
                }],
                "expected toughness < 1 (fixed), got {props:?}"
            );
        }
        other => panic!("expected ZoneChangeObjectMatchesFilter, got {other:?}"),
    }
    // Positive reach-guard pairs the negative: the clause was consumed into the
    // condition (above), so the swallow being absent is non-vacuous.
    assert!(
        !has_swallowed(
            MASSACRE_GIRL,
            "Massacre Girl, Known Killer",
            &["Menace"],
            "Condition_If"
        ),
        "Condition_If must be cleared once the condition attaches"
    );
}

// ---------------------------------------------------------------------------
// P (discriminating) — BB-FU3: other-death POSTFIX dying-subject threading.
//
// Synthetic card (no real DB card has an other-death "if its power was N or
// greater" clause): the POSTFIX threshold is what discriminates the fix from the
// retired `parse_inner_condition` proxy. Massacre Girl's INFIX "toughness was
// less than 1" never trips the postfix-only proxy, so it could not exercise the
// discriminator — this fixture can.
//
// Revert-probe (measured, proves non-vacuity): under the retired proxy this
// clause binds a Source-scoped `TriggerCondition::QuantityComparison { lhs:
// Power(Source), comparator: GE, rhs: Fixed(3) }` — the postfix "3 or greater"
// makes `parse_source_power_toughness_condition` succeed, the proxy fires, the
// dying gate declines (`Err`), and the intervening-if path binds to the SOURCE.
// Threading `dying_subject = Typed{Creature, Opponent}` (non-SelfRef) selects the
// `Some(_)` arm → the event-object `ZoneChangeObjectMatchesFilter` asserted below.
// ---------------------------------------------------------------------------

const SYNTHETIC_OTHER_DEATH_POWER: &str =
    "Whenever a creature an opponent controls dies, if its power was 3 or greater, draw a card.";

#[test]
fn other_death_power_postfix_binds_event_object_not_source() {
    let cond = parse_condition(
        SYNTHETIC_OTHER_DEATH_POWER,
        "Test Dying Power Snapshot",
        &[],
    )
    .expect("other-death power-snapshot trigger must carry an intervening-if condition");

    // Non-vacuous negative: under the reverted threading this is a Source-scoped
    // QuantityComparison. The fix makes it the event-object filter below.
    assert!(
        !matches!(cond, TriggerCondition::QuantityComparison { .. }),
        "expected event-object filter, not a Source-scoped QuantityComparison; got {cond:?}"
    );

    match cond {
        TriggerCondition::ZoneChangeObjectMatchesFilter {
            origin,
            destination,
            filter,
        } => {
            // CR 603.10a: death look-back snapshot — origin battlefield, dest graveyard.
            assert_eq!(origin, Some(Zone::Battlefield));
            assert_eq!(destination, Zone::Graveyard);
            let props = typed_props(&filter);
            assert_eq!(
                props,
                &[FilterProp::PtComparison {
                    stat: PtStat::Power,
                    scope: PtValueScope::Current,
                    comparator: Comparator::GE,
                    value: QuantityExpr::Fixed { value: 3 },
                }],
                "expected power >= 3 (fixed) on the dying event object, got {props:?}"
            );
        }
        other => panic!("expected ZoneChangeObjectMatchesFilter (event object), got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// P — Sharp-Eyed Rookie
// ---------------------------------------------------------------------------

#[test]
fn sharp_eyed_condition_is_entering_pt_vs_source_disjunction() {
    let cond = parse_condition(SHARP_EYED_ROOKIE, "Sharp-Eyed Rookie", &["Vigilance"])
        .expect("Sharp-Eyed trigger must carry an intervening-if condition");
    match cond {
        TriggerCondition::ZoneChangeObjectMatchesFilter {
            origin,
            destination,
            filter,
        } => {
            // CR 603.6a: entering (live), no look-back — origin None, dest battlefield.
            assert_eq!(origin, None);
            assert_eq!(destination, Zone::Battlefield);
            let props = typed_props(&filter);
            assert_eq!(
                props,
                &[FilterProp::AnyOf {
                    props: vec![
                        FilterProp::PtComparison {
                            stat: PtStat::Power,
                            scope: PtValueScope::Current,
                            comparator: Comparator::GT,
                            value: QuantityExpr::Ref {
                                qty: QuantityRef::Power {
                                    scope: ObjectScope::Source
                                }
                            },
                        },
                        FilterProp::PtComparison {
                            stat: PtStat::Toughness,
                            scope: PtValueScope::Current,
                            comparator: Comparator::GT,
                            value: QuantityExpr::Ref {
                                qty: QuantityRef::Toughness {
                                    scope: ObjectScope::Source
                                }
                            },
                        },
                    ]
                }],
                "expected power>src OR toughness>src, got {props:?}"
            );
        }
        other => panic!("expected ZoneChangeObjectMatchesFilter, got {other:?}"),
    }
    assert!(
        !has_swallowed(
            SHARP_EYED_ROOKIE,
            "Sharp-Eyed Rookie",
            &["Vigilance"],
            "Condition_If"
        ),
        "Condition_If must be cleared once the condition attaches"
    );
    // The residual effect must NOT keep a stranded leading "if" (regression lock:
    // the possessive recognizer must consume "if " so `strip_condition_clause`
    // removes the whole clause — otherwise the effect degrades to Unimplemented).
    let parsed = parse_oracle_text(
        SHARP_EYED_ROOKIE,
        "Sharp-Eyed Rookie",
        &["Vigilance".to_string()],
        &["Creature".to_string()],
        &[],
    );
    let execute = parsed.triggers[0]
        .execute
        .as_ref()
        .expect("Sharp-Eyed trigger has an execute");
    assert!(
        matches!(*execute.effect, Effect::PutCounter { .. }),
        "residual effect must be PutCounter, not a stranded-if Unimplemented: {:?}",
        execute.effect
    );
}

// ---------------------------------------------------------------------------
// P — Fearless Swashbuckler (+ R1: discard sub-ability must survive)
// ---------------------------------------------------------------------------

#[test]
fn fearless_condition_is_typed_attack_conjunction() {
    let cond = parse_condition(FEARLESS_SWASHBUCKLER, "Fearless Swashbuckler", &["Haste"])
        .expect("Fearless trigger must carry an intervening-if condition");
    match cond {
        TriggerCondition::And { conditions } => {
            assert_eq!(
                conditions.len(),
                2,
                "one AttackersDeclaredCount per typed noun"
            );
            for c in &conditions {
                match c {
                    TriggerCondition::AttackersDeclaredCount {
                        subject: AttackersDeclaredCountSubject::Controller { scope, filter },
                        comparator,
                        count,
                    } => {
                        assert_eq!(*scope, ControllerRef::You);
                        assert_eq!(*comparator, Comparator::GE);
                        assert_eq!(*count, 1);
                        assert!(filter.is_some(), "typed filter must be present");
                    }
                    other => panic!("expected AttackersDeclaredCount, got {other:?}"),
                }
            }
            // The two nouns are Pirate and Vehicle (order preserved).
            let subtypes: Vec<String> = conditions
                .iter()
                .filter_map(|c| match c {
                    TriggerCondition::AttackersDeclaredCount {
                        subject:
                            AttackersDeclaredCountSubject::Controller {
                                filter: Some(f), ..
                            },
                        ..
                    } => first_subtype(f),
                    _ => None,
                })
                .collect();
            assert_eq!(subtypes, vec!["Pirate".to_string(), "Vehicle".to_string()]);
        }
        other => panic!("expected And[..], got {other:?}"),
    }
}

/// R1: the "then discard two cards" tail must NOT be dropped once the leading
/// "if" is peeled. No swallow detector covers a lost "then discard", so this
/// asserts the AST directly. If the discard is absent, BB4 does not mark Fearless
/// done (stop-and-return).
#[test]
fn fearless_effect_retains_draw_then_discard() {
    let parsed = parse_oracle_text(
        FEARLESS_SWASHBUCKLER,
        "Fearless Swashbuckler",
        &["Haste".to_string()],
        &["Creature".to_string()],
        &[],
    );
    let trigger = parsed
        .triggers
        .iter()
        .find(|t| t.condition.is_some())
        .expect("Fearless trigger present");
    let execute = trigger
        .execute
        .as_ref()
        .expect("Fearless trigger has an execute ability");
    // execute is Draw 3 with a Discard 2 sub_ability.
    assert!(
        matches!(*execute.effect, Effect::Draw { .. }),
        "top effect must be Draw, got {:?}",
        execute.effect
    );
    let sub = execute
        .sub_ability
        .as_ref()
        .expect("draw-three must chain a discard-two sub_ability");
    assert!(
        matches!(*sub.effect, Effect::Discard { .. }),
        "sub_ability must be Discard (the 'then discard two cards'), got {:?}",
        sub.effect
    );
}

// ---------------------------------------------------------------------------
// P — Rubblebelt Braggart
// ---------------------------------------------------------------------------

#[test]
fn rubblebelt_condition_is_not_source_suspected() {
    let cond = parse_condition(RUBBLEBELT_BRAGGART, "Rubblebelt Braggart", &[])
        .expect("Rubblebelt trigger must carry an intervening-if condition");
    match cond {
        TriggerCondition::Not { condition } => match *condition {
            TriggerCondition::SourceMatchesFilter { filter } => {
                assert_eq!(typed_props(&filter), &[FilterProp::Suspected]);
            }
            other => panic!("expected SourceMatchesFilter, got {other:?}"),
        },
        other => panic!("expected Not{{SourceMatchesFilter}}, got {other:?}"),
    }
    assert!(
        !has_swallowed(
            RUBBLEBELT_BRAGGART,
            "Rubblebelt Braggart",
            &[],
            "Condition_If"
        ),
        "Condition_If must be cleared once the condition attaches"
    );
}

// ---------------------------------------------------------------------------
// P — Stalwart Successor (+ both swallows cleared)
// ---------------------------------------------------------------------------

#[test]
fn stalwart_condition_is_first_time_counters_and_both_swallows_clear() {
    let cond = parse_condition(STALWART_SUCCESSOR, "Stalwart Successor", &["Menace"])
        .expect("Stalwart trigger must carry an intervening-if condition");
    assert_eq!(cond, TriggerCondition::FirstTimeObjectCountersAddedThisTurn);
    // Both the Condition_If and the Duration_ThisTurn swallow must clear — the
    // latter requires the new marker in detect_duration_this_turn.
    assert!(
        !has_swallowed(
            STALWART_SUCCESSOR,
            "Stalwart Successor",
            &["Menace"],
            "Condition_If"
        ),
        "Condition_If must clear"
    );
    assert!(
        !has_swallowed(
            STALWART_SUCCESSOR,
            "Stalwart Successor",
            &["Menace"],
            "Duration_ThisTurn"
        ),
        "Duration_ThisTurn must clear (new marker)"
    );
}

// ---------------------------------------------------------------------------
// Shared filter-inspection helpers
// ---------------------------------------------------------------------------

fn typed_props(filter: &TargetFilter) -> &[FilterProp] {
    match filter {
        TargetFilter::Typed(tf) => &tf.properties,
        other => panic!("expected TargetFilter::Typed, got {other:?}"),
    }
}

fn first_subtype(filter: &TargetFilter) -> Option<String> {
    match filter {
        TargetFilter::Typed(tf) => tf.type_filters.iter().find_map(|t| match t {
            TypeFilter::Subtype(s) => Some(s.clone()),
            _ => None,
        }),
        _ => None,
    }
}

fn p1p1(runner: &GameRunner, id: ObjectId) -> u32 {
    runner
        .state()
        .objects
        .get(&id)
        .and_then(|o| o.counters.get(&CounterType::Plus1Plus1).copied())
        .unwrap_or(0)
}

// ===========================================================================
// R (runtime, real trigger pipeline) — Stalwart Successor
// ===========================================================================
//
// A {0} "Put a +1/+1 counter on target creature" spell induces a CounterAdded
// event on a creature P0 controls, which is the production entry into Stalwart's
// `TriggerMode::CounterAdded` trigger + `FirstTimeObjectCountersAddedThisTurn`
// gate.

/// The first counter on a creature this turn fires Stalwart exactly once: the
/// creature ends with two +1/+1 counters (the spell's, plus Stalwart's grant),
/// and Stalwart's own +1/+1 does NOT self-retrigger (the count-==-1 gate blocks
/// it). Reverting the runtime arm to an ungated fire pushes the total past 2.
#[test]
fn stalwart_first_counter_fires_once() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_creature_from_oracle(P0, "Stalwart Successor", 3, 2, STALWART_SUCCESSOR)
        .id();
    let a = scenario.add_creature(P0, "Grizzly Bear", 2, 2).id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Bolster", true, PUT_PLUS_ONE)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let mut runner = scenario.build();

    let out = runner.cast(spell).target_object(a).resolve();
    assert_eq!(
        out.counters(a, CounterType::Plus1Plus1),
        2,
        "first counter → Stalwart fires once (spell +1/+1 and Stalwart +1/+1); \
         self-retrigger gated"
    );
}

/// A SECOND counter placed on the same creature the same turn is gated: Stalwart
/// does NOT fire again, so the creature gains only the second spell's +1/+1
/// (2 → 3), never a fourth Stalwart counter. This is the cond-FALSE discriminator
/// — with `def.condition = None`, Stalwart fires ungated on the second counter
/// (and self-loops), pushing the total well past 3.
#[test]
fn stalwart_second_counter_same_turn_gated() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_creature_from_oracle(P0, "Stalwart Successor", 3, 2, STALWART_SUCCESSOR)
        .id();
    let a = scenario.add_creature(P0, "Grizzly Bear", 2, 2).id();
    let s1 = scenario
        .add_spell_to_hand_from_oracle(P0, "Bolster One", true, PUT_PLUS_ONE)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let s2 = scenario
        .add_spell_to_hand_from_oracle(P0, "Bolster Two", true, PUT_PLUS_ONE)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let mut runner = scenario.build();

    runner.cast(s1).target_object(a).resolve();
    // Reach-guard: the first counter DID fire Stalwart (A has 2), so the second
    // assertion below exercises the gate, not a never-reached arm.
    assert_eq!(
        p1p1(&runner, a),
        2,
        "reach-guard: first counter fired Stalwart"
    );

    let out = runner.cast(s2).target_object(a).resolve();
    assert_eq!(
        out.counters(a, CounterType::Plus1Plus1),
        3,
        "second counter same turn is gated: only the spell's +1/+1 lands (no Stalwart grant)"
    );
}

/// Per-OBJECT, not per-controller: after A receives its first counter (and its
/// Stalwart grant) this turn, a DIFFERENT creature B receiving its first counter
/// the same turn still fires Stalwart. A per-controller (`record.actor`) model
/// like the sibling `CounterAddedThisTurn` would see A's records and wrongly gate
/// B (B would stay at 1); the per-object model fires B (B → 2).
#[test]
fn stalwart_per_object_second_creature_fires() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_creature_from_oracle(P0, "Stalwart Successor", 3, 2, STALWART_SUCCESSOR)
        .id();
    let a = scenario.add_creature(P0, "Grizzly Bear", 2, 2).id();
    let b = scenario.add_creature(P0, "Hill Giant", 3, 3).id();
    let s1 = scenario
        .add_spell_to_hand_from_oracle(P0, "Bolster A", true, PUT_PLUS_ONE)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let s2 = scenario
        .add_spell_to_hand_from_oracle(P0, "Bolster B", true, PUT_PLUS_ONE)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let mut runner = scenario.build();

    runner.cast(s1).target_object(a).resolve();
    assert_eq!(
        p1p1(&runner, a),
        2,
        "reach-guard: A's first counter fired Stalwart"
    );

    let out = runner.cast(s2).target_object(b).resolve();
    assert_eq!(
        out.counters(b, CounterType::Plus1Plus1),
        2,
        "B's first counter fires Stalwart independently (per-object, not per-controller)"
    );
    assert_eq!(
        out.counters(a, CounterType::Plus1Plus1),
        2,
        "A is unchanged by B's counter"
    );
}

// ===========================================================================
// R — Sharp-Eyed Rookie (entering P/T vs source, live)
// ===========================================================================

/// Cast a vanilla {0} creature of the given P/T; the ETB is the production entry
/// into Sharp-Eyed's `TriggerMode::ChangesZone` → battlefield trigger.
fn sharp_eyed_entrant(
    power: i32,
    toughness: i32,
) -> (engine::game::scenario::CastOutcome, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_creature_from_oracle(P0, "Sharp-Eyed Rookie", 2, 2, SHARP_EYED_ROOKIE)
        .id();
    let entrant = scenario
        .add_creature_to_hand(P0, "Newcomer", power, toughness)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let mut runner = scenario.build();
    let out = runner.cast(entrant).resolve();
    (out, source)
}

fn has_clue(out: &engine::game::scenario::CastOutcome) -> bool {
    out.state()
        .objects
        .values()
        .any(|o| o.name == "Clue" && o.zone == Zone::Battlefield)
}

/// Entrant with power greater than the source (3/1 vs 2/2) → the disjunction's
/// power arm is true → +1/+1 counter on Sharp-Eyed and a Clue.
#[test]
fn sharp_eyed_bigger_power_grants_counter_and_clue() {
    let (out, source) = sharp_eyed_entrant(3, 1);
    assert_eq!(
        out.counters(source, CounterType::Plus1Plus1),
        1,
        "power>source → +1/+1"
    );
    assert!(has_clue(&out), "investigate creates a Clue");
}

/// Entrant with toughness greater than the source (1/3 vs 2/2) → the toughness
/// arm is true → +1/+1 counter (proves the disjunction, not power-only).
#[test]
fn sharp_eyed_bigger_toughness_grants_counter() {
    let (out, source) = sharp_eyed_entrant(1, 3);
    assert_eq!(
        out.counters(source, CounterType::Plus1Plus1),
        1,
        "toughness>source → +1/+1"
    );
}

/// Entrant equal to the source (2/2 vs 2/2) → neither stat is strictly greater →
/// NO counter. This discriminates the SOURCE comparison from any fixed threshold:
/// a 2/2 is not "power ≥ 3", but more importantly it equals the source, so the
/// live source-relative gate correctly declines.
#[test]
fn sharp_eyed_equal_entrant_no_counter() {
    let (out, source) = sharp_eyed_entrant(2, 2);
    assert_eq!(
        out.counters(source, CounterType::Plus1Plus1),
        0,
        "equal P/T → no counter (compared against the source, not a fixed value)"
    );
}

/// TIMING discriminator (CR 603.4: intervening-if re-checked as the ability
/// resolves): the entering-vs-source comparison reads the source's LIVE P/T at
/// RESOLUTION, not a value latched at fire time. A 3/3 entrant beats the 2/2
/// source at fire time (power 3 > 2) so the trigger fires and goes on the stack;
/// pumping the source to 4/4 BEFORE that trigger resolves makes the resolution
/// re-check FALSE on both arms (3 > 4 false, 3 > 4 false) → NO +1/+1 counter and
/// NO Clue.
///
/// Revert-failing assertion: `counters(source) == 0`. A fire-time latch (source
/// P/T snapshotted when the trigger fired at 2/2) would ignore the pump and still
/// grant the counter (== 1) plus a Clue. The reach-guard proves the trigger DID
/// fire (fire-time condition true), so the zero is the resolution-time decline,
/// not a never-fired trigger.
#[test]
fn sharp_eyed_source_pumped_after_fire_declines_at_resolution() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_creature_from_oracle(P0, "Sharp-Eyed Rookie", 2, 2, SHARP_EYED_ROOKIE)
        .id();
    let entrant = scenario
        .add_creature_to_hand(P0, "Newcomer", 3, 3)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let mut runner = scenario.build();

    // Commit the entrant to the stack, then resolve ONLY the creature spell so it
    // enters and Sharp-Eyed's ETB trigger fires onto the stack — stopping before
    // the trigger itself resolves.
    runner.cast(entrant).commit();
    let mut fired = false;
    for _ in 0..12 {
        let entered = runner
            .state()
            .objects
            .get(&entrant)
            .map(|o| o.zone == Zone::Battlefield)
            .unwrap_or(false);
        // The entrant is on the battlefield and a triggered ability sits on the
        // stack: the creature spell resolved and the ETB trigger fired. Break
        // here — do NOT pass priority again, or the trigger would resolve.
        if entered && !runner.state().stack.is_empty() {
            fired = true;
            break;
        }
        if runner.act(GameAction::PassPriority).is_err() {
            break;
        }
    }
    assert!(
        fired,
        "reach-guard: Sharp-Eyed ETB trigger fired and is on the stack \
         (entrant 3/3 beats source 2/2 at fire time)"
    );

    // Pump the source above the entrant BEFORE the trigger resolves. Set both base
    // and current P/T so a layer recompute during resolution cannot revert it.
    {
        let src = runner.state_mut().objects.get_mut(&source).unwrap();
        src.power = Some(4);
        src.toughness = Some(4);
        src.base_power = Some(4);
        src.base_toughness = Some(4);
    }

    runner.advance_until_stack_empty();

    assert_eq!(
        p1p1(&runner, source),
        0,
        "source pumped to 4/4 before resolution → 3 > 4 false on both arms → \
         no +1/+1 counter (live source read at resolution, CR 603.4)"
    );
    let clue = runner
        .state()
        .objects
        .values()
        .any(|o| o.name == "Clue" && o.zone == Zone::Battlefield);
    assert!(
        !clue,
        "condition false at resolution → investigate does not run → no Clue"
    );
}

// ===========================================================================
// R — Massacre Girl, Known Killer (death toughness snapshot)
// ===========================================================================

/// Opponent creature reduced to 0 toughness (1/1 + a -1/-1 counter) dies as an
/// SBA; the death snapshot toughness (0 < 1) satisfies the gate → P0 draws.
#[test]
fn massacre_opponent_toughness_below_one_draws() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_creature_from_oracle(P0, "Massacre Girl, Known Killer", 4, 4, MASSACRE_GIRL)
        .id();
    let victim = scenario.add_creature(P1, "Goblin", 1, 1).id();
    scenario.with_library_top(P0, &["D1", "D2", "D3"]);
    let shrink = scenario
        .add_spell_to_hand_from_oracle(P0, "Shrink", true, PUT_MINUS_ONE)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let mut runner = scenario.build();

    let out = runner.cast(shrink).target_object(victim).resolve();
    assert_eq!(
        out.zone_of(victim),
        Zone::Graveyard,
        "victim died to the -1/-1 (reach-guard)"
    );
    assert_eq!(
        out.hand_drawn(P0),
        1,
        "opponent dying with toughness 0 (<1) → Massacre draws a card"
    );
}

/// Opponent 2/2 destroyed dies with toughness 2 (≥ 1): the snapshot fails the
/// gate → NO draw. The reach-guard (victim in graveyard) makes the negative
/// non-vacuous — the death trigger path WAS reached, the condition declined.
#[test]
fn massacre_opponent_destroyed_positive_toughness_no_draw() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_creature_from_oracle(P0, "Massacre Girl, Known Killer", 4, 4, MASSACRE_GIRL)
        .id();
    let victim = scenario.add_creature(P1, "Ogre", 2, 2).id();
    scenario.with_library_top(P0, &["D1", "D2", "D3"]);
    let destroy = scenario
        .add_spell_to_hand_from_oracle(P0, "Murder", true, DESTROY)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let mut runner = scenario.build();

    let out = runner.cast(destroy).target_object(victim).resolve();
    assert_eq!(
        out.zone_of(victim),
        Zone::Graveyard,
        "victim died (reach-guard)"
    );
    assert_eq!(
        out.hand_drawn(P0),
        0,
        "toughness 2 at death (≥1) → no Massacre draw"
    );
}

/// P0's OWN creature dying with toughness 0 does NOT fire Massacre — the trigger
/// is scoped to creatures an OPPONENT controls (valid_card). Reach-guard: the
/// creature died, so the no-draw is non-vacuous.
#[test]
fn massacre_own_creature_death_no_draw() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_creature_from_oracle(P0, "Massacre Girl, Known Killer", 4, 4, MASSACRE_GIRL)
        .id();
    let own = scenario.add_creature(P0, "Goblin", 1, 1).id();
    scenario.with_library_top(P0, &["D1", "D2", "D3"]);
    let shrink = scenario
        .add_spell_to_hand_from_oracle(P0, "Shrink", true, PUT_MINUS_ONE)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let mut runner = scenario.build();

    let out = runner.cast(shrink).target_object(own).resolve();
    assert_eq!(
        out.zone_of(own),
        Zone::Graveyard,
        "own creature died (reach-guard)"
    );
    assert_eq!(
        out.hand_drawn(P0),
        0,
        "own creature death → Massacre (opponent-scoped) does not fire"
    );
}

// ===========================================================================
// R — Fearless Swashbuckler (typed attack-declaration conjunction)
// ===========================================================================

fn hand_len(runner: &GameRunner, player: PlayerId) -> usize {
    runner
        .state()
        .players
        .iter()
        .find(|p| p.id == player)
        .map(|p| p.hand.len())
        .unwrap_or(0)
}

/// Drive the post-attack stack to a quiet priority window, answering the
/// prompts Fearless (mandatory discard) and Rubblebelt (optional suspect) raise.
/// `accept_optional` decides any "you may" along the way.
fn drive_attack_prompts(runner: &mut GameRunner, accept_optional: bool) {
    for _ in 0..40 {
        runner.advance_until_stack_empty();
        match runner.state().waiting_for.clone() {
            WaitingFor::DiscardChoice { count, cards, .. } => {
                let picks: Vec<ObjectId> = cards.into_iter().take(count).collect();
                runner
                    .act(GameAction::SelectCards { cards: picks })
                    .expect("discard selection");
            }
            WaitingFor::OptionalEffectChoice { .. } => {
                runner
                    .act(GameAction::DecideOptionalEffect {
                        accept: accept_optional,
                    })
                    .expect("decide optional");
            }
            _ => break,
        }
    }
}

/// Build a Fearless board: the Swashbuckler plus a Pirate, and (when
/// `with_vehicle`) an artifact-creature Vehicle, all controlled by P0 and able
/// to attack. Returns (runner, pirate, vehicle_opt). Library stocked so the
/// draw-three has cards.
fn fearless_board(with_vehicle: bool) -> (GameRunner, ObjectId, Option<ObjectId>) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_creature_from_oracle(P0, "Fearless Swashbuckler", 3, 3, FEARLESS_SWASHBUCKLER)
        .id();
    let pirate = scenario
        .add_creature(P0, "Deadeye Buccaneer", 2, 2)
        .with_subtypes(vec!["Pirate"])
        .id();
    let vehicle = with_vehicle.then(|| {
        scenario
            .add_creature(P0, "Smuggler's Copter", 3, 3)
            .with_subtypes(vec!["Vehicle"])
            .id()
    });
    scenario.with_library_top(P0, &["D1", "D2", "D3", "D4"]);
    let mut runner = scenario.build();
    // Model a realistic artifact-creature Vehicle: add Artifact core type
    // post-build WITHOUT stripping Creature (`as_artifact` removes Creature, and a
    // non-creature can't attack). The "a Vehicle" filter matches on subtype
    // Vehicle; being an artifact-creature is the accurate crewed-Vehicle shape.
    if let Some(v) = vehicle {
        let obj = runner.state_mut().objects.get_mut(&v).unwrap();
        obj.card_types.core_types.push(CoreType::Artifact);
        obj.base_card_types.core_types.push(CoreType::Artifact);
    }
    runner.advance_to_combat();
    (runner, pirate, vehicle)
}

/// Both a Pirate AND a Vehicle attack → the conjunction is true → draw three,
/// discard two (net +1 hand). Reverting the condition to ungated changes nothing
/// here; reverting the *effect* wiring would; the discriminator is the negative
/// test below, and the reach-guard is the net-+1 delta proving the trigger fired.
#[test]
fn fearless_pirate_and_vehicle_draws_three_discards_two() {
    let (mut runner, pirate, vehicle) = fearless_board(true);
    let vehicle = vehicle.expect("vehicle present");
    let before = hand_len(&runner, P0);
    runner
        .declare_attackers(&[
            (pirate, AttackTarget::Player(P1)),
            (vehicle, AttackTarget::Player(P1)),
        ])
        .expect("declare Pirate + Vehicle");
    drive_attack_prompts(&mut runner, true);
    assert_eq!(
        hand_len(&runner, P0) as i64 - before as i64,
        1,
        "Pirate + Vehicle attacked → draw 3 then discard 2 (net +1)"
    );
}

/// Only a Pirate attacks (no Vehicle) → the Vehicle conjunct is false → the
/// intervening-if (CR 603.4) blocks the trigger at fire time → NO draw/discard,
/// hand unchanged. This is the cond-FALSE discriminator: with `condition = None`
/// Fearless would fire on any attack and the hand would move.
#[test]
fn fearless_only_pirate_no_fire() {
    let (mut runner, pirate, _none) = fearless_board(false);
    let before = hand_len(&runner, P0);
    runner
        .declare_attackers(&[(pirate, AttackTarget::Player(P1))])
        .expect("declare Pirate only");
    drive_attack_prompts(&mut runner, true);
    assert_eq!(
        hand_len(&runner, P0),
        before,
        "only a Pirate attacked → conjunction false → Fearless does not fire"
    );
}

/// Symmetric conjunct discriminator: only a Vehicle attacks (a Pirate is on the
/// board but is NOT declared as an attacker) → the Pirate conjunct of the And is
/// false → the intervening-if (CR 603.4) blocks the trigger → NO draw/discard,
/// hand unchanged. Mirror of `fearless_only_pirate_no_fire` for the other
/// conjunct. Non-vacuous: if the Pirate conjunct were dropped from the And, the
/// lone Vehicle would satisfy the condition and the hand would move (net +1);
/// the positive test proves declaring the Vehicle fires the "you attack" trigger.
#[test]
fn fearless_only_vehicle_no_fire() {
    let (mut runner, _pirate, vehicle) = fearless_board(true);
    let vehicle = vehicle.expect("vehicle present");
    let before = hand_len(&runner, P0);
    runner
        .declare_attackers(&[(vehicle, AttackTarget::Player(P1))])
        .expect("declare Vehicle only");
    drive_attack_prompts(&mut runner, true);
    assert_eq!(
        hand_len(&runner, P0),
        before,
        "only a Vehicle attacked → Pirate conjunct false → Fearless does not fire"
    );
}

// ===========================================================================
// R — Rubblebelt Braggart (source suspected designation)
// ===========================================================================

/// Attack with Rubblebelt, optionally pre-suspected, optionally accepting the
/// "you may suspect it" offer. Returns (offer_seen, final_is_suspected). The
/// offer only surfaces when the intervening-if (`not suspected`) holds at fire
/// time, so `offer_seen` is the direct cond-TRUE/FALSE discriminator.
fn rubblebelt_attack(pre_suspected: bool, accept: bool) -> (bool, bool) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let rb = scenario
        .add_creature_from_oracle(P0, "Rubblebelt Braggart", 5, 5, RUBBLEBELT_BRAGGART)
        .id();
    let mut runner = scenario.build();
    if pre_suspected {
        // CR 701.60a: designate suspected before combat (no zone move, so it
        // persists through the phase advance).
        runner
            .state_mut()
            .objects
            .get_mut(&rb)
            .unwrap()
            .is_suspected = true;
    }
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(rb, AttackTarget::Player(P1))])
        .expect("declare Rubblebelt");

    let mut offered = false;
    for _ in 0..40 {
        runner.advance_until_stack_empty();
        match runner.state().waiting_for.clone() {
            WaitingFor::OptionalEffectChoice { .. } => {
                offered = true;
                runner
                    .act(GameAction::DecideOptionalEffect { accept })
                    .expect("decide suspect offer");
            }
            _ => break,
        }
    }
    let suspected = runner.state().objects.get(&rb).unwrap().is_suspected;
    (offered, suspected)
}

/// Unsuspected attacker, accept the offer → becomes suspected. The offer being
/// presented (cond TRUE) plus the flip to suspected is the positive path.
#[test]
fn rubblebelt_unsuspected_accept_becomes_suspected() {
    let (offered, suspected) = rubblebelt_attack(false, true);
    assert!(
        offered,
        "not-suspected attacker → intervening-if true → offer presented"
    );
    assert!(
        suspected,
        "accepting the offer suspects Rubblebelt (CR 701.60a)"
    );
}

/// Unsuspected attacker, DECLINE the offer → the offer still appears (cond TRUE)
/// but Rubblebelt stays unsuspected — proves the flip is the accepted optional,
/// not a side effect of the trigger merely firing.
#[test]
fn rubblebelt_unsuspected_decline_stays_unsuspected() {
    let (offered, suspected) = rubblebelt_attack(false, false);
    assert!(offered, "not-suspected attacker → offer presented");
    assert!(
        !suspected,
        "declined optional → Rubblebelt stays unsuspected"
    );
}

/// Already-suspected attacker → the intervening-if (`not suspected`) is FALSE at
/// fire time (CR 603.4) → NO offer is ever presented. This is the cond-FALSE
/// discriminator: with `condition = None` the offer would appear regardless.
#[test]
fn rubblebelt_already_suspected_no_offer() {
    let (offered, suspected) = rubblebelt_attack(true, true);
    assert!(
        !offered,
        "already suspected → intervening-if false → no suspect offer"
    );
    assert!(suspected, "reach-guard: it was and remains suspected");
}
