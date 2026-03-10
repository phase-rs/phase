//! Canonical Forge-style object filter matching.
//!
//! Replicates the filter syntax from Java Forge's `Card.isValid()` +
//! `CardProperty.cardHasProperty()`:
//!
//! ```text
//! "Creature.Other+YouCtrl"
//!  ^^^^^^^  ^^^^^ ^^^^^^
//!  type     prop1  prop2
//! ```
//!
//! - `.` separates the **type restriction** (left) from **properties** (right).
//! - `+` chains multiple properties with AND logic.
//! - Both sides are individually optional (e.g. `"YouCtrl"` alone is valid).

use crate::types::card_type::CoreType;
use crate::types::game_state::GameState;
use crate::types::identifiers::ObjectId;
use crate::types::player::PlayerId;

/// Check if an object matches a Forge-style filter string.
///
/// `source_id` is the object that owns the ability (used for Self/Other resolution).
/// The source's controller is looked up from `state` (for YouCtrl/OppCtrl).
///
/// Returns `true` if every component of the filter is satisfied.
pub fn object_matches_filter(
    state: &GameState,
    object_id: ObjectId,
    filter: &str,
    source_id: ObjectId,
) -> bool {
    let source_controller = state.objects.get(&source_id).map(|o| o.controller);
    filter_inner(state, object_id, filter, source_id, source_controller)
}

/// Like [`object_matches_filter`], but with an explicit controller.
///
/// Use this when resolving effects from the stack where the source object
/// may no longer exist (e.g. sacrificed as a cost).
pub fn object_matches_filter_controlled(
    state: &GameState,
    object_id: ObjectId,
    filter: &str,
    source_id: ObjectId,
    controller: PlayerId,
) -> bool {
    filter_inner(state, object_id, filter, source_id, Some(controller))
}

fn filter_inner(
    state: &GameState,
    object_id: ObjectId,
    filter: &str,
    source_id: ObjectId,
    source_controller: Option<PlayerId>,
) -> bool {
    if filter.is_empty() {
        return true;
    }

    let obj = match state.objects.get(&object_id) {
        Some(o) => o,
        None => return false,
    };

    // Split on `.` first: left side is type, right side is `+`-separated properties.
    // If there's no `.`, the whole string could be either a type or a property.
    let (type_part, props_part) = match filter.split_once('.') {
        Some((t, p)) => (Some(t), Some(p)),
        None => {
            // Single token: try as type first, fall back to property
            if is_type_keyword(filter) {
                (Some(filter), None)
            } else {
                (None, Some(filter))
            }
        }
    };

    // --- Type restriction ---
    if let Some(type_str) = type_part {
        if !matches_type(type_str, obj) {
            return false;
        }
    }

    // --- Property restrictions (+ or . separated, per Forge convention) ---
    if let Some(props) = props_part {
        for prop in props.split(['+', '.']) {
            if !matches_property(prop, obj, object_id, source_id, source_controller) {
                return false;
            }
        }
    }

    true
}

/// Check if a filter token is a known type keyword.
fn is_type_keyword(token: &str) -> bool {
    matches!(
        token,
        "Creature"
            | "Land"
            | "Artifact"
            | "Enchantment"
            | "Instant"
            | "Sorcery"
            | "Planeswalker"
            | "Card"
            | "Permanent"
            | "Any"
    )
}

/// Check if an object matches a type restriction.
fn matches_type(type_str: &str, obj: &crate::game::game_object::GameObject) -> bool {
    match type_str {
        "Creature" => obj.card_types.core_types.contains(&CoreType::Creature),
        "Land" => obj.card_types.core_types.contains(&CoreType::Land),
        "Artifact" => obj.card_types.core_types.contains(&CoreType::Artifact),
        "Enchantment" => obj.card_types.core_types.contains(&CoreType::Enchantment),
        "Instant" => obj.card_types.core_types.contains(&CoreType::Instant),
        "Sorcery" => obj.card_types.core_types.contains(&CoreType::Sorcery),
        "Planeswalker" => obj.card_types.core_types.contains(&CoreType::Planeswalker),
        "Permanent" => {
            obj.card_types.core_types.contains(&CoreType::Creature)
                || obj.card_types.core_types.contains(&CoreType::Artifact)
                || obj.card_types.core_types.contains(&CoreType::Enchantment)
                || obj.card_types.core_types.contains(&CoreType::Land)
                || obj.card_types.core_types.contains(&CoreType::Planeswalker)
        }
        "Card" | "Any" => true,
        _ => true, // Unknown type, permissive fallback
    }
}

/// Check if an object satisfies a single property restriction.
fn matches_property(
    prop: &str,
    obj: &crate::game::game_object::GameObject,
    object_id: ObjectId,
    source_id: ObjectId,
    source_controller: Option<PlayerId>,
) -> bool {
    match prop {
        // Identity
        "Self" => object_id == source_id,
        "Other" => object_id != source_id,

        // Controller
        "YouCtrl" => source_controller == Some(obj.controller),
        "YouDontCtrl" => source_controller != Some(obj.controller),
        "OppCtrl" => source_controller.is_some() && source_controller != Some(obj.controller),

        // Ownership
        "YouOwn" => source_controller == Some(obj.owner),
        "YouDontOwn" => source_controller != Some(obj.owner),
        "OppOwn" => source_controller.is_some() && source_controller != Some(obj.owner),

        // State
        "tapped" => obj.tapped,
        "untapped" => !obj.tapped,

        // Permissive fallback for unrecognized properties
        _ => true,
    }
}

