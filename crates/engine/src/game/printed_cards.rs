use crate::database::CardDatabase;
use crate::types::ability::PtValue;
use crate::types::card::{CardFace, CardLayout, PrintedCardRef};
use crate::types::game_state::GameState;
use crate::types::mana::{ManaColor, ManaCost, ManaCostShard};

use super::derived::derive_display_state;
use super::game_object::{BackFaceData, CounterType, GameObject};
use super::layers::evaluate_layers;

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
    let power = parse_pt(&card_face.power);
    let toughness = parse_pt(&card_face.toughness);
    let loyalty = card_face
        .loyalty
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
    obj.loyalty = loyalty;
    // CR 306.5b: Sync loyalty counters so HasCounters condition works for animation statics.
    if let Some(loy) = loyalty {
        obj.counters.insert(CounterType::Loyalty, loy);
    }
    obj.card_types = card_face.card_type.clone();
    obj.mana_cost = card_face.mana_cost.clone();
    obj.keywords = keywords.clone();
    obj.abilities = card_face.abilities.clone();
    obj.trigger_definitions = card_face.triggers.clone();
    obj.replacement_definitions = card_face.replacements.clone();
    obj.static_definitions = card_face.static_abilities.clone();
    obj.color = color.clone();
    obj.base_power = power;
    obj.base_toughness = toughness;
    obj.base_card_types = card_face.card_type.clone();
    obj.base_keywords = keywords;
    obj.base_abilities = card_face.abilities.clone();
    obj.base_trigger_definitions = card_face.triggers.clone();
    obj.base_replacement_definitions = card_face.replacements.clone();
    obj.base_static_definitions = card_face.static_abilities.clone();
    obj.base_color = color;
    obj.printed_ref = printed_ref_from_face(card_face);
    obj.modal = card_face.modal.clone();
    obj.additional_cost = card_face.additional_cost.clone();
    obj.casting_restrictions = card_face.casting_restrictions.clone();
    obj.casting_options = card_face.casting_options.clone();

    // CR 716.3: Each Class enchantment enters the battlefield at level 1.
    // CR 400.7: A Class that re-enters is a new object at level 1.
    if card_face.card_type.subtypes.iter().any(|s| s == "Class") {
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
}

pub fn apply_card_face_to_back_face(back_face: &mut BackFaceData, card_face: &CardFace) {
    let power = parse_pt(&card_face.power);
    let toughness = parse_pt(&card_face.toughness);
    let loyalty = card_face
        .loyalty
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
    back_face.card_types = card_face.card_type.clone();
    back_face.mana_cost = card_face.mana_cost.clone();
    back_face.keywords = card_face.keywords.clone();
    back_face.abilities = card_face.abilities.clone();
    back_face.trigger_definitions = card_face.triggers.clone();
    back_face.replacement_definitions = card_face.replacements.clone();
    back_face.static_definitions = card_face.static_abilities.clone();
    back_face.color = color;
    back_face.printed_ref = printed_ref_from_face(card_face);
    back_face.modal = card_face.modal.clone();
    back_face.additional_cost = card_face.additional_cost.clone();
    back_face.casting_restrictions = card_face.casting_restrictions.clone();
    back_face.casting_options = card_face.casting_options.clone();
}

pub fn apply_back_face_to_object(obj: &mut GameObject, back_face: BackFaceData) {
    obj.name = back_face.name;
    obj.power = back_face.power;
    obj.toughness = back_face.toughness;
    obj.loyalty = back_face.loyalty;
    obj.card_types = back_face.card_types.clone();
    obj.mana_cost = back_face.mana_cost.clone();
    obj.keywords = back_face.keywords.clone();
    obj.abilities = back_face.abilities.clone();
    obj.trigger_definitions = back_face.trigger_definitions.clone();
    obj.replacement_definitions = back_face.replacement_definitions.clone();
    obj.static_definitions = back_face.static_definitions.clone();
    obj.color = back_face.color.clone();
    obj.base_power = back_face.power;
    obj.base_toughness = back_face.toughness;
    obj.base_card_types = back_face.card_types;
    obj.base_keywords = back_face.keywords;
    obj.base_abilities = back_face.abilities;
    obj.base_trigger_definitions = back_face.trigger_definitions;
    obj.base_replacement_definitions = back_face.replacement_definitions;
    obj.base_static_definitions = back_face.static_definitions;
    obj.base_color = back_face.color;
    obj.printed_ref = back_face.printed_ref;
    obj.modal = back_face.modal;
    obj.additional_cost = back_face.additional_cost;
    obj.casting_restrictions = back_face.casting_restrictions;
    obj.casting_options = back_face.casting_options;
}

pub fn snapshot_object_face(obj: &GameObject) -> BackFaceData {
    BackFaceData {
        name: obj.name.clone(),
        power: obj.power,
        toughness: obj.toughness,
        loyalty: obj.loyalty,
        card_types: obj.card_types.clone(),
        mana_cost: obj.mana_cost.clone(),
        keywords: obj.keywords.clone(),
        abilities: obj.abilities.clone(),
        trigger_definitions: obj.trigger_definitions.clone(),
        replacement_definitions: obj.replacement_definitions.clone(),
        static_definitions: obj.base_static_definitions.clone(),
        color: obj.color.clone(),
        printed_ref: obj.printed_ref.clone(),
        modal: obj.modal.clone(),
        additional_cost: obj.additional_cost.clone(),
        casting_restrictions: obj.casting_restrictions.clone(),
        casting_options: obj.casting_options.clone(),
    }
}

pub fn rehydrate_game_from_card_db(state: &mut GameState, db: &CardDatabase) {
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
            }

            // CR 715: Populate back_face for Adventure cards so the Adventure
            // half's characteristics are available at cast time.
            if obj.back_face.is_none() {
                if let Some(card_rules) = db.get_by_name(&card_face.name) {
                    if let CardLayout::Adventure(_, ref adventure_face) = card_rules.layout {
                        let mut back = BackFaceData {
                            name: String::new(),
                            power: None,
                            toughness: None,
                            loyalty: None,
                            card_types: Default::default(),
                            mana_cost: Default::default(),
                            keywords: Vec::new(),
                            abilities: Vec::new(),
                            trigger_definitions: Vec::new(),
                            replacement_definitions: Vec::new(),
                            static_definitions: Vec::new(),
                            color: Vec::new(),
                            printed_ref: None,
                            modal: None,
                            additional_cost: None,
                            casting_restrictions: Vec::new(),
                            casting_options: Vec::new(),
                        };
                        apply_card_face_to_back_face(&mut back, adventure_face);
                        obj.back_face = Some(back);
                    }
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

    if state.layers_dirty {
        evaluate_layers(state);
    }

    if changed_any {
        derive_display_state(state);
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
