//! CR 603.2c + CR 608.2c — WHICH set an anaphoric set-reference names.
//!
//! "their total power" / "the greatest power among them" / "… among those
//! creatures" are byte-identical phrases with two different referents, and they
//! resolve from two different pieces of state:
//!
//!   * the TRIGGERING BATCH — the objects of the current trigger event
//!     (`state.current_trigger_events`). Witch-king, Sky Scourge; Aloy, Savior
//!     of Meridian; Shriekwood Devourer; The Skullspore Nexus.
//!   * the CHAIN SET — the objects a PRECEDING clause of the same resolution
//!     published (`state.tracked_object_sets`). Kylox, Visionary Inventor, whose
//!     "their" is the creatures its own earlier clause just sacrificed.
//!
//! The leaf quantity combinator cannot tell them apart (the text is identical),
//! so it emits the batch reading and the clause layer re-anchors it to the chain
//! set when an EARLIER clause published one — `rebind_tracked_aggregate_to_chain_set`.
//!
//! The two readings are MUTUALLY DESTRUCTIVE, and this file pins that in both
//! directions: each card is asserted to produce the WRONG answer under the other
//! card's binding. A future change that collapses the two sources fails here.
//!
//! Oracle text is quoted from Scryfall, not from memory.

use engine::game::combat::AttackTarget;
use engine::game::game_object::ObjectIncarnationRefExt;
use engine::game::quantity::{
    resolve_quantity, resolve_quantity_with_ctx, resolve_quantity_with_targets, QuantityContext,
};
use engine::game::scenario::{GameScenario, P0, P1};
use engine::parser::parse_oracle_text;
use engine::types::ability::{
    AbilityDefinition, AggregateFunction, CardTypeSetSource, ControllerRef, DieResultBranch,
    Effect, ObjectProperty, QuantityExpr, QuantityRef, TargetFilter, TargetRef,
    TrackedAnaphorSource, TypedFilter,
};
use engine::types::events::GameEvent;
use engine::types::game_state::WaitingFor;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const KYLOX: &str = "Menace, ward {2}, haste\nWhenever Kylox attacks, sacrifice any number of other creatures, then exile the top X cards of your library, where X is their total power. You may cast any number of instant and/or sorcery spells from among the exiled cards without paying their mana costs.";
const WITCH_KING: &str = "Flying\nWhenever you attack with one or more Wraiths, exile the top X cards of your library, where X is their total power. You may play those cards this turn.";
const ALOY: &str = "Vigilance, reach\nWhenever one or more artifact creatures you control attack, discover X, where X is the greatest power among them.";
const SHRIEKWOOD: &str = "Trample\nWhenever you attack with one or more creatures, untap up to X lands, where X is the greatest power among those creatures.";
const SKULLSPORE: &str = "Whenever one or more nontoken creatures you control die, create a green Fungus Dinosaur creature token with base power and toughness each equal to the total power of those creatures.";
const ANGRATH: &str =
    "Destroy all creatures target opponent controls. Angrath deals damage to that player equal to their total power.";
const PREMONITION: &str = "When you set this scheme in motion, reveal the top two cards of your library and put them into your hand. When you reveal one or more nonland cards this way, this scheme deals damage equal to their total mana value to any target.";

/// Every `TrackedSetAggregate` reachable in a card's parse, as
/// `(function, property, source)`. Serialized so the walk is total — a new
/// nesting site cannot hide a binding from this census.
fn aggregates(
    name: &str,
    oracle: &str,
) -> Vec<(AggregateFunction, ObjectProperty, TrackedAnaphorSource)> {
    let parsed = parse_oracle_text(oracle, name, &[], &["Creature".to_string()], &[]);
    let json = serde_json::to_value(&parsed).expect("ParsedAbilities serializes");
    let mut out = Vec::new();
    collect(&json, &mut out);
    return out;

    fn collect(
        value: &serde_json::Value,
        out: &mut Vec<(AggregateFunction, ObjectProperty, TrackedAnaphorSource)>,
    ) {
        match value {
            serde_json::Value::Object(map) => {
                if map.get("type").and_then(|t| t.as_str()) == Some("PropertyAggregate") {
                    let qty: QuantityRef = serde_json::from_value(value.clone())
                        .expect("PropertyAggregate round-trips");
                    if let QuantityRef::PropertyAggregate(aggregate) = qty {
                        // Test census collects the `set` key only; member
                        // narrowing does not change the tally.
                        if let CardTypeSetSource::TrackedSet { set, .. } = aggregate.source() {
                            out.push((aggregate.function(), aggregate.property(), *set));
                        }
                    }
                }
                for v in map.values() {
                    collect(v, out);
                }
            }
            serde_json::Value::Array(items) => items.iter().for_each(|v| collect(v, out)),
            _ => {}
        }
    }
}

/// Does the card's parse contain an honest-failure marker?
fn has_unimplemented(name: &str, oracle: &str) -> bool {
    let parsed = parse_oracle_text(oracle, name, &[], &["Creature".to_string()], &[]);
    serde_json::to_string(&parsed)
        .expect("serializes")
        .contains("\"Unimplemented\"")
}

// ===========================================================================
// Parser: which source does each face bind?
// ===========================================================================

/// CR 608.2c: Kylox's "their" is the creatures its OWN PRECEDING CLAUSE
/// sacrificed — the chain-published set, not the attack batch. The sacrifice is
/// the publisher, and it sits earlier in the same resolution chain.
#[test]
fn kylox_their_total_power_binds_the_sacrificed_chain_set() {
    let bound = aggregates("Kylox, Visionary Inventor", KYLOX);
    assert_eq!(
        bound,
        vec![(
            AggregateFunction::Sum,
            ObjectProperty::Power,
            TrackedAnaphorSource::ChainSet
        )],
        "Kylox's 'their total power' must reduce the SACRIFICED chain set. \
         Binding it to the triggering batch reads the lone attacking Kylox \
         (or nothing) — a silent wrong answer."
    );
}

/// CR 603.2c: Witch-king's "their" is the ATTACKING WRAITHS — the triggering
/// batch. Its own `ExileTop` clause is itself a set publisher, which is exactly
/// why the chain scan must be STRICTLY PRIOR: a clause may not count itself as
/// its own antecedent's publisher.
#[test]
fn witch_king_their_total_power_binds_the_attacking_trigger_batch() {
    let bound = aggregates("Witch-king, Sky Scourge", WITCH_KING);
    assert_eq!(
        bound,
        vec![(
            AggregateFunction::Sum,
            ObjectProperty::Power,
            TrackedAnaphorSource::TriggeringBatch
        )],
        "Witch-king's 'their total power' must reduce the ATTACK BATCH. Its own \
         ExileTop publishes a set; if the prior-publisher scan counted the \
         current clause, this would flip to ChainSet and exile 0."
    );
}

/// CR 603.2c: "the greatest power among them" — the same batch referent under
/// the Max aggregate. t78 gated Max/Min out because the singleton event
/// extractor collapsed a multi-attacker batch to `None`; the set-valued
/// extractor lifts that gate.
#[test]
fn aloy_greatest_power_among_them_binds_the_trigger_batch() {
    let bound = aggregates("Aloy, Savior of Meridian", ALOY);
    assert_eq!(
        bound,
        vec![(
            AggregateFunction::Max,
            ObjectProperty::Power,
            TrackedAnaphorSource::TriggeringBatch
        )],
        "'the greatest power among them' must be a Max over the attack batch"
    );
}

