//! DESIGN STEP 4 (CR 732.2a ∞-pile display) — REAL 4-player game acceptance test.
//!
//! Loads the user's ACTUAL live 4-player Commander game state (the turn-2 dump), captured at
//! the exact `WaitingFor::LoopShortcut` offer for the Witherbloom, the Balancer + Sprout Swarm
//! object-growth infinite. Drives the real APNAP accept path through `apply()` and asserts that
//! `materialize_object_growth_shortcut` snapshots the winning controller's TAPPED fodder
//! Saprolings as `GameState::unbounded_loop_pile`, that `derive_views` projects it, and that it
//! survives a serde round-trip (the user's "reloaded post-accept shows no pile" bug — now fixed
//! for POST-FIX saves).
//!
//! USER DIRECTIVE (memory: real-game fixtures, not synthetic): this fixture LOADS a real
//! 4-player complete-deck saved game-state dump and drives from it — NOT a synthetic
//! `GameScenario` (synthetic tests went green while the live 4p game failed). The dump is the
//! real game: 4 seats at 40 life, full ~91-92-card libraries, 10 permanents, the intact
//! `last_loop_action_sequence` recast context (`Recast{from_zone: Hand, uses_buyback: Used}`,
//! `convoke: Convoke`), and `loop_detection: Interactive`. The dump was captured AT the offer,
//! which is strictly more faithful than a build-fresh reconstruction (it IS the failing moment).
//! `deck_pools` (registration metadata the accept→materialize drive never reads) is trimmed from
//! the committed fixture; the real decks remain fully present as in-play library objects.
//!
//! REVERT-PROBE (documented, non-vacuous): commenting out the `register_unbounded_loop_pile`
//! call in `materialize_object_growth_shortcut` (game/engine.rs) leaves `unbounded_loop_pile`
//! empty ⇒ the positive pile assertion (1), the derived-view assertion (2), and both round-trip
//! assertions (3) all FLIP to fail. The render half (∞ vs ×N) is covered by the frontend vitest
//! (`GroupedPermanent.test.tsx`, `battlefieldGrouping.test.ts`); the MEASURED tapped count here
//! is 4, so the collapsed/staggered ∞ path is exercised, and the vitest also covers the
//! single-member branch (SHOULD-FIX #1).

use engine::analysis::decision_template::IterationCount;
use engine::analysis::loop_check::ShortcutResponse;
use engine::database::card_db::CardDatabase;
use engine::game::deck_loading::{
    create_object_from_card_face, load_and_hydrate_decks, resolve_deck_list, DeckList,
};
use engine::game::derived_views::derive_views;
use engine::game::engine::{apply, start_game};
use engine::game::layers::flush_layers;
use engine::game::scenario::GameRunner;
use engine::game::zones::{add_to_zone, create_object, remove_from_zone};
use engine::types::actions::{GameAction, MulliganChoice};
use engine::types::card_type::CoreType;
use engine::types::format::FormatConfig;
use engine::types::game_state::{GameState, LoopDetectionMode, WaitingFor};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::mana::ManaColor;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;
use std::collections::BTreeSet;

use super::support::shared_card_db;

const P0: PlayerId = PlayerId(0);
const P1: PlayerId = PlayerId(1);
const P2: PlayerId = PlayerId(2);
const P3: PlayerId = PlayerId(3);

/// The real live game state, captured at the CR 732.2a object-growth `LoopShortcut` offer.
// Fixtures are stored gzip-compressed (18x smaller); inflate at first use.
fn gunzip_fixture(gz: &[u8]) -> String {
    use std::io::Read;
    let mut json = String::new();
    flate2::read::GzDecoder::new(gz)
        .read_to_string(&mut json)
        .expect("fixture .json.gz must inflate to UTF-8 JSON");
    json
}

static OFFER_STATE: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| {
    gunzip_fixture(include_bytes!(
        "../fixtures/combo_infinite_pile_4p_offer.json.gz"
    ))
});

