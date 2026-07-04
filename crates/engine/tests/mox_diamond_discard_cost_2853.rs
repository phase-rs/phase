//! Mox Diamond — the as-enters "you may discard a land card instead" cost must
//! actually discard a land when accepted, and only then does the artifact enter.
//!
//! Regression for issue #2853: the `MayCost { Discard }` replacement parsed
//! correctly (accept/decline branches present), but the runtime accept path
//! routed the discard through `pay_ability_cost` in *activation* scope. The
//! `Discard { FromHand, Chosen }` shape is only paid in *resolution* scope; in
//! activation scope it fell through to the interactive-pass-through arm and
//! returned `Paid` as a silent no-op. The land was never discarded, yet the
//! artifact still entered (the cost was skipped). These tests drive the real
//! parsed replacement through the as-enters pipeline and assert the land is
//! discarded on accept, kept on decline, and that an unpayable accept routes
//! the artifact to the graveyard.
//!
//! CR 614.12a / CR 614.12: a declined/unpayable alternative routes to the
//! owner's graveyard; CR 701.9a: discarding moves a card from hand to graveyard.

use engine::game::effects::change_zone::resolve;
use engine::game::scenario::{GameScenario, P0};
use engine::types::ability::{
    AbilityCost, QuantityExpr, ReplacementMode, ResolvedAbility, TargetFilter,
};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const MOX_DIAMOND_ORACLE: &str =
    "If this artifact would enter, you may discard a land card instead. If you do, \
     put this artifact onto the battlefield. If you don't, put it into its owner's \
     graveyard.";

/// Move a card from hand to the battlefield, surfacing its as-enters `MayCost`
/// replacement choice (mirrors the production ETB pathway for Mox Diamond).
fn enter_via_change_zone(runner_state: &mut engine::types::game_state::GameState, card: ObjectId) {
    let ability = ResolvedAbility::new(
        engine::types::ability::Effect::ChangeZone {
            origin: Some(Zone::Hand),
            destination: Zone::Battlefield,
            target: TargetFilter::SpecificObject { id: card },
            owner_library: false,
            enter_transformed: false,
            enters_under: None,
            enter_tapped: engine::types::zones::EtbTapState::Unspecified,
            enters_attacking: false,
            up_to: false,
            enter_with_counters: vec![],
            conditional_enter_with_counters: vec![],
            face_down_profile: None,
            enters_modified_if: None,
        },
        vec![],
        ObjectId(9000),
        PlayerId(0),
    );
    let mut events = Vec::new();
    resolve(runner_state, &ability, &mut events)
        .expect("Mox Diamond enter resolves into a MayCost pause");
}

/// CR 614.12a + CR 701.9a: accepting the discard cost must discard a land from
/// hand and only then put Mox Diamond onto the battlefield.
#[test]
fn mox_diamond_accept_discards_land_and_enters() {
    let mut scenario = GameScenario::new();
    let mox = scenario
        .add_creature_to_hand(P0, "Mox Diamond", 0, 0)
        .as_artifact()
        .from_oracle_text(MOX_DIAMOND_ORACLE)
        .id();
    let forest = scenario.add_land_to_hand(P0, "Forest").id();
    let mut runner = scenario.build();

    enter_via_change_zone(runner.state_mut(), mox);

    // The as-enters replacement pauses for the accept/decline choice.
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ),
        "expected Mox Diamond MayCost ReplacementChoice, got {:?}",
        runner.state().waiting_for
    );

    // Accept (index 0): pay the discard cost. With exactly one land in hand the
    // discard is forced — no further choice round-trip.
    runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("accepting the Mox Diamond discard cost should resolve");

    assert!(
        !matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ),
        "the replacement choice must be consumed after accepting"
    );
    assert_eq!(
        runner.state().objects[&forest].zone,
        Zone::Graveyard,
        "accepting the cost must discard the land to its owner's graveyard, \
         not leave it in hand"
    );
    assert_eq!(
        runner.state().objects[&mox].zone,
        Zone::Battlefield,
        "after paying the discard cost, Mox Diamond enters the battlefield"
    );
}

/// CR 614.12a: declining sends Mox Diamond to its owner's graveyard and never
/// discards a land.
#[test]
fn mox_diamond_decline_routes_to_graveyard_without_discard() {
    let mut scenario = GameScenario::new();
    let mox = scenario
        .add_creature_to_hand(P0, "Mox Diamond", 0, 0)
        .as_artifact()
        .from_oracle_text(MOX_DIAMOND_ORACLE)
        .id();
    let forest = scenario.add_land_to_hand(P0, "Forest").id();
    let mut runner = scenario.build();

    enter_via_change_zone(runner.state_mut(), mox);

    let WaitingFor::ReplacementChoice { candidates, .. } = &runner.state().waiting_for else {
        panic!(
            "expected Mox Diamond MayCost ReplacementChoice, got {:?}",
            runner.state().waiting_for
        );
    };
    let decline = candidates
        .iter()
        .position(|c| c.description.contains("Decline"))
        .expect("decline option must be offered");

    runner
        .act(GameAction::ChooseReplacement { index: decline })
        .expect("declining should resolve");

    assert_eq!(
        runner.state().objects[&mox].zone,
        Zone::Graveyard,
        "a declined Mox Diamond is routed to its owner's graveyard"
    );
    assert_eq!(
        runner.state().objects[&forest].zone,
        Zone::Hand,
        "declining must not discard a land"
    );
}

