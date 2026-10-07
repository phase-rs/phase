//! State triggers (CR 603.8) through the real priority pipeline: what the
//! effect body's "it" names, and which part of the condition is rechecked when
//! the triggered ability resolves.
//!
//! Two class-level rules are pinned here:
//!
//! 1. **"it" in a source-counter state trigger is the source (CR 608.2k).**
//!    "When there are four or more page counters on this artifact, exile it"
//!    (Mazemind Tome) and "... incarnation counters on this enchantment, exile
//!    it" (Nine Lives) bind `SelfRef`, whose resolver applies the CR 400.7
//!    new-object guard. If the source leaves the battlefield while the trigger
//!    is on the stack, the trigger cannot follow it to its new zone, and the
//!    Tome's "If you do, you gain 4 life" (CR 118.12 / CR 608.2c) does nothing.
//!    Mazemind Tome ruling (2020-06-23): "If Mazemind Tome leaves the
//!    battlefield while its triggered ability is on the stack, you can't exile
//!    it from the zone it's put into."
//!
//! 2. **A state condition is not rechecked on resolution (CR 603.8 vs
//!    CR 603.4).** Only an intervening "if" that immediately follows a trigger
//!    condition is checked again as the ability resolves. Plague Boiler ruling
//!    (2005-10-01): "removing a counter in response won't stop the effect."
//!    Emperor Crocodile ruling (2016-06-08): "It does not check again on
//!    resolution, so gaining control of a creature before then will not save
//!    Emperor Crocodile."
//!
//! Every Oracle text below is verbatim from MTGJSON, except the two synthetic
//! cards (named so no word of the name appears in their text), which are
//! documented where they are defined.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{AbilityKind, TargetRef};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::counter::CounterType;
use engine::types::game_state::{StackEntryKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const MAZEMIND_TOME: &str = "{T}, Put a page counter on this artifact: Scry 1. (Look at the top card of your library. You may put that card on the bottom.)\n{2}, {T}, Put a page counter on this artifact: Draw a card.\nWhen there are four or more page counters on this artifact, exile it. If you do, you gain 4 life.";
const NINE_LIVES: &str = "Hexproof\nIf a source would deal damage to you, prevent that damage and put an incarnation counter on this enchantment.\nWhen there are nine or more incarnation counters on this enchantment, exile it.\nWhen this enchantment leaves the battlefield, you lose the game.";
const PLAGUE_BOILER: &str = "At the beginning of your upkeep, put a plague counter on this artifact.\n{1}{B}{G}: Put a plague counter on this artifact or remove a plague counter from it.\nWhen this artifact has three or more plague counters on it, sacrifice it. If you do, destroy all nonland permanents.";
const EMPEROR_CROCODILE: &str = "When you control no other creatures, sacrifice this creature.";
const VAMPIRE_HEXMAGE: &str =
    "First strike\nSacrifice this creature: Remove all counters from target permanent.";
const PLATINUM_ANGEL: &str =
    "Flying\nYou can't lose the game and your opponents can't win the game.";
const BOOMERANG: &str = "Return target permanent to its owner's hand.";
const SHOCK: &str = "Shock deals 2 damage to any target.";
const RAISE_THE_ALARM: &str = "Create two 1/1 white Soldier creature tokens.";
const WHITESUNS_PASSAGE: &str = "You gain 5 life.";

/// Synthetic instant carrying Flicker's exact Oracle text (Flicker itself is a
/// sorcery, so it cannot be cast while a trigger is on the stack).
const BLINK_REVERSAL_NAME: &str = "Blink Reversal";
const BLINK_REVERSAL: &str =
    "Exile target nontoken permanent, then return it to the battlefield under its owner's control.";

/// Synthetic artifact: a source-counter state trigger carrying a genuine
/// intervening "if" (CR 603.4). No printed card pairs these two clauses with a
/// testable effect; the printed intervening-"if" state triggers (Hidden
/// Predators, Veiled Crocodile, Lurking Jackals, Opal Avenger) all gate on
/// "if this permanent is an enchantment", which no response can falsify here.
const GLOAMWIRE_CAPACITOR_NAME: &str = "Gloamwire Capacitor";
const GLOAMWIRE_CAPACITOR: &str = "{T}: Put a charge counter on this artifact.\nWhen there are two or more charge counters on this artifact, if you have 10 or less life, sacrifice it.";

fn mana(color: ManaType, count: usize) -> Vec<ManaUnit> {
    (0..count)
        .map(|_| ManaUnit::new(color, ObjectId(0), false, vec![]))
        .collect()
}

fn add_mana(runner: &mut GameRunner, color: ManaType, count: usize) {
    for unit in mana(color, count) {
        let _ = runner.state_mut().add_mana_to_pool(P0, unit);
    }
}

fn counters(runner: &GameRunner, id: ObjectId, kind: &str) -> u32 {
    runner.state().objects[&id]
        .counters
        .get(&CounterType::Generic(kind.to_string()))
        .copied()
        .unwrap_or(0)
}

fn zone(runner: &GameRunner, id: ObjectId) -> Zone {
    runner.state().objects[&id].zone
}

fn hand_size(runner: &GameRunner) -> usize {
    runner
        .state()
        .players
        .iter()
        .find(|player| player.id == P0)
        .expect("P0 is in the game")
        .hand
        .len()
}

/// The `n`th activated ability (printed order) of `source`.
fn activated_index(runner: &GameRunner, source: ObjectId, n: usize) -> usize {
    runner.state().objects[&source]
        .abilities
        .iter()
        .enumerate()
        .filter(|(_, ability)| matches!(ability.kind, AbilityKind::Activated))
        .map(|(index, _)| index)
        .nth(n)
        .unwrap_or_else(|| panic!("source must expose activated ability #{n}"))
}

/// Whether a triggered ability of `source` is on the stack — the reach guard
/// every negative assertion below is paired with.
fn trigger_on_stack(runner: &GameRunner, source: ObjectId) -> bool {
    runner.state().stack.iter().any(|entry| {
        entry.source_id == source && matches!(entry.kind, StackEntryKind::TriggeredAbility { .. })
    })
}

fn top_is_trigger_of(runner: &GameRunner, source: ObjectId) -> bool {
    runner.state().stack.last().is_some_and(|entry| {
        entry.source_id == source && matches!(entry.kind, StackEntryKind::TriggeredAbility { .. })
    })
}

/// CR 117.4 + CR 608.1: both players pass priority until exactly the current
/// top stack object has resolved. Unlike `GameRunner::resolve_top`, this stops
/// even when the resolution immediately puts a new state trigger on the stack
/// (keeping the stack the same height), so a response can be cast before it.
/// Returns early at a resolution-time prompt, which the caller answers.
fn resolve_one(runner: &mut GameRunner) {
    let top = runner
        .state()
        .stack
        .last()
        .map(|entry| entry.id)
        .expect("there must be an object on the stack to resolve");
    for _ in 0..8 {
        if !runner.state().stack.iter().any(|entry| entry.id == top) {
            return;
        }
        // A resolution-time prompt (e.g. a choose-one branch) is answered by
        // the caller.
        if !matches!(runner.state().waiting_for, WaitingFor::Priority { .. }) {
            return;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("passing priority must be accepted");
    }
    panic!("the top stack object never resolved");
}

/// CR 602.2: activate an ability through `apply()`, choosing `target` for its
/// target slot and paying its mana from the pool, and stop at the activator's
/// priority window — the point where CR 603.8 state triggers are put on the
/// stack (CR 603.3).
fn activate_to_priority(
    runner: &mut GameRunner,
    source: ObjectId,
    ability_index: usize,
    target: Option<ObjectId>,
) {
    runner
        .act(GameAction::ActivateAbility {
            source_id: source,
            ability_index,
        })
        .expect("the activation must be accepted");
    for _ in 0..16 {
        match &runner.state().waiting_for {
            WaitingFor::Priority { .. } => return,
            WaitingFor::ManaPayment { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("the pool must pay the activation's mana cost");
            }
            WaitingFor::TargetSelection { .. } => {
                let target = target.expect("this activation needs a declared target");
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(TargetRef::Object(target)),
                    })
                    .expect("the declared target must be legal");
            }
            other => panic!("unexpected activation prompt {other:?}"),
        }
    }
    panic!("the activation never reached a priority window");
}

