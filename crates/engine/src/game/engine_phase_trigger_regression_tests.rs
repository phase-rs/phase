use std::sync::Arc;

use super::tests::apply_oracle_to_object;
use super::*;
use crate::game::combat::AttackTarget;
use crate::game::zones::create_object;
use crate::parser::oracle::parse_oracle_text;
use crate::types::ability::{
    AbilityCondition, AbilityCost, AbilityDefinition, AbilityKind, ControllerRef, Effect,
    EffectScope, FilterProp, ObjectScope, PlayerFilter, QuantityExpr, QuantityRef,
    ReplacementDefinition, ReplacementMode, ResolvedAbility, TapStateChange, TargetFilter,
    TargetRef, TriggerConstraint, TriggerDefinition, TypeFilter, TypedFilter, UnlessPayModifier,
};
use crate::types::card::CardFace;
use crate::types::card_type::CoreType;
use crate::types::format::FormatConfig;
use crate::types::identifiers::{CardId, ObjectId};
use crate::types::keywords::Keyword;
use crate::types::mana::{ManaColor, ManaCost, ManaType, ManaUnit};
use crate::types::player::PlayerId;
use crate::types::replacements::ReplacementEvent;
use crate::types::triggers::TriggerMode;
use crate::types::zones::Zone;

fn setup_game_at_main_phase() -> GameState {
    let mut state = new_game(42);
    state.turn_number = 2;
    state.phase = Phase::PreCombatMain;
    state.active_player = PlayerId(0);
    state.priority_player = PlayerId(0);
    state.waiting_for = WaitingFor::Priority {
        player: PlayerId(0),
    };
    state
}

fn draw_ability(count: i32) -> AbilityDefinition {
    AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::Draw {
            count: QuantityExpr::Fixed { value: count },
            target: TargetFilter::Controller,
        },
    )
}

fn draw_that_many(source_id: ObjectId, controller: PlayerId) -> ResolvedAbility {
    ResolvedAbility::new(
        Effect::Draw {
            count: QuantityExpr::Ref {
                qty: QuantityRef::EventContextAmount,
            },
            target: TargetFilter::Controller,
        },
        vec![],
        source_id,
        controller,
    )
}

fn hand_to_battlefield_choice_ability(
    source_id: ObjectId,
    controller: PlayerId,
) -> ResolvedAbility {
    ResolvedAbility::new(
        Effect::ChangeZone {
            origin: Some(Zone::Hand),
            destination: Zone::Battlefield,
            target: TargetFilter::Any,
            owner_library: false,
            enter_transformed: false,
            enters_under: None,
            enter_tapped: crate::types::zones::EtbTapState::Unspecified,
            enters_attacking: false,
            up_to: false,
            enter_with_counters: vec![],
            conditional_enter_with_counters: vec![],
            face_down_profile: None,
            enters_modified_if: None,
        },
        vec![],
        source_id,
        controller,
    )
}

/// Verify that combat is skipped when there are no attackers and no triggers.
/// With no BeginCombat triggers and no potential attackers, auto_advance()
/// skips straight to PostCombatMain.
#[test]
fn combat_skipped_when_no_attackers_no_triggers() {
    let mut state = new_game(42);
    state.turn_number = 2;
    state.phase = Phase::PreCombatMain;
    state.active_player = PlayerId(0);
    state.priority_player = PlayerId(0);
    state.waiting_for = WaitingFor::Priority {
        player: PlayerId(0),
    };

    // Create a 0/1 creature with no triggers — can't attack, no combat triggers.
    let creature_id = create_object(
        &mut state,
        CardId(200),
        PlayerId(0),
        "Wall".to_string(),
        Zone::Battlefield,
    );
    {
        let obj = state.objects.get_mut(&creature_id).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.power = Some(0);
        obj.toughness = Some(1);
    }

    // Pass priority twice (P0 passes, then P1 passes) with empty stack.
    // This advances from PreCombatMain → BeginCombat → no triggers, no
    // attackers → skip to PostCombatMain.
    let result1 = apply_as_current(&mut state, GameAction::PassPriority).unwrap();
    assert!(matches!(
        result1.waiting_for,
        WaitingFor::Priority {
            player: PlayerId(1)
        }
    ));

    let result2 = apply_as_current(&mut state, GameAction::PassPriority).unwrap();

    // We should now be at PostCombatMain with empty stack.
    assert_eq!(state.phase, Phase::PostCombatMain);
    assert!(
        state.stack.is_empty(),
        "Stack should be empty — no triggers exist. Stack: {:?}",
        state.stack
    );
    assert!(
        state.pending_trigger.is_none(),
        "No pending trigger should exist"
    );
    assert!(matches!(result2.waiting_for, WaitingFor::Priority { .. }));
}

/// CR 503.1a: Upkeep triggers fire when the upkeep step begins.
#[test]
fn upkeep_trigger_fires() {
    let mut state = new_game(42);
    state.turn_number = 2;
    state.phase = Phase::Untap;
    state.active_player = PlayerId(0);
    state.priority_player = PlayerId(0);

    // Create creature with "At the beginning of your upkeep, gain 1 life"
    let creature_id = create_object(
        &mut state,
        CardId(200),
        PlayerId(0),
        "Upkeep Creature".to_string(),
        Zone::Battlefield,
    );
    {
        let obj = state.objects.get_mut(&creature_id).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.power = Some(1);
        obj.toughness = Some(1);
        obj.trigger_definitions.push(
            TriggerDefinition::new(TriggerMode::Phase)
                .phase(Phase::Upkeep)
                .constraint(TriggerConstraint::OnlyDuringYourTurn)
                .execute(AbilityDefinition::new(
                    AbilityKind::Activated,
                    Effect::GainLife {
                        amount: QuantityExpr::Fixed { value: 1 },
                        player: TargetFilter::Controller,
                    },
                ))
                .trigger_zones(vec![Zone::Battlefield]),
        );
    }

    // auto_advance from Untap should process Upkeep triggers inline
    let mut events = Vec::new();
    let wf = crate::game::turns::auto_advance(&mut state, &mut events);

    assert_eq!(state.phase, Phase::Upkeep);
    assert!(
        !state.stack.is_empty() || state.pending_trigger.is_some(),
        "Upkeep trigger should have fired"
    );
    assert!(matches!(wf, WaitingFor::Priority { .. }));
}

/// CR 507.1: BeginCombat triggers fire even when there are attackers.
#[test]
fn begin_combat_trigger_fires_with_attackers() {
    let mut state = new_game(42);
    state.turn_number = 2;
    state.phase = Phase::PreCombatMain;
    state.active_player = PlayerId(0);
    state.priority_player = PlayerId(0);
    state.waiting_for = WaitingFor::Priority {
        player: PlayerId(0),
    };

    // Create a 2/2 creature (can attack) with a BeginCombat trigger
    let creature_id = create_object(
        &mut state,
        CardId(200),
        PlayerId(0),
        "Combat Creature".to_string(),
        Zone::Battlefield,
    );
    {
        let obj = state.objects.get_mut(&creature_id).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.power = Some(2);
        obj.toughness = Some(2);
        obj.trigger_definitions.push(
            TriggerDefinition::new(TriggerMode::Phase)
                .phase(Phase::BeginCombat)
                .execute(AbilityDefinition::new(
                    AbilityKind::Activated,
                    Effect::GainLife {
                        amount: QuantityExpr::Fixed { value: 1 },
                        player: TargetFilter::Controller,
                    },
                ))
                .trigger_zones(vec![Zone::Battlefield]),
        );
    }

    // Pass priority from PreCombatMain
    let result1 = apply_as_current(&mut state, GameAction::PassPriority).unwrap();
    assert!(matches!(
        result1.waiting_for,
        WaitingFor::Priority {
            player: PlayerId(1)
        }
    ));
    let _result2 = apply_as_current(&mut state, GameAction::PassPriority).unwrap();

    // Should be at BeginCombat with trigger on stack
    assert_eq!(state.phase, Phase::BeginCombat);
    assert!(
        !state.stack.is_empty() || state.pending_trigger.is_some(),
        "BeginCombat trigger should have fired"
    );
}

/// CR 507.1: BeginCombat triggers fire even without potential attackers.
#[test]
fn begin_combat_trigger_fires_without_attackers() {
    let mut state = new_game(42);
    state.turn_number = 2;
    state.phase = Phase::PreCombatMain;
    state.active_player = PlayerId(0);
    state.priority_player = PlayerId(0);
    state.waiting_for = WaitingFor::Priority {
        player: PlayerId(0),
    };

    // Create a 0/1 creature (can't attack) with a BeginCombat trigger
    let creature_id = create_object(
        &mut state,
        CardId(200),
        PlayerId(0),
        "Trigger Wall".to_string(),
        Zone::Battlefield,
    );
    {
        let obj = state.objects.get_mut(&creature_id).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.power = Some(0);
        obj.toughness = Some(1);
        obj.trigger_definitions.push(
            TriggerDefinition::new(TriggerMode::Phase)
                .phase(Phase::BeginCombat)
                .execute(AbilityDefinition::new(
                    AbilityKind::Activated,
                    Effect::GainLife {
                        amount: QuantityExpr::Fixed { value: 1 },
                        player: TargetFilter::Controller,
                    },
                ))
                .trigger_zones(vec![Zone::Battlefield]),
        );
    }

    // Pass priority twice to advance from PreCombatMain
    let result1 = apply_as_current(&mut state, GameAction::PassPriority).unwrap();
    assert!(matches!(
        result1.waiting_for,
        WaitingFor::Priority {
            player: PlayerId(1)
        }
    ));
    let _result2 = apply_as_current(&mut state, GameAction::PassPriority).unwrap();

    // Should be at BeginCombat with trigger on stack and combat state set
    assert_eq!(state.phase, Phase::BeginCombat);
    assert!(
        state.combat.is_some(),
        "Combat state should be set when triggers fire"
    );
    assert!(
        !state.stack.is_empty() || state.pending_trigger.is_some(),
        "BeginCombat trigger should fire even without potential attackers (CR 507.1)"
    );
}

/// OnlyDuringYourTurn constraint prevents trigger from firing on opponent's turn.
#[test]
fn your_turn_constraint_blocks_on_opponents_turn() {
    let mut state = new_game(42);
    state.turn_number = 2;
    state.phase = Phase::Untap;
    // Active player is P1, but the creature is controlled by P0
    state.active_player = PlayerId(1);
    state.priority_player = PlayerId(1);

    // Create creature controlled by P0 with "At the beginning of your upkeep"
    let creature_id = create_object(
        &mut state,
        CardId(200),
        PlayerId(0),
        "Your Turn Creature".to_string(),
        Zone::Battlefield,
    );
    {
        let obj = state.objects.get_mut(&creature_id).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.power = Some(1);
        obj.toughness = Some(1);
        obj.trigger_definitions.push(
            TriggerDefinition::new(TriggerMode::Phase)
                .phase(Phase::Upkeep)
                .constraint(TriggerConstraint::OnlyDuringYourTurn)
                .execute(AbilityDefinition::new(
                    AbilityKind::Activated,
                    Effect::GainLife {
                        amount: QuantityExpr::Fixed { value: 1 },
                        player: TargetFilter::Controller,
                    },
                ))
                .trigger_zones(vec![Zone::Battlefield]),
        );
    }

    // auto_advance from Untap — it's P1's turn, but the trigger is P0's
    // with OnlyDuringYourTurn, so it should NOT fire.
    let mut events = Vec::new();
    let _wf = crate::game::turns::auto_advance(&mut state, &mut events);

    // Trigger should not have fired — phase should have advanced past Upkeep
    assert!(
        state.stack.is_empty(),
        "Trigger with OnlyDuringYourTurn should not fire on opponent's turn"
    );
    assert!(state.pending_trigger.is_none());
}

/// Put a Go-Shintai of Boundless Vigor (the issue #1243 card) onto the
/// battlefield under P0, with its real parsed trigger set, and return its id.
/// The card is its own Shrine, so it is always a legal reflexive target.
fn put_boundless_go_shintai(state: &mut GameState) -> ObjectId {
    let parsed = crate::parser::oracle::parse_oracle_text(
            "Trample\nAt the beginning of your end step, you may pay {1}. When you do, put a +1/+1 counter on target Shrine for each Shrine you control.",
            "Go-Shintai of Boundless Vigor",
            &[],
            &["Enchantment".to_string(), "Creature".to_string()],
            &["Shrine".to_string(), "Spirit".to_string()],
        );
    assert!(
        !parsed.triggers.is_empty(),
        "parser must produce the end-step trigger, got {parsed:?}"
    );

    let id = create_object(
        state,
        CardId(200),
        PlayerId(0),
        "Go-Shintai of Boundless Vigor".to_string(),
        Zone::Battlefield,
    );
    let obj = state.objects.get_mut(&id).unwrap();
    obj.card_types.core_types.push(CoreType::Creature);
    obj.card_types.core_types.push(CoreType::Enchantment);
    obj.card_types.subtypes.push("Shrine".to_string());
    obj.power = Some(5);
    obj.toughness = Some(5);
    for t in parsed.triggers {
        obj.trigger_definitions.push(t);
    }
    obj.base_card_types = obj.card_types.clone();
    id
}

fn put_kitt_kanto(state: &mut GameState) -> ObjectId {
    let parsed = crate::parser::oracle::parse_oracle_text(
        "When Kitt Kanto enters, create a 1/1 green and white Citizen creature token.\nAt the beginning of combat on each player's turn, you may tap two untapped creatures you control. When you do, target creature that player controls gets +2/+2 and gains trample until end of turn. Goad that creature.",
        "Kitt Kanto, Mayhem Diva",
        &[],
        &["Creature".to_string()],
        &["Cat".to_string(), "Bard".to_string(), "Druid".to_string()],
    );
    assert!(
        parsed
            .triggers
            .iter()
            .any(|trigger| trigger.phase == Some(Phase::BeginCombat)),
        "parser must produce Kitt's beginning-of-combat trigger, got {parsed:?}"
    );

    let id = create_object(
        state,
        CardId(3262),
        PlayerId(0),
        "Kitt Kanto, Mayhem Diva".to_string(),
        Zone::Battlefield,
    );
    let obj = state.objects.get_mut(&id).unwrap();
    obj.card_types.core_types.push(CoreType::Creature);
    obj.card_types.subtypes.push("Cat".to_string());
    obj.card_types.subtypes.push("Bard".to_string());
    obj.card_types.subtypes.push("Druid".to_string());
    obj.power = Some(3);
    obj.toughness = Some(3);
    for trigger in parsed.triggers {
        if trigger.phase == Some(Phase::BeginCombat) {
            obj.trigger_definitions.push(trigger);
        }
    }
    obj.base_card_types = obj.card_types.clone();
    id
}

fn shintai_p1p1_counters(state: &GameState, id: ObjectId) -> u32 {
    state
        .objects
        .get(&id)
        .and_then(|o| {
            o.counters
                .get(&crate::types::counter::CounterType::Plus1Plus1)
                .copied()
        })
        .unwrap_or(0)
}

fn put_hidden_cruelty_go_shintai(state: &mut GameState) -> ObjectId {
    let parsed = parse_oracle_text(
        "Deathtouch\nAt the beginning of your end step, you may pay {1}. When you do, destroy target creature with toughness X or less, where X is the number of Shrines you control.",
        "Go-Shintai of Hidden Cruelty",
        &[],
        &["Enchantment".to_string(), "Creature".to_string()],
        &["Shrine".to_string(), "Spirit".to_string()],
    );
    assert!(
        !parsed.triggers.is_empty(),
        "parser must produce the end-step trigger, got {parsed:?}"
    );

    let id = create_object(
        state,
        CardId(46630),
        PlayerId(0),
        "Go-Shintai of Hidden Cruelty".to_string(),
        Zone::Battlefield,
    );
    let obj = state.objects.get_mut(&id).unwrap();
    obj.card_types.core_types.push(CoreType::Creature);
    obj.card_types.core_types.push(CoreType::Enchantment);
    obj.card_types.subtypes.push("Shrine".to_string());
    obj.card_types.subtypes.push("Spirit".to_string());
    obj.power = Some(2);
    obj.toughness = Some(2);
    for trigger in parsed.triggers {
        obj.trigger_definitions.push(trigger);
    }
    obj.base_card_types = obj.card_types.clone();
    id
}

fn put_pt_creature(
    state: &mut GameState,
    card_id: u32,
    controller: PlayerId,
    name: &str,
    power: i32,
    toughness: i32,
) -> ObjectId {
    let id = create_object(
        state,
        CardId(card_id.into()),
        controller,
        name.to_string(),
        Zone::Battlefield,
    );
    let obj = state.objects.get_mut(&id).unwrap();
    obj.card_types.core_types.push(CoreType::Creature);
    obj.power = Some(power);
    obj.toughness = Some(toughness);
    obj.base_card_types = obj.card_types.clone();
    id
}

/// Issue #1243 — class regression. "At the beginning of your end step, you
/// may pay {1}. When you do, <effect>." parses into an end-step `Phase`
/// trigger whose execute is an optional `PayCost` carrying a reflexive
/// `WhenYouDo` sub-ability. CR 513.1a (beginning-of-end-step trigger) + CR
/// 603.1 (a triggered ability uses the stack) require the trigger to be put
/// on the stack and resolved; CR 603.12 makes "when you do" a reflexive
/// trigger that fires only if the optional payment is made. The shape is
/// shared by all four Boundless-era Go-Shintai and ~12 other "you may pay
/// {1}. When you do" cards, so this guards the whole class.
///
/// Accept path: the {1} is paid and the reflexive PutCounter resolves,
/// placing one +1/+1 counter on the lone Shrine.
#[test]
fn issue_1243_end_step_may_pay_trigger_accept_pays_and_resolves_reflexive() {
    let mut state = new_game(42);
    state.turn_number = 2;
    state.phase = Phase::End;
    state.active_player = PlayerId(0);
    state.priority_player = PlayerId(0);
    state.waiting_for = WaitingFor::Priority {
        player: PlayerId(0),
    };

    let id = put_boundless_go_shintai(&mut state);
    // One generic mana so the {1} is payable at resolution.
    state.players[0].mana_pool.add(ManaUnit::new(
        ManaType::Colorless,
        ObjectId(999),
        false,
        Vec::new(),
    ));

    let mut events = Vec::new();
    crate::game::turns::auto_advance(&mut state, &mut events);
    // CR 603.1 + CR 513.1a: the trigger must reach the stack, not be skipped.
    assert!(
        !state.stack.is_empty() || state.pending_trigger.is_some(),
        "end-step may-pay trigger must fire (waiting={:?})",
        state.waiting_for
    );

    let mut saw_may_prompt = false;
    for _ in 0..20 {
        match state.waiting_for.clone() {
            WaitingFor::Priority { player } => {
                if state.stack.is_empty() {
                    break;
                }
                apply(&mut state, player, GameAction::PassPriority).unwrap();
            }
            // CR 603.12: the "you may pay {1}" choice on resolution.
            WaitingFor::OptionalEffectChoice { player, .. } => {
                saw_may_prompt = true;
                apply(
                    &mut state,
                    player,
                    GameAction::DecideOptionalEffect { accept: true },
                )
                .unwrap();
            }
            // Reflexive "when you do" target: the only Shrine is the source.
            WaitingFor::TriggerTargetSelection { player, .. }
            | WaitingFor::TargetSelection { player, .. } => {
                apply(
                    &mut state,
                    player,
                    GameAction::SelectTargets {
                        targets: vec![TargetRef::Object(id)],
                    },
                )
                .unwrap();
            }
            _ => break,
        }
    }

    assert!(
        saw_may_prompt,
        "the 'may pay {{1}}' prompt (CR 603.12) must be surfaced at the end step"
    );
    assert_eq!(
        shintai_p1p1_counters(&state, id),
        1,
        "paying {{1}} must place one +1/+1 counter on the lone Shrine"
    );
    assert_eq!(
        state.players[0].mana_pool.mana.len(),
        0,
        "the {{1}} must actually be paid on accept"
    );
}

/// CR 118.12 + CR 603.12 + CR 701.26a + CR 102.1: Kitt Kanto's
/// beginning-of-combat trigger asks its controller to tap two untapped
/// creatures they control as a resolution-time optional cost. If paid, the
/// reflexive target must be a creature controlled by the active player whose
/// turn it is.
#[test]
fn issue_3262_kitt_kanto_taps_two_then_targets_active_player_creature() {
    let mut state = new_game(42);
    state.turn_number = 2;
    state.phase = Phase::PreCombatMain;
    state.active_player = PlayerId(1);
    state.priority_player = PlayerId(1);
    state.waiting_for = WaitingFor::Priority {
        player: PlayerId(1),
    };

    put_kitt_kanto(&mut state);
    let first_cost_creature =
        put_pt_creature(&mut state, 32620, PlayerId(0), "First Citizen", 1, 1);
    let second_cost_creature =
        put_pt_creature(&mut state, 32621, PlayerId(0), "Second Citizen", 1, 1);
    let active_player_creature = put_pt_creature(
        &mut state,
        32622,
        PlayerId(1),
        "Active Player Creature",
        1,
        1,
    );
    let controller_creature =
        put_pt_creature(&mut state, 32623, PlayerId(0), "Controller Creature", 1, 1);

    apply_as_current(&mut state, GameAction::PassPriority).unwrap();
    apply_as_current(&mut state, GameAction::PassPriority).unwrap();
    assert_eq!(state.phase, Phase::BeginCombat);
    assert!(
        !state.stack.is_empty() || state.pending_trigger.is_some(),
        "Kitt's beginning-of-combat trigger must fire on each player's turn"
    );

    let mut saw_may_prompt = false;
    let mut saw_tap_cost_prompt = false;
    let mut saw_reflexive_target_prompt = false;
    for _ in 0..30 {
        match state.waiting_for.clone() {
            WaitingFor::Priority { player } => {
                if state.stack.is_empty() {
                    break;
                }
                apply(&mut state, player, GameAction::PassPriority).unwrap();
            }
            WaitingFor::OptionalEffectChoice { player, .. } => {
                saw_may_prompt = true;
                apply(
                    &mut state,
                    player,
                    GameAction::DecideOptionalEffect { accept: true },
                )
                .unwrap();
            }
            WaitingFor::PayCost {
                player,
                kind: PayCostKind::TapCreatures { .. },
                choices,
                count,
                ..
            } => {
                saw_tap_cost_prompt = true;
                assert_eq!(player, PlayerId(0));
                assert_eq!(count, 2);
                assert!(choices.contains(&first_cost_creature));
                assert!(choices.contains(&second_cost_creature));
                assert!(!choices.contains(&active_player_creature));
                apply(
                    &mut state,
                    player,
                    GameAction::SelectCards {
                        cards: vec![first_cost_creature, second_cost_creature],
                    },
                )
                .unwrap();
            }
            WaitingFor::TriggerTargetSelection {
                player,
                target_slots,
                ..
            } => {
                assert_eq!(
                    state
                        .pending_trigger
                        .as_ref()
                        .map(|pending| pending.ability.scoped_player),
                    Some(Some(PlayerId(1))),
                    "reflexive pending trigger must retain the active-player scoped binding"
                );
                let legal_targets = &target_slots[0].legal_targets;
                assert!(
                    legal_targets.contains(&TargetRef::Object(active_player_creature)),
                    "reflexive target prompt must include the active player's creature, got {legal_targets:?}"
                );
                assert!(
                    !legal_targets.contains(&TargetRef::Object(controller_creature)),
                    "reflexive target prompt must not include the controller's own creature"
                );
                saw_reflexive_target_prompt = true;
                apply(
                    &mut state,
                    player,
                    GameAction::SelectTargets {
                        targets: vec![TargetRef::Object(active_player_creature)],
                    },
                )
                .unwrap();
            }
            WaitingFor::TargetSelection {
                player,
                target_slots,
                ..
            } => {
                let legal_targets = &target_slots[0].legal_targets;
                assert!(
                    legal_targets.contains(&TargetRef::Object(active_player_creature)),
                    "reflexive target prompt must include the active player's creature, got {legal_targets:?}"
                );
                assert!(
                    !legal_targets.contains(&TargetRef::Object(controller_creature)),
                    "reflexive target prompt must not include the controller's own creature"
                );
                saw_reflexive_target_prompt = true;
                apply(
                    &mut state,
                    player,
                    GameAction::SelectTargets {
                        targets: vec![TargetRef::Object(active_player_creature)],
                    },
                )
                .unwrap();
            }
            _ => break,
        }
    }

    assert!(saw_may_prompt, "Kitt must offer the optional tap cost");
    assert!(
        saw_tap_cost_prompt,
        "accepting Kitt's cost must surface a tap-creatures PayCost prompt"
    );
    assert!(
        saw_reflexive_target_prompt,
        "paying the tap cost must create the WhenYouDo target prompt"
    );
    assert!(state.objects[&first_cost_creature].tapped);
    assert!(state.objects[&second_cost_creature].tapped);
    assert_eq!(state.objects[&active_player_creature].power, Some(3));
    assert_eq!(state.objects[&active_player_creature].toughness, Some(3));
    assert_eq!(
        state.objects[&controller_creature].power,
        Some(1),
        "\"that player controls\" must not retarget the controller's creature"
    );
}

