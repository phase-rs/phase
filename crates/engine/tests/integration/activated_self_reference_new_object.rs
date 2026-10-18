//! An activated ability whose source leaves the battlefield and returns before
//! the ability resolves must not affect the returned object.
//!
//! CR 400.7: an object that moves from one zone to another becomes a new object
//! with no memory of, or relation to, its previous existence. An activated
//! ability's "this creature" / "~" names the object that activated it; once
//! that object is gone, the instruction has no referent and does nothing
//! (CR 400.7; CR 609.3 — an effect does only as much as it can).
//!
//! Field report: Carrion Feeder ("Sacrifice a creature: Put a +1/+1 counter on
//! this creature") sacrificed itself to pay for its own ability while under
//! Supernatural Stamina. The granted dies trigger returned it before the
//! ability resolved, and the returned Feeder got the counter.
//!
//! Two layers carried the defect:
//! - `ResolvedAbility::self_ref_is_current` answered `true` for every
//!   non-triggered ability whose captured incarnation was stale, so the generic
//!   `SelfRef` path (`targeting::resolved_object_ids_for_filter`) bound the new
//!   object — a creature blinked in response to its own pump got the pump.
//! - Effect resolvers that short-circuit `SelfRef` to `ability.source_id`
//!   (counters, tap/untap, regenerate, animate, attach, …) never asked at all.
//!   They now go through `ResolvedAbility::self_ref_binding`.
//!
//! Keyword actions keep their own rules for a departed performer: a fight does
//! not happen (CR 701.14b), a connive uses last known information — the draw
//! and discard happen, the counter has nowhere to go (CR 701.50b) — and endure
//! still offers its Spirit token (CR 701.63a).
//!
//! Each case below pairs the leave-and-return run with a control where the
//! source stays put, so a negative assertion cannot pass vacuously.

use engine::game::game_object::AttachTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{
    AbilityCost, AbilityDefinition, AbilityKind, ContinuousModification, Duration, Effect,
    EffectKind, GrantedAbilityScope, QuantityExpr, QuantityRef, ResolvedAbility, TargetFilter,
    TargetRef,
};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;
use engine::types::game_state::{StackEntryKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const CARRION_FEEDER: &str =
    "This creature can't block.\nSacrifice a creature: Put a +1/+1 counter on this creature.";

const SUPERNATURAL_STAMINA: &str = "Until end of turn, target creature gets +2/+0 and gains \
\"When this creature dies, return it to the battlefield tapped under its owner's control.\"";

/// Black Carriage, minus its upkeep-only restriction so the scenario can run in
/// the main phase.
const BLACK_CARRIAGE: &str = "Sacrifice a creature: Untap this creature.";

const PUMPER: &str = "{0}: This creature gets +1/+1 until end of turn.";
const SELF_BOUNCER: &str = "{0}: Return this creature to its owner's hand.";
const REGENERATOR: &str = "{0}: Regenerate this creature.";
const MANLAND: &str = "{0}: This land becomes a 3/3 creature until end of turn. It's still a land.";
const EQUIPMENT: &str = "Equipped creature gets +1/+1.\nEquip {0}";
/// Gideon Jura / Gideon, Ally of Zendikar's shape: an untargeted shield whose
/// recipient is the source itself.
const SELF_PROTECTOR: &str = "{0}: Prevent all damage that would be dealt to him this turn.";
const BOLT: &str = "Lightning Bolt deals 3 damage to any target.";
const FIGHTER: &str = "{0}: This creature fights target creature you don't control.";
const CONNIVER: &str = "{0}: This creature connives.";
/// Krumar Initiate's shape: an activated "this creature endures N".
const ENDURER: &str = "{0}: This creature endures 2.";

/// A flicker that reaches any permanent (Cloudshift reaches only creatures).
const FLICKER: &str = "Exile target permanent you control, then return that card to the \
battlefield under its owner's control.";

fn black() -> Vec<ManaUnit> {
    vec![ManaUnit::new(ManaType::Black, ObjectId(0), false, vec![])]
}

fn costed_ability(runner: &GameRunner, source: ObjectId) -> usize {
    runner.state().objects[&source]
        .abilities
        .iter()
        .position(|a| a.cost.is_some())
        .expect("the source has a costed activated ability")
}

fn p1p1(runner: &GameRunner, id: ObjectId) -> u32 {
    runner.state().objects[&id]
        .counters
        .get(&CounterType::Plus1Plus1)
        .copied()
        .unwrap_or(0)
}

/// Put `source`'s ability `index` on the stack (with `target`, if it has one)
/// and stop there, so the test can respond before it resolves.
fn activate_onto_stack(
    runner: &mut GameRunner,
    source: ObjectId,
    index: usize,
    target: Option<ObjectId>,
) {
    runner
        .act(GameAction::ActivateAbility {
            source_id: source,
            ability_index: index,
        })
        .expect("activation is accepted");
    for _ in 0..8 {
        match &runner.state().waiting_for {
            WaitingFor::TargetSelection { .. } => {
                let target = target.expect("this activation asks for a target");
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(TargetRef::Object(target)),
                    })
                    .expect("the target is accepted");
            }
            WaitingFor::Priority { .. } => break,
            other => panic!("unexpected prompt while activating: {other:?}"),
        }
    }
    assert!(
        !runner.state().stack.is_empty(),
        "the ability is on the stack, not resolved"
    );
}

/// Build a scenario with one permanent from `oracle` (and a flicker in hand),
/// activate its ability, optionally flicker the permanent in response, and
/// resolve everything. Returns the runner and the permanent's id.
fn activate_and_maybe_flicker(
    add: impl FnOnce(&mut GameScenario) -> ObjectId,
    target: Option<fn(&mut GameScenario) -> ObjectId>,
    flicker: bool,
) -> (GameRunner, ObjectId, Option<ObjectId>) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = add(&mut scenario);
    let target_id = target.map(|t| t(&mut scenario));
    let flicker_spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Flicker", true, FLICKER)
        .id();
    let mut runner = scenario.build();
    let index = if target_id.is_some() {
        runner.state().objects[&source]
            .abilities
            .iter()
            .position(|a| a.ability_tag.is_some())
            .expect("an equip ability")
    } else {
        costed_ability(&runner, source)
    };
    activate_onto_stack(&mut runner, source, index, target_id);
    if flicker {
        let incarnation = runner.state().objects[&source].incarnation;
        let _ = runner
            .cast(flicker_spell)
            .target_objects(&[source])
            .commit();
        runner.resolve_top();
        let returned = &runner.state().objects[&source];
        assert_eq!(
            returned.zone,
            Zone::Battlefield,
            "reach: the flicker returned it"
        );
        assert_ne!(
            returned.incarnation, incarnation,
            "reach: the returned permanent is a new object (CR 400.7)"
        );
    }
    runner.advance_until_stack_empty();
    (runner, source, target_id)
}

// ---------------------------------------------------------------------------
// The field report: sacrificed to pay its own cost, returned by a dies trigger.
// ---------------------------------------------------------------------------

fn feeder_under_stamina(sacrifice_itself: bool) -> (GameRunner, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, black());
    let feeder = scenario
        .add_creature_from_oracle(P0, "Carrion Feeder", 1, 1, CARRION_FEEDER)
        .id();
    let fodder = scenario.add_creature(P0, "Fodder", 1, 1).id();
    let stamina = scenario
        .add_spell_to_hand_from_oracle(P0, "Supernatural Stamina", true, SUPERNATURAL_STAMINA)
        .id();
    let mut runner = scenario.build();
    runner.cast(stamina).target_objects(&[feeder]).resolve();
    let index = costed_ability(&runner, feeder);
    let paid = if sacrifice_itself { feeder } else { fodder };
    runner.activate(feeder, index).pay_with(&[paid]).resolve();
    runner.advance_until_stack_empty();
    (runner, feeder)
}

