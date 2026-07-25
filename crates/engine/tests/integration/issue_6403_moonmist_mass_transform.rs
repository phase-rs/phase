//! Runtime regression for issue #6403: Moonmist's "Transform all Humans" is a
//! non-targeting mass instruction, not a request to choose one Human.

use engine::game::game_object::BackFaceData;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::card::LayoutKind;
use engine::types::card_type::{CardType, CoreType};
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaCost};
use engine::types::phase::Phase;

const MOONMIST: &str = "Transform all Humans. Prevent all combat damage that would be dealt this turn by creatures other than Werewolves and Wolves.";

fn attach_transform_back_face(runner: &mut GameRunner, object_id: ObjectId) {
    runner
        .state_mut()
        .objects
        .get_mut(&object_id)
        .unwrap()
        .back_face = Some(BackFaceData {
        name: "Back Face".to_string(),
        power: Some(3),
        toughness: Some(3),
        loyalty: None,
        defense: None,
        card_types: CardType {
            supertypes: vec![],
            core_types: vec![CoreType::Creature],
            subtypes: vec!["Werewolf".to_string()],
        },
        mana_cost: ManaCost::default(),
        keywords: vec![],
        abilities: vec![],
        trigger_definitions: Default::default(),
        replacement_definitions: Default::default(),
        static_definitions: Default::default(),
        color: vec![ManaColor::Green],
        printed_ref: None,
        modal: None,
        additional_cost: None,
        strive_cost: None,
        casting_restrictions: vec![],
        casting_options: vec![],
        // CR 712.16: this is a transforming DFC, so the mass resolver may
        // transform it rather than applying CR 701.27c's no-op.
        layout_kind: Some(LayoutKind::Transform),
    });
}

/// CR 115.10a: Moonmist's "all Humans" identifies a population; it does not
/// use the word "target". The production cast driver receives no target intent
/// here, so a regression back to `EffectScope::Single` stops at TargetSelection
/// instead of resolving. Both Human DFCs then prove the population resolver ran.
#[test]
fn moonmist_cast_transforms_all_humans_without_target_selection() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let human_a = scenario
        .add_creature(P0, "Human A", 2, 2)
        .with_subtypes(vec!["Human"])
        .id();
    let human_b = scenario
        .add_creature(P1, "Human B", 2, 2)
        .with_subtypes(vec!["Human"])
        .id();
    let non_human = scenario
        .add_creature(P1, "Goblin", 2, 2)
        .with_subtypes(vec!["Goblin"])
        .id();
    let moonmist = scenario
        .add_spell_to_hand_from_oracle(P0, "Moonmist", true, MOONMIST)
        .with_mana_cost(ManaCost::zero())
        .id();

    let mut runner = scenario.build();
    attach_transform_back_face(&mut runner, human_a);
    attach_transform_back_face(&mut runner, human_b);

    let outcome = runner.cast(moonmist).resolve();
    let state = outcome.state();

    assert!(
        state.objects[&human_a].transformed,
        "the controller's Human DFC must transform"
    );
    assert!(
        state.objects[&human_b].transformed,
        "an opponent's Human DFC must also transform"
    );
    assert!(
        !state.objects[&non_human].transformed,
        "a non-Human must stay unchanged"
    );
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "Moonmist must resolve without a target-selection prompt"
    );
}
