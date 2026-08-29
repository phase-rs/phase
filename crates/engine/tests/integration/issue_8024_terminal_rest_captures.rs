//! Issue #8024 reporter captures: two Commander boards that reached a priority
//! window still carrying a terminal resolution carrier, so the next turn advance
//! tripped the `start_next_turn` precondition at `game/turns.rs`.
//!
//! Both captures are the *exact engine-owned residue* shapes the restore
//! boundary now retires before it publishes a priority window, and they are the
//! two shapes the already-tracked `mycoloth_devour_drain_strand.rs` captures do
//! NOT cover: those two carry an ownerless `DrainStatus::Dispatching` resident,
//! while these two carry no live drain at all.
//!
//! | capture | residue |
//! |---|---|
//! | turn 26 | one `SpellResolution` frame over a completed `Spell` carrier — the bare permanent-spell epilogue |
//! | turn 10 | an EMPTY `PostReplacement` frame under a `ChangeZone` frame holding only a `devour_eligible_snapshot` |
//!
//! # Why the reporters' builds panicked and origin/main alone does not
//!
//! The live action boundary gained the same two repairs first — `#8019`
//! (`d070dca1f0`, Devour, 2026-08-28T18:31Z) and `#8031` (`ff4e09df6f`, bare
//! spell resolution, 2026-08-29T00:20Z). Both captures predate the build that
//! carried them (`v0.66.0` / `4b85b52`, 2026-08-28T20:53Z, which contains the
//! first and not the second), so on the reporters' builds nothing retired the
//! carrier and `start_next_turn` asserted.
//!
//! What those two PRs did NOT do is settle the residue at the *persistence*
//! boundary: a decode still PUBLISHED the unsettled window and relied on some
//! later `apply` to repair it. These rows pin the restore-boundary contract
//! instead, which is the one `start_next_turn` actually depends on.
//!
//! # Fixture provenance
//!
//! Reporter attachments from the issue-#8024 Discord threads. The ~21 MB raw
//! members are deliberately NOT tracked; only the derived `.json.gz` are.
//!
//! | artifact | bytes | sha256 |
//! |---|---|---|
//! | `1542918445870489660__game-state-turn-26-2026-08-28T16-37-37-913Z.zip` (JPUTK) | 4 284 800 | `a7196a2711e4554ddab89a40905f18c5de60dc195582c2e7b20078538d83ccb2` |
//! | member `game-state-turn-26-2026-08-28T16-37-37-913Z.json` | 21 647 575 | `d1466fd9d287a2aa5789c6bdb29d6767d1235629b9babeae93aa013461bd7c7c` |
//! | derived `issue_8024_spell_rest_turn26.json.gz` | 693 325 | `569b921447b0158c1e6db3fa423423e3e8da7f841edfec0782746dec9f029be1` |
//! | `1542937675252764732__game-state-turn-10-2026-08-28T16-41-59-062Z.zip` (Prentiss) | 4 530 428 | `10f762d72ac1a6e554db46ef0f0c89ced6999156af769e927501276e83fc2f78` |
//! | member `game-state-turn-10-2026-08-28T16-41-59-062Z.json` | 22 582 452 | `ee22f83dd46916e52141ed380a511487db44350a232e6511c33c736550692156` |
//! | derived `issue_8024_devour_rest_turn10.json.gz` | 732 847 | `966a5762f19d76218a50cd3ab82202993add37f6770aea89781b42fbb378d957` |
//!
//! Byte-reproducible regeneration — `-n` is load-bearing, since without it gzip
//! stamps an mtime and the digest never lands:
//!
//! ```text
//! unzip -p <zip> <member>.json | jq -c '{gameState}' | gzip -9 -n \
//!   > crates/engine/tests/integration/fixtures/issue_8024_spell_rest_turn26.json.gz
//! ```
//!
//! Unlike the pre-U5 captures elsewhere in this suite, no `deck_size` migration
//! is owed: both dumps already carry the adjacently-tagged
//! `{"type":"Exactly","data":100}` form.

use engine::types::game_state::{GameState, PersistedGameState, WaitingFor};

const SPELL_REST_TURN26: &[u8] = include_bytes!("fixtures/issue_8024_spell_rest_turn26.json.gz");
const DEVOUR_REST_TURN10: &[u8] = include_bytes!("fixtures/issue_8024_devour_rest_turn10.json.gz");

fn gunzip(gz: &[u8]) -> String {
    use std::io::Read;
    let mut json = String::new();
    flate2::read::GzDecoder::new(gz)
        .read_to_string(&mut json)
        .expect("fixture .json.gz must inflate to UTF-8 JSON");
    json
}

/// `client/src/services/gameStateExport.ts` writes a debug snapshot of the
/// runtime `GameState`, not a persistence-wire save: it carries the raw
/// `resolution_stack` field and no `resolution_state_version`.
/// `PersistedGameState`'s decoder stamps an absent version as v1, and the v1
/// reader rejects any payload carrying `resolution_stack` outright.
///
/// So the snapshot is first projected onto the v2 wire — exactly the
/// transformation `ResolutionStateWire::to_value` performs when persisting a
/// live state. Nothing else is touched; in particular the terminal carrier and
/// its frames cross verbatim. This mirrors
/// `mycoloth_devour_drain_strand::projected_capture_snapshot`.
fn projected_capture_snapshot(gz: &[u8]) -> serde_json::Value {
    let json = gunzip(gz);
    let envelope: serde_json::Value =
        serde_json::from_str(&json).expect("dump envelope parses as JSON");
    let mut snapshot = envelope["gameState"].clone();
    {
        let object = snapshot
            .as_object_mut()
            .expect("a captured gameState is a JSON object");
        assert!(
            !object.contains_key("resolution_state_version"),
            "the reporter's capture is an unversioned runtime debug snapshot"
        );
        let stack = object
            .remove("resolution_stack")
            .expect("the captured board carries a runtime resolution_stack");
        object.insert("resolution_frames".to_string(), stack);
        object.insert(
            "resolution_state_version".to_string(),
            serde_json::Value::from(2),
        );
    }
    snapshot
}

