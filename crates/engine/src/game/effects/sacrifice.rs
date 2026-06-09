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

/// Resolve the set of players whose permanents are eligible for a sacrifice
/// effect, derived from the target filter's `ControllerRef`.
///
/// CR 701.21a: A player can only sacrifice a permanent they control.
///
/// - `You` (or no controller clause): only the ability controller sacrifices
///   (the historical default).
/// - `Opponent`: each player other than the ability controller may be asked to
///   sacrifice. Per CR 701.21a, each affected player can only sacrifice their
///   own permanent; this resolver handles the single-opponent two-player case
///   by routing both filter scope and chooser to that opponent.
/// - `ScopedPlayer`: an event-context player such as the active player for
///   upkeep triggers.
/// - `TargetPlayer`: the first `TargetRef::Player` in `ability.targets` —
///   matches explicit "target player sacrifices" patterns.
fn resolve_sacrifice_scope(
    state: &GameState,
    ability: &ResolvedAbility,
    filter: &TargetFilter,
) -> Vec<PlayerId> {
    let scope = sacrifice_controller_scope(filter);
    match scope {
        None | Some(ControllerRef::You) => vec![ability.controller],
        Some(ControllerRef::ScopedPlayer) => {
            let scoped = trigger_event_scoped_player(state, ability);
            vec![scoped.unwrap_or(ability.controller)]
        }
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
        Some(ControllerRef::ParentTargetController) => {
            crate::game::targeting::resolve_effect_player_ref(
                state,
                ability,
                &TargetFilter::ParentTargetController,
            )
            .map(|pid| vec![pid])
            .unwrap_or_default()
        }
        Some(ControllerRef::DefendingPlayer) => {
            crate::game::combat::defending_player_for_attacker(state, ability.source_id)
                .map(|pid| vec![pid])
                .unwrap_or_default()
        }
        // CR 613.1: Player persisted on the source via an "as ~ enters, choose
        // a player" replacement.
        Some(ControllerRef::SourceChosenPlayer) => {
            crate::game::game_object::source_chosen_player(state, ability.source_id)
                .map(|pid| vec![pid])
                .unwrap_or_default()
        }
        // CR 608.2c + CR 109.4: Player chosen by an earlier `Choose(Player)`
        // in this resolution.
        Some(ControllerRef::ChosenPlayer { index }) => ability
            .chosen_players
            .get(index as usize)
            .copied()
            .map(|pid| vec![pid])
            .unwrap_or_default(),
        // CR 603.2 + CR 109.4: The player identified by the triggering event.
        Some(ControllerRef::TriggeringPlayer) => state
            .current_trigger_event
            .as_ref()
            .and_then(|event| crate::game::targeting::extract_player_from_event(event, state))
            .map(|pid| vec![pid])
            .unwrap_or_default(),
    }
}

fn sacrifice_controller_scope(filter: &TargetFilter) -> Option<ControllerRef> {
    crate::game::effects::target_filter_controller_scope(filter)
}

fn trigger_event_scoped_player(state: &GameState, ability: &ResolvedAbility) -> Option<PlayerId> {
    ability.scoped_player.or_else(|| {
        state
            .current_trigger_event
            .as_ref()
            .and_then(|event| crate::game::targeting::extract_player_from_event(event, state))
    })
}