/// P0's tapped, vanilla (no counters, no damage) Saprolings — the NON-CIRCULAR oracle for the
/// ∞ pile. Derived by a NAME + vanilla filter INDEPENDENT of the engine's content-eq authority
/// (`fodder_content_eq`), so matching it cross-checks the engine rather than itself.
fn p0_tapped_vanilla_saprolings(state: &GameState) -> BTreeSet<ObjectId> {
    state
        .battlefield
        .iter()
        .copied()
        .filter(|id| {
            state.objects.get(id).is_some_and(|o| {
                o.controller == P0
                    && o.tapped
                    && o.name == "Saproling"
                    && o.counters.is_empty()
                    && o.damage_marked == 0
            })
        })
        .collect()
}

/// Drive the APNAP accept: P0 (the proposer) declares, then every prompted opponent accepts
/// in turn order until the protocol closes back to ordinary priority.
fn drive_all_accept(state: &mut GameState) {
    apply(
        state,
        P0,
        GameAction::DeclareShortcut {
            count: IterationCount::Fixed(1),
            template: None,
        },
    )
    .expect("P0 (proposer) declares the object-growth shortcut");
    while let WaitingFor::RespondToShortcut { player, .. } = state.waiting_for.clone() {
        apply(
            state,
            player,
            GameAction::RespondToShortcut {
                response: ShortcutResponse::Accept,
            },
        )
        .expect("each living opponent accepts");
    }
}

#[test]
fn real_4p_object_growth_accept_writes_infinite_pile() {
    let mut state: GameState = serde_json::from_str(&OFFER_STATE)
        .expect("the real 4p offer dump must deserialize into the current GameState");

    // Precondition: the loaded state IS the real object-growth offer, recast context intact.
    assert!(
        matches!(state.waiting_for, WaitingFor::LoopShortcut { proposer, .. } if proposer == P0),
        "fixture precondition: at the CR 732.2a LoopShortcut offer for P0, got {:?}",
        state.waiting_for
    );
    assert!(
        !state.last_loop_action_sequence.is_empty(),
        "the offer must carry the intact recast context the pile re-derive drives"
    );

    // The non-circular oracle: exactly the 4 tapped vanilla Saprolings P0 controls in the
    // real game (MEASURED — the render path is collapsed/staggered ∞, not single-member).
    let oracle = p0_tapped_vanilla_saprolings(&state);
    assert_eq!(
        oracle.len(),
        4,
        "measured: P0 controls 4 tapped Saprolings in the real game state"
    );

    drive_all_accept(&mut state);

    // The protocol closed cleanly back to ordinary priority (CR 800.4a).
    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { .. }),
        "after all accept, materialize hands priority back, got {:?}",
        state.waiting_for
    );

    // (1) POSITIVE — the ∞ pile is exactly P0's tapped Saprolings (non-empty), matching the
    // independent name+vanilla oracle. This is the `register_unbounded_loop_pile` revert target.
    let pile = state
        .unbounded_loop_pile
        .get(&P0)
        .expect("accepting the object-growth loop must write P0's ∞ pile");
    assert_eq!(
        *pile, oracle,
        "the ∞ pile is exactly P0's tapped Saprolings (non-circular name+vanilla oracle)"
    );
    assert!(
        !pile.is_empty(),
        "the pile is non-empty (positive reach-guard for the negatives below)"
    );

    // (i) untapped P0 Saprolings excluded.
    for id in [406u64, 408, 409, 410].map(ObjectId) {
        assert!(
            !pile.contains(&id),
            "untapped P0 Saproling {id:?} must not be in the ∞ pile"
        );
    }
    // (ii) non-fodder permanents excluded: Witherbloom (P0, tapped) + Exotic Orchard (P3 land).
    assert!(
        !pile.contains(&ObjectId(401)),
        "Witherbloom (non-fodder, tapped, P0) must be excluded"
    );
    // (iii) an OPPONENT's permanent (P3's Exotic Orchard) is excluded — the real board carries
    // no opponent creature, so this land is the driven-test opponent-exclusion witness.
    // Opponent tapped *fodder* exclusion (a content-equal opponent Saproling) is covered
    // discriminatingly by the resource.rs unit test `tapped_fodder_members_returns_only_...`.
    assert!(
        !pile.contains(&ObjectId(313)),
        "Exotic Orchard (opponent P3's permanent) must be excluded"
    );

    // (2) DERIVED — derive_views projects the pile (battlefield-filtered, public board state).
    let derived = derive_views(&state, Some(P0));
    let derived_set: BTreeSet<ObjectId> = derived.unbounded_pile.iter().copied().collect();
    assert_eq!(
        derived_set, oracle,
        "derive_views().unbounded_pile must equal the pile set (battlefield-filtered)"
    );

    // (3) ROUND-TRIP — the pile survives serialize → deserialize (the "reloaded post-accept
    // shows no pile" fix for POST-FIX saves) AND derive_views re-exposes it.
    let json = serde_json::to_string(&state).expect("serialize the post-accept state");
    let reloaded: GameState = serde_json::from_str(&json).expect("reload the post-accept state");
    assert_eq!(
        reloaded.unbounded_loop_pile.get(&P0),
        Some(&oracle),
        "the ∞ pile survives a serde round-trip (post-fix saves reload it)"
    );
    let reloaded_set: BTreeSet<ObjectId> = derive_views(&reloaded, Some(P0))
        .unbounded_pile
        .iter()
        .copied()
        .collect();
    assert_eq!(
        reloaded_set, oracle,
        "the reloaded post-accept state re-projects the ∞ pile through derive_views"
    );
}

