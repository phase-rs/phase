//! CR 701.71a: the FRA Jace planeswalker token, created through the real
//! debug-preset `CreateToken` pipeline (`DebugAction::CreateToken` /
//! `DebugTokenRequest::Preset`), which routes through the CR 614 replacement
//! pipeline and the shared token body installer.
//!
//! Every creation below passes `run_etb: false`. With `run_etb: true` the
//! handler runs a state-based-action pass immediately after creation, and a
//! 0-loyalty planeswalker token is put into its owner's graveyard by
//! CR 704.5i before any assertion runs. The row that observes CR 704.5i gets
//! its SBA pass from an explicit `DebugAction::RunStateBasedActions` instead,
//! so the before/after pair stays observable.

use engine::ai_support::legal_actions;
use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::game::token_presets::known_token_preset_by_id;
use engine::types::ability::AbilityCost;
use engine::types::actions::{DebugAction, DebugTokenRequest, GameAction};
use engine::types::card::PrintedLoyalty;
use engine::types::card_type::CoreType;
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;
use engine::types::game_state::ActionResult;
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaColor;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

/// FRA Jace planeswalker token (MTGJSON uuid).
const JACE_TOKEN_PRESET_ID: &str = "635f825d-d6fb-59ac-a807-af08713a3794";
/// FRA Thopter token — a non-planeswalker preset from the same catalog.
const FRA_THOPTER_PRESET_ID: &str = "a2da69c3-8afb-5c15-b077-7d18dd32b93c";

fn debug_runner() -> GameRunner {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mut runner = scenario.build();
    runner.state_mut().debug_mode = true;
    runner
}

/// Creates one token from `preset_id` for `P0` with `run_etb: false` and
/// returns the new object's id together with the action result.
fn create_preset(
    runner: &mut GameRunner,
    preset_id: &str,
    enter_with_counters: Vec<(CounterType, u32)>,
) -> (ObjectId, ActionResult) {
    let result = runner
        .act(GameAction::Debug(DebugAction::CreateToken {
            request: DebugTokenRequest::Preset {
                preset_id: preset_id.to_string(),
                owner: P0,
                power_override: None,
                toughness_override: None,
                enter_with_counters,
            },
            count: 1,
            run_etb: false,
        }))
        .expect("debug CreateToken must succeed");
    let created = runner.state().last_created_token_ids.clone();
    assert_eq!(created.len(), 1, "exactly one token must be created");
    (created[0], result)
}

/// Indices of `source`'s abilities that the legal-action enumeration offers.
fn enumerated_ability_indices(runner: &GameRunner, source: ObjectId) -> Vec<usize> {
    let mut indices: Vec<usize> = legal_actions(runner.state())
        .iter()
        .filter_map(|action| match action {
            GameAction::ActivateAbility {
                source_id,
                ability_index,
            } if *source_id == source => Some(*ability_index),
            _ => None,
        })
        .collect();
    indices.sort_unstable();
    indices
}

/// CR 701.71a + CR 306.5b: the Jace token arrives with printed loyalty 0 as a
/// present value (`Some(0)`), not an absent one; a non-planeswalker token from
/// the same catalog carries no loyalty at all.
#[test]
fn jace_token_enters_with_zero_loyalty_and_thopter_with_none() {
    let mut runner = debug_runner();
    let (jace, _) = create_preset(&mut runner, JACE_TOKEN_PRESET_ID, Vec::new());
    let (thopter, _) = create_preset(&mut runner, FRA_THOPTER_PRESET_ID, Vec::new());

    let jace_obj = &runner.state().objects[&jace];
    assert_eq!(jace_obj.zone, Zone::Battlefield);
    assert_eq!(jace_obj.loyalty, Some(0));

    let thopter_obj = &runner.state().objects[&thopter];
    assert_eq!(thopter_obj.zone, Zone::Battlefield);
    assert!(thopter_obj
        .card_types
        .core_types
        .contains(&CoreType::Creature));
    assert_eq!(thopter_obj.loyalty, None);
    assert_eq!(thopter_obj.printed_loyalty, None);
}

