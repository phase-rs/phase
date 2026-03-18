use std::str::FromStr;

use crate::game::game_object::{parse_counter_type, CounterType, GameObject};
use crate::parser::oracle_util::parse_number;
use crate::types::ability::{
    AbilityCost, AbilityDefinition, ActivationRestriction, CastingRestriction, Comparator,
    SpellCastingOptionKind,
};
use crate::types::card_type::{CoreType, Supertype};
use crate::types::keywords::Keyword;
use crate::types::mana::{ManaColor, ManaCost};
use crate::types::phase::Phase;
use crate::types::player::PlayerId;
use crate::types::zones::Zone;

use super::engine::EngineError;
use crate::types::identifiers::ObjectId;

pub fn check_spell_timing(
    state: &crate::types::game_state::GameState,
    player: PlayerId,
    obj: &GameObject,
    ability_def: &AbilityDefinition,
    allow_flash_timing: bool,
) -> Result<(), EngineError> {
    let is_instant_speed = allow_flash_timing
        || obj.card_types.core_types.contains(&CoreType::Instant)
        || obj.has_keyword(&Keyword::Flash);

    if !is_instant_speed && ability_def.kind == crate::types::ability::AbilityKind::Spell {
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
            .is_some_and(|condition| !evaluate_condition_text(state, player, obj.id, condition))
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
        (ManaCost::NoCost, other) => other.clone(),
        (other, ManaCost::NoCost) => other.clone(),
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

pub fn record_spell_cast(
    state: &mut crate::types::game_state::GameState,
    player: PlayerId,
    obj: &GameObject,
) {
    state.spells_cast_this_turn = state.spells_cast_this_turn.saturating_add(1);
    *state.spells_cast_this_game.entry(player).or_insert(0) += 1;
    *state
        .spells_cast_this_turn_by_player
        .entry(player)
        .or_insert(0) += 1;
    if !obj.card_types.core_types.contains(&CoreType::Creature) {
        state
            .players_who_cast_noncreature_spell_this_turn
            .insert(player);
    }
}

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

    if obj.card_types.core_types.contains(&CoreType::Creature) {
        state
            .players_who_had_creature_etb_this_turn
            .insert(obj.controller);
        if obj
            .card_types
            .subtypes
            .iter()
            .any(|subtype| subtype.eq_ignore_ascii_case("Angel"))
            || obj
                .card_types
                .subtypes
                .iter()
                .any(|subtype| subtype.eq_ignore_ascii_case("Berserker"))
        {
            state
                .players_who_had_angel_or_berserker_etb_this_turn
                .insert(obj.controller);
        }
    }
    if obj.card_types.core_types.contains(&CoreType::Artifact) {
        state
            .players_who_had_artifact_etb_this_turn
            .insert(obj.controller);
    }
}

pub fn record_zone_change(
    state: &mut crate::types::game_state::GameState,
    object_id: ObjectId,
    from: Zone,
    to: Zone,
) {
    let Some(obj) = state.objects.get(&object_id) else {
        return;
    };

    if from == Zone::Graveyard && to != Zone::Graveyard {
        *state
            .cards_left_graveyard_this_turn
            .entry(obj.owner)
            .or_insert(0) += 1;
    }

    if from == Zone::Battlefield
        && to == Zone::Graveyard
        && obj.card_types.core_types.contains(&CoreType::Creature)
    {
        // "Dies" means "is put into a graveyard from the battlefield" (CR 700.4).
        state.creature_died_this_turn = true;
    }

    if to == Zone::Battlefield {
        record_battlefield_entry(state, object_id);
    }
}

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
        ActivationRestriction::AsSorcery => is_sorcery_speed_window(state, player),
        ActivationRestriction::AsInstant => true,
        ActivationRestriction::DuringYourTurn => state.active_player == player,
        ActivationRestriction::DuringYourUpkeep => {
            state.active_player == player && state.phase == Phase::Upkeep
        }
        ActivationRestriction::DuringCombat => is_combat_phase(state.phase),
        ActivationRestriction::BeforeAttackersDeclared => is_before_attackers_declared(state),
        ActivationRestriction::BeforeCombatDamage => is_before_combat_damage(state.phase),
        ActivationRestriction::OnlyOnceEachTurn => {
            state
                .activated_abilities_this_turn
                .get(&key)
                .copied()
                .unwrap_or(0)
                == 0
        }
        ActivationRestriction::OnlyOnce => {
            state
                .activated_abilities_this_game
                .get(&key)
                .copied()
                .unwrap_or(0)
                == 0
        }
        ActivationRestriction::MaxTimesEachTurn { count } => {
            state
                .activated_abilities_this_turn
                .get(&key)
                .copied()
                .unwrap_or(0)
                < u32::from(*count)
        }
        ActivationRestriction::RequiresCondition { text } => {
            evaluate_condition_text(state, player, source_id, text)
        }
        // CR 719.4: Only activatable while the source Case is solved.
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
    }
}

