use crate::database::CardDatabase;
use crate::types::ability::{CopiableValues, PtValue};
use crate::types::card::{CardFace, CardLayout, LayoutKind, PrintedCardRef};
use crate::types::counter::CounterType;
use crate::types::game_state::GameState;
use crate::types::keywords::Keyword;
use crate::types::mana::{ManaColor, ManaCost, ManaCostShard};
use std::sync::Arc;

use super::game_object::{BackFaceData, GameObject};
use super::public_state::{
    bump_state_revision, finalize_public_state, mark_public_state_all_dirty,
};

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
/// Returned as `(counter_type_string, count)` entries suitable for pushing
/// onto `ProposedEvent::ZoneChange::enter_with_counters`. The replacement
/// pipeline then dispatches each entry through `add_counter_with_replacement`
/// so Doubling Season / Hardened Scales / Vorinclex apply per CR 614.1a.
///
/// Returns an empty vec for non-planeswalker, non-battle permanents or when
/// the face carries no printed loyalty/defense number.
pub fn intrinsic_etb_counters(obj: &GameObject) -> Vec<(String, u32)> {
    let mut counters = Vec::new();
    if let Some(loy) = obj.loyalty {
        if loy > 0 {
            counters.push((CounterType::Loyalty.as_str().to_string(), loy));
        }
    }
    if let Some(def) = obj.defense {
        if def > 0 {
            counters.push((CounterType::Defense.as_str().to_string(), def));
        }
    }
    // CR 702.156a + CR 107.3m: Ravenous is an intrinsic ETB replacement
    // effect. The paid X is stamped on the object when the spell leaves the
    // stack, before the ZoneChange replacement pipeline applies counters.
    if obj.has_keyword(&Keyword::Ravenous) {
        if let Some(x_paid) = obj.cost_x_paid {
            if x_paid > 0 {
                counters.push((CounterType::Plus1Plus1.as_str().to_string(), x_paid));
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

pub fn rehydrate_game_from_card_db(state: &mut GameState, db: &CardDatabase) {
    // Populate card face registry for runtime card lookup (used by Conjure effect handler).
    if state.card_face_registry.is_empty() {
        state.card_face_registry = std::sync::Arc::new(
            db.face_iter()
                .map(|(key, face)| (key.to_string(), face.clone()))
                .collect(),
        );
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
            apply_card_face_to_object(obj, &card_face);

            if let Some(back_face) = obj.back_face.as_mut() {
                if let Some(back_ref) = back_face.printed_ref.clone() {
                    if let Some(back_card_face) = db.get_face_by_printed_ref(&back_ref) {
                        apply_card_face_to_back_face(back_face, back_card_face);
                    }
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
        state.layers_dirty = true;
    }

    if changed_any || state.layers_dirty {
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
        ManaCostShard::Colorless | ManaCostShard::Snow | ManaCostShard::X => vec![],
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
    use crate::game::game_object::GameObject;
    use crate::types::ability::{
        AbilityDefinition, AdditionalCost, CastingRestriction, ModalChoice, ReplacementDefinition,
        SolveCondition, SpellCastingOption, StaticDefinition, TriggerDefinition,
    };
    use crate::types::card::CardFace;
    use crate::types::card_type::{CardType, CoreType};
    use crate::types::game_state::GameState;
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::keywords::Keyword;
    use crate::types::mana::{ManaColor, ManaCost, ManaCostShard};
    use crate::types::player::PlayerId;
    use crate::types::zones::Zone;

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
            metadata: Default::default(),
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
            vec![(CounterType::Plus1Plus1.as_str().to_string(), 4)]
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
}