/// CR 603.2c: the demonstrative twin of Aloy — "among those creatures".
#[test]
fn shriekwood_greatest_power_among_those_creatures_binds_the_trigger_batch() {
    let bound = aggregates("Shriekwood Devourer", SHRIEKWOOD);
    assert_eq!(
        bound,
        vec![(
            AggregateFunction::Max,
            ObjectProperty::Power,
            TrackedAnaphorSource::TriggeringBatch
        )],
        "'the greatest power among those creatures' must be a Max over the attack batch"
    );
}

/// NO-REGRESSION CONTROL. The Skullspore Nexus is the face t78 shipped on the
/// batch reading (a DIES batch). It has no preceding publisher, so it must be
/// byte-identical after the chain-set re-anchor lands: `Sum(Power)` over the
/// triggering batch, twice (base power AND base toughness).
#[test]
fn skullspore_nexus_dies_batch_aggregate_is_unchanged() {
    let bound = aggregates("The Skullspore Nexus", SKULLSPORE);
    assert_eq!(
        bound,
        vec![
            (
                AggregateFunction::Sum,
                ObjectProperty::Power,
                TrackedAnaphorSource::TriggeringBatch
            ),
            (
                AggregateFunction::Sum,
                ObjectProperty::Power,
                TrackedAnaphorSource::TriggeringBatch
            )
        ],
        "the dies-batch face must keep the TriggeringBatch reading — the chain-set \
         re-anchor must not touch a chain with no prior publisher"
    );
}

/// CR 603.2c: HONEST-RED CONTROL. A triggering-batch anaphor in an ability with
/// NO trigger event has no antecedent — there is no batch. Angrath, Minotaur
/// Pirate carries "their total power" in a LOYALTY ability, so binding it to the
/// batch would reduce an empty set to a confident 0 while the card rendered as
/// fully supported. It must stay an honest gap.
#[test]
fn angrath_batch_anaphor_in_a_non_trigger_ability_stays_an_honest_red() {
    let bound = aggregates("Angrath, Minotaur Pirate", ANGRATH);
    assert!(
        bound.is_empty(),
        "a TriggeringBatch aggregate in a chain with no trigger event is unbindable \
         — it must not be emitted. Got {bound:?}"
    );
    assert!(
        has_unimplemented("Angrath, Minotaur Pirate", ANGRATH),
        "the unbindable anaphor must fail honestly (Effect::unimplemented), not \
         resolve to a silent 0"
    );
}

/// CR 603.2c: HONEST-RED CONTROL #2 — a batch anaphor inside a trigger whose
/// EVENT carries no object set.
///
/// Being inside a trigger is not enough: the triggering event must actually
/// expose its subjects to `extract_sources_from_event`, which only the
/// declared-attackers batch (CR 508.1) and the per-object zone-change batch
/// (CR 603.10a) do. A Premonition of Your Demise's "their total mana value" sits
/// in a `SetInMotion` scheme trigger, whose event carries no revealed cards — so
/// binding it to "the batch" deals 0 damage while rendering as fully supported.
///
/// This face was caught by the full-pool ledger (it went RED -> GREEN as a
/// silent 0), which is exactly what the ledger is for. It must stay an honest gap
/// until the reveal batch is a real carrier.
#[test]
fn scheme_reveal_batch_anaphor_stays_an_honest_red() {
    let bound = aggregates("A Premonition of Your Demise", PREMONITION);
    assert!(
        bound.is_empty(),
        "a SetInMotion trigger's event exposes no revealed-card set, so the batch \
         anaphor is unbindable and must not be emitted. Got {bound:?}"
    );
    assert!(
        has_unimplemented("A Premonition of Your Demise", PREMONITION),
        "the unbindable anaphor must fail honestly, not deal 0 damage as a green"
    );
}

// ===========================================================================
// Runtime witnesses (real pipeline) + the MUTUAL RED controls
// ===========================================================================

/// RED-FIRST RUNTIME WITNESS (2+ attackers of DIFFERENT power).
///
/// CR 508.1 + CR 603.2c: Witch-king, Sky Scourge attacks with two Wraiths of
/// power 2 and 3. "X is their total power" must reduce the WHOLE batch: 5 cards
/// exiled, not 0 (the singleton extractor's empty set) and not 3 (one attacker).
///
/// Drives the real combat/trigger/resolution pipeline.
#[test]
fn witch_king_exiles_the_total_power_of_every_attacking_wraith() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let witch_king = scenario
        .add_creature_from_oracle(P0, "Witch-king, Sky Scourge", 2, 4, WITCH_KING)
        .with_subtypes(vec!["Wraith"])
        .id();
    let wraith = scenario
        .add_creature(P0, "Nazgul", 3, 3)
        .with_subtypes(vec!["Wraith"])
        .id();
    // A non-Wraith attacker of huge power: it is NOT in the trigger's subject
    // set, so it must not contribute (guards against a live-board Aggregate
    // misparse).
    let bystander = scenario.add_creature(P0, "Bystander", 9, 9).id();

    scenario.with_library_top(P0, &["L1", "L2", "L3", "L4", "L5", "L6", "L7"]);

    let mut runner = scenario.build();
    runner.advance_to_combat();
    runner
        .declare_attackers(&[
            (witch_king, AttackTarget::Player(P1)),
            (wraith, AttackTarget::Player(P1)),
            (bystander, AttackTarget::Player(P1)),
        ])
        .expect("declare attackers");
    runner.advance_until_stack_empty();

    let state = runner.state();
    let exiled = state
        .objects
        .values()
        .filter(|o| o.zone == Zone::Exile && o.owner == P0)
        .count();

    assert_eq!(
        exiled, 5,
        "X = the total power of the ATTACKING WRAITHS (2 + 3 = 5). 0 means the \
         batch collapsed to an empty set (the singleton extractor); 14 would mean \
         the non-Wraith bystander leaked in. Got {exiled}."
    );
}

/// MUTUAL RED CONTROL #1 — Witch-king is RED under Kylox's binding.
///
/// Same state (an attack batch, NO chain-published set). The batch reading gives
/// the right answer; the chain-set reading reduces an empty `tracked_object_sets`
/// to 0. This is the assertion that fails if the two sources are ever collapsed.
#[test]
fn witch_king_state_is_red_under_the_chain_set_binding() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let witch_king = scenario
        .add_creature_from_oracle(P0, "Witch-king, Sky Scourge", 2, 4, WITCH_KING)
        .with_subtypes(vec!["Wraith"])
        .id();
    let wraith = scenario
        .add_creature(P0, "Nazgul", 3, 3)
        .with_subtypes(vec!["Wraith"])
        .id();

    let mut runner = scenario.build();
    runner.advance_to_combat();
    runner
        .declare_attackers(&[
            (witch_king, AttackTarget::Player(P1)),
            (wraith, AttackTarget::Player(P1)),
        ])
        .expect("declare attackers");

    // Freeze the trigger context the way the resolver sees it mid-resolution.
    let mut state = runner.state().clone();
    state.current_trigger_events = vec![GameEvent::AttackersDeclared {
        attacker_ids: vec![witch_king, wraith],
        defending_player: P1,
        attacks: vec![],
        declaration_records: Vec::new(),
    }];

    let batch = QuantityExpr::Ref {
        qty: QuantityRef::PropertyAggregate(
            engine::types::ability::PropertyAggregate::new(
                AggregateFunction::Sum,
                ObjectProperty::Power,
                engine::types::ability::CardTypeSetSource::TrackedSet {
                    set: TrackedAnaphorSource::TriggeringBatch,
                    caused_by: None,
                    filter: None,
                },
            )
            .expect("statically valid property aggregate"),
        ),
    };
    let chain = QuantityExpr::Ref {
        qty: QuantityRef::PropertyAggregate(
            engine::types::ability::PropertyAggregate::new(
                AggregateFunction::Sum,
                ObjectProperty::Power,
                engine::types::ability::CardTypeSetSource::TrackedSet {
                    set: TrackedAnaphorSource::ChainSet,
                    caused_by: None,
                    filter: None,
                },
            )
            .expect("statically valid property aggregate"),
        ),
    };

    assert_eq!(
        resolve_quantity(&state, &batch, P0, witch_king),
        5,
        "the CORRECT (batch) reading is the attackers' total power, 2 + 3"
    );
    assert_eq!(
        resolve_quantity(&state, &chain, P0, witch_king),
        0,
        "Kylox's ChainSet binding applied to Witch-king's state is a silent 0 — \
         no clause published a set. This is the lying-green the clause-layer \
         disambiguation exists to prevent."
    );
}