// ─────────────────────────── BUILD-FRESH acceptance ───────────────────────────
//
// Current-code reconstruction (USER DIRECTIVE — the second acceptance path). Bootstraps a
// REAL 4-player Commander game from the SAME four decks (name-resolved via the card DB, so no
// CardFace drift), drives the 4-player mulligan-KEEP, advances to P0's precombat main, debug-
// constructs the Witherbloom + Sprout Swarm object-growth board, and drives the FULL
// cast → CR 732.2a offer → APNAP accept path the load-dump test skips. Asserts the same ∞ pile
// wire (materialize → unbounded_loop_pile → derive_views → serde round-trip) on current code.

static DECKLIST_4P: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| {
    gunzip_fixture(include_bytes!(
        "../fixtures/combo_infinite_pile_decklist_4p.json.gz"
    ))
});
const RNG_SEED: u64 = 4_133_150_290_317_995;

const SPROUT_SWARM: &str = "Sprout Swarm";
const WITHERBLOOM: &str = "Witherbloom, the Balancer";

/// Place a real DB card by name into `zone` for `player` (mirrors the mana-engine test's
/// `place_on_battlefield`; `CreateCard` needs WASM name resolution so pure `apply()` can't).
fn place_card(
    state: &mut GameState,
    player: PlayerId,
    name: &str,
    zone: Zone,
    db: &CardDatabase,
) -> ObjectId {
    let face = db
        .get_face_by_name(name)
        .unwrap_or_else(|| panic!("card '{name}' not found in the card DB"));
    let id = create_object_from_card_face(state, face, player);
    remove_from_zone(state, id, Zone::Library, player);
    add_to_zone(state, id, zone, player);
    state.objects.get_mut(&id).unwrap().zone = zone;
    id
}

/// Create one vanilla green 1/1 Saproling creature token on `owner`'s battlefield — the convoke
/// fodder. Content-equal (name + 1/1, what `object_content_eq` compares) to the Saproling the
/// Sprout Swarm recast mints, so the pile re-derive matches these; green Creature so convoke can
/// tap it and Witherbloom's affinity counts it.
fn create_saproling(state: &mut GameState, owner: PlayerId) -> ObjectId {
    let card_id = CardId(state.next_object_id);
    let id = create_object(
        state,
        card_id,
        owner,
        "Saproling".to_string(),
        Zone::Battlefield,
    );
    let o = state.objects.get_mut(&id).unwrap();
    o.power = Some(1);
    o.toughness = Some(1);
    o.base_power = Some(1);
    o.base_toughness = Some(1);
    o.color = vec![ManaColor::Green];
    o.base_color = vec![ManaColor::Green];
    o.is_token = true;
    o.card_types.core_types = vec![CoreType::Creature];
    o.card_types.subtypes = vec!["Saproling".to_string()];
    o.summoning_sick = false;
    id
}

