use crate::database::synthesis::KeywordTriggerInstaller;
use crate::database::CardDatabase;
use crate::types::ability::{
    AbilityDefinition, ConjureSource, CopiableValues, Effect, PtValue, QuantityExpr,
    ReplacementCondition, ReplacementDefinition, ReplacementMode, RestrictionExpiry,
    StaticDefinition, TargetFilter, TriggerDefinition,
};
// `VoteSubject` is NOT re-imported here: `mod tests`'s only use of it
// (`crate::types::ability::VoteSubject::Named`) is fully qualified, so a gated
// import would be an `unused_imports` error.
#[cfg(test)]
use crate::types::ability::CounterSourceRider;
#[cfg(test)]
use crate::types::ability_visit::visit_effect;
use crate::types::ability_visit::{
    visit_ability_def, visit_replacement, visit_static, visit_trigger,
};
use crate::types::card::{CardFace, CardLayout, LayoutKind, PrintedCardRef, PrintedLoyalty};
use crate::types::card_type::{CardType, CoreType};
use crate::types::counter::CounterType;
use crate::types::game_state::{GameState, MeldPairRecord};
use crate::types::identifiers::ObjectId;
use crate::types::keywords::Keyword;
use crate::types::mana::{ManaColor, ManaCost, ManaCostShard};
use crate::types::replacements::ReplacementEvent;
use crate::types::zones::Zone;
use std::collections::HashMap;
use std::ops::ControlFlow;
use std::sync::Arc;

use super::game_object::{BackFaceData, GameObject};
use super::morph::apply_face_down_creature_characteristics;
use super::public_state::{
    bump_state_revision, finalize_public_state, mark_public_state_all_dirty,
};

/// CR 205.3m: Look up printed core types for a card name from deck-pool faces or
/// the card-face registry when a runtime `GameObject` lacks characteristic data.
pub fn printed_core_types_for_name<'a>(state: &'a GameState, name: &str) -> Option<&'a [CoreType]> {
    let key = name.to_lowercase();
    if let Some(face) = state.card_face_registry.get(&key) {
        return Some(&face.card_type.core_types);
    }
    for pool in &state.deck_pools {
        for entries in [
            pool.registered_main.as_ref(),
            pool.registered_sideboard.as_ref(),
            pool.current_main.as_ref(),
            pool.current_sideboard.as_ref(),
            pool.registered_companion.as_ref(),
            pool.current_companion.as_ref(),
            pool.registered_commander.as_ref(),
            pool.current_commander.as_ref(),
        ] {
            for entry in entries {
                if entry.card.name.eq_ignore_ascii_case(name) {
                    return Some(&entry.card.card_type.core_types);
                }
            }
        }
    }
    None
}

/// CR 205.3m + CR 608.2c: Whether an object matches a core card type, including
/// printed-type fallback for name-only library objects (issue #1604 class).
pub fn object_has_core_type(state: &GameState, object_id: ObjectId, card_type: CoreType) -> bool {
    let Some(obj) = state.objects.get(&object_id) else {
        return false;
    };
    if obj.card_types.core_types.contains(&card_type) {
        return true;
    }
    printed_core_types_for_name(state, &obj.name).is_some_and(|types| types.contains(&card_type))
}

pub fn printed_ref_from_face(card_face: &CardFace) -> Option<PrintedCardRef> {
    card_face
        .scryfall_oracle_id
        .as_ref()
        .map(|oracle_id| PrintedCardRef {
            oracle_id: oracle_id.clone(),
            face_name: card_face.name.clone(),
        })
}

fn printed_colors_from_face(card_face: &CardFace) -> Vec<ManaColor> {
    if let Some(colors) = &card_face.color_override {
        return colors.clone();
    }
    // CR 702.114a + CR 604.3: Devoid is a characteristic-defining ability
    // ("this object is colorless") that functions in all zones. MTGJSON normally
    // supplies `color_override: Some([])` for devoid cards, so this branch is only
    // a missing-data backstop; explicit color overrides remain authoritative.
    if card_face
        .keywords
        .iter()
        .any(|k| matches!(k, Keyword::Devoid))
    {
        return Vec::new();
    }
    derive_colors_from_mana_cost(&card_face.mana_cost)
}

pub fn apply_card_face_to_object(obj: &mut GameObject, card_face: &CardFace) {
    // CR 716.2b: capture the pre-call init flag so we can distinguish
    // first-time face application from re-application by
    // `rehydrate_game_from_card_db`. Used below to gate `class_level` seeding.
    let was_initialized = obj.base_characteristics_initialized;

    let power = parse_pt(&card_face.power);
    let toughness = parse_pt(&card_face.toughness);
    let printed_loyalty = PrintedLoyalty::from_raw(card_face.loyalty.as_deref());
    let loyalty = printed_loyalty.map(PrintedLoyalty::off_stack_value);
    // CR 310.4a: Printed defense number for battles.
    let defense = card_face
        .defense
        .as_ref()
        .and_then(|value| value.parse::<u32>().ok());
    let keywords = card_face.keywords.clone();
    let color = printed_colors_from_face(card_face);

    obj.name = card_face.name.clone();
    obj.power = power;
    obj.toughness = toughness;
    // CR 306.5b: `obj.loyalty` here is the face's printed loyalty, stored as
    // base data. The live loyalty-counter map is seeded only when the object
    // enters the battlefield, through the CR 614.1c intrinsic replacement
    // channel (`enter_with_counters` on the ZoneChange ProposedEvent).
    obj.loyalty = loyalty;
    obj.printed_loyalty = printed_loyalty;
    // CR 310.4a: `obj.defense` is the face's printed defense, stored as base
    // data. Defense counters are seeded through the CR 614.1c intrinsic
    // replacement when the battle enters the battlefield.
    obj.defense = defense;
    obj.card_types = card_face.card_type.clone();
    obj.mana_cost = card_face.mana_cost.clone();
    obj.keywords = keywords.clone();
    let mut abilities = card_face.abilities.clone();
    for ability in &mut abilities {
        ability.normalize_parsed_replacement_flags();
    }
    let mut replacements = card_face.replacements.clone();
    for replacement in &mut replacements {
        replacement.fix_legacy_parse_time_consumed_flag();
    }
    obj.abilities = Arc::new(abilities.clone());
    obj.replacement_definitions = replacements.clone().into();
    obj.static_definitions = card_face.static_abilities.clone().into();
    // CR 702.148a-b: Carry the cleave-cost ability set onto the object so the
    // casting flow can swap it in when the spell is cast for its cleave cost.
    obj.cleave_variant = card_face.cleave_variant.clone();
    obj.color = color.clone();
    obj.base_power = power;
    obj.base_toughness = toughness;
    obj.layer_base_power = power;
    obj.layer_base_toughness = toughness;
    obj.base_name = card_face.name.clone();
    obj.base_loyalty = loyalty;
    obj.base_printed_loyalty = printed_loyalty;
    obj.base_defense = defense;
    obj.base_card_types = card_face.card_type.clone();
    obj.base_mana_cost = card_face.mana_cost.clone();
    obj.base_keywords = keywords;
    obj.base_abilities = Arc::new(abilities);
    let trigger_definitions = Arc::new(card_face.triggers.clone());
    if !was_initialized {
        obj.base_trigger_definitions = trigger_definitions;
        obj.materialize_base_trigger_definitions();
    } else if obj.base_trigger_definitions.as_ref() == card_face.triggers.as_slice() {
        // Rehydrating the same face must preserve the recorded base-set
        // generation; payload equality here is only an intentional-face
        // restoration discriminator, never a live trigger identity decision.
        obj.materialize_base_trigger_definitions();
    } else {
        obj.install_trigger_base_definitions(trigger_definitions)
            .expect("trigger base-set generation must not overflow");
    }
    obj.base_replacement_definitions = Arc::new(replacements);
    obj.base_static_definitions = Arc::new(card_face.static_abilities.clone());
    obj.base_color = color;
    obj.base_characteristics_initialized = true;
    obj.printed_ref = printed_ref_from_face(card_face);
    // Display-identity baseline: the layer reset restores `printed_ref` from
    // this each pass (see `game_object::base_printed_ref`).
    obj.base_printed_ref = obj.printed_ref.clone();
    obj.source_related_token_ids = card_face.metadata.related_token_ids.clone();
    obj.spellbook = card_face.metadata.spellbook.clone();
    // Evidence that this face's printed text did not parse cleanly. Carried onto
    // the object so a consumer can tell "this card has no such ability" apart from
    // "the parser could not read that clause".
    obj.parse_warnings = card_face.parse_warnings.clone();
    obj.modal = card_face.modal.clone();
    obj.additional_cost = card_face.additional_cost.clone();
    obj.strive_cost = card_face.strive_cost.clone();
    obj.casting_restrictions = card_face.casting_restrictions.clone();
    obj.casting_options = card_face.casting_options.clone();

    // CR 716.2b: "A level is a designation that any permanent can have. A
    // Class retains its level even if it stops being a Class. Levels are not
    // a copiable characteristic." — once a Class advances past level 1, that
    // level must persist for as long as the permanent stays on the
    // battlefield. `apply_card_face_to_object` is invoked both for first-time
    // face application (deck loading, conjure, scenario seed) AND by
    // `rehydrate_game_from_card_db`, which iterates every object on state
    // load / multiplayer state-sync. Gating on the pre-call value of
    // `base_characteristics_initialized` (`was_initialized`) ensures the
    // level-1 seed runs only on first-time application; subsequent
    // rehydration preserves the runtime level. Re-entry resets are handled
    // separately by `reset_for_battlefield_entry` per CR 400.7.
    // CR 716.3: Each Class enchantment enters the battlefield at level 1.
    if !was_initialized && card_face.card_type.subtypes.iter().any(|s| s == "Class") {
        obj.class_level = Some(1);
    }

    // Digital-only Alchemy: stamp "Starting intensity N" onto the object. Gated
    // on `intensity == 0` (not `!was_initialized`) so a DFC whose starting
    // intensity lives on the back face still picks it up on transform, while
    // re-stamping a card that has already accumulated intensity never resets it.
    if obj.intensity == 0 {
        if let Some(n) = card_face.keywords.iter().find_map(|k| match k {
            crate::types::keywords::Keyword::StartingIntensity(n) => Some(*n),
            _ => None,
        }) {
            obj.intensity = n;
        }
    }

    // CR 306.5c + CR 310.4c: Rehydration must not clobber live counter-tracked
    // loyalty/defense. `rehydrate_game_from_card_db` re-applies printed faces
    // mid-game (multiplayer sync); the counter map is authoritative on the
    // battlefield, while off-battlefield loyalty/defense intentionally remains
    // the printed value per CR 306.5a / CR 310.4a.
    if was_initialized && obj.zone == Zone::Battlefield {
        if let Some(&loyalty_counters) = obj.counters.get(&CounterType::Loyalty) {
            obj.loyalty = Some(loyalty_counters);
        }
        if let Some(&defense_counters) = obj.counters.get(&CounterType::Defense) {
            obj.defense = Some(defense_counters);
        }
    }

    // CR 719.1: Initialize Case solve state from the card face.
    if card_face.card_type.subtypes.iter().any(|s| s == "Case") {
        if let Some(ref sc) = card_face.solve_condition {
            obj.case_state = Some(super::game_object::CaseState {
                is_solved: false,
                solve_condition: sc.clone(),
            });
        }
    }
    if card_face.card_type.subtypes.iter().any(|s| s == "Room") {
        obj.room_unlocks.get_or_insert_with(Default::default);
    }
    if card_face
        .card_type
        .subtypes
        .iter()
        .any(|s| s.eq_ignore_ascii_case("Attraction"))
    {
        obj.attraction_lights = if card_face.attraction_lights.is_empty() {
            super::attractions::default_attraction_lights()
        } else {
            card_face.attraction_lights.clone()
        };
    }
}

pub fn apply_card_face_to_back_face(back_face: &mut BackFaceData, card_face: &CardFace) {
    let power = parse_pt(&card_face.power);
    let toughness = parse_pt(&card_face.toughness);
    let printed_loyalty = PrintedLoyalty::from_raw(card_face.loyalty.as_deref());
    let loyalty = printed_loyalty.map(PrintedLoyalty::off_stack_value);
    // CR 310.4a: Back-face printed defense for DFCs that transform into battles.
    let defense = card_face
        .defense
        .as_ref()
        .and_then(|value| value.parse::<u32>().ok());
    let color = printed_colors_from_face(card_face);

    back_face.name = card_face.name.clone();
    back_face.power = power;
    back_face.toughness = toughness;
    back_face.loyalty = loyalty;
    back_face.printed_loyalty = printed_loyalty;
    back_face.defense = defense;
    back_face.card_types = card_face.card_type.clone();
    back_face.mana_cost = card_face.mana_cost.clone();
    back_face.keywords = card_face.keywords.clone();
    back_face.abilities = card_face.abilities.clone();
    back_face.trigger_definitions = card_face.triggers.clone().into();
    back_face.replacement_definitions = card_face.replacements.clone().into();
    back_face.static_definitions = card_face.static_abilities.clone().into();
    back_face.color = color;
    back_face.printed_ref = printed_ref_from_face(card_face);
    back_face.modal = card_face.modal.clone();
    back_face.additional_cost = card_face.additional_cost.clone();
    back_face.strive_cost = card_face.strive_cost.clone();
    back_face.casting_restrictions = card_face.casting_restrictions.clone();
    back_face.casting_options = card_face.casting_options.clone();
    // Same copy, same reason, as `apply_card_face_to_object`: evidence that THIS
    // face's printed text did not parse cleanly travels with the face.
    back_face.parse_warnings = card_face.parse_warnings.clone();
}

pub fn apply_back_face_to_object(obj: &mut GameObject, back_face: BackFaceData) {
    obj.name = back_face.name.clone();
    obj.power = back_face.power;
    obj.toughness = back_face.toughness;
    obj.loyalty = back_face.loyalty;
    obj.printed_loyalty = back_face.printed_loyalty;
    obj.defense = back_face.defense;
    obj.card_types = back_face.card_types.clone();
    obj.mana_cost = back_face.mana_cost.clone();
    obj.keywords = back_face.keywords.clone();
    obj.abilities = Arc::new(back_face.abilities.clone());
    obj.replacement_definitions = back_face.replacement_definitions.clone();
    obj.static_definitions = back_face.static_definitions.clone();
    obj.color = back_face.color.clone();
    obj.base_power = back_face.power;
    obj.base_toughness = back_face.toughness;
    obj.layer_base_power = back_face.power;
    obj.layer_base_toughness = back_face.toughness;
    obj.base_name = back_face.name.clone();
    obj.base_loyalty = back_face.loyalty;
    obj.base_printed_loyalty = back_face.printed_loyalty;
    obj.base_defense = back_face.defense;
    obj.base_card_types = back_face.card_types;
    obj.base_mana_cost = back_face.mana_cost.clone();
    obj.base_keywords = back_face.keywords;
    obj.base_abilities = Arc::new(back_face.abilities);
    let trigger_definitions = Arc::new(back_face.trigger_definitions.iter_all().cloned().collect());
    obj.install_trigger_base_definitions(trigger_definitions)
        .expect("trigger base-set generation must not overflow");
    obj.base_replacement_definitions = Arc::new(
        back_face
            .replacement_definitions
            .iter_all()
            .cloned()
            .collect(),
    );
    obj.base_static_definitions =
        Arc::new(back_face.static_definitions.iter_all().cloned().collect());
    obj.base_color = back_face.color;
    obj.base_characteristics_initialized = true;
    // Display-identity baseline tracks the now-displayed face. Cloned BEFORE the
    // move below, which consumes `back_face.printed_ref`.
    obj.base_printed_ref = back_face.printed_ref.clone();
    obj.printed_ref = back_face.printed_ref;
    obj.modal = back_face.modal;
    obj.additional_cost = back_face.additional_cost;
    obj.strive_cost = back_face.strive_cost;
    obj.casting_restrictions = back_face.casting_restrictions;
    obj.casting_options = back_face.casting_options;
    // The displayed face's diagnostics replace the outgoing face's. Both
    // directions matter and both are this one line: a back face the parser could
    // not fully read starts gating here, and transforming back off it stops.
    obj.parse_warnings = back_face.parse_warnings;
}

/// CR 306.5b + CR 310.4b + CR 614.1c: Seed the intrinsic "enters with N
/// counters" replacement for planeswalkers (loyalty counters equal to printed
/// loyalty) and battles (defense counters equal to printed defense).
///
/// Returned as `(counter_type, count)` entries suitable for pushing
/// onto `ProposedEvent::ZoneChange::enter_with_counters`. The replacement
/// pipeline then dispatches each entry through `add_counter_with_replacement`
/// so Doubling Season / Hardened Scales / Vorinclex apply per CR 614.1a.
///
/// Returns an empty vec for non-planeswalker, non-battle permanents or when
/// the face carries no printed loyalty/defense number.
/// CR 306.5b + CR 310.4b: A planeswalker enters with loyalty counters equal to
/// its printed loyalty; a battle enters with defense counters equal to its
/// printed defense. Computes those intrinsic counters from the loyalty/defense
/// values of the face the permanent will have *on entry* — the caller passes
/// the entering face's values, which is the back face for a transformed entry
/// (CR 712.14a) or the copied permanent's values for a token copy (CR 707.2).
/// Keeping this separate from [`intrinsic_etb_counters`] lets every entry path
/// (cast, effect-driven entry, play, transform-return, token-copy) seed the
/// counter map — the single source of truth for loyalty (CR 306.5c) — without
/// duplicating the rule.
pub fn intrinsic_face_counters(
    loyalty: Option<u32>,
    defense: Option<u32>,
) -> Vec<(CounterType, u32)> {
    let mut counters = Vec::new();
    if let Some(loy) = loyalty {
        if loy > 0 {
            counters.push((CounterType::Loyalty, loy));
        }
    }
    if let Some(def) = defense {
        if def > 0 {
            counters.push((CounterType::Defense, def));
        }
    }
    counters
}

