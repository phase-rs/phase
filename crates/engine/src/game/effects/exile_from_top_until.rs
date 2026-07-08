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
///   `NextMatches`, the sub_ability chain is skipped (no hit to inject). For
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
    });

    // CR 400.7: An object that moves from one zone to another becomes a new
    // object. Sub-ability chaining differs per stop-condition kind:
    //
    // * NextMatches: inject the hit card as the sub-ability's target so
    //   "cast that card" / "you may put it onto the battlefield" address the
    //   right object. If no hit was found, the sub_ability is skipped.
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
        AbilityDefinition, AbilityKind, CardPlayMode, CastingPermission, Comparator, ControllerRef,
        LibraryPosition, PlayerFilter, QuantityExpr, ReplacementDefinition, ResolvedAbility,
        TargetFilter, TargetRef, TypeFilter, TypedFilter,
    };
    use crate::types::actions::GameAction;
    use crate::types::card_type::CoreType;
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
}