#[test]
fn feeder_sacrificing_another_creature_gets_the_counter() {
    let (runner, feeder) = feeder_under_stamina(false);
    assert_eq!(p1p1(&runner, feeder), 1);
}

#[test]
fn feeder_sacrificed_to_itself_and_returned_gets_no_counter() {
    let (runner, feeder) = feeder_under_stamina(true);
    assert_eq!(
        runner.state().objects[&feeder].zone,
        Zone::Battlefield,
        "reach: Supernatural Stamina's granted dies trigger returned the Feeder"
    );
    assert_eq!(
        p1p1(&runner, feeder),
        0,
        "the returned Feeder is a new object: the counter has no referent"
    );
}

fn carriage_under_stamina(sacrifice_itself: bool) -> (GameRunner, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, black());
    let carriage = scenario
        .add_creature_from_oracle(P0, "Black Carriage", 4, 4, BLACK_CARRIAGE)
        .id();
    let fodder = scenario.add_creature(P0, "Fodder", 1, 1).id();
    let stamina = scenario
        .add_spell_to_hand_from_oracle(P0, "Supernatural Stamina", true, SUPERNATURAL_STAMINA)
        .id();
    let mut runner = scenario.build();
    runner.cast(stamina).target_objects(&[carriage]).resolve();
    runner
        .state_mut()
        .objects
        .get_mut(&carriage)
        .unwrap()
        .tapped = true;
    let index = costed_ability(&runner, carriage);
    let paid = if sacrifice_itself { carriage } else { fodder };
    runner.activate(carriage, index).pay_with(&[paid]).resolve();
    runner.advance_until_stack_empty();
    (runner, carriage)
}

#[test]
fn carriage_sacrificing_another_creature_untaps() {
    let (runner, carriage) = carriage_under_stamina(false);
    assert!(!runner.state().objects[&carriage].tapped);
}

/// CR 400.7 + CR 701.26b: Supernatural Stamina returns it TAPPED; the untap
/// belonged to the old object.
#[test]
fn carriage_sacrificed_to_itself_and_returned_stays_tapped() {
    let (runner, carriage) = carriage_under_stamina(true);
    assert_eq!(runner.state().objects[&carriage].zone, Zone::Battlefield);
    assert!(
        runner.state().objects[&carriage].tapped,
        "the returned Carriage entered tapped and is a new object"
    );
}

// ---------------------------------------------------------------------------
// Flickered in response: the generic `SelfRef` path (`self_ref_is_current`).
// ---------------------------------------------------------------------------

fn pumper(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_creature_from_oracle(P0, "Pumper", 2, 2, PUMPER)
        .id()
}

#[test]
fn pump_reaches_its_source() {
    let (runner, source, _) = activate_and_maybe_flicker(pumper, None, false);
    assert_eq!(runner.state().objects[&source].power, Some(3));
}

#[test]
fn pump_does_not_reach_a_flickered_source() {
    let (runner, source, _) = activate_and_maybe_flicker(pumper, None, true);
    assert_eq!(
        runner.state().objects[&source].power,
        Some(2),
        "the returned creature is a new object: the pump has no referent"
    );
}

fn self_bouncer(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_creature_from_oracle(P0, "Self Bouncer", 2, 2, SELF_BOUNCER)
        .id()
}

#[test]
fn self_bounce_returns_its_source() {
    let (runner, source, _) = activate_and_maybe_flicker(self_bouncer, None, false);
    assert_eq!(runner.state().objects[&source].zone, Zone::Hand);
}

#[test]
fn self_bounce_does_not_return_a_flickered_source() {
    let (runner, source, _) = activate_and_maybe_flicker(self_bouncer, None, true);
    assert_eq!(
        runner.state().objects[&source].zone,
        Zone::Battlefield,
        "the returned creature is a new object and stays"
    );
}

// ---------------------------------------------------------------------------
// Flickered in response: resolvers that bind `SelfRef` themselves.
// ---------------------------------------------------------------------------

fn regenerator(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_creature_from_oracle(P0, "Regenerator", 2, 2, REGENERATOR)
        .id()
}

fn has_regeneration_shield(runner: &GameRunner, id: ObjectId) -> bool {
    runner.state().objects[&id]
        .replacement_definitions
        .as_slice()
        .iter()
        .any(|r| r.shield_kind.is_shield())
}

#[test]
fn regenerate_shields_its_source() {
    let (runner, source, _) = activate_and_maybe_flicker(regenerator, None, false);
    assert!(has_regeneration_shield(&runner, source));
}

#[test]
fn regenerate_does_not_shield_a_flickered_source() {
    let (runner, source, _) = activate_and_maybe_flicker(regenerator, None, true);
    assert!(
        !has_regeneration_shield(&runner, source),
        "the returned creature is a new object: no shield"
    );
}

fn manland(scenario: &mut GameScenario) -> ObjectId {
    scenario.add_land_from_oracle(P0, "Manland", MANLAND).id()
}

fn is_creature(runner: &GameRunner, id: ObjectId) -> bool {
    runner.state().objects[&id]
        .card_types
        .core_types
        .contains(&CoreType::Creature)
}

#[test]
fn animate_animates_its_source() {
    let (runner, source, _) = activate_and_maybe_flicker(manland, None, false);
    assert!(is_creature(&runner, source));
}

#[test]
fn animate_does_not_animate_a_flickered_source() {
    let (runner, source, _) = activate_and_maybe_flicker(manland, None, true);
    assert!(
        !is_creature(&runner, source),
        "the returned land is a new object and stays a land"
    );
}

fn equipment(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_artifact_from_oracle(P0, "Blade", EQUIPMENT)
        .with_subtypes(vec!["Equipment"])
        .id()
}

fn bear(scenario: &mut GameScenario) -> ObjectId {
    scenario.add_creature(P0, "Bear", 2, 2).id()
}

#[test]
fn equip_attaches_its_source() {
    let (runner, source, bear) = activate_and_maybe_flicker(equipment, Some(bear), false);
    assert_eq!(
        runner.state().objects[&source].attached_to,
        Some(AttachTarget::Object(bear.unwrap()))
    );
}

/// CR 400.7 + CR 702.6a: the Equipment that returned is a new object; the equip
/// ability of the old one attaches nothing.
#[test]
fn equip_does_not_attach_a_flickered_source() {
    let (runner, source, _) = activate_and_maybe_flicker(equipment, Some(bear), true);
    assert_eq!(
        runner.state().objects[&source].attached_to,
        None,
        "the returned Equipment is a new object and stays unattached"
    );
}

// ---------------------------------------------------------------------------
// A self-scoped prevention shield latches its host at resolution.
// ---------------------------------------------------------------------------