/// CR 714.3a: A Saga entering the battlefield puts a lore counter on it.
fn intrinsic_saga_lore_counter(card_types: &CardType) -> Option<(CounterType, u32)> {
    if card_types.subtypes.iter().any(|s| s == "Saga") {
        Some((CounterType::Lore, 1))
    } else {
        None
    }
}

/// CR 306.5b + CR 310.4b + CR 714.3a: Intrinsic counters for the face a
/// permanent will have on entry — loyalty/defense from the entering face plus
/// the Saga lore counter when the entering face is a Saga (CR 712.14a
/// transformed entry reads the back face here before the physical swap).
pub fn intrinsic_entry_counters_for_face(
    printed_loyalty: Option<PrintedLoyalty>,
    fallback_loyalty: Option<u32>,
    resolving_spell_x: Option<u32>,
    defense: Option<u32>,
    card_types: &CardType,
) -> Vec<(CounterType, u32)> {
    // `printed_loyalty` is authoritative when present: in particular, an
    // explicit printed X must remain zero outside the resolving-spell path.
    // Older serialized objects and lightweight engine constructors predate that
    // provenance field, but their fixed `loyalty` baseline is still the printed
    // loyalty number required by CR 306.5b.
    let loyalty = printed_loyalty
        .map(|value| value.entry_counter_count(resolving_spell_x))
        .or(fallback_loyalty);
    let mut counters = intrinsic_face_counters(loyalty, defense);
    if let Some(lore) = intrinsic_saga_lore_counter(card_types) {
        counters.push(lore);
    }
    counters
}

pub fn intrinsic_etb_counters(
    obj: &GameObject,
    resolving_spell_x: Option<u32>,
) -> Vec<(CounterType, u32)> {
    let loyalty = obj
        .printed_loyalty
        .map(|value| value.entry_counter_count(resolving_spell_x))
        .or(obj.loyalty);
    let mut counters = intrinsic_face_counters(loyalty, obj.defense);
    // CR 702.156a + CR 107.3m: Ravenous is an intrinsic ETB replacement
    // effect. The paid X is stamped on the object when the spell leaves the
    // stack, before the ZoneChange replacement pipeline applies counters.
    if obj.has_keyword(&Keyword::Ravenous) {
        if let Some(x_paid) = obj.cost_x_paid {
            if x_paid > 0 {
                counters.push((CounterType::Plus1Plus1, x_paid));
            }
        }
    }
    counters
}

/// CR 614.1c: The counters a permanent enters with from a "~ enters with N
/// <type> counters on it" ability (Atraxa's Skitterfang → three oil counters;
/// Hangarback Walker / Walking Ballista copies → +1/+1). The parser models this
/// as a `Moved`→Battlefield replacement whose `execute` puts counters on the
/// entering object itself.
///
/// Cast/play/effect entries apply these by running the replacement during the
/// ZoneChange pass. The token-copy path, however, builds the object directly on
/// the battlefield (CR 707.2) and never runs that pass, so it must seed these
/// counters the same way it seeds intrinsic loyalty (`intrinsic_face_counters`).
/// This extracts them from the copiable replacement set so a token copy of an
/// "enters with counters" creature enters with them.
///
/// Only the unconditional, mandatory, fixed-count self form is recognized:
/// variable or conditional counts ("a +1/+1 counter for each artifact you
/// control") need resolution context this static extraction lacks, and on
/// non-copy entries the normal replacement pass still handles every form.
pub fn self_etb_counter_replacements(
    replacements: &[ReplacementDefinition],
) -> Vec<(CounterType, u32)> {
    replacements
        .iter()
        .filter_map(|repl| {
            if repl.event != ReplacementEvent::Moved
                || repl.destination_zone != Some(Zone::Battlefield)
                || !matches!(repl.mode, ReplacementMode::Mandatory)
                || repl.condition.is_some()
                || !matches!(repl.valid_card, Some(TargetFilter::SelfRef))
            {
                return None;
            }
            let Effect::PutCounter {
                counter_type,
                count: QuantityExpr::Fixed { value },
                target: TargetFilter::SelfRef,
            } = &*repl.execute.as_ref()?.effect
            else {
                return None;
            };
            (*value > 0).then(|| (counter_type.clone(), *value as u32))
        })
        .collect()
}

pub fn intrinsic_copiable_values(obj: &GameObject) -> CopiableValues {
    // CR 707.2 + CR 710.2: a flipped flip permanent's `base_*` fields hold the
    // ALTERNATIVE half (written there by `flip::apply_flipped_face_to_object`),
    // but flipped is a status (CR 110.5) and status is not copied. The copiable
    // values are the normal half, which `flip` keeps stashed in `back_face`.
    if let Some(values) = crate::game::flip::flipped_normal_copiable_values(obj) {
        return values;
    }
    CopiableValues {
        name: obj.base_name.clone(),
        mana_cost: obj.base_mana_cost.clone(),
        color: obj.base_color.clone(),
        card_types: obj.base_card_types.clone(),
        power: obj.base_power,
        toughness: obj.base_toughness,
        loyalty: obj.base_loyalty,
        printed_loyalty: obj.base_printed_loyalty,
        keywords: obj.base_keywords.clone(),
        // CopiableValues now shares `Arc<Vec<_>>` with the source object —
        // a copy-effect never mutates the ability set, so refcount sharing
        // is both correct and zero-allocation.
        abilities: Arc::clone(&obj.base_abilities),
        trigger_definitions: Arc::clone(&obj.base_trigger_definitions),
        replacement_definitions: copiable_replacement_definitions(obj),
        static_definitions: Arc::clone(&obj.base_static_definitions),
        // CR 709.5 + CR 709.5b: a Room's per-half identities are copiable —
        // the door-stamped defs above carry both halves' TEXT, this carries
        // both halves' names and costs. `None` for every non-Room source
        // (the base types are this snapshot's own Room gate).
        room_halves: obj
            .base_card_types
            .subtypes
            .iter()
            .any(|s| s == "Room")
            .then(|| crate::game::room::own_room_halves(obj)),
        // CR 707.9b exceptions are folded in by `compute_current_copiable_values`,
        // never by the printed form.
        name_origin: Default::default(),
    }
}

/// CR 707.2 / CR 707.2b: copiable values are the object's printed/defining
/// characteristics, NOT resolved continuous effects installed by other
/// permanents (CR 611.2b "for as long as you control ~" locks). A
/// `ControllerControlsSource`-gated replacement and a turn-bound, target-bound
/// die-exile rider are runtime effects durably stored in
/// `base_replacement_definitions` purely so they survive a layer reset
/// (evaluate_layers rebuilds live defs from base — layers.rs); neither is a
/// printed characteristic. Exclude them from copiable values so that a copy of
/// the affected host does not inherit the lock or die-exile rider.
///
/// Zero-alloc fast path: every printed card has no gated def, so the common case
/// keeps sharing the source `Arc<Vec<_>>`. A filtered allocation is paid only
/// when a runtime lock is actually present on the object.
fn copiable_replacement_definitions(obj: &GameObject) -> Arc<Vec<ReplacementDefinition>> {
    if !obj
        .base_replacement_definitions
        .iter()
        .any(is_runtime_non_copiable_replacement)
    {
        return Arc::clone(&obj.base_replacement_definitions);
    }
    Arc::new(
        obj.base_replacement_definitions
            .iter()
            .filter(|def| !is_runtime_non_copiable_replacement(def))
            .cloned()
            .collect(),
    )
}

/// CR 707.2 / CR 611.2b: True for a replacement that is a runtime continuous
/// effect installed by another permanent ("for as long as you control ~"),
/// durably stored in base only for layer-reset survival. Such defs are NOT
/// copiable values and must be excluded from any copiable-values surface
/// (`intrinsic_copiable_values`, the merge/mutate `merged_copiable_values`).
pub(crate) fn is_runtime_control_gated_replacement(def: &ReplacementDefinition) -> bool {
    matches!(
        def.condition,
        Some(ReplacementCondition::ControllerControlsSource { .. })
    )
}

/// CR 614.1a + CR 514.2: True for a runtime replacement attached to a damaged
/// target by an effect such as Torch the Tower or Obliterating Bolt. It is
/// persisted in base only to survive layer resets; it is not a copiable value
/// and must lapse when that object leaves the battlefield (CR 400.7).
pub(crate) fn is_runtime_target_die_exile_replacement(def: &ReplacementDefinition) -> bool {
    def.event == ReplacementEvent::Moved
        && matches!(def.valid_card, Some(TargetFilter::SelfRef))
        && matches!(def.expiry, Some(RestrictionExpiry::EndOfTurn))
        && def.destination_zone == Some(Zone::Graveyard)
        && def.execute.as_deref().is_some_and(|execute| {
            matches!(
                *execute.effect,
                Effect::ChangeZone {
                    destination: Zone::Exile,
                    ..
                }
            )
        })
}

/// CR 614.1a + CR 400.7 + CR 707.2: True for a runtime replacement bound to the
/// lifetime of the OBJECT hosting it — the "if it would leave the battlefield,
/// exile it instead" rider installed by Unearth (CR 702.84a) and the
/// parser-driven reanimation cards (Gruesome Encore, Whip of Erebos, …). It is
/// stamped `RestrictionExpiry::UntilHostLeavesPlay`. Like the die-exile rider it
/// is persisted in base only to survive CR 613.1 layer reseeds; it is NOT a
/// copiable value (a copy of the host must not inherit the exile redirect,
/// CR 707.2) and must lapse when the host leaves the battlefield (CR 400.7).
pub(crate) fn is_runtime_host_lifetime_replacement(def: &ReplacementDefinition) -> bool {
    matches!(def.expiry, Some(RestrictionExpiry::UntilHostLeavesPlay))
}

pub(crate) fn is_runtime_non_copiable_replacement(def: &ReplacementDefinition) -> bool {
    is_runtime_control_gated_replacement(def)
        || is_runtime_target_die_exile_replacement(def)
        || is_runtime_host_lifetime_replacement(def)
}

/// CR 707.2 + CR 712.4b: Build the copiable values for a melded permanent
/// DIRECTLY from the `result` card's face. Meld is LAYER-ONLY: this converter
/// feeds `install_merge_layer_effect`, so the melded permanent presents the
/// combined back faces (the named result card) WITHOUT mutating the survivor's
/// `base_*` — each component returns as its own front face on leave (CR 712.21).
/// Parameterized over any result face (a building block, not a per-card path);
/// mirrors `apply_card_face_to_object`'s field derivations without writing base.
///
/// Also used by `CreateTokenCopyFromPool` (Momir Basic) to build copiable values
/// for a creature card chosen from the format pool, which exists only as a
/// `CardFace` (no battlefield object to read via `compute_current_copiable_values`).
pub(crate) fn copiable_values_from_face(result_face: &CardFace) -> CopiableValues {
    CopiableValues {
        name: result_face.name.clone(),
        mana_cost: result_face.mana_cost.clone(),
        color: printed_colors_from_face(result_face),
        card_types: result_face.card_type.clone(),
        power: parse_pt(&result_face.power),
        toughness: parse_pt(&result_face.toughness),
        loyalty: PrintedLoyalty::from_raw(result_face.loyalty.as_deref())
            .map(PrintedLoyalty::off_stack_value),
        printed_loyalty: PrintedLoyalty::from_raw(result_face.loyalty.as_deref()),
        keywords: result_face.keywords.clone(),
        abilities: Arc::new(result_face.abilities.clone()),
        trigger_definitions: Arc::new(result_face.triggers.clone()),
        replacement_definitions: Arc::new(result_face.replacements.clone()),
        // A format-pool face is never a Room half pair.
        room_halves: None,
        name_origin: Default::default(),
        static_definitions: Arc::new(result_face.static_abilities.clone()),
    }
}

/// CR 707.2: Keyword abilities are copiable values. When a copy snapshot carries
/// a keyword but its `trigger_definitions` omit the synthesized companion
/// trigger (e.g. Persist without an explicit printed dies trigger), install the
/// missing keyword trigger so copies function correctly.
pub(crate) fn ensure_keyword_triggers_for_copiable_values(values: &mut CopiableValues) {
    let triggers = Arc::make_mut(&mut values.trigger_definitions);
    for keyword in &values.keywords {
        for trigger in KeywordTriggerInstaller::triggers_for(keyword) {
            if triggers.iter().any(|existing| existing == &trigger) {
                continue;
            }
            if triggers.iter().any(|existing| {
                KeywordTriggerInstaller::trigger_matches_keyword_kind(existing, keyword)
            }) {
                continue;
            }
            triggers.push(trigger);
        }
    }
}

/// Apply the winning Layer-1 copy effect. The caller supplies the exact
/// continuous-effect occurrence; a copied payload never imports the source
/// object's live trigger occurrences.
pub fn apply_copiable_values(
    obj: &mut GameObject,
    values: &CopiableValues,
    copy_effect: crate::types::ability::CopyEffectInstanceRef,
) {
    obj.name = values.name.clone();
    obj.mana_cost = values.mana_cost.clone();
    obj.color = values.color.clone();
    obj.card_types = values.card_types.clone();
    obj.power = values.power;
    obj.toughness = values.toughness;
    // CR 613.1a + CR 613.4b: a copy replaces the copiable baseline seen by
    // subsequent layer-7b/base-power reads until the next layer reset.
    obj.layer_base_power = values.power;
    obj.layer_base_toughness = values.toughness;
    obj.loyalty = values.loyalty;
    obj.printed_loyalty = values.printed_loyalty;
    obj.keywords = values.keywords.clone();
    // All four ability sets are Arc-shared — refcount bumps, no deep copy.
    obj.abilities = Arc::clone(&values.abilities);
    obj.trigger_definitions = values
        .trigger_definitions
        .iter()
        .cloned()
        .enumerate()
        .map(|(copied_slot, definition)| {
            crate::types::ability::TriggerEntry::new(
                crate::types::ability::TriggerDefinitionOccurrenceRef::CopiedValue {
                    copy_effect,
                    copied_slot,
                },
                definition,
            )
        })
        .collect();
    obj.replacement_definitions = Arc::clone(&values.replacement_definitions).into();
    obj.static_definitions = Arc::clone(&values.static_definitions).into();
    // CR 709.5b + CR 707.2: carry the copied Room half data. Layer-derived —
    // the Step-1 seed clears it, so it expires with this copy effect.
    obj.copied_room_halves = values.room_halves.clone();
    // CR 707.9b + CR 707.3 + CR 613.1a: EVERY applied copy assigns the name
    // origin — a later ordinary copy therefore resets an earlier exception,
    // and a chained copy of an exception-named copy keeps the folded
    // exception as its final name.
    obj.layer1_name_origin = Some(values.name_origin);
}

/// Materialize copiable values onto a newly constructed object (for example a
/// duplicate conjure). This is a new base set, not an imaginary ongoing copy
/// continuous effect, so final explicit and keyword-companion slots receive
/// printed/base identities.
pub fn install_copiable_values_as_base(obj: &mut GameObject, values: &CopiableValues) {
    obj.name = values.name.clone();
    obj.mana_cost = values.mana_cost.clone();
    obj.color = values.color.clone();
    obj.card_types = values.card_types.clone();
    obj.power = values.power;
    obj.toughness = values.toughness;
    obj.loyalty = values.loyalty;
    obj.printed_loyalty = values.printed_loyalty;
    obj.keywords = values.keywords.clone();
    obj.abilities = Arc::clone(&values.abilities);
    obj.replacement_definitions = Arc::clone(&values.replacement_definitions).into();
    obj.static_definitions = Arc::clone(&values.static_definitions).into();

    obj.base_name = values.name.clone();
    obj.base_mana_cost = values.mana_cost.clone();
    obj.base_color = values.color.clone();
    obj.base_card_types = values.card_types.clone();
    obj.base_power = values.power;
    obj.base_toughness = values.toughness;
    obj.layer_base_power = values.power;
    obj.layer_base_toughness = values.toughness;
    obj.base_loyalty = values.loyalty;
    obj.base_printed_loyalty = values.printed_loyalty;
    obj.base_keywords = values.keywords.clone();
    obj.base_abilities = Arc::clone(&values.abilities);
    obj.base_replacement_definitions = Arc::clone(&values.replacement_definitions);
    obj.base_static_definitions = Arc::clone(&values.static_definitions);
    obj.install_trigger_base_definitions(Arc::clone(&values.trigger_definitions))
        .expect("trigger base-set generation must not overflow");
    // CR 709.5b: a materialized duplicate of a Room keeps both printed halves.
    // The base slots hold the LEFT half and a synthesized back face the right
    // one — identity only (name and door cost): the halves' TEXT rides in the
    // door-stamped definition sets installed above, and `own_room_halves`
    // re-derives printed order from this exact shape (`modal_back_face` false).
    if let Some(halves) = &values.room_halves {
        // CR 707.9b: an exception-named copy ("except its name is X") keeps X
        // as its copiable name even when materialized (reachable via
        // Impossible Man copying a Room + Snowborn Simulacra / Vona de Iedo
        // conjuring a duplicate of that permanent). The half identities still
        // provide door existence and unlock costs. Which HALF name such an
        // object would show per door is undefined by the CR; keeping X
        // wholesale is the conservative reading.
        if values.name_origin != crate::types::ability::CopiedNameOrigin::Exception {
            obj.name = halves.left.name.clone();
            obj.base_name = halves.left.name.clone();
        }
        obj.mana_cost = halves.left.mana_cost.clone();
        obj.base_mana_cost = halves.left.mana_cost.clone();
        obj.modal_back_face = false;
        obj.back_face = halves
            .right
            .as_ref()
            .map(|right| crate::game::game_object::BackFaceData {
                name: right.name.clone(),
                mana_cost: right.mana_cost.clone(),
                ..Default::default()
            });
    }
    // CR 707.9b: a folded name EXCEPTION is part of the materialized base —
    // the Step-1 seed restores the runtime marker from this every pass.
    obj.base_name_origin = (values.name_origin
        == crate::types::ability::CopiedNameOrigin::Exception)
        .then_some(crate::types::ability::CopiedNameOrigin::Exception);
    obj.base_characteristics_initialized = true;
}