/// CR 614.12a: accepting with no land to discard is an unpayable cost — it falls
/// through to the decline branch (Mox to graveyard), never entering for free.
#[test]
fn mox_diamond_accept_unpayable_routes_to_graveyard() {
    let mut scenario = GameScenario::new();
    let mox = scenario
        .add_creature_to_hand(P0, "Mox Diamond", 0, 0)
        .as_artifact()
        .from_oracle_text(MOX_DIAMOND_ORACLE)
        .id();
    let mut runner = scenario.build();

    enter_via_change_zone(runner.state_mut(), mox);

    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ),
        "expected Mox Diamond MayCost ReplacementChoice, got {:?}",
        runner.state().waiting_for
    );

    runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("accepting an unpayable cost should resolve");

    assert_eq!(
        runner.state().objects[&mox].zone,
        Zone::Graveyard,
        "an unpayable Mox Diamond discard cost falls through to the graveyard \
         redirect — it must never enter the battlefield for free"
    );
}

/// CR 701.9a: with more than one eligible land, the discard requires a genuine
/// choice. The accept path must surface that choice (not silently auto-pick or
/// no-op); only after a land is actually discarded does Mox Diamond enter.
#[test]
fn mox_diamond_accept_with_multiple_lands_requires_choice() {
    let mut scenario = GameScenario::new();
    let mox = scenario
        .add_creature_to_hand(P0, "Mox Diamond", 0, 0)
        .as_artifact()
        .from_oracle_text(MOX_DIAMOND_ORACLE)
        .id();
    let forest_a = scenario.add_land_to_hand(P0, "Forest").id();
    let forest_b = scenario.add_land_to_hand(P0, "Forest").id();
    let mut runner = scenario.build();

    enter_via_change_zone(runner.state_mut(), mox);

    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ),
        "expected Mox Diamond MayCost ReplacementChoice, got {:?}",
        runner.state().waiting_for
    );

    runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("accepting the discard cost should resolve");

    // Two lands: the cost is not forced, so the engine must ask which land to
    // discard before the artifact can enter.
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::DiscardChoice { .. }),
        "expected a DiscardChoice for the two-land case, got {:?}",
        runner.state().waiting_for
    );

    runner
        .act(GameAction::SelectCards {
            cards: vec![forest_a],
        })
        .expect("selecting the land to discard should resolve");

    assert_eq!(
        runner.state().objects[&forest_a].zone,
        Zone::Graveyard,
        "the chosen land is discarded"
    );
    assert_eq!(
        runner.state().objects[&forest_b].zone,
        Zone::Hand,
        "the un-chosen land stays in hand"
    );
    assert_eq!(
        runner.state().objects[&mox].zone,
        Zone::Battlefield,
        "Mox Diamond enters after the discard choice is committed"
    );
}

/// CR 614.12a + CR 118.12: if a composite MayCost pauses for an interactive
/// discard choice, the post-choice resume must still pay the remaining suffix
/// before the replacement applies.
#[test]
fn may_cost_discard_choice_resume_pays_remaining_composite_suffix() {
    let mut scenario = GameScenario::new();
    let mox = scenario
        .add_creature_to_hand(P0, "Mox Diamond", 0, 0)
        .as_artifact()
        .from_oracle_text(MOX_DIAMOND_ORACLE)
        .id();
    let forest_a = scenario.add_land_to_hand(P0, "Forest").id();
    let forest_b = scenario.add_land_to_hand(P0, "Forest").id();

    let mut runner = scenario.build();

    {
        let obj = runner.state_mut().objects.get_mut(&mox).unwrap();
        let replacement_index = obj
            .replacement_definitions
            .iter_unchecked()
            .position(|definition| matches!(definition.mode, ReplacementMode::MayCost { .. }))
            .expect("Mox Diamond replacement should parse as MayCost");
        let replacement = &mut obj.replacement_definitions[replacement_index];
        let (discard_cost, decline) = match &replacement.mode {
            ReplacementMode::MayCost { cost, decline } => (cost.clone(), decline.clone()),
            other => panic!("expected MayCost, got {other:?}"),
        };
        replacement.mode = ReplacementMode::MayCost {
            cost: AbilityCost::Composite {
                costs: vec![
                    discard_cost,
                    AbilityCost::PayLife {
                        amount: QuantityExpr::Fixed { value: 2 },
                    },
                ],
            },
            decline,
        };
    }

    enter_via_change_zone(runner.state_mut(), mox);

    runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("accepting the composite MayCost should surface discard choice");
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::DiscardChoice { .. }),
        "expected a DiscardChoice for the two-land composite case, got {:?}",
        runner.state().waiting_for
    );

    runner
        .act(GameAction::SelectCards {
            cards: vec![forest_a],
        })
        .expect("selecting the land to discard should resume the remaining cost");

    assert_eq!(
        runner.state().objects[&forest_a].zone,
        Zone::Graveyard,
        "the chosen land is discarded"
    );
    assert_eq!(
        runner.state().objects[&forest_b].zone,
        Zone::Hand,
        "the un-chosen land stays in hand"
    );
    assert_eq!(
        runner.state().players[0].life,
        18,
        "the remaining PayLife suffix must be paid after the discard choice"
    );
    assert_eq!(
        runner.state().objects[&mox].zone,
        Zone::Battlefield,
        "Mox Diamond enters only after all composite cost components are paid"
    );
}
