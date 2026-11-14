//! Play with Fire must scry only after it actually damages a player.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::game::{effects, zones::create_object};
use engine::parser::oracle::parse_oracle_text;
use engine::parser::oracle_ir::diagnostic::OracleDiagnostic;
use engine::types::ability::{
    AbilityCondition, Comparator, DamageChannel, Effect, EffectScope, PreventionAmount,
    PreventionScope, QuantityExpr, ResolvedAbility, TargetFilter, TargetRef, TypeFilter,
};
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const PLAY_WITH_FIRE: &str = "Play with Fire deals 2 damage to any target. \
If a player is dealt damage this way, scry 1. (Look at the top card of your library. You may put that card on the bottom.)";

#[test]
fn play_with_fire_parses_player_damage_scry_condition() {
    let parsed = parse_oracle_text(
        PLAY_WITH_FIRE,
        "Play with Fire",
        &[],
        &["Instant".into()],
        &[],
    );
    let scry = parsed.abilities[0]
        .sub_ability
        .as_ref()
        .expect("Play with Fire must have a scry rider");
    let Some(AbilityCondition::And { conditions }) = &scry.condition else {
        panic!(
            "the scry rider must retain its player-damage condition: {:?}",
            scry.condition
        );
    };
    assert!(
        conditions.iter().any(|condition| matches!(
            condition,
            AbilityCondition::PreviousEffectAmount {
                comparator: Comparator::GT,
                rhs: QuantityExpr::Fixed { value: 0 },
                channel: DamageChannel::Total,
            }
        )),
        "the rider must require damage to actually be dealt: {conditions:?}"
    );
    assert!(
        conditions.iter().any(|condition| matches!(
            condition,
            AbilityCondition::Not { condition }
                if matches!(condition.as_ref(), AbilityCondition::TargetMatchesFilter {
                    filter: TargetFilter::Typed(filter),
                    use_lki: true,
                    subject_slot: None,
                } if filter.type_filters == vec![TypeFilter::Permanent])
        )),
        "the rider must reject permanent damage targets: {conditions:?}"
    );
    assert!(
        !parsed.parse_warnings.iter().any(|warning| matches!(
            warning,
            OracleDiagnostic::SwallowedClause { detector, .. } if detector == "Condition_If"
        )),
        "the represented player-damage condition must not be reported as swallowed: {:?}",
        parsed.parse_warnings
    );
}

struct PlayWithFireResult {
    scried: bool,
    player_life: i32,
    spell: ObjectId,
    creature: ObjectId,
    creature_zone: Zone,
    events: Vec<GameEvent>,
}

fn cast_play_with_fire(at_player: bool, prevent_player_damage: bool) -> PlayWithFireResult {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_card_to_library_top(P0, "Mountain");
    let creature = scenario.add_creature(P1, "Charming Scoundrel", 2, 2).id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Play with Fire", true, PLAY_WITH_FIRE)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Red],
            generic: 0,
        })
        .id();
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(ManaType::Red, ObjectId(9_901), false, vec![])],
    );
    let mut runner = scenario.build();
    if prevent_player_damage {
        let shield = create_object(
            runner.state_mut(),
            CardId(9_902),
            P1,
            "Player Shield".into(),
            Zone::Stack,
        );
        let prevention = ResolvedAbility::new(
            Effect::PreventDamage {
                amount: PreventionAmount::All,
                amount_dynamic: None,
                target: TargetFilter::Controller,
                recipient_scope: EffectScope::Single,
                scope: PreventionScope::AllDamage,
                damage_source_filter: None,
                prevention_duration: None,
            },
            vec![],
            shield,
            P1,
        );
        effects::resolve_ability_chain(runner.state_mut(), &prevention, &mut vec![], 0)
            .expect("installing the player-damage prevention shield must succeed");
    }
    let card_id = runner.state().objects[&spell].card_id;
    let cast = runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("casting Play with Fire must succeed");
    let mut events = cast.events;

    for _ in 0..24 {
        match runner.state().waiting_for.clone() {
            WaitingFor::TargetSelection { .. } => {
                let result = runner
                    .act(GameAction::SelectTargets {
                        targets: vec![if at_player {
                            TargetRef::Player(P1)
                        } else {
                            TargetRef::Object(creature)
                        }],
                    })
                    .expect("selecting Play with Fire's target must succeed");
                events.extend(result.events);
            }
            WaitingFor::ScryChoice { .. } => {
                return PlayWithFireResult {
                    scried: true,
                    player_life: runner.state().players[P1.0 as usize].life,
                    spell,
                    creature,
                    creature_zone: runner.state().objects[&creature].zone,
                    events,
                };
            }
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => {
                return PlayWithFireResult {
                    scried: false,
                    player_life: runner.state().players[P1.0 as usize].life,
                    spell,
                    creature,
                    creature_zone: runner.state().objects[&creature].zone,
                    events,
                };
            }
            WaitingFor::Priority { .. } => {
                let result = runner
                    .act(GameAction::PassPriority)
                    .expect("passing priority must succeed");
                events.extend(result.events);
            }
            other => panic!("unexpected Play with Fire prompt: {other:?}"),
        }
    }
    panic!("Play with Fire did not settle within the prompt budget");
}

#[test]
fn play_with_fire_damages_player_and_scries() {
    let result = cast_play_with_fire(true, false);
    assert!(result.scried, "damaging a player must offer scry 1");
    assert_eq!(
        result.player_life, 18,
        "the targeted player must take 2 damage"
    );
}

#[test]
fn play_with_fire_damages_creature_without_scrying() {
    let result = cast_play_with_fire(false, false);
    assert!(!result.scried, "damaging a creature must not offer scry 1");
    assert_eq!(
        result.player_life, 20,
        "damaging a creature must not change player life"
    );
    assert_eq!(
        result.creature_zone,
        Zone::Graveyard,
        "2 damage is lethal to the targeted 2/2 creature"
    );
    assert!(result.events.iter().any(|event| matches!(
        event,
        GameEvent::DamageDealt {
            source_id,
            target: TargetRef::Object(id),
            amount: 2,
            is_combat: false,
            ..
        } if *source_id == result.spell && *id == result.creature
    )));
}

#[test]
fn play_with_fire_prevented_player_damage_does_not_scry() {
    let result = cast_play_with_fire(true, true);
    assert!(
        !result.scried,
        "prevented player damage must not offer scry 1"
    );
    assert_eq!(
        result.player_life, 20,
        "the player-damage prevention shield must be applied"
    );
}