#[test]
fn issue_4663_go_shintai_hidden_cruelty_optional_pay_target_list_uses_where_x() {
    let mut state = new_game(42);
    state.turn_number = 2;
    state.phase = Phase::End;
    state.active_player = PlayerId(0);
    state.priority_player = PlayerId(0);
    state.waiting_for = WaitingFor::Priority {
        player: PlayerId(0),
    };

    put_hidden_cruelty_go_shintai(&mut state);
    let second_shrine = put_pt_creature(&mut state, 46631, PlayerId(0), "Second Shrine", 1, 1);
    let second_shrine_obj = state.objects.get_mut(&second_shrine).unwrap();
    second_shrine_obj
        .card_types
        .subtypes
        .push("Shrine".to_string());
    second_shrine_obj.base_card_types = second_shrine_obj.card_types.clone();
    let low_toughness =
        put_pt_creature(&mut state, 46632, PlayerId(1), "Legal Toughness Two", 1, 2);
    let high_toughness = put_pt_creature(
        &mut state,
        46633,
        PlayerId(1),
        "Illegal Toughness Three",
        1,
        3,
    );
    state.players[0].mana_pool.add(ManaUnit::new(
        ManaType::Colorless,
        ObjectId(46634),
        false,
        Vec::new(),
    ));

    let mut events = Vec::new();
    crate::game::turns::auto_advance(&mut state, &mut events);

    for _ in 0..20 {
        match state.waiting_for.clone() {
            WaitingFor::Priority { player } => {
                if state.stack.is_empty() {
                    break;
                }
                apply(&mut state, player, GameAction::PassPriority).unwrap();
            }
            WaitingFor::OptionalEffectChoice { player, .. } => {
                apply(
                    &mut state,
                    player,
                    GameAction::DecideOptionalEffect { accept: true },
                )
                .unwrap();
            }
            WaitingFor::TriggerTargetSelection { target_slots, .. }
            | WaitingFor::TargetSelection { target_slots, .. } => {
                assert_eq!(target_slots.len(), 1);
                assert!(
                    target_slots[0]
                        .legal_targets
                        .contains(&TargetRef::Object(low_toughness)),
                    "two Shrines should allow toughness-2 target: {:?}",
                    target_slots[0].legal_targets
                );
                assert!(
                    !target_slots[0]
                        .legal_targets
                        .contains(&TargetRef::Object(high_toughness)),
                    "two Shrines should exclude toughness-3 target: {:?}",
                    target_slots[0].legal_targets
                );
                return;
            }
            other => panic!("unexpected state while driving Hidden Cruelty trigger: {other:?}"),
        }
    }

    panic!(
        "never reached Hidden Cruelty reflexive target selection; final state = {:?}",
        state.waiting_for
    );
}

/// Issue #1243 — decline path. The trigger still goes on the stack and the
/// "may pay {1}" choice is still offered (CR 603.1), but declining means the
/// reflexive CR 603.12 "when you do" never triggers: no payment, no counter,
/// and the turn proceeds cleanly.
#[test]
fn issue_1243_end_step_may_pay_trigger_decline_places_no_counter() {
    let mut state = new_game(42);
    state.turn_number = 2;
    state.phase = Phase::End;
    state.active_player = PlayerId(0);
    state.priority_player = PlayerId(0);
    state.waiting_for = WaitingFor::Priority {
        player: PlayerId(0),
    };

    let id = put_boundless_go_shintai(&mut state);
    state.players[0].mana_pool.add(ManaUnit::new(
        ManaType::Colorless,
        ObjectId(999),
        false,
        Vec::new(),
    ));

    let mut events = Vec::new();
    crate::game::turns::auto_advance(&mut state, &mut events);
    assert!(
        !state.stack.is_empty() || state.pending_trigger.is_some(),
        "end-step may-pay trigger must fire even when it will be declined"
    );

    let mut saw_may_prompt = false;
    for _ in 0..20 {
        match state.waiting_for.clone() {
            WaitingFor::Priority { player } => {
                if state.stack.is_empty() {
                    break;
                }
                apply(&mut state, player, GameAction::PassPriority).unwrap();
            }
            WaitingFor::OptionalEffectChoice { player, .. } => {
                saw_may_prompt = true;
                apply(
                    &mut state,
                    player,
                    GameAction::DecideOptionalEffect { accept: false },
                )
                .unwrap();
            }
            _ => break,
        }
    }

    assert!(
        saw_may_prompt,
        "the 'may pay {{1}}' prompt must still be offered before declining"
    );
    assert_eq!(
        shintai_p1p1_counters(&state, id),
        0,
        "declining must place no counter (CR 603.12 reflexive does not trigger)"
    );
}

#[test]
fn spell_cast_trigger_syncs_priority_to_active_player() {
    let mut state = new_game(42);
    state.turn_number = 2;
    state.phase = Phase::PreCombatMain;
    state.active_player = PlayerId(0);
    state.priority_player = PlayerId(1);
    state.waiting_for = WaitingFor::Priority {
        player: PlayerId(1),
    };

    let creature_spell = create_object(
        &mut state,
        CardId(300),
        PlayerId(0),
        "Bear Cub".to_string(),
        Zone::Stack,
    );
    state
        .objects
        .get_mut(&creature_spell)
        .unwrap()
        .card_types
        .core_types
        .push(CoreType::Creature);
    state.stack.push_back(crate::types::game_state::StackEntry {
        id: creature_spell,
        source_id: creature_spell,
        controller: PlayerId(0),
        kind: crate::types::game_state::StackEntryKind::Spell {
            card_id: CardId(300),
            ability: None,
            casting_variant: crate::types::game_state::CastingVariant::Normal,
            actual_mana_spent: 0,
        },
    });

    let spell_cast_trigger_creature = create_object(
        &mut state,
        CardId(301),
        PlayerId(1),
        "Spell Trigger Creature".to_string(),
        Zone::Battlefield,
    );
    {
        let obj = state.objects.get_mut(&spell_cast_trigger_creature).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.trigger_definitions
            .push(
                TriggerDefinition::new(TriggerMode::SpellCast).execute(AbilityDefinition::new(
                    AbilityKind::Database,
                    Effect::Draw {
                        count: QuantityExpr::Fixed { value: 1 },
                        target: TargetFilter::Controller,
                    },
                )),
            );
    }

    let searing_spear = create_object(
        &mut state,
        CardId(302),
        PlayerId(1),
        "Searing Spear".to_string(),
        Zone::Hand,
    );
    state
        .objects
        .get_mut(&searing_spear)
        .unwrap()
        .card_types
        .core_types
        .push(CoreType::Instant);

    let result = apply_as_current(
        &mut state,
        GameAction::CastSpell {
            object_id: searing_spear,
            card_id: CardId(302),
            targets: Vec::new(),

            payment_mode: crate::types::game_state::CastPaymentMode::Auto,
        },
    )
    .unwrap();

    assert!(matches!(
        result.waiting_for,
        WaitingFor::Priority {
            player: PlayerId(0)
        }
    ));
    assert!(matches!(
        state.waiting_for,
        WaitingFor::Priority {
            player: PlayerId(0)
        }
    ));
    assert_eq!(state.priority_player, PlayerId(0));

    let pass_result = apply_as_current(&mut state, GameAction::PassPriority).unwrap();
    assert!(matches!(
        pass_result.waiting_for,
        WaitingFor::Priority {
            player: PlayerId(1)
        }
    ));
}

fn setup_esper_sentinel_unless_payment(pay_mana: bool) -> GameState {
    let mut state = new_game(42);
    state.turn_number = 2;
    state.phase = Phase::PreCombatMain;
    state.active_player = PlayerId(1);
    state.priority_player = PlayerId(1);
    state.waiting_for = WaitingFor::Priority {
        player: PlayerId(1),
    };

    create_object(
        &mut state,
        CardId(500),
        PlayerId(0),
        "Drawn Card".to_string(),
        Zone::Library,
    );

    let esper = create_object(
        &mut state,
        CardId(501),
        PlayerId(0),
        "Esper Sentinel".to_string(),
        Zone::Battlefield,
    );
    {
        let obj = state.objects.get_mut(&esper).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.power = Some(1);
        obj.toughness = Some(1);
        let mut trigger = TriggerDefinition::new(TriggerMode::SpellCast)
            .execute(AbilityDefinition::new(
                AbilityKind::Database,
                Effect::Draw {
                    count: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::Controller,
                },
            ))
            .constraint(TriggerConstraint::NthSpellThisTurn {
                n: 1,
                filter: Some(TargetFilter::Typed(
                    TypedFilter::default()
                        .with_type(TypeFilter::Non(Box::new(TypeFilter::Creature))),
                )),
            });
        trigger.unless_pay = Some(UnlessPayModifier {
            cost: AbilityCost::ManaDynamic {
                quantity: QuantityExpr::Ref {
                    qty: QuantityRef::Power {
                        scope: ObjectScope::Source,
                    },
                },
            },
            payer: TargetFilter::TriggeringPlayer,
        });
        obj.trigger_definitions.push(trigger);
    }

    let spell = create_object(
        &mut state,
        CardId(502),
        PlayerId(1),
        "Opponent Noncreature Spell".to_string(),
        Zone::Hand,
    );
    state
        .objects
        .get_mut(&spell)
        .unwrap()
        .card_types
        .core_types
        .push(CoreType::Instant);

    if pay_mana {
        state
            .players
            .iter_mut()
            .find(|player| player.id == PlayerId(1))
            .unwrap()
            .mana_pool
            .add(ManaUnit {
                color: ManaType::Colorless,
                source_id: ObjectId(0),
                pip_id: crate::types::mana::ManaPipId(0),
                supertype: None,
                source_could_produce_two_or_more_colors: false,
                restrictions: Vec::new(),
                grants: vec![],
                expiry: None,
            });
    }

    apply_as_current(
        &mut state,
        GameAction::CastSpell {
            object_id: spell,
            card_id: CardId(502),
            targets: Vec::new(),

            payment_mode: crate::types::game_state::CastPaymentMode::Auto,
        },
    )
    .unwrap();

    let mut events = Vec::new();
    crate::game::stack::resolve_top(&mut state, &mut events);

    assert!(matches!(
        state.waiting_for,
        WaitingFor::UnlessPayment {
            player: PlayerId(1),
            cost: AbilityCost::Mana { ref cost },
            ..
        } if *cost == ManaCost::generic(1)
    ));

    state
}

#[test]
fn esper_sentinel_draws_when_triggering_player_declines_x_payment() {
    let mut state = setup_esper_sentinel_unless_payment(false);

    let result = apply_as_current(&mut state, GameAction::PayUnlessCost { pay: false }).unwrap();

    assert!(matches!(result.waiting_for, WaitingFor::Priority { .. }));
    assert_eq!(state.players[0].hand.len(), 1);
    assert_eq!(state.players[1].hand.len(), 0);
}

#[test]
fn esper_sentinel_does_not_draw_when_triggering_player_pays_x() {
    let mut state = setup_esper_sentinel_unless_payment(true);

    let result = apply_as_current(&mut state, GameAction::PayUnlessCost { pay: true }).unwrap();

    assert!(matches!(result.waiting_for, WaitingFor::Priority { .. }));
    assert_eq!(state.players[0].hand.len(), 0);
    assert_eq!(state.players[1].hand.len(), 0);
}

#[test]
fn issue_1981_echo_decline_sacrifice_fires_dies_trigger() {
    let mut state = new_game(42);
    state.turn_number = 2;
    state.phase = Phase::Untap;
    state.active_player = PlayerId(0);
    state.priority_player = PlayerId(0);
    state.waiting_for = WaitingFor::Priority {
        player: PlayerId(0),
    };

    let mogg = create_object(
        &mut state,
        CardId(1981),
        PlayerId(0),
        "Mogg War Marshal".to_string(),
        Zone::Battlefield,
    );

    let oracle = "Echo {1}{R} (At the beginning of your upkeep, if this came under your control since the beginning of your last upkeep, sacrifice it unless you pay its echo cost.)\n\
When this creature enters or dies, create a 1/1 red Goblin creature token.";
    let parsed = parse_oracle_text(
        oracle,
        "Mogg War Marshal",
        &[],
        &["Creature".to_string()],
        &["Goblin".to_string(), "Warrior".to_string()],
    );
    assert!(
        parsed
            .extracted_keywords
            .iter()
            .any(|kw| matches!(kw, Keyword::Echo(_))),
        "Mogg's echo keyword must parse before synthesis"
    );

    let mut face = CardFace {
        keywords: parsed.extracted_keywords.clone(),
        triggers: parsed.triggers.clone(),
        ..CardFace::default()
    };
    crate::database::synthesis::synthesize_echo(&mut face);

    {
        let obj = state.objects.get_mut(&mogg).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.card_types.subtypes.push("Goblin".to_string());
        obj.card_types.subtypes.push("Warrior".to_string());
        obj.power = Some(1);
        obj.toughness = Some(1);
        obj.base_power = Some(1);
        obj.base_toughness = Some(1);
        obj.keywords = face.keywords.clone();
        obj.base_keywords = obj.keywords.clone();
        for trigger in face.triggers.clone() {
            obj.trigger_definitions.push(trigger);
        }
        obj.base_trigger_definitions = Arc::new(
            obj.trigger_definitions
                .iter_all()
                .map(|entry| entry.definition.clone())
                .collect(),
        );
        // CR 702.30a: the next controller-upkeep echo payment is due.
        obj.echo_due = true;
    }

    let mut events = Vec::new();
    crate::game::turns::auto_advance(&mut state, &mut events);
    assert_eq!(state.phase, Phase::Upkeep);
    assert!(
        !state.stack.is_empty(),
        "echo trigger must be on the stack at the beginning of upkeep"
    );

    events.clear();
    crate::game::stack::resolve_top(&mut state, &mut events);
    assert!(matches!(
        state.waiting_for,
        WaitingFor::UnlessPayment {
            player: PlayerId(0),
            ..
        }
    ));

    apply_as_current(&mut state, GameAction::PayUnlessCost { pay: false }).unwrap();

    assert_eq!(
        state.objects[&mogg].zone,
        Zone::Graveyard,
        "declining echo must sacrifice Mogg War Marshal"
    );
    assert!(
        !state.stack.is_empty(),
        "Mogg War Marshal's dies trigger must be put on the stack after the echo sacrifice"
    );

    apply_as_current(&mut state, GameAction::PassPriority).unwrap();
    apply_as_current(&mut state, GameAction::PassPriority).unwrap();

    let goblin_tokens = state
        .battlefield
        .iter()
        .filter_map(|id| state.objects.get(id))
        .filter(|obj| obj.is_token && obj.name == "Goblin")
        .count();
    assert_eq!(
        goblin_tokens, 1,
        "the dies trigger should resolve to one 1/1 red Goblin token"
    );
}

#[test]
fn rakdos_headliner_non_mana_echo_reaches_discard_payment() {
    // CR 702.30a: "Echo—Discard a card." is a *non-mana* echo cost. On
    // origin/main the parser drops the Echo keyword entirely for the em-dash
    // (non-mana) form, so synthesis never installs the upkeep trigger and the
    // permanent is never on the hook for a discard. This drives the real
    // pipeline (parse -> synthesize_echo -> battlefield with echo due ->
    // controller upkeep) and asserts the engine reaches the discard payment.
    let mut state = new_game(42);
    state.turn_number = 2;
    state.phase = Phase::Untap;
    state.active_player = PlayerId(0);
    state.priority_player = PlayerId(0);
    state.waiting_for = WaitingFor::Priority {
        player: PlayerId(0),
    };

    let headliner = create_object(
        &mut state,
        CardId(1982),
        PlayerId(0),
        "Rakdos Headliner".to_string(),
        Zone::Battlefield,
    );

    // A spare card in P0's hand so the discard cost has an eligible target
    // (the engine surfaces the choice rather than auto-failing the payment).
    let _spare = create_object(
        &mut state,
        CardId(1983),
        PlayerId(0),
        "Spare Card".to_string(),
        Zone::Hand,
    );

    let oracle = "Haste\n\
Echo—Discard a card. (At the beginning of your upkeep, if this came under your control since the beginning of your last upkeep, sacrifice it unless you pay its echo cost.)";
    let parsed = parse_oracle_text(
        oracle,
        "Rakdos Headliner",
        &[],
        &["Creature".to_string()],
        &["Devil".to_string()],
    );

    // Discriminating assertion: on origin/main the non-mana echo keyword is
    // dropped, so this `Echo(NonMana(Discard))` is absent.
    assert!(
        parsed.extracted_keywords.iter().any(|kw| matches!(
            kw,
            Keyword::Echo(crate::types::keywords::EchoCost::NonMana(
                AbilityCost::Discard { .. }
            ))
        )),
        "Rakdos Headliner must parse Echo(NonMana(Discard)) — got {:?}",
        parsed.extracted_keywords
    );

    let mut face = CardFace {
        keywords: parsed.extracted_keywords.clone(),
        triggers: parsed.triggers.clone(),
        ..CardFace::default()
    };
    crate::database::synthesis::synthesize_echo(&mut face);

    {
        let obj = state.objects.get_mut(&headliner).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.card_types.subtypes.push("Devil".to_string());
        obj.power = Some(3);
        obj.toughness = Some(1);
        obj.base_power = Some(3);
        obj.base_toughness = Some(1);
        obj.keywords = face.keywords.clone();
        obj.base_keywords = obj.keywords.clone();
        for trigger in face.triggers.clone() {
            obj.trigger_definitions.push(trigger);
        }
        obj.base_trigger_definitions = Arc::new(
            obj.trigger_definitions
                .iter_all()
                .map(|entry| entry.definition.clone())
                .collect(),
        );
        // CR 702.30a: the next controller-upkeep echo payment is due.
        obj.echo_due = true;
    }

    let mut events = Vec::new();
    crate::game::turns::auto_advance(&mut state, &mut events);
    assert_eq!(state.phase, Phase::Upkeep);
    assert!(
        !state.stack.is_empty(),
        "echo trigger must be on the stack at the beginning of upkeep"
    );

    events.clear();
    crate::game::stack::resolve_top(&mut state, &mut events);

    // CR 702.30a: the echo trigger resolves to an unless-payment carrying the
    // *non-mana* discard cost (not mana). On origin/main the Echo keyword is
    // dropped for the em-dash form, so no echo trigger exists and this
    // UnlessPayment-with-Discard never appears — the discriminating proof
    // that the non-mana echo cost flowed through synthesis into the payment
    // pipeline.
    assert!(
        matches!(
            state.waiting_for,
            WaitingFor::UnlessPayment {
                player: PlayerId(0),
                cost: AbilityCost::Discard { .. },
                ..
            }
        ),
        "non-mana echo must surface an UnlessPayment carrying a Discard cost — got {:?}",
        state.waiting_for
    );

    // CR 701.9: choosing to pay routes the discard cost through
    // `handle_unless_payment`, which surfaces the discard-card choice — a
    // discard cost, not a mana payment.
    apply_as_current(&mut state, GameAction::PayUnlessCost { pay: true }).unwrap();
    assert!(
        matches!(
            state.waiting_for,
            WaitingFor::WardDiscardChoice {
                player: PlayerId(0),
                ..
            }
        ),
        "paying the non-mana echo must reach the discard-choice payment — got {:?}",
        state.waiting_for
    );
}

#[test]
fn attack_trigger_resolves_before_combat_damage_and_only_once() {
    let mut state = new_game(42);
    state.turn_number = 5;
    state.phase = Phase::DeclareAttackers;
    state.active_player = PlayerId(0);
    state.priority_player = PlayerId(0);

    let ajani = create_object(
        &mut state,
        CardId(400),
        PlayerId(0),
        "Ajani's Pridemate".to_string(),
        Zone::Battlefield,
    );
    {
        let obj = state.objects.get_mut(&ajani).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.power = Some(2);
        obj.toughness = Some(2);
        obj.base_power = Some(2);
        obj.base_toughness = Some(2);
        obj.color = vec![ManaColor::White];
        obj.base_color = vec![ManaColor::White];
        obj.entered_battlefield_turn = Some(4);
        obj.trigger_definitions.push(
            TriggerDefinition::new(TriggerMode::LifeGained)
                .valid_target(TargetFilter::Controller)
                .execute(AbilityDefinition::new(
                    AbilityKind::Database,
                    Effect::PutCounter {
                        counter_type: crate::types::counter::CounterType::Plus1Plus1,
                        count: QuantityExpr::Fixed { value: 1 },
                        target: TargetFilter::SelfRef,
                    },
                )),
        );
    }

    let linden = create_object(
        &mut state,
        CardId(401),
        PlayerId(0),
        "Linden, the Steadfast Queen".to_string(),
        Zone::Battlefield,
    );
    {
        let obj = state.objects.get_mut(&linden).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.power = Some(3);
        obj.toughness = Some(3);
        obj.base_power = Some(3);
        obj.base_toughness = Some(3);
        obj.color = vec![ManaColor::White];
        obj.base_color = vec![ManaColor::White];
        obj.entered_battlefield_turn = Some(4);
        obj.trigger_definitions.push(
            TriggerDefinition::new(TriggerMode::Attacks)
                .valid_card(TargetFilter::Typed(
                    TypedFilter::creature()
                        .controller(ControllerRef::You)
                        .properties(vec![FilterProp::HasColor {
                            color: crate::types::mana::ManaColor::White,
                        }]),
                ))
                .execute(AbilityDefinition::new(
                    AbilityKind::Database,
                    Effect::GainLife {
                        amount: QuantityExpr::Fixed { value: 1 },
                        player: TargetFilter::Controller,
                    },
                )),
        );
    }

    state.waiting_for = WaitingFor::DeclareAttackers {
        player: PlayerId(0),
        valid_attacker_ids: vec![ajani, linden],
        valid_attack_targets: vec![AttackTarget::Player(PlayerId(1))],
        valid_attack_targets_by_attacker: None,
        attacker_constraints: Default::default(),
    };

    let declare_result = apply_as_current(
        &mut state,
        GameAction::DeclareAttackers {
            attacks: vec![(ajani, AttackTarget::Player(PlayerId(1)))],
            bands: vec![],
        },
    )
    .unwrap();

    assert!(matches!(
        declare_result.waiting_for,
        WaitingFor::Priority {
            player: PlayerId(0)
        }
    ));
    assert_eq!(
        state.stack.len(),
        1,
        "Linden should create exactly one stack entry"
    );
    assert_eq!(state.phase, Phase::DeclareAttackers);

    apply_as_current(&mut state, GameAction::PassPriority).unwrap();
    let linden_resolve = apply_as_current(&mut state, GameAction::PassPriority).unwrap();

    assert!(matches!(
        linden_resolve.waiting_for,
        WaitingFor::Priority {
            player: PlayerId(0)
        }
    ));
    assert_eq!(state.players[0].life, 21, "Linden should gain life once");
    assert_eq!(
        state.stack.len(),
        1,
        "Ajani's Pridemate should trigger from Linden's life gain"
    );
    assert_eq!(state.objects[&ajani].power, Some(2));
    assert_eq!(state.objects[&ajani].toughness, Some(2));

    apply_as_current(&mut state, GameAction::PassPriority).unwrap();
    let pridemate_resolve = apply_as_current(&mut state, GameAction::PassPriority).unwrap();

    assert!(matches!(
        pridemate_resolve.waiting_for,
        WaitingFor::Priority {
            player: PlayerId(0)
        }
    ));
    assert!(state.stack.is_empty());
    assert_eq!(state.objects[&ajani].power, Some(3));
    assert_eq!(state.objects[&ajani].toughness, Some(3));

    // CR 117.1c: Active player gets priority in every step — so from
    // DeclareAttackers we pass through: declare attackers (AP, NAP) →
    // declare blockers (AP, NAP, after auto-submitted empty block) →
    // combat damage resolves → end-of-combat → post-combat main.
    let mut combat_result = None;
    for _ in 0..8 {
        if state.phase == Phase::PostCombatMain {
            break;
        }
        combat_result = Some(apply_as_current(&mut state, GameAction::PassPriority).unwrap());
    }
    let combat_result = combat_result.expect("combat should advance");

    assert!(matches!(
        combat_result.waiting_for,
        WaitingFor::Priority { .. }
    ));
    assert_eq!(state.phase, Phase::PostCombatMain);
    assert_eq!(
        state.players[1].life, 17,
        "Ajani should deal 3 after receiving the pre-damage counter"
    );
    assert_eq!(
        state.players[0].life, 21,
        "No duplicate Linden life gain should occur"
    );
    assert_eq!(state.objects[&ajani].power, Some(3));
    assert_eq!(state.objects[&ajani].toughness, Some(3));
}

