//! Dominion Saboteur: "You may have this creature enter as a copy of any
//! artifact or creature on the battlefield, except it isn't legendary. If you
//! do, it enters with additional counters on it equal to the same number and
//! kinds of counters the copied permanent has on it."
//!
//! CR 707.2: counters are not copiable values, so the rider reproduces the
//! copied permanent's counters explicitly (CR 122.6 entry counters), and only
//! when the copy happens (CR 614.12a choice).

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::card_type::Supertype;
use engine::types::counter::CounterType;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const DOMINION_SABOTEUR: &str = "You may have this creature enter as a copy of any artifact or creature on the battlefield, except it isn't legendary. If you do, it enters with additional counters on it equal to the same number and kinds of counters the copied permanent has on it.";

fn cast_to_copy_offer(runner: &mut GameRunner, saboteur: ObjectId) {
    let card_id = runner.state().objects[&saboteur].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: saboteur,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast Dominion Saboteur");
    runner.advance_until_stack_empty();
    let WaitingFor::ReplacementChoice { candidates, .. } = &runner.state().waiting_for else {
        panic!(
            "Dominion Saboteur must offer its optional enter-as-copy replacement, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(
        candidates.len(),
        2,
        "copy offer includes an explicit decline"
    );
}

fn accept_copy_of(runner: &mut GameRunner, donor: ObjectId) {
    runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("accept the copy replacement");
    let WaitingFor::CopyTargetChoice { valid_targets, .. } = &runner.state().waiting_for else {
        panic!(
            "accepting must prompt for the copy source, got {:?}",
            runner.state().waiting_for
        );
    };
    assert!(valid_targets.contains(&donor));
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(donor)),
        })
        .expect("choose the copy source");
}

fn assert_settled(runner: &mut GameRunner) {
    runner.advance_until_stack_empty();
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "entry did not reach priority: {:?}",
        runner.state().waiting_for
    );
    assert!(runner.state().stack.is_empty());
}

#[test]
fn copying_a_legendary_creature_drops_legendary_and_mirrors_its_counters() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let donor = scenario
        .add_creature_from_oracle(P1, "Legendary Bear", 2, 2, "")
        .as_legendary()
        .id();
    scenario.with_counter(donor, CounterType::Plus1Plus1, 2);
    let saboteur = scenario
        .add_creature_to_hand_from_oracle(P0, "Dominion Saboteur", 3, 3, DOMINION_SABOTEUR)
        .id();
    let mut runner = scenario.build();

    cast_to_copy_offer(&mut runner, saboteur);
    accept_copy_of(&mut runner, donor);
    assert_settled(&mut runner);

    let entered = &runner.state().objects[&saboteur];
    assert_eq!(entered.zone, Zone::Battlefield);
    assert_eq!(entered.name, "Legendary Bear");
    assert!(
        !entered
            .card_types
            .supertypes
            .contains(&Supertype::Legendary),
        "CR 707.9b: the copy exception removes legendary"
    );
    assert_eq!(
        entered.counters.get(&CounterType::Plus1Plus1).copied(),
        Some(2),
        "the copy enters with the donor's two +1/+1 counters"
    );
    assert_eq!(entered.counters.len(), 1);
    assert_eq!((entered.power, entered.toughness), (Some(4), Some(4)));
    // The donor keeps its own counters; the rider adds, it does not move.
    assert_eq!(
        runner.state().objects[&donor]
            .counters
            .get(&CounterType::Plus1Plus1)
            .copied(),
        Some(2)
    );
    // The legend rule never applies: the copy is not legendary.
    assert_eq!(runner.state().objects[&donor].zone, Zone::Battlefield);
}

