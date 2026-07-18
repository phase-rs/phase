//! L02 BB1 — "Activate only if <state>" activation-restriction gating for seven
//! Standard cards. Each shape gets (P) a parse-fidelity test asserting the exact
//! `RequiresCondition { condition: Some(<payload>) }` and (R) a discriminating
//! runtime test that flips activation legality between a FALSE and a TRUE game
//! state via the production `check_activation_restrictions` gate.
//!
//! The runtime tests isolate the `RequiresCondition` restriction (filtering out
//! `AsSorcery`/tap/etc.) and assert Ok/Err flips on the game state. The FALSE-
//! state assertion is the revert guard: reverting any card's combinator/bridge
//! arm leaves `condition: None` → permissive-true → the FALSE case wrongly
//! becomes activatable and the test fails. The `!req.is_empty()` reach-guard in
//! `condition_gate_ok` proves the restriction is actually present (non-vacuous).

use engine::game::restrictions::{check_activation_restrictions, record_battlefield_entry};
use engine::game::scenario::{GameScenario, P0, P1};
use engine::parser::parse_oracle_text;
use engine::types::ability::{
    AbilityCondition, AbilityDefinition, ActivationRestriction, AggregateFunction, Comparator,
    ControllerRef, CountScope, DamageChannel, DamageKindFilter, FilterProp, ObjectScope,
    ParsedCondition, PlayerFilter, PlayerRelation, PlayerScope, QuantityExpr, QuantityRef,
    TargetFilter, TargetRef, TriggerCondition, TypeFilter, ZoneRef,
};
use engine::types::card_type::CoreType;
use engine::types::counter::CounterType;
use engine::types::game_state::{DamageRecord, GameState, ZoneChangeRecord};
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaColor;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const BONECACHE: &str = "{T}, Pay 1 life: Draw a card. Activate only if three or more cards left \
your graveyard this turn or if you've sacrificed a Food this turn.";
const TEMPLE_DEAD: &str = "{2}{B}, {T}: Transform this land. Activate only if a player has one or \
fewer cards in hand and only as a sorcery.";
const PUCAS_EYE: &str = "{3}, {T}: Draw a card. Activate only if there are five colors among \
permanents you control.";
const MASTERS: &str = "{T}: Create a 4/4 white and blue Golem artifact creature token. Activate \
only if this artifact or another artifact entered the battlefield under your control this turn.";
const TEMPLE_CYCLICAL: &str = "{2}{U}, {T}: Transform this land. Activate only if it has no time \
counters on it and only as a sorcery.";
const TEMPLE_POWER: &str = "{2}{R}, {T}: Transform this land. Activate only if red sources you \
controlled dealt 4 or more noncombat damage this turn and only as a sorcery.";
const CAVERNOUS_MAW: &str = "{2}: This land becomes a 3/3 Elemental creature until end of turn. \
It's still a Cave land. Activate only if the number of other Caves you control plus the number of \
Cave cards in your graveyard is three or greater.";

// --- shared helpers ---------------------------------------------------------

fn parse_restrictions(
    oracle: &str,
    name: &str,
    types: &[&str],
    subtypes: &[&str],
) -> Vec<ActivationRestriction> {
    let types: Vec<String> = types.iter().map(|s| s.to_string()).collect();
    let subtypes: Vec<String> = subtypes.iter().map(|s| s.to_string()).collect();
    let parsed = parse_oracle_text(oracle, name, &[], &types, &subtypes);
    parsed
        .abilities
        .into_iter()
        .next()
        .expect("card must parse an activated ability")
        .activation_restrictions
}

fn requires_condition(restrictions: &[ActivationRestriction]) -> &ParsedCondition {
    restrictions
        .iter()
        .find_map(|r| match r {
            ActivationRestriction::RequiresCondition {
                condition: Some(c), ..
            } => Some(c),
            _ => None,
        })
        .expect("expected RequiresCondition { condition: Some(..) } — a None here is the reverted state")
}

/// Isolate the `RequiresCondition` restriction(s) and evaluate ONLY those
/// against the production gate, so `AsSorcery`/tap/mana never confound the
/// condition assertion. `!req.is_empty()` is the reach-guard (non-vacuous).
fn condition_gate_ok(state: &GameState, source: ObjectId) -> bool {
    let restrictions = &state.objects.get(&source).unwrap().abilities[0].activation_restrictions;
    let req: Vec<ActivationRestriction> = restrictions
        .iter()
        .filter(|r| matches!(r, ActivationRestriction::RequiresCondition { .. }))
        .cloned()
        .collect();
    assert!(
        !req.is_empty(),
        "reach-guard: activated ability must carry a RequiresCondition restriction"
    );
    check_activation_restrictions(state, P0, source, 0, &req).is_ok()
}

// ===========================================================================
// S1 — Bonecache Overseer: Or[ cards-left-gy GE 3, sacrificed-Food GE 1 ]
// ===========================================================================

