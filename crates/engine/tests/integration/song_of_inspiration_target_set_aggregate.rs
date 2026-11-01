//! PR C (Unit 8d) tests: aggregates over the ability's chosen targets.
//!
//! Gate C-0 runs FIRST and before any PR C implementation code (plan J.4.1,
//! M1 sequencing): it confirms the rejected `ParentTarget` carrier reads 0
//! for graveyard targets, with a positive reach-guard proving the instrument
//! fired. If this probe ever reads the correct total instead, plan G.2's
//! rejection is falsified — PR C re-plans and PR B stands unchanged.

use engine::game::game_object::ObjectIncarnationRefExt;
use engine::game::quantity::resolve_quantity;
use engine::game::scenario::{GameScenario, P0};
use engine::parser::parse_oracle_text;
use engine::types::ability::{
    AbilityDefinition, AbilityKind, AggregateFunction, CardTypeSetSource, ChosenTargetSlots,
    ControllerRef, CountScope, DieResultBranch, DieRollIgnoreRule, DieRollModifier, Effect,
    ObjectProperty, PropertyAggregate, QuantityExpr, QuantityRef, ResolvedAbility, TargetFilter,
    TargetRef, TrackedAnaphorSource, TypeFilter, TypedFilter, ZoneRef,
};
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::{GameState, TrackedMemberOrigin, WaitingFor};
use engine::types::identifiers::{ObjectId, TrackedSetId};
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::resolution::{CapturedDeclarationPayload, PendingDieRoll};
use engine::types::zones::Zone;

/// Song of Inspiration, verbatim Oracle text (the row separator is an en
/// dash, matching the corpus export).
const SONG: &str = "Choose up to two target permanent cards in your graveyard. Roll a d20 and add the total mana value of those cards.\n1\u{2013}14 | Return those cards to your hand.\n15+ | Return those cards to your hand. You gain life equal to their total mana value.";

/// C-0 GATE (binding): `Objects { filter: ParentTarget }` over two graveyard
/// permanents (MV 3 + 4) must read 0 — the candidate universe has no
/// `ParentTarget` arm, so the population falls through to a zone census that
/// cannot see the ability's chosen targets (CR 608.2c + CR 115.1: the later
/// instruction names what the earlier target-selection clause chose, and no
/// existing carrier expresses that population).
#[test]
fn c0_parent_target_carrier_reads_zero_for_graveyard_targets() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let three = scenario
        .add_creature_to_graveyard(P0, "Oak", 2, 3)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![],
            generic: 3,
        })
        .id();
    scenario
        .add_creature_to_graveyard(P0, "Elm", 2, 4)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![],
            generic: 4,
        })
        .id();
    let runner = scenario.build();
    let state = runner.state();

    // Positive reach-guard: the same members total 7 through the zone carrier,
    // so a 0 below cannot pass vacuously (no members, no MV, dead instrument).
    let zone_aggregate = QuantityExpr::Ref {
        qty: QuantityRef::PropertyAggregate(
            engine::types::ability::PropertyAggregate::new(
                AggregateFunction::Sum,
                ObjectProperty::ManaValue,
                CardTypeSetSource::Zone {
                    zone: ZoneRef::Graveyard,
                    scope: CountScope::Controller,
                },
            )
            .expect("statically valid property aggregate"),
        ),
    };
    assert_eq!(
        resolve_quantity(state, &zone_aggregate, P0, three),
        7,
        "reach-guard: the two graveyard members total MV 7 through the zone carrier"
    );

    // The probe proper: the rejected carrier over the same members.
    let parent_aggregate = QuantityExpr::Ref {
        qty: QuantityRef::PropertyAggregate(
            engine::types::ability::PropertyAggregate::new(
                AggregateFunction::Sum,
                ObjectProperty::ManaValue,
                CardTypeSetSource::Objects {
                    filter: TargetFilter::ParentTarget,
                },
            )
            .expect("statically valid property aggregate"),
        ),
    };
    assert_eq!(
        resolve_quantity(state, &parent_aggregate, P0, three),
        0,
        "C-0: Objects+ParentTarget has no graveyard population arm and must read 0; \
         a correct total here falsifies G.2 and re-plans PR C"
    );
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

fn mv_cost(generic: u32) -> ManaCost {
    ManaCost::Cost {
        shards: vec![],
        generic,
    }
}

/// Stage Oak (MV 3) and Elm (MV 4) in P0's graveyard with Song in hand.
/// Returns the runner-ready scenario plus the staged ids.
fn song_scenario() -> (GameScenario, ObjectId, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let oak = scenario
        .add_creature_to_graveyard(P0, "Oak", 2, 3)
        .with_mana_cost(mv_cost(3))
        .id();
    let elm = scenario
        .add_creature_to_graveyard(P0, "Elm", 2, 4)
        .with_mana_cost(mv_cost(4))
        .id();
    let song = scenario
        .add_spell_to_hand_from_oracle(P0, "Song of Inspiration", true, SONG)
        .id();
    (scenario, oak, elm, song)
}

/// A parallel "Roll a d20." control over an identical graveyard, so the
/// differential isolates Song's modifier (same seed, same RNG consumption).
fn natural_d20() -> u8 {
    let mut control = GameScenario::new();
    control.at_phase(Phase::PreCombatMain);
    control
        .add_creature_to_graveyard(P0, "Oak", 2, 3)
        .with_mana_cost(mv_cost(3));
    control
        .add_creature_to_graveyard(P0, "Elm", 2, 4)
        .with_mana_cost(mv_cost(4));
    let control_spell = control
        .add_spell_to_hand_from_oracle(P0, "Control Die", false, "Roll a d20.")
        .id();
    let mut control_runner = control.build();
    d20_total(control_runner.cast(control_spell).resolve().events())
}

/// C-2 — RUNTIME cast drive. Two graveyard targets of MV 3 and 4 make the
/// roll modifier read 7 through `TargetedObjects`: a battlefield census
/// would read 0, and a dropped binding would read 0. Both die rows return
/// the cards, so the hand assertion holds whichever row the roll selects.
#[test]
fn song_of_inspiration_modifier_sums_both_graveyard_targets() {
    let (scenario, oak, elm, song) = song_scenario();
    let mut runner = scenario.build();
    let outcome = runner.cast(song).target_objects(&[oak, elm]).resolve();

    // Both rows return the targets: they rest in hand, and the halt is clean.
    outcome.assert_zone(&[oak, elm], Zone::Hand);
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "expected a clean return to priority, got {:?}",
        outcome.final_waiting_for()
    );

    let song_total = d20_total(outcome.events());
    let natural = natural_d20();
    assert_eq!(
        song_total as i32 - natural as i32,
        7,
        "Song's d20 modifier must be the summed MV of the two graveyard targets (3 + 4): \
         post-modifier total {song_total}, control natural {natural}"
    );
}

/// C-2 control — the walk enumerates: Oak (MV 3) plus Moth (MV 1) reads 4,
/// proving the total is summed per member rather than returned as a
/// constant, and that a second member's MV is actually visited.
#[test]
fn song_of_inspiration_modifier_enumerates_each_target() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let oak = scenario
        .add_creature_to_graveyard(P0, "Oak", 2, 3)
        .with_mana_cost(mv_cost(3))
        .id();
    let moth = scenario
        .add_creature_to_graveyard(P0, "Moth", 1, 1)
        .with_mana_cost(mv_cost(1))
        .id();
    let song = scenario
        .add_spell_to_hand_from_oracle(P0, "Song of Inspiration", true, SONG)
        .id();
    let mut runner = scenario.build();
    let outcome = runner.cast(song).target_objects(&[oak, moth]).resolve();

    outcome.assert_zone(&[oak, moth], Zone::Hand);
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "expected a clean return to priority, got {:?}",
        outcome.final_waiting_for()
    );

    let song_total = d20_total(outcome.events());
    let natural = natural_d20();
    assert_eq!(
        song_total as i32 - natural as i32,
        4,
        "Song's d20 modifier must sum each target (3 + 1): \
         post-modifier total {song_total}, control natural {natural}"
    );
}

/// Walk a parsed ability chain to its RollDie results table, if any.
fn roll_results(def: &AbilityDefinition) -> Option<&Vec<DieResultBranch>> {
    if let Effect::RollDie { results, .. } = &*def.effect {
        return Some(results);
    }
    def.sub_ability.as_deref().and_then(roll_results)
}

/// The roll modifier must be exactly the target-set mana-value sum.
fn assert_target_set_mana_value_modifier(modifier: &Option<DieRollModifier>) {
    let DieRollModifier::Add { value } = modifier.as_ref().expect("roll carries a modifier") else {
        panic!("roll modifier must be Add, got {modifier:?}");
    };
    let QuantityExpr::Ref {
        qty: QuantityRef::PropertyAggregate(aggregate),
    } = value
    else {
        panic!("roll modifier must be a property aggregate, got {value:?}");
    };
    assert_eq!(aggregate.function(), AggregateFunction::Sum);
    assert_eq!(aggregate.property(), ObjectProperty::ManaValue);
    assert!(
        matches!(
            aggregate.source(),
            CardTypeSetSource::TargetedObjects { slot: None }
        ),
        "roll modifier must aggregate the ability's targets, got {:?}",
        aggregate.source()
    );
}

/// C-3a — SHAPE. Song's `roll` marker is gone: the header is a `RollDie`
/// whose modifier is the target-set mana-value sum (present-case), and the
/// rows are consumed as branches rather than top-level abilities.
#[test]
fn song_header_modifier_is_the_target_set_aggregate() {
    let instant = vec!["Instant".to_string()];
    let parsed = parse_oracle_text(SONG, "Song of Inspiration", &[], &instant, &[]);
    assert_eq!(
        parsed.abilities.len(),
        1,
        "the rows must attach as branches, not top-level abilities"
    );
    let root = parsed.abilities.first().expect("spell ability");
    assert!(
        matches!(&*root.effect, Effect::TargetOnly { .. }),
        "the antecedent stays the target-selection clause, got {:?}",
        root.effect
    );
    let mut found = false;
    let mut current = Some(root);
    while let Some(def) = current {
        if let Effect::RollDie { modifier, .. } = &*def.effect {
            assert_target_set_mana_value_modifier(modifier);
            found = true;
        }
        current = def.sub_ability.as_deref();
    }
    assert!(found, "Song must carry a RollDie node");
    let json = serde_json::to_string(&parsed).expect("serializes");
    assert!(
        !json.contains("\"name\":\"roll\""),
        "the roll marker must be gone, got {json}"
    );
}

/// C-3b + H-1 + H-2 — SHAPE. Both rows attach with their printed bounds
/// (closed (1,14), open-ended (15,255)), and the 1–14 body is a supported
/// return-to-hand effect — not `Unimplemented`. The return lowers to
/// `Bounce` (the no-origin hand-destination form), whose tracked-set
/// sentinel target binds at resolution; the C-2 runtime tests prove the
/// cards actually reach the hand.
#[test]
fn song_rows_attach_with_printed_bounds_and_supported_bodies() {
    let instant = vec!["Instant".to_string()];
    let parsed = parse_oracle_text(SONG, "Song of Inspiration", &[], &instant, &[]);
    let results = parsed
        .abilities
        .iter()
        .find_map(roll_results)
        .expect("Song must carry a RollDie results table");
    assert_eq!(
        results
            .iter()
            .map(|row| (row.min, row.max))
            .collect::<Vec<_>>(),
        vec![(1, 14), (15, 255)],
        "closed row keeps its printed bound; open row carries the saturating bound"
    );
    let body = results
        .iter()
        .find(|row| row.min == 1)
        .map(|row| &*row.effect.effect)
        .expect("1-14 branch present");
    assert!(
        matches!(
            body,
            Effect::Bounce {
                target: TargetFilter::TrackedSet { .. },
                ..
            }
        ),
        "the 1-14 body must be a supported return effect, got {body:?}"
    );
}

/// C-3c + C-3d — HONEST-RED PIN (G.5 fallback). The 15+ body returns the
/// targets, but its lifegain half ("their total mana value") cannot lower:
/// the possessive batch default re-anchors only at the clause-assembly layer,
/// which is out of PR C's scope, so the row stays honestly red on exactly
/// that half. Exactly one `Unimplemented` node may remain, and it must be
/// the lifegain pin — never a silent absence.
#[test]
fn song_fifteen_plus_lifegain_stays_honestly_red() {
    let instant = vec!["Instant".to_string()];
    let parsed = parse_oracle_text(SONG, "Song of Inspiration", &[], &instant, &[]);
    let json = serde_json::to_string(&parsed).expect("serializes");
    assert_eq!(
        json.matches("\"type\":\"Unimplemented\"").count(),
        1,
        "exactly the lifegain half may be red, got {json}"
    );
    assert!(
        json.contains("\"name\":\"tracked_set_anaphor\""),
        "the remaining gap must be the named lifegain pin, got {json}"
    );
    let body_15 = parsed
        .abilities
        .iter()
        .find_map(roll_results)
        .expect("results table")
        .iter()
        .find(|row| row.min == 15)
        .map(|row| &row.effect)
        .expect("15+ branch present");
    assert!(
        matches!(
            &*body_15.effect,
            Effect::Bounce {
                target: TargetFilter::TrackedSet { .. },
                ..
            }
        ),
        "the 15+ return half stays supported, got {:?}",
        body_15.effect
    );
    let mut current = body_15.sub_ability.as_deref();
    let mut pinned = false;
    while let Some(def) = current {
        if let Effect::Unimplemented { name, .. } = &*def.effect {
            pinned |= name == "tracked_set_anaphor";
        }
        current = def.sub_ability.as_deref();
    }
    assert!(
        pinned,
        "the 15+ chain must carry the lifegain pin, got {body_15:?}"
    );
}

