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
use engine::game::quantity::resolve_quantity;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::parser::parse_oracle_text;
use engine::types::ability::{
    AggregateFunction, Effect, ObjectProperty, QuantityExpr, QuantityRef, TrackedAnaphorSource,
};
use engine::types::events::GameEvent;
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
                if map.get("type").and_then(|t| t.as_str()) == Some("TrackedSetAggregate") {
                    let qty: QuantityRef = serde_json::from_value(value.clone())
                        .expect("TrackedSetAggregate round-trips");
                    if let QuantityRef::TrackedSetAggregate {
                        function,
                        property,
                        source,
                    } = qty
                    {
                        out.push((function, property, source));
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
    }];

    let batch = QuantityExpr::Ref {
        qty: QuantityRef::TrackedSetAggregate {
            function: AggregateFunction::Sum,
            property: ObjectProperty::Power,
            source: TrackedAnaphorSource::TriggeringBatch,
        },
    };
    let chain = QuantityExpr::Ref {
        qty: QuantityRef::TrackedSetAggregate {
            function: AggregateFunction::Sum,
            property: ObjectProperty::Power,
            source: TrackedAnaphorSource::ChainSet,
        },
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
    }];
    let set_id = engine::types::identifiers::TrackedSetId(state.next_tracked_set_id);
    state.next_tracked_set_id += 1;
    state
        .tracked_object_sets
        .insert(set_id, vec![fodder_a, fodder_b]);

    let chain = QuantityExpr::Ref {
        qty: QuantityRef::TrackedSetAggregate {
            function: AggregateFunction::Sum,
            property: ObjectProperty::Power,
            source: TrackedAnaphorSource::ChainSet,
        },
    };
    let batch = QuantityExpr::Ref {
        qty: QuantityRef::TrackedSetAggregate {
            function: AggregateFunction::Sum,
            property: ObjectProperty::Power,
            source: TrackedAnaphorSource::TriggeringBatch,
        },
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
