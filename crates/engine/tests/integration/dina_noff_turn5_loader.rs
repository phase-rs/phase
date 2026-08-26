//! The Dina "offers, no fast-forward" turn-5 4p board, loaded through the production restore
//! chokepoint — and the row that makes every later acceptance row on this board possible.
//!
//! # Fixture provenance
//!
//! `../fixtures/dina_noff_turn5_4p.json.gz` is derived, not captured. Root of trust is the
//! read-only pristine dump root, NOT any working-directory copy:
//!
//! | artifact | bytes | sha256 |
//! |---|---|---|
//! | `/home/lgray/vibe-coding/combofb-dumps-pristine/dina-conqueror-offers-no-ff.zip` (canonical; the sole entry, line 1, of that directory's `MANIFEST.sha256`) | 4 334 390 | `4a285dbf5184545507c0d80183c4b831b3d21738f96728bb9e8eaa942a007d43` |
//! | member `game-state-turn-5-2026-08-05T21-53-17-125Z.json` | 21 442 451 | `14e2fe515310ea34f6c1f52087a0ab274842a3bf69f951b3ceacb93c9a0ca660` |
//! | derived `dina_noff_turn5_4p.json.gz` (this fixture) | 844 846 | `9843d5165cbbf7dd7bca4171c7888c190b7eba7e52a2ed095b44ff76fadd7886` |
//!
//! Regeneration re-gzips from the archive and must reproduce those bytes exactly — `-n` is
//! load-bearing, since without it gzip stamps an mtime and the digest never lands:
//!
//! ```text
//! unzip -p <canonical zip> game-state-turn-5-2026-08-05T21-53-17-125Z.json \
//!   | jq -c '{gameState}' | gzip -9 -n > crates/engine/tests/fixtures/dina_noff_turn5_4p.json.gz
//! ```
//!
//! The raw member is 21.4 MB and is deliberately NOT tracked; only the 845 KB `.json.gz` is.

use engine::types::game_state::{GameState, PersistedGameState};

fn gunzip(gz: &[u8]) -> String {
    use std::io::Read;
    let mut json = String::new();
    flate2::read::GzDecoder::new(gz)
        .read_to_string(&mut json)
        .expect("fixture .json.gz must inflate to UTF-8 JSON");
    json
}

/// Load the dump's `["gameState"]` through the REAL production restore chokepoint
/// `PersistedGameState::into_game_state` — never a bare `GameState` decode, which would skip
/// `reject_legacy_raw_prompt_authority` and `decode_persisted_resolution_state`.
fn load_dina_noff() -> GameState {
    let json = gunzip(include_bytes!("../fixtures/dina_noff_turn5_4p.json.gz"));
    let envelope: serde_json::Value =
        serde_json::from_str(&json).expect("dump envelope parses as JSON");
    serde_json::from_value::<PersistedGameState>(envelope["gameState"].clone())
        .expect("gameState deserializes through the production decoder")
        .into_game_state()
}

/// The saved ChaCha20 high-water this board carries (`gameState.rng_word_pos`). Also the
/// `current:` in the `HighWaterRegression` this board used to panic with on every load.
const DINA_NOFF_RNG_WORD_POS: u128 = 313;

