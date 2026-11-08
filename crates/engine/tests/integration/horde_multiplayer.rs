//! Horde Magic PR6 — shared-life multiplayer survivor team + setup turns.
//!
//! With 2–4 survivors, the survivors form ONE shared-life team (Two-Headed-Giant
//! style): they share a single combined life total (CR 810.8/810.9a), reused via
//! `topology::has_shared_life_resources`. The Horde occupies the archenemy seat
//! (seat 0) and reuses the OneVsMany game-over path. Before the Horde's first
//! turn, the survivors take `survivor_setup_turns` (3) turns to establish a board
//! (CR 805 shared team turns).
//!
//! Every test names an assertion that flips if the corresponding PR6 wiring is
//! reverted. Negative assertions are paired with a positive reach-guard proving
//! the input reached the seam under test.

use engine::game::deck_loading::{
    load_and_hydrate_decks, load_deck_into_state, DeckEntry, DeckPayload, PlayerDeckPayload,
};
use engine::game::effects::life::apply_damage_life_loss;
use engine::game::engine::{apply, start_game};
use engine::game::players::{team_life_total, teammates};
use engine::game::sba::check_state_based_actions;
use engine::game::turns::{advance_phase, start_next_turn};
use engine::game::zones::create_object;
use engine::types::actions::{GameAction, MulliganChoice};
use engine::types::card::CardFace;
use engine::types::card_type::{CoreType, Supertype};
use engine::types::events::GameEvent;
use engine::types::format::{ChallengeDeck, DeckSizeRule, FormatConfig, SurvivorDeckFormat};
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const HORDE: PlayerId = PlayerId(0);

/// A Horde game with `survivors` survivors (seat 0 = Horde, seats 1.. = survivors).
fn horde_game(survivors: u8) -> GameState {
    GameState::new(
        FormatConfig::horde(ChallengeDeck::CybermanHorde.default_ruleset()),
        survivors + 1,
        42,
    )
}

/// Create a creature on the battlefield under `controller`'s control (keeps the
/// Horde "undefeated" so a survivor loss routes to a Horde win, not a defeat).
fn add_creature(state: &mut GameState, controller: PlayerId) -> ObjectId {
    let card_id = CardId(state.next_object_id);
    let id = create_object(
        state,
        card_id,
        controller,
        "Zombie".to_string(),
        Zone::Battlefield,
    );
    let obj = state.objects.get_mut(&id).unwrap();
    obj.card_types.core_types.push(CoreType::Creature);
    obj.base_card_types = obj.card_types.clone();
    obj.power = Some(2);
    obj.toughness = Some(2);
    obj.base_power = Some(2);
    obj.base_toughness = Some(2);
    id
}

/// The survivors' combined starting life for `survivors` survivors, DERIVED from
/// the Cyberman ruleset's own formula (`combined_base_life + delta * (n - 1)`).
/// These tests assert the shared-life *mechanism*, not a specific balance number,
/// so they track the ruleset rather than hardcode a total that changes when life
/// is retuned. Under the current community rules: 1 → 100, 2 → 85, 3 → 70.
fn expected_combined_life(survivors: i32) -> i32 {
    let r = ChallengeDeck::CybermanHorde.default_ruleset();
    r.combined_base_life + r.per_extra_survivor_life_delta * (survivors - 1)
}

/// The expected per-seat life split: the combined total divided as evenly as
/// possible, remainder on the first survivor — mirroring `GameState::new`.
fn expected_survivor_split(survivors: i32) -> Vec<i32> {
    let combined = expected_combined_life(survivors);
    let base = combined / survivors;
    let remainder = combined - base * survivors;
    (0..survivors)
        .map(|i| if i == 0 { base + remainder } else { base })
        .collect()
}

/// What the team total would be if the distribution block were dropped: every
/// survivor keeps its full undistributed seed (`combined_base_life`), summing to
/// `survivors * combined_base_life`. The revert guards check the team is NOT this.
fn undistributed_team_total(survivors: i32) -> i32 {
    ChallengeDeck::CybermanHorde
        .default_ruleset()
        .combined_base_life
        * survivors
}

