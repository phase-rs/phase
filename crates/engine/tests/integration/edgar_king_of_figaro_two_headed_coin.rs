//! Edgar, King of Figaro — "Two-Headed Coin — The first time you flip one or
//! more coins each turn, those coins come up heads and you win those flips."
//! (phase-rs/phase#9774)
//!
//! CR references (verified against docs/MagicCompRules.txt):
//!   - CR 705.1: flipping a coin; CR 705.2: winning or losing a flip.
//!   - CR 705.3: an effect may state a flip's result and/or that a player wins
//!     it; the actual result is ignored.
//!   - CR 109.5: "you" on a static ability is its current controller.
//!   - CR 614.1a: Krark's Thumb ("instead flip two coins and ignore one").
//!
//! Official rulings (2025-06-06) these tests follow:
//!   1. A set of coins flipped at once ("flip five coins") is ONE time: all of
//!      it is fixed, and the window is spent by any earlier flip that turn.
//!   2. The coins' actual results are ignored.
//!   3. Flips that only care about heads/tails are still won (Chance Encounter
//!      triggers for each).
//!
//! Every card is built from its verbatim Oracle text. RNG outcomes are pinned
//! with the `seed_for` + pin pattern (`coin_flip_until_loss_loop.rs`); a pinned
//! LOSS that the run reports as a WIN is Edgar's fixed result, which is what
//! makes each runtime assertion revert-failing.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::static_abilities::player_wins_first_coin_flip_set_each_turn;
use engine::game::visibility::filter_state_for_viewer;
use engine::parser::parse_oracle_text;
use engine::types::ability::{AbilityKind, Effect, TargetFilter};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;
use engine::types::game_state::{CastPaymentMode, GameRng, GameState, RandomDraw, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::resolution::ResolutionStateWire;
use engine::types::statics::StaticMode;
use engine::types::zones::Zone;
use rand::Rng;

const EDGAR_NAME: &str = "Edgar, King of Figaro";
const EDGAR: &str = "When Edgar enters, draw a card for each artifact you control.\nTwo-Headed Coin — The first time you flip one or more coins each turn, those coins come up heads and you win those flips.";
const STITCH_IN_TIME: &str = "Flip a coin. If you win the flip, take an extra turn after this one.";
const FLOCK_OF_RABID_SHEEP: &str = "Flip X coins. For each flip you win, create a 2/2 green Sheep creature token named Rabid Sheep.";
const MOONLIT_MEDITATION: &str = "Enchant artifact or creature you control\nThe first time you would create one or more tokens each turn, you may instead create that many tokens that are copies of enchanted permanent.";
const RAL_ZAREK: &str = "+1: Tap target permanent, then untap another target permanent.\n−2: Ral Zarek deals 3 damage to any target.\n−7: Flip five coins. Take an extra turn after this one for each coin that comes up heads.";
const CHANCE_ENCOUNTER: &str = "Whenever you win a coin flip, put a luck counter on this enchantment.\nAt the beginning of your upkeep, if this enchantment has ten or more luck counters on it, you win the game.";
const KRARK: &str = "If you would flip a coin, instead flip two coins and ignore one.";
const ZNDRSPLT_NAME: &str = "Zndrsplt, Eye of Wisdom";
const ZNDRSPLT: &str = "Partner with Okaun, Eye of Chaos (When this creature enters, target player may put Okaun into their hand from their library, then shuffle.)\nAt the beginning of combat on your turn, flip a coin until you lose a flip.\nWhenever a player wins a coin flip, draw a card.";
const ZNDRSPLT_KEYWORDS: &[&str] = &["Partner", "Partner with"];

const W: bool = true;
const L: bool = false;

// --- Harness ----------------------------------------------------------------

fn seed_for(outcomes: &[bool]) -> u64 {
    (0..200_000)
        .find(|&seed| {
            let mut rng = GameRng::seed_from_u64(seed);
            outcomes
                .iter()
                .all(|&won| rng.draw(RandomDraw::Outcome).random_bool(0.5) == won)
        })
        .expect("the bounded seed search must find the required outcome sequence")
}

/// Pin the next `outcomes.len()` coin draws.
fn pin(state: &mut GameState, outcomes: &[bool]) {
    let seed = seed_for(outcomes);
    state.rng_seed = seed;
    state.rng_word_pos = 0;
    state.rng = GameRng::seed_from_u64(seed);
}

fn flips(events: &[GameEvent]) -> Vec<(PlayerId, bool)> {
    events
        .iter()
        .filter_map(|event| match event {
            GameEvent::CoinFlipped { player_id, won } => Some((*player_id, *won)),
            _ => None,
        })
        .collect()
}

fn hand_size(runner: &GameRunner, player: PlayerId) -> usize {
    runner.state().players[player.0 as usize].hand.len()
}

fn table() -> GameScenario {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    for player in [P0, P1] {
        scenario.with_library_top(
            player,
            &["Card 1", "Card 2", "Card 3", "Card 4", "Card 5", "Card 6"],
        );
    }
    scenario
}

fn add_edgar(scenario: &mut GameScenario, player: PlayerId) -> ObjectId {
    scenario
        .add_creature_from_oracle(player, EDGAR_NAME, 4, 5, EDGAR)
        .with_summoning_sickness()
        .id()
}

fn add_stitch(scenario: &mut GameScenario, player: PlayerId) -> ObjectId {
    scenario
        .add_spell_to_hand_from_oracle(player, "Stitch in Time", false, STITCH_IN_TIME)
        .with_mana_cost(ManaCost::zero())
        .id()
}

fn add_flock(scenario: &mut GameScenario, player: PlayerId) -> ObjectId {
    scenario
        .add_spell_to_hand_from_oracle(player, "Flock of Rabid Sheep", false, FLOCK_OF_RABID_SHEEP)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::X, ManaCostShard::Green, ManaCostShard::Green],
            generic: 0,
        })
        .id()
}

