use std::str::FromStr;

use crate::game::game_object::GameObject;
use crate::game::zones;
use crate::parser::oracle_util::parse_subtype;
use crate::types::ability::{AbilityCost, NinjutsuVariant};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;
use crate::types::identifiers::{CardId, ObjectId};
use crate::types::keywords::{FlashbackCost, Keyword, KeywordKind, ProtectionTarget};
use crate::types::mana::ManaCost;
use crate::types::phase::Phase;
use crate::types::player::PlayerId;
use crate::types::statics::StaticMode;
use crate::types::zones::Zone;

/// Check if a game object has a specific keyword, using discriminant-based matching
/// for simple keywords (ignoring associated data for parameterized variants).
pub fn has_keyword(obj: &GameObject, keyword: &Keyword) -> bool {
    obj.keywords
        .iter()
        .any(|k| std::mem::discriminant(k) == std::mem::discriminant(keyword))
}

pub fn has_keyword_kind(obj: &GameObject, kind: KeywordKind) -> bool {
    obj.keywords.iter().any(|keyword| keyword.kind() == kind)
}

pub fn object_has_effective_keyword_kind(
    state: &GameState,
    object_id: ObjectId,
    kind: KeywordKind,
) -> bool {
    let Some(obj) = state.objects.get(&object_id) else {
        return false;
    };
    if obj.zone == Zone::Battlefield {
        return obj.keywords.iter().any(|keyword| keyword.kind() == kind);
    }

    crate::game::off_zone_characteristics::off_zone_has_keyword_kind(state, object_id, kind)
}

/// CR 702.61a: True when any spell on the stack has split second. While true,
/// players can't cast spells or activate abilities that aren't mana abilities.
pub fn stack_has_split_second(state: &GameState) -> bool {
    state.stack.iter().any(|entry| {
        state
            .objects
            .get(&entry.id)
            .is_some_and(|obj| has_keyword(obj, &Keyword::SplitSecond))
    })
}

pub fn effective_flashback_cost(state: &GameState, object_id: ObjectId) -> Option<FlashbackCost> {
    let keyword = effective_keyword_for_object(state, object_id, KeywordKind::Flashback)?;
    match keyword {
        Keyword::Flashback(cost) => match cost {
            FlashbackCost::Mana(mana_cost) => Some(FlashbackCost::Mana(resolve_keyword_mana_cost(
                state, object_id, &mana_cost,
            ))),
            FlashbackCost::NonMana(ability_cost) => Some(FlashbackCost::NonMana(ability_cost)),
        },
        _ => None,
    }
}

/// CR 702.146a: Effective Disturb alt-cost for an object in the graveyard.
pub fn effective_disturb_cost(state: &GameState, object_id: ObjectId) -> Option<ManaCost> {
    let keyword =
        effective_keyword_for_object(state, object_id, KeywordKind::Disturb).or_else(|| {
            let obj = state.objects.get(&object_id)?;
            // `snapshot_object_face` clears layout_kind; a still-unswapped DFC
            // back face retains its layout kind and must not grant Disturb.
            let stored_front_face = obj
                .back_face
                .as_ref()
                .filter(|face| face.layout_kind.is_none())?;
            stored_front_face
                .keywords
                .iter()
                .find(|keyword| keyword.kind() == KeywordKind::Disturb)
                .cloned()
        })?;
    match keyword {
        Keyword::Disturb(cost) => Some(resolve_keyword_mana_cost(state, object_id, &cost)),
        _ => None,
    }
}

pub fn effective_escape_data(state: &GameState, object_id: ObjectId) -> Option<(ManaCost, u32)> {
    let keyword = effective_keyword_for_object(state, object_id, KeywordKind::Escape)?;
    match keyword {
        Keyword::Escape { cost, exile_count } => Some((
            resolve_keyword_mana_cost(state, object_id, &cost),
            exile_count,
        )),
        _ => None,
    }
}

/// CR 702.190a: Effective Sneak alt-cost for an object, honoring off-zone characteristic
/// grants (e.g., Ninja Teen's "creature cards in your graveyard have sneak {cost}").
pub fn effective_sneak_cost(state: &GameState, object_id: ObjectId) -> Option<ManaCost> {
    let keyword = effective_keyword_for_object(state, object_id, KeywordKind::Sneak)?;
    match keyword {
        Keyword::Sneak(cost) => Some(resolve_keyword_mana_cost(state, object_id, &cost)),
        _ => None,
    }
}

/// CR 702.188a + CR 604.1: honor web-slinging GRANTED by a CastWithKeyword static
/// (Amazing Spider-Man), not only printed keywords. effective_spell_keywords merges
/// printed obj.keywords with statically-granted keywords for `caster`.
pub fn effective_web_slinging_cost(
    state: &GameState,
    caster: PlayerId,
    object_id: ObjectId,
) -> Option<ManaCost> {
    super::casting::effective_spell_keywords(state, caster, object_id)
        .into_iter()
        .find_map(|k| match k {
            Keyword::WebSlinging(cost) => Some(resolve_keyword_mana_cost(state, object_id, &cost)),
            _ => None,
        })
}

fn effective_keyword_for_object(
    state: &GameState,
    object_id: ObjectId,
    kind: KeywordKind,
) -> Option<Keyword> {
    let obj = state.objects.get(&object_id)?;
    if obj.zone == Zone::Battlefield {
        return obj
            .keywords
            .iter()
            .find(|keyword| keyword.kind() == kind)
            .cloned();
    }

    crate::game::off_zone_characteristics::effective_off_zone_keyword(state, object_id, kind)
}

fn resolve_keyword_mana_cost(state: &GameState, object_id: ObjectId, cost: &ManaCost) -> ManaCost {
    match cost {
        ManaCost::SelfManaCost => state
            .objects
            .get(&object_id)
            .map(|obj| obj.mana_cost.clone())
            .unwrap_or(ManaCost::NoCost),
        _ => cost.clone(),
    }
}

