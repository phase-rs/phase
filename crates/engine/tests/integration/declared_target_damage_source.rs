//! Declared-target damage source (CR 120.1 + CR 608.2c).
//!
//! When a later instruction says "that creature deals damage equal to its
//! power ...", the object dealing the damage is the creature an earlier
//! instruction declared, and "its power" is that creature's power. Each test
//! drives the real `apply` pipeline on a card's verbatim Oracle text and
//! asserts, for every non-combat damage event, its source object, its recipient
//! and its amount.

use engine::game::combat::AttackTarget;
use engine::game::game_object::AttachTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::{TargetSelectionSlot, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const KARPLUSAN_YETI: &str = "{T}: This creature deals damage equal to its power to target creature. That creature deals damage equal to its power to this creature.";
const DURKWOOD_TRACKER: &str = "{1}{G}, {T}: If this creature is on the battlefield, it deals damage equal to its power to target attacking creature. That creature deals damage equal to its power to this creature.";
const GARGANTUAN_GORILLA: &str = "At the beginning of your upkeep, you may sacrifice a Forest. If you sacrifice a snow Forest this way, this creature gains trample until end of turn. If you don't sacrifice a Forest, sacrifice this creature and it deals 7 damage to you.\n{T}: This creature deals damage equal to its power to another target creature. That creature deals damage equal to its power to this creature.";
const TAHNGARTH: &str = "Vigilance\n{1}{R}, {T}: Tahngarth deals damage equal to its power to target creature. That creature deals damage equal to its power to Tahngarth.";
const TRACKER: &str = "{G}{G}, {T}: This creature deals damage equal to its power to target creature. That creature deals damage equal to its power to this creature.";
const VEIN_DRINKER: &str = "Flying\n{R}, {T}: This creature deals damage equal to its power to target creature. That creature deals damage equal to its power to this creature.\nWhenever a creature dealt damage by this creature this turn dies, put a +1/+1 counter on this creature.";
const STALKING_YETI: &str = "When this creature enters, if it's on the battlefield, it deals damage equal to its power to target creature an opponent controls and that creature deals damage equal to its power to this creature.\n{2}{S}: Return this creature to its owner's hand. Activate only as a sorcery. ({S} can be paid with one mana from a snow source.)";
const FORM_OF_THE_DINOSAUR: &str = "When this enchantment enters, your life total becomes 15.\nAt the beginning of your upkeep, this enchantment deals 15 damage to target creature an opponent controls and that creature deals damage equal to its power to you.";
const HUNTERS_BOW: &str = "When this Equipment enters, attach it to target creature you control. That creature deals damage equal to its power to up to one target creature you don't control.\nEquipped creature has reach and ward {2}.\nEquip {1}";
const BIND_THE_MONSTER: &str = "Enchant creature\nWhen this Aura enters, tap enchanted creature. It deals damage to you equal to its power.\nEnchanted creature doesn't untap during its controller's untap step.";
const SELFLESS_EXORCIST: &str = "{T}: Exile target creature card from a graveyard. That card deals damage equal to its power to this creature.";
const PREDATORY_URGE: &str = "Enchant creature\nEnchanted creature has \"{T}: This creature deals damage equal to its power to target creature. That creature deals damage equal to its power to this creature.\"";
const CYCLOPS_GLADIATOR: &str = "Whenever this creature attacks, you may have it deal damage equal to its power to target creature defending player controls. If you do, that creature deals damage equal to its power to this creature.";
const BACKLASH: &str =
    "Tap target untapped creature. That creature deals damage equal to its power to its controller.";
const TRAITORS_ROAR: &str = "Tap target untapped creature. It deals damage equal to its power to its controller.\nConspire (As you cast this spell, you may tap two untapped creatures you control that share a color with it. When you do, copy it and you may choose a new target for the copy.)";
const HALANA: &str = "Reach\nWhenever another creature you control enters, you may pay {2}. When you do, that creature deals damage equal to its power to target creature.\nPartner (You can have two commanders if both have partner.)";

type DamageEvent = (ObjectId, TargetRef, u32);

fn noncombat_damage(events: &[GameEvent]) -> Vec<DamageEvent> {
    events
        .iter()
        .filter_map(|event| match event {
            GameEvent::DamageDealt {
                source_id,
                target,
                amount,
                is_combat: false,
                ..
            } => Some((*source_id, target.clone(), *amount)),
            _ => None,
        })
        .collect()
}

fn mana(runner: &mut GameRunner, colors: &[ManaType]) {
    for &color in colors {
        runner
            .state_mut()
            .add_mana_to_pool(P0, ManaUnit::new(color, ObjectId(0), false, vec![]));
    }
}

fn damage_marked(runner: &GameRunner, id: ObjectId) -> u32 {
    runner.state().objects[&id].damage_marked
}

fn pick(slot: &TargetSelectionSlot, objects: &[ObjectId]) -> Option<TargetRef> {
    objects
        .iter()
        .map(|&id| TargetRef::Object(id))
        .find(|target| slot.legal_targets.contains(target))
        .or_else(|| {
            assert!(
                slot.optional,
                "no declared object is legal for a required slot: {:?}",
                slot.legal_targets
            );
            None
        })
}

/// Answers target prompts with the first declared object legal for the slot
/// (none for an optional slot with no legal declared object), answers "you
/// may" prompts with `accept`, and passes priority until the stack is empty.
fn resolve_stack(runner: &mut GameRunner, objects: &[ObjectId], accept: bool) -> Vec<GameEvent> {
    let mut events = Vec::new();
    for _ in 0..100 {
        let action = match &runner.state().waiting_for {
            WaitingFor::TargetSelection {
                target_slots,
                selection,
                ..
            }
            | WaitingFor::TriggerTargetSelection {
                target_slots,
                selection,
                ..
            } => GameAction::ChooseTarget {
                target: pick(&target_slots[selection.current_slot], objects),
            },
            WaitingFor::OptionalEffectChoice { .. } => GameAction::DecideOptionalEffect { accept },
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => return events,
            WaitingFor::Priority { .. } | WaitingFor::ManaPayment { .. } => {
                GameAction::PassPriority
            }
            other => panic!("unhandled prompt {other:?}"),
        };
        events.extend(runner.act(action).expect("action accepted").events);
    }
    panic!("the stack did not empty");
}

/// Activates `source`'s ability 0 on `foe` and returns the non-combat damage.
fn activate_on(runner: &mut GameRunner, source: ObjectId, foe: ObjectId) -> Vec<DamageEvent> {
    let outcome = runner.activate(source, 0).target_object(foe).resolve();
    noncombat_damage(outcome.events())
}

/// E-2.1. CR 120.1 + CR 120.3e: Karplusan Yeti (3/3) deals 3 to the declared
/// 5/9, then the 5/9 deals ITS power (5) to the Yeti.
#[test]
fn karplusan_yeti_declared_creature_deals_the_fight_back_damage() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let yeti = scenario
        .add_creature_from_oracle(P0, "Karplusan Yeti", 3, 3, KARPLUSAN_YETI)
        .id();
    let foe = scenario.add_creature(P1, "Hulking Brute", 5, 9).id();
    let mut runner = scenario.build();

    let damage = activate_on(&mut runner, yeti, foe);
    assert_eq!(
        damage,
        vec![
            (yeti, TargetRef::Object(foe), 3),
            (foe, TargetRef::Object(yeti), 5)
        ]
    );
}