/// Mazemind Tome with three page counters, two colorless mana for the draw
/// ability, and a library to draw from.
fn tome_scenario() -> (GameScenario, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, mana(ManaType::Colorless, 2));
    scenario.with_library_top(P0, &["Island", "Island", "Island"]);
    let tome = scenario
        .add_artifact_from_oracle(P0, "Mazemind Tome", MAZEMIND_TOME)
        .id();
    scenario.with_counter(tome, CounterType::Generic("page".to_string()), 3);
    (scenario, tome)
}

/// Activate the Tome's draw ability (its cost adds the fourth page counter), so
/// the state trigger goes on the stack above the draw ability.
fn activate_tome_draw(runner: &mut GameRunner, tome: ObjectId) {
    let draw = activated_index(runner, tome, 1);
    activate_to_priority(runner, tome, draw, None);
    assert_eq!(
        counters(runner, tome, "page"),
        4,
        "the activation cost must add the fourth page counter"
    );
    assert_eq!(
        runner.state().stack.len(),
        2,
        "draw ability + state trigger"
    );
    assert!(
        top_is_trigger_of(runner, tome),
        "reach guard: the CR 603.8 state trigger must be on top of the draw ability"
    );
}

/// CR 603.8 + CR 602.2: the page counter paid as the draw ability's cost makes
/// the state trigger fire above the draw ability. It resolves first: the Tome is
/// exiled and its controller gains 4 life; then the draw ability resolves.
#[test]
fn mazemind_tome_draw_activation_exiles_tome_and_gains_life_before_drawing() {
    let (scenario, tome) = tome_scenario();
    let mut runner = scenario.build();
    let life_before = runner.life(P0);
    let hand_before = hand_size(&runner);

    activate_tome_draw(&mut runner, tome);

    resolve_one(&mut runner);
    assert_eq!(
        zone(&runner, tome),
        Zone::Exile,
        "the trigger exiles the Tome"
    );
    assert_eq!(
        runner.life(P0),
        life_before + 4,
        "If you do, you gain 4 life"
    );
    assert_eq!(
        hand_size(&runner),
        hand_before,
        "the draw ability has not resolved yet"
    );

    resolve_one(&mut runner);
    assert!(runner.state().stack.is_empty());
    assert_eq!(
        hand_size(&runner),
        hand_before + 1,
        "the draw ability still resolves after its source left"
    );
}

