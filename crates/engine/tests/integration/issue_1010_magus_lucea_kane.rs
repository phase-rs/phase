//! Issue #1010 — Magus Lucea Kane Psychic Stimulus copies the next {X} spell or ability.

use engine::game::scenario::{GameScenario, P0};
use engine::types::ability::{
    AbilityCost, AbilityDefinition, AbilityKind, Effect, QuantityExpr, QuantityRef, TargetFilter,
};
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;

const MAGUS_ORACLE: &str = "Psychic Stimulus — {T}: Add {C}{C}. When you next cast a spell with {X} in its mana cost or activate an ability with {X} in its activation cost this turn, copy that spell or ability. You may choose new targets for the copy.";

fn add_colorless(runner: &mut engine::game::scenario::GameRunner, count: usize) {
    for _ in 0..count {
        runner.state_mut().players[0].mana_pool.add(ManaUnit::new(
            ManaType::Colorless,
            engine::types::identifiers::ObjectId(0),
            false,
            vec![],
        ));
    }
}

#[test]
fn magus_lucea_kane_tap_ability_parses_delayed_copy_not_unimplemented() {
    let parsed = engine::parser::parse_oracle_text(
        MAGUS_ORACLE,
        "Magus Lucea Kane",
        &[],
        &["Creature".to_string()],
        &[],
    );
    let ability = parsed.abilities.last().expect("tap ability");
    let sub = ability
        .sub_ability
        .as_ref()
        .expect("delayed copy sub_ability");
    assert!(
        matches!(sub.effect.as_ref(), Effect::CreateDelayedTrigger { .. }),
        "expected CreateDelayedTrigger, got {:?}",
        sub.effect
    );
}

#[test]
fn magus_lucea_kane_tap_registers_delayed_copy_for_next_x_spell() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(
        P0,
        &[
            "Card One",
            "Card Two",
            "Card Three",
            "Card Four",
            "Card Five",
            "Card Six",
        ],
    );

    let magus = scenario
        .add_creature_from_oracle(P0, "Magus Lucea Kane", 2, 4, MAGUS_ORACLE)
        .id();

    let spell = scenario
        .add_spell_to_hand(P0, "Draw X", false)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::X],
            generic: 0,
        })
        .with_ability(Effect::Draw {
            count: QuantityExpr::Ref {
                qty: QuantityRef::CostXPaid,
            },
            target: TargetFilter::Controller,
        })
        .id();

    let mut runner = scenario.build();
    let library_before = runner.state().players[0].library.len();

    runner.activate(magus, 0).resolve();
    assert_eq!(
        runner.state().delayed_triggers.len(),
        1,
        "Psychic Stimulus must register a one-shot delayed trigger"
    );

    add_colorless(&mut runner, 3);
    runner.cast(spell).x(2).resolve();

    assert_eq!(
        library_before - runner.state().players[0].library.len(),
        4,
        "Draw X=2 plus its copy should draw four cards total"
    );
    assert!(
        runner.state().delayed_triggers.is_empty(),
        "one-shot delayed trigger must be consumed after the X spell is cast"
    );
}

#[test]
fn magus_delayed_copy_trigger_sits_above_activated_ability_on_stack() {
    use engine::types::actions::GameAction;

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(
        P0,
        &[
            "Card One",
            "Card Two",
            "Card Three",
            "Card Four",
            "Card Five",
            "Card Six",
        ],
    );

    let magus = scenario
        .add_creature_from_oracle(P0, "Magus Lucea Kane", 2, 4, MAGUS_ORACLE)
        .id();

    let draw_x_ability = AbilityDefinition::new(
        AbilityKind::Activated,
        Effect::Draw {
            count: QuantityExpr::Ref {
                qty: QuantityRef::CostXPaid,
            },
            target: TargetFilter::Controller,
        },
    )
    .cost(AbilityCost::Mana {
        cost: ManaCost::Cost {
            shards: vec![ManaCostShard::X],
            generic: 0,
        },
    });

    let source = scenario
        .add_creature(P0, "Draw X Source", 1, 1)
        .with_ability_definition(draw_x_ability)
        .id();

    let mut runner = scenario.build();
    runner.activate(magus, 0).resolve();
    add_colorless(&mut runner, 3);

    runner
        .act(GameAction::ActivateAbility {
            source_id: source,
            ability_index: 0,
        })
        .expect("announce activation");
    runner
        .act(GameAction::ChooseX { value: 2 })
        .expect("announce X");
    runner
        .act(GameAction::PassPriority)
        .expect("pay activation cost");

    assert_eq!(
        runner.state().stack.len(),
        2,
        "activated ability plus Magus delayed copy trigger must both be on stack"
    );
    assert!(
        matches!(
            runner.state().stack.back().map(|e| &e.kind),
            Some(engine::types::game_state::StackEntryKind::TriggeredAbility { .. })
        ),
        "Magus delayed copy must be on top of the activating ability"
    );

    let library_before = runner.state().players[0].library.len();
    for _ in 0..16 {
        if runner.state().stack.is_empty() {
            break;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("priority pass P0");
        if runner.state().stack.is_empty() {
            break;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("priority pass P1");
    }
    assert!(
        runner.state().stack.is_empty(),
        "both stack objects must resolve, stack = {:?}",
        runner.state().stack.len()
    );
    assert_eq!(
        library_before - runner.state().players[0].library.len(),
        4,
        "copy trigger plus original and copy of Draw X=2 should draw four cards"
    );
}

#[test]
fn magus_lucea_kane_tap_registers_delayed_copy_for_next_x_activated_ability() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(
        P0,
        &[
            "Card One",
            "Card Two",
            "Card Three",
            "Card Four",
            "Card Five",
            "Card Six",
        ],
    );

    let magus = scenario
        .add_creature_from_oracle(P0, "Magus Lucea Kane", 2, 4, MAGUS_ORACLE)
        .id();

    let draw_x_ability = AbilityDefinition::new(
        AbilityKind::Activated,
        Effect::Draw {
            count: QuantityExpr::Ref {
                qty: QuantityRef::CostXPaid,
            },
            target: TargetFilter::Controller,
        },
    )
    .cost(AbilityCost::Mana {
        cost: ManaCost::Cost {
            shards: vec![ManaCostShard::X],
            generic: 0,
        },
    });

    let source = scenario
        .add_creature(P0, "Draw X Source", 1, 1)
        .with_ability_definition(draw_x_ability)
        .id();

    let mut runner = scenario.build();
    let library_before = runner.state().players[0].library.len();

    runner.activate(magus, 0).resolve();
    assert_eq!(runner.state().delayed_triggers.len(), 1);
    match &runner.state().delayed_triggers[0].condition {
        engine::types::ability::DelayedTriggerCondition::WhenNextEvent { or_trigger, .. } => {
            assert!(
                or_trigger.is_some(),
                "Magus delayed trigger must carry or_trigger for activated abilities"
            )
        }
        other => panic!("expected WhenNextEvent delayed trigger, got {other:?}"),
    }

    add_colorless(&mut runner, 3);
    runner.activate(source, 0).x(2).resolve();

    assert!(
        runner.state().delayed_triggers.is_empty(),
        "one-shot delayed trigger must be consumed after the X activated ability"
    );
    assert_eq!(
        library_before - runner.state().players[0].library.len(),
        4,
        "Draw X=2 activated ability plus its copy should draw four cards total"
    );
}