#[test]
fn s1_bonecache_parse_disjunction_payload() {
    let r = parse_restrictions(
        BONECACHE,
        "Bonecache Overseer",
        &["Creature"],
        &["Skeleton"],
    );
    match requires_condition(&r) {
        ParsedCondition::Or { conditions } => {
            assert_eq!(conditions.len(), 2, "two disjuncts");
            match &conditions[0] {
                ParsedCondition::QuantityComparison {
                    lhs:
                        QuantityExpr::Ref {
                            qty:
                                QuantityRef::ZoneChangeCountThisTurn {
                                    from: Some(Zone::Graveyard),
                                    ..
                                },
                        },
                    comparator: Comparator::GE,
                    rhs: QuantityExpr::Fixed { value: 3 },
                } => {}
                other => panic!("disjunct 0 (cards left gy GE 3): {other:?}"),
            }
            match &conditions[1] {
                ParsedCondition::QuantityComparison {
                    lhs:
                        QuantityExpr::Ref {
                            qty: QuantityRef::SacrificedThisTurn { filter, .. },
                        },
                    comparator: Comparator::GE,
                    rhs: QuantityExpr::Fixed { value: 1 },
                } => {
                    // op2 must carry the Food subtype filter, not Any.
                    assert!(
                        !matches!(filter, TargetFilter::Any),
                        "sacrificed filter must be typed (Food), not Any: {filter:?}"
                    );
                }
                other => panic!("disjunct 1 (sacrificed Food GE 1): {other:?}"),
            }
        }
        other => panic!("expected Or disjunction, got {other:?}"),
    }
}

fn bonecache_scenario() -> (GameScenario, ObjectId, [ObjectId; 3], ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let overseer = scenario
        .add_creature_from_oracle(P0, "Bonecache Overseer", 1, 1, BONECACHE)
        .id();
    // Three nontoken cards owned by P0 used as gy-departure record sources.
    let m0 = scenario.add_creature(P0, "Mote A", 1, 1).id();
    let m1 = scenario.add_creature(P0, "Mote B", 1, 1).id();
    let m2 = scenario.add_creature(P0, "Mote C", 1, 1).id();
    // A permanent controlled by P0, re-typed to a Food artifact post-build.
    let food = scenario.add_creature(P0, "Food", 0, 0).id();
    (scenario, overseer, [m0, m1, m2], food)
}

/// Re-type an object into a Food artifact (post-build; `GameScenario` exposes no
/// public raw-state mutation).
fn make_food(state: &mut GameState, id: ObjectId) {
    let obj = state.objects.get_mut(&id).unwrap();
    obj.card_types.core_types.clear();
    obj.card_types.core_types.push(CoreType::Artifact);
    obj.card_types.subtypes = vec!["Food".to_string()];
    obj.base_card_types = obj.card_types.clone();
}

fn push_gy_left(state_read: &GameState, obj: ObjectId) -> ZoneChangeRecord {
    state_read.objects[&obj].snapshot_for_zone_change(obj, Some(Zone::Graveyard), Zone::Exile)
}

fn push_sacrificed(state_read: &GameState, obj: ObjectId) -> ZoneChangeRecord {
    state_read.objects[&obj].snapshot_for_zone_change(obj, Some(Zone::Battlefield), Zone::Graveyard)
}

#[test]
fn s1_bonecache_runtime_false_when_neither_disjunct() {
    let (scenario, overseer, motes, _food) = bonecache_scenario();
    let mut runner = scenario.build();
    // Only 2 cards left the graveyard this turn, no Food sacrificed.
    for &m in &motes[..2] {
        let rec = push_gy_left(runner.state(), m);
        runner.state_mut().zone_changes_this_turn.push(rec);
    }
    assert!(
        !condition_gate_ok(runner.state(), overseer),
        "2 gy-departures and no Food sacrifice must NOT satisfy the gate"
    );
}

#[test]
fn s1_bonecache_runtime_true_via_graveyard_disjunct() {
    let (scenario, overseer, motes, _food) = bonecache_scenario();
    let mut runner = scenario.build();
    for &m in &motes {
        let rec = push_gy_left(runner.state(), m);
        runner.state_mut().zone_changes_this_turn.push(rec);
    }
    assert!(
        condition_gate_ok(runner.state(), overseer),
        "3 cards left the graveyard this turn must satisfy the first disjunct"
    );
}

#[test]
fn s1_bonecache_runtime_true_via_food_disjunct() {
    // No graveyard departures — proves the SECOND disjunct independently gates
    // (deleting the ' or if ' connector would drop it and flip this to FALSE).
    let (scenario, overseer, _motes, food) = bonecache_scenario();
    let mut runner = scenario.build();
    make_food(runner.state_mut(), food);
    let rec = push_sacrificed(runner.state(), food);
    runner.state_mut().sacrificed_permanents_this_turn.push(rec);
    assert!(
        condition_gate_ok(runner.state(), overseer),
        "a sacrificed Food this turn must satisfy the second disjunct"
    );
}

// ===========================================================================
// S2 — Temple of the Dead: PlayerCount(PlayerAttribute{All, HandSize, LE, 1}) GE 1
// ===========================================================================

#[test]
fn s2_temple_dead_parse_existential_hand_predicate() {
    let r = parse_restrictions(TEMPLE_DEAD, "Temple of the Dead", &["Land"], &[]);
    match requires_condition(&r) {
        ParsedCondition::QuantityComparison {
            lhs:
                QuantityExpr::Ref {
                    qty:
                        QuantityRef::PlayerCount {
                            filter:
                                PlayerFilter::PlayerAttribute {
                                    relation: PlayerRelation::All,
                                    attr,
                                    comparator: Comparator::LE,
                                    value,
                                },
                        },
                },
            comparator: Comparator::GE,
            rhs: QuantityExpr::Fixed { value: 1 },
        } => {
            assert!(
                matches!(**attr, QuantityRef::HandSize { .. }),
                "attr must be HandSize: {attr:?}"
            );
            assert!(
                matches!(**value, QuantityExpr::Fixed { value: 1 }),
                "threshold must be 1: {value:?}"
            );
        }
        other => panic!("expected PlayerAttribute(All, HandSize LE 1) existential, got {other:?}"),
    }
}

