//! Issue #2916: Torment of Hailfire — the repeat-X process must run before the
//! per-opponent torment; each iteration applies the scoped LoseLife effect.

use engine::game::ability_utils::build_resolved_from_def;
use engine::game::effects::resolve_ability_chain;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::parser::oracle_effect::parse_effect_chain;
use engine::types::ability::{AbilityKind, Effect, UnlessPayModifier};
use engine::types::actions::{GameAction, UnlessCostBranch};
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;

const TORMENT_ORACLE: &str = "Repeat the following process X times. Each opponent loses 3 life unless that player sacrifices a nonland permanent of their choice or discards a card.";

#[test]
fn torment_repeat_x_applies_lose_life_each_iteration() {
    let mut def = parse_effect_chain(TORMENT_ORACLE, AbilityKind::Spell);
    assert!(
        def.repeat_for.is_some(),
        "Torment must carry repeat_for X, got {:?}",
        def.repeat_for
    );
    assert!(
        def.player_scope.is_some(),
        "Torment must carry player_scope, got {:?}",
        def.player_scope
    );
    assert!(matches!(*def.effect, Effect::LoseLife { .. }));
    // Exercise the repeat × player_scope ordering without interactive unless-pay
    // prompts; the unless modifier is validated at parse time above.
    def.unless_pay = None;

    let scenario = GameScenario::new();
    let mut runner = scenario.build();

    let mut ability = build_resolved_from_def(&def, ObjectId(900), P0);
    ability.chosen_x = Some(2);
    ability.unless_pay = None;

    let life_before = runner.state().players[P1.0 as usize].life;
    let mut events = Vec::new();
    resolve_ability_chain(runner.state_mut(), &ability, &mut events, 0).unwrap();

    assert_eq!(
        runner.state().players[P1.0 as usize].life,
        life_before - 6,
        "X=2 repeat iterations must each apply 3 life loss to the opponent"
    );
}

#[test]
fn torment_repeat_x_declined_unless_choices_resume_each_iteration() {
    let def = parse_effect_chain(TORMENT_ORACLE, AbilityKind::Spell);
    let scenario = GameScenario::new();
    let mut runner = scenario.build();

    let mut ability = build_resolved_from_def(&def, ObjectId(900), P0);
    ability.chosen_x = Some(2);

    let life_before = runner.state().players[P1.0 as usize].life;
    let mut events = Vec::new();
    resolve_ability_chain(runner.state_mut(), &ability, &mut events, 0).unwrap();

    for prompt_number in 1..=2 {
        assert!(
            matches!(
                runner.state().waiting_for,
                WaitingFor::UnlessPaymentChooseCost { player: P1, .. }
            ),
            "Torment iteration {prompt_number} must ask P1 to pay or decline its disjunctive unless cost, got {:?}",
            runner.state().waiting_for
        );
        runner
            .act(GameAction::ChooseUnlessCostBranch {
                choice: UnlessCostBranch::Decline,
            })
            .expect("decline Torment unless cost");
    }

    assert_eq!(
        runner.state().players[P1.0 as usize].life,
        life_before - 6,
        "declining both X=2 unless prompts must apply 3 life loss per iteration"
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "Torment should return to priority after both repeat iterations, got {:?}",
        runner.state().waiting_for
    );
}

#[test]
fn torment_parses_repeat_player_scope_and_unless_pay() {
    let def = parse_effect_chain(TORMENT_ORACLE, AbilityKind::Spell);
    assert!(def.unless_pay.is_some());
    assert!(matches!(
        def.unless_pay.as_ref().map(|u| &u.cost),
        Some(engine::types::ability::AbilityCost::OneOf { .. })
    ));
    let _ = UnlessPayModifier::clone(def.unless_pay.as_ref().unwrap());
}
