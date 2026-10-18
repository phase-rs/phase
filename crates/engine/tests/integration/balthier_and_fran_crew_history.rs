//! The per-turn crew relation (CR 702.122b + CR 702.122c): a creature "crews"
//! a Vehicle when it is tapped to pay that Vehicle's crew cost, and the Vehicle
//! is then "crewed by" it. Recorded at payment in
//! `GameState::crew_history_this_turn` and read by
//! `FilterProp::CrewedThisTurn` in both directions.
//!
//! Verbatim Oracle text (Scryfall, 2026-10-12):
//! - Balthier and Fran: "Whenever a Vehicle crewed by Balthier and Fran this
//!   turn attacks, if it's the first combat phase of the turn, you may pay
//!   {1}{R}{G}. If you do, after this phase, there is an additional combat
//!   phase." (CR 603.4 + CR 500.8)
//! - Getaway Car, Golden Argosy, Subterranean Schooner, Turtle Van: "… creature
//!   that crewed it this turn …"
//! - Luxurious Locomotive ("for each creature that crewed it this turn", which
//!   counts departed crewers) and Smogbelcher Chariot ("perpetually gains your
//!   choice of …") fail closed.

use engine::game::combat::AttackTarget;
use engine::game::crew_payment::CrewHistoryPair;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{
    ContinuousModification, CrewRelation, Duration, FilterProp, TargetFilter, TargetRef,
};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::counter::CounterType;
use engine::types::game_state::{
    GameState, PersistedGameState, PersistedRestoreFinalization, WaitingFor,
};
use engine::types::identifiers::{ObjectId, ObjectIncarnationRef};
use engine::types::mana::ManaColor;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::triggers::TriggerMode;
use engine::types::zones::Zone;

const BALTHIER: &str = "Reach\nVehicles you control get +1/+1 and have reach and vigilance.\nWhenever a Vehicle crewed by Balthier and Fran this turn attacks, if it's the first combat phase of the turn, you may pay {1}{R}{G}. If you do, after this phase, there is an additional combat phase.";
const GOLDEN_ARGOSY: &str = "Whenever Golden Argosy attacks, exile each creature that crewed it this turn. Return them to the battlefield tapped under their owner's control at the beginning of the next end step.\nCrew 1";
const GETAWAY_CAR: &str = "Haste\nWhenever this Vehicle attacks or blocks, return up to one target creature that crewed it this turn to its owner's hand.\nCrew 1";
const SUBTERRANEAN_SCHOONER: &str =
    "Whenever this Vehicle attacks, target creature that crewed it this turn explores.\nCrew 1";
const TURTLE_VAN: &str = "Whenever this Vehicle attacks, put a +1/+1 counter on target creature that crewed it this turn. Then if that creature is a Mutant, Ninja, or Turtle, double the number of +1/+1 counters on it.\nCrew 1";
const LEISURE_BICYCLE: &str = "Whenever Leisure Bicycle attacks, target creature that crewed it this turn explores.\nCrew 1\nCycling {2}";
const LUXURIOUS_LOCOMOTIVE: &str = "Whenever this Vehicle attacks, create a Treasure token for each creature that crewed it this turn.\nCrew 1. Activate only once each turn.";
const SMOGBELCHER_CHARIOT: &str = "Menace, deathtouch, lifelink\nWhenever Smogbelcher Chariot attacks, target creature that crewed it this turn perpetually gains your choice of menace, deathtouch, or lifelink.\nCrew 3";
const PLAIN_VEHICLE: &str = "Crew 1";
const STIFLE: &str = "Counter target activated or triggered ability.";

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

fn add_balthier(s: &mut GameScenario, player: PlayerId, name: &str) -> ObjectId {
    s.add_creature(player, name, 4, 3)
        .with_subtypes(vec!["Human", "Rabbit"])
        .from_oracle_text(&BALTHIER.replace("Balthier and Fran", name))
        .id()
}

fn add_vehicle(
    s: &mut GameScenario,
    player: PlayerId,
    name: &str,
    oracle: &str,
    power: i32,
    toughness: i32,
) -> ObjectId {
    s.add_creature(player, name, power, toughness)
        .as_artifact()
        .with_subtypes(vec!["Vehicle"])
        .from_oracle_text(oracle)
        .id()
}

/// Three lands that pay Balthier and Fran's {1}{R}{G}.
fn add_payment_lands(s: &mut GameScenario) {
    s.add_basic_land(P0, ManaColor::Red);
    s.add_basic_land(P0, ManaColor::Red);
    s.add_basic_land(P0, ManaColor::Green);
}

