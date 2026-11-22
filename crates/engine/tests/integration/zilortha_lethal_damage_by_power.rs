//! Zilortha, Strength Incarnate: "Lethal damage dealt to creatures you control is determined by
//! their power rather than their toughness." (CR 120.6 + CR 704.5g + CR 613.11).
//!
//! Rulings (2020-04-17) pinned here:
//! 1. A creature with 0 power isn't destroyed unless it has at least 1 damage marked on it.
//! 2. A creature with 0 toughness is put into its owner's graveyard (CR 704.5f); Zilortha
//!    doesn't affect that.
//! 3. Wherever the game checks for lethal damage — the state-based action, assigning trample
//!    damage, excess damage (Flame Spill) — power replaces toughness.
//! 4. Nonlethal damage can become lethal when Zilortha enters or leaves.
//!
//! Oracle text verified verbatim against `data/mtgjson/AtomicCards.json`: Zilortha, Strength
//! Incarnate, Lava Dart, Flame Slash, Flame Spill, Lightning Bolt, Unsummon, Disfigure, Giant
//! Growth, Act of Treason, Darksteel Myr, Rith, Liberated Primeval, Colossal Dreadmaw, Charging
//! Monstrosaur, Sedge Scorpion, War Mammoth, Archetype of Finality, Flumph, Ram Through,
//! Putrefax, Ghalta, Primal Hunger, Battle Mastery, and the textless Yoked Ox (0/4), Yargle,
//! Glutton of Urborg (9/3) and Quakestrider Ceratops (12/8). Every creature that carries a
//! keyword under test is one of these printed cards; the remaining bodies are textless P/T
//! fillers.
//!
//! No printed creature has both deathtouch and trample of its own, so the deathtouch trampler
//! is War Mammoth ("Trample") under Archetype of Finality ("Creatures you control have
//! deathtouch.").

use engine::game::combat::AttackTarget;
use engine::game::combat_damage::combat_damage_to_defender;
use engine::game::coverage::unimplemented_mechanics;
use engine::game::engine::EngineError;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{ChoiceType, ControllerRef, TargetFilter, TargetRef, TypedFilter};
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::{CombatDamageAssignmentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::{Keyword, ProtectionTarget};
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::statics::StaticMode;
use engine::types::zones::Zone;

const ZILORTHA_NAME: &str = "Zilortha, Strength Incarnate";
const ZILORTHA: &str = "Trample\nLethal damage dealt to creatures you control is determined by \
     their power rather than their toughness.";
const LAVA_DART: &str = "Lava Dart deals 1 damage to any target.\n\
     Flashback\u{2014}Sacrifice a Mountain. (You may cast this card from your graveyard for its \
     flashback cost. Then exile it.)";
const FLAME_SLASH: &str = "Flame Slash deals 4 damage to target creature.";
const FLAME_SPILL: &str = "Flame Spill deals 4 damage to target creature. Excess damage is dealt \
     to that creature's controller instead.";
const LIGHTNING_BOLT: &str = "Lightning Bolt deals 3 damage to any target.";
const UNSUMMON: &str = "Return target creature to its owner's hand.";
const DISFIGURE: &str = "Target creature gets -2/-2 until end of turn.";
const GIANT_GROWTH: &str = "Target creature gets +3/+3 until end of turn.";
const ACT_OF_TREASON: &str = "Gain control of target creature until end of turn. Untap that \
     creature. It gains haste until end of turn. (It can attack and {T} this turn.)";
const DARKSTEEL_MYR: &str = "Indestructible (Damage and effects that say \"destroy\" don't \
     destroy this creature. If its toughness is 0 or less, it still dies.)";
const COLOSSAL_DREADMAW: &str =
    "Trample (This creature can deal excess combat damage to the player or planeswalker it's attacking.)";
const CHARGING_MONSTROSAUR: &str = "Trample, haste";
const SEDGE_SCORPION: &str =
    "Deathtouch (Any amount of damage this deals to a creature is enough to destroy it.)";
const WAR_MAMMOTH: &str = "Trample";
const ARCHETYPE_OF_FINALITY: &str = "Creatures you control have deathtouch.\nCreatures your \
     opponents control lose deathtouch and can't have or gain deathtouch.";
const RITH: &str = "Flying, ward {2}\nOther Dragons you control have ward {2}.\nAt the beginning \
     of your end step, if a creature or planeswalker an opponent controlled was dealt excess \
     damage this turn, create a 4/4 red Dragon creature token with flying.";

const FLUMPH: &str = "Defender, flying\nWhenever this creature is dealt damage, you and target \
     opponent each draw a card.";
const RAM_THROUGH: &str = "Target creature you control deals damage equal to its power to target \
     creature you don't control. If the creature you control has trample, excess damage is dealt \
     to that creature's controller instead.";
const GHALTA: &str = "This spell costs {X} less to cast, where X is the total power of \
     creatures you control.\nTrample (This creature can deal excess combat damage to the player \
     or planeswalker it's attacking.)";
const BATTLE_MASTERY: &str = "Enchant creature\nEnchanted creature has double strike. (It deals \
     both first-strike and regular combat damage.)";
const PUTREFAX: &str = "Trample, haste\nInfect (This creature deals damage to creatures in the \
     form of -1/-1 counters and to players in the form of poison counters.)\nAt the beginning of \
     the end step, sacrifice this creature.";

fn board() -> GameScenario {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    scenario
}

fn zilortha(scenario: &mut GameScenario, player: PlayerId) -> ObjectId {
    scenario
        .add_creature(player, ZILORTHA_NAME, 7, 3)
        .as_legendary()
        .from_oracle_text_with_keywords(&["Trample"], ZILORTHA)
        .id()
}

/// A printed creature on the battlefield with its verbatim Oracle text and MTGJSON keywords.
fn printed(
    scenario: &mut GameScenario,
    player: PlayerId,
    name: &str,
    (power, toughness): (i32, i32),
    keywords: &[&str],
    text: &str,
) -> ObjectId {
    scenario
        .add_creature(player, name, power, toughness)
        .from_oracle_text_with_keywords(keywords, text)
        .id()
}

/// Yoked Ox: a printed, textless 0/4.
fn yoked_ox(scenario: &mut GameScenario, player: PlayerId) -> ObjectId {
    scenario.add_creature(player, "Yoked Ox", 0, 4).id()
}

/// War Mammoth ("Trample") with deathtouch from Archetype of Finality, both P0's.
fn deathtouch_trampler(scenario: &mut GameScenario) -> ObjectId {
    printed(
        scenario,
        P0,
        "Archetype of Finality",
        (2, 3),
        &[],
        ARCHETYPE_OF_FINALITY,
    );
    printed(
        scenario,
        P0,
        "War Mammoth",
        (3, 3),
        &["Trample"],
        WAR_MAMMOTH,
    )
}

/// A free copy of a spell in P0's hand.
fn spell(scenario: &mut GameScenario, name: &str, instant: bool, text: &str) -> ObjectId {
    scenario
        .add_spell_to_hand_from_oracle(P0, name, instant, text)
        .with_mana_cost(ManaCost::zero())
        .id()
}

/// Cast `spell` at `target` and resolve it to an empty stack and a priority window. Returns the
/// resolution's events.
fn cast_at(runner: &mut GameRunner, spell: ObjectId, target: ObjectId) -> Vec<GameEvent> {
    let outcome = runner.cast(spell).target_object(target).resolve();
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "the spell resolves to a priority window, got {:?}",
        outcome.final_waiting_for()
    );
    assert!(outcome.state().stack.is_empty(), "the stack is empty");
    outcome.events().to_vec()
}

