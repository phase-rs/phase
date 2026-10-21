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

// Full printed-source consumers. These are paid public actions; direct layer
// fixtures above remain supplemental count/invalidation controls.
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::WaitingFor;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;

const MARO: &str = "Maro's power and toughness are each equal to the number of cards in your hand.";
const CRUSADER: &str = "Crusader of Odric's power and toughness are each equal to the number of creatures you control.";
const LORD: &str = "Lord of Extinction's power and toughness are each equal to the number of cards in all graveyards.";
const DAKKON: &str =
    "Dakkon Blackblade's power and toughness are each equal to the number of lands you control.";
const BLOOM: &str = "Exile target creature card from a graveyard, then create X 1/1 green Saproling creature tokens, where X is the exiled card's toughness.";
const VAROLZ: &str = "Each creature card in your graveyard has scavenge. The scavenge cost is equal to its mana cost. (Exile a creature card from your graveyard and pay its mana cost: Put a number of +1/+1 counters equal to that card's power on target creature. Scavenge only as a sorcery.)\nSacrifice another creature: Regenerate Varolz.";
const SWORDS: &str = "Exile target creature. Its controller gains life equal to its power.";
const GOOD: &str = "Sacrifice a creature: Draw cards equal to the sacrificed creature's power, then discard three cards.";
const WIPE: &str = "Exile all creatures. Each player creates a 0/0 green and blue Fractal creature token and puts a number of +1/+1 counters on it equal to the total power of creatures they controlled that were exiled this way.";
const GROWTH: &str = "Target creature gets +3/+3 until end of turn.";
const RIPPLE: &str = "Target artifact, creature, or land phases out. (While it's phased out, it's treated as though it doesn't exist. It phases in before its controller untaps during their next untap step.)";
const EXCHANGE: &str = "Exchange control of two target creatures.";
const JINNIE: &str = "If you would create one or more tokens, you may instead create that many 2/2 green Cat creature tokens with haste or that many 3/1 green Dog creature tokens with vigilance.";

fn cda_cost(generic: u32, shards: Vec<ManaCostShard>) -> ManaCost {
    ManaCost::Cost { generic, shards }
}

fn cda_fund(runner: &mut GameRunner, player: PlayerId, colors: &[ManaType]) {
    assert_eq!(
        runner.state().players[player.0 as usize].mana_pool.total(),
        0
    );
    for &color in colors {
        let _ = runner
            .state_mut()
            .add_mana_to_pool(player, ManaUnit::new(color, ObjectId(0), false, vec![]));
    }
}

fn cda_complete(state: &GameState) {
    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { .. }),
        "{:?}",
        state.waiting_for
    );
    assert!(state.stack.is_empty());
    assert!(state.resolution_stack.is_empty());
}

fn cda_pt(state: &GameState, card: ObjectId, value: i32) {
    let object = &state.objects[&card];
    assert_eq!((object.power, object.toughness), (Some(value), Some(value)));
    assert_eq!(
        (object.layer_base_power, object.layer_base_toughness),
        (Some(value), Some(value))
    );
    assert_eq!(
        (object.base_power, object.base_toughness),
        (Some(0), Some(0))
    );
}

fn cda_full_source(state: &GameState, card: ObjectId) {
    let object = &state.objects[&card];
    assert!(
        !object.abilities.is_empty()
            || !object.static_definitions.is_empty()
            || !object.replacement_definitions.is_empty()
    );
    let source = serde_json::to_string(object).unwrap();
    assert!(
        !source.contains("\"Unimplemented\""),
        "full printed source must be represented: {}",
        object.name
    );
}

#[test]
fn cda_current_values_follow_paid_draw_entry_and_graveyard_actions() {
    for zone in [Zone::Hand, Zone::Exile] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let maro = scenario
            .add_creature_to_hand_from_oracle(P0, "Maro", 0, 0, MARO)
            .with_subtypes(vec!["Elemental"])
            .with_mana_cost(cda_cost(
                2,
                vec![ManaCostShard::Green, ManaCostShard::Green],
            ))
            .id();
        let other_maro = scenario
            .add_creature_to_exile(P1, "Maro", 0, 0)
            .from_oracle_text(MARO)
            .with_subtypes(vec!["Elemental"])
            .with_mana_cost(cda_cost(
                2,
                vec![ManaCostShard::Green, ManaCostShard::Green],
            ))
            .id();
        let draw = scenario
            .add_spell_to_hand_from_oracle(P0, "Divination", false, "Draw two cards.")
            .with_mana_cost(cda_cost(2, vec![ManaCostShard::Blue]))
            .id();
        // Five at the initial boundary, including the paid spell and hand Maro.
        scenario.with_cards_in_hand(
            P0,
            if zone == Zone::Hand {
                &["A", "B", "C"]
            } else {
                &["A", "B", "C", "D"]
            },
        );
        scenario.with_cards_in_hand(P1, &["Foreign A", "Foreign B"]);
        let top = [
            scenario.add_card_to_library_top(P0, "Second draw"),
            scenario.add_card_to_library_top(P0, "First draw"),
        ];
        let mut runner = scenario.build();
        if zone == Zone::Exile {
            engine::game::zones::move_to_zone(runner.state_mut(), maro, zone, &mut vec![]);
        }
        engine::game::layers::flush_layers(runner.state_mut());
        cda_pt(runner.state(), maro, 5);
        cda_pt(runner.state(), other_maro, 2);
        cda_full_source(runner.state(), maro);
        cda_full_source(runner.state(), draw);
        cda_complete(runner.state());
        cda_fund(
            &mut runner,
            P0,
            &[ManaType::Blue, ManaType::Colorless, ManaType::Colorless],
        );
        let committed = runner.cast(draw).commit();
        cda_pt(committed.state(), maro, 4);
        assert_eq!(committed.state().players[0].mana_pool.total(), 0);
        let result = committed.resolve();
        result.assert_hand_drawn(P0, 2);
        assert!(top
            .iter()
            .all(|id| result.state().players[0].hand.contains(id)));
        assert_eq!(result.zone_of(draw), Zone::Graveyard);
        cda_pt(result.state(), maro, 6);
        cda_pt(result.state(), other_maro, 2);
        cda_complete(result.state());
    }
}

// Establish three genuinely different information authorities through a real
// paid Growth and ordinary zone transitions: old BF 7, last GY 4, current exile 3.
fn cda_last_graveyard_fixture(
    jinnie: bool,
) -> (GameRunner, ObjectId, ObjectId, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_graveyard(P0, &["Own graveyard A"]);
    scenario.with_graveyard(P1, &["Foreign graveyard A", "Foreign graveyard B"]);
    let lord = scenario
        .add_creature_from_oracle(P0, "Lord of Extinction", 0, 0, LORD)
        .with_subtypes(vec!["Elemental"])
        .with_mana_cost(cda_cost(
            3,
            vec![ManaCostShard::Black, ManaCostShard::Green],
        ))
        .id();
    scenario
        .add_creature_to_exile(P1, "Lord of Extinction", 0, 0)
        .from_oracle_text(LORD)
        .with_subtypes(vec!["Elemental"])
        .with_mana_cost(cda_cost(
            3,
            vec![ManaCostShard::Black, ManaCostShard::Green],
        ));
    let varolz = scenario
        .add_creature_from_oracle(P0, "Varolz, the Scar-Striped", 2, 2, VAROLZ)
        .as_legendary()
        .with_subtypes(vec!["Troll", "Warrior"])
        .with_mana_cost(cda_cost(
            1,
            vec![ManaCostShard::Black, ManaCostShard::Green],
        ))
        .id();
    let recipient = scenario
        .add_creature(P0, "Legal counter recipient", 2, 2)
        .id();
    if jinnie {
        scenario
            .add_creature_from_oracle(P0, "Jinnie Fay, Jetmir's Second", 3, 3, JINNIE)
            .as_legendary()
            .with_subtypes(vec!["Elf", "Druid"])
            .with_mana_cost(cda_cost(
                0,
                vec![
                    ManaCostShard::RedGreen,
                    ManaCostShard::Green,
                    ManaCostShard::GreenWhite,
                ],
            ));
    }
    let growth = scenario
        .add_spell_to_hand_from_oracle(P0, "Giant Growth", true, GROWTH)
        .with_mana_cost(cda_cost(0, vec![ManaCostShard::Green]))
        .id();
    let bloom = scenario
        .add_spell_to_hand_from_oracle(P0, "Morbid Bloom", false, BLOOM)
        .with_mana_cost(cda_cost(
            4,
            vec![ManaCostShard::Black, ManaCostShard::Green],
        ))
        .id();
    let mut runner = scenario.build();
    cda_fund(&mut runner, P0, &[ManaType::Green]);
    runner.cast(growth).target_object(lord).resolve();
    cda_full_source(runner.state(), lord);
    cda_full_source(runner.state(), varolz);
    cda_full_source(runner.state(), bloom);
    assert_eq!(runner.state().objects[&lord].power, Some(7));
    assert_eq!(runner.state().objects[&lord].layer_base_power, Some(4));
    cda_complete(runner.state());
    engine::game::zones::move_to_zone(runner.state_mut(), lord, Zone::Graveyard, &mut vec![]);
    engine::game::zones::move_to_zone(runner.state_mut(), growth, Zone::Exile, &mut vec![]);
    engine::game::layers::flush_layers(runner.state_mut());
    cda_pt(runner.state(), lord, 4);
    assert_eq!(
        runner.state().lki_by_incarnation[&lord]
            .values()
            .next()
            .unwrap()
            .power,
        Some(7)
    );
    cda_complete(runner.state());
    (runner, lord, varolz, recipient, bloom)
}

