use crate::game::filter::{matches_target_filter, FilterContext};
use crate::game::quantity::resolve_quantity_with_targets;
use crate::types::ability::{
    DigSource, Effect, EffectError, EffectKind, ResolvedAbility, TargetFilter,
};
use crate::types::events::GameEvent;
use crate::types::game_state::{GameState, WaitingFor};
use crate::types::zones::Zone;

/// CR 701.20e + CR 608.2c: Look at top N cards (shown only to the looking player),
/// select some to keep per the effect's instructions, rest go elsewhere.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let (
        library_owner_filter,
        dig_num,
        raw_keep_num,
        is_up_to,
        filter,
        kept_dest,
        rest_dest,
        is_reveal,
        enter_tapped,
        dig_source,
    ) = match &ability.effect {
        Effect::Dig {
            player,
            count,
            keep_count,
            keep_count_expr,
            up_to,
            filter,
            destination,
            rest_destination,
            reveal,
            enter_tapped,
            source,
        } => {
            let resolved_count =
                resolve_quantity_with_targets(state, count, ability).max(0) as usize;
            // CR 107.1b: a dynamic keep count that resolves negative is clamped
            // to zero (no card is kept), never a negative selection bound.
            let dynamic_keep = keep_count_expr
                .as_ref()
                .map(|e| resolve_quantity_with_targets(state, e, ability).max(0) as usize);
            let keep_all_for_reorder = destination == &Some(Zone::Library)
                && rest_destination == &Some(Zone::Library)
                && keep_count.is_none()
                && dynamic_keep.is_none();
            (
                player,
                resolved_count,
                if keep_all_for_reorder {
                    resolved_count
                } else {
                    dynamic_keep.unwrap_or_else(|| keep_count.unwrap_or(1) as usize)
                },
                *up_to,
                filter.clone(),
                *destination,
                *rest_destination,
                *reveal,
                *enter_tapped,
                *source,
            )
        }
        _ => (
            &TargetFilter::Controller,
            1,
            1,
            false,
            TargetFilter::Any,
            None,
            None,
            false,
            false,
            DigSource::Library,
        ),
    };

    let library_owner = super::resolve_player_for_context_ref(state, ability, library_owner_filter);

    // CR 401.5 + CR 608.2c: This Dig's own outcome — not a stale value from an
    // earlier link in the same chain — is what `apply_parent_chain_context`
    // relays to this Dig's immediate sub_ability. Reset here; the two "found
    // nothing" returns below (and in `resolve_from_prior_look`) set it back
    // to `true`.
    state.last_dig_found_nothing = false;

    // CR 701.20e + CR 608.2c: PriorLook means the card set was already populated
    // by a preceding look-only Dig (e.g. Birthing Ritual: sacrifice sits between
    // the look step and the choice step). Read from private_look_ids so that
    // effect_context_object (the sacrifice snapshot) is available when
    // selectable_cards is computed.
    if dig_source == DigSource::PriorLook {
        return resolve_from_prior_look(
            state,
            ability,
            events,
            library_owner,
            raw_keep_num,
            is_up_to,
            filter,
            kept_dest,
            rest_dest,
            enter_tapped,
        );
    }

    let player = state
        .players
        .iter()
        .find(|p| p.id == library_owner)
        .ok_or(EffectError::PlayerNotFound)?;

    // CR 401.5: If a library has fewer cards than required, use as many as available.
    let count = dig_num.min(player.library.len());
    if count == 0 {
        // CR 608.2c: Nothing was looked at — a chained `ParentTarget` consumer
        // ("put up to one of them on top … the rest on the bottom") has no
        // cards to act on and must not fall back to acting on this ability's
        // own source (issue #1365).
        state.last_dig_found_nothing = true;
        events.push(GameEvent::EffectResolved {
            kind: EffectKind::from(&ability.effect),
            source_id: ability.source_id,
        });
        return Ok(());
    }

    let cards: Vec<_> = player
        .library
        .iter()
        .take(count)
        .copied()
        .collect::<Vec<_>>();
    let raw_keep_count = raw_keep_num.min(cards.len());

    // CR 701.20e: Pure-peek pattern (keep_count = 0): "look at the top card" with no
    // player selection — the sub_ability condition decides whether to take it. Set
    // last_revealed_ids so RevealedHasCardType can evaluate, then return without
    // creating a DigChoice interaction.
    if raw_keep_count == 0 && !is_reveal {
        state.last_revealed_ids = cards.clone();
        // CR 701.20e: "look at" privately reveals the cards to the looking
        // player. The looker is the ability controller (e.g. Delver of Secrets'
        // "look at the top card of your library"). Record the looker-scoped peek
        // window so `filter_state_for_viewer` keeps these cards visible to the
        // looker — and only the looker — through any subsequent "you may reveal
        // that card" optional decision, instead of leaving the looking player to
        // choose blind.
        state.private_look_ids = cards.clone();
        state.private_look_player = Some(ability.controller);
        events.push(GameEvent::EffectResolved {
            kind: EffectKind::from(&ability.effect),
            source_id: ability.source_id,
        });
        return Ok(());
    }

    // CR 701.20a: If this is a reveal-dig, mark all cards as publicly revealed
    // and emit CardsRevealed before the player makes their selection.
    if is_reveal {
        for &card_id in &cards {
            state.revealed_cards.insert(card_id);
        }
        state.last_revealed_ids = cards.clone();
        let card_names: Vec<String> = cards
            .iter()
            .filter_map(|id| state.objects.get(id).map(|o| o.name.clone()))
            .collect();
        events.push(GameEvent::CardsRevealed {
            player: ability.controller,
            card_ids: cards.clone(),
            card_names,
        });
    }

    // Pre-compute selectable cards by evaluating the filter against each card.
    // CR 107.3a + CR 601.2b: Use ability context so dynamic thresholds (e.g.
    // `CmcLE { Variable("X") }`) resolve against the caster's announced X.
    let selectable_cards = if matches!(filter, TargetFilter::Any) {
        cards.clone()
    } else {
        let ctx = FilterContext::from_ability(ability);
        cards
            .iter()
            .filter(|&&card_id| matches_target_filter(state, card_id, &filter, &ctx))
            .copied()
            .collect()
    };
    // CR 608.2c + CR 701.20a/701.20e: A mass "put ALL <filter> from among them
    // onto [destination] and the rest [elsewhere]" instruction is deterministic —
    // the controller keeps EVERY matching looked-at card with no selection step.
    // The parser lowers this to the unbounded keep sentinel (`u32::MAX`) with
    // `up_to == false` and a concrete kept `destination`. Resolve it directly
    // instead of surfacing a `WaitingFor::DigChoice`, which would force a bogus
    // "choose N" prompt (issue #2896, Muxus, Goblin Grandee). When the kept
    // destination is `None` (the reveal-only tracked-set form, e.g. Zimone's
    // Experiment), downstream sub_abilities route the kept cards by type, so that
    // form still goes through the choice path below.
    if raw_keep_num == u32::MAX as usize && !is_up_to {
        if let Some(dest) = kept_dest {
            resolve_mass_put_all(
                state,
                ability,
                &cards,
                &selectable_cards,
                dest,
                rest_dest,
                enter_tapped,
                events,
            );
            return Ok(());
        }
    }

    let keep_count = if raw_keep_num == u32::MAX as usize {
        selectable_cards.len()
    } else {
        raw_keep_count
    };

    state.waiting_for = WaitingFor::DigChoice {
        player: ability.controller,
        library_owner,
        selectable_cards,
        cards,
        keep_count,
        up_to: is_up_to,
        kept_destination: kept_dest,
        rest_destination: rest_dest,
        source_id: Some(ability.source_id),
        enter_tapped,
    };

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::from(&ability.effect),
        source_id: ability.source_id,
    });

    Ok(())
}

