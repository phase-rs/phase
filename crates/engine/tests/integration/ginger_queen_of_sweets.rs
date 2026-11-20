//! Issue #10983: Ginger, Queen of Sweets' upkeep trigger must create a
//! Gingerbrute token when you're the monarch (CR 603.4 intervening-if,
//! CR 111.1 token creation, CR 111.3 token text).

use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{AbilityCost, AbilityKind, Effect, SacrificeCost, TargetFilter};
use engine::types::card_type::CoreType;
use engine::types::keywords::Keyword;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::triggers::TriggerMode;
use engine::types::zones::Zone;
use engine::types::PlayerId;

// Full verbatim MTGJSON Oracle text (card-data.json "ginger, queen of sweets").
const GINGER_ORACLE: &str = "When Ginger enters, you become the monarch.\n{2}, {T}, Sacrifice Ginger: You gain 6 life.\nAt the beginning of each upkeep, if you're the monarch, create a Gingerbrute token. (It's a {1} 1/1 Food Golem artifact creature with haste, \"{1}: This token can't be blocked this turn except by creatures with haste,\" and \"{2}, {T}, Sacrifice this token: You gain 3 life.\")";

// The upkeep line alone, for runtime fixtures: the ETB line would otherwise
// crown P0 mid-test and confound the not-monarch negative below. Per-line
// dispatch parses this identically to line 3 of the full text (the reminder
// parenthetical is stripped before dispatch either way).
const GINGER_UPKEEP: &str =
    "At the beginning of each upkeep, if you're the monarch, create a Gingerbrute token.";

fn build_runner(monarch: Option<PlayerId>) -> GameRunner {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::Untap);
    scenario
        .add_creature(P0, "Ginger, Queen of Sweets", 6, 4)
        .from_oracle_text(GINGER_UPKEEP);
    let mut runner = scenario.build();
    runner.state_mut().monarch = monarch;
    runner
}

#[test]
fn ginger_creates_gingerbrute_when_you_are_monarch() {
    let mut runner = build_runner(Some(P0));
    runner.advance_to_upkeep();
    runner.advance_until_stack_empty();
    let token_id = *runner
        .state()
        .last_created_token_ids
        .first()
        .expect("CR 111.1: Ginger's upkeep trigger must create the Gingerbrute token");
    let token = runner
        .state()
        .objects
        .get(&token_id)
        .expect("the created token exists on the battlefield");
    assert!(token.is_token, "the created object is a token");
    assert_eq!(token.zone, Zone::Battlefield);
    assert_eq!(token.controller, P0);
    // F1: the upkeep-created token is named exactly `Gingerbrute` (CR 111.11)
    // and keeps its preset link and abilities.
    assert_eq!(
        token.name, "Gingerbrute",
        "token carries exactly the Gingerbrute card name"
    );
    assert_eq!(token.power, Some(1), "CR 111.3: 1/1 from the token text");
    assert_eq!(
        token.toughness,
        Some(1),
        "CR 111.3: 1/1 from the token text"
    );
    assert!(
        token.card_types.core_types.contains(&CoreType::Artifact),
        "Gingerbrute is an artifact, got {:?}",
        token.card_types.core_types
    );
    assert!(
        token.card_types.core_types.contains(&CoreType::Creature),
        "Gingerbrute is a creature, got {:?}",
        token.card_types.core_types
    );
    assert!(
        token.card_types.subtypes.iter().any(|s| s == "Food"),
        "Gingerbrute is Food, got {:?}",
        token.card_types.subtypes
    );
    assert!(
        token.card_types.subtypes.iter().any(|s| s == "Golem"),
        "Gingerbrute is a Golem, got {:?}",
        token.card_types.subtypes
    );
    assert!(
        token.color.is_empty(),
        "Gingerbrute is colorless, got {:?}",
        token.color
    );
    assert!(token.has_keyword(&Keyword::Haste), "Gingerbrute has haste");
    assert!(
        token.token_image_ref.is_some(),
        "F1: the token must link its Gingerbrute catalog preset — without the \
         link the rider never materializes and the count below passes for the \
         wrong reason"
    );
    assert_eq!(
        token.base_abilities.len(),
        2,
        "exactly the unblockable rider and the Food sacrifice ability — \
         a third ability is the #9810 double-grant"
    );
    // F2: the token carries the Gingerbrute card's {1} mana cost (CR 111.11).
    // Reverting the install leaves the GameObject default and this fails.
    assert_eq!(
        token.mana_cost,
        ManaCost::generic(1),
        "CR 111.3: the Gingerbrute token carries its card's {{1}} cost"
    );
    // F2: the MV stays 1 after an explicit layer recomputation (CR 202.3) —
    // the install rides `base_mana_cost` through the layers reset.
    evaluate_layers(runner.state_mut());
    let token = runner
        .state()
        .objects
        .get(&token_id)
        .expect("the created token exists on the battlefield");
    assert_eq!(
        token.mana_cost,
        ManaCost::generic(1),
        "CR 202.3: the cost survives layer recomputation"
    );
    assert_eq!(
        token.base_mana_cost,
        ManaCost::generic(1),
        "CR 202.3: the layer base carries the cost"
    );
    assert_eq!(
        token.effective_mana_value(),
        1,
        "CR 202.3: the Gingerbrute token's mana value is 1"
    );
}

