//! Issue #5338 — Synthesized ninjutsu marker abilities on the battlefield must
//! not route through `GameAction::ActivateAbility` (CR 702.49a: ninjutsu functions
//! only from hand). The marker's `NinjutsuFamily` cost is a no-op in
//! `pay_ability_cost`, so the generic activation path would stack without paying mana.

use std::sync::Arc;

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::ability::{
    AbilityCost, AbilityDefinition, AbilityKind, Effect, NinjutsuVariant, RuntimeHandler,
};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaCost, ManaCostShard};
use engine::types::phase::Phase;

use super::rules::AttackTarget;

fn moon_circuit_ninjutsu_marker_ability() -> AbilityDefinition {
    AbilityDefinition::new(
        AbilityKind::Activated,
        Effect::RuntimeHandled {
            handler: RuntimeHandler::NinjutsuFamily,
        },
    )
    .cost(AbilityCost::NinjutsuFamily {
        variant: NinjutsuVariant::Ninjutsu,
        mana_cost: ManaCost::Cost {
            shards: vec![ManaCostShard::Blue],
            generic: 0,
        },
    })
}

fn wire_moon_circuit_hacker_battlefield_marker(obj: &mut engine::game::game_object::GameObject) {
    let cost = ManaCost::Cost {
        shards: vec![ManaCostShard::Blue],
        generic: 0,
    };
    obj.keywords.push(Keyword::Ninjutsu(cost));
    obj.base_keywords = obj.keywords.clone();
    let marker = moon_circuit_ninjutsu_marker_ability();
    obj.abilities = Arc::new(vec![marker.clone()]);
    obj.base_abilities = Arc::new(vec![marker]);
}

fn advance_to_declare_blockers_priority(
    runner: &mut engine::game::scenario::GameRunner,
    attacker: engine::types::identifiers::ObjectId,
) {
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(attacker, AttackTarget::Player(P1))])
        .expect("declare attackers");
    if matches!(runner.state().waiting_for, WaitingFor::Priority { .. }) {
        runner.pass_both_players();
    }
    if matches!(
        runner.state().waiting_for,
        WaitingFor::DeclareBlockers { .. }
    ) {
        runner
            .act(GameAction::DeclareBlockers {
                assignments: vec![],
            })
            .expect("declare no blockers");
    }
    assert_eq!(runner.state().phase, Phase::DeclareBlockers);
}

#[test]
fn battlefield_ninjutsu_marker_not_offered_as_activate_ability_without_mana() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let attacker = scenario.add_creature(P0, "Attacker", 1, 1).id();
    let battlefield_hacker = scenario.add_creature(P0, "Moon-Circuit Hacker", 1, 3).id();

    let mut runner = scenario.build();
    wire_moon_circuit_hacker_battlefield_marker(
        runner
            .state_mut()
            .objects
            .get_mut(&battlefield_hacker)
            .unwrap(),
    );
    advance_to_declare_blockers_priority(&mut runner, attacker);

    runner.state_mut().players[P0.0 as usize].mana_pool.clear();

    let actions = engine::ai_support::legal_actions(runner.state());
    assert!(
        !actions.iter().any(|a| matches!(
            a,
            GameAction::ActivateAbility {
                source_id,
                ability_index: 0,
            } if *source_id == battlefield_hacker
        )),
        "synthesized NinjutsuFamily marker must not be offered via ActivateAbility"
    );
    assert!(
        !engine::game::casting::can_activate_ability_now(runner.state(), P0, battlefield_hacker, 0),
        "can_activate_ability_now must reject the ninjutsu marker ability"
    );

    let stack_before = runner.state().stack.len();
    let result = runner.act(GameAction::ActivateAbility {
        source_id: battlefield_hacker,
        ability_index: 0,
    });
    assert!(
        result.is_err(),
        "ActivateAbility on ninjutsu marker must be rejected at runtime"
    );
    assert_eq!(
        runner.state().stack.len(),
        stack_before,
        "ninjutsu marker must not be put on the stack without paying mana"
    );
}
