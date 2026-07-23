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
//! per-win inside the `FlipCoinUntilLose` win_effect.
//!
//! CR references (verified against docs/MagicCompRules.txt):
//!   - CR 705.2: the flip-until-lose win effect runs once per win.
//!   - CR 707.2: a token that's a copy copies the entering creature's copiable
//!     values (name / P-T / subtypes).
//!   - CR 603.7c: the delayed exile affects only the created tokens.

use engine::game::keywords::object_has_effective_keyword_kind;
use engine::game::scenario::{GameScenario, P0};
use engine::types::keywords::KeywordKind;
use engine::types::phase::Phase;
use engine::types::zones::Zone;
use rand::SeedableRng;
use rand_chacha::ChaCha20Rng;

const MIRROR_MARCH: &str = "Whenever a nontoken creature you control enters, flip a coin until you lose a flip. For each flip you won, create a token that's a copy of that creature. Those tokens gain haste. Exile them at the beginning of the next end step.";

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
    runner.state_mut().rng = ChaCha20Rng::seed_from_u64(2);

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
    runner.state_mut().rng = ChaCha20Rng::seed_from_u64(1);
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
