//! Runtime layer-7a coverage for counted-quantity CDA power/toughness
//! (Control Win Condition, Subgoyf).
//!
//! CR 604.3 + CR 613.4a: characteristic-defining abilities that define P/T are
//! applied in layer 7a and recomputed live on every layer pass. These tests
//! drive `evaluate_layers` directly (the production layer engine) and assert the
//! resolved `power`/`toughness` — a revert of the resolver, the parser tail, or
//! the Subgoyf `+1` toughness offset flips at least one assertion here.

use engine::game::layers::evaluate_layers;
use engine::game::zones::create_object;
use engine::types::ability::{
    CardTypeSetSource, ContinuousModification, CountScope, QuantityExpr, QuantityRef,
    StaticDefinition, SubtypeExclusion, TargetFilter, ZoneRef,
};
use engine::types::card_type::CoreType;
use engine::types::game_state::GameState;
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

/// Attach a CDA that sets power to `power_qty` and toughness to `toughness_qty`
/// to a fresh 0/0 creature on the battlefield (mirrors Subgoyf / Control Win
/// Condition's printed `*`/`*` base). Returns the creature id.
fn setup_cda_creature(
    state: &mut GameState,
    controller: PlayerId,
    power_qty: QuantityExpr,
    toughness_qty: QuantityExpr,
) -> ObjectId {
    let id = create_object(
        state,
        CardId(100),
        controller,
        "CDA Creature".to_string(),
        Zone::Battlefield,
    );
    if let Some(obj) = state.objects.get_mut(&id) {
        obj.card_types.core_types = vec![CoreType::Creature];
        obj.base_card_types = obj.card_types.clone();
        obj.power = Some(0);
        obj.toughness = Some(0);
        obj.base_power = Some(0);
        obj.base_toughness = Some(0);

        let def = StaticDefinition::continuous()
            .affected(TargetFilter::SelfRef)
            .cda()
            .modifications(vec![
                ContinuousModification::SetDynamicPower { value: power_qty },
                ContinuousModification::SetDynamicToughness {
                    value: toughness_qty,
                },
            ]);
        obj.static_definitions = vec![def.clone()].into();
        obj.base_static_definitions = std::sync::Arc::new(vec![def]);
    }
    id
}

/// Put a card with the given subtypes into `owner`'s graveyard.
fn add_graveyard_card(
    state: &mut GameState,
    owner: PlayerId,
    card_id: u64,
    subtypes: &[&str],
) -> ObjectId {
    let id = create_object(
        state,
        CardId(card_id),
        owner,
        format!("GY Card {card_id}"),
        Zone::Graveyard,
    );
    if let Some(obj) = state.objects.get_mut(&id) {
        obj.card_types.subtypes = subtypes.iter().map(|s| s.to_string()).collect();
        obj.base_card_types = obj.card_types.clone();
    }
    id
}

fn recompute(state: &mut GameState) {
    state.layers_dirty.mark_full();
    evaluate_layers(state);
}

// --- Control Win Condition: P/T == number of turns you've taken this game ---

#[test]
fn control_win_condition_tracks_controller_turns_live() {
    let mut state = GameState::new_two_player(42);
    let controller = PlayerId(0);
    let opponent = PlayerId(1);

    let turns_qty = QuantityExpr::Ref {
        qty: QuantityRef::TurnsTaken,
    };
    let creature = setup_cda_creature(&mut state, controller, turns_qty.clone(), turns_qty);

    // 0 turns taken -> 0/0.
    recompute(&mut state);
    assert_eq!(state.objects[&creature].power, Some(0));
    assert_eq!(state.objects[&creature].toughness, Some(0));

    // Controller has taken 3 turns -> 3/3.
    state.players[controller.0 as usize].turns_taken = 3;
    recompute(&mut state);
    assert_eq!(state.objects[&creature].power, Some(3));
    assert_eq!(state.objects[&creature].toughness, Some(3));

    // Opponent's turns must NOT count (per-player TurnsTaken, CR 500).
    state.players[opponent.0 as usize].turns_taken = 10;
    recompute(&mut state);
    assert_eq!(
        state.objects[&creature].power,
        Some(3),
        "opponent turns must not raise a controller-scoped TurnsTaken CDA"
    );

    // Taking another turn re-derives live (not a cast-time snapshot).
    state.players[controller.0 as usize].turns_taken = 4;
    recompute(&mut state);
    assert_eq!(
        state.objects[&creature].power,
        Some(4),
        "CDA must recompute TurnsTaken live on the next layer pass"
    );
    assert_eq!(state.objects[&creature].toughness, Some(4));
}