/// MUTUAL RED CONTROL #2 — Kylox is RED under Witch-king's binding.
///
/// CR 701.21a + CR 608.2c: Kylox attacks ALONE and sacrifices two other
/// creatures (power 4 and 6). "their total power" = 10, read from the chain set
/// the sacrifice published. Under the triggering-batch reading the same state
/// yields the lone ATTACKER's power (Kylox's own 4) — a confident, plausible,
/// wrong number. That is precisely the falsification t78 recorded, pinned here.
#[test]
fn kylox_state_is_red_under_the_triggering_batch_binding() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let kylox = scenario
        .add_creature_from_oracle(P0, "Kylox, Visionary Inventor", 4, 4, KYLOX)
        .id();
    let fodder_a = scenario.add_creature(P0, "Fodder A", 4, 1).id();
    let fodder_b = scenario.add_creature(P0, "Fodder B", 6, 1).id();

    let mut runner = scenario.build();
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(kylox, AttackTarget::Player(P1))])
        .expect("declare attackers");

    // The state as the exile clause sees it: Kylox alone in the attack batch,
    // and the preceding sacrifice clause has published its two victims as the
    // chain tracked set.
    let mut state = runner.state().clone();
    state.current_trigger_events = vec![GameEvent::AttackersDeclared {
        attacker_ids: vec![kylox],
        defending_player: P1,
        attacks: vec![],
        declaration_records: Vec::new(),
    }];
    let set_id = engine::types::identifiers::TrackedSetId(state.next_tracked_set_id);
    state.next_tracked_set_id += 1;
    state
        .tracked_object_sets
        .insert(set_id, vec![fodder_a, fodder_b]);

    let chain = QuantityExpr::Ref {
        qty: QuantityRef::PropertyAggregate(
            engine::types::ability::PropertyAggregate::new(
                AggregateFunction::Sum,
                ObjectProperty::Power,
                engine::types::ability::CardTypeSetSource::TrackedSet {
                    set: TrackedAnaphorSource::ChainSet,
                    caused_by: None,
                    filter: None,
                },
            )
            .expect("statically valid property aggregate"),
        ),
    };
    let batch = QuantityExpr::Ref {
        qty: QuantityRef::PropertyAggregate(
            engine::types::ability::PropertyAggregate::new(
                AggregateFunction::Sum,
                ObjectProperty::Power,
                engine::types::ability::CardTypeSetSource::TrackedSet {
                    set: TrackedAnaphorSource::TriggeringBatch,
                    caused_by: None,
                    filter: None,
                },
            )
            .expect("statically valid property aggregate"),
        ),
    };

    assert_eq!(
        resolve_quantity(&state, &chain, P0, kylox),
        10,
        "the CORRECT (chain-set) reading is the SACRIFICED creatures' total power, 4 + 6"
    );
    assert_eq!(
        resolve_quantity(&state, &batch, P0, kylox),
        4,
        "Witch-king's TriggeringBatch binding applied to Kylox's state reads the \
         lone ATTACKER (Kylox's own power, 4) instead of the sacrificed 10 — a \
         plausible-looking wrong answer, which is why the leaf combinator may not \
         decide this axis"
    );
}

/// CR 608.2c: the chain-set publish must actually REACH the aggregate. Kylox's
/// `Sacrifice` only publishes its victims when the following sub-ability is
/// recognized as a tracked-set consumer (`next_sub_needs_tracked_set`), and the
/// consumer here is an `ExileTop` whose count is the aggregate. Without the
/// `ExileTop` arm on that predicate the sacrifice publishes nothing and the
/// aggregate reduces an empty set to 0 — green, and wrong.
#[test]
fn kylox_exile_top_is_recognized_as_a_tracked_set_consumer() {
    let parsed = parse_oracle_text(
        KYLOX,
        "Kylox, Visionary Inventor",
        &[],
        &["Creature".to_string()],
        &[],
    );
    let trigger = parsed
        .triggers
        .first()
        .expect("Kylox's attack trigger parses");
    let execute = trigger.execute.as_ref().expect("trigger has a body");

    assert!(
        matches!(*execute.effect, Effect::Sacrifice { .. }),
        "the publisher clause is the sacrifice, got {:?}",
        execute.effect
    );
    let sub = execute
        .sub_ability
        .as_ref()
        .expect("the exile clause chains off the sacrifice");
    assert!(
        matches!(*sub.effect, Effect::ExileTop { .. }),
        "the consumer clause is the ExileTop, got {:?}",
        sub.effect
    );
}

// ===========================================================================
// Unit 8c — agent-interposed "this way" anaphors narrow the tracked set (B-2..B-6)
// ===========================================================================
//
// "the toughness of the creature YOU sacrificed this way" (Danse Macabre) and
// "the total power of the creatures THEY exiled this way" (Grand Ossuary) name
// an agent between the noun and the participle. The agent threads into the
// tracked-set aggregate as a member `filter` so the resolver counts only that
// agent's members. B-1 (in-crate, `agent_interposed_anaphor_threads_member_filter`)
// pins the combinator; these pin the card-level parse, the runtime resolution,
// and the honest-red refusal of the sibling put-into-zone rows (unit 8e,
// Option 1: Danse's rows stay Unimplemented until Phase 7 wires them).

const DANSE: &str = "Each player sacrifices a nontoken creature of their choice. Roll a d20 and add the toughness of the creature you sacrificed this way.\n1—14 | Return a creature card put into a graveyard this way to the battlefield under your control.\n15+ | Return up to two creature cards put into graveyards this way to the battlefield under your control.";
const OSSUARY: &str = "Whenever a creature dies, its controller distributes a number of +1/+1 counters equal to its power among any number of target creatures they control.\nWhenever chaos ensues, each player exiles all creatures they control and creates X 1/1 green Saproling creature tokens, where X is the total power of the creatures they exiled this way. Then planeswalk.";
const DANSE_ROW_1: &str =
    "Return a creature card put into a graveyard this way to the battlefield under your control.";
const DANSE_ROW_2: &str =
    "Return up to two creature cards put into graveyards this way to the battlefield under your control.";
const ROW_BASELINE: &str = "Return a creature card to the battlefield under your control.";
const ROW_NOPARTICLE: &str =
    "Return a creature card put into a graveyard to the battlefield under your control.";

