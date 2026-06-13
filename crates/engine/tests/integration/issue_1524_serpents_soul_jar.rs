//! Issue #1524 — Serpent's Soul-Jar must exile Elves that die and allow
//! casting creature spells from among cards exiled with it.

use engine::ai_support::legal_actions;
use engine::game::scenario::{GameScenario, P0};
use engine::types::ability::{ActivationRestriction, Effect};
use engine::types::actions::{DebugAction, GameAction};
use engine::types::game_state::{ExileLinkKind, WaitingFor};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const SOUL_JAR_ORACLE: &str = "\
Whenever an Elf you control dies, exile it.\n\
{T}, Pay 2 life: Once each turn, you may cast a creature spell from among cards exiled with this artifact.";

#[test]
fn issue_1524_soul_jar_exiles_elf_and_offers_cast_from_exile() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let jar = scenario
        .add_creature(P0, "Serpent's Soul-Jar", 0, 0)
        .as_artifact()
        .from_oracle_text(SOUL_JAR_ORACLE)
        .id();

    let elf = scenario
        .add_creature(P0, "Llanowar Elves", 1, 1)
        .with_subtypes(vec!["Elf"])
        .id();

    let mut runner = scenario.build();
    runner.state_mut().debug_mode = true;

    let jar_ability = &runner.state().objects[&jar].abilities[0];
    assert!(
        matches!(jar_ability.effect.as_ref(), Effect::CastFromZone { .. }),
        "activated ability must parse as CastFromZone, got {:?}",
        jar_ability.effect
    );
    assert!(
        jar_ability
            .activation_restrictions
            .contains(&ActivationRestriction::OnlyOnceEachTurn),
        "Once each turn must be an activation restriction"
    );
    assert_eq!(
        jar_ability
            .activation_restrictions
            .iter()
            .filter(|restriction| matches!(restriction, ActivationRestriction::OnlyOnceEachTurn))
            .count(),
        1,
        "Once each turn must be represented exactly once"
    );
    assert!(
        runner.state().objects[&jar]
            .base_static_definitions
            .is_empty()
            && runner.state().objects[&jar].static_definitions.is_empty(),
        "Soul-Jar's activated permission must not also parse as a static permission"
    );

    runner
        .act(GameAction::Debug(DebugAction::Sacrifice { object_id: elf }))
        .expect("sacrificing the Elf should succeed");

    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().objects[&elf].zone,
        Zone::Exile,
        "the dying Elf must be exiled, not left in the graveyard"
    );

    assert!(
        runner.state().exile_links.iter().any(|link| {
            link.exiled_id == elf
                && link.source_id == jar
                && matches!(link.kind, ExileLinkKind::TrackedBySource)
        }),
        "exile link must connect the Elf to the Soul-Jar"
    );

    runner
        .act(GameAction::ActivateAbility {
            source_id: jar,
            ability_index: 0,
        })
        .expect("Soul-Jar activated ability should succeed");

    runner.advance_until_stack_empty();

    if matches!(
        runner.state().waiting_for,
        WaitingFor::OptionalEffectChoice { .. }
    ) {
        runner
            .act(GameAction::DecideOptionalEffect { accept: true })
            .expect("accepting optional cast from exile must succeed");
        runner.advance_until_stack_empty();
    }

    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "activation should finish at priority, got {:?}",
        runner.state().waiting_for
    );

    let legal = legal_actions(runner.state());
    assert!(
        legal.iter().any(|a| matches!(
            a,
            GameAction::CastSpell { object_id, .. } if *object_id == elf
        )),
        "the exiled Elf creature card must be castable from exile; legal_actions={legal:?}"
    );
}