fn add_green(runner: &mut GameRunner, count: usize) {
    for _ in 0..count {
        let unit = ManaUnit::new(ManaType::Green, ObjectId(0), false, vec![]);
        runner.state_mut().players[0].mana_pool.add(unit);
    }
}

/// Reach guard: the object carries Edgar's parsed CR 705.3 static.
fn assert_edgar_static_parsed(runner: &GameRunner, edgar: ObjectId) {
    assert!(
        runner.state().objects[&edgar]
            .static_definitions
            .iter_all()
            .any(|def| def.mode == StaticMode::WinFirstCoinFlipSetEachTurn),
        "reach guard: Edgar's Two-Headed Coin parsed as WinFirstCoinFlipSetEachTurn"
    );
}

/// What one drive to rest observed.
#[derive(Default)]
struct Drive {
    events: Vec<GameEvent>,
    keep_prompts: usize,
    keep_results: Vec<Vec<bool>>,
    replacement_prompts: usize,
}

/// Answer every decision until P0 holds priority on an empty stack, then assert
/// the completion boundary: no paused resolution remains. Replacement prompts
/// are declined, Krark keep prompts keep the first flip, trigger ordering is
/// identity.
fn drive_to_rest(runner: &mut GameRunner) -> Drive {
    let mut drive = Drive::default();
    for _ in 0..200 {
        let action = match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => {
                assert!(
                    runner.state().resolution_stack.is_empty(),
                    "completion boundary: no paused resolution remains"
                );
                return drive;
            }
            WaitingFor::Priority { .. } => GameAction::PassPriority,
            WaitingFor::OrderTriggers { triggers, .. } => GameAction::OrderTriggers {
                order: (0..triggers.len()).collect(),
            },
            WaitingFor::CoinFlipKeepChoice { results, .. } => {
                drive.keep_prompts += 1;
                drive.keep_results.push(results);
                GameAction::SelectCoinFlips {
                    keep_indices: vec![0],
                }
            }
            WaitingFor::ReplacementChoice { .. } => {
                drive.replacement_prompts += 1;
                GameAction::ChooseReplacement { index: 1 }
            }
            other => panic!("unexpected decision while driving to rest: {other:?}"),
        };
        drive
            .events
            .extend(runner.act(action).expect("the decision is accepted").events);
    }
    panic!("the game did not come to rest within 200 decisions");
}

