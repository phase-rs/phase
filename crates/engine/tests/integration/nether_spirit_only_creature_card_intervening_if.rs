//! Runtime regression for Nether Spirit's intervening-if self-reanimation.
//!
//! Oracle (verbatim):
//!   "At the beginning of your upkeep, if this card is the only creature card in
//!    your graveyard, you may return this card to the battlefield."
//!
//! Root cause (pre-fix): the parser dropped the intervening-if entirely, leaving
//! `TriggerDefinition.condition == None` and `trigger_zones == [Battlefield]`, so
//! the trigger could never even be DETECTED while Nether Spirit sat in the
//! graveyard, and — when force-fired — never checked the "only creature card"
//! gate. The parser fix hoists the clause into
//! `And { [SourceInZone { Graveyard }, ObjectCount(creature card in your
//! graveyard) == 1] }`; the EXISTING `trigger_condition_source_zones` walker then
//! derives `trigger_zones == [Graveyard]` and the return effect's
//! `ChangeZone.origin == Graveyard` (Jocasta, Automaton Avenger precedent,
//! issue #4566), with zero runtime changes.
//!
//! CR references (verified against docs/MagicCompRules.txt):
//!   - CR 603.4: intervening-if is checked when the event occurs AND rechecked as
//!     the ability resolves; if false at resolution the ability does nothing.
//!   - CR 113.6b: an ability that states which zone it functions in functions
//!     only from that zone (the derived `SourceInZone { Graveyard }`).
//!
//! These drive the real trigger-collection + stack-resolution pipeline
//! (`process_triggers` off-zone graveyard scan + CR 603.4 recheck in
//! `stack.rs`), not a parsed-AST shape assertion.

use super::rules::{GameRunner, GameScenario, Phase, WaitingFor, P0};
use engine::game::zones::create_object;
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::{GameState, StackEntryKind};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const NETHER_SPIRIT: &str = "At the beginning of your upkeep, if this card is the only creature card in your graveyard, you may return this card to the battlefield.";

/// True iff a triggered ability sourced by `source` is currently on the stack —
/// i.e. the intervening-if held at fire time (CR 603.4 first check) and the
/// trigger was placed on the stack.
fn nether_trigger_on_stack(runner: &GameRunner, source: ObjectId) -> bool {
    runner.state().stack.iter().any(|entry| {
        matches!(
            &entry.kind,
            StackEntryKind::TriggeredAbility { source_id, .. } if *source_id == source
        )
    })
}

/// Put a bare creature card directly into `owner`'s graveyard mid-game (models a
/// mill/discard in response). Mirrors `GameScenario::add_creature_to_graveyard`,
/// which is unavailable once the scenario has been built into a `GameRunner`.
fn add_creature_card_to_graveyard(
    state: &mut GameState,
    owner: PlayerId,
    name: &str,
    power: i32,
    toughness: i32,
) -> ObjectId {
    let card_id = CardId(state.next_object_id);
    let id = create_object(state, card_id, owner, name.to_string(), Zone::Graveyard);
    let obj = state.objects.get_mut(&id).unwrap();
    obj.card_types.core_types.push(CoreType::Creature);
    obj.base_card_types = obj.card_types.clone();
    obj.power = Some(power);
    obj.toughness = Some(toughness);
    obj.base_power = Some(power);
    obj.base_toughness = Some(toughness);
    id
}

/// Position the runner at P0's upkeep with the phase-change event delivered, so
/// any beginning-of-upkeep trigger has been collected and placed on the stack.
/// Mirrors `dark_confidant_upkeep`'s proven Untap→Upkeep setup.
fn build_at_p0_upkeep(scenario: GameScenario) -> GameRunner {
    let mut runner = scenario.build();
    runner.state_mut().turn_number = 2;
    runner.state_mut().phase = Phase::Untap;
    runner.state_mut().active_player = P0;
    runner.state_mut().priority_player = P0;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P0 };
    runner.advance_to_upkeep();
    runner
}

/// Resolve Nether Spirit's optional trigger by passing priority until it
/// resolves, answering the "you may return" prompt with `accept`, and returning
/// once the stack settles.
fn resolve_optional_and_settle(runner: &mut GameRunner, accept: bool) {
    for _ in 0..60 {
        match runner.state().waiting_for.clone() {
            WaitingFor::OptionalEffectChoice { .. } => {
                runner
                    .act(GameAction::DecideOptionalEffect { accept })
                    .expect("answer Nether Spirit's may-return prompt");
            }
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() {
                    return;
                }
                runner
                    .act(GameAction::PassPriority)
                    .expect("pass priority to resolve Nether Spirit trigger");
            }
            other => panic!("unexpected prompt resolving Nether Spirit trigger: {other:?}"),
        }
    }
    panic!("Nether Spirit trigger did not settle within 60 steps");
}

fn spirit_zone(runner: &GameRunner, id: ObjectId) -> Zone {
    runner
        .state()
        .objects
        .get(&id)
        .expect("Nether Spirit object")
        .zone
}