/// Regression test: lifelink combat damage with a GainLife replacement effect
/// (Leyline of Hope) must not double-fire "whenever you gain life" triggers.
///
/// Previously, process_combat_damage_triggers processed the LifeChanged event
/// for triggers, then run_post_action_pipeline re-processed the same events,
/// causing triggers like Essence Channeler's to fire twice per life-gain event.
#[test]
fn lifelink_replacement_does_not_double_fire_life_gain_triggers() {
    use crate::types::ability::ReplacementDefinition;
    use crate::types::counter::CounterType;
    use crate::types::replacements::ReplacementEvent;

    let mut state = new_game(42);
    state.turn_number = 5;
    state.phase = Phase::DeclareAttackers;
    state.active_player = PlayerId(0);
    state.priority_player = PlayerId(0);

    // Lifelink attacker (Ruin-Lurker Bat analog): 1/1 flying lifelink
    let bat = create_object(
        &mut state,
        CardId(500),
        PlayerId(0),
        "Ruin-Lurker Bat".to_string(),
        Zone::Battlefield,
    );
    {
        let obj = state.objects.get_mut(&bat).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.power = Some(1);
        obj.toughness = Some(1);
        obj.base_power = Some(1);
        obj.base_toughness = Some(1);
        obj.keywords.push(crate::types::keywords::Keyword::Lifelink);
        obj.base_keywords = obj.keywords.clone();
        obj.entered_battlefield_turn = Some(3);
    }

    // "Whenever you gain life, put a +1/+1 counter on this creature" (Essence Channeler)
    let channeler = create_object(
        &mut state,
        CardId(501),
        PlayerId(0),
        "Essence Channeler".to_string(),
        Zone::Battlefield,
    );
    {
        let obj = state.objects.get_mut(&channeler).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.power = Some(2);
        obj.toughness = Some(1);
        obj.base_power = Some(2);
        obj.base_toughness = Some(1);
        obj.entered_battlefield_turn = Some(3);
        obj.trigger_definitions.push(
            TriggerDefinition::new(TriggerMode::LifeGained)
                .valid_target(TargetFilter::Controller)
                .execute(AbilityDefinition::new(
                    AbilityKind::Database,
                    Effect::PutCounter {
                        counter_type: crate::types::counter::CounterType::Plus1Plus1,
                        count: QuantityExpr::Fixed { value: 1 },
                        target: TargetFilter::SelfRef,
                    },
                )),
        );
        obj.base_trigger_definitions = Arc::new(
            obj.trigger_definitions
                .iter_all()
                .map(|entry| entry.definition.clone())
                .collect(),
        );
    }

    // Leyline of Hope analog: "If you would gain life, gain that much + 1 instead"
    let leyline = create_object(
        &mut state,
        CardId(502),
        PlayerId(0),
        "Leyline of Hope".to_string(),
        Zone::Battlefield,
    );
    {
        let obj = state.objects.get_mut(&leyline).unwrap();
        obj.card_types.core_types.push(CoreType::Enchantment);
        // Leyline of Hope: "If you would gain life, you gain that much
        // life plus 1 instead." Parser emits the replaced amount as
        // `Offset { inner: EventContextAmount, offset: 1 }`, not a delta.
        obj.replacement_definitions.push(
            ReplacementDefinition::new(ReplacementEvent::GainLife).execute(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::GainLife {
                    amount: QuantityExpr::Offset {
                        inner: Box::new(QuantityExpr::Ref {
                            qty: crate::types::ability::QuantityRef::EventContextAmount,
                        }),
                        offset: 1,
                    },
                    player: TargetFilter::Controller,
                },
            )),
        );
        obj.base_replacement_definitions =
            Arc::new(obj.replacement_definitions.iter_all().cloned().collect());
    }

    // Declare bat as attacker
    state.waiting_for = WaitingFor::DeclareAttackers {
        player: PlayerId(0),
        valid_attacker_ids: vec![bat],
        valid_attack_targets: vec![AttackTarget::Player(PlayerId(1))],
        valid_attack_targets_by_attacker: None,
        attacker_constraints: Default::default(),
    };

    apply_as_current(
        &mut state,
        GameAction::DeclareAttackers {
            attacks: vec![(bat, AttackTarget::Player(PlayerId(1)))],
            bands: vec![],
        },
    )
    .unwrap();

    // Skip to combat damage: P0 pass, P1 pass (declare blockers — no blockers),
    // P0 pass, P1 pass (combat damage resolves).
    apply_as_current(&mut state, GameAction::PassPriority).unwrap();
    apply_as_current(&mut state, GameAction::PassPriority).unwrap();
    // Now at declare blockers — P1 declares no blockers
    if matches!(state.waiting_for, WaitingFor::DeclareBlockers { .. }) {
        apply_as_current(
            &mut state,
            GameAction::DeclareBlockers {
                assignments: vec![],
            },
        )
        .unwrap();
    }
    // Pass priority through to combat damage
    while state.phase != Phase::PostCombatMain
        && !matches!(state.waiting_for, WaitingFor::GameOver { .. })
    {
        if matches!(state.waiting_for, WaitingFor::Priority { .. }) {
            apply_as_current(&mut state, GameAction::PassPriority).unwrap();
        } else {
            break;
        }
    }

    // Bat dealt 1 damage → lifelink gain 1 → Leyline replaces to 2.
    // Player 0 should have gained exactly 2 life (20 → 22).
    assert_eq!(
        state.players[0].life, 22,
        "Lifelink + Leyline should gain exactly 2 life"
    );

    // Essence Channeler should have exactly 1 +1/+1 counter, not 2.
    // The bug was that the LifeChanged event was processed for triggers twice,
    // once in process_combat_damage_triggers and again in run_post_action_pipeline.
    let counters = state.objects[&channeler]
        .counters
        .get(&CounterType::Plus1Plus1)
        .copied()
        .unwrap_or(0);
    assert_eq!(
        counters, 1,
        "Essence Channeler should trigger exactly once per life-gain event, got {} counters",
        counters
    );
}

#[test]
fn card_name_choice_validates_against_all_card_names() {
    let mut state = GameState::new_two_player(42);
    state.all_card_names = vec!["Lightning Bolt".to_string(), "Counterspell".to_string()].into();
    state.waiting_for = WaitingFor::NamedChoice {
        player: PlayerId(0),
        choice_type: crate::types::ability::ChoiceType::CardName,
        options: Vec::new(),
        source: None,
        persist_player: None,
    };

    // Valid card name succeeds
    let result = apply_as_current(
        &mut state,
        GameAction::ChooseOption {
            choice: "Lightning Bolt".to_string(),
        },
    );
    assert!(result.is_ok());

    // Reset state for invalid test
    state.waiting_for = WaitingFor::NamedChoice {
        player: PlayerId(0),
        choice_type: crate::types::ability::ChoiceType::CardName,
        options: Vec::new(),
        source: None,
        persist_player: None,
    };

    // Invalid card name fails
    let result = apply_as_current(
        &mut state,
        GameAction::ChooseOption {
            choice: "Not A Real Card".to_string(),
        },
    );
    assert!(result.is_err());
}

#[test]
fn card_name_choice_is_case_insensitive() {
    let mut state = GameState::new_two_player(42);
    state.all_card_names = vec!["Lightning Bolt".to_string()].into();
    state.waiting_for = WaitingFor::NamedChoice {
        player: PlayerId(0),
        choice_type: crate::types::ability::ChoiceType::CardName,
        options: Vec::new(),
        source: None,
        persist_player: None,
    };

    let result = apply_as_current(
        &mut state,
        GameAction::ChooseOption {
            choice: "lightning bolt".to_string(),
        },
    );
    assert!(result.is_ok());
}

#[test]
fn optional_effect_choice_accept_preserves_nested_effect_zone_choice_continuation() {
    let mut state = setup_game_at_main_phase();
    let source_id = ObjectId(100);
    create_object(
        &mut state,
        CardId(1),
        PlayerId(0),
        "Perm A".to_string(),
        Zone::Battlefield,
    );
    create_object(
        &mut state,
        CardId(2),
        PlayerId(0),
        "Perm B".to_string(),
        Zone::Battlefield,
    );

    let mut ability = ResolvedAbility::new(
        Effect::Sacrifice {
            target: TargetFilter::Any,
            count: QuantityExpr::Fixed { value: 1 },
            min_count: 0,
        },
        vec![],
        source_id,
        PlayerId(0),
    );
    let mut draw = draw_that_many(source_id, PlayerId(0));
    draw.condition = Some(AbilityCondition::effect_performed());
    ability.sub_ability = Some(Box::new(draw));

    state.pending_optional_effect = Some(Box::new(ability));
    state.waiting_for = WaitingFor::OptionalEffectChoice {
        player: PlayerId(0),
        source_id,
        description: None,
        may_trigger_key: None,
    };

    let result = apply_as_current(
        &mut state,
        GameAction::DecideOptionalEffect { accept: true },
    )
    .unwrap();

    assert!(matches!(
        result.waiting_for,
        WaitingFor::EffectZoneChoice {
            player: PlayerId(0),
            ..
        }
    ));
    assert!(state.pending_continuation.is_some());
}

#[test]
fn opponent_may_choice_accept_preserves_nested_effect_zone_choice_continuation() {
    let mut state = setup_game_at_main_phase();
    let source_id = ObjectId(100);
    create_object(
        &mut state,
        CardId(1),
        PlayerId(1),
        "Hand A".to_string(),
        Zone::Hand,
    );
    create_object(
        &mut state,
        CardId(2),
        PlayerId(1),
        "Hand B".to_string(),
        Zone::Hand,
    );

    let mut ability = hand_to_battlefield_choice_ability(source_id, PlayerId(1));
    ability.sub_ability = Some(Box::new(draw_that_many(source_id, PlayerId(1))));

    state.pending_optional_effect = Some(Box::new(ability));
    state.waiting_for = WaitingFor::OpponentMayChoice {
        player: PlayerId(1),
        remaining: vec![],
        source_id,
        description: None,
    };

    let result = apply_as_current(
        &mut state,
        GameAction::DecideOptionalEffect { accept: true },
    )
    .unwrap();

    assert!(matches!(
        result.waiting_for,
        WaitingFor::EffectZoneChoice {
            player: PlayerId(1),
            ..
        }
    ));
    assert!(state.pending_continuation.is_some());
}

#[test]
fn unless_payment_decline_preserves_nested_effect_zone_choice_continuation() {
    let mut state = setup_game_at_main_phase();
    let source_id = ObjectId(100);
    create_object(
        &mut state,
        CardId(1),
        PlayerId(0),
        "Hand A".to_string(),
        Zone::Hand,
    );
    create_object(
        &mut state,
        CardId(2),
        PlayerId(0),
        "Hand B".to_string(),
        Zone::Hand,
    );

    let mut ability = hand_to_battlefield_choice_ability(source_id, PlayerId(0));
    ability.sub_ability = Some(Box::new(draw_that_many(source_id, PlayerId(0))));

    state.waiting_for = WaitingFor::UnlessPayment {
        player: PlayerId(0),
        cost: AbilityCost::PayLife {
            amount: QuantityExpr::Fixed { value: 2 },
        },
        pending_effect: Box::new(ability),
        trigger_event: None,
        effect_description: None,
        remaining: Vec::new(),
    };

    let result = apply_as_current(&mut state, GameAction::PayUnlessCost { pay: false }).unwrap();

    assert!(matches!(
        result.waiting_for,
        WaitingFor::EffectZoneChoice {
            player: PlayerId(0),
            ..
        }
    ));
    assert!(state.pending_continuation.is_some());
}

/// CR 610.3 + #783: When a permanent that exiled something "until it
/// leaves the battlefield" (Static Prison) sacrifices itself through a
/// "sacrifice unless you pay {E}" trigger, the exiled permanent must
/// return. The unless-payment decline path resolves the sacrifice but
/// historically skipped the post-action pipeline, so the exile return
/// never fired.
#[test]
fn static_prison_unless_pay_sacrifice_returns_exiled_permanent() {
    use crate::types::game_state::{ExileLink, ExileLinkKind};

    let mut state = setup_game_at_main_phase();

    let prison = create_object(
        &mut state,
        CardId(1),
        PlayerId(0),
        "Static Prison".to_string(),
        Zone::Battlefield,
    );

    // The exiled victim already sits in exile, linked to Static Prison.
    let victim = create_object(
        &mut state,
        CardId(2),
        PlayerId(1),
        "Exiled Permanent".to_string(),
        Zone::Exile,
    );
    state
        .objects
        .get_mut(&victim)
        .unwrap()
        .card_types
        .core_types
        .push(CoreType::Creature);
    state.exile_links.push(ExileLink {
        exiled_id: victim,
        source_id: prison,
        kind: ExileLinkKind::UntilSourceLeaves {
            return_zone: Zone::Battlefield,
        },
    });

    // The "sacrifice this enchantment unless you pay {E}" trigger has
    // resolved into an UnlessPayment prompt. P0 has no energy to pay.
    let sacrifice = ResolvedAbility::new(
        Effect::Sacrifice {
            target: TargetFilter::SelfRef,
            count: QuantityExpr::Fixed { value: 1 },
            min_count: 0,
        },
        vec![],
        prison,
        PlayerId(0),
    );
    state.waiting_for = WaitingFor::UnlessPayment {
        player: PlayerId(0),
        cost: AbilityCost::PayEnergy {
            amount: QuantityExpr::Fixed { value: 1 },
        },
        pending_effect: Box::new(sacrifice),
        trigger_event: None,
        effect_description: None,
        remaining: Vec::new(),
    };

    apply_as_current(&mut state, GameAction::PayUnlessCost { pay: false }).unwrap();

    assert!(
        !state.battlefield.contains(&prison),
        "Static Prison should be sacrificed"
    );
    assert!(
        state.battlefield.contains(&victim),
        "exiled permanent must return when Static Prison sacrifices itself"
    );
    assert!(
        !state.exile.contains(&victim),
        "exiled permanent must no longer be in exile"
    );
}

/// CR 118.12 + CR 118.12a: "[Effect] unless [player] pays [cost]. If they do,
/// [alternative]." When the unless cost is paid, the primary effect is
/// suppressed AND the IfAPlayerDoes sub_ability runs as the alternative
/// outcome. Cards: Rhystic Lightning, Don't Make a Sound, Divert Disaster,
/// Assimilate Essence.
#[test]
fn unless_pay_success_runs_if_a_player_does_sub_ability() {
    let mut state = setup_game_at_main_phase();
    let source_id = create_object(
        &mut state,
        CardId(910),
        PlayerId(0),
        "Rhystic Lightning Stand-In".to_string(),
        Zone::Battlefield,
    );

    // Primary effect: gain 4 life. Alternative: gain 2 life.
    // Using GainLife rather than DealDamage so the test stays self-contained
    // (no target wiring required) — the runtime branching being verified is
    // sub_ability resolution, not damage routing.
    let mut primary = ResolvedAbility::new(
        Effect::GainLife {
            amount: QuantityExpr::Fixed { value: 4 },
            player: TargetFilter::Controller,
        },
        vec![],
        source_id,
        PlayerId(0),
    );
    let mut alternative = ResolvedAbility::new(
        Effect::GainLife {
            amount: QuantityExpr::Fixed { value: 2 },
            player: TargetFilter::Controller,
        },
        vec![],
        source_id,
        PlayerId(0),
    );
    alternative.condition = Some(AbilityCondition::effect_performed());
    primary.sub_ability = Some(Box::new(alternative));

    // Player 1 (the unless payer) starts with 20 life and 2 energy to pay.
    state.players[1].energy = 2;
    state.waiting_for = WaitingFor::UnlessPayment {
        player: PlayerId(1),
        cost: AbilityCost::PayEnergy {
            amount: QuantityExpr::Fixed { value: 2 },
        },
        pending_effect: Box::new(primary),
        trigger_event: None,
        effect_description: None,
        remaining: Vec::new(),
    };

    let starting_life = state.players[0].life;
    let result = apply_as_current(&mut state, GameAction::PayUnlessCost { pay: true }).unwrap();

    assert!(matches!(result.waiting_for, WaitingFor::Priority { .. }));
    // Cost was deducted from the unless payer.
    assert_eq!(state.players[1].energy, 0);
    // Primary suppressed (no +4 life), alternative ran (+2 life from sub_ability).
    assert_eq!(state.players[0].life, starting_life + 2);
}

/// CR 603.2 + CR 118.12a: the paid IfAPlayerDoes branch resolves on the
/// unless-payment resume path, so events produced by that branch must be
/// scanned for normal triggers before priority resumes.
#[test]
fn unless_pay_success_sub_ability_fires_triggers_from_events() {
    let mut state = setup_game_at_main_phase();
    let source_id = create_object(
        &mut state,
        CardId(914),
        PlayerId(0),
        "Divert Disaster Stand-In".to_string(),
        Zone::Battlefield,
    );
    let doomed = create_object(
        &mut state,
        CardId(915),
        PlayerId(0),
        "Doomed Witness Stand-In".to_string(),
        Zone::Battlefield,
    );
    {
        let obj = state.objects.get_mut(&doomed).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.trigger_definitions.push(
            TriggerDefinition::new(TriggerMode::ChangesZone)
                .valid_card(TargetFilter::SelfRef)
                .origin(Zone::Battlefield)
                .destination(Zone::Graveyard)
                .trigger_zones(vec![Zone::Battlefield])
                .execute(AbilityDefinition::new(
                    AbilityKind::Database,
                    Effect::GainLife {
                        amount: QuantityExpr::Fixed { value: 3 },
                        player: TargetFilter::Controller,
                    },
                )),
        );
        obj.base_trigger_definitions = Arc::new(
            obj.trigger_definitions
                .iter_all()
                .map(|entry| entry.definition.clone())
                .collect(),
        );
    }

    let mut primary = ResolvedAbility::new(
        Effect::GainLife {
            amount: QuantityExpr::Fixed { value: 4 },
            player: TargetFilter::Controller,
        },
        vec![],
        source_id,
        PlayerId(0),
    );
    let mut alternative = ResolvedAbility::new(
        Effect::Sacrifice {
            target: TargetFilter::Any,
            count: QuantityExpr::Fixed { value: 1 },
            min_count: 0,
        },
        vec![TargetRef::Object(doomed)],
        source_id,
        PlayerId(0),
    );
    alternative.condition = Some(AbilityCondition::effect_performed());
    primary.sub_ability = Some(Box::new(alternative));

    state.players[1].energy = 2;
    state.waiting_for = WaitingFor::UnlessPayment {
        player: PlayerId(1),
        cost: AbilityCost::PayEnergy {
            amount: QuantityExpr::Fixed { value: 2 },
        },
        pending_effect: Box::new(primary),
        trigger_event: None,
        effect_description: None,
        remaining: Vec::new(),
    };

    let starting_life = state.players[0].life;
    let result = apply_as_current(&mut state, GameAction::PayUnlessCost { pay: true }).unwrap();

    assert!(matches!(result.waiting_for, WaitingFor::Priority { .. }));
    assert_eq!(state.objects[&doomed].zone, Zone::Graveyard);
    assert!(
        !state.stack.is_empty(),
        "the paid IfAPlayerDoes sacrifice must put the dies trigger on the stack"
    );

    apply_as_current(&mut state, GameAction::PassPriority).unwrap();
    apply_as_current(&mut state, GameAction::PassPriority).unwrap();

    assert_eq!(
        state.players[0].life,
        starting_life + 3,
        "the dies trigger from the paid sub-ability should resolve"
    );
}

/// CR 603.3b + CR 701.22a: if an unless-payment branch pauses on a
/// resolution choice, triggers produced by that branch wait until the choice
/// finishes instead of clobbering the choice prompt.
#[test]
fn unless_pay_resolution_choice_defers_branch_triggers() {
    let mut state = setup_game_at_main_phase();
    let source_id = create_object(
        &mut state,
        CardId(916),
        PlayerId(0),
        "Unless Scry Stand-In".to_string(),
        Zone::Battlefield,
    );
    for (card_id, name, effect) in [
        (
            CardId(917),
            "Scry Watcher Draw",
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
        ),
        (
            CardId(918),
            "Scry Watcher Life",
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 1 },
                player: TargetFilter::Controller,
            },
        ),
    ] {
        let watcher = create_object(
            &mut state,
            card_id,
            PlayerId(0),
            name.to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&watcher).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.trigger_definitions.push(
            TriggerDefinition::new(TriggerMode::Scry)
                .execute(AbilityDefinition::new(AbilityKind::Database, effect)),
        );
        obj.base_trigger_definitions = Arc::new(
            obj.trigger_definitions
                .iter_all()
                .map(|entry| entry.definition.clone())
                .collect(),
        );
    }
    for (card_id, name) in [
        (CardId(919), "Library One"),
        (CardId(920), "Library Two"),
        (CardId(921), "Library Three"),
    ] {
        create_object(
            &mut state,
            card_id,
            PlayerId(0),
            name.to_string(),
            Zone::Library,
        );
    }

    state.waiting_for = WaitingFor::UnlessPayment {
        player: PlayerId(1),
        cost: AbilityCost::PayEnergy {
            amount: QuantityExpr::Fixed { value: 2 },
        },
        pending_effect: Box::new(ResolvedAbility::new(
            Effect::Scry {
                count: QuantityExpr::Fixed { value: 2 },
                target: TargetFilter::Controller,
            },
            vec![],
            source_id,
            PlayerId(0),
        )),
        trigger_event: None,
        effect_description: None,
        remaining: Vec::new(),
    };

    let result = apply_as_current(&mut state, GameAction::PayUnlessCost { pay: false }).unwrap();
    let WaitingFor::ScryChoice { player, cards } = result.waiting_for.clone() else {
        panic!(
            "unless branch must preserve ScryChoice before watcher triggers, got {:?}",
            result.waiting_for
        );
    };
    assert_eq!(player, PlayerId(0));
    assert_eq!(cards.len(), 2);
    assert_eq!(
        state.deferred_triggers.len(),
        2,
        "the two scry watcher triggers should be parked until ScryChoice resolves"
    );

    let hand_after_scry_prompt = state.players[0].hand.len();
    let life_after_scry_prompt = state.players[0].life;
    apply_as_current(&mut state, GameAction::SelectCards { cards }).unwrap();
    for _ in 0..8 {
        if matches!(state.waiting_for, WaitingFor::OrderTriggers { .. }) {
            crate::game::triggers::drain_order_triggers_with_identity(&mut state);
        }
        if state.stack.is_empty() && matches!(state.waiting_for, WaitingFor::Priority { .. }) {
            break;
        }
        apply_as_current(&mut state, GameAction::PassPriority).unwrap();
    }

    assert_eq!(state.players[0].hand.len(), hand_after_scry_prompt + 1);
    assert_eq!(state.players[0].life, life_after_scry_prompt + 1);
}

/// CR 118.12: When the unless cost is declined, the primary effect runs
/// and the IfAPlayerDoes sub_ability does NOT run (its condition reads
/// `optional_effect_performed` which stays false on the decline path).
#[test]
fn unless_pay_decline_runs_primary_not_if_a_player_does_sub() {
    let mut state = setup_game_at_main_phase();
    let source_id = create_object(
        &mut state,
        CardId(911),
        PlayerId(0),
        "Rhystic Lightning Stand-In".to_string(),
        Zone::Battlefield,
    );

    let mut primary = ResolvedAbility::new(
        Effect::GainLife {
            amount: QuantityExpr::Fixed { value: 4 },
            player: TargetFilter::Controller,
        },
        vec![],
        source_id,
        PlayerId(0),
    );
    let mut alternative = ResolvedAbility::new(
        Effect::GainLife {
            amount: QuantityExpr::Fixed { value: 2 },
            player: TargetFilter::Controller,
        },
        vec![],
        source_id,
        PlayerId(0),
    );
    alternative.condition = Some(AbilityCondition::effect_performed());
    primary.sub_ability = Some(Box::new(alternative));

    state.waiting_for = WaitingFor::UnlessPayment {
        player: PlayerId(1),
        cost: AbilityCost::PayEnergy {
            amount: QuantityExpr::Fixed { value: 2 },
        },
        pending_effect: Box::new(primary),
        trigger_event: None,
        effect_description: None,
        remaining: Vec::new(),
    };

    let starting_life = state.players[0].life;
    let result = apply_as_current(&mut state, GameAction::PayUnlessCost { pay: false }).unwrap();

    assert!(matches!(result.waiting_for, WaitingFor::Priority { .. }));
    // Primary ran (+4 life), alternative did NOT (no extra +2 life).
    assert_eq!(state.players[0].life, starting_life + 4);
}

/// CR 118.12: An unless_pay effect with NO sub_ability still resolves
/// cleanly when the cost is paid (primary suppressed, no spurious chain
/// resolution).
#[test]
fn unless_pay_success_with_no_sub_ability_is_inert() {
    let mut state = setup_game_at_main_phase();
    let source_id = create_object(
        &mut state,
        CardId(912),
        PlayerId(0),
        "Plain Unless Effect".to_string(),
        Zone::Battlefield,
    );

    let primary = ResolvedAbility::new(
        Effect::GainLife {
            amount: QuantityExpr::Fixed { value: 4 },
            player: TargetFilter::Controller,
        },
        vec![],
        source_id,
        PlayerId(0),
    );

    state.players[1].energy = 2;
    state.waiting_for = WaitingFor::UnlessPayment {
        player: PlayerId(1),
        cost: AbilityCost::PayEnergy {
            amount: QuantityExpr::Fixed { value: 2 },
        },
        pending_effect: Box::new(primary),
        trigger_event: None,
        effect_description: None,
        remaining: Vec::new(),
    };

    let starting_life = state.players[0].life;
    let result = apply_as_current(&mut state, GameAction::PayUnlessCost { pay: true }).unwrap();

    assert!(matches!(result.waiting_for, WaitingFor::Priority { .. }));
    assert_eq!(state.players[1].energy, 0);
    // Primary suppressed; no sub_ability to run.
    assert_eq!(state.players[0].life, starting_life);
}