fn damage_to(events: &[GameEvent], target: ObjectId) -> Vec<(u32, u32)> {
    events
        .iter()
        .filter_map(|event| match event {
            GameEvent::DamageDealt {
                target: TargetRef::Object(id),
                amount,
                excess,
                ..
            } if *id == target => Some((*amount, *excess)),
            _ => None,
        })
        .collect()
}

fn zone(runner: &GameRunner, id: ObjectId) -> Zone {
    runner.state().objects[&id].zone
}

fn marked(runner: &GameRunner, id: ObjectId) -> u32 {
    runner.state().objects[&id].damage_marked
}

/// Attack P1 with `attackers`, declare `blocks` (`(blocker, attacker)`), then pass priority until
/// combat damage is being assigned (a prompt is up) or has been dealt.
fn attack_and_block(
    runner: &mut GameRunner,
    attackers: &[ObjectId],
    blocks: &[(ObjectId, ObjectId)],
) {
    let mut declared_attackers = false;
    let mut declared_blockers = false;
    for _ in 0..64 {
        match &runner.state().waiting_for {
            WaitingFor::AssignCombatDamage { .. } => break,
            WaitingFor::Priority { .. } if runner.state().phase == Phase::CombatDamage => break,
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            WaitingFor::DeclareAttackers { player, .. } => {
                assert_eq!(*player, P0);
                assert!(!declared_attackers, "attackers are declared once");
                declared_attackers = true;
                let attacks: Vec<_> = attackers
                    .iter()
                    .map(|&id| (id, AttackTarget::Player(P1)))
                    .collect();
                runner
                    .declare_attackers(&attacks)
                    .expect("declare attackers");
            }
            WaitingFor::DeclareBlockers { player, .. } => {
                assert_eq!(*player, P1);
                assert!(!declared_blockers, "blockers are declared once");
                declared_blockers = true;
                runner.declare_blockers(blocks).expect("declare blockers");
            }
            other => panic!("unexpected prompt before combat damage: {other:?}"),
        }
    }
    assert!(
        declared_attackers && declared_blockers,
        "combat must reach the declared block"
    );
}

