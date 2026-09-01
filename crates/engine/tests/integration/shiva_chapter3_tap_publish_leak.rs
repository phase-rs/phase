//! Regression test for the 2026-09-01 live-game bug report — Shiva, Warden of Ice
//! (back face of Jill, Shiva's Dominant) chapter III:
//!
//! `III — Cold Snap — Tap all lands your opponents control. Exile Shiva, then
//! return it to the battlefield (front face up).`
//!
//! The reported bug: after the chapter resolved, the human was prompted
//! "Choose 1 card to put onto the battlefield" with TWO candidates — the exiled
//! Saga AND an opponent's land that was already on the battlefield.
//!
//! Mechanism (verified against the authoritative game-state dump):
//! 1. Seven of the opponent's eight lands were already tapped, so the tap leg
//!    transitioned only the eighth — exactly one `PermanentTapped` event.
//! 2. The chapter's return leg reads `TrackedSet(0)` (the "it" anaphor), so the
//!    transitive `next_sub_needs_tracked_set` walk makes the tap leg publish
//!    its affected set (`Effect::SetTapState` harvest arm in
//!    `affected_objects_from_events`) into the chain tracked set.
//! 3. The exile leg extends the same chain set with the exiled Saga
//!    (chain unification in `publish_tracked_set`).
//! 4. The return leg's `TrackedSetId(0)` sentinel resolves to the merged set;
//!    `scan_zones` derives from the members' zones (Battlefield + Exile) and
//!    the opponent's land becomes an eligible "put onto the battlefield"
//!    candidate.
//!
//! CR 608.2c: the chain tracked set feeds "this way" anaphors; the chapter's
//! "return it" names the card the exile leg exiled — the tap leg's population
//! is not part of that antecedent.

use engine::game::ability_utils::build_resolved_from_def;
use engine::game::effects::resolve_ability_chain;
use engine::game::game_object::BackFaceData;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::zones::create_object;
use engine::parser::oracle_effect::parse_effect_chain;
use engine::types::ability::{
    AbilityKind, CastingPermission, Effect, PermissionGrantee, ResolvedAbility, TargetFilter,
    ThisWayCause,
};
use engine::types::card_type::{CardType, CoreType};
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::{CardId, ObjectId, TrackedSetId};
use engine::types::mana::{ManaColor, ManaCost};
use engine::types::player::PlayerId;
use engine::types::zones::{EtbTapState, Zone};

/// Verbatim chapter body from the card's Oracle text (card-data.json,
/// "shiva, warden of ice"), with the printed card-name reference "Exile Shiva"
/// written as "Exile ~" — the same self-reference normalization the saga
/// parser applies for the card's own name (the live game's parse lowers the
/// exile leg to `SelfRef`).
const COLD_SNAP: &str =
    "Tap all lands your opponents control. Exile ~, then return it to the battlefield (front face up).";

/// Jill, Shiva's Dominant — the front face the saga returns to.
fn jill_back_face() -> BackFaceData {
    BackFaceData {
        is_swap_snapshot: false,
        name: "Jill, Shiva's Dominant".to_string(),
        power: Some(2),
        toughness: Some(3),
        loyalty: None,
        printed_loyalty: None,
        defense: None,
        card_types: CardType {
            supertypes: vec![],
            core_types: vec![CoreType::Creature],
            subtypes: vec![
                "Human".to_string(),
                "Noble".to_string(),
                "Warrior".to_string(),
            ],
        },
        mana_cost: ManaCost::default(),
        keywords: vec![],
        abilities: vec![],
        trigger_definitions: Default::default(),
        replacement_definitions: Default::default(),
        static_definitions: Default::default(),
        color: vec![ManaColor::Blue],
        printed_ref: None,
        modal: None,
        additional_cost: None,
        strive_cost: None,
        casting_restrictions: vec![],
        casting_options: vec![],
        layout_kind: None,
        parse_warnings: vec![],
    }
}

