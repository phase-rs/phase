use crate::game::filter::{matches_target_filter, FilterContext};
use crate::game::zones;
use crate::types::ability::{Effect, EffectError, EffectKind, ResolvedAbility, TargetRef};
use crate::types::events::GameEvent;
use crate::types::game_state::{ExileLink, ExileLinkKind, GameState};
use crate::types::identifiers::ObjectId;
use crate::types::zones::Zone;

/// CR 701.57a + CR 702.85a: Exile cards from the top of the controller's library
/// one at a time until a card matching the filter is found. Models the
/// Discover (701.57a) / Cascade (702.85a) "exile from top until match" loop
/// generalized over an arbitrary hit filter. The hit card's ObjectId is
/// injected as a target into the sub_ability chain.
///
/// If the library is exhausted without a match, the sub_ability chain is skipped.
/// Miss cards remain in exile (specific cleanup is the sub_ability's responsibility).
///
/// CR 400.7 + CR 406.6: Each exiled card is recorded in `state.exile_links` with
/// `ExileLinkKind::TrackedBySource` so downstream effects can reference "cards
/// exiled this way" via `TargetFilter::ExiledBySource` (Etali, Primal Conqueror's
/// "you may cast any number of spells from among the nonland cards exiled this
/// way" — the same per-resolution exile-link channel that Skyclave Apparition
/// and the linked-owner family already consume).
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let filter = match &ability.effect {
        Effect::ExileFromTopUntil { filter } => filter,
        _ => return Err(EffectError::MissingParam("filter".to_string())),
    };

    let player = state
        .players
        .iter()
        .find(|p| p.id == ability.controller)
        .ok_or(EffectError::PlayerNotFound)?;

    // Snapshot library (top = index 0) to iterate without borrow conflicts.
    let library: Vec<ObjectId> = player.library.iter().copied().collect();
    let mut hit_id: Option<ObjectId> = None;

    // CR 107.3a + CR 601.2b: ability-context evaluation so dynamic thresholds
    // resolve against the resolving ability's `chosen_x`.
    let ctx = FilterContext::from_ability(ability);

    for &obj_id in &library {
        // CR 701.57a / 702.85a: Exile each card one at a time, checking the
        // hit filter after each exile so the loop terminates as soon as a
        // matching card is found.
        zones::move_to_zone(state, obj_id, Zone::Exile, events);

        // CR 400.7 + CR 406.6: Link the exiled card to the resolving source so
        // `TargetFilter::ExiledBySource` (Etali) and the per-resolution-tracking
        // family of filters (`OwnersOfCardsExiledBySource`, `CardsExiledBySource`)
        // see this exile event. Pruned automatically when the source leaves
        // play; matches the link kind used by `change_zone::move_object_to_zone`
        // for non-duration exiles.
        state.exile_links.push(ExileLink {
            exiled_id: obj_id,
            source_id: ability.source_id,
            kind: ExileLinkKind::TrackedBySource,
        });

        // Check if the just-exiled card matches the hit filter.
        if matches_target_filter(state, obj_id, filter, &ctx) {
            hit_id = Some(obj_id);
            break;
        }
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::ExileFromTopUntil,
        source_id: ability.source_id,
    });

    // CR 400.7: An object that moves from one zone to another becomes a new object.
    // If a hit was found and there is a sub_ability, resolve it with the hit card as target.
    if let (Some(hit), Some(ref sub)) = (hit_id, &ability.sub_ability) {
        let mut sub_clone = sub.as_ref().clone();
        sub_clone.targets = vec![TargetRef::Object(hit)];
        sub_clone.context = ability.context.clone();
        // Resolve the sub_ability chain directly — return early so the caller's
        // resolve_ability_chain does not double-chain the sub_ability.
        super::resolve_ability_chain(state, &sub_clone, events, 1)?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::ability::{
        PlayerFilter, QuantityExpr, ResolvedAbility, TargetFilter, TypeFilter, TypedFilter,
    };
    use crate::types::card_type::CoreType;
    use crate::types::format::FormatConfig;
    use crate::types::identifiers::CardId;
    use crate::types::player::PlayerId;

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

    /// CR 701.57a + CR 702.85a: When the iterator hits a nonland, it stops and
    /// reports the hit. CR 400.7 + CR 406.6: every exiled card (lands + the
    /// hit) is recorded with `ExileLinkKind::TrackedBySource` so
    /// `TargetFilter::ExiledBySource` lookups see all of them.
    #[test]
    fn exiles_lands_then_stops_at_nonland_and_links_all() {
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
                filter: nonland_filter(),
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
        // CR 400.7 + CR 406.6: Each exiled card linked to source.
        let linked: Vec<ObjectId> = state
            .exile_links
            .iter()
            .filter(|l| l.source_id == source)
            .map(|l| l.exiled_id)
            .collect();
        assert_eq!(
            linked.len(),
            3,
            "all three exiled cards should be linked to source"
        );
        assert!(linked.contains(&land1));
        assert!(linked.contains(&land2));
        assert!(linked.contains(&hit));
        assert!(!linked.contains(&unreached));
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
                filter: nonland_filter(),
            },
            vec![],
            source,
            PlayerId(0),
        );
        wrapped.player_scope = Some(PlayerFilter::All);

        let mut events = Vec::new();
        super::super::resolve_ability_chain(&mut state, &wrapped, &mut events, 0).unwrap();

        // All six cards (one land + one creature per player × 3 players)
        // should be linked to the resolving source.
        let linked: Vec<ObjectId> = state
            .exile_links
            .iter()
            .filter(|l| l.source_id == source)
            .map(|l| l.exiled_id)
            .collect();
        assert_eq!(
            linked.len(),
            6,
            "all six exiled cards should be linked to source (one land + one nonland per player × 3 players)"
        );
        for id in &[p0_land, p0_hit, p1_land, p1_hit, p2_land, p2_hit] {
            assert!(
                linked.contains(id),
                "expected exile link for {:?}",
                state.objects.get(id).unwrap().name
            );
            assert_eq!(
                state.objects.get(id).unwrap().zone,
                Zone::Exile,
                "card should be in exile"
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
}