/// Activate the self-protection (flickering the creature in response when
/// asked), then Lightning Bolt it. Returns whether it survived.
fn survives_bolt_after_self_protection(flicker: bool) -> bool {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(ManaType::Red, ObjectId(0), false, vec![])],
    );
    let protector = scenario
        .add_creature_from_oracle(P0, "Self Protector", 2, 2, SELF_PROTECTOR)
        .id();
    let flicker_spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Flicker", true, FLICKER)
        .id();
    let bolt = scenario
        .add_spell_to_hand_from_oracle(P0, "Lightning Bolt", true, BOLT)
        .id();
    let mut runner = scenario.build();
    let index = costed_ability(&runner, protector);
    activate_onto_stack(&mut runner, protector, index, None);
    if flicker {
        let _ = runner
            .cast(flicker_spell)
            .target_objects(&[protector])
            .commit();
        runner.resolve_top();
    }
    runner.advance_until_stack_empty();
    runner.cast(bolt).target_objects(&[protector]).resolve();
    runner.advance_until_stack_empty();
    runner.state().objects[&protector].zone == Zone::Battlefield
}

#[test]
fn self_prevention_protects_its_source() {
    assert!(
        survives_bolt_after_self_protection(false),
        "the shield prevents the Bolt"
    );
}

/// CR 400.7 + CR 615: the shield named the departed object; the returned
/// creature is unprotected.
#[test]
fn self_prevention_does_not_protect_a_flickered_source() {
    assert!(
        !survives_bolt_after_self_protection(true),
        "the returned creature is a new object: the Bolt kills it"
    );
}

// ---------------------------------------------------------------------------
// Keyword actions performed by "~": fight (no fight) and connive (LKI).
// ---------------------------------------------------------------------------

fn fight_after_activation(flicker: bool) -> u32 {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let fighter = scenario
        .add_creature_from_oracle(P0, "Fighter", 3, 3, FIGHTER)
        .id();
    let victim = scenario.add_creature(P1, "Victim", 1, 5).id();
    let flicker_spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Flicker", true, FLICKER)
        .id();
    let mut runner = scenario.build();
    let index = costed_ability(&runner, fighter);
    activate_onto_stack(&mut runner, fighter, index, Some(victim));
    if flicker {
        let _ = runner
            .cast(flicker_spell)
            .target_objects(&[fighter])
            .commit();
        runner.resolve_top();
    }
    runner.advance_until_stack_empty();
    runner.state().objects[&victim].damage_marked
}

#[test]
fn fight_is_fought_by_its_source() {
    assert_eq!(fight_after_activation(false), 3);
}

/// CR 701.14b + CR 400.7: the fighter left the battlefield; the returned
/// creature is a new object, so neither creature fights.
#[test]
fn fight_does_not_happen_after_its_source_is_flickered() {
    assert_eq!(
        fight_after_activation(true),
        0,
        "the returned creature is not the fighter"
    );
}

/// Returns (+1/+1 counters on the source, cards in its owner's graveyard).
fn connive_after_activation(flicker: bool) -> (u32, usize) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let conniver = scenario
        .add_creature_from_oracle(P0, "Conniver", 2, 2, CONNIVER)
        .id();
    // One nonland card to draw and an otherwise empty hand, so the connive's
    // draw-then-discard completes without a discard prompt.
    scenario.with_library_top(P0, &["Lib A"]);
    let flicker_spell = flicker.then(|| {
        scenario
            .add_spell_to_hand_from_oracle(P0, "Flicker", true, FLICKER)
            .id()
    });
    let mut runner = scenario.build();
    let index = costed_ability(&runner, conniver);
    activate_onto_stack(&mut runner, conniver, index, None);
    if let Some(flicker_spell) = flicker_spell {
        let _ = runner
            .cast(flicker_spell)
            .target_objects(&[conniver])
            .commit();
        runner.resolve_top();
    }
    runner.advance_until_stack_empty();
    let graveyard = runner
        .state()
        .objects
        .values()
        .filter(|o| o.zone == Zone::Graveyard && o.owner == P0 && o.name == "Lib A")
        .count();
    (p1p1(&runner, conniver), graveyard)
}

#[test]
fn connive_by_its_source_draws_discards_and_counts() {
    let (counters, graveyard) = connive_after_activation(false);
    assert_eq!(graveyard, 1, "a nonland card was discarded");
    assert_eq!(counters, 1);
}

/// CR 701.50b + CR 400.7: the connive still happens with the departed
/// object's last known information — its controller draws and discards — but
/// the returned creature is a new object and gets no counter.
#[test]
fn connive_after_its_source_is_flickered_uses_last_known_information() {
    let (counters, graveyard) = connive_after_activation(true);
    assert_eq!(graveyard, 1, "the draw and discard still happen");
    assert_eq!(counters, 0, "the returned creature is not the conniver");
}

/// Threaten's shape: a control change that outlives the stack.
const BORROW: &str = "Gain control of target creature until end of turn.";

fn graveyard_count(
    runner: &GameRunner,
    owner: engine::types::player::PlayerId,
    name: &str,
) -> usize {
    runner
        .state()
        .objects
        .values()
        .filter(|o| o.zone == Zone::Graveyard && o.owner == owner && o.name == name)
        .count()
}

fn hand_size(runner: &GameRunner, player: engine::types::player::PlayerId) -> usize {
    runner
        .state()
        .players
        .iter()
        .find(|p| p.id == player)
        .expect("the player is seated")
        .hand
        .len()
}

/// Pass priority (with the stack non-empty) until `player` holds it, so the
/// next `cast` is theirs.
fn give_priority_to(runner: &mut GameRunner, player: engine::types::player::PlayerId) {
    for _ in 0..runner.state().players.len() {
        if matches!(runner.state().waiting_for, WaitingFor::Priority { player: p } if p == player) {
            return;
        }
        let depth = runner.state().stack.len();
        runner
            .act(GameAction::PassPriority)
            .expect("passing priority is accepted");
        assert_eq!(
            runner.state().stack.len(),
            depth,
            "one pass hands priority over without resolving the stack"
        );
    }
    panic!(
        "priority never reached {player:?}: {:?}",
        runner.state().waiting_for
    );
}

/// CR 701.50b + CR 400.7: "who controlled it" is the DEPARTED permanent's
/// last controller — not its owner, and not the controller of the permanent
/// that returned. P0 controls a P1-owned conniver, activates it, and the
/// flicker returns it under its owner's control before the ability resolves:
/// P0 draws and discards, P1's library is untouched, the returned creature
/// gets no counter.
#[test]
fn connive_after_flicker_under_owners_control_is_performed_by_the_last_controller() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let conniver = scenario
        .add_creature_from_oracle(P1, "Conniver", 2, 2, CONNIVER)
        .controlled_by(P0)
        .id();
    scenario.with_library_top(P0, &["P0 Lib"]);
    scenario.with_library_top(P1, &["P1 Lib"]);
    let flicker_spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Flicker", true, FLICKER)
        .id();
    let mut runner = scenario.build();
    assert_eq!(runner.state().objects[&conniver].controller, P0);

    let index = costed_ability(&runner, conniver);
    activate_onto_stack(&mut runner, conniver, index, None);
    let _ = runner
        .cast(flicker_spell)
        .target_objects(&[conniver])
        .commit();
    runner.resolve_top();
    assert_eq!(
        runner.state().objects[&conniver].controller,
        P1,
        "the flicker returned the creature under its owner's control"
    );

    runner.advance_until_stack_empty();
    assert_eq!(
        graveyard_count(&runner, P0, "P0 Lib"),
        1,
        "the departed conniver's last controller drew and discarded"
    );
    assert_eq!(hand_size(&runner, P0), 0);
    assert_eq!(
        graveyard_count(&runner, P1, "P1 Lib"),
        0,
        "the owner did not connive"
    );
    assert_eq!(hand_size(&runner, P1), 0, "the owner did not draw");
    assert_eq!(
        p1p1(&runner, conniver),
        0,
        "the returned creature is not the conniver"
    );
}