/// Build the game: P1 controls eight lands, seven already tapped (its mana was
/// spent on the previous turn) and one untapped — the exact board shape from
fn build_scenario_with_polluted_board() -> (GameScenario, Vec<ObjectId>, ObjectId) {
    let mut scenario = GameScenario::new();
    let mut tapped_lands = Vec::new();
    for _ in 0..7 {
        let land = scenario.add_basic_land(P1, ManaColor::Blue);
        tapped_lands.push(land);
    }
    let untapped_land = scenario.add_basic_land(P1, ManaColor::Blue);
    (scenario, tapped_lands, untapped_land)
}

/// Place the Shiva-faced Saga on P0's battlefield after the runner exists
/// (`GameScenario.state` is private outside the crate).
fn stage_shiva(runner: &mut GameRunner) -> ObjectId {
    let state = runner.state_mut();
    let id = create_object(
        state,
        CardId(500),
        P0,
        "Shiva, Warden of Ice".to_string(),
        Zone::Battlefield,
    );
    let obj = state.objects.get_mut(&id).unwrap();
    obj.card_types.core_types.push(CoreType::Enchantment);
    obj.card_types.subtypes.push("Saga".to_string());
    obj.base_card_types = obj.card_types.clone();
    // CR 712.9 + CR 712.18: the lore-counter trigger has TRANSFORMED the saga to its back
    // face — the chapter resolves while Shiva is showing, exactly as in the
    // live dump. Without this flag the zone-exit front-face revert never fires
    // and the fixture diverges from the live game (whose exiled object 23 was
    // already "Jill, Shiva's Dominant").
    obj.transformed = true;
    obj.counters.insert(CounterType::Lore, 3);
    obj.back_face = Some(jill_back_face());
    id
}

#[test]
fn shiva_chapter_iii_tap_leg_must_not_pollute_return_candidates() {
    let execute = parse_effect_chain(COLD_SNAP, AbilityKind::Spell);
    let (scenario, tapped_lands, untapped_land) = build_scenario_with_polluted_board();
    let mut runner = scenario.build();
    for land in &tapped_lands {
        runner.state_mut().objects.get_mut(land).unwrap().tapped = true;
    }
    let shiva_id = stage_shiva(&mut runner);
    let resolved = build_resolved_from_def(&execute, shiva_id, P0);
    let mut events = Vec::new();
    resolve_ability_chain(runner.state_mut(), &resolved, &mut events, 0)
        .expect("chapter III resolves");

    let state = runner.state();
    let waiting = &state.waiting_for;
    assert!(
        !matches!(waiting, WaitingFor::EffectZoneChoice { .. }),
        "chapter III's return leg has a single eligible card (the exiled Saga); \
         the opponent's already-battlefield land must not join the candidate pool \
         (CR 608.2c): {waiting:?}"
    );
    assert_eq!(
        state.objects[&shiva_id].zone,
        Zone::Battlefield,
        "the Saga must return to the battlefield automatically (single candidate)"
    );
    assert_eq!(
        state.objects[&shiva_id].name, "Jill, Shiva's Dominant",
        "the returned permanent must show its front face (CR 712.14)"
    );
    assert_eq!(
        state.objects[&untapped_land].zone,
        Zone::Battlefield,
        "the opponent's land must never be a return candidate"
    );
    // Reach-guard (paired with the no-pause negative above): the tap leg
    // really ran — exactly one untapped→tapped transition (the eighth land),
    // and all eight P1 lands end tapped. Proves the fix narrowed the publish
    // gate, not the leg itself.
    assert_eq!(
        tapped_object_ids(&events),
        vec![untapped_land],
        "exactly one PermanentTapped event (the single untapped→tapped transition) \
         must prove the tap leg executed"
    );
    assert!(
        tapped_lands
            .iter()
            .chain(std::iter::once(&untapped_land))
            .all(|land| state.objects[land].tapped),
        "all eight P1 lands must be tapped after chapter III"
    );
    // Reach-guard (second): the exile leg really ran — the Saga reached Exile
    // before the return leg auto-resolved it back (CR 608.2c).
    assert!(
        zone_changed_to(&events, shiva_id, Zone::Exile),
        "the exile leg must emit ZoneChanged → Exile for the Saga"
    );
}
/// The tap + exile producer legs with the tracked-set-consuming return leg
/// REMOVED — used by the discriminating fixtures that attach their own
/// consumer leg. Producers stay parsed from real Oracle text; only the
/// consumer is synthesized (hand-built legs are the established precedent:
/// `compound_zone_change_chain_unifies_tracked_set` in the engine lib tests).
const TAP_AND_EXILE: &str = "Tap all lands your opponents control. Exile ~.";

