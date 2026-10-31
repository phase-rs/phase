//! CR 603.2h: "Do this only once each turn" limits the *action*, not the trigger.
//!
//! "A triggered ability may have an instruction followed by 'Do this only once
//! each turn.' This ability triggers only if its source's controller has not yet
//! taken the indicated action that turn." The controller takes the indicated
//! action by accepting the ability's "you may" instruction; declining takes no
//! action, so the ability keeps triggering. Once the action is taken, instances
//! already on the stack resolve with no effect (Calix / Riveteers Ascendancy
//! rulings), and a payment the controller cannot make is never offered
//! (CR 118.3 + CR 608.2d), nor is a move of an object that left its zone or a
//! reveal from an empty library (CR 608.2d). "This ability triggers only once each turn" is a
//! trigger limit (CR 603.2) and stays spent by triggering.
//!
//! Every fixture drives the real pipeline (cast → trigger → `apply`) and uses the
//! printed Oracle text (verified against MTGJSON).

use std::collections::HashSet;

use engine::game::game_object::BackFaceData;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{AbilityKind, TargetRef, TriggerDefinitionRef};
use engine::types::actions::{CastChoice, GameAction};
use engine::types::card_type::{CardType, CoreType};
use engine::types::counter::CounterType;
use engine::types::game_state::{
    AutoMayChoice, CastOfferKind, CastPaymentMode, ManaChoice, MayTriggerAutoChoiceScope,
    StackEntryKind, WaitingFor,
};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

use super::cast_this_way_gate_8721::{accept_offer_and_pay, decline_offer, offered_card};

const PANTLAZA: &str = "Whenever Pantlaza or another Dinosaur you control enters, you may discover X, where X is that creature's toughness. Do this only once each turn. (Exile cards from the top of your library until you exile a nonland card with that mana value or less. Cast it without paying its mana cost or put it into your hand. Put the rest on the bottom in a random order.)";

const ACADEMY_WALL: &str = "Defender\nWhenever you cast an instant or sorcery spell, you may draw a card. If you do, discard a card. This ability triggers only once each turn.";

const ANCIENT_CORNUCOPIA: &str = "Whenever you cast a spell that's one or more colors, you may gain 1 life for each of that spell's colors. Do this only once each turn.";

const GRAHA: &str = "Lifelink\nThrow Wide the Gates \u{2014} Whenever you cast a noncreature spell, you may pay X life, where X is that spell's mana value. If you do, create a 1/1 colorless Hero creature token and put X +1/+1 counters on it. Do this only once each turn.";

const ROAMING_THRONE: &str = "Ward {2}\nAs this creature enters, choose a creature type.\nThis creature is the chosen type in addition to its other types.\nIf a triggered ability of another creature you control of the chosen type triggers, it triggers an additional time.";

/// Turn Against (an instant), cast by P1 during P0's turn. Donate's targeted
/// `GiveControl` recipient has no player slot today (see
/// `gain_control_multi_target_6205.rs`), so the control change uses the working
/// `GainControl` path instead.
const TURN_AGAINST: &str = "Devoid (This card has no color.)\nGain control of target creature until end of turn. Untap that creature. It gains haste until end of turn.";

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn free() -> ManaCost {
    ManaCost::generic(0)
}

fn add_pantlaza(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_creature_from_oracle(P0, "Pantlaza, Sun-Favored", 4, 4, PANTLAZA)
        .with_subtypes(vec!["Dinosaur"])
        .id()
}

fn add_dinosaur_to_hand(scenario: &mut GameScenario, player: PlayerId, name: &str) -> ObjectId {
    scenario
        .add_creature_to_hand(player, name, 2, 2)
        .with_subtypes(vec!["Dinosaur"])
        .with_mana_cost(free())
        .id()
}

/// A mana value 0 nonland card on top of `player`'s library: a discover hit for
/// any X (decouples the tests from the X computation).
fn add_hit(scenario: &mut GameScenario, player: PlayerId, name: &str) -> ObjectId {
    scenario
        .add_spell_to_library_top(player, name, false)
        .with_mana_cost(free())
        .id()
}

fn cast(runner: &mut GameRunner, object_id: ObjectId) {
    let card_id = runner.state().objects[&object_id].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast from hand");
}

/// Pass priority until the cast `spell` has resolved, answering an as-enters
/// creature-type choice with `chosen`. Stops with any triggers it caused on the
/// stack (pattern: `giada_metallic_mimic.rs::cast_and_choose`).
fn resolve_spell(runner: &mut GameRunner, spell: ObjectId, chosen: &str) {
    for _ in 0..12 {
        let on_stack = runner
            .state()
            .stack
            .iter()
            .any(|entry| entry.id == spell || entry.source_id == spell);
        match runner.state().waiting_for.clone() {
            WaitingFor::NamedChoice { .. } => {
                runner
                    .act(GameAction::ChooseOption {
                        choice: chosen.to_string(),
                    })
                    .expect("answer the creature-type choice");
            }
            WaitingFor::Priority { .. } if on_stack => {
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            _ => return,
        }
    }
    panic!("the spell never resolved");
}

fn cast_and_resolve(runner: &mut GameRunner, spell: ObjectId) {
    cast(runner, spell);
    resolve_spell(runner, spell, "Dinosaur");
}

/// The definition refs of every stacked triggered ability from `source`.
fn stacked_trigger_refs(
    runner: &GameRunner,
    source: ObjectId,
) -> Vec<Option<TriggerDefinitionRef>> {
    runner
        .state()
        .stack
        .iter()
        .filter_map(|entry| match &entry.kind {
            StackEntryKind::TriggeredAbility {
                source_id, ability, ..
            } if *source_id == source => Some(ability.trigger_definition_ref.clone()),
            _ => None,
        })
        .collect()
}

fn assert_optional_prompt(runner: &GameRunner, player: PlayerId, why: &str) {
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::OptionalEffectChoice { player: p, .. } if p == player
        ),
        "{why}: expected an OptionalEffectChoice for {player:?}, got {:?}",
        runner.state().waiting_for
    );
}

fn assert_settled_priority(runner: &GameRunner, why: &str) {
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "{why}: expected Priority, got {:?}",
        runner.state().waiting_for
    );
    assert!(
        runner.state().stack.is_empty(),
        "{why}: the stack must be empty"
    );
}

fn decide(runner: &mut GameRunner, accept: bool) {
    runner
        .act(GameAction::DecideOptionalEffect { accept })
        .expect("answer the optional instruction");
}

/// Answer a discover `CastOffer` for `hit` by declining the cast, which puts the
/// hit into its owner's hand (pattern:
/// `cast_during_resolution_pipeline.rs::discover_decline_sends_hit_to_hand`).
fn decline_discover_cast(runner: &mut GameRunner, hit: ObjectId) {
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::CastOffer {
                kind: CastOfferKind::Discover { hit_card, .. },
                ..
            } if hit_card == hit
        ),
        "expected a discover CastOffer for {hit:?}, got {:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::DiscoverChoice {
            choice: CastChoice::Decline,
        })
        .expect("decline the discover cast");
    assert_eq!(runner.state().objects[&hit].zone, Zone::Hand);
}

fn indicated_actions(runner: &GameRunner) -> HashSet<(TriggerDefinitionRef, PlayerId)> {
    (*runner.state().trigger_indicated_actions_this_turn).clone()
}

/// Advance real turns until a later turn's precombat main, with P0 holding
/// priority (copied from `avalanche_of_sector_7_activation_trigger.rs`).
fn next_main_with_p0_priority(runner: &mut GameRunner) {
    let turn = runner.state().turn_number;
    for _ in 0..80 {
        if runner.state().turn_number != turn && runner.state().phase == Phase::PreCombatMain {
            while !matches!(runner.state().waiting_for, WaitingFor::Priority { player } if player == P0)
            {
                runner.act(GameAction::PassPriority).expect("pass to P0");
            }
            return;
        }
        let action = match runner.state().waiting_for {
            WaitingFor::Priority { .. } => GameAction::PassPriority,
            WaitingFor::DeclareAttackers { .. } => GameAction::DeclareAttackers {
                attacks: vec![],
                bands: vec![],
            },
            ref other => panic!("unexpected prompt advancing turns: {other:?}"),
        };
        runner.act(action).expect("advance the turn");
    }
    panic!("the next main phase was never reached");
}

fn pool(kinds: &[ManaType]) -> Vec<ManaUnit> {
    kinds
        .iter()
        .map(|&kind| ManaUnit::new(kind, ObjectId(0), false, vec![]))
        .collect()
}

/// Pantlaza on the battlefield, Roaming Throne in hand naming Dinosaur, two
/// discover hits (top first), and Dino A in hand. Throne's own entry is a
/// Dinosaur entry (CR 603.10) doubled by Throne (CR 603.2d); both Pantlaza
/// instances are declined, so the limit is still untaken when this returns with
/// Dino A's two Pantlaza instances on the stack.
struct ThroneBoard {
    runner: GameRunner,
    pantlaza: ObjectId,
    hit_1: ObjectId,
    hit_2: ObjectId,
}

fn throne_board_with_dino_a_doubled() -> ThroneBoard {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let pantlaza = add_pantlaza(&mut scenario);
    let throne = scenario
        .add_creature_to_hand_from_oracle(P0, "Roaming Throne", 4, 4, ROAMING_THRONE)
        .as_artifact_creature()
        .with_subtypes(vec!["Golem"])
        .with_mana_cost(free())
        .id();
    let dino_a = add_dinosaur_to_hand(&mut scenario, P0, "Dino A");
    let hit_2 = add_hit(&mut scenario, P0, "Hit Two");
    let hit_1 = add_hit(&mut scenario, P0, "Hit One");
    let mut runner = scenario.build();
    runner.state_mut().all_creature_types = vec!["Dinosaur".into(), "Golem".into(), "Human".into()];

    cast_and_resolve(&mut runner, throne);
    assert!(
        runner.state().objects[&throne]
            .card_types
            .subtypes
            .iter()
            .any(|subtype| subtype == "Dinosaur"),
        "reach: Roaming Throne's chosen type is Dinosaur"
    );
    assert_eq!(
        stacked_trigger_refs(&runner, pantlaza).len(),
        2,
        "reach: Throne's own Dinosaur entry triggers Pantlaza twice (CR 603.2d + CR 603.10)"
    );
    for _ in 0..2 {
        runner.advance_until_stack_empty();
        assert_optional_prompt(&runner, P0, "each Throne-entry instance offers its may");
        decide(&mut runner, false);
    }
    runner.advance_until_stack_empty();
    assert_settled_priority(&runner, "both Throne-entry instances declined");
    assert!(
        indicated_actions(&runner).is_empty(),
        "declining both instances takes no indicated action"
    );

    cast_and_resolve(&mut runner, dino_a);
    assert_eq!(
        stacked_trigger_refs(&runner, pantlaza).len(),
        2,
        "reach: Dino A's entry triggers Pantlaza twice under Roaming Throne"
    );
    ThroneBoard {
        runner,
        pantlaza,
        hit_1,
        hit_2,
    }
}

// ---------------------------------------------------------------------------
// T1 — declining does not spend the limit
// ---------------------------------------------------------------------------

