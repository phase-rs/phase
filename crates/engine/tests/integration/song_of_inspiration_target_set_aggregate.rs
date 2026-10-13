//! PR C (Unit 8d) tests: aggregates over the ability's chosen targets.
//!
//! Gate C-0 runs FIRST and before any PR C implementation code (plan J.4.1,
//! M1 sequencing): it confirms the rejected `ParentTarget` carrier reads 0
//! for graveyard targets, with a positive reach-guard proving the instrument
//! fired. If this probe ever reads the correct total instead, plan G.2's
//! rejection is falsified — PR C re-plans and PR B stands unchanged.

use engine::game::quantity::resolve_quantity;
use engine::game::scenario::{GameScenario, P0};
use engine::parser::parse_oracle_text;
use engine::types::ability::{
    AbilityDefinition, AbilityKind, AggregateFunction, CardTypeSetSource, CountScope,
    DieResultBranch, DieRollIgnoreRule, DieRollModifier, Effect, ObjectProperty, QuantityExpr,
    QuantityRef, TargetFilter, TargetRef, ZoneRef,
};
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::resolution::PendingDieRoll;
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

    if let Some(obj) = committed.state_mut().objects.get_mut(&elm) {
        obj.zone = Zone::Exile;
    }
    let exiled = committed
        .state()
        .objects
        .values()
        .filter(|o| o.zone == Zone::Exile)
        .count();
    assert_eq!(
        exiled, 1,
        "NON-VACUITY: the mid-stack exile must actually move Elm, else this \
         test cannot tell a live read from a stale one"
    );

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
