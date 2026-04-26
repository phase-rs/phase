use crate::types::ability::{
    AbilityCost, AbilityDefinition, AbilityKind, AdditionalCost, CardPlayMode, ChoiceType, Effect,
    GameRestriction, QuantityExpr, ResolvedAbility, RestrictionPlayerScope, StaticDefinition,
    TargetFilter, TargetRef,
};
use crate::types::events::GameEvent;
use crate::types::game_state::{
    CastingVariant, ConvokeMode, GameState, PendingCast, SneakPlacement, SpellCastRecord,
    StackEntry, StackEntryKind, WaitingFor,
};
use crate::types::identifiers::{CardId, ObjectId};
use crate::types::keywords::{FlashbackCost, Keyword, KeywordKind};
use crate::types::mana::{ManaCost, ManaSpellGrant, PaymentContext, SpellMeta};
use crate::types::player::PlayerId;
use crate::types::statics::{
    ActivationExemption, CastFrequency, CastingProhibitionCondition, ProhibitionScope, StaticMode,
};
use crate::types::zones::Zone;

use std::collections::HashSet;

use super::ability_utils::{
    assign_targets_in_chain, auto_select_targets, auto_select_targets_for_ability,
    begin_target_selection, begin_target_selection_for_ability, build_resolved_from_def,
    build_target_slots, compute_unavailable_modes, flatten_targets_in_chain,
    target_constraints_from_modal,
};
use super::casting_costs::{
    self, auto_tap_mana_sources, check_additional_cost_or_pay, pay_and_push_adventure,
};
use super::engine::EngineError;
use super::functioning_abilities::active_static_definitions;
use super::mana_payment;
use super::quantity::resolve_quantity;
use super::restrictions;
use super::speed::{effective_speed, set_speed};
use super::stack;
use super::targeting;

pub(crate) fn variable_speed_payment_range(cost: &AbilityCost, max_speed: u8) -> Option<(u8, u8)> {
    match cost {
        AbilityCost::PaySpeed {
            amount:
                QuantityExpr::Ref {
                    qty: crate::types::ability::QuantityRef::Variable { .. },
                },
        } => Some((0, max_speed)),
        AbilityCost::Composite { costs } => costs
            .iter()
            .find_map(|sub_cost| variable_speed_payment_range(sub_cost, max_speed)),
        _ => None,
    }
}

pub(crate) fn begin_variable_speed_payment(
    state: &mut GameState,
    player: PlayerId,
    source_id: ObjectId,
    resolved: ResolvedAbility,
    cost: AbilityCost,
    ability_index: usize,
) -> WaitingFor {
    let max_speed = effective_speed(state, player);
    let (min, max) = variable_speed_payment_range(&cost, max_speed).unwrap_or((0, max_speed));
    let mut pending = PendingCast::new(source_id, CardId(0), resolved, ManaCost::NoCost);
    pending.activation_cost = Some(cost);
    pending.activation_ability_index = Some(ability_index);
    state.pending_cast = Some(Box::new(pending));
    WaitingFor::NamedChoice {
        player,
        options: (min..=max).map(|value| value.to_string()).collect(),
        choice_type: ChoiceType::NumberRange { min, max },
        source_id: None,
    }
}

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
    /// The spell's ability definition. `None` for permanent spells with no
    /// spell-level effect (creatures, artifacts, etc.).
    ability_def: Option<AbilityDefinition>,
    mana_cost: crate::types::mana::ManaCost,
    modal: Option<crate::types::ability::ModalChoice>,
    casting_variant: CastingVariant,
    /// CR 601.2a: Zone the card was in before announcement (hand / command /
    /// graveyard / exile). Threaded onto `PendingCast.origin_zone` so that
    /// CancelCast (CR 601.2i) can return the object to its origin zone.
    origin_zone: Zone,
}

fn combined_spell_ability_def(
    obj: &crate::game::game_object::GameObject,
) -> Option<AbilityDefinition> {
    let mut spell_abilities = obj
        .abilities
        .iter()
        .filter(|a| a.kind == AbilityKind::Spell);
    let mut combined = spell_abilities.next()?.clone();

    if obj.modal.is_some() {
        return Some(combined);
    }

    for spell_ability in spell_abilities {
        append_to_ability_def_sub_chain(&mut combined, spell_ability.clone());
    }

    Some(combined)
}

fn append_to_ability_def_sub_chain(ability: &mut AbilityDefinition, next: AbilityDefinition) {
    let mut node = ability;
    while node.sub_ability.is_some() {
        node = node
            .sub_ability
            .as_mut()
            .expect("sub_ability checked above");
    }
    node.sub_ability = Some(Box::new(next));
}

/// CR 101.2 + CR 601.2a: Temporary restrictions can limit which zones affected
/// players may cast spells from.
fn is_blocked_by_cast_only_from_zones(
    state: &GameState,
    obj: &crate::game::game_object::GameObject,
    caster: PlayerId,
) -> bool {
    state
        .restrictions
        .iter()
        .any(|restriction| match restriction {
            GameRestriction::CastOnlyFromZones {
                source,
                affected_players,
                allowed_zones,
                ..
            } => {
                let source_controller = state
                    .objects
                    .get(source)
                    .map(|source_obj| source_obj.controller);
                let caster_affected = match affected_players {
                    RestrictionPlayerScope::AllPlayers => true,
                    RestrictionPlayerScope::SpecificPlayer(player) => *player == caster,
                    RestrictionPlayerScope::OpponentsOfSourceController => {
                        source_controller.is_some_and(|controller| controller != caster)
                    }
                };
                caster_affected && !allowed_zones.contains(&obj.zone)
            }
            GameRestriction::DamagePreventionDisabled { .. } => false,
            GameRestriction::CantCastSpells { .. } => false,
        })
}

/// CR 101.2: Check if a CantCastSpells restriction prevents the given player
/// from casting any spells. E.g., Silence: "Your opponents can't cast spells this turn."
fn is_blocked_by_cant_cast_spells(state: &GameState, caster: PlayerId) -> bool {
    state.restrictions.iter().any(|restriction| {
        let GameRestriction::CantCastSpells {
            source,
            affected_players,
            ..
        } = restriction
        else {
            return false;
        };
        let source_controller = state
            .objects
            .get(source)
            .map(|source_obj| source_obj.controller);
        match affected_players {
            RestrictionPlayerScope::AllPlayers => true,
            RestrictionPlayerScope::SpecificPlayer(player) => *player == caster,
            RestrictionPlayerScope::OpponentsOfSourceController => {
                source_controller.is_some_and(|controller| controller != caster)
            }
        }
    })
}