/// CR 702.122a: announce the crew ability (tapping `crewers` to pay) and let it
/// resolve.
fn crew(r: &mut GameRunner, vehicle: ObjectId, crewers: &[ObjectId]) {
    announce_crew(r, vehicle, crewers);
    r.advance_until_stack_empty();
}

fn announce_crew(r: &mut GameRunner, vehicle: ObjectId, crewers: &[ObjectId]) {
    r.act(GameAction::CrewVehicle {
        vehicle_id: vehicle,
        creature_ids: vec![],
    })
    .expect("enter crew selection");
    r.act(GameAction::CrewVehicle {
        vehicle_id: vehicle,
        creature_ids: crewers.to_vec(),
    })
    .expect("pay the crew cost");
}

fn incarnation(r: &GameRunner, id: ObjectId) -> ObjectIncarnationRef {
    ObjectIncarnationRef::from_object(&r.state().objects[&id])
}

fn pair(vehicle: ObjectIncarnationRef, crewer: ObjectIncarnationRef) -> CrewHistoryPair {
    CrewHistoryPair { vehicle, crewer }
}

fn is_creature(r: &GameRunner, id: ObjectId) -> bool {
    r.state().objects[&id]
        .card_types
        .core_types
        .contains(&CoreType::Creature)
}

fn blink(r: &mut GameRunner, id: ObjectId) {
    let mut events = Vec::new();
    engine::game::zones::move_to_zone(r.state_mut(), id, Zone::Exile, &mut events);
    engine::game::zones::move_to_zone(r.state_mut(), id, Zone::Battlefield, &mut events);
}

fn gain_control_until_eot(r: &mut GameRunner, source: ObjectId, gainer: PlayerId, id: ObjectId) {
    r.state_mut().add_transient_continuous_effect(
        source,
        gainer,
        Duration::UntilEndOfTurn,
        TargetFilter::SpecificObject { id },
        vec![ContinuousModification::ChangeController],
        None,
    );
    engine::game::layers::flush_layers(r.state_mut());
}

/// The engine's own save/restore pipeline.
fn round_trip(r: &GameRunner) -> GameRunner {
    let json = serde_json::to_string(&PersistedGameState::capture(r.state().clone()))
        .expect("state serializes");
    let state = serde_json::from_str::<PersistedGameState>(&json)
        .expect("persisted state decodes")
        .prepare_for_restore(PersistedRestoreFinalization::DeferUntilRehydrated)
        .expect("persisted state is admissible")
        .finalize_after_rehydration(|_| Ok(()))
        .expect("restored state is publishable");
    GameRunner::from_state(state)
}

// ---------------------------------------------------------------------------
// Turn driver
// ---------------------------------------------------------------------------

struct Script<'a> {
    /// Attackers for the n-th combat phase of the turn (1-based).
    attackers: &'a dyn Fn(u32) -> Vec<ObjectId>,
    /// Answer to every "you may" optional-effect prompt.
    accept_optional: bool,
    /// When simultaneous triggers are ordered, the source whose trigger
    /// resolves first (goes on the stack last).
    resolve_first: Option<ObjectId>,
    /// Target choice for every trigger target prompt.
    choose_target: &'a dyn Fn(&[TargetRef]) -> Option<TargetRef>,
}

#[derive(Default, Debug)]
struct TurnLog {
    /// `(prompted player, source)` of every optional-effect prompt.
    optional_prompts: Vec<(PlayerId, ObjectId)>,
    /// `(combat number, attackers)` of every declaration.
    declarations: Vec<(u32, Vec<ObjectId>)>,
    /// Legal targets of every trigger target prompt.
    target_prompts: Vec<Vec<TargetRef>>,
}

fn combats_started(r: &GameRunner) -> u32 {
    r.state().steps_started_this_turn.count(Phase::BeginCombat)
}

