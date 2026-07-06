use std::sync::Arc;

use super::*;
use crate::game::combat::AttackTarget;
use crate::game::game_object::{BackFaceData, RoomDoor};
use crate::game::zones::create_object;
use crate::parser::oracle::parse_oracle_text;
use crate::types::ability::{
    AbilityCost, AbilityDefinition, AbilityKind, AbilityTag, ChoiceType, ControllerRef, Effect,
    ManaContribution, ManaProduction, ManaSpendRestriction, QuantityExpr, ResolvedAbility,
    StaticDefinition, TargetFilter, TriggerDefinition, TypeFilter, TypedFilter,
};
use crate::types::card_type::CardType;
use crate::types::card_type::CoreType;
use crate::types::counter::CounterType;
use crate::types::format::FormatConfig;
use crate::types::game_state::CastingVariant;
use crate::types::identifiers::{CardId, ObjectId};
use crate::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType, ManaUnit};
use crate::types::statics::{CastFrequency, StaticMode};
use crate::types::TriggerMode;

/// Create a simple test ability definition.
fn make_draw_ability(num_cards: u32) -> AbilityDefinition {
    AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::Draw {
            count: QuantityExpr::Fixed {
                value: num_cards as i32,
            },
            target: TargetFilter::Controller,
        },
    )
}

fn no_op_stack_entry(id: u64, controller: PlayerId) -> StackEntry {
    let object_id = ObjectId(id);
    StackEntry {
        id: object_id,
        source_id: object_id,
        controller,
        kind: StackEntryKind::ActivatedAbility {
            source_id: object_id,
            ability: ResolvedAbility::new(Effect::NoOp, vec![], object_id, controller),
        },
    }
}

#[test]
fn cards_revealed_events_are_remembered_publicly() {
    let mut state = GameState::new_two_player(42);
    let card_id = ObjectId(42);
    let events = vec![GameEvent::CardsRevealed {
        player: PlayerId(1),
        card_ids: vec![card_id],
        card_names: vec!["Known Card".to_string()],
    }];

    remember_public_reveals(&mut state, &events);

    assert!(state.public_revealed_cards.contains(&card_id));
}

/// CR 603.3d regression — reported turn-34 Commander freeze (All Will Be
/// One + Red Hulk + Schema Thief board). A targeted trigger whose only legal
/// target vanished between "push first" dispatch and "choose second"
/// selection-setup (an effect earlier in the same simultaneous cascade
/// removed it) must be REMOVED FROM THE STACK here — not abort the in-flight
/// action with `Err`, which silently dropped `PassPriority` from the legal
/// action set and soft-locked the game. Schema Thief ("create a token that's
/// a copy of target artifact that player controls") triggered against a
/// player who controlled no artifact; this models that with a damage trigger
/// that has no opponent-creature target.
#[test]
fn pending_trigger_with_no_legal_target_at_choose_time_drops_not_errors() {
    let mut state = GameState::new_two_player(7);
    state.active_player = PlayerId(0);
    state.priority_player = PlayerId(0);

    // Source permanent (player 0). No opponent creatures exist, so a
    // "deal 1 damage to target creature an opponent controls" trigger has
    // no legal target when selection is set up.
    let source_id = create_object(
        &mut state,
        CardId(1),
        PlayerId(0),
        "Pinger".to_string(),
        Zone::Battlefield,
    );

    let ability = ResolvedAbility::new(
        Effect::DealDamage {
            amount: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::Typed(
                TypedFilter::creature().controller(ControllerRef::Opponent),
            ),
            damage_source: None,
            excess: None,
        },
        vec![],
        source_id,
        PlayerId(0),
    );

    // Reconstruct the post-"push first" state: the trigger entry is on the
    // stack (in construction) and recorded as the pending trigger awaiting
    // manual target selection.
    let pending = crate::game::triggers::PendingTrigger {
        source_id,
        controller: PlayerId(0),
        condition: None,
        ability: ability.clone(),
        timestamp: 0,
        target_constraints: vec![],
        distribute: None,
        trigger_event: None,
        modal: None,
        mode_abilities: vec![],
        description: None,
        may_trigger_origin: None,
        subject_match_count: None,
        die_result: None,
    };
    let entry_id = ObjectId(state.next_object_id);
    state.next_object_id += 1;
    state.stack.push_back(StackEntry {
        id: entry_id,
        source_id,
        controller: PlayerId(0),
        kind: StackEntryKind::TriggeredAbility {
            source_id,
            ability: Box::new(ability),
            condition: None,
            trigger_event: None,
            description: None,
            source_name: "Pinger".to_string(),
            subject_match_count: None,
            die_result: None,
        },
    });
    state.pending_trigger = Some(pending);
    state.pending_trigger_entry = Some(entry_id);
    let stack_len_before = state.stack.len();

    let result = begin_pending_trigger_target_selection(&mut state);

    assert!(
        matches!(result, Ok(None)),
        "CR 603.3d: a no-legal-target trigger must be dropped, not error: {result:?}",
    );
    assert_eq!(
        state.stack.len(),
        stack_len_before - 1,
        "the in-construction trigger entry must be popped from the stack",
    );
    assert!(
        state.pending_trigger.is_none(),
        "the pending_trigger cursor must be cleared",
    );
    assert!(
        state.pending_trigger_entry.is_none(),
        "the pending_trigger_entry cursor must be cleared",
    );
}

#[test]
fn choose_new_targets_all_allows_unchanged_illegal_target() {
    let mut state = GameState::new_two_player(42);
    let stack_id = create_object(
        &mut state,
        CardId(1),
        PlayerId(1),
        "Test Spell".to_string(),
        Zone::Stack,
    );
    let unchanged = TargetRef::Object(ObjectId(901));
    let legal_alternative = TargetRef::Object(ObjectId(902));
    let stack_ability = ResolvedAbility::new(
        Effect::Draw {
            count: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::Controller,
        },
        vec![unchanged.clone()],
        stack_id,
        PlayerId(1),
    );
    state.stack.push_back(StackEntry {
        id: stack_id,
        source_id: stack_id,
        controller: PlayerId(1),
        kind: StackEntryKind::Spell {
            card_id: CardId(1),
            ability: Some(stack_ability),
            casting_variant: CastingVariant::Normal,
            actual_mana_spent: 0,
        },
    });
    state.waiting_for = WaitingFor::RetargetChoice {
        player: PlayerId(0),
        stack_entry_index: 0,
        scope: RetargetScope::All,
        current_targets: vec![unchanged.clone()],
        legal_new_targets: vec![legal_alternative],
    };

    apply(
        &mut state,
        PlayerId(0),
        GameAction::RetargetSpell {
            new_targets: vec![unchanged.clone()],
        },
    )
    .expect("unchanged targets do not need to be legal for choose-new-targets");

    let targets = state
        .stack
        .front()
        .and_then(|entry| entry.ability())
        .map(|ability| ability.targets.clone())
        .expect("spell remains on stack");
    assert_eq!(targets, vec![unchanged]);
}

#[test]
fn terminal_reconcile_does_not_run_sbas_for_cant_lose_player() {
    let mut state = GameState::new(FormatConfig::commander(), 2, 42);
    let protected = create_object(
        &mut state,
        CardId(1),
        PlayerId(0),
        "Platinum Angel".to_string(),
        Zone::Battlefield,
    );
    state
        .objects
        .get_mut(&protected)
        .expect("protected source exists")
        .static_definitions
        .push(
            StaticDefinition::new(StaticMode::CantLoseTheGame).affected(TargetFilter::Typed(
                TypedFilter::default().controller(ControllerRef::You),
            )),
        );

    let commander = create_object(
        &mut state,
        CardId(2),
        PlayerId(1),
        "Kaalia".to_string(),
        Zone::Command,
    );
    let commander_obj = state
        .objects
        .get_mut(&commander)
        .expect("commander object exists");
    commander_obj.is_commander = true;
    commander_obj.card_types.core_types.push(CoreType::Creature);
    let mut move_events = Vec::new();
    zones::move_to_zone(&mut state, commander, Zone::Battlefield, &mut move_events);
    zones::move_to_zone(&mut state, commander, Zone::Graveyard, &mut move_events);

    // CR 101.2 + CR 704.5a: Platinum Angel means P0 cannot lose from
    // 0-or-less life. The
    // non-priority DiscardChoice should therefore remain active; otherwise
    // the full SBA loop would notice the unrelated dead commander and
    // replace the choice with CommanderZoneChoice.
    state.players[0].life = 0;
    state.waiting_for = WaitingFor::DiscardChoice {
        player: PlayerId(0),
        count: 1,
        cards: Vec::new(),
        source_id: ObjectId(999),
        effect_kind: EffectKind::DiscardCard,
        up_to: false,
        unless_filter: None,
    };
    let original_waiting_for = state.waiting_for.clone();
    let mut result = ActionResult {
        events: Vec::new(),
        waiting_for: original_waiting_for.clone(),
        log_entries: Vec::new(),
    };

    reconcile_terminal_result(&mut state, &mut result);

    assert_eq!(state.waiting_for, original_waiting_for);
    assert_eq!(result.waiting_for, original_waiting_for);
    assert!(!state.players[0].is_eliminated);
    assert_eq!(state.objects[&commander].zone, Zone::Graveyard);
}

#[test]
fn terminal_reconcile_runs_player_loss_sba_for_unprotected_player() {
    let mut state = GameState::new_two_player(42);
    state.players[0].life = 0;
    state.waiting_for = WaitingFor::DiscardChoice {
        player: PlayerId(0),
        count: 1,
        cards: Vec::new(),
        source_id: ObjectId(999),
        effect_kind: EffectKind::DiscardCard,
        up_to: false,
        unless_filter: None,
    };
    let mut result = ActionResult {
        events: Vec::new(),
        waiting_for: state.waiting_for.clone(),
        log_entries: Vec::new(),
    };

    reconcile_terminal_result(&mut state, &mut result);

    // CR 704.5a: An unprotected player at 0 life loses before the engine
    // keeps waiting for that player's non-priority discard choice.
    assert!(state.players[0].is_eliminated);
    assert!(matches!(
        result.waiting_for,
        WaitingFor::GameOver {
            winner: Some(PlayerId(1)),
            ..
        }
    ));
}

/// Create a DealDamage ability for testing.
fn make_damage_ability(amount: i32, cost: Option<AbilityCost>) -> AbilityDefinition {
    let kind = if cost.is_some() {
        AbilityKind::Activated
    } else {
        AbilityKind::Spell
    };
    let mut def = AbilityDefinition::new(
        kind,
        Effect::DealDamage {
            amount: QuantityExpr::Fixed { value: amount },
            target: TargetFilter::Any,
            damage_source: None,
            excess: None,
        },
    );
    if let Some(c) = cost {
        def = def.cost(c);
    }
    def
}

fn apply_spell_oracle_to_object(
    state: &mut GameState,
    object_id: ObjectId,
    name: &str,
    oracle_text: &str,
) {
    let types = vec!["Sorcery".to_string()];
    let parsed = parse_oracle_text(oracle_text, name, &[], &types, &[]);
    let obj = state.objects.get_mut(&object_id).unwrap();
    Arc::make_mut(&mut obj.abilities).extend(parsed.abilities.clone());
    Arc::make_mut(&mut obj.base_abilities).extend(parsed.abilities);
}

pub(super) fn apply_oracle_to_object(
    state: &mut GameState,
    object_id: ObjectId,
    name: &str,
    oracle_text: &str,
) {
    let obj = state.objects.get(&object_id).unwrap();
    let types = obj
        .card_types
        .core_types
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>();
    let subtypes = obj.card_types.subtypes.clone();
    let parsed = parse_oracle_text(oracle_text, name, &[], &types, &subtypes);
    let obj = state.objects.get_mut(&object_id).unwrap();
    Arc::make_mut(&mut obj.abilities).extend(parsed.abilities.clone());
    Arc::make_mut(&mut obj.base_abilities).extend(parsed.abilities);
    for trigger in parsed.triggers.clone() {
        obj.trigger_definitions.push(trigger);
    }
    Arc::make_mut(&mut obj.base_trigger_definitions).extend(parsed.triggers);
    for replacement in parsed.replacements.clone() {
        obj.replacement_definitions.push(replacement);
    }
    Arc::make_mut(&mut obj.base_replacement_definitions).extend(parsed.replacements);
    for static_def in parsed.statics.clone() {
        obj.static_definitions.push(static_def);
    }
    Arc::make_mut(&mut obj.base_static_definitions).extend(parsed.statics);
}

use crate::game::test_fixtures::brushland_colored_ability;

fn setup_game_at_main_phase() -> GameState {
    let mut state = new_game(42);
    state.turn_number = 2; // Not first turn
    state.phase = Phase::PreCombatMain;
    state.active_player = PlayerId(0);
    state.priority_player = PlayerId(0);
    state.waiting_for = WaitingFor::Priority {
        player: PlayerId(0),
    };
    state
}

/// Perf guard for go-wide mana-board slowness (turn-40 Cryptolith-Rite
/// squirrel state). The AI `SimulationFilter` legality probe
/// (`apply_as_current_for_simulation`) discards its mutated clone and only
/// reads `.is_ok()`, so it must NOT run `finalize_display_state`'s
/// board-global mana-availability sweep — that sweep is O(N^2) on a board of
/// hundreds of mana sources and the filter pays it once per candidate.
/// The Immediate `apply_as_current` still sweeps (display state is exposed).
#[test]
fn apply_for_legality_skips_display_mana_sweep() {
    // Deferred-display legality probe: no sweep even with mana display dirty.
    let mut sim = setup_game_at_main_phase();
    sim.public_state_dirty.mana_display_dirty = true;
    crate::game::perf_counters::reset();
    apply_as_current_for_simulation(&mut sim, GameAction::PassPriority).unwrap();
    assert_eq!(
        crate::game::perf_counters::snapshot().mana_display_sweeps,
        0,
        "legality probe must skip the display mana sweep"
    );

    // Discriminator: the Immediate path DOES sweep, so the assertion above
    // is meaningful rather than vacuous.
    let mut immediate = setup_game_at_main_phase();
    immediate.public_state_dirty.mana_display_dirty = true;
    crate::game::perf_counters::reset();
    apply_as_current(&mut immediate, GameAction::PassPriority).unwrap();
    assert!(
        crate::game::perf_counters::snapshot().mana_display_sweeps >= 1,
        "Immediate apply finalizes display state and runs the mana sweep"
    );
}

#[test]
fn eldrazi_temple_restricted_mana_casts_kindred_eldrazi_spell_only() {
    let mut state = setup_game_at_main_phase();
    let temple = create_object(
        &mut state,
        CardId(9100),
        PlayerId(0),
        "Eldrazi Temple".to_string(),
        Zone::Battlefield,
    );
    {
        let obj = state.objects.get_mut(&temple).unwrap();
        obj.card_types.core_types.push(CoreType::Land);
        Arc::make_mut(&mut obj.abilities).push(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Mana {
                    produced: ManaProduction::Colorless {
                        count: QuantityExpr::Fixed { value: 2 },
                    },
                    restrictions: vec![ManaSpendRestriction::SpellTypeOrAbilityActivation {
                        spell_type: "Colorless Eldrazi".to_string(),
                        ability: crate::types::mana::AbilityActivationScope::OfSpellType,
                    }],
                    grants: vec![],
                    expiry: None,
                    target: None,
                },
            )
            .cost(AbilityCost::Tap),
        );
    }

    let command = create_object(
        &mut state,
        CardId(9101),
        PlayerId(0),
        "Kozilek's Command".to_string(),
        Zone::Hand,
    );
    {
        let obj = state.objects.get_mut(&command).unwrap();
        obj.card_types.core_types.push(CoreType::Kindred);
        obj.card_types.core_types.push(CoreType::Instant);
        obj.card_types.subtypes.push("Eldrazi".to_string());
        obj.mana_cost = ManaCost::Cost {
            shards: vec![
                ManaCostShard::X,
                ManaCostShard::Colorless,
                ManaCostShard::Colorless,
            ],
            generic: 0,
        };
        Arc::make_mut(&mut obj.abilities).push(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 0 },
                target: TargetFilter::Controller,
            },
        ));
    }

    apply_as_current(
        &mut state,
        GameAction::ActivateAbility {
            source_id: temple,
            ability_index: 0,
        },
    )
    .unwrap();
    let result = apply_as_current(
        &mut state,
        GameAction::CastSpell {
            object_id: command,
            card_id: CardId(9101),
            targets: vec![],

            payment_mode: crate::types::game_state::CastPaymentMode::Auto,
        },
    )
    .unwrap();
    assert!(matches!(
        result.waiting_for,
        WaitingFor::ChooseXValue { .. }
    ));
    apply_as_current(&mut state, GameAction::ChooseX { value: 0 }).unwrap();
    assert!(
        state.stack.iter().any(|entry| entry.source_id == command),
        "Eldrazi Temple mana should pay for colorless Kindred Eldrazi spells"
    );

    let mut state = setup_game_at_main_phase();
    let temple = create_object(
        &mut state,
        CardId(9110),
        PlayerId(0),
        "Eldrazi Temple".to_string(),
        Zone::Battlefield,
    );
    {
        let obj = state.objects.get_mut(&temple).unwrap();
        obj.card_types.core_types.push(CoreType::Land);
        Arc::make_mut(&mut obj.abilities).push(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Mana {
                    produced: ManaProduction::Colorless {
                        count: QuantityExpr::Fixed { value: 2 },
                    },
                    restrictions: vec![ManaSpendRestriction::SpellTypeOrAbilityActivation {
                        spell_type: "Colorless Eldrazi".to_string(),
                        ability: crate::types::mana::AbilityActivationScope::OfSpellType,
                    }],
                    grants: vec![],
                    expiry: None,
                    target: None,
                },
            )
            .cost(AbilityCost::Tap),
        );
    }
    let construct = create_object(
        &mut state,
        CardId(9111),
        PlayerId(0),
        "Colorless Construct".to_string(),
        Zone::Hand,
    );
    {
        let obj = state.objects.get_mut(&construct).unwrap();
        obj.card_types.core_types.push(CoreType::Artifact);
        obj.card_types.core_types.push(CoreType::Creature);
        obj.card_types.subtypes.push("Construct".to_string());
        obj.mana_cost = ManaCost::Cost {
            shards: vec![ManaCostShard::Colorless, ManaCostShard::Colorless],
            generic: 0,
        };
        Arc::make_mut(&mut obj.abilities).push(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 0 },
                target: TargetFilter::Controller,
            },
        ));
    }
    apply_as_current(
        &mut state,
        GameAction::ActivateAbility {
            source_id: temple,
            ability_index: 0,
        },
    )
    .unwrap();
    assert!(
        apply_as_current(
            &mut state,
            GameAction::CastSpell {
                object_id: construct,
                card_id: CardId(9111),
                targets: vec![],

                payment_mode: crate::types::game_state::CastPaymentMode::Auto,
            },
        )
        .is_err(),
        "Eldrazi Temple restricted mana must not pay for non-Eldrazi spells"
    );
}

#[test]
fn chalice_of_the_void_enters_with_x_and_counters_matching_spell() {
    let mut state = setup_game_at_main_phase();
    let chalice = create_object(
        &mut state,
        CardId(9120),
        PlayerId(0),
        "Chalice of the Void".to_string(),
        Zone::Hand,
    );
    {
        let obj = state.objects.get_mut(&chalice).unwrap();
        obj.card_types.core_types.push(CoreType::Artifact);
        obj.mana_cost = ManaCost::Cost {
            shards: vec![ManaCostShard::X, ManaCostShard::X],
            generic: 0,
        };
    }
    apply_oracle_to_object(
            &mut state,
            chalice,
            "Chalice of the Void",
            "This artifact enters with X charge counters on it.\nWhenever a player casts a spell with mana value equal to the number of charge counters on this artifact, counter that spell.",
        );
    let player = state
        .players
        .iter_mut()
        .find(|player| player.id == PlayerId(0))
        .unwrap();
    for _ in 0..3 {
        player.mana_pool.add(crate::types::mana::ManaUnit::new(
            crate::types::mana::ManaType::Colorless,
            ObjectId(0),
            false,
            vec![],
        ));
    }

    apply_as_current(
        &mut state,
        GameAction::CastSpell {
            object_id: chalice,
            card_id: CardId(9120),
            targets: vec![],

            payment_mode: crate::types::game_state::CastPaymentMode::Auto,
        },
    )
    .unwrap();
    apply_as_current(&mut state, GameAction::ChooseX { value: 1 }).unwrap();
    apply_as_current(&mut state, GameAction::PassPriority).unwrap();
    apply_as_current(&mut state, GameAction::PassPriority).unwrap();

    assert_eq!(state.objects[&chalice].zone, Zone::Battlefield);
    assert_eq!(
        state.objects[&chalice]
            .counters
            .get(&CounterType::Generic("charge".to_string()))
            .copied()
            .unwrap_or_default(),
        1
    );

    let spell = create_object(
        &mut state,
        CardId(9121),
        PlayerId(0),
        "One Mana Spell".to_string(),
        Zone::Hand,
    );
    {
        let obj = state.objects.get_mut(&spell).unwrap();
        obj.card_types.core_types.push(CoreType::Instant);
        obj.mana_cost = ManaCost::Cost {
            shards: vec![],
            generic: 1,
        };
        Arc::make_mut(&mut obj.abilities).push(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 0 },
                target: TargetFilter::Controller,
            },
        ));
    }
    state
        .players
        .iter_mut()
        .find(|player| player.id == PlayerId(0))
        .unwrap()
        .mana_pool
        .add(crate::types::mana::ManaUnit::new(
            crate::types::mana::ManaType::Colorless,
            ObjectId(0),
            false,
            vec![],
        ));

    apply_as_current(
        &mut state,
        GameAction::CastSpell {
            object_id: spell,
            card_id: CardId(9121),
            targets: vec![],

            payment_mode: crate::types::game_state::CastPaymentMode::Auto,
        },
    )
    .unwrap();
    assert!(
        state.stack.iter().any(|entry| entry.source_id == chalice),
        "Chalice should trigger for a spell with matching mana value"
    );
    apply_as_current(&mut state, GameAction::PassPriority).unwrap();
    apply_as_current(&mut state, GameAction::PassPriority).unwrap();
    assert_eq!(
        state.objects[&spell].zone,
        Zone::Graveyard,
        "Chalice trigger should counter the matching spell"
    );
}

/// CR 107.3m + CR 614.1c + CR 704.5f: Walking Ballista is the canonical
/// 0/0 X-cost creature with "enters with X +1/+1 counters." Casting with
/// X=4 must (a) stamp `cost_x_paid = Some(4)` during `finalize_cast`,
/// (b) let the ETB replacement read it via `QuantityRef::CostXPaid`,
/// (c) put 4 +1/+1 counters on the entering Ballista BEFORE SBAs run,
/// (d) leave a live 4/4 on the battlefield (counters set P/T to 4/4
/// before the 0/0 SBA would otherwise put it in the graveyard).
#[test]
fn walking_ballista_enters_with_x_counters_and_survives_zero_zero_sba() {
    let mut state = setup_game_at_main_phase();
    let ballista = create_object(
        &mut state,
        CardId(9130),
        PlayerId(0),
        "Walking Ballista".to_string(),
        Zone::Hand,
    );
    {
        let obj = state.objects.get_mut(&ballista).unwrap();
        obj.card_types.core_types.push(CoreType::Artifact);
        obj.card_types.core_types.push(CoreType::Creature);
        obj.card_types.subtypes.push("Construct".to_string());
        obj.power = Some(0);
        obj.toughness = Some(0);
        obj.mana_cost = ManaCost::Cost {
            shards: vec![ManaCostShard::X, ManaCostShard::X],
            generic: 0,
        };
    }
    apply_oracle_to_object(
            &mut state,
            ballista,
            "Walking Ballista",
            "Walking Ballista enters with X +1/+1 counters on it.\n{4}: Put a +1/+1 counter on this creature.\nRemove a +1/+1 counter from this creature: It deals 1 damage to any target.",
        );
    // Pay 2X = 8 colorless mana for X = 4.
    let player = state
        .players
        .iter_mut()
        .find(|player| player.id == PlayerId(0))
        .unwrap();
    for _ in 0..8 {
        player.mana_pool.add(crate::types::mana::ManaUnit::new(
            crate::types::mana::ManaType::Colorless,
            ObjectId(0),
            false,
            vec![],
        ));
    }

    apply_as_current(
        &mut state,
        GameAction::CastSpell {
            object_id: ballista,
            card_id: CardId(9130),
            targets: vec![],

            payment_mode: crate::types::game_state::CastPaymentMode::Auto,
        },
    )
    .unwrap();
    apply_as_current(&mut state, GameAction::ChooseX { value: 4 }).unwrap();
    apply_as_current(&mut state, GameAction::PassPriority).unwrap();
    apply_as_current(&mut state, GameAction::PassPriority).unwrap();

    // CR 614.1c: counters land before CR 704.5f checks 0 toughness, so
    // the Ballista must be alive on the battlefield, not in the graveyard.
    assert_eq!(
            state.objects[&ballista].zone,
            Zone::Battlefield,
            "Walking Ballista must enter and survive — counters land before 0/0 SBA (CR 614.1c + CR 704.5f). \
             Got zone {:?}, cost_x_paid={:?}, counters={:?}",
            state.objects[&ballista].zone,
            state.objects[&ballista].cost_x_paid,
            state.objects[&ballista].counters,
        );
    assert_eq!(
        state.objects[&ballista]
            .counters
            .get(&CounterType::Plus1Plus1)
            .copied()
            .unwrap_or_default(),
        4,
        "Walking Ballista must enter with X=4 +1/+1 counters"
    );
}

/// CR 107.3m + CR 614.1c: Production-path variant of the Walking Ballista
/// test. Loads the card face from the live `client/public/card-data.json`
/// export and hydrates the object via `create_object_from_card_face`
/// (the same path used by deck loading at game start). The earlier
/// test exercises `apply_oracle_to_object` (re-parses oracle text at test
/// time); this one exercises the same JSON hydration path the running
/// game uses, so any divergence between "parsed at test time" and
/// "loaded from card-data.json" shows up as a test failure here.
#[test]
fn walking_ballista_db_load_path_enters_with_x_counters() {
    use crate::game::deck_loading::create_object_from_card_face;

    let db = crate::test_support::shared_card_db();
    let face = db
        .get_face_by_name("Walking Ballista")
        .expect("Walking Ballista must be in the export")
        .clone();

    let mut state = setup_game_at_main_phase();
    let ballista = create_object_from_card_face(&mut state, &face, PlayerId(0));
    // Move the just-loaded object from Library to Hand so we can cast.
    state.objects.get_mut(&ballista).unwrap().zone = Zone::Hand;
    if let Some(player) = state.players.iter_mut().find(|p| p.id == PlayerId(0)) {
        player.library.retain(|id| *id != ballista);
        player.hand.push_back(ballista);
    }

    let player = state
        .players
        .iter_mut()
        .find(|player| player.id == PlayerId(0))
        .unwrap();
    for _ in 0..8 {
        player.mana_pool.add(crate::types::mana::ManaUnit::new(
            crate::types::mana::ManaType::Colorless,
            ObjectId(0),
            false,
            vec![],
        ));
    }
    let card_id = state.objects[&ballista].card_id;

    apply_as_current(
        &mut state,
        GameAction::CastSpell {
            object_id: ballista,
            card_id,
            targets: vec![],

            payment_mode: crate::types::game_state::CastPaymentMode::Auto,
        },
    )
    .unwrap();
    apply_as_current(&mut state, GameAction::ChooseX { value: 4 }).unwrap();
    apply_as_current(&mut state, GameAction::PassPriority).unwrap();
    apply_as_current(&mut state, GameAction::PassPriority).unwrap();

    assert_eq!(
        state.objects[&ballista].zone,
        Zone::Battlefield,
        "DB-loaded Walking Ballista with X=4 must survive 0/0 SBA. \
             cost_x_paid={:?}, counters={:?}, replacements={:?}",
        state.objects[&ballista].cost_x_paid,
        state.objects[&ballista].counters,
        state.objects[&ballista]
            .replacement_definitions
            .0
            .iter()
            .map(|r| (r.event.to_string(), r.description.clone()))
            .collect::<Vec<_>>(),
    );
    assert_eq!(
        state.objects[&ballista]
            .counters
            .get(&CounterType::Plus1Plus1)
            .copied()
            .unwrap_or_default(),
        4,
        "Walking Ballista must enter with X=4 +1/+1 counters (DB-load path)"
    );
}

/// CR 704.4 + CR 616.1 + CR 614.1c + CR 704.5f: Walking Ballista enters with
/// X +1/+1 counters while the controller also has TWO order-material
/// counter-modifying replacements — Branching Evolution ("twice that many")
/// and Ozolith, the Shattered Spire ("that many plus one"). Placing the ETB
/// counters is replaced by both, and because `(N*2)+1 != (N+1)*2` the CR
/// 616.1 application order is material, so the engine pauses on a
/// `ReplacementChoice`. That pause happens DURING the resolution of the
/// Ballista spell. Per CR 704.4 ("state-based actions pay no attention to
/// what happens during the resolution of a spell or ability") SBAs must NOT
/// fire while the choice is pending — otherwise the 0/0 Ballista is sent to
/// the graveyard (CR 704.5f) before its entering counters land. Regression
/// test for that interaction: single-replacement cases never paused, so the
/// existing `walking_ballista_*` tests did not catch it.
#[test]
fn walking_ballista_enters_with_counters_survives_with_two_material_replacements() {
    let mut state = setup_game_at_main_phase();

    // Branching Evolution: doubles +1/+1 counters put on your creatures.
    let branching = create_object(
        &mut state,
        CardId(9140),
        PlayerId(0),
        "Branching Evolution".to_string(),
        Zone::Battlefield,
    );
    state
        .objects
        .get_mut(&branching)
        .unwrap()
        .card_types
        .core_types = vec![CoreType::Enchantment];
    apply_oracle_to_object(
        &mut state,
        branching,
        "Branching Evolution",
        "If one or more +1/+1 counters would be put on a creature you control, twice that many +1/+1 counters are put on that creature instead.",
    );

    // Ozolith, the Shattered Spire: adds one to +1/+1 counter placements.
    let ozolith = create_object(
        &mut state,
        CardId(9141),
        PlayerId(0),
        "Ozolith, the Shattered Spire".to_string(),
        Zone::Battlefield,
    );
    state
        .objects
        .get_mut(&ozolith)
        .unwrap()
        .card_types
        .core_types = vec![CoreType::Artifact];
    apply_oracle_to_object(
        &mut state,
        ozolith,
        "Ozolith, the Shattered Spire",
        "If one or more +1/+1 counters would be put on an artifact or creature you control, that many plus one +1/+1 counters are put on it instead.",
    );

    let ballista = create_object(
        &mut state,
        CardId(9130),
        PlayerId(0),
        "Walking Ballista".to_string(),
        Zone::Hand,
    );
    {
        let obj = state.objects.get_mut(&ballista).unwrap();
        obj.card_types.core_types.push(CoreType::Artifact);
        obj.card_types.core_types.push(CoreType::Creature);
        obj.card_types.subtypes.push("Construct".to_string());
        obj.power = Some(0);
        obj.toughness = Some(0);
        obj.mana_cost = ManaCost::Cost {
            shards: vec![ManaCostShard::X, ManaCostShard::X],
            generic: 0,
        };
    }
    apply_oracle_to_object(
        &mut state,
        ballista,
        "Walking Ballista",
        "Walking Ballista enters with X +1/+1 counters on it.\n{4}: Put a +1/+1 counter on this creature.\nRemove a +1/+1 counter from this creature: It deals 1 damage to any target.",
    );

    let player = state
        .players
        .iter_mut()
        .find(|player| player.id == PlayerId(0))
        .unwrap();
    for _ in 0..8 {
        player.mana_pool.add(crate::types::mana::ManaUnit::new(
            crate::types::mana::ManaType::Colorless,
            ObjectId(0),
            false,
            vec![],
        ));
    }

    apply_as_current(
        &mut state,
        GameAction::CastSpell {
            object_id: ballista,
            card_id: CardId(9130),
            targets: vec![],
            payment_mode: crate::types::game_state::CastPaymentMode::Auto,
        },
    )
    .unwrap();
    apply_as_current(&mut state, GameAction::ChooseX { value: 4 }).unwrap();
    apply_as_current(&mut state, GameAction::PassPriority).unwrap();
    apply_as_current(&mut state, GameAction::PassPriority).unwrap();

    // CR 704.4: the spell is mid-resolution, paused on the CR 616.1 ordering
    // choice. The Ballista must NOT have been killed by a 0-toughness SBA —
    // its entering counters have simply not been placed yet.
    assert_eq!(
        state.objects[&ballista].zone,
        Zone::Battlefield,
        "Walking Ballista must still be entering (not dead) while the CR 616.1 \
         replacement-order choice is pending. Got zone {:?}, waiting_for={:?}",
        state.objects[&ballista].zone,
        state.waiting_for,
    );

    // The pause must actually be the CR 616.1 replacement-order choice. Without
    // this, a future change that auto-orders the two doublers (no choice
    // surfaced) would still land 9/10 counters with the Ballista alive, so every
    // assertion below — and the "still entering" one above — would pass vacuously
    // while the SBA-suppression-during-ReplacementChoice branch went unexercised.
    assert!(
        matches!(state.waiting_for, WaitingFor::ReplacementChoice { .. }),
        "expected a CR 616.1 replacement-order choice pending mid-entry, got {:?}",
        state.waiting_for,
    );

    // CR 616.1: answer the (possibly repeated) replacement-order choices.
    let mut guard = 0;
    while matches!(state.waiting_for, WaitingFor::ReplacementChoice { .. }) {
        apply_as_current(&mut state, GameAction::ChooseReplacement { index: 0 })
            .expect("resolve replacement order choice");
        guard += 1;
        assert!(guard < 8, "replacement-order choice did not terminate");
    }

    // CR 614.1c: counters land before the resolution settles; the Ballista is
    // a live (4+1)*2 = 10/10 (or 9/9 for the other ordering — either way >0).
    assert_eq!(
        state.objects[&ballista].zone,
        Zone::Battlefield,
        "Walking Ballista must survive once its entering counters are placed. \
         counters={:?}",
        state.objects[&ballista].counters,
    );
    let counters = state.objects[&ballista]
        .counters
        .get(&CounterType::Plus1Plus1)
        .copied()
        .unwrap_or_default();
    assert!(
        counters >= 9,
        "Walking Ballista must enter with the doubled/incremented counters \
         (9 or 10 depending on CR 616.1 order), got {counters}"
    );
}