/// CR 603.4: with two creature cards in the graveyard the intervening-if is
/// false, so the trigger must NOT fire — nothing reaches the stack.
#[test]
fn does_not_fire_with_two_creature_cards_in_graveyard() {
    let mut scenario = GameScenario::new();
    let nether = scenario
        .add_creature_to_graveyard(P0, "Nether Spirit", 2, 2)
        .from_oracle_text(NETHER_SPIRIT)
        .id();
    // A second creature card makes the count 2 → intervening-if false.
    scenario.add_creature_to_graveyard(P0, "Grizzly Bears", 2, 2);

    let runner = build_at_p0_upkeep(scenario);

    assert!(
        !nether_trigger_on_stack(&runner, nether),
        "trigger must not fire while a second creature card is in the graveyard (CR 603.4)"
    );
}

/// Positive reach-guard for the negative above: the SAME harness (minus the
/// filler creature) DOES put the trigger on the stack, proving the negative test
/// actually exercises the intervening-if rather than passing vacuously.
#[test]
fn fires_reach_guard_when_sole_creature_card() {
    let mut scenario = GameScenario::new();
    let nether = scenario
        .add_creature_to_graveyard(P0, "Nether Spirit", 2, 2)
        .from_oracle_text(NETHER_SPIRIT)
        .id();

    let runner = build_at_p0_upkeep(scenario);

    assert!(
        nether_trigger_on_stack(&runner, nether),
        "trigger must fire (reach the stack) when Nether Spirit is the sole creature card"
    );
}

/// The trigger fires and — on accept — returns Nether Spirit to the battlefield.
/// A noncreature card also sits in the graveyard, proving the gate is
/// creature-card-specific (not merely "alone in the graveyard").
#[test]
fn fires_and_returns_when_sole_creature_card() {
    let mut scenario = GameScenario::new();
    let nether = scenario
        .add_creature_to_graveyard(P0, "Nether Spirit", 2, 2)
        .from_oracle_text(NETHER_SPIRIT)
        .id();
    // A noncreature (instant) card — a graveyard card that is NOT a creature card.
    scenario.add_spell_to_graveyard(P0, "Lightning Bolt", true);

    let mut runner = build_at_p0_upkeep(scenario);
    assert!(
        nether_trigger_on_stack(&runner, nether),
        "trigger must fire: a noncreature graveyard card does not break the creature-card gate"
    );

    resolve_optional_and_settle(&mut runner, true);

    assert_eq!(
        spirit_zone(&runner, nether),
        Zone::Battlefield,
        "accepting the may-return must move Nether Spirit to the battlefield"
    );
}

/// Decline path: the "may" is respected — Nether Spirit stays in the graveyard.
#[test]
fn declining_leaves_nether_spirit_in_graveyard() {
    let mut scenario = GameScenario::new();
    let nether = scenario
        .add_creature_to_graveyard(P0, "Nether Spirit", 2, 2)
        .from_oracle_text(NETHER_SPIRIT)
        .id();

    let mut runner = build_at_p0_upkeep(scenario);
    assert!(
        nether_trigger_on_stack(&runner, nether),
        "trigger must fire so the decline path is reachable"
    );

    resolve_optional_and_settle(&mut runner, false);

    assert_eq!(
        spirit_zone(&runner, nether),
        Zone::Graveyard,
        "declining the optional trigger must leave Nether Spirit in the graveyard"
    );
}

/// CR 603.4 resolution-time recheck: the trigger fires (sole creature card at
/// upkeep), but a second creature card enters the graveyard in response. Even
/// after accepting the may-return, the recheck fails and Nether Spirit stays put.
#[test]
fn resolution_recheck_fizzles_when_second_creature_card_appears() {
    let mut scenario = GameScenario::new();
    let nether = scenario
        .add_creature_to_graveyard(P0, "Nether Spirit", 2, 2)
        .from_oracle_text(NETHER_SPIRIT)
        .id();

    let mut runner = build_at_p0_upkeep(scenario);
    assert!(
        nether_trigger_on_stack(&runner, nether),
        "trigger must fire at upkeep while Nether Spirit is the sole creature card"
    );

    // In response (trigger on the stack, unresolved), a second creature card is
    // milled/discarded into P0's graveyard — count becomes 2.
    add_creature_card_to_graveyard(runner.state_mut(), P0, "Grizzly Bears", 2, 2);

    // Accept the may-return: the CR 603.4 recheck now sees 2 creature cards and
    // removes the ability from the stack without returning Nether Spirit.
    resolve_optional_and_settle(&mut runner, true);

    assert_eq!(
        spirit_zone(&runner, nether),
        Zone::Graveyard,
        "CR 603.4 resolution recheck must fizzle the return once a second creature card is present"
    );
}

/// Two-Nether-Spirits ruling: with two copies in the graveyard, each copy sees
/// TWO creature cards, so neither copy's intervening-if holds — no trigger fires.
#[test]
fn two_copies_neither_fires() {
    let mut scenario = GameScenario::new();
    let first = scenario
        .add_creature_to_graveyard(P0, "Nether Spirit", 2, 2)
        .from_oracle_text(NETHER_SPIRIT)
        .id();
    let second = scenario
        .add_creature_to_graveyard(P0, "Nether Spirit", 2, 2)
        .from_oracle_text(NETHER_SPIRIT)
        .id();

    let runner = build_at_p0_upkeep(scenario);

    assert!(
        !nether_trigger_on_stack(&runner, first),
        "first Nether Spirit must not fire with a second creature card (its twin) present"
    );
    assert!(
        !nether_trigger_on_stack(&runner, second),
        "second Nether Spirit must not fire with a second creature card (its twin) present"
    );
}