#[test]
fn cda_current_exile_and_swords_departure_remain_distinct() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_graveyard(P0, &["Own grave A"]);
    scenario.with_graveyard(P1, &["Foreign grave A", "Foreign grave B"]);
    let lord = scenario
        .add_creature_from_oracle(P0, "Lord of Extinction", 0, 0, LORD)
        .with_mana_cost(cda_cost(
            3,
            vec![ManaCostShard::Black, ManaCostShard::Green],
        ))
        .id();
    let witness = scenario
        .add_creature(P1, "Foreign exchange witness", 2, 2)
        .id();
    let exchange = scenario
        .add_spell_to_hand_from_oracle(P0, "Switcheroo", false, EXCHANGE)
        .with_mana_cost(cda_cost(4, vec![ManaCostShard::Blue]))
        .id();
    let growth = scenario
        .add_spell_to_hand_from_oracle(P0, "Giant Growth", true, GROWTH)
        .with_mana_cost(cda_cost(0, vec![ManaCostShard::Green]))
        .id();
    let swords = scenario
        .add_spell_to_hand_from_oracle(P0, "Swords to Plowshares", true, SWORDS)
        .with_mana_cost(cda_cost(0, vec![ManaCostShard::White]))
        .id();
    let mut runner = scenario.build();
    cda_fund(
        &mut runner,
        P0,
        &[
            ManaType::Blue,
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Colorless,
        ],
    );
    runner
        .cast(exchange)
        .target_objects(&[lord, witness])
        .resolve();
    cda_full_source(runner.state(), exchange);
    assert_eq!(runner.state().objects[&lord].controller, P1);
    cda_complete(runner.state());
    // Preparation removes one old grave card; Growth's arrival restores four.
    let grave = runner.state().players[0].graveyard[0];
    engine::game::zones::move_to_zone(runner.state_mut(), grave, Zone::Exile, &mut vec![]);
    cda_fund(&mut runner, P0, &[ManaType::Green]);
    runner.cast(growth).target_object(lord).resolve();
    assert_eq!(runner.state().objects[&lord].power, Some(7));
    assert_eq!(runner.state().objects[&lord].layer_base_power, Some(4));
    cda_complete(runner.state());
    cda_fund(&mut runner, P0, &[ManaType::White]);
    let outcome = runner.cast(swords).target_object(lord).resolve();
    outcome.assert_life_delta(P1, 7);
    outcome.assert_life_delta(P0, 0);
    assert_eq!(outcome.zone_of(lord), Zone::Exile);
    assert_eq!(outcome.zone_of(swords), Zone::Graveyard);
    cda_pt(outcome.state(), lord, 5);
    assert_eq!(outcome.state().objects[&lord].owner, P0);
    cda_full_source(outcome.state(), swords);
    cda_complete(outcome.state());
}

#[test]
fn cda_greater_good_uses_exact_sacrificed_incarnation() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_graveyard(P0, &["A"]);
    scenario.with_graveyard(P1, &["B", "C"]);
    let lord = scenario
        .add_creature_from_oracle(P0, "Lord of Extinction", 0, 0, LORD)
        .with_subtypes(vec!["Elemental"])
        .with_mana_cost(cda_cost(
            3,
            vec![ManaCostShard::Black, ManaCostShard::Green],
        ))
        .id();
    let decoy = scenario
        .add_creature(P0, "Other possible sacrifice", 2, 2)
        .id();
    let good = scenario
        .add_enchantment_from_oracle(P0, "Greater Good", GOOD)
        .with_mana_cost(cda_cost(
            2,
            vec![ManaCostShard::Green, ManaCostShard::Green],
        ))
        .id();
    let growth = scenario
        .add_spell_to_hand_from_oracle(P0, "Giant Growth", true, GROWTH)
        .with_mana_cost(cda_cost(0, vec![ManaCostShard::Green]))
        .id();
    let drawn: Vec<_> = (0..9)
        .map(|_| scenario.add_card_to_library_top(P0, "Draw witness"))
        .collect();
    let mut runner = scenario.build();
    cda_fund(&mut runner, P0, &[ManaType::Green]);
    runner.cast(growth).target_object(lord).resolve();
    assert_eq!(runner.state().objects[&lord].power, Some(7));
    cda_complete(runner.state());
    cda_full_source(runner.state(), good);
    runner
        .act(GameAction::ActivateAbility {
            source_id: good,
            ability_index: 0,
        })
        .unwrap();
    let WaitingFor::PayCost {
        player,
        choices,
        count,
        ..
    } = &runner.state().waiting_for
    else {
        panic!("real sacrifice prompt")
    };
    assert_eq!(*player, P0);
    assert_eq!(*count, 1);
    assert!(choices.contains(&lord) && choices.contains(&decoy));
    runner
        .act(GameAction::SelectCards { cards: vec![lord] })
        .unwrap();
    cda_pt(runner.state(), lord, 5);
    for _ in 0..8 {
        if matches!(runner.state().waiting_for, WaitingFor::DiscardChoice { .. }) {
            break;
        }
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { .. }
        ));
        runner.act(GameAction::PassPriority).unwrap();
    }
    let WaitingFor::DiscardChoice {
        player,
        count,
        cards,
        ..
    } = &runner.state().waiting_for
    else {
        panic!("mandatory discard after seven actual draws")
    };
    assert_eq!(*player, P0);
    assert_eq!(*count, 3);
    assert_eq!(cards.len(), 7);
    assert_eq!(runner.state().players[0].library.len(), 2);
    assert!(drawn[2..].iter().all(|id| cards.contains(id)));
    let discarded = vec![cards[6], cards[2], cards[4]];
    runner
        .act(GameAction::SelectCards {
            cards: discarded.clone(),
        })
        .unwrap();
    assert!(discarded
        .iter()
        .all(|id| runner.state().objects[id].zone == Zone::Graveyard));
    assert_eq!(runner.state().players[0].hand.len(), 4);
    assert_eq!(runner.state().objects[&decoy].zone, Zone::Battlefield);
    cda_pt(runner.state(), lord, 8);
    cda_complete(runner.state());
}

