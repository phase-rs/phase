//! Tataru Taru (FIC) — targeted-optional "target opponent may draw …" runtime
//! discriminators (CR 608.2d + CR 115.1 + CR 608.2c).
//!
//! The parser change makes `target opponent may draw …` a TARGETED optional whose
//! actor is the clause's own declared-player reference (`optional_player`), so the
//! announced opponent — and nobody else — is asked, and the old fan-out over every
//! opponent is gone. These tests drive the real cast / activate / trigger
//! pipelines in a 3-player game and assert the seat that draws (and the seat that
//! is asked), which flips when the change is reverted.
//!
//! Target the SECOND opponent (P2), not P1. At the base commit the `and`-joined
//! clause never splits, so the ability lowers to a whole-clause `Unimplemented`
//! and raises no prompt at all; with only the bare-`and` splitter applied (and no
//! narrowing) the conjunct peels to the controller-held fan-out (P1 first, both
//! opponents draw). Targeting P2 ensures neither a seat-order fallback nor the
//! controller fallback (P0) can accidentally satisfy the post-change assertion.
//!
//! Kuroki's cast-from-`their`-hand pool owner is a labelled pre-existing defect
//! (plan-v6 STOP-AND-RETURN #3): the pool owner is read from the filter's
//! top-level `controller`, which resolves to the printed controller. V11 asserts
//! only that the accept continuation is *entered* (an offer is present) and that
//! the accept/decline branches behave as base — it does NOT assert whose hand the
//! pool draws from.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const P2: PlayerId = PlayerId(2);

const TATARU_ORACLE: &str =
    "When Tataru Taru enters, you draw a card and target opponent may draw a card.";

const QUESTING_PHELDDAGRIF_ORACLE: &str = "{G}: This creature gets +1/+1 until end of turn. \
     Target opponent creates a 1/1 green Hippo creature token.\n\
     {W}: This creature gains protection from black and from red until end of turn. Target \
     opponent gains 2 life.\n\
     {U}: This creature gains flying until end of turn. Target opponent may draw a card.";

const KUROKI_ORACLE: &str = "Menace\n\
     At the beginning of your end step, target opponent may draw four cards. If they do, look \
     at that player's hand and you may cast a spell from their hand without paying its mana \
     cost. If they don't, put two +1/+1 counters on Kuroki.";

fn hand_len(runner: &GameRunner, player: PlayerId) -> usize {
    runner
        .state()
        .players
        .iter()
        .find(|p| p.id == player)
        .map(|p| p.hand.len())
        .unwrap_or(0)
}

fn library(pid: PlayerId, scenario: &mut GameScenario) {
    // CR 704.5b: an empty library decks a player out, ending the game before any
    // trigger resolves — every player's library is pre-seeded.
    scenario.with_library_top(
        pid,
        &[
            "Lib 1", "Lib 2", "Lib 3", "Lib 4", "Lib 5", "Lib 6", "Lib 7", "Lib 8", "Lib 9",
            "Lib 10",
        ],
    );
}