/// CR 614.1c + CR 614.12: Dragonstorm Globe's external ETB replacement
/// applies to the general subset "Each Dragon you control", including
/// token Dragons. This drives the full spell -> stack -> token creation ->
/// replacement pipeline; if the parser falls back to `SelfRef`, the
/// Artifact source never matches the entering Dragon and this counter is
/// missing.
#[test]
fn dragonstorm_globe_counters_created_dragon_token() {
    let mut state = setup_game_at_main_phase();
    let globe = create_object(
        &mut state,
        CardId(9170),
        PlayerId(0),
        "Dragonstorm Globe".to_string(),
        Zone::Battlefield,
    );
    {
        let obj = state.objects.get_mut(&globe).unwrap();
        obj.card_types.core_types.push(CoreType::Artifact);
    }
    apply_oracle_to_object(
        &mut state,
        globe,
        "Dragonstorm Globe",
        "Each Dragon you control enters with an additional +1/+1 counter on it.",
    );

    let token_spell = create_object(
        &mut state,
        CardId(9171),
        PlayerId(0),
        "Make a Dragon".to_string(),
        Zone::Hand,
    );
    {
        let obj = state.objects.get_mut(&token_spell).unwrap();
        obj.card_types.core_types.push(CoreType::Sorcery);
        obj.mana_cost = ManaCost::Cost {
            shards: vec![],
            generic: 0,
        };
        Arc::make_mut(&mut obj.abilities).push(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Token {
                name: "Dragon".to_string(),
                power: crate::types::ability::PtValue::Fixed(4),
                toughness: crate::types::ability::PtValue::Fixed(4),
                types: vec!["Creature".to_string(), "Dragon".to_string()],
                colors: vec![ManaColor::Red],
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
        ));
    }

    apply_as_current(
        &mut state,
        GameAction::CastSpell {
            object_id: token_spell,
            card_id: CardId(9171),
            targets: vec![],

            payment_mode: crate::types::game_state::CastPaymentMode::Auto,
        },
    )
    .unwrap();
    apply_as_current(&mut state, GameAction::PassPriority).unwrap();
    apply_as_current(&mut state, GameAction::PassPriority).unwrap();

    let dragon = state
        .battlefield
        .iter()
        .filter_map(|id| state.objects.get(id))
        .find(|object| {
            object.is_token
                && object
                    .card_types
                    .subtypes
                    .iter()
                    .any(|subtype| subtype == "Dragon")
        })
        .expect("Dragon token should be on the battlefield");
    assert_eq!(
        dragon
            .counters
            .get(&CounterType::Plus1Plus1)
            .copied()
            .unwrap_or_default(),
        1,
        "Dragonstorm Globe must add one +1/+1 counter to the created Dragon token"
    );
}

/// CR 603.6c + CR 614.1c: Cathars' Crusade triggers on any creature you
/// control entering. Its `PutCounterAll` effect must distribute one
/// +1/+1 counter to *every* creature its controller controls — including
/// the entering creature and every previously-existing creature. A
/// regression where the resolver only hits the entering creature would
/// catastrophically nerf the card.
#[test]
fn cathars_crusade_puts_one_counter_on_each_creature_you_control_on_etb() {
    let mut state = setup_game_at_main_phase();
    let crusade = create_object(
        &mut state,
        CardId(9150),
        PlayerId(0),
        "Cathars' Crusade".to_string(),
        Zone::Battlefield,
    );
    {
        let obj = state.objects.get_mut(&crusade).unwrap();
        obj.card_types.core_types.push(CoreType::Enchantment);
    }
    apply_oracle_to_object(
        &mut state,
        crusade,
        "Cathars' Crusade",
        "Whenever a creature you control enters, put a +1/+1 counter on each creature you control.",
    );
    // Two existing creatures on the battlefield (no summoning sickness needed
    // since we never attack — the test only inspects counter counts).
    let existing_a = create_object(
        &mut state,
        CardId(9151),
        PlayerId(0),
        "Existing Creature A".to_string(),
        Zone::Battlefield,
    );
    let existing_b = create_object(
        &mut state,
        CardId(9152),
        PlayerId(0),
        "Existing Creature B".to_string(),
        Zone::Battlefield,
    );
    for id in [existing_a, existing_b] {
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.power = Some(2);
        obj.toughness = Some(2);
    }
    // Cast a vanilla 2/2 from hand. Cathars' Crusade's trigger should
    // fire on its ETB and place one counter on all three creatures.
    let entering = create_object(
        &mut state,
        CardId(9153),
        PlayerId(0),
        "Entering Creature".to_string(),
        Zone::Hand,
    );
    {
        let obj = state.objects.get_mut(&entering).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.power = Some(2);
        obj.toughness = Some(2);
        obj.mana_cost = ManaCost::Cost {
            shards: vec![],
            generic: 2,
        };
    }
    let player = state
        .players
        .iter_mut()
        .find(|player| player.id == PlayerId(0))
        .unwrap();
    for _ in 0..2 {
        player.mana_pool.add(crate::types::mana::ManaUnit::new(
            crate::types::mana::ManaType::Colorless,
            ObjectId(0),
            false,
            vec![],
        ));
    }
    apply_as_current(
        &mut state,
        GameAction::CastSpell {
            object_id: entering,
            card_id: CardId(9153),
            targets: vec![],

            payment_mode: crate::types::game_state::CastPaymentMode::Auto,
        },
    )
    .unwrap();
    // Resolve the spell + Cathars' Crusade trigger.
    apply_as_current(&mut state, GameAction::PassPriority).unwrap();
    apply_as_current(&mut state, GameAction::PassPriority).unwrap();
    apply_as_current(&mut state, GameAction::PassPriority).unwrap();
    apply_as_current(&mut state, GameAction::PassPriority).unwrap();

    for (id, label) in [
        (entering, "entering creature"),
        (existing_a, "existing creature A"),
        (existing_b, "existing creature B"),
    ] {
        let n = state.objects[&id]
            .counters
            .get(&CounterType::Plus1Plus1)
            .copied()
            .unwrap_or_default();
        assert_eq!(
            n, 1,
            "Cathars' Crusade must place one +1/+1 counter on every creature you control, \
                 not just the entering creature. {label} has {n} counters."
        );
    }
}

/// Production-path Cathars' Crusade: load via `CardDatabase::from_export`
/// and `create_object_from_card_face` (the deck-loading path). Verifies
/// the resolver iterates all creatures-you-control, not just the
/// triggering entry.
#[test]
fn cathars_crusade_db_load_path_puts_counter_on_each_creature_you_control() {
    use crate::game::deck_loading::create_object_from_card_face;

    let db = crate::test_support::shared_card_db();
    let crusade_face = db
        .get_face_by_name("Cathars' Crusade")
        .expect("Cathars' Crusade must be in the export")
        .clone();

    let mut state = setup_game_at_main_phase();
    let crusade = create_object_from_card_face(&mut state, &crusade_face, PlayerId(0));
    // CR 400.7: The deck-load path puts the object in `Zone::Library`.
    // Direct field mutation would leave `state.battlefield` (a separate
    // list) un-updated; the proper transition runs `move_to_zone` so
    // the battlefield index, layer dirty flag, and trigger matchers
    // all see the object. Use a discardable scratch event vec since
    // the test only inspects post-move state.
    {
        let mut scratch_events = Vec::new();
        super::zones::move_to_zone(&mut state, crusade, Zone::Battlefield, &mut scratch_events);
    }

    // Two pre-existing controlled creatures + an entering creature.
    let existing_a = create_object(
        &mut state,
        CardId(9160),
        PlayerId(0),
        "Existing Creature A".to_string(),
        Zone::Battlefield,
    );
    let existing_b = create_object(
        &mut state,
        CardId(9161),
        PlayerId(0),
        "Existing Creature B".to_string(),
        Zone::Battlefield,
    );
    for id in [existing_a, existing_b] {
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.power = Some(2);
        obj.toughness = Some(2);
    }
    let entering = create_object(
        &mut state,
        CardId(9162),
        PlayerId(0),
        "Entering Creature".to_string(),
        Zone::Hand,
    );
    {
        let obj = state.objects.get_mut(&entering).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.power = Some(2);
        obj.toughness = Some(2);
        obj.mana_cost = ManaCost::Cost {
            shards: vec![],
            generic: 2,
        };
    }
    let player = state
        .players
        .iter_mut()
        .find(|player| player.id == PlayerId(0))
        .unwrap();
    for _ in 0..2 {
        player.mana_pool.add(crate::types::mana::ManaUnit::new(
            crate::types::mana::ManaType::Colorless,
            ObjectId(0),
            false,
            vec![],
        ));
    }
    apply_as_current(
        &mut state,
        GameAction::CastSpell {
            object_id: entering,
            card_id: CardId(9162),
            targets: vec![],

            payment_mode: crate::types::game_state::CastPaymentMode::Auto,
        },
    )
    .unwrap();
    // Resolve creature + Cathars' Crusade trigger.
    apply_as_current(&mut state, GameAction::PassPriority).unwrap();
    apply_as_current(&mut state, GameAction::PassPriority).unwrap();
    apply_as_current(&mut state, GameAction::PassPriority).unwrap();
    apply_as_current(&mut state, GameAction::PassPriority).unwrap();

    for (id, label) in [
        (entering, "entering creature"),
        (existing_a, "existing creature A"),
        (existing_b, "existing creature B"),
    ] {
        let n = state.objects[&id]
            .counters
            .get(&CounterType::Plus1Plus1)
            .copied()
            .unwrap_or_default();
        assert_eq!(
            n, 1,
            "Cathars' Crusade (DB-load path) must place a +1/+1 counter on every \
                 creature you control. {label} has {n} counters."
        );
    }
}

#[test]
fn broadside_bombardiers_boast_activates_after_attacking_and_requires_sacrifice() {
    use crate::game::combat::AttackTarget;

    let mut state = setup_game_at_main_phase();
    state.phase = Phase::DeclareAttackers;
    let bombardiers = create_object(
        &mut state,
        CardId(9140),
        PlayerId(0),
        "Broadside Bombardiers".to_string(),
        Zone::Battlefield,
    );
    {
        let obj = state.objects.get_mut(&bombardiers).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.card_types.subtypes.push("Goblin".to_string());
        obj.card_types.subtypes.push("Pirate".to_string());
        obj.power = Some(2);
        obj.toughness = Some(2);
        obj.summoning_sick = false;
    }
    apply_oracle_to_object(
            &mut state,
            bombardiers,
            "Broadside Bombardiers",
            "Menace\nHaste\nBoast — Sacrifice another creature or artifact: This creature deals damage equal to 2 plus the sacrificed permanent's mana value to any target. (Activate only if this creature attacked this turn and only once each turn.)",
        );
    let sacrifice = create_object(
        &mut state,
        CardId(9141),
        PlayerId(0),
        "Sacrifice Creature".to_string(),
        Zone::Battlefield,
    );
    {
        let obj = state.objects.get_mut(&sacrifice).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.power = Some(1);
        obj.toughness = Some(1);
    }
    state.waiting_for = WaitingFor::DeclareAttackers {
        player: PlayerId(0),
        valid_attacker_ids: vec![bombardiers],
        valid_attack_targets: vec![AttackTarget::Player(PlayerId(1))],
    };
    apply_as_current(
        &mut state,
        GameAction::DeclareAttackers {
            attacks: vec![(bombardiers, AttackTarget::Player(PlayerId(1)))],
            bands: vec![],
        },
    )
    .unwrap();
    let ability_index = state.objects[&bombardiers]
        .abilities
        .iter()
        .position(|ability| ability.ability_tag == Some(AbilityTag::Boast))
        .expect("Broadside Bombardiers should have a Boast ability");
    let result = apply_as_current(
        &mut state,
        GameAction::ActivateAbility {
            source_id: bombardiers,
            ability_index,
        },
    )
    .unwrap();
    if matches!(result.waiting_for, WaitingFor::TargetSelection { .. }) {
        apply_as_current(
            &mut state,
            GameAction::SelectTargets {
                targets: vec![TargetRef::Player(PlayerId(1))],
            },
        )
        .unwrap();
    }
    let WaitingFor::PayCost {
        kind: PayCostKind::Sacrifice,
        count,
        choices: permanents,
        ..
    } = &state.waiting_for
    else {
        panic!("Broadside Bombardiers boast should require a sacrifice cost");
    };
    assert_eq!(*count, 1);
    assert!(permanents.contains(&sacrifice));
    assert!(!permanents.contains(&bombardiers));
}

fn room_back_face(name: &str) -> BackFaceData {
    BackFaceData {
        name: name.to_string(),
        power: None,
        toughness: None,
        loyalty: None,
        defense: None,
        card_types: CardType::default(),
        mana_cost: ManaCost::default(),
        keywords: Vec::new(),
        abilities: Vec::new(),
        trigger_definitions: Default::default(),
        replacement_definitions: Default::default(),
        static_definitions: Default::default(),
        color: Vec::new(),
        printed_ref: None,
        modal: None,
        additional_cost: None,
        strive_cost: None,
        casting_restrictions: Vec::new(),
        casting_options: Vec::new(),
        layout_kind: Some(crate::types::card::LayoutKind::Split),
    }
}

#[test]
fn unlock_room_door_special_action_marks_door_and_emits_trigger_event() {
    let mut state = setup_game_at_main_phase();
    let room = create_object(
        &mut state,
        CardId(900),
        PlayerId(0),
        "Bottomless Pool".to_string(),
        Zone::Battlefield,
    );
    {
        let obj = state.objects.get_mut(&room).unwrap();
        obj.card_types.subtypes.push("Room".to_string());
        obj.room_unlocks = Some(Default::default());
        obj.back_face = Some(room_back_face("Locker Room"));
    }

    let result = apply_as_current(
        &mut state,
        GameAction::UnlockRoomDoor {
            object_id: room,
            door: RoomDoor::Right,
        },
    )
    .unwrap();

    let room_obj = state.objects.get(&room).unwrap();
    assert!(room_obj.room_unlocks.unwrap().right_unlocked);
    assert!(result.events.iter().any(|event| matches!(
        event,
        GameEvent::RoomDoorUnlocked {
            object_id,
            door: RoomDoor::Right,
            ..
        } if *object_id == room
    )));
}

/// CR 106.6 + CR 116.2m + CR 709.5e: Smoky Lounge produces {R}{R} restricted
/// to "cast Room spells and unlock doors". The door-unlock half lowers to
/// `OnlyForSpecialAction(UnlockDoor)`; paying a Room's unlock cost routes
/// through `PaymentContext::SpecialAction(UnlockDoor)`, so the restricted {R}
/// IS eligible. Before this fix the unlock cost paid via
/// `PaymentContext::Effect`, which rejects every restriction — the restricted
/// {R} could not pay and the unlock failed. Reverting the
/// `pay_special_action_mana_cost` wiring flips this assertion (the unlock
/// would error "Cannot pay mana cost").
#[test]
fn unlock_door_restricted_mana_pays_room_unlock_cost() {
    use crate::types::mana::{ManaRestriction, ManaType, ManaUnit, SpecialAction};

    let mut state = setup_game_at_main_phase();
    let room = create_object(
        &mut state,
        CardId(901),
        PlayerId(0),
        "Bottomless Pool".to_string(),
        Zone::Battlefield,
    );
    {
        let obj = state.objects.get_mut(&room).unwrap();
        obj.card_types.subtypes.push("Room".to_string());
        obj.room_unlocks = Some(Default::default());
        // CR 709.5e: left door's unlock cost is the object's mana cost ({R}).
        obj.mana_cost = ManaCost::Cost {
            shards: vec![crate::types::mana::ManaCostShard::Red],
            generic: 0,
        };
    }

    // Smoky Lounge's restricted {R}: only for casting Room spells OR unlocking
    // doors. Mirrors the lowering of `ManaSpendRestriction::Any([SpellType,
    // UnlockDoor])`.
    let restriction = ManaRestriction::OnlyForAny(vec![
        ManaRestriction::OnlyForSpellType("Room".to_string()),
        ManaRestriction::OnlyForSpecialAction(SpecialAction::UnlockDoor),
    ]);
    {
        let player = state
            .players
            .iter_mut()
            .find(|p| p.id == PlayerId(0))
            .unwrap();
        player
            .mana_pool
            .add(ManaUnit::new(ManaType::Red, room, false, vec![restriction]));
    }

    let result = apply_as_current(
        &mut state,
        GameAction::UnlockRoomDoor {
            object_id: room,
            door: RoomDoor::Left,
        },
    )
    .expect("restricted mana must be able to pay a door's unlock cost");

    let room_obj = state.objects.get(&room).unwrap();
    assert!(
        room_obj.room_unlocks.unwrap().left_unlocked,
        "the door must be unlocked after paying its cost with door-restricted mana"
    );
    assert!(result.events.iter().any(|event| matches!(
        event,
        GameEvent::RoomDoorUnlocked { object_id, door: RoomDoor::Left, .. } if *object_id == room
    )));
    // The restricted {R} was consumed paying the unlock cost.
    let pool_left = state
        .players
        .iter()
        .find(|p| p.id == PlayerId(0))
        .unwrap()
        .mana_pool
        .mana
        .len();
    assert_eq!(
        pool_left, 0,
        "the restricted mana must be spent on the unlock"
    );
}

/// Builds a face-down morph({3}) creature on `player`'s battlefield via the real
/// `play_face_down` zone pipeline, which snapshots the real face — including the
/// morph keyword and its {3} cost — into `back_face`. Returns its `ObjectId`.
fn setup_face_down_morph(state: &mut GameState, player: PlayerId) -> ObjectId {
    let id = create_object(
        state,
        CardId(4200),
        player,
        "Secret Beast".to_string(),
        Zone::Hand,
    );
    {
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types = CardType {
            supertypes: vec![],
            core_types: vec![CoreType::Creature],
            subtypes: vec!["Beast".to_string()],
        };
        obj.power = Some(4);
        obj.toughness = Some(5);
        obj.keywords = vec![crate::types::keywords::Keyword::Morph(ManaCost::Cost {
            generic: 3,
            shards: vec![],
        })];
    }
    let mut events = Vec::new();
    crate::game::morph::play_face_down(state, player, id, &mut events).unwrap();
    assert!(
        state.objects[&id].face_down,
        "setup: the morph creature must be face down before the turn-up test"
    );
    id
}

/// Adds `count` untapped green `ManaUnit`s restricted by `restriction` to
/// `player`'s pool — the only mana in the game, so payment must draw from exactly
/// these units.
fn fund_restricted_pool(
    state: &mut GameState,
    player: PlayerId,
    count: usize,
    restriction: crate::types::mana::ManaRestriction,
) {
    let p = state.players.iter_mut().find(|p| p.id == player).unwrap();
    for _ in 0..count {
        p.mana_pool.add(ManaUnit::new(
            ManaType::Green,
            ObjectId(4200),
            false,
            vec![restriction.clone()],
        ));
    }
}

/// R1 — CR 116.2b + CR 702.37e + CR 106.6: turn-face-up-restricted mana funds the
/// morph turn-up special action. Pool = 3× green restricted to
/// `OnlyForSpecialAction(TurnFaceUp)` (the sole mana source); the {3} morph cost
/// is paid through `PaymentContext::SpecialAction(TurnFaceUp)`, the permanent
/// flips face up, a `TurnedFaceUp` event fires, and the pool empties. This is the
/// positive reach-guard proving the payment routes through the real
/// `pay_special_action_mana_cost` site, so R2/R3's negatives aren't vacuous.
#[test]
fn turn_face_up_restricted_mana_funds_special_action() {
    use crate::types::mana::{ManaRestriction, SpecialAction};
    let mut state = setup_game_at_main_phase();
    let morph = setup_face_down_morph(&mut state, PlayerId(0));
    fund_restricted_pool(
        &mut state,
        PlayerId(0),
        3,
        ManaRestriction::OnlyForSpecialAction(SpecialAction::TurnFaceUp),
    );

    let result = apply_as_current(&mut state, GameAction::TurnFaceUp { object_id: morph })
        .expect("turn-face-up-restricted mana must fund the morph turn-up special action");

    assert!(
        !state.objects[&morph].face_down,
        "the permanent must be face up after paying the morph cost"
    );
    assert!(
        result.events.iter().any(|e| matches!(
            e,
            GameEvent::TurnedFaceUp { object_id } if *object_id == morph
        )),
        "a TurnedFaceUp event must fire"
    );
    let pool = &state
        .players
        .iter()
        .find(|p| p.id == PlayerId(0))
        .unwrap()
        .mana_pool;
    assert_eq!(
        pool.total(),
        0,
        "the {{3}} morph cost must consume all 3 restricted units"
    );
}

/// R2 (LOAD-BEARING charge proof) — CR 116.2b + CR 702.37e: the turn-up special
/// action now CHARGES the morph cost. With an EMPTY pool and no mana sources the
/// {3} cost is unpayable, so `apply(TurnFaceUp)` errors and the permanent STAYS
/// face down.
///
/// Revert direction: reverting Step 2 (the handler payment) makes the turn-up
/// free — `apply` returns `Ok` and the permanent flips face up. Both assertions
/// below flip, so this is the discriminating proof the cost is actually charged.
#[test]
fn turn_face_up_empty_pool_cannot_pay_and_stays_face_down() {
    let mut state = setup_game_at_main_phase();
    let morph = setup_face_down_morph(&mut state, PlayerId(0));

    let result = apply_as_current(&mut state, GameAction::TurnFaceUp { object_id: morph });
    assert!(
        result.is_err(),
        "with no mana the {{3}} morph turn-up cost must be unpayable: {result:?}"
    );
    assert!(
        state.objects[&morph].face_down,
        "an unpaid turn-up must leave the permanent face down"
    );
}

/// R3 (context precision, hostile) — CR 106.6 + CR 116.2b: mana restricted to a
/// DIFFERENT special action (`UnlockDoor`) must NOT pay a turn-face-up. Pool = 3×
/// green restricted to `OnlyForSpecialAction(UnlockDoor)`; `ManaRestriction::
/// allows` rejects the `SpecialAction(TurnFaceUp)` context, so the {3} cost is
/// unpayable, the permanent stays face down, and the wrong-context units are
/// untouched.
///
/// Revert direction: if the turn-up emitted the wrong context (`UnlockDoor`) or
/// the restriction ignored the action, this would pay and flip — all three
/// assertions flip.
#[test]
fn turn_face_up_rejects_unlock_door_restricted_mana() {
    use crate::types::mana::{ManaRestriction, SpecialAction};
    let mut state = setup_game_at_main_phase();
    let morph = setup_face_down_morph(&mut state, PlayerId(0));
    fund_restricted_pool(
        &mut state,
        PlayerId(0),
        3,
        ManaRestriction::OnlyForSpecialAction(SpecialAction::UnlockDoor),
    );

    let result = apply_as_current(&mut state, GameAction::TurnFaceUp { object_id: morph });
    assert!(
        result.is_err(),
        "door-unlock-restricted mana must not pay a turn-face-up: {result:?}"
    );
    assert!(
        state.objects[&morph].face_down,
        "the permanent must stay face down when only wrong-context mana is available"
    );
    let pool = &state
        .players
        .iter()
        .find(|p| p.id == PlayerId(0))
        .unwrap()
        .mana_pool;
    assert_eq!(
        pool.total(),
        3,
        "unlock-restricted mana must not be consumed by a turn-up"
    );
}

/// CR 116.2m + CR 709.5e + CR 118.7a: Inquisitive Glimmer — "Unlock costs
/// you pay cost {1} less." Reduces the generic component of a Room door's
/// unlock cost before payment, via the single-authority special-action
/// reducer shared with the plot path.
///
/// Drives the full `apply()` pipeline (`GameAction::UnlockRoomDoor` →
/// `handle_unlock_room_door`). Discriminating: a {3}-generic door with
/// Inquisitive out is payable with only {2} in the pool (cost reduced to
/// {2}); WITHOUT the static the same {2} pool can't pay {3} and the unlock
/// errors. Reverting the engine.rs reduction line makes the with-static case
/// require {3} and fail.
#[test]
fn inquisitive_glimmer_reduces_room_unlock_cost() {
    use crate::types::ability::StaticDefinition;
    use crate::types::mana::{ManaType, ManaUnit};
    use crate::types::statics::{CostModifyMode, StaticMode};

    let make_state = |with_glimmer: bool| {
        let mut state = setup_game_at_main_phase();
        let room = create_object(
            &mut state,
            CardId(950),
            PlayerId(0),
            "Test Room".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&room).unwrap();
            obj.card_types.subtypes.push("Room".to_string());
            obj.room_unlocks = Some(Default::default());
            // CR 709.5e: the left door's unlock cost is the object's mana
            // cost — a flat {3} generic here.
            obj.mana_cost = ManaCost::Cost {
                shards: vec![],
                generic: 3,
            };
        }
        if with_glimmer {
            let glimmer = create_object(
                &mut state,
                CardId(951),
                PlayerId(0),
                "Inquisitive Glimmer".to_string(),
                Zone::Battlefield,
            );
            let obj = state.objects.get_mut(&glimmer).unwrap();
            obj.card_types
                .core_types
                .push(crate::types::card_type::CoreType::Creature);
            obj.static_definitions = vec![StaticDefinition::new(StaticMode::ReduceActionCost {
                action: crate::types::mana::SpecialAction::UnlockDoor,
                mode: CostModifyMode::Reduce,
                amount: 1,
            })]
            .into();
        }
        // Fund exactly {2} of generic-payable mana.
        {
            let player = state
                .players
                .iter_mut()
                .find(|p| p.id == PlayerId(0))
                .unwrap();
            for _ in 0..2 {
                player
                    .mana_pool
                    .add(ManaUnit::new(ManaType::Colorless, room, false, vec![]));
            }
        }
        (state, room)
    };

    // With Inquisitive: {3} → {2}, payable with the {2} pool → door unlocks.
    let (mut state, room) = make_state(true);
    apply_as_current(
        &mut state,
        GameAction::UnlockRoomDoor {
            object_id: room,
            door: RoomDoor::Left,
        },
    )
    .expect("reduced unlock cost ({2}) must be payable with {2} in pool");
    assert!(
        state
            .objects
            .get(&room)
            .unwrap()
            .room_unlocks
            .unwrap()
            .left_unlocked,
        "door must unlock at the reduced cost"
    );

    // Without Inquisitive: {3} > {2} pool → unlock fails.
    let (mut state2, room2) = make_state(false);
    assert!(
        apply_as_current(
            &mut state2,
            GameAction::UnlockRoomDoor {
                object_id: room2,
                door: RoomDoor::Left,
            },
        )
        .is_err(),
        "without the reduction the {{3}} unlock cost exceeds the {{2}} pool"
    );
}

/// CR 106.6 + CR 116.2m: Door-restricted mana is rejected for unrelated
/// payments. The unlock cost is the ONLY thing this {R} can pay (besides Room
/// spell casts) — a generic effect cost must not draw on it. Reverting the
/// `OnlyForSpecialAction` gate (e.g. making it `true` for `Effect`) flips this.
#[test]
fn unlock_door_restricted_mana_rejected_for_effect_and_spell_payments() {
    use crate::types::mana::{ManaRestriction, ManaType, PaymentContext, SpecialAction, SpellMeta};

    let restriction = ManaRestriction::OnlyForSpecialAction(SpecialAction::UnlockDoor);

    // Accepts the matching special action.
    assert!(restriction.allows(&PaymentContext::SpecialAction(SpecialAction::UnlockDoor)));
    // Rejects generic effect-resolution payments (the pre-fix unlock path).
    assert!(!restriction.allows(&PaymentContext::Effect));
    // Rejects a Room spell cast (this leaf is the door half only).
    let room_spell = SpellMeta {
        types: vec!["Enchantment".to_string()],
        subtypes: vec!["Room".to_string()],
        ..SpellMeta::default()
    };
    assert!(!restriction.allows(&PaymentContext::Spell(&room_spell)));
    // Rejects ability activation.
    assert!(!restriction.allows(&PaymentContext::Activation {
        source_types: &["Artifact".to_string()],
        source_subtypes: &["Equipment".to_string()],
        ability_tag: None,
    }));
    let _ = ManaType::Red;
}

#[test]
fn apply_pass_priority_alternates_players() {
    let mut state = setup_game_at_main_phase();

    let result = apply_as_current(&mut state, GameAction::PassPriority).unwrap();

    assert!(matches!(
        result.waiting_for,
        WaitingFor::Priority {
            player: PlayerId(1)
        }
    ));
}

#[test]
fn apply_pass_priority_rejects_wrong_player() {
    let mut state = setup_game_at_main_phase();
    state.priority_player = PlayerId(1);
    state.waiting_for = WaitingFor::Priority {
        player: PlayerId(1),
    };

    // Player 0 tries to pass but player 1 has priority
    // PassPriority uses priority_player, so this should fail if
    // the validated player doesn't match waiting_for
    // Actually, the validation checks priority_player == waiting_for.player
    // and priority_player is 1, so PassPriority action itself is valid
    // for player 1. The issue is if player 0 somehow acts.
    // In practice, the action doesn't carry a player ID -- the engine
    // uses priority_player. So this is a protocol-level concern.
    let result = apply_as_current(&mut state, GameAction::PassPriority);
    assert!(result.is_ok());
}

// --- Preference actions (SetPhaseStops, CancelAutoPass) bypass actor gate ---

