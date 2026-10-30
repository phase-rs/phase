//! "Flip a coin until you lose a flip." as a complete instruction (CR 705.1 + CR 705.2),
//! and the per-win triggers it feeds (CR 603.2c, CR 603.3, CR 603.3b).
//!
//! Zndrsplt, Eye of Wisdom; Okaun, Eye of Chaos; and Toothy and Zndrsplt all carry
//! "At the beginning of combat on your turn, flip a coin until you lose a flip." with no
//! reward clause. Before the earlier parser fix, the parser lowered every until-loss run with an
//! `Effect::unimplemented("flip_coin_until_lose_stub", ..)` as its `win_effect`, which only
//! a following clause, absorbed by `consolidate_die_and_coin_defs`, could replace, so all
//! three cards were marked unsupported.
//!
//! What this file proves, and what it does not:
//!
//! * HISTORICAL PARSER REGRESSIONS: `three_faces_parse_the_combat_run_complete`,
//!   `three_faces_are_supported` and `combat_trigger_on_the_stack_is_a_complete_until_loss_run`
//!   reject the old parser's unsupported stub. The no-Thumb runtime already looped
//!   through the first loss and paid one trigger per won flip.
//! * THUMB REGRESSIONS (fail on revert): won-flip observers across repeated keep prompts
//!   (Zndrsplt, Okaun) and a zone-change observer when the kept win's flip run completes
//!   and the parent continuation pauses on a draw replacement (Strongbox + dredge).
//! * LOGICAL-OWNER REGRESSION: Joint Exploration's resumed scry passes an Enduring
//!   Renewal creature move before pausing on optional land placement. Konrad observes
//!   that move once, using the queued context as the collection witness (no ledger claim).
//! * PRESERVATION PINS: the copy tests (copy-owned SpellCopied observers are not re-parked
//!   while CopyRetarget is pending), Strongbox without a draw replacement, no-Thumb runs,
//!   an opponent's Thumb, and the final-action collector's single-kept-flip path.
//!
//! Every card is built from its verbatim Oracle text (MTGJSON `AtomicCards.json`). RNG
//! outcomes are pinned after the trigger is on the stack, using the `seed_for` + pin
//! pattern of `choose_flip_context_coverage.rs`; each coin runtime test asserts the observed
//! `CoinFlipped` sequence as its reach guard.

