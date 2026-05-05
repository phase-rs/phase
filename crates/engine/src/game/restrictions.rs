use crate::game::game_object::GameObject;
use crate::types::ability::{
    AbilityCost, AbilityDefinition, ActivationRestriction, CastingRestriction, ParsedCondition,
    QuantityExpr, SpellCastingOptionKind,
};
use crate::types::card_type::{CoreType, Supertype};
use crate::types::counter::{CounterMatch, CounterType};
use crate::types::game_state::CastingVariant;
use crate::types::keywords::Keyword;
use crate::types::mana::ManaCost;
use crate::types::phase::Phase;
use crate::types::player::PlayerId;
use crate::types::zones::Zone;
use crate::types::SpellCastRecord;

use super::engine::EngineError;
use crate::types::identifiers::ObjectId;

/// CR 601.3: A player can begin to cast a spell only if a rule or effect allows that player
/// to cast it and no rule or effect prohibits that player from casting it.
pub fn check_spell_timing(
    state: &crate::types::game_state::GameState,
    player: PlayerId,
    obj: &GameObject,
    ability_def: Option<&AbilityDefinition>,
    allow_flash_timing: bool,
    casting_variant: CastingVariant,
) -> Result<(), EngineError> {
    // CR 702.94a + CR 608.2g / CR 702.35a: Miracle and Madness casts happen
    // during triggered ability resolution, so timing restrictions do not apply.
    if matches!(
        casting_variant,
        CastingVariant::Miracle | CastingVariant::Madness
    ) {
        return Ok(());
    }

    // CR 702.190a: Sneak alt-cost has its own timing rule — the spell is
    // castable any time its controller could cast an instant, but ONLY during
    // the declare-blockers step. This overrides both sorcery-speed and
    // instant-speed checks.
    if matches!(casting_variant, CastingVariant::Sneak { .. }) {
        if state.phase != Phase::DeclareBlockers {
            return Err(EngineError::ActionNotAllowed(
                "Sneak-cast is legal only during the declare-blockers step".to_string(),
            ));
        }
        return Ok(());
    }

    // CR 601.3b: If an effect allows a player to cast a spell as though it had flash,
    // that player may begin to cast it at instant speed.
    // CR 702.8a: Flash allows the spell to be cast any time the player could cast an instant.
    let is_instant_speed = allow_flash_timing
        || obj.card_types.core_types.contains(&CoreType::Instant)
        || obj.has_keyword(&Keyword::Flash);

    // CR 307.1 / CR 116.1: Sorcery-speed spells can only be cast during controller's main phase with empty stack.
    // Permanent spells with no spell ability (ability_def is None) are still sorcery-speed.
    let is_spell_kind = ability_def
        .map(|a| a.kind == crate::types::ability::AbilityKind::Spell)
        .unwrap_or(true);
    if !is_instant_speed && is_spell_kind {
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

    Ok(())
}

/// CR 601.3c: If an effect allows a player to cast a spell as though it had flash only if
/// an alternative or additional cost is paid, that player may begin to cast that spell.
pub fn flash_timing_cost(
    state: &crate::types::game_state::GameState,
    player: PlayerId,
    obj: &GameObject,
) -> Option<ManaCost> {
    obj.casting_options.iter().find_map(|option| {
        if option.kind != SpellCastingOptionKind::AsThoughHadFlash {
            return None;
        }
        if option
            .condition
            .as_ref()
            .is_some_and(|condition| !evaluate_condition(state, player, obj.id, condition))
        {
            return None;
        }
        match &option.cost {
            None => Some(ManaCost::NoCost),
            Some(AbilityCost::Mana { cost }) => Some(cost.clone()),
            _ => None,
        }
    })
}

pub fn add_mana_cost(base: &ManaCost, extra: &ManaCost) -> ManaCost {
    match (base, extra) {
        (ManaCost::NoCost, other) | (ManaCost::SelfManaCost, other) => other.clone(),
        (other, ManaCost::NoCost) | (other, ManaCost::SelfManaCost) => other.clone(),
        (
            ManaCost::Cost {
                shards: base_shards,
                generic: base_generic,
            },
            ManaCost::Cost {
                shards: extra_shards,
                generic: extra_generic,
            },
        ) => {
            let mut shards = base_shards.clone();
            shards.extend(extra_shards.clone());
            ManaCost::Cost {
                shards,
                generic: base_generic + extra_generic,
            }
        }
    }
}

/// CR 601.2i: Once the steps of casting a spell are complete, the spell becomes cast.
/// Records per-player and per-turn spell casting history for restriction checking.
pub fn record_spell_cast(
    state: &mut crate::types::game_state::GameState,
    player: PlayerId,
    obj: &GameObject,
) {
    state.spells_cast_this_turn = state.spells_cast_this_turn.saturating_add(1);
    *state.spells_cast_this_game.entry(player).or_insert(0) += 1;
    // CR 117.1: Record spell characteristics for general-purpose filtered counting.
    state
        .spells_cast_this_turn_by_player
        .entry(player)
        .or_default()
        .push(SpellCastRecord {
            core_types: obj.card_types.core_types.clone(),
            supertypes: obj.card_types.supertypes.clone(),
            subtypes: obj.card_types.subtypes.clone(),
            keywords: obj.keywords.clone(),
            colors: obj.color.clone(),
            mana_value: obj.mana_cost.mana_value(),
            // CR 107.3 + CR 601.2b: Capture X-in-cost at record time so later
            // trigger-filter evaluation (e.g. "your first spell with {X} in its
            // mana cost each turn") does not need to re-examine the spell object.
            has_x_in_cost: crate::game::casting_costs::cost_has_x(&obj.mana_cost),
        });
}

/// CR 508.1m: Any abilities that trigger on attackers being declared trigger.
/// Records per-turn attack history for restriction checking.
pub fn record_attackers_declared(
    state: &mut crate::types::game_state::GameState,
    attacker_count: usize,
) {
    if attacker_count == 0 {
        return;
    }

    state.players_attacked_this_turn.insert(state.active_player);
    *state
        .attacking_creatures_this_turn
        .entry(state.active_player)
        .or_insert(0) += attacker_count as u32;
}

pub fn record_discard(state: &mut crate::types::game_state::GameState, player: PlayerId) {
    state.players_who_discarded_card_this_turn.insert(player);
}

pub fn record_token_created(state: &mut crate::types::game_state::GameState, object_id: ObjectId) {
    if let Some(obj) = state.objects.get(&object_id) {
        state
            .players_who_created_token_this_turn
            .insert(obj.controller);
    }
}

pub fn record_sacrifice(
    state: &mut crate::types::game_state::GameState,
    object_id: ObjectId,
    player: PlayerId,
) {
    if state
        .objects
        .get(&object_id)
        .is_some_and(|obj| obj.card_types.core_types.contains(&CoreType::Artifact))
    {
        state
            .players_who_sacrificed_artifact_this_turn
            .insert(player);
    }
}

/// CR 403.3: Record a battlefield entry snapshot for data-driven ETB condition queries.
pub fn record_battlefield_entry(
    state: &mut crate::types::game_state::GameState,
    object_id: ObjectId,
) {
    let Some(obj) = state.objects.get(&object_id) else {
        return;
    };
    if obj.zone != Zone::Battlefield {
        return;
    }

    let record = crate::types::game_state::BattlefieldEntryRecord {
        object_id,
        name: obj.name.clone(),
        core_types: obj.card_types.core_types.clone(),
        subtypes: obj.card_types.subtypes.clone(),
        supertypes: obj.card_types.supertypes.clone(),
        controller: obj.controller,
    };
    state.battlefield_entries_this_turn.push(record);
}

/// CR 400.7: Record a zone-change snapshot for data-driven condition queries.
pub fn record_zone_change(
    state: &mut crate::types::game_state::GameState,
    record: crate::types::game_state::ZoneChangeRecord,
) {
    let object_id = record.object_id;
    let to_zone = record.to_zone;
    state.zone_changes_this_turn.push(record);

    if to_zone == Zone::Battlefield {
        record_battlefield_entry(state, object_id);
    }
}

/// CR 601.3: Verify casting restrictions are satisfied before allowing a spell to be cast.
pub fn check_casting_restrictions(
    state: &crate::types::game_state::GameState,
    player: PlayerId,
    source_id: ObjectId,
    restrictions: &[CastingRestriction],
) -> Result<(), EngineError> {
    for restriction in restrictions {
        if !casting_restriction_applies(state, player, source_id, restriction) {
            return Err(EngineError::ActionNotAllowed(format!(
                "Casting restriction not satisfied: {restriction:?}"
            )));
        }
    }

    Ok(())
}

/// CR 602.5: A player can't begin to activate an ability that's prohibited from being activated.
pub fn check_activation_restrictions(
    state: &crate::types::game_state::GameState,
    player: PlayerId,
    source_id: ObjectId,
    ability_index: usize,
    restrictions: &[ActivationRestriction],
) -> Result<(), EngineError> {
    for restriction in restrictions {
        if !activation_restriction_applies(state, player, source_id, ability_index, restriction) {
            return Err(EngineError::ActionNotAllowed(format!(
                "Activation restriction not satisfied: {restriction:?}"
            )));
        }
    }

    Ok(())
}

/// CR 302.6 + CR 602.5a: A creature's activated ability with the tap symbol ({T}) or
/// untap symbol ({Q}) in its activation cost can't be activated unless the creature has
/// been under its controller's control continuously since their most recent turn began.
/// Creatures with haste (CR 702.10c) are exempt.
///
/// This is a universal rule applied to every activated ability whose cost contains Tap
/// or Untap, regardless of Oracle text — it is not an `ActivationRestriction` variant
/// because it is not derivable from printed text. Delegates the summoning-sickness
/// determination to the canonical `combat::has_summoning_sickness` helper.
///
/// Non-creature permanents with tap costs (e.g., Sensei's Divining Top) are unaffected:
/// `combat::has_summoning_sickness` returns false for non-creatures, matching the
/// wording "A creature's activated ability…". Animated permanents that are currently
/// creatures are correctly subject to the rule because the check reads the current
/// `GameObject::card_types` after layer evaluation.
pub(crate) fn check_summoning_sickness_for_cost(
    _state: &crate::types::game_state::GameState,
    source: &GameObject,
    cost: &AbilityCost,
) -> Result<(), EngineError> {
    if !cost_contains_tap_or_untap(cost) {
        return Ok(());
    }
    if super::combat::has_summoning_sickness(source) {
        return Err(EngineError::ActionNotAllowed(
            "Creature has summoning sickness: activated abilities with {T} or {Q} \
             can't be activated this turn (CR 302.6)"
                .to_string(),
        ));
    }
    Ok(())
}

/// Recursively inspects an `AbilityCost` for a `Tap` or `Untap` component, descending
/// into `Composite` costs. Used exclusively by `check_summoning_sickness_for_cost` to
/// gate the CR 302.6 check — no other caller should need to enumerate cost components.
fn cost_contains_tap_or_untap(cost: &AbilityCost) -> bool {
    match cost {
        AbilityCost::Tap | AbilityCost::Untap => true,
        AbilityCost::Composite { costs } => costs.iter().any(cost_contains_tap_or_untap),
        _ => false,
    }
}

/// CR 602.5b: If an activated ability has a restriction on its use (e.g., "Activate only once
/// each turn"), the restriction continues to apply even if its controller changes.
pub fn record_ability_activation(
    state: &mut crate::types::game_state::GameState,
    source_id: ObjectId,
    ability_index: usize,
) {
    let key = (source_id, ability_index);
    *state.activated_abilities_this_turn.entry(key).or_insert(0) += 1;
    *state.activated_abilities_this_game.entry(key).or_insert(0) += 1;
}

fn activation_restriction_applies(
    state: &crate::types::game_state::GameState,
    player: PlayerId,
    source_id: ObjectId,
    ability_index: usize,
    restriction: &ActivationRestriction,
) -> bool {
    let key = (source_id, ability_index);

    match restriction {
        // CR 602.5d: "Activate only as a sorcery" means the player must follow sorcery timing rules.
        ActivationRestriction::AsSorcery => is_sorcery_speed_window(state, player),
        ActivationRestriction::AsInstant => true,
        // CR 702.62a: "If you could begin to cast this card by putting it onto the
        // stack from your hand" — defer to the underlying card type's natural
        // cast timing. Instants activate any time priority is held; sorceries
        // (and other non-instant card types) require the sorcery-speed window.
        // Used by Suspend's hand-activated ability so future
        // cast-timing-mirroring activations (Foretell, etc.) reuse this primitive.
        ActivationRestriction::MatchesCardCastTiming => state
            .objects
            .get(&source_id)
            .map(|obj| {
                if obj.card_types.core_types.contains(&CoreType::Instant) {
                    true
                } else {
                    is_sorcery_speed_window(state, player)
                }
            })
            .unwrap_or(false),
        ActivationRestriction::DuringYourTurn => state.active_player == player,
        ActivationRestriction::DuringYourUpkeep => {
            state.active_player == player && state.phase == Phase::Upkeep
        }
        // CR 508.1c / CR 509.1b: Combat-phase restrictions on activation timing.
        ActivationRestriction::DuringCombat => is_combat_phase(state.phase),
        ActivationRestriction::BeforeAttackersDeclared => is_before_attackers_declared(state),
        ActivationRestriction::BeforeCombatDamage => is_before_combat_damage(state.phase),
        // CR 602.5b: Per-turn activation limit tracked via ability activation counter.
        ActivationRestriction::OnlyOnceEachTurn => {
            state
                .activated_abilities_this_turn
                .get(&key)
                .copied()
                .unwrap_or(0)
                == 0
        }
        // CR 602.5b: Per-game activation limit.
        ActivationRestriction::OnlyOnce => {
            state
                .activated_abilities_this_game
                .get(&key)
                .copied()
                .unwrap_or(0)
                == 0
        }
        // CR 602.5b: Per-turn activation count limit (e.g. "Activate only twice each turn").
        ActivationRestriction::MaxTimesEachTurn { count } => {
            state
                .activated_abilities_this_turn
                .get(&key)
                .copied()
                .unwrap_or(0)
                < u32::from(*count)
        }
        ActivationRestriction::RequiresCondition { condition } => condition
            .as_ref()
            .is_none_or(|cond| evaluate_condition(state, player, source_id, cond)),
        // CR 719.3c: Only activatable while the source Case is solved.
        ActivationRestriction::IsSolved => state
            .objects
            .get(&source_id)
            .and_then(|obj| obj.case_state.as_ref())
            .is_some_and(|cs| cs.is_solved),
        // CR 716.4: Level N+1 ability can only activate when Class is at level N.
        ActivationRestriction::ClassLevelIs { level } => state
            .objects
            .get(&source_id)
            .and_then(|obj| obj.class_level)
            .is_some_and(|current| current == *level),
        // CR 711.2a + CR 711.2b: Leveler counter range — activatable when source has
        // level counters in the specified range [minimum, maximum] (or >= minimum if unbounded).
        ActivationRestriction::LevelCounterRange { minimum, maximum } => {
            let level_counter = CounterType::Generic("level".to_string());
            let count = state
                .objects
                .get(&source_id)
                .and_then(|obj| obj.counters.get(&level_counter))
                .copied()
                .unwrap_or(0);
            count >= *minimum && maximum.is_none_or(|max| count <= max)
        }
        // CR 721.2a: "{N+}[abilities]" gate — activatable when the source has `minimum`
        // (and at most `maximum`, if specified) counters matching `counters`.
        // `CounterMatch::Any` sums across every counter type on the source;
        // `OfType(ct)` reads a single type. Mirrors `StaticCondition::HasCounters`
        // evaluation in `layers.rs` and `TriggerCondition::HasCounters` in `triggers.rs`.
        ActivationRestriction::CounterThreshold {
            counters,
            minimum,
            maximum,
        } => {
            let count: u32 = state
                .objects
                .get(&source_id)
                .map(|obj| match counters {
                    CounterMatch::Any => obj.counters.values().sum(),
                    CounterMatch::OfType(ct) => obj.counters.get(ct).copied().unwrap_or(0),
                })
                .unwrap_or(0);
            count >= *minimum && maximum.is_none_or(|max| count <= max)
        }
    }
}

/// CR 601.3: Evaluate individual casting restrictions against the current game state.
fn casting_restriction_applies(
    state: &crate::types::game_state::GameState,
    player: PlayerId,
    source_id: ObjectId,
    restriction: &CastingRestriction,
) -> bool {
    match restriction {
        // CR 307.1: A player may cast a sorcery during a main phase of their turn when the stack is empty.
        CastingRestriction::AsSorcery => is_sorcery_speed_window(state, player),
        CastingRestriction::DuringCombat => is_combat_phase(state.phase),
        CastingRestriction::DuringOpponentsTurn => state.active_player != player,
        CastingRestriction::DuringYourTurn => state.active_player == player,
        CastingRestriction::DuringYourUpkeep => {
            state.active_player == player && state.phase == Phase::Upkeep
        }
        CastingRestriction::DuringOpponentsUpkeep => {
            state.active_player != player && state.phase == Phase::Upkeep
        }
        CastingRestriction::DuringAnyUpkeep => state.phase == Phase::Upkeep,
        CastingRestriction::DuringYourEndStep => {
            state.active_player == player && state.phase == Phase::End
        }
        CastingRestriction::DuringOpponentsEndStep => {
            state.active_player != player && state.phase == Phase::End
        }
        // CR 508.1: Declare attackers step.
        CastingRestriction::DeclareAttackersStep => state.phase == Phase::DeclareAttackers,
        // CR 509.1: Declare blockers step.
        CastingRestriction::DeclareBlockersStep => state.phase == Phase::DeclareBlockers,
        CastingRestriction::BeforeAttackersDeclared => is_before_attackers_declared(state),
        CastingRestriction::BeforeBlockersDeclared => {
            matches!(state.phase, Phase::BeginCombat | Phase::DeclareAttackers)
        }
        CastingRestriction::BeforeCombatDamage => is_before_combat_damage(state.phase),
        CastingRestriction::AfterCombat => matches!(
            state.phase,
            Phase::EndCombat | Phase::PostCombatMain | Phase::End | Phase::Cleanup
        ),
        CastingRestriction::RequiresCondition { condition } => condition
            .as_ref()
            .is_none_or(|cond| evaluate_condition(state, player, source_id, cond)),
    }
}

/// Evaluate a parsed restriction condition against the current game state.
/// CR 601.3 / CR 602.5: These conditions gate whether a spell can be cast or ability activated.
pub(crate) fn evaluate_condition(
    state: &crate::types::game_state::GameState,
    player: PlayerId,
    source_id: ObjectId,
    condition: &ParsedCondition,
) -> bool {
    match condition {
        ParsedCondition::SourceInZone { zone } => state
            .objects
            .get(&source_id)
            .is_some_and(|obj| obj.zone == *zone),
        ParsedCondition::SourceIsAttacking => is_source_attacking(state, source_id),
        ParsedCondition::SourceIsAttackingOrBlocking => {
            is_source_attacking(state, source_id) || is_source_blocking(state, source_id)
        }
        ParsedCondition::SourceIsBlocked => is_source_blocked(state, source_id),
        ParsedCondition::SourcePowerAtLeast { minimum } => state
            .objects
            .get(&source_id)
            .and_then(|obj| obj.power)
            .is_some_and(|power| power >= *minimum),
        ParsedCondition::SourceHasCounterAtLeast {
            counter_type,
            count,
        } => {
            state
                .objects
                .get(&source_id)
                .and_then(|obj| obj.counters.get(counter_type))
                .copied()
                .unwrap_or(0)
                >= *count
        }
        ParsedCondition::SourceHasNoCounter { counter_type } => {
            state
                .objects
                .get(&source_id)
                .and_then(|obj| obj.counters.get(counter_type))
                .copied()
                .unwrap_or(0)
                == 0
        }
        // CR 302.6: "Summoning sickness" — a creature can't attack or use {T} abilities
        // unless controlled since start of turn. This condition checks ETB timing.
        ParsedCondition::SourceEnteredThisTurn => state
            .objects
            .get(&source_id)
            .and_then(|obj| obj.entered_battlefield_turn)
            .is_some_and(|turn| turn == state.turn_number),
        ParsedCondition::SourceIsCreature => state
            .objects
            .get(&source_id)
            .is_some_and(|obj| obj.card_types.core_types.contains(&CoreType::Creature)),
        // CR 301.5 + CR 303.4: This condition is meaningful only when the host is
        // an object (Equipment/Aura attached to a permanent). A player host
        // (CR 303.4 + CR 702.5d, Curse cycle) has no `tapped` or core_type, so
        // the predicate is false by construction — `as_object()` filters it out.
        ParsedCondition::SourceUntappedAttachedTo { required_type } => state
            .objects
            .get(&source_id)
            .and_then(|obj| obj.attached_to)
            .and_then(|t| t.as_object())
            .and_then(|attached_to| state.objects.get(&attached_to))
            .is_some_and(|obj| !obj.tapped && obj.card_types.core_types.contains(required_type)),
        ParsedCondition::SourceLacksKeyword { keyword } => state
            .objects
            .get(&source_id)
            .is_some_and(|obj| !obj.has_keyword(keyword)),
        ParsedCondition::SourceIsColor { color } => state
            .objects
            .get(&source_id)
            .is_some_and(|obj| obj.color.contains(color)),
        ParsedCondition::FirstSpellThisGame => {
            state
                .spells_cast_this_game
                .get(&player)
                .copied()
                .unwrap_or(0)
                == 0
        }
        ParsedCondition::OpponentSearchedLibraryThisTurn => state
            .players_who_searched_library_this_turn
            .iter()
            .any(|searched| *searched != player),
        ParsedCondition::BeenAttackedThisStep => state.players_attacked_this_step.contains(&player),
        ParsedCondition::ZoneCardCountAtLeast { zone, count } => {
            player_zone_ids(state, player, *zone).count() >= *count
        }
        ParsedCondition::ZoneCardTypeCountAtLeast { zone, count } => {
            distinct_zone_card_type_count(state, player, *zone) >= *count
        }
        ParsedCondition::ZoneSubtypeCardCountAtLeast {
            zone,
            subtype,
            count,
        } => {
            player_zone_ids(state, player, *zone)
                .filter(|object_id| {
                    state.objects.get(object_id).is_some_and(|obj| {
                        obj.card_types
                            .subtypes
                            .iter()
                            .any(|item| item.eq_ignore_ascii_case(subtype))
                    })
                })
                .count()
                >= *count
        }
        ParsedCondition::OpponentPoisonAtLeast { count } => state
            .players
            .iter()
            .any(|candidate| candidate.id != player && candidate.poison_counters >= *count),
        ParsedCondition::HandSizeExact { count } => player_hand_size(state, player) == *count,
        ParsedCondition::HandSizeOneOf { counts } => {
            counts.contains(&player_hand_size(state, player))
        }
        ParsedCondition::QuantityVsEachOpponent {
            lhs,
            comparator,
            rhs,
        } => {
            let lhs_expr = QuantityExpr::Ref { qty: lhs.clone() };
            let lhs_val =
                crate::game::quantity::resolve_quantity_scoped(state, &lhs_expr, source_id, player);
            state
                .players
                .iter()
                .filter(|candidate| candidate.id != player)
                .all(|candidate| {
                    let rhs_expr = QuantityExpr::Ref { qty: rhs.clone() };
                    let rhs_val = crate::game::quantity::resolve_quantity_scoped(
                        state,
                        &rhs_expr,
                        source_id,
                        candidate.id,
                    );
                    comparator.evaluate(lhs_val, rhs_val)
                })
        }
        ParsedCondition::CreaturesYouControlTotalPowerAtLeast { minimum } => {
            total_power_of_controlled_creatures(state, player) >= *minimum
        }
        ParsedCondition::YouControlLandSubtypeAny { subtypes } => {
            you_control_land_with_any_subtype(state, player, subtypes)
        }
        ParsedCondition::YouControlSubtypeCountAtLeast { subtype, count } => {
            you_control_subtype_count(state, player, subtype, *count)
        }
        ParsedCondition::YouControlCoreTypeCountAtLeast { core_type, count } => {
            controlled_objects_matching_count(state, player, |obj| {
                obj.card_types.core_types.contains(core_type)
            }) >= *count
        }
        ParsedCondition::YouControlColorPermanentCountAtLeast { color, count } => {
            controlled_objects_matching_count(state, player, |obj| obj.color.contains(color))
                >= *count
        }
        ParsedCondition::YouControlSubtypeOrGraveyardCardSubtype { subtype } => {
            you_control_subtype_count(state, player, subtype, 1)
                || graveyard_has_subtype_card(state, player, subtype)
        }
        ParsedCondition::YouControlLegendaryCreature => {
            controlled_objects_matching_count(state, player, |obj| {
                obj.card_types.core_types.contains(&CoreType::Creature)
                    && obj.card_types.supertypes.contains(&Supertype::Legendary)
            }) >= 1
        }
        ParsedCondition::YouControlNamedPlaneswalker { name } => {
            controlled_objects_matching_count(state, player, |obj| {
                obj.card_types.core_types.contains(&CoreType::Planeswalker)
                    && obj.name.contains(name.as_str())
            }) >= 1
        }
        ParsedCondition::YouControlCreatureWithKeyword { keyword } => {
            you_control_creature_with_keyword(state, player, keyword)
        }
        ParsedCondition::YouControlCreatureWithPowerAtLeast { minimum } => {
            controlled_objects_matching_count(state, player, |obj| {
                obj.card_types.core_types.contains(&CoreType::Creature)
                    && obj.power.is_some_and(|power| power >= *minimum)
            }) >= 1
        }
        ParsedCondition::YouControlCreatureWithPt { power, toughness } => {
            controlled_objects_matching_count(state, player, |obj| {
                obj.card_types.core_types.contains(&CoreType::Creature)
                    && obj.power == Some(*power)
                    && obj.toughness == Some(*toughness)
            }) >= 1
        }
        ParsedCondition::YouControlAnotherColorlessCreature => {
            controlled_objects_matching_count(state, player, |obj| {
                obj.id != source_id
                    && obj.card_types.core_types.contains(&CoreType::Creature)
                    && obj.color.is_empty()
            }) >= 1
        }
        ParsedCondition::YouControlSnowPermanentCountAtLeast { count } => {
            controlled_objects_matching_count(state, player, |obj| {
                obj.card_types.supertypes.contains(&Supertype::Snow)
            }) >= *count
        }
        ParsedCondition::YouControlDifferentPowerCreatureCountAtLeast { count } => {
            controlled_creature_power_count(state, player) >= *count
        }
        ParsedCondition::YouControlLandsWithSameNameAtLeast { count } => {
            controlled_land_same_name_count(state, player) >= *count
        }
        ParsedCondition::YouControlNoCreatures => {
            controlled_objects_matching_count(state, player, |obj| {
                obj.card_types.core_types.contains(&CoreType::Creature)
            }) == 0
        }
        ParsedCondition::YouAttackedThisTurn => state.players_attacked_this_turn.contains(&player),
        ParsedCondition::YouAttackedWithAtLeast { count } => {
            state
                .attacking_creatures_this_turn
                .get(&player)
                .copied()
                .unwrap_or(0)
                >= *count
        }
        ParsedCondition::YouCastSpellThisTurn { filter } => state
            .spells_cast_this_turn_by_player
            .get(&player)
            .is_some_and(|spells| {
                spells.iter().any(|record| {
                    filter.as_ref().is_none_or(|filter| {
                        crate::game::filter::spell_record_matches_filter(
                            record,
                            filter,
                            player,
                            &state.all_creature_types,
                        )
                    })
                })
            }),
        ParsedCondition::YouCastNoncreatureSpellThisTurn => state
            .spells_cast_this_turn_by_player
            .get(&player)
            .is_some_and(|spells| {
                spells
                    .iter()
                    .any(|record| !record.core_types.contains(&CoreType::Creature))
            }),
        ParsedCondition::YouCastSpellCountAtLeast { count } => {
            state
                .spells_cast_this_turn_by_player
                .get(&player)
                .map_or(0, |spells| spells.len() as u32)
                >= *count
        }
        ParsedCondition::YouGainedLifeThisTurn => state
            .players
            .iter()
            .find(|candidate| candidate.id == player)
            .is_some_and(|candidate| candidate.life_gained_this_turn > 0),
        ParsedCondition::YouCreatedTokenThisTurn => {
            state.players_who_created_token_this_turn.contains(&player)
        }
        ParsedCondition::YouDiscardedCardThisTurn => {
            state.players_who_discarded_card_this_turn.contains(&player)
        }
        ParsedCondition::YouSacrificedArtifactThisTurn => state
            .players_who_sacrificed_artifact_this_turn
            .contains(&player),
        // CR 700.4: "Dies" = creature moved from battlefield to graveyard.
        ParsedCondition::CreatureDiedThisTurn => state.zone_changes_this_turn.iter().any(|r| {
            r.core_types.contains(&CoreType::Creature)
                && r.from_zone == Some(Zone::Battlefield)
                && r.to_zone == Zone::Graveyard
        }),
        ParsedCondition::YouHadCreatureEnterThisTurn => state
            .battlefield_entries_this_turn
            .iter()
            .any(|r| r.core_types.contains(&CoreType::Creature) && r.controller == player),
        ParsedCondition::YouHadAngelOrBerserkerEnterThisTurn => {
            state.battlefield_entries_this_turn.iter().any(|r| {
                r.core_types.contains(&CoreType::Creature)
                    && r.controller == player
                    && r.subtypes.iter().any(|s| {
                        s.eq_ignore_ascii_case("Angel") || s.eq_ignore_ascii_case("Berserker")
                    })
            })
        }
        ParsedCondition::YouHadArtifactEnterThisTurn => state
            .battlefield_entries_this_turn
            .iter()
            .any(|r| r.core_types.contains(&CoreType::Artifact) && r.controller == player),
        ParsedCondition::CardsLeftYourGraveyardThisTurnAtLeast { count } => {
            state
                .zone_changes_this_turn
                .iter()
                .filter(|r| r.from_zone == Some(Zone::Graveyard) && r.owner == player)
                .count() as u32
                >= *count
        }
        // CR 602.5b: "Activate only if [player condition]" — count matching non-eliminated players.
        ParsedCondition::PlayerCountAtLeast { filter, minimum } => {
            crate::game::quantity::resolve_player_count(state, filter, player, source_id) as usize
                >= *minimum
        }
        // CR 702.131c: The city's blessing is a player designation that effects
        // and restrictions may identify.
        ParsedCondition::HasCityBlessing => state.city_blessing.contains(&player),
        // CR 601.3 / CR 602.5: Compound restriction — all inner conditions must be true.
        ParsedCondition::And { conditions } => conditions
            .iter()
            .all(|c| evaluate_condition(state, player, source_id, c)),
        // CR 601.3 / CR 602.5: Disjunctive restriction — any inner condition must be true.
        ParsedCondition::Or { conditions } => conditions
            .iter()
            .any(|c| evaluate_condition(state, player, source_id, c)),
        // CR 601.3 / CR 602.5: Logical negation — true when the inner condition is false.
        ParsedCondition::Not { condition } => {
            !evaluate_condition(state, player, source_id, condition)
        }
    }
}

/// CR 307.1: Sorcery-speed timing — main phase, stack empty, active player has priority.
pub(crate) fn is_sorcery_speed_window(
    state: &crate::types::game_state::GameState,
    player: PlayerId,
) -> bool {
    matches!(state.phase, Phase::PreCombatMain | Phase::PostCombatMain)
        && state.stack.is_empty()
        && state.active_player == player
}

fn is_before_attackers_declared(state: &crate::types::game_state::GameState) -> bool {
    state.active_player == state.priority_player
        && matches!(state.phase, Phase::PreCombatMain | Phase::BeginCombat)
}

/// CR 506.1: The combat phase has five steps: beginning of combat, declare attackers,
/// declare blockers, combat damage, and end of combat.
fn is_combat_phase(phase: Phase) -> bool {
    matches!(
        phase,
        Phase::BeginCombat
            | Phase::DeclareAttackers
            | Phase::DeclareBlockers
            | Phase::CombatDamage
            | Phase::EndCombat
    )
}

fn is_before_combat_damage(phase: Phase) -> bool {
    matches!(
        phase,
        Phase::BeginCombat | Phase::DeclareAttackers | Phase::DeclareBlockers
    )
}

fn you_control_creature_with_keyword(
    state: &crate::types::game_state::GameState,
    player: PlayerId,
    keyword: &Keyword,
) -> bool {
    controlled_objects_matching_count(state, player, |obj| {
        obj.card_types.core_types.contains(&CoreType::Creature) && obj.has_keyword(keyword)
    }) >= 1
}

fn you_control_land_with_any_subtype(
    state: &crate::types::game_state::GameState,
    player: PlayerId,
    subtypes: &[String],
) -> bool {
    state.battlefield.iter().any(|object_id| {
        state.objects.get(object_id).is_some_and(|obj| {
            obj.controller == player
                && obj.card_types.core_types.contains(&CoreType::Land)
                && obj.card_types.subtypes.iter().any(|subtype| {
                    subtypes
                        .iter()
                        .any(|wanted| wanted == &subtype.to_lowercase())
                })
        })
    })
}

fn you_control_subtype_count(
    state: &crate::types::game_state::GameState,
    player: PlayerId,
    subtype: &str,
    minimum: usize,
) -> bool {
    state
        .battlefield
        .iter()
        .filter(|object_id| {
            state.objects.get(object_id).is_some_and(|obj| {
                obj.controller == player
                    && obj
                        .card_types
                        .subtypes
                        .iter()
                        .any(|candidate| candidate.eq_ignore_ascii_case(subtype))
            })
        })
        .count()
        >= minimum
}

fn controlled_objects_matching_count(
    state: &crate::types::game_state::GameState,
    player: PlayerId,
    predicate: impl Fn(&GameObject) -> bool,
) -> usize {
    state
        .battlefield
        .iter()
        .filter(|object_id| {
            state
                .objects
                .get(object_id)
                .is_some_and(|obj| obj.controller == player && predicate(obj))
        })
        .count()
}

fn controlled_creature_power_count(
    state: &crate::types::game_state::GameState,
    player: PlayerId,
) -> usize {
    let mut powers = std::collections::HashSet::new();
    for object_id in &state.battlefield {
        let Some(obj) = state.objects.get(object_id) else {
            continue;
        };
        if obj.controller != player || !obj.card_types.core_types.contains(&CoreType::Creature) {
            continue;
        }
        if let Some(power) = obj.power {
            powers.insert(power);
        }
    }
    powers.len()
}

fn controlled_land_same_name_count(
    state: &crate::types::game_state::GameState,
    player: PlayerId,
) -> usize {
    let mut counts = std::collections::HashMap::<String, usize>::new();
    for object_id in &state.battlefield {
        let Some(obj) = state.objects.get(object_id) else {
            continue;
        };
        if obj.controller == player && obj.card_types.core_types.contains(&CoreType::Land) {
            *counts.entry(obj.name.clone()).or_insert(0) += 1;
        }
    }
    counts.into_values().max().unwrap_or(0)
}

fn total_power_of_controlled_creatures(
    state: &crate::types::game_state::GameState,
    player: PlayerId,
) -> i32 {
    state
        .battlefield
        .iter()
        .filter_map(|object_id| state.objects.get(object_id))
        .filter(|obj| {
            obj.controller == player && obj.card_types.core_types.contains(&CoreType::Creature)
        })
        .map(|obj| obj.power.unwrap_or(0))
        .sum()
}

fn player_hand_size(state: &crate::types::game_state::GameState, player: PlayerId) -> usize {
    state
        .players
        .iter()
        .find(|candidate| candidate.id == player)
        .map(|candidate| candidate.hand.len())
        .unwrap_or(0)
}

fn player_zone_ids<'a>(
    state: &'a crate::types::game_state::GameState,
    player: PlayerId,
    zone: crate::types::zones::Zone,
) -> Box<dyn Iterator<Item = &'a ObjectId> + 'a> {
    let Some(p) = state
        .players
        .iter()
        .find(|candidate| candidate.id == player)
    else {
        return Box::new(std::iter::empty());
    };
    match zone {
        crate::types::zones::Zone::Graveyard => Box::new(p.graveyard.iter()),
        crate::types::zones::Zone::Hand => Box::new(p.hand.iter()),
        crate::types::zones::Zone::Library => Box::new(p.library.iter()),
        _ => Box::new(std::iter::empty()),
    }
}

