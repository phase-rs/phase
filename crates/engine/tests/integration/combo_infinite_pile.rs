// engine-citation-gate: symbol anchors only
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
use engine::game::derived_views::{
    derive_views, CollapseCertainty, FamilyCollapseState, UnboundedFamily,
};
use engine::game::engine::{apply, start_game};
use engine::game::layers::{flush_layers, mark_layers_full};
use engine::game::scenario::{GameRunner, GameScenario};
use engine::game::zones::{add_to_zone, create_object, remove_from_zone};
use engine::types::ability::{Effect, TriggerDefinition};
use engine::types::actions::{GameAction, MulliganChoice};
use engine::types::card_type::CoreType;
use engine::types::format::FormatConfig;
use engine::types::game_state::{
    GameState, LoopCollapseAxis, LoopDetectionMode, PayableResource, PersistentAxisMaterialization,
    WaitingFor,
};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::mana::ManaColor;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;
use std::collections::BTreeSet;

use super::support::shared_card_db;

/// The `DerivedViews` channels both client wire goldens are lifted from, declared ONCE and
/// referenced by path from `kilo_live_offer_from_real_dump` so the two emitters cannot drift.
/// Previously each file hard-coded its own copy of these four names with only a comment coupling
/// them: `filter_map` silently DROPS a name that matches no field, and each file's drift compare
/// then reads a committed golden written by the same typo, so both sides omit the channel and
/// agree with themselves. One shared array makes an edit land on both emitters at once.
///
/// Neither frame carries all four (this file's has no `counter_display`; kilo's has no
/// `unbounded_pile`), so each non-vacuity guard asserts this set MINUS the one name its frame
/// legitimately lacks — which is what makes the union of the two guards span all four by
/// construction rather than by comment.
pub(crate) const WIRE_GOLDEN_CHANNELS: [&str; 4] = [
    "unbounded_pile",
    "unbounded_resources",
    "counter_display",
    "unbounded_families",
];

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

/// The real live game state, captured at ordinary priority with Witherbloom UNTAPPED — the
/// failing-playtest configuration where the object-growth offer did NOT surface (the untapped,
/// lower-ObjectId B/G cost-reducer suppressed the CR 732.2a detection replay).
static UNTAPPED_PRECAST_STATE: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| {
    gunzip_fixture(include_bytes!(
        "../fixtures/combo_infinite_pile_4p_untapped_precast.json.gz"
    ))
});

/// Count P0's Saprolings on the battlefield (fodder), tapped or not — the reach-guard oracle
/// for "the cast resolved and made one more Saproling".
fn count_battlefield_saprolings(state: &GameState) -> usize {
    state
        .battlefield
        .iter()
        .filter(|id| state.objects.get(id).is_some_and(|o| o.name == "Saproling"))
        .count()
}

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

/// Drive the APNAP accept at the harness default of one cycle. CR 732.2c makes the
/// accepted count BINDING on the boundary collapse prompt, so any test that later submits
/// a larger N must declare that N here — use [`drive_all_accept_n`].
fn drive_all_accept(state: &mut GameState) {
    drive_all_accept_n(state, 1);
}

/// Drive the APNAP accept at `n`: P0 (the proposer) declares `Fixed(n)`, then every
/// prompted opponent accepts in turn order until the protocol closes back to ordinary
/// priority. CR 732.2c: `n` is the count the table agrees to, so it bounds the CR 500.5
/// boundary collapse prompt — a test collapsing to N must accept at ≥ N.
fn drive_all_accept_n(state: &mut GameState, n: u32) {
    apply(
        state,
        P0,
        GameAction::DeclareShortcut {
            count: IterationCount::Fixed(n),
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
    //
    // This accept ALSO scheduled a finite `TokensCreated` collapse, but the engine DEFERS applying
    // it to the CR 500.5 boundary, while advancing to the proposal's ending point (CR 732.2c). No token has
    // been minted yet: the `oracle` set below is the tapped fodder P0 ALREADY controlled, and the ∞
    // mark over it is live, so the pile stays PROJECTED. The scheduling does not change the pile
    // set, which is why the same `oracle` comparison runs directly on the real post-accept state.
    //
    // REVERT-PROBE (RP-1): restore the `collapse_scheduled(controller, &TokensCreated) { continue; }`
    // guard in `derive_views`' pile loop ⇒ THIS `assert_eq!` fails with an empty `left` while the
    // store assertions (1)/(i)/(ii)/(iii) above stay green.
    let derived = derive_views(&state, Some(P0));
    let derived_set: BTreeSet<ObjectId> = derived.unbounded_pile.iter().copied().collect();

    // Cross-seam wire pin, PART 1 — compute + (optionally) REGENERATE. Provenance: every
    // key/value below is ENGINE-EMITTED (`serde_json::to_value(&derive_views(..))`). The three ∞
    // keys are lifted BY NAME from the real serialized DerivedViews so unrelated derived-view churn
    // cannot move this golden, while the field names and value encodings — the part the TS mirror
    // must match — stay engine-authored.
    //
    // The WRITE deliberately precedes every ∞ assertion in this fn, and the drift COMPARE
    // deliberately follows them: a revert probe that reds one of those assertions must still be
    // able to regenerate the client goldens with `UPDATE_WIRE_GOLDEN=1`, or the client-side half of
    // that probe (RP-1b, RP-2) is unreachable. An assert panic aborts the test.
    //
    // DETERMINISM: `counter_display` is a std `HashMap<ObjectId, ObjectCounterDisplay>`
    // (derived_views.rs) — the VALUE is a pre-partitioned row set, not a bare counter-type list —
    // but `serde_json::Map` is BTreeMap-backed (serde_json has no `preserve_order` feature in this
    // workspace — see Cargo.lock), so `to_value` re-sorts every map key. Measured byte-identical
    // across independent test processes. No normalization needed.
    let wire = serde_json::to_value(&derived).expect("derived views serialize");
    let golden: serde_json::Map<String, serde_json::Value> = WIRE_GOLDEN_CHANNELS
        .into_iter()
        .filter_map(|k| wire.get(k).map(|v| (k.to_string(), v.clone())))
        .collect();
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../client/src/test/fixtures/unbounded-token-wire.json"
    );
    if std::env::var_os("UPDATE_WIRE_GOLDEN").is_some() {
        // `client/src/test/fixtures/` may not exist yet; `fs::write` does not create parents.
        std::fs::create_dir_all(
            std::path::Path::new(path)
                .parent()
                .expect("golden has a parent"),
        )
        .expect("create the client wire-golden directory");
        std::fs::write(
            path,
            format!("{}\n", serde_json::to_string_pretty(&golden).unwrap()),
        )
        .expect("write the wire golden");
    }

    assert_eq!(
        derived_set, oracle,
        "derive_views().unbounded_pile must equal the pile set (battlefield-filtered)"
    );

    // NON-VACUITY GUARD for the key list above, and it sits HERE — below the WRITE — under this
    // emitter's own stated rule, because it reads `golden`, which is derived from `derived`.
    // `filter_map` DROPS a name that matches no `DerivedViews` field, and the drift compare below
    // then reads a committed file the same typo wrote — so both sides omit the channel and the
    // compare agrees with itself. Asserting the exact key SET turns a mistyped name into a RED.
    // `BTreeSet` so this does not depend on which container backs `serde_json::Map`.
    //
    // PER-FILE RESIDUAL, CLOSED BY THE PAIR: this frame legitimately carries no `counter_display`,
    // and a name a frame never populates is indistinguishable from a mistyped one from inside that
    // frame. `kilo_live_offer_from_real_dump`'s twin guard covers `counter_display` (and this file
    // covers the `unbounded_pile` its frame lacks). The union spans all four BY CONSTRUCTION: both
    // guards are `WIRE_GOLDEN_CHANNELS` minus the one name their own frame lacks, so a name added
    // to the shared array reds whichever frame does not carry it instead of being silently dropped.
    let channels: BTreeSet<&str> = golden.keys().map(String::as_str).collect();
    let mut expected = BTreeSet::from(WIRE_GOLDEN_CHANNELS);
    expected.remove("counter_display");
    assert_eq!(
        channels, expected,
        "the golden key list names a field `DerivedViews` does not have, or this frame stopped \
         carrying one it must: a mistyped name is dropped silently and the drift compare below \
         then agrees with itself. Check every name against `DerivedViews`."
    );

    // Cross-seam wire pin, PART 2 — the drift COMPARE (see PART 1 for why it sits here).
    let committed: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(path).expect("committed wire golden"))
            .unwrap();
    assert_eq!(
        serde_json::Value::Object(golden),
        committed,
        "the client's wire golden drifted from engine output — re-run with UPDATE_WIRE_GOLDEN=1"
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
    // The stash round-trips too, so the reloaded state's collapse is still SCHEDULED — and the ∞
    // pile stays PROJECTED while it is, for the same reason as (2) above. The scheduling does not
    // change the pile set, which is why the same `oracle` comparison now runs directly on the real
    // reloaded post-accept state instead of on a stash-cleared clone.
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

// ───────────────── UNTAPPED-Witherbloom PRIMARY discriminator (real dump) ─────────────────
//
// USER DIRECTIVE (memory: combo-detector-must-fire-in-real-games / real-game-fixtures-not-
// synthetic): the acceptance bar for this fix is that a REAL 4-player game with an UNTAPPED
// green cost-reducer actually surfaces the CR 732.2a object-growth offer in live play. This
// LOADS the user's ACTUAL failed-playtest dump (turn-2, ordinary priority, Witherbloom UNTAPPED,
// `last_loop_action_sequence` armed for Sprout Swarm 402) and drives the REAL cast through the
// harness `apply()` path. Pre-fix (lowest-ObjectId Canonical detection replay) the offer was
// SUPPRESSED — the replay tapped the lower-id Witherbloom (a stable-partition permanent) instead
// of a fodder Saproling, drifting `loop_states_cover_modulo_fodder_growth`'s `tapped` compare.
//
// REVERT-PROBE (MEASURED, non-vacuous): reverting `resolve_pin(ConvokeTaps)` back to
// `ConvokeTapOrder::Canonical` (or deleting the `DetectionFodderFirst` sort arm in
// `select_convoke_taps`) FLIPS the final `LoopShortcut{proposer:P0}` assertion to `Priority{P0}`
// (no offer). The buyback-return + Saproling-+1 reach-guard holds in BOTH modes (the LIVE cast
// resolves identically; only the clone-drive DETECTION differs), so it proves the cast reached
// the detector — the offer assertion is therefore not vacuous.

#[test]
fn real_4p_untapped_witherbloom_sprout_swarm_offers_object_growth_loop() {
    let state: GameState = serde_json::from_str(&UNTAPPED_PRECAST_STATE)
        .expect("the real untapped-precast 4p dump must deserialize into the current GameState");

    // ── Preconditions: the loaded state IS the real failing configuration ──
    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { player } if player == P0),
        "fixture precondition: ordinary priority for P0 (pre-cast), got {:?}",
        state.waiting_for
    );
    assert!(
        state.pending_cast.is_none(),
        "fixture precondition: no cast is in progress yet"
    );
    let witherbloom = ObjectId(401);
    let w = state
        .objects
        .get(&witherbloom)
        .expect("Witherbloom present");
    assert_eq!(w.name, "Witherbloom, the Balancer");
    assert!(
        !w.tapped,
        "fixture precondition: Witherbloom is UNTAPPED (the bug trigger)"
    );
    assert!(
        !w.is_token,
        "fixture precondition: Witherbloom is a nontoken engine permanent"
    );
    let sprout = ObjectId(402);
    assert_eq!(
        state
            .objects
            .get(&sprout)
            .map(|o| (o.name.as_str(), o.zone)),
        Some(("Sprout Swarm", Zone::Hand)),
        "fixture precondition: Sprout Swarm is in P0's hand"
    );
    // The untapped fodder Saprolings are green tokens with HIGHER ObjectIds than Witherbloom —
    // the exact divergence condition (fodder-first must beat lowest-id to reach the fodder).
    let untapped_fodder: Vec<ObjectId> = [403u64, 404, 406].map(ObjectId).to_vec();
    for id in &untapped_fodder {
        let o = state.objects.get(id).expect("fodder Saproling present");
        assert_eq!(o.name, "Saproling", "{id:?} is a Saproling");
        assert!(
            o.is_token && !o.tapped && o.controller == P0,
            "fixture: {id:?} is an untapped P0 fodder token"
        );
        assert!(
            id.0 > witherbloom.0,
            "fixture: fodder {id:?} id must exceed Witherbloom's {witherbloom:?} (divergence condition)"
        );
    }
    let saprolings_before = count_battlefield_saprolings(&state);
    assert_eq!(
        saprolings_before, 4,
        "fixture: P0 controls 4 Saprolings before the cast (403/404/405/406)"
    );

    // ── Drive the REAL Sprout Swarm cast (accept buyback + convoke one fodder Saproling for {G}) ──
    let mut runner = GameRunner::from_state(state);
    let outcome = runner
        .cast(sprout)
        .accept_optional()
        .convoke_with(&[untapped_fodder[0]])
        .commit()
        .resolve();

    // Positive reach-guard (true in BOTH modes ⇒ the negative revert-probe is non-vacuous): the
    // live cast resolved — buyback returned Sprout Swarm to hand and made one more Saproling.
    assert_eq!(
        outcome.zone_of(sprout),
        Zone::Hand,
        "buyback must return Sprout Swarm to P0's hand (reach-guard: the cast resolved)"
    );
    assert_eq!(
        count_battlefield_saprolings(outcome.state()),
        saprolings_before + 1,
        "the first iteration created exactly one more Saproling (reach-guard: +1 fodder)"
    );

    // ── DISCRIMINATOR: the object-growth offer FIRES (revert-probe → Priority{P0}, MEASURED) ──
    assert!(
        matches!(
            outcome.final_waiting_for(),
            WaitingFor::LoopShortcut { proposer, .. } if *proposer == P0
        ),
        "untapped Witherbloom + fodder-first detection MUST surface the CR 732.2a LoopShortcut \
         offer to P0, got {:?}",
        outcome.final_waiting_for()
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
    // `ObjectId(401)`. This is not arbitrary rigging: the player had already tapped it. The
    // UNTAPPED configuration (where B/G Witherbloom is the lowest-ObjectId green convoke
    // candidate) is now handled by the `DetectionFodderFirst` tap order and is covered by its
    // own real-dump discriminator `real_4p_untapped_witherbloom_sprout_swarm_offers_object_
    // growth_loop` above; here we simply reproduce the captured tapped state. Affinity counts
    // creatures "you control" (CR 702.41a) — a tapped creature is still controlled, so a tapped
    // Witherbloom still grants and self-counts for the cost reduction.
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
    // The raw `create_object`/`add_to_zone` scaffolding above bypasses the ETB dirty-marking
    // that `move_to_zone` performs in real play, so `layers_dirty` is Clean and a bare
    // `flush_layers` would be a no-op. Mark full first so the flush re-evaluates layers and
    // rebuilds `static_mode_presence` — otherwise Witherbloom's affinity-granting
    // `CastWithKeyword` static is invisible to the presence-gated grant scan (CR 604.1) and
    // the {4} generic cannot be paid.
    mark_layers_full(runner.state_mut());
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
    //
    // The accept also scheduled a finite `TokensCreated` collapse, but the engine DEFERS applying
    // it to the CR 500.5 boundary (advancing to the proposal's ending point, CR 732.2c), so nothing is
    // minted yet and the ∞ pile stays PROJECTED while it is merely scheduled. The scheduling does
    // not change the pile set, which is why the same `oracle` comparison now runs directly on the
    // real post-accept state.
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

// ─────────────────────── Basalt Monolith + Power Artifact ───────────────────────
//
// REAL 4-player playtest dump acceptance (user-flagged 2026-07-18): Basalt Monolith taps for
// {C}{C}{C}; Power Artifact reduces its {3} untap to {1}; the loop nets ONLY colorless mana.
// The loop detector correctly recorded a SINGLE `ResourceAxis::Mana(Colorless)` certificate
// for P0 (the dump proves the writer is correct). The debug/loop refill
// (`mana_payment::refill_infinite_mana`) must top the pool up with COLORLESS ONLY — the pre-fix
// body fabricated 100 of ALL SIX colors, which both violates CR 106.1b + CR 106.4 (colors are
// not interchangeable; only mana an ability produced enters the pool) and let the player
// illegally pay colored pips ({W}/{U}/…) from an infinite-COLORLESS engine.
//
// Per memory [real-game-fixtures-not-synthetic]: this LOADS the real 4p dump (`deck_pools` —
// registration metadata this refill never reads — trimmed to `[]` to keep the fixture lean,
// exactly as the sibling `combo_infinite_pile_4p_offer.json` fixture does).
//
// REVERT-PROBE (non-vacuous): restoring the pre-fix body (iterate `INFINITE_MANA_TYPES` instead
// of the recorded colors) refills 100 of every color ⇒ the "0 of every non-colorless color"
// assertions below FLIP to fail. The positive colorless==100 assertion is the reach-guard that
// proves the refill actually ran on P0, so the negatives are not vacuous.

static BASALT_INFINITE_COLORLESS_STATE: std::sync::LazyLock<String> =
    std::sync::LazyLock::new(|| {
        gunzip_fixture(include_bytes!(
            "../fixtures/basalt_power_artifact_infinite_colorless.json.gz"
        ))
    });

#[test]
fn real_4p_basalt_power_artifact_refills_colorless_only() {
    use engine::analysis::resource::ResourceAxis;
    use engine::game::mana_payment::refill_infinite_mana;
    use engine::types::mana::ManaType;

    let mut state: GameState = serde_json::from_str(&BASALT_INFINITE_COLORLESS_STATE)
        .expect("the real Basalt+Power Artifact dump must deserialize into the current GameState");

    // Precondition: the loop detector recorded EXACTLY one mana axis — colorless — for P0.
    let p0_axes = state
        .unbounded_resources
        .get(&P0)
        .expect("P0 must be flagged unbounded in the real dump");
    assert_eq!(
        *p0_axes,
        BTreeSet::from([ResourceAxis::Mana(ManaType::Colorless)]),
        "fixture precondition: P0's only recorded mana axis is Colorless (Basalt + Power Artifact)"
    );

    // Drop the buggy pre-existing all-six pollution — THAT all-six pool is the output under
    // repair. After the fix, the refill re-mints only what the recorded certificate names.
    let p0_idx = state
        .players
        .iter()
        .position(|p| p.id == P0)
        .expect("P0 present in the loaded state");
    state.players[p0_idx].mana_pool.clear();

    refill_infinite_mana(&mut state);

    let count_of = |color: ManaType| {
        state.players[p0_idx]
            .mana_pool
            .mana
            .iter()
            .filter(|u| u.color == color)
            .count()
    };
    // POSITIVE reach-guard: colorless IS topped up to the cap (proves the refill ran on P0).
    assert_eq!(
        count_of(ManaType::Colorless),
        100,
        "colorless refilled to the cap (100) for the colorless-only loop"
    );
    // DISCRIMINATOR: no colored mana is fabricated — the pre-fix all-six body FLIPS these.
    for color in [
        ManaType::White,
        ManaType::Blue,
        ManaType::Black,
        ManaType::Red,
        ManaType::Green,
    ] {
        assert_eq!(
            count_of(color),
            0,
            "{color:?} must NOT be fabricated — the loop produces only colorless (CR 106.1b/106.4)"
        );
    }
}

// ─────────────── PART 2: CR 732.2a boundary finite-resolution (TOKEN collapse) ───────────────
//
// USER DIRECTIVE (memory: real-game-fixtures-not-synthetic / combo-detector-must-fire-in-real-
// games): these LOAD the real 4p dumps and DRIVE the REAL production path (`apply(..PassPriority)`
// per priority holder) to the phase/step boundary. T1 is the primary token payoff; T2 is the
// matched mana NEGATIVE discriminator.

/// Every battlefield Saproling P0 controls (tapped or not) — the mint oracle.
/// PR-7 v4 (CR 732.2a) — the OBSERVED-growth DRIVE path: a loop whose growing axis is observed is
/// collapsed by ONE `DriveSequence` that REPLAYS the captured period N times through real `apply()`
/// at the boundary (observers fire each cycle), NOT a batched N×δ. Real 4p offer dump → real accept
/// → graft the `DriveSequence` the observed accept route emits over the REAL captured recast period
/// → real boundary → `apply(SubmitPayAmount{3})`. The replay re-casts the real Sprout Swarm buyback
/// period 3× and mints exactly 3 real Saproling tokens (one per driven cycle), and the collapsed
/// axes cash out.
///
/// This drives the `drive_persistent_axis_collapse` production seam through the real `apply()`
/// pipeline — the serde round-trip test only proves the stash payload survives; the routing unit
/// test only proves the accept route CHOOSES DriveSequence. REVERT-PROBE (discriminating): stub the
/// `drive_persistent_axis_collapse(..)` call in the `DriveSequence` submit arm to a no-op ⇒ 0 tokens
/// mint ⇒ assertion (1) FLIPS (base + 0 ≠ base + 3). MEASURED: N=3 ⇒ +3 Saprolings.
#[test]
fn real_4p_observed_drive_sequence_replays_captured_period_n_times() {
    let mut state: GameState = serde_json::from_str(&OFFER_STATE)
        .expect("the real 4p offer dump must deserialize into the current GameState");
    let seq = state.last_loop_action_sequence.clone();
    assert!(
        !seq.is_empty(),
        "the offer carries the real recast period the DriveSequence replays"
    );
    drive_all_accept_n(&mut state, 3);

    // An OBSERVED loop's accept registers ONE DriveSequence over the whole loop (all axes) instead
    // of the batched Tokens/Counters/Life. Emulate that route: drop the batched token stash the
    // accept wrote for THIS (unobserved) fixture and graft the DriveSequence the observed route
    // would emit, carrying the SAME ∞ axes the token loop marked.
    let collapsed_axes: Vec<_> = state
        .unbounded_resources
        .get(&P0)
        .expect("the accepted loop marked P0's ∞ axes")
        .iter()
        .cloned()
        .collect();
    state.pending_unbounded_materialization.clear();
    state.register_pending_materialization(
        P0,
        PersistentAxisMaterialization::DriveSequence {
            sequence: seq,
            collapsed_axes: collapsed_axes.clone(),
        },
    );

    // W2 — THE CERTAINTY-VS-FAMILY DISCRIMINATOR. This stash is a `DriveSequence`, the one
    // materialization kind with NO non-push exit, so its `tokens` family is `Committed` — while
    // `unbounded-token-wire.json`, whose SAME `tokens` family comes from a BATCHED `Tokens` stash,
    // is `Conditional`. Same family, same seat, opposite certainty: the badge is deciding on the
    // stash KIND, not on the axis it names.
    let tokens_state = derive_views(&state, None)
        .unbounded_families
        .into_iter()
        .find(|f| f.player == P0 && f.family == UnboundedFamily::Tokens)
        .map(|f| f.state);
    assert_eq!(
        tokens_state,
        Some(FamilyCollapseState::Scheduled {
            certainty: CollapseCertainty::Committed,
            prompted: Some(P0),
        }),
        "a DriveSequence replays real cycles and cannot park, so its tokens family is Committed \
         (∞→N) — contrast the batched Tokens stash behind unbounded-token-wire.json, which is \
         Conditional (∞→?)"
    );

    drive_priority_to_next_boundary(&mut state);
    assert!(
        matches!(
            state.waiting_for,
            WaitingFor::PayAmountChoice { player, resource: PayableResource::LoopCollapse { .. }, .. }
                if player == P0
        ),
        "the boundary prompts P0 for the DriveSequence LoopCollapse count, got {:?}",
        state.waiting_for
    );

    let saps_before = p0_saproling_ids(&state).len();
    apply(&mut state, P0, GameAction::SubmitPayAmount { amount: 3 })
        .expect("P0 submits the finite DriveSequence collapse count");

    // (1) DISCRIMINATOR: the DriveSequence REPLAYED the captured recast period 3× through real
    //     apply(), minting exactly 3 real Saproling tokens (one per driven cycle).
    assert_eq!(
        p0_saproling_ids(&state).len(),
        saps_before + 3,
        "SubmitPayAmount{{3}} replays the captured period 3× ⇒ 3 real Saprolings (stub drive ⇒ 0)"
    );
    // (2) the collapsed ∞ axes cash out; Priority restored.
    assert!(
        collapsed_axes.iter().all(|ax| !state
            .unbounded_resources
            .get(&P0)
            .is_some_and(|a| a.contains(ax))),
        "the DriveSequence collapses its ∞ axes"
    );
    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { .. }),
        "the boundary fixpoint restores Priority, got {:?}",
        state.waiting_for
    );
}

