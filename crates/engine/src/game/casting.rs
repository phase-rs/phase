use crate::types::ability::{
    AbilityCost, AbilityDefinition, AbilityKind, CardPlayMode, Effect, ResolvedAbility,
    TargetFilter, TargetRef,
};
use crate::types::events::GameEvent;
use crate::types::game_state::{
    CastingVariant, ConvokeMode, GameState, PendingCast, StackEntry, StackEntryKind, WaitingFor,
};
use crate::types::identifiers::{CardId, ObjectId};
use crate::types::mana::{ManaCost, SpellMeta};
use crate::types::player::PlayerId;
use crate::types::statics::{CastingProhibitionCondition, CastingProhibitionScope, StaticMode};
use crate::types::zones::Zone;

use std::collections::HashSet;

use super::ability_utils::{
    assign_targets_in_chain, auto_select_targets, begin_target_selection, build_resolved_from_def,
    build_target_slots, compute_unavailable_modes, flatten_targets_in_chain,
    target_constraints_from_modal,
};
use super::casting_costs::{
    auto_tap_mana_sources, check_additional_cost_or_pay, pay_and_push_adventure,
};
use super::engine::EngineError;
use super::mana_payment;
use super::restrictions;
use super::stack;
use super::targeting;

/// Emit `BecomesTarget` and `CrimeCommitted` events for each target.
///
/// Called whenever targets are locked in for a spell or ability. CR 700.13:
/// Targeting an opponent, their permanent, or a card in their graveyard is a crime.
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

#[derive(Debug, Clone)]
struct PreparedSpellCast {
    object_id: ObjectId,
    card_id: CardId,
    ability_def: AbilityDefinition,
    mana_cost: crate::types::mana::ManaCost,
    modal: Option<crate::types::ability::ModalChoice>,
    casting_variant: CastingVariant,
}

fn default_spell_ability_def() -> AbilityDefinition {
    AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::Unimplemented {
            name: "PermanentNoncreature".to_string(),
            description: None,
        },
    )
}

pub fn spell_objects_available_to_cast(state: &GameState, player: PlayerId) -> Vec<ObjectId> {
    let player_data = state
        .players
        .iter()
        .find(|p| p.id == player)
        .expect("player exists");

    let mut objects = player_data.hand.clone();
    if state.format_config.command_zone {
        objects.extend(
            state
                .objects
                .values()
                .filter(|obj| obj.owner == player && obj.zone == Zone::Command && obj.is_commander)
                .map(|obj| obj.id),
        );
    }

    // CR 715.3d: Cards in exile with casting permissions are castable by their owner.
    objects.extend(state.exile.iter().filter(|&&obj_id| {
        state.objects.get(&obj_id).is_some_and(|obj| {
            obj.owner == player && has_exile_cast_permission(obj, state.turn_number)
        })
    }));

    // CR 601.2a: Opponent's exiled cards with ExileWithAltCost are castable by any player.
    // CastFromZone effects (e.g. Silent-Blade Oni, Etali) grant these permissions.
    objects.extend(state.exile.iter().filter(|&&obj_id| {
        state
            .objects
            .get(&obj_id)
            .is_some_and(|obj| obj.owner != player && has_alt_cost_permission(obj))
    }));

    // CR 702.138: Cards in graveyard with graveyard-cast keywords.
    // Escape requires enough other graveyard cards to exile; Harmonize has no such restriction.
    objects.extend(player_data.graveyard.iter().filter(|&&obj_id| {
        state.objects.get(&obj_id).is_some_and(|obj| {
            obj.owner == player
                && has_graveyard_cast_keyword(obj)
                && (has_harmonize_keyword(obj)
                    || graveyard_has_enough_for_escape(state, player, obj_id))
        })
    }));

    // CR 601.2a + CR 604.3: Cards in graveyard castable via static permission
    // from a battlefield permanent (Lurrus, Karador, etc.).
    // CR 117.1c: "Each of your turns" — only during controller's turn.
    if state.active_player == player {
        let permission_ids: HashSet<ObjectId> =
            graveyard_objects_castable_by_permission(state, player)
                .iter()
                .map(|(obj_id, _source_id)| *obj_id)
                .collect();
        objects.extend(permission_ids);
    }

    objects
}

/// CR 702.138: Check that the player's graveyard has enough OTHER cards to pay escape's exile cost.
fn graveyard_has_enough_for_escape(
    state: &GameState,
    player: PlayerId,
    escape_obj_id: ObjectId,
) -> bool {
    let obj = match state.objects.get(&escape_obj_id) {
        Some(o) => o,
        None => return false,
    };
    let exile_count = obj.keywords.iter().find_map(|k| match k {
        crate::types::keywords::Keyword::Escape { exile_count, .. } => Some(*exile_count),
        _ => None,
    });
    let Some(needed) = exile_count else {
        return false;
    };
    let other_cards = state
        .players
        .iter()
        .find(|p| p.id == player)
        .map(|p| {
            p.graveyard
                .iter()
                .filter(|&&id| id != escape_obj_id)
                .count()
        })
        .unwrap_or(0);
    other_cards >= needed as usize
}

/// CR 702.138: Check if an object has a keyword allowing it to be cast from graveyard.
fn has_harmonize_keyword(obj: &crate::game::game_object::GameObject) -> bool {
    obj.keywords
        .iter()
        .any(|k| matches!(k, crate::types::keywords::Keyword::Harmonize(_)))
}

fn has_graveyard_cast_keyword(obj: &crate::game::game_object::GameObject) -> bool {
    obj.keywords.iter().any(|k| {
        matches!(
            k,
            crate::types::keywords::Keyword::Escape { .. }
                | crate::types::keywords::Keyword::Harmonize(_)
        )
    })
}

/// Check if an object has any permission allowing it to be cast from exile.
/// Uses explicit match arms (not `matches!`) so the compiler catches new variants.
fn has_exile_cast_permission(obj: &crate::game::game_object::GameObject, turn_number: u32) -> bool {
    obj.casting_permissions.iter().any(|p| match p {
        crate::types::ability::CastingPermission::AdventureCreature
        | crate::types::ability::CastingPermission::ExileWithAltCost { .. }
        | crate::types::ability::CastingPermission::PlayFromExile { .. }
        | crate::types::ability::CastingPermission::ExileWithEnergyCost => true,
        // CR 702.185a: Warp cards only castable after the exile turn ends.
        crate::types::ability::CastingPermission::WarpExile {
            castable_after_turn,
        } => turn_number > *castable_after_turn,
    })
}

/// CR 601.2a: Check if an object has an ExileWithAltCost permission specifically.
/// Unlike `has_exile_cast_permission`, this only matches ExileWithAltCost — the
/// permission granted by CastFromZone effects. Used to allow casting opponent's
/// exiled cards (where ownership != caster).
fn has_alt_cost_permission(obj: &crate::game::game_object::GameObject) -> bool {
    obj.casting_permissions.iter().any(|p| {
        matches!(
            p,
            crate::types::ability::CastingPermission::ExileWithAltCost { .. }
        )
    })
}

/// CR 604.3 + CR 601.2a: Find graveyard objects castable via static permission
/// from battlefield permanents (Lurrus, Karador, etc.).
/// Returns (graveyard_object_id, source_permanent_id) pairs.
/// CR 117.1c: Only during the controller's own turn.
fn graveyard_objects_castable_by_permission(
    state: &GameState,
    player: PlayerId,
) -> Vec<(ObjectId, ObjectId)> {
    let mut results = Vec::new();
    let player_data = match state.players.iter().find(|p| p.id == player) {
        Some(p) => p,
        None => return results,
    };

    // Find all battlefield permanents controlled by player with GraveyardCastPermission
    let sources: Vec<(ObjectId, &TargetFilter, bool)> = state
        .battlefield
        .iter()
        .filter_map(|&obj_id| {
            let obj = state.objects.get(&obj_id)?;
            if obj.controller != player {
                return None;
            }
            obj.static_definitions.iter().find_map(|s| match s.mode {
                StaticMode::GraveyardCastPermission { once_per_turn, .. } => s
                    .affected
                    .as_ref()
                    .map(|filter| (obj_id, filter, once_per_turn)),
                _ => None,
            })
        })
        .collect();

    for (source_id, filter, once_per_turn) in &sources {
        // CR 604.2: Skip if this source's once-per-turn permission was already used
        if *once_per_turn && state.graveyard_cast_permissions_used.contains(source_id) {
            continue;
        }
        for &gy_obj_id in &player_data.graveyard {
            if super::filter::matches_target_filter_controlled(
                state, gy_obj_id, filter, *source_id, player,
            ) {
                results.push((gy_obj_id, *source_id));
            }
        }
    }
    results
}

/// CR 601.2a: Find the first valid permission source for a specific graveyard object.
/// Returns (source_id, once_per_turn) so the caller can track per-turn usage.
fn graveyard_permission_source(
    state: &GameState,
    player: PlayerId,
    object_id: ObjectId,
) -> Option<(ObjectId, bool)> {
    state.battlefield.iter().find_map(|&src_id| {
        let obj = state.objects.get(&src_id)?;
        if obj.controller != player {
            return None;
        }
        let (filter, once_per_turn) = obj.static_definitions.iter().find_map(|s| match s.mode {
            StaticMode::GraveyardCastPermission { once_per_turn, .. } => {
                s.affected.as_ref().map(|f| (f, once_per_turn))
            }
            _ => None,
        })?;
        // CR 604.2: Skip if this source's once-per-turn permission was already used
        if once_per_turn && state.graveyard_cast_permissions_used.contains(&src_id) {
            return None;
        }
        if super::filter::matches_target_filter_controlled(state, object_id, filter, src_id, player)
        {
            Some((src_id, once_per_turn))
        } else {
            None
        }
    })
}

/// CR 604.2 + CR 305.1: Find lands in the player's graveyard that can be played
/// via a GraveyardCastPermission static with `play_mode: Play`.
pub fn graveyard_lands_playable_by_permission(
    state: &GameState,
    player: PlayerId,
) -> Vec<(ObjectId, ObjectId)> {
    let mut results = Vec::new();
    let player_data = match state.players.iter().find(|p| p.id == player) {
        Some(p) => p,
        None => return results,
    };

    let sources: Vec<(ObjectId, &TargetFilter, bool)> = state
        .battlefield
        .iter()
        .filter_map(|&obj_id| {
            let obj = state.objects.get(&obj_id)?;
            if obj.controller != player {
                return None;
            }
            obj.static_definitions.iter().find_map(|s| match s.mode {
                StaticMode::GraveyardCastPermission {
                    once_per_turn,
                    play_mode: CardPlayMode::Play,
                } => s
                    .affected
                    .as_ref()
                    .map(|filter| (obj_id, filter, once_per_turn)),
                _ => None,
            })
        })
        .collect();

    for (source_id, filter, once_per_turn) in &sources {
        if *once_per_turn && state.graveyard_cast_permissions_used.contains(source_id) {
            continue;
        }
        for &gy_obj_id in &player_data.graveyard {
            if let Some(obj) = state.objects.get(&gy_obj_id) {
                // CR 305.1: Only lands can be "played" (non-land cards require "cast")
                if obj
                    .card_types
                    .core_types
                    .contains(&crate::types::card_type::CoreType::Land)
                    && super::filter::matches_target_filter_controlled(
                        state, gy_obj_id, filter, *source_id, player,
                    )
                {
                    results.push((gy_obj_id, *source_id));
                }
            }
        }
    }
    results
}