fn distinct_zone_card_type_count(
    state: &crate::types::game_state::GameState,
    player: PlayerId,
    zone: crate::types::zones::Zone,
) -> usize {
    let mut card_types = std::collections::HashSet::new();
    for object_id in player_zone_ids(state, player, zone) {
        let Some(obj) = state.objects.get(object_id) else {
            continue;
        };
        for core_type in &obj.card_types.core_types {
            card_types.insert(*core_type);
        }
    }
    card_types.len()
}

fn graveyard_has_subtype_card(
    state: &crate::types::game_state::GameState,
    player: PlayerId,
    subtype: &str,
) -> bool {
    player_zone_ids(state, player, crate::types::zones::Zone::Graveyard).any(|object_id| {
        state.objects.get(object_id).is_some_and(|obj| {
            obj.card_types
                .subtypes
                .iter()
                .any(|candidate| candidate.eq_ignore_ascii_case(subtype))
        })
    })
}

/// CR 508.1k: A chosen creature becomes an attacking creature until removed from combat.
fn is_source_attacking(state: &crate::types::game_state::GameState, source_id: ObjectId) -> bool {
    state.combat.as_ref().is_some_and(|combat| {
        combat
            .attackers
            .iter()
            .any(|attacker| attacker.object_id == source_id)
    })
}