/// Drive the active player's turn to the beginning of its end step.
fn drive_to_end_step(r: &mut GameRunner, script: &Script<'_>) -> TurnLog {
    let mut log = TurnLog::default();
    let turn = r.state().turn_number;
    for _ in 0..400 {
        if r.state().turn_number != turn || matches!(r.state().phase, Phase::End | Phase::Cleanup) {
            return log;
        }
        let action = match r.state().waiting_for.clone() {
            WaitingFor::DeclareAttackers {
                valid_attacker_ids, ..
            } => {
                let combat = combats_started(r);
                let attackers: Vec<ObjectId> = (script.attackers)(combat)
                    .into_iter()
                    .filter(|id| valid_attacker_ids.contains(id))
                    .collect();
                log.declarations.push((combat, attackers.clone()));
                GameAction::DeclareAttackers {
                    attacks: attackers
                        .into_iter()
                        .map(|id| (id, AttackTarget::Player(P1)))
                        .collect(),
                    bands: vec![],
                }
            }
            WaitingFor::DeclareBlockers { .. } => GameAction::DeclareBlockers {
                assignments: vec![],
            },
            WaitingFor::OptionalEffectChoice {
                player, source_id, ..
            } => {
                log.optional_prompts.push((player, source_id));
                GameAction::DecideOptionalEffect {
                    accept: script.accept_optional,
                }
            }
            WaitingFor::OrderTriggers { triggers, .. } => {
                let first = script
                    .resolve_first
                    .and_then(|id| triggers.iter().position(|t| t.source_id == id));
                let mut order: Vec<usize> =
                    (0..triggers.len()).filter(|i| Some(*i) != first).collect();
                order.extend(first);
                GameAction::OrderTriggers { order }
            }
            WaitingFor::TriggerTargetSelection {
                target_slots,
                selection,
                ..
            } => {
                let legal = target_slots[selection.current_slot].legal_targets.clone();
                let target = (script.choose_target)(&legal);
                log.target_prompts.push(legal);
                GameAction::ChooseTarget { target }
            }
            WaitingFor::Priority { .. } | WaitingFor::ManaPayment { .. } => {
                GameAction::PassPriority
            }
            other => panic!("unexpected prompt {other:?}"),
        };
        r.act(action).expect("the turn advances");
    }
    panic!("the turn did not reach its end step");
}

/// Pass through the opponent's whole turn to the start of P0's next
/// precombat main phase, attacking with nothing.
fn pass_to_own_next_main(r: &mut GameRunner) {
    let start = r.state().turn_number;
    for _ in 0..600 {
        let st = r.state();
        if st.turn_number >= start + 2 && st.active_player == P0 && st.phase == Phase::PreCombatMain
        {
            return;
        }
        let action = match r.state().waiting_for.clone() {
            WaitingFor::DeclareAttackers { .. } => GameAction::DeclareAttackers {
                attacks: vec![],
                bands: vec![],
            },
            WaitingFor::DeclareBlockers { .. } => GameAction::DeclareBlockers {
                assignments: vec![],
            },
            WaitingFor::Priority { .. } => GameAction::PassPriority,
            other => panic!("unexpected prompt {other:?}"),
        };
        r.act(action).expect("the turn cycle advances");
    }
    panic!("did not reach P0's next main phase");
}

fn no_targets(_: &[TargetRef]) -> Option<TargetRef> {
    None
}

fn balthier_prompts(log: &TurnLog, balthier: ObjectId) -> usize {
    log.optional_prompts
        .iter()
        .filter(|(_, source)| *source == balthier)
        .count()
}

// ---------------------------------------------------------------------------
// Balthier and Fran — "a Vehicle crewed by ~ this turn"
// ---------------------------------------------------------------------------

/// Probe 1 (base case + source departure): Balthier crews Golden Argosy
/// through `CrewVehicle`; the game is saved and restored; Argosy attacks in the
/// first combat. Both attack triggers go on the stack; Argosy's resolves first
/// and exiles Balthier (it crewed Argosy). Balthier's trigger still resolves
/// (CR 113.7a), the first-combat condition still holds on resolution (CR
/// 603.4), the payment is made, and a second combat phase follows (CR 500.8).
#[test]
fn balthier_crewed_vehicle_buys_an_extra_combat_across_save_restore() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let balthier = add_balthier(&mut s, P0, "Balthier and Fran");
    let argosy = add_vehicle(&mut s, P0, "Golden Argosy", GOLDEN_ARGOSY, 3, 6);
    add_payment_lands(&mut s);
    let mut r = s.build();

    crew(&mut r, argosy, &[balthier]);
    assert!(is_creature(&r, argosy), "reach: the crew resolved");
    let recorded = pair(incarnation(&r, argosy), incarnation(&r, balthier));
    let mut r = round_trip(&r);
    assert!(
        r.state().crew_history_this_turn.contains(&recorded),
        "reach: the crew relation survives save/restore"
    );

    let log = drive_to_end_step(
        &mut r,
        &Script {
            attackers: &|combat| if combat == 1 { vec![argosy] } else { vec![] },
            accept_optional: true,
            resolve_first: Some(argosy),
            choose_target: &no_targets,
        },
    );

    assert_eq!(
        log.declarations[0],
        (1, vec![argosy]),
        "reach: Argosy attacked"
    );
    assert_eq!(
        r.state().objects[&balthier].zone,
        Zone::Exile,
        "reach: Argosy's trigger exiled Balthier before Balthier's trigger resolved"
    );
    assert_eq!(
        balthier_prompts(&log, balthier),
        1,
        "Balthier's trigger offered the payment: {log:?}"
    );
    assert_eq!(
        combats_started(&r),
        2,
        "paying {{1}}{{R}}{{G}} adds exactly one combat phase"
    );
}