fn p0_saproling_ids(state: &GameState) -> BTreeSet<ObjectId> {
    state
        .battlefield
        .iter()
        .copied()
        .filter(|id| {
            state
                .objects
                .get(id)
                .is_some_and(|o| o.controller == P0 && o.name == "Saproling")
        })
        .collect()
}

/// Drive the REAL production path — `apply(.., PassPriority)` for the actual priority
/// holder each beat — until the current phase/step ends and the `enter_phase → drain`
/// boundary runs. Returns as soon as the boundary surfaces a non-Priority prompt (e.g.
/// the LoopCollapse `PayAmountChoice`) OR the phase advances back to a Priority window
/// (the mana-negative case). Bounded so a wedged state fails loudly instead of hanging.
fn drive_priority_to_next_boundary(state: &mut GameState) {
    let start_phase = state.phase;
    for _ in 0..64 {
        let WaitingFor::Priority { player } = state.waiting_for.clone() else {
            return; // a boundary prompt (or other non-Priority wait) already surfaced
        };
        apply(state, player, GameAction::PassPriority)
            .expect("pass priority to advance toward the next phase boundary");
        if !matches!(state.waiting_for, WaitingFor::Priority { .. }) {
            return; // the phase transition surfaced a prompt (LoopCollapse, etc.)
        }
        if state.phase != start_phase {
            return; // crossed a boundary with no prompt (mana-negative case)
        }
    }
    panic!("drive_priority_to_next_boundary: no phase boundary reached within 64 passes");
}

/// T1 (TOKEN, PRIMARY payoff): a real accepted object-growth loop, at the next phase
/// boundary, prompts the controller for a finite N via `PayableResource::LoopCollapse`,
/// mints N tapped 1/1 green Saproling tokens, cashes out the ∞ status, and does NOT
/// re-prompt.
///
/// REVERT-PROBE (non-vacuous): with the §7 boundary collapse pass removed (or on
/// pre-Part-2 code) `drive_priority_to_next_boundary` surfaces NO `PayAmountChoice` and
/// mints ZERO tokens → assertions (1), (2), and (3) all FLIP. Positive reach-guards (the
/// stash-present assert after accept + the ≥1-token mint) prove non-vacuity.
#[test]
fn real_4p_object_growth_boundary_collapse_mints_finite_tokens() {
    use engine::analysis::resource::ResourceAxis;

    let mut state: GameState = serde_json::from_str(&OFFER_STATE)
        .expect("the real 4p offer dump must deserialize into the current GameState");
    assert!(
        matches!(state.waiting_for, WaitingFor::LoopShortcut { proposer, .. } if proposer == P0),
        "fixture precondition: at the CR 732.2a LoopShortcut offer for P0, got {:?}",
        state.waiting_for
    );

    drive_all_accept_n(&mut state, 5);

    // Reach-guard (accept-capture, §1): accepting the object-growth loop stashed the
    // fodder's copiable profile for P0. Non-vacuity anchor for the negatives below.
    assert!(
        state.pending_unbounded_materialization.contains_key(&P0),
        "accepting the object-growth loop must stash P0's fodder materialization profile"
    );
    // Part 1 preserved: the ∞ TokensCreated axis is marked (zero objects minted yet).
    assert!(
        state
            .unbounded_resources
            .get(&P0)
            .is_some_and(|a| a.contains(&ResourceAxis::TokensCreated)),
        "the accepted object-growth loop marks the TokensCreated ∞ axis"
    );

    let before = p0_saproling_ids(&state);
    assert_eq!(
        before.len(),
        8,
        "MEASURED: P0 controls 8 Saprolings pre-collapse (4 tapped ∞-pile + 4 untapped)"
    );

    drive_priority_to_next_boundary(&mut state);

    // (1) PROMPT — the boundary surfaces the LoopCollapse pay-amount to the controller.
    assert!(
        matches!(
            state.waiting_for,
            WaitingFor::PayAmountChoice { player, resource: PayableResource::LoopCollapse { .. }, .. }
                if player == P0
        ),
        "the phase boundary must prompt P0 for the LoopCollapse count, got {:?}",
        state.waiting_for
    );

    // (2) MINT — SubmitPayAmount{5} mints exactly 5 NEW tapped 1/1 green Saproling tokens.
    apply(&mut state, P0, GameAction::SubmitPayAmount { amount: 5 })
        .expect("P0 submits the finite loop-collapse count");
    let after = p0_saproling_ids(&state);
    assert_eq!(
        after.len(),
        before.len() + 5,
        "SubmitPayAmount{{5}} mints exactly 5 more Saprolings for P0"
    );
    let minted: Vec<ObjectId> = after.difference(&before).copied().collect();
    assert_eq!(minted.len(), 5, "exactly 5 newly-created Saproling ids");
    for id in &minted {
        let o = state.objects.get(id).expect("minted token present");
        assert!(o.is_token, "minted {id:?} is a token");
        assert!(o.tapped, "minted {id:?} enters tapped");
        assert_eq!(o.power, Some(1), "minted {id:?} has power 1");
        assert_eq!(o.toughness, Some(1), "minted {id:?} has toughness 1");
        assert_eq!(o.color, vec![ManaColor::Green], "minted {id:?} is green");
    }

    // (3) CASH-OUT — the ∞ token status ends: axis, stash, and pile all gone.
    assert!(
        !state.unbounded_resources.contains_key(&P0),
        "collapsing the token loop cashes out the ∞ TokensCreated axis"
    );
    assert!(
        !state.pending_unbounded_materialization.contains_key(&P0),
        "the materialization stash is consumed"
    );
    assert!(
        !state.unbounded_loop_pile.contains_key(&P0),
        "the token ∞ pile is cleared (display collapses from ∞ to ×N)"
    );

    // (4) CLEAN RESUME + NO RE-PROMPT.
    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { .. }),
        "after the mint the boundary fixpoint restores Priority, got {:?}",
        state.waiting_for
    );
    drive_priority_to_next_boundary(&mut state);
    assert!(
        !matches!(
            state.waiting_for,
            WaitingFor::PayAmountChoice {
                resource: PayableResource::LoopCollapse { .. },
                ..
            }
        ),
        "the cashed-out loop must NOT re-prompt at the next boundary, got {:?}",
        state.waiting_for
    );
}

/// STACK-SAFETY (Bug B): minting a large LoopCollapse batch must use O(1) stack
/// depth in N. Drives the REAL public path (apply → SubmitPayAmount →
/// drive_copy_token_batches → liminal copy-token batch) on a bounded thread stack
/// with N = 1000 (the PayAmountChoice max).
///
/// REVERT-PROBE (measured on THIS worktree, non-vacuous & discriminating): the
/// per-token stack cost differs by algorithm, not just by a base constant. The
/// pre-fix recursive commit→continue→apply path (HEAD 7458a7a8f) is O(N) depth,
/// ~20 KiB/token in the debug build — measured pre-fix @ 4 MiB mints N=50 but
/// aborts at N≥100, and @ 8 MiB N=1000 still aborts (needs ~20 MiB). The post-fix
/// iterative loop is O(1) depth in N (the only residual growth is the O(log N)
/// `im::HashMap` HAMT COW) — measured post-fix mints N=1000 at ≥3 MiB.
/// So an 8 MiB stack cleanly separates the two: post-fix mints 1000 with ~5 MiB
/// of headroom, while reverting the iterative fix flips this to a process abort
/// (Rust's stack-overflow handler `abort()`s the whole binary — the strongest
/// non-vacuity signal). NOTE: 8 MiB is a *debug-build* budget; native/WASM release
/// frames are far smaller (the user's real WASM overflow was ~N=200), but the
/// debug O(N)/O(1) split is what this test pins. Positive reach-guards (the
/// LoopCollapse-prompt precondition + `minted == 1000`) prove it isn't vacuous.
#[test]
fn loop_collapse_large_mint_does_not_overflow_small_stack() {
    let mut state: GameState =
        serde_json::from_str(&OFFER_STATE).expect("the real 4p offer dump must deserialize");
    drive_all_accept_n(&mut state, 1000);
    let before = p0_saproling_ids(&state).len();
    drive_priority_to_next_boundary(&mut state);
    assert!(
        matches!(
            state.waiting_for,
            WaitingFor::PayAmountChoice { player, resource: PayableResource::LoopCollapse { .. }, .. }
                if player == P0
        ),
        "reach-guard: at the LoopCollapse prompt for P0, got {:?}",
        state.waiting_for
    );

    // 8 MiB: comfortably above the O(1) post-fix base (~3 MiB in debug) yet far
    // below the pre-fix O(N=1000) requirement (~20 MiB). Independent of libtest's
    // RUST_MIN_STACK — the explicit builder stack is what bounds the mint depth.
    let handle = std::thread::Builder::new()
        .stack_size(8 << 20)
        .spawn(move || {
            apply(&mut state, P0, GameAction::SubmitPayAmount { amount: 1000 })
                .expect("P0 submits the max LoopCollapse count");
            state
        })
        .expect("spawn bounded-stack mint thread");
    let state = handle
        .join()
        .expect("minting 1000 copies must NOT overflow the 8 MiB stack (O(1) depth in N)");

    let after = p0_saproling_ids(&state);
    let minted = after.len().saturating_sub(before);
    assert_eq!(
        minted, 1000,
        "SubmitPayAmount{{1000}} mints exactly 1000 Saprolings"
    );
    // Semantics preserved by the iterative path (spot-check the tail token).
    let sample = after.iter().last().copied().expect("≥1 minted token");
    let o = state.objects.get(&sample).expect("minted token present");
    assert!(o.is_token && o.tapped, "minted tokens are tapped tokens");
    assert_eq!(o.color, vec![ManaColor::Green]);
    assert_eq!((o.power, o.toughness), (Some(1), Some(1)));
    // Cash-out invariant (same as T1) still holds after a large mint.
    assert!(!state.unbounded_resources.contains_key(&P0));
    assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
}

/// T2 (MANA NEGATIVE discriminator): a real infinite-COLORLESS mana loop (Basalt Monolith
/// with Power Artifact) writes NO materialization stash (§5), so the boundary collapse
/// pass does NOT prompt. Matched to T1: token axis → prompt+mint; mana axis → no prompt.
///
/// Non-vacuous: the reach-guard asserts P0 IS flagged unbounded (Mana(Colorless) axis
/// present) yet holds no stash — the discriminator is the stash, not the flag.
#[test]
fn real_4p_basalt_mana_loop_boundary_does_not_prompt_collapse() {
    use engine::analysis::resource::ResourceAxis;
    use engine::types::mana::ManaType;

    let mut state: GameState = serde_json::from_str(&BASALT_INFINITE_COLORLESS_STATE)
        .expect("the real Basalt+Power Artifact dump must deserialize into the current GameState");

    // Reach-guard: P0 IS flagged unbounded (mana axis) — but a mana loop writes no stash.
    assert_eq!(
        state.unbounded_resources.get(&P0),
        Some(&BTreeSet::from([ResourceAxis::Mana(ManaType::Colorless)])),
        "fixture precondition: P0's only unbounded axis is Mana(Colorless)"
    );
    assert!(
        !state.pending_unbounded_materialization.contains_key(&P0),
        "a mana loop must write NO materialization stash (the discriminator)"
    );
    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { player } if player == P0),
        "fixture precondition: ordinary priority for P0"
    );

    drive_priority_to_next_boundary(&mut state);

    // The boundary ran (phase advanced or a non-collapse prompt surfaced) but produced NO
    // LoopCollapse prompt — the mana axis writes no stash, so the collapse pass cannot fire.
    assert!(
        !matches!(
            state.waiting_for,
            WaitingFor::PayAmountChoice {
                resource: PayableResource::LoopCollapse { .. },
                ..
            }
        ),
        "a mana loop must NOT surface a LoopCollapse prompt at the boundary, got {:?}",
        state.waiting_for
    );
}