/// Every `TrackedSetAggregate` reachable in a card's parse, as
/// `(function, property, source, filter controller)`. The 8c extension of the
/// [`aggregates`] census above: the member filter's controller is part of the
/// binding, so it is part of the tally.
fn filtered_aggregates(
    name: &str,
    oracle: &str,
    types: &[String],
) -> Vec<(
    AggregateFunction,
    ObjectProperty,
    TrackedAnaphorSource,
    Option<ControllerRef>,
)> {
    let parsed = parse_oracle_text(oracle, name, &[], types, &[]);
    let json = serde_json::to_value(&parsed).expect("ParsedAbilities serializes");
    let mut out = Vec::new();
    collect(&json, &mut out);
    return out;

    fn collect(
        value: &serde_json::Value,
        out: &mut Vec<(
            AggregateFunction,
            ObjectProperty,
            TrackedAnaphorSource,
            Option<ControllerRef>,
        )>,
    ) {
        match value {
            serde_json::Value::Object(map) => {
                if map.get("type").and_then(|t| t.as_str()) == Some("PropertyAggregate") {
                    let qty: QuantityRef = serde_json::from_value(value.clone())
                        .expect("PropertyAggregate round-trips");
                    if let QuantityRef::PropertyAggregate(aggregate) = qty {
                        if let CardTypeSetSource::TrackedSet { set, filter, .. } =
                            aggregate.source()
                        {
                            let controller = filter.as_ref().and_then(|filter| match filter {
                                TargetFilter::Typed(typed) => typed.controller.clone(),
                                _ => None,
                            });
                            out.push((
                                aggregate.function(),
                                aggregate.property(),
                                *set,
                                controller,
                            ));
                        }
                    }
                }
                for v in map.values() {
                    collect(v, out);
                }
            }
            serde_json::Value::Array(items) => items.iter().for_each(|v| collect(v, out)),
            _ => {}
        }
    }
}

/// Count honest-failure markers in a card's parse.
fn unimplemented_count(name: &str, oracle: &str, types: &[String]) -> usize {
    let parsed = parse_oracle_text(oracle, name, &[], types, &[]);
    serde_json::to_string(&parsed)
        .expect("serializes")
        .matches("\"type\":\"Unimplemented\"")
        .count()
}

/// The single d20 `DieRolled` total from a cast's events. Reach-guarded:
/// exactly one d20 roll must have happened, so a missing roll fails here
/// instead of silently comparing against a default.
fn d20_total(events: &[GameEvent]) -> u8 {
    let mut totals = events.iter().filter_map(|event| match event {
        GameEvent::DieRolled {
            result: Some(total),
            sides: 20,
            ..
        } => Some(*total),
        _ => None,
    });
    let total = totals.next().expect("the cast must roll exactly one d20");
    assert!(
        totals.next().is_none(),
        "the cast must roll exactly one d20"
    );
    total
}

/// B-2 — RUNTIME cast drive. Danse Macabre resolves end to end: both players
/// sacrifice (mandatory fast path — one eligible creature each, so no prompt),
/// the d20 rolls with the filtered toughness bonus, and the winning die row
/// resolves to an honest no-op recorded in `unimplemented_oracle_ids`.
///
/// The lying green this guards: before the Option-1 refusal, row 1 parsed as
/// an over-broad "return any creature card from a graveyard" and would have
/// reanimated here. Post-refusal nothing leaves either graveyard.
#[test]
fn danse_macabre_cast_sacrifices_both_sides_and_honestly_no_ops_the_row() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let oak = scenario.add_creature(P0, "Oak", 2, 3).id();
    let elm = scenario.add_creature(P1, "Elm", 1, 7).id();
    let danse = scenario
        .add_spell_to_hand_from_oracle(P0, "Danse Macabre", false, DANSE)
        .id();
    let mut runner = scenario.build();

    let outcome = runner.cast(danse).resolve();

    // Both edicts paid: the sacrificed creatures rest in their owners' graveyards.
    outcome.assert_zone(&[oak, elm], Zone::Graveyard);
    // The sorcery itself resolved to its owner's graveyard (CR 608.2n).
    outcome.assert_zone(&[danse], Zone::Graveyard);
    // Nothing reanimated: no creature is on the battlefield.
    assert!(
        outcome.state().battlefield.is_empty(),
        "no row may reanimate while the put-into-zone anaphor is unwired; \
         battlefield holds {:?}",
        outcome.state().battlefield
    );
    // No `unimplemented_oracle_ids` assertion: `resolve_ability_chain` skips
    // `Effect::Unimplemented` nodes without recording the telemetry marker
    // (effects/mod.rs) — the marker only fires on the direct `resolve_effect`
    // path. That under-reporting is a pre-existing telemetry gap, out of scope
    // for unit 8c; the honest behavior pinned here is the no-op itself
    // (battlefield empty above) plus the clean halt below.
    // Clean halt: resolution finished with no dangling prompt.
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "expected a clean return to priority, got {:?}",
        outcome.final_waiting_for()
    );

    // The die modifier through the pipeline: Danse's post-modifier total
    // minus the natural from an identical control cast (same seed, same RNG
    // consumption — the sacrifices draw no randomness) must equal exactly
    // the filtered toughness bonus (oak's 3): not 0 (filter dropped the
    // bonus) and not 10 (filter ignored, elm's 7 leaked in).
    let danse_total = d20_total(outcome.events());
    let mut control_scenario = GameScenario::new();
    control_scenario.at_phase(Phase::PreCombatMain);
    control_scenario.add_creature(P0, "Oak", 2, 3);
    control_scenario.add_creature(P1, "Elm", 1, 7);
    let control_spell = control_scenario
        .add_spell_to_hand_from_oracle(P0, "Control Die", false, "Roll a d20.")
        .id();
    let mut control_runner = control_scenario.build();
    let control_outcome = control_runner.cast(control_spell).resolve();
    let natural = d20_total(control_outcome.events());
    assert_eq!(
        danse_total as i32 - natural as i32,
        3,
        "Danse's d20 modifier must be the filtered toughness bonus (oak's 3): \
         post-modifier total {danse_total}, control natural {natural}"
    );
}

/// B-2b — RUNTIME resumed path. Same die-modifier differential as B-2, but
/// each player controls TWO creatures so the sacrifice pauses for
/// `EffectZoneChoice` prompts (the driver answers in engine-listed order)
/// and completes through the resumption arms. Both of P0's creatures have
/// toughness 3, so the differential reads 3 whichever one is picked; a
/// publication miss on the resumed path reads 0 instead.
#[test]
fn danse_macabre_modifier_flows_when_sacrifice_pauses_for_choices() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let oak = scenario.add_creature(P0, "Oak", 2, 3).id();
    let pine = scenario.add_creature(P0, "Pine", 4, 3).id();
    let elm = scenario.add_creature(P1, "Elm", 1, 7).id();
    let ash = scenario.add_creature(P1, "Ash", 5, 5).id();
    let danse = scenario
        .add_spell_to_hand_from_oracle(P0, "Danse Macabre", false, DANSE)
        .id();
    let mut runner = scenario.build();
    // Declared picks, consumed in APNAP prompt order: oak (toughness 3, the
    // bonus the differential below must read) for P0, elm for P1.
    let outcome = runner.cast(danse).effect_zone(&[oak, elm]).resolve();

    // Reach guards: exactly one creature per side left the battlefield, so
    // the prompt path ran (the Auto fast path takes effect only at a single
    // eligible creature) and the roll below read a prompted completion.
    let state = outcome.state();
    let in_graveyard = |id| {
        state
            .objects
            .get(&id)
            .is_some_and(|obj| obj.zone == Zone::Graveyard)
    };
    let p0_sacrificed = [oak, pine].iter().filter(|id| in_graveyard(**id)).count();
    let p1_sacrificed = [elm, ash].iter().filter(|id| in_graveyard(**id)).count();
    assert_eq!(
        (p0_sacrificed, p1_sacrificed),
        (1, 1),
        "each side must sacrifice exactly one of its two creatures"
    );

    let danse_total = d20_total(outcome.events());
    let mut control_scenario = GameScenario::new();
    control_scenario.at_phase(Phase::PreCombatMain);
    control_scenario.add_creature(P0, "Oak", 2, 3);
    control_scenario.add_creature(P0, "Pine", 4, 3);
    control_scenario.add_creature(P1, "Elm", 1, 7);
    control_scenario.add_creature(P1, "Ash", 5, 5);
    let control_spell = control_scenario
        .add_spell_to_hand_from_oracle(P0, "Control Die", false, "Roll a d20.")
        .id();
    let mut control_runner = control_scenario.build();
    let control_outcome = control_runner.cast(control_spell).resolve();
    let natural = d20_total(control_outcome.events());
    assert_eq!(
        danse_total as i32 - natural as i32,
        3,
        "the resumed sacrifice must publish its members like the sync path: \
         post-modifier total {danse_total}, control natural {natural}"
    );
}

