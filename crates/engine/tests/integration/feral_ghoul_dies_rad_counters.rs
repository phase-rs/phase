//! Regression tests for Feral Ghoul (Fallout / PIP):
//! "When this creature dies, each opponent gets a number of rad counters equal
//! to its power."
//!
//! CR 728.1 is the AUTHORIZING rule for rad counters: "Rad counters are a kind
//! of counter a player can have (see rule 122, 'Counters')." CR 122.1: a
//! counter is a marker placed on an object *or player*. CR 122.1i points from
//! the counter chapter into rule 728.
//!
//! CR 608.2h is the AUTHORIZING rule for the value read: an effect needing
//! information from a specific object "uses the current information of that
//! object if it's in the public zone it was expected to be in; if it's no
//! longer in that zone … the effect uses the object's last known information."
//! CR 113.7a says the same for a source that has left its expected zone. That
//! matters because CR 122.2 makes the +1/+1 counter cease to exist the moment
//! Feral Ghoul changes zones — so the LKI rung, not a live read, is what must
//! supply the buffed power.
//!
//! CR 608.2c: the controller follows the instructions in the order written.
//! The engine's `player_scope: Some(PlayerFilter::Opponent)` carries "each
//! opponent" on the trigger's `execute`; the runtime driver fans out to each
//! scoped player while the effect's own `target` stays
//! `TargetFilter::Controller`.
//!
//! Revert baseline: before the parser fix, the dies trigger's body lowered to
//! `Effect::Unimplemented { name: "get", description: "get a number of rad
//! counters equal to its power" }` — the trigger fired and did nothing, so
//! every opponent ended on 0 rad counters.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::phase::Phase;
use engine::types::player::{PlayerCounterKind, PlayerId};

/// Verbatim Oracle text (Scryfall + MTGJSON agree byte for byte). A paraphrase
/// can take a different parser branch, so this must stay exact.
const FERAL_GHOUL_ORACLE: &str = "Menace\n\
Whenever another creature you control dies, put a +1/+1 counter on this creature.\n\
When this creature dies, each opponent gets a number of rad counters equal to its power.";

/// CR 608.2h + CR 113.7a + CR 122.2: a 3/3 Feral Ghoul (2/2 base plus a +1/+1
/// counter) dies, and the opponent gets **3** rad counters — the buffed power
/// read from last known information, not the printed 2 and not 0.
///
/// Revert-failing: without the postfix count arm the trigger body lowers to
/// `Effect::Unimplemented` and P1 ends on 0.
#[test]
fn feral_ghoul_dies_gives_opponent_rad_counters_equal_to_its_power() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // A 2/2 with one +1/+1 counter = a 3/3. The counter is placed directly
    // rather than by triggering line 2: a second creature dying in the same
    // combat would put that trigger on the stack *after* Feral Ghoul is already
    // in the graveyard, so the counter would rules-correctly never land.
    let ghoul = scenario
        .add_creature_from_oracle(P0, "Feral Ghoul", 2, 2, FERAL_GHOUL_ORACLE)
        .with_plus_counters(1)
        .id();
    let bolt = scenario.add_bolt_to_hand(P0);

    let mut runner = scenario.build();

    // Vacuity guard: the assertion below is meaningless if P1 already had rads.
    assert_eq!(
        runner.state().players[P1.0 as usize].player_counter(&PlayerCounterKind::Rad),
        0,
        "precondition: P1 starts with no rad counters"
    );

    runner.cast(bolt).target_objects(&[ghoul]).resolve();
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().players[P1.0 as usize].player_counter(&PlayerCounterKind::Rad),
        3,
        "CR 608.2h + CR 113.7a: Feral Ghoul died as a 3/3, so each opponent gets \
         3 rad counters read from its last known information (CR 122.2 — the \
         +1/+1 counter ceases to exist on the zone change, so a live read would \
         be wrong). A regression to the pre-fix parse lowers the trigger to \
         Effect::Unimplemented and leaves P1 on 0."
    );
}

