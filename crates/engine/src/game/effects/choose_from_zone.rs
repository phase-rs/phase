use crate::game::filter::{matches_target_filter, FilterContext};
use crate::game::players;
use crate::types::ability::{
    ChooseFromZoneConstraint, Chooser, Effect, EffectError, EffectKind, ResolvedAbility,
    TargetFilter, TargetRef, ZoneOwner,
};
use crate::types::card_type::CoreType;
use crate::types::events::GameEvent;
use crate::types::game_state::{GameState, WaitingFor};
use crate::types::identifiers::ObjectId;
use crate::types::player::PlayerId;
use crate::types::zones::Zone;

/// CR 700.2: Choose card(s) from a tracked set — player selects from exiled/revealed cards.
/// The available cards come from the most recent tracked set recorded by the parent effect
/// (e.g., ChangeZone to exile). The `chooser` field determines whether the controller or
/// an opponent makes the selection.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let (count, zone, additional_zones, zone_owner, filter, chooser, up_to, constraint) =
        match &ability.effect {
            Effect::ChooseFromZone {
                count,
                zone,
                additional_zones,
                zone_owner,
                filter,
                chooser,
                up_to,
                constraint,
                ..
            } => (
                *count as usize,
                *zone,
                additional_zones.clone(),
                *zone_owner,
                filter.clone(),
                *chooser,
                *up_to,
                constraint.clone(),
            ),
            _ => return Err(EffectError::MissingParam("ChooseFromZone".to_string())),
        };

    // CR 101.4 + CR 608.2c: "For each player, choose ... in that player's zone"
    // iterates every player in APNAP order, parking one choice per player and
    // accumulating each pick into the chain's tracked set. Routed here before
    // the single-pool path so the per-player prompts never collapse into one
    // candidate scan. Building block for Breach the Multiverse.
    if matches!(zone_owner, ZoneOwner::EachPlayer) {
        let players = crate::game::players::apnap_order(state);
        // No pick has accumulated yet — the first one must start a fresh set.
        return prompt_next_each_player(state, ability, players, false, events);
    }

    let cards = resolve_candidate_cards(
        state,
        ability,
        zone,
        &additional_zones,
        zone_owner,
        filter.as_ref(),
    )?;

    // CR 700.2: If there are no objects to choose from, skip the choice.
    if cards.is_empty() || count == 0 {
        events.push(GameEvent::EffectResolved {
            kind: EffectKind::ChooseFromZone,
            source_id: ability.source_id,
        });
        return Ok(());
    }

    let clamped_count = count.min(cards.len());

    // CR 700.2: Determine who makes the choice.
    let choosing_player = resolve_chooser(state, ability, chooser);

    state.waiting_for = WaitingFor::ChooseFromZoneChoice {
        player: choosing_player,
        cards,
        count: clamped_count,
        up_to,
        constraint,
        source_id: ability.source_id,
    };

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::ChooseFromZone,
        source_id: ability.source_id,
    });

    Ok(())
}