/// Combined survivor life: with 2 survivors the survivor-team `team_life_total`
/// equals the configured combined total, NOT the sum of two full undistributed
/// seeds. The combined total is distributed across the survivor seats in
/// `GameState::new`.
///
/// Revert guard: removing the Horde life-distribution block leaves every survivor
/// at the full base seed, so `team_life_total` sums to `undistributed_team_total`
/// and the assertions flip. Reach-guard: the per-seat split sums to the combined
/// total.
#[test]
fn two_survivors_share_one_combined_life_total() {
    let state = horde_game(2);
    let survivor_a = PlayerId(1);
    let survivor_b = PlayerId(2);
    let combined = expected_combined_life(2);
    let split = expected_survivor_split(2);

    // Reach-guard: the split matches the distribution and sums to the combined.
    let a_life = state.players[survivor_a.0 as usize].life;
    let b_life = state.players[survivor_b.0 as usize].life;
    assert_eq!(a_life, split[0], "survivor A gets the first split share");
    assert_eq!(b_life, split[1], "survivor B gets the second split share");
    assert_eq!(
        a_life + b_life,
        combined,
        "per-seat split sums to the combined total"
    );

    // Load-bearing: the shared team total is the combined total, not the sum of
    // two full base seeds.
    assert_eq!(
        team_life_total(&state, survivor_a),
        combined,
        "the survivor team shares ONE combined life total"
    );
    assert_ne!(
        team_life_total(&state, survivor_a),
        undistributed_team_total(2),
        "the survivors must NOT each keep a full base seed"
    );
    // Both survivors read the same shared total.
    assert_eq!(
        team_life_total(&state, survivor_a),
        team_life_total(&state, survivor_b),
        "both survivors read the same shared team total"
    );
}

/// Three survivors (player_count 4) also share one combined total, split as
/// evenly as possible with the remainder on the first survivor.
///
/// Revert guard: same as the 2-survivor case — dropping the distribution leaves
/// three full base seeds summing to `undistributed_team_total(3)`.
#[test]
fn three_survivors_share_one_combined_life_total() {
    let state = horde_game(3);
    let (a, b, c) = (PlayerId(1), PlayerId(2), PlayerId(3));
    let combined = expected_combined_life(3);
    let split = expected_survivor_split(3);

    // Split across 3 survivors: remainder on A, then B and C equal.
    assert_eq!(
        state.players[a.0 as usize].life, split[0],
        "remainder lands on survivor A"
    );
    assert_eq!(state.players[b.0 as usize].life, split[1]);
    assert_eq!(state.players[c.0 as usize].life, split[2]);
    assert_eq!(
        state.players[a.0 as usize].life
            + state.players[b.0 as usize].life
            + state.players[c.0 as usize].life,
        combined,
        "reach-guard: the 3-way split sums to the combined total"
    );

    assert_eq!(
        team_life_total(&state, a),
        combined,
        "the 3-survivor team shares ONE combined life total"
    );
    assert_ne!(
        team_life_total(&state, a),
        undistributed_team_total(3),
        "must not sum three full base seeds"
    );
}

/// Damaging one survivor reduces the SHARED team total (survivors lose life
/// individually per the redirect rules, but every read folds through the team).
///
/// Revert guard: without the shared-life predicate broadening,
/// `shared_life_members` would return just the damaged survivor, so
/// `team_life_total` for the OTHER survivor would be unaffected — the final
/// assertion (both drop) flips.
#[test]
fn damaging_one_survivor_reduces_shared_team_total() {
    let mut state = horde_game(2);
    let survivor_a = PlayerId(1);
    let survivor_b = PlayerId(2);
    let team_before = team_life_total(&state, survivor_a);
    assert_eq!(
        team_before,
        expected_combined_life(2),
        "reach-guard: team starts at the combined total"
    );

    let mut events = Vec::new();
    apply_damage_life_loss(&mut state, survivor_b, 4, &mut events).unwrap();

    assert_eq!(
        team_life_total(&state, survivor_a),
        team_before - 4,
        "damage to survivor B lowers the shared team total"
    );
    // Both survivors observe the reduced shared total.
    assert_eq!(
        team_life_total(&state, survivor_b),
        team_before - 4,
        "survivor B reads the same reduced shared total"
    );
    assert_eq!(
        team_life_total(&state, survivor_a),
        team_life_total(&state, survivor_b),
        "the reduction is shared, not local to the damaged survivor"
    );
}