/// The lethal minimum the open trample prompt publishes for `blocker`.
fn prompt_minimum(runner: &GameRunner, blocker: ObjectId) -> u32 {
    let WaitingFor::AssignCombatDamage {
        player,
        blockers,
        trample,
        ..
    } = &runner.state().waiting_for
    else {
        panic!(
            "expected a combat damage assignment prompt, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(*player, P0);
    assert!(trample.is_some(), "the prompted attacker tramples");
    blockers
        .iter()
        .find(|slot| slot.blocker_id == blocker)
        .expect("the blocker is a slot of the prompt")
        .lethal_minimum
}

fn assign(
    runner: &mut GameRunner,
    blocker: ObjectId,
    to_blocker: u32,
    trample_damage: u32,
) -> Result<(), EngineError> {
    runner
        .act(GameAction::AssignCombatDamage {
            mode: CombatDamageAssignmentMode::Normal,
            assignments: vec![(blocker, to_blocker)],
            trample_damage,
            controller_damage: 0,
        })
        .map(|_| ())
}

fn finish_combat(runner: &mut GameRunner) {
    runner.combat_damage();
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "combat finishes at a priority window, got {:?}",
        runner.state().waiting_for
    );
    assert!(runner.state().stack.is_empty());
}

/// Z0 (SHAPE + coverage): the static line parses to the mode with the "creatures you control"
/// filter, and a scenario-built Zilortha reports no unimplemented mechanic.
#[test]
fn zilortha_static_parses_and_is_supported() {
    let parsed = parse_oracle_text(
        ZILORTHA,
        ZILORTHA_NAME,
        &["Trample".to_owned()],
        &["Creature".to_owned()],
        &["Dinosaur".to_owned()],
    );
    assert!(parsed.abilities.is_empty(), "{:?}", parsed.abilities);
    assert!(parsed.triggers.is_empty());
    assert_eq!(parsed.statics.len(), 1, "{:?}", parsed.statics);
    let def = &parsed.statics[0];
    assert_eq!(def.mode, StaticMode::LethalDamageDeterminedByPower);
    assert_eq!(
        def.affected,
        Some(TargetFilter::from(
            TypedFilter::creature().controller(ControllerRef::You)
        ))
    );

    let mut scenario = board();
    let z = zilortha(&mut scenario, P0);
    let runner = scenario.build();
    let obj = &runner.state().objects[&z];
    assert!(
        obj.static_definitions
            .iter_all()
            .any(|d| d.mode == StaticMode::LethalDamageDeterminedByPower),
        "reach guard: the scenario-built Zilortha carries the static"
    );
    assert!(
        unimplemented_mechanics(obj).is_empty(),
        "{:?}",
        unimplemented_mechanics(obj)
    );
}

/// Z1: a 1/5 I control dies to 1 damage (power 1).
#[test]
fn my_one_five_dies_to_one_damage() {
    let mut scenario = board();
    zilortha(&mut scenario, P0);
    let mine = scenario.add_creature(P0, "One Five", 1, 5).id();
    let dart = spell(&mut scenario, "Lava Dart", true, LAVA_DART);
    let mut runner = scenario.build();

    let events = cast_at(&mut runner, dart, mine);
    assert_eq!(damage_to(&events, mine), vec![(1, 0)]);
    assert_eq!(zone(&runner, mine), Zone::Graveyard);
}

/// Z2: a 5/1 I control survives 4 damage, and dies once Zilortha leaves (ruling 4).
#[test]
fn my_five_one_survives_four_until_zilortha_leaves() {
    let mut scenario = board();
    let z = zilortha(&mut scenario, P0);
    let mine = scenario.add_creature(P0, "Five One", 5, 1).id();
    let slash = spell(&mut scenario, "Flame Slash", false, FLAME_SLASH);
    let unsummon = spell(&mut scenario, "Unsummon", true, UNSUMMON);
    let mut runner = scenario.build();

    cast_at(&mut runner, slash, mine);
    assert_eq!(zone(&runner, mine), Zone::Battlefield);
    assert_eq!(marked(&runner, mine), 4);

    cast_at(&mut runner, unsummon, z);
    assert_eq!(zone(&runner, z), Zone::Hand);
    assert_eq!(
        zone(&runner, mine),
        Zone::Graveyard,
        "CR 704.5g: 4 damage on toughness 1 is lethal again"
    );
}

/// Z3: opponents' creatures are unaffected.
#[test]
fn opponents_creatures_keep_toughness() {
    let mut scenario = board();
    zilortha(&mut scenario, P0);
    let mine = scenario.add_creature(P0, "Mine", 1, 5).id();
    let theirs = scenario.add_creature(P1, "Theirs", 1, 5).id();
    let dart_a = spell(&mut scenario, "Lava Dart", true, LAVA_DART);
    let dart_b = spell(&mut scenario, "Lava Dart", true, LAVA_DART);
    let mut runner = scenario.build();

    cast_at(&mut runner, dart_a, theirs);
    assert_eq!(zone(&runner, theirs), Zone::Battlefield);
    assert_eq!(marked(&runner, theirs), 1);
    cast_at(&mut runner, dart_b, mine);
    assert_eq!(zone(&runner, mine), Zone::Graveyard);
}

/// Z4: an opponent's trampler must assign lethal damage to my blocker by its power (ruling 3).
#[test]
fn opponents_trampler_assigns_lethal_by_my_blockers_power() {
    let mut scenario = board();
    zilortha(&mut scenario, P1);
    let blocker = scenario.add_creature(P1, "One Five", 1, 5).id();
    let attacker = printed(
        &mut scenario,
        P0,
        "Colossal Dreadmaw",
        (6, 6),
        &["Trample"],
        COLOSSAL_DREADMAW,
    );
    let mut runner = scenario.build();
    let p1_life = runner.life(P1);

    attack_and_block(&mut runner, &[attacker], &[(blocker, attacker)]);
    assert_eq!(prompt_minimum(&runner, blocker), 1);
    assert!(
        assign(&mut runner, blocker, 0, 6).is_err(),
        "CR 702.19b: excess before lethal is rejected"
    );
    assign(&mut runner, blocker, 1, 5).expect("1 is lethal for a 1-power creature");
    finish_combat(&mut runner);
    assert_eq!(runner.life(P1), p1_life - 5);
    assert_eq!(zone(&runner, blocker), Zone::Graveyard);
}

/// Z4b: Zilortha on the attacking side does not change the opponent's blocker.
#[test]
fn attacker_side_zilortha_leaves_the_blocker_on_toughness() {
    let mut scenario = board();
    zilortha(&mut scenario, P0);
    let attacker = printed(
        &mut scenario,
        P0,
        "Colossal Dreadmaw",
        (6, 6),
        &["Trample"],
        COLOSSAL_DREADMAW,
    );
    let blocker = scenario.add_creature(P1, "One Five", 1, 5).id();
    let mut runner = scenario.build();

    attack_and_block(&mut runner, &[attacker], &[(blocker, attacker)]);
    assert_eq!(prompt_minimum(&runner, blocker), 5);
}

/// Z5: ruling 1 — a 0-power creature I control (Yoked Ox) survives with no damage marked, and
/// direct positive damage destroys it (its lethal damage is 0; CR 704.5g needs damage marked).
#[test]
fn zero_power_creature_needs_one_damage() {
    let mut scenario = board();
    zilortha(&mut scenario, P0);
    let wall = yoked_ox(&mut scenario, P0);
    let mine = scenario.add_creature(P0, "One Five", 1, 5).id();
    let dart_a = spell(&mut scenario, "Lava Dart", true, LAVA_DART);
    let dart_b = spell(&mut scenario, "Lava Dart", true, LAVA_DART);
    let mut runner = scenario.build();

    cast_at(&mut runner, dart_a, mine);
    assert_eq!(zone(&runner, mine), Zone::Graveyard);
    // Reach guard: SBAs ran after the first Dart, and the undamaged 0-power creature survived.
    assert_eq!(zone(&runner, wall), Zone::Battlefield);
    assert_eq!(marked(&runner, wall), 0);
    cast_at(&mut runner, dart_b, wall);
    assert_eq!(zone(&runner, wall), Zone::Graveyard);
}

/// Z6: deathtouch is unchanged (CR 704.5h).
#[test]
fn deathtouch_still_destroys_my_creature() {
    let mut scenario = board();
    zilortha(&mut scenario, P0);
    let attacker = scenario.add_creature(P0, "Five Five", 5, 5).id();
    let blocker = printed(
        &mut scenario,
        P1,
        "Sedge Scorpion",
        (1, 1),
        &["Deathtouch"],
        SEDGE_SCORPION,
    );
    let mut runner = scenario.build();

    attack_and_block(&mut runner, &[attacker], &[(blocker, attacker)]);
    finish_combat(&mut runner);
    assert_eq!(zone(&runner, attacker), Zone::Graveyard);
    assert_eq!(zone(&runner, blocker), Zone::Graveyard);
}

/// Z7: ruling 3 — Flame Spill's excess is measured against my creature's power.
#[test]
fn flame_spill_excess_reads_power() {
    let mut scenario = board();
    zilortha(&mut scenario, P0);
    let mine = scenario.add_creature(P0, "Mine", 1, 5).id();
    let theirs = scenario.add_creature(P1, "Theirs", 1, 5).id();
    let spill_a = spell(&mut scenario, "Flame Spill", true, FLAME_SPILL);
    let spill_b = spell(&mut scenario, "Flame Spill", true, FLAME_SPILL);
    let mut runner = scenario.build();
    let p0_life = runner.life(P0);
    let p1_life = runner.life(P1);

    let events = cast_at(&mut runner, spill_a, mine);
    assert_eq!(runner.life(P0), p0_life - 3, "CR 120.4a: 3 excess to me");
    assert_eq!(damage_to(&events, mine), vec![(1, 0)]);
    assert_eq!(zone(&runner, mine), Zone::Graveyard);

    cast_at(&mut runner, spill_b, theirs);
    assert_eq!(runner.life(P1), p1_life, "4 is not lethal for toughness 5");
    assert_eq!(zone(&runner, theirs), Zone::Battlefield);
    assert_eq!(marked(&runner, theirs), 4);
}

/// Z8: "you" is Zilortha's current controller (CR 109.5). After P0 takes Zilortha, P1's
/// creatures read toughness and P0's read power.
#[test]
fn control_change_moves_the_effect() {
    let mut scenario = board();
    let z = zilortha(&mut scenario, P1);
    let theirs = scenario.add_creature(P1, "Theirs", 1, 5).id();
    let mine = scenario.add_creature(P0, "Mine", 1, 5).id();
    let treason = spell(&mut scenario, "Act of Treason", false, ACT_OF_TREASON);
    let dart_a = spell(&mut scenario, "Lava Dart", true, LAVA_DART);
    let dart_b = spell(&mut scenario, "Lava Dart", true, LAVA_DART);
    let mut runner = scenario.build();

    cast_at(&mut runner, treason, z);
    assert_eq!(runner.state().objects[&z].controller, P0);
    cast_at(&mut runner, dart_a, theirs);
    assert_eq!(zone(&runner, theirs), Zone::Battlefield);
    cast_at(&mut runner, dart_b, mine);
    assert_eq!(zone(&runner, mine), Zone::Graveyard);
}

/// Z9: Zilortha applies to itself (7 power).
#[test]
fn zilortha_survives_four_damage() {
    let mut scenario = board();
    let z = zilortha(&mut scenario, P0);
    let slash = spell(&mut scenario, "Flame Slash", false, FLAME_SLASH);
    let mut runner = scenario.build();

    cast_at(&mut runner, slash, z);
    assert_eq!(zone(&runner, z), Zone::Battlefield);
    assert_eq!(marked(&runner, z), 4);
}

/// Z10: ruling 2 — 0 toughness still kills (CR 704.5f), with no damage involved.
#[test]
fn zero_toughness_still_dies() {
    let mut scenario = board();
    zilortha(&mut scenario, P0);
    let mine = scenario.add_creature(P0, "Five Two", 5, 2).id();
    let disfigure = spell(&mut scenario, "Disfigure", true, DISFIGURE);
    let mut runner = scenario.build();

    cast_at(&mut runner, disfigure, mine);
    assert_eq!(zone(&runner, mine), Zone::Graveyard);
    assert_eq!(marked(&runner, mine), 0);
}

/// Z11: ruling 4 — damage already marked becomes lethal when Zilortha enters.
#[test]
fn marked_damage_becomes_lethal_when_zilortha_enters() {
    let mut scenario = board();
    let mine = scenario
        .add_creature(P0, "One Five", 1, 5)
        .with_damage_marked(2)
        .id();
    let z = scenario
        .add_creature_to_hand(P0, ZILORTHA_NAME, 7, 3)
        .as_legendary()
        .with_mana_cost(ManaCost::zero())
        .from_oracle_text_with_keywords(&["Trample"], ZILORTHA)
        .id();
    let mut runner = scenario.build();
    assert_eq!(
        zone(&runner, mine),
        Zone::Battlefield,
        "reach guard: alive before Zilortha"
    );

    let outcome = runner.cast(z).resolve();
    assert!(matches!(
        outcome.final_waiting_for(),
        WaitingFor::Priority { .. }
    ));
    assert_eq!(zone(&runner, z), Zone::Battlefield);
    assert_eq!(zone(&runner, mine), Zone::Graveyard);
}

/// Z12: CR 120.10 — the excess-damage record reads power, and a trigger that reads it fires.
/// Aegar, the Freezing Flame's trigger does not parse yet (its intervening-if is unrecognized),
/// so the reader is Rith, Liberated Primeval's end-step check.
#[test]
fn excess_damage_by_power_is_recorded_for_excess_readers() {
    let mut scenario = board();
    let rith = scenario
        .add_creature(P0, "Rith, Liberated Primeval", 5, 5)
        .as_legendary()
        .with_subtypes(vec!["Dragon"])
        .from_oracle_text_with_keywords(&["Flying", "Ward"], RITH)
        .id();
    zilortha(&mut scenario, P1);
    let theirs = scenario.add_creature(P1, "Two Four", 2, 4).id();
    let bolt = spell(&mut scenario, "Lightning Bolt", true, LIGHTNING_BOLT);
    let mut runner = scenario.build();

    let events = cast_at(&mut runner, bolt, theirs);
    assert_eq!(
        damage_to(&events, theirs),
        vec![(3, 1)],
        "CR 120.4a: 1 of 3 is excess"
    );
    assert_eq!(zone(&runner, theirs), Zone::Graveyard);

    // Skip combat, then let Rith's end-step trigger resolve.
    runner.advance_to_combat();
    runner.declare_attackers(&[]).expect("declare no attackers");
    runner.advance_to_end_step();
    runner.advance_until_stack_empty();
    assert_eq!(runner.state().phase, Phase::End);
    let dragons: Vec<_> = runner
        .state()
        .battlefield
        .iter()
        .filter_map(|id| runner.state().objects.get(id))
        .filter(|obj| obj.id != rith && obj.name == "Dragon" && obj.controller == P0)
        .map(|obj| (obj.power, obj.toughness))
        .collect();
    assert_eq!(
        dragons,
        vec![(Some(4), Some(4))],
        "Rith makes one 4/4 Dragon"
    );
}

/// Z13: CR 120.4a with power substituted — an undamaged Yoked Ox I control has lethal damage 0,
/// so all 4 of Flame Spill's damage is excess: none is dealt to the Ox, 4 is dealt to me, and
/// the Ox survives. CR 120.8: the Ox is dealt no damage, so no damage event names it.
#[test]
fn flame_spill_on_zero_power_creature_is_all_excess() {
    let mut scenario = board();
    zilortha(&mut scenario, P0);
    let ox = yoked_ox(&mut scenario, P0);
    let spill = spell(&mut scenario, "Flame Spill", true, FLAME_SPILL);
    let mut runner = scenario.build();
    let p0_life = runner.life(P0);

    let events = cast_at(&mut runner, spill, ox);
    assert_eq!(runner.life(P0), p0_life - 4);
    assert!(
        damage_to(&events, ox).is_empty(),
        "no damage is dealt to the Ox: {:?}",
        damage_to(&events, ox)
    );
    assert!(
        !runner
            .state()
            .damage_dealt_this_turn
            .iter()
            .any(|record| record.target == TargetRef::Object(ox)),
        "CR 120.8: the Ox has no damage record"
    );
    assert_eq!(zone(&runner, ox), Zone::Battlefield);
    assert_eq!(marked(&runner, ox), 0);
}

/// Z13b: CR 702.19b with power substituted — Charging Monstrosaur blocked by an undamaged Yoked
/// Ox under the defender's Zilortha owes the Ox nothing and may trample all 5 over.
#[test]
fn trampler_owes_a_zero_power_blocker_nothing() {
    let mut scenario = board();
    zilortha(&mut scenario, P1);
    let ox = yoked_ox(&mut scenario, P1);
    let attacker = printed(
        &mut scenario,
        P0,
        "Charging Monstrosaur",
        (5, 5),
        &["Trample", "Haste"],
        CHARGING_MONSTROSAUR,
    );
    let mut runner = scenario.build();
    let p1_life = runner.life(P1);

    attack_and_block(&mut runner, &[attacker], &[(ox, attacker)]);
    assert_eq!(prompt_minimum(&runner, ox), 0);
    assign(&mut runner, ox, 0, 5).expect("the 0-power Ox is owed no lethal damage");
    finish_combat(&mut runner);
    assert_eq!(runner.life(P1), p1_life - 5);
    assert_eq!(zone(&runner, ox), Zone::Battlefield);
    assert_eq!(marked(&runner, ox), 0);
}

/// Z13c: CR 702.2c only adds a way to be lethal; it never raises the requirement above the
/// ordinary 0. A deathtouch trampler blocked by the same Ox also owes it nothing.
#[test]
fn deathtouch_trampler_owes_a_zero_power_blocker_nothing() {
    let mut scenario = board();
    zilortha(&mut scenario, P1);
    let ox = yoked_ox(&mut scenario, P1);
    let attacker = deathtouch_trampler(&mut scenario);
    let mut runner = scenario.build();
    let p1_life = runner.life(P1);

    attack_and_block(&mut runner, &[attacker], &[(ox, attacker)]);
    assert_eq!(prompt_minimum(&runner, ox), 0);
    assign(&mut runner, ox, 0, 3).expect("the 0-power Ox is owed no lethal damage");
    finish_combat(&mut runner);
    assert_eq!(runner.life(P1), p1_life - 3);
    assert_eq!(zone(&runner, ox), Zone::Battlefield);
}

/// Z14: already lethal under power before deathtouch (CR 702.19b). Giant Growth makes the Myr
/// 3/4; Lightning Bolt marks 3, lethal by power 3 but not by toughness 4, and indestructible
/// keeps it alive. A deathtouch trampler (War Mammoth under Archetype of Finality) then owes it
/// nothing.
#[test]
fn already_lethal_by_power_owes_deathtouch_nothing() {
    let mut scenario = board();
    zilortha(&mut scenario, P1);
    let myr = scenario
        .add_creature(P1, "Darksteel Myr", 0, 1)
        .as_artifact_creature()
        .from_oracle_text_with_keywords(&["Indestructible"], DARKSTEEL_MYR)
        .id();
    let attacker = deathtouch_trampler(&mut scenario);
    let growth = spell(&mut scenario, "Giant Growth", true, GIANT_GROWTH);
    let bolt = spell(&mut scenario, "Lightning Bolt", true, LIGHTNING_BOLT);
    let mut runner = scenario.build();

    cast_at(&mut runner, growth, myr);
    cast_at(&mut runner, bolt, myr);
    assert_eq!(zone(&runner, myr), Zone::Battlefield, "CR 702.12b");
    assert_eq!(marked(&runner, myr), 3);
    let p1_life = runner.life(P1);

    attack_and_block(&mut runner, &[attacker], &[(myr, attacker)]);
    assert_eq!(prompt_minimum(&runner, myr), 0);
    assign(&mut runner, myr, 0, 3).expect("the already-lethal Myr is owed nothing");
    finish_combat(&mut runner);
    assert_eq!(runner.life(P1), p1_life - 3);
    assert_eq!(zone(&runner, myr), Zone::Battlefield);
}

/// A board with P0's Flumph (0/4), P0's Zilortha when `with_zilortha`, a free `spell` in P0's
/// hand, and a card on top of each library.
fn flumph_board(with_zilortha: bool, name: &str, text: &str) -> (GameRunner, ObjectId, ObjectId) {
    let mut scenario = board();
    if with_zilortha {
        zilortha(&mut scenario, P0);
    }
    let flumph = printed(
        &mut scenario,
        P0,
        "Flumph",
        (0, 4),
        &["Defender", "Flying"],
        FLUMPH,
    );
    let spell = spell(&mut scenario, name, true, text);
    scenario.with_library_top(P0, &["Island"]);
    scenario.with_library_top(P1, &["Island"]);
    (scenario.build(), flumph, spell)
}

/// Z16: CR 120.4a + CR 120.8 — under P0's Zilortha, Flame Spill sends all 4 damage past P0's
/// undamaged Flumph to P0. Flumph is dealt no damage, so "whenever this creature is dealt damage"
/// does not trigger and nobody draws. Reach guard: without Zilortha, Lava Dart's 1 damage to the
/// same Flumph triggers it and both players draw.
#[test]
fn fully_redirected_damage_does_not_trigger_dealt_damage() {
    let (mut runner, flumph, dart) = flumph_board(false, "Lava Dart", LAVA_DART);
    let outcome = runner.cast(dart).target_object(flumph).resolve();
    assert!(matches!(
        outcome.final_waiting_for(),
        WaitingFor::Priority { .. }
    ));
    assert!(outcome.state().stack.is_empty());
    assert_eq!(damage_to(outcome.events(), flumph), vec![(1, 0)]);
    outcome.assert_hand_drawn(P0, 1);
    outcome.assert_hand_drawn(P1, 1);

    let (mut runner, flumph, spill) = flumph_board(true, "Flame Spill", FLAME_SPILL);
    let outcome = runner.cast(spill).target_object(flumph).resolve();
    assert!(matches!(
        outcome.final_waiting_for(),
        WaitingFor::Priority { .. }
    ));
    assert!(outcome.state().stack.is_empty());
    outcome.assert_life_delta(P0, -4);
    outcome.assert_hand_drawn(P0, 0);
    outcome.assert_hand_drawn(P1, 0);
    assert!(
        damage_to(outcome.events(), flumph).is_empty(),
        "CR 120.8: Flumph is dealt no damage: {:?}",
        damage_to(outcome.events(), flumph)
    );
    outcome.assert_zone(&[flumph], Zone::Battlefield);
}

/// Z17: the counter-form leg of Z16 (CR 120.3b + CR 120.3d + CR 702.90c). Ram Through from
/// Putrefax ("Trample, haste / Infect") at P1's undamaged Yoked Ox under P1's Zilortha: the Ox's
/// lethal damage is 0, so all 5 damage is excess and goes to P1 as poison counters. The Ox gets
/// no -1/-1 counters and is dealt no damage (CR 120.8).
#[test]
fn fully_redirected_infect_damage_deals_the_creature_nothing() {
    let mut scenario = board();
    let putrefax = printed(
        &mut scenario,
        P0,
        "Putrefax",
        (5, 3),
        &["Haste", "Infect", "Trample"],
        PUTREFAX,
    );
    zilortha(&mut scenario, P1);
    let ox = yoked_ox(&mut scenario, P1);
    let ram = spell(&mut scenario, "Ram Through", true, RAM_THROUGH);
    let mut runner = scenario.build();

    let outcome = runner.cast(ram).target_objects(&[putrefax, ox]).resolve();
    assert!(matches!(
        outcome.final_waiting_for(),
        WaitingFor::Priority { .. }
    ));
    assert!(outcome.state().stack.is_empty());
    let p1 = outcome
        .state()
        .players
        .iter()
        .find(|player| player.id == P1)
        .expect("P1 exists");
    assert_eq!(
        p1.poison_counters, 5,
        "CR 120.3b: all 5 is excess, as poison"
    );
    outcome.assert_zone(&[ox], Zone::Battlefield);
    assert!(
        !outcome.events().iter().any(|event| matches!(
            event,
            GameEvent::CounterAdded { object_id, .. } if *object_id == ox
        )),
        "the Ox gets no counters"
    );
    assert!(
        damage_to(outcome.events(), ox).is_empty(),
        "CR 120.8: the Ox is dealt no damage: {:?}",
        damage_to(outcome.events(), ox)
    );
}

/// P0's Ghalta, Primal Hunger (12/12, "Trample") wearing Battle Mastery, cast onto it from P0's
/// hand, against P1's Zilortha and `partner` (a textless P1 creature). Returns the runner,
/// Ghalta, Zilortha and the partner.
fn ghalta_board(
    partner: (&str, i32, i32),
    defender: impl FnOnce(&mut GameScenario),
) -> (GameRunner, ObjectId, ObjectId, ObjectId) {
    let mut scenario = board();
    let ghalta = scenario
        .add_creature(P0, "Ghalta, Primal Hunger", 12, 12)
        .as_legendary()
        .from_oracle_text_with_keywords(&["Trample"], GHALTA)
        .with_color(vec![ManaColor::Green])
        .id();
    let mastery = scenario
        .add_spell_to_hand(P0, "Battle Mastery", false)
        .as_enchantment()
        .with_subtypes(vec!["Aura"])
        .with_mana_cost(ManaCost::zero())
        .from_oracle_text_with_keywords(&["Enchant"], BATTLE_MASTERY)
        .id();
    let z = zilortha(&mut scenario, P1);
    let (name, power, toughness) = partner;
    let partner = scenario.add_creature(P1, name, power, toughness).id();
    defender(&mut scenario);
    let mut runner = scenario.build();

    cast_at(&mut runner, mastery, ghalta);
    assert_eq!(zone(&runner, mastery), Zone::Battlefield);
    assert!(
        runner.state().objects[&ghalta].has_keyword(&Keyword::DoubleStrike),
        "reach guard: Battle Mastery gives Ghalta double strike"
    );
    (runner, ghalta, z, partner)
}

fn assign_split(runner: &mut GameRunner, assignments: Vec<(ObjectId, u32)>, trample_damage: u32) {
    runner
        .act(GameAction::AssignCombatDamage {
            mode: CombatDamageAssignmentMode::Normal,
            assignments,
            trample_damage,
            controller_damage: 0,
        })
        .expect("the assignment is legal");
}

/// Z18: CR 704.3 + CR 611.3b + CR 702.19d. Ghalta with Battle Mastery is blocked by Zilortha and
/// Yargle (9/3). In the first-strike step it assigns 7 to Zilortha (lethal by its power 7) and 5
/// to Yargle (not lethal by its power 9). Zilortha dies; its effect ends, so the repeated
/// state-based-action check finds Yargle's 5 damage lethal against toughness 3 and destroys it.
/// No creature is blocking in the regular step, so all 12 go to P1. The combat forecast, read
/// on the declared block before any damage, agrees: 12.
#[test]
fn first_strike_death_of_zilortha_returns_survivors_to_toughness() {
    let (mut runner, ghalta, z, yargle) = ghalta_board(("Yargle, Glutton of Urborg", 9, 3), |_| {});
    let p1_life = runner.life(P1);

    attack_and_block(&mut runner, &[ghalta], &[(z, ghalta), (yargle, ghalta)]);
    assert_eq!(
        prompt_minimum(&runner, z),
        7,
        "Zilortha's lethal damage is its power"
    );
    assert_eq!(prompt_minimum(&runner, yargle), 9, "so is Yargle's");
    assert_eq!(
        combat_damage_to_defender(runner.state(), ghalta, &[z, yargle]),
        12,
        "the forecast drops both first-step deaths"
    );

    assign_split(&mut runner, vec![(z, 7), (yargle, 5)], 0);
    // Through the first-strike step's state-based actions, to the next prompt or the end of
    // combat.
    runner.combat_damage();
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "no regular-step prompt: nothing is blocking Ghalta, got {:?}",
        runner.state().waiting_for
    );
    assert_eq!(zone(&runner, z), Zone::Graveyard);
    assert_eq!(
        zone(&runner, yargle),
        Zone::Graveyard,
        "CR 704.3: 5 damage on toughness 3 is lethal once Zilortha is gone"
    );
    assert_eq!(runner.life(P1), p1_life - 12);
    assert_eq!(zone(&runner, ghalta), Zone::Battlefield);
}