/// CR 101.4 + CR 608.2c: Park the next eligible player's `ChooseFromZoneChoice`
/// for a `ChooseFromZone { zone_owner: EachPlayer }` iteration, stashing the
/// players still to be prompted in `pending_per_player_zone_choice`. Players
/// whose zone holds no matching candidate are skipped (CR 608.2c — there's
/// nothing to choose). When no eligible player remains, the iteration is
/// disposed (the parked `pending_continuation` then runs). Drives both the
/// initial call from `resolve` and each resumed call from
/// `drain_pending_per_player_zone_choice`.
fn prompt_next_each_player(
    state: &mut GameState,
    ability: &ResolvedAbility,
    mut remaining_players: Vec<PlayerId>,
    accumulated: bool,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let (count, zone, additional_zones, filter, chooser, up_to, constraint) = match &ability.effect
    {
        Effect::ChooseFromZone {
            count,
            zone,
            additional_zones,
            filter,
            chooser,
            up_to,
            constraint,
            ..
        } => (
            *count as usize,
            *zone,
            additional_zones.clone(),
            filter.clone(),
            *chooser,
            *up_to,
            constraint.clone(),
        ),
        _ => return Err(EffectError::MissingParam("ChooseFromZone".to_string())),
    };

    while let Some(owner) = remaining_players.first().copied() {
        remaining_players.remove(0);

        let cards = collect_player_zone_cards(
            state,
            ability,
            owner,
            zone,
            &additional_zones,
            filter.as_ref(),
        );
        if cards.is_empty() || count == 0 {
            continue;
        }

        let clamped_count = count.min(cards.len());
        // CR 101.4 + CR 608.2c: For "for each player, choose ...", the spell's controller is
        // the chooser regardless of whose zone is scanned (Breach the
        // Multiverse). `Chooser::Opponent` would route to an opponent; honor it.
        let choosing_player = resolve_chooser(state, ability, chooser);

        state.waiting_for = WaitingFor::ChooseFromZoneChoice {
            player: choosing_player,
            cards,
            count: clamped_count,
            up_to,
            constraint,
            source_id: ability.source_id,
        };
        state.pending_per_player_zone_choice =
            Some(crate::types::game_state::PendingPerPlayerZoneChoice {
                ability: Box::new(ability.clone()),
                remaining_players,
                accumulated,
            });
        return Ok(());
    }

    // CR 608.2c: No player had an eligible card — emit the resolution event so the
    // parked continuation ("put those cards onto the battlefield") still runs.
    events.push(GameEvent::EffectResolved {
        kind: EffectKind::ChooseFromZone,
        source_id: ability.source_id,
    });
    Ok(())
}

/// CR 101.4 + CR 608.2c: Resume a per-player `ChooseFromZone { EachPlayer }`
/// iteration after the current player's pick resolves. Accumulates the chosen
/// cards into the resolution chain's tracked set (a fresh set on the first
/// pick, extended on each subsequent pick) so a downstream "put those cards
/// onto the battlefield" reads exactly the cards chosen across all players,
/// then prompts the next eligible player. Mirrors
/// `vote::drain_pending_vote_ballot_iteration`.
pub(crate) fn drain_pending_per_player_zone_choice(
    state: &mut GameState,
    chosen: &[ObjectId],
    events: &mut Vec<GameEvent>,
) {
    let Some(pending) = state.pending_per_player_zone_choice.take() else {
        return;
    };

    let crate::types::game_state::PendingPerPlayerZoneChoice {
        ability,
        remaining_players,
        accumulated,
    } = pending;

    // CR 603.7 + CR 608.2c: The FIRST pick of this per-player iteration STARTS a
    // fresh chosen-card set. It must NOT extend an earlier producer's tracked
    // set — Breach the Multiverse mills first (publishing a "Milled" set), so
    // extending here would reanimate the milled cards alongside the chosen ones
    // ("those cards" = the chosen cards only, CR 608.2c). `publish_fresh_tracked_set`
    // allocates a new set and rebinds `chain_tracked_set_id`, overwriting the
    // milled binding. Every LATER pick extends that fresh set so all players'
    // chosen cards unify under one "those cards" reference. The Cyberman / impulse
    // "milled this way" path is unaffected — it never uses this per-player drain.
    let accumulated = if chosen.is_empty() {
        accumulated
    } else if accumulated {
        super::publish_tracked_set(state, chosen.to_vec());
        true
    } else {
        super::publish_fresh_tracked_set(state, chosen.to_vec());
        true
    };

    let _ = prompt_next_each_player(state, &ability, remaining_players, accumulated, events);
}