fn temple_dead_scenario(p0_hand: usize, p1_hand: usize) -> (GameScenario, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let temple = scenario
        .add_land_from_oracle(P0, "Temple of the Dead", TEMPLE_DEAD)
        .id();
    let p0_cards: Vec<&str> = (0..p0_hand).map(|_| "Plains").collect();
    let p1_cards: Vec<&str> = (0..p1_hand).map(|_| "Island").collect();
    scenario.with_cards_in_hand(P0, &p0_cards);
    scenario.with_cards_in_hand(P1, &p1_cards);
    (scenario, temple)
}

#[test]
fn s2_temple_dead_runtime_false_when_all_players_have_two() {
    let (scenario, temple) = temple_dead_scenario(2, 2);
    let runner = scenario.build();
    assert!(
        !condition_gate_ok(runner.state(), temple),
        "every player holding >=2 cards must NOT satisfy 'a player has one or fewer'"
    );
}

#[test]
fn s2_temple_dead_runtime_true_when_opponent_has_one() {
    // P0 (controller) has 3; only the opponent has <=1. Proves PlayerRelation::All
    // (a Controller-only reading would wrongly gate FALSE here).
    let (scenario, temple) = temple_dead_scenario(3, 1);
    let runner = scenario.build();
    assert!(
        condition_gate_ok(runner.state(), temple),
        "an OPPONENT with one card must satisfy the existential (proves All, not Controller)"
    );
}

#[test]
fn s2_temple_dead_runtime_true_when_you_have_one() {
    let (scenario, temple) = temple_dead_scenario(1, 3);
    let runner = scenario.build();
    assert!(
        condition_gate_ok(runner.state(), temple),
        "you holding one card must satisfy the existential"
    );
}

// ===========================================================================
// S3 — Puca's Eye: DistinctColorsAmongPermanents(you control) EQ 5
// ===========================================================================

#[test]
fn s3_pucas_eye_parse_distinct_colors_eq_five() {
    let r = parse_restrictions(PUCAS_EYE, "Puca's Eye", &["Artifact"], &[]);
    match requires_condition(&r) {
        ParsedCondition::QuantityComparison {
            lhs:
                QuantityExpr::Ref {
                    qty: QuantityRef::DistinctColorsAmongPermanents { filter },
                },
            comparator: Comparator::EQ,
            rhs: QuantityExpr::Fixed { value: 5 },
        } => match filter {
            // Lock the "you control" scope (mirrors the S7 controller assertion):
            // dropping `you control` would count opponents' permanents too.
            TargetFilter::Typed(tf) => assert_eq!(
                tf.controller,
                Some(ControllerRef::You),
                "'permanents you control' must lock controller You: {tf:?}"
            ),
            other => panic!("expected Typed filter with controller You, got {other:?}"),
        },
        other => panic!("expected DistinctColorsAmongPermanents EQ 5, got {other:?}"),
    }
}

/// Build Puca's Eye plus one P0 creature per color; the colors are assigned
/// post-build (no public raw-state mutation on `GameScenario`).
fn pucas_eye_gate_ok(colors: &[ManaColor]) -> bool {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let eye = scenario
        .add_creature_from_oracle(P0, "Puca's Eye", 0, 0, PUCAS_EYE)
        .as_artifact()
        .id();
    let ids: Vec<ObjectId> = colors
        .iter()
        .map(|_| scenario.add_creature(P0, "Prism", 1, 1).id())
        .collect();
    let mut runner = scenario.build();
    for (id, color) in ids.iter().zip(colors.iter()) {
        runner.state_mut().objects.get_mut(id).unwrap().color = vec![*color];
    }
    condition_gate_ok(runner.state(), eye)
}

#[test]
fn s3_pucas_eye_runtime_false_with_four_colors() {
    assert!(
        !pucas_eye_gate_ok(&[
            ManaColor::White,
            ManaColor::Blue,
            ManaColor::Black,
            ManaColor::Red,
        ]),
        "four distinct colors must NOT satisfy 'five colors among permanents you control'"
    );
}

#[test]
fn s3_pucas_eye_runtime_true_with_five_colors() {
    assert!(
        pucas_eye_gate_ok(&[
            ManaColor::White,
            ManaColor::Blue,
            ManaColor::Black,
            ManaColor::Red,
            ManaColor::Green,
        ]),
        "all five colors present must satisfy the gate"
    );
}

// ===========================================================================
// S4 — Master's Manufactory: EnteredThisTurn(artifact you control) GE 1
// ===========================================================================

#[test]
fn s4_masters_parse_entered_this_turn_no_another() {
    let r = parse_restrictions(MASTERS, "Master's Guide-Mural", &["Artifact"], &[]);
    match requires_condition(&r) {
        ParsedCondition::QuantityComparison {
            lhs:
                QuantityExpr::Ref {
                    qty:
                        QuantityRef::BattlefieldEntriesThisTurn {
                            player: PlayerScope::Controller,
                            filter,
                        },
                },
            comparator: Comparator::GE,
            rhs: QuantityExpr::Fixed { value: 1 },
        } => match filter {
            TargetFilter::Typed(tf) => {
                // "under your control" scope now lives on PlayerScope::Controller
                // (resolver keys on record.controller), NOT on the type filter —
                // the filter is bare, mirroring parse_or_more_entered_count.
                assert_eq!(
                    tf.controller, None,
                    "controller scope belongs to PlayerScope::Controller, filter must be bare: {tf:?}"
                );
                assert!(
                    tf.type_filters.contains(&TypeFilter::Artifact),
                    "artifact type: {tf:?}"
                );
                assert!(
                    !tf.properties
                        .iter()
                        .any(|p| matches!(p, FilterProp::Another)),
                    "self-inclusive '~ or another' must NOT carry Another: {tf:?}"
                );
            }
            other => panic!("expected Typed filter, got {other:?}"),
        },
        other => panic!("expected BattlefieldEntriesThisTurn(Controller) GE 1, got {other:?}"),
    }
}

