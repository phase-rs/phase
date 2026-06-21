use std::str::FromStr;

use crate::game::combat::AttackTarget;
use crate::game::game_object::GameObject;
use crate::game::zone_pipeline::{self, ZoneMoveRequest};
use crate::parser::oracle_util::parse_subtype;
use crate::types::ability::{AbilityCost, CastVariantPaid, NinjutsuVariant};
use crate::types::events::GameEvent;
use crate::types::game_state::{GameState, WaitingFor};
use crate::types::identifiers::{CardId, ObjectId};
use crate::types::keywords::{
    EmbalmCost, EternalizeCost, FlashbackCost, Keyword, KeywordKind, ProtectionTarget,
};
use crate::types::mana::ManaCost;
use crate::types::phase::Phase;
use crate::types::player::PlayerId;
use crate::types::statics::StaticMode;
use crate::types::zones::Zone;

/// Check if a game object has a specific keyword, using discriminant-based matching
/// for simple keywords (ignoring associated data for parameterized variants).
///
/// Object-scoped: reads the post-layer `obj.keywords` list, which is only
/// authoritative for battlefield objects. For an object that can be in hand,
/// graveyard, exile, or on the stack, use
/// [`object_has_effective_keyword_kind`] — it consults off-zone keyword
/// grants that this function cannot see.
pub fn has_keyword(obj: &GameObject, keyword: &Keyword) -> bool {
    // allow-raw-authority: this IS the object-scoped authority
    obj.keywords
        .iter()
        .any(|k| std::mem::discriminant(k) == std::mem::discriminant(keyword))
}

