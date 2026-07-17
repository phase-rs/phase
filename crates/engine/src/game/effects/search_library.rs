use crate::game::filter::{
    matches_target_filter, matches_target_filter_in_owner_zone, FilterContext,
};
use crate::game::quantity::resolve_quantity_with_targets;
use crate::game::static_abilities::prohibition_scope_matches_player;
use crate::types::ability::{
    Effect, EffectError, EffectKind, ResolvedAbility, SearchSelectionConstraint, SharedQuality,
    TargetFilter, TargetRef,
};
use crate::types::card_type::is_land_subtype;
use crate::types::events::{GameEvent, PlayerActionKind};
use crate::types::game_state::{GameState, WaitingFor};
use crate::types::player::PlayerId;
use crate::types::statics::StaticMode;
use crate::types::zones::Zone;

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
        if let Some(player) = crate::game::ability_utils::parent_target_controller(ability, state) {
            return player;
        }
    }
    // CR 701.23a + CR 108.3 / CR 109.4: An object-relative name-hate search
    // (The End, Deadly Cover-Up, Crumble to Dust, Surgical Extraction, Test of
    // Talents, Deicide) carries its searched player as a `Typed` controller-ref
    // (`ParentTargetOwner` / `ParentTargetController`) on `target_player`. Only
    // the *searched zones' owner* is derived here; the caster remains the
    // searcher (`searcher_is_library_owner(Typed{..}) == false`, CR 701.23a
    // asymmetric). The bare `ParentTargetController` branch above is untouched so
    // Assassin's Trophy's self-search is preserved.
    if let TargetFilter::Typed(tf) = target_player {
        if let Some(ctrl) = &tf.controller {
            if let Some(pid) = crate::game::filter::controller_ref_player(
                state,
                ability.source_id,
                Some(ability.controller),
                Some(ability),
                ctrl,
            ) {
                return pid;
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
        // CR 702.124j: "Partner with" — target player searches their own library,
        // so when the target is selected via TargetFilter::Player (any player),
        // the targeted player is both the library owner and the searcher.
        // Asymmetric searches (Bribery, Praetor's Grasp) use Opponent/specific
        // player targets where the controller is the searcher — those use
        // target_player: Some(Opponent) which falls through to the else branch.
        TargetFilter::Player
            | TargetFilter::ParentTargetController
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

/// CR 701.23f + CR 614.1a: smallest active top-N library-search restriction
/// applying to `searcher_id` (a searcher restricted by a
/// `RestrictLibrarySearchToTop` source under its controller-relative `who`).
/// None = unrestricted. Multiple sources stack as the minimum (Aven Mindcensor
/// pairs all converge on the tightest cap).
fn library_search_top_limit(state: &GameState, searcher_id: PlayerId) -> Option<u32> {
    crate::game::functioning_abilities::battlefield_active_statics(state)
        .filter_map(|(bf_obj, def)| match def.mode {
            StaticMode::RestrictLibrarySearchToTop { ref who, count }
                if prohibition_scope_matches_player(who, searcher_id, bf_obj.id, state) =>
            {
                Some(count)
            }
            _ => None,
        })
        .min()
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
        SearchSelectionConstraint::DistinctQualities { qualities } => qualities
            .iter()
            .all(|quality| selection_has_distinct_quality(state, chosen, quality)),
        SearchSelectionConstraint::TotalManaValue { comparator, value } => {
            let mut total = 0;
            for id in chosen {
                let Some(obj) = state.objects.get(id) else {
                    return false;
                };
                // CR 202.3d + CR 709.4b: searched cards are in a library (off the
                // stack), so a split card contributes its combined mana value.
                total += obj.effective_mana_value() as i32;
            }
            comparator.evaluate(total, *value)
        }
        SearchSelectionConstraint::MatchEachFilter { filters } => {
            selection_matches_each_filter(state, chosen, filters)
        }
    }
}

fn selection_matches_each_filter(
    state: &GameState,
    chosen: &[crate::types::identifiers::ObjectId],
    filters: &[TargetFilter],
) -> bool {
    // CR 701.23b: stated-quality search may fail to find some or all cards.
    // A short (or empty) selection is legal as long as every chosen card can
    // claim a distinct, matching, unused filter slot — leftover slots stay
    // unfilled. Over-selection (more cards than slots) is still illegal.
    if chosen.len() > filters.len() {
        return false;
    }
    let mut used = vec![false; filters.len()];
    assign_each_chosen_to_distinct_slot(state, chosen, filters, &mut used, 0)
}

/// Card-anchored backtracking matcher: each chosen card must claim a distinct,
/// matching, still-unused filter slot. Leftover slots may go unfilled — the
/// fail-to-find case (CR 701.23b). Backtracking (not greedy) is
/// required: for filters `[basic-or-Shrine, Shrine]` and chosen `[shrine, basic]`,
/// a greedy first-match would bind shrine→slot0 then fail basic→slot1, falsely
/// rejecting the legal assignment shrine→slot1, basic→slot0.
fn assign_each_chosen_to_distinct_slot(
    state: &GameState,
    chosen: &[crate::types::identifiers::ObjectId],
    filters: &[TargetFilter],
    used: &mut [bool],
    card_idx: usize,
) -> bool {
    if card_idx == chosen.len() {
        return true;
    }

    let filter_ctx = FilterContext::neutral();
    let card_id = chosen[card_idx];
    (0..filters.len()).any(|slot_idx| {
        if used[slot_idx] || !matches_target_filter(state, card_id, &filters[slot_idx], &filter_ctx)
        {
            return false;
        }
        used[slot_idx] = true;
        let matched =
            assign_each_chosen_to_distinct_slot(state, chosen, filters, used, card_idx + 1);
        used[slot_idx] = false;
        matched
    })
}

fn selection_has_distinct_quality(
    state: &GameState,
    chosen: &[crate::types::identifiers::ObjectId],
    quality: &SharedQuality,
) -> bool {
    match quality {
        SharedQuality::Name => {
            let mut seen = std::collections::HashSet::new();
            chosen.iter().all(|id| match state.objects.get(id) {
                // CR 201.2b: searched cards have different names only if each
                // has a name and no two objects in the group have a name in common.
                Some(obj) if !obj.name.is_empty() => seen.insert(obj.name.as_str()),
                _ => false,
            })
        }
        SharedQuality::ManaValue => {
            let mut seen = std::collections::HashSet::new();
            chosen.iter().all(|id| match state.objects.get(id) {
                // CR 202.3d + CR 709.4b: distinct combined mana values off the
                // stack (a split card's MV is both halves combined).
                Some(obj) => seen.insert(obj.effective_mana_value()),
                None => false,
            })
        }
        SharedQuality::Power => {
            let mut seen = std::collections::HashSet::new();
            chosen.iter().all(|id| match state.objects.get(id) {
                Some(obj) => obj.power.is_none_or(|power| seen.insert(power)),
                None => false,
            })
        }
        SharedQuality::Toughness => {
            let mut seen = std::collections::HashSet::new();
            chosen.iter().all(|id| match state.objects.get(id) {
                Some(obj) => obj.toughness.is_none_or(|toughness| seen.insert(toughness)),
                None => false,
            })
        }
        SharedQuality::TotalPowerToughness => {
            let mut seen = std::collections::HashSet::new();
            chosen.iter().all(|id| match state.objects.get(id) {
                Some(obj) => obj
                    .power
                    .zip(obj.toughness)
                    .is_none_or(|(power, toughness)| seen.insert(power + toughness)),
                None => false,
            })
        }
        SharedQuality::CardType => {
            let mut seen = std::collections::HashSet::new();
            chosen.iter().all(|id| match state.objects.get(id) {
                Some(obj) => obj
                    .card_types
                    .core_types
                    .iter()
                    .all(|card_type| seen.insert(*card_type)),
                None => false,
            })
        }
        // CR 110.4: distinct-permanent-type check ignores non-permanent card
        // types (Kindred/Tribal etc.), so only permanent types are inserted.
        SharedQuality::PermanentType => {
            let mut seen = std::collections::HashSet::new();
            chosen.iter().all(|id| match state.objects.get(id) {
                Some(obj) => obj
                    .card_types
                    .core_types
                    .iter()
                    .filter(|card_type| card_type.is_permanent_type())
                    .all(|card_type| seen.insert(*card_type)),
                None => false,
            })
        }
        SharedQuality::CreatureType => {
            distinct_string_sets(state, chosen, |obj| obj.card_types.subtypes.clone())
        }
        // CR 709.4b: combined colors off the stack for a split card.
        SharedQuality::Color => distinct_string_sets(state, chosen, |obj| {
            obj.effective_colors()
                .iter()
                .map(|color| format!("{color:?}"))
                .collect()
        }),
        SharedQuality::LandType => distinct_string_sets(state, chosen, |obj| {
            obj.card_types
                .subtypes
                .iter()
                .filter(|subtype| is_land_subtype(subtype))
                .cloned()
                .collect()
        }),
    }
}

fn distinct_string_sets(
    state: &GameState,
    chosen: &[crate::types::identifiers::ObjectId],
    values: impl Fn(&crate::game::game_object::GameObject) -> Vec<String>,
) -> bool {
    let mut seen = std::collections::HashSet::new();
    chosen.iter().all(|id| match state.objects.get(id) {
        Some(obj) => values(obj).into_iter().all(|value| seen.insert(value)),
        None => false,
    })
}

fn search_filter_has_stated_quality(filter: &TargetFilter) -> bool {
    match filter {
        TargetFilter::Any | TargetFilter::None => false,
        TargetFilter::Typed(typed) => {
            typed.has_meaningful_type_constraint() || typed.controller.is_some()
        }
        TargetFilter::Or { filters } | TargetFilter::And { filters } => {
            filters.iter().any(search_filter_has_stated_quality)
        }
        TargetFilter::Not { filter } => search_filter_has_stated_quality(filter),
        _ => true,
    }
}

// CR 701.23b: Hidden-zone searches with a stated quality can fail to find
// some or all matching cards without converting the search filter into a
// selection-time constraint.
fn allows_hidden_zone_partial_find(filter: &TargetFilter, source_zones: &[Zone]) -> bool {
    source_zones == [Zone::Library] && search_filter_has_stated_quality(filter)
}

/// CR 701.23a + CR 401.2: Search a library — look through it, find card(s) matching criteria, then shuffle.
/// CR 401.2: Libraries are normally face-down; searching is an exception that lets a player look through cards.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    // CR 107.3a + CR 601.2b: Resolve the count expression against the ability so
    // `Variable("X")` picks up the caster's announced X. Fixed counts are unaffected.
    // CR 107.1c + CR 701.23d: Peel `UpTo` from the count expression to derive
    // the upper-bound expression and propagate the may-pick-fewer flag to
    // SearchChoice. Plain `QuantityExpr` means a mandatory count; wrapped
    // in `UpTo` means "any number of" / "up to N" — searcher picks 0..=count.
    let (filter, count, reveal, target_player, up_to, selection_constraint, split, source_zones) =
        match &ability.effect {
            Effect::SearchLibrary {
                filter,
                count,
                reveal,
                target_player,
                selection_constraint,
                split,
                source_zones,
            } => {
                let (inner, up_to) = count.peel_up_to();
                (
                    filter.clone(),
                    resolve_quantity_with_targets(state, inner, ability).max(0) as usize,
                    *reveal,
                    target_player.clone(),
                    up_to,
                    selection_constraint.clone(),
                    split.clone(),
                    source_zones.clone(),
                )
            }
            _ => (
                TargetFilter::Any,
                1,
                false,
                None,
                false,
                SearchSelectionConstraint::None,
                None,
                vec![Zone::Library],
            ),
        };

    // CR 701.23 + CR 609.3: If a CantSearchLibrary static muzzles the cause of
    // this search, the library can't be searched. Per CR 609.3 ("do as much as
    // possible"), a multi-zone search (graveyard/hand/library) still searches
    // the non-library zones — only the library component is suppressed. A
    // library-only search becomes a no-op: emit the resolution event, skip the
    // turn-tracking flag, and return.
    let library_muzzled = is_search_muzzled(state, ability.controller);
    let effective_zones: Vec<Zone> = source_zones
        .iter()
        .copied()
        .filter(|zone| !(library_muzzled && *zone == Zone::Library))
        .collect();
    if effective_zones.is_empty() {
        events.push(GameEvent::EffectResolved {
            kind: EffectKind::SearchLibrary,
            source_id: ability.source_id,
            subject: None,
        });
        return Ok(());
    }
    let searched_library = effective_zones.contains(&Zone::Library);

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

    // CR 701.23a: Library search drives the `CantSearchLibrary` muzzle and the
    // per-turn "searched a library" tracking (Aven Mindcensor, Opposition
    // Agent, Archive Trap). Only emit the event / set the flag when the library
    // is among the zones actually searched — a graveyard/hand-only search (or a
    // muzzled library component) does not count as searching a library.
    if searched_library {
        events.push(GameEvent::PlayerPerformedAction {
            player_id: searcher_id,
            action: PlayerActionKind::SearchedLibrary,
        });
        state
            .players_who_searched_library_this_turn
            .insert(searcher_id);
    }

    // CR 701.23a: The candidate set is the union of all searched zones. Every
    // zone in a multi-zone tutor shares one possessive ("search [player]'s
    // graveyard, hand, and/or library"), so all searched zones belong to the
    // library owner — the targeted player for opponent searches, the caster
    // otherwise. `searcher_id` governs WHO chooses and the shuffle/tracking, not
    // whose zones are searched. Resolve the owner once.
    // CR 701.23f + CR 614.1a: A top-N search restriction (Aven Mindcensor) caps
    // the LIBRARY portion of the candidate set to the top `n` cards
    // (`library[0]` is the top — see zones.rs). It applies per `searcher`, not
    // per library owner, and only to the library zone — a multi-zone tutor's
    // graveyard/hand portions are unrestricted (CR 609.3 "do as much as
    // possible"). The whole-library shuffle (a separate chain step) is
    // untouched, and the per-turn search tracking above already fired.
    // G3: AI needs no change here — the candidate set is truncated BEFORE
    // `WaitingFor::SearchChoice` is built below, so the AI enumerator inherits
    // the restriction from `cards`.
    let library_top_limit = library_search_top_limit(state, searcher_id);
    let Some(owner) = state.players.iter().find(|p| p.id == library_owner_id) else {
        return Err(EffectError::PlayerNotFound);
    };
    let mut candidate_ids: Vec<crate::types::identifiers::ObjectId> = Vec::new();
    for zone in &effective_zones {
        match zone {
            Zone::Library => match library_top_limit {
                Some(n) => candidate_ids.extend(owner.library.iter().take(n as usize).copied()),
                None => candidate_ids.extend(owner.library.iter().copied()),
            },
            Zone::Graveyard => candidate_ids.extend(owner.graveyard.iter().copied()),
            Zone::Hand => candidate_ids.extend(owner.hand.iter().copied()),
            // CR 701.23a: The parser only ever produces Graveyard/Hand/Library
            // for `source_zones`; other zones are not searchable tutoring zones.
            _ => continue,
        }
    }

    // CR 107.3a + CR 601.2b: Evaluate the filter with the resolving ability in
    // scope so dynamic thresholds (e.g. `CmcLE { value: Variable("X") }` for
    // Nature's Rhythm) resolve against the caster's announced X.
    // CR 109.5 + CR 400.3: library/graveyard/hand are owner-scoped zones — a
    // card's control-change LKI must not exclude its owner from a "your X"
    // filter, so match with ownership standing in for control.
    let filter_ctx = FilterContext::from_ability(ability);
    let matching: Vec<crate::types::identifiers::ObjectId> = candidate_ids
        .into_iter()
        .filter(|&obj_id| matches_target_filter_in_owner_zone(state, obj_id, &filter, &filter_ctx))
        .collect();

    if matching.is_empty() {
        // CR 701.23b: A player searching a hidden zone isn't required to find
        // cards even if they're present ("fail to find"). Resolve immediately.
        events.push(GameEvent::EffectResolved {
            kind: EffectKind::SearchLibrary,
            source_id: ability.source_id,
            subject: None,
        });
        return Ok(());
    }

    let pick_count = count.min(matching.len());
    let allows_partial_find = allows_hidden_zone_partial_find(&filter, &effective_zones);

    // CR 608.2c: Propagate the printed-text selection restriction (e.g.,
    // "with different names") into the choice state so the Select handler
    // and AI candidate enumerator both see it.
    state.waiting_for = WaitingFor::SearchChoice {
        player: searcher_id,
        // CR 701.23a: bind search-session provenance after the library muzzle
        // has produced the effective zone set. A hand/graveyard card selected
        // from this mixed search still belongs to a search of this library.
        library_owner: searched_library.then_some(library_owner_id),
        cards: matching,
        count: pick_count,
        reveal,
        up_to,
        allows_partial_find,
        constraint: selection_constraint,
        // CR 701.23a + CR 608.2c: Carry the cultivate-class split metadata so the
        // SearchChoice-completion handler can partition the found set.
        split,
    };

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::SearchLibrary,
        source_id: ability.source_id,
        subject: None,
    });

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::ability::{
        Comparator, FilterProp, QuantityExpr, QuantityRef, TypeFilter, TypedFilter,
    };
    use crate::types::card_type::CoreType;
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::mana::ManaCost;
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
                split: None,
                source_zones: vec![crate::types::zones::Zone::Library],
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
                split: None,
                source_zones: vec![crate::types::zones::Zone::Library],
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

    fn add_library_land_with_subtype(
        state: &mut GameState,
        card_id: u64,
        owner: PlayerId,
        name: &str,
        subtype: &str,
    ) -> ObjectId {
        let id = add_library_land(state, card_id, owner, name, false);
        state
            .objects
            .get_mut(&id)
            .unwrap()
            .card_types
            .subtypes
            .push(subtype.to_string());
        id
    }

    fn make_multi_zone_named_search(name: &str, zones: Vec<Zone>) -> ResolvedAbility {
        ResolvedAbility::new(
            Effect::SearchLibrary {
                filter: TargetFilter::Typed(TypedFilter::default().properties(vec![
                    FilterProp::Named {
                        name: name.to_string(),
                    },
                ])),
                count: QuantityExpr::Fixed { value: 1 },
                reveal: false,
                target_player: None,
                selection_constraint: SearchSelectionConstraint::None,
                split: None,
                source_zones: zones,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        )
    }

    #[test]
    fn multi_zone_search_offers_cards_from_every_searched_zone() {
        // CR 701.23a: A God-Pharaoh's-Gift-class tutor searches graveyard + hand
        // + library; a matching card in any of those zones is a legal choice.
        let mut state = GameState::new_two_player(42);
        let gy = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Target".to_string(),
            Zone::Graveyard,
        );
        let hand = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Target".to_string(),
            Zone::Hand,
        );
        let lib = create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "Target".to_string(),
            Zone::Library,
        );
        let _other = create_object(
            &mut state,
            CardId(4),
            PlayerId(0),
            "Other".to_string(),
            Zone::Graveyard,
        );

        let ability = make_multi_zone_named_search(
            "Target",
            vec![Zone::Graveyard, Zone::Hand, Zone::Library],
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        // CR 701.23a: Library is among the searched zones, so SearchedLibrary fires.
        assert!(events.iter().any(|event| matches!(
            event,
            GameEvent::PlayerPerformedAction {
                action: PlayerActionKind::SearchedLibrary,
                ..
            }
        )));
        assert!(state
            .players_who_searched_library_this_turn
            .contains(&PlayerId(0)));

        match &state.waiting_for {
            WaitingFor::SearchChoice {
                cards,
                library_owner,
                ..
            } => {
                assert_eq!(*library_owner, Some(PlayerId(0)));
                assert!(cards.contains(&gy), "graveyard match should be offered");
                assert!(cards.contains(&hand), "hand match should be offered");
                assert!(cards.contains(&lib), "library match should be offered");
                assert_eq!(cards.len(), 3, "only the three 'Target' cards");
            }
            other => panic!("Expected SearchChoice, got {:?}", other),
        }
    }

    #[test]
    fn search_without_library_zone_does_not_mark_searched_library() {
        // CR 701.23a: A search restricted to non-library zones (or whose library
        // component is muzzled) must not set the "searched a library" flag.
        let mut state = GameState::new_two_player(42);
        let gy = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Target".to_string(),
            Zone::Graveyard,
        );

        let ability = make_multi_zone_named_search("Target", vec![Zone::Graveyard, Zone::Hand]);
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(!events.iter().any(|event| matches!(
            event,
            GameEvent::PlayerPerformedAction {
                action: PlayerActionKind::SearchedLibrary,
                ..
            }
        )));
        assert!(!state
            .players_who_searched_library_this_turn
            .contains(&PlayerId(0)));
        match &state.waiting_for {
            WaitingFor::SearchChoice {
                cards,
                library_owner,
                ..
            } => {
                assert_eq!(*library_owner, None);
                assert!(cards.contains(&gy));
            }
            other => panic!("Expected SearchChoice, got {:?}", other),
        }
    }

    #[test]
    fn muzzled_mixed_zone_search_drops_library_provenance() {
        let mut state = GameState::new_two_player(42);
        add_cant_search_library_permanent(&mut state, PlayerId(1), ProhibitionScope::Opponents);
        let gy = create_object(
            &mut state,
            CardId(10),
            PlayerId(0),
            "Target".to_string(),
            Zone::Graveyard,
        );
        let ability = make_multi_zone_named_search(
            "Target",
            vec![Zone::Graveyard, Zone::Hand, Zone::Library],
        );

        resolve(&mut state, &ability, &mut Vec::new()).unwrap();

        match &state.waiting_for {
            WaitingFor::SearchChoice {
                cards,
                library_owner,
                ..
            } => {
                assert_eq!(*library_owner, None);
                assert_eq!(cards, &vec![gy]);
            }
            other => panic!("Expected SearchChoice, got {other:?}"),
        }
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
    fn stated_quality_library_search_can_fail_to_find_with_matches_present() {
        use crate::game::engine::apply;
        use crate::types::actions::GameAction;

        let mut state = GameState::new_two_player(42);
        let forest = add_library_land_with_subtype(&mut state, 1, PlayerId(0), "Forest", "Forest");
        let island = add_library_land_with_subtype(&mut state, 2, PlayerId(0), "Island", "Island");
        let swamp = add_library_land_with_subtype(&mut state, 3, PlayerId(0), "Swamp", "Swamp");
        let filter = TargetFilter::Or {
            filters: vec![
                TargetFilter::Typed(TypedFilter::land().subtype("Forest".to_string())),
                TargetFilter::Typed(TypedFilter::land().subtype("Island".to_string())),
            ],
        };
        let ability = make_search_ability(filter, 1);

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        match &state.waiting_for {
            WaitingFor::SearchChoice {
                cards,
                count,
                up_to,
                allows_partial_find,
                constraint,
                ..
            } => {
                assert_eq!(*count, 1);
                assert!(!*up_to, "printed text is not an up-to search");
                assert!(
                    *allows_partial_find,
                    "hidden-zone stated-quality search permits fail-to-find"
                );
                assert!(cards.contains(&forest));
                assert!(cards.contains(&island));
                assert!(!cards.contains(&swamp));
                assert!(matches!(constraint, SearchSelectionConstraint::None));
            }
            other => panic!("Expected SearchChoice, got {:?}", other),
        }

        let result = apply(
            &mut state,
            PlayerId(0),
            GameAction::SelectCards { cards: vec![] },
        )
        .unwrap();

        assert!(matches!(result.waiting_for, WaitingFor::Priority { .. }));
        assert!(state.players[0].library.contains(&forest));
        assert!(state.players[0].library.contains(&island));
        assert!(state.players[0].library.contains(&swamp));
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
            WaitingFor::SearchChoice {
                cards, constraint, ..
            } => {
                assert_eq!(cards.len(), 2);
                assert!(cards.contains(&card1));
                assert!(cards.contains(&card2));
                assert!(matches!(constraint, SearchSelectionConstraint::None));
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
                enters_under: None,
                enter_tapped: crate::types::zones::EtbTapState::Tapped,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
                conditional_enter_with_counters: vec![],
                face_down_profile: None,
                enters_modified_if: None,
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
                split: None,
                source_zones: vec![crate::types::zones::Zone::Library],
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
    fn multizone_tutor_moves_graveyard_pick_to_battlefield() {
        // CR 701.23a: A God-Pharaoh's-Gift-class tutor finds the card in the
        // graveyard/hand/library; the put-step must move it from wherever it
        // was chosen. Regression guard for the origin=Library put-step bug.
        use crate::game::effects::resolve_ability_chain;
        use crate::game::engine::apply;
        use crate::types::ability::{Effect, FilterProp};
        use crate::types::actions::GameAction;

        let mut state = GameState::new_two_player(42);
        let gy = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Target".to_string(),
            Zone::Graveyard,
        );

        let shuffle_step = ResolvedAbility::new(
            Effect::Shuffle {
                target: TargetFilter::Controller,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        // Multi-zone put-step carries origin: None (move the card from its
        // actual zone), as the parser now emits for multi-zone tutors.
        let put_step = ResolvedAbility::new(
            Effect::ChangeZone {
                origin: None,
                destination: Zone::Battlefield,
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
            vec![],
            ObjectId(100),
            PlayerId(0),
        )
        .sub_ability(shuffle_step);
        let search_step = ResolvedAbility::new(
            Effect::SearchLibrary {
                filter: TargetFilter::Typed(TypedFilter::default().properties(vec![
                    FilterProp::Named {
                        name: "Target".to_string(),
                    },
                ])),
                count: QuantityExpr::Fixed { value: 1 },
                reveal: true,
                target_player: None,
                selection_constraint: SearchSelectionConstraint::None,
                split: None,
                source_zones: vec![Zone::Graveyard, Zone::Hand, Zone::Library],
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
            GameAction::SelectCards { cards: vec![gy] },
        )
        .unwrap();

        assert_eq!(
            state.objects[&gy].zone,
            Zone::Battlefield,
            "graveyard-chosen card must reach the battlefield"
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
                enters_under: None,
                enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
                conditional_enter_with_counters: vec![],
                face_down_profile: None,
                enters_modified_if: None,
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
                split: None,
                source_zones: vec![crate::types::zones::Zone::Library],
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
    fn search_fail_to_find_no_ops_top_of_library_put() {
        // CR 701.23b: A top-of-library tutor (Search -> Shuffle ->
        // PutAtLibraryPosition, e.g. Mystical/Vampiric/Worldly Tutor) that finds
        // nothing must shuffle and place no card — not error. The search returns
        // early without a SearchChoice, so the `PutAtLibraryPosition { target: Any }`
        // resolves with no forwarded card and must no-op rather than raise
        // "requires a target".
        use crate::game::effects::resolve_ability_chain;
        use crate::types::ability::{Effect, LibraryPosition, TypeFilter};

        let mut state = GameState::new_two_player(42);
        // Library holds only a Forest; the search wants an Island -> fail to find.
        let forest = add_library_land_with_subtype(&mut state, 1, PlayerId(0), "Forest", "Forest");

        let put_step = ResolvedAbility::new(
            Effect::PutAtLibraryPosition {
                target: TargetFilter::Any,
                count: QuantityExpr::Fixed { value: 1 },
                position: LibraryPosition::Top,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        let shuffle_step = ResolvedAbility::new(
            Effect::Shuffle {
                target: TargetFilter::Controller,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        )
        .sub_ability(put_step);
        let search_step = ResolvedAbility::new(
            Effect::SearchLibrary {
                filter: TargetFilter::Typed(
                    TypedFilter::land().with_type(TypeFilter::Subtype("Island".to_string())),
                ),
                count: QuantityExpr::Fixed { value: 1 },
                reveal: true,
                target_player: None,
                selection_constraint: SearchSelectionConstraint::None,
                split: None,
                source_zones: vec![Zone::Library],
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        )
        .sub_ability(shuffle_step);

        let mut events = Vec::new();
        let result = resolve_ability_chain(&mut state, &search_step, &mut events, 0);
        assert!(
            result.is_ok(),
            "fail-to-find tutor must no-op PutAtLibraryPosition, got {result:?}"
        );
        // No card was found, so no choice is pending and the Forest stays put.
        assert!(!matches!(
            state.waiting_for,
            WaitingFor::SearchChoice { .. }
        ));
        assert_eq!(state.objects[&forest].zone, Zone::Library);
    }

    #[test]
    fn beseech_unbargained_search_exiles_then_moves_uncast_card_to_hand() {
        use crate::game::ability_utils::build_resolved_from_def;
        use crate::game::effects::resolve_ability_chain;
        use crate::game::engine::apply;
        use crate::parser::oracle_effect::parse_effect_chain;
        use crate::types::ability::AbilityKind;
        use crate::types::actions::GameAction;

        let mut state = GameState::new_two_player(42);
        let found = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Found Spell".to_string(),
            Zone::Library,
        );
        state
            .objects
            .get_mut(&found)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Sorcery);
        let ability = parse_effect_chain(
            "search your library for a card, exile it face down, then shuffle. if this spell was bargained, you may cast the exiled card without paying its mana cost if that spell's mana value is 4 or less. put the exiled card into your hand if it wasn't cast this way",
            AbilityKind::Spell,
        );
        let resolved = build_resolved_from_def(&ability, ObjectId(100), PlayerId(0));

        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &resolved, &mut events, 0).unwrap();
        assert!(matches!(state.waiting_for, WaitingFor::SearchChoice { .. }));

        apply(
            &mut state,
            PlayerId(0),
            GameAction::SelectCards { cards: vec![found] },
        )
        .unwrap();

        assert!(
            !matches!(state.waiting_for, WaitingFor::OptionalEffectChoice { .. }),
            "unbargained Beseech must not offer the cast choice"
        );
        assert_eq!(state.objects[&found].zone, Zone::Hand);
        assert!(state.players[0].hand.contains(&found));
    }

    #[test]
    fn beseech_bargained_accept_grants_cast_without_hand_fallback() {
        use crate::game::ability_utils::build_resolved_from_def;
        use crate::game::effects::resolve_ability_chain;
        use crate::game::engine::apply;
        use crate::parser::oracle_effect::parse_effect_chain;
        use crate::types::ability::{
            AbilityKind, CastPermissionConstraint, CastingPermission, Comparator, QuantityExpr,
        };
        use crate::types::actions::GameAction;

        let mut state = GameState::new_two_player(42);
        let found = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Found Spell".to_string(),
            Zone::Library,
        );
        {
            let obj = state.objects.get_mut(&found).unwrap();
            obj.card_types.core_types.push(CoreType::Sorcery);
            obj.mana_cost = ManaCost::generic(4);
        }
        let ability = parse_effect_chain(
            "search your library for a card, exile it face down, then shuffle. if this spell was bargained, you may cast the exiled card without paying its mana cost if that spell's mana value is 4 or less. put the exiled card into your hand if it wasn't cast this way",
            AbilityKind::Spell,
        );
        let mut resolved = build_resolved_from_def(&ability, ObjectId(100), PlayerId(0));
        resolved.context.additional_cost_paid = true;

        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &resolved, &mut events, 0).unwrap();
        assert!(matches!(state.waiting_for, WaitingFor::SearchChoice { .. }));

        apply(
            &mut state,
            PlayerId(0),
            GameAction::SelectCards { cards: vec![found] },
        )
        .unwrap();
        assert!(matches!(
            state.waiting_for,
            WaitingFor::OptionalEffectChoice { .. }
        ));

        apply(
            &mut state,
            PlayerId(0),
            GameAction::DecideOptionalEffect { accept: true },
        )
        .unwrap();

        let obj = state.objects.get(&found).unwrap();
        assert_eq!(obj.zone, Zone::Exile);
        assert!(!state.players[0].hand.contains(&found));
        assert!(obj.casting_permissions.iter().any(|permission| matches!(
            permission,
            CastingPermission::ExileWithAltCost {
                cost,
                constraint:
                    Some(CastPermissionConstraint::ManaValue {
                        comparator: Comparator::LE,
                        value: QuantityExpr::Fixed { value: 4 },
                    }),
                ..
            } if *cost == ManaCost::zero()
        )));
    }

    #[test]
    fn sequential_searches_forward_each_selection_and_run_shuffle_tail() {
        use crate::game::effects::resolve_ability_chain;
        use crate::game::engine::apply;
        use crate::types::ability::{Effect, TypeFilter};
        use crate::types::actions::GameAction;

        let mut state = GameState::new_two_player(42);
        let forest = add_library_land_with_subtype(&mut state, 1, PlayerId(0), "Forest", "Forest");
        let plains = add_library_land_with_subtype(&mut state, 2, PlayerId(0), "Plains", "Plains");

        let shuffle_step = ResolvedAbility::new(
            Effect::Shuffle {
                target: TargetFilter::Controller,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        let put_plains_step = ResolvedAbility::new(
            Effect::ChangeZone {
                origin: Some(Zone::Library),
                destination: Zone::Battlefield,
                target: TargetFilter::Any,
                owner_library: false,
                enter_transformed: false,
                enters_under: None,
                enter_tapped: crate::types::zones::EtbTapState::Tapped,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
                conditional_enter_with_counters: vec![],
                face_down_profile: None,
                enters_modified_if: None,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        )
        .sub_ability(shuffle_step);
        let search_plains_step = ResolvedAbility::new(
            Effect::SearchLibrary {
                filter: TargetFilter::Typed(
                    TypedFilter::land().with_type(TypeFilter::Subtype("Plains".to_string())),
                ),
                count: QuantityExpr::Fixed { value: 1 },
                reveal: false,
                target_player: None,
                selection_constraint: SearchSelectionConstraint::None,
                split: None,
                source_zones: vec![crate::types::zones::Zone::Library],
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        )
        .sub_ability(put_plains_step);
        let put_forest_step = ResolvedAbility::new(
            Effect::ChangeZone {
                origin: Some(Zone::Library),
                destination: Zone::Battlefield,
                target: TargetFilter::Any,
                owner_library: false,
                enter_transformed: false,
                enters_under: None,
                enter_tapped: crate::types::zones::EtbTapState::Tapped,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
                conditional_enter_with_counters: vec![],
                face_down_profile: None,
                enters_modified_if: None,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        )
        .sub_ability(search_plains_step);
        let search_forest_step = ResolvedAbility::new(
            Effect::SearchLibrary {
                filter: TargetFilter::Typed(
                    TypedFilter::land().with_type(TypeFilter::Subtype("Forest".to_string())),
                ),
                count: QuantityExpr::Fixed { value: 1 },
                reveal: false,
                target_player: None,
                selection_constraint: SearchSelectionConstraint::None,
                split: None,
                source_zones: vec![crate::types::zones::Zone::Library],
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        )
        .sub_ability(put_forest_step);

        let mut all_events = Vec::new();
        resolve_ability_chain(&mut state, &search_forest_step, &mut all_events, 0).unwrap();

        match &state.waiting_for {
            WaitingFor::SearchChoice { cards, .. } => {
                assert_eq!(cards, &vec![forest], "first search should offer Forest");
            }
            other => panic!("expected first SearchChoice, got {:?}", other),
        }

        let result = apply(
            &mut state,
            PlayerId(0),
            GameAction::SelectCards {
                cards: vec![forest],
            },
        )
        .unwrap();
        all_events.extend(result.events);

        assert_eq!(state.objects[&forest].zone, Zone::Battlefield);
        assert!(state.objects[&forest].tapped);
        assert_eq!(
            state.objects[&plains].zone,
            Zone::Library,
            "Plains should wait for the second search selection"
        );

        match &state.waiting_for {
            WaitingFor::SearchChoice { cards, .. } => {
                assert_eq!(cards, &vec![plains], "second search should offer Plains");
            }
            other => panic!("expected second SearchChoice, got {:?}", other),
        }

        let result = apply(
            &mut state,
            PlayerId(0),
            GameAction::SelectCards {
                cards: vec![plains],
            },
        )
        .unwrap();
        all_events.extend(result.events);

        assert_eq!(state.objects[&plains].zone, Zone::Battlefield);
        assert!(state.objects[&plains].tapped);
        assert!(state.pending_continuation.is_none());
        assert!(state.pending_repeat_iteration.is_none());
        assert!(all_events.iter().any(|event| matches!(
            event,
            GameEvent::EffectResolved {
                kind: EffectKind::Shuffle,
                ..
            }
        )));
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
                split: None,
                source_zones: vec![crate::types::zones::Zone::Library],
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

    #[test]
    fn total_mana_value_selection_constraint_checks_chosen_set_sum() {
        let mut state = GameState::new_two_player(42);
        let cmc2 = add_library_creature_with_cmc(&mut state, 1, PlayerId(0), "Small", 2);
        let cmc4 = add_library_creature_with_cmc(&mut state, 2, PlayerId(0), "Mid", 4);
        let cmc5 = add_library_creature_with_cmc(&mut state, 3, PlayerId(0), "Large", 5);
        let constraint = SearchSelectionConstraint::TotalManaValue {
            comparator: Comparator::LE,
            value: 6,
        };

        assert!(selection_satisfies_constraint(
            &state,
            &[cmc2, cmc4],
            &constraint
        ));
        assert!(!selection_satisfies_constraint(
            &state,
            &[cmc2, cmc5],
            &constraint
        ));
    }

    #[test]
    fn distinct_quality_selection_constraint_checks_power() {
        let mut state = GameState::new_two_player(42);
        let bear = add_library_creature(&mut state, 1, PlayerId(0), "Bear");
        let hound = add_library_creature(&mut state, 2, PlayerId(0), "Hound");
        let drake = add_library_creature(&mut state, 3, PlayerId(0), "Drake");
        state.objects.get_mut(&bear).unwrap().power = Some(2);
        state.objects.get_mut(&hound).unwrap().power = Some(2);
        state.objects.get_mut(&drake).unwrap().power = Some(3);
        let constraint = SearchSelectionConstraint::DistinctQualities {
            qualities: vec![SharedQuality::Power],
        };

        assert!(selection_satisfies_constraint(
            &state,
            &[bear, drake],
            &constraint
        ));
        assert!(!selection_satisfies_constraint(
            &state,
            &[bear, hound],
            &constraint
        ));
    }

    #[test]
    fn distinct_quality_selection_constraint_checks_multiple_qualities() {
        let mut state = GameState::new_two_player(42);
        let artifact_creature = add_library_creature(&mut state, 1, PlayerId(0), "Construct");
        let sorcery = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Spell".to_string(),
            Zone::Library,
        );
        {
            let obj = state.objects.get_mut(&artifact_creature).unwrap();
            obj.power = Some(2);
            obj.toughness = Some(2);
            obj.mana_cost = crate::types::mana::ManaCost::generic(2);
            obj.card_types.core_types.push(CoreType::Artifact);
        }
        {
            let obj = state.objects.get_mut(&sorcery).unwrap();
            obj.power = Some(3);
            obj.toughness = Some(3);
            obj.mana_cost = crate::types::mana::ManaCost::generic(3);
            obj.card_types.core_types = vec![CoreType::Sorcery];
        }
        let constraint = SearchSelectionConstraint::DistinctQualities {
            qualities: vec![
                SharedQuality::ManaValue,
                SharedQuality::Power,
                SharedQuality::Toughness,
                SharedQuality::CardType,
            ],
        };

        assert!(selection_satisfies_constraint(
            &state,
            &[artifact_creature, sorcery],
            &constraint
        ));

        state.objects.get_mut(&sorcery).unwrap().power = Some(2);
        assert!(!selection_satisfies_constraint(
            &state,
            &[artifact_creature, sorcery],
            &constraint
        ));
    }

    #[test]
    fn distinct_quality_selection_constraint_allows_missing_power_toughness() {
        let mut state = GameState::new_two_player(42);
        let creature = add_library_creature(&mut state, 1, PlayerId(0), "Construct");
        let instant = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Spell".to_string(),
            Zone::Library,
        );
        {
            let obj = state.objects.get_mut(&creature).unwrap();
            obj.power = Some(2);
            obj.toughness = Some(2);
            obj.card_types.core_types.push(CoreType::Artifact);
        }
        state
            .objects
            .get_mut(&instant)
            .unwrap()
            .card_types
            .core_types = vec![CoreType::Instant];
        let constraint = SearchSelectionConstraint::DistinctQualities {
            qualities: vec![
                SharedQuality::Power,
                SharedQuality::Toughness,
                SharedQuality::CardType,
            ],
        };

        assert!(selection_satisfies_constraint(
            &state,
            &[creature, instant],
            &constraint
        ));
    }

    #[test]
    fn match_each_filter_selection_constraint_requires_distinct_assignments() {
        let mut state = GameState::new_two_player(42);
        let black_green = add_library_creature(&mut state, 1, PlayerId(0), "Golgari");
        let blue = add_library_creature(&mut state, 2, PlayerId(0), "Drake");
        let colorless = add_library_creature(&mut state, 3, PlayerId(0), "Construct");
        {
            let obj = state.objects.get_mut(&black_green).unwrap();
            obj.color = vec![
                crate::types::mana::ManaColor::Black,
                crate::types::mana::ManaColor::Green,
            ];
        }
        state.objects.get_mut(&blue).unwrap().color = vec![crate::types::mana::ManaColor::Blue];

        let color_filter = |color| {
            TargetFilter::Typed(TypedFilter {
                type_filters: vec![],
                controller: None,
                properties: vec![FilterProp::HasColor { color }],
            })
        };
        let constraint = SearchSelectionConstraint::MatchEachFilter {
            filters: vec![
                color_filter(crate::types::mana::ManaColor::Black),
                color_filter(crate::types::mana::ManaColor::Green),
                color_filter(crate::types::mana::ManaColor::Blue),
            ],
        };

        assert!(!selection_satisfies_constraint(
            &state,
            &[black_green, blue, colorless],
            &constraint
        ));

        let green = add_library_creature(&mut state, 4, PlayerId(0), "Elf");
        state.objects.get_mut(&green).unwrap().color = vec![crate::types::mana::ManaColor::Green];
        assert!(selection_satisfies_constraint(
            &state,
            &[black_green, green, blue],
            &constraint
        ));
    }

    /// CR 701.23b: a stated-quality MatchEachFilter search may find FEWER cards
    /// than there are filter slots — including none — when the library can't
    /// supply one card per slot (Aang's Journey kicked: basic land + Shrine, but
    /// the deck has no Shrine). Exercises the building-block legality authority
    /// across the full short/over/distinct-slot range, including the overlapping-
    /// filter backtracking case a greedy matcher silently breaks.
    #[test]
    fn match_each_filter_permits_partial_and_empty_fail_to_find() {
        let mut state = GameState::new_two_player(42);

        // A basic land and a Shrine enchantment in the library.
        let basic = add_library_land(&mut state, 1, PlayerId(0), "Forest", true);
        let shrine = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Honden".to_string(),
            Zone::Library,
        );
        {
            let obj = state.objects.get_mut(&shrine).unwrap();
            obj.card_types.core_types = vec![CoreType::Enchantment];
            obj.card_types.subtypes.push("Shrine".to_string());
        }

        let basic_filter = TargetFilter::Typed(TypedFilter {
            type_filters: vec![TypeFilter::Land],
            controller: None,
            properties: vec![FilterProp::HasSupertype {
                value: crate::types::card_type::Supertype::Basic,
            }],
        });
        let shrine_filter = TargetFilter::Typed(TypedFilter {
            type_filters: vec![TypeFilter::Subtype("Shrine".to_string())],
            controller: None,
            properties: vec![],
        });
        let constraint = SearchSelectionConstraint::MatchEachFilter {
            filters: vec![basic_filter.clone(), shrine_filter.clone()],
        };

        // Empty selection: always legal (full fail-to-find).
        assert!(selection_satisfies_constraint(&state, &[], &constraint));

        // Partial: a single basic fills the distinct basic slot, Shrine slot
        // left unfilled — legal.
        assert!(selection_satisfies_constraint(
            &state,
            &[basic],
            &constraint
        ));
        // A single Shrine fills the Shrine slot — legal.
        assert!(selection_satisfies_constraint(
            &state,
            &[shrine],
            &constraint
        ));

        // Full selection still legal.
        assert!(selection_satisfies_constraint(
            &state,
            &[basic, shrine],
            &constraint
        ));

        // A card matching NO slot is illegal even as a singleton: a vanilla
        // creature satisfies neither the basic-land nor the Shrine filter.
        let creature = add_library_creature(&mut state, 3, PlayerId(0), "Bear");
        assert!(!selection_satisfies_constraint(
            &state,
            &[creature],
            &constraint
        ));

        // Over-selection: more cards than filter slots is illegal.
        let basic2 = add_library_land(&mut state, 4, PlayerId(0), "Island", true);
        assert!(!selection_satisfies_constraint(
            &state,
            &[basic, shrine, basic2],
            &constraint
        ));

        // Two cards both matching only the same single slot is illegal: two
        // basics against [basic, Shrine] cannot both claim the lone basic slot.
        assert!(!selection_satisfies_constraint(
            &state,
            &[basic, basic2],
            &constraint
        ));
    }

    /// CR 701.23b: the card-anchored matcher MUST backtrack, not greedily bind
    /// the first matching slot. For overlapping filters `[basic-or-Shrine, Shrine]`
    /// and chosen `[shrine, basic]`, a greedy matcher binds shrine→slot0 then
    /// fails basic→slot1 (Shrine-only), falsely rejecting the legal assignment
    /// shrine→slot1, basic→slot0. This case is the regression sentinel for a
    /// greedy rewrite.
    #[test]
    fn match_each_filter_backtracks_on_overlapping_slots() {
        let mut state = GameState::new_two_player(42);
        let basic = add_library_land(&mut state, 1, PlayerId(0), "Forest", true);
        let shrine = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Honden".to_string(),
            Zone::Library,
        );
        {
            let obj = state.objects.get_mut(&shrine).unwrap();
            obj.card_types.core_types = vec![CoreType::Enchantment];
            obj.card_types.subtypes.push("Shrine".to_string());
        }

        // slot0 matches a basic land OR a Shrine; slot1 matches only a Shrine.
        let basic_or_shrine = TargetFilter::Typed(TypedFilter {
            type_filters: vec![TypeFilter::AnyOf(vec![
                TypeFilter::Land,
                TypeFilter::Subtype("Shrine".to_string()),
            ])],
            controller: None,
            properties: vec![],
        });
        let shrine_only = TargetFilter::Typed(TypedFilter {
            type_filters: vec![TypeFilter::Subtype("Shrine".to_string())],
            controller: None,
            properties: vec![],
        });
        let constraint = SearchSelectionConstraint::MatchEachFilter {
            filters: vec![basic_or_shrine, shrine_only],
        };

        // Chosen order [shrine, basic]: only the backtracking assignment
        // (shrine→slot1, basic→slot0) is valid. A greedy first-match returns
        // false here.
        assert!(selection_satisfies_constraint(
            &state,
            &[shrine, basic],
            &constraint
        ));
    }

    /// CR 701.23d: a pure-quantity (`None`) search is unaffected by the partial-
    /// find relaxation — any size remains legal there, as before, because the
    /// count bound is enforced separately at the submission guard.
    #[test]
    fn none_constraint_unaffected_by_partial_find_relaxation() {
        let mut state = GameState::new_two_player(42);
        let a = add_library_creature(&mut state, 1, PlayerId(0), "A");
        let b = add_library_creature(&mut state, 2, PlayerId(0), "B");
        let constraint = SearchSelectionConstraint::None;
        assert!(selection_satisfies_constraint(&state, &[], &constraint));
        assert!(selection_satisfies_constraint(&state, &[a], &constraint));
        assert!(selection_satisfies_constraint(&state, &[a, b], &constraint));
        assert!(!constraint.permits_partial_find());
    }

    /// CR 107.3a + CR 601.2b: Nature's Rhythm — search for a creature card with mana
    /// value X or less. With X=4, only CMC-≤-4 creatures should be selectable,
    /// regardless of what's in the library.
    #[test]
    fn natures_rhythm_x_mana_value_restricts_search_targets() {
        let mut state = GameState::new_two_player(42);
        let cmc2 = add_library_creature_with_cmc(&mut state, 1, PlayerId(0), "Small", 2);
        let cmc4 = add_library_creature_with_cmc(&mut state, 2, PlayerId(0), "Mid", 4);
        add_library_creature_with_cmc(&mut state, 3, PlayerId(0), "Large", 5);
        add_library_creature_with_cmc(&mut state, 4, PlayerId(0), "Behemoth", 8);

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
            Effect::SearchLibrary {
                filter,
                count: QuantityExpr::Fixed { value: 1 },
                reveal: false,
                target_player: None,
                selection_constraint: SearchSelectionConstraint::None,
                split: None,
                source_zones: vec![crate::types::zones::Zone::Library],
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
            TargetFilter::Typed(TypedFilter::creature().properties(vec![FilterProp::Cmc {
                comparator: crate::types::ability::Comparator::LE,
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
                split: None,
                source_zones: vec![crate::types::zones::Zone::Library],
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
                split: None,
                source_zones: vec![crate::types::zones::Zone::Library],
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
                split: None,
                source_zones: vec![crate::types::zones::Zone::Library],
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
                split: None,
                source_zones: vec![crate::types::zones::Zone::Library],
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

    #[test]
    fn opponent_direct_search_muzzled() {
        // CR 701.23: "Your opponents can't search libraries." — cause=Opponents
        // muzzles an opponent's own library search (Mindlock Orb opponent variant).
        let mut state = GameState::new_two_player(42);
        add_cant_search_library_permanent(&mut state, PlayerId(0), ProhibitionScope::Opponents);

        let _bear = add_library_creature(&mut state, 1, PlayerId(1), "Bear");

        let ability = ResolvedAbility::new(
            Effect::SearchLibrary {
                filter: TargetFilter::Typed(TypedFilter::creature()),
                count: QuantityExpr::Fixed { value: 1 },
                reveal: false,
                target_player: None,
                selection_constraint: SearchSelectionConstraint::None,
                split: None,
                source_zones: vec![crate::types::zones::Zone::Library],
            },
            vec![],
            ObjectId(9996),
            PlayerId(1),
        );

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(
            !state
                .players_who_searched_library_this_turn
                .contains(&PlayerId(1)),
            "Opponent direct search must be muzzled"
        );
        assert!(
            !matches!(state.waiting_for, WaitingFor::SearchChoice { .. }),
            "Muzzled search must NOT transition to SearchChoice"
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
                split: None,
                source_zones: vec![crate::types::zones::Zone::Library],
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

    /// CR 608.2h + CR 701.23a: if the parent target has already left the object
    /// map, "its controller may search" must use the target's LKI controller,
    /// not fall back to the caster.
    #[test]
    fn parent_target_controller_search_uses_lki_for_missing_target() {
        let mut state = GameState::new_two_player(42);
        let destroyed = create_object(
            &mut state,
            CardId(100),
            PlayerId(1),
            "Destroyed Token".to_string(),
            Zone::Graveyard,
        );
        let lki = state.objects[&destroyed].snapshot_for_mana_spent();
        state.lki_cache.insert(destroyed, lki);
        state.objects.remove(&destroyed);
        let _opp_basic = add_library_land(&mut state, 1, PlayerId(1), "Forest", true);

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
                split: None,
                source_zones: vec![crate::types::zones::Zone::Library],
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
                "LKI must identify the missing parent target's controller"
            ),
            other => panic!("expected SearchChoice, got {other:?}"),
        }
        assert!(state
            .players_who_searched_library_this_turn
            .contains(&PlayerId(1)));
        assert!(!state
            .players_who_searched_library_this_turn
            .contains(&PlayerId(0)));
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
                split: None,
                source_zones: vec![crate::types::zones::Zone::Library],
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

    // === CR 701.23f + CR 614.1a: RestrictLibrarySearchToTop runtime enforcement ===

    fn add_restrict_search_to_top_permanent(
        state: &mut GameState,
        controller: PlayerId,
        who: ProhibitionScope,
        count: u32,
    ) -> ObjectId {
        let id = create_object(
            state,
            CardId(0xAE7),
            controller,
            "Aven Mindcensor".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.entered_battlefield_turn = Some(0);
        obj.static_definitions.push(StaticDefinition::new(
            StaticMode::RestrictLibrarySearchToTop { who, count },
        ));
        id
    }

    /// Stack `count` creatures into `owner`'s library and return their ids in
    /// top→bottom order (`library[0]` is the top — see zones.rs).
    fn stack_library_creatures(
        state: &mut GameState,
        owner: PlayerId,
        count: u64,
        base_id: u64,
    ) -> Vec<ObjectId> {
        (0..count)
            .map(|i| add_library_creature(state, base_id + i, owner, &format!("Stacked {i}")))
            .collect()
    }

    #[test]
    fn aven_caps_opponent_search_to_top_four() {
        // CR 701.23f + CR 614.1a: P0 controls Aven (who=Opponents). P1 (opponent)
        // searches their own ≥6-card library — only the top 4 cards are offered;
        // the bottom cards are excluded. Revert-fail: without truncation all 6
        // would be offered.
        let mut state = GameState::new_two_player(42);
        add_restrict_search_to_top_permanent(
            &mut state,
            PlayerId(0),
            ProhibitionScope::Opponents,
            4,
        );
        let lib = stack_library_creatures(&mut state, PlayerId(1), 6, 1);

        let ability = ResolvedAbility::new(
            Effect::SearchLibrary {
                filter: TargetFilter::Typed(TypedFilter::creature()),
                count: QuantityExpr::Fixed { value: 1 },
                reveal: false,
                target_player: None,
                selection_constraint: SearchSelectionConstraint::None,
                split: None,
                source_zones: vec![crate::types::zones::Zone::Library],
            },
            vec![],
            ObjectId(9990),
            PlayerId(1),
        );

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        match &state.waiting_for {
            WaitingFor::SearchChoice { cards, .. } => {
                assert_eq!(cards.len(), 4, "only the top 4 library cards are offered");
                for top in &lib[0..4] {
                    assert!(cards.contains(top), "top-4 card must be offered");
                }
                for bottom in &lib[4..6] {
                    assert!(
                        !cards.contains(bottom),
                        "bottom cards beyond the top 4 must be excluded"
                    );
                }
            }
            other => panic!("expected SearchChoice, got {other:?}"),
        }
    }

    #[test]
    fn aven_does_not_cap_controllers_own_search() {
        // CR 701.23f: Aven's `who = Opponents`. The controller's OWN search is not
        // restricted — all cards are offered.
        let mut state = GameState::new_two_player(42);
        add_restrict_search_to_top_permanent(
            &mut state,
            PlayerId(0),
            ProhibitionScope::Opponents,
            4,
        );
        let _lib = stack_library_creatures(&mut state, PlayerId(0), 6, 1);

        let ability = make_search_ability(TargetFilter::Typed(TypedFilter::creature()), 1);
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        match &state.waiting_for {
            WaitingFor::SearchChoice { cards, .. } => assert_eq!(
                cards.len(),
                6,
                "the controller's own search sees the whole library"
            ),
            other => panic!("expected SearchChoice, got {other:?}"),
        }
    }

    #[test]
    fn aven_cap_below_library_size_offers_all_cards() {
        // CR 701.23f: a library with fewer than N cards is searched in full —
        // take(n) clamps to the available count.
        let mut state = GameState::new_two_player(42);
        add_restrict_search_to_top_permanent(
            &mut state,
            PlayerId(0),
            ProhibitionScope::Opponents,
            4,
        );
        let lib = stack_library_creatures(&mut state, PlayerId(1), 2, 1);

        let ability = ResolvedAbility::new(
            Effect::SearchLibrary {
                filter: TargetFilter::Typed(TypedFilter::creature()),
                count: QuantityExpr::Fixed { value: 1 },
                reveal: false,
                target_player: None,
                selection_constraint: SearchSelectionConstraint::None,
                split: None,
                source_zones: vec![crate::types::zones::Zone::Library],
            },
            vec![],
            ObjectId(9991),
            PlayerId(1),
        );

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        match &state.waiting_for {
            WaitingFor::SearchChoice { cards, .. } => {
                assert_eq!(cards.len(), 2, "all available cards offered when < N");
                assert!(cards.contains(&lib[0]) && cards.contains(&lib[1]));
            }
            other => panic!("expected SearchChoice, got {other:?}"),
        }
    }

    #[test]
    fn aven_caps_by_searcher_not_library_owner() {
        // CR 701.23f: P0 controls Aven. P1 (opponent of P0) searches P2's library
        // (searcher ≠ owner). The cap keys off the SEARCHER (P1, an opponent of
        // Aven's controller), so the top-4 limit still applies to P2's library.
        let mut state = GameState::new(crate::types::format::FormatConfig::standard(), 3, 42);
        add_restrict_search_to_top_permanent(
            &mut state,
            PlayerId(0),
            ProhibitionScope::Opponents,
            4,
        );
        let lib = stack_library_creatures(&mut state, PlayerId(2), 6, 1);

        // P1 casts "search target opponent's library" targeting P2; searcher = P1.
        let ability = ResolvedAbility::new(
            Effect::SearchLibrary {
                filter: TargetFilter::Typed(TypedFilter::creature()),
                count: QuantityExpr::Fixed { value: 1 },
                reveal: false,
                target_player: Some(TargetFilter::Typed(
                    crate::types::ability::TypedFilter::default()
                        .controller(crate::types::ability::ControllerRef::Opponent),
                )),
                selection_constraint: SearchSelectionConstraint::None,
                split: None,
                source_zones: vec![crate::types::zones::Zone::Library],
            },
            vec![TargetRef::Player(PlayerId(2))],
            ObjectId(9992),
            PlayerId(1),
        );

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        match &state.waiting_for {
            WaitingFor::SearchChoice { player, cards, .. } => {
                assert_eq!(
                    *player,
                    PlayerId(1),
                    "searcher (caster P1) browses P2's library"
                );
                assert_eq!(
                    cards.len(),
                    4,
                    "cap keyed by searcher applies to P2's library"
                );
                for bottom in &lib[4..6] {
                    assert!(!cards.contains(bottom));
                }
            }
            other => panic!("expected SearchChoice, got {other:?}"),
        }
    }

    #[test]
    fn aven_caps_only_library_portion_of_multi_zone_search() {
        // CR 701.23f + CR 609.3: an opponent's multi-zone search (graveyard +
        // library) has ONLY the library portion capped; graveyard cards are all
        // offered.
        let mut state = GameState::new_two_player(42);
        add_restrict_search_to_top_permanent(
            &mut state,
            PlayerId(0),
            ProhibitionScope::Opponents,
            4,
        );
        // P1 library: 6 "Target" creatures; only the top 4 should be offered.
        let lib: Vec<ObjectId> = (0..6)
            .map(|i| {
                create_object(
                    &mut state,
                    CardId(1 + i),
                    PlayerId(1),
                    "Target".to_string(),
                    Zone::Library,
                )
            })
            .collect();
        // P1 graveyard: 3 "Target" cards; all should be offered (uncapped).
        let gy: Vec<ObjectId> = (0..3)
            .map(|i| {
                create_object(
                    &mut state,
                    CardId(100 + i),
                    PlayerId(1),
                    "Target".to_string(),
                    Zone::Graveyard,
                )
            })
            .collect();

        let ability = ResolvedAbility::new(
            Effect::SearchLibrary {
                filter: TargetFilter::Typed(TypedFilter::default().properties(vec![
                    FilterProp::Named {
                        name: "Target".to_string(),
                    },
                ])),
                count: QuantityExpr::Fixed { value: 1 },
                reveal: false,
                target_player: None,
                selection_constraint: SearchSelectionConstraint::None,
                split: None,
                source_zones: vec![Zone::Graveyard, Zone::Library],
            },
            vec![],
            ObjectId(9993),
            PlayerId(1),
        );

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        match &state.waiting_for {
            WaitingFor::SearchChoice { cards, .. } => {
                // 4 (top of library) + 3 (whole graveyard) = 7.
                assert_eq!(
                    cards.len(),
                    7,
                    "library capped to top 4; graveyard uncapped"
                );
                for top in &lib[0..4] {
                    assert!(cards.contains(top));
                }
                for bottom in &lib[4..6] {
                    assert!(!cards.contains(bottom), "library bottom cards excluded");
                }
                for g in &gy {
                    assert!(cards.contains(g), "every graveyard card is offered");
                }
            }
            other => panic!("expected SearchChoice, got {other:?}"),
        }
    }

    #[test]
    fn aven_capped_search_still_tracks_and_preserves_whole_library() {
        // CR 701.23f: a restricted search still records the searcher in the
        // per-turn tracking AND the whole library is preserved (the separate
        // shuffle chain step would still see every card; truncation only filters
        // the candidate set, not the zone contents).
        let mut state = GameState::new_two_player(42);
        add_restrict_search_to_top_permanent(
            &mut state,
            PlayerId(0),
            ProhibitionScope::Opponents,
            4,
        );
        let lib = stack_library_creatures(&mut state, PlayerId(1), 6, 1);

        let ability = ResolvedAbility::new(
            Effect::SearchLibrary {
                filter: TargetFilter::Typed(TypedFilter::creature()),
                count: QuantityExpr::Fixed { value: 1 },
                reveal: false,
                target_player: None,
                selection_constraint: SearchSelectionConstraint::None,
                split: None,
                source_zones: vec![crate::types::zones::Zone::Library],
            },
            vec![],
            ObjectId(9994),
            PlayerId(1),
        );

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(
            events.iter().any(|e| matches!(
                e,
                GameEvent::PlayerPerformedAction {
                    player_id,
                    action: PlayerActionKind::SearchedLibrary,
                } if *player_id == PlayerId(1)
            )),
            "CR 701.23f: a restricted (replaced) search still fires SearchedLibrary"
        );
        assert!(
            state
                .players_who_searched_library_this_turn
                .contains(&PlayerId(1)),
            "CR 701.23f: per-turn tracking records the restricted searcher"
        );
        let p1 = state.players.iter().find(|p| p.id == PlayerId(1)).unwrap();
        assert_eq!(
            p1.library.len(),
            6,
            "CR 701.23f: the whole library is intact — the shuffle tail sees every card"
        );
        for card in &lib {
            assert!(p1.library.contains(card));
        }
    }

    #[test]
    fn two_aven_sources_still_cap_to_top_four() {
        // CR 701.23f: two Aven Mindcensors → min(4, 4) = 4. The minimum-stacking
        // is idempotent for equal counts.
        let mut state = GameState::new_two_player(42);
        add_restrict_search_to_top_permanent(
            &mut state,
            PlayerId(0),
            ProhibitionScope::Opponents,
            4,
        );
        add_restrict_search_to_top_permanent(
            &mut state,
            PlayerId(0),
            ProhibitionScope::Opponents,
            4,
        );
        let _lib = stack_library_creatures(&mut state, PlayerId(1), 6, 1);

        let ability = ResolvedAbility::new(
            Effect::SearchLibrary {
                filter: TargetFilter::Typed(TypedFilter::creature()),
                count: QuantityExpr::Fixed { value: 1 },
                reveal: false,
                target_player: None,
                selection_constraint: SearchSelectionConstraint::None,
                split: None,
                source_zones: vec![crate::types::zones::Zone::Library],
            },
            vec![],
            ObjectId(9995),
            PlayerId(1),
        );

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        match &state.waiting_for {
            WaitingFor::SearchChoice { cards, .. } => {
                assert_eq!(cards.len(), 4, "two equal sources still cap to top 4")
            }
            other => panic!("expected SearchChoice, got {other:?}"),
        }
    }
}