#[test]
fn s4_masters_runtime_false_when_nothing_entered() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let masters = scenario
        .add_creature_from_oracle(P0, "Master's Guide-Mural", 0, 0, MASTERS)
        .as_artifact()
        .id();
    // masters entered on a PRIOR turn (entered_battlefield_turn stays None).
    let runner = scenario.build();
    assert!(
        !condition_gate_ok(runner.state(), masters),
        "no artifact entered this turn must NOT satisfy the gate"
    );
}

#[test]
fn s4_masters_runtime_true_when_source_itself_entered() {
    // ~-only entry: the source itself entered this turn, no other artifact.
    // Proves NO Another (Another would exclude the source and gate FALSE).
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let masters = scenario
        .add_creature_from_oracle(P0, "Master's Guide-Mural", 0, 0, MASTERS)
        .as_artifact()
        .id();
    let mut runner = scenario.build();
    // Seed the battlefield-entry snapshot (production path) for the source's own
    // entry this turn — the BattlefieldEntriesThisTurn resolver reads this ledger.
    record_battlefield_entry(runner.state_mut(), masters);
    assert!(
        condition_gate_ok(runner.state(), masters),
        "the source's own entry this turn must satisfy the self-inclusive gate"
    );
}

#[test]
fn s4_masters_runtime_false_when_opponent_artifact_entered() {
    // An OPPONENT's artifact entered this turn — must stay FALSE (proves controller You).
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let masters = scenario
        .add_creature_from_oracle(P0, "Master's Guide-Mural", 0, 0, MASTERS)
        .as_artifact()
        .id();
    let opp_art = scenario
        .add_creature(P1, "Opp Bauble", 0, 0)
        .as_artifact()
        .id();
    let mut runner = scenario.build();
    // Seed the entry snapshot for the OPPONENT's artifact (controller P1). The
    // resolver scopes to record.controller == P0 (PlayerScope::Controller), so
    // this entry is excluded and the gate stays FALSE.
    record_battlefield_entry(runner.state_mut(), opp_art);
    assert!(
        !condition_gate_ok(runner.state(), masters),
        "an opponent's artifact entering must NOT satisfy 'under your control' (proves player=Controller)"
    );
}

#[test]
fn s4_masters_runtime_true_when_artifact_entered_then_left_lookback() {
    // CR 608.2h look-back proof: an artifact entered under P0's control this turn
    // and has since LEFT the battlefield (died/bounced/sacrificed). "entered ...
    // this turn" is a historical event, so the gate must still be TRUE.
    //
    // Revert-to-red: under the old live-board `EnteredThisTurn` authority the
    // resolver requires `o.zone == Battlefield`, so this departed artifact would
    // be excluded and the gate would wrongly read FALSE. The
    // `BattlefieldEntriesThisTurn` snapshot survives the departure -> TRUE.
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // masters entered on a PRIOR turn (no record, entered_battlefield_turn None)
    // so it does NOT self-count — isolating the gate to the departed artifact.
    let masters = scenario
        .add_creature_from_oracle(P0, "Master's Guide-Mural", 0, 0, MASTERS)
        .as_artifact()
        .id();
    let art = scenario
        .add_creature(P0, "Departed Bauble", 0, 0)
        .as_artifact()
        .id();
    let mut runner = scenario.build();
    let turn = runner.state().turn_number;
    // Snapshot the entry while `art` is still on the battlefield (production path).
    record_battlefield_entry(runner.state_mut(), art);
    // Now `art` leaves the battlefield: it bears the live this-turn entry marker
    // yet is no longer on the battlefield, so the old live-board read would miss
    // it — only the surviving snapshot record keeps the gate TRUE.
    {
        let obj = runner.state_mut().objects.get_mut(&art).unwrap();
        obj.entered_battlefield_turn = Some(turn);
        obj.zone = Zone::Graveyard;
    }
    assert!(
        condition_gate_ok(runner.state(), masters),
        "an artifact that entered this turn then left must still satisfy the look-back gate (CR 608.2h)"
    );
}

// ===========================================================================
// S5 — Temple of Cyclical Time: CountersOn(Source, Time) EQ 0
// ===========================================================================

#[test]
fn s5_temple_cyclical_parse_no_time_counters() {
    let r = parse_restrictions(TEMPLE_CYCLICAL, "Temple of Cyclical Time", &["Land"], &[]);
    match requires_condition(&r) {
        ParsedCondition::QuantityComparison {
            lhs:
                QuantityExpr::Ref {
                    qty:
                        QuantityRef::CountersOn {
                            scope: ObjectScope::Source,
                            counter_type: Some(CounterType::Time),
                        },
                },
            comparator: Comparator::EQ,
            rhs: QuantityExpr::Fixed { value: 0 },
        } => {}
        other => panic!("expected CountersOn(Source, Time) EQ 0, got {other:?}"),
    }
}

#[test]
fn s5_temple_cyclical_runtime_false_with_time_counter() {
    // Revert guard: without the HasCounters bridge arm the condition is None →
    // permissive-true → this FALSE assertion fails.
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let temple = scenario
        .add_land_from_oracle(P0, "Temple of Cyclical Time", TEMPLE_CYCLICAL)
        .id();
    scenario.with_counter(temple, CounterType::Time, 1);
    let runner = scenario.build();
    assert!(
        !condition_gate_ok(runner.state(), temple),
        "a time counter present must NOT satisfy 'no time counters'"
    );
}

