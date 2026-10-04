//! Time the per-action engine refresh on a board of costed mana sources.
//!
//! Reproduces a reported slowdown where every action crawled once a player
//! controlled a handful of Shadowmoor filter lands (`{U/B}, {T}: Add {U}{U},
//! {U}{B}, or {B}{B}`), a Reflecting Pool, and a Vivid land. The board is built
//! from the real card export so the parsed abilities match production, then
//! each stage of the WASM bridge's per-action refresh is timed separately:
//! the public-state display sweep, `legal_actions_full`, the activation-block
//! read-out, and one AI `choose_action`.
//!
//! Build/run with an isolated target dir:
//!   CARGO_TARGET_DIR=target/ai cargo run --profile server-release \
//!       -p phase-ai --features scenario-benches --bin mana-board-bench -- [--cards client/public] [--iters N]

// pod-lab loop-3 Q5: native-binary throughput lever, gated in Cargo.toml so
// wasm32 builds of this crate's lib (pulled in by engine-wasm/draft-wasm)
// never see it.
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

use std::path::PathBuf;
use std::time::{Duration, Instant};

use engine::ai_support;
use engine::database::CardDatabase;
use engine::game::deck_loading::{load_and_hydrate_decks, resolve_deck_list, DeckList};
use engine::game::derived_views::ClientGameStateRef;
use engine::game::{derived, interaction, perf_counters, zones};
use engine::types::counter::CounterType;
use engine::types::game_state::{GameState, PublicStateDirty, WaitingFor};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;
use phase_ai::config::{create_config_for_players, AiDifficulty, Platform};
use phase_ai::search::choose_action;
use rand::rngs::StdRng;
use rand::SeedableRng;

const P0: PlayerId = PlayerId(0);
const P1: PlayerId = PlayerId(1);

/// The acting player's board from the reported game: five filter lands, a
/// Reflecting Pool, a Vivid land, and a basic.
const P0_BATTLEFIELD: &[&str] = &[
    "Vivid Marsh",
    "Sunken Ruins",
    "Sunken Ruins",
    "Island",
    "Mystic Gate",
    "Reflecting Pool",
    "Cascade Bluffs",
    "Cascade Bluffs",
    "Ajani Vengeant",
];
/// A mix of castable and uncastable spells, so the castability probe sees
/// both outcomes.
const P0_HAND: &[&str] = &[
    "Cryptic Command",
    "Cruel Ultimatum",
    "Mulldrifter",
    "Boomerang",
    "Thoughtseize",
    "Rain of Tears",
    "Fulminator Mage",
    "Shriekmaw",
];
const P1_BATTLEFIELD: &[&str] = &[
    "Forest",
    "Forest",
    "Forest",
    "Forest",
    "Forest",
    "Swamp",
    "Llanowar Wastes",
    "Twilight Mire",
    "Twilight Mire",
    "Civic Wayfinder",
    "Chameleon Colossus",
];
const P1_HAND: &[&str] = &["Wrath of God", "Bloodbraid Elf", "Nameless Inversion"];
const LIBRARY_FILLER: usize = 30;

fn deck(cards: &[&[&str]], filler: &str) -> Vec<String> {
    cards
        .iter()
        .flat_map(|names| names.iter().map(|n| n.to_string()))
        .chain(std::iter::repeat_n(filler.to_string(), LIBRARY_FILLER))
        .collect()
}

/// Move the first library card named `name` owned by `player` into `zone`.
fn place(state: &mut GameState, player: PlayerId, name: &str, zone: Zone) {
    let id = state
        .objects
        .values()
        .find(|o| o.owner == player && o.zone == Zone::Library && o.name == name)
        .map(|o| o.id)
        .unwrap_or_else(|| panic!("{name} not in {player:?}'s library"));
    let mut events = Vec::new();
    zones::move_to_zone(state, id, zone, &mut events);
}