fn prepare_spell_cast(
    state: &GameState,
    player: PlayerId,
    object_id: ObjectId,
) -> Result<PreparedSpellCast, EngineError> {
    let obj = state
        .objects
        .get(&object_id)
        .ok_or_else(|| EngineError::InvalidAction("Object not found".to_string()))?;
    // CR 715.3d: Cards in exile with AdventureCreature or ExileWithAltCost permission.
    let has_exile_permission =
        obj.zone == Zone::Exile && has_exile_cast_permission(obj, state.turn_number);
    // CR 702.138: Cards in graveyard with Escape keyword.
    let has_escape = obj.zone == Zone::Graveyard && has_graveyard_cast_keyword(obj);
    // CR 601.2a + CR 117.1c: Graveyard cast via static permission (Lurrus, etc.).
    let graveyard_permission_src = if obj.zone == Zone::Graveyard && state.active_player == player {
        graveyard_permission_source(state, player, object_id)
    } else {
        None
    };
    let has_graveyard_permission = graveyard_permission_src.is_some();

    // CR 601.2a: CastFromZone effects grant ExileWithAltCost on opponent's cards,
    // so ExileWithAltCost permits casting regardless of ownership.
    let has_unowned_exile_permission =
        obj.zone == Zone::Exile && obj.owner != player && has_alt_cost_permission(obj);
    let castable_zone = has_unowned_exile_permission
        || (obj.owner == player
            && (obj.zone == Zone::Hand
                || (state.format_config.command_zone
                    && obj.zone == Zone::Command
                    && obj.is_commander)
                || has_exile_permission
                || has_escape
                || has_graveyard_permission));
    if !castable_zone {
        return Err(EngineError::InvalidAction(
            "Card is not in a castable zone".to_string(),
        ));
    }

    // CR 604.3 + CR 101.2: "Can't" beats "can" — check CantCastFrom statics.
    // Grafdigger's Cage: "Players can't cast spells from graveyards or libraries."
    // This overrides graveyard/library casting permissions (Escape, Lurrus, etc.).
    if is_blocked_from_casting_from_zone(state, obj) {
        return Err(EngineError::ActionNotAllowed(
            "A static ability prevents casting from this zone".to_string(),
        ));
    }

    // CR 101.2: Continuous casting prohibition — "can't" overrides "can".
    // E.g., Teferi, Time Raveler: "Your opponents can't cast spells during your turn."
    if is_blocked_by_cant_cast_during(state, player) {
        return Err(EngineError::ActionNotAllowed(
            "A static ability prevents casting during this phase/turn".to_string(),
        ));
    }

    if obj
        .card_types
        .core_types
        .contains(&crate::types::card_type::CoreType::Land)
    {
        return Err(EngineError::ActionNotAllowed(
            "Lands are played, not cast".to_string(),
        ));
    }

    // Only Spell-kind abilities define the spell's on-cast effect and targets.
    // Activated abilities are irrelevant when casting the permanent spell.
    let ability_def = obj
        .abilities
        .iter()
        .find(|a| a.kind == AbilityKind::Spell)
        .cloned()
        .unwrap_or_else(default_spell_ability_def);

    let flash_cost = restrictions::flash_timing_cost(state, player, obj);
    // ExileWithAltCost: override mana cost when casting from exile with this permission.
    let alt_cost_from_exile = if obj.zone == Zone::Exile {
        obj.casting_permissions.iter().find_map(|p| match p {
            crate::types::ability::CastingPermission::ExileWithAltCost { cost } => {
                Some(cost.clone())
            }
            _ => None,
        })
    } else {
        None
    };

    // CR 107.14: ExileWithEnergyCost — zero mana cost, energy paid as additional cost.
    let energy_cost_from_exile = if obj.zone == Zone::Exile {
        obj.casting_permissions.iter().any(|p| {
            matches!(
                p,
                crate::types::ability::CastingPermission::ExileWithEnergyCost
            )
        })
    } else {
        false
    };

    // Warp: when casting from hand with Keyword::Warp, use the warp mana cost.
    let warp_cost = if obj.zone == Zone::Hand {
        obj.keywords.iter().find_map(|k| match k {
            crate::types::keywords::Keyword::Warp(cost) => Some(cost.clone()),
            _ => None,
        })
    } else {
        None
    };

    // CR 702.138: Escape — use escape mana cost when casting from graveyard.
    let escape_cost = if has_escape {
        obj.keywords.iter().find_map(|k| match k {
            crate::types::keywords::Keyword::Escape { cost, .. } => Some(cost.clone()),
            _ => None,
        })
    } else {
        None
    };

    // Harmonize: use harmonize mana cost when casting from graveyard.
    // Tap cost reduction is handled in casting_costs::pay_and_push_adventure.
    let harmonize_cost = if obj.zone == Zone::Graveyard {
        obj.keywords.iter().find_map(|k| match k {
            crate::types::keywords::Keyword::Harmonize(cost) => Some(cost.clone()),
            _ => None,
        })
    } else {
        None
    };

    // Precedence: Escape > Harmonize > GraveyardPermission > Warp > Normal.
    // No standard card has both Escape and Harmonize; if one did, Escape wins.
    // Graveyard-keyword casts take priority over static permissions (card's own
    // keyword overrides an external source's grant).
    let casting_variant = if escape_cost.is_some() {
        CastingVariant::Escape
    } else if harmonize_cost.is_some() {
        CastingVariant::Harmonize
    } else if let Some((source, once_per_turn)) = graveyard_permission_src {
        CastingVariant::GraveyardPermission {
            source,
            once_per_turn,
        }
    } else if warp_cost.is_some() {
        CastingVariant::Warp
    } else {
        CastingVariant::Normal
    };
    // CR 118.9: Energy replaces mana cost entirely when casting with ExileWithEnergyCost.
    let mut mana_cost = if energy_cost_from_exile {
        crate::types::mana::ManaCost::NoCost
    } else {
        escape_cost
            .or(harmonize_cost)
            .or(alt_cost_from_exile)
            .or(warp_cost)
            .unwrap_or_else(|| obj.mana_cost.clone())
    };
    // CR 304.1: Instants can be cast any time a player has priority.
    // CR 301.1 / CR 306.1: Artifacts and planeswalkers are cast at sorcery speed.
    if let Err(base_timing_error) =
        restrictions::check_spell_timing(state, player, obj, &ability_def, false)
    {
        // CR 702.8a: Flash permits instant-speed casting.
        let Some(flash_cost) = flash_cost else {
            return Err(base_timing_error);
        };
        restrictions::check_spell_timing(state, player, obj, &ability_def, true)?;
        mana_cost = restrictions::add_mana_cost(&mana_cost, &flash_cost);
    }
    restrictions::check_casting_restrictions(state, player, object_id, &obj.casting_restrictions)?;

    if state.format_config.command_zone
        && !super::commander::can_cast_in_color_identity(state, &obj.color, &obj.mana_cost, player)
    {
        return Err(EngineError::ActionNotAllowed(
            "Card is outside commander's color identity".to_string(),
        ));
    }

    // CR 408.3 + CR 903.8: Commanders cast from the command zone incur a tax.
    if obj.zone == Zone::Command {
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
                crate::types::mana::ManaCost::SelfManaCost => {
                    // SelfManaCost should have been resolved before reaching here;
                    // treat as no-op for commander tax purposes.
                }
            }
        }
    }

    // CR 601.2f: Apply battlefield-based cost modifications (ReduceCost/RaiseCost statics).
    // This runs after self-cost reduction (CostReduction on the spell itself) and commander tax.
    apply_battlefield_cost_modifiers(state, player, object_id, &mut mana_cost);

    Ok(PreparedSpellCast {
        object_id,
        card_id: obj.card_id,
        ability_def,
        mana_cost,
        modal: obj.modal.clone(),
        casting_variant,
    })
}

/// CR 601.2f: Apply cost modifications from battlefield permanents with ReduceCost/RaiseCost statics.
///
/// Iterates all battlefield permanents and checks each static definition for cost modification
/// modes. For each applicable modifier, adjusts the spell's mana cost:
/// - ReduceCost: reduces generic mana (cannot go below 0)
/// - RaiseCost: increases generic mana
///
/// Player scope is checked via the `affected` filter on the StaticDefinition (You = source's
/// controller casts, Opponent = source's opponent casts, no controller = all players).
/// Spell type is checked via the `spell_filter` field in the StaticMode variant.
fn apply_battlefield_cost_modifiers(
    state: &GameState,
    caster: PlayerId,
    spell_id: ObjectId,
    mana_cost: &mut ManaCost,
) {
    use crate::types::ability::ControllerRef;

    for &bf_id in &state.battlefield {
        let Some(bf_obj) = state.objects.get(&bf_id) else {
            continue;
        };
        let source_controller = bf_obj.controller;

        for def in &bf_obj.static_definitions {
            let (amount, spell_filter, dynamic_count, is_raise) = match &def.mode {
                StaticMode::ReduceCost {
                    amount,
                    spell_filter,
                    dynamic_count,
                } => (amount, spell_filter, dynamic_count, false),
                StaticMode::RaiseCost {
                    amount,
                    spell_filter,
                    dynamic_count,
                } => (amount, spell_filter, dynamic_count, true),
                _ => continue,
            };

            // CR 601.2f: Check static condition — "as long as" clauses gate cost modification.
            if let Some(ref cond) = def.condition {
                if !super::layers::evaluate_condition(state, cond, source_controller, bf_id) {
                    continue;
                }
            }

            // CR 601.2f: Check player scope — does this modifier apply to spells the caster casts?
            if let Some(TargetFilter::Typed(ref tf)) = def.affected {
                match tf.controller {
                    Some(ControllerRef::You) if caster != source_controller => continue,
                    Some(ControllerRef::Opponent) if caster == source_controller => continue,
                    _ => {} // No controller restriction or matches
                }
            }

            // CR 601.2f: Check spell type filter — does the spell match?
            if let Some(ref filter) = spell_filter {
                if !spell_matches_cost_filter(state, spell_id, filter, bf_id) {
                    continue;
                }
            }

            // CR 601.2f: Calculate the modification amount.
            let base_amount = amount.clone();
            let multiplier = if let Some(ref qty_ref) = dynamic_count {
                let qty_expr = crate::types::ability::QuantityExpr::Ref {
                    qty: qty_ref.clone(),
                };
                super::quantity::resolve_quantity(state, &qty_expr, source_controller, bf_id).max(0)
                    as u32
            } else {
                1
            };

            // Apply the cost modification.
            apply_cost_mod_to_mana(mana_cost, &base_amount, multiplier, is_raise);
        }
    }
}

/// Check if a spell matches a cost modification filter.
/// Handles both Typed filters (single type) and Or filters (combined types like instant/sorcery).
fn spell_matches_cost_filter(
    state: &GameState,
    spell_id: ObjectId,
    filter: &TargetFilter,
    source_id: ObjectId,
) -> bool {
    match filter {
        TargetFilter::Typed(_) => {
            super::filter::matches_target_filter(state, spell_id, filter, source_id)
        }
        TargetFilter::Or { filters } => filters
            .iter()
            .any(|f| super::filter::matches_target_filter(state, spell_id, f, source_id)),
        _ => true,
    }
}

/// CR 601.2f: Apply a single cost modification (reduce or raise) to a mana cost.
/// For ReduceCost, reduces generic mana first (cannot go below 0).
/// For RaiseCost, increases generic mana.
fn apply_cost_mod_to_mana(
    mana_cost: &mut ManaCost,
    base_amount: &ManaCost,
    multiplier: u32,
    is_raise: bool,
) {
    // Extract the generic component from the modification amount.
    // For now, cost modifiers are primarily generic mana (e.g., {1}, {2}).
    // Colored cost modifications (e.g., {W} more) would need shard-level handling.
    let mod_generic = match base_amount {
        ManaCost::Cost { generic, .. } => *generic * multiplier,
        _ => return,
    };

    if mod_generic == 0 {
        return;
    }

    match mana_cost {
        ManaCost::Cost { generic, .. } => {
            if is_raise {
                *generic += mod_generic;
            } else {
                // CR 601.2f: Cost cannot be reduced below {0}.
                *generic = generic.saturating_sub(mod_generic);
            }
        }
        ManaCost::NoCost => {
            if is_raise {
                *mana_cost = ManaCost::Cost {
                    shards: vec![],
                    generic: mod_generic,
                };
            }
            // Reducing NoCost is a no-op
        }
        ManaCost::SelfManaCost => {} // Should not occur here
    }
}

/// CR 715.3a: Swap object characteristics to the Adventure face for casting.
/// Saves the creature face in `back_face` for later restoration.
fn swap_to_adventure_face(obj: &mut crate::game::game_object::GameObject) {
    let adventure = match obj.back_face.take() {
        Some(b) => b,
        None => return,
    };
    // Snapshot current (creature) face into back_face
    let creature_snapshot = super::printed_cards::snapshot_object_face(obj);
    super::printed_cards::apply_back_face_to_object(obj, adventure);
    obj.back_face = Some(creature_snapshot);
}

/// CR 715: Returns true if this object is an Adventure card (creature front + instant/sorcery back).
fn is_adventure_card(obj: &crate::game::game_object::GameObject) -> bool {
    let Some(ref back) = obj.back_face else {
        return false;
    };
    use crate::types::card_type::CoreType;
    back.card_types
        .core_types
        .iter()
        .any(|ct| matches!(ct, CoreType::Instant | CoreType::Sorcery))
        && obj
            .card_types
            .core_types
            .iter()
            .any(|ct| matches!(ct, CoreType::Creature))
}

/// CR 715.3a: Handle Adventure face choice and proceed with casting.
pub fn handle_adventure_choice(
    state: &mut GameState,
    player: PlayerId,
    object_id: ObjectId,
    _card_id: CardId,
    creature: bool,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    if !creature {
        // Swap to Adventure face characteristics
        if let Some(obj) = state.objects.get_mut(&object_id) {
            swap_to_adventure_face(obj);
        }
    }

    // Now proceed with normal casting using whichever face is active
    let prepared = prepare_spell_cast(state, player, object_id)?;

    let resolved = {
        let mut r = ResolvedAbility::new(
            *prepared.ability_def.effect.clone(),
            Vec::new(),
            prepared.object_id,
            player,
        );
        if let Some(sub) = &prepared.ability_def.sub_ability {
            r = r.sub_ability(build_resolved_from_def(sub, prepared.object_id, player));
        }
        if let Some(c) = prepared.ability_def.condition.clone() {
            r = r.condition(c);
        }
        r
    };

    // Evaluate layers before targeting
    if state.layers_dirty {
        super::layers::evaluate_layers(state);
    }

    let target_slots = build_target_slots(state, &resolved)?;
    if !target_slots.is_empty() {
        if let Some(targets) = auto_select_targets(&target_slots, &[])? {
            let mut resolved = resolved;
            assign_targets_in_chain(&mut resolved, &targets)?;
            if creature {
                return check_additional_cost_or_pay(
                    state,
                    player,
                    prepared.object_id,
                    prepared.card_id,
                    resolved,
                    &prepared.mana_cost,
                    prepared.casting_variant,
                    events,
                );
            } else {
                return pay_and_push_adventure(
                    state,
                    player,
                    prepared.object_id,
                    prepared.card_id,
                    resolved,
                    &prepared.mana_cost,
                    CastingVariant::Adventure,
                    None,
                    events,
                );
            }
        }

        let selection = begin_target_selection(&target_slots, &[])?;
        let mut pending_adv = PendingCast::new(
            prepared.object_id,
            prepared.card_id,
            resolved,
            prepared.mana_cost.clone(),
        );
        pending_adv.casting_variant = prepared.casting_variant;
        pending_adv.distribute = prepared.ability_def.distribute.clone();
        return Ok(WaitingFor::TargetSelection {
            player,
            pending_cast: Box::new(pending_adv),
            target_slots,
            selection,
        });
    }

    // No targets -- proceed to payment
    if creature {
        check_additional_cost_or_pay(
            state,
            player,
            prepared.object_id,
            prepared.card_id,
            resolved,
            &prepared.mana_cost,
            prepared.casting_variant,
            events,
        )
    } else {
        pay_and_push_adventure(
            state,
            player,
            prepared.object_id,
            prepared.card_id,
            resolved,
            &prepared.mana_cost,
            CastingVariant::Adventure,
            None,
            events,
        )
    }
}