/// Convenience: check for Flying.
/// CR 702.9a: A creature with flying can't be blocked except by creatures with flying or reach.
pub fn has_flying(obj: &GameObject) -> bool {
    obj.keywords.contains(&Keyword::Flying)
}

/// Convenience: check for Haste.
/// CR 702.10a: A creature with haste can attack and activate abilities with {T} the turn it enters.
pub fn has_haste(obj: &GameObject) -> bool {
    obj.keywords.contains(&Keyword::Haste)
}

/// Convenience: check for Flash.
pub fn has_flash(obj: &GameObject) -> bool {
    obj.keywords.contains(&Keyword::Flash)
}

/// CR 702.11a: Hexproof — can't be the target of spells or abilities opponents control.
pub fn has_hexproof(obj: &GameObject) -> bool {
    obj.keywords.contains(&Keyword::Hexproof)
}

/// CR 702.18a: Shroud — can't be the target of spells or abilities.
pub fn has_shroud(obj: &GameObject) -> bool {
    obj.keywords.contains(&Keyword::Shroud)
}

/// Convenience: check for Indestructible.
/// CR 702.12a: A permanent with indestructible can't be destroyed.
pub fn has_indestructible(obj: &GameObject) -> bool {
    obj.keywords.contains(&Keyword::Indestructible)
}

/// CR 702.16b: Returns true if target's protection prevents interaction from source.
pub fn protection_prevents_from(target: &GameObject, source: &GameObject) -> bool {
    for kw in &target.keywords {
        if let Keyword::Protection(ref pt) = kw {
            if source_matches_protection_target(pt, target, source) {
                return true;
            }
        }
    }
    false
}

pub fn source_matches_protection_target(
    protection: &ProtectionTarget,
    protected: &GameObject,
    source: &GameObject,
) -> bool {
    match protection {
        ProtectionTarget::Color(color) => source.color.contains(color),
        ProtectionTarget::CardType(type_name) => source_matches_card_type(source, type_name),
        ProtectionTarget::Quality(quality) => source_matches_quality(source, quality),
        ProtectionTarget::Multicolored => source.color.len() > 1,
        ProtectionTarget::ChosenColor => protected
            .chosen_color()
            .is_some_and(|color| source.color.contains(&color)),
        // CR 702.16 + CR 205.2: "Protection from the chosen card
        // type" — resolved from the protected permanent's own chosen card type.
        // This arm only fires for objects that themselves carry the choice
        // (e.g. Serra's Emissary); creatures it grants protection to receive a
        // concrete `Protection(CardType(..))` baked in by the layer applier.
        ProtectionTarget::ChosenCardType => protected
            .chosen_card_type()
            .and_then(|ct| ct.protection_quality_str())
            .is_some_and(|quality| source_matches_card_type(source, quality)),
        // CR 702.16j: "Protection from everything" — protection from each object
        // regardless of the source's characteristic values.
        ProtectionTarget::Everything => true,
        // CR 702.16a + CR 202.3: Filter-based protection — the source matches if
        // it satisfies every FilterProp in the typed filter. Only supports
        // object-intrinsic properties (Cmc, HasColor, PowerLE/GE, etc.) that can
        // be evaluated from the source alone without game state.
        ProtectionTarget::Filter(filter) => source_matches_protection_filter(source, filter),
        // CR 702.16k: "Protection from [a player]" — the source matches if it is
        // controlled by the scoped player(s) relative to the protected object's
        // controller, regardless of the source's characteristics. "Each of your
        // opponents" (CR 702.16i) is captured by `Opponent`: any controller
        // other than the protected object's controller is an opponent in 1v1 and
        // free-for-all multiplayer. Player references with no static context
        // (target/chosen/etc.) fail closed.
        ProtectionTarget::FromPlayer(scope) => match scope {
            crate::types::ability::ControllerRef::Opponent => {
                source.controller != protected.controller
            }
            crate::types::ability::ControllerRef::You => source.controller == protected.controller,
            _ => false,
        },
    }
}

pub fn source_matches_card_type(source: &GameObject, type_name: &str) -> bool {
    use crate::types::card_type::CoreType;

    let core = &source.card_types.core_types;
    for (core_type, singular, plural) in [
        (CoreType::Artifact, "artifact", "artifacts"),
        (CoreType::Creature, "creature", "creatures"),
        (CoreType::Enchantment, "enchantment", "enchantments"),
        (CoreType::Instant, "instant", "instants"),
        (CoreType::Sorcery, "sorcery", "sorceries"),
        (CoreType::Planeswalker, "planeswalker", "planeswalkers"),
        (CoreType::Land, "land", "lands"),
    ] {
        if type_name.eq_ignore_ascii_case(singular) || type_name.eq_ignore_ascii_case(plural) {
            return core.contains(&core_type);
        }
    }

    // CR 702.16a + CR 205.3m: "protection from [creature subtype]" —
    // sources like "assassins" or "elves" are stored as CardType by the
    // parser but must match via the creature-subtype list.
    let quality = type_name.to_ascii_lowercase();
    source
        .card_types
        .subtypes
        .iter()
        .any(|st| source_subtype_matches_protection_quality(&st.to_ascii_lowercase(), &quality))
}

fn source_subtype_matches_protection_quality(source_subtype: &str, quality: &str) -> bool {
    parse_subtype(quality).is_some_and(|(subtype, consumed)| {
        consumed == quality.len() && subtype.eq_ignore_ascii_case(source_subtype)
    })
}

pub fn source_matches_quality(source: &GameObject, quality: &str) -> bool {
    match quality {
        "monocolored" => source.color.len() == 1,
        "multicolored" => source.color.len() > 1,
        _ => false,
    }
}

