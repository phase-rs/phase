//! Production-path coverage for an activated random discard cost.

use engine::game::casting::can_activate_ability_now;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{
    AbilityCost, AbilityDefinition, AbilityKind, CardSelectionMode, Effect, ReplacementDefinition,
    ReplacementMode, TargetFilter, TargetRef,
};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::replacements::ReplacementEvent;
use engine::types::zones::{EtbTapState, Zone};

const PYROMANCY: &str = "{3}, Discard a card at random: Pyromancy deals damage to any target equal to the discarded card's mana value.";

fn floating_colorless(count: usize) -> Vec<ManaUnit> {
    (0..count)
        .map(|_| ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]))
        .collect()
}

fn optional_graveyard_exile_replacement() -> ReplacementDefinition {
    ReplacementDefinition::new(ReplacementEvent::Moved)
        .destination_zone(Zone::Graveyard)
        .mode(ReplacementMode::Optional { decline: None })
        .execute(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::ChangeZone {
                origin: None,
                destination: Zone::Exile,
                target: TargetFilter::SelfRef,
                owner_library: false,
                enter_transformed: false,
                enters_under: None,
                enter_tapped: EtbTapState::Unspecified,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
                conditional_enter_with_counters: vec![],
                enters_modified_if: None,
                face_down_profile: None,
            },
        ))
}

fn setup_pyromancy(
    hand_mana_values: &[u32],
    with_replacement: bool,
) -> (GameRunner, ObjectId, Vec<ObjectId>) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, floating_colorless(3));

    if with_replacement {
        scenario
            .add_creature(P1, "Optional Rest in Peace", 1, 1)
            .with_replacement_definition(optional_graveyard_exile_replacement());
    }

    let pyromancy = scenario
        .add_enchantment_from_oracle(P0, "Pyromancy", PYROMANCY)
        .id();
    let hand = hand_mana_values
        .iter()
        .enumerate()
        .map(|(index, mana_value)| {
            scenario
                .add_creature_to_hand(P0, &format!("Random Filler {index}"), 1, 1)
                .with_mana_cost(ManaCost::generic(*mana_value))
                .id()
        })
        .collect();

    (scenario.build(), pyromancy, hand)
}

/// CR 602.2b + CR 601.2h + CR 701.9b: Pyromancy pays mana, then lets the
/// seeded game RNG select its discard without a player card-choice prompt.
#[test]
fn pyromancy_pays_its_random_discard_and_uses_the_discarded_cards_mana_value() {
    let (mut runner, pyromancy, hand) = setup_pyromancy(&[2, 5], false);
    assert!(matches!(
        runner.state().objects[&pyromancy].abilities[0].cost,
        Some(AbilityCost::Composite { ref costs })
            if costs.iter().any(|cost| matches!(
                cost,
                AbilityCost::Discard {
                    selection: CardSelectionMode::Random,
                    ..
                }
            ))
    ));

    let life_before = runner.state().players[P1.0 as usize].life;
    runner.activate(pyromancy, 0).target_player(P1).resolve();

    let discarded = hand
        .iter()
        .filter(|id| runner.state().objects[id].zone == Zone::Graveyard)
        .copied()
        .collect::<Vec<_>>();
    assert_eq!(discarded.len(), 1, "exactly one hand card pays the cost");
    let mana_value = runner.state().objects[&discarded[0]].mana_cost.mana_value() as i32;
    assert_eq!(
        runner.state().players[P1.0 as usize].life,
        life_before - mana_value,
        "the resolved ability must read the pre-move cost-paid snapshot",
    );
    assert_eq!(runner.state().players[P0.0 as usize].mana_pool.total(), 0);
}

/// CR 118.3 + CR 602.2b: the complete random discard cost is checked before
/// the activation spends any of its mana.
#[test]
fn pyromancy_with_an_empty_hand_is_rejected_before_mana_is_spent() {
    let (runner, pyromancy, _) = setup_pyromancy(&[], false);
    assert!(!can_activate_ability_now(runner.state(), P0, pyromancy, 0));
    assert_eq!(runner.state().players[P0.0 as usize].mana_pool.total(), 3);
}

/// CR 616.1 + CR 608.2k: the pre-move snapshot survives the real replacement
/// pause/resume path and is committed once after the random card is delivered.
#[test]
fn pyromancy_snapshot_survives_a_discard_zone_replacement_pause() {
    let (mut runner, pyromancy, hand) = setup_pyromancy(&[5], true);
    let life_before = runner.state().players[P1.0 as usize].life;

    runner
        .act(GameAction::ActivateAbility {
            source_id: pyromancy,
            ability_index: 0,
        })
        .expect("activate Pyromancy");
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Player(P1)),
        })
        .expect("choose Pyromancy's target");

    let WaitingFor::ReplacementChoice { ref candidates, .. } = runner.state().waiting_for else {
        panic!("random cost discard must reach the replacement pause")
    };
    let decline = candidates
        .iter()
        .position(|candidate| candidate.description == "Decline")
        .expect("optional replacement has a decline choice");
    assert!(runner.state().pending_discard_for_cost.is_some());
    runner
        .act(GameAction::ChooseReplacement { index: decline })
        .expect("decline the discard redirect and resume the activation");
    runner.advance_until_stack_empty();

    assert_eq!(runner.state().objects[&hand[0]].zone, Zone::Graveyard);
    assert_eq!(runner.state().players[P1.0 as usize].life, life_before - 5);
    assert!(runner.state().pending_discard_for_cost.is_none());
}
