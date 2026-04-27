use crate::game::quantity::resolve_quantity_with_targets;
use crate::game::sacrifice::{self, SacrificeOutcome};
use crate::types::ability::{
    ControllerRef, Effect, EffectError, EffectKind, QuantityExpr, ResolvedAbility, TargetFilter,
    TargetRef,
};
use crate::types::events::GameEvent;
use crate::types::game_state::{GameState, WaitingFor};
use crate::types::identifiers::ObjectId;
use crate::types::player::PlayerId;
use crate::types::zones::Zone;

/// CR 701.16a: Resolve the set of players whose permanents are eligible for a
/// sacrifice effect, derived from the target filter's `ControllerRef`.
///
/// - `You` (or no controller clause): only the ability controller sacrifices
///   (the historical default).
/// - `Opponent`: each player other than the ability controller may be asked to
///   sacrifice. Per CR 701.16a, each affected player chooses their own
///   permanent; this resolver handles the single-opponent two-player case by
///   routing both filter scope and chooser to that opponent.
/// - `TargetPlayer`: the first `TargetRef::Player` in `ability.targets` —
///   matches the "target player sacrifices" / "that player sacrifices" pattern
///   used by Korvold, Ruthless Winnower, and similar cards.
fn resolve_sacrifice_scope(
    state: &GameState,
    ability: &ResolvedAbility,
    filter: &TargetFilter,
) -> Vec<PlayerId> {
    let scope = match filter {
        TargetFilter::Typed(t) => t.controller.clone(),
        _ => None,
    };
    match scope {
        None | Some(ControllerRef::You) => vec![ability.controller],
        Some(ControllerRef::Opponent) => state
            .players
            .iter()
            .map(|p| p.id)
            .filter(|&id| id != ability.controller)
            .collect(),
        Some(ControllerRef::TargetPlayer) => ability
            .targets
            .iter()
            .find_map(|t| match t {
                TargetRef::Player(pid) => Some(*pid),
                TargetRef::Object(_) => None,
            })
            .map(|pid| vec![pid])
            .unwrap_or_default(),
    }
}