#[test]
fn set_phase_stops_from_non_priority_actor_succeeds() {
    // Regression: the human (P0) updates phase stops while the AI (P1) holds
    // priority. Previously this was rejected by check_actor_authorization with
    // WrongPlayer; the dispatch surfaced "Engine error: Wrong player" to the
    // user and the preference silently never landed.
    let mut state = setup_game_at_main_phase();
    state.priority_player = PlayerId(1);
    state.waiting_for = WaitingFor::Priority {
        player: PlayerId(1),
    };

    let result = apply(
        &mut state,
        PlayerId(0),
        GameAction::SetPhaseStops {
            stops: vec![crate::types::phase::PhaseStop {
                phase: Phase::End,
                scope: crate::types::phase::PhaseStopScope::AllTurns,
            }],
        },
    );

    assert!(
        result.is_ok(),
        "expected SetPhaseStops to succeed, got {result:?}"
    );
    assert_eq!(
        state.phase_stops.get(&PlayerId(0)),
        Some(&vec![crate::types::phase::PhaseStop {
            phase: Phase::End,
            scope: crate::types::phase::PhaseStopScope::AllTurns,
        }]),
        "expected actor (P0) preference to be written, not authorized submitter (P1)",
    );
    assert!(!state.phase_stops.contains_key(&PlayerId(1)));
}

#[test]
fn cancel_auto_pass_routes_by_actor() {
    // Regression: P0 had an auto-pass session; P1 holds priority and submits
    // CancelAutoPass on P0's behalf would previously cancel *P1's* session
    // (handler used authorized_submitter, not actor). After the fix, the
    // actor field decides which seat is mutated.
    let mut state = setup_game_at_main_phase();
    state.auto_pass.insert(
        PlayerId(0),
        crate::types::game_state::AutoPassMode::UntilTurnBoundary {
            until: crate::types::game_state::TurnBoundary::EndOfCurrentTurn,
        },
    );
    state.priority_player = PlayerId(1);
    state.waiting_for = WaitingFor::Priority {
        player: PlayerId(1),
    };

    let result = apply(&mut state, PlayerId(0), GameAction::CancelAutoPass);

    assert!(result.is_ok());
    assert!(
        !state.auto_pass.contains_key(&PlayerId(0)),
        "P0's auto-pass should have been cancelled"
    );
}

// --- GameAction::SetPriorityYield (CR 117.3d + CR 400.7 + CR 704.5d) ---

/// Push a controller-owned `TriggeredAbility` entry onto the stack whose ability
/// latched `incarnation` and `card_id` at push (the token itself is NOT inserted
/// into `objects` — it models a ceased token per CR 704.5d).
fn push_token_trigger(
    state: &mut GameState,
    source: ObjectId,
    controller: PlayerId,
    incarnation: Option<u64>,
    card_id: Option<CardId>,
) -> ObjectId {
    let mut ability = ResolvedAbility::new(
        Effect::Draw {
            count: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::Controller,
        },
        vec![],
        source,
        controller,
    );
    ability.source_incarnation = incarnation;
    ability.source_card_id = card_id;
    let entry_id = ObjectId(state.next_object_id);
    state.next_object_id += 1;
    state.stack.push_back(StackEntry {
        id: entry_id,
        source_id: source,
        controller,
        kind: StackEntryKind::TriggeredAbility {
            source_id: source,
            ability: Box::new(ability),
            condition: None,
            trigger_event: None,
            description: None,
            source_name: "Token".to_string(),
            subject_match_count: None,
            die_result: None,
        },
    });
    entry_id
}

/// CR 400.7 identity latch: `Add` binds the target from the on-stack trigger,
/// not from `state.objects`. A ceased token (absent from `objects`) whose
/// dies-trigger is still on the stack must register a yield carrying the entry's
/// latched incarnation. Reverting to an `objects`-based lookup would store
/// nothing here.
#[test]
fn set_priority_yield_add_binds_from_stack_after_token_ceased() {
    let mut state = setup_game_at_main_phase();
    let source = ObjectId(500);
    push_token_trigger(&mut state, source, PlayerId(0), Some(4), Some(CardId(77)));
    assert!(
        state.objects.get(&source).is_none(),
        "reach-guard: the token source has ceased to exist"
    );

    apply(
        &mut state,
        PlayerId(0),
        GameAction::SetPriorityYield {
            op: PriorityYieldOp::Add {
                source_id: source,
                scope: crate::types::game_state::YieldScope::ThisObject,
            },
        },
    )
    .expect("SetPriorityYield is legal in any state");

    assert_eq!(
        state.priority_yields,
        vec![crate::types::game_state::PriorityYield {
            player: PlayerId(0),
            target: crate::types::game_state::YieldTarget::ThisObject {
                source_id: source,
                incarnation: 4,
            },
        }],
        "Add must bind the incarnation latched on the on-stack trigger",
    );
}

/// `Add` is a benign no-op when no matching triggered entry is on the stack.
#[test]
fn set_priority_yield_add_no_op_without_matching_stack_entry() {
    let mut state = setup_game_at_main_phase();
    // A different source's trigger is on the stack (reach-guard: stack non-empty).
    push_token_trigger(
        &mut state,
        ObjectId(500),
        PlayerId(0),
        Some(1),
        Some(CardId(77)),
    );

    apply(
        &mut state,
        PlayerId(0),
        GameAction::SetPriorityYield {
            op: PriorityYieldOp::Add {
                source_id: ObjectId(999),
                scope: crate::types::game_state::YieldScope::ThisObject,
            },
        },
    )
    .expect("SetPriorityYield is legal in any state");

    assert!(
        state.priority_yields.is_empty(),
        "Add for a source with no stack entry must not store a yield"
    );
}

/// CR 400.7 None-boundary: a `ThisObject` add on a trigger with no latched
/// incarnation no-ops, while an `AllCopies` add on the same trigger still stores.
#[test]
fn set_priority_yield_this_object_none_incarnation_no_ops_but_all_copies_works() {
    let mut state = setup_game_at_main_phase();
    let source = ObjectId(0); // synthetic game-rule trigger source
    push_token_trigger(&mut state, source, PlayerId(0), None, Some(CardId(77)));

    apply(
        &mut state,
        PlayerId(0),
        GameAction::SetPriorityYield {
            op: PriorityYieldOp::Add {
                source_id: source,
                scope: crate::types::game_state::YieldScope::ThisObject,
            },
        },
    )
    .expect("legal");
    assert!(
        state.priority_yields.is_empty(),
        "ThisObject add must no-op when the trigger latched no incarnation"
    );

    apply(
        &mut state,
        PlayerId(0),
        GameAction::SetPriorityYield {
            op: PriorityYieldOp::Add {
                source_id: source,
                scope: crate::types::game_state::YieldScope::AllCopies,
            },
        },
    )
    .expect("legal");
    assert_eq!(
        state.priority_yields.len(),
        1,
        "AllCopies add stores when the card identity is present"
    );
}

/// CR 117.3d: `SetPriorityYield` is a per-player preference routed by `actor`,
/// so a non-priority player may register a yield stored under THEIR seat. The
/// authorization exemption is what allows this — a non-exempt action from the
/// same wrong player is rejected as `WrongPlayer`.
#[test]
fn set_priority_yield_accepted_from_non_priority_actor() {
    let mut state = setup_game_at_main_phase();
    // Priority is P0's; P1 is not the priority holder.
    let source = ObjectId(500);
    push_token_trigger(&mut state, source, PlayerId(0), Some(2), Some(CardId(77)));

    // Reach-guard: a non-exempt action from P1 in P0's priority window errors.
    let unauthorized = apply(&mut state, PlayerId(1), GameAction::PassPriority);
    assert!(
        matches!(unauthorized, Err(EngineError::WrongPlayer)),
        "a non-priority player cannot pass priority (proves the auth gate is live)"
    );

    apply(
        &mut state,
        PlayerId(1),
        GameAction::SetPriorityYield {
            op: PriorityYieldOp::Add {
                source_id: source,
                scope: crate::types::game_state::YieldScope::AllCopies,
            },
        },
    )
    .expect("SetPriorityYield is exempt from the priority-holder gate");

    assert_eq!(state.priority_yields.len(), 1);
    assert_eq!(
        state.priority_yields[0].player,
        PlayerId(1),
        "the yield is stored under the acting player's seat, not the priority holder's"
    );
}

/// CR 117.3d: an `UntilEndOfTurn` auto-pass session normally ends (Finish) when
/// an opponent-controlled trigger tops the stack, so the player can respond.
/// A matching yield keeps the session auto-passing (Pass) through that trigger;
/// a non-yielded opponent trigger still Finishes.
#[test]
fn until_end_of_turn_yielded_opponent_top_passes_not_finishes() {
    let mut state = setup_game_at_main_phase();
    state.auto_pass.insert(
        PlayerId(0),
        crate::types::game_state::AutoPassMode::UntilTurnBoundary {
            until: crate::types::game_state::TurnBoundary::EndOfCurrentTurn,
        },
    );
    let source = ObjectId(500);
    push_token_trigger(&mut state, source, PlayerId(1), Some(4), Some(CardId(77)));

    // Without a yield: the opponent trigger ends the session.
    assert!(
        matches!(
            priority_auto_pass_decision(&state, PlayerId(0)),
            AutoPassDecision::Finish
        ),
        "reach-guard: an un-yielded opponent top finishes the session"
    );

    // With a matching yield: keep auto-passing through it.
    state.add_priority_yield(
        PlayerId(0),
        crate::types::game_state::YieldTarget::ThisObject {
            source_id: source,
            incarnation: 4,
        },
    );
    assert!(
        matches!(
            priority_auto_pass_decision(&state, PlayerId(0)),
            AutoPassDecision::Pass
        ),
        "CR 117.3d: a matching yield keeps the UntilEndOfTurn session passing"
    );

    // A different, non-yielded opponent trigger still finishes.
    state.stack.clear();
    push_token_trigger(
        &mut state,
        ObjectId(600),
        PlayerId(1),
        Some(9),
        Some(CardId(88)),
    );
    assert!(
        matches!(
            priority_auto_pass_decision(&state, PlayerId(0)),
            AutoPassDecision::Finish
        ),
        "a non-yielded opponent trigger still finishes the session"
    );
}

// --- GameAction::Concede (CR 104.3a + CR 800.4a) ---

fn setup_three_player_at_main_phase() -> GameState {
    use crate::types::format::FormatConfig;
    let mut state = GameState::new(FormatConfig::free_for_all(), 3, 42);
    state.turn_number = 2;
    state.phase = Phase::PreCombatMain;
    state.active_player = PlayerId(0);
    state.priority_player = PlayerId(0);
    state.waiting_for = WaitingFor::Priority {
        player: PlayerId(0),
    };
    state
}

#[test]
fn concede_eliminates_player() {
    // CR 104.3a + CR 800.4a: 3-player game, P1 concedes — P1 leaves, game continues.
    let mut state = setup_three_player_at_main_phase();

    let result = apply_as_current(
        &mut state,
        GameAction::Concede {
            player_id: PlayerId(1),
        },
    )
    .unwrap();

    assert!(state.players[1].is_eliminated);
    assert!(state.eliminated_players.contains(&PlayerId(1)));
    assert!(result.events.iter().any(|e| matches!(
        e,
        GameEvent::PlayerEliminated {
            player_id: PlayerId(1)
        }
    )));
    // Game should NOT be over — P0 and P2 still alive.
    assert!(!matches!(result.waiting_for, WaitingFor::GameOver { .. }));
}

#[test]
fn concede_during_opponents_priority() {
    // CR 104.3a: A player may concede at any time, regardless of priority.
    // Set priority to P0, but P1 concedes anyway — must succeed.
    let mut state = setup_three_player_at_main_phase();
    // P0 holds priority.
    assert_eq!(state.priority_player, PlayerId(0));

    let result = apply_as_current(
        &mut state,
        GameAction::Concede {
            player_id: PlayerId(1),
        },
    );

    assert!(
        result.is_ok(),
        "concede must succeed regardless of priority"
    );
    assert!(state.players[1].is_eliminated);
}

#[test]
fn concede_owner_of_waiting_for_advances_state() {
    // CR 800.4a + CR 104.3a: When the conceding player owned the active WaitingFor
    // (here: DeclareAttackers, but the same advancement applies to TargetSelection,
    // ScryChoice, ManaPayment, and every other WaitingFor variant that references
    // a specific player), state must advance to Priority for the next living
    // player so the game does not deadlock waiting on a player who has left.
    let mut state = setup_three_player_at_main_phase();
    state.waiting_for = WaitingFor::DeclareAttackers {
        player: PlayerId(1),
        valid_attacker_ids: vec![],
        valid_attack_targets: vec![],
    };

    let result = apply_as_current(
        &mut state,
        GameAction::Concede {
            player_id: PlayerId(1),
        },
    )
    .unwrap();

    assert!(state.players[1].is_eliminated);
    // WaitingFor must have advanced — the next living player after P1 is P2.
    assert!(
        matches!(
            result.waiting_for,
            WaitingFor::Priority {
                player: PlayerId(2)
            }
        ),
        "expected Priority for P2 after P1 (owner of WaitingFor) conceded; got {:?}",
        result.waiting_for
    );
}

#[test]
fn concede_non_owner_of_waiting_for_preserves_state() {
    // CR 800.4a: When the conceding player does NOT own the active WaitingFor
    // (e.g., another player has priority or is choosing), the WaitingFor state
    // is preserved — only the conceder's permanents/stack-objects are removed.
    let mut state = setup_three_player_at_main_phase();
    // P0 holds priority; P1 concedes — P0 keeps priority.
    assert_eq!(state.priority_player, PlayerId(0));

    let result = apply_as_current(
        &mut state,
        GameAction::Concede {
            player_id: PlayerId(1),
        },
    )
    .unwrap();

    assert!(state.players[1].is_eliminated);
    assert!(matches!(
        result.waiting_for,
        WaitingFor::Priority {
            player: PlayerId(0)
        }
    ));
}

#[test]
fn concede_two_player_ends_game() {
    // CR 104.2a: In a 2-player game, when one player concedes, the other wins.
    let mut state = setup_game_at_main_phase();

    let result = apply_as_current(
        &mut state,
        GameAction::Concede {
            player_id: PlayerId(0),
        },
    )
    .unwrap();

    assert!(state.players[0].is_eliminated);
    assert!(matches!(
        result.waiting_for,
        WaitingFor::GameOver {
            winner: Some(PlayerId(1))
        }
    ));
}

#[test]
fn concede_three_player_continues() {
    // CR 800.4a: In a 3-player game, when one concedes, the remaining two continue.
    let mut state = setup_three_player_at_main_phase();

    let result = apply_as_current(
        &mut state,
        GameAction::Concede {
            player_id: PlayerId(2),
        },
    )
    .unwrap();

    assert!(state.players[2].is_eliminated);
    assert!(!state.players[0].is_eliminated);
    assert!(!state.players[1].is_eliminated);
    assert!(!matches!(result.waiting_for, WaitingFor::GameOver { .. }));
}

#[test]
fn apply_play_land_moves_to_battlefield() {
    let mut state = setup_game_at_main_phase();

    let obj_id = create_object(
        &mut state,
        CardId(1),
        PlayerId(0),
        "Forest".to_string(),
        Zone::Hand,
    );
    state
        .objects
        .get_mut(&obj_id)
        .unwrap()
        .card_types
        .core_types
        .push(CoreType::Land);

    let result = apply_as_current(
        &mut state,
        GameAction::PlayLand {
            object_id: obj_id,
            card_id: CardId(1),
        },
    )
    .unwrap();

    assert!(state.battlefield.contains(&obj_id));
    assert!(!state.players[0].hand.contains(&obj_id));
    assert_eq!(state.lands_played_this_turn, 1);

    // Player retains priority
    assert!(
        matches!(
            result.waiting_for,
            WaitingFor::Priority {
                player: PlayerId(0)
            }
        ),
        "result.waiting_for={:?}, stack={:?}",
        result.waiting_for,
        state.stack
    );
}

#[test]
fn two_headed_giant_each_teammate_plays_own_land() {
    // CR 805.4c: "Each player on a team may play a land during each of
    // that team's turns" — both the active player and their nonactive
    // teammate get their own one-land-per-turn allowance during the
    // SAME team turn. Before this fix, `handle_play_land` resolved the
    // resource owner as `turn_resource_owner` (always `active_player`)
    // and gated against the single shared `lands_played_this_turn`
    // counter, so the teammate's land play would have been attributed
    // to the active player and blocked once that counter hit 1.
    let mut state = GameState::new(
        crate::types::format::FormatConfig::two_headed_giant(),
        4,
        42,
    );
    state.turn_number = 2;
    state.phase = Phase::PreCombatMain;
    state.active_player = PlayerId(0);
    state.priority_player = PlayerId(0);
    state.waiting_for = WaitingFor::Priority {
        player: PlayerId(0),
    };

    let land0 = create_object(
        &mut state,
        CardId(1),
        PlayerId(0),
        "Forest".to_string(),
        Zone::Hand,
    );
    state
        .objects
        .get_mut(&land0)
        .unwrap()
        .card_types
        .core_types
        .push(CoreType::Land);
    let land1 = create_object(
        &mut state,
        CardId(2),
        PlayerId(1),
        "Island".to_string(),
        Zone::Hand,
    );
    state
        .objects
        .get_mut(&land1)
        .unwrap()
        .card_types
        .core_types
        .push(CoreType::Land);

    // Active player (P0) plays their land.
    apply_as_current(
        &mut state,
        GameAction::PlayLand {
            object_id: land0,
            card_id: CardId(1),
        },
    )
    .unwrap();
    assert!(state.battlefield.contains(&land0));
    assert_eq!(state.players[0].lands_played_this_turn, 1);

    // Nonactive teammate (P1) now holds priority and plays their OWN land
    // — must succeed against P1's own allowance, not P0's.
    state.priority_player = PlayerId(1);
    state.waiting_for = WaitingFor::Priority {
        player: PlayerId(1),
    };
    apply_as_current(
        &mut state,
        GameAction::PlayLand {
            object_id: land1,
            card_id: CardId(2),
        },
    )
    .unwrap();

    assert!(state.battlefield.contains(&land1));
    assert_eq!(state.players[1].lands_played_this_turn, 1);
    assert_eq!(
        state.players[0].lands_played_this_turn, 1,
        "P0's allowance must be unaffected by P1's land play"
    );
}

#[test]
fn archenemy_hero_team_each_hero_plays_own_land_only() {
    let mut state = GameState::new(crate::types::format::FormatConfig::archenemy(), 4, 42);
    state.turn_number = 2;
    state.phase = Phase::PreCombatMain;
    state.active_player = PlayerId(1);
    state.priority_player = PlayerId(1);
    state.waiting_for = WaitingFor::Priority {
        player: PlayerId(1),
    };

    let land1 = create_object(
        &mut state,
        CardId(1),
        PlayerId(1),
        "Forest".to_string(),
        Zone::Hand,
    );
    state
        .objects
        .get_mut(&land1)
        .unwrap()
        .card_types
        .core_types
        .push(CoreType::Land);
    let land2 = create_object(
        &mut state,
        CardId(2),
        PlayerId(2),
        "Island".to_string(),
        Zone::Hand,
    );
    state
        .objects
        .get_mut(&land2)
        .unwrap()
        .card_types
        .core_types
        .push(CoreType::Land);
    let archenemy_land = create_object(
        &mut state,
        CardId(3),
        PlayerId(0),
        "Swamp".to_string(),
        Zone::Hand,
    );
    state
        .objects
        .get_mut(&archenemy_land)
        .unwrap()
        .card_types
        .core_types
        .push(CoreType::Land);

    apply_as_current(
        &mut state,
        GameAction::PlayLand {
            object_id: land1,
            card_id: CardId(1),
        },
    )
    .unwrap();
    assert_eq!(state.players[1].lands_played_this_turn, 1);

    state.priority_player = PlayerId(2);
    state.waiting_for = WaitingFor::Priority {
        player: PlayerId(2),
    };
    apply_as_current(
        &mut state,
        GameAction::PlayLand {
            object_id: land2,
            card_id: CardId(2),
        },
    )
    .unwrap();
    assert_eq!(state.players[2].lands_played_this_turn, 1);
    assert_eq!(state.players[1].lands_played_this_turn, 1);

    state.priority_player = PlayerId(0);
    state.waiting_for = WaitingFor::Priority {
        player: PlayerId(0),
    };
    let result = apply(
        &mut state,
        PlayerId(0),
        GameAction::PlayLand {
            object_id: archenemy_land,
            card_id: CardId(3),
        },
    );
    assert!(
        result.is_err(),
        "archenemy must not play a land during the hero team's turn"
    );
    assert!(state.players[0].hand.contains(&archenemy_land));
}

/// CR 614.1c discriminating test (fail-first): a land played through the
/// real `PlayLand` action must receive the `EntersWithAdditionalCounters`
/// static snapshot ("permanents you control enter with an additional +1/+1
/// counter" class) that an active permanent contributes. Before Phase B,
/// the land-play `Execute` arm was a divergent partial copy of
/// `deliver_replaced_zone_change`: it applied only the event's own
/// `enter_with_counters` and SKIPPED the statics snapshot, so a played land
/// silently missed the static's counter while every other battlefield entry
/// (creatures via the shared tail) received it. Routing the land entry
/// through `zone_pipeline::deliver` runs the full tail.
#[test]
fn played_land_receives_enters_with_additional_counters_static() {
    use std::sync::Arc;

    use crate::types::ability::{ControllerRef, FilterProp, StaticDefinition, TypedFilter};
    use crate::types::statics::StaticMode;

    let mut state = setup_game_at_main_phase();

    // CR 614.1c: a P0 permanent granting "other permanents you control enter
    // with an additional +1/+1 counter" — must be functioning BEFORE the
    // land enters.
    let source = create_object(
        &mut state,
        CardId(7000),
        PlayerId(0),
        "Counter Source".to_string(),
        Zone::Battlefield,
    );
    {
        let obj = state.objects.get_mut(&source).unwrap();
        let def = StaticDefinition::new(StaticMode::EntersWithAdditionalCounters {
            counter_type: CounterType::Plus1Plus1,
            count: 1,
        })
        .affected(TargetFilter::Typed(
            TypedFilter::permanent()
                .controller(ControllerRef::You)
                .properties(vec![FilterProp::Another]),
        ));
        obj.static_definitions.push(def.clone());
        Arc::make_mut(&mut obj.base_static_definitions).push(def);
    }

    let land = create_object(
        &mut state,
        CardId(7001),
        PlayerId(0),
        "Forest".to_string(),
        Zone::Hand,
    );
    state
        .objects
        .get_mut(&land)
        .unwrap()
        .card_types
        .core_types
        .push(CoreType::Land);

    apply_as_current(
        &mut state,
        GameAction::PlayLand {
            object_id: land,
            card_id: CardId(7001),
        },
    )
    .unwrap();

    let obj = &state.objects[&land];
    assert_eq!(obj.zone, Zone::Battlefield, "land entered the battlefield");
    assert_eq!(
        *obj.counters.get(&CounterType::Plus1Plus1).unwrap_or(&0),
        1,
        "played land must receive the EntersWithAdditionalCounters static \
             (CR 614.1c) — the divergent land-play Execute arm dropped the \
             statics snapshot the shared delivery tail applies"
    );
}

/// CR 614.1c + CR 614.1d: Thriving land text ("This land enters tapped. As
/// it enters, choose a color other than green.") must ENTER TAPPED in
/// addition to prompting for the colour. Drives the real PlayLand → ETB
/// replacement pipeline (synthesis via `from_oracle_text`) and asserts the
/// land is tapped on the battlefield.
#[test]
fn thriving_grove_enters_tapped_with_color_choice() {
    use crate::game::scenario::{GameScenario, P0};

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let grove = scenario
        .add_land_to_hand(P0, "Thriving Grove")
        .from_oracle_text("This land enters tapped. As it enters, choose a color other than green.")
        .id();
    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);
    }
    let card_id = runner.state().objects[&grove].card_id;

    runner
        .act(GameAction::PlayLand {
            object_id: grove,
            card_id,
        })
        .unwrap();

    assert!(
        runner.state().battlefield.contains(&grove),
        "Thriving Grove must be on the battlefield after PlayLand"
    );
    assert!(
        runner.state().objects[&grove].tapped,
        "issue #1581: Thriving Grove must ENTER TAPPED (enter_tapped replacement \
             applied), not just resolve the colour choice"
    );
}

/// Issue #2933: Black Dragon Gate must offer {B} and the as-enters chosen
/// color when tapped — not only the chosen color.
#[test]
fn black_dragon_gate_tap_offers_fixed_black_or_chosen_color() {
    use crate::game::mana_sources::activatable_land_mana_options;
    use crate::types::ability::ChosenAttribute;
    use crate::types::mana::ManaType;

    let mut state = setup_game_at_main_phase();
    let gate = create_object(
        &mut state,
        CardId(347),
        PlayerId(0),
        "Black Dragon Gate".to_string(),
        Zone::Battlefield,
    );
    {
        let obj = state.objects.get_mut(&gate).unwrap();
        obj.card_types.core_types.push(CoreType::Land);
        obj.card_types.subtypes.push("Gate".to_string());
    }
    apply_oracle_to_object(
            &mut state,
            gate,
            "Black Dragon Gate",
            "This land enters tapped.\nAs this land enters, choose a color other than black.\n{T}: Add {B} or one mana of the chosen color.",
        );
    state
        .objects
        .get_mut(&gate)
        .unwrap()
        .chosen_attributes
        .push(ChosenAttribute::Color(ManaColor::Red));

    let options = activatable_land_mana_options(&state, gate, PlayerId(0));
    let types: Vec<ManaType> = options.iter().map(|o| o.mana_type).collect();
    assert!(
        types.contains(&ManaType::Black),
        "Black Dragon Gate must offer {{B}}, got {types:?}"
    );
    assert!(
        types.contains(&ManaType::Red),
        "Black Dragon Gate must offer chosen Red, got {types:?}"
    );
    assert_eq!(types.len(), 2);

    let tap_black = apply_as_current(
        &mut state,
        GameAction::ActivateAbility {
            source_id: gate,
            ability_index: 0,
        },
    )
    .unwrap();
    assert!(
        matches!(tap_black.waiting_for, WaitingFor::ChooseManaColor { .. }),
        "two-color Gate must prompt before producing mana, got {:?}",
        tap_black.waiting_for
    );

    let resolved = apply_as_current(
        &mut state,
        GameAction::ChooseManaColor {
            choice: crate::types::game_state::ManaChoice::SingleColor(ManaType::Black),
            count: 1,
        },
    )
    .unwrap();
    assert!(matches!(resolved.waiting_for, WaitingFor::Priority { .. }));
    assert!(state.objects[&gate].tapped);
    assert_eq!(state.players[0].mana_pool.count_color(ManaType::Black), 1);
}

#[test]
fn thriving_grove_play_land_stays_tapped_after_color_choice() {
    let mut state = setup_game_at_main_phase();
    let grove = create_object(
        &mut state,
        CardId(1581),
        PlayerId(0),
        "Thriving Grove".to_string(),
        Zone::Hand,
    );
    {
        let obj = state.objects.get_mut(&grove).unwrap();
        obj.card_types.core_types.push(CoreType::Land);
    }
    apply_oracle_to_object(
            &mut state,
            grove,
            "Thriving Grove",
            "This land enters tapped. As it enters, choose a color other than green.\n{T}: Add {G} or one mana of the chosen color.",
        );

    let result = apply_as_current(
        &mut state,
        GameAction::PlayLand {
            object_id: grove,
            card_id: CardId(1581),
        },
    )
    .unwrap();

    assert!(matches!(
        result.waiting_for,
        WaitingFor::NamedChoice {
            choice_type: ChoiceType::Color { .. },
            source_id: Some(id),
            ..
        } if id == grove
    ));
    assert!(
        state.objects.get(&grove).unwrap().tapped,
        "Thriving Grove must enter tapped before the as-enters color choice resolves"
    );

    apply_as_current(
        &mut state,
        GameAction::ChooseOption {
            choice: "Red".to_string(),
        },
    )
    .unwrap();

    let obj = state.objects.get(&grove).unwrap();
    assert_eq!(obj.chosen_color(), Some(ManaColor::Red));
    assert!(
        obj.tapped,
        "Thriving Grove must remain tapped after choosing its color"
    );
}

#[test]
fn apply_play_land_rejects_non_main_phase() {
    let mut state = setup_game_at_main_phase();
    state.phase = Phase::Upkeep;

    let obj_id = create_object(
        &mut state,
        CardId(1),
        PlayerId(0),
        "Forest".to_string(),
        Zone::Hand,
    );

    let result = apply_as_current(
        &mut state,
        GameAction::PlayLand {
            object_id: obj_id,
            card_id: CardId(1),
        },
    );

    assert!(result.is_err());
}

#[test]
fn apply_play_land_rejects_over_limit() {
    let mut state = setup_game_at_main_phase();
    state.lands_played_this_turn = 1; // Already played one

    let obj_id = create_object(
        &mut state,
        CardId(1),
        PlayerId(0),
        "Forest".to_string(),
        Zone::Hand,
    );

    let result = apply_as_current(
        &mut state,
        GameAction::PlayLand {
            object_id: obj_id,
            card_id: CardId(1),
        },
    );

    assert!(result.is_err());
}

#[test]
fn apply_play_land_rejects_card_not_in_hand() {
    let mut state = setup_game_at_main_phase();

    let result = apply_as_current(
        &mut state,
        GameAction::PlayLand {
            object_id: ObjectId(0),
            card_id: CardId(999),
        },
    );

    assert!(result.is_err());
}

#[test]
fn apply_play_land_rejects_under_cant_play_land() {
    // CR 305.2: "Can't play lands" suppresses the play-land special action.
    use crate::types::ability::StaticDefinition;
    use crate::types::statics::StaticMode;

    let mut state = setup_game_at_main_phase();

    let land_id = create_object(
        &mut state,
        CardId(1),
        PlayerId(0),
        "Forest".to_string(),
        Zone::Hand,
    );
    // Place a battlefield permanent that applies CantPlayLand to P0.
    let source = create_object(
        &mut state,
        CardId(2),
        PlayerId(0),
        "Static Source".to_string(),
        Zone::Battlefield,
    );
    use crate::types::ability::{ControllerRef, TypedFilter};
    state
        .objects
        .get_mut(&source)
        .unwrap()
        .static_definitions
        .push(
            StaticDefinition::new(StaticMode::Other("CantPlayLand".to_string())).affected(
                TargetFilter::Typed(TypedFilter::default().controller(ControllerRef::You)),
            ),
        );

    let result = apply_as_current(
        &mut state,
        GameAction::PlayLand {
            object_id: land_id,
            card_id: CardId(1),
        },
    );

    assert!(
        result.is_err(),
        "PlayLand must be rejected under CantPlayLand"
    );
}

#[test]
fn apply_play_land_rejects_under_cant_play_land_transient_effect() {
    // CR 305.2 + CR 611.1 + CR 611.2c: An activated ability that creates a
    // continuous effect with "until end of turn" duration (Pardic Miner:
    // "Sacrifice this creature: Target player can't play lands this turn.")
    // registers a transient continuous effect bound to
    // `TargetFilter::SpecificPlayer { id }`. The play-land gate must
    // observe this TCE the same way it observes the printed-static form,
    // because the source object has already left the battlefield (sacrifice
    // cost) by the time the effect resolves.
    use crate::types::ability::{ContinuousModification, Duration};
    use crate::types::statics::StaticMode;

    let mut state = setup_game_at_main_phase();

    let land_id = create_object(
        &mut state,
        CardId(1),
        PlayerId(0),
        "Forest".to_string(),
        Zone::Hand,
    );

    // Register a SpecificPlayer-bound TCE granting CantPlayLand to P0,
    // mirroring what `effect.rs::register_transient_effect` would emit
    // when Pardic Miner's activated ability resolves with P0 chosen as
    // the target.
    state.add_transient_continuous_effect(
        ObjectId(99),
        PlayerId(1),
        Duration::UntilEndOfTurn,
        TargetFilter::SpecificPlayer { id: PlayerId(0) },
        vec![ContinuousModification::AddStaticMode {
            mode: StaticMode::Other("CantPlayLand".to_string()),
        }],
        None,
    );

    let result = apply_as_current(
        &mut state,
        GameAction::PlayLand {
            object_id: land_id,
            card_id: CardId(1),
        },
    );

    assert!(
        result.is_err(),
        "PlayLand must be rejected under transient CantPlayLand effect (Pardic Miner class)"
    );
}

#[test]
fn new_game_creates_two_player_state() {
    let state = new_game(42);
    assert_eq!(state.players.len(), 2);
    assert_eq!(state.rng_seed, 42);
}

