use crate::types::ability::{Effect, EffectError, EffectKind, ResolvedAbility, TargetFilter};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;

/// CR 701.20: Reveal a specific object to all players.
///
/// Scope: `TargetFilter::SelfRef` (resolves to `ability.source_id`),
/// `ParentTarget` (chained reveal), and pre-resolved `TargetRef::Object` targets
/// are supported via the shared 3-tier `resolved_targets` dispatch. "Reveal
/// target <object>" (Hauntwoods Shrieker) resolves its `Typed` filter through
/// the targeting pipeline, which populates `ability.targets` before this
/// resolver runs, so the targeted object is read here as a pre-resolved object.
///
/// Emits a single `GameEvent::CardsRevealed` carrying all revealed card ids and names.
///
/// Per CR 701.20b, revealing a card does not cause it to change zones or otherwise
/// move — this resolver is read-only against game state.
///
/// Timing note (used by shuffle-back replacements per CR 614 + 701.20): when this
/// runs as a post-replacement effect after a redirected ZoneChange, the card has
/// already landed in its owner's library. The emitted event carries both
/// `card_ids` and `card_names`, so observers see which card caused the shuffle-back
/// regardless of the current zone.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let target = match &ability.effect {
        Effect::Reveal { target } => target.clone(),
        _ => TargetFilter::SelfRef,
    };

    // CR 608.2c + 603.10a: Delegate to the unified 3-tier dispatch so a
    // chained `Reveal { target: SelfRef }` sub-ability resolves to the source
    // object regardless of `ability.targets` (issue #323 class — without the
    // SelfRef short-circuit, chain target propagation in
    // `effects::mod.rs::resolve_ability_chain` would inherit the parent's
    // targets and reveal the wrong object).
    let effective_targets = crate::game::targeting::resolved_targets(ability, &target, state);
    let object_ids = crate::game::effects::effect_object_targets(&target, &effective_targets);

    if !object_ids.is_empty() {
        let card_names: Vec<String> = object_ids
            .iter()
            .filter_map(|id| state.objects.get(id).map(|o| o.name.clone()))
            .collect();

        // CR 700.1 + CR 701.20: a chained rider that inspects the revealed card
        // ("If it's a creature card, …" — Hauntwoods Shrieker) and an anaphoric
        // follow-up ("turn it face up") both read `last_revealed_ids`. Record
        // the revealed objects so the sub-ability's `RevealedHasCardType`
        // condition and `it` pronoun resolve to the just-revealed permanent.
        state.last_revealed_ids = object_ids.clone();

        events.push(GameEvent::CardsRevealed {
            player: ability.controller,
            card_ids: object_ids,
            card_names,
        });
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::Reveal,
        source_id: ability.source_id,
    });

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::ability::TargetRef;
    use crate::types::identifiers::CardId;
    use crate::types::player::PlayerId;
    use crate::types::zones::Zone;

    #[test]
    fn reveal_self_ref_emits_cards_revealed_with_source_object() {
        let mut state = GameState::new_two_player(42);
        let obj = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Nexus of Fate".to_string(),
            Zone::Library,
        );

        let ability = ResolvedAbility::new(
            Effect::Reveal {
                target: TargetFilter::SelfRef,
            },
            vec![],
            obj,
            PlayerId(0),
        );

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        let revealed = events.iter().find_map(|e| match e {
            GameEvent::CardsRevealed {
                player,
                card_ids,
                card_names,
            } => Some((*player, card_ids.clone(), card_names.clone())),
            _ => None,
        });

        let (player, card_ids, card_names) = revealed.expect("CardsRevealed emitted");
        assert_eq!(player, PlayerId(0));
        assert_eq!(card_ids, vec![obj]);
        assert_eq!(card_names, vec!["Nexus of Fate".to_string()]);
    }

    /// CR 608.2c (issue #323 class): a chained `Reveal { target: SelfRef }`
    /// sub-ability must reveal the source object even when chain target
    /// propagation in `effects::mod.rs::resolve_ability_chain` injected the
    /// parent's targets into `ability.targets`. Pre-fix the resolver checked
    /// `object_ids.is_empty() && SelfRef` locally; a propagated parent target
    /// would route through the chosen-targets branch and reveal the wrong
    /// object.
    #[test]
    fn reveal_selfref_overrides_propagated_parent_targets() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Source".to_string(),
            Zone::Library,
        );
        let other = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Other".to_string(),
            Zone::Library,
        );

        let ability = ResolvedAbility::new(
            Effect::Reveal {
                target: TargetFilter::SelfRef,
            },
            // Simulate chain target propagation from a parent that targeted
            // `other`. SelfRef must override and reveal the source instead.
            vec![TargetRef::Object(other)],
            source,
            PlayerId(0),
        );

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        let revealed = events.iter().find_map(|e| match e {
            GameEvent::CardsRevealed { card_ids, .. } => Some(card_ids.clone()),
            _ => None,
        });
        let card_ids = revealed.expect("CardsRevealed emitted");
        assert_eq!(
            card_ids,
            vec![source],
            "SelfRef reveal must reveal the source, not the propagated parent target"
        );
    }

    #[test]
    fn reveal_records_last_revealed_ids_for_chained_riders() {
        // CR 700.1 + CR 701.20 (Hauntwoods Shrieker class): a targeted reveal
        // must record the revealed object in `last_revealed_ids` so a chained
        // "If it's a creature card, turn it face up" rider and the anaphoric
        // "it" target both resolve to the just-revealed permanent. Reverting the
        // `last_revealed_ids` write leaves the vec empty and the assertion fails.
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Hauntwoods Shrieker".to_string(),
            Zone::Battlefield,
        );
        let revealed = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Face-Down".to_string(),
            Zone::Battlefield,
        );

        let ability = ResolvedAbility::new(
            Effect::Reveal {
                target: TargetFilter::ParentTarget,
            },
            vec![TargetRef::Object(revealed)],
            source,
            PlayerId(0),
        );

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(
            state.last_revealed_ids,
            vec![revealed],
            "the revealed object must be recorded for chained riders to read"
        );
    }

    #[test]
    fn reveal_does_not_mutate_game_state() {
        let mut state = GameState::new_two_player(42);
        let obj = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Progenitus".to_string(),
            Zone::Graveyard,
        );

        let before_revealed = state.revealed_cards.clone();
        let before_zones = state
            .objects
            .get(&obj)
            .map(|o| (o.zone, o.owner, o.controller));

        let ability = ResolvedAbility::new(
            Effect::Reveal {
                target: TargetFilter::SelfRef,
            },
            vec![],
            obj,
            PlayerId(0),
        );

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        // CR 701.20b: revealing does not change zones or mutate state.
        assert_eq!(state.revealed_cards, before_revealed);
        assert_eq!(
            state
                .objects
                .get(&obj)
                .map(|o| (o.zone, o.owner, o.controller)),
            before_zones
        );
    }
}
