//! GitHub issue #1335 — Kain, Traitorous Dragoon combat-damage trigger.
//!
//! Oracle:
//! > Jump — During your turn, Kain has flying.
//! > Whenever Kain deals combat damage to a player, that player gains control of
//! > Kain. If they do, you draw that many cards, create that many tapped Treasure
//! > tokens, then lose that much life.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{DamageKindFilter, Effect, TargetFilter, TargetRef};
use engine::types::phase::Phase;
use engine::types::triggers::TriggerMode;

use super::rules::run_combat;

const KAIN_ORACLE: &str = "Jump — During your turn, Kain has flying.\n\
Whenever Kain deals combat damage to a player, that player gains control of Kain. \
If they do, you draw that many cards, create that many tapped Treasure tokens, \
then lose that much life.";

fn effect_chain_contains<F>(ability: &engine::types::ability::AbilityDefinition, pred: F) -> bool
where
    F: Fn(&Effect) -> bool + Copy,
{
    if pred(ability.effect.as_ref()) {
        return true;
    }
    ability
        .sub_ability
        .as_ref()
        .is_some_and(|sub| effect_chain_contains(sub, pred))
}

#[test]
fn kain_parses_jump_static_and_combat_damage_trigger() {
    let parsed = parse_oracle_text(
        KAIN_ORACLE,
        "Kain, Traitorous Dragoon",
        &[],
        &["Creature".to_string()],
        &["Human".to_string(), "Knight".to_string()],
    );

    assert!(
        !parsed.statics.is_empty(),
        "Jump reminder static should parse; statics={:?}",
        parsed.statics
    );

    assert_eq!(
        parsed.triggers.len(),
        1,
        "Kain should have exactly one trigger"
    );

    let combat_trigger = parsed
        .triggers
        .iter()
        .find(|t| t.mode == TriggerMode::DamageDone)
        .expect("combat damage trigger must parse");

    assert_eq!(
        combat_trigger.valid_source,
        Some(TargetFilter::SelfRef),
        "source must be SelfRef, got {:?}",
        combat_trigger.valid_source
    );
    assert_eq!(
        combat_trigger.valid_target,
        Some(TargetFilter::Player),
        "recipient filter must be Player, got {:?}",
        combat_trigger.valid_target
    );
    assert_eq!(
        combat_trigger.damage_kind,
        DamageKindFilter::CombatOnly,
        "must require combat damage"
    );
    assert!(
        combat_trigger.execute.is_some(),
        "trigger must have executable effect chain"
    );

    let execute = combat_trigger
        .execute
        .as_ref()
        .expect("trigger should have execute");

    assert!(
        matches!(
            execute.effect.as_ref(),
            Effect::GiveControl {
                target: TargetFilter::SelfRef,
                recipient: TargetFilter::TriggeringPlayer,
            }
        ),
        "root effect must be GiveControl; got {:?}",
        execute.effect
    );
    let rider = execute
        .sub_ability
        .as_ref()
        .expect("If they do rider must be a sub_ability");
    assert!(
        rider
            .condition
            .as_ref()
            .is_some_and(|c| c.is_optional_effect_performed()),
        "rider must be gated by effect_performed; got {:?}",
        rider.condition
    );

    assert!(
        effect_chain_contains(execute, |effect| {
            matches!(
                effect,
                Effect::GiveControl {
                    target: TargetFilter::SelfRef,
                    recipient: TargetFilter::TriggeringPlayer,
                }
            )
        }),
        "expected GiveControl(SelfRef, TriggeringPlayer); chain={execute:?}"
    );

    assert!(
        effect_chain_contains(execute, |effect| matches!(effect, Effect::Draw { .. })),
        "expected Draw in if-they-do chain"
    );
    assert!(
        effect_chain_contains(execute, |effect| matches!(effect, Effect::Token { .. })),
        "expected Token in if-they-do chain"
    );
    assert!(
        effect_chain_contains(execute, |effect| matches!(effect, Effect::LoseLife { .. })),
        "expected LoseLife in if-they-do chain"
    );
}

