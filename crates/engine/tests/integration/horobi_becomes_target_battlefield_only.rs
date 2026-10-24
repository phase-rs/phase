//! CR 109.2 + CR 603.2: "Whenever a creature becomes the target of a spell or
//! ability" (Horobi, Death's Wail) watches creature PERMANENTS on the
//! battlefield. A creature card targeted in a graveyard — Nighteyes the
//! Desecrator's "Put target creature card from a graveyard onto the battlefield"
//! (Nezumi Graverobber's flip side) — is not "a creature", so the trigger must
//! not fire, and the reanimated creature must survive. Targeting a creature on
//! the battlefield still fires the trigger and destroys it.
//!
//! Zone outcomes alone cannot see the defect: a spurious trigger resolves
//! before the reanimation and its destroy finds no permanent. So each test
//! counts what reached the stack — only the targeting spell or ability itself
//! when the target is a card in a graveyard.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::events::GameEvent;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const HOROBI: &str =
    "Flying\nWhenever a creature becomes the target of a spell or ability, destroy that creature.";
const NIGHTEYES: &str =
    "{4}{B}: Put target creature card from a graveyard onto the battlefield under your control.";
const GRAVEROBBER: &str = "{1}{B}: Exile target card from an opponent's graveyard. If no cards are in that graveyard, flip this creature.";
const PUMP: &str = "Target creature gets +3/+3 until end of turn.";
const CALL_FOR_BLOOD: &str = "As an additional cost to cast this spell, sacrifice a creature.\nTarget creature gets -X/-X until end of turn, where X is the sacrificed creature's power.";
const MOGG_FANATIC: &str = "Sacrifice Mogg Fanatic: It deals 1 damage to any target.";

fn pool(black: usize, generic: usize) -> Vec<ManaUnit> {
    let unit = |mana_type| ManaUnit::new(mana_type, ObjectId(0), false, vec![]);
    std::iter::repeat_n(unit(ManaType::Black), black)
        .chain(std::iter::repeat_n(unit(ManaType::Colorless), generic))
        .collect()
}

fn stack_pushes(events: &[GameEvent]) -> usize {
    events
        .iter()
        .filter(|event| matches!(event, GameEvent::StackPushed { .. }))
        .count()
}

/// Targeting a creature card in a graveyard does not trigger Horobi: the card
/// returns to the battlefield and stays there.
#[test]
fn graveyard_creature_card_target_does_not_trigger_horobi() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P1, "Horobi, Death's Wail", 4, 4, HOROBI);
    let nighteyes = scenario
        .add_creature_from_oracle(P0, "Nighteyes the Desecrator", 4, 2, NIGHTEYES)
        .id();
    let corpse = scenario
        .add_creature_to_graveyard(P1, "Grizzly Bears", 2, 2)
        .id();
    scenario.with_mana_pool(P0, pool(1, 4));
    let mut runner = scenario.build();

    let outcome = runner
        .activate(nighteyes, 0)
        .target_object(corpse)
        .resolve();

    assert_eq!(
        stack_pushes(outcome.events()),
        1,
        "only the reanimation ability goes on the stack; Horobi must not trigger"
    );
    assert_eq!(
        outcome.zone_of(corpse),
        Zone::Battlefield,
        "a creature card targeted in a graveyard is not a creature (CR 109.2); \
         Horobi must not destroy it after it is reanimated"
    );
    assert_eq!(outcome.zone_of(nighteyes), Zone::Battlefield);
}

/// A "card" ability targeting a creature card in a graveyard (Nezumi
/// Graverobber's front face) does not trigger Horobi either.
#[test]
fn graveyard_card_target_does_not_trigger_horobi() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P1, "Horobi, Death's Wail", 4, 4, HOROBI);
    let graverobber = scenario
        .add_creature_from_oracle(P0, "Nezumi Graverobber", 2, 1, GRAVEROBBER)
        .id();
    let corpse = scenario
        .add_creature_to_graveyard(P1, "Grizzly Bears", 2, 2)
        .id();
    scenario.add_creature_to_graveyard(P1, "Hill Giant", 3, 3);
    scenario.with_mana_pool(P0, pool(1, 1));
    let mut runner = scenario.build();

    let outcome = runner
        .activate(graverobber, 0)
        .target_object(corpse)
        .resolve();

    assert_eq!(
        stack_pushes(outcome.events()),
        1,
        "only the exile ability goes on the stack; Horobi must not trigger"
    );
    assert_eq!(outcome.zone_of(corpse), Zone::Exile);
    assert_eq!(outcome.zone_of(graverobber), Zone::Battlefield);
}

/// Positive control: a creature on the battlefield becoming the target still
/// fires Horobi's trigger, which destroys it.
#[test]
fn battlefield_creature_target_triggers_horobi() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P1, "Horobi, Death's Wail", 4, 4, HOROBI);
    let bear = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let pump = scenario
        .add_spell_to_hand_from_oracle(P0, "Giant Growth", true, PUMP)
        .id();
    scenario.with_mana_pool(P0, pool(0, 1));
    let mut runner = scenario.build();

    let outcome = runner.cast(pump).target_object(bear).resolve();

    assert_eq!(
        stack_pushes(outcome.events()),
        2,
        "the pump spell and Horobi's trigger both go on the stack"
    );
    assert_eq!(
        outcome.zone_of(bear),
        Zone::Graveyard,
        "Horobi destroys a creature permanent that becomes the target"
    );
}