/// V4 + V5 — CR 608.2d + CR 115.1: Tataru's enters trigger asks ONLY the announced
/// target opponent (P2), the controller draws (V4), and only the announced
/// opponent draws (V5). The mandatory controller draw is the first clause; the
/// targeted optional is the second.
///
/// Revert discriminator: at the base commit the `and`-joined clause never splits
/// (whole-clause `Unimplemented`, so no optional prompt is raised); with only the
/// bare-`and` splitter applied the conjunct peels to the pre-narrowing
/// controller-held fan-out (P1 first, both opponents draw). Either way the
/// announced-opponent assertions below fail.
#[test]
fn tataru_enters_asks_and_draws_only_the_announced_target_opponent() {
    // --- Prompt seat: stop at the optional choice and assert the announcer. ---
    let (mut runner, card) = tataru_scenario(42);
    let stopped = runner
        .cast(card)
        .target_player(P2)
        .commit()
        .stop_at_optional()
        .resolve();
    match stopped.final_waiting_for() {
        WaitingFor::OptionalEffectChoice { player, .. } => {
            assert_eq!(
                *player, P2,
                "CR 608.2d: the announced target opponent (P2) announces the may"
            );
            assert_ne!(
                *player, P0,
                "the controller must not announce the targeted optional"
            );
        }
        other => panic!("expected the targeted optional prompt, got {other:?}"),
    }
    // V4: the controller's mandatory draw already happened (it is the first
    // clause, before the optional), so the fixture really reached the trigger.
    stopped.assert_hand_drawn(P0, 1);

    // --- Accept: only the announced opponent draws. ---
    let (mut runner, card) = tataru_scenario(42);
    let accepted = runner
        .cast(card)
        .target_player(P2)
        .accept_optional()
        .resolve();
    assert!(
        matches!(accepted.final_waiting_for(), WaitingFor::Priority { .. }),
        "the trigger resolved to a clean priority window, got {:?}",
        accepted.final_waiting_for()
    );
    accepted.assert_hand_drawn(P0, 1);
    accepted.assert_hand_drawn(P2, 1);
    accepted.assert_hand_drawn(P1, 0);

    // --- Decline: nobody but the controller draws. ---
    let (mut runner, card) = tataru_scenario(42);
    let declined = runner
        .cast(card)
        .target_player(P2)
        .decline_optional()
        .resolve();
    declined.assert_hand_drawn(P0, 1);
    declined.assert_hand_drawn(P1, 0);
    declined.assert_hand_drawn(P2, 0);
}

fn tataru_scenario(seed: u64) -> (GameRunner, ObjectId) {
    let mut scenario = GameScenario::new_n_player(3, seed);
    scenario.at_phase(Phase::PreCombatMain);
    for pid in [P0, P1, P2] {
        library(pid, &mut scenario);
    }
    let card = scenario
        .add_creature_to_hand_from_oracle(P0, "Tataru Taru", 0, 3, TATARU_ORACLE)
        .with_mana_cost(ManaCost::zero())
        .id();
    (scenario.build(), card)
}

/// V6 — CR 115.1 + CR 601.2c + CR 608.2d: Questing Phelddagrif's activated `{U}`
/// ability is a targeted optional. Targeting the SECOND opponent (P2) is the
/// discriminator: pre-change the Draw body declares NO player slot (its target is
/// `Controller`, and `extract_target_filter_from_effect` filters context refs),
/// so `target_player(P2)` cannot be assigned and both opponents are prompted;
/// post-change exactly one stack-time player target slot exists (surfaced by the
/// sub-ability recursion) and only P2 is asked/draws. The `{U}` pump half is
/// unchanged (the ability still resolves and the source is present).
#[test]
fn questing_phelddagrif_activated_draw_targets_the_announced_opponent() {
    let blue = || ManaUnit::new(ManaType::Blue, ObjectId(0), false, vec![]);
    let build_source = |scenario: &mut GameScenario| {
        scenario
            .add_creature_from_oracle(
                P0,
                "Questing Phelddagrif",
                4,
                4,
                QUESTING_PHELDDAGRIF_ORACLE,
            )
            .id()
    };

    // Prompt seat: stop at the optional choice and assert the announced opponent.
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    for pid in [P0, P1, P2] {
        library(pid, &mut scenario);
    }
    scenario.with_mana_pool(P0, vec![blue()]);
    let source = build_source(&mut scenario);
    let mut runner = scenario.build();
    let stopped = runner
        .activate(source, 2)
        .target_player(P2)
        .stop_at_optional()
        .resolve();
    match stopped.final_waiting_for() {
        WaitingFor::OptionalEffectChoice { player, .. } => {
            assert_eq!(
                *player, P2,
                "CR 608.2d: the announced target opponent (P2) announces the may"
            );
            assert_ne!(
                *player, P0,
                "the controller must not announce the targeted optional"
            );
        }
        other => panic!("expected the targeted optional prompt, got {other:?}"),
    }

    // Accept: only the announced opponent (P2) draws.
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    for pid in [P0, P1, P2] {
        library(pid, &mut scenario);
    }
    scenario.with_mana_pool(P0, vec![blue()]);
    let source = build_source(&mut scenario);
    let mut runner = scenario.build();
    // Ability order is {G}, {W}, {U}; index 2 is the targeted optional.
    let outcome = runner
        .activate(source, 2)
        .target_player(P2)
        .accept_optional()
        .resolve();
    outcome.assert_hand_drawn(P2, 1);
    outcome.assert_hand_drawn(P1, 0);
    outcome.assert_hand_drawn(P0, 0);
    assert!(
        outcome.state().objects.contains_key(&source),
        "the pump half is unchanged: the source stays on the battlefield"
    );

    // Decline: nobody draws.
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    for pid in [P0, P1, P2] {
        library(pid, &mut scenario);
    }
    scenario.with_mana_pool(P0, vec![blue()]);
    let source = build_source(&mut scenario);
    let mut runner = scenario.build();
    let outcome = runner
        .activate(source, 2)
        .target_player(P2)
        .decline_optional()
        .resolve();
    outcome.assert_hand_drawn(P0, 0);
    outcome.assert_hand_drawn(P1, 0);
    outcome.assert_hand_drawn(P2, 0);
}