/// CR 400.7 + CR 608.2k: the Tome bounced in response is a new object in its
/// owner's hand. The trigger's "it" cannot reach it there, so nothing is exiled
/// and — no exile having happened — no life is gained (CR 608.2c).
#[test]
fn mazemind_tome_bounced_in_response_stays_in_hand_and_gains_no_life() {
    let (mut scenario, tome) = tome_scenario();
    let boomerang = scenario
        .add_spell_to_hand_from_oracle(P0, "Boomerang", true, BOOMERANG)
        .id();
    let mut runner = scenario.build();
    let life_before = runner.life(P0);

    activate_tome_draw(&mut runner, tome);
    add_mana(&mut runner, ManaType::Blue, 2);
    runner.cast(boomerang).target_object(tome).commit();

    resolve_one(&mut runner);
    assert_eq!(
        zone(&runner, tome),
        Zone::Hand,
        "Boomerang returns the Tome"
    );
    assert!(
        top_is_trigger_of(&runner, tome),
        "reach guard: the state trigger is still on the stack when the Tome leaves"
    );

    resolve_one(&mut runner);
    assert!(!trigger_on_stack(&runner, tome), "the trigger has resolved");
    assert_eq!(
        zone(&runner, tome),
        Zone::Hand,
        "the trigger must not exile the Tome from its owner's hand"
    );
    assert_eq!(
        runner.life(P0),
        life_before,
        "no exile happened, so \"If you do, you gain 4 life\" does nothing"
    );
}

