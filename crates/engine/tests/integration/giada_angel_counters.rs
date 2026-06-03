//! GitHub issue #204 — Giada, Font of Hope.
//!
//! Oracle: "Flying, vigilance\nEach other Angel you control enters with an
//! additional +1/+1 counter on it for each Angel you already control.\n
//! {T}: Add {W}. Spend this mana only to cast an Angel spell."
//!
//! Two independent parser defects made Giada's replacement effect a no-op:
//!
//!   1. `parse_enters_with_counters` rejected the subtype-only subject
//!      `"Angel you control"` because a hardcoded guard only accepted subjects
//!      containing the literal word "creature" or "permanent". `valid_card`
//!      fell back to `SelfRef`, so the replacement fired only for Giada.
//!   2. `parse_for_each_controlled_type` ran a literal `tag(" you control")`
//!      after the type word, which could not match `" already control"`. The
//!      count fell back to `Fixed { value: 1 }` instead of a dynamic
//!      `ObjectCount` over Angels controlled.
//!
//! These tests drive the real cast → stack → resolve → ETB → replacement
//! pipeline through `apply` — only the real pipeline fires the `ChangeZone`
//! replacement, so hand-placing an object would bypass the bug entirely.
//!
//! CR references (verified against docs/MagicCompRules.txt):
//!   - CR 122.6 / 122.6a: "If an object enters the battlefield with counters
//!     on it ... the object's controller puts those counters on it."
//!   - CR 614.1c: a replacement effect that modifies how a permanent enters.
//!   - CR 614.12: external (non-SelfRef) ETB-counter replacements route through
//!     ChangeZone so token Angels also receive their counters.
//!   - CR 109.1: "Angel" is an object characteristic; the count is over Angel
//!     permanents under the source's controller.

use engine::game::scenario::{CastOutcome, GameScenario, P0, P1};
use engine::types::counter::CounterType;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

/// Giada's printed Oracle text — byte-identical to `client/public/card-data.json`.
const GIADA: &str = "Flying, vigilance\nEach other Angel you control enters with \
an additional +1/+1 counter on it for each Angel you already control.\n{T}: Add \
{W}. Spend this mana only to cast an Angel spell.";

/// Drive a creature spell from a player's hand through the canonical cast
/// pipeline. Scenario-built creatures have `ManaCost::zero()`, so the cast
/// auto-pays and needs no mana payment prompt.
fn cast_creature_from_hand(
    runner: &mut engine::game::scenario::GameRunner,
    hand_card: ObjectId,
) -> CastOutcome {
    runner.cast(hand_card).resolve()
}

/// Core fix: with Giada plus one other Angel already on the battlefield, a
/// newly-cast Angel enters with +1/+1 counters equal to the number of Angels
/// its controller *already* controlled (Giada + the existing Angel = 2).
///
/// This validates BOTH defects: the replacement fires for a non-Giada Angel
/// (defect #1) and the count is the dynamic `ObjectCount` (defect #2).
#[test]
fn giada_other_angels_enter_with_counters() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Giada is herself an Angel — she counts toward "Angels you already control".
    scenario
        .add_creature_from_oracle(P0, "Giada, Font of Hope", 2, 2, GIADA)
        .with_subtypes(vec!["Angel"]);
    // A second Angel already on the battlefield.
    scenario
        .add_creature(P0, "Resident Angel", 3, 3)
        .with_subtypes(vec!["Angel"]);
    // The Angel to cast — 2 Angels are controlled before it enters.
    let newcomer = scenario
        .add_creature_to_hand(P0, "Incoming Angel", 4, 4)
        .with_subtypes(vec!["Angel"])
        .id();

    let mut runner = scenario.build();
    let outcome = cast_creature_from_hand(&mut runner, newcomer);

    outcome.assert_zone(&[newcomer], Zone::Battlefield);
    // The entering Angel must enter with 2 +1/+1 counters — one per Angel
    // (Giada + Resident Angel) controlled before it entered (CR 122.6).
    outcome.assert_counters(newcomer, CounterType::Plus1Plus1, 2);
}

/// The entering Angel must NOT count itself — "Angels you *already* control".
/// With Giada the only other Angel, a second Angel enters with exactly 1
/// counter. This test fails loudly if defect #2 is unfixed: a `Fixed { value: 1 }`
/// fallback would coincidentally also produce 1 here, so the discriminating
/// scenario is `giada_other_angels_enter_with_counters` (count = 2).
#[test]
fn giada_does_not_double_count_self() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    scenario
        .add_creature_from_oracle(P0, "Giada, Font of Hope", 2, 2, GIADA)
        .with_subtypes(vec!["Angel"]);
    let newcomer = scenario
        .add_creature_to_hand(P0, "Incoming Angel", 4, 4)
        .with_subtypes(vec!["Angel"])
        .id();

    let mut runner = scenario.build();
    let outcome = cast_creature_from_hand(&mut runner, newcomer);

    // With Giada the only other Angel, the entering Angel gets exactly 1
    // counter and does not count itself (CR 122.6 — "Angels you already
    // control").
    outcome.assert_counters(newcomer, CounterType::Plus1Plus1, 1);
}