#[test]
fn ginger_skips_gingerbrute_when_you_are_not_monarch() {
    // Reach guard: the fixture line parses to a triggerable upkeep trigger,
    // so a missing token below can only mean the monarch condition declined.
    let parsed = parse_oracle_text(
        GINGER_UPKEEP,
        "Ginger, Queen of Sweets",
        &[],
        &["Creature".to_string()],
        &[],
    );
    let upkeep = parsed
        .triggers
        .iter()
        .find(|t| t.mode == TriggerMode::Phase && t.phase == Some(Phase::Upkeep))
        .expect("reach guard: upkeep trigger parses");
    assert!(
        upkeep.condition.is_some(),
        "reach guard: intervening-if parses"
    );

    let mut runner = build_runner(Some(P1));
    let before = runner.battlefield_count(P0);
    runner.advance_to_upkeep();
    runner.advance_until_stack_empty();
    assert_eq!(
        runner.battlefield_count(P0),
        before,
        "CR 603.4: no Gingerbrute when an opponent is the monarch"
    );
    assert!(
        runner.state().last_created_token_ids.is_empty(),
        "no token may be created when the intervening-if is false"
    );
}

#[test]
fn ginger_skips_gingerbrute_when_there_is_no_monarch() {
    // Reach guard: the fixture line parses to a triggerable upkeep trigger,
    // so a missing token below can only mean the monarch condition declined.
    let parsed = parse_oracle_text(
        GINGER_UPKEEP,
        "Ginger, Queen of Sweets",
        &[],
        &["Creature".to_string()],
        &[],
    );
    let upkeep = parsed
        .triggers
        .iter()
        .find(|t| t.mode == TriggerMode::Phase && t.phase == Some(Phase::Upkeep))
        .expect("reach guard: upkeep trigger parses");
    assert!(
        upkeep.condition.is_some(),
        "reach guard: intervening-if parses"
    );

    let mut runner = build_runner(None);
    let before = runner.battlefield_count(P0);
    runner.advance_to_upkeep();
    runner.advance_until_stack_empty();
    assert_eq!(
        runner.battlefield_count(P0),
        before,
        "CR 603.4: no Gingerbrute before anyone is the monarch"
    );
    assert!(
        runner.state().last_created_token_ids.is_empty(),
        "no token may be created when there is no monarch"
    );
}

/// SHAPE: the registry stays honest for unknown names — a bare descriptor
/// with no catalog entry must not resolve to a preset.
#[test]
fn unknown_named_token_stays_unimplemented() {
    // Paired positive reach-guard: the Ginger descriptor resolves through the
    // same pipeline in this test body, proving the instrument fires.
    let ginger = parse_oracle_text(
        GINGER_UPKEEP,
        "Ginger, Queen of Sweets",
        &[],
        &["Creature".to_string()],
        &[],
    );
    let ginger_upkeep = ginger
        .triggers
        .iter()
        .find(|t| t.mode == TriggerMode::Phase && t.phase == Some(Phase::Upkeep))
        .expect("reach guard: Ginger upkeep trigger parses");
    assert!(
        !matches!(
            *ginger_upkeep
                .execute
                .as_ref()
                .expect("trigger body exists")
                .effect,
            Effect::Unimplemented { .. }
        ),
        "reach guard: the registered Gingerbrute descriptor lowers to a typed effect"
    );

    // Nearest printed shape negated: Soul Tether's `Create a Heartwood
    // token. (...)` is the registered-control line this synthetic negates.
    // `Squibnocket` is used because the name is guaranteed absent from the
    // catalog (zero hits in known-tokens.toml) and no printed card exhibits
    // the unregistered-name shape (confirmed against the Step 0.5 class
    // query: no `squib` card), so honest red is the only correct outcome.
    let parsed = parse_oracle_text(
        "At the beginning of each upkeep, create a Squibnocket token.",
        "Squibnocket Maker",
        &[],
        &["Creature".to_string()],
        &[],
    );
    let upkeep = parsed
        .triggers
        .iter()
        .find(|t| t.mode == TriggerMode::Phase && t.phase == Some(Phase::Upkeep))
        .expect("reach guard: upkeep trigger parses");
    let execute = upkeep.execute.as_ref().expect("trigger body exists");
    assert!(
        matches!(*execute.effect, Effect::Unimplemented { .. }),
        "an unregistered bare-descriptor token must stay Unimplemented, not resolve a false-positive preset"
    );
}

