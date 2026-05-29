use crate::game::effects::life::{apply_damage_life_loss, apply_life_gain};
use crate::game::static_abilities::{player_has_cant_gain_life, player_has_cant_lose_life};
use crate::types::ability::{
    ContinuousModification, Duration, Effect, EffectError, EffectKind, PtStat, ResolvedAbility,
    TargetFilter, TargetRef,
};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;

/// CR 701.12g: Exchange a player's life total with the source permanent's power
/// or toughness (Tree of Perdition, Tree of Redemption, Evra, Halcyon Witness).
///
/// CR 701.12g: each value becomes equal to the previous value of the other, so
/// both previous values must be read before either changes. When a life total
/// is involved the player gains or loses the amount of life necessary to reach
/// the other value (it is a gain/loss, not a set), and the exchange is
/// all-or-nothing: if the life change is forbidden (CR 119.7 can't-gain when
/// raising, CR 119.8 can't-lose when lowering), no part of the exchange occurs.
///
/// The stat side becomes an indefinite layer-7b continuous "set" effect
/// (CR 613.4b) on the source: its base power/toughness is set to the player's
/// previous life total, so counters and +N/+N modifiers (both CR 613.4c) still
/// apply on top per the card's rulings.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let stat = match &ability.effect {
        Effect::ExchangeLifeWithStat { stat, .. } => *stat,
        // Dispatcher in effects/mod.rs only routes ExchangeLifeWithStat here.
        _ => return Ok(()),
    };

    let resolved_kind = EffectKind::from(&ability.effect);

    // "your life total" forms declare no player target and fall back to the
    // ability's controller; "target opponent's life total" supplies a player.
    let player_id = ability
        .targets
        .iter()
        .find_map(|t| match t {
            TargetRef::Player(pid) => Some(*pid),
            TargetRef::Object(_) => None,
        })
        .unwrap_or(ability.controller);

    // CR 701.12a: capture both previous values before any mutation.
    let Some(source) = state.objects.get(&ability.source_id) else {
        // Source left the battlefield before resolution — the stat side can't
        // be completed, so per CR 701.12a nothing happens.
        events.push(GameEvent::EffectResolved {
            kind: resolved_kind,
            source_id: ability.source_id,
        });
        return Ok(());
    };
    let stat_value = match stat {
        PtStat::Power => source.power,
        PtStat::Toughness => source.toughness,
        PtStat::TotalPowerToughness => {
            return Err(EffectError::InvalidParam(
                "total power and toughness is not exchangeable as a single stat".to_string(),
            ));
        }
    };
    let Some(stat_value) = stat_value else {
        // Source has no power/toughness (not a creature) — can't complete.
        events.push(GameEvent::EffectResolved {
            kind: resolved_kind,
            source_id: ability.source_id,
        });
        return Ok(());
    };

    let old_life = state
        .players
        .iter()
        .find(|p| p.id == player_id)
        .ok_or(EffectError::PlayerNotFound)?
        .life;

    // CR 701.12g + CR 119.7 / CR 119.8: All-or-nothing. The player would gain or
    // lose life to reach `stat_value`. If that change is forbidden (can't-gain
    // when raising, can't-lose when lowering), no part of the exchange occurs.
    let life_blocked = match stat_value.cmp(&old_life) {
        std::cmp::Ordering::Greater => player_has_cant_gain_life(state, player_id),
        std::cmp::Ordering::Less => player_has_cant_lose_life(state, player_id),
        std::cmp::Ordering::Equal => false,
    };
    if life_blocked {
        events.push(GameEvent::EffectResolved {
            kind: resolved_kind,
            source_id: ability.source_id,
        });
        return Ok(());
    }

    // CR 613.4b: Set the source's exchanged stat to the player's previous life
    // total via an indefinite (Duration::Permanent) layer-7b continuous effect.
    let modification = match stat {
        PtStat::Power => ContinuousModification::SetPower { value: old_life },
        PtStat::Toughness => ContinuousModification::SetToughness { value: old_life },
        PtStat::TotalPowerToughness => {
            return Err(EffectError::InvalidParam(
                "total power and toughness is not exchangeable as a single stat".to_string(),
            ));
        }
    };
    state.add_transient_continuous_effect(
        ability.source_id,
        ability.controller,
        Duration::Permanent,
        TargetFilter::SpecificObject {
            id: ability.source_id,
        },
        vec![modification],
        None,
    );

    // CR 701.12g: the player gains or loses the difference to reach the stat's
    // previous value (a gain/loss, not a set). The helpers re-check CR
    // 119.7/119.8 and route through the replacement pipeline.
    let diff = stat_value - old_life;
    let deferred = match diff.signum() {
        1 => apply_life_gain(state, player_id, diff as u32, events).err(),
        -1 => apply_damage_life_loss(state, player_id, (-diff) as u32, events).err(),
        _ => None,
    };
    if deferred.is_some() {
        // CR 614.7: a competing replacement required a player choice; the helper
        // installed the WaitingFor and the resume path completes resolution.
        return Ok(());
    }

    events.push(GameEvent::EffectResolved {
        kind: resolved_kind,
        source_id: ability.source_id,
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::layers::evaluate_layers;
    use crate::game::zones::create_object;
    use crate::types::ability::{ContinuousModification, ControllerRef, TypedFilter};
    use crate::types::card_type::{CardType, CoreType};
    use crate::types::identifiers::CardId;
    use crate::types::player::PlayerId;
    use crate::types::zones::Zone;

    /// Builds a creature on the battlefield with the given base power/toughness
    /// and recomputes layers so `power`/`toughness` are populated.
    fn create_creature(
        state: &mut GameState,
        owner: PlayerId,
        power: i32,
        toughness: i32,
    ) -> crate::types::identifiers::ObjectId {
        let id = create_object(
            state,
            CardId(700),
            owner,
            "Tree".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.base_power = Some(power);
        obj.base_toughness = Some(toughness);
        obj.base_card_types = CardType {
            supertypes: vec![],
            core_types: vec![CoreType::Creature],
            subtypes: vec![],
        };
        evaluate_layers(state);
        id
    }

    /// CR 701.12a + CR 119.5 + CR 613.4b: Tree of Perdition exchanges the
    /// opponent's life total with the source's toughness. The opponent's life
    /// becomes the toughness; the toughness is set (layer 7b) to the opponent's
    /// previous life.
    #[test]
    fn exchange_sets_opponent_life_and_source_toughness() {
        let mut state = GameState::new_two_player(42);
        let source = create_creature(&mut state, PlayerId(0), 0, 13);
        state.players[1].life = 25;

        let ability = ResolvedAbility::new(
            Effect::ExchangeLifeWithStat {
                player: TargetFilter::Typed(
                    TypedFilter::default().controller(ControllerRef::Opponent),
                ),
                stat: PtStat::Toughness,
            },
            vec![TargetRef::Player(PlayerId(1))],
            source,
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        // CR 119.5: opponent's life becomes the source's previous toughness.
        assert_eq!(state.players[1].life, 13);
        // CR 613.4b: a layer-7b set effect locks the source's toughness to the
        // opponent's previous life total.
        assert!(state.transient_continuous_effects.iter().any(|e| {
            e.source_id == source
                && e.modifications
                    .contains(&ContinuousModification::SetToughness { value: 25 })
        }));
        evaluate_layers(&mut state);
        assert_eq!(state.objects.get(&source).unwrap().toughness, Some(25));
    }

    /// CR 701.12a: "your life total" form exchanges the controller's life with
    /// the source's toughness, with no player target supplied.
    #[test]
    fn exchange_uses_controller_when_no_target() {
        let mut state = GameState::new_two_player(7);
        let source = create_creature(&mut state, PlayerId(0), 0, 13);
        state.players[0].life = 4;

        let ability = ResolvedAbility::new(
            Effect::ExchangeLifeWithStat {
                player: TargetFilter::Controller,
                stat: PtStat::Toughness,
            },
            vec![],
            source,
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.players[0].life, 13);
        evaluate_layers(&mut state);
        assert_eq!(state.objects.get(&source).unwrap().toughness, Some(4));
    }

    /// CR 701.12a + CR 119.7: All-or-nothing. If the player can't gain life and
    /// the exchange would raise their life, no part of the exchange occurs — the
    /// toughness is not set either.
    #[test]
    fn exchange_blocked_when_cant_gain_life_does_nothing() {
        use crate::types::ability::{StaticDefinition, TargetFilter as TF};
        use crate::types::statics::StaticMode;

        let mut state = GameState::new_two_player(99);
        let source = create_creature(&mut state, PlayerId(0), 0, 13);
        state.players[0].life = 5;
        // Player 0 can't gain life.
        let lock = create_object(
            &mut state,
            CardId(901),
            PlayerId(0),
            "Lock".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&lock)
            .unwrap()
            .static_definitions
            .push(
                StaticDefinition::new(StaticMode::CantGainLife).affected(TF::Typed(
                    TypedFilter::default().controller(ControllerRef::You),
                )),
            );

        let ability = ResolvedAbility::new(
            Effect::ExchangeLifeWithStat {
                player: TargetFilter::Controller,
                stat: PtStat::Toughness,
            },
            vec![],
            source,
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        // CR 701.12a: life unchanged (would have risen 5 → 13) ...
        assert_eq!(state.players[0].life, 5);
        // ... and the toughness was not set.
        assert!(!state.transient_continuous_effects.iter().any(|e| {
            matches!(
                e.modifications.first(),
                Some(ContinuousModification::SetToughness { .. })
            )
        }));
    }
}