#[test]
fn kain_object_carries_parsed_combat_damage_trigger() {
    let mut scenario = GameScenario::new();
    let kain = scenario
        .add_creature_from_oracle(P0, "Kain, Traitorous Dragoon", 2, 4, KAIN_ORACLE)
        .id();
    let runner = scenario.build();
    let obj = &runner.state().objects[&kain];

    let triggers: Vec<_> = obj.trigger_definitions.iter_unchecked().collect();
    assert_eq!(triggers.len(), 1, "object should carry one trigger");
    let trig = triggers[0];
    assert_eq!(trig.definition.mode, TriggerMode::DamageDone);
    assert!(
        trig.definition.execute.is_some(),
        "object trigger must have executable effect chain"
    );
    let execute = trig.definition.execute.as_ref().unwrap();
    assert!(
        matches!(execute.effect.as_ref(), Effect::GiveControl { .. }),
        "runtime trigger root must be GiveControl, got {:?}",
        execute.effect
    );
}

#[test]
fn kain_resolve_chain_transfers_control_from_damage_dealt_event() {
    use engine::game::ability_utils::build_resolved_from_def;
    use engine::game::effects::resolve_ability_chain;
    use engine::game::layers::evaluate_layers;
    use engine::game::zones::create_object;
    use engine::types::ability::TargetRef;
    use engine::types::events::GameEvent;
    use engine::types::game_state::GameState;
    use engine::types::identifiers::CardId;
    use engine::types::player::PlayerId;
    use engine::types::zones::Zone;

    let parsed = parse_oracle_text(
        KAIN_ORACLE,
        "Kain, Traitorous Dragoon",
        &[],
        &["Creature".to_string()],
        &["Human".to_string(), "Knight".to_string()],
    );
    let trig = &parsed.triggers[0];
    let execute = trig.execute.as_ref().expect("execute");

    let mut state = GameState::new_two_player(42);
    for (idx, name) in ["Card A", "Card B", "Card C"].into_iter().enumerate() {
        create_object(
            &mut state,
            CardId((idx + 10) as u64),
            PlayerId(0),
            name.to_string(),
            Zone::Library,
        );
    }
    let kain = create_object(
        &mut state,
        CardId(1),
        PlayerId(0),
        "Kain, Traitorous Dragoon".to_string(),
        Zone::Battlefield,
    );
    state.current_trigger_event = Some(GameEvent::DamageDealt {
        source_id: kain,
        target: TargetRef::Player(PlayerId(1)),
        amount: 2,
        is_combat: true,
        excess: 0,
    });

    let resolved = build_resolved_from_def(execute, kain, PlayerId(0));
    let mut events = Vec::new();
    resolve_ability_chain(&mut state, &resolved, &mut events, 0).expect("chain resolves");
    evaluate_layers(&mut state);

    assert!(
        state.transient_continuous_effects.iter().any(|effect| {
            effect.modifications.iter().any(|m| {
                matches!(
                    m,
                    engine::types::ability::ContinuousModification::ChangeController
                )
            })
        }),
        "GiveControl must install ChangeController TCE; events={events:?}"
    );
    assert_eq!(
        state.objects[&kain].controller, P1,
        "damaged player must control Kain after layers"
    );
    assert_eq!(
        state.players[P0.0 as usize].hand.len(),
        2,
        "attacker should draw two after control transfer"
    );
}