/// CR 702.16a + CR 202.3: Evaluate a filter-based protection predicate against
/// a source object. Tests every `FilterProp` in the typed filter's properties
/// (conjunction — all must match). Only supports object-intrinsic properties
/// that can be resolved from the source alone without game state access.
///
fn source_matches_protection_filter(
    source: &GameObject,
    filter: &crate::types::ability::TargetFilter,
) -> bool {
    use crate::types::ability::{FilterProp, QuantityExpr, TargetFilter};

    let TargetFilter::Typed(typed) = filter else {
        return false;
    };
    // All FilterProp predicates must match (conjunction).
    typed.properties.iter().all(|prop| match prop {
        // CR 202.3: Mana value comparison — only Fixed thresholds are valid
        // in protection text (no dynamic quantity refs like SelfManaValue).
        FilterProp::Cmc { comparator, value } => {
            let QuantityExpr::Fixed { value: threshold } = value else {
                return false;
            };
            comparator.evaluate(source.mana_cost.mana_value() as i32, *threshold)
        }
        // Future: other intrinsic properties (HasColor, PowerLE/GE, etc.)
        // can be added here as the class of filter-based protection grows.
        _ => false,
    })
}

/// Batch parse keyword strings into typed Keyword values.
/// Used when creating GameObjects from parsed card data.
pub fn parse_keywords(keyword_strings: &[String]) -> Vec<Keyword> {
    keyword_strings
        .iter()
        .map(|s| Keyword::from_str(s).unwrap())
        .collect()
}

/// CR 702.49: Check if the current phase allows activation of a Ninjutsu-family variant.
///
/// CR 702.190a: Sneak is intentionally absent from `NinjutsuVariant` — it is a
/// cast alt-cost handled in `casting::handle_cast_spell_as_sneak`, not an
/// activated ability — so it cannot reach this function.
pub fn ninjutsu_timing_ok(phase: &Phase, variant: &NinjutsuVariant) -> bool {
    match variant {
        // CR 702.49a/d: Ninjutsu/CommanderNinjutsu can be activated during declare blockers step or later
        NinjutsuVariant::Ninjutsu | NinjutsuVariant::CommanderNinjutsu => {
            matches!(phase, Phase::DeclareBlockers | Phase::CombatDamage)
        }
    }
}

/// CR 702.49: Return the creatures that can be returned for this variant.
/// - Ninjutsu/CommanderNinjutsu: unblocked attackers controlled by `player`
pub fn returnable_creatures_for_variant(
    state: &GameState,
    player: PlayerId,
    variant: &NinjutsuVariant,
) -> Vec<ObjectId> {
    match variant {
        NinjutsuVariant::Ninjutsu | NinjutsuVariant::CommanderNinjutsu => {
            super::combat::unblocked_attackers(state)
                .into_iter()
                .filter(|&id| {
                    state
                        .objects
                        .get(&id)
                        .is_some_and(|o| o.controller == player)
                })
                .collect()
        }
    }
}

/// CR 702.49a-c: Resolve Ninjutsu-family activation.
///
/// Validates the activation, returns the specified creature to its owner's hand,
/// and puts the Ninjutsu creature onto the battlefield tapped and attacking the
/// same player/planeswalker as the returned creature.
///
/// CR 702.49c: The creature is never "declared as an attacker" so it
/// does not fire "whenever ~ attacks" triggers.
pub fn activate_ninjutsu(
    state: &mut GameState,
    player: PlayerId,
    ninjutsu_obj_id: ObjectId,
    creature_to_return: ObjectId,
    events: &mut Vec<GameEvent>,
) -> Result<(), String> {
    // CR 903.8: Commander tax applies to casting, not to ninjutsu activation.
    let p = &state.players[player.0 as usize];
    if !p.hand.contains(&ninjutsu_obj_id) && !state.command_zone.contains(&ninjutsu_obj_id) {
        return Err("Ninjutsu-family card not in hand or command zone".to_string());
    }

    // Determine which variant from the card's keywords
    let ninjutsu_obj = state
        .objects
        .get(&ninjutsu_obj_id)
        .ok_or("Ninjutsu-family card object not found")?;
    if ninjutsu_obj.owner != player {
        return Err("You don't own that Ninjutsu-family card".to_string());
    }
    let variant = ninjutsu_family_variant(ninjutsu_obj)
        .ok_or("Card does not have a Ninjutsu-family keyword")?;
    if ninjutsu_obj.zone == Zone::Command && !matches!(variant, NinjutsuVariant::CommanderNinjutsu)
    {
        return Err("Only commander ninjutsu can be activated from the command zone".to_string());
    }

    // CR 702.49a/d: Extract the activation cost (validated after all other checks, paid before mutations)
    let mana_cost =
        ninjutsu_family_cost(ninjutsu_obj).ok_or("Ninjutsu-family card has no mana cost")?;

    // Validate timing
    if !ninjutsu_timing_ok(&state.phase, &variant) {
        return Err(format!(
            "{variant:?} can only be activated during the declare blockers step"
        ));
    }

    // Validate: must be in combat
    let combat = state.combat.as_ref().ok_or("No active combat")?;

    // Validate the creature to return based on variant (CR 702.190a: Sneak is
    // intentionally absent from `NinjutsuVariant`, so this match is exhaustive
    // without any guard against the cast-only path).
    let (defending_player, attack_target) = match variant {
        NinjutsuVariant::Ninjutsu | NinjutsuVariant::CommanderNinjutsu => {
            // Must be an unblocked attacker
            let attacker_info = combat
                .attackers
                .iter()
                .find(|a| a.object_id == creature_to_return)
                .ok_or("Specified creature is not an attacker")?
                .clone();

            let is_blocked = combat
                .blocker_assignments
                .get(&creature_to_return)
                .is_some_and(|blockers| !blockers.is_empty());
            if is_blocked {
                return Err("Attacker is blocked".to_string());
            }

            (attacker_info.defending_player, attacker_info.attack_target)
        }
    };

    // Validate: creature controlled by player
    let creature_obj = state
        .objects
        .get(&creature_to_return)
        .ok_or("Creature not found")?;
    if creature_obj.controller != player {
        return Err("You don't control that creature".to_string());
    }

    // CR 601.2f: Apply ability cost reduction from statics like Silver-Fur Master
    // CR 601.2f: All ninjutsu-family variants share the "ninjutsu" keyword for cost reduction.
    let effective_cost = apply_ability_cost_reduction(state, player, "ninjutsu", mana_cost);

    // CR 702.49a/d: Pay the ninjutsu-family mana cost (after all validation, before mutations)
    super::casting::pay_ability_cost(
        state,
        player,
        ninjutsu_obj_id,
        &AbilityCost::Mana {
            cost: effective_cost,
        },
        events,
    )
    .map_err(|e| e.to_string())?;

    // 1. Return creature to owner's hand
    zones::move_to_zone(state, creature_to_return, Zone::Hand, events);

    // Remove the returned creature from combat state if it was an attacker
    if let Some(combat) = state.combat.as_mut() {
        combat
            .attackers
            .retain(|a| a.object_id != creature_to_return);
        combat.blocker_assignments.remove(&creature_to_return);
    }

    // 2. Move Ninjutsu-family card from hand/command zone to battlefield
    zones::move_to_zone(state, ninjutsu_obj_id, Zone::Battlefield, events);

    // CR 702.49: Track which alt-cost variant was paid this turn on the
    // cast-variant-paid tag (placement + tapped + summoning sickness is
    // delegated to the shared helper).
    if let Some(obj) = state.objects.get_mut(&ninjutsu_obj_id) {
        obj.cast_variant_paid = Some((variant.into(), state.turn_number));
    }

    // CR 702.49c: Place onto combat.attackers alongside the returned creature's
    // defender WITHOUT firing AttackersDeclared (no "whenever ~ attacks" triggers).
    super::combat::place_attacking_alongside(
        state,
        ninjutsu_obj_id,
        defending_player,
        attack_target,
        events,
    );

    // CR 702.49a: Emit event for "whenever you activate a ninjutsu ability" triggers.
    events.push(GameEvent::NinjutsuActivated {
        player_id: player,
        source_id: ninjutsu_obj_id,
    });

    crate::game::layers::mark_layers_full(state);

    Ok(())
}