use engine::game::coverage::card_face_has_unimplemented_parts;
use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::zones::move_to_zone;
use engine::parser::parse_oracle_text;
use engine::types::ability::{CoinFlipResult, CoinFlipRun, Effect, TargetFilter, TargetRef};
use engine::types::actions::GameAction;
use engine::types::card::CardFace;
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;
use engine::types::game_state::{CastPaymentMode, GameRng, RandomDraw, StackEntryKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::triggers::TriggerMode;
use engine::types::zones::Zone;
use rand::Rng;

const ZNDRSPLT: &str = "Partner with Okaun, Eye of Chaos (When this creature enters, target player may put Okaun into their hand from their library, then shuffle.)\nAt the beginning of combat on your turn, flip a coin until you lose a flip.\nWhenever a player wins a coin flip, draw a card.";
const OKAUN: &str = "Partner with Zndrsplt, Eye of Wisdom (When this creature enters, target player may put Zndrsplt into their hand from their library, then shuffle.)\nAt the beginning of combat on your turn, flip a coin until you lose a flip.\nWhenever a player wins a coin flip, double Okaun's power and toughness until end of turn.";
const TOOTHY: &str = "Whenever you win a coin flip, put a +1/+1 counter on Toothy and Zndrsplt.\nAt the beginning of combat on your turn, flip a coin until you lose a flip.\nWhen Toothy and Zndrsplt leaves the battlefield, draw a card for each +1/+1 counter on Toothy and Zndrsplt.";
const KRARK: &str = "If you would flip a coin, instead flip two coins and ignore one.";

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

fn order_identity(runner: &mut GameRunner, trigger_count: usize) -> Vec<GameEvent> {
    runner
        .act(GameAction::OrderTriggers {
            order: (0..trigger_count).collect(),
        })
        .expect("identity trigger order is accepted")
        .events
}

/// Pass priority until the stack is empty and the table is at rest, draining any
/// `OrderTriggers` prompt with the identity order (the triggers in these tests that share a
/// controller are indistinguishable). Returns every event produced on the way.
fn drive_to_rest(runner: &mut GameRunner) -> Vec<GameEvent> {
    let mut events = Vec::new();
    for _ in 0..60 {
        match runner.state().waiting_for.clone() {
            WaitingFor::OrderTriggers { triggers, .. } => {
                events.extend(order_identity(runner, triggers.len()));
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
                events.extend(order_identity(runner, triggers.len()));
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

/// CR 603.2c + CR 603.3: each kept win triggers once; those abilities reach the stack
/// after the run completes.
#[test]
fn krarks_thumb_run_triggers_once_per_kept_win_after_the_run() {
    let mut scenario = two_player_table();
    let zndrsplt = add_zndrsplt(&mut scenario, P0);
    scenario.add_artifact_from_oracle(P0, "Krark's Thumb", KRARK);
    let mut runner = to_beginning_of_combat(scenario);
    let before = hand_size(&runner, P0);
    let combat_id = runner.state().stack.back().expect("combat trigger").id;
    pin(&mut runner, &[W, L, W, L, L, W]);
    let mut events = resolve_combat_trigger(&mut runner, combat_id);
    let mut prompts = 0;
    for want in [W, W, L] {
        assert!(
            matches!(
                runner.state().waiting_for,
                WaitingFor::CoinFlipKeepChoice { .. }
            ),
            "required Thumb keep prompt"
        );
        prompts += 1;
        assert_eq!(
            flips(&events).len(),
            prompts - 1,
            "only answered flips emit events"
        );
        // CR 603.3: the reached prompt is still inside the combat resolution.
        assert_eq!(hand_size(&runner, P0), before);
        assert!(!runner.state().stack.iter().any(|entry| {
            entry.source_id == zndrsplt
                && matches!(
                    entry.ability().map(|a| &a.effect),
                    Some(Effect::Draw { .. })
                )
        }));
        events.extend(keep_mixed_coin(&mut runner, want));
    }
    events.extend(drain_trigger_ordering(&mut runner));
    let draw_triggers = runner
        .state()
        .stack
        .iter()
        .filter(|entry| {
            entry.source_id == zndrsplt
                && matches!(
                    entry.ability().map(|a| &a.effect),
                    Some(Effect::Draw { .. })
                )
        })
        .count();
    events.extend(drive_to_rest(&mut runner));
    assert_eq!(prompts, 3);
    assert_eq!(flips(&events), vec![(P0, W), (P0, W), (P0, L)]);
    let draws = hand_size(&runner, P0) - before;
    assert_eq!(draws, 2, "CR 603.2c: one draw per surviving won flip");
    assert_eq!(draw_triggers, 2);
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    assert!(runner.state().stack.is_empty());
    assert!(runner.state().resolution_stack.is_empty());
}

/// CR 707.10 + CR 603.2c: a copy is not cast; Magecraft sees cast + copy once each.
#[test]
fn krarks_thumb_copy_retarget_draws_once_for_cast_and_once_for_copy() {
    thumb_copy_magecraft(true);
}

#[test]
fn krarks_thumb_copy_keeps_targets_without_duplicate_magecraft() {
    thumb_copy_magecraft(false);
}

fn thumb_copy_magecraft(retarget: bool) {
    let mut scenario = two_player_table();
    scenario.add_creature(P0, "Krark, the Thumbless", 2, 2)
        .from_oracle_text_with_keywords(&["Partner"], "Whenever you cast an instant or sorcery spell, flip a coin. If you lose the flip, return that spell to its owner's hand. If you win the flip, copy that spell, and you may choose new targets for the copy.\nPartner (You can have two commanders if both have partner.)");
    scenario.add_artifact_from_oracle(P0, "Krark's Thumb", KRARK);
    scenario.add_creature_from_oracle(
        P0,
        "Archmage Emeritus",
        2,
        2,
        "Magecraft — Whenever you cast or copy an instant or sorcery spell, draw a card.",
    );
    let a = scenario.add_creature(P1, "Creature A", 2, 2).id();
    let b = scenario.add_creature(P1, "Creature B", 2, 2).id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Unsummon",
            true,
            "Return target creature to its owner's hand.",
        )
        .with_mana_cost(ManaCost::Cost {
            generic: 0,
            shards: vec![ManaCostShard::Blue],
        })
        .id();
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(ManaType::Blue, ObjectId(0), false, vec![])],
    );
    let mut runner = scenario.build();
    // The cast driver cannot stop on cast-trigger ordering; announce publicly.
    let card_id = runner.state().objects[&spell].card_id;
    let mut events = runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("announce Unsummon")
        .events;
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::TargetSelection { .. }
    ));
    events.extend(
        runner
            .act(GameAction::ChooseTarget {
                target: Some(TargetRef::Object(a)),
            })
            .expect("target creature A")
            .events,
    );
    if matches!(runner.state().waiting_for, WaitingFor::ManaPayment { .. }) {
        events.extend(
            runner
                .act(GameAction::PassPriority)
                .expect("pay Unsummon's mana")
                .events,
        );
    }
    assert_eq!(
        runner.state().objects[&spell].zone,
        Zone::Stack,
        "cast committed"
    );
    let before = hand_size(&runner, P0);
    pin(&mut runner, &[W, L]);
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. } | WaitingFor::OrderTriggers { .. }
    ));
    for _ in 0..20 {
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } => {
                assert!(
                    !runner.state().stack.is_empty(),
                    "cast trigger must reach a keep prompt"
                );
                events.extend(
                    runner
                        .act(GameAction::PassPriority)
                        .expect("resolve cast triggers")
                        .events,
                );
            }
            WaitingFor::OrderTriggers { triggers, .. } => {
                events.extend(order_identity(&mut runner, triggers.len()));
            }
            _ => break,
        }
    }
    events.extend(keep_mixed_coin(&mut runner, W));
    let WaitingFor::CopyRetarget { copy_id, .. } = runner.state().waiting_for.clone() else {
        panic!(
            "required copy retarget prompt: {:?}",
            runner.state().waiting_for
        );
    };
    assert_ne!(copy_id, spell, "the copy has a distinct stack identity");
    let action = if retarget {
        GameAction::ChooseTarget {
            target: Some(TargetRef::Object(b)),
        }
    } else {
        GameAction::KeepAllCopyTargets
    };
    events.extend(runner.act(action).expect("answer CopyRetarget").events);
    events.extend(drive_to_rest(&mut runner));
    let copies = events
        .iter()
        .filter(|event| matches!(event, GameEvent::SpellCopied { .. }))
        .count();
    let draws = hand_size(&runner, P0) - before;
    assert_eq!(
        flips(&events),
        vec![(P0, W)],
        "the copy must not trigger another Krark flip"
    );
    assert_eq!(copies, 1);
    assert!(events.iter().any(|event| matches!(
        event, GameEvent::SpellCopied { original_id, .. } if *original_id == spell
    )));
    let cast_ids: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            GameEvent::SpellCast { object_id, .. } => Some(*object_id),
            _ => None,
        })
        .collect();
    assert_eq!(cast_ids, vec![spell], "CR 707.10: the copy is not cast");
    assert_eq!(runner.state().objects[&a].zone, Zone::Hand);
    assert_eq!(
        runner.state().objects[&b].zone,
        if retarget {
            Zone::Hand
        } else {
            Zone::Battlefield
        }
    );
    assert_eq!(draws, 2, "Magecraft observes exactly the cast and the copy");
}