/// M1 — RUNTIME modified-toughness read (CR 608.2h). Oak (2/3) is pumped by
/// a parsed, resolved Giant Growth to 5/6 before Danse is cast; both players
/// then sacrifice through the real pipeline. Danse's modifier must read
/// Oak's departure (last-known) toughness 6 — not the retained base 3 the
/// zone-exit revert leaves live, and not 0 (unpublished) or 10 (Elm leaked).
#[test]
fn danse_macabre_modifier_reads_modified_toughness_from_departure_lki() {
    const GIANT_GROWTH: &str = "Target creature gets +3/+3 until end of turn.";
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let oak = scenario.add_creature(P0, "Oak", 2, 3).id();
    let elm = scenario.add_creature(P1, "Elm", 1, 7).id();
    let growth = scenario
        .add_spell_to_hand_from_oracle(P0, "Giant Growth", true, GIANT_GROWTH)
        .id();
    let danse = scenario
        .add_spell_to_hand_from_oracle(P0, "Danse Macabre", false, DANSE)
        .id();
    let mut runner = scenario.build();

    // Pump first, through the real cast pipeline.
    runner.cast(growth).target_objects(&[oak]).resolve();
    // Reach guard: Oak stands 5/6 before the sacrifice, so a 6 below reads
    // the modification and a 3 reads the reverted base.
    let pumped = runner.state().objects.get(&oak).expect("Oak stands");
    assert_eq!(
        (pumped.power, pumped.toughness),
        (Some(5), Some(6)),
        "Giant Growth must resolve to 5/6 before Danse is cast"
    );

    let outcome = runner.cast(danse).resolve();
    outcome.assert_zone(&[oak, elm], Zone::Graveyard);

    let danse_total = d20_total(outcome.events());
    let mut control_scenario = GameScenario::new();
    control_scenario.at_phase(Phase::PreCombatMain);
    control_scenario.add_creature(P0, "Oak", 2, 3);
    control_scenario.add_creature(P1, "Elm", 1, 7);
    let control_spell = control_scenario
        .add_spell_to_hand_from_oracle(P0, "Control Die", false, "Roll a d20.")
        .id();
    let mut control_runner = control_scenario.build();
    let control_outcome = control_runner.cast(control_spell).resolve();
    let natural = d20_total(control_outcome.events());
    assert_eq!(
        danse_total as i32 - natural as i32,
        6,
        "Danse's d20 modifier must read Oak's last-known toughness 6: \
         post-modifier total {danse_total}, control natural {natural}"
    );
}

/// M1 unchanged control — the other side's modification must not leak, and
/// an unmodified sacrifice keeps its base read. Elm (1/7) is pumped to 4/10
/// while Oak stays 2/3: the modifier must be exactly Oak's 3.
#[test]
fn danse_macabre_modifier_ignores_other_players_modified_toughness() {
    const GIANT_GROWTH: &str = "Target creature gets +3/+3 until end of turn.";
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let oak = scenario.add_creature(P0, "Oak", 2, 3).id();
    let elm = scenario.add_creature(P1, "Elm", 1, 7).id();
    let growth = scenario
        .add_spell_to_hand_from_oracle(P0, "Giant Growth", true, GIANT_GROWTH)
        .id();
    let danse = scenario
        .add_spell_to_hand_from_oracle(P0, "Danse Macabre", false, DANSE)
        .id();
    let mut runner = scenario.build();

    runner.cast(growth).target_objects(&[elm]).resolve();
    let pumped = runner.state().objects.get(&elm).expect("Elm stands");
    assert_eq!(
        (pumped.power, pumped.toughness),
        (Some(4), Some(10)),
        "Giant Growth must resolve to 4/10 before Danse is cast"
    );

    let outcome = runner.cast(danse).resolve();
    outcome.assert_zone(&[oak, elm], Zone::Graveyard);

    let danse_total = d20_total(outcome.events());
    let mut control_scenario = GameScenario::new();
    control_scenario.at_phase(Phase::PreCombatMain);
    control_scenario.add_creature(P0, "Oak", 2, 3);
    control_scenario.add_creature(P1, "Elm", 1, 7);
    let control_spell = control_scenario
        .add_spell_to_hand_from_oracle(P0, "Control Die", false, "Roll a d20.")
        .id();
    let mut control_runner = control_scenario.build();
    let control_outcome = control_runner.cast(control_spell).resolve();
    let natural = d20_total(control_outcome.events());
    assert_eq!(
        danse_total as i32 - natural as i32,
        3,
        "Danse's d20 modifier must stay Oak's unmodified 3: \
         post-modifier total {danse_total}, control natural {natural}"
    );
}