/// T-A (CR 500.5, PRIMARY): a LOOP-backed infinite-colorless mana axis (Basalt Monolith +
/// Power Artifact — NOT the debug toggle, so P0 is absent from `debug_infinite_mana`) DRAINS
/// and DE-REALIZES at the next step/phase boundary. The keep-gate (E3) now reads the explicit
/// debug marker, not "has a Mana axis", so a non-debug player is `Drop`ped; the queue-empty
/// axis-clear (E4) removes the Mana axis so `refill_infinite_mana` can't re-seed it.
///
/// REVERT-PROBE (two independent flips):
///  - Revert E3 (keep-gate back to the `Mana(_)` scan) → P0's pool is KEPT → the
///    `colorless == 0` assertion FLIPS (MEASURED baseline M2: 100→100).
///  - Revert E4 (drop the boundary axis-clear) → the pool empties but the axis persists →
///    the end-of-`apply` `refill_infinite_mana` re-seeds it → both the `unbounded_resources[P0]
///    == None` assertion (axis still present) and the `colorless == 0` assertion (re-seeded)
///    FLIP.
///
/// Non-vacuity: the reach-guard asserts P0 IS flagged (`Mana(Colorless)`) and the pool is
/// actually seeded (`colorless > 0`) before the drive — the 100→0 delta is real (MEASURED M3).
#[test]
fn real_4p_basalt_mana_loop_boundary_drains_and_derealizes() {
    use engine::analysis::resource::ResourceAxis;
    use engine::game::mana_payment::refill_infinite_mana;
    use engine::types::mana::ManaType;

    let mut state: GameState = serde_json::from_str(&BASALT_INFINITE_COLORLESS_STATE)
        .expect("the real Basalt+Power Artifact dump must deserialize into the current GameState");

    // Precondition: P0's only unbounded axis is the LOOP-backed Mana(Colorless), and P0 is
    // NOT debug-toggled — the discriminator that makes it drain rather than persist.
    assert_eq!(
        state.unbounded_resources.get(&P0),
        Some(&BTreeSet::from([ResourceAxis::Mana(ManaType::Colorless)])),
        "fixture precondition: P0's only unbounded axis is Mana(Colorless)"
    );
    assert!(
        !state.debug_infinite_mana.contains(&P0),
        "fixture precondition: the loop-backed axis is NOT the debug toggle"
    );

    // Seed the pool so the drain has a real 100→0 delta (MEASURED M3).
    refill_infinite_mana(&mut state);
    let p0_idx = state
        .players
        .iter()
        .position(|p| p.id == P0)
        .expect("P0 present in the loaded state");
    // Reach-guard (non-vacuity): the pool is actually full of colorless before the boundary.
    assert!(
        state.players[p0_idx]
            .mana_pool
            .count_color(ManaType::Colorless)
            > 0,
        "reach-guard: refill seeded P0's colorless pool (the drain delta is non-vacuous)"
    );

    drive_priority_to_next_boundary(&mut state);

    // (1) DRAIN: the loop-backed pool empties at the CR 500.5 boundary (E3 keep-gate false).
    assert_eq!(
        state.players[p0_idx]
            .mana_pool
            .count_color(ManaType::Colorless),
        0,
        "a loop-backed (non-debug) ∞-mana pool must DRAIN at the step/phase boundary (CR 500.5)"
    );
    // (2) DE-REALIZE: the Mana axis is cleared so refill cannot re-seed it (E4).
    assert_eq!(
        state.unbounded_resources.get(&P0),
        None,
        "the loop-backed ∞-mana axis must be de-realized at the boundary (E4 axis-clear)"
    );
}

/// T-D (CR 500.5 + CR 732.2a, ORDERING — E4 placed BEFORE the token-collapse check):
/// a controller who holds BOTH a loop-backed ∞-mana axis AND an accepted token loop at the
/// same boundary must have the mana axis DRAINED+cleared while the token loop STILL collapses.
/// Because E4 clears the mana axis before the token pause returns, the intervening
/// boundary-crossing `apply()` (and the later `SubmitPayAmount` `apply()`) run
/// `refill_infinite_mana` with NO mana axis present — so the just-drained pool is not re-seeded.
///
/// LIVE test (reviewer #4): the base is the REAL 4p Basalt dump (P0 already `{Mana(Colorless)}`);
/// the token loop is grafted via the engine's OWN single-authority writers
/// (`mark_unbounded_loop` and `register_pending_materialization`) — the standard Part-2 accept
/// footprint — NOT synthetic scaffolding. The two-pass boundary it exercises is the real
/// production path.
///
/// REVERT-PROBE: move E4 to AFTER the `next_apnap_player_with_pending_materialization` check →
/// the Mana axis is still live when the boundary-crossing `apply()` runs `refill_infinite_mana`
/// → `colorless > 0` at the prompt → the drain assertion (2) FLIPS.
#[test]
fn real_4p_mana_and_token_boundary_drains_mana_and_still_collapses() {
    use engine::analysis::resource::ResourceAxis;
    use engine::game::mana_payment::refill_infinite_mana;
    use engine::types::ability::CopiableValues;
    use engine::types::card_type::CardType;
    use engine::types::mana::{ManaCost, ManaType};

    let mut state: GameState = serde_json::from_str(&BASALT_INFINITE_COLORLESS_STATE)
        .expect("the real Basalt+Power Artifact dump must deserialize into the current GameState");

    // Precondition: P0 already carries the LOOP-backed Mana(Colorless) axis, non-debug.
    assert_eq!(
        state.unbounded_resources.get(&P0),
        Some(&BTreeSet::from([ResourceAxis::Mana(ManaType::Colorless)])),
        "fixture precondition: P0's only unbounded axis is Mana(Colorless)"
    );
    assert!(
        !state.debug_infinite_mana.contains(&P0),
        "fixture precondition: the mana axis is loop-backed, not the debug toggle"
    );

    // Graft a token loop onto the real dump via the engine's single-authority writers — this is
    // the standard Part-2 accept footprint (a TokensCreated axis + a materialization stash).
    state.mark_unbounded_loop(P0, &[ResourceAxis::TokensCreated]);
    let profile = Box::new(CopiableValues {
        name: "Saproling".to_string(),
        mana_cost: ManaCost::default(),
        color: vec![ManaColor::Green],
        card_types: CardType {
            supertypes: vec![],
            core_types: vec![CoreType::Creature],
            subtypes: vec!["Saproling".to_string()],
        },
        power: Some(1),
        toughness: Some(1),
        loyalty: None,
        printed_loyalty: None,
        keywords: vec![],
        abilities: std::sync::Arc::default(),
        trigger_definitions: std::sync::Arc::default(),
        replacement_definitions: std::sync::Arc::default(),
        static_definitions: std::sync::Arc::default(),
        room_halves: None,
        name_origin: Default::default(),
    });
    state.register_pending_materialization(P0, PersistentAxisMaterialization::Tokens(profile));

    // Seed so the drain has a real delta.
    refill_infinite_mana(&mut state);
    let p0_idx = state
        .players
        .iter()
        .position(|p| p.id == P0)
        .expect("P0 present in the loaded state");
    assert!(
        state.players[p0_idx]
            .mana_pool
            .count_color(ManaType::Colorless)
            > 0,
        "reach-guard: P0's colorless pool is seeded before the boundary (drain non-vacuous)"
    );

    drive_priority_to_next_boundary(&mut state);

    // (1) TOKEN half still LIVE: the boundary surfaces the LoopCollapse prompt for P0.
    assert!(
        matches!(
            state.waiting_for,
            WaitingFor::PayAmountChoice { player, resource: PayableResource::LoopCollapse { .. }, .. }
                if player == P0
        ),
        "the coexisting token loop must still prompt LoopCollapse at the boundary, got {:?}",
        state.waiting_for
    );
    // (2) MANA half DRAINED before the token pause (the E4-ordering DISCRIMINATOR): E4 cleared
    //     the axis BEFORE the token check, so the end-of-apply refill saw no Mana axis and did
    //     not re-seed the just-drained pool.
    assert_eq!(
        state.players[p0_idx]
            .mana_pool
            .count_color(ManaType::Colorless),
        0,
        "the loop-backed mana axis must be drained+cleared BEFORE the token pause (no refill re-seed)"
    );

    // Resolve the token collapse.
    apply(&mut state, P0, GameAction::SubmitPayAmount { amount: 3 })
        .expect("P0 submits the finite loop-collapse count");

    // (3) After the SubmitPayAmount apply()'s own end-of-action refill, mana stays 0 (the axis
    //     was already gone) and the token axis is cashed out — both loops ended.
    assert_eq!(
        state.players[p0_idx]
            .mana_pool
            .count_color(ManaType::Colorless),
        0,
        "mana stays drained after the LoopCollapse submit — no refill re-seed"
    );
    assert!(
        !state.unbounded_resources.contains_key(&P0),
        "both the mana axis (E4) and the token axis (collapse) are cashed out"
    );
}

// ───────────── REVISION 2: one-shot-bootstrap tapped seed + convoke=None over-fire guard ─────────────
//
// The accepted object-growth loop can be DEMONSTRATED off a one-shot: the human convoked the
// {B}{G} cost-reducer (Witherbloom) for the {G}, tapping it — it can't re-tap next cycle, so the
// sustainable period taps a created Saproling instead. At the accept beat ZERO tapped Saprolings
// exist on the live board, so `tapped_fodder_members` is ∅ → the ∞ pile rendered empty (the user's
// bug). REVISION 2 seeds a representative tapped Saproling (∞ anchor) + a CR 702.51a optional-
// convoke untapped remainder (W+1), gated on `period.taps_fodder && is_empty()` so a convoke=None
// UNTAPPED-growth loop (which the `board_covers_modulo_fodder` `>=` cover also admits with an empty
// tapped set) is NOT seeded.

/// P0's UNTAPPED battlefield Saprolings — the working-set remainder (W) plus the CR 702.51a
/// untapped remainder seed; correctly EXCLUDED from the tapped ∞ pile.
fn p0_untapped_saprolings(state: &GameState) -> BTreeSet<ObjectId> {
    state
        .battlefield
        .iter()
        .copied()
        .filter(|id| {
            state
                .objects
                .get(id)
                .is_some_and(|o| o.controller == P0 && !o.tapped && o.name == "Saproling")
        })
        .collect()
}

/// T-NEW-1 (REVISION 2): a convoke object-growth loop BOOTSTRAPPED off a one-shot leaves ZERO
/// tapped fodder at accept, so pre-fix the ∞ pile renders empty (the user's bug). The accept-time
/// seed mints one representative TAPPED Saproling (the ∞ anchor) and one UNTAPPED Saproling
/// (CR 702.51a's optional-convoke capping cast → W+1), gated on `period.taps_fodder && is_empty()`.
/// Drives the REAL cast → CR 732.2a offer → APNAP accept → phase-boundary collapse end to end.
///
/// REVERT-PROBES (documented + implementer-run, non-vacuous):
///  - delete the `seed_representative_fodder(.., true)` tapped seed → step 3 pile EMPTY (the
///    pre-fix bug) → the pile/oracle/derived/round-trip assertions FLIP.
///  - delete the `seed_representative_fodder(.., false)` untapped seed → step 3 untapped count is
///    5 not 6 → the W+1 assertion FLIPS.
///
/// Non-vacuity anchor = the step-1 offer + 0-tapped reach-guard, which holds in BOTH pre/post-fix
/// (only the seeded pile differs), so the positive pile assertion cannot pass vacuously.
#[test]
fn real_4p_one_shot_bootstrap_seeds_tapped_infinite_pile_and_w_plus_1_untapped() {
    let mut state: GameState = serde_json::from_str(&UNTAPPED_PRECAST_STATE)
        .expect("the real untapped-precast 4p dump must deserialize into the current GameState");

    // ── Setup mutation 1 (rules-neutral): untap Saproling 405 so ZERO tapped fodder exists — the
    // "bootstrap tapped only the one-shot" start. MEASURED: 405 is the sole pre-tapped Saproling.
    let sap405 = ObjectId(405);
    assert_eq!(
        state
            .objects
            .get(&sap405)
            .map(|o| (o.name.as_str(), o.tapped)),
        Some(("Saproling", true)),
        "fixture precondition: 405 is a tapped Saproling (the one to untap)"
    );
    state.objects.get_mut(&sap405).unwrap().tapped = false;
    assert!(
        p0_tapped_vanilla_saprolings(&state).is_empty(),
        "after untapping 405, P0 has ZERO tapped Saprolings (the one-shot-bootstrap start)"
    );

    // ── Setup mutation 2 (CR 702.51a-neutral HARNESS accommodation): flip Witherbloom(401)'s color
    // to Green-first. The ENGINE pip-matches convoke color (a B/G creature legally pays a {G} pip
    // regardless of order); only `GameRunner::convoke_with` picks `color.first()`, so Green-first
    // lets the harness tap Witherbloom for the {G}. Both `color` and `base_color` are set so a
    // `flush_layers` inside the cast pipeline does not revert it.
    let witherbloom = ObjectId(401);
    {
        let w = state.objects.get_mut(&witherbloom).unwrap();
        assert_eq!(w.name, "Witherbloom, the Balancer");
        w.color = vec![ManaColor::Green, ManaColor::Black];
        w.base_color = vec![ManaColor::Green, ManaColor::Black];
    }

    // ── Step 1: drive the REAL Sprout Swarm cast, convoking WITHERBLOOM (the non-fodder one-shot)
    // for {G}. Reach-guards hold in BOTH pre/post-fix ⇒ the negative revert-probes are non-vacuous.
    let sprout = ObjectId(402);
    let mut runner = GameRunner::from_state(state);
    let outcome = runner
        .cast(sprout)
        .accept_optional()
        .convoke_with(&[witherbloom])
        .commit()
        .resolve();

    assert!(
        matches!(
            outcome.final_waiting_for(),
            WaitingFor::LoopShortcut { proposer, .. } if *proposer == P0
        ),
        "convoking the one-shot Witherbloom MUST surface the CR 732.2a offer, got {:?}",
        outcome.final_waiting_for()
    );
    assert!(
        outcome
            .state()
            .objects
            .get(&witherbloom)
            .is_some_and(|w| w.tapped),
        "reach-guard: the convoke tapped Witherbloom (the one-shot, unusable next cycle)"
    );
    assert!(
        p0_tapped_vanilla_saprolings(outcome.state()).is_empty(),
        "reach-guard: ZERO tapped Saprolings at the offer (the empty-pile bug config)"
    );
    assert_eq!(
        count_battlefield_saprolings(outcome.state()),
        5,
        "reach-guard: the cast made a 5th (untapped) Saproling — W=5 untapped working set"
    );

    // ── Step 2: APNAP accept → materialize.
    drive_all_accept_n(runner.state_mut(), 5);
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "after all accept, materialize hands priority back, got {:?}",
        runner.state().waiting_for
    );

    // ── Step 3 POSITIVE (the fix): the seed anchored a NON-empty tapped ∞ pile of exactly one P0
    // tapped vanilla Saproling; and W+1 = 6 untapped Saprolings remain.
    let oracle = p0_tapped_vanilla_saprolings(runner.state());
    assert_eq!(
        oracle.len(),
        1,
        "the accept-time seed minted exactly ONE representative tapped Saproling (the ∞ anchor)"
    );
    let pile = runner
        .state()
        .unbounded_loop_pile
        .get(&P0)
        .expect("the seeded object-growth loop must write a NON-empty ∞ pile (the fix)");
    assert_eq!(
        *pile, oracle,
        "the ∞ pile is exactly the seeded tapped Saproling (non-circular name+vanilla oracle)"
    );
    // The seeded pile member is a tapped 1/1 green Saproling token.
    let seed_id = *pile.iter().next().unwrap();
    let seed = runner.state().objects.get(&seed_id).expect("seed present");
    assert!(seed.is_token, "the seed is a token");
    assert!(seed.tapped, "the seed is tapped (the ∞ anchor)");
    assert_eq!(
        (seed.power, seed.toughness),
        (Some(1), Some(1)),
        "seed is a 1/1"
    );
    assert_eq!(seed.color, vec![ManaColor::Green], "seed is green");
    // W+1 untapped remainder (CR 702.51a optional-convoke capping cast).
    assert_eq!(
        p0_untapped_saprolings(runner.state()).len(),
        6,
        "the untapped remainder seed leaves W+1 = 6 untapped Saprolings (revert → 5)"
    );

    // derive_views projects the pile; it survives a serde round-trip.
    //
    // The accept also scheduled a finite `TokensCreated` collapse, but the engine DEFERS applying
    // it to the CR 500.5 boundary (advancing to the proposal's ending point, CR 732.2c), so nothing is
    // minted yet and the ∞ pile stays PROJECTED while it is merely scheduled. The scheduling does
    // not change the pile set, which is why the same `oracle` comparison now runs directly on the
    // real post-accept state.
    let derived_set: BTreeSet<ObjectId> = derive_views(runner.state(), Some(P0))
        .unbounded_pile
        .iter()
        .copied()
        .collect();
    assert_eq!(
        derived_set, oracle,
        "derive_views().unbounded_pile equals the seeded pile"
    );
    let json = serde_json::to_string(runner.state()).expect("serialize post-accept");
    let reloaded: GameState = serde_json::from_str(&json).expect("reload post-accept");
    assert_eq!(
        reloaded.unbounded_loop_pile.get(&P0),
        Some(&oracle),
        "the seeded ∞ pile survives a serde round-trip (post-fix saves reload it)"
    );

    // ── Step 4 BOUNDARY lock-in: name N=5 at the phase boundary → steady-state board is 6 untapped
    // + (1 seed + 5 minted) = 6 tapped Saprolings + Witherbloom tapped, cashed out, no re-prompt.
    drive_priority_to_next_boundary(runner.state_mut());
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::PayAmountChoice { player, resource: PayableResource::LoopCollapse { .. }, .. }
                if player == P0
        ),
        "the phase boundary must prompt P0 for the LoopCollapse count, got {:?}",
        runner.state().waiting_for
    );
    apply(
        runner.state_mut(),
        P0,
        GameAction::SubmitPayAmount { amount: 5 },
    )
    .expect("P0 submits the finite loop-collapse count");
    assert_eq!(
        p0_untapped_saprolings(runner.state()).len(),
        6,
        "post-boundary: W+1 = 6 untapped Saprolings preserved"
    );
    assert_eq!(
        p0_tapped_vanilla_saprolings(runner.state()).len(),
        6,
        "post-boundary: 6 tapped Saprolings (1 accept seed + 5 boundary mint)"
    );
    assert!(
        runner
            .state()
            .objects
            .get(&witherbloom)
            .is_some_and(|w| w.tapped),
        "post-boundary: Witherbloom (the one-shot) stays tapped"
    );
    assert!(
        !runner.state().unbounded_resources.contains_key(&P0),
        "collapsing the token loop cashes out the ∞ TokensCreated axis"
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "after the mint the boundary fixpoint restores Priority, got {:?}",
        runner.state().waiting_for
    );
    drive_priority_to_next_boundary(runner.state_mut());
    assert!(
        !matches!(
            runner.state().waiting_for,
            WaitingFor::PayAmountChoice {
                resource: PayableResource::LoopCollapse { .. },
                ..
            }
        ),
        "the cashed-out loop must NOT re-prompt at the next boundary, got {:?}",
        runner.state().waiting_for
    );
}