/// Team loss: when the survivors' SHARED total reaches 0 or less, ALL survivors
/// are eliminated by the CR 704.5a / CR 810.8c life SBA and the Horde wins via the
/// archenemy game-over path — even though one survivor is still individually
/// positive. This proves the loss is on the shared total, not per-seat.
///
/// Revert guard: without the shared-life broadening, `team_life_total(survivor_a)`
/// would equal survivor A's own +5 (positive), so A would NOT be collected as a
/// loser — the "both eliminated" assertion flips.
#[test]
fn shared_total_at_zero_loses_the_whole_team_horde_wins() {
    let mut state = horde_game(2);
    let survivor_a = PlayerId(1);
    let survivor_b = PlayerId(2);
    add_creature(&mut state, HORDE); // Horde undefeated → survivor loss = Horde win.

    // One survivor positive individually, the other deep negative, so the SHARED
    // total is -1 (<= 0) while survivor A alone is +5.
    state.players[survivor_a.0 as usize].life = 5;
    state.players[survivor_b.0 as usize].life = -6;
    state.phase = Phase::PreCombatMain;
    state.active_player = HORDE;
    state.priority_player = HORDE;
    state.waiting_for = WaitingFor::Priority { player: HORDE };

    // Reach-guard: survivor A is individually positive, but the TEAM total is <= 0.
    assert!(
        state.players[survivor_a.0 as usize].life > 0,
        "reach-guard: survivor A is individually still alive on its own life"
    );
    assert!(
        team_life_total(&state, survivor_a) <= 0,
        "reach-guard: the shared team total is 0 or less"
    );

    let mut events: Vec<GameEvent> = Vec::new();
    check_state_based_actions(&mut state, &mut events);

    assert!(
        state.players[survivor_a.0 as usize].is_eliminated,
        "survivor A (individually positive) is eliminated because the TEAM lost"
    );
    assert!(
        state.players[survivor_b.0 as usize].is_eliminated,
        "survivor B is eliminated with the team"
    );
    assert!(
        matches!(
            state.waiting_for,
            WaitingFor::GameOver {
                winner: Some(HORDE)
            }
        ),
        "with the whole survivor team eliminated and the Horde undefeated, the Horde \
         wins via the archenemy path. waiting_for={:?}",
        state.waiting_for
    );
}

/// Setup turns: from a fresh Horde game the survivors take exactly
/// `survivor_setup_turns` (3) turns before the Horde takes its first turn. Driven
/// through the real `start_next_turn` turn-rotation seam.
///
/// Revert guard: without the `turns_to_skip` seeding in `GameState::new`, the
/// Horde's turn is NOT skipped, so the active player after the FIRST
/// `start_next_turn` would already be the Horde — the `actives[1] == survivor`
/// assertion flips.
#[test]
fn survivors_take_setup_turns_before_the_horde() {
    let mut state = horde_game(2);
    // The published Cyberman ruleset gives the survivors 3 setup turns.
    let setup_turns = state
        .format_config
        .horde_ruleset
        .as_ref()
        .unwrap()
        .survivor_setup_turns;
    assert_eq!(
        setup_turns, 3,
        "reach-guard: Cyberman survivor_setup_turns is 3"
    );

    // The survivor turn representative (lowest non-Horde seat).
    let survivor_rep = PlayerId(1);

    // Turn 1 is the initial state (starting_player is a survivor, PR1). Then drive
    // the next several turn boundaries and record who is active at each.
    let mut actives = vec![state.active_player];
    let mut events = Vec::new();
    for _ in 0..3 {
        start_next_turn(&mut state, &mut events);
        actives.push(state.active_player);
    }

    // The first `setup_turns` turns are all the survivor team; the (setup_turns+1)th
    // is the Horde's first turn.
    assert_eq!(
        actives[0], survivor_rep,
        "turn 1 (initial) belongs to the survivor team"
    );
    assert_eq!(
        actives[1], survivor_rep,
        "turn 2 is still the survivor team (the Horde's turn was skipped)"
    );
    assert_eq!(
        actives[2], survivor_rep,
        "turn 3 is still the survivor team"
    );
    assert_eq!(
        actives[3], HORDE,
        "turn 4 is the Horde's FIRST turn — after exactly 3 survivor setup turns"
    );

    // Exactly `survivor_setup_turns` survivor turns precede the Horde's first.
    let survivor_turns_before_horde = actives.iter().take_while(|&&p| p != HORDE).count();
    assert_eq!(
        survivor_turns_before_horde as u8, setup_turns,
        "survivors take exactly survivor_setup_turns turns before the Horde"
    );
}

