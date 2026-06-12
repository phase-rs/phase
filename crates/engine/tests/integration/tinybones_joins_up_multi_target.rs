//! Integration test for GitHub issue #426 — Tinybones Joins Up's ETB trigger
//! "any number of target players each discard a card".
//!
//! The ETB trigger parses correctly to `Discard { count: 1, target: Player }`
//! with `multi_target: { min: 0, max: null }` — an all-optional multi-target
//! set over `TargetFilter::Player`. The reported symptom was that the targeting
//! UI never surfaced and the targeted players were skipped.
//!
//! This file is the regression deliverable: it drives the REAL `apply`
//! pipeline in a 3-player game — cast Tinybones Joins Up → resolve → ETB fires
//! through `process_triggers` → `WaitingFor::TriggerTargetSelection` over the 3
//! players → `SelectTargets`/`ChooseTarget` → discards resolve. No synthetic
//! `pending_trigger`, no hand-rolled `WaitingFor`.
//!
//! Pinned `WaitingFor` variant: the triggered-ability target path constructs
//! `WaitingFor::TriggerTargetSelection` (`engine::game::engine::
//! begin_pending_trigger_target_selection`). `MultiTargetSelection` is the
//! spell-cast path and is NOT used here.
//!
//! CR 603.3d: "The remainder of the process for putting a triggered ability on
//! the stack is identical to the process for casting a spell listed in rules
//! 601.2c-d." — the trigger's controller chooses its targets as it goes on the
//! stack.
//! CR 601.2c: "If the spell has a variable number of targets, the player
//! announces how many targets they will choose before they announce those
//! targets." — "any number of target players" permits choosing 0+ players.

use engine::game::scenario::GameScenario;
use engine::types::actions::GameAction;
use engine::types::game_state::CastPaymentMode;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaCost;
use engine::types::player::PlayerId;

/// Tinybones Joins Up's printed Oracle text — byte-identical to
/// `client/public/card-data.json` and MTGJSON `AtomicCards.json`.
const TINYBONES_JOINS_UP: &str = "When Tinybones Joins Up enters, any number of \
     target players each discard a card.\nWhenever a legendary creature you \
     control enters, any number of target players each mill a card and lose 1 life.";

const P0: PlayerId = PlayerId(0);
const P1: PlayerId = PlayerId(1);
const P2: PlayerId = PlayerId(2);

/// Number of cards in a player's hand.
fn hand_count(runner: &engine::game::scenario::GameRunner, player: PlayerId) -> usize {
    runner
        .state()
        .players
        .iter()
        .find(|p| p.id == player)
        .map(|p| p.hand.len())
        .expect("player exists")
}

/// Number of cards in a player's graveyard.
fn graveyard_count(runner: &engine::game::scenario::GameRunner, player: PlayerId) -> usize {
    runner
        .state()
        .players
        .iter()
        .find(|p| p.id == player)
        .map(|p| p.graveyard.len())
        .expect("player exists")
}

/// Build a 3-player game with Tinybones Joins Up in P0's hand (as a 0-cost
/// Legendary Enchantment, so casting needs no mana prompt) and exactly one
/// card in every player's hand for discard.
fn setup_three_player() -> (engine::game::scenario::GameRunner, ObjectId) {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(engine::types::phase::Phase::PreCombatMain);

    // One card in each player's hand — a discard of `count: 1` auto-resolves
    // when the hand holds exactly one card, keeping the test deterministic
    // (no per-player `DiscardChoice` round-trips to drive).
    for &pid in &[P0, P1, P2] {
        scenario.with_cards_in_hand(pid, &["Filler Card"]);
    }

    // Tinybones Joins Up enters P0's hand, parsed from its real Oracle text.
    let tinybones = scenario
        .add_spell_to_hand_from_oracle(P0, "Tinybones Joins Up", false, TINYBONES_JOINS_UP)
        .as_enchantment()
        .with_mana_cost(ManaCost::zero())
        .id();

    (scenario.build(), tinybones)
}

/// Cast Tinybones Joins Up and resolve it onto the battlefield. Returns once
/// the ETB trigger has been processed and the engine is waiting on target
/// selection (or has auto-resolved if no players were targeted).
fn cast_tinybones(runner: &mut engine::game::scenario::GameRunner, tinybones: ObjectId) {
    let card_id = runner
        .state()
        .objects
        .get(&tinybones)
        .expect("Tinybones Joins Up exists")
        .card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: tinybones,
            card_id,
            targets: vec![],

            payment_mode: CastPaymentMode::Auto,
        })
        .expect("casting a 0-cost enchantment should succeed");
    // Drain priority so the enchantment resolves onto the battlefield and its
    // ETB trigger is produced + dispatched through `process_triggers`.
    runner.advance_until_stack_empty();
}