/// The one-shot-bootstrap rig driven to the ACCEPTED state: the real buyback+convoke Sprout
/// Swarm cast (convoking the one-shot Witherbloom for the {G}) → CR 732.2a offer → APNAP
/// accept of `Fixed(5)`. The two setup mutations are the same rules-neutral ones
/// `real_4p_one_shot_bootstrap_seeds_tapped_infinite_pile_and_w_plus_1_untapped` documents in
/// full (untap 405 so ZERO tapped fodder exists; Green-first Witherbloom so
/// `GameRunner::convoke_with` picks a colour the engine already pip-matches).
///
/// This rig is chosen for the ∞-row backing arms below because its accept-time seed makes the
/// pile provably ONE object, so "the last backing member leaves" is a single `move_to_zone`.
fn one_shot_bootstrap_accepted_state() -> GameState {
    let mut state: GameState = serde_json::from_str(&UNTAPPED_PRECAST_STATE)
        .expect("the real untapped-precast 4p dump must deserialize into the current GameState");
    state
        .objects
        .get_mut(&ObjectId(405))
        .expect("fixture carries Saproling 405")
        .tapped = false;
    {
        let w = state
            .objects
            .get_mut(&ObjectId(401))
            .expect("fixture carries Witherbloom 401");
        w.color = vec![ManaColor::Green, ManaColor::Black];
        w.base_color = vec![ManaColor::Green, ManaColor::Black];
    }
    let mut runner = GameRunner::from_state(state);
    let outcome = runner
        .cast(ObjectId(402))
        .accept_optional()
        .convoke_with(&[ObjectId(401)])
        .commit()
        .resolve();
    assert!(
        matches!(
            outcome.final_waiting_for(),
            WaitingFor::LoopShortcut { proposer, .. } if *proposer == P0
        ),
        "rig reach-guard: the convoked recast must surface P0's CR 732.2a offer, got {:?}",
        outcome.final_waiting_for()
    );
    drive_all_accept_n(runner.state_mut(), 5);
    runner.state().clone()
}

/// MED-1 (CR 732.2c + CR 110.1): an ACCEPTED object-growth `∞` ROW SURVIVES losing its entire
/// registered backing. Once the last player accepts, the shortcut is taken and the growth will
/// land at the boundary, so a row that vanished with the pile would have the HUD deny a result the
/// table already agreed to.
///
/// ONE rig, TWO arms, THE SAME assertion — `derive_views(..).unbounded_resources` contains
/// `ResourceAxis::TokensCreated`:
///
/// | arm (in run order) | what leaves the battlefield       | THE assertion |
/// |--------------------|-----------------------------------|---------------|
/// | control            | a non-pile untapped Saproling     | **present**   |
/// | subject            | the pile's ONLY member (the seed) | **present**   |
///
/// The control is the matched pair, not a second scenario: same fixture, same cast, same
/// accept, same `move_to_zone` chokepoint, differing only in WHICH object departs. It runs FIRST
/// on purpose — see the comment at that arm.
///
/// THE ROW IS NOT KEPT BY ACCIDENT. This state is ACCEPTED, so the projection's FIRST conjunct
/// (`!accepted_axes.contains_key(&axis)`) is false and the backing check is never consulted — and
/// the schedule half asserted below proves the stash really does name this axis, so the conjunct
/// is decided by a live fact rather than by an empty map. The UNACCEPTED half of the same gate,
/// where a dead pile really does revoke the row, is pinned by
/// `derived_views::tests::an_accepted_token_collapse_keeps_its_row_when_its_pile_dies`'
/// NON-VACUITY arm.
///
/// MUTATION (RUN, and the reason this test exists in this shape): **DROP** the
/// `!accepted_axes.contains_key(&axis)` conjunct from the row loop's gate ⇒ the SUBJECT arm reds
/// with an empty row set (the pre-fix behaviour: an accepted collapse silently losing its badge
/// before it lands), while the CONTROL arm stays green because its backing is still live.
///
/// The store and cash-out guards below are what make the surviving row HONEST rather than merely
/// present, and they are more load-bearing here than they were when this arm asserted absence: a
/// kept row is a claim about growth that will still land, so the test drives the real CR 500.5
/// boundary and mints. They are also the anti-"register enablers instead" tripwire: routing this
/// through `zones`' defuse would call `clear_unbounded_loop`, which also wipes
/// `pending_unbounded_materialization` and its CR 732.2c bound — i.e. one dying token would
/// cancel the collapse the whole table accepted. These rows go red the moment that happens.
#[test]
fn accepted_object_growth_row_survives_losing_its_entire_pile() {
    use engine::analysis::resource::ResourceAxis;
    use engine::game::zones::move_to_zone;
    use engine::types::events::GameEvent;

    let base = one_shot_bootstrap_accepted_state();

    // ── REACH GUARDS: the accepted state really carries the row and a one-member backing set.
    // Without these, the subject arm's "absent" could pass on a state that never had a row.
    assert_eq!(
        base.unbounded_resources.len(),
        1,
        "reach-guard: exactly one controller carries ∞ marks on this rig"
    );
    let marked = base
        .unbounded_resources
        .get(&P0)
        .expect("the accept marks P0's ∞ axes")
        .clone();
    assert!(
        marked.contains(&ResourceAxis::TokensCreated),
        "reach-guard: the object-growth accept marks TokensCreated, got {marked:?}"
    );
    let pile = base
        .unbounded_loop_pile
        .get(&P0)
        .expect("the object-growth accept registers a ∞ pile")
        .clone();
    assert_eq!(
        pile.len(),
        1,
        "reach-guard: this rig's seeded pile is exactly ONE object, so a single departure \
         empties the whole backing set"
    );
    let seed_id = *pile.iter().next().unwrap();
    assert!(
        base.battlefield.contains(&seed_id),
        "reach-guard: the pile member is on the battlefield at accept"
    );

    let rows = |state: &GameState| -> Vec<ResourceAxis> {
        derive_views(state, Some(P0))
            .unbounded_resources
            .iter()
            .map(|r| r.axis)
            .collect()
    };

    // ── CONTROL FIRST (matched pair, and the wire-level non-vacuity anchor for the subject
    // arm below): same rig, same `move_to_zone` chokepoint — but the object that leaves is NOT
    // in the pile, so the backing survives and the row must PERSIST. Deliberately ordered
    // BEFORE the subject: an equivalent "row present on the untouched base state" guard would
    // panic first under the TRIVIALIZE mutation and MASK this arm, leaving the pair one-sided.
    let mut control = base.clone();
    let bystander = *p0_untapped_saprolings(&control)
        .iter()
        .next()
        .expect("the W+1 untapped remainder supplies a non-pile bystander");
    assert_ne!(
        bystander, seed_id,
        "control precondition: the departing object is NOT a pile member"
    );
    let mut events: Vec<GameEvent> = Vec::new();
    move_to_zone(&mut control, bystander, Zone::Graveyard, &mut events);
    assert!(
        !control.battlefield.contains(&bystander),
        "the control's departure really happened"
    );
    let control_rows = rows(&control);
    assert!(
        control_rows.contains(&ResourceAxis::TokensCreated),
        "THE assertion (control): the registered pile still has a live member, so the ∞ row \
         must persist, got {control_rows:?}"
    );

    // ── SUBJECT: the last (only) backing member leaves through the real production
    // chokepoint `zones::move_to_zone`, not by hand-editing the pile.
    let mut subject = base.clone();
    let mut events: Vec<GameEvent> = Vec::new();
    move_to_zone(&mut subject, seed_id, Zone::Graveyard, &mut events);
    assert!(
        !subject.battlefield.contains(&seed_id),
        "the departure really happened (CR 110.1: it stopped being a permanent)"
    );
    let subject_rows = rows(&subject);
    assert!(
        subject_rows.contains(&ResourceAxis::TokensCreated),
        "THE assertion (subject): the table ACCEPTED this collapse (CR 732.2c), so the \
         TokensCreated ∞ row survives its ENTIRE registered pile leaving the battlefield — the \
         growth still lands at the boundary below, got {subject_rows:?}"
    );

    // THE CR 732.2c PIN: a dropped ROW does not cancel the accepted collapse. Doc blocks cite
    // THIS test as the witness that a row may vanish while the growth the table agreed to still
    // lands, and without the next two assertions that citation rests on the inference "the stash
    // key survives, therefore the growth still happens" — whose middle steps (that this stash
    // still SCHEDULES the axis, and that the boundary still APPLIES it once the backing is gone)
    // are checked nowhere. Asserted at the store and at the boundary rather than against a wire
    // channel, because `pending_unbounded_materialization` is the contract's authority: it is
    // what `game::turns` reads to prompt and cash out. A projection could be deleted entirely and
    // the growth would still land; that is the point being pinned.
    let subject_scheduled = subject.scheduled_collapse_axes(
        subject
            .pending_unbounded_materialization
            .get(&P0)
            .expect("the surviving stash, asserted below"),
    );
    assert!(
        subject_scheduled.contains(&ResourceAxis::TokensCreated),
        "THE assertion (subject, schedule half): the surviving stash must still SCHEDULE the axis \
         whose row just died, got {subject_scheduled:?}"
    );

    // …and it really cashes out. Same post-loss state, driven to the real CR 500.5 boundary
    // through `drive_priority_to_next_boundary` and collapsed through the public `apply()` path.
    // `amount: 1` because any accepted bound is ≥ 1, so this cannot fail on the CR 732.2c ceiling;
    // a stubbed collapse mints 0 and reds it.
    let mut cashed = subject.clone();
    drive_priority_to_next_boundary(&mut cashed);
    assert!(
        matches!(
            cashed.waiting_for,
            WaitingFor::PayAmountChoice {
                player,
                resource: PayableResource::LoopCollapse { .. },
                ..
            } if player == P0
        ),
        "reach-guard: the boundary still prompts P0 to collapse the axis whose row was dropped, \
         got {:?}",
        cashed.waiting_for
    );
    let saps_before = p0_saproling_ids(&cashed).len();
    apply(&mut cashed, P0, GameAction::SubmitPayAmount { amount: 1 })
        .expect("P0 collapses the accepted growth even though its ∞ row had been dropped");
    assert_eq!(
        p0_saproling_ids(&cashed).len(),
        saps_before + 1,
        "THE assertion (subject, cash-out half): dropping the ∞ ROW is a DISPLAY revocation — the \
         growth the table unanimously accepted still lands at the boundary (CR 732.2c)"
    );

    // SCOPE: NOTHING leaves the wire — the projected axis set is exactly the marked set. An
    // acceptance gate that is too broad (keeping rows it should not) cannot be caught here, but
    // one that is too NARROW is: any axis this rig marks and the projection drops reds this
    // equality. The load-bearing control for the `None` (never-registered ⇒ badge unchanged)
    // branch is `loop_shortcut_mana_engine::mana_engine_accept_still_renders_its_infinity_badge`,
    // which reds if `object_growth_backing`'s catch-all arm returns `Some(false)` instead of
    // `None`; the UNACCEPTED revocation half is
    // `derived_views::tests::an_accepted_token_collapse_keeps_its_row_when_its_pile_dies`.
    assert_eq!(
        subject_rows.iter().copied().collect::<BTreeSet<_>>(),
        marked,
        "scope: with the collapse accepted, every marked axis keeps its ∞ row"
    );

    // STORE: a DISPLAY revocation only. Nothing here may touch the accepted-collapse stash,
    // the mark, or the pile — the boundary and the zone-exit defuse still read all three.
    assert!(
        subject.pending_unbounded_materialization.contains_key(&P0),
        "the accepted-collapse stash must SURVIVE — dropping a row may not cancel growth the \
         table already unanimously accepted (CR 732.2c)"
    );
    assert!(
        subject.pending_materialization_count.contains_key(&P0),
        "…and so must its CR 732.2c accepted-count bound"
    );
    assert!(
        subject
            .unbounded_loop_pile
            .get(&P0)
            .is_some_and(|p| p.contains(&seed_id)),
        "the STORE is not filtered: it still carries the departed member"
    );
    assert!(
        subject
            .unbounded_resources
            .get(&P0)
            .is_some_and(|axes| axes.contains(&ResourceAxis::TokensCreated)),
        "the MARK survives too; only the projection stops rendering it"
    );
}

/// T-NEW-2 (REVISION 2 — the BLOCKER-1 discriminator): a convoke=None UNTAPPED-growth loop must
/// NOT seed. Build-fresh Sprout Swarm with Convoke STRIPPED and `mana_cost = base_mana_cost = {1}`
/// so Witherbloom's affinity for creatures fully covers base{1}+buyback{3}={4} with {0} mana and
/// no convoke — a period that creates a Saproling UNTAPPED and taps nothing. The
/// `board_covers_modulo_fodder` `>=` untapped cover admits this growth, so it reaches
/// `materialize_object_growth_shortcut` with an EMPTY tapped-fodder set — the buggy `is_empty()`-
/// only guard would over-fire and mint 2 spurious tokens. The sound `period.taps_fodder` axis is
/// FALSE here → NO seed.
///
/// REVERT-PROBE #3 (documented + implementer-run, non-vacuous): replace Edit B's guard with the
/// buggy `is_empty()`-only guard → the seed fires → `unbounded_loop_pile[P0]` becomes `Some` (1
/// tapped seed), `p0_tapped_vanilla_saprolings` non-empty, and the post-accept Saproling count is
/// `pre + 2` → every POSITIVE assertion FLIPS. Non-vacuity anchor = the offer +
/// `convoke_tappable_count == 0` reach-guard, which holds in BOTH guard variants (only the seed
/// differs).
#[test]
fn build_fresh_convoke_none_untapped_growth_does_not_seed_tapped_pile() {
    use engine::types::keywords::Keyword;
    use engine::types::mana::ManaCost;

    let Some(db) = shared_card_db() else {
        return; // card DB unavailable in this environment — skip like the other DB-backed tests.
    };

    let state = bootstrap_4p_game(db);
    let mut runner = GameRunner::from_state(state);

    // Object-growth board on P0, but a NON-convoke maker: Witherbloom (tapped; still grants affinity
    // — a tapped creature is still controlled, CR 702.41a) + 4 Saproling fodder + a Sprout Swarm
    // whose Convoke keyword is stripped and whose cost is pure generic {1} (so affinity covers
    // base{1}+buyback{3}={4} for {0} mana, no convoke tap).
    let witherbloom = place_card(runner.state_mut(), P0, WITHERBLOOM, Zone::Battlefield, db);
    runner
        .state_mut()
        .objects
        .get_mut(&witherbloom)
        .unwrap()
        .tapped = true;
    let _fodder: Vec<ObjectId> = (0..4)
        .map(|_| create_saproling(runner.state_mut(), P0))
        .collect();
    let sprout = place_card(runner.state_mut(), P0, SPROUT_SWARM, Zone::Hand, db);
    {
        let o = runner.state_mut().objects.get_mut(&sprout).unwrap();
        o.keywords.retain(|k| !matches!(k, Keyword::Convoke));
        o.base_keywords.retain(|k| !matches!(k, Keyword::Convoke));
        o.mana_cost = ManaCost::generic(1);
        o.base_mana_cost = ManaCost::generic(1);
    }
    // Flush layers so Witherbloom's affinity static is live and the stripped keyword/cost stick.
    // The raw scaffolding above never marks `layers_dirty`, so mark full first — otherwise the
    // flush is a no-op and `static_mode_presence` never learns of Witherbloom's affinity-granting
    // `CastWithKeyword` static, closing the presence-gated grant scan (CR 604.1) and making the
    // {4} generic unpayable.
    mark_layers_full(runner.state_mut());
    flush_layers(runner.state_mut());

    let sap_before_cast = count_battlefield_saprolings(runner.state());
    assert_eq!(sap_before_cast, 4, "4 fodder Saprolings before the cast");

    // Cast: no convoke, no mana seeded — affinity covers the whole {4} generic.
    let outcome = runner.cast(sprout).accept_optional().commit().resolve();

    // REACH-GUARD (positive, holds in BOTH guard variants ⇒ the negatives are non-vacuous): the
    // offer FIRES for a NON-convoke period (empty decision schema, zero convoke-tappable), and the
    // cast resolved making one more UNTAPPED Saproling.
    match outcome.final_waiting_for() {
        WaitingFor::LoopShortcut {
            proposer, schema, ..
        } if *proposer == P0 => {
            assert!(
                schema.points.is_empty(),
                "a non-convoke period carries no per-iteration decision points"
            );
            assert_eq!(
                schema.convoke_tappable_count, 0,
                "a non-convoke period has zero convoke-tappable creatures (the discriminator input)"
            );
        }
        other => panic!("expected the CR 732.2a offer to P0, got {other:?}"),
    }
    assert_eq!(
        count_battlefield_saprolings(outcome.state()),
        sap_before_cast + 1,
        "reach-guard: the cast made exactly one more Saproling (untapped-growth)"
    );
    assert!(
        p0_tapped_vanilla_saprolings(outcome.state()).is_empty(),
        "reach-guard: the untapped-growth cast tapped NO Saproling"
    );

    // Accept → materialize.
    let sap_pre_accept = count_battlefield_saprolings(runner.state());
    drive_all_accept(runner.state_mut());
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "after all accept, materialize hands priority back, got {:?}",
        runner.state().waiting_for
    );

    // POSITIVE (the fix): `period.taps_fodder == false` → NO seed fires.
    assert!(
        !runner.state().unbounded_loop_pile.contains_key(&P0),
        "a convoke=None untapped-growth loop must NOT anchor a tapped ∞ pile (no seed)"
    );
    assert!(
        p0_tapped_vanilla_saprolings(runner.state()).is_empty(),
        "no representative tapped Saproling is minted for an untapped-growth loop"
    );
    assert_eq!(
        count_battlefield_saprolings(runner.state()),
        sap_pre_accept,
        "the Saproling count is UNCHANGED across accept — no spurious seed mint (buggy guard → +2)"
    );
}

// ─────────── PR-7 v4 (CR 732.2a): batched persistent-axis boundary collapse (counter + life) ───────────

