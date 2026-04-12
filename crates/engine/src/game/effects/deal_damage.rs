use std::collections::HashSet;

use crate::game::filter;
use crate::game::keywords;
use crate::game::quantity::resolve_quantity_with_targets;
use crate::game::replacement::{self, ReplacementResult};
use crate::types::ability::{
    DamageSource, Effect, EffectError, EffectKind, PlayerFilter, ResolvedAbility, TargetFilter,
    TargetRef,
};
use crate::types::card_type::CoreType;
use crate::types::counter::CounterType;
use crate::types::events::GameEvent;
use crate::types::game_state::{DamageRecord, GameState};
use crate::types::identifiers::ObjectId;
use crate::types::keywords::Keyword;
use crate::types::player::PlayerId;
use crate::types::proposed_event::ProposedEvent;

/// Source attributes needed for damage application (CR 120.3).
/// Read from the source object before the mutable damage phase to avoid borrow conflicts.
pub(crate) struct DamageContext {
    pub(crate) source_id: ObjectId,
    pub(crate) controller: PlayerId,
    pub(crate) has_deathtouch: bool,
    pub(crate) has_lifelink: bool,
    pub(crate) has_wither: bool,
    pub(crate) has_infect: bool,
}

impl DamageContext {
    /// Build context by reading keywords from the source object.
    /// Returns None if source doesn't exist in state.
    pub(crate) fn from_source(state: &GameState, source_id: ObjectId) -> Option<Self> {
        state.objects.get(&source_id).map(|obj| Self {
            source_id,
            controller: obj.controller,
            has_deathtouch: obj.has_keyword(&Keyword::Deathtouch),
            has_lifelink: obj.has_keyword(&Keyword::Lifelink),
            has_wither: obj.has_keyword(&Keyword::Wither),
            has_infect: obj.has_keyword(&Keyword::Infect),
        })
    }

    /// Fallback context when source no longer exists (all keyword flags false).
    /// CR 702.15c: last known information should be used for lifelink, but if the
    /// source is truly gone with no LKI available, defaulting to false is safe.
    pub(crate) fn fallback(source_id: ObjectId, controller: PlayerId) -> Self {
        Self {
            source_id,
            controller,
            has_deathtouch: false,
            has_lifelink: false,
            has_wither: false,
            has_infect: false,
        }
    }
}

/// Outcome of applying damage through the replacement pipeline.
pub(crate) enum DamageResult {
    /// Damage applied (possibly modified/prevented). Contains post-replacement amount dealt.
    Applied(u32),
    /// A replacement effect requires a player choice before damage resolves.
    NeedsChoice,
}