/// The count **scales** with power — it is not a constant. A 5/5 Feral Ghoul
/// gives 5. Together with the test above (which expects 3) this pins the count
/// against both a `Fixed` regression and a base-power (2) read.
#[test]
fn feral_ghoul_rad_count_tracks_its_power_not_a_constant() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // 2/2 + three +1/+1 counters = 5/5, carrying 2 damage already; the 3-damage
    // bolt brings marked damage to 5 >= toughness 5, lethal under SBA.
    let ghoul = scenario
        .add_creature_from_oracle(P0, "Feral Ghoul", 2, 2, FERAL_GHOUL_ORACLE)
        .with_plus_counters(3)
        .with_damage_marked(2)
        .id();
    let bolt = scenario.add_bolt_to_hand(P0);

    let mut runner = scenario.build();
    runner.cast(bolt).target_objects(&[ghoul]).resolve();
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().players[P1.0 as usize].player_counter(&PlayerCounterKind::Rad),
        5,
        "CR 608.2h: the rad count equals the dying creature's power (5), proving \
         the count is the LKI power and not a constant"
    );
}

/// "Each opponent" fans out to EVERY opponent, and the controller gets none.
///
/// This is the first exercise anywhere in the corpus of `player_scope` fan-out
/// combined with a non-`Fixed`, `ObjectScope::Source`-scoped count: the two
/// existing "each opponent gets a <kind> counter" cards (Prologue to Phyresis,
/// Vraska's Fall) are both `Fixed{1}`. The count is re-resolved once per
/// fan-out iteration with the acting controller rebound, so this test is what
/// establishes that the rebind does not perturb the source-scoped LKI read.
///
/// The triple assertion discriminates the three real regression signatures:
///   (a) `player_scope` lost        -> 3 / 0 / 0 (counters go to the controller)
///   (b) `target` rewritten to an opponent-class filter -> 0 / 0 / 0 (no targets,
///       so the resolver no-ops)
///   (c) source LKI read perturbed by the rebind -> P1/P2 read 0 or the base 2
#[test]
fn feral_ghoul_dies_gives_every_opponent_rad_counters() {
    const P2: PlayerId = PlayerId(2);

    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);

    let ghoul = scenario
        .add_creature_from_oracle(P0, "Feral Ghoul", 2, 2, FERAL_GHOUL_ORACLE)
        .with_plus_counters(1)
        .id();
    let bolt = scenario.add_bolt_to_hand(P0);

    let mut runner = scenario.build();

    for p in [P0, P1, P2] {
        assert_eq!(
            runner.state().players[p.0 as usize].player_counter(&PlayerCounterKind::Rad),
            0,
            "precondition: every player starts with no rad counters"
        );
    }

    runner.cast(bolt).target_objects(&[ghoul]).resolve();
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().players[P1.0 as usize].player_counter(&PlayerCounterKind::Rad),
        3,
        "the first opponent gets 3 rad counters"
    );
    assert_eq!(
        runner.state().players[P2.0 as usize].player_counter(&PlayerCounterKind::Rad),
        3,
        "EVERY opponent gets 3 rad counters — the fan-out must reach \
         the second opponent with the same source LKI value"
    );
    assert_eq!(
        runner.state().players[P0.0 as usize].player_counter(&PlayerCounterKind::Rad),
        0,
        "CR 806.1 + CR 102.4 + CR 102.3: in a Free-for-All multiplayer game the \
         players compete as individuals, so it is not a game between teams and \
         CR 102.4 makes \"your team\" mean the same thing as \"you\"; CR 102.3 then \
         makes a player's opponents every player not on that team, i.e. every \
         OTHER player. P1 and P2 are therefore P0's opponents, and P0 is not \
         its own opponent and gets nothing. A lost player_scope would give the \
         controller 3 and the opponents 0."
    );
}