fn keep_mixed_coin(runner: &mut GameRunner, want: bool) -> Vec<GameEvent> {
    let WaitingFor::CoinFlipKeepChoice {
        player,
        results,
        keep_count,
    } = runner.state().waiting_for.clone()
    else {
        panic!("required Thumb prompt: {:?}", runner.state().waiting_for);
    };
    assert_eq!(player, P0);
    assert_eq!(keep_count, 1);
    assert_eq!(results.len(), 2);
    assert_ne!(results[0], results[1], "reach guard: mixed pair");
    let events = runner
        .act(GameAction::SelectCoinFlips {
            keep_indices: vec![results.iter().position(|&won| won == want).unwrap()],
        })
        .expect("keep the requested coin")
        .events;
    assert_eq!(flips(&events), vec![(P0, want)]);
    events
}

fn drain_trigger_ordering(runner: &mut GameRunner) -> Vec<GameEvent> {
    let mut events = Vec::new();
    while let WaitingFor::OrderTriggers { triggers, .. } = runner.state().waiting_for.clone() {
        events.extend(order_identity(runner, triggers.len()));
    }
    events
}

/// CR 603.2c + CR 701.10b: each kept win doubles the current P/T at resolution.
#[test]
fn krarks_thumb_okaun_doubles_separately_after_the_run() {
    let mut scenario = two_player_table();
    let okaun = add_okaun(&mut scenario, P0);
    scenario.add_artifact_from_oracle(P0, "Krark's Thumb", KRARK);
    let mut runner = to_beginning_of_combat(scenario);
    let combat_id = runner.state().stack.back().expect("combat trigger").id;
    pin(&mut runner, &[W, L, W, L, L, W]);
    let mut events = resolve_combat_trigger(&mut runner, combat_id);
    for want in [W, W, L] {
        events.extend(keep_mixed_coin(&mut runner, want));
        let object = &runner.state().objects[&okaun];
        assert_eq!(
            (object.power, object.toughness),
            (Some(3), Some(3)),
            "CR 603.3: doubling waits until the run completes"
        );
    }
    events.extend(drain_trigger_ordering(&mut runner));
    assert_eq!(flips(&events), vec![(P0, W), (P0, W), (P0, L)]);
    assert_eq!(runner.state().stack.len(), 2, "one doubling per kept win");
    assert!(runner
        .state()
        .stack
        .iter()
        .all(|entry| entry.source_id == okaun));
    for expected in [6, 12] {
        let trigger_id = runner.state().stack.back().expect("doubling trigger").id;
        events.extend(resolve_combat_trigger(&mut runner, trigger_id));
        runner.state_mut().layers_dirty.mark_full();
        evaluate_layers(runner.state_mut());
        let object = &runner.state().objects[&okaun];
        assert_eq!(
            (object.power, object.toughness),
            (Some(expected), Some(expected))
        );
    }
    events.extend(drive_to_rest(&mut runner));
    assert_eq!(flips(&events), vec![(P0, W), (P0, W), (P0, L)]);
}