#[test]
fn cda_morbid_bloom_uses_last_graveyard_toughness() {
    for choice in [None, Some(false), Some(true)] {
        let (mut runner, lord, _, _, bloom) = cda_last_graveyard_fixture(choice.is_some());
        cda_fund(
            &mut runner,
            P0,
            &[
                ManaType::Black,
                ManaType::Green,
                ManaType::Colorless,
                ManaType::Colorless,
                ManaType::Colorless,
                ManaType::Colorless,
            ],
        );
        if choice.is_some() {
            runner
                .cast(bloom)
                .target_object(lord)
                .commit()
                .stop_at_optional()
                .resolve();
            let WaitingFor::ReplacementChoice {
                player,
                candidate_count,
                ..
            } = &runner.state().waiting_for
            else {
                panic!("full Jinnie replacement before births")
            };
            assert_eq!(*player, P0);
            assert_eq!(*candidate_count, 2);
            assert_eq!(runner.state().objects[&bloom].zone, Zone::Stack);
            cda_pt(runner.state(), lord, 3);
            assert_eq!(
                runner
                    .state()
                    .objects
                    .values()
                    .filter(|object| object.is_token)
                    .count(),
                0
            );
            let saved = serde_json::to_string(runner.state()).unwrap();
            *runner.state_mut() = serde_json::from_str(&saved).unwrap();
            runner
                .act(GameAction::ChooseReplacement {
                    index: if choice == Some(true) { 0 } else { 1 },
                })
                .unwrap();
            if choice == Some(true) {
                let WaitingFor::ChooseOneOfBranch {
                    player, branches, ..
                } = &runner.state().waiting_for
                else {
                    panic!("Cat/Dog public branch")
                };
                assert_eq!(*player, P0);
                assert_eq!(branches.len(), 2);
                runner.act(GameAction::ChooseBranch { index: 1 }).unwrap();
            }
        } else {
            runner.cast(bloom).target_object(lord).resolve();
        }
        let tokens: Vec<_> = runner
            .state()
            .objects
            .values()
            .filter(|object| object.is_token)
            .collect();
        assert_eq!(tokens.len(), 4, "lastGY4, not currentExile3 or staleBF7");
        for token in tokens {
            assert_eq!(token.zone, Zone::Battlefield);
            assert_eq!(token.owner, P0);
            assert_eq!(token.controller, P0);
            assert_eq!(token.color, vec![ManaColor::Green]);
            assert!(token.card_types.core_types.contains(&CoreType::Creature));
            if choice == Some(true) {
                assert_eq!((token.power, token.toughness), (Some(3), Some(1)));
                assert!(token.card_types.subtypes.iter().any(|name| name == "Dog"));
                assert!(token
                    .keywords
                    .iter()
                    .any(|keyword| matches!(keyword, engine::types::keywords::Keyword::Vigilance)));
            } else {
                assert_eq!((token.power, token.toughness), (Some(1), Some(1)));
                assert!(token
                    .card_types
                    .subtypes
                    .iter()
                    .any(|name| name == "Saproling"));
            }
        }
        cda_pt(runner.state(), lord, 4);
        assert_eq!(runner.state().objects[&bloom].zone, Zone::Graveyard);
        assert_eq!(runner.state().players[0].mana_pool.total(), 0);
        cda_complete(runner.state());
    }
}

#[test]
fn cda_scavenge_uses_last_graveyard_source_incarnation() {
    let (mut runner, lord, _, recipient, _) = cda_last_graveyard_fixture(false);
    // Off-zone storage may retain a prior controller; the owner still
    // announces and pays this graveyard activation.
    runner
        .state_mut()
        .objects
        .get_mut(&lord)
        .unwrap()
        .controller = P1;
    let abilities = engine::game::casting::activated_ability_definitions(runner.state(), lord);
    let (index, ability) = abilities
        .into_iter()
        .find(|(_, definition)| {
            matches!(
                definition.effect.as_ref(),
                engine::types::ability::Effect::PutCounter { .. }
            )
        })
        .expect("real Varolz grants synthesized Scavenge");
    assert_eq!(ability.kind, engine::types::ability::AbilityKind::Activated);
    assert_eq!(
        ability.cost,
        Some(engine::types::ability::AbilityCost::Composite {
            costs: vec![
                engine::types::ability::AbilityCost::Mana {
                    cost: cda_cost(3, vec![ManaCostShard::Black, ManaCostShard::Green])
                },
                engine::types::ability::AbilityCost::Exile {
                    count: 1,
                    zone: Some(Zone::Graveyard),
                    filter: Some(TargetFilter::SelfRef)
                },
            ]
        })
    );
    let before = runner.state().objects[&lord].incarnation;
    cda_fund(
        &mut runner,
        P0,
        &[
            ManaType::Black,
            ManaType::Green,
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Colorless,
        ],
    );
    runner
        .act(GameAction::ActivateAbility {
            source_id: lord,
            ability_index: index,
        })
        .unwrap();
    let WaitingFor::TargetSelection {
        player,
        pending_cast,
        target_slots,
        ..
    } = &runner.state().waiting_for
    else {
        panic!("actual target-first Scavenge")
    };
    assert_eq!(*player, P0);
    assert_eq!(pending_cast.ability.source_incarnation, Some(before));
    assert!(!target_slots.is_empty());
    assert_eq!(runner.state().objects[&lord].zone, Zone::Graveyard);
    runner
        .act(GameAction::ChooseTarget {
            target: TargetRef::Object(recipient),
        })
        .unwrap();
    assert_eq!(runner.state().objects[&lord].zone, Zone::Exile);
    assert_ne!(runner.state().objects[&lord].incarnation, before);
    assert_eq!(
        runner
            .state()
            .stack
            .last()
            .unwrap()
            .ability()
            .unwrap()
            .source_incarnation,
        Some(before)
    );
    assert_eq!(
        runner.state().lki_by_incarnation[&lord][&before].power,
        Some(4)
    );
    assert_eq!(runner.state().players[0].mana_pool.total(), 0);
    cda_pt(runner.state(), lord, 3);
    let saved = serde_json::to_string(runner.state()).unwrap();
    *runner.state_mut() = serde_json::from_str(&saved).unwrap();
    for _ in 0..8 {
        if runner.state().stack.is_empty() {
            break;
        }
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { .. }
        ));
        runner.act(GameAction::PassPriority).unwrap();
    }
    assert_eq!(
        runner.state().objects[&recipient]
            .counters
            .get(&CounterType::Plus1Plus1),
        Some(&4)
    );
    assert_eq!(runner.state().objects[&recipient].power, Some(6));
    cda_pt(runner.state(), lord, 3);
    cda_complete(runner.state());
    let after = runner.state().objects[&lord].incarnation;
    assert!(runner
        .act(GameAction::ActivateAbility {
            source_id: lord,
            ability_index: index
        })
        .is_err());
    assert_eq!(runner.state().objects[&lord].incarnation, after);
    assert_eq!(
        runner.state().objects[&recipient]
            .counters
            .get(&CounterType::Plus1Plus1),
        Some(&4)
    );
    cda_complete(runner.state());
}

#[test]
fn cda_remote_values_follow_incremental_population_entries() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature(P0, "Own A", 2, 2);
    scenario.add_creature(P0, "Own B", 2, 2);
    scenario.add_creature(P1, "Foreign A", 2, 2);
    scenario.add_basic_land(P0, ManaColor::Green);
    scenario.add_basic_land(P0, ManaColor::Blue);
    for _ in 0..5 {
        scenario.add_basic_land(P1, ManaColor::White);
    }
    let crusader = scenario
        .add_creature_to_exile(P0, "Crusader of Odric", 0, 0)
        .from_oracle_text(CRUSADER)
        .with_subtypes(vec!["Human", "Soldier"])
        .with_mana_cost(cda_cost(2, vec![ManaCostShard::White]))
        .id();
    let dakkon = scenario
        .add_creature_from_oracle(P0, "Dakkon Blackblade", 0, 0, DAKKON)
        .as_legendary()
        .with_subtypes(vec!["Human", "Warrior"])
        .with_mana_cost(cda_cost(
            2,
            vec![
                ManaCostShard::White,
                ManaCostShard::Blue,
                ManaCostShard::Blue,
                ManaCostShard::Black,
            ],
        ))
        .id();
    scenario.with_commander(dakkon);
    let creature = scenario
        .add_creature_to_hand(P0, "Creature entrant", 2, 2)
        .id();
    let irrelevant = scenario
        .add_artifact_to_hand_from_oracle(
            P0,
            "Mirror Gallery",
            "The \"legend rule\" doesn't apply.",
        )
        .with_mana_cost(cda_cost(5, vec![]))
        .id();
    let land = scenario.add_land_to_hand(P0, "Forest").id();
    let mut runner = scenario.build();
    for id in [creature, irrelevant, land] {
        engine::game::zones::move_to_zone(runner.state_mut(), id, Zone::Library, &mut vec![]);
    }
    engine::game::layers::flush_layers(runner.state_mut());
    cda_pt(runner.state(), crusader, 2);
    cda_pt(runner.state(), dakkon, 2);
    cda_complete(runner.state());
    // No manual dirtiness: production library-to-BF proposals and the normal
    // public boundary exercise prepare_incremental_flush's magnitude checks.
    engine::game::zones::move_to_zone(
        runner.state_mut(),
        irrelevant,
        Zone::Battlefield,
        &mut vec![],
    );
    engine::game::layers::flush_layers(runner.state_mut());
    cda_pt(runner.state(), crusader, 2);
    cda_pt(runner.state(), dakkon, 2);
    cda_complete(runner.state());
    engine::game::zones::move_to_zone(runner.state_mut(), creature, Zone::Battlefield, &mut vec![]);
    engine::game::layers::flush_layers(runner.state_mut());
    cda_pt(runner.state(), crusader, 3);
    cda_pt(runner.state(), dakkon, 2);
    cda_complete(runner.state());
    engine::game::zones::move_to_zone(runner.state_mut(), land, Zone::Battlefield, &mut vec![]);
    engine::game::layers::flush_layers(runner.state_mut());
    cda_pt(runner.state(), crusader, 3);
    cda_pt(runner.state(), dakkon, 3);
    cda_complete(runner.state());
    // Its own zone changes even though the counted battlefield is unchanged.
    engine::game::zones::move_to_zone(runner.state_mut(), crusader, Zone::Graveyard, &mut vec![]);
    engine::game::layers::flush_layers(runner.state_mut());
    cda_pt(runner.state(), crusader, 3);
    engine::game::zones::move_to_zone(runner.state_mut(), crusader, Zone::Exile, &mut vec![]);
    engine::game::layers::flush_layers(runner.state_mut());
    cda_pt(runner.state(), crusader, 3);
    cda_complete(runner.state());
}

