//! Regression: `ExploreAll`'s sub-ability must resolve exactly once.
//!
//! The bug: `explore::resolve_single_explorer` is the authority for an
//! `ExploreAll`'s sub-ability chain (it carries the printed tail onto the
//! terminal explorer and synthesizes the per-explorer `TrackedSet`
//! continuation). The generic chain walker in `resolve_chain_body` ALSO
//! processed `ExploreAll.sub_ability`, so on a paused explore (the nonland
//! `DigChoice`) the sub was stashed onto `pending_continuation` a SECOND time.
//!
//! When the sub is a benign tail (gain life) it double-executes. When the sub
//! is the synthesized `ExploreAll { TrackedSet }` continuation, the second
//! prepend chains it to itself, producing a self-renewing loop that re-explores
//! the same permanent forever (Hakbal of the Surging Soul: unbounded +1/+1
//! counters — the reported Discord bug).
use engine::game::scenario::GameScenario;
use engine::types::ability::{
    AbilityDefinition, AbilityKind, ControllerRef, Effect, QuantityExpr, TargetFilter,
    TriggerConstraint, TriggerDefinition, TypedFilter,
};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::phase::Phase;
use engine::types::triggers::TriggerMode;
use engine::types::zones::Zone;
use engine::types::PlayerId;

const P0: PlayerId = PlayerId(0);

#[test]
fn explore_all_tail_effect_runs_exactly_once() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_life(P0, 20);

    // A Merfolk with a begin-combat trigger: "each Merfolk you control
    // explores, then you gain 3 life." The gain-life tail is the observable
    // proxy for "the ExploreAll sub-ability resolved".
    let trigger = TriggerDefinition::new(TriggerMode::Phase)
        .phase(Phase::BeginCombat)
        .trigger_zones(vec![Zone::Battlefield])
        .constraint(TriggerConstraint::OnlyDuringYourTurn)
        .execute(
            AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::ExploreAll {
                    filter: TargetFilter::Typed(
                        TypedFilter::creature()
                            .subtype("Merfolk".to_string())
                            .controller(ControllerRef::You),
                    ),
                },
            )
            .sub_ability(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::GainLife {
                    amount: QuantityExpr::Fixed { value: 3 },
                    player: TargetFilter::Controller,
                },
            )),
        );

    let explorer = {
        let mut b = scenario.add_creature(P0, "Test Merfolk", 2, 2);
        b.with_subtypes(vec!["Merfolk"]);
        b.with_trigger_definition(trigger);
        b.id()
    };
    let _ = explorer;

    // Nonland on top so the explore takes the +1/+1 / DigChoice branch (the
    // pausing path that triggered the double-stash).
    scenario.with_library_top(P0, &["Lightning Bolt", "Lightning Bolt"]);

    let mut runner = scenario.build();
    assert_eq!(runner.state().players[0].life, 20);

    runner.advance_to_combat();

    // Resolve any explore prompts; bounded so an infinite loop is a test failure.
    for step in 0..40 {
        let waiting = runner.state().waiting_for.clone();
        let action = match waiting {
            WaitingFor::ExploreChoice { choosable, .. } => GameAction::ChooseTarget {
                target: Some(engine::types::ability::TargetRef::Object(choosable[0])),
            },
            WaitingFor::DigChoice { cards, .. } => GameAction::SelectCards {
                cards: vec![cards[0]],
            },
            WaitingFor::Priority { .. } | WaitingFor::DeclareAttackers { .. } => break,
            other => panic!("unexpected prompt at step {step}: {other:?}"),
        };
        if runner.act(action).is_err() {
            break;
        }
        assert!(step < 39, "explore did not terminate — infinite loop");
    }

    // CR 119.3: the gain-life tail must fire exactly once → 20 + 3 = 23.
    // The double-stash made it fire twice (26) or, for a self-referencing
    // explore continuation, loop forever.
    assert_eq!(
        runner.state().players[0].life,
        23,
        "ExploreAll sub-ability (gain 3 life) must resolve exactly once"
    );
}