/// CR 603.2c + CR 614.1a: the final-action collector sees only the kept result.
#[test]
fn krarks_thumb_single_flip_final_collector_fires_once() {
    for want in [W, L] {
        let mut scenario = two_player_table();
        add_zndrsplt(&mut scenario, P0);
        scenario.add_artifact_from_oracle(P0, "Krark's Thumb", KRARK);
        let spell = scenario
            .add_spell_to_hand_from_oracle(
                P0,
                "Stitch in Time",
                false,
                "Flip a coin. If you win the flip, take an extra turn after this one.",
            )
            .with_mana_cost(ManaCost::Cost {
                generic: 1,
                shards: vec![ManaCostShard::Blue, ManaCostShard::Red],
            })
            .id();
        scenario.with_mana_pool(
            P0,
            [ManaType::Colorless, ManaType::Blue, ManaType::Red]
                .into_iter()
                .map(|kind| ManaUnit::new(kind, ObjectId(0), false, vec![]))
                .collect(),
        );
        let mut runner = scenario.build();
        runner.cast(spell).commit();
        let before = hand_size(&runner, P0);
        pin(&mut runner, &[W, L]);
        let mut events = resolve_combat_trigger(&mut runner, spell);
        events.extend(keep_mixed_coin(&mut runner, want));
        events.extend(drive_to_rest(&mut runner));
        assert_eq!(flips(&events), vec![(P0, want)]);
        assert_eq!(hand_size(&runner, P0) - before, usize::from(want));
        assert_eq!(
            runner.state().phase,
            Phase::PreCombatMain,
            "do not advance to the extra turn"
        );
    }
}

/// CR 109.5 + CR 614.1a: an opponent's Thumb ("If you would flip") does not replace
/// the active player's flips.
#[test]
fn opponents_thumb_preserves_unprompted_until_loss_run() {
    let mut scenario = two_player_table();
    add_zndrsplt(&mut scenario, P0);
    let thumb = scenario
        .add_artifact_from_oracle(P1, "Krark's Thumb", KRARK)
        .id();
    let mut runner = to_beginning_of_combat(scenario);
    assert_eq!(runner.state().objects[&thumb].zone, Zone::Battlefield);
    assert_eq!(runner.state().objects[&thumb].controller, P1);
    let before = hand_size(&runner, P0);
    pin(&mut runner, &[W, W, L]);
    let events = drive_to_rest(&mut runner);
    assert_eq!(flips(&events), vec![(P0, W), (P0, W), (P0, L)]);
    assert_eq!(hand_size(&runner, P0) - before, 2);
}

/// CR 603.2c + CR 603.3: a sacrifice observer waits through a later draw choice.
#[test]
fn krarks_thumb_strongbox_preserves_zone_observer_through_dredge() {
    strongbox_zone_observer(true);
}

#[test]
fn krarks_thumb_strongbox_preserves_zone_observer_without_draw_replacement() {
    strongbox_zone_observer(false);
}