/// B-2c — RUNTIME replacement-paused automatic path (F1). One eligible
/// creature per side, so the sacrifice takes the all-automatic fast path with
/// no `EffectZoneChoice` prompt — but two competing graveyard replacements
/// (Rest in Peace + Leyline of the Void) force the simultaneous sacrifice to
/// pause mid-batch on a CR 616.1 order choice. The detached roll tail must
/// ride the continuation across that pause and resume exactly once: the cast
/// rolls exactly one d20, and the differential against an identical control
/// cast reads exactly the caster's sacrificed toughness (oak's 3) — not 0
/// (tail dropped / members unpublished) and not 10 (elm's 7 leaked in).
///
/// The lying green this guards: the pre-fix automatic arm dropped
/// `after_scope` on `PausedForReplacement`, so the cast finished the
/// sacrifices and never rolled.
#[test]
fn danse_macabre_roll_tail_survives_automatic_sacrifice_replacement_pause() {
    const RIP: &str =
        "If a card or token would be put into a graveyard from anywhere, exile it instead.";
    const LEYLINE: &str =
        "If a card or token would be put into an opponent's graveyard from anywhere, exile it instead.";

    // Reach: without a scripted answer the driver must stall on the CR 616.1
    // order choice — proving the automatic path really pauses rather than
    // completing synchronously.
    let mut reach = GameScenario::new();
    reach.at_phase(Phase::PreCombatMain);
    reach.add_creature(P0, "Oak", 2, 3);
    reach.add_creature(P1, "Elm", 1, 7);
    reach
        .add_creature(P0, "Rest in Peace", 0, 0)
        .as_enchantment()
        .from_oracle_text(RIP);
    reach
        .add_creature(P0, "Leyline of the Void", 0, 0)
        .as_enchantment()
        .from_oracle_text(LEYLINE);
    let reach_danse = reach
        .add_spell_to_hand_from_oracle(P0, "Danse Macabre", false, DANSE)
        .id();
    let mut reach_runner = reach.build();
    let stalled = reach_runner.cast(reach_danse).resolve();
    assert!(
        matches!(
            stalled.final_waiting_for(),
            WaitingFor::ReplacementChoice { .. }
        ),
        "reach: the automatic sacrifice must pause on the replacement order choice, got {:?}",
        stalled.final_waiting_for()
    );

    // Behavior: the identical board with the order choice answered — the
    // pause resolves, both sacrifices complete, and the roll tail resumes.
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let oak = scenario.add_creature(P0, "Oak", 2, 3).id();
    let elm = scenario.add_creature(P1, "Elm", 1, 7).id();
    let rip = scenario
        .add_creature(P0, "Rest in Peace", 0, 0)
        .as_enchantment()
        .from_oracle_text(RIP)
        .id();
    let leyline = scenario
        .add_creature(P0, "Leyline of the Void", 0, 0)
        .as_enchantment()
        .from_oracle_text(LEYLINE)
        .id();
    let danse = scenario
        .add_spell_to_hand_from_oracle(P0, "Danse Macabre", false, DANSE)
        .id();
    let mut runner = scenario.build();

    let outcome = runner.cast(danse).replacement_choice(0).resolve();

    // Clean halt: resolution finished with no dangling prompt.
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "expected a clean return to priority, got {:?}",
        outcome.final_waiting_for()
    );
    // Both edicts paid through the replacements: the sacrificed creatures
    // were exiled instead of reaching a graveyard.
    outcome.assert_zone(&[oak, elm], Zone::Exile);
    // The resolving sorcery is itself a card put into a graveyard (CR
    // 608.2n), so Rest in Peace exiles it too.
    outcome.assert_zone(&[danse], Zone::Exile);
    // Nothing reanimated: only the two replacement enchantments remain.
    outcome.assert_zone(&[rip, leyline], Zone::Battlefield);
    assert_eq!(
        outcome.state().battlefield.len(),
        2,
        "no row may reanimate while the put-into-zone anaphor is unwired"
    );

    // Exactly one d20 (the resumed tail rolls once) with exactly the
    // caster-side filtered bonus: oak's 3, read through LKI from exile.
    let danse_total = d20_total(outcome.events());
    let mut control_scenario = GameScenario::new();
    control_scenario.at_phase(Phase::PreCombatMain);
    control_scenario.add_creature(P0, "Oak", 2, 3);
    control_scenario.add_creature(P1, "Elm", 1, 7);
    control_scenario
        .add_creature(P0, "Rest in Peace", 0, 0)
        .as_enchantment()
        .from_oracle_text(RIP);
    control_scenario
        .add_creature(P0, "Leyline of the Void", 0, 0)
        .as_enchantment()
        .from_oracle_text(LEYLINE);
    let control_spell = control_scenario
        .add_spell_to_hand_from_oracle(P0, "Control Die", false, "Roll a d20.")
        .id();
    let mut control_runner = control_scenario.build();
    let control_outcome = control_runner.cast(control_spell).resolve();
    let natural = d20_total(control_outcome.events());
    assert_eq!(
        danse_total as i32 - natural as i32,
        3,
        "the replacement-paused sacrifice must publish its members and resume the roll: \
         post-modifier total {danse_total}, control natural {natural}"
    );
}

/// B-3 — SHAPE. The interposed agent is part of the card-level binding, not
/// just the combinator: Danse's "you" and Ossuary's "they" each narrow the
/// chain set to that agent's members.
#[test]
fn agent_interposed_anaphor_binds_chain_set_with_member_filter_at_card_level() {
    let sorcery = vec!["Sorcery".to_string()];
    assert_eq!(
        filtered_aggregates("Danse Macabre", DANSE, &sorcery),
        vec![(
            AggregateFunction::Sum,
            ObjectProperty::Toughness,
            TrackedAnaphorSource::ChainSet,
            Some(ControllerRef::You)
        )],
        "Danse's roll bonus is the toughness of the creature YOU sacrificed — \
         the chain set narrowed to the caster's members"
    );

    // Grand Ossuary's "the total power of the creatures they exiled this way"
    // is the `they` leaf's measured corpus fragment, but no aggregate reaches
    // the census (observed: the empty-census assertion below passes on the
    // rebased head). Two red nodes remain, both honest: the dies trigger's
    // distribute sentence — "a number of +1/+1 counters equal to its power"
    // is not a count grammar either tree accepts (`parse_number` excludes
    // the "a/an" article in both; verified by source comparison) — pinned
    // below as `unparsed_quantity`; and the chaos trigger's token sub-ability
    // ("creates X 1/1 green Saproling ... where X is ..."), pinned below with
    // its full-sentence description. Note the rebase delta is taxonomy-only:
    // on our tree that node was `Unimplemented { name: "creates" }`; main's
    // evolved row-body vocabulary renamed it to `unparsed_verb_arguments`
    // (same vocabulary as the Danse rows above) while the clause stays red —
    // verified by the full-tree dump, which shows the chaos trigger's own
    // `ChangeZoneAll` green with the token red as its sequential sub-ability.
    // B-1 pins the `they` combinator shape and B-5 pins its runtime scoping
    // directly.
    let plane = vec!["Plane".to_string()];
    assert_eq!(
        filtered_aggregates("Grand Ossuary", OSSUARY, &plane),
        Vec::new(),
        "Ossuary contributes no supported aggregate while its token clause is red"
    );
    // Reach-guards so the empty census above is not vacuous: the exile
    // preamble parses supported and exactly two red nodes remain — the
    // distribute sentence's quantity and the token sub-ability — not a total
    // parse collapse.
    let ossuary_json = serde_json::to_string(&parse_oracle_text(
        OSSUARY,
        "Grand Ossuary",
        &[],
        &plane,
        &[],
    ))
    .expect("serializes");
    assert!(
        ossuary_json.contains("\"type\":\"ChangeZoneAll\""),
        "the exile preamble must stay supported, got {ossuary_json}"
    );
    assert!(
        ossuary_json.contains("\"name\":\"unparsed_quantity\"")
            && ossuary_json.contains("distributes a number of"),
        "the remaining red node must be the distribute sentence's quantity, \
         got {ossuary_json}"
    );
    assert!(
        ossuary_json.contains("creates X 1/1 green Saproling"),
        "the token sub-ability must stay red keeping its full sentence, got {ossuary_json}"
    );
    assert_eq!(
        ossuary_json.matches("\"type\":\"Unimplemented\"").count(),
        2,
        "exactly two red nodes must remain, got {ossuary_json}"
    );
}