/// Abandon Attachments #81 parallel: a stale `cost_payment_failed_flag`
/// from a previous resolution must NOT block the IfAPlayerDoes sub_ability
/// when the unless cost is paid. The success path clears the flag the
/// same way `handle_optional_effect_choice` does for accepts.
#[test]
fn unless_pay_success_clears_stale_cost_payment_failed_flag() {
    let mut state = setup_game_at_main_phase();
    // Simulate a previous resolution that left the flag set.
    state.cost_payment_failed_flag = true;

    let source_id = create_object(
        &mut state,
        CardId(913),
        PlayerId(0),
        "Stale Flag Source".to_string(),
        Zone::Battlefield,
    );

    let mut primary = ResolvedAbility::new(
        Effect::GainLife {
            amount: QuantityExpr::Fixed { value: 4 },
            player: TargetFilter::Controller,
        },
        vec![],
        source_id,
        PlayerId(0),
    );
    let mut alternative = ResolvedAbility::new(
        Effect::GainLife {
            amount: QuantityExpr::Fixed { value: 2 },
            player: TargetFilter::Controller,
        },
        vec![],
        source_id,
        PlayerId(0),
    );
    alternative.condition = Some(AbilityCondition::effect_performed());
    primary.sub_ability = Some(Box::new(alternative));

    state.players[1].energy = 2;
    state.waiting_for = WaitingFor::UnlessPayment {
        player: PlayerId(1),
        cost: AbilityCost::PayEnergy {
            amount: QuantityExpr::Fixed { value: 2 },
        },
        pending_effect: Box::new(primary),
        trigger_event: None,
        effect_description: None,
        remaining: Vec::new(),
    };

    let starting_life = state.players[0].life;
    let _ = apply_as_current(&mut state, GameAction::PayUnlessCost { pay: true }).unwrap();

    // Alternative ran (+2 life), so the stale flag was correctly cleared.
    assert_eq!(state.players[0].life, starting_life + 2);
    assert!(
        !state.cost_payment_failed_flag,
        "cost_payment_failed_flag should be cleared by the success path"
    );
}

#[test]
fn unless_energy_payment_deducts_energy_and_skips_effect() {
    let mut state = setup_game_at_main_phase();
    let source_id = create_object(
        &mut state,
        CardId(901),
        PlayerId(0),
        "Energy Source".to_string(),
        Zone::Battlefield,
    );
    state.players[0].energy = 2;
    state.waiting_for = WaitingFor::UnlessPayment {
        player: PlayerId(0),
        cost: AbilityCost::PayEnergy {
            amount: QuantityExpr::Fixed { value: 2 },
        },
        pending_effect: Box::new(ResolvedAbility::new(
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 1 },
                player: crate::types::ability::TargetFilter::Controller,
            },
            vec![],
            source_id,
            PlayerId(0),
        )),
        trigger_event: None,
        effect_description: None,
        remaining: Vec::new(),
    };

    let result = apply_as_current(&mut state, GameAction::PayUnlessCost { pay: true }).unwrap();

    assert!(matches!(result.waiting_for, WaitingFor::Priority { .. }));
    assert_eq!(state.players[0].energy, 0);
    assert_eq!(state.players[0].life, 20);
}

#[test]
fn unless_discard_payment_filters_eligible_hand_cards() {
    let mut state = setup_game_at_main_phase();
    let source_id = create_object(
        &mut state,
        CardId(900),
        PlayerId(0),
        "Source".to_string(),
        Zone::Battlefield,
    );
    let land_id = create_object(
        &mut state,
        CardId(1),
        PlayerId(0),
        "Land Card".to_string(),
        Zone::Hand,
    );
    let creature_id = create_object(
        &mut state,
        CardId(2),
        PlayerId(0),
        "Creature Card".to_string(),
        Zone::Hand,
    );
    state
        .objects
        .get_mut(&land_id)
        .expect("land object")
        .card_types
        .core_types = vec![CoreType::Land];
    state
        .objects
        .get_mut(&creature_id)
        .expect("creature object")
        .card_types
        .core_types = vec![CoreType::Creature];

    state.waiting_for = WaitingFor::UnlessPayment {
        player: PlayerId(0),
        cost: AbilityCost::Discard {
            count: QuantityExpr::Fixed { value: 1 },
            filter: Some(TargetFilter::Typed(TypedFilter {
                type_filters: vec![TypeFilter::Land],
                controller: None,
                properties: vec![],
            })),
            selection: crate::types::ability::CardSelectionMode::Chosen,
            self_scope: crate::types::ability::DiscardSelfScope::FromHand,
        },
        pending_effect: Box::new(draw_that_many(source_id, PlayerId(0))),
        trigger_event: None,
        effect_description: None,
        remaining: Vec::new(),
    };

    let result = apply_as_current(&mut state, GameAction::PayUnlessCost { pay: true }).unwrap();

    match result.waiting_for {
        WaitingFor::WardDiscardChoice { cards, .. } => assert_eq!(cards, vec![land_id]),
        other => panic!("expected filtered WardDiscardChoice, got {other:?}"),
    }
}

#[test]
fn multi_target_selection_preserves_nested_effect_zone_choice_continuation() {
    let mut state = setup_game_at_main_phase();
    let source_id = ObjectId(100);
    let target_id = create_object(
        &mut state,
        CardId(1),
        PlayerId(0),
        "Tap Target".to_string(),
        Zone::Battlefield,
    );
    create_object(
        &mut state,
        CardId(2),
        PlayerId(0),
        "Hand A".to_string(),
        Zone::Hand,
    );
    create_object(
        &mut state,
        CardId(3),
        PlayerId(0),
        "Hand B".to_string(),
        Zone::Hand,
    );

    create_object(
        &mut state,
        CardId(4),
        PlayerId(0),
        "Sac A".to_string(),
        Zone::Battlefield,
    );
    create_object(
        &mut state,
        CardId(5),
        PlayerId(0),
        "Sac B".to_string(),
        Zone::Battlefield,
    );

    let mut pending_ability = ResolvedAbility::new(
        Effect::SetTapState {
            target: TargetFilter::Any,
            scope: EffectScope::Single,
            state: TapStateChange::Tap,
        },
        vec![],
        source_id,
        PlayerId(0),
    );
    let mut sacrifice_ability = ResolvedAbility::new(
        Effect::Sacrifice {
            target: TargetFilter::Any,
            count: QuantityExpr::Fixed { value: 1 },
            min_count: 0,
        },
        vec![TargetRef::Player(PlayerId(0))],
        source_id,
        PlayerId(0),
    );
    sacrifice_ability.sub_ability = Some(Box::new(draw_that_many(source_id, PlayerId(0))));
    pending_ability.sub_ability = Some(Box::new(sacrifice_ability));

    state.waiting_for = WaitingFor::MultiTargetSelection {
        player: PlayerId(0),
        legal_targets: vec![target_id],
        min_targets: 1,
        max_targets: 1,
        pending_ability: Box::new(pending_ability),
    };

    let result = apply_as_current(
        &mut state,
        GameAction::SelectCards {
            cards: vec![target_id],
        },
    )
    .unwrap();

    assert!(matches!(
        result.waiting_for,
        WaitingFor::EffectZoneChoice {
            player: PlayerId(0),
            ..
        }
    ));
    assert!(state.pending_continuation.is_some());
    assert!(state.objects[&target_id].tapped);
}

#[test]
fn effect_zone_choice_handler_resolves_sacrifice_and_continuation() {
    let mut state = setup_game_at_main_phase();
    let source_id = ObjectId(100);
    let obj_id = create_object(
        &mut state,
        CardId(1),
        PlayerId(0),
        "Chosen Permanent".to_string(),
        Zone::Battlefield,
    );
    state.waiting_for = WaitingFor::EffectZoneChoice {
        player: PlayerId(0),
        cards: vec![obj_id],
        count: 1,
        min_count: 0,
        up_to: false,
        source_id,
        effect_kind: EffectKind::Sacrifice,
        zone: Zone::Battlefield,
        destination: None,
        enter_tapped: crate::types::zones::EtbTapState::Unspecified,
        enter_transformed: false,
        enters_under_player: None,
        enters_attacking: false,
        owner_library: false,
        track_exiled_by_source: false,
        face_down_profile: None,
        enter_with_counters: vec![],
        conditional_enter_with_counters: vec![],
        count_param: 0,
        library_position: None,
        is_cost_payment: false,
        enters_modified_if: None,
    };
    state.pending_continuation = Some(crate::types::game_state::PendingContinuation::new(
        Box::new(ResolvedAbility::new(
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 2 },
                player: crate::types::ability::TargetFilter::Controller,
            },
            vec![],
            source_id,
            PlayerId(0),
        )),
        &state,
    ));

    let result = apply_as_current(
        &mut state,
        GameAction::SelectCards {
            cards: vec![obj_id],
        },
    )
    .unwrap();

    assert!(matches!(result.waiting_for, WaitingFor::Priority { .. }));
    assert!(state.players[0].graveyard.contains(&obj_id));
    assert_eq!(state.players[0].life, 22);
    assert_eq!(state.last_effect_count, Some(1));
}

#[test]
fn effect_zone_choice_handler_resolves_untap_selection() {
    let mut state = setup_game_at_main_phase();
    let source_id = ObjectId(100);
    let chosen_land = create_object(
        &mut state,
        CardId(1),
        PlayerId(0),
        "Chosen Land".to_string(),
        Zone::Battlefield,
    );
    let unchosen_land = create_object(
        &mut state,
        CardId(2),
        PlayerId(0),
        "Unchosen Land".to_string(),
        Zone::Battlefield,
    );
    for id in [chosen_land, unchosen_land] {
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Land);
        obj.tapped = true;
    }

    state.waiting_for = WaitingFor::EffectZoneChoice {
        player: PlayerId(0),
        cards: vec![chosen_land, unchosen_land],
        count: 2,
        min_count: 0,
        up_to: true,
        source_id,
        effect_kind: EffectKind::Untap,
        zone: Zone::Battlefield,
        destination: None,
        enter_tapped: crate::types::zones::EtbTapState::Unspecified,
        enter_transformed: false,
        enters_under_player: None,
        enters_attacking: false,
        owner_library: false,
        track_exiled_by_source: false,
        face_down_profile: None,
        enter_with_counters: vec![],
        conditional_enter_with_counters: vec![],
        count_param: 0,
        library_position: None,
        is_cost_payment: false,
        enters_modified_if: None,
    };

    let result = apply_as_current(
        &mut state,
        GameAction::SelectCards {
            cards: vec![chosen_land],
        },
    )
    .unwrap();

    assert!(matches!(result.waiting_for, WaitingFor::Priority { .. }));
    assert!(!state.objects[&chosen_land].tapped);
    assert!(state.objects[&unchosen_land].tapped);
    assert_eq!(state.last_effect_count, Some(1));
}

#[test]
fn effect_zone_choice_up_to_respects_min_count() {
    let mut state = setup_game_at_main_phase();
    let source_id = ObjectId(100);
    let obj_id = create_object(
        &mut state,
        CardId(1),
        PlayerId(0),
        "Chosen Permanent".to_string(),
        Zone::Battlefield,
    );
    state.waiting_for = WaitingFor::EffectZoneChoice {
        player: PlayerId(0),
        cards: vec![obj_id],
        count: 1,
        min_count: 1,
        up_to: true,
        source_id,
        effect_kind: EffectKind::Sacrifice,
        zone: Zone::Battlefield,
        destination: None,
        enter_tapped: crate::types::zones::EtbTapState::Unspecified,
        enter_transformed: false,
        enters_under_player: None,
        enters_attacking: false,
        owner_library: false,
        track_exiled_by_source: false,
        face_down_profile: None,
        enter_with_counters: vec![],
        conditional_enter_with_counters: vec![],
        count_param: 0,
        library_position: None,
        is_cost_payment: false,
        enters_modified_if: None,
    };

    let result = apply_as_current(&mut state, GameAction::SelectCards { cards: vec![] });

    assert!(result.is_err());
    assert!(state.battlefield.contains(&obj_id));
}

#[test]
fn choose_one_of_enters_branch_choice_state() {
    let mut state = setup_game_at_main_phase();
    let source_id = ObjectId(100);
    let ability = ResolvedAbility::new(
        Effect::ChooseOneOf {
            chooser: PlayerFilter::Controller,
            branches: vec![draw_ability(1), draw_ability(2)],
        },
        vec![],
        source_id,
        PlayerId(0),
    );
    let mut events = Vec::new();

    effects::resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

    assert!(matches!(
        state.waiting_for,
        WaitingFor::ChooseOneOfBranch {
            player: PlayerId(0),
            controller: PlayerId(0),
            source_id: ObjectId(100),
            ..
        }
    ));
}

#[test]
fn choose_one_of_branch_resolves_selected_branch_with_original_controller() {
    let mut state = setup_game_at_main_phase();
    let source_id = ObjectId(100);
    let branch_gain = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::GainLife {
            amount: QuantityExpr::Fixed { value: 3 },
            player: TargetFilter::Controller,
        },
    );
    let branch_lose = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::LoseLife {
            amount: QuantityExpr::Fixed { value: 3 },
            target: None,
        },
    );
    state.waiting_for = WaitingFor::ChooseOneOfBranch {
        player: PlayerId(1),
        controller: PlayerId(0),
        source_id,
        branches: vec![branch_gain, branch_lose],
        branch_descriptions: vec!["Gain 3 life.".to_string(), "Lose 3 life.".to_string()],
        parent_targets: vec![],
        context: Default::default(),
        replacement_applied: Default::default(),
        remaining_players: vec![],
    };

    apply_as_current(&mut state, GameAction::ChooseBranch { index: 0 }).unwrap();

    assert_eq!(
        state.players[0].life, 23,
        "branch text using controller must resolve for original controller"
    );
    assert_eq!(state.players[1].life, 20);
}

#[test]
fn choose_one_of_each_opponent_prompts_apnap_and_branch_targets_faced_player() {
    let mut state = GameState::new(FormatConfig::standard(), 3, 42);
    state.turn_number = 2;
    state.phase = Phase::PreCombatMain;
    state.active_player = PlayerId(1);
    state.priority_player = PlayerId(1);
    state.waiting_for = WaitingFor::Priority {
        player: PlayerId(1),
    };
    let source_id = ObjectId(100);
    let branch_target_player_loses_life = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::LoseLife {
            amount: QuantityExpr::Fixed { value: 1 },
            target: Some(TargetFilter::Player),
        },
    );
    let ability = ResolvedAbility::new(
        Effect::ChooseOneOf {
            chooser: PlayerFilter::Opponent,
            branches: vec![branch_target_player_loses_life.clone(), draw_ability(1)],
        },
        vec![],
        source_id,
        PlayerId(0),
    );
    let mut events = Vec::new();

    effects::resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

    assert!(matches!(
        state.waiting_for,
        WaitingFor::ChooseOneOfBranch {
            player: PlayerId(1),
            remaining_players: ref rest,
            ..
        } if rest == &vec![PlayerId(2)]
    ));

    apply_as_current(&mut state, GameAction::ChooseBranch { index: 0 }).unwrap();

    assert_eq!(state.players[1].life, 19);
    assert_eq!(state.players[2].life, 20);
    assert!(matches!(
        state.waiting_for,
        WaitingFor::ChooseOneOfBranch {
            player: PlayerId(2),
            remaining_players: ref rest,
            ..
        } if rest.is_empty()
    ));

    apply_as_current(&mut state, GameAction::ChooseBranch { index: 0 }).unwrap();

    assert_eq!(state.players[1].life, 19);
    assert_eq!(state.players[2].life, 19);
    assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
}

#[test]
fn choose_one_of_scoped_player_sacrifice_prompts_faced_opponent() {
    let mut state = GameState::new_two_player(42);
    state.turn_number = 2;
    state.phase = Phase::PreCombatMain;
    state.active_player = PlayerId(0);
    state.priority_player = PlayerId(0);
    state.waiting_for = WaitingFor::Priority {
        player: PlayerId(0),
    };
    let source_id = ObjectId(100);
    let own_creature = create_object(
        &mut state,
        CardId(1),
        PlayerId(0),
        "Controller Creature".to_string(),
        Zone::Battlefield,
    );
    let opp_creature = create_object(
        &mut state,
        CardId(2),
        PlayerId(1),
        "Opponent Creature".to_string(),
        Zone::Battlefield,
    );
    let opp_creature_b = create_object(
        &mut state,
        CardId(3),
        PlayerId(1),
        "Second Opponent Creature".to_string(),
        Zone::Battlefield,
    );
    for id in [own_creature, opp_creature, opp_creature_b] {
        state
            .objects
            .get_mut(&id)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);
    }
    let sacrifice_branch = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::Sacrifice {
            target: TargetFilter::Typed(
                TypedFilter::creature().controller(ControllerRef::ScopedPlayer),
            ),
            count: QuantityExpr::Fixed { value: 1 },
            min_count: 0,
        },
    );
    let ability = ResolvedAbility::new(
        Effect::ChooseOneOf {
            chooser: PlayerFilter::Opponent,
            branches: vec![sacrifice_branch, draw_ability(1)],
        },
        vec![],
        source_id,
        PlayerId(0),
    );
    let mut events = Vec::new();

    effects::resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();
    apply_as_current(&mut state, GameAction::ChooseBranch { index: 0 }).unwrap();

    match &state.waiting_for {
        WaitingFor::EffectZoneChoice { player, cards, .. } => {
            assert_eq!(*player, PlayerId(1));
            assert_eq!(cards, &vec![opp_creature, opp_creature_b]);
            assert!(!cards.contains(&own_creature));
        }
        other => panic!("expected EffectZoneChoice for faced opponent, got {other:?}"),
    }
}

#[test]
fn choose_one_of_controller_token_branch_ignores_faced_opponent() {
    let mut state = GameState::new_two_player(42);
    state.turn_number = 2;
    state.phase = Phase::PreCombatMain;
    state.active_player = PlayerId(0);
    state.priority_player = PlayerId(0);
    state.waiting_for = WaitingFor::Priority {
        player: PlayerId(0),
    };
    let source_id = ObjectId(100);
    let sacrifice_branch = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::Sacrifice {
            target: TargetFilter::Typed(
                TypedFilter::creature().controller(ControllerRef::ScopedPlayer),
            ),
            count: QuantityExpr::Fixed { value: 1 },
            min_count: 0,
        },
    );
    let token_branch = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::Token {
            name: "b_3_3_a_dalek_menace".to_string(),
            power: crate::types::ability::PtValue::Fixed(0),
            toughness: crate::types::ability::PtValue::Fixed(0),
            types: vec![],
            colors: vec![],
            keywords: vec![],
            tapped: false,
            count: QuantityExpr::Fixed { value: 1 },
            owner: TargetFilter::Controller,
            attach_to: None,
            enters_attacking: false,
            supertypes: vec![],
            static_abilities: vec![],
            enter_with_counters: vec![],
        },
    );
    let ability = ResolvedAbility::new(
        Effect::ChooseOneOf {
            chooser: PlayerFilter::Opponent,
            branches: vec![sacrifice_branch, token_branch],
        },
        vec![],
        source_id,
        PlayerId(0),
    );
    let mut events = Vec::new();

    effects::resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();
    apply_as_current(&mut state, GameAction::ChooseBranch { index: 1 }).unwrap();

    let token = state
        .battlefield
        .iter()
        .filter_map(|id| state.objects.get(id))
        .find(|object| object.is_token)
        .expect("expected Dalek token");
    assert_eq!(token.controller, PlayerId(0));
    assert_eq!(token.owner, PlayerId(0));
}

#[test]
fn player_scope_all_uses_apnap_order_and_resumes_remaining_players() {
    let mut state = setup_game_at_main_phase();
    state.active_player = PlayerId(1);
    state.priority_player = PlayerId(1);
    state.waiting_for = WaitingFor::Priority {
        player: PlayerId(1),
    };

    let source_id = ObjectId(100);
    let p0_a = create_object(
        &mut state,
        CardId(1),
        PlayerId(0),
        "P0 A".to_string(),
        Zone::Battlefield,
    );
    let p0_b = create_object(
        &mut state,
        CardId(2),
        PlayerId(0),
        "P0 B".to_string(),
        Zone::Battlefield,
    );
    let p1_a = create_object(
        &mut state,
        CardId(3),
        PlayerId(1),
        "P1 A".to_string(),
        Zone::Battlefield,
    );
    let p1_b = create_object(
        &mut state,
        CardId(4),
        PlayerId(1),
        "P1 B".to_string(),
        Zone::Battlefield,
    );
    for id in [p0_a, p0_b, p1_a, p1_b] {
        state
            .objects
            .get_mut(&id)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);
    }

    let mut ability = ResolvedAbility::new(
        Effect::Sacrifice {
            target: TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::You)),
            count: QuantityExpr::Fixed { value: 1 },
            min_count: 0,
        },
        vec![],
        source_id,
        PlayerId(0),
    );
    ability.player_scope = Some(PlayerFilter::All);

    let mut events = Vec::new();
    effects::resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

    assert!(matches!(
        state.waiting_for,
        WaitingFor::EffectZoneChoice {
            player: PlayerId(1),
            ..
        }
    ));

    let result =
        apply_as_current(&mut state, GameAction::SelectCards { cards: vec![p1_a] }).unwrap();

    assert!(matches!(
        result.waiting_for,
        WaitingFor::EffectZoneChoice {
            player: PlayerId(0),
            ..
        }
    ));
}

#[test]
fn post_replacement_choose_sets_named_choice_waiting_for() {
    let mut state = GameState::new_two_player(42);
    let source_id = create_object(
        &mut state,
        CardId(1),
        PlayerId(0),
        "Multiversal Passage".to_string(),
        Zone::Battlefield,
    );
    let mut events = Vec::new();

    let effect_def = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::Choose {
            choice_type: crate::types::ability::ChoiceType::BasicLandType,
            persist: false,
            selection: crate::types::ability::TargetSelectionMode::Chosen,
        },
    )
    .sub_ability(AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::LoseLife {
            amount: QuantityExpr::Fixed { value: 2 },
            target: None,
        },
    ));

    let waiting_for = engine_replacement::apply_post_replacement_effect(
        &mut state,
        &effect_def,
        Some(source_id),
        None,
        None,
        Default::default(),
        &mut events,
    );

    assert!(matches!(
        waiting_for,
        Some(WaitingFor::NamedChoice {
            choice_type: crate::types::ability::ChoiceType::BasicLandType,
            ..
        })
    ));
    assert!(state.pending_continuation.is_some());
}

#[test]
fn choose_option_with_exact_source_stores_chosen_attribute() {
    use crate::types::ability::ChoiceType;
    use crate::types::mana::ManaColor;

    let mut state = GameState::new_two_player(42);
    let obj_id = create_object(
        &mut state,
        CardId(1),
        PlayerId(0),
        "Captivating Crossroads".to_string(),
        Zone::Battlefield,
    );

    // Set up an exact-object prompt (simulating a persist=true Choose).
    state.waiting_for = WaitingFor::NamedChoice {
        player: PlayerId(0),
        choice_type: ChoiceType::color(),
        options: vec![
            "White".to_string(),
            "Blue".to_string(),
            "Black".to_string(),
            "Red".to_string(),
            "Green".to_string(),
        ],
        source: Some(
            crate::types::game_state::NamedChoiceSource::from_trigger_source(
                crate::game::triggers::trigger_source_context_for_latch(
                    &state,
                    state.objects.get(&obj_id).unwrap(),
                ),
                crate::types::game_state::NamedChoiceSourceBinding::ExactObjectAndResolution,
            ),
        ),
        persist_player: None,
    };

    let result = apply_as_current(
        &mut state,
        GameAction::ChooseOption {
            choice: "Red".to_string(),
        },
    );
    assert!(result.is_ok());

    // Verify the choice was stored on the object
    let obj = state.objects.get(&obj_id).unwrap();
    assert_eq!(obj.chosen_color(), Some(ManaColor::Red));
}

#[test]
fn glacierwood_siege_resolution_prompts_for_anchor_word_choice() {
    let mut state = setup_game_at_main_phase();
    let siege_id = create_object(
        &mut state,
        CardId(621),
        PlayerId(0),
        "Glacierwood Siege".to_string(),
        Zone::Stack,
    );
    {
        let obj = state.objects.get_mut(&siege_id).unwrap();
        obj.card_types.core_types.push(CoreType::Enchantment);
        obj.base_card_types = obj.card_types.clone();
    }
    apply_oracle_to_object(
        &mut state,
        siege_id,
        "Glacierwood Siege",
        "As this enchantment enters, choose Temur or Sultai.\n\
• Temur — Whenever you cast an instant or sorcery spell, target player mills four cards.\n\
• Sultai — You may play lands from your graveyard.",
    );

    state.stack.push_back(StackEntry {
        id: siege_id,
        source_id: siege_id,
        controller: PlayerId(0),
        kind: StackEntryKind::Spell {
            card_id: CardId(621),
            ability: None,
            casting_variant: crate::types::game_state::CastingVariant::Normal,
            actual_mana_spent: 0,
        },
    });

    apply_as_current(&mut state, GameAction::PassPriority).unwrap();
    let resolve = apply_as_current(&mut state, GameAction::PassPriority).unwrap();

    assert!(state.battlefield.contains(&siege_id));
    match resolve.waiting_for {
        WaitingFor::NamedChoice {
            player,
            choice_type: crate::types::ability::ChoiceType::Labeled { ref options },
            source: Some(source),
            ..
        } => {
            assert_eq!(player, PlayerId(0));
            assert_eq!(source.prompt.identity.reference.object_id, siege_id);
            assert_eq!(
                source.binding,
                crate::types::game_state::NamedChoiceSourceBinding::ExactObjectAndResolution
            );
            assert_eq!(options, &vec!["Temur".to_string(), "Sultai".to_string()]);
        }
        other => panic!("expected Glacierwood Siege anchor choice, got {other:?}"),
    }

    apply_as_current(
        &mut state,
        GameAction::ChooseOption {
            choice: "Temur".to_string(),
        },
    )
    .unwrap();

    assert_eq!(state.objects[&siege_id].chosen_label(), Some("Temur"));
}