/// Two same-class in-place producers, for the tap→tap→grant union fixture.
const TAP_LANDS_AND_CREATURES: &str =
    "Tap all lands your opponents control. Tap all creatures you control.";

/// A single tap leg with a TERMINAL tracked-set consumer, for the
/// terminal-vs-nested-consumer fixtures.
const TAP_LANDS: &str = "Tap all lands your opponents control.";

/// Append `leg` to the tail of `ability`'s sub_ability chain. The integration
/// crate cannot call the crate-private `append_to_sub_chain`; the walk mirrors
/// it exactly.
fn append_leg(ability: &mut ResolvedAbility, leg: ResolvedAbility) {
    let mut node = ability;
    while node.sub_ability.is_some() {
        node = node.sub_ability.as_mut().unwrap().as_mut();
    }
    node.sub_ability = Some(Box::new(leg));
}

/// A NON-consuming tracked-set reader: `GrantCastingPermission` with the
/// `TrackedSetId(0)` sentinel resolves through `grant_permission::resolve`'s
/// highest-set-id ladder WITHOUT removing the set, so the chain's tracked set
/// stays inspectable after `resolve_ability_chain` returns. This is the only
/// post-resolve set-content read that is not vacuous — the `ChangeZone`
/// consumer removes the set and its member-causes in lockstep when it scans.
fn grant_tracked_set_leg(source_id: ObjectId, controller: PlayerId) -> ResolvedAbility {
    ResolvedAbility::new(
        Effect::GrantCastingPermission {
            permission: CastingPermission::WarpExile {
                castable_after_turn: 1,
            },
            target: TargetFilter::TrackedSet {
                id: TrackedSetId(0),
            },
            grantee: PermissionGrantee::AbilityController,
        },
        vec![],
        source_id,
        controller,
    )
}

/// Board with `count` P1 basic lands, all untapped (hostile fixture (i): the
/// tap leg transitions `count` objects — pollution magnitude changes, the
/// behavior must not).
fn build_scenario_with_untapped_lands(count: usize) -> (GameScenario, Vec<ObjectId>) {
    let mut scenario = GameScenario::new();
    let lands = (0..count)
        .map(|_| scenario.add_basic_land(P1, ManaColor::Blue))
        .collect();
    (scenario, lands)
}

/// Objects that received a `PermanentTapped` event, in event order — the
/// transition-only tap population.
fn tapped_object_ids(events: &[GameEvent]) -> Vec<ObjectId> {
    events
        .iter()
        .filter_map(|event| match event {
            GameEvent::PermanentTapped { object_id, .. } => Some(*object_id),
            _ => None,
        })
        .collect()
}

fn zone_changed_to(events: &[GameEvent], object_id: ObjectId, to: Zone) -> bool {
    events.iter().any(|event| {
        matches!(
            event,
            GameEvent::ZoneChanged {
                object_id: id,
                to: dest,
                ..
            } if *id == object_id && *dest == to
        )
    })
}

/// Any zone change at all for `object_id` — used to prove a non-member never
/// entered a mass-move's eligible pool.
fn zone_changed_at_all(events: &[GameEvent], object_id: ObjectId) -> bool {
    events.iter().any(
        |event| matches!(event, GameEvent::ZoneChanged { object_id: id, .. } if *id == object_id),
    )
}

