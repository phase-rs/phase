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
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
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

/// A spell targeting a creature card in a graveyard (e.g. Zombify) does not
/// trigger Horobi: only the spell is placed on the stack, the creature returns
/// to the battlefield, and Horobi does not destroy it (CR 109.2).
#[test]
fn graveyard_creature_card_spell_target_does_not_trigger_horobi() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P1, "Horobi, Death's Wail", 4, 4, HOROBI);
    let corpse = scenario
        .add_creature_to_graveyard(P0, "Grizzly Bears", 2, 2)
        .id();
    let zombify = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Zombify",
            false,
            "Return target creature card from your graveyard to the battlefield.",
        )
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Black],
            generic: 3,
        })
        .id();
    scenario.with_mana_pool(P0, pool(1, 3));
    let mut runner = scenario.build();

    let outcome = runner.cast(zombify).target_object(corpse).resolve();

    assert_eq!(
        stack_pushes(outcome.events()),
        1,
        "only the reanimation spell goes on the stack; Horobi must not trigger"
    );
    assert_eq!(
        outcome.zone_of(corpse),
        Zone::Battlefield,
        "a creature card targeted in a graveyard is not a creature permanent (CR 109.2); \
         Horobi must not trigger or destroy it"
    );
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
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Black],
            generic: 0,
        })
        .id();
    let mut runner = scenario.build();

    let commit = runner.cast(disfigure).target_object(pet).commit();

    assert!(
        commit.events().iter().any(|event| matches!(
            event,
            GameEvent::PermanentSacrificed { object_id, .. } if *object_id == pet
        )),
        "Blood Pet must be sacrificed as a mana source during payment"
    );
    assert_eq!(
        stack_pushes(commit.events()),
        2,
        "both Disfigure and Horobi's trigger must reach the stack"
    );

    let outcome = commit.resolve();

    assert_eq!(outcome.zone_of(pet), Zone::Graveyard);
    assert!(matches!(
        outcome.final_waiting_for(),
        WaitingFor::Priority { .. }
    ));
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
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Black],
            generic: 0,
        })
        .id();
    let mut runner = scenario.build();

    let commit = runner.cast(charm).modes(&[0]).target_object(pet).commit();

    assert!(
        commit.events().iter().any(|event| matches!(
            event,
            GameEvent::PermanentSacrificed { object_id, .. } if *object_id == pet
        )),
        "Blood Pet must be sacrificed as a mana source during payment"
    );
    assert_eq!(
        stack_pushes(commit.events()),
        2,
        "both the modal spell and Horobi's trigger must reach the stack"
    );

    let outcome = commit.resolve();

    assert_eq!(outcome.zone_of(pet), Zone::Graveyard);
    assert!(matches!(
        outcome.final_waiting_for(),
        WaitingFor::Priority { .. }
    ));
}

/// CR 601.2c + CR 601.2g + CR 603.2: the initial automatic spell route. Cut Down
/// targets a creature with total power and toughness 5 or less. Blood Pet (1/1)
/// is the sole eligible target (Horobi is 4/4, total 8 > 5), so target selection
/// is unprompted and automatic. Automatic payment sacrifices Blood Pet for {B}.
/// Pre-payment trigger collection must ensure Horobi triggers despite Blood Pet
/// being in the graveyard by the time payment completes.
#[test]
fn cut_down_targeting_sacrificed_blood_pet_mana_source_triggers_horobi() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P1, "Horobi, Death's Wail", 4, 4, HOROBI);
    let pet = scenario
        .add_creature_from_oracle(P0, "Blood Pet", 1, 1, "Sacrifice this creature: Add {B}.")
        .id();
    let cut_down = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Cut Down",
            true,
            "Destroy target creature with total power and toughness 5 or less.",
        )
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Black],
            generic: 0,
        })
        .id();
    let mut runner = scenario.build();

    // No target_object specified: auto-selects Blood Pet as the sole legal target.
    let commit = runner.cast(cut_down).commit();

    assert!(
        commit.events().iter().any(|event| matches!(
            event,
            GameEvent::PermanentSacrificed { object_id, .. } if *object_id == pet
        )),
        "Blood Pet must be sacrificed as a mana source during payment"
    );
    assert_eq!(
        stack_pushes(commit.events()),
        2,
        "both Cut Down and Horobi's trigger must reach the stack"
    );

    let outcome = commit.resolve();

    assert_eq!(outcome.zone_of(pet), Zone::Graveyard);
    assert!(matches!(
        outcome.final_waiting_for(),
        WaitingFor::Priority { .. }
    ));
}

const BLOODCHIEFS_THIRST: &str = "Kicker {2}{B} (You may pay an additional {2}{B} as you cast this spell.)\nDestroy target creature or planeswalker with mana value 2 or less. If this spell was kicked, instead destroy target creature or planeswalker.";
const CONTROLLED_CREATURE_AURA: &str = "Enchant creature you control
Enchanted creature gets +1/+1.";
const INSATIABLE_HEMOPHAGE: &str = "Mutate {2}{B} (If you cast this spell for its mutate cost, put it over or under target non-Human creature you own. They mutate into the creature on top plus all abilities from under it.)\nDeathtouch\nWhenever this creature mutates, each opponent loses X life and you gain X life, where X is the number of times this creature has mutated.";