/// Z18b: the survivor control. Against Zilortha and Quakestrider Ceratops (12/8) the first step
/// assigns 7 to Zilortha and 5 to the Ceratops. Zilortha dies; the Ceratops's 5 damage is not
/// lethal against toughness 8, so it survives and still blocks. In the regular step Ghalta owes
/// it 8 - 5 = 3 (its toughness, not its power 12 less 5) and tramples 9 over. The forecast
/// agrees: 9.
#[test]
fn first_strike_survivor_owes_its_toughness_after_zilortha_dies() {
    let (mut runner, ghalta, z, ceratops) = ghalta_board(("Quakestrider Ceratops", 12, 8), |_| {});
    let p1_life = runner.life(P1);

    attack_and_block(&mut runner, &[ghalta], &[(z, ghalta), (ceratops, ghalta)]);
    assert_eq!(prompt_minimum(&runner, z), 7);
    assert_eq!(prompt_minimum(&runner, ceratops), 12);
    assert_eq!(
        combat_damage_to_defender(runner.state(), ghalta, &[z, ceratops]),
        9,
        "the forecast re-derives the survivor's remainder against toughness"
    );

    assign_split(&mut runner, vec![(z, 7), (ceratops, 5)], 0);
    // Through the first-strike step's state-based actions, to the next prompt or the end of
    // combat.
    runner.combat_damage();
    assert_eq!(zone(&runner, z), Zone::Graveyard);
    assert_eq!(zone(&runner, ceratops), Zone::Battlefield);
    assert_eq!(marked(&runner, ceratops), 5);
    assert_eq!(
        prompt_minimum(&runner, ceratops),
        3,
        "CR 702.19b: lethal damage counts the 5 marked, against toughness 8"
    );
    assign(&mut runner, ceratops, 3, 9).expect("3 is lethal for the Ceratops now");
    finish_combat(&mut runner);
    assert_eq!(runner.life(P1), p1_life - 9);
    assert_eq!(zone(&runner, ceratops), Zone::Graveyard);
}

