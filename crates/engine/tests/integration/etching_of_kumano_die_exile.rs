//! Etching of Kumano exiles creatures dealt damage by a source it controls.

use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{Effect, FilterProp, ReplacementCondition, TargetFilter};
use engine::types::card_type::CoreType;
use engine::types::game_state::WaitingFor;
use engine::types::phase::Phase;
use engine::types::replacements::ReplacementEvent;
use engine::types::zones::Zone;

const ETCHING: &str =
    "Haste\nIf a creature dealt damage this turn by a source you controlled would die, exile it instead.";

#[test]
fn etching_of_kumano_parses_a_graveyard_move_replacement() {
    let parsed = parse_oracle_text(
        ETCHING,
        "Etching of Kumano",
        &["Haste".into()],
        &["Enchantment".into(), "Creature".into()],
        &["Human".into(), "Shaman".into()],
    );
    assert_eq!(parsed.replacements.len(), 1);
    let replacement = &parsed.replacements[0];
    assert_eq!(replacement.event, ReplacementEvent::Moved);
    assert_eq!(replacement.destination_zone, Some(Zone::Graveyard));
    assert!(matches!(
        replacement.condition,
        Some(ReplacementCondition::DealtDamageThisTurnBySource { .. })
    ));
    assert!(matches!(
        &replacement.valid_card,
        Some(TargetFilter::Typed(filter)) if filter.properties.contains(&FilterProp::InZone {
            zone: Zone::Battlefield,
        })
    ));
    assert!(matches!(
        replacement.execute.as_ref().map(|execute| &*execute.effect),
        Some(Effect::ChangeZone {
            destination: Zone::Exile,
            ..
        })
    ));
}

#[test]
fn literal_self_destroy_replacement_remains_a_destroy_replacement() {
    let parsed = parse_oracle_text(
        "If Destroy Guard would be destroyed, exile it instead.",
        "Destroy Guard",
        &[],
        &["Creature".into()],
        &[],
    );
    assert_eq!(parsed.replacements.len(), 1);
    let replacement = &parsed.replacements[0];
    assert_eq!(replacement.event, ReplacementEvent::Destroy);
    assert_eq!(replacement.destination_zone, None);
    assert!(matches!(
        replacement.execute.as_ref().map(|execute| &*execute.effect),
        Some(Effect::ChangeZone {
            destination: Zone::Exile,
            ..
        })
    ));
}

#[test]
fn etching_of_kumano_exiles_a_creature_it_kills_in_combat() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let attacker = scenario.add_creature(P0, "Target Attacker", 1, 2).id();
    let etching = scenario
        .add_creature_from_oracle(P1, "Etching of Kumano", 2, 2, ETCHING)
        .id();
    let mut runner = scenario.build();

    runner.advance_to_combat();
    runner
        .declare_attackers(&[(attacker, AttackTarget::Player(P1))])
        .expect("the target must be able to attack");
    if matches!(runner.state().waiting_for, WaitingFor::Priority { .. }) {
        runner.pass_both_players();
    }
    runner
        .declare_blockers(&[(etching, attacker)])
        .expect("Etching must be able to block the target");
    runner.combat_damage();

    assert_eq!(
        runner.state().objects[&attacker].zone,
        Zone::Exile,
        "a creature killed by Etching's combat damage must be exiled"
    );
}

#[test]
fn unconditional_die_exile_replacement_exiles_a_sacrificed_creature() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let victim = scenario.add_creature(P0, "Target Creature", 1, 1).id();
    scenario.add_creature_from_oracle(
        P1,
        "Die-Exile Watcher",
        2,
        2,
        "If a nontoken creature an opponent controls would die, exile it instead.",
    );
    let sacrifice = scenario
        .add_spell_to_hand_from_oracle(P0, "Synthetic Sacrifice", true, "Sacrifice a creature.")
        .id();
    let mut runner = scenario.build();

    runner.cast(sacrifice).resolve();

    assert_eq!(
        runner.state().objects[&victim].zone,
        Zone::Exile,
        "would-die replacement must intercept sacrifice, not only destruction"
    );
}

