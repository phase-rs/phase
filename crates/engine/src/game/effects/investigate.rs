use crate::types::ability::{EffectError, PtValue, ResolvedAbility};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;

/// CR 701.16a: Investigate — create a Clue artifact token.
///
/// A Clue token is a colorless Artifact — Clue with "{2}, Sacrifice this
/// artifact: Draw a card." The token creation reuses the existing token
/// resolver by constructing a synthetic Token effect.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    // CR 111.10f: A Clue token is a colorless Clue artifact token with
    // "{2}, Sacrifice this artifact: Draw a card."
    // Build a synthetic Token effect and resolve through the standard token pipeline.
    let clue_ability = ResolvedAbility::new(
        crate::types::ability::Effect::Token {
            name: "Clue".to_string(),
            power: PtValue::Fixed(0),
            toughness: PtValue::Fixed(0),
            types: vec!["Artifact".to_string(), "Clue".to_string()],
            colors: vec![],
            keywords: vec![],
            tapped: false,
            count: crate::types::ability::QuantityExpr::Fixed { value: 1 },
            owner: crate::types::ability::TargetFilter::Controller,
            attach_to: None,
            enters_attacking: false,
            supertypes: vec![],
            static_abilities: vec![],
            enter_with_counters: vec![],
        },
        ability.targets.clone(),
        ability.source_id,
        ability.controller,
    );
    super::token::resolve(state, &clue_ability, events)
}