/// Shared board for the activated fight-back cards: the source on P0 activates
/// on an opponent's 1/9, whose 1 damage back is marked on the source
/// (CR 120.3e).
fn fight_back_row(
    name: &str,
    power: i32,
    toughness: i32,
    keywords: &[&str],
    text: &str,
    colors: &[ManaType],
) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_creature(P0, name, power, toughness)
        .from_oracle_text_with_keywords(keywords, text)
        .id();
    let foe = scenario.add_creature(P1, "Hulking Brute", 1, 9).id();
    let mut runner = scenario.build();
    mana(&mut runner, colors);

    let damage = activate_on(&mut runner, source, foe);
    assert_eq!(
        damage,
        vec![
            (source, TargetRef::Object(foe), power as u32),
            (foe, TargetRef::Object(source), 1)
        ],
        "{name}: the declared creature deals its power back to the source"
    );
    assert_eq!(damage_marked(&runner, source), 1, "{name}");
}

/// E-2.3. Gargantuan Gorilla (7/7), "another target creature".
#[test]
fn gargantuan_gorilla_declared_creature_deals_the_fight_back_damage() {
    fight_back_row("Gargantuan Gorilla", 7, 7, &[], GARGANTUAN_GORILLA, &[]);
}

/// E-2.4. Tahngarth, Talruum Hero (4/4): "That creature deals damage equal to
/// its power to Tahngarth" (CR 201.5).
#[test]
fn tahngarth_declared_creature_deals_the_fight_back_damage() {
    fight_back_row(
        "Tahngarth, Talruum Hero",
        4,
        4,
        &["Vigilance"],
        TAHNGARTH,
        &[ManaType::Red, ManaType::Red],
    );
}

