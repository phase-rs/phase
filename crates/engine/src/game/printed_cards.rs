use crate::database::synthesis::KeywordTriggerInstaller;
use crate::database::CardDatabase;
use crate::types::ability::{
    AbilityCost, AbilityDefinition, ConjureSource, ContinuousModification, CopiableValues,
    CounterSourceRider, Effect, PtValue, QuantityExpr, ReplacementCondition, ReplacementDefinition,
    ReplacementMode, StaticDefinition, TargetFilter, TriggerDefinition, VoteSubject,
};
use crate::types::card::{CardFace, CardLayout, LayoutKind, PrintedCardRef};
use crate::types::card_type::{CardType, CoreType};
use crate::types::counter::CounterType;
use crate::types::game_state::GameState;
use crate::types::identifiers::ObjectId;
use crate::types::keywords::Keyword;
use crate::types::mana::{ManaColor, ManaCost, ManaCostShard};
use crate::types::replacements::ReplacementEvent;
use crate::types::zones::Zone;
use std::collections::HashMap;
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
    let loyalty = card_face
        .loyalty
        .as_ref()
        .and_then(|value| value.parse::<u32>().ok());
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
    obj.trigger_definitions = card_face.triggers.clone().into();
    obj.replacement_definitions = replacements.clone().into();
    obj.static_definitions = card_face.static_abilities.clone().into();
    // CR 702.148a-b: Carry the cleave-cost ability set onto the object so the
    // casting flow can swap it in when the spell is cast for its cleave cost.
    obj.cleave_variant = card_face.cleave_variant.clone();
    obj.color = color.clone();
    obj.base_power = power;
    obj.base_toughness = toughness;
    obj.base_name = card_face.name.clone();
    obj.base_loyalty = loyalty;
    obj.base_defense = defense;
    obj.base_card_types = card_face.card_type.clone();
    obj.base_mana_cost = card_face.mana_cost.clone();
    obj.base_keywords = keywords;
    obj.base_abilities = Arc::new(abilities);
    obj.base_trigger_definitions = Arc::new(card_face.triggers.clone());
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
    let loyalty = card_face
        .loyalty
        .as_ref()
        .and_then(|value| value.parse::<u32>().ok());
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
}

pub fn apply_back_face_to_object(obj: &mut GameObject, back_face: BackFaceData) {
    obj.name = back_face.name.clone();
    obj.power = back_face.power;
    obj.toughness = back_face.toughness;
    obj.loyalty = back_face.loyalty;
    obj.defense = back_face.defense;
    obj.card_types = back_face.card_types.clone();
    obj.mana_cost = back_face.mana_cost.clone();
    obj.keywords = back_face.keywords.clone();
    obj.abilities = Arc::new(back_face.abilities.clone());
    obj.trigger_definitions = back_face.trigger_definitions.clone();
    obj.replacement_definitions = back_face.replacement_definitions.clone();
    obj.static_definitions = back_face.static_definitions.clone();
    obj.color = back_face.color.clone();
    obj.base_power = back_face.power;
    obj.base_toughness = back_face.toughness;
    obj.base_name = back_face.name.clone();
    obj.base_loyalty = back_face.loyalty;
    obj.base_defense = back_face.defense;
    obj.base_card_types = back_face.card_types;
    obj.base_mana_cost = back_face.mana_cost.clone();
    obj.base_keywords = back_face.keywords;
    obj.base_abilities = Arc::new(back_face.abilities);
    obj.base_trigger_definitions =
        Arc::new(back_face.trigger_definitions.iter_all().cloned().collect());
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
    loyalty: Option<u32>,
    defense: Option<u32>,
    card_types: &CardType,
) -> Vec<(CounterType, u32)> {
    let mut counters = intrinsic_face_counters(loyalty, defense);
    if let Some(lore) = intrinsic_saga_lore_counter(card_types) {
        counters.push(lore);
    }
    counters
}