/// CR 603.3d + CR 601.2c: An all-optional multi-target trigger over
/// `TargetFilter::Player` MUST surface an interactive `TriggerTargetSelection`
/// listing every player as a legal target — it must NOT auto-resolve to an
/// empty selection.
#[test]
fn tinybones_etb_surfaces_interactive_player_target_selection() {
    let (mut runner, tinybones) = setup_three_player();
    cast_tinybones(&mut runner, tinybones);

    match &runner.state().waiting_for {
        WaitingFor::TriggerTargetSelection {
            player,
            target_slots,
            selection,
            ..
        } => {
            assert_eq!(
                *player, P0,
                "the trigger's controller (P0) chooses its targets"
            );
            // CR 601.2c: "any number of target players" — one optional slot per
            // legal player, all 3 players legal targets of the first slot.
            assert!(
                !target_slots.is_empty(),
                "an all-optional multi-target trigger must surface target slots, \
                 not auto-resolve to an empty selection"
            );
            assert!(
                target_slots.iter().all(|slot| slot.optional),
                "every slot of an `any number` (min == 0) multi-target set is optional"
            );
            let legal = &selection.current_legal_targets;
            assert_eq!(
                legal.len(),
                3,
                "all 3 players must be legal targets of the multi-target trigger"
            );
            for pid in [P0, P1, P2] {
                assert!(
                    legal.iter().any(|t| {
                        matches!(t, engine::types::ability::TargetRef::Player(p) if *p == pid)
                    }),
                    "{pid:?} must be a legal player target"
                );
            }
        }
        other => {
            panic!("expected TriggerTargetSelection after Tinybones Joins Up's ETB, got {other:?}")
        }
    }
}

/// CR 603.3d: Selecting 2 of the 3 players causes exactly those 2 players to
/// each discard a card; the unselected player discards nothing.
///
/// Regression guard (issue #426): the multi-target-over-`Player` resolution
/// fan-out in `resolve_ability_chain` recurses once per chosen player with
/// `targets` narrowed to that one player, so each targeted player discards.
/// Before the fan-out, `Discard`'s resolver resolved only the first
/// `TargetRef::Player` and a 2-player selection discarded just one player.
#[test]
fn tinybones_etb_targeting_two_of_three_discards_only_those_two() {
    let (mut runner, tinybones) = setup_three_player();
    cast_tinybones(&mut runner, tinybones);

    let hand_before = [
        hand_count(&runner, P0),
        hand_count(&runner, P1),
        hand_count(&runner, P2),
    ];
    assert_eq!(hand_before, [1, 1, 1], "each player starts with one card");

    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::TriggerTargetSelection { .. }
        ),
        "ETB trigger must pause on TriggerTargetSelection"
    );

    // Select P1 and P2 (skip P0). `SelectTargets` fills the multi-target
    // slots; the remaining optional slot is left unfilled.
    runner
        .act(GameAction::SelectTargets {
            targets: vec![
                engine::types::ability::TargetRef::Player(P1),
                engine::types::ability::TargetRef::Player(P2),
            ],
        })
        .expect("selecting two player targets must succeed");

    runner.advance_until_stack_empty();

    // P1 and P2 each discarded their only card; P0 was not targeted.
    assert_eq!(
        hand_count(&runner, P0),
        1,
        "P0 was not targeted and must not discard"
    );
    assert_eq!(
        hand_count(&runner, P1),
        0,
        "P1 was targeted and must discard its card"
    );
    assert_eq!(
        hand_count(&runner, P2),
        0,
        "P2 was targeted and must discard its card"
    );
    assert_eq!(
        graveyard_count(&runner, P1),
        1,
        "P1's discarded card moves to its graveyard"
    );
    assert_eq!(
        graveyard_count(&runner, P2),
        1,
        "P2's discarded card moves to its graveyard"
    );
    assert_eq!(
        graveyard_count(&runner, P0),
        0,
        "P0 discarded nothing — its graveyard stays empty"
    );
}

/// CR 601.2c: "any number" includes zero — choosing no players is legal and
/// resolution must complete with no discards and no error.
///
/// Regression guard (issue #426): the multi-target-over-`Player` fan-out
/// treats a zero-chosen-player `TargetFilter::Player` effect as a no-op (emit
/// `EffectResolved`, never reach `resolve_effect`). Before the fan-out,
/// `resolve_player_for_context_ref` fell back to `ability.controller` and the
/// trigger's controller wrongly discarded a card.
#[test]
fn tinybones_etb_targeting_zero_players_discards_nothing() {
    let (mut runner, tinybones) = setup_three_player();
    cast_tinybones(&mut runner, tinybones);

    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::TriggerTargetSelection { .. }
        ),
        "ETB trigger must pause on TriggerTargetSelection"
    );

    // Choose zero targets — every slot is optional, so an empty selection is
    // legal per "any number of target players".
    runner
        .act(GameAction::SelectTargets { targets: vec![] })
        .expect("selecting zero player targets must be legal for a min-0 multi-target");

    runner.advance_until_stack_empty();

    for pid in [P0, P1, P2] {
        assert_eq!(
            hand_count(&runner, pid),
            1,
            "{pid:?} kept its card — no players were targeted"
        );
        assert_eq!(
            graveyard_count(&runner, pid),
            0,
            "{pid:?} discarded nothing"
        );
    }
    assert!(
        !matches!(
            runner.state().waiting_for,
            WaitingFor::TriggerTargetSelection { .. }
        ),
        "resolution must complete after a zero-target selection"
    );
}