#[test]
fn restricted_color_choice_rejects_excluded_color() {
    use crate::types::ability::ChoiceType;
    use crate::types::mana::ManaColor;

    let mut state = GameState::new_two_player(42);
    state.waiting_for = WaitingFor::NamedChoice {
        player: PlayerId(0),
        choice_type: ChoiceType::color_excluding(vec![ManaColor::White]),
        options: vec![
            "Blue".to_string(),
            "Black".to_string(),
            "Red".to_string(),
            "Green".to_string(),
        ],
        source: None,
        persist_player: None,
    };

    let result = apply_as_current(
        &mut state,
        GameAction::ChooseOption {
            choice: "White".to_string(),
        },
    );

    assert!(result.is_err());
}

#[test]
fn copy_target_choice_resolves_become_copy() {
    // CR 707.9: Test the CopyTargetChoice → BecomeCopy flow.
    // Set up a clone creature on battlefield and a target creature to copy.
    let mut state = GameState::new_two_player(42);

    let target_id = zones::create_object(
        &mut state,
        CardId(1),
        PlayerId(0),
        "Grizzly Bears".to_string(),
        Zone::Battlefield,
    );
    {
        let target = state.objects.get_mut(&target_id).unwrap();
        target.base_power = Some(2);
        target.base_toughness = Some(2);
        target.power = Some(2);
        target.toughness = Some(2);
    }

    let clone_id = zones::create_object(
        &mut state,
        CardId(2),
        PlayerId(0),
        "Clone".to_string(),
        Zone::Battlefield,
    );
    {
        let clone = state.objects.get_mut(&clone_id).unwrap();
        clone.base_power = Some(0);
        clone.base_toughness = Some(0);
        clone.power = Some(0);
        clone.toughness = Some(0);
        clone.replacement_definitions.push(
            crate::types::ability::ReplacementDefinition::new(
                crate::types::replacements::ReplacementEvent::Moved,
            )
            .execute(crate::types::ability::AbilityDefinition::new(
                crate::types::ability::AbilityKind::Spell,
                crate::types::ability::Effect::BecomeCopy {
                    recipient: TargetFilter::SelfRef,
                    target: TargetFilter::Any,
                    duration: None,
                    mana_value_limit: Some(
                        crate::types::ability::CopyManaValueLimit::AmountSpentToCastSource,
                    ),
                    additional_modifications: vec![
                        crate::types::ability::ContinuousModification::AddSubtype {
                            subtype: "Bird".to_string(),
                        },
                        crate::types::ability::ContinuousModification::AddKeyword {
                            keyword: crate::types::keywords::Keyword::Flying,
                        },
                    ],
                },
            )),
        );
    }

    // Set up CopyTargetChoice waiting state
    state.waiting_for = WaitingFor::CopyTargetChoice {
        player: PlayerId(0),
        source_id: clone_id,
        valid_targets: vec![target_id],
        max_mana_value: None,
    };

    // Player chooses to copy Grizzly Bears
    let result = apply_as_current(
        &mut state,
        GameAction::ChooseTarget {
            target: Some(TargetRef::Object(target_id)),
        },
    );
    assert!(result.is_ok());

    // Verify the clone now has the target's characteristics
    let clone = state.objects.get(&clone_id).unwrap();
    assert_eq!(clone.name, "Grizzly Bears");
    assert_eq!(clone.power, Some(2));
    assert_eq!(clone.toughness, Some(2));
    assert!(clone.card_types.subtypes.contains(&"Bird".to_string()));
    assert!(clone
        .keywords
        .contains(&crate::types::keywords::Keyword::Flying));
}

#[test]
fn copy_target_choice_applies_copied_enter_with_counters_replacement_before_sba() {
    let mut state = GameState::new_two_player(42);

    let ghave = zones::create_object(
        &mut state,
        CardId(1),
        PlayerId(1),
        "Ghave, Guru of Spores".to_string(),
        Zone::Battlefield,
    );
    {
        let obj = state.objects.get_mut(&ghave).unwrap();
        obj.base_power = Some(0);
        obj.base_toughness = Some(0);
        obj.power = Some(5);
        obj.toughness = Some(5);
        obj.counters
            .insert(crate::types::counter::CounterType::Plus1Plus1, 5);
        let enter_with_counters = crate::types::ability::ReplacementDefinition::new(
            crate::types::replacements::ReplacementEvent::Moved,
        )
        .execute(crate::types::ability::AbilityDefinition::new(
            crate::types::ability::AbilityKind::Spell,
            Effect::PutCounter {
                counter_type: crate::types::counter::CounterType::Plus1Plus1,
                count: crate::types::ability::QuantityExpr::Fixed { value: 5 },
                target: TargetFilter::SelfRef,
            },
        ))
        .valid_card(TargetFilter::SelfRef);
        obj.base_replacement_definitions = Arc::new(vec![enter_with_counters.clone()]);
        obj.replacement_definitions.push(enter_with_counters);
    }

    let assassin = zones::create_object(
        &mut state,
        CardId(2),
        PlayerId(0),
        "Callidus Assassin".to_string(),
        Zone::Battlefield,
    );
    {
        let obj = state.objects.get_mut(&assassin).unwrap();
        obj.base_power = Some(3);
        obj.base_toughness = Some(3);
        obj.power = Some(3);
        obj.toughness = Some(3);
        obj.replacement_definitions.push(
            crate::types::ability::ReplacementDefinition::new(
                crate::types::replacements::ReplacementEvent::Moved,
            )
            .execute(crate::types::ability::AbilityDefinition::new(
                crate::types::ability::AbilityKind::Spell,
                Effect::BecomeCopy {
                    recipient: TargetFilter::SelfRef,
                    target: TargetFilter::Typed(crate::types::ability::TypedFilter::new(
                        crate::types::ability::TypeFilter::Creature,
                    )),
                    duration: None,
                    mana_value_limit: None,
                    additional_modifications: Vec::new(),
                },
            )),
        );
    }

    state.waiting_for = WaitingFor::CopyTargetChoice {
        player: PlayerId(0),
        source_id: assassin,
        valid_targets: vec![ghave],
        max_mana_value: None,
    };

    apply_as_current(
        &mut state,
        GameAction::ChooseTarget {
            target: Some(TargetRef::Object(ghave)),
        },
    )
    .expect("copy target choice should resolve");

    let copied = state.objects.get(&assassin).unwrap();
    assert_eq!(copied.zone, Zone::Battlefield);
    assert_eq!(copied.name, "Ghave, Guru of Spores");
    assert_eq!(
        copied
            .counters
            .get(&crate::types::counter::CounterType::Plus1Plus1)
            .copied(),
        Some(5),
        "CR 614.12: copied self ETB counters must apply before SBAs"
    );
    assert_eq!(copied.power, Some(5));
    assert_eq!(copied.toughness, Some(5));
}

#[test]
fn echoing_deeps_copying_sunken_citadel_prompts_for_the_copied_color_choice() {
    let mut state = setup_game_at_main_phase();
    let citadel = zones::create_object(
        &mut state,
        CardId(9_021),
        PlayerId(1),
        "Sunken Citadel".to_string(),
        Zone::Graveyard,
    );
    state
        .objects
        .get_mut(&citadel)
        .unwrap()
        .card_types
        .core_types
        .push(CoreType::Land);
    apply_oracle_to_object(
        &mut state,
        citadel,
        "Sunken Citadel",
        "This land enters tapped. As it enters, choose a color.\n{T}: Add one mana of the chosen color.\n{T}: Add two mana of the chosen color. Spend this mana only to activate abilities of land sources.",
    );
    let deeps = zones::create_object(
        &mut state,
        CardId(9_022),
        PlayerId(0),
        "Echoing Deeps".to_string(),
        Zone::Battlefield,
    );
    state
        .objects
        .get_mut(&deeps)
        .unwrap()
        .card_types
        .core_types
        .push(CoreType::Land);
    apply_oracle_to_object(
        &mut state,
        deeps,
        "Echoing Deeps",
        "You may have this land enter tapped as a copy of any land card in a graveyard, except it's a Cave in addition to its other types.\n{T}: Add {C}.",
    );
    state.waiting_for = WaitingFor::CopyTargetChoice {
        player: PlayerId(0),
        source_id: deeps,
        valid_targets: vec![citadel],
        max_mana_value: None,
    };

    let result = apply_as_current(
        &mut state,
        GameAction::ChooseTarget {
            target: Some(TargetRef::Object(citadel)),
        },
    )
    .expect("Echoing Deeps should copy Sunken Citadel");
    let WaitingFor::NamedChoice {
        player,
        source: Some(source),
        options,
        ..
    } = result.waiting_for
    else {
        panic!(
            "the copied as-enters choice must be offered, got {:?}",
            state.waiting_for
        );
    };
    assert_eq!(player, PlayerId(0));
    assert_eq!(source.prompt.identity.reference.object_id, deeps);
    assert!(options
        .iter()
        .any(|option| option.eq_ignore_ascii_case("blue")));

    apply_as_current(
        &mut state,
        GameAction::ChooseOption {
            choice: options
                .into_iter()
                .find(|option| option.eq_ignore_ascii_case("blue"))
                .unwrap(),
        },
    )
    .expect("the copied land must accept its as-enters color choice");
    let copy = &state.objects[&deeps];
    assert_eq!(copy.name, "Sunken Citadel");
    assert_eq!(copy.chosen_color(), Some(ManaColor::Blue));
}

/// CR 614.1c + CR 707.9: Echoing Deeps can enter tapped as a copy only when
/// a land card exists in a graveyard. With no copy source, it enters untapped
/// and must not expose an invalid accept-replacement action to the AI.
#[test]
fn echoing_deeps_with_empty_graveyards_enters_untapped_without_copy_offer() {
    let mut state = setup_game_at_main_phase();
    let deeps = zones::create_object(
        &mut state,
        CardId(9_023),
        PlayerId(0),
        "Echoing Deeps".to_string(),
        Zone::Hand,
    );
    state
        .objects
        .get_mut(&deeps)
        .unwrap()
        .card_types
        .core_types
        .push(CoreType::Land);
    apply_oracle_to_object(
        &mut state,
        deeps,
        "Echoing Deeps",
        "You may have this land enter tapped as a copy of any land card in a graveyard, except it's a Cave in addition to its other types.\n{T}: Add {C}.",
    );
    assert!(state
        .players
        .iter()
        .all(|player| player.graveyard.is_empty()));

    let result = apply_as_current(
        &mut state,
        GameAction::PlayLand {
            object_id: deeps,
            card_id: CardId(9_023),
        },
    )
    .expect("Echoing Deeps should be playable with empty graveyards");

    assert!(
        matches!(result.waiting_for, WaitingFor::Priority { .. }),
        "an impossible copy must not offer an accept-replacement action, got {:?}",
        result.waiting_for
    );
    let entered = &state.objects[&deeps];
    assert_eq!(entered.zone, Zone::Battlefield);
    assert!(
        !entered.tapped,
        "an un-copied Echoing Deeps enters untapped"
    );
}

/// CR 614.1c + CR 707.9: the impossible-copy guard must not suppress a real
/// Echoing Deeps choice. A land card in either graveyard makes the optional
/// replacement applicable; accepting it prompts for that land and the Deeps
/// enters tapped as its copy with the additional Cave subtype.
#[test]
fn echoing_deeps_with_graveyard_land_offers_and_applies_copy() {
    let mut state = setup_game_at_main_phase();
    let forest = zones::create_object(
        &mut state,
        CardId(9_024),
        PlayerId(1),
        "Forest".to_string(),
        Zone::Graveyard,
    );
    state
        .objects
        .get_mut(&forest)
        .unwrap()
        .card_types
        .core_types
        .push(CoreType::Land);
    let deeps = zones::create_object(
        &mut state,
        CardId(9_025),
        PlayerId(0),
        "Echoing Deeps".to_string(),
        Zone::Hand,
    );
    state
        .objects
        .get_mut(&deeps)
        .unwrap()
        .card_types
        .core_types
        .push(CoreType::Land);
    apply_oracle_to_object(
        &mut state,
        deeps,
        "Echoing Deeps",
        "You may have this land enter tapped as a copy of any land card in a graveyard, except it's a Cave in addition to its other types.\n{T}: Add {C}.",
    );

    let result = apply_as_current(
        &mut state,
        GameAction::PlayLand {
            object_id: deeps,
            card_id: CardId(9_025),
        },
    )
    .expect("Echoing Deeps should be playable with a graveyard land");
    let WaitingFor::ReplacementChoice {
        player,
        candidate_count,
        ..
    } = result.waiting_for
    else {
        panic!(
            "a valid graveyard land must expose the optional copy replacement, got {:?}",
            state.waiting_for
        );
    };
    assert_eq!(player, PlayerId(0));
    assert_eq!(
        candidate_count, 2,
        "the player may accept or decline the copy"
    );

    let result = apply_as_current(&mut state, GameAction::ChooseReplacement { index: 0 })
        .expect("accepting Echoing Deeps' replacement should prompt for a copy target");
    let WaitingFor::CopyTargetChoice {
        player,
        source_id,
        valid_targets,
        ..
    } = result.waiting_for
    else {
        panic!(
            "accepting the copy replacement must expose the graveyard land, got {:?}",
            state.waiting_for
        );
    };
    assert_eq!(player, PlayerId(0));
    assert_eq!(source_id, deeps);
    assert_eq!(valid_targets, vec![forest]);

    apply_as_current(
        &mut state,
        GameAction::ChooseTarget {
            target: Some(TargetRef::Object(forest)),
        },
    )
    .expect("Echoing Deeps should enter as a copy of the chosen graveyard land");
    let entered = &state.objects[&deeps];
    assert_eq!(entered.zone, Zone::Battlefield);
    assert_eq!(entered.name, "Forest");
    assert!(entered.tapped, "a copied Echoing Deeps enters tapped");
    assert!(
        entered
            .card_types
            .subtypes
            .iter()
            .any(|subtype| subtype == "Cave"),
        "the copy must be a Cave in addition to its other types"
    );
}

/// CR 614.12a + CR 707.9: Callidus Assassin grants its copy a "When this
/// creature enters" trigger as part of the entering-as-copy bundle. The
/// ETB event for the copy must fire *after* the player chooses a target
/// for the copy effect and `BecomeCopy` has stamped the granted trigger
/// onto `trigger_definitions` — otherwise the trigger silently never
/// fires. Regression for: the deferred-trigger replay path in
/// `engine_priority::run_post_action_pipeline` +
/// `engine_replacement::handle_copy_target_choice`.
#[test]
fn copy_target_choice_fires_granted_etb_trigger_against_deferred_entry_event() {
    use crate::types::ability::{
        AbilityDefinition, AbilityKind, ContinuousModification, QuantityExpr, TriggerDefinition,
    };
    use crate::types::triggers::TriggerMode;

    let mut state = GameState::new_two_player(42);

    let bear = zones::create_object(
        &mut state,
        CardId(1),
        PlayerId(1),
        "Bear".to_string(),
        Zone::Battlefield,
    );
    {
        let obj = state.objects.get_mut(&bear).unwrap();
        obj.base_power = Some(2);
        obj.base_toughness = Some(2);
        obj.power = Some(2);
        obj.toughness = Some(2);
    }

    // Granted trigger: "When this creature enters, controller draws a card."
    // Targetless to keep the test focused on the deferral mechanism rather
    // than target-selection plumbing.
    let granted = TriggerDefinition::new(TriggerMode::ChangesZone)
        .execute(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
        ))
        .valid_card(TargetFilter::SelfRef)
        .destination(Zone::Battlefield);

    let assassin = zones::create_object(
        &mut state,
        CardId(2),
        PlayerId(0),
        "Callidus Assassin".to_string(),
        Zone::Battlefield,
    );
    {
        let obj = state.objects.get_mut(&assassin).unwrap();
        obj.base_power = Some(3);
        obj.base_toughness = Some(3);
        obj.power = Some(3);
        obj.toughness = Some(3);
        obj.replacement_definitions.push(
            crate::types::ability::ReplacementDefinition::new(
                crate::types::replacements::ReplacementEvent::Moved,
            )
            .execute(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::BecomeCopy {
                    recipient: TargetFilter::SelfRef,
                    target: TargetFilter::Typed(crate::types::ability::TypedFilter::new(
                        crate::types::ability::TypeFilter::Creature,
                    )),
                    duration: None,
                    mana_value_limit: None,
                    additional_modifications: vec![ContinuousModification::GrantTrigger {
                        trigger: Box::new(granted.clone()),
                    }],
                },
            )),
        );
    }

    // Capture a real `ZoneChanged` for Callidus by bouncing it through
    // stack→battlefield once. We then put it in the deferred queue to
    // model what the post-action pipeline does at the moment
    // `CopyTargetChoice` is set up.
    {
        let mut warmup_events: Vec<GameEvent> = Vec::new();
        zones::move_to_zone(&mut state, assassin, Zone::Stack, &mut warmup_events);
        warmup_events.clear();
        zones::move_to_zone(&mut state, assassin, Zone::Battlefield, &mut warmup_events);
        let entry_event = warmup_events
            .into_iter()
            .find(|e| {
                matches!(
                    e,
                    GameEvent::ZoneChanged { object_id, to, .. }
                        if *object_id == assassin && *to == Zone::Battlefield
                )
            })
            .expect("move_to_zone must emit a ZoneChanged for the entry");
        state.deferred_entry_events.push(entry_event);
    }
    state.waiting_for = WaitingFor::CopyTargetChoice {
        player: PlayerId(0),
        source_id: assassin,
        valid_targets: vec![bear],
        max_mana_value: None,
    };

    apply_as_current(
        &mut state,
        GameAction::ChooseTarget {
            target: Some(TargetRef::Object(bear)),
        },
    )
    .expect("copy target choice should resolve");

    // After the copy resolves and layers re-evaluate, the granted trigger
    // must be on the copy's trigger_definitions...
    let copied = state.objects.get(&assassin).unwrap();
    assert!(
        copied
            .trigger_definitions
            .iter_all()
            .any(|t| t.definition == granted),
        "BecomeCopy's GrantTrigger modification must be present on the copy"
    );

    // ...and the deferred entry event must have been replayed through
    // process_triggers, so the granted ETB matched and queued.
    assert!(
        state.deferred_entry_events.is_empty(),
        "deferred entry events must be drained after copy choice resolves"
    );
    let trigger_fired = state.pending_trigger.is_some()
        || state.stack.iter().any(|entry| {
            matches!(
                entry.kind,
                crate::types::game_state::StackEntryKind::TriggeredAbility { source_id, .. }
                    if source_id == assassin
            )
        });
    assert!(
        trigger_fired,
        "granted ETB trigger must fire from the deferred entry event"
    );
}

/// Issue #429 — CR 113.2c + CR 603.3b + CR 707.10: When the copy-replacement
/// ETB event is replayed by `handle_copy_target_choice`, multiple interactive
/// triggers can fire simultaneously. `process_triggers` sets the first as
/// `state.pending_trigger` and stashes the rest into `state.deferred_triggers`.
/// The handler previously returned `WaitingFor::Priority` unconditionally,
/// silently dropping the first trigger's target-selection prompt. The handler
/// must hand back the active trigger's `TriggerTargetSelection` instead.
#[test]
fn copy_target_choice_surfaces_interactive_trigger_prompt_for_deferred_entry() {
    use crate::types::ability::{
        AbilityDefinition, AbilityKind, QuantityExpr, TriggerDefinition, TypedFilter,
    };
    use crate::types::triggers::TriggerMode;

    let mut state = GameState::new_two_player(42);

    // Two observers, each with a *targeted* "when a creature enters, deal 1
    // damage to target creature" ETB trigger. Both watch the replayed
    // Callidus entry event, so two interactive triggers fire at once.
    let make_observer = |state: &mut GameState, card: u64| -> ObjectId {
        let obs = zones::create_object(
            state,
            CardId(card),
            PlayerId(0),
            format!("Observer {card}"),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&obs).unwrap();
            obj.card_types
                .core_types
                .push(crate::types::card_type::CoreType::Creature);
            obj.base_power = Some(1);
            obj.base_toughness = Some(1);
            obj.power = Some(1);
            obj.toughness = Some(1);
            obj.trigger_definitions.push(
                TriggerDefinition::new(TriggerMode::ChangesZone)
                    .execute(AbilityDefinition::new(
                        AbilityKind::Spell,
                        Effect::DealDamage {
                            amount: QuantityExpr::Fixed { value: 1 },
                            target: TargetFilter::Typed(TypedFilter::creature()),
                            damage_source: None,
                            excess: None,
                        },
                    ))
                    .valid_card(TargetFilter::Typed(TypedFilter::creature()))
                    .destination(Zone::Battlefield),
            );
        }
        obs
    };
    let observer_a = make_observer(&mut state, 10);
    let observer_b = make_observer(&mut state, 11);

    // Copy target on the battlefield.
    let bear = zones::create_object(
        &mut state,
        CardId(1),
        PlayerId(1),
        "Bear".to_string(),
        Zone::Battlefield,
    );
    {
        let obj = state.objects.get_mut(&bear).unwrap();
        obj.card_types
            .core_types
            .push(crate::types::card_type::CoreType::Creature);
        obj.base_power = Some(2);
        obj.base_toughness = Some(2);
        obj.power = Some(2);
        obj.toughness = Some(2);
        // CR 707.2: `BecomeCopy` copies the *intrinsic copiable values*
        // (`base_*` fields), not the layer-derived ones. The bear's
        // creature type must live on `base_card_types` / `base_name` so the
        // realized copy is a creature — otherwise the observers' creature-
        // filtered ETB triggers never match the replayed copy entry.
        obj.base_card_types = obj.card_types.clone();
        obj.base_name = obj.name.clone();
    }

    // Callidus Assassin with a plain BecomeCopy "enter as a copy" replacement.
    let assassin = zones::create_object(
        &mut state,
        CardId(2),
        PlayerId(0),
        "Callidus Assassin".to_string(),
        Zone::Battlefield,
    );
    {
        let obj = state.objects.get_mut(&assassin).unwrap();
        obj.card_types
            .core_types
            .push(crate::types::card_type::CoreType::Creature);
        obj.base_power = Some(3);
        obj.base_toughness = Some(3);
        obj.power = Some(3);
        obj.toughness = Some(3);
        obj.replacement_definitions.push(
            crate::types::ability::ReplacementDefinition::new(
                crate::types::replacements::ReplacementEvent::Moved,
            )
            .execute(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::BecomeCopy {
                    recipient: TargetFilter::SelfRef,
                    target: TargetFilter::Typed(TypedFilter::creature()),
                    duration: None,
                    mana_value_limit: None,
                    additional_modifications: Vec::new(),
                },
            )),
        );
    }

    // Capture a real `ZoneChanged` for Callidus entering, mirroring what the
    // post-action pipeline stashes when `CopyTargetChoice` is set up.
    {
        let mut warmup_events: Vec<GameEvent> = Vec::new();
        zones::move_to_zone(&mut state, assassin, Zone::Stack, &mut warmup_events);
        warmup_events.clear();
        zones::move_to_zone(&mut state, assassin, Zone::Battlefield, &mut warmup_events);
        let entry_event = warmup_events
            .into_iter()
            .find(|e| {
                matches!(
                    e,
                    GameEvent::ZoneChanged { object_id, to, .. }
                        if *object_id == assassin && *to == Zone::Battlefield
                )
            })
            .expect("move_to_zone must emit a ZoneChanged for the entry");
        state.deferred_entry_events.push(entry_event);
    }
    state.waiting_for = WaitingFor::CopyTargetChoice {
        player: PlayerId(0),
        source_id: assassin,
        valid_targets: vec![bear],
        max_mana_value: None,
    };

    let _waiting = apply_as_current(
        &mut state,
        GameAction::ChooseTarget {
            target: Some(TargetRef::Object(bear)),
        },
    )
    .expect("copy target choice should resolve")
    .waiting_for;

    // CR 603.3b (#531): The two simultaneously-fired interactive ETB
    // triggers belong to one controller (PlayerId(0)); the engine emits
    // OrderTriggers first. Drain with identity so the legacy assertion
    // below can inspect the post-ordering TriggerTargetSelection state.
    crate::game::triggers::drain_order_triggers_with_identity(&mut state);
    let waiting = state.waiting_for.clone();

    // The first interactive trigger's target-selection prompt must be
    // surfaced — not silently dropped in favor of Priority.
    assert!(
        matches!(waiting, WaitingFor::TriggerTargetSelection { .. }),
        "expected the first interactive ETB trigger's prompt, got {waiting:?}"
    );
    assert!(
        state.pending_trigger.is_some(),
        "the active interactive trigger must be set as pending_trigger"
    );
    // The second simultaneously-fired trigger must be retained in the
    // deferred queue so it reaches the stack after the first resolves.
    assert_eq!(
        state.deferred_triggers.len(),
        1,
        "the sibling interactive trigger must be deferred, not dropped"
    );
    // Both observers must be the trigger sources (one active, one deferred).
    let pending_src = state.pending_trigger.as_ref().unwrap().source_id;
    let deferred_src = state.deferred_triggers[0].pending.source_id;
    let mut srcs = [pending_src, deferred_src];
    srcs.sort_by_key(|id| id.0);
    let mut expected = [observer_a, observer_b];
    expected.sort_by_key(|id| id.0);
    assert_eq!(
        srcs, expected,
        "both observers' ETB triggers must be accounted for"
    );
}