/// CR 603.2h: declining the discover takes no action, so the next Dinosaur's
/// entry triggers Pantlaza again and can discover (acceptance criterion 1).
#[test]
fn pantlaza_declined_discover_leaves_the_next_dinosaur_free_to_discover() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let pantlaza = add_pantlaza(&mut scenario);
    let dino_a = add_dinosaur_to_hand(&mut scenario, P0, "Dino A");
    let dino_b = add_dinosaur_to_hand(&mut scenario, P0, "Dino B");
    let hit = add_hit(&mut scenario, P0, "Discovered Hit");
    let mut runner = scenario.build();

    cast_and_resolve(&mut runner, dino_a);
    assert_eq!(stacked_trigger_refs(&runner, pantlaza).len(), 1);
    runner.advance_until_stack_empty();
    assert_optional_prompt(&runner, P0, "reach: Dino A triggers Pantlaza");
    decide(&mut runner, false);
    runner.advance_until_stack_empty();
    assert_settled_priority(&runner, "after declining Dino A's discover");
    assert!(indicated_actions(&runner).is_empty());
    assert_eq!(runner.state().objects[&hit].zone, Zone::Library);

    cast_and_resolve(&mut runner, dino_b);
    assert_eq!(
        stacked_trigger_refs(&runner, pantlaza).len(),
        1,
        "the declined discover did not spend the limit, so Dino B triggers Pantlaza"
    );
    runner.advance_until_stack_empty();
    assert_optional_prompt(&runner, P0, "Dino B's Pantlaza trigger offers discover");
    decide(&mut runner, true);
    decline_discover_cast(&mut runner, hit);
    runner.advance_until_stack_empty();
    assert_settled_priority(&runner, "after Dino B's discover");
}

// ---------------------------------------------------------------------------
// T2 — taking the action spends the limit for the rest of the turn
// ---------------------------------------------------------------------------

/// CR 603.2h: once P0 discovers, Pantlaza stops triggering for the rest of the
/// turn (acceptance criterion 2), and "that turn" resets at the next turn.
#[test]
fn pantlaza_discover_stops_the_trigger_until_the_next_turn() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let pantlaza = add_pantlaza(&mut scenario);
    let dino_a = add_dinosaur_to_hand(&mut scenario, P0, "Dino A");
    let dino_b = add_dinosaur_to_hand(&mut scenario, P0, "Dino B");
    let dino_c = scenario
        .add_creature_to_hand(P0, "Dino C", 2, 2)
        .with_subtypes(vec!["Dinosaur"])
        .with_keyword(Keyword::Flash)
        .with_mana_cost(free())
        .id();
    add_hit(&mut scenario, P0, "Hit C");
    let hit_b = add_hit(&mut scenario, P0, "Hit B");
    let hit_a = add_hit(&mut scenario, P0, "Hit A");
    scenario.with_library_top(P1, &["P1 Card A", "P1 Card B", "P1 Card C"]);
    let mut runner = scenario.build();

    cast_and_resolve(&mut runner, dino_a);
    let refs = stacked_trigger_refs(&runner, pantlaza);
    let [Some(pantlaza_ref)] = refs.as_slice() else {
        panic!("one Pantlaza trigger with a definition ref, got {refs:?}");
    };
    let pantlaza_ref = pantlaza_ref.clone();
    runner.advance_until_stack_empty();
    assert_optional_prompt(&runner, P0, "reach: Dino A triggers Pantlaza");
    decide(&mut runner, true);
    decline_discover_cast(&mut runner, hit_a);
    runner.advance_until_stack_empty();
    assert_settled_priority(&runner, "after Dino A's discover");
    assert_eq!(
        indicated_actions(&runner),
        HashSet::from([(pantlaza_ref.clone(), P0)]),
        "accepting the discover takes the indicated action"
    );

    cast_and_resolve(&mut runner, dino_b);
    assert!(
        stacked_trigger_refs(&runner, pantlaza).is_empty(),
        "Pantlaza no longer triggers this turn"
    );
    assert_settled_priority(&runner, "Dino B's entry triggers nothing");
    assert_eq!(runner.state().objects[&hit_b].zone, Zone::Library);

    // CR 603.2h "that turn": the next turn (P1's) resets the limit; Dino C has flash.
    next_main_with_p0_priority(&mut runner);
    assert!(
        indicated_actions(&runner).is_empty(),
        "the indicated action resets at the next turn"
    );
    cast_and_resolve(&mut runner, dino_c);
    assert_eq!(stacked_trigger_refs(&runner, pantlaza).len(), 1);
    runner.advance_until_stack_empty();
    assert_optional_prompt(&runner, P0, "Pantlaza triggers again on a new turn");
}

// ---------------------------------------------------------------------------
// T3 — "This ability triggers only once each turn" is unchanged
// ---------------------------------------------------------------------------

/// CR 603.2: "This ability triggers only once each turn" is spent by triggering,
/// so declining the first draw still leaves the ability spent (acceptance
/// criterion 3).
#[test]
fn academy_wall_triggers_only_once_each_turn_even_when_declined() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "Academy Wall", 0, 5, ACADEMY_WALL);
    let first = scenario
        .add_spell_to_hand_from_oracle(P0, "Test Instant One", true, "You gain 1 life.")
        .with_mana_cost(free())
        .id();
    let second = scenario
        .add_spell_to_hand_from_oracle(P0, "Test Instant Two", true, "You gain 1 life.")
        .with_mana_cost(free())
        .id();
    scenario.with_library_top(P0, &["Card A", "Card B"]);
    let mut runner = scenario.build();

    cast(&mut runner, first);
    runner.advance_until_stack_empty();
    assert_optional_prompt(
        &runner,
        P0,
        "reach: the first instant triggers Academy Wall",
    );
    decide(&mut runner, false);
    runner.advance_until_stack_empty();
    assert_settled_priority(&runner, "after declining the first draw");
    assert_eq!(runner.state().triggers_fired_this_turn.len(), 1);

    cast(&mut runner, second);
    runner.advance_until_stack_empty();
    assert_settled_priority(
        &runner,
        "the declined trigger still spent \"triggers only once each turn\"",
    );
    assert_eq!(runner.state().triggers_fired_this_turn.len(), 1);
    assert!(indicated_actions(&runner).is_empty());
}

// ---------------------------------------------------------------------------
// T4 / T4a / T4b — instances already on the stack (Roaming Throne)
// ---------------------------------------------------------------------------

/// CR 603.2h + CR 603.2d: Roaming Throne doubles Pantlaza's trigger; both
/// instances are on the stack before either resolves. Once the first discovers,
/// the second "resolves but has no effect": no prompt and no second discover.
#[test]
fn roaming_throne_second_pantlaza_instance_does_nothing_after_the_first_discovers() {
    let ThroneBoard {
        mut runner,
        pantlaza,
        hit_1,
        hit_2,
    } = throne_board_with_dino_a_doubled();
    let hand_before = runner.state().players[P0.0 as usize].hand.len();

    runner.advance_until_stack_empty();
    assert_optional_prompt(&runner, P0, "reach: the first instance offers discover");
    decide(&mut runner, true);
    decline_discover_cast(&mut runner, hit_1);
    runner.advance_until_stack_empty();

    assert_settled_priority(&runner, "the second instance resolves with no prompt");
    assert!(stacked_trigger_refs(&runner, pantlaza).is_empty());
    assert_eq!(
        runner.state().objects[&hit_2].zone,
        Zone::Library,
        "the second instance must not discover again"
    );
    assert_eq!(
        runner.state().players[P0.0 as usize].hand.len(),
        hand_before + 1,
        "only the first discover's hit reaches the hand"
    );
}

/// CR 603.2h: a declined instance takes no action, so the next instance on the
/// stack still offers its "you may" and can discover.
#[test]
fn roaming_throne_declining_the_first_pantlaza_instance_leaves_the_second_free() {
    let ThroneBoard {
        mut runner,
        hit_1,
        hit_2,
        ..
    } = throne_board_with_dino_a_doubled();

    runner.advance_until_stack_empty();
    assert_optional_prompt(&runner, P0, "reach: the first instance offers discover");
    decide(&mut runner, false);
    runner.advance_until_stack_empty();
    assert_optional_prompt(&runner, P0, "the second instance still offers discover");
    decide(&mut runner, true);
    decline_discover_cast(&mut runner, hit_1);
    runner.advance_until_stack_empty();
    assert_settled_priority(&runner, "after the second instance discovers");
    assert_eq!(runner.state().objects[&hit_2].zone, Zone::Library);
}

/// CR 603.2h + CR 603.5: a remembered "always accept" answer for the first
/// instance cannot take the action again for the second — the spent limit is
/// checked before any stored answer is read.
#[test]
fn roaming_throne_remembered_accept_does_not_discover_twice() {
    let ThroneBoard {
        mut runner,
        hit_1,
        hit_2,
        ..
    } = throne_board_with_dino_a_doubled();

    runner.advance_until_stack_empty();
    assert_optional_prompt(&runner, P0, "reach: the first instance offers discover");
    runner
        .act(GameAction::DecideOptionalEffectAndRemember {
            choice: AutoMayChoice::Accept,
            scope: MayTriggerAutoChoiceScope::ExactInstance,
        })
        .expect("accept and remember");
    decline_discover_cast(&mut runner, hit_1);
    runner.advance_until_stack_empty();
    assert_settled_priority(&runner, "the second instance neither prompts nor discovers");
    assert_eq!(
        runner.state().objects[&hit_2].zone,
        Zone::Library,
        "the remembered accept must not discover a second time"
    );
}

// ---------------------------------------------------------------------------
// T6 — the limit is per ability (two sources)
// ---------------------------------------------------------------------------

/// CR 603.2h: the limit belongs to each ability — with two Ancient Cornucopias
/// each gains life once (Nykthos / Riveteers "each copy" rulings).
#[test]
fn two_ancient_cornucopias_each_take_their_own_action_once() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_artifact_from_oracle(P0, "Ancient Cornucopia", ANCIENT_CORNUCOPIA);
    scenario.add_artifact_from_oracle(P0, "Ancient Cornucopia", ANCIENT_CORNUCOPIA);
    let first = scenario
        .add_spell_to_hand_from_oracle(P0, "Red Instant One", true, "Draw a card.")
        .with_mana_cost(free())
        .with_color(vec![ManaColor::Red])
        .id();
    let second = scenario
        .add_spell_to_hand_from_oracle(P0, "Red Instant Two", true, "Draw a card.")
        .with_mana_cost(free())
        .with_color(vec![ManaColor::Red])
        .id();
    scenario.with_library_top(P0, &["Card A", "Card B", "Card C"]);
    let mut runner = scenario.build();
    let life_before = runner.life(P0);

    cast(&mut runner, first);
    for prompt in ["first Cornucopia", "second Cornucopia"] {
        runner.advance_until_stack_empty();
        assert_optional_prompt(&runner, P0, prompt);
        decide(&mut runner, true);
    }
    runner.advance_until_stack_empty();
    assert_settled_priority(&runner, "after both Cornucopias resolve");
    assert_eq!(
        runner.life(P0),
        life_before + 2,
        "each Cornucopia gained 1 life"
    );
    assert_eq!(indicated_actions(&runner).len(), 2);

    cast(&mut runner, second);
    runner.advance_until_stack_empty();
    assert_settled_priority(&runner, "both Cornucopias already took their action");
    assert_eq!(runner.life(P0), life_before + 2);
}

// ---------------------------------------------------------------------------
// T7 — the limit is keyed by the source's controller
// ---------------------------------------------------------------------------

