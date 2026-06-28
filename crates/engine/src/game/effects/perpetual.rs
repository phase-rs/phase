//! Digital-only Alchemy (no CR entry): `Effect::ApplyPerpetual` — apply a
//! "perpetually" modification that permanently edits a card and follows it
//! across every zone.
//!
//! Like [`super::intensify`], the change is recorded on the object
//! (`GameObject::perpetual_mods`) and edits a persistent characteristic, so it
//! survives zone changes and serialization. Increment 1 covers base
//! power/toughness ("perpetually become(s)/has base power and toughness P/T",
//! e.g. High Fae Prankster, Three Tree Battalion, Blood Age Muster).
//!
//! Target resolution routes through `resolved_targets` so ParentTarget anaphora
//! (Stationed/VehicleCrewed events, chain propagation) bind the correct object;
//! `Any` falls back to the source when no referent is available.

use crate::types::ability::{Effect, EffectError, EffectKind, ResolvedAbility};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;

/// Target resolution: uses the effect's `target` filter through the shared
/// `resolved_targets` machinery (ParentTarget event anaphora, chain propagation,
/// or source fallback for `Any`).
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let Effect::ApplyPerpetual {
        target,
        modification,
    } = &ability.effect
    else {
        return Err(EffectError::MissingParam("ApplyPerpetual".to_string()));
    };
    let modification = modification.clone();
    let target = target.clone();

    let effective_targets = crate::game::targeting::resolved_targets(ability, &target, state);
    let mut ids = crate::game::effects::effect_object_targets(&target, &effective_targets);
    if ids.is_empty() {
        ids.push(ability.source_id);
    }

    let mut changed = false;
    for id in ids {
        if let Some(obj) = state.objects.get_mut(&id) {
            obj.apply_perpetual_modification(&modification);
            changed = true;
        }
    }

    if changed {
        // CR 613.1: a perpetual edit to base power/toughness changes a
        // characteristic that the layer pass derives live P/T from, so the board
        // must be re-evaluated — otherwise `obj.power`/`obj.toughness` and public
        // state stay at their pre-effect values until some unrelated future
        // layer-dirtying event. The `Full` flush also marks public state dirty.
        crate::game::layers::mark_layers_full(state);
        events.push(GameEvent::EffectResolved {
            kind: EffectKind::from(&ability.effect),
            source_id: ability.source_id,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::game::zones::create_object;
    use crate::types::ability::{Effect, PerpetualModification, ResolvedAbility, TargetRef};
    use crate::types::events::GameEvent;
    use crate::types::game_state::GameState;
    use crate::types::identifiers::CardId;
    use crate::types::player::PlayerId;
    use crate::types::zones::Zone;

    #[test]
    fn perpetual_sets_base_power_toughness_and_records_it() {
        let mut state = GameState::new_two_player(7);
        let id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Three Tree Battalion Duplicate".to_string(),
            Zone::Battlefield,
        );
        state.objects.get_mut(&id).unwrap().base_power = Some(5);
        state.objects.get_mut(&id).unwrap().base_toughness = Some(5);

        let modification = PerpetualModification::SetBasePowerToughness {
            power: 1,
            toughness: 1,
        };
        let ability = ResolvedAbility::new(
            Effect::ApplyPerpetual {
                target: crate::types::ability::TargetFilter::Any,
                modification: modification.clone(),
            },
            vec![TargetRef::Object(id)],
            id,
            PlayerId(0),
        );
        let mut events = Vec::new();
        super::resolve(&mut state, &ability, &mut events).unwrap();

        let obj = state.objects.get(&id).unwrap();
        assert_eq!(obj.base_power, Some(1));
        assert_eq!(obj.base_toughness, Some(1));
        assert!(obj.perpetual_mods.contains(&modification));
    }

    /// CR 613.1: the perpetual base-P/T edit must dirty layers so the live,
    /// publicly visible `power`/`toughness` are recomputed at the next flush —
    /// not just the persistent `base_*` fields. Mirrors the rules/display
    /// boundary (`flush_layers`, a no-op unless `layers_dirty` is set).
    #[test]
    fn perpetual_base_pt_updates_live_pt_after_layer_flush() {
        let mut state = GameState::new_two_player(7);
        let id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "High Fae Prankster".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&id).unwrap();
            obj.base_power = Some(2);
            obj.base_toughness = Some(2);
        }
        // Establish the pre-effect live P/T through the normal layer pass.
        crate::game::layers::mark_layers_full(&mut state);
        crate::game::layers::flush_layers(&mut state);
        assert_eq!(state.objects.get(&id).unwrap().power, Some(2));

        let ability = ResolvedAbility::new(
            Effect::ApplyPerpetual {
                target: crate::types::ability::TargetFilter::Any,
                modification: PerpetualModification::SetBasePowerToughness {
                    power: 4,
                    toughness: 1,
                },
            },
            vec![TargetRef::Object(id)],
            id,
            PlayerId(0),
        );
        let mut events = Vec::new();
        super::resolve(&mut state, &ability, &mut events).unwrap();

        // The resolver must have dirtied layers; flushing recomputes live P/T.
        crate::game::layers::flush_layers(&mut state);
        let obj = state.objects.get(&id).unwrap();
        assert_eq!(obj.power, Some(4));
        assert_eq!(obj.toughness, Some(1));
    }

    #[test]
    fn perpetual_modify_pt_adds_to_base_and_updates_live_pt() {
        let mut state = GameState::new_two_player(7);
        let id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Heir to Dragonfire".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&id).unwrap();
            obj.base_power = Some(1);
            obj.base_toughness = Some(1);
        }
        crate::game::layers::mark_layers_full(&mut state);
        crate::game::layers::flush_layers(&mut state);

        let ability = ResolvedAbility::new(
            Effect::ApplyPerpetual {
                target: crate::types::ability::TargetFilter::Any,
                modification: PerpetualModification::ModifyPowerToughness {
                    power_delta: 3,
                    toughness_delta: 3,
                },
            },
            vec![TargetRef::Object(id)],
            id,
            PlayerId(0),
        );
        let mut events = Vec::new();
        super::resolve(&mut state, &ability, &mut events).unwrap();

        crate::game::layers::flush_layers(&mut state);
        let obj = state.objects.get(&id).unwrap();
        assert_eq!(obj.base_power, Some(4));
        assert_eq!(obj.base_toughness, Some(4));
        assert_eq!(obj.power, Some(4));
        assert_eq!(obj.toughness, Some(4));
    }

    #[test]
    fn perpetual_grant_keywords_adds_to_object() {
        use crate::types::keywords::Keyword;

        let mut state = GameState::new_two_player(7);
        let id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Monoist Gravliner".to_string(),
            Zone::Battlefield,
        );

        let modification = PerpetualModification::GrantKeywords {
            keywords: vec![Keyword::Deathtouch, Keyword::Lifelink],
        };
        let ability = ResolvedAbility::new(
            Effect::ApplyPerpetual {
                target: crate::types::ability::TargetFilter::Any,
                modification: modification.clone(),
            },
            vec![TargetRef::Object(id)],
            id,
            PlayerId(0),
        );
        let mut events = Vec::new();
        super::resolve(&mut state, &ability, &mut events).unwrap();

        let obj = state.objects.get(&id).unwrap();
        assert!(obj.has_keyword(&Keyword::Deathtouch));
        assert!(obj.has_keyword(&Keyword::Lifelink));
        assert!(obj.perpetual_mods.contains(&modification));
    }

    #[test]
    fn perpetual_grant_keywords_parent_target_uses_stationed_event() {
        use crate::types::ability::TargetFilter;
        use crate::types::keywords::Keyword;

        let mut state = GameState::new_two_player(7);
        let spacecraft = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Monoist Gravliner".to_string(),
            Zone::Battlefield,
        );
        let creature = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Stationing Creature".to_string(),
            Zone::Battlefield,
        );
        state.current_trigger_event = Some(GameEvent::Stationed {
            spacecraft_id: spacecraft,
            creature_id: creature,
            counters_added: 1,
        });

        let modification = PerpetualModification::GrantKeywords {
            keywords: vec![Keyword::Deathtouch, Keyword::Lifelink],
        };
        let ability = ResolvedAbility::new(
            Effect::ApplyPerpetual {
                target: TargetFilter::ParentTarget,
                modification: modification.clone(),
            },
            vec![],
            spacecraft,
            PlayerId(0),
        );
        let mut events = Vec::new();
        super::resolve(&mut state, &ability, &mut events).unwrap();

        let stationer = state.objects.get(&creature).unwrap();
        assert!(stationer.has_keyword(&Keyword::Deathtouch));
        assert!(stationer.has_keyword(&Keyword::Lifelink));
        assert!(!state
            .objects
            .get(&spacecraft)
            .unwrap()
            .has_keyword(&Keyword::Deathtouch));
    }
}