/// Detect which activated-family `NinjutsuVariant` a game object has, if any.
/// CR 702.190a: Sneak is a cast alt-cost handled in
/// `casting::handle_cast_spell_as_sneak`, so it does not appear in
/// `NinjutsuVariant` and is not matched here.
fn ninjutsu_family_variant(obj: &GameObject) -> Option<NinjutsuVariant> {
    for kw in &obj.keywords {
        match kw {
            Keyword::Ninjutsu(_) => return Some(NinjutsuVariant::Ninjutsu),
            Keyword::CommanderNinjutsu(_) => return Some(NinjutsuVariant::CommanderNinjutsu),
            _ => {}
        }
    }
    None
}

/// CR 702.49b: Extract the mana cost for a ninjutsu-family (activated)
/// keyword on this object. Excludes Sneak and Web-slinging because they are
/// cast alternative costs, not activated abilities.
fn ninjutsu_family_cost(obj: &GameObject) -> Option<ManaCost> {
    for kw in &obj.keywords {
        match kw {
            Keyword::Ninjutsu(c) | Keyword::CommanderNinjutsu(c) => return Some(c.clone()),
            _ => {}
        }
    }
    None
}

/// CR 601.2f: Scan battlefield for ReduceAbilityCost statics that reduce the cost
/// of a specific ability type, and apply the reduction to the given mana cost.
/// `ability_keyword` is the lowered keyword name to match (e.g., "ninjutsu", "equip").
fn apply_ability_cost_reduction(
    state: &GameState,
    player: PlayerId,
    ability_keyword: &str,
    mut cost: ManaCost,
) -> ManaCost {
    // CR 702.26b + CR 604.1: Functioning gate owned by `battlefield_active_statics`.
    for (bf_obj, static_def) in
        crate::game::functioning_abilities::battlefield_active_statics(state)
    {
        if bf_obj.controller != player {
            continue;
        }
        if let StaticMode::ReduceAbilityCost {
            ref keyword,
            amount,
            ref dynamic_count,
            ..
        } = static_def.mode
        {
            if keyword == ability_keyword {
                // CR 601.2f: When dynamic_count is present, the total reduction is
                // amount * resolve_quantity(dynamic_count). E.g., "cost {1} less for each Dragon".
                let multiplier = dynamic_count.as_ref().map_or(1u32, |qty_ref| {
                    let expr = crate::types::ability::QuantityExpr::Ref {
                        qty: qty_ref.clone(),
                    };
                    crate::game::quantity::resolve_quantity(
                        state,
                        &expr,
                        bf_obj.controller,
                        bf_obj.id,
                    )
                    .max(0) as u32
                });
                let total_reduction = amount.saturating_mul(multiplier);
                if let ManaCost::Cost {
                    ref mut generic, ..
                } = cost
                {
                    *generic = generic.saturating_sub(total_reduction);
                }
            }
        }
    }
    cost
}