pub fn intrinsic_etb_counters(obj: &GameObject) -> Vec<(CounterType, u32)> {
    let mut counters = intrinsic_face_counters(obj.loyalty, obj.defense);
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
    CopiableValues {
        name: obj.base_name.clone(),
        mana_cost: obj.base_mana_cost.clone(),
        color: obj.base_color.clone(),
        card_types: obj.base_card_types.clone(),
        power: obj.base_power,
        toughness: obj.base_toughness,
        loyalty: obj.base_loyalty,
        keywords: obj.base_keywords.clone(),
        // CopiableValues now shares `Arc<Vec<_>>` with the source object —
        // a copy-effect never mutates the ability set, so refcount sharing
        // is both correct and zero-allocation.
        abilities: Arc::clone(&obj.base_abilities),
        trigger_definitions: Arc::clone(&obj.base_trigger_definitions),
        replacement_definitions: copiable_replacement_definitions(obj),
        static_definitions: Arc::clone(&obj.base_static_definitions),
    }
}

/// CR 707.2 / CR 707.2b: copiable values are the object's printed/defining
/// characteristics, NOT resolved continuous effects installed by other
/// permanents (CR 611.2b "for as long as you control ~" locks). A
/// `ControllerControlsSource`-gated replacement is a runtime continuous effect
/// durably stored in `base_replacement_definitions` purely so it survives a
/// layer reset (evaluate_layers rebuilds live defs from base — layers.rs); it is
/// NOT a printed characteristic. Exclude it from copiable values so that a copy
/// of the locked host (becomes-a-copy or a copy-token) does not inherit the lock.
///
/// Zero-alloc fast path: every printed card has no gated def, so the common case
/// keeps sharing the source `Arc<Vec<_>>`. A filtered allocation is paid only
/// when a runtime lock is actually present on the object.
fn copiable_replacement_definitions(obj: &GameObject) -> Arc<Vec<ReplacementDefinition>> {
    if !obj
        .base_replacement_definitions
        .iter()
        .any(is_runtime_control_gated_replacement)
    {
        return Arc::clone(&obj.base_replacement_definitions);
    }
    Arc::new(
        obj.base_replacement_definitions
            .iter()
            .filter(|def| !is_runtime_control_gated_replacement(def))
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
        loyalty: result_face
            .loyalty
            .as_ref()
            .and_then(|value| value.parse::<u32>().ok()),
        keywords: result_face.keywords.clone(),
        abilities: Arc::new(result_face.abilities.clone()),
        trigger_definitions: Arc::new(result_face.triggers.clone()),
        replacement_definitions: Arc::new(result_face.replacements.clone()),
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

pub fn apply_copiable_values(obj: &mut GameObject, values: &CopiableValues) {
    obj.name = values.name.clone();
    obj.mana_cost = values.mana_cost.clone();
    obj.color = values.color.clone();
    obj.card_types = values.card_types.clone();
    obj.power = values.power;
    obj.toughness = values.toughness;
    obj.loyalty = values.loyalty;
    obj.keywords = values.keywords.clone();
    // All four ability sets are Arc-shared — refcount bumps, no deep copy.
    obj.abilities = Arc::clone(&values.abilities);
    obj.trigger_definitions = Arc::clone(&values.trigger_definitions).into();
    obj.replacement_definitions = Arc::clone(&values.replacement_definitions).into();
    obj.static_definitions = Arc::clone(&values.static_definitions).into();
}

pub fn snapshot_object_face(obj: &GameObject) -> BackFaceData {
    BackFaceData {
        name: obj.name.clone(),
        power: obj.power,
        toughness: obj.toughness,
        loyalty: obj.loyalty,
        defense: obj.defense,
        card_types: obj.card_types.clone(),
        mana_cost: obj.mana_cost.clone(),
        keywords: obj.keywords.clone(),
        // BackFaceData still stores Vec<T>; deep-clone when snapshotting.
        abilities: (*obj.abilities).clone(),
        trigger_definitions: obj.trigger_definitions.clone(),
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
// These walkers yield every conjure name reachable from a `CardFace`. They
// traverse every nested ability/effect/cost carrier. The core `walk_effect`
// match is wildcard-free so any future `Effect` variant that carries a nested
// `Box<Effect>` / `Box<AbilityDefinition>` must be handled here at compile time.
//
// TODO: consolidate with coverage traversal (`game/coverage.rs`). The coverage
// pass builds `ParsedItem` trees rather than yielding `Effect`s, so no reusable
// visitor exists today; extracting one is out of scope for this memory fix.
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

fn walk_ability_def(def: &AbilityDefinition, out: &mut Vec<String>) {
    walk_effect(&def.effect, out);
    if let Some(cost) = &def.cost {
        walk_cost(cost, out);
    }
    if let Some(sub) = &def.sub_ability {
        walk_ability_def(sub, out);
    }
    if let Some(else_ability) = &def.else_ability {
        walk_ability_def(else_ability, out);
    }
    for mode in &def.mode_abilities {
        walk_ability_def(mode, out);
    }
    // "unless [player] pays {cost}" — the cost may be an EffectCost that conjures.
    if let Some(unless_pay) = &def.unless_pay {
        walk_cost(&unless_pay.cost, out);
    }
}

fn walk_trigger(trigger: &TriggerDefinition, out: &mut Vec<String>) {
    if let Some(execute) = &trigger.execute {
        walk_ability_def(execute, out);
    }
    if let Some(unless_pay) = &trigger.unless_pay {
        walk_cost(&unless_pay.cost, out);
    }
}

fn walk_replacement(replacement: &ReplacementDefinition, out: &mut Vec<String>) {
    if let Some(execute) = &replacement.execute {
        walk_ability_def(execute, out);
    }
    // The mode carries the decline continuation (and, for MayCost, a cost),
    // either of which may conjure. Descend into both.
    match &replacement.mode {
        ReplacementMode::MayCost { cost, decline } => {
            walk_cost(cost, out);
            if let Some(decline) = decline {
                walk_ability_def(decline, out);
            }
        }
        ReplacementMode::Optional { decline } => {
            if let Some(decline) = decline {
                walk_ability_def(decline, out);
            }
        }
        ReplacementMode::Mandatory => {}
    }
    // `runtime_execute` holds a resolution-time continuation that is never
    // present on a printed/static `CardFace`; skipped intentionally.
}

fn walk_static(static_def: &StaticDefinition, out: &mut Vec<String>) {
    for modification in &static_def.modifications {
        walk_continuous_mod(modification, out);
    }
}

fn walk_continuous_mod(modification: &ContinuousModification, out: &mut Vec<String>) {
    match modification {
        ContinuousModification::GrantAbility { definition } => walk_ability_def(definition, out),
        ContinuousModification::GrantTrigger { trigger } => walk_trigger(trigger, out),
        ContinuousModification::GrantStaticAbility { definition } => walk_static(definition, out),
        ContinuousModification::CopyValues { values, .. } => walk_copiable_values(values, out),
        // Remaining modifications carry no nested ability/effect carriers.
        // GrantAllActivatedAbilitiesOf / GrantAllTriggeredAbilitiesOf only hold a
        // source `TargetFilter`; the granted abilities/triggers are pulled live
        // from the provider objects at layer collection time, not nested here.
        ContinuousModification::GrantAllActivatedAbilitiesOf { .. }
        | ContinuousModification::GrantAllTriggeredAbilitiesOf { .. }
        | ContinuousModification::SetName { .. }
        | ContinuousModification::AddPower { .. }
        | ContinuousModification::AddToughness { .. }
        | ContinuousModification::SetPower { .. }
        | ContinuousModification::SetToughness { .. }
        | ContinuousModification::AddKeyword { .. }
        | ContinuousModification::RemoveKeyword { .. }
        | ContinuousModification::RemoveAllAbilities
        | ContinuousModification::AddType { .. }
        | ContinuousModification::RemoveType { .. }
        | ContinuousModification::AddSubtype { .. }
        | ContinuousModification::RemoveSubtype { .. }
        | ContinuousModification::SetCardTypes { .. }
        | ContinuousModification::RemoveAllSubtypes { .. }
        | ContinuousModification::SetDynamicPower { .. }
        | ContinuousModification::SetDynamicToughness { .. }
        | ContinuousModification::SetPowerDynamic { .. }
        | ContinuousModification::SetToughnessDynamic { .. }
        | ContinuousModification::AddDynamicPower { .. }
        | ContinuousModification::AddDynamicToughness { .. }
        | ContinuousModification::AddDynamicKeyword { .. }
        | ContinuousModification::AddAllCreatureTypes
        | ContinuousModification::AddAllBasicLandTypes
        | ContinuousModification::AddAllLandTypes
        | ContinuousModification::AddChosenSubtype { .. }
        | ContinuousModification::AddChosenColor
        | ContinuousModification::RemoveChosenKeyword
        | ContinuousModification::AddChosenKeyword
        | ContinuousModification::SetColor { .. }
        | ContinuousModification::AddColor { .. }
        | ContinuousModification::AddStaticMode { .. }
        | ContinuousModification::SwitchPowerToughness
        | ContinuousModification::AssignDamageFromToughness
        | ContinuousModification::AssignDamageAsThoughUnblocked
        | ContinuousModification::AssignNoCombatDamage
        | ContinuousModification::ChangeController
        | ContinuousModification::SetBasicLandType { .. }
        | ContinuousModification::SetChosenBasicLandType
        | ContinuousModification::SetChosenName
        | ContinuousModification::RetainPrintedTriggerFromSource { .. }
        | ContinuousModification::RetainPrintedAbilityFromSource { .. }
        | ContinuousModification::AddSupertype { .. }
        | ContinuousModification::RemoveSupertype { .. }
        | ContinuousModification::AddCounterOnEnter { .. }
        | ContinuousModification::SetStartingLoyalty { .. }
        | ContinuousModification::RemoveManaCost => {}
    }
}

fn walk_copiable_values(values: &CopiableValues, out: &mut Vec<String>) {
    for ability in values.abilities.iter() {
        walk_ability_def(ability, out);
    }
    for trigger in values.trigger_definitions.iter() {
        walk_trigger(trigger, out);
    }
    for static_def in values.static_definitions.iter() {
        walk_static(static_def, out);
    }
    for replacement in values.replacement_definitions.iter() {
        walk_replacement(replacement, out);
    }
}

fn walk_cost(cost: &AbilityCost, out: &mut Vec<String>) {
    match cost {
        AbilityCost::EffectCost { effect } => walk_effect(effect, out),
        AbilityCost::Composite { costs } | AbilityCost::OneOf { costs } => {
            for sub in costs {
                walk_cost(sub, out);
            }
        }
        AbilityCost::PerCounter { base, .. } => walk_cost(base, out),
        // Remaining costs carry no nested effect/cost carriers.
        AbilityCost::Mana { .. }
        | AbilityCost::ManaDynamic { .. }
        | AbilityCost::Tap
        | AbilityCost::Untap
        | AbilityCost::Loyalty { .. }
        | AbilityCost::Sacrifice(_)
        | AbilityCost::PayLife { .. }
        | AbilityCost::Discard { .. }
        | AbilityCost::Exile { .. }
        | AbilityCost::ExileMaterials { .. }
        | AbilityCost::CollectEvidence { .. }
        | AbilityCost::ExileWithAggregate { .. }
        | AbilityCost::TapCreatures { .. }
        | AbilityCost::RemoveCounter { .. }
        | AbilityCost::PayEnergy { .. }
        | AbilityCost::PaySpeed { .. }
        | AbilityCost::ReturnToHand { .. }
        | AbilityCost::Unattach
        | AbilityCost::Mill { .. }
        | AbilityCost::Exert
        | AbilityCost::Blight { .. }
        | AbilityCost::Reveal { .. }
        | AbilityCost::Behold { .. }
        | AbilityCost::Waterbend { .. }
        | AbilityCost::NinjutsuFamily { .. }
        // CR 118.9: a borrowed keyword cost carries no nested effect/cost carrier.
        | AbilityCost::KeywordCostOfCastSpell { .. }
        | AbilityCost::Unimplemented { .. } => {}
    }
}

/// Yield every conjure name carried by `effect` and its nested ability/effect
/// carriers. The match is wildcard-free, so a new `Effect` variant forces a
/// decision here (compile error until handled). That guarantee is necessary but
/// not sufficient: a variant wrongly added to the leaf arm, or a new nested
/// *struct field* (which is field access, not a match arm), compiles silently.
/// `walker_covers_every_nested_carrier` is the complementary safety net for
/// those cases — extend it whenever a carrier is added.
fn walk_effect(effect: &Effect, out: &mut Vec<String>) {
    match effect {
        Effect::Intensify { .. } => {}
        Effect::ApplyPerpetual { .. } => {}
        // CR 614.11: A one-shot draw replacement nests its substitute Effect
        // (Words of Worship/Wilding). Walk it so any conjure name it carries is
        // surfaced (GainLife/Token carry none today, but it is a nested carrier).
        Effect::CreateDrawReplacement { replacement_effect } => {
            walk_effect(replacement_effect, out)
        }
        // CR 614.1a: A planeswalk replacement nests its substitute Effect (Fixed
        // Point in Time: chaos ensues). Walk it so any conjure name it carries is
        // surfaced (ChaosEnsues carries none today, but it is a nested carrier).
        Effect::CreatePlaneswalkReplacement { replacement_effect } => {
            walk_effect(replacement_effect, out)
        }
        // Heist exiles a card from an opponent's library at random; it does not
        // name a conjure card, so there is no static face to preload.
        Effect::Heist { .. } | Effect::HeistExile => {}
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
        // A spellbook draft conjures the chosen card, but the list lives on the
        // card face (`metadata.spellbook`), not in the effect — the registry
        // seed collects it directly from the face (see
        // `collect_conjure_names_from_face`), so nothing to gather here.
        Effect::DraftFromSpellbook { .. } => {}
        Effect::TurnFaceUp { .. } => {}
        Effect::TurnFaceDown { .. } => {}
        // Nested-ability carriers — descend.
        Effect::Vote {
            per_choice_effect,
            subject,
            ..
        } => {
            for sub in per_choice_effect {
                walk_ability_def(sub, out);
            }
            // CR 701.38b: object-pool votes (Council's Judgment, Prime
            // Minister's Cabinet Room) leave `per_choice_effect` empty and
            // carry the sole nested AbilityDefinition in `outcome_template`.
            // Walk it so any conjure name a future object-vote outcome names is
            // surfaced (the current exile-only class carries none).
            if let VoteSubject::Objects {
                outcome_template, ..
            } = subject
            {
                walk_ability_def(outcome_template, out);
            }
        }
        Effect::SeparateIntoPiles {
            chosen_pile_effect, ..
        } => walk_ability_def(chosen_pile_effect, out),
        Effect::RevealFromHand { on_decline, .. } => {
            if let Some(sub) = on_decline {
                walk_ability_def(sub, out);
            }
        }
        // Only the delayed `effect` is walked; the `condition`'s embedded
        // TriggerDefinition has `execute: None` by construction (it is a matcher,
        // not a payload), so it carries no conjure name.
        Effect::CreateDelayedTrigger { effect, .. } => walk_ability_def(effect, out),
        Effect::FlipCoin {
            win_effect,
            lose_effect,
            ..
        }
        | Effect::FlipCoins {
            win_effect,
            lose_effect,
            ..
        } => {
            if let Some(sub) = win_effect {
                walk_ability_def(sub, out);
            }
            if let Some(sub) = lose_effect {
                walk_ability_def(sub, out);
            }
        }
        Effect::FlipCoinUntilLose { win_effect } => walk_ability_def(win_effect, out),
        Effect::RollDie { results, .. } => {
            for branch in results {
                walk_ability_def(&branch.effect, out);
            }
        }
        Effect::ChooseOneOf { branches, .. } => {
            for branch in branches {
                walk_ability_def(branch, out);
            }
        }
        // GenericEffect applies static abilities at resolution; their
        // modifications can grant abilities/triggers that themselves conjure.
        // Descend into the granted definitions rather than treating it as a leaf.
        Effect::GenericEffect {
            static_abilities, ..
        } => {
            for static_def in static_abilities {
                walk_static(static_def, out);
            }
        }
        // Carries a nested ReplacementDefinition whose execute/decline/cost may conjure.
        Effect::AddTargetReplacement { replacement, .. } => walk_replacement(replacement, out),
        // Counter's `source_rider` may apply a static to the countered source
        // (LosesAbilities) that grants an ability that conjures. The Destroy
        // rider carries no static.
        Effect::Counter { source_rider, .. } => {
            if let Some(CounterSourceRider::LosesAbilities { static_def }) = source_rider {
                walk_static(static_def, out);
            }
        }
        // Tokens and emblems can host granted static/triggered abilities that conjure.
        Effect::Token {
            static_abilities, ..
        } => {
            for static_def in static_abilities {
                walk_static(static_def, out);
            }
        }
        Effect::CreateEmblem { statics, triggers } => {
            for static_def in statics {
                walk_static(static_def, out);
            }
            for trigger in triggers {
                walk_trigger(trigger, out);
            }
        }
        // Leaf effects with no nested ability/effect carrier.
        Effect::StartYourEngines { .. }
        | Effect::ChangeSpeed { .. }
        | Effect::DealDamage { .. }
        | Effect::ApplyPostReplacementDamage { .. }
        // CR 120.1: leaf effect — the source/recipient filters carry no nested
        // ability or effect to walk.
        | Effect::EachDealsDamageEqualToPower { .. }
        | Effect::EachSourceDealsDamage { .. }
        | Effect::Draw { .. }
        | Effect::Pump { .. }
        | Effect::PairWith { .. }
        | Effect::Destroy { .. }
        | Effect::Regenerate { .. }
        | Effect::RemoveAllDamage { .. }
        | Effect::CounterAll { .. }
        | Effect::GainLife { .. }
        | Effect::LoseLife { .. }
        | Effect::ExchangeLifeWithStat { .. }
        | Effect::ExchangeLifeTotals { .. }
        // CR 701.26a/b: all tap/untap scopes are leaf effects here.
        | Effect::SetTapState { .. }
        | Effect::RemoveCounter { .. }
        | Effect::Sacrifice { .. }
        | Effect::DiscardCard { .. }
        | Effect::Mill { .. }
        | Effect::Scry { .. }
        | Effect::PumpAll { .. }
        | Effect::DamageAll { .. }
        | Effect::DamageEachPlayer { .. }
        | Effect::DestroyAll { .. }
        | Effect::ChangeZone { .. }
        | Effect::ChangeZoneAll { .. }
        | Effect::Dig { .. }
        | Effect::GainControl { .. }
        | Effect::GainControlAll { .. }
        | Effect::ControlNextTurn { .. }
        | Effect::Attach { .. }
        | Effect::UnattachAll { .. }
        | Effect::Surveil { .. }
        | Effect::Fight { .. }
        | Effect::Bounce { .. }
        | Effect::BounceAll { .. }
        | Effect::Explore
        | Effect::ExploreAll { .. }
        | Effect::Investigate
        | Effect::Tribute { .. }
        | Effect::TimeTravel
        | Effect::BecomeMonarch
        | Effect::NoOp
        | Effect::Proliferate
        | Effect::ProliferateTarget { .. }
        | Effect::EndTheTurn
        | Effect::EndCombatPhase
        | Effect::Populate
        | Effect::Clash
        | Effect::SwitchPT { .. }
        | Effect::CopySpell { .. }
        | Effect::EpicCopy { .. }
        | Effect::CastCopyOfCard { .. }
        | Effect::CopyTokenOf { .. }
        // owner/type_filter are TargetFilters; no nested ability carrier and the
        // copy source comes from the format pool, so this is a leaf for conjure
        // collection.
        | Effect::CreateTokenCopyFromPool { .. }
        | Effect::Myriad
        | Effect::Encore
        | Effect::ExileHaunting { .. }
        | Effect::HideawayConceal { .. }
        | Effect::CopyTokenBlockingAttacker { .. }
        | Effect::BecomeCopy { .. }
        | Effect::GainActivatedAbilitiesOfTarget { .. }
        | Effect::ChooseCard { .. }
        | Effect::PutCounter { .. }
        | Effect::PutCounterAll { .. }
        | Effect::MultiplyCounter { .. }
        | Effect::DoublePT { .. }
        | Effect::DoublePTAll { .. }
        | Effect::MoveCounters { .. }
        | Effect::Animate { .. }
        | Effect::RegisterBending { .. }
        | Effect::Cleanup { .. }
        | Effect::Mana { .. }
        | Effect::Discard { .. }
        | Effect::Shuffle { .. }
        | Effect::Transform { .. }
        | Effect::SearchLibrary { .. }
        | Effect::SearchOutsideGame { .. }
        | Effect::RevealHand { .. }
        | Effect::Reveal { .. }
        | Effect::RevealTop { .. }
        | Effect::ExileTop { .. }
        | Effect::TargetOnly { .. }
        | Effect::Choose { .. }
        | Effect::ChooseDamageSource { .. }
        | Effect::Suspect { .. }
        | Effect::Unsuspect { .. }
        | Effect::Connive { .. }
        | Effect::PhaseOut { .. }
        | Effect::PhaseIn { .. }
        | Effect::ForceBlock { .. }
        | Effect::ForceAttack { .. }
        | Effect::SolveCase
        | Effect::BecomePrepared { .. }
        | Effect::BecomeUnprepared { .. }
        | Effect::BecomeSaddled { .. }
        | Effect::BecomeBlocked { .. }
        | Effect::SetClassLevel { .. }
        | Effect::AddRestriction { .. }
        | Effect::ReduceNextSpellCost { .. }
        | Effect::GrantNextSpellAbility { .. }
        | Effect::AddPendingETBCounters { .. }
        | Effect::PayCost { .. }
        | Effect::CastFromZone { .. }
        | Effect::FreeCastFromZones { .. }
        | Effect::ExileResolvingSpellInsteadOfGraveyard
        | Effect::PreventDamage { .. }
        | Effect::LoseTheGame { .. }
        | Effect::WinTheGame { .. }
        | Effect::RingTemptsYou
        | Effect::VentureIntoDungeon
        | Effect::VentureInto { .. }
        | Effect::TakeTheInitiative
        | Effect::Planeswalk
        | Effect::ChaosEnsues
        | Effect::OpenAttractions { .. }
        | Effect::RollToVisitAttractions
        | Effect::AssembleContraptions { .. }
        | Effect::AssembleContraptionsFromRollDifference
        | Effect::CrankContraptions { .. }
        | Effect::ReassembleContraption { .. }
        | Effect::AssembleContraptionOnSprocket { .. }
        | Effect::ReassembleContraptionOnSprocket { .. }
        | Effect::PutSticker { .. }
        | Effect::ApplySticker { .. }
        | Effect::ProcessRadCounters
        | Effect::GrantCastingPermission { .. }
        | Effect::ChooseFromZone { .. }
        | Effect::RememberCard { .. }
        | Effect::ForEachCategoryExile { .. }
        | Effect::ChooseObjectsIntoTrackedSet { .. }
        | Effect::ChooseAndSacrificeRest { .. }
        | Effect::Exploit { .. }
        | Effect::GainEnergy { .. }
        | Effect::GivePlayerCounter { .. }
        | Effect::LoseAllPlayerCounters { .. }
        | Effect::ExileFromTopUntil { .. }
        | Effect::RevealUntil { .. }
        | Effect::Discover { .. }
        | Effect::Cascade
        | Effect::Ripple { .. }
        | Effect::MiracleCast { .. }
        | Effect::MadnessCast { .. }
        | Effect::PutAtLibraryPosition { .. }
        | Effect::ChooseDrawnThisTurnPayOrTopdeck { .. }
        | Effect::PutOnTopOrBottom { .. }
        | Effect::GiftDelivery { .. }
        | Effect::Goad { .. }
        | Effect::GoadAll { .. }
        | Effect::Detain { .. }
        | Effect::SetRoomDoorLock { .. }
        | Effect::ExchangeControl { .. }
        | Effect::ChangeTargets { .. }
        | Effect::Manifest { .. }
        | Effect::ManifestDread
        | Effect::Cloak { .. }
        | Effect::ExtraTurn { .. }
        | Effect::GrantExtraLoyaltyActivations { .. }
        | Effect::SkipNextTurn { .. }
        | Effect::SkipNextStep { .. }
        | Effect::AdditionalPhase { .. }
        | Effect::Double { .. }
        | Effect::RuntimeHandled { .. }
        | Effect::Incubate { .. }
        | Effect::Amass { .. }
        | Effect::Monstrosity { .. }
        | Effect::Renown { .. }
        | Effect::Bolster { .. }
        | Effect::Adapt { .. }
        | Effect::Learn
        | Effect::Forage
        | Effect::Harness
        | Effect::CollectEvidence { .. }
        | Effect::Endure { .. }
        | Effect::BlightEffect { .. }
        | Effect::Seek { .. }
        | Effect::SetLifeTotal { .. }
        | Effect::SetDayNight { .. }
        | Effect::GiveControl { .. }
        | Effect::RemoveFromCombat { .. }
        | Effect::CreateDamageReplacement { .. }
        | Effect::CombineHost { .. }
        | Effect::ChooseAugmentAndCombineWithHost { .. }
        // CR 614.12 + CR 303.4: ReturnAsAura.grants carry typed
        // ContinuousModifications, never conjured card names.
        | Effect::ReturnAsAura { .. }
        | Effect::Specialize
        // CR 608.2d + CR 122.1: counter-kind choice / consume carry no conjure names.
        | Effect::ChooseCounterKind { .. }
        | Effect::PutChosenCounter { .. }
        | Effect::Unimplemented { .. } => {}
    }
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

/// CR 712 / CR 715 / CR 722: Attach the other printed face to `obj.back_face`
/// when absent. Required for transformed zone changes (Fable of the
/// Mirror-Breaker chapter III, Ajani flip triggers), adventurer casts, MDFC
/// casts, and prepare spell access. Without this, `deliver_replaced_zone_change`
/// silently skips transform when `back_face` is `None` and saga ETB lore-counter
/// replacements fire on the front face.
pub fn populate_back_face_if_dfc(obj: &mut GameObject, db: &CardDatabase, card_face: &CardFace) {
    if obj.back_face.is_some() {
        return;
    }

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
            obj.printed_ref
                .as_ref()
                .and_then(|printed_ref| db.get_other_face_by_printed_ref(printed_ref))
                .map(|face| (layout_kind, face))
        });
    let Some((layout_kind, face)) = second_face else {
        return;
    };

    let mut back = BackFaceData {
        name: String::new(),
        power: None,
        toughness: None,
        loyalty: None,
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
        layout_kind: None,
    };
    apply_card_face_to_back_face(&mut back, face);
    if layout_kind != LayoutKind::Single {
        back.layout_kind = Some(layout_kind);
    }
    obj.back_face = Some(back);
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
            intrinsic_etb_counters(&obj),
            vec![(CounterType::Plus1Plus1, 4)]
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