/// A token Angel must also receive the ETB counters. Token entry is a
/// `ChangeZone` event (CR 111.1) — validates the `valid_card != SelfRef`
/// branch routing the replacement through `ChangeZone` (CR 614.12).
#[test]
fn giada_token_angel_receives_counters() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    scenario
        .add_creature_from_oracle(P0, "Giada, Font of Hope", 2, 2, GIADA)
        .with_subtypes(vec!["Angel"]);
    scenario
        .add_creature(P0, "Resident Angel", 3, 3)
        .with_subtypes(vec!["Angel"]);
    // A sorcery that creates a 4/4 white Angel creature token.
    let token_maker = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Angelic Summons",
            false,
            "Create a 4/4 white Angel creature token with flying.",
        )
        .id();

    let mut runner = scenario.build();

    let outcome = runner.cast(token_maker).resolve();

    // The token is a new battlefield object — locate the Angel token by its
    // token identity and Angel subtype (CR 111.1).
    let token = outcome
        .state()
        .objects
        .values()
        .find(|o| {
            o.is_token
                && o.zone == Zone::Battlefield
                && o.card_types.subtypes.iter().any(|s| s == "Angel")
        })
        .map(|o| o.id)
        .expect("an Angel token must be on the battlefield");
    // The Angel token must enter with 2 +1/+1 counters (Giada + Resident
    // Angel) — external ETB-counter replacements route through ChangeZone so
    // tokens are covered (CR 614.12).
    outcome.assert_counters(token, CounterType::Plus1Plus1, 2);
}

/// Giada herself entering gets NO ETB counter — "each *other* Angel" excludes
/// the source. Validates defect #1's fix produced a `Typed` filter carrying
/// `FilterProp::Another`, not a `SelfRef` that would (wrongly) match Giada.
#[test]
fn giada_self_does_not_get_extra_counters() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let giada = scenario
        .add_creature_to_hand_from_oracle(P0, "Giada, Font of Hope", 2, 2, GIADA)
        .with_subtypes(vec!["Angel"])
        .id();

    let mut runner = scenario.build();
    let outcome = cast_creature_from_hand(&mut runner, giada);

    // Giada's own entry gets no ETB counter — "each OTHER Angel" excludes the
    // source (FilterProp::Another).
    outcome.assert_counters(giada, CounterType::Plus1Plus1, 0);
}

/// A non-Angel creature entering gets no counter — validates the
/// `Subtype("Angel")` filter discriminates by subtype.
#[test]
fn non_angel_creature_unaffected() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    scenario
        .add_creature_from_oracle(P0, "Giada, Font of Hope", 2, 2, GIADA)
        .with_subtypes(vec!["Angel"]);
    // A vanilla Bear — no Angel subtype.
    let bear = scenario
        .add_creature_to_hand(P0, "Grizzly Bears", 2, 2)
        .with_subtypes(vec!["Bear"])
        .id();

    let mut runner = scenario.build();
    let outcome = cast_creature_from_hand(&mut runner, bear);

    // A non-Angel creature must not receive Giada's ETB counters — the
    // replacement's valid_card filter is Subtype("Angel").
    outcome.assert_counters(bear, CounterType::Plus1Plus1, 0);
}

/// An opponent's Angel entering does NOT receive counters — "Angel YOU
/// control" excludes opponent permanents.
#[test]
fn opponent_angel_unaffected() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    scenario
        .add_creature_from_oracle(P0, "Giada, Font of Hope", 2, 2, GIADA)
        .with_subtypes(vec!["Angel"]);
    let opp_angel = scenario
        .add_creature_to_hand(P1, "Opposing Angel", 3, 3)
        .with_subtypes(vec!["Angel"])
        .id();

    let mut runner = scenario.build();
    // Documented escape hatch: hand priority to P1 so they can cast.
    {
        let state = runner.state_mut();
        state.active_player = P1;
        state.priority_player = P1;
        state.waiting_for = engine::types::game_state::WaitingFor::Priority { player: P1 };
    }

    let outcome = cast_creature_from_hand(&mut runner, opp_angel);

    // An opponent's Angel must not receive Giada's ETB counters — the
    // replacement's valid_card filter is controller: You.
    outcome.assert_counters(opp_angel, CounterType::Plus1Plus1, 0);
}
