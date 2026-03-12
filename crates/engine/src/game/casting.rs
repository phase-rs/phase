use std::collections::HashSet;

use crate::types::ability::{AbilityDefinition, AbilityKind, Effect, ResolvedAbility, TargetRef};
use crate::types::card_type::CoreType;
use crate::types::events::GameEvent;
use crate::types::game_state::{GameState, PendingCast, StackEntry, StackEntryKind, WaitingFor};
use crate::types::identifiers::{CardId, ObjectId};
use crate::types::mana::{ManaCostShard, ManaType};
use crate::types::phase::Phase;
use crate::types::player::PlayerId;
use crate::types::zones::Zone;

use super::engine::EngineError;
use super::mana_abilities;
use super::mana_payment;
use super::mana_sources::{self, ManaSourceOption};
use super::stack;
use super::targeting;
use super::zones;

/// Emit `BecomesTarget` and `CrimeCommitted` events for each target.
///
/// Called whenever targets are locked in for a spell or ability. Per MTG rules,
/// targeting an opponent, their permanent, or a card in their graveyard is a crime.
pub(crate) fn emit_targeting_events(
    state: &GameState,
    targets: &[TargetRef],
    source_id: ObjectId,
    controller: PlayerId,
    events: &mut Vec<GameEvent>,
) {
    let mut crime_committed = false;
    for target in targets {
        match target {
            TargetRef::Object(obj_id) => {
                events.push(GameEvent::BecomesTarget {
                    object_id: *obj_id,
                    source_id,
                });
                if !crime_committed {
                    if let Some(obj) = state.objects.get(obj_id) {
                        if obj.controller != controller && obj.owner != controller {
                            crime_committed = true;
                        }
                    }
                }
            }
            TargetRef::Player(pid) => {
                if !crime_committed && *pid != controller {
                    crime_committed = true;
                }
            }
        }
    }
    if crime_committed {
        events.push(GameEvent::CrimeCommitted {
            player_id: controller,
        });
    }
}

/// Cast a spell from hand (or command zone in Commander format).
pub fn handle_cast_spell(
    state: &mut GameState,
    player: PlayerId,
    card_id: CardId,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    // 1. Find object in player's hand matching card_id
    let player_data = state
        .players
        .iter()
        .find(|p| p.id == player)
        .expect("player exists");

    let object_id = player_data
        .hand
        .iter()
        .find(|&&obj_id| {
            state
                .objects
                .get(&obj_id)
                .map(|obj| obj.card_id == card_id)
                .unwrap_or(false)
        })
        .copied()
        .or_else(|| {
            // In Commander format, also check the command zone
            if !state.format_config.command_zone {
                return None;
            }
            state
                .objects
                .values()
                .find(|obj| {
                    obj.card_id == card_id
                        && obj.owner == player
                        && obj.zone == Zone::Command
                        && obj.is_commander
                })
                .map(|obj| obj.id)
        })
        .ok_or_else(|| EngineError::InvalidAction("Card not found in hand".to_string()))?;

    let obj = state.objects.get(&object_id).unwrap();

    // 2. Get the first ability (or use default for vanilla permanents)
    let ability_def = if obj.abilities.is_empty() {
        // Vanilla creatures/enchantments/etc. have no explicit ability text
        // but are still castable -- they resolve by entering the battlefield.
        AbilityDefinition {
            kind: AbilityKind::Spell,
            effect: Effect::Unimplemented {
                name: "PermanentNoncreature".to_string(),
                description: None,
            },
            cost: None,
            sub_ability: None,
            duration: None,
            description: None,
            target_prompt: None,
            sorcery_speed: false,
        }
    } else {
        obj.abilities[0].clone()
    };

    // 3. Validate timing
    let is_instant_speed = obj.card_types.core_types.contains(&CoreType::Instant)
        || obj.has_keyword(&crate::types::keywords::Keyword::Flash);

    if !is_instant_speed && ability_def.kind == AbilityKind::Spell {
        // Sorcery-speed: main phase + empty stack + active player
        match state.phase {
            Phase::PreCombatMain | Phase::PostCombatMain => {}
            _ => {
                return Err(EngineError::ActionNotAllowed(
                    "Sorcery-speed spells can only be cast during main phases".to_string(),
                ));
            }
        }
        if !state.stack.is_empty() {
            return Err(EngineError::ActionNotAllowed(
                "Sorcery-speed spells can only be cast when the stack is empty".to_string(),
            ));
        }
        if state.active_player != player {
            return Err(EngineError::ActionNotAllowed(
                "Sorcery-speed spells can only be cast by the active player".to_string(),
            ));
        }
    }

    // 3b. Color identity check (Commander format only)
    if state.format_config.command_zone {
        let obj = state.objects.get(&object_id).unwrap();
        if !super::commander::can_cast_in_color_identity(state, &obj.color, player) {
            return Err(EngineError::ActionNotAllowed(
                "Card is outside commander's color identity".to_string(),
            ));
        }
    }

    // 4. Build ResolvedAbility from typed fields -- no params/svars
    let obj = state.objects.get(&object_id).unwrap();
    let casting_from_command_zone = obj.zone == Zone::Command;
    let mut mana_cost = obj.mana_cost.clone();

    // Apply commander tax when casting from command zone
    if casting_from_command_zone {
        let tax = super::commander::commander_tax(state, object_id);
        if tax > 0 {
            match &mut mana_cost {
                crate::types::mana::ManaCost::Cost { generic, .. } => {
                    *generic += tax;
                }
                crate::types::mana::ManaCost::NoCost => {
                    mana_cost = crate::types::mana::ManaCost::Cost {
                        shards: vec![],
                        generic: tax,
                    };
                }
            }
        }
    }
    let resolved = ResolvedAbility {
        effect: ability_def.effect.clone(),
        targets: Vec::new(),
        source_id: object_id,
        controller: player,
        sub_ability: ability_def
            .sub_ability
            .as_ref()
            .map(|sub| Box::new(build_resolved_from_def(sub, object_id, player))),
        duration: None,
    };

    // 5. Handle targeting -- ensure layers evaluated before target legality
    if state.layers_dirty {
        super::layers::evaluate_layers(state);
    }

    // Check if this is an Aura spell -- Auras target via Enchant keyword, not via effect targets
    // Re-read obj after evaluate_layers (which needs &mut state)
    let obj = state.objects.get(&object_id).unwrap();
    let is_aura = obj.card_types.subtypes.iter().any(|s| s == "Aura");
    if is_aura {
        let enchant_filter = obj.keywords.iter().find_map(|k| {
            if let crate::types::keywords::Keyword::Enchant(filter) = k {
                Some(filter.clone())
            } else {
                None
            }
        });
        if let Some(filter) = enchant_filter {
            let legal = targeting::find_legal_targets_typed(state, &filter, player, object_id);
            if legal.is_empty() {
                return Err(EngineError::ActionNotAllowed(
                    "No legal targets for Aura".to_string(),
                ));
            }
            if legal.len() == 1 {
                let mut resolved = resolved;
                resolved.targets = legal;
                return pay_and_push(
                    state, player, object_id, card_id, resolved, &mana_cost, events,
                );
            } else {
                return Ok(WaitingFor::TargetSelection {
                    player,
                    pending_cast: Box::new(PendingCast {
                        object_id,
                        card_id,
                        ability: resolved,
                        cost: mana_cost,
                    }),
                    legal_targets: legal,
                });
            }
        }
    }

    // Targeting uses typed target_prompt or target filter from ability_def
    // For now, use string-based targeting via the old filter system
    // until the typed targeting infrastructure is complete
    let has_targets = has_targeting_requirement(&ability_def);
    if has_targets {
        let valid_tgts = get_valid_tgts_string(&ability_def);
        let legal = targeting::find_legal_targets(state, &valid_tgts, player, object_id);
        if legal.is_empty() {
            return Err(EngineError::ActionNotAllowed(
                "No legal targets available".to_string(),
            ));
        }
        if legal.len() == 1 {
            // Auto-target
            let mut resolved = resolved;
            resolved.targets = legal;
            return pay_and_push(
                state, player, object_id, card_id, resolved, &mana_cost, events,
            );
        } else {
            // Need target selection from player
            return Ok(WaitingFor::TargetSelection {
                player,
                pending_cast: Box::new(PendingCast {
                    object_id,
                    card_id,
                    ability: resolved,
                    cost: mana_cost,
                }),
                legal_targets: legal,
            });
        }
    }

    // 6. Pay mana cost
    pay_and_push(
        state, player, object_id, card_id, resolved, &mana_cost, events,
    )
}