/// Discriminator A (CR 608.2c nearest-antecedent, set-content): the tap leg is
/// superseded by the exile leg in publisher position, so the chain tracked set
/// must contain ONLY the exiled Saga — never a tapped land. The non-consuming
/// `GrantCastingPermission` reader keeps the set inspectable after resolution
/// (unlike the `ChangeZone` consumer, which drains it on scan).
///
/// Pre-fix: the tap leg publishes the tapped land into the chain set
/// (transitive `next_sub_needs_tracked_set` walk) and the exile leg extends the
/// same set (chain unification) → the bound set is [land, Saga] and this
/// assertion fails. Post-fix: the veto declines the tap publish → [Saga] only.
#[test]
fn grant_reader_binds_only_the_exiled_saga_not_tapped_lands() {
    let execute = parse_effect_chain(TAP_AND_EXILE, AbilityKind::Spell);
    let (scenario, tapped_lands, untapped_land) = build_scenario_with_polluted_board();
    let mut runner = scenario.build();
    for land in &tapped_lands {
        runner.state_mut().objects.get_mut(land).unwrap().tapped = true;
    }
    let shiva_id = stage_shiva(&mut runner);
    let mut resolved = build_resolved_from_def(&execute, shiva_id, P0);
    append_leg(&mut resolved, grant_tracked_set_leg(shiva_id, P0));
    let mut events = Vec::new();
    resolve_ability_chain(runner.state_mut(), &resolved, &mut events, 0)
        .expect("tap → exile → grant chain resolves");

    let state = runner.state();
    // Reach-guards: the tap leg ran (exactly one untapped→tapped transition)
    // and the exile leg ran (Saga reached Exile; the grant reader proves it
    // bound the Saga).
    assert_eq!(
        tapped_object_ids(&events),
        vec![untapped_land],
        "the tap leg must run (one untapped→tapped transition)"
    );
    assert_eq!(
        state.objects[&shiva_id].zone,
        Zone::Exile,
        "the exile leg must run (Saga sits in Exile; no return leg in this fixture)"
    );
    assert_eq!(
        state.objects[&shiva_id].casting_permissions.len(),
        1,
        "the grant reader must bind the Saga — proves the consumer executed"
    );

    // Discriminating assertion, targeted at the CHAIN's set id (the grant
    // ladder's `max_by_key` spans the append-only map; never a whole-map sweep).
    let set_id = state
        .chain_tracked_set_id
        .expect("the exile leg must publish the chain tracked set the reader binds");
    let bound = &state.tracked_object_sets[&set_id];
    assert_eq!(
        bound.as_slice(),
        &[shiva_id],
        "the chain tracked set must contain ONLY the exiled Saga — the tap leg's \
         population is not the antecedent of the tracked-set reader (CR 608.2c): {bound:?}"
    );
    // Cause-stamp coherence: the exile leg stamps `Exiled`; no land carries any
    // member-cause stamp.
    let causes = &state.tracked_set_member_causes[&set_id];
    assert_eq!(
        causes.get(&shiva_id),
        Some(&ThisWayCause::Exiled),
        "the Saga must carry the Exiled cause stamp"
    );
    assert!(
        causes.keys().all(|id| *id == shiva_id),
        "no tapped land may carry a member-cause stamp: {causes:?}"
    );
}

