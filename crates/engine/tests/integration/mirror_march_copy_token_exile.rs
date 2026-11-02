//! Runtime regression for Mirror March (#5966).
//!
//! Oracle (verbatim): "Whenever a nontoken creature you control enters, flip a
//! coin until you lose a flip. For each flip you won, create a token that's a
//! copy of that creature. Those tokens gain haste. Exile them at the beginning
//! of the next end step."
//!
//! Reported bug: the delayed "Exile them" cleanup exiled the ORIGINAL entering
//! creature (its target was `TriggeringSource`) alongside/instead of the copy
//! tokens. The parser fix lowers the win clause to `CopyTokenOf` and binds the
//! "those tokens"/"them" anaphora to the created tokens (`LastCreated`), folded
//! per-win inside the `FlipCoins { flips: UntilLoss }` win_effect.
//!
//! The same per-win quantifier also trails a win clause: Crazed Firecat's
//! "Put a +1/+1 counter on this creature for each flip you won." runs its
//! one-counter placement once per win inside the same `FlipCoins { flips: UntilLoss }` loop.
//!
//! CR references (verified against docs/MagicCompRules.txt):
//!   - CR 705.2: the flip-until-lose win effect runs once per win.
//!   - CR 707.2: a token that's a copy copies the entering creature's copiable
//!     values (name / P-T / subtypes).
//!   - CR 603.7c: the delayed exile affects only the created tokens.

use engine::game::keywords::object_has_effective_keyword_kind;
use engine::game::scenario::{GameScenario, P0};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::{CastPaymentMode, GameRng, WaitingFor};
use engine::types::keywords::KeywordKind;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const CRAZED_FIRECAT: &str = "When this creature enters, flip a coin until you lose a flip. Put a +1/+1 counter on this creature for each flip you won.";
const MIRROR_MARCH: &str = "Whenever a nontoken creature you control enters, flip a coin until you lose a flip. For each flip you won, create a token that's a copy of that creature. Those tokens gain haste. Exile them at the beginning of the next end step.";
const TAVERN_SCOUNDREL: &str = "Whenever you win a coin flip, create two Treasure tokens. (They're artifacts with \"{T}, Sacrifice this token: Add one mana of any color.\")\n{1}, {T}, Sacrifice another permanent: Flip a coin.";
const KRARKS_THUMB: &str = "If you would flip a coin, instead flip two coins and ignore one.";

/// CR 707.2 + CR 603.7c (#5966): with Mirror March out, a nontoken creature
/// entering under its controller creates copy tokens (one per coin-flip win)
/// that gain haste; the delayed cleanup exiles ONLY those tokens at the next end
/// step — the original entering creature stays on the battlefield.
#[test]
fn mirror_march_exiles_only_copy_tokens_not_the_original() {
    let mut scenario = GameScenario::new();
    // Post-combat main so advancing to the end step does not stop at the
    // declare-attackers turn-based action (CR 508.1).
    scenario.at_phase(Phase::PostCombatMain);

    // Mirror March's trigger, staged on the battlefield as a 4/4 carrier (>0
    // toughness so it survives state-based actions; the ETB-of-others trigger
    // functions from the battlefield regardless of the carrier's card type).
    scenario.add_creature_from_oracle(P0, "Mirror March", 4, 4, MIRROR_MARCH);

    // A distinctive nontoken creature that will enter and be copied.
    let bear = scenario
        .add_creature_to_hand(P0, "Grizzly Bears", 2, 2)
        .with_subtypes(vec!["Bear"])
        .id();

    let mut runner = scenario.build();
    // Seed the flip sequence to win, win, lose → exactly 2 copy tokens.
    runner.state_mut().rng = GameRng::seed_from_u64(2);

    // Cast the bear; on resolution it enters, firing Mirror March's ETB trigger.
    runner.cast(bear).resolve();
    runner.advance_until_stack_empty();

    // Collect the copy tokens P0 controls.
    let copy_token_ids: Vec<_> = {
        let state = runner.state();
        state
            .battlefield
            .iter()
            .filter_map(|id| state.objects.get(id))
            .filter(|o| o.is_token && o.controller == P0)
            .map(|o| o.id)
            .collect()
    };
    assert_eq!(
        copy_token_ids.len(),
        2,
        "seed 2 (win,win,lose) must create exactly 2 copy tokens"
    );

    // CR 707.2: each token is an actual copy of Grizzly Bears (this is the
    // ParentTarget-resolves-to-the-entering-creature runtime check), and each
    // has haste ("Those tokens gain haste" bound to the created tokens).
    {
        let state = runner.state();
        for &tid in &copy_token_ids {
            let t = state.objects.get(&tid).expect("token exists");
            assert_eq!(
                t.name, "Grizzly Bears",
                "token copies the entering creature's name"
            );
            assert_eq!(t.power, Some(2), "token copies power 2");
            assert_eq!(t.toughness, Some(2), "token copies toughness 2");
            assert!(
                t.card_types
                    .subtypes
                    .iter()
                    .any(|s| s.eq_ignore_ascii_case("Bear")),
                "token copies the Bear subtype, got {:?}",
                t.card_types.subtypes
            );
            assert!(
                object_has_effective_keyword_kind(state, tid, KeywordKind::Haste),
                "each copy token must gain haste"
            );
        }
    }

    // Advance to the next end step: the delayed exile resolves.
    runner.advance_to_end_step();
    runner.advance_until_stack_empty();

    let state = runner.state();
    // Both copy tokens have left the battlefield (exiled; a token in exile then
    // ceases to exist via SBA, CR 111.8).
    for &tid in &copy_token_ids {
        assert!(
            !state.battlefield.contains(&tid),
            "copy token {tid:?} must be exiled at the next end step"
        );
    }
    // REVERT-GUARD (#5966): the ORIGINAL entering creature must NOT be exiled.
    // Pre-fix the delayed exile targeted `TriggeringSource` (the entering
    // creature), so the bear was exiled — this assertion flips on revert.
    assert!(
        state.battlefield.contains(&bear),
        "the original entering creature must remain on the battlefield (#5966)"
    );
    assert_ne!(
        state.objects.get(&bear).map(|o| o.zone),
        Some(Zone::Exile),
        "the original entering creature must not be in exile (#5966)"
    );
}