#[test]
fn s5_temple_cyclical_runtime_true_with_zero_time_counters() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let temple = scenario
        .add_land_from_oracle(P0, "Temple of Cyclical Time", TEMPLE_CYCLICAL)
        .id();
    let runner = scenario.build();
    assert!(
        condition_gate_ok(runner.state(), temple),
        "zero time counters must satisfy the gate"
    );
}

// ===========================================================================
// S6 — Temple of Power: DamageDealtThisTurn(red, you, noncombat, Sum) GE 4
// ===========================================================================

#[test]
fn s6_temple_power_parse_filtered_damage_threshold() {
    let r = parse_restrictions(TEMPLE_POWER, "Temple of Power", &["Land"], &[]);
    match requires_condition(&r) {
        ParsedCondition::QuantityComparison {
            lhs:
                QuantityExpr::Ref {
                    qty:
                        QuantityRef::DamageDealtThisTurn {
                            source,
                            aggregate: AggregateFunction::Sum,
                            group_by: None,
                            damage_kind: DamageKindFilter::NoncombatOnly,
                            channel: DamageChannel::Total,
                            ..
                        },
                },
            comparator: Comparator::GE,
            rhs: QuantityExpr::Fixed { value: 4 },
        } => match &**source {
            TargetFilter::Typed(tf) => {
                assert_eq!(tf.controller, Some(ControllerRef::You), "you controlled");
                assert!(
                    tf.properties.iter().any(|p| matches!(
                        p,
                        FilterProp::HasColor {
                            color: ManaColor::Red
                        }
                    )),
                    "red source filter: {tf:?}"
                );
            }
            other => panic!("expected Typed source filter, got {other:?}"),
        },
        other => panic!("expected DamageDealtThisTurn(Sum, noncombat) GE 4, got {other:?}"),
    }
}

fn temple_power_scenario() -> (GameScenario, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let temple = scenario
        .add_land_from_oracle(P0, "Temple of Power", TEMPLE_POWER)
        .id();
    (scenario, temple)
}

fn red_noncombat_record(amount: u32, controller: engine::types::player::PlayerId) -> DamageRecord {
    DamageRecord {
        target: TargetRef::Player(P1),
        amount,
        is_combat: false,
        source_colors: vec![ManaColor::Red],
        source_controller_snapshot: controller,
        ..Default::default()
    }
}

#[test]
fn s6_temple_power_runtime_true_with_four_red_noncombat() {
    let (scenario, temple) = temple_power_scenario();
    let mut runner = scenario.build();
    runner
        .state_mut()
        .damage_dealt_this_turn
        .push_back(red_noncombat_record(4, P0));
    assert!(
        condition_gate_ok(runner.state(), temple),
        "4 noncombat damage from a red source you controlled must satisfy the gate"
    );
}

#[test]
fn s6_temple_power_runtime_false_with_three_red_noncombat() {
    let (scenario, temple) = temple_power_scenario();
    let mut runner = scenario.build();
    runner
        .state_mut()
        .damage_dealt_this_turn
        .push_back(red_noncombat_record(3, P0));
    assert!(
        !condition_gate_ok(runner.state(), temple),
        "only 3 noncombat damage must NOT satisfy 'four or more'"
    );
}

#[test]
fn s6_temple_power_runtime_false_when_combat_damage() {
    // 4 red combat damage — proves NoncombatOnly (combat is excluded).
    let (scenario, temple) = temple_power_scenario();
    let mut runner = scenario.build();
    let mut rec = red_noncombat_record(4, P0);
    rec.is_combat = true;
    runner.state_mut().damage_dealt_this_turn.push_back(rec);
    assert!(
        !condition_gate_ok(runner.state(), temple),
        "combat damage must NOT satisfy 'noncombat' (proves NoncombatOnly)"
    );
}

#[test]
fn s6_temple_power_runtime_false_when_non_red_source() {
    // 4 blue noncombat damage — proves the color filter.
    let (scenario, temple) = temple_power_scenario();
    let mut runner = scenario.build();
    let mut rec = red_noncombat_record(4, P0);
    rec.source_colors = vec![ManaColor::Blue];
    runner.state_mut().damage_dealt_this_turn.push_back(rec);
    assert!(
        !condition_gate_ok(runner.state(), temple),
        "a non-red source must NOT satisfy the red filter"
    );
}

#[test]
fn s6_temple_power_runtime_false_when_opponent_controlled() {
    // 4 red noncombat from an opponent-controlled source — proves 'you controlled'.
    let (scenario, temple) = temple_power_scenario();
    let mut runner = scenario.build();
    runner
        .state_mut()
        .damage_dealt_this_turn
        .push_back(red_noncombat_record(4, P1));
    assert!(
        !condition_gate_ok(runner.state(), temple),
        "an opponent-controlled red source must NOT satisfy 'you controlled'"
    );
}

// ===========================================================================
// S7 — Cavernous Maw: Sum[ other Caves you control, Cave cards in your gy ] GE 3
// ===========================================================================