/// CR 509.1g: A chosen creature becomes a blocking creature until removed from combat.
fn is_source_blocking(state: &crate::types::game_state::GameState, source_id: ObjectId) -> bool {
    state
        .combat
        .as_ref()
        .is_some_and(|combat| combat.blocker_to_attacker.contains_key(&source_id))
}

/// CR 509.1h: An attacking creature with blockers declared for it becomes a blocked creature.
fn is_source_blocked(state: &crate::types::game_state::GameState, source_id: ObjectId) -> bool {
    state
        .combat
        .as_ref()
        .and_then(|combat| combat.blocker_assignments.get(&source_id))
        .is_some_and(|blockers| !blockers.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::parser::oracle_condition::parse_restriction_condition;
    use crate::types::ability::{AbilityKind, Effect, ParsedCondition, QuantityExpr};
    use crate::types::card_type::CoreType;
    use crate::types::counter::CounterType;
    use crate::types::game_state::WaitingFor;
    use crate::types::identifiers::CardId;
    use crate::types::zones::Zone;

    /// Two-step pattern: parse condition text, then evaluate.
    /// Returns `true` for unrecognized conditions (matching prior permissive behavior).
    fn parse_and_evaluate_condition(
        state: &crate::types::game_state::GameState,
        player: PlayerId,
        source_id: ObjectId,
        text: &str,
    ) -> bool {
        match parse_restriction_condition(text) {
            Some(cond) => evaluate_condition(state, player, source_id, &cond),
            None => true,
        }
    }

    #[test]
    fn activation_once_each_turn_uses_shared_counter() {
        let mut state = crate::types::game_state::GameState::new_two_player(42);
        record_ability_activation(&mut state, ObjectId(10), 1);

        let result = check_activation_restrictions(
            &state,
            PlayerId(0),
            ObjectId(10),
            1,
            &[ActivationRestriction::OnlyOnceEachTurn],
        );

        assert!(result.is_err());
    }

    #[test]
    fn city_blessing_restriction_checks_player_designation() {
        let mut state = crate::types::game_state::GameState::new_two_player(42);
        let player = PlayerId(0);
        let source_id = ObjectId(10);
        let condition = ParsedCondition::HasCityBlessing;

        assert!(!evaluate_condition(&state, player, source_id, &condition));
        state.city_blessing.insert(player);
        assert!(evaluate_condition(&state, player, source_id, &condition));
    }

    #[test]
    fn evaluates_you_control_creature_with_flying_condition() {
        let mut state = crate::types::game_state::GameState::new_two_player(42);
        let bird = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Bird".to_string(),
            Zone::Battlefield,
        );
        let bird_obj = state.objects.get_mut(&bird).unwrap();
        bird_obj.card_types.core_types.push(CoreType::Creature);
        bird_obj.keywords.push(Keyword::Flying);

        assert!(parse_and_evaluate_condition(
            &state,
            PlayerId(0),
            bird,
            "you control a creature with flying"
        ));
    }

    #[test]
    fn evaluates_you_control_two_or_more_vampires_condition() {
        let mut state = crate::types::game_state::GameState::new_two_player(42);
        for card_id in 1..=2 {
            let vampire = create_object(
                &mut state,
                CardId(card_id),
                PlayerId(0),
                format!("Vampire {card_id}"),
                Zone::Battlefield,
            );
            let obj = state.objects.get_mut(&vampire).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.card_types.subtypes.push("Vampire".to_string());
        }

        assert!(parse_and_evaluate_condition(
            &state,
            PlayerId(0),
            ObjectId(1),
            "you control two or more vampires"
        ));
    }

    #[test]
    fn evaluates_opponent_searched_library_this_turn_condition() {
        let mut state = crate::types::game_state::GameState::new_two_player(42);
        state
            .players_who_searched_library_this_turn
            .insert(PlayerId(1));

        assert!(parse_and_evaluate_condition(
            &state,
            PlayerId(0),
            ObjectId(1),
            "an opponent searched their library this turn"
        ));
    }

    #[test]
    fn evaluates_you_attacked_with_two_or_more_creatures_this_turn_condition() {
        let mut state = crate::types::game_state::GameState::new_two_player(42);
        state.players_attacked_this_turn.insert(PlayerId(0));
        state.attacking_creatures_this_turn.insert(PlayerId(0), 2);

        assert!(parse_and_evaluate_condition(
            &state,
            PlayerId(0),
            ObjectId(1),
            "you attacked with two or more creatures this turn"
        ));
    }

    #[test]
    fn zero_attacker_declaration_does_not_satisfy_you_attacked_this_turn() {
        let mut state = crate::types::game_state::GameState::new_two_player(42);
        state.active_player = PlayerId(0);

        record_attackers_declared(&mut state, 0);

        assert!(!parse_and_evaluate_condition(
            &state,
            PlayerId(0),
            ObjectId(1),
            "you attacked this turn"
        ));

        record_attackers_declared(&mut state, 1);

        assert!(parse_and_evaluate_condition(
            &state,
            PlayerId(0),
            ObjectId(1),
            "you attacked this turn"
        ));
    }

    #[test]
    fn evaluates_creatures_you_control_total_power_condition() {
        let mut state = crate::types::game_state::GameState::new_two_player(42);
        for (card_id, power) in [(1, 3), (2, 5)] {
            let creature = create_object(
                &mut state,
                CardId(card_id),
                PlayerId(0),
                format!("Creature {card_id}"),
                Zone::Battlefield,
            );
            let obj = state.objects.get_mut(&creature).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.power = Some(power);
        }

        assert!(parse_and_evaluate_condition(
            &state,
            PlayerId(0),
            ObjectId(1),
            "creatures you control have total power 8 or greater"
        ));
    }

    #[test]
    fn evaluates_graveyard_card_count_condition() {
        let mut state = crate::types::game_state::GameState::new_two_player(42);
        for card_id in 1..=7 {
            create_object(
                &mut state,
                CardId(card_id),
                PlayerId(0),
                format!("Card {card_id}"),
                Zone::Graveyard,
            );
        }

        assert!(parse_and_evaluate_condition(
            &state,
            PlayerId(0),
            ObjectId(1),
            "there are seven or more cards in your graveyard"
        ));
    }

    #[test]
    fn evaluates_you_control_three_or_more_artifacts_condition() {
        let mut state = crate::types::game_state::GameState::new_two_player(42);
        for card_id in 1..=3 {
            let artifact = create_object(
                &mut state,
                CardId(card_id),
                PlayerId(0),
                format!("Artifact {card_id}"),
                Zone::Battlefield,
            );
            state
                .objects
                .get_mut(&artifact)
                .unwrap()
                .card_types
                .core_types
                .push(CoreType::Artifact);
        }

        assert!(parse_and_evaluate_condition(
            &state,
            PlayerId(0),
            ObjectId(1),
            "you control three or more artifacts"
        ));
    }

    #[test]
    fn evaluates_hand_size_choice_condition() {
        let mut state = crate::types::game_state::GameState::new_two_player(42);
        for card_id in 1..=7 {
            create_object(
                &mut state,
                CardId(card_id),
                PlayerId(0),
                format!("Card {card_id}"),
                Zone::Hand,
            );
        }

        assert!(parse_and_evaluate_condition(
            &state,
            PlayerId(0),
            ObjectId(1),
            "you have exactly zero or seven cards in hand"
        ));
    }

    #[test]
    fn evaluates_creature_died_this_turn_condition() {
        let mut state = crate::types::game_state::GameState::new_two_player(42);
        state
            .zone_changes_this_turn
            .push(crate::types::game_state::ZoneChangeRecord {
                name: "Grizzly Bears".to_string(),
                core_types: vec![CoreType::Creature],
                ..crate::types::game_state::ZoneChangeRecord::test_minimal(
                    ObjectId(99),
                    Some(Zone::Battlefield),
                    Zone::Graveyard,
                )
            });

        assert!(parse_and_evaluate_condition(
            &state,
            PlayerId(0),
            ObjectId(1),
            "a creature died this turn"
        ));
    }

    #[test]
    fn evaluates_cast_instant_or_sorcery_this_turn_condition() {
        let mut state = crate::types::game_state::GameState::new_two_player(42);
        state.spells_cast_this_turn_by_player.insert(
            PlayerId(0),
            vec![crate::types::game_state::SpellCastRecord {
                core_types: vec![CoreType::Instant],
                supertypes: Vec::new(),
                subtypes: Vec::new(),
                keywords: Vec::new(),
                colors: Vec::new(),
                mana_value: 1,
                has_x_in_cost: false,
            }],
        );

        assert!(parse_and_evaluate_condition(
            &state,
            PlayerId(0),
            ObjectId(1),
            "you've cast an instant or sorcery spell this turn"
        ));
        assert!(!parse_and_evaluate_condition(
            &state,
            PlayerId(1),
            ObjectId(1),
            "you've cast an instant or sorcery spell this turn"
        ));
    }

    #[test]
    fn evaluates_artifact_entered_this_turn_condition() {
        let mut state = crate::types::game_state::GameState::new_two_player(42);
        let artifact = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Relic".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&artifact)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Artifact);
        record_battlefield_entry(&mut state, artifact);

        assert!(parse_and_evaluate_condition(
            &state,
            PlayerId(0),
            artifact,
            "this artifact or another artifact entered the battlefield under your control this turn"
        ));
    }

    #[test]
    fn evaluates_cards_left_graveyard_this_turn_condition() {
        let mut state = crate::types::game_state::GameState::new_two_player(42);
        // Push 3 zone-change records for cards leaving the graveyard.
        for i in 0..3 {
            state
                .zone_changes_this_turn
                .push(crate::types::game_state::ZoneChangeRecord {
                    name: format!("Card {}", i),
                    ..crate::types::game_state::ZoneChangeRecord::test_minimal(
                        ObjectId(100 + i),
                        Some(Zone::Graveyard),
                        Zone::Exile,
                    )
                });
        }

        assert!(parse_and_evaluate_condition(
            &state,
            PlayerId(0),
            ObjectId(1),
            "three or more cards left your graveyard this turn"
        ));
    }

    #[test]
    fn evaluates_source_counter_condition() {
        let mut state = crate::types::game_state::GameState::new_two_player(42);
        let artifact = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Oil Vessel".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&artifact).unwrap();
        obj.card_types.core_types.push(CoreType::Artifact);
        obj.counters
            .insert(CounterType::Generic("oil".to_string()), 2);

        assert!(parse_and_evaluate_condition(
            &state,
            PlayerId(0),
            artifact,
            "this artifact has two or more oil counters on it"
        ));
    }

    #[test]
    fn spell_timing_allows_flash_override() {
        let mut state = crate::types::game_state::GameState::new_two_player(42);
        state.phase = Phase::End;
        state.active_player = PlayerId(1);
        state.waiting_for = WaitingFor::Priority {
            player: PlayerId(0),
        };

        let mut obj = GameObject::new(
            ObjectId(10),
            CardId(10),
            PlayerId(0),
            "Sorcery".to_string(),
            Zone::Hand,
        );
        obj.card_types.core_types.push(CoreType::Sorcery);
        let ability = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: crate::types::ability::TargetFilter::Controller,
            },
        );

        assert!(check_spell_timing(
            &state,
            PlayerId(0),
            &obj,
            Some(&ability),
            true,
            CastingVariant::Normal
        )
        .is_ok());
    }
}
