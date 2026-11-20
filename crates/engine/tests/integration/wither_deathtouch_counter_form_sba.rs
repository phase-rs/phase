//! CR 704.5h: a creature dealt damage by a deathtouch source since the last state-based-action
//! check is destroyed, whether that damage was marked (CR 120.3e) or dealt as -1/-1 counters
//! (CR 120.3d), and the record lasts one check; damage prevented to 0 is not dealt (CR 120.8).
use engine::game::combat::AttackTarget;
use engine::game::sba::check_state_based_actions;
use engine::game::scenario::{GameRunner, GameScenario, Outcome, P0, P1};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const SMOLDERING_BUTCHER: &str =
    "Wither (This deals damage to creatures in the form of -1/-1 counters.)";
const GLISTENER_ELF: &str = "Infect (This creature deals damage to creatures in the form of -1/-1 counters and to players in the form of poison counters.)";
const TYPHOID_RATS: &str =
    "Deathtouch (Any amount of damage this deals to a creature is enough to destroy it.)";
const WALL_OF_STONE: &str = "Defender (This creature can't attack.)";
const DAUNTING_DEFENDER: &str =
    "If a source would deal damage to a Cleric creature you control, prevent 1 of that damage.";
const SOLEMNITY: &str = "Players can't get counters.\nCounters can't be put on artifacts, creatures, enchantments, or lands.";
const SEDGE_TROLL: &str =
    "This creature gets +1/+1 as long as you control a Swamp.\n{B}: Regenerate this creature.";
const DARKSTEEL_COLOSSUS: &str = "Trample (This creature can deal excess combat damage to the player or planeswalker it's attacking.)\nIndestructible (Damage and effects that say \"destroy\" don't destroy this creature.)\nIf Darksteel Colossus would be put into a graveyard from anywhere, reveal Darksteel Colossus and shuffle it into its owner's library instead.";
const HEROIC_INTERVENTION: &str =
    "Permanents you control gain hexproof and indestructible until end of turn.";
const DARKSTEEL_GARGOYLE: &str =
    "Flying\nIndestructible (Damage and effects that say \"destroy\" don't destroy this creature.)";
const BONDS_OF_MORTALITY: &str = "When this enchantment enters, draw a card.\n{G}: Creatures your opponents control lose hexproof and indestructible until end of turn.";
const OHRAN_VIPER: &str = "Whenever this creature deals combat damage to a creature, destroy that creature at end of combat.\nWhenever this creature deals combat damage to a player, you may draw a card.";
const KALITAS: &str = "Lifelink\nIf a nontoken creature an opponent controls would die, instead exile that card and create a 2/2 black Zombie creature token.\n{2}{B}, Sacrifice another Vampire or Zombie: Put two +1/+1 counters on Kalitas.";
const REST_IN_PEACE: &str = "When this enchantment enters, exile all graveyards.\nIf a card or token would be put into a graveyard from anywhere, exile it instead.";

fn run_combat_with_blocks(
    runner: &mut GameRunner,
    attacker: ObjectId,
    defender: PlayerId,
    blocker: ObjectId,
) -> Outcome {
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(attacker, AttackTarget::Player(defender))])
        .expect("declare attackers");
    pass_to_declare_blockers(runner);
    runner
        .declare_blockers(&[(blocker, attacker)])
        .expect("declare blockers");
    runner.combat_damage()
}