/// Cast `spell` (X = `x` if given), pin the coin draws once it is on the stack,
/// and drive to rest.
fn cast_pinned(
    runner: &mut GameRunner,
    spell: ObjectId,
    x: Option<u32>,
    outcomes: &[bool],
) -> Drive {
    let cast = runner.cast(spell);
    let mut commit = match x {
        Some(x) => cast.x(x).commit(),
        None => cast.commit(),
    };
    pin(commit.state_mut(), outcomes);
    drop(commit);
    drive_to_rest(runner)
}

fn luck_counters(runner: &GameRunner, id: ObjectId) -> u32 {
    runner.state().objects[&id]
        .counters
        .get(&CounterType::Generic("luck".to_string()))
        .copied()
        .unwrap_or(0)
}

// --- Parser -----------------------------------------------------------------

/// CR 705.3 + CR 109.5: the Two-Headed Coin line (flavor word peeled) parses to
/// one controller-scoped `WinFirstCoinFlipSetEachTurn` static and nothing is
/// left unimplemented; the ETB draw trigger is untouched.
#[test]
fn two_headed_coin_parses_as_a_controller_scoped_static() {
    let parsed = parse_oracle_text(EDGAR, EDGAR_NAME, &[], &["Creature".to_string()], &[]);
    let statics: Vec<_> = parsed
        .statics
        .iter()
        .filter(|def| def.mode == StaticMode::WinFirstCoinFlipSetEachTurn)
        .collect();
    assert_eq!(statics.len(), 1, "exactly one Two-Headed Coin static");
    assert_eq!(statics[0].affected, Some(TargetFilter::Controller));
    assert!(
        parsed
            .abilities
            .iter()
            .all(|a| !matches!(*a.effect, Effect::Unimplemented { .. })),
        "no unimplemented ability remains: {:?}",
        parsed.abilities
    );
    assert_eq!(
        parsed.triggers.len(),
        1,
        "the ETB draw trigger still parses"
    );
}

/// Near misses keep strict failure: a different subject or a missing "you win
/// those flips" clause is not Edgar's ability, so it must not lower to the
/// static. Each keeps an honest `Unimplemented` naming its own text. The
/// positive control is the exact line above.
#[test]
fn two_headed_coin_near_misses_stay_unimplemented() {
    for line in [
        "The first time an opponent flips one or more coins each turn, those coins come up heads and you win those flips.",
        "The first time you flip one or more coins each turn, those coins come up heads.",
        "The first time you flip one or more coins each turn, those coins come up tails and you win those flips.",
    ] {
        let parsed = parse_oracle_text(line, "Near Miss", &[], &["Enchantment".to_string()], &[]);
        assert!(
            parsed
                .statics
                .iter()
                .all(|def| def.mode != StaticMode::WinFirstCoinFlipSetEachTurn),
            "{line}: must not lower to the Edgar static"
        );
        assert!(
            parsed.abilities.iter().any(|a| matches!(
                &*a.effect,
                Effect::Unimplemented { description: Some(d), .. } if d.contains("first time")
            ) && a.kind == AbilityKind::Spell),
            "{line}: keeps an honest Unimplemented, got {:?}",
            parsed.abilities
        );
    }
}

// --- The window: first set each turn ----------------------------------------