/// Returns true if the spell has at least one legal target (or requires no targets).
/// Used by phase-ai's legal_actions to avoid including uncastable spells in the action set.
pub fn spell_has_legal_targets(
    state: &GameState,
    obj: &crate::game::game_object::GameObject,
    player: PlayerId,
) -> bool {
    // Aura spells target via the Enchant keyword rather than the effect's target field.
    let is_aura = obj.card_types.subtypes.iter().any(|s| s == "Aura");
    if is_aura {
        let enchant_filter = obj.keywords.iter().find_map(|k| {
            if let crate::types::keywords::Keyword::Enchant(filter) = k {
                Some(filter.clone())
            } else {
                None
            }
        });
        return enchant_filter.is_some_and(|filter| {
            !targeting::find_legal_targets_typed(state, &filter, player, obj.id).is_empty()
        });
    }

    let ability_def = match obj.abilities.first() {
        Some(a) => a,
        None => return true, // Vanilla permanent needs no targets
    };

    if !has_targeting_requirement(ability_def) {
        return true;
    }

    let valid_tgts = get_valid_tgts_string(ability_def);
    !targeting::find_legal_targets(state, &valid_tgts, player, obj.id).is_empty()
}

/// Check if an ability definition has a targeting requirement.
fn has_targeting_requirement(def: &AbilityDefinition) -> bool {
    use crate::types::ability::TargetFilter;
    match &def.effect {
        Effect::DealDamage { target, .. }
        | Effect::Pump { target, .. }
        | Effect::Destroy { target, .. }
        | Effect::Counter { target, .. }
        | Effect::Tap { target, .. }
        | Effect::Untap { target, .. }
        | Effect::Sacrifice { target, .. }
        | Effect::GainControl { target, .. }
        | Effect::Attach { target, .. }
        | Effect::Fight { target, .. }
        | Effect::Bounce { target, .. }
        | Effect::CopySpell { target, .. } => !matches!(
            target,
            TargetFilter::None | TargetFilter::SelfRef | TargetFilter::Controller
        ),
        Effect::GenericEffect {
            target: Some(target),
            ..
        } => !matches!(
            target,
            TargetFilter::None | TargetFilter::SelfRef | TargetFilter::Controller
        ),
        _ => false,
    }
}