/// CR 101.4: Candidate cards in a SINGLE player's zone(s) for a per-player
/// iteration, applying the effect's filter. Unlike `collect_direct_zone_cards`,
/// the owner is supplied explicitly (the iterating player), so the tracked-set
/// short-circuit in `resolve_candidate_cards` is bypassed — each player's
/// graveyard is scanned independently.
fn collect_player_zone_cards(
    state: &GameState,
    ability: &ResolvedAbility,
    owner: PlayerId,
    zone: Zone,
    additional_zones: &[Zone],
    filter: Option<&TargetFilter>,
) -> Vec<ObjectId> {
    let filter_ctx = FilterContext::from_ability(ability);
    let mut zones = Vec::with_capacity(1 + additional_zones.len());
    zones.push(zone);
    zones.extend_from_slice(additional_zones);
    zones
        .into_iter()
        .flat_map(|zone| object_ids_in_player_zone(state, owner, zone))
        .filter(|id| {
            filter.is_none_or(|filter| matches_target_filter(state, *id, filter, &filter_ctx))
        })
        .collect()
}

/// CR 608.2c + CR 608.2d + CR 603.7: Resolve the candidate card pool for a
/// tracked-set pick.
///
/// Priority order:
/// 1. The current resolution chain's tracked set (if non-empty).
/// 2. The latest non-empty tracked set from any prior publish in this game.
/// 3. Explicit `TargetRef::Object` targets on the ability.
/// 4. Direct zone scan (`zone` + `additional_zones`).
fn resolve_candidate_cards(
    state: &GameState,
    ability: &ResolvedAbility,
    zone: Zone,
    additional_zones: &[Zone],
    zone_owner: ZoneOwner,
    filter: Option<&TargetFilter>,
) -> Result<Vec<ObjectId>, EffectError> {
    if let Some(cards) = chain_tracked_set_cards(state) {
        return Ok(cards);
    }

    let cards = crate::game::targeting::latest_tracked_set_id(state)
        .and_then(|id| state.tracked_object_sets.get(&id).cloned())
        .unwrap_or_else(|| {
            ability
                .targets
                .iter()
                .filter_map(|t| match t {
                    TargetRef::Object(id) => Some(*id),
                    _ => None,
                })
                .collect()
        });

    let cards = if cards.is_empty() {
        collect_direct_zone_cards(state, ability, zone, additional_zones, zone_owner, filter)?
    } else {
        cards
    };

    Ok(cards)
}

fn chain_tracked_set_cards(state: &GameState) -> Option<Vec<ObjectId>> {
    let chain_id = state.chain_tracked_set_id?;
    let cards = state.tracked_object_sets.get(&chain_id)?;
    (!cards.is_empty()).then(|| cards.clone())
}

fn collect_direct_zone_cards(
    state: &GameState,
    ability: &ResolvedAbility,
    zone: Zone,
    additional_zones: &[Zone],
    zone_owner: ZoneOwner,
    filter: Option<&TargetFilter>,
) -> Result<Vec<ObjectId>, EffectError> {
    let filter_ctx = FilterContext::from_ability(ability);
    let mut zones = Vec::with_capacity(1 + additional_zones.len());
    zones.push(zone);
    zones.extend_from_slice(additional_zones);

    // CR 701.38d: For ScopedPlayer on Battlefield, scan ALL battlefield
    // permanents and rely on the filter (FilterProp::Owned { ScopedPlayer })
    // to restrict to objects owned by the voter. This is necessary because
    // "owned by" is distinct from "controlled by" — the voter may own
    // permanents that another player controls.
    if matches!(zone_owner, ZoneOwner::ScopedPlayer)
        && zones.iter().any(|z| matches!(z, Zone::Battlefield))
    {
        return Ok(state
            .battlefield
            .iter()
            .copied()
            .filter(|id| state.objects.get(id).is_some_and(|obj| obj.is_phased_in()))
            .filter(|id| {
                filter.is_none_or(|filter| matches_target_filter(state, *id, filter, &filter_ctx))
            })
            .collect());
    }

    let owner = resolve_zone_owner(state, ability, zone_owner)?;

    Ok(zones
        .into_iter()
        .flat_map(|zone| object_ids_in_player_zone(state, owner, zone))
        .filter(|id| {
            filter.is_none_or(|filter| matches_target_filter(state, *id, filter, &filter_ctx))
        })
        .collect())
}