fn casting_restriction_applies(
    state: &crate::types::game_state::GameState,
    player: PlayerId,
    source_id: ObjectId,
    restriction: &CastingRestriction,
) -> bool {
    match restriction {
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
        CastingRestriction::DeclareAttackersStep => state.phase == Phase::DeclareAttackers,
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
        CastingRestriction::RequiresCondition { text } => {
            evaluate_condition_text(state, player, source_id, text)
        }
    }
}

pub fn evaluate_condition_text(
    state: &crate::types::game_state::GameState,
    player: PlayerId,
    source_id: ObjectId,
    text: &str,
) -> bool {
    let lower = text.trim().trim_end_matches('.').to_lowercase();
    let Some(condition) = parse_condition_text(&lower) else {
        // Preserve pre-enforcement behavior for conditions we have not lowered yet.
        return true;
    };
    evaluate_condition(state, player, source_id, &condition)
}

/// A player-relative quantity that can be measured for any participant.
/// Used in QuantityVsEachOpponent to compare the active player against each opponent.
#[derive(Debug, Clone, PartialEq, Eq)]
enum PlayerQuantity {
    HandSize,
    ControlledCreatureCount,
}

enum RestrictionCondition {
    SourceInZone(Zone),
    SourceIsAttacking,
    SourceIsAttackingOrBlocking,
    SourceIsBlocked,
    SourcePowerAtLeast(i32),
    SourceHasCounterAtLeast {
        counter_type: CounterType,
        count: u32,
    },
    SourceHasNoCounter(CounterType),
    SourceEnteredThisTurn,
    SourceIsCreature,
    SourceUntappedAttachedTo {
        required_type: CoreType,
    },
    SourceLacksKeyword(Keyword),
    SourceIsColor(crate::types::mana::ManaColor),
    FirstSpellThisGame,
    OpponentSearchedLibraryThisTurn,
    BeenAttackedThisStep,
    GraveyardCardCountAtLeast(usize),
    GraveyardCardTypeCountAtLeast(usize),
    GraveyardSubtypeCardCountAtLeast {
        subtype: String,
        count: usize,
    },
    OpponentPoisonAtLeast(u32),
    HandSizeExact(usize),
    HandSizeOneOf(Vec<usize>),
    QuantityVsEachOpponent {
        lhs: PlayerQuantity,
        comparator: Comparator,
        rhs: PlayerQuantity,
    },
    CreaturesYouControlTotalPowerAtLeast(i32),
    YouControlLandSubtypeAny(Vec<String>),
    YouControlSubtypeCountAtLeast {
        subtype: String,
        count: usize,
    },
    YouControlCoreTypeCountAtLeast {
        core_type: CoreType,
        count: usize,
    },
    YouControlColorPermanentCountAtLeast {
        color: crate::types::mana::ManaColor,
        count: usize,
    },
    YouControlSubtypeOrGraveyardCardSubtype {
        subtype: String,
    },
    YouControlLegendaryCreature,
    YouControlNamedPlaneswalker(String),
    YouControlCreatureWithKeyword(Keyword),
    YouControlCreatureWithPowerAtLeast(i32),
    YouControlCreatureWithPt {
        power: i32,
        toughness: i32,
    },
    YouControlAnotherColorlessCreature,
    YouControlSnowPermanentCountAtLeast(usize),
    YouControlDifferentPowerCreatureCountAtLeast(usize),
    YouControlLandsWithSameNameAtLeast(usize),
    YouControlNoCreatures,
    YouAttackedThisTurn,
    YouAttackedWithAtLeast(u32),
    YouCastNoncreatureSpellThisTurn,
    YouCastSpellCountAtLeast(u32),
    YouGainedLifeThisTurn,
    YouCreatedTokenThisTurn,
    YouDiscardedCardThisTurn,
    YouSacrificedArtifactThisTurn,
    CreatureDiedThisTurn,
    YouHadCreatureEnterThisTurn,
    YouHadAngelOrBerserkerEnterThisTurn,
    YouHadArtifactEnterThisTurn,
    CardsLeftYourGraveyardThisTurnAtLeast(u32),
}