#[test]
fn copying_an_artifact_mirrors_every_counter_kind() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let donor = scenario
        .add_artifact_from_oracle(P0, "Charged Relic", "")
        .id();
    let charge = CounterType::Generic("charge".to_string());
    scenario.with_counter(donor, charge.clone(), 3);
    scenario.with_counter(donor, CounterType::Plus1Plus1, 1);
    let saboteur = scenario
        .add_creature_to_hand_from_oracle(P0, "Dominion Saboteur", 3, 3, DOMINION_SABOTEUR)
        .id();
    let mut runner = scenario.build();

    cast_to_copy_offer(&mut runner, saboteur);
    accept_copy_of(&mut runner, donor);
    assert_settled(&mut runner);

    let entered = &runner.state().objects[&saboteur];
    assert_eq!(entered.zone, Zone::Battlefield);
    assert_eq!(entered.name, "Charged Relic");
    assert_eq!(entered.counters.get(&charge).copied(), Some(3));
    assert_eq!(
        entered.counters.get(&CounterType::Plus1Plus1).copied(),
        Some(1)
    );
    assert_eq!(entered.counters.len(), 2);
}

#[test]
fn declining_the_copy_enters_a_plain_creature_without_counters() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let donor = scenario
        .add_creature_from_oracle(P1, "Legendary Bear", 2, 2, "")
        .as_legendary()
        .id();
    scenario.with_counter(donor, CounterType::Plus1Plus1, 2);
    let saboteur = scenario
        .add_creature_to_hand_from_oracle(P0, "Dominion Saboteur", 3, 3, DOMINION_SABOTEUR)
        .id();
    let mut runner = scenario.build();

    cast_to_copy_offer(&mut runner, saboteur);
    runner
        .act(GameAction::ChooseReplacement { index: 1 })
        .expect("decline the copy replacement");
    assert_settled(&mut runner);

    let entered = &runner.state().objects[&saboteur];
    assert_eq!(entered.zone, Zone::Battlefield);
    assert_eq!(entered.name, "Dominion Saboteur");
    assert!(
        entered.counters.is_empty(),
        "\"If you do\": no counters without the copy, got {:?}",
        entered.counters
    );
    assert_eq!((entered.power, entered.toughness), (Some(3), Some(3)));
}

/// Zombify {3}{B} Sorcery — verbatim Oracle text.
const ZOMBIFY: &str = "Return target creature card from your graveyard to the battlefield.";
/// Cackling Counterpart {1}{U}{U} Instant — verbatim Oracle text.
const CACKLING_COUNTERPART: &str = "Create a token that's a copy of target creature you control.\nFlashback {5}{U}{U} (You may cast this card from your graveyard for its flashback cost. Then exile it.)";

/// CR 614.1c + CR 614.12a: the enter-as-copy replacement applies however the
/// creature enters, not only when it is cast. Returned from the graveyard by
/// an effect, it still copies the chosen permanent and its counters.
#[test]
fn returned_from_the_graveyard_it_still_copies_the_counters() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let donor = scenario
        .add_creature_from_oracle(P1, "Legendary Bear", 2, 2, "")
        .as_legendary()
        .id();
    scenario.with_counter(donor, CounterType::Plus1Plus1, 2);
    let saboteur = scenario
        .add_creature_to_graveyard(P0, "Dominion Saboteur", 3, 3)
        .from_oracle_text(DOMINION_SABOTEUR)
        .id();
    let zombify = scenario
        .add_spell_to_hand_from_oracle(P0, "Zombify", false, ZOMBIFY)
        .id();
    let mut runner = scenario.build();

    let outcome = runner
        .cast(zombify)
        .target_object(saboteur)
        .replacement_choice(0)
        .copy_target(donor)
        .resolve();
    assert_eq!(outcome.zone_of(saboteur), Zone::Battlefield);

    let entered = &runner.state().objects[&saboteur];
    assert_eq!(entered.name, "Legendary Bear");
    assert!(!entered
        .card_types
        .supertypes
        .contains(&Supertype::Legendary));
    assert_eq!(
        entered.counters.get(&CounterType::Plus1Plus1).copied(),
        Some(2)
    );
    assert_eq!(entered.counters.len(), 1);
    assert_eq!((entered.power, entered.toughness), (Some(4), Some(4)));
}

