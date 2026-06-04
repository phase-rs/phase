//! Regression test for GitHub issue #434 — Virulent Emissary's
//! "Whenever another creature you control enters, you gain 1 life" trigger.
//!
//! The bug report claimed Virulent Emissary "did not trigger for the first few
//! turns". An end-to-end trace of the card data and the `ChangesZone` trigger
//! matcher found no defect: the card parses correctly with
//! `valid_card: Typed { type_filters: [Creature], controller: You,
//! properties: [Another] }` and `execute: GainLife { amount: 1,
//! player: controller }`. The most plausible reading of the report is that the
//! creatures the reporter expected to trigger Emissary were
//! *opponent-controlled* — which correctly do NOT satisfy "another creature
//! **you** control".
//!
//! This file is the regression deliverable: it pins the trigger's behavior by
//! driving the real `apply` pipeline via the opinionated cast harness
//! (`runner.cast(spell).resolve()`: cast → stack → resolve → ETB → trigger →
//! life gain) — no synthetic `GameEvent`s, no manual `GameState` poking beyond
//! the documented `state_mut()` escape hatch for setting the active player.
//!
//! CR 603.6a: "Enters-the-battlefield abilities trigger when a permanent enters
//! the battlefield. These are written ... 'Whenever a [type] enters, ...'"
//! CR 111.1: "A token is a marker used to represent any permanent ..." — tokens
//! are permanents and their entry fires the same ETB trigger (Case B).

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

/// Virulent Emissary's printed Oracle text — byte-identical to
/// `client/public/card-data.json` and MTGJSON `AtomicCards.json` (set ECL).
const VIRULENT_EMISSARY: &str =
    "Deathtouch\nWhenever another creature you control enters, you gain 1 life.";

/// Case A — another creature **you control** enters via a creature SPELL
/// resolving: Virulent Emissary's trigger fires and its controller gains 1 life.
///
/// This exercises the exact `controller: You` + `Another` combination — the
/// creature that enters is controlled by Emissary's controller and is a
/// different object than Emissary itself.
#[test]
fn virulent_emissary_triggers_on_your_creature_spell() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Virulent Emissary already on P0's battlefield.
    scenario.add_creature_from_oracle(P0, "Virulent Emissary", 1, 1, VIRULENT_EMISSARY);
    // A vanilla creature spell in P0's hand to cast.
    let bear = scenario.add_creature_to_hand(P0, "Grizzly Bear", 2, 2).id();

    let mut runner = scenario.build();
    // Vanilla creatures have ManaCost::zero(), so the cast auto-pays and the
    // ETB trigger resolves through the harness's resolution driver.
    let outcome = runner.cast(bear).resolve();

    outcome.assert_zone(&[bear], Zone::Battlefield);
    outcome.assert_life_delta(
        P0,
        1, // Virulent Emissary's "another creature you control enters" trigger
          // gains its controller exactly 1 life (CR 603.6a).
    );
}

/// Same-shape comparison that genuinely exercises the `controller: You` plus
/// `Another` combination: a second Virulent Emissary is placed under the
/// opponent. Its trigger watches the opponent's own creatures, so a creature
/// P1 controls entering gains P1 (not P0) life. This confirms the `You`
/// controller axis is resolved relative to each Emissary's own controller,
/// not hardcoded to P0 — proving the exact filter shape, not just "another
/// creature".
#[test]
fn virulent_emissary_controller_relative_you_filter() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // One Emissary per player.
    scenario.add_creature_from_oracle(P0, "Virulent Emissary", 1, 1, VIRULENT_EMISSARY);
    scenario.add_creature_from_oracle(P1, "Virulent Emissary", 1, 1, VIRULENT_EMISSARY);
    // P1 will cast a creature on P1's turn.
    let bear = scenario.add_creature_to_hand(P1, "Grizzly Bear", 2, 2).id();

    let mut runner = scenario.build();
    // Documented escape hatch: make it P1's turn so P1 may cast a sorcery-speed
    // creature with priority.
    {
        let state = runner.state_mut();
        state.active_player = P1;
        state.priority_player = P1;
        state.waiting_for = engine::types::game_state::WaitingFor::Priority { player: P1 };
    }

    let outcome = runner.cast(bear).resolve();

    outcome.assert_life_delta(
        P1,
        1, // P1's Emissary gains P1 1 life — `controller: You` resolves relative
          // to that Emissary's controller.
    );
    outcome.assert_life_delta(
        P0,
        0, // P0's Emissary must NOT fire — the creature is controlled by P1, not
          // P0 ('another creature YOU control').
    );
}