fn kuroki_scenario(seed: u64) -> (GameRunner, ObjectId) {
    let mut scenario = GameScenario::new_n_player(3, seed);
    scenario.at_phase(Phase::PreCombatMain);
    for pid in [P0, P1, P2] {
        library(pid, &mut scenario);
    }
    let kuroki = scenario
        .add_creature_from_oracle(P0, "Kuroki, Thief of Talents", 4, 4, KUROKI_ORACLE)
        .id();
    // The ANNOUNCED opponent (the Kuroki tests target P2) holds a spell, and the
    // printed controller holds one too, so the accept continuation's cast offer is
    // non-vacuous at whichever hand the engine reads (plan-v6 §Drive, V11): with
    // the announced-target pool AND with the defective printed-controller pool.
    scenario.add_spell_to_hand_from_oracle(P2, "Opponent Spell", false, "Draw a card.");
    scenario.add_spell_to_hand_from_oracle(P0, "Controller Spell", false, "Draw a card.");
    (scenario.build(), kuroki)
}

fn plus_counters(runner: &GameRunner, object: ObjectId) -> u32 {
    runner.state().objects[&object]
        .counters
        .get(&CounterType::Plus1Plus1)
        .copied()
        .unwrap_or(0)
}

/// What a Kuroki drive observed.
struct KurokiDrive {
    /// Every `OptionalEffectChoice` seat, in order (the first is the announced
    /// opponent's draw decision; a later one would be the accept continuation).
    optional_seats: Vec<PlayerId>,
    /// How many `CastFromZone` offer cards surfaced (`EffectZoneChoice`); > 0
    /// means the accept-gated continuation was entered.
    cast_offer_cards: usize,
    /// `(P1, P2)` hand sizes when the first cast offer surfaced: the announced
    /// opponent's draw has resolved and no offered spell has been cast yet.
    hands_at_offer: Option<(usize, usize)>,
}