/// COST-SHAPE (matrix #4): the rider is the catalog `{1}` unblockable
/// activation and the sac ability is the Food `{2}`+Tap+Sacrifice activation
/// (CR 111.3). Haste arrives via keywords, not abilities. Per the N3 branch
/// rule this asserts the activated-ability form because the gate probe prints
/// the Gingerbrute catalog extras as abilities (`abil=false`, `trig/stat =
/// true` vacuously) — had the rider lowered to a trigger or static, this row
/// would assert that kind and location instead.
#[test]
fn gingerbrute_abilities_have_rider_and_food_cost_shapes() {
    let mut runner = build_runner(Some(P0));
    runner.advance_to_upkeep();
    runner.advance_until_stack_empty();
    let token_id = *runner
        .state()
        .last_created_token_ids
        .first()
        .expect("reach guard: the upkeep trigger must create the token");
    let token = runner
        .state()
        .objects
        .get(&token_id)
        .expect("the created token exists on the battlefield");
    assert_eq!(
        token.base_abilities.len(),
        2,
        "reach guard: rider + Food sac, no #9810 double-grant"
    );
    assert!(
        token.has_keyword(&Keyword::Haste),
        "haste arrives via keywords, not counted in the 2 abilities"
    );
    let mut saw_rider = false;
    let mut saw_sac = false;
    for ability in token.base_abilities.iter() {
        assert_eq!(
            ability.kind,
            AbilityKind::Activated,
            "both Gingerbrute abilities are activations, got {:?}",
            ability.kind
        );
        if ability.cost
            == Some(AbilityCost::Mana {
                cost: ManaCost::Cost {
                    shards: vec![],
                    generic: 1,
                },
            })
        {
            saw_rider = true;
        } else if ability.cost
            == Some(AbilityCost::Composite {
                costs: vec![
                    AbilityCost::Mana {
                        cost: ManaCost::Cost {
                            shards: vec![],
                            generic: 2,
                        },
                    },
                    AbilityCost::Tap,
                    AbilityCost::Sacrifice(SacrificeCost::count(TargetFilter::SelfRef, 1)),
                ],
            })
        {
            saw_sac = true;
        } else {
            panic!(
                "unexpected Gingerbrute ability cost shape: {:?}",
                ability.cost
            );
        }
    }
    assert!(
        saw_rider,
        "the {{1}} unblockable rider activation is present"
    );
    assert!(
        saw_sac,
        "the {{2}}+Tap+Sacrifice Food activation is present"
    );
}

/// SHAPE: the full card parses with a typed upkeep-trigger body.
#[test]
fn ginger_upkeep_body_parses_to_a_token_effect_shape() {
    let parsed = parse_oracle_text(
        GINGER_ORACLE,
        "Ginger, Queen of Sweets",
        &[],
        &["Creature".to_string()],
        &[],
    );
    // Reach guards: the rest of the card parses, proving the input got past
    // upstream short-circuits before the body assertion below runs.
    assert_eq!(
        parsed.abilities.len(),
        1,
        "reach guard: the activated lifegain ability parses"
    );
    assert_eq!(
        parsed.triggers.len(),
        2,
        "reach guard: ETB + upkeep triggers parse"
    );
    let upkeep = parsed
        .triggers
        .iter()
        .find(|t| t.mode == TriggerMode::Phase && t.phase == Some(Phase::Upkeep))
        .expect("reach guard: upkeep trigger parses");
    assert!(
        upkeep.condition.is_some(),
        "reach guard: IsMonarch intervening-if parses"
    );
    let execute = upkeep.execute.as_ref().expect("trigger body exists");
    assert!(
        !matches!(*execute.effect, Effect::Unimplemented { .. }),
        "RED (#10983): the upkeep body must lower to a typed token effect, \
         not Effect::Unimplemented"
    );
}