/// CR 400.7 + CR 608.2k: a Tome exiled and returned in response is a new
/// permanent with no counters. The trigger's "it" named the old object, so the
/// new Tome stays on the battlefield and no life is gained.
#[test]
fn mazemind_tome_flickered_in_response_new_tome_stays_and_gains_no_life() {
    let (mut scenario, tome) = tome_scenario();
    let blink = scenario
        .add_spell_to_hand_from_oracle(P0, BLINK_REVERSAL_NAME, true, BLINK_REVERSAL)
        .id();
    let mut runner = scenario.build();
    let life_before = runner.life(P0);

    activate_tome_draw(&mut runner, tome);
    add_mana(&mut runner, ManaType::Colorless, 1);
    add_mana(&mut runner, ManaType::White, 1);
    runner.cast(blink).target_object(tome).commit();

    resolve_one(&mut runner);
    assert_eq!(
        zone(&runner, tome),
        Zone::Battlefield,
        "the Tome returns to the battlefield"
    );
    assert_eq!(
        counters(&runner, tome, "page"),
        0,
        "reach guard: the returned Tome is a new object without page counters"
    );
    assert!(
        top_is_trigger_of(&runner, tome),
        "reach guard: the old Tome's state trigger is still on the stack"
    );

    resolve_one(&mut runner);
    assert!(!trigger_on_stack(&runner, tome), "the trigger has resolved");
    assert_eq!(
        zone(&runner, tome),
        Zone::Battlefield,
        "the trigger must not exile the new Tome"
    );
    assert_eq!(runner.life(P0), life_before, "no exile, so no life gained");
}

/// CR 603.8: the state condition is not rechecked on resolution. Removing every
/// page counter in response (Vampire Hexmage) does not stop the trigger — the
/// Tome is still exiled and its controller still gains 4 life.
#[test]
fn mazemind_tome_counters_removed_in_response_still_exiles_and_gains_life() {
    let (mut scenario, tome) = tome_scenario();
    let hexmage = scenario
        .add_creature_from_oracle(P0, "Vampire Hexmage", 2, 1, VAMPIRE_HEXMAGE)
        .id();
    let mut runner = scenario.build();
    let life_before = runner.life(P0);

    activate_tome_draw(&mut runner, tome);
    let hexmage_ability = activated_index(&runner, hexmage, 0);
    activate_to_priority(&mut runner, hexmage, hexmage_ability, Some(tome));

    resolve_one(&mut runner);
    assert_eq!(
        counters(&runner, tome, "page"),
        0,
        "reach guard: Vampire Hexmage removed every page counter"
    );
    assert!(
        top_is_trigger_of(&runner, tome),
        "reach guard: the state trigger is still on the stack"
    );

    resolve_one(&mut runner);
    assert_eq!(
        zone(&runner, tome),
        Zone::Exile,
        "the state trigger is not rechecked on resolution, so the Tome is exiled"
    );
    assert_eq!(
        runner.life(P0),
        life_before + 4,
        "and its controller gains 4 life"
    );
}

/// CR 603.8: Plague Boiler ruling — "removing a counter in response won't stop
/// the effect." The third plague counter comes from the Boiler's own activated
/// ability; Vampire Hexmage strips every counter in response, and the trigger
/// still sacrifices the Boiler and destroys all nonland permanents.
#[test]
fn plague_boiler_counters_removed_in_response_still_sacrifices_and_destroys() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mut pool = mana(ManaType::Colorless, 1);
    pool.extend(mana(ManaType::Black, 1));
    pool.extend(mana(ManaType::Green, 1));
    scenario.with_mana_pool(P0, pool);
    let boiler = scenario
        .add_artifact_from_oracle(P0, "Plague Boiler", PLAGUE_BOILER)
        .id();
    scenario.with_counter(boiler, CounterType::Generic("plague".to_string()), 2);
    let hexmage = scenario
        .add_creature_from_oracle(P0, "Vampire Hexmage", 2, 1, VAMPIRE_HEXMAGE)
        .id();
    let bystander = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
    let mut runner = scenario.build();

    let boiler_ability = activated_index(&runner, boiler, 0);
    activate_to_priority(&mut runner, boiler, boiler_ability, None);
    resolve_one(&mut runner);
    match &runner.state().waiting_for {
        WaitingFor::ChooseOneOfBranch { .. } => {}
        other => panic!("expected Plague Boiler's put-or-remove choice, got {other:?}"),
    }
    runner
        .act(GameAction::ChooseBranch { index: 0 })
        .expect("choosing to put a plague counter must succeed");
    assert_eq!(counters(&runner, boiler, "plague"), 3);
    assert!(
        top_is_trigger_of(&runner, boiler),
        "reach guard: three plague counters put the state trigger on the stack"
    );

    let hexmage_ability = activated_index(&runner, hexmage, 0);
    activate_to_priority(&mut runner, hexmage, hexmage_ability, Some(boiler));
    resolve_one(&mut runner);
    assert_eq!(
        counters(&runner, boiler, "plague"),
        0,
        "reach guard: Vampire Hexmage removed every plague counter"
    );
    assert!(top_is_trigger_of(&runner, boiler));

    resolve_one(&mut runner);
    assert_eq!(
        zone(&runner, boiler),
        Zone::Graveyard,
        "the trigger still sacrifices Plague Boiler"
    );
    assert_eq!(
        zone(&runner, bystander),
        Zone::Graveyard,
        "If you do, destroy all nonland permanents"
    );
}

