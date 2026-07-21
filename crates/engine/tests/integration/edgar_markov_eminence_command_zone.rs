//! Discriminating runtime regression for #817 — Edgar Markov's Eminence
//! triggered ability must fire while Edgar is in the command zone.
//!
//! Oracle (verified from `client/public/card-data.json`):
//!   Eminence — Whenever you cast another Vampire spell, if Edgar is in
//!   the command zone or on the battlefield, create a 1/1 black Vampire
//!   creature token.
//!
//! CR annotations (grep-verified against `docs/MagicCompRules.txt`):
//!   - CR 207.2c — "Eminence" is an ability word with no independent rules
//!     meaning; the intervening-"if" clause is what makes the trigger function
//!     from the command zone.
//!   - CR 113.6b — an ability that mentions a zone functions only from the
//!     zone(s) it mentions; the parsed trigger's `trigger_zones` must therefore
//!     include `Command`, not just `Battlefield`.
//!   - CR 603.4 — the intervening-"if" condition is checked when the trigger
//!     would be put on the stack and again as it resolves; Edgar in the command
//!     zone satisfies it both times.
//!   - CR 408 — the command zone.
//!
//! Unlike `the_ur_dragon_eminence.rs` (which hand-builds the cost-reduction
//! *static* to exercise the cost-determination path), this test drives the real
//! parser via `from_oracle_text` and the full cast pipeline, so it guards the
//! end-to-end parser → command-zone trigger scan → resolution path that #817
//! reported broken. That path currently has no runtime coverage.

use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

/// Edgar Markov's Eminence line. `add_creature_to_hand_from_oracle` runs this
/// through the production parser, so the trigger's `trigger_zones` are whatever
/// the parser derives — the exact surface #817 is about.
const EDGAR_EMINENCE: &str = "Eminence — Whenever you cast another Vampire spell, \
     if Edgar is in the command zone or on the battlefield, create a 1/1 \
     black Vampire creature token.";

/// Count 1/1 Vampire creature *tokens* controlled by `controller` on the
/// battlefield. Filtering on `is_token` distinguishes the created token from the
/// non-token Vampire spell that was cast to trigger it.
fn vampire_token_count(runner: &GameRunner, controller: PlayerId) -> usize {
    let state = runner.state();
    state
        .battlefield
        .iter()
        .filter_map(|id| state.objects.get(id))
        .filter(|o| {
            o.is_token
                && o.controller == controller
                && o.card_types
                    .subtypes
                    .iter()
                    .any(|s| s.eq_ignore_ascii_case("Vampire"))
        })
        .count()
}

/// CR 113.6b + CR 603.4: with Edgar in the command zone, casting *another*
/// Vampire spell must create exactly one 1/1 Vampire token.
#[test]
fn edgar_eminence_triggers_another_vampire_from_command_zone() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Edgar Markov: parse the Eminence trigger from real Oracle text, then move
    // to the command zone (CR 408) — the zone #817 says the trigger is missed.
    let edgar_id = scenario
        .add_creature_to_hand_from_oracle(P0, "Edgar Markov", 4, 4, EDGAR_EMINENCE)
        .with_subtypes(vec!["Vampire", "Knight"])
        .id();
    scenario.with_commander(edgar_id);

    // A different Vampire spell — this is the "another Vampire spell" that
    // triggers Eminence (CR 201.4 self-reference excludes Edgar itself).
    let vampire_id = scenario
        .add_creature_to_hand(P0, "Vampire Recruit", 1, 1)
        .with_subtypes(vec!["Vampire"])
        .id();

    let mut runner = scenario.build();
    assert_eq!(
        vampire_token_count(&runner, P0),
        0,
        "no Vampire tokens should exist before the spell is cast"
    );

    runner.cast(vampire_id).resolve();

    assert_eq!(
        vampire_token_count(&runner, P0),
        1,
        "CR 113.6b/603.4: casting another Vampire spell with Edgar Markov in the \
         command zone must create exactly one 1/1 Vampire token (#817)"
    );
}

/// Negative control: a non-Vampire spell must NOT trigger Eminence, proving the
/// token in the positive case comes from the Vampire-typed cast and not from an
/// over-broad command-zone trigger scan.
#[test]
fn edgar_eminence_ignores_non_vampire_spell_from_command_zone() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let edgar_id = scenario
        .add_creature_to_hand_from_oracle(P0, "Edgar Markov", 4, 4, EDGAR_EMINENCE)
        .with_subtypes(vec!["Vampire", "Knight"])
        .id();
    scenario.with_commander(edgar_id);

    // A Human (non-Vampire) spell — the `Subtype Vampire` gate must reject it.
    let human_id = scenario
        .add_creature_to_hand(P0, "Village Elder", 2, 2)
        .with_subtypes(vec!["Human"])
        .id();

    let mut runner = scenario.build();
    runner.cast(human_id).resolve();

    assert_eq!(
        vampire_token_count(&runner, P0),
        0,
        "casting a non-Vampire spell must not trigger Eminence (#817 negative control)"
    );
}