/// Negative control: an immediate losing flip (0 wins) creates no tokens and
/// exiles nothing — the original creature is untouched. Pairs with the positive
/// test as the reach-guard proving the trigger actually ran.
#[test]
fn mirror_march_no_wins_creates_no_tokens() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PostCombatMain);
    scenario.add_creature_from_oracle(P0, "Mirror March", 4, 4, MIRROR_MARCH);
    let bear = scenario
        .add_creature_to_hand(P0, "Grizzly Bears", 2, 2)
        .with_subtypes(vec!["Bear"])
        .id();

    let mut runner = scenario.build();
    // Seed 1: first flip is a loss → 0 wins.
    runner.state_mut().rng = GameRng::seed_from_u64(1);
    runner.cast(bear).resolve();
    runner.advance_until_stack_empty();

    let token_count = runner
        .state()
        .battlefield
        .iter()
        .filter_map(|id| runner.state().objects.get(id))
        .filter(|o| o.is_token && o.controller == P0)
        .count();
    assert_eq!(token_count, 0, "an immediate losing flip creates no tokens");

    runner.advance_to_end_step();
    runner.advance_until_stack_empty();
    assert!(
        runner.state().battlefield.contains(&bear),
        "the entering creature is untouched when no tokens are created"
    );
}

/// CR 705.2: Crazed Firecat's ETB flips until a loss, then puts one +1/+1
/// counter on itself per won flip. Resolves the full Oracle through the real
/// cast → ETB trigger → coin-flip loop pipeline.
fn crazed_firecat_counters_after_etb(seed: u64) -> u32 {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PostCombatMain);
    let firecat = scenario
        .add_creature_to_hand_from_oracle(P0, "Crazed Firecat", 4, 4, CRAZED_FIRECAT)
        .id();

    let mut runner = scenario.build();
    runner.state_mut().rng = GameRng::seed_from_u64(seed);
    runner.cast(firecat).resolve();
    runner.advance_until_stack_empty();

    let state = runner.state();
    assert!(
        state.battlefield.contains(&firecat),
        "Crazed Firecat must be on the battlefield"
    );
    state.objects[&firecat]
        .counters
        .get(&CounterType::Plus1Plus1)
        .copied()
        .unwrap_or(0)
}

#[test]
fn crazed_firecat_puts_one_counter_per_won_flip() {
    // Seed 2 flips win, win, lose (see the Mirror March test above): two
    // counters, one per won flip — not zero (a dropped win clause) and not one
    // (a single unconditional placement).
    assert_eq!(crazed_firecat_counters_after_etb(2), 2);
}

#[test]
fn crazed_firecat_immediate_loss_puts_no_counters() {
    // Seed 1 loses the first flip: zero wins, zero counters. Pairs with the
    // positive test above, which proves the win clause is reached.
    assert_eq!(crazed_firecat_counters_after_etb(1), 0);
}