fn parse_condition_text(text: &str) -> Option<RestrictionCondition> {
    if let Some(condition) = parse_source_condition(text) {
        return Some(condition);
    }
    if let Some(condition) = parse_you_control_condition(text) {
        return Some(condition);
    }
    if let Some(condition) = parse_graveyard_condition(text) {
        return Some(condition);
    }
    if let Some(condition) = parse_hand_condition(text) {
        return Some(condition);
    }

    match text {
        "this spell is the first spell you've cast this game" => {
            Some(RestrictionCondition::FirstSpellThisGame)
        }
        "an opponent searched their library this turn" => {
            Some(RestrictionCondition::OpponentSearchedLibraryThisTurn)
        }
        "you've been attacked this step" => Some(RestrictionCondition::BeenAttackedThisStep),
        "an opponent has three or more poison counters" => {
            Some(RestrictionCondition::OpponentPoisonAtLeast(3))
        }
        "you attacked this turn" => Some(RestrictionCondition::YouAttackedThisTurn),
        "you gained life this turn" => Some(RestrictionCondition::YouGainedLifeThisTurn),
        "you created a token this turn" => Some(RestrictionCondition::YouCreatedTokenThisTurn),
        "a creature died this turn" => Some(RestrictionCondition::CreatureDiedThisTurn),
        "you've cast a noncreature spell this turn" => {
            Some(RestrictionCondition::YouCastNoncreatureSpellThisTurn)
        }
        "you've discarded a card this turn" => Some(RestrictionCondition::YouDiscardedCardThisTurn),
        "you've sacrificed an artifact this turn" => {
            Some(RestrictionCondition::YouSacrificedArtifactThisTurn)
        }
        "you had a creature enter the battlefield under your control this turn" => {
            Some(RestrictionCondition::YouHadCreatureEnterThisTurn)
        }
        "you had an angel or berserker enter the battlefield under your control this turn" => {
            Some(RestrictionCondition::YouHadAngelOrBerserkerEnterThisTurn)
        }
        "this artifact or another artifact entered the battlefield under your control this turn" => {
            Some(RestrictionCondition::YouHadArtifactEnterThisTurn)
        }
        _ => {
            if let Some(count) =
                parse_numeric_threshold(text, "you attacked with ", " creatures this turn")
            {
                return Some(RestrictionCondition::YouAttackedWithAtLeast(count as u32));
            }
            if let Some(count) =
                parse_numeric_threshold(text, "you've cast ", " or more spells this turn")
            {
                return Some(RestrictionCondition::YouCastSpellCountAtLeast(count as u32));
            }
            if let Some(count) = parse_numeric_threshold(
                text,
                "",
                " or more cards left your graveyard this turn",
            ) {
                return Some(RestrictionCondition::CardsLeftYourGraveyardThisTurnAtLeast(
                    count as u32,
                ));
            }
            None
        }
    }
}