/// The production restore chokepoint, end to end. Decoding AS
/// `PersistedGameState` (rather than decoding a bare `GameState`) is what routes
/// the dump through `reject_legacy_raw_prompt_authority` +
/// `decode_persisted_resolution_state`, and `into_game_state` is what the
/// server's `from_persisted` and WASM's restore both funnel through.
fn restore_capture(gz: &[u8]) -> GameState {
    serde_json::from_value::<PersistedGameState>(projected_capture_snapshot(gz))
        .expect("the projected snapshot deserializes through the production decoder")
        .into_game_state()
        .expect("the persisted capture satisfies the checked restore contract")
}

/// The residue each capture carries BEFORE restore, read out of the fixture's
/// own bytes. Without this arm the rows below would be "a restore produced a
/// settled state" with no evidence that this state was ever unsettled — the
/// exact vacuity that makes a no-panic assertion worthless.
fn captured_residue(gz: &[u8]) -> (usize, bool) {
    let snapshot = projected_capture_snapshot(gz);
    let frames = snapshot["resolution_frames"]["frames"]
        .as_array()
        .expect("the projected wire carries a frames array")
        .len();
    let carrier = !snapshot["resolving_stack_entry"].is_null();
    (frames, carrier)
}

/// CR 608.2n / CR 608.3: resolution genuinely completes — an instant, sorcery,
/// or ability leaves the stack as the final part of its own resolution
/// (turn-10), and a permanent spell finishes through the CR 608.3 steps
/// (turn-26). CR 117.3b: the active player then receives priority. CR 500.2: a
/// phase or step ends only once the stack is empty and all players pass in
/// succession — so no resolution carrier may still be owned. That conjunction
/// is precisely what `game::turns::start_next_turn` asserts.
///
/// Non-vacuity: `captured_residue` pins that each fixture really does carry the
/// wedged shape before restore (turn-26: one frame + a carrier; turn-10: two
/// frames + a carrier), and the board identity guards below pin that the real
/// 4-player Commander capture loaded rather than a default state.
///
/// Discrimination (measured, not asserted): deleting the
/// `recover_terminal_resolution_rest_on_restore` call from
/// `PersistedGameState::prepare_for_restore` makes both boards publish the
/// captured residue, and `restore_capture` then fails at
/// `PersistedRestoreError::UnsettledPriorityResolution` for turn-26 and leaves
/// `resolution_stack` non-empty for turn-10.
#[test]
fn issue_8024_captures_settle_their_terminal_carrier_before_publication() {
    for (gz, turn, expected_frames) in [
        (SPELL_REST_TURN26, 26u32, 1usize),
        (DEVOUR_REST_TURN10, 10u32, 2usize),
    ] {
        let (frames, carrier) = captured_residue(gz);
        assert_eq!(
            (frames, carrier),
            (expected_frames, true),
            "turn-{turn} fixture must carry the captured terminal residue, or this row measures \
             nothing",
        );

        let state = restore_capture(gz);

        // Board identity — this is the reporter's real 4-seat Commander board.
        assert_eq!(state.players.len(), 4, "the captured 4p board must load");
        assert_eq!(state.turn_number, turn);

        // The four conjuncts of the `start_next_turn` precondition, in order.
        assert!(
            state.stack.is_empty(),
            "turn-{turn}: the captured stack was already empty"
        );
        assert!(
            state.resolution_stack.is_empty(),
            "turn-{turn}: restore must not publish the terminal resolution frame"
        );
        assert!(
            state.resolving_stack_entry.is_none(),
            "turn-{turn}: restore must settle the completed stack carrier (CR 608.2c)"
        );
        assert!(
            matches!(state.waiting_for, WaitingFor::Priority { .. }),
            "turn-{turn}: restore must publish a settled Priority window (CR 117.3b)"
        );
        assert!(
            state.pending_resolution_completion.is_none(),
            "turn-{turn}: no completion hold may survive publication"
        );
    }
}

/// CR 117.3b: the recovered window belongs to the active player, not to
/// whichever seat the capture happened to freeze priority on. Both captures
/// froze on a non-active seat, so this is a live difference rather than a
/// restatement of the row above.
#[test]
fn issue_8024_recovered_priority_returns_to_the_active_player() {
    for (gz, turn) in [(SPELL_REST_TURN26, 26u32), (DEVOUR_REST_TURN10, 10u32)] {
        let snapshot = projected_capture_snapshot(gz);
        let captured_priority = snapshot["priority_player"]
            .as_u64()
            .expect("the capture records a priority player");
        let captured_active = snapshot["active_player"]
            .as_u64()
            .expect("the capture records an active player");
        assert_ne!(
            captured_priority, captured_active,
            "turn-{turn}: the capture must freeze priority off the active seat, or this row is a \
             restatement",
        );

        let state = restore_capture(gz);
        assert_eq!(
            state.waiting_for,
            WaitingFor::Priority {
                player: state.active_player
            },
            "turn-{turn}: CR 117.3b hands the recovered window to the active player",
        );
        assert_eq!(state.priority_player, state.active_player);
    }
}