#[test]
fn unconditional_die_exile_replacement_exiles_a_lethally_damaged_creature() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let attacker = scenario.add_creature(P0, "Target Attacker", 1, 2).id();
    let blocker = scenario.add_creature(P1, "Blocking Creature", 2, 2).id();
    scenario.add_creature_from_oracle(
        P1,
        "Die-Exile Watcher",
        2,
        2,
        "If a nontoken creature an opponent controls would die, exile it instead.",
    );
    let mut runner = scenario.build();

    runner.advance_to_combat();
    runner
        .declare_attackers(&[(attacker, AttackTarget::Player(P1))])
        .expect("the target must be able to attack");
    if matches!(runner.state().waiting_for, WaitingFor::Priority { .. }) {
        runner.pass_both_players();
    }
    runner
        .declare_blockers(&[(blocker, attacker)])
        .expect("the blocker must be able to block the target");
    runner.combat_damage();

    assert_eq!(
        runner.state().objects[&attacker].zone,
        Zone::Exile,
        "would-die replacement must intercept lethal-damage SBA"
    );
}

#[test]
fn self_die_exile_replacement_exiles_a_sacrificed_creature() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let victim = scenario
        .add_creature_from_oracle(
            P0,
            "Self-Exile Creature",
            1,
            1,
            "If Self-Exile Creature would die, exile it instead.",
        )
        .id();
    let sacrifice = scenario
        .add_spell_to_hand_from_oracle(P0, "Synthetic Sacrifice", true, "Sacrifice a creature.")
        .id();
    let mut runner = scenario.build();

    runner.cast(sacrifice).resolve();

    assert_eq!(
        runner.state().objects[&victim].zone,
        Zone::Exile,
        "self would-die replacement must intercept sacrifice"
    );
}

#[test]
fn self_die_exile_replacement_exiles_a_lethally_damaged_creature() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let attacker = scenario
        .add_creature_from_oracle(
            P0,
            "Self-Exile Creature",
            1,
            2,
            "If Self-Exile Creature would die, exile it instead.",
        )
        .id();
    let blocker = scenario.add_creature(P1, "Blocking Creature", 2, 2).id();
    let mut runner = scenario.build();

    runner.advance_to_combat();
    runner
        .declare_attackers(&[(attacker, AttackTarget::Player(P1))])
        .expect("the target must be able to attack");
    if matches!(runner.state().waiting_for, WaitingFor::Priority { .. }) {
        runner.pass_both_players();
    }
    runner
        .declare_blockers(&[(blocker, attacker)])
        .expect("the blocker must be able to block the target");
    runner.combat_damage();

    assert_eq!(
        runner.state().objects[&attacker].zone,
        Zone::Exile,
        "self would-die replacement must intercept lethal-damage SBA"
    );
}

#[test]
fn unconditional_die_exile_replacement_does_not_exile_a_milled_creature_card() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let victim = scenario.add_card_to_library_top(P0, "Milled Creature");
    scenario.add_card_to_library_top(P1, "Milled Noncreature");
    scenario.add_creature_from_oracle(
        P1,
        "Die-Exile Watcher",
        2,
        2,
        "If a nontoken creature an opponent controls would die, exile it instead.",
    );
    let mill = scenario
        .add_spell_to_hand_from_oracle(P0, "Synthetic Mill", true, "Each player mills a card.")
        .id();
    let mut runner = scenario.build();
    let victim_object = runner.state_mut().objects.get_mut(&victim).unwrap();
    victim_object.card_types.core_types.push(CoreType::Creature);
    victim_object.base_card_types = victim_object.card_types.clone();

    runner.cast(mill).resolve();

    assert_eq!(
        runner.state().objects[&victim].zone,
        Zone::Graveyard,
        "a creature card milled from a library did not die and must not be exiled"
    );
}

#[test]
fn unconditional_die_exile_replacement_does_not_exile_a_discarded_creature_card() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let victim = scenario
        .add_creature_to_hand(P1, "Discarded Creature", 1, 1)
        .id();
    scenario.add_creature_from_oracle(
        P0,
        "Die-Exile Watcher",
        2,
        2,
        "If a nontoken creature an opponent controls would die, exile it instead.",
    );
    let discard = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Synthetic Discard",
            true,
            "Each opponent discards a card.",
        )
        .id();
    let mut runner = scenario.build();

    runner.cast(discard).resolve();

    assert_eq!(
        runner.state().objects[&victim].zone,
        Zone::Graveyard,
        "a creature card discarded from a hand did not die and must not be exiled"
    );
}