/// CR 701.50b: the last controller is read at DEPARTURE, so it is not the
/// ability's controller either. P0 activates a P1-owned conniver it controls;
/// in response P1 takes it back (Threaten's shape) and then flickers it. The
/// permanent left under P1's control, so P1 draws and discards; the
/// activator P0 does not.
#[test]
fn connive_after_control_change_and_flicker_is_performed_by_the_last_controller_not_the_activator()
{
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let conniver = scenario
        .add_creature_from_oracle(P1, "Conniver", 2, 2, CONNIVER)
        .controlled_by(P0)
        .id();
    scenario.with_library_top(P0, &["P0 Lib"]);
    scenario.with_library_top(P1, &["P1 Lib"]);
    let borrow = scenario
        .add_spell_to_hand_from_oracle(P1, "Borrow", true, BORROW)
        .id();
    let flicker_spell = scenario
        .add_spell_to_hand_from_oracle(P1, "Flicker", true, FLICKER)
        .id();
    let mut runner = scenario.build();

    let index = costed_ability(&runner, conniver);
    activate_onto_stack(&mut runner, conniver, index, None);
    let ability_controller = runner.state().stack.last().map(|entry| entry.controller);
    assert_eq!(ability_controller, Some(P0), "P0 activated the connive");

    give_priority_to(&mut runner, P1);
    let _ = runner.cast(borrow).target_objects(&[conniver]).commit();
    runner.resolve_top();
    assert_eq!(
        runner.state().objects[&conniver].controller,
        P1,
        "P1 took control before the conniver left"
    );

    give_priority_to(&mut runner, P1);
    let _ = runner
        .cast(flicker_spell)
        .target_objects(&[conniver])
        .commit();
    runner.resolve_top();
    assert_eq!(runner.state().objects[&conniver].controller, P1);

    runner.advance_until_stack_empty();
    assert_eq!(
        graveyard_count(&runner, P1, "P1 Lib"),
        1,
        "the departed conniver's last controller (P1) drew and discarded"
    );
    assert_eq!(hand_size(&runner, P1), 0);
    assert_eq!(
        graveyard_count(&runner, P0, "P0 Lib"),
        0,
        "the ability's controller did not connive"
    );
    assert_eq!(
        hand_size(&runner, P0),
        0,
        "the ability's controller did not draw"
    );
    assert_eq!(
        p1p1(&runner, conniver),
        0,
        "the returned creature is not the conniver"
    );
}

/// Activate the endure (flickering the creature in response when asked) and
/// stop at its branch choice. Returns the runner, the source and the branches'
/// descriptions.
fn endure_choice(flicker: bool) -> (GameRunner, ObjectId, Vec<String>) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let endurer = scenario
        .add_creature_from_oracle(P0, "Endurer", 2, 2, ENDURER)
        .id();
    let flicker_spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Flicker", true, FLICKER)
        .id();
    let mut runner = scenario.build();
    let index = costed_ability(&runner, endurer);
    activate_onto_stack(&mut runner, endurer, index, None);
    if flicker {
        let _ = runner
            .cast(flicker_spell)
            .target_objects(&[endurer])
            .commit();
        runner.resolve_top();
    }
    for _ in 0..8 {
        if let WaitingFor::ChooseOneOfBranch { branches, .. } = &runner.state().waiting_for {
            let descriptions = branches
                .iter()
                .map(|b| b.description.clone().unwrap_or_default())
                .collect();
            return (runner, endurer, descriptions);
        }
        runner
            .act(GameAction::PassPriority)
            .expect("priority passes until the endure choice");
    }
    panic!("the endure never asked for its branch");
}

fn spirit_tokens(runner: &GameRunner) -> usize {
    runner
        .state()
        .objects
        .values()
        .filter(|o| o.zone == Zone::Battlefield && o.is_token)
        .count()
}

#[test]
fn endure_offers_counters_on_its_source() {
    let (mut runner, endurer, branches) = endure_choice(false);
    let counters = branches
        .iter()
        .position(|d| d.contains("counters"))
        .expect("the counters branch is offered");
    assert_eq!(branches.len(), 2);
    runner
        .act(GameAction::ChooseBranch { index: counters })
        .expect("the counters branch is chosen");
    runner.advance_until_stack_empty();
    assert_eq!(p1p1(&runner, endurer), 2);
}

/// CR 701.63a + CR 400.7: the enduring permanent is gone — the returned
/// creature is a new object — so only the Spirit token is left to choose.
#[test]
fn endure_after_its_source_is_flickered_only_creates_the_token() {
    let (mut runner, endurer, branches) = endure_choice(true);
    assert_eq!(
        branches.len(),
        1,
        "no counters branch for a departed permanent: {branches:?}"
    );
    runner
        .act(GameAction::ChooseBranch { index: 0 })
        .expect("the token branch is chosen");
    runner.advance_until_stack_empty();
    assert_eq!(spirit_tokens(&runner), 1, "the Spirit token is created");
    assert_eq!(
        p1p1(&runner, endurer),
        0,
        "the returned creature gets no counters"
    );
}

// Verbatim printed cards, with their real costs, for the announced-X and
// controller-provenance cases. The older ENDURER remains a fixed-amount fixture.
const KRUMAR_INITIATE: &str = "{X}{B}, {T}, Pay X life: This creature endures X. Activate only as a sorcery. (Put X +1/+1 counters on it or create an X/X white Spirit creature token.)";
const FLICKER_OF_FATE: &str = "Exile target creature or enchantment, then return it to the battlefield under its owner's control.";
const TURN_AGAINST: &str = "Devoid (This card has no color.)\nGain control of target creature until end of turn. Untap that creature. It gains haste until end of turn.";
const UNSUMMON: &str = "Return target creature to its owner's hand.";

fn krumar_scenario(owner: PlayerId) -> (GameScenario, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_creature_from_oracle(owner, "Krumar Initiate", 2, 2, KRUMAR_INITIATE)
        .with_subtypes(vec!["Human", "Cleric"])
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Black],
            generic: 1,
        })
        .controlled_by(P0)
        .id();
    scenario.with_mana_pool(
        P0,
        [
            ManaType::Black,
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::White,
            ManaType::Colorless,
            ManaType::White,
            ManaType::Colorless,
            ManaType::Blue,
        ]
        .into_iter()
        .map(|kind| ManaUnit::new(kind, ObjectId(0), false, vec![]))
        .collect(),
    );
    scenario.with_mana_pool(
        P1,
        [
            ManaType::Red,
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Colorless,
        ]
        .into_iter()
        .map(|kind| ManaUnit::new(kind, ObjectId(0), false, vec![]))
        .collect(),
    );
    (scenario, source)
}

fn costed_flicker_of_fate(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_spell_to_hand_from_oracle(P0, "Flicker of Fate", true, FLICKER_OF_FATE)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::White],
            generic: 1,
        })
        .id()
}

fn costed_turn_against(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_spell_to_hand_from_oracle(P1, "Turn Against", true, TURN_AGAINST)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Red],
            generic: 4,
        })
        // Apply the keyword-aware face last: with_mana_cost derives colors.
        .from_oracle_text_with_keywords(&["Devoid"], TURN_AGAINST)
        .id()
}

