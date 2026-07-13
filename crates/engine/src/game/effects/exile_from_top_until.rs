use crate::game::filter::{matches_target_filter, FilterContext};
use crate::game::quantity::resolve_quantity;
use crate::types::ability::{
    Effect, EffectError, EffectKind, ObjectProperty, ResolvedAbility, TargetRef, UntilCondition,
};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;
use crate::types::identifiers::ObjectId;
use crate::types::zones::Zone;

/// CR 701.13a + CR 608.2c: Exile cards from the top of the acting player's
/// library one at a time until the typed `until` predicate is satisfied.
///
/// The `UntilCondition` axis selects between two stop-condition families:
///
/// * `NextMatches(filter)` — Etali / Cascade / Discover-shape (CR 701.57a /
///   CR 702.85a). The just-exiled card is checked against the filter; the loop
///   ends on the first match. The hit `ObjectId` is injected into the
///   sub_ability chain as a target so downstream "cast that card" / "put it
///   onto the battlefield" sub-effects can address it.
///
/// * `CumulativeThreshold { property, comparator, threshold }` — Tasha's
///   Hideous Laughter / Dream Harvest / Improvisation Capstone (CR 202.3 +
///   CR 107.3e). Every exiled card contributes `property` (mana value /
///   power / toughness) to a running sum; the loop ends as soon as the
///   comparator vs the threshold is satisfied. No hit card is injected — the
///   sub_ability chain (if any) sees the per-resolution exile-link channel
///   via `TargetFilter::ExiledBySource` (Improvisation Capstone's "you may
///   cast any number of spells from among them").
///
/// In both modes:
///
/// * If the library is exhausted without satisfying the predicate, every card
///   in the library is exiled and the loop terminates naturally. For
///   `NextMatches`, the "cast that card" link is skipped (no hit to inject) —
///   but any exile-cleanup link that references `ExiledBySource` (Jodah, the
///   Unifier's "put the rest on the bottom") STILL runs, so the exiled pile is
///   never stranded (CR 608.2c + CR 701.13a; Jodah ruling: with no hit, all
///   exiled cards are put on the bottom in a random order). For
///   `CumulativeThreshold`, the sub_ability chain still runs because the
///   per-resolution exile links are independently meaningful.
///
/// * CR 400.7 + CR 406.6: Each exiled card is recorded in `state.exile_links`
///   with `ExileLinkKind::TrackedBySource` so downstream effects can reference
///   "cards exiled this way" via `TargetFilter::ExiledBySource`.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let (player_filter, until) = match &ability.effect {
        Effect::ExileFromTopUntil { player, until } => (player, until),
        _ => return Err(EffectError::MissingParam("until".to_string())),
    };

    // CR 109.5 + CR 608.2c: "your library" lowers to `TargetFilter::Controller`
    // (the ability's controller). Mirror `exile_top::resolve` — do not consult
    // `scoped_player`, which carries event-bound players on combat-damage triggers
    // (The Infamous Cruelclaw, issue #2881) and would exile from the wrong library.
    // Per-iteration "their library" uses `TargetFilter::ScopedPlayer` or a typed
    // `ControllerRef::ScopedPlayer` filter instead.
    let acting_player = super::resolve_player_for_context_ref(state, ability, player_filter);
    let player = state
        .players
        .iter()
        .find(|p| p.id == acting_player)
        .ok_or(EffectError::PlayerNotFound)?;

    // Snapshot library (top = index 0) to iterate without borrow conflicts.
    let library: Vec<ObjectId> = player.library.iter().copied().collect();

    // CR 107.3a + CR 601.2b: ability-context evaluation so dynamic thresholds
    // resolve against the resolving ability's `chosen_x`.
    let ctx = FilterContext::from_ability(ability);

    // For `CumulativeThreshold`, resolve the threshold once up-front so X /
    // dynamic refs read from the same context the ability is resolving in.
    let threshold_value: Option<i32> = match until {
        UntilCondition::NextMatches { .. } => None,
        UntilCondition::CumulativeThreshold { threshold, .. } => Some(resolve_quantity(
            state,
            threshold,
            ability.controller,
            ability.source_id,
        )),
    };

    let mut hit_id: Option<ObjectId> = None;
    let mut cumulative: i32 = 0;
    let track_exiled_by_source =
        crate::game::exile_links::should_track_exiled_by_source(state, ability.source_id, ability);

    for &obj_id in &library {
        // CR 701.13a: Exile the card through the shared zone-change pipeline so
        // replacement effects, exile links, and zone bookkeeping stay identical
        // to `Effect::ChangeZone`.
        match super::change_zone::execute_zone_move(
            state,
            obj_id,
            Zone::Library,
            Zone::Exile,
            ability.source_id,
            ability.duration.as_ref(),
            false,
            crate::types::zones::EtbTapState::Unspecified,
            None,
            &[],
            None,
            track_exiled_by_source,
            None,
            None,
            events,
        ) {
            super::change_zone::ZoneMoveResult::Done => {}
            super::change_zone::ZoneMoveResult::NeedsChoice(player) => {
                state.waiting_for =
                    crate::game::replacement::replacement_choice_waiting_for(player, state);
                return Ok(());
            }
            super::change_zone::ZoneMoveResult::NeedsAuraAttachmentChoice => return Ok(()),
        }

        match until {
            UntilCondition::NextMatches { filter } => {
                // CR 701.57a / 702.85a: Stop on the first card matching the
                // filter; expose it to the sub_ability chain.
                if matches_target_filter(state, obj_id, filter, &ctx) {
                    hit_id = Some(obj_id);
                    break;
                }
            }
            UntilCondition::CumulativeThreshold {
                property,
                comparator,
                ..
            } => {
                // CR 202.3 + CR 107.3e: Add this card's contribution and stop
                // once the running sum satisfies the comparator vs threshold.
                cumulative = cumulative.saturating_add(extract_property(state, obj_id, *property));
                if comparator.evaluate(
                    cumulative,
                    threshold_value.expect("threshold resolved for cumulative branch"),
                ) {
                    break;
                }
            }
        }
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::ExileFromTopUntil,
        source_id: ability.source_id,
        subject: None,
    });

    // CR 400.7: An object that moves from one zone to another becomes a new
    // object. Sub-ability chaining differs per stop-condition kind:
    //
    // * NextMatches: inject the hit card as the sub-ability's target so
    //   "cast that card" / "you may put it onto the battlefield" address the
    //   right object. If no hit was found, the "cast that card" link is
    //   skipped, but any downstream exile-cleanup link that references
    //   `ExiledBySource` (Jodah's "put the rest on the bottom") still runs so
    //   the exiled pile is not stranded (CR 608.2c + CR 701.13a).
    //
    // * CumulativeThreshold: there is no single hit card. The sub_ability
    //   chain (if any) reads from `TargetFilter::ExiledBySource` to address
    //   the whole exiled set (Improvisation Capstone, Dream Harvest). Run it
    //   with the original target list intact.
    if let Some(ref sub) = ability.sub_ability {
        match until {
            UntilCondition::NextMatches { .. } => {
                if let Some(hit) = hit_id {
                    let mut sub_clone = sub.as_ref().clone();
                    // CR 608.2c + CR 610.3: Sub-ability target injection is conditional
                    // on the sub-ability's effect filter:
                    //
                    // * Filter references `ExiledBySource` (Etali Primal Conqueror's
                    //   "cast any number of spells from among the nonland cards exiled
                    //   this way", Improvisation Capstone class) — leave `targets`
                    //   untouched (parser-emitted, empty) so `cast_from_zone::resolve`
                    //   enumerates the full per-resolution exile-link set via
                    //   `linked_exile_cards_for_source`. Pre-binding the single hit
                    //   here would limit the offer to one card per player-iteration
                    //   rather than the union across players.
                    //
                    // * Filter does NOT reference `ExiledBySource` (Chaos Wand /
                    //   Fallen Shinobi "cast that card" with `ParentTarget`, "put N
                    //   counters on it" `PutCounter { target: ParentTarget }`,
                    //   Cascade-shape "put it onto the battlefield") — inject the hit
                    //   as the parent target so anaphoric `ParentTarget` / `SelfRef`
                    //   references resolve to the just-exiled card.
                    if !sub_effect_references_exiled_by_source(&sub_clone) {
                        sub_clone.targets = vec![TargetRef::Object(hit)];
                    }
                    sub_clone.context = ability.context.clone();
                    super::resolve_ability_chain(state, &sub_clone, events, 1)?;
                } else if let Some(cleanup) = first_exiled_by_source_link(sub.as_ref()) {
                    // CR 608.2c + CR 701.13a: Library exhausted with no hit
                    // (Jodah, the Unifier — no legendary nonland with lesser
                    // mana value). The "cast that card" link has nothing to
                    // address and is skipped, but the exile-cleanup link that
                    // references `ExiledBySource` ("put the rest on the bottom
                    // of your library in a random order") MUST still run — its
                    // pool is every card this ability exiled. Without this the
                    // whole exiled pile is stranded in exile and the acting
                    // player is decked out by the engine (issue #5277).
                    //
                    // Jodah ruling: with no hit, all exiled cards return. The
                    // cleanup's `DistinctFrom { ParentTarget }` leg fails open
                    // when there are no object targets, so clearing `targets`
                    // here lets every exiled card be swept to the bottom.
                    let mut cleanup_clone = cleanup.clone();
                    cleanup_clone.targets = vec![];
                    cleanup_clone.context = ability.context.clone();
                    super::resolve_ability_chain(state, &cleanup_clone, events, 1)?;
                }
            }
            UntilCondition::CumulativeThreshold { .. } => {
                let mut sub_clone = sub.as_ref().clone();
                sub_clone.context = ability.context.clone();
                super::resolve_ability_chain(state, &sub_clone, events, 1)?;
            }
        }
    }

    Ok(())
}