/// Hostile: the Horde seat is NOT a member of the survivor shared-life team. The
/// Horde's life is never part of `team_life_total(survivor)`, and it is not a
/// teammate of any survivor.
///
/// Revert guard: if the Horde were folded into the survivor team, setting the
/// Horde's life to a large sentinel would inflate `team_life_total(survivor)` and
/// the `== 20` assertion would flip.
#[test]
fn horde_is_not_a_member_of_the_survivor_team() {
    let mut state = horde_game(2);
    let survivor_a = PlayerId(1);
    let survivor_b = PlayerId(2);

    // The Horde is not a teammate of any survivor.
    let survivor_a_teammates = teammates(&state, survivor_a);
    assert!(
        !survivor_a_teammates.contains(&HORDE),
        "the Horde must not be a teammate of a survivor"
    );
    assert!(
        survivor_a_teammates.contains(&survivor_b),
        "reach-guard: the other survivor IS a teammate (team membership is real)"
    );

    // Set the Horde's underlying life field to a large sentinel; the survivor team
    // total must be unaffected (the Horde's life is excluded).
    state.players[HORDE.0 as usize].life = 999;
    assert_eq!(
        team_life_total(&state, survivor_a),
        expected_combined_life(2),
        "the Horde's life is NOT part of the survivor team total"
    );
}

/// Regression: a SINGLE-survivor Horde game (player_count 2) still works — the
/// survivor's team total equals its own (unsplit) life, a degenerate team of one.
///
/// Revert guard: an off-by-one in the distribution (e.g. dividing by the wrong
/// count) would change the sole survivor's life away from the combined total.
#[test]
fn single_survivor_team_total_equals_own_life() {
    let state = horde_game(1);
    let survivor = PlayerId(1);
    // A lone survivor keeps the full single-survivor combined base (no delta).
    let combined = expected_combined_life(1);

    assert_eq!(
        state.players[survivor.0 as usize].life, combined,
        "a lone survivor keeps the full combined base life"
    );
    assert_eq!(
        team_life_total(&state, survivor),
        combined,
        "a degenerate team of one reads its own life as the team total"
    );
    // The Horde's default life is untouched by the distribution (it has no life total).
    assert_eq!(
        state.players[HORDE.0 as usize].life, state.format_config.starting_life,
        "the Horde seat's life field is left at its seeded value (never consulted)"
    );
}

/// Regression (found by the headless AI sim): the REAL game-start seam must give
/// the survivors the first turn and their setup turns. `start_game` previously
/// took an unconditional archenemy branch (CR 904.6) that forced the Horde (= the
/// archenemy seat) to take turn 1, overriding the survivor-first
/// `FormatConfig::starting_player()` and never engaging the `turns_to_skip`
/// setup-turn seeding — so survivors were steamrolled from turn 1. The prior
/// setup-turn test drove `start_next_turn` directly and missed this, because the
/// bug lived in `start_game` / `start_game_with_starting_player`.
///
/// Revert guard: restoring either unconditional archenemy override makes turn 1
/// the Horde — the `active_player != HORDE` assertion flips.
#[test]
fn horde_game_start_gives_survivors_the_first_turn_and_setup_turns() {
    let mut state = horde_game(2);
    let survivor_rep = PlayerId(1);
    let setup_turns = state
        .format_config
        .horde_ruleset
        .as_ref()
        .unwrap()
        .survivor_setup_turns;
    assert_eq!(
        setup_turns, 3,
        "reach-guard: Cyberman survivor_setup_turns is 3"
    );

    // Drive the ACTUAL production start seam (not `start_next_turn` directly).
    start_game(&mut state);

    assert_ne!(
        state.active_player, HORDE,
        "the Horde must NOT take the first turn — the survivors set up first"
    );
    assert_eq!(
        state.active_player, survivor_rep,
        "turn 1 belongs to the survivor team"
    );

    // Exactly `setup_turns` survivor turns precede the Horde's first turn.
    let mut actives = vec![state.active_player];
    let mut events = Vec::new();
    for _ in 0..3 {
        start_next_turn(&mut state, &mut events);
        actives.push(state.active_player);
    }
    let survivor_turns_before_horde = actives.iter().take_while(|&&p| p != HORDE).count();
    assert_eq!(
        survivor_turns_before_horde as u8, setup_turns,
        "survivors take exactly survivor_setup_turns turns before the Horde via the real start_game path; got {actives:?}"
    );
    assert_eq!(
        actives[3],
        HORDE,
        "the Horde's first turn is turn {}",
        setup_turns + 1
    );
}