// CR 107.3a + CR 602.2b + CR 119.4: announce X through the production action;
// mana, tap and life must all be paid before the captured ability is readable.
fn activate_krumar_onto_stack(
    runner: &mut GameRunner,
    source: ObjectId,
    x: u32,
) -> ResolvedAbility {
    let index = costed_ability(runner, source);
    assert!(
        matches!(runner.state().objects[&source].abilities[index].effect.as_ref(), Effect::Endure {
        amount: QuantityExpr::Ref { qty: QuantityRef::Variable { name } }, subject: TargetFilter::SelfRef,
    } if name == "X")
    );
    let life_before = runner.state().players[0].life;
    let mana_before = runner.state().players[0].mana_pool.total();
    runner
        .act(GameAction::ActivateAbility {
            source_id: source,
            ability_index: index,
        })
        .unwrap();
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ChooseXValue { player: P0, .. }
    ));
    let result = runner.act(GameAction::ChooseX { value: x }).unwrap();
    assert!(result
        .events
        .iter()
        .any(|event| matches!(event, GameEvent::XValueChosen {
        player: P0, object_id, value,
    } if *object_id == source && *value == x)));
    for _ in 0..8 {
        match runner.state().waiting_for {
            WaitingFor::Priority { .. } => break,
            WaitingFor::ManaPayment { .. } => {
                runner.act(GameAction::PassPriority).unwrap();
            }
            ref other => panic!("unexpected Krumar payment prompt: {other:?}"),
        }
    }
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    assert_eq!(runner.state().stack.len(), 1);
    let ability = match &runner.state().stack.last().unwrap().kind {
        StackEntryKind::ActivatedAbility { ability, .. } => ability.as_ref().clone(),
        other => panic!("expected committed activation, got {other:?}"),
    };
    assert_eq!(ability.source_id, source);
    assert_eq!(
        ability.source_incarnation,
        Some(runner.state().objects[&source].incarnation)
    );
    assert_eq!(ability.controller, P0);
    let mut current = Some(&ability);
    while let Some(def) = current {
        assert_eq!(def.chosen_x, Some(x));
        current = def.sub_ability.as_deref();
    }
    assert!(runner.state().objects[&source].tapped);
    assert_eq!(runner.state().players[0].life, life_before - x as i32);
    assert_eq!(runner.state().players[1].life, 20);
    assert_eq!(
        runner.state().players[0].mana_pool.total(),
        mana_before - (x as usize + 1)
    );
    assert_eq!(
        runner.state().players[0]
            .mana_pool
            .count_color(ManaType::Black),
        0
    );
    ability
}

