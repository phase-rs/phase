use crate::game::filter::{matches_target_filter, FilterContext};
use crate::game::quantity::resolve_quantity_with_targets;
use crate::game::static_abilities::prohibition_scope_matches_player;
use crate::types::ability::{
    Effect, EffectError, EffectKind, ResolvedAbility, SearchSelectionConstraint, TargetFilter,
    TargetRef,
};
use crate::types::events::{GameEvent, PlayerActionKind};
use crate::types::game_state::{GameState, WaitingFor};
use crate::types::player::PlayerId;
use crate::types::statics::StaticMode;

/// CR 701.23a: Resolve `SearchLibrary.target_player` to the library owner's
/// `PlayerId`. Handles both the pre-resolved path (caster picked a Player at
/// cast time, e.g., "search target opponent's library" → `TargetRef::Player`
/// already in `ability.targets`) and the context-ref path (subject-inherited
/// filter like `ParentTargetController`, which resolves against the parent
/// target object's controller at resolution time).
///
/// Returns the caster as a safe fallback if neither resolves.
fn resolve_library_owner(
    state: &GameState,
    ability: &ResolvedAbility,
    target_player: &TargetFilter,
) -> PlayerId {
    // Pre-resolved: a TargetRef::Player was picked at cast time.
    if let Some(pid) = ability.targets.iter().find_map(|t| match t {
        TargetRef::Player(pid) => Some(*pid),
        _ => None,
    }) {
        return pid;
    }
    // CR 608.2c: Context-ref — "its controller" resolves against the first
    // object in the parent ability chain's targets (the Destroyed permanent
    // for Assassin's Trophy, the exiled spell for Praetor's Grasp variants, …).
    if matches!(target_player, TargetFilter::ParentTargetController) {
        if let Some(parent_obj_id) = ability.targets.iter().find_map(|t| match t {
            TargetRef::Object(id) => Some(*id),
            _ => None,
        }) {
            if let Some(obj) = state.objects.get(&parent_obj_id) {
                return obj.controller;
            }
        }
    }
    ability.controller
}

/// CR 701.23a + CR 117.3a: The "searcher" is the player following the "search"
/// instruction. For subject-anchored targets (e.g., "its controller may search
/// their library"), the subject is both the library owner and the searcher —
/// they pick the card from their own library. For target-selected libraries
/// ("search target opponent's library"), the caster searches through the
/// chosen opponent's library.
fn searcher_is_library_owner(target_player: &TargetFilter) -> bool {
    matches!(
        target_player,
        TargetFilter::ParentTargetController
            | TargetFilter::TriggeringPlayer
            | TargetFilter::TriggeringSpellController
            | TargetFilter::TriggeringSpellOwner
    )
}

/// CR 701.23 + CR 609.3: Check if any active CantSearchLibrary static on the battlefield
/// muzzles the source of this search. `ability.controller` is the player who controls
/// the spell/ability that would cause the search (the "cause"). If muzzled, the search
/// is treated as an impossible action and produces no game-state change (CR 609.3).
///
/// E.g., Ashiok, Dream Render: `"Spells and abilities your opponents control can't cause
/// their controller to search their library."` — cause=Opponents means the Ashiok
/// controller's opponents' spells/abilities are muzzled.
///
/// NOTE: Ashiok's Oracle is grammatically "cause **their controller** to search **their**
/// library" — both pronouns bind to the cause's controller (i.e., self-search only).
/// The current implementation muzzles ALL searches caused by an opponent regardless of
/// the searching player, which is a minor over-block for the rare case of an opponent's
/// effect searching a non-controller's library (e.g., Splinter targeting). Most printed
/// search effects are self-searches where the distinction does not matter. Tightening
/// this to require `searcher == cause_controller` is tracked as a follow-up refinement.
fn is_search_muzzled(state: &GameState, cause_controller: crate::types::player::PlayerId) -> bool {
    // CR 702.26b + CR 604.1: Functioning gate owned by `battlefield_active_statics`.
    for (bf_obj, def) in crate::game::functioning_abilities::battlefield_active_statics(state) {
        let StaticMode::CantSearchLibrary { ref cause } = def.mode else {
            continue;
        };
        if prohibition_scope_matches_player(cause, cause_controller, bf_obj.id, state) {
            return true;
        }
    }
    false
}