#[test]
fn cda_phasing_control_and_source_incarnations_keep_ordinary_boundaries() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_creature_from_oracle(P0, "Crusader of Odric", 0, 0, CRUSADER)
        .with_subtypes(vec!["Human", "Soldier"])
        .with_mana_cost(cda_cost(2, vec![ManaCostShard::White]))
        .id();
    let counted = scenario.add_creature(P0, "Counted member", 2, 2).id();
    scenario.add_creature(P0, "Resident member", 2, 2);
    let remote = scenario
        .add_creature_to_exile(P0, "Crusader of Odric", 0, 0)
        .from_oracle_text(CRUSADER)
        .with_subtypes(vec!["Human", "Soldier"])
        .with_mana_cost(cda_cost(2, vec![ManaCostShard::White]))
        .id();
    let ripple: Vec<_> = (0..3)
        .map(|_| {
            scenario
                .add_spell_to_hand_from_oracle(P0, "Reality Ripple", true, RIPPLE)
                .with_mana_cost(cda_cost(1, vec![ManaCostShard::Blue]))
                .id()
        })
        .collect();
    let land = scenario.add_basic_land(P0, ManaColor::Green);
    scenario.add_basic_land(P0, ManaColor::Blue);
    let dakkon = scenario
        .add_creature_from_oracle(P0, "Dakkon Blackblade", 0, 0, DAKKON)
        .as_legendary()
        .with_subtypes(vec!["Human", "Warrior"])
        .with_mana_cost(cda_cost(
            2,
            vec![
                ManaCostShard::White,
                ManaCostShard::Blue,
                ManaCostShard::Blue,
                ManaCostShard::Black,
            ],
        ))
        .id();
    scenario.with_commander(dakkon);
    for player in [P0, P1] {
        scenario.with_library_top(player, &["Draw A", "Draw B", "Draw C"]);
    }
    let mut runner = scenario.build();
    engine::game::layers::flush_layers(runner.state_mut());
    cda_pt(runner.state(), source, 3);
    cda_pt(runner.state(), remote, 3);
    let epoch = runner.state().objects[&source].incarnation;
    cda_complete(runner.state());
    cda_fund(&mut runner, P0, &[ManaType::Blue, ManaType::Colorless]);
    runner.cast(ripple[0]).target_object(source).resolve();
    cda_full_source(runner.state(), ripple[0]);
    assert!(runner.state().objects[&source].is_phased_out());
    assert_eq!(runner.state().objects[&source].incarnation, epoch);
    cda_pt(runner.state(), source, 3); // frozen, excluded from reset AND apply
    cda_pt(runner.state(), remote, 2);
    cda_complete(runner.state());
    cda_fund(&mut runner, P0, &[ManaType::Blue, ManaType::Colorless]);
    runner.cast(ripple[1]).target_object(counted).resolve();
    assert!(runner.state().objects[&counted].is_phased_out());
    cda_pt(runner.state(), remote, 1);
    cda_pt(runner.state(), source, 3);
    cda_complete(runner.state());
    cda_pt(runner.state(), dakkon, 2);
    cda_fund(&mut runner, P0, &[ManaType::Blue, ManaType::Colorless]);
    runner.cast(ripple[2]).target_object(land).resolve();
    assert!(runner.state().objects[&land].is_phased_out());
    cda_pt(runner.state(), dakkon, 1);
    cda_complete(runner.state());
    let initial_turn = runner.state().turn_number;
    for _ in 0..96 {
        if runner.state().turn_number > initial_turn
            && runner.state().active_player == P0
            && runner.state().phase == Phase::Upkeep
        {
            break;
        }
        match runner.state().waiting_for {
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).unwrap();
            }
            WaitingFor::DeclareAttackers { .. } => {
                runner
                    .act(GameAction::DeclareAttackers {
                        attacks: vec![],
                        bands: vec![],
                    })
                    .unwrap();
            }
            WaitingFor::DeclareBlockers { .. } => {
                runner
                    .act(GameAction::DeclareBlockers {
                        assignments: vec![],
                    })
                    .unwrap();
            }
            ref other => panic!("ordinary turn progression: {other:?}"),
        }
    }
    assert_eq!(runner.state().active_player, P0);
    assert!(!runner.state().objects[&source].is_phased_out());
    assert!(!runner.state().objects[&counted].is_phased_out());
    assert_eq!(runner.state().objects[&source].incarnation, epoch);
    cda_pt(runner.state(), source, 3);
    cda_pt(runner.state(), remote, 3);
    cda_pt(runner.state(), dakkon, 2);
    assert!(!runner.state().objects[&land].is_phased_out());
    cda_complete(runner.state());
}

fn cda_combined_wipe_fixture() -> (
    GameRunner,
    ObjectId,
    ObjectId,
    ObjectId,
    [ObjectId; 2],
    ObjectId,
    ObjectId,
) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_graveyard(P0, &["Earlier own grave"]);
    scenario.with_graveyard(P1, &["Earlier foreign grave"]);
    let crusader = scenario
        .add_creature_from_oracle(P0, "Crusader of Odric", 0, 0, CRUSADER)
        .with_plus_counters(2)
        .with_subtypes(vec!["Human", "Soldier"])
        .with_mana_cost(cda_cost(2, vec![ManaCostShard::White]))
        .id();
    let lord = scenario
        .add_creature_from_oracle(P1, "Lord of Extinction", 0, 0, LORD)
        .with_subtypes(vec!["Elemental"])
        .with_mana_cost(cda_cost(
            3,
            vec![ManaCostShard::Black, ManaCostShard::Green],
        ))
        .id();
    scenario.add_creature(P0, "P0 witness", 2, 2);
    scenario.add_creature(P1, "P1 witness", 3, 3);
    let maro = scenario
        .add_creature_to_exile(P0, "Maro", 0, 0)
        .from_oracle_text(MARO)
        .with_subtypes(vec!["Elemental"])
        .with_mana_cost(cda_cost(
            2,
            vec![ManaCostShard::Green, ManaCostShard::Green],
        ))
        .id();
    let exchange = scenario
        .add_spell_to_hand_from_oracle(P0, "Switcheroo", false, EXCHANGE)
        .with_mana_cost(cda_cost(4, vec![ManaCostShard::Blue]))
        .id();
    let growth = scenario
        .add_spell_to_hand_from_oracle(P0, "Giant Growth", true, GROWTH)
        .with_mana_cost(cda_cost(0, vec![ManaCostShard::Green]))
        .id();
    let wipes = [0, 1].map(|_| {
        scenario
            .add_spell_to_hand_from_oracle(P0, "Oversimplify", false, WIPE)
            .with_mana_cost(cda_cost(3, vec![ManaCostShard::Green, ManaCostShard::Blue]))
            .id()
    });
    let draw = scenario
        .add_spell_to_hand_from_oracle(P0, "Divination", false, "Draw two cards.")
        .with_mana_cost(cda_cost(2, vec![ManaCostShard::Blue]))
        .id();
    let later_growth = scenario
        .add_spell_to_hand_from_oracle(P0, "Giant Growth", true, GROWTH)
        .with_mana_cost(cda_cost(0, vec![ManaCostShard::Green]))
        .id();
    scenario.add_card_to_hand(P0, "Hand witness");
    scenario.with_library_top(P0, &["Draw first", "Draw second", "Reserve"]);
    let mut runner = scenario.build();
    cda_fund(
        &mut runner,
        P0,
        &[
            ManaType::Blue,
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Colorless,
        ],
    );
    runner
        .cast(exchange)
        .target_objects(&[crusader, lord])
        .resolve();
    cda_full_source(runner.state(), exchange);
    assert_eq!(runner.state().objects[&lord].controller, P0);
    assert_eq!(runner.state().objects[&crusader].controller, P1);
    cda_complete(runner.state());
    cda_fund(&mut runner, P0, &[ManaType::Green]);
    runner.cast(growth).target_object(lord).resolve();
    assert_eq!(runner.state().objects[&lord].power, Some(7));
    assert_eq!(runner.state().objects[&lord].layer_base_power, Some(4));
    assert_eq!(runner.state().objects[&crusader].power, Some(4));
    assert_eq!(runner.state().objects[&crusader].layer_base_power, Some(2));
    cda_pt(runner.state(), maro, 5);
    cda_complete(runner.state());
    (runner, lord, crusader, maro, wipes, draw, later_growth)
}