/// CR 707.2 + CR 614.1c: a token copy of an uncopied Dominion Saboteur has its
/// enter-as-copy ability as it enters, so the token may itself enter as a copy
/// of another permanent, with that permanent's counters.
#[test]
fn a_token_copy_created_by_an_effect_copies_the_counters() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let donor = scenario
        .add_artifact_from_oracle(P1, "Charged Relic", "")
        .id();
    let charge = CounterType::Generic("charge".to_string());
    scenario.with_counter(donor, charge.clone(), 3);
    let saboteur = scenario
        .add_creature_from_oracle(P0, "Dominion Saboteur", 3, 3, DOMINION_SABOTEUR)
        .id();
    let counterpart = scenario
        .add_spell_to_hand_from_oracle(P0, "Cackling Counterpart", true, CACKLING_COUNTERPART)
        .id();
    let mut runner = scenario.build();

    let _ = runner
        .cast(counterpart)
        .target_object(saboteur)
        .replacement_choice(0)
        .copy_target(donor)
        .resolve();
    assert_settled(&mut runner);

    let state = runner.state();
    let tokens: Vec<_> = state
        .objects
        .values()
        .filter(|object| object.is_token && object.zone == Zone::Battlefield)
        .collect();
    assert_eq!(tokens.len(), 1, "exactly one token copy enters");
    let token = tokens[0];
    assert_eq!(token.name, "Charged Relic");
    assert_eq!(
        token.counters.get(&charge).copied(),
        Some(3),
        "the token enters with the copied artifact's three charge counters"
    );
    assert_eq!(token.counters.len(), 1);
    // The uncopied Saboteur the token copied is untouched.
    let original = &state.objects[&saboteur];
    assert_eq!(original.name, "Dominion Saboteur");
    assert!(original.counters.is_empty());
}

// ── Acquired enter-as-copy replacements (CR 614.12 + CR 616.1f + CR 707.9e) ──

/// Sculpting Steel {3} Artifact — verbatim Oracle text.
const SCULPTING_STEEL: &str =
    "You may have this artifact enter as a copy of any artifact on the battlefield.";

/// Answers the copy-source prompt with `donor`, asserting it is a legal choice.
fn choose_copy_source(runner: &mut GameRunner, donor: ObjectId) {
    let WaitingFor::CopyTargetChoice { valid_targets, .. } = &runner.state().waiting_for else {
        panic!(
            "expected a copy-source prompt, got {:?}",
            runner.state().waiting_for
        );
    };
    assert!(
        valid_targets.contains(&donor),
        "{donor:?} must be a legal copy source"
    );
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(donor)),
        })
        .expect("choose the copy source");
}