/// Negative control: a Vehicle crewed only by another creature does not
/// trigger Balthier — the relation is "crewed by ~", not "a Vehicle attacks".
#[test]
fn vehicle_crewed_by_another_creature_does_not_trigger_balthier() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let balthier = add_balthier(&mut s, P0, "Balthier and Fran");
    let bear = s.add_creature(P0, "Bear", 2, 2).id();
    let vehicle = add_vehicle(&mut s, P0, "Test Vehicle", PLAIN_VEHICLE, 3, 3);
    add_payment_lands(&mut s);
    let mut r = s.build();

    crew(&mut r, vehicle, &[bear]);
    assert!(
        r.state()
            .crew_history_this_turn
            .contains(&pair(incarnation(&r, vehicle), incarnation(&r, bear))),
        "reach: the bear's crew payment was recorded"
    );
    let log = drive_to_end_step(
        &mut r,
        &Script {
            attackers: &|_| vec![vehicle],
            accept_optional: true,
            resolve_first: None,
            choose_target: &no_targets,
        },
    );
    assert_eq!(
        log.declarations[0],
        (1, vec![vehicle]),
        "reach: the Vehicle attacked"
    );
    assert_eq!(r.state().objects[&balthier].zone, Zone::Battlefield);
    assert_eq!(balthier_prompts(&log, balthier), 0, "no Balthier trigger");
    assert_eq!(combats_started(&r), 1);
}

/// CR 400.7: Balthier crews, then leaves and returns — the returned object
/// crewed nothing, so the attack does not trigger it.
#[test]
fn blinked_balthier_is_a_new_object_that_crewed_nothing() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let balthier = add_balthier(&mut s, P0, "Balthier and Fran");
    let vehicle = add_vehicle(&mut s, P0, "Test Vehicle", PLAIN_VEHICLE, 3, 3);
    add_payment_lands(&mut s);
    let mut r = s.build();

    crew(&mut r, vehicle, &[balthier]);
    let before = incarnation(&r, balthier);
    blink(&mut r, balthier);
    assert_ne!(
        incarnation(&r, balthier),
        before,
        "reach: a new incarnation"
    );
    assert!(
        r.state().objects[&balthier]
            .trigger_definitions
            .iter_unchecked()
            .any(|entry| entry.definition.mode == TriggerMode::Attacks),
        "reach: the returned Balthier still has its attack trigger"
    );

    let log = drive_to_end_step(
        &mut r,
        &Script {
            attackers: &|_| vec![vehicle],
            accept_optional: true,
            resolve_first: None,
            choose_target: &no_targets,
        },
    );
    assert_eq!(
        log.declarations[0],
        (1, vec![vehicle]),
        "reach: the Vehicle attacked"
    );
    assert_eq!(balthier_prompts(&log, balthier), 0);
    assert_eq!(combats_started(&r), 1);
}

/// "This turn": a crew payment from a previous turn does not count. The
/// ledger expires at turn start; a fresh crew by another creature next turn
/// does not trigger Balthier.
#[test]
fn last_turns_crew_payment_has_expired() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let balthier = add_balthier(&mut s, P0, "Balthier and Fran");
    let bear = s.add_creature(P0, "Bear", 2, 2).id();
    let vehicle = add_vehicle(&mut s, P0, "Test Vehicle", PLAIN_VEHICLE, 3, 3);
    add_payment_lands(&mut s);
    s.with_library_top(P0, &["Card A", "Card B"]);
    s.with_library_top(P1, &["Card C", "Card D"]);
    let mut r = s.build();

    crew(&mut r, vehicle, &[balthier]);
    assert!(
        !r.state().crew_history_this_turn.is_empty(),
        "reach: Balthier's payment was recorded this turn"
    );
    pass_to_own_next_main(&mut r);
    assert!(
        r.state().crew_history_this_turn.is_empty(),
        "the per-turn crew relation expires at turn start"
    );

    crew(&mut r, vehicle, &[bear]);
    assert!(
        is_creature(&r, vehicle),
        "reach: the Vehicle is crewed again"
    );
    let log = drive_to_end_step(
        &mut r,
        &Script {
            attackers: &|_| vec![vehicle],
            accept_optional: true,
            resolve_first: None,
            choose_target: &no_targets,
        },
    );
    assert_eq!(
        log.declarations[0],
        (1, vec![vehicle]),
        "reach: the Vehicle attacked"
    );
    assert_eq!(balthier_prompts(&log, balthier), 0);
}