/// A `Supertype::Legendary` creature `CardFace` — an eligible EDH commander.
fn survivor_commander_face() -> CardFace {
    let mut face = CardFace {
        name: "Survivor General".to_string(),
        ..CardFace::default()
    };
    face.card_type.supertypes.push(Supertype::Legendary);
    face.card_type.core_types.push(CoreType::Creature);
    face
}

/// A basic land `CardFace` for filling out the 99-card main deck.
fn plains_face() -> CardFace {
    let mut face = CardFace {
        name: "Plains".to_string(),
        ..CardFace::default()
    };
    face.card_type.supertypes.push(Supertype::Basic);
    face.card_type.core_types.push(CoreType::Land);
    face.card_type.subtypes.push("Plains".to_string());
    face
}

/// A Commander-format Horde (e.g. Sauron/Saruman, built for EDH survivors)
/// wires the SURVIVORS as EDH players end-to-end: `command_zone`, the 21
/// commander-damage SBA, `uses_commander`, and a 100-card deck size all derive
/// from `SurvivorDeckFormat::Commander`, and the data-driven deck loader places
/// the survivor's commander in the command zone. This proves survivors can bring
/// and play full EDH decks against the Horde.
///
/// Revert guard: if the survivor-format axis stopped driving the command
/// zone / threshold, the config asserts flip; if `load_deck_into_state` stopped
/// honoring the survivor's commander slot, the command-zone lookup returns None.
#[test]
fn commander_format_horde_wires_survivor_commanders() {
    let ruleset = ChallengeDeck::SauronHorde.default_ruleset();
    assert_eq!(
        ruleset.survivor_deck_format,
        SurvivorDeckFormat::Commander,
        "reach-guard: Sauron survivors are EDH (Commander) players"
    );

    let config = FormatConfig::horde(ruleset);
    // The survivor EDH axis: command zone + 21 commander-damage + 100-card deck.
    assert!(
        config.command_zone,
        "Horde-Commander survivors get a command zone"
    );
    assert_eq!(
        config.commander_damage_threshold,
        Some(21),
        "CR 903.10a commander-damage SBA is active for survivors"
    );
    assert!(config.uses_commander);
    assert_eq!(config.deck_size, DeckSizeRule::Exactly(100));

    // Seat 0 = Horde, seat 1 = survivor. The loader maps `payload.opponent`
    // to seat 1, so the survivor's EDH deck (99 basics + a commander) goes there.
    let mut state = GameState::new(config, 2, 42);
    let survivor = PlayerId(1);

    let payload = DeckPayload {
        opponent: PlayerDeckPayload {
            main_deck: vec![DeckEntry {
                card: plains_face(),
                count: 99,
            }],
            commander: vec![DeckEntry {
                card: survivor_commander_face(),
                count: 1,
            }],
            ..PlayerDeckPayload::default()
        },
        ..DeckPayload::default()
    };
    load_deck_into_state(&mut state, &payload);

    // The survivor's commander is a real command-zone commander they own.
    let placed = state
        .command_zone
        .iter()
        .map(|id| &state.objects[id])
        .find(|o| o.is_commander && o.owner == survivor);
    assert!(
        placed.is_some(),
        "the survivor's commander is placed in the command zone"
    );
    assert_eq!(
        placed.unwrap().name,
        "Survivor General",
        "the placed commander is the survivor's chosen EDH commander"
    );

    // The Horde seat (0) runs the challenge library, never an EDH commander.
    assert!(
        !state
            .command_zone
            .iter()
            .any(|id| state.objects[id].is_commander && state.objects[id].owner == HORDE),
        "the Horde seat has no commander"
    );
}