/// Drive Kuroki's end-step trigger in a real turn loop: answer the trigger's
/// target prompt with `target`, answer the first `OptionalEffectChoice` (the
/// announced opponent's draw decision) with `accept_draw`, and answer any later
/// `OptionalEffectChoice` (the accept continuation's cast offer) with
/// `accept_later`. At most `count` offered `EffectZoneChoice` cards (the first
/// ones) are selected. Stops on the first post-trigger priority window with an
/// empty stack.
fn drive_kuroki(
    runner: &mut GameRunner,
    target: PlayerId,
    accept_draw: bool,
    accept_later: bool,
) -> KurokiDrive {
    use engine::types::ability::TargetRef;
    let mut drive = KurokiDrive {
        optional_seats: Vec::new(),
        cast_offer_cards: 0,
        hands_at_offer: None,
    };
    let mut first = true;
    let mut engaged = false;
    let mut converged = false;
    for _ in 0..400 {
        let waiting = runner.state().waiting_for.clone();
        match waiting {
            WaitingFor::Priority { .. } if engaged && runner.state().stack.is_empty() => {
                converged = true;
                break;
            }
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("priority pass must be accepted");
            }
            WaitingFor::TriggerTargetSelection { .. } => {
                engaged = true;
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(TargetRef::Player(target)),
                    })
                    .expect("the trigger's target choice must be accepted");
            }
            WaitingFor::OptionalEffectChoice { player, .. } => {
                engaged = true;
                drive.optional_seats.push(player);
                let accept = if first { accept_draw } else { accept_later };
                first = false;
                runner
                    .act(GameAction::DecideOptionalEffect { accept })
                    .expect("the optional decision must be accepted");
            }
            WaitingFor::EffectZoneChoice { cards, count, .. } => {
                drive
                    .hands_at_offer
                    .get_or_insert_with(|| (hand_len(runner, P1), hand_len(runner, P2)));
                drive.cast_offer_cards += cards.len();
                // "you may cast a spell": select at most `count` offered cards.
                let chosen: Vec<ObjectId> = cards.into_iter().take(count).collect();
                runner
                    .act(GameAction::SelectCards { cards: chosen })
                    .expect("the cast-offer selection must be accepted");
            }
            WaitingFor::DeclareAttackers { .. } => {
                runner
                    .act(GameAction::DeclareAttackers {
                        attacks: vec![],
                        bands: vec![],
                    })
                    .expect("the attackers declaration must be accepted");
            }
            WaitingFor::DeclareBlockers { .. } => {
                runner
                    .act(GameAction::DeclareBlockers {
                        assignments: vec![],
                    })
                    .expect("the blockers declaration must be accepted");
            }
            other => panic!("unexpected prompt while driving Kuroki: {other:?}"),
        }
    }
    assert!(
        converged,
        "Kuroki's resolution must reach Priority with an empty stack; stopped at {:?} \
         (stack depth {}, prompts {:?})",
        runner.state().waiting_for,
        runner.state().stack.len(),
        drive.optional_seats
    );
    drive
}

/// V11 (accept / draw seat) — CR 608.2c + CR 115.1 + CR 608.2d: Kuroki's
/// end-step `target opponent may draw four cards` asks ONLY the announced target
/// opponent (P2). Revert discriminator: pre-change the controller-held fan-out
/// asks and draws for BOTH opponents (seats `[P1, P2]`); post-change the first
/// seat is exactly `P2`. Targeting the SECOND opponent also refutes the
/// seat-order fallback in `resolve_effect_player_ref(Opponent)` (which would pick
/// P1) and the controller fallback (which would pick P0).
///
/// Non-regression (plan-v6 §F1′, STOP-AND-RETURN #3): the accept-gated
/// continuation IS entered (an offer is present — asserting its mere PRESENCE,
/// not whose hand it draws from), and the `Not(OptionalEffectPerformed)` gate is
/// suppressed, so no `+1/+1` counters are placed on accept. Hand sizes are read
/// when the cast offer surfaces, before any offered spell is cast, so the draw
/// assertions do not depend on which hand the offer reads from.
#[test]
fn kuroki_draw_branch_asks_and_draws_only_the_announced_opponent() {
    let (mut runner, kuroki) = kuroki_scenario(42);
    let p1_before = hand_len(&runner, P1);
    let p2_before = hand_len(&runner, P2);

    let drive = drive_kuroki(&mut runner, P2, true, true);

    assert_eq!(
        drive.optional_seats.first().copied(),
        Some(P2),
        "the announcement goes to the announced target opponent (P2), not P0/P1"
    );
    assert!(
        !drive.optional_seats.contains(&P1),
        "the other opponent must never be asked: {:?}",
        drive.optional_seats
    );
    assert!(
        drive.cast_offer_cards > 0,
        "the accept-gated continuation must be entered with an offer present"
    );
    assert_eq!(
        drive.hands_at_offer,
        Some((p1_before, p2_before + 4)),
        "at the cast offer the announced opponent (P2) has drawn four cards and \
         the other opponent (P1) has drawn none"
    );
    assert_eq!(
        plus_counters(&runner, kuroki),
        0,
        "CR 608.2d: the \"if they don't\" counter branch is suppressed on accept"
    );
}