/// CR 702.122c: "crewed by" names the creatures tapped to PAY the crew cost.
/// Balthier's crew ability is countered after payment; a second crew by a bear
/// animates the Vehicle; the attack still triggers Balthier. Declining the
/// payment adds no combat ("If you do").
#[test]
fn countered_crew_payment_still_makes_the_vehicle_crewed_by_balthier() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let balthier = add_balthier(&mut s, P0, "Balthier and Fran");
    let bear = s.add_creature(P0, "Bear", 2, 2).id();
    let vehicle = add_vehicle(&mut s, P0, "Test Vehicle", PLAIN_VEHICLE, 3, 3);
    let stifle = s
        .add_spell_to_hand_from_oracle(P0, "Stifle", true, STIFLE)
        .id();
    add_payment_lands(&mut s);
    let mut r = s.build();

    announce_crew(&mut r, vehicle, &[balthier]);
    let crew_entry = r
        .state()
        .stack
        .back()
        .expect("the crew ability is on the stack")
        .id;
    r.cast(stifle).target_object(crew_entry).resolve();
    assert!(r.state().stack.is_empty(), "reach: the stack resolved");
    assert!(
        !is_creature(&r, vehicle),
        "reach: the countered crew ability did not animate the Vehicle"
    );

    crew(&mut r, vehicle, &[bear]);
    assert!(is_creature(&r, vehicle), "reach: the bear's crew resolved");
    let log = drive_to_end_step(
        &mut r,
        &Script {
            attackers: &|_| vec![vehicle],
            accept_optional: false,
            resolve_first: None,
            choose_target: &no_targets,
        },
    );
    assert_eq!(
        log.optional_prompts,
        vec![(P0, balthier)],
        "the Vehicle was crewed by Balthier this turn (CR 702.122c)"
    );
    assert_eq!(
        combats_started(&r),
        1,
        "declining the payment adds no combat"
    );
}

/// Multiple crew payments, and the intervening-if: the Vehicle is crewed by a
/// bear and then by Balthier. Its first-combat attack offers the payment; it
/// attacks again in the added combat, where "if it's the first combat phase"
/// is false, so Balthier does not trigger again and no third combat follows.
#[test]
fn added_combat_attack_does_not_retrigger() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let balthier = add_balthier(&mut s, P0, "Balthier and Fran");
    let bear = s.add_creature(P0, "Bear", 2, 2).id();
    let vehicle = add_vehicle(&mut s, P0, "Test Vehicle", PLAIN_VEHICLE, 3, 3);
    add_payment_lands(&mut s);
    let mut r = s.build();

    crew(&mut r, vehicle, &[bear]);
    crew(&mut r, vehicle, &[balthier]);
    assert_eq!(
        r.state().crew_history_this_turn.len(),
        2,
        "reach: both crew payments were recorded"
    );
    let log = drive_to_end_step(
        &mut r,
        &Script {
            attackers: &|_| vec![vehicle],
            accept_optional: true,
            resolve_first: None,
            choose_target: &no_targets,
        },
    );
    assert_eq!(
        log.declarations,
        vec![(1, vec![vehicle]), (2, vec![vehicle])],
        "reach: the Vehicle attacked in both combats (vigilance from Balthier)"
    );
    assert_eq!(balthier_prompts(&log, balthier), 1);
    assert_eq!(combats_started(&r), 2);
}

/// Control change: Balthier's controller changes after it crews. The relation
/// is about the object, and the trigger has no "you control" restriction — the
/// attack still triggers it, under its new controller.
#[test]
fn balthier_triggers_for_its_current_controller_after_a_control_change() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let balthier = add_balthier(&mut s, P0, "Balthier and Fran");
    let vehicle = add_vehicle(&mut s, P0, "Test Vehicle", PLAIN_VEHICLE, 3, 3);
    let thief = s.add_creature(P1, "Thief", 1, 1).id();
    let mut r = s.build();

    crew(&mut r, vehicle, &[balthier]);
    gain_control_until_eot(&mut r, thief, P1, balthier);
    assert_eq!(
        r.state().objects[&balthier].controller,
        P1,
        "reach: P1 controls Balthier"
    );

    let log = drive_to_end_step(
        &mut r,
        &Script {
            attackers: &|_| vec![vehicle],
            accept_optional: false,
            resolve_first: None,
            choose_target: &no_targets,
        },
    );
    assert_eq!(
        log.declarations[0],
        (1, vec![vehicle]),
        "reach: the Vehicle attacked"
    );
    assert_eq!(log.optional_prompts, vec![(P1, balthier)]);
}

