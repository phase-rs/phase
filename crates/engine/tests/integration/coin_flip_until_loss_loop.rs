//! "Flip a coin until you lose a flip." as a complete instruction (CR 705.1 + CR 705.2),
//! and the per-win triggers it feeds (CR 603.2c, CR 603.3, CR 603.3b).
//!
//! Zndrsplt, Eye of Wisdom; Okaun, Eye of Chaos; and Toothy and Zndrsplt all carry
//! "At the beginning of combat on your turn, flip a coin until you lose a flip." with no
//! reward clause. At the base commit the parser lowered every until-loss run with an
//! `Effect::unimplemented("flip_coin_until_lose_stub", ..)` as its `win_effect`, which only
//! a following clause, absorbed by `consolidate_die_and_coin_defs`, could replace, so all
//! three cards were marked unsupported.
//!
//! What this file proves, and what it does not:
//!
//! * REVERT-FAILING (parse / support): `three_faces_parse_the_combat_run_complete`,
//!   `three_faces_are_supported` and `combat_trigger_on_the_stack_is_a_complete_until_loss_run`
//!   fail against the base parser. The base runtime already looped to the first loss and
//!   already paid one trigger per won flip, so for this change a behavior-level revert
//!   failure does not exist: the stub's branch was skipped as an `Unimplemented` effect and
//!   recorded nothing observable. The revert-failing proof is the parser output and the
//!   support authority, not the runtime.
//! * PRESERVATION PINS (every other test): each "passes at BASE; derived from Oracle text +
//!   CR" and carries the derived value in its doc comment. They pin the runtime half of the
//!   claim (one trigger per won flip, flipper only) so a later change to the run or the
//!   matcher cannot regress it. The Krark's Thumb interaction is not covered by this file.
//!
//! Every card is built from its verbatim Oracle text (MTGJSON `AtomicCards.json`). RNG
//! outcomes are pinned after the trigger is on the stack, using the `seed_for` + pin
//! pattern of `choose_flip_context_coverage.rs`; each runtime test asserts the observed
//! `CoinFlipped` sequence as its reach guard.