fn cda_fractal(state: &GameState, player: PlayerId, value: u32) -> ObjectId {
    let tokens: Vec<_> = state
        .battlefield
        .iter()
        .filter_map(|id| state.objects.get(id))
        .filter(|object| object.is_token && object.name == "Fractal" && object.controller == player)
        .collect();
    assert_eq!(tokens.len(), 1);
    let token = tokens[0];
    assert_eq!(token.owner, player);
    assert_eq!((token.base_power, token.base_toughness), (Some(0), Some(0)));
    assert_eq!(
        (token.power, token.toughness),
        (Some(value as i32), Some(value as i32))
    );
    assert_eq!(
        token
            .counters
            .get(&CounterType::Plus1Plus1)
            .copied()
            .unwrap_or(0),
        value
    );
    assert!(token.color.contains(&ManaColor::Green) && token.color.contains(&ManaColor::Blue));
    assert!(token.card_types.core_types.contains(&CoreType::Creature));
    assert!(token
        .card_types
        .subtypes
        .iter()
        .any(|name| name == "Fractal"));
    token.id
}

#[test]
fn cda_oversimplify_keeps_departure_power_while_exiled_cdas_recompute() {
    let (mut runner, lord, crusader, maro, wipes, draw, later_growth) = cda_combined_wipe_fixture();
    cda_fund(
        &mut runner,
        P0,
        &[
            ManaType::Green,
            ManaType::Blue,
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Colorless,
        ],
    );
    let committed = runner.cast(wipes[0]).commit();
    cda_pt(committed.state(), maro, 4);
    let outcome = committed.resolve();
    cda_full_source(outcome.state(), wipes[0]);
    cda_fractal(outcome.state(), P0, 9);
    cda_fractal(outcome.state(), P1, 7);
    cda_pt(outcome.state(), lord, 5);
    cda_pt(outcome.state(), crusader, 1);
    assert_eq!(outcome.state().objects[&lord].zone, Zone::Exile);
    assert_eq!(outcome.state().objects[&crusader].zone, Zone::Exile);
    assert_eq!(outcome.state().players[0].mana_pool.total(), 0);
    cda_complete(outcome.state());
    cda_fund(
        &mut runner,
        P0,
        &[ManaType::Blue, ManaType::Colorless, ManaType::Colorless],
    );
    let committed = runner.cast(draw).commit();
    cda_pt(committed.state(), maro, 3);
    let outcome = committed.resolve();
    outcome.assert_hand_drawn(P0, 2);
    cda_pt(outcome.state(), maro, 5);
    cda_fractal(outcome.state(), P0, 9);
    let foreign = cda_fractal(outcome.state(), P1, 7);
    cda_complete(outcome.state());
    // A second genuine same-name producer gets a new logical occurrence and
    // reads a different population. It cannot borrow the first 9/7 snapshot.
    cda_fund(&mut runner, P0, &[ManaType::Green]);
    runner.cast(later_growth).target_object(foreign).resolve();
    assert_eq!(runner.state().objects[&foreign].power, Some(10));
    cda_complete(runner.state());
    cda_fund(
        &mut runner,
        P0,
        &[
            ManaType::Green,
            ManaType::Blue,
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Colorless,
        ],
    );
    runner.cast(wipes[1]).resolve();
    cda_fractal(runner.state(), P0, 9);
    cda_fractal(runner.state(), P1, 10);
    cda_pt(runner.state(), lord, 8); // first wipe, draw, later Growth, second wipe
    cda_pt(runner.state(), crusader, 1);
    assert!(runner.state().return_result_frames.is_empty());
    cda_complete(runner.state());
}

#[test]
fn cda_oversimplify_paused_batch_reload_and_empty_remain_distinct() {
    use engine::types::ability::{AbilityCost, ReplacementDefinition, ReplacementMode};
    use engine::types::replacements::ReplacementEvent;
    let (mut runner, lord, crusader, _, wipes, _, _) = cda_combined_wipe_fixture();
    let victims: Vec<_> = runner.state().battlefield.iter().copied().collect();
    assert_eq!(victims.len(), 4);
    for id in &victims {
        let replacement = ReplacementDefinition::new(ReplacementEvent::Moved)
            .valid_card(TargetFilter::SelfRef)
            .destination_zone(Zone::Exile)
            .mode(ReplacementMode::MayCost {
                cost: AbilityCost::PayLife {
                    amount: QuantityExpr::Fixed { value: 1 },
                },
                decline: None,
            });
        let object = runner.state_mut().objects.get_mut(id).unwrap();
        object.replacement_definitions.push(replacement.clone());
        std::sync::Arc::make_mut(&mut object.base_replacement_definitions).push(replacement);
    }
    cda_fund(
        &mut runner,
        P0,
        &[
            ManaType::Green,
            ManaType::Blue,
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Colorless,
        ],
    );
    runner.cast(wipes[0]).commit().stop_at_optional().resolve();
    for completed in 0..4 {
        let WaitingFor::ReplacementChoice {
            player,
            candidate_count,
            ..
        } = &runner.state().waiting_for
        else {
            panic!("every member has a genuine replacement pause")
        };
        assert!([P0, P1].contains(player));
        assert_eq!(*candidate_count, 2);
        assert!(runner
            .state()
            .objects
            .values()
            .all(|object| !object.is_token));
        let pending = runner
            .state()
            .active_change_zone_frame()
            .unwrap()
            .pending
            .as_ref()
            .unwrap();
        assert_eq!(
            pending
                .logical_zone_change_group
                .all_origin_occurrences
                .len(),
            completed
        );
        let saved = serde_json::to_string(runner.state()).unwrap();
        *runner.state_mut() = serde_json::from_str(&saved).unwrap();
        runner
            .act(GameAction::ChooseReplacement { index: 1 })
            .unwrap();
    }
    cda_fractal(runner.state(), P0, 9);
    cda_fractal(runner.state(), P1, 7);
    cda_pt(runner.state(), lord, 5);
    cda_pt(runner.state(), crusader, 1);
    assert!(runner.state().return_result_frames.is_empty());
    cda_complete(runner.state());

    // Full paid empty producer: actual 0/0 tokens are born and then die to SBA.
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let lord = scenario
        .add_creature_to_exile(P0, "Lord of Extinction", 0, 0)
        .from_oracle_text(LORD)
        .with_subtypes(vec!["Elemental"])
        .with_mana_cost(cda_cost(
            3,
            vec![ManaCostShard::Black, ManaCostShard::Green],
        ))
        .id();
    let empty = scenario
        .add_spell_to_hand_from_oracle(P0, "Oversimplify", false, WIPE)
        .with_mana_cost(cda_cost(3, vec![ManaCostShard::Green, ManaCostShard::Blue]))
        .id();
    let mut runner = scenario.build();
    cda_fund(
        &mut runner,
        P0,
        &[
            ManaType::Green,
            ManaType::Blue,
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Colorless,
        ],
    );
    let outcome = runner.cast(empty).resolve();
    let tokens: Vec<_> = outcome
        .events()
        .iter()
        .filter_map(|event| match event {
            engine::types::events::GameEvent::TokenCreated {
                object_id,
                name,
                source_id,
            } if name == "Fractal" && *source_id == empty => Some(*object_id),
            _ => None,
        })
        .collect();
    assert_eq!(tokens.len(), 2);
    assert_ne!(tokens[0], tokens[1]);
    for id in tokens {
        // CR 704.5f + CR 704.5d: each actual zero-toughness birth dies, then
        // ceases to exist. Its recorded departure preserves the zero values.
        let record = outcome
            .state()
            .zone_changes_this_turn
            .iter()
            .find(|record| {
                record.object_id == id
                    && record.from_zone == Some(Zone::Battlefield)
                    && record.to_zone == Zone::Graveyard
            })
            .unwrap();
        assert_eq!((record.power, record.toughness), (Some(0), Some(0)));
        assert!(!outcome.events().iter().any(|event| matches!(event, engine::types::events::GameEvent::CounterAdded { object_id, .. } if *object_id == id)));
        assert!(!outcome.state().objects.contains_key(&id));
    }
    cda_pt(outcome.state(), lord, 1); // the completed wipe, never a previous result
    assert!(outcome.state().return_result_frames.is_empty());
    cda_complete(outcome.state());
}

