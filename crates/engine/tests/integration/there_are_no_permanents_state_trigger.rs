//! Surface parser near-miss: the GLOBAL-scope state trigger
//! "When there are no [type] on the battlefield, <effect>" (CR 603.8) was not
//! routed to a `StateCondition` trigger, so the whole line degraded to
//! `Effect::Unimplemented` and never fired.
//!
//! The state-trigger dispatch in `oracle_trigger.rs` already recognizes a bare
//! game-state condition after "when "/"whenever " by delegating to the single
//! condition authority `parse_inner_condition` and bridging its result with
//! `static_condition_to_trigger_condition` — but it accepted ONLY a
//! `ControlsType` result ("when you control a <filter>", Endangered Armodon).
//! The existential-emptiness form "there are no <type> on the battlefield"
//! already lowers, via that same `parse_inner_condition`, to
//! `StaticCondition::QuantityComparison { ObjectCount(<filter>) == 0 }` — this
//! is the exact predicate the intervening-`if` form ("if there are no Zombies
//! on the battlefield", Sarcomancy/Spirit Mirror) has parsed since PR #4917.
//! The bridge already maps `QuantityComparison` 1:1 to
//! `TriggerCondition::QuantityComparison`; only the trigger gate rejected it.
//!
//! Widening that one gate to also accept the bridged `QuantityComparison`
//! routes the emptiness form to a real `StateCondition` trigger. No new effect,
//! no resolver change: `check_state_triggers` fires any `StateCondition` trigger
//! whose `check_trigger_condition` predicate becomes true, and
//! `QuantityComparison` is already evaluated there (the count is taken across
//! the whole battlefield with no controller restriction, so "no creatures" /
//! "no lands" means *any* player's — CR 110.1 / CR 403.1).
//!
//! Four real cards each pair this global-emptiness state trigger with a
//! self-sacrifice:
//!   * Drop of Honey / Porphyry Nodes / Task Mage Assembly —
//!     "When there are no creatures on the battlefield, sacrifice this enchantment."
//!   * Mana Vortex — "When there are no lands on the battlefield, sacrifice this enchantment."
//!
//! Each test drives the REAL state-trigger pipeline (`check_state_triggers` →
//! stack resolution) and asserts the CR 603.8 boundary: the sacrifice fires only
//! once the last matching permanent is gone. On `main` the line lowers to
//! `Unimplemented`, so the `StateCondition` reach-guard is empty and every
//! assertion below is honestly red.
//!
//! CR references (verified against docs/MagicCompRules.txt):
//!   - CR 603.8: a state trigger fires as soon as the game state matches its
//!     condition (and not again until the condition has become false and true).
//!   - CR 110.1 / CR 403.1: the battlefield holds every player's permanents; a
//!     type count with no controller restriction spans all players.
//!   - CR 701.16a: to sacrifice a permanent is to move it to its owner's
//!     graveyard.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::zones::create_object;
use engine::types::card_type::CoreType;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::phase::Phase;
use engine::types::triggers::TriggerMode;
use engine::types::zones::Zone;
use engine::types::PlayerId;

// Drop of Honey / Porphyry Nodes / Task Mage Assembly, isolated to the
// state-trigger line (the clause parse is context-independent, so a standalone
// permanent exercises exactly the arm the printed cards reach).
const NO_CREATURES: &str =
    "When there are no creatures on the battlefield, sacrifice this enchantment.";
// Mana Vortex's third line — the SAME recognizer arm, a different permanent type.
const NO_LANDS: &str = "When there are no lands on the battlefield, sacrifice this enchantment.";

/// Turn the creature scaffold `add_creature_from_oracle` installs into a plain
/// Enchantment (the real card type of all four anchors): drop the Creature type
/// and clear P/T so the 0/0 body neither dies to CR 704.5f nor counts as a
/// creature for the emptiness check. Mirrors `mazemind_tome`'s `make_artifact`.
fn make_enchantment(runner: &mut GameRunner, id: ObjectId) {
    let obj = runner.state_mut().objects.get_mut(&id).unwrap();
    obj.card_types
        .core_types
        .retain(|t| *t != CoreType::Creature);
    obj.card_types.core_types.push(CoreType::Enchantment);
    obj.power = None;
    obj.toughness = None;
    obj.base_power = None;
    obj.base_toughness = None;
    obj.base_card_types = obj.card_types.clone();
}

/// Put a plain permanent of `core_type` onto `player`'s battlefield. Creatures
/// get a 2/2 body so CR 704.5f can't cull them mid-test (which would spuriously
/// empty the board and fire the trigger under test). Mirrors the direct
/// `create_object` battlefield fixture idiom used across the integration suite.
fn add_board_filler(
    runner: &mut GameRunner,
    player: PlayerId,
    name: &str,
    core_type: CoreType,
) -> ObjectId {
    let card_id = CardId(runner.state().next_object_id);
    let id = create_object(
        runner.state_mut(),
        card_id,
        player,
        name.to_string(),
        Zone::Battlefield,
    );
    let obj = runner.state_mut().objects.get_mut(&id).unwrap();
    obj.card_types.core_types.push(core_type);
    if core_type == CoreType::Creature {
        obj.power = Some(2);
        obj.toughness = Some(2);
        obj.base_power = Some(2);
        obj.base_toughness = Some(2);
    }
    obj.base_card_types = obj.card_types.clone();
    id
}

