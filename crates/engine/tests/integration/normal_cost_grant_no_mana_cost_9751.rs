//! PR #9751 review — a normal-cost cast grant cannot cast a card with no mana
//! cost (CR 118.6 + CR 601.2h).
//!
//! Emry, Lurker of the Loch's "{T}: Choose target artifact card in your
//! graveyard. You may cast that card this turn." grants a cast at the card's
//! printed cost. A card with no mana cost (Lotus Bloom) has an unpayable cost,
//! so the grant cannot cast it; a card costing {0} is payable and can be cast.

use engine::ai_support::legal_actions;
use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::types::ability::{CastingPermission, ExileGrantCostProvenance};
use engine::types::actions::GameAction;
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const EMRY_ABILITY: &str =
    "{T}: Choose target artifact card in your graveyard. You may cast that card this turn.";

/// Emry's ability resolved targeting an artifact in P0's graveyard with the
/// given printed cost.
fn granted(cost: ManaCost) -> (GameRunner, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let emry = scenario
        .add_creature_from_oracle(P0, "Emry", 1, 2, EMRY_ABILITY)
        .id();
    let artifact = scenario
        .add_creature_to_graveyard(P0, "Graveyard Trinket", 0, 0)
        .as_artifact()
        .with_mana_cost(cost.clone())
        .id();
    let mut runner = scenario.build();
    runner.activate(emry, 0).target_object(artifact).resolve();
    let object = &runner.state().objects[&artifact];
    assert_eq!(object.zone, Zone::Graveyard);
    assert_eq!(object.mana_cost, cost, "reach guard: printed cost");
    assert!(
        object.casting_permissions.iter().any(|permission| matches!(
            permission,
            CastingPermission::ExileWithAltCost {
                cost_provenance: ExileGrantCostProvenance::NormalCost,
                ..
            }
        )),
        "reach guard: Emry granted a normal-cost cast: {:?}",
        object.casting_permissions
    );
    (runner, artifact)
}

/// Whether the legal actions include casting `card`.
fn offers_cast(runner: &GameRunner, card: ObjectId) -> bool {
    legal_actions(runner.state()).iter().any(
        |action| matches!(action, GameAction::CastSpell { object_id, .. } if *object_id == card),
    )
}

/// CR 118.6 + CR 601.2h: the unpayable cost can't be paid, so the card is not
/// castable and a cast attempt is refused.
#[test]
fn a_normal_cost_grant_cannot_cast_a_card_with_no_mana_cost() {
    let (mut runner, artifact) = granted(ManaCost::NoCost);
    assert!(!offers_cast(&runner, artifact), "no legal cast is offered");
    assert!(runner.cast(artifact).try_resolve().is_err());
    assert_eq!(runner.state().objects[&artifact].zone, Zone::Graveyard);
}

/// Control: a printed {0} is payable, so the same grant casts it.
#[test]
fn a_normal_cost_grant_casts_a_card_costing_zero() {
    let (mut runner, artifact) = granted(ManaCost::zero());
    assert!(offers_cast(&runner, artifact), "the cast is offered");
    let outcome = runner.cast(artifact).resolve();
    assert_eq!(outcome.zone_of(artifact), Zone::Battlefield);
}

/// An exiled card with a stored `SelfManaCost` grant whose provenance is the
/// serde default (`Alternative`), as in a game saved before provenance existed.
fn exiled_with_self_mana_cost_grant(cost: ManaCost) -> (GameRunner, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let card = scenario
        .add_creature_to_exile(P0, "Exiled Trinket", 1, 1)
        .as_artifact()
        .with_mana_cost(cost)
        .id();
    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&card)
        .unwrap()
        .casting_permissions
        .push(CastingPermission::ExileWithAltCost {
            cost: ManaCost::SelfManaCost,
            cost_provenance: ExileGrantCostProvenance::Alternative,
            cast_transformed: false,
            constraint: None,
            granted_to: Some(P0),
            resolution_cleanup: None,
            duration: None,
            source_id: None,
            graveyard_replacement: None,
            enters_with_counter: None,
            enters_with_modifications: Vec::new(),
            mana_spend_permission: None,
            cast_cost_modifier: None,
        });
    (runner, card)
}

/// CR 118.6: a stored `SelfManaCost` restates the printed cost whatever its
/// provenance, so it cannot cast a card with no mana cost either.
#[test]
fn a_stored_self_mana_cost_grant_cannot_cast_a_card_with_no_mana_cost() {
    let (mut runner, card) = exiled_with_self_mana_cost_grant(ManaCost::NoCost);
    assert!(!offers_cast(&runner, card), "no legal cast is offered");
    assert!(runner.cast(card).try_resolve().is_err());
    assert_eq!(runner.state().objects[&card].zone, Zone::Exile);
}

/// Control: the same stored grant casts a card costing {0}.
#[test]
fn a_stored_self_mana_cost_grant_casts_a_card_costing_zero() {
    let (mut runner, card) = exiled_with_self_mana_cost_grant(ManaCost::zero());
    assert!(offers_cast(&runner, card), "the cast is offered");
    let outcome = runner.cast(card).resolve();
    assert_eq!(outcome.zone_of(card), Zone::Battlefield);
}
