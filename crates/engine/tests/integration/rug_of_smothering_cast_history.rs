//! Rug of Smothering counts the spells cast by the player who caused its trigger.
//!
//! CR 601.2a + CR 603.2: the player who casts the spell is the player named by
//! the trigger, even when a different player controls the permanent.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::parser::oracle::has_unimplemented;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const RUG_OF_SMOTHERING: &str =
    "Flying\nWhenever a player casts a spell, they lose 1 life for each spell they've cast this turn.";
const MAGEBANE_LIZARD: &str = "Whenever a player casts a noncreature spell, this creature deals damage to that player equal to the number of noncreature spells they've cast this turn.";
const DAMPING_SPHERE: &str = "If a land is tapped for two or more mana, it produces {C} instead of any other type and amount.\nEach spell a player casts costs {1} more to cast for each other spell that player has cast this turn.";

fn cast_and_assert_life_delta(
    runner: &mut engine::game::scenario::GameRunner,
    spell: engine::types::identifiers::ObjectId,
    label: &str,
    player: engine::types::player::PlayerId,
    delta: i32,
) {
    let outcome = runner
        .cast(spell)
        .try_resolve()
        .unwrap_or_else(|error| panic!("{label} cast rejected: {error:?}"));
    outcome.assert_life_delta(player, delta);
}

fn give_priority_to_p1(runner: &mut engine::game::scenario::GameRunner) {
    runner.state_mut().priority_player = P1;
    runner.state_mut().waiting_for = engine::types::game_state::WaitingFor::Priority { player: P1 };
}

#[test]
fn rug_counts_the_triggering_players_spells() {
    let mut scenario = GameScenario::new();
    let p0_history_a = scenario
        .add_spell_to_hand(P0, "P0 History A", true)
        .with_mana_cost(ManaCost::zero())
        .id();
    let p0_history_b = scenario
        .add_spell_to_hand(P0, "P0 History B", true)
        .with_mana_cost(ManaCost::zero())
        .id();
    let rug = scenario
        .add_creature_to_hand_from_oracle(P0, "Rug of Smothering", 3, 3, RUG_OF_SMOTHERING)
        .with_mana_cost(ManaCost::zero())
        .id();
    let p1_spell_a = scenario
        .add_spell_to_hand(P1, "P1 Spell A", true)
        .with_mana_cost(ManaCost::zero())
        .id();
    let p1_spell_b = scenario
        .add_spell_to_hand(P1, "P1 Spell B", true)
        .with_mana_cost(ManaCost::zero())
        .id();

    scenario.at_phase(Phase::PreCombatMain);
    let mut runner = scenario.build();
    assert!(
        runner.state().objects[&rug]
            .base_trigger_definitions
            .iter()
            .all(|trigger| trigger
                .execute
                .as_deref()
                .is_some_and(|ability| !has_unimplemented(ability))),
        "Rug's parsed trigger must contain a real executable effect"
    );

    // P0's history deliberately differs from P1's history. Rug is not on the
    // battlefield yet, so these casts cannot create Rug triggers.
    assert_eq!(runner.state().objects[&p0_history_a].zone, Zone::Hand);
    cast_and_assert_life_delta(&mut runner, p0_history_a, "P0 history A", P0, 0);
    assert_eq!(runner.state().objects[&p0_history_b].zone, Zone::Hand);
    cast_and_assert_life_delta(&mut runner, p0_history_b, "P0 history B", P0, 0);
    assert_eq!(
        runner.state().spells_cast_this_turn_by_player[&P0].len(),
        2,
        "reach guard: P0's pre-Rug cast history must be recorded"
    );

    // Casting Rug through the normal pipeline puts it under P0's control.
    assert_eq!(runner.state().objects[&rug].zone, Zone::Hand);
    runner.cast(rug).resolve();
    assert!(runner.state().battlefield.contains(&rug));

    // The active player remains P0 in this scenario, but P1 receives priority
    // and casts through the same production entry point. The trigger's player
    // binding must follow the SpellCast event, not Rug's controller.
    give_priority_to_p1(&mut runner);

    let p1_before = runner.life(P1);
    let p0_before = runner.life(P0);
    assert_eq!(runner.state().objects[&p1_spell_a].zone, Zone::Hand);
    cast_and_assert_life_delta(&mut runner, p1_spell_a, "P1 spell A", P1, -1);
    assert_eq!(runner.life(P1), p1_before - 1);
    assert_eq!(runner.life(P0), p0_before);
    assert_eq!(runner.state().spells_cast_this_turn_by_player[&P1].len(), 1);

    give_priority_to_p1(&mut runner);
    assert_eq!(runner.state().objects[&p1_spell_b].zone, Zone::Hand);
    cast_and_assert_life_delta(&mut runner, p1_spell_b, "P1 spell B", P1, -2);
    assert_eq!(runner.life(P1), p1_before - 3);
    assert_eq!(runner.life(P0), p0_before);
    assert_eq!(runner.state().spells_cast_this_turn_by_player[&P1].len(), 2);
}