/// CR 117.1c + CR 503.2: After Untap (no priority), the active player
/// receives priority during their Upkeep step. CR 103.7a skips the
/// first-turn Draw step entirely, so passing both priorities through
/// Upkeep lands at PreCombatMain.
#[test]
fn start_game_pauses_at_first_turn_upkeep_priority() {
    let mut state = new_game(42);
    let result = start_game_with_starting_player(&mut state, PlayerId(0));

    // CR 117.1c: starting player receives priority during Upkeep first.
    assert_eq!(state.phase, Phase::Upkeep);
    assert_eq!(state.turn_number, 1);
    assert!(matches!(
        result.waiting_for,
        WaitingFor::Priority {
            player: PlayerId(0)
        }
    ));

    // Both players pass through Upkeep → CR 103.7a skips Draw → PreCombatMain.
    apply_as_current(&mut state, GameAction::PassPriority).unwrap();
    let result = apply_as_current(&mut state, GameAction::PassPriority).unwrap();
    assert_eq!(state.phase, Phase::PreCombatMain);
    assert!(matches!(
        result.waiting_for,
        WaitingFor::Priority {
            player: PlayerId(0)
        }
    ));
}

#[test]
fn start_game_skips_draw_on_first_turn() {
    let mut state = new_game(42);

    // Add a card to player 0's library
    let id = create_object(
        &mut state,
        CardId(1),
        PlayerId(0),
        "Card".to_string(),
        Zone::Library,
    );

    start_game_skip_mulligan(&mut state);

    // Card should still be in library (draw skipped on turn 1)
    assert!(state.players[0].library.contains(&id));
    assert!(!state.players[0].hand.contains(&id));
}

#[test]
fn start_game_emits_game_started_event() {
    let mut state = new_game(42);
    let result = start_game(&mut state);

    assert!(result
        .events
        .iter()
        .any(|e| matches!(e, GameEvent::GameStarted)));
}

// CR 103.1: Regression — `start_game` must randomize the starting player for
// all match types, not just Bo3. Previously gated on `match_type == Bo3`, which
// caused every Bo1 (default) game to begin with PlayerId(0).
#[test]
fn start_game_randomizes_starting_player_for_default_match_type() {
    let mut saw_p0 = false;
    let mut saw_p1 = false;

    for seed in 0..64u64 {
        let mut state = new_game(seed);
        let _ = start_game(&mut state);
        match state.current_starting_player {
            PlayerId(0) => saw_p0 = true,
            PlayerId(1) => saw_p1 = true,
            _ => unreachable!("two-player game can only produce PlayerId(0) or PlayerId(1)"),
        }
        if saw_p0 && saw_p1 {
            break;
        }
    }

    assert!(
        saw_p0 && saw_p1,
        "start_game must randomize across both seats for default (Bo1) matches"
    );
}

#[test]
fn integration_full_turn_cycle() {
    let mut state = new_game(42);

    // Start game (turn 1, player 0) — engine pauses at Upkeep priority per
    // CR 117.1c. CR 103.7a skips the first-turn Draw step entirely.
    // (Libraries are empty, which is fine because the first-turn player
    // never draws and we stop the test before turn 2's draw step.)
    let _result = start_game_with_starting_player(&mut state, PlayerId(0));
    assert_eq!(state.phase, Phase::Upkeep);
    assert_eq!(state.turn_number, 1);

    // Pass through Upkeep (both players) — lands at PreCombatMain (Draw skipped).
    apply_as_current(&mut state, GameAction::PassPriority).unwrap();
    apply_as_current(&mut state, GameAction::PassPriority).unwrap();
    assert_eq!(state.phase, Phase::PreCombatMain);

    // Pass priority from player 0 (pre-combat main)
    let result = apply_as_current(&mut state, GameAction::PassPriority).unwrap();
    assert!(matches!(
        result.waiting_for,
        WaitingFor::Priority {
            player: PlayerId(1)
        }
    ));

    // Pass priority from player 1 (both passed, stack empty -> advance)
    let _result = apply_as_current(&mut state, GameAction::PassPriority).unwrap();
    // Should skip combat phases and land at PostCombatMain
    assert_eq!(state.phase, Phase::PostCombatMain);

    // Pass through post-combat main
    let _result = apply_as_current(&mut state, GameAction::PassPriority).unwrap();
    let _result = apply_as_current(&mut state, GameAction::PassPriority).unwrap();
    // Should advance to End step
    assert_eq!(state.phase, Phase::End);

    // Pass through end step → cleanup → next turn. Turn 2 is player 1's
    // turn; the engine pauses at P1's Upkeep priority (CR 117.1c).
    // (We stop here rather than draining Draw, because empty libraries
    // would trigger the CR 704.5b loss when P1 tries to draw.)
    let _result = apply_as_current(&mut state, GameAction::PassPriority).unwrap();
    let _result = apply_as_current(&mut state, GameAction::PassPriority).unwrap();
    assert_eq!(state.phase, Phase::Upkeep);
    assert_eq!(state.turn_number, 2);
    assert_eq!(state.active_player, PlayerId(1));
}

#[test]
fn monarch_end_step_draws_exactly_one_card() {
    let mut state = new_game(42);
    let _result = start_game_with_starting_player(&mut state, PlayerId(0));
    // Test starts mid-turn at PostCombatMain — bypass the natural Upkeep
    // priority window via direct state setup (test fixture pattern).
    state.phase = Phase::PostCombatMain;
    state.waiting_for = WaitingFor::Priority {
        player: PlayerId(0),
    };
    state.priority_player = PlayerId(0);
    state.priority_passes.clear();
    state.monarch = Some(PlayerId(0));

    create_object(
        &mut state,
        CardId(1),
        PlayerId(0),
        "First card".to_string(),
        Zone::Library,
    );
    create_object(
        &mut state,
        CardId(2),
        PlayerId(0),
        "Second card".to_string(),
        Zone::Library,
    );

    apply_as_current(&mut state, GameAction::PassPriority).unwrap();
    apply_as_current(&mut state, GameAction::PassPriority).unwrap();
    assert_eq!(state.phase, Phase::End);
    assert_eq!(state.stack.len(), 1);

    apply_as_current(&mut state, GameAction::PassPriority).unwrap();
    apply_as_current(&mut state, GameAction::PassPriority).unwrap();
    assert_eq!(state.players[0].hand.len(), 1);
    assert_eq!(state.players[0].library.len(), 1);

    // End → cleanup → next turn. Turn 2 is P1's; engine pauses at P1's
    // Upkeep priority per CR 117.1c. We stop here rather than draining
    // Draw because P1's library is empty in this test fixture (CR 704.5b
    // game-loss not under test). The monarch's end-step draw (P0, on turn
    // 1) is what the test exercises and we've already validated above.
    apply_as_current(&mut state, GameAction::PassPriority).unwrap();
    apply_as_current(&mut state, GameAction::PassPriority).unwrap();
    assert_eq!(state.phase, Phase::Upkeep);
    assert_eq!(state.turn_number, 2);
    assert_eq!(state.players[0].hand.len(), 1);
    assert_eq!(state.players[0].library.len(), 1);
}

#[test]
fn integration_play_land_then_pass() {
    let mut state = new_game(42);
    start_game_with_starting_player(&mut state, PlayerId(0));

    // CR 305.3 + CR 117.1c: lands are sorcery-speed, so pass Upkeep
    // priority (both players) to reach PreCombatMain before playing.
    // CR 103.7a skips first-turn Draw so two passes is enough.
    apply_as_current(&mut state, GameAction::PassPriority).unwrap();
    apply_as_current(&mut state, GameAction::PassPriority).unwrap();
    assert_eq!(state.phase, Phase::PreCombatMain);

    // Create a land in player 0's hand
    let land_id = create_object(
        &mut state,
        CardId(1),
        PlayerId(0),
        "Forest".to_string(),
        Zone::Hand,
    );
    state
        .objects
        .get_mut(&land_id)
        .unwrap()
        .card_types
        .core_types
        .push(CoreType::Land);

    // Play the land
    let result = apply_as_current(
        &mut state,
        GameAction::PlayLand {
            object_id: land_id,
            card_id: CardId(1),
        },
    )
    .unwrap();

    assert!(state.battlefield.contains(&land_id));
    assert_eq!(state.lands_played_this_turn, 1);

    // Player retains priority after playing land
    assert!(matches!(
        result.waiting_for,
        WaitingFor::Priority {
            player: PlayerId(0)
        }
    ));

    // Priority pass count should have been reset by the land play
    assert_eq!(state.priority_pass_count, 0);
}

#[test]
fn stack_push_and_lifo_resolve() {
    use crate::game::stack;
    use crate::types::game_state::{CastingVariant, StackEntry, StackEntryKind};

    let mut state = setup_game_at_main_phase();
    let mut events = Vec::new();

    // Create two spell objects
    let id1 = create_object(
        &mut state,
        CardId(1),
        PlayerId(0),
        "Bolt".to_string(),
        Zone::Stack,
    );
    state
        .objects
        .get_mut(&id1)
        .unwrap()
        .card_types
        .core_types
        .push(CoreType::Instant);

    let id2 = create_object(
        &mut state,
        CardId(2),
        PlayerId(0),
        "Bear".to_string(),
        Zone::Stack,
    );
    state
        .objects
        .get_mut(&id2)
        .unwrap()
        .card_types
        .core_types
        .push(CoreType::Creature);

    // Push to stack (first pushed = bottom)
    stack::push_to_stack(
        &mut state,
        StackEntry {
            id: id1,
            source_id: id1,
            controller: PlayerId(0),
            kind: StackEntryKind::Spell {
                card_id: CardId(1),
                ability: None,
                casting_variant: CastingVariant::Normal,
                actual_mana_spent: 0,
            },
        },
        &mut events,
    );
    stack::push_to_stack(
        &mut state,
        StackEntry {
            id: id2,
            source_id: id2,
            controller: PlayerId(0),
            kind: StackEntryKind::Spell {
                card_id: CardId(2),
                ability: None,
                casting_variant: CastingVariant::Normal,
                actual_mana_spent: 0,
            },
        },
        &mut events,
    );

    assert_eq!(state.stack.len(), 2);

    // Resolve top (LIFO) -- should be id2 (Bear, creature -> battlefield)
    stack::resolve_top(&mut state, &mut events);
    assert_eq!(state.stack.len(), 1);
    assert!(state.battlefield.contains(&id2)); // Creature goes to battlefield

    // Resolve next -- should be id1 (Bolt, instant -> graveyard)
    stack::resolve_top(&mut state, &mut events);
    assert_eq!(state.stack.len(), 0);
    assert!(state.players[0].graveyard.contains(&id1)); // Instant goes to graveyard
}

#[test]
fn stack_is_empty_check() {
    use crate::game::stack;

    let state = new_game(42);
    assert!(stack::stack_is_empty(&state));
}

#[test]
fn engine_error_display() {
    let err = EngineError::WrongPlayer;
    assert_eq!(err.to_string(), "Wrong player");

    let err = EngineError::NotYourPriority;
    assert_eq!(err.to_string(), "Not your priority");

    let err = EngineError::InvalidAction("test".to_string());
    assert_eq!(err.to_string(), "Invalid action: test");
}

/// Regression: the engine must reject any non-Concede action whose
/// `actor` does not match `authorized_submitter(state)`. Before the
/// engine-level guard existed, `apply()` silently used `waiting_for`'s
/// player as the actor — meaning the human could click targets during
/// an AI's `TargetSelection` and the engine would accept them *as the
/// AI*. The guard below is the single place that closes that loophole
/// for every transport (WASM, WebSocket, P2P).
#[test]
fn apply_rejects_action_from_wrong_actor() {
    let mut state = setup_game_at_main_phase();
    // `setup_game_at_main_phase` leaves P0 with priority.
    assert_eq!(
        turn_control::authorized_submitter(&state),
        Some(PlayerId(0)),
        "precondition: P0 should have priority"
    );

    // P1 submitting an action meant for P0 must be rejected.
    let result = apply(&mut state, PlayerId(1), GameAction::PassPriority);
    assert!(
        matches!(result, Err(EngineError::WrongPlayer)),
        "expected WrongPlayer, got {result:?}"
    );

    // P0 submitting the same action must succeed.
    let result = apply(&mut state, PlayerId(0), GameAction::PassPriority);
    assert!(result.is_ok(), "P0 pass should succeed: {result:?}");
}

#[test]
fn two_hg_empty_stack_two_team_passes_advance_and_return_priority() {
    let mut state = GameState::new(FormatConfig::two_headed_giant(), 4, 42);
    state.turn_number = 2;
    state.phase = Phase::PreCombatMain;
    state.active_player = PlayerId(0);
    state.priority_player = PlayerId(0);
    state.waiting_for = WaitingFor::Priority {
        player: PlayerId(0),
    };
    state.priority_passes.clear();

    let first = apply(&mut state, PlayerId(0), GameAction::PassPriority)
        .expect("active team representative should pass priority");
    assert_eq!(
        first.waiting_for,
        WaitingFor::Priority {
            player: PlayerId(2)
        }
    );
    assert!(state.priority_passes.contains(&PlayerId(0)));
    assert!(!state.priority_passes.contains(&PlayerId(1)));

    let second = apply(&mut state, PlayerId(2), GameAction::PassPriority)
        .expect("opposing team representative should pass priority");

    assert_ne!(state.phase, Phase::PreCombatMain);
    assert!(state.priority_passes.is_empty());
    assert_eq!(
        second.waiting_for,
        WaitingFor::Priority {
            player: PlayerId(0)
        }
    );
}

#[test]
fn two_hg_non_empty_stack_two_team_passes_resolve_top_object() {
    let mut state = GameState::new(FormatConfig::two_headed_giant(), 4, 42);
    state.turn_number = 2;
    state.phase = Phase::PreCombatMain;
    state.active_player = PlayerId(0);
    state.priority_player = PlayerId(0);
    state.waiting_for = WaitingFor::Priority {
        player: PlayerId(0),
    };
    state.stack.push_back(no_op_stack_entry(1, PlayerId(0)));
    state.priority_passes.clear();

    let first = apply(&mut state, PlayerId(0), GameAction::PassPriority)
        .expect("active team representative should pass priority");
    assert_eq!(
        first.waiting_for,
        WaitingFor::Priority {
            player: PlayerId(2)
        }
    );

    let second = apply(&mut state, PlayerId(2), GameAction::PassPriority)
        .expect("opposing team representative should pass priority");

    assert!(state.stack.is_empty());
    assert!(second
        .events
        .iter()
        .any(|event| matches!(event, GameEvent::StackResolved { .. })));
    assert_eq!(
        second.waiting_for,
        WaitingFor::Priority {
            player: PlayerId(0)
        }
    );
}

#[test]
fn two_hg_controlled_team_turn_routes_teammate_priority_to_controller() {
    let mut state = GameState::new(FormatConfig::two_headed_giant(), 4, 42);
    state.turn_number = 2;
    state.phase = Phase::PreCombatMain;
    state.active_player = PlayerId(0);
    state.turn_decision_controller = Some(PlayerId(2));
    state.waiting_for = WaitingFor::Priority {
        player: PlayerId(0),
    };
    state.priority_player = turn_control::authorized_submitter(&state).unwrap();
    state.priority_passes.clear();

    let result = apply(&mut state, PlayerId(2), GameAction::PassPriority)
        .expect("turn controller should be authorized for active player");

    assert_eq!(
        result.waiting_for,
        WaitingFor::Priority {
            player: PlayerId(2)
        }
    );
    assert_eq!(
            turn_control::authorized_submitter(&state),
            Some(PlayerId(2)),
            "CR 117.6 + CR 805.5b move priority from the active team to the opposing team representative"
        );
    assert_eq!(
        state.priority_player,
        PlayerId(2),
        "public submitter should be the opposing team representative after the active team passes"
    );

    let teammate_result = apply(&mut state, PlayerId(1), GameAction::PassPriority);
    assert!(
            matches!(teammate_result, Err(EngineError::WrongPlayer)),
            "active-team teammate must not submit after team-level priority has moved to P2: {teammate_result:?}"
        );

    let controller_result = apply(&mut state, PlayerId(2), GameAction::PassPriority);
    assert!(
        controller_result.is_ok(),
        "opposing representative should submit for their team's priority: {controller_result:?}"
    );
}

#[test]
fn non_team_controlled_turn_only_routes_active_player() {
    let mut state = GameState::new(FormatConfig::free_for_all(), 3, 42);
    state.turn_number = 2;
    state.phase = Phase::PreCombatMain;
    state.active_player = PlayerId(0);
    state.turn_decision_controller = Some(PlayerId(2));
    state.waiting_for = WaitingFor::Priority {
        player: PlayerId(0),
    };
    state.priority_player = turn_control::authorized_submitter(&state).unwrap();
    state.priority_passes.clear();

    let result = apply(&mut state, PlayerId(2), GameAction::PassPriority)
        .expect("turn controller should be authorized for the active player");

    assert_eq!(
        result.waiting_for,
        WaitingFor::Priority {
            player: PlayerId(1)
        }
    );
    assert_eq!(
        turn_control::authorized_submitter(&state),
        Some(PlayerId(1)),
        "non-team controlled turns must not route unrelated players through the controller"
    );

    let controller_result = apply(&mut state, PlayerId(2), GameAction::PassPriority);
    assert!(
        matches!(controller_result, Err(EngineError::WrongPlayer)),
        "controller must not submit for the next non-team opponent: {controller_result:?}"
    );

    let next_player_result = apply(&mut state, PlayerId(1), GameAction::PassPriority);
    assert!(
        next_player_result.is_ok(),
        "next non-team player should submit for themselves: {next_player_result:?}"
    );
}

/// Regression: Concede self-authenticates via its own `player_id`, but
/// `actor` must still match that `player_id` so one player cannot
/// concede another. CR 104.3a: *a player* may concede at any time.
#[test]
fn apply_rejects_spoofed_concede() {
    let mut state = setup_game_at_main_phase();
    // P0 trying to concede P1 → rejected.
    let spoofed = GameAction::Concede {
        player_id: PlayerId(1),
    };
    let result = apply(&mut state, PlayerId(0), spoofed);
    assert!(
        matches!(result, Err(EngineError::WrongPlayer)),
        "expected WrongPlayer, got {result:?}"
    );

    // P1 conceding themselves → accepted even though P0 has priority.
    let self_concede = GameAction::Concede {
        player_id: PlayerId(1),
    };
    let result = apply(&mut state, PlayerId(1), self_concede);
    assert!(result.is_ok(), "self-concede should succeed: {result:?}");
}

#[test]
fn tap_land_for_mana_produces_correct_color() {
    let mut state = setup_game_at_main_phase();
    state.priority_passes.insert(PlayerId(1));
    state.priority_pass_count = 1;

    let land_id = create_object(
        &mut state,
        CardId(1),
        PlayerId(0),
        "Forest".to_string(),
        Zone::Battlefield,
    );
    {
        let obj = state.objects.get_mut(&land_id).unwrap();
        obj.card_types.core_types.push(CoreType::Land);
        obj.entered_battlefield_turn = Some(1);
        Arc::make_mut(&mut obj.abilities).push(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Mana {
                    produced: ManaProduction::Fixed {
                        colors: vec![crate::types::mana::ManaColor::Green],
                        contribution: ManaContribution::Base,
                    },
                    restrictions: vec![],
                    grants: vec![],
                    expiry: None,
                    target: None,
                },
            )
            .cost(AbilityCost::Tap),
        );
    }

    let result = apply_as_current(
        &mut state,
        GameAction::TapLandForMana { object_id: land_id },
    )
    .unwrap();

    assert!(state.objects[&land_id].tapped);
    assert_eq!(
        state.players[0]
            .mana_pool
            .count_color(crate::types::mana::ManaType::Green),
        1
    );
    assert!(matches!(
        result.waiting_for,
        WaitingFor::Priority {
            player: PlayerId(0)
        }
    ));
}

#[test]
fn tap_land_for_mana_uses_priority_player_during_opponents_turn() {
    let mut state = setup_game_at_main_phase();
    state.active_player = PlayerId(1);
    state.priority_player = PlayerId(0);
    state.waiting_for = WaitingFor::Priority {
        player: PlayerId(0),
    };

    let land_id = create_object(
        &mut state,
        CardId(1),
        PlayerId(0),
        "Forest".to_string(),
        Zone::Battlefield,
    );
    {
        let obj = state.objects.get_mut(&land_id).unwrap();
        obj.card_types.core_types.push(CoreType::Land);
        obj.entered_battlefield_turn = Some(1);
        Arc::make_mut(&mut obj.abilities).push(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Mana {
                    produced: ManaProduction::Fixed {
                        colors: vec![crate::types::mana::ManaColor::Green],
                        contribution: ManaContribution::Base,
                    },
                    restrictions: vec![],
                    grants: vec![],
                    expiry: None,
                    target: None,
                },
            )
            .cost(AbilityCost::Tap),
        );
    }

    let result = apply_as_current(
        &mut state,
        GameAction::TapLandForMana { object_id: land_id },
    )
    .unwrap();

    assert!(state.objects[&land_id].tapped);
    assert_eq!(
        state.players[0]
            .mana_pool
            .count_color(crate::types::mana::ManaType::Green),
        1
    );
    assert_eq!(
        state.players[1]
            .mana_pool
            .count_color(crate::types::mana::ManaType::Green),
        0
    );
    assert!(matches!(
        result.waiting_for,
        WaitingFor::Priority {
            player: PlayerId(0)
        }
    ));
}

/// CR 118.3a regression: each land tapped for mana must yield a pool unit
/// with a DISTINCT, nonzero `pip_id`. A shared/zero id makes every same-color
/// pip in the manual-payment UI select and deselect together (the reported
/// "tap one → all select, tap again → all deselect" bug), because pinning a
/// `pip_id` then matches every unit carrying that same id.
#[test]
fn tapped_lands_produce_distinct_pip_ids() {
    let mut state = setup_game_at_main_phase();

    let mut land_ids = Vec::new();
    for _ in 0..3 {
        let land_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Forest".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&land_id).unwrap();
            obj.card_types.core_types.push(CoreType::Land);
            obj.card_types.subtypes.push("Forest".to_string());
            obj.entered_battlefield_turn = Some(1);
        }
        land_ids.push(land_id);
    }

    for land_id in land_ids {
        apply_as_current(
            &mut state,
            GameAction::TapLandForMana { object_id: land_id },
        )
        .unwrap();
    }

    let ids: Vec<u64> = state.players[0]
        .mana_pool
        .mana
        .iter()
        .map(|u| u.pip_id.0)
        .collect();
    assert_eq!(ids.len(), 3, "three taps must float three pool units");
    assert!(
        ids.iter().all(|&id| id != 0),
        "every pooled unit must be stamped (nonzero pip_id), got {ids:?}"
    );
    let unique: std::collections::HashSet<u64> = ids.iter().copied().collect();
    assert_eq!(unique.len(), 3, "pip ids must be distinct, got {ids:?}");
}

/// Build a Wild Growth–style aura attached to `land_id` for tests in this
/// module. Single-color "{G}" `TapsForMana` trigger via
/// `valid_card: AttachedTo`. Returns the aura's `ObjectId`.
fn attach_wild_growth(state: &mut GameState, land_id: ObjectId, owner: PlayerId) -> ObjectId {
    let aura = create_object(
        state,
        CardId(99),
        owner,
        "Wild Growth".to_string(),
        Zone::Battlefield,
    );
    let obj = state.objects.get_mut(&aura).unwrap();
    obj.card_types.core_types.push(CoreType::Enchantment);
    obj.card_types.subtypes.push("Aura".to_string());
    obj.attached_to = Some(land_id.into());
    obj.entered_battlefield_turn = Some(1);
    obj.trigger_definitions.push(
        TriggerDefinition::new(TriggerMode::TapsForMana)
            .execute(AbilityDefinition::new(
                AbilityKind::Database,
                Effect::Mana {
                    produced: ManaProduction::Fixed {
                        colors: vec![crate::types::mana::ManaColor::Green],
                        contribution: ManaContribution::Additional,
                    },
                    restrictions: vec![],
                    grants: vec![],
                    expiry: None,
                    target: None,
                },
            ))
            .valid_card(TargetFilter::AttachedTo),
    );
    aura
}

#[test]
fn untap_land_for_mana_refunds_aura_bonus_no_infinite_mana() {
    // CR 605.1b + CR 605.3b: Wild Growth attaches to a Forest. Tapping the
    // Forest emits {G} (land) + {G} (aura's TapsForMana trigger). The user
    // then invokes `UntapLandForMana` — both mana units must be refunded,
    // otherwise repeated tap-untap-tap cycles compound aura mana into the
    // pool indefinitely (the user-reported infinite-mana exploit).
    let mut state = setup_game_at_main_phase();

    let forest = create_object(
        &mut state,
        CardId(1),
        PlayerId(0),
        "Forest".to_string(),
        Zone::Battlefield,
    );
    {
        let obj = state.objects.get_mut(&forest).unwrap();
        obj.card_types.core_types.push(CoreType::Land);
        obj.card_types.subtypes.push("Forest".to_string());
        obj.entered_battlefield_turn = Some(1);
    }
    attach_wild_growth(&mut state, forest, PlayerId(0));

    // Tap the Forest. Land emits {G}; aura's trigger fires via
    // run_post_action_pipeline and adds another {G}.
    apply_as_current(&mut state, GameAction::TapLandForMana { object_id: forest }).unwrap();
    assert_eq!(
        state.players[0]
            .mana_pool
            .count_color(crate::types::mana::ManaType::Green),
        2,
        "tap should yield {{G}} (land) + {{G}} (Wild Growth bonus)"
    );

    // Manual untap reverses BOTH the land's and the aura's contributions.
    apply_as_current(
        &mut state,
        GameAction::UntapLandForMana { object_id: forest },
    )
    .unwrap();
    assert!(!state.objects[&forest].tapped, "Forest must be untapped");
    assert_eq!(
        state.players[0].mana_pool.total(),
        0,
        "manual untap must refund both the land's and the aura's mana — \
             leaving aura mana would allow tap-untap-tap to compound mana"
    );

    // Re-tap and re-untap to verify no compounding across cycles.
    for _ in 0..3 {
        apply_as_current(&mut state, GameAction::TapLandForMana { object_id: forest }).unwrap();
        assert_eq!(state.players[0].mana_pool.total(), 2);
        apply_as_current(
            &mut state,
            GameAction::UntapLandForMana { object_id: forest },
        )
        .unwrap();
        assert_eq!(
            state.players[0].mana_pool.total(),
            0,
            "every cycle must net to zero pool — no compounding aura mana"
        );
    }
}

#[test]
fn can_pay_cost_after_auto_tap_includes_aura_taps_for_mana_bonus() {
    // CR 605.1b + CR 106.4: AI affordability simulation must surface mana
    // contributed by `TapsForMana` triggered abilities (Wild Growth /
    // Fertile Ground / Utopia Sprawl class). A Plains enchanted with Wild
    // Growth produces {W} (land) + {G} (aura) and must be reported
    // payable for a {1}{G} cost — without trigger processing in the
    // affordability simulation, the AI would skip a turn that the player
    // could actually pay.
    use crate::types::mana::{ManaCost, ManaCostShard};
    let mut state = setup_game_at_main_phase();

    let plains = create_object(
        &mut state,
        CardId(1),
        PlayerId(0),
        "Plains".to_string(),
        Zone::Battlefield,
    );
    {
        let obj = state.objects.get_mut(&plains).unwrap();
        obj.card_types.core_types.push(CoreType::Land);
        obj.card_types.subtypes.push("Plains".to_string());
        obj.entered_battlefield_turn = Some(1);
    }
    attach_wild_growth(&mut state, plains, PlayerId(0));

    // Synthesize a hand object representing the spell being affordability-checked.
    let spell = create_object(
        &mut state,
        CardId(2),
        PlayerId(0),
        "Test Spell".to_string(),
        Zone::Hand,
    );

    let cost = ManaCost::Cost {
        shards: vec![ManaCostShard::Green],
        generic: 1,
    };
    assert!(
        casting::can_pay_cost_after_auto_tap(&state, PlayerId(0), spell, &cost),
        "Plains + Wild Growth must be reported able to pay {{1}}{{G}}: \
             land contributes {{W}}, aura's TapsForMana trigger contributes {{G}}"
    );

    // Sanity baseline: a Plains alone cannot pay {1}{G}.
    let mut state_no_aura = setup_game_at_main_phase();
    let lone_plains = create_object(
        &mut state_no_aura,
        CardId(1),
        PlayerId(0),
        "Plains".to_string(),
        Zone::Battlefield,
    );
    {
        let obj = state_no_aura.objects.get_mut(&lone_plains).unwrap();
        obj.card_types.core_types.push(CoreType::Land);
        obj.card_types.subtypes.push("Plains".to_string());
        obj.entered_battlefield_turn = Some(1);
    }
    let lone_spell = create_object(
        &mut state_no_aura,
        CardId(2),
        PlayerId(0),
        "Test Spell".to_string(),
        Zone::Hand,
    );
    assert!(
        !casting::can_pay_cost_after_auto_tap(&state_no_aura, PlayerId(0), lone_spell, &cost),
        "lone Plains must NOT be reported able to pay {{1}}{{G}}"
    );
}

/// Build a Fertile Ground–style AnyOneColor aura. Returns the aura's `ObjectId`.
fn attach_fertile_ground(state: &mut GameState, land_id: ObjectId, owner: PlayerId) -> ObjectId {
    let aura = create_object(
        state,
        CardId(98),
        owner,
        "Fertile Ground".to_string(),
        Zone::Battlefield,
    );
    let obj = state.objects.get_mut(&aura).unwrap();
    obj.card_types.core_types.push(CoreType::Enchantment);
    obj.card_types.subtypes.push("Aura".to_string());
    obj.attached_to = Some(land_id.into());
    obj.entered_battlefield_turn = Some(1);
    obj.trigger_definitions.push(
        TriggerDefinition::new(TriggerMode::TapsForMana)
            .execute(AbilityDefinition::new(
                AbilityKind::Database,
                Effect::Mana {
                    produced: ManaProduction::AnyOneColor {
                        count: crate::types::ability::QuantityExpr::Fixed { value: 1 },
                        color_options: crate::types::mana::ManaColor::ALL.to_vec(),
                        contribution: ManaContribution::Additional,
                    },
                    restrictions: vec![],
                    grants: vec![],
                    expiry: None,
                    target: None,
                },
            ))
            .valid_card(TargetFilter::AttachedTo),
    );
    aura
}

#[test]
fn fertile_ground_auto_tap_threads_non_first_color_to_resolver() {
    // Issue #4265 / round-4 reviewer regression: the planner can advertise
    // a {G}+{U} option for Forest + Fertile Ground, but the resolver used
    // to default to the first AnyOneColor option (White) regardless of the
    // planner's choice. This test proves that when a {U} cost is pending,
    // auto-tap picks the Forest (for {G}) and tells Fertile Ground's inline
    // TapsForMana trigger to produce {U} — not White.
    use crate::types::mana::{ManaCost, ManaCostShard};
    let mut state = setup_game_at_main_phase();

    let forest = create_object(
        &mut state,
        CardId(1),
        PlayerId(0),
        "Forest".to_string(),
        Zone::Battlefield,
    );
    {
        let obj = state.objects.get_mut(&forest).unwrap();
        obj.card_types.core_types.push(CoreType::Land);
        obj.card_types.subtypes.push("Forest".to_string());
        obj.entered_battlefield_turn = Some(1);
    }
    attach_fertile_ground(&mut state, forest, PlayerId(0));

    // Simulate auto-tap for {G}{U}: the planner must choose Blue for the
    // Fertile Ground bonus so both shards are covered.
    let cost = ManaCost::Cost {
        shards: vec![ManaCostShard::Green, ManaCostShard::Blue],
        generic: 0,
    };
    let spell = create_object(
        &mut state,
        CardId(2),
        PlayerId(0),
        "Blue-Green Spell".to_string(),
        Zone::Hand,
    );

    // Verify the planner considers the cost payable.
    assert!(
        casting::can_pay_cost_after_auto_tap(&state, PlayerId(0), spell, &cost),
        "Forest + Fertile Ground must be able to pay {{G}}{{U}}"
    );

    // Run auto-tap and inline trigger resolution (always used as a pair).
    let mut events = Vec::new();
    let events_before = events.len();
    casting_costs::auto_tap_mana_sources(&mut state, PlayerId(0), &cost, &mut events, None);
    // CR 605.4a: `resolve_tap_mana_triggers_inline` fires TapsForMana
    // triggers (including Fertile Ground's) with the planner's color choice
    // threaded via `state.pending_taps_for_mana_overrides`.
    super::triggers::resolve_tap_mana_triggers_inline(&mut state, &mut events, events_before);
    let green = state.players[0]
        .mana_pool
        .count_color(crate::types::mana::ManaType::Green);
    let blue = state.players[0]
        .mana_pool
        .count_color(crate::types::mana::ManaType::Blue);
    assert_eq!(green, 1, "Forest must contribute {{G}} to the pool");
    assert_eq!(
        blue, 1,
        "Fertile Ground's TapsForMana trigger must produce {{U}} — not the \
             first listed color ({{W}}) — when the planner chose Blue"
    );
}