fn resolve_zone_owner(
    state: &GameState,
    ability: &ResolvedAbility,
    zone_owner: ZoneOwner,
) -> Result<PlayerId, EffectError> {
    match zone_owner {
        ZoneOwner::Controller => Ok(ability.controller),
        ZoneOwner::TargetedPlayer => ability
            .targets
            .iter()
            .find_map(|target| match target {
                TargetRef::Player(player) => Some(*player),
                _ => None,
            })
            .ok_or_else(|| EffectError::MissingParam("ChooseFromZone targeted player".to_string())),
        ZoneOwner::Opponent => players::opponents(state, ability.controller)
            .into_iter()
            .next()
            .ok_or_else(|| EffectError::MissingParam("ChooseFromZone opponent".to_string())),
        // CR 701.38d: The scoped player (voter) supplies the zone.
        ZoneOwner::ScopedPlayer => Ok(ability.scoped_player.unwrap_or(ability.controller)),
        // CR 101.4: `EachPlayer` resolves a *set* of zone owners, not one — it
        // is handled by `prompt_next_each_player`, which scans each player's
        // zone directly via `collect_direct_zone_cards` and never routes here.
        ZoneOwner::EachPlayer => Err(EffectError::MissingParam(
            "ChooseFromZone EachPlayer resolves per-player, not via single owner".to_string(),
        )),
    }
}

fn object_ids_in_player_zone(state: &GameState, player: PlayerId, zone: Zone) -> Vec<ObjectId> {
    let Some(player_state) = state.players.iter().find(|p| p.id == player) else {
        return Vec::new();
    };

    match zone {
        Zone::Hand => player_state.hand.iter().copied().collect(),
        Zone::Library => player_state.library.iter().copied().collect(),
        Zone::Graveyard => player_state.graveyard.iter().copied().collect(),
        Zone::Exile => state
            .exile
            .iter()
            .copied()
            .filter(|id| state.objects.get(id).is_some_and(|obj| obj.owner == player))
            .collect(),
        Zone::Battlefield => state
            .battlefield
            .iter()
            .copied()
            .filter(|id| {
                state
                    .objects
                    .get(id)
                    .is_some_and(|obj| obj.controller == player && obj.is_phased_in())
            })
            .collect(),
        Zone::Stack => state
            .stack
            .iter()
            .filter(|entry| entry.controller == player)
            .map(|entry| entry.id)
            .collect(),
        Zone::Command => Vec::new(),
    }
}

/// CR 700.2: Resolve the `Chooser` enum to an actual `PlayerId`.
/// For `Opponent`, first checks ability targets for a pre-targeted opponent player
/// (handles "target opponent chooses"), then falls back to the first opponent in APNAP order.
fn resolve_chooser(state: &GameState, ability: &ResolvedAbility, chooser: Chooser) -> PlayerId {
    match chooser {
        Chooser::Controller => ability.controller,
        Chooser::Opponent => {
            // Check if an opponent was already targeted by the spell.
            if let Some(targeted_opponent) = ability.targets.iter().find_map(|t| match t {
                TargetRef::Player(id) if *id != ability.controller => Some(*id),
                _ => None,
            }) {
                return targeted_opponent;
            }
            // Fallback: first opponent in APNAP order (CR-correct for 2-player).
            players::opponents(state, ability.controller)
                .into_iter()
                .next()
                .unwrap_or(ability.controller)
        }
    }
}

pub fn selection_satisfies_constraint(
    state: &GameState,
    chosen: &[ObjectId],
    constraint: Option<&ChooseFromZoneConstraint>,
) -> bool {
    match constraint {
        None => true,
        Some(ChooseFromZoneConstraint::DistinctCardTypes { categories }) => {
            selected_cards_cover_distinct_card_types(state, chosen, categories)
        }
    }
}

