//! Token-storm scaling CI gate (engine tier).
//!
//! Permanent regression guard proving the `StaticModePresence` O(1) index keeps
//! whole-battlefield static scans out of the two highest-fanout production
//! enumeration paths — target legality (`find_legal_targets`) and combat
//! declaration (`get_valid_attacker_ids` / `legal_actions_full`) — at 1000-object
//! scale, INCLUDING the production serde-restore → `flush_layers` seam that the AI
//! search and saved-state loader hit. Locks in the Unit 1/Unit 2 perf work
//! (commits a7ea537, dd8fe5d, 48861f8) against silent regression.
//!
//! DB-free by construction: `GameState::new_two_player` + `create_object` only,
//! never loading `client/public/card-data.json` (mirrors the
//! `targeting.rs:4683` unit test; `scripts/check-test-card-data-load.sh` guards
//! this). Under `cargo nextest` each test runs in its own process, so the
//! `thread_local!` perf counters cannot bleed across tests and the exact `== 0`
//! assertions are sound. When run locally with plain `cargo test` the counters
//! are still per-thread; nextest (Tilt `test-engine`) is the authoritative
//! process-per-test harness.

use engine::ai_support::legal_actions_full;
use engine::game::combat::{get_valid_attack_targets, get_valid_attacker_ids};
use engine::game::layers;
use engine::game::perf_counters;
use engine::game::targeting::find_legal_targets;
use engine::game::zones::create_object;
use engine::types::ability::{StaticDefinition, TargetFilter, TargetRef, TypedFilter};
use engine::types::card_type::CoreType;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::player::PlayerId;
use engine::types::statics::{StaticMode, StaticModeKind};
use engine::types::zones::Zone;

const TOKENS: usize = 1000;

/// Combat-declaration board size. `token_storm_declare_attackers_gate` drives
/// `legal_actions_full`, which enumerates the declare-attackers action
/// cross-product (O(attackers)) — at 1000 tokens that runs ~13s in a debug
/// build, too slow for an always-on gate. 300 keeps the discrimination margin
/// intact while staying fast: legitimate per-attacker work is O(N) (a few
/// hundred to low-thousands of scan increments), whereas a reverted presence
/// gate makes each of the N attackers force a whole-battlefield scan → O(N²) =
/// 90_000 at N=300. The `< 20_000` bound sits orders of magnitude clear of both
/// sides (≈10²–10³ legit ≪ 20_000 ≪ 90_000 pathological), so it still flips red
/// on a gate revert. 1a/1c/2a stay at `TOKENS` (1000) because `find_legal_targets`
/// and the direct combat enumerator are fast at that scale.
const DECLARE_TOKENS: usize = 300;

/// `TargetFilter` matching every creature. Round-2 amendment: the private
/// `#[cfg(test)]` helper at `targeting.rs:2200` is not reachable from an
/// integration-test crate, so define the equivalent locally from public API
/// (`TypedFilter::creature()` is pub at `types/ability.rs:3410`; idiom mirrors
/// `aurora_awakener_reveal_until_n_permanents.rs:318`).
fn creature_filter() -> TargetFilter {
    TargetFilter::Typed(TypedFilter::creature())
}

/// `count` vanilla creature tokens controlled by `owner`, plus one bare global
/// Vigilance static so the presence index is provably non-empty yet does NOT
/// touch the `IgnoreHexproof` bit the target-enumeration gate consults.
///
/// The Vigilance seed uses the proven bare-global idiom (no `.affected()`):
/// `obj.static_definitions = vec![StaticDefinition::new(StaticMode::Vigilance)].into();`
/// — identical to `targeting.rs:4748` / `game_state.rs:9136`. After a flush this
/// leaves `StaticModeKind::Vigilance` PRESENT and `IgnoreHexproof` precisely
/// ABSENT, which is what makes the index discriminate kinds (not "all-empty").
fn token_storm_board(owner: PlayerId, count: usize) -> (GameState, Vec<ObjectId>) {
    let mut state = GameState::new_two_player(42);
    let mut ids = Vec::with_capacity(count);
    for i in 0..count {
        let id = create_object(
            &mut state,
            CardId(1000 + i as u64),
            owner,
            format!("Token{i}"),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&id)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);
        ids.push(id);
    }
    let src = create_object(
        &mut state,
        CardId(9999),
        owner,
        "Vigilance Anthem".to_string(),
        Zone::Battlefield,
    );
    state.objects.get_mut(&src).unwrap().static_definitions =
        vec![StaticDefinition::new(StaticMode::Vigilance)].into();
    (state, ids)
}