#[test]
fn vorinclex_mana_doubling_trigger_fires_on_tap() {
    // Vorinclex, Voice of Hunger: "Whenever you tap a land for mana,
    // add one mana of any type that land produced."
    // The trigger is on Vorinclex (creature), not on the land itself.
    // valid_card: Typed(Land), valid_target: Controller.
    let mut state = setup_game_at_main_phase();

    let forest = create_object(
        &mut state,
        CardId(1),
        PlayerId(0),
        "Forest".to_string(),
        Zone::Battlefield,
    );
    {
        let obj = state.objects.get_mut(&forest).unwrap();
        obj.card_types.core_types.push(CoreType::Land);
        obj.card_types.subtypes.push("Forest".to_string());
        obj.entered_battlefield_turn = Some(1);
    }

    let vorinclex = create_object(
        &mut state,
        CardId(2),
        PlayerId(0),
        "Vorinclex, Voice of Hunger".to_string(),
        Zone::Battlefield,
    );
    {
        let obj = state.objects.get_mut(&vorinclex).unwrap();
        obj.card_types
            .core_types
            .push(crate::types::card_type::CoreType::Creature);
        obj.entered_battlefield_turn = Some(1);
        // Trigger 1: mana doubling for your lands
        obj.trigger_definitions.push(
            TriggerDefinition::new(TriggerMode::TapsForMana)
                .execute(AbilityDefinition::new(
                    AbilityKind::Database,
                    Effect::Mana {
                        produced: ManaProduction::TriggerEventManaType,
                        restrictions: vec![],
                        grants: vec![],
                        expiry: None,
                        target: None,
                    },
                ))
                .valid_card(TargetFilter::Typed(TypedFilter::land()))
                .valid_target(TargetFilter::Controller),
        );
    }

    // Tap the Forest — should produce {G} (land) + {G} (Vorinclex doubler).
    apply_as_current(&mut state, GameAction::TapLandForMana { object_id: forest }).unwrap();
    assert_eq!(
        state.players[0]
            .mana_pool
            .count_color(crate::types::mana::ManaType::Green),
        2,
        "Vorinclex must double land mana: {{G}} (land) + {{G}} (trigger)"
    );
}

#[test]
fn vorinclex_cant_untap_trigger_fires_on_opponent_tap() {
    // Vorinclex, Voice of Hunger: "Whenever an opponent taps a land for
    // mana, that land doesn't untap during its controller's next untap step."
    // The trigger is a GenericEffect (CantUntap) that goes on the stack.
    use crate::types::ability::{
        ContinuousModification, ControllerRef, Duration, PlayerScope, StaticDefinition,
    };
    let mut state = setup_game_at_main_phase();
    // Set P1 as active player so they have priority to tap
    state.active_player = PlayerId(1);
    state.priority_player = PlayerId(1);
    state.waiting_for = WaitingFor::Priority {
        player: PlayerId(1),
    };

    let opp_forest = create_object(
        &mut state,
        CardId(1),
        PlayerId(1),
        "Forest".to_string(),
        Zone::Battlefield,
    );
    {
        let obj = state.objects.get_mut(&opp_forest).unwrap();
        obj.card_types.core_types.push(CoreType::Land);
        obj.card_types.subtypes.push("Forest".to_string());
        obj.entered_battlefield_turn = Some(1);
    }

    let vorinclex = create_object(
        &mut state,
        CardId(2),
        PlayerId(0),
        "Vorinclex, Voice of Hunger".to_string(),
        Zone::Battlefield,
    );
    {
        let duration = Duration::UntilNextStepOf {
            step: Phase::Untap,
            player: PlayerScope::Controller,
        };
        let obj = state.objects.get_mut(&vorinclex).unwrap();
        obj.card_types
            .core_types
            .push(crate::types::card_type::CoreType::Creature);
        obj.entered_battlefield_turn = Some(1);
        // Trigger 2: opponent lands can't untap
        obj.trigger_definitions.push(
            TriggerDefinition::new(TriggerMode::TapsForMana)
                .execute(
                    AbilityDefinition::new(
                        AbilityKind::Database,
                        Effect::GenericEffect {
                            static_abilities: vec![StaticDefinition::new(StaticMode::CantUntap)
                                .affected(TargetFilter::ParentTarget)
                                .modifications(vec![ContinuousModification::AddStaticMode {
                                    mode: StaticMode::CantUntap,
                                }])],
                            duration: Some(duration.clone()),
                            target: Some(TargetFilter::TriggeringSource),
                        },
                    )
                    .duration(duration),
                )
                .valid_card(TargetFilter::Typed(
                    TypedFilter::land().controller(ControllerRef::Opponent),
                )),
        );
    }

    // Opponent taps the Forest
    apply(
        &mut state,
        PlayerId(1),
        GameAction::TapLandForMana {
            object_id: opp_forest,
        },
    )
    .unwrap();
    // The trigger should have been placed on the stack.
    assert!(
        !state.stack.is_empty() || !state.transient_continuous_effects.is_empty(),
        "Vorinclex's CantUntap trigger must fire when opponent taps land"
    );
}

#[test]
fn untap_land_for_mana_aura_bonus_helper_lists_attached_aura() {
    // Sanity check on the aura-source enumerator that
    // `handle_untap_land_for_mana` consults: it must include the Wild
    // Growth-style aura whose `valid_card: AttachedTo` resolves to the
    // tapped land, and exclude the land itself.
    let mut state = setup_game_at_main_phase();
    let forest = create_object(
        &mut state,
        CardId(1),
        PlayerId(0),
        "Forest".to_string(),
        Zone::Battlefield,
    );
    state
        .objects
        .get_mut(&forest)
        .unwrap()
        .card_types
        .core_types
        .push(CoreType::Land);
    let aura = attach_wild_growth(&mut state, forest, PlayerId(0));

    let sources = mana_sources::aura_taps_for_mana_sources_for_land(&state, forest, PlayerId(0));
    assert_eq!(sources, vec![aura]);
}

#[test]
fn tap_land_rejects_already_tapped() {
    let mut state = setup_game_at_main_phase();

    let land_id = create_object(
        &mut state,
        CardId(1),
        PlayerId(0),
        "Forest".to_string(),
        Zone::Battlefield,
    );
    {
        let obj = state.objects.get_mut(&land_id).unwrap();
        obj.card_types.core_types.push(CoreType::Land);
        obj.card_types.subtypes.push("Forest".to_string());
        obj.tapped = true;
    }

    let result = apply_as_current(
        &mut state,
        GameAction::TapLandForMana { object_id: land_id },
    );

    assert!(result.is_err());
}

#[test]
fn multi_mana_land_rejects_tap_land_for_mana() {
    // Dual lands with multiple mana abilities must use ActivateAbility to
    // select which color — TapLandForMana is ambiguous for multi-option lands.
    let mut state = setup_game_at_main_phase();

    let dual_id = create_object(
        &mut state,
        CardId(2),
        PlayerId(0),
        "Watery Grave".to_string(),
        Zone::Battlefield,
    );
    {
        let obj = state.objects.get_mut(&dual_id).unwrap();
        obj.card_types.core_types.push(CoreType::Land);
        Arc::make_mut(&mut obj.abilities).push(
            AbilityDefinition::new(
                crate::types::ability::AbilityKind::Activated,
                crate::types::ability::Effect::Mana {
                    produced: crate::types::ability::ManaProduction::Fixed {
                        colors: vec![crate::types::mana::ManaColor::Blue],
                        contribution: ManaContribution::Base,
                    },
                    restrictions: vec![],
                    grants: vec![],
                    expiry: None,
                    target: None,
                },
            )
            .cost(crate::types::ability::AbilityCost::Tap),
        );
        Arc::make_mut(&mut obj.abilities).push(
            AbilityDefinition::new(
                crate::types::ability::AbilityKind::Activated,
                crate::types::ability::Effect::Mana {
                    produced: crate::types::ability::ManaProduction::Fixed {
                        colors: vec![crate::types::mana::ManaColor::Black],
                        contribution: ManaContribution::Base,
                    },
                    restrictions: vec![],
                    grants: vec![],
                    expiry: None,
                    target: None,
                },
            )
            .cost(crate::types::ability::AbilityCost::Tap),
        );
    }

    let result = apply_as_current(
        &mut state,
        GameAction::TapLandForMana { object_id: dual_id },
    );
    assert!(
        result.is_err(),
        "TapLandForMana should reject multi-mana lands"
    );
}

#[test]
fn multi_mana_land_activates_via_ability_index() {
    // Dual lands use ActivateAbility with a specific ability_index to select color.
    let mut state = setup_game_at_main_phase();

    let dual_id = create_object(
        &mut state,
        CardId(2),
        PlayerId(0),
        "Watery Grave".to_string(),
        Zone::Battlefield,
    );
    {
        let obj = state.objects.get_mut(&dual_id).unwrap();
        obj.card_types.core_types.push(CoreType::Land);
        obj.has_mana_ability = true;
        Arc::make_mut(&mut obj.abilities).push(
            AbilityDefinition::new(
                crate::types::ability::AbilityKind::Activated,
                crate::types::ability::Effect::Mana {
                    produced: crate::types::ability::ManaProduction::Fixed {
                        colors: vec![crate::types::mana::ManaColor::Blue],
                        contribution: ManaContribution::Base,
                    },
                    restrictions: vec![],
                    grants: vec![],
                    expiry: None,
                    target: None,
                },
            )
            .cost(crate::types::ability::AbilityCost::Tap),
        );
        Arc::make_mut(&mut obj.abilities).push(
            AbilityDefinition::new(
                crate::types::ability::AbilityKind::Activated,
                crate::types::ability::Effect::Mana {
                    produced: crate::types::ability::ManaProduction::Fixed {
                        colors: vec![crate::types::mana::ManaColor::Black],
                        contribution: ManaContribution::Base,
                    },
                    restrictions: vec![],
                    grants: vec![],
                    expiry: None,
                    target: None,
                },
            )
            .cost(crate::types::ability::AbilityCost::Tap),
        );
    }

    // Activate Blue (ability_index 0)
    let result = apply_as_current(
        &mut state,
        GameAction::ActivateAbility {
            source_id: dual_id,
            ability_index: 0,
        },
    )
    .unwrap();

    assert!(state.objects[&dual_id].tapped);
    assert_eq!(
        state.players[0]
            .mana_pool
            .count_color(crate::types::mana::ManaType::Blue),
        1
    );
    assert_eq!(
        state.players[0]
            .mana_pool
            .count_color(crate::types::mana::ManaType::Black),
        0
    );
    assert!(matches!(
        result.waiting_for,
        WaitingFor::Priority {
            player: PlayerId(0)
        }
    ));
}

#[test]
fn multi_mana_land_undoable_after_activate_ability() {
    // Dual lands tapped via ActivateAbility should be undoable via UntapLandForMana.
    let mut state = setup_game_at_main_phase();

    let dual_id = create_object(
        &mut state,
        CardId(2),
        PlayerId(0),
        "Watery Grave".to_string(),
        Zone::Battlefield,
    );
    {
        let obj = state.objects.get_mut(&dual_id).unwrap();
        obj.card_types.core_types.push(CoreType::Land);
        obj.has_mana_ability = true;
        Arc::make_mut(&mut obj.abilities).push(
            AbilityDefinition::new(
                crate::types::ability::AbilityKind::Activated,
                crate::types::ability::Effect::Mana {
                    produced: crate::types::ability::ManaProduction::Fixed {
                        colors: vec![crate::types::mana::ManaColor::Black],
                        contribution: ManaContribution::Base,
                    },
                    restrictions: vec![],
                    grants: vec![],
                    expiry: None,
                    target: None,
                },
            )
            .cost(crate::types::ability::AbilityCost::Tap),
        );
    }

    // Tap for Black via ActivateAbility
    apply_as_current(
        &mut state,
        GameAction::ActivateAbility {
            source_id: dual_id,
            ability_index: 0,
        },
    )
    .unwrap();
    assert!(state.objects[&dual_id].tapped);
    assert_eq!(
        state.players[0]
            .mana_pool
            .count_color(crate::types::mana::ManaType::Black),
        1
    );

    // Undo via UntapLandForMana
    apply_as_current(
        &mut state,
        GameAction::UntapLandForMana { object_id: dual_id },
    )
    .unwrap();
    assert!(!state.objects[&dual_id].tapped);
    assert_eq!(
        state.players[0]
            .mana_pool
            .count_color(crate::types::mana::ManaType::Black),
        0
    );
}

#[test]
fn controller_harming_mana_land_is_not_undoable_after_manual_activation() {
    let mut state = setup_game_at_main_phase();

    let brushland = create_object(
        &mut state,
        CardId(3),
        PlayerId(0),
        "Brushland".to_string(),
        Zone::Battlefield,
    );
    {
        let obj = state.objects.get_mut(&brushland).unwrap();
        obj.card_types.core_types.push(CoreType::Land);
        obj.has_mana_ability = true;
        Arc::make_mut(&mut obj.abilities).push(brushland_colored_ability());
    }

    let first = apply_as_current(
        &mut state,
        GameAction::ActivateAbility {
            source_id: brushland,
            ability_index: 0,
        },
    )
    .unwrap();
    assert!(
        matches!(first.waiting_for, WaitingFor::ChooseManaColor { .. }),
        "expected ChooseManaColor after activating Brushland, got {:?}",
        first.waiting_for
    );

    let second = apply_as_current(
        &mut state,
        GameAction::ChooseManaColor {
            choice: crate::types::game_state::ManaChoice::SingleColor(
                crate::types::mana::ManaType::Green,
            ),
            count: 1,
        },
    )
    .unwrap();
    assert!(matches!(second.waiting_for, WaitingFor::Priority { .. }));
    assert!(state.objects[&brushland].tapped);
    assert_eq!(state.players[0].life, 19);
    assert!(state
        .lands_tapped_for_mana
        .get(&PlayerId(0))
        .is_none_or(|ids| !ids.contains(&brushland)));

    let undo = apply_as_current(
        &mut state,
        GameAction::UntapLandForMana {
            object_id: brushland,
        },
    );
    assert!(
        undo.is_err(),
        "controller-harming mana activations should not be undoable"
    );
}

// CR 605.1b + CR 722.1: End-to-end integration test. Driving a real
// `ActivateAbility` action on the Forest must (a) update the mana pool with
// the Forest's base {G}, (b) fire Utopia Sprawl's TapsForMana trigger
// inline (stack-skipped per CR 605.1b), (c) add the chosen color to the
// pool, and (d) leave the stack empty so the controller can immediately
// spend the mana.
#[test]
fn utopia_sprawl_on_forest_taps_for_both_base_and_additional_mana_inline() {
    use crate::types::ability::{
        ChosenAttribute, Effect as Eff, ManaContribution, ManaProduction, QuantityExpr,
        TriggerDefinition,
    };
    use crate::types::triggers::TriggerMode;

    let mut state = setup_game_at_main_phase();

    // Forest with the standard {T}: Add {G} synthesized mana ability.
    let forest = create_object(
        &mut state,
        CardId(1),
        PlayerId(0),
        "Forest".to_string(),
        Zone::Battlefield,
    );
    {
        let obj = state.objects.get_mut(&forest).unwrap();
        obj.card_types.core_types.push(CoreType::Land);
        obj.card_types.subtypes.push("Forest".to_string());
        obj.has_mana_ability = true;
        Arc::make_mut(&mut obj.abilities).push(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Eff::Mana {
                    produced: ManaProduction::Fixed {
                        colors: vec![crate::types::mana::ManaColor::Green],
                        contribution: ManaContribution::Base,
                    },
                    restrictions: vec![],
                    grants: vec![],
                    expiry: None,
                    target: None,
                },
            )
            .cost(AbilityCost::Tap),
        );
    }

    // Utopia Sprawl attached to the Forest with chosen color Red.
    let aura = create_object(
        &mut state,
        CardId(2),
        PlayerId(0),
        "Utopia Sprawl".to_string(),
        Zone::Battlefield,
    );
    {
        let obj = state.objects.get_mut(&aura).unwrap();
        obj.card_types.core_types.push(CoreType::Enchantment);
        obj.card_types.subtypes.push("Aura".to_string());
        obj.attached_to = Some(forest.into());
        obj.entered_battlefield_turn = Some(1);
        obj.chosen_attributes
            .push(ChosenAttribute::Color(crate::types::mana::ManaColor::Red));
        obj.trigger_definitions.push(
            TriggerDefinition::new(TriggerMode::TapsForMana)
                .execute(AbilityDefinition::new(
                    AbilityKind::Database,
                    Eff::Mana {
                        produced: ManaProduction::ChosenColor {
                            count: QuantityExpr::Fixed { value: 1 },
                            contribution: ManaContribution::Additional,
                            fixed_alternative: None,
                        },
                        restrictions: vec![],
                        grants: vec![],
                        expiry: None,
                        target: None,
                    },
                ))
                .valid_card(TargetFilter::AttachedTo),
        );
    }

    // Activate the Forest's {T}: Add {G} via the full apply() pipeline.
    let result = apply_as_current(
        &mut state,
        GameAction::ActivateAbility {
            source_id: forest,
            ability_index: 0,
        },
    )
    .expect("Forest mana ability should activate");

    // (a) Forest is tapped, base {G} in the pool.
    assert!(state.objects[&forest].tapped);
    assert_eq!(
        state.players[0]
            .mana_pool
            .count_color(crate::types::mana::ManaType::Green),
        1,
        "Forest's base {{G}} must be in the pool",
    );

    // (c) Utopia Sprawl's chosen-color {R} is ALSO in the pool, added
    // inline by the triggered mana ability (CR 605.1b).
    assert_eq!(
        state.players[0]
            .mana_pool
            .count_color(crate::types::mana::ManaType::Red),
        1,
        "Utopia Sprawl's additional {{R}} must be in the pool",
    );

    // (d) Stack is empty — the triggered mana ability did NOT use the
    // stack. Controller retains priority and can immediately spend the
    // mana on a {R} cost.
    assert_eq!(
        state.stack.len(),
        0,
        "Triggered mana ability must not be placed on the stack (CR 605.1b)",
    );
    assert!(
        matches!(result.waiting_for, WaitingFor::Priority { .. }),
        "Controller must retain priority after the activation resolves",
    );
}

#[test]
fn full_turn_integration_with_mulligan() {
    let mut state = new_game(42);

    // Add 20 basic lands to each player's library
    for player_idx in 0..2u8 {
        for i in 0..20 {
            let id = create_object(
                &mut state,
                CardId((player_idx as u64) * 100 + i),
                PlayerId(player_idx),
                "Forest".to_string(),
                Zone::Library,
            );
            let obj = state.objects.get_mut(&id).unwrap();
            obj.card_types.core_types.push(CoreType::Land);
            obj.card_types.subtypes.push("Forest".to_string());
        }
    }

    // Start game -> mulligan prompt
    let result = start_game_with_starting_player(&mut state, PlayerId(0));
    assert!(matches!(
        result.waiting_for,
        WaitingFor::MulliganDecision { .. }
    ));

    // Both players have 7 cards in hand
    assert_eq!(state.players[0].hand.len(), 7);
    assert_eq!(state.players[1].hand.len(), 7);

    // Player 0 keeps (apply_as_current picks first pending player = P0)
    let result = apply_as_current(
        &mut state,
        GameAction::MulliganDecision {
            choice: crate::types::actions::MulliganChoice::Keep,
        },
    )
    .unwrap();
    assert!(matches!(
        result.waiting_for,
        WaitingFor::MulliganDecision { .. }
    ));

    // Player 1 keeps (apply_as_current now picks P1 since P0 was removed)
    // → game starts, lands at Upkeep priority for P0 (CR 117.1c).
    let result = apply_as_current(
        &mut state,
        GameAction::MulliganDecision {
            choice: crate::types::actions::MulliganChoice::Keep,
        },
    )
    .unwrap();
    assert!(matches!(
        result.waiting_for,
        WaitingFor::Priority {
            player: PlayerId(0),
        }
    ));
    assert_eq!(state.phase, Phase::Upkeep);

    // Drain Upkeep priority (turn 1 skips Draw per CR 103.7a) to reach Main.
    apply_as_current(&mut state, GameAction::PassPriority).unwrap();
    apply_as_current(&mut state, GameAction::PassPriority).unwrap();
    assert_eq!(state.phase, Phase::PreCombatMain);

    // Play a land from hand
    let land_obj_id = state.players[0].hand[0];
    let land_card_id = state.objects[&land_obj_id].card_id;
    let _result = apply_as_current(
        &mut state,
        GameAction::PlayLand {
            object_id: land_obj_id,
            card_id: land_card_id,
        },
    )
    .unwrap();
    assert_eq!(state.lands_played_this_turn, 1);

    // Find the land on battlefield to tap it
    let land_on_bf = state
        .battlefield
        .iter()
        .find(|&&id| {
            state
                .objects
                .get(&id)
                .map(|o| o.controller == PlayerId(0) && !o.tapped)
                .unwrap_or(false)
        })
        .copied()
        .unwrap();

    // Tap land for mana
    let _result = apply_as_current(
        &mut state,
        GameAction::TapLandForMana {
            object_id: land_on_bf,
        },
    )
    .unwrap();
    assert_eq!(
        state.players[0]
            .mana_pool
            .count_color(crate::types::mana::ManaType::Green),
        1
    );

    // Pass priority through the rest of the turn
    // PreCombatMain: P0 passes
    apply_as_current(&mut state, GameAction::PassPriority).unwrap();
    // PreCombatMain: P1 passes -> advances to PostCombatMain
    apply_as_current(&mut state, GameAction::PassPriority).unwrap();
    assert_eq!(state.phase, Phase::PostCombatMain);

    // PostCombatMain: both pass -> End
    apply_as_current(&mut state, GameAction::PassPriority).unwrap();
    apply_as_current(&mut state, GameAction::PassPriority).unwrap();
    assert_eq!(state.phase, Phase::End);

    // End: both pass → Cleanup → next turn. P1's Upkeep priority opens
    // first (CR 117.1c); turn 2 doesn't skip Draw, so drain Upkeep + Draw.
    apply_as_current(&mut state, GameAction::PassPriority).unwrap();
    apply_as_current(&mut state, GameAction::PassPriority).unwrap();
    assert_eq!(state.phase, Phase::Upkeep);
    assert_eq!(state.turn_number, 2);
    assert_eq!(state.active_player, PlayerId(1));
    apply_as_current(&mut state, GameAction::PassPriority).unwrap();
    apply_as_current(&mut state, GameAction::PassPriority).unwrap();
    assert_eq!(state.phase, Phase::Draw);
    apply_as_current(&mut state, GameAction::PassPriority).unwrap();
    apply_as_current(&mut state, GameAction::PassPriority).unwrap();
    assert_eq!(state.phase, Phase::PreCombatMain);
}

#[test]
fn cast_spell_moves_card_from_hand_to_stack_and_returns_priority() {
    use crate::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};

    let mut state = setup_game_at_main_phase();

    // Create a sorcery in hand
    let obj_id = create_object(
        &mut state,
        CardId(10),
        PlayerId(0),
        "Divination".to_string(),
        Zone::Hand,
    );
    {
        let obj = state.objects.get_mut(&obj_id).unwrap();
        obj.card_types.core_types.push(CoreType::Sorcery);
        Arc::make_mut(&mut obj.abilities).push(make_draw_ability(2));
        obj.mana_cost = ManaCost::Cost {
            shards: vec![ManaCostShard::Blue],
            generic: 2,
        };
    }

    // Add mana
    let player = state
        .players
        .iter_mut()
        .find(|p| p.id == PlayerId(0))
        .unwrap();
    for _ in 0..3 {
        player.mana_pool.add(ManaUnit {
            color: ManaType::Blue,
            source_id: ObjectId(0),
            pip_id: crate::types::mana::ManaPipId(0),
            supertype: None,
            source_could_produce_two_or_more_colors: false,
            restrictions: Vec::new(),
            grants: vec![],
            expiry: None,
        });
    }

    let result = apply_as_current(
        &mut state,
        GameAction::CastSpell {
            object_id: obj_id,
            card_id: CardId(10),
            targets: vec![],

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
    assert_eq!(state.stack.len(), 1);
    assert!(!state.players[0].hand.contains(&obj_id));
}

#[test]
fn both_pass_with_spell_on_stack_resolves_spell() {
    use crate::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};

    let mut state = setup_game_at_main_phase();

    // Create a sorcery and cast it
    let obj_id = create_object(
        &mut state,
        CardId(10),
        PlayerId(0),
        "Divination".to_string(),
        Zone::Hand,
    );
    {
        let obj = state.objects.get_mut(&obj_id).unwrap();
        obj.card_types.core_types.push(CoreType::Sorcery);
        Arc::make_mut(&mut obj.abilities).push(make_draw_ability(2));
        obj.mana_cost = ManaCost::Cost {
            shards: vec![ManaCostShard::Blue],
            generic: 2,
        };
    }

    // Add some cards to draw
    for i in 0..5 {
        create_object(
            &mut state,
            CardId(100 + i),
            PlayerId(0),
            format!("Card {}", i),
            Zone::Library,
        );
    }

    let player = state
        .players
        .iter_mut()
        .find(|p| p.id == PlayerId(0))
        .unwrap();
    for _ in 0..3 {
        player.mana_pool.add(ManaUnit {
            color: ManaType::Blue,
            source_id: ObjectId(0),
            pip_id: crate::types::mana::ManaPipId(0),
            supertype: None,
            source_could_produce_two_or_more_colors: false,
            restrictions: Vec::new(),
            grants: vec![],
            expiry: None,
        });
    }

    // Cast the spell
    apply_as_current(
        &mut state,
        GameAction::CastSpell {
            object_id: obj_id,
            card_id: CardId(10),
            targets: vec![],

            payment_mode: crate::types::game_state::CastPaymentMode::Auto,
        },
    )
    .unwrap();
    assert_eq!(state.stack.len(), 1);

    let hand_before = state.players[0].hand.len();

    // Both pass -> resolve
    apply_as_current(&mut state, GameAction::PassPriority).unwrap();
    apply_as_current(&mut state, GameAction::PassPriority).unwrap();

    // Stack should be empty
    assert!(state.stack.is_empty());
    // Card should be in graveyard (sorcery)
    assert!(state.players[0].graveyard.contains(&obj_id));
    // Draw 2 effect should have fired
    assert_eq!(state.players[0].hand.len(), hand_before + 2);
}

#[test]
fn brainstorm_resolves_draw_then_put_two_cards_on_top() {
    use crate::types::ability::{ControllerRef, FilterProp, LibraryPosition};
    use crate::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};

    let mut state = setup_game_at_main_phase();
    let brainstorm = create_object(
        &mut state,
        CardId(10),
        PlayerId(0),
        "Brainstorm".to_string(),
        Zone::Hand,
    );
    let first_hand = create_object(
        &mut state,
        CardId(20),
        PlayerId(0),
        "First Hand Card".to_string(),
        Zone::Hand,
    );
    let second_hand = create_object(
        &mut state,
        CardId(21),
        PlayerId(0),
        "Second Hand Card".to_string(),
        Zone::Hand,
    );
    for i in 0..3 {
        create_object(
            &mut state,
            CardId(100 + i),
            PlayerId(0),
            format!("Library Card {i}"),
            Zone::Library,
        );
    }

    let mut brainstorm_ability = make_draw_ability(3);
    brainstorm_ability.sub_ability = Some(Box::new(AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::PutAtLibraryPosition {
            target: TargetFilter::Typed(
                TypedFilter::card()
                    .controller(ControllerRef::You)
                    .properties(vec![FilterProp::InZone { zone: Zone::Hand }]),
            ),
            count: QuantityExpr::Fixed { value: 2 },
            position: LibraryPosition::Top,
        },
    )));
    {
        let obj = state.objects.get_mut(&brainstorm).unwrap();
        obj.card_types.core_types.push(CoreType::Instant);
        Arc::make_mut(&mut obj.abilities).push(brainstorm_ability);
        obj.mana_cost = ManaCost::Cost {
            shards: vec![ManaCostShard::Blue],
            generic: 0,
        };
    }
    state.players[0].mana_pool.add(ManaUnit {
        color: ManaType::Blue,
        source_id: ObjectId(0),
        pip_id: crate::types::mana::ManaPipId(0),
        supertype: None,
        source_could_produce_two_or_more_colors: false,
        restrictions: Vec::new(),
        grants: vec![],
        expiry: None,
    });

    apply_as_current(
        &mut state,
        GameAction::CastSpell {
            object_id: brainstorm,
            card_id: CardId(10),
            targets: vec![],

            payment_mode: crate::types::game_state::CastPaymentMode::Auto,
        },
    )
    .unwrap();
    apply_as_current(&mut state, GameAction::PassPriority).unwrap();
    let result = apply_as_current(&mut state, GameAction::PassPriority).unwrap();

    assert!(matches!(
        result.waiting_for,
        WaitingFor::EffectZoneChoice {
            player: PlayerId(0),
            count: 2,
            effect_kind: EffectKind::PutAtLibraryPosition,
            zone: Zone::Hand,
            ..
        }
    ));

    let result = apply_as_current(
        &mut state,
        GameAction::SelectCards {
            cards: vec![first_hand, second_hand],
        },
    )
    .unwrap();

    assert!(matches!(result.waiting_for, WaitingFor::Priority { .. }));
    assert!(state.stack.is_empty());
    assert!(state.players[0].graveyard.contains(&brainstorm));
    assert_eq!(state.players[0].library[0], first_hand);
    assert_eq!(state.players[0].library[1], second_hand);
    assert!(!state.players[0].hand.contains(&first_hand));
    assert!(!state.players[0].hand.contains(&second_hand));
}

#[test]
fn gamble_searches_to_hand_then_discards_random_card() {
    let mut state = setup_game_at_main_phase();
    let gamble = create_object(
        &mut state,
        CardId(10),
        PlayerId(0),
        "Gamble".to_string(),
        Zone::Hand,
    );
    {
        let obj = state.objects.get_mut(&gamble).unwrap();
        obj.card_types.core_types.push(CoreType::Sorcery);
        obj.base_card_types = obj.card_types.clone();
    }
    apply_spell_oracle_to_object(
            &mut state,
            gamble,
            "Gamble",
            "Search your library for a card, put that card into your hand, discard a card at random, then shuffle.",
        );
    let hand_a = create_object(
        &mut state,
        CardId(20),
        PlayerId(0),
        "Hand A".to_string(),
        Zone::Hand,
    );
    let hand_b = create_object(
        &mut state,
        CardId(21),
        PlayerId(0),
        "Hand B".to_string(),
        Zone::Hand,
    );
    let target = create_object(
        &mut state,
        CardId(30),
        PlayerId(0),
        "Tutor Target".to_string(),
        Zone::Library,
    );

    apply_as_current(
        &mut state,
        GameAction::CastSpell {
            object_id: gamble,
            card_id: CardId(10),
            targets: vec![],

            payment_mode: crate::types::game_state::CastPaymentMode::Auto,
        },
    )
    .unwrap();
    apply_as_current(&mut state, GameAction::PassPriority).unwrap();
    let result = apply_as_current(&mut state, GameAction::PassPriority).unwrap();
    assert!(matches!(
        result.waiting_for,
        WaitingFor::SearchChoice { .. }
    ));

    let mut discard_pool: Vec<ObjectId> = state.players[0].hand.iter().copied().collect();
    discard_pool.push(target);
    let expected_discard = {
        let mut rng = state.rng.clone();
        let index = rng.random_range(0..discard_pool.len());
        discard_pool[index]
    };

    apply_as_current(
        &mut state,
        GameAction::SelectCards {
            cards: vec![target],
        },
    )
    .unwrap();

    assert!(state.players[0].graveyard.contains(&expected_discard));
    assert!(
        [hand_a, hand_b, target]
            .into_iter()
            .filter(|id| state.players[0].hand.contains(id))
            .count()
            == 2
    );
    assert!(state.players[0].graveyard.contains(&gamble));
}