/// CR 603.2h "its source's controller has not yet taken the indicated action":
/// after P0 discovers and P1 then gains control of Pantlaza, P1 has taken no
/// action, so Pantlaza triggers for P1.
#[test]
fn pantlaza_under_a_new_controller_triggers_for_that_controller() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let pantlaza = add_pantlaza(&mut scenario);
    let dino_a = add_dinosaur_to_hand(&mut scenario, P0, "Dino A");
    let turn_against = scenario
        .add_spell_to_hand_from_oracle(P1, "Turn Against", true, TURN_AGAINST)
        .with_mana_cost(free())
        .from_oracle_text_with_keywords(&["Devoid"], TURN_AGAINST)
        .id();
    let p1_dino = scenario
        .add_creature_to_hand(P1, "P1 Dino", 2, 2)
        .with_subtypes(vec!["Dinosaur"])
        .with_keyword(Keyword::Flash)
        .with_mana_cost(free())
        .id();
    let hit = add_hit(&mut scenario, P0, "Hit");
    add_hit(&mut scenario, P1, "P1 Hit");
    let mut runner = scenario.build();

    cast_and_resolve(&mut runner, dino_a);
    let refs = stacked_trigger_refs(&runner, pantlaza);
    let [Some(pantlaza_ref)] = refs.as_slice() else {
        panic!("one Pantlaza trigger with a definition ref, got {refs:?}");
    };
    let pantlaza_ref = pantlaza_ref.clone();
    runner.advance_until_stack_empty();
    assert_optional_prompt(&runner, P0, "reach: P0's Pantlaza offers discover");
    decide(&mut runner, true);
    decline_discover_cast(&mut runner, hit);
    runner.advance_until_stack_empty();
    assert_eq!(
        indicated_actions(&runner),
        HashSet::from([(pantlaza_ref.clone(), P0)])
    );

    // P1 takes Pantlaza with an instant during P0's turn ("that turn" is unchanged).
    runner.act(GameAction::PassPriority).expect("P0 passes");
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { player } if player == P1),
        "P1 holds priority: {:?}",
        runner.state().waiting_for
    );
    runner.cast(turn_against).target_object(pantlaza).resolve();
    assert_eq!(
        runner.state().objects[&pantlaza].controller,
        P1,
        "reach: Turn Against gives Pantlaza to P1"
    );
    assert_settled_priority(&runner, "after Turn Against resolves");

    runner.act(GameAction::PassPriority).expect("P0 passes");
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { player } if player == P1),
        "P1 holds priority: {:?}",
        runner.state().waiting_for
    );
    cast(&mut runner, p1_dino);
    resolve_spell(&mut runner, p1_dino, "Dinosaur");
    assert_eq!(
        stacked_trigger_refs(&runner, pantlaza),
        vec![Some(pantlaza_ref.clone())],
        "P1's Pantlaza trigger is the same ability (no zone change, CR 400.7)"
    );
    runner.advance_until_stack_empty();
    assert_optional_prompt(&runner, P1, "P1 has not taken the action this turn");
    decide(&mut runner, false);
    runner.advance_until_stack_empty();
    assert_eq!(
        indicated_actions(&runner),
        HashSet::from([(pantlaza_ref, P0)]),
        "P1's decline takes no action"
    );
}

// ---------------------------------------------------------------------------
// T8 — an unaffordable optional life payment is not offered and spends nothing
// ---------------------------------------------------------------------------

fn graha_refs(runner: &GameRunner, graha: ObjectId) -> Vec<Option<TriggerDefinitionRef>> {
    stacked_trigger_refs(runner, graha)
}

fn heroes(runner: &GameRunner) -> Vec<ObjectId> {
    let state = runner.state();
    state
        .battlefield
        .iter()
        .copied()
        .filter(|id| state.objects[id].name == "Hero")
        .collect()
}

/// CR 118.3 + CR 119.4 + CR 608.2d: at 2 life, paying 3 life is impossible, so
/// G'raha's "you may pay X life" is not offered and takes no action; the next
/// noncreature spell (X = 1) still triggers and paying spends the limit.
#[test]
fn graha_unaffordable_life_payment_is_not_offered_and_does_not_spend_the_limit() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let graha = scenario
        .add_creature_from_oracle(P0, "G'raha Tia, Scion Reborn", 2, 3, GRAHA)
        .id();
    scenario.with_life(P0, 2);
    scenario.with_mana_pool(P0, pool(&[ManaType::Blue; 4]));
    let mv_three = scenario
        .add_spell_to_hand_from_oracle(P0, "Test Draw", false, "Draw a card.")
        .with_mana_cost(ManaCost::generic(3))
        .id();
    let mv_one = scenario
        .add_spell_to_hand_from_oracle(P0, "Test Draw One", false, "Draw a card.")
        .with_mana_cost(ManaCost::generic(1))
        .id();
    let mv_zero = scenario
        .add_spell_to_hand_from_oracle(P0, "Test Draw Zero", false, "Draw a card.")
        .with_mana_cost(free())
        .id();
    scenario.with_library_top(P0, &["Card A", "Card B", "Card C", "Card D"]);
    let mut runner = scenario.build();

    cast(&mut runner, mv_three);
    assert_eq!(
        graha_refs(&runner, graha).len(),
        1,
        "reach: the mana value 3 spell triggers G'raha"
    );
    runner.advance_until_stack_empty();
    assert_settled_priority(&runner, "paying 3 life at 2 life is never offered");
    assert!(heroes(&runner).is_empty(), "no payment, so no Hero");
    assert_eq!(runner.life(P0), 2);
    assert!(indicated_actions(&runner).is_empty());

    cast(&mut runner, mv_one);
    let refs = graha_refs(&runner, graha);
    let [Some(graha_ref)] = refs.as_slice() else {
        panic!("one G'raha trigger with a definition ref, got {refs:?}");
    };
    let graha_ref = graha_ref.clone();
    runner.advance_until_stack_empty();
    assert_optional_prompt(&runner, P0, "paying 1 life at 2 life is offered");
    decide(&mut runner, true);
    runner.advance_until_stack_empty();
    assert_settled_priority(&runner, "after paying 1 life");
    let heroes = heroes(&runner);
    assert_eq!(heroes.len(), 1, "the paid trigger creates a Hero");
    assert_eq!(
        runner.state().objects[&heroes[0]]
            .counters
            .get(&CounterType::Plus1Plus1)
            .copied()
            .unwrap_or(0),
        1
    );
    assert_eq!(runner.life(P0), 1);
    assert_eq!(indicated_actions(&runner), HashSet::from([(graha_ref, P0)]));

    cast(&mut runner, mv_zero);
    assert!(
        graha_refs(&runner, graha).is_empty(),
        "G'raha took its action this turn, so it no longer triggers"
    );
}

// ---------------------------------------------------------------------------
// T9 / T10 — the halves of one split printed ability share one limit
// ---------------------------------------------------------------------------

const CORRUPTION_OF_TOWASHI: &str = "When this enchantment enters, incubate 4. (Create an Incubator token with four +1/+1 counters on it and \"{2}: Transform this token.\" It transforms into a 0/0 Phyrexian artifact creature.)\nWhenever a permanent you control transforms or a permanent you control enters transformed, you may draw a card. Do this only once each turn.";

const MACH_1: &str = "Flying\nWhen MACH-1 enters and whenever you gain life, surveil 1. This ability triggers only once each turn. (To surveil 1, look at the top card of your library. You may put it into your graveyard.)";

/// Thraben Gargoyle's printed front face: a production self-transform activation
/// (event "a permanent you control transforms").
const THRABEN_GARGOYLE: &str = "Defender\n{6}: Transform this creature.";

/// The activation paragraph of Jill, Shiva's Dominant: a production return of a
/// double-faced card to the battlefield transformed (CR 712.14a — event "a permanent
/// you control enters transformed"). Her ETB paragraph is not under test.
const JILL_RETURN_TRANSFORMED: &str = "{3}{U}{U}, {T}: Exile Jill, then return it to the battlefield transformed under its owner's control. Activate only as a sorcery.";

fn creature_back_face(name: &str, power: i32, toughness: i32) -> BackFaceData {
    BackFaceData {
        name: name.to_string(),
        power: Some(power),
        toughness: Some(toughness),
        card_types: CardType {
            supertypes: vec![],
            core_types: vec![CoreType::Creature],
            subtypes: vec![],
        },
        ..Default::default()
    }
}

/// Corruption of Towashi (no ETB) beside two P0 double-faced creatures: Thraben
/// Gargoyle transforms (Corruption's "transforms" half) and Jill returns
/// transformed (its "enters transformed" half).
struct CorruptionBoard {
    runner: GameRunner,
    corruption: ObjectId,
    gargoyle: ObjectId,
    jill: ObjectId,
    /// The "transforms" half's identity, which keys the shared limit.
    transforms_half: TriggerDefinitionRef,
    /// The "enters transformed" half's identity.
    enters_transformed_half: TriggerDefinitionRef,
}

fn corruption_board() -> CorruptionBoard {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let corruption = scenario
        .add_enchantment_from_oracle(P0, "Corruption of Towashi", CORRUPTION_OF_TOWASHI)
        .id();
    let gargoyle = scenario
        .add_creature_from_oracle(P0, "Thraben Gargoyle", 2, 2, THRABEN_GARGOYLE)
        .id();
    let jill = scenario
        .add_creature_from_oracle(P0, "Jill, Shiva's Dominant", 2, 2, JILL_RETURN_TRANSFORMED)
        .id();
    scenario.with_library_top(P0, &["Card A", "Card B", "Card C"]);
    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&gargoyle)
        .unwrap()
        .back_face = Some(creature_back_face("Stonewing Antagonizer", 4, 2));
    runner.state_mut().objects.get_mut(&jill).unwrap().back_face =
        Some(creature_back_face("Returned Back Face", 4, 5));

    // CR 113.2c: the split halves stay distinct abilities, and both key their limit
    // ledger on the first half.
    let object = &runner.state().objects[&corruption];
    let entries = object.trigger_definitions.as_slice();
    assert_eq!(
        entries.len(),
        3,
        "reach: Corruption's ETB plus two split halves"
    );
    let first = object.trigger_definition_ref(&entries[1]);
    let second = object.trigger_definition_ref(&entries[2]);
    assert_ne!(first, second, "the halves keep their own identities");
    assert_eq!(*first.limit_ledger_ref(&entries[1].definition), first);
    assert_eq!(*second.limit_ledger_ref(&entries[2].definition), first);
    let (first, second) = (first.clone(), second.clone());

    CorruptionBoard {
        runner,
        corruption,
        gargoyle,
        jill,
        transforms_half: first,
        enters_transformed_half: second,
    }
}