/// Bootstrap a real 4-player Commander game from the four extracted decks and return it at P0's
/// precombat main with priority (loop detection Interactive). Panics with a specific message at
/// the first bootstrap step that fails to reach a drivable P0 main (STOP-AND-RETURN signal).
fn bootstrap_4p_game(db: &CardDatabase) -> GameState {
    let decklist: DeckList =
        serde_json::from_str(&DECKLIST_4P).expect("the 4p decklist fixture must deserialize");
    let payload = resolve_deck_list(db, &decklist);
    let mut state = GameState::new(FormatConfig::commander(), 4, RNG_SEED);
    load_and_hydrate_decks(&mut state, &payload, Some(db));

    start_game(&mut state);
    assert!(
        matches!(state.waiting_for, WaitingFor::MulliganDecision { .. }),
        "start_game with real libraries must open the mulligan (got {:?})",
        state.waiting_for
    );
    // Drive 4-player mulligan KEEP — the prompt carries all living seats simultaneously.
    for pid in [P0, P1, P2, P3] {
        let _ = apply(
            &mut state,
            pid,
            GameAction::MulliganDecision {
                choice: MulliganChoice::Keep,
            },
        );
    }
    assert!(
        !matches!(state.waiting_for, WaitingFor::MulliganDecision { .. }),
        "all four seats KEEP must complete the mulligan (got {:?})",
        state.waiting_for
    );
    // Sanity: it's a real complete game — every seat has a full library and a real hand.
    for p in &state.players {
        assert!(
            !p.library.is_empty() && !p.hand.is_empty(),
            "seat {:?} must have a real library + opening hand",
            p.id
        );
    }

    // Advance to P0's precombat main with priority (the sanctioned rhys `start_main_phase`
    // direct-set — avoids multi-phase orchestration; the mulligan above proved the real start).
    state.turn_number = 2;
    state.phase = Phase::PreCombatMain;
    state.active_player = P0;
    state.priority_player = P0;
    state.waiting_for = WaitingFor::Priority { player: P0 };
    state.loop_detection = LoopDetectionMode::Interactive;
    state
}