/// C-3 (Command) — SHAPE. Command the Dreadhorde's `deal` marker is gone:
/// the damage quantity is the target-set mana-value sum, reached through the
/// same contextual bare-card seam as Song's header.
#[test]
fn command_dreadhorde_damage_is_the_target_set_aggregate() {
    const COMMAND: &str = "Choose any number of target creature and/or planeswalker cards in graveyards. Command the Dreadhorde deals damage to you equal to the total mana value of those cards. Put them onto the battlefield under your control.";
    let sorcery = vec!["Sorcery".to_string()];
    let parsed = parse_oracle_text(COMMAND, "Command the Dreadhorde", &[], &sorcery, &[]);
    let mut found = false;
    for ability in &parsed.abilities {
        let mut current = Some(ability);
        while let Some(def) = current {
            if let Effect::DealDamage { amount, .. } = &*def.effect {
                let QuantityExpr::Ref {
                    qty: QuantityRef::PropertyAggregate(aggregate),
                } = amount
                else {
                    panic!("damage quantity must be a property aggregate, got {amount:?}");
                };
                assert_eq!(aggregate.function(), AggregateFunction::Sum);
                assert_eq!(aggregate.property(), ObjectProperty::ManaValue);
                assert!(
                    matches!(
                        aggregate.source(),
                        CardTypeSetSource::TargetedObjects { slot: None }
                    ),
                    "damage must aggregate the ability's targets, got {:?}",
                    aggregate.source()
                );
                found = true;
            }
            current = def.sub_ability.as_deref();
        }
    }
    assert!(
        found,
        "Command must carry a damage quantity over its targets"
    );
    let json = serde_json::to_string(&parsed).expect("serializes");
    assert!(
        !json.contains("\"name\":\"deal\""),
        "the deal marker must be gone, got {json}"
    );
}

/// C-3 anti-overclaim — Amzu and Cait fail upstream of the aggregate (their
/// markers are `put` / `unbound_subject`, class D.4) and must stay red: PR C
/// binds no `TargetedObjects` source into either card.
#[test]
fn amzu_and_cait_stay_red() {
    const AMZU: &str = "Flying, menace\nOther Insects you control have menace.\nWhenever one or more cards leave your graveyard, you may create a 1/1 black and green Insect creature token, then put a number of +1/+1 counters on it equal to the greatest mana value among those cards. Do this only once each turn.";
    const CAIT: &str = "During your turn, Cait has indestructible.\nWhenever Cait attacks, you and defending player each draw a card, then discard a card. Put two +1/+1 counters on Cait if you discarded the card with the greatest mana value among those cards or tied for greatest.";
    let creature = vec!["Creature".to_string()];
    for (name, oracle) in [("Amzu, Swarm's Hunger", AMZU), ("Cait, Cage Brawler", CAIT)] {
        let parsed = parse_oracle_text(oracle, name, &[], &creature, &[]);
        let json = serde_json::to_string(&parsed).expect("serializes");
        assert!(
            json.contains("\"type\":\"Unimplemented\""),
            "{name} must stay red, got {json}"
        );
        assert!(
            !json.contains("\"TargetedObjects\""),
            "{name} must not gain a target-set source, got {json}"
        );
    }
}

/// C-2 hostile (CR 608.2b) — Elm is exiled while Song waits on the stack, so
/// it is an illegal target at resolution: the aggregate must read the
/// surviving target only (3), not 0 and not the stale sum 7. The surviving
/// target still returns to hand while the illegal one stays exiled.
#[test]
fn song_of_inspiration_modifier_drops_a_target_lost_mid_stack() {
    let (scenario, oak, elm, song) = song_scenario();
    let mut runner = scenario.build();
    let mut committed = runner.cast(song).target_objects(&[oak, elm]).commit();

    // F9 (CR 400.7 + CR 608.2b): move Elm through the real zone pipeline
    // while Song waits on the stack. A direct `obj.zone =` assignment
    // bypasses zone collections, LKI, and incarnation bookkeeping, so it
    // cannot discriminate the production target-legality boundary.
    let elm_incarnation_before = committed
        .state()
        .objects
        .get(&elm)
        .expect("Elm is staged in the graveyard")
        .incarnation;
    let mut exile_events = Vec::new();
    engine::game::zones::move_to_zone(committed.state_mut(), elm, Zone::Exile, &mut exile_events);
    assert!(
        exile_events.iter().any(|event| matches!(
            event,
            GameEvent::ZoneChanged {
                object_id,
                from: Some(Zone::Graveyard),
                to: Zone::Exile,
                ..
            } if *object_id == elm
        )),
        "NON-VACUITY: the mid-stack exile must move Elm graveyard→exile \
         through the pipeline, got {exile_events:?}"
    );
    let elm_after = committed
        .state()
        .objects
        .get(&elm)
        .expect("Elm survives the move under its storage id");
    assert_eq!(
        elm_after.zone,
        Zone::Exile,
        "the pipeline must deliver Elm to exile"
    );
    assert_ne!(
        elm_after.incarnation, elm_incarnation_before,
        "CR 400.7: the exiled Elm is a new object, not the targeted incarnation"
    );
    // No LKI assertion here: the pipeline snapshots LKI only on departure
    // from the battlefield, exile, or the stack (`zones.rs`), so a
    // graveyard→exile move records none. The illegality rides on zone
    // membership plus the incarnation break, both asserted above.

    let outcome = committed.resolve();
    // Oak returns; Elm is unaffected (CR 608.2b) and stays exiled.
    outcome.assert_zone(&[oak], Zone::Hand);
    outcome.assert_zone(&[elm], Zone::Exile);
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "expected a clean return to priority, got {:?}",
        outcome.final_waiting_for()
    );

    let song_total = d20_total(outcome.events());
    let natural = natural_d20();
    assert_eq!(
        song_total as i32 - natural as i32,
        3,
        "Song's d20 modifier must read the surviving target only (3): \
         post-modifier total {song_total}, control natural {natural}"
    );
}

/// R1-MED REGRESSION (Step-6 R1 finding: a suspended-then-resumed roll modifier
/// reads 0 instead of the parked target-set total). The CR 706.6 keep-choice
/// resume has no ability in scope, so the parked frame's `targets` are the
/// only carrier for a `TargetedObjects` aggregate: the resume must feed them
/// to the modifier (CR 608.2c: the roll instruction still means what the
/// target-selection clause chose; CR 706.2: modifiers add to the natural
/// result to produce the final number).
///
/// Fixture: Oak (MV 3) + Elm (MV 4) parked as the frame's targets with Song's
/// exact modifier shape — the `assert_target_set_mana_value_modifier` guard
/// below ties the typed fixture to Song's real parse so it cannot drift —
/// over naturals [3, 3], a tie under one Lowest rule that forces the keep
/// prompt (the CR 706.6 suspension under test).
///
/// A chained reader on the resume path: the 10-row gains life equal to the roll,
/// so it observes the post-modifier aggregate through the
/// `QuantityRef::EventContextAmount` cascade — the consumer the
/// `SelectDieRolls` save/restore discipline exists to serve.
///
/// REVERT-FAILING: without the parked-targets channel the resume resolves the
/// aggregate against an empty population and reads 0, so the survivor's final
/// number is 3, the 10-row never matches, and life stays 20.
#[test]
fn resumed_keep_choice_modifier_reads_the_parked_targets() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let roller = scenario.add_creature(P0, "Roller", 1, 1).id();
    let oak = scenario
        .add_creature_to_graveyard(P0, "Oak", 2, 3)
        .with_mana_cost(mv_cost(3))
        .id();
    let elm = scenario
        .add_creature_to_graveyard(P0, "Elm", 2, 4)
        .with_mana_cost(mv_cost(4))
        .id();
    let mut runner = scenario.build();

    let modifier = DieRollModifier::Add {
        value: QuantityExpr::Ref {
            qty: QuantityRef::PropertyAggregate(
                engine::types::ability::PropertyAggregate::new(
                    AggregateFunction::Sum,
                    ObjectProperty::ManaValue,
                    CardTypeSetSource::TargetedObjects { slot: None },
                )
                .expect("statically valid property aggregate"),
            ),
        },
    };
    assert_target_set_mana_value_modifier(&Some(modifier.clone()));

    // CR 706.3a: each surviving die consults the results table independently.
    // The 10-row matches only a post-modifier 10 (natural 3 + parked 7), so a
    // dropped binding (3) misses it and the lifegain never fires.
    let gain_branch = DieResultBranch {
        min: 10,
        max: 10,
        effect: Box::new(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::GainLife {
                amount: QuantityExpr::Ref {
                    qty: QuantityRef::EventContextAmount,
                },
                player: TargetFilter::Controller,
            },
        )),
    };
    runner.state_mut().push_die_roll_frame(PendingDieRoll {
        source_id: roller,
        controller: P0,
        roller: P0,
        quantity_controller: None,
        scoped_player: None,
        targets: vec![TargetRef::Object(oak), TargetRef::Object(elm)],
        sides: 20,
        results: vec![3, 3],
        ignore_rules: vec![DieRollIgnoreRule::Lowest],
        results_table: vec![gain_branch],
        modifier: Some(modifier),
        die_result: None,
        next_index: 0,
        running_total: 0,
        rolled_any: false,
        forced_ignored: vec![],
        // No creating ability: a directly pushed frame has no chain-root
        // target list for counter-gated branch readers.
        chain_root_targets: vec![],
        trigger_context: None,
        illegal_target_slots: vec![],
        chain_root_target_pins: vec![],
        declared_capture: None,
        local_target_pins: vec![],
        keyed_target_incarnations: vec![],
        declared_player: None,
    });
    runner.state_mut().waiting_for = WaitingFor::DieKeepChoice {
        player: P0,
        results: vec![3, 3],
        ignorable_indices: vec![0, 1],
        ignore_count: 1,
    };

    let result = runner
        .act(GameAction::SelectDieRolls {
            ignore_indices: vec![0],
        })
        .expect("submit the tie-break");

    let rolls: Vec<u8> = result
        .events
        .iter()
        .filter_map(|event| match event {
            GameEvent::DieRolled {
                result: Some(total),
                ..
            } => Some(*total),
            _ => None,
        })
        .collect();
    assert_eq!(
        rolls.len(),
        1,
        "reach-guard: exactly one of the two tied rolls survives the keep choice"
    );
    assert_eq!(
        rolls,
        vec![10],
        "CR 706.2: the survivor's final number is its natural 3 plus the parked \
         targets' total MV (3 + 4); a dropped binding reads 0 and emits 3"
    );
    assert_eq!(
        runner.state().players[P0.0 as usize].life,
        30,
        "the 10-row's chained reader must observe the post-modifier aggregate on \
         the resume path (20 + 10); a dropped binding misses the row and life stays 20"
    );
    assert_eq!(
        runner.state().die_result_this_resolution,
        None,
        "post-completion restore discipline: the inner aggregate must not leak \
         into the outer context once the instruction finishes"
    );
}

/// PR-C F4 — RESUME. A paused indexed roll resolves its slot against the
/// carried declared list (slot-base numbering), not the local slice: slot 1
/// names B even though the local list holds only B at position 0, and the
/// earlier illegal slot 0 does not renumber the selection (CR 608.2b
/// indexing happens before checks). Natural 10 + B's power 5 = 15; raw
/// local indexing misses (`get(1)` on a one-element list) and emits 10.
#[test]
fn parked_indexed_roll_resolves_slot_against_chain_root_despite_earlier_illegal_slot() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let condemned = scenario.add_creature(P0, "Condemned", 2, 2).id();
    let brute = scenario.add_creature(P0, "Brute", 5, 5).id();
    let roller = scenario.add_creature(P0, "Roller", 1, 1).id();
    let runner = scenario.build();
    let mut state = runner.state().clone();

    let modifier = DieRollModifier::Add {
        value: QuantityExpr::Ref {
            qty: QuantityRef::PropertyAggregate(
                engine::types::ability::PropertyAggregate::new(
                    AggregateFunction::Sum,
                    ObjectProperty::Power,
                    CardTypeSetSource::TargetedObjects { slot: Some(1) },
                )
                .expect("statically valid property aggregate"),
            ),
        },
    };
    let pending = PendingDieRoll {
        source_id: roller,
        controller: P0,
        roller: P0,
        quantity_controller: None,
        scoped_player: None,
        // Local slice holds only the roller's own target; declared slot 1
        // still names the brute through the declared list.
        targets: vec![TargetRef::Object(brute)],
        sides: 20,
        results: vec![10],
        ignore_rules: vec![],
        results_table: vec![],
        modifier: Some(modifier),
        die_result: None,
        next_index: 0,
        running_total: 0,
        rolled_any: false,
        forced_ignored: vec![],
        chain_root_targets: vec![],
        trigger_context: None,
        // The carrier stamped slot 0 illegal as the chain began to resolve;
        // indexing still counts it (no renumbering).
        illegal_target_slots: vec![0],
        chain_root_target_pins: vec![],
        declared_capture: Some(CapturedDeclarationPayload {
            targets: vec![TargetRef::Object(condemned), TargetRef::Object(brute)],
            target_pins: vec![None, None],
            ahead: 0,
        }),
        local_target_pins: vec![],
        keyed_target_incarnations: vec![],
        declared_player: Some(engine::game::filter::DeclaredPlayerTarget::Undeclared),
    };
    let rolls = resumed_die_totals(&mut state, pending);
    assert_eq!(
        rolls,
        vec![15],
        "declared slot 1 names the brute's power (10 + 5); raw local indexing misses and emits 10"
    );
}

