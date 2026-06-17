//! Regression for issue #1301: activating Cauldron of Essence's
//! "{1}{B}{G}, {T}, Sacrifice a creature: Return target creature card from
//! your graveyard to the battlefield" ability let the engine auto-select the
//! permanent that was just sacrificed to pay the ability's own cost as the
//! return target.
//!
//! https://github.com/phase-rs/phase/issues/1301
//!
//! Per CR 601.2c / CR 602.2b, targets are chosen before costs are paid, so
//! the sacrificed creature was never in the graveyard at the moment legal
//! targets were determined. The card's own Oracle rulings confirm this
//! explicitly: "Because targets are chosen before costs are paid, the target
//! of Cauldron of Essence's last ability can't be the creature sacrificed to
//! pay its cost." This engine pays non-self Sacrifice/Discard/Exile
//! activation costs BEFORE target selection as a documented shortcut (see
//! `push_activated_ability_to_stack` in `casting_costs.rs`), which let the
//! just-sacrificed permanent slip back in as a legal "creature card in your
//! graveyard" target. The fix excludes a cost-paid object that left the
//! battlefield from legal targets for any other slot of the same activation
//! (`exclude_cost_paid_object_that_left_battlefield` in `ability_utils.rs`).

use engine::game::engine::apply_as_current;
use engine::game::scenario::{GameScenario, P0};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::{PayCostKind, WaitingFor};
use engine::types::mana::ManaColor;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

fn issue_1301_db() -> &'static engine::database::card_db::CardDatabase {
    static DB: std::sync::OnceLock<engine::database::card_db::CardDatabase> =
        std::sync::OnceLock::new();
    DB.get_or_init(|| {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/issue_1301_cauldron_of_essence_cards.json");
        engine::database::card_db::CardDatabase::from_export(&path)
            .expect("issue_1301_cauldron_of_essence_cards.json fixture must load")
    })
}

/// Baseline: with a legal target independent of the sacrifice fodder (a
/// creature card already in the graveyard), activation pays {1}{B}{G} via
/// real auto-tapped lands, sacrifices the token, and resolves normally.
#[test]
fn cauldron_of_essence_activation_pays_mana_and_resolves() {
    let db = issue_1301_db();

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let cauldron = scenario.add_real_card(P0, "Cauldron of Essence", Zone::Battlefield, db);
    // CR 601.2g: lands that must be auto-tapped for {1}{B}{G}, exercising the
    // real auto-tap payment path rather than a pre-funded mana pool.
    scenario.add_basic_land(P0, ManaColor::Black);
    scenario.add_basic_land(P0, ManaColor::Green);
    scenario.add_basic_land(P0, ManaColor::White);
    // Sacrifice fodder — mirrors a Human token from "Bring Back".
    let token = scenario.add_creature(P0, "Human Token", 1, 1).id();
    let buried_creature = scenario
        .add_creature_to_graveyard(P0, "Buried Creature", 2, 2)
        .id();

    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    apply_as_current(
        runner.state_mut(),
        GameAction::ActivateAbility {
            source_id: cauldron,
            ability_index: 0,
        },
    )
    .expect("activating Cauldron of Essence should be legal with lands and a sacrifice target");

    match &runner.state().waiting_for {
        WaitingFor::PayCost {
            kind: PayCostKind::Sacrifice,
            ..
        } => {}
        other => panic!("expected PayCost Sacrifice detour, got {:?}", other),
    }

    apply_as_current(
        runner.state_mut(),
        GameAction::SelectCards { cards: vec![token] },
    )
    .expect("sacrificing the token should pay the {1}{B}{G} + tap + sacrifice cost in full");

    assert!(
        runner.state().objects[&cauldron].tapped,
        "Cauldron of Essence should be tapped as part of its own activation cost"
    );

    // The token is excluded from legal targets (it's the cost-paid object that
    // just left the battlefield), leaving the buried creature as the sole
    // legal target. `auto_select_targets_for_ability` commits it without an
    // interactive `TargetSelection` round-trip, landing straight on the
    // post-announcement `Priority` window with the ability on the stack.
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "ability should be on the stack after the sole legal target auto-resolves, got {:?}",
        runner.state().waiting_for
    );
    let stack_entry = runner
        .state()
        .stack
        .iter()
        .find(|entry| entry.source_id == cauldron)
        .expect("Cauldron of Essence's activated ability should be on the stack");
    let engine::types::game_state::StackEntryKind::ActivatedAbility { ability, .. } =
        &stack_entry.kind
    else {
        panic!(
            "expected an ActivatedAbility stack entry, got {:?}",
            stack_entry.kind
        );
    };
    assert_eq!(
        engine::game::ability_utils::flatten_targets_in_chain(ability),
        vec![TargetRef::Object(buried_creature)],
        "the buried creature (not the sacrificed token) must be the auto-selected target"
    );
}

/// Mirrors the Discord repro shape: no creature card sits in the graveyard
/// before activation, so the only thing that could ever populate "a creature
/// card in your graveyard" is the very token being sacrificed to pay the
/// ability's own cost. Per CR 601.2c / CR 602.2b and the card's own ruling,
/// that creature is not a legal target — activation must fail rather than
/// silently completing against an illegal target.
#[test]
fn cauldron_of_essence_rejects_sacrificed_creature_as_its_own_return_target() {
    let db = issue_1301_db();

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let cauldron = scenario.add_real_card(P0, "Cauldron of Essence", Zone::Battlefield, db);
    scenario.add_basic_land(P0, ManaColor::Black);
    scenario.add_basic_land(P0, ManaColor::Green);
    scenario.add_basic_land(P0, ManaColor::White);
    let token = scenario.add_creature(P0, "Human Token", 1, 1).id();
    // A second creature on the battlefield (not in the graveyard) confirms
    // the rejection is about zone/legality, not merely "no other creature
    // exists at all".
    scenario.add_creature(P0, "Human Token 2", 1, 1).id();

    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    apply_as_current(
        runner.state_mut(),
        GameAction::ActivateAbility {
            source_id: cauldron,
            ability_index: 0,
        },
    )
    .expect("activating Cauldron of Essence should be legal with lands and a sacrifice target");

    let result = apply_as_current(
        runner.state_mut(),
        GameAction::SelectCards { cards: vec![token] },
    );

    assert!(
        result.is_err(),
        "sacrificing the only creature card that could become a graveyard target must fail \
         the activation rather than silently targeting the sacrificed creature, got {:?}",
        result
    );
}