/// CR 120.3 + CR 120.4b: Apply damage from a single source to a single target through
/// the full replacement/prevention pipeline.
///
/// Handles: protection (CR 702.16b), replacement effects (CR 120.4b), damage marking
/// (CR 120.3e), planeswalker loyalty (CR 120.3c / CR 306.8), wither (CR 702.80),
/// infect (CR 702.90), deathtouch (CR 702.2b), lifelink (CR 702.15b), and
/// DamageDealt event emission.
///
/// Event ordering: DamageDealt is emitted before lifelink LifeChanged.
/// EffectResolved is NOT emitted — that remains the caller's responsibility.
///
/// Returns `DamageResult::Applied(actual_amount)` or `DamageResult::NeedsChoice`.
pub(crate) fn apply_damage_to_target(
    state: &mut GameState,
    ctx: &DamageContext,
    target: TargetRef,
    amount: u32,
    is_combat: bool,
    events: &mut Vec<GameEvent>,
) -> Result<DamageResult, EffectError> {
    // CR 120.8: If a source would deal 0 damage, it does not deal damage at all.
    if amount == 0 {
        return Ok(DamageResult::Applied(0));
    }

    // CR 702.16b + CR 702.16e: Protection prevents damage from sources with the matching quality.
    // Emits DamagePrevented so "when damage is prevented" triggers can fire.
    if let TargetRef::Object(target_obj_id) = &target {
        if let (Some(target_obj), Some(source_obj)) = (
            state.objects.get(target_obj_id),
            state.objects.get(&ctx.source_id),
        ) {
            if keywords::protection_prevents_from(target_obj, source_obj) {
                events.push(GameEvent::DamagePrevented {
                    source_id: ctx.source_id,
                    target: target.clone(),
                    amount,
                });
                return Ok(DamageResult::Applied(0));
            }
        }
    }

    let proposed = ProposedEvent::Damage {
        source_id: ctx.source_id,
        target: target.clone(),
        amount,
        is_combat,
        applied: HashSet::new(),
    };

    match replacement::replace_event(state, proposed, events) {
        ReplacementResult::Execute(event) => {
            if let ProposedEvent::Damage {
                target: ref t,
                amount: actual_amount,
                ..
            } = event
            {
                match t {
                    TargetRef::Object(obj_id) => {
                        if ctx.has_wither || ctx.has_infect {
                            // CR 702.80 + CR 702.90: Wither/infect deals damage as -1/-1 counters.
                            if let Some(target_obj) = state.objects.get_mut(obj_id) {
                                let entry = target_obj
                                    .counters
                                    .entry(CounterType::Minus1Minus1)
                                    .or_insert(0);
                                *entry += actual_amount;
                                if ctx.has_deathtouch {
                                    target_obj.dealt_deathtouch_damage = true;
                                }
                            }
                            state.layers_dirty = true;
                        } else if let Some(target_obj) = state.objects.get_mut(obj_id) {
                            if target_obj
                                .card_types
                                .core_types
                                .contains(&CoreType::Planeswalker)
                            {
                                // CR 120.3c / CR 306.8: Damage to a planeswalker removes loyalty counters.
                                let current = target_obj.loyalty.unwrap_or(0);
                                let new_loyalty = current.saturating_sub(actual_amount);
                                target_obj.loyalty = Some(new_loyalty);
                                target_obj
                                    .counters
                                    .insert(CounterType::Loyalty, new_loyalty);
                            } else {
                                // CR 120.3e: Damage to a creature marks damage.
                                target_obj.damage_marked += actual_amount;
                                // CR 702.2b: Track deathtouch for SBA lethal-damage check.
                                if ctx.has_deathtouch {
                                    target_obj.dealt_deathtouch_damage = true;
                                }
                            }
                        }
                    }
                    TargetRef::Player(player_id) => {
                        if ctx.has_infect {
                            // CR 702.90: Infect deals damage to players as poison counters.
                            if let Some(player) =
                                state.players.iter_mut().find(|p| p.id == *player_id)
                            {
                                player.poison_counters += actual_amount;
                            }
                        } else {
                            // CR 120.3a: Damage to a player causes life loss.
                            if super::life::apply_damage_life_loss(
                                state,
                                *player_id,
                                actual_amount,
                                events,
                            )
                            .is_err()
                            {
                                // CR 614.7: Life loss replacement needs player choice.
                                return Ok(DamageResult::NeedsChoice);
                            }
                        }
                    }
                }

                // CR 120.10: Compute excess damage beyond lethal for creatures/planeswalkers.
                let excess = match &t {
                    TargetRef::Object(obj_id) => {
                        state
                            .objects
                            .get(obj_id)
                            .and_then(|obj| {
                                if obj.card_types.core_types.contains(&CoreType::Creature) {
                                    obj.toughness.map(|toughness| {
                                        // damage_marked already includes actual_amount (line 158)
                                        let damage_before =
                                            obj.damage_marked.saturating_sub(actual_amount);
                                        let lethal = if ctx.has_deathtouch {
                                            // CR 702.2c: Any nonzero damage from deathtouch = lethal
                                            if damage_before == 0 {
                                                1u32
                                            } else {
                                                0
                                            }
                                        } else {
                                            (toughness as u32).saturating_sub(damage_before)
                                        };
                                        actual_amount.saturating_sub(lethal)
                                    })
                                } else if obj
                                    .card_types
                                    .core_types
                                    .contains(&CoreType::Planeswalker)
                                {
                                    // CR 120.10: Excess for planeswalkers = damage beyond pre-hit loyalty.
                                    // Loyalty was already decremented, so reconstruct pre-hit value.
                                    let pre_loyalty = obj.loyalty.unwrap_or(0) + actual_amount;
                                    Some(actual_amount.saturating_sub(pre_loyalty))
                                } else {
                                    Some(0)
                                }
                            })
                            .unwrap_or(0)
                    }
                    TargetRef::Player(_) => 0,
                };

                events.push(GameEvent::DamageDealt {
                    source_id: ctx.source_id,
                    target: t.clone(),
                    amount: actual_amount,
                    is_combat,
                    excess,
                });

                // CR 120.1: Record damage for "was dealt damage by" condition queries.
                if actual_amount > 0 {
                    state.damage_dealt_this_turn.push(DamageRecord {
                        source_id: ctx.source_id,
                        target: t.clone(),
                        amount: actual_amount,
                        is_combat,
                    });
                }

                // CR 702.15b / CR 120.3f: Lifelink — controller gains life equal to damage dealt.
                if ctx.has_lifelink
                    && actual_amount > 0
                    && super::life::apply_life_gain(state, ctx.controller, actual_amount, events)
                        .is_err()
                {
                    // CR 614.7: Life gain replacement needs player choice.
                    // Damage was already dealt; lifelink gain is deferred.
                    return Ok(DamageResult::NeedsChoice);
                }

                Ok(DamageResult::Applied(actual_amount))
            } else {
                Ok(DamageResult::Applied(0))
            }
        }
        ReplacementResult::Prevented => Ok(DamageResult::Applied(0)),
        ReplacementResult::NeedsChoice(player) => {
            // Only set waiting_for for non-combat damage; combat damage cannot pause mid-resolution.
            if !is_combat {
                state.waiting_for = replacement::replacement_choice_waiting_for(player, state);
            }
            Ok(DamageResult::NeedsChoice)
        }
    }
}