/// V11 (decline) — CR 608.2d + CR 121.1: declining the may draws nobody and
/// resolves the "if they don't" branch: exactly two +1/+1 counters on Kuroki.
#[test]
fn kuroki_decline_branch_places_two_counters_and_draws_nobody() {
    let (mut runner, kuroki) = kuroki_scenario(42);
    let p1_before = hand_len(&runner, P1);
    let p2_before = hand_len(&runner, P2);

    let drive = drive_kuroki(&mut runner, P2, false, false);

    assert_eq!(
        drive.optional_seats.first().copied(),
        Some(P2),
        "the announcement goes to the announced target opponent (P2), not P0/P1"
    );
    assert!(
        !drive.optional_seats.contains(&P1),
        "the other opponent must never be asked: {:?}",
        drive.optional_seats
    );
    assert_eq!(hand_len(&runner, P1), p1_before, "declined draw");
    assert_eq!(hand_len(&runner, P2), p2_before, "declined draw");
    assert_eq!(
        plus_counters(&runner, kuroki),
        2,
        "CR 608.2d: the \"if they don't\" branch puts two +1/+1 counters on Kuroki"
    );
}

// ---------------------------------------------------------------------------
// Unit 1 runtime coverage — the splitter's targeted-discard siblings
// (`<verb clause> and target opponent discards …`). The announced opponent, and
// only that opponent, discards, and the card's own count is read relative to
// that seat.
// ---------------------------------------------------------------------------

/// Drives a resolution that can raise a target choice, a discard choice, a trigger
/// order and priority prompts to a `Priority` state with an empty stack. Every
/// `act` must be ACCEPTED: a silently rejected action would otherwise let the
/// resolution stall with the hand/count assertions still satisfied.
///
/// Returns the `(player, cards chosen)` discard pairs in resolution order.
fn drive_discard_resolution(runner: &mut GameRunner, target: PlayerId) -> Vec<(PlayerId, usize)> {
    use engine::types::ability::TargetRef;
    let mut discards = Vec::new();
    let mut converged = false;
    for _ in 0..400 {
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => {
                converged = true;
                break;
            }
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("priority pass must be accepted");
            }
            WaitingFor::TriggerTargetSelection { .. } => {
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(TargetRef::Player(target)),
                    })
                    .expect("the trigger's target choice must be accepted");
            }
            WaitingFor::DiscardChoice {
                player,
                count,
                cards,
                ..
            } => {
                let chosen: Vec<ObjectId> = cards.into_iter().take(count).collect();
                assert_eq!(
                    chosen.len(),
                    count,
                    "the discard must offer at least {count} cards"
                );
                discards.push((player, chosen.len()));
                runner
                    .act(GameAction::SelectCards { cards: chosen })
                    .expect("the discard selection must be accepted");
            }
            WaitingFor::OrderTriggers { .. } => {
                engine::game::triggers::drain_order_triggers_with_identity(runner.state_mut());
            }
            other => panic!("unexpected prompt while resolving: {other:?}"),
        }
    }
    assert!(
        converged,
        "the resolution must reach Priority with an empty stack; stopped at {:?}",
        runner.state().waiting_for
    );
    discards
}

/// V6b — CR 608.2d + CR 115.1: the class member whose chain announces an OBJECT
/// target BEFORE the optional player slot. Soldevi Heretic's activated ability
/// targets a creature for the prevention half and the announced opponent for the
/// may, so this row exercises the declared-player slot numbering where the player
/// group is not the chain's first declared target.
#[test]
fn soldevi_heretic_activated_draw_targets_the_announced_opponent() {
    const SOLDEVI: &str = "{W}, {T}: Prevent the next 2 damage that would be dealt to target \
         creature this turn. Target opponent may draw a card.";
    let white = || ManaUnit::new(ManaType::White, ObjectId(0), false, vec![]);
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    for pid in [P0, P1, P2] {
        library(pid, &mut scenario);
    }
    scenario.with_mana_pool(P0, vec![white()]);
    let soldevi = scenario
        .add_creature_from_oracle(P0, "Soldevi Heretic", 2, 2, SOLDEVI)
        .id();
    let bystander = scenario
        .add_creature_from_oracle(P1, "Bystander", 1, 1, "")
        .id();
    let mut runner = scenario.build();

    let outcome = runner
        .activate(soldevi, 0)
        .target_object(bystander)
        .target_player(P2)
        .accept_optional()
        .resolve();

    outcome.assert_hand_drawn(P2, 1);
    outcome.assert_hand_drawn(P1, 0);
    outcome.assert_hand_drawn(P0, 0);
}