fn activate_only_ability(runner: &mut GameRunner, source: ObjectId, mana: &[ManaType]) {
    for &kind in mana {
        runner
            .state_mut()
            .add_mana_to_pool(P0, ManaUnit::new(kind, ObjectId(0), false, vec![]));
    }
    let ability_index = runner.state().objects[&source]
        .abilities
        .iter()
        .position(|ability| ability.kind == AbilityKind::Activated)
        .expect("the source has its printed activated ability");
    runner
        .act(GameAction::ActivateAbility {
            source_id: source,
            ability_index,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("activate the printed ability");
}

/// Event A — "a permanent you control transforms": Thraben Gargoyle's activation.
fn transform_gargoyle(runner: &mut GameRunner, gargoyle: ObjectId) {
    activate_only_ability(runner, gargoyle, &[ManaType::Colorless; 6]);
}

/// Event B — "a permanent you control enters transformed": Jill's activation.
fn return_jill_transformed(runner: &mut GameRunner, jill: ObjectId) {
    let mut mana = vec![ManaType::Colorless; 3];
    mana.extend([ManaType::Blue; 2]);
    activate_only_ability(runner, jill, &mana);
}

/// Resolve the stack, answering every "you may" prompt with `accept` and every
/// surveil with "keep on top". Returns the definition refs of the `source`
/// triggers that reached the stack.
fn resolve_all(
    runner: &mut GameRunner,
    source: ObjectId,
    accept: bool,
) -> Vec<Option<TriggerDefinitionRef>> {
    let mut seen = HashSet::new();
    let mut refs = Vec::new();
    for _ in 0..60 {
        for entry in &runner.state().stack {
            if let StackEntryKind::TriggeredAbility {
                source_id, ability, ..
            } = &entry.kind
            {
                if *source_id == source && seen.insert(entry.id) {
                    refs.push(ability.trigger_definition_ref.clone());
                }
            }
        }
        let action = match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => return refs,
            WaitingFor::Priority { .. } => GameAction::PassPriority,
            WaitingFor::OptionalEffectChoice { player, .. } => {
                assert_eq!(player, P0, "the controller answers the instruction");
                GameAction::DecideOptionalEffect { accept }
            }
            WaitingFor::SurveilChoice { .. } => GameAction::SelectCards { cards: vec![] },
            other => panic!("unexpected prompt while resolving: {other:?}"),
        };
        runner.act(action).expect("resolve the stack");
    }
    panic!("the stack never settled");
}

fn hand_size(runner: &GameRunner) -> usize {
    runner.state().players[P0.0 as usize].hand.len()
}

/// CR 113.2c + CR 603.2h: drawing off the "transforms" half takes the indicated
/// action of the one printed ability, so a later "enters transformed" the same turn
/// does not trigger it.
#[test]
fn corruption_of_towashi_draw_on_a_transform_spends_its_enters_transformed_half() {
    let CorruptionBoard {
        mut runner,
        corruption,
        gargoyle,
        jill,
        transforms_half,
        ..
    } = corruption_board();
    let hand_before = hand_size(&runner);

    transform_gargoyle(&mut runner, gargoyle);
    let fired = resolve_all(&mut runner, corruption, true);
    assert_eq!(
        fired,
        vec![Some(transforms_half)],
        "reach: the transform triggers the transforms half"
    );
    assert!(runner.state().objects[&gargoyle].transformed);
    assert_eq!(hand_size(&runner), hand_before + 1, "the draw was taken");

    return_jill_transformed(&mut runner, jill);
    let fired = resolve_all(&mut runner, corruption, true);
    assert!(
        runner.state().objects[&jill].transformed
            && runner.state().objects[&jill].zone == Zone::Battlefield,
        "reach: Jill entered transformed"
    );
    assert!(
        fired.is_empty(),
        "the enters-transformed half shares the spent limit: {fired:?}"
    );
    assert_eq!(hand_size(&runner), hand_before + 1);
}

/// CR 603.2h: declining the draw on a transform takes no action, so the
/// enters-transformed half still triggers, and accepting it draws once.
#[test]
fn corruption_of_towashi_declined_draw_leaves_its_other_half_free() {
    let CorruptionBoard {
        mut runner,
        corruption,
        gargoyle,
        jill,
        transforms_half,
        enters_transformed_half,
    } = corruption_board();
    let hand_before = hand_size(&runner);

    transform_gargoyle(&mut runner, gargoyle);
    let fired = resolve_all(&mut runner, corruption, false);
    assert_eq!(
        fired,
        vec![Some(transforms_half)],
        "reach: the transform triggers the transforms half"
    );
    assert_eq!(hand_size(&runner), hand_before, "the draw was declined");

    return_jill_transformed(&mut runner, jill);
    let fired = resolve_all(&mut runner, corruption, true);
    assert!(
        fired.contains(&Some(enters_transformed_half)),
        "the untaken limit leaves the enters-transformed half free: {fired:?}"
    );
    assert_eq!(
        hand_size(&runner),
        hand_before + 1,
        "the ability draws once"
    );
}

/// CR 113.2c + CR 603.2h: the ability draws once across a transformed entry and a
/// later transform.
#[test]
fn corruption_of_towashi_draw_on_an_enters_transformed_spends_its_transform_half() {
    let CorruptionBoard {
        mut runner,
        corruption,
        gargoyle,
        jill,
        enters_transformed_half,
        ..
    } = corruption_board();
    let hand_before = hand_size(&runner);

    return_jill_transformed(&mut runner, jill);
    let fired = resolve_all(&mut runner, corruption, true);
    assert!(
        fired.contains(&Some(enters_transformed_half)),
        "reach: the transformed entry triggers the enters-transformed half: {fired:?}"
    );
    assert_eq!(
        hand_size(&runner),
        hand_before + 1,
        "the ability draws once for the entry"
    );

    transform_gargoyle(&mut runner, gargoyle);
    let fired = resolve_all(&mut runner, corruption, true);
    assert!(
        runner.state().objects[&gargoyle].transformed,
        "reach: the Gargoyle transformed"
    );
    assert!(
        fired.is_empty(),
        "the transforms half shares the spent limit: {fired:?}"
    );
    assert_eq!(hand_size(&runner), hand_before + 1);
}

fn gain_one_life(scenario: &mut GameScenario, name: &str) -> ObjectId {
    scenario
        .add_spell_to_hand_from_oracle(P0, name, true, "You gain 1 life.")
        .with_mana_cost(free())
        .id()
}

/// CR 603.2: MACH-1's life-gain half triggers once each turn (reach guard for the
/// shared-limit test below).
#[test]
fn mach_1_life_gain_half_triggers_only_once_each_turn() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mach = scenario
        .add_creature_from_oracle(P0, "MACH-1, Swooping Scoundrel", 1, 3, MACH_1)
        .id();
    let first = gain_one_life(&mut scenario, "Test Gain One");
    let second = gain_one_life(&mut scenario, "Test Gain Two");
    scenario.with_library_top(P0, &["Card A", "Card B"]);
    let mut runner = scenario.build();
    let life_before = runner.life(P0);

    cast(&mut runner, first);
    let fired = resolve_all(&mut runner, mach, true);
    assert_eq!(
        runner.life(P0),
        life_before + 1,
        "reach: the first spell gained life"
    );
    assert_eq!(fired.len(), 1, "gaining life triggers MACH-1");

    cast(&mut runner, second);
    let fired = resolve_all(&mut runner, mach, true);
    assert_eq!(
        runner.life(P0),
        life_before + 2,
        "reach: the second spell gained life, so the trigger event happened"
    );
    assert!(fired.is_empty(), "{fired:?}");
}

// ---------------------------------------------------------------------------
// An impossible optional discard is not offered and spends no limit
// ---------------------------------------------------------------------------

const IRREVERENT_GREMLIN: &str = "Menace (This creature can't be blocked except by two or more creatures.)\nWhenever another creature you control with power 2 or less enters, you may discard a card. If you do, draw a card. Do this only once each turn.";

/// A free token maker: each activation is "another creature you control with
/// power 2 or less enters" for Irreverent Gremlin.
const SOLDIER_MAKER: &str = "{0}: Create a 1/1 white Soldier creature token.";

/// A free draw, to give P0 a card to discard.
const FREE_DRAW: &str = "{0}: Draw a card.";

/// Activate `source`'s only ability for free and pass priority until it has
/// resolved, stopping with any triggers it caused on the stack.
fn activate_and_resolve(runner: &mut GameRunner, source: ObjectId) {
    activate_only_ability(runner, source, &[]);
    for _ in 0..12 {
        let on_stack = runner
            .state()
            .stack
            .iter()
            .any(|entry| entry.source_id == source);
        match runner.state().waiting_for {
            WaitingFor::Priority { .. } if on_stack => {
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            _ => return,
        }
    }
    panic!("the activated ability never resolved");
}

fn graveyard_size(runner: &GameRunner) -> usize {
    runner.state().players[P0.0 as usize].graveyard.len()
}

/// CR 118.12 + CR 118.3 + CR 608.2d + CR 603.2h: with an empty hand, Irreverent
/// Gremlin's "you may discard a card" can't be paid, so it is not offered and the
/// controller takes no indicated action; the next entry the same turn triggers it
/// again, and once a card is discarded the limit is spent.
#[test]
fn irreverent_gremlin_empty_hand_discard_is_not_offered_and_does_not_spend_the_limit() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let gremlin = scenario
        .add_creature_from_oracle(P0, "Irreverent Gremlin", 2, 2, IRREVERENT_GREMLIN)
        .id();
    let soldier_maker = scenario
        .add_artifact_from_oracle(P0, "Soldier Maker", SOLDIER_MAKER)
        .id();
    let card_drawer = scenario
        .add_artifact_from_oracle(P0, "Card Drawer", FREE_DRAW)
        .id();
    scenario.with_library_top(P0, &["Card A", "Card B", "Card C"]);
    let mut runner = scenario.build();
    assert_eq!(hand_size(&runner), 0);
    let graveyard_before = graveyard_size(&runner);

    // Leg 1: empty hand — the trigger resolves without a prompt.
    activate_and_resolve(&mut runner, soldier_maker);
    let refs = stacked_trigger_refs(&runner, gremlin);
    let [Some(gremlin_ref)] = refs.as_slice() else {
        panic!("reach: the Soldier's entry triggers Gremlin once, got {refs:?}");
    };
    let gremlin_ref = gremlin_ref.clone();
    runner.advance_until_stack_empty();
    assert_settled_priority(&runner, "an empty-hand discard is never offered");
    assert!(
        indicated_actions(&runner).is_empty(),
        "an impossible discard takes no indicated action"
    );
    assert_eq!(hand_size(&runner), 0, "no card drawn");
    assert_eq!(
        graveyard_size(&runner),
        graveyard_before,
        "no card discarded"
    );

    // Leg 2: one card in hand — the next entry triggers again and is offered.
    activate_and_resolve(&mut runner, card_drawer);
    assert_eq!(hand_size(&runner), 1, "reach: P0 drew a card to discard");
    activate_and_resolve(&mut runner, soldier_maker);
    assert_eq!(
        stacked_trigger_refs(&runner, gremlin).len(),
        1,
        "the untaken limit leaves Gremlin free to trigger"
    );
    runner.advance_until_stack_empty();
    assert_optional_prompt(&runner, P0, "a payable discard is offered");
    decide(&mut runner, true);
    runner.advance_until_stack_empty();
    assert_settled_priority(&runner, "after discarding and drawing");
    assert_eq!(
        indicated_actions(&runner),
        HashSet::from([(gremlin_ref, P0)]),
        "discarding takes the indicated action"
    );
    assert_eq!(hand_size(&runner), 1, "discarded one card, drew one card");
    assert_eq!(graveyard_size(&runner), graveyard_before + 1);

    // Leg 3: the limit is spent — the next entry does not trigger Gremlin.
    let battlefield_before = runner.state().battlefield.len();
    activate_and_resolve(&mut runner, soldier_maker);
    assert_eq!(
        runner.state().battlefield.len(),
        battlefield_before + 1,
        "reach: another Soldier entered"
    );
    assert!(
        stacked_trigger_refs(&runner, gremlin).is_empty(),
        "Gremlin's controller took its action this turn, so it no longer triggers"
    );
}

/// CR 113.2c + CR 603.2: "When MACH-1 enters and whenever you gain life, surveil 1.
/// This ability triggers only once each turn." is one ability, so its entering
/// spends the limit for the life-gain half the same turn.
#[test]
fn mach_1_entering_spends_its_life_gain_half() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mach = scenario
        .add_creature_to_hand_from_oracle(P0, "MACH-1, Swooping Scoundrel", 1, 3, MACH_1)
        .with_mana_cost(free())
        .id();
    let gain = gain_one_life(&mut scenario, "Test Gain");
    scenario.with_library_top(P0, &["Card A", "Card B"]);
    let mut runner = scenario.build();
    let life_before = runner.life(P0);

    cast(&mut runner, mach);
    let fired = resolve_all(&mut runner, mach, true);
    assert_eq!(fired.len(), 1, "reach: MACH-1's entering triggers it");
    assert_eq!(runner.state().objects[&mach].zone, Zone::Battlefield);

    cast(&mut runner, gain);
    let fired = resolve_all(&mut runner, mach, true);
    assert_eq!(runner.life(P0), life_before + 1, "reach: P0 gained life");
    assert!(
        fired.is_empty(),
        "the life-gain half shares the spent limit: {fired:?}"
    );
}