/// CR 120.1: Deal N damage — reduces life for players, marks damage on creatures.
/// Reads amount from `Effect::DealDamage { amount }`.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let (num_dmg, damage_source, target_filter): (u32, Option<DamageSource>, &TargetFilter) =
        match &ability.effect {
            Effect::DealDamage {
                amount,
                damage_source,
                target,
            } => (
                resolve_quantity_with_targets(state, amount, ability) as u32,
                *damage_source,
                target,
            ),
            _ => return Err(EffectError::MissingParam("DealDamage amount".to_string())),
        };

    // CR 120.3: Determine damage source. When DamageSource::Target, the first resolved
    // object target is the damage source (e.g., "target creature deals damage to itself").
    let ctx = if matches!(damage_source, Some(DamageSource::Target)) {
        ability
            .targets
            .iter()
            .find_map(|t| match t {
                TargetRef::Object(id) => DamageContext::from_source(state, *id),
                _ => None,
            })
            .unwrap_or_else(|| DamageContext::fallback(ability.source_id, ability.controller))
    } else {
        DamageContext::from_source(state, ability.source_id)
            .unwrap_or_else(|| DamageContext::fallback(ability.source_id, ability.controller))
    };

    // Resolve effective targets: use explicit targets if present, otherwise derive
    // implicit target from the TargetFilter for non-targeted damage ("to you", "to itself").
    let implicit;
    let effective_targets = if !ability.targets.is_empty() {
        &ability.targets
    } else {
        implicit = match target_filter {
            TargetFilter::Controller => vec![TargetRef::Player(ability.controller)],
            TargetFilter::SelfRef => vec![TargetRef::Object(ability.source_id)],
            _ => vec![],
        };
        &implicit
    };

    // CR 601.2d: If the caster distributed damage among targets at cast time,
    // apply per-target amounts from ability.distribution instead of uniform damage.
    if let Some(distribution) = &ability.distribution {
        for (target, amount) in distribution {
            match apply_damage_to_target(state, &ctx, target.clone(), *amount, false, events)? {
                DamageResult::Applied(_) => {}
                DamageResult::NeedsChoice => return Ok(()),
            }
        }
    } else {
        for target in effective_targets {
            match apply_damage_to_target(state, &ctx, target.clone(), num_dmg, false, events)? {
                DamageResult::Applied(_) => {}
                DamageResult::NeedsChoice => return Ok(()),
            }
        }
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::from(&ability.effect),
        source_id: ability.source_id,
    });

    Ok(())
}