#[test]
fn s7_cavernous_maw_parse_additive_sum() {
    let r = parse_restrictions(CAVERNOUS_MAW, "Cavernous Maw", &["Land"], &["Cave"]);
    match requires_condition(&r) {
        ParsedCondition::QuantityComparison {
            lhs: QuantityExpr::Sum { exprs },
            comparator: Comparator::GE,
            rhs: QuantityExpr::Fixed { value: 3 },
        } => {
            assert_eq!(exprs.len(), 2, "two summed terms");
            match &exprs[0] {
                QuantityExpr::Ref {
                    qty:
                        QuantityRef::ObjectCount {
                            filter: TargetFilter::Typed(tf),
                        },
                } => {
                    assert_eq!(
                        tf.controller,
                        Some(ControllerRef::You),
                        "term A you control"
                    );
                    assert!(
                        tf.properties
                            .iter()
                            .any(|p| matches!(p, FilterProp::Another)),
                        "term A 'other Caves' must carry Another: {tf:?}"
                    );
                    // Lock the Cave subtype axis: dropping it would count all
                    // permanents you control, not just Caves.
                    assert!(
                        tf.type_filters
                            .iter()
                            .any(|f| matches!(f, TypeFilter::Subtype(s) if s.eq_ignore_ascii_case("Cave"))),
                        "term A must carry the Cave subtype: {tf:?}"
                    );
                }
                other => panic!("term A must be ObjectCount, got {other:?}"),
            }
            match &exprs[1] {
                QuantityExpr::Ref {
                    qty:
                        QuantityRef::ZoneCardCount {
                            zone: ZoneRef::Graveyard,
                            scope: CountScope::Controller,
                            filter: Some(TargetFilter::Typed(tf_b)),
                            ..
                        },
                } => {
                    // Lock the Cave subtype axis on term B ("Cave cards in your gy").
                    assert!(
                        tf_b.type_filters
                            .iter()
                            .any(|f| matches!(f, TypeFilter::Subtype(s) if s.eq_ignore_ascii_case("Cave"))),
                        "term B must carry the Cave subtype: {tf_b:?}"
                    );
                }
                other => panic!(
                    "term B must be graveyard ZoneCardCount(Controller, Typed Cave), got {other:?}"
                ),
            }
        }
        other => panic!("expected Sum GE 3, got {other:?}"),
    }
}

/// Build Cavernous Maw + `other_caves` other Cave lands controlled by P0 +
/// `own_gy` / `opp_gy` Cave cards in the respective graveyards.
fn maw_scenario(other_caves: usize, own_gy: usize, opp_gy: usize) -> (GameScenario, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let maw = scenario
        .add_land_from_oracle(P0, "Cavernous Maw", CAVERNOUS_MAW)
        .with_subtypes(vec!["Cave"])
        .id();
    for _ in 0..other_caves {
        scenario
            .add_creature(P0, "Some Cave", 0, 0)
            .as_land()
            .with_subtypes(vec!["Cave"]);
    }
    for _ in 0..own_gy {
        scenario
            .add_creature_to_graveyard(P0, "Gy Cave", 0, 0)
            .as_land()
            .with_subtypes(vec!["Cave"]);
    }
    for _ in 0..opp_gy {
        scenario
            .add_creature_to_graveyard(P1, "Opp Gy Cave", 0, 0)
            .as_land()
            .with_subtypes(vec!["Cave"]);
    }
    (scenario, maw)
}

#[test]
fn s7_cavernous_maw_runtime_false_when_sum_is_two() {
    // 1 other Cave + 1 own-gy Cave = 2 < 3. Also proves Maw does NOT self-count:
    // if the source were counted, term A would be 2 and the sum 3 (TRUE).
    let (scenario, maw) = maw_scenario(1, 1, 0);
    let runner = scenario.build();
    assert!(
        !condition_gate_ok(runner.state(), maw),
        "sum of 2 must NOT satisfy 'three or greater'"
    );
}

#[test]
fn s7_cavernous_maw_runtime_true_split_across_terms() {
    // 2 other Caves + 1 own-gy Cave = 3.
    let (scenario, maw) = maw_scenario(2, 1, 0);
    let runner = scenario.build();
    assert!(
        condition_gate_ok(runner.state(), maw),
        "2 battlefield Caves + 1 graveyard Cave = 3 must satisfy the gate"
    );
}

#[test]
fn s7_cavernous_maw_runtime_true_all_in_graveyard() {
    // 0 other Caves + 3 own-gy Caves = 3 (exercises term B alone).
    let (scenario, maw) = maw_scenario(0, 3, 0);
    let runner = scenario.build();
    assert!(
        condition_gate_ok(runner.state(), maw),
        "3 graveyard Caves alone must satisfy the gate"
    );
}

#[test]
fn s7_cavernous_maw_runtime_false_source_does_not_self_count() {
    // 2 other Caves, no graveyard Caves = 2. If Maw self-counted (no Another),
    // term A would be 3 and the gate TRUE. Asserting FALSE proves Another.
    let (scenario, maw) = maw_scenario(2, 0, 0);
    let runner = scenario.build();
    assert!(
        !condition_gate_ok(runner.state(), maw),
        "the source Cave must NOT self-count (proves Another on term A)"
    );
}

#[test]
fn s7_cavernous_maw_runtime_false_opponent_graveyard_cave_excluded() {
    // 2 other Caves + a Cave in the OPPONENT's graveyard = 2 (term B scope is
    // Controller). If opponent gy counted, sum would be 3 (TRUE).
    let (scenario, maw) = maw_scenario(2, 0, 1);
    let runner = scenario.build();
    assert!(
        !condition_gate_ok(runner.state(), maw),
        "a Cave in the opponent's graveyard must NOT count (proves Controller scope on term B)"
    );
}

// ===========================================================================
// BB-FU1 — bare-subject "a/an/another [type] entered ... this turn" look-back
//
// The bare-subject COUNT form (`parse_entered_this_turn_subject`, condition.rs)
// previously emitted the live-board `QuantityRef::EnteredThisTurn`, which
// under-counts a permanent that entered this turn then LEFT the battlefield
// (CR 608.2i look-back violation). BB-FU1 migrates it to the
// `BattlefieldEntriesThisTurn { PlayerScope::Controller }` ledger snapshot —
// mirroring the already-migrated siblings (`parse_or_more_entered_count`, the
// `~ or another` disjunct). This must NOT touch the un-migrated TargetFilter
// "put a counter on it" form (Malamet), which per CR 608.2h needs a live target.
//
// Count vehicle = Gargoyle Flock (routes through the bare-subject branch).
// Target vehicle = Malamet Battle Glyph (routes through the TargetFilter form).
// ===========================================================================