#[test]
fn zilortha_protected_after_blocks_stays_in_double_strike_forecast() {
    let mut mother = None;
    let (mut runner, ghalta, z, yargle) = ghalta_board(
        ("Yargle, Glutton of Urborg", 9, 3),
        |scenario| {
            mother = Some(
                scenario
                    .add_creature_from_oracle(
                        P1,
                        "Mother of Runes",
                        1,
                        1,
                        "{T}: Target creature you control gains protection from the color of your choice until end of turn.",
                    )
                    .with_color(vec![ManaColor::White])
                    .id(),
            );
        },
    );
    let mother = mother.expect("Mother of Runes is on the battlefield");
    let p1_life = runner.life(P1);

    runner.advance_to_combat();
    runner
        .declare_attackers(&[(ghalta, AttackTarget::Player(P1))])
        .unwrap();
    for _ in 0..4 {
        if matches!(
            runner.state().waiting_for,
            WaitingFor::DeclareBlockers { .. }
        ) {
            break;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("pass to blockers");
    }
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::DeclareBlockers { player: P1, .. }
    ));
    runner
        .declare_blockers(&[(z, ghalta), (yargle, ghalta)])
        .unwrap();

    // CR 509.2: the defender can activate Mother of Runes in the blockers step.
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P0 }
    ));
    runner.act(GameAction::PassPriority).unwrap();
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P1 }
    ));
    let outcome = runner.activate(mother, 0).target_object(z).resolve();
    assert!(matches!(
        outcome.final_waiting_for(),
        WaitingFor::NamedChoice { player: P1, choice_type: ChoiceType::Color { .. }, options, source: Some(source), .. }
            if options.iter().any(|option| option == "Green")
                && source.prompt.identity.reference.object_id == mother
    ));
    runner
        .act(GameAction::ChooseOption {
            choice: "Green".into(),
        })
        .unwrap();
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    assert!(runner.state().stack.is_empty());
    assert!(
        runner.state().objects[&z].has_keyword(&Keyword::Protection(ProtectionTarget::Color(
            ManaColor::Green
        ),))
    );
    assert_eq!(
        runner.state().objects[&ghalta].color,
        vec![ManaColor::Green]
    );
    let forecast = combat_damage_to_defender(runner.state(), ghalta, &[z, yargle]);

    // CR 702.19b: protection does not change the lethal damage owed for assignment.
    runner.combat_damage();
    assert_eq!(prompt_minimum(&runner, z), 7);
    assert_eq!(prompt_minimum(&runner, yargle), 9);
    assign_split(&mut runner, vec![(z, 7), (yargle, 5)], 0);
    runner.combat_damage();

    // CR 702.16e + CR 120.6: Zilortha takes no damage, and Yargle keeps its 5 marked.
    assert_eq!(zone(&runner, z), Zone::Battlefield);
    assert_eq!(marked(&runner, z), 0);
    assert_eq!(zone(&runner, yargle), Zone::Battlefield);
    assert_eq!(marked(&runner, yargle), 5);
    assert_eq!(prompt_minimum(&runner, z), 7);
    assert_eq!(prompt_minimum(&runner, yargle), 4);
    assign_split(&mut runner, vec![(z, 7), (yargle, 4)], 1);
    finish_combat(&mut runner);

    // CR 702.19b + CR 704.5g: 1 tramples over and Yargle has lethal damage by power.
    let actual = p1_life - runner.life(P1);
    assert_eq!(actual, 1);
    assert_eq!(zone(&runner, z), Zone::Battlefield);
    assert_eq!(zone(&runner, yargle), Zone::Graveyard);
    assert_eq!(
        forecast, actual,
        "forecast must match the actual trample damage"
    );
}

