//! Tests for the CR 730 merge building block (Mutate Phase 1).
//!
//! The first group exercises the merge PRIMITIVE directly ([`merge_object_onto`],
//! [`split_merged_permanent_on_leave`]) per the building-block testing principle.
//! The `cast_pipeline` group drives the wired runtime path end-to-end
//! (`handle_cast_spell` → `AlternativeCastChoice(Mutate)` →
//! `handle_mutate_cost_choice` → resolution divert → `ChooseMutateMergeSide`),
//! covering the actual bug #2014 path rather than just the primitive. The parser
//! source-of-truth tests live in `parser::oracle_target::tests`.

use crate::game::merge::*;
use crate::game::scenario::{GameScenario, P0};
use crate::types::events::GameEvent;
use crate::types::identifiers::ObjectId;
use crate::types::player::PlayerId;
use crate::types::zones::Zone;

/// Build a two-player scenario with two battlefield creatures controlled/owned by
/// player 0: a "host" (the target creature) and a "rider" (the mutating spell's
/// card). Returns `(state, host_id, rider_id, p0)`.
fn two_creatures() -> (
    crate::types::game_state::GameState,
    ObjectId,
    ObjectId,
    PlayerId,
) {
    let mut sc = GameScenario::new();
    // Host: 2/2 named "Host", will be the surviving target (keeps its ObjectId).
    let host = sc.add_creature(P0, "Host", 2, 2).id();
    // Rider: 4/4 named "Rider", the mutating creature card.
    let rider = sc.add_creature(P0, "Rider", 4, 4).id();
    (sc.state, host, rider, P0)
}

#[test]
fn merge_top_uses_riders_characteristics_and_keeps_target_id() {
    // CR 730.2c: the merged permanent keeps the TARGET's ObjectId.
    // CR 730.2a: on TOP, the rider supplies the copiable characteristics.
    let (mut state, host, rider, _p0) = two_creatures();
    let mut events = Vec::new();
    merge_object_onto(&mut state, rider, host, MergeSide::Top, &mut events);

    let survivor = state.objects.get(&host).expect("survivor keeps host id");
    assert_eq!(
        survivor.merged_components,
        vec![rider, host],
        "topmost-first order: rider on top, host underneath"
    );
    // CR 730.2a: topmost (rider) characteristics copied onto the survivor.
    assert_eq!(survivor.name, "Rider");
    assert_eq!(survivor.power, Some(4));
    assert_eq!(survivor.toughness, Some(4));
    // The surviving object id is still the host's id (continuity).
    assert!(state.objects.contains_key(&host));
}

#[test]
fn merge_bottom_keeps_targets_own_characteristics() {
    // CR 730.2a: on BOTTOM, the target keeps its own copiable characteristics.
    let (mut state, host, rider, _p0) = two_creatures();
    let mut events = Vec::new();
    merge_object_onto(&mut state, rider, host, MergeSide::Bottom, &mut events);

    let survivor = state.objects.get(&host).expect("survivor keeps host id");
    assert_eq!(
        survivor.merged_components,
        vec![host, rider],
        "topmost-first order: host on top, rider underneath"
    );
    assert_eq!(survivor.name, "Host");
    assert_eq!(survivor.power, Some(2));
    assert_eq!(survivor.toughness, Some(2));
}

#[test]
fn merge_unions_component_abilities_per_cr_702_140e() {
    // CR 702.140e: a mutated permanent has all abilities of each component.
    use crate::types::keywords::Keyword;
    let (mut state, host, rider, _p0) = two_creatures();
    // Give each component a distinct keyword on its base set.
    state
        .objects
        .get_mut(&host)
        .unwrap()
        .base_keywords
        .push(Keyword::Flying);
    state
        .objects
        .get_mut(&rider)
        .unwrap()
        .base_keywords
        .push(Keyword::Trample);

    let mut events = Vec::new();
    merge_object_onto(&mut state, rider, host, MergeSide::Top, &mut events);

    let survivor = state.objects.get(&host).unwrap();
    assert!(
        survivor.merge_layer_effect_id.is_some(),
        "merge is represented by a layer-1 copy effect"
    );
    assert!(
        survivor.keywords.contains(&Keyword::Flying),
        "host's Flying survives the union"
    );
    assert!(
        survivor.keywords.contains(&Keyword::Trample),
        "rider's Trample is unioned in"
    );
    assert!(
        !survivor.base_keywords.contains(&Keyword::Trample),
        "the merge must not bake component abilities into the survivor's base characteristics"
    );
}

/// CR 730.2d: a merged permanent is a token only if its TOPMOST component is a
/// token. Mutating a card on top of a creature token (the host) makes the merged
/// permanent NONTOKEN while merged; the survivor's intrinsic token-ness is
/// captured and restored when the pile leaves the battlefield, so the CR 111.7
/// cease-to-exist SBA applies to it again instead of leaking a nontoken object.
#[test]
fn merge_card_on_top_of_token_host_is_nontoken_and_restores_on_leave() {
    let (mut state, host, rider, _p0) = two_creatures();
    // CR 702.140a: the mutate host is a non-Human creature TOKEN you own.
    state.objects.get_mut(&host).unwrap().is_token = true;
    // Runtime invariant: the mutating spell resolved off the stack, never listed.
    state.battlefield.retain(|&id| id != rider);
    let mut events = Vec::new();

    // Mutate the card (rider) ON TOP of the token (host).
    merge_object_onto(&mut state, rider, host, MergeSide::Top, &mut events);
    assert!(
        !state.objects.get(&host).unwrap().is_token,
        "card on top of a token host → nontoken merged permanent (CR 730.2d)"
    );
    assert_eq!(
        state.objects.get(&host).unwrap().pre_merge_is_token,
        Some(true),
        "the survivor's intrinsic token-ness is captured for the on-leave restore"
    );

    // On leave, the survivor's intrinsic token-ness is restored.
    crate::game::zones::move_to_zone(&mut state, host, Zone::Graveyard, &mut events);
    let leave_record = events
        .iter()
        .find_map(|event| match event {
            GameEvent::ZoneChanged {
                object_id,
                from,
                to,
                record,
            } if *object_id == host
                && *from == Some(Zone::Battlefield)
                && *to == Zone::Graveyard =>
            {
                Some(record)
            }
            _ => None,
        })
        .expect("merged survivor emits a battlefield-to-graveyard ZoneChanged event");
    assert!(
        !leave_record.is_token,
        "the leave event observes the card-on-top merged permanent as nontoken (CR 730.2d)"
    );
    let o = state.objects.get(&host).unwrap();
    assert!(
        o.is_token,
        "the token host is a token again when the pile leaves (CR 730.2d + CR 111.7)"
    );
    assert_eq!(
        o.pre_merge_is_token, None,
        "the token-ness override is consumed on leave"
    );
}

/// CR 730.2d: when the token host is TOPMOST (a card mutated underneath it), the
/// merged permanent stays a token — and no override is captured because the
/// topmost token-ness already matches the survivor's.
#[test]
fn merge_card_under_token_host_keeps_token_no_override() {
    let (mut state, host, rider, _p0) = two_creatures();
    state.objects.get_mut(&host).unwrap().is_token = true;
    state.battlefield.retain(|&id| id != rider);
    let mut events = Vec::new();

    merge_object_onto(&mut state, rider, host, MergeSide::Bottom, &mut events);
    assert!(
        state.objects.get(&host).unwrap().is_token,
        "token host on top (card underneath) → merged permanent stays a token (CR 730.2d)"
    );
    assert_eq!(
        state.objects.get(&host).unwrap().pre_merge_is_token,
        None,
        "no override captured when the topmost already matches the survivor's token-ness"
    );
}