pub fn snapshot_object_face(obj: &GameObject) -> BackFaceData {
    BackFaceData {
        name: obj.name.clone(),
        power: obj.power,
        toughness: obj.toughness,
        loyalty: obj.loyalty,
        printed_loyalty: obj.printed_loyalty,
        defense: obj.defense,
        card_types: obj.card_types.clone(),
        mana_cost: obj.mana_cost.clone(),
        keywords: obj.keywords.clone(),
        // BackFaceData still stores Vec<T>; deep-clone when snapshotting.
        abilities: (*obj.abilities).clone(),
        trigger_definitions: obj
            .trigger_definitions
            .iter_all()
            .map(|entry| entry.definition.clone())
            .collect(),
        replacement_definitions: obj.replacement_definitions.clone(),
        // Snapshot: deref the Arc to satisfy `Definitions::from(Vec<T>)`.
        static_definitions: (*obj.base_static_definitions).clone().into(),
        color: obj.color.clone(),
        printed_ref: obj.printed_ref.clone(),
        modal: obj.modal.clone(),
        additional_cost: obj.additional_cost.clone(),
        strive_cost: obj.strive_cost.clone(),
        casting_restrictions: obj.casting_restrictions.clone(),
        casting_options: obj.casting_options.clone(),
        // The outgoing face's diagnostics ride out with it, so the return trip
        // restores them rather than inheriting whatever the other face had.
        parse_warnings: obj.parse_warnings.clone(),
        layout_kind: None,
    }
}

/// Snapshot an object's **printed/base** characteristics into a [`BackFaceData`],
/// deliberately ignoring any live fields that continuous effects (CR 613) may
/// have altered.
///
/// Use this instead of [`snapshot_object_face`] whenever the snapshot is taken
/// from a permanent already on the battlefield — specifically when turning it
/// face down via a spell or ability (CR 708.2a). At that point the live fields
/// may include layer-applied modifications (e.g. a +1/+1 anthem making a 2/2
/// appear as 3/3). If those inflated values were stored in `back_face`,
/// [`apply_back_face_to_object`] would write them into both live and base on
/// restoration (CR 708.8), causing the anthem to reapply from an already-
/// inflated baseline and produce a permanently-wrong value.
///
/// Fields with no base equivalent (`modal`, `additional_cost`, `strive_cost`,
/// `casting_restrictions`, `casting_options`) are invariant after card creation
/// and are taken directly from the live object.
pub fn snapshot_object_base_face(obj: &GameObject) -> BackFaceData {
    BackFaceData {
        name: obj.base_name.clone(),
        power: obj.base_power,
        toughness: obj.base_toughness,
        loyalty: obj.base_loyalty,
        printed_loyalty: obj.base_printed_loyalty,
        defense: obj.base_defense,
        card_types: obj.base_card_types.clone(),
        mana_cost: obj.base_mana_cost.clone(),
        keywords: obj.base_keywords.clone(),
        abilities: (*obj.base_abilities).clone(),
        // Share the Arc rather than deep-cloning the Vec — semantically
        // identical and avoids an allocation on every face-down resolution.
        trigger_definitions: Arc::clone(&obj.base_trigger_definitions).into(),
        replacement_definitions: Arc::clone(&obj.base_replacement_definitions).into(),
        static_definitions: Arc::clone(&obj.base_static_definitions).into(),
        color: obj.base_color.clone(),
        printed_ref: obj.base_printed_ref.clone(),
        // Casting metadata: invariant after card creation, no base equivalent.
        modal: obj.modal.clone(),
        additional_cost: obj.additional_cost.clone(),
        strive_cost: obj.strive_cost.clone(),
        casting_restrictions: obj.casting_restrictions.clone(),
        casting_options: obj.casting_options.clone(),
        // Face-derived, with no base/live split to choose between: nothing writes
        // `parse_warnings` except a face install, so the live field IS the printed
        // face's diagnostics and the layer system never touches it.
        parse_warnings: obj.parse_warnings.clone(),
        layout_kind: None,
    }
}

// ---------------------------------------------------------------------------
// Conjure-target effect walker
//
// `Effect::Conjure` (digital-only, no CR entry) creates a card from outside the
// game (`game/effects/conjure.rs`). The handler resolves the conjured face from
// `GameState::card_face_registry`, which previously held *every* card face in the
// database — a full-DB clone on each game init. To avoid that allocation spike,
// `rehydrate_game_from_card_db` now scopes the registry to exactly the faces a
// game can reach as Conjure targets: the transitive closure of conjure names
// over the seed faces present in the game (objects + deck pools).
//
// These wrappers yield every conjure name reachable from a `CardFace`. The
// traversal itself lives in `crate::types::ability_visit`, which owns the
// wildcard-free `Effect` / `ContinuousModification` / `AbilityCost` matches: a
// future variant carrying a nested `Box<Effect>` / `Box<AbilityDefinition>` is a
// compile error there. These wrappers supply only the conjure/meld
// name-extraction leaf (`collect_conjure_names`).
//
// The reusable visitor this file's TODO asked for now exists:
// `crate::types::ability_visit`. `game/coverage.rs` is still NOT migrated — its
// pass builds `ParsedItem` trees rather than yielding `Effect`s, and
// `coverage::ability_tree_any` is deliberately narrower (it has a `_ => {}`
// wildcard); broadening it would change the coverage report. See the
// `types::ability_visit` module doc.
// ---------------------------------------------------------------------------

/// Collect every conjure name reachable from a single card face's ability set.
fn collect_conjure_names_from_face(face: &CardFace, out: &mut Vec<String>) {
    for ability in &face.abilities {
        walk_ability_def(ability, out);
    }
    for trigger in &face.triggers {
        walk_trigger(trigger, out);
    }
    for static_def in &face.static_abilities {
        walk_static(static_def, out);
    }
    for replacement in &face.replacements {
        walk_replacement(replacement, out);
    }
    // Alchemy spellbook: every card a spellbook draft can produce must be in the
    // registry to be instantiable by the conjure path.
    out.extend(face.metadata.spellbook.iter().cloned());
}

/// The conjure/meld name extraction that `visit_effect` used to inline. Split
/// out so the traversal itself is reusable (see `types::ability_visit`).
fn collect_conjure_names(effect: &Effect, out: &mut Vec<String>) {
    match effect {
        Effect::Conjure { cards, .. } => {
            // Only named-conjure has a static card name to seed into the face
            // registry. Duplicate-conjure copies a card already in play (its face
            // travels on the referenced object), so there is nothing to preload.
            for conjure_card in cards {
                if let ConjureSource::Named { name } = &conjure_card.source {
                    out.push(name.clone());
                }
            }
        }
        // CR 701.42 / CR 712.4b: the melded permanent presents the `result`
        // card's characteristics, but `result` is an outside-the-game third card.
        // Seed its name so `build_conjure_registry` preloads its `CardFace` into
        // `card_face_registry`. `source` and `partner` are live battlefield
        // objects the resolver finds by printed identity — they need no registry
        // seeding.
        Effect::Meld { result, .. } => out.push(result.clone()),
        _ => {}
    }
}

fn walk_ability_def(def: &AbilityDefinition, out: &mut Vec<String>) {
    let _ = visit_ability_def(def, &mut |effect| {
        collect_conjure_names(effect, out);
        ControlFlow::Continue(())
    });
}

fn walk_trigger(trigger: &TriggerDefinition, out: &mut Vec<String>) {
    let _ = visit_trigger(trigger, &mut |effect| {
        collect_conjure_names(effect, out);
        ControlFlow::Continue(())
    });
}

fn walk_replacement(replacement: &ReplacementDefinition, out: &mut Vec<String>) {
    let _ = visit_replacement(replacement, &mut |effect| {
        collect_conjure_names(effect, out);
        ControlFlow::Continue(())
    });
}

fn walk_static(static_def: &StaticDefinition, out: &mut Vec<String>) {
    let _ = visit_static(static_def, &mut |effect| {
        collect_conjure_names(effect, out);
        ControlFlow::Continue(())
    });
}

#[cfg(test)]
fn walk_effect(effect: &Effect, out: &mut Vec<String>) {
    let _ = visit_effect(effect, &mut |e| {
        collect_conjure_names(e, out);
        ControlFlow::Continue(())
    });
}

/// Collect every conjure name seeded by the faces present in the game: each
/// object's printed face (resolved via the database) plus every deck-pool face
/// (carried inline as `DeckEntry.card`).
///
/// Boundary: only printed faces are seeds. A sourceless object (a token or
/// emblem with no `printed_ref`) whose granted ability conjures would not seed
/// its target. No current card hits this; revisit if a printed-faceless
/// conjure source is ever added.
fn collect_seed_conjure_names(state: &GameState, db: &CardDatabase) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();

    for object in state.objects.values() {
        if let Some(printed_ref) = &object.printed_ref {
            if let Some(face) = db.get_face_by_printed_ref(printed_ref) {
                collect_conjure_names_from_face(face, &mut names);
            }
        }
    }

    for pool in &state.deck_pools {
        let entry_lists = [
            &pool.registered_main,
            &pool.registered_sideboard,
            &pool.current_main,
            &pool.current_sideboard,
            &pool.registered_companion,
            &pool.current_companion,
            &pool.registered_commander,
            &pool.current_commander,
        ];
        for entry_list in entry_lists {
            for entry in entry_list.iter() {
                collect_conjure_names_from_face(&entry.card, &mut names);
            }
        }
    }

    names
}

/// Build the scoped Conjure registry: the transitive closure of conjure-target
/// faces reachable from the seed faces present in the game. The closure follows
/// conjure names (a conjured card may itself conjure another) to a fixpoint.
/// Returns the registry plus every conjure name encountered along the way (used
/// by the debug-only walker-coverage safety net).
pub(crate) fn build_conjure_registry(
    state: &GameState,
    db: &CardDatabase,
) -> (HashMap<String, CardFace>, Vec<String>) {
    let mut pending = collect_seed_conjure_names(state, db);
    let mut all_collected = pending.clone();

    // Transitive closure: resolve each pending name, insert its face, and walk
    // it for further conjure names until the frontier is empty.
    let mut registry: HashMap<String, CardFace> = HashMap::new();
    while let Some(name) = pending.pop() {
        // The Conjure handler keys lookups by `name.to_lowercase()`
        // (game/effects/conjure.rs); mirror that exactly so resolution hits.
        let key = name.to_lowercase();
        if registry.contains_key(&key) {
            continue;
        }
        let Some(face) = db.get_face_by_name(&name) else {
            continue;
        };
        let before = pending.len();
        collect_conjure_names_from_face(face, &mut pending);
        all_collected.extend_from_slice(&pending[before..]);
        registry.insert(key, face.clone());
    }

    (registry, all_collected)
}

/// CR 712 / CR 715 / CR 722: Build the other printed face for a face-complete
/// card source. This is shared by normal database hydration and debug card
/// batches so a paused batch can retain DFC/Adventure/Omen/Meld/Prepare data
/// without consulting the card database again on resume.
pub fn back_face_for_card_face(db: &CardDatabase, card_face: &CardFace) -> Option<BackFaceData> {
    let printed_ref = printed_ref_from_face(card_face);
    back_face_for_card_face_with_printed_ref(db, card_face, printed_ref.as_ref())
}

fn back_face_for_card_face_with_printed_ref(
    db: &CardDatabase,
    card_face: &CardFace,
    printed_ref: Option<&PrintedCardRef>,
) -> Option<BackFaceData> {
    let second_face = db
        .get_by_name(&card_face.name)
        .and_then(|card_rules| match &card_rules.layout {
            // CR 715: Adventurer cards have alternative Adventure characteristics.
            CardLayout::Adventure(_, back) => Some((LayoutKind::Adventure, back)),
            // CR 712: Transforming, modal, meld, and omen DFCs need their other face.
            CardLayout::Transform(_, back) => Some((LayoutKind::Transform, back)),
            CardLayout::Modal(_, back) => Some((LayoutKind::Modal, back)),
            CardLayout::Meld(_, back) => Some((LayoutKind::Meld, back)),
            CardLayout::Omen(_, back) => Some((LayoutKind::Omen, back)),
            // CR 710.1b: a flip card's alternative name, text box, type line,
            // power, and toughness live on its bottom half. Stored in the same
            // `back_face` slot so `flip::flip_permanent` can apply it — the
            // `LayoutKind::Flip` tag is what keeps it out of every double-faced
            // path (`transform::is_double_faced_permanent`,
            // `transform::transform_permanent`, MDFC/Adventure face choice).
            CardLayout::Flip(_, back) => Some((LayoutKind::Flip, back)),
            // CR 722: Preparation cards expose prepare-spell characteristics.
            CardLayout::Prepare(_, back) => Some((LayoutKind::Prepare, back)),
            _ => None,
        })
        .or_else(|| {
            let layout_kind = card_face
                .scryfall_oracle_id
                .as_deref()
                .and_then(|id| db.get_layout_kind(id))
                .unwrap_or(LayoutKind::Single);
            printed_ref
                .and_then(|printed_ref| db.get_other_face_by_printed_ref(printed_ref))
                .map(|face| (layout_kind, face))
        });
    let (layout_kind, face) = second_face?;

    let mut back = BackFaceData {
        name: String::new(),
        power: None,
        toughness: None,
        loyalty: None,
        printed_loyalty: None,
        defense: None,
        card_types: Default::default(),
        mana_cost: Default::default(),
        keywords: Vec::new(),
        abilities: Vec::new(),
        trigger_definitions: crate::types::definitions::Definitions::default(),
        replacement_definitions: crate::types::definitions::Definitions::default(),
        static_definitions: crate::types::definitions::Definitions::default(),
        color: Vec::new(),
        printed_ref: None,
        modal: None,
        additional_cost: None,
        strive_cost: None,
        casting_restrictions: Vec::new(),
        casting_options: Vec::new(),
        // Empty seed; `apply_card_face_to_back_face` below fills it from the face.
        parse_warnings: Vec::new(),
        layout_kind: None,
    };
    apply_card_face_to_back_face(&mut back, face);
    if layout_kind != LayoutKind::Single {
        back.layout_kind = Some(layout_kind);
    }
    Some(back)
}

/// CR 712 / CR 715 / CR 722: Attach the other printed face to `obj.back_face`
/// when absent. Required for transformed zone changes (Fable of the
/// Mirror-Breaker chapter III, Ajani flip triggers), adventurer casts, MDFC
/// casts, and prepare spell access. Without this, `deliver_replaced_zone_change`
/// silently skips transform when `back_face` is `None` and saga ETB lore-counter
/// replacements fire on the front face.
pub fn populate_back_face_if_dfc(obj: &mut GameObject, db: &CardDatabase, card_face: &CardFace) {
    if obj.back_face.is_none() {
        obj.back_face =
            back_face_for_card_face_with_printed_ref(db, card_face, obj.printed_ref.as_ref());
    }
}

pub fn rehydrate_game_from_card_db(state: &mut GameState, db: &CardDatabase) {
    rehydrate_card_db_metadata(state, db);
    let (changed_any, changed_battlefield) = reapply_printed_faces_from_card_db(state, db);
    repair_battlefield_trigger_index_after_face_reapply(state, changed_battlefield);

    if changed_any || state.layers_dirty.is_dirty() {
        bump_state_revision(state);
        mark_public_state_all_dirty(state);
        finalize_public_state(state);
    }
}