/// CR 701.71a + CR 306.3 + CR 306.5b: the full body of the Jace token, and
/// the debug preset path's post-creation catalog injection adds nothing on top
/// of the two registry loyalty abilities.
#[test]
fn jace_token_body_and_catalog_channel_injects_nothing() {
    // Reach-guard for the catalog-injection entry point: the debug handler only
    // calls `inject_catalog_token_abilities` when the preset carries an image ref.
    let preset = known_token_preset_by_id(JACE_TOKEN_PRESET_ID).expect("FRA Jace token preset");
    assert!(
        preset.token_image_ref.is_some(),
        "the Jace preset must carry a token image ref, or catalog injection never runs"
    );

    let mut runner = debug_runner();
    let (jace, result) = create_preset(&mut runner, JACE_TOKEN_PRESET_ID, Vec::new());

    assert!(
        result.events.iter().any(|event| matches!(
            event,
            GameEvent::TokenCreated { object_id, .. } if *object_id == jace
        )),
        "a TokenCreated event must name the Jace token"
    );

    let obj = &runner.state().objects[&jace];
    assert_eq!(obj.zone, Zone::Battlefield);
    assert_eq!(
        obj.token_image_ref
            .as_ref()
            .map(|image_ref| image_ref.preset_id.as_str()),
        Some(JACE_TOKEN_PRESET_ID),
        "the debug handler must have linked the preset before catalog injection"
    );

    // Body.
    assert!(obj.card_types.core_types.contains(&CoreType::Planeswalker));
    assert!(obj
        .card_types
        .subtypes
        .iter()
        .any(|subtype| subtype == "Jace"));
    assert_eq!(obj.color, vec![ManaColor::Blue]);

    // All four loyalty fields, each asserted separately.
    assert_eq!(obj.loyalty, Some(0));
    assert_eq!(obj.printed_loyalty, Some(PrintedLoyalty::Fixed(0)));
    assert_eq!(obj.base_loyalty, Some(0));
    assert_eq!(obj.base_printed_loyalty, Some(PrintedLoyalty::Fixed(0)));

    // Catalog channel injected nothing: exactly the two registry abilities.
    assert_eq!(obj.abilities.len(), 2);
    let loyalty_costs: Vec<i32> = obj
        .abilities
        .iter()
        .filter_map(|ability| match ability.cost {
            Some(AbilityCost::Loyalty { amount }) => Some(amount),
            _ => None,
        })
        .collect();
    assert_eq!(loyalty_costs, vec![-1, -3]);
    assert!(obj.static_definitions.is_empty());
    assert!(obj.trigger_definitions.is_empty());
    assert!(obj.keywords.is_empty());

    // The registry display text, without the catalog's reminder parenthetical.
    assert_eq!(
        obj.token_rules_text,
        Some("[−1]: Surveil 1.\n[−3]: Draw a card.".to_string())
    );
}

/// CR 704.5i + CR 306.9: a Jace token with loyalty 0 is put into its owner's
/// graveyard by the state-based-action check; with one loyalty counter it
/// survives the same check.
#[test]
fn zero_loyalty_jace_token_dies_to_state_based_actions() {
    let mut runner = debug_runner();
    let (jace, _) = create_preset(&mut runner, JACE_TOKEN_PRESET_ID, Vec::new());
    assert_eq!(
        runner.state().objects[&jace].zone,
        Zone::Battlefield,
        "reach-guard: the token must be on the battlefield before the SBA pass"
    );

    let result = runner
        .act(GameAction::Debug(DebugAction::RunStateBasedActions))
        .expect("debug SBA pass must succeed");

    assert!(
        result.events.iter().any(|event| matches!(
            event,
            GameEvent::ZoneChanged { object_id, from: Some(Zone::Battlefield), to: Zone::Graveyard, .. }
                if *object_id == jace
        )),
        "CR 704.5i must move the 0-loyalty Jace token from the battlefield to the graveyard"
    );
    // CR 704.5d: the token then ceases to exist in the same SBA loop, so the
    // graveyard does not retain it; the zone-change event above is the witness.
    assert!(!runner.state().battlefield.contains(&jace));
    assert!(!runner.state().players[0].graveyard.contains(&jace));
}