#[test]
fn copy_target_choice_rejects_invalid_target() {
    let mut state = GameState::new_two_player(42);

    let valid_id = zones::create_object(
        &mut state,
        CardId(1),
        PlayerId(0),
        "Bear".to_string(),
        Zone::Battlefield,
    );
    let invalid_id = zones::create_object(
        &mut state,
        CardId(2),
        PlayerId(0),
        "Bird".to_string(),
        Zone::Battlefield,
    );
    let clone_id = zones::create_object(
        &mut state,
        CardId(3),
        PlayerId(0),
        "Clone".to_string(),
        Zone::Battlefield,
    );

    state.waiting_for = WaitingFor::CopyTargetChoice {
        player: PlayerId(0),
        source_id: clone_id,
        valid_targets: vec![valid_id], // Bird is NOT in valid targets
        max_mana_value: None,
    };

    // Try to choose invalid target
    let result = apply_as_current(
        &mut state,
        GameAction::ChooseTarget {
            target: Some(TargetRef::Object(invalid_id)),
        },
    );
    assert!(result.is_err());
}

// ── Superior Spider-Man integration test ──
// CR 707.9 + CR 707.2 + CR 613.1d + CR 603.12: Full flow for
// `Mind Swap — You may have Superior Spider-Man enter as a copy of any
// creature card in a graveyard, except his name is Superior Spider-Man and
// he's a 4/4 Spider Human Hero in addition to his other types. When you
// do, exile that card.`
#[test]
fn superior_spider_man_full_copy_flow_copies_graveyard_card_and_exiles_it() {
    use crate::types::ability::ContinuousModification;
    use crate::types::card_type::Supertype;

    let mut state = GameState::new_two_player(42);

    // Elesh Norn in PlayerId(1)'s graveyard with abilities + keywords.
    let elesh = zones::create_object(
        &mut state,
        CardId(1),
        PlayerId(1),
        "Elesh Norn".to_string(),
        Zone::Graveyard,
    );
    {
        let obj = state.objects.get_mut(&elesh).unwrap();
        obj.base_name = "Elesh Norn".to_string();
        obj.base_power = Some(7);
        obj.base_toughness = Some(7);
        obj.base_card_types = crate::types::card_type::CardType {
            supertypes: vec![Supertype::Legendary],
            core_types: vec![CoreType::Creature],
            subtypes: vec!["Phyrexian".to_string(), "Praetor".to_string()],
        };
        obj.base_keywords = vec![crate::types::keywords::Keyword::Vigilance];
        obj.base_abilities = Arc::new(vec![crate::types::ability::AbilityDefinition::new(
            crate::types::ability::AbilityKind::Activated,
            Effect::Draw {
                count: crate::types::ability::QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
        )]);
    }

    // Superior Spider-Man freshly on battlefield under PlayerId(0)'s control.
    let spidey = zones::create_object(
        &mut state,
        CardId(2),
        PlayerId(0),
        "Superior Spider-Man".to_string(),
        Zone::Battlefield,
    );
    {
        let obj = state.objects.get_mut(&spidey).unwrap();
        obj.base_name = "Superior Spider-Man".to_string();
        obj.base_power = Some(4);
        obj.base_toughness = Some(4);
        obj.base_card_types = crate::types::card_type::CardType {
            supertypes: vec![Supertype::Legendary],
            core_types: vec![CoreType::Creature],
            subtypes: vec![
                "Spider".to_string(),
                "Human".to_string(),
                "Hero".to_string(),
            ],
        };
        // Install the replacement exactly as the parser would emit it:
        // BecomeCopy with additional_modifications + reflexive sub_ability.
        let reflexive = crate::types::ability::AbilityDefinition::new(
            crate::types::ability::AbilityKind::Spell,
            Effect::ChangeZone {
                origin: None,
                destination: Zone::Exile,
                target: TargetFilter::ParentTarget,
                owner_library: false,
                enter_transformed: false,
                enters_under: None,
                enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
                conditional_enter_with_counters: vec![],
                face_down_profile: None,
                enters_modified_if: None,
            },
        );
        let reflexive = crate::types::ability::AbilityDefinition {
            condition: Some(crate::types::ability::AbilityCondition::WhenYouDo),
            ..reflexive
        };
        let become_copy = crate::types::ability::AbilityDefinition::new(
            crate::types::ability::AbilityKind::Spell,
            Effect::BecomeCopy {
                recipient: TargetFilter::SelfRef,
                target: TargetFilter::Typed(
                    crate::types::ability::TypedFilter::new(
                        crate::types::ability::TypeFilter::Creature,
                    )
                    .properties(vec![
                        crate::types::ability::FilterProp::InZone {
                            zone: Zone::Graveyard,
                        },
                    ]),
                ),
                duration: None,
                mana_value_limit: None,
                additional_modifications: vec![
                    ContinuousModification::SetName {
                        name: "Superior Spider-Man".to_string(),
                    },
                    ContinuousModification::SetPower { value: 4 },
                    ContinuousModification::SetToughness { value: 4 },
                    ContinuousModification::AddSubtype {
                        subtype: "Spider".to_string(),
                    },
                    ContinuousModification::AddSubtype {
                        subtype: "Human".to_string(),
                    },
                    ContinuousModification::AddSubtype {
                        subtype: "Hero".to_string(),
                    },
                ],
            },
        )
        .sub_ability(reflexive);
        obj.replacement_definitions.push(
            crate::types::ability::ReplacementDefinition::new(
                crate::types::replacements::ReplacementEvent::Moved,
            )
            .execute(become_copy),
        );
    }

    // Simulate reaching CopyTargetChoice directly (the replacement pipeline
    // tests cover the preceding "enter" pause; here we focus on the
    // post-choice resolution: copy + reflexive trigger firing).
    state.waiting_for = WaitingFor::CopyTargetChoice {
        player: PlayerId(0),
        source_id: spidey,
        valid_targets: vec![elesh],
        max_mana_value: None,
    };

    let result = apply_as_current(
        &mut state,
        GameAction::ChooseTarget {
            target: Some(TargetRef::Object(elesh)),
        },
    );
    assert!(result.is_ok(), "copy target choice should resolve");

    // (a) Copied abilities from Elesh Norn: activated Draw ability should be present.
    let copied = state.objects.get(&spidey).unwrap();
    assert!(
        copied
            .abilities
            .iter()
            .any(|a| matches!(&*a.effect, Effect::Draw { .. })),
        "copied abilities must include Elesh Norn's Draw"
    );
    assert!(
        copied
            .keywords
            .contains(&crate::types::keywords::Keyword::Vigilance),
        "copied keywords must include Vigilance"
    );

    // (b) Name is overridden to Superior Spider-Man (not Elesh Norn).
    assert_eq!(
        copied.name, "Superior Spider-Man",
        "SetName must override the copied name"
    );

    // (c) P/T overridden to 4/4.
    assert_eq!(copied.power, Some(4));
    assert_eq!(copied.toughness, Some(4));

    // (d) Types include Elesh Norn's (Phyrexian, Praetor) AND additive
    //     Spider/Human/Hero.
    for subtype in ["Phyrexian", "Praetor", "Spider", "Human", "Hero"] {
        assert!(
            copied.card_types.subtypes.iter().any(|s| s == subtype),
            "missing subtype {subtype} in {:?}",
            copied.card_types.subtypes
        );
    }

    // (e) Reflexive trigger fired and exiled Elesh Norn from the graveyard.
    // `WhenYouDo` either resolves inline within the parent chain or queues
    // a `PendingTrigger` → CR 603.12 + CR 603.7a. Drain priority passes up
    // to a small bound so the trigger resolves before we assert. Each pass
    // resolves at most one stack item; the cap prevents infinite loops if
    // a new state dead-ends.
    for _ in 0..16 {
        if matches!(state.waiting_for, WaitingFor::Priority { .. }) && state.stack.is_empty() {
            break;
        }
        if apply_as_current(&mut state, GameAction::PassPriority).is_err() {
            break;
        }
    }

    let elesh_obj = state
        .objects
        .get(&elesh)
        .expect("Elesh Norn object still present after exile");
    assert_eq!(
        elesh_obj.zone,
        Zone::Exile,
        "reflexive trigger must exile the copied graveyard card"
    );
}

/// CR 603.12: Focused regression — a reflexive `When you do, …` sub_ability
/// attached to a `BecomeCopy` replacement fires exactly once after the copy
/// resolution, and its `TargetFilter::ParentTarget` resolves to the card the
/// player chose to copy. Scoped to the reflexive path only — no name/P-T
/// modifications, no supertypes, no copied abilities — so a failure
/// diagnoses the CR 603.12 path rather than the surrounding clone-suffix
/// parsing or layer application.
#[test]
fn reflexive_when_you_do_fires_after_become_copy_replacement() {
    let mut state = GameState::new_two_player(42);

    // Plain creature sitting in the opponent's graveyard — the reflexive
    // exile target. No modifiers: we're testing trigger timing and parent
    // target forwarding, not copy mechanics.
    let source_card = zones::create_object(
        &mut state,
        CardId(10),
        PlayerId(1),
        "Bear".to_string(),
        Zone::Graveyard,
    );
    {
        let obj = state.objects.get_mut(&source_card).unwrap();
        obj.base_card_types = crate::types::card_type::CardType {
            supertypes: vec![],
            core_types: vec![CoreType::Creature],
            subtypes: vec![],
        };
    }

    // Cloner: a minimal permanent with BecomeCopy + reflexive "when you
    // do, exile that card" sub_ability. `TargetFilter::ParentTarget`
    // forwards the chosen copy source to the exile step.
    let cloner = zones::create_object(
        &mut state,
        CardId(11),
        PlayerId(0),
        "Test Cloner".to_string(),
        Zone::Battlefield,
    );
    {
        let obj = state.objects.get_mut(&cloner).unwrap();
        obj.base_card_types = crate::types::card_type::CardType {
            supertypes: vec![],
            core_types: vec![CoreType::Creature],
            subtypes: vec![],
        };
        let reflexive = crate::types::ability::AbilityDefinition {
            condition: Some(crate::types::ability::AbilityCondition::WhenYouDo),
            ..crate::types::ability::AbilityDefinition::new(
                crate::types::ability::AbilityKind::Spell,
                Effect::ChangeZone {
                    origin: None,
                    destination: Zone::Exile,
                    target: TargetFilter::ParentTarget,
                    owner_library: false,
                    enter_transformed: false,
                    enters_under: None,
                    enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                    enters_attacking: false,
                    up_to: false,
                    enter_with_counters: vec![],
                    conditional_enter_with_counters: vec![],
                    face_down_profile: None,
                    enters_modified_if: None,
                },
            )
        };
        let become_copy = crate::types::ability::AbilityDefinition::new(
            crate::types::ability::AbilityKind::Spell,
            Effect::BecomeCopy {
                recipient: TargetFilter::SelfRef,
                target: TargetFilter::Typed(
                    crate::types::ability::TypedFilter::new(
                        crate::types::ability::TypeFilter::Creature,
                    )
                    .properties(vec![
                        crate::types::ability::FilterProp::InZone {
                            zone: Zone::Graveyard,
                        },
                    ]),
                ),
                duration: None,
                mana_value_limit: None,
                additional_modifications: vec![],
            },
        )
        .sub_ability(reflexive);
        obj.replacement_definitions.push(
            crate::types::ability::ReplacementDefinition::new(
                crate::types::replacements::ReplacementEvent::Moved,
            )
            .execute(become_copy),
        );
    }

    // Enter directly into the post-copy-choice waiting state — the
    // preceding "enter as a copy of" pause is covered by other tests;
    // here we isolate the reflexive resolution.
    state.waiting_for = WaitingFor::CopyTargetChoice {
        player: PlayerId(0),
        source_id: cloner,
        valid_targets: vec![source_card],
        max_mana_value: None,
    };

    // Accumulate events across the full resolution so we can count
    // exile transitions — CR 603.12a requires the reflexive to fire
    // exactly once per trigger event, and exiling an already-exiled
    // card is a no-op zone move that would silently mask double-firing
    // if we only asserted on end-state.
    let mut all_events: Vec<GameEvent> = Vec::new();

    let result = apply_as_current(
        &mut state,
        GameAction::ChooseTarget {
            target: Some(TargetRef::Object(source_card)),
        },
    )
    .expect("copy target choice should resolve");
    all_events.extend(result.events);

    // Drain priority passes until the reflexive trigger has resolved.
    // CR 603.12: the reflexive is created during the replacement's
    // resolution and fires based on the "choose and copy" event that
    // already occurred. Cap drained iterations — if we hit the cap the
    // loop never reached Priority + empty stack and the test must fail
    // loudly rather than silently proceeding.
    let cap = 16;
    let mut drained = false;
    for _ in 0..cap {
        if matches!(state.waiting_for, WaitingFor::Priority { .. }) && state.stack.is_empty() {
            drained = true;
            break;
        }
        match apply_as_current(&mut state, GameAction::PassPriority) {
            Ok(r) => all_events.extend(r.events),
            Err(_) => {
                drained = true;
                break;
            }
        }
    }
    assert!(
        drained,
        "drain loop exceeded {cap} iterations without reaching \
             Priority + empty stack — reflexive trigger path is stuck"
    );

    // ParentTarget was forwarded: the graveyard card is now exiled.
    let exiled = state
        .objects
        .get(&source_card)
        .expect("source card object preserved after exile");
    assert_eq!(
        exiled.zone,
        Zone::Exile,
        "reflexive `When you do, exile that card` must exile the copy source \
             (TargetFilter::ParentTarget forwarded from BecomeCopy resolution)"
    );

    // CR 603.12a: the reflexive triggers exactly once for the one
    // BecomeCopy event. Count ZoneChanged events moving the source
    // card into exile. A silent double-fire (same source, same dest)
    // would push 2 events here even though the final state is
    // identical, catching regressions that end-state assertions miss.
    let exile_moves = all_events
        .iter()
        .filter(|ev| {
            matches!(ev, GameEvent::ZoneChanged {
                    object_id,
                    to: Zone::Exile,
                    ..
                } if *object_id == source_card)
        })
        .count();
    assert_eq!(
        exile_moves, 1,
        "reflexive must fire exactly once per CR 603.12a; got {exile_moves} exile \
             transitions of the copy source (expected 1)"
    );
}

/// CR 117.1c + CR 509.1 + CR 702.49: When an attacker exists but the defending
/// player has no legal blockers, the declare blockers step still runs and the
/// active player still receives priority during it. This window is what makes
/// Ninjutsu-family activations (notably Sneak, CR 702.49 variant — restricted
/// to this step only) reachable when attacking into an empty board.
#[test]
fn declare_blockers_grants_ap_priority_when_no_legal_blockers() {
    let mut state = new_game(42);
    state.turn_number = 2;
    state.phase = Phase::DeclareAttackers;
    state.active_player = PlayerId(0);
    state.priority_player = PlayerId(0);

    let attacker = create_object(
        &mut state,
        CardId(500),
        PlayerId(0),
        "Attacker".to_string(),
        Zone::Battlefield,
    );
    {
        let obj = state.objects.get_mut(&attacker).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.power = Some(2);
        obj.toughness = Some(2);
        obj.entered_battlefield_turn = Some(1);
    }
    // Defender has no creatures — no legal blocks.

    state.waiting_for = WaitingFor::DeclareAttackers {
        player: PlayerId(0),
        valid_attacker_ids: vec![attacker],
        valid_attack_targets: vec![AttackTarget::Player(PlayerId(1))],
        valid_attack_targets_by_attacker: None,
        attacker_constraints: Default::default(),
    };

    apply_as_current(
        &mut state,
        GameAction::DeclareAttackers {
            attacks: vec![(attacker, AttackTarget::Player(PlayerId(1)))],
            bands: vec![],
        },
    )
    .unwrap();

    // AP passes in DeclareAttackers; NAP passes; engine advances into
    // DeclareBlockers, auto-submits empty blockers (nothing to choose),
    // and — per CR 117.1c — hands priority back to the active player
    // *during the declare blockers step*.
    apply_as_current(&mut state, GameAction::PassPriority).unwrap();
    let result = apply_as_current(&mut state, GameAction::PassPriority).unwrap();

    assert_eq!(
        state.phase,
        Phase::DeclareBlockers,
        "step should be declare blockers, not skipped past"
    );
    assert!(
        matches!(
            result.waiting_for,
            WaitingFor::Priority {
                player: PlayerId(0)
            }
        ),
        "active player must receive priority in declare blockers step \
             (CR 117.1c) so they can activate Sneak (CR 702.49); got {:?}",
        result.waiting_for
    );
}

// ---- CR 702.24a: Cumulative upkeep end-to-end (Mystic Remora) ----------
//
// These tests exercise the full pipeline from "upkeep trigger fires" to
// "controller pays or sacrifices":
//   1. Synthesized trigger (PayCumulativeUpkeep, Phase=Upkeep, valid_target
//      Controller) fires when the controller's upkeep step begins.
//   2. Outer `Effect::PutCounter { CounterType::Age }` ticks the counter
//      on the source before the sub-ability runs.
//   3. Sub-ability `Effect::Sacrifice` carries `unless_pay` =
//      `AbilityCost::PerCounter { Age, SelfRef, base }`, which expands at
//      resolution time to `Mana { N × base }`.
//   4. Player answers `PayUnlessCost { pay: bool }` — pay keeps the
//      permanent, decline sacrifices it.
//
// Closest precedent: `setup_esper_sentinel_unless_payment` (CR 118.12 tax
// trigger) — same `auto_advance` → `resolve_top` → `PayUnlessCost`
// scaffolding. The Mystic Remora flow differs only in how the trigger is
// sourced (synthesized by Keyword::CumulativeUpkeep, not parsed) and in
// the `PerCounter` expansion that lives in the sub-ability's unless-cost.

fn cumulative_upkeep_exile_top_trigger() -> TriggerDefinition {
    crate::database::synthesis::build_cumulative_upkeep_trigger(AbilityCost::Exile {
        count: 1,
        zone: Some(Zone::Library),
        filter: None,
    })
}

fn setup_top_library_exile_upkeep_state(
    preloaded_age_counters: u32,
    library_count: u32,
) -> (GameState, ObjectId, Vec<ObjectId>) {
    let mut state = new_game(42);
    state.turn_number = 2;
    state.phase = Phase::Untap;
    state.active_player = PlayerId(0);
    state.priority_player = PlayerId(0);

    let mut library_cards = Vec::new();
    for index in 0..library_count {
        library_cards.push(create_object(
            &mut state,
            CardId(8000 + u64::from(index)),
            PlayerId(0),
            format!("Library Card {}", index + 1),
            Zone::Library,
        ));
    }

    let source = create_object(
        &mut state,
        CardId(70240),
        PlayerId(0),
        "Top-Library Exile Upkeep".to_string(),
        Zone::Battlefield,
    );
    {
        let obj = state.objects.get_mut(&source).unwrap();
        obj.card_types.core_types.push(CoreType::Enchantment);
        obj.trigger_definitions
            .push(cumulative_upkeep_exile_top_trigger());
        if preloaded_age_counters > 0 {
            obj.counters.insert(
                crate::types::counter::CounterType::Age,
                preloaded_age_counters,
            );
        }
    }

    (state, source, library_cards)
}

fn install_optional_exile_move_replacement(state: &mut GameState, card_id: ObjectId) {
    let replacement_source = create_object(
        state,
        CardId(70241),
        PlayerId(0),
        "Optional Exile Move Replacement".to_string(),
        Zone::Battlefield,
    );
    let obj = state.objects.get_mut(&replacement_source).unwrap();
    obj.card_types.core_types.push(CoreType::Enchantment);
    obj.replacement_definitions.push(
        ReplacementDefinition::new(ReplacementEvent::Moved)
            .mode(ReplacementMode::Optional { decline: None })
            .valid_card(TargetFilter::SpecificObject { id: card_id })
            .destination_zone(Zone::Exile),
    );
}

#[test]
fn top_library_exile_cumulative_upkeep_exiles_top_cards_and_keeps_permanent() {
    let (mut state, source, library_cards) = setup_top_library_exile_upkeep_state(1, 3);

    advance_to_unless_payment_prompt(&mut state);

    match &state.waiting_for {
        WaitingFor::UnlessPayment { player, cost, .. } => {
            assert_eq!(*player, PlayerId(0));
            assert_eq!(
                    cost,
                    &AbilityCost::Exile {
                        count: 2,
                        zone: Some(Zone::Library),
                        filter: None,
                    },
                    "one preloaded age counter plus the upkeep tick should require exiling two top cards"
                );
        }
        other => panic!("expected UnlessPayment for top-library exile, got {other:?}"),
    }

    apply_as_current(&mut state, GameAction::PayUnlessCost { pay: true })
        .expect("top-library exile cumulative-upkeep cost should be payable");

    assert_eq!(state.objects[&source].zone, Zone::Battlefield);
    assert_eq!(state.objects[&library_cards[0]].zone, Zone::Exile);
    assert_eq!(state.objects[&library_cards[1]].zone, Zone::Exile);
    assert_eq!(state.objects[&library_cards[2]].zone, Zone::Library);
    assert_eq!(
        state.players[0].library.front().copied(),
        Some(library_cards[2]),
        "the third card should become the new library top after paying"
    );
}

#[test]
fn top_library_exile_cumulative_upkeep_sacrifices_when_library_payment_unpayable() {
    let (mut state, source, library_cards) = setup_top_library_exile_upkeep_state(1, 1);

    advance_to_unless_payment_prompt(&mut state);

    match &state.waiting_for {
        WaitingFor::UnlessPayment { cost, .. } => assert_eq!(
            cost,
            &AbilityCost::Exile {
                count: 2,
                zone: Some(Zone::Library),
                filter: None,
            }
        ),
        other => panic!("expected UnlessPayment for top-library exile, got {other:?}"),
    }

    apply_as_current(&mut state, GameAction::PayUnlessCost { pay: true })
        .expect("unpayable top-library exile cost should fall through to sacrifice");

    assert_eq!(
            state.objects[&source].zone,
            Zone::Graveyard,
            "partial cumulative-upkeep payments are not allowed; too few library cards sacrifices the permanent"
        );
    assert_eq!(
        state.objects[&library_cards[0]].zone,
        Zone::Library,
        "failed payment must not partially exile the available top card"
    );
}

#[test]
fn top_library_exile_cumulative_upkeep_replacement_choice_is_atomic() {
    let (mut state, source, library_cards) = setup_top_library_exile_upkeep_state(1, 3);
    install_optional_exile_move_replacement(&mut state, library_cards[1]);

    advance_to_unless_payment_prompt(&mut state);

    apply_as_current(&mut state, GameAction::PayUnlessCost { pay: true })
        .expect("choice-based top-library exile cost should fall through to sacrifice");

    assert_eq!(
        state.objects[&source].zone,
        Zone::Graveyard,
        "a choice-based replacement makes the deterministic cumulative-upkeep payment fail"
    );
    assert_eq!(
        state.objects[&library_cards[0]].zone,
        Zone::Library,
        "failed payment must not partially exile the first top card"
    );
    assert_eq!(
        state.objects[&library_cards[1]].zone,
        Zone::Library,
        "failed payment must not partially exile later top cards"
    );
    assert_eq!(
        state.players[0].library.front().copied(),
        Some(library_cards[0]),
        "choice-based payment failure leaves library order untouched"
    );
    assert!(
        state.pending_replacement.is_none(),
        "abandoned deterministic payment must not leave a replacement choice pending"
    );
}

/// Build the synthesized cumulative-upkeep trigger for "Cumulative upkeep
/// {N}" (mana base cost) by delegating to the production synthesizer.
/// Binding the end-to-end tests to the real builder ensures any regression
/// in `build_cumulative_upkeep_trigger` (e.g., flipping AddCounter →
/// Sacrifice ordering, dropping `.phase(Upkeep)`, or changing the
/// PerCounter payer) breaks the Mystic Remora pipeline tests loudly
/// rather than silently passing against a stale inline mirror.
fn cumulative_upkeep_mana_trigger(generic: u32) -> TriggerDefinition {
    crate::database::synthesis::build_cumulative_upkeep_trigger(AbilityCost::Mana {
        cost: ManaCost::generic(generic),
    })
}

/// Construct a solo state with Mystic Remora on the battlefield,
/// controller = PlayerId(0) = active player, at Phase::Untap so
/// `auto_advance` will fire the upkeep trigger.
fn setup_mystic_remora_upkeep_state() -> (GameState, ObjectId) {
    let mut state = new_game(42);
    state.turn_number = 2;
    state.phase = Phase::Untap;
    state.active_player = PlayerId(0);
    state.priority_player = PlayerId(0);

    let remora = create_object(
        &mut state,
        CardId(7024),
        PlayerId(0),
        "Mystic Remora".to_string(),
        Zone::Battlefield,
    );
    {
        let obj = state.objects.get_mut(&remora).unwrap();
        obj.card_types.core_types.push(CoreType::Enchantment);
        obj.trigger_definitions
            .push(cumulative_upkeep_mana_trigger(1));
    }

    (state, remora)
}

/// Advance from Untap through Upkeep, fire the cumulative-upkeep trigger,
/// and resolve it. Mirrors the Esper Sentinel pattern of `auto_advance`
/// (to populate the stack) then `resolve_top` (to walk the outer
/// AddCounter → sub-ability Sacrifice/PerCounter chain into
/// `WaitingFor::UnlessPayment`).
fn advance_to_unless_payment_prompt(state: &mut GameState) {
    let mut events = Vec::new();
    let _wf = crate::game::turns::auto_advance(state, &mut events);
    // CR 503.1a: the trigger landed on the stack during Phase::Upkeep.
    assert_eq!(state.phase, Phase::Upkeep);
    assert!(
        !state.stack.is_empty(),
        "cumulative-upkeep trigger must be on the stack after auto_advance"
    );
    crate::game::stack::resolve_top(state, &mut events);
}

/// Give PlayerId(0) `generic` colorless mana units so they can satisfy a
/// `Mana { generic: N }` unless-cost. Mirrors the `mana_pool.add` idiom
/// used by `setup_esper_sentinel_unless_payment`.
fn give_p0_colorless_mana(state: &mut GameState, generic: u32) {
    let p0 = state
        .players
        .iter_mut()
        .find(|p| p.id == PlayerId(0))
        .expect("PlayerId(0)");
    for _ in 0..generic {
        p0.mana_pool.add(ManaUnit::new(
            ManaType::Colorless,
            ObjectId(0),
            false,
            vec![],
        ));
    }
}

/// Reset `phase`, `active_player`, `priority_player`, `stack`,
/// `pending_trigger`, and `waiting_for` so the next `auto_advance`
/// re-enters PlayerId(0)'s upkeep and re-fires the cumulative-upkeep
/// trigger. The age counter on `remora` persists across this transition
/// (counters live on the object and outlive phase changes), which is
/// exactly the CR 702.24a "accumulates each upkeep" invariant under
/// test.
///
/// Does NOT clear per-turn bookkeeping (`priority_passes`,
/// `spells_cast_this_turn`, `spells_cast_this_turn_by_player`,
/// `pending_trigger_event_batch`, etc.) — safe for cumulative-upkeep
/// tests that never pass priority or cast spells mid-test. Tasks 10-13
/// (Polar Kraken, Inner Sanctum, source-gone, multi-instance) must
/// re-evaluate this scope if their flow does either; expanding the
/// resets is preferable to silent state drift.
fn rewind_to_next_p0_upkeep(state: &mut GameState) {
    state.turn_number += 2;
    state.phase = Phase::Untap;
    state.active_player = PlayerId(0);
    state.priority_player = PlayerId(0);
    state.stack.clear();
    state.pending_trigger = None;
    // CR 603.3c + CR 603.3d: clear the in-construction cursor too —
    // symmetric with `pending_trigger`. Without this, a trigger pushed
    // earlier in the test could leave `pending_trigger_entry` pointing
    // to a now-cleared `state.stack`, tripping the push-first invariants
    // on the next trigger.
    state.pending_trigger_entry = None;
    state.waiting_for = WaitingFor::Priority {
        player: PlayerId(0),
    };
}

/// CR 702.24a + CR 118.12: Paying the cumulative-upkeep cost keeps the
/// permanent on the battlefield. Verifies the age counter ticks first
/// (outer AddCounter resolves before the sub-ability), the prompt expands
/// to `Mana{1}` (1 counter × base {1}), and the post-pay state has the
/// permanent still on the battlefield with the age counter intact.
#[test]
fn mystic_remora_upkeep_pay_path_keeps_permanent_and_adds_age_counter() {
    let (mut state, remora_id) = setup_mystic_remora_upkeep_state();

    advance_to_unless_payment_prompt(&mut state);
    // CR 500.5: mana pools empty between phases. Add the unless-cost
    // payment AFTER `auto_advance` settles in Upkeep so the mana persists
    // through to `PayUnlessCost` (mirrors what real play models: the
    // controller would tap a land in response to the trigger).
    give_p0_colorless_mana(&mut state, 1);

    // CR 702.24a: outer AddCounter resolved first, so the counter exists
    // before the per-counter unless-cost is computed.
    assert_eq!(
        state.objects[&remora_id]
            .counters
            .get(&crate::types::counter::CounterType::Age)
            .copied(),
        Some(1),
        "age counter must be added before the unless-pay prompt"
    );

    // CR 118.12 + CR 702.24a: PerCounter expanded to {1} for 1 age counter.
    match &state.waiting_for {
        WaitingFor::UnlessPayment { player, cost, .. } => {
            assert_eq!(*player, PlayerId(0), "controller is the unless-payer");
            match cost {
                AbilityCost::Mana { cost: mana } => {
                    assert_eq!(
                        *mana,
                        ManaCost::generic(1),
                        "1 age counter × base {{1}} = {{1}}"
                    );
                }
                other => panic!("expected Mana cost, got {other:?}"),
            }
        }
        other => panic!("expected UnlessPayment prompt, got {other:?}"),
    }

    let _ = apply_as_current(&mut state, GameAction::PayUnlessCost { pay: true }).unwrap();

    // CR 702.24a: paying the cost keeps the permanent on the battlefield.
    assert_eq!(
        state.objects[&remora_id].zone,
        Zone::Battlefield,
        "paying the cumulative-upkeep cost must NOT sacrifice the permanent"
    );
    assert!(
        !state.players[0].graveyard.contains(&remora_id),
        "permanent must not be in graveyard when paid"
    );
}

/// CR 702.24a + CR 118.12: Declining the cumulative-upkeep cost sacrifices
/// the permanent. The sub-ability's `Effect::Sacrifice` runs because the
/// player chose not to pay; the source moves to its controller's
/// graveyard.
#[test]
fn mystic_remora_upkeep_decline_path_sacrifices() {
    let (mut state, remora_id) = setup_mystic_remora_upkeep_state();

    advance_to_unless_payment_prompt(&mut state);

    let _ = apply_as_current(&mut state, GameAction::PayUnlessCost { pay: false }).unwrap();

    // CR 701.21a: To sacrifice a permanent, its controller moves it from
    // the battlefield directly to its owner's graveyard.
    assert!(
        state.players[0].graveyard.contains(&remora_id),
        "declining the unless-cost must sacrifice the permanent; graveyard={:?}",
        state.players[0].graveyard
    );
    assert_ne!(
        state.objects[&remora_id].zone,
        Zone::Battlefield,
        "permanent must leave the battlefield on decline"
    );
}

/// CR 702.24a: "...put an age counter on it. Then sacrifice it unless you
/// pay its upkeep cost for each age counter on it." Three consecutive
/// upkeeps with payment must yield costs {1}, {2}, {3} (1, 2, 3 counters
/// respectively) and three age counters at the end. This is the
/// load-bearing test for the `PerCounter` expansion: it confirms that
/// each tick of the counter strictly precedes the cost computation, and
/// that counters accumulate across turns.
#[test]
fn mystic_remora_three_upkeeps_costs_one_two_three() {
    let (mut state, remora_id) = setup_mystic_remora_upkeep_state();

    for (turn_idx, expected_generic) in [1u32, 2, 3].iter().enumerate() {
        advance_to_unless_payment_prompt(&mut state);
        // CR 500.5: mana pools empty between phases. Provide the unless-
        // cost payment AFTER `auto_advance` settles in Upkeep so the
        // mana survives into `PayUnlessCost`.
        give_p0_colorless_mana(&mut state, *expected_generic);

        // The age counter for THIS upkeep is already in place when we
        // reach the unless-pay prompt — counter total is turn_idx + 1.
        let expected_counters = (turn_idx + 1) as u32;
        assert_eq!(
            state.objects[&remora_id]
                .counters
                .get(&crate::types::counter::CounterType::Age)
                .copied(),
            Some(expected_counters),
            "upkeep {turn_idx}: expected {expected_counters} age counter(s) before payment"
        );

        match &state.waiting_for {
            WaitingFor::UnlessPayment {
                cost: AbilityCost::Mana { cost: mana },
                ..
            } => {
                assert_eq!(
                    *mana,
                    ManaCost::generic(*expected_generic),
                    "upkeep {turn_idx}: expected Mana({{{expected_generic}}}), got {mana:?}"
                );
            }
            other => {
                panic!("upkeep {turn_idx}: expected Mana unless-payment prompt, got {other:?}")
            }
        }

        let _ = apply_as_current(&mut state, GameAction::PayUnlessCost { pay: true }).unwrap();
        assert_eq!(
            state.objects[&remora_id].zone,
            Zone::Battlefield,
            "upkeep {turn_idx}: paying keeps the permanent on the battlefield"
        );

        // Reset to next controller upkeep for the next iteration.
        if turn_idx < 2 {
            rewind_to_next_p0_upkeep(&mut state);
        }
    }

    // CR 702.24a: counters strictly accumulate. After three paid upkeeps,
    // the permanent carries three age counters.
    assert_eq!(
        state.objects[&remora_id]
            .counters
            .get(&crate::types::counter::CounterType::Age)
            .copied(),
        Some(3),
        "three age counters must have accumulated across three upkeeps"
    );
}

/// Build the synthesized cumulative-upkeep trigger for "Cumulative upkeep
/// — Sacrifice a land" (Polar Kraken's sacrifice-cost variant) by delegating
/// to the production synthesizer. Mirrors `cumulative_upkeep_mana_trigger`
/// (which exercises the `Mana` arm of `expand_per_counter`); this helper
/// exercises the `Sacrifice` arm. Binding to the real builder ensures any
/// regression in `build_cumulative_upkeep_trigger`'s handling of a
/// non-Mana base cost (chained-ability ordering, PerCounter payer,
/// `.phase(Upkeep)` gating) breaks the Polar Kraken pipeline test loudly.
///
/// CR 702.24a: cumulative upkeep cost format is `[cost]` where `[cost]`
/// may be any cost. Sacrifice-a-land is the canonical non-mana variant
/// (Polar Kraken, Phyrexian Soulgorger).
use crate::types::ability::SacrificeCost;

fn cumulative_upkeep_sacrifice_land_trigger() -> TriggerDefinition {
    crate::database::synthesis::build_cumulative_upkeep_trigger(AbilityCost::Sacrifice(
        SacrificeCost::count(TargetFilter::Typed(TypedFilter::land()), 1),
    ))
}

/// Construct a solo state with Polar Kraken on the battlefield (controller
/// = PlayerId(0) = active player) plus three Forests for sacrifice fodder,
/// at Phase::Untap so `auto_advance` will fire the upkeep trigger. The
/// three-forest count is deliberate: the test sacrifices exactly one, and
/// the surviving two prove that `handle_unless_payment_sacrifice`'s
/// eligible-permanents collection didn't over-sacrifice or sacrifice the
/// wrong land.
///
/// Returns `(state, kraken_id, [forest0, forest1, forest2])`.
fn setup_polar_kraken_upkeep_state() -> (GameState, ObjectId, Vec<ObjectId>) {
    let mut state = new_game(42);
    state.turn_number = 2;
    state.phase = Phase::Untap;
    state.active_player = PlayerId(0);
    state.priority_player = PlayerId(0);

    let kraken = create_object(
        &mut state,
        CardId(7100),
        PlayerId(0),
        "Polar Kraken".to_string(),
        Zone::Battlefield,
    );
    {
        let obj = state.objects.get_mut(&kraken).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.card_types.subtypes.push("Kraken".to_string());
        obj.trigger_definitions
            .push(cumulative_upkeep_sacrifice_land_trigger());
    }

    let mut forests = Vec::with_capacity(3);
    for i in 0..3 {
        let forest = create_object(
            &mut state,
            CardId(7101 + i),
            PlayerId(0),
            "Forest".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&forest).unwrap();
            obj.card_types.core_types.push(CoreType::Land);
            obj.card_types.subtypes.push("Forest".to_string());
        }
        forests.push(forest);
    }

    (state, kraken, forests)
}