pub fn spell_objects_available_to_cast(state: &GameState, player: PlayerId) -> Vec<ObjectId> {
    let player_data = state
        .players
        .iter()
        .find(|p| p.id == player)
        .expect("player exists");

    let mut objects: Vec<ObjectId> = player_data.hand.iter().copied().collect();
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
    objects.extend(state.exile.iter().copied().filter(|&obj_id| {
        state.objects.get(&obj_id).is_some_and(|obj| {
            obj.owner == player && has_exile_cast_permission(obj, state.turn_number)
        })
    }));

    // CR 601.2a: Opponent's exiled cards with ExileWithAltCost are castable by any player.
    // CastFromZone effects (e.g. Silent-Blade Oni, Etali) grant these permissions.
    objects.extend(state.exile.iter().copied().filter(|&obj_id| {
        state
            .objects
            .get(&obj_id)
            .is_some_and(|obj| obj.owner != player && has_alt_cost_permission(obj))
    }));

    // CR 702.34 / CR 702.138 / CR 702.180: Cards in graveyard with graveyard-cast keywords.
    // Escape requires enough other graveyard cards to exile; Flashback and Harmonize have no such restriction.
    objects.extend(player_data.graveyard.iter().copied().filter(|&obj_id| {
        state.objects.get(&obj_id).is_some_and(|obj| {
            obj.owner == player
                && has_effective_graveyard_cast_keyword(state, obj_id, obj)
                && (has_harmonize_keyword(obj)
                    || has_flashback_keyword(state, obj_id)
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

    // CR 401.5 + CR 118.9 + CR 601.2a: Top card of library castable via a
    // `TopOfLibraryCastPermission` static (Realmwalker, Future Sight, Bolas's
    // Citadel, Magus of the Future, etc.). Filter is re-evaluated each call
    // because the top changes between priority windows. The card itself stays
    // in `Zone::Library` until `finalize_cast` performs the standard zone-
    // change to `Zone::Stack` — there is NO exile step (CR 601.2a:
    // "moves that card from where it is to the stack").
    if let Some((top_id, _src, _alt)) =
        top_of_library_permission_source(state, player, Some(CardPlayMode::Cast))
    {
        // Only non-land cards reach the cast path; lands flow through the
        // play-land action (`top_of_library_lands_playable_by_permission`).
        if state.objects.get(&top_id).is_some_and(|o| {
            !o.card_types
                .core_types
                .contains(&crate::types::card_type::CoreType::Land)
        }) {
            objects.push(top_id);
        }
    }

    // CR 101.2: If a CantCastSpells restriction blocks this player, no spells are available.
    if is_blocked_by_cant_cast_spells(state, player) {
        return vec![];
    }

    objects
        .into_iter()
        .filter(|obj_id| {
            state
                .objects
                .get(obj_id)
                .is_some_and(|obj| !is_blocked_by_cast_only_from_zones(state, obj, player))
        })
        .collect()
}

/// CR 702.138: Check that the player's graveyard has enough OTHER cards to pay escape's exile cost.
fn graveyard_has_enough_for_escape(
    state: &GameState,
    player: PlayerId,
    escape_obj_id: ObjectId,
) -> bool {
    let exile_count = super::keywords::effective_escape_data(state, escape_obj_id)
        .map(|(_, exile_count)| exile_count);
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

/// CR 702.180: Check if an object has the Harmonize keyword.
fn has_harmonize_keyword(obj: &crate::game::game_object::GameObject) -> bool {
    obj.keywords
        .iter()
        .any(|k| matches!(k, crate::types::keywords::Keyword::Harmonize(_)))
}

/// CR 702.34: Check if an object has the Flashback keyword.
fn has_flashback_keyword(state: &GameState, object_id: ObjectId) -> bool {
    super::keywords::object_has_effective_keyword_kind(state, object_id, KeywordKind::Flashback)
}

// CR 702.34 (Flashback) / CR 702.138 (Escape) / CR 702.180 (Harmonize):
// graveyard-cast alternative costs. Sneak (CR 702.190a) is a HAND-cast
// alt-cost and is deliberately NOT listed here — including it would
// misclassify graveyard objects with a granted Sneak as castable from the
// graveyard, which the rules do not permit.
fn has_effective_graveyard_cast_keyword(
    state: &GameState,
    object_id: ObjectId,
    obj: &crate::game::game_object::GameObject,
) -> bool {
    super::keywords::object_has_effective_keyword_kind(state, object_id, KeywordKind::Escape)
        || obj
            .keywords
            .iter()
            .any(|k| matches!(k, crate::types::keywords::Keyword::Harmonize(_)))
        || has_flashback_keyword(state, object_id)
}

fn upsert_keyword_by_kind(keywords: &mut Vec<Keyword>, keyword: Keyword) {
    if let Some(existing) = keywords
        .iter_mut()
        .find(|existing| existing.kind() == keyword.kind())
    {
        *existing = keyword;
    } else {
        keywords.push(keyword);
    }
}

/// CR 601.2a + CR 603.4: Look up the pre-announcement zone for a spell that
/// is currently mid-cast. `obj.zone` stays at the origin until `finalize_cast`
/// performs the Hand→Stack move itself, but should the ordering ever change
/// this fallback preserves correctness for filters like "spells you cast from
/// exile have convoke" that must evaluate against the pre-announcement zone.
fn pending_cast_origin_zone_for(state: &GameState, object_id: ObjectId) -> Option<Zone> {
    if let Some(pc) = state.waiting_for.pending_cast_ref() {
        if pc.object_id == object_id {
            return Some(pc.origin_zone);
        }
    }
    if let Some(pc) = state.pending_cast.as_ref() {
        if pc.object_id == object_id {
            return Some(pc.origin_zone);
        }
    }
    None
}

fn granted_spell_keywords(
    state: &GameState,
    caster: PlayerId,
    object_id: ObjectId,
) -> Vec<Keyword> {
    let Some(spell_obj) = state.objects.get(&object_id) else {
        return Vec::new();
    };

    let origin_zone = pending_cast_origin_zone_for(state, object_id).unwrap_or(spell_obj.zone);

    let mut keywords = Vec::new();
    // CR 702.26b + CR 604.1: Functioning gate owned by
    // `battlefield_active_statics`; inline `def.condition` check removed.
    for (source_obj, def) in super::functioning_abilities::battlefield_active_statics(state) {
        let StaticMode::CastWithKeyword { keyword } = &def.mode else {
            continue;
        };

        let matches = def.affected.as_ref().is_none_or(|filter| {
            super::filter::spell_object_matches_filter_from(
                spell_obj,
                origin_zone,
                caster,
                filter,
                source_obj.controller,
                &state.all_creature_types,
            )
        });
        if !matches {
            continue;
        }

        upsert_keyword_by_kind(&mut keywords, keyword.clone());
    }

    keywords
}

pub(crate) fn effective_spell_keywords(
    state: &GameState,
    caster: PlayerId,
    object_id: ObjectId,
) -> Vec<Keyword> {
    let Some(obj) = state.objects.get(&object_id) else {
        return Vec::new();
    };

    let mut keywords = obj.keywords.clone();
    for keyword in granted_spell_keywords(state, caster, object_id) {
        upsert_keyword_by_kind(&mut keywords, keyword);
    }

    // CR 702.34a: The flashback keyword is granted while the object isn't on
    // the battlefield. Use the pre-announcement zone so flashback still
    // applies for spells being cast from graveyard even after `finalize_cast`
    // moves them to the stack.
    let effective_origin_zone = pending_cast_origin_zone_for(state, object_id).unwrap_or(obj.zone);
    if effective_origin_zone != Zone::Battlefield
        && super::keywords::object_has_effective_keyword_kind(
            state,
            object_id,
            KeywordKind::Flashback,
        )
    {
        upsert_keyword_by_kind(
            &mut keywords,
            Keyword::Flashback(FlashbackCost::Mana(ManaCost::SelfManaCost)),
        );
    }

    keywords
}

pub(super) fn build_spell_meta(
    state: &GameState,
    caster: PlayerId,
    object_id: ObjectId,
) -> Option<SpellMeta> {
    state.objects.get(&object_id).map(|obj| SpellMeta {
        types: obj
            .card_types
            .core_types
            .iter()
            .map(|ct| format!("{ct:?}"))
            .collect(),
        subtypes: obj.card_types.subtypes.clone(),
        keyword_kinds: effective_spell_keyword_kinds(state, caster, object_id),
        cast_from_zone: Some(pending_cast_origin_zone_for(state, object_id).unwrap_or(obj.zone)),
    })
}

fn effective_spell_keyword_kinds(
    state: &GameState,
    caster: PlayerId,
    object_id: ObjectId,
) -> Vec<KeywordKind> {
    let mut kinds = Vec::new();
    for keyword in effective_spell_keywords(state, caster, object_id) {
        let kind = keyword.kind();
        if !kinds.contains(&kind) {
            kinds.push(kind);
        }
    }

    kinds
}

/// Check if an object has any permission allowing it to be cast from exile.
/// Uses explicit match arms (not `matches!`) so the compiler catches new variants.
fn has_exile_cast_permission(obj: &crate::game::game_object::GameObject, turn_number: u32) -> bool {
    obj.casting_permissions.iter().any(|p| match p {
        crate::types::ability::CastingPermission::AdventureCreature
        | crate::types::ability::CastingPermission::ExileWithAltCost { .. }
        | crate::types::ability::CastingPermission::ExileWithAltAbilityCost { .. }
        | crate::types::ability::CastingPermission::PlayFromExile { .. }
        | crate::types::ability::CastingPermission::ExileWithEnergyCost => true,
        // CR 702.185a: Warp cards only castable after the exile turn ends.
        crate::types::ability::CastingPermission::WarpExile {
            castable_after_turn,
        } => turn_number > *castable_after_turn,
        // CR 702.170d: Plotted cards only castable on a later turn than the
        // one they became plotted on (owner's main phase, empty stack — those
        // conditions are enforced separately by sorcery-speed timing).
        crate::types::ability::CastingPermission::Plotted { turn_plotted } => {
            turn_number > *turn_plotted
        }
    })
}

/// CR 601.2a: Check if an object has an alt-cost cast-from-exile permission
/// (i.e., the spell may be cast by paying *something other than* its mana
/// cost). Used to allow casting opponent's exiled cards (where ownership !=
/// caster). Both the `ManaCost`-only `ExileWithAltCost` and the broader
/// `ExileWithAltAbilityCost` (Nashi, "pay life equal to its mana value")
/// satisfy this predicate.
fn has_alt_cost_permission(obj: &crate::game::game_object::GameObject) -> bool {
    obj.casting_permissions.iter().any(|p| {
        matches!(
            p,
            crate::types::ability::CastingPermission::ExileWithAltCost { .. }
                | crate::types::ability::CastingPermission::ExileWithAltAbilityCost { .. }
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
    let sources: Vec<(ObjectId, &TargetFilter, CastFrequency)> = state
        .battlefield
        .iter()
        .filter_map(|&obj_id| {
            let obj = state.objects.get(&obj_id)?;
            if obj.controller != player {
                return None;
            }
            active_static_definitions(state, obj).find_map(|s| match s.mode {
                StaticMode::GraveyardCastPermission { frequency, .. } => s
                    .affected
                    .as_ref()
                    .map(|filter| (obj_id, filter, frequency)),
                _ => None,
            })
        })
        .collect();

    for (source_id, filter, frequency) in &sources {
        // CR 604.2: Skip if this source's once-per-turn permission was already used
        if *frequency == CastFrequency::OncePerTurn
            && state.graveyard_cast_permissions_used.contains(source_id)
        {
            continue;
        }
        let ctx = super::filter::FilterContext::from_source_with_controller(*source_id, player);
        for &gy_obj_id in &player_data.graveyard {
            if super::filter::matches_target_filter(state, gy_obj_id, filter, &ctx) {
                results.push((gy_obj_id, *source_id));
            }
        }
    }
    results
}

/// CR 601.2a: Find the first valid permission source for a specific graveyard object.
/// Returns (source_id, frequency) so the caller can track per-turn usage.
fn graveyard_permission_source(
    state: &GameState,
    player: PlayerId,
    object_id: ObjectId,
) -> Option<(ObjectId, CastFrequency)> {
    state.battlefield.iter().find_map(|&src_id| {
        let obj = state.objects.get(&src_id)?;
        if obj.controller != player {
            return None;
        }
        let (filter, frequency) =
            active_static_definitions(state, obj).find_map(|s| match s.mode {
                StaticMode::GraveyardCastPermission { frequency, .. } => {
                    s.affected.as_ref().map(|f| (f, frequency))
                }
                _ => None,
            })?;
        // CR 604.2: Skip if this source's once-per-turn permission was already used
        if frequency == CastFrequency::OncePerTurn
            && state.graveyard_cast_permissions_used.contains(&src_id)
        {
            return None;
        }
        if super::filter::matches_target_filter(
            state,
            object_id,
            filter,
            &super::filter::FilterContext::from_source_with_controller(src_id, player),
        ) {
            Some((src_id, frequency))
        } else {
            None
        }
    })
}

/// CR 401.5 + CR 118.9 + CR 601.2a: Find the (single) top card of `player`'s
/// library if a battlefield static grants `TopOfLibraryCastPermission` whose
/// `affected` filter matches it. Returns `(top_card_id, source_id, alt_cost)`.
///
/// Filter eligibility is re-evaluated each call because the top of library
/// changes between priority windows; callers (`spell_objects_available_to_cast`,
/// `prepare_spell_cast`) invoke this fresh each lookup. `play_mode_filter`
/// gates which permissions count: `Some(CardPlayMode::Cast)` for the spell-
/// availability path, `Some(CardPlayMode::Play)` for land plays. `None` lets
/// any mode through.
pub(crate) fn top_of_library_permission_source(
    state: &GameState,
    player: PlayerId,
    play_mode_filter: Option<CardPlayMode>,
) -> Option<(
    ObjectId,
    ObjectId,
    Option<crate::types::ability::AbilityCost>,
)> {
    let player_data = state.players.iter().find(|p| p.id == player)?;
    let &top_id = player_data.library.front()?;
    state.battlefield.iter().find_map(|&src_id| {
        let obj = state.objects.get(&src_id)?;
        if obj.controller != player {
            return None;
        }
        let (filter, alt_cost) =
            active_static_definitions(state, obj).find_map(|s| match &s.mode {
                StaticMode::TopOfLibraryCastPermission {
                    play_mode,
                    alt_cost,
                } => {
                    // Gate by play_mode: Cast permissions cover only spells;
                    // Play permissions cover both lands and non-land spells
                    // (CR 305.1). When the caller specifies a mode, only
                    // permissions matching that mode (or wider) qualify.
                    let mode_matches = match play_mode_filter {
                        None => true,
                        Some(CardPlayMode::Play) => *play_mode == CardPlayMode::Play,
                        Some(CardPlayMode::Cast) => true,
                    };
                    if !mode_matches {
                        return None;
                    }
                    s.affected.as_ref().map(|f| (f, alt_cost.clone()))
                }
                _ => None,
            })?;
        if super::filter::matches_target_filter(
            state,
            top_id,
            filter,
            &super::filter::FilterContext::from_source_with_controller(src_id, player),
        ) {
            Some((top_id, src_id, alt_cost))
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

    let sources: Vec<(ObjectId, &TargetFilter, CastFrequency)> = state
        .battlefield
        .iter()
        .filter_map(|&obj_id| {
            let obj = state.objects.get(&obj_id)?;
            if obj.controller != player {
                return None;
            }
            active_static_definitions(state, obj).find_map(|s| match s.mode {
                StaticMode::GraveyardCastPermission {
                    frequency,
                    play_mode: CardPlayMode::Play,
                } => s
                    .affected
                    .as_ref()
                    .map(|filter| (obj_id, filter, frequency)),
                _ => None,
            })
        })
        .collect();

    for (source_id, filter, frequency) in &sources {
        if *frequency == CastFrequency::OncePerTurn
            && state.graveyard_cast_permissions_used.contains(source_id)
        {
            continue;
        }
        let ctx = super::filter::FilterContext::from_source_with_controller(*source_id, player);
        for &gy_obj_id in &player_data.graveyard {
            if let Some(obj) = state.objects.get(&gy_obj_id) {
                // CR 305.1: Only lands can be "played" (non-land cards require "cast")
                if obj
                    .card_types
                    .core_types
                    .contains(&crate::types::card_type::CoreType::Land)
                    && super::filter::matches_target_filter(state, gy_obj_id, filter, &ctx)
                {
                    results.push((gy_obj_id, *source_id));
                }
            }
        }
    }
    results
}

/// CR 601.2b + CR 118.9a: Find the first `CastFromHandFree` static permission
/// source on the controller's battlefield whose filter admits the given spell.
/// Returns `(source_id, frequency)` so callers can track per-turn usage.
///
/// For `OncePerTurn` sources, the already-used set is consulted; exhausted sources
/// do not qualify. `Unlimited` sources always qualify if their filter matches.
pub(crate) fn hand_cast_free_permission_source(
    state: &GameState,
    player: PlayerId,
    obj: &crate::game::game_object::GameObject,
) -> Option<(ObjectId, CastFrequency)> {
    state.battlefield.iter().find_map(|&src_id| {
        let src_obj = state.objects.get(&src_id)?;
        if src_obj.controller != player {
            return None;
        }
        let (filter, frequency) =
            active_static_definitions(state, src_obj).find_map(|s| match s.mode {
                StaticMode::CastFromHandFree { frequency } => {
                    s.affected.as_ref().map(|f| (f, frequency))
                }
                _ => None,
            })?;
        // CR 601.2b: Skip if this source's once-per-turn slot was already used.
        if frequency == CastFrequency::OncePerTurn
            && state.hand_cast_free_permissions_used.contains(&src_id)
        {
            return None;
        }
        if super::filter::matches_target_filter(
            state,
            obj.id,
            filter,
            &super::filter::FilterContext::from_source_with_controller(src_id, player),
        ) {
            Some((src_id, frequency))
        } else {
            None
        }
    })
}

/// Returns the effective mana cost for casting a spell, after all modifiers
/// (alt costs, commander tax, battlefield reducers, affinity).
/// Returns `None` if the object cannot be cast.
pub fn effective_spell_cost(
    state: &GameState,
    player: PlayerId,
    object_id: ObjectId,
) -> Option<crate::types::mana::ManaCost> {
    prepare_spell_cast(state, player, object_id)
        .ok()
        .map(|p| p.mana_cost)
}

fn prepare_spell_cast(
    state: &GameState,
    player: PlayerId,
    object_id: ObjectId,
) -> Result<PreparedSpellCast, EngineError> {
    prepare_spell_cast_with_variant_override(state, player, object_id, None)
}

/// CR 702.190a: Variant-overriding entry point for cast paths that need a
/// specific `CastingVariant` applied before timing/cost resolution (e.g., Sneak
/// forces declare-blockers timing regardless of the cost the mana-path picked).
fn prepare_spell_cast_with_variant_override(
    state: &GameState,
    player: PlayerId,
    object_id: ObjectId,
    variant_override: Option<CastingVariant>,
) -> Result<PreparedSpellCast, EngineError> {
    let obj = state
        .objects
        .get(&object_id)
        .ok_or_else(|| EngineError::InvalidAction("Object not found".to_string()))?;
    // CR 715.3d: Cards in exile with AdventureCreature or ExileWithAltCost permission.
    let has_exile_permission =
        obj.zone == Zone::Exile && has_exile_cast_permission(obj, state.turn_number);
    let has_madness = obj.zone == Zone::Exile
        && matches!(variant_override, Some(CastingVariant::Madness))
        && obj.owner == player
        && obj
            .keywords
            .iter()
            .any(|k| matches!(k, crate::types::keywords::Keyword::Madness(_)));
    // CR 702.34 / CR 702.138 / CR 702.180: Cards in graveyard with graveyard-cast keywords.
    let has_escape = obj.zone == Zone::Graveyard
        && super::keywords::object_has_effective_keyword_kind(
            state,
            object_id,
            KeywordKind::Escape,
        );
    let has_graveyard_cast_keyword =
        obj.zone == Zone::Graveyard && has_effective_graveyard_cast_keyword(state, object_id, obj);
    // CR 601.2a + CR 117.1c: Graveyard cast via static permission (Lurrus, etc.).
    let graveyard_permission_src = if obj.zone == Zone::Graveyard && state.active_player == player {
        graveyard_permission_source(state, player, object_id)
    } else {
        None
    };
    let has_graveyard_permission = graveyard_permission_src.is_some();

    // CR 401.5 + CR 118.9 + CR 601.2a: Top-of-library cast via static permission
    // (Realmwalker, Future Sight, Bolas's Citadel, etc.). The card must be the
    // current top of `player`'s library AND match the static's `affected`
    // filter. The optional `alt_cost` flows through to `prepare_spell_cast`'s
    // alt-cost branch below, mirroring `ExileWithAltAbilityCost` semantics.
    let top_of_library_permission_src = if obj.zone == Zone::Library && obj.owner == player {
        top_of_library_permission_source(state, player, Some(CardPlayMode::Cast))
            .filter(|(top_id, _, _)| *top_id == object_id)
    } else {
        None
    };
    let has_top_of_library_permission = top_of_library_permission_src.is_some();

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
                || has_madness
                || has_exile_permission
                || has_graveyard_cast_keyword
                || has_graveyard_permission
                || has_top_of_library_permission));
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

    // CR 101.2: Temporary blanket prohibition — "can't cast spells this turn."
    // E.g., Silence: "Your opponents can't cast spells this turn."
    if is_blocked_by_cant_cast_spells(state, player) {
        return Err(EngineError::ActionNotAllowed(
            "A temporary effect prevents you from casting spells this turn".to_string(),
        ));
    }

    // CR 101.2: Blanket casting prohibition — "you can't cast [type] spells."
    // E.g., Steel Golem: "You can't cast creature spells."
    if is_blocked_by_cant_be_cast(state, player, obj) {
        return Err(EngineError::ActionNotAllowed(
            "A static ability prevents you from casting this spell".to_string(),
        ));
    }

    if is_blocked_by_cast_only_from_zones(state, obj, player) {
        return Err(EngineError::ActionNotAllowed(
            "A temporary effect prevents casting from this zone".to_string(),
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

    // CR 101.2 + CR 604.1: Per-turn casting limit — "can't cast more than N spells each turn."
    // E.g., Rule of Law, High Noon, Deafening Silence.
    if is_blocked_by_per_turn_cast_limit(state, player, obj) {
        return Err(EngineError::ActionNotAllowed(
            "A static ability limits the number of spells you can cast this turn".to_string(),
        ));
    }

    // Only Spell-kind abilities define the spell's on-cast effect and targets.
    // Activated abilities are irrelevant when casting the permanent spell.
    let ability_def = combined_spell_ability_def(obj);

    let flash_cost = restrictions::flash_timing_cost(state, player, obj);
    // ExileWithAltCost / ExileWithAltAbilityCost: override mana cost when
    // casting from exile via an alt-cost permission. The non-mana branch
    // (ExileWithAltAbilityCost) zeroes the mana cost — its `AbilityCost` is
    // routed through `pay_additional_cost` in `check_additional_cost_or_pay`
    // (CR 118.9 + CR 119.4).
    let alt_cost_from_exile = if obj.zone == Zone::Exile {
        obj.casting_permissions.iter().find_map(|p| match p {
            crate::types::ability::CastingPermission::ExileWithAltCost { cost, .. } => {
                Some(cost.clone())
            }
            crate::types::ability::CastingPermission::ExileWithAltAbilityCost { .. } => {
                Some(crate::types::mana::ManaCost::zero())
            }
            _ => None,
        })
    } else if obj.zone == Zone::Library
        && top_of_library_permission_src
            .as_ref()
            .is_some_and(|(_, _, alt)| alt.is_some())
    {
        // CR 401.5 + CR 118.9: Bolas's Citadel — alt-cost rider on the static
        // grant zeros the spell's mana cost; the `AbilityCost` body is paid
        // by `check_additional_cost_or_pay`'s top-of-library branch.
        Some(crate::types::mana::ManaCost::zero())
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
        super::keywords::effective_escape_data(state, object_id).map(|(cost, _)| cost)
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

    // CR 702.34a: Flashback — use flashback cost when casting from graveyard.
    let flashback_cost = if obj.zone == Zone::Graveyard {
        super::keywords::effective_flashback_cost(state, object_id)
    } else {
        None
    };

    // CR 702.190a: Sneak alt-cost when casting from HAND. The
    // `effective_sneak_cost` lookup goes through `effective_keyword_for_object`
    // so off-zone keyword grants (e.g., statics that grant Sneak to cards in
    // your hand) are visible. Sneak is NOT auto-selected as the active
    // `casting_variant` — it is opted into explicitly by
    // `handle_cast_spell_as_sneak` via `variant_override`, which enforces
    // declare-blockers timing (CR 702.190a), returns the unblocked attacker
    // as cost payment, and — for permanent spells only (CR 702.190b) —
    // places the permanent tapped+attacking on resolution.
    let sneak_cost = if obj.zone == Zone::Hand {
        super::keywords::effective_sneak_cost(state, object_id)
    } else {
        None
    };

    // CR 702.34a + CR 118.8 + CR 601.2f: Split flashback into mana vs non-mana
    // components for the payment pipeline. Compound flashback costs
    // ("Flashback—{1}{U}, Pay 3 life") are stored as
    // `FlashbackCost::NonMana(AbilityCost::Composite([Mana, ...]))`; we extract
    // the mana sub-cost so the spell pays its mana through the normal mana-payment
    // flow while the residual non-mana sub-costs are routed through
    // `pay_additional_cost`. Mirrors `extract_x_mana_cost` (casting_costs.rs).
    let (flashback_mana_cost, flashback_non_mana_cost) =
        split_flashback_cost_components(flashback_cost.as_ref());

    // Precedence: Escape > Harmonize > Flashback > GraveyardPermission > Warp > Normal.
    // No standard card has multiple graveyard-cast keywords; if one did, the card's own
    // keyword overrides an external source's grant (GraveyardPermission).
    //
    // CR 702.190a: Sneak is not auto-selected from the keyword-presence chain —
    // it is opted into explicitly via `variant_override` by the
    // `handle_cast_spell_as_sneak` entry point. This preserves Sneak's
    // permission-aware eligibility (the HasKeywordKind filter on the granting
    // rider) while keeping the default cast path for GY creatures under
    // GraveyardCastPermission unchanged.
    // CR 702.62a: Suspend free-cast detection — when casting an exile-zone card
    // that has `Keyword::Suspend` AND an `ExileWithAltCost` permission (granted
    // by the synthesized last-counter trigger via `Effect::CastFromZone`), the
    // cast is the suspend "play it without paying its mana cost" path. Mirrors
    // Warp/Flashback's keyword-presence detection and avoids coupling
    // `Effect::CastFromZone` to a cast-variant override field.
    let is_suspend_cast = obj.zone == Zone::Exile
        && alt_cost_from_exile.is_some()
        && obj
            .keywords
            .iter()
            .any(|k| matches!(k, crate::types::keywords::Keyword::Suspend { .. }));

    // CR 702.170d: Plot free-cast detection — when casting an exile-zone card
    // with a `CastingPermission::Plotted { turn_plotted }` (on a later turn
    // than it was plotted), the cast is the plot "without paying its mana
    // cost" path. Mirrors `is_suspend_cast` — permission-keyed, no separate
    // keyword-presence check (Plot is a hand-zone activated ability; once the
    // card is in exile with the Plotted permission, the keyword's job is done).
    let is_plot_cast = obj.zone == Zone::Exile
        && obj
            .casting_permissions
            .iter()
            .any(|p| matches!(p, crate::types::ability::CastingPermission::Plotted { .. }));

    let casting_variant = variant_override.unwrap_or_else(|| {
        if is_suspend_cast {
            CastingVariant::Suspend
        } else if is_plot_cast {
            CastingVariant::Plot
        } else if escape_cost.is_some() {
            CastingVariant::Escape
        } else if harmonize_cost.is_some() {
            CastingVariant::Harmonize
        } else if flashback_cost.is_some() {
            CastingVariant::Flashback
        } else if let Some((source, frequency)) = graveyard_permission_src {
            CastingVariant::GraveyardPermission { source, frequency }
        } else if warp_cost.is_some() {
            CastingVariant::Warp
        } else {
            CastingVariant::Normal
        }
    });
    // CR 702.96a: When the caller explicitly opted into Overload (via
    // `variant_override = Some(CastingVariant::Overload)`), substitute the
    // overload mana cost taken from the hand object's `Keyword::Overload(cost)`
    // payload. Mirrors the Evoke/Warp cost-selection pattern below.
    let overload_cost = if casting_variant == CastingVariant::Overload {
        obj.keywords.iter().find_map(|k| match k {
            crate::types::keywords::Keyword::Overload(cost) => Some(cost.clone()),
            _ => None,
        })
    } else {
        None
    };
    // CR 702.74a: When the caller explicitly opted into Evoke (via
    // `variant_override = Some(CastingVariant::Evoke)`), substitute the evoke
    // mana cost taken from the hand object's `Keyword::Evoke(cost)` payload.
    let evoke_cost = if casting_variant == CastingVariant::Evoke {
        obj.keywords.iter().find_map(|k| match k {
            crate::types::keywords::Keyword::Evoke(cost) => Some(cost.clone()),
            _ => None,
        })
    } else {
        None
    };
    // CR 601.2b + CR 118.9a: CastFromHandFree — static permission grants free
    // casting from hand. Auto-application is restricted to `Unlimited` sources
    // (Omniscience, Tamiyo emblem); `OncePerTurn` sources (Zaffai) must be opted
    // into explicitly via a dedicated action to preserve the player's "may cast"
    // choice and make per-turn slot consumption visible at the action layer.
    let hand_cast_free = obj.zone == Zone::Hand
        && !matches!(casting_variant, CastingVariant::HandPermission { .. })
        && hand_cast_free_permission_source(state, player, obj)
            .is_some_and(|(_, frequency)| frequency == CastFrequency::Unlimited);

    // CR 118.9: Energy replaces mana cost entirely when casting with ExileWithEnergyCost.
    // CR 702.34a: Non-mana flashback costs use NoCost for mana (cost is paid separately).
    // CR 702.190a: sneak_cost only applies when the caster actually elected
    // the Sneak path (variant_override == Some(Sneak{..})). Otherwise a GY
    // creature with Sneak available plus another permission (e.g. Lurrus)
    // would erroneously use the Sneak cost for a non-Sneak cast.
    let effective_sneak_cost_for_path = if matches!(casting_variant, CastingVariant::Sneak { .. }) {
        sneak_cost
    } else {
        None
    };
    // CR 601.2b: HandPermission variant (A2 opt-in path for Zaffai) also pays
    // no mana cost — the granting static replaces the mana cost with nothing.
    let is_hand_permission_variant =
        matches!(casting_variant, CastingVariant::HandPermission { .. });
    // CR 702.94a: Miracle alternative cost — pulled from `Keyword::Miracle(cost)`
    // on the hand object. Only honored when the caller explicitly opted into the
    // Miracle variant via the reveal prompt.
    let miracle_cost = if casting_variant == CastingVariant::Miracle {
        obj.keywords.iter().find_map(|k| match k {
            crate::types::keywords::Keyword::Miracle(cost) => Some(cost.clone()),
            _ => None,
        })
    } else {
        None
    };
    let madness_cost = if casting_variant == CastingVariant::Madness {
        obj.keywords.iter().find_map(|k| match k {
            crate::types::keywords::Keyword::Madness(cost) => Some(cost.clone()),
            _ => None,
        })
    } else {
        None
    };
    // CR 702.34a: When the flashback cost is purely non-mana (e.g. Battle Screech's
    // "tap three white creatures"), the spell pays no mana through the normal flow.
    // For compound flashback costs ("{1}{U}, Pay 3 life") we still want the mana
    // sub-cost paid normally — `flashback_mana_cost` is `Some` in that case and is
    // selected by the `else` branch below.
    let pure_non_mana_flashback =
        flashback_non_mana_cost.is_some() && flashback_mana_cost.is_none();
    // CR 702.170d: Plot casts are always free — the Plotted permission encodes
    // "without paying its mana cost". Zero the mana cost at preparation time,
    // mirroring the hand-free / flashback-non-mana paths above.
    let mut mana_cost = if energy_cost_from_exile
        || hand_cast_free
        || is_hand_permission_variant
        || pure_non_mana_flashback
        || is_plot_cast
    {
        crate::types::mana::ManaCost::NoCost
    } else {
        miracle_cost
            .or(madness_cost)
            .or(evoke_cost)
            .or(overload_cost)
            .or(escape_cost)
            .or(harmonize_cost)
            .or(flashback_mana_cost)
            .or(effective_sneak_cost_for_path)
            .or(alt_cost_from_exile)
            .or(warp_cost)
            .unwrap_or_else(|| obj.mana_cost.clone())
    };
    let has_granted_flash =
        effective_spell_keyword_kinds(state, player, object_id).contains(&KeywordKind::Flash);
    // CR 304.1: Instants can be cast any time a player has priority.
    // CR 301.1 / CR 306.1: Artifacts and planeswalkers are cast at sorcery speed.
    if let Err(base_timing_error) = restrictions::check_spell_timing(
        state,
        player,
        obj,
        ability_def.as_ref(),
        has_granted_flash,
        casting_variant,
    ) {
        // CR 702.8a: Flash permits instant-speed casting.
        let Some(flash_cost) = flash_cost else {
            return Err(base_timing_error);
        };
        restrictions::check_spell_timing(
            state,
            player,
            obj,
            ability_def.as_ref(),
            true,
            casting_variant,
        )?;
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

    // CR 117.7 + CR 601.2f: Apply self-spell cost modifications — statics printed on
    // the spell itself ("This spell costs {N} less to cast ...") with `active_zones`
    // covering Hand/Stack and `affected = SelfRef`. These cannot be found by the
    // battlefield scanner below because the card is not on the battlefield.
    apply_self_spell_cost_modifiers(state, player, object_id, &mut mana_cost);

    // CR 601.2f: Apply battlefield-based cost modifications (ReduceCost/RaiseCost statics).
    // This runs after self-cost reduction (CostReduction on the spell itself) and commander tax.
    apply_battlefield_cost_modifiers(state, player, object_id, &mut mana_cost);

    // CR 702.41a: Affinity — reduce cost by {1} for each matching permanent controlled.
    apply_affinity_reduction(state, player, object_id, &mut mana_cost);

    // CR 601.2f: Apply one-shot pending cost reductions ("the next spell costs {N} less").
    apply_pending_spell_cost_reductions(state, player, object_id, &mut mana_cost);

    // CR 702.96b-c: When casting with Overload, transform the spell's ability
    // tree so every target-bearing effect is promoted to its all-matching
    // counterpart (Destroy→DestroyAll, Pump→PumpAll, DealDamage→DamageAll,
    // Tap→TapAll, Bounce→ChangeZoneAll). The transformed effects carry no
    // TargetRef slots, so target selection is naturally skipped (CR 702.96c).
    let mut ability_def = ability_def;
    if casting_variant == CastingVariant::Overload {
        if let Some(def) = ability_def.as_mut() {
            super::effects::overload::transform_ability_def(def);
        }
    }

    let origin_zone = obj.zone;
    Ok(PreparedSpellCast {
        object_id,
        card_id: obj.card_id,
        ability_def,
        mana_cost,
        modal: obj.modal.clone(),
        casting_variant,
        origin_zone,
    })
}

/// CR 117.7 + CR 601.2f: Apply self-spell cost modifications — `ReduceCost` / `RaiseCost`
/// statics printed on the spell being cast, with `affected = SelfRef` and `active_zones`
/// covering the card's current zone (Hand for normal casting, Stack for the cost-
/// determination step). Handles cards like Tolarian Terror where the cost reduction is
/// inherent to the spell and must apply before the spell resolves.
fn apply_self_spell_cost_modifiers(
    state: &GameState,
    caster: PlayerId,
    spell_id: ObjectId,
    mana_cost: &mut ManaCost,
) {
    let Some(spell_obj) = state.objects.get(&spell_id) else {
        return;
    };

    // CR 113.6 + CR 604.1: A static ability only functions in zones listed by
    // `active_zones`; battlefield-default (empty) statics do not apply here.
    // We iterate the spell's own static definitions without running the layer
    // pipeline: layers pre-compute battlefield characteristics, not cast-time
    // cost deltas on cards in hand.
    for def in spell_obj.static_definitions.iter_all() {
        if def.active_zones.is_empty() {
            continue;
        }
        if !def.active_zones.contains(&spell_obj.zone) {
            continue;
        }
        // CR 117.7: Only self-referential cost statics apply here. Any other
        // `affected` scoping would indicate a battlefield-style static that
        // should be handled by the battlefield scanner.
        if !matches!(def.affected, Some(TargetFilter::SelfRef)) {
            continue;
        }

        let (amount, dynamic_count, is_raise) = match &def.mode {
            StaticMode::ReduceCost {
                amount,
                dynamic_count,
                ..
            } => (amount, dynamic_count, false),
            StaticMode::RaiseCost {
                amount,
                dynamic_count,
                ..
            } => (amount, dynamic_count, true),
            _ => continue,
        };

        // CR 604.1: Evaluate any trailing condition ("if you control a Wizard").
        if let Some(ref cond) = def.condition {
            if !super::layers::evaluate_condition(state, cond, caster, spell_id) {
                continue;
            }
        }

        // CR 601.2f: Resolve the dynamic multiplier (e.g., "for each instant or
        // sorcery card in your graveyard"). Static amount with no multiplier = 1.
        let multiplier = if let Some(ref qty_ref) = dynamic_count {
            let qty_expr = crate::types::ability::QuantityExpr::Ref {
                qty: qty_ref.clone(),
            };
            super::quantity::resolve_quantity(state, &qty_expr, caster, spell_id).max(0) as u32
        } else {
            1
        };

        apply_cost_mod_to_mana(mana_cost, amount, multiplier, is_raise);
    }
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

    // CR 702.26b + CR 114.4: Functioning gate (phased-out / command-zone) owned
    // by `battlefield_functioning_statics`. We deliberately use the non-
    // condition-filtered helper here — CR 604.1 condition evaluation must run
    // against `caster` (so `SpellsCastThisTurn`-style conditions resolve against
    // the casting player's history), not against the static's controller. The
    // inline `evaluate_condition(... caster, ...)` call below does that work.
    for (bf_obj, def) in super::functioning_abilities::battlefield_functioning_statics(state) {
        let bf_id = bf_obj.id;
        let source_controller = bf_obj.controller;

        {
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

            // CR 601.2f: Check player scope — does this modifier apply to spells the caster casts?
            // Must run before condition check so QuantityComparison resolves against the caster.
            if let Some(TargetFilter::Typed(ref tf)) = def.affected {
                match tf.controller {
                    Some(ControllerRef::You) if caster != source_controller => continue,
                    Some(ControllerRef::Opponent) if caster == source_controller => continue,
                    _ => {} // No controller restriction or matches
                }
            }

            // CR 601.2f: Check static condition — "as long as" clauses gate cost modification.
            // Uses `caster` so SpellsCastThisTurn resolves against the casting player's history.
            if let Some(ref cond) = def.condition {
                if !super::layers::evaluate_condition(state, cond, caster, bf_id) {
                    continue;
                }
            }

            // CR 601.2f: Check spell type filter — does the spell match?
            if let Some(ref filter) = spell_filter {
                if !spell_matches_cost_filter(state, caster, spell_id, filter, bf_id) {
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
    caster: PlayerId,
    spell_id: ObjectId,
    filter: &TargetFilter,
    source_id: ObjectId,
) -> bool {
    let Some(spell_obj) = state.objects.get(&spell_id) else {
        return false;
    };
    let Some(source_obj) = state.objects.get(&source_id) else {
        return false;
    };

    match filter {
        TargetFilter::Typed(_) => super::filter::spell_object_matches_filter(
            spell_obj,
            caster,
            filter,
            source_obj.controller,
            &state.all_creature_types,
        ),
        TargetFilter::Or { filters } => filters.iter().any(|f| {
            super::filter::spell_object_matches_filter(
                spell_obj,
                caster,
                f,
                source_obj.controller,
                &state.all_creature_types,
            )
        }),
        // CR 601.2e: Cost modifications only apply when the filter explicitly matches.
        // Fail-closed: unrecognized filter shapes do not universally reduce costs.
        _ => false,
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

/// CR 702.41a: Apply Affinity cost reduction from the spell's own keywords.
///
/// For each `Keyword::Affinity(type_filter)` on the spell, counts matching
/// permanents on the battlefield controlled by the caster and reduces the
/// spell's generic mana cost by that count (floor at 0).
/// CR 702.41b: Multiple Affinity instances each apply separately.
fn apply_affinity_reduction(
    state: &GameState,
    caster: PlayerId,
    spell_id: ObjectId,
    mana_cost: &mut ManaCost,
) {
    if !state.objects.contains_key(&spell_id) {
        return;
    }
    for kw in effective_spell_keywords(state, caster, spell_id) {
        if let Keyword::Affinity(ref type_filter) = kw {
            let filter = TargetFilter::Typed(type_filter.clone());
            let ctx = super::filter::FilterContext::from_source(state, spell_id);
            let count = state
                .battlefield
                .iter()
                .filter(|&&id| {
                    let Some(obj) = state.objects.get(&id) else {
                        return false;
                    };
                    obj.controller == caster
                        && super::filter::matches_target_filter(state, id, &filter, &ctx)
                })
                .count() as u32;
            apply_cost_mod_to_mana(mana_cost, &ManaCost::generic(1), count, false);
        }
    }
}

/// CR 601.2f: Apply one-shot pending cost reductions (read-only during cost calculation).
/// The matching entry is consumed later in `consume_pending_spell_cost_reduction`.
fn apply_pending_spell_cost_reductions(
    state: &GameState,
    caster: PlayerId,
    spell_id: ObjectId,
    mana_cost: &mut ManaCost,
) {
    for r in &state.pending_spell_cost_reductions {
        if r.player != caster {
            continue;
        }
        let matches = match &r.spell_filter {
            None => true,
            Some(filter) => spell_matches_cost_filter(state, caster, spell_id, filter, spell_id),
        };
        if matches {
            apply_cost_mod_to_mana(mana_cost, &ManaCost::generic(1), r.amount, false);
            break; // Only apply the first matching reduction
        }
    }
}

/// CR 601.2f: Consume (remove) a one-shot pending cost reduction after a spell is cast.
pub(super) fn consume_pending_spell_cost_reduction(state: &mut GameState, caster: PlayerId) {
    if let Some(idx) = state
        .pending_spell_cost_reductions
        .iter()
        .position(|r| r.player == caster && r.spell_filter.is_none())
    {
        state.pending_spell_cost_reductions.remove(idx);
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
    if creature {
        // Creature face is just a normal creature spell — delegate to the standard
        // cast pipeline so vanilla creature faces (no spell ability), modal cards,
        // X costs, and other shared casting features all work uniformly. Mirrors
        // the Warp/Overload "cast normally" pattern.
        return continue_cast_from_prepared(state, player, object_id, events);
    }

    // CR 715.3a: Swap to Adventure face characteristics
    if let Some(obj) = state.objects.get_mut(&object_id) {
        swap_to_adventure_face(obj);
    }

    let prepared = prepare_spell_cast(state, player, object_id)?;

    // CR 601.2a + CR 715.3a: Announce the Adventure spell onto the stack before
    // mode/target/cost processing. The Adventure path bypasses
    // continue_with_prepared so it must announce explicitly.
    announce_spell_on_stack(state, player, &prepared, events);

    // The Adventure face is always an instant or sorcery, so it always has a
    // spell ability_def (synthesized from its Oracle text).
    let ability_def = prepared
        .ability_def
        .as_ref()
        .expect("adventure spell face must have ability_def");

    let resolved = {
        let mut r = ResolvedAbility::new(
            *ability_def.effect.clone(),
            Vec::new(),
            prepared.object_id,
            player,
        );
        if let Some(sub) = &ability_def.sub_ability {
            r = r.sub_ability(build_resolved_from_def(sub, prepared.object_id, player));
        }
        if let Some(c) = ability_def.condition.clone() {
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
        if let Some(targets) =
            auto_select_targets_for_ability(state, &resolved, &target_slots, &[])?
        {
            let mut resolved = resolved;
            assign_targets_in_chain(&mut resolved, &targets)?;
            return pay_and_push_adventure(
                state,
                player,
                prepared.object_id,
                prepared.card_id,
                resolved,
                &prepared.mana_cost,
                CastingVariant::Adventure,
                None,
                prepared.origin_zone,
                events,
            );
        }

        let selection = begin_target_selection_for_ability(state, &resolved, &target_slots, &[])?;
        let mut pending_adv = PendingCast::new(
            prepared.object_id,
            prepared.card_id,
            resolved,
            prepared.mana_cost.clone(),
        );
        // CR 715.3a: Preserve Adventure casting variant so the spell resolves to exile.
        // prepare_spell_cast always returns CastingVariant::Normal — override here.
        pending_adv.casting_variant = CastingVariant::Adventure;
        pending_adv.distribute = ability_def.distribute.clone();
        pending_adv.origin_zone = prepared.origin_zone;
        return Ok(WaitingFor::TargetSelection {
            player,
            pending_cast: Box::new(pending_adv),
            target_slots,
            selection,
        });
    }

    // No targets -- proceed to payment.
    pay_and_push_adventure(
        state,
        player,
        prepared.object_id,
        prepared.card_id,
        resolved,
        &prepared.mana_cost,
        CastingVariant::Adventure,
        None,
        prepared.origin_zone,
        events,
    )
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

/// CR 702.96a: Handle Overload cost choice and proceed with casting. When
/// `use_overload` is true, the cast is prepared with `CastingVariant::Overload`
/// — the overload mana cost substitutes for the printed cost and the spell's
/// ability tree is transformed (target → each, CR 702.96b-c). When false, the
/// cast proceeds normally (no variant override → `Normal`).
pub fn handle_overload_cost_choice(
    state: &mut GameState,
    player: PlayerId,
    object_id: ObjectId,
    _card_id: CardId,
    use_overload: bool,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    if use_overload {
        let prepared = prepare_spell_cast_with_variant_override(
            state,
            player,
            object_id,
            Some(CastingVariant::Overload),
        )?;
        return continue_with_prepared(state, player, prepared, events);
    }
    continue_cast_from_prepared(state, player, object_id, events)
}

/// CR 702.74a: Handle Evoke cost choice and proceed with casting. When
/// `use_evoke` is true, the cast is prepared with `CastingVariant::Evoke`
/// (which substitutes the evoke mana cost for the printed mana cost). When
/// false, the cast proceeds normally (no variant override → `Normal`).
pub fn handle_evoke_cost_choice(
    state: &mut GameState,
    player: PlayerId,
    object_id: ObjectId,
    _card_id: CardId,
    use_evoke: bool,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    if use_evoke {
        let prepared = prepare_spell_cast_with_variant_override(
            state,
            player,
            object_id,
            Some(CastingVariant::Evoke),
        )?;
        return continue_with_prepared(state, player, prepared, events);
    }
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

/// CR 702.190a + b: Cast a spell from HAND via the Sneak alternative cost.
///
/// Per CR 702.190a, "Sneak [cost]" reads: "Any time you could cast an instant
/// during your declare blockers step, you may cast this spell by paying
/// [cost] and returning an unblocked creature you control to its owner's
/// hand rather than paying this spell's mana cost." This applies to any card
/// type — creature, artifact, enchantment, planeswalker, sorcery, or instant.
///
/// Validates:
/// - `hand_object` is in `player`'s hand and matches `card_id`.
/// - `hand_object` has an effective Sneak cost (printed keyword or rider-
///   granted, via `effective_sneak_cost`).
/// - `creature_to_return` is an unblocked attacker controlled by `player`.
///
/// Builds a `CastingVariant::Sneak { returned_creature, placement }` override
/// where `placement` is `Some(SneakPlacement { .. })` only for permanent
/// spells (CR 702.190b) — instants and sorceries carry `None` and resolve
/// normally without an alongside-attacker placement.
///
/// Routes through the standard casting pipeline. `prepare_spell_cast_with_
/// variant_override` enforces declare-blockers timing (`restrictions.rs`) and
/// selects the Sneak mana cost. The returned creature is bounced to its
/// owner's hand at `finalize_cast_to_stack` (`casting_costs.rs`) as part of
/// paying the Sneak cost.
pub fn handle_cast_spell_as_sneak(
    state: &mut GameState,
    player: PlayerId,
    hand_object: ObjectId,
    card_id: CardId,
    creature_to_return: ObjectId,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    // Sanity: object exists, matches card_id, and is in the caster's hand.
    // CR 702.190a: Sneak is a hand-cast alt-cost; graveyard/exile casts are
    // not legal under this keyword.
    let obj = state.objects.get(&hand_object).ok_or_else(|| {
        EngineError::InvalidAction(format!("Object {hand_object:?} does not exist"))
    })?;
    if obj.card_id != card_id {
        return Err(EngineError::InvalidAction(format!(
            "Object {hand_object:?} does not match card_id {card_id:?}",
        )));
    }
    if obj.zone != Zone::Hand || obj.owner != player {
        return Err(EngineError::ActionNotAllowed(
            "Sneak-cast requires a hand card owned by the caster".to_string(),
        ));
    }

    // CR 702.190a: Must have an effective Sneak cost (intrinsic or granted).
    if super::keywords::effective_sneak_cost(state, hand_object).is_none() {
        return Err(EngineError::ActionNotAllowed(
            "Card has no Sneak permission".to_string(),
        ));
    }

    // CR 702.190b: Capture placement data from the returned creature's
    // `AttackerInfo` only for permanent spells — CR 702.190b applies only to
    // "a permanent spell whose sneak cost was paid" (CR 110.4b). Non-permanent
    // spells (instants/sorceries) resolve normally with no alongside-attacker
    // step. Delegates to the shared `stack::is_permanent_spell` helper so the
    // CR 110.4b definition lives in one place.
    let is_permanent_spell = super::stack::is_permanent_spell(state, hand_object);

    // CR 702.190a: The returned creature must be an unblocked attacker
    // controlled by `player`.
    let combat = state
        .combat
        .as_ref()
        .ok_or_else(|| EngineError::ActionNotAllowed("No active combat".to_string()))?;
    let attacker_info = combat
        .attackers
        .iter()
        .find(|a| a.object_id == creature_to_return)
        .cloned()
        .ok_or_else(|| {
            EngineError::ActionNotAllowed("Creature to return is not an attacker".to_string())
        })?;
    let is_blocked = combat
        .blocker_assignments
        .get(&creature_to_return)
        .is_some_and(|blockers| !blockers.is_empty());
    if is_blocked {
        return Err(EngineError::ActionNotAllowed(
            "Attacker is blocked".to_string(),
        ));
    }
    let returned_obj = state
        .objects
        .get(&creature_to_return)
        .ok_or_else(|| EngineError::InvalidAction("Creature to return not found".to_string()))?;
    if returned_obj.controller != player {
        return Err(EngineError::ActionNotAllowed(
            "You don't control that creature".to_string(),
        ));
    }

    let placement = if is_permanent_spell {
        Some(SneakPlacement {
            defender: attacker_info.defending_player,
            attack_target: attacker_info.attack_target,
        })
    } else {
        None
    };
    let variant = CastingVariant::Sneak {
        returned_creature: creature_to_return,
        placement,
    };

    let prepared =
        prepare_spell_cast_with_variant_override(state, player, hand_object, Some(variant))?;
    continue_with_prepared(state, player, prepared, events)
}

/// CR 601.2b + CR 118.9a: Cast a spell from hand for free via a
/// `StaticMode::CastFromHandFree` permission source (Zaffai).
///
/// Validates:
/// - `object_id` is in the caster's hand and matches `card_id`.
/// - `source_id` controls an active `CastFromHandFree` static whose filter
///   matches `object_id`, and its once-per-turn slot (when applicable) has
///   not been consumed this turn.
///
/// Builds a `CastingVariant::HandPermission { source, frequency }` override and
/// routes through the standard casting pipeline. On finalize-to-stack,
/// `casting_costs.rs` records `source_id` in `hand_cast_free_permissions_used`
/// for `OncePerTurn` frequencies.
///
/// Omniscience's `Unlimited` silent path is NOT routed through here — it uses
/// `GameAction::CastSpell` with `CastingVariant::Normal` and a `NoCost`
/// short-circuit. This entry point is reserved for the opt-in choice surface.
pub fn handle_cast_spell_for_free(
    state: &mut GameState,
    player: PlayerId,
    object_id: ObjectId,
    card_id: CardId,
    source_id: ObjectId,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    let obj = state
        .objects
        .get(&object_id)
        .ok_or_else(|| EngineError::InvalidAction("Object not found".to_string()))?;
    if obj.card_id != card_id {
        return Err(EngineError::InvalidAction(format!(
            "Object {object_id:?} does not match card_id {card_id:?}"
        )));
    }
    // CR 601.2b: Spell must be in the caster's hand.
    if obj.zone != Zone::Hand || obj.owner != player {
        return Err(EngineError::ActionNotAllowed(
            "CastSpellForFree requires a hand card owned by the caster".to_string(),
        ));
    }
    // CR 601.2b + CR 400.7: The granting source's permission must be active and
    // filter-matched. `hand_cast_free_permission_source` also enforces that any
    // `OncePerTurn` slot has not already been consumed this turn.
    let (matched_source, frequency) = hand_cast_free_permission_source(state, player, obj)
        .ok_or_else(|| {
            EngineError::ActionNotAllowed(
                "No CastFromHandFree permission source admits this spell".to_string(),
            )
        })?;
    if matched_source != source_id {
        return Err(EngineError::ActionNotAllowed(
            "Named source is not the permission grantor for this spell".to_string(),
        ));
    }
    let variant = CastingVariant::HandPermission {
        source: source_id,
        frequency,
    };
    let prepared =
        prepare_spell_cast_with_variant_override(state, player, object_id, Some(variant))?;
    continue_with_prepared(state, player, prepared, events)
}

/// CR 702.94a + CR 603.11: Cast a spell from hand via its Miracle alternative
/// mana cost after the player accepted the reveal prompt. Validates:
/// - `object_id` matches `card_id` and is in the caster's hand.
/// - The card still has `Keyword::Miracle(cost)` (layer effects between queue
///   and accept may have removed it — in that case the cast fails cleanly).
///
/// Builds a `CastingVariant::Miracle` override and routes through the shared
/// casting pipeline; `prepare_spell_cast_with_variant_override` substitutes
/// the miracle cost for the printed mana cost via the `Keyword::Miracle`
/// payload it discovers on the object.
pub fn handle_cast_spell_as_miracle(
    state: &mut GameState,
    player: PlayerId,
    object_id: ObjectId,
    card_id: CardId,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    let obj = state
        .objects
        .get(&object_id)
        .ok_or_else(|| EngineError::InvalidAction("Object not found".to_string()))?;
    if obj.card_id != card_id {
        return Err(EngineError::InvalidAction(format!(
            "Object {object_id:?} does not match card_id {card_id:?}"
        )));
    }
    // CR 702.94a: Miracle-revealed spells are cast from hand.
    if obj.zone != Zone::Hand || obj.owner != player {
        return Err(EngineError::ActionNotAllowed(
            "CastSpellAsMiracle requires a hand card owned by the caster".to_string(),
        ));
    }
    // CR 702.94a: The keyword must still be present — it can have been removed
    // by layers / replacement effects between offer time and accept time.
    let has_miracle = obj
        .keywords
        .iter()
        .any(|k| matches!(k, crate::types::keywords::Keyword::Miracle(_)));
    if !has_miracle {
        return Err(EngineError::ActionNotAllowed(
            "Card no longer has miracle".to_string(),
        ));
    }
    let prepared = prepare_spell_cast_with_variant_override(
        state,
        player,
        object_id,
        Some(CastingVariant::Miracle),
    )?;
    continue_with_prepared(state, player, prepared, events)
}

/// CR 702.35a: Cast a discarded card from exile via its Madness alternative
/// mana cost after the madness triggered ability resolves.
pub fn handle_cast_spell_as_madness(
    state: &mut GameState,
    player: PlayerId,
    object_id: ObjectId,
    card_id: CardId,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    let obj = state
        .objects
        .get(&object_id)
        .ok_or_else(|| EngineError::InvalidAction("Object not found".to_string()))?;
    if obj.card_id != card_id {
        return Err(EngineError::InvalidAction(format!(
            "Object {object_id:?} does not match card_id {card_id:?}"
        )));
    }
    if obj.zone != Zone::Exile || obj.owner != player {
        return Err(EngineError::ActionNotAllowed(
            "CastSpellAsMadness requires an exiled card owned by the caster".to_string(),
        ));
    }
    let has_madness = obj
        .keywords
        .iter()
        .any(|k| matches!(k, crate::types::keywords::Keyword::Madness(_)));
    if !has_madness {
        return Err(EngineError::ActionNotAllowed(
            "Card no longer has madness".to_string(),
        ));
    }
    let prepared = prepare_spell_cast_with_variant_override(
        state,
        player,
        object_id,
        Some(CastingVariant::Madness),
    )?;
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

    // CR 702.74a: Evoke — when a hand card has Keyword::Evoke and both costs
    // are affordable, present a choice. Auto-skip when only one cost is viable.
    // Unlike Warp, Evoke is opt-in via variant_override (the printed mana cost
    // remains the default), so the only routing needed is when the player picks
    // the evoke cost.
    if let Some(obj) = state.objects.get(&object_id) {
        if obj.zone == Zone::Hand {
            if let Some(evoke_cost) = obj.keywords.iter().find_map(|k| match k {
                crate::types::keywords::Keyword::Evoke(cost) => Some(cost.clone()),
                _ => None,
            }) {
                let normal_affordable =
                    can_pay_cost_after_auto_tap(state, player, object_id, &obj.mana_cost);
                let evoke_affordable =
                    can_pay_cost_after_auto_tap(state, player, object_id, &evoke_cost);
                if normal_affordable && evoke_affordable {
                    return Ok(WaitingFor::EvokeCostChoice {
                        player,
                        object_id,
                        card_id,
                        normal_cost: obj.mana_cost.clone(),
                        evoke_cost,
                    });
                }
                if !normal_affordable && evoke_affordable {
                    // Only evoke is payable — proceed via the evoke path.
                    return handle_evoke_cost_choice(
                        state, player, object_id, card_id, true, events,
                    );
                }
                // Otherwise (normal-only or neither): fall through to normal cast.
            }
        }
    }

    // CR 702.96a: Overload — when a hand card has Keyword::Overload and both
    // costs are affordable, present a choice. Auto-skip when only one cost is
    // viable. Mirrors the Evoke opt-in flow: Overload is opt-in via
    // variant_override (the printed mana cost remains the default) so the only
    // routing needed is when the player picks the overload cost.
    if let Some(obj) = state.objects.get(&object_id) {
        if obj.zone == Zone::Hand {
            if let Some(overload_cost) = obj.keywords.iter().find_map(|k| match k {
                crate::types::keywords::Keyword::Overload(cost) => Some(cost.clone()),
                _ => None,
            }) {
                let normal_affordable =
                    can_pay_cost_after_auto_tap(state, player, object_id, &obj.mana_cost);
                let overload_affordable =
                    can_pay_cost_after_auto_tap(state, player, object_id, &overload_cost);
                if normal_affordable && overload_affordable {
                    return Ok(WaitingFor::OverloadCostChoice {
                        player,
                        object_id,
                        card_id,
                        normal_cost: obj.mana_cost.clone(),
                        overload_cost,
                    });
                }
                if !normal_affordable && overload_affordable {
                    // Only overload is payable — proceed via the overload path.
                    return handle_overload_cost_choice(
                        state, player, object_id, card_id, true, events,
                    );
                }
                // Otherwise (normal-only or neither): fall through to normal cast.
            }
        }
    }

    continue_cast_from_prepared(state, player, object_id, events)
}

/// CR 601.2a: Announce the spell by pushing a placeholder `StackEntry` onto
/// the stack. Called exactly once per spell cast, at the top of
/// `continue_with_prepared` / `continue_with_no_ability` /
/// `handle_adventure_choice` (i.e., after all pre-announcement choices like
/// Adventure/Warp/MDFC have resolved and `prepare_spell_cast` succeeded).
///
/// The stack entry is pushed with `ability: None` and `actual_mana_spent: 0`;
/// `finalize_cast` updates these in place once choices and costs are committed
/// and performs the `Zone::Stack` zone change for the object itself. Keeping
/// `obj.zone` equal to the origin zone (hand / graveyard / exile / command)
/// until finalize preserves CR-correct evaluation of off-zone continuous
/// effects (CR 604.3 — "each nonland card in your graveyard has escape", cast-
/// with-keyword statics that filter "spells you cast from exile", etc.). The
/// CR-visible invariant — "the spell is on the stack" — is expressed by the
/// presence of the StackEntry, not the object's zone field.
///
/// If the cast is aborted at any step (CR 601.2i), `handle_cancel_cast` pops
/// this entry; no zone reversion is needed because `obj.zone` never changed.
fn announce_spell_on_stack(
    state: &mut GameState,
    player: PlayerId,
    prepared: &PreparedSpellCast,
    events: &mut Vec<GameEvent>,
) {
    stack::push_to_stack(
        state,
        StackEntry {
            id: prepared.object_id,
            source_id: prepared.object_id,
            controller: player,
            kind: StackEntryKind::Spell {
                card_id: prepared.card_id,
                ability: None,
                casting_variant: prepared.casting_variant,
                actual_mana_spent: 0,
            },
        },
        events,
    );
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
    // Permanent spells with no spell ability skip modal/targeting/effect resolution
    // and proceed directly to cost payment — unless they are Auras, which target
    // via the Enchant keyword and need the Aura targeting path below.
    if prepared.ability_def.is_none() {
        let is_aura = state
            .objects
            .get(&prepared.object_id)
            .map(|obj| obj.card_types.subtypes.iter().any(|s| s == "Aura"))
            .unwrap_or(false);
        if !is_aura {
            return continue_with_no_ability(state, player, prepared, events);
        }
    }

    // CR 601.2a: The spell goes on the stack at announcement, before any
    // mode/target/cost steps. All subsequent branches construct a `PendingCast`
    // that references an object already on the stack.
    announce_spell_on_stack(state, player, &prepared, events);

    // Build the resolved ability from the ability_def, or a placeholder for auras
    // with no spell-level ability (aura targeting is via the Enchant keyword).
    let resolved = if let Some(ref ability_def) = prepared.ability_def {
        // CR 601.2c: The player announcing a spell with modes chooses the mode(s).
        if let Some(ref modal_choice) = prepared.modal {
            // Cap max_choices to actual mode count
            let mut capped = modal_choice.clone();
            capped.max_choices = capped.max_choices.min(capped.mode_count);
            let target_constraints = target_constraints_from_modal(&capped);

            // Build a placeholder resolved ability -- will be replaced after mode selection
            let placeholder = ResolvedAbility::new(
                *ability_def.effect.clone(),
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
            pending_modal.distribute = ability_def.distribute.clone();
            pending_modal.target_constraints = target_constraints;
            pending_modal.origin_zone = prepared.origin_zone;
            return Ok(WaitingFor::ModeChoice {
                player,
                modal: capped,
                pending_cast: Box::new(pending_modal),
            });
        }

        let mut r = ResolvedAbility::new(
            *ability_def.effect.clone(),
            Vec::new(),
            prepared.object_id,
            player,
        );
        if let Some(sub) = &ability_def.sub_ability {
            r = r.sub_ability(build_resolved_from_def(sub, prepared.object_id, player));
        }
        if let Some(c) = ability_def.condition.clone() {
            r = r.condition(c);
        }
        r
    } else {
        // Aura placeholder — will carry targets from Enchant keyword targeting
        ResolvedAbility::new(
            Effect::Unimplemented {
                name: String::new(),
                description: None,
            },
            Vec::new(),
            prepared.object_id,
            player,
        )
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
                    prepared.origin_zone,
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
                pending_aura.distribute = prepared
                    .ability_def
                    .as_ref()
                    .and_then(|a| a.distribute.clone());
                pending_aura.origin_zone = prepared.origin_zone;
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
        if let Some(targets) =
            auto_select_targets_for_ability(state, &resolved, &target_slots, &[])?
        {
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
                prepared.origin_zone,
                events,
            );
        }

        let selection = begin_target_selection_for_ability(state, &resolved, &target_slots, &[])?;
        let mut pending_targets = PendingCast::new(
            prepared.object_id,
            prepared.card_id,
            resolved,
            prepared.mana_cost.clone(),
        );
        pending_targets.casting_variant = prepared.casting_variant;
        pending_targets.distribute = prepared
            .ability_def
            .as_ref()
            .and_then(|a| a.distribute.clone());
        pending_targets.origin_zone = prepared.origin_zone;
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
        prepared.origin_zone,
        events,
    )
}

/// Fast path for permanent spells with no spell-level ability.
/// Skips modal/targeting/effect — proceeds directly to cost payment.
fn continue_with_no_ability(
    state: &mut GameState,
    player: PlayerId,
    prepared: PreparedSpellCast,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    // Auras always have a spell ability (Enchant keyword generates targeting),
    // so this path is only for non-Aura permanents.

    // CR 601.2a: Announce the spell onto the stack before any cost payment.
    announce_spell_on_stack(state, player, &prepared, events);

    // Build a placeholder resolved ability for cost-payment plumbing.
    // The PendingCast infrastructure requires a ResolvedAbility; it carries no
    // meaningful effect and will be discarded (pushed as `ability: None`) when
    // finalize_cast_to_stack detects no Spell-kind AbilityDefinition on the object.
    let placeholder = ResolvedAbility::new(
        Effect::Unimplemented {
            name: String::new(),
            description: None,
        },
        Vec::new(),
        prepared.object_id,
        player,
    );
    check_additional_cost_or_pay(
        state,
        player,
        prepared.object_id,
        prepared.card_id,
        placeholder,
        &prepared.mana_cost,
        prepared.casting_variant,
        prepared.origin_zone,
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
    let ability_def = match combined_spell_ability_def(obj) {
        Some(a) => a,
        None => return true, // Permanent with no spell abilities needs no targets
    };

    let resolved = build_resolved_from_def(&ability_def, obj.id, player);
    match build_target_slots(&simulated, &resolved) {
        Ok(target_slots) => {
            if target_slots.is_empty() {
                true
            } else {
                auto_select_targets_for_ability(&simulated, &resolved, &target_slots, &[]).is_ok()
            }
        }
        Err(_) => false,
    }
}

/// CR 601.2b + CR 118.9a: Check whether `object_id` can legally be cast for
/// free via the given `source_id` right now. Mirrors `can_cast_object_now`'s
/// timing/targeting checks using a `CastingVariant::HandPermission { source,
/// frequency }` override so the mana cost is `NoCost` and the source's
/// once-per-turn slot (if any) is consulted.
pub fn can_cast_for_free_now(
    state: &GameState,
    player: PlayerId,
    object_id: ObjectId,
    source_id: ObjectId,
    frequency: CastFrequency,
) -> bool {
    let variant = CastingVariant::HandPermission {
        source: source_id,
        frequency,
    };
    let Ok(prepared) =
        prepare_spell_cast_with_variant_override(state, player, object_id, Some(variant))
    else {
        return false;
    };
    let Some(obj) = state.objects.get(&prepared.object_id) else {
        return false;
    };
    // CR 118.9a: NoCost means mana affordability is automatic; the remaining
    // gate is legal-targets for targeted spells (permanent spells skip via
    // `spell_has_legal_targets` semantics).
    prepared.modal.is_some() || spell_has_legal_targets(state, obj, player)
}

/// CR 601.2b: Enumerate `(object_id, source_id, frequency)` candidates for
/// `CastSpellForFree` — for each hand-spell the caller could cast and each
/// active `CastFromHandFree { OncePerTurn }` permission source that admits it.
///
/// `Unlimited` sources (Omniscience) are intentionally excluded: they route
/// through the implicit `CastSpell` silent-free path to avoid duplicating the
/// same candidate action under two different action variants.
pub fn hand_cast_free_candidates(
    state: &GameState,
    player: PlayerId,
) -> Vec<(ObjectId, ObjectId, CastFrequency)> {
    // CR 601.2b + CR 400.7: Collect active (source_id, frequency, filter)
    // triples for OncePerTurn permissions that haven't been consumed this turn.
    let sources: Vec<(ObjectId, TargetFilter, CastFrequency)> = state
        .battlefield
        .iter()
        .filter_map(|&src_id| {
            let src_obj = state.objects.get(&src_id)?;
            if src_obj.controller != player {
                return None;
            }
            active_static_definitions(state, src_obj).find_map(|s| match s.mode {
                StaticMode::CastFromHandFree { frequency } => {
                    if frequency == CastFrequency::OncePerTurn
                        && state.hand_cast_free_permissions_used.contains(&src_id)
                    {
                        None
                    } else if frequency == CastFrequency::OncePerTurn {
                        s.affected.as_ref().map(|f| (src_id, f.clone(), frequency))
                    } else {
                        None
                    }
                }
                _ => None,
            })
        })
        .collect();

    if sources.is_empty() {
        return Vec::new();
    }

    let mut out = Vec::new();
    let Some(player_data) = state.players.iter().find(|p| p.id == player) else {
        return out;
    };
    for &hand_id in &player_data.hand {
        for (src_id, filter, frequency) in &sources {
            let ctx = super::filter::FilterContext::from_source_with_controller(*src_id, player);
            if !super::filter::matches_target_filter(state, hand_id, filter, &ctx) {
                continue;
            }
            if can_cast_for_free_now(state, player, hand_id, *src_id, *frequency) {
                out.push((hand_id, *src_id, *frequency));
            }
        }
    }
    out
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

    // CR 702.34a + CR 118.3 + CR 119.8: Flashback's non-mana cost (e.g. "pay N
    // life") is an additional cost. Pre-check affordability so a CantLoseLife
    // lock or insufficient life filters the flashback from legal actions.
    if prepared.casting_variant == CastingVariant::Flashback {
        if let Some(FlashbackCost::NonMana(ref cost)) =
            super::keywords::effective_flashback_cost(state, prepared.object_id)
        {
            if let Some(amount) = find_pay_life_cost(cost, state, player, prepared.object_id) {
                if !super::life_costs::can_pay_life_cost(state, player, amount) {
                    return false;
                }
            }
        }
    }

    // CR 601.2b + CR 118.3 + CR 119.8: Additional-cost affordability — any
    // `AbilityCost::PayLife` attached as an additional cost (Required or
    // Optional-but-required-to-cast) must be payable for the spell to be cast.
    // For Optional additional costs this is a false-negative in the locked case
    // only if the optional cost is the ONLY affordability gate, which is never
    // the case; the mana cost already has to be payable on its own.
    if let Some(AdditionalCost::Required(cost)) = state
        .objects
        .get(&prepared.object_id)
        .and_then(|o| o.additional_cost.as_ref())
    {
        if let Some(amount) = find_pay_life_cost(cost, state, player, prepared.object_id) {
            if !super::life_costs::can_pay_life_cost(state, player, amount) {
                return false;
            }
        }
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

    let creature_face_ok = (prepared.modal.is_some()
        || spell_has_legal_targets(state, obj, player))
        && can_pay_cost_after_auto_tap(state, player, prepared.object_id, &prepared.mana_cost);

    if creature_face_ok {
        return true;
    }

    // CR 715.3a: For adventure cards, also evaluate the adventure face (instant/sorcery).
    // The creature face may be unaffordable while the adventure face is castable — in that
    // case the card is still legally castable and will prompt AdventureCastChoice.
    if is_adventure_card(obj) {
        let mut sim = state.clone();
        if let Some(sim_obj) = sim.objects.get_mut(&object_id) {
            swap_to_adventure_face(sim_obj);
        }
        return can_cast_object_now(&sim, player, object_id);
    }

    false
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
    let spell_meta = build_spell_meta(&simulated, player, source_id);

    let mut tap_events: Vec<crate::types::events::GameEvent> = Vec::new();
    super::casting_costs::auto_tap_mana_sources(
        &mut simulated,
        player,
        cost,
        &mut tap_events,
        Some(source_id),
    );

    // CR 605.1b: A `TapsForMana` triggered mana ability (Fertile Ground / Wild
    // Growth / Utopia Sprawl class) resolves inline and adds bonus mana to the
    // pool when the enchanted land is tapped for mana. The auto-tap helper
    // emits the `ManaAdded` events but does not run the trigger pipeline; in
    // the production action flow `run_post_action_pipeline` later processes
    // them, but this affordability simulation is a one-shot clone — without an
    // explicit pipeline run the aura's bonus mana would never reach the
    // simulated pool, causing AI legal-actions to skip a cast that the player
    // would actually be able to pay (e.g. Plains + Wild Growth → cost {1}{G}).
    if !tap_events.is_empty() {
        super::triggers::process_triggers(&mut simulated, &tap_events);
    }

    let any_color = super::static_abilities::player_can_spend_as_any_color(&simulated, player);
    // CR 107.4f + CR 118.3 + CR 119.8: Include the caster's Phyrexian life
    // budget so a cost containing {C/P} shards is only reported payable when
    // either mana or sufficient life (respecting CantLoseLife) is available.
    let max_life = super::life_costs::max_phyrexian_life_payments(&simulated, player);
    let spell_ctx = spell_meta.as_ref().map(PaymentContext::Spell);
    simulated
        .players
        .iter()
        .find(|p| p.id == player)
        .is_some_and(|player_data| {
            mana_payment::can_pay_for_spell(
                &player_data.mana_pool,
                cost,
                spell_ctx.as_ref(),
                any_color,
                max_life,
            )
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
/// Used by spell casting (`pay_and_push`). Builds a `PaymentContext::Spell` from
/// the cast object's types so CR 106.6 spell-side restrictions (`allows_spell`)
/// gate which restricted mana is eligible. For ability activation, use
/// `pay_ability_mana_cost` instead so restrictions are evaluated against the
/// source permanent's types via `allows_activation`.
pub(super) fn pay_mana_cost(
    state: &mut GameState,
    player: PlayerId,
    source_id: ObjectId,
    cost: &crate::types::mana::ManaCost,
    events: &mut Vec<GameEvent>,
) -> Result<(), EngineError> {
    pay_mana_cost_with_choices(state, player, source_id, cost, None, events)
}

/// CR 107.4f + CR 601.2f: Pay a spell's mana cost, honoring explicit per-shard
/// Phyrexian choices when provided. `None` preserves the legacy auto-decide
/// behavior (prefer mana, fall back to life).
pub(super) fn pay_mana_cost_with_choices(
    state: &mut GameState,
    player: PlayerId,
    source_id: ObjectId,
    cost: &crate::types::mana::ManaCost,
    phyrexian_choices: Option<&[crate::types::game_state::ShardChoice]>,
    events: &mut Vec<GameEvent>,
) -> Result<(), EngineError> {
    if state.layers_dirty {
        super::layers::evaluate_layers(state);
    }

    let spell_meta = build_spell_meta(state, player, source_id);
    let spell_ctx = spell_meta.as_ref().map(PaymentContext::Spell);

    let spent_units = auto_tap_and_pay_cost(
        state,
        player,
        source_id,
        cost,
        spell_ctx.as_ref(),
        phyrexian_choices,
        events,
    )?;

    // CR 106.6: Apply mana spell grants to the spell being cast.
    apply_mana_spell_grants(state, source_id, &spent_units);

    // CR 601.2h: Track whether mana was actually spent to cast this spell,
    // and the per-color breakdown for Adamant-style intervening-if checks
    // (CR 207.2c).
    if !spent_units.is_empty() {
        if let Some(obj) = state.objects.get_mut(&source_id) {
            obj.mana_spent_to_cast = true;
            obj.mana_spent_to_cast_amount = spent_units.len() as u32;
            for unit in &spent_units {
                obj.colors_spent_to_cast.add_unit(unit);
            }
        }
    }

    Ok(())
}

/// CR 106.6: Pay the mana cost of an activated ability. Unlike `pay_mana_cost`
/// (which builds a spell context and consults `allows_spell`), this builds a
/// `PaymentContext::Activation` from the source permanent's core types and
/// subtypes so restrictions like Flamebraider's "activate abilities of
/// Elemental sources" and Heart of Ramos's "activate abilities only" are
/// enforced correctly at the spend gate.
///
/// Callers: `pay_ability_cost` for `AbilityCost::Mana` sub-costs. Spell-side
/// bookkeeping (mana-spent-to-cast, spell grants) is intentionally skipped —
/// those are cast-only concerns.
pub(super) fn pay_ability_mana_cost(
    state: &mut GameState,
    player: PlayerId,
    source_id: ObjectId,
    cost: &crate::types::mana::ManaCost,
    events: &mut Vec<GameEvent>,
) -> Result<(), EngineError> {
    if state.layers_dirty {
        super::layers::evaluate_layers(state);
    }

    let (source_types, source_subtypes) = activation_source_types(state, source_id);
    let activation_ctx = PaymentContext::Activation {
        source_types: &source_types,
        source_subtypes: &source_subtypes,
    };

    let _spent_units = auto_tap_and_pay_cost(
        state,
        player,
        source_id,
        cost,
        Some(&activation_ctx),
        None,
        events,
    )?;

    Ok(())
}

/// Shared mana-payment core: auto-taps sources, validates affordability,
/// executes the spend with the given payment context, and processes any
/// Phyrexian life payments. Returns the spent units so spell-specific callers
/// can apply grants / bookkeeping. Single authority for restriction gating.
fn auto_tap_and_pay_cost(
    state: &mut GameState,
    player: PlayerId,
    source_id: ObjectId,
    cost: &crate::types::mana::ManaCost,
    ctx: Option<&PaymentContext<'_>>,
    phyrexian_choices: Option<&[crate::types::game_state::ShardChoice]>,
    events: &mut Vec<GameEvent>,
) -> Result<Vec<crate::types::mana::ManaUnit>, EngineError> {
    auto_tap_mana_sources(state, player, cost, events, Some(source_id));

    {
        let player_data = state
            .players
            .iter()
            .find(|p| p.id == player)
            .expect("player exists");
        let any_color = super::static_abilities::player_can_spend_as_any_color(state, player);
        // CR 107.4f + CR 118.3 + CR 119.8: Life budget for Phyrexian shards —
        // respects CantLoseLife (budget 0 under lock) and current life total.
        let max_life = super::life_costs::max_phyrexian_life_payments(state, player);
        if !mana_payment::can_pay_for_spell(&player_data.mana_pool, cost, ctx, any_color, max_life)
        {
            return Err(EngineError::ActionNotAllowed(
                "Cannot pay mana cost".to_string(),
            ));
        }
    }

    let any_color = super::static_abilities::player_can_spend_as_any_color(state, player);
    let hand_demand = mana_payment::compute_hand_color_demand(state, player, source_id);
    let player_data = state
        .players
        .iter_mut()
        .find(|p| p.id == player)
        .expect("player exists");
    let (spent_units, life_payments) = mana_payment::pay_cost_with_demand_and_choices(
        &mut player_data.mana_pool,
        cost,
        Some(&hand_demand),
        ctx,
        any_color,
        phyrexian_choices,
    )
    .map_err(|_| EngineError::ActionNotAllowed("Mana payment failed".to_string()))?;

    // CR 107.4f + CR 118.3b + CR 119.4 + CR 119.8: Each Phyrexian shard paid
    // with life routes through the single-authority life-cost helper so the
    // deduction IS a life-loss event (replacement pipeline + CantLoseLife
    // short-circuit apply consistently).
    for payment in &life_payments {
        let amount = u32::try_from(payment.amount).unwrap_or(0);
        match super::life_costs::pay_life_as_cost(state, player, amount, events) {
            super::life_costs::PayLifeCostResult::Paid { .. } => {}
            super::life_costs::PayLifeCostResult::InsufficientLife
            | super::life_costs::PayLifeCostResult::LockedCantLoseLife => {
                return Err(EngineError::ActionNotAllowed(
                    "Cannot pay Phyrexian life cost".to_string(),
                ));
            }
        }
    }

    Ok(spent_units)
}

/// CR 106.6: Build (core-types, subtypes) slices for a `PaymentContext::Activation`
/// from the source permanent. Mirrors `build_spell_meta`'s type extraction so
/// `allows_activation` and `allows_spell` consult identically-shaped strings.
pub(super) fn activation_source_types(
    state: &GameState,
    source_id: ObjectId,
) -> (Vec<String>, Vec<String>) {
    state
        .objects
        .get(&source_id)
        .map(|obj| {
            let types = obj
                .card_types
                .core_types
                .iter()
                .map(|ct| format!("{ct:?}"))
                .collect();
            let subtypes = obj.card_types.subtypes.clone();
            (types, subtypes)
        })
        .unwrap_or_default()
}

/// CR 106.6: When mana with spell grants is spent to cast a spell, apply those
/// grants to the spell object (e.g., "that spell can't be countered").
fn apply_mana_spell_grants(
    state: &mut GameState,
    spell_id: ObjectId,
    spent_units: &[crate::types::mana::ManaUnit],
) {
    let has_cant_be_countered = spent_units
        .iter()
        .any(|u| u.grants.contains(&ManaSpellGrant::CantBeCountered));

    if has_cant_be_countered {
        if let Some(obj) = state.objects.get_mut(&spell_id) {
            // Only add if not already present (idempotent).
            if !obj
                .static_definitions
                .iter_all()
                .any(|sd| sd.mode == StaticMode::CantBeCountered)
            {
                obj.static_definitions
                    .push(StaticDefinition::new(StaticMode::CantBeCountered));
            }
        }
    }
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
            // CR 106.6: Ability activation — restriction enforcement routes
            // through `allows_activation` (not `allows_spell`) via the
            // activation context built from the source permanent's types.
            pay_ability_mana_cost(state, player, source_id, cost, events)?;
        }
        AbilityCost::Composite { costs } => {
            for sub_cost in costs {
                pay_ability_cost(state, player, source_id, sub_cost, events)?;
            }
        }
        // CR 118.3: Sacrifice as a cost — sacrifice the source (SelfRef) or a chosen permanent.
        AbilityCost::Sacrifice { target, .. } => {
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
        // CR 118.3 + CR 702.97a: "Exile this card from your graveyard" as a self-ref
        // activation cost (Scavenge, Renew, and other graveyard-activated abilities).
        // The source is identified by SelfRef; no player choice is needed, so this
        // is an auto-payable cost (no WaitingFor round-trip). Non-self exile costs
        // (targeted exile from any zone) are still handled by the catch-all below.
        AbilityCost::Exile {
            filter: Some(TargetFilter::SelfRef),
            zone: Some(Zone::Graveyard),
            count: 1,
        } => {
            let obj = state.objects.get(&source_id).ok_or_else(|| {
                EngineError::InvalidAction("Source object not found for exile cost".to_string())
            })?;
            if obj.zone != Zone::Graveyard {
                return Err(EngineError::ActionNotAllowed(
                    "Cannot exile from graveyard: source is not in a graveyard".to_string(),
                ));
            }
            super::zones::move_to_zone(state, source_id, Zone::Exile, events);
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
        AbilityCost::PaySpeed { amount } => {
            let amount = resolve_quantity(state, amount, player, source_id);
            let amount = u8::try_from(amount.max(0)).unwrap_or(u8::MAX);
            let current_speed = effective_speed(state, player);
            if amount > current_speed {
                return Err(EngineError::ActionNotAllowed("Not enough speed".into()));
            }
            set_speed(state, player, Some(current_speed - amount), events);
        }
        // CR 606.4: Loyalty abilities use loyalty counter adjustment as their cost.
        // Called after target selection when the ability was initiated interactively.
        // Routes through the single-authority counter resolver so replacement
        // effects (Vorinclex, Doubling Season) can apply per CR 614.1a and
        // obj.loyalty stays in sync with counters[Loyalty] (CR 306.5b).
        AbilityCost::Loyalty { amount } => {
            let amount = *amount;
            match amount.cmp(&0) {
                std::cmp::Ordering::Greater => {
                    super::effects::counters::add_counter_with_replacement(
                        state,
                        source_id,
                        crate::types::counter::CounterType::Loyalty,
                        amount as u32,
                        events,
                    );
                }
                std::cmp::Ordering::Less => {
                    super::effects::counters::remove_counter_with_replacement(
                        state,
                        source_id,
                        crate::types::counter::CounterType::Loyalty,
                        (-amount) as u32,
                        events,
                    );
                }
                std::cmp::Ordering::Equal => {}
            }
        }
        // CR 118.3 + CR 122: Remove-counter cost. The SelfRef form ("Remove N
        // {type} counters from ~") is auto-payable — no player choice is needed,
        // so it lands here rather than in an interactive WaitingFor round-trip.
        // Routes through the single-authority counter resolver so replacement
        // effects (Vorinclex, Doubling Season) apply per CR 614.1a and
        // obj.loyalty/obj.defense stay in sync per CR 306.5b / CR 310.4c.
        // Legality (CR 118.3: "can't pay a cost without having the necessary
        // resources") is enforced upstream by `AbilityCost::is_payable` in
        // cost_payability.rs before activation is committed.
        AbilityCost::RemoveCounter {
            count,
            counter_type,
            target: None,
        } => {
            let counter_kind = crate::types::counter::parse_counter_type(counter_type);
            super::effects::counters::remove_counter_with_replacement(
                state,
                source_id,
                counter_kind,
                *count,
                events,
            );
        }
        // Targeted remove-counter costs ("remove a counter from target X") would
        // need an interactive WaitingFor flow to let the player pick the permanent.
        // The current parser only emits `target: None`, so this is unreachable in
        // practice but kept exhaustive to catch any future parser extension.
        AbilityCost::RemoveCounter {
            target: Some(_), ..
        } => {
            return Err(EngineError::ActionNotAllowed(
                "Targeted remove-counter costs require interactive resolution and must be \
                 intercepted before reaching pay_ability_cost"
                    .to_string(),
            ));
        }
        // CR 701.43a: "To exert a permanent, its controller chooses to have it
        // not untap during its controller's next untap step." Modeled as a
        // transient continuous effect with `StaticMode::CantUntap` scoped to
        // `Duration::UntilControllerNextUntapStep` on the source permanent,
        // identical to the "doesn't untap during its controller's next untap
        // step" pattern already handled by the layer system (see
        // `layers::prune_controller_untap_step_effects`).
        //
        // CR 701.43b: "A permanent can be exerted even if it's not tapped or
        // has already been exerted in a turn." Pushing a second identical
        // effect is harmless — both expire during the same untap step.
        //
        // CR 701.43c: "An object that isn't on the battlefield can't be
        // exerted." Enforced here so off-battlefield activations (which
        // shouldn't reach this site for Exert costs on permanents) fail
        // loudly rather than creating a dangling effect.
        AbilityCost::Exert => {
            let obj = state.objects.get(&source_id).ok_or_else(|| {
                EngineError::InvalidAction("Source object not found for exert cost".to_string())
            })?;
            if obj.zone != Zone::Battlefield {
                return Err(EngineError::ActionNotAllowed(
                    "Cannot exert: source is not on the battlefield".to_string(),
                ));
            }
            let controller = obj.controller;
            state.add_transient_continuous_effect(
                source_id,
                controller,
                crate::types::ability::Duration::UntilControllerNextUntapStep,
                TargetFilter::SpecificObject { id: source_id },
                vec![
                    crate::types::ability::ContinuousModification::AddStaticMode {
                        mode: StaticMode::CantUntap,
                    },
                ],
                None,
            );
        }
        // Other cost types (Exile, PayLife, etc.) require interactive resolution
        // and are intercepted before reaching pay_ability_cost, or are not yet auto-payable.
        AbilityCost::Untap
        | AbilityCost::PayLife { .. }
        | AbilityCost::Discard { .. }
        | AbilityCost::Exile { .. }
        | AbilityCost::CollectEvidence { .. }
        | AbilityCost::TapCreatures { .. }
        | AbilityCost::ReturnToHand { .. }
        | AbilityCost::Mill { .. }
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
        AbilityCost::Sacrifice { target, .. } if !matches!(target, TargetFilter::SelfRef) => {
            Some(target)
        }
        AbilityCost::Composite { costs } => costs.iter().find_map(find_non_self_sacrifice),
        _ => None,
    }
}

fn find_non_self_discard(cost: &AbilityCost) -> Option<(&QuantityExpr, Option<&TargetFilter>)> {
    match cost {
        AbilityCost::Discard {
            count,
            filter,
            self_ref: false,
            ..
        } => Some((count, filter.as_ref())),
        AbilityCost::Composite { costs } => costs.iter().find_map(find_non_self_discard),
        _ => None,
    }
}

pub(crate) fn find_eligible_discard_targets(
    state: &GameState,
    player: PlayerId,
    source: ObjectId,
    filter: Option<&TargetFilter>,
) -> Vec<ObjectId> {
    let ctx = super::filter::FilterContext::from_source(state, source);
    state
        .players
        .get(player.0 as usize)
        .map(|player_state| {
            player_state
                .hand
                .iter()
                .copied()
                .filter(|&id| {
                    id != source
                        && filter.is_none_or(|f| {
                            super::filter::matches_target_filter(state, id, f, &ctx)
                        })
                })
                .collect()
        })
        .unwrap_or_default()
}

fn find_return_to_hand_cost(cost: &AbilityCost) -> Option<(u32, Option<&TargetFilter>)> {
    match cost {
        AbilityCost::ReturnToHand { count, filter } => Some((*count, filter.as_ref())),
        AbilityCost::Composite { costs } => costs.iter().find_map(find_return_to_hand_cost),
        _ => None,
    }
}

pub(crate) fn find_eligible_return_to_hand_targets(
    state: &GameState,
    player: PlayerId,
    source: ObjectId,
    filter: Option<&TargetFilter>,
) -> Vec<ObjectId> {
    let ctx = super::filter::FilterContext::from_source(state, source);
    state
        .battlefield
        .iter()
        .copied()
        .filter(|&id| {
            state.objects.get(&id).is_some_and(|obj| {
                obj.controller == player
                    && filter
                        .is_none_or(|f| super::filter::matches_target_filter(state, id, f, &ctx))
            })
        })
        .collect()
}

/// CR 702.34a + CR 118.8: Partition a flashback cost into its mana sub-cost (paid
/// through the normal mana-payment flow) and its residual non-mana sub-cost (paid
/// as an additional cost via `pay_additional_cost`).
///
/// Compound flashback costs ("Flashback—{1}{U}, Pay 3 life") are stored by the
/// parser as `FlashbackCost::NonMana(AbilityCost::Composite([Mana, PayLife, ...]))`.
/// This helper extracts the embedded `Mana` sub-cost so both halves of the cost
/// are paid through their proper pipelines. Mirrors `extract_x_mana_cost` in
/// casting_costs.rs.
///
/// Returns `(mana_sub_cost, non_mana_residual)`. Either may be `None`:
///   - Pure-mana flashback     → `(Some(mana), None)`
///   - Pure non-mana           → `(None, Some(cost))`
///   - Compound mana+non-mana  → `(Some(mana), Some(residual))`
pub(super) fn split_flashback_cost_components(
    flashback: Option<&FlashbackCost>,
) -> (Option<crate::types::mana::ManaCost>, Option<AbilityCost>) {
    let Some(fb) = flashback else {
        return (None, None);
    };
    match fb {
        FlashbackCost::Mana(mana) => (Some(mana.clone()), None),
        FlashbackCost::NonMana(AbilityCost::Mana { cost }) => (Some(cost.clone()), None),
        FlashbackCost::NonMana(AbilityCost::Composite { costs }) => {
            // Find the (single) Mana sub-cost and partition the rest.
            let mana_idx = costs
                .iter()
                .position(|sub| matches!(sub, AbilityCost::Mana { .. }));
            match mana_idx {
                None => (
                    None,
                    Some(AbilityCost::Composite {
                        costs: costs.clone(),
                    }),
                ),
                Some(idx) => {
                    let mut remaining = costs.clone();
                    let AbilityCost::Mana { cost: extracted } = remaining.remove(idx) else {
                        unreachable!("position() guarantees Mana variant")
                    };
                    let residual = match remaining.len() {
                        0 => None,
                        1 => Some(remaining.into_iter().next().unwrap()),
                        _ => Some(AbilityCost::Composite { costs: remaining }),
                    };
                    (Some(extracted), residual)
                }
            }
        }
        FlashbackCost::NonMana(other) => (None, Some(other.clone())),
    }
}

/// Walk a cost tree and return the first `PayLife` amount found, resolved
/// against the given state/player/source context. Used to pre-validate
/// pay-life affordability before simulation, since `pay_ability_cost`
/// treats `AbilityCost::PayLife` as a no-op.
///
/// `QuantityExpr` resolves dynamically (e.g. War Room's
/// `QuantityRef::ColorsInCommandersColorIdentity`), so this helper must be
/// evaluated at activation time against the current game state.
fn find_pay_life_cost(
    cost: &AbilityCost,
    state: &GameState,
    player: PlayerId,
    source_id: ObjectId,
) -> Option<u32> {
    match cost {
        AbilityCost::PayLife { amount } => {
            let resolved =
                super::quantity::resolve_quantity(state, amount, player, source_id).max(0) as u32;
            Some(resolved)
        }
        AbilityCost::Composite { costs } => costs
            .iter()
            .find_map(|c| find_pay_life_cost(c, state, player, source_id)),
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
            super::filter::matches_target_filter(
                state,
                id,
                filter,
                &super::filter::FilterContext::from_source(state, source_id),
            )
        })
        .collect()
}

fn can_pay_ability_cost_now(
    state: &GameState,
    player: PlayerId,
    source_id: ObjectId,
    cost: &AbilityCost,
) -> bool {
    // CR 601.2b: Unified choice-of-object + resource payability pre-gate. This
    // keeps legal-action generation in sync with `handle_activate_ability`, so
    // the AI never proposes an activation that the submit path would reject.
    if !cost.is_payable(state, player, source_id) {
        return false;
    }
    // CR 118.3: Pre-check non-self sacrifice eligibility before simulation.
    // The simulation would give a false positive since pay_ability_cost's
    // non-self Sacrifice arm is a no-op (it's handled interactively).
    if let Some(sac_filter) = find_non_self_sacrifice(cost) {
        if find_eligible_sacrifice_targets(state, player, source_id, sac_filter).is_empty() {
            return false;
        }
    }
    // Waterbend mana cost is paid interactively via ManaPayment, so
    // pay_ability_cost treats it as a no-op. Pre-check affordability here
    // to avoid listing unpayable Waterbend abilities as legal actions.
    if let Some(wb_cost) = find_waterbend_cost(cost) {
        if !can_pay_cost_after_auto_tap(state, player, source_id, wb_cost) {
            return false;
        }
    }
    // CR 118.3 + CR 119.4b + CR 119.8: Pay-life is paid interactively (or via
    // the effect resolver); `pay_ability_cost`'s `PayLife` arm is a no-op.
    // Pre-check both insufficient-life and CantLoseLife so locked or underfunded
    // activated abilities never appear as legal actions.
    if let Some(amount) = find_pay_life_cost(cost, state, player, source_id) {
        if !super::life_costs::can_pay_life_cost(state, player, amount) {
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
    // CR 701.35a: Detained permanents' activated abilities can't be activated.
    if !obj.detained_by.is_empty() {
        return false;
    }
    // CR 602.5 + CR 603.2a: Consult active CantBeActivated statics — a player can't
    // begin to activate an ability that's prohibited from being activated. Note this
    // only affects activated abilities (CR 603.2a: triggered abilities are unaffected
    // and use SuppressTriggers instead).
    // CR 605.1a: The ability definition is passed through so the prohibition can apply
    // its mana-ability exemption (Pithing Needle class) via the single classifier authority.
    if is_blocked_by_cant_be_activated(state, player, source_id, &ability_def) {
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
    // CR 606.3: A loyalty ability may be activated only if no player has previously
    // activated a loyalty ability of *that permanent* this turn. The generic
    // `OnlyOnceEachTurn` activation restriction tracks per `(source_id, ability_index)`,
    // which is the wrong granularity — it would let each loyalty ability fire once.
    // Defer to `can_activate_loyalty`, the single authority for the per-permanent gate.
    if matches!(ability_def.cost, Some(AbilityCost::Loyalty { .. }))
        && !super::planeswalker::can_activate_loyalty(state, source_id, player)
    {
        return false;
    }
    // CR 302.6 + CR 602.5a: Universal summoning-sickness gate for {T}/{Q} activated
    // abilities on creatures. Applies to every activated ability regardless of Oracle
    // text, so it lives as a structural helper rather than an ActivationRestriction.
    if let Some(ref cost) = ability_def.cost {
        if restrictions::check_summoning_sickness_for_cost(state, obj, cost).is_err() {
            return false;
        }
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
            target_slots.is_empty()
                || auto_select_targets_for_ability(&simulated, &resolved, &target_slots, &[])
                    .is_ok()
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

    // CR 602.5 + CR 603.2a: Reject activation if any CantBeActivated static prohibits
    // the player from activating this permanent's activated abilities.
    // CR 605.1a: The exemption gate (Pithing Needle's "unless they're mana abilities")
    // is applied inside `is_blocked_by_cant_be_activated` via `mana_abilities::is_mana_ability`.
    if is_blocked_by_cant_be_activated(state, player, source_id, &ability_def) {
        return Err(EngineError::ActionNotAllowed(
            "Activated abilities of this permanent can't be activated (CR 602.5)".to_string(),
        ));
    }

    // CR 601.2f: Apply self-referential cost reduction before any cost payment.
    apply_cost_reduction(state, &mut ability_def, player, source_id);

    // CR 601.2b: If the activation cost requires a choice of object and no
    // legal object exists, the ability can't be activated.
    if let Some(ref cost) = ability_def.cost {
        if !cost.is_payable(state, player, source_id) {
            return Err(EngineError::ActionNotAllowed(
                "Cannot pay activation cost".to_string(),
            ));
        }
    }

    restrictions::check_activation_restrictions(
        state,
        player,
        source_id,
        ability_index,
        &ability_def.activation_restrictions,
    )?;

    // CR 302.6 + CR 602.5a: Universal summoning-sickness gate for {T}/{Q} activated
    // abilities on creatures. Mirrors the check in `can_activate_ability_now` so both
    // the AI legality gate and the runtime activation path agree.
    if let Some(ref cost) = ability_def.cost {
        let obj = state.objects.get(&source_id).ok_or_else(|| {
            EngineError::InvalidAction("Object not found during summoning-sickness check".into())
        })?;
        restrictions::check_summoning_sickness_for_cost(state, obj, cost)?;
    }

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
        // CR 603.4: Stamp the printed-ability index for per-turn resolution tracking
        // before any branch path that pushes this ability onto the stack.
        r.ability_index = Some(ability_index);
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

        if let Some((count, filter)) = find_non_self_discard(cost) {
            let count =
                super::quantity::resolve_quantity(state, count, player, source_id).max(0) as usize;
            let eligible = find_eligible_discard_targets(state, player, source_id, filter);
            if eligible.len() < count {
                return Err(EngineError::ActionNotAllowed(
                    "Not enough cards in hand to discard".into(),
                ));
            }
            let mut pending_discard =
                PendingCast::new(source_id, CardId(0), resolved, ManaCost::NoCost);
            pending_discard.activation_cost = Some(cost.clone());
            pending_discard.activation_ability_index = Some(ability_index);
            return Ok(WaitingFor::DiscardForCost {
                player,
                count,
                cards: eligible,
                pending_cast: Box::new(pending_discard),
            });
        }

        // CR 118.3: Pre-check for ReturnToHand costs — same WaitingFor detour pattern as
        // Sacrifice above. Ordering matters for Composite costs: Sacrifice wins if both are
        // present, but no real cards combine them.
        if let Some((count, filter)) = find_return_to_hand_cost(cost) {
            let eligible = find_eligible_return_to_hand_targets(state, player, source_id, filter);
            if eligible.len() < count as usize {
                return Err(EngineError::ActionNotAllowed(
                    "No eligible permanents to return".into(),
                ));
            }
            let mut pending_return =
                PendingCast::new(source_id, CardId(0), resolved, ManaCost::NoCost);
            pending_return.activation_cost = Some(cost.clone());
            pending_return.activation_ability_index = Some(ability_index);
            return Ok(WaitingFor::ReturnToHandForCost {
                player,
                count: count as usize,
                permanents: eligible,
                pending_cast: Box::new(pending_return),
            });
        }

        // Waterbend cost: detour to ManaPayment with Waterbend mode.
        if let Some(wb_cost) = find_waterbend_cost(cost) {
            let mut pending_wb = PendingCast::new(source_id, CardId(0), resolved, wb_cost.clone());
            pending_wb.activation_cost = Some(cost.clone());
            pending_wb.activation_ability_index = Some(ability_index);
            state.pending_cast = Some(Box::new(pending_wb));
            return casting_costs::enter_payment_step(
                state,
                player,
                Some(ConvokeMode::Waterbend),
                events,
            );
        }

        // CR 107.1b + CR 601.2f: When an activated ability's cost includes a mana
        // cost containing X — either directly (`Mana { cost }`) or as a sub-cost
        // of a Composite (e.g., `Tap + Pay {X}`) — divert to ChooseXValue so X is
        // chosen before mana payment. The remaining non-mana sub-costs (Tap,
        // Sacrifice, etc.) are paid after ManaPayment via `activation_cost`.
        if let Some((mana_cost, remaining)) = casting_costs::extract_x_mana_cost(cost) {
            let mut pending_x = PendingCast::new(source_id, CardId(0), resolved, mana_cost);
            pending_x.activation_cost = remaining;
            pending_x.activation_ability_index = Some(ability_index);
            state.pending_cast = Some(Box::new(pending_x));
            return casting_costs::enter_payment_step(state, player, None, events);
        }
    }

    let target_slots = build_target_slots(state, &resolved)?;
    if !target_slots.is_empty() {
        if let Some(targets) =
            auto_select_targets_for_ability(state, &resolved, &target_slots, &[])?
        {
            let mut resolved = resolved;
            assign_targets_in_chain(&mut resolved, &targets)?;

            if let Some(ref cost) = ability_def.cost {
                if variable_speed_payment_range(cost, effective_speed(state, player)).is_some() {
                    return Ok(begin_variable_speed_payment(
                        state,
                        player,
                        source_id,
                        resolved,
                        cost.clone(),
                        ability_index,
                    ));
                }
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
            // CR 117.1b: Priority permits unbounded activation. `pending_activations`
            // is a per-priority-window AI-guard — see `GameState::pending_activations`.
            state.pending_activations.push((source_id, ability_index));
            events.push(GameEvent::AbilityActivated { source_id });
            state.priority_passes.clear();
            state.priority_pass_count = 0;
            return Ok(WaitingFor::Priority { player });
        }

        let selection = begin_target_selection_for_ability(state, &resolved, &target_slots, &[])?;
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
        if variable_speed_payment_range(cost, effective_speed(state, player)).is_some() {
            return Ok(begin_variable_speed_payment(
                state,
                player,
                source_id,
                resolved,
                cost.clone(),
                ability_index,
            ));
        }
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
    // CR 117.1b: Priority permits unbounded activation. `pending_activations`
    // is a per-priority-window AI-guard — see `GameState::pending_activations`.
    state.pending_activations.push((source_id, ability_index));
    events.push(GameEvent::AbilityActivated { source_id });

    state.priority_passes.clear();
    state.priority_pass_count = 0;

    Ok(WaitingFor::Priority { player })
}

/// CR 601.2i: If the player is unable or unwilling to complete a cast, the
/// process is reversed: the spell is removed from the stack and any costs
/// paid/choices made are rewound. The engine exposes this as
/// `GameAction::CancelCast` at each interactive WaitingFor step before mana is
/// actually debited.
///
/// For spell casts (distinguished by `activation_ability_index.is_none()`) the
/// StackEntry pushed at announcement (CR 601.2a) is removed here. The object's
/// `zone` field was left at the origin zone across the cast pipeline (see
/// `announce_spell_on_stack` / `finalize_cast` for the rationale), so no zone
/// reversion is needed — the object is already in its origin zone.
/// Activated-ability casts never placed an object on the stack during target
/// selection, so no stack rollback is needed for them.
pub fn handle_cancel_cast(
    state: &mut GameState,
    pending: &PendingCast,
    _events: &mut Vec<GameEvent>,
) {
    state.cancelled_casts.push(pending.object_id);

    if pending.activation_ability_index.is_none() {
        // CR 601.2i: Remove the placeholder stack entry pushed at announcement.
        // No other player can interject between announce and cancel, so the
        // entry is still the topmost object for this cast.
        if let Some(pos) = state
            .stack
            .iter()
            .rposition(|entry| entry.id == pending.object_id)
        {
            state.stack.remove(pos);
        }
    }
}

// Cost payment handlers are in casting_costs module.
pub(crate) use super::casting_costs::{
    handle_discard_for_cost, handle_return_to_hand_for_cost, handle_sacrifice_for_cost,
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

/// CR 101.2: Check if a casting prohibition scope applies to the given caster.
/// Shared by CantBeCast, CantCastDuring, and PerTurnCastLimit.
fn casting_prohibition_scope_matches(
    who: &ProhibitionScope,
    caster: PlayerId,
    source_obj: &super::game_object::GameObject,
    state: &GameState,
) -> bool {
    let _ = source_obj;
    super::static_abilities::prohibition_scope_matches_player(who, caster, source_obj.id, state)
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
    // CR 702.26b + CR 604.1: Functioning gate owned by `battlefield_active_statics`.
    for (bf_obj, def) in super::functioning_abilities::battlefield_active_statics(state) {
        if def.mode != StaticMode::CantCastFrom {
            continue;
        }
        // The affected filter encodes zone restrictions via InAnyZone.
        if let Some(ref filter) = def.affected {
            if super::filter::matches_target_filter(
                state,
                object_id,
                filter,
                &super::filter::FilterContext::from_source(state, bf_obj.id),
            ) {
                return true;
            }
        }
    }
    false
}

/// CR 602.5 + CR 603.2a: Check if any active CantBeActivated static on the battlefield
/// prohibits the given player from activating the given permanent's activated abilities.
/// Each matching static contributes both an activator-axis check (`who` vs caster) AND
/// a permanent-axis check (`source_filter` vs the object whose ability is being activated).
///
/// Per CR 603.2a, this only affects ACTIVATED abilities; triggered abilities are suppressed
/// via the separate `SuppressTriggers` variant.
///
/// CR 605.1a: When the static carries `exemption: ManaAbilities` (Pithing Needle class),
/// abilities classified as mana abilities by the single authority
/// `mana_abilities::is_mana_ability` bypass the prohibition.
///
/// - Chalice of Life (`who=AllPlayers, source_filter=SelfRef`): prohibits Chalice's own
///   activations regardless of controller.
/// - Clarion Conqueror (`who=AllPlayers, source_filter=Artifact/Creature/Planeswalker`):
///   prohibits activation of any artifact/creature/planeswalker's activated abilities.
/// - Karn, the Great Creator (`who=AllPlayers, source_filter=Artifact with ControllerRef::Opponent`):
///   prohibits activation of opponent-controlled artifacts' activated abilities.
/// - Pithing Needle (`source_filter=HasChosenName, exemption=ManaAbilities`): prohibits
///   activation of named-card sources except their mana abilities.
fn is_blocked_by_cant_be_activated(
    state: &GameState,
    caster: PlayerId,
    activating_source_id: ObjectId,
    activating_ability: &AbilityDefinition,
) -> bool {
    // CR 702.26b + CR 604.1: Functioning gate owned by `battlefield_active_statics`.
    for (bf_obj, def) in super::functioning_abilities::battlefield_active_statics(state) {
        let bf_id = bf_obj.id;
        let StaticMode::CantBeActivated {
            ref who,
            ref source_filter,
            ref exemption,
        } = def.mode
        else {
            continue;
        };
        // CR 109.5: The "who" axis — is the caster within the scope?
        if !casting_prohibition_scope_matches(who, caster, bf_obj, state) {
            continue;
        }
        // CR 602.5: The permanent-axis — does the object whose ability is being
        // activated match the static's filter? `ControllerRef` is resolved against
        // the static's source controller (`bf_id`), not the caster.
        let filter_ctx = super::filter::FilterContext::from_source(state, bf_id);
        if !super::filter::matches_target_filter(
            state,
            activating_source_id,
            source_filter,
            &filter_ctx,
        ) {
            continue;
        }
        // CR 605.1a: Apply the exemption gate. Routes through the single
        // `mana_abilities::is_mana_ability` classifier — no duplicated logic.
        match exemption {
            ActivationExemption::None => return true,
            ActivationExemption::ManaAbilities => {
                if !super::mana_abilities::is_mana_ability(activating_ability) {
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

    // CR 702.26b + CR 604.1: Functioning gate owned by `battlefield_active_statics`.
    for (bf_obj, def) in super::functioning_abilities::battlefield_active_statics(state) {
        {
            let StaticMode::CantCastDuring { ref who, ref when } = def.mode else {
                continue;
            };

            // CR 101.2: Check if the caster is in the affected scope.
            if !casting_prohibition_scope_matches(who, caster, bf_obj, state) {
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
                CastingProhibitionCondition::NotDuringYourTurn => {
                    // CR 117.1a + CR 604.1: "can cast spells only during your turn"
                    // = blocked when it is NOT the controller's turn.
                    state.active_player != bf_obj.controller
                }
                CastingProhibitionCondition::NotSorcerySpeed => {
                    // CR 117.1: "can cast spells only any time they could cast a sorcery"
                    // Blocked when NOT at sorcery speed: active player's main phase + empty stack.
                    let at_sorcery_speed = state.active_player == caster
                        && matches!(state.phase, Phase::PreCombatMain | Phase::PostCombatMain)
                        && state.stack.is_empty();
                    !at_sorcery_speed
                }
            };
            if condition_met {
                return true;
            }
        }
    }
    false
}

/// CR 101.2: Check if any CantBeCast static on the battlefield prevents
/// the given player from casting the given spell.
/// Handles scope-based checks (opponents, all players, controller, enchanted creature's
/// controller) and filter-based checks (type, mana value, chosen name, chosen card type).
fn is_blocked_by_cant_be_cast(
    state: &GameState,
    caster: PlayerId,
    spell_obj: &super::game_object::GameObject,
) -> bool {
    // CR 702.26b + CR 604.1: Functioning gate owned by `battlefield_active_statics`
    // — including the per-static `condition` check; no inline duplication needed.
    for (bf_obj, def) in super::functioning_abilities::battlefield_active_statics(state) {
        let StaticMode::CantBeCast { ref who } = def.mode else {
            continue;
        };

        // CR 101.2: Check if the caster is in the affected scope.
        if !casting_prohibition_scope_matches(who, caster, bf_obj, state) {
            continue;
        }

        // CR 604.1: Check spell filter if present.
        if let Some(ref affected) = def.affected {
            if !cant_cast_filter_matches(state, spell_obj, affected, bf_obj) {
                continue;
            }
        }

        return true;
    }
    false
}

/// CR 101.2: Check if a spell matches a CantBeCast affected filter.
/// Handles type filters, mana value comparisons, chosen name, and chosen card type.
/// Source-dependent filters (HasChosenName, IsChosenCardType) are resolved here
/// because they need the source permanent's chosen attributes.
fn cant_cast_filter_matches(
    state: &GameState,
    spell_obj: &super::game_object::GameObject,
    filter: &TargetFilter,
    source_obj: &super::game_object::GameObject,
) -> bool {
    use crate::types::ability::{ChosenAttribute, FilterProp};

    match filter {
        // CR 201.2: "spells with the chosen name" — match spell name against source's chosen name.
        TargetFilter::HasChosenName => {
            let chosen_name = source_obj.chosen_attributes.iter().find_map(|a| match a {
                ChosenAttribute::CardName(n) => Some(n.as_str()),
                _ => None,
            });
            chosen_name.is_some_and(|name| name.eq_ignore_ascii_case(&spell_obj.name))
        }
        // CR 205: Typed filter with IsChosenCardType requires source's chosen card type.
        TargetFilter::Typed(tf)
            if tf
                .properties
                .iter()
                .any(|p| matches!(p, FilterProp::IsChosenCardType)) =>
        {
            let chosen_type = source_obj.chosen_attributes.iter().find_map(|a| match a {
                ChosenAttribute::CardType(ct) => Some(ct),
                _ => None,
            });
            let Some(chosen_type) = chosen_type else {
                return false;
            };
            spell_obj
                .card_types
                .core_types
                .iter()
                .any(|ct| ct == chosen_type)
        }
        // All other filters delegate to the spell record matcher.
        _ => {
            let record = SpellCastRecord {
                core_types: spell_obj.card_types.core_types.clone(),
                supertypes: spell_obj.card_types.supertypes.clone(),
                subtypes: spell_obj.card_types.subtypes.clone(),
                keywords: spell_obj.keywords.clone(),
                colors: spell_obj.color.clone(),
                mana_value: spell_obj.mana_cost.mana_value(),
                has_x_in_cost: super::casting_costs::cost_has_x(&spell_obj.mana_cost),
            };
            super::filter::spell_record_matches_filter(
                &record,
                filter,
                source_obj.controller,
                &state.all_creature_types,
            )
        }
    }
}

/// CR 101.2 + CR 604.1: Check if any PerTurnCastLimit static on the battlefield prevents
/// the given player from casting the given spell this turn.
/// E.g., Rule of Law: "Each player can't cast more than one spell each turn."
/// E.g., Deafening Silence: "Each player can't cast more than one noncreature spell each turn."
fn is_blocked_by_per_turn_cast_limit(
    state: &GameState,
    caster: PlayerId,
    spell_obj: &super::game_object::GameObject,
) -> bool {
    // CR 702.26b + CR 604.1: Functioning gate owned by `battlefield_active_statics`.
    for (bf_obj, def) in super::functioning_abilities::battlefield_active_statics(state) {
        {
            let StaticMode::PerTurnCastLimit {
                ref who,
                max,
                ref spell_filter,
            } = def.mode
            else {
                continue;
            };

            // CR 101.2: Check if the caster is in the affected scope.
            if !casting_prohibition_scope_matches(who, caster, bf_obj, state) {
                continue;
            }

            // If a spell filter is set, first check if the spell being cast matches.
            // E.g., Deafening Silence only limits noncreature spells — creature spells
            // are unaffected regardless of how many noncreature spells were cast.
            if let Some(filter) = spell_filter {
                let current_record = SpellCastRecord {
                    core_types: spell_obj.card_types.core_types.clone(),
                    supertypes: spell_obj.card_types.supertypes.clone(),
                    subtypes: spell_obj.card_types.subtypes.clone(),
                    keywords: spell_obj.keywords.clone(),
                    colors: spell_obj.color.clone(),
                    mana_value: spell_obj.mana_cost.mana_value(),
                    has_x_in_cost: super::casting_costs::cost_has_x(&spell_obj.mana_cost),
                };
                if !super::filter::spell_record_matches_filter(
                    &current_record,
                    filter,
                    bf_obj.controller,
                    &state.all_creature_types,
                ) {
                    continue;
                }
            }

            // Count matching spells already cast this turn by this player.
            // The current spell has not yet been recorded (recording happens in
            // finalize_cast), so this correctly counts only prior spells.
            let cast_count = state
                .spells_cast_this_turn_by_player
                .get(&caster)
                .map(|records| match spell_filter {
                    None => records.len(),
                    Some(filter) => records
                        .iter()
                        .filter(|r| {
                            super::filter::spell_record_matches_filter(
                                r,
                                filter,
                                bf_obj.controller,
                                &state.all_creature_types,
                            )
                        })
                        .count(),
                })
                .unwrap_or(0);

            if cast_count >= max as usize {
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
    use crate::parser::oracle_static::parse_static_line;
    use crate::types::ability::{
        ActivationRestriction, BasicLandType, ChosenAttribute, ChosenSubtypeKind,
        ContinuousModification, ControllerRef, GameRestriction, ManaContribution, ManaProduction,
        QuantityExpr, RestrictionExpiry, RestrictionPlayerScope, StaticDefinition, TargetFilter,
        TypeFilter, TypedFilter,
    };
    use crate::types::actions::GameAction;
    use crate::types::card_type::CoreType;
    use crate::types::events::GameEvent;
    use crate::types::keywords::{FlashbackCost, Keyword, KeywordKind};
    use crate::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType, ManaUnit};
    use crate::types::phase::Phase;
    use std::sync::Arc;

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
                grants: vec![],
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

    fn add_brushland_like_land(
        state: &mut GameState,
        card_id: CardId,
        name: &str,
        controller_harm: bool,
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
        Arc::make_mut(&mut obj.abilities).push(
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
        let colored = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Mana {
                produced: ManaProduction::AnyOneColor {
                    count: QuantityExpr::Fixed { value: 1 },
                    color_options: vec![ManaColor::Green, ManaColor::White],
                    contribution: ManaContribution::Base,
                },
                restrictions: vec![],
                grants: vec![],
                expiry: None,
                target: None,
            },
        )
        .cost(AbilityCost::Tap);
        Arc::make_mut(&mut obj.abilities).push(if controller_harm {
            colored.sub_ability(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::DealDamage {
                    amount: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::Controller,
                    damage_source: None,
                },
            ))
        } else {
            colored
        });
        land
    }

    fn create_single_color_spell_in_hand(
        state: &mut GameState,
        card_id: CardId,
        name: &str,
        shard: ManaCostShard,
    ) -> ObjectId {
        let obj_id = create_object(state, card_id, PlayerId(0), name.to_string(), Zone::Hand);
        let obj = state.objects.get_mut(&obj_id).unwrap();
        obj.card_types.core_types.push(CoreType::Instant);
        Arc::make_mut(&mut obj.abilities).push(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: crate::types::ability::TargetFilter::Controller,
            },
        ));
        obj.mana_cost = ManaCost::Cost {
            shards: vec![shard],
            generic: 0,
        };
        obj_id
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
            Arc::make_mut(&mut obj.abilities).push(AbilityDefinition::new(
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

    #[test]
    fn prepare_spell_cast_chains_all_non_modal_spell_abilities_in_order() {
        let mut state = setup_game_at_main_phase();
        let obj_id = create_object(
            &mut state,
            CardId(11),
            PlayerId(0),
            "Two-Step Spell".to_string(),
            Zone::Hand,
        );
        let obj = state.objects.get_mut(&obj_id).unwrap();
        obj.card_types.core_types.push(CoreType::Instant);
        Arc::make_mut(&mut obj.abilities).push(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Scry {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
        ));
        Arc::make_mut(&mut obj.abilities).push(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
        ));

        let prepared = prepare_spell_cast(&state, PlayerId(0), obj_id).unwrap();
        let combined = prepared
            .ability_def
            .expect("non-modal instant should prepare a spell ability");
        assert!(matches!(*combined.effect, Effect::Scry { .. }));
        let sub = combined
            .sub_ability
            .as_ref()
            .expect("second spell ability should be chained");
        assert!(matches!(*sub.effect, Effect::Draw { .. }));
    }

    #[test]
    fn can_cast_object_now_checks_targets_across_all_non_modal_spell_abilities() {
        let mut state = setup_game_at_main_phase();
        let obj_id = create_object(
            &mut state,
            CardId(12),
            PlayerId(0),
            "Draw Then Doom".to_string(),
            Zone::Hand,
        );
        let obj = state.objects.get_mut(&obj_id).unwrap();
        obj.card_types.core_types.push(CoreType::Sorcery);
        Arc::make_mut(&mut obj.abilities).push(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
        ));
        Arc::make_mut(&mut obj.abilities).push(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Destroy {
                target: TargetFilter::Typed(TypedFilter::creature()),
                cant_regenerate: false,
            },
        ));

        let castable = can_cast_object_now(&state, PlayerId(0), obj_id);
        assert!(
            !castable,
            "later spell abilities with unresolved targets must still gate castability"
        );
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
            Arc::make_mut(&mut obj.abilities).push(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Draw {
                    count: QuantityExpr::Fixed { value: 2 },
                    target: TargetFilter::Controller,
                },
            ));
            obj.mana_cost = ManaCost::Cost {
                shards: vec![ManaCostShard::Blue],
                generic: 2,
            };
        }
        obj_id
    }

    fn create_creature_spell_in_hand(state: &mut GameState, player: PlayerId) -> ObjectId {
        let obj_id = create_object(
            state,
            CardId(22),
            player,
            "Hill Giant".to_string(),
            Zone::Hand,
        );
        {
            let obj = state.objects.get_mut(&obj_id).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            Arc::make_mut(&mut obj.abilities).push(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Unimplemented {
                    name: "Creature".to_string(),
                    description: None,
                },
            ));
            obj.mana_cost = ManaCost::Cost {
                shards: vec![ManaCostShard::Red],
                generic: 3,
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
        Arc::make_mut(&mut obj.abilities).push(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Mana {
                    produced: crate::types::ability::ManaProduction::Fixed {
                        colors: vec![ManaColor::Blue],
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
                AbilityKind::Activated,
                Effect::Mana {
                    produced: crate::types::ability::ManaProduction::Fixed {
                        colors: vec![ManaColor::Black],
                        contribution: ManaContribution::Base,
                    },
                    restrictions: vec![],
                    grants: vec![],
                    expiry: None,
                    target: None,
                },
            )
            .cost(crate::types::ability::AbilityCost::Tap)
            .activation_restrictions(vec![
                crate::types::ability::ActivationRestriction::RequiresCondition {
                    condition: crate::parser::oracle_condition::parse_restriction_condition(
                        "you control an Island or a Swamp",
                    ),
                },
            ]),
        );
        obj_id
    }

    fn create_starting_town(state: &mut GameState, player: PlayerId, card_id: CardId) -> ObjectId {
        let obj_id = create_object(
            state,
            card_id,
            player,
            "Starting Town".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&obj_id).unwrap();
        obj.card_types.core_types.push(CoreType::Land);
        Arc::make_mut(&mut obj.abilities).push(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Mana {
                    produced: crate::types::ability::ManaProduction::Colorless {
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
        Arc::make_mut(&mut obj.abilities).push(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Mana {
                    produced: crate::types::ability::ManaProduction::AnyOneColor {
                        count: QuantityExpr::Fixed { value: 1 },
                        color_options: vec![ManaColor::White, ManaColor::Blue],
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
                    AbilityCost::PayLife {
                        amount: QuantityExpr::Fixed { value: 1 },
                    },
                ],
            }),
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
        Arc::make_mut(&mut obj.abilities).push(
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

    /// CR 107.1b + CR 601.2f: Casting a spell with X in its cost enters
    /// `ChooseXValue` first; after `ChooseX(n)` the cost is concretized and
    /// payment proceeds against the now-definite total.
    #[test]
    fn x_cost_spell_prompts_for_x_then_pays_concretized_cost() {
        use super::super::engine::apply_as_current;
        use crate::types::ability::QuantityRef;

        let mut state = setup_game_at_main_phase();

        // Build an X-cost sorcery: cost {X}{G}{G}, effect "Draw X cards"
        // (stand-in for the Nature's Rhythm pattern where resolution reads X).
        let obj_id = create_object(
            &mut state,
            CardId(900),
            PlayerId(0),
            "Synthetic X Sorcery".to_string(),
            Zone::Hand,
        );
        {
            let obj = state.objects.get_mut(&obj_id).unwrap();
            obj.card_types.core_types.push(CoreType::Sorcery);
            Arc::make_mut(&mut obj.abilities).push(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Draw {
                    count: QuantityExpr::Ref {
                        qty: QuantityRef::Variable {
                            name: "X".to_string(),
                        },
                    },
                    target: TargetFilter::Controller,
                },
            ));
            obj.mana_cost = ManaCost::Cost {
                shards: vec![ManaCostShard::X, ManaCostShard::Green, ManaCostShard::Green],
                generic: 0,
            };
        }

        // Pool: 5 green. Fixed portion is GG = 2. Pool alone gives max X = 3.
        add_mana(&mut state, PlayerId(0), ManaType::Green, 5);

        // Cast — expect ChooseXValue (not ManaPayment).
        let result = apply_as_current(
            &mut state,
            GameAction::CastSpell {
                object_id: obj_id,
                card_id: CardId(900),
                targets: vec![],
            },
        )
        .unwrap();
        let max = match result.waiting_for {
            WaitingFor::ChooseXValue { max, .. } => max,
            other => panic!("expected ChooseXValue, got {other:?}"),
        };
        assert_eq!(max, 3, "pool of 5 minus fixed GG=2 should bound X at 3");

        // Seed 3 cards in library so Draw can succeed at resolution.
        for i in 0..3 {
            create_object(
                &mut state,
                CardId(910 + i),
                PlayerId(0),
                format!("Library Card {i}"),
                Zone::Library,
            );
        }

        // Commit X = 3. Because the concretized cost `{3}{G}{G}` contains no
        // hybrid/Phyrexian shards and convoke is inactive, `enter_payment_step`
        // classifies payment as Unambiguous and auto-finalizes — the spell goes
        // straight to the stack without a `ManaPayment` round trip.
        let result = apply_as_current(&mut state, GameAction::ChooseX { value: 3 }).unwrap();
        assert!(
            !matches!(result.waiting_for, WaitingFor::ManaPayment { .. }),
            "auto-pay should skip ManaPayment for unambiguous concretized costs"
        );
        assert!(
            result
                .events
                .iter()
                .any(|e| matches!(e, GameEvent::XValueChosen { value: 3, .. })),
            "should emit XValueChosen event"
        );
        assert_eq!(
            state.players[0].hand.len(),
            0,
            "spell moved from hand to stack"
        );
        assert_eq!(state.stack.len(), 1, "spell on stack after auto-pay");
        assert!(
            state.pending_cast.is_none(),
            "pending_cast is consumed by auto-finalization"
        );

        for _ in 0..4 {
            if state.stack.is_empty() && matches!(state.waiting_for, WaitingFor::Priority { .. }) {
                break;
            }
            let _ = apply_as_current(&mut state, GameAction::PassPriority).unwrap();
        }
        let hand_after = state.players[0].hand.len();
        assert_eq!(
            hand_after, 3,
            "X=3 should result in drawing 3 cards at resolution (hand_after={hand_after})"
        );
    }

    /// CR 601.2f: Player can cancel a cast before committing to an X value.
    #[test]
    fn x_cost_cancellation_returns_to_priority() {
        use super::super::engine::apply_as_current;
        use crate::types::ability::QuantityRef;

        let mut state = setup_game_at_main_phase();
        let obj_id = create_object(
            &mut state,
            CardId(901),
            PlayerId(0),
            "Synthetic X Sorcery".to_string(),
            Zone::Hand,
        );
        {
            let obj = state.objects.get_mut(&obj_id).unwrap();
            obj.card_types.core_types.push(CoreType::Sorcery);
            Arc::make_mut(&mut obj.abilities).push(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Draw {
                    count: QuantityExpr::Ref {
                        qty: QuantityRef::Variable {
                            name: "X".to_string(),
                        },
                    },
                    target: TargetFilter::Controller,
                },
            ));
            obj.mana_cost = ManaCost::Cost {
                shards: vec![ManaCostShard::X],
                generic: 0,
            };
        }
        add_mana(&mut state, PlayerId(0), ManaType::Colorless, 3);

        let result = apply_as_current(
            &mut state,
            GameAction::CastSpell {
                object_id: obj_id,
                card_id: CardId(901),
                targets: vec![],
            },
        )
        .unwrap();
        assert!(matches!(
            result.waiting_for,
            WaitingFor::ChooseXValue { .. }
        ));

        let result = apply_as_current(&mut state, GameAction::CancelCast).unwrap();
        assert!(matches!(result.waiting_for, WaitingFor::Priority { .. }));
        assert!(state.pending_cast.is_none());
        assert!(!state.players[0].hand.is_empty(), "spell returned to hand");
    }

    /// Blaze pattern (CR 107.1b): {X}{R} "Deal X damage to target creature."
    /// Validates that Effect::DealDamage resolves X via ability context
    /// (not the deprecated last_named_choice fallback).
    #[test]
    fn x_cost_deal_x_damage_lands_for_chosen_x() {
        use super::super::engine::apply_as_current;
        use crate::types::ability::{QuantityRef, TargetFilter};

        let mut state = setup_game_at_main_phase();

        // Create a creature to target.
        let creature = create_object(
            &mut state,
            CardId(990),
            PlayerId(1),
            "Target Creature".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&creature).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.power = Some(5);
            obj.toughness = Some(5);
        }

        let obj_id = create_object(
            &mut state,
            CardId(903),
            PlayerId(0),
            "Synthetic Blaze".to_string(),
            Zone::Hand,
        );
        {
            let obj = state.objects.get_mut(&obj_id).unwrap();
            obj.card_types.core_types.push(CoreType::Sorcery);
            Arc::make_mut(&mut obj.abilities).push(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::DealDamage {
                    amount: QuantityExpr::Ref {
                        qty: QuantityRef::Variable {
                            name: "X".to_string(),
                        },
                    },
                    target: TargetFilter::Any,
                    damage_source: None,
                },
            ));
            obj.mana_cost = ManaCost::Cost {
                shards: vec![ManaCostShard::X, ManaCostShard::Red],
                generic: 0,
            };
        }
        add_mana(&mut state, PlayerId(0), ManaType::Red, 5);

        let result = apply_as_current(
            &mut state,
            GameAction::CastSpell {
                object_id: obj_id,
                card_id: CardId(903),
                targets: vec![],
            },
        )
        .unwrap();
        assert!(matches!(
            result.waiting_for,
            WaitingFor::TargetSelection { .. }
        ));

        // Select the creature as target — flow then advances to ChooseXValue.
        let result = apply_as_current(
            &mut state,
            GameAction::SelectTargets {
                targets: vec![crate::types::ability::TargetRef::Object(creature)],
            },
        )
        .unwrap();
        let max = match result.waiting_for {
            WaitingFor::ChooseXValue { max, .. } => max,
            other => panic!("expected ChooseXValue after targets selected, got {other:?}"),
        };
        assert_eq!(max, 4, "pool=5 minus fixed R=1 should bound X at 4");

        apply_as_current(&mut state, GameAction::ChooseX { value: 4 }).unwrap();

        // Drive priority passes until the stack resolves.
        for _ in 0..5 {
            if state.stack.is_empty() && matches!(state.waiting_for, WaitingFor::Priority { .. }) {
                break;
            }
            let _ = apply_as_current(&mut state, GameAction::PassPriority).unwrap();
        }

        // X=4 damage applied to a 5-toughness creature — marked damage or destroyed.
        // Check via the DamageDealt events the flow emitted.
        // The creature should have damage_marked == 4 (or be in graveyard if damage >= toughness).
        // Here 4 < 5, so it's still on battlefield with damage marked.
        let target = state
            .objects
            .get(&creature)
            .expect("creature still on battlefield");
        assert_eq!(
            target.damage_marked, 4,
            "X=4 should mark 4 damage on the target (actual={})",
            target.damage_marked
        );
    }

    /// Passing priority during `ChooseXValue` is illegal — caster must commit
    /// or cancel (CR 601.2f).
    #[test]
    fn x_cost_pass_priority_rejected_during_choose_x() {
        use super::super::engine::apply_as_current;
        use crate::types::ability::QuantityRef;

        let mut state = setup_game_at_main_phase();
        let obj_id = create_object(
            &mut state,
            CardId(905),
            PlayerId(0),
            "Synthetic X".to_string(),
            Zone::Hand,
        );
        {
            let obj = state.objects.get_mut(&obj_id).unwrap();
            obj.card_types.core_types.push(CoreType::Sorcery);
            Arc::make_mut(&mut obj.abilities).push(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Draw {
                    count: QuantityExpr::Ref {
                        qty: QuantityRef::Variable {
                            name: "X".to_string(),
                        },
                    },
                    target: TargetFilter::Controller,
                },
            ));
            obj.mana_cost = ManaCost::Cost {
                shards: vec![ManaCostShard::X],
                generic: 0,
            };
        }
        add_mana(&mut state, PlayerId(0), ManaType::Colorless, 2);

        apply_as_current(
            &mut state,
            GameAction::CastSpell {
                object_id: obj_id,
                card_id: CardId(905),
                targets: vec![],
            },
        )
        .unwrap();
        assert!(matches!(state.waiting_for, WaitingFor::ChooseXValue { .. }));

        let result = apply_as_current(&mut state, GameAction::PassPriority);
        assert!(
            result.is_err(),
            "PassPriority must be rejected during ChooseXValue"
        );
    }

    /// AI legal actions: `candidates::candidate_actions_broad` enumerates
    /// every legal X value (0..=max) when in `ChooseXValue`.
    #[test]
    fn x_cost_ai_enumerates_full_range() {
        use crate::ai_support::candidate_actions_broad;
        use crate::types::ability::QuantityRef;

        let mut state = setup_game_at_main_phase();
        let obj_id = create_object(
            &mut state,
            CardId(906),
            PlayerId(0),
            "Synthetic X".to_string(),
            Zone::Hand,
        );
        {
            let obj = state.objects.get_mut(&obj_id).unwrap();
            obj.card_types.core_types.push(CoreType::Sorcery);
            Arc::make_mut(&mut obj.abilities).push(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Draw {
                    count: QuantityExpr::Ref {
                        qty: QuantityRef::Variable {
                            name: "X".to_string(),
                        },
                    },
                    target: TargetFilter::Controller,
                },
            ));
            obj.mana_cost = ManaCost::Cost {
                shards: vec![ManaCostShard::X],
                generic: 0,
            };
        }
        add_mana(&mut state, PlayerId(0), ManaType::Colorless, 3);

        super::super::engine::apply_as_current(
            &mut state,
            GameAction::CastSpell {
                object_id: obj_id,
                card_id: CardId(906),
                targets: vec![],
            },
        )
        .unwrap();

        let max = match state.waiting_for {
            WaitingFor::ChooseXValue { max, .. } => max,
            ref other => panic!("expected ChooseXValue, got {other:?}"),
        };
        assert_eq!(max, 3);

        let candidates = candidate_actions_broad(&state);
        let choose_x: Vec<u32> = candidates
            .iter()
            .filter_map(|c| match c.action {
                GameAction::ChooseX { value } => Some(value),
                _ => None,
            })
            .collect();
        assert_eq!(
            choose_x,
            vec![0, 1, 2, 3],
            "AI should enumerate one ChooseX candidate per legal value"
        );
    }

    /// `ChooseXValue` preserves `convoke_mode` so a spell with both X and
    /// Waterbend/Convoke reaches `ManaPayment` with the mode intact.
    #[test]
    fn x_cost_preserves_convoke_mode_through_choice() {
        use crate::game::casting_costs::enter_payment_step;
        use crate::types::ability::QuantityRef;

        let mut state = setup_game_at_main_phase();
        // Construct a pending cast directly so we can exercise enter_payment_step
        // with a non-None convoke_mode (normal flow doesn't compose X+convoke
        // without extra setup, but the helper is the single authority that must
        // thread convoke_mode through).
        let mut pending = PendingCast::new(
            ObjectId(123),
            CardId(0),
            ResolvedAbility::new(
                Effect::Draw {
                    count: QuantityExpr::Ref {
                        qty: QuantityRef::Variable {
                            name: "X".to_string(),
                        },
                    },
                    target: TargetFilter::Controller,
                },
                Vec::new(),
                ObjectId(123),
                PlayerId(0),
            ),
            ManaCost::Cost {
                shards: vec![ManaCostShard::X],
                generic: 0,
            },
        );
        pending.activation_ability_index = None;
        state.pending_cast = Some(Box::new(pending));

        let mut events = Vec::new();
        let waiting = enter_payment_step(
            &mut state,
            PlayerId(0),
            Some(ConvokeMode::Waterbend),
            &mut events,
        )
        .expect("enter_payment_step should succeed for X + Waterbend pending cast");
        match waiting {
            WaitingFor::ChooseXValue { convoke_mode, .. } => {
                assert_eq!(
                    convoke_mode,
                    Some(ConvokeMode::Waterbend),
                    "convoke_mode must pass through ChooseXValue"
                );
            }
            other => panic!("expected ChooseXValue, got {other:?}"),
        }
    }

    /// Activated abilities with composite costs like `Tap + Pay {X}` must route
    /// through ChooseXValue (X is chosen before mana payment per CR 601.2f), and
    /// the Tap sub-cost must be deferred to `activation_cost` so it is paid after
    /// ManaPayment completes — not during the announce-X phase.
    #[test]
    fn x_cost_activated_composite_tap_prompts_for_x_and_taps_on_resolution() {
        use super::super::engine::apply_as_current;
        use crate::types::ability::{AbilityCost, QuantityRef};

        let mut state = setup_game_at_main_phase();
        let source = create_object(
            &mut state,
            CardId(950),
            PlayerId(0),
            "Composite X Relic".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&source).unwrap();
            obj.card_types.core_types.push(CoreType::Artifact);
            Arc::make_mut(&mut obj.abilities).push(
                AbilityDefinition::new(
                    AbilityKind::Activated,
                    Effect::Draw {
                        count: QuantityExpr::Ref {
                            qty: QuantityRef::Variable {
                                name: "X".to_string(),
                            },
                        },
                        target: TargetFilter::Controller,
                    },
                )
                .cost(AbilityCost::Composite {
                    costs: vec![
                        AbilityCost::Tap,
                        AbilityCost::Mana {
                            cost: ManaCost::Cost {
                                shards: vec![ManaCostShard::X],
                                generic: 0,
                            },
                        },
                    ],
                }),
            );
        }
        add_mana(&mut state, PlayerId(0), ManaType::Colorless, 2);

        // Activate — expect ChooseXValue, source not yet tapped.
        apply_as_current(
            &mut state,
            GameAction::ActivateAbility {
                source_id: source,
                ability_index: 0,
            },
        )
        .unwrap();
        let max = match state.waiting_for {
            WaitingFor::ChooseXValue { max, .. } => max,
            ref other => panic!("expected ChooseXValue, got {other:?}"),
        };
        assert_eq!(max, 2, "pool of 2 bounds X at 2");
        assert!(
            !state.objects[&source].tapped,
            "source must not be tapped before ManaPayment completes"
        );
        let pending = state.pending_cast.as_ref().expect("pending cast present");
        assert!(
            matches!(pending.activation_cost, Some(AbilityCost::Tap)),
            "activation_cost must hold the deferred Tap sub-cost, got {:?}",
            pending.activation_cost
        );

        // Commit X = 1. The concretized mana cost is `{1}` (pure generic), so
        // `enter_payment_step` auto-finalizes: mana pays, the deferred Tap
        // activation cost fires, and the ability lands on the stack — all within
        // the single `ChooseX` action, no intermediate `ManaPayment` round trip.
        apply_as_current(&mut state, GameAction::ChooseX { value: 1 }).unwrap();
        assert!(
            !matches!(state.waiting_for, WaitingFor::ManaPayment { .. }),
            "auto-pay should skip ManaPayment when the concretized cost is unambiguous"
        );
        assert!(
            state.objects[&source].tapped,
            "source must be tapped — auto-finalization paid mana and the deferred Tap"
        );
        assert_eq!(
            state.stack.len(),
            1,
            "activated ability on stack after auto-pay"
        );
    }

    #[test]
    fn activated_discard_cost_prompts_and_resumes_activation() {
        use super::super::engine::apply_as_current;
        use crate::types::ability::{AbilityCost, AbilityKind, Effect};

        let mut state = setup_game_at_main_phase();
        let blood = create_object(
            &mut state,
            CardId(970),
            PlayerId(0),
            "Blood".to_string(),
            Zone::Battlefield,
        );
        let discarded = create_object(
            &mut state,
            CardId(971),
            PlayerId(0),
            "Discard Me".to_string(),
            Zone::Hand,
        );
        {
            let obj = state.objects.get_mut(&blood).unwrap();
            obj.card_types.core_types.push(CoreType::Artifact);
            obj.card_types.subtypes.push("Blood".to_string());
            Arc::make_mut(&mut obj.abilities).push(
                AbilityDefinition::new(
                    AbilityKind::Activated,
                    Effect::Draw {
                        count: QuantityExpr::Fixed { value: 1 },
                        target: TargetFilter::Controller,
                    },
                )
                .cost(AbilityCost::Composite {
                    costs: vec![
                        AbilityCost::Mana {
                            cost: ManaCost::generic(1),
                        },
                        AbilityCost::Tap,
                        AbilityCost::Discard {
                            count: QuantityExpr::Fixed { value: 1 },
                            filter: None,
                            random: false,
                            self_ref: false,
                        },
                        AbilityCost::Sacrifice {
                            target: TargetFilter::SelfRef,
                            count: 1,
                        },
                    ],
                }),
            );
        }
        add_mana(&mut state, PlayerId(0), ManaType::Colorless, 1);

        apply_as_current(
            &mut state,
            GameAction::ActivateAbility {
                source_id: blood,
                ability_index: 0,
            },
        )
        .unwrap();

        match &state.waiting_for {
            WaitingFor::DiscardForCost { cards, count, .. } => {
                assert_eq!(*count, 1);
                assert_eq!(cards, &vec![discarded]);
            }
            other => panic!("expected DiscardForCost, got {other:?}"),
        }
        assert!(
            state.objects[&blood].zone == Zone::Battlefield,
            "cost payment must pause before tapping or sacrificing the source"
        );

        apply_as_current(
            &mut state,
            GameAction::SelectCards {
                cards: vec![discarded],
            },
        )
        .unwrap();

        assert_eq!(state.objects[&discarded].zone, Zone::Graveyard);
        assert_eq!(state.objects[&blood].zone, Zone::Graveyard);
        assert_eq!(state.stack.len(), 1);
        assert!(matches!(
            state.stack[0].kind,
            StackEntryKind::ActivatedAbility { source_id, .. } if source_id == blood
        ));
    }

    /// Composite costs with Sacrifice + Pay {X}{G}: the fixed G contributes to
    /// the cost floor, so `max_x_value` computes the available X after reserving
    /// 1 mana for the G shard. The Sacrifice sub-cost is deferred to
    /// `activation_cost` and consumed during stack push.
    #[test]
    fn x_cost_activated_composite_sacrifice_bounds_x_by_fixed_portion() {
        use crate::types::ability::{AbilityCost, QuantityRef, TargetFilter};

        let mut state = setup_game_at_main_phase();
        let source = create_object(
            &mut state,
            CardId(951),
            PlayerId(0),
            "Composite X Sac Altar".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&source).unwrap();
            obj.card_types.core_types.push(CoreType::Artifact);
            Arc::make_mut(&mut obj.abilities).push(
                AbilityDefinition::new(
                    AbilityKind::Activated,
                    Effect::Draw {
                        count: QuantityExpr::Ref {
                            qty: QuantityRef::Variable {
                                name: "X".to_string(),
                            },
                        },
                        target: TargetFilter::Controller,
                    },
                )
                .cost(AbilityCost::Composite {
                    costs: vec![
                        AbilityCost::Sacrifice {
                            target: TargetFilter::SelfRef,
                            count: 1,
                        },
                        AbilityCost::Mana {
                            cost: ManaCost::Cost {
                                shards: vec![ManaCostShard::X, ManaCostShard::Green],
                                generic: 0,
                            },
                        },
                    ],
                }),
            );
        }
        // Pool: 1 green + 3 colorless = 4 total. Fixed portion = G (1).
        // Max X = (4 - 1) / 1 = 3.
        add_mana(&mut state, PlayerId(0), ManaType::Green, 1);
        add_mana(&mut state, PlayerId(0), ManaType::Colorless, 3);

        super::super::engine::apply_as_current(
            &mut state,
            GameAction::ActivateAbility {
                source_id: source,
                ability_index: 0,
            },
        )
        .unwrap();
        let max = match state.waiting_for {
            WaitingFor::ChooseXValue { max, .. } => max,
            ref other => panic!("expected ChooseXValue, got {other:?}"),
        };
        assert_eq!(
            max, 3,
            "pool 4 minus fixed G=1 should bound X at 3 for composite Sacrifice + {{X}}{{G}}"
        );

        // activation_cost should hold the deferred Sacrifice sub-cost.
        let pending = state.pending_cast.as_ref().expect("pending cast present");
        assert!(
            matches!(
                pending.activation_cost,
                Some(AbilityCost::Sacrifice {
                    target: TargetFilter::SelfRef,
                    count: 1
                })
            ),
            "activation_cost must hold the deferred Sacrifice sub-cost, got {:?}",
            pending.activation_cost
        );
    }

    /// CR 117.7 + CR 601.2f: A self-spell cost reduction printed on the card itself
    /// ("This spell costs {1} less to cast for each instant and sorcery card in your
    /// graveyard.") must fire while the card is in hand. Verifies the parser-emitted
    /// static (affected = SelfRef, active_zones = [Hand, Stack]) is picked up by the
    /// casting-time scanner and reduces the spell's generic cost.
    #[test]
    fn tolarian_terror_self_cost_reduction_applies_from_hand() {
        use crate::types::statics::StaticMode;

        let mut state = setup_game_at_main_phase();
        let obj_id = create_object(
            &mut state,
            CardId(990),
            PlayerId(0),
            "Tolarian Terror".to_string(),
            Zone::Hand,
        );
        {
            let obj = state.objects.get_mut(&obj_id).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.mana_cost = ManaCost::Cost {
                shards: vec![ManaCostShard::Blue],
                generic: 6,
            };
            // Self-spell cost reduction as the parser emits it: 1 generic per qualifying
            // card in the graveyard, affected = SelfRef, active in Hand/Stack.
            use crate::types::ability::{CountScope, QuantityRef, ZoneRef};
            let mut def = StaticDefinition::new(StaticMode::ReduceCost {
                amount: ManaCost::generic(1),
                spell_filter: None,
                dynamic_count: Some(QuantityRef::ZoneCardCount {
                    zone: ZoneRef::Graveyard,
                    card_types: vec![TypeFilter::Instant, TypeFilter::Sorcery],
                    scope: CountScope::Controller,
                }),
            })
            .affected(TargetFilter::SelfRef);
            def.active_zones = vec![Zone::Hand, Zone::Stack];
            obj.static_definitions.push(def);
        }

        // Seed three instants/sorceries into the controller's graveyard.
        for (i, ct) in [CoreType::Instant, CoreType::Sorcery, CoreType::Instant]
            .into_iter()
            .enumerate()
        {
            let id = create_object(
                &mut state,
                CardId(900 + i as u64),
                PlayerId(0),
                format!("GY{i}"),
                Zone::Graveyard,
            );
            state
                .objects
                .get_mut(&id)
                .unwrap()
                .card_types
                .core_types
                .push(ct);
        }

        let player = PlayerId(0);
        let mut mana_cost = state.objects.get(&obj_id).unwrap().mana_cost.clone();
        super::super::casting::apply_self_spell_cost_modifiers(
            &state,
            player,
            obj_id,
            &mut mana_cost,
        );

        // Printed cost 6 generic; three qualifying cards should reduce by 3 → 3 generic.
        match mana_cost {
            ManaCost::Cost { generic, .. } => assert_eq!(
                generic, 3,
                "3 qualifying graveyard cards should reduce generic from 6 to 3, got {generic}"
            ),
            other => panic!("expected ManaCost::Cost, got {other:?}"),
        }
    }

    /// CR 601.2f: Cost reductions are applied during cost determination (before
    /// `enter_payment_step` runs), so `max_x_value` sees the reduced cost and
    /// bounds X accordingly. A pending "next spell costs {1} less" reduction on
    /// a {X}{2}{G} spell raises the affordable X by 1.
    #[test]
    fn x_cost_accounts_for_pending_cost_reduction_in_max() {
        use super::super::engine::apply_as_current;
        use crate::types::ability::QuantityRef;
        use crate::types::game_state::PendingSpellCostReduction;

        let mut state = setup_game_at_main_phase();
        let obj_id = create_object(
            &mut state,
            CardId(960),
            PlayerId(0),
            "Synthetic X Reduced".to_string(),
            Zone::Hand,
        );
        {
            let obj = state.objects.get_mut(&obj_id).unwrap();
            obj.card_types.core_types.push(CoreType::Sorcery);
            Arc::make_mut(&mut obj.abilities).push(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Draw {
                    count: QuantityExpr::Ref {
                        qty: QuantityRef::Variable {
                            name: "X".to_string(),
                        },
                    },
                    target: TargetFilter::Controller,
                },
            ));
            obj.mana_cost = ManaCost::Cost {
                shards: vec![ManaCostShard::X, ManaCostShard::Green],
                generic: 2,
            };
        }
        // Pool: 4 green. Without reduction: fixed = 1 (G) + 2 (generic) = 3, max X = 1.
        // With reduction of 1: fixed = 1 (G) + 1 (reduced generic) = 2, max X = 2.
        add_mana(&mut state, PlayerId(0), ManaType::Green, 4);
        state
            .pending_spell_cost_reductions
            .push(PendingSpellCostReduction {
                player: PlayerId(0),
                amount: 1,
                spell_filter: None,
            });

        apply_as_current(
            &mut state,
            GameAction::CastSpell {
                object_id: obj_id,
                card_id: CardId(960),
                targets: vec![],
            },
        )
        .unwrap();
        let max = match state.waiting_for {
            WaitingFor::ChooseXValue { max, .. } => max,
            ref other => panic!("expected ChooseXValue, got {other:?}"),
        };
        assert_eq!(
            max, 2,
            "cost reduction of 1 should raise affordable X from 1 to 2"
        );
    }

    /// Multi-X costs ({X}{X}): each point of X costs 2 mana, so `max_x_value`
    /// must divide remaining capacity by the X-count.
    #[test]
    fn x_cost_double_x_max_divides_by_count() {
        use super::super::engine::apply_as_current;
        use crate::types::ability::QuantityRef;

        let mut state = setup_game_at_main_phase();
        let obj_id = create_object(
            &mut state,
            CardId(904),
            PlayerId(0),
            "Synthetic {X}{X}".to_string(),
            Zone::Hand,
        );
        {
            let obj = state.objects.get_mut(&obj_id).unwrap();
            obj.card_types.core_types.push(CoreType::Sorcery);
            Arc::make_mut(&mut obj.abilities).push(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Draw {
                    count: QuantityExpr::Ref {
                        qty: QuantityRef::Variable {
                            name: "X".to_string(),
                        },
                    },
                    target: TargetFilter::Controller,
                },
            ));
            obj.mana_cost = ManaCost::Cost {
                shards: vec![ManaCostShard::X, ManaCostShard::X],
                generic: 0,
            };
        }
        add_mana(&mut state, PlayerId(0), ManaType::Colorless, 7);

        let result = apply_as_current(
            &mut state,
            GameAction::CastSpell {
                object_id: obj_id,
                card_id: CardId(904),
                targets: vec![],
            },
        )
        .unwrap();
        match result.waiting_for {
            WaitingFor::ChooseXValue { max, .. } => {
                assert_eq!(max, 3, "pool=7, x_count=2, so max X = 7 / 2 = 3");
            }
            other => panic!("expected ChooseXValue, got {other:?}"),
        }
    }

    /// Invalid X values (exceeding max) must be rejected.
    #[test]
    fn x_cost_rejects_value_above_max() {
        use super::super::engine::apply_as_current;
        use crate::types::ability::QuantityRef;

        let mut state = setup_game_at_main_phase();
        let obj_id = create_object(
            &mut state,
            CardId(902),
            PlayerId(0),
            "Synthetic X Sorcery".to_string(),
            Zone::Hand,
        );
        {
            let obj = state.objects.get_mut(&obj_id).unwrap();
            obj.card_types.core_types.push(CoreType::Sorcery);
            Arc::make_mut(&mut obj.abilities).push(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Draw {
                    count: QuantityExpr::Ref {
                        qty: QuantityRef::Variable {
                            name: "X".to_string(),
                        },
                    },
                    target: TargetFilter::Controller,
                },
            ));
            obj.mana_cost = ManaCost::Cost {
                shards: vec![ManaCostShard::X],
                generic: 0,
            };
        }
        add_mana(&mut state, PlayerId(0), ManaType::Colorless, 2);

        apply_as_current(
            &mut state,
            GameAction::CastSpell {
                object_id: obj_id,
                card_id: CardId(902),
                targets: vec![],
            },
        )
        .unwrap();

        // Pool of 2, no free producers → max X = 2. Requesting 5 must fail.
        let result = apply_as_current(&mut state, GameAction::ChooseX { value: 5 });
        assert!(result.is_err(), "ChooseX above max should error");
        // State remains in ChooseXValue.
        assert!(matches!(state.waiting_for, WaitingFor::ChooseXValue { .. }));
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
        state.stack.push_back(StackEntry {
            id: ObjectId(99),
            source_id: ObjectId(99),
            controller: PlayerId(1),
            kind: StackEntryKind::Spell {
                card_id: CardId(99),
                ability: None,
                casting_variant: CastingVariant::Normal,
                actual_mana_spent: 0,
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
    fn cast_with_keyword_flash_allows_creature_spell_outside_normal_timing() {
        let mut state = setup_game_at_main_phase();
        state.phase = Phase::End;
        state.active_player = PlayerId(1);
        state.priority_player = PlayerId(0);

        let source_id = create_object(
            &mut state,
            CardId(23),
            PlayerId(0),
            "Leyline".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&source_id)
            .unwrap()
            .static_definitions
            .push(
                parse_static_line(
                    "Creature cards you own that aren't on the battlefield have flash.",
                )
                .expect("static should parse"),
            );

        let obj_id = create_creature_spell_in_hand(&mut state, PlayerId(0));
        add_mana(&mut state, PlayerId(0), ManaType::Red, 1);
        add_mana(&mut state, PlayerId(0), ManaType::Colorless, 3);

        let mut events = Vec::new();
        let result = handle_cast_spell(&mut state, PlayerId(0), obj_id, CardId(22), &mut events)
            .expect("granted flash should allow the cast");

        assert!(matches!(result, WaitingFor::Priority { .. }));
        assert_eq!(state.stack.len(), 1);
    }

    #[test]
    fn cast_with_keyword_convoke_enters_mana_payment_for_matching_spell() {
        let mut state = setup_game_at_main_phase();
        let source_id = create_object(
            &mut state,
            CardId(24),
            PlayerId(0),
            "Convoke Banner".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&source_id)
            .unwrap()
            .static_definitions
            .push(
                parse_static_line("Creature spells you cast have convoke.")
                    .expect("static should parse"),
            );

        let helper = create_object(
            &mut state,
            CardId(25),
            PlayerId(0),
            "Elf".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&helper)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        let obj_id = create_creature_spell_in_hand(&mut state, PlayerId(0));
        let result =
            handle_cast_spell(&mut state, PlayerId(0), obj_id, CardId(22), &mut Vec::new())
                .expect("granted convoke should make the cast start");

        assert!(matches!(
            result,
            WaitingFor::ManaPayment {
                convoke_mode: Some(ConvokeMode::Convoke),
                ..
            }
        ));
        assert!(effective_spell_keyword_kinds(&state, PlayerId(0), obj_id)
            .contains(&KeywordKind::Convoke));
    }

    #[test]
    fn cast_with_keyword_convoke_honors_from_exile_filter() {
        let mut state = setup_game_at_main_phase();
        let source_id = create_object(
            &mut state,
            CardId(26),
            PlayerId(0),
            "Exile Banner".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&source_id)
            .unwrap()
            .static_definitions
            .push(
                parse_static_line("Spells you cast from exile have convoke.")
                    .expect("static should parse"),
            );

        let helper = create_object(
            &mut state,
            CardId(27),
            PlayerId(0),
            "Elf".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&helper)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        let obj_id = create_object(
            &mut state,
            CardId(28),
            PlayerId(0),
            "Exiled Divination".to_string(),
            Zone::Exile,
        );
        {
            let obj = state.objects.get_mut(&obj_id).unwrap();
            obj.card_types.core_types.push(CoreType::Sorcery);
            Arc::make_mut(&mut obj.abilities).push(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Draw {
                    count: QuantityExpr::Fixed { value: 2 },
                    target: TargetFilter::Controller,
                },
            ));
            obj.mana_cost = ManaCost::Cost {
                shards: vec![ManaCostShard::Blue],
                generic: 2,
            };
            obj.casting_permissions
                .push(crate::types::ability::CastingPermission::PlayFromExile {
                    duration: crate::types::ability::Duration::Permanent,
                    granted_to: PlayerId(0),
                });
        }

        let result =
            handle_cast_spell(&mut state, PlayerId(0), obj_id, CardId(28), &mut Vec::new())
                .expect("exiled spell should be castable with granted convoke");

        assert!(matches!(
            result,
            WaitingFor::ManaPayment {
                convoke_mode: Some(ConvokeMode::Convoke),
                ..
            }
        ));
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
        Arc::make_mut(&mut obj.abilities).push(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Draw {
                    count: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::Controller,
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
    fn return_to_hand_cost_moves_selected_permanent_before_activation() {
        use crate::game::engine::apply_as_current;

        let mut state = setup_game_at_main_phase();
        let source = create_object(
            &mut state,
            CardId(71),
            PlayerId(0),
            "Quirion Ranger".to_string(),
            Zone::Battlefield,
        );
        let forest = add_basic_land(&mut state, CardId(72), "Forest", "Forest");
        {
            let obj = state.objects.get_mut(&source).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            Arc::make_mut(&mut obj.abilities).push(
                AbilityDefinition::new(
                    AbilityKind::Activated,
                    Effect::Untap {
                        target: TargetFilter::Typed(TypedFilter::new(TypeFilter::Creature)),
                    },
                )
                .cost(AbilityCost::ReturnToHand {
                    count: 1,
                    filter: Some(TargetFilter::Typed(
                        TypedFilter::new(TypeFilter::Subtype("Forest".to_string()))
                            .controller(ControllerRef::You),
                    )),
                })
                .activation_restrictions(vec![ActivationRestriction::OnlyOnceEachTurn]),
            );
        }

        let waiting =
            handle_activate_ability(&mut state, PlayerId(0), source, 0, &mut Vec::new()).unwrap();
        assert!(matches!(waiting, WaitingFor::ReturnToHandForCost { .. }));
        state.waiting_for = waiting;

        apply_as_current(
            &mut state,
            GameAction::SelectCards {
                cards: vec![forest],
            },
        )
        .unwrap();

        assert_eq!(state.objects[&forest].zone, Zone::Hand);
        assert!(state.players[0].hand.contains(&forest));
        assert!(!state.battlefield.contains(&forest));
        // After cost payment the forest is gone, leaving only the Ranger itself as a
        // valid "target creature". auto_select_targets_for_ability picks the sole
        // legal target and pushes the ability to the stack without a TargetSelection
        // round-trip, which is why activated_abilities_this_turn is already incremented.
        assert_eq!(
            state
                .activated_abilities_this_turn
                .get(&(source, 0))
                .copied(),
            Some(1)
        );
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
        Arc::make_mut(&mut obj.abilities).push(AbilityDefinition::new(
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
            Arc::make_mut(&mut spell.abilities).push(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Draw {
                    count: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::Controller,
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
            Arc::make_mut(&mut spell.abilities).push(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Draw {
                    count: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::Controller,
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
            Arc::make_mut(&mut spell.abilities).push(AbilityDefinition::new(
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
    fn auto_tap_uses_brushland_and_loses_life_when_it_is_only_colored_source() {
        let mut state = setup_game_at_main_phase();
        let spell_id = create_single_color_spell_in_hand(
            &mut state,
            CardId(28),
            "Adventure Awaits",
            ManaCostShard::Green,
        );
        let brushland = add_brushland_like_land(&mut state, CardId(29), "Brushland", true);

        let mut events = Vec::new();
        let result =
            handle_cast_spell(&mut state, PlayerId(0), spell_id, CardId(28), &mut events).unwrap();

        assert!(matches!(result, WaitingFor::Priority { .. }));
        assert!(state.objects[&brushland].tapped);
        assert_eq!(state.players[0].life, 19);
        assert_eq!(state.stack.len(), 1, "spell should be on the stack");
    }

    #[test]
    fn auto_tap_prefers_safe_land_over_controller_harming_source() {
        let mut state = setup_game_at_main_phase();
        let spell_id = create_single_color_spell_in_hand(
            &mut state,
            CardId(30),
            "Lay of the Land",
            ManaCostShard::Green,
        );
        let brushland = add_brushland_like_land(&mut state, CardId(31), "Brushland", true);
        let safe_land = add_brushland_like_land(&mut state, CardId(32), "Safe Grove", false);

        let mut events = Vec::new();
        handle_cast_spell(&mut state, PlayerId(0), spell_id, CardId(30), &mut events).unwrap();

        assert!(
            !state.objects[&brushland].tapped,
            "auto-tap should avoid the harmful source when a safe equivalent exists"
        );
        assert!(state.objects[&safe_land].tapped);
        assert_eq!(state.players[0].life, 20);
    }

    #[test]
    fn cancel_cast_during_target_selection_returns_to_priority() {
        use crate::game::engine::apply_as_current;
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
        let result = apply_as_current(
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
        // CR 601.2a: The spell is announced onto the stack at the start of
        // the cast. The object's own zone stays at the origin until finalize
        // so mid-cast continuous effects (graveyard escape, cast-from-exile
        // filters, etc.) keep evaluating correctly.
        assert_eq!(state.stack.len(), 1);
        assert_eq!(state.stack[0].id, obj_id);
        assert!(!state.players[0].hand.is_empty());

        // CR 601.2i: Cancel -> the placeholder stack entry is popped and the
        // card remains in hand (no zone revert needed because no zone change
        // has been committed yet).
        let result = apply_as_current(&mut state, GameAction::CancelCast).unwrap();
        assert!(matches!(result.waiting_for, WaitingFor::Priority { .. }));
        assert!(state.stack.is_empty());
        assert!(!state.players[0].hand.is_empty());
        assert!(state.players[0].hand.contains(&obj_id));
    }

    /// CR 601.2a: After announcement, the spell is the topmost object on the
    /// stack and remains there through each interactive cast step. This test
    /// exercises the stack-at-announcement invariant during TargetSelection.
    #[test]
    fn spell_is_on_stack_during_target_selection() {
        use crate::game::engine::apply_as_current;
        use crate::types::actions::GameAction;

        let mut state = setup_game_at_main_phase();
        let obj_id = create_instant_in_hand(&mut state, PlayerId(0));
        add_mana(&mut state, PlayerId(0), ManaType::Red, 1);

        // Two ambiguous targets force interactive selection.
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

        let result = apply_as_current(
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

        // CR 601.2a: The StackEntry exists from announcement — no ghost-entry
        // synthesis is needed on the client side.
        assert_eq!(state.stack.len(), 1);
        let entry = &state.stack[0];
        assert_eq!(entry.id, obj_id);
        assert_eq!(entry.controller, PlayerId(0));
        // CR 601.2i: The placeholder entry has no ability attached yet —
        // `finalize_cast` fills it in after costs commit.
        match &entry.kind {
            StackEntryKind::Spell {
                card_id,
                ability,
                actual_mana_spent,
                ..
            } => {
                assert_eq!(*card_id, CardId(10));
                assert!(ability.is_none());
                assert_eq!(*actual_mana_spent, 0);
            }
            other => panic!("expected Spell stack entry, got {:?}", other),
        }
    }

    /// CR 601.2i: Cancelling from `ManaPayment` pops the placeholder stack
    /// entry pushed at announcement. Exercises the Convoke path which keeps
    /// the PendingCast on outer `GameState::pending_cast`.
    #[test]
    fn cancel_cast_from_mana_payment_pops_stack_entry() {
        use crate::game::engine::apply_as_current;
        use crate::types::actions::GameAction;

        let mut state = setup_game_at_main_phase();

        // A convoke-eligible creature on the battlefield gates the convoke
        // flow into ManaPayment (without one, convoke_mode is filtered away
        // and the flow goes straight to finalize_cast).
        let creature_id = create_object(
            &mut state,
            CardId(59),
            PlayerId(0),
            "Token".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&creature_id).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.has_summoning_sickness = false;
        }

        let obj_id = create_object(
            &mut state,
            CardId(60),
            PlayerId(0),
            "Convoke Spell".to_string(),
            Zone::Hand,
        );
        {
            let obj = state.objects.get_mut(&obj_id).unwrap();
            obj.card_types.core_types.push(CoreType::Sorcery);
            obj.keywords.push(Keyword::Convoke);
            obj.mana_cost = ManaCost::Cost {
                shards: vec![],
                generic: 2,
            };
            Arc::make_mut(&mut obj.abilities).push(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Draw {
                    count: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::Controller,
                },
            ));
        }
        add_mana(&mut state, PlayerId(0), ManaType::Colorless, 2);

        let result = apply_as_current(
            &mut state,
            GameAction::CastSpell {
                object_id: obj_id,
                card_id: CardId(60),
                targets: vec![],
            },
        )
        .unwrap();
        assert!(matches!(result.waiting_for, WaitingFor::ManaPayment { .. }));
        assert_eq!(state.stack.len(), 1);
        assert!(state.pending_cast.is_some());

        let result = apply_as_current(&mut state, GameAction::CancelCast).unwrap();
        assert!(matches!(result.waiting_for, WaitingFor::Priority { .. }));
        assert!(state.stack.is_empty());
        assert!(state.pending_cast.is_none());
        assert!(state.players[0].hand.contains(&obj_id));
    }

    // --- Aura casting tests ---
    // Note: `ControllerRef` + `TargetFilter` are already imported at the test module
    // head (where the CantBeActivated tests need them). No local re-import required.

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
        if let StackEntryKind::Spell {
            ability: Some(ability),
            ..
        } = &state.stack[0].kind
        {
            assert_eq!(
                ability.targets,
                vec![crate::types::ability::TargetRef::Object(creature)]
            );
        } else {
            panic!("Expected spell with ability on stack");
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
        if let StackEntryKind::Spell {
            ability: Some(ability),
            ..
        } = &state.stack[0].kind
        {
            assert_eq!(
                ability.targets,
                vec![crate::types::ability::TargetRef::Object(creature)]
            );
        } else {
            panic!("Expected spell with ability on stack");
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

        // CR 601.2a: Simulate announcement (normally performed by
        // `announce_spell_on_stack` in the continue_with_prepared path) so
        // `finalize_cast` finds the existing stack entry to update. Only push
        // the StackEntry — the object's `zone` stays at the origin (Hand)
        // until `finalize_cast` performs the Hand→Stack zone change itself.
        stack::push_to_stack(
            &mut state,
            StackEntry {
                id: object_id,
                source_id: object_id,
                controller: PlayerId(0),
                kind: StackEntryKind::Spell {
                    card_id: CardId(77),
                    ability: None,
                    casting_variant: CastingVariant::Normal,
                    actual_mana_spent: 0,
                },
            },
            &mut events,
        );

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
            Zone::Hand,
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
            Arc::make_mut(&mut obj.abilities).push(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::DealDamage {
                    amount: QuantityExpr::Fixed { value: 2 },
                    target: crate::types::ability::TargetFilter::Any,
                    damage_source: None,
                },
            ));
            // Mode 1: Draw a card
            Arc::make_mut(&mut obj.abilities).push(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Draw {
                    count: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::Controller,
                },
            ));
            // Mode 2: Gain 3 life
            Arc::make_mut(&mut obj.abilities).push(AbilityDefinition::new(
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
        state.battlefield.push_back(creature);

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
            StackEntryKind::Spell {
                ability: Some(ability),
                ..
            } => {
                // First mode is Draw
                assert!(matches!(
                    ability.effect,
                    Effect::Draw {
                        count: QuantityExpr::Fixed { value: 1 },
                        ..
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
            defense: None,
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
            trigger_definitions: Default::default(),
            replacement_definitions: Default::default(),
            static_definitions: Default::default(),
            color: vec![ManaColor::Red],
            printed_ref: None,
            modal: None,
            additional_cost: None,
            strive_cost: None,
            casting_restrictions: Vec::new(),
            casting_options: Vec::new(),
            layout_kind: None,
        });

        obj_id
    }

    /// Regression: adventure card is castable (via adventure face) even when the
    /// creature face cost is unaffordable. Previously can_cast_object_now gated on
    /// the creature face cost and would return false, suppressing AdventureCastChoice.
    #[test]
    fn adventure_cast_choice_when_only_adventure_affordable() {
        let mut state = setup_game_at_main_phase();
        let obj_id = create_adventure_in_hand(&mut state, PlayerId(0));
        // Creature costs {2}{R} (3 mana) — only give 2 mana (enough for adventure {1}{R} only)
        add_mana(&mut state, PlayerId(0), ManaType::Red, 2);

        // can_cast_object_now must return true since adventure face is affordable
        assert!(
            can_cast_object_now(&state, PlayerId(0), obj_id),
            "Adventure card should be castable when adventure face cost is affordable"
        );

        let mut events = Vec::new();
        let result =
            handle_cast_spell(&mut state, PlayerId(0), obj_id, CardId(70), &mut events).unwrap();

        assert!(
            matches!(result, WaitingFor::AdventureCastChoice { player, .. }
                if player == PlayerId(0)),
            "Expected AdventureCastChoice even when only adventure face is affordable, got {:?}",
            result
        );
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

        state.stack.push_back(StackEntry {
            id: obj_id,
            source_id: obj_id,
            controller: PlayerId(0),
            kind: StackEntryKind::Spell {
                card_id: CardId(70),
                ability: Some(ResolvedAbility::new(
                    Effect::DealDamage {
                        amount: QuantityExpr::Fixed { value: 2 },
                        target: crate::types::ability::TargetFilter::Any,
                        damage_source: None,
                    },
                    vec![TargetRef::Player(PlayerId(1))],
                    obj_id,
                    PlayerId(0),
                )),
                casting_variant: CastingVariant::Adventure,
                actual_mana_spent: 0,
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
        state.stack.push_back(StackEntry {
            id: obj_id,
            source_id: obj_id,
            controller: PlayerId(0),
            kind: StackEntryKind::Spell {
                card_id: CardId(70),
                ability: Some(ResolvedAbility::new(
                    Effect::DealDamage {
                        amount: QuantityExpr::Fixed { value: 2 },
                        target: crate::types::ability::TargetFilter::Any,
                        damage_source: None,
                    },
                    vec![TargetRef::Player(PlayerId(1))],
                    obj_id,
                    PlayerId(0),
                )),
                casting_variant: CastingVariant::Adventure,
                actual_mana_spent: 0,
            },
        });

        // Counter the spell (remove from stack, move to graveyard)
        state.stack.pop_back();
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
            count: 1,
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
            count: 1,
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
                    count: QuantityExpr::Fixed { value: 1 },
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

    use crate::types::statics::{CastingProhibitionCondition, ProhibitionScope};

    fn add_cant_cast_during_permanent(
        state: &mut GameState,
        controller: PlayerId,
        who: ProhibitionScope,
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

    fn add_cast_only_from_hand_restriction(
        state: &mut GameState,
        controller: PlayerId,
        affected_players: RestrictionPlayerScope,
    ) -> ObjectId {
        let source = create_object(
            state,
            CardId(state.next_object_id),
            controller,
            "Restriction Source".to_string(),
            Zone::Exile,
        );
        state.restrictions.push(GameRestriction::CastOnlyFromZones {
            source,
            affected_players,
            allowed_zones: vec![Zone::Hand],
            expiry: RestrictionExpiry::UntilPlayerNextTurn { player: controller },
        });
        source
    }

    #[test]
    fn cant_cast_during_runtime_opponent_blocked_on_controllers_turn() {
        let mut state = setup_game_at_main_phase();
        // Player 0 controls Teferi-like permanent: opponents can't cast during your turn
        add_cant_cast_during_permanent(
            &mut state,
            PlayerId(0),
            ProhibitionScope::Opponents,
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
            ProhibitionScope::Opponents,
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
            ProhibitionScope::AllPlayers,
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
            ProhibitionScope::AllPlayers,
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

    // --- PerTurnCastLimit enforcement tests ---

    fn add_per_turn_cast_limit_permanent(
        state: &mut GameState,
        controller: PlayerId,
        who: ProhibitionScope,
        max: u32,
        spell_filter: Option<TargetFilter>,
    ) -> ObjectId {
        let id = create_object(
            state,
            CardId(state.next_object_id),
            controller,
            "Limiter".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&id)
            .unwrap()
            .static_definitions
            .push(StaticDefinition::new(StaticMode::PerTurnCastLimit {
                who,
                max,
                spell_filter,
            }));
        id
    }

    fn make_spell_obj(state: &mut GameState, controller: PlayerId, is_creature: bool) -> ObjectId {
        use crate::types::card_type::CoreType;
        let id = create_object(
            state,
            CardId(state.next_object_id),
            controller,
            "Test Spell".to_string(),
            Zone::Hand,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        if is_creature {
            obj.card_types.core_types = vec![CoreType::Creature];
        } else {
            obj.card_types.core_types = vec![CoreType::Instant];
        }
        id
    }

    #[test]
    fn per_turn_limit_all_players_blocks_after_one_cast() {
        let mut state = setup_game_at_main_phase();
        add_per_turn_cast_limit_permanent(
            &mut state,
            PlayerId(0),
            ProhibitionScope::AllPlayers,
            1,
            None,
        );
        let spell_id = make_spell_obj(&mut state, PlayerId(0), false);

        // No spells cast yet — should NOT be blocked
        let obj = state.objects.get(&spell_id).unwrap();
        assert!(!is_blocked_by_per_turn_cast_limit(&state, PlayerId(0), obj));

        // Record one spell cast (clone to avoid borrow conflict)
        let obj_clone = state.objects.get(&spell_id).unwrap().clone();
        restrictions::record_spell_cast(&mut state, PlayerId(0), &obj_clone);

        // Now should be blocked
        let obj = state.objects.get(&spell_id).unwrap();
        assert!(is_blocked_by_per_turn_cast_limit(&state, PlayerId(0), obj));
    }

    #[test]
    fn per_turn_limit_controller_scope_blocks_only_controller() {
        let mut state = setup_game_at_main_phase();
        add_per_turn_cast_limit_permanent(
            &mut state,
            PlayerId(0),
            ProhibitionScope::Controller,
            1,
            None,
        );
        let spell_id = make_spell_obj(&mut state, PlayerId(0), false);

        let obj_clone = state.objects.get(&spell_id).unwrap().clone();
        restrictions::record_spell_cast(&mut state, PlayerId(0), &obj_clone);
        restrictions::record_spell_cast(&mut state, PlayerId(1), &obj_clone);

        let obj = state.objects.get(&spell_id).unwrap();
        // Controller (P0) should be blocked
        assert!(is_blocked_by_per_turn_cast_limit(&state, PlayerId(0), obj));
        // Opponent (P1) should NOT be blocked
        assert!(!is_blocked_by_per_turn_cast_limit(&state, PlayerId(1), obj));
    }

    #[test]
    fn per_turn_limit_opponents_scope_blocks_only_opponents() {
        let mut state = setup_game_at_main_phase();
        add_per_turn_cast_limit_permanent(
            &mut state,
            PlayerId(0),
            ProhibitionScope::Opponents,
            1,
            None,
        );
        let spell_id = make_spell_obj(&mut state, PlayerId(0), false);

        let obj_clone = state.objects.get(&spell_id).unwrap().clone();
        restrictions::record_spell_cast(&mut state, PlayerId(0), &obj_clone);
        restrictions::record_spell_cast(&mut state, PlayerId(1), &obj_clone);

        let obj = state.objects.get(&spell_id).unwrap();
        // Controller (P0) should NOT be blocked by their own "opponents" restriction
        assert!(!is_blocked_by_per_turn_cast_limit(&state, PlayerId(0), obj));
        // Opponent (P1) should be blocked
        assert!(is_blocked_by_per_turn_cast_limit(&state, PlayerId(1), obj));
    }

    #[test]
    fn per_turn_limit_noncreature_filter_allows_creature_spells() {
        let mut state = setup_game_at_main_phase();
        // Deafening Silence: noncreature filter
        add_per_turn_cast_limit_permanent(
            &mut state,
            PlayerId(0),
            ProhibitionScope::AllPlayers,
            1,
            Some(TargetFilter::Typed(TypedFilter {
                type_filters: vec![TypeFilter::Non(Box::new(TypeFilter::Creature))],
                ..TypedFilter::default()
            })),
        );

        // Cast a noncreature spell first
        let nc_id = make_spell_obj(&mut state, PlayerId(0), false);
        let nc_clone = state.objects.get(&nc_id).unwrap().clone();
        restrictions::record_spell_cast(&mut state, PlayerId(0), &nc_clone);

        // Trying to cast another noncreature → blocked
        let nc_obj = state.objects.get(&nc_id).unwrap();
        assert!(is_blocked_by_per_turn_cast_limit(
            &state,
            PlayerId(0),
            nc_obj
        ));

        // Trying to cast a creature → NOT blocked (creatures bypass the filter)
        let cr_id = make_spell_obj(&mut state, PlayerId(0), true);
        let cr_obj = state.objects.get(&cr_id).unwrap();
        assert!(!is_blocked_by_per_turn_cast_limit(
            &state,
            PlayerId(0),
            cr_obj
        ));
    }

    #[test]
    fn per_turn_limit_max_two_allows_second_cast() {
        let mut state = setup_game_at_main_phase();
        add_per_turn_cast_limit_permanent(
            &mut state,
            PlayerId(0),
            ProhibitionScope::Controller,
            2,
            None,
        );
        let spell_id = make_spell_obj(&mut state, PlayerId(0), false);

        // First cast OK
        let obj_clone = state.objects.get(&spell_id).unwrap().clone();
        restrictions::record_spell_cast(&mut state, PlayerId(0), &obj_clone);
        let obj = state.objects.get(&spell_id).unwrap();
        assert!(!is_blocked_by_per_turn_cast_limit(&state, PlayerId(0), obj));

        // Second cast OK
        restrictions::record_spell_cast(&mut state, PlayerId(0), &obj_clone);

        // Third cast → blocked
        let obj = state.objects.get(&spell_id).unwrap();
        assert!(is_blocked_by_per_turn_cast_limit(&state, PlayerId(0), obj));
    }

    #[test]
    fn per_turn_limit_multiple_sources_strictest_wins() {
        let mut state = setup_game_at_main_phase();
        // Permanent A: allows 2 spells per turn
        let a_id = add_per_turn_cast_limit_permanent(
            &mut state,
            PlayerId(0),
            ProhibitionScope::AllPlayers,
            2,
            None,
        );
        // Permanent B: allows only 1 spell per turn (stricter)
        let b_id = add_per_turn_cast_limit_permanent(
            &mut state,
            PlayerId(0),
            ProhibitionScope::AllPlayers,
            1,
            None,
        );
        let spell_id = make_spell_obj(&mut state, PlayerId(0), false);

        // Record one spell cast
        let obj_clone = state.objects.get(&spell_id).unwrap().clone();
        restrictions::record_spell_cast(&mut state, PlayerId(0), &obj_clone);

        // Blocked: B's limit of 1 applies
        let obj = state.objects.get(&spell_id).unwrap();
        assert!(is_blocked_by_per_turn_cast_limit(&state, PlayerId(0), obj));

        // Remove B (stricter source) from battlefield
        state.battlefield.retain(|id| *id != b_id);

        // Now only A's limit of 2 remains — 1 cast < 2, so NOT blocked
        let obj = state.objects.get(&spell_id).unwrap();
        assert!(!is_blocked_by_per_turn_cast_limit(&state, PlayerId(0), obj));

        // Suppress unused variable warnings
        let _ = a_id;
    }

    #[test]
    fn cant_cast_during_not_your_turn_blocks_on_opponent_turn() {
        let mut state = setup_game_at_main_phase();
        // Player 0 controls Fires-like permanent: controller can't cast outside their turn
        add_cant_cast_during_permanent(
            &mut state,
            PlayerId(0),
            ProhibitionScope::Controller,
            CastingProhibitionCondition::NotDuringYourTurn,
        );
        // Active player is 0 (controller's turn) — controller should NOT be blocked
        assert!(!is_blocked_by_cant_cast_during(&state, PlayerId(0)));

        // Switch to opponent's turn
        state.active_player = PlayerId(1);

        // Now controller IS blocked (not their turn)
        assert!(is_blocked_by_cant_cast_during(&state, PlayerId(0)));
        // Opponent is NOT blocked (Controller scope only affects P0)
        assert!(!is_blocked_by_cant_cast_during(&state, PlayerId(1)));
    }

    #[test]
    fn cast_only_from_zones_blocks_affected_opponent_from_exile() {
        let mut state = setup_game_at_main_phase();
        add_cast_only_from_hand_restriction(
            &mut state,
            PlayerId(0),
            RestrictionPlayerScope::OpponentsOfSourceController,
        );
        let exiled = create_object(
            &mut state,
            CardId(500),
            PlayerId(1),
            "Exiled Spell".to_string(),
            Zone::Exile,
        );
        state
            .objects
            .get_mut(&exiled)
            .unwrap()
            .casting_permissions
            .push(crate::types::ability::CastingPermission::ExileWithAltCost {
                cost: ManaCost::generic(2),
                cast_transformed: false,
                constraint: None,
            });

        assert!(is_blocked_by_cast_only_from_zones(
            &state,
            state.objects.get(&exiled).unwrap(),
            PlayerId(1)
        ));
        assert!(!spell_objects_available_to_cast(&state, PlayerId(1)).contains(&exiled));
    }

    #[test]
    fn cast_only_from_zones_allows_hand_casts_for_affected_player() {
        let mut state = setup_game_at_main_phase();
        add_cast_only_from_hand_restriction(
            &mut state,
            PlayerId(0),
            RestrictionPlayerScope::OpponentsOfSourceController,
        );
        let hand_spell = create_object(
            &mut state,
            CardId(501),
            PlayerId(1),
            "Hand Spell".to_string(),
            Zone::Hand,
        );

        assert!(!is_blocked_by_cast_only_from_zones(
            &state,
            state.objects.get(&hand_spell).unwrap(),
            PlayerId(1)
        ));
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
        Arc::make_mut(&mut obj.abilities).push(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Mana {
                    produced: ManaProduction::Fixed {
                        colors: vec![ManaColor::Green],
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
    fn counterspell_with_starting_town_mana_appears_in_legal_actions() {
        use crate::ai_support::legal_actions;
        use crate::game::derived::derive_display_state;
        use crate::types::actions::GameAction;

        let mut state = setup_game_at_main_phase();
        state.active_player = PlayerId(1);
        state.priority_player = PlayerId(0);
        state.waiting_for = WaitingFor::Priority {
            player: PlayerId(0),
        };

        create_starting_town(&mut state, PlayerId(0), CardId(121));
        add_basic_land(&mut state, CardId(120), "Island", "Island");

        let counterspell = create_object(
            &mut state,
            CardId(122),
            PlayerId(0),
            "Test Counterspell".to_string(),
            Zone::Hand,
        );
        {
            let obj = state.objects.get_mut(&counterspell).unwrap();
            obj.card_types.core_types.push(CoreType::Instant);
            Arc::make_mut(&mut obj.abilities).push(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Counter {
                    target: TargetFilter::Typed(crate::types::ability::TypedFilter::card()),
                    source_static: None,
                    unless_payment: None,
                },
            ));
            obj.mana_cost = ManaCost::Cost {
                shards: vec![ManaCostShard::Blue],
                generic: 1,
            };
        }

        let creature_spell = create_object(
            &mut state,
            CardId(123),
            PlayerId(1),
            "Opponent Creature".to_string(),
            Zone::Stack,
        );
        {
            let obj = state.objects.get_mut(&creature_spell).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
        }
        state.stack.push_back(StackEntry {
            id: creature_spell,
            source_id: creature_spell,
            controller: PlayerId(1),
            kind: StackEntryKind::Spell {
                card_id: CardId(123),
                ability: None,
                casting_variant: CastingVariant::Normal,
                actual_mana_spent: 0,
            },
        });

        derive_display_state(&mut state);

        assert!(
            can_cast_object_now(&state, PlayerId(0), counterspell),
            "Counterspell should be castable with Island plus Starting Town's life-payment mana"
        );

        let actions = legal_actions(&state);
        assert!(
            actions.iter().any(|action| matches!(
                action,
                GameAction::CastSpell { object_id, .. } if *object_id == counterspell
            )),
            "Counterspell should appear in legal_actions during the opponent's spell priority window"
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
            Arc::make_mut(&mut obj.abilities).push(
                AbilityDefinition::new(
                    AbilityKind::Activated,
                    Effect::Mana {
                        produced: ManaProduction::Fixed {
                            colors: vec![ManaColor::Green],
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
            Arc::make_mut(&mut obj.abilities).push(
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
                            contribution: crate::types::ability::ManaContribution::Base,
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

    #[test]
    fn first_qualified_spell_reducer_does_not_make_unrelated_artifact_castable() {
        let mut state = setup_game_at_main_phase();

        let reducer = create_object(
            &mut state,
            CardId(300),
            PlayerId(0),
            "Reducer".to_string(),
            Zone::Battlefield,
        );
        state.objects.get_mut(&reducer).unwrap().static_definitions.push(
            parse_static_line(
                "The first non-Lemur creature spell with flying you cast during each of your turns costs {1} less to cast.",
            )
            .unwrap(),
        );

        let drum = create_object(
            &mut state,
            CardId(301),
            PlayerId(0),
            "Springleaf Drum".to_string(),
            Zone::Hand,
        );
        {
            let obj = state.objects.get_mut(&drum).unwrap();
            obj.card_types.core_types.push(CoreType::Artifact);
            obj.mana_cost = ManaCost::generic(1);
            Arc::make_mut(&mut obj.abilities).push(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Unimplemented {
                    name: "Artifact".to_string(),
                    description: None,
                },
            ));
        }

        assert!(!can_cast_object_now(&state, PlayerId(0), drum));
    }

    #[test]
    fn first_qualified_spell_reducer_only_applies_to_first_matching_spell() {
        let mut state = setup_game_at_main_phase();

        let reducer = create_object(
            &mut state,
            CardId(302),
            PlayerId(0),
            "Reducer".to_string(),
            Zone::Battlefield,
        );
        state.objects.get_mut(&reducer).unwrap().static_definitions.push(
            parse_static_line(
                "The first non-Lemur creature spell with flying you cast during each of your turns costs {1} less to cast.",
            )
            .unwrap(),
        );

        let first_bird = create_object(
            &mut state,
            CardId(303),
            PlayerId(0),
            "Bird One".to_string(),
            Zone::Hand,
        );
        {
            let obj = state.objects.get_mut(&first_bird).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.card_types.subtypes.push("Bird".to_string());
            obj.keywords.push(Keyword::Flying);
            obj.mana_cost = ManaCost::generic(1);
            Arc::make_mut(&mut obj.abilities).push(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Unimplemented {
                    name: "Creature".to_string(),
                    description: None,
                },
            ));
        }

        assert!(can_cast_object_now(&state, PlayerId(0), first_bird));

        state.spells_cast_this_turn_by_player.insert(
            PlayerId(0),
            vec![crate::types::SpellCastRecord {
                core_types: vec![CoreType::Creature],
                supertypes: vec![],
                subtypes: vec!["Bird".to_string()],
                keywords: vec![Keyword::Flying],
                colors: vec![],
                mana_value: 1,
                has_x_in_cost: false,
            }],
        );

        let second_bird = create_object(
            &mut state,
            CardId(304),
            PlayerId(0),
            "Bird Two".to_string(),
            Zone::Hand,
        );
        {
            let obj = state.objects.get_mut(&second_bird).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.card_types.subtypes.push("Bird".to_string());
            obj.keywords.push(Keyword::Flying);
            obj.mana_cost = ManaCost::generic(1);
            Arc::make_mut(&mut obj.abilities).push(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Unimplemented {
                    name: "Creature".to_string(),
                    description: None,
                },
            ));
        }

        assert!(!can_cast_object_now(&state, PlayerId(0), second_bird));
    }

    #[test]
    fn spell_matches_cost_filter_fail_closed_for_unrecognized_variants() {
        let state = GameState::default();
        let spell_id = ObjectId(1);
        let source_id = ObjectId(2);

        // Recognized: Typed filter delegates to matches_target_filter
        assert!(!spell_matches_cost_filter(
            &state,
            PlayerId(0),
            spell_id,
            &TargetFilter::Typed(crate::types::ability::TypedFilter::creature()),
            source_id,
        ));

        // Unrecognized: None, Player, SelfRef all return false (fail-closed)
        assert!(!spell_matches_cost_filter(
            &state,
            PlayerId(0),
            spell_id,
            &TargetFilter::None,
            source_id,
        ));
        assert!(!spell_matches_cost_filter(
            &state,
            PlayerId(0),
            spell_id,
            &TargetFilter::Player,
            source_id,
        ));
        assert!(!spell_matches_cost_filter(
            &state,
            PlayerId(0),
            spell_id,
            &TargetFilter::SelfRef,
            source_id,
        ));
    }

    // -----------------------------------------------------------------------
    // Flashback (CR 702.34)
    // -----------------------------------------------------------------------

    /// Create an instant in the graveyard with Flashback.
    fn add_flashback_instant_to_graveyard(
        state: &mut GameState,
        player: PlayerId,
        flashback_cost: ManaCost,
        mana_cost: ManaCost,
    ) -> ObjectId {
        let card_id = CardId(state.next_object_id);
        let obj_id = create_object(
            state,
            card_id,
            player,
            "Think Twice".to_string(),
            Zone::Graveyard,
        );
        let obj = state.objects.get_mut(&obj_id).unwrap();
        obj.card_types.core_types.push(CoreType::Instant);
        obj.base_card_types = obj.card_types.clone();
        obj.mana_cost = mana_cost;
        obj.base_keywords
            .push(Keyword::Flashback(FlashbackCost::Mana(
                flashback_cost.clone(),
            )));
        obj.keywords = obj.base_keywords.clone();
        // Give it a simple draw effect so it has an ability to cast
        let ability = crate::types::ability::AbilityDefinition::new(
            crate::types::ability::AbilityKind::Spell,
            crate::types::ability::Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: crate::types::ability::TargetFilter::Controller,
            },
        );
        Arc::make_mut(&mut obj.abilities).push(ability.clone());
        Arc::make_mut(&mut obj.base_abilities).push(ability);
        obj_id
    }

    #[test]
    fn flashback_card_appears_castable_from_graveyard() {
        let mut state = setup_game_at_main_phase();
        let flashback_cost = ManaCost::Cost {
            generic: 2,
            shards: vec![ManaCostShard::Blue],
        };
        let card_cost = ManaCost::Cost {
            generic: 1,
            shards: vec![ManaCostShard::Blue],
        };
        let obj_id =
            add_flashback_instant_to_graveyard(&mut state, PlayerId(0), flashback_cost, card_cost);

        let available = spell_objects_available_to_cast(&state, PlayerId(0));
        assert!(
            available.contains(&obj_id),
            "Flashback card in graveyard should be castable"
        );
    }

    #[test]
    fn flashback_uses_flashback_cost_not_mana_cost() {
        let mut state = setup_game_at_main_phase();
        let flashback_cost = ManaCost::Cost {
            generic: 5,
            shards: vec![],
        };
        let card_cost = ManaCost::Cost {
            generic: 1,
            shards: vec![ManaCostShard::Blue],
        };
        let obj_id = add_flashback_instant_to_graveyard(
            &mut state,
            PlayerId(0),
            flashback_cost.clone(),
            card_cost,
        );

        let prepared = prepare_spell_cast(&state, PlayerId(0), obj_id).unwrap();
        assert_eq!(
            prepared.casting_variant,
            CastingVariant::Flashback,
            "Casting from graveyard with Flashback keyword should use CastingVariant::Flashback"
        );
        assert_eq!(
            prepared.mana_cost, flashback_cost,
            "Should use flashback cost, not card mana cost"
        );
    }

    #[test]
    fn flashback_card_in_hand_uses_normal_variant() {
        let mut state = setup_game_at_main_phase();
        let flashback_cost = ManaCost::Cost {
            generic: 5,
            shards: vec![],
        };
        let card_cost = ManaCost::Cost {
            generic: 1,
            shards: vec![ManaCostShard::Blue],
        };
        // Create in hand instead of graveyard
        let card_id = CardId(state.next_object_id);
        let obj_id = create_object(
            &mut state,
            card_id,
            PlayerId(0),
            "Think Twice".to_string(),
            Zone::Hand,
        );
        let obj = state.objects.get_mut(&obj_id).unwrap();
        obj.card_types.core_types.push(CoreType::Instant);
        obj.base_card_types = obj.card_types.clone();
        obj.mana_cost = card_cost.clone();
        obj.base_keywords
            .push(Keyword::Flashback(FlashbackCost::Mana(
                flashback_cost.clone(),
            )));
        obj.keywords = obj.base_keywords.clone();
        let ability = crate::types::ability::AbilityDefinition::new(
            crate::types::ability::AbilityKind::Spell,
            crate::types::ability::Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: crate::types::ability::TargetFilter::Controller,
            },
        );
        Arc::make_mut(&mut obj.abilities).push(ability.clone());
        Arc::make_mut(&mut obj.base_abilities).push(ability);

        let prepared = prepare_spell_cast(&state, PlayerId(0), obj_id).unwrap();
        assert_eq!(
            prepared.casting_variant,
            CastingVariant::Normal,
            "Flashback card in hand should use Normal variant"
        );
        assert_eq!(
            prepared.mana_cost, card_cost,
            "Should use card's mana cost when cast from hand"
        );
    }

    #[test]
    fn transient_flashback_grant_in_graveyard_is_castable_until_cleanup() {
        let mut state = setup_game_at_main_phase();
        let card_cost = ManaCost::Cost {
            generic: 1,
            shards: vec![ManaCostShard::Red],
        };
        let obj_id = add_flashback_instant_to_graveyard(
            &mut state,
            PlayerId(0),
            ManaCost::NoCost,
            card_cost.clone(),
        );
        let obj = state.objects.get_mut(&obj_id).unwrap();
        obj.base_keywords.clear();
        obj.keywords.clear();

        state.add_transient_continuous_effect(
            obj_id,
            PlayerId(0),
            crate::types::ability::Duration::UntilEndOfTurn,
            TargetFilter::SpecificObject { id: obj_id },
            vec![crate::types::ability::ContinuousModification::AddKeyword {
                keyword: Keyword::Flashback(FlashbackCost::Mana(ManaCost::SelfManaCost)),
            }],
            None,
        );

        let prepared = prepare_spell_cast(&state, PlayerId(0), obj_id).unwrap();
        assert_eq!(prepared.casting_variant, CastingVariant::Flashback);
        assert_eq!(prepared.mana_cost, card_cost);

        super::super::layers::prune_end_of_turn_effects(&mut state);
        assert!(
            !spell_objects_available_to_cast(&state, PlayerId(0)).contains(&obj_id),
            "Temporary flashback grant should expire at cleanup"
        );
    }

    #[test]
    fn static_graveyard_flashback_grant_makes_spell_castable() {
        let mut state = setup_game_at_main_phase();
        let card_cost = ManaCost::Cost {
            generic: 2,
            shards: vec![ManaCostShard::Blue],
        };
        let obj_id = add_flashback_instant_to_graveyard(
            &mut state,
            PlayerId(0),
            ManaCost::NoCost,
            card_cost.clone(),
        );
        let obj = state.objects.get_mut(&obj_id).unwrap();
        obj.base_keywords.clear();
        obj.keywords.clear();

        let source_id = create_object(
            &mut state,
            CardId(999),
            PlayerId(0),
            "Lier".to_string(),
            Zone::Battlefield,
        );
        let source = state.objects.get_mut(&source_id).unwrap();
        source.card_types.core_types.push(CoreType::Creature);
        source.base_card_types = source.card_types.clone();
        source.static_definitions.push(
            crate::types::ability::StaticDefinition::continuous()
                .affected(TargetFilter::Typed(
                    crate::types::ability::TypedFilter::new(TypeFilter::AnyOf(vec![
                        TypeFilter::Instant,
                        TypeFilter::Sorcery,
                    ]))
                    .controller(crate::types::ability::ControllerRef::You)
                    .properties(vec![
                        crate::types::ability::FilterProp::InZone {
                            zone: Zone::Graveyard,
                        },
                    ]),
                ))
                .modifications(vec![
                    crate::types::ability::ContinuousModification::AddKeyword {
                        keyword: Keyword::Flashback(FlashbackCost::Mana(ManaCost::SelfManaCost)),
                    },
                ]),
        );
        source.base_static_definitions =
            Arc::new(source.static_definitions.iter_all().cloned().collect());

        let available = spell_objects_available_to_cast(&state, PlayerId(0));
        assert!(available.contains(&obj_id));

        let prepared = prepare_spell_cast(&state, PlayerId(0), obj_id).unwrap();
        assert_eq!(prepared.casting_variant, CastingVariant::Flashback);
        assert_eq!(prepared.mana_cost, card_cost);
    }

    #[test]
    fn parsed_static_graveyard_escape_grant_makes_spell_castable() {
        let mut state = setup_game_at_main_phase();
        let obj_id = add_flashback_instant_to_graveyard(
            &mut state,
            PlayerId(0),
            ManaCost::NoCost,
            ManaCost::Cost {
                generic: 1,
                shards: vec![ManaCostShard::Red],
            },
        );
        let obj = state.objects.get_mut(&obj_id).unwrap();
        obj.base_keywords.clear();
        obj.keywords.clear();

        let source_id = create_object(
            &mut state,
            CardId(1001),
            PlayerId(0),
            "Underworld Breach".to_string(),
            Zone::Battlefield,
        );
        let parsed = crate::parser::oracle::parse_oracle_text(
            "Each nonland card in your graveyard has escape.\nThe escape cost is equal to the card's mana cost plus exile three other cards from your graveyard.",
            "Underworld Breach",
            &[],
            &[String::from("Enchantment")],
            &[],
        );
        let source = state.objects.get_mut(&source_id).unwrap();
        source.card_types.core_types.push(CoreType::Enchantment);
        source.base_card_types = source.card_types.clone();
        source.static_definitions = parsed.statics.clone().into();
        source.base_static_definitions = Arc::new(parsed.statics);

        for idx in 0..3 {
            let filler_id = create_object(
                &mut state,
                CardId(1100 + idx),
                PlayerId(0),
                format!("Filler {idx}"),
                Zone::Graveyard,
            );
            let filler = state.objects.get_mut(&filler_id).unwrap();
            filler.card_types.core_types.push(CoreType::Sorcery);
            filler.base_card_types = filler.card_types.clone();
        }

        let available = spell_objects_available_to_cast(&state, PlayerId(0));
        assert!(available.contains(&obj_id));

        let prepared = prepare_spell_cast(&state, PlayerId(0), obj_id).unwrap();
        assert_eq!(prepared.casting_variant, CastingVariant::Escape);
        assert_eq!(
            prepared.mana_cost,
            ManaCost::Cost {
                generic: 1,
                shards: vec![ManaCostShard::Red],
            }
        );
    }

    #[test]
    fn granted_escape_requires_exile_cost_payment() {
        let mut state = setup_game_at_main_phase();
        let obj_id = add_flashback_instant_to_graveyard(
            &mut state,
            PlayerId(0),
            ManaCost::NoCost,
            ManaCost::Cost {
                generic: 1,
                shards: vec![ManaCostShard::Red],
            },
        );
        let obj = state.objects.get_mut(&obj_id).unwrap();
        obj.base_keywords.clear();
        obj.keywords.clear();

        let source_id = create_object(
            &mut state,
            CardId(1002),
            PlayerId(0),
            "Underworld Breach".to_string(),
            Zone::Battlefield,
        );
        let parsed = crate::parser::oracle::parse_oracle_text(
            "Each nonland card in your graveyard has escape.\nThe escape cost is equal to the card's mana cost plus exile three other cards from your graveyard.",
            "Underworld Breach",
            &[],
            &[String::from("Enchantment")],
            &[],
        );
        let source = state.objects.get_mut(&source_id).unwrap();
        source.card_types.core_types.push(CoreType::Enchantment);
        source.base_card_types = source.card_types.clone();
        source.static_definitions = parsed.statics.clone().into();
        source.base_static_definitions = Arc::new(parsed.statics);

        for idx in 0..3 {
            let filler_id = create_object(
                &mut state,
                CardId(1200 + idx),
                PlayerId(0),
                format!("Filler {idx}"),
                Zone::Graveyard,
            );
            let filler = state.objects.get_mut(&filler_id).unwrap();
            filler.card_types.core_types.push(CoreType::Sorcery);
            filler.base_card_types = filler.card_types.clone();
        }

        let card_id = state.objects.get(&obj_id).unwrap().card_id;

        let waiting = handle_cast_spell(&mut state, PlayerId(0), obj_id, card_id, &mut Vec::new())
            .expect("granted escape should start cost payment");
        assert!(matches!(
            waiting,
            WaitingFor::ExileFromGraveyardForCost { count: 3, .. }
        ));
    }

    #[test]
    fn granted_non_mana_flashback_pays_additional_cost() {
        let mut state = setup_game_at_main_phase();
        let obj_id = add_flashback_instant_to_graveyard(
            &mut state,
            PlayerId(0),
            ManaCost::NoCost,
            ManaCost::Cost {
                generic: 2,
                shards: vec![ManaCostShard::Green],
            },
        );
        let obj = state.objects.get_mut(&obj_id).unwrap();
        obj.base_keywords.clear();
        obj.keywords.clear();

        let source_id = create_object(
            &mut state,
            CardId(1003),
            PlayerId(0),
            "Grantor".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&source_id)
            .unwrap()
            .static_definitions
            .push(
                StaticDefinition::continuous()
                    .affected(TargetFilter::SpecificObject { id: obj_id })
                    .modifications(vec![ContinuousModification::AddKeyword {
                        keyword: Keyword::Flashback(FlashbackCost::NonMana(AbilityCost::PayLife {
                            amount: QuantityExpr::Fixed { value: 2 },
                        })),
                    }]),
            );

        let card_id = state.objects.get(&obj_id).unwrap().card_id;

        let waiting = handle_cast_spell(&mut state, PlayerId(0), obj_id, card_id, &mut Vec::new())
            .expect("granted non-mana flashback should be castable");

        assert!(matches!(waiting, WaitingFor::Priority { .. }));
        assert_eq!(state.players[0].life, 18);
        assert_eq!(state.stack.len(), 1);
    }

    /// CR 702.34a + CR 119.8: Flashback with a non-mana PayLife cost is not
    /// castable when the caster has CantLoseLife — the cost "can't be paid"
    /// per CR 119.8, so `can_cast_object_now` must reject it and the spell
    /// must not appear in the castable-objects list.
    #[test]
    fn non_mana_flashback_filtered_under_cant_lose_life() {
        use crate::types::statics::StaticMode;

        let mut state = setup_game_at_main_phase();
        let obj_id = add_flashback_instant_to_graveyard(
            &mut state,
            PlayerId(0),
            ManaCost::NoCost,
            ManaCost::Cost {
                generic: 2,
                shards: vec![ManaCostShard::Green],
            },
        );
        // Replace the flashback mana cost with a pay-life cost directly on the card.
        let obj = state.objects.get_mut(&obj_id).unwrap();
        obj.base_keywords.clear();
        obj.keywords.clear();
        obj.base_keywords
            .push(Keyword::Flashback(FlashbackCost::NonMana(
                AbilityCost::PayLife {
                    amount: QuantityExpr::Fixed { value: 2 },
                },
            )));
        obj.keywords = obj.base_keywords.clone();

        // Install CantLoseLife on PlayerId(0).
        let lock_id = create_object(
            &mut state,
            CardId(0x5117),
            PlayerId(0),
            "Life Lock".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&lock_id)
            .unwrap()
            .static_definitions
            .push(
                StaticDefinition::new(StaticMode::CantLoseLife).affected(TargetFilter::Typed(
                    TypedFilter::default().controller(ControllerRef::You),
                )),
            );

        assert!(
            !can_cast_object_now(&state, PlayerId(0), obj_id),
            "Flashback with PayLife cost must be unreachable under CantLoseLife"
        );
    }

    #[test]
    fn self_graveyard_static_flashback_grant_is_castable() {
        let mut state = setup_game_at_main_phase();
        let obj_id = add_flashback_instant_to_graveyard(
            &mut state,
            PlayerId(0),
            ManaCost::NoCost,
            ManaCost::Cost {
                generic: 3,
                shards: vec![ManaCostShard::Green],
            },
        );
        let obj = state.objects.get_mut(&obj_id).unwrap();
        obj.base_keywords.clear();
        obj.keywords.clear();
        obj.static_definitions.push(
            crate::types::ability::StaticDefinition::continuous()
                .affected(TargetFilter::SelfRef)
                .condition(crate::types::ability::StaticCondition::And {
                    conditions: vec![
                        crate::types::ability::StaticCondition::OpponentPoisonAtLeast { count: 3 },
                        crate::types::ability::StaticCondition::SourceInZone {
                            zone: Zone::Graveyard,
                        },
                    ],
                })
                .modifications(vec![
                    crate::types::ability::ContinuousModification::AddKeyword {
                        keyword: Keyword::Flashback(FlashbackCost::Mana(ManaCost::Cost {
                            generic: 2,
                            shards: vec![ManaCostShard::Green],
                        })),
                    },
                ]),
        );
        obj.base_static_definitions =
            Arc::new(obj.static_definitions.iter_all().cloned().collect());
        state.players[1].poison_counters = 3;

        let available = spell_objects_available_to_cast(&state, PlayerId(0));
        assert!(available.contains(&obj_id));

        let prepared = prepare_spell_cast(&state, PlayerId(0), obj_id).unwrap();
        assert_eq!(prepared.casting_variant, CastingVariant::Flashback);
        assert_eq!(
            prepared.mana_cost,
            ManaCost::Cost {
                generic: 2,
                shards: vec![ManaCostShard::Green],
            }
        );
    }

    /// CR 702.34a + CR 118.8 + CR 118.3b: Compound flashback cost
    /// ("Flashback—{1}{U}, Pay 3 life") — Deep Analysis class. The mana
    /// sub-cost is paid through the normal mana flow; the residual life
    /// sub-cost is paid as an additional cost via `pay_additional_cost`.
    /// Both sides must succeed for the spell to be cast.
    #[test]
    fn compound_flashback_pays_mana_and_life() {
        let mut state = setup_game_at_main_phase();
        let obj_id = add_flashback_instant_to_graveyard(
            &mut state,
            PlayerId(0),
            ManaCost::NoCost, // unused — overwritten below
            ManaCost::Cost {
                generic: 3,
                shards: vec![ManaCostShard::Blue],
            },
        );
        // Replace the keyword with a compound flashback cost: {1}{U} + Pay 3 life.
        let obj = state.objects.get_mut(&obj_id).unwrap();
        obj.base_keywords.clear();
        obj.keywords.clear();
        let compound = AbilityCost::Composite {
            costs: vec![
                AbilityCost::Mana {
                    cost: ManaCost::Cost {
                        generic: 1,
                        shards: vec![ManaCostShard::Blue],
                    },
                },
                AbilityCost::PayLife {
                    amount: QuantityExpr::Fixed { value: 3 },
                },
            ],
        };
        obj.base_keywords
            .push(Keyword::Flashback(FlashbackCost::NonMana(compound)));
        obj.keywords = obj.base_keywords.clone();
        let card_id = obj.card_id;

        // The prepared spell pays only the mana sub-cost through the normal flow.
        let prepared = prepare_spell_cast(&state, PlayerId(0), obj_id).unwrap();
        assert_eq!(prepared.casting_variant, CastingVariant::Flashback);
        assert_eq!(
            prepared.mana_cost,
            ManaCost::Cost {
                generic: 1,
                shards: vec![ManaCostShard::Blue],
            },
            "compound flashback's mana sub-cost should be the spell's mana cost"
        );

        // Provide {1}{U} of mana.
        add_mana(&mut state, PlayerId(0), ManaType::Blue, 1);
        add_mana(&mut state, PlayerId(0), ManaType::Colorless, 1);

        let life_before = state.players[0].life;
        handle_cast_spell(&mut state, PlayerId(0), obj_id, card_id, &mut Vec::new())
            .expect("compound flashback with payable mana + life should be castable");

        assert_eq!(
            state.players[0].life,
            life_before - 3,
            "Pay 3 life sub-cost must be paid as additional cost"
        );
        assert_eq!(state.stack.len(), 1, "spell should be on the stack");
        assert_eq!(
            state.players[0].mana_pool.total(),
            0,
            "{{1}}{{U}} mana sub-cost must be drained from the pool"
        );
    }

    /// CR 702.34a + CR 119.8: Compound flashback cost is not castable when the
    /// caster lacks life to pay the additional cost — even with sufficient mana.
    #[test]
    fn compound_flashback_filtered_when_life_insufficient() {
        let mut state = setup_game_at_main_phase();
        let obj_id = add_flashback_instant_to_graveyard(
            &mut state,
            PlayerId(0),
            ManaCost::NoCost,
            ManaCost::Cost {
                generic: 3,
                shards: vec![ManaCostShard::Blue],
            },
        );
        let obj = state.objects.get_mut(&obj_id).unwrap();
        obj.base_keywords.clear();
        obj.keywords.clear();
        obj.base_keywords
            .push(Keyword::Flashback(FlashbackCost::NonMana(
                AbilityCost::Composite {
                    costs: vec![
                        AbilityCost::Mana {
                            cost: ManaCost::Cost {
                                generic: 1,
                                shards: vec![ManaCostShard::Blue],
                            },
                        },
                        AbilityCost::PayLife {
                            amount: QuantityExpr::Fixed { value: 3 },
                        },
                    ],
                },
            )));
        obj.keywords = obj.base_keywords.clone();

        // CR 118.3: a player can't pay a cost without sufficient resources;
        // CR 119.4: paying life requires life total >= amount.
        // Drop life to 2 so paying 3 is unpayable.
        state.players[0].life = 2;

        // Provide {1}{U} of mana so mana side is fine.
        add_mana(&mut state, PlayerId(0), ManaType::Blue, 1);
        add_mana(&mut state, PlayerId(0), ManaType::Colorless, 1);

        assert!(
            !can_cast_object_now(&state, PlayerId(0), obj_id),
            "compound flashback must be filtered when life is insufficient to pay the residual cost"
        );
    }

    #[test]
    fn cast_with_keyword_convoke_uses_caster_not_stored_controller() {
        let mut state = setup_game_at_main_phase();
        let source_id = create_object(
            &mut state,
            CardId(1004),
            PlayerId(0),
            "Exile Banner".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&source_id)
            .unwrap()
            .static_definitions
            .push(
                parse_static_line("Spells you cast from exile have convoke.")
                    .expect("static should parse"),
            );

        let helper = create_object(
            &mut state,
            CardId(1005),
            PlayerId(0),
            "Elf".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&helper)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        let obj_id = create_object(
            &mut state,
            CardId(1006),
            PlayerId(1),
            "Borrowed Spell".to_string(),
            Zone::Exile,
        );
        {
            let obj = state.objects.get_mut(&obj_id).unwrap();
            obj.card_types.core_types.push(CoreType::Sorcery);
            Arc::make_mut(&mut obj.abilities).push(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Draw {
                    count: QuantityExpr::Fixed { value: 2 },
                    target: TargetFilter::Controller,
                },
            ));
            obj.mana_cost = ManaCost::Cost {
                shards: vec![ManaCostShard::Blue],
                generic: 2,
            };
            obj.casting_permissions.push(
                crate::types::ability::CastingPermission::ExileWithAltCost {
                    cost: obj.mana_cost.clone(),
                    cast_transformed: false,
                    constraint: None,
                },
            );
        }

        let waiting = handle_cast_spell(
            &mut state,
            PlayerId(0),
            obj_id,
            CardId(1006),
            &mut Vec::new(),
        )
        .expect("the acting player should receive granted convoke");
        assert!(matches!(
            waiting,
            WaitingFor::ManaPayment {
                convoke_mode: Some(ConvokeMode::Convoke),
                ..
            }
        ));
    }

    #[test]
    fn gran_gran_reduces_noncreature_spell_with_three_lessons_in_graveyard() {
        let mut state = setup_game_at_main_phase();

        let gran_gran = create_object(
            &mut state,
            CardId(600),
            PlayerId(0),
            "Gran-Gran".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&gran_gran).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.card_types.subtypes.extend([
                "Human".to_string(),
                "Peasant".to_string(),
                "Ally".to_string(),
            ]);
            obj.static_definitions.push(
                parse_static_line(
                    "Noncreature spells you cast cost {1} less to cast as long as there are three or more Lesson cards in your graveyard.",
                )
                .expect("Gran-Gran reducer should parse"),
            );
        }

        for i in 0..3u64 {
            let lesson = create_object(
                &mut state,
                CardId(610 + i),
                PlayerId(0),
                format!("Lesson {i}"),
                Zone::Graveyard,
            );
            let obj = state.objects.get_mut(&lesson).unwrap();
            obj.card_types.core_types.push(CoreType::Sorcery);
            obj.card_types.subtypes.push("Lesson".to_string());
        }

        let spell = create_object(
            &mut state,
            CardId(620),
            PlayerId(0),
            "Test Noncreature Spell".to_string(),
            Zone::Hand,
        );
        {
            let obj = state.objects.get_mut(&spell).unwrap();
            obj.card_types.core_types.push(CoreType::Instant);
            obj.mana_cost = ManaCost::Cost {
                shards: vec![ManaCostShard::Blue],
                generic: 1,
            };
            Arc::make_mut(&mut obj.abilities).push(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Draw {
                    count: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::Controller,
                },
            ));
        }

        let effective = effective_spell_cost(&state, PlayerId(0), spell)
            .expect("effective cost should resolve");
        assert_eq!(
            effective,
            ManaCost::Cost {
                shards: vec![ManaCostShard::Blue],
                generic: 0,
            }
        );
    }

    /// CR 601.2f: Thalia, Guardian of Thraben raises noncreature spell costs by {1}.
    /// When the AI has insufficient mana to pay the taxed cost, `can_cast_object_now`
    /// must return false so the spell never appears in the candidate action list.
    #[test]
    fn raise_cost_static_prevents_unaffordable_noncreature_cast() {
        use crate::ai_support::{candidate_actions, legal_actions};
        use crate::game::derived::derive_display_state;
        use crate::types::actions::GameAction;

        let mut state = setup_game_at_main_phase();

        // Thalia on the opponent's battlefield: "Noncreature spells cost {1} more to cast."
        let thalia = create_object(
            &mut state,
            CardId(700),
            PlayerId(1),
            "Thalia, Guardian of Thraben".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&thalia).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.static_definitions.push(
                parse_static_line("Noncreature spells cost {1} more to cast.")
                    .expect("Thalia RaiseCost should parse"),
            );
            obj.base_static_definitions =
                Arc::new(obj.static_definitions.iter_all().cloned().collect());
        }

        // One Mountain for player 0 — enough for {R} but not {1}{R}
        add_basic_land(&mut state, CardId(701), "Mountain", "Mountain");

        // Lightning Bolt in hand: costs {R}, but Thalia makes it {1}{R}
        let bolt = create_object(
            &mut state,
            CardId(702),
            PlayerId(0),
            "Lightning Bolt".to_string(),
            Zone::Hand,
        );
        {
            let obj = state.objects.get_mut(&bolt).unwrap();
            obj.card_types.core_types.push(CoreType::Instant);
            obj.mana_cost = ManaCost::Cost {
                shards: vec![ManaCostShard::Red],
                generic: 0,
            };
            Arc::make_mut(&mut obj.abilities).push(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::DealDamage {
                    amount: QuantityExpr::Fixed { value: 3 },
                    target: TargetFilter::Any,
                    damage_source: None,
                },
            ));
        }

        derive_display_state(&mut state);

        // With Thalia's tax, Lightning Bolt costs {1}{R} but player has only 1 Mountain ({R}).
        assert!(
            !can_cast_object_now(&state, PlayerId(0), bolt),
            "Lightning Bolt should NOT be castable — Thalia tax makes it {{1}}{{R}} with only 1 Mountain"
        );

        // Must not appear in candidate or legal actions
        let candidates = candidate_actions(&state);
        assert!(
            !candidates.iter().any(|c| matches!(
                &c.action,
                GameAction::CastSpell { object_id, .. } if *object_id == bolt
            )),
            "Unaffordable spell must not appear in candidate_actions"
        );

        let actions = legal_actions(&state);
        assert!(
            !actions.iter().any(|a| matches!(
                a,
                GameAction::CastSpell { object_id, .. } if *object_id == bolt
            )),
            "Unaffordable spell must not appear in legal_actions"
        );
    }

    /// CR 601.2f: With enough mana to cover Thalia's tax, the spell remains castable.
    #[test]
    fn raise_cost_static_allows_affordable_noncreature_cast() {
        use crate::game::derived::derive_display_state;

        let mut state = setup_game_at_main_phase();

        // Thalia on opponent's battlefield
        let thalia = create_object(
            &mut state,
            CardId(710),
            PlayerId(1),
            "Thalia, Guardian of Thraben".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&thalia).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.static_definitions.push(
                parse_static_line("Noncreature spells cost {1} more to cast.")
                    .expect("Thalia RaiseCost should parse"),
            );
            obj.base_static_definitions =
                Arc::new(obj.static_definitions.iter_all().cloned().collect());
        }

        // Two Mountains — enough for {1}{R}
        add_basic_land(&mut state, CardId(711), "Mountain", "Mountain");
        add_basic_land(&mut state, CardId(712), "Mountain 2", "Mountain");

        // Lightning Bolt: {R} → {1}{R} with Thalia
        let bolt = create_object(
            &mut state,
            CardId(713),
            PlayerId(0),
            "Lightning Bolt".to_string(),
            Zone::Hand,
        );
        {
            let obj = state.objects.get_mut(&bolt).unwrap();
            obj.card_types.core_types.push(CoreType::Instant);
            obj.mana_cost = ManaCost::Cost {
                shards: vec![ManaCostShard::Red],
                generic: 0,
            };
            Arc::make_mut(&mut obj.abilities).push(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::DealDamage {
                    amount: QuantityExpr::Fixed { value: 3 },
                    target: TargetFilter::Any,
                    damage_source: None,
                },
            ));
        }

        derive_display_state(&mut state);

        // With 2 Mountains, {1}{R} is affordable
        assert!(
            can_cast_object_now(&state, PlayerId(0), bolt),
            "Lightning Bolt should be castable with 2 Mountains (covers {{1}}{{R}} after Thalia tax)"
        );
    }

    // === CR 602.5 + CR 603.2a: CantBeActivated runtime enforcement tests ===
    //
    // These exercise the building-block (`is_blocked_by_cant_be_activated` +
    // `can_activate_ability_now`) directly rather than end-to-end game flow.

    /// Attach a `CantBeActivated` static to a freshly-created permanent on the battlefield.
    fn add_cant_be_activated_source(
        state: &mut GameState,
        controller: PlayerId,
        who: ProhibitionScope,
        source_filter: TargetFilter,
    ) -> ObjectId {
        let id = create_object(
            state,
            CardId(0xCAC7),
            controller,
            "Activation Prohibitor".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.entered_battlefield_turn = Some(0);
        obj.static_definitions
            .push(StaticDefinition::new(StaticMode::CantBeActivated {
                who,
                source_filter,
                // CR 605.1a: Existing test helpers cover the Karn/Clarion family which
                // has no exemption suffix.
                exemption: ActivationExemption::None,
            }));
        id
    }

    /// Attach an artifact creature with a Tap-only activated ability to the battlefield.
    fn add_artifact_with_activated_ability(
        state: &mut GameState,
        controller: PlayerId,
    ) -> ObjectId {
        let id = create_object(
            state,
            CardId(0xA8CF),
            controller,
            "Artifact Dude".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Artifact);
        obj.card_types.core_types.push(CoreType::Creature);
        obj.entered_battlefield_turn = Some(0);
        Arc::make_mut(&mut obj.abilities).push(
            crate::types::ability::AbilityDefinition::new(
                crate::types::ability::AbilityKind::Activated,
                crate::types::ability::Effect::Draw {
                    count: QuantityExpr::Fixed { value: 1 },
                    target: crate::types::ability::TargetFilter::Controller,
                },
            )
            .cost(crate::types::ability::AbilityCost::Tap),
        );
        id
    }

    #[test]
    fn karn_blocks_opponent_artifact_activation() {
        // CR 602.5: Karn the Great Creator — activated abilities of artifacts your
        // opponents control can't be activated. `who = AllPlayers, source_filter =
        // Artifact + ControllerRef::Opponent`.
        let mut state = setup_game_at_main_phase();
        // Karn on P0's battlefield.
        add_cant_be_activated_source(
            &mut state,
            PlayerId(0),
            ProhibitionScope::AllPlayers,
            TargetFilter::Typed(
                TypedFilter::new(TypeFilter::Artifact).controller(ControllerRef::Opponent),
            ),
        );
        // Artifact under P1's control (P0's opponent from Karn's perspective).
        let p1_artifact = add_artifact_with_activated_ability(&mut state, PlayerId(1));
        let p1_ability = state.objects[&p1_artifact].abilities[0].clone();

        assert!(
            is_blocked_by_cant_be_activated(&state, PlayerId(1), p1_artifact, &p1_ability),
            "Karn must block P1 from activating their own artifact's ability"
        );
        assert!(
            !can_activate_ability_now(&state, PlayerId(1), p1_artifact, 0),
            "can_activate_ability_now must reject activation under Karn"
        );
    }

    #[test]
    fn karn_permits_own_artifact_activation() {
        // CR 602.5: Karn's filter has `ControllerRef::Opponent` — an artifact under
        // Karn's own controller is NOT blocked.
        let mut state = setup_game_at_main_phase();
        add_cant_be_activated_source(
            &mut state,
            PlayerId(0),
            ProhibitionScope::AllPlayers,
            TargetFilter::Typed(
                TypedFilter::new(TypeFilter::Artifact).controller(ControllerRef::Opponent),
            ),
        );
        // Artifact under P0's (Karn controller's) control.
        let p0_artifact = add_artifact_with_activated_ability(&mut state, PlayerId(0));
        let p0_ability = state.objects[&p0_artifact].abilities[0].clone();

        assert!(
            !is_blocked_by_cant_be_activated(&state, PlayerId(0), p0_artifact, &p0_ability),
            "Karn must NOT block its own controller's artifact activations"
        );
        assert!(
            can_activate_ability_now(&state, PlayerId(0), p0_artifact, 0),
            "can_activate_ability_now must accept activation for Karn's own artifacts"
        );
    }

    #[test]
    fn clarion_blocks_activation_of_multi_type_filter_set() {
        // CR 602.5 + CR 603.2a: Clarion Conqueror — activated abilities of artifacts,
        // creatures, and planeswalkers your opponents control can't be activated.
        //
        // Use the parser-emitted Or-disjunction form to exercise the real runtime path.
        let mut state = setup_game_at_main_phase();
        let clarion_static = parse_static_line(
            "Activated abilities of artifacts, creatures, and planeswalkers your opponents control can't be activated.",
        )
        .expect("Clarion parses");
        // Attach it to a permanent on P0's battlefield.
        let prohibitor = create_object(
            &mut state,
            CardId(0xC1A0),
            PlayerId(0),
            "Clarion".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&prohibitor)
            .unwrap()
            .static_definitions
            .push(clarion_static);

        // Artifact-creature (matches Clarion's filter) under P1.
        let p1_creature = add_artifact_with_activated_ability(&mut state, PlayerId(1));
        let p1_ability = state.objects[&p1_creature].abilities[0].clone();
        assert!(
            is_blocked_by_cant_be_activated(&state, PlayerId(1), p1_creature, &p1_ability),
            "Clarion must block an opponent's artifact/creature activation"
        );
    }

    #[test]
    fn cant_be_activated_selfref_blocks_only_this_permanent() {
        // CR 602.5: Chalice-of-Life-class — `source_filter = SelfRef`. Only the
        // source permanent's OWN activated abilities are blocked; other permanents
        // are unaffected.
        let mut state = setup_game_at_main_phase();
        // Prohibitor whose self-ref filter blocks only itself.
        let prohibitor = create_object(
            &mut state,
            CardId(0xCACA),
            PlayerId(0),
            "SelfRef Prohibitor".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&prohibitor).unwrap();
            obj.card_types.core_types.push(CoreType::Artifact);
            obj.entered_battlefield_turn = Some(0);
            obj.static_definitions
                .push(StaticDefinition::new(StaticMode::CantBeActivated {
                    who: ProhibitionScope::AllPlayers,
                    source_filter: TargetFilter::SelfRef,
                    exemption: ActivationExemption::None,
                }));
            Arc::make_mut(&mut obj.abilities).push(
                crate::types::ability::AbilityDefinition::new(
                    crate::types::ability::AbilityKind::Activated,
                    crate::types::ability::Effect::Draw {
                        count: QuantityExpr::Fixed { value: 1 },
                        target: crate::types::ability::TargetFilter::Controller,
                    },
                )
                .cost(crate::types::ability::AbilityCost::Tap),
            );
        }

        let prohibitor_ability = state.objects[&prohibitor].abilities[0].clone();
        // The prohibitor's own abilities are blocked.
        assert!(
            is_blocked_by_cant_be_activated(&state, PlayerId(0), prohibitor, &prohibitor_ability),
            "SelfRef must block the prohibitor's own activations"
        );

        // Another, unrelated artifact with activated ability is NOT blocked.
        let other = add_artifact_with_activated_ability(&mut state, PlayerId(0));
        let other_ability = state.objects[&other].abilities[0].clone();
        assert!(
            !is_blocked_by_cant_be_activated(&state, PlayerId(0), other, &other_ability),
            "SelfRef must NOT block other permanents' activations"
        );
    }

    // === CR 605.1a: Pithing Needle mana-ability exemption gate ===

    /// Build a Llanowar-Elves-style mana ability: `{T}: Add {G}` (no targets, produces mana).
    fn make_tap_for_green_mana_ability() -> AbilityDefinition {
        use crate::types::ability::{AbilityKind, Effect, ManaProduction};
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
        .cost(crate::types::ability::AbilityCost::Tap)
    }

    #[test]
    fn is_mana_ability_classifier_authoritative() {
        // CR 605.1a: A {T}: Add {G} ability classifies as a mana ability;
        // a tap+sacrifice activated ability with a player target (Mindslaver-shape)
        // does NOT classify as a mana ability.
        let mana_ab = make_tap_for_green_mana_ability();
        assert!(
            super::super::mana_abilities::is_mana_ability(&mana_ab),
            "CR 605.1a: {{T}}: Add {{G}} must classify as a mana ability"
        );

        // Mindslaver-shape: ControlNextTurn does not produce mana.
        let mindslaver_ab = AbilityDefinition::new(
            crate::types::ability::AbilityKind::Activated,
            crate::types::ability::Effect::ControlNextTurn {
                target: TargetFilter::Player,
                grant_extra_turn_after: false,
            },
        )
        .cost(crate::types::ability::AbilityCost::Tap);
        assert!(
            !super::super::mana_abilities::is_mana_ability(&mindslaver_ab),
            "CR 605.1a: ControlNextTurn does not produce mana — must NOT classify as a mana ability"
        );
    }

    #[test]
    fn pithing_needle_blocks_named_non_mana_ability_but_not_mana_ability() {
        // CR 605.1a + CR 602.5: Pithing Needle naming "Llanowar Elves" must
        // - NOT block Llanowar Elves's mana ability ({T}: Add {G}).
        // - Block any non-mana activated ability of a source named "Llanowar Elves".
        use crate::types::ability::ChosenAttribute;
        let mut state = setup_game_at_main_phase();

        // Pithing Needle on P0 with chosen name "Llanowar Elves" and the
        // CantBeActivated(HasChosenName, ManaAbilities) static.
        let needle = create_object(
            &mut state,
            CardId(0x9EED1E),
            PlayerId(0),
            "Pithing Needle".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&needle).unwrap();
            obj.card_types.core_types.push(CoreType::Artifact);
            obj.entered_battlefield_turn = Some(0);
            obj.chosen_attributes
                .push(ChosenAttribute::CardName("Llanowar Elves".to_string()));
            obj.static_definitions
                .push(StaticDefinition::new(StaticMode::CantBeActivated {
                    who: ProhibitionScope::AllPlayers,
                    source_filter: TargetFilter::HasChosenName,
                    exemption: ActivationExemption::ManaAbilities,
                }));
        }

        // Llanowar Elves on P1 with two abilities: [0] mana ({T}: Add {G})
        // and [1] a non-mana Draw ability (synthetic — exercises the gate).
        let elves = create_object(
            &mut state,
            CardId(0xE17E5),
            PlayerId(1),
            "Llanowar Elves".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&elves).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.entered_battlefield_turn = Some(0);
            Arc::make_mut(&mut obj.abilities).push(make_tap_for_green_mana_ability());
            Arc::make_mut(&mut obj.abilities).push(
                AbilityDefinition::new(
                    crate::types::ability::AbilityKind::Activated,
                    crate::types::ability::Effect::Draw {
                        count: QuantityExpr::Fixed { value: 1 },
                        target: crate::types::ability::TargetFilter::Controller,
                    },
                )
                .cost(crate::types::ability::AbilityCost::Tap),
            );
        }

        let mana_ability = state.objects[&elves].abilities[0].clone();
        let non_mana_ability = state.objects[&elves].abilities[1].clone();

        // CR 605.1a: The mana ability is exempt from Pithing Needle's prohibition.
        assert!(
            !is_blocked_by_cant_be_activated(&state, PlayerId(1), elves, &mana_ability),
            "Pithing Needle must NOT block Llanowar Elves's mana ability (CR 605.1a exemption)"
        );

        // CR 602.5: The non-mana ability of the named source is blocked.
        assert!(
            is_blocked_by_cant_be_activated(&state, PlayerId(1), elves, &non_mana_ability),
            "Pithing Needle must block non-mana activated abilities of named sources"
        );

        // An unrelated permanent (different name) is not blocked even on a non-mana ability.
        let other = add_artifact_with_activated_ability(&mut state, PlayerId(1));
        let other_ability = state.objects[&other].abilities[0].clone();
        assert!(
            !is_blocked_by_cant_be_activated(&state, PlayerId(1), other, &other_ability),
            "Pithing Needle must NOT block sources whose name doesn't match the chosen name"
        );
    }

    // === CR 119.8: pay-life cost under CantLoseLife ===

    /// Add a permanent granting `CantLoseLife` to its controller.
    fn add_cant_lose_life_permanent(state: &mut GameState, owner: PlayerId) -> ObjectId {
        use crate::types::statics::StaticMode;
        let id = create_object(
            state,
            CardId(0x5117),
            owner,
            "Life Lock".to_string(),
            Zone::Battlefield,
        );
        state.objects.get_mut(&id).unwrap().static_definitions.push(
            StaticDefinition::new(StaticMode::CantLoseLife).affected(TargetFilter::Typed(
                TypedFilter::default().controller(ControllerRef::You),
            )),
        );
        id
    }

    /// Attach a Greed-style activated ability ({1}{B}, Pay 2 life: Draw a card) to
    /// a freshly-created permanent on the battlefield controlled by `controller`.
    fn add_pay_life_activated_ability(state: &mut GameState, controller: PlayerId) -> ObjectId {
        let id = create_object(
            state,
            CardId(0x6DEE),
            controller,
            "Greed-like".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Enchantment);
        obj.entered_battlefield_turn = Some(0);
        Arc::make_mut(&mut obj.abilities).push(
            crate::types::ability::AbilityDefinition::new(
                crate::types::ability::AbilityKind::Activated,
                crate::types::ability::Effect::Draw {
                    count: QuantityExpr::Fixed { value: 1 },
                    target: crate::types::ability::TargetFilter::Controller,
                },
            )
            .cost(AbilityCost::PayLife {
                amount: QuantityExpr::Fixed { value: 2 },
            }),
        );
        id
    }

    /// CR 119.8: A Greed-style activated ability with `Pay 2 life` cost is filtered
    /// from legal actions when the activating player has CantLoseLife.
    #[test]
    fn pay_life_activated_ability_filtered_under_cant_lose_life() {
        let mut state = setup_game_at_main_phase();
        add_cant_lose_life_permanent(&mut state, PlayerId(0));
        let greed = add_pay_life_activated_ability(&mut state, PlayerId(0));

        assert!(
            !can_activate_ability_now(&state, PlayerId(0), greed, 0),
            "can_activate_ability_now must reject PayLife activation under CantLoseLife"
        );
    }

    /// CR 119.8: Same ability is legal when the controller does NOT have the lock.
    #[test]
    fn pay_life_activated_ability_legal_without_lock() {
        let mut state = setup_game_at_main_phase();
        let greed = add_pay_life_activated_ability(&mut state, PlayerId(0));

        assert!(
            can_activate_ability_now(&state, PlayerId(0), greed, 0),
            "can_activate_ability_now must accept PayLife activation without a lock"
        );
    }

    /// CR 118.3: With `life < amount`, can_activate_ability_now must reject.
    #[test]
    fn pay_life_activated_ability_filtered_when_insufficient_life() {
        let mut state = setup_game_at_main_phase();
        state.players[0].life = 1;
        let greed = add_pay_life_activated_ability(&mut state, PlayerId(0));

        assert!(
            !can_activate_ability_now(&state, PlayerId(0), greed, 0),
            "can_activate_ability_now must reject PayLife activation with insufficient life"
        );
    }

    // === CR 107.4f: Phyrexian mana life-payment integration ===

    /// Build a Gitaxian-Probe-style instant whose cost is a single Phyrexian
    /// shard, plus an optional generic component.
    fn create_phyrexian_instant_in_hand(
        state: &mut GameState,
        player: PlayerId,
        shards: Vec<ManaCostShard>,
        generic: u32,
    ) -> ObjectId {
        let obj_id = create_object(
            state,
            CardId(0x9117),
            player,
            "Phyrexian Probe".to_string(),
            Zone::Hand,
        );
        let obj = state.objects.get_mut(&obj_id).unwrap();
        obj.card_types.core_types.push(CoreType::Instant);
        Arc::make_mut(&mut obj.abilities).push(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
        ));
        obj.mana_cost = ManaCost::Cost { shards, generic };
        obj_id
    }

    /// CR 107.4f + CR 118.3b + CR 119.4: Paying a Phyrexian shard with life
    /// actually deducts 2 life from the caster.
    #[test]
    fn phyrexian_cast_with_life_deducts_life() {
        let mut state = setup_game_at_main_phase();
        let spell = create_phyrexian_instant_in_hand(
            &mut state,
            PlayerId(0),
            vec![ManaCostShard::PhyrexianBlue],
            0,
        );
        // Empty mana pool → the {U/P} must auto-resolve to the 2-life fallback.
        let life_before = state.players[0].life;

        let mut events = Vec::new();
        let result = handle_cast_spell(&mut state, PlayerId(0), spell, CardId(0x9117), &mut events);

        assert!(
            result.is_ok(),
            "cast must succeed paying 2 life for {{U/P}}"
        );
        assert_eq!(
            state.players[0].life,
            life_before - 2,
            "CR 118.3b: paying 2 life must reduce the life total by 2"
        );
        assert!(
            events
                .iter()
                .any(|e| matches!(e, GameEvent::LifeChanged { player_id, amount: -2 } if *player_id == PlayerId(0))),
            "CR 119.4: pay-life must emit a LifeChanged event with amount -2"
        );
    }

    /// CR 107.4f + CR 118.3: With insufficient life and no mana of the color,
    /// the Phyrexian cast is denied.
    #[test]
    fn phyrexian_cast_denied_when_life_insufficient() {
        let mut state = setup_game_at_main_phase();
        state.players[0].life = 1; // < 2
        let spell = create_phyrexian_instant_in_hand(
            &mut state,
            PlayerId(0),
            vec![ManaCostShard::PhyrexianBlue],
            0,
        );

        assert!(
            !can_cast_object_now(&state, PlayerId(0), spell),
            "can_cast_object_now must reject Phyrexian cast when life < 2 and mana unavailable"
        );

        let mut events = Vec::new();
        let result = handle_cast_spell(&mut state, PlayerId(0), spell, CardId(0x9117), &mut events);
        assert!(
            result.is_err(),
            "handle_cast_spell must error when Phyrexian cost is unpayable"
        );
        assert_eq!(
            state.players[0].life, 1,
            "life must be unchanged on denied cast"
        );
    }

    /// CR 107.4f + CR 119.8: Under CantLoseLife, the life fallback is illegal,
    /// so a Phyrexian cast with no mana of the color is denied entirely.
    #[test]
    fn phyrexian_cast_denied_under_cant_lose_life() {
        let mut state = setup_game_at_main_phase();
        add_cant_lose_life_permanent(&mut state, PlayerId(0));
        let spell = create_phyrexian_instant_in_hand(
            &mut state,
            PlayerId(0),
            vec![ManaCostShard::PhyrexianBlue],
            0,
        );

        assert!(
            !can_cast_object_now(&state, PlayerId(0), spell),
            "can_cast_object_now must reject Phyrexian cast under CantLoseLife when mana unavailable"
        );

        let mut events = Vec::new();
        let result = handle_cast_spell(&mut state, PlayerId(0), spell, CardId(0x9117), &mut events);
        assert!(
            result.is_err(),
            "handle_cast_spell must error when Phyrexian cost can't be paid under CantLoseLife"
        );
    }

    /// CR 107.4f + CR 601.2f: Baseline — with both {U} and 2 life viable, the engine
    /// pauses at `WaitingFor::PhyrexianPayment` to let the caster pick per shard. A
    /// `PayMana` choice finalizes the cast without changing life.
    #[test]
    fn phyrexian_cast_with_mana_leaves_life_unchanged() {
        let mut state = setup_game_at_main_phase();
        let spell = create_phyrexian_instant_in_hand(
            &mut state,
            PlayerId(0),
            vec![ManaCostShard::PhyrexianBlue],
            0,
        );
        add_mana(&mut state, PlayerId(0), ManaType::Blue, 1);
        let life_before = state.players[0].life;

        let mut events = Vec::new();
        let waiting =
            handle_cast_spell(&mut state, PlayerId(0), spell, CardId(0x9117), &mut events)
                .expect("announce cast");
        match waiting {
            crate::types::game_state::WaitingFor::PhyrexianPayment { shards, .. } => {
                assert_eq!(shards.len(), 1);
                assert!(matches!(
                    shards[0].options,
                    crate::types::game_state::ShardOptions::ManaOrLife
                ));
            }
            other => panic!("expected PhyrexianPayment, got {other:?}"),
        }
        // Submit PayMana choice via direct resume helper.
        let choices = vec![crate::types::game_state::ShardChoice::PayMana];
        let result = super::casting_costs::finalize_mana_payment_with_phyrexian_choices(
            &mut state,
            PlayerId(0),
            &choices,
            &mut events,
        );
        assert!(result.is_ok(), "resume with PayMana must succeed");
        assert_eq!(
            state.players[0].life, life_before,
            "paying mana (not life) must not change life total"
        );
    }

    /// CR 107.4f + CR 118.3b: Multi-Phyrexian cost paid entirely with life —
    /// each shard deducts 2, total life loss = 2 × shard_count.
    #[test]
    fn phyrexian_multi_shard_all_life_deducts_per_shard() {
        let mut state = setup_game_at_main_phase();
        let spell = create_phyrexian_instant_in_hand(
            &mut state,
            PlayerId(0),
            vec![ManaCostShard::PhyrexianWhite, ManaCostShard::PhyrexianBlue],
            0,
        );
        let life_before = state.players[0].life;

        let mut events = Vec::new();
        let result = handle_cast_spell(&mut state, PlayerId(0), spell, CardId(0x9117), &mut events);

        assert!(
            result.is_ok(),
            "cast must succeed paying 4 life for {{W/P}}{{U/P}}"
        );
        assert_eq!(
            state.players[0].life,
            life_before - 4,
            "two Phyrexian shards paid with life must deduct 2+2 = 4 life"
        );
    }

    /// CR 107.4f + CR 118.3b + CR 601.2f: Mixed Phyrexian payment — one shard has
    /// both options (`ManaOrLife`), the other only life (`LifeOnly`). Engine pauses,
    /// caster submits PayMana for shard 0 and PayLife for shard 1, net 2-life deduction.
    #[test]
    fn phyrexian_mixed_one_mana_one_life() {
        let mut state = setup_game_at_main_phase();
        let spell = create_phyrexian_instant_in_hand(
            &mut state,
            PlayerId(0),
            vec![ManaCostShard::PhyrexianWhite, ManaCostShard::PhyrexianBlue],
            0,
        );
        add_mana(&mut state, PlayerId(0), ManaType::White, 1);
        let life_before = state.players[0].life;

        let mut events = Vec::new();
        let waiting =
            handle_cast_spell(&mut state, PlayerId(0), spell, CardId(0x9117), &mut events)
                .expect("announce cast");
        match waiting {
            crate::types::game_state::WaitingFor::PhyrexianPayment { shards, .. } => {
                assert_eq!(shards.len(), 2, "both Phyrexian shards present");
                assert!(matches!(
                    shards[0].options,
                    crate::types::game_state::ShardOptions::ManaOrLife
                ));
                assert!(matches!(
                    shards[1].options,
                    crate::types::game_state::ShardOptions::LifeOnly
                ));
            }
            other => panic!("expected PhyrexianPayment, got {other:?}"),
        }
        let choices = vec![
            crate::types::game_state::ShardChoice::PayMana,
            crate::types::game_state::ShardChoice::PayLife,
        ];
        let result = super::casting_costs::finalize_mana_payment_with_phyrexian_choices(
            &mut state,
            PlayerId(0),
            &choices,
            &mut events,
        );
        assert!(result.is_ok(), "resume with Mana+Life must succeed");
        assert_eq!(
            state.players[0].life,
            life_before - 2,
            "only the mana-unavailable shard falls back to 2 life"
        );
    }

    /// CR 107.4f + CR 601.2f: When every shard has only one viable option, the engine
    /// must auto-decide (no pause) — this mirrors the pre-batch behavior for trivial cases.
    #[test]
    fn phyrexian_cast_no_pause_when_all_shards_trivial() {
        let mut state = setup_game_at_main_phase();
        state.players[0].life = 1; // Life < 2 → LifeOnly impossible; combined with
                                   // an empty pool, cost becomes unpayable but let's
                                   // give mana so every shard is ManaOnly.
        let spell = create_phyrexian_instant_in_hand(
            &mut state,
            PlayerId(0),
            vec![ManaCostShard::PhyrexianBlue],
            0,
        );
        add_mana(&mut state, PlayerId(0), ManaType::Blue, 1);
        let life_before = state.players[0].life;

        let mut events = Vec::new();
        let waiting =
            handle_cast_spell(&mut state, PlayerId(0), spell, CardId(0x9117), &mut events)
                .expect("announce cast");
        // `life = 1` → max_phyrexian_life_payments = 0 → shard options = ManaOnly →
        // auto-decide; no pause; cast proceeds to Priority.
        assert!(
            !matches!(
                waiting,
                crate::types::game_state::WaitingFor::PhyrexianPayment { .. }
            ),
            "trivial-choice casts must not pause for PhyrexianPayment"
        );
        assert_eq!(state.players[0].life, life_before, "life unchanged");
    }

    /// CR 107.4f + CR 601.2f: Full engine round-trip via `apply`. Both options viable →
    /// dispatcher returns `PhyrexianPayment`; submitting `SubmitPhyrexianChoices` advances
    /// to `Priority`; life is unchanged if `PayMana` was chosen.
    #[test]
    fn phyrexian_engine_round_trip_dispatcher() {
        use crate::game::engine::apply_as_current;
        use crate::types::game_state::{ShardChoice, ShardOptions};

        let mut state = setup_game_at_main_phase();
        let spell = create_phyrexian_instant_in_hand(
            &mut state,
            PlayerId(0),
            vec![ManaCostShard::PhyrexianBlue],
            0,
        );
        add_mana(&mut state, PlayerId(0), ManaType::Blue, 1);
        let life_before = state.players[0].life;

        let cast = GameAction::CastSpell {
            object_id: spell,
            card_id: CardId(0x9117),
            targets: Vec::new(),
        };
        let result = apply_as_current(&mut state, cast).expect("announce cast");
        match &result.waiting_for {
            crate::types::game_state::WaitingFor::PhyrexianPayment { shards, .. } => {
                assert_eq!(shards.len(), 1);
                assert!(matches!(shards[0].options, ShardOptions::ManaOrLife));
            }
            other => panic!("expected PhyrexianPayment, got {other:?}"),
        }

        // Submit PayMana.
        let submit = GameAction::SubmitPhyrexianChoices {
            choices: vec![ShardChoice::PayMana],
        };
        let result = apply_as_current(&mut state, submit).expect("submit choices");
        assert_eq!(
            state.players[0].life, life_before,
            "PayMana keeps life unchanged"
        );
        // The waiting_for advances past PhyrexianPayment.
        assert!(!matches!(
            result.waiting_for,
            crate::types::game_state::WaitingFor::PhyrexianPayment { .. }
        ));
    }

    /// CR 107.4f + CR 601.2f + CR 118.3: Engine round-trip — submitting PayLife deducts 2 life.
    #[test]
    fn phyrexian_engine_round_trip_pay_life() {
        use crate::game::engine::apply_as_current;
        use crate::types::game_state::ShardChoice;

        let mut state = setup_game_at_main_phase();
        let spell = create_phyrexian_instant_in_hand(
            &mut state,
            PlayerId(0),
            vec![ManaCostShard::PhyrexianBlue],
            0,
        );
        add_mana(&mut state, PlayerId(0), ManaType::Blue, 1);
        let life_before = state.players[0].life;

        let cast = GameAction::CastSpell {
            object_id: spell,
            card_id: CardId(0x9117),
            targets: Vec::new(),
        };
        let _ = apply_as_current(&mut state, cast).expect("announce cast");
        let submit = GameAction::SubmitPhyrexianChoices {
            choices: vec![ShardChoice::PayLife],
        };
        let _ = apply_as_current(&mut state, submit).expect("submit choices");
        assert_eq!(
            state.players[0].life,
            life_before - 2,
            "PayLife deducts 2 per shard"
        );
    }

    /// CR 107.4f + CR 601.2f: Engine dispatcher rejects submitting the wrong number of
    /// choices.
    #[test]
    fn phyrexian_engine_rejects_mismatched_choice_count() {
        use crate::game::engine::apply_as_current;
        use crate::types::game_state::ShardChoice;

        let mut state = setup_game_at_main_phase();
        let spell = create_phyrexian_instant_in_hand(
            &mut state,
            PlayerId(0),
            vec![ManaCostShard::PhyrexianBlue],
            0,
        );
        add_mana(&mut state, PlayerId(0), ManaType::Blue, 1);

        let cast = GameAction::CastSpell {
            object_id: spell,
            card_id: CardId(0x9117),
            targets: Vec::new(),
        };
        let _ = apply_as_current(&mut state, cast).expect("announce cast");
        let submit = GameAction::SubmitPhyrexianChoices {
            choices: vec![ShardChoice::PayMana, ShardChoice::PayLife], // length 2 vs 1 shard
        };
        let result = apply_as_current(&mut state, submit);
        assert!(result.is_err(), "mismatched choice count must error");
    }

    /// CR 107.4f + CR 601.2f + CR 118.3: Submitting PayLife with insufficient life
    /// (i.e., life dropped mid-cast) is rejected via validation at resume time.
    #[test]
    fn phyrexian_submit_rejects_stale_paylife_under_insufficient_life() {
        let mut state = setup_game_at_main_phase();
        let spell = create_phyrexian_instant_in_hand(
            &mut state,
            PlayerId(0),
            vec![ManaCostShard::PhyrexianBlue],
            0,
        );
        add_mana(&mut state, PlayerId(0), ManaType::Blue, 1);

        let mut events = Vec::new();
        let _waiting =
            handle_cast_spell(&mut state, PlayerId(0), spell, CardId(0x9117), &mut events)
                .expect("announce cast");
        // Now mid-cast, life drops to 1 (below the 2-life threshold for a Phyrexian shard).
        state.players[0].life = 1;

        // The current shard options would be `ManaOnly` (mana available, life_budget=0),
        // so compute_phyrexian_shards reports ManaOnly — engine dispatch validation
        // will reject PayLife. This path is exercised through the dispatcher, not directly.
        // Here we assert the shape is correct by re-computing shards.
        let spell_meta = build_spell_meta(&state, PlayerId(0), spell);
        let any_color =
            crate::game::static_abilities::player_can_spend_as_any_color(&state, PlayerId(0));
        let max_life = crate::game::life_costs::max_phyrexian_life_payments(&state, PlayerId(0));
        let spell_ctx = spell_meta.as_ref().map(PaymentContext::Spell);
        let current_shards = crate::game::mana_payment::compute_phyrexian_shards(
            &state.players[0].mana_pool,
            &state.objects.get(&spell).unwrap().mana_cost,
            spell_ctx.as_ref(),
            any_color,
            max_life,
        );
        assert_eq!(current_shards.len(), 1);
        assert!(
            matches!(
                current_shards[0].options,
                crate::types::game_state::ShardOptions::ManaOnly
            ),
            "after life drop below 2, shard options must collapse to ManaOnly"
        );
    }

    // --- CR 702.190 Sneak cast-path tests ---

    /// Build: active player with
    /// - an unblocked attacker on battlefield (creature_to_return candidate)
    /// - a creature card in HAND with intrinsic Sneak({3}{B}) (CR 702.190a:
    ///   Sneak is cast from hand)
    /// - enough mana to pay {3}{B}
    /// - phase set to DeclareBlockers with a non-empty combat state
    fn setup_sneak_scenario() -> (GameState, ObjectId, ObjectId) {
        let mut state = setup_game_at_main_phase();
        state.turn_number = 2;
        state.phase = Phase::DeclareBlockers;
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);
        state.waiting_for = WaitingFor::Priority {
            player: PlayerId(0),
        };

        // Unblocked attacker controlled by player 0, already on battlefield
        let attacker_id = create_object(
            &mut state,
            CardId(100),
            PlayerId(0),
            "Grizzly Bears".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&attacker_id).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.power = Some(2);
            obj.toughness = Some(2);
            obj.tapped = true;
            obj.entered_battlefield_turn = Some(1);
        }
        state.combat = Some(crate::game::combat::CombatState {
            attackers: vec![crate::game::combat::AttackerInfo::attacking_player(
                attacker_id,
                PlayerId(1),
            )],
            ..Default::default()
        });

        // Creature card in HAND with intrinsic Sneak({3}{B}) + mana cost {4}{B}{B}
        // so we can distinguish sneak-cost from normal-cost payments.
        // CR 702.190a: Sneak is a hand-cast alternative cost.
        let sneak_card_id = create_object(
            &mut state,
            CardId(200),
            PlayerId(0),
            "Sneaky Beast".to_string(),
            Zone::Hand,
        );
        {
            let obj = state.objects.get_mut(&sneak_card_id).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.power = Some(4);
            obj.toughness = Some(4);
            obj.mana_cost = ManaCost::Cost {
                generic: 4,
                shards: vec![ManaCostShard::Black, ManaCostShard::Black],
            };
            obj.keywords.push(Keyword::Sneak(ManaCost::Cost {
                generic: 3,
                shards: vec![ManaCostShard::Black],
            }));
            obj.base_keywords = obj.keywords.clone();
            // Ensure hand list is consistent.
            if !state.players[0].hand.contains(&sneak_card_id) {
                state.players[0].hand.push_back(sneak_card_id);
            }
        }

        // Mana: {3}{B}
        add_mana(&mut state, PlayerId(0), ManaType::Black, 1);
        add_mana(&mut state, PlayerId(0), ManaType::Colorless, 3);

        (state, attacker_id, sneak_card_id)
    }

    #[test]
    fn sneak_cast_rejected_outside_declare_blockers() {
        let (mut state, attacker_id, sneak_card_id) = setup_sneak_scenario();
        state.phase = Phase::PreCombatMain;
        let card_id = state.objects.get(&sneak_card_id).unwrap().card_id;
        let mut events = Vec::new();
        let result = handle_cast_spell_as_sneak(
            &mut state,
            PlayerId(0),
            sneak_card_id,
            card_id,
            attacker_id,
            &mut events,
        );
        assert!(
            result.is_err(),
            "Sneak outside declare-blockers should fail"
        );
    }

    #[test]
    fn sneak_cast_succeeds_and_pays_sneak_cost() {
        let (mut state, attacker_id, sneak_card_id) = setup_sneak_scenario();
        let card_id = state.objects.get(&sneak_card_id).unwrap().card_id;
        let pool_before = state.players[0].mana_pool.total();
        let mut events = Vec::new();
        handle_cast_spell_as_sneak(
            &mut state,
            PlayerId(0),
            sneak_card_id,
            card_id,
            attacker_id,
            &mut events,
        )
        .expect("Sneak cast should succeed");
        let pool_after = state.players[0].mana_pool.total();
        // Sneak cost {3}{B} = 4 units paid; normal cost is {4}{B}{B} = 6 units.
        assert_eq!(
            pool_before - pool_after,
            4,
            "Should pay Sneak cost ({{3}}{{B}}) not normal cost ({{4}}{{B}}{{B}})"
        );
        // Returned creature goes to hand.
        let attacker = state.objects.get(&attacker_id).unwrap();
        assert_eq!(
            attacker.zone,
            Zone::Hand,
            "Returned creature should be bounced to hand"
        );
        // Spell on stack.
        assert!(
            !state.stack.is_empty(),
            "Sneak-cast spell should be on the stack"
        );
    }

    #[test]
    fn sneak_cast_rejected_when_creature_not_an_attacker() {
        let (mut state, _attacker_id, sneak_card_id) = setup_sneak_scenario();
        // Create a non-attacking creature controlled by player 0.
        let non_attacker = create_object(
            &mut state,
            CardId(300),
            PlayerId(0),
            "Idle Creature".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&non_attacker).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.power = Some(1);
            obj.toughness = Some(1);
        }
        let card_id = state.objects.get(&sneak_card_id).unwrap().card_id;
        let mut events = Vec::new();
        let result = handle_cast_spell_as_sneak(
            &mut state,
            PlayerId(0),
            sneak_card_id,
            card_id,
            non_attacker,
            &mut events,
        );
        assert!(
            result.is_err(),
            "Sneak with non-attacker should be rejected"
        );
    }

    #[test]
    fn sneak_cast_rejected_when_no_sneak_on_card() {
        let (mut state, attacker_id, _sneak_card_id) = setup_sneak_scenario();
        // Create a plain hand creature with no Sneak.
        let plain_card = create_object(
            &mut state,
            CardId(400),
            PlayerId(0),
            "Plain Creature".to_string(),
            Zone::Hand,
        );
        {
            let obj = state.objects.get_mut(&plain_card).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.mana_cost = ManaCost::Cost {
                generic: 1,
                shards: vec![],
            };
            if !state.players[0].hand.contains(&plain_card) {
                state.players[0].hand.push_back(plain_card);
            }
        }
        let card_id = state.objects.get(&plain_card).unwrap().card_id;
        let mut events = Vec::new();
        let result = handle_cast_spell_as_sneak(
            &mut state,
            PlayerId(0),
            plain_card,
            card_id,
            attacker_id,
            &mut events,
        );
        assert!(
            result.is_err(),
            "Sneak-cast of non-Sneak card should be rejected"
        );
    }

    #[test]
    fn sneak_cast_resolves_tapped_and_attacking() {
        let (mut state, attacker_id, sneak_card_id) = setup_sneak_scenario();
        let card_id = state.objects.get(&sneak_card_id).unwrap().card_id;
        let mut events = Vec::new();
        handle_cast_spell_as_sneak(
            &mut state,
            PlayerId(0),
            sneak_card_id,
            card_id,
            attacker_id,
            &mut events,
        )
        .expect("cast should succeed");

        // Resolve the spell on the stack.
        crate::game::stack::resolve_top(&mut state, &mut events);

        let obj = state.objects.get(&sneak_card_id).unwrap();
        assert_eq!(
            obj.zone,
            Zone::Battlefield,
            "Resolved Sneak creature should be on battlefield"
        );
        assert!(
            obj.tapped,
            "Sneak creature should enter tapped (CR 702.190b)"
        );
        let combat = state.combat.as_ref().unwrap();
        let placed = combat
            .attackers
            .iter()
            .find(|a| a.object_id == sneak_card_id)
            .expect("Sneak creature should be in attackers");
        assert_eq!(
            placed.defending_player,
            PlayerId(1),
            "Should attack same defender as returned creature"
        );
        assert_eq!(
            obj.cast_variant_paid,
            Some((
                crate::types::ability::CastVariantPaid::Sneak,
                state.turn_number
            )),
            "Sneak resolution should tag cast_variant_paid"
        );
        // No AttackersDeclared event for the Sneak creature.
        let has_attackers_declared = events
            .iter()
            .any(|e| matches!(e, GameEvent::AttackersDeclared { .. }));
        assert!(
            !has_attackers_declared,
            "Sneak resolution must not fire AttackersDeclared"
        );
    }

    #[test]
    fn sneak_cast_legal_action_in_declare_blockers() {
        let (state, attacker_id, sneak_card_id) = setup_sneak_scenario();
        let card_id = state.objects.get(&sneak_card_id).unwrap().card_id;
        let actions = crate::ai_support::legal_actions(&state);
        let has_sneak_cast = actions.iter().any(|a| {
            matches!(
                a,
                GameAction::CastSpellAsSneak {
                    hand_object,
                    card_id: cid,
                    creature_to_return,
                } if *hand_object == sneak_card_id
                    && *cid == card_id
                    && *creature_to_return == attacker_id
            )
        });
        assert!(
            has_sneak_cast,
            "legal_actions should include CastSpellAsSneak in DeclareBlockers"
        );
    }

    #[test]
    fn sneak_cast_not_legal_action_outside_declare_blockers() {
        let (mut state, _attacker_id, _sneak_card_id) = setup_sneak_scenario();
        state.phase = Phase::PreCombatMain;
        state.combat = None;
        let actions = crate::ai_support::legal_actions(&state);
        let has_sneak_cast = actions
            .iter()
            .any(|a| matches!(a, GameAction::CastSpellAsSneak { .. }));
        assert!(
            !has_sneak_cast,
            "CastSpellAsSneak should not be offered outside DeclareBlockers"
        );
    }

    /// CR 702.190a: A non-permanent (sorcery) spell with Sneak can be cast
    /// from hand. CR 702.190b does NOT apply — the spell resolves normally
    /// and `place_attacking_alongside` must not fire. The returned creature
    /// is still bounced to hand as part of paying the Sneak cost.
    #[test]
    fn sneak_cast_hand_sorcery_resolves_without_alongside_attacker() {
        let (mut state, attacker_id, _creature_sneak_id) = setup_sneak_scenario();

        // Add a SORCERY in hand with Sneak({1}{W}) — mirrors Leonardo's Technique.
        let sorcery_id = create_object(
            &mut state,
            CardId(500),
            PlayerId(0),
            "Sneaky Sorcery".to_string(),
            Zone::Hand,
        );
        {
            let obj = state.objects.get_mut(&sorcery_id).unwrap();
            obj.card_types.core_types.push(CoreType::Sorcery);
            obj.mana_cost = ManaCost::Cost {
                generic: 3,
                shards: vec![ManaCostShard::White],
            };
            obj.keywords.push(Keyword::Sneak(ManaCost::Cost {
                generic: 1,
                shards: vec![ManaCostShard::White],
            }));
            obj.base_keywords = obj.keywords.clone();
            if !state.players[0].hand.contains(&sorcery_id) {
                state.players[0].hand.push_back(sorcery_id);
            }
        }
        // Sneak cost {1}{W} requires 1 white; grant it.
        add_mana(&mut state, PlayerId(0), ManaType::White, 1);

        let card_id = state.objects.get(&sorcery_id).unwrap().card_id;
        let mut events = Vec::new();
        handle_cast_spell_as_sneak(
            &mut state,
            PlayerId(0),
            sorcery_id,
            card_id,
            attacker_id,
            &mut events,
        )
        .expect("Sneak cast of sorcery from hand should succeed");

        // The returned creature is bounced to hand (CR 702.190a).
        let returned = state.objects.get(&attacker_id).unwrap();
        assert_eq!(
            returned.zone,
            Zone::Hand,
            "Returned attacker should go to hand per CR 702.190a"
        );
        // The sorcery is on the stack.
        assert!(
            state.stack.iter().any(|e| e.id == sorcery_id),
            "Sorcery should be on the stack"
        );

        // Inspect the stack entry's casting_variant — placement must be None
        // for a non-permanent spell.
        let stack_entry = state
            .stack
            .iter()
            .find(|e| e.id == sorcery_id)
            .expect("sorcery on stack");
        if let StackEntryKind::Spell {
            casting_variant, ..
        } = &stack_entry.kind
        {
            match casting_variant {
                CastingVariant::Sneak { placement, .. } => {
                    assert!(
                        placement.is_none(),
                        "CR 702.190b does not apply to non-permanent spells; placement must be None"
                    );
                }
                other => panic!("expected CastingVariant::Sneak, got {other:?}"),
            }
        } else {
            panic!("stack entry should be a Spell");
        }

        // Resolve the sorcery.
        crate::game::stack::resolve_top(&mut state, &mut events);
        let obj = state.objects.get(&sorcery_id).unwrap();
        // CR 608.2n: Non-permanent spells go to owner's graveyard on resolution.
        assert_eq!(
            obj.zone,
            Zone::Graveyard,
            "Resolved sorcery should go to graveyard, not battlefield"
        );
        // `place_attacking_alongside` MUST NOT have fired for a non-permanent
        // spell — the sorcery itself must not appear among attackers.
        if let Some(combat) = state.combat.as_ref() {
            assert!(
                !combat.attackers.iter().any(|a| a.object_id == sorcery_id),
                "CR 702.190b: non-permanent Sneak cast must not enter combat as an attacker"
            );
        }
    }

    /// CR 702.190a: Sneak is cast from HAND. Casting a Sneak object whose
    /// source zone is anything other than the caster's hand must be rejected,
    /// even if the object has an effective Sneak keyword. Covers the general
    /// zone rule, not just the graveyard special case.
    #[test]
    fn sneak_cast_requires_source_in_hand() {
        for bad_zone in [Zone::Graveyard, Zone::Exile, Zone::Battlefield] {
            let (mut state, attacker_id, sneak_card_id) = setup_sneak_scenario();
            // Relocate the Sneak card out of hand into `bad_zone` and sync the
            // owning zone list where applicable.
            {
                let obj = state.objects.get_mut(&sneak_card_id).unwrap();
                obj.zone = bad_zone;
            }
            state.players[0].hand.retain(|id| *id != sneak_card_id);
            match bad_zone {
                Zone::Graveyard => state.players[0].graveyard.push_back(sneak_card_id),
                Zone::Exile => state.exile.push_back(sneak_card_id),
                Zone::Battlefield => state.battlefield.push_back(sneak_card_id),
                _ => unreachable!(),
            }

            let card_id = state.objects.get(&sneak_card_id).unwrap().card_id;
            let mut events = Vec::new();
            let result = handle_cast_spell_as_sneak(
                &mut state,
                PlayerId(0),
                sneak_card_id,
                card_id,
                attacker_id,
                &mut events,
            );
            assert!(
                result.is_err(),
                "CR 702.190a: Sneak cast from {bad_zone:?} must be rejected \
                 (source zone must be Hand)"
            );
        }
    }

    // === CR 302.6 + CR 602.5a: summoning-sickness gate tests ===
    //
    // Exercise the universal `check_summoning_sickness_for_cost` helper through
    // `can_activate_ability_now` — the single gate shared by human runtime
    // activation and AI legal-action generation.
    mod summoning_sickness_gate {
        use super::*;
        use crate::game::derived::derive_display_state;

        /// Attach a creature with a Tap-cost activated ability on `player`'s battlefield,
        /// entering on `entered_turn`. Returns the ObjectId.
        fn add_creature_with_tap_ability(
            state: &mut GameState,
            player: PlayerId,
            entered_turn: u32,
        ) -> ObjectId {
            let id = create_object(
                state,
                CardId(0x5ACF),
                player,
                "Tappy McTap".to_string(),
                Zone::Battlefield,
            );
            let current_turn = state.turn_number;
            let obj = state.objects.get_mut(&id).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.entered_battlefield_turn = Some(entered_turn);
            obj.summoning_sick = entered_turn >= current_turn;
            Arc::make_mut(&mut obj.abilities).push(
                AbilityDefinition::new(
                    AbilityKind::Activated,
                    Effect::Draw {
                        count: QuantityExpr::Fixed { value: 1 },
                        target: TargetFilter::Controller,
                    },
                )
                .cost(AbilityCost::Tap),
            );
            id
        }

        #[test]
        fn creature_cast_this_turn_cannot_tap() {
            // Krenko reprints itself — tap-for-creature the same turn it enters is illegal.
            let mut state = setup_game_at_main_phase();
            let turn = state.turn_number;
            let krenko = add_creature_with_tap_ability(&mut state, PlayerId(0), turn);
            derive_display_state(&mut state);
            assert!(
                !can_activate_ability_now(&state, PlayerId(0), krenko, 0),
                "summoning-sick creature's {{T}} ability must not be activatable (CR 302.6)"
            );
        }

        #[test]
        fn creature_cast_previous_turn_can_tap() {
            let mut state = setup_game_at_main_phase();
            // turn_number = 2 in setup; entered on turn 1.
            let krenko = add_creature_with_tap_ability(&mut state, PlayerId(0), 1);
            derive_display_state(&mut state);
            assert!(
                can_activate_ability_now(&state, PlayerId(0), krenko, 0),
                "creature under controller's control since prior turn may tap (CR 302.6)"
            );
        }

        #[test]
        fn haste_creature_can_tap_same_turn() {
            let mut state = setup_game_at_main_phase();
            let turn = state.turn_number;
            let krenko = add_creature_with_tap_ability(&mut state, PlayerId(0), turn);
            {
                let obj = state.objects.get_mut(&krenko).unwrap();
                obj.keywords.push(Keyword::Haste);
            }
            derive_display_state(&mut state);
            assert!(
                can_activate_ability_now(&state, PlayerId(0), krenko, 0),
                "haste exempts a creature from summoning sickness (CR 702.10c)"
            );
        }

        #[test]
        fn non_creature_artifact_can_tap_same_turn() {
            // Sensei's Divining Top: artifact with {T} cost, no summoning sickness.
            let mut state = setup_game_at_main_phase();
            let turn = state.turn_number;
            let top = create_object(
                &mut state,
                CardId(0x7077),
                PlayerId(0),
                "Sensei's Divining Top".to_string(),
                Zone::Battlefield,
            );
            {
                let obj = state.objects.get_mut(&top).unwrap();
                obj.card_types.core_types.push(CoreType::Artifact);
                obj.entered_battlefield_turn = Some(turn);
                Arc::make_mut(&mut obj.abilities).push(
                    AbilityDefinition::new(
                        AbilityKind::Activated,
                        Effect::Draw {
                            count: QuantityExpr::Fixed { value: 1 },
                            target: TargetFilter::Controller,
                        },
                    )
                    .cost(AbilityCost::Tap),
                );
            }
            derive_display_state(&mut state);
            assert!(
                can_activate_ability_now(&state, PlayerId(0), top, 0),
                "non-creature permanents are not subject to summoning sickness (CR 302.6)"
            );
        }

        #[test]
        fn animated_land_this_turn_cannot_tap() {
            // Land animated into a creature this turn is subject to summoning sickness.
            let mut state = setup_game_at_main_phase();
            let turn = state.turn_number;
            let land = create_object(
                &mut state,
                CardId(0x1A4D),
                PlayerId(0),
                "Mutavault-like".to_string(),
                Zone::Battlefield,
            );
            {
                let obj = state.objects.get_mut(&land).unwrap();
                obj.card_types.core_types.push(CoreType::Land);
                // Animation: the permanent is currently a creature too.
                obj.card_types.core_types.push(CoreType::Creature);
                obj.entered_battlefield_turn = Some(turn);
                obj.summoning_sick = true;
                Arc::make_mut(&mut obj.abilities).push(
                    AbilityDefinition::new(
                        AbilityKind::Activated,
                        Effect::Draw {
                            count: QuantityExpr::Fixed { value: 1 },
                            target: TargetFilter::Controller,
                        },
                    )
                    .cost(AbilityCost::Tap),
                );
            }
            derive_display_state(&mut state);
            assert!(
                !can_activate_ability_now(&state, PlayerId(0), land, 0),
                "currently-a-creature animated land must obey summoning sickness (CR 302.6)"
            );
        }

        #[test]
        fn untap_cost_also_gated() {
            // CR 107.6 / CR 302.6: {Q} is likewise gated by summoning sickness.
            let mut state = setup_game_at_main_phase();
            let turn = state.turn_number;
            let creature = create_object(
                &mut state,
                CardId(0x8A7A),
                PlayerId(0),
                "Q-cost Creature".to_string(),
                Zone::Battlefield,
            );
            {
                let obj = state.objects.get_mut(&creature).unwrap();
                obj.card_types.core_types.push(CoreType::Creature);
                obj.entered_battlefield_turn = Some(turn);
                obj.summoning_sick = true;
                // Already tapped so Untap cost is payable mechanically.
                obj.tapped = true;
                Arc::make_mut(&mut obj.abilities).push(
                    AbilityDefinition::new(
                        AbilityKind::Activated,
                        Effect::Draw {
                            count: QuantityExpr::Fixed { value: 1 },
                            target: TargetFilter::Controller,
                        },
                    )
                    .cost(AbilityCost::Untap),
                );
            }
            derive_display_state(&mut state);
            assert!(
                !can_activate_ability_now(&state, PlayerId(0), creature, 0),
                "creature with {{Q}} cost must obey summoning sickness (CR 107.6 + CR 302.6)"
            );
        }
    }

    // CR 118.3 + CR 122: remove-counter cost payment — building-block tests.
    // These exercise the `AbilityCost::RemoveCounter` arm of `pay_ability_cost`
    // directly (not through Mindless Automaton) so the primitive is covered for
    // any activated ability whose cost is "Remove N {type} counters from ~".
    mod remove_counter_cost {
        use super::*;
        use crate::types::counter::CounterType;

        fn source_with_counters(
            state: &mut GameState,
            counter_type: CounterType,
            count: u32,
        ) -> ObjectId {
            let id = create_object(
                state,
                CardId(900),
                PlayerId(0),
                "Mindless Automaton".to_string(),
                Zone::Battlefield,
            );
            if count > 0 {
                state
                    .objects
                    .get_mut(&id)
                    .unwrap()
                    .counters
                    .insert(counter_type, count);
            }
            id
        }

        #[test]
        fn pays_when_counters_present() {
            let mut state = setup_game_at_main_phase();
            let source = source_with_counters(&mut state, CounterType::Plus1Plus1, 2);
            let cost = AbilityCost::RemoveCounter {
                count: 2,
                counter_type: "+1/+1".to_string(),
                target: None,
            };
            let mut events = Vec::new();
            pay_ability_cost(&mut state, PlayerId(0), source, &cost, &mut events)
                .expect("cost should pay with 2 +1/+1 counters available");
            let remaining = state
                .objects
                .get(&source)
                .unwrap()
                .counters
                .get(&CounterType::Plus1Plus1)
                .copied()
                .unwrap_or(0);
            assert_eq!(remaining, 0, "both +1/+1 counters should be removed");
            assert!(
                events.iter().any(|e| matches!(
                    e,
                    GameEvent::CounterRemoved {
                        object_id,
                        counter_type: CounterType::Plus1Plus1,
                        count: 2,
                    } if *object_id == source
                )),
                "CounterRemoved event for 2 P1P1 should be emitted, got {events:?}"
            );
        }

        // CR 118.3: a player can't pay a cost without the necessary resources.
        // Legality is enforced by `is_payable` before activation commits, so
        // with zero counters the cost is not payable.
        #[test]
        fn not_payable_without_counters() {
            let mut state = setup_game_at_main_phase();
            let source = source_with_counters(&mut state, CounterType::Plus1Plus1, 0);
            let cost = AbilityCost::RemoveCounter {
                count: 1,
                counter_type: "+1/+1".to_string(),
                target: None,
            };
            assert!(
                !cost.is_payable(&state, PlayerId(0), source),
                "cost must be unpayable when the source has no +1/+1 counters"
            );
            assert!(
                !can_pay_ability_cost_now(&state, PlayerId(0), source, &cost),
                "can_pay_ability_cost_now must reject an unpayable remove-counter cost"
            );
        }

        #[test]
        fn not_payable_with_insufficient_counters() {
            let mut state = setup_game_at_main_phase();
            let source = source_with_counters(&mut state, CounterType::Plus1Plus1, 1);
            let cost = AbilityCost::RemoveCounter {
                count: 2,
                counter_type: "+1/+1".to_string(),
                target: None,
            };
            assert!(
                !cost.is_payable(&state, PlayerId(0), source),
                "cost must be unpayable when the source has fewer than N counters"
            );
        }

        // CR 614.1a: replacement effects see counter-removal events. Because
        // payment routes through `remove_counter_with_replacement`, effects such
        // as Vorinclex (doubling) or shield-style prevention apply. Verified
        // indirectly here by observing the event shape and that the pipeline
        // was invoked via the single-authority primitive.
        #[test]
        fn emits_counter_removed_through_replacement_pipeline() {
            let mut state = setup_game_at_main_phase();
            let source = source_with_counters(&mut state, CounterType::Plus1Plus1, 3);
            let cost = AbilityCost::RemoveCounter {
                count: 1,
                counter_type: "+1/+1".to_string(),
                target: None,
            };
            let mut events = Vec::new();
            pay_ability_cost(&mut state, PlayerId(0), source, &cost, &mut events).unwrap();
            let removed_count = events
                .iter()
                .filter_map(|e| match e {
                    GameEvent::CounterRemoved {
                        object_id,
                        counter_type: CounterType::Plus1Plus1,
                        count,
                    } if *object_id == source => Some(*count),
                    _ => None,
                })
                .sum::<u32>();
            assert_eq!(removed_count, 1);
            assert_eq!(
                state
                    .objects
                    .get(&source)
                    .unwrap()
                    .counters
                    .get(&CounterType::Plus1Plus1)
                    .copied()
                    .unwrap_or(0),
                2,
                "one counter removed, two remain"
            );
        }
    }

    /// CR 702.96a-c: Overload end-to-end — `handle_cast_spell` on a hand card
    /// with `Keyword::Overload(cost)` offers `WaitingFor::OverloadCostChoice`
    /// when both costs are affordable, and selecting overload prepares the
    /// spell with `CastingVariant::Overload`, substitutes the overload cost,
    /// and transforms the ability's `Destroy { target }` into `DestroyAll`.
    mod overload_cast_flow {
        use super::*;
        use crate::types::keywords::Keyword;
        use crate::types::mana::ManaCost;

        fn create_damn_in_hand(state: &mut GameState, player: PlayerId) -> ObjectId {
            let obj_id = create_object(state, CardId(42), player, "Damn".to_string(), Zone::Hand);
            let obj = state.objects.get_mut(&obj_id).unwrap();
            obj.card_types.core_types.push(CoreType::Sorcery);
            // Printed cost: {1}{B}. Overload cost: {2}{W}{W}.
            obj.mana_cost = ManaCost::Cost {
                shards: vec![ManaCostShard::Black],
                generic: 1,
            };
            Arc::make_mut(&mut obj.abilities).push(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Destroy {
                    target: TargetFilter::Typed(TypedFilter {
                        type_filters: vec![TypeFilter::Creature],
                        controller: None,
                        properties: vec![],
                    }),
                    cant_regenerate: true,
                },
            ));
            obj.keywords.push(Keyword::Overload(ManaCost::Cost {
                shards: vec![ManaCostShard::White, ManaCostShard::White],
                generic: 2,
            }));
            obj_id
        }

        #[test]
        fn offer_overload_when_both_costs_affordable() {
            let mut state = setup_game_at_main_phase();
            // Pay both printed ({1}{B}) and overload ({2}{W}{W}) from a
            // generous mana pool so the offer path is taken.
            add_mana(&mut state, PlayerId(0), ManaType::Black, 2);
            add_mana(&mut state, PlayerId(0), ManaType::White, 4);
            let obj = create_damn_in_hand(&mut state, PlayerId(0));
            let mut events = Vec::new();
            let wf =
                handle_cast_spell(&mut state, PlayerId(0), obj, CardId(42), &mut events).unwrap();
            assert!(
                matches!(wf, WaitingFor::OverloadCostChoice { .. }),
                "expected OverloadCostChoice offer, got {:?}",
                wf
            );
        }

        #[test]
        fn opting_into_overload_transforms_destroy_to_destroy_all() {
            let mut state = setup_game_at_main_phase();
            add_mana(&mut state, PlayerId(0), ManaType::White, 4);
            let obj = create_damn_in_hand(&mut state, PlayerId(0));
            let prepared = prepare_spell_cast_with_variant_override(
                &state,
                PlayerId(0),
                obj,
                Some(CastingVariant::Overload),
            )
            .expect("overload prepare succeeds");
            assert_eq!(prepared.casting_variant, CastingVariant::Overload);
            // Overload mana cost substituted for printed cost.
            match prepared.mana_cost {
                ManaCost::Cost {
                    ref shards,
                    generic,
                } => {
                    assert_eq!(generic, 2);
                    assert_eq!(shards.len(), 2);
                }
                other => panic!("expected overload cost substituted, got {:?}", other),
            }
            // Destroy → DestroyAll.
            let def = prepared.ability_def.expect("spell ability present");
            assert!(
                matches!(*def.effect, Effect::DestroyAll { .. }),
                "expected DestroyAll after overload transform, got {:?}",
                def.effect
            );
        }
    }

    /// CR 701.43a / CR 701.43b / CR 502.3: Exert cost — Arena of Glory class.
    mod exert_cost {
        use super::*;
        use crate::game::turns::execute_untap;

        fn make_battlefield_permanent(state: &mut GameState) -> ObjectId {
            let id = create_object(
                state,
                CardId(10),
                PlayerId(0),
                "Arena of Glory".to_string(),
                Zone::Battlefield,
            );
            state.objects.get_mut(&id).unwrap().card_types.core_types = vec![CoreType::Land];
            id
        }

        /// CR 701.43a: Paying the Exert cost marks the source so it skips its
        /// controller's next untap step. CR 502.3: On that next untap step the
        /// permanent stays tapped and the marker is pruned; on the following
        /// untap step it untaps normally.
        #[test]
        fn exert_skips_next_untap_then_untaps() {
            let mut state = setup_game_at_main_phase();
            let id = make_battlefield_permanent(&mut state);
            // Tap it to mirror Arena of Glory's combined {T}, Exert cost path:
            // the tap is part of the composite cost; we model post-payment state.
            state.objects.get_mut(&id).unwrap().tapped = true;

            let mut events = Vec::new();
            pay_ability_cost(
                &mut state,
                PlayerId(0),
                id,
                &AbilityCost::Exert,
                &mut events,
            )
            .expect("exert cost pays");

            // Effect was added for this permanent with the correct duration.
            let effects: Vec<_> = state
                .transient_continuous_effects
                .iter()
                .filter(
                    |e| matches!(e.affected, TargetFilter::SpecificObject { id: oid } if oid == id),
                )
                .collect();
            assert_eq!(effects.len(), 1);
            assert_eq!(
                effects[0].duration,
                crate::types::ability::Duration::UntilControllerNextUntapStep
            );
            assert!(effects[0].modifications.iter().any(|m| matches!(
                m,
                crate::types::ability::ContinuousModification::AddStaticMode {
                    mode: StaticMode::CantUntap,
                }
            )));

            // CR 502.3: Next untap step — permanent stays tapped, marker pruned.
            state.active_player = PlayerId(0);
            let mut events = Vec::new();
            execute_untap(&mut state, &mut events);
            assert!(
                state.objects[&id].tapped,
                "exerted permanent must not untap during its controller's next untap step"
            );
            assert!(
                !state.transient_continuous_effects.iter().any(|e| {
                    matches!(e.affected, TargetFilter::SpecificObject { id: oid } if oid == id)
                }),
                "exert marker must be pruned after the skipped untap step"
            );

            // Following untap step — untaps normally.
            let mut events = Vec::new();
            execute_untap(&mut state, &mut events);
            assert!(!state.objects[&id].tapped);
        }

        /// CR 701.43b: A permanent can be exerted even if already exerted. Two
        /// effects stack harmlessly — both expire during the same untap step.
        #[test]
        fn exert_is_idempotent() {
            let mut state = setup_game_at_main_phase();
            let id = make_battlefield_permanent(&mut state);

            let mut events = Vec::new();
            pay_ability_cost(
                &mut state,
                PlayerId(0),
                id,
                &AbilityCost::Exert,
                &mut events,
            )
            .expect("first exert");
            pay_ability_cost(
                &mut state,
                PlayerId(0),
                id,
                &AbilityCost::Exert,
                &mut events,
            )
            .expect("second exert");

            let count = state
                .transient_continuous_effects
                .iter()
                .filter(
                    |e| matches!(e.affected, TargetFilter::SpecificObject { id: oid } if oid == id),
                )
                .count();
            assert_eq!(count, 2);

            // Tap then untap step — still stays tapped; both markers pruned together.
            state.objects.get_mut(&id).unwrap().tapped = true;
            state.active_player = PlayerId(0);
            let mut events = Vec::new();
            execute_untap(&mut state, &mut events);
            assert!(state.objects[&id].tapped);
            assert_eq!(
                state
                    .transient_continuous_effects
                    .iter()
                    .filter(|e| matches!(e.affected, TargetFilter::SpecificObject { id: oid } if oid == id))
                    .count(),
                0
            );
        }

        /// CR 701.43c: An object that isn't on the battlefield can't be exerted.
        #[test]
        fn exert_rejects_off_battlefield_source() {
            let mut state = setup_game_at_main_phase();
            let id = create_object(
                &mut state,
                CardId(11),
                PlayerId(0),
                "Not On Field".to_string(),
                Zone::Hand,
            );

            let mut events = Vec::new();
            let result = pay_ability_cost(
                &mut state,
                PlayerId(0),
                id,
                &AbilityCost::Exert,
                &mut events,
            );
            assert!(matches!(result, Err(EngineError::ActionNotAllowed(_))));
            assert_eq!(state.transient_continuous_effects.len(), 0);
        }

        /// CR 502.3: Exert marker on player P's permanent is NOT pruned during
        /// opponent Q's untap step — it persists until P's next untap step.
        #[test]
        fn exert_marker_persists_through_opponent_untap_step() {
            let mut state = setup_game_at_main_phase();
            let id = make_battlefield_permanent(&mut state);

            let mut events = Vec::new();
            pay_ability_cost(
                &mut state,
                PlayerId(0),
                id,
                &AbilityCost::Exert,
                &mut events,
            )
            .expect("exert cost pays");

            // Opponent's untap step runs first — effect must survive.
            state.active_player = PlayerId(1);
            let mut events = Vec::new();
            execute_untap(&mut state, &mut events);
            assert_eq!(
                state
                    .transient_continuous_effects
                    .iter()
                    .filter(|e| matches!(e.affected, TargetFilter::SpecificObject { id: oid } if oid == id))
                    .count(),
                1,
                "exert marker must survive opponent's untap step"
            );

            // Now P's untap step — marker applies and is then pruned.
            state.active_player = PlayerId(0);
            state.objects.get_mut(&id).unwrap().tapped = true;
            let mut events = Vec::new();
            execute_untap(&mut state, &mut events);
            assert!(state.objects[&id].tapped);
            assert_eq!(
                state
                    .transient_continuous_effects
                    .iter()
                    .filter(|e| matches!(e.affected, TargetFilter::SpecificObject { id: oid } if oid == id))
                    .count(),
                0
            );
        }
    }

    // === CR 606.3: Planeswalker per-permanent loyalty gate tests ===
    //
    // CR 606.3: "A player may activate a loyalty ability of a permanent they
    // control any time they have priority and the stack is empty during a
    // main phase of their turn, but only if no player has previously activated
    // a loyalty ability of *that permanent* that turn."
    //
    // Verifies that `can_activate_ability_now` defers to `can_activate_loyalty`
    // so the per-permanent gate is honored. Without the dispatch, the generic
    // `OnlyOnceEachTurn` activation restriction would track per
    // `(source_id, ability_index)` and incorrectly let each loyalty ability of
    // a planeswalker activate independently in the same turn.
    mod loyalty_gate {
        use super::*;
        use crate::types::ability::ActivationRestriction;

        /// Build a loyalty ability with the given amount and a non-targeted draw effect
        /// so target-slot construction never blocks the gate under test. Mirrors
        /// `apply_loyalty_restrictions`'s production output: AsSorcery + OnlyOnceEachTurn.
        fn make_loyalty_ability(amount: i32) -> AbilityDefinition {
            let mut def = AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Draw {
                    count: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::Controller,
                },
            )
            .cost(AbilityCost::Loyalty { amount });
            def.activation_restrictions
                .push(ActivationRestriction::AsSorcery);
            def.activation_restrictions
                .push(ActivationRestriction::OnlyOnceEachTurn);
            def.sorcery_speed = true;
            def
        }

        /// Place a planeswalker on `owner`'s battlefield with the given loyalty and
        /// abilities. Skips summoning sickness (PWs do not have it; CR 302.6 applies
        /// only to {T}/{Q} costs anyway).
        fn add_planeswalker(
            state: &mut GameState,
            owner: PlayerId,
            name: &str,
            loyalty: u32,
            abilities: Vec<AbilityDefinition>,
        ) -> ObjectId {
            let id = create_object(
                state,
                CardId(state.next_object_id),
                owner,
                name.to_string(),
                Zone::Battlefield,
            );
            let obj = state.objects.get_mut(&id).unwrap();
            obj.card_types.core_types.push(CoreType::Planeswalker);
            // CR 306.5b: loyalty IS the count of loyalty counters on the PW.
            obj.loyalty = Some(loyalty);
            obj.counters
                .insert(crate::types::counter::CounterType::Loyalty, loyalty);
            obj.abilities = Arc::new(abilities);
            // Entered prior turn — irrelevant for loyalty (no summoning sickness),
            // but keep it consistent with other PW tests.
            obj.entered_battlefield_turn = Some(state.turn_number - 1);
            id
        }

        /// CR 606.3: After activating loyalty ability A of a planeswalker, every
        /// other loyalty ability on the *same* planeswalker must be denied for
        /// the rest of the turn (per-permanent gate, not per-ability-index).
        #[test]
        fn second_loyalty_ability_same_pw_same_turn_denied() {
            let mut state = setup_game_at_main_phase();
            let pw = add_planeswalker(
                &mut state,
                PlayerId(0),
                "Three-Ability PW",
                4,
                vec![
                    make_loyalty_ability(1),
                    make_loyalty_ability(-1),
                    make_loyalty_ability(-3),
                ],
            );

            // Pre-activation: all three abilities are activatable.
            assert!(
                can_activate_ability_now(&state, PlayerId(0), pw, 0),
                "ability 0 must be activatable before any loyalty activation"
            );
            assert!(
                can_activate_ability_now(&state, PlayerId(0), pw, 1),
                "ability 1 must be activatable before any loyalty activation"
            );
            assert!(
                can_activate_ability_now(&state, PlayerId(0), pw, 2),
                "ability 2 must be activatable before any loyalty activation"
            );

            // Simulate first-loyalty activation by flipping the per-permanent gate
            // (handle_activate_loyalty sets this; we exercise the predicate directly).
            state
                .objects
                .get_mut(&pw)
                .unwrap()
                .loyalty_activated_this_turn = true;

            // CR 606.3: every remaining loyalty ability on the same PW is denied.
            assert!(
                !can_activate_ability_now(&state, PlayerId(0), pw, 0),
                "CR 606.3: ability 0 must be denied after PW's loyalty already used"
            );
            assert!(
                !can_activate_ability_now(&state, PlayerId(0), pw, 1),
                "CR 606.3: ability 1 must be denied after PW's loyalty already used"
            );
            assert!(
                !can_activate_ability_now(&state, PlayerId(0), pw, 2),
                "CR 606.3: ability 2 must be denied after PW's loyalty already used"
            );
        }

        /// CR 606.3: The per-permanent gate resets at the start of the controller's
        /// turn (verified via `loyalty_activation_resets_at_turn_start` in
        /// `planeswalker.rs`). Reproducing the reset effect here ensures the
        /// dispatch path re-allows activation once the flag is cleared.
        #[test]
        fn loyalty_ability_re_allowed_after_turn_reset() {
            let mut state = setup_game_at_main_phase();
            let pw = add_planeswalker(
                &mut state,
                PlayerId(0),
                "PW",
                4,
                vec![make_loyalty_ability(1), make_loyalty_ability(-1)],
            );

            // Mark loyalty as used this turn — both abilities denied.
            state
                .objects
                .get_mut(&pw)
                .unwrap()
                .loyalty_activated_this_turn = true;
            assert!(!can_activate_ability_now(&state, PlayerId(0), pw, 0));
            assert!(!can_activate_ability_now(&state, PlayerId(0), pw, 1));

            // Simulate turn-start reset (turns.rs:260-261 does this for active player).
            state
                .objects
                .get_mut(&pw)
                .unwrap()
                .loyalty_activated_this_turn = false;

            assert!(
                can_activate_ability_now(&state, PlayerId(0), pw, 0),
                "ability 0 must be activatable again after loyalty_activated_this_turn reset"
            );
            assert!(
                can_activate_ability_now(&state, PlayerId(0), pw, 1),
                "ability 1 must be activatable again after loyalty_activated_this_turn reset"
            );
        }

        /// CR 606.3: The gate is per-permanent, not per-controller. Activating a
        /// loyalty ability on PW1 must not lock out loyalty abilities on PW2.
        #[test]
        fn different_planeswalkers_independent() {
            let mut state = setup_game_at_main_phase();
            let pw1 = add_planeswalker(
                &mut state,
                PlayerId(0),
                "PW1",
                4,
                vec![make_loyalty_ability(1), make_loyalty_ability(-1)],
            );
            let pw2 = add_planeswalker(
                &mut state,
                PlayerId(0),
                "PW2",
                4,
                vec![make_loyalty_ability(1), make_loyalty_ability(-1)],
            );

            // Use loyalty on PW1 only.
            state
                .objects
                .get_mut(&pw1)
                .unwrap()
                .loyalty_activated_this_turn = true;

            // PW1 fully locked.
            assert!(!can_activate_ability_now(&state, PlayerId(0), pw1, 0));
            assert!(!can_activate_ability_now(&state, PlayerId(0), pw1, 1));
            // PW2 unaffected — both abilities still activatable.
            assert!(
                can_activate_ability_now(&state, PlayerId(0), pw2, 0),
                "CR 606.3 is per-permanent: PW2 ability 0 must remain activatable"
            );
            assert!(
                can_activate_ability_now(&state, PlayerId(0), pw2, 1),
                "CR 606.3 is per-permanent: PW2 ability 1 must remain activatable"
            );
        }
    }

    /// CR 401.5 + CR 118.9: Realmwalker grants `TopOfLibraryCastPermission`
    /// keyed on the chosen creature type. The top card of library is castable
    /// only when its type matches the static's `affected` filter; cards on
    /// the battlefield/elsewhere never enter `spell_objects_available_to_cast`
    /// via this path. The card must NOT be exiled — it stays in the library
    /// until `finalize_cast` performs the standard library → stack move.
    mod top_of_library_cast_permission_runtime {
        use super::*;
        use crate::types::ability::{
            CardPlayMode, StaticDefinition, TargetFilter, TypeFilter, TypedFilter,
        };
        use crate::types::card_type::CoreType;
        use crate::types::statics::StaticMode;

        fn put_creature_on_top_of_library(
            state: &mut GameState,
            owner: PlayerId,
            card_id: CardId,
        ) -> ObjectId {
            // Create the object and place it explicitly at the front of the
            // library — `create_object(... Zone::Library)` does not guarantee
            // top-of-library positioning across runtime helpers, so we set
            // it ourselves to keep the test scope narrow.
            let obj_id =
                create_object(state, card_id, owner, "Top Card".to_string(), Zone::Library);
            // Stamp it as a creature so the filter check evaluates against
            // realistic characteristics.
            {
                let obj = state.objects.get_mut(&obj_id).unwrap();
                obj.card_types.core_types = vec![CoreType::Creature];
                obj.card_types.subtypes = vec!["Elf".to_string()];
            }
            // Move it to the front of the library.
            let player = state.players.iter_mut().find(|p| p.id == owner).unwrap();
            player.library.retain(|id| *id != obj_id);
            player.library.push_front(obj_id);
            obj_id
        }

        fn install_realmwalker_static(state: &mut GameState, controller: PlayerId, subtype: &str) {
            // Synthesize a Realmwalker permanent on the battlefield carrying
            // the `TopOfLibraryCastPermission` static; affected filter is
            // creature spells of the chosen creature type (modeled via a
            // bare subtype for the test rather than the full chosen-type
            // mechanism, which would require Choose plumbing).
            let src = create_object(
                state,
                CardId(8801),
                controller,
                "Realmwalker (test)".to_string(),
                Zone::Battlefield,
            );
            let def = StaticDefinition::new(StaticMode::TopOfLibraryCastPermission {
                play_mode: CardPlayMode::Cast,
                alt_cost: None,
            })
            .affected(TargetFilter::Typed(TypedFilter {
                type_filters: vec![
                    TypeFilter::Creature,
                    TypeFilter::Subtype(subtype.to_string()),
                ],
                controller: None,
                properties: vec![],
            }));
            state
                .objects
                .get_mut(&src)
                .unwrap()
                .static_definitions
                .push(def);
        }

        #[test]
        fn realmwalker_grants_cast_permission_when_top_matches_filter() {
            let mut state = setup_game_at_main_phase();
            install_realmwalker_static(&mut state, PlayerId(0), "Elf");
            let top = put_creature_on_top_of_library(&mut state, PlayerId(0), CardId(900));

            let available = spell_objects_available_to_cast(&state, PlayerId(0));
            assert!(
                available.contains(&top),
                "Realmwalker must surface a matching top-of-library creature for cast"
            );

            // The card must remain in the library — no exile step.
            assert_eq!(
                state.objects.get(&top).unwrap().zone,
                Zone::Library,
                "permission grant must not exile the card (impulse-draw regression check)"
            );
        }

        #[test]
        fn realmwalker_rejects_top_when_filter_misses() {
            let mut state = setup_game_at_main_phase();
            install_realmwalker_static(&mut state, PlayerId(0), "Goblin");
            // Top of library is an Elf — fails the chosen-type filter.
            let top = put_creature_on_top_of_library(&mut state, PlayerId(0), CardId(901));

            let available = spell_objects_available_to_cast(&state, PlayerId(0));
            assert!(
                !available.contains(&top),
                "filter mismatch must NOT surface the top card as castable"
            );
        }

        /// Filter is re-evaluated each call: changing the top card to a non-
        /// matching one revokes availability without any state mutation on
        /// the static itself.
        #[test]
        fn realmwalker_filter_reevaluates_per_priority_window() {
            let mut state = setup_game_at_main_phase();
            install_realmwalker_static(&mut state, PlayerId(0), "Elf");
            let top_elf = put_creature_on_top_of_library(&mut state, PlayerId(0), CardId(902));
            assert!(spell_objects_available_to_cast(&state, PlayerId(0)).contains(&top_elf));

            // Mutate the object's subtypes so it no longer matches.
            state.objects.get_mut(&top_elf).unwrap().card_types.subtypes =
                vec!["Goblin".to_string()];
            assert!(
                !spell_objects_available_to_cast(&state, PlayerId(0)).contains(&top_elf),
                "stale filter result must not be cached"
            );
        }

        /// Negative regression — a battlefield card with no
        /// `TopOfLibraryCastPermission` static must not surface the top of
        /// library as castable.
        #[test]
        fn no_static_means_no_top_of_library_cast() {
            let mut state = setup_game_at_main_phase();
            let top = put_creature_on_top_of_library(&mut state, PlayerId(0), CardId(903));
            let available = spell_objects_available_to_cast(&state, PlayerId(0));
            assert!(!available.contains(&top));
        }
    }
}