/// PR-7 v4 (CR 732.2a / CR 122.1 / CR 119.3): the boundary collapse batches N×δ for the beneficial
/// COUNTER axis and — in the SAME submit — DECLINES the LIFE axis because the real 4p board carries
/// a functioning life observer. Real 4p offer dump → real accept → graft a +1/+1 counter axis +
/// a life axis onto the accepted token loop (`mark_unbounded_loop` + `register_pending_materialization`
/// are the standard Part-2 accept writers) → real boundary → `apply(SubmitPayAmount{5})`.
///
/// MEASURED on this fixture (throwaway probe, then removed): post-accept `counter_growth_is_observed
/// == false`, `life_growth_is_observed == true` — the board has NO counter observer but a REAL life
/// observer. So this one submit exercises BOTH firewall branches at once and PROVES the firewall is
/// AXIS-SPECIFIC, not a coarse OR: the counter batches (unobserved) while the life is vetoed
/// (observed) — a coarse OR would wrongly veto the counter too.
///
/// REVERT-PROBE (discriminating):
///  - delete the `PersistentAxisMaterialization::Counters` submit arm ⇒ the +1/+1 counter is
///    unchanged ⇒ assertion (1) FLIPS.
///  - collapse `ObservedGrowth`'s two fields into one coarse OR (`counter || life`), so
///    `boundary_declines` answers the same way for both axes ⇒ the counter is wrongly declined ⇒
///    assertion (1) FLIPS. Axis-specificity is load-bearing.
///  - make `boundary_declines` return `false` for `PersistentAxisMaterialization::Life` ⇒ the
///    observed life wrongly batches (+15) ⇒ assertion (2) FLIPS.
///
/// The token mint (assertion 3) is the positive reach-guard proving the submit ran past any
/// short-circuit; no assertion is vacuous.
#[test]
fn real_4p_boundary_collapse_batches_unobserved_counter_and_declines_observed_life() {
    use engine::analysis::resource::{CounterClass, ObjectClass, ResourceAxis};
    use engine::types::counter::CounterType;
    use engine::types::game_state::CounterGrowth;

    let mut state: GameState = serde_json::from_str(&OFFER_STATE)
        .expect("the real 4p offer dump must deserialize into the current GameState");
    drive_all_accept_n(&mut state, 5);

    // Graft a beneficial +1/+1 counter axis (UNOBSERVED on this board) and a life axis (OBSERVED)
    // onto the accepted token loop — the SAME single-authority writers the accept path uses.
    let creature = *p0_saproling_ids(&state)
        .iter()
        .next()
        .expect("P0 controls at least one Saproling to bear a +1/+1 counter");
    let base_counters = 1u32;
    state
        .objects
        .get_mut(&creature)
        .unwrap()
        .counters
        .insert(CounterType::Plus1Plus1, base_counters);
    let p0_life_before = state.players.iter().find(|p| p.id == P0).unwrap().life;

    state.mark_unbounded_loop(
        P0,
        &[
            ResourceAxis::Counter(CounterClass::Plus1Plus1, ObjectClass::Creature),
            ResourceAxis::Life(P0),
        ],
    );
    state.register_pending_materialization(
        P0,
        PersistentAxisMaterialization::Counters(vec![CounterGrowth {
            object: creature,
            counter: CounterType::Plus1Plus1,
            per_cycle_delta: 2,
        }]),
    );
    state.register_pending_materialization(
        P0,
        PersistentAxisMaterialization::Life {
            player: P0,
            per_cycle_delta: 3,
        },
    );

    drive_priority_to_next_boundary(&mut state);
    assert!(
        matches!(
            state.waiting_for,
            WaitingFor::PayAmountChoice { player, resource: PayableResource::LoopCollapse { .. }, .. }
                if player == P0
        ),
        "the boundary must prompt P0 for the multi-axis LoopCollapse count, got {:?}",
        state.waiting_for
    );

    let saps_before = p0_saproling_ids(&state);
    apply(&mut state, P0, GameAction::SubmitPayAmount { amount: 5 })
        .expect("P0 submits the finite multi-axis loop-collapse count");

    // (1) COUNTER axis (UNOBSERVED): +1/+1 grew by N×δ = 5×2 = 10 (base 1 → 11). The batched-path
    //     DISCRIMINATOR + axis-specificity discriminator (a coarse OR would veto this too).
    assert_eq!(
        state
            .objects
            .get(&creature)
            .unwrap()
            .counters
            .get(&CounterType::Plus1Plus1)
            .copied()
            .unwrap_or(0),
        base_counters + 5 * 2,
        "SubmitPayAmount{{5}} adds 5×2 = 10 +1/+1 counters (unobserved axis batches)"
    );
    // (2) LIFE axis (OBSERVED on the real board): DECLINED — life UNCHANGED, axis stays ∞. The
    //     finding-#4 re-check DISCRIMINATOR (delete it ⇒ +15 wrongly applies).
    assert_eq!(
        state.players.iter().find(|p| p.id == P0).unwrap().life,
        p0_life_before,
        "the real board's life observer ⇒ the batched life collapse is DECLINED (unchanged)"
    );
    // (3) TOKEN axis still mints N (positive reach-guard; multi-item dispatch unregressed).
    assert_eq!(
        p0_saproling_ids(&state).len(),
        saps_before.len() + 5,
        "5 tapped Saproling tokens mint alongside the counter collapse"
    );
    // (4) Axis-scoped cash-out: the collapsed counter axis is gone, the DECLINED life axis stays ∞.
    assert!(
        state
            .unbounded_resources
            .get(&P0)
            .is_some_and(|a| a.contains(&ResourceAxis::Life(P0))),
        "the declined life axis stays ∞-marked for manual play (CR 732.1b — the shortcut \
         system determines how the loop is broken; see BoundaryHold::ObservedGrowth)"
    );
    assert!(
        !state
            .unbounded_resources
            .get(&P0)
            .is_some_and(|a| a.contains(&ResourceAxis::Counter(
                CounterClass::Plus1Plus1,
                ObjectClass::Creature
            ))),
        "the collapsed counter axis cashes out of the ∞ status"
    );
    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { .. }),
        "the boundary fixpoint restores Priority, got {:?}",
        state.waiting_for
    );
}

/// PR-7 v4 (CR 732.2a) — FINDING #4 (accept→boundary observer-drift): the observed-growth firewall
/// runs at ACCEPT, but the controller could cast an observer of the growing class BEFORE the
/// boundary. Because the batched `apply_counter_addition` bypasses the counter doubler pipeline, a
/// lump N×δ apply would mis-honor a newly-present observer. The submit handler RE-CHECKS the
/// firewall per-axis and DECLINES the batched COUNTER collapse when an observer appeared, leaving
/// the ∞ axis for manual play — unambiguously sound. CR 732.1a/1b FRAME THAT DECLINE: the engine
/// is the table's shortcut system and determines how the elided loop is broken; the full statement
/// lives on `engine_resolution_choices::BoundaryHold::ObservedGrowth`.
///
/// MATCHED PAIR with `real_4p_boundary_collapse_batches_unobserved_counter_and_declines_observed_life`
/// (no counter observer ⇒ the counter batches 5×2): the SAME grafted +1/+1 counter loop, WITH a
/// `CounterAdded` observer (Corpsejack-like) grafted into the accept→boundary window, DECLINES
/// (counter unchanged, axis stays ∞). MEASURED: this fixture's post-accept
/// `counter_growth_is_observed == false`, so WITHOUT the graft the counter batches — the graft is
/// LOAD-BEARING (the drift, not an incidental board observer, flips the outcome). REVERT-PROBE
/// (discriminating): delete the `if boundary_declines(item, observed) { continue; }` line ⇒ the
/// batched counter wrongly grows (+10) and the axis clears ⇒ assertion (1) FLIPS. The token mint
/// (assertion 2) is the positive reach-guard proving the submit ran past the short-circuit.
///
/// This fn is ALSO the `unbounded-declined-wire.json` golden emitter. Its write ordering is
/// load-bearing — see the ordering rule at the wire pin below (PART 1), and the general statement
/// of it in `kilo_live_offer_from_real_dump.rs`.
#[test]
fn real_4p_counter_observer_drift_in_window_declines_batched_counter_but_still_mints_tokens() {
    use engine::analysis::resource::{CounterClass, ObjectClass, ResourceAxis};
    use engine::types::ability::TriggerDefinition;
    use engine::types::counter::CounterType;
    use engine::types::game_state::CounterGrowth;
    use engine::types::triggers::TriggerMode;

    let mut state: GameState = serde_json::from_str(&OFFER_STATE)
        .expect("the real 4p offer dump must deserialize into the current GameState");
    drive_all_accept_n(&mut state, 5);

    // Graft a +1/+1 counter axis (UNOBSERVED at accept — MEASURED counter_growth_is_observed=false).
    let creature = *p0_saproling_ids(&state)
        .iter()
        .next()
        .expect("P0 controls at least one Saproling to bear a +1/+1 counter");
    let base_counters = 1u32;
    state
        .objects
        .get_mut(&creature)
        .unwrap()
        .counters
        .insert(CounterType::Plus1Plus1, base_counters);
    state.mark_unbounded_loop(
        P0,
        &[ResourceAxis::Counter(
            CounterClass::Plus1Plus1,
            ObjectClass::Creature,
        )],
    );
    state.register_pending_materialization(
        P0,
        PersistentAxisMaterialization::Counters(vec![CounterGrowth {
            object: creature,
            counter: CounterType::Plus1Plus1,
            per_cycle_delta: 2,
        }]),
    );

    // FINDING #4: simulate the controller casting a counter observer (Corpsejack) in the
    // accept→boundary window — attach a `CounterAdded` trigger to a P0 battlefield permanent AFTER
    // the accept-time firewall already ran. WITHOUT this graft the counter batches (matched pair).
    let observer_host = *p0_saproling_ids(&state)
        .iter()
        .find(|id| **id != creature)
        .unwrap_or(&creature);
    state
        .objects
        .get_mut(&observer_host)
        .unwrap()
        .trigger_definitions = vec![TriggerDefinition::new(TriggerMode::CounterAdded)].into();

    // PRE-BOUNDARY CAPTURE — a local, deliberately NOT an assertion. `derive_views` takes
    // `&GameState`, so this cannot move the stash. Its assertion (M2-a) sits BELOW the golden
    // WRITE: see the ordering rule at the wire pin, PART 1.
    let pre_boundary_families = derive_views(&state, None).unbounded_families;

    drive_priority_to_next_boundary(&mut state);
    assert!(
        matches!(
            state.waiting_for,
            WaitingFor::PayAmountChoice { player, resource: PayableResource::LoopCollapse { .. }, .. }
                if player == P0
        ),
        "the token axis still prompts LoopCollapse at the boundary, got {:?}",
        state.waiting_for
    );

    let saps_before = p0_saproling_ids(&state);
    apply(&mut state, P0, GameAction::SubmitPayAmount { amount: 5 })
        .expect("P0 submits the loop-collapse count");

    // Cross-seam wire pin, PART 1 — compute + (optionally) REGENERATE `unbounded-declined-wire.json`,
    // the POST-DECLINE frame the client's badge test reads. Only the two keys the FE test consumes
    // are lifted, so unrelated derived-view churn cannot move this golden. The WRITE precedes M2-a
    // and M2-b below deliberately: an assert panic aborts the test, so an assertion placed above
    // the write would make the client-side half of its own revert probe unreachable.
    let views = derive_views(&state, None);
    let wire = serde_json::to_value(&views).expect("derived views serialize");
    let golden: serde_json::Map<String, serde_json::Value> =
        ["unbounded_resources", "unbounded_families"]
            .into_iter()
            .filter_map(|k| wire.get(k).map(|v| (k.to_string(), v.clone())))
            .collect();
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../client/src/test/fixtures/unbounded-declined-wire.json"
    );
    if std::env::var_os("UPDATE_WIRE_GOLDEN").is_some() {
        std::fs::create_dir_all(
            std::path::Path::new(path)
                .parent()
                .expect("golden has a parent"),
        )
        .expect("create the client wire-golden directory");
        std::fs::write(
            path,
            format!("{}\n", serde_json::to_string_pretty(&golden).unwrap()),
        )
        .expect("write the wire golden");
    }

    // M2-a — the accept→boundary window, read off the PRE-BOUNDARY capture. THE MED-2 ENGINE
    // DISCRIMINATOR: while the collapse is merely staged the badge must already say the promise is
    // conditional, because this very fixture is the case where it will not be kept.
    // MUTATION: `Counters => None` in `possible_hold` ⇒ `Scheduled(Committed)` ⇒ RED.
    // This sits AFTER the WRITE so a mutation that reds it can still regenerate the golden —
    // M2-d(b) depends on that.
    assert!(
        pre_boundary_families.iter().any(|f| f.player == P0
            && f.family == UnboundedFamily::Counters
            && f.state
                == FamilyCollapseState::Scheduled {
                    certainty: CollapseCertainty::Conditional,
                    prompted: Some(P0),
                }),
        "in the accept→boundary window the counters family is Scheduled(Conditional) — a batched \
         Counters collapse can still be declined, so ∞→? not ∞→N; got {pre_boundary_families:?}"
    );

    // M2-b — the POST-decline frame. The axis is still ∞ (assertion (1) below pins the store) but
    // the stash is gone, so the badge stops promising anything at all.
    // MUTATION: make `scheduled_display_axes` read `unbounded_resources` instead of the stash ⇒
    // the declined family stays `Scheduled` ⇒ RED here AND in the regenerated golden.
    // Also AFTER the WRITE, for the same reason.
    assert!(
        views.unbounded_families.iter().any(|f| f.player == P0
            && f.family == UnboundedFamily::Counters
            && f.state == FamilyCollapseState::Unscheduled),
        "after the decline the counters axis is still ∞ but nothing is staged, so its family is \
         Unscheduled and the badge renders a bare ∞; got {:?}",
        views.unbounded_families
    );

    // (1) DISCRIMINATOR: the batched counter collapse is DECLINED (an observer appeared in the
    //     window) — +1/+1 UNCHANGED, and the counter ∞ axis stays MARKED for manual play.
    assert_eq!(
        state
            .objects
            .get(&creature)
            .unwrap()
            .counters
            .get(&CounterType::Plus1Plus1)
            .copied()
            .unwrap_or(0),
        base_counters,
        "a counter observer drifted into the accept→boundary window ⇒ batched counter DECLINED"
    );
    assert!(
        state
            .unbounded_resources
            .get(&P0)
            .is_some_and(|a| a.contains(&ResourceAxis::Counter(
                CounterClass::Plus1Plus1,
                ObjectClass::Creature
            ))),
        "the declined counter axis stays ∞-marked for manual play (CR 732.1b — the shortcut \
         system determines how the loop is broken; see BoundaryHold::ObservedGrowth)"
    );
    // (2) POSITIVE reach-guard: the Tokens axis STILL mints N (tokens honor observers via real ETB
    //     events, so they always proceed) — proves the submit ran and the negative is non-vacuous.
    assert_eq!(
        p0_saproling_ids(&state).len(),
        saps_before.len() + 5,
        "the token axis still mints 5 (only the observer-drifted counter axis is declined)"
    );

    // Cross-seam wire pin, PART 2 — the drift COMPARE (see PART 1 for why it sits here).
    let committed: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(path).expect("committed wire golden"))
            .unwrap();
    assert_eq!(
        serde_json::Value::Object(golden),
        committed,
        "the client's declined wire golden drifted from engine output — re-run with \
         UPDATE_WIRE_GOLDEN=1"
    );
}

// ─────────── CR 732.2a: LoopCollapse prompt axis-label derivation ───────────

/// Drive the REAL production boundary and return the axis label the `LoopCollapse`
/// prompt carries (`turns.rs` derives it from the controller's stash at construction).
fn collapse_axis_at_boundary(state: &mut GameState) -> LoopCollapseAxis {
    drive_priority_to_next_boundary(state);
    match &state.waiting_for {
        WaitingFor::PayAmountChoice {
            resource: PayableResource::LoopCollapse { axis },
            player,
            ..
        } if *player == P0 => *axis,
        other => panic!("expected P0's LoopCollapse boundary prompt, got {other:?}"),
    }
}

/// T2 (TOKENS): the natural accept path stashes a `Tokens` materialization, so a pure
/// token loop labels its collapse prompt `Tokens` — the whole-production path, no graft.
///
/// REVERT-PROBE: change `LoopCollapseAxis::from_materializations` to `return
/// LoopCollapseAxis::Mixed;` ⇒ this assertion FLIPS. (Reverting to the OLD always-Tokens
/// behavior leaves T2 green — T1/T3/T4 are the discriminators that catch that revert.)
#[test]
fn loop_collapse_prompt_labels_token_axis_tokens() {
    let mut state: GameState = serde_json::from_str(&OFFER_STATE)
        .expect("the real 4p offer dump must deserialize into the current GameState");
    drive_all_accept(&mut state);
    // Reach-guard: the accept stashed the token materialization (non-vacuity anchor).
    assert!(
        state.pending_unbounded_materialization.contains_key(&P0),
        "accepting the object-growth loop stashes P0's token materialization"
    );
    assert_eq!(
        collapse_axis_at_boundary(&mut state),
        LoopCollapseAxis::Tokens,
        "a pure token loop labels the collapse prompt Tokens"
    );
}

/// T1 (COUNTERS — PRIMARY discriminator): a pure counter loop labels its collapse prompt
/// `Counters`, NOT the old hardcoded `Tokens`. This is the exact defect the fix targets
/// (the prompt used to always say "tokens" even for a counter loop).
///
/// REVERT-PROBE: change `from_materializations` to `return LoopCollapseAxis::Tokens;`
/// (the pre-fix behavior) ⇒ this assertion FLIPS from Counters to Tokens.
#[test]
fn loop_collapse_prompt_labels_counter_axis_counters() {
    use engine::types::counter::CounterType;
    use engine::types::game_state::CounterGrowth;

    let mut state: GameState = serde_json::from_str(&OFFER_STATE)
        .expect("the real 4p offer dump must deserialize into the current GameState");
    drive_all_accept(&mut state);
    let creature = *p0_saproling_ids(&state)
        .iter()
        .next()
        .expect("P0 controls at least one Saproling to bear a counter axis");
    // Replace the natural token stash with a PURE counter materialization (the single-
    // authority writer the accept path uses).
    state.pending_unbounded_materialization.clear();
    state.register_pending_materialization(
        P0,
        PersistentAxisMaterialization::Counters(vec![CounterGrowth {
            object: creature,
            counter: CounterType::Plus1Plus1,
            per_cycle_delta: 2,
        }]),
    );
    assert_eq!(
        collapse_axis_at_boundary(&mut state),
        LoopCollapseAxis::Counters,
        "a pure counter loop labels the collapse prompt Counters (revert to Tokens ⇒ flips)"
    );
}

/// T3 (LIFE): a pure life-gain loop labels its collapse prompt `Life`.
///
/// REVERT-PROBE: `from_materializations` → `return LoopCollapseAxis::Tokens;` ⇒ FLIPS.
#[test]
fn loop_collapse_prompt_labels_life_axis_life() {
    let mut state: GameState = serde_json::from_str(&OFFER_STATE)
        .expect("the real 4p offer dump must deserialize into the current GameState");
    drive_all_accept(&mut state);
    state.pending_unbounded_materialization.clear();
    state.register_pending_materialization(
        P0,
        PersistentAxisMaterialization::Life {
            player: P0,
            per_cycle_delta: 3,
        },
    );
    assert_eq!(
        collapse_axis_at_boundary(&mut state),
        LoopCollapseAxis::Life,
        "a pure life loop labels the collapse prompt Life"
    );
}

/// T4 (MIXED): a loop that collapses two distinct axes at once (counter + life, the same
/// two-axis shape the batched-collapse test grafts) labels its prompt `Mixed`.
///
/// REVERT-PROBE: change the `≥2 → Mixed` fold to return the first axis ⇒ this assertion
/// FLIPS from Mixed to Counters.
#[test]
fn loop_collapse_prompt_labels_multi_axis_mixed() {
    use engine::types::counter::CounterType;
    use engine::types::game_state::CounterGrowth;

    let mut state: GameState = serde_json::from_str(&OFFER_STATE)
        .expect("the real 4p offer dump must deserialize into the current GameState");
    drive_all_accept(&mut state);
    let creature = *p0_saproling_ids(&state)
        .iter()
        .next()
        .expect("P0 controls at least one Saproling to bear a counter axis");
    state.pending_unbounded_materialization.clear();
    state.register_pending_materialization(
        P0,
        PersistentAxisMaterialization::Counters(vec![CounterGrowth {
            object: creature,
            counter: CounterType::Plus1Plus1,
            per_cycle_delta: 1,
        }]),
    );
    state.register_pending_materialization(
        P0,
        PersistentAxisMaterialization::Life {
            player: P0,
            per_cycle_delta: 1,
        },
    );
    assert_eq!(
        collapse_axis_at_boundary(&mut state),
        LoopCollapseAxis::Mixed,
        "a two-axis (counter + life) loop labels the collapse prompt Mixed (first-axis-wins ⇒ flips)"
    );
}

