//! CR 201.5 + CR 400.7j: a forwarded result is not the printed source.
//! Exercise synchronous self-return and Equipment returned to its original
//! source host.

use engine::game::game_object::AttachTarget;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::events::{GameEvent, PlayerActionKind};
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{parse_mtgjson_mana_cost, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const BEACON: &str = "Put target artifact or creature card from a graveyard onto the battlefield under your control. Shuffle Beacon of Unrest into its owner's library.";
const SNUFFLER: &str = "When this creature enters, return target Equipment card from your graveyard to the battlefield attached to this creature.\nWhenever you sacrifice an artifact, put a +1/+1 counter on this creature.";
const BONESPLITTER: &str = "Equipped creature gets +2/+0.\nEquip {1}";

#[test]
fn beacon_returns_ornithopter_then_shuffles_only_its_own_source() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let beacon = scenario
        .add_spell_to_hand_from_oracle(P0, "Beacon of Unrest", false, BEACON)
        .with_mana_cost(parse_mtgjson_mana_cost("{3}{B}{B}"))
        .id();
    let thopter = scenario
        .add_creature_to_graveyard(P1, "Ornithopter", 0, 2)
        .as_artifact_creature()
        .with_subtypes(vec!["Thopter"])
        .with_mana_cost(parse_mtgjson_mana_cost("{0}"))
        .from_oracle_text_with_keywords(&["Flying"], "Flying")
        .id();
    scenario.with_mana_pool(
        P0,
        [
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Black,
            ManaType::Black,
        ]
        .into_iter()
        .map(|color| ManaUnit::new(color, ObjectId(0), false, vec![]))
        .collect(),
    );
    let mut runner = scenario.build();
    let outcome = runner.cast(beacon).target_object(thopter).resolve();
    assert_eq!(outcome.mana_pool_total(P0), 0);
    let state = outcome.state();
    assert_eq!(state.objects[&thopter].zone, Zone::Battlefield);
    assert_eq!(state.objects[&thopter].owner, P1);
    assert_eq!(state.objects[&thopter].controller, P0);
    assert_eq!(state.objects[&beacon].zone, Zone::Library);
    assert!(state.players[P0.0 as usize].library.contains(&beacon));
    assert!(!state.players[P1.0 as usize].library.contains(&thopter));
    let shuffles: Vec<_> = outcome
        .events()
        .iter()
        .filter_map(|event| match event {
            GameEvent::PlayerPerformedAction {
                player_id,
                action: PlayerActionKind::ShuffledLibrary,
                ..
            } => Some(*player_id),
            _ => None,
        })
        .collect();
    assert_eq!(shuffles, vec![P0]);
    assert!(state.pending_replacement.is_none());
    assert!(state.resolution_stack.is_empty());
    assert!(state.resolving_stack_entry.is_none());
    assert!(state.stack.is_empty());
    assert!(matches!(
        state.waiting_for,
        WaitingFor::Priority { player: P0 }
    ));
}

#[test]
fn magnetic_snuffler_returns_equipment_attached_to_the_original_source() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let equipment = scenario
        .add_artifact_to_graveyard(P0, "Bonesplitter")
        .with_subtypes(vec!["Equipment"])
        .with_mana_cost(parse_mtgjson_mana_cost("{1}"))
        .from_oracle_text(BONESPLITTER)
        .id();
    let snuffler = scenario
        .add_creature_to_hand_from_oracle(P0, "Magnetic Snuffler", 4, 4, SNUFFLER)
        .as_artifact_creature()
        .with_subtypes(vec!["Construct"])
        .with_mana_cost(parse_mtgjson_mana_cost("{5}"))
        .id();
    scenario.with_mana_pool(
        P0,
        (0..5)
            .map(|_| ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]))
            .collect(),
    );
    let mut runner = scenario.build();
    let outcome = runner.cast(snuffler).target_object(equipment).resolve();
    assert_eq!(outcome.mana_pool_total(P0), 0);
    let state = outcome.state();
    assert_eq!(state.objects[&equipment].zone, Zone::Battlefield);
    assert_eq!(state.objects[&snuffler].zone, Zone::Battlefield);
    assert_eq!(
        state.objects[&equipment].attached_to,
        Some(AttachTarget::Object(snuffler))
    );
    assert!(state.objects[&snuffler].attachments.contains(&equipment));
    assert_eq!(state.objects[&snuffler].power, Some(6));
    assert!(state.resolution_stack.is_empty());
    assert!(state.stack.is_empty());
    assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
}
