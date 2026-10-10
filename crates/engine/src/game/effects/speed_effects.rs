use crate::game::quantity::resolve_quantity_with_targets;
use crate::game::speed::{decrease_speed, increase_speed, set_speed};
use crate::types::ability::{
    Effect, EffectError, PlayerFilter, ResolvedAbility, SpeedDelta, TargetFilter, TargetRef,
};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;
use crate::types::player::PlayerId;

pub(crate) fn players_for_filter(
    state: &GameState,
    filter: &PlayerFilter,
    ability: &ResolvedAbility,
) -> Vec<PlayerId> {
    let controller = ability.controller;
    let source_id = ability.source_id;
    match filter {
        PlayerFilter::Controller => vec![controller],
        PlayerFilter::Opponent => state
            .players
            .iter()
            .filter(|player| !player.is_eliminated && player.id != controller)
            .map(|player| player.id)
            .collect(),
        PlayerFilter::DefendingPlayer => {
            crate::game::targeting::resolve_event_context_target_for_event_or_state(
                state,
                &crate::types::ability::TargetFilter::DefendingPlayer,
                source_id,
                state.current_trigger_event.as_ref(),
            )
            .and_then(|target| match target {
                crate::types::ability::TargetRef::Player(player) => Some(player),
                _ => None,
            })
            .into_iter()
            .collect()
        }
        PlayerFilter::OpponentLostLife => state
            .players
            .iter()
            .filter(|player| !player.is_eliminated)
            .filter(|player| player.id != controller && player.life_lost_this_turn > 0)
            .map(|player| player.id)
            .collect(),
        PlayerFilter::OpponentGainedLife => state
            .players
            .iter()
            .filter(|player| !player.is_eliminated)
            .filter(|player| player.id != controller && player.life_gained_this_turn > 0)
            .map(|player| player.id)
            .collect(),
        // CR 104.5 / CR 800.4: Players who lost have left the game; this
        // filter is quantity-only and has no live speed-effect recipient.
        PlayerFilter::HasLostTheGame => Vec::new(),
        // CR 506.2 + CR 508.6: Count-only filter (Suppressor Skyguard); it has
        // no live speed-effect recipient meaning.
        PlayerFilter::OpponentOfTriggeringPlayerNotAttacked => Vec::new(),
        // CR 120.1 + CR 510.1 + CR 120.9 + CR 608.2i + CR 120.2a/120.2b: Each
        // opponent who was dealt damage of the given kind this turn, optionally
        // restricted to a matching source.
        PlayerFilter::OpponentDealtDamage {
            kind,
            source,
            min_sources,
        } => state
            .players
            .iter()
            .filter(|player| !player.is_eliminated)
            .filter(|player| {
                crate::game::quantity::opponent_dealt_damage_matches(
                    state,
                    player.id,
                    controller,
                    *kind,
                    source,
                    *min_sources,
                    source_id,
                )
            })
            .map(|player| player.id)
            .collect(),
        // CR 508.6: each opponent the subject attacked within scope.
        PlayerFilter::OpponentAttacked { subject, scope } => state
            .players
            .iter()
            .filter(|player| !player.is_eliminated)
            .filter(|player| {
                player.id != controller
                    && state.opponent_attacked(*subject, *scope, controller, source_id, player.id)
            })
            .map(|player| player.id)
            .collect(),
        // CR 508.6 + CR 102.2 + CR 102.3: each opponent of the controller who is
        // attacking the enchanted player (the Commander 2017 "each opponent
        // attacking that player does the same" curse rider). Delegates to the
        // single scope authority (live, kind-preserving, team-aware).
        PlayerFilter::OpponentAttackingEnchantedPlayer => state
            .players
            .iter()
            .filter(|player| !player.is_eliminated)
            .filter(|player| {
                crate::game::effects::matches_player_scope(
                    state, player.id, filter, controller, source_id,
                )
            })
            .map(|player| player.id)
            .collect(),
        PlayerFilter::All => state
            .players
            .iter()
            .filter(|player| !player.is_eliminated)
            .map(|player| player.id)
            .collect(),
        // CR 608.2c + CR 109.4 + CR 608.2h: every non-eliminated player except
        // the anchor's set. The ability-aware path: the `exclude` anchor is
        // resolved recursively through this same function so an
        // ability-target-dependent anchor (ParentObjectTargetController) reads
        // `ability.targets` / last-known info, which the generic
        // `matches_player_scope` predicate cannot. This is the authoritative
        // resolver for `AllExcept` effect-iteration (see the player_scope driver
        // routing in `effects::mod.rs`).
        PlayerFilter::AllExcept { exclude }
            if matches!(exclude.as_ref(), PlayerFilter::ParentObjectTargetOwner) =>
        {
            players_except_required_owner_anchor(state, ability)
        }
        PlayerFilter::AllExcept { exclude } => {
            let excluded = players_for_filter(state, exclude, ability);
            state
                .players
                .iter()
                .filter(|player| !player.is_eliminated && !excluded.contains(&player.id))
                .map(|player| player.id)
                .collect()
        }
        PlayerFilter::HighestSpeed => {
            let highest_speed = state
                .players
                .iter()
                .filter(|player| !player.is_eliminated)
                .map(|player| crate::game::speed::effective_speed(state, player.id))
                .max()
                .unwrap_or(0);
            state
                .players
                .iter()
                .filter(|player| !player.is_eliminated)
                .filter(|player| {
                    crate::game::speed::effective_speed(state, player.id) == highest_speed
                })
                .map(|player| player.id)
                .collect()
        }
        PlayerFilter::ZoneChangedThisWay => state
            .players
            .iter()
            .filter(|player| !player.is_eliminated)
            .filter(|player| {
                state.last_zone_changed_ids.iter().any(|id| {
                    state
                        .objects
                        .get(id)
                        .is_some_and(|obj| obj.owner == player.id)
                })
            })
            .map(|player| player.id)
            .collect(),
        PlayerFilter::PerformedActionThisWay { relation, action } => state
            .players
            .iter()
            .filter(|player| !player.is_eliminated)
            .filter(|player| {
                crate::game::players::matches_relation(state, player.id, controller, *relation)
                    && crate::game::players::performed_action_this_way(state, player.id, *action)
            })
            .map(|player| player.id)
            .collect(),
        PlayerFilter::OwnersOfCardsExiledBySource => state
            .players
            .iter()
            .filter(|player| !player.is_eliminated)
            .filter(|player| {
                crate::game::players::owns_card_exiled_by_source(state, player.id, source_id)
            })
            .map(|player| player.id)
            .collect(),
        PlayerFilter::TriggeringPlayer => state
            .current_trigger_event
            .as_ref()
            .and_then(|e| crate::game::targeting::extract_player_from_event(e, state))
            .into_iter()
            .collect(),
        // CR 120.3 + CR 603.2c: Each opponent other than the triggering opponent.
        // Falls back to plain Opponent semantics when no trigger event is in scope.
        PlayerFilter::OpponentOtherThanTriggering => {
            let triggering = state
                .current_trigger_event
                .as_ref()
                .and_then(|e| crate::game::targeting::extract_player_from_event(e, state));
            state
                .players
                .iter()
                .filter(|player| {
                    !player.is_eliminated
                        && crate::game::players::is_opponent(state, controller, player.id)
                })
                .filter(|player| triggering.is_none_or(|pid| pid != player.id))
                .map(|player| player.id)
                .collect()
        }
        // CR 102.2 + CR 102.3 + CR 603.2: Each opponent of the triggering
        // (casting) player, CR 102.3-aware via `players::is_opponent` (teammates
        // in 2HG are not opponents). Fail closed (empty) when no trigger event
        // anchors the caster.
        PlayerFilter::OpponentOfTriggeringPlayer => {
            let caster = state
                .current_trigger_event
                .as_ref()
                .and_then(|e| crate::game::targeting::extract_player_from_event(e, state));
            state
                .players
                .iter()
                .filter(|player| {
                    !player.is_eliminated
                        && caster
                            .is_some_and(|c| crate::game::players::is_opponent(state, c, player.id))
                })
                .map(|player| player.id)
                .collect()
        }
        // CR 608.2c + CR 701.38: Players who cast a vote for the recorded
        // choice index in the most recent vote within the current top-level
        // resolution. Read directly off the transient ballot ledger.
        PlayerFilter::VotedFor { choice_index } => state
            .players
            .iter()
            .filter(|player| !player.is_eliminated)
            .filter(|player| {
                state
                    .last_vote_ballots
                    .iter()
                    .any(|(voter, idx)| *voter == player.id && *idx == *choice_index)
            })
            .map(|player| player.id)
            .collect(),
        // CR 109.4 + CR 608.2c: the controller of the first object target of
        // the resolving ability ("reduce that opponent's speed", anaphoring
        // the controller of a bounced creature).
        PlayerFilter::ParentObjectTargetController => {
            crate::game::ability_utils::parent_target_controller(ability, state)
                .filter(|pid| {
                    state
                        .players
                        .iter()
                        .any(|player| player.id == *pid && !player.is_eliminated)
                })
                .into_iter()
                .collect()
        }
        // CR 108.3 + CR 109.4: the owner of the first object target — owner-axis
        // sibling of `ParentObjectTargetController`.
        PlayerFilter::ParentObjectTargetOwner => parent_object_target_owner_player(state, ability)
            .into_iter()
            .collect(),
        // CR 201.5a: an unlatched caster names nobody.
        PlayerFilter::GrantingObjectCaster => Vec::new(),
        // CR 608.2c + CR 109.4: the resolution-scoped chosen player at `index`.
        PlayerFilter::ChosenPlayer { index } => ability
            .chosen_players
            .get(*index as usize)
            .copied()
            .filter(|pid| {
                state
                    .players
                    .iter()
                    .any(|player| player.id == *pid && !player.is_eliminated)
            })
            .into_iter()
            .collect(),
        // CR 109.4 + CR 109.5: "each [player class] who controls [comparator]
        // [count] [filter]" — candidates satisfying both `relation` and the
        // controlled-permanent count comparison.
        PlayerFilter::ControlsCount {
            relation,
            filter,
            comparator,
            count,
        } => {
            let threshold =
                crate::game::quantity::resolve_quantity(state, count, controller, source_id);
            state
                .players
                .iter()
                .filter(|player| !player.is_eliminated)
                .filter(|player| {
                    crate::game::players::matches_relation(state, player.id, controller, *relation)
                        && crate::game::effects::player_control_count_compares(
                            state,
                            player.id,
                            filter,
                            *comparator,
                            threshold,
                            source_id,
                        )
                })
                .map(|player| player.id)
                .collect()
        }
        // CR 402.1 / 119.1 / 122.1f / 404.1: "each [player class] whose [scalar
        // attr] [comparator] [value]" — candidates satisfying both `relation`
        // and the per-candidate scalar comparison. `attr` is read for each
        // candidate; `value` keeps the ability controller and binds
        // `scoped_player` to that candidate.
        PlayerFilter::PlayerAttribute {
            relation,
            attr,
            comparator,
            value,
        } => state
            .players
            .iter()
            .filter(|player| !player.is_eliminated)
            .filter(|player| {
                crate::game::players::matches_relation(state, player.id, controller, *relation) && {
                    let threshold = crate::game::quantity::resolve_quantity_with_ctx(
                        state,
                        value,
                        controller,
                        crate::game::quantity::QuantityContext {
                            scoped_player: Some(player.id),
                            ..crate::game::quantity::QuantityContext::new(source_id)
                        },
                    );
                    crate::game::effects::candidate_player_scalar_with_state(
                        state, player, controller, attr,
                    )
                    .is_some_and(|lhs| comparator.evaluate(lhs, threshold))
                }
            })
            .map(|player| player.id)
            .collect(),
        // CR 608.2c + CR 608.2h + CR 109.4: "each [player class] who
        // controlled/owned a [filter] this way" — candidates satisfying both
        // `relation` and possession of a member of the most recent tracked
        // object set. Delegates to the shared possession authority.
        PlayerFilter::TrackedSetPossessor {
            relation,
            possession,
            filter,
            caused_by,
        } => state
            .players
            .iter()
            .filter(|player| !player.is_eliminated)
            .filter(|player| {
                crate::game::players::matches_relation(state, player.id, controller, *relation)
                    && crate::game::quantity::possessed_tracked_set_member(
                        state,
                        player.id,
                        *possession,
                        filter,
                        *caused_by,
                        controller,
                        source_id,
                    )
            })
            .map(|player| player.id)
            .collect(),
    }
}