/// The issue's own test (#9774), across eight unpinned seeds: P0's first flip
/// of the turn is won. Reach: a flip happened for every seed.
#[test]
fn edgar_first_flip_each_turn_is_won() {
    let outcomes: Vec<Vec<(PlayerId, bool)>> = (0..8)
        .map(|seed| {
            let mut scenario = GameScenario::new_n_player(2, seed);
            scenario.at_phase(Phase::PreCombatMain);
            add_edgar(&mut scenario, P0);
            let stitch = add_stitch(&mut scenario, P0);
            let mut runner = scenario.build();
            let cast = runner.cast(stitch).commit();
            drop(cast);
            flips(&drive_to_rest(&mut runner).events)
        })
        .collect();
    assert!(
        outcomes.iter().all(|o| o.len() == 1),
        "reach: Stitch in Time flipped one coin for every seed: {outcomes:?}"
    );
    assert!(
        outcomes.iter().all(|o| o[0] == (P0, W)),
        "CR 705.3: the first flip is won for every seed: {outcomes:?}"
    );
}

/// The window is the FIRST set only: a pinned loss is won on the first Stitch
/// and stays a loss on the second the same turn. Stitch's win also queues the
/// extra turn, so the fixed result reached the branch.
#[test]
fn only_the_first_flip_set_each_turn_is_fixed() {
    let mut scenario = table();
    let edgar = add_edgar(&mut scenario, P0);
    let first = add_stitch(&mut scenario, P0);
    let second = add_stitch(&mut scenario, P0);
    let mut runner = scenario.build();
    assert_edgar_static_parsed(&runner, edgar);

    let one = cast_pinned(&mut runner, first, None, &[L]);
    assert_eq!(
        flips(&one.events),
        vec![(P0, W)],
        "first set: pinned loss is won"
    );
    assert_eq!(
        runner.state().extra_turns.len(),
        1,
        "the won flip took the extra turn"
    );
    assert!(runner
        .state()
        .players_who_flipped_coins_this_turn
        .contains(&P0));

    let two = cast_pinned(&mut runner, second, None, &[L]);
    assert_eq!(flips(&two.events), vec![(P0, L)], "second set: not fixed");
    assert_eq!(runner.state().extra_turns.len(), 1, "no second extra turn");
}

/// Two instances of the static are one existence query: still only the first
/// set is fixed.
#[test]
fn two_edgars_still_fix_only_the_first_set() {
    let mut scenario = table();
    let edgar = add_edgar(&mut scenario, P0);
    let other = add_edgar(&mut scenario, P0);
    let first = add_stitch(&mut scenario, P0);
    let second = add_stitch(&mut scenario, P0);
    let mut runner = scenario.build();
    assert_edgar_static_parsed(&runner, edgar);
    assert_edgar_static_parsed(&runner, other);

    assert_eq!(
        flips(&cast_pinned(&mut runner, first, None, &[L]).events),
        vec![(P0, W)]
    );
    assert_eq!(
        flips(&cast_pinned(&mut runner, second, None, &[L]).events),
        vec![(P0, L)]
    );
}

/// Ruling 1: a flip made before Edgar arrived spends the turn's window, so
/// Edgar entering afterwards fixes nothing that turn. Edgar is cast after the
/// first flip. Reach: once Edgar resolves, the static applies to P0, and the
/// second flip ran.
#[test]
fn a_flip_before_edgar_arrived_spends_the_window() {
    let mut scenario = table();
    let edgar = scenario
        .add_creature_to_hand_from_oracle(P0, EDGAR_NAME, 4, 5, EDGAR)
        .with_mana_cost(ManaCost::zero())
        .id();
    let first = add_stitch(&mut scenario, P0);
    let second = add_stitch(&mut scenario, P0);
    let mut runner = scenario.build();

    assert_eq!(
        flips(&cast_pinned(&mut runner, first, None, &[L]).events),
        vec![(P0, L)]
    );

    let cast = runner.cast(edgar).commit();
    drop(cast);
    drive_to_rest(&mut runner);
    assert_eq!(
        runner.state().objects[&edgar].zone,
        Zone::Battlefield,
        "reach: Edgar entered"
    );
    assert_edgar_static_parsed(&runner, edgar);
    assert!(
        player_wins_first_coin_flip_set_each_turn(runner.state(), P0),
        "reach guard: Edgar's static now applies to P0"
    );

    let two = cast_pinned(&mut runner, second, None, &[L]);
    assert_eq!(
        flips(&two.events),
        vec![(P0, L)],
        "the window was already spent"
    );
}