use engine::game::coverage::card_face_has_unimplemented_parts;
use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::zones::move_to_zone;
use engine::parser::parse_oracle_text;
use engine::types::ability::{CoinFlipResult, CoinFlipRun, Effect, TargetFilter};
use engine::types::actions::GameAction;
use engine::types::card::CardFace;
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;
use engine::types::game_state::{GameRng, RandomDraw, StackEntryKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::triggers::TriggerMode;
use engine::types::zones::Zone;
use rand::Rng;

const ZNDRSPLT: &str = "Partner with Okaun, Eye of Chaos (When this creature enters, target player may put Okaun into their hand from their library, then shuffle.)\nAt the beginning of combat on your turn, flip a coin until you lose a flip.\nWhenever a player wins a coin flip, draw a card.";
const OKAUN: &str = "Partner with Zndrsplt, Eye of Wisdom (When this creature enters, target player may put Zndrsplt into their hand from their library, then shuffle.)\nAt the beginning of combat on your turn, flip a coin until you lose a flip.\nWhenever a player wins a coin flip, double Okaun's power and toughness until end of turn.";
const TOOTHY: &str = "Whenever you win a coin flip, put a +1/+1 counter on Toothy and Zndrsplt.\nAt the beginning of combat on your turn, flip a coin until you lose a flip.\nWhen Toothy and Zndrsplt leaves the battlefield, draw a card for each +1/+1 counter on Toothy and Zndrsplt.";

const ZNDRSPLT_NAME: &str = "Zndrsplt, Eye of Wisdom";
const OKAUN_NAME: &str = "Okaun, Eye of Chaos";
const TOOTHY_NAME: &str = "Toothy and Zndrsplt";

/// MTGJSON `keywords` for each face, so the Partner-with line routes as it does in card data.
const ZNDRSPLT_KEYWORDS: &[&str] = &["Partner", "Partner with"];
const OKAUN_KEYWORDS: &[&str] = &["Double", "Partner", "Partner with"];
const TOOTHY_KEYWORDS: &[&str] = &[];

const W: bool = true;
const L: bool = false;

fn seed_for(outcomes: &[bool]) -> u64 {
    (0..100_000)
        .find(|&seed| {
            let mut rng = GameRng::seed_from_u64(seed);
            outcomes
                .iter()
                .all(|&won| rng.draw(RandomDraw::Outcome).random_bool(0.5) == won)
        })
        .expect("the bounded seed search must find the required outcome sequence")
}

/// Pin the next `outcomes.len()` coin draws. Call after the trigger or spell is on the stack
/// and before the flips.
fn pin(runner: &mut GameRunner, outcomes: &[bool]) {
    let seed = seed_for(outcomes);
    let state = runner.state_mut();
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
    runner
        .state()
        .players
        .iter()
        .find(|p| p.id == player)
        .expect("player exists")
        .hand
        .len()
}

fn plus_counters(runner: &GameRunner, id: ObjectId) -> u32 {
    runner.state().objects[&id]
        .counters
        .get(&CounterType::Plus1Plus1)
        .copied()
        .unwrap_or(0)
}

fn strings(names: &[&str]) -> Vec<String> {
    names.iter().map(|name| name.to_string()).collect()
}

/// Stock both libraries so every draw has a card to take.
fn stock_libraries(scenario: &mut GameScenario) {
    for player in [P0, P1] {
        scenario.with_library_top(
            player,
            &["Card 1", "Card 2", "Card 3", "Card 4", "Card 5", "Card 6"],
        );
    }
}

fn add_zndrsplt(scenario: &mut GameScenario, player: PlayerId) -> ObjectId {
    scenario
        .add_creature(player, ZNDRSPLT_NAME, 1, 4)
        .from_oracle_text_with_keywords(ZNDRSPLT_KEYWORDS, ZNDRSPLT)
        .id()
}

fn add_okaun(scenario: &mut GameScenario, player: PlayerId) -> ObjectId {
    scenario
        .add_creature(player, OKAUN_NAME, 3, 3)
        .from_oracle_text_with_keywords(OKAUN_KEYWORDS, OKAUN)
        .id()
}

fn add_toothy(scenario: &mut GameScenario, player: PlayerId) -> ObjectId {
    scenario
        .add_creature(player, TOOTHY_NAME, 2, 2)
        .from_oracle_text_with_keywords(TOOTHY_KEYWORDS, TOOTHY)
        .id()
}

/// Advance from PreCombatMain to the beginning-of-combat step and return with the combat
/// trigger(s) on the stack.
fn to_beginning_of_combat(scenario: GameScenario) -> GameRunner {
    let mut runner = scenario.build();
    runner.advance_to_phase(Phase::BeginCombat);
    assert_eq!(
        runner.state().phase,
        Phase::BeginCombat,
        "reach guard: the beginning-of-combat step was reached"
    );
    runner
}

fn two_player_table() -> GameScenario {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    stock_libraries(&mut scenario);
    scenario
}

/// Pass priority until the stack is empty and the table is at rest, draining any
/// `OrderTriggers` prompt with the identity order (the triggers in these tests that share a
/// controller are indistinguishable). Returns every event produced on the way.
fn drive_to_rest(runner: &mut GameRunner) -> Vec<GameEvent> {
    let mut events = Vec::new();
    for _ in 0..60 {
        match runner.state().waiting_for.clone() {
            WaitingFor::OrderTriggers { triggers, .. } => {
                let result = runner
                    .act(GameAction::OrderTriggers {
                        order: (0..triggers.len()).collect(),
                    })
                    .expect("identity trigger order is accepted");
                events.extend(result.events);
            }
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => {
                assert!(
                    runner.state().resolution_stack.is_empty(),
                    "no paused resolution remains"
                );
                return events;
            }
            WaitingFor::Priority { .. } => {
                let result = runner
                    .act(GameAction::PassPriority)
                    .expect("pass priority to resolve the stack");
                events.extend(result.events);
            }
            other => panic!("unexpected prompt while driving to rest: {other:?}"),
        }
    }
    panic!("the stack did not empty within 60 steps");
}

/// Pass priority until the combat trigger `combat_id` has left the stack, draining
/// `OrderTriggers` with the identity order. Stops before the triggers the run produced are
/// resolved. Returns the events produced.
fn resolve_combat_trigger(runner: &mut GameRunner, combat_id: ObjectId) -> Vec<GameEvent> {
    let mut events = Vec::new();
    for _ in 0..20 {
        match runner.state().waiting_for.clone() {
            WaitingFor::OrderTriggers { triggers, .. } => {
                events.extend(
                    runner
                        .act(GameAction::OrderTriggers {
                            order: (0..triggers.len()).collect(),
                        })
                        .expect("identity trigger order is accepted")
                        .events,
                );
            }
            WaitingFor::Priority { .. }
                if runner
                    .state()
                    .stack
                    .iter()
                    .any(|entry| entry.id == combat_id) =>
            {
                events.extend(
                    runner
                        .act(GameAction::PassPriority)
                        .expect("pass priority toward the combat trigger")
                        .events,
                );
            }
            _ => return events,
        }
    }
    panic!("the combat trigger did not leave the stack within 20 steps");
}

/// Parse `oracle` the way card data does and wrap it in a face for the support authority.
fn face_for(name: &str, keywords: &[&str], oracle: &str) -> CardFace {
    let parsed = parse_oracle_text(
        oracle,
        name,
        &strings(keywords),
        &strings(&["Creature"]),
        &[],
    );
    CardFace {
        name: name.to_string(),
        oracle_text: Some(oracle.to_string()),
        abilities: parsed.abilities,
        triggers: parsed.triggers,
        static_abilities: parsed.statics,
        replacements: parsed.replacements,
        additional_cost: parsed.additional_cost,
        ..Default::default()
    }
}

/// Assert `effect` is the complete, reward-less until-loss run for the controller.
fn assert_complete_until_loss_run(effect: &Effect) {
    let Effect::FlipCoins {
        flips: CoinFlipRun::UntilLoss,
        win_effect: None,
        lose_effect: None,
        flipper: TargetFilter::Controller,
    } = effect
    else {
        panic!("CR 705.1: expected a complete until-loss run with no stub reward, got {effect:?}");
    };
}

// --- REVERT-FAILING: parse shape and support -----------------------------------------------

/// P2. CR 705.1 + CR 705.2: "flip a coin until you lose a flip" with no per-win clause is a
/// complete instruction. REVERT: restoring the `flip_coin_until_lose_stub` in
/// `lower_imperative_family_effect` makes the Phase trigger's `win_effect`
/// `Some(Unimplemented)`, so the shape assertion fails for all three faces.
#[test]
fn three_faces_parse_the_combat_run_complete() {
    for (name, keywords, oracle, flipped_coin_target) in [
        (
            ZNDRSPLT_NAME,
            ZNDRSPLT_KEYWORDS,
            ZNDRSPLT,
            // "a player": the trigger names no restriction on who flipped.
            Some(TargetFilter::Player),
        ),
        (
            OKAUN_NAME,
            OKAUN_KEYWORDS,
            OKAUN,
            Some(TargetFilter::Player),
        ),
        // "you": only the controller's wins.
        (
            TOOTHY_NAME,
            TOOTHY_KEYWORDS,
            TOOTHY,
            Some(TargetFilter::Controller),
        ),
    ] {
        let face = face_for(name, keywords, oracle);

        let combat = face
            .triggers
            .iter()
            .find(|t| t.mode == TriggerMode::Phase && t.phase == Some(Phase::BeginCombat))
            .unwrap_or_else(|| {
                panic!("{name}: reach guard, the beginning-of-combat trigger parsed")
            });
        assert!(
            combat.constraint.is_some(),
            "{name}: reach guard, 'on your turn' parsed as a trigger constraint"
        );
        let execute = combat
            .execute
            .as_ref()
            .unwrap_or_else(|| panic!("{name}: the combat trigger has an effect"));
        assert_complete_until_loss_run(&execute.effect);
        assert!(
            execute.sub_ability.is_none(),
            "{name}: the run has no following clause"
        );

        let won = face
            .triggers
            .iter()
            .find(|t| t.mode == TriggerMode::FlippedCoin)
            .unwrap_or_else(|| panic!("{name}: reach guard, the won-flip trigger parsed"));
        assert_eq!(won.coin_flip_result, Some(CoinFlipResult::Won), "{name}");
        assert_eq!(won.valid_target, flipped_coin_target, "{name}");
    }
}

/// Support authority: the three faces carry no unsupported part, so card data marks them
/// supported. REVERT: with the `flip_coin_until_lose_stub` restored, the Phase trigger's
/// `win_effect` is `Some(Unimplemented)` and `card_face_has_unimplemented_parts` is true for
/// all three faces. Reach guard: the combat trigger parsed on each face.
#[test]
fn three_faces_are_supported() {
    for (name, keywords, oracle) in [
        (ZNDRSPLT_NAME, ZNDRSPLT_KEYWORDS, ZNDRSPLT),
        (OKAUN_NAME, OKAUN_KEYWORDS, OKAUN),
        (TOOTHY_NAME, TOOTHY_KEYWORDS, TOOTHY),
    ] {
        let face = face_for(name, keywords, oracle);
        assert!(
            face.triggers
                .iter()
                .any(|t| t.mode == TriggerMode::Phase && t.phase == Some(Phase::BeginCombat)),
            "{name}: reach guard, the beginning-of-combat trigger parsed"
        );
        assert!(
            !card_face_has_unimplemented_parts(&face),
            "{name} must parse with no unsupported part: {face:#?}"
        );
    }
}

/// Pipeline-driven: the combat trigger put on the real stack carries the complete run.
/// REVERT: at BASE the stack entry's `win_effect` is `Some(Unimplemented stub)`.
#[test]
fn combat_trigger_on_the_stack_is_a_complete_until_loss_run() {
    type Add = fn(&mut GameScenario, PlayerId) -> ObjectId;
    let cases: [(&str, Add); 3] = [
        (ZNDRSPLT_NAME, add_zndrsplt),
        (OKAUN_NAME, add_okaun),
        (TOOTHY_NAME, add_toothy),
    ];
    for (name, add) in cases {
        let mut scenario = two_player_table();
        let source = add(&mut scenario, P0);
        let runner = to_beginning_of_combat(scenario);

        // Reach guards: exactly the one combat trigger is on the stack, from this source
        // (the Partner-with enters trigger did not fire for a card placed on the battlefield).
        assert_eq!(runner.state().stack.len(), 1, "{name}: one trigger");
        let top = runner.state().stack.back().expect("trigger on the stack");
        assert_eq!(top.source_id, source, "{name}");
        assert_eq!(top.controller, P0, "{name}");
        let StackEntryKind::TriggeredAbility { ability, .. } = &top.kind else {
            panic!(
                "{name}: the stack entry is a triggered ability, got {:?}",
                top.kind
            );
        };
        assert_complete_until_loss_run(&ability.effect);
        let serialized = serde_json::to_string(ability).expect("stack ability serializes");
        assert!(
            !serialized.contains("Unimplemented"),
            "{name}: no Unimplemented anywhere in the ability tree: {serialized}"
        );
    }
}

// --- PRESERVATION PINS (pass at BASE; derived from Oracle text + CR) ---------------------------

/// Z1. Derived: the controller flips until the first loss, so W,W,L is three `CoinFlipped`
/// events for P0 (CR 705.1 + CR 705.2). "Whenever a player wins a coin flip" triggers once per
/// won flip (CR 603.2c), so P0 draws 2. PRESERVATION PIN, passes at BASE.
#[test]
fn zndrsplt_draws_once_per_won_flip_until_the_loss() {
    let mut scenario = two_player_table();
    add_zndrsplt(&mut scenario, P0);
    let mut runner = to_beginning_of_combat(scenario);
    let hand_before = hand_size(&runner, P0);

    pin(&mut runner, &[W, W, L]);
    let events = drive_to_rest(&mut runner);

    assert_eq!(
        flips(&events),
        vec![(P0, W), (P0, W), (P0, L)],
        "reach guard: the run looped through two wins to the loss"
    );
    assert_eq!(hand_size(&runner, P0) - hand_before, 2, "one draw per win");
    assert_eq!(hand_size(&runner, P1), 0, "P1 drew nothing");
}

/// Z2. Derived: an immediate loss is one `CoinFlipped{won:false}` and no win triggers, so
/// the hand is unchanged (CR 705.2: nobody wins a lost flip). PRESERVATION PIN.
#[test]
fn zndrsplt_immediate_loss_draws_nothing() {
    let mut scenario = two_player_table();
    add_zndrsplt(&mut scenario, P0);
    let mut runner = to_beginning_of_combat(scenario);
    let hand_before = hand_size(&runner, P0);

    pin(&mut runner, &[L]);
    let events = drive_to_rest(&mut runner);

    assert_eq!(flips(&events), vec![(P0, L)], "reach guard: one lost flip");
    assert_eq!(hand_size(&runner, P0), hand_before, "hand +0");
}

/// O1. Derived: Okaun's printed 3/3 doubles once per won flip: two wins give 6/6 then
/// 12/12 (CR 701.10b + 2018-06-08 doubling ruling; CR 603.2c one trigger per win). PRESERVATION PIN.
#[test]
fn okaun_doubles_once_per_won_flip() {
    let mut scenario = two_player_table();
    let okaun = add_okaun(&mut scenario, P0);
    let mut runner = to_beginning_of_combat(scenario);

    pin(&mut runner, &[W, W, L]);
    let events = drive_to_rest(&mut runner);

    assert_eq!(
        flips(&events),
        vec![(P0, W), (P0, W), (P0, L)],
        "reach guard: two wins observed"
    );
    runner.state_mut().layers_dirty.mark_full();
    evaluate_layers(runner.state_mut());
    let object = &runner.state().objects[&okaun];
    assert_eq!(
        (object.power, object.toughness),
        (Some(12), Some(12)),
        "3/3 -> 6/6 -> 12/12"
    );
}

/// T1. Derived: Toothy's "whenever you win" counts its controller's wins only, so W,W,L gives
/// two +1/+1 counters (CR 705.2, CR 603.2c). PRESERVATION PIN.
#[test]
fn toothy_gets_a_counter_per_won_flip() {
    let mut scenario = two_player_table();
    let toothy = add_toothy(&mut scenario, P0);
    let mut runner = to_beginning_of_combat(scenario);

    pin(&mut runner, &[W, W, L]);
    let events = drive_to_rest(&mut runner);

    assert_eq!(
        flips(&events),
        vec![(P0, W), (P0, W), (P0, L)],
        "reach guard: three flips for P0"
    );
    assert_eq!(plus_counters(&runner, toothy), 2);
}

/// A1. Derived: P0's run is the only run (P1's combat triggers need P1's turn). "A player"
/// triggers fire for P0's wins whoever controls the source, "you" only for the controller's:
/// P0's Zndrsplt draws P0 two cards, P1's Zndrsplt draws P1 two cards, and P1's Toothy gets
/// none (CR 705.2: only the flipper wins). The non-active player's triggers go on the stack
/// last, so a P1 trigger is on top (CR 603.3b). PRESERVATION PIN.
#[test]
fn a_player_triggers_fire_for_the_flipper_but_you_triggers_do_not() {
    let mut scenario = two_player_table();
    add_zndrsplt(&mut scenario, P0);
    let p1_zndrsplt = add_zndrsplt(&mut scenario, P1);
    let p1_toothy = add_toothy(&mut scenario, P1);
    let mut runner = to_beginning_of_combat(scenario);
    assert_eq!(
        runner.state().stack.len(),
        1,
        "reach guard: only the active player's combat trigger is on the stack"
    );

    let combat_id = runner.state().stack.back().expect("combat trigger").id;

    pin(&mut runner, &[W, W, L]);
    // Resolve the combat trigger; stop as soon as the won-flip triggers have been placed.
    let events = resolve_combat_trigger(&mut runner, combat_id);
    assert_eq!(
        flips(&events),
        vec![(P0, W), (P0, W), (P0, L)],
        "reach guard: the run completed"
    );
    assert_eq!(runner.state().stack.len(), 4, "two triggers per Zndrsplt");
    let top = runner.state().stack.back().expect("a trigger on top");
    assert_eq!(
        top.controller, P1,
        "CR 603.3b: the NAP's triggers go on last"
    );
    assert_eq!(top.source_id, p1_zndrsplt);

    drive_to_rest(&mut runner);
    assert_eq!(hand_size(&runner, P0), 2, "P0's Zndrsplt: two draws");
    assert_eq!(hand_size(&runner, P1), 2, "P1's Zndrsplt: 'a player' fires");
    assert_eq!(
        plus_counters(&runner, p1_toothy),
        0,
        "'you win' is the controller's wins only; P0 flipped"
    );
}

/// SL1. Derived (CR 113.7a): a triggered ability on the stack is independent of its source,
/// so Zndrsplt's combat trigger still runs after Zndrsplt leaves: the run flips W,L. The
/// draw trigger's source is gone, so nothing is drawn. PRESERVATION PIN.
#[test]
fn the_run_resolves_after_its_source_left_the_battlefield() {
    let mut scenario = two_player_table();
    let zndrsplt = add_zndrsplt(&mut scenario, P0);
    let mut runner = to_beginning_of_combat(scenario);
    assert_eq!(
        runner.state().stack.len(),
        1,
        "reach guard: the trigger is on the stack"
    );
    let hand_before = hand_size(&runner, P0);

    let mut leave_events = Vec::new();
    move_to_zone(
        runner.state_mut(),
        zndrsplt,
        Zone::Graveyard,
        &mut leave_events,
    );
    assert_eq!(runner.state().objects[&zndrsplt].zone, Zone::Graveyard);
    assert_eq!(
        runner.state().stack.len(),
        1,
        "reach guard: the trigger stayed on the stack"
    );

    pin(&mut runner, &[W, L]);
    let events = drive_to_rest(&mut runner);

    assert_eq!(
        flips(&events),
        vec![(P0, W), (P0, L)],
        "the run still flips"
    );
    assert_eq!(
        hand_size(&runner, P0),
        hand_before,
        "the departed source draws nothing"
    );
}