// ---------------------------------------------------------------------------
// CR 603.2h + CR 601.2i + CR 733.1: a "you may cast …" indicated action is taken
// when the spell becomes cast. Offering the cast is not casting: a declined offer,
// a rejected election or a cancelled cast is reversed and spends nothing.
// ---------------------------------------------------------------------------

const EMET_SELCH: &str = "Spells you cast from your graveyard cost {2} less to cast.\nWhenever one or more opponents lose life, you may cast target instant or sorcery card from your graveyard. If that spell would be put into your graveyard, exile it instead. Do this only once each turn.";

const LIFE_DRAIN_ENGINE: &str = "{0}: Each opponent loses 1 life.";

const PLANETARIUM_ORACLE: &str = "{1}, {T}: Scry 2.\nWhenever you scry or surveil, look at the top card of your library. You may cast that card without paying its mana cost. Do this only once each turn. (Look at the card after you scry or surveil.)";

const SHOCK: &str = "Shock deals 2 damage to any target.";

/// Emet-Selch of the Third Seat (both Oracle lines) with a free "each opponent
/// loses 1 life" activation, one `{R}` "Draw a card." instant in P0's graveyard per
/// name, Mountains to pay for each cast and a seeded library. CR 118.7a: the {2}
/// generic reduction leaves {R} to pay.
struct EmetBoard {
    runner: GameRunner,
    emet: ObjectId,
    drain: ObjectId,
    instants: Vec<ObjectId>,
}

fn emet_board(instant_names: &[&str]) -> EmetBoard {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let emet = scenario
        .add_creature_from_oracle(P0, "Emet-Selch of the Third Seat", 2, 2, EMET_SELCH)
        .id();
    let drain = scenario
        .add_artifact_from_oracle(P0, "Life Drain Engine", LIFE_DRAIN_ENGINE)
        .id();
    let instants = instant_names
        .iter()
        .map(|name| {
            scenario
                .add_spell_to_graveyard(P0, name, true)
                .from_oracle_text("Draw a card.")
                .with_mana_cost(ManaCost::Cost {
                    shards: vec![ManaCostShard::Red],
                    generic: 0,
                })
                .id()
        })
        .collect();
    for _ in 0..3 {
        scenario.add_basic_land(P0, ManaColor::Red);
    }
    scenario.with_library_top(P0, &["Card A", "Card B", "Card C", "Card D"]);
    EmetBoard {
        runner: scenario.build(),
        emet,
        drain,
        instants,
    }
}

/// Activate the drain and resolve it; the opponent's life loss triggers Emet-Selch.
/// Answers the trigger's target choice with `target` when one is raised (CR 603.3d)
/// and returns with the trigger on the stack.
fn drain_targeting(
    runner: &mut GameRunner,
    board_drain: ObjectId,
    emet: ObjectId,
    target: ObjectId,
) {
    let p1_life = runner.life(P1);
    let stacked_before = stacked_trigger_refs(runner, emet).len();
    activate_and_resolve(runner, board_drain);
    assert_eq!(
        runner.life(P1),
        p1_life - 1,
        "reach: the opponent lost life"
    );
    if matches!(
        runner.state().waiting_for,
        WaitingFor::TriggerTargetSelection { .. }
    ) {
        runner
            .act(GameAction::ChooseTarget {
                target: Some(TargetRef::Object(target)),
            })
            .expect("target the graveyard instant");
    }
    assert_eq!(
        stacked_trigger_refs(runner, emet).len(),
        stacked_before + 1,
        "reach: the life loss triggers Emet-Selch"
    );
}

/// The Emet-Selch trigger's ledger key, read off a stacked instance.
fn stacked_emet_ref(runner: &GameRunner, emet: ObjectId) -> TriggerDefinitionRef {
    match stacked_trigger_refs(runner, emet).first() {
        Some(Some(key)) => key.clone(),
        other => panic!("an Emet-Selch trigger with a definition ref is stacked: {other:?}"),
    }
}

/// E1 (maintainer's case): accepting "you may cast" opens the cast offer; declining
/// the offer casts nothing, so no indicated action is taken and the next life loss
/// triggers Emet-Selch again.
#[test]
fn emet_selch_declined_cast_offer_does_not_spend_the_limit() {
    let EmetBoard {
        mut runner,
        emet,
        drain,
        instants,
    } = emet_board(&["Test Draw"]);
    let test_draw = instants[0];

    drain_targeting(&mut runner, drain, emet, test_draw);
    runner.advance_until_stack_empty();
    assert_optional_prompt(&runner, P0, "reach: Emet-Selch offers its may");
    decide(&mut runner, true);
    assert_eq!(
        offered_card(&runner),
        Some(test_draw),
        "reach: accepting opens the cast offer for the targeted card"
    );
    decline_offer(&mut runner);
    assert_settled_priority(&runner, "after declining the cast offer");
    assert!(
        indicated_actions(&runner).is_empty(),
        "CR 733.1: an offered cast that is not cast takes no indicated action"
    );
    assert_eq!(runner.state().objects[&test_draw].zone, Zone::Graveyard);

    drain_targeting(&mut runner, drain, emet, test_draw);
    runner.advance_until_stack_empty();
    assert_optional_prompt(
        &runner,
        P0,
        "the untaken limit leaves Emet-Selch free to trigger",
    );
}

/// E2 + E3: a cast started and then cancelled at payment spends nothing (the limit
/// rides the cast, not the offer); a cast that completes spends it at CR 601.2i,
/// and the ability stops triggering for the turn.
#[test]
fn emet_selch_cancelled_cast_spends_nothing_and_a_completed_cast_spends_the_limit() {
    let EmetBoard {
        mut runner,
        emet,
        drain,
        instants,
    } = emet_board(&["Test Draw"]);
    let test_draw = instants[0];

    // E2: accept, elect the cast, then cancel at mana payment.
    drain_targeting(&mut runner, drain, emet, test_draw);
    let emet_ref = stacked_emet_ref(&runner, emet);
    runner.advance_until_stack_empty();
    assert_optional_prompt(&runner, P0, "reach: Emet-Selch offers its may");
    decide(&mut runner, true);
    assert_eq!(offered_card(&runner), Some(test_draw));
    runner
        .act(GameAction::GraveyardPaidCastChoice {
            choice: CastChoice::Cast,
        })
        .expect("elect the offered cast");
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::ManaPayment { .. }),
        "reach: the elected cast pauses at mana payment, got {:?}",
        runner.state().waiting_for
    );
    assert!(
        indicated_actions(&runner).is_empty(),
        "an elected cast still paying has not become cast (CR 601.2i)"
    );
    runner
        .act(GameAction::CancelCast)
        .expect("cancel the cast at payment");
    runner.advance_until_stack_empty();
    assert_settled_priority(&runner, "after cancelling the cast");
    assert!(
        indicated_actions(&runner).is_empty(),
        "CR 733.1: a cancelled cast is reversed and takes no indicated action"
    );
    assert_eq!(runner.state().objects[&test_draw].zone, Zone::Graveyard);

    // E3: the next life loss triggers again; this time the cast completes.
    drain_targeting(&mut runner, drain, emet, test_draw);
    runner.advance_until_stack_empty();
    assert_optional_prompt(&runner, P0, "the cancelled cast left the limit untaken");
    decide(&mut runner, true);
    assert_eq!(offered_card(&runner), Some(test_draw));
    accept_offer_and_pay(&mut runner);
    assert_eq!(
        runner.state().objects[&test_draw].zone,
        Zone::Stack,
        "reach: the offered card was cast"
    );
    assert_eq!(
        indicated_actions(&runner),
        HashSet::from([(emet_ref, P0)]),
        "CR 601.2i: the spell became cast, so the controller took the indicated action"
    );

    // The drain is an instant-speed activation: the next life loss comes while the
    // cast spell is still on the stack, and Emet-Selch no longer triggers.
    let p1_life = runner.life(P1);
    activate_and_resolve(&mut runner, drain);
    assert_eq!(
        runner.life(P1),
        p1_life - 1,
        "reach: the opponent lost life again"
    );
    assert!(
        stacked_trigger_refs(&runner, emet).is_empty(),
        "the action was taken this turn, so Emet-Selch no longer triggers"
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "no Emet-Selch trigger is waiting for a target either, got {:?}",
        runner.state().waiting_for
    );
    assert_eq!(
        runner
            .state()
            .stack
            .iter()
            .map(|entry| entry.id)
            .collect::<Vec<_>>(),
        vec![test_draw],
        "only the cast spell remains on the stack"
    );
}

/// Two Emet-Selch instances on the stack, each targeting its own instant (so the
/// lower one's target stays legal, CR 608.2b): instance 1 targets `instants[0]`,
/// instance 2 — on top — targets `instants[1]`.
fn emet_stacked_twice() -> EmetBoard {
    let mut board = emet_board(&["Test Draw A", "Test Draw B"]);
    let (lower, upper) = (board.instants[0], board.instants[1]);
    drain_targeting(&mut board.runner, board.drain, board.emet, lower);
    drain_targeting(&mut board.runner, board.drain, board.emet, upper);
    assert_eq!(
        stacked_trigger_refs(&board.runner, board.emet).len(),
        2,
        "reach: two Emet-Selch instances are stacked"
    );
    board
}

/// E4a (stacked instances): the top instance accepts and declines the offer, which
/// takes no action, so the lower instance still offers its may.
#[test]
fn emet_selch_top_instance_declining_the_offer_leaves_the_lower_instance_free() {
    let EmetBoard {
        mut runner,
        emet: _,
        drain: _,
        instants,
    } = emet_stacked_twice();
    runner.advance_until_stack_empty();
    assert_optional_prompt(&runner, P0, "reach: the top instance offers its may");
    decide(&mut runner, true);
    assert_eq!(
        offered_card(&runner),
        Some(instants[1]),
        "reach: the top instance offers its own target"
    );
    decline_offer(&mut runner);
    assert_optional_prompt(
        &runner,
        P0,
        "the top instance took no action, so the lower instance still offers its may",
    );
    assert!(indicated_actions(&runner).is_empty());
}

/// E4b (stacked instances): the top instance casts its card, which becomes cast
/// while that instance resolves (CR 608.2g) — so when the lower instance resolves
/// the action has been taken and it does nothing.
#[test]
fn emet_selch_top_instance_casting_leaves_the_lower_instance_with_nothing_to_do() {
    let EmetBoard {
        mut runner,
        emet,
        drain: _,
        instants,
    } = emet_stacked_twice();
    let emet_ref = stacked_emet_ref(&runner, emet);
    runner.advance_until_stack_empty();
    assert_optional_prompt(&runner, P0, "reach: the top instance offers its may");
    decide(&mut runner, true);
    assert_eq!(offered_card(&runner), Some(instants[1]));
    accept_offer_and_pay(&mut runner);
    assert_eq!(
        runner.state().stack.last().map(|entry| entry.id),
        Some(instants[1]),
        "reach: the top instance's card is cast above the lower instance"
    );
    runner.advance_until_stack_empty();
    assert_settled_priority(
        &runner,
        "the lower instance resolves with no prompt once the action was taken",
    );
    assert_eq!(
        runner.state().objects[&instants[0]].zone,
        Zone::Graveyard,
        "the lower instance cast nothing"
    );
    assert_eq!(indicated_actions(&runner), HashSet::from([(emet_ref, P0)]));
}

const CRYPTOLITH_FRAGMENT: &str = "This artifact enters tapped.\n{T}: Add one mana of any color. Each player loses 1 life.\nAt the beginning of your upkeep, if each player has 10 or less life, transform this artifact.";

/// How P0 pays for Emet-Selch's offered cast with Cryptolith Fragment.
#[derive(Clone, Copy)]
enum FragmentPayment {
    /// Pass priority at the payment prompt: auto-payment taps the fragment.
    Auto,
    /// Activate the fragment's mana ability at the payment prompt, then pay.
    Manual,
}