/// B-4 — RESOLVER + MUTUAL DESTRUCTION. A chain set holding the caster's own
/// sacrificed 1/1, the opponent's own 1/7, and a 1/2 the caster controlled
/// but did not own (read from LKI — all members have left the battlefield)
/// resolves 3 under Danse's `You` filter and 10 unfiltered. Every wrong
/// reading differs: dropping the filter reads 10 (over-count), and either
/// owner-keyed reading (1 or 8) misses the control-but-not-own arm, which is
/// exactly the arm that separates a controller predicate from an owner
/// predicate.
#[test]
fn filtered_tracked_set_aggregate_counts_only_the_named_agents_members_via_lki() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let oak = scenario.add_creature_to_graveyard(P0, "Oak", 1, 1).id();
    let elm = scenario.add_creature_to_graveyard(P1, "Elm", 1, 7).id();
    // CR 701.21a: sacrificed from under P0's control, so it moved to its
    // owner's (P1's) graveyard — owner and controller disagree by construction.
    let loaned = scenario.add_creature_to_graveyard(P1, "Loaned", 1, 2).id();
    let runner = scenario.build();

    // LKI as the sacrifice publisher would have captured it at zone-change time.
    let mut state = runner.state().clone();
    for oid in [oak, elm] {
        let snapshot = state
            .objects
            .get(&oid)
            .expect("tracked member exists")
            .snapshot_public_characteristics();
        state.lki_cache.insert(oid, snapshot);
    }
    let mut loaned_snapshot = state
        .objects
        .get(&loaned)
        .expect("tracked member exists")
        .snapshot_public_characteristics();
    loaned_snapshot.controller = P0;
    state.lki_cache.insert(loaned, loaned_snapshot);
    let set_id = engine::types::identifiers::TrackedSetId(state.next_tracked_set_id);
    state.next_tracked_set_id += 1;
    state
        .tracked_object_sets
        .insert(set_id, vec![oak, elm, loaned]);

    let aggregate = |filter: Option<TargetFilter>| QuantityExpr::Ref {
        qty: QuantityRef::PropertyAggregate(
            engine::types::ability::PropertyAggregate::new(
                AggregateFunction::Sum,
                ObjectProperty::Toughness,
                CardTypeSetSource::TrackedSet {
                    set: TrackedAnaphorSource::ChainSet,
                    caused_by: None,
                    filter,
                },
            )
            .expect("statically valid property aggregate"),
        ),
    };
    let filtered = aggregate(Some(TargetFilter::Typed(
        TypedFilter::card().controller(ControllerRef::You),
    )));
    let unfiltered = aggregate(None);

    assert_eq!(
        resolve_quantity(&state, &filtered, P0, oak),
        3,
        "Danse's filter admits the caster's own 1/1 plus the controlled-but-not-owned 1/2"
    );
    assert_eq!(
        resolve_quantity(&state, &unfiltered, P0, oak),
        10,
        "without the member filter every member's toughness leaks in — the \
         over-count that makes the restriction load-bearing, not decorative"
    );

    // The no-LKI fallback arm: with the snapshots gone (the cache clears on
    // phase/step transitions), the loaned member resolves from its live
    // graveyard object, where CR 108.4a substitutes its owner (P1) for the
    // controller query — so it drops out and the filter reads 1, not 3. That
    // drop is the specified degradation, and it proves the LKI branch (not
    // the live-object branch) carries the control-but-not-own arm.
    state.lki_cache.clear();
    assert_eq!(
        resolve_quantity(&state, &filtered, P0, oak),
        1,
        "without LKI the control-but-not-own arm is invisible to the filter \
         (CR 108.4a owner substitution)"
    );
    assert_eq!(
        resolve_quantity(&state, &unfiltered, P0, oak),
        10,
        "unfiltered over live objects without LKI still over-counts to 10"
    );
}

/// PR-C F3 — RESOLVER. A battlefield target that leaves mid-resolution is
/// read from its departure LKI, not its retained live entry (CR 608.2h + CR
/// 400.7): the 2/2 buffed to 5/6 departs with 5/6 LKI while the retained
/// graveyard entry reverts to base 2/2 — the Power aggregate over the pinned
/// target must read 5, not 2. The buff values are fixture-set; the LKI
/// capture, incarnation bump, and base revert below all run through the real
/// zone pipeline.
#[test]
fn targeted_objects_power_reads_departure_lki_after_battlefield_exit() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let brute = scenario.add_creature(P0, "Brute", 2, 2).id();
    let source = scenario.add_creature(P0, "Source", 1, 1).id();
    let runner = scenario.build();
    let mut state = runner.state().clone();

    // Production-shaped ability shell: Doom Blade's real targeting sentence.
    let parsed = parse_oracle_text(
        "Destroy target nonblack creature.",
        "Doom Blade",
        &[],
        &["Instant".to_string()],
        &[],
    );
    let def = parsed
        .abilities
        .first()
        .expect("Doom Blade parses an ability");
    let mut ability = engine::game::ability_utils::build_resolved_from_def_with_targets(
        def,
        source,
        P0,
        vec![TargetRef::Object(brute)],
    );
    ability.announce_target_pins(&state);
    assert!(
        ability
            .target_pin_at(0)
            .is_some_and(|pin| pin.is_current(&state)),
        "reach guard: the pin names the live battlefield incarnation"
    );
    {
        let obj = state.objects.get_mut(&brute).expect("brute exists");
        obj.power = Some(5);
        obj.toughness = Some(6);
    }
    let mut events = Vec::new();
    engine::game::zones::move_to_zone(&mut state, brute, Zone::Graveyard, &mut events);

    // Reach guards: the exit staled the pin, captured 5/6 departure LKI under
    // the pinned incarnation, and reverted the retained entry to base 2/2.
    let pin = ability.target_pin_at(0).expect("pin announced");
    assert!(
        !pin.is_current(&state),
        "reach guard: the battlefield exit must stale the pin"
    );
    let live = state.objects.get(&brute).expect("retained entry exists");
    assert_eq!(
        (live.power, live.toughness),
        (Some(2), Some(2)),
        "reach guard: the retained entry reverts to base 2/2"
    );
    let history = state
        .lki_by_incarnation
        .get(&brute)
        .and_then(|history| history.get(&pin.incarnation))
        .expect("departure LKI captured under the pinned incarnation");
    assert_eq!(
        (history.power, history.toughness),
        (Some(5), Some(6)),
        "reach guard: departure LKI captures the 5/6 body"
    );

    let power = QuantityExpr::Ref {
        qty: QuantityRef::PropertyAggregate(
            engine::types::ability::PropertyAggregate::new(
                AggregateFunction::Sum,
                ObjectProperty::Power,
                CardTypeSetSource::TargetedObjects { slot: None },
            )
            .expect("statically valid property aggregate"),
        ),
    };
    assert_eq!(
        resolve_quantity_with_targets(&state, &power, &ability),
        5,
        "a departed battlefield target reads its departure body (5), not the retained base entry (2)"
    );
}

/// PR-C F3 — RESOLVER preservation. A card targeted in its graveyard that
/// never leaves is read live (Song of Inspiration's shape): the pinned,
/// current incarnation resolves through the live object, not LKI.
#[test]
fn targeted_objects_toughness_reads_live_for_unmoved_graveyard_target() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let ghoul = scenario.add_creature_to_graveyard(P0, "Ghoul", 3, 4).id();
    let source = scenario.add_creature(P0, "Source", 1, 1).id();
    let runner = scenario.build();
    let state = runner.state().clone();

    let parsed = parse_oracle_text(
        "Destroy target nonblack creature.",
        "Doom Blade",
        &[],
        &["Instant".to_string()],
        &[],
    );
    let def = parsed
        .abilities
        .first()
        .expect("Doom Blade parses an ability");
    let mut ability = engine::game::ability_utils::build_resolved_from_def_with_targets(
        def,
        source,
        P0,
        vec![TargetRef::Object(ghoul)],
    );
    ability.announce_target_pins(&state);
    assert!(
        ability
            .target_pin_at(0)
            .is_some_and(|pin| pin.is_current(&state)),
        "reach guard: the pin names the live graveyard incarnation"
    );

    let toughness = QuantityExpr::Ref {
        qty: QuantityRef::PropertyAggregate(
            engine::types::ability::PropertyAggregate::new(
                AggregateFunction::Sum,
                ObjectProperty::Toughness,
                CardTypeSetSource::TargetedObjects { slot: None },
            )
            .expect("statically valid property aggregate"),
        ),
    };
    assert_eq!(
        resolve_quantity_with_targets(&state, &toughness, &ability),
        4,
        "an unmoved graveyard target reads its live body (4)"
    );
}