/// E-2.5. Tracker (2/2).
#[test]
fn tracker_declared_creature_deals_the_fight_back_damage() {
    fight_back_row(
        "Tracker",
        2,
        2,
        &[],
        TRACKER,
        &[ManaType::Green, ManaType::Green],
    );
}

/// E-2.6. Vein Drinker (4/4).
#[test]
fn vein_drinker_declared_creature_deals_the_fight_back_damage() {
    fight_back_row(
        "Vein Drinker",
        4,
        4,
        &["Flying"],
        VEIN_DRINKER,
        &[ManaType::Red],
    );
}

/// E-2.2. Durkwood Tracker (4/3) targets an attacking creature: P0's own 2/9
/// attacks, and in the declare-attackers step the Tracker's ability makes the
/// attacker deal ITS power (2) to the Tracker.
#[test]
fn durkwood_tracker_declared_attacker_deals_the_fight_back_damage() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let tracker = scenario
        .add_creature_from_oracle(P0, "Durkwood Tracker", 4, 3, DURKWOOD_TRACKER)
        .id();
    let attacker = scenario.add_creature(P0, "Hulking Brute", 2, 9).id();
    let mut runner = scenario.build();
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(attacker, AttackTarget::Player(P1))])
        .expect("declaring the attack must succeed");
    resolve_stack(&mut runner, &[], true);
    mana(&mut runner, &[ManaType::Green, ManaType::Green]);

    let damage = activate_on(&mut runner, tracker, attacker);
    assert_eq!(
        damage,
        vec![
            (tracker, TargetRef::Object(attacker), 4),
            (attacker, TargetRef::Object(tracker), 2)
        ]
    );
    assert_eq!(damage_marked(&runner, tracker), 2);
}

/// E-2.7. CR 120.1 + CR 608.2c: Stalking Yeti (3/3) enters; its trigger deals 3
/// to the declared 2/9, then the 2/9 deals ITS power (2) to the Yeti.
#[test]
fn stalking_yeti_declared_creature_deals_the_fight_back_damage() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let yeti = scenario
        .add_creature_to_hand_from_oracle(P0, "Stalking Yeti", 3, 3, STALKING_YETI)
        .id();
    let foe = scenario.add_creature(P1, "Hulking Brute", 2, 9).id();
    let mut runner = scenario.build();

    let outcome = runner.cast(yeti).target_object(foe).resolve();
    assert_eq!(
        noncombat_damage(outcome.events()),
        vec![
            (yeti, TargetRef::Object(foe), 3),
            (foe, TargetRef::Object(yeti), 2)
        ]
    );
    assert_eq!(damage_marked(&runner, yeti), 2);
}

/// E-2.8. CR 120.1 + CR 120.3a: Form of the Dinosaur's upkeep trigger deals 15
/// to the declared 6/9, then the 6/9 deals ITS power (6) to you. Staging: the
/// enchantment is seeded onto the battlefield, so its "your life total becomes
/// 15" enters trigger (CR 119.5) never fires and the pre-read life is 20.
#[test]
fn form_of_the_dinosaur_declared_creature_deals_damage_to_you() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::Untap);
    let form = scenario
        .add_enchantment_from_oracle(P0, "Form of the Dinosaur", FORM_OF_THE_DINOSAUR)
        .id();
    let foe = scenario.add_creature(P1, "Hulking Brute", 6, 9).id();
    let mut runner = scenario.build();
    let life_before = runner.life(P0);
    assert_eq!(
        life_before, 20,
        "seeded staging: the enters trigger never set 15"
    );

    runner.advance_to_upkeep();
    let damage = noncombat_damage(&resolve_stack(&mut runner, &[foe], true));
    assert_eq!(
        damage,
        vec![
            (form, TargetRef::Object(foe), 15),
            (foe, TargetRef::Player(P0), 6)
        ]
    );
    assert_eq!(runner.life(P0), life_before - 6);
}