/// E5 (CR 603.2 + CR 603.2h + CR 601.2g + CR 601.2i): Emet-Selch's offered card is
/// paid for with Cryptolith Fragment (the only mana source), so each player loses 1
/// life while the cast is being paid for — before the spell becomes cast. The
/// controller has not yet taken the indicated action, so that life loss triggers
/// Emet-Selch; the new instance resolves after the cast completed, so it does
/// nothing.
fn emet_selch_life_loss_while_paying_triggers_it_again(payment: FragmentPayment) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let emet = scenario
        .add_creature_from_oracle(P0, "Emet-Selch of the Third Seat", 2, 2, EMET_SELCH)
        .id();
    let drain = scenario
        .add_artifact_from_oracle(P0, "Life Drain Engine", LIFE_DRAIN_ENGINE)
        .id();
    let fragment = scenario
        .add_artifact_from_oracle(P0, "Cryptolith Fragment", CRYPTOLITH_FRAGMENT)
        .id();
    let [cast_card, other_card] = ["Test Draw A", "Test Draw B"].map(|name| {
        scenario
            .add_spell_to_graveyard(P0, name, true)
            .from_oracle_text("Draw a card.")
            .with_mana_cost(ManaCost::Cost {
                shards: vec![ManaCostShard::Red],
                generic: 0,
            })
            .id()
    });
    scenario.with_library_top(P0, &["Card A", "Card B", "Card C", "Card D"]);
    let mut runner = scenario.build();
    assert!(
        !runner.state().objects[&fragment].tapped,
        "reach: the fragment is untapped and can pay for the cast"
    );

    drain_targeting(&mut runner, drain, emet, cast_card);
    let emet_ref = stacked_emet_ref(&runner, emet);
    runner.advance_until_stack_empty();
    assert_optional_prompt(&runner, P0, "reach: Emet-Selch offers its may");
    decide(&mut runner, true);
    assert_eq!(offered_card(&runner), Some(cast_card));
    let p1_life = runner.life(P1);
    runner
        .act(GameAction::GraveyardPaidCastChoice {
            choice: CastChoice::Cast,
        })
        .expect("elect the offered cast");
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::ManaPayment { .. }),
        "reach: the elected cast pauses at mana payment, got {:?}",
        runner.state().waiting_for
    );
    if let FragmentPayment::Manual = payment {
        activate_only_ability(&mut runner, fragment, &[]);
        if matches!(
            runner.state().waiting_for,
            WaitingFor::ChooseManaColor { .. }
        ) {
            runner
                .act(GameAction::ChooseManaColor {
                    choice: ManaChoice::SingleColor(ManaType::Red),
                    count: 1,
                })
                .expect("tap the fragment for red");
        }
        assert_eq!(
            runner.life(P1),
            p1_life - 1,
            "reach: the opponent lost life before the cast was paid for"
        );
        assert!(
            indicated_actions(&runner).is_empty(),
            "the spell has not become cast yet (CR 601.2i)"
        );
    }
    for _ in 0..8 {
        if !matches!(runner.state().waiting_for, WaitingFor::ManaPayment { .. }) {
            break;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("pay the offered cast");
    }
    if matches!(
        runner.state().waiting_for,
        WaitingFor::TriggerTargetSelection { .. }
    ) {
        runner
            .act(GameAction::ChooseTarget {
                target: Some(TargetRef::Object(other_card)),
            })
            .expect("target the remaining graveyard instant");
    }
    assert!(
        runner.state().objects[&fragment].tapped,
        "reach: the fragment paid for the cast"
    );
    assert_eq!(
        runner.life(P1),
        p1_life - 1,
        "reach: the opponent lost life once, while the cast was being paid for"
    );
    assert_eq!(
        runner.state().objects[&cast_card].zone,
        Zone::Stack,
        "reach: the offered card was cast"
    );
    assert_eq!(
        indicated_actions(&runner),
        HashSet::from([(emet_ref, P0)]),
        "CR 601.2i: the spell became cast, so the controller took the indicated action"
    );
    assert_eq!(
        stacked_trigger_refs(&runner, emet).len(),
        1,
        "CR 603.2h: the life loss came before the spell became cast, so it triggers Emet-Selch"
    );

    runner.advance_until_stack_empty();
    assert_settled_priority(
        &runner,
        "the new instance resolves with no prompt once the action was taken",
    );
    assert_eq!(
        runner.state().objects[&other_card].zone,
        Zone::Graveyard,
        "the new instance cast nothing"
    );
}

#[test]
fn emet_selch_life_loss_while_auto_paying_for_its_cast_triggers_it_again() {
    emet_selch_life_loss_while_paying_triggers_it_again(FragmentPayment::Auto);
}

#[test]
fn emet_selch_life_loss_while_manually_paying_for_its_cast_triggers_it_again() {
    emet_selch_life_loss_while_paying_triggers_it_again(FragmentPayment::Manual);
}

const PLAY_OF_THE_GAME: &str =
    "Assist (Another player can pay up to {6} of this spell's cost.)\nExile all nonland permanents.";

/// E5b (CR 603.2 + CR 603.2h + CR 702.132a + CR 601.2h + CR 601.2i): Emet-Selch
/// offers Play of the Game from P0's graveyard and P1 assists with one generic
/// mana from Cryptolith Fragment, their only mana source. The helper's tap makes
/// each player lose 1 life while the cast is being paid for — before the spell
/// becomes cast — so P1's life loss triggers Emet-Selch again. A second eligible
/// instant stays in the graveyard, so the new instance always has a target.
#[test]
fn emet_selch_life_loss_while_an_assisting_opponent_pays_triggers_it_again() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let emet = scenario
        .add_creature_from_oracle(P0, "Emet-Selch of the Third Seat", 2, 2, EMET_SELCH)
        .id();
    let drain = scenario
        .add_artifact_from_oracle(P0, "Life Drain Engine", LIFE_DRAIN_ENGINE)
        .id();
    let fragment = scenario
        .add_artifact_from_oracle(P1, "Cryptolith Fragment", CRYPTOLITH_FRAGMENT)
        .id();
    let play_of_the_game = scenario
        .add_spell_to_graveyard(P0, "Play of the Game", false)
        .from_oracle_text_with_keywords(&["Assist"], PLAY_OF_THE_GAME)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::White, ManaCostShard::White],
            generic: 6,
        })
        .id();
    let other_card = scenario
        .add_spell_to_graveyard(P0, "Test Draw", true)
        .from_oracle_text("Draw a card.")
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Red],
            generic: 0,
        })
        .id();
    // CR 118.7a + CR 702.132a: {6}{W}{W} less Emet-Selch's {2} is {4}{W}{W}; P1
    // pays one generic, P0 the remaining {3}{W}{W}.
    for _ in 0..5 {
        scenario.add_basic_land(P0, ManaColor::White);
    }
    scenario.with_library_top(P0, &["Card A", "Card B", "Card C", "Card D"]);
    let mut runner = scenario.build();
    assert!(
        runner.state().objects[&play_of_the_game]
            .keywords
            .contains(&Keyword::Assist),
        "reach: Play of the Game has Assist"
    );
    assert!(
        !runner.state().objects[&fragment].tapped,
        "reach: the helper's fragment is untapped and can assist"
    );

    drain_targeting(&mut runner, drain, emet, play_of_the_game);
    let emet_ref = stacked_emet_ref(&runner, emet);
    runner.advance_until_stack_empty();
    assert_optional_prompt(&runner, P0, "reach: Emet-Selch offers its may");
    decide(&mut runner, true);
    assert_eq!(offered_card(&runner), Some(play_of_the_game));
    runner
        .act(GameAction::GraveyardPaidCastChoice {
            choice: CastChoice::Cast,
        })
        .expect("elect the offered cast");
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::AssistChoosePlayer { player: P0, .. }
        ),
        "reach: the elected cast offers Assist, got {:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::ChooseAssistPlayer { player: Some(P1) })
        .expect("choose the opponent to assist");
    let (p0_life, p1_life) = (runner.life(P0), runner.life(P1));
    runner
        .act(GameAction::CommitAssistPayment { generic: 1 })
        .expect("the opponent commits one generic mana");
    for _ in 0..8 {
        if !matches!(runner.state().waiting_for, WaitingFor::ManaPayment { .. }) {
            break;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("pay the offered cast");
    }
    if matches!(
        runner.state().waiting_for,
        WaitingFor::TriggerTargetSelection { .. }
    ) {
        runner
            .act(GameAction::ChooseTarget {
                target: Some(TargetRef::Object(other_card)),
            })
            .expect("target the remaining graveyard instant");
    }
    assert!(
        runner.state().objects[&fragment].tapped,
        "reach: the helper paid with the fragment"
    );
    assert_eq!(
        (runner.life(P0), runner.life(P1)),
        (p0_life - 1, p1_life - 1),
        "reach: each player lost 1 life while the cast was being paid for"
    );
    assert_eq!(
        runner.state().objects[&play_of_the_game].zone,
        Zone::Stack,
        "reach: the offered card was cast"
    );
    assert_eq!(
        indicated_actions(&runner),
        HashSet::from([(emet_ref, P0)]),
        "CR 601.2i: the spell became cast, so the controller took the indicated action"
    );
    assert_eq!(
        stacked_trigger_refs(&runner, emet).len(),
        1,
        "CR 603.2h: the assisting opponent's life loss came before the spell became cast, \
         so it triggers Emet-Selch"
    );

    runner.advance_until_stack_empty();
    assert_settled_priority(
        &runner,
        "the new instance resolves with no prompt once the action was taken",
    );
    assert_eq!(
        runner.state().objects[&other_card].zone,
        Zone::Graveyard,
        "the new instance cast nothing"
    );
    assert_eq!(
        runner.state().objects[&emet].zone,
        Zone::Exile,
        "reach: Play of the Game resolved and exiled every nonland permanent"
    );
}

/// Planetarium of Wan Shi Tong (full Oracle text) with `looked` on top of P0's
/// library and two "Scry 1." enablers in hand.
struct PlanetariumBoard {
    runner: GameRunner,
    planetarium: ObjectId,
    looked: ObjectId,
    enablers: [ObjectId; 2],
}

fn planetarium_board(looked_name: &str, looked_oracle: &str) -> PlanetariumBoard {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let planetarium = scenario
        .add_artifact_from_oracle(P0, "Planetarium of Wan Shi Tong", PLANETARIUM_ORACLE)
        .id();
    scenario.with_library_top(P0, &["Card A", "Card B", "Card C"]);
    let looked = scenario
        .add_spell_to_library_top(P0, looked_name, true)
        .from_oracle_text(looked_oracle)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Red],
            generic: 0,
        })
        .id();
    let enablers = [
        scenario
            .add_spell_to_hand_from_oracle(P0, "Scry Enabler One", false, "Scry 1.")
            .with_mana_cost(free())
            .id(),
        scenario
            .add_spell_to_hand_from_oracle(P0, "Scry Enabler Two", false, "Scry 1.")
            .with_mana_cost(free())
            .id(),
    ];
    PlanetariumBoard {
        runner: scenario.build(),
        planetarium,
        looked,
        enablers,
    }
}

/// Cast a "Scry 1." enabler and keep the scried card on top. Returns with any
/// Planetarium trigger resolved up to its prompt.
fn scry_keeping_top(runner: &mut GameRunner, enabler: ObjectId) {
    cast(runner, enabler);
    runner.advance_until_stack_empty();
    let WaitingFor::ScryChoice { cards, .. } = runner.state().waiting_for.clone() else {
        panic!(
            "reach: the enabler scries, got {:?}",
            runner.state().waiting_for
        );
    };
    runner
        .act(GameAction::SelectCards { cards })
        .expect("keep the scried card on top");
    runner.advance_until_stack_empty();
}