/// A run that flips no coins (X = 0) did not flip "one or more coins", so the
/// window stays open for the next flip.
#[test]
fn zero_flips_do_not_spend_the_window() {
    let mut scenario = table();
    add_edgar(&mut scenario, P0);
    let flock = add_flock(&mut scenario, P0);
    let stitch = add_stitch(&mut scenario, P0);
    let mut runner = scenario.build();
    add_green(&mut runner, 2);

    let zero = cast_pinned(&mut runner, flock, Some(0), &[L]);
    assert!(flips(&zero.events).is_empty(), "X = 0 flips nothing");
    assert_eq!(
        runner.state().objects[&flock].zone,
        Zone::Graveyard,
        "reach: Flock resolved"
    );
    assert!(!runner
        .state()
        .players_who_flipped_coins_this_turn
        .contains(&P0));

    let one = cast_pinned(&mut runner, stitch, None, &[L]);
    assert_eq!(
        flips(&one.events),
        vec![(P0, W)],
        "the window was still open"
    );
}

/// A new turn reopens the window (the ledger is cleared at turn start). The
/// first Stitch is fixed and takes P0's extra turn; in that turn the next
/// Stitch is fixed again.
#[test]
fn a_new_turn_reopens_the_window() {
    let mut scenario = table();
    add_edgar(&mut scenario, P0);
    let first = add_stitch(&mut scenario, P0);
    let second = add_stitch(&mut scenario, P0);
    let mut runner = scenario.build();
    let turn = runner.state().turn_number;

    assert_eq!(
        flips(&cast_pinned(&mut runner, first, None, &[L]).events),
        vec![(P0, W)]
    );

    runner.advance_to_end_step();
    runner.advance_to_phase(Phase::PreCombatMain);
    assert_eq!(
        runner.state().turn_number,
        turn + 1,
        "reach: the next turn began"
    );
    assert_eq!(
        runner.state().active_player,
        P0,
        "reach: it is P0's extra turn"
    );
    assert!(
        runner
            .state()
            .players_who_flipped_coins_this_turn
            .is_empty(),
        "turn start clears the ledger"
    );

    assert_eq!(
        flips(&cast_pinned(&mut runner, second, None, &[L]).events),
        vec![(P0, W)]
    );
}

// --- Scope: controller and zone (hostile) ------------------------------------

/// CR 109.5: "you" is Edgar's current controller. Under P1's control, P0's
/// flip is not fixed. Reach: the static parsed and applies to P1, and P0's flip
/// ran.
#[test]
fn edgar_controlled_by_an_opponent_does_not_fix_your_flips() {
    let mut scenario = table();
    let edgar = scenario
        .add_creature_from_oracle(P0, EDGAR_NAME, 4, 5, EDGAR)
        .controlled_by(P1)
        .id();
    let stitch = add_stitch(&mut scenario, P0);
    let mut runner = scenario.build();
    assert_edgar_static_parsed(&runner, edgar);
    assert!(player_wins_first_coin_flip_set_each_turn(
        runner.state(),
        P1
    ));
    assert!(!player_wins_first_coin_flip_set_each_turn(
        runner.state(),
        P0
    ));

    let drive = cast_pinned(&mut runner, stitch, None, &[L]);
    assert_eq!(flips(&drive.events), vec![(P0, L)]);
    assert!(runner
        .state()
        .players_who_flipped_coins_this_turn
        .contains(&P0));
}

/// CR 113.6: Edgar's static functions only on the battlefield. Reach: the
/// graveyard Edgar carries the parsed static and P0's flip ran.
#[test]
fn edgar_in_the_graveyard_fixes_nothing() {
    let mut scenario = table();
    let edgar = scenario
        .add_creature_to_graveyard(P0, EDGAR_NAME, 4, 5)
        .from_oracle_text(EDGAR)
        .id();
    let stitch = add_stitch(&mut scenario, P0);
    let mut runner = scenario.build();
    assert_edgar_static_parsed(&runner, edgar);

    let drive = cast_pinned(&mut runner, stitch, None, &[L]);
    assert_eq!(flips(&drive.events), vec![(P0, L)]);
    assert!(runner
        .state()
        .players_who_flipped_coins_this_turn
        .contains(&P0));
}