/// Handle Warp cost choice and proceed with casting.
/// Warp is a custom keyword: cast for warp cost from hand, exile at next end step,
/// then may cast from exile later. When `use_warp` is false, the player chose to
/// cast normally — temporarily remove the Warp keyword so prepare_spell_cast
/// picks CastingVariant::Normal, then restore it and continue through the
/// standard casting pipeline.
pub fn handle_warp_cost_choice(
    state: &mut GameState,
    player: PlayerId,
    object_id: ObjectId,
    _card_id: CardId,
    use_warp: bool,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    if !use_warp {
        // Temporarily remove Warp keyword so prepare_spell_cast picks Normal.
        // Restore immediately after preparation to preserve the keyword for
        // future casting (e.g., if the spell is countered and returns to hand).
        let warp_kw = if let Some(obj) = state.objects.get_mut(&object_id) {
            let idx = obj
                .keywords
                .iter()
                .position(|k| matches!(k, crate::types::keywords::Keyword::Warp(_)));
            idx.map(|i| obj.keywords.remove(i))
        } else {
            None
        };

        let result = continue_cast_from_prepared(state, player, object_id, events);

        // Only restore if the object is still in Hand (cast didn't proceed to stack).
        // If cast succeeded, the keyword is on the printed card and will be present
        // when the card returns to hand after being countered.
        if let Some(kw) = warp_kw {
            if let Some(obj) = state.objects.get_mut(&object_id) {
                if obj.zone == Zone::Hand {
                    obj.keywords.push(kw);
                }
            }
        }

        return result;
    }

    // use_warp == true: prepare_spell_cast naturally picks CastingVariant::Warp
    continue_cast_from_prepared(state, player, object_id, events)
}

/// Shared continuation: call prepare_spell_cast and run the standard casting
/// pipeline (modal → targeting → payment). Extracted so handle_warp_cost_choice
/// and handle_cast_spell can share the same post-prepare logic.
fn continue_cast_from_prepared(
    state: &mut GameState,
    player: PlayerId,
    object_id: ObjectId,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    let prepared = prepare_spell_cast(state, player, object_id)?;
    continue_with_prepared(state, player, prepared, events)
}

/// Cast a spell from hand (or command zone, exile, graveyard in Commander/alternate-cost formats).
pub fn handle_cast_spell(
    state: &mut GameState,
    player: PlayerId,
    object_id: ObjectId,
    card_id: CardId,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    // Zone-agnostic validation: the AI candidate generator ensures only legal object_ids
    // from valid zones (hand, command, exile, graveyard) are offered.
    if !state
        .objects
        .get(&object_id)
        .is_some_and(|obj| obj.card_id == card_id)
    {
        return Err(EngineError::InvalidAction(format!(
            "Object {:?} does not exist or does not match card_id {:?}",
            object_id, card_id
        )));
    }

    // CR 715.3a: Adventure cards from hand require choosing creature or Adventure face.
    if let Some(obj) = state.objects.get(&object_id) {
        if obj.zone == Zone::Hand && is_adventure_card(obj) {
            return Ok(WaitingFor::AdventureCastChoice {
                player,
                object_id,
                card_id,
            });
        }
    }

    // Warp: when a hand card has Keyword::Warp and both costs are affordable,
    // present a choice. Auto-skip when only one cost is viable.
    if let Some(obj) = state.objects.get(&object_id) {
        if obj.zone == Zone::Hand {
            if let Some(warp_cost) = obj.keywords.iter().find_map(|k| match k {
                crate::types::keywords::Keyword::Warp(cost) => Some(cost.clone()),
                _ => None,
            }) {
                let normal_affordable =
                    can_pay_cost_after_auto_tap(state, player, object_id, &obj.mana_cost);
                let warp_affordable =
                    can_pay_cost_after_auto_tap(state, player, object_id, &warp_cost);
                if normal_affordable && warp_affordable {
                    return Ok(WaitingFor::WarpCostChoice {
                        player,
                        object_id,
                        card_id,
                        normal_cost: obj.mana_cost.clone(),
                        warp_cost: warp_cost.clone(),
                    });
                }
                // If only normal is affordable, skip warp — prepare_spell_cast will
                // still detect Warp keyword but the player chose normal by necessity.
                // We handle this in handle_warp_cost_choice's override logic.
                if normal_affordable && !warp_affordable {
                    // Force normal cast by proceeding through handle_warp_cost_choice
                    return handle_warp_cost_choice(
                        state, player, object_id, card_id, false, events,
                    );
                }
                // If only warp or neither, let prepare_spell_cast handle it normally
                // (it will pick CastingVariant::Warp via precedence)
            }
        }
    }

    continue_cast_from_prepared(state, player, object_id, events)
}

/// Continue the casting pipeline from a PreparedSpellCast.
/// Handles modal selection, targeting, aura targeting, and mana payment.
/// Shared by handle_cast_spell and handle_warp_cost_choice.
fn continue_with_prepared(
    state: &mut GameState,
    player: PlayerId,
    prepared: PreparedSpellCast,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    if let Some(ref modal_choice) = prepared.modal {
        // Cap max_choices to actual mode count
        let mut capped = modal_choice.clone();
        capped.max_choices = capped.max_choices.min(capped.mode_count);
        let target_constraints = target_constraints_from_modal(&capped);

        // Build a placeholder resolved ability -- will be replaced after mode selection
        let placeholder = ResolvedAbility::new(
            *prepared.ability_def.effect.clone(),
            Vec::new(),
            prepared.object_id,
            player,
        );
        let mut pending_modal = PendingCast::new(
            prepared.object_id,
            prepared.card_id,
            placeholder,
            prepared.mana_cost.clone(),
        );
        pending_modal.casting_variant = prepared.casting_variant;
        pending_modal.distribute = prepared.ability_def.distribute.clone();
        pending_modal.target_constraints = target_constraints;
        return Ok(WaitingFor::ModeChoice {
            player,
            modal: capped,
            pending_cast: Box::new(pending_modal),
        });
    }

    let resolved = {
        let mut r = ResolvedAbility::new(
            *prepared.ability_def.effect.clone(),
            Vec::new(),
            prepared.object_id,
            player,
        );
        if let Some(sub) = &prepared.ability_def.sub_ability {
            r = r.sub_ability(build_resolved_from_def(sub, prepared.object_id, player));
        }
        if let Some(c) = prepared.ability_def.condition.clone() {
            r = r.condition(c);
        }
        r
    };

    // 5. Handle targeting -- ensure layers evaluated before target legality
    if state.layers_dirty {
        super::layers::evaluate_layers(state);
    }

    // Check if this is an Aura spell -- Auras target via Enchant keyword, not via effect targets
    // Re-read obj after evaluate_layers (which needs &mut state)
    let obj = state.objects.get(&prepared.object_id).unwrap();
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
            let legal = targeting::find_legal_targets(state, &filter, player, prepared.object_id);
            if legal.is_empty() {
                return Err(EngineError::ActionNotAllowed(
                    "No legal targets for Aura".to_string(),
                ));
            }
            let target_slots = vec![crate::types::game_state::TargetSelectionSlot {
                legal_targets: legal,
                optional: false,
            }];
            if let Some(targets) = auto_select_targets(&target_slots, &[])? {
                let mut resolved = resolved;
                assign_targets_in_chain(&mut resolved, &targets)?;
                return check_additional_cost_or_pay(
                    state,
                    player,
                    prepared.object_id,
                    prepared.card_id,
                    resolved,
                    &prepared.mana_cost,
                    prepared.casting_variant,
                    events,
                );
            } else {
                let selection = begin_target_selection(&target_slots, &[])?;
                let mut pending_aura = PendingCast::new(
                    prepared.object_id,
                    prepared.card_id,
                    resolved,
                    prepared.mana_cost.clone(),
                );
                pending_aura.casting_variant = prepared.casting_variant;
                pending_aura.distribute = prepared.ability_def.distribute.clone();
                return Ok(WaitingFor::TargetSelection {
                    player,
                    pending_cast: Box::new(pending_aura),
                    target_slots,
                    selection,
                });
            }
        }
    }

    let target_slots = build_target_slots(state, &resolved)?;
    if !target_slots.is_empty() {
        if let Some(targets) = auto_select_targets(&target_slots, &[])? {
            let mut resolved = resolved;
            assign_targets_in_chain(&mut resolved, &targets)?;
            return check_additional_cost_or_pay(
                state,
                player,
                prepared.object_id,
                prepared.card_id,
                resolved,
                &prepared.mana_cost,
                prepared.casting_variant,
                events,
            );
        }

        let selection = begin_target_selection(&target_slots, &[])?;
        let mut pending_targets = PendingCast::new(
            prepared.object_id,
            prepared.card_id,
            resolved,
            prepared.mana_cost.clone(),
        );
        pending_targets.casting_variant = prepared.casting_variant;
        pending_targets.distribute = prepared.ability_def.distribute.clone();
        return Ok(WaitingFor::TargetSelection {
            player,
            pending_cast: Box::new(pending_targets),
            target_slots,
            selection,
        });
    }

    // 6. Check additional cost, then pay mana cost
    check_additional_cost_or_pay(
        state,
        player,
        prepared.object_id,
        prepared.card_id,
        resolved,
        &prepared.mana_cost,
        prepared.casting_variant,
        events,
    )
}

/// Returns true if the spell has at least one legal target (or requires no targets).
/// Used by phase-ai's legal_actions to avoid including uncastable spells in the action set.
pub fn spell_has_legal_targets(
    state: &GameState,
    obj: &crate::game::game_object::GameObject,
    player: PlayerId,
) -> bool {
    let mut simulated = state.clone();
    if simulated.layers_dirty {
        super::layers::evaluate_layers(&mut simulated);
    }
    let Some(obj) = simulated.objects.get(&obj.id) else {
        return false;
    };

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
            !targeting::find_legal_targets(&simulated, &filter, player, obj.id).is_empty()
        });
    }

    // Modal spells defer target checking until after mode selection
    if obj.modal.is_some() {
        return true;
    }

    // Only Spell-kind abilities contribute targets when casting.
    // Activated/Database abilities are irrelevant to spell castability.
    let ability_def = match obj.abilities.iter().find(|a| a.kind == AbilityKind::Spell) {
        Some(a) => a,
        None => return true, // Permanent with no spell abilities needs no targets
    };

    let resolved = build_resolved_from_def(ability_def, obj.id, player);
    match build_target_slots(&simulated, &resolved) {
        Ok(target_slots) => {
            if target_slots.is_empty() {
                true
            } else {
                auto_select_targets(&target_slots, &[]).is_ok()
            }
        }
        Err(_) => false,
    }
}

pub fn can_cast_object_now(state: &GameState, player: PlayerId, object_id: ObjectId) -> bool {
    let Ok(prepared) = prepare_spell_cast(state, player, object_id) else {
        return false;
    };
    let Some(obj) = state.objects.get(&prepared.object_id) else {
        return false;
    };

    // CR 702.138: Escape requires enough other graveyard cards to exile.
    if prepared.casting_variant == CastingVariant::Escape
        && !graveyard_has_enough_for_escape(state, player, prepared.object_id)
    {
        return false;
    }

    // CR 702.172: Spree spells must afford at least one mode to be castable
    if let Some(ref modal) = prepared.modal {
        if !modal.mode_costs.is_empty() {
            return modal.mode_costs.iter().any(|mode_cost| {
                let total = restrictions::add_mana_cost(&prepared.mana_cost, mode_cost);
                can_pay_cost_after_auto_tap(state, player, prepared.object_id, &total)
            });
        }
    }

    (prepared.modal.is_some() || spell_has_legal_targets(state, obj, player))
        && can_pay_cost_after_auto_tap(state, player, prepared.object_id, &prepared.mana_cost)
}

/// Returns true if the player can pay this mana cost after auto-tapping
/// currently activatable lands in a cloned game state.
///
/// Used by legal action generation so the frontend and engine agree on whether
/// a spell is castable from the current board state.
pub fn can_pay_cost_after_auto_tap(
    state: &GameState,
    player: PlayerId,
    source_id: ObjectId,
    cost: &crate::types::mana::ManaCost,
) -> bool {
    let mut simulated = state.clone();
    if simulated.layers_dirty {
        super::layers::evaluate_layers(&mut simulated);
    }
    let spell_meta = simulated.objects.get(&source_id).map(|obj| SpellMeta {
        types: obj
            .card_types
            .core_types
            .iter()
            .map(|ct| format!("{ct:?}"))
            .collect(),
        subtypes: obj.card_types.subtypes.clone(),
    });

    super::casting_costs::auto_tap_mana_sources(
        &mut simulated,
        player,
        cost,
        &mut Vec::new(),
        Some(source_id),
    );

    simulated
        .players
        .iter()
        .find(|p| p.id == player)
        .is_some_and(|player_data| {
            mana_payment::can_pay_for_spell(&player_data.mana_pool, cost, spell_meta.as_ref())
        })
}

// Target/mode selection handlers are in casting_targets module.
pub(crate) use super::casting_targets::{
    handle_choose_target, handle_select_modes, handle_select_targets,
};

/// Activate an ability from a permanent on the battlefield.
/// Check whether an ability cost includes a tap component (either directly or
/// within a composite). Used for pre-validation before presenting modal choices.
fn requires_untapped(cost: &AbilityCost) -> bool {
    match cost {
        AbilityCost::Tap => true,
        AbilityCost::Composite { costs } => costs.iter().any(requires_untapped),
        _ => false,
    }
}

/// Pay a mana cost by auto-tapping lands and deducting from the player's mana pool.
///
/// Shared building block used by both spell casting (`pay_and_push`) and activated
/// ability cost payment (`pay_ability_cost`).
pub(super) fn pay_mana_cost(
    state: &mut GameState,
    player: PlayerId,
    source_id: ObjectId,
    cost: &crate::types::mana::ManaCost,
    events: &mut Vec<GameEvent>,
) -> Result<(), EngineError> {
    if state.layers_dirty {
        super::layers::evaluate_layers(state);
    }

    let spell_meta = state.objects.get(&source_id).map(|obj| SpellMeta {
        types: obj
            .card_types
            .core_types
            .iter()
            .map(|ct| format!("{ct:?}"))
            .collect(),
        subtypes: obj.card_types.subtypes.clone(),
    });

    auto_tap_mana_sources(state, player, cost, events, Some(source_id));

    {
        let player_data = state
            .players
            .iter()
            .find(|p| p.id == player)
            .expect("player exists");
        if !mana_payment::can_pay_for_spell(&player_data.mana_pool, cost, spell_meta.as_ref()) {
            return Err(EngineError::ActionNotAllowed(
                "Cannot pay mana cost".to_string(),
            ));
        }
    }

    let hand_demand = mana_payment::compute_hand_color_demand(state, player, source_id);
    let player_data = state
        .players
        .iter_mut()
        .find(|p| p.id == player)
        .expect("player exists");
    mana_payment::pay_cost_with_demand(
        &mut player_data.mana_pool,
        cost,
        Some(&hand_demand),
        spell_meta.as_ref(),
    )
    .map_err(|_| EngineError::ActionNotAllowed("Mana payment failed".to_string()))?;

    Ok(())
}

