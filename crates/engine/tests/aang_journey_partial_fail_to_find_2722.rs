//! Regression (issue #2722): a `MatchEachFilter` library search must not
//! DEADLOCK (game freeze) when the library can't supply one card per filter
//! slot.
//!
//! Aang's Journey, kicked, searches the library for "a basic land card and a
//! Shrine card". A deck with basics but NO Shrine cannot fill the Shrine slot.
//! Pre-fix, the `SearchChoice` submission guard required EXACTLY `count` cards
//! satisfying `MatchEachFilter`, and the AI candidate generator only enumerated
//! `count`-sized combinations — so against a no-Shrine library every candidate
//! was filtered out, the legal-action set was EMPTY, and the game froze with no
//! submittable action.
//!
//! CR 701.23b: when searching a hidden zone for cards with a stated quality, a
//! player "isn't required to find some or all of those cards even if they're
//! present" — a stated-quality search may ALWAYS fail to find some or all of the
//! described cards (CR 701.23b), distinct from a pure-quantity search which must
//! find as many as possible (CR 701.23d).
//!
//! This drives the REAL pipeline: `search_library::resolve` parks the live
//! `WaitingFor::SearchChoice`, the AI candidate enumerator (`candidate_actions_
//! broad`) must return a non-empty legal-action set, and the real engine
//! submission handler (`apply_as_current` → `engine_resolution_choices`) accepts
//! both a single-basic partial pick and the empty full-fail-to-find. The
//! `SearchLibrary → ChangeZone(Hand) → Shuffle` continuation moves the found
//! card to hand and shuffles, mirroring the real card's resolution chain.
//!
//! This is NOT an AST-shape test. Pre-fix it fails at the `candidate_actions_
//! broad` non-empty assertion (empty legal-action set == freeze) and at the
//! submission `.expect(...)` (the guard rejects the short pick).

use engine::ai_support::candidate_actions_broad;
use engine::game::effects::resolve_ability_chain;
use engine::game::zones::create_object;
use engine::types::ability::{
    Effect, FilterProp, QuantityExpr, ResolvedAbility, SearchSelectionConstraint, TargetFilter,
    TypeFilter, TypedFilter,
};
use engine::types::actions::GameAction;
use engine::types::card_type::{CoreType, Supertype};
use engine::types::events::{GameEvent, PlayerActionKind};
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const P0: PlayerId = PlayerId(0);

/// A basic land in P0's library.
fn add_basic_land(state: &mut GameState, card: u64, name: &str) -> ObjectId {
    let id = create_object(state, CardId(card), P0, name.to_string(), Zone::Library);
    let obj = state.objects.get_mut(&id).unwrap();
    obj.card_types.core_types = vec![CoreType::Land];
    obj.card_types.supertypes.push(Supertype::Basic);
    id
}

/// The kicked Aang's-Journey search: find one basic land AND one Shrine, then
/// move the found card(s) to hand and shuffle. `count: 2` (two filter slots).
fn aangs_journey_search(source: ObjectId) -> ResolvedAbility {
    let basic_filter = TargetFilter::Typed(TypedFilter {
        type_filters: vec![TypeFilter::Land],
        controller: None,
        properties: vec![FilterProp::HasSupertype {
            value: Supertype::Basic,
        }],
    });
    let shrine_filter = TargetFilter::Typed(TypedFilter {
        type_filters: vec![TypeFilter::Subtype("Shrine".to_string())],
        controller: None,
        properties: vec![],
    });

    // Continuation link 2: shuffle the searcher's library.
    let shuffle = ResolvedAbility::new(
        Effect::Shuffle {
            target: TargetFilter::Controller,
        },
        vec![],
        source,
        P0,
    );
    // Continuation link 1: move the found card(s) to hand.
    let mut to_hand = ResolvedAbility::new(
        Effect::ChangeZone {
            origin: Some(Zone::Library),
            destination: Zone::Hand,
            target: TargetFilter::Any,
            owner_library: false,
            enter_transformed: false,
            enters_under: None,
            enter_tapped: Default::default(),
            enters_attacking: false,
            up_to: false,
            enter_with_counters: vec![],
            conditional_enter_with_counters: vec![],
            face_down_profile: None,
        },
        vec![],
        source,
        P0,
    );
    to_hand.sub_ability = Some(Box::new(shuffle));

    // Head: search for [basic, Shrine].
    let mut search = ResolvedAbility::new(
        Effect::SearchLibrary {
            filter: TargetFilter::Any,
            count: QuantityExpr::Fixed { value: 2 },
            reveal: false,
            target_player: None,
            selection_constraint: SearchSelectionConstraint::MatchEachFilter {
                filters: vec![basic_filter, shrine_filter],
            },
            split: None,
            source_zones: vec![Zone::Library],
        },
        vec![],
        source,
        P0,
    );
    search.sub_ability = Some(Box::new(to_hand));
    search
}

/// Build a no-Shrine library: three basics. Returns (state, source, basics).
fn setup_no_shrine_library() -> (GameState, ObjectId, Vec<ObjectId>) {
    let mut state = GameState::new_two_player(42);
    state.active_player = P0;

    let source = create_object(
        &mut state,
        CardId(99),
        P0,
        "Aang's Journey".to_string(),
        Zone::Battlefield,
    );

    // No Shrine in the deck — only basics, so the Shrine slot can never fill.
    let basics = vec![
        add_basic_land(&mut state, 1, "Forest"),
        add_basic_land(&mut state, 2, "Island"),
        add_basic_land(&mut state, 3, "Mountain"),
    ];

    (state, source, basics)
}