// --- Sets: multi-coin, Krark's Thumb, until-loss ------------------------------

fn ral_board(with_krark: bool) -> (GameRunner, ObjectId, ObjectId) {
    let mut scenario = table();
    let edgar = add_edgar(&mut scenario, P0);
    let ral = scenario
        .add_planeswalker_from_oracle(P0, "Ral Zarek", "Ral", 7, RAL_ZAREK)
        .id();
    let chance = scenario
        .add_enchantment_from_oracle(P0, "Chance Encounter", CHANCE_ENCOUNTER)
        .id();
    if with_krark {
        scenario.add_artifact_from_oracle(P0, "Krark's Thumb", KRARK);
    }
    let runner = scenario.build();
    assert_edgar_static_parsed(&runner, edgar);
    (runner, ral, chance)
}

/// Rulings 1 + 3: Ral Zarek's −7 flips five coins at once — one set — so all
/// five come up heads and are won: five extra turns, and Chance Encounter gets
/// five luck counters. Pinned all losses, so every win is Edgar's.
#[test]
fn ral_zarek_ultimate_is_one_fixed_set() {
    let (mut runner, ral, chance) = ral_board(false);
    pin(runner.state_mut(), &[L, L, L, L, L]);
    let outcome = runner.activate(ral, 2).resolve();

    assert_eq!(
        flips(outcome.events()),
        vec![(P0, W); 5],
        "all five coins of the set are won"
    );
    assert_eq!(runner.state().extra_turns.len(), 5, "five extra turns");
    assert_eq!(
        luck_counters(&runner, chance),
        5,
        "ruling 3: five won flips, five luck counters"
    );
    assert!(matches!(
        outcome.final_waiting_for(),
        WaitingFor::Priority { .. }
    ));
    assert!(runner.state().stack.is_empty() && runner.state().resolution_stack.is_empty());
}