/// Extract a string-based filter for targeting compatibility.
/// This bridges typed TargetFilter to the existing string-based targeting system.
fn get_valid_tgts_string(def: &AbilityDefinition) -> String {
    use crate::types::ability::TargetFilter;
    let target = match &def.effect {
        Effect::DealDamage { target, .. }
        | Effect::Pump { target, .. }
        | Effect::Destroy { target, .. }
        | Effect::Counter { target, .. }
        | Effect::Tap { target, .. }
        | Effect::Untap { target, .. }
        | Effect::Sacrifice { target, .. }
        | Effect::GainControl { target, .. }
        | Effect::Attach { target, .. }
        | Effect::Fight { target, .. }
        | Effect::Bounce { target, .. }
        | Effect::CopySpell { target, .. } => target,
        Effect::GenericEffect {
            target: Some(target),
            ..
        } => target,
        _ => return "Any".to_string(),
    };
    match target {
        TargetFilter::Any => "Any".to_string(),
        TargetFilter::Player => "Player".to_string(),
        TargetFilter::Controller => "Player.You".to_string(),
        TargetFilter::Typed {
            card_type,
            controller,
            ..
        } => {
            let type_str = match card_type {
                Some(crate::types::ability::TypeFilter::Creature) => "Creature",
                Some(crate::types::ability::TypeFilter::Land) => "Land",
                Some(crate::types::ability::TypeFilter::Artifact) => "Artifact",
                Some(crate::types::ability::TypeFilter::Enchantment) => "Enchantment",
                Some(crate::types::ability::TypeFilter::Card) => "Card",
                _ => "Any",
            };
            let ctrl_str = match controller {
                Some(crate::types::ability::ControllerRef::You) => ".YouCtrl",
                Some(crate::types::ability::ControllerRef::Opponent) => ".OppCtrl",
                None => "",
            };
            format!("{}{}", type_str, ctrl_str)
        }
        _ => "Any".to_string(),
    }
}

/// Build a ResolvedAbility from an AbilityDefinition recursively.
fn build_resolved_from_def(
    def: &AbilityDefinition,
    source_id: ObjectId,
    controller: PlayerId,
) -> ResolvedAbility {
    ResolvedAbility {
        effect: def.effect.clone(),
        targets: Vec::new(),
        source_id,
        controller,
        sub_ability: def
            .sub_ability
            .as_ref()
            .map(|sub| Box::new(build_resolved_from_def(sub, source_id, controller))),
        duration: def.duration.clone(),
    }
}

/// Handle target selection for a pending cast.
pub fn handle_select_targets(
    state: &mut GameState,
    player: PlayerId,
    targets: Vec<TargetRef>,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    // Extract PendingCast from WaitingFor::TargetSelection
    let pending = match &state.waiting_for {
        WaitingFor::TargetSelection {
            pending_cast,
            legal_targets,
            ..
        } => {
            // Validate targets are legal
            for t in &targets {
                if !legal_targets.contains(t) {
                    return Err(EngineError::InvalidAction(
                        "Illegal target selected".to_string(),
                    ));
                }
            }
            *pending_cast.clone()
        }
        _ => {
            return Err(EngineError::InvalidAction(
                "Not waiting for target selection".to_string(),
            ));
        }
    };

    let mut ability = pending.ability;
    ability.targets = targets;

    pay_and_push(
        state,
        player,
        pending.object_id,
        pending.card_id,
        ability,
        &pending.cost,
        events,
    )
}

/// Activate an ability from a permanent on the battlefield.
pub fn handle_activate_ability(
    state: &mut GameState,
    player: PlayerId,
    source_id: ObjectId,
    ability_index: usize,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    let obj = state
        .objects
        .get(&source_id)
        .ok_or_else(|| EngineError::InvalidAction("Object not found".to_string()))?;

    if obj.zone != Zone::Battlefield {
        return Err(EngineError::InvalidAction(
            "Object is not on the battlefield".to_string(),
        ));
    }
    if obj.controller != player {
        return Err(EngineError::NotYourPriority);
    }
    if ability_index >= obj.abilities.len() {
        return Err(EngineError::InvalidAction(
            "Invalid ability index".to_string(),
        ));
    }

    let ability_def = obj.abilities[ability_index].clone();

    // Handle tap cost
    let has_tap_cost = matches!(
        ability_def.cost,
        Some(crate::types::ability::AbilityCost::Tap)
    );

    if has_tap_cost {
        let obj = state.objects.get(&source_id).unwrap();
        if obj.tapped {
            return Err(EngineError::ActionNotAllowed(
                "Cannot activate tap ability: permanent is tapped".to_string(),
            ));
        }
        let obj = state.objects.get_mut(&source_id).unwrap();
        obj.tapped = true;
        events.push(GameEvent::PermanentTapped {
            object_id: source_id,
        });
    }

    let resolved = ResolvedAbility {
        effect: ability_def.effect.clone(),
        targets: Vec::new(),
        source_id,
        controller: player,
        sub_ability: ability_def
            .sub_ability
            .as_ref()
            .map(|sub| Box::new(build_resolved_from_def(sub, source_id, player))),
        duration: None,
    };

    // Handle targeting
    if has_targeting_requirement(&ability_def) {
        let valid_tgts = get_valid_tgts_string(&ability_def);
        let legal = targeting::find_legal_targets(state, &valid_tgts, player, source_id);
        if legal.is_empty() {
            return Err(EngineError::ActionNotAllowed(
                "No legal targets available".to_string(),
            ));
        }
        if legal.len() == 1 {
            let mut resolved = resolved;
            resolved.targets = legal;

            emit_targeting_events(state, &resolved.targets, source_id, player, events);

            // Fall through to push to stack
            let entry_id = ObjectId(state.next_object_id);
            state.next_object_id += 1;

            stack::push_to_stack(
                state,
                StackEntry {
                    id: entry_id,
                    source_id,
                    controller: player,
                    kind: StackEntryKind::ActivatedAbility {
                        source_id,
                        ability: resolved,
                    },
                },
                events,
            );

            events.push(GameEvent::AbilityActivated { source_id });
            state.priority_passes.clear();
            state.priority_pass_count = 0;
            return Ok(WaitingFor::Priority { player });
        } else {
            // For activated abilities, we need target selection too
            // Use a PendingCast with a dummy card_id
            return Ok(WaitingFor::TargetSelection {
                player,
                pending_cast: Box::new(PendingCast {
                    object_id: source_id,
                    card_id: CardId(0),
                    ability: resolved,
                    cost: crate::types::mana::ManaCost::NoCost,
                }),
                legal_targets: legal,
            });
        }
    }

    // Push to stack
    let entry_id = ObjectId(state.next_object_id);
    state.next_object_id += 1;

    stack::push_to_stack(
        state,
        StackEntry {
            id: entry_id,
            source_id,
            controller: player,
            kind: StackEntryKind::ActivatedAbility {
                source_id,
                ability: resolved,
            },
        },
        events,
    );

    events.push(GameEvent::AbilityActivated { source_id });

    state.priority_passes.clear();
    state.priority_pass_count = 0;

    Ok(WaitingFor::Priority { player })
}