/// PR-C F4 — RESUME. A paused indexed roll drops a selected slot the carrier
/// stamped illegal: slot 0 names the condemned object through the chain-root
/// list, the stamp excludes it, and the modifier reads 0 (natural 10 stays
/// 10). Raw local indexing would return the brute (position 0 of the local
/// slice) and emit 15.
#[test]
fn parked_indexed_roll_drops_selected_slot_stamped_illegal() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let condemned = scenario.add_creature(P0, "Condemned", 2, 2).id();
    let brute = scenario.add_creature(P0, "Brute", 5, 5).id();
    let roller = scenario.add_creature(P0, "Roller", 1, 1).id();
    let runner = scenario.build();
    let mut state = runner.state().clone();

    let modifier = DieRollModifier::Add {
        value: QuantityExpr::Ref {
            qty: QuantityRef::PropertyAggregate(
                engine::types::ability::PropertyAggregate::new(
                    AggregateFunction::Sum,
                    ObjectProperty::Power,
                    CardTypeSetSource::TargetedObjects { slot: Some(0) },
                )
                .expect("statically valid property aggregate"),
            ),
        },
    };
    let pending = PendingDieRoll {
        source_id: roller,
        controller: P0,
        roller: P0,
        quantity_controller: None,
        scoped_player: None,
        targets: vec![TargetRef::Object(brute)],
        sides: 20,
        results: vec![10],
        ignore_rules: vec![],
        results_table: vec![],
        modifier: Some(modifier),
        die_result: None,
        next_index: 0,
        running_total: 0,
        rolled_any: false,
        forced_ignored: vec![],
        chain_root_targets: vec![],
        trigger_context: None,
        illegal_target_slots: vec![0],
        chain_root_target_pins: vec![],
        declared_capture: Some(CapturedDeclarationPayload {
            targets: vec![TargetRef::Object(condemned), TargetRef::Object(brute)],
            target_pins: vec![None, None],
            ahead: 0,
        }),
        local_target_pins: vec![],
        keyed_target_incarnations: vec![],
        declared_player: Some(engine::game::filter::DeclaredPlayerTarget::Undeclared),
    };
    let rolls = resumed_die_totals(&mut state, pending);
    assert_eq!(
        rolls,
        vec![10],
        "the stamped-illegal slot contributes nothing; raw local indexing returns the brute and emits 15"
    );
}

/// F1 — RESUME. A paused indexed roll admits a selected slot whose
/// announced incarnation legally departed: the brute's pin goes stale
/// through a real pipeline exit, and the carried pin reads its exact
/// departure LKI power 2 (natural 10 + 2 = 12) — the same
/// occurrence-aware read the live path applies. Only a carrier-stamped
/// illegal slot contributes nothing.
#[test]
fn parked_indexed_roll_reads_departure_lki_for_selected_slot_with_stale_pin() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let brute = scenario.add_creature(P0, "Brute", 2, 2).id();
    let roller = scenario.add_creature(P0, "Roller", 1, 1).id();
    let runner = scenario.build();
    let mut state = runner.state().clone();

    // Production-shaped shell: Doom Blade's real targeting sentence pins the
    // live battlefield incarnation; the real pipeline exit below stales it.
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
        roller,
        P0,
        vec![TargetRef::Object(brute)],
    );
    ability.announce_target_pins(&state);
    let pin = ability.target_pin_at(0).expect("pin announced");
    let mut zone_events = Vec::new();
    engine::game::zones::move_to_zone(&mut state, brute, Zone::Graveyard, &mut zone_events);
    assert!(
        !pin.is_current(&state),
        "reach guard: the battlefield exit must stale the pin"
    );

    let modifier = DieRollModifier::Add {
        value: QuantityExpr::Ref {
            qty: QuantityRef::PropertyAggregate(
                engine::types::ability::PropertyAggregate::new(
                    AggregateFunction::Sum,
                    ObjectProperty::Power,
                    CardTypeSetSource::TargetedObjects { slot: Some(0) },
                )
                .expect("statically valid property aggregate"),
            ),
        },
    };
    let pending = PendingDieRoll {
        source_id: roller,
        controller: P0,
        roller: P0,
        quantity_controller: None,
        scoped_player: None,
        targets: vec![TargetRef::Object(brute)],
        sides: 20,
        results: vec![10],
        ignore_rules: vec![],
        results_table: vec![],
        modifier: Some(modifier),
        die_result: None,
        next_index: 0,
        running_total: 0,
        rolled_any: false,
        forced_ignored: vec![],
        chain_root_targets: vec![],
        trigger_context: None,
        illegal_target_slots: vec![],
        chain_root_target_pins: vec![],
        declared_capture: Some(CapturedDeclarationPayload {
            targets: vec![TargetRef::Object(brute)],
            target_pins: vec![Some(pin)],
            ahead: 0,
        }),
        local_target_pins: vec![],
        keyed_target_incarnations: vec![],
        declared_player: Some(engine::game::filter::DeclaredPlayerTarget::Undeclared),
    };
    let rolls = resumed_die_totals(&mut state, pending);
    assert_eq!(
        rolls,
        vec![12],
        "the stale-pinned slot reads its departure power 2 (10 + 2); only a stamped-illegal slot contributes nothing"
    );
}

/// M2 — RESUME. Pins must align with the list they gate: the frame below
/// carries an EMPTY root with a live local target, plus a stale root pin
/// left over for another slot. The stale root pin must not judge the local
/// referent — the local read proceeds (10 + 2 = 12). Pre-fix the lookup
/// applied root pins to the local list and dropped the slot (10).
#[test]
fn parked_indexed_roll_does_not_judge_local_fallback_by_root_pins() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let brute = scenario.add_creature(P0, "Brute", 2, 2).id();
    let ghost = scenario.add_creature(P0, "Ghost", 3, 3).id();
    let roller = scenario.add_creature(P0, "Roller", 1, 1).id();
    let runner = scenario.build();
    let mut state = runner.state().clone();

    // A stale pin for GHOST (exited through the real pipeline); brute never moves.
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
        roller,
        P0,
        vec![TargetRef::Object(ghost)],
    );
    ability.announce_target_pins(&state);
    let ghost_pin = ability.target_pin_at(0).expect("pin announced");
    let mut zone_events = Vec::new();
    engine::game::zones::move_to_zone(&mut state, ghost, Zone::Graveyard, &mut zone_events);
    assert!(
        !ghost_pin.is_current(&state),
        "reach guard: the battlefield exit must stale the ghost pin"
    );

    let modifier = DieRollModifier::Add {
        value: QuantityExpr::Ref {
            qty: QuantityRef::PropertyAggregate(
                engine::types::ability::PropertyAggregate::new(
                    AggregateFunction::Sum,
                    ObjectProperty::Power,
                    CardTypeSetSource::TargetedObjects { slot: Some(0) },
                )
                .expect("statically valid property aggregate"),
            ),
        },
    };
    let pending = PendingDieRoll {
        source_id: roller,
        controller: P0,
        roller: P0,
        quantity_controller: None,
        scoped_player: None,
        targets: vec![TargetRef::Object(brute)],
        sides: 20,
        results: vec![10],
        ignore_rules: vec![],
        results_table: vec![],
        modifier: Some(modifier),
        die_result: None,
        next_index: 0,
        running_total: 0,
        rolled_any: false,
        forced_ignored: vec![],
        chain_root_targets: vec![],
        trigger_context: None,
        illegal_target_slots: vec![],
        chain_root_target_pins: vec![Some(ghost_pin)],
        declared_capture: None,
        local_target_pins: vec![],
        keyed_target_incarnations: vec![],
        declared_player: None,
    };
    let rolls = resumed_die_totals(&mut state, pending);
    assert_eq!(
        rolls,
        vec![12],
        "the live local brute reads 2; judging it by the ghost's stale root pin drops the slot and emits 10"
    );
}

/// M2 legacy guard — RESUME. A frame carrying only the chain-root list (no
/// declared entries, as all pre-M2 frames do) keeps the established root
/// behavior through the refactor: stamped slot 0 drops, so the slot-0 read
/// contributes nothing (natural 10 stays 10). Green before and after.
#[test]
fn parked_indexed_roll_root_only_frame_keeps_legacy_selection() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let condemned = scenario.add_creature(P0, "Condemned", 2, 2).id();
    let brute = scenario.add_creature(P0, "Brute", 5, 5).id();
    let roller = scenario.add_creature(P0, "Roller", 1, 1).id();
    let runner = scenario.build();
    let mut state = runner.state().clone();

    let modifier = DieRollModifier::Add {
        value: QuantityExpr::Ref {
            qty: QuantityRef::PropertyAggregate(
                engine::types::ability::PropertyAggregate::new(
                    AggregateFunction::Sum,
                    ObjectProperty::Power,
                    CardTypeSetSource::TargetedObjects { slot: Some(0) },
                )
                .expect("statically valid property aggregate"),
            ),
        },
    };
    let pending = PendingDieRoll {
        source_id: roller,
        controller: P0,
        roller: P0,
        quantity_controller: None,
        scoped_player: None,
        targets: vec![TargetRef::Object(brute)],
        sides: 20,
        results: vec![10],
        ignore_rules: vec![],
        results_table: vec![],
        modifier: Some(modifier),
        die_result: None,
        next_index: 0,
        running_total: 0,
        rolled_any: false,
        forced_ignored: vec![],
        chain_root_targets: vec![TargetRef::Object(condemned), TargetRef::Object(brute)],
        trigger_context: None,
        illegal_target_slots: vec![0],
        chain_root_target_pins: vec![None, None],
        declared_capture: None,
        local_target_pins: vec![],
        keyed_target_incarnations: vec![],
        declared_player: None,
    };
    let rolls = resumed_die_totals(&mut state, pending);
    assert_eq!(
        rolls,
        vec![10],
        "the stamped-illegal root slot contributes nothing"
    );
}

/// M2 — RESUME (modal slot base + downstream offset). Mode 1 of a modal
/// spell declares slot 0 naming Brute 5/5; mode 0 declares slot 0 naming
/// Condemned 2/2 (stamped illegal at resolution). The parked read of the
/// mode-1 slot must resolve Brute (10 + 5 = 15): the stamp judges root
/// numbering (ahead 1 + index 0 = 1, not stamped), and the read counts from
/// the mode base, not the chain root. Pre-fix the lookup indexes the root
/// list raw and checks the raw stamp: root[0] is Condemned, stamped → drop
/// → 10. (Pre-fix frames cannot express modal facts — that absence was
/// the bug — so this frame carries the declared list and offset.)
#[test]
fn parked_indexed_roll_resolves_modal_slot_against_its_base() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let condemned = scenario.add_creature(P0, "Condemned", 2, 2).id();
    let brute = scenario.add_creature(P0, "Brute", 5, 5).id();
    let roller = scenario.add_creature(P0, "Roller", 1, 1).id();
    let runner = scenario.build();
    let mut state = runner.state().clone();

    let modifier = DieRollModifier::Add {
        value: QuantityExpr::Ref {
            qty: QuantityRef::PropertyAggregate(
                engine::types::ability::PropertyAggregate::new(
                    AggregateFunction::Sum,
                    ObjectProperty::Power,
                    CardTypeSetSource::TargetedObjects { slot: Some(0) },
                )
                .expect("statically valid property aggregate"),
            ),
        },
    };
    let pending = PendingDieRoll {
        source_id: roller,
        controller: P0,
        roller: P0,
        quantity_controller: None,
        scoped_player: None,
        targets: vec![TargetRef::Object(brute)],
        sides: 20,
        results: vec![10],
        ignore_rules: vec![],
        results_table: vec![],
        modifier: Some(modifier),
        die_result: None,
        next_index: 0,
        running_total: 0,
        rolled_any: false,
        forced_ignored: vec![],
        chain_root_targets: vec![TargetRef::Object(condemned), TargetRef::Object(brute)],
        trigger_context: None,
        illegal_target_slots: vec![0],
        chain_root_target_pins: vec![None, None],
        declared_capture: Some(CapturedDeclarationPayload {
            targets: vec![TargetRef::Object(brute)],
            target_pins: vec![None],
            ahead: 1,
        }),
        local_target_pins: vec![],
        keyed_target_incarnations: vec![],
        declared_player: Some(engine::game::filter::DeclaredPlayerTarget::Undeclared),
    };
    let rolls = resumed_die_totals(&mut state, pending);
    assert_eq!(
        rolls,
        vec![15],
        "the mode-1 slot-0 read resolves Brute (10 + 5); raw root indexing drops the stamped root slot 0 and emits 10"
    );
}

/// M2 guard — RESUME. Repeated IDs with divergent pins select positionally:
/// slot 0's pin went stale while slot 1's pin for the same object is
/// current, so the slot-1 read proceeds live (10 + 2 = 12). The pins here
/// are staged per-slot (as retarget capture produces); the capture half is
/// pinned separately against the real capture helper.
#[test]
fn parked_indexed_roll_reads_repeated_id_through_its_own_pin() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let brute = scenario.add_creature(P0, "Brute", 2, 2).id();
    let roller = scenario.add_creature(P0, "Roller", 1, 1).id();
    let runner = scenario.build();
    let mut state = runner.state().clone();

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
        roller,
        P0,
        vec![TargetRef::Object(brute)],
    );
    ability.announce_target_pins(&state);
    let stale_pin = ability.target_pin_at(0).expect("pin announced");
    let mut zone_events = Vec::new();
    engine::game::zones::move_to_zone(&mut state, brute, Zone::Exile, &mut zone_events);
    engine::game::zones::move_to_zone(&mut state, brute, Zone::Battlefield, &mut zone_events);
    let current_pin = engine::types::identifiers::ObjectIncarnationRef::from_object(
        state.objects.get(&brute).expect("brute returned"),
    );
    assert!(
        !stale_pin.is_current(&state) && current_pin.is_current(&state),
        "reach guard: stale/current pin pair required"
    );

    let modifier = DieRollModifier::Add {
        value: QuantityExpr::Ref {
            qty: QuantityRef::PropertyAggregate(
                engine::types::ability::PropertyAggregate::new(
                    AggregateFunction::Sum,
                    ObjectProperty::Power,
                    CardTypeSetSource::TargetedObjects { slot: Some(1) },
                )
                .expect("statically valid property aggregate"),
            ),
        },
    };
    let pending = PendingDieRoll {
        source_id: roller,
        controller: P0,
        roller: P0,
        quantity_controller: None,
        scoped_player: None,
        targets: vec![TargetRef::Object(brute), TargetRef::Object(brute)],
        sides: 20,
        results: vec![10],
        ignore_rules: vec![],
        results_table: vec![],
        modifier: Some(modifier),
        die_result: None,
        next_index: 0,
        running_total: 0,
        rolled_any: false,
        forced_ignored: vec![],
        chain_root_targets: vec![],
        trigger_context: None,
        illegal_target_slots: vec![],
        chain_root_target_pins: vec![],
        declared_capture: Some(CapturedDeclarationPayload {
            targets: vec![TargetRef::Object(brute), TargetRef::Object(brute)],
            target_pins: vec![Some(stale_pin), Some(current_pin)],
            ahead: 0,
        }),
        local_target_pins: vec![],
        keyed_target_incarnations: vec![],
        declared_player: Some(engine::game::filter::DeclaredPlayerTarget::Undeclared),
    };
    let rolls = resumed_die_totals(&mut state, pending);
    assert_eq!(
        rolls,
        vec![12],
        "slot 1 reads through its own current pin (10 + 2); first-match pinning would judge it by slot 0's stale pin and emit 10"
    );
}