/// Serde round-trip: drops every `#[serde(skip)]` field, so the restored state
/// has `static_mode_presence = all_present` and `layers_dirty = LayersDirty::full()`
/// — exactly the production shape an AI-search / saved-game restore produces.
fn restore(state: &GameState) -> GameState {
    serde_json::from_str(&serde_json::to_string(state).unwrap()).unwrap()
}

/// Test 1a — target enumeration does no full scan at 1000-object scale after a
/// FULL-FLUSH serde restore (class a AND class b).
///
/// Overlap note (A4): the class-a signal on a hand-built board is already covered
/// by `token_storm_target_enumeration_does_no_static_full_scans` (`targeting.rs:4683`),
/// which refreshes via `evaluate_layers`. This test's UNIQUE contribution is the
/// production-shaped serde-restore + `flush_layers` seam: `flush_layers` dispatches
/// `LayersDirty::Full => evaluate_layers` (`layers.rs:1968`) → `refresh_static_mode_presence`
/// (`layers.rs:1920`), additionally exercising the `flush_layers` dispatch +
/// `mark_public_state_all_dirty` seam that production (AI restore, saved-game load) hits.
#[test]
fn token_storm_scaling_gate_absent_modes() {
    let (state, ids) = token_storm_board(PlayerId(1), TOKENS);
    let mut restored = restore(&state);
    // Pre-flush the restored state carries the all-present serde default.
    assert!(
        restored
            .static_mode_presence
            .contains(StaticModeKind::IgnoreHexproof),
        "restored state must start with the conservative all-present index"
    );

    // Full arm → evaluate_layers → refresh_static_mode_presence → PRECISE index.
    layers::flush_layers(&mut restored);

    // The index discriminates kinds: Vigilance present, IgnoreHexproof absent.
    assert!(
        restored
            .static_mode_presence
            .contains(StaticModeKind::Vigilance),
        "the seeded Vigilance static must be indexed present after flush"
    );
    assert!(
        !restored
            .static_mode_presence
            .contains(StaticModeKind::IgnoreHexproof),
        "no IgnoreHexproof static exists — the index must report it absent after flush"
    );

    perf_counters::reset();
    let targets = find_legal_targets(&restored, &creature_filter(), PlayerId(0), ObjectId(99));
    let counters = perf_counters::snapshot();

    // Revert guard (class a): removing the `can_target` presence gate makes this
    // non-zero. Revert guard (class b): removing the `flush_layers` call above
    // leaves the all-present index, and every token forces a scan → non-zero.
    assert_eq!(
        counters.static_full_scans, 0,
        "token-storm target enumeration must not run any whole-battlefield static scan \
         after a full-flush restore"
    );
    // Reach-guard: the 0-scan is not from an empty result — every token is legal.
    assert_eq!(targets.len(), TOKENS, "every token must be a legal target");
    assert!(targets.contains(&TargetRef::Object(ids[0])));
    assert!(targets.contains(&TargetRef::Object(ids[TOKENS - 1])));
}

