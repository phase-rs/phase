use crate::types::ability::{EffectError, PtValue, ResolvedAbility};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;

/// CR 702.136: Investigate — create a Clue artifact token.
///
/// A Clue token is a colorless Artifact — Clue with "{2}, Sacrifice this
/// artifact: Draw a card." The token creation reuses the existing token
/// resolver by constructing a synthetic Token effect.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    // Build a synthetic Token effect for a Clue token and resolve it
    // through the standard token pipeline.
    let clue_ability = ResolvedAbility::new(
        crate::types::ability::Effect::Token {
            name: "Clue".to_string(),
            power: PtValue::Fixed(0),
            toughness: PtValue::Fixed(0),
            types: vec!["Artifact".to_string(), "Clue".to_string()],
            colors: vec![],
            keywords: vec![],
            tapped: false,
            count: crate::types::ability::CountValue::Fixed(1),
        },
        ability.targets.clone(),
        ability.source_id,
        ability.controller,
    );
    super::token::resolve(state, &clue_ability, events)
}
