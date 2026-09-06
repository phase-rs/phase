//! Diagnostic: Sliver Overlord's two printed `{3}` activated abilities must be
//! offered in `legal_actions_by_object` whenever the controller can produce
//! three mana — including when the only mana available comes from a mana
//! ability GRANTED to the Sliver army by another Sliver (Gemhide/Manaweft).
//!
//! Reported from play: the ability picker listed only the three abilities
//! granted by other Slivers (regenerate, pay-2-life bounce, add one mana) and
//! neither of Sliver Overlord's own `{3}` abilities.

use engine::ai_support::legal_actions_full;
use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::types::actions::GameAction;
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaColor;
use engine::types::phase::Phase;

const OVERLORD: &str = "{3}: Search your library for a Sliver card, reveal that card, put it into your hand, then shuffle.\n{3}: Gain control of target Sliver.";
const GEMHIDE: &str = "All Slivers have \"{T}: Add one mana of any color.\"";
const CRYPT: &str = "All Slivers have \"{T}: Regenerate target Sliver.\"";

fn offered_indices(runner: &GameRunner, id: ObjectId) -> Vec<usize> {
    let (_, _, grouped) = legal_actions_full(runner.state());
    grouped
        .get(&id)
        .map(Vec::as_slice)
        .unwrap_or(&[])
        .iter()
        .filter_map(|a| match a {
            GameAction::ActivateAbility {
                source_id,
                ability_index,
            } if *source_id == id => Some(*ability_index),
            _ => None,
        })
        .collect()
}

#[test]
fn overlord_abilities_offered_with_lands() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let overlord = scenario
        .add_creature(P0, "Sliver Overlord", 7, 7)
        .with_subtypes(vec!["Sliver", "Mutant"])
        .from_oracle_text(OVERLORD)
        .id();
    for _ in 0..3 {
        scenario.add_basic_land(P0, ManaColor::Green);
    }

    let runner = scenario.build();
    let indices = offered_indices(&runner, overlord);
    assert!(
        indices.contains(&0) && indices.contains(&1),
        "expected both printed {{3}} abilities offered with 3 untapped lands; got {indices:?}",
    );
}

#[test]
fn overlord_abilities_offered_with_granted_sliver_mana() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let overlord = scenario
        .add_creature(P0, "Sliver Overlord", 7, 7)
        .with_subtypes(vec!["Sliver", "Mutant"])
        .from_oracle_text(OVERLORD)
        .id();
    scenario
        .add_creature(P0, "Gemhide Sliver", 1, 1)
        .with_subtypes(vec!["Sliver"])
        .from_oracle_text(GEMHIDE);
    scenario
        .add_creature(P0, "Crypt Sliver", 1, 1)
        .with_subtypes(vec!["Sliver"])
        .from_oracle_text(CRYPT);
    scenario
        .add_creature(P0, "Metallic Sliver", 1, 1)
        .with_subtypes(vec!["Sliver"]);
    scenario
        .add_creature(P0, "Muscle Sliver", 1, 1)
        .with_subtypes(vec!["Sliver"]);

    let runner = scenario.build();
    let indices = offered_indices(&runner, overlord);
    let (_, _, grouped) = legal_actions_full(runner.state());
    assert!(
        indices.contains(&0) && indices.contains(&1),
        "expected both printed {{3}} abilities offered when 4 Slivers can each tap for mana; got {indices:?}\nall actions: {:?}",
        grouped.get(&overlord),
    );
}

/// Rules out the multiplayer / commander pathway: the report came from a
/// 4-player Commander game with Sliver Overlord as the commander. The
/// permanent stays on the battlefield (`with_commander` would move it to the
/// command zone), carrying only the `is_commander` designation.
#[test]
fn overlord_abilities_offered_in_four_player_commander_game() {
    let mut scenario = GameScenario::new_n_player(4, 7);
    scenario.at_phase(Phase::PreCombatMain);

    let overlord = scenario
        .add_creature(P0, "Sliver Overlord", 7, 7)
        .with_subtypes(vec!["Sliver", "Mutant"])
        .from_oracle_text(OVERLORD)
        .commander()
        .id();
    for _ in 0..3 {
        scenario.add_basic_land(P0, ManaColor::Green);
    }

    let runner = scenario.build();
    let indices = offered_indices(&runner, overlord);
    assert!(
        indices.contains(&0) && indices.contains(&1),
        "expected both printed {{3}} abilities in a 4-player commander game; got {indices:?}",
    );
}

/// Screenshot replica: every other Sliver is tapped, so the ONLY mana the
/// controller can make is the single mana from the Overlord's own granted
/// "{T}: Add one mana of any color". `{3}` is then genuinely unpayable, and the
/// picker shows only the granted abilities — exactly the reported symptom.
#[test]
fn overlord_abilities_absent_when_army_is_tapped_out() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let overlord = scenario
        .add_creature(P0, "Sliver Overlord", 7, 7)
        .with_subtypes(vec!["Sliver", "Mutant"])
        .from_oracle_text(OVERLORD)
        .id();
    let gemhide = scenario
        .add_creature(P0, "Gemhide Sliver", 1, 1)
        .with_subtypes(vec!["Sliver"])
        .from_oracle_text(GEMHIDE)
        .id();
    let crypt = scenario
        .add_creature(P0, "Crypt Sliver", 1, 1)
        .with_subtypes(vec!["Sliver"])
        .from_oracle_text(CRYPT)
        .id();

    let mut runner = scenario.build();
    for id in [gemhide, crypt] {
        runner.state_mut().objects.get_mut(&id).unwrap().tapped = true;
    }

    let indices = offered_indices(&runner, overlord);
    assert!(
        !indices.contains(&0) && !indices.contains(&1),
        "with only one available mana the {{3}} abilities must be withheld; got {indices:?}",
    );
    assert!(
        !indices.is_empty(),
        "the granted abilities must still be offered — this is what the player saw",
    );
}

/// Isolates the `is_commander` designation from the player count.
#[test]
fn overlord_abilities_offered_with_commander_flag_two_players() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let overlord = scenario
        .add_creature(P0, "Sliver Overlord", 7, 7)
        .with_subtypes(vec!["Sliver", "Mutant"])
        .from_oracle_text(OVERLORD)
        .commander()
        .id();
    for _ in 0..3 {
        scenario.add_basic_land(P0, ManaColor::Green);
    }

    let runner = scenario.build();
    let indices = offered_indices(&runner, overlord);
    assert!(
        indices.contains(&0) && indices.contains(&1),
        "commander designation alone must not withhold the {{3}} abilities; got {indices:?}",
    );
}

/// Isolates the player count from the `is_commander` designation.
#[test]
fn overlord_abilities_offered_in_four_player_game_without_commander_flag() {
    let mut scenario = GameScenario::new_n_player(4, 7);
    scenario.at_phase(Phase::PreCombatMain);

    let overlord = scenario
        .add_creature(P0, "Sliver Overlord", 7, 7)
        .with_subtypes(vec!["Sliver", "Mutant"])
        .from_oracle_text(OVERLORD)
        .id();
    for _ in 0..3 {
        scenario.add_basic_land(P0, ManaColor::Green);
    }

    let runner = scenario.build();
    let state = runner.state();
    let indices = offered_indices(&runner, overlord);
    assert!(
        indices.contains(&0) && indices.contains(&1),
        "four seats must not withhold the {{3}} abilities; got {indices:?}\n\
         active={:?} priority={:?} waiting={:?} battlefield={}",
        state.active_player,
        state.priority_player,
        state.waiting_for,
        state.battlefield.len(),
    );
}