/// CR 701.20e + CR 608.2c: Resolve a Dig whose card set comes from a
/// preceding look-only Dig (`source: DigSource::PriorLook`). Two sub-paths:
///
/// 1. **Decline branch** (`raw_keep_num == 0` with `rest_dest == Library`):
///    No interactive choice. Route ALL looked-at cards to library bottom then
///    clear the private look window. This fires when the player declined the
///    optional sacrifice that gates the "if you do, put … from among those"
///    instruction (Birthing Ritual).
///
/// 2. **Interactive path** (`raw_keep_num > 0`): Present `WaitingFor::DigChoice`
///    reading from `state.private_look_ids`. The sacrifice snapshot is already
///    stored in `ability.context.effect_context_object` at this point, so the
///    CMC filter (`CmcLE { CostPaidObject MV + 1 }`) evaluates correctly.
#[allow(clippy::too_many_arguments)]
fn resolve_from_prior_look(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
    library_owner: crate::types::player::PlayerId,
    raw_keep_num: usize,
    is_up_to: bool,
    filter: TargetFilter,
    kept_dest: Option<Zone>,
    rest_dest: Option<Zone>,
    enter_tapped: bool,
) -> Result<(), EffectError> {
    let cards = state.private_look_ids.clone();
    if cards.is_empty() {
        // CR 608.2c: mirrors the empty-library branch in `resolve` (issue
        // #1365) — no cards were looked at, so a chained `ParentTarget`
        // consumer must not self-fallback.
        state.last_dig_found_nothing = true;
        events.push(GameEvent::EffectResolved {
            kind: EffectKind::Dig,
            source_id: ability.source_id,
        });
        return Ok(());
    }

    let raw_keep_count = raw_keep_num.min(cards.len());

    // Decline branch: keep_count=0 means "put all on rest_dest" (player declined
    // the gating action, e.g. the optional sacrifice). Route all looked-at
    // cards to rest_dest without any interactive prompt.
    if raw_keep_count == 0 {
        if let Some(dest) = rest_dest {
            crate::game::engine_resolution_choices::route_rest_partition(
                state, &cards, dest, events,
            );
        }
        state.private_look_ids.clear();
        state.private_look_player = None;
        events.push(GameEvent::EffectResolved {
            kind: EffectKind::Dig,
            source_id: ability.source_id,
        });
        return Ok(());
    }

    // Interactive path: present DigChoice. selectable_cards uses the sacrifice
    // snapshot already stamped onto ability.context.effect_context_object.
    let selectable_cards = if matches!(filter, TargetFilter::Any) {
        cards.clone()
    } else {
        let ctx = FilterContext::from_ability(ability);
        cards
            .iter()
            .filter(|&&card_id| matches_target_filter(state, card_id, &filter, &ctx))
            .copied()
            .collect()
    };

    // CR 608.2c: If no cards pass the filter, auto-resolve by routing all to
    // rest_dest instead of surfacing an impossible DigChoice prompt.
    if selectable_cards.is_empty() {
        if let Some(dest) = rest_dest {
            crate::game::engine_resolution_choices::route_rest_partition(
                state, &cards, dest, events,
            );
        }
        state.private_look_ids.clear();
        state.private_look_player = None;
        events.push(GameEvent::EffectResolved {
            kind: EffectKind::Dig,
            source_id: ability.source_id,
        });
        return Ok(());
    }

    // Cap keep_count to selectable count — can't keep more than there are legal
    // choices, regardless of what the effect text specifies.
    let keep_count = if raw_keep_num == u32::MAX as usize {
        selectable_cards.len()
    } else {
        raw_keep_num.min(selectable_cards.len())
    };

    state.waiting_for = WaitingFor::DigChoice {
        player: ability.controller,
        library_owner,
        selectable_cards,
        cards,
        keep_count,
        up_to: is_up_to,
        kept_destination: kept_dest,
        rest_destination: rest_dest,
        source_id: Some(ability.source_id),
        enter_tapped,
    };

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::Dig,
        source_id: ability.source_id,
    });

    Ok(())
}