/// CR 608.2c: Decide whether the sub-ability's effect filter forwards the
/// per-resolution single hit (`ParentTarget` / `SelfRef` consumers) or the
/// whole tracked-exile set (`ExiledBySource` consumers). Delegates to
/// `extract_target_filter_from_effect` so target-filter extraction stays
/// in lockstep with the canonical accessor used by stack/trigger code.
fn sub_effect_references_exiled_by_source(sub: &ResolvedAbility) -> bool {
    crate::game::triggers::extract_target_filter_from_effect(&sub.effect)
        .map(crate::types::ability::TargetFilter::references_exiled_by_source)
        .unwrap_or(false)
}

/// CR 608.2c + CR 701.13a: True when an effect's RAW target filter references
/// `ExiledBySource`. Distinct from [`sub_effect_references_exiled_by_source`],
/// which routes through `extract_target_filter_from_effect` — that accessor
/// strips context refs (and `ExiledBySource` IS a context ref via
/// `is_context_ref`), so it returns `None` for the very cleanup this walk is
/// looking for. Read the un-stripped `Effect::target_filter` instead.
fn effect_target_references_exiled_by_source(sub: &ResolvedAbility) -> bool {
    sub.effect
        .target_filter()
        .is_some_and(crate::types::ability::TargetFilter::references_exiled_by_source)
}

/// CR 608.2c + CR 701.13a: Walk a `NextMatches` sub-chain for the FIRST link
/// whose effect references `ExiledBySource` — the exile-cleanup ("put the rest
/// on the bottom") that must still run when the library was exhausted with no
/// hit. The cast link (Jodah's "cast that card" via `ParentTarget`) is skipped
/// because it references no exiled-set channel; its cleanup lives one link
/// deeper as its `sub_ability` (mainline chain) or `else_ability` (the declined
/// path the parser also wires). Descends both edges so either wiring is found.
fn first_exiled_by_source_link(sub: &ResolvedAbility) -> Option<&ResolvedAbility> {
    if effect_target_references_exiled_by_source(sub) {
        return Some(sub);
    }
    if let Some(next) = sub.sub_ability.as_deref() {
        if let Some(found) = first_exiled_by_source_link(next) {
            return Some(found);
        }
    }
    if let Some(alt) = sub.else_ability.as_deref() {
        if let Some(found) = first_exiled_by_source_link(alt) {
            return Some(found);
        }
    }
    None
}