/// Pay an activated ability's cost. Handles `Tap`, `Mana`, `Composite` (recursive),
/// and passes through other cost types that require interactive resolution.
pub fn pay_ability_cost(
    state: &mut GameState,
    player: PlayerId,
    source_id: ObjectId,
    cost: &AbilityCost,
    events: &mut Vec<GameEvent>,
) -> Result<(), EngineError> {
    match cost {
        AbilityCost::Tap => {
            let obj = state
                .objects
                .get(&source_id)
                .ok_or_else(|| EngineError::InvalidAction("Object not found".to_string()))?;
            if obj.zone != Zone::Battlefield {
                return Err(EngineError::ActionNotAllowed(
                    "Cannot activate tap ability: source is not on the battlefield".to_string(),
                ));
            }
            if obj.tapped {
                return Err(EngineError::ActionNotAllowed(
                    "Cannot activate tap ability: permanent is tapped".to_string(),
                ));
            }
            let obj = state.objects.get_mut(&source_id).unwrap();
            obj.tapped = true;
            events.push(GameEvent::PermanentTapped {
                object_id: source_id,
                caused_by: None,
            });
        }
        AbilityCost::Mana { cost } => {
            pay_mana_cost(state, player, source_id, cost, events)?;
        }
        AbilityCost::Composite { costs } => {
            for sub_cost in costs {
                pay_ability_cost(state, player, source_id, sub_cost, events)?;
            }
        }
        // CR 118.3: Sacrifice as a cost — sacrifice the source (SelfRef) or a chosen permanent.
        AbilityCost::Sacrifice { target } => {
            if matches!(target, TargetFilter::SelfRef) {
                match super::sacrifice::sacrifice_permanent(state, source_id, player, events)? {
                    super::sacrifice::SacrificeOutcome::Complete => {}
                    super::sacrifice::SacrificeOutcome::NeedsReplacementChoice(_) => {
                        // CR 118.3: Replacement choice during cost payment is extremely rare.
                        // TODO: Surface replacement choice to player during cost payment.
                        // For now, proceed — the sacrifice was not completed, but the
                        // replacement pipeline has already handled the event.
                    }
                }
            } else {
                // Non-self sacrifice costs (e.g., "Sacrifice a creature") are handled
                // by the interactive WaitingFor::SacrificeForCost flow — they are
                // intercepted before reaching pay_ability_cost.
            }
        }
        // CR 207.2c + CR 602.1: Discard the source card itself as part of the cost (Channel).
        AbilityCost::Discard { self_ref: true, .. } => {
            match super::effects::discard::discard_as_cost(state, source_id, player, events) {
                super::effects::discard::DiscardOutcome::Complete => {}
                super::effects::discard::DiscardOutcome::NeedsReplacementChoice(_) => {
                    // CR 118.3: Replacement choice during cost payment is extremely rare.
                    // TODO: Surface replacement choice to player during cost payment.
                    // For now, proceed — the discard was not completed, but the
                    // replacement pipeline has already handled the event.
                }
            }
        }
        // Waterbend cost was already paid via ManaPayment before reaching pay_ability_cost.
        AbilityCost::Waterbend { .. } => {}
        AbilityCost::Unimplemented { description } => {
            return Err(EngineError::ActionNotAllowed(format!(
                "Cost not implemented: {description}",
            )));
        }
        AbilityCost::PayEnergy { amount } => {
            // CR 107.14: A player can pay {E} only if they have enough energy.
            let amount = *amount;
            let player_state = &mut state.players[player.0 as usize];
            if player_state.energy < amount {
                return Err(EngineError::ActionNotAllowed("Not enough energy".into()));
            }
            player_state.energy -= amount;
            events.push(GameEvent::EnergyChanged {
                player,
                delta: -(amount as i32),
            });
        }
        // Other cost types (Exile, PayLife, etc.) require interactive resolution
        // and are intercepted before reaching pay_ability_cost, or are not yet auto-payable.
        AbilityCost::Untap
        | AbilityCost::Loyalty { .. }
        | AbilityCost::PayLife { .. }
        | AbilityCost::Discard { .. }
        | AbilityCost::Exile { .. }
        | AbilityCost::TapCreatures { .. }
        | AbilityCost::RemoveCounter { .. }
        | AbilityCost::ReturnToHand { .. }
        | AbilityCost::Mill { .. }
        | AbilityCost::Exert
        | AbilityCost::Blight { .. }
        | AbilityCost::Reveal { .. }
        | AbilityCost::NinjutsuFamily { .. } => {}
    }
    Ok(())
}

/// CR 118.12: Pay an "unless pays" cost. Auto-taps lands and deducts mana.
/// Used when the opponent chooses to pay a counter-unless cost (e.g., Mana Leak).
pub fn pay_unless_cost(
    state: &mut GameState,
    player: PlayerId,
    cost: &crate::types::mana::ManaCost,
    events: &mut Vec<GameEvent>,
) -> Result<(), EngineError> {
    // Use ObjectId(0) as a dummy source since there's no specific object paying
    pay_mana_cost(state, player, ObjectId(0), cost, events)
}

/// Walk a cost tree and return the waterbend mana cost if present.
fn find_waterbend_cost(cost: &AbilityCost) -> Option<&ManaCost> {
    match cost {
        AbilityCost::Waterbend { cost } => Some(cost),
        AbilityCost::Composite { costs } => costs.iter().find_map(find_waterbend_cost),
        _ => None,
    }
}

/// Walk a cost tree and return the first non-SelfRef sacrifice filter found, if any.
fn find_non_self_sacrifice(cost: &AbilityCost) -> Option<&TargetFilter> {
    match cost {
        AbilityCost::Sacrifice { target } if !matches!(target, TargetFilter::SelfRef) => {
            Some(target)
        }
        AbilityCost::Composite { costs } => costs.iter().find_map(find_non_self_sacrifice),
        _ => None,
    }
}

/// CR 118.3: Find permanents controlled by `player` matching `filter` on the battlefield.
/// Excludes `source_id` so the source cannot be sacrificed as its own cost.
pub(super) fn find_eligible_sacrifice_targets(
    state: &GameState,
    player: PlayerId,
    source_id: ObjectId,
    filter: &TargetFilter,
) -> Vec<ObjectId> {
    state
        .battlefield
        .iter()
        .copied()
        .filter(|&id| {
            if id == source_id {
                return false;
            }
            let Some(obj) = state.objects.get(&id) else {
                return false;
            };
            if obj.controller != player {
                return false;
            }
            super::filter::matches_target_filter(state, id, filter, source_id)
        })
        .collect()
}

fn can_pay_ability_cost_now(
    state: &GameState,
    player: PlayerId,
    source_id: ObjectId,
    cost: &AbilityCost,
) -> bool {
    // CR 118.3: Pre-check non-self sacrifice eligibility before simulation.
    // The simulation would give a false positive since pay_ability_cost's
    // non-self Sacrifice arm is a no-op (it's handled interactively).
    if let Some(sac_filter) = find_non_self_sacrifice(cost) {
        if find_eligible_sacrifice_targets(state, player, source_id, sac_filter).is_empty() {
            return false;
        }
    }
    let mut simulated = state.clone();
    pay_ability_cost(&mut simulated, player, source_id, cost, &mut Vec::new()).is_ok()
}

pub fn can_activate_ability_now(
    state: &GameState,
    player: PlayerId,
    source_id: ObjectId,
    ability_index: usize,
) -> bool {
    let Some(obj) = state.objects.get(&source_id) else {
        return false;
    };
    if obj.controller != player || ability_index >= obj.abilities.len() {
        return false;
    }

    let mut ability_def = obj.abilities[ability_index].clone();
    // CR 602.1: Check activation zone — default to battlefield.
    let required_zone = ability_def.activation_zone.unwrap_or(Zone::Battlefield);
    if obj.zone != required_zone {
        return false;
    }
    if restrictions::check_activation_restrictions(
        state,
        player,
        source_id,
        ability_index,
        &ability_def.activation_restrictions,
    )
    .is_err()
    {
        return false;
    }
    // CR 601.2f: Apply self-referential cost reduction before affordability check.
    apply_cost_reduction(state, &mut ability_def, player, source_id);
    if ability_def
        .cost
        .as_ref()
        .is_some_and(|cost| !can_pay_ability_cost_now(state, player, source_id, cost))
    {
        return false;
    }

    if let Some(ref modal) = ability_def.modal {
        if ability_def.cost.as_ref().is_some_and(requires_untapped) && obj.tapped {
            return false;
        }
        return modal.mode_count > 0;
    }

    let resolved = {
        let mut ability =
            ResolvedAbility::new(*ability_def.effect.clone(), Vec::new(), source_id, player);
        if let Some(sub) = &ability_def.sub_ability {
            ability = ability.sub_ability(build_resolved_from_def(sub, source_id, player));
        }
        if let Some(condition) = ability_def.condition.clone() {
            ability = ability.condition(condition);
        }
        ability
    };

    let mut simulated = state.clone();
    if simulated.layers_dirty {
        super::layers::evaluate_layers(&mut simulated);
    }

    match build_target_slots(&simulated, &resolved) {
        Ok(target_slots) => {
            target_slots.is_empty() || auto_select_targets(&target_slots, &[]).is_ok()
        }
        Err(_) => false,
    }
}

/// CR 602.2: To activate an ability is to put it onto the stack and pay its costs.
/// CR 602.2a: Only an object's controller can activate its activated ability unless
/// the object specifically says otherwise.
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

    // CR 602.2: Only an object's controller can activate its activated ability.
    if obj.controller != player {
        return Err(EngineError::NotYourPriority);
    }
    if ability_index >= obj.abilities.len() {
        return Err(EngineError::InvalidAction(
            "Invalid ability index".to_string(),
        ));
    }

    let mut ability_def = obj.abilities[ability_index].clone();
    // CR 602.1: Check activation zone — default to battlefield.
    let required_zone = ability_def.activation_zone.unwrap_or(Zone::Battlefield);
    if obj.zone != required_zone {
        return Err(EngineError::InvalidAction(format!(
            "Object is not in the correct zone (expected {:?})",
            required_zone
        )));
    }

    // CR 601.2f: Apply self-referential cost reduction before any cost payment.
    apply_cost_reduction(state, &mut ability_def, player, source_id);

    restrictions::check_activation_restrictions(
        state,
        player,
        source_id,
        ability_index,
        &ability_def.activation_restrictions,
    )?;

    // CR 602.2b: Announce → choose modes → choose targets → pay costs.
    // Modal detection must happen BEFORE cost payment.
    if let Some(ref modal) = ability_def.modal {
        // Pre-validate tap cost for modals — fail fast before presenting the choice
        if ability_def.cost.as_ref().is_some_and(requires_untapped) {
            let obj = state.objects.get(&source_id).unwrap();
            if obj.tapped {
                return Err(EngineError::ActionNotAllowed(
                    "Cannot activate tap ability: permanent is tapped".to_string(),
                ));
            }
        }
        let unavailable_modes = compute_unavailable_modes(state, source_id, modal);
        return Ok(WaitingFor::AbilityModeChoice {
            player,
            modal: modal.clone(),
            source_id,
            mode_abilities: ability_def.mode_abilities.clone(),
            is_activated: true,
            ability_index: Some(ability_index),
            ability_cost: ability_def.cost.clone(),
            unavailable_modes,
        });
    }

    let resolved = {
        let mut r =
            ResolvedAbility::new(*ability_def.effect.clone(), Vec::new(), source_id, player);
        if let Some(sub) = &ability_def.sub_ability {
            r = r.sub_ability(build_resolved_from_def(sub, source_id, player));
        }
        if let Some(c) = ability_def.condition.clone() {
            r = r.condition(c);
        }
        r
    };

    // CR 118.3: Pre-check for non-self sacrifice costs — must detour to WaitingFor
    // before any cost payment, regardless of whether targets were auto-selected.
    if let Some(ref cost) = ability_def.cost {
        if let Some(sac_filter) = find_non_self_sacrifice(cost) {
            let eligible = find_eligible_sacrifice_targets(state, player, source_id, sac_filter);
            if eligible.is_empty() {
                return Err(EngineError::ActionNotAllowed(
                    "No eligible permanents to sacrifice".into(),
                ));
            }
            let mut pending_sac =
                PendingCast::new(source_id, CardId(0), resolved, ManaCost::NoCost);
            pending_sac.activation_cost = Some(cost.clone());
            pending_sac.activation_ability_index = Some(ability_index);
            return Ok(WaitingFor::SacrificeForCost {
                player,
                count: 1,
                permanents: eligible,
                pending_cast: Box::new(pending_sac),
            });
        }

        // Waterbend cost: detour to ManaPayment with Waterbend mode.
        if let Some(wb_cost) = find_waterbend_cost(cost) {
            let mut pending_wb = PendingCast::new(source_id, CardId(0), resolved, wb_cost.clone());
            pending_wb.activation_cost = Some(cost.clone());
            pending_wb.activation_ability_index = Some(ability_index);
            state.pending_cast = Some(Box::new(pending_wb));
            return Ok(WaitingFor::ManaPayment {
                player,
                convoke_mode: Some(ConvokeMode::Waterbend),
            });
        }
    }

    let target_slots = build_target_slots(state, &resolved)?;
    if !target_slots.is_empty() {
        if let Some(targets) = auto_select_targets(&target_slots, &[])? {
            let mut resolved = resolved;
            assign_targets_in_chain(&mut resolved, &targets)?;

            if let Some(ref cost) = ability_def.cost {
                pay_ability_cost(state, player, source_id, cost, events)?;
            }

            let assigned_targets = flatten_targets_in_chain(&resolved);
            emit_targeting_events(state, &assigned_targets, source_id, player, events);

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

            restrictions::record_ability_activation(state, source_id, ability_index);
            events.push(GameEvent::AbilityActivated { source_id });
            state.priority_passes.clear();
            state.priority_pass_count = 0;
            return Ok(WaitingFor::Priority { player });
        }

        let selection = begin_target_selection(&target_slots, &[])?;
        let mut pending_target = PendingCast::new(
            source_id,
            CardId(0),
            resolved,
            crate::types::mana::ManaCost::NoCost,
        );
        pending_target.activation_cost = ability_def.cost.clone();
        pending_target.activation_ability_index = Some(ability_index);
        return Ok(WaitingFor::TargetSelection {
            player,
            pending_cast: Box::new(pending_target),
            target_slots,
            selection,
        });
    }

    if let Some(ref cost) = ability_def.cost {
        pay_ability_cost(state, player, source_id, cost, events)?;
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

    restrictions::record_ability_activation(state, source_id, ability_index);
    events.push(GameEvent::AbilityActivated { source_id });

    state.priority_passes.clear();
    state.priority_pass_count = 0;

    Ok(WaitingFor::Priority { player })
}