#[test]
fn cda_scavenge_wrong_colors_no_targets_and_sorcery_timing_have_funded_controls() {
    use engine::types::keywords::Keyword;
    for hostile in ["colors", "targets", "opponent", "combat", "stack"] {
        let (mut runner, lord, _, recipient, bloom) = cda_last_graveyard_fixture(false);
        let pristine = runner.state().clone();
        let index = engine::game::casting::activated_ability_definitions(runner.state(), lord)
            .into_iter()
            .find(|(_, ability)| {
                matches!(
                    ability.effect.as_ref(),
                    engine::types::ability::Effect::PutCounter { .. }
                )
            })
            .unwrap()
            .0;
        if hostile == "stack" {
            cda_fund(
                &mut runner,
                P0,
                &[
                    ManaType::Black,
                    ManaType::Green,
                    ManaType::Colorless,
                    ManaType::Colorless,
                    ManaType::Colorless,
                    ManaType::Colorless,
                ],
            );
            let mut committed = runner.cast(bloom).target_object(lord).commit();
            for color in [
                ManaType::Black,
                ManaType::Green,
                ManaType::Colorless,
                ManaType::Colorless,
                ManaType::Colorless,
            ] {
                let _ = committed
                    .state_mut()
                    .add_mana_to_pool(P0, ManaUnit::new(color, ObjectId(0), false, vec![]));
            }
            assert!(committed
                .act(GameAction::ActivateAbility {
                    source_id: lord,
                    ability_index: index
                })
                .is_err());
            assert_eq!(committed.state().objects[&lord].zone, Zone::Graveyard);
            assert_eq!(committed.state().players[0].mana_pool.total(), 5);
            let result = committed.resolve();
            assert_eq!(
                result
                    .state()
                    .objects
                    .values()
                    .filter(|object| object.is_token)
                    .count(),
                4
            );
            cda_complete(result.state());
        } else {
            let mana = if hostile == "colors" {
                [ManaType::Colorless; 5]
            } else {
                [
                    ManaType::Black,
                    ManaType::Green,
                    ManaType::Colorless,
                    ManaType::Colorless,
                    ManaType::Colorless,
                ]
            };
            cda_fund(&mut runner, P0, &mana);
            if hostile == "targets" {
                // Legitimate initial shroud on EVERY possible creature target,
                // including the actual granting Varolz itself.
                let battlefield: Vec<_> = runner.state().battlefield.iter().copied().collect();
                for id in battlefield {
                    let object = runner.state_mut().objects.get_mut(&id).unwrap();
                    object.base_keywords.push(Keyword::Shroud);
                    object.keywords.push(Keyword::Shroud);
                }
            }
            if hostile == "opponent" {
                runner.state_mut().active_player = P1;
            }
            if hostile == "combat" {
                runner.state_mut().phase = Phase::BeginCombat;
            }
            cda_full_source(runner.state(), lord);
            let before = runner.state().objects[&lord].incarnation;
            assert!(
                runner
                    .act(GameAction::ActivateAbility {
                        source_id: lord,
                        ability_index: index
                    })
                    .is_err(),
                "{hostile}"
            );
            assert_eq!(runner.state().objects[&lord].zone, Zone::Graveyard);
            assert_eq!(runner.state().objects[&lord].incarnation, before);
            assert_eq!(runner.state().players[0].mana_pool.total(), 5);
            assert_eq!(
                runner.state().objects[&recipient]
                    .counters
                    .get(&CounterType::Plus1Plus1),
                None
            );
            cda_complete(runner.state());
        }
        // SAME-TEST positive, same full producer/definition/index/source shape.
        let mut positive = GameRunner::from_state(pristine);
        cda_fund(
            &mut positive,
            P0,
            &[
                ManaType::Black,
                ManaType::Green,
                ManaType::Colorless,
                ManaType::Colorless,
                ManaType::Colorless,
            ],
        );
        positive
            .activate(lord, index)
            .target_object(recipient)
            .resolve();
        assert_eq!(positive.state().objects[&lord].zone, Zone::Exile);
        assert_eq!(
            positive.state().objects[&recipient]
                .counters
                .get(&CounterType::Plus1Plus1),
            Some(&4)
        );
        assert_eq!(positive.state().players[0].mana_pool.total(), 0);
        cda_pt(positive.state(), lord, 3);
        cda_complete(positive.state());
    }
}

#[test]
fn cda_scavenge_cost_replacement_reload_and_same_id_reentry_preserve_the_old_source() {
    use engine::types::ability::{
        AbilityCost, AbilityDefinition, AbilityKind, Effect, ReplacementDefinition, ReplacementMode,
    };
    use engine::types::replacements::ReplacementEvent;
    for return_zone in [Zone::Graveyard, Zone::Battlefield] {
        let (mut runner, lord, _, recipient, _) = cda_last_graveyard_fixture(false);
        let before = runner.state().objects[&lord].incarnation;
        let replacement = ReplacementDefinition::new(ReplacementEvent::Moved)
            .valid_card(TargetFilter::SelfRef)
            .active_zones(vec![Zone::Graveyard])
            .destination_zone(Zone::Exile)
            .mode(ReplacementMode::MayCost {
                cost: AbilityCost::PayLife {
                    amount: QuantityExpr::Fixed { value: 1 },
                },
                decline: Some(Box::new(AbilityDefinition::new(
                    AbilityKind::Spell,
                    Effect::NoOp,
                ))),
            });
        let object = runner.state_mut().objects.get_mut(&lord).unwrap();
        object.replacement_definitions.push(replacement.clone());
        std::sync::Arc::make_mut(&mut object.base_replacement_definitions).push(replacement);
        let index = engine::game::casting::activated_ability_definitions(runner.state(), lord)
            .into_iter()
            .find(|(_, ability)| matches!(ability.effect.as_ref(), Effect::PutCounter { .. }))
            .unwrap()
            .0;
        cda_fund(
            &mut runner,
            P0,
            &[
                ManaType::Black,
                ManaType::Green,
                ManaType::Colorless,
                ManaType::Colorless,
                ManaType::Colorless,
            ],
        );
        runner
            .act(GameAction::ActivateAbility {
                source_id: lord,
                ability_index: index,
            })
            .unwrap();
        let WaitingFor::TargetSelection {
            player,
            pending_cast,
            target_slots,
            ..
        } = &runner.state().waiting_for
        else {
            panic!("target first")
        };
        assert_eq!(*player, P0);
        assert_eq!(pending_cast.ability.source_incarnation, Some(before));
        assert!(target_slots[0]
            .legal_targets
            .contains(&TargetRef::Object(recipient)));
        assert!(target_slots[0].legal_targets.len() > 1);
        runner
            .act(GameAction::ChooseTarget {
                target: TargetRef::Object(recipient),
            })
            .unwrap();
        let WaitingFor::ReplacementChoice {
            player,
            candidate_count,
            ..
        } = runner.state().waiting_for
        else {
            panic!("actual self-exile cost suspension")
        };
        assert_eq!(player, P0);
        assert_eq!(candidate_count, 2);
        assert!(runner.state().pending_cost_move_resume.is_some());
        let saved = serde_json::to_string(runner.state()).unwrap();
        *runner.state_mut() = serde_json::from_str(&saved).unwrap();
        runner
            .act(GameAction::ChooseReplacement { index: 1 })
            .unwrap();
        assert_eq!(runner.state().objects[&lord].zone, Zone::Exile);
        assert_eq!(
            runner
                .state()
                .stack
                .last()
                .unwrap()
                .ability()
                .unwrap()
                .source_incarnation,
            Some(before)
        );
        assert_eq!(
            runner.state().lki_by_incarnation[&lord][&before].power,
            Some(4)
        );
        cda_pt(runner.state(), lord, 3);
        engine::game::zones::move_to_zone(runner.state_mut(), lord, return_zone, &mut vec![]);
        // A later same-id object has a different current value (GY4 or BF3).
        // Remove a foreign member so the newer graveyard object is3 as well.
        if return_zone == Zone::Graveyard {
            let witness = runner.state().players[1].graveyard[0];
            engine::game::zones::move_to_zone(runner.state_mut(), witness, Zone::Hand, &mut vec![]);
        }
        let saved = serde_json::to_string(runner.state()).unwrap();
        *runner.state_mut() = serde_json::from_str(&saved).unwrap();
        for _ in 0..8 {
            if runner.state().stack.is_empty() {
                break;
            }
            assert!(matches!(
                runner.state().waiting_for,
                WaitingFor::Priority { .. }
            ));
            runner.act(GameAction::PassPriority).unwrap();
        }
        assert_ne!(runner.state().objects[&lord].incarnation, before);
        assert_eq!(runner.state().objects[&lord].power, Some(3));
        assert_eq!(
            runner.state().objects[&recipient]
                .counters
                .get(&CounterType::Plus1Plus1),
            Some(&4)
        );
        cda_complete(runner.state());
    }
}