/// CR 608.2c + CR 603.2: Mirror March finishes flipping before creating its
/// copies, so only the original Scoundrel can trigger on this run's wins.
#[test]
fn mirror_march_copies_of_tavern_scoundrel_see_none_of_the_runs_flips() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PostCombatMain);
    scenario.add_creature_from_oracle(P0, "Mirror March", 4, 4, MIRROR_MARCH);
    let scoundrel = scenario
        .add_creature_to_hand_from_oracle(P0, "Tavern Scoundrel", 1, 3, TAVERN_SCOUNDREL)
        .id();

    let mut runner = scenario.build();
    // Scanning seeds 0..64 for win, win, lose selects seed 2. At least two
    // wins are needed: with just one win an interleaved copy sees no later win.
    runner.state_mut().rng = GameRng::seed_from_u64(2);
    runner.cast(scoundrel).resolve();
    runner.advance_until_stack_empty();

    let state = runner.state();
    let tokens: Vec<_> = state
        .battlefield
        .iter()
        .filter_map(|id| state.objects.get(id))
        .filter(|object| object.is_token && object.controller == P0)
        .collect();
    let copies = tokens
        .iter()
        .filter(|object| object.name == "Tavern Scoundrel")
        .count();
    let treasures = tokens
        .iter()
        .filter(|object| object.name == "Treasure")
        .count();
    assert!(copies >= 2, "the pinned seed must reach multiple won flips");
    assert!(
        treasures > 0,
        "the original Scoundrel's won-flip trigger must actually fire"
    );
    assert_eq!(
        treasures,
        2 * copies,
        "only the original Scoundrel sees the wins; the copies enter after all flips"
    );
    assert!(state.battlefield.contains(&scoundrel));
    assert!(!state.objects[&scoundrel].is_token);
    assert!(
        state.stack.is_empty(),
        "all copy and Treasure triggers finish"
    );
    assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
}

/// CR 608.2c + CR 614.4: copied flip doublers enter after the run, so each
/// flip is replaced only by the original doubler's ability (CR 614.1a).
#[test]
fn mirror_march_copying_a_flip_doubler_doubles_each_flip_only_once() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PostCombatMain);
    scenario.add_creature_from_oracle(P0, "Mirror March", 4, 4, MIRROR_MARCH);
    // The krark_thumb_coin_flip scaffold uses the same nonlegendary 0/1
    // creature carrier: the replacement does not depend on artifact type.
    // Avoiding legendary keeps the copies on the battlefield (CR 704.5j).
    let doubler = scenario
        .add_creature_to_hand_from_oracle(P0, "Krark's Thumb", 0, 1, KRARKS_THUMB)
        .id();

    let mut runner = scenario.build();
    // Scanning seeds 0..64 for a true in the first pair and a false in the
    // second selects seed 0: [true, false], then [false, true].
    runner.state_mut().rng = GameRng::seed_from_u64(0);
    let card_id = runner.state().objects[&doubler].card_id;
    let mut result = runner
        .act(GameAction::CastSpell {
            object_id: doubler,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast the Oracle-defined flip doubler");

    let mut prompts = 0;
    let mut kept_wins = 0;
    let mut kept_losses = 0;
    for _ in 0..60 {
        match result.waiting_for.clone() {
            WaitingFor::CoinFlipKeepChoice {
                player,
                results,
                keep_count,
            } => {
                prompts += 1;
                assert_eq!(player, P0, "the entering creature's controller flips");
                assert_eq!(keep_count, 1, "ignore one of the two results");
                assert_eq!(
                    results.len(),
                    2,
                    "copies must not enter early and double a later flip again"
                );
                // Keep a win first to force another flip; thereafter keep a
                // loss whenever offered so the run completes deterministically.
                let desired = prompts == 1;
                let keep = results.iter().position(|won| *won == desired).unwrap_or(0);
                if prompts == 1 {
                    assert!(results[keep], "the pinned seed must offer a first win");
                }
                if results[keep] {
                    kept_wins += 1;
                } else {
                    kept_losses += 1;
                }
                result = runner
                    .act(GameAction::SelectCoinFlips {
                        keep_indices: vec![keep],
                    })
                    .expect("keep the selected coin flip");
            }
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
            WaitingFor::Priority { .. } => {
                result = runner
                    .act(GameAction::PassPriority)
                    .expect("pass priority to resolve the creature and Mirror March");
            }
            other => panic!("unexpected waiting state during Mirror March: {other:?}"),
        }
    }

    assert!(prompts >= 2, "a kept win must be followed by another flip");
    assert!(kept_wins >= 1, "the copy-producing branch must be reached");
    assert_eq!(kept_losses, 1, "a kept loss finishes the run");
    let state = runner.state();
    // Only the COUNT of tokens is asserted, not which object they copy: at
    // BASE a token created by the won-flip branch after a Krark keep-choice
    // suspension copies Mirror March itself instead of the entering creature
    // (the "that creature" referent is lost across the suspension). That is a
    // pre-existing defect outside this refactor, so it is pinned neither as BASE
    // nor as fixed.
    let tokens = state
        .battlefield
        .iter()
        .filter_map(|id| state.objects.get(id))
        .filter(|object| object.is_token && object.controller == P0)
        .count();
    assert_eq!(
        tokens, kept_wins,
        "one copy token is owed for each kept win"
    );
    assert!(state.battlefield.contains(&doubler));
    assert!(state.stack.is_empty(), "Mirror March must finish resolving");
    assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
}