/// M2 — RESUME. A parked every-target read preserves the selected occurrence
/// into characteristics: Brute 2/2 is pumped to 5/5 by a parsed, resolved
/// Giant Growth, pinned, then departed through the real pipeline. The
/// unslotted aggregate must read its departure LKI power 5 (10 + 5 = 15) —
/// not the retained reverted base 2 (12). Pre-fix the parked read was
/// pin-blind and emitted 12.
#[test]
fn parked_unslotted_roll_reads_departed_member_from_departure_lki() {
    const GIANT_GROWTH: &str = "Target creature gets +3/+3 until end of turn.";
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let brute = scenario.add_creature(P0, "Brute", 2, 2).id();
    let roller = scenario.add_creature(P0, "Roller", 1, 1).id();
    let growth = scenario
        .add_spell_to_hand_from_oracle(P0, "Giant Growth", true, GIANT_GROWTH)
        .id();
    let mut runner = scenario.build();
    runner.cast(growth).target_objects(&[brute]).resolve();
    let pumped = runner.state().objects.get(&brute).expect("Brute stands");
    assert_eq!(
        (pumped.power, pumped.toughness),
        (Some(5), Some(5)),
        "reach guard: Giant Growth must resolve to 5/5"
    );

    // Pin the pumped incarnation, then depart through the real pipeline.
    let mut state = runner.state().clone();
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
        roller,
        P0,
        vec![TargetRef::Object(brute)],
    );
    ability.announce_target_pins(&state);
    let pin = ability.target_pin_at(0).expect("pin announced");
    let mut zone_events = Vec::new();
    engine::game::zones::move_to_zone(&mut state, brute, Zone::Graveyard, &mut zone_events);
    assert!(
        !pin.is_current(&state),
        "reach guard: the battlefield exit must stale the pin"
    );

    let modifier = DieRollModifier::Add {
        value: QuantityExpr::Ref {
            qty: QuantityRef::PropertyAggregate(
                engine::types::ability::PropertyAggregate::new(
                    AggregateFunction::Sum,
                    ObjectProperty::Power,
                    CardTypeSetSource::TargetedObjects { slot: None },
                )
                .expect("statically valid property aggregate"),
            ),
        },
    };
    let pending = PendingDieRoll {
        source_id: roller,
        controller: P0,
        roller: P0,
        quantity_controller: None,
        scoped_player: None,
        targets: vec![TargetRef::Object(brute)],
        sides: 20,
        results: vec![10],
        ignore_rules: vec![],
        results_table: vec![],
        modifier: Some(modifier),
        die_result: None,
        next_index: 0,
        running_total: 0,
        rolled_any: false,
        forced_ignored: vec![],
        chain_root_targets: vec![TargetRef::Object(brute)],
        trigger_context: None,
        illegal_target_slots: vec![],
        chain_root_target_pins: vec![],
        declared_capture: None,
        local_target_pins: vec![Some(pin)],
        keyed_target_incarnations: vec![],
        declared_player: None,
    };
    let rolls = resumed_die_totals(&mut state, pending);
    assert_eq!(
        rolls,
        vec![15],
        "the unslotted parked read measures departure power 5 (10 + 5); a pin-blind live read emits 12"
    );
}

/// M2 — RESUME. Mixed player/object slots keep declared numbering: slot 0
/// names a player (no characteristics, no pin) and slot 1 names Brute 2/2,
/// so the slot-1 read contributes 2 (10 + 2 = 12). Compressing players out
/// of the numbering would read slot 1 out of range and emit 10.
#[test]
fn parked_indexed_roll_counts_player_slots_in_declared_numbering() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let brute = scenario.add_creature(P0, "Brute", 2, 2).id();
    let roller = scenario.add_creature(P0, "Roller", 1, 1).id();
    let runner = scenario.build();
    let mut state = runner.state().clone();

    let modifier = DieRollModifier::Add {
        value: QuantityExpr::Ref {
            qty: QuantityRef::PropertyAggregate(
                engine::types::ability::PropertyAggregate::new(
                    AggregateFunction::Sum,
                    ObjectProperty::Power,
                    CardTypeSetSource::TargetedObjects { slot: Some(1) },
                )
                .expect("statically valid property aggregate"),
            ),
        },
    };
    let pending = PendingDieRoll {
        source_id: roller,
        controller: P0,
        roller: P0,
        quantity_controller: None,
        scoped_player: None,
        targets: vec![
            TargetRef::Player(engine::game::scenario::P1),
            TargetRef::Object(brute),
        ],
        sides: 20,
        results: vec![10],
        ignore_rules: vec![],
        results_table: vec![],
        modifier: Some(modifier),
        die_result: None,
        next_index: 0,
        running_total: 0,
        rolled_any: false,
        forced_ignored: vec![],
        chain_root_targets: vec![],
        trigger_context: None,
        illegal_target_slots: vec![],
        chain_root_target_pins: vec![],
        declared_capture: Some(CapturedDeclarationPayload {
            targets: vec![
                TargetRef::Player(engine::game::scenario::P1),
                TargetRef::Object(brute),
            ],
            target_pins: vec![None, None],
            ahead: 0,
        }),
        local_target_pins: vec![],
        keyed_target_incarnations: vec![],
        declared_player: Some(engine::game::filter::DeclaredPlayerTarget::Legal(
            engine::game::scenario::P1,
        )),
    };
    let rolls = resumed_die_totals(&mut state, pending);
    assert_eq!(
        rolls,
        vec![12],
        "slot 1 names Brute through the player slot 0 (10 + 2)"
    );
}

/// MED1-T1 — PAUSE. A Max aggregate over the Blot-Out axis ("each
/// creature/planeswalker an opponent controls") parked mid-roll must resolve
/// the TargetOpponent controller axis against the DECLARED player target:
/// with P1 declared, the resumed read sees P1's Brute 5/5 (1 + 5 = 6).
/// The inline anchor is the Imperial Edict family, which resolves the same
/// axis live; a pin-blind resumed read sees no declared player and emits a
/// non-6 total (or errors on the empty set).
#[test]
fn parked_max_aggregate_resolves_target_opponent_axis_from_declared_player() {
    use engine::game::scenario::P1;
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature(P0, "Own Brute", 9, 9);
    scenario.add_creature(P1, "Foe Brute", 5, 5);
    let roller = scenario.add_creature(P0, "Roller", 1, 1).id();
    let runner = scenario.build();
    let mut state = runner.state().clone();

    let blot_out_axis = |type_filter| {
        TargetFilter::Typed(TypedFilter {
            type_filters: vec![type_filter],
            controller: Some(ControllerRef::TargetOpponent),
            properties: vec![],
        })
    };
    let modifier = DieRollModifier::Add {
        value: QuantityExpr::Ref {
            qty: QuantityRef::PropertyAggregate(
                engine::types::ability::PropertyAggregate::new(
                    AggregateFunction::Max,
                    ObjectProperty::Power,
                    CardTypeSetSource::Objects {
                        filter: TargetFilter::Or {
                            filters: vec![
                                blot_out_axis(TypeFilter::Creature),
                                blot_out_axis(TypeFilter::Planeswalker),
                            ],
                        },
                    },
                )
                .expect("statically valid property aggregate"),
            ),
        },
    };
    let pending = PendingDieRoll {
        source_id: roller,
        controller: P0,
        roller: P0,
        quantity_controller: None,
        scoped_player: None,
        // No local player: the recorded verdict below is the only thing
        // naming P1, so list reconstruction would answer Undeclared (1).
        targets: vec![],
        sides: 20,
        results: vec![1],
        ignore_rules: vec![],
        results_table: vec![],
        modifier: Some(modifier),
        die_result: None,
        next_index: 0,
        running_total: 0,
        rolled_any: false,
        forced_ignored: vec![],
        chain_root_targets: vec![],
        trigger_context: None,
        illegal_target_slots: vec![],
        chain_root_target_pins: vec![],
        declared_capture: Some(CapturedDeclarationPayload {
            targets: vec![TargetRef::Player(P1)],
            target_pins: vec![None],
            ahead: 0,
        }),
        local_target_pins: vec![],
        keyed_target_incarnations: vec![],
        declared_player: Some(engine::game::filter::DeclaredPlayerTarget::Legal(P1)),
    };
    let rolls = resumed_die_totals(&mut state, pending);
    assert_eq!(
        rolls,
        vec![6],
        "the resumed read measures the declared opponent's max power (1 + 5), not own 9"
    );
}

/// B1a — PAUSE (local-first priority, pre-field frame). The live authority
/// reads the node's own player target before the root fallback; a frame
/// without a captured verdict reconstructs the same order: with P1 local
/// and P0 on the root (and the recorded mode-base list showing P0), the
/// resumed TargetOpponent axis resolves P1 (1 + 5 = 6). Preferring the
/// recorded base list would emit 11 (P0's 9 + 1).
#[test]
fn parked_declared_player_prefers_local_over_root() {
    use engine::game::scenario::P1;
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature(P0, "Own Brute", 9, 9);
    scenario.add_creature(P1, "Foe Brute", 5, 5);
    // NOTE: the Roller below is P0's 1-power creature, so a root/base-P0
    // misread sums 9 + 1 and emits 11, not 10.
    let roller = scenario.add_creature(P0, "Roller", 1, 1).id();
    let runner = scenario.build();
    let mut state = runner.state().clone();

    let pending = PendingDieRoll {
        source_id: roller,
        controller: P0,
        roller: P0,
        quantity_controller: None,
        scoped_player: None,
        targets: vec![TargetRef::Player(P1)],
        sides: 20,
        results: vec![1],
        ignore_rules: vec![],
        results_table: vec![],
        modifier: Some(opponent_power_sum_modifier()),
        die_result: None,
        next_index: 0,
        running_total: 0,
        rolled_any: false,
        forced_ignored: vec![],
        chain_root_targets: vec![TargetRef::Player(P0)],
        trigger_context: None,
        illegal_target_slots: vec![],
        chain_root_target_pins: vec![None],
        // Pre-field frame: no captured verdict, so the legacy path
        // reconstructs local-first from these lists.
        declared_capture: Some(CapturedDeclarationPayload {
            targets: vec![TargetRef::Player(P0)],
            target_pins: vec![None],
            ahead: 0,
        }),
        local_target_pins: vec![],
        keyed_target_incarnations: vec![],
        declared_player: None,
    };
    assert_eq!(
        resumed_die_totals(&mut state, pending),
        vec![6],
        "the resumed read follows the local P1 (1 + 5), not root/base P0 (11)"
    );
}

/// B1a — PAUSE (chain stamp, pre-field frame). With no local player, a
/// frame without a captured verdict falls back to the recorded declared
/// chain and honors the CR 608.2b stamp: declared P1 at stamped slot 0
/// names nobody (1 + 0 = 1) even though the legacy root list shows
/// unstamped P0 at its slot 1. Reading the legacy list would emit 11.
#[test]
fn parked_declared_player_honors_chain_illegal_stamp() {
    use engine::game::scenario::P1;
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature(P0, "Own Brute", 9, 9);
    let foe = scenario.add_creature(P1, "Foe Brute", 5, 5).id();
    let roller = scenario.add_creature(P0, "Roller", 1, 1).id();
    let runner = scenario.build();
    let mut state = runner.state().clone();

    let pending = PendingDieRoll {
        source_id: roller,
        controller: P0,
        roller: P0,
        quantity_controller: None,
        scoped_player: None,
        targets: vec![],
        sides: 20,
        results: vec![1],
        ignore_rules: vec![],
        results_table: vec![],
        modifier: Some(opponent_power_sum_modifier()),
        die_result: None,
        next_index: 0,
        running_total: 0,
        rolled_any: false,
        forced_ignored: vec![],
        chain_root_targets: vec![TargetRef::Object(foe), TargetRef::Player(P0)],
        trigger_context: None,
        illegal_target_slots: vec![0],
        chain_root_target_pins: vec![None, None],
        // Pre-field frame: no captured verdict, so the legacy path
        // reconstructs the stamp check from the declared chain.
        declared_capture: Some(CapturedDeclarationPayload {
            targets: vec![TargetRef::Player(P1)],
            target_pins: vec![None],
            ahead: 0,
        }),
        local_target_pins: vec![],
        keyed_target_incarnations: vec![],
        declared_player: None,
    };
    assert_eq!(
        resumed_die_totals(&mut state, pending),
        vec![1],
        "the stamped chain player names nobody (1 + 0); the legacy list is not consulted"
    );
}