/// R4-#5 — a POPULATED Horde placeholder commander slot must not place: the
/// Horde seat's submitted deck is wholly replaced by its engine-supplied
/// challenge deck (`load_horde_library`), so whatever sits in seat 0's
/// `commander` slot is inert — while the survivors' commanders still load.
/// Companion to `commander_format_horde_wires_survivor_commanders`, whose
/// Horde payload is EMPTY and therefore never exercises the skip.
///
/// Revert guard: placing submitted commanders for every seat seats the
/// placeholder in the command zone — the absence assertion flips. Skipping
/// too broadly (survivors included) unseats the real EDH commander — the
/// presence assertion flips.
#[test]
fn populated_horde_placeholder_commander_is_not_placed() {
    let ruleset = ChallengeDeck::SauronHorde.default_ruleset();
    let config = FormatConfig::horde(ruleset);
    let mut state = GameState::new(config, 2, 42);
    let survivor = PlayerId(1);

    // Seat 0 = Horde with a POPULATED placeholder slot (the shape a deck
    // builder submits before the engine replaces the whole seat deck);
    // seat 1 = survivor with a real EDH deck.
    let mut placeholder = CardFace {
        name: "Horde Placeholder".to_string(),
        ..CardFace::default()
    };
    placeholder.card_type.supertypes.push(Supertype::Legendary);
    placeholder.card_type.core_types.push(CoreType::Creature);
    let payload = DeckPayload {
        player: PlayerDeckPayload {
            main_deck: vec![DeckEntry {
                card: plains_face(),
                count: 99,
            }],
            commander: vec![DeckEntry {
                card: placeholder,
                count: 1,
            }],
            ..PlayerDeckPayload::default()
        },
        opponent: PlayerDeckPayload {
            main_deck: vec![DeckEntry {
                card: plains_face(),
                count: 99,
            }],
            commander: vec![DeckEntry {
                card: survivor_commander_face(),
                count: 1,
            }],
            ..PlayerDeckPayload::default()
        },
        ..DeckPayload::default()
    };
    load_deck_into_state(&mut state, &payload);

    // The survivor's commander still loads.
    let placed = state
        .command_zone
        .iter()
        .map(|id| &state.objects[id])
        .find(|o| o.is_commander && o.owner == survivor);
    assert!(
        placed.is_some(),
        "the survivor's commander is placed in the command zone"
    );
    assert_eq!(
        placed.unwrap().name,
        "Survivor General",
        "the placed commander is the survivor's chosen EDH commander"
    );

    // The populated Horde placeholder seats nothing.
    assert!(
        !state
            .command_zone
            .iter()
            .any(|id| { state.objects[id].is_commander && state.objects[id].owner == HORDE }),
        "no Horde-seat commander may be placed from the replaced placeholder slot"
    );
    assert!(
        !state
            .command_zone
            .iter()
            .any(|id| state.objects[id].name == "Horde Placeholder"),
        "the placeholder card itself must not appear in the command zone"
    );
}