/// P1: Planetarium's free cast spends the limit when the looked-at spell becomes
/// cast, and the ability stops triggering for the turn.
#[test]
fn planetarium_free_cast_spends_the_limit_when_the_spell_becomes_cast() {
    let PlanetariumBoard {
        mut runner,
        planetarium,
        looked,
        enablers,
    } = planetarium_board("Looked Draw", "Draw a card.");

    cast(&mut runner, enablers[0]);
    runner.advance_until_stack_empty();
    let WaitingFor::ScryChoice { cards, .. } = runner.state().waiting_for.clone() else {
        panic!(
            "reach: the enabler scries, got {:?}",
            runner.state().waiting_for
        );
    };
    runner
        .act(GameAction::SelectCards { cards })
        .expect("keep the scried card on top");
    let planetarium_ref = match stacked_trigger_refs(&runner, planetarium).as_slice() {
        [Some(key)] => key.clone(),
        other => panic!("reach: the scry triggers Planetarium once, got {other:?}"),
    };
    runner.advance_until_stack_empty();
    assert_optional_prompt(&runner, P0, "reach: Planetarium offers its may");
    decide(&mut runner, true);
    assert_eq!(
        runner.state().objects[&looked].zone,
        Zone::Stack,
        "reach: the looked-at card is cast during the resolution"
    );
    assert_eq!(
        indicated_actions(&runner),
        HashSet::from([(planetarium_ref, P0)]),
        "CR 601.2i: the free cast became cast, so the indicated action was taken"
    );
    runner.advance_until_stack_empty();
    assert_settled_priority(&runner, "after the free spell resolved");

    scry_keeping_top(&mut runner, enablers[1]);
    assert!(
        stacked_trigger_refs(&runner, planetarium).is_empty(),
        "the action was taken this turn, so Planetarium no longer triggers"
    );
    assert_settled_priority(&runner, "no further Planetarium prompt this turn");
}

/// P2: a free cast cancelled at its target choice is reversed (CR 733.1): the
/// looked-at card returns to the library, no action is taken, and the next scry
/// triggers Planetarium again.
#[test]
fn planetarium_cancelled_free_cast_does_not_spend_the_limit() {
    let PlanetariumBoard {
        mut runner,
        planetarium: _,
        looked,
        enablers,
    } = planetarium_board("Shock", SHOCK);

    scry_keeping_top(&mut runner, enablers[0]);
    assert_optional_prompt(&runner, P0, "reach: Planetarium offers its may");
    decide(&mut runner, true);
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::TargetSelection { .. }
        ),
        "reach: the free Shock pauses at its target choice, got {:?}",
        runner.state().waiting_for
    );
    assert!(
        indicated_actions(&runner).is_empty(),
        "a cast still choosing targets has not become cast (CR 601.2i)"
    );
    runner
        .act(GameAction::CancelCast)
        .expect("cancel the free cast at its target choice");
    runner.advance_until_stack_empty();
    assert_settled_priority(&runner, "after cancelling the free cast");
    assert!(
        indicated_actions(&runner).is_empty(),
        "CR 733.1: a cancelled cast is reversed and takes no indicated action"
    );
    assert_eq!(
        runner.state().objects[&looked].zone,
        Zone::Library,
        "the cancelled card returns to the library"
    );

    scry_keeping_top(&mut runner, enablers[1]);
    assert_optional_prompt(
        &runner,
        P0,
        "the untaken limit leaves Planetarium free to trigger again",
    );
}

// ---------------------------------------------------------------------------
// An impossible move or reveal is not offered and spends no limit
// (CR 608.2d + CR 603.2h)
// ---------------------------------------------------------------------------

const THE_REAPER: &str = "When The Reaper enters, put a -1/-1 counter on each of up to two target creatures.\nWhenever a creature an opponent controls with a -1/-1 counter on it dies, you may put that card onto the battlefield under your control. Do this only once each turn.";

const ACADEMY_RECTOR: &str = "When this creature dies, you may exile it. If you do, search your library for an enchantment card, put that card onto the battlefield, then shuffle.";

const BLADEWINGS_THRALL: &str = "This creature has flying as long as you control a Dragon.\nWhen a Dragon enters, you may return this card from your graveyard to the battlefield.";

const PRIORITY_BOARDING: &str = "Whenever you roll a die, you may reveal the top card of your library. Do this only once each turn. Whenever you reveal a card with mana value less than the result this way, you may exile it. If you do, you may play it this turn.";

/// Synthetic: a free removal spell, so a creature dies on demand.
const DESTROY_TARGET_CREATURE: &str = "Destroy target creature.";

/// Synthetic: a free graveyard exile, cast in response to a trigger whose object
/// is in a graveyard.
const EXILE_GRAVEYARD_CARD: &str = "Exile target card from a graveyard.";

/// Synthetic: a free die roll, so Priority Boarding triggers on demand.
const DIE_ROLLER: &str = "{0}: Roll a d20.";

/// Synthetic: a free way to put the drawn card back on top of the library.
const HAND_TO_LIBRARY_TOP: &str = "{0}: Put a card from your hand on top of your library.";

/// Synthetic: an optional reveal whose "If you do" rider shows whether the
/// reveal was performed.
const REVEAL_FOR_LIFE: &str =
    "{0}: You may reveal the top card of your library. If you do, you gain 3 life.";

fn add_free_spell(
    scenario: &mut GameScenario,
    name: &str,
    is_instant: bool,
    text: &str,
) -> ObjectId {
    scenario
        .add_spell_to_hand_from_oracle(P0, name, is_instant, text)
        .with_mana_cost(free())
        .id()
}

/// Cast `spell` at `target` and pass priority until the spell has resolved,
/// stopping with any triggers it caused on the stack.
fn cast_at_and_resolve(runner: &mut GameRunner, spell: ObjectId, target: ObjectId) {
    let _ = runner.cast(spell).target_object(target).commit();
    resolve_spell(runner, spell, "Dinosaur");
}

/// Cast `spell` at `target` and pass priority until the stack is empty, leaving
/// every "you may" unanswered: a prompt stops the driver where it stands.
fn cast_at_and_resolve_stack(runner: &mut GameRunner, spell: ObjectId, target: ObjectId) {
    runner
        .cast(spell)
        .target_object(target)
        .commit()
        .stop_at_optional()
        .resolve();
}

/// CR 608.2d + CR 400.7 + CR 603.2h: a card exiled from the graveyard before The
/// Reaper's trigger resolves is a new object that can't be put onto the
/// battlefield, so the "you may" is not offered and no indicated action is taken;
/// the next qualifying death triggers The Reaper again, and taking that card
/// spends the limit.
#[test]
fn the_reaper_exiled_card_is_not_offered_and_does_not_spend_the_limit() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let reaper = scenario
        .add_creature_from_oracle(P0, "The Reaper, King No More", 3, 3, THE_REAPER)
        .as_artifact_creature()
        .id();
    let bears = ["Bear A", "Bear B", "Bear C"].map(|name| {
        scenario
            .add_creature(P1, name, 2, 2)
            .with_minus_counters(1)
            .id()
    });
    let murders = ["Murder A", "Murder B", "Murder C"]
        .map(|name| add_free_spell(&mut scenario, name, false, DESTROY_TARGET_CREATURE));
    let purge = add_free_spell(&mut scenario, "Purge", true, EXILE_GRAVEYARD_CARD);
    let mut runner = scenario.build();

    // Leg 1: Bear A dies, then is exiled in response to The Reaper's trigger.
    cast_at_and_resolve(&mut runner, murders[0], bears[0]);
    let refs = stacked_trigger_refs(&runner, reaper);
    let [Some(reaper_ref)] = refs.as_slice() else {
        panic!("reach: Bear A's death triggers The Reaper once, got {refs:?}");
    };
    let reaper_ref = reaper_ref.clone();
    cast_at_and_resolve_stack(&mut runner, purge, bears[0]);
    assert_eq!(runner.state().objects[&bears[0]].zone, Zone::Exile);
    assert_settled_priority(&runner, "an exiled card's return is never offered");
    assert!(
        indicated_actions(&runner).is_empty(),
        "an impossible return takes no indicated action"
    );

    // Leg 2: Bear B dies — the untaken limit lets The Reaper trigger and offer it.
    cast_at_and_resolve(&mut runner, murders[1], bears[1]);
    assert_eq!(
        stacked_trigger_refs(&runner, reaper).len(),
        1,
        "the untaken limit leaves The Reaper free to trigger"
    );
    runner.advance_until_stack_empty();
    assert_optional_prompt(&runner, P0, "a card still in the graveyard is offered");
    decide(&mut runner, true);
    runner.advance_until_stack_empty();
    assert_settled_priority(&runner, "after taking Bear B");
    assert_eq!(runner.state().objects[&bears[1]].zone, Zone::Battlefield);
    assert_eq!(runner.state().objects[&bears[1]].controller, P0);
    assert_eq!(
        indicated_actions(&runner),
        HashSet::from([(reaper_ref, P0)]),
        "putting the card onto the battlefield takes the indicated action"
    );

    // Leg 3: the limit is spent — Bear C's death does not trigger The Reaper.
    cast_at_and_resolve(&mut runner, murders[2], bears[2]);
    assert_eq!(
        runner.state().objects[&bears[2]].zone,
        Zone::Graveyard,
        "reach: Bear C died"
    );
    assert!(
        stacked_trigger_refs(&runner, reaper).is_empty(),
        "The Reaper's controller took its action this turn, so it no longer triggers"
    );
}

/// Academy Rector, killed by a free removal spell, with its dies trigger on the
/// stack, an enchantment card in P0's library and a free graveyard exile in hand.
struct RectorBoard {
    runner: GameRunner,
    rector: ObjectId,
    enchantment: ObjectId,
    purge: ObjectId,
}

fn rector_board() -> RectorBoard {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let rector = scenario
        .add_creature_from_oracle(P0, "Academy Rector", 1, 2, ACADEMY_RECTOR)
        .id();
    let enchantment = scenario
        .add_spell_to_library_top(P0, "Library Enchantment", false)
        .as_enchantment()
        .id();
    let murder = add_free_spell(&mut scenario, "Murder", false, DESTROY_TARGET_CREATURE);
    let purge = add_free_spell(&mut scenario, "Purge", true, EXILE_GRAVEYARD_CARD);
    let mut runner = scenario.build();
    cast_at_and_resolve(&mut runner, murder, rector);
    assert_eq!(
        stacked_trigger_refs(&runner, rector).len(),
        1,
        "reach: Academy Rector's death triggers it"
    );
    RectorBoard {
        runner,
        rector,
        enchantment,
        purge,
    }
}

/// CR 608.2d + CR 400.7 + CR 118.12: Academy Rector exiled from the graveyard in
/// response can't be exiled by its own trigger, so "you may exile it" is not
/// offered and its "If you do" search does not happen; with Rector still in the
/// graveyard the same trigger is offered and accepting it searches.
#[test]
fn academy_rector_exiled_in_response_is_not_offered_and_does_not_search() {
    let RectorBoard {
        mut runner,
        rector,
        enchantment,
        purge,
    } = rector_board();
    cast_at_and_resolve_stack(&mut runner, purge, rector);
    assert_eq!(runner.state().objects[&rector].zone, Zone::Exile);
    assert_settled_priority(&runner, "an exiled Rector's exile is never offered");
    assert_eq!(
        runner.state().objects[&enchantment].zone,
        Zone::Library,
        "the \"If you do\" search did not happen"
    );

    let RectorBoard {
        mut runner,
        rector,
        enchantment: _,
        purge: _,
    } = rector_board();
    runner.advance_until_stack_empty();
    assert_optional_prompt(&runner, P0, "a Rector still in the graveyard is offered");
    decide(&mut runner, true);
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::SearchChoice { .. }),
        "exiling Rector runs the \"If you do\" search, got {:?}",
        runner.state().waiting_for
    );
    assert_eq!(runner.state().objects[&rector].zone, Zone::Exile);
}