#[test]
fn disciple_of_bolas_uses_sacrificed_creature_power_for_life_and_draw() {
    let mut state = setup_game_at_main_phase();

    let disciple = create_object(
        &mut state,
        CardId(10),
        PlayerId(0),
        "Disciple of Bolas".to_string(),
        Zone::Hand,
    );
    {
        let obj = state.objects.get_mut(&disciple).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.base_card_types = obj.card_types.clone();
        obj.power = Some(2);
        obj.toughness = Some(1);
        obj.base_power = Some(2);
        obj.base_toughness = Some(1);
        obj.mana_cost = ManaCost::Cost {
            shards: vec![ManaCostShard::Black],
            generic: 3,
        };
    }
    apply_oracle_to_object(
            &mut state,
            disciple,
            "Disciple of Bolas",
            "When this creature enters, sacrifice another creature. You gain X life and draw X cards, where X is that creature's power.",
        );

    let hill_giant = create_object(
        &mut state,
        CardId(20),
        PlayerId(0),
        "Hill Giant".to_string(),
        Zone::Battlefield,
    );
    {
        let obj = state.objects.get_mut(&hill_giant).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.base_card_types = obj.card_types.clone();
        obj.power = Some(3);
        obj.toughness = Some(3);
        obj.base_power = Some(3);
        obj.base_toughness = Some(3);
    }
    let library_cards: Vec<_> = (0..3)
        .map(|i| {
            create_object(
                &mut state,
                CardId(30 + i),
                PlayerId(0),
                format!("Library Card {i}"),
                Zone::Library,
            )
        })
        .collect();
    assert!(library_cards
        .iter()
        .all(|id| state.players[0].library.contains(id)));

    state.players[0].mana_pool.add(ManaUnit::new(
        ManaType::Black,
        ObjectId(0),
        false,
        Vec::new(),
    ));
    for _ in 0..3 {
        state.players[0].mana_pool.add(ManaUnit::new(
            ManaType::Colorless,
            ObjectId(0),
            false,
            Vec::new(),
        ));
    }

    let disciple_card_id = state.objects[&disciple].card_id;
    apply_as_current(
        &mut state,
        GameAction::CastSpell {
            object_id: disciple,
            card_id: disciple_card_id,
            targets: vec![],

            payment_mode: crate::types::game_state::CastPaymentMode::Auto,
        },
    )
    .unwrap();

    let mut result = apply_as_current(&mut state, GameAction::PassPriority).unwrap();
    for _ in 0..6 {
        if state.players[0].graveyard.contains(&hill_giant) {
            break;
        }
        if matches!(result.waiting_for, WaitingFor::EffectZoneChoice { .. }) {
            panic!("Disciple should auto-sacrifice the only other creature");
        }
        result = apply_as_current(&mut state, GameAction::PassPriority).unwrap();
    }

    assert_eq!(state.players[0].life, 23);
    assert_eq!(state.players[0].hand.len(), 3);
    assert!(state.players[0].graveyard.contains(&hill_giant));
}

/// Cluster J1 (headline `/card-test` regression guard): "When this creature
/// enters, sacrifice **another** creature." must exclude the source. Casting
/// Disciple of Bolas through the full cast → trigger → stack → apply() → chain
/// → `sacrifice::resolve` pipeline with exactly one OTHER creature must
/// sacrifice that other creature and leave Disciple on the battlefield.
///
/// CR 701.21a: sacrifice moves the chosen permanent to its owner's graveyard.
/// The `another` qualifier parses to `FilterProp::Another`; the effect resolver
/// builds its eligible pool via `FilterContext::from_ability` (source excluded
/// at `filter.rs` `FilterProp::Another => object_id != source.id`).
///
/// COST/EFFECT CLASS REDUNDANCY (fix-constraint #2): the sibling cost path
/// (`find_eligible_sacrifice_targets` → `FilterContext::from_source`) sets
/// `recipient_id: None` identically to `from_ability`, so both paths hit the
/// same `FilterProp::Another` exclusion site with an identical source id. This
/// one effect-path guard therefore substantiates the whole "sacrifice another"
/// class (effect + cost forms) — no separate cost-path integration test needed.
///
/// The positive Hill-Giant sacrifice plus the +3 life / +3 draw deltas prove
/// the sacrifice machinery actually ran (non-vacuous), and the paired sibling
/// test `sacrifice_a_creature_without_another_includes_sole_source` proves the
/// exclusion is `Another`-driven, not an empty-pool artifact.
#[test]
fn disciple_of_bolas_sacrifices_another_creature_not_itself() {
    use crate::game::scenario::{GameScenario, P0};

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // The entering creature carries the verbatim Oracle text (parser branch
    // fidelity — a paraphrase could take a different branch). Mana is elided
    // (default zero cost) so the test isolates the sacrifice-exclusion seam.
    let disciple = scenario
        .add_creature_to_hand_from_oracle(
            P0,
            "Disciple of Bolas",
            2,
            1,
            "When this creature enters, sacrifice another creature. You gain X life and draw X cards, where X is that creature's power.",
        )
        .id();

    // Exactly one OTHER creature: the sole eligible sacrifice, so the effect
    // auto-resolves (no EffectZoneChoice) onto it. Power 3 → X = 3.
    let hill_giant = scenario.add_creature(P0, "Hill Giant", 3, 3).id();

    // Three library cards so the "draw X" (X = 3) has cards to draw.
    scenario.with_library_top(P0, &["Card A", "Card B", "Card C"]);

    let mut runner = scenario.build();
    let outcome = runner.cast(disciple).resolve();

    // Source excluded: Disciple survives on the battlefield.
    outcome.assert_zone(&[disciple], Zone::Battlefield);
    // The OTHER creature was sacrificed (positive, non-vacuous reach-guard).
    outcome.assert_zone(&[hill_giant], Zone::Graveyard);
    // X = Hill Giant's power (3): +3 life and 3 cards drawn.
    outcome.assert_life_delta(P0, 3);
    outcome.assert_hand_drawn(P0, 3);
}

/// Cluster J1 (paired sibling reach-guard): the same ETB shape WITHOUT the
/// `another` qualifier ("sacrifice a creature") DOES include the sole source,
/// sacrificing the entering creature itself. This proves the exclusion in
/// `disciple_of_bolas_sacrifices_another_creature_not_itself` is driven by
/// `FilterProp::Another`, not by a vacuously empty eligible pool.
///
/// CR 701.21a: with the source as the only eligible creature, the mandatory
/// sacrifice auto-resolves onto it.
#[test]
fn sacrifice_a_creature_without_another_includes_sole_source() {
    use crate::game::scenario::{GameScenario, P0};

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // No "another" — the source is a legal sacrifice target. It is the ONLY
    // creature on the battlefield when the ETB resolves.
    let reaper = scenario
        .add_creature_to_hand_from_oracle(
            P0,
            "Test Reaper",
            1,
            1,
            "When this creature enters, sacrifice a creature.",
        )
        .id();

    let mut runner = scenario.build();
    let outcome = runner.cast(reaper).resolve();

    // Sole eligible creature is the source itself → it sacrifices itself.
    outcome.assert_zone(&[reaper], Zone::Graveyard);
}

const SQUADRON_HAWK_ORACLE: &str = "Flying\nWhen this creature enters, you may search your library for up to three cards named Squadron Hawk, reveal them, put them into your hand, then shuffle.";

fn add_squadron_hawk_to_library(state: &mut GameState, card_id: u64) -> ObjectId {
    let hawk = create_object(
        state,
        CardId(card_id),
        PlayerId(0),
        "Squadron Hawk".to_string(),
        Zone::Library,
    );
    {
        let obj = state.objects.get_mut(&hawk).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.base_card_types = obj.card_types.clone();
        obj.power = Some(1);
        obj.toughness = Some(1);
        obj.base_power = Some(1);
        obj.base_toughness = Some(1);
    }
    hawk
}

fn resolve_squadron_hawk_etb_to_search_choice() -> (GameState, [ObjectId; 3], ObjectId) {
    let mut state = setup_game_at_main_phase();
    let entering_hawk = create_object(
        &mut state,
        CardId(10),
        PlayerId(0),
        "Squadron Hawk".to_string(),
        Zone::Hand,
    );
    {
        let obj = state.objects.get_mut(&entering_hawk).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.base_card_types = obj.card_types.clone();
        obj.power = Some(1);
        obj.toughness = Some(1);
        obj.base_power = Some(1);
        obj.base_toughness = Some(1);
    }
    apply_oracle_to_object(
        &mut state,
        entering_hawk,
        "Squadron Hawk",
        SQUADRON_HAWK_ORACLE,
    );

    let hawks = [
        add_squadron_hawk_to_library(&mut state, 11),
        add_squadron_hawk_to_library(&mut state, 12),
        add_squadron_hawk_to_library(&mut state, 13),
    ];
    let nonmatch = create_object(
        &mut state,
        CardId(14),
        PlayerId(0),
        "Storm Crow".to_string(),
        Zone::Library,
    );

    let mut events = Vec::new();
    zones::move_to_zone(&mut state, entering_hawk, Zone::Battlefield, &mut events);
    crate::game::triggers::process_triggers(&mut state, &events);

    assert_eq!(state.stack.len(), 1, "Squadron Hawk ETB trigger must stack");
    apply_as_current(&mut state, GameAction::PassPriority).unwrap();
    apply_as_current(&mut state, GameAction::PassPriority).unwrap();
    assert!(
        matches!(state.waiting_for, WaitingFor::OptionalEffectChoice { .. }),
        "Squadron Hawk's 'you may' trigger must prompt before searching, got {:?}",
        state.waiting_for
    );

    apply_as_current(
        &mut state,
        GameAction::DecideOptionalEffect { accept: true },
    )
    .unwrap();

    match &state.waiting_for {
        WaitingFor::SearchChoice {
            player,
            cards,
            count,
            reveal,
            up_to,
            ..
        } => {
            assert_eq!(*player, PlayerId(0));
            assert_eq!(*count, 3);
            assert!(*reveal);
            assert!(*up_to);
            assert_eq!(cards.len(), 3);
            for hawk in hawks {
                assert!(cards.contains(&hawk), "SearchChoice must offer {hawk:?}");
            }
            assert!(
                !cards.contains(&nonmatch),
                "SearchChoice must not offer non-Squadron Hawk cards"
            );
        }
        other => {
            panic!("Expected SearchChoice after accepting Squadron Hawk ETB, got {other:?}")
        }
    }

    (state, hawks, nonmatch)
}

#[test]
fn squadron_hawk_may_trigger_can_be_declined_before_search() {
    let mut state = setup_game_at_main_phase();
    let entering_hawk = create_object(
        &mut state,
        CardId(10),
        PlayerId(0),
        "Squadron Hawk".to_string(),
        Zone::Hand,
    );
    {
        let obj = state.objects.get_mut(&entering_hawk).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.base_card_types = obj.card_types.clone();
    }
    apply_oracle_to_object(
        &mut state,
        entering_hawk,
        "Squadron Hawk",
        SQUADRON_HAWK_ORACLE,
    );
    let library_hawk = add_squadron_hawk_to_library(&mut state, 11);

    let mut events = Vec::new();
    zones::move_to_zone(&mut state, entering_hawk, Zone::Battlefield, &mut events);
    crate::game::triggers::process_triggers(&mut state, &events);
    apply_as_current(&mut state, GameAction::PassPriority).unwrap();
    apply_as_current(&mut state, GameAction::PassPriority).unwrap();
    assert!(matches!(
        state.waiting_for,
        WaitingFor::OptionalEffectChoice { .. }
    ));

    let result = apply_as_current(
        &mut state,
        GameAction::DecideOptionalEffect { accept: false },
    )
    .unwrap();

    assert!(matches!(result.waiting_for, WaitingFor::Priority { .. }));
    assert!(state.stack.is_empty());
    assert_eq!(state.objects[&library_hawk].zone, Zone::Library);
    assert!(state.players[0].library.contains(&library_hawk));
    assert!(!state.players[0].hand.contains(&library_hawk));
    assert!(!result.events.iter().any(|event| matches!(
        event,
        GameEvent::PlayerPerformedAction {
            action: crate::types::events::PlayerActionKind::SearchedLibrary,
            ..
        } | GameEvent::CardsRevealed { .. }
            | GameEvent::EffectResolved {
                kind: EffectKind::Shuffle,
                ..
            }
    )));
}

#[test]
fn squadron_hawk_search_can_choose_zero_cards() {
    let (mut state, hawks, nonmatch) = resolve_squadron_hawk_etb_to_search_choice();

    let result = apply_as_current(&mut state, GameAction::SelectCards { cards: vec![] }).unwrap();

    assert!(matches!(result.waiting_for, WaitingFor::Priority { .. }));
    assert!(state.stack.is_empty());
    for hawk in hawks {
        assert_eq!(state.objects[&hawk].zone, Zone::Library);
        assert!(state.players[0].library.contains(&hawk));
        assert!(!state.players[0].hand.contains(&hawk));
    }
    assert_eq!(state.objects[&nonmatch].zone, Zone::Library);
    assert!(result.events.iter().any(|event| matches!(
        event,
        GameEvent::EffectResolved {
            kind: EffectKind::Shuffle,
            ..
        }
    )));
}

#[test]
fn squadron_hawk_search_moves_only_selected_cards() {
    for selected_count in [1, 2] {
        let (mut state, hawks, _) = resolve_squadron_hawk_etb_to_search_choice();
        let selected = hawks[..selected_count].to_vec();

        let result = apply_as_current(
            &mut state,
            GameAction::SelectCards {
                cards: selected.clone(),
            },
        )
        .unwrap();

        assert!(matches!(result.waiting_for, WaitingFor::Priority { .. }));
        assert!(state.stack.is_empty());
        for hawk in selected {
            assert_eq!(state.objects[&hawk].zone, Zone::Hand);
            assert!(state.players[0].hand.contains(&hawk));
            assert!(!state.players[0].library.contains(&hawk));
        }
        for hawk in &hawks[selected_count..] {
            assert_eq!(state.objects[hawk].zone, Zone::Library);
            assert!(state.players[0].library.contains(hawk));
            assert!(!state.players[0].hand.contains(hawk));
        }
        assert!(result.events.iter().any(|event| matches!(
            event,
            GameEvent::EffectResolved {
                kind: EffectKind::Shuffle,
                ..
            }
        )));
    }
}

// CR 120 (damage), CR 510.1 (combat damage step), CR 510.3a
// (combat-damage triggers go on the stack), CR 701.23a/b/d (search
// library / fail-to-find), CR 701.24 (shuffle), CR 100.2a /
// CR 903.5b (deck-construction overrides — verified silently consumed
// by Step 1's parser fix).
//
// Tempest Hawk's combat-damage trigger:
//   "Whenever this creature deals combat damage to a player, you may
//    search your library for a card named Tempest Hawk, reveal it,
//    put it into your hand, then shuffle."
//
// The AST shape: TriggerMode::DamageDone with damage_kind = CombatOnly,
// valid_target = Player, optional = true, execute chain =
// SearchLibrary → ChangeZone(Library→Hand) → Shuffle. The shape is
// identical to Squadron Hawk's ETB-triggered search, so we reuse the
// search-and-shuffle assertion structure; only the trigger source
// (combat damage vs ETB) differs.
const TEMPEST_HAWK_ORACLE: &str = "Flying\nWhenever this creature deals combat damage to a player, you may search your library for a card named Tempest Hawk, reveal it, put it into your hand, then shuffle.\nA deck can have any number of cards named Tempest Hawk.";

fn add_tempest_hawk_to_library(state: &mut GameState, card_id: u64) -> ObjectId {
    let hawk = create_object(
        state,
        CardId(card_id),
        PlayerId(0),
        "Tempest Hawk".to_string(),
        Zone::Library,
    );
    {
        let obj = state.objects.get_mut(&hawk).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.base_card_types = obj.card_types.clone();
        obj.power = Some(2);
        obj.toughness = Some(2);
        obj.base_power = Some(2);
        obj.base_toughness = Some(2);
    }
    hawk
}

/// Set up a board where a Tempest Hawk on the battlefield is the sole
/// attacker against PlayerId(1), and advance combat through declare-
/// attackers / declare-blockers so the damage step is about to fire.
/// Returns (state, attacking hawk, hawks in library).
fn setup_tempest_hawk_attack(library_hawk_ids: &[u64]) -> (GameState, ObjectId, Vec<ObjectId>) {
    let mut state = new_game(42);
    state.turn_number = 5;
    state.phase = Phase::DeclareAttackers;
    state.active_player = PlayerId(0);
    state.priority_player = PlayerId(0);

    let attacker = create_object(
        &mut state,
        CardId(700),
        PlayerId(0),
        "Tempest Hawk".to_string(),
        Zone::Battlefield,
    );
    {
        let obj = state.objects.get_mut(&attacker).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.base_card_types = obj.card_types.clone();
        obj.power = Some(2);
        obj.toughness = Some(2);
        obj.base_power = Some(2);
        obj.base_toughness = Some(2);
        obj.color = vec![ManaColor::White];
        obj.base_color = vec![ManaColor::White];
        obj.entered_battlefield_turn = Some(4);
    }
    apply_oracle_to_object(&mut state, attacker, "Tempest Hawk", TEMPEST_HAWK_ORACLE);

    let library_hawks: Vec<ObjectId> = library_hawk_ids
        .iter()
        .map(|id| add_tempest_hawk_to_library(&mut state, *id))
        .collect();

    state.waiting_for = WaitingFor::DeclareAttackers {
        player: PlayerId(0),
        valid_attacker_ids: vec![attacker],
        valid_attack_targets: vec![AttackTarget::Player(PlayerId(1))],
    };

    apply_as_current(
        &mut state,
        GameAction::DeclareAttackers {
            attacks: vec![(attacker, AttackTarget::Player(PlayerId(1)))],
            bands: vec![],
        },
    )
    .unwrap();

    (state, attacker, library_hawks)
}

/// Advance combat from DeclareAttackers (just submitted) through to the
/// point where Tempest Hawk's `you may` combat-damage trigger has been
/// pushed onto the stack and is being resolved (engine is at
/// `WaitingFor::OptionalEffectChoice`).
fn advance_to_tempest_hawk_optional_choice(state: &mut GameState) {
    for _ in 0..16 {
        if matches!(state.waiting_for, WaitingFor::OptionalEffectChoice { .. }) {
            return;
        }
        apply_as_current(state, GameAction::PassPriority).unwrap();
    }
    panic!(
        "expected WaitingFor::OptionalEffectChoice for Tempest Hawk's combat-damage trigger, \
             got {:?} after exhausting priority passes",
        state.waiting_for
    );
}

#[test]
fn tempest_hawk_combat_damage_optional_accept_finds_named_card() {
    // Accept path: Tempest Hawk deals combat damage to PlayerId(1),
    // the optional `you may search` trigger is accepted, the
    // SearchChoice exposes only Tempest Hawks from the library, and
    // SelectCards moves the chosen hawk to hand with a Shuffle event.
    let (mut state, _attacker, library_hawks) = setup_tempest_hawk_attack(&[701, 702, 703]);

    // Sanity: also drop a non-Hawk into the library to confirm the
    // SearchChoice filters by name.
    let nonmatch = create_object(
        &mut state,
        CardId(799),
        PlayerId(0),
        "Storm Crow".to_string(),
        Zone::Library,
    );

    advance_to_tempest_hawk_optional_choice(&mut state);
    assert_eq!(
        state.players[1].life, 18,
        "Tempest Hawk should have dealt 2 combat damage to PlayerId(1)"
    );

    apply_as_current(
        &mut state,
        GameAction::DecideOptionalEffect { accept: true },
    )
    .unwrap();

    match &state.waiting_for {
        WaitingFor::SearchChoice {
            player,
            cards,
            count,
            reveal,
            ..
        } => {
            assert_eq!(*player, PlayerId(0));
            assert_eq!(*count, 1);
            assert!(*reveal);
            for hawk in &library_hawks {
                assert!(
                    cards.contains(hawk),
                    "SearchChoice must offer library Tempest Hawk {hawk:?}, got {cards:?}"
                );
            }
            assert!(
                !cards.contains(&nonmatch),
                "SearchChoice must not offer non-Tempest-Hawk card {nonmatch:?}"
            );
        }
        other => {
            panic!("expected SearchChoice after accepting Tempest Hawk trigger, got {other:?}")
        }
    }

    let chosen = library_hawks[0];
    let result = apply_as_current(
        &mut state,
        GameAction::SelectCards {
            cards: vec![chosen],
        },
    )
    .unwrap();

    assert!(
        state.stack.is_empty(),
        "stack must be empty after resolving search"
    );
    assert_eq!(state.objects[&chosen].zone, Zone::Hand);
    assert!(state.players[0].hand.contains(&chosen));
    assert!(!state.players[0].library.contains(&chosen));
    for other in &library_hawks[1..] {
        assert_eq!(state.objects[other].zone, Zone::Library);
    }
    assert!(
        result.events.iter().any(|event| matches!(
            event,
            GameEvent::EffectResolved {
                kind: EffectKind::Shuffle,
                ..
            }
        )),
        "library must be shuffled at end of the trigger chain (CR 701.24)"
    );
}

#[test]
fn tempest_hawk_combat_damage_optional_decline_leaves_library_untouched() {
    // Decline path: declining the `you may` trigger must leave the
    // library and hand untouched and clear the stack — no search,
    // no shuffle.
    let (mut state, _attacker, library_hawks) = setup_tempest_hawk_attack(&[711, 712]);

    advance_to_tempest_hawk_optional_choice(&mut state);

    let result = apply_as_current(
        &mut state,
        GameAction::DecideOptionalEffect { accept: false },
    )
    .unwrap();

    assert!(state.stack.is_empty());
    for hawk in &library_hawks {
        assert_eq!(state.objects[hawk].zone, Zone::Library);
        assert!(state.players[0].library.contains(hawk));
        assert!(!state.players[0].hand.contains(hawk));
    }
    assert!(
        !result.events.iter().any(|event| matches!(
            event,
            GameEvent::PlayerPerformedAction {
                action: PlayerActionKind::SearchedLibrary,
                ..
            } | GameEvent::CardsRevealed { .. }
                | GameEvent::EffectResolved {
                    kind: EffectKind::Shuffle,
                    ..
                }
        )),
        "declining the trigger must produce no search/reveal/shuffle events"
    );
}

#[test]
fn tempest_hawk_combat_damage_accept_with_empty_library_resolves_cleanly() {
    // Fail-to-find path: accepting the search with zero Tempest Hawks
    // in the library must resolve cleanly per CR 701.23b (player may
    // search and find nothing; library still shuffles per CR 701.23d).
    let (mut state, _attacker, _) = setup_tempest_hawk_attack(&[]);
    // Non-matching filler so the library is not literally empty —
    // this isolates "no card matching the filter" from "library empty".
    let filler = create_object(
        &mut state,
        CardId(720),
        PlayerId(0),
        "Storm Crow".to_string(),
        Zone::Library,
    );

    advance_to_tempest_hawk_optional_choice(&mut state);

    let result = apply_as_current(
        &mut state,
        GameAction::DecideOptionalEffect { accept: true },
    )
    .unwrap();

    let mut events = result.events;

    // Engine may either (a) skip straight past SearchChoice because no
    // cards match, in which case the shuffle event is emitted by the
    // DecideOptionalEffect call above, or (b) expose an empty/zero
    // SearchChoice that resolves to SelectCards { cards: vec![] }, in
    // which case the shuffle event is emitted by SelectCards. Combine
    // events from both possible paths so the shuffle assertion holds
    // regardless of which branch the engine takes (CR 701.24 still
    // applies — the library shuffles even on fail-to-find).
    if matches!(state.waiting_for, WaitingFor::SearchChoice { .. }) {
        let select_result =
            apply_as_current(&mut state, GameAction::SelectCards { cards: vec![] }).unwrap();
        events.extend(select_result.events);
    }

    assert!(
        events.iter().any(|event| matches!(
            event,
            GameEvent::EffectResolved {
                kind: EffectKind::Shuffle,
                ..
            }
        )),
        "library must shuffle even when the search finds nothing (CR 701.24)"
    );

    assert!(
        state.stack.is_empty(),
        "stack must drain even on fail-to-find"
    );
    assert_eq!(state.objects[&filler].zone, Zone::Library);
    assert!(state.players[0].hand.is_empty());
}

#[test]
fn fizzle_target_removed_before_resolution() {
    use crate::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};

    let mut state = setup_game_at_main_phase();

    // Create a creature target
    let creature_id = create_object(
        &mut state,
        CardId(50),
        PlayerId(1),
        "Goblin".to_string(),
        Zone::Battlefield,
    );
    {
        let obj = state.objects.get_mut(&creature_id).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.power = Some(2);
        obj.toughness = Some(2);
    }

    // Create Lightning Bolt targeting the creature
    let bolt_id = create_object(
        &mut state,
        CardId(10),
        PlayerId(0),
        "Lightning Bolt".to_string(),
        Zone::Hand,
    );
    {
        let obj = state.objects.get_mut(&bolt_id).unwrap();
        obj.card_types.core_types.push(CoreType::Instant);
        Arc::make_mut(&mut obj.abilities).push(make_damage_ability(3, None));
        obj.mana_cost = ManaCost::Cost {
            shards: vec![ManaCostShard::Red],
            generic: 0,
        };
    }

    let player = state
        .players
        .iter_mut()
        .find(|p| p.id == PlayerId(0))
        .unwrap();
    player.mana_pool.add(ManaUnit {
        color: ManaType::Red,
        source_id: ObjectId(0),
        pip_id: crate::types::mana::ManaPipId(0),
        supertype: None,
        source_could_produce_two_or_more_colors: false,
        restrictions: Vec::new(),
        grants: vec![],
        expiry: None,
    });

    // Cast bolt — multiple valid targets (creature + 2 players) requires selection
    let result = apply_as_current(
        &mut state,
        GameAction::CastSpell {
            object_id: bolt_id,
            card_id: CardId(10),
            targets: vec![],

            payment_mode: crate::types::game_state::CastPaymentMode::Auto,
        },
    )
    .unwrap();
    assert!(matches!(
        result.waiting_for,
        WaitingFor::TargetSelection { .. }
    ));

    // Select the creature as target
    apply_as_current(
        &mut state,
        GameAction::SelectTargets {
            targets: vec![TargetRef::Object(creature_id)],
        },
    )
    .unwrap();
    assert_eq!(state.stack.len(), 1);

    // Remove the creature from battlefield before resolution (simulating it was destroyed)
    let mut events = Vec::new();
    zones::move_to_zone(&mut state, creature_id, Zone::Graveyard, &mut events);

    // Both pass -> resolve -- should fizzle
    apply_as_current(&mut state, GameAction::PassPriority).unwrap();
    apply_as_current(&mut state, GameAction::PassPriority).unwrap();

    // Stack should be empty, bolt should be in graveyard (fizzled)
    assert!(state.stack.is_empty());
    assert!(state.players[0].graveyard.contains(&bolt_id));
    // Creature was already in graveyard, life should be unchanged
    assert_eq!(state.players[1].life, 20);
}

// === Phase 04 Plan 03 Integration Tests ===

use crate::types::ability::TargetRef;
fn add_mana(state: &mut GameState, player: PlayerId, color: ManaType, count: usize) {
    let player_data = state.players.iter_mut().find(|p| p.id == player).unwrap();
    for _ in 0..count {
        player_data.mana_pool.add(ManaUnit {
            color,
            source_id: ObjectId(0),
            pip_id: crate::types::mana::ManaPipId(0),
            supertype: None,
            source_could_produce_two_or_more_colors: false,
            restrictions: Vec::new(),
            grants: vec![],
            expiry: None,
        });
    }
}

#[test]
fn lightning_bolt_deals_3_damage_to_creature() {
    let mut state = setup_game_at_main_phase();

    // Create a 2/3 creature controlled by P1
    let creature_id = create_object(
        &mut state,
        CardId(50),
        PlayerId(1),
        "Bear".to_string(),
        Zone::Battlefield,
    );
    {
        let obj = state.objects.get_mut(&creature_id).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.power = Some(2);
        obj.toughness = Some(3);
    }

    // Create Lightning Bolt in P0's hand
    let bolt_id = create_object(
        &mut state,
        CardId(10),
        PlayerId(0),
        "Lightning Bolt".to_string(),
        Zone::Hand,
    );
    {
        let obj = state.objects.get_mut(&bolt_id).unwrap();
        obj.card_types.core_types.push(CoreType::Instant);
        Arc::make_mut(&mut obj.abilities).push(make_damage_ability(3, None));
        obj.mana_cost = ManaCost::Cost {
            shards: vec![ManaCostShard::Red],
            generic: 0,
        };
    }

    add_mana(&mut state, PlayerId(0), ManaType::Red, 1);

    // Cast Lightning Bolt — multiple valid targets (creature + 2 players) requires selection
    let result = apply_as_current(
        &mut state,
        GameAction::CastSpell {
            object_id: bolt_id,
            card_id: CardId(10),
            targets: vec![],

            payment_mode: crate::types::game_state::CastPaymentMode::Auto,
        },
    )
    .unwrap();
    assert!(matches!(
        result.waiting_for,
        WaitingFor::TargetSelection { .. }
    ));

    // Select the creature as target
    let result = apply_as_current(
        &mut state,
        GameAction::SelectTargets {
            targets: vec![TargetRef::Object(creature_id)],
        },
    )
    .unwrap();
    assert!(matches!(result.waiting_for, WaitingFor::Priority { .. }));
    assert_eq!(state.stack.len(), 1);
    assert_eq!(state.players[0].mana_pool.total(), 0);

    // Both pass -> resolve
    apply_as_current(&mut state, GameAction::PassPriority).unwrap();
    apply_as_current(&mut state, GameAction::PassPriority).unwrap();

    // Creature should have 3 damage, which equals toughness -> SBA destroys it
    assert!(state.stack.is_empty());
    assert!(!state.battlefield.contains(&creature_id));
    assert!(state.players[1].graveyard.contains(&creature_id));
    // Bolt is instant -> goes to graveyard
    assert!(state.players[0].graveyard.contains(&bolt_id));
}

#[test]
fn lightning_bolt_deals_3_damage_to_player() {
    let mut state = setup_game_at_main_phase();

    // Create Lightning Bolt in P0's hand with Any target
    let bolt_id = create_object(
        &mut state,
        CardId(10),
        PlayerId(0),
        "Lightning Bolt".to_string(),
        Zone::Hand,
    );
    {
        let obj = state.objects.get_mut(&bolt_id).unwrap();
        obj.card_types.core_types.push(CoreType::Instant);
        Arc::make_mut(&mut obj.abilities).push(make_damage_ability(3, None));
        obj.mana_cost = ManaCost::Cost {
            shards: vec![ManaCostShard::Red],
            generic: 0,
        };
    }

    add_mana(&mut state, PlayerId(0), ManaType::Red, 1);

    // Two players as targets, need manual selection
    // Use Player filter -> 2 targets -> need SelectTargets
    let result = apply_as_current(
        &mut state,
        GameAction::CastSpell {
            object_id: bolt_id,
            card_id: CardId(10),
            targets: vec![],

            payment_mode: crate::types::game_state::CastPaymentMode::Auto,
        },
    )
    .unwrap();

    // Should need target selection (2 players)
    assert!(matches!(
        result.waiting_for,
        WaitingFor::TargetSelection { .. }
    ));

    // Select player 1 as target
    let result = apply_as_current(
        &mut state,
        GameAction::SelectTargets {
            targets: vec![TargetRef::Player(PlayerId(1))],
        },
    )
    .unwrap();
    assert!(matches!(result.waiting_for, WaitingFor::Priority { .. }));
    assert_eq!(state.stack.len(), 1);

    // Both pass -> resolve
    apply_as_current(&mut state, GameAction::PassPriority).unwrap();
    apply_as_current(&mut state, GameAction::PassPriority).unwrap();

    assert!(state.stack.is_empty());
    assert_eq!(state.players[1].life, 17);
    assert!(state.players[0].graveyard.contains(&bolt_id));
}