/// CR 702.24a + CR 118.12 + CR 701.21: Paying the cumulative-upkeep cost
/// via the sacrifice-a-land variant. At counter=1, the per-counter expansion
/// of `Sacrifice { Land, count: 1 }` yields `Sacrifice { Land, count: 1 }`
/// (1 × 1 = 1), and paying by sacrificing one of three controlled forests
/// keeps Polar Kraken on the battlefield with one forest in the graveyard
/// and two untouched. This is the structural-identity case for the
/// `Sacrifice` arm of `expand_per_counter` — Mystic Remora's three-upkeep
/// test already covers the multiplicative case for the `Mana` arm.
#[test]
fn polar_kraken_upkeep_sacrifice_cost_path() {
    let (mut state, kraken_id, forest_ids) = setup_polar_kraken_upkeep_state();
    advance_to_unless_payment_prompt(&mut state);

    // CR 702.24a: outer AddCounter resolved first, so one age counter
    // sits on the Kraken before the per-counter unless-cost is computed.
    assert_eq!(
        state.objects[&kraken_id]
            .counters
            .get(&crate::types::counter::CounterType::Age)
            .copied(),
        Some(1),
        "age counter must be added before the unless-pay prompt"
    );

    // CR 118.12 + CR 702.24a: PerCounter expanded `Sacrifice { Land, 1 }`
    // for 1 age counter to `Sacrifice { Land, 1 }` (1 × 1 = 1).
    match &state.waiting_for {
        WaitingFor::UnlessPayment { player, cost, .. } => {
            assert_eq!(*player, PlayerId(0), "controller is the unless-payer");
            match cost {
                AbilityCost::Sacrifice(cost) => {
                    assert_eq!(
                        cost.requirement.fixed_count(),
                        Some(1),
                        "1 age counter × base count 1 = 1"
                    );
                    assert_eq!(
                        cost.target,
                        TargetFilter::Typed(TypedFilter::land()),
                        "unless-cost target filter must remain Land"
                    );
                }
                other => panic!("expected Sacrifice cost, got {other:?}"),
            }
        }
        other => panic!("expected UnlessPayment prompt, got {other:?}"),
    }

    // CR 118.12 + CR 701.21: Pay → engine collects eligible controlled
    // Lands and surfaces `WaitingFor::WardSacrificeChoice` for the player
    // to pick which permanent to sacrifice.
    let _ = apply_as_current(&mut state, GameAction::PayUnlessCost { pay: true }).unwrap();
    match &state.waiting_for {
        WaitingFor::WardSacrificeChoice {
            player,
            permanents,
            remaining,
            ..
        } => {
            assert_eq!(*player, PlayerId(0), "controller picks the sacrifice");
            assert_eq!(*remaining, 1, "exactly one sacrifice required");
            assert_eq!(
                permanents.len(),
                3,
                "all three controlled forests must be eligible"
            );
            for fid in &forest_ids {
                assert!(
                    permanents.contains(fid),
                    "forest {fid:?} must be an eligible sacrifice"
                );
            }
        }
        other => panic!("expected WardSacrificeChoice prompt, got {other:?}"),
    }

    // CR 701.21: Choose the first forest as the sacrifice victim.
    let _ = apply_as_current(
        &mut state,
        GameAction::SelectCards {
            cards: vec![forest_ids[0]],
        },
    )
    .unwrap();

    // CR 702.24a: paying the cost keeps the permanent on the battlefield.
    assert_eq!(
        state.objects[&kraken_id].zone,
        Zone::Battlefield,
        "paying the cumulative-upkeep cost must NOT sacrifice the Kraken"
    );
    // CR 701.21a: To sacrifice a permanent, its controller moves it from
    // the battlefield directly to its owner's graveyard.
    assert_eq!(
        state.objects[&forest_ids[0]].zone,
        Zone::Graveyard,
        "the chosen forest must be in the graveyard"
    );
    assert!(
        state.players[0].graveyard.contains(&forest_ids[0]),
        "graveyard must contain the sacrificed forest"
    );
    // The two unchosen forests stay on the battlefield — proves the
    // sacrifice path didn't over-select.
    assert_eq!(
        state.objects[&forest_ids[1]].zone,
        Zone::Battlefield,
        "unchosen forest 1 must remain on the battlefield"
    );
    assert_eq!(
        state.objects[&forest_ids[2]].zone,
        Zone::Battlefield,
        "unchosen forest 2 must remain on the battlefield"
    );
}

/// Build the synthesized cumulative-upkeep trigger for "Cumulative upkeep
/// — Pay 2 life" (Inner Sanctum's life-cost variant) by delegating to the
/// production synthesizer. Mirrors `cumulative_upkeep_mana_trigger` and
/// `cumulative_upkeep_sacrifice_land_trigger`; this helper exercises the
/// `PayLife` arm of `expand_per_counter` (CR 702.24a + CR 119.4).
fn cumulative_upkeep_pay_life_trigger(amount: i32) -> TriggerDefinition {
    crate::database::synthesis::build_cumulative_upkeep_trigger(AbilityCost::PayLife {
        amount: QuantityExpr::Fixed { value: amount },
    })
}

/// Construct a solo state with Inner Sanctum on the battlefield (controller
/// = PlayerId(0) = active player) at Phase::Untap so `auto_advance` will
/// fire the upkeep trigger. **One age counter is pre-loaded** on Inner
/// Sanctum so the first upkeep that `auto_advance` resolves ticks the
/// counter from 1 → 2 — exercising the multiplicative step of the
/// `PayLife` arm of `expand_per_counter` (base 2 × counter 2 = 4 life).
/// This skips the structurally-trivial counter=1 case, which the Polar
/// Kraken sacrifice test already covers for the non-Mana arm.
fn setup_inner_sanctum_second_upkeep_state() -> (GameState, ObjectId) {
    let mut state = new_game(42);
    state.turn_number = 2;
    state.phase = Phase::Untap;
    state.active_player = PlayerId(0);
    state.priority_player = PlayerId(0);

    let sanctum = create_object(
        &mut state,
        CardId(7200),
        PlayerId(0),
        "Inner Sanctum".to_string(),
        Zone::Battlefield,
    );
    {
        let obj = state.objects.get_mut(&sanctum).unwrap();
        obj.card_types.core_types.push(CoreType::Enchantment);
        obj.trigger_definitions
            .push(cumulative_upkeep_pay_life_trigger(2));
        // CR 702.24a: pre-load one age counter so the next upkeep tick
        // produces counter=2, yielding the per-counter expansion
        // PayLife{2 × 2} = PayLife{4}.
        obj.counters
            .insert(crate::types::counter::CounterType::Age, 1);
    }

    (state, sanctum)
}

/// CR 702.24a + CR 118.12 + CR 119.4: Paying the cumulative-upkeep cost
/// via the pay-life variant at counter=2. Pre-loading one age counter
/// means the second upkeep ticks the counter from 1 → 2, and the
/// `PerCounter` expansion of `PayLife { Fixed(2) }` yields
/// `PayLife { Fixed(4) }` (2 × 2 = 4 life). Paying 4 life keeps Inner
/// Sanctum on the battlefield and deducts 4 from the controller's life
/// total — the load-bearing assertion for the `PayLife` arm of
/// `expand_per_counter`'s `QuantityExpr::scaled_by` composition.
#[test]
fn inner_sanctum_upkeep_two_age_counters_pays_four_life() {
    let (mut state, sanctum_id) = setup_inner_sanctum_second_upkeep_state();
    advance_to_unless_payment_prompt(&mut state);

    // CR 702.24a: outer AddCounter resolved first; the pre-loaded counter
    // ticked from 1 → 2 before the per-counter unless-cost is computed.
    assert_eq!(
        state.objects[&sanctum_id]
            .counters
            .get(&crate::types::counter::CounterType::Age)
            .copied(),
        Some(2),
        "age counter should tick from 1 (pre-loaded) to 2 on this upkeep"
    );

    // CR 118.12 + CR 702.24a + CR 119.4: PerCounter expanded
    // `PayLife { Fixed(2) }` for 2 age counters to `PayLife { Fixed(4) }`
    // (2 × 2 = 4). This is the load-bearing multiplicative assertion for
    // the `PayLife` arm of `expand_per_counter`.
    match &state.waiting_for {
        WaitingFor::UnlessPayment { player, cost, .. } => {
            assert_eq!(*player, PlayerId(0), "controller is the unless-payer");
            match cost {
                AbilityCost::PayLife { amount } => {
                    assert_eq!(
                        *amount,
                        QuantityExpr::Fixed { value: 4 },
                        "2 age counters × base 2 life = 4 life"
                    );
                }
                other => panic!("expected PayLife cost, got {other:?}"),
            }
        }
        other => panic!("expected UnlessPayment prompt, got {other:?}"),
    }

    // CR 119.4: pay-life unless-costs are auto-deducted from the player's
    // life total at `PayUnlessCost { pay: true }` time — no intermediate
    // choice prompt (unlike Sacrifice, which surfaces a permanent
    // picker). Snapshot the life total before paying so the delta is
    // measurable.
    let life_before = state.players[0].life;
    let _ = apply_as_current(&mut state, GameAction::PayUnlessCost { pay: true }).unwrap();

    // CR 119.4: 4 life paid → life total decreases by exactly 4.
    assert_eq!(
        state.players[0].life,
        life_before - 4,
        "paying 4 life must reduce life total by 4"
    );
    // CR 702.24a: paying the cost keeps the permanent on the battlefield.
    assert_eq!(
        state.objects[&sanctum_id].zone,
        Zone::Battlefield,
        "paying the cumulative-upkeep cost must NOT sacrifice the permanent"
    );
    assert!(
        !state.players[0].graveyard.contains(&sanctum_id),
        "permanent must not be in graveyard when paid"
    );
}

/// CR 702.24a + CR 603.4 + CR 400.7: "if this permanent is on the
/// battlefield" is an intervening-if condition re-checked at trigger
/// resolution. If the source permanent has left the battlefield between
/// trigger fire and resolution (bounced, exiled, etc.), the entire
/// chained ability no-ops: no age counter is placed, no unless-pay prompt
/// is emitted, and no sacrifice occurs.
///
/// This is the regression test for the cumulative-upkeep
/// `TriggerCondition::SourceInZone { Battlefield }` guard wired in
/// `build_cumulative_upkeep_trigger`. Without that guard, the trigger
/// would resolve against the (now-hand-zone) source object: the outer
/// `Effect::PutCounter` would still write an age counter onto the object
/// in hand, and the sub-ability would still prompt the controller with a
/// `Mana{1}` unless-payment — a spurious prompt fundamentally inconsistent
/// with CR 702.24a.
///
/// The flow exercises the resolution-time re-evaluation specifically:
///   1. `auto_advance` from Untap into Upkeep, firing the trigger onto the
///      stack (source is still on the battlefield at fire-time, so the
///      intervening-if passes).
///   2. Move the source to hand (simulates a bounce spell resolving on
///      top of the upkeep trigger).
///   3. `resolve_top` should see the condition fail at resolution time
///      (per `stack::resolve_top`'s CR 603.4 re-check) and walk away
///      without invoking the AddCounter → sub-ability chain.
#[test]
fn cumulative_upkeep_source_gone_before_resolution_is_noop() {
    let (mut state, remora_id) = setup_mystic_remora_upkeep_state();

    // Step 1: fire the trigger onto the stack but DO NOT resolve it.
    // `auto_advance` settles in Phase::Upkeep with the trigger queued.
    let mut events = Vec::new();
    let _wf = crate::game::turns::auto_advance(&mut state, &mut events);
    assert_eq!(
        state.phase,
        Phase::Upkeep,
        "auto_advance must pause in Upkeep with the trigger queued"
    );
    assert!(
        !state.stack.is_empty(),
        "cumulative-upkeep trigger must be on the stack pre-bounce"
    );
    // Source is still on the battlefield at fire-time and has no age
    // counter yet (outer AddCounter resolves at stack resolution).
    assert_eq!(state.objects[&remora_id].zone, Zone::Battlefield);
    assert_eq!(
        state.objects[&remora_id]
            .counters
            .get(&crate::types::counter::CounterType::Age)
            .copied()
            .unwrap_or(0),
        0,
        "no age counter before stack resolution"
    );

    // Step 2: bounce the source to its owner's hand. In real play this
    // would be a Boomerang or Unsummon resolving on top of the upkeep
    // trigger. We move it directly to keep the test focused on the
    // intervening-if re-check at resolution time.
    // CR 400.7: this conceptually creates a new object in the hand zone;
    // here ObjectId is preserved (engine maintains object identity in the
    // `objects` map across zone changes), which is the harder case for
    // the no-op semantics — the same id remains addressable.
    crate::game::zones::move_to_zone(&mut state, remora_id, Zone::Hand, &mut events);
    assert_eq!(
        state.objects[&remora_id].zone,
        Zone::Hand,
        "source must be in hand after bounce"
    );

    // Step 3: resolve the top of the stack. The
    // `TriggerCondition::SourceInZone { Battlefield }` re-check should
    // fail (source is in Hand now), so `stack::resolve_top` emits
    // `StackResolved` without invoking the outer AddCounter or the
    // sub-ability chain.
    crate::game::stack::resolve_top(&mut state, &mut events);

    // No unless-payment prompt — the chain never reached the sub-ability.
    assert!(
        !matches!(state.waiting_for, WaitingFor::UnlessPayment { .. }),
        "no unless-pay prompt when source has left the battlefield; got: {:?}",
        state.waiting_for
    );

    // No age counter — outer AddCounter never ran.
    assert_eq!(
        state.objects[&remora_id]
            .counters
            .get(&crate::types::counter::CounterType::Age)
            .copied()
            .unwrap_or(0),
        0,
        "no age counter should be placed when the intervening-if no-ops"
    );

    // Source stays in hand. Not sacrificed, not returned to battlefield.
    assert_eq!(
        state.objects[&remora_id].zone,
        Zone::Hand,
        "source must remain in hand; no Effect::Sacrifice ran"
    );
    assert!(
        !state.players[0].graveyard.contains(&remora_id),
        "source must not be sacrificed to graveyard when the chain no-ops"
    );

    // The trigger left the stack via the CR 603.4 no-op exit, not via
    // normal resolution — stack is now empty.
    assert!(
        state.stack.is_empty(),
        "stack must be cleared after the no-op resolution"
    );
}