/// Cancel a pending cast, reverting any side effects (e.g. untapping a source tapped for cost).
pub fn handle_cancel_cast(
    state: &mut GameState,
    pending: &PendingCast,
    events: &mut Vec<GameEvent>,
) {
    // For activated abilities (card_id == CardId(0)), the source may have been
    // tapped as part of the activation cost. Untap it on cancel.
    if pending.card_id == CardId(0) {
        if let Some(obj) = state.objects.get_mut(&pending.object_id) {
            if obj.tapped {
                obj.tapped = false;
                events.push(GameEvent::PermanentUntapped {
                    object_id: pending.object_id,
                });
            }
        }
    }
}

/// Pay the mana cost, move card to stack, push stack entry, emit events.
fn pay_and_push(
    state: &mut GameState,
    player: PlayerId,
    object_id: ObjectId,
    card_id: CardId,
    ability: ResolvedAbility,
    cost: &crate::types::mana::ManaCost,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    // Check for X in cost -- if present, return ManaPayment for player input
    if let crate::types::mana::ManaCost::Cost { shards, .. } = cost {
        if shards.contains(&ManaCostShard::X) {
            return Ok(WaitingFor::ManaPayment { player });
        }
    }

    // Auto-tap lands to fill mana pool before paying
    auto_tap_lands(state, player, cost, events);

    // Auto-pay mana cost
    {
        let player_data = state
            .players
            .iter()
            .find(|p| p.id == player)
            .expect("player exists");
        if !mana_payment::can_pay(&player_data.mana_pool, cost) {
            return Err(EngineError::ActionNotAllowed(
                "Cannot pay mana cost".to_string(),
            ));
        }
    }

    // Compute hand color demand to guide hybrid mana spending
    let hand_demand = mana_payment::compute_hand_color_demand(state, player, object_id);
    let player_data = state
        .players
        .iter_mut()
        .find(|p| p.id == player)
        .expect("player exists");
    let _ =
        mana_payment::pay_cost_with_demand(&mut player_data.mana_pool, cost, Some(&hand_demand))
            .map_err(|_| EngineError::ActionNotAllowed("Mana payment failed".to_string()))?;

    // Record commander cast before moving (need to check zone before move)
    let was_in_command_zone = state
        .objects
        .get(&object_id)
        .map(|obj| obj.zone == Zone::Command && obj.is_commander)
        .unwrap_or(false);

    // Emit targeting events before the spell moves to the stack
    emit_targeting_events(state, &ability.targets, object_id, player, events);

    // Move card from hand/command zone to stack zone
    zones::move_to_zone(state, object_id, Zone::Stack, events);

    // Track commander cast count for tax calculation
    if was_in_command_zone {
        super::commander::record_commander_cast(state, object_id);
    }

    // Push stack entry
    stack::push_to_stack(
        state,
        StackEntry {
            id: object_id,
            source_id: object_id,
            controller: player,
            kind: StackEntryKind::Spell { card_id, ability },
        },
        events,
    );

    state.priority_passes.clear();
    state.priority_pass_count = 0;

    events.push(GameEvent::SpellCast {
        card_id,
        controller: player,
    });

    state.spells_cast_this_turn = state.spells_cast_this_turn.saturating_add(1);

    Ok(WaitingFor::Priority { player })
}

/// Find and mark the first unused land producing `needed` color. Returns true if found.
fn tap_matching_land(
    available: &[ManaSourceOption],
    used_sources: &mut HashSet<ObjectId>,
    to_tap: &mut Vec<ManaSourceOption>,
    needed: ManaType,
) -> bool {
    let Some(option) = available
        .iter()
        .find(|option| option.mana_type == needed && !used_sources.contains(&option.object_id))
    else {
        return false;
    };

    used_sources.insert(option.object_id);
    to_tap.push(*option);
    true
}