/// PR-C F5 — IDENTIFICATION CONTRACT. A roll modifier pairing the Danse
/// aggregate (toughness over the You-filtered chain set: oak's 3) with a
/// par-added filter-bearing `TrackedSet` aggregate (toughness over the
/// P1-filtered chain set: elm's 7) keeps both legs: the summed modifier is
/// 10. This pins the parallel-aggregate modifier shape every F5 visitor
/// must keep supporting — a member filter invisible to discovery, rewrite,
/// chosen-target, recipient, or granter classification would silently drop
/// or misbind its leg.
#[test]
fn roll_modifier_with_two_tracked_set_aggregates_keeps_both_legs() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let oak = scenario.add_creature_to_graveyard(P0, "Oak", 2, 3).id();
    let elm = scenario.add_creature_to_graveyard(P1, "Elm", 1, 7).id();
    let runner = scenario.build();

    let mut state = runner.state().clone();
    for oid in [oak, elm] {
        let snapshot = state
            .objects
            .get(&oid)
            .expect("tracked member exists")
            .snapshot_public_characteristics();
        state.lki_cache.insert(oid, snapshot);
    }
    let set_id = engine::types::identifiers::TrackedSetId(state.next_tracked_set_id);
    state.next_tracked_set_id += 1;
    state.tracked_object_sets.insert(set_id, vec![oak, elm]);

    let aggregate = |controller: ControllerRef| QuantityExpr::Ref {
        qty: QuantityRef::PropertyAggregate(
            engine::types::ability::PropertyAggregate::new(
                AggregateFunction::Sum,
                ObjectProperty::Toughness,
                CardTypeSetSource::TrackedSet {
                    set: engine::types::ability::TrackedAnaphorSource::ChainSet,
                    caused_by: None,
                    filter: Some(TargetFilter::Typed(TypedFilter {
                        type_filters: vec![],
                        controller: Some(controller),
                        properties: vec![],
                    })),
                },
            )
            .expect("statically valid property aggregate"),
        ),
    };
    let modifier = QuantityExpr::Sum {
        exprs: vec![
            aggregate(ControllerRef::You),
            aggregate(ControllerRef::SpecificPlayer { id: P1 }),
        ],
    };
    assert_eq!(
        resolve_quantity(&state, &modifier, P0, oak),
        10,
        "the Danse leg reads oak's 3 and the par-added leg reads elm's 7"
    );
}

/// B-5 — RESOLVER. Ossuary's `they` (ScopedPlayer) follows the resolution
/// scope: scoped to P1 it counts only P1's exiled member; with no scope
/// active it falls back to the ability controller.
#[test]
fn scoped_player_member_filter_follows_the_resolution_scope() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let oak = scenario.add_creature_to_graveyard(P0, "Oak", 2, 3).id();
    let elm = scenario.add_creature_to_graveyard(P1, "Elm", 1, 7).id();
    let runner = scenario.build();

    let mut state = runner.state().clone();
    for oid in [oak, elm] {
        let snapshot = state
            .objects
            .get(&oid)
            .expect("tracked member exists")
            .snapshot_public_characteristics();
        state.lki_cache.insert(oid, snapshot);
    }
    let set_id = engine::types::identifiers::TrackedSetId(state.next_tracked_set_id);
    state.next_tracked_set_id += 1;
    state.tracked_object_sets.insert(set_id, vec![oak, elm]);

    let expr = QuantityExpr::Ref {
        qty: QuantityRef::PropertyAggregate(
            engine::types::ability::PropertyAggregate::new(
                AggregateFunction::Sum,
                ObjectProperty::Toughness,
                CardTypeSetSource::TrackedSet {
                    set: TrackedAnaphorSource::ChainSet,
                    caused_by: None,
                    filter: Some(TargetFilter::Typed(
                        TypedFilter::card().controller(ControllerRef::ScopedPlayer),
                    )),
                },
            )
            .expect("statically valid property aggregate"),
        ),
    };
    let ctx_with = |scoped_player| QuantityContext {
        entering: None,
        source: oak,
        trigger_source: None,
        recipient: None,
        scoped_player,
        damage_source: None,
        spell: None,
        event_amount: None,
        granting_object: None,
    };

    assert_eq!(
        resolve_quantity_with_ctx(&state, &expr, P0, ctx_with(Some(P1))),
        7,
        "scoped to P1, only P1's exiled member (toughness 7) counts"
    );
    assert_eq!(
        resolve_quantity_with_ctx(&state, &expr, P0, ctx_with(None)),
        3,
        "with no scope active the filter falls back to the controller's members"
    );
}

/// B-6 — SHAPE. Danse's put-into-zone rows refuse honestly (unit 8e, Option 1)
/// while the neighboring shapes keep parsing: the particle-less and bare
/// forms still reach `ChangeZone`, and Danse's own sacrifice + roll preamble
/// stays supported — only the two rows are red.
#[test]
fn danse_put_into_zone_rows_refuse_honestly_while_neighbors_parse() {
    let sorcery = vec!["Sorcery".to_string()];
    // The two FULL bodies — singular "a graveyard" and "up to two" plural
    // "graveyards" — both surface as Unimplemented.
    assert_eq!(
        unimplemented_count("Probe Row", DANSE_ROW_1, &sorcery),
        1,
        "row 1 names a put-into-zone set the engine cannot execute"
    );
    assert_eq!(
        unimplemented_count("Probe Row", DANSE_ROW_2, &sorcery),
        1,
        "row 2 (up to two, plural graveyards) refuses the same way"
    );

    // Reach-guards against over-refusal: without "this way" both shapes parse
    // to an honest ChangeZone with no red marker.
    for (label, text) in [("baseline", ROW_BASELINE), ("no-particle", ROW_NOPARTICLE)] {
        let parsed = parse_oracle_text(text, "Probe Row", &[], &sorcery, &[]);
        let json = serde_json::to_string(&parsed).expect("serializes");
        assert!(
            json.contains("\"type\":\"ChangeZone\""),
            "{label} must still reach ChangeZone, got {json}"
        );
        assert!(
            !json.contains("Unimplemented"),
            "{label} must stay fully supported, got {json}"
        );
    }

    // Danse itself: the sacrifice preamble and the d20 roll stay supported —
    // exactly the two die rows are red, no more.
    let parsed = parse_oracle_text(DANSE, "Danse Macabre", &[], &sorcery, &[]);
    let json = serde_json::to_string(&parsed).expect("serializes");
    assert!(
        json.contains("\"type\":\"Sacrifice\""),
        "the edict preamble must stay supported, got {json}"
    );
    assert!(
        json.contains("\"type\":\"RollDie\""),
        "the d20 roll must stay supported, got {json}"
    );
    assert_eq!(
        json.matches("\"type\":\"Unimplemented\"").count(),
        2,
        "exactly the two die rows may be red, got {json}"
    );

    // The results table itself: both rows keep their printed ranges and both
    // bodies refuse as honest-red `Unimplemented`. Main's evolved row-body
    // taxonomy names the marker "unparsed_verb_arguments" (verb recognized,
    // arguments not modeled) where our tree said "return" — equally red, so
    // the pin follows main's vocabulary while keeping the full-sentence
    // description assertion. The participle axis that would bind them
    // (unit 8e) is DEFERRED(phase 7), so refusal — not an over-broad
    // reanimation — is the honest shape.
    fn roll_results(def: &AbilityDefinition) -> Option<&Vec<DieResultBranch>> {
        if let Effect::RollDie { results, .. } = &*def.effect {
            return Some(results);
        }
        def.sub_ability.as_deref().and_then(roll_results)
    }
    let results = parsed
        .abilities
        .iter()
        .find_map(roll_results)
        .expect("Danse must carry a RollDie results table");
    assert_eq!(
        results
            .iter()
            .map(|row| (row.min, row.max))
            .collect::<Vec<_>>(),
        vec![(1, 14), (15, 255)],
        "both die rows keep their printed ranges"
    );
    for (index, row) in results.iter().enumerate() {
        assert!(
            matches!(&*row.effect.effect, Effect::Unimplemented { name, description, .. }
            if name == "unparsed_verb_arguments"
                && description.as_deref().is_some_and(|d| {
                    d.contains("Return")
                        && d.contains("put into")
                        && d.contains("graveyard")
                        && d.contains("battlefield under your control")
                })),
            "row {index} body must refuse as honest-red Unimplemented keeping \
             the full return sentence, got {:?}",
            row.effect.effect
        );
    }
}