/// UNIT (CR 732.2a): `LoopCollapseAxis::from_materializations` maps each stash shape to its
/// label — including the LOAD-BEARING observed-growth `DriveSequence → axis` path (the Kilo
/// combo pushes a single `DriveSequence` with `Counter(Other, Other)`, NOT a batched
/// `Counters` item; a derivation that ignored `DriveSequence` would mislabel Kilo `Mixed`).
///
/// REVERT-PROBE: `from_materializations` → `return LoopCollapseAxis::Tokens;` ⇒ every
/// non-Tokens assertion FLIPS. Removing the `DriveSequence` arm ⇒ the two drive-sequence
/// assertions FLIP to Mixed.
#[test]
fn loop_collapse_axis_from_materializations_maps_each_shape() {
    use engine::analysis::resource::{CounterClass, ObjectClass, ResourceAxis};
    use engine::types::counter::CounterType;
    use engine::types::game_state::CounterGrowth;
    use engine::types::mana::ManaType;

    let counters = [PersistentAxisMaterialization::Counters(vec![
        CounterGrowth {
            object: ObjectId(1),
            counter: CounterType::Plus1Plus1,
            per_cycle_delta: 1,
        },
    ])];
    assert_eq!(
        LoopCollapseAxis::from_materializations(&counters),
        LoopCollapseAxis::Counters
    );

    let life = [PersistentAxisMaterialization::Life {
        player: P0,
        per_cycle_delta: 1,
    }];
    assert_eq!(
        LoopCollapseAxis::from_materializations(&life),
        LoopCollapseAxis::Life
    );

    // LOAD-BEARING: the observed-growth DriveSequence carrying the Kilo counter axis maps
    // to Counters (not Mixed) — the single-DriveSequence shape the flagship combo pushes.
    let drive_counter = [PersistentAxisMaterialization::DriveSequence {
        sequence: vec![],
        collapsed_axes: vec![ResourceAxis::Counter(
            CounterClass::Other,
            ObjectClass::Other,
        )],
    }];
    assert_eq!(
        LoopCollapseAxis::from_materializations(&drive_counter),
        LoopCollapseAxis::Counters,
        "the flagship Kilo DriveSequence(Counter) labels Counters, not Mixed"
    );

    // The Tokens mapping via the DriveSequence path (a TokensCreated observed loop).
    let drive_tokens = [PersistentAxisMaterialization::DriveSequence {
        sequence: vec![],
        collapsed_axes: vec![ResourceAxis::TokensCreated],
    }];
    assert_eq!(
        LoopCollapseAxis::from_materializations(&drive_tokens),
        LoopCollapseAxis::Tokens
    );

    // Two distinct axes → Mixed.
    let mixed = [
        PersistentAxisMaterialization::Life {
            player: P0,
            per_cycle_delta: 1,
        },
        PersistentAxisMaterialization::Counters(vec![CounterGrowth {
            object: ObjectId(1),
            counter: CounterType::Plus1Plus1,
            per_cycle_delta: 1,
        }]),
    ];
    assert_eq!(
        LoopCollapseAxis::from_materializations(&mixed),
        LoopCollapseAxis::Mixed
    );

    // Empty stash → Mixed (defensive).
    assert_eq!(
        LoopCollapseAxis::from_materializations(&[]),
        LoopCollapseAxis::Mixed
    );

    // A non-materializable DriveSequence axis contributes no label → Mixed (defensive).
    let drive_mana = [PersistentAxisMaterialization::DriveSequence {
        sequence: vec![],
        collapsed_axes: vec![ResourceAxis::Mana(ManaType::Colorless)],
    }];
    assert_eq!(
        LoopCollapseAxis::from_materializations(&drive_mana),
        LoopCollapseAxis::Mixed
    );
}

/// [MED] (CR 732.2a defense-in-depth): the `Tokens` boundary-mint arm must early-return when the
/// copy-token mint PAUSES for a replacement choice, preserving the replacement `waiting_for` and
/// the paused `pending_copy_token_resolution` instead of advancing the phase / overwriting
/// `waiting_for = Priority`.
///
/// DELIBERATELY FIREWALL-UNREACHABLE: the offer firewall (`game/engine.rs`'s
/// `drive_loop_action_iteration`, whose exhaustive fail-closed `_ => Err(RecastAbort)` arm has no
/// replacement-/target-choice branch) guarantees a certified shortcut's per-cycle fodder mint cannot
/// pause, so this state cannot arise in real play. The test constructs it directly — installing an
/// OPTIONAL token-creation replacement (CR 616.1 single optional candidate → `replace_event` returns
/// `NeedsChoice` from `game/replacement.rs`'s `replacement_is_optional` single-candidate branch) on a P0
/// battlefield object AFTER accept — to exercise the defensive guard. (Two IDENTICAL replacements
/// would be immaterially ordered and auto-resolve with NO pause, making the test vacuous; a single
/// MANDATORY replacement applies without a pause — hence a single OPTIONAL candidate.)
///
/// NON-VACUITY REACH-GUARD: the first assertion checks the mint actually paused
/// (`pending_copy_token_resolution.is_some()`), so a fizzled/auto-resolved mint (non-matching or
/// immaterial replacement) fails loudly rather than passing vacuously.
///
/// REVERT-FAILING assertion (MEASURED, implementer-run revert-probe): delete the guard → the arm
/// falls through to `collapsed.push(Tokens)` + `clear_collapsed_materializations`, which cashes out
/// the Tokens ∞ axis (`TokensCreated`) and DROPS the ∞ token pile — even though the paused mint
/// created ZERO tokens. So the guard's load-bearing effect is PRESERVING the ∞ axis/pile on a
/// paused mint; the revert-probe flips the `unbounded_resources`/`unbounded_loop_pile` assertions
/// below (the phase-drain is a no-op while a replacement choice is pending, so `waiting_for` stays
/// `ReplacementChoice` either way — that is a correctness sanity check, not the discriminator).
#[test]
fn med_tokens_boundary_mint_pause_preserves_replacement_choice() {
    use engine::analysis::resource::ResourceAxis;
    use engine::types::ability::{QuantityModification, ReplacementDefinition, ReplacementMode};
    use engine::types::replacements::ReplacementEvent;
    use std::sync::Arc;

    let mut state: GameState = serde_json::from_str(&OFFER_STATE)
        .expect("the real 4p offer dump must deserialize into the current GameState");
    drive_all_accept_n(&mut state, 3);

    // Install an OPTIONAL token-count-doubling replacement ("you may create twice that many tokens
    // instead", CR 616.1) on a fresh P0 battlefield permanent — AFTER accept, so it never perturbs
    // the accept-time fodder-derivation clone-drive (installing it before accept would abort
    // certification at the firewall, which is exactly the invariant this guard backstops). At the
    // boundary Tokens mint, `replace_event` sees ONE optional candidate for the copy-token
    // `CreateToken` event → returns `NeedsChoice` → `drive_copy_token_batches` sets
    // `pending_copy_token_resolution` AND `waiting_for = ReplacementChoice`.
    let doubler = create_object(
        &mut state,
        CardId(9001),
        P0,
        "Optional Token Doubler".to_string(),
        Zone::Battlefield,
    );
    let mut def = ReplacementDefinition::new(ReplacementEvent::CreateToken);
    def.mode = ReplacementMode::Optional { decline: None };
    def.quantity_modification = Some(QuantityModification::DOUBLE);
    let reps = vec![def];
    let obj = state.objects.get_mut(&doubler).unwrap();
    obj.replacement_definitions = reps.clone().into();
    obj.base_replacement_definitions = Arc::new(reps);

    drive_priority_to_next_boundary(&mut state);
    assert!(
        matches!(
            state.waiting_for,
            WaitingFor::PayAmountChoice { player, resource: PayableResource::LoopCollapse { .. }, .. }
                if player == P0
        ),
        "the boundary must prompt P0 for the Tokens LoopCollapse count, got {:?}",
        state.waiting_for
    );
    // Pre-submit reach-guards: the accepted token loop marked the Tokens ∞ axis + a non-empty ∞
    // pile — the capability the paused mint must NOT cash out. (Non-vacuity: if these were absent
    // the preservation assertions below would be trivially satisfiable.)
    assert!(
        state
            .unbounded_resources
            .get(&P0)
            .is_some_and(|a| a.contains(&ResourceAxis::TokensCreated)),
        "pre-submit reach-guard: the accepted token loop marks the TokensCreated ∞ axis"
    );
    assert!(
        state
            .unbounded_loop_pile
            .get(&P0)
            .is_some_and(|p| !p.is_empty()),
        "pre-submit reach-guard: the accepted token loop has a non-empty ∞ pile"
    );
    // M2-c — THE EXACT FRAME WHERE THE OLD BADGE LIED. At this pre-submit frame the loop really is
    // scheduled, and the old row flag made the HUD promise "a finite amount will be chosen". This
    // very test then proves that the mint parks and NOTHING is chosen. `Conditional` is what makes
    // the badge honest here.
    // MUTATION: `Tokens => None` in `possible_hold` ⇒ this reports Scheduled(Committed) ⇒ RED.
    assert!(
        derive_views(&state, None)
            .unbounded_families
            .iter()
            .any(|f| f.player == P0
                && f.family == UnboundedFamily::Tokens
                && f.state
                    == FamilyCollapseState::Scheduled {
                        certainty: CollapseCertainty::Conditional,
                        prompted: Some(P0),
                    }),
        "pre-submit: an accepted Tokens collapse is Scheduled(Conditional) — its boundary mint can \
         park on a replacement choice, which is exactly what happens below; got {:?}",
        derive_views(&state, None).unbounded_families
    );

    apply(&mut state, P0, GameAction::SubmitPayAmount { amount: 3 })
        .expect("P0 submits the finite token loop-collapse count");

    // Non-vacuity reach-guard: the mint actually paused for the replacement (proves the OPTIONAL
    // replacement matched the synthetic mint; a non-matching / auto-resolving construction leaves
    // this None and fails the test loudly).
    assert!(
        state.active_copy_token().is_some(),
        "reach-guard: the boundary Tokens mint paused on the optional replacement \
         (pending_copy_token_resolution set)"
    );
    // Correctness sanity (holds with AND without the guard — the phase-drain no-ops while a
    // replacement choice is pending): the replacement prompt is surfaced, not clobbered to Priority.
    assert!(
        matches!(state.waiting_for, WaitingFor::ReplacementChoice { .. }),
        "the paused replacement choice is surfaced, got {:?}",
        state.waiting_for
    );
    // DISCRIMINATOR (revert-flip): the paused mint created ZERO tokens, so it must NOT cash out
    // the Tokens ∞ capability. Without the guard, `collapsed.push(Tokens)` +
    // `clear_collapsed_materializations` remove the TokensCreated axis and drop the ∞ pile ⇒ both
    // assertions FLIP to FAIL (measured via implementer revert-probe).
    assert!(
        state
            .unbounded_resources
            .get(&P0)
            .is_some_and(|a| a.contains(&ResourceAxis::TokensCreated)),
        "REVERT-FLIP: the paused mint must preserve the Tokens ∞ axis, not cash it out for a \
         batch that minted zero tokens"
    );
    assert!(
        state
            .unbounded_loop_pile
            .get(&P0)
            .is_some_and(|p| !p.is_empty()),
        "REVERT-FLIP: the paused mint must preserve the ∞ token pile, not drop it"
    );
    // M2-c, post-pause — the badge stops promising. This stands alone as a WRONG-AUTHORITY
    // detector (a badge reading `unbounded_resources` instead of the stash would still say
    // `Scheduled` here); it is deliberately NOT an argument that `Tokens` is `Committed`.
    // `take_pending_materialization` removes the WHOLE controller list, so a declined `Counters`
    // axis reports `Unscheduled` at this point identically — the symmetry is the point.
    assert!(
        derive_views(&state, None)
            .unbounded_families
            .iter()
            .any(|f| f.player == P0
                && f.family == UnboundedFamily::Tokens
                && f.state == FamilyCollapseState::Unscheduled),
        "post-pause: the axis is still ∞ but the stash is gone, so the tokens family promises \
         nothing; got {:?}",
        derive_views(&state, None).unbounded_families
    );

    // PRODUCTION-REACHABILITY ANCHOR for the "pile present, stash absent" shape. Several R6a
    // rows build that shape by cloning a post-accept state and clearing
    // `pending_unbounded_materialization` by hand; these two lines assert the ENGINE produces it
    // unaided, right here, so those clones are grounded in a measured production sequence rather
    // than in a comment. The sequence is the CR 616.1 mint-pause above:
    // `SubmitPayAmount` → `take_pending_materialization` (removes the whole stash) →
    // `engine_resolution_choices`' pause guard → `clear_collapsed_materializations(player,
    // &collapsed)` with the paused `Tokens` item ABSENT from `collapsed` ⇒ the pile survives
    // while the stash is gone.
    assert!(
        !state.pending_unbounded_materialization.contains_key(&P0),
        "the submit's take_pending_materialization removed P0's stash, got {:?}",
        state.pending_unbounded_materialization.get(&P0)
    );
    assert!(
        !derive_views(&state, Some(P0)).unbounded_pile.is_empty(),
        "…and with no stash left to schedule a collapse, the ∞ pile renders on the WIRE — the \
         engine-reachable 'pile present, stash absent' shape the clone-based R6a arms emulate"
    );
}

/// [BLOCKER] (#6259 review, CR 732.2a pause-safety): a MIXED stash pausing on the `Tokens`
/// axis must NOT strand a finite-applied `Counters` axis with a stale ∞ mark, and must not
/// skip the deterministic `Counters` axis depending on stash registration order. Before this
/// fix, production registers `Tokens` before `Counters` (Tokens→Counters→Life in
/// `materialize_object_growth_shortcut`), so the `for item in &items` loop hit `Tokens` FIRST —
/// a pausing Tokens mint early-returned before `Counters` ever ran on this pass, and whatever
/// HAD already landed in `collapsed` was never cashed out, leaving stale ∞ marks.
///
/// FIX: Edit 1 sorts `items` so the ONLY pause-prone axis (`Tokens`) runs LAST regardless of
/// registration order. Edit 2 calls `clear_collapsed_materializations(player, &collapsed)` at
/// the `Tokens` pause guard, cashing out whatever finite axes DID commit before the pause.
///
/// CONSTRUCTION: real 4p offer dump → real accept (registers the `Tokens` stash for P0) →
/// graft an UNOBSERVED `Counters` axis onto a P0 Saproling (the same single-authority
/// `mark_unbounded_loop` + `register_pending_materialization` writers the accept path uses, per
/// `real_4p_boundary_collapse_batches_unobserved_counter_and_declines_observed_life`) → install
/// the OPTIONAL token-doubler replacement AFTER accept (per
/// `med_tokens_boundary_mint_pause_preserves_replacement_choice`) so the boundary `Tokens` mint
/// PAUSES on a `NeedsChoice` replacement choice → drive to the boundary → `SubmitPayAmount{4}`.
///
/// REVERT-FLIPS (MEASURED, implementer-run revert-probe — see report):
///  - delete Edit 2's `clear_collapsed_materializations` call at the pause guard ⇒ the
///    collapsed `Counters` axis is never cashed out ⇒ assertion (3)'s "Counter axis gone"
///    FLIPS to FAIL (stale ∞ left on the applied counter axis).
///  - delete Edit 1's `sort_by_key` ⇒ `Tokens` (registered by the accept path before this
///    test's `Counters` graft) processes FIRST, pauses at index 0, and `Counters` is NEVER
///    reached ⇒ assertion (2) FLIPS to FAIL (counter stays at base, unchanged).
#[test]
fn med_mixed_counter_tokens_pause_commits_finite_counter_and_keeps_only_tokens_unbounded() {
    use engine::analysis::resource::{CounterClass, ObjectClass, ResourceAxis};
    use engine::types::ability::{QuantityModification, ReplacementDefinition, ReplacementMode};
    use engine::types::counter::CounterType;
    use engine::types::game_state::CounterGrowth;
    use engine::types::replacements::ReplacementEvent;
    use std::sync::Arc;

    let mut state: GameState = serde_json::from_str(&OFFER_STATE)
        .expect("the real 4p offer dump must deserialize into the current GameState");
    drive_all_accept_n(&mut state, 4);

    // Graft an UNOBSERVED +1/+1 counter axis onto a P0 Saproling — the same single-authority
    // writers the accept path uses (mirrors
    // real_4p_boundary_collapse_batches_unobserved_counter_and_declines_observed_life).
    let creature = *p0_saproling_ids(&state)
        .iter()
        .next()
        .expect("P0 controls at least one Saproling to bear a +1/+1 counter");
    let base_counters = 1u32;
    state
        .objects
        .get_mut(&creature)
        .unwrap()
        .counters
        .insert(CounterType::Plus1Plus1, base_counters);
    state.mark_unbounded_loop(
        P0,
        &[ResourceAxis::Counter(
            CounterClass::Plus1Plus1,
            ObjectClass::Creature,
        )],
    );
    state.register_pending_materialization(
        P0,
        PersistentAxisMaterialization::Counters(vec![CounterGrowth {
            object: creature,
            counter: CounterType::Plus1Plus1,
            per_cycle_delta: 2,
        }]),
    );

    // Install the OPTIONAL token-doubler replacement AFTER accept (mirrors
    // med_tokens_boundary_mint_pause_preserves_replacement_choice) so the boundary Tokens mint
    // pauses on a NeedsChoice replacement instead of completing.
    let doubler = create_object(
        &mut state,
        CardId(9002),
        P0,
        "Optional Token Doubler".to_string(),
        Zone::Battlefield,
    );
    let mut def = ReplacementDefinition::new(ReplacementEvent::CreateToken);
    def.mode = ReplacementMode::Optional { decline: None };
    def.quantity_modification = Some(QuantityModification::DOUBLE);
    let reps = vec![def];
    let obj = state.objects.get_mut(&doubler).unwrap();
    obj.replacement_definitions = reps.clone().into();
    obj.base_replacement_definitions = Arc::new(reps);

    drive_priority_to_next_boundary(&mut state);
    assert!(
        matches!(
            state.waiting_for,
            WaitingFor::PayAmountChoice { player, resource: PayableResource::LoopCollapse { .. }, .. }
                if player == P0
        ),
        "the boundary must prompt P0 for the mixed Counters+Tokens LoopCollapse count, got {:?}",
        state.waiting_for
    );

    apply(&mut state, P0, GameAction::SubmitPayAmount { amount: 4 })
        .expect("P0 submits the finite mixed-axis loop-collapse count");

    // (1) Reach-guard: the Tokens mint actually paused (proves the submit ran past the
    //     Counters axis and into the Tokens axis, not a fizzled/auto-resolved mint).
    assert!(
        state.active_copy_token().is_some(),
        "reach-guard: the boundary Tokens mint paused on the optional replacement"
    );
    assert!(
        matches!(state.waiting_for, WaitingFor::ReplacementChoice { .. }),
        "the paused replacement choice is surfaced, not clobbered, got {:?}",
        state.waiting_for
    );

    // (2) FINITE PRIOR EFFECT ONCE: the grafted counter axis committed exactly once — 4×2 = 8 —
    //     which only holds because Edit 1 processes Counters BEFORE the Tokens pause.
    assert_eq!(
        state
            .objects
            .get(&creature)
            .unwrap()
            .counters
            .get(&CounterType::Plus1Plus1)
            .copied()
            .unwrap_or(0),
        base_counters + 4 * 2,
        "SubmitPayAmount{{4}} commits the Counters axis exactly once (4×2 = 8) despite the \
         Tokens axis pausing later in the same pass"
    );

    // (3) ONLY Tokens ∞: the collapsed Counters axis is cashed out (Edit 2), the still-paused
    //     Tokens axis is NOT (its ∞ axis + pile survive for the eventual resume/manual play).
    assert!(
        !state
            .unbounded_resources
            .get(&P0)
            .is_some_and(|a| a.contains(&ResourceAxis::Counter(
                CounterClass::Plus1Plus1,
                ObjectClass::Creature
            ))),
        "REVERT-FLIP: the committed Counters axis must cash out of the ∞ status, not strand a \
         stale ∞ mark on a finite-applied axis"
    );
    assert!(
        state
            .unbounded_resources
            .get(&P0)
            .is_some_and(|a| a.contains(&ResourceAxis::TokensCreated)),
        "the still-paused Tokens axis stays ∞-marked (not yet collapsed)"
    );
    assert!(
        state
            .unbounded_loop_pile
            .get(&P0)
            .is_some_and(|p| !p.is_empty()),
        "the still-paused Tokens axis keeps its ∞ pile (not dropped mid-pause)"
    );

    // (4) Phase NOT advanced: the boundary drain is skipped while the replacement is pending —
    //     the paused prompt stays surfaced, not clobbered to Priority.
    assert!(
        !matches!(state.waiting_for, WaitingFor::Priority { .. }),
        "the phase drain must not run while the Tokens mint is mid-pause, got {:?}",
        state.waiting_for
    );
}