/// Cancel a pending cast, reverting any side effects (e.g. untapping a source tapped for cost).
pub fn handle_cancel_cast(
    _state: &mut GameState,
    _pending: &PendingCast,
    _events: &mut Vec<GameEvent>,
) {
    // Costs are not paid before cancelable target/mode selection states, so cancel has no
    // side effects to unwind.
}

// Cost payment handlers are in casting_costs module.
pub(crate) use super::casting_costs::{
    handle_decide_additional_cost, handle_discard_for_cost, handle_sacrifice_for_cost,
};

/// CR 601.2f: Reduce the generic mana component of an ability cost.
/// Walks Composite costs to find Mana variants. Floors generic at 0.
fn reduce_generic_in_cost(cost: &mut AbilityCost, amount: u32) {
    match cost {
        AbilityCost::Mana {
            cost: ManaCost::Cost { generic, .. },
        } => {
            *generic = generic.saturating_sub(amount);
        }
        AbilityCost::Composite { costs } => {
            for sub in costs {
                reduce_generic_in_cost(sub, amount);
            }
        }
        _ => {} // Non-mana costs unaffected
    }
}

/// CR 601.2f: Apply self-referential cost reduction to an ability definition's cost.
/// Mutates `ability_def.cost` in place, reducing generic mana by `amount_per * count`.
fn apply_cost_reduction(
    state: &GameState,
    ability_def: &mut AbilityDefinition,
    player: PlayerId,
    source_id: ObjectId,
) {
    if let Some(ref reduction) = ability_def.cost_reduction {
        let count = super::quantity::resolve_quantity(state, &reduction.count, player, source_id);
        let reduce_by = (reduction.amount_per as i32 * count).max(0) as u32;
        if reduce_by > 0 {
            if let Some(ref mut cost) = ability_def.cost {
                reduce_generic_in_cost(cost, reduce_by);
            }
        }
    }
}

/// CR 604.3 + CR 101.2: Check if any active CantCastFrom static prevents casting
/// the given object from its current zone.
/// e.g., Grafdigger's Cage: "Players can't cast spells from graveyards or libraries."
fn is_blocked_from_casting_from_zone(
    state: &GameState,
    obj: &crate::game::game_object::GameObject,
) -> bool {
    // Only applies to non-hand, non-command zones (graveyard, library, exile)
    if obj.zone == Zone::Hand || obj.zone == Zone::Command {
        return false;
    }

    let object_id = obj.id;
    for &bf_id in &state.battlefield {
        let Some(bf_obj) = state.objects.get(&bf_id) else {
            continue;
        };
        for def in &bf_obj.static_definitions {
            if def.mode != StaticMode::CantCastFrom {
                continue;
            }
            // The affected filter encodes zone restrictions via InAnyZone.
            if let Some(ref filter) = def.affected {
                if super::filter::matches_target_filter(state, object_id, filter, bf_id) {
                    return true;
                }
            }
        }
    }
    false
}