// CR 701.63a: inspect both authorities stored by the existing choice machine,
// then submit its typed branch through the real reducer.
fn choose_krumar_endure(
    runner: &mut GameRunner,
    chooser: PlayerId,
    departed: bool,
    counters: bool,
) {
    for _ in 0..8 {
        if matches!(
            runner.state().waiting_for,
            WaitingFor::ChooseOneOfBranch { .. }
        ) {
            break;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("Endure reaches its choice");
    }
    let index = match &runner.state().waiting_for {
        WaitingFor::ChooseOneOfBranch {
            player,
            controller,
            branches,
            ..
        } => {
            assert_eq!(*player, chooser);
            assert_eq!(*controller, chooser);
            assert_eq!(branches.len(), if departed { 1 } else { 2 });
            assert!(branches
                .iter()
                .any(|b| matches!(b.effect.as_ref(), Effect::Token { .. })));
            assert_eq!(
                branches
                    .iter()
                    .any(|b| matches!(b.effect.as_ref(), Effect::PutCounter { .. })),
                !departed
            );
            branches
                .iter()
                .position(|b| {
                    if counters {
                        matches!(b.effect.as_ref(), Effect::PutCounter { .. })
                    } else {
                        matches!(b.effect.as_ref(), Effect::Token { .. })
                    }
                })
                .unwrap()
        }
        other => panic!("expected positive Endure choice, got {other:?}"),
    };
    assert_eq!(runner.state().waiting_for.acting_player(), Some(chooser));
    assert_eq!(runner.state().waiting_for.acting_players(), vec![chooser]);
    let actions = engine::ai_support::legal_actions(runner.state());
    assert_eq!(
        actions
            .iter()
            .filter(|a| matches!(a, GameAction::ChooseBranch { .. }))
            .count(),
        if departed { 1 } else { 2 }
    );
    assert!(actions
        .iter()
        .any(|a| matches!(a, GameAction::ChooseBranch { index: i } if *i == index)));
    runner.act(GameAction::ChooseBranch { index }).unwrap();
    assert!(runner.state().stack.is_empty());
    assert!(runner.state().resolution_stack.is_empty());
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
}

// CR 701.63a + CR 111.2: exactly one Spirit, whose creator owns and controls it.
fn assert_krumar_spirit(runner: &GameRunner, creator: PlayerId) {
    let tokens: Vec<_> = runner
        .state()
        .objects
        .values()
        .filter(|o| o.zone == Zone::Battlefield && o.is_token)
        .collect();
    assert_eq!(tokens.len(), 1);
    let token = tokens[0];
    assert_eq!(token.power, Some(2));
    assert_eq!(token.toughness, Some(2));
    assert_eq!(token.color, vec![ManaColor::White]);
    assert!(token.card_types.core_types.contains(&CoreType::Creature));
    assert!(token.card_types.subtypes.iter().any(|s| s == "Spirit"));
    assert_eq!(token.owner, creator);
    assert_eq!(token.controller, creator);
}

#[test]
fn krumar_endure_uses_announced_x_for_counters() {
    let (scenario, source) = krumar_scenario(P0);
    let mut runner = scenario.build();
    let index = costed_ability(&runner, source);
    let outcome = runner.activate(source, index).x(2).resolve();
    outcome.assert_life_delta(P0, -2);
    outcome.assert_life_delta(P1, 0);
    assert!(outcome.state().objects[&source].tapped);
    assert_eq!(outcome.mana_pool_total(P0), 5);
    assert_eq!(outcome.mana_pool_color(P0, ManaType::Black), 0);
    assert!(matches!(
        outcome.final_waiting_for(),
        WaitingFor::ChooseOneOfBranch { .. }
    ));
    assert!(outcome
        .events()
        .iter()
        .any(|e| matches!(e, GameEvent::XValueChosen { value: 2, .. })));
    choose_krumar_endure(&mut runner, P0, false, true);
    // CR 701.63a + CR 122.1a: two counters on the enduring incarnation.
    assert_eq!(p1p1(&runner, source), 2);
    assert_eq!(spirit_tokens(&runner), 0);
}

#[test]
fn krumar_endure_uses_announced_x_for_spirit() {
    let (scenario, source) = krumar_scenario(P0);
    let mut runner = scenario.build();
    activate_krumar_onto_stack(&mut runner, source, 2);
    choose_krumar_endure(&mut runner, P0, false, false);
    assert_krumar_spirit(&runner, P0);
    assert_eq!(p1p1(&runner, source), 0);
}

// CR 608.2h + CR 701.63a: a live permanent supplies its current controller.
#[test]
fn krumar_endure_live_control_change_uses_current_controller_for_both_branches() {
    for counters in [true, false] {
        let (mut scenario, source) = krumar_scenario(P0);
        let turn_against = costed_turn_against(&mut scenario);
        let mut runner = scenario.build();
        assert!(runner.state().objects[&turn_against].color.is_empty());
        assert!(runner.state().objects[&turn_against]
            .keywords
            .iter()
            .any(|keyword| matches!(keyword, engine::types::keywords::Keyword::Devoid)));
        let ability = activate_krumar_onto_stack(&mut runner, source, 2);
        give_priority_to(&mut runner, P1);
        let response = runner.cast(turn_against).target_object(source).commit();
        assert_eq!(response.state().players[1].mana_pool.total(), 0);
        runner.resolve_top();
        assert_eq!(runner.state().stack.len(), 1);
        assert_eq!(runner.state().objects[&source].controller, P1);
        assert_eq!(
            runner.state().objects[&source].incarnation,
            ability.source_incarnation.unwrap()
        );
        assert_eq!(ability.self_ref_binding(runner.state()), Some(source));
        choose_krumar_endure(&mut runner, P1, false, counters);
        if counters {
            assert_eq!(p1p1(&runner, source), 2);
            assert_eq!(spirit_tokens(&runner), 0);
        } else {
            assert_krumar_spirit(&runner, P1);
            assert_eq!(p1p1(&runner, source), 0);
        }
    }
}

// CR 400.7 + CR 608.2h: returned ownership does not replace the old controller.
#[test]
fn krumar_endure_flickered_under_owners_control_uses_last_controller() {
    let (mut scenario, source) = krumar_scenario(P1);
    let flicker = costed_flicker_of_fate(&mut scenario);
    let mut runner = scenario.build();
    assert_eq!(runner.state().objects[&source].owner, P1);
    assert_eq!(runner.state().objects[&source].controller, P0);
    let ability = activate_krumar_onto_stack(&mut runner, source, 2);
    runner.cast(flicker).target_object(source).commit();
    runner.resolve_top();
    assert_eq!(runner.state().objects[&source].zone, Zone::Battlefield);
    assert_eq!(runner.state().objects[&source].controller, P1);
    assert_ne!(
        runner.state().objects[&source].incarnation,
        ability.source_incarnation.unwrap()
    );
    assert_eq!(ability.self_ref_binding(runner.state()), None);
    assert_eq!(
        runner.state().lki_by_incarnation[&source][&ability.source_incarnation.unwrap()].controller,
        P0
    );
    choose_krumar_endure(&mut runner, P0, true, false);
    assert_krumar_spirit(&runner, P0);
    assert_eq!(p1p1(&runner, source), 0);
}

// CR 113.7a + CR 608.2h + CR 701.63a: the departure controller, distinct
// from activator and owner, supplies both chooser and Spirit creator.
#[test]
fn krumar_endure_departed_control_change_uses_last_controller_not_activator() {
    let (mut scenario, source) = krumar_scenario(P0);
    let turn_against = costed_turn_against(&mut scenario);
    let flicker = costed_flicker_of_fate(&mut scenario);
    let mut runner = scenario.build();
    let ability = activate_krumar_onto_stack(&mut runner, source, 2);
    give_priority_to(&mut runner, P1);
    let response = runner.cast(turn_against).target_object(source).commit();
    assert_eq!(response.state().players[1].mana_pool.total(), 0);
    runner.resolve_top();
    assert_eq!(runner.state().objects[&source].controller, P1);
    give_priority_to(&mut runner, P0);
    runner.cast(flicker).target_object(source).commit();
    runner.resolve_top();
    assert_eq!(runner.state().objects[&source].zone, Zone::Battlefield);
    assert_eq!(runner.state().objects[&source].controller, P0);
    assert_ne!(
        runner.state().objects[&source].incarnation,
        ability.source_incarnation.unwrap()
    );
    assert_eq!(ability.controller, P0);
    assert_eq!(ability.self_ref_binding(runner.state()), None);
    assert_eq!(
        runner.state().lki_by_incarnation[&source][&ability.source_incarnation.unwrap()].controller,
        P1
    );
    choose_krumar_endure(&mut runner, P1, true, false);
    assert_krumar_spirit(&runner, P1);
    assert_eq!(p1p1(&runner, source), 0);
}

fn krumar_endure_departed_controller_lifetime(concede_last_controller: bool) {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_creature_from_oracle(P0, "Krumar Initiate", 2, 2, KRUMAR_INITIATE)
        .with_subtypes(vec!["Human", "Cleric"])
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Black],
            generic: 1,
        })
        .controlled_by(P0)
        .id();
    scenario.with_mana_pool(
        P0,
        [
            ManaType::Black,
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::White,
            ManaType::Colorless,
        ]
        .into_iter()
        .map(|kind| ManaUnit::new(kind, ObjectId(0), false, vec![]))
        .collect(),
    );
    scenario.with_mana_pool(
        P1,
        [
            ManaType::Red,
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Colorless,
        ]
        .into_iter()
        .map(|kind| ManaUnit::new(kind, ObjectId(0), false, vec![]))
        .collect(),
    );
    let turn_against = costed_turn_against(&mut scenario);
    let flicker = costed_flicker_of_fate(&mut scenario);
    let mut runner = scenario.build();
    assert!(runner.state().objects[&turn_against].color.is_empty());
    let ability = activate_krumar_onto_stack(&mut runner, source, 2);
    let original_incarnation = ability.source_incarnation.unwrap();
    assert_eq!(runner.state().players[0].mana_pool.total(), 2);
    give_priority_to(&mut runner, P1);
    let response = runner.cast(turn_against).target_object(source).commit();
    assert_eq!(response.state().players[1].mana_pool.total(), 0);
    runner.resolve_top();
    assert_eq!(runner.state().stack.len(), 1);
    assert_eq!(runner.state().stack[0].controller, P0);
    assert_eq!(runner.state().objects[&source].controller, P1);
    assert_eq!(
        runner.state().objects[&source].incarnation,
        original_incarnation
    );
    assert_eq!(ability.self_ref_binding(runner.state()), Some(source));
    give_priority_to(&mut runner, P0);
    let response = runner.cast(flicker).target_object(source).commit();
    assert_eq!(response.state().players[0].mana_pool.total(), 0);
    runner.resolve_top();
    // CR 400.7 + CR 608.2h: the returned P0 object is distinct from the
    // departed performer whose exact last controller was P1.
    assert_eq!(runner.state().objects[&source].zone, Zone::Battlefield);
    assert_eq!(runner.state().objects[&source].owner, P0);
    assert_eq!(runner.state().objects[&source].controller, P0);
    assert_ne!(
        runner.state().objects[&source].incarnation,
        original_incarnation
    );
    assert_eq!(ability.controller, P0);
    assert_eq!(ability.self_ref_binding(runner.state()), None);
    assert_eq!(
        runner.state().lki_by_incarnation[&source][&original_incarnation].controller,
        P1
    );
    if concede_last_controller {
        // CR 104.3a + CR 800.4a: P1 leaves immediately, while P0's
        // activation and P0-owned returned permanent remain in the game.
        runner.act(GameAction::Concede { player_id: P1 }).unwrap();
    }
    assert_eq!(
        runner.state().players[1].is_eliminated,
        concede_last_controller
    );
    assert!(!runner.state().players[0].is_eliminated);
    assert!(!runner.state().players[2].is_eliminated);
    assert_eq!(runner.state().players[2].id, PlayerId(2));
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    assert_eq!(runner.state().stack.len(), 1);
    let entry = &runner.state().stack[0];
    assert_eq!(entry.controller, P0);
    match &entry.kind {
        StackEntryKind::ActivatedAbility {
            ability: retained, ..
        } => {
            assert_eq!(retained.source_id, source);
            assert_eq!(retained.source_incarnation, Some(original_incarnation));
            assert_eq!(retained.controller, P0);
            assert_eq!(retained.chosen_x, Some(2));
            assert_eq!(retained.self_ref_binding(runner.state()), None);
        }
        other => panic!("expected retained Krumar activation, got {other:?}"),
    }
    assert_eq!(runner.state().players[0].life, 18);
    assert_eq!(runner.state().players[0].mana_pool.total(), 0);
    assert_eq!(runner.state().objects[&source].zone, Zone::Battlefield);
    assert_eq!(runner.state().objects[&source].owner, P0);
    assert_eq!(runner.state().objects[&source].controller, P0);
    assert_ne!(
        runner.state().objects[&source].incarnation,
        original_incarnation
    );
    assert_eq!(
        runner.state().lki_by_incarnation[&source][&original_incarnation].controller,
        P1
    );
    if concede_last_controller {
        let mut events = Vec::new();
        for _ in 0..8 {
            if runner.state().stack.is_empty() {
                break;
            }
            assert!(matches!(
                runner.state().waiting_for,
                WaitingFor::Priority { .. }
            ));
            events.extend(runner.act(GameAction::PassPriority).unwrap().events);
        }
        // CR 701.63a + CR 118.12a + CR 800.4f: the departed player's
        // optional counter cost is unpaid; no dead-player choice is required.
        let player = match runner.state().waiting_for {
            WaitingFor::Priority { player } => player,
            ref other => panic!("Endure must complete to live priority, got {other:?}"),
        };
        assert!([P0, PlayerId(2)].contains(&player));
        assert_eq!(runner.state().priority_player, player);
        assert_eq!(runner.state().waiting_for.acting_players(), vec![player]);
        assert_eq!(
            engine::game::turn_control::authorized_submitters(runner.state()),
            vec![player]
        );
        assert!(runner.state().stack.is_empty());
        assert!(runner.state().resolution_stack.is_empty());
        // Normal no-op leaf completion is an engine contract (CR 609.3).
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, GameEvent::EffectResolved {
                kind: EffectKind::Endure, source_id, subject: None,
            } if *source_id == source))
                .count(),
            1
        );
        // CR 800.4b + CR 800.4d: no token may be created for departed P1.
        assert_eq!(spirit_tokens(&runner), 0);
    } else {
        choose_krumar_endure(&mut runner, P1, true, false);
        assert_krumar_spirit(&runner, P1);
    }
    // CR 400.7: the returned incarnation cannot receive the old counters.
    assert_eq!(p1p1(&runner, source), 0);
}

