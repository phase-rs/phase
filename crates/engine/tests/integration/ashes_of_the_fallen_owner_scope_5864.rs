//! Issue #5864 (review follow-up): Ashes of the Fallen grants the chosen creature
//! type to creature cards in YOUR graveyard scoped by OWNERSHIP, not stale
//! last-known control. The parser encodes this as `FilterProp::Owned { You }`
//! (matches `obj.owner` directly), so the continuous effect must reach a card you
//! OWN in your graveyard even if an opponent controlled it when it died, and must
//! NOT reach an opponent-owned card in their graveyard.
//!
//! These drive the production Layer engine (`evaluate_layers`) — the coverage the
//! prior parser-only AST tests did not exercise (CR 400.3 + CR 109.5).

use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::ChosenAttribute;
use engine::types::identifiers::ObjectId;

const ASHES: &str = "As this artifact enters, choose a creature type.\nEach creature card in your graveyard has the chosen creature type in addition to its other types.";

fn subtypes(runner: &GameRunner, id: ObjectId) -> Vec<String> {
    runner.state().objects[&id].card_types.subtypes.clone()
}

#[test]
fn ashes_grants_chosen_type_in_owner_graveyard_by_ownership_not_control() {
    let mut scenario = GameScenario::new();
    let ashes = scenario
        .add_creature(P0, "Ashes of the Fallen", 0, 0)
        .as_artifact()
        .from_oracle_text(ASHES)
        .id();
    // (A) Baseline: a Bear you own, in your graveyard.
    let owned_bear = scenario
        .add_creature_to_graveyard(P0, "Owned Bear", 2, 2)
        .with_subtypes(vec!["Bear"])
        .id();
    // (B) Stolen-then-died: a Bear you OWN that an opponent CONTROLLED when it
    // died. It rests in YOUR graveyard (owner P0) carrying a stale controller (P1).
    let reclaimed_bear = scenario
        .add_creature_to_graveyard(P0, "Reclaimed Bear", 2, 2)
        .with_subtypes(vec!["Bear"])
        .id();
    // (C) An opponent's own graveyard card — must NOT be reached by your Ashes.
    let opponent_bear = scenario
        .add_creature_to_graveyard(P1, "Opponent Bear", 2, 2)
        .with_subtypes(vec!["Bear"])
        .id();

    let mut runner = scenario.build();
    // Simulate the stolen-then-died card: owner stays P0, control is a stale P1.
    runner
        .state_mut()
        .objects
        .get_mut(&reclaimed_bear)
        .unwrap()
        .controller = P1;
    // Ashes' chosen creature type is Zombie.
    runner
        .state_mut()
        .objects
        .get_mut(&ashes)
        .unwrap()
        .chosen_attributes = vec![ChosenAttribute::CreatureType("Zombie".to_string())];

    evaluate_layers(runner.state_mut());

    // (A) A card you own in your graveyard gains the chosen type, additively.
    assert!(
        subtypes(&runner, owned_bear).iter().any(|s| s == "Zombie"),
        "owned graveyard creature must gain the chosen type, got {:?}",
        subtypes(&runner, owned_bear)
    );
    assert!(
        subtypes(&runner, owned_bear).iter().any(|s| s == "Bear"),
        "the grant is additive — the card keeps its own subtype"
    );

    // (B) OWNERSHIP, not control: a card you OWN reached even with a stale opponent
    // controller (control-change LKI must not exclude its owner — CR 109.5).
    assert!(
        subtypes(&runner, reclaimed_bear)
            .iter()
            .any(|s| s == "Zombie"),
        "a creature you OWN in your graveyard must gain the type even with a stale \
         opponent controller (ownership, not control), got {:?}",
        subtypes(&runner, reclaimed_bear)
    );

    // (C) An opponent-owned graveyard card must NOT be reached by your Ashes.
    assert!(
        !subtypes(&runner, opponent_bear)
            .iter()
            .any(|s| s == "Zombie"),
        "an opponent-owned graveyard card must not gain YOUR Ashes' chosen type, got {:?}",
        subtypes(&runner, opponent_bear)
    );
}