/// Discriminator B (CR 608.2c, mass-move consumer): the same producer shape
/// consumed by a hand-built `ChangeZoneAll { target: TrackedSet(0) }` — the
/// brief's second sentinel-reading path (change_zone.rs resolves the sentinel
/// for the mass path too).
///
/// The destination is Graveyard, NOT the plan's literal `Battlefield`: with
/// destination == the polluted members' current zone, a same-zone mass move is
/// a fully silent no-op (no event, no count inflation, no movement), so the
/// plan's literal shape cannot fail pre-fix and does not discriminate. Moving
/// to a zone OUTSIDE the polluted population makes the corruption observable
/// while exercising the identical sentinel path. Exactly the Saga is moved;
/// no land is eligible (pre-fix the polluted set puts the battlefield lands
/// into the member scan and moves every one of them to the Graveyard).
#[test]
fn change_zone_all_mass_move_moves_only_the_exiled_saga() {
    let execute = parse_effect_chain(TAP_AND_EXILE, AbilityKind::Spell);
    let (scenario, tapped_lands, untapped_land) = build_scenario_with_polluted_board();
    let mut runner = scenario.build();
    for land in &tapped_lands {
        runner.state_mut().objects.get_mut(land).unwrap().tapped = true;
    }
    let shiva_id = stage_shiva(&mut runner);
    let all_lands: Vec<ObjectId> = tapped_lands
        .iter()
        .chain(std::iter::once(&untapped_land))
        .copied()
        .collect();
    let mut resolved = build_resolved_from_def(&execute, shiva_id, P0);
    append_leg(
        &mut resolved,
        ResolvedAbility::new(
            Effect::ChangeZoneAll {
                origin: None,
                destination: Zone::Graveyard,
                target: TargetFilter::TrackedSet {
                    id: TrackedSetId(0),
                },
                enters_under: None,
                enter_tapped: EtbTapState::Unspecified,
                enters_attacking: false,
                enter_with_counters: vec![],
                face_down_profile: None,
                library_position: None,
                random_order: false,
            },
            vec![],
            shiva_id,
            P0,
        ),
    );
    let mut events = Vec::new();
    resolve_ability_chain(runner.state_mut(), &resolved, &mut events, 0)
        .expect("tap → exile → mass-move chain resolves");

    let state = runner.state();
    // Reach-guards: tap leg ran; exile leg ran (Saga reached Exile before the
    // mass move took it on to the Graveyard).
    assert_eq!(
        tapped_object_ids(&events),
        vec![untapped_land],
        "the tap leg must run (one untapped→tapped transition)"
    );
    assert!(
        zone_changed_to(&events, shiva_id, Zone::Exile),
        "the exile leg must emit ZoneChanged → Exile for the Saga"
    );
    // Discriminating assertion: exactly the Saga is moved.
    assert_eq!(
        state.objects[&shiva_id].zone,
        Zone::Graveyard,
        "the mass move must move the exiled Saga (the tracked set's only member)"
    );
    for land in &all_lands {
        assert!(
            !zone_changed_at_all(&events, *land),
            "a tapped land must never enter the ChangeZoneAll member scan (CR 608.2c)"
        );
        assert_eq!(
            state.objects[land].zone,
            Zone::Battlefield,
            "the opponent's land must never be moved by the mass-move consumer"
        );
    }
    // Second discriminator: the mass path must move exactly ONE object. Pre-fix
    // the polluted set puts the battlefield lands into the member scan pool, so
    // the move count inflates.
    assert_eq!(
        state.last_effect_count,
        Some(1),
        "the ChangeZoneAll mass move must move exactly the exiled Saga (CR 608.2c), \
         got count {count:?}",
        count = state.last_effect_count
    );
}

/// Negative control: `SetTapState → ChangeZone{Exile}` with NO tracked-set
/// consumer publishes nothing. Pre-fix this passes (the publish gate only
/// fires when a descendant references TrackedSet); post-fix it must STAY green
/// — it catches an accidental unconditional veto that would wedge every
/// producer publish.
#[test]
fn tap_exile_chain_with_no_consumer_publishes_no_tracked_set() {
    let execute = parse_effect_chain(TAP_AND_EXILE, AbilityKind::Spell);
    let (scenario, tapped_lands, untapped_land) = build_scenario_with_polluted_board();
    let mut runner = scenario.build();
    for land in &tapped_lands {
        runner.state_mut().objects.get_mut(land).unwrap().tapped = true;
    }
    let shiva_id = stage_shiva(&mut runner);
    let mut events = Vec::new();
    let resolved = build_resolved_from_def(&execute, shiva_id, P0);
    resolve_ability_chain(runner.state_mut(), &resolved, &mut events, 0)
        .expect("producer-only chain resolves");
    let state = runner.state();
    // Reach-guard: the chain really ran — the Saga was exiled and the tap
    // transitioned the eighth land.
    assert_eq!(
        state.objects[&shiva_id].zone,
        Zone::Exile,
        "the exile leg must run in the producer-only chain"
    );
    assert_eq!(
        tapped_object_ids(&events),
        vec![untapped_land],
        "the tap leg must run in the producer-only chain"
    );
    assert!(
        !state.tracked_object_sets.values().any(|set| set
            .iter()
            .any(|id| *id == shiva_id || tapped_lands.contains(id) || *id == untapped_land)),
        "no consumer → no publish: no tracked set may contain the Saga or any land"
    );
}