/// **Row 9, positive arm.** The chokepoint rehydrates: a load that ENDS at
/// `PersistedGameState::into_game_state`, as `load_dina_noff` does, leaves the LIVE stream at the
/// saved high-water, so a later export-time capture is legal instead of a rewind. Scope: that is
/// the chokepoint's own postcondition, not a claim that every shipped ingress ends here —
/// `server-core`'s `GameSession::from_persisted` re-seeds afterwards and zeroes `rng_word_pos`
/// with it, so the server ends at an agreed live-0 / high-water-0 pair rather than at this
/// resumed position.
///
/// Non-vacuity: the reach-guard below pins the real board (4 seats, turn 5, the captured life
/// vector, a NON-ZERO saved high-water), so "no panic" cannot be satisfied by a degenerate or
/// empty state. Discrimination: deleting `state.rehydrate_rng()` from
/// `PersistedGameState::into_game_state` leaves the live stream at word 0 while `rng_word_pos`
/// stays 313, and `assert_eq!(live, state.rng_word_pos)` reds with `0 != 313` — measured, not
/// asserted. `capture_rng_word_pos` then `.expect`-panics on the same revert.
#[test]
fn c0_the_native_loader_rehydrates_the_persisted_rng_stream() {
    let mut state = load_dina_noff();

    // Reach-guard: this is the captured 4p board, not a default or empty state.
    assert_eq!(state.players.len(), 4, "the real 4p board must have loaded");
    assert_eq!(state.turn_number, 5, "captured on turn 5");
    assert_eq!(
        state.players.iter().map(|p| p.life).collect::<Vec<_>>(),
        vec![51, 29, 34, 34],
        "the captured life vector identifies this exact board",
    );
    assert_eq!(
        state.rng_word_pos, DINA_NOFF_RNG_WORD_POS,
        "the board must carry a NON-ZERO saved high-water, or the row measures nothing",
    );

    // The invariant the panic was only a signature of: live stream position == persisted
    // high-water. Measuring it directly is deliberate rather than driving the board to a shuffle:
    // every shuffle source it holds (Terramorphic Expanse, Evolving Wilds, Fabled Passage) is an
    // ACTIVATED ability, so a pass-only driver reaches none of them, and a drive-based row here
    // was measured VACUOUS twice before this one replaced it.
    assert_eq!(
        state.rng.get_word_pos(),
        state.rng_word_pos,
        "into_game_state must fast-forward the live ChaCha20 stream to the saved high-water",
    );

    // The production consequence, through the `pub` engine seam that used to blow up: the
    // export-time capture every subsequent save performs.
    state.capture_rng_word_pos();
    assert_eq!(
        state.rng_word_pos, DINA_NOFF_RNG_WORD_POS,
        "a capture at the restored position must not move the high-water",
    );
}

/// **Row 9, WASM arm.** The WASM restore keeps its own `state.rehydrate_rng()` after the
/// chokepoint (in `engine-wasm`'s `restore_game_state`), so the load path now runs it twice. Safe
/// only because `rehydrate_rng` is idempotent — both of its statements are absolute assignments
/// from persisted fields (`rng = seed_from_u64(rng_seed)`, then `set_word_pos(rng_word_pos)`), so
/// it neither accumulates nor advances. This row measures that instead of trusting it, which is
/// why no `engine-wasm` edit is owed.
///
/// Discrimination (RUN, both mutations): drop the reseed and make the fast-forward RELATIVE —
/// `self.rng.set_word_pos(self.rng.get_word_pos() + self.rng_word_pos)` — and the position
/// assertion reds `626 != 313` with the draw comparison behind it, because the second run
/// accumulates. Note that an absolute-but-wrong fast-forward (`set_word_pos(rng_word_pos + 1)`)
/// does NOT red here and is not what this row claims to catch: idempotence is a property of
/// ASSIGNMENT, so only an accumulating form can break it.
#[test]
fn c0_a_second_rehydrate_is_a_no_op_so_the_wasm_restore_may_repeat_it() {
    use rand::RngCore;

    let mut once = load_dina_noff();
    let mut twice = load_dina_noff();
    twice.rehydrate_rng(); // the WASM restore's own repeat, on top of the chokepoint's

    assert_eq!(
        twice.rng.get_word_pos(),
        once.rng.get_word_pos(),
        "a repeated rehydrate must not move the stream",
    );
    assert_eq!(
        twice.rng_word_pos, once.rng_word_pos,
        "a repeated rehydrate must not move the persisted high-water",
    );

    // Position equality alone would survive a same-position-different-keystream bug, so compare
    // the values the two streams actually produce.
    for draw in 0..5 {
        assert_eq!(
            twice.rng.next_u32(),
            once.rng.next_u32(),
            "double-rehydrated stream diverged at draw {draw}",
        );
    }
}

/// **Row 9, negative control.** The same board, one axis changed: the live stream rewound to word
/// 0, which is exactly the state a decode produced before the chokepoint rehydrated. It panics.
///
/// Without this arm the positive arm above is a claim that a call does not panic, with no evidence
/// that it ever could on this board.
#[test]
#[should_panic(expected = "HighWaterRegression")]
fn c0_an_unrehydrated_stream_on_the_same_board_still_panics() {
    let mut state = load_dina_noff();
    // Re-create the pre-rehydrate live position. `advance_rng_high_water` rejects on
    // `requested < rng_word_pos`, and `requested` is read from the live stream — so the position
    // is the whole failing condition.
    state.rng.set_word_pos(0);
    state.capture_rng_word_pos();
}