fn hunters_bow_board(
    objects_for: fn(ObjectId, ObjectId) -> Vec<ObjectId>,
) -> (GameRunner, Vec<DamageEvent>, ObjectId, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let bow = scenario
        .add_artifact_to_hand_from_oracle(P0, "Hunter's Bow", "")
        .with_subtypes(vec!["Equipment"])
        .from_oracle_text_with_keywords(&["Equip"], HUNTERS_BOW)
        .id();
    let mine = scenario.add_creature(P0, "Grizzly Bears", 4, 4).id();
    let foe = scenario.add_creature(P1, "Hulking Brute", 2, 9).id();
    let mut runner = scenario.build();
    let outcome = runner
        .cast(bow)
        .target_objects(&objects_for(mine, foe))
        .resolve();
    let damage = noncombat_damage(outcome.events());
    (runner, damage, bow, mine, foe)
}

/// E-2.9. CR 120.1 + CR 608.2c: Hunter's Bow attaches to the declared 4/4,
/// which deals ITS power (4) to the declared 2/9.
#[test]
fn hunters_bow_attached_creature_deals_its_power() {
    let (runner, damage, bow, mine, foe) = hunters_bow_board(|mine, foe| vec![mine, foe]);
    assert!(
        runner.state().objects[&bow].attached_to == Some(AttachTarget::Object(mine)),
        "the Bow must be attached to the declared creature"
    );
    assert_eq!(damage, vec![(mine, TargetRef::Object(foe), 4)]);
}

/// E-2.10. CR 115.6: with "up to one target creature" declared as zero targets,
/// the clause deals no damage. Reach guard: the Bow still attached.
#[test]
fn hunters_bow_with_no_second_target_deals_no_damage() {
    let (runner, damage, bow, mine, _foe) = hunters_bow_board(|mine, _foe| vec![mine]);
    assert!(
        runner.state().objects[&bow].attached_to == Some(AttachTarget::Object(mine)),
        "reach guard: the enters trigger resolved and attached the Bow"
    );
    assert_eq!(damage, vec![]);
}

/// E-2.11. CR 120.1 + CR 120.3a: Bind the Monster enchants and taps the
/// opponent's 6/9, which deals ITS power (6) to you.
#[test]
fn bind_the_monster_enchanted_creature_deals_damage_to_you() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let aura = scenario
        .add_spell_to_hand(P0, "Bind the Monster", false)
        .as_enchantment()
        .with_subtypes(vec!["Aura"])
        .from_oracle_text_with_keywords(&["Enchant"], BIND_THE_MONSTER)
        .id();
    let foe = scenario.add_creature(P1, "Hulking Brute", 6, 9).id();
    let mut runner = scenario.build();
    let life_before = runner.life(P0);

    let outcome = runner.cast(aura).target_object(foe).resolve();
    assert_eq!(
        noncombat_damage(outcome.events()),
        vec![(foe, TargetRef::Player(P0), 6)]
    );
    assert_eq!(runner.life(P0), life_before - 6);
}

/// E-2.12. CR 120.1 + CR 400.7 + CR 608.2h: Selfless Exorcist exiles a 4/4
/// creature card from a graveyard; that card deals its power (4) to the
/// Exorcist.
#[test]
fn selfless_exorcist_exiled_card_deals_the_damage() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let exorcist = scenario
        .add_creature_from_oracle(P0, "Selfless Exorcist", 3, 4, SELFLESS_EXORCIST)
        .id();
    let card = scenario
        .add_creature_to_graveyard(P1, "Graveyard Bear", 4, 4)
        .id();
    let mut runner = scenario.build();

    let damage = activate_on(&mut runner, exorcist, card);
    assert_eq!(damage, vec![(card, TargetRef::Object(exorcist), 4)]);
    assert_eq!(runner.state().objects[&card].zone, Zone::Exile);
}

/// E-2.13. Predatory Urge grants the fight-back ability to the enchanted 3/9;
/// activated on a 2/9, the 2/9 deals ITS power (2) back.
#[test]
fn predatory_urge_granted_ability_declared_creature_deals_the_fight_back_damage() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let aura = scenario
        .add_spell_to_hand(P0, "Predatory Urge", false)
        .as_enchantment()
        .with_subtypes(vec!["Aura"])
        .from_oracle_text_with_keywords(&["Enchant"], PREDATORY_URGE)
        .id();
    let host = scenario.add_creature(P0, "Grizzly Bears", 3, 9).id();
    let foe = scenario.add_creature(P1, "Hulking Brute", 2, 9).id();
    let mut runner = scenario.build();
    runner.cast(aura).target_object(host).resolve();

    let damage = activate_on(&mut runner, host, foe);
    assert_eq!(
        damage,
        vec![
            (host, TargetRef::Object(foe), 3),
            (foe, TargetRef::Object(host), 2)
        ]
    );
}