const GARGOYLE_FLOCK: &str = "Flying\nSkyswarm — At the beginning of your end step, if a creature \
entered the battlefield under your control this turn, create a 1/1 blue Tyranid Gargoyle creature \
token with flying.";

const MALAMET: &str =
    "Choose target creature you control and target creature you don't control. If \
the creature you control entered this turn, put a +1/+1 counter on it. Then those creatures fight \
each other.";

/// Parse Gargoyle Flock and return its intervening-if quantity comparison operands.
/// The bare-subject count form lowers to a `TriggerCondition::QuantityComparison`.
fn gargoyle_count_condition() -> (QuantityExpr, Comparator, QuantityExpr) {
    let parsed = parse_oracle_text(
        GARGOYLE_FLOCK,
        "Gargoyle Flock",
        &[],
        &["Creature".to_string()],
        &["Gargoyle".to_string()],
    );
    let trig = parsed
        .triggers
        .iter()
        .find(|t| t.condition.is_some())
        .expect("Gargoyle Flock must parse a trigger carrying an intervening-if condition");
    match trig.condition.clone().unwrap() {
        TriggerCondition::QuantityComparison {
            lhs,
            comparator,
            rhs,
        } => (lhs, comparator, rhs),
        other => panic!("expected TriggerCondition::QuantityComparison, got {other:?}"),
    }
}

/// Walk an ability's `sub_ability` chain and return the first attached condition.
fn first_ability_condition(def: &AbilityDefinition) -> Option<&AbilityCondition> {
    let mut cur = def;
    loop {
        if let Some(c) = cur.condition.as_ref() {
            return Some(c);
        }
        cur = cur.sub_ability.as_deref()?;
    }
}

/// Evaluate a bare quantity-comparison intervening-if through the production
/// activation-restriction gate — the same public `check_activation_restrictions`
/// path the S4 sibling runtime tests use. `restrictions::evaluate_condition`'s
/// `QuantityComparison` arm resolves via `resolve_quantity_scoped`, reaching the
/// identical `BattlefieldEntriesThisTurn` runtime arm (quantity.rs) that the
/// trigger intervening-if's `resolve_quantity_for_trigger_check` reaches. The
/// operands are the parser's own output for the real card (see
/// `gargoyle_count_condition`), so the fix under test drives this resolution.
fn count_gate_ok(
    state: &GameState,
    source: ObjectId,
    lhs: QuantityExpr,
    comparator: Comparator,
    rhs: QuantityExpr,
) -> bool {
    let req = vec![ActivationRestriction::RequiresCondition {
        condition: Some(ParsedCondition::QuantityComparison {
            lhs,
            comparator,
            rhs,
        }),
    }];
    check_activation_restrictions(state, P0, source, 0, &req).is_ok()
}

#[test]
fn bbfu1_gargoyle_flock_parse_count_form_migrated_to_ledger() {
    // Reach-guard / primary parser-change discriminator: the real card's count
    // form must now emit the ledger variant. Reverting BB-FU1 (restoring
    // `EnteredThisTurn` + `inject_controller_you`) makes this arm panic RED.
    let (lhs, comparator, rhs) = gargoyle_count_condition();
    assert_eq!(comparator, Comparator::GE);
    assert!(
        matches!(rhs, QuantityExpr::Fixed { value: 1 }),
        "count-form threshold is GE 1: {rhs:?}"
    );
    match lhs {
        QuantityExpr::Ref {
            qty:
                QuantityRef::BattlefieldEntriesThisTurn {
                    player: PlayerScope::Controller,
                    filter: TargetFilter::Typed(tf),
                },
        } => {
            // Controller scope moved off the filter onto PlayerScope::Controller,
            // leaving a bare filter that mirrors parse_or_more_entered_count. (The old
            // live-board battlefield requirement was a runtime zone-check in the
            // EnteredThisTurn arm, not a filter prop, so nothing else drops here.)
            assert_eq!(
                tf.controller, None,
                "controller belongs to PlayerScope::Controller; filter must be bare: {tf:?}"
            );
            assert!(
                tf.type_filters.contains(&TypeFilter::Creature),
                "creature type retained: {tf:?}"
            );
        }
        other => panic!(
            "Gargoyle Flock count form must emit BattlefieldEntriesThisTurn(Controller) after \
             BB-FU1 (the pre-fix EnteredThisTurn emit flips this RED): {other:?}"
        ),
    }
}

