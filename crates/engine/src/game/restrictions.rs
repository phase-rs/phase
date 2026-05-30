use crate::game::game_object::GameObject;
use crate::types::ability::{
    AbilityCost, AbilityDefinition, AbilityTag, ActivationRestriction, CastingRestriction,
    ControllerRef, FilterProp, ParsedCondition, QuantityExpr, SpellCastingOptionKind, TargetFilter,
    TypeFilter,
};
use crate::types::card_type::{CoreType, Supertype};
use crate::types::counter::{CounterMatch, CounterType};
use crate::types::game_state::{BattlefieldEntryRecord, CastingVariant};
use crate::types::keywords::Keyword;
use crate::types::mana::{ManaColor, ManaCost};
use crate::types::phase::Phase;
use crate::types::player::PlayerId;
use crate::types::statics::StaticMode;
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
            Some(cost) if cost.is_payable(state, player, obj.id) => Some(ManaCost::NoCost),
            Some(_) => None,
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
/// CR 601.2a: Every cast spell has a from-zone, but the broader `GameObject`
/// surface (`obj.cast_from_zone`) carries an `Option<Zone>` because non-cast
/// objects (tokens, emblems) lack one. Tests that exercise this helper without
/// having gone through the cast pipeline default the missing zone to
/// `Zone::Hand` — the canonical fallback used elsewhere by `SpellCastRecord`.
pub fn record_spell_cast(
    state: &mut crate::types::game_state::GameState,
    player: PlayerId,
    obj: &GameObject,
    cast_variant: crate::types::game_state::CastingVariant,
) {
    record_spell_cast_from_zone(
        state,
        player,
        obj,
        obj.cast_from_zone.unwrap_or(Zone::Hand),
        cast_variant,
    );
}

pub fn record_spell_cast_from_zone(
    state: &mut crate::types::game_state::GameState,
    player: PlayerId,
    obj: &GameObject,
    from_zone: Zone,
    cast_variant: crate::types::game_state::CastingVariant,
) {
    state.spells_cast_this_turn = state.spells_cast_this_turn.saturating_add(1);
    *state.spells_cast_this_game.entry(player).or_insert(0) += 1;
    // CR 117.1: Record spell characteristics for general-purpose filtered counting.
    let record = SpellCastRecord {
        name: obj.name.clone(),
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
        from_zone,
        // CR 702.185c: Capture the alternative-cast variant so per-turn
        // spell-history conditions ("a spell was warped this turn") can
        // resolve after the spell has left the stack.
        cast_variant,
    };
    state
        .spells_cast_this_turn_by_player
        .entry(player)
        .or_default()
        .push_back(record.clone());
    // CR 117.1: Game-scope history mirror — not cleared between turns so
    // "named {LITERAL} this game" conditions (Approach of the Second Sun)
    // can see all prior casts.
    state
        .spells_cast_this_game_by_player
        .entry(player)
        .or_default()
        .push_back(record);
}

/// CR 702.185c: True when any player cast a spell using `variant` this turn.
/// `spells_cast_this_turn_by_player` is turn-scoped (cleared between turns), so
/// this answers "a spell was warped this turn" (and any future "cast via X this
/// turn" query) without inspecting the spell objects, which may have left the
/// stack. Not controller-scoped — every player's history is scanned.
pub fn spell_cast_with_variant_this_turn(
    state: &crate::types::game_state::GameState,
    variant: &crate::types::game_state::CastingVariant,
) -> bool {
    state
        .spells_cast_this_turn_by_player
        .values()
        .flat_map(|records| records.iter())
        .any(|record| &record.cast_variant == variant)
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

    // CR 508.6 + CR 508.5: record the defending players attacked this declaration.
    // `players_attacked_this_step` already holds this declaration's defenders.
    let active = state.active_player;
    state
        .attacked_defenders_this_turn
        .entry(active)
        .or_default()
        .extend(state.players_attacked_this_step.iter().copied());
}

pub fn record_discard(state: &mut crate::types::game_state::GameState, player: PlayerId) {
    state.players_who_discarded_card_this_turn.insert(player);
    *state
        .cards_discarded_this_turn_by_player
        .entry(player)
        .or_insert(0) += 1;
}

pub fn record_token_created(state: &mut crate::types::game_state::GameState, object_id: ObjectId) {
    if let Some(obj) = state.objects.get(&object_id) {
        state
            .players_who_created_token_this_turn
            .insert(obj.controller);
        state
            .created_tokens_this_turn
            .push(obj.snapshot_for_zone_change(object_id, None, Zone::Battlefield));
    }
}

pub fn record_sacrifice(
    state: &mut crate::types::game_state::GameState,
    object_id: ObjectId,
    player: PlayerId,
) {
    let Some(obj) = state.objects.get(&object_id) else {
        return;
    };
    state
        .sacrificed_permanents_this_turn
        .push(obj.snapshot_for_zone_change(object_id, Some(Zone::Battlefield), Zone::Graveyard));
    if obj.card_types.core_types.contains(&CoreType::Artifact) {
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
        colors: obj.color.clone(),
        controller: obj.controller,
    };
    state.battlefield_entries_this_turn.push(record);
}

fn entry_controller_matches(
    controller: &ControllerRef,
    record_controller: PlayerId,
    player: PlayerId,
) -> bool {
    match controller {
        ControllerRef::You => record_controller == player,
        ControllerRef::Opponent => record_controller != player,
        _ => false,
    }
}

fn entry_type_filter_matches(record: &BattlefieldEntryRecord, type_filter: &TypeFilter) -> bool {
    match type_filter {
        TypeFilter::Creature => record.core_types.contains(&CoreType::Creature),
        TypeFilter::Land => record.core_types.contains(&CoreType::Land),
        TypeFilter::Artifact => record.core_types.contains(&CoreType::Artifact),
        TypeFilter::Enchantment => record.core_types.contains(&CoreType::Enchantment),
        TypeFilter::Planeswalker => record.core_types.contains(&CoreType::Planeswalker),
        TypeFilter::Battle => record.core_types.contains(&CoreType::Battle),
        TypeFilter::Permanent => record.core_types.iter().any(|core| {
            matches!(
                core,
                CoreType::Artifact
                    | CoreType::Battle
                    | CoreType::Creature
                    | CoreType::Enchantment
                    | CoreType::Land
                    | CoreType::Planeswalker
            )
        }),
        TypeFilter::Card | TypeFilter::Any => true,
        TypeFilter::Non(inner) => !entry_type_filter_matches(record, inner),
        TypeFilter::Subtype(subtype) => record
            .subtypes
            .iter()
            .any(|record_subtype| record_subtype.eq_ignore_ascii_case(subtype)),
        TypeFilter::AnyOf(filters) => filters
            .iter()
            .any(|inner| entry_type_filter_matches(record, inner)),
        _ => false,
    }
}