fn build_state(db: &CardDatabase) -> GameState {
    let mut list = DeckList::default();
    list.player.main_deck = deck(&[P0_BATTLEFIELD, P0_HAND], "Island");
    list.opponent.main_deck = deck(&[P1_BATTLEFIELD, P1_HAND], "Forest");
    let payload = resolve_deck_list(db, &list);

    let mut state = GameState::new_two_player(42);
    load_and_hydrate_decks(&mut state, &payload, Some(db));
    engine::game::engine::start_game_skip_mulligan(&mut state);

    // Return any opening hand to the library so placement is deterministic.
    let hand: Vec<_> = state
        .objects
        .values()
        .filter(|o| o.zone == Zone::Hand)
        .map(|o| o.id)
        .collect();
    for id in hand {
        let mut events = Vec::new();
        zones::move_to_zone(&mut state, id, Zone::Library, &mut events);
    }

    for (player, battlefield, hand) in
        [(P0, P0_BATTLEFIELD, P0_HAND), (P1, P1_BATTLEFIELD, P1_HAND)]
    {
        for name in battlefield {
            place(&mut state, player, name, Zone::Battlefield);
        }
        for name in hand {
            place(&mut state, player, name, Zone::Hand);
        }
    }

    // Raw placement skips "enters with counters"; restore the Vivid land's
    // charge counters and clear summoning sickness for a mid-game board.
    for id in state.battlefield.clone() {
        let obj = state.objects.get_mut(&id).expect("battlefield object");
        obj.summoning_sick = false;
        if obj.name == "Vivid Marsh" {
            obj.counters
                .insert(CounterType::Generic("charge".to_string()), 2);
        }
    }

    state.turn_number = 20;
    state.phase = Phase::PreCombatMain;
    state.active_player = P0;
    state.priority_player = P0;
    state.waiting_for = WaitingFor::Priority { player: P0 };
    engine::game::layers::flush_layers(&mut state);
    state
}

fn time<T>(iters: u32, mut f: impl FnMut() -> T) -> (Duration, T) {
    let mut last = None;
    let start = Instant::now();
    for _ in 0..iters {
        last = Some(f());
    }
    (start.elapsed() / iters, last.expect("iters > 0"))
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let arg = |flag: &str| args.windows(2).find(|w| w[0] == flag).map(|w| w[1].clone());
    let cards_root = PathBuf::from(arg("--cards").unwrap_or_else(|| "client/public".to_string()));
    let iters: u32 = arg("--iters").and_then(|s| s.parse().ok()).unwrap_or(5);

    let db =
        CardDatabase::from_export(&cards_root.join("card-data.json")).expect("load card-data.json");
    let state = build_state(&db);

    println!("debug_assertions = {}", cfg!(debug_assertions));
    println!("iters = {iters}");
    println!(
        "objects = {}  battlefield = {}",
        state.objects.len(),
        state.battlefield.len()
    );
    println!();

    perf_counters::reset();
    let (display, _) = time(iters, || {
        let mut s = state.clone();
        s.public_state_dirty = PublicStateDirty::all_dirty();
        derived::derive_display_state(&mut s);
    });
    let display_counters = perf_counters::snapshot();

    perf_counters::reset();
    let (legal, (actions, _, _)) = time(iters, || ai_support::legal_actions_full(&state));
    let legal_counters = perf_counters::snapshot();

    perf_counters::reset();
    let (blocks, _) = time(iters, || ai_support::activation_block_reasons(&state));
    let block_counters = perf_counters::snapshot();

    perf_counters::reset();
    let (auto_pass, _) = time(iters, || {
        ai_support::auto_pass_recommended(&state, &actions)
    });
    let auto_pass_counters = perf_counters::snapshot();

    perf_counters::reset();
    let (interaction, _) = time(iters, || {
        interaction::derive_viewer_interaction(&state, &state, state.active_player)
    });
    let interaction_counters = perf_counters::snapshot();

    perf_counters::reset();
    let (snapshot, json_len) = time(iters, || {
        serde_json::to_string(&ClientGameStateRef::wrap(&state, Some(P0)))
            .expect("serialize client state")
            .len()
    });
    let snapshot_counters = perf_counters::snapshot();

    let config =
        create_config_for_players(AiDifficulty::Medium, Platform::Native, 2).into_measurement(42);
    let mut rng = StdRng::seed_from_u64(42);
    perf_counters::reset();
    let (choose, _) = time(1, || choose_action(&state, P0, &config, &mut rng));
    let choose_counters = perf_counters::snapshot();

    let row = |label: &str, dt: Duration, c: &perf_counters::PerfCounterSnapshot, n: u32| {
        println!(
            "{label:<26} {dt:>12.3?}   legality clones/iter = {}",
            c.state_clone_for_legality / u64::from(n)
        );
    };
    println!("legal actions = {}", actions.len());
    row("display sweep", display, &display_counters, iters);
    row("legal_actions_full", legal, &legal_counters, iters);
    row("activation_block_reasons", blocks, &block_counters, iters);
    row(
        "auto_pass_recommended",
        auto_pass,
        &auto_pass_counters,
        iters,
    );
    row(
        "derive_viewer_interaction",
        interaction,
        &interaction_counters,
        iters,
    );
    row("client state JSON", snapshot, &snapshot_counters, iters);
    println!("client state JSON size  {json_len} bytes");
    row("choose_action (P0)", choose, &choose_counters, 1);
}