/// Two-Horde wave dispatch: EVERY Horde commander's precombat main seeds a
/// wave — not just the primary (archenemy) seat. Two Towers alternates Sauron
/// (seat 0) and Saruman (seat 1) Horde turns (Reading B); the precombat
/// dispatch must gate on the Horde-seat authority so the secondary commander
/// reveals too. Driven through the real `start_game` / `advance_phase` seams
/// (turns roll over inside `advance_phase`); the wave is observed (seeded
/// budget) but never drained (no priority passes), so each observation is
/// independent.
///
/// Order-agnostic by design: the exact turn order (starting seat, setup-skip
/// consumption) is owned by the setup fixes, so this test observes Horde turns
/// until BOTH commanders have acted and asserts a wave on each. Commander
/// alternation itself is pinned by `begin_wave_rotates_the_active_horde_commander`.
///
/// Revert guard: gating the dispatch on `topology::archenemy` again leaves
/// Saruman's precombats unseeded — the per-turn `> 0` assertion flips.
#[test]
fn both_horde_commanders_seed_a_wave_on_each_horde_turn() {
    const SAURON: PlayerId = PlayerId(0);
    const SARUMAN: PlayerId = PlayerId(1);
    const SURVIVOR: PlayerId = PlayerId(2);

    let mut state = GameState::new(
        FormatConfig::horde(ChallengeDeck::LotrTwoTowersHorde.default_ruleset()),
        3,
        42,
    );
    // Reach-guard: both Horde seats designated (no silent drop).
    assert_eq!(
        state.horde_seats,
        vec![SAURON, SARUMAN],
        "Two Towers designates both Horde seats"
    );

    // Real libraries through the canonical init path: challenge libraries for
    // both Horde seats, 60 basics for the survivor (it draws every turn).
    let db = crate::support::shared_card_db().expect("fixture has Two Towers cards");
    let payload = DeckPayload {
        ai_decks: vec![PlayerDeckPayload {
            main_deck: vec![DeckEntry {
                card: plains_face(),
                count: 60,
            }],
            ..PlayerDeckPayload::default()
        }],
        ..DeckPayload::default()
    };
    load_and_hydrate_decks(&mut state, &payload, Some(db));
    for seat in [SAURON, SARUMAN] {
        assert!(
            !state.players[seat.0 as usize].library.is_empty(),
            "reach-guard: {seat:?} has a loaded challenge library"
        );
    }
    assert_eq!(
        state.players[SURVIVOR.0 as usize].library.len(),
        60,
        "reach-guard: the survivor has a 60-card library to draw from"
    );

    start_game(&mut state);

    // The survivor drew an opening hand, so the game opens on its mulligan
    // prompt (the handless Horde seats are not pending). Keep through the
    // public action path to reach a drivable turn 1.
    assert!(
        matches!(state.waiting_for, WaitingFor::MulliganDecision { .. }),
        "start_game with a real survivor library must open the mulligan (got {:?})",
        state.waiting_for
    );
    let _ = apply(
        &mut state,
        SURVIVOR,
        GameAction::MulliganDecision {
            choice: MulliganChoice::Keep,
        },
    );
    assert!(
        !matches!(state.waiting_for, WaitingFor::MulliganDecision { .. }),
        "the survivor's KEEP must complete the mulligan (got {:?})",
        state.waiting_for
    );

    // Drive whole turns purely through `advance_phase` (turns roll over
    // internally at Cleanup). Each hop enters exactly one phase; a transition
    // INTO precombat is a real dispatch firing. On every Horde precombat
    // entry, assert the wave seeded. Stop once EACH commander has taken two
    // waved turns — order-agnostic (no assumption about the starting seat or
    // setup-skip consumption), but sustained across rotations.
    let mut events = Vec::new();
    let mut waves: Vec<(PlayerId, u32)> = Vec::new();
    let mut prev_phase = state.phase;
    for _ in 0..400 {
        let sauron_waves = waves.iter().filter(|(s, _)| *s == SAURON).count();
        let saruman_waves = waves.iter().filter(|(s, _)| *s == SARUMAN).count();
        if sauron_waves >= 2 && saruman_waves >= 2 {
            break;
        }
        advance_phase(&mut state, &mut events);
        if state.phase == Phase::PreCombatMain
            && prev_phase != Phase::PreCombatMain
            && (state.active_player == SAURON || state.active_player == SARUMAN)
        {
            // Load-bearing per turn: this commander's precombat seeded a wave
            // (UntilRarity seed = library size > 0).
            assert!(
                state.horde_wave_remaining > 0,
                "{:?}'s precombat must seed a wave, got budget {}",
                state.active_player,
                state.horde_wave_remaining
            );
            waves.push((state.active_player, state.horde_wave_remaining));
        }
        prev_phase = state.phase;
    }
    let seats: Vec<PlayerId> = waves.iter().map(|(s, _)| *s).collect();
    assert!(
        seats.contains(&SAURON) && seats.contains(&SARUMAN),
        "both commanders take waved turns, got {waves:?}"
    );
    assert!(
        waves.iter().filter(|(s, _)| *s == SAURON).count() >= 2
            && waves.iter().filter(|(s, _)| *s == SARUMAN).count() >= 2,
        "sustained waved turns across rotations, got {waves:?}"
    );
}