fn selected_cards_cover_distinct_card_types(
    state: &GameState,
    chosen: &[ObjectId],
    categories: &[CoreType],
) -> bool {
    if chosen.is_empty() {
        return true;
    }
    if chosen.len() > categories.len() {
        return false;
    }

    let card_options: Option<Vec<Vec<usize>>> = chosen
        .iter()
        .map(|id| {
            state.objects.get(id).map(|obj| {
                categories
                    .iter()
                    .enumerate()
                    .filter_map(|(idx, category)| {
                        obj.card_types.core_types.contains(category).then_some(idx)
                    })
                    .collect::<Vec<_>>()
            })
        })
        .collect();

    let mut card_options = match card_options {
        Some(options) => options,
        None => return false,
    };
    if card_options.iter().any(Vec::is_empty) {
        return false;
    }

    card_options.sort_by_key(Vec::len);
    let mut used = vec![false; categories.len()];
    assign_distinct_categories(&card_options, &mut used, 0)
}

fn assign_distinct_categories(card_options: &[Vec<usize>], used: &mut [bool], idx: usize) -> bool {
    if idx == card_options.len() {
        return true;
    }
    for &category_idx in &card_options[idx] {
        if used[category_idx] {
            continue;
        }
        used[category_idx] = true;
        if assign_distinct_categories(card_options, used, idx + 1) {
            return true;
        }
        used[category_idx] = false;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::ability::{TypeFilter, TypedFilter};
    use crate::types::identifiers::{CardId, TrackedSetId};
    use crate::types::zones::Zone;

    /// Regression: `ChooseFromZoneConstraint` must serialize internally tagged
    /// (`{ "type": "DistinctCardTypes", ... }`) so the frontend `CardChoiceModal`
    /// confirm gate — which discriminates on `constraint.type` — can recognize the
    /// constraint. The default external representation left `type` undefined and
    /// permanently disabled the confirm button (e.g. Atraxa, Grand Unifier).
    #[test]
    fn distinct_card_types_constraint_is_internally_tagged() {
        let constraint = ChooseFromZoneConstraint::DistinctCardTypes {
            categories: vec![CoreType::Creature, CoreType::Land],
        };
        let value = serde_json::to_value(&constraint).unwrap();
        assert_eq!(value["type"], "DistinctCardTypes");
        assert_eq!(value["categories"][0], "Creature");
        assert_eq!(value["categories"][1], "Land");
        // Round-trips back to an equal value.
        let back: ChooseFromZoneConstraint = serde_json::from_value(value).unwrap();
        assert_eq!(back, constraint);
    }

    #[test]
    fn resolve_with_controller_chooser() {
        let mut state = GameState::new_two_player(42);
        let card1 = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Card A".to_string(),
            Zone::Exile,
        );
        let card2 = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Card B".to_string(),
            Zone::Exile,
        );

        // Simulate tracked set from parent ChangeZone
        state
            .tracked_object_sets
            .insert(TrackedSetId(1), vec![card1, card2]);
        state.next_tracked_set_id = 2;

        let ability = ResolvedAbility::new(
            Effect::ChooseFromZone {
                count: 1,
                zone: Zone::Exile,
                additional_zones: Vec::new(),
                zone_owner: ZoneOwner::Controller,
                filter: None,
                chooser: Chooser::Controller,
                up_to: false,
                constraint: None,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        match &state.waiting_for {
            WaitingFor::ChooseFromZoneChoice {
                player,
                cards,
                count,
                up_to,
                constraint,
                ..
            } => {
                assert_eq!(*player, PlayerId(0), "Controller should be the chooser");
                assert_eq!(cards.len(), 2);
                assert_eq!(*count, 1);
                assert!(!up_to);
                assert!(constraint.is_none());
            }
            other => panic!("Expected ChooseFromZoneChoice, got {:?}", other),
        }
    }

    #[test]
    fn resolve_with_opponent_chooser() {
        let mut state = GameState::new_two_player(42);
        let card1 = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Card A".to_string(),
            Zone::Exile,
        );

        state
            .tracked_object_sets
            .insert(TrackedSetId(1), vec![card1]);
        state.next_tracked_set_id = 2;

        let ability = ResolvedAbility::new(
            Effect::ChooseFromZone {
                count: 1,
                zone: Zone::Exile,
                additional_zones: Vec::new(),
                zone_owner: ZoneOwner::Controller,
                filter: None,
                chooser: Chooser::Opponent,
                up_to: false,
                constraint: None,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        match &state.waiting_for {
            WaitingFor::ChooseFromZoneChoice { player, count, .. } => {
                assert_eq!(*player, PlayerId(1), "Opponent should be the chooser");
                assert_eq!(*count, 1);
            }
            other => panic!("Expected ChooseFromZoneChoice, got {:?}", other),
        }
    }

    #[test]
    fn resolve_with_targeted_opponent() {
        let mut state = GameState::new_two_player(42);
        let card1 = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Card A".to_string(),
            Zone::Exile,
        );

        state
            .tracked_object_sets
            .insert(TrackedSetId(1), vec![card1]);
        state.next_tracked_set_id = 2;

        // Simulate a targeted opponent (e.g., Gifts Ungiven targeting PlayerId(1))
        let ability = ResolvedAbility::new(
            Effect::ChooseFromZone {
                count: 1,
                zone: Zone::Exile,
                additional_zones: Vec::new(),
                zone_owner: ZoneOwner::Controller,
                filter: None,
                chooser: Chooser::Opponent,
                up_to: false,
                constraint: None,
            },
            vec![TargetRef::Player(PlayerId(1))],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        match &state.waiting_for {
            WaitingFor::ChooseFromZoneChoice { player, .. } => {
                assert_eq!(
                    *player,
                    PlayerId(1),
                    "Targeted opponent should be the chooser"
                );
            }
            other => panic!("Expected ChooseFromZoneChoice, got {:?}", other),
        }
    }

    #[test]
    fn empty_tracked_set_skips_choice() {
        let mut state = GameState::new_two_player(42);

        let ability = ResolvedAbility::new(
            Effect::ChooseFromZone {
                count: 1,
                zone: Zone::Exile,
                additional_zones: Vec::new(),
                zone_owner: ZoneOwner::Controller,
                filter: None,
                chooser: Chooser::Opponent,
                up_to: false,
                constraint: None,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        // Should not set ChooseFromZoneChoice — no cards to choose from
        assert!(
            !matches!(state.waiting_for, WaitingFor::ChooseFromZoneChoice { .. }),
            "Should skip choice when tracked set is empty"
        );
    }

    #[test]
    fn count_clamped_to_available_cards() {
        let mut state = GameState::new_two_player(42);
        let card1 = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Card A".to_string(),
            Zone::Exile,
        );

        state
            .tracked_object_sets
            .insert(TrackedSetId(1), vec![card1]);
        state.next_tracked_set_id = 2;

        // Request 3 but only 1 card available
        let ability = ResolvedAbility::new(
            Effect::ChooseFromZone {
                count: 3,
                zone: Zone::Exile,
                additional_zones: Vec::new(),
                zone_owner: ZoneOwner::Controller,
                filter: None,
                chooser: Chooser::Controller,
                up_to: false,
                constraint: None,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        match &state.waiting_for {
            WaitingFor::ChooseFromZoneChoice { count, .. } => {
                assert_eq!(*count, 1, "Count should be clamped to available cards");
            }
            other => panic!("Expected ChooseFromZoneChoice, got {:?}", other),
        }
    }

    #[test]
    fn direct_zone_choice_filters_controller_hand() {
        let mut state = GameState::new_two_player(42);
        let creature = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Grizzly Bears".to_string(),
            Zone::Hand,
        );
        state
            .objects
            .get_mut(&creature)
            .unwrap()
            .card_types
            .core_types = vec![CoreType::Creature];
        let land = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Forest".to_string(),
            Zone::Hand,
        );
        state.objects.get_mut(&land).unwrap().card_types.core_types = vec![CoreType::Land];

        let ability = ResolvedAbility::new(
            Effect::ChooseFromZone {
                count: 1,
                zone: Zone::Hand,
                additional_zones: Vec::new(),
                zone_owner: ZoneOwner::Controller,
                filter: Some(TargetFilter::Typed(TypedFilter {
                    type_filters: vec![TypeFilter::Creature],
                    ..Default::default()
                })),
                chooser: Chooser::Controller,
                up_to: false,
                constraint: None,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        match &state.waiting_for {
            WaitingFor::ChooseFromZoneChoice { cards, count, .. } => {
                assert_eq!(*cards, vec![creature]);
                assert_eq!(*count, 1);
            }
            other => panic!("Expected ChooseFromZoneChoice, got {:?}", other),
        }
    }

    #[test]
    fn direct_zone_choice_uses_targeted_players_zones() {
        let mut state = GameState::new_two_player(42);
        let graveyard_card = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Graveyard Card".to_string(),
            Zone::Graveyard,
        );
        let hand_card = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Hand Card".to_string(),
            Zone::Hand,
        );
        let controller_hand_card = create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "Controller Hand Card".to_string(),
            Zone::Hand,
        );

        let ability = ResolvedAbility::new(
            Effect::ChooseFromZone {
                count: 1,
                zone: Zone::Graveyard,
                additional_zones: vec![Zone::Hand],
                zone_owner: ZoneOwner::TargetedPlayer,
                filter: None,
                chooser: Chooser::Controller,
                up_to: false,
                constraint: None,
            },
            vec![TargetRef::Player(PlayerId(1))],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        match &state.waiting_for {
            WaitingFor::ChooseFromZoneChoice { cards, .. } => {
                assert_eq!(*cards, vec![graveyard_card, hand_card]);
                assert!(!cards.contains(&controller_hand_card));
            }
            other => panic!("Expected ChooseFromZoneChoice, got {:?}", other),
        }
    }

    #[test]
    fn direct_zone_choice_requires_targeted_player() {
        let mut state = GameState::new_two_player(42);
        let _card = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Hand Card".to_string(),
            Zone::Hand,
        );

        let ability = ResolvedAbility::new(
            Effect::ChooseFromZone {
                count: 1,
                zone: Zone::Hand,
                additional_zones: Vec::new(),
                zone_owner: ZoneOwner::TargetedPlayer,
                filter: None,
                chooser: Chooser::Controller,
                up_to: false,
                constraint: None,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();

        let err = resolve(&mut state, &ability, &mut events).unwrap_err();
        assert!(
            matches!(err, EffectError::MissingParam(message) if message == "ChooseFromZone targeted player")
        );
    }

    #[test]
    fn distinct_card_type_constraint_accepts_valid_assignment() {
        let mut state = GameState::new_two_player(42);
        let artifact_creature = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Patchwork Automaton".to_string(),
            Zone::Library,
        );
        state
            .objects
            .get_mut(&artifact_creature)
            .unwrap()
            .card_types
            .core_types = vec![CoreType::Artifact, CoreType::Creature];
        let creature = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Elvish Mystic".to_string(),
            Zone::Library,
        );
        state
            .objects
            .get_mut(&creature)
            .unwrap()
            .card_types
            .core_types = vec![CoreType::Creature];

        assert!(selection_satisfies_constraint(
            &state,
            &[artifact_creature, creature],
            Some(&ChooseFromZoneConstraint::DistinctCardTypes {
                categories: vec![CoreType::Artifact, CoreType::Creature],
            }),
        ));
    }

    #[test]
    fn distinct_card_type_constraint_rejects_duplicate_assignment_only() {
        let mut state = GameState::new_two_player(42);
        let creature_a = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Elvish Mystic".to_string(),
            Zone::Library,
        );
        state
            .objects
            .get_mut(&creature_a)
            .unwrap()
            .card_types
            .core_types = vec![CoreType::Creature];
        let creature_b = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Llanowar Elves".to_string(),
            Zone::Library,
        );
        state
            .objects
            .get_mut(&creature_b)
            .unwrap()
            .card_types
            .core_types = vec![CoreType::Creature];

        assert!(!selection_satisfies_constraint(
            &state,
            &[creature_a, creature_b],
            Some(&ChooseFromZoneConstraint::DistinctCardTypes {
                categories: vec![CoreType::Artifact, CoreType::Creature],
            }),
        ));
    }

    /// End-to-end regression for Atraxa, Grand Unifier's ETB chain:
    /// RevealTop(10) → ChooseFromZone(DistinctCardTypes) must offer only the
    /// revealed library cards.
    #[test]
    fn atraxa_style_reveal_top_chain_offers_revealed_library_cards() {
        use super::super::resolve_ability_chain;
        use crate::types::ability::TargetFilter;

        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(900),
            PlayerId(0),
            "Atraxa, Grand Unifier".to_string(),
            Zone::Battlefield,
        );

        let mut library_top = Vec::new();
        for i in 0..10 {
            let core_type = if i % 2 == 0 {
                CoreType::Creature
            } else {
                CoreType::Instant
            };
            let id = create_object(
                &mut state,
                CardId(i + 1),
                PlayerId(0),
                format!("Library Card {i}"),
                Zone::Library,
            );
            state.objects.get_mut(&id).unwrap().card_types.core_types = vec![core_type];
            library_top.push(id);
        }
        let _library_padding = create_object(
            &mut state,
            CardId(50),
            PlayerId(0),
            "Library Padding".to_string(),
            Zone::Library,
        );

        let stale_graveyard_card = create_object(
            &mut state,
            CardId(99),
            PlayerId(0),
            "Stale Graveyard Card".to_string(),
            Zone::Graveyard,
        );
        state
            .tracked_object_sets
            .insert(TrackedSetId(5), vec![stale_graveyard_card]);
        state.next_tracked_set_id = 6;

        let categories = vec![
            CoreType::Artifact,
            CoreType::Battle,
            CoreType::Creature,
            CoreType::Enchantment,
            CoreType::Instant,
            CoreType::Land,
            CoreType::Planeswalker,
            CoreType::Sorcery,
        ];
        let change_zone = Box::new(ResolvedAbility::new(
            Effect::ChangeZone {
                origin: Some(Zone::Library),
                destination: Zone::Hand,
                target: TargetFilter::Any,
                owner_library: false,
                enter_transformed: false,
                enters_under: None,
                enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
                face_down_profile: None,
            },
            vec![],
            source,
            PlayerId(0),
        ));
        let choose = ResolvedAbility {
            sub_ability: Some(change_zone),
            ..ResolvedAbility::new(
                Effect::ChooseFromZone {
                    count: categories.len() as u32,
                    zone: Zone::Library,
                    additional_zones: Vec::new(),
                    zone_owner: ZoneOwner::Controller,
                    filter: None,
                    chooser: Chooser::Controller,
                    up_to: true,
                    constraint: Some(ChooseFromZoneConstraint::DistinctCardTypes { categories }),
                },
                vec![],
                source,
                PlayerId(0),
            )
        };
        let reveal = ResolvedAbility {
            sub_ability: Some(Box::new(choose)),
            ..ResolvedAbility::new(
                Effect::RevealTop {
                    player: TargetFilter::Controller,
                    count: 10,
                },
                vec![],
                source,
                PlayerId(0),
            )
        };

        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &reveal, &mut events, 0).unwrap();

        match &state.waiting_for {
            WaitingFor::ChooseFromZoneChoice { cards, up_to, .. } => {
                assert!(up_to);
                assert_eq!(cards.len(), 10, "must offer exactly the ten revealed cards");
                for id in &library_top {
                    assert!(cards.contains(id), "missing revealed library card {id:?}");
                }
                assert!(
                    !cards.contains(&stale_graveyard_card),
                    "graveyard cards must never appear in the reveal-and-choose pool"
                );
                assert!(
                    !cards.contains(&_library_padding),
                    "cards below the reveal window must not be offered"
                );
            }
            other => panic!(
                "Expected ChooseFromZoneChoice after RevealTop, got {:?}",
                other
            ),
        }
    }
}