/// Activate `ability_index` on `source`, then pass priority until the stack settles empty at a
/// `Priority` window OR a `LoopShortcut` offer surfaces (mirrors the mana-engine harness).
fn low3_activate_and_settle(runner: &mut GameRunner, source: ObjectId, ability_index: usize) {
    runner
        .act(GameAction::ActivateAbility {
            source_id: source,
            ability_index,
        })
        .expect("activation is legal");
    for _ in 0..60 {
        match &runner.state().waiting_for {
            WaitingFor::LoopShortcut { .. } => break,
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
            _ => {}
        }
        if runner.act(GameAction::PassPriority).is_err() {
            break;
        }
    }
}

/// [LOW-3] E2E: accepting a REAL UNOBSERVED life-growth loop drives the accept-time production
/// routing in `materialize_object_growth_shortcut` (engine.rs) into the BATCHED `Life` branch —
/// the branch every prior Counters/Life collapse test GRAFTS via `register_pending_materialization`
/// (bypassing the real δ-capture + `life_growth_is_observed` decision). The OBSERVED (DriveSequence)
/// counter route is already covered by `kilo_accept_collapses_at_boundary_to_exactly_n_counters`
/// (kilo_live_offer_from_real_dump.rs); this closes the UNOBSERVED batched gap.
///
/// CONSTRUCTION (self-contained, no dump): a synthetic creature with an off-stack mana ability
/// (CR 605.3b — the only activation class that SEEDS a loop period; `game/engine.rs`'s `apply_action`
/// opens the period on that off-stack activation), plus a
/// free gain-life and a free untap. Ordered [mana, gain-life, untap] so the 2-step prefix leaves
/// the creature TAPPED — the offer's re-drive can't re-tap it, aborting any premature pure-mana
/// offer and forcing the life beat into the certified 3-step period. No `LifeChanged` trigger /
/// `GainLife` replacement / life-total reader is on the board ⇒ `life_growth_is_observed == false`
/// ⇒ the accept routes to the BATCHED `Life` stash.
///
/// REVERT-FAILING assertion: break the routing predicate — force the observed branch, or drop the
/// `else`/`if !life.is_empty()` batched registration in `materialize_object_growth_shortcut` — and
/// the produced stash shape changes (a `DriveSequence`, or an empty stash) ⇒ the batched-`Life`
/// assertion FLIPS. Non-vacuity reach-guards: the recorded 3-step period, the surfaced offer, and
/// the non-empty post-accept stash all gate the shape assertion.
#[test]
fn low3_unobserved_life_growth_accept_registers_batched_life() {
    use engine::analysis::resource::ResourceAxis;

    let runner = low3_life_engine_accepted(Low3BoardEtbTrigger::Absent);

    // Reach-guard: the accept produced a non-empty deferred stash (not a no-op).
    let stash = runner
        .state()
        .pending_unbounded_materialization
        .get(&P0)
        .cloned()
        .unwrap_or_default();
    assert!(
        !stash.is_empty(),
        "reach-guard: the accept must register a deferred materialization stash for P0"
    );
    // DISCRIMINATOR: the UNOBSERVED life growth routes to a BATCHED `Life` item carrying the
    // per-cycle δ — produced by the real accept-time routing, never grafted.
    assert!(
        stash.iter().any(|m| matches!(
            m,
            PersistentAxisMaterialization::Life { player, per_cycle_delta }
                if *player == P0 && *per_cycle_delta >= 1
        )),
        "the unobserved life loop must register a BATCHED Life stash (per_cycle_delta captured), \
         got {stash:?}"
    );
    // DISCRIMINATOR: an UNOBSERVED loop BATCHES — it must NOT register a DriveSequence (that is the
    // observed route; forcing the observed branch flips this).
    assert!(
        !stash
            .iter()
            .any(|m| matches!(m, PersistentAxisMaterialization::DriveSequence { .. })),
        "an unobserved life loop batches; it must not register a DriveSequence, got {stash:?}"
    );
    // The life axis is ∞-marked (the mana axis is too; both are real unbounded axes here).
    assert!(
        runner
            .state()
            .unbounded_resources
            .get(&P0)
            .is_some_and(|axes| axes.contains(&ResourceAxis::Life(P0))),
        "the accepted loop marks the Life axis ∞ for P0"
    );
}

/// Whether [`low3_life_engine_accepted`] installs a FUNCTIONING battlefield-entry trigger on the
/// board before the loop is driven. `Present` is the hostile arm for the `token_profile.is_some()`
/// conjunct of `life_etb_sourced` (engine.rs): it makes `board_has_functioning_etb_trigger` TRUE
/// while the loop stays mana-only, so the conjunct is the ONLY thing still holding the route on the
/// batched arm.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Low3BoardEtbTrigger {
    Absent,
    Present,
}

/// A minimal functioning battlefield-entry life trigger (the Prosperous Innkeeper shape, built
/// directly because this fixture is synthetic and loads no card pool). Its EFFECT is deliberately
/// `GainLife`: that is the production shape that double-pays when a `Tokens` collapse mints real
/// CR 603.6a entries, and it is NOT a life OBSERVER — `life_growth_is_observed` keys on
/// `TriggerEventKey::LifeChanged` / `ReplacementEvent::GainLife`, never on `EnterBattlefield` — so
/// installing it cannot flip `life_observed` and mask what this arm is measuring.
fn low3_board_etb_life_trigger() -> TriggerDefinition {
    use engine::types::ability::{AbilityDefinition, AbilityKind, QuantityExpr, TargetFilter};
    use engine::types::triggers::TriggerMode;

    let mut def = TriggerDefinition::new(TriggerMode::ChangesZone);
    def.destination = Some(Zone::Battlefield);
    def.trigger_zones = vec![Zone::Battlefield];
    def.execute = Some(Box::new(AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::GainLife {
            amount: QuantityExpr::Fixed { value: 1 },
            player: TargetFilter::Controller,
        },
    )));
    def.description =
        Some("Whenever another creature you control enters, you gain 1 life.".to_string());
    def
}

/// Build the LOW-3 mana+life engine, optionally graft a functioning board ETB trigger, drive one
/// certified period, and take the offer through the real APNAP accept. Returns the post-accept
/// runner. Shared so both arms inherit the same non-vacuity reach-guards (3-step period, surfaced
/// offer) — the only difference between them is `etb`.
fn low3_life_engine_accepted(etb: Low3BoardEtbTrigger) -> GameRunner {
    use engine::game::mana_abilities::is_mana_ability;
    use engine::types::ability::TapStateChange;

    let mut scenario = GameScenario::new_n_player(2, 7);
    scenario.at_phase(Phase::PreCombatMain);
    let engine_id = scenario
        .add_creature_from_oracle(
            P0,
            "Lifedynamo",
            2,
            2,
            "{T}: Add {C}.\n{0}: You gain 1 life.\n{0}: Untap Lifedynamo.",
        )
        .id();
    let mut runner = scenario.build();
    runner.state_mut().loop_detection = LoopDetectionMode::Interactive;
    if etb == Low3BoardEtbTrigger::Present {
        // Grafted BEFORE the drive so the board is identical every cycle (a permanent that never
        // changes cannot perturb the modulo-resources loop cover), and so the predicate the accept
        // reads is the one this board really has. Nothing enters the battlefield during the
        // mana+life period, so the trigger never actually fires — `board_has_functioning_etb_trigger`
        // is a board-shape question, not a fired-this-cycle one.
        let host = create_life_gainer(runner.state_mut(), P0, "Grafted Innkeeper");
        graft_trigger(runner.state_mut(), host, low3_board_etb_life_trigger());
        // Reach-guard: the graft must survive the layer rebuild and be ACTIVE (CR 113.6) on a
        // battlefield permanent. Without this the arm would pass VACUOUSLY — a dropped graft leaves
        // `board_has_functioning_etb_trigger` false, which is the `Absent` arm wearing a new name.
        let state = runner.state();
        let host_obj = &state.objects[&host];
        assert!(
            engine::game::functioning_abilities::active_trigger_definitions(state, host_obj).any(
                |active| active.definition.destination == Some(Zone::Battlefield)
                    && active.definition.mode == engine::types::triggers::TriggerMode::ChangesZone
            ),
            "reach-guard: the grafted battlefield-entry trigger must be ACTIVE on the host"
        );
    }

    // Derive the ability indices off the layer-built object (robust to parser ordering).
    let (mana_idx, life_idx, untap_idx) = {
        let abilities = &runner.state().objects[&engine_id].abilities;
        let mana_idx = abilities
            .iter()
            .position(is_mana_ability)
            .expect("the {T}: Add {C} mana ability");
        let life_idx = abilities
            .iter()
            .position(|d| matches!(&*d.effect, Effect::GainLife { .. }))
            .expect("the {0}: You gain 1 life ability");
        let untap_idx = abilities
            .iter()
            .position(|d| {
                matches!(
                    &*d.effect,
                    Effect::SetTapState {
                        state: TapStateChange::Untap,
                        ..
                    }
                )
            })
            .expect("the {0}: Untap ability");
        (mana_idx, life_idx, untap_idx)
    };

    // Drive one period [mana, gain-life, untap], settling each beat; the offer surfaces after the
    // untap beat closes the 3-step certified period.
    low3_activate_and_settle(&mut runner, engine_id, mana_idx);
    low3_activate_and_settle(&mut runner, engine_id, life_idx);
    low3_activate_and_settle(&mut runner, engine_id, untap_idx);

    // Reach-guard: the 3-step period recorded (non-vacuous — a shorter seq would be a different
    // loop / a drive artifact).
    assert_eq!(
        runner.state().last_loop_action_sequence.len(),
        3,
        "the certified period is the 3-step [mana, gain-life, untap] sequence, got {:?}",
        runner.state().last_loop_action_sequence
    );
    // Reach-guard: the CR 732.2a offer surfaced for P0.
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::LoopShortcut { proposer, .. } if proposer == P0),
        "the unobserved life engine must surface a LoopShortcut offer for P0, got {:?}",
        runner.state().waiting_for
    );

    // Accept through the REAL APNAP pipeline → materialize_object_growth_shortcut routing.
    runner
        .act(GameAction::DeclareShortcut {
            count: IterationCount::Fixed(1),
            template: None,
        })
        .expect("P0 declares the shortcut");
    runner
        .act(GameAction::RespondToShortcut {
            response: ShortcutResponse::Accept,
        })
        .expect("the single opponent accepts");
    runner
}

/// PINS the `token_profile.is_some()` conjunct of `life_etb_sourced` (engine.rs) — the conjunct the
/// sibling test above cannot reach, because its board has no functioning ETB trigger at all, so
/// `board_has_functioning_etb_trigger` short-circuits the predicate false before `token_profile` is
/// ever consulted.
///
/// Same mana-only Lifedynamo loop, but with a functioning battlefield-entry trigger grafted onto the
/// board. That makes TWO of the three conjuncts true (`!life.is_empty()` and
/// `board_has_functioning_etb_trigger`), so `token_profile.is_some()` — false here, because a
/// mana-only collapse mints no tokens and `current_period_fodder` returns `None` — is the ONLY thing
/// still holding this loop on the BATCHED route.
///
/// Why that matters: the collapse re-earns an ETB-sourced life axis only if it MINTS the entries
/// that re-fire the trigger. A mana engine mints nothing, so there is no double-pay and the O(N)
/// replay is pure cost. Deleting the conjunct silently sends every life-growing loop with any board
/// ETB trigger — mana engines included — down the concrete replay.
///
/// REVERT-PROBE (RUN): delete `&& token_profile.is_some()` from `life_etb_sourced` ⇒ this test's
/// batched-`Life` assertion FAILS (the stash becomes a lone `DriveSequence`). The sibling
/// `Absent`-arm test stays green under that same deletion, which is precisely why this arm exists.
#[test]
fn low3_mana_only_life_growth_stays_batched_despite_board_etb_trigger() {
    let runner = low3_life_engine_accepted(Low3BoardEtbTrigger::Present);

    let stash = runner
        .state()
        .pending_unbounded_materialization
        .get(&P0)
        .cloned()
        .unwrap_or_default();
    // Reach-guard: the accept produced a stash at all.
    assert!(
        !stash.is_empty(),
        "reach-guard: the accept must register a deferred materialization stash for P0"
    );
    // Reach-guard: this really is the mana-only shape — a `Tokens` item would mean the loop DID
    // mint fodder, `token_profile` would be `Some`, and the conjunct would no longer be the
    // load-bearing one.
    assert!(
        !stash
            .iter()
            .any(|m| matches!(m, PersistentAxisMaterialization::Tokens(_))),
        "reach-guard: the mana-only loop must stash no Tokens axis, got {stash:?}"
    );
    // DISCRIMINATOR: with the ETB trigger present and the loop token-less, the route stays BATCHED.
    // Drop `token_profile.is_some()` from `life_etb_sourced` and this flips to a DriveSequence.
    assert!(
        stash.iter().any(|m| matches!(
            m,
            PersistentAxisMaterialization::Life { player, per_cycle_delta }
                if *player == P0 && *per_cycle_delta >= 1
        )),
        "a token-less life loop must stay on the BATCHED Life route even with a board ETB trigger, \
         got {stash:?}"
    );
    assert!(
        !stash
            .iter()
            .any(|m| matches!(m, PersistentAxisMaterialization::DriveSequence { .. })),
        "a token-less life loop must not route to the concrete replay, got {stash:?}"
    );
}

// ───────── CR 732.2a + CR 603.6a: an ETB-SOURCED life axis routes to the concrete replay ─────────
//
// MEASURED DEFECT (real 4p Sprout Swarm dump `.fb-dumps/witherbloom-sprout-lumaret-works-slow`,
// engine UNMODIFIED, `combofb_probe e no_spear_accept`): P0's board carried Bogwater Lumaret
// ("Whenever ~ or another creature you control enters, you gain 1 life"). The accept registered
// the BATCHED pair `[Tokens(Saproling), Life { player: P0, per_cycle_delta: 1 }]`.
// `SubmitPayAmount(50)` took P0 from 546 to 596 (the batched +50) and left 50 real token-ETB
// triggers on the stack; draining them paid the SAME life a SECOND time, ending at 646. The
// batched arithmetic is not wrong — the ROUTE is: the collapse's own `Tokens` minting re-earns an
// ETB-sourced life axis, so the accept pays twice. On the concrete replay the real ETB triggers
// are the ONLY life source, which is what the board actually does.
//
// `combo_infinite_pile_4p_offer.json.gz` is a capture of the same game with NO life gainer on the
// battlefield (MEASURED: accept ⇒ `unbounded == {TokensCreated}`, route `["Tokens"]`), so these
// tests host the engine's own parse of a REAL card from this dump's pool — Prosperous Innkeeper's
// "Whenever another creature you control enters, you gain 1 life" — on a live permanent.

/// Prosperous Innkeeper's parsed "gain 1 life" battlefield-entry trigger, taken from the real
/// object in this dump. Selected by EFFECT, never by index: the Innkeeper also carries a
/// Treasure-token ETB trigger, and picking that one silently makes every life assertion vacuous.
fn innkeeper_etb_life_trigger(state: &GameState) -> TriggerDefinition {
    state
        .objects
        .values()
        .filter(|o| o.name == "Prosperous Innkeeper")
        .flat_map(|o| o.trigger_definitions.iter_unchecked())
        .map(|entry| &entry.definition)
        .find(|def| {
            def.execute
                .as_ref()
                .is_some_and(|a| matches!(*a.effect, Effect::GainLife { .. }))
        })
        .expect("Prosperous Innkeeper's ETB life trigger is in this dump's card pool")
        .clone()
}

/// MEASURED (revert-probe round 1): this dump carries Mortality Spear, whose life-conditional
/// cost static makes `fire_time_conditions_read_projected_resource` true — so
/// `life_growth_is_observed` already routes ANY life axis on this board to the replay, and a
/// route test built on it passes with the fix reverted. Strip those statics (the honest analogue
/// of a deck without that card, and exactly what the `combofb_probe e no_spear_accept` variant
/// does on the live dump) so the ONLY thing that can flip the route is the ETB source.
fn strip_life_conditional_cost_static(state: &mut GameState) {
    let ids: Vec<ObjectId> = state
        .objects
        .values()
        .filter(|o| o.name == "Mortality Spear")
        .map(|o| o.id)
        .collect();
    assert!(
        !ids.is_empty(),
        "fixture precondition: this dump must contain the Mortality Spear whose static is stripped"
    );
    for id in ids {
        let o = state.objects.get_mut(&id).unwrap();
        o.static_definitions.clear();
        o.base_static_definitions = std::sync::Arc::new(Vec::new());
    }
    mark_layers_full(state);
    flush_layers(state);
}