#[test]
fn krumar_endure_departed_eliminated_controller_completes_without_choice() {
    krumar_endure_departed_controller_lifetime(true);
}

#[test]
fn krumar_endure_departed_living_controller_still_creates_spirit() {
    krumar_endure_departed_controller_lifetime(false);
}

// CR 400.7 + CR 608.2h: two departures of one storage id must not contaminate
// the original activation's incarnation-qualified last-known controller.
#[test]
fn krumar_endure_second_departure_does_not_replace_original_lki() {
    let (mut scenario, source) = krumar_scenario(P0);
    let turn_against = costed_turn_against(&mut scenario);
    let flicker = costed_flicker_of_fate(&mut scenario);
    let second_flicker = costed_flicker_of_fate(&mut scenario);
    let mut runner = scenario.build();
    let ability = activate_krumar_onto_stack(&mut runner, source, 2);
    give_priority_to(&mut runner, P1);
    let response = runner.cast(turn_against).target_object(source).commit();
    assert_eq!(response.state().players[1].mana_pool.total(), 0);
    runner.resolve_top();
    assert_eq!(runner.state().objects[&source].controller, P1);
    give_priority_to(&mut runner, P0);
    runner.cast(flicker).target_object(source).commit();
    runner.resolve_top();
    let first_return = runner.state().objects[&source].incarnation;
    assert_eq!(runner.state().objects[&source].zone, Zone::Battlefield);
    assert_eq!(runner.state().objects[&source].controller, P0);
    assert_ne!(first_return, ability.source_incarnation.unwrap());
    give_priority_to(&mut runner, P0);
    runner.cast(second_flicker).target_object(source).commit();
    runner.resolve_top();
    assert_eq!(runner.state().objects[&source].zone, Zone::Battlefield);
    assert_eq!(runner.state().objects[&source].controller, P0);
    assert_ne!(runner.state().objects[&source].incarnation, first_return);
    assert_ne!(
        runner.state().objects[&source].incarnation,
        ability.source_incarnation.unwrap()
    );
    assert_eq!(
        runner.state().lki_by_incarnation[&source][&first_return].controller,
        P0
    );
    assert_eq!(
        runner.state().lki_by_incarnation[&source][&ability.source_incarnation.unwrap()].controller,
        P1
    );
    assert_eq!(ability.self_ref_binding(runner.state()), None);
    choose_krumar_endure(&mut runner, P1, true, false);
    assert_krumar_spirit(&runner, P1);
    assert_eq!(p1p1(&runner, source), 0);
}

// CR 400.7 + CR 608.2h: the departed incarnation need not return to endure.
#[test]
fn krumar_endure_unsummoned_source_uses_departure_controller() {
    let (mut scenario, source) = krumar_scenario(P0);
    let turn_against = costed_turn_against(&mut scenario);
    let unsummon = scenario
        .add_spell_to_hand_from_oracle(P0, "Unsummon", true, UNSUMMON)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Blue],
            generic: 0,
        })
        .id();
    let mut runner = scenario.build();
    let ability = activate_krumar_onto_stack(&mut runner, source, 2);
    give_priority_to(&mut runner, P1);
    let response = runner.cast(turn_against).target_object(source).commit();
    assert_eq!(response.state().players[1].mana_pool.total(), 0);
    runner.resolve_top();
    assert_eq!(runner.state().objects[&source].controller, P1);
    give_priority_to(&mut runner, P0);
    runner.cast(unsummon).target_object(source).commit();
    runner.resolve_top();
    assert_eq!(runner.state().objects[&source].zone, Zone::Hand);
    assert!(runner.state().players[0].hand.contains(&source));
    assert_ne!(
        runner.state().objects[&source].incarnation,
        ability.source_incarnation.unwrap()
    );
    assert_eq!(
        runner.state().lki_by_incarnation[&source][&ability.source_incarnation.unwrap()].controller,
        P1
    );
    assert_eq!(ability.self_ref_binding(runner.state()), None);
    choose_krumar_endure(&mut runner, P1, true, false);
    assert_krumar_spirit(&runner, P1);
    assert_eq!(p1p1(&runner, source), 0);
}

// CR 701.63b + CR 119.4b: real X=0 pays black mana and taps, then completes
// Endure without a choice or token, for both live and departed incarnations.
#[test]
fn krumar_endure_zero_completes_live_and_departed_without_choice() {
    for departed in [false, true] {
        let (mut scenario, source) = krumar_scenario(P0);
        let flicker = costed_flicker_of_fate(&mut scenario);
        let mut runner = scenario.build();
        let ability = activate_krumar_onto_stack(&mut runner, source, 0);
        if departed {
            runner.cast(flicker).target_object(source).commit();
            runner.resolve_top();
            assert_eq!(runner.state().objects[&source].zone, Zone::Battlefield);
            assert_ne!(
                runner.state().objects[&source].incarnation,
                ability.source_incarnation.unwrap()
            );
            assert_eq!(ability.self_ref_binding(runner.state()), None);
        } else {
            assert_eq!(ability.self_ref_binding(runner.state()), Some(source));
        }
        let mut events = Vec::new();
        for _ in 0..8 {
            if runner.state().stack.is_empty() {
                break;
            }
            assert!(matches!(
                runner.state().waiting_for,
                WaitingFor::Priority { .. }
            ));
            events.extend(runner.act(GameAction::PassPriority).unwrap().events);
        }
        assert_eq!(
            events
                .iter()
                .filter(|e| matches!(e, GameEvent::EffectResolved {
            kind: EffectKind::Endure, source_id, subject: None,
        } if *source_id == source))
                .count(),
            1
        );
        assert_eq!(spirit_tokens(&runner), 0);
        assert_eq!(p1p1(&runner, source), 0);
        assert!(runner.state().stack.is_empty());
        assert!(runner.state().resolution_stack.is_empty());
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { .. }
        ));
    }
}