/// CR 608.2c + CR 701.20a/701.20e: Deterministically resolve a mass "put ALL
/// <filter> from among them" Dig — every filter-matching looked-at card
/// (`selectable`) goes to `dest`, every other looked-at card goes to
/// `rest_destination` (None = bottom of library; CR 701.20a "in a random
/// order"). No `DigChoice` interaction is surfaced because the instruction
/// admits no player choice.
///
/// The rest pile is routed first (a deterministic library/zone placement), so a
/// rare CR 303.4f/616.1 battlefield-entry pause on a kept card cannot strand it.
/// The kept cards then move through the zone-change pipeline (CR 614.1c — ETB
/// triggers fire, intrinsic enters-with counters seed). On a kept-card pause the
/// remaining kept moves and the tracked-set publish are deferred onto a
/// `RevealRestPile` completion (empty rest pile — already placed), mirroring the
/// `DigChoice` handler's deferral contract.
#[allow(clippy::too_many_arguments)]
fn resolve_mass_put_all(
    state: &mut GameState,
    ability: &ResolvedAbility,
    cards: &[crate::types::identifiers::ObjectId],
    selectable: &[crate::types::identifiers::ObjectId],
    dest: Zone,
    rest_destination: Option<Zone>,
    enter_tapped: bool,
    events: &mut Vec<GameEvent>,
) {
    let rest: Vec<_> = cards
        .iter()
        .filter(|id| !selectable.contains(id))
        .copied()
        .collect();

    // Route the (deterministic) rest pile first so a kept-card pause cannot
    // strand it. None => bottom of library (CR 701.20a "in a random order").
    crate::game::engine_resolution_choices::route_rest_partition(
        state,
        &rest,
        rest_destination.unwrap_or(Zone::Library),
        events,
    );

    if dest == Zone::Battlefield {
        // CR 614.1c + CR 306.5b / CR 310.4b: route battlefield entries through
        // the batch zone-change pipeline so ETB triggers fire, intrinsic
        // enters-with counters / tap state seed, and any CR 303.4f / CR 616.1
        // pause preserves the remaining kept tail. CR 400.7: attribute entries
        // to the Dig's source.
        let reqs: Vec<_> = selectable
            .iter()
            .map(|&obj_id| {
                let mut req = crate::game::zone_pipeline::ZoneMoveRequest::effect(
                    obj_id,
                    Zone::Battlefield,
                    ability.source_id,
                );
                req.mods.enter_tapped =
                    crate::types::zones::EtbTapState::from_legacy_bool(enter_tapped);
                req
            })
            .collect();
        match crate::game::zone_pipeline::move_objects_simultaneously(state, reqs, events) {
            crate::game::zone_pipeline::BatchMoveResult::Done => {}
            crate::game::zone_pipeline::BatchMoveResult::NeedsChoice => {
                crate::game::zone_pipeline::defer_completion_on_pause(
                    state,
                    crate::types::game_state::BatchCompletion::RevealRestPile {
                        player: ability.controller,
                        rest_cards: Vec::new(),
                        rest_destination: rest_destination.unwrap_or(Zone::Library),
                        clear_markers: Vec::new(),
                        publish_tracked_set: Some(selectable.to_vec()),
                        emit_reveal_until_resolved: None,
                    },
                );
                return;
            }
        }
    } else {
        for &obj_id in selectable {
            crate::game::zones::move_to_zone(state, obj_id, dest, events);
        }
    }

    // CR 701.20b + CR 608.2c: publish the kept (revealed) cards as a fresh
    // tracked set so any downstream sub_ability can route them by type.
    super::publish_fresh_tracked_set(state, selectable.to_vec());

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::from(&ability.effect),
        source_id: ability.source_id,
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::engine_resolution_choices::{
        handle_resolution_choice, ResolutionChoiceOutcome,
    };
    use crate::game::zones::create_object;
    use crate::parser::oracle_effect::parse_effect_chain;
    use crate::types::ability::SpellContext;
    use crate::types::ability::{
        AbilityCondition, AbilityKind, FilterProp, QuantityExpr, TypedFilter,
    };
    use crate::types::actions::GameAction;
    use crate::types::card_type::CoreType;
    use crate::types::card_type::Supertype;
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::mana::{ManaCost, ManaCostShard};
    use crate::types::player::PlayerId;
    use crate::types::zones::Zone;

    fn make_dig_ability(dig_num: u32) -> ResolvedAbility {
        ResolvedAbility::new(
            Effect::Dig {
                player: TargetFilter::Controller,
                count: QuantityExpr::Fixed {
                    value: dig_num as i32,
                },
                destination: None,
                keep_count: None,
                keep_count_expr: None,
                up_to: false,
                filter: TargetFilter::Any,
                rest_destination: None,
                reveal: false,
                enter_tapped: false,
                source: DigSource::Library,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        )
    }

    #[test]
    fn test_dig_5_keep_1_sets_waiting_for_dig_choice() {
        let mut state = GameState::new_two_player(42);
        for i in 0..7 {
            create_object(
                &mut state,
                CardId(i + 1),
                PlayerId(0),
                format!("Card {}", i),
                Zone::Library,
            );
        }
        let top_5: Vec<_> = state.players[0]
            .library
            .iter()
            .take(5)
            .copied()
            .collect::<Vec<_>>();

        let ability = make_dig_ability(5);
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        match &state.waiting_for {
            WaitingFor::DigChoice {
                player,
                cards,
                keep_count,
                ..
            } => {
                assert_eq!(*player, PlayerId(0));
                assert_eq!(cards.len(), 5);
                assert_eq!(*cards, top_5);
                assert_eq!(*keep_count, 1);
            }
            other => panic!("Expected DigChoice, got {:?}", other),
        }
    }

    #[test]
    fn test_dig_with_empty_library_does_nothing() {
        let mut state = GameState::new_two_player(42);
        assert!(state.players[0].library.is_empty());

        let ability = make_dig_ability(3);
        let mut events = Vec::new();

        let result = resolve(&mut state, &ability, &mut events);
        assert!(result.is_ok());
        assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
        assert!(
            state.last_dig_found_nothing,
            "an empty-library Dig must flag that it found nothing, so a chained \
             ParentTarget consumer does not self-fallback (issue #1365)"
        );
        assert!(
            events
                .iter()
                .any(|e| matches!(e, GameEvent::EffectResolved { .. })),
            "an empty-library Dig must still emit EffectResolved"
        );
    }

    #[test]
    fn pure_peek_uses_target_players_library_without_moving_cards() {
        let mut state = GameState::new_two_player(42);
        create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Opponent Top".to_string(),
            Zone::Library,
        );
        let top_card = state.players[1].library[0];
        let ability = ResolvedAbility::new(
            Effect::Dig {
                player: TargetFilter::Player,
                count: QuantityExpr::Fixed { value: 1 },
                destination: None,
                keep_count: Some(0),
                keep_count_expr: None,
                up_to: false,
                filter: TargetFilter::Any,
                rest_destination: None,
                reveal: false,
                enter_tapped: false,
                source: DigSource::Library,
            },
            vec![crate::types::ability::TargetRef::Player(PlayerId(1))],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.last_revealed_ids, vec![top_card]);
        assert_eq!(state.objects[&top_card].zone, Zone::Library);
        assert_eq!(state.players[1].library.front(), Some(&top_card));
        assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
        // CR 701.20e: the looker is the ability controller, not the library
        // owner — the peeked opponent card is visible to the controller only.
        assert_eq!(state.private_look_ids, vec![top_card]);
        assert_eq!(state.private_look_player, Some(PlayerId(0)));
    }

    /// CR 701.20e (issue #2021, Delver of Secrets): a bare "look at the top card
    /// of your library" peek must privately reveal the card to the looking
    /// player, so they can SEE it before deciding a subsequent "you may reveal
    /// that card" optional. The peek records a looker-scoped window
    /// (`private_look_ids` / `private_look_player`) that `filter_state_for_viewer`
    /// surfaces to the looker and hides from opponents.
    #[test]
    fn look_at_top_card_makes_peek_visible_to_looker_only() {
        use crate::game::visibility::filter_state_for_viewer;

        let mut state = GameState::new_two_player(42);
        create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Delver Top Card".to_string(),
            Zone::Library,
        );
        let top_card = state.players[0].library[0];

        // "look at the top card of your library" — Dig keep_count 0, no reveal.
        let ability = ResolvedAbility::new(
            Effect::Dig {
                player: TargetFilter::Controller,
                count: QuantityExpr::Fixed { value: 1 },
                destination: None,
                keep_count: Some(0),
                keep_count_expr: None,
                up_to: false,
                filter: TargetFilter::Any,
                rest_destination: None,
                reveal: false,
                enter_tapped: false,
                source: DigSource::Library,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.private_look_ids, vec![top_card]);
        assert_eq!(state.private_look_player, Some(PlayerId(0)));
        // CR 701.20e: a private "look at" must NOT publicly reveal the card.
        assert!(!state.revealed_cards.contains(&top_card));

        // The looking player (PlayerId(0)) can see the peeked card's identity.
        let looker_view = filter_state_for_viewer(&state, PlayerId(0));
        assert_eq!(
            looker_view.objects[&top_card].name, "Delver Top Card",
            "the looking player must see the card they looked at"
        );

        // The opponent (PlayerId(1)) must NOT see it — the library card is hidden.
        let opp_view = filter_state_for_viewer(&state, PlayerId(1));
        assert_ne!(
            opp_view.objects[&top_card].name, "Delver Top Card",
            "the private look must not leak the card to opponents"
        );
    }

    #[test]
    fn dig_reorder_mode_sets_keep_count_to_all_seen_cards() {
        let mut state = GameState::new_two_player(42);
        for i in 0..5 {
            create_object(
                &mut state,
                CardId(i + 1),
                PlayerId(0),
                format!("Card {}", i),
                Zone::Library,
            );
        }
        let ability = ResolvedAbility::new(
            Effect::Dig {
                player: TargetFilter::Controller,
                count: QuantityExpr::Fixed { value: 3 },
                destination: Some(Zone::Library),
                keep_count: None,
                keep_count_expr: None,
                up_to: false,
                filter: TargetFilter::Any,
                rest_destination: Some(Zone::Library),
                reveal: false,
                enter_tapped: false,
                source: DigSource::Library,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        match &state.waiting_for {
            WaitingFor::DigChoice {
                cards, keep_count, ..
            } => {
                assert_eq!(cards.len(), 3);
                assert_eq!(*keep_count, 3);
            }
            other => panic!("Expected DigChoice, got {:?}", other),
        }
    }

    /// CR 701.20b + CR 608.2c: After the player's `SelectCards` resolves a
    /// `DigChoice`, the kept (revealed) cards must be published to
    /// `state.tracked_object_sets` so downstream sub_abilities can route
    /// them by type via `TargetFilter::TrackedSetFiltered`. Zimone's
    /// Experiment depends on this — its post-Dig `"Put all land cards
    /// revealed this way onto the battlefield tapped"` resolves against
    /// the tracked set the Dig choice publishes.
    #[test]
    fn dig_choice_publishes_kept_cards_as_tracked_set() {
        use crate::game::engine_resolution_choices::{
            handle_resolution_choice, ResolutionChoiceOutcome,
        };
        use crate::types::actions::GameAction;
        use crate::types::identifiers::TrackedSetId;

        let mut state = GameState::new_two_player(42);
        let mut card_ids = Vec::new();
        for i in 0..5 {
            let id = create_object(
                &mut state,
                CardId(i + 1),
                PlayerId(0),
                format!("Card {}", i),
                Zone::Library,
            );
            card_ids.push(id);
        }
        let cards_on_top: Vec<_> = state.players[0]
            .library
            .iter()
            .take(5)
            .copied()
            .collect::<Vec<_>>();
        let kept: Vec<_> = cards_on_top[..2].to_vec();

        // Simulate Zimone's Dig setup: keep up to 2, no inline destination,
        // rest → library bottom. Matches the parse shape of Zimone's post-
        // `parse_dig_from_among`-patch Dig.
        let waiting = WaitingFor::DigChoice {
            player: PlayerId(0),
            library_owner: PlayerId(0),
            selectable_cards: cards_on_top.clone(),
            cards: cards_on_top.clone(),
            keep_count: 2,
            up_to: true,
            kept_destination: None,
            rest_destination: Some(Zone::Library),
            source_id: Some(ObjectId(100)),
            enter_tapped: false,
        };
        let action = GameAction::SelectCards {
            cards: kept.clone(),
        };
        let next_id_before = state.next_tracked_set_id;
        let mut events = Vec::new();

        let outcome = handle_resolution_choice(&mut state, waiting, action, &mut events)
            .expect("DigChoice resolution must succeed");
        assert!(matches!(outcome, ResolutionChoiceOutcome::WaitingFor(_)));
        for &obj_id in &kept {
            assert_eq!(
                state.objects[&obj_id].zone,
                Zone::Library,
                "reveal-only DigChoice must not auto-route kept cards"
            );
            assert!(
                !state.players[0].hand.contains(&obj_id),
                "reveal-only DigChoice must not move kept cards to hand"
            );
        }

        // A fresh tracked set must publish the kept/revealed selection so
        // downstream TrackedSetFiltered routing (Zimone land/creature split)
        // resolves against the cards the player chose to keep.
        let tracked_id = TrackedSetId(next_id_before);
        let set = state
            .tracked_object_sets
            .get(&tracked_id)
            .expect("tracked set must be inserted for the kept cards");
        assert_eq!(
            *set, kept,
            "tracked set must contain exactly the kept cards"
        );
        assert_eq!(
            state.next_tracked_set_id,
            next_id_before + 1,
            "next_tracked_set_id must have advanced"
        );
        assert_eq!(
            state.chain_tracked_set_id,
            Some(tracked_id),
            "TrackedSetId(0) continuations must bind to the kept-card set"
        );
    }

    #[test]
    fn dig_choice_empty_selection_rebinds_fresh_tracked_set() {
        use crate::game::engine_resolution_choices::{
            handle_resolution_choice, ResolutionChoiceOutcome,
        };
        use crate::types::actions::GameAction;
        use crate::types::identifiers::TrackedSetId;

        let mut state = GameState::new_two_player(42);
        let prior = TrackedSetId(7);
        state.tracked_object_sets.insert(prior, vec![ObjectId(999)]);
        state.chain_tracked_set_id = Some(prior);
        let cards: Vec<_> = (0..2)
            .map(|i| {
                create_object(
                    &mut state,
                    CardId(i + 20),
                    PlayerId(0),
                    format!("Card {}", i),
                    Zone::Library,
                )
            })
            .collect();
        let next_id_before = state.next_tracked_set_id;
        let waiting = WaitingFor::DigChoice {
            player: PlayerId(0),
            library_owner: PlayerId(0),
            selectable_cards: cards.clone(),
            cards,
            keep_count: 2,
            up_to: true,
            kept_destination: None,
            rest_destination: Some(Zone::Library),
            source_id: Some(ObjectId(100)),
            enter_tapped: false,
        };
        let mut events = Vec::new();

        let outcome = handle_resolution_choice(
            &mut state,
            waiting,
            GameAction::SelectCards { cards: Vec::new() },
            &mut events,
        )
        .expect("DigChoice resolution must succeed");

        assert!(matches!(outcome, ResolutionChoiceOutcome::WaitingFor(_)));
        let fresh = TrackedSetId(next_id_before);
        assert_eq!(state.tracked_object_sets.get(&fresh), Some(&Vec::new()));
        assert_eq!(state.chain_tracked_set_id, Some(fresh));
    }

    #[test]
    fn dig_choice_reorders_all_looked_at_cards_on_top_before_continuation() {
        use crate::game::engine_resolution_choices::{
            handle_resolution_choice, ResolutionChoiceOutcome,
        };
        use crate::types::actions::GameAction;
        use crate::types::game_state::PendingContinuation;

        let mut state = GameState::new_two_player(42);
        for i in 0..5 {
            create_object(
                &mut state,
                CardId(i + 1),
                PlayerId(0),
                format!("Card {}", i),
                Zone::Library,
            );
        }
        let cards_on_top: Vec<_> = state.players[0]
            .library
            .iter()
            .take(3)
            .copied()
            .collect::<Vec<_>>();
        let remaining_library: Vec<_> = state.players[0]
            .library
            .iter()
            .skip(3)
            .copied()
            .collect::<Vec<_>>();
        let selected_order = vec![cards_on_top[2], cards_on_top[0], cards_on_top[1]];

        let waiting = WaitingFor::DigChoice {
            player: PlayerId(0),
            library_owner: PlayerId(0),
            selectable_cards: cards_on_top.clone(),
            cards: cards_on_top,
            keep_count: 3,
            up_to: false,
            kept_destination: Some(Zone::Library),
            rest_destination: Some(Zone::Library),
            source_id: Some(ObjectId(100)),
            enter_tapped: false,
        };
        state.pending_continuation =
            Some(PendingContinuation::new(Box::new(ResolvedAbility::new(
                Effect::Draw {
                    count: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::Controller,
                },
                vec![],
                ObjectId(100),
                PlayerId(0),
            ))));

        let mut events = Vec::new();
        let outcome = handle_resolution_choice(
            &mut state,
            waiting,
            GameAction::SelectCards {
                cards: selected_order.clone(),
            },
            &mut events,
        )
        .expect("DigChoice resolution must succeed");

        assert!(matches!(outcome, ResolutionChoiceOutcome::WaitingFor(_)));
        assert!(
            state.players[0].hand.contains(&selected_order[0]),
            "draw continuation must draw the first card in the selected order"
        );
        let expected_library: Vec<_> = selected_order[1..]
            .iter()
            .chain(remaining_library.iter())
            .copied()
            .collect();
        assert_eq!(
            state.players[0].library.iter().copied().collect::<Vec<_>>(),
            expected_library,
            "selected order must become top-of-library order before drawing"
        );
    }

    #[test]
    fn dig_choice_rejects_duplicate_selected_cards() {
        use crate::game::engine_resolution_choices::handle_resolution_choice;
        use crate::types::actions::GameAction;

        let mut state = GameState::new_two_player(42);
        for i in 0..3 {
            create_object(
                &mut state,
                CardId(i + 1),
                PlayerId(0),
                format!("Card {}", i),
                Zone::Library,
            );
        }
        let original_library = state.players[0].library.iter().copied().collect::<Vec<_>>();
        let cards_on_top = original_library.clone();

        let waiting = WaitingFor::DigChoice {
            player: PlayerId(0),
            library_owner: PlayerId(0),
            selectable_cards: cards_on_top.clone(),
            cards: cards_on_top.clone(),
            keep_count: 3,
            up_to: false,
            kept_destination: Some(Zone::Library),
            rest_destination: Some(Zone::Library),
            source_id: Some(ObjectId(100)),
            enter_tapped: false,
        };

        let mut events = Vec::new();
        let result = handle_resolution_choice(
            &mut state,
            waiting,
            GameAction::SelectCards {
                cards: vec![cards_on_top[0], cards_on_top[0], cards_on_top[1]],
            },
            &mut events,
        );

        assert!(result.is_err(), "duplicate selections must be rejected");
        assert_eq!(
            state.players[0].library.iter().copied().collect::<Vec<_>>(),
            original_library,
            "invalid duplicate selection must not mutate library order"
        );
    }

    /// CR 401.2 + CR 608.2c: a `DigChoice` selection must be drawn from the cards
    /// actually looked at. Regression guard for the freeform-selection hole — the
    /// old handler skipped this check whenever `selectable_cards` was empty (a
    /// filtered dig that matched nothing), so an `apply`-level `SelectCards` with
    /// a foreign object id was accepted and moved into the chooser's hand.
    #[test]
    fn dig_choice_rejects_card_not_looked_at() {
        use crate::game::engine_resolution_choices::handle_resolution_choice;
        use crate::types::actions::GameAction;

        let mut state = GameState::new_two_player(42);
        for i in 0..3 {
            create_object(
                &mut state,
                CardId(i + 1),
                PlayerId(0),
                format!("Card {i}"),
                Zone::Library,
            );
        }
        let cards_on_top = state.players[0].library.iter().copied().collect::<Vec<_>>();
        // A card the dig never looked at.
        let foreign = create_object(
            &mut state,
            CardId(99),
            PlayerId(0),
            "Foreign".to_string(),
            Zone::Library,
        );
        let original_library = state.players[0].library.iter().copied().collect::<Vec<_>>();

        // Filtered dig that matched nothing -> empty selectable set (the hole).
        let waiting = WaitingFor::DigChoice {
            player: PlayerId(0),
            library_owner: PlayerId(0),
            selectable_cards: Vec::new(),
            cards: cards_on_top,
            keep_count: 1,
            up_to: true,
            kept_destination: Some(Zone::Hand),
            rest_destination: Some(Zone::Graveyard),
            source_id: Some(ObjectId(100)),
            enter_tapped: false,
        };

        let mut events = Vec::new();
        let result = handle_resolution_choice(
            &mut state,
            waiting,
            GameAction::SelectCards {
                cards: vec![foreign],
            },
            &mut events,
        );

        assert!(
            result.is_err(),
            "a card that was not looked at must be rejected (CR 401.2)"
        );
        assert!(
            !state.players[0].hand.contains(&foreign),
            "rejected selection must not move the foreign card to hand"
        );
        assert_eq!(
            state.players[0].library.iter().copied().collect::<Vec<_>>(),
            original_library,
            "rejected selection must not mutate the library"
        );
    }

    /// CR 401.2 + CR 608.2c: when a dig's filter matches nothing, the only legal
    /// keep-selection is empty — a looked-at card that doesn't match the filter
    /// must still be rejected. Regression guard for the same empty-`selectable`
    /// hole: the old handler accepted it and moved it to hand.
    #[test]
    fn dig_choice_rejects_card_excluded_by_empty_filter() {
        use crate::game::engine_resolution_choices::handle_resolution_choice;
        use crate::types::actions::GameAction;

        let mut state = GameState::new_two_player(42);
        for i in 0..3 {
            create_object(
                &mut state,
                CardId(i + 1),
                PlayerId(0),
                format!("Card {i}"),
                Zone::Library,
            );
        }
        let cards_on_top = state.players[0].library.iter().copied().collect::<Vec<_>>();
        let original_library = cards_on_top.clone();

        let waiting = WaitingFor::DigChoice {
            player: PlayerId(0),
            library_owner: PlayerId(0),
            selectable_cards: Vec::new(),
            cards: cards_on_top.clone(),
            keep_count: 1,
            up_to: true,
            kept_destination: Some(Zone::Hand),
            rest_destination: Some(Zone::Graveyard),
            source_id: Some(ObjectId(100)),
            enter_tapped: false,
        };

        let mut events = Vec::new();
        let result = handle_resolution_choice(
            &mut state,
            waiting,
            GameAction::SelectCards {
                cards: vec![cards_on_top[0]],
            },
            &mut events,
        );

        assert!(
            result.is_err(),
            "a looked-at card that doesn't match the filter must be rejected when the filter matched nothing"
        );
        assert!(
            !state.players[0].hand.contains(&cards_on_top[0]),
            "rejected selection must not move the card to hand"
        );
        assert_eq!(
            state.players[0].library.iter().copied().collect::<Vec<_>>(),
            original_library,
            "rejected selection must not mutate the library"
        );
    }

    #[test]
    fn dig_choice_forwards_kept_cards_to_conditional_continuation() {
        use crate::game::engine_resolution_choices::{
            handle_resolution_choice, ResolutionChoiceOutcome,
        };
        use crate::types::actions::GameAction;
        use crate::types::game_state::PendingContinuation;

        let mut state = GameState::new_two_player(42);
        let kept = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Legendary Creature".to_string(),
            Zone::Library,
        );
        state
            .objects
            .get_mut(&kept)
            .unwrap()
            .card_types
            .supertypes
            .push(Supertype::Legendary);

        let other = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Other Creature".to_string(),
            Zone::Library,
        );
        let waiting = WaitingFor::DigChoice {
            player: PlayerId(0),
            library_owner: PlayerId(0),
            selectable_cards: vec![kept, other],
            cards: vec![kept, other],
            keep_count: 1,
            up_to: true,
            kept_destination: Some(Zone::Hand),
            rest_destination: Some(Zone::Library),
            source_id: Some(ObjectId(100)),
            enter_tapped: false,
        };
        let mut gain_life = ResolvedAbility::new(
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 3 },
                player: TargetFilter::Controller,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        gain_life.kind = AbilityKind::Spell;
        gain_life.condition = Some(AbilityCondition::TargetMatchesFilter {
            filter: TargetFilter::Typed(TypedFilter::default().properties(vec![
                FilterProp::HasSupertype {
                    value: Supertype::Legendary,
                },
            ])),
            use_lki: false,
            subject_slot: None,
        });
        state.pending_continuation = Some(PendingContinuation::new(Box::new(gain_life)));

        let mut events = Vec::new();
        let outcome = handle_resolution_choice(
            &mut state,
            waiting,
            GameAction::SelectCards { cards: vec![kept] },
            &mut events,
        )
        .expect("DigChoice resolution must succeed");

        assert!(matches!(outcome, ResolutionChoiceOutcome::WaitingFor(_)));
        assert_eq!(
            state.players[0].life, 23,
            "conditional continuation must evaluate against the selected card"
        );
    }

    #[test]
    fn dig_choice_marks_optional_context_from_kept_selection() {
        use crate::game::engine_resolution_choices::{
            handle_resolution_choice, ResolutionChoiceOutcome,
        };
        use crate::types::actions::GameAction;
        use crate::types::game_state::PendingContinuation;

        let mut state = GameState::new_two_player(42);
        let first = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Creature".to_string(),
            Zone::Library,
        );
        let second = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Spell".to_string(),
            Zone::Library,
        );
        let waiting = WaitingFor::DigChoice {
            player: PlayerId(0),
            library_owner: PlayerId(0),
            selectable_cards: vec![first],
            cards: vec![first, second],
            keep_count: 1,
            up_to: true,
            kept_destination: Some(Zone::Hand),
            rest_destination: Some(Zone::Library),
            source_id: Some(ObjectId(100)),
            enter_tapped: false,
        };
        let mut gain_life = ResolvedAbility::new(
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 3 },
                player: TargetFilter::Controller,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        gain_life.kind = AbilityKind::Spell;
        gain_life.condition = Some(AbilityCondition::Not {
            condition: Box::new(AbilityCondition::effect_performed()),
        });
        state.pending_continuation = Some(PendingContinuation::new(Box::new(gain_life)));

        let mut events = Vec::new();
        let outcome = handle_resolution_choice(
            &mut state,
            waiting,
            GameAction::SelectCards { cards: vec![] },
            &mut events,
        )
        .expect("DigChoice resolution must succeed");

        assert!(matches!(outcome, ResolutionChoiceOutcome::WaitingFor(_)));
        assert_eq!(
            state.players[0].life, 23,
            "declining an up-to Dig selection must satisfy Not(IfYouDo)"
        );
    }

    #[test]
    fn dig_choice_marks_optional_context_from_nonempty_selection() {
        use crate::game::engine_resolution_choices::{
            handle_resolution_choice, ResolutionChoiceOutcome,
        };
        use crate::types::actions::GameAction;
        use crate::types::game_state::PendingContinuation;

        let mut state = GameState::new_two_player(42);
        let first = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Creature".to_string(),
            Zone::Library,
        );
        let second = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Spell".to_string(),
            Zone::Library,
        );
        let waiting = WaitingFor::DigChoice {
            player: PlayerId(0),
            library_owner: PlayerId(0),
            selectable_cards: vec![first],
            cards: vec![first, second],
            keep_count: 1,
            up_to: true,
            kept_destination: Some(Zone::Hand),
            rest_destination: Some(Zone::Library),
            source_id: Some(ObjectId(100)),
            enter_tapped: false,
        };
        let mut gain_life = ResolvedAbility::new(
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 3 },
                player: TargetFilter::Controller,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        gain_life.kind = AbilityKind::Spell;
        gain_life.condition = Some(AbilityCondition::Not {
            condition: Box::new(AbilityCondition::effect_performed()),
        });
        state.pending_continuation = Some(PendingContinuation::new(Box::new(gain_life)));

        let mut events = Vec::new();
        let outcome = handle_resolution_choice(
            &mut state,
            waiting,
            GameAction::SelectCards { cards: vec![first] },
            &mut events,
        )
        .expect("DigChoice resolution must succeed");

        assert!(matches!(outcome, ResolutionChoiceOutcome::WaitingFor(_)));
        assert_eq!(
            state.players[0].life, 20,
            "keeping a card must make Not(IfYouDo) false"
        );
    }

    /// CR 107.3a + CR 601.2b: Dig's filter evaluation must flow through
    /// `FilterContext::from_ability`, so dynamic thresholds (e.g. `CmcLE { X }`)
    /// resolve against the caster's announced `chosen_x`. Bucket-B regression test
    /// for the filter-context migration — ensures Dig doesn't lose X resolution.
    #[test]
    fn dig_filter_resolves_x_against_chosen_x() {
        use crate::types::ability::{FilterProp, QuantityExpr, QuantityRef, TypedFilter};
        use crate::types::card_type::CoreType;
        use crate::types::mana::ManaCost;
        let mut state = GameState::new_two_player(42);
        // Build three creatures of different CMCs in the library.
        for (i, cmc) in [(1u64, 1u32), (2, 3), (3, 6)].into_iter() {
            let id = create_object(
                &mut state,
                CardId(i),
                PlayerId(0),
                format!("CMC {}", cmc),
                Zone::Library,
            );
            let obj = state.objects.get_mut(&id).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.mana_cost = ManaCost::generic(cmc);
        }

        let filter =
            TargetFilter::Typed(TypedFilter::creature().properties(vec![FilterProp::Cmc {
                comparator: crate::types::ability::Comparator::LE,
                value: QuantityExpr::Ref {
                    qty: QuantityRef::Variable {
                        name: "X".to_string(),
                    },
                },
            }]));
        let mut ability = ResolvedAbility::new(
            Effect::Dig {
                player: TargetFilter::Controller,
                count: QuantityExpr::Fixed { value: 3 },
                destination: None,
                keep_count: Some(1),
                keep_count_expr: None,
                up_to: false,
                filter,
                rest_destination: None,
                reveal: false,
                enter_tapped: false,
                source: DigSource::Library,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        ability.chosen_x = Some(3);

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        match &state.waiting_for {
            WaitingFor::DigChoice {
                selectable_cards, ..
            } => {
                // Selectable set should be exactly the CMC-1 and CMC-3 creatures.
                assert_eq!(selectable_cards.len(), 2);
            }
            other => panic!("Expected DigChoice, got {:?}", other),
        }
    }

    /// CR 608.2c + CR 701.20e: an unbounded ("put ALL", `keep_count == u32::MAX`,
    /// `up_to == false`) Dig with a concrete kept `destination` is deterministic —
    /// every filter-matching looked-at card is kept with NO `DigChoice` prompt
    /// (issue #2896). Here "put all creature cards from among the top three into
    /// your hand" must move exactly the two creatures to hand and bottom the
    /// non-matching card, surfacing no choice.
    #[test]
    fn dig_unbounded_exact_count_resolves_all_matching_no_choice() {
        use crate::types::card_type::CoreType;

        let mut state = GameState::new_two_player(42);
        let creature_a = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Creature A".to_string(),
            Zone::Library,
        );
        let creature_b = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Creature B".to_string(),
            Zone::Library,
        );
        let instant = create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "Instant".to_string(),
            Zone::Library,
        );
        for id in [creature_a, creature_b] {
            state
                .objects
                .get_mut(&id)
                .unwrap()
                .card_types
                .core_types
                .push(CoreType::Creature);
        }

        let ability = ResolvedAbility::new(
            Effect::Dig {
                player: TargetFilter::Controller,
                count: QuantityExpr::Fixed { value: 3 },
                destination: Some(Zone::Hand),
                keep_count: Some(u32::MAX),
                keep_count_expr: None,
                up_to: false,
                filter: TargetFilter::Typed(TypedFilter::creature()),
                rest_destination: Some(Zone::Library),
                reveal: false,
                enter_tapped: false,
                source: DigSource::Library,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(
            !matches!(state.waiting_for, WaitingFor::DigChoice { .. }),
            "a mass 'put all' Dig must not surface a DigChoice, got {:?}",
            state.waiting_for
        );
        // Both creatures kept to hand; the non-matching instant bottomed.
        for id in [creature_a, creature_b] {
            assert!(
                state.players[0].hand.contains(&id),
                "every matching creature must reach the hand"
            );
        }
        assert_eq!(
            state.objects[&instant].zone,
            Zone::Library,
            "the non-matching card must go to the library, not the graveyard"
        );
    }

    /// Runtime regression test for issue #4273 (Birthing Ritual). The parser
    /// now assembles: look-only Dig → Sacrifice → `from_prior_look` choice Dig.
    /// The choice Dig reads `state.private_look_ids` and evaluates the CMC
    /// filter AFTER the sacrifice snapshot is available in
    /// `effect_context_object`.
    ///
    /// CR 202.3 + CR 608.2c: the "where X is 1 plus the sacrificed creature's
    /// mana value" bound resolves against the sacrificed creature snapshot held
    /// in `ResolvedAbility.effect_context_object`. CR 701.20e: the look-only
    /// step populates `private_look_ids`; the PriorLook step reads it and
    /// presents WaitingFor::DigChoice with selectable_cards correctly filtered.
    #[test]
    fn birthing_ritual_runtime_dig_filter_respects_sacrificed_creature_mana_value() {
        use crate::parser::oracle_effect::parse_effect_chain;
        use crate::types::ability::{AbilityKind, CostPaidObjectSnapshot};
        use crate::types::card_type::CoreType;
        use crate::types::mana::ManaCost;

        // Parse the Birthing Ritual effect text and extract the from_prior_look
        // choice Dig — it is wired as Sacrifice.sub_ability in the new chain.
        let def = parse_effect_chain(
            "look at the top seven cards of your library. Then you may sacrifice a creature. \
             If you do, you may put a creature card with mana value X or less from among those \
             cards onto the battlefield, where X is 1 plus the sacrificed creature's mana value. \
             Put the rest on the bottom of your library in a random order.",
            AbilityKind::Spell,
        );
        // Chain: def(Dig, look-only) → sub(Sacrifice) → sub(Dig, PriorLook)
        let sac_def = def
            .sub_ability
            .as_deref()
            .expect("Dig must have Sacrifice as sub_ability");
        let choice_def = sac_def
            .sub_ability
            .as_deref()
            .expect("Sacrifice must have PriorLook Dig as sub_ability");
        assert!(
            matches!(
                &*choice_def.effect,
                Effect::Dig {
                    source: DigSource::PriorLook,
                    ..
                }
            ),
            "Sacrifice.sub_ability must be a PriorLook Dig, got {:?}",
            choice_def.effect
        );

        let mut state = GameState::new_two_player(42);

        // The creature being sacrificed lives on the battlefield with mana
        // value 3 — the bound becomes mana value ≤ 3 + 1 = 4.
        let sacrificed = create_object(
            &mut state,
            CardId(900),
            PlayerId(0),
            "Sacrificed Creature".into(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&sacrificed).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.mana_cost = ManaCost::generic(3);
        }
        let sac_snapshot = CostPaidObjectSnapshot {
            object_id: sacrificed,
            lki: state
                .objects
                .get(&sacrificed)
                .unwrap()
                .snapshot_for_mana_spent(),
        };

        // The looked-at set (normally populated by the look-only Dig): a
        // mana-value-4 creature (selectable, 4 ≤ 4) and a mana-value-5
        // creature (NOT selectable, 5 > 4). Place them in Library so
        // DigChoice's `library_owner` resolution still finds the player.
        let mv4 = create_object(
            &mut state,
            CardId(901),
            PlayerId(0),
            "MV4 Creature".into(),
            Zone::Library,
        );
        let mv5 = create_object(
            &mut state,
            CardId(902),
            PlayerId(0),
            "MV5 Creature".into(),
            Zone::Library,
        );
        for (id, cmc) in [(mv4, 4u64), (mv5, 5)] {
            let obj = state.objects.get_mut(&id).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.mana_cost = ManaCost::generic(cmc as u32);
        }

        // Simulate what the look-only Dig would have stored.
        state.private_look_ids = vec![mv4, mv5];
        state.private_look_player = Some(PlayerId(0));

        // Build ResolvedAbility from the PriorLook choice Dig, carrying
        // the sacrifice snapshot the runtime reads for the CMC bound.
        let mut ability = ResolvedAbility::new(
            (*choice_def.effect).clone(),
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        ability.effect_context_object = Some(sac_snapshot);

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        match &state.waiting_for {
            WaitingFor::DigChoice {
                selectable_cards,
                cards,
                kept_destination,
                ..
            } => {
                assert_eq!(
                    cards.len(),
                    2,
                    "both looked-at creatures appear in the DigChoice (CR 701.20e)"
                );
                assert_eq!(
                    selectable_cards,
                    &vec![mv4],
                    "only the mana-value-4 creature is ≤ (sacrificed MV 3 + 1)"
                );
                assert!(
                    !selectable_cards.contains(&mv5),
                    "the mana-value-5 creature exceeds the bound and is not selectable"
                );
                assert_eq!(
                    *kept_destination,
                    Some(Zone::Battlefield),
                    "the chosen creature is put onto the battlefield"
                );
            }
            other => panic!("Expected DigChoice, got {:?}", other),
        }
    }

    /// CR 608.2c + CR 701.20e: decline branch — when the player declines the
    /// optional sacrifice (Birthing Ritual), ALL looked-at cards must go to the
    /// bottom of the library via the `PriorLook` Dig with `keep_count=0`
    /// wired as the choice Dig's `else_ability`. No WaitingFor::DigChoice is
    /// surfaced.
    #[test]
    fn birthing_ritual_decline_sacrifice_puts_all_looked_at_cards_on_bottom() {
        use crate::parser::oracle_effect::parse_effect_chain;
        use crate::types::ability::AbilityKind;

        let def = parse_effect_chain(
            "look at the top seven cards of your library. Then you may sacrifice a creature. \
             If you do, you may put a creature card with mana value X or less from among those \
             cards onto the battlefield, where X is 1 plus the sacrificed creature's mana value. \
             Put the rest on the bottom of your library in a random order.",
            AbilityKind::Spell,
        );
        let sac_def = def.sub_ability.as_deref().unwrap();
        let choice_def = sac_def.sub_ability.as_deref().unwrap();
        // The decline branch is the choice Dig's else_ability.
        let decline_def = choice_def
            .else_ability
            .as_deref()
            .expect("choice Dig must have else_ability (decline: all on bottom)");
        assert!(
            matches!(
                &*decline_def.effect,
                Effect::Dig {
                    source: DigSource::PriorLook,
                    keep_count: Some(0),
                    ..
                }
            ),
            "else_ability must be a PriorLook Dig with keep_count=0, got {:?}",
            decline_def.effect
        );

        let mut state = GameState::new_two_player(42);

        // Place two "looked at" cards at the library top and pre-populate
        // private_look_ids so the decline-branch Dig sees them.
        let card_a = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "CardA".into(),
            Zone::Library,
        );
        let card_b = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "CardB".into(),
            Zone::Library,
        );
        state.private_look_ids = vec![card_a, card_b];
        state.private_look_player = Some(PlayerId(0));

        let ability = ResolvedAbility::new(
            (*decline_def.effect).clone(),
            vec![],
            ObjectId(100),
            PlayerId(0),
        );

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        // No interactive choice must be surfaced.
        assert!(
            !matches!(state.waiting_for, WaitingFor::DigChoice { .. }),
            "decline branch must not surface a DigChoice"
        );
        // Both cards must be at the library bottom (last positions).
        let lib = &state.players[0].library;
        assert!(
            lib.last() == Some(&card_a) || lib.last() == Some(&card_b),
            "declined looked-at cards must be at library bottom, got {lib:?}"
        );
        // private_look_ids is cleared after the decline branch.
        assert!(
            state.private_look_ids.is_empty(),
            "private_look_ids must be cleared after decline-branch routing"
        );
    }

    /// CR 201.2 + CR 201.2a: `FilterProp::NameMatchesAnyPermanent` must restrict
    /// the Dig's selectable set to library cards whose printed name equals the
    /// name of some permanent on the battlefield. Controllers of the on-board
    /// permanents don't matter when `controller = None` — any permanent
    /// anywhere on the battlefield counts. This is the Mitotic Manipulation
    /// primitive: `filter = NameMatchesAnyPermanent { controller: None }`.
    #[test]
    fn dig_with_name_matches_any_permanent_filter() {
        use crate::types::ability::{ControllerRef, FilterProp, QuantityExpr, TypedFilter};
        let mut state = GameState::new_two_player(42);
        // Library has three cards: "Forest", "Goblin", "Island".
        for (i, name) in ["Forest", "Goblin", "Island"].iter().enumerate() {
            create_object(
                &mut state,
                CardId(i as u64 + 1),
                PlayerId(0),
                (*name).into(),
                Zone::Library,
            );
        }
        // Opponent controls a "Forest" permanent on the battlefield; controller
        // doesn't matter when controller=None.
        create_object(
            &mut state,
            CardId(100),
            PlayerId(1),
            "Forest".into(),
            Zone::Battlefield,
        );

        let filter = TargetFilter::Typed(TypedFilter::default().properties(vec![
            FilterProp::NameMatchesAnyPermanent { controller: None },
        ]));
        let ability = ResolvedAbility::new(
            Effect::Dig {
                player: TargetFilter::Controller,
                count: QuantityExpr::Fixed { value: 3 },
                destination: Some(Zone::Battlefield),
                keep_count: Some(1),
                keep_count_expr: None,
                up_to: true,
                filter: filter.clone(),
                rest_destination: Some(Zone::Library),
                reveal: false,
                enter_tapped: false,
                source: DigSource::Library,
            },
            vec![],
            ObjectId(200),
            PlayerId(0),
        );

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        match &state.waiting_for {
            WaitingFor::DigChoice {
                selectable_cards,
                cards,
                kept_destination,
                rest_destination,
                ..
            } => {
                assert_eq!(cards.len(), 3, "all 3 library cards are revealed");
                assert_eq!(
                    selectable_cards.len(),
                    1,
                    "only Forest matches an on-battlefield permanent"
                );
                let forest_obj = state
                    .objects
                    .get(&selectable_cards[0])
                    .expect("selectable object exists");
                assert_eq!(forest_obj.name, "Forest");
                assert_eq!(*kept_destination, Some(Zone::Battlefield));
                assert_eq!(*rest_destination, Some(Zone::Library));
            }
            other => panic!("Expected DigChoice, got {:?}", other),
        }

        // Verify the controller-scoped variant: with controller=You, the filter
        // only matches permanents controlled by the ability's controller. The
        // on-board "Forest" is controlled by PlayerId(1), so no library card
        // should match.
        let filter_you = TargetFilter::Typed(TypedFilter::default().properties(vec![
            FilterProp::NameMatchesAnyPermanent {
                controller: Some(ControllerRef::You),
            },
        ]));
        let ability_you = ResolvedAbility::new(
            Effect::Dig {
                player: TargetFilter::Controller,
                count: QuantityExpr::Fixed { value: 3 },
                destination: Some(Zone::Battlefield),
                keep_count: Some(1),
                keep_count_expr: None,
                up_to: true,
                filter: filter_you,
                rest_destination: Some(Zone::Library),
                reveal: false,
                enter_tapped: false,
                source: DigSource::Library,
            },
            vec![],
            ObjectId(201),
            PlayerId(0),
        );
        let mut events2 = Vec::new();
        resolve(&mut state, &ability_you, &mut events2).unwrap();
        match &state.waiting_for {
            WaitingFor::DigChoice {
                selectable_cards, ..
            } => {
                assert_eq!(
                    selectable_cards.len(),
                    0,
                    "no library card shares a name with a permanent you control"
                );
            }
            other => panic!("Expected DigChoice, got {:?}", other),
        }
    }

    /// CR 608.2c + CR 701.20e: Dig with `destination = Some(Battlefield)` and
    /// `rest_destination = Some(Library)` must route the chosen card to the
    /// battlefield (ETB triggers fire) and the unchosen cards to the bottom of
    /// the owner's library. This is the Mitotic Manipulation primitive at
    /// resolution time — no sub_ability chain required.
    #[test]
    fn dig_resolves_kept_to_battlefield_and_rest_to_library_bottom() {
        use crate::game::engine_resolution_choices::{
            handle_resolution_choice, ResolutionChoiceOutcome,
        };
        use crate::types::actions::GameAction;
        let mut state = GameState::new_two_player(42);
        for i in 0..5 {
            create_object(
                &mut state,
                CardId(i + 1),
                PlayerId(0),
                format!("Card {}", i),
                Zone::Library,
            );
        }
        let cards_on_top: Vec<_> = state.players[0]
            .library
            .iter()
            .take(5)
            .copied()
            .collect::<Vec<_>>();
        let kept = vec![cards_on_top[2]]; // pick the middle card
        let rest_ids: Vec<_> = cards_on_top
            .iter()
            .filter(|id| !kept.contains(id))
            .copied()
            .collect();

        let waiting = WaitingFor::DigChoice {
            player: PlayerId(0),
            library_owner: PlayerId(0),
            selectable_cards: cards_on_top.clone(),
            cards: cards_on_top.clone(),
            keep_count: 1,
            up_to: true,
            kept_destination: Some(Zone::Battlefield),
            rest_destination: Some(Zone::Library),
            source_id: Some(ObjectId(100)),
            enter_tapped: false,
        };
        let action = GameAction::SelectCards {
            cards: kept.clone(),
        };
        let mut events = Vec::new();
        let outcome =
            handle_resolution_choice(&mut state, waiting, action, &mut events).expect("ok");
        assert!(matches!(outcome, ResolutionChoiceOutcome::WaitingFor(_)));

        // Kept card is on the battlefield.
        let kept_obj = state.objects.get(&kept[0]).expect("kept object exists");
        assert_eq!(kept_obj.zone, Zone::Battlefield);
        // Rest of the cards are at the bottom of PlayerId(0)'s library.
        let library = &state.players[0].library;
        let bottom: Vec<_> = library
            .iter()
            .rev()
            .take(rest_ids.len())
            .rev()
            .copied()
            .collect();
        for id in &rest_ids {
            assert!(
                bottom.contains(id),
                "card {:?} must be at library bottom",
                id
            );
            let obj = state.objects.get(id).expect("rest object exists");
            assert_eq!(obj.zone, Zone::Library);
        }
    }

    /// Issue #2896 (Muxus, Goblin Grandee). CR 608.2c + CR 701.20a/701.20e: a
    /// mass "put ALL <filter> from among them onto the battlefield and the rest
    /// on the bottom of your library in a random order" Dig is a *deterministic*
    /// instruction — there is no "choose" step. The resolver must put every
    /// filter-matching looked-at card onto the battlefield with NO `DigChoice`
    /// prompt, and bottom the non-matching cards (NOT graveyard).
    ///
    /// Pre-fix this surfaced a `WaitingFor::DigChoice` (forcing a player/AI
    /// selection — the reported "made to choose one Goblin" bug) and routed the
    /// rest pile to the graveyard because the parser left `rest_destination =
    /// None`. This test drives the real parser-produced AST through the real
    /// resolver, so it discriminates both defects at once.
    #[test]
    fn muxus_mass_put_all_resolves_deterministically_no_choice() {
        use crate::parser::oracle_effect::parse_effect_chain;
        use crate::types::ability::AbilityKind;
        use crate::types::card_type::CoreType;
        use crate::types::mana::ManaCost;

        // Parse Muxus's ETB effect text (the portion after the trigger prefix).
        let def = parse_effect_chain(
            "reveal the top six cards of your library. Put all Goblin creature cards \
             with mana value 5 or less from among them onto the battlefield and the rest \
             on the bottom of your library in a random order.",
            AbilityKind::Spell,
        );

        // The parser must yield a mass-put Dig: all matching Goblins to the
        // battlefield, rest to the bottom of the library.
        match &*def.effect {
            Effect::Dig {
                keep_count,
                up_to,
                destination,
                rest_destination,
                ..
            } => {
                assert_eq!(
                    *keep_count,
                    Some(u32::MAX),
                    "'put all' must lower to the unbounded keep sentinel"
                );
                assert!(!*up_to, "'put all' is not an up-to choice");
                assert_eq!(*destination, Some(Zone::Battlefield));
                assert_eq!(
                    *rest_destination,
                    Some(Zone::Library),
                    "the in-clause 'and the rest on the bottom' rider must set rest=Library, \
                     not fall through to the graveyard default"
                );
            }
            other => panic!("expected a Dig effect, got {other:?}"),
        }

        let mut state = GameState::new_two_player(42);

        // Library top six: two Goblin creatures with mana value <= 5 (matching)
        // and four non-matching cards.
        let goblin_a = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Goblin A".into(),
            Zone::Library,
        );
        let goblin_b = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Goblin B".into(),
            Zone::Library,
        );
        for id in [goblin_a, goblin_b] {
            let obj = state.objects.get_mut(&id).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.card_types.subtypes.push("Goblin".to_string());
            obj.mana_cost = ManaCost::generic(3);
        }
        let rest: Vec<_> = (0..4)
            .map(|i| {
                create_object(
                    &mut state,
                    CardId(10 + i),
                    PlayerId(0),
                    format!("Rest {i}"),
                    Zone::Library,
                )
            })
            .collect();

        let ability =
            ResolvedAbility::new((*def.effect).clone(), vec![], ObjectId(100), PlayerId(0));
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        // The discriminating assertion: NO player choice is surfaced.
        assert!(
            !matches!(state.waiting_for, WaitingFor::DigChoice { .. }),
            "a mass 'put all' Dig must not surface a DigChoice prompt, got {:?}",
            state.waiting_for
        );

        // Both Goblins are on the battlefield.
        for id in [goblin_a, goblin_b] {
            assert_eq!(
                state.objects[&id].zone,
                Zone::Battlefield,
                "every matching Goblin must enter the battlefield with no choice"
            );
        }

        // Every non-matching card is on the bottom of the library — NOT the graveyard.
        let library: Vec<_> = state.players[0].library.iter().copied().collect();
        for id in &rest {
            assert_eq!(
                state.objects[id].zone,
                Zone::Library,
                "a non-matching revealed card must go to the library, not the graveyard"
            );
            assert!(
                library.contains(id),
                "non-matching card {id:?} must remain in the library"
            );
        }
        let bottom: Vec<_> = library
            .iter()
            .rev()
            .take(rest.len())
            .rev()
            .copied()
            .collect();
        for id in &rest {
            assert!(
                bottom.contains(id),
                "non-matching card {id:?} must be at the bottom of the library"
            );
        }
    }

    /// Issue #738 (Consult the Star Charts): "Look at the top X cards of your
    /// library, where X is the number of lands you control. Put one of those
    /// cards into your hand. If this spell was kicked, put two of those cards
    /// into your hand instead. Put the rest on the bottom of your library in
    /// a random order." Parses the real Oracle text through the live parser
    /// (`parse_effect_chain` — same entry point `birthing_ritual_runtime_dig_
    /// filter_respects_sacrificed_creature_mana_value` uses) so the test tracks
    /// the actual parser output rather than a hand-authored AST, and asserts
    /// the shape: a top-level `Dig` (kicked, keep_count 2) gated by
    /// `AbilityCondition::AdditionalCostPaidInstead`, with an `else_ability`
    /// carrying the unkicked `Dig` (keep_count 1) — see `resolve_ability_
    /// chain`'s top-level-condition + `else_ability` fallback (CR 608.2c).
    fn consult_the_star_charts_ability(
        controller: crate::types::player::PlayerId,
        source: ObjectId,
        kicked: bool,
    ) -> ResolvedAbility {
        let def = parse_effect_chain(
            "Look at the top X cards of your library, where X is the number of lands you \
             control. Put one of those cards into your hand. If this spell was kicked, put \
             two of those cards into your hand instead. Put the rest on the bottom of your \
             library in a random order.",
            AbilityKind::Spell,
        );
        assert!(
            matches!(
                &*def.effect,
                Effect::Dig {
                    keep_count: Some(2),
                    ..
                }
            ),
            "parser must assemble the kicked Dig (keep_count 2) as the root effect, got {:?}",
            def.effect
        );
        assert_eq!(
            def.condition,
            Some(AbilityCondition::AdditionalCostPaidInstead),
            "the root Dig must be gated on AdditionalCostPaidInstead"
        );
        match def.else_ability.as_deref() {
            Some(else_def) => assert!(
                matches!(
                    &*else_def.effect,
                    Effect::Dig {
                        keep_count: Some(1),
                        ..
                    }
                ),
                "else_ability must carry the unkicked Dig (keep_count 1), got {:?}",
                else_def.effect
            ),
            None => panic!("parser must produce an else_ability for the unkicked branch"),
        }

        let mut ability = ResolvedAbility::new((*def.effect).clone(), vec![], source, controller);
        ability.condition = def.condition.clone();
        ability.else_ability = def.else_ability.as_deref().map(|e| {
            Box::new(ResolvedAbility::new(
                (*e.effect).clone(),
                vec![],
                source,
                controller,
            ))
        });
        ability.context = SpellContext {
            additional_cost_paid: kicked,
            ..Default::default()
        };
        ability
    }

    fn run_consult_the_star_charts(kicked: bool) -> (GameState, usize) {
        let mut state = GameState::new_two_player(42);
        let source = ObjectId(100);
        let controller = PlayerId(0);

        for i in 0..3 {
            let land = create_object(
                &mut state,
                CardId(i + 1),
                controller,
                format!("Land {i}"),
                Zone::Battlefield,
            );
            state
                .objects
                .get_mut(&land)
                .unwrap()
                .card_types
                .core_types
                .push(CoreType::Land);
        }
        for i in 0..5 {
            create_object(
                &mut state,
                CardId(i + 10),
                controller,
                format!("Library Card {i}"),
                Zone::Library,
            );
        }

        let ability = consult_the_star_charts_ability(controller, source, kicked);
        let mut events = Vec::new();
        crate::game::effects::resolve_ability_chain(&mut state, &ability, &mut events, 0)
            .expect("Consult the Star Charts resolution must succeed");

        let (selectable_cards, keep_count) = match &state.waiting_for {
            WaitingFor::DigChoice {
                selectable_cards,
                keep_count,
                ..
            } => (selectable_cards.clone(), *keep_count),
            other => panic!("expected DigChoice, got {other:?}"),
        };
        let chosen: Vec<_> = selectable_cards.into_iter().take(keep_count).collect();
        let waiting = state.waiting_for.clone();
        let outcome = handle_resolution_choice(
            &mut state,
            waiting,
            GameAction::SelectCards { cards: chosen },
            &mut events,
        )
        .expect("DigChoice resolution must succeed");
        assert!(matches!(outcome, ResolutionChoiceOutcome::WaitingFor(_)));

        (state, keep_count)
    }

    #[test]
    fn consult_the_star_charts_unkicked_puts_one_card_in_hand() {
        let (state, keep_count) = run_consult_the_star_charts(false);
        assert_eq!(keep_count, 1, "unkicked must keep exactly 1 card");
        assert_eq!(
            state.players[0].hand.len(),
            1,
            "unkicked Consult the Star Charts must put exactly 1 card into hand"
        );
    }

    #[test]
    fn consult_the_star_charts_kicked_puts_two_cards_in_hand() {
        let (state, keep_count) = run_consult_the_star_charts(true);
        assert_eq!(keep_count, 2, "kicked must keep exactly 2 cards");
        assert_eq!(
            state.players[0].hand.len(),
            2,
            "kicked Consult the Star Charts must put exactly 2 cards into hand"
        );
    }

    /// Issue #1365 (Thassa's Oracle reanimated via Dread Return with an empty
    /// library — Hermit Druid milled the whole deck first). CR 401.5: a Dig
    /// against an empty library looks at zero cards. The chained "put up to
    /// one of them on top … the rest on the bottom" instruction has nothing to
    /// place, but the trailing `WinTheGame` gate (devotion >= library size)
    /// must still evaluate against the UNDISTURBED game state — Thassa's
    /// Oracle must still be on the battlefield and the library must still be
    /// empty when the condition is checked.
    ///
    /// Pre-fix, the `PutAtLibraryPosition` link's `ParentTarget` resolution
    /// fell back to "self" (no prior Dig selection, empty `ability.targets`),
    /// moving Thassa's Oracle itself from the battlefield onto its own
    /// library — corrupting devotion (no longer on the battlefield) and the
    /// library count (now 1, not 0) before `WinTheGame`'s condition evaluated.
    #[test]
    fn thassas_oracle_dread_return_empty_library_still_wins() {
        let def = parse_effect_chain(
            "Look at the top X cards of your library, where X is your devotion to blue. \
             Put up to one of them on top of your library and the rest on the bottom of \
             your library in a random order. If X is greater than or equal to the number \
             of cards in your library, you win the game.",
            AbilityKind::Spell,
        );

        let mut state = GameState::new_two_player(42);
        assert!(state.players[0].library.is_empty(), "library milled to 0");

        // Thassa's Oracle reanimated onto the battlefield (e.g. via Dread
        // Return) — its own {1}{U} cost contributes 1 to devotion to blue.
        let oracle = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Thassa's Oracle".to_string(),
            Zone::Battlefield,
        );
        state.objects.get_mut(&oracle).unwrap().mana_cost = ManaCost::Cost {
            shards: vec![ManaCostShard::Blue],
            generic: 1,
        };

        let ability =
            crate::game::ability_utils::build_resolved_from_def(&def, oracle, PlayerId(0));
        let mut events = Vec::new();
        crate::game::effects::resolve_ability_chain(&mut state, &ability, &mut events, 0)
            .expect("Thassa's Oracle ETB chain must resolve");

        assert_eq!(
            state.objects[&oracle].zone,
            Zone::Battlefield,
            "Thassa's Oracle itself must NOT be moved into the library — there was \
             nothing looked at to place"
        );
        assert!(
            state.eliminated_players.contains(&PlayerId(1)),
            "devotion (1) >= library size (0) must win the game (CR 104.2b)"
        );
    }
}
