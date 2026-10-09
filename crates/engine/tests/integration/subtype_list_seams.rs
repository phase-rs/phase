//! CR 205.3: subtype lists read whole where this PR's class reaches them.
//! Mary Read and Anne Bonny's discard trigger ("an Island, Pirate, or Vehicle
//! card") and Unagi's Spray's condition ("If you control a Fish, Octopus,
//! Otter, Seal, Serpent, or Whale").

use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::types::ability::{TargetFilter, TypeFilter};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const MARY_READ: &str = "Haste\n{T}: Draw a card, then discard a card.\nWhenever you discard an Island, Pirate, or Vehicle card, create a tapped Treasure token.";

/// Every `TypeFilter` leg of a filter tree, flattened.
fn legs(filter: &TargetFilter) -> Vec<TypeFilter> {
    match filter {
        TargetFilter::Typed(tf) => tf.type_filters.clone(),
        TargetFilter::Or { filters } | TargetFilter::And { filters } => {
            filters.iter().flat_map(legs).collect()
        }
        _ => Vec::new(),
    }
}

fn treasures(runner: &GameRunner) -> Vec<ObjectId> {
    runner
        .state()
        .battlefield
        .iter()
        .copied()
        .filter(|id| runner.state().objects[id].name == "Treasure")
        .collect()
}

/// Mary Read activates, draws, and discards `discarded` (a card put in hand
/// with `subtypes` / as a land). Returns the Treasures created.
fn mary_discards(subtypes: Vec<&'static str>, land: bool) -> (GameRunner, Vec<ObjectId>) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Drawn Card"]);
    let mary = scenario
        .add_creature_from_oracle(P0, "Mary Read and Anne Bonny", 3, 3, MARY_READ)
        .id();
    let discarded = if land {
        scenario
            .add_land_to_hand(P0, "Island Card")
            .with_subtypes(subtypes)
            .id()
    } else {
        scenario
            .add_creature_to_hand(P0, "Discarded Card", 2, 2)
            .with_subtypes(subtypes)
            .id()
    };
    let mut runner = scenario.build();
    runner
        .act(GameAction::ActivateAbility {
            source_id: mary,
            ability_index: 0,
        })
        .expect("activate");
    for _ in 0..16 {
        match runner.state().waiting_for.clone() {
            WaitingFor::DiscardChoice { .. } => {
                runner
                    .act(GameAction::SelectCards {
                        cards: vec![discarded],
                    })
                    .expect("discard");
            }
            WaitingFor::Priority { .. } if !runner.state().stack.is_empty() => {
                runner.act(GameAction::PassPriority).expect("pass");
            }
            _ => break,
        }
    }
    assert_eq!(
        runner.state().objects[&discarded].zone,
        Zone::Graveyard,
        "reach guard: the chosen card was discarded"
    );
    let made = treasures(&runner);
    (runner, made)
}

/// The parsed trigger names every leg of the list, and only those.
#[test]
fn mary_read_discard_filter_holds_the_whole_list() {
    let mut scenario = GameScenario::new();
    let mary = scenario
        .add_creature_from_oracle(P0, "Mary Read and Anne Bonny", 3, 3, MARY_READ)
        .id();
    let runner = scenario.build();
    let filter = runner.state().objects[&mary]
        .trigger_definitions
        .iter_unchecked()
        .find_map(|entry| entry.definition().valid_card.clone())
        .expect("the discard trigger has a card filter");
    let mut subtypes: Vec<String> = legs(&filter)
        .into_iter()
        .filter_map(|leg| match leg {
            TypeFilter::Subtype(name) => Some(name),
            _ => None,
        })
        .collect();
    subtypes.sort();
    assert_eq!(subtypes, ["Island", "Pirate", "Vehicle"], "{filter:?}");
}

/// CR 701.9 + CR 111.10a: discarding a listed card creates a TAPPED Treasure;
/// an unlisted card creates nothing.
#[test]
fn mary_read_makes_a_tapped_treasure_only_for_listed_cards() {
    for (label, subtypes, land, expected) in [
        ("Pirate", vec!["Pirate"], false, 1),
        ("Island", vec!["Island"], true, 1),
        ("Bear", vec!["Bear"], false, 0),
    ] {
        let (runner, made) = mary_discards(subtypes, land);
        assert_eq!(made.len(), expected, "{label}");
        for treasure in made {
            assert!(
                runner.state().objects[&treasure].tapped,
                "{label}: the Treasure enters tapped"
            );
        }
    }
}