// --- Subgoyf: power == N distinct non-creature subtypes in all graveyards,
//     toughness == N + 1 (CR 205.3 + CR 208.2). ---

fn subgoyf_power_qty() -> QuantityExpr {
    QuantityExpr::Ref {
        qty: QuantityRef::DistinctSubtypes {
            source: CardTypeSetSource::Zone {
                zone: ZoneRef::Graveyard,
                scope: CountScope::All,
            },
            exclude: SubtypeExclusion::CreatureTypes,
        },
    }
}

fn subgoyf_toughness_qty() -> QuantityExpr {
    QuantityExpr::Offset {
        inner: Box::new(subgoyf_power_qty()),
        offset: 1,
    }
}

#[test]
fn subgoyf_counts_distinct_noncreature_subtypes_plus_one_toughness() {
    let mut state = GameState::new_two_player(7);
    let controller = PlayerId(0);
    let opponent = PlayerId(1);

    // Goblin and Wizard are creature types (excluded); Aura, Equipment, Arcane
    // are non-creature subtypes (counted).
    state.all_creature_types = vec!["Goblin".to_string(), "Wizard".to_string()];

    let creature = setup_cda_creature(
        &mut state,
        controller,
        subgoyf_power_qty(),
        subgoyf_toughness_qty(),
    );

    // Empty graveyards: 0 distinct non-creature subtypes -> 0/1.
    recompute(&mut state);
    assert_eq!(state.objects[&creature].power, Some(0));
    assert_eq!(
        state.objects[&creature].toughness,
        Some(1),
        "empty-graveyard Subgoyf is 0/1 (N+1 toughness offset), not 0/0"
    );

    // Seed both players' graveyards.
    //  - controller: [Aura] (1 non-creature) + [Goblin] (creature-only, excluded)
    //  - opponent:   [Equipment, Aura] (Aura duplicates controller's -> counted once)
    //  - opponent:   [Goblin, Wizard] (all creature types -> excluded)
    add_graveyard_card(&mut state, controller, 1, &["Aura"]);
    add_graveyard_card(&mut state, controller, 2, &["Goblin"]);
    add_graveyard_card(&mut state, opponent, 3, &["Equipment", "Aura"]);
    add_graveyard_card(&mut state, opponent, 4, &["Goblin", "Wizard"]);

    // Distinct non-creature subtypes across ALL graveyards: {Aura, Equipment} = 2.
    recompute(&mut state);
    assert_eq!(
        state.objects[&creature].power,
        Some(2),
        "distinct non-creature subtypes across all graveyards (Aura, Equipment); \
         duplicate Aura counts once, creature-only cards excluded"
    );
    assert_eq!(
        state.objects[&creature].toughness,
        Some(3),
        "toughness is N+1 = 3 (the +1 offset is a revert-failing discriminator)"
    );

    // Adding a card with a new non-creature subtype raises the live count.
    add_graveyard_card(&mut state, controller, 5, &["Arcane"]);
    recompute(&mut state);
    assert_eq!(
        state.objects[&creature].power,
        Some(3),
        "new distinct non-creature subtype (Arcane) recomputes live"
    );
    assert_eq!(state.objects[&creature].toughness, Some(4));
}

