//! Integration tests for Bramble Elemental and Shielding Plax:
//!
//! Bramble Elemental:
//! "Whenever an Aura becomes attached to this creature, create two 1/1 green Saproling creature tokens."
//!
//! Shielding Plax:
//! "Enchant creature
//! When this Aura enters, draw a card.
//! Enchanted creature can't be the target of spells or abilities your opponents control."

use engine::game::keywords::has_keyword;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::game_state::WaitingFor;
use engine::types::keywords::Keyword;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

const BRAMBLE_ELEMENTAL: &str =
    "Whenever an Aura becomes attached to this creature, create two 1/1 green Saproling creature tokens.";

const SHIELDING_PLAX: &str = "Enchant creature\nWhen this Aura enters, draw a card.\nEnchanted creature can't be the target of spells or abilities your opponents control.";

fn saproling_count(runner: &GameRunner, player: PlayerId) -> usize {
    let state = runner.state();
    state
        .battlefield
        .iter()
        .filter_map(|id| state.objects.get(id))
        .filter(|obj| {
            obj.controller == player
                && obj.card_types.subtypes.iter().any(|s| s == "Saproling")
                && obj.power == Some(1)
                && obj.toughness == Some(1)
        })
        .count()
}

#[test]
fn bramble_elemental_creates_tokens_when_enchanted_by_shielding_plax() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Forest", "Forest"]);

    let bramble = scenario
        .add_creature_from_oracle(P0, "Bramble Elemental", 4, 4, BRAMBLE_ELEMENTAL)
        .id();

    let plax = scenario
        .add_spell_to_hand(P0, "Shielding Plax", false)
        .as_enchantment()
        .with_subtypes(vec!["Aura"])
        .with_mana_cost(ManaCost::generic(0))
        .from_oracle_text_with_keywords(&["Enchant"], SHIELDING_PLAX)
        .id();

    let mut runner = scenario.build();

    // Cast Shielding Plax targeting Bramble Elemental
    let outcome = runner.cast(plax).target_object(bramble).resolve();

    // 1. Bramble Elemental creates two 1/1 green Saprolings
    assert_eq!(
        saproling_count(&runner, P0),
        2,
        "Bramble Elemental must create 2 Saproling tokens when an Aura becomes attached"
    );

    // 2. Shielding Plax ETB draws a card (hand delta since commit is +1)
    outcome.assert_hand_drawn(P0, 1);

    // 3. Bramble Elemental has Hexproof from Shielding Plax
    assert!(
        has_keyword(&runner.state().objects[&bramble], &Keyword::Hexproof),
        "Bramble Elemental must have Hexproof granted by Shielding Plax"
    );
}

#[test]
fn shielding_plax_grants_hexproof_to_enchanted_creature() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Forest"]);

    let bear = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();

    let plax = scenario
        .add_spell_to_hand(P0, "Shielding Plax", false)
        .as_enchantment()
        .with_subtypes(vec!["Aura"])
        .with_mana_cost(ManaCost::generic(0))
        .from_oracle_text_with_keywords(&["Enchant"], SHIELDING_PLAX)
        .id();

    let mut runner = scenario.build();

    // Initially Grizzly Bears does not have Hexproof
    assert!(
        !has_keyword(&runner.state().objects[&bear], &Keyword::Hexproof),
        "Grizzly Bears should not have Hexproof before being enchanted"
    );

    // Cast Shielding Plax targeting Grizzly Bears
    runner.cast(plax).target_object(bear).resolve();

    // After resolution, Grizzly Bears has Hexproof (opponents cannot target it)
    assert!(
        has_keyword(&runner.state().objects[&bear], &Keyword::Hexproof),
        "Grizzly Bears must have Hexproof granted by Shielding Plax"
    );

    // Shielding Plax itself is NOT the creature and does not have Hexproof on itself
    assert!(
        !has_keyword(&runner.state().objects[&plax], &Keyword::Hexproof),
        "Shielding Plax itself must not have Hexproof (the static ability affects the enchanted creature)"
    );
}

#[test]
fn opponent_cannot_target_creature_enchanted_by_shielding_plax() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Forest"]);

    let bear = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();

    let plax = scenario
        .add_spell_to_hand(P0, "Shielding Plax", false)
        .as_enchantment()
        .with_subtypes(vec!["Aura"])
        .with_mana_cost(ManaCost::generic(0))
        .from_oracle_text_with_keywords(&["Enchant"], SHIELDING_PLAX)
        .id();

    let spitting_earth = scenario
        .add_spell_to_hand(P1, "Spitting Earth", false)
        .with_mana_cost(ManaCost::generic(0))
        .from_oracle_text("Spitting Earth deals damage to target creature equal to the number of Mountains you control.")
        .id();

    let mut runner = scenario.build();

    // P0 enchants Grizzly Bears with Shielding Plax
    runner.cast(plax).target_object(bear).resolve();

    // Give priority to P1
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P1 };
    runner.state_mut().priority_player = P1;

    // P1 tries to cast Spitting Earth targeting Grizzly Bears
    let result = runner
        .cast(spitting_earth)
        .target_object(bear)
        .try_resolve();
    assert!(
        result.is_err(),
        "Opponent should not be able to target creature enchanted with Shielding Plax"
    );
}