fn live_player(state: &GameState, player_id: PlayerId) -> Option<PlayerId> {
    state
        .players
        .iter()
        .any(|player| player.id == player_id && !player.is_eliminated)
        .then_some(player_id)
}

fn parent_object_target_owner_player(
    state: &GameState,
    ability: &ResolvedAbility,
) -> Option<PlayerId> {
    parent_object_target_owner_anchor(state, ability).and_then(|player| live_player(state, player))
}

fn parent_object_target_owner_anchor(
    state: &GameState,
    ability: &ResolvedAbility,
) -> Option<PlayerId> {
    crate::game::ability_utils::parent_target_owner(ability, state).or_else(|| {
        crate::game::targeting::resolve_event_context_target(
            state,
            &TargetFilter::ParentTargetOwner,
            ability.source_id,
        )
        .and_then(|target| match target {
            TargetRef::Player(player) => Some(player),
            TargetRef::Object(object_id) => state.objects.get(&object_id).map(|obj| obj.owner),
        })
    })
}

fn players_except_required_owner_anchor(
    state: &GameState,
    ability: &ResolvedAbility,
) -> Vec<PlayerId> {
    // CR 108.3 + CR 800.4a: "each other player" excludes the object's owner
    // even if that owner has left the game before this still-controlled trigger
    // resolves; only recipients are filtered to live players below.
    let Some(owner) = parent_object_target_owner_anchor(state, ability) else {
        return Vec::new();
    };
    state
        .players
        .iter()
        .filter(|player| !player.is_eliminated && player.id != owner)
        .map(|player| player.id)
        .collect()
}