/// CR 601.2c + CR 601.2h: targets are chosen before costs are paid, so a
/// creature that becomes the target while on the battlefield and is then
/// sacrificed to pay that same spell's cost still triggers Horobi. The
/// battlefield gate must not read the post-cost zone.
#[test]
fn target_sacrificed_as_cost_still_triggers_horobi() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P1, "Horobi, Death's Wail", 4, 4, HOROBI);
    let bear = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Call for Blood", true, CALL_FOR_BLOOD)
        .id();
    scenario.with_mana_pool(P0, pool(1, 4));
    let mut runner = scenario.build();

    let outcome = runner
        .cast(spell)
        .target_object(bear)
        .sacrifice_with(&[bear])
        .resolve();

    assert_eq!(outcome.zone_of(bear), Zone::Graveyard);
    assert_eq!(
        stack_pushes(outcome.events()),
        2,
        "the spell and Horobi's trigger both go on the stack"
    );
}

/// CR 601.2c + CR 602.2b + CR 601.2h: targets are chosen before costs are paid.
/// When Mogg Fanatic activates its ability targeting itself with Horobi on the
/// battlefield, its automatic self-sacrifice moves it to the graveyard during
/// cost payment, but it became the target while on the battlefield. Horobi must
/// still trigger, and both the activated ability and Horobi's trigger reach the stack.
#[test]
fn mogg_fanatic_targeting_itself_triggers_horobi() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P1, "Horobi, Death's Wail", 4, 4, HOROBI);
    let fanatic = scenario
        .add_creature_from_oracle(P0, "Mogg Fanatic", 1, 1, MOGG_FANATIC)
        .id();
    let mut runner = scenario.build();

    let outcome = runner.activate(fanatic, 0).target_object(fanatic).resolve();

    assert_eq!(outcome.zone_of(fanatic), Zone::Graveyard);
    assert_eq!(
        stack_pushes(outcome.events()),
        2,
        "both Mogg Fanatic's ability and Horobi's trigger must reach the stack"
    );
}

/// CR 601.2c + CR 601.2g + CR 602.2b: targets are announced before mana costs
/// are paid. When Bonesplitter's equip ability targets Blood Pet with an empty
/// mana pool, the automatic mana payment sacrifices Blood Pet to pay the {1}
/// equip cost. Because Blood Pet was a legal battlefield creature target when
/// announced, Horobi must trigger, and both the equip ability and Horobi's
/// trigger reach the stack.
#[test]
fn bonesplitter_targeting_sacrificed_blood_pet_mana_source_triggers_horobi() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P1, "Horobi, Death's Wail", 4, 4, HOROBI);
    let pet = scenario
        .add_creature_from_oracle(P0, "Blood Pet", 1, 1, "Sacrifice this creature: Add {B}.")
        .id();
    let bonesplitter = scenario
        .add_artifact_from_oracle(P0, "Bonesplitter", "Equip {1}")
        .with_subtypes(vec!["Equipment"])
        .id();
    let mut runner = scenario.build();

    let outcome = runner
        .activate(bonesplitter, 0)
        .target_object(pet)
        .resolve();

    assert_eq!(outcome.zone_of(pet), Zone::Graveyard);
    assert_eq!(
        stack_pushes(outcome.events()),
        2,
        "both Bonesplitter's equip ability and Horobi's trigger must reach the stack"
    );
}

/// CR 601.2c + CR 601.2g + CR 603.2: the spell route of the same rule. Disfigure
/// targets Blood Pet, and with an empty mana pool automatic payment sacrifices
/// Blood Pet for {B}. Blood Pet became the target while on the battlefield, so
/// Horobi must trigger even though the target is in the graveyard by the time
/// the event would otherwise be matched.
#[test]
fn disfigure_targeting_sacrificed_blood_pet_mana_source_triggers_horobi() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P1, "Horobi, Death's Wail", 4, 4, HOROBI);
    let pet = scenario
        .add_creature_from_oracle(P0, "Blood Pet", 1, 1, "Sacrifice this creature: Add {B}.")
        .id();
    let disfigure = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Disfigure",
            true,
            "Target creature gets -2/-2 until end of turn.",
        )
        .id();
    let mut runner = scenario.build();

    let outcome = runner.cast(disfigure).target_object(pet).resolve();

    assert_eq!(outcome.zone_of(pet), Zone::Graveyard);
    assert_eq!(
        stack_pushes(outcome.events()),
        2,
        "both Disfigure and Horobi's trigger must reach the stack"
    );
}

/// CR 700.2 + CR 601.2c + CR 603.2: the modal spell route. Targets for the
/// chosen mode are declared after the mode choice, still before mana payment,
/// so the Blood Pet sacrifice for {B} must not hide the targeting from Horobi.
#[test]
fn modal_spell_targeting_sacrificed_blood_pet_mana_source_triggers_horobi() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P1, "Horobi, Death's Wail", 4, 4, HOROBI);
    let pet = scenario
        .add_creature_from_oracle(P0, "Blood Pet", 1, 1, "Sacrifice this creature: Add {B}.")
        .id();
    let charm = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Test Charm",
            true,
            "Choose one —\n• Target creature gets -2/-2 until end of turn.\n• Target player loses 2 life.",
        )
        .id();
    let mut runner = scenario.build();

    let outcome = runner.cast(charm).modes(&[0]).target_object(pet).resolve();

    assert_eq!(outcome.zone_of(pet), Zone::Graveyard);
    assert_eq!(
        stack_pushes(outcome.events()),
        2,
        "both the modal spell and Horobi's trigger must reach the stack"
    );
}