// Synthetic class/composition fixtures: these are not Spider-Man Oracle text.
// The granting activation excludes itself, donates a distinct marker from a
// live SelfRef, and independently draws for its original activator.
fn self_ref_donor_completion_and_draw(stale: bool) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(
        P0,
        [ManaType::White, ManaType::Colorless]
            .into_iter()
            .map(|kind| ManaUnit::new(kind, ObjectId(0), false, vec![]))
            .collect(),
    );
    let recipient = scenario
        .add_creature(P0, "Independent Recipient", 2, 2)
        .id();
    let marker = AbilityDefinition::new(
        AbilityKind::Activated,
        Effect::GainLife {
            amount: QuantityExpr::Fixed { value: 3 },
            player: TargetFilter::Controller,
        },
    )
    .cost(AbilityCost::Mana {
        cost: ManaCost::generic(0),
    });
    let granting = AbilityDefinition::new(
        AbilityKind::Activated,
        Effect::GainActivatedAbilitiesOfTarget {
            target: TargetFilter::SelfRef,
            recipient: TargetFilter::SpecificObject { id: recipient },
            scope: GrantedAbilityScope::AllOther,
            duration: Some(Duration::UntilEndOfTurn),
        },
    )
    .cost(AbilityCost::Mana {
        cost: ManaCost::generic(0),
    })
    .sub_ability(AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::Draw {
            count: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::Controller,
        },
    ));
    let donor = scenario
        .add_creature(P1, "Synthetic Donor", 2, 2)
        .controlled_by(P0)
        .with_ability_definition(granting)
        .with_ability_definition(marker.clone())
        .id();
    let p0_top = scenario.add_card_to_library_top(P0, "P0 Independent Draw");
    let p1_top = scenario.add_card_to_library_top(P1, "P1 Untouched Top");
    let flicker = costed_flicker_of_fate(&mut scenario);
    let mut runner = scenario.build();
    let recipient_incarnation = runner.state().objects[&recipient].incarnation;
    activate_onto_stack(&mut runner, donor, 0, None);
    let ability = match &runner.state().stack.last().unwrap().kind {
        StackEntryKind::ActivatedAbility { ability, .. } => ability.clone(),
        other => panic!("expected donor activation, got {other:?}"),
    };
    assert_eq!(
        ability.source_incarnation,
        Some(runner.state().objects[&donor].incarnation)
    );
    assert_eq!(ability.controller, P0);
    assert!(matches!(
        ability.effect,
        Effect::GainActivatedAbilitiesOfTarget {
            target: TargetFilter::SelfRef,
            ..
        }
    ));
    assert!(matches!(
        ability.sub_ability.as_ref().unwrap().effect,
        Effect::Draw {
            count: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::Controller,
        }
    ));
    if stale {
        runner.cast(flicker).target_object(donor).commit();
        runner.resolve_top();
        assert_eq!(runner.state().stack.len(), 1);
        assert_eq!(runner.state().objects[&donor].zone, Zone::Battlefield);
        assert_eq!(runner.state().objects[&donor].controller, P1);
        assert_ne!(
            runner.state().objects[&donor].incarnation,
            ability.source_incarnation.unwrap()
        );
        assert_eq!(ability.self_ref_binding(runner.state()), None);
    } else {
        assert_eq!(ability.self_ref_binding(runner.state()), Some(donor));
    }
    assert_eq!(runner.state().objects[&recipient].zone, Zone::Battlefield);
    assert_eq!(
        runner.state().objects[&recipient].incarnation,
        recipient_incarnation
    );
    assert!(runner.state().objects[&donor]
        .abilities
        .iter()
        .any(|a| a == &marker));
    let mut events = Vec::new();
    for _ in 0..8 {
        if runner.state().stack.is_empty() {
            break;
        }
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { .. }
        ));
        events.extend(runner.act(GameAction::PassPriority).unwrap().events);
    }
    // Normal leaf completion is an engine contract. Even a stale-leaf error
    // in a selective control still reaches independent Draw (CR 608.2c).
    let completions: Vec<_> = events
        .iter()
        .enumerate()
        .filter_map(|(index, e)| {
            matches!(e, GameEvent::EffectResolved {
        kind: EffectKind::GainActivatedAbilitiesOfTarget, source_id, subject: None,
    } if *source_id == donor)
            .then_some(index)
        })
        .collect();
    assert_eq!(completions.len(), 1);
    let draw_completion = events
        .iter()
        .position(|e| {
            matches!(e, GameEvent::EffectResolved {
        kind: EffectKind::Draw, source_id, ..
    } if *source_id == donor)
        })
        .expect("independent Draw completes");
    assert!(completions[0] < draw_completion);
    // CR 113.8 + CR 121.1: Draw Controller remains the original activator.
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e, GameEvent::CardDrawn {
        player_id: P0, object_id, ..
    } if *object_id == p0_top))
            .count(),
        1
    );
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e, GameEvent::CardDrawn { .. }))
            .count(),
        1
    );
    assert!(runner.state().players[0].hand.contains(&p0_top));
    assert_eq!(runner.state().objects[&p0_top].zone, Zone::Hand);
    assert_eq!(runner.state().players[1].library[0], p1_top);
    assert_eq!(runner.state().objects[&p1_top].zone, Zone::Library);
    assert!(runner.state().stack.is_empty());
    assert!(runner.state().resolution_stack.is_empty());
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    if stale {
        assert!(runner.state().objects[&recipient].abilities.is_empty());
        assert!(!runner
            .state()
            .transient_continuous_effects
            .iter()
            .any(|tce| tce.source_id == donor
                && tce
                    .modifications
                    .iter()
                    .any(|m| matches!(m, ContinuousModification::GrantAbility { .. }))));
    } else {
        // CR 611.2c: AllOther snapshots only the independent marker.
        assert_eq!(
            runner.state().objects[&recipient].abilities.as_slice(),
            &[marker]
        );
        let outcome = runner.activate(recipient, 0).resolve();
        outcome.assert_life_delta(P0, 3);
        outcome.assert_life_delta(P1, 0);
        outcome.assert_hand_drawn(P0, 0);
        assert!(!outcome.events().iter().any(|e| matches!(
            e,
            GameEvent::EffectResolved {
                kind: EffectKind::GainActivatedAbilitiesOfTarget | EffectKind::Draw,
                ..
            }
        )));
        assert!(matches!(
            outcome.final_waiting_for(),
            WaitingFor::Priority { .. }
        ));
        assert!(outcome.state().stack.is_empty());
        assert!(outcome.state().resolution_stack.is_empty());
    }
    assert_eq!(runner.state().objects[&donor].abilities.len(), 2);
    assert!(!runner
        .state()
        .transient_continuous_effects
        .iter()
        .any(|tce| tce.source_id == donor
            && tce.affected == TargetFilter::SpecificObject { id: donor }));
}

#[test]
fn stale_self_ref_donor_completes_and_draws_without_granting_returned_abilities() {
    self_ref_donor_completion_and_draw(true);
}

#[test]
fn live_self_ref_donor_grants_and_completes_independent_draw() {
    self_ref_donor_completion_and_draw(false);
}
