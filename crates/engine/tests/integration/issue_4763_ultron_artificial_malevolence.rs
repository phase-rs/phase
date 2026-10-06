//! Issue #4763: Ultron, Artificial Malevolence — the "if the token isn't a
//! creature" gate on the 2/2 Robot Villain override.
//!
//! https://github.com/phase-rs/phase/issues/4763
//!
//! Oracle: "Whenever another nontoken artifact you control enters, you may pay
//! {2}. If you do, create a token that's a copy of it. If the token isn't a
//! creature, it becomes a 2/2 Robot Villain creature in addition to its other
//! types."
//!
//! Two things must hold, and the reported defect broke the first:
//!
//! 1. CR 608.2c — the "If the token isn't a creature" gate is a game-state
//!    predicate evaluated as the ability resolves. A copy of an artifact that is
//!    ALREADY a creature must keep its copied power/toughness (CR 707.2), not be
//!    overwritten to 2/2 by a gate the parser dropped.
//! 2. CR 111.1 + CR 608.2c — the anaphor is "the token", so both the gate and the
//!    type/P-T override bind to the just-created copy, never to the nontoken
//!    artifact that triggered the ability.

use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::types::card_type::CoreType;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const ULTRON_ORACLE: &str = "Whenever another nontoken artifact you control enters, you may pay \
{2}. If you do, create a token that's a copy of it. If the token isn't a creature, it becomes a \
2/2 Robot Villain creature in addition to its other types.";

fn pool(units: usize) -> Vec<ManaUnit> {
    (0..units)
        .map(|_| ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]))
        .collect()
}

/// The copy token Ultron minted — the only battlefield token, since the scenario
/// creates no others.
fn copy_token(runner: &GameRunner) -> ObjectId {
    let tokens: Vec<ObjectId> = runner
        .state()
        .objects
        .iter()
        .filter(|(_, obj)| obj.is_token && obj.zone == Zone::Battlefield)
        .map(|(id, _)| *id)
        .collect();
    assert_eq!(
        tokens.len(),
        1,
        "Ultron must mint exactly one copy token, got {tokens:?}"
    );
    tokens[0]
}

/// CR 707.2: the token copies the artifact creature's copiable characteristics,
/// and the "isn't a creature" gate is FALSE — so the 2/2 Robot Villain override
/// must not run. The reported defect dropped the gate and stamped 2/2 onto a
/// copy that was already a 4/4.
#[test]
fn ultron_copy_of_an_artifact_creature_keeps_its_printed_power_toughness() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, pool(8));

    scenario
        .add_artifact_from_oracle(P0, "Ultron, Artificial Malevolence", ULTRON_ORACLE)
        .as_creature();

    let golem = scenario.add_creature_to_hand(P0, "Steel Golem", 4, 4).id();

    let mut runner = scenario.build();
    // CR 301.1 + CR 302.1: make the entering permanent an ARTIFACT creature.
    // `CardBuilder::as_artifact` strips the Creature type, so the artifact type
    // is added through the documented `state_mut` escape hatch instead.
    {
        let obj = runner.state_mut().objects.get_mut(&golem).unwrap();
        obj.card_types.core_types.push(CoreType::Artifact);
        obj.base_card_types = obj.card_types.clone();
    }

    runner.cast(golem).accept_optional().resolve();

    let token = copy_token(&runner);
    let token_obj = runner.state().objects.get(&token).unwrap();
    assert_eq!(
        token_obj.name, "Steel Golem",
        "the token copies the artifact"
    );
    assert_eq!(
        (token_obj.power, token_obj.toughness),
        (Some(4), Some(4)),
        "CR 707.2: a copy that is already a creature keeps its copied P/T; the \
         2/2 Robot Villain override is gated on the token NOT being a creature"
    );
    assert!(
        !token_obj
            .card_types
            .subtypes
            .iter()
            .any(|s| s == "Robot" || s == "Villain"),
        "the gated type override must not run for a creature copy, got {:?}",
        token_obj.card_types.subtypes
    );

    // CR 111.1: the original nontoken artifact is untouched — the anaphor is
    // "the token", not the triggering permanent.
    let original = runner.state().objects.get(&golem).unwrap();
    assert_eq!((original.power, original.toughness), (Some(4), Some(4)));
    assert!(!original.is_token);
}

/// The affirmative half of the same gate: a copy of a NONCREATURE artifact is
/// not a creature, so the override runs — on the token, and only on the token.
#[test]
fn ultron_copy_of_a_noncreature_artifact_becomes_a_two_two_robot_villain() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, pool(8));

    scenario
        .add_artifact_from_oracle(P0, "Ultron, Artificial Malevolence", ULTRON_ORACLE)
        .as_creature();

    let relic = scenario
        .add_artifact_to_hand_from_oracle(P0, "Inert Relic", "")
        .id();

    let mut runner = scenario.build();
    runner.cast(relic).accept_optional().resolve();

    let token = copy_token(&runner);
    let token_obj = runner.state().objects.get(&token).unwrap();
    assert_eq!(token_obj.name, "Inert Relic");
    assert!(
        token_obj
            .card_types
            .core_types
            .contains(&CoreType::Creature)
            && token_obj
                .card_types
                .core_types
                .contains(&CoreType::Artifact),
        "CR 205.1b: the token becomes a creature IN ADDITION to its other types, got {:?}",
        token_obj.card_types.core_types
    );
    assert_eq!((token_obj.power, token_obj.toughness), (Some(2), Some(2)));
    for subtype in ["Robot", "Villain"] {
        assert!(
            token_obj.card_types.subtypes.iter().any(|s| s == subtype),
            "expected {subtype} subtype, got {:?}",
            token_obj.card_types.subtypes
        );
    }

    // The reported symptom: the override landing on the entering artifact rather
    // than on its copy. The original must stay a noncreature artifact.
    let original = runner.state().objects.get(&relic).unwrap();
    assert!(
        !original.card_types.core_types.contains(&CoreType::Creature),
        "the triggering nontoken artifact must NOT become a creature, got {:?}",
        original.card_types.core_types
    );
    assert_eq!((original.power, original.toughness), (None, None));
}