/// B1a — PAUSE (recorded Illegal verdict). A recorded frame answers its
/// captured verdict without consulting any list: with Illegal recorded,
/// the resumed axis matches nothing (1 + 0 = 1) even though the legacy
/// root list shows unstamped P1. List reconstruction would emit 6 — and so
/// would an Undeclared verdict, through the trigger-event fallback the
/// staged `LifeChanged` arms, so this also pins Illegal against a dropped
/// verdict.
#[test]
fn parked_declared_player_recorded_illegal_ignores_lists() {
    use engine::game::scenario::P1;
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature(P1, "Foe Brute", 5, 5);
    let roller = scenario.add_creature(P0, "Roller", 1, 1).id();
    let runner = scenario.build();
    let mut state = runner.state().clone();
    state.current_trigger_event = Some(GameEvent::LifeChanged {
        player_id: P1,
        amount: 0,
        new_total: engine::types::events::LifeTotalReading::default(),
    });

    let pending = PendingDieRoll {
        source_id: roller,
        controller: P0,
        roller: P0,
        quantity_controller: None,
        scoped_player: None,
        targets: vec![],
        sides: 20,
        results: vec![1],
        ignore_rules: vec![],
        results_table: vec![],
        modifier: Some(opponent_power_sum_modifier()),
        die_result: None,
        next_index: 0,
        running_total: 0,
        rolled_any: false,
        forced_ignored: vec![],
        chain_root_targets: vec![TargetRef::Player(P1)],
        trigger_context: None,
        illegal_target_slots: vec![],
        chain_root_target_pins: vec![None],
        declared_capture: Some(CapturedDeclarationPayload {
            targets: vec![],
            target_pins: vec![],
            ahead: 0,
        }),
        local_target_pins: vec![],
        keyed_target_incarnations: vec![],
        declared_player: Some(engine::game::filter::DeclaredPlayerTarget::Illegal),
    };
    assert_eq!(
        resumed_die_totals(&mut state, pending),
        vec![1],
        "the recorded Illegal verdict names nobody (1 + 0); list reconstruction or a dropped verdict would emit 6"
    );
}

/// B1a — PAUSE (compaction parity). The live fast path reads the node's own
/// player without consulting the root-numbered stamp (validation compacts
/// the node before the park); the twin matches: local P1 with a stamp at
/// root slot 0 still resolves P1 (1 + 5 = 6).
#[test]
fn parked_declared_player_ignores_root_numbered_stamp_on_local() {
    use engine::game::scenario::P1;
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature(P1, "Foe Brute", 5, 5);
    let roller = scenario.add_creature(P0, "Roller", 1, 1).id();
    let runner = scenario.build();
    let mut state = runner.state().clone();

    let pending = PendingDieRoll {
        source_id: roller,
        controller: P0,
        roller: P0,
        quantity_controller: None,
        scoped_player: None,
        targets: vec![TargetRef::Player(P1)],
        sides: 20,
        results: vec![1],
        ignore_rules: vec![],
        results_table: vec![],
        modifier: Some(opponent_power_sum_modifier()),
        die_result: None,
        next_index: 0,
        running_total: 0,
        rolled_any: false,
        forced_ignored: vec![],
        chain_root_targets: vec![],
        trigger_context: None,
        illegal_target_slots: vec![0],
        chain_root_target_pins: vec![],
        declared_capture: Some(CapturedDeclarationPayload {
            targets: vec![],
            target_pins: vec![],
            ahead: 0,
        }),
        local_target_pins: vec![],
        keyed_target_incarnations: vec![],
        declared_player: Some(engine::game::filter::DeclaredPlayerTarget::Legal(P1)),
    };
    assert_eq!(
        resumed_die_totals(&mut state, pending),
        vec![6],
        "the local P1 survives the root-numbered stamp (1 + 5), exactly like live"
    );
}

/// B1a — PAUSE (absent control). With no player on any parked list, the twin
/// answers Undeclared and the axis matches nothing (1 + 0 = 1) — never a
/// neighboring list's non-player entry, never the event player.
#[test]
fn parked_declared_player_absent_matches_nothing() {
    use engine::game::scenario::P1;
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature(P1, "Foe Brute", 5, 5);
    let roller = scenario.add_creature(P0, "Roller", 1, 1).id();
    let runner = scenario.build();
    let mut state = runner.state().clone();

    let pending = PendingDieRoll {
        source_id: roller,
        controller: P0,
        roller: P0,
        quantity_controller: None,
        scoped_player: None,
        targets: vec![],
        sides: 20,
        results: vec![1],
        ignore_rules: vec![],
        results_table: vec![],
        modifier: Some(opponent_power_sum_modifier()),
        die_result: None,
        next_index: 0,
        running_total: 0,
        rolled_any: false,
        forced_ignored: vec![],
        chain_root_targets: vec![],
        trigger_context: None,
        illegal_target_slots: vec![],
        chain_root_target_pins: vec![],
        declared_capture: Some(CapturedDeclarationPayload {
            targets: vec![],
            target_pins: vec![],
            ahead: 0,
        }),
        local_target_pins: vec![],
        keyed_target_incarnations: vec![],
        declared_player: Some(engine::game::filter::DeclaredPlayerTarget::Undeclared),
    };
    assert_eq!(
        resumed_die_totals(&mut state, pending),
        vec![1],
        "no parked player means no match (1 + 0)"
    );
}