/// CR 702.24b: "If a permanent has multiple instances of cumulative
/// upkeep, each triggers separately. However, the age counters are not
/// connected to any particular ability; each cumulative upkeep ability
/// will count the total number of age counters on the permanent at the
/// time that ability resolves."
///
/// Construct a synthetic permanent with TWO `PayCumulativeUpkeep`
/// triggers — a `Mana{1}` base and a `PayLife{1}` base — controlled by
/// PlayerId(0). No real MTG card prints two cumulative-upkeep abilities,
/// so the only way to exercise the shared-counter semantics is to attach
/// both triggers in-test. Returns the perm's id; the controller is
/// PlayerId(0) (active player) and the phase is set so `auto_advance`
/// fires both triggers at upkeep.
fn setup_two_instance_cumulative_upkeep_state() -> (GameState, ObjectId) {
    let mut state = new_game(42);
    state.turn_number = 2;
    state.phase = Phase::Untap;
    state.active_player = PlayerId(0);
    state.priority_player = PlayerId(0);

    let perm = create_object(
        &mut state,
        CardId(7300),
        PlayerId(0),
        "Synthetic Multi-Upkeep Permanent".to_string(),
        Zone::Battlefield,
    );
    {
        let obj = state.objects.get_mut(&perm).unwrap();
        obj.card_types.core_types.push(CoreType::Enchantment);
        // CR 702.24b: each instance triggers separately. Attaching both
        // to the same object is the load-bearing test setup — the
        // production builders are reused unchanged so any regression in
        // `build_cumulative_upkeep_trigger` (counter ordering, payer
        // resolution, intervening-if guard) breaks this test loudly.
        obj.trigger_definitions
            .push(cumulative_upkeep_mana_trigger(1));
        obj.trigger_definitions
            .push(cumulative_upkeep_pay_life_trigger(1));
    }

    (state, perm)
}

/// CR 702.24b + CR 603.3b: Multi-instance cumulative upkeep — two
/// abilities each trigger separately and share the age-counter pool,
/// with each ability reading the running total at its own resolution
/// time. Synthetic permanent carries `Mana{1}` and `PayLife{1}` upkeep
/// triggers. At upkeep, both fire and the controller orders them via
/// `OrderTriggers` (CR 603.3b). Whichever trigger resolves first sees
/// the counter tick 0 → 1 (cost scales × 1); whichever resolves second
/// sees the counter tick 1 → 2 (cost scales × 2, the load-bearing
/// assertion). The stack order is the active player's choice — the
/// test pins ordering via a specific `OrderTriggers` permutation but
/// asserts the cost SET observed across both prompts (×1 paired with
/// ×2), independent of which printed trigger ended up where on the
/// stack. Final state: 2 age counters, no sacrifice, controller paid
/// the ×1 + ×2 multiples of each base across the two prompts.
///
/// This is the load-bearing test for CR 702.24b — the only scenario
/// where the counter pool is read at resolution time (not at trigger
/// fire time) is multi-instance. Single-instance accumulation tests
/// (Mystic Remora three-upkeep) can't distinguish "read at fire" vs
/// "read at resolve" because only one tick happens between fire and
/// resolve. Two triggers in one batch make the distinction observable:
/// if the engine read at fire-time, both prompts would see counter=0;
/// if it read between AddCounter and unless-pay computation (post-tick
/// per trigger), the second prompt sees counter=2.
#[test]
fn cumulative_upkeep_multi_instance_each_ticks_own_counter() {
    let (mut state, perm_id) = setup_two_instance_cumulative_upkeep_state();
    let life_before = state.players[0].life;

    // Step 1: `auto_advance` settles in Upkeep and `process_phase_triggers`
    // collects both PayCumulativeUpkeep triggers. With two triggers from
    // a single controller, the engine prompts P0 to order them via
    // CR 603.3b before any trigger lands on the stack.
    let mut events = Vec::new();
    let _wf = crate::game::turns::auto_advance(&mut state, &mut events);
    assert_eq!(
        state.phase,
        Phase::Upkeep,
        "auto_advance must pause in Upkeep so both triggers can be ordered"
    );
    match &state.waiting_for {
        WaitingFor::OrderTriggers { player, triggers } => {
            assert_eq!(*player, PlayerId(0), "controller orders own triggers");
            assert_eq!(
                triggers.len(),
                2,
                "both cumulative-upkeep triggers must be in the prompt"
            );
        }
        other => panic!("expected OrderTriggers prompt, got {other:?}"),
    }

    // Step 2: CR 603.3b + CR 405.3: Submit a fixed permutation so the
    // stack order is deterministic across runs. The CR 702.24b
    // invariant under test — running-total semantics across two
    // instances — holds regardless of WHICH printed trigger resolves
    // first, so the per-cost assertions below are written against the
    // RESOLUTION ORDER (`first_cost`, `second_cost`), not against the
    // identity of the underlying trigger.
    let _ = apply_as_current(&mut state, GameAction::OrderTriggers { order: vec![1, 0] }).unwrap();
    assert!(
        !state.stack.is_empty(),
        "both triggers must be on the stack after ordering"
    );

    // Step 3: Resolve the top of the stack — the first of two cumulative
    // upkeep triggers. The outer AddCounter ticks the age counter 0 → 1;
    // the sub-ability unless-pay reads counter=1 and expands the base
    // cost × 1 (so Mana{1} → Mana{1}, or PayLife{1} → PayLife{1}).
    let mut events = Vec::new();
    crate::game::stack::resolve_top(&mut state, &mut events);

    // CR 702.24a + CR 702.24b: the first trigger's AddCounter resolved,
    // so the counter is 1 before the unless-pay computes.
    assert_eq!(
        state.objects[&perm_id]
            .counters
            .get(&crate::types::counter::CounterType::Age)
            .copied(),
        Some(1),
        "first resolving trigger must tick counter to 1"
    );
    let first_cost = match &state.waiting_for {
        WaitingFor::UnlessPayment { player, cost, .. } => {
            assert_eq!(*player, PlayerId(0), "controller pays the unless-cost");
            cost.clone()
        }
        other => panic!("expected first UnlessPayment, got {other:?}"),
    };

    // CR 500.5: mana pools empty between phases — add the {1} payment
    // AFTER auto_advance settles in Upkeep so the mana persists into
    // `PayUnlessCost`. The cost shape is asserted in the set-based
    // check below; here we just need to satisfy whichever cost arrived.
    pay_unless_payment_dispatching(&mut state, &first_cost);

    // Step 4: Resolve the next stack entry — the second cumulative
    // upkeep trigger. Counter ticks 1 → 2; the unless-pay reads
    // counter=2 and expands the base cost × 2 (so PayLife{1} →
    // PayLife{2}, or Mana{1} → Mana{2}). This is the load-bearing
    // assertion for CR 702.24b: the second trigger sees the running
    // total, not the value the first trigger started with.
    let mut events = Vec::new();
    crate::game::stack::resolve_top(&mut state, &mut events);

    assert_eq!(
        state.objects[&perm_id]
            .counters
            .get(&crate::types::counter::CounterType::Age)
            .copied(),
        Some(2),
        "second resolving trigger must see post-tick total of 2 \
             (CR 702.24b: shared counter pool, read at resolution time)"
    );
    let second_cost = match &state.waiting_for {
        WaitingFor::UnlessPayment { player, cost, .. } => {
            assert_eq!(*player, PlayerId(0), "controller pays the unless-cost");
            cost.clone()
        }
        other => panic!("expected second UnlessPayment, got {other:?}"),
    };

    // CR 702.24b — the canonical assertion: the cost SET observed
    // across the two prompts must include EXACTLY one ×1-scaled cost
    // (the first trigger to resolve, ticking 0→1) and one ×2-scaled
    // cost (the second trigger to resolve, ticking 1→2). Stack order
    // is the active player's choice per CR 603.3b — both `{Mana{1},
    // PayLife{2}}` and `{PayLife{1}, Mana{2}}` are valid outcomes,
    // distinguished only by which trigger sits on top. The invariant
    // under test is *running-total semantics*: one cost reads counter=1,
    // the other reads counter=2. If the engine had read the counter
    // pool at trigger-fire time (counter=0 for both) or post-double-
    // tick (counter=2 for both), the SET would be `{Mana{0}, PayLife{0}}`
    // or `{Mana{2}, PayLife{2}}` — both ruled out below.
    let costs = [first_cost.clone(), second_cost.clone()];
    // The first cost (resolved at counter=1) must be the ×1 form of
    // either base — Mana{1} or PayLife{1}.
    let first_is_one_scaled = matches!(
        &first_cost,
        AbilityCost::Mana { cost: mana } if *mana == ManaCost::generic(1)
    ) || matches!(
        &first_cost,
        AbilityCost::PayLife {
            amount: QuantityExpr::Fixed { value: 1 },
        }
    );
    assert!(
        first_is_one_scaled,
        "first-resolving trigger must read counter=1 and scale base × 1 \
             (Mana{{1}} or PayLife(1)). Got {first_cost:?}"
    );
    // The second cost (resolved at counter=2) must be the ×2 form of
    // either base — Mana{2} or PayLife{2}.
    let second_is_two_scaled = matches!(
        &second_cost,
        AbilityCost::Mana { cost: mana } if *mana == ManaCost::generic(2)
    ) || matches!(
        &second_cost,
        AbilityCost::PayLife {
            amount: QuantityExpr::Fixed { value: 2 },
        }
    );
    assert!(
        second_is_two_scaled,
        "second-resolving trigger must read counter=2 and scale base × 2 \
             (Mana{{2}} or PayLife(2)) — this is the load-bearing CR 702.24b \
             assertion that the counter pool is SHARED across instances and \
             read at each ability's RESOLUTION TIME. Got {second_cost:?}"
    );
    // CR 702.24b — the cost types must be distinct (one Mana, one
    // PayLife). If both triggers somehow surfaced the same shape we
    // would have lost the separate-instance identity.
    let mana_count = costs
        .iter()
        .filter(|c| matches!(c, AbilityCost::Mana { .. }))
        .count();
    let life_count = costs
        .iter()
        .filter(|c| matches!(c, AbilityCost::PayLife { .. }))
        .count();
    assert_eq!(
        mana_count, 1,
        "exactly one Mana cost across the two prompts; got {costs:?}"
    );
    assert_eq!(
        life_count, 1,
        "exactly one PayLife cost across the two prompts; got {costs:?}"
    );

    // Pay the second unless-cost. The dispatcher handles whichever
    // shape arrived second.
    pay_unless_payment_dispatching(&mut state, &second_cost);

    // CR 702.24b: final state — both triggers paid, 2 age counters
    // accumulated, permanent stayed on the battlefield, and the
    // controller paid exactly the ×1 + ×2 multiples of the PayLife
    // base across the two prompts.
    assert_eq!(
        state.objects[&perm_id]
            .counters
            .get(&crate::types::counter::CounterType::Age)
            .copied(),
        Some(2),
        "both triggers' AddCounter effects must have ticked the shared pool"
    );
    assert_eq!(
        state.objects[&perm_id].zone,
        Zone::Battlefield,
        "paying both cumulative-upkeep costs must keep the permanent on the battlefield"
    );
    assert!(
        !state.players[0].graveyard.contains(&perm_id),
        "permanent must not be sacrificed when both costs are paid"
    );
    // CR 119.4: total life delta = whichever resolution paid PayLife.
    //   - If PayLife resolved FIRST (counter=1), it cost 1 life.
    //   - If PayLife resolved SECOND (counter=2), it cost 2 life.
    // Either way the Mana cost contributes 0 to the life delta. Compute
    // the expected delta from the first cost shape: when the first cost
    // was PayLife, total -1; when the first cost was Mana, total -2.
    let expected_life_delta = if matches!(&first_cost, AbilityCost::PayLife { .. }) {
        1
    } else {
        2
    };
    assert_eq!(
        state.players[0].life,
        life_before - expected_life_delta,
        "controller paid exactly the PayLife trigger's scaled cost in life \
             (the Mana trigger contributes 0 to life delta)"
    );
}

/// Pay the unless-cost surfaced as `cost` on behalf of PlayerId(0).
/// Dispatches on the cost shape so the multi-instance test can pay
/// either `Mana{N}` or `PayLife{N}` in whichever order the engine
/// resolves the two triggers. Other cost shapes (Sacrifice, PayEnergy,
/// Discard) are not exercised by this test and are flagged with a
/// panic to surface scope-creep if a future cumulative-upkeep variant
/// is added.
fn pay_unless_payment_dispatching(state: &mut GameState, cost: &AbilityCost) {
    match cost {
        // CR 118.12 + CR 500.5: provision the colorless mana, then pay.
        // CR 202.3: `mana_value()` is the authoritative count of mana
        // units required — it folds generic + shards into a single int
        // and is robust to future cost shapes (e.g. hybrid symbols)
        // that aren't exercised by the current Mana{1} base.
        AbilityCost::Mana { cost: mana_cost } => {
            give_p0_colorless_mana(state, mana_cost.mana_value());
            apply_as_current(state, GameAction::PayUnlessCost { pay: true })
                .expect("PayUnlessCost { pay: true } must succeed for Mana cost");
        }
        // CR 118.12 + CR 119.4: life is auto-deducted at PayUnlessCost time —
        // no intermediate mana-payment prompt.
        AbilityCost::PayLife { .. } => {
            apply_as_current(state, GameAction::PayUnlessCost { pay: true })
                .expect("PayUnlessCost { pay: true } must succeed for PayLife cost");
        }
        other => panic!(
            "unexpected unless-cost shape in multi-instance cumulative-upkeep test: {other:?}"
        ),
    }
}

/// Build the synthesized cumulative-upkeep trigger for "Cumulative upkeep
/// {W} or {U}" (Jötun Owl Keeper's disjunctive cost variant) by delegating
/// to the production synthesizer. Mirrors `cumulative_upkeep_mana_trigger`,
/// `cumulative_upkeep_sacrifice_land_trigger`, and
/// `cumulative_upkeep_pay_life_trigger`; this helper exercises the `OneOf`
/// arm of `expand_per_counter` plus the Composite-of-OneOfs routing path in
/// `handle_unless_payment_choose_cost` (CR 702.24a: "If [cost] has choices
/// associated with it, each choice is made separately for each age counter,
/// then either the entire set of costs is paid, or none of them is paid").
///
/// CR 702.24a: a `OneOf { Mana(W), Mana(U) }` base cost is the canonical
/// disjunctive cumulative-upkeep shape (Jötun Owl Keeper, Arctic Nishoba,
/// Earthen Goo).
fn cumulative_upkeep_one_of_w_or_u_trigger() -> TriggerDefinition {
    let mana_w = AbilityCost::Mana {
        cost: ManaCost::Cost {
            shards: vec![crate::types::mana::ManaCostShard::White],
            generic: 0,
        },
    };
    let mana_u = AbilityCost::Mana {
        cost: ManaCost::Cost {
            shards: vec![crate::types::mana::ManaCostShard::Blue],
            generic: 0,
        },
    };
    crate::database::synthesis::build_cumulative_upkeep_trigger(AbilityCost::OneOf {
        costs: vec![mana_w, mana_u],
    })
}

/// Construct a solo state with Jötun Owl Keeper on the battlefield
/// (controller = PlayerId(0) = active player) at Phase::Untap so
/// `auto_advance` will fire the upkeep trigger. **One age counter is
/// pre-loaded** on the Owl Keeper so the first upkeep that
/// `auto_advance` resolves ticks the counter from 1 → 2 — exercising the
/// multiplicative step of the `OneOf` arm of `expand_per_counter`, which
/// expands `OneOf{[W,U]}` × 2 → `Composite { [OneOf{[W,U]}, OneOf{[W,U]}] }`.
/// This is the load-bearing setup for CR 702.24a's "each choice is made
/// separately for each age counter" clause — counter=1 would collapse to a
/// trivial single-prompt case, and we specifically want the multi-prompt
/// disjunctive flow.
fn setup_jotun_owl_keeper_second_upkeep_state() -> (GameState, ObjectId) {
    let mut state = new_game(42);
    state.turn_number = 2;
    state.phase = Phase::Untap;
    state.active_player = PlayerId(0);
    state.priority_player = PlayerId(0);

    let owl = create_object(
        &mut state,
        CardId(7400),
        PlayerId(0),
        "Jötun Owl Keeper".to_string(),
        Zone::Battlefield,
    );
    {
        let obj = state.objects.get_mut(&owl).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.card_types.subtypes.push("Giant".to_string());
        obj.trigger_definitions
            .push(cumulative_upkeep_one_of_w_or_u_trigger());
        // CR 702.24a: pre-load one age counter so the next upkeep tick
        // produces counter=2, yielding the per-counter expansion
        // `OneOf{[W,U]}` × 2 → `Composite { [OneOf{[W,U]}, OneOf{[W,U]}] }`.
        obj.counters
            .insert(crate::types::counter::CounterType::Age, 1);
    }

    (state, owl)
}

/// CR 702.24a + CR 118.12: End-to-end OneOf × N flow — Jötun Owl Keeper's
/// "{W} or {U}" cumulative-upkeep cost at counter=2 expands to a
/// `Composite` of two `OneOf` sub-costs. The engine surfaces one
/// `UnlessPaymentChooseCost` prompt per disjunctive sub-cost; each pick
/// accumulates into `chosen`. After the last prompt, the accumulated picks
/// collapse into `Composite { [Mana(W), Mana(U)] }` which the single-cost
/// `handle_unless_payment` folds into a combined `{W}{U}` mana payment.
/// Paying the combined cost keeps the Owl Keeper on the battlefield and
/// drains the controller's mana pool of the two colored units.
///
/// This is the capstone test for the OneOf × N pipeline: it exercises the
/// synthesizer (Task 7) producing the trigger, the PerCounter resolution
/// (Task 6) expanding `OneOf × 2` → `Composite[OneOf, OneOf]`, the
/// multi-choice routing (Task 14) walking each disjunctive choice, and the
/// Composite-of-Mana payment (Task 14) folding the picks into a combined
/// mana payment. CR 702.24a: "each choice is made separately for each age
/// counter, then either the entire set of costs is paid, or none of them
/// is paid."
#[test]
fn jotun_owl_keeper_one_of_x_n_pays_combined_mana() {
    use crate::types::actions::UnlessCostBranch;
    let (mut state, owl_id) = setup_jotun_owl_keeper_second_upkeep_state();
    advance_to_unless_payment_prompt(&mut state);

    // CR 702.24a: outer AddCounter resolved first; the pre-loaded counter
    // ticked from 1 → 2 before the per-counter unless-cost is computed.
    assert_eq!(
        state.objects[&owl_id]
            .counters
            .get(&crate::types::counter::CounterType::Age)
            .copied(),
        Some(2),
        "age counter should tick from 1 (pre-loaded) to 2 on this upkeep"
    );

    // CR 702.24a + CR 118.12a: PerCounter expanded `OneOf{[W,U]}` × 2 to
    // `Composite { [OneOf{[W,U]}, OneOf{[W,U]}] }`. The engine surfaces
    // the FIRST disjunctive choice with one entry remaining in
    // `remaining_choices`.
    match &state.waiting_for {
        WaitingFor::UnlessPaymentChooseCost {
            player,
            costs,
            remaining_choices,
            chosen,
            ..
        } => {
            assert_eq!(*player, PlayerId(0), "controller is the unless-payer");
            assert_eq!(costs.len(), 2, "first choice exposes both alternatives");
            assert_eq!(
                remaining_choices.len(),
                1,
                "one more disjunctive choice queued (counter=2 → 2 prompts)"
            );
            assert!(
                chosen.is_empty(),
                "no choices made yet before the first prompt"
            );
        }
        other => panic!("expected first UnlessPaymentChooseCost, got {other:?}"),
    }

    // Pick {W} (index 0). The first pick accumulates into `chosen`; the
    // queue is drained; the second OneOf prompt surfaces.
    apply_as_current(
        &mut state,
        GameAction::ChooseUnlessCostBranch {
            choice: UnlessCostBranch::Pay { index: 0 },
        },
    )
    .expect("first ChooseUnlessCostBranch should surface the next prompt");

    // CR 702.24a + CR 118.12a: SECOND disjunctive choice prompt.
    // `remaining_choices` is now empty; `chosen` carries [Mana(W)].
    match &state.waiting_for {
        WaitingFor::UnlessPaymentChooseCost {
            costs,
            remaining_choices,
            chosen,
            ..
        } => {
            assert_eq!(costs.len(), 2, "second choice exposes both alternatives");
            assert!(
                remaining_choices.is_empty(),
                "no more disjunctive choices queued"
            );
            assert_eq!(chosen.len(), 1, "first pick accumulated into `chosen`");
            assert!(
                matches!(
                    &chosen[0],
                    AbilityCost::Mana { cost: ManaCost::Cost { shards, generic: 0 } }
                        if shards.as_slice() == [crate::types::mana::ManaCostShard::White]
                ),
                "first pick is Mana({{W}}) as selected by index 0; got {:?}",
                &chosen[0]
            );
        }
        other => panic!("expected second UnlessPaymentChooseCost, got {other:?}"),
    }

    // CR 500.5 + CR 118.12: Provision {W}{U} in P0's mana pool BEFORE the
    // final pick. The second ChooseUnlessCostBranch routes through
    // `handle_unless_payment_choose_cost` → builds
    // `Composite { [Mana(W), Mana(U)] }` → re-enters
    // `handle_unless_payment(state, .., pay=true)` → folds the Composite
    // into a combined `{W}{U}` ManaCost → calls `pay_unless_cost`. So the
    // mana must already be in the pool by the time the second action is
    // dispatched. Real play would tap a Plains and an Island in response
    // to the trigger before answering the second prompt; we shortcut by
    // dropping the mana directly into the pool.
    let p0 = state
        .players
        .iter_mut()
        .find(|p| p.id == PlayerId(0))
        .expect("PlayerId(0)");
    p0.mana_pool
        .add(ManaUnit::new(ManaType::White, ObjectId(0), false, vec![]));
    p0.mana_pool
        .add(ManaUnit::new(ManaType::Blue, ObjectId(0), false, vec![]));

    // Pick {U} (index 1). The second pick accumulates, the queue is
    // empty, so `handle_unless_payment_choose_cost` collapses
    // `chosen = [Mana(W), Mana(U)]` into `Composite { ... }` and routes
    // straight into `handle_unless_payment` with `pay = true`. That
    // handler's all-Mana-Composite arm folds the inner costs via
    // `ManaCost::plus` and pays the combined `{W}{U}` cost — there is no
    // intermediate `UnlessPayment` prompt visible to the test, the
    // payment happens inline. (See `engine_payment_choices::handle_unless_payment`
    // L592-599 for the fold + pay logic.)
    apply_as_current(
        &mut state,
        GameAction::ChooseUnlessCostBranch {
            choice: UnlessCostBranch::Pay { index: 1 },
        },
    )
    .expect("second ChooseUnlessCostBranch should fold + pay the combined Composite-of-Mana");

    // CR 702.24a: paying the cost keeps the permanent on the battlefield.
    assert_eq!(
        state.objects[&owl_id].zone,
        Zone::Battlefield,
        "paying the cumulative-upkeep cost must NOT sacrifice the Owl Keeper"
    );
    assert!(
        !state.players[0].graveyard.contains(&owl_id),
        "permanent must not be in graveyard when paid"
    );

    // CR 118.12 + CR 202.3: The combined `{W}{U}` payment drained the
    // White + Blue units from the mana pool. This is the load-bearing
    // assertion that the Composite-of-Mana fold path actually paid the
    // colored cost (and not, e.g., zero generic via a buggy unwrap).
    let p0_after = state.players.iter().find(|p| p.id == PlayerId(0)).unwrap();
    assert_eq!(
        p0_after.mana_pool.total(),
        0,
        "combined {{W}}{{U}} cost drains both colored mana units from the pool"
    );
}