/// Install `def` on `host` and rebuild layers so it FUNCTIONS in its source's zone (CR 113.6).
fn graft_trigger(state: &mut GameState, host: ObjectId, def: TriggerDefinition) {
    state
        .objects
        .get_mut(&host)
        .expect("graft host")
        .trigger_definitions
        .push(def);
    mark_layers_full(state);
    flush_layers(state);
}

/// A plain 1/1 creature permanent for `owner` to host a grafted trigger. Deliberately NOT named
/// "Saproling" so it cannot join the loop's content-equal fodder class.
fn create_life_gainer(state: &mut GameState, owner: PlayerId, name: &str) -> ObjectId {
    let card_id = CardId(state.next_object_id);
    let id = create_object(state, card_id, owner, name.to_string(), Zone::Battlefield);
    let o = state.objects.get_mut(&id).unwrap();
    o.power = Some(1);
    o.toughness = Some(1);
    o.base_power = Some(1);
    o.base_toughness = Some(1);
    o.card_types.core_types = vec![CoreType::Creature];
    o.summoning_sick = false;
    id
}

/// A coarse label per registered materialization — the ROUTE oracle. `DriveSequence` is the
/// concrete replay; `Tokens`/`Counters`/`Life` are the batched N×δ collapse.
fn route_labels(state: &GameState, player: PlayerId) -> Vec<String> {
    state
        .pending_unbounded_materialization
        .get(&player)
        .map(|v| {
            v.iter()
                .map(|m| match m {
                    PersistentAxisMaterialization::DriveSequence { .. } => {
                        "DriveSequence".to_string()
                    }
                    PersistentAxisMaterialization::Tokens(_) => "Tokens".to_string(),
                    PersistentAxisMaterialization::Counters(_) => "Counters".to_string(),
                    PersistentAxisMaterialization::Life {
                        player,
                        per_cycle_delta,
                    } => format!("Life({player:?},{per_cycle_delta})"),
                })
                .collect()
        })
        .unwrap_or_default()
}

fn life_of(state: &GameState, player: PlayerId) -> i32 {
    state
        .players
        .iter()
        .find(|p| p.id == player)
        .expect("seat is in the dump")
        .life
}

/// Resolve everything the collapse left on the stack — the step that EXPOSED the double-count
/// (the batched route's 50 leftover token-ETB triggers paid the life a second time on the drain).
fn drain_stack(state: &mut GameState) {
    for _ in 0..8192 {
        if state.stack.is_empty() {
            return;
        }
        let WaitingFor::Priority { player } = state.waiting_for.clone() else {
            return;
        };
        apply(state, player, GameAction::PassPriority).expect("pass priority to drain the stack");
    }
    panic!("drain_stack: the stack did not empty within 8192 passes");
}

/// Drive the accepted loop to its boundary, collapse at `n`, drain the stack, and return the
/// number of Saprolings minted — the whole production tail in one place.
fn collapse_at(state: &mut GameState, n: u32) -> usize {
    let saps_before = p0_saproling_ids(state).len();
    drive_priority_to_next_boundary(state);
    assert!(
        matches!(
            state.waiting_for,
            WaitingFor::PayAmountChoice { player, resource: PayableResource::LoopCollapse { .. }, .. }
                if player == P0
        ),
        "the boundary must prompt P0 for the LoopCollapse count, got {:?}",
        state.waiting_for
    );
    apply(state, P0, GameAction::SubmitPayAmount { amount: n })
        .expect("P0 submits the finite loop-collapse count");
    drain_stack(state);
    p0_saproling_ids(state).len() - saps_before
}

/// R6b-converge (CR 732.2a + CR 603.6a): the two collapse ROUTES must land on the SAME life total.
///
/// ARM 1 (ETB-observer board): a real parsed "whenever another creature you control enters, you
/// gain 1 life" trigger on a P0 permanent. The accept must register `DriveSequence` — NOT the
/// batched pair — and collapsing at N=5 must pay the life exactly ONCE, measured ACROSS the
/// collapse *and* the stack drain (the drain is where the double-count surfaced: 596 → 646).
///
/// REVERT-PROBE (discriminating, RUN): drop the `life_etb_sourced` conjunct from the route
/// decision in `materialize_object_growth_shortcut` ⇒ the route flips to
/// `["Tokens", "Life(PlayerId(0),1)"]` and the life total over-counts by exactly N (the batched
/// +N at the collapse plus the N real token ETBs on the drain).
///
/// MUST-NOT-FLIP, ARM 2: the same dump with NO life gainer grows no life axis (MEASURED:
/// `unbounded == {TokensCreated}`) and must still register the batched `Tokens`.
#[test]
fn batched_and_replay_routes_converge_on_the_same_life_total() {
    const N: u32 = 5;

    // ── ARM 2 (must-NOT-flip): no life axis ⇒ the pure token loop still BATCHES. ──
    let mut plain: GameState = serde_json::from_str(&OFFER_STATE)
        .expect("the real 4p offer dump must deserialize into the current GameState");
    strip_life_conditional_cost_static(&mut plain);
    drive_all_accept_n(&mut plain, N);
    assert_eq!(
        route_labels(&plain, P0),
        vec!["Tokens".to_string()],
        "a pure token loop with NO life axis keeps the batched Tokens route"
    );

    // ── ARM 1 (primary): an ETB-sourced life axis ⇒ the concrete replay, paid exactly once. ──
    let mut state: GameState = serde_json::from_str(&OFFER_STATE).unwrap();
    strip_life_conditional_cost_static(&mut state);
    let etb_life = innkeeper_etb_life_trigger(&state);
    let host = create_life_gainer(&mut state, P0, "Grafted Innkeeper");
    graft_trigger(&mut state, host, etb_life);

    drive_all_accept_n(&mut state, N);
    assert_eq!(
        route_labels(&state, P0),
        vec!["DriveSequence".to_string()],
        "an ETB-sourced life axis routes to the concrete replay (revert 4a ⇒ [Tokens, Life(..,1)])"
    );

    let life_before = life_of(&state, P0);
    let minted = collapse_at(&mut state, N);

    // (1) POSITIVE reach-guard: the collapse actually ran and minted N real tokens.
    assert_eq!(
        minted, N as usize,
        "the collapse mints exactly N real Saprolings"
    );
    // (2) DISCRIMINATOR: one Saproling ETB per driven cycle × 1 life each = exactly N. The
    //     batched route pays N at the collapse AND N again when the real ETBs drain ⇒ 2N.
    let life_per_cycle = 1i32; // the single grafted ETB gainer on P0's battlefield
    assert_eq!(
        life_of(&state, P0) - life_before,
        N as i32 * life_per_cycle,
        "the ETB life is paid ONCE across collapse+drain (batched route ⇒ over-counts by N)"
    );
}

/// MIXED-CAUSE (CR 732.2a): TWO ETB life gainers on P0's board ⇒ a batched route would carry
/// `per_cycle_delta == 2`. The route must flip to the concrete replay for the WHOLE axis, and a
/// materialization MUST still be registered (M5 reach-guard: an empty registration would make
/// every downstream assertion vacuous).
///
/// This is why the fix is a ROUTE decision and not a registration-cancelling suppressor: a
/// suppressor keyed on "an ETB source exists" would drop the whole `Life` registration and
/// under-apply. Here it would pay N instead of 2N.
///
/// REVERT-PROBE (discriminating, RUN): drop the `life_etb_sourced` conjunct ⇒ the route flips to
/// `["Tokens", "Life(PlayerId(0),2)"]` and the total becomes 4N (2N batched + 2N on the drain).
#[test]
fn mixed_cause_life_axis_routes_to_replay() {
    const N: u32 = 5;
    let mut state: GameState = serde_json::from_str(&OFFER_STATE)
        .expect("the real 4p offer dump must deserialize into the current GameState");
    strip_life_conditional_cost_static(&mut state);
    let etb_life = innkeeper_etb_life_trigger(&state);
    let first = create_life_gainer(&mut state, P0, "Grafted Innkeeper A");
    graft_trigger(&mut state, first, etb_life.clone());
    let second = create_life_gainer(&mut state, P0, "Grafted Innkeeper B");
    graft_trigger(&mut state, second, etb_life);

    drive_all_accept_n(&mut state, N);
    let labels = route_labels(&state, P0);
    assert!(
        !labels.is_empty(),
        "M5 reach-guard: the route-flipped accept must register SOMETHING, got {labels:?}"
    );
    assert_eq!(
        labels,
        vec!["DriveSequence".to_string()],
        "a mixed-cause life axis routes wholesale to the concrete replay, got {labels:?}"
    );

    let life_before = life_of(&state, P0);
    let minted = collapse_at(&mut state, N);

    assert_eq!(
        minted, N as usize,
        "reach-guard: the collapse minted N tokens"
    );
    // DISCRIMINATOR: BOTH gainers pay, once each, per driven cycle ⇒ 2 per cycle.
    assert_eq!(
        life_of(&state, P0) - life_before,
        N as i32 * 2,
        "per_cycle_delta == 2 is paid in full ONCE (a suppressor ⇒ N, the batched route ⇒ 4N)"
    );
}

/// CR 732.2a + CR 109.5: an OPPONENT's controller-agnostic ETB life-gainer sits on the board
/// alongside P0's own. P0's accept must still materialize P0's FULL delta — a board-level
/// boolean suppressor ("some ETB life-gainer exists ⇒ drop the life axis") would name the wrong
/// beneficiary and silently zero P0's gain.
///
/// REVERT-PROBE (discriminating, RUN): replace the route flip with a board-level suppressor of
/// the `Life` registration ⇒ P0's Δlife collapses to 0 while P1's stays N.
#[test]
fn opponents_etb_life_gainer_does_not_suppress_your_axis() {
    const N: u32 = 5;
    let mut state: GameState = serde_json::from_str(&OFFER_STATE)
        .expect("the real 4p offer dump must deserialize into the current GameState");
    strip_life_conditional_cost_static(&mut state);
    let etb_life = innkeeper_etb_life_trigger(&state);

    // P0's own gainer ("another creature YOU control enters").
    let p0_host = create_life_gainer(&mut state, P0, "Grafted Innkeeper");
    graft_trigger(&mut state, p0_host, etb_life.clone());

    // P1 gets a Soul-Warden-shaped copy: the same parsed GainLife trigger with the `You`
    // controller narrowing dropped, so P0's tokens feed P1's life too.
    let mut soul_warden = etb_life;
    soul_warden.valid_card = None;
    let p1_host = create_life_gainer(&mut state, P1, "Grafted Soul Warden");
    graft_trigger(&mut state, p1_host, soul_warden);

    drive_all_accept_n(&mut state, N);
    let labels = route_labels(&state, P0);
    assert!(
        !labels.is_empty(),
        "reach-guard: the accept must register a materialization, got {labels:?}"
    );
    assert_eq!(
        labels,
        vec!["DriveSequence".to_string()],
        "the ETB-sourced axis routes to the replay even with a foreign gainer present"
    );

    let p0_before = life_of(&state, P0);
    let p1_before = life_of(&state, P1);
    let minted = collapse_at(&mut state, N);

    assert_eq!(
        minted, N as usize,
        "reach-guard: the collapse minted N tokens"
    );
    // DISCRIMINATOR: P0's OWN axis is materialized in full — a board-level suppressor zeroes it.
    assert_eq!(
        life_of(&state, P0) - p0_before,
        N as i32,
        "P0's own ETB life axis is paid in full despite the opponent's gainer"
    );
    // POSITIVE control: the opponent's gainer really did fire, so the assertion above is not
    // passing because nothing triggered at all.
    assert_eq!(
        life_of(&state, P1) - p1_before,
        N as i32,
        "the opponent's controller-agnostic gainer also pays once per driven cycle"
    );
}

/// R4-C1 COMBINED GATE (4a + 4b together; per-commit green is explicitly insufficient).
///
/// The same ETB life gainer as `batched_and_replay_routes_converge_on_the_same_life_total`, but
/// with `batched: true` — the "Whenever ONE OR MORE creatures you control enter, you gain 1 life"
/// shape (CR 603.2c). Collapsing at N now produces N SEPARATE same-turn token batches (one per
/// replayed cycle), so the total is right only if BOTH fixes hold:
///
/// * 4a (route): the ETB-sourced axis must take the concrete replay. Reverting it re-introduces
///   the batched `Life` on top of the real entries ⇒ amplified over-count.
/// * 4b (index): each replayed cycle's entry must carry its OWN zone-change index. Reverting it
///   leaves every entry on the `0` placeholder, so `batched_zone_change_already_collected` keys
///   all N batches to `(def, 0)` and only the FIRST fires ⇒ the trigger-count assertion fails.
///
/// Both revert-probes were RUN; observed values are in the assertion messages.
#[test]
fn combined_batched_etb_gainer_fires_once_per_replayed_cycle() {
    const N: u32 = 5;
    let mut state: GameState = serde_json::from_str(&OFFER_STATE)
        .expect("the real 4p offer dump must deserialize into the current GameState");
    strip_life_conditional_cost_static(&mut state);
    let mut batched_gainer = innkeeper_etb_life_trigger(&state);
    batched_gainer.batched = true;
    let host = create_life_gainer(&mut state, P0, "Grafted Batched Innkeeper");
    graft_trigger(&mut state, host, batched_gainer);

    drive_all_accept_n(&mut state, N);
    assert_eq!(
        route_labels(&state, P0),
        vec!["DriveSequence".to_string()],
        "4a: a batched ETB life gainer still routes the axis to the concrete replay"
    );

    let life_before = life_of(&state, P0);
    let minted = collapse_at(&mut state, N);

    // POSITIVE reach-guard: N cycles really replayed.
    assert_eq!(minted, N as usize, "the replay minted one token per cycle");
    // DISCRIMINATOR (needs BOTH fixes): N distinct same-turn batches ⇒ N fires ⇒ +N.
    // revert 4b ⇒ all N batches collide on `(def, 0)` ⇒ +1. revert 4a ⇒ batched Life on top ⇒ >N.
    assert_eq!(
        life_of(&state, P0) - life_before,
        N as i32,
        "each replayed cycle is its OWN batch and fires once (revert 4b ⇒ 1, revert 4a ⇒ more)"
    );
}

/// NON-`GainLife` LIFE SOURCE (CR 732.2a + CR 603.6a + CR 702.15b): the Terror-of-the-Peaks
/// shape — an ETB *damage* trigger on a permanent with LIFELINK. The life axis is just as
/// ETB-sourced as Soul Warden's, but it never passes through `Effect::GainLife`: it reaches
/// `apply_life_gain` from `effects/deal_damage.rs`'s CR 702.15b lifelink leg.
///
/// This is the fixture that forced the route predicate to be AXIS-shaped rather than
/// EFFECT-shaped. An earlier form asked whether the trigger's effect chain contained an
/// `Effect::GainLife`; life reaches `apply_life_gain` from FOUR resolvers (`life.rs`,
/// `double.rs`, `exchange_life.rs`, `deal_damage.rs`), so that test answers NO here, the loop
/// keeps the batched `[Tokens, Life]` pair, and the 596-vs-646 double-apply reproduces.
///
/// REVERT-PROBE (discriminating, RUN): re-add the `Effect::GainLife` chain test as a conjunct of
/// `board_has_functioning_etb_trigger` ⇒ this board's route flips back to
/// `["Tokens", "Life(PlayerId(0),3)"]` and the assertion below fails on the labels.
#[test]
fn lifelink_etb_damage_life_axis_routes_to_replay() {
    const N: u32 = 5;
    // 3 opponents in this 4p pod × 1 damage each, all dealt by one lifelink source per entry.
    const LIFELINK_PER_CYCLE: i32 = 3;

    let mut state: GameState = serde_json::from_str(&OFFER_STATE)
        .expect("the real 4p offer dump must deserialize into the current GameState");
    strip_life_conditional_cost_static(&mut state);

    // The real parsed battlefield-entry trigger CONDITION from this dump's pool, with only its
    // EFFECT swapped for the Impact-Tremors damage body. What is under test is the life SOURCE,
    // so the matcher half stays a real card's parse.
    let mut ping = innkeeper_etb_life_trigger(&state);
    ping.execute = Some(Box::new(engine::types::ability::AbilityDefinition::new(
        engine::types::ability::AbilityKind::Spell,
        Effect::DamageEachPlayer {
            amount: engine::types::ability::QuantityExpr::Fixed { value: 1 },
            player_filter: engine::types::ability::PlayerFilter::Opponent,
        },
    )));
    let host = create_life_gainer(&mut state, P0, "Grafted Terror of the Peaks");
    // CR 702.15b: damage dealt by a source with lifelink also causes its controller to gain that
    // much life. `DamageContext::from_source` reads the EFFECTIVE keyword set, so the printed +
    // base grant here is what makes the damage leg gain life.
    {
        let o = state.objects.get_mut(&host).expect("grafted host");
        o.keywords.push(engine::types::keywords::Keyword::Lifelink);
        o.base_keywords
            .push(engine::types::keywords::Keyword::Lifelink);
    }
    graft_trigger(&mut state, host, ping);

    drive_all_accept_n(&mut state, N);
    let labels = route_labels(&state, P0);
    assert!(
        !labels.is_empty(),
        "reach-guard: the accept must register a materialization, got {labels:?}"
    );
    assert_eq!(
        labels,
        vec!["DriveSequence".to_string()],
        "a LIFELINK-sourced ETB life axis routes to the concrete replay \
         (an Effect::GainLife shape test ⇒ [Tokens, Life(..,3)]), got {labels:?}"
    );

    let p0_before = life_of(&state, P0);
    let p1_before = life_of(&state, P1);
    let minted = collapse_at(&mut state, N);

    assert_eq!(
        minted, N as usize,
        "reach-guard: the collapse minted N tokens"
    );
    // POSITIVE control: the damage really was dealt, so the lifelink assertion below cannot pass
    // because nothing triggered at all.
    assert_eq!(
        life_of(&state, P1) - p1_before,
        -(N as i32),
        "each replayed cycle pings every opponent once"
    );
    // DISCRIMINATOR: the lifelink life is paid ONCE across collapse+drain. The batched route pays
    // the `Life { per_cycle_delta: 3 }` at the collapse AND the N real token ETBs pay it again on
    // the drain ⇒ 2 × 3N.
    assert_eq!(
        life_of(&state, P0) - p0_before,
        N as i32 * LIFELINK_PER_CYCLE,
        "the CR 702.15b lifelink gain is paid ONCE (batched route ⇒ double)"
    );
}