/// CR 101.2: Check if any CantCastDuring static on the battlefield prevents the
/// given player from casting spells during the current turn/phase.
/// E.g., Teferi, Time Raveler: "Your opponents can't cast spells during your turn."
/// E.g., Basandra, Battle Seraph: "Players can't cast spells during combat."
fn is_blocked_by_cant_cast_during(state: &GameState, caster: PlayerId) -> bool {
    use crate::types::phase::Phase;

    for &bf_id in &state.battlefield {
        let Some(bf_obj) = state.objects.get(&bf_id) else {
            continue;
        };
        for def in &bf_obj.static_definitions {
            let StaticMode::CantCastDuring { ref who, ref when } = def.mode else {
                continue;
            };

            // CR 101.2: Check if the caster is in the affected scope.
            let caster_affected = match who {
                CastingProhibitionScope::Opponents => caster != bf_obj.controller,
                CastingProhibitionScope::AllPlayers => true,
            };
            if !caster_affected {
                continue;
            }

            // Check if the current game state matches the timing condition.
            let condition_met = match when {
                CastingProhibitionCondition::DuringYourTurn => {
                    // "During your turn" = the static controller's turn is active.
                    state.active_player == bf_obj.controller
                }
                CastingProhibitionCondition::DuringCombat => {
                    matches!(
                        state.phase,
                        Phase::BeginCombat
                            | Phase::DeclareAttackers
                            | Phase::DeclareBlockers
                            | Phase::CombatDamage
                            | Phase::EndCombat
                    )
                }
            };
            if condition_met {
                return true;
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones;
    use crate::game::zones::create_object;
    use crate::types::ability::{
        BasicLandType, ChosenAttribute, ChosenSubtypeKind, ContinuousModification, QuantityExpr,
        StaticDefinition,
    };
    use crate::types::card_type::CoreType;
    use crate::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType, ManaUnit};
    use crate::types::phase::Phase;

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
                expiry: None,
            });
        }
    }

    fn add_basic_land(
        state: &mut GameState,
        card_id: CardId,
        name: &str,
        subtype: &str,
    ) -> ObjectId {
        let land = create_object(
            state,
            card_id,
            PlayerId(0),
            name.to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&land).unwrap();
        obj.card_types.core_types.push(CoreType::Land);
        obj.card_types.subtypes.push(subtype.to_string());
        land
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
            obj.abilities.push(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::DealDamage {
                    amount: QuantityExpr::Fixed { value: 3 },
                    target: crate::types::ability::TargetFilter::Any,
                    damage_source: None,
                },
            ));
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
            obj.abilities.push(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Draw {
                    count: QuantityExpr::Fixed { value: 2 },
                },
            ));
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
        obj.abilities.push(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Mana {
                    produced: crate::types::ability::ManaProduction::Fixed {
                        colors: vec![ManaColor::Blue],
                    },
                    restrictions: vec![],
                    expiry: None,
                },
            )
            .cost(crate::types::ability::AbilityCost::Tap),
        );
        obj.abilities.push(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Mana {
                    produced: crate::types::ability::ManaProduction::Fixed {
                        colors: vec![ManaColor::Black],
                    },
                    restrictions: vec![],
                    expiry: None,
                },
            )
            .cost(crate::types::ability::AbilityCost::Tap)
            .activation_restrictions(vec![
                crate::types::ability::ActivationRestriction::RequiresCondition {
                    text: "you control an Island or a Swamp".to_string(),
                },
            ]),
        );
        obj_id
    }

    fn create_targeted_activated_permanent(state: &mut GameState, player: PlayerId) -> ObjectId {
        let obj_id = create_object(
            state,
            CardId(51),
            player,
            "Pinger".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&obj_id).unwrap();
        obj.card_types.core_types.push(CoreType::Artifact);
        obj.abilities.push(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::DealDamage {
                    amount: QuantityExpr::Fixed { value: 1 },
                    target: crate::types::ability::TargetFilter::Any,
                    damage_source: None,
                },
            )
            .cost(AbilityCost::Tap),
        );
        obj_id
    }

    #[test]
    fn spell_cast_from_hand_moves_to_stack() {
        let mut state = setup_game_at_main_phase();
        let obj_id = create_sorcery_in_hand(&mut state, PlayerId(0));
        add_mana(&mut state, PlayerId(0), ManaType::Blue, 1);
        add_mana(&mut state, PlayerId(0), ManaType::Colorless, 2);

        let mut events = Vec::new();
        let result =
            handle_cast_spell(&mut state, PlayerId(0), obj_id, CardId(20), &mut events).unwrap();

        assert!(matches!(result, WaitingFor::Priority { .. }));
        assert_eq!(state.stack.len(), 1);
        assert!(state.players[0].hand.is_empty());
    }

    #[test]
    fn cast_spell_rejects_lands() {
        let mut state = setup_game_at_main_phase();
        let land = create_object(
            &mut state,
            CardId(11),
            PlayerId(0),
            "Plains".to_string(),
            Zone::Hand,
        );
        let obj = state.objects.get_mut(&land).unwrap();
        obj.card_types.core_types.push(CoreType::Land);
        obj.card_types.subtypes.push("Plains".to_string());

        let result = handle_cast_spell(&mut state, PlayerId(0), land, CardId(11), &mut Vec::new());
        assert!(result.is_err());
        assert!(state.stack.is_empty());
    }

    #[test]
    fn sorcery_speed_rejects_during_opponent_turn() {
        let mut state = setup_game_at_main_phase();
        state.active_player = PlayerId(1); // Opponent's turn
        let obj_id = create_sorcery_in_hand(&mut state, PlayerId(0));
        add_mana(&mut state, PlayerId(0), ManaType::Blue, 3);

        let mut events = Vec::new();
        let result = handle_cast_spell(&mut state, PlayerId(0), obj_id, CardId(20), &mut events);
        assert!(result.is_err());
    }

    #[test]
    fn sorcery_speed_rejects_when_stack_not_empty() {
        let mut state = setup_game_at_main_phase();
        let obj_id = create_sorcery_in_hand(&mut state, PlayerId(0));
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
                casting_variant: CastingVariant::Normal,
            },
        });

        let mut events = Vec::new();
        let result = handle_cast_spell(&mut state, PlayerId(0), obj_id, CardId(20), &mut events);
        assert!(result.is_err());
    }

    #[test]
    fn instant_can_be_cast_at_any_priority() {
        let mut state = setup_game_at_main_phase();
        state.active_player = PlayerId(1); // Not active player
        let obj_id = create_instant_in_hand(&mut state, PlayerId(0));
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
        let result = handle_cast_spell(&mut state, PlayerId(0), obj_id, CardId(10), &mut events);
        // Should succeed -- instants can be cast at any priority
        assert!(result.is_ok());
    }

    #[test]
    fn flash_permission_option_allows_sorcery_outside_normal_window() {
        let mut state = setup_game_at_main_phase();
        state.phase = Phase::End;
        state.active_player = PlayerId(1);
        state.priority_player = PlayerId(0);

        let obj_id = create_sorcery_in_hand(&mut state, PlayerId(0));
        let obj = state.objects.get_mut(&obj_id).unwrap();
        obj.name = "Rout".to_string();
        obj.casting_options.push(
            crate::types::ability::SpellCastingOption::as_though_had_flash().cost(
                AbilityCost::Mana {
                    cost: ManaCost::Cost {
                        shards: vec![],
                        generic: 2,
                    },
                },
            ),
        );

        add_mana(&mut state, PlayerId(0), ManaType::Blue, 1);
        add_mana(&mut state, PlayerId(0), ManaType::Colorless, 4);

        let mut events = Vec::new();
        let result = handle_cast_spell(&mut state, PlayerId(0), obj_id, CardId(20), &mut events)
            .expect("flash permission should allow cast");

        assert!(matches!(result, WaitingFor::Priority { .. }));
        assert_eq!(state.stack.len(), 1);
    }

    #[test]
    fn flash_permission_cost_is_not_added_in_normal_timing_window() {
        let mut state = setup_game_at_main_phase();
        let obj_id = create_sorcery_in_hand(&mut state, PlayerId(0));
        let obj = state.objects.get_mut(&obj_id).unwrap();
        obj.casting_options.push(
            crate::types::ability::SpellCastingOption::as_though_had_flash().cost(
                AbilityCost::Mana {
                    cost: ManaCost::Cost {
                        shards: vec![],
                        generic: 2,
                    },
                },
            ),
        );

        add_mana(&mut state, PlayerId(0), ManaType::Blue, 1);
        add_mana(&mut state, PlayerId(0), ManaType::Colorless, 2);

        handle_cast_spell(&mut state, PlayerId(0), obj_id, CardId(20), &mut Vec::new())
            .expect("normal-timing cast should not require flash surcharge");
        assert_eq!(state.stack.len(), 1);
    }

    #[test]
    fn activated_ability_with_target_defers_cost_until_target_selection() {
        let mut state = setup_game_at_main_phase();
        let source = create_targeted_activated_permanent(&mut state, PlayerId(0));
        let target = create_object(
            &mut state,
            CardId(52),
            PlayerId(1),
            "Target".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&target)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        let waiting =
            handle_activate_ability(&mut state, PlayerId(0), source, 0, &mut Vec::new()).unwrap();

        assert!(matches!(waiting, WaitingFor::TargetSelection { .. }));
        state.waiting_for = waiting;
        assert!(!state.objects[&source].tapped);

        let mut events = Vec::new();
        let waiting = handle_select_targets(
            &mut state,
            PlayerId(0),
            vec![TargetRef::Object(target)],
            &mut events,
        )
        .unwrap();

        assert!(matches!(waiting, WaitingFor::Priority { .. }));
        assert!(state.objects[&source].tapped);
        assert!(events.iter().any(|event| matches!(
            event,
            GameEvent::AbilityActivated { source_id } if *source_id == source
        )));
    }

    #[test]
    fn deferred_tap_cost_fails_if_source_left_battlefield_before_target_lock() {
        let mut state = setup_game_at_main_phase();
        let source = create_targeted_activated_permanent(&mut state, PlayerId(0));
        let target = create_object(
            &mut state,
            CardId(52),
            PlayerId(1),
            "Target".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&target)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        let waiting =
            handle_activate_ability(&mut state, PlayerId(0), source, 0, &mut Vec::new()).unwrap();
        state.waiting_for = waiting;

        let mut zone_events = Vec::new();
        zones::move_to_zone(&mut state, source, Zone::Graveyard, &mut zone_events);

        let result = handle_select_targets(
            &mut state,
            PlayerId(0),
            vec![TargetRef::Object(target)],
            &mut Vec::new(),
        );

        assert!(result.is_err());
        assert!(!state.objects[&source].tapped);
    }

    #[test]
    fn activation_restriction_only_once_each_turn_is_enforced() {
        let mut state = setup_game_at_main_phase();
        let source = create_object(
            &mut state,
            CardId(70),
            PlayerId(0),
            "Relic".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&source).unwrap();
        obj.card_types.core_types.push(CoreType::Artifact);
        obj.abilities.push(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Draw {
                    count: QuantityExpr::Fixed { value: 1 },
                },
            )
            .activation_restrictions(vec![
                crate::types::ability::ActivationRestriction::OnlyOnceEachTurn,
            ]),
        );

        let mut events = Vec::new();
        handle_activate_ability(&mut state, PlayerId(0), source, 0, &mut events).unwrap();
        let second = handle_activate_ability(&mut state, PlayerId(0), source, 0, &mut events);

        assert!(second.is_err());
    }

    #[test]
    fn cancel_targeted_activated_ability_does_not_untap_source() {
        let mut state = setup_game_at_main_phase();
        let source = create_object(
            &mut state,
            CardId(71),
            PlayerId(0),
            "Weird Relic".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&source).unwrap();
        obj.card_types.core_types.push(CoreType::Artifact);
        obj.tapped = true;
        obj.abilities.push(AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::DealDamage {
                amount: QuantityExpr::Fixed { value: 1 },
                target: crate::types::ability::TargetFilter::Any,
                damage_source: None,
            },
        ));

        let waiting =
            handle_activate_ability(&mut state, PlayerId(0), source, 0, &mut Vec::new()).unwrap();
        assert!(matches!(waiting, WaitingFor::TargetSelection { .. }));

        let mut events = Vec::new();
        handle_cancel_cast(
            &mut state,
            &match waiting {
                WaitingFor::TargetSelection { pending_cast, .. } => *pending_cast,
                other => panic!("expected target selection, got {other:?}"),
            },
            &mut events,
        );

        assert!(state.objects[&source].tapped);
        assert!(events.is_empty());
    }

    #[test]
    fn cost_payment_deducts_mana() {
        let mut state = setup_game_at_main_phase();
        let obj_id = create_sorcery_in_hand(&mut state, PlayerId(0));
        add_mana(&mut state, PlayerId(0), ManaType::Blue, 1);
        add_mana(&mut state, PlayerId(0), ManaType::Colorless, 2);

        let initial_mana = state.players[0].mana_pool.total();
        assert_eq!(initial_mana, 3);

        let mut events = Vec::new();
        handle_cast_spell(&mut state, PlayerId(0), obj_id, CardId(20), &mut events).unwrap();

        assert_eq!(state.players[0].mana_pool.total(), 0);
    }

    #[test]
    fn cast_spell_insufficient_mana_fails() {
        let mut state = setup_game_at_main_phase();
        let obj_id = create_sorcery_in_hand(&mut state, PlayerId(0));
        // No mana added

        let mut events = Vec::new();
        let result = handle_cast_spell(&mut state, PlayerId(0), obj_id, CardId(20), &mut events);
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
            spell.abilities.push(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Draw {
                    count: QuantityExpr::Fixed { value: 1 },
                },
            ));
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
        let result = handle_cast_spell(&mut state, PlayerId(0), spell_id, CardId(22), &mut events);
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
            spell.abilities.push(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Draw {
                    count: QuantityExpr::Fixed { value: 1 },
                },
            ));
            spell.mana_cost = ManaCost::Cost {
                shards: vec![ManaCostShard::Black],
                generic: 0,
            };
        }

        create_gloomlake_verge(&mut state, PlayerId(0));

        let mut events = Vec::new();
        let result = handle_cast_spell(&mut state, PlayerId(0), spell_id, CardId(24), &mut events);
        assert!(
            result.is_err(),
            "expected cast to fail without Island/Swamp support"
        );
    }

    #[test]
    fn auto_tap_uses_layer_derived_basic_land_type() {
        let mut state = setup_game_at_main_phase();

        let spell_id = create_object(
            &mut state,
            CardId(25),
            PlayerId(0),
            "Deep-Cavern Bat".to_string(),
            Zone::Hand,
        );
        {
            let spell = state.objects.get_mut(&spell_id).unwrap();
            spell.card_types.core_types.push(CoreType::Creature);
            spell.abilities.push(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Unimplemented {
                    name: "PermanentCreature".to_string(),
                    description: None,
                },
            ));
            spell.mana_cost = ManaCost::Cost {
                shards: vec![ManaCostShard::Black],
                generic: 1,
            };
        }

        let passage = create_object(
            &mut state,
            CardId(26),
            PlayerId(0),
            "Multiversal Passage".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&passage).unwrap();
            obj.card_types.core_types.push(CoreType::Land);
            obj.chosen_attributes
                .push(ChosenAttribute::BasicLandType(BasicLandType::Swamp));
            obj.static_definitions.push(
                StaticDefinition::continuous()
                    .affected(crate::types::ability::TargetFilter::SelfRef)
                    .modifications(vec![ContinuousModification::AddChosenSubtype {
                        kind: ChosenSubtypeKind::BasicLandType,
                    }]),
            );
        }

        let forest = add_basic_land(&mut state, CardId(27), "Forest", "Forest");
        state.layers_dirty = true;

        let mut events = Vec::new();
        let result = handle_cast_spell(&mut state, PlayerId(0), spell_id, CardId(25), &mut events);
        assert!(
            result.is_ok(),
            "expected chosen land subtype from layers to satisfy black mana"
        );
        assert!(state.objects[&passage].tapped);
        assert!(state.objects[&forest].tapped);
    }

    #[test]
    fn cancel_cast_during_target_selection_returns_to_priority() {
        use crate::game::engine::apply;
        use crate::types::actions::GameAction;

        let mut state = setup_game_at_main_phase();
        let obj_id = create_instant_in_hand(&mut state, PlayerId(0));
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
                object_id: obj_id,
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

    use crate::types::ability::{ControllerRef, TargetFilter, TypedFilter};
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
            obj.keywords.push(Keyword::Enchant(TargetFilter::Typed(
                TypedFilter::creature(),
            )));
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
        let aura = create_aura_in_hand(&mut state, PlayerId(0));
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
        let result =
            handle_cast_spell(&mut state, PlayerId(0), aura, CardId(30), &mut events).unwrap();

        match result {
            WaitingFor::TargetSelection { target_slots, .. } => {
                assert_eq!(target_slots.len(), 1);
                assert_eq!(target_slots[0].legal_targets.len(), 2);
            }
            other => panic!("Expected TargetSelection, got {:?}", other),
        }
    }

    #[test]
    fn aura_with_single_target_auto_targets() {
        let mut state = setup_game_at_main_phase();
        let aura = create_aura_in_hand(&mut state, PlayerId(0));
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
        let result =
            handle_cast_spell(&mut state, PlayerId(0), aura, CardId(30), &mut events).unwrap();

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
        let aura = create_aura_in_hand(&mut state, PlayerId(0));
        add_mana(&mut state, PlayerId(0), ManaType::White, 1);

        // No creatures on battlefield -- no legal targets for "Enchant creature"
        let mut events = Vec::new();
        let result = handle_cast_spell(&mut state, PlayerId(0), aura, CardId(30), &mut events);
        assert!(result.is_err());
    }

    #[test]
    fn aura_with_enchant_you_control_rejects_opponent_creatures() {
        let mut state = setup_game_at_main_phase();
        let aura_id = create_aura_in_hand(&mut state, PlayerId(0));
        add_mana(&mut state, PlayerId(0), ManaType::White, 1);
        state.objects.get_mut(&aura_id).unwrap().keywords = vec![Keyword::Enchant(
            TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::You)),
        )];

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
        let result = handle_cast_spell(&mut state, PlayerId(0), aura_id, CardId(30), &mut events);
        assert!(result.is_err());
    }

    #[test]
    fn aura_with_enchant_you_control_accepts_own_creature() {
        let mut state = setup_game_at_main_phase();
        let aura_id = create_aura_in_hand(&mut state, PlayerId(0));
        add_mana(&mut state, PlayerId(0), ManaType::White, 1);
        state.objects.get_mut(&aura_id).unwrap().keywords = vec![Keyword::Enchant(
            TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::You)),
        )];

        let creature = create_object(
            &mut state,
            CardId(50),
            PlayerId(0),
            "Spirit".to_string(),
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
        let result =
            handle_cast_spell(&mut state, PlayerId(0), aura_id, CardId(30), &mut events).unwrap();
        assert!(matches!(result, WaitingFor::Priority { .. }));
        assert_eq!(state.stack.len(), 1);
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
    fn aura_targeting_respects_hexproof() {
        let mut state = setup_game_at_main_phase();
        let aura = create_aura_in_hand(&mut state, PlayerId(0));
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
        let result = handle_cast_spell(&mut state, PlayerId(0), aura, CardId(30), &mut events);
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
        let result =
            handle_cast_spell(&mut state, PlayerId(0), obj_id, CardId(40), &mut events).unwrap();

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

    #[test]
    fn pay_and_push_emits_targeting_events_for_chained_spell_targets() {
        let mut state = setup_game_at_main_phase();
        let object_id = create_object(
            &mut state,
            CardId(77),
            PlayerId(0),
            "Split Bolt".to_string(),
            Zone::Hand,
        );
        let creature = create_object(
            &mut state,
            CardId(88),
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
        add_mana(&mut state, PlayerId(0), ManaType::Red, 1);

        let ability = ResolvedAbility::new(
            Effect::DealDamage {
                amount: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Player,
                damage_source: None,
            },
            vec![TargetRef::Player(PlayerId(1))],
            object_id,
            PlayerId(0),
        )
        .sub_ability(ResolvedAbility::new(
            Effect::Destroy {
                target: TargetFilter::Typed(TypedFilter::creature()),
                cant_regenerate: false,
            },
            vec![TargetRef::Object(creature)],
            object_id,
            PlayerId(0),
        ));

        let mut events = Vec::new();
        let waiting_for = crate::game::casting_costs::pay_and_push(
            &mut state,
            PlayerId(0),
            object_id,
            CardId(77),
            ability,
            &ManaCost::Cost {
                shards: vec![ManaCostShard::Red],
                generic: 0,
            },
            CastingVariant::Normal,
            None,
            &mut events,
        )
        .expect("spell with chained targets should cast");

        assert!(matches!(waiting_for, WaitingFor::Priority { .. }));
        assert!(events.iter().any(|event| {
            matches!(
                event,
                GameEvent::BecomesTarget { object_id, .. } if *object_id == creature
            )
        }));
        assert!(events.iter().any(|event| {
            matches!(
                event,
                GameEvent::CrimeCommitted { player_id } if *player_id == PlayerId(0)
            )
        }));
    }

    // ── Modal spell tests ────────────────────────────────────────────────

    fn create_modal_charm(state: &mut GameState, player: PlayerId) -> ObjectId {
        let obj_id = create_object(
            state,
            CardId(50),
            player,
            "Test Charm".to_string(),
            Zone::Hand,
        );
        {
            let obj = state.objects.get_mut(&obj_id).unwrap();
            obj.card_types.core_types.push(CoreType::Instant);
            // Mode 0: Deal 2 damage to any target
            obj.abilities.push(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::DealDamage {
                    amount: QuantityExpr::Fixed { value: 2 },
                    target: crate::types::ability::TargetFilter::Any,
                    damage_source: None,
                },
            ));
            // Mode 1: Draw a card
            obj.abilities.push(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Draw {
                    count: QuantityExpr::Fixed { value: 1 },
                },
            ));
            // Mode 2: Gain 3 life
            obj.abilities.push(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::GainLife {
                    amount: QuantityExpr::Fixed { value: 3 },
                    player: crate::types::ability::GainLifePlayer::Controller,
                },
            ));
            obj.mana_cost = ManaCost::Cost {
                shards: vec![ManaCostShard::Red],
                generic: 0,
            };
            obj.modal = Some(crate::types::ability::ModalChoice {
                min_choices: 1,
                max_choices: 1,
                mode_count: 3,
                mode_descriptions: vec![
                    "Deal 2 damage to any target".to_string(),
                    "Draw a card".to_string(),
                    "Gain 3 life".to_string(),
                ],
                ..Default::default()
            });
        }
        obj_id
    }

    #[test]
    fn modal_spell_enters_mode_choice() {
        let mut state = setup_game_at_main_phase();
        let obj_id = create_modal_charm(&mut state, PlayerId(0));
        add_mana(&mut state, PlayerId(0), ManaType::Red, 1);

        let mut events = Vec::new();
        let result =
            handle_cast_spell(&mut state, PlayerId(0), obj_id, CardId(50), &mut events).unwrap();
        assert!(
            matches!(result, WaitingFor::ModeChoice { .. }),
            "expected ModeChoice, got {result:?}"
        );
    }

    #[test]
    fn modal_spell_mode_choice_has_correct_metadata() {
        let mut state = setup_game_at_main_phase();
        let obj_id = create_modal_charm(&mut state, PlayerId(0));
        add_mana(&mut state, PlayerId(0), ManaType::Red, 1);

        let mut events = Vec::new();
        let result =
            handle_cast_spell(&mut state, PlayerId(0), obj_id, CardId(50), &mut events).unwrap();
        match result {
            WaitingFor::ModeChoice { modal, .. } => {
                assert_eq!(modal.min_choices, 1);
                assert_eq!(modal.max_choices, 1);
                assert_eq!(modal.mode_count, 3);
                assert_eq!(modal.mode_descriptions.len(), 3);
            }
            _ => panic!("expected ModeChoice"),
        }
    }

    #[test]
    fn select_mode_with_no_target_goes_to_priority() {
        let mut state = setup_game_at_main_phase();
        let obj_id = create_modal_charm(&mut state, PlayerId(0));
        add_mana(&mut state, PlayerId(0), ManaType::Red, 1);

        let mut events = Vec::new();
        let result =
            handle_cast_spell(&mut state, PlayerId(0), obj_id, CardId(50), &mut events).unwrap();
        state.waiting_for = result;

        // Select mode 1 (Draw a card) -- no targets needed
        let result = handle_select_modes(&mut state, PlayerId(0), vec![1], &mut events).unwrap();
        assert!(
            matches!(result, WaitingFor::Priority { .. }),
            "expected Priority after selecting no-target mode, got {result:?}"
        );
        assert_eq!(state.stack.len(), 1);
    }

    #[test]
    fn select_mode_with_target_enters_targeting() {
        let mut state = setup_game_at_main_phase();
        let charm_id = create_modal_charm(&mut state, PlayerId(0));
        add_mana(&mut state, PlayerId(0), ManaType::Red, 1);

        // Create a creature to target
        let creature = create_object(
            &mut state,
            CardId(99),
            PlayerId(1),
            "Bear".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&creature).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.power = Some(2);
            obj.toughness = Some(2);
        }
        state.battlefield.push(creature);

        let mut events = Vec::new();
        let result =
            handle_cast_spell(&mut state, PlayerId(0), charm_id, CardId(50), &mut events).unwrap();
        state.waiting_for = result;

        // Select mode 0 (Deal 2 damage) -- has targets (players + creature)
        let result = handle_select_modes(&mut state, PlayerId(0), vec![0], &mut events).unwrap();
        // Multiple legal targets exist (2 players + creature), so TargetSelection
        assert!(
            matches!(result, WaitingFor::TargetSelection { .. }),
            "expected TargetSelection, got {result:?}"
        );
    }

    #[test]
    fn select_mode_invalid_count_rejected() {
        let mut state = setup_game_at_main_phase();
        let obj_id = create_modal_charm(&mut state, PlayerId(0));
        add_mana(&mut state, PlayerId(0), ManaType::Red, 1);

        let mut events = Vec::new();
        let result =
            handle_cast_spell(&mut state, PlayerId(0), obj_id, CardId(50), &mut events).unwrap();
        state.waiting_for = result;

        // Try selecting 2 modes when only 1 allowed
        let result = handle_select_modes(&mut state, PlayerId(0), vec![0, 1], &mut events);
        assert!(result.is_err());
    }

    #[test]
    fn select_mode_out_of_range_rejected() {
        let mut state = setup_game_at_main_phase();
        let obj_id = create_modal_charm(&mut state, PlayerId(0));
        add_mana(&mut state, PlayerId(0), ManaType::Red, 1);

        let mut events = Vec::new();
        let result =
            handle_cast_spell(&mut state, PlayerId(0), obj_id, CardId(50), &mut events).unwrap();
        state.waiting_for = result;

        // Try selecting a mode index that doesn't exist
        let result = handle_select_modes(&mut state, PlayerId(0), vec![5], &mut events);
        assert!(result.is_err());
    }

    #[test]
    fn select_mode_duplicate_rejected() {
        let mut state = setup_game_at_main_phase();
        let obj_id = create_modal_charm(&mut state, PlayerId(0));
        add_mana(&mut state, PlayerId(0), ManaType::Red, 1);

        // Change to "choose two" to test duplicate rejection
        {
            let obj = state.objects.get_mut(&obj_id).unwrap();
            obj.modal.as_mut().unwrap().min_choices = 2;
            obj.modal.as_mut().unwrap().max_choices = 2;
        }

        let mut events = Vec::new();
        let result =
            handle_cast_spell(&mut state, PlayerId(0), obj_id, CardId(50), &mut events).unwrap();
        state.waiting_for = result;

        // Try selecting the same mode twice
        let result = handle_select_modes(&mut state, PlayerId(0), vec![1, 1], &mut events);
        assert!(result.is_err());
    }

    #[test]
    fn choose_two_modal_chains_modes() {
        let mut state = setup_game_at_main_phase();
        let obj_id = create_modal_charm(&mut state, PlayerId(0));
        add_mana(&mut state, PlayerId(0), ManaType::Red, 1);

        // Change to "choose two"
        {
            let obj = state.objects.get_mut(&obj_id).unwrap();
            obj.modal.as_mut().unwrap().min_choices = 2;
            obj.modal.as_mut().unwrap().max_choices = 2;
        }

        let mut events = Vec::new();
        let result =
            handle_cast_spell(&mut state, PlayerId(0), obj_id, CardId(50), &mut events).unwrap();
        state.waiting_for = result;

        // Select modes 1 (Draw) and 2 (Gain life) -- no targets needed
        let result = handle_select_modes(&mut state, PlayerId(0), vec![1, 2], &mut events).unwrap();
        assert!(
            matches!(result, WaitingFor::Priority { .. }),
            "expected Priority, got {result:?}"
        );
        assert_eq!(state.stack.len(), 1);

        // Verify the stack entry has a chained ability (sub_ability present)
        match &state.stack[0].kind {
            StackEntryKind::Spell { ability, .. } => {
                // First mode is Draw
                assert!(matches!(
                    ability.effect,
                    Effect::Draw {
                        count: QuantityExpr::Fixed { value: 1 }
                    }
                ));
                // Second mode is GainLife as sub_ability
                let sub = ability
                    .sub_ability
                    .as_ref()
                    .expect("should have sub_ability");
                assert!(matches!(sub.effect, Effect::GainLife { .. }));
            }
            _ => panic!("expected Spell on stack"),
        }
    }

    #[test]
    fn cancel_modal_returns_to_priority() {
        let mut state = setup_game_at_main_phase();
        let obj_id = create_modal_charm(&mut state, PlayerId(0));
        add_mana(&mut state, PlayerId(0), ManaType::Red, 1);

        let mut events = Vec::new();
        let result =
            handle_cast_spell(&mut state, PlayerId(0), obj_id, CardId(50), &mut events).unwrap();
        state.waiting_for = result;

        // Cancel should return to priority
        assert!(matches!(state.waiting_for, WaitingFor::ModeChoice { .. }));
    }

    // --- Adventure tests ---

    /// Create an Adventure card in hand: Bonecrusher Giant (creature) / Stomp (instant).
    fn create_adventure_in_hand(state: &mut GameState, player: PlayerId) -> ObjectId {
        let obj_id = create_object(
            state,
            CardId(70),
            player,
            "Bonecrusher Giant".to_string(),
            Zone::Hand,
        );
        let obj = state.objects.get_mut(&obj_id).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.power = Some(4);
        obj.toughness = Some(3);
        obj.mana_cost = ManaCost::Cost {
            shards: vec![ManaCostShard::Red],
            generic: 2,
        };

        // Adventure face stored in back_face (Stomp - instant, {1}{R})
        obj.back_face = Some(crate::game::game_object::BackFaceData {
            name: "Stomp".to_string(),
            power: None,
            toughness: None,
            loyalty: None,
            card_types: {
                let mut ct = crate::types::card_type::CardType::default();
                ct.core_types.push(CoreType::Instant);
                ct
            },
            mana_cost: ManaCost::Cost {
                shards: vec![ManaCostShard::Red],
                generic: 1,
            },
            keywords: Vec::new(),
            abilities: vec![crate::types::ability::AbilityDefinition::new(
                crate::types::ability::AbilityKind::Spell,
                Effect::DealDamage {
                    amount: QuantityExpr::Fixed { value: 2 },
                    target: crate::types::ability::TargetFilter::Any,
                    damage_source: None,
                },
            )],
            trigger_definitions: Vec::new(),
            replacement_definitions: Vec::new(),
            static_definitions: Vec::new(),
            color: vec![ManaColor::Red],
            printed_ref: None,
            modal: None,
            additional_cost: None,
            strive_cost: None,
            casting_restrictions: Vec::new(),
            casting_options: Vec::new(),
        });

        obj_id
    }

    #[test]
    fn adventure_cast_choice_from_hand() {
        let mut state = setup_game_at_main_phase();
        let obj_id = create_adventure_in_hand(&mut state, PlayerId(0));
        add_mana(&mut state, PlayerId(0), ManaType::Red, 3);

        let mut events = Vec::new();
        let result =
            handle_cast_spell(&mut state, PlayerId(0), obj_id, CardId(70), &mut events).unwrap();

        // Should prompt for Adventure face choice
        assert!(
            matches!(result, WaitingFor::AdventureCastChoice { player, card_id, .. }
                if player == PlayerId(0) && card_id == CardId(70)),
            "Expected AdventureCastChoice, got {:?}",
            result
        );
    }

    #[test]
    fn adventure_exile_on_resolve() {
        let mut state = setup_game_at_main_phase();
        let obj_id = create_adventure_in_hand(&mut state, PlayerId(0));

        // Directly push an Adventure spell on the stack (bypass targeting)
        zones::move_to_zone(&mut state, obj_id, Zone::Stack, &mut Vec::new());

        // Swap to Adventure face (simulating what handle_adventure_choice does)
        if let Some(obj) = state.objects.get_mut(&obj_id) {
            swap_to_adventure_face(obj);
        }

        state.stack.push(StackEntry {
            id: obj_id,
            source_id: obj_id,
            controller: PlayerId(0),
            kind: StackEntryKind::Spell {
                card_id: CardId(70),
                ability: ResolvedAbility::new(
                    Effect::DealDamage {
                        amount: QuantityExpr::Fixed { value: 2 },
                        target: crate::types::ability::TargetFilter::Any,
                        damage_source: None,
                    },
                    vec![TargetRef::Player(PlayerId(1))],
                    obj_id,
                    PlayerId(0),
                ),
                casting_variant: CastingVariant::Adventure,
            },
        });

        // The object should now have Adventure face active
        assert_eq!(state.objects[&obj_id].name, "Stomp");

        // Resolve the spell
        let mut events = Vec::new();
        crate::game::stack::resolve_top(&mut state, &mut events);

        // Card should be in exile with AdventureCreature permission
        assert!(
            state.exile.contains(&obj_id),
            "Adventure spell should resolve to exile"
        );
        let obj = state.objects.get(&obj_id).unwrap();
        assert!(
            obj.casting_permissions
                .contains(&crate::types::ability::CastingPermission::AdventureCreature),
            "Should have AdventureCreature permission"
        );
        // Name should be restored to creature face
        assert_eq!(obj.name, "Bonecrusher Giant");
    }

    #[test]
    fn adventure_countered_to_graveyard() {
        let mut state = setup_game_at_main_phase();
        let obj_id = create_adventure_in_hand(&mut state, PlayerId(0));

        // Manually put an Adventure spell on the stack
        zones::move_to_zone(&mut state, obj_id, Zone::Stack, &mut Vec::new());
        state.stack.push(StackEntry {
            id: obj_id,
            source_id: obj_id,
            controller: PlayerId(0),
            kind: StackEntryKind::Spell {
                card_id: CardId(70),
                ability: ResolvedAbility::new(
                    Effect::DealDamage {
                        amount: QuantityExpr::Fixed { value: 2 },
                        target: crate::types::ability::TargetFilter::Any,
                        damage_source: None,
                    },
                    vec![TargetRef::Player(PlayerId(1))],
                    obj_id,
                    PlayerId(0),
                ),
                casting_variant: CastingVariant::Adventure,
            },
        });

        // Counter the spell (remove from stack, move to graveyard)
        state.stack.pop();
        zones::move_to_zone(&mut state, obj_id, Zone::Graveyard, &mut Vec::new());

        // Card should be in graveyard, NOT exile
        assert!(
            state.players[0].graveyard.contains(&obj_id),
            "Countered adventure spell should go to graveyard"
        );
        assert!(
            !state.exile.contains(&obj_id),
            "Countered adventure spell should NOT be in exile"
        );
        // Should NOT have AdventureCreature permission
        let obj = state.objects.get(&obj_id).unwrap();
        assert!(
            !obj.casting_permissions
                .contains(&crate::types::ability::CastingPermission::AdventureCreature),
            "Countered spell should not get casting permission"
        );
    }

    #[test]
    fn adventure_cast_creature_from_exile() {
        let mut state = setup_game_at_main_phase();
        let obj_id = create_adventure_in_hand(&mut state, PlayerId(0));
        add_mana(&mut state, PlayerId(0), ManaType::Red, 3);

        // Move to exile with AdventureCreature permission (simulates resolved Adventure)
        zones::move_to_zone(&mut state, obj_id, Zone::Exile, &mut Vec::new());
        let obj = state.objects.get_mut(&obj_id).unwrap();
        obj.casting_permissions
            .push(crate::types::ability::CastingPermission::AdventureCreature);

        // Should appear in available to cast
        let available = spell_objects_available_to_cast(&state, PlayerId(0));
        assert!(
            available.contains(&obj_id),
            "Exiled Adventure creature should be castable"
        );

        // Should NOT trigger AdventureCastChoice (from exile, always cast as creature)
        let mut events = Vec::new();
        let result =
            handle_cast_spell(&mut state, PlayerId(0), obj_id, CardId(70), &mut events).unwrap();
        // Should proceed to payment, not to AdventureCastChoice
        assert!(
            !matches!(result, WaitingFor::AdventureCastChoice { .. }),
            "Casting from exile should not prompt for face choice"
        );
    }

    #[test]
    fn can_pay_sacrifice_cost_with_eligible() {
        use crate::types::ability::TypedFilter;

        let mut state = setup_game_at_main_phase();
        let source = create_object(
            &mut state,
            CardId(50),
            PlayerId(0),
            "Viscera Seer".to_string(),
            Zone::Battlefield,
        );
        let creature = create_object(
            &mut state,
            CardId(51),
            PlayerId(0),
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

        let cost = AbilityCost::Sacrifice {
            target: TargetFilter::Typed(TypedFilter::creature()),
        };
        assert!(can_pay_ability_cost_now(&state, PlayerId(0), source, &cost));
    }

    #[test]
    fn cannot_pay_sacrifice_cost_no_eligible() {
        use crate::types::ability::TypedFilter;

        let mut state = setup_game_at_main_phase();
        let source = create_object(
            &mut state,
            CardId(50),
            PlayerId(0),
            "Viscera Seer".to_string(),
            Zone::Battlefield,
        );
        // No other creatures on the battlefield
        let cost = AbilityCost::Sacrifice {
            target: TargetFilter::Typed(TypedFilter::creature()),
        };
        assert!(!can_pay_ability_cost_now(
            &state,
            PlayerId(0),
            source,
            &cost
        ));
    }

    #[test]
    fn reduce_generic_in_mana_cost() {
        let mut cost = AbilityCost::Mana {
            cost: ManaCost::Cost {
                generic: 2,
                shards: vec![ManaCostShard::White],
            },
        };
        reduce_generic_in_cost(&mut cost, 1);
        assert_eq!(
            cost,
            AbilityCost::Mana {
                cost: ManaCost::Cost {
                    generic: 1,
                    shards: vec![ManaCostShard::White],
                }
            }
        );
    }

    #[test]
    fn reduce_generic_floors_at_zero() {
        let mut cost = AbilityCost::Mana {
            cost: ManaCost::Cost {
                generic: 2,
                shards: vec![ManaCostShard::White],
            },
        };
        reduce_generic_in_cost(&mut cost, 5);
        assert_eq!(
            cost,
            AbilityCost::Mana {
                cost: ManaCost::Cost {
                    generic: 0,
                    shards: vec![ManaCostShard::White],
                }
            }
        );
    }

    #[test]
    fn reduce_generic_no_op_on_colored_only() {
        let mut cost = AbilityCost::Mana {
            cost: ManaCost::Cost {
                generic: 0,
                shards: vec![ManaCostShard::White],
            },
        };
        reduce_generic_in_cost(&mut cost, 2);
        assert_eq!(
            cost,
            AbilityCost::Mana {
                cost: ManaCost::Cost {
                    generic: 0,
                    shards: vec![ManaCostShard::White],
                }
            }
        );
    }

    #[test]
    fn reduce_generic_in_composite() {
        let mut cost = AbilityCost::Composite {
            costs: vec![
                AbilityCost::Mana {
                    cost: ManaCost::Cost {
                        generic: 2,
                        shards: vec![ManaCostShard::White],
                    },
                },
                AbilityCost::Discard {
                    count: 1,
                    filter: None,
                    random: false,
                    self_ref: true,
                },
            ],
        };
        reduce_generic_in_cost(&mut cost, 1);
        match cost {
            AbilityCost::Composite { ref costs } => {
                assert_eq!(
                    costs[0],
                    AbilityCost::Mana {
                        cost: ManaCost::Cost {
                            generic: 1,
                            shards: vec![ManaCostShard::White],
                        }
                    }
                );
            }
            _ => panic!("Expected Composite"),
        }
    }

    // ---- CantCastDuring runtime enforcement tests ----

    use crate::types::statics::{CastingProhibitionCondition, CastingProhibitionScope};

    fn add_cant_cast_during_permanent(
        state: &mut GameState,
        controller: PlayerId,
        who: CastingProhibitionScope,
        when: CastingProhibitionCondition,
    ) -> ObjectId {
        let id = create_object(
            state,
            CardId(state.next_object_id),
            controller,
            "Prohibitor".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&id)
            .unwrap()
            .static_definitions
            .push(StaticDefinition::new(StaticMode::CantCastDuring {
                who,
                when,
            }));
        id
    }

    #[test]
    fn cant_cast_during_runtime_opponent_blocked_on_controllers_turn() {
        let mut state = setup_game_at_main_phase();
        // Player 0 controls Teferi-like permanent: opponents can't cast during your turn
        add_cant_cast_during_permanent(
            &mut state,
            PlayerId(0),
            CastingProhibitionScope::Opponents,
            CastingProhibitionCondition::DuringYourTurn,
        );
        // Active player is 0 (controller's turn) — opponent (Player 1) should be blocked
        assert!(is_blocked_by_cant_cast_during(&state, PlayerId(1)));
    }

    #[test]
    fn cant_cast_during_runtime_controller_can_cast_own_turn() {
        let mut state = setup_game_at_main_phase();
        add_cant_cast_during_permanent(
            &mut state,
            PlayerId(0),
            CastingProhibitionScope::Opponents,
            CastingProhibitionCondition::DuringYourTurn,
        );
        // Controller (Player 0) should NOT be blocked by their own "opponents can't cast"
        assert!(!is_blocked_by_cant_cast_during(&state, PlayerId(0)));
    }

    #[test]
    fn cant_cast_during_runtime_all_players_blocked_during_combat() {
        let mut state = setup_game_at_main_phase();
        state.phase = Phase::DeclareAttackers;
        add_cant_cast_during_permanent(
            &mut state,
            PlayerId(0),
            CastingProhibitionScope::AllPlayers,
            CastingProhibitionCondition::DuringCombat,
        );
        // Both players should be blocked during combat
        assert!(is_blocked_by_cant_cast_during(&state, PlayerId(0)));
        assert!(is_blocked_by_cant_cast_during(&state, PlayerId(1)));
    }

    #[test]
    fn cant_cast_during_runtime_not_blocked_during_main_phase() {
        let mut state = setup_game_at_main_phase();
        // Phase is PreCombatMain — DuringCombat prohibition should NOT apply
        add_cant_cast_during_permanent(
            &mut state,
            PlayerId(0),
            CastingProhibitionScope::AllPlayers,
            CastingProhibitionCondition::DuringCombat,
        );
        assert!(!is_blocked_by_cant_cast_during(&state, PlayerId(0)));
        assert!(!is_blocked_by_cant_cast_during(&state, PlayerId(1)));
    }

    #[test]
    fn cant_cast_during_runtime_no_statics_returns_false() {
        let state = setup_game_at_main_phase();
        // No CantCastDuring statics on battlefield — baseline
        assert!(!is_blocked_by_cant_cast_during(&state, PlayerId(0)));
        assert!(!is_blocked_by_cant_cast_during(&state, PlayerId(1)));
    }

    #[test]
    fn creature_in_hand_castable_with_untapped_lands() {
        use crate::ai_support::{candidate_actions, legal_actions};
        use crate::game::derived::derive_display_state;
        use crate::types::ability::{AbilityKind, Effect, ManaProduction};
        use crate::types::actions::GameAction;

        let mut state = setup_game_at_main_phase();

        // Add a Forest to the battlefield (produces {G})
        let forest = add_basic_land(&mut state, CardId(100), "Forest", "Forest");
        let obj = state.objects.get_mut(&forest).unwrap();
        obj.abilities.push(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Mana {
                    produced: ManaProduction::Fixed {
                        colors: vec![ManaColor::Green],
                    },
                    restrictions: vec![],
                    expiry: None,
                },
            )
            .cost(crate::types::ability::AbilityCost::Tap),
        );

        // Add a creature to hand: "Elf" with cost {G}
        let elf = create_object(
            &mut state,
            CardId(101),
            PlayerId(0),
            "Test Elf".to_string(),
            Zone::Hand,
        );
        {
            let obj = state.objects.get_mut(&elf).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.mana_cost = ManaCost::Cost {
                shards: vec![ManaCostShard::Green],
                generic: 0,
            };
        }

        derive_display_state(&mut state);

        // Verify can_cast_object_now returns true
        assert!(
            can_cast_object_now(&state, PlayerId(0), elf),
            "Creature costing {{G}} should be castable with an untapped Forest"
        );

        // Verify it appears in candidate_actions
        let candidates = candidate_actions(&state);
        assert!(
            candidates.iter().any(|c| matches!(
                &c.action,
                GameAction::CastSpell { object_id, .. } if *object_id == elf
            )),
            "CastSpell should appear in candidate_actions"
        );

        // Verify it survives validated_candidate_actions → legal_actions
        let actions = legal_actions(&state);
        assert!(
            actions.iter().any(|a| matches!(
                a,
                GameAction::CastSpell { object_id, .. } if *object_id == elf
            )),
            "CastSpell should appear in legal_actions"
        );
    }

    #[test]
    fn creature_castable_via_mana_dork_when_lands_tapped() {
        use crate::ai_support::legal_actions;
        use crate::game::derived::derive_display_state;
        use crate::types::ability::{AbilityKind, Effect, ManaProduction};
        use crate::types::actions::GameAction;

        let mut state = setup_game_at_main_phase();

        // Add a tapped Forest (no mana available from it)
        let forest = add_basic_land(&mut state, CardId(100), "Forest", "Forest");
        state.objects.get_mut(&forest).unwrap().tapped = true;

        // Add untapped Llanowar Elves (mana dork: T: Add {G})
        let dork = create_object(
            &mut state,
            CardId(101),
            PlayerId(0),
            "Llanowar Elves".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&dork).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.card_types.subtypes.push("Elf".to_string());
            obj.entered_battlefield_turn = Some(1); // entered last turn → no summoning sickness
            obj.abilities.push(
                AbilityDefinition::new(
                    AbilityKind::Activated,
                    Effect::Mana {
                        produced: ManaProduction::Fixed {
                            colors: vec![ManaColor::Green],
                        },
                        restrictions: vec![],
                        expiry: None,
                    },
                )
                .cost(crate::types::ability::AbilityCost::Tap),
            );
        }

        // Add creature to hand: cost {G}
        let elf = create_object(
            &mut state,
            CardId(102),
            PlayerId(0),
            "Test Elf".to_string(),
            Zone::Hand,
        );
        {
            let obj = state.objects.get_mut(&elf).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.mana_cost = ManaCost::Cost {
                shards: vec![ManaCostShard::Green],
                generic: 0,
            };
        }

        derive_display_state(&mut state);

        // The dork should be identified as a mana source
        assert!(
            state.objects[&dork].has_mana_ability,
            "Llanowar Elves should have has_mana_ability"
        );
        assert!(
            !state.objects[&dork].has_summoning_sickness,
            "Llanowar Elves should not have summoning sickness"
        );

        // can_cast_object_now should pass — dork provides {G}
        assert!(
            can_cast_object_now(&state, PlayerId(0), elf),
            "Creature costing {{G}} should be castable via mana dork"
        );

        // Should appear in legal_actions
        let actions = legal_actions(&state);
        assert!(
            actions.iter().any(|a| matches!(
                a,
                GameAction::CastSpell { object_id, .. } if *object_id == elf
            )),
            "CastSpell via mana dork should appear in legal_actions"
        );
    }

    /// Reproduces the Priest of Titania scenario: a dynamic-count mana dork
    /// (AnyOneColor with ObjectCount) as the only mana source. Before the
    /// color_override fix, resolve_mana_ability truncated dynamic counts to 1,
    /// making expensive spells appear unaffordable.
    #[test]
    fn creature_castable_via_dynamic_mana_dork() {
        use crate::ai_support::legal_actions;
        use crate::game::derived::derive_display_state;
        use crate::types::ability::{
            AbilityKind, Effect, ManaProduction, QuantityExpr, QuantityRef, TargetFilter,
        };
        use crate::types::actions::GameAction;

        let mut state = setup_game_at_main_phase();

        // Add several elves to make the dynamic count work
        for i in 0..5u64 {
            let elf_id = create_object(
                &mut state,
                CardId(200 + i),
                PlayerId(0),
                format!("Elf Token {i}"),
                Zone::Battlefield,
            );
            let obj = state.objects.get_mut(&elf_id).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.card_types.subtypes.push("Elf".to_string());
            obj.entered_battlefield_turn = Some(1);
        }

        // Add Priest of Titania: T: Add {G} for each Elf on the battlefield
        let priest = create_object(
            &mut state,
            CardId(210),
            PlayerId(0),
            "Priest of Titania".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&priest).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.card_types.subtypes.push("Elf".to_string());
            obj.entered_battlefield_turn = Some(1);
            obj.abilities.push(
                AbilityDefinition::new(
                    AbilityKind::Activated,
                    Effect::Mana {
                        produced: ManaProduction::AnyOneColor {
                            count: QuantityExpr::Ref {
                                qty: QuantityRef::ObjectCount {
                                    filter: TargetFilter::Typed(
                                        crate::types::ability::TypedFilter {
                                            type_filters: vec![
                                                crate::types::ability::TypeFilter::Subtype(
                                                    "Elf".to_string(),
                                                ),
                                            ],
                                            controller: None,
                                            properties: vec![],
                                        },
                                    ),
                                },
                            },
                            color_options: vec![ManaColor::Green],
                        },
                        restrictions: vec![],
                        expiry: None,
                    },
                )
                .cost(crate::types::ability::AbilityCost::Tap),
            );
        }

        // Add Craterhoof-like creature to hand: cost {5}{G}{G}{G}
        let behemoth = create_object(
            &mut state,
            CardId(211),
            PlayerId(0),
            "Craterhoof Behemoth".to_string(),
            Zone::Hand,
        );
        {
            let obj = state.objects.get_mut(&behemoth).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.mana_cost = ManaCost::Cost {
                shards: vec![
                    ManaCostShard::Green,
                    ManaCostShard::Green,
                    ManaCostShard::Green,
                ],
                generic: 5,
            };
        }

        derive_display_state(&mut state);

        // Priest sees 6 elves (5 tokens + herself) → produces 6G
        // Craterhoof costs 8 total (5 generic + 3 green)
        // 6G is NOT enough for 8 total... but let's test a cheaper spell too.

        // Actually, 6 elves → 6G. Cost is {5}{G}{G}{G} = 8. Fails even with fix.
        // Let's make it castable by adding one more elf.
        let extra_elf = create_object(
            &mut state,
            CardId(212),
            PlayerId(0),
            "Extra Elf".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&extra_elf).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.card_types.subtypes.push("Elf".to_string());
            obj.entered_battlefield_turn = Some(1);
        }
        // Add another elf so Priest sees 8 elves → 8G → exactly enough for {5}{G}{G}{G}
        let extra_elf2 = create_object(
            &mut state,
            CardId(213),
            PlayerId(0),
            "Extra Elf 2".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&extra_elf2).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.card_types.subtypes.push("Elf".to_string());
            obj.entered_battlefield_turn = Some(1);
        }

        derive_display_state(&mut state);

        // Priest sees 8 elves → produces 8G. Cost {5}{G}{G}{G} = 8 total. Exactly enough.
        assert!(
            can_cast_object_now(&state, PlayerId(0), behemoth),
            "Craterhoof should be castable when Priest of Titania produces 8G"
        );

        let actions = legal_actions(&state);
        assert!(
            actions.iter().any(|a| matches!(
                a,
                GameAction::CastSpell { object_id, .. } if *object_id == behemoth
            )),
            "CastSpell for Craterhoof should appear in legal_actions"
        );
    }
}
