use crate::types::ability::{
    CastingPermission, Effect, EffectError, EffectKind, PermissionGrantee, ResolvedAbility,
    TargetFilter, TargetRef,
};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;
use crate::types::identifiers::TrackedSetId;
use crate::types::player::PlayerId;

#[cfg(test)]
use crate::game::casting::{can_pay_cost_after_auto_tap, spell_objects_available_to_cast};
#[cfg(test)]
use crate::types::ability::{AbilityDefinition, AbilityKind, ManaSpendPermission, QuantityExpr};
#[cfg(test)]
use crate::types::card_type::CoreType;
#[cfg(test)]
use crate::types::identifiers::ObjectId;
#[cfg(test)]
use crate::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
#[cfg(test)]
use std::sync::Arc;

/// Grant a CastingPermission to the target object (CR 604.6).
///
/// Implements static abilities that modify where/how a card can be cast, such as
/// "You may cast this card from exile" (CR 604.6: static abilities that apply while
/// a card is in a zone you could cast it from). Building block for Airbending,
/// Foretell, Suspend, and similar "cast from exile" mechanics.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let (permission, target_filter, grantee) = match &ability.effect {
        Effect::GrantCastingPermission {
            permission,
            target,
            grantee,
        } => (permission.clone(), target, *grantee),
        _ => return Err(EffectError::MissingParam("permission".to_string())),
    };

    // CR 608.2c (issue #323 class): intrinsic permission targets (`SelfRef`
    // and tracked-set anaphora) resolve from their own filter regardless of
    // `ability.targets`. Short-circuit BEFORE the chosen-targets fallback so
    // chained grants don't inherit parent targets via
    // `effects::mod.rs::resolve_ability_chain`.
    let (target_ids, tracked_set_group): (Vec<_>, Option<TrackedSetId>) = match target_filter {
        TargetFilter::SelfRef => (vec![ability.source_id], None),
        // CR 608.2c: The `TrackedSetId(0)` sentinel binds to the highest tracked
        // set id — the set the immediately preceding effect in this chain
        // published. Empty sets are *not* skipped: an empty current set means
        // the preceding effect affected nothing, not "fall back to a stale
        // set." This deliberately differs from `targeting::latest_tracked_set_id`
        // (which skips empties for inline "from among the milled cards"
        // continuations) — see the regression test
        // `tracked_set_sentinel_does_not_reuse_prior_non_empty_set_when_current_move_is_empty`.
        TargetFilter::TrackedSet {
            id: TrackedSetId(0),
        } => state
            .tracked_object_sets
            .iter()
            .max_by_key(|(id, _)| id.0)
            .map(|(id, objects)| (objects.clone(), Some(*id)))
            .unwrap_or_default(),
        TargetFilter::TrackedSet { id } => (
            state
                .tracked_object_sets
                .get(id)
                .cloned()
                .unwrap_or_default(),
            Some(*id),
        ),
        TargetFilter::ParentTarget => {
            let ids: Vec<_> = ability
                .targets
                .iter()
                .filter_map(|target| match target {
                    TargetRef::Object(obj_id) => Some(*obj_id),
                    TargetRef::Player(_) => None,
                })
                .collect();
            // CR 608.2c: an anaphoric "it"/"this card" with no explicit target
            // slot (e.g. The Foretold Soldier — "exile it ... it becomes
            // foretold") binds to the ability's own source, which the preceding
            // chained ChangeZone left in the new zone (the engine preserves the
            // ObjectId across zone moves). Guarded on the empty case so grants
            // that carry explicit object targets (plotted / PlayFromExile) are
            // unaffected.
            if ids.is_empty() {
                (vec![ability.source_id], None)
            } else {
                (ids, None)
            }
        }
        TargetFilter::Any | TargetFilter::None if !ability.targets.is_empty() => (
            ability
                .targets
                .iter()
                .filter_map(|target| match target {
                    TargetRef::Object(obj_id) => Some(*obj_id),
                    TargetRef::Player(_) => None,
                })
                .collect(),
            None,
        ),
        TargetFilter::Any | TargetFilter::None => (vec![ability.source_id], None),
        other => {
            let ctx = crate::game::filter::FilterContext::from_ability(ability);
            (
                state
                    .objects
                    .keys()
                    .copied()
                    .filter(|obj_id| {
                        crate::game::filter::matches_target_filter(state, *obj_id, other, &ctx)
                    })
                    .collect(),
                None,
            )
        }
    };

    // CR 611.2a/b + CR 108.3: Resolve `grantee` to the `PlayerId` that a
    // `PlayFromExile` permission's `granted_to` should bind to. For
    // `ObjectOwner`, this varies per iterated object and is computed inside
    // the loop. For the other variants it is constant across iterations.
    let constant_grantee: Option<PlayerId> = match grantee {
        PermissionGrantee::AbilityController => Some(ability.controller),
        PermissionGrantee::ParentTargetController => ability
            .targets
            .iter()
            .find_map(|t| match t {
                TargetRef::Player(pid) => Some(*pid),
                TargetRef::Object(_) => None,
            })
            .or(Some(ability.controller)),
        PermissionGrantee::ObjectOwner => None, // per-iteration
    };

    for obj_id in target_ids {
        // Compute `granted_to` for this object. For `ObjectOwner` we read the
        // object's owner here so each iteration binds independently (CR 108.3).
        let granted_to_pid = constant_grantee.unwrap_or_else(|| {
            state
                .objects
                .get(&obj_id)
                .map(|o| o.owner)
                .unwrap_or(ability.controller)
        });
        // CR 702.143d: compute any effective foretell cost (printed OR granted by
        // a static such as Singing Towers of Darillium, with its derived cost)
        // BEFORE the mutable object borrow below — `foretell_cost` takes `&state`
        // and would conflict with the live `&mut obj`. Used only by the Foretold
        // branch; harmless to precompute for other permissions.
        let derived_foretell = crate::game::casting::foretell_cost(state, obj_id);
        if let Some(obj) = state.objects.get_mut(&obj_id) {
            let mut granted = permission.clone();
            if let CastingPermission::PlayFromExile {
                granted_to,
                source_id,
                exiled_by_ability_controller,
                single_use,
                single_use_group,
                ..
            } = &mut granted
            {
                *granted_to = granted_to_pid;
                *source_id = Some(ability.source_id);
                *exiled_by_ability_controller = Some(ability.controller);
                if *single_use {
                    *single_use_group = tracked_set_group;
                }
            }
            // CR 611.2a + CR 118.9: Bind `granted_to` for `ExileWithAltCost` and
            // `ExileWithAltAbilityCost` to the resolved grantee. Without this
            // step, an Airbender owned by the controller of the airbended card
            // would grant a permission whose `has_exile_cast_permission` check
            // falls back to `obj.owner == player` — which happens to coincide
            // for Airbending today but breaks the moment an
            // attack-trigger-style grant exiles cards from each opponent's
            // library (Jeleva, Nephalia's Scourge) and grants the cast
            // permission to its controller, not to each card's owner. Only
            // overwrite parser-emitted `None` placeholders so call sites that
            // computed a specific PlayerId (e.g., Discover/Cascade WaitingFor
            // continuations) keep their existing binding.
            match &mut granted {
                CastingPermission::ExileWithAltCost {
                    granted_to: granted_to @ None,
                    ..
                }
                | CastingPermission::ExileWithAltAbilityCost {
                    granted_to: granted_to @ None,
                    ..
                } => {
                    *granted_to = Some(granted_to_pid);
                }
                _ => {}
            }
            // CR 702.170a + CR 702.170d: `Plotted { turn_plotted }` is stamped
            // at grant-resolution time from `state.turn_number`, mirroring how
            // `PlayFromExile { granted_to }` is bound to a concrete `PlayerId`
            // above. Synthesized plot activations use placeholder `0`; the
            // real turn number is filled in here so the "later turn" gate in
            // `has_exile_cast_permission` reflects when the grant resolved.
            if let CastingPermission::Plotted { turn_plotted } = &mut granted {
                *turn_plotted = state.turn_number;
            }
            let plotted_for = if matches!(granted, CastingPermission::Plotted { .. }) {
                Some(granted_to_pid)
            } else {
                None
            };
            // CR 702.143d: an effect-driven "becomes foretold" designation —
            // distinct from the CR 702.143a foretell special action.
            let mut became_foretold = None;
            if let CastingPermission::Foretold {
                turn_foretold,
                cost,
            } = &mut granted
            {
                // CR 702.143d: the turn the card became foretold is stamped here
                // so the "after the turn it became foretold has ended" cast gate
                // reflects when the grant resolved (mirrors Plotted above).
                *turn_foretold = state.turn_number;
                obj.foretold = true;
                // CR 702.143a / CR 702.143e: foretold cards sit face down in exile.
                obj.face_down = true;
                // CR 702.143d: the card is castable "for any foretell cost it
                // has" — the printed Foretell keyword is the single authority
                // for that cost (shared with the foretell special action via
                // casting::foretell_cost). A parser-baked cost survives whenever
                // the object has no Foretell keyword of its own, covering the
                // CR 702.143d case where an effect grants a foretell cost to a
                // card that lacks one.
                if let Some(kw_cost) = derived_foretell.clone() {
                    *cost = kw_cost;
                }
                became_foretold = Some(obj_id);
            }
            obj.casting_permissions.push(granted);
            if let Some(player_id) = plotted_for {
                events.push(GameEvent::BecomesPlotted {
                    object_id: obj_id,
                    player_id,
                });
            }
            if let Some(object_id) = became_foretold {
                events.push(GameEvent::BecameForetold { object_id });
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
    use crate::types::ability::{CastingPermission, Duration, PlayerScope, TargetFilter};
    use crate::types::game_state::GameState;
    use crate::types::identifiers::CardId;
    use crate::types::phase::Phase;
    use crate::types::zones::Zone;

    /// CR 611.2a default: grantee defaults to the ability controller.
    #[test]
    fn grantee_ability_controller_binds_to_caster() {
        let mut state = GameState::new_two_player(1);
        let target = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Bear".to_string(),
            Zone::Exile,
        );
        let ability = ResolvedAbility::new(
            Effect::GrantCastingPermission {
                permission: CastingPermission::PlayFromExile {
                    duration: Duration::UntilNextTurnOf {
                        player: PlayerScope::Controller,
                    },
                    granted_to: PlayerId(0),
                    frequency: crate::types::statics::CastFrequency::Unlimited,
                    source_id: None,
                    exiled_by_ability_controller: None,
                    mana_spend_permission: None,
                    card_filter: None,
                    single_use_group: None,
                    single_use: false,
                    cast_cost_raise: None,
                    land_enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                },
                target: TargetFilter::Any,
                grantee: PermissionGrantee::AbilityController,
            },
            vec![TargetRef::Object(target)],
            crate::types::identifiers::ObjectId(100),
            PlayerId(0), // caster
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        let obj = &state.objects[&target];
        assert_eq!(obj.casting_permissions.len(), 1);
        match obj.casting_permissions[0] {
            CastingPermission::PlayFromExile { granted_to, .. } => {
                assert_eq!(granted_to, PlayerId(0), "granted_to should be the caster");
            }
            _ => panic!("expected PlayFromExile"),
        }
    }

    /// CR 603.7 + CR 611.2a: A tracked-set `single_use` `PlayFromExile` grant
    /// carries the tracked-set identity as its consumption group. The source id
    /// remains the ability source for provenance/once-per-turn logic.
    #[test]
    fn single_use_play_from_exile_grant_stamps_tracked_set_group() {
        let mut state = GameState::new_two_player(1);
        let source = crate::types::identifiers::ObjectId(100);
        let target = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Exiled Card".to_string(),
            Zone::Exile,
        );
        let tracked_set = TrackedSetId(7);
        state.tracked_object_sets.insert(tracked_set, vec![target]);
        let ability = ResolvedAbility::new(
            Effect::GrantCastingPermission {
                permission: CastingPermission::PlayFromExile {
                    duration: Duration::UntilEndOfNextTurnOf {
                        player: PlayerScope::Controller,
                    },
                    granted_to: PlayerId(0),
                    frequency: crate::types::statics::CastFrequency::Unlimited,
                    source_id: None,
                    exiled_by_ability_controller: None,
                    mana_spend_permission: None,
                    card_filter: None,
                    single_use_group: None,
                    single_use: true,
                    cast_cost_raise: None,
                    land_enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                },
                target: TargetFilter::TrackedSet { id: tracked_set },
                grantee: PermissionGrantee::AbilityController,
            },
            vec![],
            source,
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        match &state.objects[&target].casting_permissions[0] {
            CastingPermission::PlayFromExile {
                source_id,
                single_use_group,
                ..
            } => {
                assert_eq!(*source_id, Some(source));
                assert_eq!(*single_use_group, Some(tracked_set));
            }
            other => panic!("expected PlayFromExile, got {other:?}"),
        }
    }

    #[test]
    fn plotted_permission_stamps_turn_and_emits_event() {
        let mut state = GameState::new_two_player(1);
        state.turn_number = 7;
        let target = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Plotted Card".to_string(),
            Zone::Exile,
        );
        let ability = ResolvedAbility::new(
            Effect::GrantCastingPermission {
                permission: CastingPermission::Plotted { turn_plotted: 0 },
                target: TargetFilter::Any,
                grantee: PermissionGrantee::ObjectOwner,
            },
            vec![TargetRef::Object(target)],
            crate::types::identifiers::ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(
            state.objects[&target].casting_permissions,
            vec![CastingPermission::Plotted { turn_plotted: 7 }]
        );
        assert!(events.iter().any(|event| matches!(
            event,
            GameEvent::BecomesPlotted {
                object_id,
                player_id: PlayerId(1)
            } if *object_id == target
        )));
    }

    #[test]
    fn plotted_permission_uses_parent_target_not_source_when_targets_are_present() {
        let mut state = GameState::new_two_player(1);
        state.turn_number = 1;
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Aven Interrupter".to_string(),
            Zone::Battlefield,
        );
        let target = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Target Spell".to_string(),
            Zone::Exile,
        );
        let ability = ResolvedAbility::new(
            Effect::GrantCastingPermission {
                permission: CastingPermission::Plotted { turn_plotted: 0 },
                target: TargetFilter::ParentTarget,
                grantee: PermissionGrantee::ObjectOwner,
            },
            vec![TargetRef::Object(target)],
            source,
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(state.objects[&source].casting_permissions.is_empty());
        assert_eq!(
            state.objects[&target].casting_permissions,
            vec![CastingPermission::Plotted { turn_plotted: 1 }]
        );
    }

    /// CR 702.143d + CR 608.2c: The Foretold Soldier's "exile it ... it becomes
    /// foretold" grant. The trigger has no explicit target slot, so the
    /// `ParentTarget` grant must fall back to the ability source (the just-exiled
    /// card). At resolution the object becomes a face-down foretold card whose
    /// cost is derived from its printed Foretell keyword, the turn is stamped,
    /// and a `BecameForetold` event fires (NOT the `Foretold` special-action
    /// event — CR 702.143c).
    #[test]
    fn becomes_foretold_grant_derives_cost_and_falls_back_to_source() {
        use crate::types::keywords::Keyword;

        let mut state = GameState::new_two_player(1);
        state.turn_number = 5;
        // The exiled card with its printed Foretell {1}{G}.
        let card = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "The Foretold Soldier".to_string(),
            Zone::Exile,
        );
        let foretell_cost = ManaCost::Cost {
            shards: vec![ManaCostShard::Green],
            generic: 1,
        };
        {
            // Mirror production `create_object_from_card_face`, which populates
            // BOTH the live and copiable-base keyword sets. `foretell_cost` reads
            // the copiable base (`effective_off_zone_keywords`) for a
            // non-battlefield card, so the printed foretell must be in
            // `base_keywords`.
            let obj = state.objects.get_mut(&card).unwrap();
            obj.keywords.push(Keyword::Foretell(foretell_cost.clone()));
            obj.base_keywords
                .push(Keyword::Foretell(foretell_cost.clone()));
        }

        // Empty target list + `source_id == card` mirrors the anaphoric "it"
        // trigger after the chained ChangeZone has moved the source to exile.
        let ability = ResolvedAbility::new(
            Effect::GrantCastingPermission {
                permission: CastingPermission::Foretold {
                    cost: ManaCost::zero(),
                    turn_foretold: 0,
                },
                target: TargetFilter::ParentTarget,
                grantee: PermissionGrantee::ObjectOwner,
            },
            vec![],
            card,
            PlayerId(1),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        let obj = &state.objects[&card];
        assert!(obj.foretold, "card must become foretold");
        assert!(
            obj.face_down,
            "foretold card sits face down (CR 702.143a/e)"
        );
        assert!(
            matches!(
                obj.casting_permissions.as_slice(),
                [CastingPermission::Foretold { cost, turn_foretold }]
                    if *cost == foretell_cost && *turn_foretold == 5
            ),
            "permission must carry the printed foretell cost and the stamped turn, got {:?}",
            obj.casting_permissions
        );
        assert!(
            events.iter().any(
                |e| matches!(e, GameEvent::BecameForetold { object_id } if *object_id == card)
            ),
            "a BecameForetold event must fire"
        );
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, GameEvent::Foretold { .. })),
            "becoming foretold must NOT emit the foretell special-action event (CR 702.143c)"
        );
    }

    /// CR 108.3: `ObjectOwner` grants the permission to each iterated object's
    /// owner — powers Suspend Aggression's "its owner may play it".
    #[test]
    fn grantee_object_owner_binds_per_target_to_owner() {
        let mut state = GameState::new_two_player(1);
        let p0_card = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "MyCard".to_string(),
            Zone::Exile,
        );
        let p1_card = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "TheirCard".to_string(),
            Zone::Exile,
        );

        let ability = ResolvedAbility::new(
            Effect::GrantCastingPermission {
                permission: CastingPermission::PlayFromExile {
                    duration: Duration::UntilNextTurnOf {
                        player: PlayerScope::Controller,
                    },
                    granted_to: PlayerId(0),
                    frequency: crate::types::statics::CastFrequency::Unlimited,
                    source_id: None,
                    exiled_by_ability_controller: None,
                    mana_spend_permission: None,
                    card_filter: None,
                    single_use_group: None,
                    single_use: false,
                    cast_cost_raise: None,
                    land_enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                },
                target: TargetFilter::Any,
                grantee: PermissionGrantee::ObjectOwner,
            },
            vec![TargetRef::Object(p0_card), TargetRef::Object(p1_card)],
            crate::types::identifiers::ObjectId(100),
            PlayerId(0), // caster — NOT the grantee
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        for (obj_id, expected_owner) in [(p0_card, PlayerId(0)), (p1_card, PlayerId(1))] {
            let obj = &state.objects[&obj_id];
            assert_eq!(obj.casting_permissions.len(), 1);
            match obj.casting_permissions[0] {
                CastingPermission::PlayFromExile { granted_to, .. } => {
                    assert_eq!(
                        granted_to, expected_owner,
                        "granted_to should equal object owner"
                    );
                }
                _ => panic!("expected PlayFromExile"),
            }
        }
    }

    /// CR 109.4: `ParentTargetController` binds the grant to the first
    /// `TargetRef::Player` in the ability's targets — powers Expedited
    /// Inheritance's "its controller may ... they may play those cards".
    #[test]
    fn grantee_parent_target_controller_binds_to_player_target() {
        let mut state = GameState::new_two_player(1);
        let exiled = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Card".to_string(),
            Zone::Exile,
        );

        let ability = ResolvedAbility::new(
            Effect::GrantCastingPermission {
                permission: CastingPermission::PlayFromExile {
                    duration: Duration::UntilNextTurnOf {
                        player: PlayerScope::Controller,
                    },
                    granted_to: PlayerId(0),
                    frequency: crate::types::statics::CastFrequency::Unlimited,
                    source_id: None,
                    exiled_by_ability_controller: None,
                    mana_spend_permission: None,
                    card_filter: None,
                    single_use_group: None,
                    single_use: false,
                    cast_cost_raise: None,
                    land_enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                },
                target: TargetFilter::Any,
                grantee: PermissionGrantee::ParentTargetController,
            },
            vec![TargetRef::Player(PlayerId(1)), TargetRef::Object(exiled)],
            crate::types::identifiers::ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        let obj = &state.objects[&exiled];
        assert_eq!(obj.casting_permissions.len(), 1);
        match obj.casting_permissions[0] {
            CastingPermission::PlayFromExile { granted_to, .. } => {
                assert_eq!(
                    granted_to,
                    PlayerId(1),
                    "granted_to should equal the player target"
                );
            }
            _ => panic!("expected PlayFromExile"),
        }
    }

    // ----------------------------------------------------------------
    // Rocco, Street Chef (issue #412): Duration::UntilNextStepOf { step: End } and
    // its end-step prune lifecycle.
    // ----------------------------------------------------------------

    /// CR 513.1 + CR 611.2a: Granting `PlayFromExile { duration:
    /// UntilNextStepOf { step: End, player: Controller } }` writes the permission with the
    /// new variant onto each object owned by the iterated player. This
    /// covers the parser-output → resolver path for Rocco's first trigger.
    #[test]
    fn rocco_end_step_grant_writes_until_next_end_step_permission() {
        let mut state = GameState::new_two_player(1);
        let p0_card = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "MyCard".to_string(),
            Zone::Exile,
        );
        let p1_card = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "TheirCard".to_string(),
            Zone::Exile,
        );

        let ability = ResolvedAbility::new(
            Effect::GrantCastingPermission {
                permission: CastingPermission::PlayFromExile {
                    duration: Duration::UntilNextStepOf {
                        step: Phase::End,
                        player: PlayerScope::Controller,
                    },
                    granted_to: PlayerId(0),
                    frequency: crate::types::statics::CastFrequency::Unlimited,
                    source_id: None,
                    exiled_by_ability_controller: None,
                    mana_spend_permission: None,
                    card_filter: None,
                    single_use_group: None,
                    single_use: false,
                    cast_cost_raise: None,
                    land_enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                },
                target: TargetFilter::Any,
                grantee: PermissionGrantee::ObjectOwner,
            },
            vec![TargetRef::Object(p0_card), TargetRef::Object(p1_card)],
            crate::types::identifiers::ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        for (obj_id, expected_owner) in [(p0_card, PlayerId(0)), (p1_card, PlayerId(1))] {
            let obj = &state.objects[&obj_id];
            assert_eq!(obj.casting_permissions.len(), 1);
            match &obj.casting_permissions[0] {
                CastingPermission::PlayFromExile {
                    duration,
                    granted_to,
                    exiled_by_ability_controller,
                    ..
                } => {
                    assert_eq!(
                        duration,
                        &Duration::UntilNextStepOf {
                            step: Phase::End,
                            player: PlayerScope::Controller,
                        },
                    );
                    assert_eq!(*granted_to, expected_owner);
                    assert_eq!(*exiled_by_ability_controller, Some(PlayerId(0)));
                }
                _ => panic!("expected PlayFromExile"),
            }
        }
    }

    /// CR 513.1: `prune_end_step_casting_permissions` removes only the
    /// `UntilNextStepOf { step: End }` permissions whose `granted_to == active_player`.
    /// Permissions for other players survive (asymmetric multiplayer
    /// prune), and non-end-step durations (Permanent, UntilEndOfTurn,
    /// UntilNextTurnOf) all survive.
    #[test]
    fn end_step_prune_only_removes_matching_grantee_and_variant() {
        use crate::game::layers::prune_end_step_casting_permissions;
        use crate::types::statics::CastFrequency;

        let mut state = GameState::new_two_player(1);
        let card_a = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "A".to_string(),
            Zone::Exile,
        );
        let card_b = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "B".to_string(),
            Zone::Exile,
        );
        let card_c = create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "C".to_string(),
            Zone::Exile,
        );

        let mk_perm = |duration: Duration, granted_to: PlayerId| CastingPermission::PlayFromExile {
            duration,
            granted_to,
            frequency: CastFrequency::Unlimited,
            source_id: None,
            exiled_by_ability_controller: None,
            mana_spend_permission: None,
            card_filter: None,
            single_use_group: None,
            single_use: false,
            cast_cost_raise: None,
            land_enter_tapped: crate::types::zones::EtbTapState::Unspecified,
        };

        state.objects.get_mut(&card_a).unwrap().casting_permissions = vec![mk_perm(
            Duration::UntilNextStepOf {
                step: Phase::End,
                player: PlayerScope::Controller,
            },
            PlayerId(0),
        )];
        state.objects.get_mut(&card_b).unwrap().casting_permissions = vec![mk_perm(
            Duration::UntilNextStepOf {
                step: Phase::End,
                player: PlayerScope::Controller,
            },
            PlayerId(1),
        )];
        state.objects.get_mut(&card_c).unwrap().casting_permissions = vec![
            mk_perm(Duration::UntilEndOfTurn, PlayerId(0)),
            mk_perm(Duration::Permanent, PlayerId(0)),
            mk_perm(
                Duration::UntilNextTurnOf {
                    player: PlayerScope::Controller,
                },
                PlayerId(0),
            ),
        ];

        // Active player is 0 — only their UntilNextStepOf { step: End } permissions go.
        prune_end_step_casting_permissions(&mut state, PlayerId(0));

        assert!(
            state.objects[&card_a].casting_permissions.is_empty(),
            "P0's UntilNextStepOf {{ step: End }} grant should be pruned",
        );
        assert_eq!(
            state.objects[&card_b].casting_permissions.len(),
            1,
            "P1's UntilNextStepOf {{ step: End }} grant survives P0's end step",
        );
        assert_eq!(
            state.objects[&card_c].casting_permissions.len(),
            3,
            "non-end-step durations all survive the prune",
        );
    }

    /// CR 513.1 + CR 611.2a/b: Rocco's duration text says "your next end
    /// step", so the expiration anchor is the effect controller even when the
    /// play permission is granted to each exiled card's owner.
    #[test]
    fn rocco_end_step_prune_uses_effect_controller_not_object_owner() {
        use crate::game::layers::prune_end_step_casting_permissions;
        use crate::types::statics::CastFrequency;

        let mut state = GameState::new_two_player(1);
        let opponents_card = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "TheirCard".to_string(),
            Zone::Exile,
        );
        let permission = CastingPermission::PlayFromExile {
            duration: Duration::UntilNextStepOf {
                step: Phase::End,
                player: PlayerScope::Controller,
            },
            granted_to: PlayerId(1),
            frequency: CastFrequency::Unlimited,
            source_id: None,
            exiled_by_ability_controller: Some(PlayerId(0)),
            mana_spend_permission: None,
            card_filter: None,
            single_use_group: None,
            single_use: false,
            cast_cost_raise: None,
            land_enter_tapped: crate::types::zones::EtbTapState::Unspecified,
        };
        state
            .objects
            .get_mut(&opponents_card)
            .unwrap()
            .casting_permissions = vec![permission];

        prune_end_step_casting_permissions(&mut state, PlayerId(1));
        assert_eq!(
            state.objects[&opponents_card].casting_permissions.len(),
            1,
            "permission must survive the card owner's end step",
        );

        prune_end_step_casting_permissions(&mut state, PlayerId(0));
        assert!(
            state.objects[&opponents_card]
                .casting_permissions
                .is_empty(),
            "permission must expire at Rocco controller's next end step",
        );
    }

    /// CR 513.2 ordering regression: a new `UntilNextStepOf { step: End }` permission
    /// granted by an end-step trigger DURING this end step (after the
    /// prune has already run) MUST survive — CR 513.2 prevents the end
    /// step from "backing up" so the new trigger lands after the prune.
    /// `turns.rs::auto_advance::Phase::End` runs the prune BEFORE
    /// `process_phase_triggers`, so grants from those triggers are NOT
    /// wiped by the same end step.
    #[test]
    fn end_step_prune_runs_before_triggers_so_new_grants_survive() {
        use crate::game::layers::prune_end_step_casting_permissions;
        use crate::types::statics::CastFrequency;

        let mut state = GameState::new_two_player(1);
        let exiled_pre_prune = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Old".to_string(),
            Zone::Exile,
        );
        // Pre-existing grant from a PREVIOUS turn's end-step trigger.
        state
            .objects
            .get_mut(&exiled_pre_prune)
            .unwrap()
            .casting_permissions = vec![CastingPermission::PlayFromExile {
            duration: Duration::UntilNextStepOf {
                step: Phase::End,
                player: PlayerScope::Controller,
            },
            granted_to: PlayerId(0),
            frequency: CastFrequency::Unlimited,
            source_id: None,
            exiled_by_ability_controller: None,
            mana_spend_permission: None,
            card_filter: None,
            single_use_group: None,
            single_use: false,
            cast_cost_raise: None,
            land_enter_tapped: crate::types::zones::EtbTapState::Unspecified,
        }];

        // Simulate the ordering in `turns.rs`:
        // 1. Prune runs first.
        prune_end_step_casting_permissions(&mut state, PlayerId(0));
        // After prune the previous grant is gone.
        assert!(state.objects[&exiled_pre_prune]
            .casting_permissions
            .is_empty());

        // 2. End-step trigger fires AFTER the prune (CR 513.2 — no back-up).
        //    Simulate the trigger by resolving a fresh grant on a new exiled card.
        let exiled_this_step = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "New".to_string(),
            Zone::Exile,
        );
        let ability = ResolvedAbility::new(
            Effect::GrantCastingPermission {
                permission: CastingPermission::PlayFromExile {
                    duration: Duration::UntilNextStepOf {
                        step: Phase::End,
                        player: PlayerScope::Controller,
                    },
                    granted_to: PlayerId(0),
                    frequency: CastFrequency::Unlimited,
                    source_id: None,
                    exiled_by_ability_controller: None,
                    mana_spend_permission: None,
                    card_filter: None,
                    single_use_group: None,
                    single_use: false,
                    cast_cost_raise: None,
                    land_enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                },
                target: TargetFilter::Any,
                grantee: PermissionGrantee::ObjectOwner,
            },
            vec![TargetRef::Object(exiled_this_step)],
            crate::types::identifiers::ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        // The new grant survives the same end step's prune (CR 513.2).
        assert_eq!(
            state.objects[&exiled_this_step].casting_permissions.len(),
            1,
            "new same-step grant must survive (CR 513.2)",
        );
        match &state.objects[&exiled_this_step].casting_permissions[0] {
            CastingPermission::PlayFromExile {
                duration,
                granted_to,
                ..
            } => {
                assert_eq!(
                    duration,
                    &Duration::UntilNextStepOf {
                        step: Phase::End,
                        player: PlayerScope::Controller,
                    },
                );
                assert_eq!(*granted_to, PlayerId(0));
            }
            _ => panic!("expected PlayFromExile"),
        }
    }

    /// CR 514.2: `prune_end_of_turn_casting_permissions` at cleanup must
    /// NOT touch `UntilNextStepOf { step: End }` permissions — that variant is
    /// pruned at the next end step instead. Defensive arm in the
    /// cleanup prune match.
    #[test]
    fn cleanup_prune_retains_until_next_end_step_permissions() {
        use crate::game::layers::prune_end_of_turn_casting_permissions;
        use crate::types::statics::CastFrequency;

        let mut state = GameState::new_two_player(1);
        let card = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "X".to_string(),
            Zone::Exile,
        );
        state.objects.get_mut(&card).unwrap().casting_permissions =
            vec![CastingPermission::PlayFromExile {
                duration: Duration::UntilNextStepOf {
                    step: Phase::End,
                    player: PlayerScope::Controller,
                },
                granted_to: PlayerId(0),
                frequency: CastFrequency::Unlimited,
                source_id: None,
                exiled_by_ability_controller: None,
                mana_spend_permission: None,
                card_filter: None,
                single_use_group: None,
                single_use: false,
                cast_cost_raise: None,
                land_enter_tapped: crate::types::zones::EtbTapState::Unspecified,
            }];

        prune_end_of_turn_casting_permissions(&mut state);

        assert_eq!(
            state.objects[&card].casting_permissions.len(),
            1,
            "UntilNextStepOf {{ step: End }} must survive the cleanup prune",
        );
    }

    /// CR 502.3: `prune_until_next_turn_casting_permissions` at the
    /// untap step must NOT touch `UntilNextStepOf { step: End }` permissions either.
    #[test]
    fn untap_prune_retains_until_next_end_step_permissions() {
        use crate::game::layers::prune_until_next_turn_casting_permissions;
        use crate::types::statics::CastFrequency;

        let mut state = GameState::new_two_player(1);
        let card = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "X".to_string(),
            Zone::Exile,
        );
        state.objects.get_mut(&card).unwrap().casting_permissions =
            vec![CastingPermission::PlayFromExile {
                duration: Duration::UntilNextStepOf {
                    step: Phase::End,
                    player: PlayerScope::Controller,
                },
                granted_to: PlayerId(0),
                frequency: CastFrequency::Unlimited,
                source_id: None,
                exiled_by_ability_controller: None,
                mana_spend_permission: None,
                card_filter: None,
                single_use_group: None,
                single_use: false,
                cast_cost_raise: None,
                land_enter_tapped: crate::types::zones::EtbTapState::Unspecified,
            }];

        prune_until_next_turn_casting_permissions(&mut state, PlayerId(0));

        assert_eq!(
            state.objects[&card].casting_permissions.len(),
            1,
            "UntilNextStepOf {{ step: End }} must survive the untap-step prune",
        );
    }

    /// Issue #1200 — Gonti, Night Minister: the third-person tracked-set grant
    /// with `AnyTypeOrColor` must let the parent player target pay off-color mana
    /// to cast the exiled spell.
    #[test]
    fn parent_target_controller_any_mana_allows_off_color_cast_payment() {
        let mut state = GameState::new_two_player(1);
        let exiled = create_object(
            &mut state,
            CardId(1200),
            PlayerId(1),
            "Borrowed Blue Spell".to_string(),
            Zone::Exile,
        );
        state
            .tracked_object_sets
            .insert(TrackedSetId(1200), vec![exiled]);
        {
            let obj = state.objects.get_mut(&exiled).unwrap();
            obj.card_types.core_types.push(CoreType::Sorcery);
            Arc::make_mut(&mut obj.abilities).push(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Draw {
                    count: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::Controller,
                },
            ));
            obj.mana_cost = ManaCost::Cost {
                shards: vec![ManaCostShard::Blue],
                generic: 0,
            };
        }

        let ability = ResolvedAbility::new(
            Effect::GrantCastingPermission {
                permission: CastingPermission::PlayFromExile {
                    duration: Duration::Permanent,
                    granted_to: PlayerId(0),
                    frequency: crate::types::statics::CastFrequency::Unlimited,
                    source_id: None,
                    exiled_by_ability_controller: None,
                    mana_spend_permission: Some(ManaSpendPermission::AnyTypeOrColor),
                    card_filter: None,
                    single_use_group: None,
                    single_use: false,
                    cast_cost_raise: None,
                    land_enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                },
                target: TargetFilter::TrackedSet {
                    id: TrackedSetId(0),
                },
                grantee: PermissionGrantee::ParentTargetController,
            },
            vec![TargetRef::Player(PlayerId(1))],
            crate::types::identifiers::ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        state.players[1].mana_pool.add(ManaUnit::new(
            ManaType::Red,
            ObjectId(0),
            false,
            Vec::new(),
        ));

        assert!(
            spell_objects_available_to_cast(&state, PlayerId(1)).contains(&exiled),
            "parent target should see the exiled spell as castable"
        );
        assert!(
            can_pay_cost_after_auto_tap(
                &state,
                PlayerId(1),
                exiled,
                &state.objects[&exiled].mana_cost
            ),
            "AnyTypeOrColor should let red mana pay for a blue spell"
        );
    }
}