#[test]
fn magebane_lizard_filters_the_triggering_players_noncreature_spells() {
    let mut scenario = GameScenario::new();
    let magebane = scenario
        .add_creature_to_hand_from_oracle(P0, "Magebane Lizard", 1, 2, MAGEBANE_LIZARD)
        .with_mana_cost(ManaCost::zero())
        .id();
    let p1_noncreature_a = scenario
        .add_spell_to_hand(P1, "P1 Noncreature A", true)
        .with_mana_cost(ManaCost::zero())
        .id();
    let p1_creature = scenario
        .add_creature_to_hand(P1, "P1 Creature", 1, 1)
        .with_mana_cost(ManaCost::zero())
        .id();
    let p1_noncreature_b = scenario
        .add_spell_to_hand(P1, "P1 Noncreature B", true)
        .with_mana_cost(ManaCost::zero())
        .id();

    scenario.at_phase(Phase::PreCombatMain);
    let mut runner = scenario.build();
    assert!(
        runner.state().objects[&magebane]
            .base_trigger_definitions
            .iter()
            .all(|trigger| trigger
                .execute
                .as_deref()
                .is_some_and(|ability| !has_unimplemented(ability))),
        "Magebane Lizard's parsed trigger must contain a real executable effect"
    );

    runner.cast(magebane).resolve();
    runner.state_mut().active_player = P1;
    give_priority_to_p1(&mut runner);

    let p1_before = runner.life(P1);
    let p0_before = runner.life(P0);
    cast_and_assert_life_delta(&mut runner, p1_noncreature_a, "P1 noncreature A", P1, -1);
    assert_eq!(runner.life(P1), p1_before - 1);
    assert_eq!(runner.life(P0), p0_before);

    // A creature spell is excluded by Magebane Lizard's printed filter.
    give_priority_to_p1(&mut runner);
    cast_and_assert_life_delta(&mut runner, p1_creature, "P1 creature", P1, 0);
    assert_eq!(runner.life(P1), p1_before - 1);
    assert_eq!(runner.life(P0), p0_before);

    // Only the first noncreature spell contributes to this trigger's count,
    // so the second noncreature spell deals 2 damage even though a creature
    // spell was cast between them.
    give_priority_to_p1(&mut runner);
    cast_and_assert_life_delta(&mut runner, p1_noncreature_b, "P1 noncreature B", P1, -2);
    assert_eq!(runner.life(P1), p1_before - 3);
    assert_eq!(runner.life(P0), p0_before);
}

#[test]
fn damping_sphere_counts_the_player_casting_the_spell() {
    let mut scenario = GameScenario::new();
    let p0_history_a = scenario
        .add_spell_to_hand(P0, "P0 History A", true)
        .with_mana_cost(ManaCost::zero())
        .id();
    let p0_history_b = scenario
        .add_spell_to_hand(P0, "P0 History B", true)
        .with_mana_cost(ManaCost::zero())
        .id();
    let p1_history = scenario
        .add_spell_to_hand(P1, "P1 History", true)
        .with_mana_cost(ManaCost::zero())
        .id();
    let damping_sphere = scenario
        .add_artifact_to_hand_from_oracle(P0, "Damping Sphere", DAMPING_SPHERE)
        .with_mana_cost(ManaCost::zero())
        .id();
    let p1_next_spell = scenario
        .add_spell_to_hand(P1, "P1 Next Spell", true)
        .with_mana_cost(ManaCost::zero())
        .id();
    scenario.with_mana_pool(
        P1,
        vec![ManaUnit::new(
            ManaType::Colorless,
            ObjectId(9_999),
            false,
            vec![],
        )],
    );
    scenario.at_phase(Phase::PreCombatMain);

    let mut runner = scenario.build();
    cast_and_assert_life_delta(&mut runner, p0_history_a, "P0 history A", P0, 0);
    cast_and_assert_life_delta(&mut runner, p0_history_b, "P0 history B", P0, 0);
    give_priority_to_p1(&mut runner);
    cast_and_assert_life_delta(&mut runner, p1_history, "P1 history", P1, 0);

    runner.state_mut().priority_player = P0;
    runner.state_mut().waiting_for = engine::types::game_state::WaitingFor::Priority { player: P0 };
    runner.cast(damping_sphere).resolve();
    assert!(runner.state().battlefield.contains(&damping_sphere));

    // P0 has cast three spells, including Damping Sphere, and P1 has cast one.
    // With P1 active, the next spell costs one extra mana for P1's history.
    runner.state_mut().active_player = P1;
    give_priority_to_p1(&mut runner);
    cast_and_assert_life_delta(&mut runner, p1_next_spell, "P1 next spell", P1, 0);
    assert_eq!(
        runner.state().players[P1.0 as usize].mana_pool.total(),
        0,
        "Damping Sphere must consume exactly P1's one-mana history tax"
    );
}