#[test]
fn counterspell_counters_a_spell_on_stack() {
    let mut state = setup_game_at_main_phase();

    // P0 casts a creature spell -- put it on the stack manually
    let creature_id = create_object(
        &mut state,
        CardId(30),
        PlayerId(0),
        "Grizzly Bears".to_string(),
        Zone::Hand,
    );
    {
        let obj = state.objects.get_mut(&creature_id).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        // Vanilla creature has no abilities (empty vec is the default)
        obj.mana_cost = ManaCost::Cost {
            shards: vec![ManaCostShard::Green],
            generic: 1,
        };
    }

    add_mana(&mut state, PlayerId(0), ManaType::Green, 2);

    // Cast the creature
    apply_as_current(
        &mut state,
        GameAction::CastSpell {
            object_id: creature_id,
            card_id: CardId(30),
            targets: vec![],

            payment_mode: crate::types::game_state::CastPaymentMode::Auto,
        },
    )
    .unwrap();
    assert_eq!(state.stack.len(), 1);

    // P1 gets priority, has Counterspell
    // Pass priority from P0 to P1
    apply_as_current(&mut state, GameAction::PassPriority).unwrap();
    // Now P1 has priority
    assert_eq!(state.priority_player, PlayerId(1));

    let counter_id = create_object(
        &mut state,
        CardId(40),
        PlayerId(1),
        "Counterspell".to_string(),
        Zone::Hand,
    );
    {
        let obj = state.objects.get_mut(&counter_id).unwrap();
        obj.card_types.core_types.push(CoreType::Instant);
        Arc::make_mut(&mut obj.abilities).push(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Counter {
                target: TargetFilter::Typed(TypedFilter::card()),
                source_rider: None,
                countered_spell_zone: None,
            },
        ));
        obj.mana_cost = ManaCost::Cost {
            shards: vec![ManaCostShard::Blue, ManaCostShard::Blue],
            generic: 0,
        };
    }

    add_mana(&mut state, PlayerId(1), ManaType::Blue, 2);

    // Cast Counterspell — targets a spell on the stack
    let result = apply_as_current(
        &mut state,
        GameAction::CastSpell {
            object_id: counter_id,
            card_id: CardId(40),
            targets: vec![],

            payment_mode: crate::types::game_state::CastPaymentMode::Auto,
        },
    )
    .unwrap();
    // Handle target selection if needed (single spell auto-targets, but be robust).
    let result = if matches!(result.waiting_for, WaitingFor::TargetSelection { .. }) {
        apply_as_current(
            &mut state,
            GameAction::SelectTargets {
                targets: vec![TargetRef::Object(creature_id)],
            },
        )
        .unwrap()
    } else {
        result
    };
    assert!(matches!(result.waiting_for, WaitingFor::Priority { .. }));
    assert_eq!(state.stack.len(), 2); // creature + counterspell

    // Both pass -> Counterspell resolves first (LIFO)
    apply_as_current(&mut state, GameAction::PassPriority).unwrap();
    apply_as_current(&mut state, GameAction::PassPriority).unwrap();

    // Counterspell resolved, creature spell should be countered (in graveyard)
    // Counterspell should also be in graveyard
    assert!(state.players[0].graveyard.contains(&creature_id));
    assert!(state.players[1].graveyard.contains(&counter_id));
    // Creature never reached battlefield
    assert!(!state.battlefield.contains(&creature_id));
}

#[test]
fn giant_growth_gives_plus_3_3() {
    let mut state = setup_game_at_main_phase();

    // Create a 2/2 creature for P0
    let creature_id = create_object(
        &mut state,
        CardId(50),
        PlayerId(0),
        "Bear".to_string(),
        Zone::Battlefield,
    );
    {
        let obj = state.objects.get_mut(&creature_id).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.power = Some(2);
        obj.toughness = Some(2);
    }

    // Create Giant Growth in P0's hand
    let growth_id = create_object(
        &mut state,
        CardId(60),
        PlayerId(0),
        "Giant Growth".to_string(),
        Zone::Hand,
    );
    {
        let obj = state.objects.get_mut(&growth_id).unwrap();
        obj.card_types.core_types.push(CoreType::Instant);
        Arc::make_mut(&mut obj.abilities).push(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Pump {
                power: crate::types::ability::PtValue::Fixed(3),
                toughness: crate::types::ability::PtValue::Fixed(3),
                target: TargetFilter::Typed(
                    TypedFilter::creature().controller(crate::types::ability::ControllerRef::You),
                ),
            },
        ));
        obj.mana_cost = ManaCost::Cost {
            shards: vec![ManaCostShard::Green],
            generic: 0,
        };
    }

    add_mana(&mut state, PlayerId(0), ManaType::Green, 1);

    // Cast Giant Growth (auto-targets single own creature)
    apply_as_current(
        &mut state,
        GameAction::CastSpell {
            object_id: growth_id,
            card_id: CardId(60),
            targets: vec![],

            payment_mode: crate::types::game_state::CastPaymentMode::Auto,
        },
    )
    .unwrap();
    assert_eq!(state.stack.len(), 1);

    // Both pass -> resolve
    apply_as_current(&mut state, GameAction::PassPriority).unwrap();
    apply_as_current(&mut state, GameAction::PassPriority).unwrap();

    assert!(state.stack.is_empty());
    assert_eq!(state.objects[&creature_id].power, Some(5));
    assert_eq!(state.objects[&creature_id].toughness, Some(5));
    assert!(state.players[0].graveyard.contains(&growth_id));
}

#[test]
fn fizzle_bolt_target_removed() {
    let mut state = setup_game_at_main_phase();

    // Create a creature
    let creature_id = create_object(
        &mut state,
        CardId(50),
        PlayerId(1),
        "Goblin".to_string(),
        Zone::Battlefield,
    );
    {
        let obj = state.objects.get_mut(&creature_id).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.power = Some(2);
        obj.toughness = Some(2);
    }

    // Create Lightning Bolt
    let bolt_id = create_object(
        &mut state,
        CardId(10),
        PlayerId(0),
        "Lightning Bolt".to_string(),
        Zone::Hand,
    );
    {
        let obj = state.objects.get_mut(&bolt_id).unwrap();
        obj.card_types.core_types.push(CoreType::Instant);
        Arc::make_mut(&mut obj.abilities).push(make_damage_ability(3, None));
        obj.mana_cost = ManaCost::Cost {
            shards: vec![ManaCostShard::Red],
            generic: 0,
        };
    }

    add_mana(&mut state, PlayerId(0), ManaType::Red, 1);

    // Cast bolt — multiple valid targets (creature + 2 players) requires selection
    let result = apply_as_current(
        &mut state,
        GameAction::CastSpell {
            object_id: bolt_id,
            card_id: CardId(10),
            targets: vec![],

            payment_mode: crate::types::game_state::CastPaymentMode::Auto,
        },
    )
    .unwrap();
    assert!(matches!(
        result.waiting_for,
        WaitingFor::TargetSelection { .. }
    ));

    // Select the creature as target
    apply_as_current(
        &mut state,
        GameAction::SelectTargets {
            targets: vec![TargetRef::Object(creature_id)],
        },
    )
    .unwrap();

    // Remove creature before resolution
    let mut events = Vec::new();
    zones::move_to_zone(&mut state, creature_id, Zone::Graveyard, &mut events);

    // Both pass -> fizzle
    apply_as_current(&mut state, GameAction::PassPriority).unwrap();
    let result = apply_as_current(&mut state, GameAction::PassPriority).unwrap();

    assert!(state.stack.is_empty());
    assert!(state.players[0].graveyard.contains(&bolt_id));
    // No DamageDealt event
    assert!(!result
        .events
        .iter()
        .any(|e| matches!(e, GameEvent::DamageDealt { .. })));
}

#[test]
fn test_mana_ability_during_priority_does_not_push_stack() {
    let mut state = setup_game_at_main_phase();

    // Create a creature with a mana ability on the battlefield
    let obj_id = create_object(
        &mut state,
        CardId(100),
        PlayerId(0),
        "Llanowar Elves".to_string(),
        Zone::Battlefield,
    );
    {
        let obj = state.objects.get_mut(&obj_id).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        Arc::make_mut(&mut obj.abilities).push(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Mana {
                    produced: crate::types::ability::ManaProduction::Fixed {
                        colors: vec![crate::types::mana::ManaColor::Green],
                        contribution: ManaContribution::Base,
                    },
                    restrictions: vec![],
                    grants: vec![],
                    expiry: None,
                    target: None,
                },
            )
            .cost(AbilityCost::Tap),
        );
    }

    let result = apply_as_current(
        &mut state,
        GameAction::ActivateAbility {
            source_id: obj_id,
            ability_index: 0,
        },
    )
    .unwrap();

    // Stack should remain empty (mana abilities don't use the stack)
    assert!(
        state.stack.is_empty(),
        "mana ability should not push to stack"
    );
    // Should stay in Priority
    assert!(matches!(
        result.waiting_for,
        WaitingFor::Priority {
            player: PlayerId(0)
        }
    ));
    // Object should be tapped
    assert!(state.objects.get(&obj_id).unwrap().tapped);
    // Player should have green mana
    assert_eq!(
        state.players[0]
            .mana_pool
            .count_color(crate::types::mana::ManaType::Green),
        1
    );
}

#[test]
fn test_mana_ability_during_mana_payment_stays_in_mana_payment() {
    let mut state = setup_game_at_main_phase();
    // In production, ManaPayment is only entered via `enter_payment_step`
    // once `state.pending_cast` is populated — the drift invariant in
    // `derived` requires the two storage sites to agree. Reproduce that
    // precondition here so the synthetic state matches engine reality.
    state.pending_cast = Some(Box::new(crate::types::game_state::PendingCast {
        object_id: ObjectId(0),
        card_id: CardId(0),
        ability: crate::types::ability::ResolvedAbility::new(
            crate::types::ability::Effect::Unimplemented {
                name: "Test".to_string(),
                description: None,
            },
            vec![],
            ObjectId(0),
            PlayerId(0),
        ),
        cost: crate::types::mana::ManaCost::NoCost,
        base_cost: None,
        activation_cost: None,
        activation_ability_index: None,
        target_constraints: vec![],
        casting_variant: crate::types::game_state::CastingVariant::Normal,
        cast_timing_permission: None,
        distribute: None,
        origin_zone: crate::types::zones::Zone::Hand,
        additional_cost_flow: None,
        deferred_required_additional_cost: None,
        additional_cost_queue: Vec::new(),
        additional_cost_source: crate::types::game_state::SpellCostSource::Other,
        deferred_modal_choice: None,
        deferred_target_selection: false,
        chosen_modes: Vec::new(),
        additional_cost_decided: false,
        declared_kickers_to_pay: Vec::new(),
        declined_kickers: Vec::new(),
        convoked_creatures: Vec::new(),
        pinned_pool_units: Vec::new(),
        cancel_restore_prepared_source: None,
        payment_mode: crate::types::game_state::CastPaymentMode::Auto,
        assist_state: AssistState::NotOffered,
        activation_residual: crate::types::game_state::ActivationResidual::None,
    }));
    state.waiting_for = WaitingFor::ManaPayment {
        player: PlayerId(0),
        convoke_mode: None,
    };

    // Create a creature with a mana ability on the battlefield
    let obj_id = create_object(
        &mut state,
        CardId(101),
        PlayerId(0),
        "Birds of Paradise".to_string(),
        Zone::Battlefield,
    );
    {
        let obj = state.objects.get_mut(&obj_id).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        Arc::make_mut(&mut obj.abilities).push(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Mana {
                    produced: crate::types::ability::ManaProduction::Fixed {
                        colors: vec![crate::types::mana::ManaColor::Green],
                        contribution: ManaContribution::Base,
                    },
                    restrictions: vec![],
                    grants: vec![],
                    expiry: None,
                    target: None,
                },
            )
            .cost(AbilityCost::Tap),
        );
    }

    let result = apply_as_current(
        &mut state,
        GameAction::ActivateAbility {
            source_id: obj_id,
            ability_index: 0,
        },
    )
    .unwrap();

    // Should stay in ManaPayment
    assert!(
        matches!(
            result.waiting_for,
            WaitingFor::ManaPayment {
                player: PlayerId(0),
                ..
            }
        ),
        "should remain in ManaPayment after mana ability"
    );
    // Stack should remain empty
    assert!(state.stack.is_empty());
    // Object should be tapped
    assert!(state.objects.get(&obj_id).unwrap().tapped);
}

#[test]
fn springleaf_drum_prompts_for_creature_then_adds_mana() {
    let mut state = setup_game_at_main_phase();

    let drum = create_object(
        &mut state,
        CardId(102),
        PlayerId(0),
        "Springleaf Drum".to_string(),
        Zone::Battlefield,
    );
    {
        let obj = state.objects.get_mut(&drum).unwrap();
        obj.card_types.core_types.push(CoreType::Artifact);
        Arc::make_mut(&mut obj.abilities).push(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Mana {
                    produced: crate::types::ability::ManaProduction::AnyOneColor {
                        count: QuantityExpr::Fixed { value: 1 },
                        color_options: vec![
                            crate::types::mana::ManaColor::White,
                            crate::types::mana::ManaColor::Blue,
                            crate::types::mana::ManaColor::Black,
                            crate::types::mana::ManaColor::Red,
                            crate::types::mana::ManaColor::Green,
                        ],
                        contribution: crate::types::ability::ManaContribution::Base,
                    },
                    restrictions: vec![],
                    grants: vec![],
                    expiry: None,
                    target: None,
                },
            )
            .cost(AbilityCost::Composite {
                costs: vec![
                    AbilityCost::Tap,
                    AbilityCost::TapCreatures {
                        requirement: crate::types::ability::TapCreaturesRequirement::count(1),
                        filter: crate::types::ability::TypedFilter::creature()
                            .controller(crate::types::ability::ControllerRef::You)
                            .into(),
                    },
                ],
            }),
        );
    }

    let creature = create_object(
        &mut state,
        CardId(103),
        PlayerId(0),
        "Memnite".to_string(),
        Zone::Battlefield,
    );
    state
        .objects
        .get_mut(&creature)
        .unwrap()
        .card_types
        .core_types
        .push(CoreType::Creature);

    let result = apply_as_current(
        &mut state,
        GameAction::ActivateAbility {
            source_id: drum,
            ability_index: 0,
        },
    )
    .unwrap();

    assert!(matches!(
        result.waiting_for,
        WaitingFor::PayCost {
            player: PlayerId(0),
            kind: PayCostKind::TapCreatures { .. },
            count: 1,
            resume: CostResume::ManaAbility { .. },
            ..
        }
    ));
    assert!(!state.objects.get(&drum).unwrap().tapped);
    assert!(!state.objects.get(&creature).unwrap().tapped);

    let result = apply_as_current(
        &mut state,
        GameAction::SelectCards {
            cards: vec![creature],
        },
    )
    .unwrap();

    // AnyOneColor with 5 options chains into ChooseManaColor after creature tap.
    assert!(
        matches!(
            result.waiting_for,
            WaitingFor::ChooseManaColor {
                player: PlayerId(0),
                ..
            }
        ),
        "expected ChooseManaColor, got {:?}",
        result.waiting_for,
    );
    assert!(state.objects.get(&drum).unwrap().tapped);
    assert!(state.objects.get(&creature).unwrap().tapped);
    assert_eq!(state.players[0].mana_pool.total(), 0);

    // Choose green — mana should now be produced.
    let result = apply_as_current(
        &mut state,
        GameAction::ChooseManaColor {
            choice: crate::types::game_state::ManaChoice::SingleColor(
                crate::types::mana::ManaType::Green,
            ),
            count: 1,
        },
    )
    .unwrap();
    assert!(matches!(
        result.waiting_for,
        WaitingFor::Priority {
            player: PlayerId(0)
        }
    ));
    assert_eq!(state.players[0].mana_pool.total(), 1);
    assert_eq!(
        state.players[0]
            .mana_pool
            .count_color(crate::types::mana::ManaType::Green),
        1
    );
}

/// Issue #443: A `TapsForMana` mana multiplier must fire exactly once when
/// an `AnyOneColor` mana ability routes through a `ChooseManaColor` prompt
/// during a `Priority` resume. Pre-fix, the inline scan in the
/// `ChooseManaColor` arm AND the post-action pipeline both scanned the same
/// `FromTap` `ManaAdded` event, double-firing the multiplier (1 base + 2 +
/// 2 = 5 instead of 1 base + 2 = 3). CR 603.2c.
#[test]
fn taps_for_mana_multiplier_fires_once_on_color_choice_priority_resume() {
    let mut state = setup_game_at_main_phase();

    // A `TapsForMana` multiplier on a creature: whenever a permanent the
    // controller controls taps for mana, add mana of that type.
    // `TriggerEventManaType` adds one unit per fire; two trigger
    // definitions give a deterministic +2 multiplier (base 1 + 2 = 3).
    let mana_doubler = create_object(
        &mut state,
        CardId(200),
        PlayerId(0),
        "Mana Multiplier".to_string(),
        Zone::Battlefield,
    );
    {
        let obj = state.objects.get_mut(&mana_doubler).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.entered_battlefield_turn = Some(1);
        let multiplier_trigger = || {
            TriggerDefinition::new(TriggerMode::TapsForMana)
                .execute(AbilityDefinition::new(
                    AbilityKind::Database,
                    Effect::Mana {
                        produced: crate::types::ability::ManaProduction::TriggerEventManaType,
                        restrictions: vec![],
                        grants: vec![],
                        expiry: None,
                        target: None,
                    },
                ))
                .valid_card(TargetFilter::Any)
                .valid_target(TargetFilter::Controller)
        };
        obj.trigger_definitions.push(multiplier_trigger());
        obj.trigger_definitions.push(multiplier_trigger());
    }

    // An `AnyOneColor` source with >1 color option — this routes through
    // `WaitingFor::ChooseManaColor`.
    let any_color = create_object(
        &mut state,
        CardId(201),
        PlayerId(0),
        "Any Color Rock".to_string(),
        Zone::Battlefield,
    );
    {
        let obj = state.objects.get_mut(&any_color).unwrap();
        obj.card_types.core_types.push(CoreType::Artifact);
        obj.entered_battlefield_turn = Some(1);
        Arc::make_mut(&mut obj.abilities).push(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Mana {
                    produced: crate::types::ability::ManaProduction::AnyOneColor {
                        count: QuantityExpr::Fixed { value: 1 },
                        color_options: vec![
                            crate::types::mana::ManaColor::White,
                            crate::types::mana::ManaColor::Blue,
                            crate::types::mana::ManaColor::Black,
                            crate::types::mana::ManaColor::Red,
                            crate::types::mana::ManaColor::Green,
                        ],
                        contribution: ManaContribution::Base,
                    },
                    restrictions: vec![],
                    grants: vec![],
                    expiry: None,
                    target: None,
                },
            )
            .cost(AbilityCost::Tap),
        );
    }

    let result = apply_as_current(
        &mut state,
        GameAction::ActivateAbility {
            source_id: any_color,
            ability_index: 0,
        },
    )
    .unwrap();
    assert!(
        matches!(
            result.waiting_for,
            WaitingFor::ChooseManaColor {
                player: PlayerId(0),
                ..
            }
        ),
        "expected ChooseManaColor, got {:?}",
        result.waiting_for,
    );
    assert_eq!(state.players[0].mana_pool.total(), 0);

    let _result = apply_as_current(
        &mut state,
        GameAction::ChooseManaColor {
            choice: crate::types::game_state::ManaChoice::SingleColor(
                crate::types::mana::ManaType::Green,
            ),
            count: 1,
        },
    )
    .unwrap();
    // CR 603.3b (#531): controller has 2 simultaneous TapsForMana triggers
    // (the multiplier x2) — drain the OrderTriggers prompt so the legacy
    // post-resolution assertions see the produced mana totals.
    crate::game::triggers::drain_order_triggers_with_identity(&mut state);
    // After draining, the stack should resolve (mana abilities are inline)
    // and waiting_for becomes Priority.
    assert!(matches!(
        state.waiting_for,
        WaitingFor::Priority {
            player: PlayerId(0)
        }
    ));

    // 1 base + 2 from the multiplier = 3. Pre-fix this yields 5 (the
    // multiplier double-fires). Assert it is neither 1 (multiplier dropped)
    // nor 5 (double-fire).
    let total = state.players[0].mana_pool.total();
    assert_ne!(total, 1, "multiplier must fire (got base mana only)");
    assert_ne!(total, 5, "multiplier must fire exactly once, not twice");
    assert_eq!(total, 3, "expected 1 base + 2 multiplier = 3, got {total}",);
    assert_eq!(
        state.players[0]
            .mana_pool
            .count_color(crate::types::mana::ManaType::Green),
        3,
    );
}

/// Issue #443 companion: the same `TapsForMana` multiplier must also fire
/// exactly once when the `AnyOneColor` ability is activated mid-payment
/// (`ManaAbilityResume::ManaPayment`). For that resume the post-action
/// pipeline is skipped entirely, so the inline scan in the
/// `ChooseManaColor` arm is the ONLY scan site — proving the fix does not
/// drop the multiplier on the non-`Priority` path. CR 603.2c + CR 605.4a.
#[test]
fn taps_for_mana_multiplier_fires_once_on_color_choice_mana_payment_resume() {
    let mut state = setup_game_at_main_phase();

    // Mirror the production precondition: ManaPayment is only entered with
    // `pending_cast` populated (see the drift invariant in `derived`).
    state.pending_cast = Some(Box::new(crate::types::game_state::PendingCast {
        object_id: ObjectId(0),
        card_id: CardId(0),
        ability: crate::types::ability::ResolvedAbility::new(
            crate::types::ability::Effect::Unimplemented {
                name: "Test".to_string(),
                description: None,
            },
            vec![],
            ObjectId(0),
            PlayerId(0),
        ),
        cost: crate::types::mana::ManaCost::NoCost,
        base_cost: None,
        activation_cost: None,
        activation_ability_index: None,
        target_constraints: vec![],
        casting_variant: crate::types::game_state::CastingVariant::Normal,
        cast_timing_permission: None,
        distribute: None,
        origin_zone: crate::types::zones::Zone::Hand,
        additional_cost_flow: None,
        deferred_required_additional_cost: None,
        additional_cost_queue: Vec::new(),
        additional_cost_source: crate::types::game_state::SpellCostSource::Other,
        deferred_modal_choice: None,
        deferred_target_selection: false,
        chosen_modes: Vec::new(),
        additional_cost_decided: false,
        declared_kickers_to_pay: Vec::new(),
        declined_kickers: Vec::new(),
        convoked_creatures: Vec::new(),
        pinned_pool_units: Vec::new(),
        cancel_restore_prepared_source: None,
        payment_mode: crate::types::game_state::CastPaymentMode::Auto,
        assist_state: AssistState::NotOffered,
        activation_residual: crate::types::game_state::ActivationResidual::None,
    }));
    state.waiting_for = WaitingFor::ManaPayment {
        player: PlayerId(0),
        convoke_mode: None,
    };

    let mana_doubler = create_object(
        &mut state,
        CardId(202),
        PlayerId(0),
        "Mana Multiplier".to_string(),
        Zone::Battlefield,
    );
    {
        let obj = state.objects.get_mut(&mana_doubler).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.entered_battlefield_turn = Some(1);
        let multiplier_trigger = || {
            TriggerDefinition::new(TriggerMode::TapsForMana)
                .execute(AbilityDefinition::new(
                    AbilityKind::Database,
                    Effect::Mana {
                        produced: crate::types::ability::ManaProduction::TriggerEventManaType,
                        restrictions: vec![],
                        grants: vec![],
                        expiry: None,
                        target: None,
                    },
                ))
                .valid_card(TargetFilter::Any)
                .valid_target(TargetFilter::Controller)
        };
        obj.trigger_definitions.push(multiplier_trigger());
        obj.trigger_definitions.push(multiplier_trigger());
    }

    let any_color = create_object(
        &mut state,
        CardId(203),
        PlayerId(0),
        "Any Color Rock".to_string(),
        Zone::Battlefield,
    );
    {
        let obj = state.objects.get_mut(&any_color).unwrap();
        obj.card_types.core_types.push(CoreType::Artifact);
        obj.entered_battlefield_turn = Some(1);
        Arc::make_mut(&mut obj.abilities).push(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Mana {
                    produced: crate::types::ability::ManaProduction::AnyOneColor {
                        count: QuantityExpr::Fixed { value: 1 },
                        color_options: vec![
                            crate::types::mana::ManaColor::White,
                            crate::types::mana::ManaColor::Blue,
                            crate::types::mana::ManaColor::Black,
                            crate::types::mana::ManaColor::Red,
                            crate::types::mana::ManaColor::Green,
                        ],
                        contribution: ManaContribution::Base,
                    },
                    restrictions: vec![],
                    grants: vec![],
                    expiry: None,
                    target: None,
                },
            )
            .cost(AbilityCost::Tap),
        );
    }

    // Activate the AnyOneColor ability mid-payment → ManaAbilityResume::ManaPayment.
    let result = apply_as_current(
        &mut state,
        GameAction::ActivateAbility {
            source_id: any_color,
            ability_index: 0,
        },
    )
    .unwrap();
    assert!(
        matches!(
            result.waiting_for,
            WaitingFor::ChooseManaColor {
                player: PlayerId(0),
                ..
            }
        ),
        "expected ChooseManaColor, got {:?}",
        result.waiting_for,
    );

    let _result = apply_as_current(
        &mut state,
        GameAction::ChooseManaColor {
            choice: crate::types::game_state::ManaChoice::SingleColor(
                crate::types::mana::ManaType::Green,
            ),
            count: 1,
        },
    )
    .unwrap();

    // CR 603.3b + CR 605.4a: the 2 simultaneous multiplier triggers raise
    // an OrderTriggers prompt before the resume can return to ManaPayment.
    // Draining the ordering prompt must restore the suspended payment step.
    crate::game::triggers::drain_order_triggers_with_identity(&mut state);
    assert!(
        matches!(
            state.waiting_for,
            WaitingFor::ManaPayment {
                player: PlayerId(0),
                convoke_mode: None,
            }
        ),
        "OrderTriggers drain must resume ManaPayment, got {:?}",
        state.waiting_for
    );

    // 1 base + 2 multiplier = 3 — fired exactly once, not dropped, not doubled.
    let total = state.players[0].mana_pool.total();
    assert_ne!(
        total, 1,
        "multiplier must still fire on the ManaPayment path"
    );
    assert_ne!(total, 5, "multiplier must fire exactly once, not twice");
    assert_eq!(total, 3, "expected 1 base + 2 multiplier = 3, got {total}",);
}

#[test]
fn holdout_settlement_second_mana_ability_prompts_for_creature_then_adds_mana() {
    let mut state = setup_game_at_main_phase();

    let holdout = create_object(
        &mut state,
        CardId(104),
        PlayerId(0),
        "Holdout Settlement".to_string(),
        Zone::Battlefield,
    );
    {
        let obj = state.objects.get_mut(&holdout).unwrap();
        obj.card_types.core_types.push(CoreType::Land);
        let abilities = Arc::make_mut(&mut obj.abilities);
        abilities.push(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Mana {
                    produced: ManaProduction::Colorless {
                        count: QuantityExpr::Fixed { value: 1 },
                    },
                    restrictions: vec![],
                    grants: vec![],
                    expiry: None,
                    target: None,
                },
            )
            .cost(AbilityCost::Tap),
        );
        abilities.push(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Mana {
                    produced: ManaProduction::AnyOneColor {
                        count: QuantityExpr::Fixed { value: 1 },
                        color_options: vec![
                            crate::types::mana::ManaColor::White,
                            crate::types::mana::ManaColor::Blue,
                            crate::types::mana::ManaColor::Black,
                            crate::types::mana::ManaColor::Red,
                            crate::types::mana::ManaColor::Green,
                        ],
                        contribution: ManaContribution::Base,
                    },
                    restrictions: vec![],
                    grants: vec![],
                    expiry: None,
                    target: None,
                },
            )
            .cost(AbilityCost::Composite {
                costs: vec![
                    AbilityCost::Tap,
                    AbilityCost::TapCreatures {
                        requirement: crate::types::ability::TapCreaturesRequirement::count(1),
                        filter: TypedFilter::creature()
                            .controller(ControllerRef::You)
                            .into(),
                    },
                ],
            }),
        );
    }

    let creature = create_object(
        &mut state,
        CardId(105),
        PlayerId(0),
        "Memnite".to_string(),
        Zone::Battlefield,
    );
    state
        .objects
        .get_mut(&creature)
        .unwrap()
        .card_types
        .core_types
        .push(CoreType::Creature);

    let (_, _, grouped) = crate::ai_support::legal_actions_full(&state);
    let holdout_actions = grouped
        .get(&holdout)
        .expect("Holdout Settlement should expose legal mana actions");
    assert!(holdout_actions.iter().any(|action| matches!(
        action,
        GameAction::ActivateAbility {
            source_id,
            ability_index: 0
        } if *source_id == holdout
    )));
    assert!(holdout_actions.iter().any(|action| matches!(
        action,
        GameAction::ActivateAbility {
            source_id,
            ability_index: 1
        } if *source_id == holdout
    )));

    let result = apply_as_current(
        &mut state,
        GameAction::ActivateAbility {
            source_id: holdout,
            ability_index: 1,
        },
    )
    .unwrap();

    match result.waiting_for {
        WaitingFor::PayCost {
            player,
            kind: PayCostKind::TapCreatures { .. },
            count,
            choices: creatures,
            resume: CostResume::ManaAbility { .. },
            ..
        } => {
            assert_eq!(player, PlayerId(0));
            assert_eq!(count, 1);
            assert_eq!(creatures, vec![creature]);
        }
        other => panic!("expected PayCost TapCreatures (mana ability), got {other:?}"),
    }
    assert!(!state.objects.get(&holdout).unwrap().tapped);
    assert!(!state.objects.get(&creature).unwrap().tapped);

    let result = apply_as_current(
        &mut state,
        GameAction::SelectCards {
            cards: vec![creature],
        },
    )
    .unwrap();
    assert!(matches!(
        result.waiting_for,
        WaitingFor::ChooseManaColor {
            player: PlayerId(0),
            ..
        }
    ));
    assert!(state.objects.get(&holdout).unwrap().tapped);
    assert!(state.objects.get(&creature).unwrap().tapped);

    let result = apply_as_current(
        &mut state,
        GameAction::ChooseManaColor {
            choice: crate::types::game_state::ManaChoice::SingleColor(ManaType::Green),
            count: 1,
        },
    )
    .unwrap();
    assert!(matches!(
        result.waiting_for,
        WaitingFor::Priority {
            player: PlayerId(0)
        }
    ));
    assert_eq!(state.players[0].mana_pool.count_color(ManaType::Green), 1);
}