/// B1-production — the real capture/keep-pause/`SelectDieRolls` boundary
/// for the supported direct axis. A synthetic player-targeting die spell is
/// CAST with Barbarian Class raising the roll; the keep prompt is answered
/// through `GameAction`, and the resumed modifier must resolve the declared
/// player target (P1's 5, not P0's 9). Capture, pause, and resume are all
/// production; only the aggregate's controller is rebound (the parser binds
/// the population to the caster in this position, so the test installs the
/// target-derived axis the boundary must carry — which the constructor
/// accepts, proving the shape admits).
#[test]
fn production_pause_resolves_declared_player_axis() {
    const CENSUS_ROLL: &str =
        "Choose target player. Roll a d20 and add the total power of creatures they control.";
    use engine::game::scenario::P1;
    let mut scenario = GameScenario::new_n_player(2, 49);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature(P0, "Own Brute", 9, 9);
    scenario.add_creature(P1, "Foe Brute", 5, 5);
    scenario.add_enchantment_from_oracle(P0, "Barbarian Class", BARBARIAN_CLASS_L1);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Census Roll", true, CENSUS_ROLL)
        .id();
    let mut runner = scenario.build();
    rebind_census_roll_to_target_axis(runner.state_mut(), spell);

    runner.cast(spell).target_player(P1).commit();
    let top_targets = runner
        .state()
        .stack
        .back()
        .and_then(|entry| entry.ability())
        .map(|ability| ability.targets.clone())
        .unwrap_or_default();
    assert_eq!(
        top_targets,
        vec![TargetRef::Player(P1)],
        "reach-guard: P1 must be declared on the resolving spell"
    );
    runner.resolve_top();

    let WaitingFor::DieKeepChoice {
        player,
        results,
        ignorable_indices,
        ignore_count,
    } = &runner.state().waiting_for
    else {
        panic!(
            "reach-guard: the raised d20s must open a real CR 706.6 ignore choice, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(*player, P0, "the roller answers the keep choice");
    assert_eq!(*ignore_count, 1, "Barbarian ignores one lowest roll");
    assert_eq!(
        ignorable_indices.len(),
        2,
        "both raised dice are offered, got {results:?}"
    );

    let survivor = *results.iter().max().expect("two raised dice");
    let ignore = if results[0] <= results[1] { 0 } else { 1 };
    let result = runner
        .act(GameAction::SelectDieRolls {
            ignore_indices: vec![ignore],
        })
        .expect("submit the keep choice");
    let rolls: Vec<u8> = result
        .events
        .iter()
        .filter_map(|event| match event {
            GameEvent::DieRolled {
                result: Some(total),
                sides: 20,
                ..
            } => Some(*total),
            _ => None,
        })
        .collect();
    assert_eq!(
        rolls,
        vec![survivor + 5],
        "the survivor adds the declared player's power (P1's 5); a dropped binding emits the bare survivor, a P0 misread adds 9"
    );
}

/// B1-production — INLINE control for the paused test above: the same
/// rebound spell cast WITHOUT Barbarian resolves its modifier live (no
/// suspension), so the total must carry the same declared-player addend.
/// The pinned total is seed-49's natural plus P1's 5 (M6 pins dice the same
/// way); a dropped binding emits the bare natural, a P0 misread adds 9.
#[test]
fn inline_census_roll_resolves_declared_player_axis() {
    const CENSUS_ROLL: &str =
        "Choose target player. Roll a d20 and add the total power of creatures they control.";
    use engine::game::scenario::P1;
    let mut scenario = GameScenario::new_n_player(2, 49);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature(P0, "Own Brute", 9, 9);
    scenario.add_creature(P1, "Foe Brute", 5, 5);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Census Roll", true, CENSUS_ROLL)
        .id();
    let mut runner = scenario.build();
    rebind_census_roll_to_target_axis(runner.state_mut(), spell);

    runner.cast(spell).target_player(P1).commit();
    // `resolve_top` drops its events; drive priority through `act` like it
    // does so the inline total stays observable.
    let initial_stack_len = runner.state().stack.len();
    let mut events = Vec::new();
    for _ in 0..10 {
        if runner.state().stack.len() < initial_stack_len {
            break;
        }
        match runner.act(GameAction::PassPriority) {
            Ok(result) => events.extend(result.events),
            Err(_) => break,
        }
    }
    let rolls: Vec<u8> = events
        .iter()
        .filter_map(|event| match event {
            GameEvent::DieRolled {
                result: Some(total),
                sides: 20,
                ..
            } => Some(*total),
            _ => None,
        })
        .collect();
    assert_eq!(
        rolls,
        vec![10],
        "seed 49 rolls a natural 5 and adds the declared player's 5; a dropped binding emits 5, a P0 misread 14"
    );
}

/// Rebind Census Roll's parsed aggregate population from the parser's caster
/// default to the declared-player axis the suspension boundary must carry.
/// The wholesale rebuild through the public constructor doubles as an
/// admission proof: a refused shape would panic here, not at the boundary.
fn rebind_census_roll_to_target_axis(state: &mut GameState, spell: ObjectId) {
    use std::sync::Arc;
    let spell_obj = state.objects.get_mut(&spell).expect("spell in hand");
    let defs = Arc::get_mut(&mut spell_obj.abilities).expect("uniquely owned defs");
    let sub = defs
        .iter_mut()
        .find_map(|def| def.sub_ability.as_mut())
        .expect("Census Roll parses a roll sub-ability");
    let Effect::RollDie {
        modifier: Some(DieRollModifier::Add { value }),
        ..
    } = sub.effect.as_mut()
    else {
        panic!("Census Roll parses an added roll modifier");
    };
    let QuantityExpr::Ref {
        qty: QuantityRef::PropertyAggregate(existing),
    } = value
    else {
        panic!("Census Roll parses an aggregate modifier");
    };
    assert_eq!(
        existing.function(),
        AggregateFunction::Sum,
        "parse-stability guard: still a power sum"
    );
    *value = QuantityExpr::Ref {
        qty: QuantityRef::PropertyAggregate(
            engine::types::ability::PropertyAggregate::new(
                AggregateFunction::Sum,
                ObjectProperty::Power,
                CardTypeSetSource::Objects {
                    filter: TargetFilter::Typed(TypedFilter {
                        type_filters: vec![TypeFilter::Creature],
                        controller: Some(ControllerRef::TargetPlayer),
                        properties: vec![],
                    }),
                },
            )
            .expect("the direct target-player axis admits"),
        ),
    };
}

/// Rebind a modal census roll's added aggregate population from the
/// parser's caster default to the declared-player axis. Walks every mode
/// definition's sub chain for the RollDie node (mode 0 has no roll); the
/// shape assertions fail loudly if the parse drifts, and the count pins
/// that exactly one census roll exists.
fn rebind_modal_census_roll_to_target_axis(state: &mut GameState, spell: ObjectId) {
    use std::sync::Arc;
    let spell_obj = state.objects.get_mut(&spell).expect("spell in hand");
    let defs = Arc::get_mut(&mut spell_obj.abilities).expect("uniquely owned defs");
    let mut rebound = 0;
    for def in defs.iter_mut() {
        let mut node = Some(def);
        while let Some(current) = node {
            if let Effect::RollDie {
                modifier: Some(DieRollModifier::Add { value }),
                ..
            } = current.effect.as_mut()
            {
                let QuantityExpr::Ref {
                    qty: QuantityRef::PropertyAggregate(existing),
                } = value
                else {
                    panic!("modal census roll parses an aggregate modifier");
                };
                assert_eq!(
                    existing.function(),
                    AggregateFunction::Sum,
                    "parse-stability guard: still a power sum"
                );
                *value = QuantityExpr::Ref {
                    qty: QuantityRef::PropertyAggregate(
                        engine::types::ability::PropertyAggregate::new(
                            AggregateFunction::Sum,
                            ObjectProperty::Power,
                            CardTypeSetSource::Objects {
                                filter: TargetFilter::Typed(TypedFilter {
                                    type_filters: vec![TypeFilter::Creature],
                                    controller: Some(ControllerRef::TargetPlayer),
                                    properties: vec![],
                                }),
                            },
                        )
                        .expect("the direct target-player axis admits"),
                    ),
                };
                rebound += 1;
            }
            node = current.sub_ability.as_mut().map(|boxed| boxed.as_mut());
        }
    }
    assert_eq!(
        rebound, 1,
        "exactly one census roll across the modal spell's modes"
    );
}

/// B1-production — MEDIUM 1. CR 608.2c: a resumed instruction reads the same
/// player it would have read without the pause. Mode 0 declares P0 and mode
/// 1 declares P1 plus the pumped foe; the roll node inherits only the foe,
/// so live reads root-first P0 while a mode-relative re-derivation would
/// read P1. The keep pause plus `SelectDieRolls` must resolve the resumed
/// total with P0's 10 (9 + 1), exactly like the inline control below — a
/// mode-relative misread adds P1's 5 instead (6 after the pump).
#[test]
fn modal_pause_resolves_root_player_not_mode_player() {
    const MODAL_CENSUS_ROLL: &str = "Choose two \u{2014}\n\u{2022} Target player gains 1 life.\n\u{2022} Choose target player. Target creature gets +1/+1 until end of turn. Roll a d20 and add the total power of creatures they control.";
    use engine::game::scenario::P1;
    let mut scenario = GameScenario::new_n_player(2, 49);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature(P0, "Own Brute", 9, 9);
    let foe = scenario.add_creature(P1, "Foe Brute", 5, 5).id();
    scenario.add_creature(P0, "Roller", 1, 1);
    scenario.add_enchantment_from_oracle(P0, "Barbarian Class", BARBARIAN_CLASS_L1);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Modal Census Roll", true, MODAL_CENSUS_ROLL)
        .id();
    let mut runner = scenario.build();
    rebind_modal_census_roll_to_target_axis(runner.state_mut(), spell);

    runner
        .cast(spell)
        .modes(&[0, 1])
        .target_players(&[P0, P1])
        .target_object(foe)
        .commit();
    let mut chain_targets = Vec::new();
    let mut node = runner
        .state()
        .stack
        .back()
        .and_then(|entry| entry.ability());
    while let Some(current) = node {
        chain_targets.extend(current.targets.iter().cloned());
        node = current.sub_ability.as_deref();
    }
    assert_eq!(
        chain_targets,
        vec![
            TargetRef::Player(P0),
            TargetRef::Player(P1),
            TargetRef::Object(foe),
        ],
        "reach-guard: mode 0 declares P0, mode 1 declares P1 and the foe, in written order"
    );
    runner.resolve_top();

    let WaitingFor::DieKeepChoice {
        player,
        results,
        ignorable_indices,
        ignore_count,
    } = &runner.state().waiting_for
    else {
        panic!(
            "reach-guard: the raised d20s must open a real CR 706.6 ignore choice, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(*player, P0, "the roller answers the keep choice");
    assert_eq!(*ignore_count, 1, "Barbarian ignores one lowest roll");
    assert_eq!(
        ignorable_indices.len(),
        2,
        "both raised dice are offered, got {results:?}"
    );

    let survivor = *results.iter().max().expect("two raised dice");
    let ignore = if results[0] <= results[1] { 0 } else { 1 };
    let result = runner
        .act(GameAction::SelectDieRolls {
            ignore_indices: vec![ignore],
        })
        .expect("submit the keep choice");
    let rolls: Vec<u8> = result
        .events
        .iter()
        .filter_map(|event| match event {
            GameEvent::DieRolled {
                result: Some(total),
                sides: 20,
                ..
            } => Some(*total),
            _ => None,
        })
        .collect();
    assert_eq!(
        rolls,
        vec![survivor + 10],
        "the survivor adds the root-first player's power (P0's 9 + 1); a mode-relative misread adds P1's pumped 6"
    );
    assert_eq!(
        runner.state().objects[&foe].power,
        Some(6),
        "reach-guard: mode 1's pump bound the foe, so the roll node holds only objects"
    );
    assert!(
        runner.state().stack.is_empty(),
        "completion: the modal spell finishes after the resumed roll"
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "completion: priority resumes, got {:?}",
        runner.state().waiting_for
    );
}

/// B1-production — INLINE control for the paused modal test above: the same
/// rebound spell cast WITHOUT Barbarian resolves its modifier live (no
/// suspension), so the total must carry the same root-first addend. The
/// pinned total is seed-49's natural plus P0's 10; the two tests agree if
/// and only if pause and live read the same player.
#[test]
fn modal_inline_roll_reads_root_player() {
    const MODAL_CENSUS_ROLL: &str = "Choose two \u{2014}\n\u{2022} Target player gains 1 life.\n\u{2022} Choose target player. Target creature gets +1/+1 until end of turn. Roll a d20 and add the total power of creatures they control.";
    use engine::game::scenario::P1;
    let mut scenario = GameScenario::new_n_player(2, 49);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature(P0, "Own Brute", 9, 9);
    let foe = scenario.add_creature(P1, "Foe Brute", 5, 5).id();
    scenario.add_creature(P0, "Roller", 1, 1);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Modal Census Roll", true, MODAL_CENSUS_ROLL)
        .id();
    let mut runner = scenario.build();
    rebind_modal_census_roll_to_target_axis(runner.state_mut(), spell);

    runner
        .cast(spell)
        .modes(&[0, 1])
        .target_players(&[P0, P1])
        .target_object(foe)
        .commit();
    let initial_stack_len = runner.state().stack.len();
    let mut events = Vec::new();
    for _ in 0..10 {
        if runner.state().stack.len() < initial_stack_len {
            break;
        }
        match runner.act(GameAction::PassPriority) {
            Ok(result) => events.extend(result.events),
            Err(_) => break,
        }
    }
    let rolls: Vec<u8> = events
        .iter()
        .filter_map(|event| match event {
            GameEvent::DieRolled {
                result: Some(total),
                sides: 20,
                ..
            } => Some(*total),
            _ => None,
        })
        .collect();
    assert_eq!(
        rolls,
        vec![15],
        "seed 49 rolls a natural 5 and adds the root-first player's 10; a mode-relative misread adds 6"
    );
}

/// Sum of power over creatures the target opponent controls — the shared
/// direct-axis modifier for the B1a parked-player probes.
fn opponent_power_sum_modifier() -> DieRollModifier {
    DieRollModifier::Add {
        value: QuantityExpr::Ref {
            qty: QuantityRef::PropertyAggregate(
                engine::types::ability::PropertyAggregate::new(
                    AggregateFunction::Sum,
                    ObjectProperty::Power,
                    CardTypeSetSource::Objects {
                        filter: TargetFilter::Typed(TypedFilter {
                            type_filters: vec![TypeFilter::Creature],
                            controller: Some(ControllerRef::TargetOpponent),
                            properties: vec![],
                        }),
                    },
                )
                .expect("statically valid property aggregate"),
            ),
        },
    }
}

/// Drive `pending` through the real resume entry and collect emitted totals.
fn resumed_die_totals(state: &mut GameState, pending: PendingDieRoll) -> Vec<u8> {
    // Production resume: park the crafted frame on the resolution stack and
    // answer the keep prompt through the public action path, ignoring nothing.
    // Every fixture stages an empty forced set, so the production
    // forced-union is a no-op and this reads exactly the direct-resume path.
    let roller = pending.roller;
    let results = pending.results.clone();
    state.push_die_roll_frame(pending);
    state.waiting_for = WaitingFor::DieKeepChoice {
        player: roller,
        results,
        ignorable_indices: Vec::new(),
        ignore_count: 0,
    };
    let outcome = engine::game::engine::apply(
        state,
        roller,
        GameAction::SelectDieRolls {
            ignore_indices: Vec::new(),
        },
    )
    .expect("resuming the parked roll must not error");
    outcome
        .events
        .iter()
        .filter_map(|event| match event {
            GameEvent::DieRolled {
                result: Some(total),
                ..
            } => Some(*total),
            _ => None,
        })
        .collect()
}

/// M6: production-created Song pause. Parsed Song is CAST with Barbarian
/// Class's supported die replacement on board, driven manually (commit +
/// priority) because the cast auto-driver answers keep prompts
/// deterministically. Seed 49 ties the raised d20s at [5, 5] (seed search
/// over 42..400 found 16 tied seeds; this one keeps natural 5 + MV 7 = 12
/// inside the supported 1-14 row, clear of the honestly-red 15+ lifegain).
/// Pins the actual prompt/actor/options, answers through `GameAction`, and
/// verifies the modifier total, row delivery, priority, and a completed
/// continuation — the capture seam at `roll_die.rs:213` exercised without
/// any hand-installed suspension state.
#[test]
fn song_keep_choice_pause_is_production_created() {
    const BARBARIAN_CLASS_L1: &str = "If you would roll one or more dice, instead roll that many \
        dice plus one and ignore the lowest roll.";
    let mut scenario = GameScenario::new_n_player(2, 49);
    scenario.at_phase(Phase::PreCombatMain);
    let oak = scenario
        .add_creature_to_graveyard(P0, "Oak", 2, 3)
        .with_mana_cost(mv_cost(3))
        .id();
    let elm = scenario
        .add_creature_to_graveyard(P0, "Elm", 2, 4)
        .with_mana_cost(mv_cost(4))
        .id();
    scenario.add_enchantment_from_oracle(P0, "Barbarian Class", BARBARIAN_CLASS_L1);
    let song = scenario
        .add_spell_to_hand_from_oracle(P0, "Song of Inspiration", true, SONG)
        .id();
    let mut runner = scenario.build();

    runner.cast(song).target_objects(&[oak, elm]).commit();
    runner.resolve_top();

    // The actual prompt, actor, and options off the production path.
    let WaitingFor::DieKeepChoice {
        player,
        results,
        ignorable_indices,
        ignore_count,
    } = &runner.state().waiting_for
    else {
        panic!(
            "reach-guard: the tied d20s must open a real CR 706.6 ignore choice, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(*player, P0, "the roller answers the keep choice");
    assert_eq!(*results, vec![5, 5], "seed 49 ties the raised d20s at 5");
    assert_eq!(*ignorable_indices, vec![0, 1], "both tied dice are offered");
    assert_eq!(*ignore_count, 1, "Barbarian ignores one lowest roll");
    assert_eq!(
        runner.state().objects[&oak].zone,
        Zone::Graveyard,
        "reach-guard: the row must not deliver while the choice is open"
    );

    let result = runner
        .act(GameAction::SelectDieRolls {
            ignore_indices: vec![0],
        })
        .expect("submit the keep choice");
    let rolls: Vec<u8> = result
        .events
        .iter()
        .filter_map(|event| match event {
            GameEvent::DieRolled {
                result: Some(total),
                sides: 20,
                ..
            } => Some(*total),
            _ => None,
        })
        .collect();
    assert_eq!(
        rolls,
        vec![12],
        "CR 706.2: the survivor's final number is its natural 5 plus the \
         parked targets' total MV (3 + 4); a dropped binding reads 0 and emits 5"
    );

    runner.advance_until_stack_empty();
    assert_eq!(
        runner.state().objects[&oak].zone,
        Zone::Hand,
        "the 1-14 row returns Oak to hand"
    );
    assert_eq!(
        runner.state().objects[&elm].zone,
        Zone::Hand,
        "the 1-14 row returns Elm to hand"
    );
    assert_eq!(
        runner.state().players[P0.0 as usize].life,
        20,
        "total 12 stays in the 1-14 row: the 15+ lifegain must not fire"
    );
    assert_eq!(
        runner.state().objects[&song].zone,
        Zone::Graveyard,
        "the resolved instant rests in the graveyard"
    );
    assert!(
        runner.state().stack.is_empty(),
        "the continuation completes: the stack is empty"
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "expected a clean return to priority, got {:?}",
        runner.state().waiting_for
    );
    assert_eq!(
        runner.state().die_result_this_resolution,
        None,
        "post-completion restore discipline: the inner aggregate must not leak \
         into the outer context once the instruction finishes"
    );
}

/// MED2 — Barbarian Class level 1 (copied from
/// `barbarian_class_die_roll_replacement.rs`, whose const is private): the
/// roll-raiser that forces a deterministic d1 tie and a real keep-prompt
/// pause below.
const BARBARIAN_CLASS_L1: &str = "If you would roll one or more dice, instead roll that many \
dice plus one and ignore the lowest roll.";

/// MED2 — `Sum(Power, TargetedObjects { slot: Some(0) })` as a roll addend:
/// the indexed slot read whose capture presence is under test.
fn med2_slot_zero_power_modifier() -> DieRollModifier {
    DieRollModifier::Add {
        value: QuantityExpr::Ref {
            qty: QuantityRef::PropertyAggregate(
                engine::types::ability::PropertyAggregate::new(
                    AggregateFunction::Sum,
                    ObjectProperty::Power,
                    CardTypeSetSource::TargetedObjects { slot: Some(0) },
                )
                .expect("statically valid property aggregate"),
            ),
        },
    }
}

/// MED2 — the single d1 `DieRolled` total. Reach-guarded like `d20_total`:
/// exactly one d1 roll must have happened.
fn d1_total(events: &[GameEvent]) -> u8 {
    let mut totals = events.iter().filter_map(|event| match event {
        GameEvent::DieRolled {
            result: Some(total),
            sides: 1,
            ..
        } => Some(*total),
        _ => None,
    });
    let total = totals.next().expect("the cast must roll exactly one d1");
    assert!(totals.next().is_none(), "the cast must roll exactly one d1");
    total
}

/// MED2 PRIMARY — an authoritative empty chosen-slot list must stay empty
/// across a REAL keep-prompt pause. `Recorded([])` (no declared slots) plus
/// a held-but-unannounced 5-power local object: the live read is 0, and the
/// resumed read must also be 0 — the held object must not backfill slot 0.
/// Pre-fix the resume promotes the held object and totals 6 instead of 1.
#[test]
fn recorded_empty_slot_ignores_held_local_across_a_real_keep_pause() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let held = scenario.add_creature(P0, "Held", 5, 5).id();
    let roller = scenario.add_creature(P0, "Roller", 1, 1).id();
    scenario.add_enchantment_from_oracle(P0, "Barbarian Class", BARBARIAN_CLASS_L1);
    let mut runner = scenario.build();

    let mut ability = ResolvedAbility::new(
        Effect::RollDie {
            count: QuantityExpr::Fixed { value: 1 },
            sides: 1,
            results: vec![],
            modifier: Some(med2_slot_zero_power_modifier()),
        },
        vec![],
        roller,
        P0,
    );
    engine::game::ability_utils::assign_targets_in_chain(runner.state(), &mut ability, &[])
        .expect("assigning no targets to a sinkless roll records");
    assert_eq!(
        ability.chosen_target_slots,
        ChosenTargetSlots::Recorded(vec![]),
        "NON-VACUITY: the no-target sink must record an authoritative empty list"
    );
    ability.push_target(TargetRef::Object(held));
    assert_eq!(
        ability.targets,
        vec![TargetRef::Object(held)],
        "NON-VACUITY: the held object rides the local list"
    );
    assert_eq!(
        ability.chosen_target_slots,
        ChosenTargetSlots::Recorded(vec![]),
        "NON-VACUITY: the unannounced push must not rewrite the recorded choice"
    );

    let mut events = Vec::new();
    engine::game::effects::roll_die::resolve(runner.state_mut(), &ability, &mut events)
        .expect("the raised roll proposes");
    match runner.state().waiting_for.clone() {
        WaitingFor::DieKeepChoice {
            results,
            ignorable_indices,
            ignore_count,
            ..
        } => {
            assert_eq!(
                results,
                vec![1, 1],
                "REACH-GUARD: d1 plus Barbarian must tie and open the keep prompt"
            );
            assert_eq!(ignorable_indices, vec![0, 1]);
            assert_eq!(ignore_count, 1);
        }
        other => panic!("REACH-GUARD: the raised tie must suspend on DieKeepChoice, got {other:?}"),
    }
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, GameEvent::DieRolled { .. })),
        "CR 706.6: nothing is emitted while the ignore choice is open"
    );

    let outcome = runner
        .act(GameAction::SelectDieRolls {
            ignore_indices: vec![0],
        })
        .expect("the roller may ignore a tied-lowest roll");
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "expected a clean return to priority, got {:?}",
        runner.state().waiting_for
    );
    assert_eq!(
        d1_total(&outcome.events),
        1,
        "MED2: authoritative empty slot 0 reads 0 after the pause (natural 1 + 0); \
         the held 5-power object must not backfill it"
    );
}

/// MED2 CONTROL — the same Recorded-empty plus held setup WITHOUT a pause
/// reads 0 through the live slot authority: the unraised d1 never prompts,
/// and the total is the bare natural. Green pre- and post-fix; guards the
/// instrument the primary test measures against.
#[test]
fn recorded_empty_slot_reads_zero_inline_without_a_pause() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let held = scenario.add_creature(P0, "Held", 5, 5).id();
    let roller = scenario.add_creature(P0, "Roller", 1, 1).id();
    let mut runner = scenario.build();

    let mut ability = ResolvedAbility::new(
        Effect::RollDie {
            count: QuantityExpr::Fixed { value: 1 },
            sides: 1,
            results: vec![],
            modifier: Some(med2_slot_zero_power_modifier()),
        },
        vec![],
        roller,
        P0,
    );
    engine::game::ability_utils::assign_targets_in_chain(runner.state(), &mut ability, &[])
        .expect("assigning no targets to a sinkless roll records");
    ability.push_target(TargetRef::Object(held));

    let mut events = Vec::new();
    engine::game::effects::roll_die::resolve(runner.state_mut(), &ability, &mut events)
        .expect("the unreplaced roll resolves");
    assert!(
        !matches!(runner.state().waiting_for, WaitingFor::DieKeepChoice { .. }),
        "REACH-GUARD: the unraised d1 cannot tie and must not prompt, got {:?}",
        runner.state().waiting_for
    );
    assert_eq!(
        d1_total(&events),
        1,
        "the live slot authority reads Recorded([]) as no target (natural 1 + 0)"
    );
}