#[test]
fn cda_two_real_varolz_grants_follow_each_source_lifetime() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let gallery = scenario
        .add_artifact_from_oracle(P0, "Mirror Gallery", "The \"legend rule\" doesn't apply.")
        .with_mana_cost(cda_cost(5, vec![]))
        .id();
    let sources = [0, 1].map(|_| {
        scenario
            .add_creature_from_oracle(P0, "Varolz, the Scar-Striped", 2, 2, VAROLZ)
            .as_legendary()
            .with_subtypes(vec!["Troll", "Warrior"])
            .with_mana_cost(cda_cost(
                1,
                vec![ManaCostShard::Black, ManaCostShard::Green],
            ))
            .id()
    });
    let lord = scenario
        .add_creature_to_graveyard(P0, "Lord of Extinction", 0, 0)
        .from_oracle_text(LORD)
        .with_mana_cost(cda_cost(
            3,
            vec![ManaCostShard::Black, ManaCostShard::Green],
        ))
        .id();
    let maro = scenario
        .add_creature_to_graveyard(P0, "Maro", 0, 0)
        .from_oracle_text(MARO)
        .with_mana_cost(cda_cost(
            2,
            vec![ManaCostShard::Green, ManaCostShard::Green],
        ))
        .id();
    scenario.with_graveyard(P1, &["Foreign A", "Foreign B"]);
    let swords = [0, 1].map(|_| {
        scenario
            .add_spell_to_hand_from_oracle(P0, "Swords to Plowshares", true, SWORDS)
            .with_mana_cost(cda_cost(0, vec![ManaCostShard::White]))
            .id()
    });
    scenario.with_cards_in_hand(P0, &["Hand A", "Hand B", "Hand C"]);
    let recipient = scenario.add_creature(P0, "Counter recipient", 2, 2).id();
    let mut runner = scenario.build();
    engine::game::layers::flush_layers(runner.state_mut());
    cda_pt(runner.state(), lord, 4);
    cda_pt(runner.state(), maro, 5);
    for id in [gallery, sources[0], sources[1], lord, maro] {
        cda_full_source(runner.state(), id);
    }
    for id in [lord, maro] {
        assert!(
            !engine::game::casting::activated_ability_definitions(runner.state(), id).is_empty()
        );
    }
    cda_fund(&mut runner, P0, &[ManaType::White]);
    runner.cast(swords[0]).target_object(sources[0]).resolve();
    assert_eq!(runner.state().objects[&sources[1]].zone, Zone::Battlefield);
    let (index, _) = engine::game::casting::activated_ability_definitions(runner.state(), lord)
        .into_iter()
        .find(|(_, ability)| {
            matches!(
                ability.effect.as_ref(),
                engine::types::ability::Effect::PutCounter { .. }
            )
        })
        .unwrap();
    cda_pt(runner.state(), lord, 5);
    cda_complete(runner.state());
    cda_fund(
        &mut runner,
        P0,
        &[
            ManaType::Black,
            ManaType::Green,
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Colorless,
        ],
    );
    runner
        .activate(lord, index)
        .target_object(recipient)
        .resolve();
    assert_eq!(
        runner.state().objects[&recipient]
            .counters
            .get(&CounterType::Plus1Plus1),
        Some(&5)
    );
    cda_complete(runner.state());
    assert!(!engine::game::casting::activated_ability_definitions(runner.state(), maro).is_empty());
    cda_fund(&mut runner, P0, &[ManaType::White]);
    runner.cast(swords[1]).target_object(sources[1]).resolve();
    assert!(engine::game::casting::activated_ability_definitions(runner.state(), maro).is_empty());
    assert_eq!(runner.state().objects[&maro].zone, Zone::Graveyard);
    assert_eq!(
        runner.state().objects[&recipient]
            .counters
            .get(&CounterType::Plus1Plus1),
        Some(&5)
    );
    cda_complete(runner.state());
}

#[test]
fn cda_bloom_removed_target_fizzles_beside_a_real_four_token_control() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let lord = scenario
        .add_creature_to_graveyard(P0, "Lord of Extinction", 0, 0)
        .from_oracle_text(LORD)
        .with_subtypes(vec!["Elemental"])
        .with_mana_cost(cda_cost(
            3,
            vec![ManaCostShard::Black, ManaCostShard::Green],
        ))
        .id();
    scenario.with_graveyard(P1, &["A", "B", "C"]);
    let bloom = scenario
        .add_spell_to_hand_from_oracle(P0, "Morbid Bloom", false, BLOOM)
        .with_mana_cost(cda_cost(
            4,
            vec![ManaCostShard::Black, ManaCostShard::Green],
        ))
        .id();
    let cremate = scenario
        .add_spell_to_hand_from_oracle(
            P1,
            "Cremate",
            true,
            "Exile target card from a graveyard.\nDraw a card.",
        )
        .with_mana_cost(cda_cost(0, vec![ManaCostShard::Black]))
        .id();
    let drawn = scenario.add_card_to_library_top(P1, "Cremate draw witness");
    let mut runner = scenario.build();
    cda_fund(
        &mut runner,
        P0,
        &[
            ManaType::Black,
            ManaType::Green,
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Colorless,
        ],
    );
    cda_fund(&mut runner, P1, &[ManaType::Black]);
    let mut committed = runner.cast(bloom).target_object(lord).commit();
    cda_pt(committed.state(), lord, 4);
    committed.act(GameAction::PassPriority).unwrap();
    let result = committed.cast(cremate).target_object(lord).resolve();
    cda_full_source(result.state(), cremate);
    assert!(result.state().players[1].hand.contains(&drawn));
    assert!(result
        .state()
        .objects
        .values()
        .all(|object| !object.is_token));
    assert_eq!(result.zone_of(lord), Zone::Exile);
    assert_eq!(result.zone_of(bloom), Zone::Graveyard);
    cda_complete(result.state());
    drop(committed);
    let (mut positive, lord, _, _, bloom) = cda_last_graveyard_fixture(false);
    cda_fund(
        &mut positive,
        P0,
        &[
            ManaType::Black,
            ManaType::Green,
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Colorless,
        ],
    );
    positive.cast(bloom).target_object(lord).resolve();
    assert_eq!(
        positive
            .state()
            .objects
            .values()
            .filter(|object| object.is_token)
            .count(),
        4
    );
    cda_complete(positive.state());
}

#[test]
fn cda_switcheroo_illegal_and_same_controller_twins_keep_a_real_exchange_guard() {
    for case in ["exchange", "same", "illegal"] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let crusader = scenario
            .add_creature_from_oracle(P0, "Crusader of Odric", 0, 0, CRUSADER)
            .with_subtypes(vec!["Human", "Soldier"])
            .with_mana_cost(cda_cost(2, vec![ManaCostShard::White]))
            .id();
        let own = scenario.add_creature(P0, "Own A", 2, 2).id();
        scenario.add_creature(P0, "Own B", 2, 2);
        let foreign = scenario.add_creature(P1, "Foreign A", 2, 2).id();
        scenario.add_creature(P1, "Foreign B", 2, 2);
        let exchange = scenario
            .add_spell_to_hand_from_oracle(P0, "Switcheroo", false, EXCHANGE)
            .with_mana_cost(cda_cost(4, vec![ManaCostShard::Blue]))
            .id();
        let swords = scenario
            .add_spell_to_hand_from_oracle(P0, "Swords to Plowshares", true, SWORDS)
            .with_mana_cost(cda_cost(0, vec![ManaCostShard::White]))
            .id();
        let mut runner = scenario.build();
        cda_fund(
            &mut runner,
            P0,
            &[
                ManaType::Blue,
                ManaType::Colorless,
                ManaType::Colorless,
                ManaType::Colorless,
                ManaType::Colorless,
            ],
        );
        let other = if case == "same" { own } else { foreign };
        let mut committed = runner
            .cast(exchange)
            .target_objects(&[crusader, other])
            .commit();
        cda_pt(committed.state(), crusader, 3);
        if case == "illegal" {
            let _ = committed.state_mut().add_mana_to_pool(
                P0,
                ManaUnit::new(ManaType::White, ObjectId(0), false, vec![]),
            );
            committed.cast(swords).target_object(foreign).resolve();
        } else {
            committed.resolve();
        }
        cda_full_source(runner.state(), exchange);
        assert_eq!(
            runner.state().objects[&crusader].controller,
            if case == "exchange" { P1 } else { P0 }
        );
        cda_pt(
            runner.state(),
            crusader,
            if case == "exchange" { 2 } else { 3 },
        );
        assert_eq!(
            runner.state().objects[&other].controller,
            if case == "exchange" || case == "same" {
                P0
            } else {
                P1
            }
        );
        cda_complete(runner.state());
        if case == "exchange" {
            cda_fund(&mut runner, P0, &[ManaType::White]);
            let result = runner.cast(swords).target_object(crusader).resolve();
            result.assert_life_delta(P1, 2);
            result.assert_life_delta(P0, 0);
            cda_pt(result.state(), crusader, 3); // off-BF You is the owner
            cda_complete(result.state());
            engine::game::zones::move_to_zone(
                runner.state_mut(),
                crusader,
                Zone::Hand,
                &mut vec![],
            );
            engine::game::layers::flush_layers(runner.state_mut());
            cda_pt(runner.state(), crusader, 3);
            engine::game::zones::move_to_zone(
                runner.state_mut(),
                crusader,
                Zone::Battlefield,
                &mut vec![],
            );
            engine::game::layers::flush_layers(runner.state_mut());
            cda_pt(runner.state(), crusader, 4);
            assert_eq!(runner.state().objects[&crusader].controller, P0);
            cda_complete(runner.state());
        }
    }
}