/// Check if a player matches a Forge-style player filter (e.g. "You", "Opp").
///
/// Used by static abilities that target players rather than objects.
pub fn player_matches_filter(
    player_id: PlayerId,
    filter: &str,
    source_controller: Option<PlayerId>,
) -> bool {
    for part in filter.split('+') {
        match part {
            "You" => {
                if source_controller != Some(player_id) {
                    return false;
                }
            }
            "Opp" => {
                if source_controller == Some(player_id) {
                    return false;
                }
            }
            _ => {}
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::card_type::CoreType;
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::player::PlayerId;

    fn setup() -> GameState {
        GameState::new_two_player(42)
    }

    fn add_creature(state: &mut GameState, owner: PlayerId, name: &str) -> ObjectId {
        let id = create_object(
            state,
            CardId(state.next_object_id),
            owner,
            name.to_string(),
            crate::types::zones::Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&id)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);
        id
    }

    #[test]
    fn empty_filter_matches_everything() {
        let mut state = setup();
        let id = add_creature(&mut state, PlayerId(0), "Bear");
        assert!(object_matches_filter(&state, id, "", id));
    }

    #[test]
    fn type_filter_matches_correct_type() {
        let mut state = setup();
        let id = add_creature(&mut state, PlayerId(0), "Bear");
        assert!(object_matches_filter(&state, id, "Creature", id));
        assert!(!object_matches_filter(&state, id, "Land", id));
        assert!(object_matches_filter(&state, id, "Card", id));
        assert!(object_matches_filter(&state, id, "Any", id));
    }

    #[test]
    fn self_filter() {
        let mut state = setup();
        let a = add_creature(&mut state, PlayerId(0), "A");
        let b = add_creature(&mut state, PlayerId(0), "B");
        assert!(object_matches_filter(&state, a, "Card.Self", a));
        assert!(!object_matches_filter(&state, b, "Card.Self", a));
    }

    #[test]
    fn other_filter_excludes_source() {
        let mut state = setup();
        let marshal = add_creature(&mut state, PlayerId(0), "Benalish Marshal");
        let bear = add_creature(&mut state, PlayerId(0), "Bear");

        let filter = "Creature.Other+YouCtrl";

        // Marshal should NOT match its own "Other" filter
        assert!(!object_matches_filter(&state, marshal, filter, marshal));
        // Bear should match
        assert!(object_matches_filter(&state, bear, filter, marshal));
    }

    #[test]
    fn you_ctrl_filter() {
        let mut state = setup();
        let mine = add_creature(&mut state, PlayerId(0), "Mine");
        let theirs = add_creature(&mut state, PlayerId(1), "Theirs");

        assert!(object_matches_filter(
            &state,
            mine,
            "Creature.YouCtrl",
            mine
        ));
        assert!(!object_matches_filter(
            &state,
            theirs,
            "Creature.YouCtrl",
            mine
        ));
    }

    #[test]
    fn opp_ctrl_filter() {
        let mut state = setup();
        let mine = add_creature(&mut state, PlayerId(0), "Mine");
        let theirs = add_creature(&mut state, PlayerId(1), "Theirs");

        assert!(!object_matches_filter(
            &state,
            mine,
            "Creature.OppCtrl",
            mine
        ));
        assert!(object_matches_filter(
            &state,
            theirs,
            "Creature.OppCtrl",
            mine
        ));
    }

    #[test]
    fn combined_type_and_properties() {
        let mut state = setup();
        let source = add_creature(&mut state, PlayerId(0), "Lord");
        let ally = add_creature(&mut state, PlayerId(0), "Ally");
        let enemy = add_creature(&mut state, PlayerId(1), "Enemy");

        // "Creature.Other+YouCtrl" — other creatures I control
        assert!(!object_matches_filter(
            &state,
            source,
            "Creature.Other+YouCtrl",
            source
        ));
        assert!(object_matches_filter(
            &state,
            ally,
            "Creature.Other+YouCtrl",
            source
        ));
        assert!(!object_matches_filter(
            &state,
            enemy,
            "Creature.Other+YouCtrl",
            source
        ));
    }

    #[test]
    fn permanent_matches_multiple_types() {
        let mut state = setup();
        let id = add_creature(&mut state, PlayerId(0), "Bear");
        assert!(object_matches_filter(&state, id, "Permanent", id));
    }

    #[test]
    fn player_filter_you() {
        assert!(player_matches_filter(PlayerId(0), "You", Some(PlayerId(0))));
        assert!(!player_matches_filter(
            PlayerId(1),
            "You",
            Some(PlayerId(0))
        ));
    }

    #[test]
    fn player_filter_opp() {
        assert!(!player_matches_filter(
            PlayerId(0),
            "Opp",
            Some(PlayerId(0))
        ));
        assert!(player_matches_filter(PlayerId(1), "Opp", Some(PlayerId(0))));
    }
}