/// CR 702.179a: Effects that instruct players to start their engines set speed to 1
/// only if the player currently has no speed.
pub fn resolve_start(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let Effect::StartYourEngines { player_scope } = &ability.effect else {
        return Err(EffectError::InvalidParam(
            "expected StartYourEngines".to_string(),
        ));
    };

    for player_id in players_for_filter(state, player_scope, ability) {
        let has_no_speed = state
            .players
            .iter()
            .find(|player| player.id == player_id)
            .is_some_and(|player| player.speed.is_none());
        if has_no_speed {
            set_speed(state, player_id, Some(1), events);
        }
    }

    Ok(())
}

/// CR 702.179c-d: Change speed by the resolved amount in `direction` for each
/// selected player. `Decrease` honors an optional card-text-derived `floor`.
pub fn resolve_change_speed(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let Effect::ChangeSpeed {
        player_scope,
        amount,
        direction,
        floor,
    } = &ability.effect
    else {
        return Err(EffectError::InvalidParam(
            "expected ChangeSpeed".to_string(),
        ));
    };

    let amount = resolve_quantity_with_targets(state, amount, ability);
    let amount = u8::try_from(amount.max(0)).unwrap_or(u8::MAX);
    if amount == 0 {
        return Ok(());
    }

    for player_id in players_for_filter(state, player_scope, ability) {
        match direction {
            SpeedDelta::Increase => increase_speed(state, player_id, amount, events),
            SpeedDelta::Decrease => decrease_speed(state, player_id, amount, *floor, events),
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ability::{
        Comparator, PlayerRelation, PlayerScope, QuantityExpr, QuantityRef, RoundingMode, TargetRef,
    };
    use crate::types::events::GameEvent;
    use crate::types::format::FormatConfig;
    use crate::types::game_state::ZoneChangeRecord;
    use crate::types::identifiers::ObjectId;
    use crate::types::zones::Zone;

    #[test]
    fn player_attribute_starting_life_threshold_binds_each_speed_recipient() {
        // CR 103.4 + CR 904.5 + CR 119.1: P1 has a 40-life baseline;
        // both hero seats have a 20-life baseline.
        let mut format = FormatConfig::archenemy();
        format.archenemy_player = Some(PlayerId(1));
        let mut state = GameState::new(format, 3, 42);
        state.players[1].life = 15;
        state.players[2].life = 15;
        let filter = PlayerFilter::PlayerAttribute {
            relation: PlayerRelation::Opponent,
            attr: Box::new(QuantityRef::LifeTotal {
                player: PlayerScope::ScopedPlayer,
            }),
            comparator: Comparator::LT,
            value: Box::new(QuantityExpr::DivideRounded {
                inner: Box::new(QuantityExpr::Ref {
                    qty: QuantityRef::StartingLifeTotal {
                        player: PlayerScope::ScopedPlayer,
                    },
                }),
                divisor: 2,
                rounding: RoundingMode::Down,
            }),
        };
        let ability = ResolvedAbility::new(
            Effect::StartYourEngines {
                player_scope: filter.clone(),
            },
            Vec::<TargetRef>::new(),
            ObjectId(900),
            PlayerId(0),
        );

        let mut events = Vec::new();
        resolve_start(&mut state, &ability, &mut events).unwrap();
        assert_eq!(state.players[1].speed, Some(1));
        assert_eq!(state.players[2].speed, None);
    }

    /// CR 119.1 + CR 810.9a: `players_for_filter` with a `PlayerAttribute`
    /// life-total predicate reads each candidate's TEAM total through the
    /// migrated `candidate_player_scalar_with_state` call (Site 6). Controller
    /// P0 (team A); opposing team B {2,3} has members at 5 and 6 (team 11).
    /// A `LifeTotal >= 10` opponent predicate counts BOTH team-B members
    /// because each reads the team total (11 >= 10), even though neither
    /// individual reaches 10. Reverting Site 6 (stateless `Some(p.life)`)
    /// reads 5 and 6 individually and the filter selects NOBODY.
    #[test]
    fn player_attribute_life_total_reads_team_total_in_2hg() {
        let mut state = GameState::new(FormatConfig::two_headed_giant(), 4, 0);
        state.players[2].life = 5;
        state.players[3].life = 6; // team B total = 11

        let filter = PlayerFilter::PlayerAttribute {
            relation: PlayerRelation::Opponent,
            attr: Box::new(QuantityRef::LifeTotal {
                player: PlayerScope::ScopedPlayer,
            }),
            comparator: Comparator::GE,
            value: Box::new(QuantityExpr::Fixed { value: 10 }),
        };
        let ability = ResolvedAbility::new(
            Effect::StartYourEngines {
                player_scope: PlayerFilter::Controller,
            },
            Vec::<TargetRef>::new(),
            ObjectId(0),
            PlayerId(0),
        );

        let mut selected = players_for_filter(&state, &filter, &ability);
        selected.sort_by_key(|p| p.0);
        assert_eq!(
            selected,
            vec![PlayerId(2), PlayerId(3)],
            "both team-B members count: each reads the team total (11 >= 10)"
        );

        // Sibling: raise the threshold above the team total → neither counts.
        let filter_high = PlayerFilter::PlayerAttribute {
            relation: PlayerRelation::Opponent,
            attr: Box::new(QuantityRef::LifeTotal {
                player: PlayerScope::ScopedPlayer,
            }),
            comparator: Comparator::GE,
            value: Box::new(QuantityExpr::Fixed { value: 12 }),
        };
        assert!(players_for_filter(&state, &filter_high, &ability).is_empty());
    }

    /// CR 608.2c + CR 109.4 + CR 608.2h: `players_for_filter` with
    /// `AllExcept { ParentObjectTargetController }` returns every non-eliminated
    /// player EXCEPT the controller of the ability's first object target. With a
    /// 3-player state and a target object controlled by P1, the result is
    /// {P0, P2} — the exclusion anchor reads `ability.targets` (which the generic
    /// `matches_player_scope` predicate cannot), proving the ability-aware path.
    #[test]
    fn all_except_parent_target_controller_excludes_target_controller() {
        use crate::game::zones::create_object;
        use crate::types::identifiers::CardId;
        use crate::types::zones::Zone;

        let mut state = GameState::new(FormatConfig::standard(), 3, 0);
        // Object owned and controlled by P1 — the parent target.
        let target = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Targeted Permanent".to_string(),
            Zone::Battlefield,
        );

        let filter = PlayerFilter::AllExcept {
            exclude: Box::new(PlayerFilter::ParentObjectTargetController),
        };
        let ability = ResolvedAbility::new(
            Effect::StartYourEngines {
                player_scope: PlayerFilter::Controller,
            },
            vec![TargetRef::Object(target)],
            ObjectId(0),
            PlayerId(0),
        );

        let mut selected = players_for_filter(&state, &filter, &ability);
        selected.sort_by_key(|p| p.0);
        assert_eq!(
            selected,
            vec![PlayerId(0), PlayerId(2)],
            "AllExcept excludes the parent target's controller (P1)"
        );
    }

    /// CR 108.3 + CR 603.10a + CR 608.2c + CR 608.2h: owner-relative
    /// `AllExcept { ParentObjectTargetOwner }` must exclude the departed
    /// object's owner from the zone-change record, not the trigger controller.
    #[test]
    fn all_except_parent_target_owner_excludes_zone_change_owner() {
        let mut state = GameState::new(FormatConfig::standard(), 3, 0);
        let moved = ObjectId(7);
        state.current_trigger_event = Some(GameEvent::ZoneChanged {
            object_id: moved,
            from: Some(Zone::Battlefield),
            to: Zone::Graveyard,
            record: Box::new(ZoneChangeRecord {
                owner: PlayerId(0),
                arrival: crate::types::game_state::ArrivalIdentity {
                    owner: PlayerId(0),
                    controller: PlayerId(1),
                },
                controller: PlayerId(1),
                ..ZoneChangeRecord::test_minimal(moved, Some(Zone::Battlefield), Zone::Graveyard)
            }),
        });

        let filter = PlayerFilter::AllExcept {
            exclude: Box::new(PlayerFilter::ParentObjectTargetOwner),
        };
        let ability = ResolvedAbility::new(
            Effect::StartYourEngines {
                player_scope: PlayerFilter::Controller,
            },
            Vec::<TargetRef>::new(),
            ObjectId(900),
            PlayerId(1),
        );

        let mut selected = players_for_filter(&state, &filter, &ability);
        selected.sort_by_key(|p| p.0);
        assert_eq!(
            selected,
            vec![PlayerId(1), PlayerId(2)],
            "owner P0 is excluded; controller P1 and third player P2 remain"
        );
    }

    /// CR 108.3 + CR 608.2c: `AllExcept { ParentObjectTargetOwner }` is an
    /// owner-required composite. With no target or event owner anchor, it fails
    /// closed instead of treating the excluded set as empty and selecting all.
    #[test]
    fn all_except_parent_target_owner_without_anchor_returns_no_players() {
        let state = GameState::new(FormatConfig::standard(), 3, 0);
        let filter = PlayerFilter::AllExcept {
            exclude: Box::new(PlayerFilter::ParentObjectTargetOwner),
        };
        let ability = ResolvedAbility::new(
            Effect::StartYourEngines {
                player_scope: PlayerFilter::Controller,
            },
            Vec::<TargetRef>::new(),
            ObjectId(900),
            PlayerId(1),
        );

        assert!(
            players_for_filter(&state, &filter, &ability).is_empty(),
            "missing owner anchor must not leak to every player"
        );
    }

    /// Curse-rider fixture: a curse controlled by P0 enchanting `enchanted`, and
    /// one creature per `attacking` player attacking `enchanted`, with the
    /// declaration ledger written exactly as declaration writes it. Returns the
    /// state, the curse, and the attackers in `attacking` order.
    fn curse_attack_fixture(
        format: FormatConfig,
        enchanted: PlayerId,
        attacking: &[PlayerId],
    ) -> (GameState, ObjectId, Vec<ObjectId>) {
        use crate::game::combat::{AttackerInfo, CombatState};
        use crate::game::game_object::AttachTarget;
        use crate::game::zones::create_object;
        use crate::types::card_type::CoreType;
        use crate::types::identifiers::CardId;

        let mut state = GameState::new(format, 4, 0);
        let curse = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Curse".to_string(),
            Zone::Battlefield,
        );
        state.objects.get_mut(&curse).unwrap().attached_to = Some(AttachTarget::Player(enchanted));

        let attackers: Vec<ObjectId> = attacking
            .iter()
            .map(|&player| {
                let card_id = CardId(state.next_object_id);
                let id = create_object(
                    &mut state,
                    card_id,
                    player,
                    "Attacker".to_string(),
                    Zone::Battlefield,
                );
                state
                    .objects
                    .get_mut(&id)
                    .unwrap()
                    .card_types
                    .core_types
                    .push(CoreType::Creature);
                id
            })
            .collect();

        let mut combat = CombatState {
            attackers: attackers
                .iter()
                .map(|&id| AttackerInfo::attacking_player(id, enchanted))
                .collect(),
            ..CombatState::default()
        };
        for &player in attacking {
            combat
                .attacked_defenders_this_combat
                .entry(player)
                .or_default()
                .insert(enchanted);
        }
        state.combat = Some(combat);
        (state, curse, attackers)
    }

    fn start_engines_for_attackers_of_enchanted(curse: ObjectId) -> ResolvedAbility {
        ResolvedAbility::new(
            Effect::StartYourEngines {
                player_scope: PlayerFilter::OpponentAttackingEnchantedPlayer,
            },
            Vec::<TargetRef>::new(),
            curse,
            PlayerId(0),
        )
    }

    /// CR 508.6 + CR 506.4: `players_for_filter` reads LIVE combat membership for
    /// `OpponentAttackingEnchantedPlayer`. P2 and P3 both attack the enchanted
    /// P1; P2's attacker is then removed from combat, which prunes the attacker
    /// but not the declaration ledger. Only P3 is still attacking P1. Reverting
    /// to the ledger read starts P2's engines too.
    #[test]
    fn opponent_attacking_enchanted_player_drops_attacker_removed_from_combat() {
        let (mut state, curse, attackers) = curse_attack_fixture(
            FormatConfig::standard(),
            PlayerId(1),
            &[PlayerId(2), PlayerId(3)],
        );
        crate::game::effects::remove_from_combat::remove_object_from_combat(
            &mut state,
            attackers[0],
        );
        assert!(
            state.player_attacked_player_this_combat(PlayerId(2), PlayerId(1)),
            "precondition: the declaration ledger still records P2 attacking P1"
        );

        let ability = start_engines_for_attackers_of_enchanted(curse);
        let mut events = Vec::new();
        resolve_start(&mut state, &ability, &mut events).unwrap();

        assert_eq!(
            state.players[2].speed, None,
            "P2's attacker was removed from combat: P2 is not attacking P1"
        );
        assert_eq!(
            state.players[3].speed,
            Some(1),
            "P3 is still attacking the enchanted player"
        );
        assert_eq!(
            state.players[0].speed, None,
            "the controller is not an opponent"
        );
        assert_eq!(
            state.players[1].speed, None,
            "the enchanted player is not attacking"
        );
    }

    /// CR 102.3 + CR 508.6: "opponent" is team-aware. In Two-Headed Giant P1 is
    /// P0's teammate, so P1 attacking the enchanted P2 is not an opponent
    /// attacking that player. The same board in a free-for-all game makes P1 an
    /// opponent, which proves the arm is reached and the team axis alone decides.
    /// Reverting to the raw `!= controller` test starts P1's engines in 2HG.
    #[test]
    fn opponent_attacking_enchanted_player_excludes_teammates() {
        let (mut state, curse, _) = curse_attack_fixture(
            FormatConfig::two_headed_giant(),
            PlayerId(2),
            &[PlayerId(1)],
        );
        let ability = start_engines_for_attackers_of_enchanted(curse);
        let mut events = Vec::new();
        resolve_start(&mut state, &ability, &mut events).unwrap();
        assert_eq!(
            state.players[1].speed, None,
            "P1 is P0's teammate in 2HG, not an opponent"
        );

        let (mut state, curse, _) =
            curse_attack_fixture(FormatConfig::standard(), PlayerId(2), &[PlayerId(1)]);
        let ability = start_engines_for_attackers_of_enchanted(curse);
        let mut events = Vec::new();
        resolve_start(&mut state, &ability, &mut events).unwrap();
        assert_eq!(
            state.players[1].speed,
            Some(1),
            "reach-guard: in a free-for-all game P1 is an opponent attacking P2"
        );
    }
}