#[test]
fn cda_greater_good_discards_the_whole_smaller_hand_after_a_real_draw() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let good = scenario
        .add_enchantment_from_oracle(P0, "Greater Good", GOOD)
        .with_mana_cost(cda_cost(
            2,
            vec![ManaCostShard::Green, ManaCostShard::Green],
        ))
        .id();
    let creature = scenario.add_creature(P0, "One-power sacrifice", 1, 1).id();
    let drawn = scenario.add_card_to_library_top(P0, "Only draw witness");
    let mut runner = scenario.build();
    runner
        .act(GameAction::ActivateAbility {
            source_id: good,
            ability_index: 0,
        })
        .unwrap();
    let WaitingFor::PayCost {
        player,
        choices,
        count,
        ..
    } = &runner.state().waiting_for
    else {
        panic!("ordinary sacrifice")
    };
    assert_eq!(*player, P0);
    assert_eq!(*count, 1);
    assert_eq!(choices, &vec![creature]);
    runner
        .act(GameAction::SelectCards {
            cards: vec![creature],
        })
        .unwrap();
    for _ in 0..8 {
        if matches!(runner.state().waiting_for, WaitingFor::DiscardChoice { .. }) {
            break;
        }
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { .. }
        ));
        runner.act(GameAction::PassPriority).unwrap();
    }
    let WaitingFor::DiscardChoice {
        player,
        count,
        cards,
        ..
    } = &runner.state().waiting_for
    else {
        panic!("discard the smaller hand")
    };
    assert_eq!(*player, P0);
    assert_eq!(*count, 1);
    assert_eq!(cards, &vec![drawn]);
    assert!(runner.state().players[0].library.is_empty());
    runner
        .act(GameAction::SelectCards { cards: vec![drawn] })
        .unwrap();
    assert_eq!(runner.state().objects[&drawn].zone, Zone::Graveyard);
    assert!(runner.state().players[0].hand.is_empty());
    cda_full_source(runner.state(), good);
    cda_complete(runner.state());
}

#[test]
fn cda_swords_all_illegal_target_has_a_paid_nonzero_life_guard() {
    for shroud in [true, false] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        scenario.with_graveyard(P0, &["A", "B"]);
        scenario.with_graveyard(P1, &["C", "D"]);
        let mut creature = scenario.add_creature_from_oracle(P0, "Lord of Extinction", 0, 0, LORD);
        creature
            .with_subtypes(vec!["Elemental"])
            .with_mana_cost(cda_cost(
                3,
                vec![ManaCostShard::Black, ManaCostShard::Green],
            ));
        if shroud {
            creature.with_keyword(engine::types::keywords::Keyword::Shroud);
        }
        let lord = creature.id();
        let swords = scenario
            .add_spell_to_hand_from_oracle(P0, "Swords to Plowshares", true, SWORDS)
            .with_mana_cost(cda_cost(0, vec![ManaCostShard::White]))
            .id();
        let mut runner = scenario.build();
        cda_fund(&mut runner, P0, &[ManaType::White]);
        engine::game::layers::flush_layers(runner.state_mut());
        cda_full_source(runner.state(), swords);
        cda_full_source(runner.state(), lord);
        cda_pt(runner.state(), lord, 4);
        if shroud {
            let card_id = runner.state().objects[&swords].card_id;
            assert!(runner
                .act(GameAction::CastSpell {
                    object_id: swords,
                    card_id,
                    targets: vec![],
                    payment_mode: engine::types::game_state::CastPaymentMode::Auto
                })
                .is_err());
            assert_eq!(runner.state().objects[&lord].zone, Zone::Battlefield);
            assert_eq!(runner.state().players[0].life, 20);
            assert_eq!(runner.state().players[0].mana_pool.total(), 1);
            cda_complete(runner.state());
        } else {
            let result = runner.cast(swords).target_object(lord).resolve();
            result.assert_life_delta(P0, 4);
            cda_pt(result.state(), lord, 5);
            assert_eq!(result.state().players[0].mana_pool.total(), 0);
            cda_complete(result.state());
        }
    }
}

#[test]
fn cda_exile_maro_tracks_zero_during_paid_announcement_then_two_actual_draws() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let maro = scenario
        .add_creature_to_exile(P0, "Maro", 0, 0)
        .from_oracle_text(MARO)
        .with_subtypes(vec!["Elemental"])
        .with_mana_cost(cda_cost(
            2,
            vec![ManaCostShard::Green, ManaCostShard::Green],
        ))
        .id();
    let draw = scenario
        .add_spell_to_hand_from_oracle(P0, "Divination", false, "Draw two cards.")
        .with_mana_cost(cda_cost(2, vec![ManaCostShard::Blue]))
        .id();
    scenario.with_library_top(P0, &["First", "Second"]);
    let mut runner = scenario.build();
    engine::game::layers::flush_layers(runner.state_mut());
    cda_pt(runner.state(), maro, 1);
    cda_fund(
        &mut runner,
        P0,
        &[ManaType::Blue, ManaType::Colorless, ManaType::Colorless],
    );
    let committed = runner.cast(draw).commit();
    cda_pt(committed.state(), maro, 0);
    assert_eq!(committed.state().objects[&maro].zone, Zone::Exile);
    let result = committed.resolve();
    result.assert_hand_drawn(P0, 2);
    cda_pt(result.state(), maro, 2);
    cda_complete(result.state());
}

#[test]
fn cda_oversimplify_redirected_departure_is_excluded_while_current_card_recomputes() {
    use engine::types::ability::{AbilityDefinition, AbilityKind, Effect, ReplacementDefinition};
    use engine::types::replacements::ReplacementEvent;
    let (mut runner, lord, crusader, _, wipes, _, _) = cda_combined_wipe_fixture();
    let redirect: Effect = serde_json::from_value(serde_json::json!({ "type": "ChangeZone", "destination": "Hand", "target": { "type": "SelfRef" } })).unwrap();
    let replacement = ReplacementDefinition::new(ReplacementEvent::Moved)
        .valid_card(TargetFilter::SelfRef)
        .destination_zone(Zone::Exile)
        .execute(AbilityDefinition::new(AbilityKind::Spell, redirect));
    let object = runner.state_mut().objects.get_mut(&lord).unwrap();
    object.replacement_definitions.push(replacement.clone());
    std::sync::Arc::make_mut(&mut object.base_replacement_definitions).push(replacement);
    cda_fund(
        &mut runner,
        P0,
        &[
            ManaType::Green,
            ManaType::Blue,
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Colorless,
        ],
    );
    let result = runner.cast(wipes[0]).resolve();
    assert_eq!(result.zone_of(lord), Zone::Hand);
    cda_pt(result.state(), lord, 5);
    cda_pt(result.state(), crusader, 1);
    cda_fractal(result.state(), P0, 2);
    cda_fractal(result.state(), P1, 7);
    let departure = result
        .state()
        .zone_changes_this_turn
        .iter()
        .find(|record| record.object_id == lord && record.from_zone == Some(Zone::Battlefield))
        .unwrap();
    assert_eq!(departure.to_zone, Zone::Hand);
    assert_eq!(departure.power, Some(7));
    assert_eq!(departure.controller, P0);
    assert_eq!(departure.owner, P1);
    assert_eq!(result.state().players[0].mana_pool.total(), 0);
    cda_complete(result.state());
}
