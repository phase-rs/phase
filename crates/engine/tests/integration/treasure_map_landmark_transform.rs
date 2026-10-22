//! Treasure Map — "{1}, {T}: Scry 1. Put a landmark counter on this artifact.
//! Then if there are three or more landmark counters on it, remove those
//! counters, transform this artifact, and create three Treasure tokens."
//!
//! CR 608.2c: the "if" gates the whole clause and is checked once, as the
//! clause begins. Removing the counters (the first gated instruction) must not
//! make the later gated instructions (transform, create tokens) re-read the
//! now-empty counter total and skip themselves.

use engine::game::game_object::BackFaceData;
use engine::game::scenario::{GameScenario, P0};
use engine::types::actions::GameAction;
use engine::types::card_type::{CardType, CoreType};
use engine::types::counter::CounterType;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaType, ManaUnit};
use engine::types::phase::Phase;

const TREASURE_MAP_ORACLE: &str = "{1}, {T}: Scry 1. Put a landmark counter on this artifact. Then if there are three or more landmark counters on it, remove those counters, transform this artifact, and create three Treasure tokens. (They're artifacts with \"{T}, Sacrifice this token: Add one mana of any color.\")";

fn treasure_cove_back_face() -> BackFaceData {
    BackFaceData {
        is_swap_snapshot: false,
        trigger_printed_origins: Vec::new(),
        name: "Treasure Cove".to_string(),
        power: None,
        toughness: None,
        loyalty: None,
        printed_loyalty: None,
        defense: None,
        card_types: CardType {
            supertypes: vec![],
            core_types: vec![CoreType::Land],
            subtypes: vec![],
        },
        mana_cost: ManaCost::default(),
        keywords: vec![],
        abilities: vec![],
        trigger_definitions: Default::default(),
        replacement_definitions: Default::default(),
        static_definitions: Default::default(),
        color: vec![],
        printed_ref: None,
        modal: None,
        additional_cost: None,
        strive_cost: None,
        casting_restrictions: vec![],
        casting_options: vec![],
        layout_kind: None,
        parse_warnings: vec![],
    }
}

fn activate_with_prior_counters(prior: u32) -> (engine::game::scenario::GameRunner, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let map = scenario
        .add_creature(P0, "Treasure Map", 0, 0)
        .as_artifact()
        .from_oracle_text(TREASURE_MAP_ORACLE)
        .id();
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(
            ManaType::Colorless,
            ObjectId(0),
            false,
            vec![],
        )],
    );
    let mut runner = scenario.build();
    runner.state_mut().active_player = P0;
    runner.state_mut().priority_player = P0;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P0 };
    {
        let obj = runner.state_mut().objects.get_mut(&map).unwrap();
        obj.back_face = Some(treasure_cove_back_face());
        if prior > 0 {
            obj.counters
                .insert(CounterType::Generic("landmark".to_string()), prior);
        }
    }
    runner
        .act(GameAction::ActivateAbility {
            source_id: map,
            ability_index: 0,
        })
        .expect("Treasure Map's ability must be payable ({1}, {T})");
    for _ in 0..20 {
        match &runner.state().waiting_for {
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
            WaitingFor::Priority { .. } => {
                if runner.act(GameAction::PassPriority).is_err() {
                    break;
                }
            }
            WaitingFor::ScryChoice { .. } => {
                runner
                    .act(GameAction::SelectCards { cards: vec![] })
                    .expect("keep scry on top");
            }
            _ => break,
        }
    }
    (runner, map)
}

fn treasure_count(runner: &engine::game::scenario::GameRunner) -> usize {
    runner
        .state()
        .objects
        .values()
        .filter(|o| o.is_token && o.name == "Treasure")
        .count()
}

/// The third landmark counter flips the map, clears the counters and makes
/// three Treasures.
#[test]
fn third_landmark_counter_transforms_and_makes_treasures() {
    let (runner, map) = activate_with_prior_counters(2);
    let obj = &runner.state().objects[&map];
    assert!(
        obj.transformed,
        "Treasure Map must transform at three counters"
    );
    assert_eq!(obj.name, "Treasure Cove");
    assert_eq!(
        obj.counters
            .get(&CounterType::Generic("landmark".to_string()))
            .copied()
            .unwrap_or(0),
        0,
        "the landmark counters are removed"
    );
    assert_eq!(treasure_count(&runner), 3);
}

/// Below the threshold only the counter is added.
#[test]
fn second_landmark_counter_does_not_transform() {
    let (runner, map) = activate_with_prior_counters(1);
    let obj = &runner.state().objects[&map];
    assert!(!obj.transformed);
    assert_eq!(
        obj.counters
            .get(&CounterType::Generic("landmark".to_string()))
            .copied(),
        Some(2)
    );
    assert_eq!(treasure_count(&runner), 0);
}