/// Probe P2: with Krark's Thumb each of the five flips is doubled, but the set
/// is fixed, so every kept coin is a win: five surviving wins (not ten), no keep
/// prompt, five extra turns and five luck counters.
#[test]
fn ral_zarek_with_krarks_thumb_keeps_five_fixed_wins() {
    let (mut runner, ral, chance) = ral_board(true);
    runner
        .act(GameAction::ActivateAbility {
            source_id: ral,
            ability_index: 2,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("Ral's −7 is activatable");
    pin(runner.state_mut(), &[L; 10]);
    let drive = drive_to_rest(&mut runner);

    assert_eq!(
        flips(&drive.events),
        vec![(P0, W); 5],
        "five surviving flips, all won"
    );
    assert_eq!(
        drive.keep_prompts, 0,
        "a fixed set has a single outcome: no keep choice"
    );
    assert_eq!(runner.state().extra_turns.len(), 5);
    assert_eq!(luck_counters(&runner, chance), 5);
}

fn zndrsplt_at_combat(with_krark: bool) -> GameRunner {
    let mut scenario = table();
    let edgar = add_edgar(&mut scenario, P0);
    scenario
        .add_creature(P0, ZNDRSPLT_NAME, 1, 4)
        .from_oracle_text_with_keywords(ZNDRSPLT_KEYWORDS, ZNDRSPLT)
        .with_summoning_sickness();
    if with_krark {
        scenario.add_artifact_from_oracle(P0, "Krark's Thumb", KRARK);
    }
    let mut runner = scenario.build();
    assert_edgar_static_parsed(&runner, edgar);
    runner.advance_to_phase(Phase::BeginCombat);
    assert_eq!(
        runner.state().phase,
        Phase::BeginCombat,
        "reach: beginning of combat"
    );
    assert_eq!(
        runner.state().stack.len(),
        1,
        "reach: Zndrsplt's run is on the stack"
    );
    runner
}

/// "Flip a coin until you lose a flip" flips one coin at a time, so each flip
/// is its own set: only the first is fixed. Pinned [L, L]: the first is won
/// (Zndrsplt draws), the second stays a loss and ends the run.
#[test]
fn until_loss_fixes_only_its_first_flip() {
    let mut runner = zndrsplt_at_combat(false);
    let hand_before = hand_size(&runner, P0);
    pin(runner.state_mut(), &[L, L]);
    let drive = drive_to_rest(&mut runner);

    assert_eq!(flips(&drive.events), vec![(P0, W), (P0, L)]);
    assert_eq!(
        hand_size(&runner, P0) - hand_before,
        1,
        "one won flip, one draw"
    );
}

/// Keep-choice pause after a fixed first flip, saved and restored while parked.
/// Zndrsplt + Krark's Thumb: flip 1 is doubled but fixed, so it settles inline;
/// flip 2 is doubled and unfixed, so it parks a keep choice. The state
/// round-trips through the resolution wire at that prompt, the window stays
/// spent, and keeping the won coin continues the unfixed run to its loss.
#[test]
fn until_loss_keep_choice_after_a_fixed_first_flip_survives_save_and_load() {
    let mut runner = zndrsplt_at_combat(true);
    // Flip 1: two draws (ignored, fixed). Flip 2: [W, L] offered. Flip 3: [L, L].
    pin(runner.state_mut(), &[L, L, W, L, L, L]);
    let mut events = Vec::new();
    for _ in 0..20 {
        if matches!(
            runner.state().waiting_for,
            WaitingFor::CoinFlipKeepChoice { .. }
        ) {
            break;
        }
        events.extend(runner.act(GameAction::PassPriority).expect("pass").events);
    }
    let WaitingFor::CoinFlipKeepChoice { results, .. } = runner.state().waiting_for.clone() else {
        panic!(
            "reach: flip 2 parks a keep choice, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(
        flips(&events),
        vec![(P0, W)],
        "flip 1 settled inline, fixed"
    );
    assert_eq!(results, vec![W, L], "flip 2 is unfixed: the actual coins");
    assert!(
        runner.state().active_coin_flip_frame().is_some(),
        "reach: the run parked"
    );

    // Production save: capture the live RNG offset (the stream itself is not
    // serialized), then rehydrate it on load.
    runner.state_mut().capture_rng_word_pos();
    let wire = serde_json::to_value(ResolutionStateWire::from_game_state(runner.state().clone()))
        .expect("the parked state serializes");
    assert_eq!(
        wire["players_who_flipped_coins_this_turn"],
        serde_json::json!([0])
    );
    let restored: ResolutionStateWire =
        serde_json::from_value(wire).expect("the parked state round-trips");
    *runner.state_mut() = restored.into_game_state();
    runner.state_mut().rehydrate_rng();
    assert!(runner
        .state()
        .players_who_flipped_coins_this_turn
        .contains(&P0));

    let drive = drive_to_rest(&mut runner);
    events.extend(drive.events);
    assert_eq!(
        flips(&events),
        vec![(P0, W), (P0, W), (P0, L)],
        "kept W continues the run; flip 3 is unfixed and ends it (keeps offered: {:?})",
        drive.keep_results
    );
    assert_eq!(
        drive.keep_prompts, 2,
        "flips 2 and 3 each asked (the first after the reload)"
    );
}

/// ED1-1: a branch choice inside the fixed set. Flock X=3 with Moonlit
/// Meditation: the first won flip's Sheep raises Moonlit's optional
/// replacement. Edgar fixed that first flip and the window is spent; the
/// prompt is declined through the public action and the resolution completes.
///
/// Pre-existing, not changed here: a coin run whose branch raises a resolution
/// choice abandons its remaining flips (the same board without Edgar and with
/// three pinned wins also stops after one flip at base). This test asserts
/// Edgar's contribution and the completion boundary, not the truncated count.
#[test]
fn branch_choice_inside_a_fixed_set_completes() {
    let mut scenario = table();
    add_edgar(&mut scenario, P0);
    let bears = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let moonlit = scenario
        .add_enchantment_from_oracle(P0, "Moonlit Meditation", MOONLIT_MEDITATION)
        .id();
    let flock = add_flock(&mut scenario, P0);
    let mut runner = scenario.build();
    runner.attach_as_bestowed_aura(moonlit, bears);
    add_green(&mut runner, 5);

    let drive = cast_pinned(&mut runner, flock, Some(3), &[L, L, L]);
    assert_eq!(
        drive.replacement_prompts, 1,
        "reach: Moonlit's prompt was raised"
    );
    assert_eq!(
        flips(&drive.events).first(),
        Some(&(P0, W)),
        "the first flip is fixed"
    );
    assert!(runner
        .state()
        .players_who_flipped_coins_this_turn
        .contains(&P0));
}

// --- RNG, state, visibility --------------------------------------------------

/// Decision 6: the coin is still drawn and its result ignored, so a fixed flip
/// consumes the same RNG as the unfixed one.
#[test]
fn a_fixed_flip_consumes_the_same_rng_as_an_unfixed_one() {
    let positions: Vec<u128> = [true, false]
        .into_iter()
        .map(|with_edgar| {
            let mut scenario = table();
            if with_edgar {
                add_edgar(&mut scenario, P0);
            }
            let stitch = add_stitch(&mut scenario, P0);
            let mut runner = scenario.build();
            let cast = runner.cast(stitch).commit();
            drop(cast);
            pin(runner.state_mut(), &[L]);
            let before = runner.state().rng_word_pos;
            let expected = if with_edgar { W } else { L };
            assert_eq!(
                flips(&drive_to_rest(&mut runner).events),
                vec![(P0, expected)]
            );
            runner.state().rng_word_pos - before
        })
        .collect();
    assert_eq!(positions[0], positions[1], "same draw consumption");
}

/// The ledger is public history: viewer filtering keeps it for every viewer.
#[test]
fn the_flip_ledger_survives_viewer_filtering() {
    let mut scenario = table();
    add_edgar(&mut scenario, P0);
    let stitch = add_stitch(&mut scenario, P0);
    let mut runner = scenario.build();
    cast_pinned(&mut runner, stitch, None, &[L]);
    for viewer in [P0, P1] {
        let filtered = filter_state_for_viewer(runner.state(), viewer);
        assert!(
            filtered.players_who_flipped_coins_this_turn.contains(&P0),
            "viewer {viewer:?}"
        );
    }
}

/// Deterministic serialization: two ledgers with the same members inserted in
/// different orders serialize identically; a state written before the field
/// existed loads with an empty ledger.
#[test]
fn the_flip_ledger_serializes_deterministically_and_defaults_when_absent() {
    let runner = table().build();
    let mut forward = runner.state().clone();
    forward.players_who_flipped_coins_this_turn.insert(P0);
    forward.players_who_flipped_coins_this_turn.insert(P1);
    let mut backward = runner.state().clone();
    backward.players_who_flipped_coins_this_turn.insert(P1);
    backward.players_who_flipped_coins_this_turn.insert(P0);
    let forward_json = serde_json::to_value(&forward).expect("serializes");
    assert_eq!(
        forward_json["players_who_flipped_coins_this_turn"],
        serde_json::json!([0, 1])
    );
    assert_eq!(
        serde_json::to_string(&forward).unwrap(),
        serde_json::to_string(&backward).unwrap()
    );

    let mut legacy = forward_json;
    legacy
        .as_object_mut()
        .expect("GameState is a JSON object")
        .remove("players_who_flipped_coins_this_turn");
    let loaded: GameState =
        serde_json::from_value(legacy).expect("a state without the field loads");
    assert!(loaded.players_who_flipped_coins_this_turn.is_empty());
}