#[test]
fn kain_stack_trigger_resolution_transfers_control_and_rewards_attacker() {
    use engine::game::layers::evaluate_layers;
    use engine::game::stack;
    use engine::game::triggers::{drain_order_triggers_with_identity, process_triggers};
    use engine::game::zones::create_object;
    use engine::types::events::GameEvent;
    use engine::types::game_state::GameState;
    use engine::types::identifiers::CardId;
    use engine::types::player::PlayerId;
    use engine::types::zones::Zone;

    let parsed = parse_oracle_text(
        KAIN_ORACLE,
        "Kain, Traitorous Dragoon",
        &[],
        &["Creature".to_string()],
        &["Human".to_string(), "Knight".to_string()],
    );
    let trigger = parsed.triggers[0].clone();

    let mut state = GameState::new_two_player(42);
    for (idx, name) in ["Card A", "Card B", "Card C"].into_iter().enumerate() {
        create_object(
            &mut state,
            CardId((idx + 10) as u64),
            PlayerId(0),
            name.to_string(),
            Zone::Library,
        );
    }
    let kain = create_object(
        &mut state,
        CardId(1),
        PlayerId(0),
        "Kain, Traitorous Dragoon".to_string(),
        Zone::Battlefield,
    );
    {
        let obj = state.objects.get_mut(&kain).unwrap();
        obj.trigger_definitions.push(trigger.clone());
        obj.base_trigger_definitions = std::sync::Arc::new(vec![trigger.clone()]);
    }
    state.trigger_index.remove(kain);
    let defs: smallvec::SmallVec<[engine::types::ability::TriggerDefinition; 4]> =
        smallvec::smallvec![trigger];
    state.trigger_index.add(kain, &defs, false);

    let event = GameEvent::DamageDealt {
        source_id: kain,
        target: TargetRef::Player(PlayerId(1)),
        amount: 2,
        is_combat: true,
        excess: 0,
    };
    process_triggers(&mut state, &[event]);
    drain_order_triggers_with_identity(&mut state);

    let mut events = Vec::new();
    while !state.stack.is_empty() {
        stack::resolve_top(&mut state, &mut events);
    }
    evaluate_layers(&mut state);

    assert!(
        state.transient_continuous_effects.iter().any(|effect| {
            effect.modifications.iter().any(|m| {
                matches!(
                    m,
                    engine::types::ability::ContinuousModification::ChangeController
                )
            })
        }),
        "stack path must install ChangeController TCE"
    );
    assert_eq!(state.objects[&kain].controller, P1);
    assert_eq!(state.players[P0.0 as usize].hand.len(), 2);
}

#[test]
fn kain_combat_damage_transfers_control_and_rewards_attacker() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Card A", "Card B", "Card C", "Card D"]);
    let kain = scenario
        .add_creature_from_oracle(P0, "Kain, Traitorous Dragoon", 2, 4, KAIN_ORACLE)
        .id();
    let mut runner = scenario.build();

    let p0_hand_before = runner.state().players[P0.0 as usize].hand.len();
    let p0_life_before = runner.state().players[P0.0 as usize].life;

    run_combat(&mut runner, vec![kain], vec![]);
    assert!(
        !runner.state().stack.is_empty(),
        "Kain's combat damage trigger should be on the stack after damage"
    );
    runner.advance_until_stack_empty();

    let has_control_effect = runner
        .state()
        .transient_continuous_effects
        .iter()
        .any(|effect| {
            effect.modifications.iter().any(|m| {
                matches!(
                    m,
                    engine::types::ability::ContinuousModification::ChangeController
                )
            })
        });
    assert!(
        has_control_effect,
        "GiveControl should install a Layer-2 ChangeController effect"
    );

    let p1_life = runner.state().players[P1.0 as usize].life;
    assert_eq!(p1_life, 18, "Kain should deal 2 combat damage to P1");

    assert!(
        !runner.state().objects[&kain].trigger_definitions.is_empty(),
        "Kain must register at least one trigger"
    );

    let p0_hand_after = runner.state().players[P0.0 as usize].hand.len();
    assert_eq!(
        p0_hand_after - p0_hand_before,
        2,
        "attacker should draw cards equal to combat damage dealt (Kain is 2/4)"
    );

    assert_eq!(
        runner.state().objects[&kain].controller,
        P1,
        "damaged player must gain control of Kain"
    );

    let p0_treasures = runner
        .state()
        .objects
        .values()
        .filter(|obj| {
            obj.controller == P0
                && obj
                    .card_types
                    .subtypes
                    .iter()
                    .any(|st| st.eq_ignore_ascii_case("Treasure"))
        })
        .count();
    assert_eq!(
        p0_treasures, 2,
        "attacker should receive two tapped Treasure tokens"
    );

    let p0_life_after = runner.state().players[P0.0 as usize].life;
    assert_eq!(
        p0_life_before - p0_life_after,
        2,
        "attacker should lose life equal to damage dealt"
    );
}