/// CR 614.12 + CR 616.1f: entering as a copy of an uncopied Sculpting Steel
/// gives the entering permanent Steel's own optional "enter as a copy"
/// replacement, which now applies to the same entry. Asserts that offer, with
/// its explicit decline, is raised for the entrant.
fn assert_acquired_copy_offer(runner: &GameRunner, entrant: ObjectId) {
    let WaitingFor::ReplacementChoice { candidates, .. } = &runner.state().waiting_for else {
        panic!(
            "the acquired enter-as-copy replacement must be offered, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(candidates.len(), 2, "the acquired offer includes a decline");
    assert!(
        candidates
            .iter()
            .all(|candidate| candidate.source_id == entrant),
        "the acquired replacement belongs to the entering permanent: {candidates:?}"
    );
}

struct AcquiredCopyBoard {
    runner: GameRunner,
    saboteur: ObjectId,
    steel: ObjectId,
    relic: ObjectId,
}

/// A textless test artifact's only ability: an entry trigger, so a test can
/// count how many times the realized copy's entry was observed.
const RELIC_ENTRY_TRIGGER: &str = "When this artifact enters, you gain 2 life.";

/// An uncopied Sculpting Steel with two +1/+1 counters, a Charged Relic (with
/// an entry trigger) with three charge counters, and Dominion Saboteur in hand.
fn acquired_copy_board() -> AcquiredCopyBoard {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let steel = scenario
        .add_artifact_from_oracle(P1, "Sculpting Steel", SCULPTING_STEEL)
        .id();
    scenario.with_counter(steel, CounterType::Plus1Plus1, 2);
    let relic = scenario
        .add_artifact_from_oracle(P1, "Charged Relic", RELIC_ENTRY_TRIGGER)
        .id();
    scenario.with_counter(relic, CounterType::Generic("charge".to_string()), 3);
    let saboteur = scenario
        .add_creature_to_hand_from_oracle(P0, "Dominion Saboteur", 3, 3, DOMINION_SABOTEUR)
        .id();
    AcquiredCopyBoard {
        runner: scenario.build(),
        saboteur,
        steel,
        relic,
    }
}

/// CR 616.1c + CR 616.1f + CR 707.9e: Dominion Saboteur copies an uncopied
/// Sculpting Steel, then applies the copy replacement it acquired from Steel to
/// copy Charged Relic. The later copy retires the Saboteur exception's
/// additional counters, and Steel's own replacement copies no counters.
#[test]
fn copying_an_uncopied_sculpting_steel_offers_its_copy_replacement_too() {
    let AcquiredCopyBoard {
        mut runner,
        saboteur,
        steel,
        relic,
        ..
    } = acquired_copy_board();
    let life_before = runner.state().players[0].life;

    cast_to_copy_offer(&mut runner, saboteur);
    accept_copy_of(&mut runner, steel);
    assert_acquired_copy_offer(&runner, saboteur);
    runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("apply the acquired copy replacement");
    choose_copy_source(&mut runner, relic);
    assert_settled(&mut runner);
    assert_eq!(
        runner.state().players[0].life,
        life_before + 2,
        "CR 614.12a + CR 603.6a: the entry is observed once, as the final copy"
    );

    let entered = &runner.state().objects[&saboteur];
    assert_eq!(entered.zone, Zone::Battlefield);
    assert_eq!(
        entered.name, "Charged Relic",
        "the winning copy is the Relic"
    );
    assert!(
        entered.counters.is_empty(),
        "CR 707.9e: the later copy retires Saboteur's copied counters, and \
         Steel's replacement copies none; got {:?}",
        entered.counters
    );
    assert!(
        entered.replacement_definitions.is_empty(),
        "the Relic copy carries no enter-as-copy replacement"
    );
}

/// CR 616.1e + CR 707.9e: declining the acquired replacement leaves the first
/// copy standing, so Saboteur's exception still adds Steel's counters.
#[test]
fn declining_the_acquired_copy_replacement_keeps_the_first_copy_and_its_counters() {
    let AcquiredCopyBoard {
        mut runner,
        saboteur,
        steel,
        ..
    } = acquired_copy_board();

    cast_to_copy_offer(&mut runner, saboteur);
    accept_copy_of(&mut runner, steel);
    assert_acquired_copy_offer(&runner, saboteur);
    runner
        .act(GameAction::ChooseReplacement { index: 1 })
        .expect("decline the acquired copy replacement");
    assert_settled(&mut runner);

    let entered = &runner.state().objects[&saboteur];
    assert_eq!(entered.zone, Zone::Battlefield);
    assert_eq!(entered.name, "Sculpting Steel");
    assert_eq!(
        entered.counters.get(&CounterType::Plus1Plus1).copied(),
        Some(2),
        "the surviving first copy keeps Saboteur's copied counters"
    );
    assert_eq!(entered.counters.len(), 1);
}

/// CR 614.5 + CR 616.1f: the replacement acquired from a given copy source
/// applies to this entry once. Copying that same Sculpting Steel again ends the
/// chain instead of offering its replacement a second time.
#[test]
fn copying_the_same_steel_again_ends_the_acquired_copy_chain() {
    let AcquiredCopyBoard {
        mut runner,
        saboteur,
        steel,
        ..
    } = acquired_copy_board();

    cast_to_copy_offer(&mut runner, saboteur);
    accept_copy_of(&mut runner, steel);
    assert_acquired_copy_offer(&runner, saboteur);
    runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("apply the acquired copy replacement");
    choose_copy_source(&mut runner, steel);
    assert_settled(&mut runner);

    let entered = &runner.state().objects[&saboteur];
    assert_eq!(entered.name, "Sculpting Steel");
    assert!(
        entered.counters.is_empty(),
        "CR 707.9e: the second copy retired Saboteur's copied counters; got {:?}",
        entered.counters
    );
}

/// CR 707.9e (Altered Ego / Clone example) for the Clone family itself: a cast
/// Sculpting Steel that copies an uncopied Sculpting Steel may then copy
/// another artifact with the replacement it acquired.
#[test]
fn a_sculpting_steel_copying_an_uncopied_steel_may_copy_again() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let resident = scenario
        .add_artifact_from_oracle(P1, "Sculpting Steel", SCULPTING_STEEL)
        .id();
    let relic = scenario
        .add_artifact_from_oracle(P1, "Charged Relic", "")
        .id();
    let entering = scenario
        .add_artifact_to_hand_from_oracle(P0, "Sculpting Steel", SCULPTING_STEEL)
        .id();
    let mut runner = scenario.build();

    cast_to_copy_offer(&mut runner, entering);
    accept_copy_of(&mut runner, resident);
    assert_acquired_copy_offer(&runner, entering);
    runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("apply the acquired copy replacement");
    choose_copy_source(&mut runner, relic);
    assert_settled(&mut runner);

    let entered = &runner.state().objects[&entering];
    assert_eq!(entered.zone, Zone::Battlefield);
    assert_eq!(entered.name, "Charged Relic");
}

/// CR 614.12 + CR 616.1f on the token path: a token copy of an uncopied
/// Dominion Saboteur copies an uncopied Sculpting Steel and then, through the
/// replacement acquired from it, Charged Relic — with no counters (CR 707.9e).
#[test]
fn a_token_copy_also_offers_the_acquired_copy_replacement() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let steel = scenario
        .add_artifact_from_oracle(P1, "Sculpting Steel", SCULPTING_STEEL)
        .id();
    scenario.with_counter(steel, CounterType::Plus1Plus1, 2);
    let relic = scenario
        .add_artifact_from_oracle(P1, "Charged Relic", "")
        .id();
    scenario.with_counter(relic, CounterType::Generic("charge".to_string()), 3);
    let saboteur = scenario
        .add_creature_from_oracle(P0, "Dominion Saboteur", 3, 3, DOMINION_SABOTEUR)
        .id();
    let counterpart = scenario
        .add_spell_to_hand_from_oracle(P0, "Cackling Counterpart", true, CACKLING_COUNTERPART)
        .id();
    let mut runner = scenario.build();
    let _ = runner.cast(counterpart).target_object(saboteur).commit();
    runner.advance_until_stack_empty();
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ),
        "the token copy offers Saboteur's replacement, got {:?}",
        runner.state().waiting_for
    );
    accept_copy_of(&mut runner, steel);
    let token = {
        let WaitingFor::ReplacementChoice { candidates, .. } = &runner.state().waiting_for else {
            panic!(
                "the token's acquired enter-as-copy replacement must be offered, got {:?}",
                runner.state().waiting_for
            );
        };
        candidates[0].source_id
    };
    assert_acquired_copy_offer(&runner, token);
    runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("apply the acquired copy replacement");
    choose_copy_source(&mut runner, relic);
    assert_settled(&mut runner);

    let state = runner.state();
    let tokens: Vec<_> = state
        .objects
        .values()
        .filter(|object| object.is_token && object.zone == Zone::Battlefield)
        .collect();
    assert_eq!(tokens.len(), 1, "exactly one token copy enters");
    assert_eq!(tokens[0].id, token);
    assert_eq!(tokens[0].name, "Charged Relic");
    assert!(
        tokens[0].counters.is_empty(),
        "CR 707.9e: no copied counters survive the later copy; got {:?}",
        tokens[0].counters
    );
}