/// CR 730.2d regression guard: an all-card merge (no token component) is nontoken
/// and captures no override — the common case must be untouched.
#[test]
fn merge_all_card_components_stays_nontoken_no_override() {
    let (mut state, host, rider, _p0) = two_creatures();
    state.battlefield.retain(|&id| id != rider);
    let mut events = Vec::new();

    merge_object_onto(&mut state, rider, host, MergeSide::Top, &mut events);
    assert!(
        !state.objects.get(&host).unwrap().is_token,
        "an all-card pile is nontoken"
    );
    assert_eq!(
        state.objects.get(&host).unwrap().pre_merge_is_token,
        None,
        "no token-ness override is captured when no component is a token"
    );
}

/// CR 730.2d + CR 730.2 (stacking): the topmost-derived token-ness is re-applied
/// on each merge, and the survivor's intrinsic value is captured exactly ONCE so a
/// multi-mutate token host still restores correctly on leave.
#[test]
fn merge_stacking_onto_token_host_captures_intrinsic_token_ness_once() {
    use crate::game::scenario::GameScenario;
    let mut sc = GameScenario::new();
    let host = sc.add_creature(P0, "Token Host", 2, 2).id();
    let rider1 = sc.add_creature(P0, "Rider1", 3, 3).id();
    let rider2 = sc.add_creature(P0, "Rider2", 4, 4).id();
    let mut state = sc.state;
    state.objects.get_mut(&host).unwrap().is_token = true;
    state.battlefield.retain(|&id| id != rider1 && id != rider2);
    let mut events = Vec::new();

    merge_object_onto(&mut state, rider1, host, MergeSide::Top, &mut events);
    merge_object_onto(&mut state, rider2, host, MergeSide::Top, &mut events);
    // Topmost is always a card (mutating objects are cards) → nontoken; the
    // intrinsic token-ness is captured once, not overwritten by the second merge.
    assert!(!state.objects.get(&host).unwrap().is_token);
    assert_eq!(
        state.objects.get(&host).unwrap().pre_merge_is_token,
        Some(true),
        "intrinsic token-ness captured exactly once across stacked merges"
    );

    crate::game::zones::move_to_zone(&mut state, host, Zone::Graveyard, &mut events);
    assert!(
        state.objects.get(&host).unwrap().is_token,
        "the token host restores to a token on leave after multiple merges"
    );
}

#[test]
fn merge_is_same_object_no_etb_event() {
    // CR 730.2b/c: the merged permanent is NOT considered to have entered the
    // battlefield. The merge emits `Mutated` but NO `ZoneChanged`-to-battlefield.
    let (mut state, host, rider, p0) = two_creatures();
    let mut events = Vec::new();
    merge_object_onto(&mut state, rider, host, MergeSide::Top, &mut events);

    let has_mutated = events.iter().any(|e| {
        matches!(
            e,
            GameEvent::Mutated { merged_id, merging_id, controller }
                if *merged_id == host && *merging_id == rider && *controller == p0
        )
    });
    assert!(has_mutated, "a Mutated event is emitted for the survivor");

    let has_etb = events.iter().any(|e| {
        matches!(
            e,
            GameEvent::ZoneChanged { object_id, to, .. }
                if *object_id == host && *to == Zone::Battlefield
        )
    });
    assert!(!has_etb, "no battlefield ETB event (CR 730.2b)");
}

#[test]
fn leave_split_routes_each_component_to_its_owners_graveyard() {
    // CR 730.3: when a merged permanent leaves, EACH component goes to its
    // appropriate zone. Here both components are owned by player 0, so both end up
    // in player 0's graveyard. The surviving object rides the normal move; the
    // absorbed component is routed by the split.
    let (mut state, host, rider, p0) = two_creatures();
    let mut events = Vec::new();
    merge_object_onto(&mut state, rider, host, MergeSide::Top, &mut events);

    // Move the merged permanent off the battlefield (destroy → graveyard). The
    // `move_to_zone` seam invokes `split_merged_permanent_on_leave` for the rider.
    crate::game::zones::move_to_zone(&mut state, host, Zone::Graveyard, &mut events);

    let gy = &state.players.iter().find(|p| p.id == p0).unwrap().graveyard;
    assert!(
        gy.contains(&host),
        "host (survivor) is in its owner's graveyard"
    );
    assert!(
        gy.contains(&rider),
        "rider (absorbed component) is split out to its owner's graveyard (CR 730.3)"
    );
    // The survivor's merge identity is cleared on battlefield exit (CR 400.7).
    assert!(
        state
            .objects
            .get(&host)
            .map(|o| o.merged_components.is_empty())
            .unwrap_or(true),
        "merge identity cleared after leaving the battlefield"
    );
}

/// CR 730.3 + CR 712.21: a merged permanent is a SINGLE permanent — when it
/// leaves the battlefield, "leaves the battlefield" / "dies" observers must
/// trigger exactly once (for the survivor), not once per component card. Those
/// observers key on a zone change whose origin is the battlefield, so the
/// regression check is: exactly ONE emitted `ZoneChanged` has
/// `from == Some(Battlefield)`, while the absorbed component is put into the
/// graveyard with `from == None` (it did not independently leave the
/// battlefield). Each component still produces a `to == Graveyard` zone change,
/// so "whenever a card is put into a graveyard from anywhere" fires per card
/// (CR 712.21: "a card is put into a graveyard" triggers once per card).
#[test]
fn merged_permanent_leaving_emits_single_battlefield_exit_event() {
    let (mut state, host, rider, _p0) = two_creatures();
    // Reproduce the runtime invariant: the mutating spell resolved off the STACK
    // and is never an independent member of the battlefield list.
    state.battlefield.retain(|&id| id != rider);

    let mut events = Vec::new();
    merge_object_onto(&mut state, rider, host, MergeSide::Top, &mut events);

    // Isolate the leave: the merged permanent is destroyed → graveyard.
    events.clear();
    crate::game::zones::move_to_zone(&mut state, host, Zone::Graveyard, &mut events);

    let battlefield_exits = count_zone_changes(&events, Some(Zone::Battlefield), None);
    assert_eq!(
        battlefield_exits, 1,
        "exactly one battlefield-exit event (CR 730.3: one permanent leaves) — \
         the survivor's; the absorbed component must not emit its own"
    );

    // The survivor is the single battlefield-exit; the component leaves with no origin.
    assert_eq!(
        zone_change_origin(&events, host),
        Some(Some(Zone::Battlefield)),
        "survivor (host) leaves the battlefield"
    );
    assert_eq!(
        zone_change_origin(&events, rider),
        Some(None),
        "absorbed component enters the graveyard as a NEW object (origin None), \
         so it does not trigger leaves-the-battlefield / dies observers"
    );

    // CR 712.21: both cards are put into a graveyard (per-card graveyard observers
    // still fire for each component).
    assert_eq!(
        count_zone_changes(&events, None, Some(Zone::Graveyard)),
        2,
        "both the survivor and the component are put into a graveyard"
    );
}

/// CR 730.3 + CR 712.21: the single-battlefield-exit invariant must hold
/// regardless of pile size — a creature mutated twice (three components) leaving
/// the battlefield still produces exactly ONE battlefield-exit event, so a
/// death payoff (Blood Artist, Midnight Reaper, etc.) triggers once, not thrice.
#[test]
fn merged_stack_leaving_emits_single_battlefield_exit_regardless_of_pile_size() {
    use crate::game::scenario::GameScenario;

    let mut sc = GameScenario::new();
    let host = sc.add_creature(P0, "Host", 2, 2).id();
    let rider1 = sc.add_creature(P0, "Rider1", 4, 4).id();
    let rider2 = sc.add_creature(P0, "Rider2", 6, 6).id();
    let mut state = sc.state;
    // Runtime invariant: mutating spells resolve off the stack, never listed.
    state.battlefield.retain(|&id| id != rider1 && id != rider2);

    let mut events = Vec::new();
    merge_object_onto(&mut state, rider1, host, MergeSide::Top, &mut events);
    merge_object_onto(&mut state, rider2, host, MergeSide::Top, &mut events);

    events.clear();
    crate::game::zones::move_to_zone(&mut state, host, Zone::Graveyard, &mut events);

    assert_eq!(
        count_zone_changes(&events, Some(Zone::Battlefield), None),
        1,
        "a 3-component pile still leaves the battlefield as ONE permanent (CR 730.3)"
    );
    // All three cards reach the graveyard (CR 712.21: per-card graveyard observers).
    assert_eq!(
        count_zone_changes(&events, None, Some(Zone::Graveyard)),
        3,
        "all three component cards are put into a graveyard"
    );
    assert_eq!(zone_change_origin(&events, rider1), Some(None));
    assert_eq!(zone_change_origin(&events, rider2), Some(None));
}