/// CR 701.21a: To sacrifice a permanent, its controller moves it to its owner's graveyard.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    // CR 701.16a + CR 608.2c: Resolve the dynamic sacrifice count through
    // `resolve_quantity_with_targets` so `player_scope` iteration and per-
    // player refs (HandSize, ObjectCount{you-control}) resolve against the
    // rebound controller. A missing Sacrifice effect falls back to 1 so the
    // compatibility branch below preserves existing behavior.
    // CR 701.21a + CR 608.2d: Peel `UpTo` from the count expression to derive
    // the upper-bound expression and the may-pick-fewer flag. Plain
    // `QuantityExpr` (Fixed/Ref/HalfRounded/...) means a mandatory count;
    // wrapped in `UpTo` means the player may select 0..=count.
    let default_count = QuantityExpr::Fixed { value: 1 };
    let (filter, count_expr, up_to) = match &ability.effect {
        Effect::Sacrifice { target, count } => {
            let (inner, up_to) = count.peel_up_to();
            (target, inner, up_to)
        }
        _ => (&TargetFilter::Any, &default_count, false),
    };
    let count = resolve_quantity_with_targets(state, count_expr, ability).max(0) as usize;

    let targeted_objects: Vec<ObjectId> = ability
        .targets
        .iter()
        .filter_map(|target| match target {
            TargetRef::Object(obj_id) => Some(*obj_id),
            _ => None,
        })
        .collect();

    if targeted_objects.is_empty() {
        // CR 701.16a: Derive the player(s) whose permanents are in scope from
        // the target filter's ControllerRef. Defaults to `[ability.controller]`
        // when no controller clause is present (historical "you sacrifice"
        // default). For `Opponent` / `TargetPlayer`, each affected player is
        // both the filter scope and the chooser.
        let scoped_players = resolve_sacrifice_scope(state, ability, filter);
        // Fall back to the ability controller when no scope resolves (e.g.
        // TargetPlayer with no target selected). Preserves the prior behavior
        // for edge cases.
        let affected = if scoped_players.is_empty() {
            vec![ability.controller]
        } else {
            scoped_players
        };

        // Single-chooser case: one scoped player picks from their pool. Handles
        // 2-player "an opponent sacrifices" and all "target player sacrifices"
        // patterns. Multi-opponent multiplayer sacrifice is deferred to a
        // queued WaitingFor infrastructure.
        let chooser = affected[0];
        // CR 107.3a + CR 601.2b: ability-context filter evaluation.
        let ctx = crate::game::filter::FilterContext::from_ability(ability);
        let eligible: Vec<ObjectId> = state
            .battlefield
            .iter()
            .copied()
            .filter(|id| {
                state.objects.get(id).is_some_and(|obj| {
                    obj.controller == chooser
                        && !obj.is_emblem
                        && crate::game::filter::matches_target_filter(state, *id, filter, &ctx)
                })
            })
            .collect();

        if count == 0 {
            // CR 107.3a: A dynamic count that resolves to zero is a legal
            // no-op (e.g. "sacrifice half the permanents they control" when
            // the player controls none). Emit and exit without failing.
            events.push(GameEvent::EffectResolved {
                kind: EffectKind::from(&ability.effect),
                source_id: ability.source_id,
            });
            return Ok(());
        }

        if eligible.is_empty() {
            if !up_to {
                state.cost_payment_failed_flag = true;
            }
            events.push(GameEvent::EffectResolved {
                kind: EffectKind::from(&ability.effect),
                source_id: ability.source_id,
            });
            return Ok(());
        }

        // CR 701.16b: When the resolved count is at least the eligible pool
        // and the sacrifice is mandatory, every eligible permanent is
        // sacrificed — the player has no choice. Fast-path this rather than
        // round-tripping through EffectZoneChoice.
        if !up_to && eligible.len() <= count {
            let mut sacrificed: i32 = 0;
            for obj_id in eligible {
                match sacrifice::sacrifice_permanent(state, obj_id, chooser, events) {
                    Ok(SacrificeOutcome::Complete) => sacrificed += 1,
                    Ok(SacrificeOutcome::NeedsReplacementChoice(player)) => {
                        state.waiting_for =
                            crate::game::replacement::replacement_choice_waiting_for(player, state);
                        return Ok(());
                    }
                    Err(_) => {}
                }
            }
            state.last_effect_count = Some(sacrificed);
            events.push(GameEvent::EffectResolved {
                kind: EffectKind::from(&ability.effect),
                source_id: ability.source_id,
            });
            return Ok(());
        }

        // CR 701.16a: "Sacrifice N permanents" — the affected player picks
        // which `count` permanents out of the eligible pool. Clamped to pool
        // size for safety; the branch above handles the mandatory-all case.
        let choice_count = count.min(eligible.len());
        state.waiting_for = WaitingFor::EffectZoneChoice {
            player: chooser,
            cards: eligible,
            count: choice_count,
            up_to,
            source_id: ability.source_id,
            effect_kind: EffectKind::Sacrifice,
            zone: Zone::Battlefield,
            destination: None,
            enter_tapped: false,
            enter_transformed: false,
            under_your_control: false,
            enters_attacking: false,
            owner_library: false,
        };

        // EffectResolved is emitted by the EffectZoneChoice handler after the player chooses
        // (matching the DiscardChoice pattern — single authority for the event).
        return Ok(());
    }

    for obj_id in targeted_objects {
        let obj = state
            .objects
            .get(&obj_id)
            .ok_or(EffectError::ObjectNotFound(obj_id))?;

        // CR 114.5: Emblems cannot be sacrificed
        if obj.is_emblem {
            continue;
        }

        // CR 701.21a: A player can't sacrifice something that isn't a permanent.
        if obj.zone != Zone::Battlefield {
            continue;
        }

        let player_id = obj.controller;

        match sacrifice::sacrifice_permanent(state, obj_id, player_id, events) {
            Ok(SacrificeOutcome::Complete) => {}
            Ok(SacrificeOutcome::NeedsReplacementChoice(player)) => {
                state.waiting_for =
                    crate::game::replacement::replacement_choice_waiting_for(player, state);
                return Ok(());
            }
            Err(_) => {
                // Object may have left the battlefield between check and sacrifice;
                // skip silently (same as the zone check above).
                continue;
            }
        }
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::from(&ability.effect),
        source_id: ability.source_id,
    });

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::ability::{Effect, TargetFilter};
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::player::PlayerId;

    fn make_sacrifice_ability(target: ObjectId) -> ResolvedAbility {
        ResolvedAbility::new(
            Effect::Sacrifice {
                target: TargetFilter::Any,
                count: QuantityExpr::Fixed { value: 1 },
            },
            vec![TargetRef::Object(target)],
            ObjectId(100),
            PlayerId(0),
        )
    }

    fn make_choice_sacrifice_ability(up_to: bool) -> ResolvedAbility {
        let count = if up_to {
            QuantityExpr::up_to(QuantityExpr::Fixed { value: 1 })
        } else {
            QuantityExpr::Fixed { value: 1 }
        };
        ResolvedAbility::new(
            Effect::Sacrifice {
                target: TargetFilter::Any,
                count,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        )
    }

    #[test]
    fn sacrifice_moves_to_graveyard() {
        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Creature".to_string(),
            Zone::Battlefield,
        );
        let ability = make_sacrifice_ability(obj_id);
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(!state.battlefield.contains(&obj_id));
        assert!(state.players[0].graveyard.contains(&obj_id));
    }

    #[test]
    fn sacrifice_emits_permanent_sacrificed_event() {
        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Creature".to_string(),
            Zone::Battlefield,
        );
        let ability = make_sacrifice_ability(obj_id);
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(events.iter().any(|e| matches!(e, GameEvent::PermanentSacrificed { object_id, player_id } if *object_id == obj_id && *player_id == PlayerId(0))));
    }

    #[test]
    fn empty_targets_sets_effect_zone_choice_when_multiple_permanents_exist() {
        let mut state = GameState::new_two_player(42);
        let a = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "A".to_string(),
            Zone::Battlefield,
        );
        let b = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "B".to_string(),
            Zone::Battlefield,
        );
        let ability = make_choice_sacrifice_ability(false);
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        match &state.waiting_for {
            WaitingFor::EffectZoneChoice {
                player,
                cards,
                count,
                effect_kind,
                zone,
                ..
            } => {
                assert_eq!(*player, PlayerId(0));
                assert_eq!(*count, 1);
                assert_eq!(*effect_kind, EffectKind::Sacrifice);
                assert_eq!(*zone, Zone::Battlefield);
                assert!(cards.contains(&a));
                assert!(cards.contains(&b));
            }
            other => panic!("expected EffectZoneChoice, got {other:?}"),
        }
    }

    #[test]
    fn empty_targets_with_single_permanent_auto_sacrifices_and_records_count() {
        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Only Permanent".to_string(),
            Zone::Battlefield,
        );
        let ability = make_choice_sacrifice_ability(false);
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(!state.battlefield.contains(&obj_id));
        assert!(state.players[0].graveyard.contains(&obj_id));
        assert_eq!(state.last_effect_count, Some(1));
    }

    #[test]
    fn mandatory_empty_target_sacrifice_without_permanents_sets_failure_flag() {
        let mut state = GameState::new_two_player(42);
        let ability = make_choice_sacrifice_ability(false);
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(state.cost_payment_failed_flag);
    }

    // CR 701.16a: When the target filter scopes sacrifice to opponents
    // (ControllerRef::Opponent) or a target player (ControllerRef::TargetPlayer),
    // the affected player — not the ability controller — both provides the
    // eligible permanent pool and makes the choice.
    fn make_scoped_sacrifice_ability(
        controller: ControllerRef,
        targets: Vec<TargetRef>,
    ) -> ResolvedAbility {
        // `TypedFilter::default()` with only a controller clause bypasses the
        // type-filter check (type_filters is empty → passes unconditionally),
        // letting the tests focus on controller scoping without wiring up a
        // full core_types vec on each bare-name test object.
        let typed = crate::types::ability::TypedFilter::default().controller(controller);
        ResolvedAbility::new(
            Effect::Sacrifice {
                target: TargetFilter::Typed(typed),
                count: QuantityExpr::Fixed { value: 1 },
            },
            targets,
            ObjectId(100),
            PlayerId(0),
        )
    }

    #[test]
    fn opponent_scope_routes_choice_to_opponent() {
        let mut state = GameState::new_two_player(42);
        // Ability controller permanent — must NOT appear in eligible pool.
        let _own = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Mine".to_string(),
            Zone::Battlefield,
        );
        let opp_a = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "OppA".to_string(),
            Zone::Battlefield,
        );
        let opp_b = create_object(
            &mut state,
            CardId(3),
            PlayerId(1),
            "OppB".to_string(),
            Zone::Battlefield,
        );
        let ability = make_scoped_sacrifice_ability(ControllerRef::Opponent, vec![]);
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        match &state.waiting_for {
            WaitingFor::EffectZoneChoice { player, cards, .. } => {
                assert_eq!(*player, PlayerId(1), "opponent must be the chooser");
                assert!(cards.contains(&opp_a) && cards.contains(&opp_b));
                assert_eq!(cards.len(), 2, "ability controller's permanent excluded");
            }
            other => panic!("expected EffectZoneChoice, got {other:?}"),
        }
    }

    #[test]
    fn target_player_scope_routes_choice_to_target_player() {
        let mut state = GameState::new_two_player(42);
        let _own = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Mine".to_string(),
            Zone::Battlefield,
        );
        let tp_a = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "TpA".to_string(),
            Zone::Battlefield,
        );
        let tp_b = create_object(
            &mut state,
            CardId(3),
            PlayerId(1),
            "TpB".to_string(),
            Zone::Battlefield,
        );
        let ability = make_scoped_sacrifice_ability(
            ControllerRef::TargetPlayer,
            vec![TargetRef::Player(PlayerId(1))],
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        match &state.waiting_for {
            WaitingFor::EffectZoneChoice { player, cards, .. } => {
                assert_eq!(*player, PlayerId(1));
                assert!(cards.contains(&tp_a) && cards.contains(&tp_b));
                assert_eq!(cards.len(), 2);
            }
            other => panic!("expected EffectZoneChoice, got {other:?}"),
        }
    }

    #[test]
    fn up_to_empty_target_sacrifice_without_permanents_does_not_fail() {
        let mut state = GameState::new_two_player(42);
        let ability = make_choice_sacrifice_ability(true);
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(!state.cost_payment_failed_flag);
        assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
    }
}