/// Build a lone Enchantment carrying `oracle`'s emptiness state trigger, place
/// `filler` (if any) on the board, then run the engine's real state-trigger scan
/// (the same call the priority pipeline makes) and drain the stack. Returns the
/// runner and the enchantment's id.
fn run_case(oracle: &str, filler: Option<(PlayerId, CoreType)>) -> (GameRunner, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let ench = scenario
        .add_creature_from_oracle(P0, "Emptiness Nodes Test", 0, 0, oracle)
        .id();
    let mut runner = scenario.build();
    make_enchantment(&mut runner, ench);

    if let Some((player, core_type)) = filler {
        add_board_filler(&mut runner, player, "Board Filler", core_type);
    }

    // POSITIVE reach-guard: the emptiness line parsed to a `StateCondition`
    // trigger (not `Unimplemented`). On `main` this is empty, so the whole test
    // is honestly red and no assertion below can pass vacuously.
    assert!(
        runner.state().objects[&ench]
            .trigger_definitions
            .iter_unchecked()
            .any(|entry| entry.definition.mode == TriggerMode::StateCondition),
        "the \"{oracle}\" line must parse to a StateCondition trigger; got modes {:?}",
        runner.state().objects[&ench]
            .trigger_definitions
            .iter_unchecked()
            .map(|entry| entry.definition.mode.clone())
            .collect::<Vec<_>>()
    );

    // CR 603.8 + CR 117.1: state triggers are checked whenever a player would
    // receive priority. Invoke the same scan the priority pipeline runs, then
    // resolve whatever it put on the stack.
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P0 };
    engine::game::triggers::check_state_triggers(runner.state_mut());
    runner.advance_until_stack_empty();
    (runner, ench)
}

fn zone(runner: &GameRunner, id: ObjectId) -> Zone {
    runner.state().objects[&id].zone
}

/// POSITIVE (creatures): with no creatures anywhere on the battlefield the state
/// trigger fires and the enchantment sacrifices itself (CR 603.8 + CR 701.16a).
#[test]
fn no_creatures_fires_and_sacrifices_the_enchantment() {
    let (runner, ench) = run_case(NO_CREATURES, None);
    assert_eq!(
        zone(&runner, ench),
        Zone::Graveyard,
        "with no creatures on the battlefield the trigger must fire and move the \
         enchantment to its owner's graveyard"
    );
}

/// NEGATIVE discriminator (creatures): while a creature is on the battlefield the
/// emptiness condition is false, so the present-but-unmet state trigger does NOT
/// fire and the enchantment stays put. The reach-guard proves the trigger exists,
/// so this is a genuine 1-vs-0 boundary, not a vacuous "never parsed" pass.
#[test]
fn no_creatures_does_not_fire_while_a_creature_is_present() {
    let (runner, ench) = run_case(NO_CREATURES, Some((P0, CoreType::Creature)));
    assert_eq!(
        zone(&runner, ench),
        Zone::Battlefield,
        "while a creature is on the battlefield the trigger must not fire"
    );
}

/// Global scope discriminator: an opponent's creature also keeps the condition
/// false. This rejects a controller-scoped lowering of "there are no creatures
/// on the battlefield."
#[test]
fn no_creatures_does_not_fire_while_an_opponent_creature_is_present() {
    let (runner, ench) = run_case(NO_CREATURES, Some((P1, CoreType::Creature)));
    assert_eq!(
        zone(&runner, ench),
        Zone::Battlefield,
        "an opponent's creature is still on the battlefield and must prevent the trigger"
    );
}

/// POSITIVE (lands): the same recognizer arm, keyed on a different permanent
/// type — with no lands on the battlefield the enchantment sacrifices itself.
#[test]
fn no_lands_fires_and_sacrifices_the_enchantment() {
    let (runner, ench) = run_case(NO_LANDS, None);
    assert_eq!(
        zone(&runner, ench),
        Zone::Graveyard,
        "with no lands on the battlefield the trigger must fire and move the \
         enchantment to its owner's graveyard"
    );
}

/// NEGATIVE discriminator (lands): a land on the battlefield keeps the emptiness
/// condition false, so the trigger does not fire.
#[test]
fn no_lands_does_not_fire_while_a_land_is_present() {
    let (runner, ench) = run_case(NO_LANDS, Some((P0, CoreType::Land)));
    assert_eq!(
        zone(&runner, ench),
        Zone::Battlefield,
        "while a land is on the battlefield the trigger must not fire"
    );
}