/// CR 603.8: Emperor Crocodile ruling — the ability "does not check again on
/// resolution, so gaining control of a creature before then will not save
/// Emperor Crocodile." Sacrificing Vampire Hexmage leaves the Crocodile alone,
/// and two Soldier tokens created in response do not stop the sacrifice.
#[test]
fn emperor_crocodile_creatures_created_in_response_do_not_save_it() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mut pool = mana(ManaType::Colorless, 1);
    pool.extend(mana(ManaType::White, 1));
    scenario.with_mana_pool(P0, pool);
    let crocodile = scenario
        .add_creature_from_oracle(P0, "Emperor Crocodile", 5, 5, EMPEROR_CROCODILE)
        .id();
    let hexmage = scenario
        .add_creature_from_oracle(P0, "Vampire Hexmage", 2, 1, VAMPIRE_HEXMAGE)
        .id();
    let alarm = scenario
        .add_spell_to_hand_from_oracle(P0, "Raise the Alarm", true, RAISE_THE_ALARM)
        .id();
    let mut runner = scenario.build();

    // Sacrificing Hexmage (its cost) leaves Emperor Crocodile as P0's only
    // creature; the state trigger goes on the stack above Hexmage's ability.
    let hexmage_ability = activated_index(&runner, hexmage, 0);
    activate_to_priority(&mut runner, hexmage, hexmage_ability, Some(crocodile));
    assert!(
        top_is_trigger_of(&runner, crocodile),
        "reach guard: controlling no other creatures put the state trigger on the stack"
    );

    runner.cast(alarm).commit();
    resolve_one(&mut runner);
    let other_creatures = runner
        .state()
        .battlefield
        .iter()
        .filter(|id| **id != crocodile)
        .map(|id| &runner.state().objects[id])
        .filter(|obj| {
            obj.controller == P0 && obj.card_types.core_types.contains(&CoreType::Creature)
        })
        .count();
    assert_eq!(
        other_creatures, 2,
        "reach guard: Raise the Alarm created two Soldiers before the trigger resolves"
    );
    assert!(top_is_trigger_of(&runner, crocodile));

    resolve_one(&mut runner);
    assert_eq!(
        zone(&runner, crocodile),
        Zone::Graveyard,
        "the state condition is not rechecked, so Emperor Crocodile is sacrificed"
    );
}

/// Gloamwire Capacitor at 10 life with one charge counter; its `{T}` ability
/// adds the second counter and its state trigger (with the intervening "if you
/// have 10 or less life") goes on the stack.
fn capacitor_with_trigger_on_stack(response_mana: bool) -> (GameRunner, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_life(P0, 10);
    if response_mana {
        let mut pool = mana(ManaType::Colorless, 1);
        pool.extend(mana(ManaType::White, 1));
        scenario.with_mana_pool(P0, pool);
    }
    let capacitor = scenario
        .add_artifact_from_oracle(P0, GLOAMWIRE_CAPACITOR_NAME, GLOAMWIRE_CAPACITOR)
        .id();
    scenario.with_counter(capacitor, CounterType::Generic("charge".to_string()), 1);
    let passage = scenario
        .add_spell_to_hand_from_oracle(P0, "Whitesun's Passage", true, WHITESUNS_PASSAGE)
        .id();
    let mut runner = scenario.build();

    let tap = activated_index(&runner, capacitor, 0);
    activate_to_priority(&mut runner, capacitor, tap, None);
    resolve_one(&mut runner);
    assert_eq!(counters(&runner, capacitor, "charge"), 2);
    assert!(
        top_is_trigger_of(&runner, capacitor),
        "reach guard: two charge counters at 10 life put the state trigger on the stack"
    );
    (runner, capacitor, passage)
}