/// CR 608.2c: When a targeted player holds more than one card, `Discard`
/// surfaces an interactive `WaitingFor::DiscardChoice`. With two targeted
/// players each holding 2 cards, the fan-out resolves the first player, pauses
/// on their `DiscardChoice`, stashes the second player as a continuation, and
/// resumes after the choice — exercising the fan-out's pause →
/// `append_to_pending_continuation` → `drain_pending_continuation` → resume
/// cycle that the one-card-per-hand tests skip.
#[test]
fn tinybones_etb_targeting_two_players_with_multi_card_hands_each_discards_one() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(engine::types::phase::Phase::PreCombatMain);

    // P1 and P2 each hold 2 cards — a `count: 1` discard cannot auto-resolve,
    // forcing an interactive `DiscardChoice` per targeted player. P0 holds 1.
    scenario.with_cards_in_hand(P0, &["P0 Card"]);
    scenario.with_cards_in_hand(P1, &["P1 Card A", "P1 Card B"]);
    scenario.with_cards_in_hand(P2, &["P2 Card A", "P2 Card B"]);

    let tinybones = scenario
        .add_spell_to_hand_from_oracle(P0, "Tinybones Joins Up", false, TINYBONES_JOINS_UP)
        .as_enchantment()
        .with_mana_cost(ManaCost::zero())
        .id();

    let mut runner = scenario.build();
    cast_tinybones(&mut runner, tinybones);

    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::TriggerTargetSelection { .. }
        ),
        "ETB trigger must pause on TriggerTargetSelection"
    );

    // Target P1 and P2 (skip P0).
    runner
        .act(GameAction::SelectTargets {
            targets: vec![
                engine::types::ability::TargetRef::Player(P1),
                engine::types::ability::TargetRef::Player(P2),
            ],
        })
        .expect("selecting two player targets must succeed");

    // The fan-out resolves P1 first; with 2 cards in hand `Discard` pauses on
    // an interactive `DiscardChoice`. Drive each player's choice in turn,
    // capping iterations so a stuck `WaitingFor` fails loudly rather than
    // looping forever.
    let mut discard_choices_handled = 0;
    for _ in 0..50 {
        match &runner.state().waiting_for {
            WaitingFor::DiscardChoice {
                player,
                count,
                cards,
                ..
            } => {
                assert_eq!(
                    *count, 1,
                    "Tinybones' ETB discards exactly one card per player"
                );
                // The choosing player must be one of the two targeted players.
                assert!(
                    *player == P1 || *player == P2,
                    "only targeted players P1/P2 may face a DiscardChoice, got {player:?}"
                );
                let chosen: Vec<ObjectId> = cards.iter().take(*count).copied().collect();
                runner
                    .act(GameAction::SelectCards { cards: chosen })
                    .expect("resolving the discard choice should succeed");
                discard_choices_handled += 1;
            }
            WaitingFor::Priority { .. } => runner.advance_until_stack_empty(),
            _ => break,
        }
    }

    assert_eq!(
        discard_choices_handled, 2,
        "each of the two targeted players (P1, P2) must face exactly one \
         interactive DiscardChoice — the fan-out's pause/continuation cycle"
    );

    // Each targeted player discarded exactly one of their two cards.
    assert_eq!(
        hand_count(&runner, P1),
        1,
        "P1 was targeted and discards exactly one of its two cards"
    );
    assert_eq!(
        hand_count(&runner, P2),
        1,
        "P2 was targeted and discards exactly one of its two cards"
    );
    assert_eq!(
        graveyard_count(&runner, P1),
        1,
        "P1's discarded card moves to its graveyard"
    );
    assert_eq!(
        graveyard_count(&runner, P2),
        1,
        "P2's discarded card moves to its graveyard"
    );
    // P0 was not targeted — untouched.
    assert_eq!(
        hand_count(&runner, P0),
        1,
        "P0 was not targeted and keeps its card"
    );
    assert_eq!(
        graveyard_count(&runner, P0),
        0,
        "P0 discarded nothing — its graveyard stays empty"
    );
    // Resolution completed cleanly — no stuck choice state.
    assert!(
        !matches!(
            runner.state().waiting_for,
            WaitingFor::DiscardChoice { .. } | WaitingFor::TriggerTargetSelection { .. }
        ),
        "resolution must complete after both players' discard choices"
    );
}