/// A second object with the same text names itself with "~": only the object
/// that actually crewed the Vehicle triggers.
#[test]
fn only_the_crewing_object_triggers_not_a_same_text_twin() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let balthier = add_balthier(&mut s, P0, "Balthier and Fran");
    let twin = add_balthier(&mut s, P0, "Balthier Twin");
    let vehicle = add_vehicle(&mut s, P0, "Test Vehicle", PLAIN_VEHICLE, 3, 3);
    let mut r = s.build();

    crew(&mut r, vehicle, &[balthier]);
    let log = drive_to_end_step(
        &mut r,
        &Script {
            attackers: &|_| vec![vehicle],
            accept_optional: false,
            resolve_first: None,
            choose_target: &no_targets,
        },
    );
    assert_eq!(
        log.declarations[0],
        (1, vec![vehicle]),
        "reach: the Vehicle attacked"
    );
    assert_eq!(log.optional_prompts, vec![(P0, balthier)]);
    assert_eq!(balthier_prompts(&log, twin), 0);
}

// ---------------------------------------------------------------------------
// "creature that crewed it this turn"
// ---------------------------------------------------------------------------

/// Probe 2: Golden Argosy is crewed by two creatures; one crewer's controller
/// changes; Argosy attacks and leaves before its trigger resolves. Both
/// crewers are exiled (the source is the triggering incarnation, CR 113.7a),
/// and the unrelated creatures are untouched. (The end-step return is a
/// separate, pre-existing tracked-set return defect, not exercised here.)
#[test]
fn golden_argosy_exiles_exactly_its_crewers_even_after_it_leaves() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let argosy = add_vehicle(&mut s, P0, "Golden Argosy", GOLDEN_ARGOSY, 3, 6);
    let crewer_a = s.add_creature(P0, "Crewer A", 1, 1).id();
    let crewer_b = s.add_creature(P0, "Crewer B", 1, 1).id();
    let bystander = s.add_creature(P0, "Bystander", 1, 1).id();
    let thief = s.add_creature(P1, "Thief", 1, 1).id();
    let mut r = s.build();

    crew(&mut r, argosy, &[crewer_a, crewer_b]);
    gain_control_until_eot(&mut r, thief, P1, crewer_b);
    assert_eq!(
        r.state().objects[&crewer_b].controller,
        P1,
        "reach: control changed"
    );

    r.advance_to_combat();
    r.declare_attackers(&[(argosy, AttackTarget::Player(P1))])
        .expect("Argosy attacks");
    assert!(
        r.state()
            .stack
            .iter()
            .any(|entry| entry.source_id == argosy),
        "reach: Argosy's attack trigger is on the stack"
    );
    let mut events = Vec::new();
    engine::game::zones::move_to_zone(r.state_mut(), argosy, Zone::Graveyard, &mut events);
    r.advance_until_stack_empty();

    assert_eq!(r.state().objects[&crewer_a].zone, Zone::Exile);
    assert_eq!(r.state().objects[&crewer_b].zone, Zone::Exile);
    assert_eq!(r.state().objects[&bystander].zone, Zone::Battlefield);
    assert_eq!(
        r.state().objects[&thief].zone,
        Zone::Battlefield,
        "a creature that never crewed Argosy is not exiled"
    );
}

/// Probe 3a: Getaway Car's attack branch — only a creature that crewed it is a
/// legal target; the chosen crewer returns to its owner's hand.
#[test]
fn getaway_car_attack_returns_only_a_crewer() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let car = add_vehicle(&mut s, P0, "Getaway Car", GETAWAY_CAR, 4, 3);
    let crewer = s.add_creature(P0, "Crewer", 1, 1).id();
    let non_crewer = s.add_creature(P0, "Non-crewer", 1, 1).id();
    let opposing = s.add_creature(P1, "Opposing", 1, 1).id();
    let mut r = s.build();

    crew(&mut r, car, &[crewer]);
    let log = drive_to_end_step(
        &mut r,
        &Script {
            attackers: &|_| vec![car],
            accept_optional: true,
            resolve_first: None,
            choose_target: &|_| Some(TargetRef::Object(crewer)),
        },
    );
    let legal = log
        .target_prompts
        .first()
        .expect("reach: the attack trigger asked for a target");
    assert!(legal.contains(&TargetRef::Object(crewer)));
    assert!(!legal.contains(&TargetRef::Object(non_crewer)));
    assert!(!legal.contains(&TargetRef::Object(opposing)));
    assert_eq!(r.state().objects[&crewer].zone, Zone::Hand);
    assert_eq!(r.state().objects[&non_crewer].zone, Zone::Battlefield);
}