/// CR 702.49a/d: Look up the source object, variant, and effective cost for
/// every Ninjutsu-family card the player may activate.
pub fn ninjutsu_family_activatable_sources(
    state: &GameState,
    player: PlayerId,
) -> Vec<(ObjectId, CardId, NinjutsuVariant, ManaCost)> {
    let p = &state.players[player.0 as usize];
    let hand_sources = p.hand.iter().filter_map(|&obj_id| {
        let obj = state.objects.get(&obj_id)?;
        let variant = ninjutsu_family_variant(obj)?;
        let cost =
            apply_ability_cost_reduction(state, player, "ninjutsu", ninjutsu_family_cost(obj)?);
        Some((obj_id, obj.card_id, variant, cost))
    });

    let command_sources = state.command_zone.iter().filter_map(|&obj_id| {
        let obj = state.objects.get(&obj_id)?;
        if obj.owner != player {
            return None;
        }
        let variant = ninjutsu_family_variant(obj)?;
        if !matches!(variant, NinjutsuVariant::CommanderNinjutsu) {
            return None;
        }
        let cost =
            apply_ability_cost_reduction(state, player, "ninjutsu", ninjutsu_family_cost(obj)?);
        Some((obj_id, obj.card_id, variant, cost))
    });

    hand_sources.chain(command_sources).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use crate::ai_support::legal_actions;
    use crate::types::ability::{
        AbilityDefinition, AbilityKind, Effect, ManaContribution, ManaProduction,
    };
    use crate::types::actions::GameAction;
    use crate::types::game_state::WaitingFor;
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType, ManaUnit};
    use crate::types::player::PlayerId;
    use crate::types::zones::Zone;

    fn make_obj() -> GameObject {
        GameObject::new(
            ObjectId(1),
            CardId(1),
            PlayerId(0),
            "Test".to_string(),
            Zone::Battlefield,
        )
    }

    #[test]
    fn has_keyword_simple_match() {
        let mut obj = make_obj();
        obj.keywords.push(Keyword::Flying);
        assert!(has_keyword(&obj, &Keyword::Flying));
        assert!(!has_keyword(&obj, &Keyword::Haste));
    }

    /// CR 702.16 + CR 205.2: `source_matches_protection_target`'s
    /// `ChosenCardType` arm resolves against the *protected* object's own
    /// chosen card type. A creature-typed source matches when the protected
    /// object chose Creature; a non-creature source does not. An object with
    /// no chosen card type is matched by nothing through this arm.
    #[test]
    fn source_matches_protection_target_chosen_card_type() {
        use crate::types::ability::ChosenAttribute;
        use crate::types::card_type::CoreType;

        let mut protected = make_obj();
        protected
            .chosen_attributes
            .push(ChosenAttribute::CardType(CoreType::Creature));

        let mut creature_source = make_obj();
        creature_source.card_types.core_types = vec![CoreType::Creature];
        let mut instant_source = make_obj();
        instant_source.card_types.core_types = vec![CoreType::Instant];

        assert!(
            source_matches_protection_target(
                &ProtectionTarget::ChosenCardType,
                &protected,
                &creature_source,
            ),
            "creature source must match protection from chosen card type Creature"
        );
        assert!(
            !source_matches_protection_target(
                &ProtectionTarget::ChosenCardType,
                &protected,
                &instant_source,
            ),
            "instant source must NOT match protection from chosen card type Creature"
        );

        // No chosen card type -> the arm protects from nothing.
        let no_choice = make_obj();
        assert!(!source_matches_protection_target(
            &ProtectionTarget::ChosenCardType,
            &no_choice,
            &creature_source,
        ));
    }

    /// CR 702.16a + CR 205.3m + #881: "protection from [creature subtype]" — the
    /// parser stores the subtype as `ProtectionTarget::CardType("assassins")`.
    /// `source_matches_card_type` must recognise creature subtypes via the
    /// source's `card_types.subtypes` list.
    #[test]
    fn source_matches_protection_from_creature_subtype() {
        let mut haytham = make_obj();
        haytham.card_types.core_types = vec![crate::types::card_type::CoreType::Creature];
        haytham
            .keywords
            .push(Keyword::Protection(ProtectionTarget::CardType(
                "assassins".to_string(),
            )));

        // An Assassin creature must match "protection from assassins".
        let mut assassin_source = make_obj();
        assassin_source.card_types.core_types = vec![crate::types::card_type::CoreType::Creature];
        assassin_source
            .card_types
            .subtypes
            .push("Assassin".to_string());

        assert!(
            source_matches_protection_target(
                &ProtectionTarget::CardType("assassins".to_string()),
                &haytham,
                &assassin_source,
            ),
            "Assassin creature must match 'protection from assassins'"
        );

        // A non-Assassin creature must NOT match.
        let mut knight_source = make_obj();
        knight_source.card_types.core_types = vec![crate::types::card_type::CoreType::Creature];
        knight_source.card_types.subtypes.push("Knight".to_string());

        assert!(
            !source_matches_protection_target(
                &ProtectionTarget::CardType("assassins".to_string()),
                &haytham,
                &knight_source,
            ),
            "Knight creature must NOT match 'protection from assassins'"
        );
    }

    /// CR 702.16a + CR 205.3m: subtype protection must understand MTG subtype
    /// plurals without corrupting singular subtypes ending in "s".
    #[test]
    fn source_matches_protection_from_irregular_creature_subtype_plurals() {
        for (quality, subtype) in [
            ("elves", "Elf"),
            ("fungi", "Fungus"),
            ("pegasus", "Pegasus"),
            ("pegasi", "Pegasus"),
            ("pegasuses", "Pegasus"),
        ] {
            let mut protected = make_obj();
            protected
                .keywords
                .push(Keyword::Protection(ProtectionTarget::CardType(
                    quality.to_string(),
                )));

            let mut source = make_obj();
            source.card_types.core_types = vec![crate::types::card_type::CoreType::Creature];
            source.card_types.subtypes.push(subtype.to_string());

            assert!(
                source_matches_protection_target(
                    &ProtectionTarget::CardType(quality.to_string()),
                    &protected,
                    &source,
                ),
                "{subtype} source must match protection from {quality}"
            );
        }
    }

    /// Issue #767 / CR 702.16k: "protection from each of your opponents"
    /// (Figure of Fable's Avatar form) — a source controlled by an opponent of
    /// the protected permanent's controller matches; a source the protected
    /// permanent's own controller controls does not.
    #[test]
    fn source_matches_protection_from_opponents() {
        use crate::types::ability::ControllerRef;
        use crate::types::player::PlayerId;

        let mut protected = make_obj();
        protected.controller = PlayerId(0);
        let mut opponent_source = make_obj();
        opponent_source.controller = PlayerId(1);
        let mut own_source = make_obj();
        own_source.controller = PlayerId(0);

        let from_opponents = ProtectionTarget::FromPlayer(ControllerRef::Opponent);
        assert!(
            source_matches_protection_target(&from_opponents, &protected, &opponent_source),
            "opponent-controlled source must match protection from each of your opponents"
        );
        assert!(
            !source_matches_protection_target(&from_opponents, &protected, &own_source),
            "own-controlled source must NOT match protection from each of your opponents"
        );

        // CR 702.16k with `You` scope is the controller-relative inverse.
        let from_you = ProtectionTarget::FromPlayer(ControllerRef::You);
        assert!(source_matches_protection_target(
            &from_you,
            &protected,
            &own_source
        ));
        assert!(!source_matches_protection_target(
            &from_you,
            &protected,
            &opponent_source
        ));
    }

    #[test]
    fn has_keyword_discriminant_matching() {
        let mut obj = make_obj();
        obj.keywords.push(Keyword::Kicker(ManaCost::Cost {
            generic: 1,
            shards: vec![ManaCostShard::Green],
        }));
        // Discriminant match -- doesn't care about the param value
        assert!(has_keyword(
            &obj,
            &Keyword::Kicker(ManaCost::Cost {
                generic: 0,
                shards: vec![ManaCostShard::Red],
            })
        ));
        assert!(!has_keyword(
            &obj,
            &Keyword::Cycling(crate::types::keywords::CyclingCost::Mana(ManaCost::Cost {
                generic: 2,
                shards: vec![],
            }))
        ));
    }

    #[test]
    fn convenience_functions() {
        let mut obj = make_obj();
        obj.keywords.push(Keyword::Flying);
        obj.keywords.push(Keyword::Haste);
        obj.keywords.push(Keyword::Flash);
        obj.keywords.push(Keyword::Hexproof);
        obj.keywords.push(Keyword::Shroud);
        obj.keywords.push(Keyword::Indestructible);

        assert!(has_flying(&obj));
        assert!(has_haste(&obj));
        assert!(has_flash(&obj));
        assert!(has_hexproof(&obj));
        assert!(has_shroud(&obj));
        assert!(has_indestructible(&obj));
    }

    #[test]
    fn protection_from_instants_prevents_damage() {
        let mut protected = make_obj();
        protected
            .keywords
            .push(Keyword::Protection(ProtectionTarget::CardType(
                "instants".to_string(),
            )));

        let mut source = make_obj();
        source
            .card_types
            .core_types
            .push(crate::types::card_type::CoreType::Instant);

        assert!(protection_prevents_from(&protected, &source));
    }

    #[test]
    fn protection_from_display_case_artifact_matches_artifact_source() {
        let mut protected = make_obj();
        protected
            .keywords
            .push(Keyword::Protection(ProtectionTarget::CardType(
                "Artifact".to_string(),
            )));

        let mut source = make_obj();
        source
            .card_types
            .core_types
            .push(crate::types::card_type::CoreType::Artifact);

        assert!(protection_prevents_from(&protected, &source));
    }

    /// CR 702.16a + CR 202.3: Protection from mana value 3 or less prevents
    /// interaction from sources with mana value <= 3 and allows sources with
    /// mana value > 3.
    #[test]
    fn protection_from_mana_value_filter() {
        use crate::types::ability::{
            Comparator, FilterProp, QuantityExpr, TargetFilter, TypedFilter,
        };

        let mut protected = make_obj();
        protected
            .keywords
            .push(Keyword::Protection(ProtectionTarget::Filter(
                TargetFilter::Typed(TypedFilter::default().properties(vec![FilterProp::Cmc {
                    comparator: Comparator::LE,
                    value: QuantityExpr::Fixed { value: 3 },
                }])),
            )));

        // Source with mana value 2 (≤ 3) — should be prevented
        let mut source_low = make_obj();
        source_low.mana_cost = ManaCost::Cost {
            generic: 2,
            shards: vec![],
        };
        assert!(
            protection_prevents_from(&protected, &source_low),
            "MV 2 source should be prevented by protection from MV 3 or less"
        );

        // Source with mana value 3 (≤ 3) — should be prevented
        let mut source_exact = make_obj();
        source_exact.mana_cost = ManaCost::Cost {
            generic: 3,
            shards: vec![],
        };
        assert!(
            protection_prevents_from(&protected, &source_exact),
            "MV 3 source should be prevented by protection from MV 3 or less"
        );

        // Source with mana value 4 (> 3) — should NOT be prevented
        let mut source_high = make_obj();
        source_high.mana_cost = ManaCost::Cost {
            generic: 4,
            shards: vec![],
        };
        assert!(
            !protection_prevents_from(&protected, &source_high),
            "MV 4 source should NOT be prevented by protection from MV 3 or less"
        );

        // Source with mana value 0 (≤ 3) — should be prevented (tokens, lands)
        let source_zero = make_obj();
        assert!(
            protection_prevents_from(&protected, &source_zero),
            "MV 0 source should be prevented by protection from MV 3 or less"
        );
    }

    /// CR 702.16a + CR 202.3: Protection from mana value 3 or greater prevents
    /// interaction from sources with mana value >= 3.
    #[test]
    fn protection_from_mana_value_greater() {
        use crate::types::ability::{
            Comparator, FilterProp, QuantityExpr, TargetFilter, TypedFilter,
        };

        let mut protected = make_obj();
        protected
            .keywords
            .push(Keyword::Protection(ProtectionTarget::Filter(
                TargetFilter::Typed(TypedFilter::default().properties(vec![FilterProp::Cmc {
                    comparator: Comparator::GE,
                    value: QuantityExpr::Fixed { value: 3 },
                }])),
            )));

        // Source with mana value 2 (< 3) — should NOT be prevented
        let mut source_low = make_obj();
        source_low.mana_cost = ManaCost::Cost {
            generic: 2,
            shards: vec![],
        };
        assert!(
            !protection_prevents_from(&protected, &source_low),
            "MV 2 source should NOT be prevented by protection from MV 3 or greater"
        );

        // Source with mana value 4 (≥ 3) — should be prevented
        let mut source_high = make_obj();
        source_high.mana_cost = ManaCost::Cost {
            generic: 4,
            shards: vec![],
        };
        assert!(
            protection_prevents_from(&protected, &source_high),
            "MV 4 source should be prevented by protection from MV 3 or greater"
        );
    }

    #[test]
    fn parse_keywords_known() {
        let strings = vec![
            "Flying".to_string(),
            "Haste".to_string(),
            "Deathtouch".to_string(),
        ];
        let parsed = parse_keywords(&strings);
        assert_eq!(
            parsed,
            vec![Keyword::Flying, Keyword::Haste, Keyword::Deathtouch]
        );
    }

    #[test]
    fn parse_keywords_parameterized() {
        let strings = vec!["Kicker:1G".to_string(), "Ward:2".to_string()];
        let parsed = parse_keywords(&strings);
        assert_eq!(
            parsed[0],
            Keyword::Kicker(ManaCost::Cost {
                generic: 1,
                shards: vec![ManaCostShard::Green],
            })
        );
        assert_eq!(
            parsed[1],
            Keyword::Ward(crate::types::keywords::WardCost::Mana(ManaCost::Cost {
                generic: 2,
                shards: vec![],
            }))
        );
    }

    #[test]
    fn parse_keywords_unknown() {
        let strings = vec!["NotReal".to_string()];
        let parsed = parse_keywords(&strings);
        assert_eq!(parsed[0], Keyword::Unknown("NotReal".to_string()));
    }

    #[test]
    fn has_keyword_method_on_game_object() {
        let mut obj = make_obj();
        obj.keywords.push(Keyword::Indestructible);
        assert!(obj.has_keyword(&Keyword::Indestructible));
        assert!(!obj.has_keyword(&Keyword::Flying));
    }

    use crate::game::combat::{AttackerInfo, CombatState};
    use crate::game::zones::create_object;
    use crate::types::events::GameEvent;
    use crate::types::game_state::GameState;

    fn add_mana_land(state: &mut GameState, card_id: CardId, color: ManaColor) -> ObjectId {
        let land_id = create_object(
            state,
            card_id,
            PlayerId(0),
            "Test Land".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&land_id).unwrap();
        obj.card_types
            .core_types
            .push(crate::types::card_type::CoreType::Land);
        Arc::make_mut(&mut obj.abilities).push(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Mana {
                    produced: ManaProduction::Fixed {
                        colors: vec![color],
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
        land_id
    }

    fn setup_ninjutsu_scenario() -> (GameState, ObjectId, ObjectId) {
        let mut state = GameState::new_two_player(42);
        state.active_player = PlayerId(0);
        state.turn_number = 2;
        state.phase = crate::types::phase::Phase::DeclareBlockers;

        // Create an attacker on battlefield
        let attacker_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Grizzly Bears".to_string(),
            crate::types::zones::Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&attacker_id).unwrap();
            obj.card_types
                .core_types
                .push(crate::types::card_type::CoreType::Creature);
            obj.power = Some(2);
            obj.toughness = Some(2);
            obj.tapped = true;
            obj.entered_battlefield_turn = Some(1); // no summoning sickness
        }

        // Set up combat state with attacker unblocked
        state.combat = Some(CombatState {
            attackers: vec![AttackerInfo::attacking_player(attacker_id, PlayerId(1))],
            ..Default::default()
        });

        // Create Ninjutsu creature in hand
        let ninja_id = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Ninja of the Deep Hours".to_string(),
            crate::types::zones::Zone::Hand,
        );
        {
            let obj = state.objects.get_mut(&ninja_id).unwrap();
            obj.card_types
                .core_types
                .push(crate::types::card_type::CoreType::Creature);
            obj.power = Some(2);
            obj.toughness = Some(2);
            obj.keywords.push(Keyword::Ninjutsu(ManaCost::Cost {
                generic: 1,
                shards: vec![ManaCostShard::Blue],
            }));
            obj.base_keywords = obj.keywords.clone();
        }

        // Add mana for ninjutsu activation cost ({1}{U})
        for color in [ManaType::Blue, ManaType::Colorless] {
            state.players[0].mana_pool.add(ManaUnit {
                color,
                source_id: ObjectId(0),
                snow: false,
                source_could_produce_two_or_more_colors: false,
                restrictions: Vec::new(),
                grants: vec![],
                expiry: None,
            });
        }

        (state, attacker_id, ninja_id)
    }

    #[test]
    fn ninjutsu_returns_attacker_to_hand() {
        let (mut state, attacker_id, ninja_id) = setup_ninjutsu_scenario();
        let mut events = Vec::new();

        activate_ninjutsu(&mut state, PlayerId(0), ninja_id, attacker_id, &mut events)
            .expect("activation should succeed");

        // Attacker should be in hand
        let attacker = state.objects.get(&attacker_id).unwrap();
        assert_eq!(
            attacker.zone,
            crate::types::zones::Zone::Hand,
            "Attacker should be returned to hand"
        );
    }

    #[test]
    fn ninjutsu_creature_enters_battlefield_tapped_attacking() {
        let (mut state, attacker_id, ninja_id) = setup_ninjutsu_scenario();
        let mut events = Vec::new();

        activate_ninjutsu(&mut state, PlayerId(0), ninja_id, attacker_id, &mut events)
            .expect("activation should succeed");

        // Ninjutsu creature should be on battlefield, tapped, attacking
        let ninja = state.objects.get(&ninja_id).unwrap();
        assert_eq!(ninja.zone, crate::types::zones::Zone::Battlefield);
        assert!(ninja.tapped, "Ninjutsu creature should be tapped");
        assert_eq!(
            ninja.entered_battlefield_turn,
            Some(state.turn_number),
            "Should have summoning sickness"
        );

        // Should be in combat attackers
        let combat = state.combat.as_ref().unwrap();
        assert!(
            combat.attackers.iter().any(|a| a.object_id == ninja_id),
            "Ninjutsu creature should be in attackers list"
        );
        // Should be attacking same player as returned attacker
        let ninja_attacker = combat
            .attackers
            .iter()
            .find(|a| a.object_id == ninja_id)
            .unwrap();
        assert_eq!(
            ninja_attacker.defending_player,
            PlayerId(1),
            "Should attack same player"
        );
    }

    #[test]
    fn ninjutsu_creature_does_not_fire_attack_triggers() {
        let (mut state, attacker_id, ninja_id) = setup_ninjutsu_scenario();
        let mut events = Vec::new();

        activate_ninjutsu(&mut state, PlayerId(0), ninja_id, attacker_id, &mut events)
            .expect("activation should succeed");

        // CR 702.49c: No AttackersDeclared event should be emitted for the Ninjutsu creature
        let has_attackers_declared = events
            .iter()
            .any(|e| matches!(e, GameEvent::AttackersDeclared { .. }));
        assert!(
            !has_attackers_declared,
            "No AttackersDeclared event should fire for Ninjutsu creature"
        );
    }

    #[test]
    fn ninjutsu_fails_if_attacker_is_blocked() {
        let (mut state, attacker_id, ninja_id) = setup_ninjutsu_scenario();

        // Add a blocker assignment
        let blocker_id = create_object(
            &mut state,
            CardId(3),
            PlayerId(1),
            "Wall".to_string(),
            crate::types::zones::Zone::Battlefield,
        );
        state
            .combat
            .as_mut()
            .unwrap()
            .blocker_assignments
            .insert(attacker_id, vec![blocker_id]);

        let mut events = Vec::new();
        let result = activate_ninjutsu(&mut state, PlayerId(0), ninja_id, attacker_id, &mut events);
        assert!(result.is_err(), "Should fail when attacker is blocked");
    }

    #[test]
    fn ninjutsu_fails_without_combat() {
        let (mut state, attacker_id, ninja_id) = setup_ninjutsu_scenario();
        state.combat = None; // Remove combat

        let mut events = Vec::new();
        let result = activate_ninjutsu(&mut state, PlayerId(0), ninja_id, attacker_id, &mut events);
        assert!(result.is_err(), "Should fail without active combat");
    }

    #[test]
    fn ninjutsu_activation_fails_without_mana() {
        let (mut state, attacker_id, ninja_id) = setup_ninjutsu_scenario();
        // Clear all mana
        state.players[0].mana_pool.clear();

        let mut events = Vec::new();
        let result = activate_ninjutsu(&mut state, PlayerId(0), ninja_id, attacker_id, &mut events);
        assert!(result.is_err(), "Should fail without mana");

        // Verify no zone changes occurred — creature still on battlefield, ninja still in hand
        let attacker = state.objects.get(&attacker_id).unwrap();
        assert_eq!(
            attacker.zone,
            Zone::Battlefield,
            "Attacker should not have moved"
        );
        let ninja = state.objects.get(&ninja_id).unwrap();
        assert_eq!(ninja.zone, Zone::Hand, "Ninja should still be in hand");
    }

    #[test]
    fn ninjutsu_activation_deducts_mana() {
        let (mut state, attacker_id, ninja_id) = setup_ninjutsu_scenario();
        let mut events = Vec::new();

        activate_ninjutsu(&mut state, PlayerId(0), ninja_id, attacker_id, &mut events)
            .expect("activation should succeed");

        // Mana pool should be empty after paying {1}{U}
        assert_eq!(
            state.players[0].mana_pool.total(),
            0,
            "Mana pool should be empty after ninjutsu payment"
        );
    }

    #[test]
    fn ninjutsu_legal_action_uses_auto_tappable_mana_sources() {
        let (mut state, attacker_id, ninja_id) = setup_ninjutsu_scenario();
        state.players[0].mana_pool.clear();
        state.priority_player = PlayerId(0);
        state.waiting_for = WaitingFor::Priority {
            player: PlayerId(0),
        };

        add_mana_land(&mut state, CardId(10), ManaColor::Blue);
        add_mana_land(&mut state, CardId(11), ManaColor::Blue);

        let actions = legal_actions(&state);

        assert!(
            actions.iter().any(|a| matches!(
                a,
                GameAction::ActivateNinjutsu {
                    ninjutsu_object_id,
                    creature_to_return,
                } if *ninjutsu_object_id == ninja_id && *creature_to_return == attacker_id
            )),
            "Ninjutsu should be legal when untapped mana sources can pay the cost"
        );

        let (_, _, grouped) = crate::ai_support::legal_actions_full(&state);
        assert!(
            grouped
                .get(&ninja_id)
                .is_some_and(|actions| actions.iter().any(|a| matches!(
                    a,
                    GameAction::ActivateNinjutsu {
                        ninjutsu_object_id,
                        creature_to_return,
                    } if *ninjutsu_object_id == ninja_id && *creature_to_return == attacker_id
                ))),
            "Ninjutsu should be grouped under the hand object for frontend playability"
        );
    }
}