#[test]
fn build_fresh_4p_cast_offer_accept_writes_infinite_pile() {
    let Some(db) = shared_card_db() else {
        return; // card DB unavailable in this environment — skip like the other DB-backed tests.
    };

    let state = bootstrap_4p_game(db);
    let mut runner = GameRunner::from_state(state);

    // Debug-construct the object-growth board on P0: Witherbloom (grants affinity) + four green
    // Saproling fodder + Sprout Swarm in hand. Four fodder matches the proven `sprout_swarm_
    // scenario` count so affinity fully covers {1}{G} + Buyback {3}; convoke pays the {G}.
    let _witherbloom = place_card(runner.state_mut(), P0, WITHERBLOOM, Zone::Battlefield, db);
    // Tap Witherbloom, FAITHFUL to the captured real state: in the committed turn-2 dump
    // (`combo_infinite_pile_4p_offer.json`) object 401 "Witherbloom, the Balancer" (controller 0)
    // is `tapped: true` at the exact LoopShortcut offer — the load-dump test asserts the same
    // `ObjectId(401)`. This is not arbitrary rigging: the player had already tapped it. It also
    // matters mechanically — real Witherbloom is B/G, so an UNTAPPED one is the lowest-ObjectId
    // green convoke candidate, and the deterministic detection replay (`select_convoke_taps`,
    // lowest-id-per-color) would tap the stable Witherbloom instead of a fodder Saproling,
    // drifting the stable partition and suppressing the offer (see the KNOWN-GAP note at
    // `select_convoke_taps`). A tapped Witherbloom is removed from the convoke candidate set so
    // the replay taps a Saproling (the sustainable loop the player actually plays). Affinity
    // counts creatures "you control" (CR 702.41a) — a tapped creature is still controlled, so a
    // tapped Witherbloom still grants and self-counts for the cost reduction.
    runner
        .state_mut()
        .objects
        .get_mut(&_witherbloom)
        .unwrap()
        .tapped = true;
    let fodder: Vec<ObjectId> = (0..4)
        .map(|_| create_saproling(runner.state_mut(), P0))
        .collect();
    let sprout = place_card(runner.state_mut(), P0, SPROUT_SWARM, Zone::Hand, db);
    // Flush layers so Witherbloom's affinity static is live before the cast computes cost.
    flush_layers(runner.state_mut());

    // Cast Sprout Swarm paying Buyback {3} and convoke-tapping one green Saproling for the {G}.
    let outcome = runner
        .cast(sprout)
        .accept_optional()
        .convoke_with(&[fodder[0]])
        .commit()
        .resolve();
    assert!(
        matches!(
            outcome.final_waiting_for(),
            WaitingFor::LoopShortcut { proposer, .. } if *proposer == P0
        ),
        "the object-growth cast must OFFER a LoopShortcut to P0, got {:?}",
        outcome.final_waiting_for()
    );

    // Drive the APNAP accept (P0 declares; the three living opponents accept).
    drive_all_accept(runner.state_mut());
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "after all accept, materialize hands priority back, got {:?}",
        runner.state().waiting_for
    );

    // The non-circular oracle: P0's tapped vanilla Saprolings AFTER the real cast (dynamic —
    // the build-fresh tapped count is whatever the single real convoke-cast produced).
    let oracle = p0_tapped_vanilla_saprolings(runner.state());
    assert!(
        !oracle.is_empty(),
        "the real convoke-cast must have left ≥1 tapped Saproling (positive reach-guard)"
    );

    // (1) POSITIVE — the ∞ pile equals P0's tapped Saprolings. `register_unbounded_loop_pile`
    // revert target.
    let pile = runner
        .state()
        .unbounded_loop_pile
        .get(&P0)
        .expect("accepting the object-growth loop must write P0's ∞ pile");
    assert_eq!(
        *pile, oracle,
        "the ∞ pile is exactly P0's tapped Saprolings (build-fresh, current code)"
    );

    // (i) every UNtapped P0 Saproling is excluded.
    let untapped_p0_saprolings: Vec<ObjectId> = runner
        .state()
        .battlefield
        .iter()
        .copied()
        .filter(|id| {
            runner
                .state()
                .objects
                .get(id)
                .is_some_and(|o| o.controller == P0 && !o.tapped && o.name == "Saproling")
        })
        .collect();
    assert!(
        !untapped_p0_saprolings.is_empty()
            && untapped_p0_saprolings.iter().all(|id| !pile.contains(id)),
        "untapped P0 Saprolings must exist and all be excluded from the pile"
    );
    // (ii) the non-fodder Witherbloom is excluded.
    assert!(
        !pile.contains(&_witherbloom),
        "Witherbloom (non-fodder) must be excluded from the pile"
    );

    // (2) DERIVED — derive_views projects the pile.
    let derived_set: BTreeSet<ObjectId> = derive_views(runner.state(), Some(P0))
        .unbounded_pile
        .iter()
        .copied()
        .collect();
    assert_eq!(
        derived_set, oracle,
        "derive_views().unbounded_pile must equal the pile set (build-fresh)"
    );

    // (3) ROUND-TRIP — the pile survives a serde round-trip on a current-code save.
    let json = serde_json::to_string(runner.state()).expect("serialize the post-accept state");
    let reloaded: GameState = serde_json::from_str(&json).expect("reload the post-accept state");
    assert_eq!(
        reloaded.unbounded_loop_pile.get(&P0),
        Some(&oracle),
        "the ∞ pile survives a serde round-trip (build-fresh, current code)"
    );
}