/// MED2 CONTROL — a legacy frame whose capture is genuinely ABSENT (empty
/// declared context, populated local list) keeps the local fallback: the
/// resumed total is the natural plus the held power. Production always
/// captures now, so the frame and prompt are staged — but the prompt answer
/// and the resume are real. Green pre- and post-fix; guards the fallback
/// the fix must preserve.
#[test]
fn legacy_frame_without_capture_still_falls_back_to_local() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let held = scenario.add_creature(P0, "Held", 5, 5).id();
    let roller = scenario.add_creature(P0, "Roller", 1, 1).id();
    let mut runner = scenario.build();

    runner.state_mut().push_die_roll_frame(PendingDieRoll {
        source_id: roller,
        controller: P0,
        roller: P0,
        quantity_controller: None,
        scoped_player: None,
        targets: vec![TargetRef::Object(held)],
        sides: 1,
        results: vec![1, 1],
        ignore_rules: vec![],
        results_table: vec![],
        modifier: Some(med2_slot_zero_power_modifier()),
        die_result: None,
        next_index: 0,
        running_total: 0,
        rolled_any: false,
        forced_ignored: vec![],
        chain_root_targets: vec![],
        trigger_context: None,
        illegal_target_slots: vec![],
        chain_root_target_pins: vec![],
        declared_capture: None,
        local_target_pins: vec![],
        keyed_target_incarnations: vec![],
        declared_player: None,
    });
    runner.state_mut().waiting_for = WaitingFor::DieKeepChoice {
        player: P0,
        results: vec![1, 1],
        ignorable_indices: vec![0, 1],
        ignore_count: 1,
    };

    let outcome = runner
        .act(GameAction::SelectDieRolls {
            ignore_indices: vec![1],
        })
        .expect("the staged keep prompt answers");
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "expected a clean return to priority, got {:?}",
        runner.state().waiting_for
    );
    assert_eq!(
        d1_total(&outcome.events),
        6,
        "legacy absent capture keeps the local fallback (natural 1 + held 5)"
    );
}

/// MED2 CONTROL — a Recorded-NONEMPTY slot reads through the same real
/// keep-prompt pause: slot 0 names the recorded brute, and the resumed
/// total carries its power. Green pre- and post-fix; guards the preferred
/// declared arm the fix must not disturb.
#[test]
fn recorded_nonempty_slot_reads_through_a_real_keep_pause() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let brute = scenario.add_creature(P0, "Brute", 5, 5).id();
    let roller = scenario.add_creature(P0, "Roller", 1, 1).id();
    scenario.add_enchantment_from_oracle(P0, "Barbarian Class", BARBARIAN_CLASS_L1);
    let mut runner = scenario.build();

    let mut ability = ResolvedAbility::new(
        Effect::RollDie {
            count: QuantityExpr::Fixed { value: 1 },
            sides: 1,
            results: vec![],
            modifier: Some(med2_slot_zero_power_modifier()),
        },
        vec![],
        roller,
        P0,
    );
    engine::game::ability_utils::assign_targets_in_chain(
        runner.state(),
        &mut ability,
        &[TargetRef::Object(brute)],
    )
    .expect("assigning the brute to a sinkless roll records");
    assert_eq!(
        ability.chosen_target_slots,
        ChosenTargetSlots::Recorded(vec![0]),
        "NON-VACUITY: the sinkless assignment must record slot 0"
    );

    let mut events = Vec::new();
    engine::game::effects::roll_die::resolve(runner.state_mut(), &ability, &mut events)
        .expect("the raised roll proposes");
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::DieKeepChoice { .. }),
        "REACH-GUARD: d1 plus Barbarian must tie and open the keep prompt, got {:?}",
        runner.state().waiting_for
    );

    let outcome = runner
        .act(GameAction::SelectDieRolls {
            ignore_indices: vec![0],
        })
        .expect("the roller may ignore a tied-lowest roll");
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "expected a clean return to priority, got {:?}",
        runner.state().waiting_for
    );
    assert_eq!(
        d1_total(&outcome.events),
        6,
        "Recorded slot 0 names the brute through the pause (natural 1 + 5)"
    );
}

/// R3-A — ChainSet power sum narrowed by the direct target-player axis, for
/// fixtures pinning the snapshot/record declared-player twin.
fn chainset_target_player_sum_modifier() -> DieRollModifier {
    DieRollModifier::Add {
        value: QuantityExpr::Ref {
            qty: QuantityRef::PropertyAggregate(
                PropertyAggregate::new(
                    AggregateFunction::Sum,
                    ObjectProperty::Power,
                    CardTypeSetSource::TrackedSet {
                        set: TrackedAnaphorSource::ChainSet,
                        caused_by: None,
                        filter: Some(TargetFilter::Typed(TypedFilter {
                            type_filters: vec![TypeFilter::Creature],
                            controller: Some(ControllerRef::TargetPlayer),
                            properties: vec![],
                        })),
                    },
                )
                .expect("the direct target-player axis admits"),
            ),
        },
    }
}

/// R3-B — power sum over creatures the PRINTED controller controls.
fn you_power_sum_modifier() -> DieRollModifier {
    DieRollModifier::Add {
        value: QuantityExpr::Ref {
            qty: QuantityRef::PropertyAggregate(
                PropertyAggregate::new(
                    AggregateFunction::Sum,
                    ObjectProperty::Power,
                    CardTypeSetSource::Objects {
                        filter: TargetFilter::Typed(TypedFilter {
                            type_filters: vec![TypeFilter::Creature],
                            controller: Some(ControllerRef::You),
                            properties: vec![],
                        }),
                    },
                )
                .expect("statically valid property aggregate"),
            ),
        },
    }
}

/// R4-B1 — power sum over creatures the SCOPED seat controls.
fn scoped_power_sum_modifier() -> DieRollModifier {
    DieRollModifier::Add {
        value: QuantityExpr::Ref {
            qty: QuantityRef::PropertyAggregate(
                PropertyAggregate::new(
                    AggregateFunction::Sum,
                    ObjectProperty::Power,
                    CardTypeSetSource::Objects {
                        filter: TargetFilter::Typed(TypedFilter {
                            type_filters: vec![TypeFilter::Creature],
                            controller: Some(ControllerRef::ScopedPlayer),
                            properties: vec![],
                        }),
                    },
                )
                .expect("statically valid property aggregate"),
            ),
        },
    }
}

/// R3-A — stage `{victim}` as the latest tracked set with a
/// Battlefield-departure origin naming `incarnation`. The SNAPSHOT behind
/// the origin must be real (stamped by a production departure); the set is
/// the population under test.
fn stage_tracked_departure_set(
    state: &mut GameState,
    victim: ObjectId,
    incarnation: u64,
) -> TrackedSetId {
    let set_id = TrackedSetId(state.next_tracked_set_id);
    state.next_tracked_set_id += 1;
    state.tracked_object_sets.insert(set_id, vec![victim]);
    state.tracked_set_member_origins.insert(
        set_id,
        [(
            victim,
            TrackedMemberOrigin {
                departed_from: Zone::Battlefield,
                departure_incarnation: incarnation,
            },
        )]
        .into_iter()
        .collect(),
    );
    set_id
}

/// R3-A PAUSE — a snapshot-backed member filtered by the direct
/// target-player axis reads the PARKED declared player after a production
/// keep pause. The victim's stamped departure (power 5, controller P1) plus
/// legal P1 contributes 5; pre-fix the record arm ignored the frame and
/// contributed 0 (totals 6 vs 1).
#[test]
fn snapshot_pause_reads_parked_declared_player() {
    use engine::game::scenario::P1;
    const MURDER: &str = "Destroy target creature.";
    let mut scenario = GameScenario::new_n_player(2, 49);
    scenario.at_phase(Phase::PreCombatMain);
    let doomed = scenario.add_creature(P1, "Doomed Brute", 5, 5).id();
    let roller = scenario.add_creature(P0, "Roller", 1, 1).id();
    scenario.add_enchantment_from_oracle(P0, "Barbarian Class", BARBARIAN_CLASS_L1);
    let murder = scenario
        .add_spell_to_hand_from_oracle(P0, "Murder", true, MURDER)
        .id();
    let mut runner = scenario.build();

    runner.cast(murder).target_object(doomed).commit();
    runner.resolve_top();
    assert!(
        !runner.state().battlefield.contains(&doomed),
        "reach-guard: murder kills through production"
    );
    let history = runner
        .state()
        .lki_by_incarnation
        .get(&doomed)
        .expect("reach-guard: the departure stamps LKI");
    let incarnation = *history.keys().max().expect("a departure incarnation");
    let snap = history.get(&incarnation).expect("the stamped snapshot");
    assert_eq!(snap.power, Some(5), "the snapshot keeps departure power");
    assert_eq!(snap.controller, P1, "the snapshot keeps its controller");

    stage_tracked_departure_set(runner.state_mut(), doomed, incarnation);
    let sets_before_roll = runner.state().tracked_object_sets.len();

    let mut ability = ResolvedAbility::new(
        Effect::RollDie {
            count: QuantityExpr::Fixed { value: 1 },
            sides: 1,
            results: vec![],
            modifier: Some(chainset_target_player_sum_modifier()),
        },
        vec![],
        roller,
        P0,
    );
    engine::game::ability_utils::assign_targets_in_chain(
        runner.state(),
        &mut ability,
        &[TargetRef::Player(P1)],
    )
    .expect("declaring P1 records");
    let mut events = Vec::new();
    engine::game::effects::roll_die::resolve(runner.state_mut(), &ability, &mut events)
        .expect("the raised roll proposes");
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::DieKeepChoice { .. }),
        "REACH-GUARD: d1 plus Barbarian must open the keep prompt, got {:?}",
        runner.state().waiting_for
    );
    let outcome = runner
        .act(GameAction::SelectDieRolls {
            ignore_indices: vec![0],
        })
        .expect("the roller may ignore a tied-lowest roll");
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "completion: priority resumes, got {:?}",
        runner.state().waiting_for
    );
    assert_eq!(
        runner.state().tracked_object_sets.len(),
        sets_before_roll,
        "premise-guard: nothing publishes a newer set mid-roll, so ChainSet is staged"
    );
    assert_eq!(
        d1_total(&outcome.events),
        6,
        "survivor 1 + departure power 5; a parked-blind resume gives 1"
    );
}