/// Case B — another creature you control enters as a TOKEN: Virulent Emissary's
/// trigger fires (CR 111.1: a token is a permanent; its battlefield entry is a
/// zone-change matching the ETB trigger).
#[test]
fn virulent_emissary_triggers_on_your_token() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    scenario.add_creature_from_oracle(P0, "Virulent Emissary", 1, 1, VIRULENT_EMISSARY);
    // A sorcery that creates a creature token P0 controls.
    let token_maker = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Soldier Summons",
            false,
            "Create a 1/1 white Soldier creature token.",
        )
        .id();

    let mut runner = scenario.build();
    let battlefield_creatures_before = count_creatures(runner.state(), P0);

    let outcome = runner.cast(token_maker).resolve();

    assert_eq!(
        count_creatures(outcome.state(), P0),
        battlefield_creatures_before + 1,
        "the token-making spell must create exactly one creature token"
    );
    outcome.assert_life_delta(
        P0,
        1, // Virulent Emissary triggers on a token creature you control entering
          // (CR 111.1 + CR 603.6a).
    );
}

/// Case C — an OPPONENT-controlled creature enters: Virulent Emissary's trigger
/// does NOT fire. This is correct rules behavior ("another creature **you**
/// control") and is the behavior most likely mistaken for a bug in issue #434.
#[test]
fn virulent_emissary_does_not_trigger_on_opponent_creature() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Emissary under P0; the opponent (P1) casts a creature.
    scenario.add_creature_from_oracle(P0, "Virulent Emissary", 1, 1, VIRULENT_EMISSARY);
    let bear = scenario.add_creature_to_hand(P1, "Grizzly Bear", 2, 2).id();

    let mut runner = scenario.build();
    // Documented escape hatch: make it P1's turn so P1 can cast their creature.
    {
        let state = runner.state_mut();
        state.active_player = P1;
        state.priority_player = P1;
        state.waiting_for = engine::types::game_state::WaitingFor::Priority { player: P1 };
    }

    let outcome = runner.cast(bear).resolve();

    outcome.assert_zone(&[bear], Zone::Battlefield);
    outcome.assert_life_delta(
        P0,
        0, // Virulent Emissary must NOT trigger when an opponent's creature
          // enters — 'another creature you control' excludes opponent ETBs.
    );
}

/// Case D — Virulent Emissary's OWN entry does NOT fire its trigger. The
/// `Another` clause excludes the trigger source itself (CR 603.6a — the
/// newcomer is checked, but "another" filters out the source object).
#[test]
fn virulent_emissary_does_not_trigger_on_its_own_entry() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Virulent Emissary starts in P0's HAND — it will be cast and enter the
    // battlefield itself.
    let emissary = scenario
        .add_creature_to_hand_from_oracle(P0, "Virulent Emissary", 1, 1, VIRULENT_EMISSARY)
        .id();

    let mut runner = scenario.build();
    let outcome = runner.cast(emissary).resolve();

    outcome.assert_zone(&[emissary], Zone::Battlefield);
    outcome.assert_life_delta(
        P0,
        0, // Virulent Emissary's own entry must NOT gain its controller life —
          // the 'another' clause excludes the trigger source itself.
    );
}

/// Count creatures controlled by `player` on the battlefield.
fn count_creatures(state: &engine::types::game_state::GameState, player: PlayerId) -> usize {
    state
        .battlefield
        .iter()
        .filter_map(|id| state.objects.get(id))
        .filter(|o| {
            o.controller == player
                && o.card_types
                    .core_types
                    .contains(&engine::types::card_type::CoreType::Creature)
        })
        .count()
}