/// CR 608.2c: Validate a chosen card-id set against the search-selection
/// constraint propagated from `Effect::SearchLibrary.selection_constraint`.
/// Centralized here so the resolver, the engine submission guard, and the AI
/// candidate filter share one authority — adding a new constraint variant
/// requires changes only at this single site (and the parser / enum).
pub fn selection_satisfies_constraint(
    state: &GameState,
    chosen: &[crate::types::identifiers::ObjectId],
    constraint: &SearchSelectionConstraint,
) -> bool {
    match constraint {
        SearchSelectionConstraint::None => true,
        SearchSelectionConstraint::DistinctNames => {
            let mut seen = std::collections::HashSet::new();
            chosen.iter().all(|id| match state.objects.get(id) {
                // CR 201.2: Two cards "have the same name" iff their card-name
                // strings match. Compare on the canonical printed name held by
                // the GameObject; absent objects (defensive) fail the check.
                Some(obj) => seen.insert(obj.name.as_str()),
                None => false,
            })
        }
    }
}

/// CR 701.23a + CR 401.2: Search a library — look through it, find card(s) matching criteria, then shuffle.
/// CR 401.2: Libraries are normally face-down; searching is an exception that lets a player look through cards.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    // CR 701.23 + CR 609.3: If a CantSearchLibrary static muzzles the cause of this
    // search, the search does nothing. Per CR 609.3, an effect that attempts to do
    // something impossible does only as much as possible — so we skip the search
    // entirely, do NOT mark the turn-tracking flag, and emit only the resolution
    // event so downstream bookkeeping sees a completed (no-op) effect.
    if is_search_muzzled(state, ability.controller) {
        events.push(GameEvent::EffectResolved {
            kind: EffectKind::SearchLibrary,
            source_id: ability.source_id,
        });
        return Ok(());
    }

    // CR 107.3a + CR 601.2b: Resolve the count expression against the ability so
    // `Variable("X")` picks up the caster's announced X. Fixed counts are unaffected.
    // CR 107.1c + CR 701.23d: Peel `UpTo` from the count expression to derive
    // the upper-bound expression and propagate the may-pick-fewer flag to
    // SearchChoice. Plain `QuantityExpr` means a mandatory count; wrapped
    // in `UpTo` means "any number of" / "up to N" — searcher picks 0..=count.
    let (filter, count, reveal, target_player, up_to, selection_constraint) = match &ability.effect
    {
        Effect::SearchLibrary {
            filter,
            count,
            reveal,
            target_player,
            selection_constraint,
        } => {
            let (inner, up_to) = count.peel_up_to();
            (
                filter.clone(),
                resolve_quantity_with_targets(state, inner, ability).max(0) as usize,
                *reveal,
                target_player.clone(),
                up_to,
                selection_constraint.clone(),
            )
        }
        _ => (
            TargetFilter::Any,
            1,
            false,
            None,
            false,
            SearchSelectionConstraint::None,
        ),
    };

    // CR 701.23a: Determine the library owner and the searcher.
    //   - Library owner: the player whose library is searched (driven by
    //     `target_player` when set, caster otherwise).
    //   - Searcher: the player carrying out the "search" instruction
    //     (library owner for subject-anchored target_player variants, caster
    //     otherwise — matching the Oracle-text grammatical subject of
    //     "search").
    let library_owner_id = match target_player.as_ref() {
        Some(filter) => resolve_library_owner(state, ability, filter),
        None => ability.controller,
    };
    let searcher_id = match target_player.as_ref() {
        Some(filter) if searcher_is_library_owner(filter) => library_owner_id,
        _ => ability.controller,
    };

    let player = state
        .players
        .iter()
        .find(|p| p.id == library_owner_id)
        .ok_or(EffectError::PlayerNotFound)?;
    events.push(GameEvent::PlayerPerformedAction {
        player_id: searcher_id,
        action: PlayerActionKind::SearchedLibrary,
    });
    state
        .players_who_searched_library_this_turn
        .insert(searcher_id);

    // CR 107.3a + CR 601.2b: Evaluate the filter with the resolving ability
    // in scope so dynamic thresholds (e.g. `CmcLE { value: Variable("X") }`
    // for Nature's Rhythm) resolve against the caster's announced X.
    let filter_ctx = FilterContext::from_ability(ability);
    let matching: Vec<_> = player
        .library
        .iter()
        .filter(|&&obj_id| matches_target_filter(state, obj_id, &filter, &filter_ctx))
        .copied()
        .collect();

    if matching.is_empty() {
        // CR 701.23b: A player searching a hidden zone isn't required to find
        // cards even if they're present ("fail to find"). Resolve immediately.
        events.push(GameEvent::EffectResolved {
            kind: EffectKind::SearchLibrary,
            source_id: ability.source_id,
        });
        return Ok(());
    }

    let pick_count = count.min(matching.len());

    // CR 608.2c: Propagate the printed-text selection restriction (e.g.,
    // "with different names") into the choice state so the Select handler
    // and AI candidate enumerator both see it.
    state.waiting_for = WaitingFor::SearchChoice {
        player: searcher_id,
        cards: matching,
        count: pick_count,
        reveal,
        up_to,
        constraint: selection_constraint,
    };

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::SearchLibrary,
        source_id: ability.source_id,
    });

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::ability::{QuantityExpr, TypedFilter};
    use crate::types::card_type::CoreType;
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::player::PlayerId;
    use crate::types::zones::Zone;

    fn make_search_ability(filter: TargetFilter, count: i32) -> ResolvedAbility {
        ResolvedAbility::new(
            Effect::SearchLibrary {
                filter,
                count: QuantityExpr::Fixed { value: count },
                reveal: false,
                target_player: None,
                selection_constraint: SearchSelectionConstraint::None,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        )
    }

    fn make_search_ability_up_to(filter: TargetFilter, count: i32) -> ResolvedAbility {
        // CR 107.1c: "any number of" / "up to N" — searcher picks 0..=count.
        ResolvedAbility::new(
            Effect::SearchLibrary {
                filter,
                count: QuantityExpr::up_to(QuantityExpr::Fixed { value: count }),
                reveal: false,
                target_player: None,
                selection_constraint: SearchSelectionConstraint::None,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        )
    }

    fn add_library_creature(
        state: &mut GameState,
        card_id: u64,
        owner: PlayerId,
        name: &str,
    ) -> ObjectId {
        let id = create_object(
            state,
            CardId(card_id),
            owner,
            name.to_string(),
            Zone::Library,
        );
        state
            .objects
            .get_mut(&id)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);
        id
    }

    fn add_library_land(
        state: &mut GameState,
        card_id: u64,
        owner: PlayerId,
        name: &str,
        basic: bool,
    ) -> ObjectId {
        let id = create_object(
            state,
            CardId(card_id),
            owner,
            name.to_string(),
            Zone::Library,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types = vec![CoreType::Land];
        if basic {
            obj.card_types
                .supertypes
                .push(crate::types::card_type::Supertype::Basic);
        }
        id
    }

    #[test]
    fn search_finds_matching_cards_sets_search_choice() {
        let mut state = GameState::new_two_player(42);
        let bear = add_library_creature(&mut state, 1, PlayerId(0), "Bear");
        let _land = add_library_land(&mut state, 2, PlayerId(0), "Forest", true);

        let ability = make_search_ability(TargetFilter::Typed(TypedFilter::creature()), 1);
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(events.iter().any(|event| matches!(
            event,
            GameEvent::PlayerPerformedAction {
                player_id,
                action: PlayerActionKind::SearchedLibrary,
            } if *player_id == PlayerId(0)
        )));

        match &state.waiting_for {
            WaitingFor::SearchChoice {
                player,
                cards,
                count,
                reveal,
                ..
            } => {
                assert_eq!(*player, PlayerId(0));
                assert_eq!(*count, 1);
                assert!(!reveal);
                assert!(cards.contains(&bear), "Should contain the creature");
                assert_eq!(cards.len(), 1, "Should NOT contain the land");
            }
            other => panic!("Expected SearchChoice, got {:?}", other),
        }
    }

    #[test]
    fn search_up_to_propagates_flag_and_floors_sentinel_to_matching_len() {
        // CR 107.1c: Sarkhan -7 pattern — "any number of Dragon creature cards".
        // Parser emits count=i32::MAX + up_to=true; resolver must floor pick_count
        // to matching.len() AND propagate up_to=true into SearchChoice.
        let mut state = GameState::new_two_player(42);
        let _c1 = add_library_creature(&mut state, 1, PlayerId(0), "Dragon A");
        let _c2 = add_library_creature(&mut state, 2, PlayerId(0), "Dragon B");

        let ability =
            make_search_ability_up_to(TargetFilter::Typed(TypedFilter::creature()), i32::MAX);
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        match &state.waiting_for {
            WaitingFor::SearchChoice {
                count,
                up_to,
                cards,
                ..
            } => {
                assert!(*up_to, "up_to should propagate into SearchChoice");
                assert_eq!(*count, 2, "pick_count should floor to matching.len()");
                assert_eq!(cards.len(), 2);
            }
            other => panic!("Expected SearchChoice, got {:?}", other),
        }
    }

    #[test]
    fn search_with_any_filter_shows_all_library_cards() {
        let mut state = GameState::new_two_player(42);
        let card1 = add_library_creature(&mut state, 1, PlayerId(0), "Bear");
        let card2 = add_library_land(&mut state, 2, PlayerId(0), "Forest", true);

        let ability = make_search_ability(TargetFilter::Any, 1);
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        match &state.waiting_for {
            WaitingFor::SearchChoice { cards, .. } => {
                assert_eq!(cards.len(), 2);
                assert!(cards.contains(&card1));
                assert!(cards.contains(&card2));
            }
            other => panic!("Expected SearchChoice, got {:?}", other),
        }
    }

    #[test]
    fn search_empty_library_resolves_immediately() {
        let mut state = GameState::new_two_player(42);
        assert!(state.players[0].library.is_empty());

        let ability = make_search_ability(TargetFilter::Any, 1);
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        // Should NOT set SearchChoice — fail to find
        assert!(
            !matches!(state.waiting_for, WaitingFor::SearchChoice { .. }),
            "Should not set SearchChoice for empty library"
        );
        assert!(events.iter().any(|event| matches!(
            event,
            GameEvent::PlayerPerformedAction {
                player_id,
                action: PlayerActionKind::SearchedLibrary,
            } if *player_id == PlayerId(0)
        )));
        assert!(events.iter().any(|e| matches!(
            e,
            GameEvent::EffectResolved {
                kind: EffectKind::SearchLibrary,
                ..
            }
        )));
    }

    #[test]
    fn search_no_matches_resolves_immediately() {
        let mut state = GameState::new_two_player(42);
        // Only lands in library, searching for creatures
        add_library_land(&mut state, 1, PlayerId(0), "Forest", true);
        add_library_land(&mut state, 2, PlayerId(0), "Plains", true);

        let ability = make_search_ability(TargetFilter::Typed(TypedFilter::creature()), 1);
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(
            !matches!(state.waiting_for, WaitingFor::SearchChoice { .. }),
            "Should not set SearchChoice when no cards match"
        );
    }

    /// CR 701.23b + CR 701.20a: End-to-end Ranging Raptors / Rampant Growth shape —
    /// SearchLibrary(basic land) → ChangeZone(Library→Battlefield, Any) → Shuffle.
    /// When the library contains no matching cards, the search fails to find,
    /// the put-step must no-op (via the change_zone Library+Any+empty-targets
    /// guard), and the trailing Shuffle MUST still fire. This locks down the
    /// full chain traversal that the change_zone unit test alone cannot verify.
    #[test]
    fn search_fail_to_find_preserves_shuffle_tail() {
        use crate::game::effects::resolve_ability_chain;
        use crate::types::ability::Effect;

        let mut state = GameState::new_two_player(42);
        // Library has only non-basic cards; the search for a basic land will
        // fail to find. Both players seeded so a regression that scans across
        // libraries would have candidates to pull.
        let p0_nonbasic = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Non-basic".to_string(),
            Zone::Library,
        );
        let p1_card = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Opponent Card".to_string(),
            Zone::Library,
        );
        let battlefield_before = state.battlefield.clone();

        // Chain: Search(basic land) → ChangeZone(Library→Battlefield, Any) → Shuffle.
        let shuffle_step = ResolvedAbility::new(
            Effect::Shuffle {
                target: TargetFilter::Controller,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        let put_step = ResolvedAbility::new(
            Effect::ChangeZone {
                origin: Some(Zone::Library),
                destination: Zone::Battlefield,
                target: TargetFilter::Any,
                owner_library: false,
                enter_transformed: false,
                under_your_control: false,
                enter_tapped: true,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        )
        .sub_ability(shuffle_step);
        let search_step = ResolvedAbility::new(
            Effect::SearchLibrary {
                filter: TargetFilter::Typed(TypedFilter::land().properties(vec![
                    crate::types::ability::FilterProp::HasSupertype {
                        value: crate::types::card_type::Supertype::Basic,
                    },
                ])),
                count: QuantityExpr::Fixed { value: 1 },
                reveal: false,
                target_player: None,
                selection_constraint: SearchSelectionConstraint::None,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        )
        .sub_ability(put_step);

        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &search_step, &mut events, 0).unwrap();

        assert_eq!(
            state.battlefield, battlefield_before,
            "Fail-to-find must NOT move any library card onto the battlefield"
        );
        assert_eq!(
            state.objects[&p0_nonbasic].zone,
            Zone::Library,
            "Non-basic library card stays put on fail-to-find"
        );
        assert_eq!(
            state.objects[&p1_card].zone,
            Zone::Library,
            "Opponent library card must not be reachable from a fail-to-find put-step"
        );
        assert!(
            !matches!(state.waiting_for, WaitingFor::EffectZoneChoice { .. }),
            "Fail-to-find must not prompt an EffectZoneChoice (the reported bug)"
        );
        assert!(
            events.iter().any(|e| matches!(
                e,
                GameEvent::EffectResolved {
                    kind: EffectKind::Shuffle,
                    ..
                }
            )),
            "Trailing Shuffle MUST fire even when the search found nothing \
             (CR 701.20a: the 'then shuffle' tail is unconditional)"
        );
    }

    #[test]
    fn search_choice_change_zone_continuation_moves_selected_card_to_hand() {
        use crate::game::effects::resolve_ability_chain;
        use crate::game::engine::apply;
        use crate::types::ability::Effect;
        use crate::types::actions::GameAction;

        let mut state = GameState::new_two_player(42);
        let land = add_library_land(&mut state, 1, PlayerId(0), "Forest", true);

        let shuffle_step = ResolvedAbility::new(
            Effect::Shuffle {
                target: TargetFilter::Controller,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        let put_step = ResolvedAbility::new(
            Effect::ChangeZone {
                origin: Some(Zone::Library),
                destination: Zone::Hand,
                target: TargetFilter::Any,
                owner_library: false,
                enter_transformed: false,
                under_your_control: false,
                enter_tapped: false,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        )
        .sub_ability(shuffle_step);
        let search_step = ResolvedAbility::new(
            Effect::SearchLibrary {
                filter: TargetFilter::Typed(TypedFilter::land().properties(vec![
                    crate::types::ability::FilterProp::HasSupertype {
                        value: crate::types::card_type::Supertype::Basic,
                    },
                ])),
                count: QuantityExpr::Fixed { value: 1 },
                reveal: true,
                target_player: None,
                selection_constraint: SearchSelectionConstraint::None,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        )
        .sub_ability(put_step);

        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &search_step, &mut events, 0).unwrap();
        assert!(matches!(state.waiting_for, WaitingFor::SearchChoice { .. }));

        apply(
            &mut state,
            PlayerId(0),
            GameAction::SelectCards { cards: vec![land] },
        )
        .unwrap();

        assert_eq!(state.objects[&land].zone, Zone::Hand);
        assert!(state.players[0].hand.contains(&land));
    }

    #[test]
    fn search_only_searches_controllers_library() {
        let mut state = GameState::new_two_player(42);
        let _opponent_creature = add_library_creature(&mut state, 1, PlayerId(1), "Opponent Bear");
        // Controller has no creatures
        add_library_land(&mut state, 2, PlayerId(0), "Forest", true);

        let ability = make_search_ability(TargetFilter::Typed(TypedFilter::creature()), 1);
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        // Should fail to find — opponent's library is not searched
        assert!(
            !matches!(state.waiting_for, WaitingFor::SearchChoice { .. }),
            "Should not search opponent's library"
        );
    }

    #[test]
    fn search_with_reveal_sets_reveal_flag() {
        let mut state = GameState::new_two_player(42);
        add_library_creature(&mut state, 1, PlayerId(0), "Bear");

        let ability = ResolvedAbility::new(
            Effect::SearchLibrary {
                filter: TargetFilter::Typed(TypedFilter::creature()),
                count: QuantityExpr::Fixed { value: 1 },
                reveal: true,
                target_player: None,
                selection_constraint: SearchSelectionConstraint::None,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        match &state.waiting_for {
            WaitingFor::SearchChoice { reveal, .. } => {
                assert!(*reveal);
            }
            other => panic!("Expected SearchChoice, got {:?}", other),
        }
    }

    fn add_library_creature_with_cmc(
        state: &mut GameState,
        card_id: u64,
        owner: PlayerId,
        name: &str,
        cmc: u32,
    ) -> ObjectId {
        use crate::types::mana::ManaCost;
        let id = create_object(
            state,
            CardId(card_id),
            owner,
            name.to_string(),
            Zone::Library,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.mana_cost = ManaCost::generic(cmc);
        id
    }

    /// CR 107.3a + CR 601.2b: Nature's Rhythm — search for a creature card with mana
    /// value X or less. With X=4, only CMC-≤-4 creatures should be selectable,
    /// regardless of what's in the library.
    #[test]
    fn natures_rhythm_x_mana_value_restricts_search_targets() {
        use crate::types::ability::{FilterProp, QuantityExpr, QuantityRef};
        let mut state = GameState::new_two_player(42);
        let cmc2 = add_library_creature_with_cmc(&mut state, 1, PlayerId(0), "Small", 2);
        let cmc4 = add_library_creature_with_cmc(&mut state, 2, PlayerId(0), "Mid", 4);
        add_library_creature_with_cmc(&mut state, 3, PlayerId(0), "Large", 5);
        add_library_creature_with_cmc(&mut state, 4, PlayerId(0), "Behemoth", 8);

        let filter =
            TargetFilter::Typed(TypedFilter::creature().properties(vec![FilterProp::CmcLE {
                value: QuantityExpr::Ref {
                    qty: QuantityRef::Variable {
                        name: "X".to_string(),
                    },
                },
            }]));
        let mut ability = ResolvedAbility::new(
            Effect::SearchLibrary {
                filter,
                count: QuantityExpr::Fixed { value: 1 },
                reveal: false,
                target_player: None,
                selection_constraint: SearchSelectionConstraint::None,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        ability.chosen_x = Some(4);

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        match &state.waiting_for {
            WaitingFor::SearchChoice { cards, .. } => {
                assert_eq!(cards.len(), 2, "Expected only CMC-2 and CMC-4 creatures");
                assert!(cards.contains(&cmc2));
                assert!(cards.contains(&cmc4));
            }
            other => panic!("Expected SearchChoice, got {:?}", other),
        }
    }

    /// CR 107.3b: X=0 restricts to CMC-0 creatures only.
    #[test]
    fn natures_rhythm_x_zero_restricts_to_cmc_zero_creatures() {
        use crate::types::ability::{FilterProp, QuantityExpr, QuantityRef};
        let mut state = GameState::new_two_player(42);
        let zero_cmc = add_library_creature_with_cmc(&mut state, 1, PlayerId(0), "Zero", 0);
        add_library_creature_with_cmc(&mut state, 2, PlayerId(0), "NonZero", 2);

        let filter =
            TargetFilter::Typed(TypedFilter::creature().properties(vec![FilterProp::CmcLE {
                value: QuantityExpr::Ref {
                    qty: QuantityRef::Variable {
                        name: "X".to_string(),
                    },
                },
            }]));
        let mut ability = ResolvedAbility::new(
            Effect::SearchLibrary {
                filter,
                count: QuantityExpr::Fixed { value: 1 },
                reveal: false,
                target_player: None,
                selection_constraint: SearchSelectionConstraint::None,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        ability.chosen_x = Some(0);

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        match &state.waiting_for {
            WaitingFor::SearchChoice { cards, .. } => {
                assert_eq!(cards.len(), 1);
                assert!(cards.contains(&zero_cmc));
            }
            other => panic!("Expected SearchChoice, got {:?}", other),
        }
    }

    /// CR 107.3a: `SearchLibrary.count = Variable("X")` with `chosen_x = 3` →
    /// `pick_count == 3`.
    #[test]
    fn search_library_with_x_count_picks_x_cards() {
        use crate::types::ability::{QuantityExpr, QuantityRef};
        let mut state = GameState::new_two_player(42);
        for i in 0..5 {
            add_library_creature(&mut state, 1 + i as u64, PlayerId(0), &format!("C{i}"));
        }

        let mut ability = ResolvedAbility::new(
            Effect::SearchLibrary {
                filter: TargetFilter::Typed(TypedFilter::creature()),
                count: QuantityExpr::Ref {
                    qty: QuantityRef::Variable {
                        name: "X".to_string(),
                    },
                },
                reveal: false,
                target_player: None,
                selection_constraint: SearchSelectionConstraint::None,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        ability.chosen_x = Some(3);

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        match &state.waiting_for {
            WaitingFor::SearchChoice { count, .. } => {
                assert_eq!(*count, 3);
            }
            other => panic!("Expected SearchChoice, got {:?}", other),
        }
    }

    // === CR 701.23 + CR 609.3: CantSearchLibrary runtime enforcement tests ===

    use crate::types::ability::StaticDefinition;
    use crate::types::statics::{ProhibitionScope, StaticMode};

    fn add_cant_search_library_permanent(
        state: &mut GameState,
        controller: PlayerId,
        cause: ProhibitionScope,
    ) -> ObjectId {
        let id = create_object(
            state,
            CardId(0xA51),
            controller,
            "Ashiok".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Planeswalker);
        obj.entered_battlefield_turn = Some(0);
        obj.static_definitions
            .push(StaticDefinition::new(StaticMode::CantSearchLibrary {
                cause,
            }));
        id
    }

    #[test]
    fn ashiok_muzzles_opponent_caused_search() {
        // CR 701.23 + CR 609.3: Ashiok on P0's battlefield (cause=Opponents). A P1
        // spell/ability resolving into a search is muzzled: no library inspection,
        // no turn-flag mutation, no SearchChoice state transition.
        let mut state = GameState::new_two_player(42);
        add_cant_search_library_permanent(&mut state, PlayerId(0), ProhibitionScope::Opponents);

        // P1 library contains searchable creatures.
        let _bear = add_library_creature(&mut state, 1, PlayerId(1), "Bear");
        let _runeclaw = add_library_creature(&mut state, 2, PlayerId(1), "Runeclaw Bear");

        // Ability controller = P1 (the opponent of Ashiok's controller).
        let ability = ResolvedAbility::new(
            Effect::SearchLibrary {
                filter: TargetFilter::Typed(TypedFilter::creature()),
                count: QuantityExpr::Fixed { value: 1 },
                reveal: false,
                target_player: None,
                selection_constraint: SearchSelectionConstraint::None,
            },
            vec![],
            ObjectId(9999),
            PlayerId(1),
        );

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        // CR 609.3: No progress. No PlayerPerformedAction::SearchedLibrary event,
        // no turn-flag mutation, no SearchChoice waiting state.
        assert!(
            !events.iter().any(
                |e| matches!(e, GameEvent::PlayerPerformedAction { action, .. }
                    if matches!(action, PlayerActionKind::SearchedLibrary))
            ),
            "Muzzled search must NOT emit PlayerPerformedAction::SearchedLibrary"
        );
        assert!(
            !state
                .players_who_searched_library_this_turn
                .contains(&PlayerId(1)),
            "Muzzled search must NOT mark the turn-tracking flag"
        );
        assert!(
            !matches!(state.waiting_for, WaitingFor::SearchChoice { .. }),
            "Muzzled search must NOT transition to SearchChoice"
        );
        // EffectResolved is emitted so downstream bookkeeping sees a completed (no-op) effect.
        assert!(
            events.iter().any(|e| matches!(
                e,
                GameEvent::EffectResolved {
                    kind: EffectKind::SearchLibrary,
                    ..
                }
            )),
            "Muzzled search must emit a completed EffectResolved event (CR 609.3 no-op)"
        );
    }

    #[test]
    fn ashiok_permits_own_controller_search() {
        // CR 701.23: Ashiok's static is `cause = Opponents`. Its own controller's
        // searches are not muzzled.
        let mut state = GameState::new_two_player(42);
        add_cant_search_library_permanent(&mut state, PlayerId(0), ProhibitionScope::Opponents);

        let _bear = add_library_creature(&mut state, 1, PlayerId(0), "Bear");

        // Ability controller = P0 (Ashiok's own controller).
        let ability = ResolvedAbility::new(
            Effect::SearchLibrary {
                filter: TargetFilter::Typed(TypedFilter::creature()),
                count: QuantityExpr::Fixed { value: 1 },
                reveal: false,
                target_player: None,
                selection_constraint: SearchSelectionConstraint::None,
            },
            vec![],
            ObjectId(9998),
            PlayerId(0),
        );

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(
            state
                .players_who_searched_library_this_turn
                .contains(&PlayerId(0)),
            "Non-muzzled search must mark the turn-tracking flag"
        );
        assert!(
            matches!(state.waiting_for, WaitingFor::SearchChoice { .. }),
            "Non-muzzled search must transition to SearchChoice"
        );
    }

    /// CR 608.2c + CR 701.23a: Assassin's Trophy-shape search with
    /// `target_player = Some(ParentTargetController)` + parent target = an
    /// opponent's permanent. The opponent (destroyed permanent's controller)
    /// is both the library owner AND the searcher — `WaitingFor::SearchChoice`
    /// must prompt them, and the turn-tracking flag must record them, not the
    /// caster.
    #[test]
    fn parent_target_controller_search_prompts_opponent() {
        let mut state = GameState::new_two_player(42);
        // Opponent (P1) owns the destroyed permanent and the library to search.
        let destroyed = create_object(
            &mut state,
            CardId(100),
            PlayerId(1),
            "Opponent Land".to_string(),
            Zone::Graveyard,
        );
        state.objects.get_mut(&destroyed).unwrap().controller = PlayerId(1);
        let _opp_basic = add_library_land(&mut state, 1, PlayerId(1), "Forest", true);

        // Caster (P0) casts the spell. Parent target is the destroyed permanent.
        let ability = ResolvedAbility::new(
            Effect::SearchLibrary {
                filter: TargetFilter::Typed(TypedFilter::land().properties(vec![
                    crate::types::ability::FilterProp::HasSupertype {
                        value: crate::types::card_type::Supertype::Basic,
                    },
                ])),
                count: QuantityExpr::Fixed { value: 1 },
                reveal: false,
                target_player: Some(TargetFilter::ParentTargetController),
                selection_constraint: SearchSelectionConstraint::None,
            },
            vec![TargetRef::Object(destroyed)],
            ObjectId(9997),
            PlayerId(0),
        );

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        match &state.waiting_for {
            WaitingFor::SearchChoice { player, .. } => assert_eq!(
                *player,
                PlayerId(1),
                "SearchChoice must prompt the destroyed permanent's controller (opponent), not the caster"
            ),
            other => panic!("expected SearchChoice, got {:?}", other),
        }
        assert!(
            state
                .players_who_searched_library_this_turn
                .contains(&PlayerId(1)),
            "turn-tracking flag must record the searcher (opponent)"
        );
        assert!(
            !state
                .players_who_searched_library_this_turn
                .contains(&PlayerId(0)),
            "caster did NOT search — turn-tracking flag must not record them"
        );
    }

    /// CR 701.23a: Praetor's Grasp-shape regression — "search target opponent's
    /// library". The caster picks through the opponent's library (searcher =
    /// caster). Guards against the new ParentTargetController resolver arm
    /// incorrectly re-routing all `target_player`-set searches to the library
    /// owner.
    #[test]
    fn target_opponent_library_search_keeps_caster_as_searcher() {
        use crate::types::ability::{ControllerRef, TypedFilter};
        let mut state = GameState::new_two_player(42);
        let _opp_card = add_library_creature(&mut state, 1, PlayerId(1), "Bribed Bear");

        // "Search target opponent's library" — caster = P0, targeted player = P1.
        let ability = ResolvedAbility::new(
            Effect::SearchLibrary {
                filter: TargetFilter::Typed(TypedFilter::creature()),
                count: QuantityExpr::Fixed { value: 1 },
                reveal: false,
                target_player: Some(TargetFilter::Typed(
                    TypedFilter::default().controller(ControllerRef::Opponent),
                )),
                selection_constraint: SearchSelectionConstraint::None,
            },
            vec![TargetRef::Player(PlayerId(1))],
            ObjectId(9996),
            PlayerId(0),
        );

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        match &state.waiting_for {
            WaitingFor::SearchChoice { player, .. } => assert_eq!(
                *player,
                PlayerId(0),
                "Praetor's Grasp-style search: the CASTER browses the opponent's library"
            ),
            other => panic!("expected SearchChoice, got {:?}", other),
        }
        assert!(
            state
                .players_who_searched_library_this_turn
                .contains(&PlayerId(0)),
            "caster is the searcher for 'search target opponent's library'"
        );
    }
}