/// Populate Conjure registry and card-name validation lists on first rehydrate.
fn rehydrate_card_db_metadata(state: &mut GameState, db: &CardDatabase) {
    if state.meld_pair_registry.is_empty() {
        state.meld_pair_registry = Arc::new(build_meld_pair_registry(db));
    }
    // Populate the Conjure card-face registry (used by the Conjure effect
    // handler). Scoped to exactly the faces reachable as Conjure targets so we
    // never clone the entire database into per-game state. Decks with no
    // conjure cards yield an empty registry and pay no allocation cost.
    if state.card_face_registry.is_empty() {
        let (registry, collected_names) = build_conjure_registry(state, db);

        // Safety net: a walker that misses a nested effect/ability carrier would
        // silently ship a broken conjure. Fire only for names the database
        // *could* resolve — names it cannot resolve (typos, Alchemy-only,
        // export-filtered) are legitimately absent today.
        #[cfg(debug_assertions)]
        for name in &collected_names {
            debug_assert!(
                db.get_face_by_name(name).is_none() || registry.contains_key(&name.to_lowercase()),
                "conjure walker missed resolvable card '{name}' — a nested \
                 effect/ability carrier is not traversed by walk_effect"
            );
        }
        #[cfg(not(debug_assertions))]
        let _ = collected_names;

        state.card_face_registry = std::sync::Arc::new(registry);
    }

    // Restore the `#[serde(skip)]` "name a card" validation list. Without this,
    // a NamedChoice { choice_type: CardName, options: [] } (e.g. Petrified Hamlet's
    // "choose a land card name") leaves the AI with zero legal candidates after a
    // game is restored from a persisted snapshot, deadlocking the session.
    if state.all_card_names.is_empty() {
        state.all_card_names = db.card_names().into();
    }

    // CR 707.2 + CR 202.3: Build the Momir Basic random-token pool. Gated on the
    // format AND emptiness: `rehydrate_card_db_metadata` also runs on the
    // mid-game debug-spawn path (engine-wasm), so without the emptiness guard we
    // would rescan the full creature corpus on every spawn.
    //
    // The emptiness check must watch `momir_pool_faces`, NOT just `momir_pool`:
    // `momir_pool` is serialized but `momir_pool_faces` is `#[serde(skip)]`
    // (it holds full `CardFace` values, too heavy to ship). After ANY
    // deserialize — `restore_game_state` on worker restart/PWA update, or a peer
    // syncing — `momir_pool` comes back populated while `momir_pool_faces` is
    // empty. Gating on `momir_pool.is_empty()` alone would then refuse to rebuild
    // the faces map, leaving `CreateTokenCopyFromPool` with zero hydratable
    // candidates (every name in the pool misses the empty faces map) and the
    // emblem silently makes no token. Rebuilding when EITHER is empty restores
    // the faces map; the rebuild overwrites `momir_pool` wholesale, so a
    // non-empty pool is regenerated identically (keys are sorted → deterministic
    // across peers), never duplicated.
    if state.format_config.format == crate::types::format::GameFormat::Momir
        && (state.momir_pool.is_empty() || state.momir_pool_faces.is_empty())
    {
        let mut pool: std::collections::BTreeMap<i32, Vec<String>> =
            std::collections::BTreeMap::new();
        let mut faces: HashMap<String, CardFace> = HashMap::new();
        for face in db
            .face_index
            .values()
            .filter(|face| face.card_type.core_types.contains(&CoreType::Creature))
            // CR 202.1b + CR 202.3b + CR 712.8a: `face_index` holds BOTH faces of
            // every multi-face card, so a transform/flip/meld BACK face (which has
            // no printed mana cost → `ManaCost::NoCost`, mana value 0) would key
            // into the pool at MV 0. A back face is not a separately castable
            // creature *card* (outside the battlefield a DFC has only its front
            // face's characteristics), so it is never a valid Momir pick. Exclude
            // costless faces by their data signal: only an ABSENT manaCost maps to
            // `NoCost`, so modal-DFC creature backs (explicit cost → `Cost{..}`)
            // and genuine `{0}` creatures (`Cost{generic:0}`) are preserved.
            .filter(|face| !matches!(face.mana_cost, ManaCost::NoCost))
        {
            let mv = face.mana_cost.mana_value() as i32;
            pool.entry(mv).or_default().push(face.name.clone());
            faces.insert(face.name.to_lowercase(), face.clone());
        }
        // Deterministic selection order regardless of DB iteration order.
        for names in pool.values_mut() {
            names.sort();
        }
        state.momir_pool = pool;
        state.momir_pool_faces = std::sync::Arc::new(faces);
    }
}

/// CR 701.42b + CR 712.4: derive the physical meld-pair authority from card
/// database layout metadata and the parsed meld instruction. A forged effect
/// whose three named faces are not all database-backed meld faces is excluded.
fn build_meld_pair_registry(db: &CardDatabase) -> HashMap<String, MeldPairRecord> {
    let mut registry = HashMap::new();
    for (_, face) in db.face_iter() {
        let mut effects = Vec::new();
        collect_meld_effects_from_face(face, &mut effects);
        for (source, partner, result) in effects {
            if !meld_front_maps_to_result(db, source, result)
                || !meld_front_maps_to_result(db, partner, result)
            {
                continue;
            }
            let key = meld_pair_key(source, partner);
            registry.insert(
                key,
                MeldPairRecord {
                    source: source.clone(),
                    partner: partner.clone(),
                    result: result.clone(),
                },
            );
        }
    }
    registry
}

fn meld_pair_key(source: &str, partner: &str) -> String {
    format!("{}\0{}", source.to_lowercase(), partner.to_lowercase())
}

fn meld_front_maps_to_result(db: &CardDatabase, front: &str, result: &str) -> bool {
    let mtgjson_layout_matches = db.get_by_name(front).is_some_and(|rules| {
        matches!(
            &rules.layout,
            CardLayout::Meld(printed_front, combined_back)
                if printed_front.name.eq_ignore_ascii_case(front)
                    && combined_back.name.eq_ignore_ascii_case(result)
        )
    });
    if mtgjson_layout_matches {
        return true;
    }

    // The production card-data export intentionally stores faces rather than
    // reconstructed `CardRules`. Recover the same exact front -> combined-back
    // relation from the shared oracle id and layout discriminant; checking only
    // `LayoutKind::Meld` would admit a forged result from a different meld pair.
    let Some(front_face) = db.get_face_by_name(front) else {
        return false;
    };
    let Some(printed_ref) = printed_ref_from_face(front_face) else {
        return false;
    };
    matches!(
        db.get_layout_kind(&printed_ref.oracle_id),
        Some(LayoutKind::Meld)
    ) && db
        .get_other_face_by_printed_ref(&printed_ref)
        .is_some_and(|combined_back| combined_back.name.eq_ignore_ascii_case(result))
}

fn collect_meld_effects_from_face<'a>(
    face: &'a CardFace,
    out: &mut Vec<(&'a String, &'a String, &'a String)>,
) {
    for ability in &face.abilities {
        collect_meld_effects_from_ability(ability, out);
    }
    for trigger in &face.triggers {
        if let Some(execute) = trigger.execute.as_deref() {
            collect_meld_effects_from_ability(execute, out);
        }
    }
}

fn collect_meld_effects_from_ability<'a>(
    ability: &'a AbilityDefinition,
    out: &mut Vec<(&'a String, &'a String, &'a String)>,
) {
    if let Effect::Meld {
        source,
        partner,
        result,
        ..
    } = ability.effect.as_ref()
    {
        out.push((source, partner, result));
    }
    if let Some(sub) = ability.sub_ability.as_deref() {
        collect_meld_effects_from_ability(sub, out);
    }
    if let Some(otherwise) = ability.else_ability.as_deref() {
        collect_meld_effects_from_ability(otherwise, out);
    }
    for mode in &ability.mode_abilities {
        collect_meld_effects_from_ability(mode, out);
    }
}

/// Re-apply printed faces from `db` to every object that carries a `printed_ref`.
/// Does not finalize public state or flush layers.
fn reapply_printed_faces_from_card_db(state: &mut GameState, db: &CardDatabase) -> (bool, bool) {
    let object_ids: Vec<_> = state.objects.keys().copied().collect();
    let mut changed_any = false;
    let mut changed_battlefield = false;

    for object_id in object_ids {
        let Some(printed_ref) = state
            .objects
            .get(&object_id)
            .and_then(|obj| obj.printed_ref.clone())
        else {
            continue;
        };

        let Some(card_face) = db.get_face_by_printed_ref(&printed_ref).cloned() else {
            continue;
        };

        let zone = state.objects[&object_id].zone;
        if let Some(obj) = state.objects.get_mut(&object_id) {
            let is_face_down_battlefield = obj.face_down && obj.zone == Zone::Battlefield;

            if is_face_down_battlefield {
                if obj.back_face.is_none() {
                    obj.back_face = Some(snapshot_object_face(obj));
                }
            } else if obj.is_token {
                // CR 111.1 + CR 707.2: A token's characteristics are synthesized
                // at creation (e.g. a copy token created with "isn't legendary",
                // or a non-legendary token copy of a legendary creature) and are
                // persisted in full as part of its serialized state — they are
                // NOT derived from any printed card. A token-copy of a real card
                // carries that card's `printed_ref` purely as a display/art hint
                // (see `token_copy::resolve`), so re-applying the printed face's
                // copiable values here would clobber the token's synthesized
                // characteristics — wrongly re-adding the Legendary supertype to
                // a non-legendary token copy of a legendary card and triggering
                // the legend rule (CR 704.5j) on load. Restore only the display
                // pointer the DB lookup confirmed; leave game characteristics
                // untouched.
                obj.printed_ref = printed_ref_from_face(&card_face);
                obj.base_printed_ref = obj.printed_ref.clone();
            } else {
                apply_card_face_to_object(obj, &card_face);
                // CR 702.103b: Rehydration re-stamps printed characteristics from
                // card-data.json, which clobbers the synthesized Aura subtype while
                // `bestow_form` remains set (issue #3253). Re-apply the bestow
                // type-changing effect so WASM/client views see a legal Aura.
                if obj.bestow_form.is_some() {
                    crate::game::casting::apply_bestow_aura_form(obj);
                }
            }

            if let Some(back_face) = obj.back_face.as_mut() {
                if let Some(back_ref) = back_face.printed_ref.clone() {
                    if let Some(back_card_face) = db.get_face_by_printed_ref(&back_ref) {
                        if obj.is_token {
                            // CR 111.1 + CR 707.2: token back-face
                            // characteristics are serialized copiable values,
                            // not values to re-derive from the printed card.
                            back_face.printed_ref = printed_ref_from_face(back_card_face);
                        } else {
                            apply_card_face_to_back_face(back_face, back_card_face);
                        }
                    } else if is_face_down_battlefield && !obj.is_token {
                        apply_card_face_to_back_face(back_face, &card_face);
                    }
                } else if is_face_down_battlefield && !obj.is_token {
                    apply_card_face_to_back_face(back_face, &card_face);
                }
                // CR 712.12: Restore layout_kind if it was cleared (e.g. after MDFC
                // front-face choice). Ensures bounced MDFCs can prompt face choice again.
                if back_face.layout_kind.is_none() {
                    back_face.layout_kind = db
                        .get_by_name(&card_face.name)
                        .and_then(|rules| match &rules.layout {
                            CardLayout::Adventure(..) => Some(LayoutKind::Adventure),
                            CardLayout::Transform(..) => Some(LayoutKind::Transform),
                            CardLayout::Modal(..) => Some(LayoutKind::Modal),
                            CardLayout::Meld(..) => Some(LayoutKind::Meld),
                            CardLayout::Omen(..) => Some(LayoutKind::Omen),
                            // CR 710.1b: restore the flip tag so a reloaded
                            // flip permanent's stashed alternative face stays
                            // excluded from the double-faced paths.
                            CardLayout::Flip(..) => Some(LayoutKind::Flip),
                            // CR 702.xxx: Prepare (Strixhaven) — treat like Adventure for
                            // back-face layout tracking. Assign when WotC publishes SOS CR update.
                            CardLayout::Prepare(..) => Some(LayoutKind::Prepare),
                            _ => None,
                        })
                        .or_else(|| {
                            // Fallback for export-loaded databases where `cards` is empty.
                            card_face
                                .scryfall_oracle_id
                                .as_deref()
                                .and_then(|id| db.get_layout_kind(id))
                        });
                }
            }

            // CR 710.1c: a flip card's color and mana cost don't change if the
            // permanent is flipped. A flipped permanent's `printed_ref` names
            // the ALTERNATIVE half, which carries no printed mana cost, so the
            // `apply_card_face_to_object` reapply above would blank it on every
            // reload. Restore both from the (just-refreshed) normal half stashed
            // in `back_face` — the same values `flip::flip_permanent`
            // deliberately left untouched when it flipped the permanent.
            crate::game::flip::restore_normal_cost_and_color_if_flipped(obj);

            if is_face_down_battlefield {
                // CR 708.2a: This reload path only runs while `printed_ref` is
                // still set (see the `obj.printed_ref.clone()` guard above);
                // effect-driven face-down entries (Cyber-Controller) clear
                // `printed_ref` and carry their `FaceDownProfile` characteristics
                // directly, so they never reach here. The vanilla 2/2 default
                // reproduces the morph/manifest reload behavior.
                apply_face_down_creature_characteristics(
                    obj,
                    &crate::types::ability::FaceDownProfile::vanilla_2_2(),
                );
                changed_any = true;
                changed_battlefield = true;
                continue;
            }

            // Digital-only Specialize: load all specialized faces for runtime choice.
            if obj.specialize_faces.is_none() {
                if let Some(rules) = db.get_by_name(&card_face.name) {
                    if let CardLayout::Specialize(_, variants) = &rules.layout {
                        obj.specialize_faces =
                            Some(super::specialize::specialize_faces_from_variants(variants));
                    }
                }
            }

            populate_back_face_if_dfc(obj, db, &card_face);
        }

        changed_any = true;
        if zone == crate::types::zones::Zone::Battlefield {
            changed_battlefield = true;
        }
    }

    (changed_any, changed_battlefield)
}

/// CR 603.6a: `apply_card_face_to_object` may replace `trigger_definitions`
/// without touching the derived index. Rebuild so upkeep triggers (e.g. Mystic
/// Remora cumulative upkeep) stay indexed before the next event consult.
fn repair_battlefield_trigger_index_after_face_reapply(
    state: &mut GameState,
    changed_battlefield: bool,
) {
    if changed_battlefield {
        crate::game::layers::mark_layers_full(state);
        crate::types::game_state::TriggerIndex::rebuild_from_battlefield(state);
    }
}

fn parse_pt(val: &Option<PtValue>) -> Option<i32> {
    val.as_ref().map(|pt| match pt {
        PtValue::Fixed(n) => *n,
        // No game state at deck-load time; dynamic P/T resolves to 0.
        PtValue::Variable(_) | PtValue::Quantity(_) => 0,
    })
}

fn shard_colors(shard: &ManaCostShard) -> Vec<ManaColor> {
    match shard {
        ManaCostShard::White | ManaCostShard::TwoWhite | ManaCostShard::PhyrexianWhite => {
            vec![ManaColor::White]
        }
        ManaCostShard::Blue | ManaCostShard::TwoBlue | ManaCostShard::PhyrexianBlue => {
            vec![ManaColor::Blue]
        }
        ManaCostShard::Black | ManaCostShard::TwoBlack | ManaCostShard::PhyrexianBlack => {
            vec![ManaColor::Black]
        }
        ManaCostShard::Red | ManaCostShard::TwoRed | ManaCostShard::PhyrexianRed => {
            vec![ManaColor::Red]
        }
        ManaCostShard::Green | ManaCostShard::TwoGreen | ManaCostShard::PhyrexianGreen => {
            vec![ManaColor::Green]
        }
        ManaCostShard::WhiteBlue | ManaCostShard::PhyrexianWhiteBlue => {
            vec![ManaColor::White, ManaColor::Blue]
        }
        ManaCostShard::WhiteBlack | ManaCostShard::PhyrexianWhiteBlack => {
            vec![ManaColor::White, ManaColor::Black]
        }
        ManaCostShard::BlueBlack | ManaCostShard::PhyrexianBlueBlack => {
            vec![ManaColor::Blue, ManaColor::Black]
        }
        ManaCostShard::BlueRed | ManaCostShard::PhyrexianBlueRed => {
            vec![ManaColor::Blue, ManaColor::Red]
        }
        ManaCostShard::BlackRed | ManaCostShard::PhyrexianBlackRed => {
            vec![ManaColor::Black, ManaColor::Red]
        }
        ManaCostShard::BlackGreen | ManaCostShard::PhyrexianBlackGreen => {
            vec![ManaColor::Black, ManaColor::Green]
        }
        ManaCostShard::RedWhite | ManaCostShard::PhyrexianRedWhite => {
            vec![ManaColor::Red, ManaColor::White]
        }
        ManaCostShard::RedGreen | ManaCostShard::PhyrexianRedGreen => {
            vec![ManaColor::Red, ManaColor::Green]
        }
        ManaCostShard::GreenWhite | ManaCostShard::PhyrexianGreenWhite => {
            vec![ManaColor::Green, ManaColor::White]
        }
        ManaCostShard::GreenBlue | ManaCostShard::PhyrexianGreenBlue => {
            vec![ManaColor::Green, ManaColor::Blue]
        }
        ManaCostShard::ColorlessWhite => vec![ManaColor::White],
        ManaCostShard::ColorlessBlue => vec![ManaColor::Blue],
        ManaCostShard::ColorlessBlack => vec![ManaColor::Black],
        ManaCostShard::ColorlessRed => vec![ManaColor::Red],
        ManaCostShard::ColorlessGreen => vec![ManaColor::Green],
        ManaCostShard::Colorless
        | ManaCostShard::Snow
        | ManaCostShard::X
        | ManaCostShard::TwoOrMoreColorSource => vec![],
    }
}