/// Auto-tap untapped lands controlled by `player` to produce enough mana for `cost`.
///
/// Strategy: tap lands producing colors required by the cost first (colored shards),
/// then tap any remaining untapped lands for generic requirements.
fn auto_tap_lands(
    state: &mut GameState,
    player: PlayerId,
    cost: &crate::types::mana::ManaCost,
    events: &mut Vec<GameEvent>,
) {
    use crate::types::mana::ManaCost;

    let (shards, generic) = match cost {
        ManaCost::NoCost => return,
        ManaCost::Cost { shards, generic } if shards.is_empty() && *generic == 0 => return,
        ManaCost::Cost { shards, generic } => (shards, *generic),
    };

    // Build list of activatable mana options for untapped lands this player controls.
    let available: Vec<ManaSourceOption> = state
        .battlefield
        .iter()
        .filter_map(|&oid| {
            let obj = state.objects.get(&oid)?;
            if obj.controller != player || obj.tapped {
                return None;
            }
            if !obj
                .card_types
                .core_types
                .contains(&crate::types::card_type::CoreType::Land)
            {
                return None;
            }
            Some(mana_sources::activatable_land_mana_options(
                state, oid, player,
            ))
        })
        .flatten()
        .collect();

    let mut to_tap: Vec<ManaSourceOption> = Vec::new();
    let mut used_sources: HashSet<ObjectId> = HashSet::new();

    // Phase 1: satisfy colored and hybrid shards by tapping matching lands
    let mut deferred_generic: usize = 0;
    for shard in shards {
        use crate::game::mana_payment::{shard_to_mana_type, ShardRequirement};
        match shard_to_mana_type(*shard) {
            ShardRequirement::Single(color) | ShardRequirement::Phyrexian(color) => {
                tap_matching_land(&available, &mut used_sources, &mut to_tap, color);
            }
            ShardRequirement::Hybrid(a, b) => {
                if !tap_matching_land(&available, &mut used_sources, &mut to_tap, a) {
                    tap_matching_land(&available, &mut used_sources, &mut to_tap, b);
                }
            }
            ShardRequirement::TwoGenericHybrid(color) => {
                // Prefer 1 matching-color land over 2 generic lands
                if !tap_matching_land(&available, &mut used_sources, &mut to_tap, color) {
                    deferred_generic += 2;
                }
            }
            ShardRequirement::ColorlessHybrid(color) => {
                if !tap_matching_land(
                    &available,
                    &mut used_sources,
                    &mut to_tap,
                    ManaType::Colorless,
                ) {
                    tap_matching_land(&available, &mut used_sources, &mut to_tap, color);
                }
            }
            ShardRequirement::HybridPhyrexian(a, b) => {
                if !tap_matching_land(&available, &mut used_sources, &mut to_tap, a) {
                    tap_matching_land(&available, &mut used_sources, &mut to_tap, b);
                }
            }
            ShardRequirement::Snow | ShardRequirement::X => {
                deferred_generic += 1;
            }
        }
    }

    // Phase 2: satisfy generic cost + deferred shards with any remaining untapped lands
    let mut remaining_generic = generic as usize + deferred_generic;
    for option in &available {
        if remaining_generic == 0 {
            break;
        }
        if used_sources.insert(option.object_id) {
            to_tap.push(*option);
            remaining_generic = remaining_generic.saturating_sub(1);
        }
    }

    // Phase 3: tap and produce mana
    for option in to_tap {
        if let Some(ability_index) = option.ability_index {
            let Some(ability_def) = state
                .objects
                .get(&option.object_id)
                .and_then(|obj| obj.abilities.get(ability_index))
                .cloned()
            else {
                continue;
            };
            let _ = mana_abilities::resolve_mana_ability(
                state,
                option.object_id,
                player,
                &ability_def,
                events,
            );
            continue;
        }

        if let Some(obj) = state.objects.get_mut(&option.object_id) {
            obj.tapped = true;
        }
        events.push(GameEvent::PermanentTapped {
            object_id: option.object_id,
        });
        mana_payment::produce_mana(state, option.object_id, option.mana_type, player, events);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::card_type::CoreType;
    use crate::types::mana::{ManaColor, ManaCost, ManaType, ManaUnit};

    fn setup_game_at_main_phase() -> GameState {
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

    fn add_mana(state: &mut GameState, player: PlayerId, color: ManaType, count: usize) {
        let player_data = state.players.iter_mut().find(|p| p.id == player).unwrap();
        for _ in 0..count {
            player_data.mana_pool.add(ManaUnit {
                color,
                source_id: ObjectId(0),
                snow: false,
                restrictions: Vec::new(),
            });
        }
    }

    fn create_instant_in_hand(state: &mut GameState, player: PlayerId) -> ObjectId {
        let obj_id = create_object(
            state,
            CardId(10),
            player,
            "Lightning Bolt".to_string(),
            Zone::Hand,
        );
        {
            let obj = state.objects.get_mut(&obj_id).unwrap();
            obj.card_types.core_types.push(CoreType::Instant);
            obj.abilities.push(AbilityDefinition {
                kind: AbilityKind::Spell,
                effect: Effect::DealDamage {
                    amount: crate::types::ability::DamageAmount::Fixed(3),
                    target: crate::types::ability::TargetFilter::Any,
                },
                cost: None,
                sub_ability: None,
                duration: None,
                description: None,
                target_prompt: None,
                sorcery_speed: false,
            });
            obj.mana_cost = ManaCost::Cost {
                shards: vec![ManaCostShard::Red],
                generic: 0,
            };
        }
        obj_id
    }

    fn create_sorcery_in_hand(state: &mut GameState, player: PlayerId) -> ObjectId {
        let obj_id = create_object(
            state,
            CardId(20),
            player,
            "Divination".to_string(),
            Zone::Hand,
        );
        {
            let obj = state.objects.get_mut(&obj_id).unwrap();
            obj.card_types.core_types.push(CoreType::Sorcery);
            obj.abilities.push(AbilityDefinition {
                kind: AbilityKind::Spell,
                effect: Effect::Draw { count: 2 },
                cost: None,
                sub_ability: None,
                duration: None,
                description: None,
                target_prompt: None,
                sorcery_speed: false,
            });
            obj.mana_cost = ManaCost::Cost {
                shards: vec![ManaCostShard::Blue],
                generic: 2,
            };
        }
        obj_id
    }

    fn create_gloomlake_verge(state: &mut GameState, player: PlayerId) -> ObjectId {
        let obj_id = create_object(
            state,
            CardId(21),
            player,
            "Gloomlake Verge".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&obj_id).unwrap();
        obj.card_types.core_types.push(CoreType::Land);
        obj.abilities.push(AbilityDefinition {
            kind: AbilityKind::Activated,
            effect: Effect::Mana {
                produced: crate::types::ability::ManaProduction::Fixed {
                    colors: vec![ManaColor::Blue],
                },
            },
            cost: Some(crate::types::ability::AbilityCost::Tap),
            sub_ability: None,
            duration: None,
            description: None,
            target_prompt: None,
            sorcery_speed: false,
        });
        obj.abilities.push(AbilityDefinition {
            kind: AbilityKind::Activated,
            effect: Effect::Mana {
                produced: crate::types::ability::ManaProduction::Fixed {
                    colors: vec![ManaColor::Black],
                },
            },
            cost: Some(crate::types::ability::AbilityCost::Tap),
            sub_ability: Some(Box::new(AbilityDefinition {
                kind: AbilityKind::Activated,
                effect: Effect::Unimplemented {
                    name: "activate_only_if_controls_land_subtype_any".to_string(),
                    description: Some("Island|Swamp".to_string()),
                },
                cost: None,
                sub_ability: None,
                duration: None,
                description: None,
                target_prompt: None,
                sorcery_speed: false,
            })),
            duration: None,
            description: None,
            target_prompt: None,
            sorcery_speed: false,
        });
        obj_id
    }

    #[test]
    fn spell_cast_from_hand_moves_to_stack() {
        let mut state = setup_game_at_main_phase();
        let _obj_id = create_sorcery_in_hand(&mut state, PlayerId(0));
        add_mana(&mut state, PlayerId(0), ManaType::Blue, 1);
        add_mana(&mut state, PlayerId(0), ManaType::Colorless, 2);

        let mut events = Vec::new();
        let result = handle_cast_spell(&mut state, PlayerId(0), CardId(20), &mut events).unwrap();

        assert!(matches!(result, WaitingFor::Priority { .. }));
        assert_eq!(state.stack.len(), 1);
        assert!(state.players[0].hand.is_empty());
    }

    #[test]
    fn sorcery_speed_rejects_during_opponent_turn() {
        let mut state = setup_game_at_main_phase();
        state.active_player = PlayerId(1); // Opponent's turn
        let _obj_id = create_sorcery_in_hand(&mut state, PlayerId(0));
        add_mana(&mut state, PlayerId(0), ManaType::Blue, 3);

        let mut events = Vec::new();
        let result = handle_cast_spell(&mut state, PlayerId(0), CardId(20), &mut events);
        assert!(result.is_err());
    }

    #[test]
    fn sorcery_speed_rejects_when_stack_not_empty() {
        let mut state = setup_game_at_main_phase();
        let _obj_id = create_sorcery_in_hand(&mut state, PlayerId(0));
        add_mana(&mut state, PlayerId(0), ManaType::Blue, 3);

        // Put something on the stack
        state.stack.push(StackEntry {
            id: ObjectId(99),
            source_id: ObjectId(99),
            controller: PlayerId(1),
            kind: StackEntryKind::Spell {
                card_id: CardId(99),
                ability: ResolvedAbility::new(
                    Effect::Unimplemented {
                        name: String::new(),
                        description: None,
                    },
                    vec![],
                    ObjectId(99),
                    PlayerId(1),
                ),
            },
        });

        let mut events = Vec::new();
        let result = handle_cast_spell(&mut state, PlayerId(0), CardId(20), &mut events);
        assert!(result.is_err());
    }

    #[test]
    fn instant_can_be_cast_at_any_priority() {
        let mut state = setup_game_at_main_phase();
        state.active_player = PlayerId(1); // Not active player
        let _obj_id = create_instant_in_hand(&mut state, PlayerId(0));
        add_mana(&mut state, PlayerId(0), ManaType::Red, 1);

        // Create a target creature
        let creature_id = create_object(
            &mut state,
            CardId(50),
            PlayerId(1),
            "Goblin".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&creature_id)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        let mut events = Vec::new();
        let result = handle_cast_spell(&mut state, PlayerId(0), CardId(10), &mut events);
        // Should succeed -- instants can be cast at any priority
        assert!(result.is_ok());
    }

    #[test]
    fn cost_payment_deducts_mana() {
        let mut state = setup_game_at_main_phase();
        let _obj_id = create_sorcery_in_hand(&mut state, PlayerId(0));
        add_mana(&mut state, PlayerId(0), ManaType::Blue, 1);
        add_mana(&mut state, PlayerId(0), ManaType::Colorless, 2);

        let initial_mana = state.players[0].mana_pool.total();
        assert_eq!(initial_mana, 3);

        let mut events = Vec::new();
        handle_cast_spell(&mut state, PlayerId(0), CardId(20), &mut events).unwrap();

        assert_eq!(state.players[0].mana_pool.total(), 0);
    }

    #[test]
    fn cast_spell_insufficient_mana_fails() {
        let mut state = setup_game_at_main_phase();
        let _obj_id = create_sorcery_in_hand(&mut state, PlayerId(0));
        // No mana added

        let mut events = Vec::new();
        let result = handle_cast_spell(&mut state, PlayerId(0), CardId(20), &mut events);
        assert!(result.is_err());
    }

    #[test]
    fn auto_tap_respects_conditional_land_secondary_color() {
        let mut state = setup_game_at_main_phase();

        // Spell cost {B}
        let spell_id = create_object(
            &mut state,
            CardId(22),
            PlayerId(0),
            "Cut Down".to_string(),
            Zone::Hand,
        );
        {
            let spell = state.objects.get_mut(&spell_id).unwrap();
            spell.card_types.core_types.push(CoreType::Instant);
            spell.abilities.push(AbilityDefinition {
                kind: AbilityKind::Spell,
                effect: Effect::Draw { count: 1 },
                cost: None,
                sub_ability: None,
                duration: None,
                description: None,
                target_prompt: None,
                sorcery_speed: false,
            });
            spell.mana_cost = ManaCost::Cost {
                shards: vec![ManaCostShard::Black],
                generic: 0,
            };
        }

        create_gloomlake_verge(&mut state, PlayerId(0));
        let island = create_object(
            &mut state,
            CardId(23),
            PlayerId(0),
            "Island".to_string(),
            Zone::Battlefield,
        );
        let island_obj = state.objects.get_mut(&island).unwrap();
        island_obj.card_types.core_types.push(CoreType::Land);
        island_obj.card_types.subtypes.push("Island".to_string());

        let mut events = Vec::new();
        let result = handle_cast_spell(&mut state, PlayerId(0), CardId(22), &mut events);
        assert!(
            result.is_ok(),
            "expected conditional black mana to be available"
        );
    }

    #[test]
    fn auto_tap_blocks_conditional_land_secondary_color_without_requirement() {
        let mut state = setup_game_at_main_phase();

        // Spell cost {B}
        let spell_id = create_object(
            &mut state,
            CardId(24),
            PlayerId(0),
            "Cut Down".to_string(),
            Zone::Hand,
        );
        {
            let spell = state.objects.get_mut(&spell_id).unwrap();
            spell.card_types.core_types.push(CoreType::Instant);
            spell.abilities.push(AbilityDefinition {
                kind: AbilityKind::Spell,
                effect: Effect::Draw { count: 1 },
                cost: None,
                sub_ability: None,
                duration: None,
                description: None,
                target_prompt: None,
                sorcery_speed: false,
            });
            spell.mana_cost = ManaCost::Cost {
                shards: vec![ManaCostShard::Black],
                generic: 0,
            };
        }

        create_gloomlake_verge(&mut state, PlayerId(0));

        let mut events = Vec::new();
        let result = handle_cast_spell(&mut state, PlayerId(0), CardId(24), &mut events);
        assert!(
            result.is_err(),
            "expected cast to fail without Island/Swamp support"
        );
    }

    #[test]
    fn cancel_cast_during_target_selection_returns_to_priority() {
        use crate::game::engine::apply;
        use crate::types::actions::GameAction;

        let mut state = setup_game_at_main_phase();
        let _obj_id = create_instant_in_hand(&mut state, PlayerId(0));
        add_mana(&mut state, PlayerId(0), ManaType::Red, 1);

        // Create two creatures so targeting is ambiguous (not auto-targeted)
        for card_id_val in [50, 51] {
            let cid = create_object(
                &mut state,
                CardId(card_id_val),
                PlayerId(1),
                "Goblin".to_string(),
                Zone::Battlefield,
            );
            state
                .objects
                .get_mut(&cid)
                .unwrap()
                .card_types
                .core_types
                .push(CoreType::Creature);
        }

        // Cast the spell -> should enter TargetSelection
        let result = apply(
            &mut state,
            GameAction::CastSpell {
                card_id: CardId(10),
                targets: vec![],
            },
        )
        .unwrap();
        assert!(matches!(
            result.waiting_for,
            WaitingFor::TargetSelection { .. }
        ));
        // Card should still be in hand
        assert!(!state.players[0].hand.is_empty());

        // Cancel -> should return to Priority
        let result = apply(&mut state, GameAction::CancelCast).unwrap();
        assert!(matches!(result.waiting_for, WaitingFor::Priority { .. }));
        // Card should still be in hand after cancel
        assert!(!state.players[0].hand.is_empty());
    }

    // --- Aura casting tests ---

    use crate::types::ability::TargetFilter;
    use crate::types::keywords::Keyword;

    /// Create an Aura enchantment in hand with Enchant creature keyword.
    fn create_aura_in_hand(state: &mut GameState, player: PlayerId) -> ObjectId {
        let obj_id = create_object(
            state,
            CardId(30),
            player,
            "Pacifism".to_string(),
            Zone::Hand,
        );
        {
            let obj = state.objects.get_mut(&obj_id).unwrap();
            obj.card_types.core_types.push(CoreType::Enchantment);
            obj.card_types.subtypes.push("Aura".to_string());
            obj.keywords.push(Keyword::Enchant(TargetFilter::Typed {
                card_type: Some(crate::types::ability::TypeFilter::Creature),
                subtype: None,
                controller: None,
                properties: vec![],
            }));
            obj.mana_cost = ManaCost::Cost {
                shards: vec![ManaCostShard::White],
                generic: 0,
            };
        }
        obj_id
    }

    #[test]
    fn aura_with_multiple_targets_returns_target_selection() {
        let mut state = setup_game_at_main_phase();
        let _aura = create_aura_in_hand(&mut state, PlayerId(0));
        add_mana(&mut state, PlayerId(0), ManaType::White, 1);

        // Create two creatures as potential targets
        for card_id_val in [50, 51] {
            let cid = create_object(
                &mut state,
                CardId(card_id_val),
                PlayerId(1),
                "Goblin".to_string(),
                Zone::Battlefield,
            );
            state
                .objects
                .get_mut(&cid)
                .unwrap()
                .card_types
                .core_types
                .push(CoreType::Creature);
        }

        let mut events = Vec::new();
        let result = handle_cast_spell(&mut state, PlayerId(0), CardId(30), &mut events).unwrap();

        match result {
            WaitingFor::TargetSelection { legal_targets, .. } => {
                assert_eq!(legal_targets.len(), 2);
            }
            other => panic!("Expected TargetSelection, got {:?}", other),
        }
    }

    #[test]
    fn aura_with_single_target_auto_targets() {
        let mut state = setup_game_at_main_phase();
        let _aura = create_aura_in_hand(&mut state, PlayerId(0));
        add_mana(&mut state, PlayerId(0), ManaType::White, 1);

        // Create one creature as the only target
        let creature = create_object(
            &mut state,
            CardId(50),
            PlayerId(1),
            "Goblin".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&creature)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        let mut events = Vec::new();
        let result = handle_cast_spell(&mut state, PlayerId(0), CardId(30), &mut events).unwrap();

        // Should auto-target and go straight to Priority (on stack)
        assert!(matches!(result, WaitingFor::Priority { .. }));
        assert_eq!(state.stack.len(), 1);
        // Verify the target was recorded on the stack entry
        if let StackEntryKind::Spell { ability, .. } = &state.stack[0].kind {
            assert_eq!(
                ability.targets,
                vec![crate::types::ability::TargetRef::Object(creature)]
            );
        } else {
            panic!("Expected spell on stack");
        }
    }

    #[test]
    fn aura_with_no_legal_targets_fails() {
        let mut state = setup_game_at_main_phase();
        let _aura = create_aura_in_hand(&mut state, PlayerId(0));
        add_mana(&mut state, PlayerId(0), ManaType::White, 1);

        // No creatures on battlefield -- no legal targets for "Enchant creature"
        let mut events = Vec::new();
        let result = handle_cast_spell(&mut state, PlayerId(0), CardId(30), &mut events);
        assert!(result.is_err());
    }

    #[test]
    fn aura_targeting_respects_hexproof() {
        let mut state = setup_game_at_main_phase();
        let _aura = create_aura_in_hand(&mut state, PlayerId(0));
        add_mana(&mut state, PlayerId(0), ManaType::White, 1);

        // Create a hexproof creature controlled by opponent
        let creature = create_object(
            &mut state,
            CardId(50),
            PlayerId(1),
            "Hexproof Bear".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&creature).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.keywords.push(Keyword::Hexproof);
            obj.base_keywords.push(Keyword::Hexproof);
        }

        // Only target is hexproof opponent creature -- should fail
        let mut events = Vec::new();
        let result = handle_cast_spell(&mut state, PlayerId(0), CardId(30), &mut events);
        assert!(result.is_err());
    }

    #[test]
    fn non_aura_enchantment_does_not_trigger_aura_targeting() {
        let mut state = setup_game_at_main_phase();

        // Create a global enchantment (no Aura subtype, no Enchant keyword)
        let obj_id = create_object(
            &mut state,
            CardId(40),
            PlayerId(0),
            "Intangible Virtue".to_string(),
            Zone::Hand,
        );
        {
            let obj = state.objects.get_mut(&obj_id).unwrap();
            obj.card_types.core_types.push(CoreType::Enchantment);
            // No "Aura" subtype, no Enchant keyword
            obj.mana_cost = ManaCost::Cost {
                shards: vec![ManaCostShard::White],
                generic: 0,
            };
        }
        add_mana(&mut state, PlayerId(0), ManaType::White, 1);

        let mut events = Vec::new();
        let result = handle_cast_spell(&mut state, PlayerId(0), CardId(40), &mut events).unwrap();

        // Should resolve normally (Priority), not enter TargetSelection
        assert!(matches!(result, WaitingFor::Priority { .. }));
        assert_eq!(state.stack.len(), 1);
    }

    #[test]
    fn emit_targeting_events_opponent_object_is_crime() {
        let mut state = setup_game_at_main_phase();
        let target = create_object(
            &mut state,
            CardId(50),
            PlayerId(1),
            "Goblin".to_string(),
            Zone::Battlefield,
        );
        let mut events = Vec::new();
        emit_targeting_events(
            &state,
            &[TargetRef::Object(target)],
            ObjectId(99),
            PlayerId(0),
            &mut events,
        );
        assert!(events.iter().any(
            |e| matches!(e, GameEvent::BecomesTarget { object_id, .. } if *object_id == target)
        ));
        assert!(events.iter().any(
            |e| matches!(e, GameEvent::CrimeCommitted { player_id } if *player_id == PlayerId(0))
        ));
    }

    #[test]
    fn emit_targeting_events_own_object_no_crime() {
        let mut state = setup_game_at_main_phase();
        let target = create_object(
            &mut state,
            CardId(50),
            PlayerId(0),
            "Bear".to_string(),
            Zone::Battlefield,
        );
        let mut events = Vec::new();
        emit_targeting_events(
            &state,
            &[TargetRef::Object(target)],
            ObjectId(99),
            PlayerId(0),
            &mut events,
        );
        assert!(events
            .iter()
            .any(|e| matches!(e, GameEvent::BecomesTarget { .. })));
        assert!(!events
            .iter()
            .any(|e| matches!(e, GameEvent::CrimeCommitted { .. })));
    }

    #[test]
    fn emit_targeting_events_opponent_player_is_crime() {
        let state = setup_game_at_main_phase();
        let mut events = Vec::new();
        emit_targeting_events(
            &state,
            &[TargetRef::Player(PlayerId(1))],
            ObjectId(99),
            PlayerId(0),
            &mut events,
        );
        assert!(events.iter().any(
            |e| matches!(e, GameEvent::CrimeCommitted { player_id } if *player_id == PlayerId(0))
        ));
    }
}