fn entry_color_matches(record: &BattlefieldEntryRecord, color: &ManaColor) -> bool {
    record.colors.iter().any(|entry_color| entry_color == color)
}

fn battlefield_entry_matches_filter(
    record: &BattlefieldEntryRecord,
    filter: &TargetFilter,
    player: PlayerId,
) -> bool {
    match filter {
        TargetFilter::Any => true,
        TargetFilter::Typed(typed) => {
            if let Some(controller) = &typed.controller {
                if !entry_controller_matches(controller, record.controller, player) {
                    return false;
                }
            }
            if !typed
                .type_filters
                .iter()
                .all(|type_filter| entry_type_filter_matches(record, type_filter))
            {
                return false;
            }
            typed.properties.iter().all(|prop| match prop {
                FilterProp::HasColor { color } => entry_color_matches(record, color),
                FilterProp::InZone { zone } => *zone == Zone::Battlefield,
                _ => false,
            })
        }
        _ => false,
    }
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
/// determination to `summoning_sick_for_tap_ability`, which consults both the
/// `combat::has_summoning_sickness` base rule and the
/// `StaticMode::CanActivateAbilitiesAsThoughHaste` bypass (Tyvar, Jubilant Brawler).
///
/// Non-creature permanents with tap costs (e.g., Sensei's Divining Top) are unaffected:
/// `combat::has_summoning_sickness` returns false for non-creatures, matching the
/// wording "A creature's activated ability…". Animated permanents that are currently
/// creatures are correctly subject to the rule because the check reads the current
/// `GameObject::card_types` after layer evaluation.
pub(crate) fn check_summoning_sickness_for_cost(
    state: &crate::types::game_state::GameState,
    source: &GameObject,
    cost: &AbilityCost,
) -> Result<(), EngineError> {
    if !cost_contains_tap_or_untap(cost) {
        return Ok(());
    }
    if summoning_sick_for_tap_ability(state, source) {
        return Err(EngineError::ActionNotAllowed(
            "Creature has summoning sickness: activated abilities with {T} or {Q} \
             can't be activated this turn (CR 302.6)"
                .to_string(),
        ));
    }
    Ok(())
}

/// CR 602.5a + CR 702.10c: Does `obj` count as summoning-sick for the purpose of
/// activating a `{T}`/`{Q}` ability?
///
/// Returns `false` when `obj` is not summoning-sick at all. Otherwise it is
/// summoning-sick under the base rule, and we return `false` only when a
/// `StaticMode::CanActivateAbilitiesAsThoughHaste` static (Tyvar, Jubilant
/// Brawler) applies to `obj` — that static lifts the CR 602.5a activation gate
/// "as though those creatures had haste". This is the single shared predicate
/// used by both the activation-time check and the mana-source candidate
/// generation, so the bypass is honored uniformly across both paths.
pub(crate) fn summoning_sick_for_tap_ability(
    state: &crate::types::game_state::GameState,
    obj: &GameObject,
) -> bool {
    if !super::combat::has_summoning_sickness(obj) {
        return false;
    }
    !super::static_abilities::check_static_ability(
        state,
        StaticMode::CanActivateAbilitiesAsThoughHaste,
        &super::static_abilities::StaticCheckContext {
            target_id: Some(obj.id),
            ..Default::default()
        },
    )
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

/// CR 702.142b: Compute the effective per-turn activation limit for an ability.
/// Normally `OnlyOnceEachTurn` means limit = 1, but `ModifyActivationLimit` statics
/// can override this for abilities matching a keyword tag (e.g., boast).
fn effective_activation_limit(
    state: &crate::types::game_state::GameState,
    player: PlayerId,
    source_id: ObjectId,
    ability_index: usize,
) -> u32 {
    // Check if the ability at this index has a keyword tag
    let ability_tag = state
        .objects
        .get(&source_id)
        .and_then(|obj| obj.abilities.get(ability_index))
        .and_then(|def| def.ability_tag);
    let Some(tag) = ability_tag else {
        return 1; // No tag → default once-per-turn
    };
    let keyword = match tag {
        AbilityTag::Boast => "boast",
        AbilityTag::Evolve => "evolve",
        AbilityTag::Exhaust => "exhaust",
        AbilityTag::Outlast => "outlast",
    };
    // Scan battlefield for ModifyActivationLimit statics that affect this keyword
    let mut limit: u32 = 1;
    for (bf_obj, static_def) in
        crate::game::functioning_abilities::battlefield_active_statics(state)
    {
        if bf_obj.controller != player {
            continue;
        }
        if let StaticMode::ModifyActivationLimit {
            keyword: ref kw,
            new_limit,
        } = static_def.mode
        {
            if kw == keyword {
                // Check if the source object is affected by this static
                if static_def.affected.as_ref().is_some_and(|filter| {
                    super::filter::matches_target_filter(
                        state,
                        source_id,
                        filter,
                        &super::filter::FilterContext::from_source_with_controller(
                            bf_obj.id,
                            bf_obj.controller,
                        ),
                    )
                }) {
                    limit = limit.max(u32::from(new_limit));
                }
            }
        }
    }
    limit
}

fn has_activate_as_instant_permission(
    state: &crate::types::game_state::GameState,
    player: PlayerId,
    source_id: ObjectId,
    ability_index: usize,
) -> bool {
    let Some(ability) = state
        .objects
        .get(&source_id)
        .and_then(|obj| obj.abilities.get(ability_index))
    else {
        return false;
    };
    let cost_categories = ability.cost_categories();
    if cost_categories.is_empty() {
        return false;
    }

    crate::game::functioning_abilities::battlefield_active_statics(state).any(
        |(static_source, def)| {
            if static_source.controller != player {
                return false;
            }
            let StaticMode::ActivateAsInstant {
                cost_category: permitted_category,
            } = def.mode
            else {
                return false;
            };
            if !cost_categories.contains(&permitted_category) {
                return false;
            }
            def.affected.as_ref().is_some_and(|filter| {
                super::filter::matches_target_filter(
                    state,
                    source_id,
                    filter,
                    &super::filter::FilterContext::from_source_with_controller(
                        static_source.id,
                        static_source.controller,
                    ),
                )
            })
        },
    )
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
        ActivationRestriction::AsSorcery => {
            is_sorcery_speed_window(state, player)
                || has_activate_as_instant_permission(state, player, source_id, ability_index)
        }
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
        // CR 702.142b: ModifyActivationLimit statics may raise the limit for tagged abilities.
        ActivationRestriction::OnlyOnceEachTurn => {
            let current_count = state
                .activated_abilities_this_turn
                .get(&key)
                .copied()
                .unwrap_or(0);
            let limit = effective_activation_limit(state, player, source_id, ability_index);
            current_count < limit
        }
        // CR 602.5b: Per-object activation limit. `zones::move_to_zone` clears
        // this count when CR 400.7 makes the stored id represent a new object.
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
        // CR 702.142a: Boast — "activate only if this creature attacked this turn".
        ParsedCondition::SourceAttackedThisTurn => {
            state.creatures_attacked_this_turn.contains(&source_id)
        }
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
        ParsedCondition::QuantityComparison {
            lhs,
            comparator,
            rhs,
        } => {
            let lhs_val =
                crate::game::quantity::resolve_quantity_scoped(state, lhs, source_id, player);
            let rhs_val =
                crate::game::quantity::resolve_quantity_scoped(state, rhs, source_id, player);
            comparator.evaluate(lhs_val, rhs_val)
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
        // CR 602.5b: honor the parameterized controller scope.
        ParsedCondition::ControlsCreatureWithKeyword {
            controller,
            keyword,
        } => match controller {
            ControllerRef::You => you_control_creature_with_keyword(state, player, keyword),
            _ => opponent_controls_creature_with_keyword(state, player, keyword),
        },
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
        ParsedCondition::YouPlayedLandThisTurn => state
            .players
            .get(usize::from(player.0))
            .is_some_and(|player| player.lands_played_this_turn > 0),
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
        ParsedCondition::BattlefieldEntriesThisTurn { filter, count } => {
            state
                .battlefield_entries_this_turn
                .iter()
                .filter(|record| battlefield_entry_matches_filter(record, filter, player))
                .count() as u32
                >= *count
        }
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
        // CR 102.1: "The active player is the player whose turn it is."
        ParsedCondition::IsYourTurn => state.active_player == player,
        // CR 601.3d + CR 608.2c: "if it targets a [filter]" — gates a casting
        // permission on the chosen targets of the in-flight spell. Read from
        // `state.pending_cast.ability.targets` when targets have been committed.
        // Before target selection (announcement-time check by `flash_timing_cost`),
        // `pending_cast` is `None` for the candidate-generation pass and the
        // committed targets are absent during the cast-announcement check —
        // both cases evaluate to `true` so the cast may be announced and proceed
        // to target selection. Final validation runs at
        // `finish_pending_cast_cost_or_pay` against the now-committed targets,
        // where this same evaluator returns the authoritative answer.
        ParsedCondition::SpellTargetsFilter { filter } => {
            spell_targets_filter(state, source_id, filter)
        }
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

/// CR 601.3d + CR 608.2c: Evaluate `SpellTargetsFilter` against the in-flight
/// spell's chosen targets, if any are committed.
///
/// Returns:
/// - `true` when targets have not yet been chosen (the cast may proceed to
///   target selection; final validation runs at finalize).
/// - `true` when at least one committed object target satisfies `filter`.
/// - `false` only when targets have been chosen AND none of them match.
///
/// Target lookup priority:
/// 1. `state.pending_cast` — set during the mid-cast WaitingFor::TargetSelection
///    and post-target validation gate. Read its `ability.targets`.
/// 2. The top of the stack — once `finalize_cast` has installed the spell with
///    its `ResolvedAbility`, the targets live on the stack entry.
///
/// Final validation in `finish_pending_cast_cost_or_pay` calls
/// `target_dependent_flash_permission_satisfied` directly with the now-committed
/// `ResolvedAbility` so it does not depend on `state.pending_cast` being
/// installed at that exact instant.
fn spell_targets_filter(
    state: &crate::types::game_state::GameState,
    source_id: ObjectId,
    filter: &crate::types::ability::TargetFilter,
) -> bool {
    use crate::types::ability::TargetRef;
    // Prefer the in-flight pending cast when it matches the source, else fall
    // through to the stack: a spell whose ResolvedAbility carries committed
    // targets is the authoritative source post-announcement. An unrelated
    // pending cast (different `object_id`) is not relevant — keep walking.
    let targets: Option<Vec<TargetRef>> = state
        .pending_cast
        .as_ref()
        .filter(|pending| pending.object_id == source_id)
        .map(|pending| super::ability_utils::flatten_targets_in_chain(&pending.ability))
        .or_else(|| {
            state
                .stack
                .iter()
                .rev()
                .find(|entry| entry.id == source_id)
                .and_then(|entry| match &entry.kind {
                    crate::types::game_state::StackEntryKind::Spell {
                        ability: Some(resolved),
                        ..
                    } => Some(super::ability_utils::flatten_targets_in_chain(resolved)),
                    _ => None,
                })
        });
    let Some(targets) = targets else {
        // Neither a matching pending cast nor a stack entry: the source is
        // pre-announcement (the candidate-generator pass `flash_timing_cost`
        // runs against). Defer the verdict to finalize.
        return true;
    };
    if targets.is_empty() {
        // CR 601.2c: pre-target-selection — defer the verdict to finalize.
        return true;
    }
    let ctx = super::filter::FilterContext::from_source(state, source_id);
    targets.iter().any(|target| match target {
        crate::types::ability::TargetRef::Object(object_id) => {
            super::filter::matches_target_filter(state, *object_id, filter, &ctx)
        }
        crate::types::ability::TargetRef::Player(_) => false,
    })
}

/// CR 601.3d + CR 702.8a: Validate, post-target, that every target-dependent
/// flash permission on the cast object is satisfied by the chosen targets in
/// `ability`. Returns `Ok(())` when each `AsThoughHadFlash` option whose
/// `condition` is a `SpellTargetsFilter` either does not gate this cast or
/// passes against the targets.
///
/// Called at `finish_pending_cast_cost_or_pay` after `assign_targets_in_chain`
/// has committed the player's choices. If a target-dependent flash permission
/// authorized the cast (i.e., the cast is outside the sorcery-speed window via
/// `cast_timing_permission == AsThoughHadFlash`) AND no flash permission's
/// condition currently passes, the cast is illegal under CR 601.3d and must be
/// aborted.
pub(crate) fn target_dependent_flash_permission_satisfied(
    state: &crate::types::game_state::GameState,
    player: PlayerId,
    object_id: ObjectId,
    ability: &crate::types::ability::ResolvedAbility,
) -> bool {
    use crate::types::ability::{ParsedCondition, SpellCastingOptionKind, TargetRef};
    let Some(obj) = state.objects.get(&object_id) else {
        return true;
    };
    // CR 702.8a: A real Flash keyword (printed or granted via continuous effect)
    // authorizes instant-speed casting independent of any conditional flash
    // option. If the spell has Flash, the cast is legal regardless of any
    // `AsThoughHadFlash` option's condition.
    let has_real_flash = super::casting::effective_spell_keyword_kinds(state, player, object_id)
        .contains(&crate::types::keywords::KeywordKind::Flash);
    if has_real_flash {
        return true;
    }
    let targets = super::ability_utils::flatten_targets_in_chain(ability);
    let ctx = super::filter::FilterContext::from_source(state, object_id);
    let evaluate_target_filter = |filter: &crate::types::ability::TargetFilter| -> bool {
        targets.iter().any(|t| match t {
            TargetRef::Object(id) => super::filter::matches_target_filter(state, *id, filter, &ctx),
            TargetRef::Player(_) => false,
        })
    };
    // CR 601.3d: For each AsThoughHadFlash option whose condition is
    // target-dependent, re-evaluate now (we couldn't at announcement). For
    // unconditional options or options with a non-target-dependent condition
    // (e.g. "if you control a Faerie"), defer to the announcement-time check
    // performed by `flash_timing_cost`: that check already gated the cast on
    // entry, and re-running it here would over-strictly reject casts where the
    // game state changed mid-cast (an unusual but possible edge case the rules
    // do not require us to police a second time).
    obj.casting_options
        .iter()
        .filter(|o| o.kind == SpellCastingOptionKind::AsThoughHadFlash)
        .any(|option| match option.condition.as_ref() {
            None => true,
            Some(ParsedCondition::SpellTargetsFilter { filter }) => evaluate_target_filter(filter),
            Some(_other_non_target_condition) => true,
        })
}

/// CR 601.3d: For a spell whose only instant-speed permission is a
/// target-dependent flash option, a cast can only legally proceed if at
/// least one legal target for the spell ALSO satisfies the flash option's
/// `SpellTargetsFilter`. This is the pre-target (candidate-generation)
/// FEASIBILITY check — distinct from the post-target SATISFACTION gate
/// `target_dependent_flash_permission_satisfied`, which tests the player's
/// already-chosen targets. CR 702.8a: a real Flash keyword bypasses entirely.
pub(crate) fn target_dependent_flash_permission_feasible(
    state: &crate::types::game_state::GameState,
    player: PlayerId,
    object_id: ObjectId,
) -> bool {
    use crate::types::ability::{SpellCastingOptionKind, TargetRef};

    // CR 702.8a: A real Flash keyword (printed or granted via continuous
    // effect) authorizes instant-speed casting independent of any conditional
    // flash option — short-circuit before any feasibility analysis.
    let has_real_flash = super::casting::effective_spell_keyword_kinds(state, player, object_id)
        .contains(&crate::types::keywords::KeywordKind::Flash);
    if has_real_flash {
        return true;
    }

    let Some(obj) = state.objects.get(&object_id) else {
        return true;
    };

    // Collect every target-dependent flash gating filter. With none, there is
    // no target-dependent flash permission to police (unconditional or
    // non-target-dependent flash) — mirror the post-target gate's deferral.
    let gating_filters: Vec<&crate::types::ability::TargetFilter> = obj
        .casting_options
        .iter()
        .filter(|o| o.kind == SpellCastingOptionKind::AsThoughHadFlash)
        .filter_map(|o| match o.condition.as_ref() {
            Some(ParsedCondition::SpellTargetsFilter { filter }) => Some(filter),
            _ => None,
        })
        .collect();
    if gating_filters.is_empty() {
        return true;
    }

    // CR 601.3d: Layers must be evaluated before computing legal targets so
    // granted types/keywords are visible — mirror `spell_has_legal_targets`
    // (casting.rs). `find_legal_targets` does not evaluate layers itself.
    let mut simulated = state.clone();
    if simulated.layers_dirty {
        super::layers::evaluate_layers(&mut simulated);
    }
    let Some(obj) = simulated.objects.get(&object_id) else {
        return true;
    };

    // Branch dispatch mirrors `spell_has_legal_targets`: Aura → modal → normal.
    let base_legal_targets: Vec<TargetRef> = if obj.card_types.subtypes.iter().any(|s| s == "Aura")
    {
        // 4a. Aura: targets via the Enchant keyword filter.
        let Some(enchant_filter) = obj.keywords.iter().find_map(|k| match k {
            Keyword::Enchant(filter) => Some(filter.clone()),
            _ => None,
        }) else {
            return false;
        };
        super::targeting::find_legal_targets(&simulated, &enchant_filter, player, obj.id)
    } else if obj.modal.is_some() {
        // 4b. Modal: targets are chosen after mode selection — defer to the
        // finalize-time satisfaction gate.
        return true;
    } else {
        // 4c. Normal: union of every Spell-ability target slot's legal targets.
        let Some(def) = super::casting::combined_spell_ability_def(obj) else {
            // Permanent with no spell ability needs no targets.
            return true;
        };
        let resolved = super::ability_utils::build_resolved_from_def(&def, obj.id, player);
        match super::ability_utils::build_target_slots(&simulated, &resolved) {
            Ok(slots) => {
                if slots.is_empty() {
                    // A SpellTargetsFilter condition requires a target slot to
                    // satisfy — an empty slot set cannot be feasible.
                    return false;
                }
                slots
                    .into_iter()
                    .flat_map(|slot| slot.legal_targets)
                    .collect()
            }
            Err(_) => return false,
        }
    };

    // CR 601.3d: Feasibility = some base legal target ALSO matches a gating
    // flash filter. The flash filter is object-scoped, so a `Player` target can
    // never satisfy it — mirror `target_dependent_flash_permission_satisfied`.
    let ctx = super::filter::FilterContext::from_source(&simulated, object_id);
    gating_filters.iter().any(|flash_filter| {
        base_legal_targets.iter().any(|target| match target {
            TargetRef::Object(id) => {
                super::filter::matches_target_filter(&simulated, *id, flash_filter, &ctx)
            }
            TargetRef::Player(_) => false,
        })
    })
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
    // CR 723: compare the active player against the semantic priority *seat*, not
    // `priority_player` (the authorized submitter). Under turn-control these
    // diverge, so the raw field would never equal `active_player` during a
    // controlled turn and wrongly close this window. Behavior is identical
    // without turn-control, where the seat and submitter are the same player.
    super::turn_control::priority_seat(state) == state.active_player
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

/// CR 602.5b: True when any opponent of `player` controls a creature with `keyword`.
fn opponent_controls_creature_with_keyword(
    state: &crate::types::game_state::GameState,
    player: PlayerId,
    keyword: &Keyword,
) -> bool {
    crate::game::players::opponents(state, player)
        .into_iter()
        .any(|opponent| you_control_creature_with_keyword(state, opponent, keyword))
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
                if obj.controller != player {
                    return false;
                }
                if subtype.eq_ignore_ascii_case("commander") {
                    return obj.is_commander;
                }
                obj.card_types
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
    fn land_played_restriction_checks_player_land_count() {
        let mut state = crate::types::game_state::GameState::new_two_player(42);
        let player = PlayerId(0);
        let source_id = ObjectId(10);
        let condition = ParsedCondition::YouPlayedLandThisTurn;

        assert!(!evaluate_condition(&state, player, source_id, &condition));
        state.players[usize::from(player.0)].lands_played_this_turn = 1;
        assert!(evaluate_condition(&state, player, source_id, &condition));
    }

    #[test]
    fn battlefield_entry_history_condition_survives_object_leaving() {
        let mut state = crate::types::game_state::GameState::new_two_player(42);
        state
            .battlefield_entries_this_turn
            .push(BattlefieldEntryRecord {
                object_id: ObjectId(99),
                name: "Green Creature".to_string(),
                core_types: vec![CoreType::Creature],
                subtypes: vec![],
                supertypes: vec![],
                colors: vec![ManaColor::Green],
                controller: PlayerId(1),
            });
        let mut filter = crate::types::ability::TypedFilter::creature();
        filter.controller = Some(ControllerRef::Opponent);
        filter.properties.push(FilterProp::HasColor {
            color: ManaColor::Green,
        });
        let condition = ParsedCondition::BattlefieldEntriesThisTurn {
            filter: TargetFilter::Typed(filter),
            count: 1,
        };

        assert!(evaluate_condition(
            &state,
            PlayerId(0),
            ObjectId(10),
            &condition
        ));
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
    fn evaluates_opponent_controls_creature_with_flying_condition() {
        // CR 602.5b: Groundling Pouncer — "an opponent controls a creature with flying".
        let mut state = crate::types::game_state::GameState::new_two_player(42);

        // No flyers anywhere → condition false.
        assert!(!parse_and_evaluate_condition(
            &state,
            PlayerId(0),
            ObjectId(1),
            "an opponent controls a creature with flying"
        ));

        // YOU control a flyer → still false (the controller scope is honored).
        let your_bird = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Your Bird".to_string(),
            Zone::Battlefield,
        );
        let your_bird_obj = state.objects.get_mut(&your_bird).unwrap();
        your_bird_obj.card_types.core_types.push(CoreType::Creature);
        your_bird_obj.keywords.push(Keyword::Flying);
        assert!(!parse_and_evaluate_condition(
            &state,
            PlayerId(0),
            your_bird,
            "an opponent controls a creature with flying"
        ));

        // An OPPONENT controls a flyer → condition true.
        let opp_bird = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Opponent Bird".to_string(),
            Zone::Battlefield,
        );
        let opp_bird_obj = state.objects.get_mut(&opp_bird).unwrap();
        opp_bird_obj.card_types.core_types.push(CoreType::Creature);
        opp_bird_obj.keywords.push(Keyword::Flying);
        assert!(parse_and_evaluate_condition(
            &state,
            PlayerId(0),
            your_bird,
            "an opponent controls a creature with flying"
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
    fn evaluates_you_control_a_commander_condition() {
        let mut state = crate::types::game_state::GameState::new_two_player(42);
        let creature = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Legendary Creature".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&creature)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        assert!(!parse_and_evaluate_condition(
            &state,
            PlayerId(0),
            creature,
            "you control a commander"
        ));

        state.objects.get_mut(&creature).unwrap().is_commander = true;
        assert!(parse_and_evaluate_condition(
            &state,
            PlayerId(0),
            creature,
            "you control a commander"
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
    fn is_your_turn_condition_tracks_active_player() {
        // CR 102.1: the active player is the player whose turn it is.
        // Drives the real `evaluate_condition` over `IsYourTurn` and its
        // `Not` wrapper (the form produced for "if it's not your turn").
        let mut state = crate::types::game_state::GameState::new_two_player(42);
        let is_your_turn = ParsedCondition::IsYourTurn;
        let not_your_turn = ParsedCondition::Not {
            condition: Box::new(ParsedCondition::IsYourTurn),
        };

        state.active_player = PlayerId(0);
        assert!(evaluate_condition(
            &state,
            PlayerId(0),
            ObjectId(1),
            &is_your_turn
        ));
        assert!(!evaluate_condition(
            &state,
            PlayerId(0),
            ObjectId(1),
            &not_your_turn
        ));

        state.active_player = PlayerId(1);
        assert!(!evaluate_condition(
            &state,
            PlayerId(0),
            ObjectId(1),
            &is_your_turn
        ));
        assert!(evaluate_condition(
            &state,
            PlayerId(0),
            ObjectId(1),
            &not_your_turn
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
            crate::im::Vector::from(vec![crate::types::game_state::SpellCastRecord {
                name: String::new(),
                core_types: vec![CoreType::Instant],
                supertypes: Vec::new(),
                subtypes: Vec::new(),
                keywords: Vec::new(),
                colors: Vec::new(),
                mana_value: 1,
                has_x_in_cost: false,
                from_zone: Zone::Hand,
                cast_variant: crate::types::game_state::CastingVariant::Normal,
            }]),
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
    fn evaluates_filtered_spell_count_quantity_restriction() {
        let mut state = crate::types::game_state::GameState::new_two_player(42);
        state.spells_cast_this_turn_by_player.insert(
            PlayerId(0),
            crate::im::Vector::from(vec![
                crate::types::game_state::SpellCastRecord {
                    name: String::new(),
                    core_types: vec![CoreType::Instant],
                    supertypes: Vec::new(),
                    subtypes: Vec::new(),
                    keywords: Vec::new(),
                    colors: Vec::new(),
                    mana_value: 1,
                    has_x_in_cost: false,
                    from_zone: Zone::Hand,
                    cast_variant: crate::types::game_state::CastingVariant::Normal,
                },
                crate::types::game_state::SpellCastRecord {
                    name: String::new(),
                    core_types: vec![CoreType::Sorcery],
                    supertypes: Vec::new(),
                    subtypes: Vec::new(),
                    keywords: Vec::new(),
                    colors: Vec::new(),
                    mana_value: 2,
                    has_x_in_cost: false,
                    from_zone: Zone::Hand,
                    cast_variant: crate::types::game_state::CastingVariant::Normal,
                },
                crate::types::game_state::SpellCastRecord {
                    name: String::new(),
                    core_types: vec![CoreType::Instant],
                    supertypes: Vec::new(),
                    subtypes: Vec::new(),
                    keywords: Vec::new(),
                    colors: Vec::new(),
                    mana_value: 3,
                    has_x_in_cost: false,
                    from_zone: Zone::Hand,
                    cast_variant: crate::types::game_state::CastingVariant::Normal,
                },
            ]),
        );

        assert!(parse_and_evaluate_condition(
            &state,
            PlayerId(0),
            ObjectId(1),
            "you've cast three or more instant and/or sorcery spells this turn"
        ));
        assert!(!parse_and_evaluate_condition(
            &state,
            PlayerId(1),
            ObjectId(1),
            "you've cast three or more instant and/or sorcery spells this turn"
        ));
    }

    #[test]
    fn evaluates_filtered_morbid_quantity_restriction() {
        let mut state = crate::types::game_state::GameState::new_two_player(42);
        state
            .zone_changes_this_turn
            .push(crate::types::game_state::ZoneChangeRecord {
                name: "Skeleton".to_string(),
                core_types: vec![CoreType::Creature],
                subtypes: vec!["Skeleton".to_string()],
                controller: PlayerId(0),
                ..crate::types::game_state::ZoneChangeRecord::test_minimal(
                    ObjectId(99),
                    Some(Zone::Battlefield),
                    Zone::Graveyard,
                )
            });

        assert!(!parse_and_evaluate_condition(
            &state,
            PlayerId(0),
            ObjectId(1),
            "a non-Skeleton creature died under your control this turn"
        ));

        state
            .zone_changes_this_turn
            .push(crate::types::game_state::ZoneChangeRecord {
                name: "Vampire".to_string(),
                core_types: vec![CoreType::Creature],
                subtypes: vec!["Vampire".to_string()],
                controller: PlayerId(0),
                ..crate::types::game_state::ZoneChangeRecord::test_minimal(
                    ObjectId(100),
                    Some(Zone::Battlefield),
                    Zone::Graveyard,
                )
            });

        assert!(parse_and_evaluate_condition(
            &state,
            PlayerId(0),
            ObjectId(1),
            "a non-Skeleton creature died under your control this turn"
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

    /// CR 601.3d + CR 903.3 + CR 702.8a — Timely Ward class.
    ///
    /// Builds a spell with `SpellCastingOption::as_though_had_flash().condition(
    /// SpellTargetsFilter { IsCommander })` and verifies the post-target gate:
    /// - targets containing a commander → permission satisfied (cast legal)
    /// - targets without a commander → permission unsatisfied (cast illegal)
    /// - real Flash keyword on the spell → permission satisfied regardless
    ///   (printed Flash trumps the conditional flash option per CR 702.8a)
    #[test]
    fn target_dependent_flash_permission_satisfied_against_commander_target() {
        use crate::game::game_object::GameObject;
        use crate::types::ability::{
            FilterProp, ParsedCondition, ResolvedAbility, SpellCastingOption, TargetFilter,
            TargetRef, TypedFilter,
        };

        let mut state = crate::types::game_state::GameState::new_two_player(42);

        // Caster: PlayerId(0). Opponent: PlayerId(1).
        let caster = PlayerId(0);
        let opponent = PlayerId(1);

        // Spell (Timely Ward stand-in) in caster's hand.
        let mut spell = GameObject::new(
            ObjectId(10),
            CardId(10),
            caster,
            "Timely Ward".to_string(),
            Zone::Hand,
        );
        spell.card_types.core_types.push(CoreType::Enchantment);
        spell
            .casting_options
            .push(SpellCastingOption::as_though_had_flash().condition(
                ParsedCondition::SpellTargetsFilter {
                    filter: TargetFilter::Typed(TypedFilter {
                        properties: vec![FilterProp::IsCommander],
                        ..Default::default()
                    }),
                },
            ));
        state.objects.insert(spell.id, spell);

        // Commander creature controlled by the opponent, on the battlefield.
        let commander = create_object(
            &mut state,
            CardId(20),
            opponent,
            "Some Commander".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&commander).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.is_commander = true;
        }

        // Non-commander creature for the negative case.
        let plain = create_object(
            &mut state,
            CardId(21),
            opponent,
            "Ordinary Bear".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&plain).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
        }

        let ability_with_commander = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 0 },
                target: TargetFilter::Controller,
            },
            vec![TargetRef::Object(commander)],
            ObjectId(10),
            caster,
        );
        let ability_with_plain = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 0 },
                target: TargetFilter::Controller,
            },
            vec![TargetRef::Object(plain)],
            ObjectId(10),
            caster,
        );

        assert!(
            target_dependent_flash_permission_satisfied(
                &state,
                caster,
                ObjectId(10),
                &ability_with_commander
            ),
            "casting at instant speed targeting a commander must satisfy the flash condition"
        );
        assert!(
            !target_dependent_flash_permission_satisfied(
                &state,
                caster,
                ObjectId(10),
                &ability_with_plain
            ),
            "casting at instant speed targeting a non-commander must FAIL the flash condition"
        );
    }

    /// CR 702.8a: A real Flash keyword on the spell short-circuits the
    /// target-dependent flash permission check — printed Flash authorizes
    /// instant-speed casting irrespective of any `AsThoughHadFlash` option's
    /// condition.
    #[test]
    fn real_flash_keyword_overrides_target_dependent_flash_condition() {
        use crate::game::game_object::GameObject;
        use crate::types::ability::{
            FilterProp, ParsedCondition, ResolvedAbility, SpellCastingOption, TargetFilter,
            TargetRef, TypedFilter,
        };
        use crate::types::keywords::Keyword;

        let mut state = crate::types::game_state::GameState::new_two_player(42);
        let caster = PlayerId(0);
        let opponent = PlayerId(1);

        let mut spell = GameObject::new(
            ObjectId(10),
            CardId(10),
            caster,
            "Hypothetical With Both".to_string(),
            Zone::Hand,
        );
        spell.card_types.core_types.push(CoreType::Enchantment);
        spell.keywords.push(Keyword::Flash);
        spell
            .casting_options
            .push(SpellCastingOption::as_though_had_flash().condition(
                ParsedCondition::SpellTargetsFilter {
                    filter: TargetFilter::Typed(TypedFilter {
                        properties: vec![FilterProp::IsCommander],
                        ..Default::default()
                    }),
                },
            ));
        state.objects.insert(spell.id, spell);

        // A non-commander target — the conditional flash option's filter would
        // FAIL against this target, but the printed Flash keyword should
        // independently authorize the cast.
        let plain = create_object(
            &mut state,
            CardId(21),
            opponent,
            "Ordinary Bear".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&plain)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        let ability = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 0 },
                target: TargetFilter::Controller,
            },
            vec![TargetRef::Object(plain)],
            ObjectId(10),
            caster,
        );
        assert!(
            target_dependent_flash_permission_satisfied(&state, caster, ObjectId(10), &ability),
            "printed Flash keyword must short-circuit the target-dependent flash check"
        );
    }

    /// CR 601.3d: The pre-target FEASIBILITY check for a target-dependent flash
    /// permission. A conditional-flash Enchantment whose only instant-speed
    /// permission is `SpellTargetsFilter { IsCommander }` is castable at instant
    /// speed only if a commander target legally exists. With only a
    /// non-commander creature present the cast is infeasible; adding a commander
    /// makes it feasible.
    #[test]
    fn target_dependent_flash_permission_feasible_requires_a_commander_target() {
        use crate::game::game_object::GameObject;
        use crate::types::ability::{
            AbilityDefinition, AbilityKind, Effect, FilterProp, ParsedCondition,
            SpellCastingOption, TargetFilter, TypedFilter,
        };

        let mut state = crate::types::game_state::GameState::new_two_player(42);
        let caster = PlayerId(0);
        let opponent = PlayerId(1);

        // Conditional-flash Enchantment with a Spell-kind ability that targets
        // a creature; the only flash permission is gated on IsCommander.
        let mut spell = GameObject::new(
            ObjectId(10),
            CardId(10),
            caster,
            "Timely Ward".to_string(),
            Zone::Hand,
        );
        spell.card_types.core_types.push(CoreType::Enchantment);
        spell
            .casting_options
            .push(SpellCastingOption::as_though_had_flash().condition(
                ParsedCondition::SpellTargetsFilter {
                    filter: TargetFilter::Typed(TypedFilter {
                        properties: vec![FilterProp::IsCommander],
                        ..Default::default()
                    }),
                },
            ));
        std::sync::Arc::make_mut(&mut spell.abilities).push(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Destroy {
                target: TargetFilter::Typed(TypedFilter::creature()),
                cant_regenerate: false,
            },
        ));
        state.objects.insert(spell.id, spell);

        // Non-commander creature only — no commander target exists.
        let plain = create_object(
            &mut state,
            CardId(21),
            opponent,
            "Ordinary Bear".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&plain)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        assert!(
            !target_dependent_flash_permission_feasible(&state, caster, ObjectId(10)),
            "no commander on the battlefield ⇒ the conditional flash cast is infeasible"
        );

        // Add a commander creature: a satisfying target now exists.
        let commander = create_object(
            &mut state,
            CardId(20),
            opponent,
            "Some Commander".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&commander).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.is_commander = true;
        }
        assert!(
            target_dependent_flash_permission_feasible(&state, caster, ObjectId(10)),
            "a commander creature on the battlefield ⇒ the conditional flash cast is feasible"
        );
    }

    /// CR 702.8a: A printed Flash keyword short-circuits the pre-target
    /// feasibility check — even with no condition-satisfying target the cast is
    /// feasible because real Flash authorizes instant-speed casting outright.
    #[test]
    fn target_dependent_flash_permission_feasible_real_flash_bypass() {
        use crate::game::game_object::GameObject;
        use crate::types::ability::{
            AbilityDefinition, AbilityKind, Effect, FilterProp, ParsedCondition,
            SpellCastingOption, TargetFilter, TypedFilter,
        };
        use crate::types::keywords::Keyword;

        let mut state = crate::types::game_state::GameState::new_two_player(42);
        let caster = PlayerId(0);
        let opponent = PlayerId(1);

        let mut spell = GameObject::new(
            ObjectId(10),
            CardId(10),
            caster,
            "Has Both".to_string(),
            Zone::Hand,
        );
        spell.card_types.core_types.push(CoreType::Enchantment);
        spell.keywords.push(Keyword::Flash);
        spell
            .casting_options
            .push(SpellCastingOption::as_though_had_flash().condition(
                ParsedCondition::SpellTargetsFilter {
                    filter: TargetFilter::Typed(TypedFilter {
                        properties: vec![FilterProp::IsCommander],
                        ..Default::default()
                    }),
                },
            ));
        std::sync::Arc::make_mut(&mut spell.abilities).push(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Destroy {
                target: TargetFilter::Typed(TypedFilter::creature()),
                cant_regenerate: false,
            },
        ));
        state.objects.insert(spell.id, spell);

        // Only a non-commander target — would fail the conditional flash filter.
        let plain = create_object(
            &mut state,
            CardId(21),
            opponent,
            "Ordinary Bear".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&plain)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        assert!(
            target_dependent_flash_permission_feasible(&state, caster, ObjectId(10)),
            "printed Flash must bypass the pre-target feasibility check (CR 702.8a)"
        );
    }

    /// CR 601.3d: Modal cards choose targets after mode selection, so the
    /// pre-target feasibility check defers to the finalize-time satisfaction
    /// gate — `obj.modal.is_some()` ⇒ feasible even with no satisfying target.
    #[test]
    fn target_dependent_flash_permission_feasible_modal_defers() {
        use crate::game::game_object::GameObject;
        use crate::types::ability::{
            FilterProp, ModalChoice, ParsedCondition, SpellCastingOption, TargetFilter, TypedFilter,
        };

        let mut state = crate::types::game_state::GameState::new_two_player(42);
        let caster = PlayerId(0);

        let mut spell = GameObject::new(
            ObjectId(10),
            CardId(10),
            caster,
            "Modal Conditional Flash".to_string(),
            Zone::Hand,
        );
        spell.card_types.core_types.push(CoreType::Instant);
        spell.modal = Some(ModalChoice {
            min_choices: 1,
            max_choices: 1,
            mode_count: 2,
            ..Default::default()
        });
        spell
            .casting_options
            .push(SpellCastingOption::as_though_had_flash().condition(
                ParsedCondition::SpellTargetsFilter {
                    filter: TargetFilter::Typed(TypedFilter {
                        properties: vec![FilterProp::IsCommander],
                        ..Default::default()
                    }),
                },
            ));
        state.objects.insert(spell.id, spell);

        // No commander, no targets at all — but the modal branch defers.
        assert!(
            target_dependent_flash_permission_feasible(&state, caster, ObjectId(10)),
            "modal cards defer the feasibility verdict to the finalize-time gate"
        );
    }

    /// CR 601.3d: Aura branch — a conditional-flash Aura targets via its
    /// `Keyword::Enchant` filter. Feasibility requires a battlefield object that
    /// matches BOTH the Enchant filter AND the flash `SpellTargetsFilter`.
    #[test]
    fn target_dependent_flash_permission_feasible_aura_branch() {
        use crate::game::game_object::GameObject;
        use crate::types::ability::{
            FilterProp, ParsedCondition, SpellCastingOption, TargetFilter, TypedFilter,
        };
        use crate::types::keywords::Keyword;

        let mut state = crate::types::game_state::GameState::new_two_player(42);
        let caster = PlayerId(0);
        let opponent = PlayerId(1);

        // Conditional-flash Aura: Enchant creature, flash gated on IsCommander.
        let mut aura = GameObject::new(
            ObjectId(10),
            CardId(10),
            caster,
            "Conditional Flash Aura".to_string(),
            Zone::Hand,
        );
        aura.card_types.core_types.push(CoreType::Enchantment);
        aura.card_types.subtypes.push("Aura".to_string());
        aura.keywords.push(Keyword::Enchant(TargetFilter::Typed(
            TypedFilter::creature(),
        )));
        aura.casting_options
            .push(SpellCastingOption::as_though_had_flash().condition(
                ParsedCondition::SpellTargetsFilter {
                    filter: TargetFilter::Typed(TypedFilter {
                        properties: vec![FilterProp::IsCommander],
                        ..Default::default()
                    }),
                },
            ));
        state.objects.insert(aura.id, aura);

        // Non-commander creature only: matches Enchant filter but not the flash
        // filter ⇒ infeasible.
        let plain = create_object(
            &mut state,
            CardId(21),
            opponent,
            "Ordinary Bear".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&plain)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);
        assert!(
            !target_dependent_flash_permission_feasible(&state, caster, ObjectId(10)),
            "Aura with only a non-commander enchantable target ⇒ infeasible"
        );

        // Commander creature: matches both the Enchant filter and the flash
        // filter ⇒ feasible.
        let commander = create_object(
            &mut state,
            CardId(20),
            opponent,
            "Some Commander".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&commander).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.is_commander = true;
        }
        assert!(
            target_dependent_flash_permission_feasible(&state, caster, ObjectId(10)),
            "Aura with a commander enchantable target ⇒ feasible"
        );
    }

    /// CR 117.1 + CR 201.2 + CR 601.2: Cast-pipeline integration for Approach
    /// of the Second Sun's "another spell named ~ this game" gate. Exercises
    /// the full path: `record_spell_cast_from_zone` → `SpellCastRecord.name`
    /// populated on the game-scope history → name-filtered
    /// `QuantityRef::SpellsCastThisGame` resolves correctly.
    ///
    /// The earlier `resolve_quantity_spells_cast_this_game_filtered_by_name`
    /// test hand-populates `spells_cast_this_game_by_player`, bypassing the
    /// pipeline hook. This test fails if any future cast path forks the
    /// recording flow (alt-cost, free cast, escape, etc.) and forgets to
    /// invoke `record_spell_cast_from_zone`, or if the `name` field stops
    /// being captured from the cast object — both regressions Approach of
    /// the Second Sun would silently inherit otherwise.
    #[test]
    fn approach_of_the_second_sun_round_trips_through_record_spell_cast() {
        use crate::game::game_object::GameObject;
        use crate::game::quantity::resolve_quantity;
        use crate::types::ability::{
            CountScope, FilterProp, QuantityExpr, QuantityRef, TargetFilter, TypedFilter,
        };

        let mut state = crate::types::game_state::GameState::new_two_player(42);
        let caster = PlayerId(0);

        // Build an Approach `GameObject` shaped the way the cast pipeline
        // would hand it to `record_spell_cast_from_zone`.
        let approach = GameObject::new(
            ObjectId(10),
            CardId(10),
            caster,
            "Approach of the Second Sun".to_string(),
            Zone::Stack,
        );

        // Mirror the parser's emitted filter exactly: lowercased name match
        // against `SpellCastRecord.name` via `eq_ignore_ascii_case`.
        let approach_filter =
            TargetFilter::Typed(TypedFilter::default().properties(vec![FilterProp::Named {
                name: "approach of the second sun".to_string(),
            }]));
        let approach_count = QuantityExpr::Ref {
            qty: QuantityRef::SpellsCastThisGame {
                scope: CountScope::Controller,
                filter: Some(approach_filter),
            },
        };

        // Pre-cast: no Approaches recorded → count is 0, gate fails.
        assert_eq!(
            resolve_quantity(&state, &approach_count, caster, ObjectId(10)),
            0,
            "no casts recorded yet"
        );

        // First cast: pipeline records the spell.
        record_spell_cast(
            &mut state,
            caster,
            &approach,
            crate::types::game_state::CastingVariant::Normal,
        );
        let history = state
            .spells_cast_this_game_by_player
            .get(&caster)
            .expect("first cast must populate the game-scope history");
        assert_eq!(history.len(), 1);
        assert_eq!(
            history[0].name, "Approach of the Second Sun",
            "record_spell_cast must capture `obj.name` so name filters can match it"
        );
        assert_eq!(
            resolve_quantity(&state, &approach_count, caster, ObjectId(10)),
            1,
            "first Approach must register against the named-filter count"
        );

        // Second cast: same name, same player. The "another" gate (>= 2)
        // is now satisfied.
        record_spell_cast(
            &mut state,
            caster,
            &approach,
            crate::types::game_state::CastingVariant::Normal,
        );
        assert_eq!(
            resolve_quantity(&state, &approach_count, caster, ObjectId(10)),
            2,
            "second Approach must bring the count to 2 — `another spell named ~ this game` becomes true"
        );

        // Cross-player scope safety: a different player's casts of the same
        // name must NOT count toward the caster's controller-scoped gate.
        let opponent = PlayerId(1);
        record_spell_cast(
            &mut state,
            opponent,
            &approach,
            crate::types::game_state::CastingVariant::Normal,
        );
        assert_eq!(
            resolve_quantity(&state, &approach_count, caster, ObjectId(10)),
            2,
            "controller-scoped count must ignore an opponent's named-Approach casts"
        );
    }

    /// CR 702.185c: `record_spell_cast` threads the `CastingVariant` onto the
    /// persisted `SpellCastRecord`, and `spell_cast_with_variant_this_turn`
    /// reads it. A warp cast makes "a spell was warped this turn" true; a
    /// normal cast does not. Verifies the building block — the recording hook
    /// and the resolver — independent of any single card.
    #[test]
    fn spell_cast_with_variant_this_turn_tracks_warp() {
        use crate::game::game_object::GameObject;
        use crate::types::game_state::CastingVariant;

        let mut state = crate::types::game_state::GameState::new_two_player(7);
        let caster = PlayerId(0);
        let spell = GameObject::new(
            ObjectId(20),
            CardId(20),
            caster,
            "Warp Spell".to_string(),
            Zone::Stack,
        );

        // No casts yet → false.
        assert!(!spell_cast_with_variant_this_turn(
            &state,
            &CastingVariant::Warp
        ));

        // A normal cast records `CastingVariant::Normal` → warp query still false.
        record_spell_cast(&mut state, caster, &spell, CastingVariant::Normal);
        assert_eq!(
            state.spells_cast_this_turn_by_player[&caster][0].cast_variant,
            CastingVariant::Normal
        );
        assert!(!spell_cast_with_variant_this_turn(
            &state,
            &CastingVariant::Warp
        ));

        // A warp cast records `CastingVariant::Warp` → warp query becomes true.
        record_spell_cast(&mut state, caster, &spell, CastingVariant::Warp);
        assert_eq!(
            state.spells_cast_this_turn_by_player[&caster][1].cast_variant,
            CastingVariant::Warp
        );
        assert!(spell_cast_with_variant_this_turn(
            &state,
            &CastingVariant::Warp
        ));
    }
}