/// Probe 3b: Getaway Car's block branch, with zero targets chosen ("up to
/// one") — the trigger resolves harmlessly.
#[test]
fn getaway_car_block_with_zero_targets_resolves_harmlessly() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let car = add_vehicle(&mut s, P0, "Getaway Car", GETAWAY_CAR, 4, 3);
    let crewer = s.add_creature(P0, "Crewer", 1, 1).id();
    let non_crewer = s.add_creature(P0, "Non-crewer", 1, 1).id();
    let attacker = s.add_creature(P1, "Attacker", 2, 2).id();
    let mut r = s.build();

    // P1's turn; P0 holds priority in P1's precombat main and crews.
    r.state_mut().active_player = P1;
    r.state_mut().priority_player = P0;
    r.state_mut().waiting_for = WaitingFor::Priority { player: P0 };
    crew(&mut r, car, &[crewer]);
    assert!(
        is_creature(&r, car),
        "reach: the Car is crewed on P1's turn"
    );

    r.advance_to_combat();
    r.act(GameAction::DeclareAttackers {
        attacks: vec![(attacker, AttackTarget::Player(P0))],
        bands: vec![],
    })
    .expect("P1 attacks");
    for _ in 0..8 {
        if matches!(r.state().waiting_for, WaitingFor::DeclareBlockers { .. }) {
            break;
        }
        r.act(GameAction::PassPriority)
            .expect("advance to blockers");
    }
    r.declare_blockers(&[(car, attacker)])
        .expect("the Car blocks");

    let WaitingFor::TriggerTargetSelection {
        target_slots,
        selection,
        ..
    } = r.state().waiting_for.clone()
    else {
        panic!(
            "expected the block trigger's target prompt, got {:?}",
            r.state().waiting_for
        );
    };
    let legal = &target_slots[selection.current_slot].legal_targets;
    assert!(
        legal.contains(&TargetRef::Object(crewer)),
        "reach: the crewer is legal"
    );
    assert!(!legal.contains(&TargetRef::Object(non_crewer)));
    r.act(GameAction::ChooseTarget { target: None })
        .expect("up to one: zero targets is legal");
    r.advance_until_stack_empty();
    assert_eq!(r.state().objects[&crewer].zone, Zone::Battlefield);
    assert_eq!(r.state().objects[&non_crewer].zone, Zone::Battlefield);
}

/// Subterranean Schooner: only a crewer may explore; it reveals a land, which
/// goes to its controller's hand.
#[test]
fn subterranean_schooner_crewer_explores() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let schooner = add_vehicle(
        &mut s,
        P0,
        "Subterranean Schooner",
        SUBTERRANEAN_SCHOONER,
        3,
        4,
    );
    let crewer = s.add_creature(P0, "Crewer", 1, 1).id();
    let second_crewer = s.add_creature(P0, "Second Crewer", 1, 1).id();
    let non_crewer = s.add_creature(P0, "Non-crewer", 1, 1).id();
    let forest = s.add_land_to_library_top(P0, "Forest").id();
    let mut r = s.build();

    // Two crewers, so the single-target trigger asks rather than auto-targets.
    crew(&mut r, schooner, &[crewer, second_crewer]);
    let log = drive_to_end_step(
        &mut r,
        &Script {
            attackers: &|_| vec![schooner],
            accept_optional: true,
            resolve_first: None,
            choose_target: &|_| Some(TargetRef::Object(crewer)),
        },
    );
    let legal = log
        .target_prompts
        .first()
        .expect("reach: the attack trigger asked for a target");
    assert!(legal.contains(&TargetRef::Object(crewer)));
    assert!(!legal.contains(&TargetRef::Object(non_crewer)));
    assert_eq!(
        r.state().objects[&forest].zone,
        Zone::Hand,
        "the crewer explored"
    );
}

/// Turtle Van: two crewers; only crewers are legal; a Turtle crewer gets the
/// +1/+1 counter and then the doubling (CR 122.1).
#[test]
fn turtle_van_counters_a_turtle_crewer_and_doubles_them() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let van = add_vehicle(&mut s, P0, "Turtle Van", TURTLE_VAN, 4, 4);
    let turtle = s
        .add_creature(P0, "Turtle", 1, 1)
        .with_subtypes(vec!["Turtle"])
        .id();
    let bear = s.add_creature(P0, "Bear", 1, 1).id();
    let non_crewer = s.add_creature(P0, "Non-crewer", 1, 1).id();
    let mut r = s.build();

    crew(&mut r, van, &[turtle, bear]);
    let log = drive_to_end_step(
        &mut r,
        &Script {
            attackers: &|_| vec![van],
            accept_optional: true,
            resolve_first: None,
            choose_target: &|_| Some(TargetRef::Object(turtle)),
        },
    );
    let legal = log
        .target_prompts
        .first()
        .expect("reach: the attack trigger asked for a target");
    assert!(legal.contains(&TargetRef::Object(turtle)));
    assert!(legal.contains(&TargetRef::Object(bear)));
    assert!(!legal.contains(&TargetRef::Object(non_crewer)));
    let counters = |id: ObjectId| {
        r.state().objects[&id]
            .counters
            .get(&CounterType::Plus1Plus1)
            .copied()
            .unwrap_or(0)
    };
    assert_eq!(counters(turtle), 2, "one counter, then doubled");
    assert_eq!(counters(bear), 0);
}