/// CR 701.21a: To sacrifice a permanent, its controller moves it to its owner's graveyard.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    // CR 609.3: Resolve the dynamic sacrifice count through
    // `resolve_quantity_with_targets` before attempting the sacrifice so
    // mandatory effects can do as much as possible against the rebound
    // controller. A missing Sacrifice effect falls back to 1 so the
    // compatibility branch below preserves existing behavior.
    // Peel `UpTo` from the count expression to derive the upper-bound
    // expression and the may-pick-fewer flag. Plain
    // `QuantityExpr` (Fixed/Ref/DivideRounded/...) means a mandatory count;
    // wrapped in `UpTo` means the player may select 0..=count.
    let default_count = QuantityExpr::Fixed { value: 1 };
    let (filter, count_expr, up_to, min_count) = match &ability.effect {
        Effect::Sacrifice {
            target,
            count,
            min_count,
        } => {
            let (inner, up_to) = count.peel_up_to();
            (target, inner, up_to, *min_count)
        }
        _ => (&TargetFilter::Any, &default_count, false, 0),
    };
    let scoped_ability;
    let ability = if matches!(
        sacrifice_controller_scope(filter),
        Some(ControllerRef::ScopedPlayer)
    ) {
        if let Some(player) = trigger_event_scoped_player(state, ability) {
            scoped_ability = {
                let mut scoped = ability.clone();
                scoped.set_scoped_player_recursive(player);
                scoped
            };
            &scoped_ability
        } else {
            ability
        }
    } else {
        ability
    };
    let count = resolve_quantity_with_targets(state, count_expr, ability).max(0) as usize;

    let targeted_objects = if matches!(
        sacrifice_controller_scope(filter),
        Some(ControllerRef::ParentTargetController)
    ) {
        Vec::new()
    } else {
        crate::game::effects::effect_object_targets(filter, &ability.targets)
    };

    if targeted_objects.is_empty() {
        // CR 701.21a: Derive the player(s) whose permanents are in scope from
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
                // CR 614.13a/b: restrict to objects present before the devourer co-entry
                // began; vacuous when None. (Pool is built from LIVE battlefield, so an
                // object an earlier co-entering devourer already sacrificed is excluded by
                // the live basis, and the devourers themselves by the snapshot.)
                state
                    .devour_eligible_snapshot
                    .as_ref()
                    .is_none_or(|s| s.contains(id))
                    && state.objects.get(id).is_some_and(|obj| {
                        obj.controller == chooser
                            && !obj.is_emblem
                            && crate::game::filter::matches_target_filter(state, *id, filter, &ctx)
                            && !crate::game::static_abilities::triggered_cause_sacrifice_or_exile_muzzled(
                                state,
                                ability,
                                *id,
                                chooser,
                            )
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

        // CR 701.21a + CR 609.3: When the resolved count is at least the
        // eligible pool and the sacrifice is mandatory, sacrifice every
        // eligible permanent — the effect does as much as possible. Fast-path
        // this rather than round-tripping through EffectZoneChoice.
        if !up_to && eligible.len() <= count {
            let mut sacrificed: i32 = 0;
            for &obj_id in &eligible {
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
            // CR 701.21a + CR 603.10a + CR 608.2f: every eligible permanent was
            // sacrificed as part of the same resolution event, so co-departing
            // sacrifice/LTB observers (Blood Artist) observe each other.
            // `departed_subset` drops any permanent that didn't actually leave
            // (e.g. CantBeSacrificed members excluded upstream).
            crate::game::zones::mark_simultaneous_departures(
                events,
                &crate::game::zones::departed_subset(state, &eligible),
            );
            state.last_effect_count = Some(sacrificed);
            events.push(GameEvent::EffectResolved {
                kind: EffectKind::from(&ability.effect),
                source_id: ability.source_id,
            });
            return Ok(());
        }

        // CR 701.21a: "Sacrifice N permanents" — the affected player picks
        // which `count` permanents out of the eligible pool. Clamped to pool
        // size for safety; the branch above handles the mandatory-all case.
        let choice_count = count.min(eligible.len());
        state.waiting_for = WaitingFor::EffectZoneChoice {
            player: chooser,
            cards: eligible,
            count: choice_count,
            min_count: min_count.min(choice_count),
            up_to,
            source_id: ability.source_id,
            effect_kind: EffectKind::Sacrifice,
            zone: Zone::Battlefield,
            destination: None,
            enter_tapped: crate::types::zones::EtbTapState::Unspecified,
            enter_transformed: false,
            enters_under_player: None,
            enters_attacking: false,
            owner_library: false,
            track_exiled_by_source: false,
            count_param: 0,
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

        // CR 701.21a: Defense-in-depth — a player can only sacrifice permanents
        // they control. The primary fix is that Sacrifice no longer creates
        // target slots (see extract_target_filter_from_effect), but if this
        // path is ever reached, enforce controller ownership.
        if obj.controller != ability.controller {
            continue;
        }

        let player_id = obj.controller;

        if crate::game::static_abilities::triggered_cause_sacrifice_or_exile_muzzled(
            state, ability, obj_id, player_id,
        ) {
            continue;
        }

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
    use crate::game::effects::resolve_ability_chain;
    use crate::game::zones::create_object;
    use crate::types::ability::{
        AbilityKind, AggregateFunction, Comparator, ControllerRef, Effect, FilterProp,
        ObjectProperty, PtStat, PtValueScope, QuantityRef, TargetFilter, TypedFilter,
    };
    use crate::types::actions::GameAction;
    use crate::types::card_type::CoreType;
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::player::PlayerId;

    fn make_sacrifice_ability(target: ObjectId) -> ResolvedAbility {
        ResolvedAbility::new(
            Effect::Sacrifice {
                target: TargetFilter::Any,
                count: QuantityExpr::Fixed { value: 1 },
                min_count: 0,
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
                min_count: 0,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        )
    }

    /// CR 208.4b + CR 613.4b: Discriminating runtime test that base power/
    /// toughness (layer 7b) is enforced, NOT current power/toughness (after
    /// counters in 7c). This is the Angelic Aberration ETB filter:
    /// "sacrifice any number of creatures each with base power or toughness 1
    /// or less". The filter is `AnyOf [PtComparison{Power,Base,LE,1},
    /// PtComparison{Toughness,Base,LE,1}]`.
    ///
    /// The test drives the actual sacrifice handler (`resolve`), which computes
    /// the eligible set via `matches_target_filter` → `matches_filter_prop`'s
    /// base-scope arm. A base-1/1 creature carrying a +1/+1 counter (current
    /// 2/2) MUST be eligible (base power 1 ≤ 1) while a base-3/3 creature MUST
    /// NOT — the exact case that would fail under a current-P/T mapping.
    #[test]
    fn sacrifice_base_pt_filter_enforces_base_not_current() {
        use crate::types::ability::{Comparator, FilterProp, PtStat, PtValueScope, TypedFilter};
        use crate::types::card_type::CoreType;

        let mut state = GameState::new_two_player(42);

        // A: base 1/1 with a +1/+1 counter → current 2/2. Base power 1 ≤ 1 ⇒ eligible.
        let a = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Counter Goblin".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&a).unwrap();
            obj.card_types.core_types = vec![CoreType::Creature];
            obj.base_power = Some(1);
            obj.base_toughness = Some(1);
            obj.power = Some(2);
            obj.toughness = Some(2);
        }

        // B: base 3/3, current 3/3. Base power 3 and base toughness 3 ⇒ NOT eligible.
        let b = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Big Bear".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&b).unwrap();
            obj.card_types.core_types = vec![CoreType::Creature];
            obj.base_power = Some(3);
            obj.base_toughness = Some(3);
            obj.power = Some(3);
            obj.toughness = Some(3);
        }

        // C: base 0/1 ⇒ eligible (base power 0 ≤ 1). A second eligible creature
        // forces the multi-choice path so the handler exposes the eligible set
        // via `EffectZoneChoice.cards` rather than auto-sacrificing.
        let c = create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "Wall".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&c).unwrap();
            obj.card_types.core_types = vec![CoreType::Creature];
            obj.base_power = Some(0);
            obj.base_toughness = Some(1);
            obj.power = Some(0);
            obj.toughness = Some(1);
        }

        let filter =
            TargetFilter::Typed(TypedFilter::creature().properties(vec![FilterProp::AnyOf {
                props: vec![
                    FilterProp::PtComparison {
                        stat: PtStat::Power,
                        scope: PtValueScope::Base,
                        comparator: Comparator::LE,
                        value: QuantityExpr::Fixed { value: 1 },
                    },
                    FilterProp::PtComparison {
                        stat: PtStat::Toughness,
                        scope: PtValueScope::Base,
                        comparator: Comparator::LE,
                        value: QuantityExpr::Fixed { value: 1 },
                    },
                ],
            }]));

        // Mirror Angelic Aberration's actual count shape:
        // `UpTo(ObjectCount(<same base-PT filter>))`, min_count 0.
        let ability = ResolvedAbility::new(
            Effect::Sacrifice {
                target: filter.clone(),
                count: QuantityExpr::up_to(QuantityExpr::Ref {
                    qty: QuantityRef::ObjectCount { filter },
                }),
                min_count: 0,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        match &state.waiting_for {
            WaitingFor::EffectZoneChoice { cards, .. } => {
                assert!(
                    cards.contains(&a),
                    "base-1/1-with-counter (current 2/2) must be eligible (base power 1 ≤ 1)"
                );
                assert!(
                    cards.contains(&c),
                    "base-0/1 must be eligible (base power 0 ≤ 1)"
                );
                assert!(
                    !cards.contains(&b),
                    "base-3/3 must NOT be eligible — would be a current-vs-base bug only if it leaked in"
                );
            }
            other => panic!("expected EffectZoneChoice with eligible set, got {other:?}"),
        }
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
    fn interactive_sacrifice_publishes_tracked_set_for_this_way_draw() {
        let mut state = GameState::new_two_player(42);
        let sacrifice_a = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Permanent A".to_string(),
            Zone::Battlefield,
        );
        let sacrifice_b = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Permanent B".to_string(),
            Zone::Battlefield,
        );
        for index in 0..2 {
            create_object(
                &mut state,
                CardId(10 + index),
                PlayerId(0),
                format!("Library Card {index}"),
                Zone::Library,
            );
        }

        let mut ability = ResolvedAbility::new(
            Effect::Sacrifice {
                target: TargetFilter::Any,
                count: QuantityExpr::up_to(QuantityExpr::Fixed { value: 2 }),
                min_count: 0,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        ability.sub_ability = Some(Box::new(ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Ref {
                    qty: QuantityRef::TrackedSetSize,
                },
                target: TargetFilter::Controller,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        )));
        ability.kind = AbilityKind::Spell;

        let hand_before = state.players[0].hand.len();
        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

        assert!(matches!(
            state.waiting_for,
            WaitingFor::EffectZoneChoice {
                effect_kind: EffectKind::Sacrifice,
                up_to: true,
                ..
            }
        ));

        crate::game::engine::apply(
            &mut state,
            PlayerId(0),
            GameAction::SelectCards {
                cards: vec![sacrifice_a, sacrifice_b],
            },
        )
        .unwrap();

        assert_eq!(state.last_effect_count, Some(2));
        assert!(state.players[0].graveyard.contains(&sacrifice_a));
        assert!(state.players[0].graveyard.contains(&sacrifice_b));
        assert_eq!(
            state.players[0].hand.len() - hand_before,
            2,
            "draw should read TrackedSetSize from permanents sacrificed this way"
        );
    }

    #[test]
    fn mandatory_empty_target_sacrifice_without_permanents_sets_failure_flag() {
        let mut state = GameState::new_two_player(42);
        let ability = make_choice_sacrifice_ability(false);
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(state.cost_payment_failed_flag);
    }

    // CR 701.21a: When the target filter scopes sacrifice to opponents
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
                min_count: 0,
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
    fn parent_target_controller_scope_routes_choice_to_parent_controller() {
        let mut state = GameState::new_two_player(42);
        let parent = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Target Permanent".to_string(),
            Zone::Battlefield,
        );
        let _own_land = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Own Land".to_string(),
            Zone::Battlefield,
        );
        let their_land_a = create_object(
            &mut state,
            CardId(3),
            PlayerId(1),
            "Their Land A".to_string(),
            Zone::Battlefield,
        );
        let their_land_b = create_object(
            &mut state,
            CardId(4),
            PlayerId(1),
            "Their Land B".to_string(),
            Zone::Battlefield,
        );
        for id in [_own_land, their_land_a, their_land_b] {
            state
                .objects
                .get_mut(&id)
                .expect("test land exists")
                .card_types
                .core_types
                .push(crate::types::card_type::CoreType::Land);
        }
        let ability = ResolvedAbility::new(
            Effect::Sacrifice {
                target: TargetFilter::Typed(
                    crate::types::ability::TypedFilter::land()
                        .controller(ControllerRef::ParentTargetController),
                ),
                count: QuantityExpr::Fixed { value: 1 },
                min_count: 0,
            },
            vec![TargetRef::Object(parent)],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        match &state.waiting_for {
            WaitingFor::EffectZoneChoice { player, cards, .. } => {
                assert_eq!(*player, PlayerId(1));
                assert!(cards.contains(&their_land_a) && cards.contains(&their_land_b));
                assert_eq!(cards.len(), 2);
            }
            other => panic!("expected EffectZoneChoice, got {other:?}"),
        }
    }

    #[test]
    fn parent_target_controller_scope_uses_damage_event_source_controller() {
        let mut state = GameState::new_two_player(42);
        let damage_source = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Damage Source".to_string(),
            Zone::Stack,
        );
        let own = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Mine".to_string(),
            Zone::Battlefield,
        );
        let source_controller_permanent = create_object(
            &mut state,
            CardId(3),
            PlayerId(1),
            "Theirs".to_string(),
            Zone::Battlefield,
        );
        state.current_trigger_event = Some(GameEvent::DamageDealt {
            source_id: damage_source,
            target: TargetRef::Player(PlayerId(0)),
            amount: 1,
            is_combat: false,
            excess: 0,
        });
        let ability = make_scoped_sacrifice_ability(ControllerRef::ParentTargetController, vec![]);
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(state.battlefield.contains(&own));
        assert!(!state.battlefield.contains(&source_controller_permanent));
        assert!(state.players[1]
            .graveyard
            .contains(&source_controller_permanent));
        assert_eq!(state.last_effect_count, Some(1));
    }

    #[test]
    fn scoped_player_scope_uses_trigger_event_player() {
        let mut state = GameState::new_two_player(42);
        state.active_player = PlayerId(1);
        state.current_trigger_event = Some(GameEvent::PhaseChanged {
            phase: crate::types::phase::Phase::Upkeep,
        });
        let _own = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Mine".to_string(),
            Zone::Battlefield,
        );
        let scoped_a = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "ScopedA".to_string(),
            Zone::Battlefield,
        );
        let scoped_b = create_object(
            &mut state,
            CardId(3),
            PlayerId(1),
            "ScopedB".to_string(),
            Zone::Battlefield,
        );
        let ability = make_scoped_sacrifice_ability(ControllerRef::ScopedPlayer, vec![]);
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        match &state.waiting_for {
            WaitingFor::EffectZoneChoice { player, cards, .. } => {
                assert_eq!(*player, PlayerId(1));
                assert!(cards.contains(&scoped_a) && cards.contains(&scoped_b));
                assert_eq!(cards.len(), 2);
            }
            other => panic!("expected EffectZoneChoice, got {other:?}"),
        }
    }

    /// CR 701.21a: Even if the targeted path is reached (defense-in-depth),
    /// sacrifice must skip permanents not controlled by the ability controller.
    #[test]
    fn targeted_path_skips_opponent_permanents() {
        let mut state = GameState::new_two_player(42);
        // Create a permanent controlled by the opponent
        let opp_obj = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Opponent Creature".to_string(),
            Zone::Battlefield,
        );
        // Simulate the targeted path with an opponent's object as target
        let ability = make_sacrifice_ability(opp_obj);
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        // The opponent's permanent must NOT be sacrificed
        assert!(
            state.battlefield.contains(&opp_obj),
            "opponent's permanent should remain on battlefield"
        );
        assert!(
            !state.players[1].graveyard.contains(&opp_obj),
            "opponent's permanent should not be in graveyard"
        );
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

    /// Issue #320 (Tergrid's Shadow): "Each player sacrifices two creatures."
    /// parses as `Effect::Sacrifice { target: Typed(Creature, controller: None) }`
    /// with `player_scope: All`. The player_scope iteration loop must rebind
    /// `controller` to each player so the sacrifice resolver picks the
    /// iterated player as chooser. Resolved
    /// incidentally by the issue #310 spell-cast `player_scope` propagation
    /// fix, but pinned here at the resolver layer for direct coverage.
    #[test]
    fn player_scope_all_sacrifice_iterates_each_player() {
        use crate::game::effects::resolve_ability_chain;
        use crate::types::ability::{PlayerFilter, TypeFilter, TypedFilter};
        use crate::types::card_type::CoreType;

        let mut state = GameState::new_two_player(42);
        // Caster has 2 creatures, opponent has 2 creatures.
        let mut all_creatures = Vec::new();
        for (player, base) in [(PlayerId(0), 10), (PlayerId(1), 20)] {
            for offset in 0..2 {
                let id = create_object(
                    &mut state,
                    CardId(base + offset),
                    player,
                    format!("P{} Creature {offset}", player.0),
                    Zone::Battlefield,
                );
                state
                    .objects
                    .get_mut(&id)
                    .unwrap()
                    .card_types
                    .core_types
                    .push(CoreType::Creature);
                all_creatures.push((player, id));
            }
        }

        let mut ability = ResolvedAbility::new(
            Effect::Sacrifice {
                target: TargetFilter::Typed(TypedFilter::new(TypeFilter::Creature)),
                count: QuantityExpr::Fixed { value: 2 },
                min_count: 0,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        ability.player_scope = Some(PlayerFilter::All);

        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

        // First scoped iteration is APNAP — caster (PlayerId 0). Both their
        // creatures are auto-sacrificed since count == eligible count.
        assert!(
            state.players[0]
                .graveyard
                .iter()
                .filter(|id| all_creatures.iter().any(|(_, c)| c == *id))
                .count()
                == 2,
            "caster must sacrifice 2 creatures"
        );

        // Second iteration enters EffectZoneChoice because P1 has exactly 2
        // creatures and count is exactly 2 — but the auto-take path applies
        // when eligible == count. Either way, the affected player must be
        // PlayerId(1). If a choice is pending, validate that.
        match &state.waiting_for {
            WaitingFor::EffectZoneChoice { player, .. } => {
                assert_eq!(*player, PlayerId(1), "second scoped player must be P1");
            }
            WaitingFor::Priority { .. } => {
                // Auto-resolved (eligible == count).
                assert_eq!(
                    state.players[1]
                        .graveyard
                        .iter()
                        .filter(|id| all_creatures.iter().any(|(_, c)| c == *id))
                        .count(),
                    2,
                    "opponent must also sacrifice 2 creatures"
                );
            }
            other => panic!("unexpected waiting_for: {other:?}"),
        }
    }

    /// Issue #458: Scapeshift end-to-end. Drives the real parser + engine
    /// pipeline — parse the actual Oracle text, assert the AST, then resolve
    /// the chain and verify the player may sacrifice 0..=all lands (not exactly
    /// one) and search for "that many" land cards.
    /// CR 107.1c (any number includes zero) + CR 608.2c (back-reference count).
    fn scapeshift_ability() -> ResolvedAbility {
        let parsed = crate::parser::parse_oracle_text(
            "Sacrifice any number of lands. Search your library for up to that many \
             land cards, put them onto the battlefield tapped, then shuffle.",
            "Scapeshift",
            &[],
            &["Sorcery".to_string()],
            &[],
        );
        assert_eq!(
            parsed.abilities.len(),
            1,
            "Scapeshift has one spell ability"
        );
        crate::game::ability_utils::build_resolved_from_def(
            &parsed.abilities[0],
            ObjectId(100),
            PlayerId(0),
        )
    }

    fn assert_scapeshift_ast(effect: &Effect) {
        // Top-level: Sacrifice with UpTo(ObjectCount{Land}) and min_count 0.
        let Effect::Sacrifice {
            count, min_count, ..
        } = effect
        else {
            panic!("expected Effect::Sacrifice, got {effect:?}");
        };
        assert_eq!(*min_count, 0, "\"any number\" includes zero (CR 107.1c)");
        let QuantityExpr::UpTo { max } = count else {
            panic!("expected UpTo sacrifice count, got {count:?}");
        };
        assert!(
            matches!(
                **max,
                QuantityExpr::Ref {
                    qty: QuantityRef::ObjectCount { .. }
                }
            ),
            "expected ObjectCount ceiling, got {max:?}"
        );
    }

    fn scapeshift_search_effect(chain: &ResolvedAbility) -> Effect {
        let mut node = chain;
        loop {
            if matches!(node.effect, Effect::SearchLibrary { .. }) {
                return node.effect.clone();
            }
            node = node
                .sub_ability
                .as_deref()
                .expect("SearchLibrary must exist in the Scapeshift chain");
        }
    }

    fn scapeshift_state(land_card_ids: &[u64]) -> (GameState, Vec<ObjectId>) {
        use crate::types::card_type::CoreType;

        let mut state = GameState::new_two_player(42);
        state.turn_number = 2;
        state.phase = crate::types::phase::Phase::PreCombatMain;
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);
        state.waiting_for = WaitingFor::Priority {
            player: PlayerId(0),
        };

        // Five battlefield lands controlled by PlayerId(0).
        let mut battlefield_lands = Vec::new();
        for i in 0..5u64 {
            let id = create_object(
                &mut state,
                CardId(10 + i),
                PlayerId(0),
                format!("BF Land {i}"),
                Zone::Battlefield,
            );
            state
                .objects
                .get_mut(&id)
                .unwrap()
                .card_types
                .core_types
                .push(CoreType::Land);
            battlefield_lands.push(id);
        }

        // Land cards in the library.
        for &card_id in land_card_ids {
            let id = create_object(
                &mut state,
                CardId(card_id),
                PlayerId(0),
                format!("Library Land {card_id}"),
                Zone::Library,
            );
            state
                .objects
                .get_mut(&id)
                .unwrap()
                .card_types
                .core_types
                .push(CoreType::Land);
        }
        // A non-land library card to prove the search filter is applied.
        let spell = create_object(
            &mut state,
            CardId(99),
            PlayerId(0),
            "Library Spell".to_string(),
            Zone::Library,
        );
        state
            .objects
            .get_mut(&spell)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Instant);

        (state, battlefield_lands)
    }

    #[test]
    fn scapeshift_sacrifice_three_lands_searches_for_three() {
        use crate::game::engine::apply;
        use crate::types::actions::GameAction;

        let chain = scapeshift_ability();
        assert_scapeshift_ast(&chain.effect);

        // SearchLibrary count must carry the EventContextAmount back-reference,
        // wrapped as UpTo so the searcher picks 0..=that-many.
        match scapeshift_search_effect(&chain) {
            Effect::SearchLibrary { count, .. } => {
                let QuantityExpr::UpTo { max } = count else {
                    panic!("expected UpTo search count, got {count:?}");
                };
                assert_eq!(
                    *max,
                    QuantityExpr::Ref {
                        qty: QuantityRef::EventContextAmount
                    },
                    "search count must back-reference the sacrificed count"
                );
            }
            other => panic!("expected SearchLibrary, got {other:?}"),
        }

        let (mut state, battlefield_lands) = scapeshift_state(&[20, 21, 22, 23]);
        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &chain, &mut events, 0).unwrap();

        // Player may sacrifice 0..=5 lands — proves it is not a fixed 1.
        match &state.waiting_for {
            WaitingFor::EffectZoneChoice {
                player,
                cards,
                count,
                min_count,
                up_to,
                ..
            } => {
                assert_eq!(*player, PlayerId(0));
                assert_eq!(*count, 5, "all five battlefield lands are eligible");
                assert_eq!(*min_count, 0, "may sacrifice zero (CR 107.1c)");
                assert!(*up_to, "variable-count sacrifice");
                assert_eq!(cards.len(), 5);
            }
            other => panic!("expected EffectZoneChoice, got {other:?}"),
        }

        // Sacrifice exactly 3 lands.
        let chosen: Vec<ObjectId> = battlefield_lands[..3].to_vec();
        let result = apply(
            &mut state,
            PlayerId(0),
            GameAction::SelectCards {
                cards: chosen.clone(),
            },
        )
        .unwrap();

        for id in &chosen {
            assert!(
                state.players[0].graveyard.contains(id),
                "sacrificed land should be in graveyard"
            );
        }
        assert_eq!(state.last_effect_count, Some(3), "stamped sacrificed count");

        // The SearchLibrary continuation: pick limit is the "that many" = 3
        // (the sacrificed count). The eligible pool is all 4 library lands —
        // the non-land library card is excluded by the Land filter.
        let search_cards = match &result.waiting_for {
            WaitingFor::SearchChoice {
                player,
                cards,
                count,
                up_to,
                ..
            } => {
                assert_eq!(*player, PlayerId(0));
                assert_eq!(*count, 3, "\"that many\" resolved to 3 sacrificed");
                assert!(*up_to);
                assert_eq!(
                    cards.len(),
                    4,
                    "all 4 library lands match (non-land excluded)"
                );
                cards.clone()
            }
            other => panic!("expected SearchChoice, got {other:?}"),
        };

        // Select 3 land cards — they enter the battlefield tapped, then shuffle.
        let found: Vec<ObjectId> = search_cards[..3].to_vec();
        apply(
            &mut state,
            PlayerId(0),
            GameAction::SelectCards {
                cards: found.clone(),
            },
        )
        .unwrap();

        let on_bf_tapped = found
            .iter()
            .filter(|id| {
                state
                    .objects
                    .get(id)
                    .is_some_and(|obj| obj.zone == Zone::Battlefield && obj.tapped)
            })
            .count();
        assert_eq!(
            on_bf_tapped, 3,
            "all 3 found lands enter the battlefield tapped"
        );
    }

    #[test]
    fn scapeshift_sacrifice_zero_lands_searches_for_zero() {
        use crate::game::engine::apply;
        use crate::types::actions::GameAction;

        let chain = scapeshift_ability();
        let (mut state, _battlefield_lands) = scapeshift_state(&[20, 21, 22, 23]);
        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &chain, &mut events, 0).unwrap();

        assert!(matches!(
            state.waiting_for,
            WaitingFor::EffectZoneChoice { .. }
        ));

        // Sacrifice zero lands — must not panic. The "that many" back-reference
        // resolves to 0, so the search may pick at most 0 cards.
        let result = apply(
            &mut state,
            PlayerId(0),
            GameAction::SelectCards { cards: vec![] },
        )
        .unwrap();
        assert_eq!(state.last_effect_count, Some(0));
        assert!(
            state.players[0].graveyard.is_empty(),
            "no land should have been sacrificed, found {:?}",
            state.players[0].graveyard
        );
        // If a SearchChoice is presented at all, its pick limit must be 0.
        if let WaitingFor::SearchChoice { count, .. } = &result.waiting_for {
            assert_eq!(*count, 0, "search for \"up to 0\" must allow 0 picks");
        }
    }

    /// Issue #463 — Soul Shatter: "Each opponent sacrifices a creature or
    /// planeswalker with the greatest mana value among creatures and
    /// planeswalkers they control."
    ///
    /// CR 202.3 + CR 608.2h + CR 701.21a: each opponent's eligible pool must
    /// be restricted to *that opponent's* permanents tied for the greatest
    /// mana value among their own creatures/planeswalkers — never a global
    /// battlefield maximum.
    ///
    /// The board is constructed so the two opponents have *different* maxima
    /// and P2's max strictly exceeds P1's: P1 controls MV 2/3/5/5 (max 5),
    /// P2 controls MV 1/6 (max 6). A global-aggregate bug would compute
    /// max = 6 across both boards and offer P1 *nothing* (no MV-6 of P1's),
    /// while offering P2 only their MV-6. Correct per-controller scoping
    /// offers P1 exactly their two MV-5 permanents and P2 exactly their MV-6.
    /// Each opponent's resolution is driven through `resolve_ability_chain`
    /// with that opponent rebound as the acting controller — exactly what the
    /// `player_scope` loop in `effects/mod.rs` does per opponent.
    #[test]
    fn soul_shatter_offers_only_per_opponent_greatest_mv_permanents() {
        use crate::types::ability::{
            AggregateFunction, Comparator, ControllerRef, FilterProp, ObjectProperty, QuantityExpr,
            QuantityRef, TypeFilter, TypedFilter,
        };
        use crate::types::card_type::CoreType;
        use crate::types::mana::ManaCost;

        // 3-player game: caster P0, opponents P1 and P2.
        let mut state = GameState::new(crate::types::format::FormatConfig::standard(), 3, 99);

        // Place a creature/planeswalker for `owner` with the given mana value
        // and core type, returning its ObjectId.
        let place = |state: &mut GameState, owner: PlayerId, mv: u32, ty: CoreType| {
            let id = create_object(
                state,
                CardId(state.next_object_id),
                owner,
                format!("P{}-MV{mv}", owner.0),
                Zone::Battlefield,
            );
            let obj = state.objects.get_mut(&id).expect("object exists");
            obj.card_types.core_types.push(ty);
            obj.mana_cost = ManaCost::generic(mv);
            id
        };

        // P1 controls MV 2, 3, 5, 5 — max is 5 (two permanents tied).
        let p1_mv2 = place(&mut state, PlayerId(1), 2, CoreType::Creature);
        let p1_mv3 = place(&mut state, PlayerId(1), 3, CoreType::Planeswalker);
        let p1_mv5a = place(&mut state, PlayerId(1), 5, CoreType::Creature);
        let p1_mv5b = place(&mut state, PlayerId(1), 5, CoreType::Planeswalker);
        // P2 controls MV 1, 6 — max is 6, strictly greater than P1's max.
        let p2_mv1 = place(&mut state, PlayerId(2), 1, CoreType::Creature);
        let p2_mv6 = place(&mut state, PlayerId(2), 6, CoreType::Creature);

        // The Soul Shatter target filter: Or[Typed(Creature, You, [Cmc]),
        // Typed(Planeswalker, You, [Cmc])] where the Cmc prop carries the
        // per-controller `Aggregate { Max, ManaValue, eligible-set }`.
        let eligible_set = TargetFilter::Or {
            filters: vec![
                TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::You)),
                TargetFilter::Typed(
                    TypedFilter::default()
                        .with_type(TypeFilter::Planeswalker)
                        .controller(ControllerRef::You),
                ),
            ],
        };
        let superlative = FilterProp::Cmc {
            comparator: Comparator::EQ,
            value: QuantityExpr::Ref {
                qty: QuantityRef::Aggregate {
                    function: AggregateFunction::Max,
                    property: ObjectProperty::ManaValue,
                    filter: eligible_set,
                },
            },
        };
        let soul_shatter_target = TargetFilter::Or {
            filters: vec![
                TargetFilter::Typed(
                    TypedFilter::creature()
                        .controller(ControllerRef::You)
                        .properties(vec![superlative.clone()]),
                ),
                TargetFilter::Typed(
                    TypedFilter::default()
                        .with_type(TypeFilter::Planeswalker)
                        .controller(ControllerRef::You)
                        .properties(vec![superlative]),
                ),
            ],
        };

        // Build the per-opponent sacrifice resolution. The `player_scope`
        // loop (`effects/mod.rs`) rebinds `controller` to each opponent before
        // resolving the chain; we mirror that here by constructing the chain
        // with the opponent already bound as `controller`.
        let make_per_opponent = |opponent: PlayerId| {
            ResolvedAbility::new(
                Effect::Sacrifice {
                    target: soul_shatter_target.clone(),
                    count: QuantityExpr::Fixed { value: 1 },
                    min_count: 0,
                },
                vec![],
                ObjectId(500),
                opponent,
            )
        };

        // --- Opponent P1: max over P1's board is 5; pool = the two MV-5. ---
        let mut state_p1 = state.clone();
        let mut events = Vec::new();
        resolve_ability_chain(
            &mut state_p1,
            &make_per_opponent(PlayerId(1)),
            &mut events,
            0,
        )
        .unwrap();
        match &state_p1.waiting_for {
            WaitingFor::EffectZoneChoice { player, cards, .. } => {
                assert_eq!(*player, PlayerId(1), "P1 is the chooser");
                let mut got = cards.clone();
                got.sort_by_key(|id| id.0);
                let mut want = vec![p1_mv5a, p1_mv5b];
                want.sort_by_key(|id| id.0);
                assert_eq!(
                    got, want,
                    "P1 must be offered exactly their two MV-5 permanents \
                     (a global max=6 bug would leave P1 with an empty pool)"
                );
                assert!(!cards.contains(&p1_mv2) && !cards.contains(&p1_mv3));
                assert!(!cards.contains(&p2_mv1) && !cards.contains(&p2_mv6));
            }
            other => panic!("expected EffectZoneChoice for P1, got {other:?}"),
        }

        // --- Opponent P2: max over P2's board is 6; pool = the single MV-6. ---
        let mut state_p2 = state.clone();
        let mut events = Vec::new();
        resolve_ability_chain(
            &mut state_p2,
            &make_per_opponent(PlayerId(2)),
            &mut events,
            0,
        )
        .unwrap();
        // P2 has exactly one permanent at their max (MV-6) and one other; the
        // single-permanent fast path auto-sacrifices it, so assert it landed
        // in P2's graveyard and the MV-1 stayed on the battlefield.
        assert!(
            state_p2.players[2].graveyard.contains(&p2_mv6),
            "P2's MV-6 (their per-board max) must be the sacrificed permanent"
        );
        assert!(
            state_p2.battlefield.contains(&p2_mv1),
            "P2's MV-1 is below their per-board max and must not be sacrificed"
        );
        assert!(
            state_p2.battlefield.contains(&p1_mv5a) && state_p2.battlefield.contains(&p1_mv5b),
            "P1's permanents are untouched when P2 is the sacrificing opponent"
        );
    }

    #[test]
    fn sacrifice_greatest_power_offers_only_tied_highest_power_creatures() {
        let mut state = GameState::new_two_player(42);
        let place = |state: &mut GameState, controller: PlayerId, power: i32| {
            let id = create_object(
                state,
                CardId(state.next_object_id),
                controller,
                format!("P{} Power {power}", controller.0),
                Zone::Battlefield,
            );
            let obj = state.objects.get_mut(&id).expect("object exists");
            obj.card_types.core_types.push(CoreType::Creature);
            obj.power = Some(power);
            id
        };

        let caster_creature = place(&mut state, PlayerId(0), 9);
        let lower = place(&mut state, PlayerId(1), 2);
        let tied_a = place(&mut state, PlayerId(1), 5);
        let tied_b = place(&mut state, PlayerId(1), 5);

        let eligible_set =
            TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::ScopedPlayer));
        let greatest_power = FilterProp::PtComparison {
            stat: PtStat::Power,
            scope: PtValueScope::Current,
            comparator: Comparator::EQ,
            value: QuantityExpr::Ref {
                qty: QuantityRef::Aggregate {
                    function: AggregateFunction::Max,
                    property: ObjectProperty::Power,
                    filter: eligible_set,
                },
            },
        };
        let target = TargetFilter::Typed(
            TypedFilter::creature()
                .controller(ControllerRef::You)
                .properties(vec![greatest_power]),
        );
        let ability = ResolvedAbility::new(
            Effect::Sacrifice {
                target,
                count: QuantityExpr::Fixed { value: 1 },
                min_count: 0,
            },
            vec![],
            ObjectId(500),
            PlayerId(1),
        );
        let mut events = Vec::new();

        resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

        match &state.waiting_for {
            WaitingFor::EffectZoneChoice { player, cards, .. } => {
                assert_eq!(*player, PlayerId(1));
                let mut got = cards.clone();
                got.sort_by_key(|id| id.0);
                let mut want = vec![tied_a, tied_b];
                want.sort_by_key(|id| id.0);
                assert_eq!(got, want);
                assert!(!cards.contains(&lower));
                assert!(!cards.contains(&caster_creature));
            }
            other => panic!("expected EffectZoneChoice, got {other:?}"),
        }
    }

    /// CR 608.2c: "[Mandatory action]. If you do, [rider]." — a mandatory effect
    /// that performs its action satisfies the `IfYouDo`
    /// (`EffectOutcome { OptionalEffectPerformed }`) gate on its sibling, even
    /// though there was no "you may" decision. Regression for issue #1514: Dark
    /// Depths' "sacrifice it. If you do, create Marit Lage" never created the
    /// token because the mandatory sacrifice left `optional_effect_performed`
    /// false. Building-block test on the `Sacrifice` → `Token` chain (covers the
    /// whole mandatory-rider class, not just Dark Depths).
    #[test]
    fn mandatory_sacrifice_if_you_do_rider_fires() {
        use crate::types::ability::{AbilityCondition, PtValue, SubAbilityLink};

        let mut state = GameState::new_two_player(42);
        let victim = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Doomed Permanent".to_string(),
            Zone::Battlefield,
        );

        // "Sacrifice it. If you do, create a 1/1 token." — a mandatory sacrifice
        // with an `IfYouDo`-gated Token sibling, exactly the shape the parser
        // emits for Dark Depths' Marit Lage rider.
        let mut rider = ResolvedAbility::new(
            Effect::Token {
                name: "Test Token".to_string(),
                power: PtValue::Fixed(1),
                toughness: PtValue::Fixed(1),
                types: vec!["Creature".to_string()],
                colors: vec![],
                keywords: vec![],
                tapped: false,
                count: QuantityExpr::Fixed { value: 1 },
                owner: TargetFilter::Controller,
                attach_to: None,
                enters_attacking: false,
                supertypes: vec![],
                static_abilities: vec![],
                enter_with_counters: vec![],
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        )
        .condition(AbilityCondition::effect_performed());
        rider.sub_link = SubAbilityLink::SequentialSibling;

        let mut ability = make_sacrifice_ability(victim);
        ability.sub_ability = Some(Box::new(rider));

        let tokens_before = state.battlefield.len();
        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

        // The victim was sacrificed and the IfYouDo rider created the token.
        assert!(
            !state.battlefield.contains(&victim),
            "mandatory sacrifice must remove the victim"
        );
        let created = state
            .battlefield
            .iter()
            .filter_map(|id| state.objects.get(id))
            .any(|obj| obj.is_token && obj.name == "Test Token");
        assert!(
            created,
            "the mandatory-sacrifice IfYouDo rider must create the token \
             (battlefield went from {tokens_before} to {})",
            state.battlefield.len()
        );
    }
}