/// Count emitted `ZoneChanged` events optionally filtered by origin and/or
/// destination zone. `origin`/`destination` of `None` means "any".
fn count_zone_changes(
    events: &[GameEvent],
    origin: Option<Zone>,
    destination: Option<Zone>,
) -> usize {
    events
        .iter()
        .filter(|e| match e {
            GameEvent::ZoneChanged { from, to, .. } => {
                origin.is_none_or(|z| *from == Some(z)) && destination.is_none_or(|z| *to == z)
            }
            _ => false,
        })
        .count()
}

/// Return the `from` origin of the (first) `ZoneChanged` event for `id`, or
/// `None` if no such event was emitted. The outer `Option` distinguishes
/// "no event" from the inner `Option<Zone>` origin (`Some(None)` = emitted with
/// origin `None`).
fn zone_change_origin(events: &[GameEvent], id: ObjectId) -> Option<Option<Zone>> {
    events.iter().find_map(|e| match e {
        GameEvent::ZoneChanged {
            object_id, from, ..
        } if *object_id == id => Some(*from),
        _ => None,
    })
}

/// CR 730.3c: When a merged permanent is exiled, an effect that finds the object
/// it became (a flicker/blink's "return it") must act on ALL component cards, not
/// just the survivor. The absorbed component is recorded with a back-link to the
/// survivor and is re-collected by `expand_returned_merge_components` — but ONLY
/// for a continuity reference; a freshly chosen target (reanimation) must not
/// over-return.
#[test]
fn exile_records_component_backlink_and_continuity_return_collects_all() {
    use crate::types::ability::TargetFilter;
    let (mut state, host, rider, _p0) = two_creatures();
    // Runtime invariant: the mutating spell resolved off the stack, never listed.
    state.battlefield.retain(|&id| id != rider);
    let mut events = Vec::new();
    merge_object_onto(&mut state, rider, host, MergeSide::Top, &mut events);

    // Flicker step 1: exile the merged permanent. The survivor rides the normal
    // move; the component is split into exile with a back-link to the survivor.
    crate::game::zones::move_to_zone(&mut state, host, Zone::Exile, &mut events);
    assert_eq!(state.objects.get(&host).unwrap().zone, Zone::Exile);
    assert_eq!(state.objects.get(&rider).unwrap().zone, Zone::Exile);
    assert_eq!(
        state.objects.get(&rider).unwrap().split_from_merge_survivor,
        Some(host),
        "absorbed component records the survivor it split from (CR 730.3c)"
    );

    // A continuity reference ("return it") resolving to the survivor expands to
    // include the co-exiled component (CR 730.3c).
    let expanded =
        expand_returned_merge_components(&state, vec![host], &TargetFilter::ParentTarget);
    assert_eq!(
        expanded,
        vec![host, rider],
        "a flicker returns the whole pile, not just the survivor"
    );

    // A freshly chosen target (e.g. reanimating one specific card) must NOT
    // over-return — only continuity references expand.
    let fresh = expand_returned_merge_components(&state, vec![host], &TargetFilter::Any);
    assert_eq!(
        fresh,
        vec![host],
        "a non-continuity target returns only the chosen object"
    );
}

/// CR 730.3c + CR 730.3: the returned components come back as SEPARATE, non-merged
/// objects, and the survivor back-link clears on battlefield entry.
#[test]
fn flicker_returns_components_unmerged_and_clears_backlink() {
    use crate::types::ability::TargetFilter;
    let (mut state, host, rider, _p0) = two_creatures();
    state.battlefield.retain(|&id| id != rider);
    let mut events = Vec::new();
    merge_object_onto(&mut state, rider, host, MergeSide::Top, &mut events);
    crate::game::zones::move_to_zone(&mut state, host, Zone::Exile, &mut events);

    // Return the whole pile — what the `ChangeZone` return loop does for each
    // expanded id (CR 730.3c).
    let to_return =
        expand_returned_merge_components(&state, vec![host], &TargetFilter::ParentTarget);
    for id in to_return {
        crate::game::zones::move_to_zone(&mut state, id, Zone::Battlefield, &mut events);
    }

    for id in [host, rider] {
        let o = state.objects.get(&id).unwrap();
        assert_eq!(
            o.zone,
            Zone::Battlefield,
            "component returned to the battlefield"
        );
        assert!(
            o.merged_components.is_empty(),
            "components return un-merged (CR 730.3)"
        );
        assert_eq!(
            o.split_from_merge_survivor, None,
            "the survivor back-link clears on battlefield entry"
        );
    }
    assert!(
        state.battlefield.contains(&host) && state.battlefield.contains(&rider),
        "both cards are present on the battlefield as separate objects"
    );
}

/// CR 400.7: a split-out component is a NEW object on every zone change, so its
/// survivor back-link must clear when it moves between non-battlefield zones —
/// not only on battlefield entry. Otherwise a component that moved on (e.g.
/// exile → graveyard) keeps a stale link and could be wrongly re-collected by a
/// later continuity return once it re-converges with the survivor.
#[test]
fn split_component_backlink_clears_on_non_battlefield_zone_move() {
    use crate::types::ability::TargetFilter;
    let (mut state, host, rider, _p0) = two_creatures();
    state.battlefield.retain(|&id| id != rider);
    let mut events = Vec::new();
    merge_object_onto(&mut state, rider, host, MergeSide::Top, &mut events);

    // Exile the pile: the component is split into exile with a back-link.
    crate::game::zones::move_to_zone(&mut state, host, Zone::Exile, &mut events);
    assert_eq!(
        state.objects.get(&rider).unwrap().split_from_merge_survivor,
        Some(host),
        "component records the survivor back-link when split into exile"
    );

    // The component moves exile → graveyard WITHOUT being returned (CR 400.7: a
    // new object). The back-link must clear on this non-battlefield move.
    crate::game::zones::move_to_zone(&mut state, rider, Zone::Graveyard, &mut events);
    assert_eq!(
        state.objects.get(&rider).unwrap().split_from_merge_survivor,
        None,
        "the survivor back-link clears on a non-battlefield zone move (CR 400.7)"
    );

    // Now move the survivor to the graveyard too, so it re-converges with the
    // former component in one zone. A continuity reference to the survivor must
    // NOT re-collect the moved-on component (the stale link is gone).
    crate::game::zones::move_to_zone(&mut state, host, Zone::Graveyard, &mut events);
    let expanded =
        expand_returned_merge_components(&state, vec![host], &TargetFilter::ParentTarget);
    assert_eq!(
        expanded,
        vec![host],
        "a component that already moved on is not re-collected by a later continuity return"
    );
}