/// R3-A INLINE control — the same staged departure without Barbarian
/// resolves live (no suspension), so the total must carry the same addend.
/// The two tests agree if and only if pause and live read the same player.
#[test]
fn snapshot_inline_roll_reads_declared_player() {
    use engine::game::scenario::P1;
    const MURDER: &str = "Destroy target creature.";
    let mut scenario = GameScenario::new_n_player(2, 49);
    scenario.at_phase(Phase::PreCombatMain);
    let doomed = scenario.add_creature(P1, "Doomed Brute", 5, 5).id();
    let roller = scenario.add_creature(P0, "Roller", 1, 1).id();
    let murder = scenario
        .add_spell_to_hand_from_oracle(P0, "Murder", true, MURDER)
        .id();
    let mut runner = scenario.build();

    runner.cast(murder).target_object(doomed).commit();
    runner.resolve_top();
    let incarnation = *runner
        .state()
        .lki_by_incarnation
        .get(&doomed)
        .expect("stamped departure")
        .keys()
        .max()
        .expect("a departure incarnation");
    stage_tracked_departure_set(runner.state_mut(), doomed, incarnation);

    let mut ability = ResolvedAbility::new(
        Effect::RollDie {
            count: QuantityExpr::Fixed { value: 1 },
            sides: 1,
            results: vec![],
            modifier: Some(chainset_target_player_sum_modifier()),
        },
        vec![],
        roller,
        P0,
    );
    engine::game::ability_utils::assign_targets_in_chain(
        runner.state(),
        &mut ability,
        &[TargetRef::Player(P1)],
    )
    .expect("declaring P1 records");
    let mut events = Vec::new();
    engine::game::effects::roll_die::resolve(runner.state_mut(), &ability, &mut events)
        .expect("the raised roll resolves");
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "no Barbarian means no pause, got {:?}",
        runner.state().waiting_for
    );
    assert_eq!(
        d1_total(&events),
        6,
        "inline survivor 1 + departure power 5, matching the paused read"
    );
}

/// R3-A CONTROL — a legal but NONMATCHING declared player contributes
/// nothing through the same production pause: P0 is chosen, the snapshot is
/// P1's, the filter rejects. Green pre- and post-fix; guards the rejecting
/// arm the fix must not disturb.
#[test]
fn snapshot_pause_nonmatching_player_contributes_nothing() {
    use engine::game::scenario::P1;
    const MURDER: &str = "Destroy target creature.";
    let mut scenario = GameScenario::new_n_player(2, 49);
    scenario.at_phase(Phase::PreCombatMain);
    let doomed = scenario.add_creature(P1, "Doomed Brute", 5, 5).id();
    let roller = scenario.add_creature(P0, "Roller", 1, 1).id();
    scenario.add_enchantment_from_oracle(P0, "Barbarian Class", BARBARIAN_CLASS_L1);
    let murder = scenario
        .add_spell_to_hand_from_oracle(P0, "Murder", true, MURDER)
        .id();
    let mut runner = scenario.build();

    runner.cast(murder).target_object(doomed).commit();
    runner.resolve_top();
    let incarnation = *runner
        .state()
        .lki_by_incarnation
        .get(&doomed)
        .expect("stamped departure")
        .keys()
        .max()
        .expect("a departure incarnation");
    stage_tracked_departure_set(runner.state_mut(), doomed, incarnation);

    let mut ability = ResolvedAbility::new(
        Effect::RollDie {
            count: QuantityExpr::Fixed { value: 1 },
            sides: 1,
            results: vec![],
            modifier: Some(chainset_target_player_sum_modifier()),
        },
        vec![],
        roller,
        P0,
    );
    engine::game::ability_utils::assign_targets_in_chain(
        runner.state(),
        &mut ability,
        &[TargetRef::Player(P0)],
    )
    .expect("declaring P0 records");
    let mut events = Vec::new();
    engine::game::effects::roll_die::resolve(runner.state_mut(), &ability, &mut events)
        .expect("the raised roll proposes");
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::DieKeepChoice { .. }),
        "REACH-GUARD: the pause must still open, got {:?}",
        runner.state().waiting_for
    );
    let outcome = runner
        .act(GameAction::SelectDieRolls {
            ignore_indices: vec![0],
        })
        .expect("the roller may ignore a tied-lowest roll");
    assert_eq!(
        d1_total(&outcome.events),
        1,
        "survivor 1 + 0: P0 is legal but the snapshot is P1's"
    );
}

/// R3-A CONTROL (staged Illegal) — a recorded Illegal verdict on the parked
/// frame matches nothing on the record arm (1 + 0 = 1) even though the
/// staged `LifeChanged` arms the trigger-event fallback the Undeclared arm
/// would take (which would emit 6). Green pre- and post-fix; pins Illegal
/// against a dropped verdict on the snapshot path.
#[test]
fn snapshot_pause_illegal_player_contributes_nothing() {
    use engine::game::scenario::P1;
    const MURDER: &str = "Destroy target creature.";
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let doomed = scenario.add_creature(P1, "Doomed Brute", 5, 5).id();
    let roller = scenario.add_creature(P0, "Roller", 1, 1).id();
    let murder = scenario
        .add_spell_to_hand_from_oracle(P0, "Murder", true, MURDER)
        .id();
    let mut runner = scenario.build();

    runner.cast(murder).target_object(doomed).commit();
    runner.resolve_top();
    let incarnation = *runner
        .state()
        .lki_by_incarnation
        .get(&doomed)
        .expect("stamped departure")
        .keys()
        .max()
        .expect("a departure incarnation");
    stage_tracked_departure_set(runner.state_mut(), doomed, incarnation);

    let mut state = runner.state().clone();
    state.current_trigger_event = Some(GameEvent::LifeChanged {
        player_id: P1,
        amount: 0,
        new_total: engine::types::events::LifeTotalReading::default(),
    });
    let pending = PendingDieRoll {
        source_id: roller,
        controller: P0,
        roller: P0,
        quantity_controller: None,
        scoped_player: None,
        targets: vec![],
        sides: 1,
        results: vec![1],
        ignore_rules: vec![],
        results_table: vec![],
        modifier: Some(chainset_target_player_sum_modifier()),
        die_result: None,
        next_index: 0,
        running_total: 0,
        rolled_any: false,
        forced_ignored: vec![],
        chain_root_targets: vec![TargetRef::Player(P1)],
        trigger_context: None,
        illegal_target_slots: vec![0],
        chain_root_target_pins: vec![None],
        declared_capture: Some(CapturedDeclarationPayload {
            targets: vec![TargetRef::Player(P1)],
            target_pins: vec![None],
            ahead: 0,
        }),
        local_target_pins: vec![],
        keyed_target_incarnations: vec![],
        declared_player: Some(engine::game::filter::DeclaredPlayerTarget::Illegal),
    };
    assert_eq!(
        resumed_die_totals(&mut state, pending),
        vec![1],
        "the recorded Illegal verdict names nobody (1 + 0); a dropped verdict would emit 6"
    );
}

/// R3-B PAUSE — a P0-owned roll rebound to opponent seat P1 (printed P0
/// retained, acting + scoped seat P1 — the bind_scoped_seat shape) with a
/// You-sum modifier over asymmetric populations (P0 9, P1 5) must add P0's 9
/// after a production keep pause, exactly like the inline path. Pre-fix the
/// resumed modifier read the acting seat: totals 10 vs 6.
#[test]
fn scoped_pause_reads_printed_controller_not_seat() {
    use engine::game::scenario::P1;
    let mut scenario = GameScenario::new_n_player(2, 49);
    scenario.at_phase(Phase::PreCombatMain);
    let own = scenario.add_creature(P0, "Own Brute", 9, 9).id();
    scenario.add_creature(P1, "Foe Brute", 5, 5);
    // Barbarian sits with the SEAT (P1): the rebound roll belongs to P1, so
    // only P1's replacement opens the production keep prompt.
    scenario.add_enchantment_from_oracle(P1, "Barbarian Class", BARBARIAN_CLASS_L1);
    let mut runner = scenario.build();

    let mut ability = ResolvedAbility::new(
        Effect::RollDie {
            count: QuantityExpr::Fixed { value: 1 },
            sides: 1,
            results: vec![],
            modifier: Some(you_power_sum_modifier()),
        },
        vec![],
        own,
        P0,
    );
    ability.set_original_controller_recursive(P0);
    ability.set_controller_recursive(P1);
    ability.set_scoped_player_recursive(P1);
    let mut events = Vec::new();
    engine::game::effects::roll_die::resolve(runner.state_mut(), &ability, &mut events)
        .expect("the raised roll proposes");
    let WaitingFor::DieKeepChoice { player, .. } = &runner.state().waiting_for else {
        panic!(
            "REACH-GUARD: d1 plus Barbarian must open the keep prompt, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(*player, P1, "the P1 seat answers the keep choice");
    let outcome = runner
        .act(GameAction::SelectDieRolls {
            ignore_indices: vec![0],
        })
        .expect("the seat may ignore a tied-lowest roll");
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "completion: priority resumes, got {:?}",
        runner.state().waiting_for
    );
    assert_eq!(
        d1_total(&outcome.events),
        10,
        "survivor 1 + P0's 9; an acting-seat misread gives 6"
    );
}

/// R3-B INLINE control — the same seat-bound roll without Barbarian
/// resolves live (no suspension), so the total must carry the same printed
/// addend. The two tests agree if and only if pause and live read the same
/// controller.
#[test]
fn scoped_inline_roll_reads_printed_controller() {
    use engine::game::scenario::P1;
    let mut scenario = GameScenario::new_n_player(2, 49);
    scenario.at_phase(Phase::PreCombatMain);
    let own = scenario.add_creature(P0, "Own Brute", 9, 9).id();
    scenario.add_creature(P1, "Foe Brute", 5, 5);
    let mut runner = scenario.build();

    let mut ability = ResolvedAbility::new(
        Effect::RollDie {
            count: QuantityExpr::Fixed { value: 1 },
            sides: 1,
            results: vec![],
            modifier: Some(you_power_sum_modifier()),
        },
        vec![],
        own,
        P0,
    );
    ability.set_original_controller_recursive(P0);
    ability.set_controller_recursive(P1);
    ability.set_scoped_player_recursive(P1);
    let mut events = Vec::new();
    engine::game::effects::roll_die::resolve(runner.state_mut(), &ability, &mut events)
        .expect("the raised roll resolves");
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "no Barbarian means no pause, got {:?}",
        runner.state().waiting_for
    );
    assert_eq!(
        d1_total(&events),
        10,
        "inline survivor 1 + P0's 9, matching the paused read"
    );
}

/// R4-B1 PAUSE — the R3-B seat shape (printed P0, acting + scoped seat P1)
/// with a ScopedPlayer-sum modifier over asymmetric populations (P0 9, P1
/// 5) must add P1's 5 after a production keep pause, exactly like the
/// inline path. Pre-fix the resumed read dropped the scoped binding and
/// fell back to the printed controller: totals 10 vs 6.
#[test]
fn scoped_pause_reads_scoped_seat_not_printed_controller() {
    use engine::game::scenario::P1;
    let mut scenario = GameScenario::new_n_player(2, 49);
    scenario.at_phase(Phase::PreCombatMain);
    let own = scenario.add_creature(P0, "Own Brute", 9, 9).id();
    scenario.add_creature(P1, "Foe Brute", 5, 5);
    // Barbarian sits with the SEAT (P1): the rebound roll belongs to P1, so
    // only P1's replacement opens the production keep prompt.
    scenario.add_enchantment_from_oracle(P1, "Barbarian Class", BARBARIAN_CLASS_L1);
    let mut runner = scenario.build();

    let mut ability = ResolvedAbility::new(
        Effect::RollDie {
            count: QuantityExpr::Fixed { value: 1 },
            sides: 1,
            results: vec![],
            modifier: Some(scoped_power_sum_modifier()),
        },
        vec![],
        own,
        P0,
    );
    ability.set_original_controller_recursive(P0);
    ability.set_controller_recursive(P1);
    ability.set_scoped_player_recursive(P1);
    let mut events = Vec::new();
    engine::game::effects::roll_die::resolve(runner.state_mut(), &ability, &mut events)
        .expect("the raised roll proposes");
    let WaitingFor::DieKeepChoice { player, .. } = &runner.state().waiting_for else {
        panic!(
            "REACH-GUARD: d1 plus Barbarian must open the keep prompt, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(*player, P1, "the P1 seat answers the keep choice");
    let outcome = runner
        .act(GameAction::SelectDieRolls {
            ignore_indices: vec![0],
        })
        .expect("the seat may ignore a tied-lowest roll");
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "completion: priority resumes, got {:?}",
        runner.state().waiting_for
    );
    assert_eq!(
        d1_total(&outcome.events),
        6,
        "survivor 1 + P1's 5; a dropped scoped binding falls back to P0's 9 and emits 10"
    );
}

/// R4-B1 INLINE control — the same seat-bound roll without Barbarian
/// resolves live (no suspension), so the total must carry the same scoped
/// addend. The two tests agree if and only if pause and live read the same
/// scoped seat.
#[test]
fn scoped_inline_roll_reads_scoped_seat() {
    use engine::game::scenario::P1;
    let mut scenario = GameScenario::new_n_player(2, 49);
    scenario.at_phase(Phase::PreCombatMain);
    let own = scenario.add_creature(P0, "Own Brute", 9, 9).id();
    scenario.add_creature(P1, "Foe Brute", 5, 5);
    let mut runner = scenario.build();

    let mut ability = ResolvedAbility::new(
        Effect::RollDie {
            count: QuantityExpr::Fixed { value: 1 },
            sides: 1,
            results: vec![],
            modifier: Some(scoped_power_sum_modifier()),
        },
        vec![],
        own,
        P0,
    );
    ability.set_original_controller_recursive(P0);
    ability.set_controller_recursive(P1);
    ability.set_scoped_player_recursive(P1);
    let mut events = Vec::new();
    engine::game::effects::roll_die::resolve(runner.state_mut(), &ability, &mut events)
        .expect("the raised roll resolves");
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "no Barbarian means no pause, got {:?}",
        runner.state().waiting_for
    );
    assert_eq!(
        d1_total(&events),
        6,
        "inline survivor 1 + P1's 5, matching the paused read"
    );
}