/// Deal damage to all permanents (and optionally players) matching the filter.
/// Reads amount and filter from `Effect::DamageAll { amount, target }`.
/// CR 120.3: Damage is dealt simultaneously to all affected objects.
/// CR 120.3e: Non-combat damage from an effect is marked on each matching creature.
pub fn resolve_all(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let (num_dmg, target_filter): (u32, TargetFilter) = match &ability.effect {
        Effect::DamageAll { amount, target } => {
            // CR 107.1b: Ability-context resolve so X-damage-to-all ("Deal X damage to each...")
            // reads the caster-chosen X.
            let dmg = resolve_quantity_with_targets(state, amount, ability).max(0) as u32;
            (dmg, target.clone())
        }
        _ => return Err(EffectError::MissingParam("DamageAll amount".to_string())),
    };

    let target_filter = crate::game::effects::resolved_object_filter(ability, &target_filter);

    // Collect matching object IDs.
    // CR 107.3a + CR 601.2b: ability-context filter evaluation.
    let ctx = filter::FilterContext::from_ability(ability);
    let matching: Vec<_> = state
        .battlefield
        .iter()
        .filter(|id| filter::matches_target_filter(state, **id, &target_filter, &ctx))
        .copied()
        .collect();

    // TODO(CR 120.3h): Battle card type not handled — damage to a battle should remove defense counters.
    // Player damage uses the separate DamageEachPlayer effect type (PlayerFilter +
    // per-player quantity resolution). DamageAll is intentionally object-only.
    // TODO(CR 120.3): NeedsChoice during batch damage returns early — remaining targets skip damage.
    //   This is an engine-wide replacement-choice limitation, not specific to resolve_all.
    let ctx = DamageContext::from_source(state, ability.source_id)
        .unwrap_or_else(|| DamageContext::fallback(ability.source_id, ability.controller));

    for obj_id in matching {
        match apply_damage_to_target(
            state,
            &ctx,
            TargetRef::Object(obj_id),
            num_dmg,
            false,
            events,
        )? {
            DamageResult::Applied(_) => {}
            DamageResult::NeedsChoice => return Ok(()),
        }
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::from(&ability.effect),
        source_id: ability.source_id,
    });

    Ok(())
}