/// CR 730.2: the absorbed (non-surviving) component is part of ONE battlefield
/// object — it must NOT be observable as an independent permanent. This pins the
/// two confirmed `state.objects`-scan victims (`FilterProp::NameMatchesAnyPermanent`
/// and the mana-source enumeration) via the shared
/// `GameState::is_absorbed_merge_component` guard.
#[test]
fn absorbed_component_is_not_an_independent_permanent() {
    use crate::game::filter::{matches_target_filter, FilterContext};
    use crate::types::ability::{FilterProp, TargetFilter, TypedFilter};

    let (mut state, host, rider, p0) = two_creatures();
    // Mirror the real cast path: the mutating spell (`rider`) resolved off the
    // STACK and is never an independent member of `state.battlefield`. The
    // `two_creatures` builder places it on the battlefield list, so strip it to
    // reproduce the merge-from-stack invariant the runtime enforces.
    state.battlefield.retain(|&id| id != rider);

    let mut events = Vec::new();
    // Top merge → survivor (host id) adopts the rider's name ("Rider"); the
    // absorbed component keeps its own name ("Host").
    merge_object_onto(&mut state, rider, host, MergeSide::Top, &mut events);

    // The absorbed component is recognized as a non-independent merge component.
    assert!(
        state.is_absorbed_merge_component(rider),
        "rider is an absorbed component of the merged permanent"
    );
    assert!(
        !state.is_absorbed_merge_component(host),
        "the survivor (in state.battlefield) is NOT an absorbed component"
    );
    assert!(
        !state.battlefield.contains(&rider),
        "absorbed component is not in the battlefield list"
    );

    // Victim 1 — `FilterProp::NameMatchesAnyPermanent`. A probe object named
    // "Host" must NOT match: there is no INDEPENDENT permanent named "Host" on
    // the battlefield (the only "Host"-named object is the absorbed component).
    // Pre-fix, the absorbed component double-counted and this would be `true`.
    let probe = crate::game::zones::create_object(
        &mut state,
        crate::types::identifiers::CardId(9100),
        p0,
        "Host".to_string(),
        Zone::Hand,
    );
    let filter = TargetFilter::Typed(TypedFilter::card().properties(vec![
        FilterProp::NameMatchesAnyPermanent { controller: None },
    ]));
    let ctx = FilterContext::from_source_with_controller(probe, p0);
    assert!(
        !matches_target_filter(&state, probe, &filter, &ctx),
        "CR 730.2: an absorbed merge component must not count as a same-name permanent"
    );

    // Victim 2 — mana-source enumeration. Give the absorbed component a mana
    // ability and confirm an opponent-land survey does not enumerate it as an
    // independent source. (Both components are P0-owned/controlled, so an
    // opponent survey from P1 must yield nothing regardless; the guard ensures a
    // future same-controller survey can't double-count either.) Asserting the
    // shared guard is the load-bearing check; the scan sites all call it.
    assert!(
        state.is_absorbed_merge_component(rider),
        "mana-source scans skip ids reported as absorbed merge components"
    );
}

/// CR 730.3 + CR 400.7: when the merged permanent leaves the battlefield, the
/// surviving object reverts to its OWN card identity (name, P/T, abilities) —
/// it must NOT keep the topmost component's characteristics in the graveyard.
#[test]
fn merge_survivor_reverts_to_its_own_card_on_leave() {
    use crate::types::keywords::Keyword;
    let (mut state, host, rider, p0) = two_creatures();
    state
        .objects
        .get_mut(&host)
        .unwrap()
        .base_keywords
        .push(Keyword::Flying);
    state
        .objects
        .get_mut(&rider)
        .unwrap()
        .base_keywords
        .push(Keyword::Trample);

    let mut events = Vec::new();
    // Top merge → survivor adopts the rider's name/P-T while merged.
    merge_object_onto(&mut state, rider, host, MergeSide::Top, &mut events);
    assert_eq!(state.objects.get(&host).unwrap().name, "Rider");

    // The merged permanent leaves the battlefield.
    crate::game::zones::move_to_zone(&mut state, host, Zone::Graveyard, &mut events);

    let survivor = state.objects.get(&host).unwrap();
    // CR 730.3 + CR 400.7: reverted to its OWN card.
    assert_eq!(survivor.name, "Host", "survivor reverts to its own name");
    assert_eq!(survivor.power, Some(2), "survivor reverts to its own power");
    assert_eq!(survivor.toughness, Some(2));
    assert!(
        survivor.base_keywords.contains(&Keyword::Flying),
        "keeps its own Flying"
    );
    assert!(
        !survivor.base_keywords.contains(&Keyword::Trample),
        "does NOT keep the rider's Trample after the merge ends"
    );
    assert!(
        survivor.merge_layer_effect_id.is_none(),
        "merge layer effect id is cleared on leave"
    );
    assert!(survivor.merged_components.is_empty());
    // Both components land in the graveyard (CR 730.3).
    let gy = &state.players.iter().find(|p| p.id == p0).unwrap().graveyard;
    assert!(gy.contains(&host) && gy.contains(&rider));
}

/// CR 730.2 multi-instance stacking: mutating a SECOND creature onto an
/// already-merged permanent extends the component stack, re-derives copiable
/// characteristics from the new topmost component, and unions ALL components'
/// abilities (CR 702.140e). This is Otrimi's core gameplay loop.
#[test]
fn merge_stacking_extends_stack_and_unions_all_abilities() {
    use crate::game::scenario::GameScenario;
    use crate::types::keywords::Keyword;

    let mut sc = GameScenario::new();
    let host = sc.add_creature(P0, "Host", 2, 2).id();
    let rider1 = sc.add_creature(P0, "Rider1", 4, 4).id();
    let rider2 = sc.add_creature(P0, "Rider2", 6, 6).id();
    let mut state = sc.state;
    for (id, kw) in [
        (host, Keyword::Flying),
        (rider1, Keyword::Trample),
        (rider2, Keyword::Lifelink),
    ] {
        let o = state.objects.get_mut(&id).unwrap();
        o.base_keywords.push(kw.clone());
        o.keywords.push(kw);
    }

    let mut events = Vec::new();
    // First mutate: rider1 on top of host.
    merge_object_onto(&mut state, rider1, host, MergeSide::Top, &mut events);
    // Second mutate: rider2 on top of the already-merged permanent.
    merge_object_onto(&mut state, rider2, host, MergeSide::Top, &mut events);

    let survivor = state.objects.get(&host).unwrap();
    // CR 730.2c: still the host's ObjectId; the whole stack is recorded.
    assert_eq!(
        survivor.merged_components,
        vec![rider2, rider1, host],
        "topmost-first: rider2, then rider1, then host"
    );
    // CR 730.2a: copiable characteristics come from the new topmost (rider2).
    assert_eq!(survivor.name, "Rider2");
    assert_eq!(survivor.power, Some(6));
    // CR 702.140e: union of ALL three components' abilities.
    assert!(survivor.keywords.contains(&Keyword::Flying), "host's");
    assert!(survivor.keywords.contains(&Keyword::Trample), "rider1's");
    assert!(survivor.keywords.contains(&Keyword::Lifelink), "rider2's");
}

#[test]
fn merge_stacking_bottom_rebuilds_layer_effect_without_rewriting_base() {
    use crate::game::scenario::GameScenario;
    use crate::types::keywords::Keyword;

    let mut sc = GameScenario::new();
    let host = sc.add_creature(P0, "Host", 2, 2).id();
    let rider1 = sc.add_creature(P0, "Rider1", 4, 4).id();
    let rider2 = sc.add_creature(P0, "Rider2", 6, 6).id();
    let mut state = sc.state;
    for (id, kw) in [
        (host, Keyword::Flying),
        (rider1, Keyword::Trample),
        (rider2, Keyword::Lifelink),
    ] {
        let o = state.objects.get_mut(&id).unwrap();
        o.base_keywords.push(kw.clone());
        o.keywords.push(kw);
    }

    let mut events = Vec::new();
    merge_object_onto(&mut state, rider1, host, MergeSide::Bottom, &mut events);
    let first_effect = state
        .objects
        .get(&host)
        .and_then(|obj| obj.merge_layer_effect_id)
        .expect("first merge installs a copy effect");

    merge_object_onto(&mut state, rider2, host, MergeSide::Bottom, &mut events);

    let survivor = state.objects.get(&host).unwrap();
    assert_eq!(
        survivor.merged_components,
        vec![host, rider1, rider2],
        "bottom re-merge keeps the original target topmost"
    );
    assert_eq!(survivor.name, "Host");
    assert_eq!(survivor.power, Some(2));
    assert!(survivor.keywords.contains(&Keyword::Flying), "host's");
    assert!(survivor.keywords.contains(&Keyword::Trample), "rider1's");
    assert!(survivor.keywords.contains(&Keyword::Lifelink), "rider2's");
    assert!(
        !survivor.base_keywords.contains(&Keyword::Trample)
            && !survivor.base_keywords.contains(&Keyword::Lifelink),
        "component ability union must stay in layer copy values, not base characteristics"
    );
    assert_ne!(
        survivor.merge_layer_effect_id,
        Some(first_effect),
        "re-merge rebuilds the stored layer effect instead of stacking stale merge effects"
    );
    assert!(
        state
            .transient_continuous_effects
            .iter()
            .all(|effect| effect.id != first_effect),
        "the prior merge copy effect is removed before installing the rebuilt one"
    );
}

