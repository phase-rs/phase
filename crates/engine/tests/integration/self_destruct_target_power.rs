//! Self-Destruct's two targets are chosen during casting, but both damage
//! amounts come from the first target creature's current power.

use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{
    ContinuousModification, DamageSource, Effect, StaticDefinition, TargetFilter, TargetRef,
    TypeFilter, TypedFilter,
};
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, StackEntryKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::statics::StaticMode;
use engine::types::zones::Zone;

const SELF_DESTRUCT_ORACLE: &str =
    "Target creature you control deals X damage to any other target and X damage to itself, where X is its power.";

fn add_red_mana(runner: &mut GameRunner) {
    runner
        .state_mut()
        .players
        .iter_mut()
        .find(|player| player.id == P0)
        .expect("P0 exists")
        .mana_pool
        .add(ManaUnit::new(ManaType::Red, ObjectId(0), false, vec![]));
}

fn setup(
    source_pt: (i32, i32),
    recipient_pt: (i32, i32),
    anthem: bool,
) -> (GameRunner, ObjectId, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario.add_vanilla(P0, source_pt.0, source_pt.1);
    let recipient = scenario.add_vanilla(P1, recipient_pt.0, recipient_pt.1);
    if anthem {
        let static_def = StaticDefinition::new(StaticMode::Continuous)
            .affected(TargetFilter::Typed(TypedFilter::new(TypeFilter::Creature)))
            .modifications(vec![ContinuousModification::AddPower { value: 1 }]);
        scenario
            .add_creature(P0, "Self-Destruct Test Anthem", 0, 5)
            .with_static_definition(static_def);
    }
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Self-Destruct", false, SELF_DESTRUCT_ORACLE)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Red],
            generic: 0,
        })
        .id();
    let mut runner = scenario.build();
    runner.state_mut().layers_dirty.mark_full();
    evaluate_layers(runner.state_mut());
    add_red_mana(&mut runner);
    (runner, spell, source, recipient)
}

/// Drives CR 601.2 target declaration, normal mana payment, and stack resolution.
fn cast_self_destruct(
    runner: &mut GameRunner,
    spell: ObjectId,
    source: ObjectId,
    recipient: ObjectId,
) {
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("Self-Destruct cast starts");

    let WaitingFor::TargetSelection { selection, .. } = &runner.state().waiting_for else {
        panic!("first Self-Destruct choice must target the controlled source");
    };
    assert!(selection
        .current_legal_targets
        .contains(&TargetRef::Object(source)));
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(source)),
        })
        .expect("choose the damage source creature");

    let WaitingFor::TargetSelection { selection, .. } = &runner.state().waiting_for else {
        panic!("second Self-Destruct choice must select the other recipient");
    };
    assert!(
        !selection
            .current_legal_targets
            .contains(&TargetRef::Object(source)),
        "CR 115.4: 'another target' must reject the chosen source creature"
    );
    assert!(selection
        .current_legal_targets
        .contains(&TargetRef::Object(recipient)));
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(recipient)),
        })
        .expect("choose the other damage recipient");

    assert!(
        runner.state().pending_cast.is_none(),
        "Auto payment finalizes the normal-cost cast after the final target"
    );
    let entry = runner
        .state()
        .stack
        .last()
        .expect("final target selection must leave the announced spell on the stack");
    assert_eq!(entry.id, spell);
    let StackEntryKind::Spell {
        ability: Some(ability),
        ..
    } = &entry.kind
    else {
        panic!("the finalized Self-Destruct stack entry must carry its bound ability");
    };
    let Effect::TargetOnly { .. } = &ability.effect else {
        panic!("the target-subject declaration must remain the stack root");
    };
    let first_damage = ability
        .sub_ability
        .as_deref()
        .expect("the stack root must retain the first bound damage leg");
    assert!(matches!(
        (&first_damage.effect, first_damage.sub_ability.as_deref()),
        (
            Effect::DealDamage {
                damage_source: Some(DamageSource::Target),
                ..
            },
            Some(sub_ability)
        ) if matches!(
            &sub_ability.effect,
            Effect::DealDamage {
                damage_source: Some(DamageSource::Target),
                target: TargetFilter::ParentTargetSlot { index: 0 },
                ..
            }
        )
    ));

    for _ in 0..8 {
        if runner.state().stack.is_empty() {
            return;
        }
        runner.pass_both_players();
    }
    panic!("Self-Destruct did not resolve within priority reach guard");
}

#[test]
fn self_destruct_uses_two_power_and_only_the_source_dies() {
    let (mut runner, spell, source, recipient) = setup((2, 2), (3, 3), false);
    cast_self_destruct(&mut runner, spell, source, recipient);

    let state = runner.state();
    assert_eq!(state.objects[&spell].zone, Zone::Graveyard);
    assert_eq!(state.objects[&source].zone, Zone::Graveyard);
    assert_eq!(state.objects[&recipient].zone, Zone::Battlefield);
    assert_eq!(
        state.objects[&recipient].damage_marked, 2,
        "CR 120.1 + CR 208.1: the recipient takes the selected source's power"
    );
}

#[test]
fn self_destruct_three_power_trades_both_creatures() {
    let (mut runner, spell, source, recipient) = setup((3, 3), (3, 3), false);
    cast_self_destruct(&mut runner, spell, source, recipient);

    assert_eq!(runner.state().objects[&source].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&recipient].zone, Zone::Graveyard);
}

#[test]
fn self_destruct_uses_modified_effective_power() {
    let (mut runner, spell, source, recipient) = setup((2, 2), (3, 3), true);
    assert_eq!(
        runner.state().objects[&source].power,
        Some(3),
        "the continuous anthem must make the selected source's effective power 3"
    );
    cast_self_destruct(&mut runner, spell, source, recipient);

    assert_eq!(runner.state().objects[&source].zone, Zone::Graveyard);
    assert_eq!(
        runner.state().objects[&recipient].zone,
        Zone::Graveyard,
        "the modified power (3), rather than printed power (2), must determine X"
    );
}