/// CR 701.23b: a kicked Aang's-Journey search against a no-Shrine library must
/// NOT freeze. The live SearchChoice must offer a non-empty legal-action set
/// (the empty fail-to-find decline plus single-basic partial picks), and the
/// real submission handler must accept a single-basic pick — moving that basic
/// to hand and shuffling.
#[test]
fn match_each_filter_partial_find_does_not_freeze_and_submits_partial() {
    let (mut state, source, basics) = setup_no_shrine_library();
    let forest = basics[0];
    let ability = aangs_journey_search(source);

    let mut events = Vec::new();
    resolve_ability_chain(&mut state, &ability, &mut events, 0).expect("search resolves");

    // The search parks at a SearchChoice over the three basics (the matching
    // candidate set). Pre-fix this state is unsubmittable.
    let WaitingFor::SearchChoice { cards, count, .. } = &state.waiting_for else {
        panic!("expected SearchChoice, got {:?}", state.waiting_for);
    };
    assert_eq!(*count, 2, "two filter slots → count 2");
    assert_eq!(
        cards.len(),
        3,
        "all three basics match the search filter union"
    );

    // CR 701.23b: the legal-action set must be NON-EMPTY (anti-freeze). Pre-fix
    // it is empty because every count-sized combination of basics fails the
    // MatchEachFilter constraint (no Shrine for slot 2).
    let candidates = candidate_actions_broad(&state);
    assert!(
        !candidates.is_empty(),
        "deadlock: the SearchChoice produced no legal actions (game freeze)"
    );

    // The empty fail-to-find decline must be among the legal actions.
    assert!(
        candidates
            .iter()
            .any(|c| matches!(&c.action, GameAction::SelectCards { cards } if cards.is_empty())),
        "the empty fail-to-find decline must be a legal action"
    );
    // A single-basic partial pick must be among the legal actions.
    assert!(
        candidates.iter().any(|c| matches!(
            &c.action,
            GameAction::SelectCards { cards } if cards.len() == 1 && basics.contains(&cards[0])
        )),
        "a single-basic partial pick must be a legal action"
    );

    // CR 701.23b — AI-VALIDATED path (issue #2722 Finding 1). The broad
    // enumerator above bypasses `cheap_reject_candidate`; the AI actually
    // consumes `validated_candidate_actions` (FilterPipeline →
    // BasicLegalityFilter → cheap_reject_candidate). If the SearchChoice
    // cheap-reject arm uses up_to-only logic, it rejects the legal empty/partial
    // pick → the validated set is EMPTY → the AI freezes at the same
    // SearchChoice. This assertion FAILS pre-Finding-1-fix and PASSES after.
    let validated = engine::ai_support::validated_candidate_actions(&state);
    assert!(
        !validated.is_empty(),
        "deadlock: the AI-validated legal-action set is empty (game freeze)"
    );
    assert!(
        validated
            .iter()
            .any(|c| matches!(&c.action, GameAction::SelectCards { cards } if cards.is_empty())),
        "the empty fail-to-find decline must survive the AI-validated pipeline"
    );

    // Submit a single-basic pick through the REAL submission handler.
    let result = engine::game::apply_as_current(
        &mut state,
        GameAction::SelectCards {
            cards: vec![forest],
        },
    )
    .expect("submitting a single-basic partial pick must succeed (fail-to-find for the Shrine)");

    // The continuation moved the Forest to hand and shuffled the library.
    assert_eq!(
        state.objects[&forest].zone,
        Zone::Hand,
        "the found basic must move to hand"
    );
    assert!(
        !state.players[0].library.contains(&forest),
        "the found basic must leave the library"
    );
    assert!(
        result.events.iter().any(|e| matches!(
            e,
            GameEvent::PlayerPerformedAction {
                action: PlayerActionKind::ShuffledLibrary,
                ..
            }
        )),
        "the library must be shuffled after the search"
    );
}

/// CR 701.23b: the FULL fail-to-find (empty selection) is also legal and
/// completes — the searcher finds neither described card, then shuffles.
#[test]
fn match_each_filter_full_fail_to_find_empty_selection_completes() {
    let (mut state, source, _basics) = setup_no_shrine_library();
    let ability = aangs_journey_search(source);

    let mut events = Vec::new();
    resolve_ability_chain(&mut state, &ability, &mut events, 0).expect("search resolves");
    assert!(
        matches!(state.waiting_for, WaitingFor::SearchChoice { .. }),
        "expected SearchChoice, got {:?}",
        state.waiting_for
    );

    // Submit the empty fail-to-find through the REAL submission handler.
    let result =
        engine::game::apply_as_current(&mut state, GameAction::SelectCards { cards: vec![] })
            .expect("submitting the empty full fail-to-find must succeed");

    // No card left the library, but the library was still shuffled.
    assert_eq!(
        state.players[0].library.len(),
        3,
        "no card is found, so all three basics remain in the library"
    );
    assert!(
        result.events.iter().any(|e| matches!(
            e,
            GameEvent::PlayerPerformedAction {
                action: PlayerActionKind::ShuffledLibrary,
                ..
            }
        )),
        "the library must be shuffled even on a full fail-to-find"
    );
    assert!(
        !matches!(state.waiting_for, WaitingFor::SearchChoice { .. }),
        "the search must have completed (no lingering SearchChoice)"
    );
}