fn strongbox_zone_observer(dredge: bool) {
    let mut scenario = two_player_table();
    let strongbox = scenario
        .add_artifact_from_oracle(P0, "Sorcerer's Strongbox",
            "{2}, {T}: Flip a coin. If you win the flip, sacrifice this artifact and draw three cards.")
        .id();
    scenario.add_artifact_from_oracle(P0, "Krark's Thumb", KRARK);
    scenario.add_creature(P0, "Marionette Apprentice", 1, 2)
        .from_oracle_text_with_keywords(&["Fabricate"], "Fabricate 1 (When this creature enters, put a +1/+1 counter on it or create a 1/1 colorless Servo artifact creature token.)\nWhenever another creature or artifact you control is put into a graveyard from the battlefield, each opponent loses 1 life.");
    if dredge {
        scenario.add_creature_to_graveyard(P0, "Stinkweed Imp", 1, 2)
            .from_oracle_text_with_keywords(&["Flying", "Dredge"], "Flying\nWhenever this creature deals combat damage to a creature, destroy that creature.\nDredge 5 (If you would draw a card, you may mill five cards instead. If you do, return this card from your graveyard to your hand.)");
    }
    scenario.with_mana_pool(
        P0,
        (0..2)
            .map(|_| ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]))
            .collect(),
    );
    let mut runner = scenario.build();
    let life_before = runner.state().players[1].life;
    let hand_before = hand_size(&runner, P0);
    runner
        .act(GameAction::ActivateAbility {
            source_id: strongbox,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("activate printed Strongbox");
    if matches!(runner.state().waiting_for, WaitingFor::ManaPayment { .. }) {
        runner
            .act(GameAction::PassPriority)
            .expect("pay Strongbox activation");
    }
    let activation = runner
        .state()
        .stack
        .back()
        .expect("Strongbox activation on stack")
        .id;
    pin(&mut runner, &[W, L]);
    let mut events = resolve_combat_trigger(&mut runner, activation);
    events.extend(keep_mixed_coin(&mut runner, W));
    assert_eq!(runner.state().objects[&strongbox].zone, Zone::Graveyard);
    assert!(events.iter().any(|event| matches!(
        event,
        GameEvent::ZoneChanged { object_id, from: Some(Zone::Battlefield), to: Zone::Graveyard, .. }
            if *object_id == strongbox
    )), "the kept win sacrificed Strongbox");
    if dredge {
        let WaitingFor::ReplacementChoice {
            player, candidates, ..
        } = runner.state().waiting_for.clone()
        else {
            panic!(
                "required dredge prompt after sacrifice: {:?}",
                runner.state().waiting_for
            );
        };
        assert_eq!(player, P0);
        assert!(candidates
            .iter()
            .any(|candidate| candidate.source_name == "Stinkweed Imp"));
        for _ in 0..3 {
            if !matches!(
                runner.state().waiting_for,
                WaitingFor::ReplacementChoice { .. }
            ) {
                break;
            }
            events.extend(
                runner
                    .act(GameAction::ChooseReplacement { index: 1 })
                    .expect("decline dredge and draw the card")
                    .events,
            );
        }
    }
    events.extend(drive_to_rest(&mut runner));
    assert_eq!(flips(&events), vec![(P0, W)]);
    assert_eq!(
        hand_size(&runner, P0) - hand_before,
        3,
        "the parent draw completed"
    );
    assert_eq!(
        life_before - runner.state().players[1].life,
        1,
        "CR 603.2c: Strongbox's sacrifice triggers exactly one opponent life loss"
    );
}

/// CR 603.2c + CR 608.2c: a logical zone owner queues Konrad during the resumed
/// scry slice, before Joint Exploration pauses again on its kicked instruction.
/// The owner does not claim the consumed ledger, so parking must honor the
/// queued zone-change witness rather than collect a second copy.
#[test]
fn scry_continuation_deduplicates_logically_owned_zone_observer_before_later_prompt() {
    let mut scenario = two_player_table();
    scenario.add_enchantment_from_oracle(P0, "Enduring Renewal",
        "Play with your hand revealed.\nIf you would draw a card, reveal the top card of your library instead. If it's a creature card, put it into your graveyard. Otherwise, draw a card.\nWhenever a creature is put into your graveyard from the battlefield, return it to your hand.");
    let observer = scenario.add_creature_from_oracle(P0, "Syr Konrad, the Grim", 5, 4,
        "Whenever another creature dies, or a creature card is put into a graveyard from anywhere other than the battlefield, or a creature card leaves your graveyard, Syr Konrad deals 1 damage to each opponent.\n{1}{B}: Each player mills a card. (They each put the top card of their library into their graveyard.)").id();
    let creature = scenario
        .add_creature_to_hand(P0, "Grizzly Bears", 2, 2)
        .id();
    let land = scenario.add_land_to_hand(P0, "Forest").id();
    let spell = scenario.add_spell_to_hand(P0, "Joint Exploration", true)
        .from_oracle_text_with_keywords(&["Kicker"], "Kicker {G} (You may pay an additional {G} as you cast this spell.)\nScry 2, then draw a card. If this spell was kicked, you may put a land card from your hand onto the battlefield.")
        .with_mana_cost(ManaCost::Cost { generic: 1, shards: vec![ManaCostShard::Blue] }).id();
    scenario.with_mana_pool(
        P0,
        [ManaType::Colorless, ManaType::Blue, ManaType::Green]
            .into_iter()
            .map(|kind| ManaUnit::new(kind, ObjectId(0), false, vec![]))
            .collect(),
    );
    let mut runner = scenario.build();
    // Fixture placement only; the observed move is produced by the draw replacement.
    move_to_zone(runner.state_mut(), creature, Zone::Library, &mut Vec::new());
    runner.state_mut().players[0]
        .library
        .retain(|id| *id != creature);
    runner.state_mut().players[0].library.push_front(creature);
    let life_before = runner.state().players[1].life;
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast printed Joint Exploration");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::OptionalCostChoice { .. }
    ));
    runner
        .act(GameAction::DecideOptionalCost { pay: true })
        .expect("pay printed kicker");
    if matches!(runner.state().waiting_for, WaitingFor::ManaPayment { .. }) {
        runner
            .act(GameAction::PassPriority)
            .expect("pay Joint Exploration");
    }
    assert_eq!(runner.state().objects[&spell].zone, Zone::Stack);
    let mut events = resolve_combat_trigger(&mut runner, spell);
    let WaitingFor::ScryChoice { player, cards } = runner.state().waiting_for.clone() else {
        panic!(
            "required Joint Exploration scry: {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(player, P0);
    assert_eq!(cards.len(), 2);
    assert!(cards.contains(&creature));
    events.extend(
        runner
            .act(GameAction::SelectCards {
                cards: vec![creature],
            })
            .expect("keep the creature and bottom the other scry card")
            .events,
    );
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::OptionalEffectChoice { player: P0, source_id, .. } if source_id == spell
        ),
        "required later kicked land-placement prompt: {:?}",
        runner.state().waiting_for
    );
    assert_eq!(runner.state().objects[&land].zone, Zone::Hand);
    assert_eq!(runner.state().objects[&creature].zone, Zone::Graveyard);
    let is_observed_move = |event: &GameEvent| {
        matches!(
            event,
            GameEvent::ZoneChanged { object_id, from: Some(Zone::Library), to: Zone::Graveyard, .. }
                if *object_id == creature
        )
    };
    assert_eq!(
        events
            .iter()
            .filter(|event| is_observed_move(event))
            .count(),
        1,
        "reach guard: the replacement really moved the revealed creature"
    );
    let queued = runner
        .state()
        .deferred_triggers
        .iter()
        .filter(|context| {
            context.pending.source_id == observer
                && context.trigger_events.iter().any(is_observed_move)
        })
        .count();
    assert!(
        queued > 0,
        "reach guard: Konrad observed the real zone move"
    );
    assert_eq!(
        runner.state().consumed_before_priority_trigger_events.len(),
        0,
        "this logical owner supplies a queued witness, not a consumed-ledger claim"
    );
    assert_eq!(
        runner.state().players[1].life,
        life_before,
        "CR 603.3: the queued observer waits for the parent resolution"
    );
    events.extend(
        runner
            .act(GameAction::DecideOptionalEffect { accept: false })
            .expect("decline the later land placement")
            .events,
    );
    events.extend(drain_trigger_ordering(&mut runner));
    let placed = runner
        .state()
        .stack
        .iter()
        .filter(|entry| entry.source_id == observer)
        .count();
    assert!(
        placed > 0,
        "reach guard: the queued observer reached the stack"
    );
    events.extend(drive_to_rest(&mut runner));
    assert_eq!(
        life_before - runner.state().players[1].life,
        1,
        "CR 603.2c: one creature move deals one Konrad damage; queued={queued}, placed={placed}"
    );
    assert_eq!(queued, 1);
    assert_eq!(placed, 1);
    assert_eq!(runner.state().objects[&land].zone, Zone::Hand);
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    assert!(runner.state().stack.is_empty());
    assert!(runner.state().resolution_stack.is_empty());
}
