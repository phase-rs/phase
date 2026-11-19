//! Issue #2916: Torment of Hailfire — the repeat-X process must run before the
//! per-opponent torment; each iteration applies the scoped LoseLife effect.

use engine::game::ability_utils::build_resolved_from_def;
use engine::game::effects::resolve_ability_chain;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::parser::oracle::parse_oracle_text;
use engine::parser::oracle_effect::parse_effect_chain;
use engine::types::ability::{
    AbilityCost, AbilityDefinition, AbilityKind, Effect, PlayerFilter, RepeatForTiming,
    ResolvedAbility, UnlessPayModifier,
};
use engine::types::actions::{GameAction, UnlessCostBranch};
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const TORMENT_ORACLE: &str = "Repeat the following process X times. Each opponent loses 3 life unless that player sacrifices a nonland permanent of their choice or discards a card.";

#[test]
fn torment_repeat_x_applies_lose_life_each_iteration() {
    let mut def = parse_effect_chain(TORMENT_ORACLE, AbilityKind::Spell);
    assert!(
        def.repeat_for.is_some(),
        "Torment must carry repeat_for X, got {:?}",
        def.repeat_for
    );
    // Torment of Hailfire's ruling (2017-07-14): "Each repetition through this
    // process is done separately." The count is never the for-each instance
    // set, and the parser-to-resolver transfer keeps it so.
    assert_eq!(def.repeat_timing, RepeatForTiming::Sequential);
    assert_eq!(
        build_resolved_from_def(&def, ObjectId(900), P0).repeat_timing,
        RepeatForTiming::Sequential
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

/// Torment of Hailfire's ruling (2017-07-14): "Each repetition through this
/// process is done separately." Cast through the production pipeline, the card
/// P1 chose to discard on the first repetition is in the graveyard before the
/// second repetition's prompt.
#[test]
fn torment_cast_discards_each_round_before_the_next_prompt() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let torment = scenario
        .add_spell_to_hand_from_oracle(P0, "Torment of Hailfire", false, TORMENT_ORACLE)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::X, ManaCostShard::Black, ManaCostShard::Black],
            generic: 0,
        })
        .id();
    scenario.add_creature(P1, "Grizzly Bears", 2, 2);
    let card = scenario.add_card_to_hand(P1, "Filler Card");
    scenario.with_mana_pool(
        P0,
        (0..4)
            .map(|_| ManaUnit::new(ManaType::Black, ObjectId(0), false, vec![]))
            .collect(),
    );
    let mut runner = scenario.build();

    let outcome = runner.cast(torment).x(2).resolve();
    drop(outcome);

    let index = match &runner.state().waiting_for {
        WaitingFor::UnlessPaymentChooseCost { player, costs, .. } => {
            assert_eq!(*player, P1, "round 1 asks P1");
            costs
                .iter()
                .position(|cost| matches!(cost, AbilityCost::Discard { .. }))
                .expect("the discard branch is offered")
        }
        other => panic!("expected P1's round-1 prompt, got {other:?}"),
    };
    runner
        .act(GameAction::ChooseUnlessCostBranch {
            choice: UnlessCostBranch::Pay { index },
        })
        .expect("P1 chooses to discard");
    runner
        .act(GameAction::SelectCards { cards: vec![card] })
        .expect("P1 discards the card");

    assert!(
        matches!(
            &runner.state().waiting_for,
            WaitingFor::UnlessPayment { player, .. } | WaitingFor::UnlessPaymentChooseCost { player, .. }
                if *player == P1
        ),
        "reach guard: round 2's prompt is open, got {:?}",
        runner.state().waiting_for
    );
    assert_eq!(
        runner.state().objects[&card].zone,
        Zone::Graveyard,
        "round 1's discard happened before round 2's prompt"
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

const VIPER_ORACLE: &str = "As an additional cost to cast this spell, you may sacrifice any number of nonland permanents. This spell costs {1} less to cast for each permanent sacrificed this way.\nWhenever this creature enters or attacks, put a blight counter on it. Then for each blight counter on it, each opponent loses 4 life unless that player sacrifices a nonland permanent of their choice or discards a card.";

/// The node of a parsed ability chain that carries Viper's counted repeat.
fn counted_definition(root: &AbilityDefinition) -> &AbilityDefinition {
    let mut node = Some(root);
    while let Some(def) = node {
        if def.repeat_for.is_some() {
            return def;
        }
        node = def.sub_ability.as_deref();
    }
    panic!("no node of Viper's chain carries repeat_for: {root:#?}");
}

fn counted_resolved(root: &ResolvedAbility) -> &ResolvedAbility {
    let mut node = Some(root);
    while let Some(ability) = node {
        if ability.repeat_for.is_some() {
            return ability;
        }
        node = ability.sub_ability.as_deref();
    }
    panic!("no node of Viper's resolved chain carries repeat_for");
}

/// CR 608.2c + CR 101.4: Rottenmouth Viper's "Then for each blight counter on
/// it, each opponent loses 4 life unless …" is a for-each instance set, and the
/// def-to-resolved transfer carries that stamp together with its count. The
/// resolved ability's equality distinguishes it from the same ability stamped
/// `Sequential`.
#[test]
fn viper_for_each_stamp_transfers_into_the_resolved_ability() {
    let parsed = parse_oracle_text(
        VIPER_ORACLE,
        "Rottenmouth Viper",
        &[],
        &["Creature".to_string()],
        &["Elemental".to_string(), "Snake".to_string()],
    );
    assert_eq!(parsed.triggers.len(), 1, "{:?}", parsed.triggers);
    let execute = parsed.triggers[0].execute.as_deref().expect("execute");

    let counted = counted_definition(execute);
    // Reach guard: the node is the scoped unless-loss, repeated per blight counter.
    assert!(matches!(*counted.effect, Effect::LoseLife { .. }));
    assert_eq!(counted.player_scope, Some(PlayerFilter::Opponent));
    assert_eq!(
        counted.repeat_timing,
        RepeatForTiming::SimultaneousInstances
    );

    let resolved = build_resolved_from_def(execute, ObjectId(900), P0);
    let counted_ability = counted_resolved(&resolved);
    assert!(counted_ability.repeat_for.is_some());
    assert_eq!(
        counted_ability.repeat_timing,
        RepeatForTiming::SimultaneousInstances
    );

    // PartialEq conjunct: identical in every field but the timing, not equal.
    let mut same = counted_ability.clone();
    assert_eq!(&same, counted_ability);
    same.repeat_timing = RepeatForTiming::Sequential;
    assert_ne!(&same, counted_ability);
}