/// CR 603.4: an intervening "if" beside a state condition IS rechecked on
/// resolution. Gaining life in response makes "if you have 10 or less life"
/// false, so the trigger does nothing and the artifact survives.
#[test]
fn state_trigger_intervening_if_false_on_resolution_does_nothing() {
    let (mut runner, capacitor, passage) = capacitor_with_trigger_on_stack(true);
    runner.cast(passage).commit();
    resolve_one(&mut runner);
    assert_eq!(
        runner.life(P0),
        15,
        "reach guard: Whitesun's Passage resolved"
    );
    assert!(top_is_trigger_of(&runner, capacitor));

    resolve_one(&mut runner);
    assert!(
        !trigger_on_stack(&runner, capacitor),
        "the trigger has left the stack"
    );
    assert_eq!(
        zone(&runner, capacitor),
        Zone::Battlefield,
        "the intervening \"if\" is false on resolution, so nothing is sacrificed"
    );
}

/// Positive twin: with the intervening "if" still true, the trigger sacrifices
/// the artifact.
#[test]
fn state_trigger_intervening_if_true_on_resolution_applies_effect() {
    let (mut runner, capacitor, _) = capacitor_with_trigger_on_stack(false);
    resolve_one(&mut runner);
    assert_eq!(
        zone(&runner, capacitor),
        Zone::Graveyard,
        "the intervening \"if\" holds on resolution, so the artifact is sacrificed"
    );
}

/// CR 400.7 + CR 608.2k: Nine Lives' ninth incarnation counter (from preventing
/// Shock's damage) puts its exile trigger on the stack; flickering Nine Lives in
/// response makes a new object, which the trigger's "it" cannot exile. Platinum
/// Angel keeps the leaves-the-battlefield "you lose the game" from ending the
/// game.
#[test]
fn nine_lives_flickered_in_response_new_nine_lives_stays_on_battlefield() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mut pool = mana(ManaType::Red, 1);
    pool.extend(mana(ManaType::Colorless, 1));
    pool.extend(mana(ManaType::White, 1));
    scenario.with_mana_pool(P0, pool);
    let nine_lives = scenario
        .add_enchantment_from_oracle(P0, "Nine Lives", NINE_LIVES)
        .id();
    scenario.with_counter(
        nine_lives,
        CounterType::Generic("incarnation".to_string()),
        8,
    );
    scenario.add_creature_from_oracle(P0, "Platinum Angel", 4, 4, PLATINUM_ANGEL);
    let shock = scenario
        .add_spell_to_hand_from_oracle(P0, "Shock", true, SHOCK)
        .id();
    let blink = scenario
        .add_spell_to_hand_from_oracle(P0, BLINK_REVERSAL_NAME, true, BLINK_REVERSAL)
        .id();
    let mut runner = scenario.build();
    let life_before = runner.life(P0);

    runner.cast(shock).target_player(P0).commit();
    resolve_one(&mut runner);
    assert_eq!(
        runner.life(P0),
        life_before,
        "Nine Lives prevents Shock's damage"
    );
    assert_eq!(counters(&runner, nine_lives, "incarnation"), 9);
    assert!(
        top_is_trigger_of(&runner, nine_lives),
        "reach guard: nine incarnation counters put the exile trigger on the stack"
    );

    runner.cast(blink).target_object(nine_lives).commit();
    resolve_one(&mut runner);
    assert_eq!(zone(&runner, nine_lives), Zone::Battlefield);
    assert_eq!(
        counters(&runner, nine_lives, "incarnation"),
        0,
        "reach guard: the returned Nine Lives is a new object"
    );
    assert!(
        trigger_on_stack(&runner, nine_lives),
        "reach guard: the old object's exile trigger is still on the stack"
    );

    runner.advance_until_stack_empty();
    assert!(
        !matches!(runner.state().waiting_for, WaitingFor::GameOver { .. }),
        "Platinum Angel keeps the leaves-the-battlefield trigger from ending the game"
    );
    assert_eq!(
        zone(&runner, nine_lives),
        Zone::Battlefield,
        "the exile trigger must not exile the new Nine Lives"
    );
}