#[test]
fn bbfu1_gargoyle_flock_runtime_true_when_creature_entered_then_left_lookback() {
    // (a) POSITIVE FLIP — CR 608.2i look-back: a creature entered under P0's
    // control this turn and has since LEFT the battlefield (died/bounced/
    // sacrificed). The intervening-if count gate must still read TRUE.
    //
    // Revert-to-red: under the pre-BB-FU1 emit the reach-guard below fails first
    // (parsed qty is EnteredThisTurn, not BattlefieldEntriesThisTurn). Even
    // bypassing it, the live-board runtime read (quantity.rs EnteredThisTurn arm)
    // requires `o.zone == Battlefield`, so the departed creature is excluded ->
    // count 0 -> gate FALSE. The BattlefieldEntriesThisTurn snapshot survives the
    // departure -> TRUE. Verified by manual revert (see BB-FU1 report).
    let (lhs, comparator, rhs) = gargoyle_count_condition();
    assert!(
        matches!(
            &lhs,
            QuantityExpr::Ref {
                qty: QuantityRef::BattlefieldEntriesThisTurn {
                    player: PlayerScope::Controller,
                    ..
                },
            }
        ),
        "reach-guard: count form must resolve BattlefieldEntriesThisTurn(Controller): {lhs:?}"
    );

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // Gargoyle Flock entered a PRIOR turn (add_creature stamps entered_battlefield_turn
    // = turn-1, with no this-turn ledger record) so it never self-counts -> isolates the
    // gate to the departed entrant.
    let flock = scenario
        .add_creature_from_oracle(P0, "Gargoyle Flock", 3, 4, GARGOYLE_FLOCK)
        .id();
    let entrant = scenario.add_creature(P0, "Departed Beast", 2, 2).id();
    let mut runner = scenario.build();
    let turn = runner.state().turn_number;
    // Snapshot the entry while `entrant` is still on the battlefield (production path).
    record_battlefield_entry(runner.state_mut(), entrant);
    // Now it leaves: bears the live this-turn marker yet is no longer on the
    // battlefield, so only the surviving ledger record keeps the gate TRUE.
    {
        let obj = runner.state_mut().objects.get_mut(&entrant).unwrap();
        obj.entered_battlefield_turn = Some(turn);
        obj.zone = Zone::Graveyard;
    }
    assert!(
        count_gate_ok(runner.state(), flock, lhs, comparator, rhs),
        "a creature that entered this turn then left must still satisfy the count-form look-back \
         gate (CR 608.2i)"
    );
}

#[test]
fn bbfu1_gargoyle_flock_runtime_true_when_creature_entered_and_stayed() {
    // (b) B1 ENTERED-AND-STAYED positive control — the ordinary case: TRUE under
    // both the new ledger authority and the old live-board read. Proves the fix
    // didn't regress the normal case. One-line variation of (a), minus departure.
    let (lhs, comparator, rhs) = gargoyle_count_condition();
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let flock = scenario
        .add_creature_from_oracle(P0, "Gargoyle Flock", 3, 4, GARGOYLE_FLOCK)
        .id();
    let entrant = scenario.add_creature(P0, "Fresh Beast", 2, 2).id();
    let mut runner = scenario.build();
    record_battlefield_entry(runner.state_mut(), entrant);
    // entrant STAYS on the battlefield.
    assert!(
        count_gate_ok(runner.state(), flock, lhs, comparator, rhs),
        "a creature that entered this turn and stayed must satisfy the count-form gate"
    );
}

#[test]
fn bbfu1_gargoyle_flock_runtime_false_when_opponent_creature_entered() {
    // Controller-scope control: an OPPONENT's creature entered this turn -> the
    // gate reads FALSE (proves PlayerScope::Controller, not any-player). Mirrors
    // s4_masters_runtime_false_when_opponent_artifact_entered.
    let (lhs, comparator, rhs) = gargoyle_count_condition();
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let flock = scenario
        .add_creature_from_oracle(P0, "Gargoyle Flock", 3, 4, GARGOYLE_FLOCK)
        .id();
    let opp = scenario.add_creature(P1, "Enemy Beast", 2, 2).id();
    let mut runner = scenario.build();
    // Entry snapshot scoped to the OPPONENT (record.controller == P1). The gate
    // scopes to record.controller == P0, so this entry is excluded -> FALSE.
    record_battlefield_entry(runner.state_mut(), opp);
    assert!(
        !count_gate_ok(runner.state(), flock, lhs, comparator, rhs),
        "an opponent's creature entering must NOT satisfy 'under your control' (proves \
         player=Controller)"
    );
}

#[test]
fn bbfu1_malamet_target_form_not_over_migrated() {
    // (c) B2 boundary-LOCK — the UN-migrated TargetFilter "put a +1/+1 counter on
    // it" form (conditions.rs) must stay a live-target check
    // (AbilityCondition::TargetMatchesFilter { use_lki: false } carrying
    // FilterProp::EnteredThisTurn), NOT the ledger BattlefieldEntriesThisTurn
    // count. This form ACTS ON the subject (puts a counter on it), so per
    // CR 608.2h it needs a currently legal target — a departed object can't be
    // targeted. Contrast the count form (Gargoyle Flock), a pure CR 608.2i
    // look-back tally that survives departure. Flips RED if the target form is
    // ever over-migrated to the ledger.
    let parsed = parse_oracle_text(
        MALAMET,
        "Malamet Battle Glyph",
        &[],
        &["Sorcery".to_string()],
        &[],
    );
    let root = parsed
        .abilities
        .first()
        .expect("Malamet must parse a spell ability");
    let cond = first_ability_condition(root)
        .expect("Malamet's PutCounter rider must carry an if-condition");
    match cond {
        AbilityCondition::TargetMatchesFilter {
            filter, use_lki, ..
        } => {
            assert!(
                !*use_lki,
                "present-tense 'entered this turn' is a current-target check, not LKI: {cond:?}"
            );
            match filter {
                TargetFilter::Typed(tf) => assert!(
                    tf.properties
                        .iter()
                        .any(|p| matches!(p, FilterProp::EnteredThisTurn)),
                    "target form must carry FilterProp::EnteredThisTurn (live target), not the \
                     ledger count: {tf:?}"
                ),
                other => panic!("expected a Typed filter: {other:?}"),
            }
        }
        other => panic!(
            "Malamet's target form must stay AbilityCondition::TargetMatchesFilter — an \
             over-migration to the ledger count form would break CR 608.2h targeting: {other:?}"
        ),
    }
}
