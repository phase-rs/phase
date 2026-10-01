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
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::counter::CounterType;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
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
    for _ in 0..2 {
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