fn pass_to_declare_blockers(runner: &mut GameRunner) {
    for _ in 0..8 {
        if runner.waiting_for_kind() == "DeclareBlockers" {
            break;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("pass priority into the declare-blockers step");
    }
}

fn add_mana(runner: &mut GameRunner, player: PlayerId, colored: &[ManaType]) {
    for &color in colored {
        let _ = runner
            .state_mut()
            .add_mana_to_pool(player, ManaUnit::new(color, ObjectId(0), false, vec![]));
    }
}

fn minus_counters(runner: &GameRunner, id: ObjectId) -> u32 {
    runner.state().objects[&id]
        .counters
        .get(&CounterType::Minus1Minus1)
        .copied()
        .unwrap_or(0)
}

fn minus_counters_placed(outcome: &Outcome, id: ObjectId) -> u32 {
    outcome
        .events()
        .iter()
        .filter_map(|event| match event {
            GameEvent::CounterAdded {
                object_id,
                counter_type: CounterType::Minus1Minus1,
                count,
                ..
            } if *object_id == id => Some(*count),
            _ => None,
        })
        .sum()
}

fn damage_dealt_by(outcome: &Outcome, source: ObjectId) -> Vec<u32> {
    outcome
        .events()
        .iter()
        .filter_map(|event| match event {
            GameEvent::DamageDealt {
                source_id, amount, ..
            } if *source_id == source => Some(*amount),
            _ => None,
        })
        .collect()
}

/// P1's wither (Smoldering Butcher) or infect (Glistener Elf) attacker, given deathtouch as by
/// Basilisk Collar ("Equipped creature has deathtouch and lifelink."), blocked by P0's creature.
fn counter_form_attacker(scenario: &mut GameScenario, infect: bool) -> ObjectId {
    if infect {
        scenario
            .add_creature_from_oracle(P1, "Glistener Elf", 1, 1, GLISTENER_ELF)
            .with_keyword(Keyword::Infect)
            .deathtouch()
            .id()
    } else {
        scenario
            .add_creature_from_oracle(P1, "Smoldering Butcher", 4, 2, SMOLDERING_BUTCHER)
            .wither()
            .deathtouch()
            .id()
    }
}

fn p1_attacks_into(scenario: GameScenario, attacker: ObjectId, blocker: ObjectId) -> GameRunner {
    let mut runner = scenario.build();
    runner.state_mut().active_player = P1;
    run_combat_with_blocks(&mut runner, attacker, P0, blocker);
    runner
}

/// P1/P3: wither and infect deathtouch damage dealt as -1/-1 counters destroys a 0/8 Wall of Stone.
#[test]
fn counter_form_deathtouch_damage_destroys() {
    for (infect, counters) in [(false, 4), (true, 1)] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let wall = scenario
            .add_creature_from_oracle(P0, "Wall of Stone", 0, 8, WALL_OF_STONE)
            .defender()
            .id();
        let attacker = counter_form_attacker(&mut scenario, infect);
        let mut runner = scenario.build();
        runner.state_mut().active_player = P1;
        let outcome = run_combat_with_blocks(&mut runner, attacker, P0, wall);

        assert_eq!(
            minus_counters_placed(&outcome, wall),
            counters,
            "infect={infect}"
        );
        assert_eq!(runner.state().objects[&wall].zone, Zone::Graveyard);
    }
}

/// P2: under Solemnity the wither damage is still dealt though no counter is placed, so CR 704.5h destroys.
#[test]
fn deathtouch_wither_damage_without_counters_destroys() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_enchantment_from_oracle(P0, "Solemnity", SOLEMNITY);
    let wall = scenario
        .add_creature_from_oracle(P0, "Wall of Stone", 0, 8, WALL_OF_STONE)
        .defender()
        .id();
    let butcher = counter_form_attacker(&mut scenario, false);
    let mut runner = scenario.build();
    runner.state_mut().active_player = P1;
    let outcome = run_combat_with_blocks(&mut runner, butcher, P0, wall);

    assert_eq!(damage_dealt_by(&outcome, butcher), vec![4]);
    assert_eq!(minus_counters_placed(&outcome, wall), 0);
    assert_eq!(runner.state().objects[&wall].zone, Zone::Graveyard);
}

/// N1/N2: deathtouch damage Daunting Defender prevents to 0 is not dealt, on the marked and counter legs.
#[test]
fn deathtouch_damage_prevented_to_zero_does_not_destroy() {
    for infect in [false, true] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let defender = scenario
            .add_creature_from_oracle(P0, "Daunting Defender", 3, 3, DAUNTING_DEFENDER)
            .with_subtypes(vec!["Human", "Cleric"])
            .id();
        let attacker = if infect {
            counter_form_attacker(&mut scenario, true)
        } else {
            scenario
                .add_creature_from_oracle(P1, "Typhoid Rats", 1, 1, TYPHOID_RATS)
                .deathtouch()
                .id()
        };
        let runner = p1_attacks_into(scenario, attacker, defender);

        assert_eq!(
            runner.state().objects[&attacker].zone,
            Zone::Graveyard,
            "combat damage resolved (infect={infect})"
        );
        assert_eq!(runner.state().objects[&defender].damage_marked, 0);
        assert_eq!(minus_counters(&runner, defender), 0);
        assert_eq!(
            runner.state().objects[&defender].zone,
            Zone::Battlefield,
            "infect={infect}"
        );
    }
}