/// CR 202.3 / CR 208 / CR 209: Look up the requested measurable property of an
/// exiled object. Mirrors the per-property dispatch used by
/// `quantity::resolve_quantity`'s aggregate-of-objects branch so both
/// callers compute the same number for the same card.
fn extract_property(state: &GameState, obj_id: ObjectId, property: ObjectProperty) -> i32 {
    let Some(obj) = state.objects.get(&obj_id) else {
        return 0;
    };
    match property {
        ObjectProperty::Power => obj.power.unwrap_or(0),
        // CR 202.3d + CR 202.3e + CR 709.4b: combined MV for a split card off the
        // stack (X treated as 0 off the stack, chosen half on the stack).
        ObjectProperty::ManaValue => i32::try_from(obj.effective_mana_value()).unwrap_or(i32::MAX),
        ObjectProperty::Toughness => obj.toughness.unwrap_or(0),
        // CR 107.4a + CR 202.1: colored mana symbols of `color` in the cost.
        ObjectProperty::ManaSymbolCount(color) => i32::try_from(
            crate::game::devotion::count_cost_color_symbols(&obj.mana_cost, color),
        )
        .unwrap_or(i32::MAX),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::engine::apply;
    use crate::game::zones::create_object;
    use crate::types::ability::{
        AbilityDefinition, AbilityKind, CardPlayMode, CastFromZoneDriver, CastingPermission,
        Comparator, ControllerRef, FilterProp, LibraryPosition, PlayerFilter, QuantityExpr,
        ReplacementDefinition, ResolvedAbility, SubAbilityLink, TargetFilter, TargetRef,
        TypeFilter, TypedFilter,
    };
    use crate::types::actions::GameAction;
    use crate::types::card_type::{CoreType, Supertype};
    use crate::types::format::FormatConfig;
    use crate::types::identifiers::CardId;
    use crate::types::mana::ManaCost;
    use crate::types::player::PlayerId;
    use crate::types::replacements::ReplacementEvent;

    /// Helper: set up a card in a player's library with the given core type.
    fn add_library_card(
        state: &mut GameState,
        owner: PlayerId,
        name: &str,
        is_land: bool,
    ) -> ObjectId {
        let card_id = CardId(state.next_object_id);
        let id = create_object(state, card_id, owner, name.to_string(), Zone::Library);
        let obj = state.objects.get_mut(&id).unwrap();
        if is_land {
            obj.card_types.core_types.push(CoreType::Land);
        } else {
            obj.card_types.core_types.push(CoreType::Creature);
        }
        id
    }

    fn nonland_filter() -> TargetFilter {
        TargetFilter::Typed(
            TypedFilter::default().with_type(TypeFilter::Non(Box::new(TypeFilter::Land))),
        )
    }

    fn instant_or_sorcery_filter() -> TargetFilter {
        TargetFilter::Or {
            filters: vec![
                TargetFilter::Typed(TypedFilter::new(TypeFilter::Instant)),
                TargetFilter::Typed(TypedFilter::new(TypeFilter::Sorcery)),
            ],
        }
    }

    /// CR 701.57a + CR 702.85a: When the iterator hits a nonland, it stops and
    /// reports the hit. This bare effect has no linked-exile consumer, so it
    /// moves cards to exile without adding source display links.
    #[test]
    fn exiles_lands_then_stops_at_nonland_without_links_without_consumer() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(100),
            PlayerId(0),
            "Etali, Primal Conqueror".to_string(),
            Zone::Battlefield,
        );

        let land1 = add_library_card(&mut state, PlayerId(0), "Forest", true);
        let land2 = add_library_card(&mut state, PlayerId(0), "Mountain", true);
        let hit = add_library_card(&mut state, PlayerId(0), "Bear", false);
        let unreached = add_library_card(&mut state, PlayerId(0), "Unreached", false);
        state.players[0].library = crate::im::vector![land1, land2, hit, unreached];

        let ability = ResolvedAbility::new(
            Effect::ExileFromTopUntil {
                player: TargetFilter::Controller,
                until: UntilCondition::NextMatches {
                    filter: nonland_filter(),
                },
            },
            vec![],
            source,
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        // Library: only `unreached` should remain (top three exiled).
        assert_eq!(
            state.players[0].library.iter().copied().collect::<Vec<_>>(),
            vec![unreached]
        );
        for &id in &[land1, land2, hit] {
            assert_eq!(
                state.objects.get(&id).unwrap().zone,
                Zone::Exile,
                "exiled card should be in exile zone"
            );
        }
        let linked: Vec<ObjectId> = state
            .exile_links
            .iter()
            .filter(|l| l.source_id == source)
            .map(|l| l.exiled_id)
            .collect();
        assert_eq!(
            linked.len(),
            0,
            "bare exile-until effects should not create source display links"
        );
    }

    #[test]
    fn scoped_player_exiles_from_faced_players_library() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(100),
            PlayerId(0),
            "Ensnared by the Mara".to_string(),
            Zone::Battlefield,
        );

        let controller_hit = add_library_card(&mut state, PlayerId(0), "Controller Bear", false);
        state.players[0].library = crate::im::vector![controller_hit];

        let faced_land = add_library_card(&mut state, PlayerId(1), "Faced Forest", true);
        let faced_hit = add_library_card(&mut state, PlayerId(1), "Faced Bear", false);
        state.players[1].library = crate::im::vector![faced_land, faced_hit];

        let mut ability = ResolvedAbility::new(
            Effect::ExileFromTopUntil {
                // CR 608.2c: faced-player villainous-choice branches bind
                // "their library" to the scoped event player, not Controller.
                player: TargetFilter::ScopedPlayer,
                until: UntilCondition::NextMatches {
                    filter: nonland_filter(),
                },
            },
            vec![],
            source,
            PlayerId(0),
        );
        ability.set_scoped_player_recursive(PlayerId(1));

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(
            state.objects.get(&controller_hit).unwrap().zone,
            Zone::Library,
            "controller library must not be used for a faced-player branch"
        );
        assert_eq!(state.objects.get(&faced_land).unwrap().zone, Zone::Exile);
        assert_eq!(state.objects.get(&faced_hit).unwrap().zone, Zone::Exile);
    }

    /// CR 701.13a + CR 601.2 + CR 118.9: A targeted-player
    /// ExileFromTopUntil chain uses the chosen player's library. If the caster
    /// accepts the optional free-cast branch, CastFromZone grants permission but
    /// does not move the hit to the stack in this resolver pipeline. The hit
    /// remains source-linked, so cleanup moves it with the misses.
    #[test]
    fn targeted_player_accept_cast_offer_cleans_up_uncast_hit_and_misses() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(100),
            PlayerId(0),
            "Chaos Wand".to_string(),
            Zone::Battlefield,
        );

        let controller_card = add_library_card(&mut state, PlayerId(0), "Controller Bear", false);
        state.players[0].library = crate::im::vector![controller_card];

        let miss_a = add_library_card(&mut state, PlayerId(1), "Opponent Bear", false);
        let miss_b = add_library_card(&mut state, PlayerId(1), "Opponent Elk", false);
        let hit = create_object(
            &mut state,
            CardId(4),
            PlayerId(1),
            "Lightning Bolt".to_string(),
            Zone::Library,
        );
        state
            .objects
            .get_mut(&hit)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Instant);
        let unreached = add_library_card(&mut state, PlayerId(1), "Unreached Bear", false);
        state.players[1].library = crate::im::vector![miss_a, miss_b, hit, unreached];

        let cleanup = ResolvedAbility::new(
            Effect::PutAtLibraryPosition {
                target: TargetFilter::ExiledBySource,
                count: QuantityExpr::Fixed { value: 0 },
                position: LibraryPosition::Bottom,
            },
            vec![],
            source,
            PlayerId(0),
        );
        let mut cast = ResolvedAbility::new(
            Effect::CastFromZone {
                target: TargetFilter::ParentTarget,
                without_paying_mana_cost: true,
                mode: CardPlayMode::Cast,
                cast_transformed: false,
                alt_ability_cost: None,
                constraint: None,
                duration: None,
                driver: crate::types::ability::CastFromZoneDriver::LingeringPermission,
                mana_spend_permission: None,
            },
            vec![],
            source,
            PlayerId(0),
        );
        cast.optional = true;
        cast.sub_ability = Some(Box::new(cleanup.clone()));
        cast.else_ability = Some(Box::new(cleanup));

        let mut ability = ResolvedAbility::new(
            Effect::ExileFromTopUntil {
                player: TargetFilter::Typed(
                    TypedFilter::default().controller(ControllerRef::Opponent),
                ),
                until: UntilCondition::NextMatches {
                    filter: instant_or_sorcery_filter(),
                },
            },
            vec![TargetRef::Player(PlayerId(1))],
            source,
            PlayerId(0),
        );
        ability.sub_ability = Some(Box::new(cast));

        let mut events = Vec::new();
        super::super::resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();
        assert!(matches!(
            state.waiting_for,
            crate::types::game_state::WaitingFor::OptionalEffectChoice { .. }
        ));

        apply(
            &mut state,
            PlayerId(0),
            GameAction::DecideOptionalEffect { accept: true },
        )
        .unwrap();

        assert_eq!(state.objects[&controller_card].zone, Zone::Library);
        assert_eq!(state.objects[&hit].zone, Zone::Library);
        assert!(state.objects[&hit].casting_permissions.is_empty());
        assert_eq!(state.objects[&miss_a].zone, Zone::Library);
        assert_eq!(state.objects[&miss_b].zone, Zone::Library);
        assert!(state.players[1].library.contains(&miss_a));
        assert!(state.players[1].library.contains(&miss_b));
        assert!(state.players[1].library.contains(&hit));
        assert!(state.players[1].library.contains(&unreached));
        assert!(!state
            .exile_links
            .iter()
            .any(|link| link.source_id == source));
    }

    /// CR 608.2c: Declining the optional cast branch leaves the hit
    /// source-linked, so the same ExiledBySource cleanup moves misses and hit.
    #[test]
    fn targeted_player_decline_cast_offer_cleans_up_hit_and_misses() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(100),
            PlayerId(0),
            "Chaos Wand".to_string(),
            Zone::Battlefield,
        );

        let miss = add_library_card(&mut state, PlayerId(1), "Opponent Bear", false);
        let hit = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Lightning Bolt".to_string(),
            Zone::Library,
        );
        state
            .objects
            .get_mut(&hit)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Instant);
        let unreached = add_library_card(&mut state, PlayerId(1), "Unreached Bear", false);
        state.players[1].library = crate::im::vector![miss, hit, unreached];

        let cleanup = ResolvedAbility::new(
            Effect::PutAtLibraryPosition {
                target: TargetFilter::ExiledBySource,
                count: QuantityExpr::Fixed { value: 0 },
                position: LibraryPosition::Bottom,
            },
            vec![],
            source,
            PlayerId(0),
        );
        let mut cast = ResolvedAbility::new(
            Effect::CastFromZone {
                target: TargetFilter::ParentTarget,
                without_paying_mana_cost: true,
                mode: CardPlayMode::Cast,
                cast_transformed: false,
                alt_ability_cost: None,
                constraint: None,
                duration: None,
                driver: crate::types::ability::CastFromZoneDriver::LingeringPermission,
                mana_spend_permission: None,
            },
            vec![],
            source,
            PlayerId(0),
        );
        cast.optional = true;
        cast.sub_ability = Some(Box::new(cleanup.clone()));
        cast.else_ability = Some(Box::new(cleanup));

        let mut ability = ResolvedAbility::new(
            Effect::ExileFromTopUntil {
                player: TargetFilter::Typed(
                    TypedFilter::default().controller(ControllerRef::Opponent),
                ),
                until: UntilCondition::NextMatches {
                    filter: instant_or_sorcery_filter(),
                },
            },
            vec![TargetRef::Player(PlayerId(1))],
            source,
            PlayerId(0),
        );
        ability.sub_ability = Some(Box::new(cast));

        let mut events = Vec::new();
        super::super::resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();
        apply(
            &mut state,
            PlayerId(0),
            GameAction::DecideOptionalEffect { accept: false },
        )
        .unwrap();

        assert_eq!(state.objects[&miss].zone, Zone::Library);
        assert_eq!(state.objects[&hit].zone, Zone::Library);
        assert!(state.players[1].library.contains(&miss));
        assert!(state.players[1].library.contains(&hit));
        assert!(state.players[1].library.contains(&unreached));
        assert!(state.objects[&hit].casting_permissions.is_empty());
        assert!(!state
            .exile_links
            .iter()
            .any(|link| link.source_id == source));
    }

    #[test]
    fn exile_from_top_until_routes_each_move_through_replacement_pipeline() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(100),
            PlayerId(0),
            "Replacement Source".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&source).unwrap();
            obj.replacement_definitions.push(
                ReplacementDefinition::new(ReplacementEvent::Moved)
                    .execute(AbilityDefinition::new(
                        AbilityKind::Spell,
                        Effect::ChangeZone {
                            origin: None,
                            destination: Zone::Graveyard,
                            target: TargetFilter::Any,
                            owner_library: false,
                            enter_transformed: false,
                            enters_under: None,
                            enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                            enters_attacking: false,
                            up_to: false,
                            enter_with_counters: vec![],
                            conditional_enter_with_counters: vec![],
                            face_down_profile: None,
                            enters_modified_if: None,
                        },
                    ))
                    .destination_zone(Zone::Exile),
            );
        }

        let hit = add_library_card(&mut state, PlayerId(0), "Bear", false);
        state.players[0].library = crate::im::vector![hit];

        let ability = ResolvedAbility::new(
            Effect::ExileFromTopUntil {
                player: TargetFilter::Controller,
                until: UntilCondition::NextMatches {
                    filter: nonland_filter(),
                },
            },
            vec![],
            source,
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(
            state.objects[&hit].zone,
            Zone::Graveyard,
            "Moved replacement should redirect the top-card exile"
        );
        assert!(
            state.players[0].graveyard.contains(&hit),
            "redirected card should be tracked in graveyard"
        );
        assert!(
            !state.exile.contains(&hit),
            "redirected card must not remain in exile"
        );
    }

    /// CR 608.2 + CR 701.57a + CR 702.85a: Etali-shape — `player_scope: All`
    /// drives per-player iteration; each iteration runs ExileFromTopUntil
    /// against the iterating player's library, exiling lands until a nonland
    /// is hit, and links all exiled cards to the resolving Etali source. After
    /// all iterations, `state.exile_links` reflects exiles from every player's
    /// library through the same source — the per-resolution channel
    /// `TargetFilter::ExiledBySource` consumes for "the nonland cards exiled
    /// this way" lookups.
    #[test]
    fn etali_player_scope_all_iterates_each_library_and_links_all() {
        let mut state = GameState::new(FormatConfig::standard(), 3, 42);
        let source = create_object(
            &mut state,
            CardId(100),
            PlayerId(0),
            "Etali, Primal Conqueror".to_string(),
            Zone::Battlefield,
        );

        // Each player's library: one Land then one Creature (so each iteration
        // exiles one land + one creature, linking both).
        let p0_land = add_library_card(&mut state, PlayerId(0), "P0 Forest", true);
        let p0_hit = add_library_card(&mut state, PlayerId(0), "P0 Beast", false);
        state.players[0].library = crate::im::vector![p0_land, p0_hit];

        let p1_land = add_library_card(&mut state, PlayerId(1), "P1 Mountain", true);
        let p1_hit = add_library_card(&mut state, PlayerId(1), "P1 Goblin", false);
        state.players[1].library = crate::im::vector![p1_land, p1_hit];

        let p2_land = add_library_card(&mut state, PlayerId(2), "P2 Plains", true);
        let p2_hit = add_library_card(&mut state, PlayerId(2), "P2 Soldier", false);
        state.players[2].library = crate::im::vector![p2_land, p2_hit];

        // Build the player_scope-wrapped ability via the standard
        // resolve_ability_chain entrypoint so the per-iterating-player rebind
        // is exercised by the same path Etali's runtime uses.
        let mut wrapped = ResolvedAbility::new(
            Effect::ExileFromTopUntil {
                player: TargetFilter::Controller,
                until: UntilCondition::NextMatches {
                    filter: nonland_filter(),
                },
            },
            vec![],
            source,
            PlayerId(0),
        );
        wrapped.player_scope = Some(PlayerFilter::All);

        let mut events = Vec::new();
        super::super::resolve_ability_chain(&mut state, &wrapped, &mut events, 0).unwrap();

        // No typed linked-exile consumer exists in this test ability, so the
        // six cards move to exile without source display links.
        let linked: Vec<ObjectId> = state
            .exile_links
            .iter()
            .filter(|l| l.source_id == source)
            .map(|l| l.exiled_id)
            .collect();
        assert_eq!(
            linked.len(),
            0,
            "bare exile-until effects should not create source display links"
        );
        for id in &[p0_land, p0_hit, p1_land, p1_hit, p2_land, p2_hit] {
            assert_eq!(
                state.objects.get(id).unwrap().zone,
                Zone::Exile,
                "card should be in exile"
            );
        }
    }

    /// CR 608.2c + CR 610.3 + CR 118.9 + CR 701.13a: Etali Primal Conqueror —
    /// `player_scope: All` outer + `NextMatches` exile-until + `CastFromZone`
    /// sub-ability whose filter references `ExiledBySource`. After per-player
    /// iteration writes one ExileLink per hit per player, the guard at the
    /// `NextMatches` sub-ability dispatch must skip the single-hit pre-bind
    /// so `cast_from_zone::resolve` enumerates every linked card via
    /// `linked_exile_cards_for_source` and grants `ExileWithAltCost { zero }`
    /// to each — NOT just the single-hit `ObjectId`.
    ///
    /// Negative-control siblings (Chaos Wand `targeted_player_accept_cast_...`,
    /// `put_counter_it_after_exile_from_top_until_resolves_to_parent_target`)
    /// must remain green, proving the guard preserves the pre-bind for
    /// `ParentTarget`-shape consumers.
    #[test]
    fn etali_each_player_exile_until_grants_cast_permission_to_every_linked_hit() {
        let mut state = GameState::new(FormatConfig::standard(), 3, 42);
        let source = create_object(
            &mut state,
            CardId(100),
            PlayerId(0),
            "Etali, Primal Conqueror".to_string(),
            Zone::Battlefield,
        );

        // Each player's library: one Land then one Creature (the nonland hit).
        let p0_land = add_library_card(&mut state, PlayerId(0), "P0 Forest", true);
        let p0_hit = add_library_card(&mut state, PlayerId(0), "P0 Beast", false);
        state.players[0].library = crate::im::vector![p0_land, p0_hit];

        let p1_land = add_library_card(&mut state, PlayerId(1), "P1 Mountain", true);
        let p1_hit = add_library_card(&mut state, PlayerId(1), "P1 Goblin", false);
        state.players[1].library = crate::im::vector![p1_land, p1_hit];

        let p2_land = add_library_card(&mut state, PlayerId(2), "P2 Plains", true);
        let p2_hit = add_library_card(&mut state, PlayerId(2), "P2 Soldier", false);
        state.players[2].library = crate::im::vector![p2_land, p2_hit];

        // Sub-ability: CastFromZone with filter referencing ExiledBySource (the
        // Etali shape after the parser fix). With empty targets, the guard at
        // the NextMatches dispatch must skip the single-hit pre-bind so
        // cast_from_zone::resolve materializes every linked exile card.
        let cast_sub = ResolvedAbility::new(
            Effect::CastFromZone {
                target: TargetFilter::And {
                    filters: vec![TargetFilter::ExiledBySource, nonland_filter()],
                },
                without_paying_mana_cost: true,
                mode: CardPlayMode::Cast,
                cast_transformed: false,
                alt_ability_cost: None,
                constraint: None,
                duration: None,
                driver: crate::types::ability::CastFromZoneDriver::LingeringPermission,
                mana_spend_permission: None,
            },
            vec![],
            source,
            PlayerId(0),
        );

        let mut wrapped = ResolvedAbility::new(
            Effect::ExileFromTopUntil {
                player: TargetFilter::Controller,
                until: UntilCondition::NextMatches {
                    filter: nonland_filter(),
                },
            },
            vec![],
            source,
            PlayerId(0),
        );
        wrapped.player_scope = Some(PlayerFilter::All);
        wrapped.sub_ability = Some(Box::new(cast_sub));

        let mut events = Vec::new();
        super::super::resolve_ability_chain(&mut state, &wrapped, &mut events, 0).unwrap();

        // All six cards should be in exile (lands + nonland hits).
        for id in &[p0_land, p0_hit, p1_land, p1_hit, p2_land, p2_hit] {
            assert_eq!(
                state.objects.get(id).unwrap().zone,
                Zone::Exile,
                "card {:?} should be in exile",
                id
            );
        }

        // CR 608.2 + CR 406.6: Exactly six TrackedBySource links — one per
        // exiled card per player-iteration. A regression that collapsed
        // `player_scope: All` to a single iteration (or that double-counted
        // a single player's library) would fail this count check even if the
        // permission-grant assertions below happened to still pass.
        let linked_count = state
            .exile_links
            .iter()
            .filter(|l| l.source_id == source)
            .count();
        assert_eq!(
            linked_count, 6,
            "expected 6 source-linked exiles (3 lands + 3 hits), got {linked_count}",
        );

        // Every nonland hit must carry ExileWithAltCost { zero } granted to
        // Etali's controller (PlayerId(0)). Lands must NOT — the AND with the
        // nonland filter excludes them from the cast permission.
        for &hit in &[p0_hit, p1_hit, p2_hit] {
            let perms = &state.objects[&hit].casting_permissions;
            let zero_cost_etali_permissions = perms
                .iter()
                .filter(|p| {
                    matches!(
                        p,
                        CastingPermission::ExileWithAltCost { cost, granted_to: Some(g), .. }
                            if *cost == ManaCost::zero() && *g == PlayerId(0)
                    )
                })
                .count();
            assert_eq!(
                zero_cost_etali_permissions,
                1,
                "nonland hit {:?} must have ExileWithAltCost {{ zero, granted_to: PlayerId(0) }} in casting_permissions={:?}",
                hit,
                perms
            );
        }
        for &land in &[p0_land, p1_land, p2_land] {
            assert!(
                state.objects[&land].casting_permissions.is_empty(),
                "land {:?} must not have casting permissions (typed leg excludes it)",
                land
            );
        }
    }

    /// CR 608.2 + CR 111.2: Akroan Horse-shape — `Effect::Token` with
    /// `owner: TargetFilter::Controller` under `player_scope: Opponent`
    /// rebinds Controller per-iteration so each opponent owns the token they
    /// create. Pinning regression test for the per-iterating-player Token
    /// owner rebind path that already works through the existing
    /// `scoped.controller = *pid` rebinding at `resolve_ability_chain`'s
    /// player_scope iteration loop.
    #[test]
    fn akroan_horse_each_opponent_creates_token_per_opponent_ownership() {
        use crate::types::ability::PtValue;
        use crate::types::mana::ManaColor;

        let mut state = GameState::new(FormatConfig::standard(), 3, 42);
        let source = create_object(
            &mut state,
            CardId(100),
            PlayerId(0),
            "Akroan Horse".to_string(),
            Zone::Battlefield,
        );

        let mut ability = ResolvedAbility::new(
            Effect::Token {
                name: "Soldier".to_string(),
                power: PtValue::Fixed(1),
                toughness: PtValue::Fixed(1),
                types: vec!["Creature".to_string(), "Soldier".to_string()],
                colors: vec![ManaColor::White],
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
            source,
            PlayerId(0),
        );
        ability.player_scope = Some(PlayerFilter::Opponent);

        let mut events = Vec::new();
        super::super::resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

        // Two soldier tokens should exist — one owned by each opponent.
        let tokens: Vec<_> = state
            .battlefield
            .iter()
            .filter_map(|id| state.objects.get(id))
            .filter(|object| object.is_token && object.name == "Soldier")
            .map(|object| (object.owner, object.controller))
            .collect();
        assert_eq!(
            tokens.len(),
            2,
            "expected 2 soldier tokens, got {:?}",
            tokens
        );
        let mut owners: Vec<PlayerId> = tokens.iter().map(|(o, _)| *o).collect();
        owners.sort();
        assert_eq!(
            owners,
            vec![PlayerId(1), PlayerId(2)],
            "tokens should be owned by each opponent (PlayerId(1), PlayerId(2)), got {:?}",
            tokens
        );
        // Controller matches owner: token controller = scoped controller per CR 111.2.
        for (owner, controller) in &tokens {
            assert_eq!(owner, controller, "token controller should match its owner");
        }
        // Akroan Horse's controller (PlayerId(0)) should not own any of the tokens.
        assert!(
            !tokens.iter().any(|(owner, _)| *owner == PlayerId(0)),
            "Akroan controller should not own any of the tokens"
        );
    }

    /// Helper: add a card to a player's library with a specific generic mana
    /// value contribution (CR 202.3 — generic only, no shards, so mana value
    /// equals `generic_cost`).
    fn add_library_card_with_mv(
        state: &mut GameState,
        owner: PlayerId,
        name: &str,
        generic_cost: u32,
    ) -> ObjectId {
        let card_id = CardId(state.next_object_id);
        let id = create_object(state, card_id, owner, name.to_string(), Zone::Library);
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.mana_cost = ManaCost::generic(generic_cost);
        id
    }

    /// CR 202.3 + CR 107.3e: Tasha's Hideous Laughter mainline — exile from
    /// the top of an opponent's library until the cumulative mana value of
    /// the exiled cards reaches 20. Library has cards summing to exactly 20
    /// across the first three cards; the fourth card must remain in library.
    #[test]
    fn cumulative_threshold_stops_when_running_sum_reaches_threshold() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(100),
            PlayerId(0),
            "Tasha's Hideous Laughter".to_string(),
            Zone::Battlefield,
        );

        // Mana values 8 + 7 + 5 = 20 (≥ 20 reached on third); fourth (4) untouched.
        let c1 = add_library_card_with_mv(&mut state, PlayerId(0), "Eight", 8);
        let c2 = add_library_card_with_mv(&mut state, PlayerId(0), "Seven", 7);
        let c3 = add_library_card_with_mv(&mut state, PlayerId(0), "Five", 5);
        let c4 = add_library_card_with_mv(&mut state, PlayerId(0), "Four", 4);
        state.players[0].library = crate::im::vector![c1, c2, c3, c4];

        let ability = ResolvedAbility::new(
            Effect::ExileFromTopUntil {
                player: TargetFilter::Controller,
                until: UntilCondition::CumulativeThreshold {
                    property: ObjectProperty::ManaValue,
                    comparator: Comparator::GE,
                    threshold: QuantityExpr::Fixed { value: 20 },
                },
            },
            vec![],
            source,
            PlayerId(0),
        );

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        for &id in &[c1, c2, c3] {
            assert_eq!(
                state.objects.get(&id).unwrap().zone,
                Zone::Exile,
                "card with cumulative-MV contribution should be exiled"
            );
        }
        assert_eq!(
            state.objects.get(&c4).unwrap().zone,
            Zone::Library,
            "card after the threshold was reached should remain in the library"
        );
    }

    /// CR 202.3 + CR 107.3e: When the library cannot reach the threshold even
    /// after exiling every card, the loop terminates naturally with the entire
    /// library exiled. Tasha's Hideous Laughter against a small deck.
    #[test]
    fn cumulative_threshold_exhausts_library_when_threshold_unreachable() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(100),
            PlayerId(0),
            "Tasha's Hideous Laughter".to_string(),
            Zone::Battlefield,
        );

        // Total MV = 1 + 2 = 3; threshold 20 unreachable.
        let c1 = add_library_card_with_mv(&mut state, PlayerId(0), "One", 1);
        let c2 = add_library_card_with_mv(&mut state, PlayerId(0), "Two", 2);
        state.players[0].library = crate::im::vector![c1, c2];

        let ability = ResolvedAbility::new(
            Effect::ExileFromTopUntil {
                player: TargetFilter::Controller,
                until: UntilCondition::CumulativeThreshold {
                    property: ObjectProperty::ManaValue,
                    comparator: Comparator::GE,
                    threshold: QuantityExpr::Fixed { value: 20 },
                },
            },
            vec![],
            source,
            PlayerId(0),
        );

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(
            state.players[0].library.is_empty(),
            "library should be fully exiled when threshold is unreachable"
        );
        for &id in &[c1, c2] {
            assert_eq!(state.objects.get(&id).unwrap().zone, Zone::Exile);
        }
    }

    /// CR 608.2 + CR 202.3: Tasha multiplayer — `player_scope: Opponent`
    /// drives per-opponent iteration; each opponent independently exiles from
    /// their own library until that player's running cumulative mana value
    /// satisfies the threshold. One opponent's accumulated value must not
    /// affect another's.
    #[test]
    fn cumulative_threshold_each_opponent_resolves_independently() {
        let mut state = GameState::new(FormatConfig::standard(), 3, 42);
        let source = create_object(
            &mut state,
            CardId(100),
            PlayerId(0),
            "Tasha's Hideous Laughter".to_string(),
            Zone::Battlefield,
        );

        // Opponent 1 hits threshold on the second card (10 + 10 = 20).
        let p1_a = add_library_card_with_mv(&mut state, PlayerId(1), "P1 Ten A", 10);
        let p1_b = add_library_card_with_mv(&mut state, PlayerId(1), "P1 Ten B", 10);
        let p1_unreached = add_library_card_with_mv(&mut state, PlayerId(1), "P1 Six", 6);
        state.players[1].library = crate::im::vector![p1_a, p1_b, p1_unreached];

        // Opponent 2 hits threshold on the third card (8 + 8 + 8 = 24 ≥ 20).
        let p2_a = add_library_card_with_mv(&mut state, PlayerId(2), "P2 Eight A", 8);
        let p2_b = add_library_card_with_mv(&mut state, PlayerId(2), "P2 Eight B", 8);
        let p2_c = add_library_card_with_mv(&mut state, PlayerId(2), "P2 Eight C", 8);
        let p2_unreached = add_library_card_with_mv(&mut state, PlayerId(2), "P2 Two", 2);
        state.players[2].library = crate::im::vector![p2_a, p2_b, p2_c, p2_unreached];

        // Controller (PlayerId(0)) library must be untouched.
        let p0_card = add_library_card_with_mv(&mut state, PlayerId(0), "P0 One", 1);
        state.players[0].library = crate::im::vector![p0_card];

        let mut wrapped = ResolvedAbility::new(
            Effect::ExileFromTopUntil {
                player: TargetFilter::Controller,
                until: UntilCondition::CumulativeThreshold {
                    property: ObjectProperty::ManaValue,
                    comparator: Comparator::GE,
                    threshold: QuantityExpr::Fixed { value: 20 },
                },
            },
            vec![],
            source,
            PlayerId(0),
        );
        wrapped.player_scope = Some(PlayerFilter::Opponent);

        let mut events = Vec::new();
        super::super::resolve_ability_chain(&mut state, &wrapped, &mut events, 0).unwrap();

        // Controller library untouched.
        assert_eq!(
            state.objects.get(&p0_card).unwrap().zone,
            Zone::Library,
            "controller library must not be exiled"
        );

        // Opponent 1: first two exiled, third remains.
        assert_eq!(state.objects.get(&p1_a).unwrap().zone, Zone::Exile);
        assert_eq!(state.objects.get(&p1_b).unwrap().zone, Zone::Exile);
        assert_eq!(
            state.objects.get(&p1_unreached).unwrap().zone,
            Zone::Library,
            "opponent 1's third card should remain — independent threshold"
        );

        // Opponent 2: first three exiled, fourth remains.
        assert_eq!(state.objects.get(&p2_a).unwrap().zone, Zone::Exile);
        assert_eq!(state.objects.get(&p2_b).unwrap().zone, Zone::Exile);
        assert_eq!(state.objects.get(&p2_c).unwrap().zone, Zone::Exile);
        assert_eq!(
            state.objects.get(&p2_unreached).unwrap().zone,
            Zone::Library,
            "opponent 2's fourth card should remain — independent threshold"
        );
    }

    /// Cumulative-threshold sub-ability dispatch: in the `CumulativeThreshold`
    /// arm there is no single hit card, but the resolver must still run the
    /// sub_ability chain (with the original target list intact) so that
    /// follow-up sentences like Improvisation Capstone's "You may cast any
    /// number of spells from among them" reach their resolver. The
    /// `EffectKind::EffectResolved` event for the sub-ability is the
    /// observable signal that the chain ran.
    #[test]
    fn cumulative_threshold_runs_sub_ability_chain() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(100),
            PlayerId(0),
            "Improvisation Capstone".to_string(),
            Zone::Battlefield,
        );

        let c1 = add_library_card_with_mv(&mut state, PlayerId(0), "Two", 2);
        let c2 = add_library_card_with_mv(&mut state, PlayerId(0), "Two B", 2);
        state.players[0].library = crate::im::vector![c1, c2];

        // Sub-ability is a trivial Scry 1 — easy to detect by EffectResolved
        // kind because it doesn't depend on target legality.
        let sub = ResolvedAbility::new(
            Effect::Scry {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
            vec![],
            source,
            PlayerId(0),
        );
        let mut ability = ResolvedAbility::new(
            Effect::ExileFromTopUntil {
                player: TargetFilter::Controller,
                until: UntilCondition::CumulativeThreshold {
                    property: ObjectProperty::ManaValue,
                    comparator: Comparator::GE,
                    threshold: QuantityExpr::Fixed { value: 4 },
                },
            },
            vec![],
            source,
            PlayerId(0),
        );
        ability.sub_ability = Some(Box::new(sub));

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        // Both cards exiled (2 + 2 = 4 ≥ 4).
        assert_eq!(state.objects.get(&c1).unwrap().zone, Zone::Exile);
        assert_eq!(state.objects.get(&c2).unwrap().zone, Zone::Exile);

        // Sub-ability ran — there should be a Scry resolution event.
        let sub_kinds: Vec<EffectKind> = events
            .iter()
            .filter_map(|e| match e {
                GameEvent::EffectResolved { kind, .. } => Some(*kind),
                _ => None,
            })
            .collect();
        assert!(
            sub_kinds.contains(&EffectKind::Scry),
            "sub_ability (Scry) must run for CumulativeThreshold even though no hit card was injected; got events kinds {sub_kinds:?}"
        );
    }

    // --- Jodah, the Unifier (#5277 + #5292) ---------------------------------

    /// A legendary-supertype nonland filter — the simplified `NextMatches`
    /// predicate the hand-built Jodah fixtures use (the real card additionally
    /// requires `mana value < CostPaidObject`, threaded only through the full
    /// trigger; the shape assertion in `jodah_parse_lowers_the_rest_cleanup`
    /// pins the verbatim parse so these fixtures cannot drift).
    fn legendary_nonland_filter() -> TargetFilter {
        TargetFilter::And {
            filters: vec![
                nonland_filter(),
                TargetFilter::Typed(TypedFilter::default().properties(vec![
                    FilterProp::HasSupertype {
                        value: Supertype::Legendary,
                    },
                ])),
            ],
        }
    }

    /// The normalized "put the rest on the bottom" cleanup target: every card
    /// this ability exiled EXCEPT the ParentTarget hit (declined hit stays in
    /// exile). Mirrors `normalize_exile_until_cast_bottom_cleanup`.
    fn jodah_rest_cleanup_target() -> TargetFilter {
        TargetFilter::And {
            filters: vec![
                TargetFilter::ExiledBySource,
                TargetFilter::Typed(TypedFilter::default().properties(vec![
                    FilterProp::DistinctFrom {
                        reference: Box::new(TargetFilter::ParentTarget),
                    },
                ])),
            ],
        }
    }

    /// Build a Jodah trigger chain in the NORMALIZED parser shape:
    /// `ExileFromTopUntil { NextMatches(legendary nonland) }`
    ///   → optional `CastFromZone { ParentTarget, DuringResolution }`
    ///       sub = cleanup, else = cleanup (both "put the rest on the bottom").
    fn jodah_chain(source: ObjectId) -> ResolvedAbility {
        let cleanup = ResolvedAbility::new(
            Effect::PutAtLibraryPosition {
                target: jodah_rest_cleanup_target(),
                count: QuantityExpr::Fixed { value: 0 },
                position: LibraryPosition::Bottom,
            },
            vec![],
            source,
            PlayerId(0),
        );
        let mut cast = ResolvedAbility::new(
            Effect::CastFromZone {
                target: TargetFilter::ParentTarget,
                without_paying_mana_cost: true,
                mode: CardPlayMode::Cast,
                cast_transformed: false,
                alt_ability_cost: None,
                constraint: None,
                duration: None,
                driver: CastFromZoneDriver::DuringResolution,
                mana_spend_permission: None,
            },
            vec![],
            source,
            PlayerId(0),
        );
        cast.optional = true;
        cast.sub_link = SubAbilityLink::SequentialSibling;
        cast.sub_ability = Some(Box::new(cleanup.clone()));
        cast.else_ability = Some(Box::new(cleanup));

        let mut head = ResolvedAbility::new(
            Effect::ExileFromTopUntil {
                player: TargetFilter::Controller,
                until: UntilCondition::NextMatches {
                    filter: legendary_nonland_filter(),
                },
            },
            vec![],
            source,
            PlayerId(0),
        );
        head.sub_ability = Some(Box::new(cast));
        head
    }

    fn add_legendary_library_card(state: &mut GameState, owner: PlayerId, name: &str) -> ObjectId {
        let id = add_library_card(state, owner, name, false);
        state
            .objects
            .get_mut(&id)
            .unwrap()
            .card_types
            .supertypes
            .push(Supertype::Legendary);
        id
    }

    /// (a) #5277 — no hit: the library holds only nonmatching cards. The loop
    /// exhausts the library, no legendary is exiled, and the "cast that card"
    /// link is skipped — but the "put the rest on the bottom" cleanup MUST still
    /// run so every exiled card returns to the library. Pre-fix the whole pile
    /// was stranded in exile and the player decked out.
    ///
    /// Discriminating assertion: `library.len() == N` after resolution. Revert
    /// the runtime no-hit branch and all N cards stay in `Zone::Exile`.
    #[test]
    fn jodah_no_hit_returns_all_exiled_cards_to_library() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(100),
            PlayerId(0),
            "Jodah, the Unifier".to_string(),
            Zone::Battlefield,
        );

        // Four nonlegendary creatures — none satisfy the legendary predicate.
        let c1 = add_library_card(&mut state, PlayerId(0), "Bear One", false);
        let c2 = add_library_card(&mut state, PlayerId(0), "Bear Two", false);
        let c3 = add_library_card(&mut state, PlayerId(0), "Bear Three", false);
        let c4 = add_library_card(&mut state, PlayerId(0), "Bear Four", false);
        let all = [c1, c2, c3, c4];
        state.players[0].library = crate::im::vector![c1, c2, c3, c4];

        let ability = jodah_chain(source);
        let mut events = Vec::new();
        super::super::resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

        // Reach-guard: the exile-until loop genuinely ran (no early
        // short-circuit) — every card left the library at least momentarily and
        // no legendary hit was recorded.
        assert!(
            state.stack.is_empty(),
            "no hit means nothing should be cast onto the stack"
        );

        // Discriminating: every exiled card is back in the library, none stranded.
        for &id in &all {
            assert_eq!(
                state.objects.get(&id).unwrap().zone,
                Zone::Library,
                "no-hit cleanup must return {id:?} to the library, not strand it in exile"
            );
        }
        assert_eq!(
            state.players[0].library.len(),
            all.len(),
            "the entire library must be reconstituted (bottom-in-random-order into empty library)"
        );
        for &id in &all {
            assert!(
                state.players[0].library.contains(&id),
                "library membership must include {id:?}"
            );
        }
        assert!(
            !state.exile.contains(&c1) && !state.exile.contains(&c4),
            "no exiled card may remain stranded in the exile zone"
        );
    }

    /// (b) Hit + accept: library = [miss, miss, legendary hit, unreached]. The
    /// loop stops at the legendary hit. Accepting casts it during resolution;
    /// the misses go to the bottom and the unreached card is untouched.
    #[test]
    fn jodah_hit_accept_casts_hit_and_returns_misses() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(100),
            PlayerId(0),
            "Jodah, the Unifier".to_string(),
            Zone::Battlefield,
        );

        let miss_a = add_library_card(&mut state, PlayerId(0), "Bear A", false);
        let miss_b = add_library_card(&mut state, PlayerId(0), "Bear B", false);
        let hit = add_legendary_library_card(&mut state, PlayerId(0), "Legendary Hit");
        let unreached = add_library_card(&mut state, PlayerId(0), "Unreached", false);
        state.players[0].library = crate::im::vector![miss_a, miss_b, hit, unreached];

        let ability = jodah_chain(source);
        let mut events = Vec::new();
        super::super::resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();
        assert!(
            matches!(
                state.waiting_for,
                crate::types::game_state::WaitingFor::OptionalEffectChoice { .. }
            ),
            "the hit must offer an optional cast prompt; got {:?}",
            state.waiting_for
        );

        apply(
            &mut state,
            PlayerId(0),
            GameAction::DecideOptionalEffect { accept: true },
        )
        .unwrap();

        // The accepted hit is cast during resolution — it is on the stack.
        assert_eq!(
            state.objects[&hit].zone,
            Zone::Stack,
            "accepting the free cast must put the legendary hit on the stack; zone={:?}",
            state.objects[&hit].zone
        );
        // Misses returned to the library bottom.
        assert_eq!(state.objects[&miss_a].zone, Zone::Library);
        assert_eq!(state.objects[&miss_b].zone, Zone::Library);
        assert!(state.players[0].library.contains(&miss_a));
        assert!(state.players[0].library.contains(&miss_b));
        // Unreached card never left the library.
        assert_eq!(state.objects[&unreached].zone, Zone::Library);
        assert!(state.players[0].library.contains(&unreached));
    }

    /// (c) Hit + decline (the discriminating Jodah-vs-Chaos-Wand case): the
    /// declined legendary hit REMAINS IN EXILE (Scryfall ruling), while the
    /// misses are put on the bottom. The `DistinctFrom { ParentTarget }` leg of
    /// the cleanup excludes the declined hit from the sweep.
    ///
    /// Discriminating assertion: `hit.zone == Zone::Exile` after decline. Revert
    /// the `filter_refs_parent_target` extension (so the decline branch stops
    /// inheriting the hit target) and the hit is swept back to the library.
    #[test]
    fn jodah_hit_decline_leaves_hit_in_exile_returns_misses() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(100),
            PlayerId(0),
            "Jodah, the Unifier".to_string(),
            Zone::Battlefield,
        );

        let miss = add_library_card(&mut state, PlayerId(0), "Bear Miss", false);
        let hit = add_legendary_library_card(&mut state, PlayerId(0), "Legendary Hit");
        let unreached = add_library_card(&mut state, PlayerId(0), "Unreached", false);
        state.players[0].library = crate::im::vector![miss, hit, unreached];

        let ability = jodah_chain(source);
        let mut events = Vec::new();
        super::super::resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();
        apply(
            &mut state,
            PlayerId(0),
            GameAction::DecideOptionalEffect { accept: false },
        )
        .unwrap();

        // Discriminating: the declined hit stays in exile.
        assert_eq!(
            state.objects[&hit].zone,
            Zone::Exile,
            "a declined Jodah hit must REMAIN in exile (ruling), not return to the library"
        );
        assert!(
            !state.players[0].library.contains(&hit),
            "the declined hit must not be swept to the library bottom"
        );
        // The miss is returned to the library bottom.
        assert_eq!(state.objects[&miss].zone, Zone::Library);
        assert!(state.players[0].library.contains(&miss));
        // Unreached card untouched.
        assert_eq!(state.objects[&unreached].zone, Zone::Library);
        assert!(state.players[0].library.contains(&unreached));
    }

    /// (d) Parser shape: Jodah's verbatim Oracle text lowers to the normalized
    /// chain — the cast's cleanup (both `sub_ability` and `else_ability`)
    /// targets `And { ExiledBySource, DistinctFrom { ParentTarget } }` with
    /// position Bottom. Positive reach-guard: zero `Unimplemented` effects in the
    /// parsed trigger chain. Pins the hand-built fixtures above against drift.
    #[test]
    fn jodah_parse_lowers_the_rest_cleanup() {
        let parsed = crate::parser::parse_oracle_text(
            "Legendary creatures you control get +X/+X, where X is the number of legendary creatures you control.\n\
             Whenever you cast a legendary spell from your hand, exile cards from the top of your library until you exile a legendary nonland card with lesser mana value. You may cast that card without paying its mana cost. Put the rest on the bottom of your library in a random order.",
            "Jodah, the Unifier",
            &[],
            &["Legendary".to_string(), "Creature".to_string()],
            &[],
        );

        let exec = parsed
            .triggers
            .iter()
            .find_map(|t| t.execute.as_ref())
            .filter(|e| matches!(&*e.effect, Effect::ExileFromTopUntil { .. }))
            .expect("Jodah exile-from-top-until trigger must parse");

        // head → cast → cleanup.
        assert!(
            matches!(
                &*exec.effect,
                Effect::ExileFromTopUntil {
                    until: UntilCondition::NextMatches { .. },
                    ..
                }
            ),
            "head must be ExileFromTopUntil {{ NextMatches }}"
        );
        let cast = exec.sub_ability.as_deref().expect("cast sub-ability");
        assert!(cast.optional, "the cast must be optional (you MAY cast)");
        assert!(
            matches!(
                &*cast.effect,
                Effect::CastFromZone {
                    target: TargetFilter::ParentTarget,
                    ..
                }
            ),
            "cast must target the ParentTarget hit, got {:?}",
            cast.effect
        );

        // Both the mainline cleanup and the decline else must be the normalized
        // "put the rest on the bottom" form.
        let expect_rest_cleanup = |ab: &AbilityDefinition, label: &str| {
            let Effect::PutAtLibraryPosition {
                target, position, ..
            } = &*ab.effect
            else {
                panic!("{label} must be PutAtLibraryPosition, got {:?}", ab.effect);
            };
            assert!(
                matches!(position, LibraryPosition::Bottom),
                "{label} must place on the bottom"
            );
            assert_eq!(
                target,
                &jodah_rest_cleanup_target(),
                "{label} target must be And {{ ExiledBySource, DistinctFrom {{ ParentTarget }} }}"
            );
        };
        expect_rest_cleanup(
            cast.sub_ability.as_deref().expect("cleanup sub-ability"),
            "cleanup sub_ability",
        );
        expect_rest_cleanup(
            cast.else_ability
                .as_deref()
                .expect("cleanup wired as else_ability for the decline path"),
            "cleanup else_ability",
        );

        // Positive reach-guard: no Unimplemented anywhere in the trigger chain.
        fn has_unimplemented(ab: &AbilityDefinition) -> bool {
            matches!(&*ab.effect, Effect::Unimplemented { .. })
                || ab.sub_ability.as_deref().is_some_and(has_unimplemented)
                || ab.else_ability.as_deref().is_some_and(has_unimplemented)
        }
        assert!(
            !has_unimplemented(exec),
            "the Jodah trigger chain must contain no Unimplemented effects"
        );
    }
}