fn evaluate_condition(
    state: &crate::types::game_state::GameState,
    player: PlayerId,
    source_id: ObjectId,
    condition: &RestrictionCondition,
) -> bool {
    match condition {
        RestrictionCondition::SourceInZone(zone) => state
            .objects
            .get(&source_id)
            .is_some_and(|obj| obj.zone == *zone),
        RestrictionCondition::SourceIsAttacking => is_source_attacking(state, source_id),
        RestrictionCondition::SourceIsAttackingOrBlocking => {
            is_source_attacking(state, source_id) || is_source_blocking(state, source_id)
        }
        RestrictionCondition::SourceIsBlocked => is_source_blocked(state, source_id),
        RestrictionCondition::SourcePowerAtLeast(minimum) => state
            .objects
            .get(&source_id)
            .and_then(|obj| obj.power)
            .is_some_and(|power| power >= *minimum),
        RestrictionCondition::SourceHasCounterAtLeast {
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
        RestrictionCondition::SourceHasNoCounter(counter_type) => {
            state
                .objects
                .get(&source_id)
                .and_then(|obj| obj.counters.get(counter_type))
                .copied()
                .unwrap_or(0)
                == 0
        }
        RestrictionCondition::SourceEnteredThisTurn => state
            .objects
            .get(&source_id)
            .and_then(|obj| obj.entered_battlefield_turn)
            .is_some_and(|turn| turn == state.turn_number),
        RestrictionCondition::SourceIsCreature => state
            .objects
            .get(&source_id)
            .is_some_and(|obj| obj.card_types.core_types.contains(&CoreType::Creature)),
        RestrictionCondition::SourceUntappedAttachedTo { required_type } => state
            .objects
            .get(&source_id)
            .and_then(|obj| obj.attached_to)
            .and_then(|attached_to| state.objects.get(&attached_to))
            .is_some_and(|obj| !obj.tapped && obj.card_types.core_types.contains(required_type)),
        RestrictionCondition::SourceLacksKeyword(keyword) => state
            .objects
            .get(&source_id)
            .is_some_and(|obj| !obj.has_keyword(keyword)),
        RestrictionCondition::SourceIsColor(color) => state
            .objects
            .get(&source_id)
            .is_some_and(|obj| obj.color.contains(color)),
        RestrictionCondition::FirstSpellThisGame => {
            state
                .spells_cast_this_game
                .get(&player)
                .copied()
                .unwrap_or(0)
                == 0
        }
        RestrictionCondition::OpponentSearchedLibraryThisTurn => state
            .players_who_searched_library_this_turn
            .iter()
            .any(|searched| *searched != player),
        RestrictionCondition::BeenAttackedThisStep => {
            state.players_attacked_this_step.contains(&player)
        }
        RestrictionCondition::GraveyardCardCountAtLeast(count) => {
            player_graveyard_ids(state, player).len() >= *count
        }
        RestrictionCondition::GraveyardCardTypeCountAtLeast(count) => {
            distinct_graveyard_card_type_count(state, player) >= *count
        }
        RestrictionCondition::GraveyardSubtypeCardCountAtLeast { subtype, count } => state
            .players
            .iter()
            .find(|candidate| candidate.id == player)
            .is_some_and(|candidate| {
                candidate
                    .graveyard
                    .iter()
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
            }),
        RestrictionCondition::OpponentPoisonAtLeast(count) => state
            .players
            .iter()
            .any(|candidate| candidate.id != player && candidate.poison_counters >= *count),
        RestrictionCondition::HandSizeExact(count) => player_hand_size(state, player) == *count,
        RestrictionCondition::HandSizeOneOf(counts) => {
            counts.contains(&player_hand_size(state, player))
        }
        RestrictionCondition::QuantityVsEachOpponent {
            lhs,
            comparator,
            rhs,
        } => {
            let lhs_val = resolve_player_quantity(state, lhs, player);
            state
                .players
                .iter()
                .filter(|candidate| candidate.id != player)
                .all(|candidate| {
                    let rhs_val = resolve_player_quantity(state, rhs, candidate.id);
                    comparator.clone().evaluate(lhs_val as i32, rhs_val as i32)
                })
        }
        RestrictionCondition::CreaturesYouControlTotalPowerAtLeast(minimum) => {
            total_power_of_controlled_creatures(state, player) >= *minimum
        }
        RestrictionCondition::YouControlLandSubtypeAny(subtypes) => {
            you_control_land_with_any_subtype(state, player, subtypes)
        }
        RestrictionCondition::YouControlSubtypeCountAtLeast { subtype, count } => {
            you_control_subtype_count(state, player, subtype, *count)
        }
        RestrictionCondition::YouControlCoreTypeCountAtLeast { core_type, count } => {
            controlled_objects_matching_count(state, player, |obj| {
                obj.card_types.core_types.contains(core_type)
            }) >= *count
        }
        RestrictionCondition::YouControlColorPermanentCountAtLeast { color, count } => {
            controlled_objects_matching_count(state, player, |obj| obj.color.contains(color))
                >= *count
        }
        RestrictionCondition::YouControlSubtypeOrGraveyardCardSubtype { subtype } => {
            you_control_subtype_count(state, player, subtype, 1)
                || graveyard_has_subtype_card(state, player, subtype)
        }
        RestrictionCondition::YouControlLegendaryCreature => {
            controlled_objects_matching_count(state, player, |obj| {
                obj.card_types.core_types.contains(&CoreType::Creature)
                    && obj.card_types.supertypes.contains(&Supertype::Legendary)
            }) >= 1
        }
        RestrictionCondition::YouControlNamedPlaneswalker(name) => {
            controlled_objects_matching_count(state, player, |obj| {
                obj.card_types.core_types.contains(&CoreType::Planeswalker)
                    && obj.name.contains(name)
            }) >= 1
        }
        RestrictionCondition::YouControlCreatureWithKeyword(keyword) => {
            you_control_creature_with_keyword(state, player, keyword)
        }
        RestrictionCondition::YouControlCreatureWithPowerAtLeast(minimum) => {
            controlled_objects_matching_count(state, player, |obj| {
                obj.card_types.core_types.contains(&CoreType::Creature)
                    && obj.power.is_some_and(|power| power >= *minimum)
            }) >= 1
        }
        RestrictionCondition::YouControlCreatureWithPt { power, toughness } => {
            controlled_objects_matching_count(state, player, |obj| {
                obj.card_types.core_types.contains(&CoreType::Creature)
                    && obj.power == Some(*power)
                    && obj.toughness == Some(*toughness)
            }) >= 1
        }
        RestrictionCondition::YouControlAnotherColorlessCreature => {
            controlled_objects_matching_count(state, player, |obj| {
                obj.id != source_id
                    && obj.card_types.core_types.contains(&CoreType::Creature)
                    && obj.color.is_empty()
            }) >= 1
        }
        RestrictionCondition::YouControlSnowPermanentCountAtLeast(count) => {
            controlled_objects_matching_count(state, player, |obj| {
                obj.card_types.supertypes.contains(&Supertype::Snow)
            }) >= *count
        }
        RestrictionCondition::YouControlDifferentPowerCreatureCountAtLeast(count) => {
            controlled_creature_power_count(state, player) >= *count
        }
        RestrictionCondition::YouControlLandsWithSameNameAtLeast(count) => {
            controlled_land_same_name_count(state, player) >= *count
        }
        RestrictionCondition::YouControlNoCreatures => {
            controlled_objects_matching_count(state, player, |obj| {
                obj.card_types.core_types.contains(&CoreType::Creature)
            }) == 0
        }
        RestrictionCondition::YouAttackedThisTurn => {
            state.players_attacked_this_turn.contains(&player)
        }
        RestrictionCondition::YouAttackedWithAtLeast(count) => {
            state
                .attacking_creatures_this_turn
                .get(&player)
                .copied()
                .unwrap_or(0)
                >= *count
        }
        RestrictionCondition::YouCastNoncreatureSpellThisTurn => state
            .players_who_cast_noncreature_spell_this_turn
            .contains(&player),
        RestrictionCondition::YouCastSpellCountAtLeast(count) => {
            state
                .spells_cast_this_turn_by_player
                .get(&player)
                .copied()
                .unwrap_or(0)
                >= *count
        }
        RestrictionCondition::YouGainedLifeThisTurn => state
            .players
            .iter()
            .find(|candidate| candidate.id == player)
            .is_some_and(|candidate| candidate.life_gained_this_turn > 0),
        RestrictionCondition::YouCreatedTokenThisTurn => {
            state.players_who_created_token_this_turn.contains(&player)
        }
        RestrictionCondition::YouDiscardedCardThisTurn => {
            state.players_who_discarded_card_this_turn.contains(&player)
        }
        RestrictionCondition::YouSacrificedArtifactThisTurn => state
            .players_who_sacrificed_artifact_this_turn
            .contains(&player),
        RestrictionCondition::CreatureDiedThisTurn => state.creature_died_this_turn,
        RestrictionCondition::YouHadCreatureEnterThisTurn => state
            .players_who_had_creature_etb_this_turn
            .contains(&player),
        RestrictionCondition::YouHadAngelOrBerserkerEnterThisTurn => state
            .players_who_had_angel_or_berserker_etb_this_turn
            .contains(&player),
        RestrictionCondition::YouHadArtifactEnterThisTurn => state
            .players_who_had_artifact_etb_this_turn
            .contains(&player),
        RestrictionCondition::CardsLeftYourGraveyardThisTurnAtLeast(count) => {
            state
                .cards_left_graveyard_this_turn
                .get(&player)
                .copied()
                .unwrap_or(0)
                >= *count
        }
    }
}

fn is_sorcery_speed_window(state: &crate::types::game_state::GameState, player: PlayerId) -> bool {
    matches!(state.phase, Phase::PreCombatMain | Phase::PostCombatMain)
        && state.stack.is_empty()
        && state.active_player == player
}

fn is_before_attackers_declared(state: &crate::types::game_state::GameState) -> bool {
    state.active_player == state.priority_player
        && matches!(state.phase, Phase::PreCombatMain | Phase::BeginCombat)
}

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

fn resolve_player_quantity(
    state: &crate::types::game_state::GameState,
    quantity: &PlayerQuantity,
    player: PlayerId,
) -> usize {
    match quantity {
        PlayerQuantity::HandSize => player_hand_size(state, player),
        PlayerQuantity::ControlledCreatureCount => {
            controlled_objects_matching_count(state, player, |obj| {
                obj.card_types.core_types.contains(&CoreType::Creature)
            })
        }
    }
}

fn player_graveyard_ids(
    state: &crate::types::game_state::GameState,
    player: PlayerId,
) -> &[ObjectId] {
    state
        .players
        .iter()
        .find(|candidate| candidate.id == player)
        .map(|candidate| candidate.graveyard.as_slice())
        .unwrap_or(&[])
}

fn distinct_graveyard_card_type_count(
    state: &crate::types::game_state::GameState,
    player: PlayerId,
) -> usize {
    let mut card_types = std::collections::HashSet::new();
    for object_id in player_graveyard_ids(state, player) {
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
    player_graveyard_ids(state, player).iter().any(|object_id| {
        state.objects.get(object_id).is_some_and(|obj| {
            obj.card_types
                .subtypes
                .iter()
                .any(|candidate| candidate.eq_ignore_ascii_case(subtype))
        })
    })
}

fn is_source_attacking(state: &crate::types::game_state::GameState, source_id: ObjectId) -> bool {
    state.combat.as_ref().is_some_and(|combat| {
        combat
            .attackers
            .iter()
            .any(|attacker| attacker.object_id == source_id)
    })
}

fn is_source_blocking(state: &crate::types::game_state::GameState, source_id: ObjectId) -> bool {
    state
        .combat
        .as_ref()
        .is_some_and(|combat| combat.blocker_to_attacker.contains_key(&source_id))
}

fn is_source_blocked(state: &crate::types::game_state::GameState, source_id: ObjectId) -> bool {
    state
        .combat
        .as_ref()
        .and_then(|combat| combat.blocker_assignments.get(&source_id))
        .is_some_and(|blockers| !blockers.is_empty())
}

fn parse_you_control_land_subtypes(text: &str) -> Option<Vec<String>> {
    if !text.starts_with("you control an ") && !text.starts_with("you control a ") {
        return None;
    }
    let rest = text
        .strip_prefix("you control an ")
        .or_else(|| text.strip_prefix("you control a "))?;
    if !rest.contains(" or ") {
        return None;
    }
    let subtypes = rest
        .split(" or ")
        .map(|piece| {
            piece
                .trim()
                .trim_start_matches("a ")
                .trim_start_matches("an ")
                .to_string()
        })
        .collect::<Vec<_>>();
    if subtypes.len() < 2 {
        return None;
    }
    if !subtypes.iter().all(|subtype| {
        matches!(
            subtype.as_str(),
            "plains" | "island" | "swamp" | "mountain" | "forest" | "desert"
        )
    }) {
        return None;
    }
    Some(subtypes)
}

fn parse_you_control_subtype_count(text: &str) -> Option<(usize, String)> {
    let prefix = "you control ";
    let rest = text.strip_prefix(prefix)?;
    let (minimum_text, subtype_text) = rest.split_once(" or more ")?;
    let minimum = parse_count_word(minimum_text)?;

    let normalized = subtype_text.trim();
    if parse_core_type_word(normalized).is_some()
        || normalized.ends_with(" permanents")
        || normalized == "snow permanents"
    {
        return None;
    }

    let subtype = normalized.trim_end_matches('s').trim().to_string();
    Some((minimum, subtype))
}

fn parse_you_control_condition(text: &str) -> Option<RestrictionCondition> {
    if text == "you control a desert or there is a desert card in your graveyard" {
        return Some(
            RestrictionCondition::YouControlSubtypeOrGraveyardCardSubtype {
                subtype: "desert".to_string(),
            },
        );
    }
    if let Some(subtypes) = parse_you_control_land_subtypes(text) {
        return Some(RestrictionCondition::YouControlLandSubtypeAny(subtypes));
    }
    if let Some((count, subtype)) = parse_you_control_subtype_count(text) {
        return Some(RestrictionCondition::YouControlSubtypeCountAtLeast { subtype, count });
    }
    if let Some(count) = parse_numeric_threshold(
        text,
        "creatures you control have total power ",
        " or greater",
    ) {
        return Some(RestrictionCondition::CreaturesYouControlTotalPowerAtLeast(
            count as i32,
        ));
    }
    if let Some(count) = parse_numeric_threshold(
        text,
        "you control ",
        " or more creatures with different powers",
    ) {
        return Some(RestrictionCondition::YouControlDifferentPowerCreatureCountAtLeast(count));
    }
    if let Some(count) =
        parse_numeric_threshold(text, "you control ", " or more lands with the same name")
    {
        return Some(RestrictionCondition::YouControlLandsWithSameNameAtLeast(
            count,
        ));
    }
    if let Some(count) = parse_numeric_threshold(text, "you control ", " or more snow permanents") {
        return Some(RestrictionCondition::YouControlSnowPermanentCountAtLeast(
            count,
        ));
    }
    if let Some(count) = parse_numeric_threshold(text, "you control ", " or more ") {
        let suffix = text.split(" or more ").nth(1)?;
        if let Some(color) = parse_color_word(suffix.trim_end_matches(" permanents")) {
            return Some(RestrictionCondition::YouControlColorPermanentCountAtLeast {
                color,
                count,
            });
        }
        if let Some(core_type) = parse_core_type_word(suffix) {
            return Some(RestrictionCondition::YouControlCoreTypeCountAtLeast { core_type, count });
        }
    }
    if let Some(power) =
        parse_numeric_threshold(text, "you control a creature with power ", " or greater")
    {
        return Some(RestrictionCondition::YouControlCreatureWithPowerAtLeast(
            power as i32,
        ));
    }
    if let Some((power, toughness)) = parse_creature_pt_condition(text) {
        return Some(RestrictionCondition::YouControlCreatureWithPt { power, toughness });
    }
    match text {
        "you control a creature with flying" => Some(
            RestrictionCondition::YouControlCreatureWithKeyword(Keyword::Flying),
        ),
        "you control a legendary creature" => {
            Some(RestrictionCondition::YouControlLegendaryCreature)
        }
        "you control another colorless creature" => {
            Some(RestrictionCondition::YouControlAnotherColorlessCreature)
        }
        "you control fewer creatures than each opponent" => {
            Some(RestrictionCondition::QuantityVsEachOpponent {
                lhs: PlayerQuantity::ControlledCreatureCount,
                comparator: Comparator::LT,
                rhs: PlayerQuantity::ControlledCreatureCount,
            })
        }
        "you control no creatures" => Some(RestrictionCondition::YouControlNoCreatures),
        _ => {
            if let Some(name) = text
                .strip_prefix("you control an ")
                .or_else(|| text.strip_prefix("you control a "))
                .and_then(|rest| rest.strip_suffix(" planeswalker"))
            {
                return Some(RestrictionCondition::YouControlNamedPlaneswalker(
                    capitalize_condition_word(name),
                ));
            }
            if let Some(rest) = text
                .strip_prefix("you control an ")
                .or_else(|| text.strip_prefix("you control a "))
            {
                if let Some(core_type) = parse_core_type_word(rest) {
                    return Some(RestrictionCondition::YouControlCoreTypeCountAtLeast {
                        core_type,
                        count: 1,
                    });
                }
                return Some(RestrictionCondition::YouControlSubtypeCountAtLeast {
                    subtype: rest.to_string(),
                    count: 1,
                });
            }
            None
        }
    }
}

fn parse_graveyard_condition(text: &str) -> Option<RestrictionCondition> {
    if let Some(count) =
        parse_numeric_threshold(text, "there are ", " or more cards in your graveyard")
    {
        return Some(RestrictionCondition::GraveyardCardCountAtLeast(count));
    }
    if let Some(count) = parse_numeric_threshold(
        text,
        "there are ",
        " or more card types among cards in your graveyard",
    ) {
        return Some(RestrictionCondition::GraveyardCardTypeCountAtLeast(count));
    }
    if let Some(subtype) = text
        .strip_prefix("there is an ")
        .and_then(|rest| rest.strip_suffix(" card in your graveyard"))
    {
        return Some(RestrictionCondition::GraveyardSubtypeCardCountAtLeast {
            subtype: subtype.to_string(),
            count: 1,
        });
    }
    if let Some(subtype) = text
        .strip_prefix("two or more ")
        .and_then(|rest| rest.strip_suffix(" cards are in your graveyard"))
    {
        return Some(RestrictionCondition::GraveyardSubtypeCardCountAtLeast {
            subtype: subtype.trim_end_matches('s').to_string(),
            count: 2,
        });
    }
    None
}

fn parse_hand_condition(text: &str) -> Option<RestrictionCondition> {
    match text {
        "you have no cards in hand" => Some(RestrictionCondition::HandSizeExact(0)),
        "you have more cards in hand than each opponent" => {
            Some(RestrictionCondition::QuantityVsEachOpponent {
                lhs: PlayerQuantity::HandSize,
                comparator: Comparator::GT,
                rhs: PlayerQuantity::HandSize,
            })
        }
        "you have exactly zero or seven cards in hand" => {
            Some(RestrictionCondition::HandSizeOneOf(vec![0, 7]))
        }
        _ => parse_numeric_threshold(text, "you have exactly ", " cards in hand")
            .map(RestrictionCondition::HandSizeExact),
    }
}

fn parse_source_condition(text: &str) -> Option<RestrictionCondition> {
    match text {
        "from your graveyard" | "this card is in your graveyard" => {
            Some(RestrictionCondition::SourceInZone(Zone::Graveyard))
        }
        "this card is suspended" => Some(RestrictionCondition::SourceInZone(Zone::Exile)),
        "this creature is attacking" => Some(RestrictionCondition::SourceIsAttacking),
        "this creature is blocked" => Some(RestrictionCondition::SourceIsBlocked),
        "this creature is attacking or blocking" => {
            Some(RestrictionCondition::SourceIsAttackingOrBlocking)
        }
        "this permanent is a creature" => Some(RestrictionCondition::SourceIsCreature),
        "this creature entered this turn" | "this land entered this turn" => {
            Some(RestrictionCondition::SourceEnteredThisTurn)
        }
        "enchanted land is untapped" => Some(RestrictionCondition::SourceUntappedAttachedTo {
            required_type: CoreType::Land,
        }),
        "enchanted creature is untapped" => Some(RestrictionCondition::SourceUntappedAttachedTo {
            required_type: CoreType::Creature,
        }),
        "this creature doesn't have defender" => {
            Some(RestrictionCondition::SourceLacksKeyword(Keyword::Defender))
        }
        "this creature is blue" => Some(RestrictionCondition::SourceIsColor(
            crate::types::mana::ManaColor::Blue,
        )),
        _ => {
            if text == "this creature is attacking or blocking and only once each turn" {
                return Some(RestrictionCondition::SourceIsAttackingOrBlocking);
            }
            if let Some(power) =
                parse_numeric_threshold(text, "this creature's power is ", " or greater")
            {
                return Some(RestrictionCondition::SourcePowerAtLeast(power as i32));
            }
            if let Some((counter_type, count)) = parse_counter_requirement(text) {
                return Some(RestrictionCondition::SourceHasCounterAtLeast {
                    counter_type,
                    count,
                });
            }
            if let Some(counter_type) = parse_counter_absence_requirement(text) {
                return Some(RestrictionCondition::SourceHasNoCounter(counter_type));
            }
            None
        }
    }
}

fn parse_numeric_threshold(text: &str, prefix: &str, suffix: &str) -> Option<usize> {
    let middle = text.strip_prefix(prefix)?.strip_suffix(suffix)?.trim();
    parse_count_word(middle)
}

fn parse_count_word(text: &str) -> Option<usize> {
    let trimmed = text.trim();
    if trimmed == "zero" {
        return Some(0);
    }
    parse_number(trimmed).and_then(|(count, rest)| rest.is_empty().then_some(count as usize))
}

fn parse_core_type_word(text: &str) -> Option<CoreType> {
    CoreType::from_str(&capitalize_condition_word(
        text.trim().trim_end_matches('s'),
    ))
    .ok()
}

fn parse_color_word(text: &str) -> Option<ManaColor> {
    ManaColor::from_str(&capitalize_condition_word(
        text.trim().trim_end_matches('s'),
    ))
    .ok()
}

fn parse_creature_pt_condition(text: &str) -> Option<(i32, i32)> {
    let stats = text
        .strip_prefix("you control a ")
        .and_then(|rest| rest.strip_suffix(" creature"))?;
    let (power, toughness) = stats.split_once('/')?;
    Some((power.parse().ok()?, toughness.parse().ok()?))
}

fn parse_counter_requirement(text: &str) -> Option<(CounterType, u32)> {
    if let Some(counter_name) = text
        .strip_prefix("this artifact has ")
        .or_else(|| text.strip_prefix("this enchantment has "))
        .and_then(|rest| rest.strip_suffix(" counters on it"))
    {
        let (count_text, counter_name) = counter_name.split_once(" or more ")?;
        return Some((
            parse_counter_type(counter_name),
            parse_count_word(count_text)? as u32,
        ));
    }
    if let Some(counter_name) = text
        .strip_prefix("there are ")
        .and_then(|rest| rest.strip_suffix(" counters on this artifact"))
    {
        let (count_text, counter_name) = counter_name.split_once(" or more ")?;
        return Some((
            parse_counter_type(counter_name),
            parse_count_word(count_text)? as u32,
        ));
    }
    None
}

fn parse_counter_absence_requirement(text: &str) -> Option<CounterType> {
    text.strip_prefix("there are no ")
        .and_then(|rest| rest.strip_suffix(" counters on this artifact"))
        .map(parse_counter_type)
}

fn capitalize_condition_word(text: &str) -> String {
    let mut out = String::new();
    for (index, piece) in text.split_whitespace().enumerate() {
        if index > 0 {
            out.push(' ');
        }
        let mut chars = piece.chars();
        if let Some(first) = chars.next() {
            out.push(first.to_ascii_uppercase());
            out.extend(chars);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::ability::{AbilityKind, Effect, QuantityExpr};
    use crate::types::card_type::CoreType;
    use crate::types::game_state::WaitingFor;
    use crate::types::identifiers::CardId;
    use crate::types::zones::Zone;

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

        assert!(evaluate_condition_text(
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

        assert!(evaluate_condition_text(
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

        assert!(evaluate_condition_text(
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

        assert!(evaluate_condition_text(
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

        assert!(!evaluate_condition_text(
            &state,
            PlayerId(0),
            ObjectId(1),
            "you attacked this turn"
        ));

        record_attackers_declared(&mut state, 1);

        assert!(evaluate_condition_text(
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

        assert!(evaluate_condition_text(
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

        assert!(evaluate_condition_text(
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

        assert!(evaluate_condition_text(
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

        assert!(evaluate_condition_text(
            &state,
            PlayerId(0),
            ObjectId(1),
            "you have exactly zero or seven cards in hand"
        ));
    }

    #[test]
    fn evaluates_creature_died_this_turn_condition() {
        let mut state = crate::types::game_state::GameState::new_two_player(42);
        state.creature_died_this_turn = true;

        assert!(evaluate_condition_text(
            &state,
            PlayerId(0),
            ObjectId(1),
            "a creature died this turn"
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

        assert!(evaluate_condition_text(
            &state,
            PlayerId(0),
            artifact,
            "this artifact or another artifact entered the battlefield under your control this turn"
        ));
    }

    #[test]
    fn evaluates_cards_left_graveyard_this_turn_condition() {
        let mut state = crate::types::game_state::GameState::new_two_player(42);
        state.cards_left_graveyard_this_turn.insert(PlayerId(0), 3);

        assert!(evaluate_condition_text(
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

        assert!(evaluate_condition_text(
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
            },
        );

        assert!(check_spell_timing(&state, PlayerId(0), &obj, &ability, true).is_ok());
    }
}