/// CR 608.2d + CR 400.7: Bladewing's Thrall exiled from the graveyard in response
/// to its own trigger can't be returned, so "you may return this card" is not
/// offered; with the Thrall still in the graveyard it is offered and returns.
#[test]
fn bladewings_thrall_exiled_in_response_is_not_offered() {
    let build = || {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let thrall = scenario
            .add_creature_to_graveyard(P0, "Bladewing's Thrall", 3, 3)
            .with_subtypes(vec!["Zombie"])
            .from_oracle_text(BLADEWINGS_THRALL)
            .id();
        let dragon = scenario
            .add_creature_to_hand(P0, "Test Dragon", 4, 4)
            .with_subtypes(vec!["Dragon"])
            .with_mana_cost(free())
            .id();
        let purge = add_free_spell(&mut scenario, "Purge", true, EXILE_GRAVEYARD_CARD);
        let mut runner = scenario.build();
        cast(&mut runner, dragon);
        resolve_spell(&mut runner, dragon, "Dinosaur");
        assert_eq!(
            stacked_trigger_refs(&runner, thrall).len(),
            1,
            "reach: the Dragon's entry triggers Bladewing's Thrall from the graveyard"
        );
        (runner, thrall, purge)
    };

    let (mut runner, thrall, purge) = build();
    cast_at_and_resolve_stack(&mut runner, purge, thrall);
    assert_eq!(runner.state().objects[&thrall].zone, Zone::Exile);
    assert_settled_priority(&runner, "an exiled Thrall's return is never offered");

    let (mut runner, thrall, _) = build();
    runner.advance_until_stack_empty();
    assert_optional_prompt(&runner, P0, "a Thrall still in the graveyard is offered");
    decide(&mut runner, true);
    runner.advance_until_stack_empty();
    assert_settled_priority(&runner, "after returning the Thrall");
    assert_eq!(runner.state().objects[&thrall].zone, Zone::Battlefield);
}

/// Priority Boarding with a free die roller, a free draw and a free way to put
/// the drawn card back on top of P0's library, which holds only `card_a`.
struct BoardingBoard {
    runner: GameRunner,
    boarding: ObjectId,
    roller: ObjectId,
    drawer: ObjectId,
    topper: ObjectId,
    card_a: ObjectId,
}

fn boarding_board() -> BoardingBoard {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let boarding = scenario
        .add_enchantment_from_oracle(P0, "Priority Boarding", PRIORITY_BOARDING)
        .id();
    let roller = scenario
        .add_artifact_from_oracle(P0, "Die Roller", DIE_ROLLER)
        .id();
    let drawer = scenario
        .add_artifact_from_oracle(P0, "Card Drawer", FREE_DRAW)
        .id();
    let topper = scenario
        .add_artifact_from_oracle(P0, "Library Topper", HAND_TO_LIBRARY_TOP)
        .id();
    let card_a = scenario.add_card_to_library_top(P0, "Card A");
    scenario.with_library_top(P1, &["P1 Card A", "P1 Card B", "P1 Card C"]);
    BoardingBoard {
        runner: scenario.build(),
        boarding,
        roller,
        drawer,
        topper,
        card_a,
    }
}

/// On another player's turn the active player receives priority after each
/// resolution (CR 117.3b); pass it around to P0 so P0 can activate.
fn p0_priority(runner: &mut GameRunner) {
    while matches!(runner.state().waiting_for, WaitingFor::Priority { player } if player != P0) {
        runner.act(GameAction::PassPriority).expect("pass to P0");
    }
}

/// Draw `card_a`, leaving P0's library empty.
fn draw_card_a(board: &mut BoardingBoard) {
    p0_priority(&mut board.runner);
    activate_and_resolve(&mut board.runner, board.drawer);
    assert_eq!(
        board.runner.state().objects[&board.card_a].zone,
        Zone::Hand,
        "reach: Card A was drawn"
    );
    assert!(board.runner.state().players[P0.0 as usize]
        .library
        .is_empty());
}

/// Put `card_a` from P0's hand back on top of the library.
fn return_card_a(board: &mut BoardingBoard) {
    p0_priority(&mut board.runner);
    activate_and_resolve(&mut board.runner, board.topper);
    if matches!(
        board.runner.state().waiting_for,
        WaitingFor::EffectZoneChoice { .. }
    ) {
        board
            .runner
            .act(GameAction::SelectCards {
                cards: vec![board.card_a],
            })
            .expect("put Card A on top of the library");
    }
    assert_eq!(
        board.runner.state().players[P0.0 as usize].library.front(),
        Some(&board.card_a),
        "reach: Card A is back on top of the library"
    );
}

/// Roll a die; returns the number of Priority Boarding triggers it stacked.
fn roll(board: &mut BoardingBoard) -> usize {
    p0_priority(&mut board.runner);
    activate_and_resolve(&mut board.runner, board.roller);
    stacked_trigger_refs(&board.runner, board.boarding).len()
}

/// Resolve the stack, declining every "you may" (Priority Boarding's "you may
/// exile it" rider) and returning the events, so a reveal can be observed.
fn resolve_declining(runner: &mut GameRunner) -> Vec<engine::types::events::GameEvent> {
    let mut events = Vec::new();
    for _ in 0..40 {
        let action = match runner.state().waiting_for {
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => return events,
            WaitingFor::Priority { .. } => GameAction::PassPriority,
            WaitingFor::OptionalEffectChoice { .. } => {
                GameAction::DecideOptionalEffect { accept: false }
            }
            ref other => panic!("unexpected prompt while resolving: {other:?}"),
        };
        events.extend(runner.act(action).expect("resolve the stack").events);
    }
    panic!("the stack never settled");
}

fn revealed(events: &[engine::types::events::GameEvent], card: ObjectId) -> bool {
    events.iter().any(|event| {
        matches!(
            event,
            engine::types::events::GameEvent::CardsRevealed { card_ids, .. }
                if card_ids.contains(&card)
        )
    })
}

/// CR 608.2d + CR 701.20a + CR 603.2h: with an empty library, Priority Boarding's
/// "you may reveal the top card of your library" would reveal nothing, so it is
/// not offered and no indicated action is taken; once the library has a card the
/// next roll triggers it again, and revealing spends the limit.
#[test]
fn priority_boarding_empty_library_reveal_is_not_offered_and_does_not_spend_the_limit() {
    let mut board = boarding_board();
    draw_card_a(&mut board);

    // Leg 1: empty library — the trigger resolves without a prompt.
    let refs = {
        activate_and_resolve(&mut board.runner, board.roller);
        stacked_trigger_refs(&board.runner, board.boarding)
    };
    let [Some(boarding_ref)] = refs.as_slice() else {
        panic!("reach: the roll triggers Priority Boarding once, got {refs:?}");
    };
    let boarding_ref = boarding_ref.clone();
    board.runner.advance_until_stack_empty();
    assert_settled_priority(&board.runner, "an empty-library reveal is never offered");
    assert!(
        indicated_actions(&board.runner).is_empty(),
        "an impossible reveal takes no indicated action"
    );

    // Leg 2: Card A back on top — the next roll triggers again and is offered.
    return_card_a(&mut board);
    assert_eq!(
        roll(&mut board),
        1,
        "the untaken limit leaves Priority Boarding free to trigger"
    );
    board.runner.advance_until_stack_empty();
    assert_optional_prompt(&board.runner, P0, "a non-empty library's reveal is offered");
    let mut events = board
        .runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .expect("accept the reveal")
        .events;
    events.extend(resolve_declining(&mut board.runner));
    assert!(revealed(&events, board.card_a), "Card A was revealed");
    assert_eq!(
        indicated_actions(&board.runner),
        HashSet::from([(boarding_ref, P0)]),
        "revealing takes the indicated action"
    );

    // Leg 3: the limit is spent — the next roll does not trigger Priority Boarding.
    assert_eq!(roll(&mut board), 0, "the reveal was taken this turn");
}

/// CR 603.5 + CR 608.2d + CR 603.2h: a remembered "always accept" is not read for
/// an impossible reveal — the empty-library instance spends nothing — while a
/// later possible instance the same turn is accepted by it and spends the limit.
#[test]
fn priority_boarding_remembered_accept_does_not_spend_the_limit_on_an_empty_library() {
    let mut board = boarding_board();

    // Turn 1: remember "always accept" on a real reveal.
    assert_eq!(
        roll(&mut board),
        1,
        "reach: the roll triggers Priority Boarding"
    );
    board.runner.advance_until_stack_empty();
    assert_optional_prompt(&board.runner, P0, "the first reveal is offered");
    let mut events = board
        .runner
        .act(GameAction::DecideOptionalEffectAndRemember {
            choice: AutoMayChoice::Accept,
            scope: MayTriggerAutoChoiceScope::ExactInstance,
        })
        .expect("accept and remember")
        .events;
    events.extend(resolve_declining(&mut board.runner));
    assert!(
        revealed(&events, board.card_a),
        "reach: Card A was revealed"
    );
    assert_eq!(indicated_actions(&board.runner).len(), 1);
    draw_card_a(&mut board);

    // P1's turn: the ledger resets, and P0's library is empty.
    next_main_with_p0_priority(&mut board.runner);
    assert!(indicated_actions(&board.runner).is_empty());
    assert_eq!(
        roll(&mut board),
        1,
        "reach: the roll triggers Priority Boarding"
    );
    let events = resolve_declining(&mut board.runner);
    assert!(
        !events.iter().any(|event| matches!(
            event,
            engine::types::events::GameEvent::CardsRevealed { .. }
        )),
        "nothing is revealed from an empty library"
    );
    assert!(
        indicated_actions(&board.runner).is_empty(),
        "the remembered accept is not applied to an impossible reveal"
    );

    // A card on top: the remembered accept reveals it and spends the limit.
    return_card_a(&mut board);
    assert_eq!(
        roll(&mut board),
        1,
        "the untaken limit leaves it free to trigger"
    );
    let events = resolve_declining(&mut board.runner);
    assert!(
        revealed(&events, board.card_a),
        "the remembered accept reveals Card A"
    );
    assert_eq!(
        indicated_actions(&board.runner).len(),
        1,
        "the remembered accept takes the indicated action"
    );
}

/// CR 608.2d + CR 118.12: an optional reveal with an empty library is not offered
/// and its "If you do" rider does not happen; with a card it is offered and the
/// rider follows the accepted reveal.
#[test]
fn optional_reveal_with_an_empty_library_skips_its_if_you_do_rider() {
    let build = |library: &[&str]| {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let source = scenario
            .add_artifact_from_oracle(P0, "Reveal Engine", REVEAL_FOR_LIFE)
            .id();
        scenario.with_library_top(P0, library);
        (scenario.build(), source)
    };

    let (mut runner, source) = build(&[]);
    let life_before = runner.life(P0);
    runner.activate(source, 0).stop_at_optional().resolve();
    assert_settled_priority(&runner, "an empty-library reveal is never offered");
    assert_eq!(runner.life(P0), life_before, "the rider did not run");

    let (mut runner, source) = build(&["Card A"]);
    let life_before = runner.life(P0);
    runner.activate(source, 0).stop_at_optional().resolve();
    assert_optional_prompt(&runner, P0, "a non-empty library's reveal is offered");
    decide(&mut runner, true);
    runner.advance_until_stack_empty();
    assert_settled_priority(&runner, "after the accepted reveal");
    assert_eq!(runner.life(P0), life_before + 3, "the rider ran");
}