/// Object-scoped keyword-kind query — same battlefield-only caveat as
/// [`has_keyword`]; prefer [`object_has_effective_keyword_kind`] for objects
/// that can be off-battlefield.
pub fn has_keyword_kind(obj: &GameObject, kind: KeywordKind) -> bool {
    // allow-raw-authority: this IS the object-scoped authority
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

/// CR 702.138a: Resolve an object's effective escape cost into its mana sub-cost
/// (paid via the normal mana flow, CR 601.2g) and the residual additional cost
/// (usually "Exile N other cards from your graveyard", possibly a Composite of
/// several exile clauses; paid via `pay_additional_cost`, CR 601.2h).
///
/// A printed escape card always carries at least the graveyard-exile sub-cost,
/// and the granted/native compound (parser) always carries the exile residual.
/// Only a `FromStr`/JSON placeholder (`EscapeCost::Mana` with no residual) or a
/// parse failure yields no residual — refuse those (return `None`) so the
/// mis-parse surfaces rather than producing an illegal cost-free escape cast.
pub fn effective_escape_data(
    state: &GameState,
    object_id: ObjectId,
) -> Option<(ManaCost, AbilityCost)> {
    let keyword = effective_keyword_for_object(state, object_id, KeywordKind::Escape)?;
    match keyword {
        Keyword::Escape(escape) => {
            let (mana, residual) = super::casting::split_escape_cost_components(&escape);
            // CR 702.138a: a real escape cost always has a non-mana residual
            // (the exile sub-cost). No residual ⇒ unparsed placeholder ⇒ refuse.
            let residual = residual?;
            let mana =
                resolve_keyword_mana_cost(state, object_id, &mana.unwrap_or(ManaCost::NoCost));
            Some((mana, residual))
        }
        _ => None,
    }
}

/// CR 702.164b: A creature's total toxic value is the sum of N over ALL its
/// effective toxic instances (printed + granted, on or off the battlefield).
/// Sums over the plural `effective_off_zone_keywords` primitive (battlefield →
/// `obj.keywords`; off-battlefield → off-zone continuous-effect resolution),
/// matching the effective view used by the sibling `object_has_effective_keyword_kind`
/// flags rather than reading printed `obj.keywords` directly. (Toxic has no
/// distinct `KeywordKind` — it collapses to `Unknown` — so the sum is taken over
/// the `Keyword::Toxic` variant, not a kind filter.)
pub fn effective_total_toxic_value(state: &GameState, object_id: ObjectId) -> u32 {
    crate::game::off_zone_characteristics::effective_off_zone_keywords(state, object_id)
        .iter()
        .filter_map(|keyword| match keyword {
            Keyword::Toxic(amount) => Some(*amount),
            _ => None,
        })
        .sum()
}

/// CR 702.187b: Effective Mayhem alt-cost for a card in the graveyard, honoring
/// off-zone characteristic grants (e.g. Green Goblin's "Each nonland card in
/// your graveyard has mayhem. The mayhem cost is equal to its mana cost.") in
/// addition to a printed Mayhem keyword. The availability gate ("discarded this
/// turn") is checked separately by the caster, not here.
pub fn effective_mayhem_cost(state: &GameState, object_id: ObjectId) -> Option<ManaCost> {
    let keyword = effective_keyword_for_object(state, object_id, KeywordKind::Mayhem)?;
    match keyword {
        Keyword::Mayhem(cost) => Some(resolve_keyword_mana_cost(state, object_id, &cost)),
        _ => None,
    }
}

/// CR 702.180a: Effective Harmonize alt-cost for a card in the graveyard,
/// honoring off-zone keyword grants (e.g. Songcrafter Mage's "target instant or
/// sorcery card in your graveyard gains harmonize until end of turn. Its
/// harmonize cost is equal to its mana cost.") in addition to a printed Harmonize
/// keyword. Resolves `ManaCost::SelfManaCost` to the card's own mana cost via
/// `resolve_keyword_mana_cost`, mirroring `effective_mayhem_cost`. The
/// tap-a-creature cost reduction (CR 702.180a/b) is applied separately at
/// payment time.
pub fn effective_harmonize_cost(state: &GameState, object_id: ObjectId) -> Option<ManaCost> {
    let keyword = effective_keyword_for_object(state, object_id, KeywordKind::Harmonize)?;
    match keyword {
        Keyword::Harmonize(cost) => Some(resolve_keyword_mana_cost(state, object_id, &cost)),
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

/// CR 702.128a / CR 702.129a + CR 602.1a: Resolve a `ManaCost::SelfManaCost`
/// payload carried by a granted Embalm / Eternalize keyword to the recipient
/// card's concrete mana cost. Embalm / Eternalize are graveyard *activated*
/// abilities whose mana sub-cost is paid through `AbilityCost::Mana`, and that
/// payment path treats `SelfManaCost` as a free cost — so a self-cost grant must
/// be concretized before the activated ability is synthesized. Non-self-cost
/// keywords (printed Embalm `{3}{W}`, or any other keyword) pass through
/// unchanged.
pub fn resolve_self_cost_graveyard_activated_keyword(
    state: &GameState,
    object_id: ObjectId,
    keyword: &Keyword,
) -> Keyword {
    match keyword {
        Keyword::Embalm(EmbalmCost::Mana(cost)) => Keyword::Embalm(EmbalmCost::Mana(
            resolve_keyword_mana_cost(state, object_id, cost),
        )),
        Keyword::Eternalize(EternalizeCost::Mana(cost)) => Keyword::Eternalize(
            EternalizeCost::Mana(resolve_keyword_mana_cost(state, object_id, cost)),
        ),
        other => other.clone(),
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

    // Validate: creature controlled by player and still on the battlefield.
    // CR 506.4 + CR 702.49a: A creature that died or left combat before the
    // declare blockers step cannot be returned by Ninjutsu.
    let creature_obj = state
        .objects
        .get(&creature_to_return)
        .ok_or("Creature not found")?;
    if creature_obj.controller != player {
        return Err("You don't control that creature".to_string());
    }
    if !super::combat::is_attacker_in_play(state, creature_to_return) {
        return Err("Attacker is no longer on the battlefield".to_string());
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
    // CR 702.49a + CR 614.6: ninjutsu returns the unblocked attacker to its
    // owner's hand. This return is part of the ninjutsu activation COST (CR
    // 702.49a: "Return an unblocked attacking creature you control to its owner's
    // hand" is one of the ability's costs), so the move is attributed via
    // `ZoneMoveRequest::cost`, not `effect`.
    //
    // Route through the zone-change pipeline so a board-wide `Moved` "would leave
    // the battlefield → ... instead" redirect is consulted. No DESTINATION-GATED
    // (`destination_zone(Hand)`) Moved replacement exists in the current pool, but
    // a `destination_zone: None` wildcard CAN match this battlefield→hand move:
    // notably unearth (CR 702.84a, `database/unearth.rs`) installs a SelfRef
    // "if it would leave the battlefield, exile it instead" redirect, so an
    // unearthed attacker returned by ninjutsu is now correctly redirected to EXILE
    // instead of hand (the raw mover this replaced silently violated CR 702.84a).
    // The consult also future-proofs the site per the single-entry principle.
    // Attributed to the ninja entering the battlefield. Hand destination has no
    // counters, so a Hand-landing delivery cannot pause; a redirect to a
    // non-pausing zone (exile) is likewise `Done`. Assert `Done` rather than
    // discarding the result so a future reachable pause trips tests instead of
    // silently executing past a parked prompt.
    let return_result = zone_pipeline::move_object(
        state,
        ZoneMoveRequest::cost(creature_to_return, Zone::Hand, ninjutsu_obj_id),
        events,
    );
    debug_assert!(
        matches!(return_result, zone_pipeline::ZoneMoveResult::Done),
        "ninjutsu return must not pause (Hand has no counters; redirects land in non-pausing zones today)"
    );

    // Remove the returned creature from combat state if it was an attacker
    if let Some(combat) = state.combat.as_mut() {
        combat
            .attackers
            .retain(|a| a.object_id != creature_to_return);
        combat.blocker_assignments.remove(&creature_to_return);
    }

    // 2. Move Ninjutsu-family card from hand/command zone to battlefield.
    //
    // CR 614.1c: route the entry through the zone-change pipeline so the
    // delivery tail applies enters-with-counters statics ("creatures you
    // control enter with an additional +1/+1 counter" — Hardened Scales /
    // Conclave Mentor class) to the entering ninja; the raw `move_to_zone`
    // skipped that tail, so the ninja entered without them. CR 400.7 attributes
    // the entry to the ninja itself (the pre-pipeline raw move recorded no
    // source; the cast-variant tag below records the ninjutsu provenance).
    //
    // CR 616.1: a battlefield-entry pause IS reachable here — two co-played
    // external enter tap-state `Moved` effects writing in *opposite* directions
    // (one enters tapped, one enters untapped — the Frozen Aether + Spelunking
    // class) are last-applied-wins, a material CR 616.1e/f collision that
    // surfaces an ordering prompt (same-direction writes commute, no prompt —
    // see replacement.rs `CommuteClass::EnterTapped`/`EnterUntapped`) (see
    // `paused_ninjutsu_entry_resumes_with_combat_placement_and_tag`). On the
    // pause, the post-entry ninjutsu work (cast-variant tag + CR 702.49c combat
    // placement + CR 702.49a trigger event) is deferred onto a
    // `BatchCompletion::NinjutsuPlacement` so the replacement-choice resume
    // runs it exactly once after the entry delivers — the old bail skipped it,
    // leaving the resumed ninja untagged and non-attacking.
    let ninjutsu_placement = crate::types::game_state::BatchCompletion::NinjutsuPlacement {
        player,
        ninjutsu_obj_id,
        cast_variant: variant.into(),
        defending_player,
        attack_target,
    };
    match super::zone_pipeline::move_object(
        state,
        super::zone_pipeline::ZoneMoveRequest::effect(
            ninjutsu_obj_id,
            Zone::Battlefield,
            ninjutsu_obj_id,
        ),
        events,
    ) {
        super::zone_pipeline::ZoneMoveResult::Done => {
            // CR 707.9: `move_object` can return `Done` while the delivery tail
            // already surfaced `CopyTargetChoice` (enter-as-copy). Defer combat
            // placement until the copy target resolves — same contract as the
            // `NeedsChoice` arms below.
            if ninjutsu_entry_paused_on_mid_entry_choice(state) {
                super::zone_pipeline::defer_completion_on_pause(state, ninjutsu_placement);
                return Ok(());
            }
        }
        super::zone_pipeline::ZoneMoveResult::NeedsChoice(_)
        | super::zone_pipeline::ZoneMoveResult::NeedsAuraAttachmentChoice => {
            super::zone_pipeline::defer_completion_on_pause(state, ninjutsu_placement);
            return Ok(());
        }
    }

    finish_ninjutsu_entry(
        state,
        player,
        ninjutsu_obj_id,
        variant.into(),
        defending_player,
        attack_target,
        events,
    );

    Ok(())
}

/// CR 614.12a + CR 707.9: True when the ninja's battlefield entry paused on an
/// interactive mid-entry choice and post-entry ninjutsu work must wait.
fn ninjutsu_entry_paused_on_mid_entry_choice(state: &GameState) -> bool {
    matches!(
        state.waiting_for,
        WaitingFor::CopyTargetChoice { .. }
            | WaitingFor::ReplacementChoice { .. }
            | WaitingFor::EffectZoneChoice { .. }
            | WaitingFor::ReturnAsAuraTarget { .. }
            | WaitingFor::ChooseOneOfBranch { .. }
            | WaitingFor::NamedChoice { .. }
    )
}

/// CR 702.49 + CR 702.49a + CR 702.49c: Post-entry ninjutsu work, run exactly
/// once after the ninja's battlefield entry delivers — inline on the
/// synchronous path, or from `BatchCompletion::NinjutsuPlacement` when the
/// entry parked on a CR 616.1 replacement-ordering choice and resumed.
pub(crate) fn finish_ninjutsu_entry(
    state: &mut GameState,
    player: PlayerId,
    ninjutsu_obj_id: ObjectId,
    cast_variant: CastVariantPaid,
    defending_player: PlayerId,
    attack_target: AttackTarget,
    events: &mut Vec<GameEvent>,
) {
    // Arrival gate (twin of `finish_attraction_open`'s CR 701.51c gate): the
    // cast-variant tag and the CR 702.49c combat placement are battlefield
    // semantics — `ZoneMoveResult::Done` also covers prevented/redirected
    // deliveries, so running them unconditionally would tag a non-battlefield
    // object and place it into `combat.attackers`. Unreachable today (no
    // supported `Moved` redirect targets a battlefield entry's destination
    // away from the battlefield), but the gate keeps the helper correct by
    // construction rather than by census.
    if state
        .objects
        .get(&ninjutsu_obj_id)
        .is_some_and(|obj| obj.zone == Zone::Battlefield)
    {
        // CR 702.49: Track which alt-cost variant was paid this turn on the
        // cast-variant-paid tag (placement + tapped + summoning sickness is
        // delegated to the shared helper).
        if let Some(obj) = state.objects.get_mut(&ninjutsu_obj_id) {
            obj.cast_variant_paid = Some((cast_variant, state.turn_number));
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
    }

    // CR 702.49a: Emit event for "whenever you activate a ninjutsu ability"
    // triggers. Deliberately OUTSIDE the arrival gate, unlike the Attraction
    // twin's `AttractionOpened`: CR 701.51c explicitly suppresses the "opens an
    // Attraction" trigger when the entry is prevented/replaced, but ninjutsu's
    // activation event occurred when the ability was activated (cost paid,
    // attacker returned) — a redirected entry does not un-activate it.
    events.push(GameEvent::NinjutsuActivated {
        player_id: player,
        source_id: ninjutsu_obj_id,
    });

    crate::game::layers::mark_layers_full(state);
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

    /// CR 702.164b: a creature's total toxic value is the sum of N over ALL its
    /// toxic instances. `effective_total_toxic_value` must enumerate every
    /// instance (here a distinct `Toxic(2)` + `Toxic(1)`) and sum to 3, rather
    /// than collapsing to the first match.
    #[test]
    fn effective_total_toxic_value_sums_all_instances() {
        let mut state = GameState::new_two_player(1);
        let id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Toxic Creature".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.keywords.push(Keyword::Toxic(2));
        obj.keywords.push(Keyword::Toxic(1));

        assert_eq!(
            effective_total_toxic_value(&state, id),
            3,
            "total toxic value sums all distinct instances"
        );

        // A creature with no toxic has total toxic value 0.
        let plain = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Plain".to_string(),
            Zone::Battlefield,
        );
        assert_eq!(effective_total_toxic_value(&state, plain), 0);
    }

    /// CR 702.164b (regression for issue #955): a *granted* Toxic 1 (e.g. Skrelv)
    /// applied on top of a *printed* Toxic 1 (e.g. Jawbone Duelist) must sum to a
    /// total toxic value of 2 — both instances remain on the keyword list so the
    /// aggregate reader counts every copy. This drives the real layer-6 grant
    /// pipeline (`add_transient_continuous_effect` + `evaluate_layers`), exercising
    /// the `AddKeyword` summing branch end-to-end. Pre-fix the layer-6 dedup
    /// (`!obj.keywords.contains(&kw)`) dropped the identical granted Toxic(1) and
    /// this asserted 1, not 2.
    #[test]
    fn granted_toxic_stacks_with_printed_toxic_via_layers() {
        use crate::types::ability::{ContinuousModification, Duration, TargetFilter};

        let mut state = GameState::new_two_player(1);

        // Printed Toxic 1 creature (the recipient of the grant).
        let creature = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Jawbone Duelist".to_string(),
            Zone::Battlefield,
        );
        // `evaluate_layers` resets `obj.keywords = obj.base_keywords.clone()` each
        // pass, so the printed Toxic must live in `base_keywords` to survive the
        // reset and be present when the layer-6 grant is applied on top of it.
        let obj = state.objects.get_mut(&creature).unwrap();
        obj.base_keywords.push(Keyword::Toxic(1));
        obj.keywords.push(Keyword::Toxic(1));

        // Grant source (stands in for Skrelv granting Toxic 1).
        let source = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Skrelv".to_string(),
            Zone::Battlefield,
        );

        // CR 613.1f layer-6 ability-adding grant of an identical Toxic 1.
        state.add_transient_continuous_effect(
            source,
            PlayerId(0),
            Duration::UntilEndOfTurn,
            TargetFilter::SpecificObject { id: creature },
            vec![ContinuousModification::AddKeyword {
                keyword: Keyword::Toxic(1),
            }],
            None,
        );

        crate::game::layers::evaluate_layers(&mut state);

        // The aggregate reader must see BOTH instances summed (1 + 1 = 2).
        assert_eq!(
            effective_total_toxic_value(&state, creature),
            2,
            "CR 702.164b: granted Toxic 1 must sum with printed Toxic 1 to total 2"
        );

        // Sub-assert: the keyword list physically holds two Toxic instances after
        // the grant (the printed one + the granted one), not a deduped single.
        let toxic_count = state.objects[&creature]
            .keywords
            .iter()
            .filter(|kw| matches!(kw, Keyword::Toxic(_)))
            .count();
        assert_eq!(
            toxic_count, 2,
            "both printed and granted Toxic instances must remain on the keyword list"
        );
    }

    /// CR 702.138a: a bare-mana escape with no exile residual is a parse failure
    /// / `FromStr` placeholder, not a legal cost-free escape. `effective_escape_data`
    /// must refuse it (return `None`) so the mis-parse can't produce an illegal
    /// 0-cost escape cast, while a well-parsed compound cost (mana + exile
    /// residual) passes through with its mana sub-cost resolved and the residual
    /// returned.
    #[test]
    fn effective_escape_data_refuses_bare_mana_no_residual() {
        use crate::types::keywords::EscapeCost;

        let escape_mana = ManaCost::Cost {
            generic: 2,
            shards: vec![ManaCostShard::Black],
        };
        let make_escape_obj = |state: &mut GameState, escape: EscapeCost| {
            let id = create_object(
                state,
                CardId(1),
                PlayerId(0),
                "Escape Test".to_string(),
                Zone::Battlefield,
            );
            let obj = state.objects.get_mut(&id).unwrap();
            obj.keywords.push(Keyword::Escape(escape));
            id
        };

        // Bare-mana placeholder (no exile residual) -> refused.
        let mut state = GameState::new_two_player(1);
        let bare_id = make_escape_obj(&mut state, EscapeCost::Mana(escape_mana.clone()));
        assert_eq!(effective_escape_data(&state, bare_id), None);

        // Well-parsed compound (mana + exile residual) -> mana resolved, residual returned.
        for n in [1u32, 2, 5] {
            let mut state = GameState::new_two_player(1);
            let exile = AbilityCost::Exile {
                count: n,
                zone: Some(Zone::Graveyard),
                filter: None,
            };
            let compound = AbilityCost::Composite {
                costs: vec![
                    AbilityCost::Mana {
                        cost: escape_mana.clone(),
                    },
                    exile.clone(),
                ],
            };
            let id = make_escape_obj(&mut state, EscapeCost::NonMana(compound));
            assert_eq!(
                effective_escape_data(&state, id),
                Some((escape_mana.clone(), exile)),
                "compound escape with exile count {n} must resolve mana + residual",
            );
        }
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
                supertype: None,
                source_could_produce_two_or_more_colors: false,
                restrictions: Vec::new(),
                grants: vec![],
                expiry: None,
            });
        }

        (state, attacker_id, ninja_id)
    }

    /// CR 702.49c + CR 616.1 discriminating test (fail-first): a ninja whose
    /// battlefield entry parks on a replacement-ordering prompt (two opposite-
    /// direction enter tap-state `Moved` effects — one enters tapped, one enters
    /// untapped: the Frozen Aether + Spelunking class, last-applied-wins and so a
    /// material CR 616.1e/f collision) must, after
    /// the prompt is answered, still receive the FULL post-entry ninjutsu work:
    /// the CR 702.49c tapped-and-attacking combat placement and the CR 702.49
    /// cast-variant provenance tag. The old bail skipped both — the resumed
    /// ninja entered untagged and non-attacking.
    #[test]
    fn paused_ninjutsu_entry_resumes_with_combat_placement_and_tag() {
        use crate::game::engine::apply_as_current;
        use crate::types::ability::{ReplacementDefinition, TargetFilter};
        use crate::types::replacements::ReplacementEvent;

        let (mut state, attacker_id, ninja_id) = setup_ninjutsu_scenario();

        // A genuinely *material* enter tap-state collision: one replacement makes
        // the entering ninja enter tapped (Frozen Aether class), the other makes
        // it enter untapped (Spelunking / Archelos class). Opposite directions
        // are last-applied-wins, so CR 616.1e/f requires the controller to order
        // them and the entry parks on a ReplacementChoice. (Two same-direction
        // writes commute — see replacement.rs `CommuteClass::EnterTapped`/`EnterUntapped`.)
        for (offset, name, state_change) in [
            (
                0u64,
                "Frozen Aether",
                crate::types::ability::TapStateChange::Tap,
            ),
            (
                1,
                "Spelunking",
                crate::types::ability::TapStateChange::Untap,
            ),
        ] {
            let oid = ObjectId(9000 + offset);
            let mut src = GameObject::new(
                oid,
                CardId(900 + offset),
                PlayerId(1),
                name.to_string(),
                Zone::Battlefield,
            );
            src.replacement_definitions = vec![ReplacementDefinition::new(ReplacementEvent::Moved)
                .execute(AbilityDefinition::new(
                    AbilityKind::Spell,
                    Effect::SetTapState {
                        target: TargetFilter::SelfRef,
                        scope: crate::types::ability::EffectScope::Single,
                        state: state_change,
                    },
                ))
                .destination_zone(Zone::Battlefield)
                .description(name.to_string())]
            .into();
            state.objects.insert(oid, src);
            state.battlefield.push_back(oid);
        }

        let mut events = Vec::new();
        activate_ninjutsu(&mut state, PlayerId(0), ninja_id, attacker_id, &mut events)
            .expect("activation should succeed");

        // CR 616.1: the colliding tap/untap (opposite-direction) writes parked
        // the ninja's entry.
        let WaitingFor::ReplacementChoice {
            player: chooser, ..
        } = state.waiting_for.clone()
        else {
            panic!(
                "expected parked ReplacementChoice for the tap/untap collision, got {:?}",
                state.waiting_for
            );
        };
        assert_eq!(
            state.objects[&ninja_id].zone,
            Zone::Hand,
            "ninja entry must be parked, not delivered, while the prompt is live"
        );
        state.priority_player = chooser;

        apply_as_current(&mut state, GameAction::ChooseReplacement { index: 0 })
            .expect("resume replacement choice");

        let ninja = &state.objects[&ninja_id];
        assert_eq!(
            ninja.zone,
            Zone::Battlefield,
            "entry delivered after resume"
        );
        assert!(
            state
                .combat
                .as_ref()
                .is_some_and(|c| c.attackers.iter().any(|a| a.object_id == ninja_id)),
            "resumed ninja must be placed attacking (CR 702.49c) — the old bail skipped combat placement"
        );
        assert!(
            ninja.cast_variant_paid.is_some(),
            "resumed ninja must carry the ninjutsu cast-variant tag (CR 702.49)"
        );
    }

    /// CR 702.49 + CR 707.9 (issue #3662): Sakashima's Student — ninjutsu entry
    /// with an optional enter-as-copy replacement must surface `CopyTargetChoice`
    /// and defer CR 702.49c combat placement until the copy target is chosen.
    #[test]
    fn ninjutsu_enter_as_copy_defers_combat_placement_until_copy_resolves() {
        use crate::game::engine::apply_as_current;
        use crate::game::zones::create_object;
        use crate::types::ability::{
            AbilityDefinition, AbilityKind, ContinuousModification, Effect, ReplacementDefinition,
            ReplacementMode, TargetFilter, TargetRef, TypeFilter, TypedFilter,
        };
        use crate::types::card_type::CoreType;
        use crate::types::replacements::ReplacementEvent;

        let (mut state, attacker_id, ninja_id) = setup_ninjutsu_scenario();

        let bear_id = create_object(
            &mut state,
            CardId(10),
            PlayerId(1),
            "Bear".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&bear_id).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.power = Some(4);
            obj.toughness = Some(4);
        }

        {
            let obj = state.objects.get_mut(&ninja_id).unwrap();
            obj.replacement_definitions.push(
                ReplacementDefinition::new(ReplacementEvent::Moved)
                    .valid_card(TargetFilter::SelfRef)
                    .destination_zone(Zone::Battlefield)
                    .mode(ReplacementMode::Optional { decline: None })
                    .execute(AbilityDefinition::new(
                        AbilityKind::Spell,
                        Effect::BecomeCopy {
                            target: TargetFilter::Typed(TypedFilter::new(TypeFilter::Creature)),
                            duration: None,
                            mana_value_limit: None,
                            additional_modifications: vec![ContinuousModification::AddSubtype {
                                subtype: "Ninja".to_string(),
                            }],
                        },
                    )),
            );
        }

        state.waiting_for = WaitingFor::Priority {
            player: PlayerId(0),
        };
        state.priority_player = PlayerId(0);

        apply_as_current(
            &mut state,
            GameAction::ActivateNinjutsu {
                ninjutsu_object_id: ninja_id,
                creature_to_return: attacker_id,
            },
        )
        .expect("ninjutsu activation should succeed");

        assert!(
            !state
                .combat
                .as_ref()
                .is_some_and(|c| c.attackers.iter().any(|a| a.object_id == ninja_id)),
            "combat placement must not run before the copy target is chosen"
        );

        if matches!(state.waiting_for, WaitingFor::ReplacementChoice { .. }) {
            apply_as_current(&mut state, GameAction::ChooseReplacement { index: 0 })
                .expect("accept enter-as-copy");
        }

        let WaitingFor::CopyTargetChoice { valid_targets, .. } = state.waiting_for.clone() else {
            panic!(
                "expected CopyTargetChoice after ninjutsu entry, got {:?}",
                state.waiting_for
            );
        };
        assert!(
            valid_targets.contains(&bear_id),
            "opponent's Bear must be a legal copy target"
        );

        apply_as_current(
            &mut state,
            GameAction::ChooseTarget {
                target: Some(TargetRef::Object(bear_id)),
            },
        )
        .expect("choose copy target");

        assert!(
            state
                .combat
                .as_ref()
                .is_some_and(|c| c.attackers.iter().any(|a| a.object_id == ninja_id)),
            "ninja must be placed attacking after copy + ninjutsu placement complete"
        );
        let ninja = &state.objects[&ninja_id];
        assert_eq!(ninja.power, Some(4), "ninja must copy Bear's power");
        assert_eq!(ninja.toughness, Some(4), "ninja must copy Bear's toughness");
        assert!(
            ninja.card_types.subtypes.iter().any(|s| s == "Ninja"),
            "copy exception must retain Ninja subtype"
        );
        assert!(
            ninja.cast_variant_paid.is_some(),
            "ninjutsu cast-variant tag must be applied after deferred placement"
        );
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

    /// CR 702.84a + CR 702.49a + CR 614.6 (discriminating): an unearthed attacker
    /// returned by ninjutsu must be redirected to EXILE, not hand. Unearth installs
    /// a SelfRef "if it would leave the battlefield, exile it instead of putting it
    /// anywhere else" `Moved` replacement (`destination_zone: None` wildcard) — the
    /// ninjutsu return (battlefield → hand) is a battlefield-exit, so the consult
    /// fires and the attacker lands in exile. This pins the rules fix that
    /// routing the ninjutsu return through `move_object`'s replacement consult
    /// enables (the prior raw mover dropped to hand, silently violating CR 702.84a).
    #[test]
    fn unearthed_attacker_returned_by_ninjutsu_is_exiled_not_returned_to_hand() {
        use crate::types::ability::{ReplacementDefinition, TargetFilter};
        use crate::types::replacements::ReplacementEvent;
        use crate::types::zones::EtbTapState;

        let (mut state, attacker_id, ninja_id) = setup_ninjutsu_scenario();

        // Install the unearth leaves-battlefield redirect on the attacker
        // (mirrors `database/unearth.rs::leaves_battlefield_exile_step`): a
        // SelfRef-bound `Moved` replacement with NO `destination_zone` gate, so it
        // matches any battlefield-exit including this battlefield → hand return.
        {
            let attacker = state.objects.get_mut(&attacker_id).unwrap();
            attacker.replacement_definitions =
                vec![ReplacementDefinition::new(ReplacementEvent::Moved)
                    .valid_card(TargetFilter::SelfRef)
                    .execute(AbilityDefinition::new(
                        AbilityKind::Spell,
                        Effect::ChangeZone {
                            origin: Some(Zone::Battlefield),
                            destination: Zone::Exile,
                            target: TargetFilter::SelfRef,
                            owner_library: false,
                            enter_transformed: false,
                            enters_under: None,
                            enter_tapped: EtbTapState::Unspecified,
                            enters_attacking: false,
                            up_to: false,
                            enter_with_counters: vec![],
                            face_down_profile: None,
                        },
                    ))]
                .into();
        }

        let mut events = Vec::new();
        activate_ninjutsu(&mut state, PlayerId(0), ninja_id, attacker_id, &mut events)
            .expect("activation should succeed");

        let attacker = state.objects.get(&attacker_id).unwrap();
        assert_eq!(
            attacker.zone,
            Zone::Exile,
            "CR 702.84a: the unearth redirect must send the returned attacker to exile, not hand"
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
    fn ninjutsu_legal_with_mana_already_in_pool() {
        let (mut state, attacker_id, ninja_id) = setup_ninjutsu_scenario();
        state.priority_player = PlayerId(0);
        state.waiting_for = WaitingFor::Priority {
            player: PlayerId(0),
        };

        let actions = legal_actions(&state);

        assert!(
            actions.iter().any(|a| matches!(
                a,
                GameAction::ActivateNinjutsu {
                    ninjutsu_object_id,
                    creature_to_return,
                } if *ninjutsu_object_id == ninja_id && *creature_to_return == attacker_id
            )),
            "Ninjutsu must be legal when the activation cost is already in the mana pool"
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