/// Hostile fixture (i): ALL eight lands untapped. The tap leg now transitions
/// eight objects (pollution magnitude changes) — the behavior must not.
#[test]
fn all_untapped_board_does_not_pollute_return_candidates() {
    let execute = parse_effect_chain(COLD_SNAP, AbilityKind::Spell);
    let (scenario, lands) = build_scenario_with_untapped_lands(8);
    let mut runner = scenario.build();
    let shiva_id = stage_shiva(&mut runner);
    let mut events = Vec::new();
    let resolved = build_resolved_from_def(&execute, shiva_id, P0);
    resolve_ability_chain(runner.state_mut(), &resolved, &mut events, 0)
        .expect("chapter III resolves");
    let state = runner.state();
    assert!(
        !matches!(state.waiting_for, WaitingFor::EffectZoneChoice { .. }),
        "no return-candidate prompt on the all-untapped board: {:?}",
        state.waiting_for
    );
    assert_eq!(state.objects[&shiva_id].zone, Zone::Battlefield);
    assert_eq!(state.objects[&shiva_id].name, "Jill, Shiva's Dominant");
    // Reach-guard: the tap leg transitioned all eight lands.
    let tapped = tapped_object_ids(&events);
    assert_eq!(
        tapped.len(),
        8,
        "all eight lands transitioned untapped→tapped (larger pollution magnitude)"
    );
    for land in &lands {
        assert!(state.objects[land].tapped, "every land must be tapped");
        assert_eq!(
            state.objects[land].zone,
            Zone::Battlefield,
            "the land must never be a return candidate"
        );
    }
}

/// Hostile fixture (iii): two same-class in-place publishers chained
/// (`tap lands → tap creatures → grant{TrackedSet}`). Under the narrowed veto
/// (MEASURED via Kathril, see `transitive_publish_superseded`'s doc), a
/// same-class in-place sibling producer is NOT a superseding moving publisher,
/// so both tap legs keep publishing and the grant binds the UNION the chain
/// unification produces — the plan's original (iii) semantics, which also pins
/// that the narrowing did not reintroduce forward-only suppression for
/// same-verb chains (the Kathril #6321 class).
#[test]
fn chained_tap_publishers_grant_binds_the_unified_union() {
    let execute = parse_effect_chain(TAP_LANDS_AND_CREATURES, AbilityKind::Spell);
    let (mut scenario, tapped_lands, untapped_land) = build_scenario_with_polluted_board();
    let creature_b = scenario.add_vanilla(P0, 2, 2);
    let creature_a = scenario.add_vanilla(P0, 2, 2);
    let mut runner = scenario.build();
    for land in &tapped_lands {
        runner.state_mut().objects.get_mut(land).unwrap().tapped = true;
    }
    let shiva_id = stage_shiva(&mut runner);
    let mut resolved = build_resolved_from_def(&execute, shiva_id, P0);
    append_leg(&mut resolved, grant_tracked_set_leg(shiva_id, P0));
    let mut events = Vec::new();
    resolve_ability_chain(runner.state_mut(), &resolved, &mut events, 0)
        .expect("tap → tap → grant chain resolves");

    let state = runner.state();
    // Reach-guards: both tap legs ran (land transition + creature transitions).
    let tapped = tapped_object_ids(&events);
    assert!(
        tapped.contains(&untapped_land)
            && tapped.contains(&creature_a)
            && tapped.contains(&creature_b),
        "both tap legs must run: {tapped:?}"
    );
    assert!(state.objects[&creature_a].tapped && state.objects[&creature_b].tapped);
    // Union assertion: the grant binds BOTH producers' populations — same-class
    // in-place chains backward-merge through chain unification.
    for creature in [&creature_a, &creature_b] {
        assert_eq!(
            state.objects[creature].casting_permissions.len(),
            1,
            "the grant must bind the second tap leg's population"
        );
    }
    // The first tap leg's AFFECTED population is transition-only: the single
    // untapped→tapped land joins the union; the seven already-tapped lands were
    // not affected by the leg and are correctly absent.
    assert_eq!(
        state.objects[&untapped_land].casting_permissions.len(),
        1,
        "the grant must also bind the first tap leg's affected population (union, \
         not forward-only suppression)"
    );
    for land in &tapped_lands {
        assert!(
            state.objects[land].casting_permissions.is_empty(),
            "a land the tap leg did not transition is not part of any affected \
             population and must not receive the grant"
        );
    }
}

