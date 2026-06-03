//! Digital-only Specialize (Alchemy Horizons: Baldur's Gate): pay a generic cost,
//! discard a colored card or basic land, then permanently become the matching
//! specialized face. Not in the Comprehensive Rules — behavior follows Arena.

use std::collections::HashMap;

use crate::game::game_object::BackFaceData;
use crate::game::printed_cards::{
    apply_back_face_to_object, apply_card_face_to_back_face, snapshot_object_face,
};
use crate::types::card::CardFace;
use crate::types::events::GameEvent;
use crate::types::game_state::{GameState, LKISnapshot};
use crate::types::identifiers::ObjectId;
use crate::types::keywords::Keyword;
use crate::types::mana::ManaColor;
use crate::types::zones::Zone;

use super::engine::EngineError;
use super::layers;

/// Specialized faces keyed by the color pip added for that version.
pub type SpecializeFaceMap = HashMap<ManaColor, BackFaceData>;

/// Infer which specialization color a variant face represents from its mana cost / colors.
pub fn specialize_color_for_face(face: &CardFace) -> Option<ManaColor> {
    let colors = face.color_override.clone().unwrap_or_else(|| {
        crate::game::printed_cards::derive_colors_from_mana_cost(&face.mana_cost)
    });
    if colors.len() == 1 {
        return Some(colors[0]);
    }
    None
}

/// Build specialize variant storage from a `CardLayout::Specialize` rules object.
pub fn specialize_faces_from_variants(variants: &[CardFace]) -> SpecializeFaceMap {
    let mut map = SpecializeFaceMap::new();
    for face in variants {
        if let Some(color) = specialize_color_for_face(face) {
            let mut back = empty_back_face();
            apply_card_face_to_back_face(&mut back, face);
            map.insert(color, back);
        }
    }
    map
}

/// Colors the player may choose after discarding `lki`, intersected with `available`.
pub fn eligible_specialize_colors(
    lki: &LKISnapshot,
    available: &SpecializeFaceMap,
) -> Vec<ManaColor> {
    let mut from_discard = lki.colors.clone();
    if from_discard.is_empty() {
        for subtype in &lki.subtypes {
            if let Some(color) = basic_land_color(subtype) {
                from_discard.push(color);
            }
        }
    }
    let order = [
        ManaColor::White,
        ManaColor::Blue,
        ManaColor::Black,
        ManaColor::Red,
        ManaColor::Green,
    ];
    order
        .iter()
        .copied()
        .filter(|c| from_discard.contains(c) && available.contains_key(c))
        .collect()
}

fn empty_back_face() -> BackFaceData {
    BackFaceData {
        name: String::new(),
        power: None,
        toughness: None,
        loyalty: None,
        defense: None,
        card_types: Default::default(),
        mana_cost: Default::default(),
        keywords: vec![],
        abilities: vec![],
        trigger_definitions: Default::default(),
        replacement_definitions: Default::default(),
        static_definitions: Default::default(),
        color: vec![],
        printed_ref: None,
        modal: None,
        additional_cost: None,
        strive_cost: None,
        casting_restrictions: vec![],
        casting_options: vec![],
        layout_kind: None,
    }
}

fn basic_land_color(subtype: &str) -> Option<ManaColor> {
    match subtype.to_ascii_lowercase().as_str() {
        "plains" => Some(ManaColor::White),
        "island" => Some(ManaColor::Blue),
        "swamp" => Some(ManaColor::Black),
        "mountain" => Some(ManaColor::Red),
        "forest" => Some(ManaColor::Green),
        _ => None,
    }
}

/// Apply the chosen specialized face. One-way — cannot specialize again.
pub fn specialize_permanent(
    state: &mut GameState,
    object_id: ObjectId,
    color: ManaColor,
    events: &mut Vec<GameEvent>,
) -> Result<(), EngineError> {
    let faces = state
        .objects
        .get(&object_id)
        .and_then(|o| o.specialize_faces.clone())
        .ok_or_else(|| EngineError::InvalidAction("Object has no specialize faces".into()))?;

    if state
        .objects
        .get(&object_id)
        .is_some_and(|o| o.specialized_color.is_some())
    {
        return Err(EngineError::InvalidAction(
            "Permanent has already specialized".into(),
        ));
    }

    let face = faces.get(&color).cloned().ok_or_else(|| {
        EngineError::InvalidAction(format!("No specialized face for {:?}", color))
    })?;

    let obj = state
        .objects
        .get(&object_id)
        .ok_or_else(|| EngineError::InvalidAction("Object not found".into()))?;
    if obj.zone != Zone::Battlefield {
        return Err(EngineError::InvalidAction(
            "Only battlefield permanents can specialize".into(),
        ));
    }

    let obj = state.objects.get_mut(&object_id).unwrap();
    if obj.back_face.is_none() {
        obj.back_face = Some(snapshot_object_face(obj));
    }
    apply_back_face_to_object(obj, face);
    obj.specialized_color = Some(color);
    obj.specialize_faces = None;
    obj.keywords
        .retain(|k| !matches!(k, Keyword::Specialize(_)));
    obj.base_keywords
        .retain(|k| !matches!(k, Keyword::Specialize(_)));
    layers::mark_layers_full(state);

    events.push(GameEvent::Specialized { object_id, color });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::player::PlayerId;

    fn empty_lki() -> LKISnapshot {
        LKISnapshot {
            name: String::new(),
            power: None,
            toughness: None,
            base_power: None,
            base_toughness: None,
            mana_value: 0,
            controller: PlayerId(0),
            owner: PlayerId(0),
            card_types: vec![],
            subtypes: vec![],
            supertypes: vec![],
            keywords: vec![],
            colors: vec![],
            chosen_attributes: vec![],
            counters: Default::default(),
        }
    }

    #[test]
    fn eligible_colors_from_multicolor_discard() {
        let mut available = SpecializeFaceMap::new();
        available.insert(ManaColor::White, empty_back_face());
        available.insert(ManaColor::Blue, empty_back_face());
        available.insert(ManaColor::Red, empty_back_face());

        let mut lki = empty_lki();
        lki.colors = vec![ManaColor::White, ManaColor::Blue];
        assert_eq!(
            eligible_specialize_colors(&lki, &available),
            vec![ManaColor::White, ManaColor::Blue]
        );
    }

    #[test]
    fn eligible_colors_from_basic_land_subtype() {
        let mut available = SpecializeFaceMap::new();
        available.insert(ManaColor::Green, empty_back_face());
        let mut lki = empty_lki();
        lki.subtypes = vec!["Forest".to_string()];
        assert_eq!(
            eligible_specialize_colors(&lki, &available),
            vec![ManaColor::Green]
        );
    }
}