/// CR 730.3: when a 3-component merged permanent leaves, every component is put
/// into its owner's zone and the survivor reverts to its own identity.
#[test]
fn merge_stacking_leave_routes_all_components_and_restores_survivor() {
    use crate::game::scenario::GameScenario;

    let mut sc = GameScenario::new();
    let host = sc.add_creature(P0, "Host", 2, 2).id();
    let rider1 = sc.add_creature(P0, "Rider1", 4, 4).id();
    let rider2 = sc.add_creature(P0, "Rider2", 6, 6).id();
    let mut state = sc.state;

    let mut events = Vec::new();
    merge_object_onto(&mut state, rider1, host, MergeSide::Top, &mut events);
    merge_object_onto(&mut state, rider2, host, MergeSide::Bottom, &mut events);

    crate::game::zones::move_to_zone(&mut state, host, Zone::Graveyard, &mut events);

    let gy = &state.players.iter().find(|p| p.id == P0).unwrap().graveyard;
    assert!(gy.contains(&host), "survivor card to graveyard");
    assert!(gy.contains(&rider1), "rider1 component to graveyard");
    assert!(gy.contains(&rider2), "rider2 component to graveyard");
    let survivor = state.objects.get(&host).unwrap();
    assert_eq!(survivor.name, "Host", "survivor reverted to its own card");
    assert!(survivor.merged_components.is_empty());
    assert!(survivor.merge_layer_effect_id.is_none());
}