/// Hostile fixture (iv), terminal side: `tap → grant{TrackedSet}` — the grant
/// is a TERMINAL consumer (its subtree contains no further tracked-set
/// reference), so the tap leg is NOT in superseded publisher position and MUST
/// keep publishing. Pins the R2-1 terminal-consumer guarantee behaviorally.
#[test]
fn terminal_tracked_set_consumer_keeps_the_tap_publish() {
    let execute = parse_effect_chain(TAP_LANDS, AbilityKind::Spell);
    let (scenario, lands) = build_scenario_with_untapped_lands(8);
    let mut runner = scenario.build();
    let shiva_id = stage_shiva(&mut runner);
    let mut resolved = build_resolved_from_def(&execute, shiva_id, P0);
    append_leg(&mut resolved, grant_tracked_set_leg(shiva_id, P0));
    let mut events = Vec::new();
    resolve_ability_chain(runner.state_mut(), &resolved, &mut events, 0)
        .expect("tap → grant chain resolves");

    let state = runner.state();
    // Reach-guard: the tap leg transitioned all eight lands.
    assert_eq!(
        tapped_object_ids(&events).len(),
        8,
        "the tap leg must run and transition all eight lands"
    );
    // Positive assertion: a terminal consumer keeps the publish — every tapped
    // land receives the grant.
    for land in &lands {
        assert_eq!(
            state.objects[land].casting_permissions.len(),
            1,
            "a terminal tracked-set consumer keeps the tap leg's publish (CR 608.2c)"
        );
    }
}

/// Hostile fixture (iv), nested side: `tap → grant{TrackedSet} →
/// grant2{TrackedSet}` — the first consumer's own subtree references
/// TrackedSet, so it IS in publisher position (`node_or_later_is_publisher_position`'s
/// `node_or_later` disjunct) and the tap leg IS superseded: neither reader
/// binds the tap population, even though both wanted it (the Motivated Pony
/// loaded gun, carried verbatim into the veto's doc). Pins the R2-1
/// chain-wide scope behaviorally.
#[test]
fn nested_tracked_set_consumer_supersedes_the_tap_publish() {
    let execute = parse_effect_chain(TAP_LANDS, AbilityKind::Spell);
    let (scenario, lands) = build_scenario_with_untapped_lands(8);
    let mut runner = scenario.build();
    let shiva_id = stage_shiva(&mut runner);
    let mut resolved = build_resolved_from_def(&execute, shiva_id, P0);
    append_leg(&mut resolved, grant_tracked_set_leg(shiva_id, P0));
    append_leg(&mut resolved, grant_tracked_set_leg(shiva_id, P0));
    let mut events = Vec::new();
    resolve_ability_chain(runner.state_mut(), &resolved, &mut events, 0)
        .expect("tap → grant → grant chain resolves");

    let state = runner.state();
    // Reach-guard: the tap leg ran (all eight lands transitioned and are
    // tapped) — the veto was a publish-gate narrowing, not a dead tap leg.
    assert_eq!(
        tapped_object_ids(&events).len(),
        8,
        "the tap leg must run and transition all eight lands"
    );
    for land in &lands {
        assert!(state.objects[land].tapped);
        // Discriminating assertion: a nested consumer supersedes the publish —
        // the tap population reaches NEITHER reader.
        assert!(
            state.objects[land].casting_permissions.is_empty(),
            "with two chained tracked-set consumers the superseded tap leg's \
             population must reach neither reader (CR 608.2c chain-wide scope)"
        );
    }
}