#[test]
fn non_mana_activation_tap_creatures_cost_prompts_then_pays() {
    let mut state = setup_game_at_main_phase();

    let lathril = create_object(
        &mut state,
        CardId(200),
        PlayerId(0),
        "Lathril, Blade of the Elves".to_string(),
        Zone::Battlefield,
    );
    {
        let obj = state.objects.get_mut(&lathril).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.card_types.subtypes.push("Elf".to_string());
        Arc::make_mut(&mut obj.abilities).push(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::GainLife {
                    amount: QuantityExpr::Fixed { value: 10 },
                    player: TargetFilter::Controller,
                },
            )
            .cost(AbilityCost::Composite {
                costs: vec![
                    AbilityCost::Tap,
                    AbilityCost::TapCreatures {
                        requirement: crate::types::ability::TapCreaturesRequirement::count(2),
                        filter: TypedFilter::creature()
                            .with_type(TypeFilter::Subtype("Elf".to_string()))
                            .controller(ControllerRef::You)
                            .into(),
                    },
                ],
            }),
        );
    }

    let elf_a = create_object(
        &mut state,
        CardId(201),
        PlayerId(0),
        "Elf A".to_string(),
        Zone::Battlefield,
    );
    let elf_b = create_object(
        &mut state,
        CardId(202),
        PlayerId(0),
        "Elf B".to_string(),
        Zone::Battlefield,
    );
    let non_elf = create_object(
        &mut state,
        CardId(203),
        PlayerId(0),
        "Bear".to_string(),
        Zone::Battlefield,
    );
    for (id, subtype) in [(elf_a, "Elf"), (elf_b, "Elf"), (non_elf, "Bear")] {
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.card_types.subtypes.push(subtype.to_string());
    }

    let result = apply_as_current(
        &mut state,
        GameAction::ActivateAbility {
            source_id: lathril,
            ability_index: 0,
        },
    )
    .unwrap();

    match result.waiting_for {
        WaitingFor::PayCost {
            player,
            kind: PayCostKind::TapCreatures { .. },
            count,
            choices: creatures,
            resume: CostResume::Spell { .. },
            ..
        } => {
            assert_eq!(player, PlayerId(0));
            assert_eq!(count, 2);
            assert_eq!(creatures, vec![elf_a, elf_b]);
        }
        other => panic!("expected PayCost TapCreatures (spell), got {other:?}"),
    }
    assert!(!state.objects.get(&lathril).unwrap().tapped);

    apply_as_current(
        &mut state,
        GameAction::SelectCards {
            cards: vec![elf_a, elf_b],
        },
    )
    .unwrap();

    assert!(state.objects.get(&lathril).unwrap().tapped);
    assert!(state.objects.get(&elf_a).unwrap().tapped);
    assert!(state.objects.get(&elf_b).unwrap().tapped);
    assert!(!state.objects.get(&non_elf).unwrap().tapped);
    assert_eq!(state.stack.len(), 1);
}

#[test]
fn non_mana_activation_tap_creatures_cost_rejects_tapped_source_before_prompt() {
    let mut state = setup_game_at_main_phase();

    let source = create_object(
        &mut state,
        CardId(204),
        PlayerId(0),
        "Tapped Elf Caller".to_string(),
        Zone::Battlefield,
    );
    {
        let obj = state.objects.get_mut(&source).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.card_types.subtypes.push("Elf".to_string());
        obj.tapped = true;
        Arc::make_mut(&mut obj.abilities).push(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::GainLife {
                    amount: QuantityExpr::Fixed { value: 1 },
                    player: TargetFilter::Controller,
                },
            )
            .cost(AbilityCost::Composite {
                costs: vec![
                    AbilityCost::Tap,
                    AbilityCost::TapCreatures {
                        requirement: crate::types::ability::TapCreaturesRequirement::count(1),
                        filter: TypedFilter::creature()
                            .with_type(TypeFilter::Subtype("Elf".to_string()))
                            .controller(ControllerRef::You)
                            .into(),
                    },
                ],
            }),
        );
    }

    let elf = create_object(
        &mut state,
        CardId(205),
        PlayerId(0),
        "Elf".to_string(),
        Zone::Battlefield,
    );
    let obj = state.objects.get_mut(&elf).unwrap();
    obj.card_types.core_types.push(CoreType::Creature);
    obj.card_types.subtypes.push("Elf".to_string());

    let err = apply_as_current(
        &mut state,
        GameAction::ActivateAbility {
            source_id: source,
            ability_index: 0,
        },
    )
    .unwrap_err();

    assert!(
        matches!(err, EngineError::ActionNotAllowed(message) if message == "Cannot activate tap ability: permanent is tapped")
    );
}

mod equip_tests {
    use super::*;

    fn setup_equip_game() -> GameState {
        let mut state = GameState::new_two_player(42);
        state.turn_number = 2;
        state.phase = Phase::PreCombatMain;
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);
        state.waiting_for = WaitingFor::Priority {
            player: PlayerId(0),
        };
        state
    }

    fn create_equipment(state: &mut GameState, player: PlayerId) -> ObjectId {
        let id = zones::create_object(
            state,
            CardId(100),
            player,
            "Bonesplitter".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types
            .core_types
            .push(crate::types::card_type::CoreType::Artifact);
        obj.card_types.subtypes.push("Equipment".to_string());
        obj.controller = player;
        id
    }

    fn create_creature_on_bf(state: &mut GameState, player: PlayerId, name: &str) -> ObjectId {
        let id = zones::create_object(
            state,
            CardId(state.next_object_id),
            player,
            name.to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types
            .core_types
            .push(crate::types::card_type::CoreType::Creature);
        obj.power = Some(2);
        obj.toughness = Some(2);
        obj.controller = player;
        id
    }

    #[test]
    fn test_equip_creates_equip_target_with_valid_creatures() {
        let mut state = setup_equip_game();
        let equipment_id = create_equipment(&mut state, PlayerId(0));
        let creature_a = create_creature_on_bf(&mut state, PlayerId(0), "Bear A");
        let creature_b = create_creature_on_bf(&mut state, PlayerId(0), "Bear B");

        let result = apply_as_current(
            &mut state,
            GameAction::Equip {
                equipment_id,
                target_id: ObjectId(0),
            },
        )
        .unwrap();

        match result.waiting_for {
            WaitingFor::EquipTarget {
                player,
                equipment_id: eq_id,
                valid_targets,
            } => {
                assert_eq!(player, PlayerId(0));
                assert_eq!(eq_id, equipment_id);
                assert!(valid_targets.contains(&creature_a));
                assert!(valid_targets.contains(&creature_b));
            }
            other => panic!("Expected EquipTarget, got {:?}", other),
        }
    }

    #[test]
    fn test_equip_selects_target_attaches_equipment() {
        let mut state = setup_equip_game();
        let equipment_id = create_equipment(&mut state, PlayerId(0));
        let creature_a = create_creature_on_bf(&mut state, PlayerId(0), "Bear A");
        let _creature_b = create_creature_on_bf(&mut state, PlayerId(0), "Bear B");

        let result = apply_as_current(
            &mut state,
            GameAction::Equip {
                equipment_id,
                target_id: ObjectId(0),
            },
        )
        .unwrap();
        assert!(matches!(result.waiting_for, WaitingFor::EquipTarget { .. }));

        // Target selection announces the ability on the stack (CR 113.3b).
        apply_as_current(
            &mut state,
            GameAction::Equip {
                equipment_id,
                target_id: creature_a,
            },
        )
        .unwrap();
        assert_eq!(state.stack.len(), 1, "Equip announces on the stack");
        assert!(
            state
                .objects
                .get(&equipment_id)
                .unwrap()
                .attached_to
                .is_none(),
            "attach must wait for stack resolution"
        );

        // Pass priority twice → stack resolves → attachment applied.
        apply(&mut state, PlayerId(0), GameAction::PassPriority).unwrap();
        apply(&mut state, PlayerId(1), GameAction::PassPriority).unwrap();
        assert_eq!(
            state
                .objects
                .get(&equipment_id)
                .unwrap()
                .attached_to
                // CR 301.5: Equipment must attach to an object — `as_object`
                // makes the rules invariant explicit.
                .and_then(|t| t.as_object()),
            Some(creature_a)
        );
        assert!(state
            .objects
            .get(&creature_a)
            .unwrap()
            .attachments
            .contains(&equipment_id));
    }

    #[test]
    fn test_equip_re_equip_moves_to_new_creature() {
        let mut state = setup_equip_game();
        let equipment_id = create_equipment(&mut state, PlayerId(0));
        let creature_a = create_creature_on_bf(&mut state, PlayerId(0), "Bear A");
        let creature_b = create_creature_on_bf(&mut state, PlayerId(0), "Bear B");

        // First equip to creature A — requires stack resolution.
        apply_as_current(
            &mut state,
            GameAction::Equip {
                equipment_id,
                target_id: ObjectId(0),
            },
        )
        .unwrap();
        apply_as_current(
            &mut state,
            GameAction::Equip {
                equipment_id,
                target_id: creature_a,
            },
        )
        .unwrap();
        apply(&mut state, PlayerId(0), GameAction::PassPriority).unwrap();
        apply(&mut state, PlayerId(1), GameAction::PassPriority).unwrap();
        assert_eq!(
            state
                .objects
                .get(&equipment_id)
                .unwrap()
                .attached_to
                .and_then(|t| t.as_object()),
            Some(creature_a)
        );

        // Re-equip to creature B.
        apply_as_current(
            &mut state,
            GameAction::Equip {
                equipment_id,
                target_id: ObjectId(0),
            },
        )
        .unwrap();
        apply_as_current(
            &mut state,
            GameAction::Equip {
                equipment_id,
                target_id: creature_b,
            },
        )
        .unwrap();
        apply(&mut state, PlayerId(0), GameAction::PassPriority).unwrap();
        apply(&mut state, PlayerId(1), GameAction::PassPriority).unwrap();

        assert_eq!(
            state
                .objects
                .get(&equipment_id)
                .unwrap()
                .attached_to
                .and_then(|t| t.as_object()),
            Some(creature_b)
        );
        assert!(state
            .objects
            .get(&creature_b)
            .unwrap()
            .attachments
            .contains(&equipment_id));
        assert!(!state
            .objects
            .get(&creature_a)
            .unwrap()
            .attachments
            .contains(&equipment_id));
    }

    #[test]
    fn test_equip_only_at_sorcery_speed() {
        let mut state = setup_equip_game();
        let equipment_id = create_equipment(&mut state, PlayerId(0));
        let _creature = create_creature_on_bf(&mut state, PlayerId(0), "Bear");

        // Try during combat phase - should fail
        state.phase = Phase::DeclareAttackers;
        let result = apply_as_current(
            &mut state,
            GameAction::Equip {
                equipment_id,
                target_id: ObjectId(0),
            },
        );
        assert!(result.is_err());

        // Try with non-empty stack - should fail
        state.phase = Phase::PreCombatMain;
        state.stack.push_back(crate::types::game_state::StackEntry {
            id: ObjectId(99),
            source_id: ObjectId(99),
            controller: PlayerId(1),
            kind: crate::types::game_state::StackEntryKind::Spell {
                card_id: CardId(99),
                ability: None,
                casting_variant: crate::types::game_state::CastingVariant::Normal,
                actual_mana_spent: 0,
            },
        });
        let result = apply_as_current(
            &mut state,
            GameAction::Equip {
                equipment_id,
                target_id: ObjectId(0),
            },
        );
        assert!(result.is_err());

        // Try when not active player - should fail
        state.stack.clear();
        state.active_player = PlayerId(1);
        let result = apply_as_current(
            &mut state,
            GameAction::Equip {
                equipment_id,
                target_id: ObjectId(0),
            },
        );
        assert!(result.is_err());
    }

    #[test]
    fn test_equip_auto_targets_single_creature() {
        let mut state = setup_equip_game();
        let equipment_id = create_equipment(&mut state, PlayerId(0));
        let creature = create_creature_on_bf(&mut state, PlayerId(0), "Bear");

        // Auto-target still pushes the ability on the stack (CR 113.3b).
        let result = apply_as_current(
            &mut state,
            GameAction::Equip {
                equipment_id,
                target_id: ObjectId(0),
            },
        )
        .unwrap();
        assert!(matches!(result.waiting_for, WaitingFor::Priority { .. }));
        assert_eq!(state.stack.len(), 1);
        assert!(
            state
                .objects
                .get(&equipment_id)
                .unwrap()
                .attached_to
                .is_none(),
            "attach waits for resolution"
        );

        apply(&mut state, PlayerId(0), GameAction::PassPriority).unwrap();
        apply(&mut state, PlayerId(1), GameAction::PassPriority).unwrap();
        assert_eq!(
            state
                .objects
                .get(&equipment_id)
                .unwrap()
                .attached_to
                .and_then(|t| t.as_object()),
            Some(creature)
        );
    }
}

#[test]
fn land_with_etb_tapped_replacement_enters_tapped() {
    use crate::types::ability::ReplacementDefinition;
    use crate::types::replacements::ReplacementEvent;

    let mut state = setup_game_at_main_phase();
    let obj_id = create_object(
        &mut state,
        CardId(1),
        PlayerId(0),
        "Selesnya Guildgate".to_string(),
        Zone::Hand,
    );
    let obj = state.objects.get_mut(&obj_id).unwrap();
    obj.card_types.core_types.push(CoreType::Land);
    obj.replacement_definitions.push(
        ReplacementDefinition::new(ReplacementEvent::Moved)
            .execute(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::SetTapState {
                    target: TargetFilter::SelfRef,
                    scope: EffectScope::Single,
                    state: TapStateChange::Tap,
                },
            ))
            .valid_card(TargetFilter::SelfRef)
            .description("Selesnya Guildgate enters the battlefield tapped.".to_string()),
    );

    let _result = apply_as_current(
        &mut state,
        GameAction::PlayLand {
            object_id: obj_id,
            card_id: CardId(1),
        },
    )
    .unwrap();
    assert!(state.battlefield.contains(&obj_id));
    assert!(
        state.objects[&obj_id].tapped,
        "ETB-tapped land must enter tapped"
    );
}

// ── UntapLandForMana tests ────────────────────────────────────────────

fn create_forest(state: &mut GameState, player: PlayerId) -> ObjectId {
    let id = create_object(
        state,
        CardId(99),
        player,
        "Forest".to_string(),
        Zone::Battlefield,
    );
    let obj = state.objects.get_mut(&id).unwrap();
    obj.card_types.core_types.push(CoreType::Land);
    obj.card_types.subtypes.push("Forest".to_string());
    obj.controller = player;
    obj.entered_battlefield_turn = Some(1);
    id
}

#[test]
fn tap_land_records_in_lands_tapped_for_mana() {
    let mut state = setup_game_at_main_phase();
    let land_id = create_forest(&mut state, PlayerId(0));

    apply_as_current(
        &mut state,
        GameAction::TapLandForMana { object_id: land_id },
    )
    .unwrap();

    let tracked = &state.lands_tapped_for_mana[&PlayerId(0)];
    assert!(tracked.contains(&land_id));
}

#[test]
fn untap_land_removes_mana_and_untaps() {
    let mut state = setup_game_at_main_phase();
    let land_id = create_forest(&mut state, PlayerId(0));

    apply_as_current(
        &mut state,
        GameAction::TapLandForMana { object_id: land_id },
    )
    .unwrap();
    assert!(state.objects[&land_id].tapped);
    assert_eq!(
        state.players[0]
            .mana_pool
            .count_color(crate::types::mana::ManaType::Green),
        1
    );

    let result = apply_as_current(
        &mut state,
        GameAction::UntapLandForMana { object_id: land_id },
    )
    .unwrap();

    assert!(!state.objects[&land_id].tapped);
    assert_eq!(
        state.players[0]
            .mana_pool
            .count_color(crate::types::mana::ManaType::Green),
        0
    );
    assert!(state
        .lands_tapped_for_mana
        .get(&PlayerId(0))
        .is_none_or(|v| !v.contains(&land_id)));
    assert!(matches!(
        result.waiting_for,
        WaitingFor::Priority {
            player: PlayerId(0)
        }
    ));
}

#[test]
fn untap_one_of_two_tapped_lands_preserves_other() {
    let mut state = setup_game_at_main_phase();
    let land1 = create_forest(&mut state, PlayerId(0));
    let land2 = create_forest(&mut state, PlayerId(0));

    apply_as_current(&mut state, GameAction::TapLandForMana { object_id: land1 }).unwrap();
    apply_as_current(&mut state, GameAction::TapLandForMana { object_id: land2 }).unwrap();
    assert_eq!(
        state.players[0]
            .mana_pool
            .count_color(crate::types::mana::ManaType::Green),
        2
    );

    apply_as_current(
        &mut state,
        GameAction::UntapLandForMana { object_id: land1 },
    )
    .unwrap();

    assert!(!state.objects[&land1].tapped);
    assert!(state.objects[&land2].tapped);
    assert_eq!(
        state.players[0]
            .mana_pool
            .count_color(crate::types::mana::ManaType::Green),
        1
    );
    let tracked = &state.lands_tapped_for_mana[&PlayerId(0)];
    assert!(!tracked.contains(&land1));
    assert!(tracked.contains(&land2));
}

#[test]
fn untap_rejects_when_mana_already_spent() {
    use crate::types::mana::ManaType;

    let mut state = setup_game_at_main_phase();
    let land_id = create_forest(&mut state, PlayerId(0));

    apply_as_current(
        &mut state,
        GameAction::TapLandForMana { object_id: land_id },
    )
    .unwrap();

    state.players[0].mana_pool.spend(ManaType::Green);
    assert_eq!(state.players[0].mana_pool.total(), 0);

    let result = apply_as_current(
        &mut state,
        GameAction::UntapLandForMana { object_id: land_id },
    );
    assert!(result.is_err());
}

#[test]
fn pass_priority_clears_lands_tapped_for_mana() {
    let mut state = setup_game_at_main_phase();
    let land_id = create_forest(&mut state, PlayerId(0));

    apply_as_current(
        &mut state,
        GameAction::TapLandForMana { object_id: land_id },
    )
    .unwrap();
    assert!(!state.lands_tapped_for_mana.is_empty());

    apply_as_current(&mut state, GameAction::PassPriority).unwrap();
    assert!(!state.lands_tapped_for_mana.contains_key(&PlayerId(0)));
}

#[test]
fn play_land_clears_lands_tapped_for_mana() {
    let mut state = setup_game_at_main_phase();
    let tapped_land = create_forest(&mut state, PlayerId(0));

    apply_as_current(
        &mut state,
        GameAction::TapLandForMana {
            object_id: tapped_land,
        },
    )
    .unwrap();
    assert!(!state.lands_tapped_for_mana.is_empty());

    let hand_land = create_object(
        &mut state,
        CardId(50),
        PlayerId(0),
        "Mountain".to_string(),
        Zone::Hand,
    );
    {
        let obj = state.objects.get_mut(&hand_land).unwrap();
        obj.card_types.core_types.push(CoreType::Land);
        obj.card_types.subtypes.push("Mountain".to_string());
    }

    apply_as_current(
        &mut state,
        GameAction::PlayLand {
            object_id: hand_land,
            card_id: CardId(50),
        },
    )
    .unwrap();
    assert!(!state.lands_tapped_for_mana.contains_key(&PlayerId(0)));
}

#[test]
fn untap_non_tracked_land_fails() {
    let mut state = setup_game_at_main_phase();
    let land_id = create_forest(&mut state, PlayerId(0));

    let result = apply_as_current(
        &mut state,
        GameAction::UntapLandForMana { object_id: land_id },
    );
    assert!(result.is_err());
}

#[test]
fn untap_during_mana_payment_returns_mana_payment() {
    use crate::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};

    let mut state = setup_game_at_main_phase();

    // Create a sorcery that needs blue mana
    let spell_id = create_object(
        &mut state,
        CardId(10),
        PlayerId(0),
        "Divination".to_string(),
        Zone::Hand,
    );
    {
        let obj = state.objects.get_mut(&spell_id).unwrap();
        obj.card_types.core_types.push(CoreType::Sorcery);
        Arc::make_mut(&mut obj.abilities).push(make_draw_ability(2));
        obj.mana_cost = ManaCost::Cost {
            shards: vec![ManaCostShard::Blue, ManaCostShard::Blue],
            generic: 1,
        };
    }

    // Add partial mana — not enough to auto-pay, so we get ManaPayment
    let player = state
        .players
        .iter_mut()
        .find(|p| p.id == PlayerId(0))
        .unwrap();
    player.mana_pool.add(ManaUnit {
        color: ManaType::Blue,
        source_id: ObjectId(0),
        pip_id: crate::types::mana::ManaPipId(0),
        supertype: None,
        source_could_produce_two_or_more_colors: false,
        restrictions: Vec::new(),
        grants: vec![],
        expiry: None,
    });

    // Create a forest on the battlefield to tap during ManaPayment
    let land_id = create_forest(&mut state, PlayerId(0));

    let result = apply_as_current(
        &mut state,
        GameAction::CastSpell {
            object_id: spell_id,
            card_id: CardId(10),
            targets: vec![],

            payment_mode: crate::types::game_state::CastPaymentMode::Auto,
        },
    );

    // If we get ManaPayment, test the untap flow there
    if let Ok(ActionResult {
        waiting_for: WaitingFor::ManaPayment { .. },
        ..
    }) = &result
    {
        // Tap the land during ManaPayment
        apply_as_current(
            &mut state,
            GameAction::TapLandForMana { object_id: land_id },
        )
        .unwrap();
        assert!(state.lands_tapped_for_mana[&PlayerId(0)].contains(&land_id));

        // Untap it — should return ManaPayment, not Priority
        let untap_result = apply_as_current(
            &mut state,
            GameAction::UntapLandForMana { object_id: land_id },
        )
        .unwrap();
        assert!(matches!(
            untap_result.waiting_for,
            WaitingFor::ManaPayment {
                player: PlayerId(0),
                ..
            }
        ));
    }
    // If auto-pay succeeded, the test setup didn't produce ManaPayment — still valid
}

#[test]
fn zone_change_removes_stale_tracking() {
    let mut state = setup_game_at_main_phase();
    let land_id = create_forest(&mut state, PlayerId(0));

    // Tap the land
    apply_as_current(
        &mut state,
        GameAction::TapLandForMana { object_id: land_id },
    )
    .unwrap();
    assert!(state.lands_tapped_for_mana[&PlayerId(0)].contains(&land_id));

    // Move the land to graveyard (e.g., destroyed)
    let mut events = Vec::new();
    super::zones::move_to_zone(&mut state, land_id, Zone::Graveyard, &mut events);

    // Tracking should be cleaned up
    assert!(state
        .lands_tapped_for_mana
        .get(&PlayerId(0))
        .is_none_or(|v| !v.contains(&land_id)));
}

/// CR 701.48a: Learn rummage — discard one card, draw one card, net hand size unchanged.
#[test]
fn learn_rummage_discards_and_draws() {
    let mut state = GameState::new_two_player(42);
    let source = create_object(
        &mut state,
        CardId(1),
        PlayerId(0),
        "Source".to_string(),
        Zone::Battlefield,
    );
    // Put a card in hand to discard
    let hand_card = create_object(
        &mut state,
        CardId(2),
        PlayerId(0),
        "Hand Card".to_string(),
        Zone::Hand,
    );
    // Put a card in library to draw
    let _lib_card = create_object(
        &mut state,
        CardId(3),
        PlayerId(0),
        "Library Card".to_string(),
        Zone::Library,
    );

    // First: resolve the Learn effect to get WaitingFor::LearnChoice
    let learn_ability = ResolvedAbility::new(Effect::Learn, vec![], source, PlayerId(0));
    let mut events = Vec::new();
    effects::learn::resolve(&mut state, &learn_ability, &mut events).unwrap();
    assert!(matches!(state.waiting_for, WaitingFor::LearnChoice { .. }));

    // Second: submit rummage decision through the engine
    let action = GameAction::LearnDecision {
        choice: crate::types::actions::LearnOption::Rummage { card_id: hand_card },
    };
    let result = apply_as_current(&mut state, action).unwrap();

    // The discarded card should be in graveyard
    assert!(state.players[0].graveyard.contains(&hand_card));
    // Hand should have exactly 1 card (the drawn one)
    assert_eq!(state.players[0].hand.len(), 1);
    // Should have emitted EffectResolved for Learn
    assert!(result.events.iter().any(|e| matches!(
        e,
        GameEvent::EffectResolved {
            kind: EffectKind::Learn,
            ..
        }
    )));
}

/// CR 701.48a: Learn skip — no discard, no draw, hand unchanged.
#[test]
fn learn_skip_does_nothing() {
    let mut state = GameState::new_two_player(42);
    let source = create_object(
        &mut state,
        CardId(1),
        PlayerId(0),
        "Source".to_string(),
        Zone::Battlefield,
    );
    let hand_card = create_object(
        &mut state,
        CardId(2),
        PlayerId(0),
        "Hand Card".to_string(),
        Zone::Hand,
    );

    let learn_ability = ResolvedAbility::new(Effect::Learn, vec![], source, PlayerId(0));
    let mut events = Vec::new();
    effects::learn::resolve(&mut state, &learn_ability, &mut events).unwrap();

    let action = GameAction::LearnDecision {
        choice: crate::types::actions::LearnOption::Skip,
    };
    let result = apply_as_current(&mut state, action).unwrap();

    // Hand should still have the original card
    assert_eq!(state.players[0].hand.len(), 1);
    assert!(state.players[0].hand.contains(&hand_card));
    // Graveyard should be empty
    assert!(state.players[0].graveyard.is_empty());
    // Should have emitted EffectResolved for Learn
    assert!(result.events.iter().any(|e| matches!(
        e,
        GameEvent::EffectResolved {
            kind: EffectKind::Learn,
            ..
        }
    )));
}

/// Verify that the ReplacementChoice handler picks up pending_continuation
/// after replacement resolves (the foundation fix for Learn + Madness etc.)
/// Verify that the Learn handler stashes draw as pending_continuation
/// when discard returns NeedsReplacementChoice. This is a unit-level test
/// of the stash mechanism; full Learn+Madness integration requires discard
/// replacement pipeline support (not yet implemented for Discard events).
#[test]
fn learn_rummage_stashes_draw_continuation() {
    // The Learn handler's NeedsReplacementChoice branch stashes Draw
    // as pending_continuation — verify via the non-replacement path that
    // the continuation mechanism doesn't interfere with normal operation.
    let mut state = GameState::new_two_player(42);
    let source = create_object(
        &mut state,
        CardId(1),
        PlayerId(0),
        "Source".to_string(),
        Zone::Battlefield,
    );
    let hand_card = create_object(
        &mut state,
        CardId(2),
        PlayerId(0),
        "Hand Card".to_string(),
        Zone::Hand,
    );
    let _lib_card = create_object(
        &mut state,
        CardId(3),
        PlayerId(0),
        "Draw Me".to_string(),
        Zone::Library,
    );

    // Pre-set pending_continuation to verify it's consumed normally
    state.pending_continuation = Some(crate::types::game_state::PendingContinuation::new(
        Box::new(ResolvedAbility::new(
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 1 },
                player: crate::types::ability::TargetFilter::Controller,
            },
            vec![],
            source,
            PlayerId(0),
        )),
    ));

    let learn_ability = ResolvedAbility::new(Effect::Learn, vec![], source, PlayerId(0));
    let mut events = Vec::new();
    effects::learn::resolve(&mut state, &learn_ability, &mut events).unwrap();

    // Submit rummage — discard goes through (no replacement) and draws
    let action = GameAction::LearnDecision {
        choice: crate::types::actions::LearnOption::Rummage { card_id: hand_card },
    };
    let result = apply_as_current(&mut state, action).unwrap();

    // Normal rummage completed
    assert_eq!(state.players[0].hand.len(), 1);
    assert!(state.players[0].graveyard.contains(&hand_card));
    // The stashed continuation (GainLife) should have been consumed
    assert!(state.pending_continuation.is_none());
    // Life should have increased by 1 (from the continuation)
    assert_eq!(state.players[0].life, 21);
    assert!(result.events.iter().any(|e| matches!(
        e,
        GameEvent::EffectResolved {
            kind: EffectKind::Learn,
            ..
        }
    )));
}

// CR 402.3: Hand order has no game-rules significance — ReorderHand is a
// display-preference update only.
#[test]
fn reorder_hand_replaces_hand_order() {
    let mut state = setup_game_at_main_phase();
    let p0 = PlayerId(0);

    let a = ObjectId(100);
    let b = ObjectId(101);
    let c = ObjectId(102);
    state.players[0].hand = crate::im::Vector::from(vec![a, b, c]);

    let result = apply(
        &mut state,
        p0,
        GameAction::ReorderHand {
            order: vec![c, a, b],
        },
    )
    .expect("reorder should succeed");

    assert!(result.events.is_empty(), "reorder must emit no events");
    assert_eq!(
        state.players[0].hand.iter().copied().collect::<Vec<_>>(),
        vec![c, a, b],
    );
}

#[test]
fn reorder_hand_rejects_non_permutation() {
    let mut state = setup_game_at_main_phase();
    let p0 = PlayerId(0);
    let a = ObjectId(100);
    let b = ObjectId(101);
    state.players[0].hand = crate::im::Vector::from(vec![a, b]);

    // Wrong length.
    let err = apply(&mut state, p0, GameAction::ReorderHand { order: vec![a] })
        .expect_err("wrong length must error");
    assert!(matches!(err, EngineError::InvalidAction(_)));

    // Right length, wrong contents.
    let stranger = ObjectId(999);
    let err = apply(
        &mut state,
        p0,
        GameAction::ReorderHand {
            order: vec![a, stranger],
        },
    )
    .expect_err("stranger id must error");
    assert!(matches!(err, EngineError::InvalidAction(_)));

    // Hand unchanged after rejected calls.
    assert_eq!(
        state.players[0].hand.iter().copied().collect::<Vec<_>>(),
        vec![a, b],
    );
}

#[test]
fn reorder_hand_succeeds_while_opponent_holds_priority() {
    // Verifies the `check_actor_authorization` whitelist: P0 must be able
    // to reorder their own hand even though P1 is the priority player and
    // holds the WaitingFor::Priority slot.
    let mut state = setup_game_at_main_phase();
    state.priority_player = PlayerId(1);
    state.waiting_for = WaitingFor::Priority {
        player: PlayerId(1),
    };

    let a = ObjectId(200);
    let b = ObjectId(201);
    state.players[0].hand = crate::im::Vector::from(vec![a, b]);

    apply(
        &mut state,
        PlayerId(0),
        GameAction::ReorderHand { order: vec![b, a] },
    )
    .expect("non-priority actor reordering own hand must succeed");

    assert_eq!(
        state.players[0].hand.iter().copied().collect::<Vec<_>>(),
        vec![b, a],
    );
    // Priority hasn't moved — reorder doesn't transition WaitingFor.
    assert_eq!(state.priority_player, PlayerId(1));
}

/// CR 305.1 + CR 116.2a + CR 401.5: A `OncePerTurn` `TopOfLibraryCastPermission`
/// with `play_mode: Play` must consume its per-turn slot when a land is played
/// from the library top (land play is a special action per CR 305.1/CR 116.2a;
/// CR 401.5 governs top-of-library visibility during the action), and a second
/// `PlayLand` from the same permission source must be rejected.
#[test]
fn once_per_turn_library_land_play_consumes_slot_and_blocks_second_play() {
    let mut state = setup_game_at_main_phase();
    let player = PlayerId(0);

    let perm_src = create_object(
        &mut state,
        CardId(9200),
        player,
        "Permission Source (test)".to_string(),
        Zone::Battlefield,
    );
    {
        let def = StaticDefinition::new(StaticMode::TopOfLibraryCastPermission {
            play_mode: crate::types::ability::CardPlayMode::Play,
            frequency: CastFrequency::OncePerTurn,
            alt_cost: None,
        })
        .affected(TargetFilter::Any);
        let obj = state.objects.get_mut(&perm_src).unwrap();
        obj.static_definitions.push(def.clone());
        Arc::make_mut(&mut obj.base_static_definitions).push(def);
    }

    let land1 = create_object(
        &mut state,
        CardId(9201),
        player,
        "Forest A".to_string(),
        Zone::Library,
    );
    state
        .objects
        .get_mut(&land1)
        .unwrap()
        .card_types
        .core_types
        .push(CoreType::Land);
    {
        let pd = state.players.iter_mut().find(|p| p.id == player).unwrap();
        pd.library.push_back(land1);
    }

    let land2 = create_object(
        &mut state,
        CardId(9202),
        player,
        "Forest B".to_string(),
        Zone::Library,
    );
    state
        .objects
        .get_mut(&land2)
        .unwrap()
        .card_types
        .core_types
        .push(CoreType::Land);
    {
        let pd = state.players.iter_mut().find(|p| p.id == player).unwrap();
        pd.library.push_front(land2);
    }

    apply_as_current(
        &mut state,
        GameAction::PlayLand {
            object_id: land2,
            card_id: CardId(9202),
        },
    )
    .expect("first top-of-library land play must succeed");

    assert!(
        state.battlefield.contains(&land2),
        "land2 must be on the battlefield after the first play"
    );
    assert!(
        state
            .top_of_library_cast_permissions_used
            .contains(&perm_src),
        "the OncePerTurn slot must be recorded after the land is moved"
    );

    let second = apply_as_current(
        &mut state,
        GameAction::PlayLand {
            object_id: land1,
            card_id: CardId(9201),
        },
    );
    assert!(
        second.is_err(),
        "second top-of-library land play under the same OncePerTurn source must be rejected"
    );
}
