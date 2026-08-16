//! Momir's Madness format integration tests.
//!
//! These drive the REAL activation + resolution pipeline (`GameAction::
//! ActivateAbility` -> `ChooseX` -> discard cost -> mana payment -> resolve)
//! through `GameRunner`, asserting on state deltas. The primary test
//! (`momir_emblem_creates_creature_token_with_matching_mv`) would fail if the
//! `CreateTokenCopyFromPool` resolver or the emblem grant were reverted.

use std::collections::BTreeMap;
use std::sync::Arc;

use engine::game::deck_loading::momir_emblem_ability;
use engine::game::scenario::GameRunner;
use engine::types::ability::{
    CardSelectionMode, Comparator, Effect, PtValue, QuantityExpr, ResolvedAbility, TargetFilter,
    TypeFilter, TypedFilter,
};
use engine::types::actions::GameAction;
use engine::types::card::CardFace;
use engine::types::card_type::{CardType, CoreType};
use engine::types::format::FormatConfig;
use engine::types::game_state::{GameState, PayCostKind, WaitingFor};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::mana::{ManaCost, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const P0: PlayerId = PlayerId(0);

/// A synthetic creature face with the given name and mana value (paid as
/// generic mana for simplicity).
fn creature_face(name: &str, mana_value: u32) -> CardFace {
    CardFace {
        name: name.to_string(),
        mana_cost: ManaCost::Cost {
            shards: vec![],
            generic: mana_value,
        },
        card_type: CardType {
            supertypes: vec![],
            core_types: vec![CoreType::Creature],
            subtypes: vec!["Beast".to_string()],
        },
        power: Some(PtValue::Fixed(mana_value as i32)),
        toughness: Some(PtValue::Fixed(mana_value as i32)),
        ..Default::default()
    }
}

/// Build a Momir's Madness game state at precombat main with P0 holding priority.
/// The pool is populated directly (mirroring `rehydrate_card_db_metadata`) so
/// the test does not depend on the full card database.
fn momir_state(pool: &[(u32, &str)]) -> (GameState, ObjectId) {
    let mut state = GameState::new(FormatConfig::momir(), 2, 42);
    state.phase = Phase::PreCombatMain;
    state.turn_number = 2;
    state.active_player = P0;
    state.priority_player = P0;
    state.waiting_for = WaitingFor::Priority { player: P0 };

    // Populate the Momir pool directly.
    let mut by_mv: BTreeMap<i32, Vec<String>> = BTreeMap::new();
    let mut faces = std::collections::HashMap::new();
    for (mv, name) in pool {
        by_mv.entry(*mv as i32).or_default().push(name.to_string());
        faces.insert(name.to_lowercase(), creature_face(name, *mv));
    }
    for names in by_mv.values_mut() {
        names.sort();
    }
    state.momir_pool = by_mv;
    state.momir_pool_faces = Arc::new(faces);

    // Grant the Momir emblem to P0.
    let emblem_id = engine::game::effects::create_emblem::grant_emblem(
        &mut state,
        P0,
        Vec::new(),
        Vec::new(),
        vec![momir_emblem_ability()],
    );

    (state, emblem_id)
}

/// Give P0 `amount` colorless mana in their pool (pays generic costs) and one
/// disposable card in hand.
fn fund_and_card(state: &mut GameState, amount: u32) -> ObjectId {
    for _ in 0..amount {
        state.players[0].mana_pool.add(ManaUnit::new(
            ManaType::Colorless,
            ObjectId(0),
            false,
            vec![],
        ));
    }
    // A disposable card to discard for the cost.
    engine::game::zones::create_object(state, CardId(999), P0, "Plains".to_string(), Zone::Hand)
}

/// Drive `GameAction::ActivateAbility` on the emblem with X = `x`, paying the
/// discard with `discard_card` and the mana from the pool. Returns the runner
/// after resolution.
fn activate_emblem(
    state: GameState,
    emblem_id: ObjectId,
    x: u32,
    discard_card: ObjectId,
) -> GameRunner {
    let mut runner = GameRunner::from_state(state);
    runner
        .act(GameAction::ActivateAbility {
            source_id: emblem_id,
            ability_index: 0,
        })
        .expect("activating the Momir emblem must be accepted");

    for _ in 0..32 {
        match &runner.state().waiting_for {
            WaitingFor::ChooseXValue { .. } => {
                runner
                    .act(GameAction::ChooseX { value: x })
                    .expect("ChooseX must be accepted");
            }
            WaitingFor::PayCost {
                kind: PayCostKind::Discard,
                ..
            } => {
                runner
                    .act(GameAction::SelectCards {
                        cards: vec![discard_card],
                    })
                    .expect("discarding to pay the cost must be accepted");
            }
            WaitingFor::ManaPayment { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("finalizing mana payment must be accepted");
            }
            WaitingFor::Priority { .. } => break,
            other => panic!("unexpected WaitingFor during activation: {other:?}"),
        }
    }

    // Resolve the ability off the stack.
    for _ in 0..16 {
        if runner.state().stack.is_empty() {
            break;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("passing priority to resolve the emblem ability must be accepted");
    }
    runner
}

/// PRIMARY DISCRIMINATING TEST. Reverting either the emblem grant or the
/// `CreateTokenCopyFromPool` resolver makes the asserted token absent and this
/// fails. The flipping assertion is `token_count == 1` with mana value 3.
#[test]
fn momir_emblem_creates_creature_token_with_matching_mv() {
    let (mut state, emblem_id) =
        momir_state(&[(3, "Hill Giant"), (2, "Gray Ogre"), (5, "Air Elemental")]);
    let card = fund_and_card(&mut state, 3);

    let runner = activate_emblem(state, emblem_id, 3, card);

    // A creature token with mana value 3 exists on the battlefield.
    let tokens: Vec<&engine::game::game_object::GameObject> = runner
        .state()
        .battlefield
        .iter()
        .filter_map(|id| runner.state().objects.get(id))
        .filter(|o| o.is_token)
        .collect();
    assert_eq!(
        tokens.len(),
        1,
        "exactly one creature token must be created, got {}",
        tokens.len()
    );
    let token = tokens[0];
    assert!(
        token.card_types.core_types.contains(&CoreType::Creature),
        "the token must be a creature"
    );
    assert_eq!(
        token.mana_cost.mana_value(),
        3,
        "the token's mana value must equal the X paid (3), got {}",
        token.mana_cost.mana_value()
    );
    assert_eq!(
        token.name, "Hill Giant",
        "with only one MV-3 creature in the pool, that creature must be copied"
    );

    // The discarded card moved hand -> graveyard.
    assert_eq!(
        runner.state().objects[&card].zone,
        Zone::Graveyard,
        "the cost card must be discarded to the graveyard"
    );
}

/// Determinism: identical seed + setup yields the same chosen creature name.
#[test]
fn momir_selection_is_deterministic_under_seed() {
    let pool = &[(4, "Air Elemental"), (4, "Wind Drake"), (4, "Cloud Sprite")];

    let run_once = || {
        let (mut state, emblem_id) = momir_state(pool);
        let card = fund_and_card(&mut state, 4);
        let runner = activate_emblem(state, emblem_id, 4, card);
        runner
            .state()
            .battlefield
            .iter()
            .filter_map(|id| runner.state().objects.get(id))
            .find(|o| o.is_token)
            .map(|o| o.name.clone())
            .expect("a token must be created")
    };

    assert_eq!(
        run_once(),
        run_once(),
        "same seed + setup must select the same creature"
    );
}

/// Once-per-turn: a second activation in the same turn is rejected.
#[test]
fn momir_emblem_only_once_each_turn() {
    let (mut state, emblem_id) = momir_state(&[(3, "Hill Giant")]);
    let _card = fund_and_card(&mut state, 6);
    // A second discard fodder card for the (rejected) second attempt.
    let card2 = engine::game::zones::create_object(
        &mut state,
        CardId(998),
        P0,
        "Island".to_string(),
        Zone::Hand,
    );

    let mut runner = activate_emblem(state, emblem_id, 3, card2);

    // After one activation this turn, the ability is no longer activatable.
    let legal = engine::game::casting::can_activate_ability_now(runner.state(), P0, emblem_id, 0);
    assert!(
        !legal,
        "CR 602.5b: the once-each-turn emblem ability must be unavailable after one use"
    );

    // Direct re-activation is rejected by the engine.
    let err = runner.act(GameAction::ActivateAbility {
        source_id: emblem_id,
        ability_index: 0,
    });
    assert!(
        err.is_err(),
        "re-activating the once-each-turn emblem ability in the same turn must be rejected"
    );
}

/// Sorcery-speed: the ability is not activatable outside the controller's main
/// phase (CR 307.5 requires main phase + empty stack + priority).
#[test]
fn momir_emblem_is_sorcery_speed() {
    let (mut state, emblem_id) = momir_state(&[(3, "Hill Giant")]);
    // Move to the upkeep step (not a main phase) — sorcery-speed timing fails.
    state.phase = Phase::Upkeep;
    state.waiting_for = WaitingFor::Priority { player: P0 };

    let legal = engine::game::casting::can_activate_ability_now(&state, P0, emblem_id, 0);
    assert!(
        !legal,
        "CR 307.5: a sorcery-speed ability must not be activatable outside the main phase"
    );
}

/// Building-block test: the `CreateTokenCopyFromPool` primitive with
/// `Comparator::LE` exercises the BTreeMap range path (distinct from Momir's
/// EQ keyed lookup) and creates a creature token of MV <= bound.
#[test]
fn create_token_copy_from_pool_le_bound_oko_style() {
    let (mut state, emblem_id) =
        momir_state(&[(2, "Gray Ogre"), (4, "Air Elemental"), (9, "Big Thing")]);
    // Set chosen_x irrelevant; we drive the resolver directly with a Fixed bound.
    let _ = emblem_id;
    let card = fund_and_card(&mut state, 0);
    let _ = card;

    let effect = Effect::CreateTokenCopyFromPool {
        owner: TargetFilter::Controller,
        type_filter: TargetFilter::Any,
        mv: Comparator::LE,
        mv_bound: QuantityExpr::Fixed { value: 8 },
        selection: CardSelectionMode::Random,
        count: QuantityExpr::Fixed { value: 1 },
        tapped: false,
        enters_attacking: false,
    };
    let ability = ResolvedAbility::new(effect, vec![], emblem_id, P0);
    let mut events = Vec::new();
    engine::game::effects::create_token_copy_from_pool::resolve(&mut state, &ability, &mut events)
        .expect("LE-bound pool copy must resolve");

    let token = state
        .battlefield
        .iter()
        .filter_map(|id| state.objects.get(id))
        .find(|o| o.is_token)
        .expect("a token must be created for an MV<=8 pool with eligible creatures");
    assert!(
        token.mana_cost.mana_value() <= 8,
        "LE bound must only copy creatures with mana value <= 8, got {}",
        token.mana_cost.mana_value()
    );
    assert_ne!(
        token.name, "Big Thing",
        "the MV-9 creature must be excluded by the LE-8 bound"
    );
}

/// LOW-1 regression: the effect's `type_filter` ("additional filter applied to
/// the hydrated face") MUST exclude non-matching candidates from the random pool.
/// Pool has three MV-3 creatures sharing one mana value: two `Goblin`s and one
/// `Wizard`. With `type_filter = Subtype(Wizard)` and `selection = Random`, the
/// ONLY eligible candidate is the Wizard, so the created token must be the Wizard
/// regardless of RNG. Before the fix, `type_filter` was only checked for `== None`
/// (never applied to the face), so the random pick ranged over all three and
/// could produce a Goblin.
///
/// Positive correctness check: `token.name == "Pool Wizard"`. The fully
/// deterministic discriminator for LOW-1 is
/// `create_token_copy_from_pool_type_filter_no_match_is_noop` below (this one's
/// revert-detection depends on the RNG happening to pick a Goblin).
#[test]
fn create_token_copy_from_pool_applies_type_filter() {
    let (mut state, emblem_id) = momir_state(&[]);
    // Seed a custom MV-3 pool: two Goblins and one Wizard, all at the same mana
    // value so the comparator alone cannot disambiguate them.
    let mut by_mv: BTreeMap<i32, Vec<String>> = BTreeMap::new();
    by_mv.insert(
        3,
        vec![
            "Pool Goblin A".to_string(),
            "Pool Goblin B".to_string(),
            "Pool Wizard".to_string(),
        ],
    );
    let mut faces = std::collections::HashMap::new();
    for name in ["Pool Goblin A", "Pool Goblin B"] {
        let mut face = creature_face(name, 3);
        face.card_type.subtypes = vec!["Goblin".to_string()];
        faces.insert(name.to_lowercase(), face);
    }
    let mut wizard = creature_face("Pool Wizard", 3);
    wizard.card_type.subtypes = vec!["Wizard".to_string()];
    faces.insert("pool wizard".to_string(), wizard);
    state.momir_pool = by_mv;
    state.momir_pool_faces = Arc::new(faces);

    let effect = Effect::CreateTokenCopyFromPool {
        owner: TargetFilter::Controller,
        type_filter: TargetFilter::Typed(TypedFilter::new(TypeFilter::Subtype(
            "Wizard".to_string(),
        ))),
        mv: Comparator::EQ,
        mv_bound: QuantityExpr::Fixed { value: 3 },
        selection: CardSelectionMode::Random,
        count: QuantityExpr::Fixed { value: 1 },
        tapped: false,
        enters_attacking: false,
    };
    let ability = ResolvedAbility::new(effect, vec![], emblem_id, P0);
    let mut events = Vec::new();
    engine::game::effects::create_token_copy_from_pool::resolve(&mut state, &ability, &mut events)
        .expect("type-filtered pool copy must resolve");

    let token = state
        .battlefield
        .iter()
        .filter_map(|id| state.objects.get(id))
        .find(|o| o.is_token)
        .expect("a token must be created when an eligible Wizard exists");
    assert_eq!(
        token.name, "Pool Wizard",
        "type_filter = Subtype(Wizard) must exclude the Goblins from the pool"
    );
}

/// LOW-1 corollary: when NO candidate satisfies `type_filter`, the random pool is
/// empty and no token is created (CR 609.3 do-as-much-as-possible), rather than
/// the filter being ignored and a non-matching creature being copied.
#[test]
fn create_token_copy_from_pool_type_filter_no_match_is_noop() {
    let (mut state, emblem_id) = momir_state(&[(3, "Gray Ogre")]); // subtype Beast
    let effect = Effect::CreateTokenCopyFromPool {
        owner: TargetFilter::Controller,
        type_filter: TargetFilter::Typed(TypedFilter::new(TypeFilter::Subtype(
            "Wizard".to_string(),
        ))),
        mv: Comparator::EQ,
        mv_bound: QuantityExpr::Fixed { value: 3 },
        selection: CardSelectionMode::Random,
        count: QuantityExpr::Fixed { value: 1 },
        tapped: false,
        enters_attacking: false,
    };
    let ability = ResolvedAbility::new(effect, vec![], emblem_id, P0);
    let mut events = Vec::new();
    engine::game::effects::create_token_copy_from_pool::resolve(&mut state, &ability, &mut events)
        .expect("a type_filter with no matches must be a clean no-op");

    let token_count = state
        .battlefield
        .iter()
        .filter_map(|id| state.objects.get(id))
        .filter(|o| o.is_token)
        .count();
    assert_eq!(
        token_count, 0,
        "no token may be created when type_filter excludes every candidate"
    );
}

/// Empty-pool: a mana value with no creatures creates no token and does not panic.
#[test]
fn create_token_copy_from_pool_empty_candidates_is_noop() {
    let (mut state, emblem_id) = momir_state(&[(2, "Gray Ogre")]);

    let effect = Effect::CreateTokenCopyFromPool {
        owner: TargetFilter::Controller,
        type_filter: TargetFilter::Any,
        mv: Comparator::EQ,
        mv_bound: QuantityExpr::Fixed { value: 7 }, // no MV-7 creatures
        selection: CardSelectionMode::Random,
        count: QuantityExpr::Fixed { value: 1 },
        tapped: false,
        enters_attacking: false,
    };
    let ability = ResolvedAbility::new(effect, vec![], emblem_id, P0);
    let mut events = Vec::new();
    engine::game::effects::create_token_copy_from_pool::resolve(&mut state, &ability, &mut events)
        .expect("empty candidate set must be a clean no-op");

    let token_count = state
        .battlefield
        .iter()
        .filter_map(|id| state.objects.get(id))
        .filter(|o| o.is_token)
        .count();
    assert_eq!(
        token_count, 0,
        "no token may be created from an empty pool key"
    );
}

/// Format config: Momir's Madness is 20 life, 60-card deck, command zone enabled.
#[test]
fn momir_format_config_values() {
    let config = FormatConfig::momir();
    assert_eq!(config.starting_life, 20);
    assert_eq!(config.deck_size, 60);
    assert!(config.command_zone, "Momir needs the command zone enabled");
    assert!(!config.uses_commander);
}

/// grant_emblem installs a command-zone-activatable ability on the emblem.
#[test]
fn grant_emblem_installs_command_zone_activated_ability() {
    let mut state = GameState::new(FormatConfig::momir(), 2, 42);
    let emblem_id = engine::game::effects::create_emblem::grant_emblem(
        &mut state,
        P0,
        Vec::new(),
        Vec::new(),
        vec![momir_emblem_ability()],
    );
    let emblem = &state.objects[&emblem_id];
    assert!(emblem.is_emblem);
    assert_eq!(emblem.zone, Zone::Command);
    assert_eq!(emblem.abilities.len(), 1);
    assert_eq!(
        emblem.abilities[0].activation_zone,
        Some(Zone::Command),
        "the Momir ability must be command-zone-activatable"
    );
}

/// MP serialization: the pool fields are `#[serde(skip)]` — they must not appear
/// in the serialized form and are rebuilt on rehydrate.
#[test]
fn momir_pool_is_not_serialized() {
    let (state, _emblem) = momir_state(&[(3, "Hill Giant"), (5, "Air Elemental")]);
    assert!(!state.momir_pool.is_empty(), "precondition: pool populated");

    let json = serde_json::to_string(&state).expect("serialize Momir state");
    assert!(
        !json.contains("momir_pool"),
        "momir_pool / momir_pool_faces must be #[serde(skip)] and absent from the wire form"
    );

    let de: GameState = serde_json::from_str(&json).expect("deserialize Momir state");
    assert!(
        de.momir_pool.is_empty(),
        "a deserialized peer starts with an empty pool (rebuilt on rehydrate)"
    );
    // format_config survives, so the peer knows to rebuild the Momir pool.
    assert_eq!(
        de.format_config.format,
        engine::types::format::GameFormat::Momir
    );
}

/// CR 111.5 guard: a synthetic instant/sorcery pool entry yields no token.
#[test]
fn create_token_copy_from_pool_instant_sorcery_guard() {
    let mut state = GameState::new(FormatConfig::momir(), 2, 42);
    let mut faces = std::collections::HashMap::new();
    let mut sorcery_face = creature_face("Sorcery Sham", 3);
    sorcery_face.card_type.core_types = vec![CoreType::Sorcery];
    sorcery_face.power = None;
    sorcery_face.toughness = None;
    faces.insert("sorcery sham".to_string(), sorcery_face);
    let mut by_mv: BTreeMap<i32, Vec<String>> = BTreeMap::new();
    by_mv.insert(3, vec!["Sorcery Sham".to_string()]);
    state.momir_pool = by_mv;
    state.momir_pool_faces = Arc::new(faces);

    let emblem_id = engine::game::effects::create_emblem::grant_emblem(
        &mut state,
        P0,
        Vec::new(),
        Vec::new(),
        vec![momir_emblem_ability()],
    );

    let effect = Effect::CreateTokenCopyFromPool {
        owner: TargetFilter::Controller,
        type_filter: TargetFilter::Any,
        mv: Comparator::EQ,
        mv_bound: QuantityExpr::Fixed { value: 3 },
        selection: CardSelectionMode::Random,
        count: QuantityExpr::Fixed { value: 1 },
        tapped: false,
        enters_attacking: false,
    };
    let ability = ResolvedAbility::new(effect, vec![], emblem_id, P0);
    let mut events = Vec::new();
    engine::game::effects::create_token_copy_from_pool::resolve(&mut state, &ability, &mut events)
        .expect("instant/sorcery guard must be a clean no-op");

    let token_count = state
        .battlefield
        .iter()
        .filter_map(|id| state.objects.get(id))
        .filter(|o| o.is_token)
        .count();
    assert_eq!(
        token_count, 0,
        "CR 111.5: a token that's a copy of an instant/sorcery is not created"
    );
}