#[test]
fn zilortha_combat_forecast_drops_hydra_after_prevention_removes_its_counters() {
    let mut scenario = board();
    scenario.at_phase(Phase::PostCombatMain);
    scenario.with_library_top(P0, &["Island"]);
    let hydra = scenario
        .add_creature_to_hand_from_oracle(
            P1,
            "Protean Hydra",
            0,
            0,
            "This creature enters with X +1/+1 counters on it.\n\
             If damage would be dealt to this creature, prevent that damage and remove that many +1/+1 counters from it.\n\
             Whenever a +1/+1 counter is removed from this creature, put two +1/+1 counters on it at the beginning of the next end step.",
        )
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::X, ManaCostShard::Green],
            generic: 0,
        })
        .id();
    scenario.with_mana_pool(
        P1,
        vec![ManaUnit::new(ManaType::Green, hydra, false, vec![]); 13],
    );
    let ghalta = scenario
        .add_creature(P0, "Ghalta, Primal Hunger", 12, 12)
        .as_legendary()
        .from_oracle_text_with_keywords(&["Trample"], GHALTA)
        .with_color(vec![ManaColor::Green])
        .id();
    let mastery = scenario
        .add_spell_to_hand(P0, "Battle Mastery", false)
        .as_enchantment()
        .with_subtypes(vec!["Aura"])
        .with_mana_cost(ManaCost::zero())
        .from_oracle_text_with_keywords(&["Enchant"], BATTLE_MASTERY)
        .id();
    let mut runner = scenario.build();

    let state = runner.state_mut();
    state.active_player = P1;
    state.priority_player = P1;
    state.waiting_for = WaitingFor::Priority { player: P1 };

    let outcome = runner.cast(hydra).x(12).resolve();
    outcome.assert_zone(&[hydra], Zone::Battlefield);
    assert!(matches!(
        outcome.final_waiting_for(),
        WaitingFor::Priority { .. }
    ));
    assert!(outcome.state().stack.is_empty());

    runner.advance_to_phase(Phase::PreCombatMain);
    assert_eq!(runner.state().active_player, P0);
    cast_at(&mut runner, mastery, ghalta);
    assert!(runner.state().objects[&ghalta].has_keyword(&Keyword::DoubleStrike));
    let p1_life = runner.life(P1);
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(ghalta, AttackTarget::Player(P1))])
        .unwrap();
    for _ in 0..4 {
        if matches!(
            runner.state().waiting_for,
            WaitingFor::DeclareBlockers { .. }
        ) {
            break;
        }
        runner.act(GameAction::PassPriority).unwrap();
    }
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::DeclareBlockers { player: P1, .. }
    ));
    runner.declare_blockers(&[(hydra, ghalta)]).unwrap();
    assert_eq!(runner.state().objects[&hydra].toughness, Some(12));
    let forecast = combat_damage_to_defender(runner.state(), ghalta, &[hydra]);

    runner.act(GameAction::PassPriority).unwrap();
    runner.act(GameAction::PassPriority).unwrap();
    // CR 615.5 + CR 704.5f: prevention removes all twelve counters, leaving Hydra at 0/0.
    assert_eq!(zone(&runner, hydra), Zone::Graveyard);
    assert_eq!(runner.life(P1), p1_life);
    finish_combat(&mut runner);

    // CR 702.19d: Ghalta assigns all regular-step damage to the defending player.
    let actual = p1_life - runner.life(P1);
    assert_eq!(actual, 12);
    assert_eq!(
        forecast, actual,
        "forecast must match the actual trample damage"
    );
}