/// Test 1b — combat declaration is bounded after a full-flush restore (class-a
/// composite only).
///
/// `legal_actions_full` self-heals (`ai_support/mod.rs:948-951`, `:960-966`): it
/// re-flushes a dirty state before enumerating. This test therefore asserts the
/// class-a index-precision composite only — the combat enumerators used to BUILD
/// the state carry no class-b guard here (that guard is Test 1c's direct-drive).
///
/// Non-vacuousness: `get_valid_attacker_ids` scans only creatures controlled by
/// `state.active_player` (`combat.rs:2929`), so the board is built UNDER the active
/// player. With `new_two_player`, `active_player == PlayerId(0)`; building
/// `DECLARE_TOKENS` tokens under it yields that many valid attackers, so the
/// per-attacker O(N) work is real and reverting a combat presence gate blows the
/// `< 20_000` bound (see the `DECLARE_TOKENS` margin justification).
#[test]
fn token_storm_declare_attackers_gate() {
    let probe = GameState::new_two_player(42);
    let active = probe.active_player;
    let (state, _ids) = token_storm_board(active, DECLARE_TOKENS);
    let mut restored = restore(&state);
    layers::flush_layers(&mut restored);

    // Mirror combat.rs:2996-3006 — build the declare-attackers payload from the
    // real enumerators.
    let valid_attacker_ids = get_valid_attacker_ids(&restored);
    let valid_attack_targets = get_valid_attack_targets(&restored);
    assert_eq!(
        valid_attacker_ids.len(),
        DECLARE_TOKENS,
        "all active-player tokens must be valid attackers (non-vacuous fixture)"
    );
    restored.waiting_for = WaitingFor::DeclareAttackers {
        player: active,
        valid_attacker_ids,
        valid_attack_targets,
        valid_attack_targets_by_attacker: None,
        attacker_constraints: Default::default(),
    };

    perf_counters::reset();
    let (actions, _costs, _grouped) = legal_actions_full(&restored);
    let counters = perf_counters::snapshot();

    assert_eq!(
        counters.combat_shadow_block_scans, 0,
        "combat declaration must not run a shadow-block full scan after a full-flush restore"
    );
    assert!(
        counters.static_full_scans < 20_000,
        "combat declaration static scans must stay bounded (legitimate per-attacker O(N) \
         work only); got {}",
        counters.static_full_scans
    );
    assert!(
        !actions.is_empty(),
        "declare-attackers enumeration must offer at least one legal action"
    );
}

/// Test 1c — missing flush ⇒ explosion (class-b positive control), for BOTH the
/// target path and the combat enumerator path.
///
/// This is the paired positive control that proves 1a/1b's `== 0` / bounded
/// assertions are reachable (not short-circuited on an empty result) and guards
/// them against a future self-heal of the read-only enumerators making the 0
/// vacuous. On an UNFLUSHED restored state the index is all-present, so every
/// token forces a whole-battlefield scan.
#[test]
fn token_storm_missing_flush_explodes() {
    // Target path: unflushed all-present index → per-token scan.
    let (state, _) = token_storm_board(PlayerId(1), TOKENS);
    let unflushed = restore(&state); // SKIP flush.
    perf_counters::reset();
    let _ = find_legal_targets(&unflushed, &creature_filter(), PlayerId(0), ObjectId(99));
    assert!(
        perf_counters::snapshot().static_full_scans >= TOKENS as u64,
        "unflushed all-present index must force a whole-battlefield scan per token \
         (target path)"
    );

    // Combat direct-drive: `get_valid_attacker_ids` takes `&GameState` and CANNOT
    // self-heal (combat.rs:2918). Build tokens under the active player so the
    // per-creature scan branch (combat.rs:2929, controller == active) is reached.
    let probe = GameState::new_two_player(42);
    let active = probe.active_player;
    let (state, _) = token_storm_board(active, TOKENS);
    let unflushed = restore(&state); // SKIP flush.
    perf_counters::reset();
    let _ = get_valid_attacker_ids(&unflushed);
    assert!(
        perf_counters::snapshot().static_full_scans >= TOKENS as u64,
        "unflushed all-present index forces a per-creature combat static scan \
         (combat direct-drive; non-self-healing &GameState)"
    );
}