/// CR 120.3: Deal damage to each player matching a filter, with per-player quantity.
/// Resolves `amount` for each player using `resolve_quantity_scoped()`.
/// Used for "deals damage to each player equal to [per-player quantity]" patterns.
pub fn resolve_each_player(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let (amount_expr, player_filter) = match &ability.effect {
        Effect::DamageEachPlayer {
            amount,
            player_filter,
        } => (amount, *player_filter),
        _ => {
            return Err(EffectError::MissingParam(
                "DamageEachPlayer amount".to_string(),
            ))
        }
    };

    let ctx = DamageContext::from_source(state, ability.source_id)
        .unwrap_or_else(|| DamageContext::fallback(ability.source_id, ability.controller));

    // Collect matching player IDs first to avoid borrow issues.
    let player_ids: Vec<PlayerId> = state
        .players
        .iter()
        .filter(|p| {
            !p.is_eliminated
                && match &player_filter {
                    PlayerFilter::Controller => p.id == ability.controller,
                    PlayerFilter::All => true,
                    PlayerFilter::Opponent => p.id != ability.controller,
                    PlayerFilter::OpponentLostLife => {
                        p.id != ability.controller && p.life_lost_this_turn > 0
                    }
                    PlayerFilter::OpponentGainedLife => {
                        p.id != ability.controller && p.life_gained_this_turn > 0
                    }
                    PlayerFilter::HighestSpeed => {
                        let highest_speed = state
                            .players
                            .iter()
                            .filter(|player| !player.is_eliminated)
                            .map(|player| player.speed.unwrap_or(0))
                            .max()
                            .unwrap_or(0);
                        p.speed.unwrap_or(0) == highest_speed
                    }
                    PlayerFilter::ZoneChangedThisWay => state
                        .last_zone_changed_ids
                        .iter()
                        .any(|id| state.objects.get(id).is_some_and(|obj| obj.owner == p.id)),
                }
        })
        .map(|p| p.id)
        .collect();

    for pid in player_ids {
        // CR 120.3: Resolve quantity scoped to this player.
        let dmg = crate::game::quantity::resolve_quantity_scoped(
            state,
            amount_expr,
            ability.source_id,
            pid,
        )
        .max(0) as u32;
        if dmg > 0 {
            match apply_damage_to_target(state, &ctx, TargetRef::Player(pid), dmg, false, events)? {
                DamageResult::Applied(_) => {}
                DamageResult::NeedsChoice => return Ok(()),
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
    use crate::types::ability::{QuantityExpr, TargetFilter, TypedFilter};
    use crate::types::card_type::CoreType;
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::player::PlayerId;
    use crate::types::zones::Zone;

    fn make_ability(num_dmg: u32, targets: Vec<TargetRef>) -> ResolvedAbility {
        ResolvedAbility::new(
            Effect::DealDamage {
                amount: QuantityExpr::Fixed {
                    value: num_dmg as i32,
                },
                target: TargetFilter::Any,
                damage_source: None,
            },
            targets,
            ObjectId(100),
            PlayerId(0),
        )
    }

    #[test]
    fn deal_damage_to_creature() {
        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Bear".to_string(),
            Zone::Battlefield,
        );
        let ability = make_ability(3, vec![TargetRef::Object(obj_id)]);
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.objects[&obj_id].damage_marked, 3);
    }

    #[test]
    fn deal_damage_to_player() {
        let mut state = GameState::new_two_player(42);
        let ability = make_ability(5, vec![TargetRef::Player(PlayerId(1))]);
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.players[1].life, 15);
    }

    #[test]
    fn deal_damage_emits_events() {
        let mut state = GameState::new_two_player(42);
        let ability = make_ability(2, vec![TargetRef::Player(PlayerId(0))]);
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(events
            .iter()
            .any(|e| matches!(e, GameEvent::DamageDealt { amount: 2, .. })));
        assert!(events
            .iter()
            .any(|e| matches!(e, GameEvent::EffectResolved { .. })));
    }

    #[test]
    fn damage_all_creatures() {
        let mut state = GameState::new_two_player(42);
        let bear1 = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Bear".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&bear1)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        let bear2 = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Opp Bear".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&bear2)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        let ability = ResolvedAbility::new(
            Effect::DamageAll {
                amount: QuantityExpr::Fixed { value: 2 },
                target: TargetFilter::Typed(TypedFilter {
                    type_filters: vec![crate::types::ability::TypeFilter::Creature],
                    controller: None,
                    properties: vec![],
                }),
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve_all(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.objects[&bear1].damage_marked, 2);
        assert_eq!(state.objects[&bear2].damage_marked, 2);
    }

    #[test]
    fn damage_to_planeswalker_removes_loyalty() {
        let mut state = GameState::new_two_player(42);
        let pw_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Jace".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&pw_id).unwrap();
            obj.card_types.core_types.push(CoreType::Planeswalker);
            obj.loyalty = Some(5);
        }
        let ability = make_ability(3, vec![TargetRef::Object(pw_id)]);
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        // Damage removes loyalty, not damage_marked
        assert_eq!(state.objects[&pw_id].loyalty, Some(2)); // 5 - 3
        assert_eq!(state.objects[&pw_id].damage_marked, 0);
    }

    #[test]
    fn lethal_damage_to_planeswalker_sets_loyalty_zero() {
        let mut state = GameState::new_two_player(42);
        let pw_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Liliana".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&pw_id).unwrap();
            obj.card_types.core_types.push(CoreType::Planeswalker);
            obj.loyalty = Some(2);
        }
        let ability = make_ability(5, vec![TargetRef::Object(pw_id)]);
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        // Damage exceeds loyalty: clamped to 0 via saturating_sub
        assert_eq!(state.objects[&pw_id].loyalty, Some(0));
    }

    fn make_source_with_keyword(
        state: &mut GameState,
        keyword: crate::types::keywords::Keyword,
    ) -> ObjectId {
        let source_id = create_object(
            state,
            CardId(50),
            PlayerId(0),
            "Source".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&source_id).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.keywords.push(keyword);
        source_id
    }

    fn make_ability_with_source(
        num_dmg: u32,
        targets: Vec<TargetRef>,
        source_id: ObjectId,
    ) -> ResolvedAbility {
        ResolvedAbility::new(
            Effect::DealDamage {
                amount: QuantityExpr::Fixed {
                    value: num_dmg as i32,
                },
                target: TargetFilter::Any,
                damage_source: None,
            },
            targets,
            source_id,
            PlayerId(0),
        )
    }

    #[test]
    fn lifelink_spell_damage_to_player() {
        let mut state = GameState::new_two_player(42);
        let source_id =
            make_source_with_keyword(&mut state, crate::types::keywords::Keyword::Lifelink);
        let ability = make_ability_with_source(3, vec![TargetRef::Player(PlayerId(1))], source_id);
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        // CR 702.15b: Source controller gains life equal to damage dealt.
        assert_eq!(state.players[1].life, 17); // 20 - 3
        assert_eq!(state.players[0].life, 23); // 20 + 3
    }

    #[test]
    fn lifelink_spell_damage_to_creature() {
        let mut state = GameState::new_two_player(42);
        let source_id =
            make_source_with_keyword(&mut state, crate::types::keywords::Keyword::Lifelink);
        let target_id = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Bear".to_string(),
            Zone::Battlefield,
        );
        let ability = make_ability_with_source(3, vec![TargetRef::Object(target_id)], source_id);
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.objects[&target_id].damage_marked, 3);
        // CR 702.15b: Lifelink triggers on creature damage too.
        assert_eq!(state.players[0].life, 23); // 20 + 3
    }

    #[test]
    fn deathtouch_spell_damage_tracked() {
        let mut state = GameState::new_two_player(42);
        let source_id =
            make_source_with_keyword(&mut state, crate::types::keywords::Keyword::Deathtouch);
        let target_id = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Bear".to_string(),
            Zone::Battlefield,
        );
        let ability = make_ability_with_source(1, vec![TargetRef::Object(target_id)], source_id);
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.objects[&target_id].damage_marked, 1);
        // CR 702.2b: Deathtouch damage tracked for SBA.
        assert!(state.objects[&target_id].dealt_deathtouch_damage);
    }

    #[test]
    fn resolve_all_planeswalker_loyalty() {
        let mut state = GameState::new_two_player(42);
        let pw_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Jace".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&pw_id).unwrap();
            obj.card_types.core_types.push(CoreType::Planeswalker);
            obj.loyalty = Some(4);
        }

        let ability = ResolvedAbility::new(
            Effect::DamageAll {
                amount: QuantityExpr::Fixed { value: 2 },
                target: TargetFilter::Typed(TypedFilter {
                    type_filters: vec![crate::types::ability::TypeFilter::Planeswalker],
                    controller: None,
                    properties: vec![],
                }),
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve_all(&mut state, &ability, &mut events).unwrap();

        // CR 120.3c: Damage to planeswalker removes loyalty, not damage_marked.
        assert_eq!(state.objects[&pw_id].loyalty, Some(2));
        assert_eq!(state.objects[&pw_id].damage_marked, 0);
    }

    #[test]
    fn resolve_all_deathtouch_tracked() {
        let mut state = GameState::new_two_player(42);
        let source_id =
            make_source_with_keyword(&mut state, crate::types::keywords::Keyword::Deathtouch);
        state
            .objects
            .get_mut(&source_id)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        let target_id = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Bear".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&target_id)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        let ability = ResolvedAbility::new(
            Effect::DamageAll {
                amount: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Typed(TypedFilter {
                    type_filters: vec![crate::types::ability::TypeFilter::Creature],
                    controller: None,
                    properties: vec![],
                }),
            },
            vec![],
            source_id,
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve_all(&mut state, &ability, &mut events).unwrap();

        // CR 702.2b: Deathtouch tracked even through area damage.
        assert!(state.objects[&target_id].dealt_deathtouch_damage);
    }

    #[test]
    fn excess_damage_to_creature() {
        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Bear".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&obj_id).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.toughness = Some(2);
        }
        let ability = make_ability(5, vec![TargetRef::Object(obj_id)]);
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        // CR 120.10: 5 damage to 2-toughness creature = 3 excess
        let dmg_event = events
            .iter()
            .find(|e| matches!(e, GameEvent::DamageDealt { .. }))
            .unwrap();
        if let GameEvent::DamageDealt { excess, .. } = dmg_event {
            assert_eq!(*excess, 3);
        } else {
            panic!("expected DamageDealt event");
        }
    }

    #[test]
    fn excess_damage_with_deathtouch() {
        let mut state = GameState::new_two_player(42);
        let source_id =
            make_source_with_keyword(&mut state, crate::types::keywords::Keyword::Deathtouch);
        let target_id = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Dragon".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&target_id).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.toughness = Some(5);
        }
        let ability = make_ability_with_source(3, vec![TargetRef::Object(target_id)], source_id);
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        // CR 702.2c: Deathtouch makes 1 damage lethal, so 3 - 1 = 2 excess
        let dmg_event = events
            .iter()
            .find(|e| matches!(e, GameEvent::DamageDealt { .. }))
            .unwrap();
        if let GameEvent::DamageDealt { excess, .. } = dmg_event {
            assert_eq!(*excess, 2);
        } else {
            panic!("expected DamageDealt event");
        }
    }

    #[test]
    fn excess_damage_with_preexisting_damage() {
        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Bear".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&obj_id).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.toughness = Some(3);
            obj.damage_marked = 1; // Pre-existing damage
        }
        let ability = make_ability(4, vec![TargetRef::Object(obj_id)]);
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        // CR 120.10: toughness=3, pre-damage=1, lethal=(3-1)=2, excess=4-2=2
        let dmg_event = events
            .iter()
            .find(|e| matches!(e, GameEvent::DamageDealt { .. }))
            .unwrap();
        if let GameEvent::DamageDealt { excess, .. } = dmg_event {
            assert_eq!(*excess, 2);
        } else {
            panic!("expected DamageDealt event");
        }
    }

    #[test]
    fn excess_damage_to_player_is_zero() {
        let mut state = GameState::new_two_player(42);
        let ability = make_ability(5, vec![TargetRef::Player(PlayerId(1))]);
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        // Players don't have excess damage
        let dmg_event = events
            .iter()
            .find(|e| matches!(e, GameEvent::DamageDealt { .. }))
            .unwrap();
        if let GameEvent::DamageDealt { excess, .. } = dmg_event {
            assert_eq!(*excess, 0);
        } else {
            panic!("expected DamageDealt event");
        }
    }

    #[test]
    fn wither_spell_damage_applies_counters() {
        let mut state = GameState::new_two_player(42);
        let source_id =
            make_source_with_keyword(&mut state, crate::types::keywords::Keyword::Wither);
        let target_id = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Bear".to_string(),
            Zone::Battlefield,
        );
        let ability = make_ability_with_source(2, vec![TargetRef::Object(target_id)], source_id);
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        // CR 702.80: Wither applies -1/-1 counters instead of marking damage.
        assert_eq!(state.objects[&target_id].damage_marked, 0);
        assert_eq!(
            state.objects[&target_id]
                .counters
                .get(&crate::types::counter::CounterType::Minus1Minus1)
                .copied()
                .unwrap_or(0),
            2
        );
    }
}
