//! CR 115.1 + Whitemane Lion ruling (issue #563): regression guard for the
//! non-targeted self-bounce ETB infinite-loop pathology.
//!
//! Pre-fix the AI would cast Whitemane Lion ({1}{W}, 2/2, ETB "return a
//! creature you control to its owner's hand") and the ETB resolution let it
//! pick itself as the bounce target. With the source returning to hand each
//! cast, the AI re-cast it on the next priority window for an unbounded
//! number of iterations per turn — the canonical symptom was the duel-suite
//! safety cap of 10,000 actions being hit before any turn could end.
//!
//! The PR addresses this in three layered ways:
//!
//! 1. Parser routes the non-targeted "return a creature you control" Oracle
//!    text to `Effect::Bounce { selection: BounceSelection::AtResolution }`
//!    rather than emitting a target slot the AI fills with the source itself.
//! 2. The resolver branch in `effects/bounce.rs` enumerates eligible permanents
//!    at resolution and (when only the source itself is eligible) auto-moves
//!    without ever asking the AI to "target".
//! 3. `MAX_CASTS_OF_SAME_CARD_PER_TURN` in `phase-ai/src/search.rs` caps the
//!    AI at 3 casts of any one card name per turn as a defence-in-depth net
//!    against pathological positive-EV recasts.
//!
//! Together these bound the pathology; this test locks the bound in.
//!
//! `#[ignore]` because it loads card-data.json (requires `cargo run --bin
//! card-data-export` or the setup.sh script), which isn't available in
//! unit-test CI. Opt in via
//! `cargo test -p phase-ai --test whitemane_lion_bounded -- --ignored`.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use engine::database::CardDatabase;
use engine::game::deck_loading::{
    load_deck_into_state, resolve_deck_list, DeckList, PlayerDeckList,
};
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::player::PlayerId;
use phase_ai::auto_play::run_ai_actions;
use phase_ai::config::{create_config_for_players, AiDifficulty, Platform};

/// Comfortable headroom for natural-game-completion variance. Pre-fix runs hit
/// 10,000 every game; post-fix this deck completes naturally well under 4,000.
const BOUND_ACTIONS: usize = 4000;

fn load_db() -> CardDatabase {
    let cards_dir = std::env::var("PHASE_CARDS_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("..")
                .join("..")
                .join("client")
                .join("public")
        });
    let export_path = cards_dir.join("card-data.json");
    CardDatabase::from_export(&export_path)
        .unwrap_or_else(|e| panic!("load card-data.json from {}: {e}", export_path.display()))
}

fn repeat(name: &str, n: usize) -> Vec<String> {
    std::iter::repeat_n(name.to_string(), n).collect()
}

/// 60-card deck packed with Whitemane Lion. Other creatures give the AI legal
/// non-source bounce targets so it cannot trivially "pass" because no other
/// option exists; the test thus discriminates against the loop being broken
/// for the right reason (cap + resolver fix), not for an accidental absence
/// of any Whitemane Lion-castable position.
fn deck_whitemane_lion_stack() -> Vec<String> {
    let mut d = Vec::with_capacity(60);
    d.extend(repeat("Plains", 28));
    d.extend(repeat("Whitemane Lion", 16));
    d.extend(repeat("Savannah Lions", 8));
    d.extend(repeat("Elite Vanguard", 8));
    d
}

#[test]
#[ignore = "loads card-data.json + runs a full game; opt in via --ignored"]
fn whitemane_lion_self_bounce_does_not_infinite_loop() {
    let db = load_db();

    let list = DeckList {
        player: PlayerDeckList {
            main_deck: deck_whitemane_lion_stack(),
            sideboard: Vec::new(),
            commander: Vec::new(),
            ..Default::default()
        },
        opponent: PlayerDeckList {
            main_deck: deck_whitemane_lion_stack(),
            sideboard: Vec::new(),
            commander: Vec::new(),
            ..Default::default()
        },
        ai_decks: Vec::new(),
        ai_difficulties: Vec::new(),
    };
    let payload = resolve_deck_list(&db, &list);

    let mut state = GameState::new_two_player(1);
    load_deck_into_state(&mut state, &payload);
    engine::game::engine::start_game(&mut state);

    let ai_players: HashSet<PlayerId> = [PlayerId(0), PlayerId(1)].into_iter().collect();
    let config = create_config_for_players(AiDifficulty::Easy, Platform::Native, 2);
    let ai_configs: HashMap<PlayerId, _> =
        [(PlayerId(0), config.clone()), (PlayerId(1), config.clone())]
            .into_iter()
            .collect();

    let mut total_actions: usize = 0;
    loop {
        let results = run_ai_actions(&mut state, &ai_players, &ai_configs);
        if results.is_empty() {
            break;
        }
        total_actions += results.len();
        if total_actions >= BOUND_ACTIONS {
            panic!(
                "whitemane-lion-stack mirror exceeded action bound: {total_actions} >= \
                 {BOUND_ACTIONS} at turn {}. Likely a regression in non-targeted bounce \
                 routing (parser/resolver) or `MAX_CASTS_OF_SAME_CARD_PER_TURN`.",
                state.turn_number,
            );
        }
    }

    assert!(
        matches!(state.waiting_for, WaitingFor::GameOver { .. }),
        "game did not reach GameOver (actions = {total_actions}, turn = {}, waiting_for \
         discriminant = {:?})",
        state.turn_number,
        std::mem::discriminant(&state.waiting_for),
    );
}