pub fn derive_colors_from_mana_cost(mana_cost: &ManaCost) -> Vec<ManaColor> {
    match mana_cost {
        ManaCost::NoCost
        | ManaCost::SelfManaCost
        | ManaCost::SelfManaValue
        | ManaCost::SelfManaCostReduced { .. } => vec![],
        ManaCost::Cost { shards, .. } => {
            let mut colors = Vec::new();
            for shard in shards {
                for color in shard_colors(shard) {
                    if !colors.contains(&color) {
                        colors.push(color);
                    }
                }
            }
            colors
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database::CardDatabase;
    use crate::game::deck_loading::create_object_from_card_face;
    use crate::game::deck_loading::DeckEntry;
    use crate::game::game_object::GameObject;
    use crate::game::zones::create_object;
    use crate::types::ability::{
        AbilityCost, AbilityDefinition, AbilityKind, AdditionalCost, CastingRestriction,
        ConjureCard, ContinuousModification, ControllerRef, DelayedTriggerCondition,
        DieResultBranch, Effect, ModalChoice, PlayerFilter, PlayerScope, QuantityExpr,
        ReplacementDefinition, SolveCondition, SpellCastingOption, StaticDefinition, TargetFilter,
        TriggerDefinition, UnlessPayModifier, VoterScope,
    };
    use crate::types::card::CardFace;
    use crate::types::card_type::{CardType, CoreType};
    use crate::types::game_state::GameState;
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::keywords::Keyword;
    use crate::types::mana::{ManaColor, ManaCost, ManaCostShard};
    use crate::types::player::PlayerId;
    use crate::types::replacements::ReplacementEvent;
    use crate::types::statics::StaticMode;
    use crate::types::triggers::TriggerMode;
    use crate::types::zones::Zone;
    use crate::types::Phase;
    use std::sync::Arc;

    fn trigger_copiable_values() -> CopiableValues {
        let mut source = GameObject::new(
            ObjectId(1),
            CardId(1),
            PlayerId(0),
            "Trigger Source".to_string(),
            Zone::Battlefield,
        );
        source.base_trigger_definitions = Arc::new(vec![
            TriggerDefinition::new(TriggerMode::Phase),
            TriggerDefinition::new(TriggerMode::Attacks),
        ]);
        intrinsic_copiable_values(&source)
    }

    /// A bare "Moved SelfRef -> Exile" redirect with NO expiry stamp — the
    /// Personal Decoy printed-static shape (CMB1 playtest card). This is the
    /// fixture that kills shape-widening: the runtime detectors must key on the
    /// `UntilHostLeavesPlay` expiry stamp, NOT on the redirect shape, so a
    /// printed-static exile redirect is never misclassified as a runtime rider.
    fn bare_moved_selfref_exile_rider() -> ReplacementDefinition {
        ReplacementDefinition::new(ReplacementEvent::Moved)
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
                    enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                    enters_attacking: false,
                    up_to: false,
                    enter_with_counters: vec![],
                    conditional_enter_with_counters: vec![],
                    face_down_profile: None,
                    enters_modified_if: None,
                },
            ))
    }

    /// R10 (issue #5976): a redirect that lacks the `UntilHostLeavesPlay` stamp
    /// must be classified as NEITHER a host-lifetime rider NOR non-copiable — the
    /// detectors key on the expiry stamp, not the Moved->Exile shape, so a
    /// printed-static exile redirect (Personal Decoy) is never widened into a
    /// runtime rider.
    #[test]
    fn r10_bare_exile_redirect_without_expiry_is_not_runtime_rider() {
        let def = bare_moved_selfref_exile_rider();
        assert!(
            !is_runtime_host_lifetime_replacement(&def),
            "a redirect with no UntilHostLeavesPlay stamp is NOT a host-lifetime rider"
        );
        assert!(
            !is_runtime_non_copiable_replacement(&def),
            "a printed-static exile redirect must stay copiable (shape must not widen)"
        );
    }

    /// R6 (issue #5976): the same redirect, now stamped `UntilHostLeavesPlay`, IS
    /// a runtime rider (CR 702.84a) and must be excluded from the host's copiable
    /// values so a copy does not inherit the exile redirect (CR 707.2). This is
    /// the production seam the runtime token-copy path
    /// (`compute_current_copiable_values` -> `copiable_replacement_definitions`)
    /// consumes.
    #[test]
    fn host_lifetime_rider_is_non_copiable_and_excluded_from_copiable_values() {
        let rider = bare_moved_selfref_exile_rider()
            .expiry(crate::types::ability::RestrictionExpiry::UntilHostLeavesPlay);
        assert!(is_runtime_host_lifetime_replacement(&rider));
        assert!(is_runtime_non_copiable_replacement(&rider));

        let mut obj = GameObject::new(
            ObjectId(7),
            CardId(7),
            PlayerId(0),
            "Unearthed".to_string(),
            Zone::Battlefield,
        );
        obj.base_replacement_definitions = Arc::new(vec![rider]);

        let copiable = intrinsic_copiable_values(&obj);
        assert!(
            copiable
                .replacement_definitions
                .iter()
                .all(|r| !is_runtime_host_lifetime_replacement(r)),
            "CR 707.2: a copy must not inherit the host-lifetime exile rider"
        );
        assert!(
            copiable.replacement_definitions.is_empty(),
            "the only rider was the non-copiable host-lifetime one, so copiable defs are empty"
        );
    }

    fn copy_recipient(id: u64) -> GameObject {
        GameObject::new(
            ObjectId(id),
            CardId(id),
            PlayerId(0),
            "Copy Recipient".to_string(),
            Zone::Battlefield,
        )
    }

    #[test]
    fn unchanged_copy_across_recomputation_keeps_copy_slots() {
        let values = trigger_copiable_values();
        let copy_effect = crate::types::ability::CopyEffectInstanceRef {
            continuous_effect_id: 17,
            modification_index: 2,
        };
        let mut recipient = copy_recipient(2);

        apply_copiable_values(&mut recipient, &values, copy_effect);
        let first = recipient
            .trigger_definitions
            .iter_all()
            .map(|entry| entry.occurrence.clone())
            .collect::<Vec<_>>();
        apply_copiable_values(&mut recipient, &values, copy_effect);
        let second = recipient
            .trigger_definitions
            .iter_all()
            .map(|entry| entry.occurrence.clone())
            .collect::<Vec<_>>();

        assert_eq!(first, second);
        assert!(matches!(
            first.as_slice(),
            [
                crate::types::ability::TriggerDefinitionOccurrenceRef::CopiedValue {
                    copy_effect: first_effect,
                    copied_slot: 0,
                },
                crate::types::ability::TriggerDefinitionOccurrenceRef::CopiedValue {
                    copy_effect: second_effect,
                    copied_slot: 1,
                },
            ] if *first_effect == copy_effect && *second_effect == copy_effect
        ));
    }

    #[test]
    fn replacement_copy_and_copy_of_copy_receive_new_recipient_copy_refs() {
        let values = trigger_copiable_values();
        let first_copy = crate::types::ability::CopyEffectInstanceRef {
            continuous_effect_id: 17,
            modification_index: 2,
        };
        let replacement_copy = crate::types::ability::CopyEffectInstanceRef {
            continuous_effect_id: 18,
            modification_index: 2,
        };
        let mut recipient = copy_recipient(2);

        apply_copiable_values(&mut recipient, &values, first_copy);
        let first_occurrence = recipient.trigger_definitions[0].occurrence.clone();
        apply_copiable_values(&mut recipient, &values, replacement_copy);
        let replacement_occurrence = recipient.trigger_definitions[0].occurrence.clone();

        assert_ne!(first_occurrence, replacement_occurrence);

        let copy_of_copy_effect = crate::types::ability::CopyEffectInstanceRef {
            continuous_effect_id: 19,
            modification_index: 2,
        };
        let mut copy_of_copy = copy_recipient(3);
        apply_copiable_values(&mut copy_of_copy, &values, copy_of_copy_effect);
        assert_ne!(
            copy_of_copy.trigger_definitions[0].occurrence, replacement_occurrence,
            "copy-of-copy must be keyed by its own winning copy-effect occurrence"
        );
        assert_ne!(
            recipient.trigger_definition_ref(&recipient.trigger_definitions[0]),
            copy_of_copy.trigger_definition_ref(&copy_of_copy.trigger_definitions[0]),
            "copy-of-copy must not import the source object's exact trigger ref"
        );
    }

    #[test]
    fn duplicate_base_install_uses_printed_slots_not_copy_effect_refs() {
        let values = trigger_copiable_values();
        let mut first_duplicate = copy_recipient(2);
        let mut second_duplicate = copy_recipient(3);

        install_copiable_values_as_base(&mut first_duplicate, &values);
        install_copiable_values_as_base(&mut second_duplicate, &values);

        for duplicate in [&first_duplicate, &second_duplicate] {
            assert!(duplicate.trigger_definitions.iter_all().all(|entry| {
                matches!(
                    entry.occurrence,
                    crate::types::ability::TriggerDefinitionOccurrenceRef::Printed { .. }
                )
            }));
        }
        assert_ne!(
            first_duplicate.trigger_definition_ref(&first_duplicate.trigger_definitions[0]),
            second_duplicate.trigger_definition_ref(&second_duplicate.trigger_definitions[0]),
            "fresh duplicated objects retain distinct source incarnation authority"
        );
    }

    #[test]
    fn full_face_replacement_allocates_a_distinct_printed_trigger_base_set() {
        let mut object = copy_recipient(4);
        let mut first_face = test_face(
            "First Face",
            "first-face-oracle-id",
            vec![CoreType::Creature],
            ManaCost::default(),
        );
        first_face.triggers = vec![TriggerDefinition::new(TriggerMode::Phase)];
        let mut replacement_face = test_face(
            "Replacement Face",
            "replacement-face-oracle-id",
            vec![CoreType::Creature],
            ManaCost::default(),
        );
        replacement_face.triggers = vec![TriggerDefinition::new(TriggerMode::Attacks)];

        apply_card_face_to_object(&mut object, &first_face);
        let first = object.trigger_definition_ref(&object.trigger_definitions[0]);
        apply_card_face_to_object(&mut object, &replacement_face);
        let replacement = object.trigger_definition_ref(&object.trigger_definitions[0]);

        assert_ne!(
            first, replacement,
            "a full face replacement must allocate a new printed trigger base-set generation"
        );
        assert!(matches!(
            replacement.occurrence,
            crate::types::ability::TriggerDefinitionOccurrenceRef::Printed { .. }
        ));
    }

    fn test_face(
        name: &str,
        oracle_id: &str,
        core_types: Vec<CoreType>,
        mana_cost: ManaCost,
    ) -> CardFace {
        CardFace {
            name: name.to_string(),
            mana_cost,
            card_type: CardType {
                supertypes: vec![],
                core_types,
                subtypes: vec![],
            },
            power: None,
            toughness: None,
            loyalty: None,
            defense: None,
            oracle_text: None,
            non_ability_text: None,
            flavor_name: None,
            keywords: Vec::<Keyword>::new(),
            abilities: Vec::<AbilityDefinition>::new(),
            triggers: Vec::<TriggerDefinition>::new(),
            static_abilities: Vec::<StaticDefinition>::new(),
            replacements: Vec::<ReplacementDefinition>::new(),
            cleave_variant: None,
            color_override: None,
            color_identity: vec![],
            scryfall_oracle_id: Some(oracle_id.to_string()),
            modal: None::<ModalChoice>,
            additional_cost: None::<AdditionalCost>,
            casting_restrictions: Vec::<CastingRestriction>::new(),
            casting_options: Vec::<SpellCastingOption>::new(),
            solve_condition: None::<SolveCondition>,
            strive_cost: None,
            parse_warnings: vec![],
            brawl_commander: false,
            is_commander: false,
            is_oathbreaker: false,
            deck_copy_limit: None,
            metadata: Default::default(),
            rarities: Default::default(),
            attraction_lights: vec![],
        }
    }

    /// CR 710.1c: a flip card's color and mana cost don't change if the
    /// permanent is flipped — including across a state reload.
    ///
    /// A flipped permanent's `printed_ref` names the ALTERNATIVE half, which (on
    /// every real flip card) has no printed mana cost. Without the
    /// `restore_normal_cost_and_color_if_flipped` call in
    /// `reapply_printed_faces_from_card_db`, the reapply blanks the cost and the
    /// permanent silently becomes a {0} object on load. Reverting that call
    /// fails the mana-cost assertion below.
    #[test]
    fn rehydrate_keeps_a_flipped_permanents_mana_cost_and_color() {
        let normal_cost = ManaCost::Cost {
            shards: vec![ManaCostShard::White],
            generic: 0,
        };
        let mut normal_half = test_face(
            "Rehydrate Flip Normal",
            "rehydrate-flip-oracle-id",
            vec![CoreType::Creature],
            normal_cost.clone(),
        );
        normal_half.color_override = Some(vec![ManaColor::White]);
        // CR 710.1b: the alternative half has no printed mana cost and no
        // printed color indicator — exactly as MTGJSON reports face b.
        let mut alternative_half = test_face(
            "Rehydrate Flip Alternative",
            "rehydrate-flip-oracle-id",
            vec![CoreType::Creature],
            ManaCost::default(),
        );
        alternative_half.color_override = Some(vec![]);
        let db = db_from_faces(&[normal_half.clone(), alternative_half.clone()]);

        let mut state = GameState::new_two_player(42);
        let id = create_object(
            &mut state,
            CardId(31),
            PlayerId(0),
            "Rehydrate Flip Alternative".to_string(),
            Zone::Battlefield,
        );
        let object = state.objects.get_mut(&id).unwrap();
        // Post-flip state, exactly as `flip::flip_permanent` leaves it: the
        // alternative half is displayed, the normal half is stashed, and the
        // mana cost / color are still the normal half's (CR 710.1c).
        object.flipped = true;
        object.printed_ref = printed_ref_from_face(&alternative_half);
        object.base_printed_ref = object.printed_ref.clone();
        object.mana_cost = normal_cost.clone();
        object.base_mana_cost = normal_cost.clone();
        object.color = vec![ManaColor::White];
        object.base_color = vec![ManaColor::White];
        object.back_face = Some(BackFaceData {
            name: normal_half.name.clone(),
            power: None,
            toughness: None,
            loyalty: None,
            printed_loyalty: None,
            defense: None,
            card_types: normal_half.card_type.clone(),
            mana_cost: normal_cost.clone(),
            keywords: vec![],
            abilities: vec![],
            trigger_definitions: Default::default(),
            replacement_definitions: Default::default(),
            static_definitions: Default::default(),
            color: vec![ManaColor::White],
            printed_ref: printed_ref_from_face(&normal_half),
            modal: None,
            additional_cost: None,
            strive_cost: None,
            casting_restrictions: vec![],
            casting_options: vec![],
            layout_kind: None,
            parse_warnings: vec![],
        });

        rehydrate_game_from_card_db(&mut state, &db);

        let object = &state.objects[&id];
        assert!(
            object.flipped,
            "reach guard: the permanent is still flipped"
        );
        assert_eq!(
            object.name, "Rehydrate Flip Alternative",
            "reach guard: the reapply really did run over the alternative half"
        );
        assert_eq!(
            object.mana_cost, normal_cost,
            "CR 710.1c: reloading must not blank a flipped permanent's mana cost"
        );
        assert_eq!(object.base_mana_cost, normal_cost);
        assert_eq!(
            object.color,
            vec![ManaColor::White],
            "CR 710.1c: reloading must not blank a flipped permanent's color"
        );
        assert_eq!(object.base_color, vec![ManaColor::White]);
    }

    /// CR 604.3: explicit all-zone color data is authoritative even when a face
    /// also has Devoid. Production devoid cards normally enter through this path
    /// with `color_override: Some([])`.
    #[test]
    fn color_override_wins_for_devoid_face() {
        let mut face = test_face(
            "Touch of the Void",
            "touch-of-the-void-oracle-id",
            vec![CoreType::Instant],
            ManaCost::Cost {
                shards: vec![ManaCostShard::Red],
                generic: 1,
            },
        );
        // Without Devoid, the {1}{R} cost would make it red.
        assert_eq!(
            derive_colors_from_mana_cost(&face.mana_cost),
            vec![ManaColor::Red]
        );
        face.color_override = Some(vec![ManaColor::Red]);
        face.keywords.push(Keyword::Devoid);

        let mut obj = GameObject::new(
            ObjectId(1),
            CardId(0),
            PlayerId(0),
            face.name.clone(),
            Zone::Hand,
        );
        apply_card_face_to_object(&mut obj, &face);

        assert_eq!(obj.color, vec![ManaColor::Red]);
        assert_eq!(obj.base_color, vec![ManaColor::Red]);
    }

    /// CR 702.114a + CR 604.3: if all-zone color data is missing, Devoid is a
    /// backstop that builds the face colorless outside the battlefield too.
    #[test]
    fn devoid_face_without_color_override_falls_back_to_colorless() {
        let mut face = test_face(
            "Muraganda Eldrazi",
            "muraganda-eldrazi-oracle-id",
            vec![CoreType::Creature],
            ManaCost::Cost {
                shards: vec![ManaCostShard::Green],
                generic: 3,
            },
        );
        face.keywords.push(Keyword::Devoid);

        let mut obj = GameObject::new(
            ObjectId(1),
            CardId(0),
            PlayerId(0),
            face.name.clone(),
            Zone::Hand,
        );
        apply_card_face_to_object(&mut obj, &face);

        assert!(
            obj.color.is_empty(),
            "devoid object must be colorless; got {:?}",
            obj.color
        );
        assert!(
            obj.base_color.is_empty(),
            "devoid base color must be colorless; got {:?}",
            obj.base_color
        );
    }

    /// CR 111.1 + CR 707.2 + CR 704.5j: A non-legendary token that's a copy of
    /// a legendary card (Miirym, Sentinel Wyrm — "create a token that's a copy
    /// of it, except it isn't legendary") carries the legendary card's
    /// `printed_ref` purely as a display/art hint. On game load,
    /// `rehydrate_game_from_card_db` must NOT re-apply the legendary printed
    /// face's copiable characteristics to the token — doing so wrongly re-adds
    /// the Legendary supertype, and two such same-name tokens then collapse
    /// under the legend rule on load. The token's synthesized characteristics
    /// are persisted in full, so rehydration must leave them untouched.
    #[test]
    fn rehydrate_preserves_non_legendary_token_copy_of_legendary() {
        // A legendary card face in the database. The tokens are non-legendary
        // copies of this card and carry its printed_ref for art lookup.
        let mut legendary = test_face(
            "Ancient Gold Dragon",
            "ancient-gold-dragon-oracle-id",
            vec![CoreType::Creature],
            ManaCost::default(),
        );
        legendary.card_type.supertypes = vec![crate::types::card_type::Supertype::Legendary];
        let export = serde_json::json!({
            "ancient gold dragon": serde_json::to_value(&legendary).unwrap(),
        })
        .to_string();
        let db = CardDatabase::from_json_str(&export).expect("export db should parse");

        let printed_ref = printed_ref_from_face(&legendary).unwrap();

        let mut state = GameState::new_two_player(42);

        // Two non-legendary tokens, each a copy of the legendary card (CR 707.2
        // with an "isn't legendary" exception): NOT legendary, but carrying the
        // legendary card's printed_ref as the art hint.
        let mut token_ids = Vec::new();
        for card_id in [CardId(10), CardId(11)] {
            let id = create_object(
                &mut state,
                card_id,
                PlayerId(0),
                "Ancient Gold Dragon".to_string(),
                Zone::Battlefield,
            );
            let obj = state.objects.get_mut(&id).unwrap();
            obj.is_token = true;
            // Non-legendary: the "isn't legendary" exception stamped at creation.
            obj.card_types = CardType {
                supertypes: vec![],
                core_types: vec![CoreType::Creature],
                subtypes: vec!["Dragon".to_string()],
            };
            obj.base_card_types = obj.card_types.clone();
            obj.base_characteristics_initialized = true;
            // Art hint only — points at the legendary printed card.
            obj.printed_ref = Some(printed_ref.clone());
            obj.base_printed_ref = Some(printed_ref.clone());
            token_ids.push(id);
        }

        // Simulate loading a saved game.
        rehydrate_game_from_card_db(&mut state, &db);

        // CR 205.4: Rehydration must not re-add the Legendary supertype to a
        // non-legendary token copy.
        for id in &token_ids {
            let obj = state.objects.get(id).unwrap();
            assert!(
                !obj.card_types
                    .supertypes
                    .contains(&crate::types::card_type::Supertype::Legendary),
                "rehydration must not make a non-legendary token copy legendary"
            );
            assert!(!obj
                .base_card_types
                .supertypes
                .contains(&crate::types::card_type::Supertype::Legendary));
            // The display/art pointer is still restored.
            assert_eq!(obj.printed_ref.as_ref(), Some(&printed_ref));
        }

        // CR 704.5j: The legend-rule SBA must NOT fire for two non-legendary
        // same-name tokens.
        let mut events = Vec::new();
        crate::game::sba::check_state_based_actions(&mut state, &mut events);
        assert!(
            !matches!(
                state.waiting_for,
                crate::types::game_state::WaitingFor::ChooseLegend { .. }
            ),
            "non-legendary token copies must not trigger the legend rule on load"
        );
    }

    /// CR 707.2 + CR 202.3: The Momir random-token pool's hydration map
    /// (`momir_pool_faces`) is `#[serde(skip)]`, while `momir_pool` is
    /// serialized. After a deserialize-then-rehydrate cycle (`restore_game_state`
    /// on worker restart / PWA update, or a peer sync), `momir_pool` is populated
    /// but `momir_pool_faces` is empty. Rehydration MUST rebuild the faces map in
    /// that state — otherwise `CreateTokenCopyFromPool` finds zero hydratable
    /// candidates and the Momir emblem silently makes no creature token. This is
    /// the discriminating guard: it fails if the rebuild is gated on
    /// `momir_pool.is_empty()` alone (the pre-fix behavior).
    #[test]
    fn momir_pool_faces_rebuilt_after_restore_drops_serde_skip_map() {
        // A mana-value-4 creature ({3}{G} = MV 4) is the only card in the pool.
        let creature = test_face(
            "Test Pool Beast",
            "test-pool-beast-oracle-id",
            vec![CoreType::Creature],
            ManaCost::Cost {
                shards: vec![ManaCostShard::Green],
                generic: 3,
            },
        );
        let export = serde_json::json!({
            "test pool beast": serde_json::to_value(&creature).unwrap(),
        })
        .to_string();
        let db = CardDatabase::from_json_str(&export).expect("export db should parse");

        let mut state = GameState::new_two_player(42);
        state.format_config = crate::types::format::FormatConfig::momir();

        // First hydration builds both the pool and the faces map.
        rehydrate_game_from_card_db(&mut state, &db);
        assert_eq!(
            state.momir_pool.get(&4).map(Vec::as_slice),
            Some(["Test Pool Beast".to_string()].as_slice()),
            "MV-4 creature must land in the pool keyed by mana value"
        );
        assert!(
            state.momir_pool_faces.contains_key("test pool beast"),
            "faces map must hydrate the MV-4 creature on first build"
        );

        // Simulate the serde round-trip: `momir_pool` survives, the
        // `#[serde(skip)]` faces map comes back empty.
        state.momir_pool_faces = std::sync::Arc::new(HashMap::new());
        assert!(!state.momir_pool.is_empty(), "pool persists across serde");

        // Rehydrating a restored game must repopulate the faces map even though
        // `momir_pool` is non-empty.
        rehydrate_game_from_card_db(&mut state, &db);
        assert!(
            state.momir_pool_faces.contains_key("test pool beast"),
            "faces map must be rebuilt after a restore that dropped the skip map"
        );
    }

    fn self_etb_plus_one_replacement(count: i32) -> ReplacementDefinition {
        crate::types::ability::ReplacementDefinition::new(ReplacementEvent::Moved)
            .destination_zone(Zone::Battlefield)
            .valid_card(TargetFilter::SelfRef)
            .execute(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::PutCounter {
                    counter_type: CounterType::Plus1Plus1,
                    count: QuantityExpr::Fixed { value: count },
                    target: TargetFilter::SelfRef,
                },
            ))
    }

    /// CR 614.1c: "~ enters with N +1/+1 counters on it" (Hangarback Walker /
    /// Walking Ballista class, like Atraxa's Skitterfang's oil counters) is
    /// modeled as a Moved→Battlefield self-replacement. The extractor must
    /// surface `(counter, N)` so a token copy — which bypasses the ZoneChange
    /// replacement pass — can seed those counters on entry.
    #[test]
    fn self_etb_counter_replacement_extracts_fixed_self_counters() {
        assert_eq!(
            self_etb_counter_replacements(&[self_etb_plus_one_replacement(3)]),
            vec![(CounterType::Plus1Plus1, 3)],
        );
    }

    /// A conditional "enters with" replacement is NOT a plain fixed-count self
    /// seed — it must be left to the normal replacement pass, not statically
    /// extracted (where the condition would be silently ignored). A replacement
    /// whose destination is not the battlefield must also be ignored.
    #[test]
    fn self_etb_counter_replacement_skips_conditional_and_non_self() {
        let conditional = ReplacementDefinition {
            condition: Some(
                crate::types::ability::ReplacementCondition::UnlessPlayerLifeAtMost { amount: 5 },
            ),
            ..self_etb_plus_one_replacement(1)
        };
        let wrong_zone = ReplacementDefinition {
            destination_zone: Some(Zone::Graveyard),
            ..self_etb_plus_one_replacement(1)
        };
        assert!(self_etb_counter_replacements(&[conditional, wrong_zone]).is_empty());
    }

    /// CR 111.1 + CR 707.2: The same token-copy rehydration rule applies to a
    /// serialized back face. Rehydration may refresh the display pointer, but it
    /// must not re-apply the printed back face's Legendary supertype to the
    /// token's persisted back-face characteristics.
    #[test]
    fn rehydrate_preserves_token_copy_back_face_characteristics() {
        let oracle_id = "token-copy-dfc-oracle-id";
        let mut front = test_face(
            "Legendary Front",
            oracle_id,
            vec![CoreType::Creature],
            ManaCost::default(),
        );
        front.card_type.supertypes = vec![crate::types::card_type::Supertype::Legendary];
        let mut back = test_face(
            "Legendary Back",
            oracle_id,
            vec![CoreType::Creature],
            ManaCost::default(),
        );
        back.card_type.supertypes = vec![crate::types::card_type::Supertype::Legendary];
        let export = serde_json::json!({
            "legendary front": serde_json::to_value(&front).unwrap(),
            "legendary back": serde_json::to_value(&back).unwrap(),
        })
        .to_string();
        let db = CardDatabase::from_json_str(&export).expect("export db should parse");

        let front_ref = printed_ref_from_face(&front).unwrap();
        let back_ref = printed_ref_from_face(&back).unwrap();

        let mut state = GameState::new_two_player(42);
        let id = create_object(
            &mut state,
            CardId(20),
            PlayerId(0),
            "Legendary Front".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.is_token = true;
        obj.card_types = CardType {
            supertypes: vec![],
            core_types: vec![CoreType::Creature],
            subtypes: vec!["Dragon".to_string()],
        };
        obj.base_card_types = obj.card_types.clone();
        obj.base_characteristics_initialized = true;
        obj.printed_ref = Some(front_ref.clone());
        obj.base_printed_ref = Some(front_ref);

        let mut token_back = snapshot_object_face(obj);
        token_back.name = "Legendary Back".to_string();
        token_back.card_types = CardType {
            supertypes: vec![],
            core_types: vec![CoreType::Creature],
            subtypes: vec!["Dragon".to_string()],
        };
        token_back.printed_ref = Some(back_ref.clone());
        obj.back_face = Some(token_back);

        rehydrate_game_from_card_db(&mut state, &db);

        let back_face = state.objects[&id]
            .back_face
            .as_ref()
            .expect("token back face should remain present");
        assert!(
            !back_face
                .card_types
                .supertypes
                .contains(&crate::types::card_type::Supertype::Legendary),
            "rehydration must not make a token back face legendary"
        );
        assert_eq!(back_face.printed_ref.as_ref(), Some(&back_ref));
    }

    #[test]
    fn ravenous_intrinsic_counters_use_paid_x() {
        let mut obj = GameObject::new(
            ObjectId(1),
            CardId(1),
            PlayerId(0),
            "Ravener".to_string(),
            Zone::Stack,
        );
        obj.keywords.push(Keyword::Ravenous);
        obj.cost_x_paid = Some(4);

        assert_eq!(
            intrinsic_etb_counters(&obj, None),
            vec![(CounterType::Plus1Plus1, 4)]
        );
    }

    #[test]
    fn x_loyalty_uses_the_resolving_spell_x_and_survives_copying() {
        let resolving = intrinsic_entry_counters_for_face(
            Some(PrintedLoyalty::X),
            Some(0),
            Some(3),
            None,
            &CardType::default(),
        );
        assert_eq!(resolving, vec![(CounterType::Loyalty, 3)]);

        let mut source = GameObject::new(
            ObjectId(1),
            CardId(1),
            PlayerId(0),
            "X Walker".to_string(),
            Zone::Battlefield,
        );
        source.base_printed_loyalty = Some(PrintedLoyalty::X);
        source.printed_loyalty = Some(PrintedLoyalty::X);
        source.base_loyalty = Some(0);
        source.loyalty = Some(0);

        let values = intrinsic_copiable_values(&source);
        assert_eq!(values.printed_loyalty, Some(PrintedLoyalty::X));

        let mut copy = GameObject::new(
            ObjectId(2),
            CardId(2),
            PlayerId(0),
            "Copy".to_string(),
            Zone::Battlefield,
        );
        install_copiable_values_as_base(&mut copy, &values);
        assert_eq!(copy.printed_loyalty, Some(PrintedLoyalty::X));
        assert_eq!(copy.base_printed_loyalty, Some(PrintedLoyalty::X));
        assert_eq!(
            intrinsic_etb_counters(&copy, None),
            Vec::new(),
            "CR 107.3g: a copied X-loyalty permanent that is not resolving a spell has X=0"
        );
    }

    /// CR 712.12: MDFC land face selection requires `LayoutKind::Modal` on the back
    /// face. When loading from the export path (card-data.json), the `layout` field
    /// in the export entry must be propagated through the layout_index so that
    /// `rehydrate_game_from_card_db` stamps the correct LayoutKind.
    #[test]
    fn rehydrate_populates_modal_dfc_layout_kind_from_export() {
        let cragcrown = test_face(
            "Cragcrown Pathway",
            "shared-mdfc-oracle-id",
            vec![CoreType::Land],
            ManaCost::default(),
        );
        let timbercrown = test_face(
            "Timbercrown Pathway",
            "shared-mdfc-oracle-id",
            vec![CoreType::Land],
            ManaCost::default(),
        );
        // Simulate an export with the `layout` field set (as oracle_gen now does).
        // Wrap each CardFace with the export-only `layout` field via JSON merge.
        let mut cragcrown_json = serde_json::to_value(&cragcrown).unwrap();
        cragcrown_json["layout"] = serde_json::json!("modal_dfc");
        let mut timbercrown_json = serde_json::to_value(&timbercrown).unwrap();
        timbercrown_json["layout"] = serde_json::json!("modal_dfc");
        let export = serde_json::json!({
            "cragcrown pathway": cragcrown_json,
            "timbercrown pathway": timbercrown_json,
        })
        .to_string();
        let db = CardDatabase::from_json_str(&export).expect("export db should parse");

        let mut state = GameState::default();
        let object_id = create_object_from_card_face(
            &mut state,
            db.get_face_by_name("Cragcrown Pathway").unwrap(),
            PlayerId(0),
        );

        rehydrate_game_from_card_db(&mut state, &db);

        let obj = state.objects.get(&object_id).unwrap();
        let back_face = obj
            .back_face
            .as_ref()
            .expect("rehydrate should attach the MDFC back face");
        assert_eq!(back_face.name, "Timbercrown Pathway");
        assert_eq!(
            back_face.layout_kind,
            Some(LayoutKind::Modal),
            "CR 712.12: MDFC back face must have LayoutKind::Modal for face choice prompt"
        );
    }

    #[test]
    fn rehydrate_populates_adventure_back_face_from_export_db() {
        let giant = test_face(
            "Bonecrusher Giant",
            "shared-adventure-oracle-id",
            vec![CoreType::Creature],
            ManaCost::Cost {
                shards: vec![ManaCostShard::Red],
                generic: 2,
            },
        );
        let stomp = test_face(
            "Stomp",
            "shared-adventure-oracle-id",
            vec![CoreType::Instant],
            ManaCost::Cost {
                shards: vec![ManaCostShard::Red],
                generic: 1,
            },
        );
        let mut giant_json = serde_json::to_value(&giant).unwrap();
        giant_json["layout"] = serde_json::json!("adventure");
        let mut stomp_json = serde_json::to_value(&stomp).unwrap();
        stomp_json["layout"] = serde_json::json!("adventure");
        let export = serde_json::json!({
            "bonecrusher giant": giant_json,
            "stomp": stomp_json,
        })
        .to_string();
        let db = CardDatabase::from_json_str(&export).expect("export db should parse");

        let mut state = GameState::default();
        let object_id = create_object_from_card_face(
            &mut state,
            db.get_face_by_name("Bonecrusher Giant").unwrap(),
            PlayerId(0),
        );
        let obj = state.objects.get(&object_id).unwrap();
        assert!(
            obj.back_face.is_none(),
            "precondition: deck loading starts with only the front face"
        );

        rehydrate_game_from_card_db(&mut state, &db);

        let obj = state.objects.get(&object_id).unwrap();
        let back_face = obj
            .back_face
            .as_ref()
            .expect("rehydrate should attach the adventure face");
        assert_eq!(back_face.name, "Stomp");
        assert_eq!(back_face.color, vec![ManaColor::Red]);
        assert_eq!(
            back_face.layout_kind,
            Some(LayoutKind::Adventure),
            "Adventure back face should carry LayoutKind::Adventure from export"
        );
    }

    /// CR 712.14a: Transform DFCs (Fable of the Mirror-Breaker) must hydrate
    /// `back_face` from the export so chapter-III `enter_transformed` returns
    /// work at resolution time.
    #[test]
    fn populate_back_face_attaches_transform_dfc_back_from_export() {
        let fable = test_face(
            "Fable of the Mirror-Breaker",
            "fable-oracle-id",
            vec![CoreType::Enchantment],
            ManaCost::Cost {
                shards: vec![ManaCostShard::Red],
                generic: 2,
            },
        );
        let reflection = test_face(
            "Reflection of Kiki-Jiki",
            "fable-oracle-id",
            vec![CoreType::Creature],
            ManaCost::default(),
        );
        let mut fable_json = serde_json::to_value(&fable).unwrap();
        fable_json["layout"] = serde_json::json!("transform");
        let mut reflection_json = serde_json::to_value(&reflection).unwrap();
        reflection_json["layout"] = serde_json::json!("transform");
        let export = serde_json::json!({
            "fable of the mirror-breaker": fable_json,
            "reflection of kiki-jiki": reflection_json,
        })
        .to_string();
        let db = CardDatabase::from_json_str(&export).expect("export db should parse");

        let mut state = GameState::default();
        let object_id = create_object_from_card_face(
            &mut state,
            db.get_face_by_name("Fable of the Mirror-Breaker").unwrap(),
            PlayerId(0),
        );
        let obj = state.objects.get_mut(&object_id).unwrap();
        populate_back_face_if_dfc(
            obj,
            &db,
            db.get_face_by_name("Fable of the Mirror-Breaker").unwrap(),
        );

        let back_face = obj
            .back_face
            .as_ref()
            .expect("transform DFC must hydrate back_face from export");
        assert_eq!(back_face.name, "Reflection of Kiki-Jiki");
        assert_eq!(
            back_face.layout_kind,
            Some(LayoutKind::Transform),
            "transform back face must carry LayoutKind::Transform"
        );
    }

    #[test]
    fn rehydrate_uses_hidden_prepare_face_when_back_face_name_collides() {
        let front = test_face(
            "Emeritus of Truce",
            "prepare-oracle-id",
            vec![CoreType::Creature],
            ManaCost::Cost {
                shards: vec![ManaCostShard::White],
                generic: 1,
            },
        );
        let prepare_back = test_face(
            "Swords to Plowshares",
            "prepare-oracle-id",
            vec![CoreType::Sorcery],
            ManaCost::Cost {
                shards: vec![ManaCostShard::White],
                generic: 0,
            },
        );
        let standalone = test_face(
            "Swords to Plowshares",
            "standalone-oracle-id",
            vec![CoreType::Instant],
            ManaCost::Cost {
                shards: vec![ManaCostShard::White],
                generic: 0,
            },
        );

        let mut front_json = serde_json::to_value(&front).unwrap();
        front_json["layout"] = serde_json::json!("prepare");
        let mut prepare_back_json = serde_json::to_value(&prepare_back).unwrap();
        prepare_back_json["layout"] = serde_json::json!("prepare");
        let standalone_json = serde_json::to_value(&standalone).unwrap();
        let export = serde_json::json!({
            "emeritus of truce": front_json,
            "swords to plowshares": standalone_json,
            "swords to plowshares [prepare-oracle-id]": prepare_back_json,
        })
        .to_string();
        let db = CardDatabase::from_json_str(&export).expect("export db should parse");
        assert_eq!(
            db.get_face_by_name("Swords to Plowshares")
                .unwrap()
                .scryfall_oracle_id
                .as_deref(),
            Some("standalone-oracle-id"),
            "canonical name lookup must keep the standalone card"
        );

        let mut state = GameState::default();
        let object_id = create_object_from_card_face(
            &mut state,
            db.get_face_by_name("Emeritus of Truce").unwrap(),
            PlayerId(0),
        );

        rehydrate_game_from_card_db(&mut state, &db);

        let back_face = state.objects[&object_id]
            .back_face
            .as_ref()
            .expect("rehydrate should attach the hidden prepare spell face");
        assert_eq!(back_face.name, "Swords to Plowshares");
        assert_eq!(back_face.layout_kind, Some(LayoutKind::Prepare));
    }

    #[test]
    fn rehydrate_preserves_face_down_battlefield_public_characteristics() {
        let mut face = test_face(
            "Hidden Sorcery",
            "face-down-rehydrate-oracle-id",
            vec![CoreType::Sorcery],
            ManaCost::Cost {
                shards: vec![ManaCostShard::Black],
                generic: 1,
            },
        );
        face.keywords.push(Keyword::Sneak(ManaCost::Cost {
            shards: vec![ManaCostShard::Black],
            generic: 0,
        }));
        let export = serde_json::json!({
            "hidden sorcery": serde_json::to_value(&face).unwrap(),
        })
        .to_string();
        let db = CardDatabase::from_json_str(&export).expect("export db should parse");

        let mut state = GameState::default();
        let object_id = create_object_from_card_face(
            &mut state,
            db.get_face_by_name("Hidden Sorcery").unwrap(),
            PlayerId(0),
        );
        state.battlefield.push_back(object_id);
        {
            let obj = state.objects.get_mut(&object_id).unwrap();
            obj.zone = Zone::Battlefield;
            obj.face_down = true;
            obj.back_face = Some(snapshot_object_face(obj));
        }

        rehydrate_game_from_card_db(&mut state, &db);

        let obj = state.objects.get(&object_id).unwrap();
        assert!(obj.face_down);
        assert_eq!(obj.name, "");
        assert_eq!(obj.card_types.core_types, vec![CoreType::Creature]);
        assert_eq!(obj.power, Some(2));
        assert_eq!(obj.toughness, Some(2));
        assert!(obj.keywords.is_empty());
        assert!(obj.abilities.is_empty());

        let hidden_face = obj
            .back_face
            .as_ref()
            .expect("face-down permanent should keep hidden original face");
        assert_eq!(hidden_face.name, "Hidden Sorcery");
        assert_eq!(hidden_face.card_types.core_types, vec![CoreType::Sorcery]);
        assert_eq!(hidden_face.keywords.len(), 1);

        state.active_player = PlayerId(1);
        assert!(
            crate::game::combat::get_valid_blocker_ids(&state).contains(&object_id),
            "rehydrated face-down battlefield permanents must be legal blocker candidates"
        );
    }

    fn test_class_face(name: &str, oracle_id: &str) -> CardFace {
        let mut face = test_face(
            name,
            oracle_id,
            vec![CoreType::Enchantment],
            ManaCost::default(),
        );
        face.card_type.subtypes.push("Class".to_string());
        face
    }

    /// CR 716.2b: "A Class retains its level even if it stops being a Class."
    /// Once a Class has advanced past level 1, that level must persist for as
    /// long as the permanent stays on the battlefield. `rehydrate_game_from_card_db`
    /// must not stomp the runtime level back to 1 when refreshing card-face
    /// characteristics on state load / multiplayer state-sync.
    #[test]
    fn rehydrate_preserves_advanced_class_level() {
        let face = test_class_face("Test Class", "test-class-oracle-id");
        let mut face_json = serde_json::to_value(&face).unwrap();
        face_json["layout"] = serde_json::json!("class");
        let export = serde_json::json!({
            "test class": face_json,
        })
        .to_string();
        let db = CardDatabase::from_json_str(&export).expect("export db should parse");

        let mut state = GameState::default();
        let object_id = create_object_from_card_face(
            &mut state,
            db.get_face_by_name("Test Class").unwrap(),
            PlayerId(0),
        );

        // Precondition: first-time face application seeded class_level=1.
        assert_eq!(
            state.objects.get(&object_id).unwrap().class_level,
            Some(1),
            "first-time face application should seed CR 716.3 level 1"
        );

        // Simulate the Class advancing to level 3 (e.g. via SetClassLevel).
        state.objects.get_mut(&object_id).unwrap().class_level = Some(3);

        // Rehydration must not reset the runtime level.
        rehydrate_game_from_card_db(&mut state, &db);

        assert_eq!(
            state.objects.get(&object_id).unwrap().class_level,
            Some(3),
            "CR 716.2b: rehydration must preserve the advanced level"
        );
    }

    /// CR 306.5c: Rehydration must preserve live loyalty counters on battlefield
    /// planeswalkers (Daretti, Scrap Savant regression).
    #[test]
    fn rehydrate_preserves_planeswalker_loyalty_counters() {
        let mut face = test_face(
            "Daretti, Scrap Savant",
            "daretti-scrap-savant-oracle-id",
            vec![CoreType::Planeswalker],
            ManaCost::default(),
        );
        face.loyalty = Some("3".to_string());
        let export = serde_json::json!({
            "daretti, scrap savant": serde_json::to_value(&face).unwrap(),
        })
        .to_string();
        let db = CardDatabase::from_json_str(&export).expect("export db should parse");

        let mut state = GameState::new_two_player(42);
        let pw_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Daretti, Scrap Savant".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&pw_id).unwrap();
            obj.card_types.core_types.push(CoreType::Planeswalker);
            obj.base_loyalty = Some(3);
            obj.loyalty = Some(1);
            obj.counters.insert(CounterType::Loyalty, 1);
            obj.base_characteristics_initialized = true;
            obj.printed_ref = printed_ref_from_face(&face);
            obj.base_printed_ref = obj.printed_ref.clone();
        }

        rehydrate_game_from_card_db(&mut state, &db);

        assert_eq!(
            state.objects.get(&pw_id).unwrap().loyalty,
            Some(1),
            "rehydration must not reset loyalty to printed base when counters differ"
        );
        assert_eq!(
            state
                .objects
                .get(&pw_id)
                .unwrap()
                .counters
                .get(&CounterType::Loyalty),
            Some(&1)
        );
    }

    /// CR 310.4c: Rehydration must preserve live defense counters on battlefield
    /// battles, matching the planeswalker loyalty path.
    #[test]
    fn rehydrate_preserves_battle_defense_counters() {
        let mut face = test_face(
            "Invasion of Testoria",
            "invasion-of-testoria-oracle-id",
            vec![CoreType::Battle],
            ManaCost::default(),
        );
        face.defense = Some("5".to_string());
        let export = serde_json::json!({
            "invasion of testoria": serde_json::to_value(&face).unwrap(),
        })
        .to_string();
        let db = CardDatabase::from_json_str(&export).expect("export db should parse");

        let mut state = GameState::new_two_player(42);
        let battle_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Invasion of Testoria".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&battle_id).unwrap();
            obj.card_types.core_types.push(CoreType::Battle);
            obj.base_defense = Some(5);
            obj.defense = Some(2);
            obj.counters.insert(CounterType::Defense, 2);
            obj.base_characteristics_initialized = true;
            obj.printed_ref = printed_ref_from_face(&face);
            obj.base_printed_ref = obj.printed_ref.clone();
        }

        rehydrate_game_from_card_db(&mut state, &db);

        assert_eq!(
            state.objects.get(&battle_id).unwrap().defense,
            Some(2),
            "rehydration must not reset defense to printed base when counters differ"
        );
        assert_eq!(
            state
                .objects
                .get(&battle_id)
                .unwrap()
                .counters
                .get(&CounterType::Defense),
            Some(&2)
        );
    }

    /// CR 716.3: A fresh Class entering the battlefield seeds at level 1. The
    /// `was_initialized` gate must not block first-time application.
    #[test]
    fn first_time_face_application_seeds_class_level_one() {
        let face = test_class_face("Fresh Class", "fresh-class-oracle-id");

        let mut obj = GameObject::new(
            ObjectId(1),
            CardId(1),
            PlayerId(0),
            "Fresh Class".to_string(),
            Zone::Battlefield,
        );
        // Precondition: a fresh GameObject has not been initialized.
        assert!(!obj.base_characteristics_initialized);
        assert_eq!(obj.class_level, None);

        apply_card_face_to_object(&mut obj, &face);

        assert_eq!(
            obj.class_level,
            Some(1),
            "CR 716.3: first-time face application of a Class must seed level 1"
        );
        assert!(
            obj.base_characteristics_initialized,
            "first-time application must mark the object initialized"
        );
    }

    // -----------------------------------------------------------------------
    // Conjure registry scoping tests
    // -----------------------------------------------------------------------

    /// Build a `CardDatabase` from in-memory faces via the export JSON path so
    /// `get_face_by_name` / `get_face_by_printed_ref` resolve exactly as in
    /// production. Each face must carry a distinct oracle id.
    fn db_from_faces(faces: &[CardFace]) -> CardDatabase {
        let mut map = serde_json::Map::new();
        for face in faces {
            map.insert(
                face.name.to_lowercase(),
                serde_json::to_value(face).unwrap(),
            );
        }
        let json = serde_json::Value::Object(map).to_string();
        CardDatabase::from_json_str(&json).expect("export db should parse")
    }

    /// CR 701.42b + CR 712.4: the production JSON loader's layout metadata,
    /// not arbitrary effect text, is the authority for canonical meld pairs.
    #[test]
    fn real_card_database_builds_only_canonical_meld_pair_registry_entries() {
        let source_name = "Registry Meld Source";
        let partner_name = "Registry Meld Partner";
        let result_name = "Registry Meld Result";
        let forged_result_name = "Ordinary Forged Result";
        let cross_pair_result_name = "Other Pair Meld Result";
        let mut source = test_face(
            source_name,
            "registry-meld-source-oracle",
            vec![CoreType::Creature],
            ManaCost::default(),
        );
        for result in [result_name, forged_result_name, cross_pair_result_name] {
            source.abilities.push(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Meld {
                    source: source_name.to_string(),
                    partner: partner_name.to_string(),
                    result: result.to_string(),
                    source_filter: TargetFilter::SelfRef,
                    partner_filter: TargetFilter::Any,
                    entry: crate::types::ability::PermanentEntryMode::Normal,
                },
            ));
        }
        let partner = test_face(
            partner_name,
            "registry-meld-partner-oracle",
            vec![CoreType::Creature],
            ManaCost::default(),
        );
        let result_for_source = test_face(
            result_name,
            "registry-meld-source-oracle",
            vec![CoreType::Creature],
            ManaCost::default(),
        );
        let result_for_partner = test_face(
            result_name,
            "registry-meld-partner-oracle",
            vec![CoreType::Creature],
            ManaCost::default(),
        );
        let other_front = test_face(
            "Other Pair Meld Front",
            "other-pair-meld-oracle",
            vec![CoreType::Creature],
            ManaCost::default(),
        );
        let cross_pair_result = test_face(
            cross_pair_result_name,
            "other-pair-meld-oracle",
            vec![CoreType::Creature],
            ManaCost::default(),
        );
        let forged = test_face(
            forged_result_name,
            "ordinary-forged-result-oracle",
            vec![CoreType::Creature],
            ManaCost::default(),
        );

        let mut export = serde_json::Map::new();
        for (key, face, layout) in [
            (source.name.to_lowercase(), &source, "meld"),
            (
                result_for_source.name.to_lowercase(),
                &result_for_source,
                "meld",
            ),
            (partner.name.to_lowercase(), &partner, "meld"),
            (
                "hidden partner meld result".to_string(),
                &result_for_partner,
                "meld",
            ),
            (other_front.name.to_lowercase(), &other_front, "meld"),
            (
                cross_pair_result.name.to_lowercase(),
                &cross_pair_result,
                "meld",
            ),
            (forged.name.to_lowercase(), &forged, "normal"),
        ] {
            let mut json = serde_json::to_value(face).unwrap();
            json["layout"] = serde_json::json!(layout);
            export.insert(key, json);
        }
        let db = CardDatabase::from_json_str(&serde_json::Value::Object(export).to_string())
            .expect("production CardDatabase export should parse");

        let registry = build_meld_pair_registry(&db);
        let key = meld_pair_key(source_name, partner_name);
        assert_eq!(registry.len(), 1, "non-meld result faces are rejected");
        assert_eq!(
            registry.get(&key),
            Some(&MeldPairRecord {
                source: source_name.to_string(),
                partner: partner_name.to_string(),
                result: result_name.to_string(),
            })
        );

        let mut state = GameState::new_two_player(42);
        rehydrate_game_from_card_db(&mut state, &db);
        assert_eq!(state.meld_pair_registry.as_ref(), &registry);
    }

    fn conjure_ability(target_name: &str, destination: Zone) -> AbilityDefinition {
        AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Conjure {
                cards: vec![ConjureCard {
                    source: ConjureSource::Named {
                        name: target_name.to_string(),
                    },
                    count: QuantityExpr::Fixed { value: 1 },
                }],
                destination,
                tapped: false,
                library_position: None,
                library_players: None,
            },
        )
    }

    fn deck_entry(card: CardFace) -> DeckEntry {
        DeckEntry { card, count: 1 }
    }

    #[test]
    fn registry_scopes_to_reachable_conjure_targets_not_full_db() {
        // The seed deck card conjures exactly one target. The database also
        // holds many unrelated faces that must NOT enter the registry.
        let mut conjurer = test_face(
            "Conjurer Source",
            "oracle-conjurer",
            vec![CoreType::Sorcery],
            ManaCost::default(),
        );
        conjurer
            .abilities
            .push(conjure_ability("Conjured Spirit", Zone::Battlefield));

        let target = test_face(
            "Conjured Spirit",
            "oracle-spirit",
            vec![CoreType::Creature],
            ManaCost::default(),
        );

        // Unrelated noise that should be excluded by scoping.
        let noise_a = test_face(
            "Noise A",
            "oracle-noise-a",
            vec![CoreType::Land],
            ManaCost::default(),
        );
        let noise_b = test_face(
            "Noise B",
            "oracle-noise-b",
            vec![CoreType::Instant],
            ManaCost::default(),
        );

        let db = db_from_faces(&[conjurer.clone(), target.clone(), noise_a, noise_b]);

        let mut state = GameState::default();
        create_object_from_card_face(&mut state, &conjurer, PlayerId(0));

        rehydrate_game_from_card_db(&mut state, &db);

        assert_eq!(
            state.card_face_registry.len(),
            1,
            "registry must hold only the reachable conjure target, not the full db"
        );
        assert!(
            state.card_face_registry.contains_key("conjured spirit"),
            "the conjure target must be present, keyed lowercase"
        );
    }

    #[test]
    fn registry_empty_when_no_conjure_cards() {
        let vanilla = test_face(
            "Vanilla Bear",
            "oracle-vanilla",
            vec![CoreType::Creature],
            ManaCost::default(),
        );
        let db = db_from_faces(std::slice::from_ref(&vanilla));

        let mut state = GameState::default();
        create_object_from_card_face(&mut state, &vanilla, PlayerId(0));

        rehydrate_game_from_card_db(&mut state, &db);

        assert!(
            state.card_face_registry.is_empty(),
            "non-conjure decks must produce an empty registry (no allocation spike)"
        );
    }

    #[test]
    fn registry_keys_mixed_case_conjure_target_lowercase() {
        // B3: a conjure target whose printed name has capitals must be keyed by
        // its lowercased form so the handler's `name.to_lowercase()` lookup hits.
        let mut conjurer = test_face(
            "Mixed Case Conjurer",
            "oracle-mixed-conjurer",
            vec![CoreType::Sorcery],
            ManaCost::default(),
        );
        conjurer
            .abilities
            .push(conjure_ability("Aetherflux Reservoir", Zone::Battlefield));

        let target = test_face(
            "Aetherflux Reservoir",
            "oracle-aetherflux",
            vec![CoreType::Artifact],
            ManaCost::default(),
        );

        let db = db_from_faces(&[conjurer.clone(), target.clone()]);

        let mut state = GameState::default();
        create_object_from_card_face(&mut state, &conjurer, PlayerId(0));

        rehydrate_game_from_card_db(&mut state, &db);

        // Mirror the conjure handler's lookup (game/effects/conjure.rs).
        let resolved = state
            .card_face_registry
            .get(&"Aetherflux Reservoir".to_lowercase());
        assert!(
            resolved.is_some(),
            "mixed-case conjure target must resolve via lowercased key"
        );
        assert_eq!(resolved.unwrap().name, "Aetherflux Reservoir");
    }

    #[test]
    fn registry_follows_transitive_conjure_chain() {
        // A conjures B, B conjures C → registry must contain B and C.
        let mut card_a = test_face(
            "Card A",
            "oracle-a",
            vec![CoreType::Sorcery],
            ManaCost::default(),
        );
        card_a.abilities.push(conjure_ability("Card B", Zone::Hand));

        let mut card_b = test_face(
            "Card B",
            "oracle-b",
            vec![CoreType::Sorcery],
            ManaCost::default(),
        );
        card_b.abilities.push(conjure_ability("Card C", Zone::Hand));

        let card_c = test_face(
            "Card C",
            "oracle-c",
            vec![CoreType::Creature],
            ManaCost::default(),
        );

        let db = db_from_faces(&[card_a.clone(), card_b.clone(), card_c.clone()]);

        let mut state = GameState::default();
        // Seed Card A via the deck pool to also exercise the deck-pool seed path.
        state
            .deck_pools
            .push(crate::types::game_state::PlayerDeckPool {
                player: PlayerId(0),
                current_main: std::sync::Arc::new(vec![deck_entry(card_a.clone())]),
                ..Default::default()
            });

        rehydrate_game_from_card_db(&mut state, &db);

        assert_eq!(state.card_face_registry.len(), 2);
        assert!(state.card_face_registry.contains_key("card b"));
        assert!(
            state.card_face_registry.contains_key("card c"),
            "transitive conjure (B conjures C) must be followed to fixpoint"
        );
    }

    /// FIELD-COVERAGE: place an `Effect::Conjure` in EVERY nested ability/effect
    /// carrier and assert the walker collects all names. A future struct gaining
    /// a new `Box<AbilityDefinition>` field is NOT caught by the compiler (it is
    /// struct-field access, not a match arm) — this test is that safety net.
    #[test]
    fn walker_covers_every_nested_carrier() {
        let mut names: Vec<String> = Vec::new();

        // sub_ability / else_ability / mode_abilities on AbilityDefinition.
        let mut def = AbilityDefinition::new(AbilityKind::Spell, Effect::Investigate);
        def.sub_ability = Some(Box::new(conjure_ability("sub", Zone::Hand)));
        def.else_ability = Some(Box::new(conjure_ability("else", Zone::Hand)));
        def.mode_abilities.push(conjure_ability("mode", Zone::Hand));
        // cost: EffectCost carrying a Conjure effect.
        def.cost = Some(AbilityCost::EffectCost {
            effect: Box::new(Effect::Conjure {
                cards: vec![ConjureCard {
                    source: ConjureSource::Named {
                        name: "cost".to_string(),
                    },
                    count: QuantityExpr::Fixed { value: 1 },
                }],
                destination: Zone::Hand,
                tapped: false,
                library_position: None,
                library_players: None,
            }),
        });
        def.unless_pay = Some(UnlessPayModifier {
            cost: AbilityCost::EffectCost {
                effect: Box::new(Effect::Conjure {
                    cards: vec![ConjureCard {
                        source: ConjureSource::Named {
                            name: "unless_pay_ability".to_string(),
                        },
                        count: QuantityExpr::Fixed { value: 1 },
                    }],
                    destination: Zone::Hand,
                    tapped: false,
                    library_position: None,
                    library_players: None,
                }),
            },
            payer: TargetFilter::Controller,
        });
        walk_ability_def(&def, &mut names);

        // Effect-level carriers.
        let vote = Effect::Vote {
            choices: vec!["x".into()],
            per_choice_effect: vec![Box::new(conjure_ability("vote", Zone::Hand))],
            starting_with: ControllerRef::You,
            voter_scope: VoterScope::AllPlayers,
            tally_mode: crate::types::ability::VoteTally::PerVote,
            subject: crate::types::ability::VoteSubject::Named,
            visibility: crate::types::ability::VoteVisibility::Open,
        };
        walk_effect(&vote, &mut names);

        let piles = Effect::SeparateIntoPiles {
            partition_subject: VoterScope::EachOpponent,
            object_filter: TargetFilter::Any,
            chooser: PlayerScope::Controller,
            chosen_pile_effect: Box::new(conjure_ability("piles", Zone::Hand)),
            pile_source: crate::types::ability::PileSource::Battlefield,
            unchosen_pile_effect: None,
        };
        walk_effect(&piles, &mut names);

        let reveal = Effect::RevealFromHand {
            filter: TargetFilter::Any,
            on_decline: Some(Box::new(conjure_ability("on_decline", Zone::Hand))),
        };
        walk_effect(&reveal, &mut names);

        let delayed = Effect::CreateDelayedTrigger {
            condition: DelayedTriggerCondition::AtNextPhase {
                phase: Phase::Upkeep,
            },
            effect: Box::new(conjure_ability("delayed", Zone::Hand)),
            uses_tracked_set: false,
        };
        walk_effect(&delayed, &mut names);

        let flip = Effect::FlipCoin {
            win_effect: Some(Box::new(conjure_ability("flip_win", Zone::Hand))),
            lose_effect: Some(Box::new(conjure_ability("flip_lose", Zone::Hand))),
            flipper: crate::types::ability::TargetFilter::Controller,
        };
        walk_effect(&flip, &mut names);

        let until_lose = Effect::FlipCoinUntilLose {
            win_effect: Box::new(conjure_ability("until_lose", Zone::Hand)),
        };
        walk_effect(&until_lose, &mut names);

        let roll = Effect::RollDie {
            count: QuantityExpr::Fixed { value: 1 },
            sides: 6,
            results: vec![DieResultBranch {
                min: 1,
                max: 6,
                effect: Box::new(conjure_ability("roll", Zone::Hand)),
            }],
            modifier: None,
        };
        walk_effect(&roll, &mut names);

        let choose_one = Effect::ChooseOneOf {
            chooser: PlayerFilter::Controller,
            branches: vec![conjure_ability("choose_one", Zone::Hand)],
        };
        walk_effect(&choose_one, &mut names);

        // GenericEffect applies static abilities at resolution; descend into the
        // granted definitions.
        let mut generic_static = StaticDefinition::new(StaticMode::Continuous);
        generic_static
            .modifications
            .push(ContinuousModification::GrantAbility {
                definition: Box::new(conjure_ability("generic_effect", Zone::Hand)),
            });
        let generic = Effect::GenericEffect {
            static_abilities: vec![generic_static],
            duration: None,
            target: None,
            end_cost: None,
        };
        walk_effect(&generic, &mut names);

        // AddTargetReplacement carries a nested ReplacementDefinition that may conjure.
        let mut atr_replacement = ReplacementDefinition::new(ReplacementEvent::ChangeZone);
        atr_replacement.execute = Some(Box::new(conjure_ability(
            "add_target_replacement",
            Zone::Hand,
        )));
        let add_target_repl = Effect::AddTargetReplacement {
            replacement: Box::new(atr_replacement),
            target: TargetFilter::Any,
        };
        walk_effect(&add_target_repl, &mut names);

        // Token can grant static abilities that conjure.
        let mut token_static = StaticDefinition::new(StaticMode::Continuous);
        token_static
            .modifications
            .push(ContinuousModification::GrantAbility {
                definition: Box::new(conjure_ability("token_static", Zone::Hand)),
            });
        let token = Effect::Token {
            name: "T".to_string(),
            power: PtValue::Fixed(1),
            toughness: PtValue::Fixed(1),
            types: vec!["Creature".to_string()],
            colors: vec![],
            keywords: vec![],
            tapped: false,
            count: QuantityExpr::Fixed { value: 1 },
            owner: TargetFilter::Controller,
            attach_to: None,
            enters_attacking: false,
            supertypes: vec![],
            static_abilities: vec![token_static],
            enter_with_counters: vec![],
        };
        walk_effect(&token, &mut names);

        // Emblem hosts static + triggered abilities that conjure.
        let mut emblem_static = StaticDefinition::new(StaticMode::Continuous);
        emblem_static
            .modifications
            .push(ContinuousModification::GrantAbility {
                definition: Box::new(conjure_ability("emblem_static", Zone::Hand)),
            });
        let mut emblem_trigger = TriggerDefinition::new(TriggerMode::ChangesZone);
        emblem_trigger.execute = Some(Box::new(conjure_ability("emblem_trigger", Zone::Hand)));
        let emblem = Effect::CreateEmblem {
            statics: vec![emblem_static],
            triggers: vec![emblem_trigger],
        };
        walk_effect(&emblem, &mut names);

        // Counter.source_rider (LosesAbilities) may grant an ability that conjures.
        let mut counter_static = StaticDefinition::new(StaticMode::Continuous);
        counter_static
            .modifications
            .push(ContinuousModification::GrantAbility {
                definition: Box::new(conjure_ability("counter_source_static", Zone::Hand)),
            });
        let counter = Effect::Counter {
            target: TargetFilter::Any,
            source_rider: Some(CounterSourceRider::LosesAbilities {
                static_def: Box::new(counter_static),
                duration: Box::new(crate::types::ability::Duration::UntilHostLeavesPlay),
            }),
            countered_spell_zone: None,
        };
        walk_effect(&counter, &mut names);

        // Trigger / replacement / static carriers via CardFace.
        let mut face = test_face(
            "Carrier Face",
            "oracle-carrier",
            vec![CoreType::Creature],
            ManaCost::default(),
        );
        let mut trigger = TriggerDefinition::new(TriggerMode::ChangesZone);
        trigger.execute = Some(Box::new(conjure_ability("trigger", Zone::Hand)));
        trigger.unless_pay = Some(UnlessPayModifier {
            cost: AbilityCost::EffectCost {
                effect: Box::new(Effect::Conjure {
                    cards: vec![ConjureCard {
                        source: ConjureSource::Named {
                            name: "unless_pay_trigger".to_string(),
                        },
                        count: QuantityExpr::Fixed { value: 1 },
                    }],
                    destination: Zone::Hand,
                    tapped: false,
                    library_position: None,
                    library_players: None,
                }),
            },
            payer: TargetFilter::Controller,
        });
        face.triggers.push(trigger);

        let mut replacement = ReplacementDefinition::new(ReplacementEvent::ChangeZone);
        replacement.execute = Some(Box::new(conjure_ability("replacement", Zone::Hand)));
        face.replacements.push(replacement);

        // Static carrying a granted ability whose effect conjures.
        let mut static_def = StaticDefinition::new(StaticMode::Continuous);
        static_def
            .modifications
            .push(ContinuousModification::GrantAbility {
                definition: Box::new(conjure_ability("granted_ability", Zone::Hand)),
            });
        face.static_abilities.push(static_def);

        // ReplacementMode carriers: MayCost { cost, decline } and Optional { decline }.
        let mut repl_maycost = ReplacementDefinition::new(ReplacementEvent::ChangeZone);
        repl_maycost.mode = ReplacementMode::MayCost {
            cost: AbilityCost::EffectCost {
                effect: Box::new(Effect::Conjure {
                    cards: vec![ConjureCard {
                        source: ConjureSource::Named {
                            name: "repl_maycost_cost".to_string(),
                        },
                        count: QuantityExpr::Fixed { value: 1 },
                    }],
                    destination: Zone::Hand,
                    tapped: false,
                    library_position: None,
                    library_players: None,
                }),
            },
            decline: Some(Box::new(conjure_ability(
                "repl_maycost_decline",
                Zone::Hand,
            ))),
        };
        face.replacements.push(repl_maycost);

        let mut repl_optional = ReplacementDefinition::new(ReplacementEvent::ChangeZone);
        repl_optional.mode = ReplacementMode::Optional {
            decline: Some(Box::new(conjure_ability(
                "repl_optional_decline",
                Zone::Hand,
            ))),
        };
        face.replacements.push(repl_optional);

        // CR 614.11: CreateDrawReplacement nests its substitute Effect; the
        // walker must descend into it (Words of Worship/Wilding class).
        let draw_repl = Effect::CreateDrawReplacement {
            replacement_effect: Box::new(Effect::Conjure {
                cards: vec![ConjureCard {
                    source: ConjureSource::Named {
                        name: "draw_replacement".to_string(),
                    },
                    count: QuantityExpr::Fixed { value: 1 },
                }],
                destination: Zone::Hand,
                tapped: false,
                library_position: None,
                library_players: None,
            }),
        };
        walk_effect(&draw_repl, &mut names);

        collect_conjure_names_from_face(&face, &mut names);

        let expected = [
            "sub",
            "else",
            "mode",
            "cost",
            "vote",
            "piles",
            "on_decline",
            "delayed",
            "flip_win",
            "flip_lose",
            "until_lose",
            "roll",
            "choose_one",
            "trigger",
            "replacement",
            "granted_ability",
            "generic_effect",
            "repl_maycost_cost",
            "repl_maycost_decline",
            "repl_optional_decline",
            "add_target_replacement",
            "token_static",
            "emblem_static",
            "emblem_trigger",
            "counter_source_static",
            "unless_pay_ability",
            "unless_pay_trigger",
            "draw_replacement",
        ];
        for name in expected {
            assert!(
                names.iter().any(|n| n == name),
                "walker missed conjure name '{name}' in a nested carrier"
            );
        }
    }

    /// Issue #581: rehydration must repair a partially stale derived index before
    /// `finalize_public_state` flushes layers (which would mask a missing repair).
    #[test]
    fn rehydrate_repairs_stale_trigger_index_before_layer_flush() {
        use crate::game::trigger_index::{candidates_for_event, reindex_object_triggers};
        use crate::types::events::GameEvent;
        use crate::types::triggers::TriggerEventKey;

        let mut face = test_face(
            "Test Upkeep Enchantment",
            "test-upkeep-enchantment-oracle-id",
            vec![CoreType::Enchantment],
            ManaCost::default(),
        );
        face.triggers
            .push(TriggerDefinition::new(TriggerMode::PayCumulativeUpkeep));

        let export = serde_json::json!({
            "test upkeep enchantment": serde_json::to_value(&face).unwrap(),
        })
        .to_string();
        let db = CardDatabase::from_json_str(&export).expect("export db should parse");

        let mut state = GameState::new_two_player(42);
        let id = create_object_from_card_face(&mut state, &face, PlayerId(0));
        {
            let obj = state.objects.get_mut(&id).unwrap();
            obj.zone = Zone::Battlefield;
        }
        state.battlefield.push_back(id);
        reindex_object_triggers(&mut state, id);

        let upkeep_key = TriggerEventKey::BeginningOfPhase(Phase::Upkeep);
        if let Some(bucket) = state.trigger_index.by_key.get_mut(&upkeep_key) {
            bucket.retain(|oid| *oid != id);
            if bucket.is_empty() {
                state.trigger_index.by_key.remove(&upkeep_key);
            }
        }
        state.trigger_index.unclassified.retain(|oid| *oid != id);
        state
            .trigger_index
            .by_key
            .entry(TriggerEventKey::BeginningOfPhase(Phase::Draw))
            .or_default()
            .push(id);

        let before = candidates_for_event(
            &state,
            &GameEvent::PhaseChanged {
                phase: Phase::Upkeep,
            },
        );
        assert!(
            !before.contains(&id),
            "precondition: stale index must omit the upkeep permanent"
        );

        let (_, changed_battlefield) = reapply_printed_faces_from_card_db(&mut state, &db);
        repair_battlefield_trigger_index_after_face_reapply(&mut state, changed_battlefield);

        let after = candidates_for_event(
            &state,
            &GameEvent::PhaseChanged {
                phase: Phase::Upkeep,
            },
        );
        assert!(
            after.contains(&id),
            "rehydrate must rebuild the derived index before layer flush (issue #581)"
        );
    }

    /// Issue #3253: card-data rehydration must not leave a bestowed permanent
    /// without the synthesized Aura subtype while `bestow_form` remains set.
    #[test]
    fn rehydrate_resyncs_bestow_aura_subtype() {
        use crate::game::game_object::{AttachTarget, BestowFormState};

        let mut face = test_face(
            "Springheart Nantuko",
            "springheart-oracle",
            vec![CoreType::Enchantment, CoreType::Creature],
            ManaCost::Cost {
                shards: vec![ManaCostShard::Green, ManaCostShard::Green],
                generic: 1,
            },
        );
        face.card_type.subtypes = vec!["Insect".into(), "Monk".into()];
        let db = db_from_faces(&[face.clone()]);
        let printed_ref = printed_ref_from_face(&face).unwrap();

        let mut state = GameState::new_two_player(3253);
        let host_id = create_object(
            &mut state,
            CardId(3253),
            PlayerId(0),
            "Host".into(),
            Zone::Battlefield,
        );
        let aura_id = create_object(
            &mut state,
            CardId(3254),
            PlayerId(0),
            "Springheart Nantuko".into(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&aura_id).unwrap();
            obj.card_types = face.card_type.clone();
            obj.base_card_types = face.card_type.clone();
            obj.printed_ref = Some(printed_ref.clone());
            obj.base_printed_ref = Some(printed_ref);
            obj.base_characteristics_initialized = true;
        }

        crate::game::casting::apply_bestow_aura_form(state.objects.get_mut(&aura_id).unwrap());
        {
            let obj = state.objects.get_mut(&aura_id).unwrap();
            obj.attached_to = Some(AttachTarget::Object(host_id));
        }

        rehydrate_game_from_card_db(&mut state, &db);

        let obj = state.objects.get(&aura_id).unwrap();
        assert_eq!(obj.zone, Zone::Battlefield);
        assert_eq!(obj.bestow_form, Some(BestowFormState));
        assert!(
            obj.card_types.subtypes.iter().any(|s| s == "Aura"),
            "live subtypes must include Aura after rehydrate, got {:?}",
            obj.card_types.subtypes
        );
        assert!(
            obj.base_card_types.subtypes.iter().any(|s| s == "Aura"),
            "base subtypes must include Aura after rehydrate, got {:?}",
            obj.base_card_types.subtypes
        );
        assert!(
            !obj.card_types.core_types.contains(&CoreType::Creature),
            "bestowed object must not keep Creature core type"
        );
    }
}