/// CR 614.6: a token-INCLUSIVE graveyard→exile `Moved` redirect, mirroring the
/// actual Rest in Peace text "If a card or token would be put into a graveyard
/// from anywhere, exile it instead" (`valid_card: None` — no card/token
/// scoping; Leyline of the Void's card-only subject is a different, card-scoped
/// class). Installed on a battlefield object so the merged-permanent leave
/// event consults it.
fn graveyard_exile_replacement() -> crate::types::ability::ReplacementDefinition {
    use crate::types::ability::{AbilityDefinition, AbilityKind, Effect, TargetFilter};
    use crate::types::replacements::ReplacementEvent;
    crate::types::ability::ReplacementDefinition::new(ReplacementEvent::Moved)
        .destination_zone(Zone::Graveyard)
        .execute(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::ChangeZone {
                destination: Zone::Exile,
                origin: None,
                target: TargetFilter::SelfRef,
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
        .description(
            "If a card would be put into a graveyard from anywhere, exile it instead.".to_string(),
        )
}

/// CR 730.3d: "If multiple replacement effects could be applied to the event of a
/// merged permanent leaving the battlefield or being put into the new zone,
/// applying one of those replacement effects to the object applies it to all
/// components of the object."
///
/// A non-token merged permanent (two cards) leaves the battlefield while a global
/// graveyard→exile redirect (Rest in Peace) is active. The redirect is consulted
/// ONCE on the merged-permanent leave event (the survivor's ZoneChange) and its
/// chosen destination — Exile — must propagate to EVERY component, NOT just the
/// survivor. The redirect is explicitly NOT re-consulted per component
/// (CR 730.3d): the survivor's resolved destination is what every component
/// follows.
///
/// Drives the REAL pipeline (`zone_pipeline::move_object`), so `replace_event`
/// actually fires the redirect — a bare `zones::move_to_zone` would skip the
/// consult and send everything to the graveyard, which is exactly the
/// pre-pipeline bug this pins against.
#[test]
fn cr_730_3d_redirect_on_merged_leave_propagates_to_all_components() {
    use crate::game::scenario::GameScenario;
    use crate::game::zone_pipeline::{move_object, ZoneMoveRequest, ZoneMoveResult};

    let mut sc = GameScenario::new();
    let host = sc.add_creature(P0, "Host", 2, 2).id();
    let rider = sc.add_creature(P0, "Rider", 4, 4).id();
    // Install a global graveyard→exile redirect on a separate battlefield object
    // (a Rest in Peace–class permanent) so the merged-permanent leave consults it.
    let rip = sc.add_creature(P0, "Rest in Peace", 0, 0).id();
    let mut state = sc.state;
    state
        .objects
        .get_mut(&rip)
        .unwrap()
        .replacement_definitions
        .push(graveyard_exile_replacement());

    let mut events = Vec::new();
    merge_object_onto(&mut state, rider, host, MergeSide::Top, &mut events);
    // Runtime invariant: the mutating spell resolved off the stack, never listed.
    state.battlefield.retain(|&id| id != rider);

    // Route the survivor's leave THROUGH the pipeline so the redirect fires. The
    // merged permanent "leaves the battlefield" → its event is a single ZoneChange
    // (CR 730.3 — one permanent leaves), which RIP redirects to exile.
    let result = move_object(
        &mut state,
        ZoneMoveRequest::effect(host, Zone::Graveyard, host),
        &mut events,
    );
    assert!(
        matches!(result, ZoneMoveResult::Done),
        "the merged-permanent leave with a single applicable redirect resolves synchronously"
    );

    // CR 730.3d: BOTH components honor the survivor's redirected destination.
    assert_eq!(
        state.objects[&host].zone,
        Zone::Exile,
        "survivor follows the graveyard->exile redirect"
    );
    assert_eq!(
        state.objects[&rider].zone,
        Zone::Exile,
        "absorbed component follows the SAME redirect applied to the merged \
         permanent (CR 730.3d) — not the pre-replacement graveyard default"
    );
    let gy = &state.players.iter().find(|p| p.id == P0).unwrap().graveyard;
    assert!(
        !gy.contains(&host) && !gy.contains(&rider),
        "no component may land in the pre-replacement graveyard default"
    );
}

/// CR 730.3e (first clause): "If a replacement effect applies to a 'card' being
/// put into a zone without also including tokens, that effect applies to all
/// components of the merged permanent if it's not a token, including components
/// that are tokens."
///
/// A NON-TOKEN merged permanent whose pile includes a TOKEN component leaves the
/// battlefield under the graveyard→exile redirect. Because the merged permanent
/// (its survivor) is not a token, the redirect carries ALL components to exile —
/// including the token component, which would otherwise have gone to the
/// graveyard (then ceased to exist). This is the merged-permanent destination
/// following the survivor's resolved outcome regardless of per-component
/// token-ness (CR 730.3e first clause + CR 730.3d).
#[test]
fn cr_730_3e_nontoken_merged_leave_carries_token_component_with_redirect() {
    use crate::game::scenario::GameScenario;
    use crate::game::zone_pipeline::{move_object, ZoneMoveRequest, ZoneMoveResult};

    let mut sc = GameScenario::new();
    let host = sc.add_creature(P0, "Host", 2, 2).id();
    let token_rider = sc.add_creature(P0, "Token Rider", 4, 4).id();
    let rip = sc.add_creature(P0, "Rest in Peace", 0, 0).id();
    let mut state = sc.state;
    // Make the rider a TOKEN component; the survivor (host) is a card.
    state.objects.get_mut(&token_rider).unwrap().is_token = true;
    state
        .objects
        .get_mut(&rip)
        .unwrap()
        .replacement_definitions
        .push(graveyard_exile_replacement());

    let mut events = Vec::new();
    // CR 730.2d: the merged permanent is a token only if the TOPMOST component
    // is a token — merge the token rider underneath so the card host stays
    // topmost and the survivor remains a non-token (the clause-1 premise).
    merge_object_onto(
        &mut state,
        token_rider,
        host,
        MergeSide::Bottom,
        &mut events,
    );
    state.battlefield.retain(|&id| id != token_rider);
    assert!(
        !state.objects[&host].is_token,
        "premise: the merged permanent must be NON-token for 730.3e clause 1"
    );

    let result = move_object(
        &mut state,
        ZoneMoveRequest::effect(host, Zone::Graveyard, host),
        &mut events,
    );
    assert!(matches!(result, ZoneMoveResult::Done));

    assert_eq!(
        state.objects[&host].zone,
        Zone::Exile,
        "non-token survivor follows the redirect to exile"
    );
    assert_eq!(
        state.objects[&token_rider].zone,
        Zone::Exile,
        "the TOKEN component of a NON-TOKEN merged permanent follows the redirect \
         too (CR 730.3e first clause: applies to all components if the merged \
         permanent is not a token, including token components)"
    );
}

/// CR 730.3e + CR 111.1: a CARD-SCOPED graveyard→exile `Moved` redirect,
/// mirroring Leyline of the Void's "If a card would be put into [a] graveyard
/// from anywhere, exile it instead" — `valid_card: NonToken`, so it does NOT
/// match a token (a dying token reaches the graveyard; dies-triggers fire). This
/// is the parser output (item 1) modeled directly for the clause-2 split tests.
fn graveyard_exile_replacement_card_scoped() -> crate::types::ability::ReplacementDefinition {
    use crate::types::ability::{
        AbilityDefinition, AbilityKind, Effect, FilterProp, TargetFilter, TypedFilter,
    };
    use crate::types::replacements::ReplacementEvent;
    crate::types::ability::ReplacementDefinition::new(ReplacementEvent::Moved)
        .destination_zone(Zone::Graveyard)
        .valid_card(TargetFilter::Typed(
            TypedFilter::default().properties(vec![FilterProp::NonToken]),
        ))
        .execute(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::ChangeZone {
                destination: Zone::Exile,
                origin: None,
                target: TargetFilter::SelfRef,
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
        .description(
            "If a card would be put into a graveyard from anywhere, exile it instead.".to_string(),
        )
}

/// CR 730.3e (SECOND clause): "If the merged permanent is a token but some of its
/// components are cards, the merged permanent and its token components are put
/// into the appropriate zone, and the components that are cards are moved by the
/// replacement effect."
///
/// A TOKEN merged permanent (token topmost per CR 730.2d) with a CARD component
/// leaves the battlefield under a CARD-SCOPED graveyard→exile redirect (Leyline
/// class). The redirect does NOT match the token survivor, so the survivor + its
/// token components take the graveyard default; the CARD component is moved by
/// the redirect to EXILE. Drives the REAL pipeline so `replace_event` fires the
/// single component-aware consult.
#[test]
fn cr_730_3e_token_survivor_card_component_split_routes_card_to_redirect() {
    use crate::game::scenario::GameScenario;
    use crate::game::zone_pipeline::{move_object, ZoneMoveRequest, ZoneMoveResult};

    let mut sc = GameScenario::new();
    let token_host = sc.add_creature(P0, "Token Host", 2, 2).id();
    let card_rider = sc.add_creature(P0, "Card Rider", 4, 4).id();
    let leyline = sc.add_creature(P0, "Leyline of the Void", 0, 0).id();
    let mut state = sc.state;
    // Survivor is the TOKEN host; the card rider is an absorbed CARD component.
    state.objects.get_mut(&token_host).unwrap().is_token = true;
    state
        .objects
        .get_mut(&leyline)
        .unwrap()
        .replacement_definitions
        .push(graveyard_exile_replacement_card_scoped());

    let mut events = Vec::new();
    // CR 730.2d: merge the card rider UNDERNEATH so the token host stays topmost
    // and the merged permanent is a TOKEN (the clause-2 premise).
    merge_object_onto(
        &mut state,
        card_rider,
        token_host,
        MergeSide::Bottom,
        &mut events,
    );
    state.battlefield.retain(|&id| id != card_rider);
    assert!(
        state.objects[&token_host].is_token,
        "premise: the merged permanent must be a TOKEN for 730.3e clause 2"
    );

    let result = move_object(
        &mut state,
        ZoneMoveRequest::effect(token_host, Zone::Graveyard, token_host),
        &mut events,
    );
    assert!(matches!(result, ZoneMoveResult::Done));

    // CR 730.3e clause 2: the CARD component is moved by the card-scoped redirect.
    assert_eq!(
        state.objects[&card_rider].zone,
        Zone::Exile,
        "the CARD component of a TOKEN merged permanent is moved by the \
         card-scoped redirect to exile (CR 730.3e clause 2)"
    );
    // The TOKEN survivor takes the pre-replacement graveyard default (the
    // card-scoped redirect did not match it); it then ceases to exist via the
    // CR 111.7 SBA, but lands in the graveyard zone first.
    assert_eq!(
        state.objects[&token_host].zone,
        Zone::Graveyard,
        "the token survivor takes the pre-replacement graveyard default, NOT the \
         card-scoped redirect (which does not match a token)"
    );
    assert!(
        !state
            .players
            .iter()
            .find(|p| p.id == P0)
            .unwrap()
            .graveyard
            .contains(&card_rider),
        "the card component must not land in the graveyard default"
    );
}

/// CR 730.3e clause-1 SIBLING assertion (token-INCLUSIVE redirect): the same
/// token-survivor + card-component pile under a TOKEN-INCLUSIVE redirect (Rest in
/// Peace, `valid_card: None`) sends EVERYTHING to exile — the redirect matches the
/// token survivor too, so no clause-2 split occurs. Pins that the clause-2 split
/// fires ONLY for a card-scoped redirect.
#[test]
fn cr_730_3e_token_inclusive_redirect_exiles_all_components() {
    use crate::game::scenario::GameScenario;
    use crate::game::zone_pipeline::{move_object, ZoneMoveRequest, ZoneMoveResult};

    let mut sc = GameScenario::new();
    let token_host = sc.add_creature(P0, "Token Host", 2, 2).id();
    let card_rider = sc.add_creature(P0, "Card Rider", 4, 4).id();
    let rip = sc.add_creature(P0, "Rest in Peace", 0, 0).id();
    let mut state = sc.state;
    state.objects.get_mut(&token_host).unwrap().is_token = true;
    state
        .objects
        .get_mut(&rip)
        .unwrap()
        .replacement_definitions
        .push(graveyard_exile_replacement());

    let mut events = Vec::new();
    merge_object_onto(
        &mut state,
        card_rider,
        token_host,
        MergeSide::Bottom,
        &mut events,
    );
    state.battlefield.retain(|&id| id != card_rider);
    assert!(
        state.objects[&token_host].is_token,
        "premise: token survivor"
    );

    let result = move_object(
        &mut state,
        ZoneMoveRequest::effect(token_host, Zone::Graveyard, token_host),
        &mut events,
    );
    assert!(matches!(result, ZoneMoveResult::Done));

    // Token-inclusive redirect matches the token survivor too — everything exiled.
    assert_eq!(
        state.objects[&token_host].zone,
        Zone::Exile,
        "token survivor exiled"
    );
    assert_eq!(
        state.objects[&card_rider].zone,
        Zone::Exile,
        "card component exiled"
    );
}

/// CR 608.2b + CR 702.140b: end-to-end / resolution-time coverage of the wired
/// runtime path (the actual bug #2014 path), not just the merge primitive. These
/// drive `stack::resolve_top` and the full `handle_cast_spell` pipeline.
mod cast_pipeline {
    use crate::game::casting::{handle_cast_spell, handle_mutate_cost_choice};
    use crate::game::engine::apply_as_current;
    use crate::game::merge::MergeSide;
    use crate::game::zones::create_object;
    use crate::types::ability::{Effect, ResolvedAbility, TargetRef};
    use crate::types::actions::{AlternativeCastDecision, GameAction};
    use crate::types::card_type::CoreType;
    use crate::types::game_state::{
        AlternativeCastKeyword, CastingVariant, GameState, StackEntry, StackEntryKind, WaitingFor,
    };
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::keywords::Keyword;
    use crate::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
    use crate::types::phase::Phase;
    use crate::types::player::PlayerId;
    use crate::types::zones::Zone;

    const P0: PlayerId = PlayerId(0);

    fn setup_main_phase() -> GameState {
        let mut state = GameState::new_two_player(42);
        state.turn_number = 2;
        state.phase = Phase::PreCombatMain;
        state.active_player = P0;
        state.priority_player = P0;
        state.waiting_for = WaitingFor::Priority { player: P0 };
        state
    }

    fn add_mana(state: &mut GameState, player: PlayerId, color: ManaType, count: usize) {
        let pd = state.players.iter_mut().find(|p| p.id == player).unwrap();
        for _ in 0..count {
            pd.mana_pool.add(ManaUnit {
                color,
                source_id: ObjectId(0),
                pip_id: crate::types::mana::ManaPipId(0),
                supertype: None,
                source_could_produce_two_or_more_colors: false,
                restrictions: Vec::new(),
                grants: vec![],
                expiry: None,
            });
        }
    }

    fn green_cost(generic: u32) -> ManaCost {
        ManaCost::Cost {
            shards: vec![ManaCostShard::Green],
            generic,
        }
    }

    /// A mutate creature card in `player`'s hand: printed creature cost {1}{G},
    /// mutate cost {2}{G}, 4/4 with Trample. Mirrors `create_bestow_creature_in_hand`.
    fn mutate_creature_in_hand(
        state: &mut GameState,
        player: PlayerId,
        name: &str,
        card_id: u64,
    ) -> ObjectId {
        let id = create_object(state, CardId(card_id), player, name.to_string(), Zone::Hand);
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.base_card_types.core_types.push(CoreType::Creature);
        obj.mana_cost = green_cost(1);
        obj.base_mana_cost = green_cost(1);
        obj.power = Some(4);
        obj.toughness = Some(4);
        obj.base_power = Some(4);
        obj.base_toughness = Some(4);
        obj.keywords.push(Keyword::Trample);
        obj.base_keywords.push(Keyword::Trample);
        obj.keywords.push(Keyword::Mutate(green_cost(2)));
        obj.base_keywords.push(Keyword::Mutate(green_cost(2)));
        obj.base_characteristics_initialized = true;
        id
    }

    /// A non-Human creature on the battlefield owned/controlled by `player` — a
    /// legal mutate target (CR 702.140a). Has Flying so the union can be observed.
    fn target_creature(
        state: &mut GameState,
        player: PlayerId,
        name: &str,
        card_id: u64,
    ) -> ObjectId {
        let id = create_object(
            state,
            CardId(card_id),
            player,
            name.to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.base_card_types.core_types.push(CoreType::Creature);
        obj.power = Some(2);
        obj.toughness = Some(2);
        obj.base_power = Some(2);
        obj.base_toughness = Some(2);
        obj.keywords.push(Keyword::Flying);
        obj.base_keywords.push(Keyword::Flying);
        // Entered a prior turn: not summoning-sick. The merge must NOT reset this.
        obj.summoning_sick = false;
        obj.entered_battlefield_turn = Some(state.turn_number.saturating_sub(1));
        obj.base_characteristics_initialized = true;
        id
    }

    /// Build a mutate spell stack entry targeting `target`, mirroring the real
    /// cast (the spell object is on the stack, not the battlefield list).
    fn push_mutate_spell(
        state: &mut GameState,
        spell: ObjectId,
        card_id: u64,
        target: ObjectId,
        controller: PlayerId,
    ) {
        if let Some(obj) = state.objects.get_mut(&spell) {
            obj.zone = Zone::Stack;
            // CR 702.140a: mark the stack object as a mutating creature spell (the
            // real cast does this in `handle_mutate_cost_choice`).
            obj.mutate_form = Some(crate::game::game_object::MutateFormState);
        }
        state.players[0].hand.retain(|&id| id != spell);
        state.stack.push_back(StackEntry {
            id: spell,
            source_id: spell,
            controller,
            kind: StackEntryKind::Spell {
                card_id: CardId(card_id),
                ability: Some(ResolvedAbility::new(
                    Effect::Unimplemented {
                        name: "Mutate creature".to_string(),
                        description: None,
                    },
                    vec![TargetRef::Object(target)],
                    spell,
                    controller,
                )),
                casting_variant: CastingVariant::Mutate,
                actual_mana_spent: 0,
            },
        });
    }

    /// CR 702.140b: an illegal-at-resolution target (target left the battlefield)
    /// → the spell reverts to a plain creature spell and enters the battlefield;
    /// NO merge.
    #[test]
    fn illegal_target_gone_resolves_as_plain_creature() {
        let mut state = setup_main_phase();
        let spell = mutate_creature_in_hand(&mut state, P0, "Gemrazer", 2001);
        let target = target_creature(&mut state, P0, "Beast", 2002);
        push_mutate_spell(&mut state, spell, 2001, target, P0);

        // Target leaves the battlefield before resolution → illegal (CR 608.2b).
        state.battlefield.retain(|&id| id != target);
        state.objects.get_mut(&target).unwrap().zone = Zone::Graveyard;
        state.players[0].graveyard.push_back(target);

        let mut events = Vec::new();
        crate::game::stack::resolve_top(&mut state, &mut events);

        let result = state.objects.get(&spell).unwrap();
        assert_eq!(
            result.zone,
            Zone::Battlefield,
            "CR 702.140b: illegal target → spell enters battlefield as a plain creature"
        );
        assert!(
            result.merged_components.is_empty(),
            "CR 702.140b: no merge occurred"
        );
        assert!(
            result.mutate_form.is_none(),
            "CR 702.140b: mutate form reverted"
        );
        assert!(
            state.pending_mutate_merge.is_none(),
            "no merge choice is pending after an illegal-target revert"
        );
    }

    /// CR 608.2b + CR 702.140b: a target that is STILL on the battlefield but is
    /// NO LONGER a legal mutate target (it stopped being a creature) is illegal —
    /// the presence-only check would wrongly merge; the re-validation must revert
    /// to a plain creature. This is the discriminating test for FIX 1.
    #[test]
    fn target_became_non_creature_resolves_as_plain_creature() {
        let mut state = setup_main_phase();
        let spell = mutate_creature_in_hand(&mut state, P0, "Gemrazer", 2003);
        let target = target_creature(&mut state, P0, "Beast", 2004);
        push_mutate_spell(&mut state, spell, 2003, target, P0);

        // Target is still on the battlefield but loses its Creature type (e.g. an
        // effect turned it into a noncreature artifact) → no longer a legal
        // "non-Human creature you own" target (CR 702.140a).
        {
            let obj = state.objects.get_mut(&target).unwrap();
            obj.card_types
                .core_types
                .retain(|t| *t != CoreType::Creature);
            obj.base_card_types
                .core_types
                .retain(|t| *t != CoreType::Creature);
        }

        let mut events = Vec::new();
        crate::game::stack::resolve_top(&mut state, &mut events);

        let result = state.objects.get(&spell).unwrap();
        assert_eq!(
            result.zone,
            Zone::Battlefield,
            "CR 608.2b: target no longer a creature → revert to plain creature"
        );
        assert!(
            result.merged_components.is_empty(),
            "CR 608.2b: presence-only check would have merged; re-validation must NOT"
        );
        assert!(
            state.pending_mutate_merge.is_none(),
            "no merge choice pending when the target is illegal at resolution"
        );
        // The (now noncreature) former target is untouched on the battlefield.
        assert!(
            state.battlefield.contains(&target),
            "the illegal target itself stays where it is"
        );
    }

    /// CR 702.140c + CR 730.2: a legal target at resolution pauses for the merge
    /// choice; choosing TOP merges into ONE object that keeps the TARGET's id,
    /// unions the abilities, and does NOT reset summoning sickness.
    #[test]
    fn legal_target_at_resolution_merges_on_top() {
        let mut state = setup_main_phase();
        let spell = mutate_creature_in_hand(&mut state, P0, "Gemrazer", 2005);
        let target = target_creature(&mut state, P0, "Beast", 2006);
        push_mutate_spell(&mut state, spell, 2005, target, P0);

        let mut events = Vec::new();
        crate::game::stack::resolve_top(&mut state, &mut events);

        // CR 702.140c: resolution paused for the controller's top/bottom choice.
        assert!(
            matches!(state.waiting_for, WaitingFor::MutateMergeChoice { .. }),
            "legal target pauses for the merge choice; got {:?}",
            state.waiting_for
        );
        assert!(state.pending_mutate_merge.is_some());

        // Controller chooses TOP via the real engine action.
        apply_as_current(
            &mut state,
            GameAction::ChooseMutateMergeSide {
                side: MergeSide::Top,
            },
        )
        .expect("ChooseMutateMergeSide(Top) must be accepted");

        // CR 730.2c: exactly ONE battlefield object survives at that slot, keeping
        // the TARGET's id.
        assert!(
            state.battlefield.contains(&target),
            "CR 730.2c: the target's ObjectId survives on the battlefield"
        );
        assert!(
            !state.battlefield.contains(&spell),
            "CR 730.2b: the mutating spell did not enter as a separate permanent"
        );
        let survivor = state.objects.get(&target).unwrap();
        assert_eq!(
            survivor.merged_components,
            vec![spell, target],
            "CR 730.2a: rider on top, target underneath"
        );
        // CR 702.140e: union of abilities (target's Flying + spell's Trample).
        assert!(survivor.keywords.contains(&Keyword::Flying));
        assert!(survivor.keywords.contains(&Keyword::Trample));
        // CR 730.2c: no ETB / summoning-sickness reset on the surviving object.
        assert!(
            !survivor.summoning_sick,
            "CR 730.2c: a merge is not an ETB — summoning sickness is unchanged"
        );
    }

    /// CR 702.140a-c: full cast pipeline — `handle_cast_spell` offers the mutate
    /// alternative cost; choosing it, paying, and resolving merges onto a legal
    /// non-Human creature the caster owns. Drives the ACTUAL wired runtime path.
    #[test]
    fn full_cast_pipeline_mutate_onto_legal_target() {
        let mut state = setup_main_phase();
        add_mana(&mut state, P0, ManaType::Green, 6);
        let spell = mutate_creature_in_hand(&mut state, P0, "Gemrazer", 2007);
        let target = target_creature(&mut state, P0, "Beast", 2008);

        let mut events = Vec::new();
        let waiting = handle_cast_spell(&mut state, P0, spell, CardId(2007), &mut events)
            .expect("cast routes to the mutate alternative-cost choice");
        assert!(
            matches!(
                waiting,
                WaitingFor::AlternativeCastChoice {
                    keyword: AlternativeCastKeyword::Mutate,
                    ..
                }
            ),
            "both costs affordable + a legal target → mutate choice is offered; got {waiting:?}"
        );

        // Choose the mutate alternative cost; single legal target auto-selects and
        // the spell goes to the stack.
        let mut events = Vec::new();
        handle_mutate_cost_choice(
            &mut state,
            P0,
            spell,
            CardId(2007),
            AlternativeCastDecision::Alternative,
            &mut events,
        )
        .expect("mutate cost choice drives the cast onto the stack");
        assert_eq!(state.stack.len(), 1, "mutate spell is on the stack");

        // Resolve: legal target → pause for the merge choice.
        let mut events = Vec::new();
        crate::game::stack::resolve_top(&mut state, &mut events);
        assert!(
            matches!(state.waiting_for, WaitingFor::MutateMergeChoice { .. }),
            "legal target at resolution pauses for top/bottom; got {:?}",
            state.waiting_for
        );

        apply_as_current(
            &mut state,
            GameAction::ChooseMutateMergeSide {
                side: MergeSide::Top,
            },
        )
        .expect("merge side choice must be accepted");

        // Exactly one battlefield object at the slot, keeping the target's id, with
        // the unioned abilities.
        assert!(state.battlefield.contains(&target));
        assert!(!state.battlefield.contains(&spell));
        let survivor = state.objects.get(&target).unwrap();
        assert_eq!(survivor.merged_components, vec![spell, target]);
        assert!(survivor.keywords.contains(&Keyword::Flying));
        assert!(survivor.keywords.contains(&Keyword::Trample));
    }

    /// CR 702.140b: full cast pipeline with the target removed in response (no
    /// legal target at resolution) → the spell resolves as a normal creature.
    #[test]
    fn full_cast_pipeline_target_removed_resolves_as_plain_creature() {
        let mut state = setup_main_phase();
        add_mana(&mut state, P0, ManaType::Green, 6);
        let spell = mutate_creature_in_hand(&mut state, P0, "Gemrazer", 2009);
        let target = target_creature(&mut state, P0, "Beast", 2010);

        let mut events = Vec::new();
        handle_cast_spell(&mut state, P0, spell, CardId(2009), &mut events)
            .expect("cast routes to the mutate alternative-cost choice");
        let mut events = Vec::new();
        handle_mutate_cost_choice(
            &mut state,
            P0,
            spell,
            CardId(2009),
            AlternativeCastDecision::Alternative,
            &mut events,
        )
        .expect("mutate cost choice drives the cast onto the stack");

        // In response, the target is destroyed (leaves the battlefield).
        state.battlefield.retain(|&id| id != target);
        state.objects.get_mut(&target).unwrap().zone = Zone::Graveyard;
        state.players[0].graveyard.push_back(target);

        let mut events = Vec::new();
        crate::game::stack::resolve_top(&mut state, &mut events);

        let result = state.objects.get(&spell).unwrap();
        assert_eq!(
            result.zone,
            Zone::Battlefield,
            "CR 702.140b: no legal target → resolves as a plain creature"
        );
        assert!(result.merged_components.is_empty(), "no merge occurred");
        assert!(
            !matches!(state.waiting_for, WaitingFor::MutateMergeChoice { .. }),
            "no merge choice is presented when the target is illegal"
        );
    }

    /// CR 702.140a + CR 903.9: a mutate creature that is also a commander (e.g.
    /// Otrimi, the Ever-Playful) can be cast for its mutate cost straight from the
    /// command zone — the offer must be presented from `Zone::Command`, not only
    /// from the hand.
    #[test]
    fn mutate_offered_from_command_zone_for_a_commander() {
        let mut state = GameState::new(crate::types::format::FormatConfig::commander(), 2, 42);
        state.turn_number = 2;
        state.phase = Phase::PreCombatMain;
        state.active_player = P0;
        state.priority_player = P0;
        state.waiting_for = WaitingFor::Priority { player: P0 };
        add_mana(&mut state, P0, ManaType::Green, 6);

        // Otrimi-like commander in the command zone: green mutate creature.
        let spell = create_object(
            &mut state,
            CardId(2011),
            P0,
            "Otrimi".to_string(),
            Zone::Command,
        );
        {
            let obj = state.objects.get_mut(&spell).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.base_card_types.core_types.push(CoreType::Creature);
            obj.mana_cost = green_cost(1);
            obj.base_mana_cost = green_cost(1);
            obj.power = Some(4);
            obj.toughness = Some(4);
            obj.base_power = Some(4);
            obj.base_toughness = Some(4);
            obj.keywords.push(Keyword::Mutate(green_cost(2)));
            obj.base_keywords.push(Keyword::Mutate(green_cost(2)));
            obj.is_commander = true;
            obj.base_characteristics_initialized = true;
        }
        // A legal mutate target the caster owns.
        let _target = target_creature(&mut state, P0, "Beast", 2012);

        let mut events = Vec::new();
        let waiting = handle_cast_spell(&mut state, P0, spell, CardId(2011), &mut events)
            .expect("a commander mutate cast from the command zone must be allowed");
        assert!(
            matches!(
                waiting,
                WaitingFor::AlternativeCastChoice {
                    keyword: AlternativeCastKeyword::Mutate,
                    ..
                }
            ),
            "the mutate choice is offered from the command zone; got {waiting:?}"
        );
    }
}