const UNAGIS_SPRAY: &str = "Target creature gets -4/-0 until end of turn. If you control a Fish, Octopus, Otter, Seal, Serpent, or Whale, draw a card.";

fn spray_draws(controlled: Option<&'static str>) -> usize {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Drawn Card"]);
    let victim = scenario
        .add_creature(engine::game::scenario::P1, "Victim", 4, 4)
        .id();
    if let Some(subtype) = controlled {
        scenario
            .add_creature(P0, "Sea Creature", 1, 1)
            .with_subtypes(vec![subtype]);
    }
    let spray = scenario
        .add_spell_to_hand_from_oracle(P0, "Unagi's Spray", true, UNAGIS_SPRAY)
        .with_mana_cost(engine::types::mana::ManaCost::zero())
        .id();
    let mut runner = scenario.build();
    let library = runner.state().players[0].library.len();
    runner.cast(spray).target_object(victim).resolve();
    library - runner.state().players[0].library.len()
}

/// CR 205.3m + CR 608.2c: any listed subtype satisfies the condition; an
/// unlisted one doesn't.
#[test]
fn unagis_spray_draws_for_any_listed_subtype() {
    assert_eq!(spray_draws(Some("Otter")), 1, "Otter");
    assert_eq!(spray_draws(Some("Whale")), 1, "Whale (the last leg)");
    assert_eq!(spray_draws(Some("Fish")), 1, "Fish (the first leg)");
    assert_eq!(spray_draws(Some("Bear")), 0, "Bear");
    assert_eq!(spray_draws(None), 0, "nothing");
}

/// The parsed card carries no gap, and its condition names all six legs.
#[test]
fn unagis_spray_condition_holds_the_whole_list() {
    let mut scenario = GameScenario::new();
    let spray = scenario
        .add_spell_to_hand_from_oracle(P0, "Unagi's Spray", true, UNAGIS_SPRAY)
        .id();
    let runner = scenario.build();
    let debug = format!("{:?}", runner.state().objects[&spray].abilities);
    assert!(!debug.contains("Unimplemented"), "{debug}");
    for subtype in ["Fish", "Octopus", "Otter", "Seal", "Serpent", "Whale"] {
        assert!(
            debug.contains(&format!("\"{subtype}\"")),
            "{subtype}: {debug}"
        );
    }
}

const RAKDOS_THE_SHOWSTOPPER: &str = "Flying, trample\nWhen Rakdos enters, flip a coin for each creature that isn't a Demon, Devil, or Imp. Destroy each creature whose coin comes up tails.";

/// CR 705.1: Rakdos, the Showstopper's per-creature coin flip isn't modelled,
/// and the destroy clause used to drop "whose coin comes up tails" and destroy
/// EVERY creature. The clause now fails closed: the trigger carries the gap,
/// and Rakdos entering destroys nothing.
#[test]
fn rakdos_the_showstopper_fails_closed_instead_of_destroying_everything() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let bear = scenario.add_creature(P0, "Bear", 2, 2).id();
    let theirs = scenario
        .add_creature(engine::game::scenario::P1, "Their Bear", 2, 2)
        .id();
    let rakdos = scenario
        .add_creature_to_hand(P0, "Rakdos, the Showstopper", 6, 6)
        .with_subtypes(vec!["Demon"])
        .from_oracle_text_with_keywords(&["Flying", "Trample"], RAKDOS_THE_SHOWSTOPPER)
        .with_mana_cost(engine::types::mana::ManaCost::zero())
        .id();
    let mut runner = scenario.build();
    let debug = format!(
        "{:?}",
        runner.state().objects[&rakdos]
            .trigger_definitions
            .iter_unchecked()
            .map(|entry| entry.definition().execute.clone())
            .collect::<Vec<_>>()
    );
    assert!(debug.contains("per_object_coin_flip_outcome"), "{debug}");
    runner.cast(rakdos).resolve();
    for _ in 0..16 {
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } if !runner.state().stack.is_empty() => {
                runner.act(GameAction::PassPriority).expect("pass");
            }
            _ => break,
        }
    }
    assert_eq!(
        runner.state().objects[&rakdos].zone,
        Zone::Battlefield,
        "reach guard"
    );
    assert_eq!(runner.state().objects[&bear].zone, Zone::Battlefield);
    assert_eq!(runner.state().objects[&theirs].zone, Zone::Battlefield);
}