/// PRODUCTION-PATH regression: advancing a turn through the real `start_next_turn`
/// must invalidate the layer cache when `turns_taken` changes, so a `TurnsTaken`
/// CDA (Control Win Condition) re-derives its P/T WITHOUT any manual `mark_full`.
///
/// This is the counterpart to `control_win_condition_tracks_controller_turns_live`,
/// which forces a full recompute by hand and therefore proves only that the
/// resolver works once re-evaluation is forced — not that the turn-advance path
/// triggers it. `evaluate_layers` always recomputes when called, so the real-world
/// bug is a CLEAN cache the game loop never re-evaluates: `start_next_turn` must
/// mark layers dirty. Reverting the `layers_dirty.mark_full()` in `start_next_turn`
/// leaves the cache `Clean` here, so the `is_dirty` assertion (and the gated
/// recompute below it) fail.
#[test]
fn control_win_condition_invalidated_by_start_next_turn() {
    let mut state = GameState::new_two_player(42);
    let controller = PlayerId(0);

    let turns_qty = QuantityExpr::Ref {
        qty: QuantityRef::TurnsTaken,
    };
    let creature = setup_cda_creature(&mut state, controller, turns_qty.clone(), turns_qty);

    // Establish a CLEAN layer cache at a known P/T: P0 has taken 5 turns -> 5/5.
    state.players[controller.0 as usize].turns_taken = 5;
    state.layers_dirty.mark_full();
    evaluate_layers(&mut state);
    assert_eq!(state.objects[&creature].power, Some(5));
    assert!(
        !state.layers_dirty.is_dirty(),
        "precondition: the layer cache is clean before the turn advance"
    );

    // Advance to P0's next turn through the REAL turn-start path (P1 -> P0), which
    // increments P0.turns_taken 5 -> 6. No manual layer invalidation.
    state.active_player = PlayerId(1);
    let mut events = Vec::new();
    engine::game::turns::start_next_turn(&mut state, &mut events);
    assert_eq!(state.active_player, controller, "turn advanced to P0");
    assert_eq!(state.players[controller.0 as usize].turns_taken, 6);

    // THE FIX: the turn-advance dirtied the layer cache (revert-failing — a quiet
    // turn has no other dirty source, so without the fix this stays Clean).
    assert!(
        state.layers_dirty.is_dirty(),
        "start_next_turn must invalidate the layer cache when turns_taken changes"
    );

    // Mirror the production game loop: recompute only because the cache is dirty.
    if state.layers_dirty.is_dirty() {
        evaluate_layers(&mut state);
    }
    assert_eq!(
        state.objects[&creature].power,
        Some(6),
        "the Control Win Condition CDA re-derives to 6 through the real turn path"
    );
    assert_eq!(state.objects[&creature].toughness, Some(6));
}

/// PRODUCTION-PATH regression: moving a card with a new non-creature subtype into
/// a graveyard through the real `move_to_zone` path must invalidate the layer
/// cache while a Subgoyf-style `DistinctSubtypes { Zone { Graveyard } }` CDA is
/// live, so its P/T re-derives WITHOUT any manual `mark_full`. Subgoyf's CDA has
/// no static `condition` — it depends on the graveyard purely through its
/// `SetDynamicPower`/`SetDynamicToughness` MODIFICATIONS — so the previous
/// condition-only `any_active_static_reads_zone_membership` check missed it and
/// left the cache clean after graveyard churn. Reverting that helper's
/// modification scan makes the `is_dirty` assertion below fail (a Library ->
/// Graveyard move has no other dirty source).
#[test]
fn subgoyf_invalidated_by_graveyard_move() {
    let mut state = GameState::new_two_player(7);
    let controller = PlayerId(0);
    // Goblin/Wizard are creature types (excluded); Aura is a counted non-creature
    // subtype.
    state.all_creature_types = vec!["Goblin".to_string(), "Wizard".to_string()];

    let creature = setup_cda_creature(
        &mut state,
        controller,
        subgoyf_power_qty(),
        subgoyf_toughness_qty(),
    );

    // A card with a non-creature subtype (Aura) in the library, to be milled into
    // the graveyard through the production zone-move path.
    let milled = create_object(
        &mut state,
        CardId(50),
        controller,
        "Aura Card".to_string(),
        Zone::Library,
    );
    state.objects.get_mut(&milled).unwrap().card_types.subtypes = vec!["Aura".to_string()];

    // Clean layer cache at the empty-graveyard value: 0/1 (N+1 toughness).
    state.layers_dirty.mark_full();
    evaluate_layers(&mut state);
    assert_eq!(state.objects[&creature].power, Some(0));
    assert_eq!(state.objects[&creature].toughness, Some(1));
    assert!(
        !state.layers_dirty.is_dirty(),
        "precondition: the layer cache is clean before the graveyard move"
    );

    // Mill the Aura card into the graveyard through the real zone-move path.
    let mut events = Vec::new();
    engine::game::zones::move_to_zone(&mut state, milled, Zone::Graveyard, &mut events);

    // THE FIX: the graveyard membership change dirtied the layer cache because the
    // live Subgoyf CDA's modifications read the graveyard (revert-failing).
    assert!(
        state.layers_dirty.is_dirty(),
        "moving a card with a new non-creature subtype into a graveyard must invalidate \
         the layer cache while a Subgoyf-style DistinctSubtypes CDA is live"
    );

    // Mirror the production game loop: recompute only because the cache is dirty.
    if state.layers_dirty.is_dirty() {
        evaluate_layers(&mut state);
    }
    assert_eq!(
        state.objects[&creature].power,
        Some(1),
        "Subgoyf re-derives to power 1 (one new non-creature subtype in the graveyard)"
    );
    assert_eq!(state.objects[&creature].toughness, Some(2));
}