/// Cards in a player's library.
fn library_count(runner: &engine::game::scenario::GameRunner, player: PlayerId) -> usize {
    runner
        .state()
        .players
        .iter()
        .find(|p| p.id == player)
        .map(|p| p.library.len())
        .expect("player exists")
}

/// A player's current life total.
fn life_total(runner: &engine::game::scenario::GameRunner, player: PlayerId) -> i32 {
    runner
        .state()
        .players
        .iter()
        .find(|p| p.id == player)
        .map(|p| p.life)
        .expect("player exists")
}

/// CR 101.4 + CR 608.2c: The same multi-target-over-`Player` fan-out covers
/// Tinybones Joins Up's SECOND trigger — "any number of target players each
/// mill a card and lose 1 life" — which parses to `Mill { target: Player }`
/// with a `LoseLife` sub-ability. The fan-out dispatches purely on the
/// effect's `TargetFilter::Player`, so `Mill` fans out identically to
/// `Discard`, and the per-player narrowed clone keeps `sub_ability` intact so
/// each targeted player mills then loses 1 life.
#[test]
fn tinybones_second_trigger_mill_and_lose_life_fans_out_per_player() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(engine::types::phase::Phase::PreCombatMain);

    // Every player has a library to mill from and a known life total.
    for &pid in &[P0, P1, P2] {
        scenario.with_library_top(pid, &["Library Card"]);
        scenario.with_life(pid, 20);
    }

    // Tinybones Joins Up already on P0's battlefield so its second trigger
    // ("Whenever a legendary creature you control enters") is live.
    scenario
        .add_spell_to_hand_from_oracle(P0, "Tinybones Joins Up", false, TINYBONES_JOINS_UP)
        .as_enchantment()
        .with_mana_cost(ManaCost::zero());

    // A legendary creature P0 controls, cast to trigger Tinybones' 2nd ability.
    let legend = scenario
        .add_creature_to_hand(P0, "Legendary Bear", 2, 2)
        .as_legendary()
        .with_mana_cost(ManaCost::zero())
        .id();

    let mut runner = scenario.build();

    // Resolve Tinybones onto the battlefield first.
    let tinybones_id = runner
        .state()
        .players
        .iter()
        .find(|p| p.id == P0)
        .and_then(|p| p.hand.iter().next().copied())
        .expect("Tinybones Joins Up in P0's hand");
    cast_tinybones(&mut runner, tinybones_id);
    // Tinybones' own ETB (trigger 1) is min-0 multi-target — choose zero so it
    // resolves to a no-op and we cleanly reach the legendary-creature cast.
    if matches!(
        runner.state().waiting_for,
        WaitingFor::TriggerTargetSelection { .. }
    ) {
        runner
            .act(GameAction::SelectTargets { targets: vec![] })
            .expect("zero-target selection for Tinybones' own ETB");
        runner.advance_until_stack_empty();
    }

    // Cast the legendary creature — its ETB fires Tinybones' 2nd trigger.
    let legend_card = runner
        .state()
        .objects
        .get(&legend)
        .expect("legendary creature exists")
        .card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: legend,
            card_id: legend_card,
            targets: vec![],

            payment_mode: CastPaymentMode::Auto,
        })
        .expect("casting a 0-cost legendary creature should succeed");
    runner.advance_until_stack_empty();

    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::TriggerTargetSelection { .. }
        ),
        "Tinybones' legendary-ETB trigger must pause on TriggerTargetSelection"
    );

    // Target P1 and P2 — each should mill a card and lose 1 life.
    runner
        .act(GameAction::SelectTargets {
            targets: vec![
                engine::types::ability::TargetRef::Player(P1),
                engine::types::ability::TargetRef::Player(P2),
            ],
        })
        .expect("selecting two player targets must succeed");
    runner.advance_until_stack_empty();

    assert_eq!(
        library_count(&runner, P1),
        0,
        "P1 was targeted and must mill its library card"
    );
    assert_eq!(
        library_count(&runner, P2),
        0,
        "P2 was targeted and must mill its library card"
    );
    assert_eq!(
        library_count(&runner, P0),
        1,
        "P0 was not targeted and must not mill"
    );
    assert_eq!(life_total(&runner, P1), 19, "P1 loses 1 life");
    assert_eq!(life_total(&runner, P2), 19, "P2 loses 1 life");
    assert_eq!(
        life_total(&runner, P0),
        20,
        "P0 was not targeted and keeps its life"
    );
}