fn cyclops_gladiator_attack(accept: bool) -> (GameRunner, Vec<DamageEvent>, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let gladiator = scenario
        .add_creature_from_oracle(P0, "Cyclops Gladiator", 4, 9, CYCLOPS_GLADIATOR)
        .id();
    let foe = scenario.add_creature(P1, "Hulking Brute", 6, 9).id();
    let mut runner = scenario.build();
    runner.advance_to_combat();
    let mut events = runner
        .declare_attackers(&[(gladiator, AttackTarget::Player(P1))])
        .expect("declaring the attack must succeed")
        .events;
    events.extend(resolve_stack(&mut runner, &[foe], accept));
    let damage = noncombat_damage(&events);
    (runner, damage, gladiator, foe)
}

/// E-2.14. CR 120.1 + CR 120.3e: Cyclops Gladiator (4/9) attacks and "you may"
/// is taken: it deals 4 to the declared 6/9, then the 6/9 deals ITS power (6)
/// to the Gladiator, marked on the Gladiator.
#[test]
fn cyclops_gladiator_accepted_declared_creature_deals_damage_back() {
    let (runner, damage, gladiator, foe) = cyclops_gladiator_attack(true);
    assert_eq!(
        damage,
        vec![
            (gladiator, TargetRef::Object(foe), 4),
            (foe, TargetRef::Object(gladiator), 6)
        ]
    );
    assert_eq!(damage_marked(&runner, gladiator), 6);
}

/// E-2.15. CR 603.5: declining "you may" deals no damage at all. Reach guard:
/// the accepted row on the same board.
#[test]
fn cyclops_gladiator_declined_deals_no_damage() {
    let (_runner, damage, _gladiator, _foe) = cyclops_gladiator_attack(false);
    assert_eq!(damage, vec![]);
}

fn tap_and_backlash_row(name: &str, is_instant: bool, keywords: &[&str], text: &str) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand(P0, name, is_instant)
        .from_oracle_text_with_keywords(keywords, text)
        .id();
    let foe = scenario.add_creature(P1, "Hulking Brute", 6, 9).id();
    let mut runner = scenario.build();
    let life_before = runner.life(P1);

    let outcome = runner
        .cast(spell)
        .target_object(foe)
        .decline_optional()
        .resolve();
    assert!(
        runner.state().objects[&foe].tapped,
        "{name}: the 6/9 is tapped"
    );
    assert_eq!(
        noncombat_damage(outcome.events()),
        vec![(foe, TargetRef::Player(P1), 6)],
        "{name}: the tapped creature deals its power to its controller"
    );
    assert_eq!(runner.life(P1), life_before - 6, "{name}");
}

/// E-2.16. CR 120.1 + CR 120.3a: Backlash taps the opponent's 6/9, which deals
/// ITS power (6) to its controller.
#[test]
fn backlash_tapped_creature_deals_damage_to_its_controller() {
    tap_and_backlash_row("Backlash", true, &[], BACKLASH);
}

/// E-2.17. CR 120.1 + CR 120.3a: Traitor's Roar, same reading.
#[test]
fn traitors_roar_tapped_creature_deals_damage_to_its_controller() {
    tap_and_backlash_row("Traitor's Roar", false, &["Conspire"], TRAITORS_ROAR);
}

/// E-6.1, preservation. CR 120.1: Halana's "that creature" is the creature
/// whose entering triggered the ability, not a declared target; a 5/5 enters,
/// {2} is paid, and the 5/5 deals its power to the declared 2/9.
#[test]
fn halana_entering_creature_deals_the_damage() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_creature(P0, "Halana, Kessig Ranger", 3, 4)
        .from_oracle_text_with_keywords(&["Partner", "Reach"], HALANA);
    let entering = scenario
        .add_creature_to_hand(P0, "Charging Beast", 5, 5)
        .id();
    let foe = scenario.add_creature(P1, "Hulking Brute", 2, 9).id();
    let mut runner = scenario.build();
    mana(&mut runner, &[ManaType::Colorless, ManaType::Colorless]);

    let outcome = runner
        .cast(entering)
        .target_object(foe)
        .accept_optional()
        .resolve();
    assert_eq!(
        noncombat_damage(outcome.events()),
        vec![(entering, TargetRef::Object(foe), 5)]
    );
}