/// Paired control for the CR 704.5i row: the same token with one loyalty
/// counter is not put into the graveyard by the same SBA pass.
#[test]
fn jace_token_with_a_loyalty_counter_survives_state_based_actions() {
    let mut runner = debug_runner();
    let (jace, _) = create_preset(
        &mut runner,
        JACE_TOKEN_PRESET_ID,
        vec![(CounterType::Loyalty, 1)],
    );

    runner
        .act(GameAction::Debug(DebugAction::RunStateBasedActions))
        .expect("debug SBA pass must succeed");

    let obj = &runner.state().objects[&jace];
    assert_eq!(obj.zone, Zone::Battlefield);
    assert_eq!(obj.loyalty, Some(1));
}

/// CR 606.3 + CR 606.6: with one loyalty counter, only `[−1]` is offered; the
/// live loyalty tracks the counter while the printed loyalty stays 0
/// (CR 306.5b / CR 306.5c).
///
/// This row does not discriminate the entry loyalty seeding: the activation
/// path reads absent loyalty as zero and the counter sync overwrites the field.
#[test]
fn jace_token_loyalty_abilities_are_enumerated_by_cost() {
    let mut runner = debug_runner();
    let (jace, _) = create_preset(
        &mut runner,
        JACE_TOKEN_PRESET_ID,
        vec![(CounterType::Loyalty, 1)],
    );

    let obj = &runner.state().objects[&jace];
    assert_eq!(
        obj.loyalty,
        Some(1),
        "reach-guard: the entry counter must reach the derived loyalty"
    );
    assert_eq!(obj.printed_loyalty, Some(PrintedLoyalty::Fixed(0)));

    assert_eq!(enumerated_ability_indices(&runner, jace), vec![0]);
}

/// CR 606.6: at loyalty 0 neither loyalty ability is affordable.
#[test]
fn zero_loyalty_jace_token_offers_no_loyalty_ability() {
    let mut runner = debug_runner();
    let (jace, _) = create_preset(&mut runner, JACE_TOKEN_PRESET_ID, Vec::new());
    assert_eq!(runner.state().objects[&jace].zone, Zone::Battlefield);
    assert_eq!(runner.state().objects[&jace].loyalty, Some(0));

    assert!(enumerated_ability_indices(&runner, jace).is_empty());
}

/// CR 606.3: after `[−1]` has been activated and resolved this turn, the token
/// still has loyalty to pay another `[−1]` but no loyalty ability is offered.
#[test]
fn jace_token_loyalty_abilities_respect_once_per_turn() {
    let mut runner = debug_runner();
    let (jace, _) = create_preset(
        &mut runner,
        JACE_TOKEN_PRESET_ID,
        vec![(CounterType::Loyalty, 2)],
    );
    assert_eq!(
        enumerated_ability_indices(&runner, jace),
        vec![0],
        "reach-guard: [−1] must be offered before the activation"
    );

    runner
        .act(GameAction::ActivateAbility {
            source_id: jace,
            ability_index: 0,
        })
        .expect("activating [−1] must succeed");
    runner.advance_until_stack_empty();

    let obj = &runner.state().objects[&jace];
    assert_eq!(obj.zone, Zone::Battlefield);
    assert_eq!(obj.loyalty, Some(1), "[−1] must have been paid");
    assert_eq!(obj.loyalty_activations_this_turn, 1);
    assert!(runner.state().stack.is_empty());

    assert!(enumerated_ability_indices(&runner, jace).is_empty());
}