/// CR 121.1 + CR 701.9a + CR 115.1: Mindculling — "You draw two cards and target
/// opponent discards two cards." The splitter makes the second conjunct its own
/// targeted clause, so only the announced opponent discards, by exactly two.
///
/// Revert discriminator: at base the conjunct never splits (the whole clause is
/// `Unimplemented`, no draw and no discard); the announcement/target assertions
/// below fail.
#[test]
fn mindculling_targeted_discard_hits_only_the_announced_opponent() {
    const MINDCULLING: &str = "You draw two cards and target opponent discards two cards.";
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    for pid in [P0, P1, P2] {
        library(pid, &mut scenario);
    }
    // Different hand sizes so each seat's discard is measurable independently.
    for index in 0..5 {
        scenario.add_card_to_hand(P1, &format!("P1 Filler {index}"));
    }
    for index in 0..3 {
        scenario.add_card_to_hand(P2, &format!("P2 Filler {index}"));
    }
    let mindculling = scenario
        .add_spell_to_hand_from_oracle(P0, "Mindculling", false, MINDCULLING)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();
    let p0_before = hand_len(&runner, P0);
    let p1_before = hand_len(&runner, P1);
    let p2_before = hand_len(&runner, P2);

    runner.cast(mindculling).target_player(P2).commit();
    let discards = drive_discard_resolution(&mut runner, P2);

    assert_eq!(
        hand_len(&runner, P0),
        p0_before - 1 + 2,
        "the caster draws two (the cast itself consumed one card)"
    );
    assert_eq!(
        hand_len(&runner, P2),
        p2_before - 2,
        "the announced opponent discards exactly two"
    );
    assert_eq!(
        hand_len(&runner, P1),
        p1_before,
        "the other opponent is never asked and never discards"
    );
    assert_eq!(
        discards,
        vec![(P2, 2)],
        "exactly one discard, by the announced opponent, of two cards"
    );
}

/// CR 701.9a + CR 608.2c + CR 115.1: Brink of Madness — "At the beginning of your
/// upkeep, if you have no cards in hand, sacrifice this enchantment and target
/// opponent discards their hand." The whole-hand count is the ANNOUNCED
/// opponent's hand, so only that seat is emptied.
#[test]
fn brink_of_madness_whole_hand_discard_hits_only_the_announced_opponent() {
    const BRINK: &str = "At the beginning of your upkeep, if you have no cards in hand, \
         sacrifice this enchantment and target opponent discards their hand.";
    let mut scenario = GameScenario::new_n_player(3, 42);
    for pid in [P0, P1, P2] {
        library(pid, &mut scenario);
    }
    let brink = scenario
        .add_enchantment_from_oracle(P0, "Brink of Madness", BRINK)
        .id();
    for index in 0..5 {
        scenario.add_card_to_hand(P1, &format!("P1 Filler {index}"));
    }
    for index in 0..3 {
        scenario.add_card_to_hand(P2, &format!("P2 Filler {index}"));
    }
    let mut runner = scenario.build();
    // The intervening-if gate needs an EMPTY controller hand.
    assert_eq!(
        hand_len(&runner, P0),
        0,
        "reach guard: the upkeep gate requires an empty controller hand"
    );
    let p1_before = hand_len(&runner, P1);
    let p2_before = hand_len(&runner, P2);

    // CR 500.1 + CR 503.1: drive the engine's own turn machinery into P0's
    // upkeep, where the phase trigger fires (the intervening-if is already true:
    // the controller's hand is empty).
    runner.advance_to_upkeep();

    let discards = drive_discard_resolution(&mut runner, P2);

    assert_eq!(
        hand_len(&runner, P2),
        0,
        "the announced opponent discards their WHOLE hand ({p2_before} cards)"
    );
    assert_eq!(
        hand_len(&runner, P1),
        p1_before,
        "the other opponent is never asked and never discards"
    );
    // CR 701.9a: discarding an entire hand leaves nothing to choose, so the
    // engine raises no `DiscardChoice` prompt — the hand deltas above are the
    // discriminating signal for who discarded.
    assert!(
        discards.is_empty(),
        "a whole-hand discard needs no choice prompt, got {discards:?}"
    );
    assert_eq!(
        runner.state().objects[&brink].zone,
        Zone::Graveyard,
        "the sacrifice half of the same clause still resolves"
    );
}