fn blood_pet_and_horobi(scenario: &mut GameScenario) -> ObjectId {
    // Horobi's mana value (4, {2}{B}{B}) keeps it out of Bloodchief's Thirst's
    // MV ≤ 2 filter.
    scenario
        .add_creature_from_oracle(P1, "Horobi, Death's Wail", 4, 4, HOROBI)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Black, ManaCostShard::Black],
            generic: 2,
        });
    scenario
        .add_creature_from_oracle(P0, "Blood Pet", 1, 1, "Sacrifice this creature: Add {B}.")
        .id()
}

fn assert_pet_sacrificed_and_both_stacked(
    commit: &engine::game::scenario::CastCommit<'_>,
    pet: ObjectId,
) {
    assert!(
        commit.events().iter().any(|event| matches!(
            event,
            GameEvent::PermanentSacrificed { object_id, .. } if *object_id == pet
        )),
        "Blood Pet must be sacrificed as a mana source during payment"
    );
    assert_eq!(
        stack_pushes(commit.events()),
        2,
        "both the spell and Horobi's trigger must reach the stack"
    );
}

/// CR 601.2c + CR 601.2g + CR 603.2: the ownerless deferred-target route.
/// Bloodchief's Thirst's kicker is unaffordable (only Blood Pet's {B} is
/// available), so its target is chosen after the kicker decision with no
/// preceding prompt. Blood Pet is the only legal target (Horobi's mana value is
/// 4) and is sacrificed for the {B}; Horobi must still trigger.
#[test]
fn deferred_kicker_spell_targeting_sacrificed_blood_pet_triggers_horobi() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let pet = blood_pet_and_horobi(&mut scenario);
    let thirst = scenario
        .add_spell_to_hand(P0, "Bloodchief's Thirst", false)
        .from_oracle_text_with_keywords(&["Kicker"], BLOODCHIEFS_THIRST)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Black],
            generic: 0,
        })
        .id();
    let mut runner = scenario.build();

    let commit = runner.cast(thirst).commit();

    assert_pet_sacrificed_and_both_stacked(&commit, pet);
    let outcome = commit.resolve();
    assert_eq!(outcome.zone_of(pet), Zone::Graveyard);
    assert!(matches!(
        outcome.final_waiting_for(),
        WaitingFor::Priority { .. }
    ));
}

/// CR 601.2c + CR 601.2g + CR 603.2: the automatic Aura-targeting route.
/// An Aura that can only enchant a creature its caster controls (the engine does
/// not yet type Kaya's Ghostform's "creature or planeswalker you control" clause)
/// has Blood Pet as its sole legal target; payment sacrifices it.
#[test]
fn aura_targeting_sacrificed_blood_pet_triggers_horobi() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let pet = blood_pet_and_horobi(&mut scenario);
    let enchant: Keyword = "Enchant:creature you control"
        .parse()
        .expect("enchant keyword parses");
    assert!(matches!(enchant, Keyword::Enchant(_)));
    let aura = scenario
        .add_spell_to_hand(P0, "Test Aura", false)
        .as_enchantment()
        .with_subtypes(vec!["Aura"])
        .from_oracle_text_with_keywords(&[], CONTROLLED_CREATURE_AURA)
        .with_keyword(enchant)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Black],
            generic: 0,
        })
        .id();
    let mut runner = scenario.build();

    let commit = runner.cast(aura).commit();

    assert_pet_sacrificed_and_both_stacked(&commit, pet);
    let outcome = commit.resolve();
    assert_eq!(outcome.zone_of(pet), Zone::Graveyard);
    assert!(matches!(
        outcome.final_waiting_for(),
        WaitingFor::Priority { .. }
    ));
}

/// CR 702.140a + CR 601.2c + CR 601.2g + CR 603.2: the automatic mutate route.
/// Casting for the mutate cost targets a non-Human creature its caster owns;
/// Blood Pet is the sole host and supplies the {B} of the {2}{B} mutate cost by
/// being sacrificed; the pool holds only the two generic mana.
#[test]
fn mutate_targeting_sacrificed_blood_pet_triggers_horobi() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let pet = blood_pet_and_horobi(&mut scenario);
    scenario.with_mana_pool(P0, pool(0, 2));
    let mutator = scenario
        .add_creature_to_hand_from_oracle(P0, "Insatiable Hemophage", 3, 3, INSATIABLE_HEMOPHAGE)
        .from_oracle_text_with_keywords(&["Mutate"], INSATIABLE_HEMOPHAGE)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Black],
            generic: 3,
        })
        .id();
    let mut runner = scenario.build();

    let commit = runner.cast(mutator).commit();

    assert_pet_sacrificed_and_both_stacked(&commit, pet);
    let outcome = commit.resolve();
    assert_eq!(outcome.zone_of(pet), Zone::Graveyard);
    assert!(matches!(
        outcome.final_waiting_for(),
        WaitingFor::Priority { .. }
    ));
}
