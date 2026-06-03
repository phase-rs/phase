use crate::database::CardDatabase;
use crate::types::ability::{
    AbilityCost, AbilityDefinition, ContinuousModification, CopiableValues, CounterSourceRider,
    Effect, PtValue, ReplacementDefinition, ReplacementMode, StaticDefinition, TriggerDefinition,
};
use crate::types::card::{CardFace, CardLayout, LayoutKind, PrintedCardRef};
use crate::types::card_type::CoreType;
use crate::types::counter::CounterType;
use crate::types::game_state::GameState;
use crate::types::identifiers::ObjectId;
use crate::types::keywords::Keyword;
use crate::types::mana::{ManaColor, ManaCost, ManaCostShard};
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
    let color = card_face
        .color_override
        .clone()
        .unwrap_or_else(|| derive_colors_from_mana_cost(&card_face.mana_cost));

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
    obj.abilities = Arc::new(card_face.abilities.clone());
    obj.trigger_definitions = card_face.triggers.clone().into();
    obj.replacement_definitions = card_face.replacements.clone().into();
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
    obj.base_abilities = Arc::new(card_face.abilities.clone());
    obj.base_trigger_definitions = Arc::new(card_face.triggers.clone());
    obj.base_replacement_definitions = Arc::new(card_face.replacements.clone());
    obj.base_static_definitions = Arc::new(card_face.static_abilities.clone());
    obj.base_color = color;
    obj.base_characteristics_initialized = true;
    obj.printed_ref = printed_ref_from_face(card_face);
    // Display-identity baseline: the layer reset restores `printed_ref` from
    // this each pass (see `game_object::base_printed_ref`).
    obj.base_printed_ref = obj.printed_ref.clone();
    obj.source_related_token_ids = card_face.metadata.related_token_ids.clone();
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
    let color = card_face
        .color_override
        .clone()
        .unwrap_or_else(|| derive_colors_from_mana_cost(&card_face.mana_cost));

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
pub fn intrinsic_etb_counters(obj: &GameObject) -> Vec<(CounterType, u32)> {
    let mut counters = Vec::new();
    if let Some(loy) = obj.loyalty {
        if loy > 0 {
            counters.push((CounterType::Loyalty, loy));
        }
    }
    if let Some(def) = obj.defense {
        if def > 0 {
            counters.push((CounterType::Defense, def));
        }
    }
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
        replacement_definitions: Arc::clone(&obj.base_replacement_definitions),
        static_definitions: Arc::clone(&obj.base_static_definitions),
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
        ContinuousModification::SetName { .. }
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
        | ContinuousModification::SetColor { .. }
        | ContinuousModification::AddColor { .. }
        | ContinuousModification::AddStaticMode { .. }
        | ContinuousModification::SwitchPowerToughness
        | ContinuousModification::AssignDamageFromToughness
        | ContinuousModification::AssignDamageAsThoughUnblocked
        | ContinuousModification::AssignNoCombatDamage
        | ContinuousModification::ChangeController
        | ContinuousModification::SetBasicLandType { .. }
        | ContinuousModification::RetainPrintedTriggerFromSource { .. }
        | ContinuousModification::AddSupertype { .. }
        | ContinuousModification::RemoveSupertype { .. }
        | ContinuousModification::AddCounterOnEnter { .. } => {}
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
        | AbilityCost::Sacrifice { .. }
        | AbilityCost::PayLife { .. }
        | AbilityCost::Discard { .. }
        | AbilityCost::Exile { .. }
        | AbilityCost::CollectEvidence { .. }
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
        Effect::Conjure { cards, .. } => {
            for conjure_card in cards {
                out.push(conjure_card.name.clone());
            }
        }
        // Nested-ability carriers — descend.
        Effect::Vote {
            per_choice_effect, ..
        } => {
            for sub in per_choice_effect {
                walk_ability_def(sub, out);
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
        | Effect::Draw { .. }
        | Effect::Pump { .. }
        | Effect::PairWith { .. }
        | Effect::Destroy { .. }
        | Effect::Regenerate { .. }
        | Effect::CounterAll { .. }
        | Effect::GainLife { .. }
        | Effect::LoseLife { .. }
        | Effect::ExchangeLifeWithStat { .. }
        | Effect::Tap { .. }
        | Effect::Untap { .. }
        | Effect::TapAll { .. }
        | Effect::UntapAll { .. }
        | Effect::AddCounter { .. }
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
        | Effect::Proliferate
        | Effect::EndTheTurn
        | Effect::EndCombatPhase
        | Effect::Populate
        | Effect::Clash
        | Effect::SwitchPT { .. }
        | Effect::CopySpell { .. }
        | Effect::CastCopyOfCard { .. }
        | Effect::CopyTokenOf { .. }
        | Effect::Myriad
        | Effect::CopyTokenBlockingAttacker { .. }
        | Effect::BecomeCopy { .. }
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
        | Effect::Connive { .. }
        | Effect::PhaseOut { .. }
        | Effect::PhaseIn { .. }
        | Effect::ForceBlock { .. }
        | Effect::ForceAttack { .. }
        | Effect::SolveCase
        | Effect::BecomePrepared { .. }
        | Effect::BecomeUnprepared { .. }
        | Effect::SetClassLevel { .. }
        | Effect::AddRestriction { .. }
        | Effect::ReduceNextSpellCost { .. }
        | Effect::GrantNextSpellAbility { .. }
        | Effect::AddPendingETBCounters { .. }
        | Effect::PayCost { .. }
        | Effect::CastFromZone { .. }
        | Effect::PreventDamage { .. }
        | Effect::LoseTheGame
        | Effect::WinTheGame
        | Effect::RingTemptsYou
        | Effect::VentureIntoDungeon
        | Effect::VentureInto { .. }
        | Effect::TakeTheInitiative
        | Effect::OpenAttractions { .. }
        | Effect::RollToVisitAttractions
        | Effect::ProcessRadCounters
        | Effect::GrantCastingPermission { .. }
        | Effect::ChooseFromZone { .. }
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
        | Effect::MiracleCast { .. }
        | Effect::MadnessCast { .. }
        | Effect::PutAtLibraryPosition { .. }
        | Effect::ChooseDrawnThisTurnPayOrTopdeck { .. }
        | Effect::PutOnTopOrBottom { .. }
        | Effect::GiftDelivery { .. }
        | Effect::Goad { .. }
        | Effect::GoadAll { .. }
        | Effect::Detain { .. }
        | Effect::ExchangeControl { .. }
        | Effect::ChangeTargets { .. }
        | Effect::Manifest { .. }
        | Effect::ManifestDread
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
        | Effect::CollectEvidence { .. }
        | Effect::Endure { .. }
        | Effect::BlightEffect { .. }
        | Effect::Seek { .. }
        | Effect::SetLifeTotal { .. }
        | Effect::SetDayNight { .. }
        | Effect::GiveControl { .. }
        | Effect::RemoveFromCombat { .. }
        | Effect::CreateDamageReplacement { .. }
        // CR 614.12 + CR 303.4: ReturnAsAura.grants carry typed
        // ContinuousModifications, never conjured card names.
        | Effect::ReturnAsAura { .. }
        | Effect::Specialize
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
fn build_conjure_registry(
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

pub fn rehydrate_game_from_card_db(state: &mut GameState, db: &CardDatabase) {
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
            } else {
                apply_card_face_to_object(obj, &card_face);
            }

            if let Some(back_face) = obj.back_face.as_mut() {
                if let Some(back_ref) = back_face.printed_ref.clone() {
                    if let Some(back_card_face) = db.get_face_by_printed_ref(&back_ref) {
                        apply_card_face_to_back_face(back_face, back_card_face);
                    } else if is_face_down_battlefield {
                        apply_card_face_to_back_face(back_face, &card_face);
                    }
                } else if is_face_down_battlefield {
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
                apply_face_down_creature_characteristics(obj);
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

            // Populate back_face for dual-faced layouts so the other face's
            // characteristics are available for transform, adventure cast, and
            // preview display (Ctrl-hover).
            if obj.back_face.is_none() {
                let second_face = db
                    .get_by_name(&card_face.name)
                    .and_then(|card_rules| match &card_rules.layout {
                        // CR 715: Adventure half available at cast time
                        CardLayout::Adventure(_, back) => Some((LayoutKind::Adventure, back)),
                        // CR 712: Transform / Modal DFC / Meld / Omen back face
                        CardLayout::Transform(_, back) => Some((LayoutKind::Transform, back)),
                        CardLayout::Modal(_, back) => Some((LayoutKind::Modal, back)),
                        CardLayout::Meld(_, back) => Some((LayoutKind::Meld, back)),
                        CardLayout::Omen(_, back) => Some((LayoutKind::Omen, back)),
                        // CR 702.xxx: Prepare (Strixhaven) — face `b` is the prepare spell
                        // (Sorcery/Instant), held in back_face for runtime copy-cast access.
                        CardLayout::Prepare(_, back) => Some((LayoutKind::Prepare, back)),
                        _ => None,
                    })
                    .or_else(|| {
                        // Fallback for export-loaded databases where `cards` is empty.
                        // Use the layout_index (populated from the `layout` field in
                        // card-data.json) to determine the correct LayoutKind.
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
                if let Some((layout_kind, face)) = second_face {
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
            }
        }

        changed_any = true;
        if zone == crate::types::zones::Zone::Battlefield {
            changed_battlefield = true;
        }
    }

    if changed_battlefield {
        crate::game::layers::mark_layers_full(state);
    }

    if changed_any || state.layers_dirty.is_dirty() {
        bump_state_revision(state);
        mark_public_state_all_dirty(state);
        finalize_public_state(state);
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
        ManaCost::NoCost | ManaCost::SelfManaCost => vec![],
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
                    name: target_name.to_string(),
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
                    name: "cost".to_string(),
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
                        name: "unless_pay_ability".to_string(),
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
        };
        walk_effect(&flip, &mut names);

        let until_lose = Effect::FlipCoinUntilLose {
            win_effect: Box::new(conjure_ability("until_lose", Zone::Hand)),
        };
        walk_effect(&until_lose, &mut names);

        let roll = Effect::RollDie {
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
                        name: "unless_pay_trigger".to_string(),
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
                        name: "repl_maycost_cost".to_string(),
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
        ];
        for name in expected {
            assert!(
                names.iter().any(|n| n == name),
                "walker missed conjure name '{name}' in a nested carrier"
            );
        }
    }
}