// ---------------------------------------------------------------------------
// Parse shapes and fail-closed siblings
// ---------------------------------------------------------------------------

fn parse(name: &str, oracle: &str) -> engine::parser::oracle::ParsedAbilities {
    engine::parser::oracle::parse_oracle_text(
        oracle,
        name,
        &[],
        &["Artifact".to_string()],
        &["Vehicle".to_string()],
    )
}

fn json(value: &impl serde::Serialize) -> String {
    serde_json::to_string(value).expect("serializes")
}

/// The complete turn-scoped clause reaches every target-filter carrier.
#[test]
fn crewed_it_this_turn_lands_in_each_target_filter() {
    let crewed_source = json(&FilterProp::CrewedThisTurn {
        relation: CrewRelation::CrewedSource,
    });
    for (name, oracle) in [
        ("Getaway Car", GETAWAY_CAR),
        ("Golden Argosy", GOLDEN_ARGOSY),
        ("Leisure Bicycle", LEISURE_BICYCLE),
        ("Subterranean Schooner", SUBTERRANEAN_SCHOONER),
        ("Turtle Van", TURTLE_VAN),
    ] {
        let parsed = parse(name, oracle);
        assert_eq!(parsed.triggers.len(), 1, "{name}: one trigger");
        let trigger = json(&parsed.triggers[0]);
        assert!(trigger.contains(&crewed_source), "{name}: {trigger}");
        assert!(!trigger.contains("Unimplemented"), "{name}: {trigger}");
    }

    let parsed = engine::parser::oracle::parse_oracle_text(
        BALTHIER,
        "Balthier and Fran",
        &[],
        &["Creature".to_string()],
        &["Human".to_string(), "Rabbit".to_string()],
    );
    let trigger = &parsed.triggers[0];
    assert_eq!(trigger.mode, TriggerMode::Attacks);
    assert!(
        json(&trigger.valid_card).contains(&json(&FilterProp::CrewedThisTurn {
            relation: CrewRelation::CrewedBySource,
        }))
    );
}

/// Luxurious Locomotive counts crewers that have since left the battlefield
/// (its ruling); a live-membership count would undercount, so the ability is
/// unsupported rather than wrong.
#[test]
fn luxurious_locomotive_count_fails_closed() {
    let parsed = parse("Luxurious Locomotive", LUXURIOUS_LOCOMOTIVE);
    let all = json(&parsed);
    assert!(
        all.contains("crew_history_population_count"),
        "the crewer count must be an honest gap: {all}"
    );
    assert!(
        !json(&parsed.triggers).contains("ObjectCount"),
        "no live-membership count may remain attributed as supported"
    );
}

/// Smogbelcher Chariot's grant is perpetual; no arm models a choice among
/// perpetual keyword grants, so the clause is unsupported rather than an
/// until-end-of-turn grant.
#[test]
fn smogbelcher_chariot_perpetual_choice_fails_closed() {
    let parsed = parse("Smogbelcher Chariot", SMOGBELCHER_CHARIOT);
    let trigger = json(&parsed.triggers);
    assert!(trigger.contains("perpetual_gain_choice"), "{trigger}");
    assert!(!trigger.contains("ChooseOneOf"), "{trigger}");
}

/// Serde: a nonempty ledger round-trips; a payload without the field (legacy
/// save) loads with an empty ledger.
#[test]
fn crew_history_serde_round_trip_and_legacy_default() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let bear = s.add_creature(P0, "Bear", 2, 2).id();
    let vehicle = add_vehicle(&mut s, P0, "Test Vehicle", PLAIN_VEHICLE, 3, 3);
    let mut r = s.build();
    crew(&mut r, vehicle, &[bear]);

    let mut value = serde_json::to_value(r.state()).expect("serializes");
    let restored: GameState = serde_json::from_value(value.clone()).expect("deserializes");
    assert_eq!(
        restored.crew_history_this_turn,
        r.state().crew_history_this_turn,
        "a nonempty ledger round-trips"
    );
    let removed = value
        .as_object_mut()
        .expect("GameState is an object")
        .remove("crew_history_this_turn");
    assert!(removed.is_some(), "reach: a nonempty ledger is serialized");
    let legacy: GameState = serde_json::from_value(value).expect("legacy payload deserializes");
    assert!(legacy.crew_history_this_turn.is_empty());
}