/// R: regeneration replaces the CR 704.5h destruction (CR 701.19a) and leaves the infect counter.
#[test]
fn regeneration_replaces_counter_form_deathtouch_destruction() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let troll = scenario
        .add_creature_from_oracle(P0, "Sedge Troll", 2, 2, SEDGE_TROLL)
        .id();
    let elf = scenario
        .add_creature_from_oracle(P1, "Glistener Elf", 1, 1, GLISTENER_ELF)
        .with_keyword(Keyword::Infect)
        .deathtouch()
        .id();
    let mut runner = scenario.build();
    add_mana(&mut runner, P0, &[ManaType::Black]);
    runner.activate(troll, 0).resolve();
    let outcome = run_combat_with_blocks(&mut runner, troll, P1, elf);

    assert_eq!(runner.state().objects[&elf].zone, Zone::Graveyard);
    assert!(outcome
        .events()
        .iter()
        .any(|event| matches!(event, GameEvent::Regenerated { object_id } if *object_id == troll)));
    assert_eq!(runner.state().objects[&troll].zone, Zone::Battlefield);
    assert_eq!(minus_counters(&runner, troll), 1);
}

/// I: an indestructible creature is not destroyed by deathtouch damage (CR 702.12b).
#[test]
fn indestructible_survives_counter_form_deathtouch_damage() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let colossus = scenario
        .add_creature_from_oracle(P0, "Darksteel Colossus", 11, 11, DARKSTEEL_COLOSSUS)
        .indestructible()
        .id();
    let butcher = counter_form_attacker(&mut scenario, false);
    let runner = p1_attacks_into(scenario, butcher, colossus);

    assert_eq!(minus_counters(&runner, colossus), 4);
    assert_eq!(runner.state().objects[&colossus].zone, Zone::Battlefield);
}

/// H: deathtouch damage dealt while Heroic Intervention made the Wall indestructible does not destroy it
/// once indestructible ends, because the check after combat damage ended the record.
#[test]
fn deathtouch_record_does_not_outlive_its_check() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let wall = scenario
        .add_creature_from_oracle(P0, "Wall of Stone", 0, 8, WALL_OF_STONE)
        .defender()
        .id();
    let intervention = scenario
        .add_spell_to_hand_from_oracle(P0, "Heroic Intervention", true, HEROIC_INTERVENTION)
        .id();
    let butcher = counter_form_attacker(&mut scenario, false);
    let mut runner = scenario.build();
    runner.state_mut().active_player = P1;
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(butcher, AttackTarget::Player(P0))])
        .expect("declare attackers");
    runner
        .act(GameAction::PassPriority)
        .expect("P1 passes priority in the declare-attackers step");
    add_mana(&mut runner, P0, &[ManaType::Green, ManaType::Colorless]);
    runner.cast(intervention).resolve();
    assert!(runner.state().objects[&wall].has_keyword(&Keyword::Indestructible));
    pass_to_declare_blockers(&mut runner);
    runner
        .declare_blockers(&[(wall, butcher)])
        .expect("declare blockers");
    runner.combat_damage();
    assert_eq!(minus_counters(&runner, wall), 4);
    assert_eq!(runner.state().objects[&wall].zone, Zone::Battlefield);

    runner.advance_to_end_step();
    runner.advance_to_upkeep();
    assert_eq!(runner.state().phase, Phase::Upkeep);
    assert!(!runner.state().objects[&wall].has_keyword(&Keyword::Indestructible));
    assert_eq!(runner.state().objects[&wall].zone, Zone::Battlefield);
    assert_eq!(minus_counters(&runner, wall), 4);
}

/// B: Bonds of Mortality ruling — lethal marked damage destroys a creature that loses indestructible,
/// earlier deathtouch damage does not.
#[test]
fn losing_indestructible_after_deathtouch_damage_does_not_destroy() {
    for deathtouch in [true, false] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let gargoyle = scenario
            .add_creature_from_oracle(P0, "Darksteel Gargoyle", 3, 3, DARKSTEEL_GARGOYLE)
            .flying()
            .indestructible()
            .id();
        let attacker = if deathtouch {
            scenario
                .add_creature_from_oracle(P1, "Typhoid Rats", 1, 1, TYPHOID_RATS)
                .deathtouch()
                .id()
        } else {
            scenario
                .add_creature_from_oracle(P1, "Hill Giant", 3, 3, "")
                .id()
        };
        let bonds = scenario
            .add_enchantment_from_oracle(P1, "Bonds of Mortality", BONDS_OF_MORTALITY)
            .id();
        let mut runner = p1_attacks_into(scenario, attacker, gargoyle);
        let expected_marked = if deathtouch { 1 } else { 3 };
        assert_eq!(
            runner.state().objects[&gargoyle].damage_marked,
            expected_marked
        );
        assert_eq!(runner.state().objects[&gargoyle].zone, Zone::Battlefield);

        runner.advance_to_phase(Phase::PostCombatMain);
        add_mana(&mut runner, P1, &[ManaType::Green]);
        runner.activate(bonds, 0).resolve();
        runner.advance_until_stack_empty();

        let gargoyle_obj = &runner.state().objects[&gargoyle];
        if deathtouch {
            assert_eq!(gargoyle_obj.zone, Zone::Battlefield);
            assert!(!gargoyle_obj.has_keyword(&Keyword::Indestructible));
        } else {
            assert_eq!(gargoyle_obj.zone, Zone::Graveyard);
        }
    }
}

/// E: damage Daunting Defender prevents to 0 raises no `DamageDealt`, so Ohran Viper's trigger does not
/// fire; against Hill Giant it fires.
#[test]
fn damage_prevented_to_zero_raises_no_damage_event() {
    for cleric in [true, false] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let blocker = if cleric {
            scenario
                .add_creature_from_oracle(P0, "Daunting Defender", 3, 3, DAUNTING_DEFENDER)
                .with_subtypes(vec!["Human", "Cleric"])
                .id()
        } else {
            scenario
                .add_creature_from_oracle(P0, "Hill Giant", 3, 3, "")
                .with_subtypes(vec!["Giant"])
                .id()
        };
        let viper = scenario
            .add_creature_from_oracle(P1, "Ohran Viper", 1, 3, OHRAN_VIPER)
            .id();
        let mut runner = scenario.build();
        runner.state_mut().active_player = P1;
        let outcome = run_combat_with_blocks(&mut runner, viper, P0, blocker);
        let dealt = damage_dealt_by(&outcome, viper);
        runner.advance_to_end_step();

        if cleric {
            assert_eq!(runner.state().objects[&viper].zone, Zone::Graveyard);
            assert!(dealt.is_empty(), "prevented damage raised {dealt:?}");
            assert_eq!(runner.state().objects[&blocker].zone, Zone::Battlefield);
        } else {
            assert_eq!(dealt, vec![1]);
            assert_eq!(runner.state().objects[&blocker].zone, Zone::Graveyard);
        }
    }
}

/// S: CR 616.1 — Kalitas, Traitor of Ghet and Rest in Peace both replace the first dying creature, so
/// the check pauses; it has ended the indestructible Gargoyle's record and kept the unreached
/// Courser's, which the resumed check destroys.
#[test]
fn paused_check_keeps_deathtouch_record_of_unreached_candidate() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P1, "Kalitas, Traitor of Ghet", 3, 4, KALITAS);
    scenario.add_enchantment_from_oracle(P1, "Rest in Peace", REST_IN_PEACE);
    let bears = scenario
        .add_creature_from_oracle(P0, "Grizzly Bears", 2, 2, "")
        .id();
    let courser = scenario
        .add_creature_from_oracle(P0, "Centaur Courser", 3, 3, "")
        .id();
    let gargoyle = scenario
        .add_creature_from_oracle(P0, "Darksteel Gargoyle", 3, 3, DARKSTEEL_GARGOYLE)
        .flying()
        .indestructible()
        .id();
    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&bears)
        .unwrap()
        .damage_marked = 2;
    for id in [courser, gargoyle] {
        let obj = runner.state_mut().objects.get_mut(&id).unwrap();
        obj.damage_marked = 1;
        obj.dealt_deathtouch_damage = true;
    }

    let mut events = Vec::new();
    check_state_based_actions(runner.state_mut(), &mut events);
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ),
        "got {:?}",
        runner.state().waiting_for
    );
    assert_eq!(runner.state().objects[&courser].zone, Zone::Battlefield);
    assert!(runner.state().objects[&courser].dealt_deathtouch_damage);
    assert!(!runner.state().objects[&gargoyle].dealt_deathtouch_damage);

    for _ in 0..2 {
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ));
        runner
            .act(GameAction::ChooseReplacement { index: 0 })
            .expect("answer the CR 616.1 choice");
    }
    check_state_based_actions(runner.state_mut(), &mut events);

    assert_eq!(runner.state().objects[&bears].zone, Zone::Exile);
    assert_eq!(runner.state().objects[&courser].zone, Zone::Exile);
    assert_eq!(runner.state().objects[&gargoyle].zone, Zone::Battlefield);
}
