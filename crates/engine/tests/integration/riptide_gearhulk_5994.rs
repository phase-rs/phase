//! Runtime regression for issue #5994: Riptide Gearhulk's per-opponent ETB target
//! fanout must be OPTIONAL ("up to one target"), so declining a target for an
//! opponent does not fizzle the ability.
//!
//! Riptide's ETB — "When this creature enters, for each opponent, put up to one
//! target nonland permanent that player controls into its owner's library third
//! from the top." — lowers to a per-opponent fanout with
//! `MultiTargetSpec { min: 0, max: PlayerCount { Opponent } }`. Before the parser
//! fix, the "put ..." verb fell through `per_opponent_target_fanout_min`'s
//! "gain control of"-only anchor and each per-opponent target lowered as MANDATORY
//! (`min: 1`); declining one then made the ability fizzle (and the harness's
//! required-slot check panics), which is what these tests would catch on a revert.
//!
//! https://github.com/phase-rs/phase/issues/5994

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;
use engine::types::PlayerId;

/// Riptide Gearhulk's verbatim Oracle text (keywords carried separately).
const RIPTIDE_ORACLE: &str = "Double strike\nProwess\nWhen this creature enters, for each opponent, put up to one target nonland permanent that player controls into its owner's library third from the top.";

const P2: PlayerId = PlayerId(2);

/// {1}{W}{W}{U}{U} funded into the pool so the cast auto-pays and never surfaces
/// a `ManaPayment` window.
fn riptide_mana() -> Vec<ManaUnit> {
    vec![
        ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
        ManaUnit::new(ManaType::White, ObjectId(0), false, vec![]),
        ManaUnit::new(ManaType::White, ObjectId(0), false, vec![]),
        ManaUnit::new(ManaType::Blue, ObjectId(0), false, vec![]),
        ManaUnit::new(ManaType::Blue, ObjectId(0), false, vec![]),
    ]
}

/// CR 601.2c (issue #5994): declining EVERY per-opponent target must resolve the
/// ability, not fizzle. In a three-player game (two opponents) with a nonland
/// permanent under each opponent, casting Riptide and declining both optional
/// per-opponent targets leaves both permanents on the battlefield and resolves
/// cleanly. If the `min` lowering reverted to 1, the per-opponent object slot
/// would be REQUIRED and the harness could not decline it (a required-slot panic).
#[test]
fn riptide_declining_all_per_opponent_targets_does_not_fizzle() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, riptide_mana());

    let p1_perm = scenario.add_creature(P1, "Opp1 Bear", 2, 2).id();
    let p2_perm = scenario.add_creature(P2, "Opp2 Bear", 2, 2).id();
    let riptide = scenario
        .add_creature_to_hand_from_oracle(P0, "Riptide Gearhulk", 4, 4, RIPTIDE_ORACLE)
        .id();

    let mut runner = scenario.build();

    // Declare no object targets: every optional per-opponent object slot is
    // declined. (Player intent is offered so player slots, if any, are satisfied.)
    let outcome = runner.cast(riptide).target_players(&[P1, P2]).resolve();

    // CR 601.2c: the ability must actually RESOLVE (return priority), not stall
    // mid-resolution. This is the discriminating check: a mandatory (`min: 1`)
    // lowering does not produce declinable per-opponent target slots — it halts at
    // a non-targeted `EffectZoneChoice` and never reaches this Priority window, so
    // the assertion fails on a revert.
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "declining every per-opponent target must resolve the ETB back to priority, \
         got {:?}",
        outcome.final_waiting_for()
    );
    outcome.assert_zone(&[riptide], Zone::Battlefield);
    outcome.assert_zone(&[p1_perm], Zone::Battlefield);
    outcome.assert_zone(&[p2_perm], Zone::Battlefield);
}

/// CR 601.2c (issue #5994): with two opponents, the controller may choose a target
/// for ONE opponent while DECLINING the (optional) per-opponent target for the
/// other, and the ability still resolves — moving only the chosen permanent. This
/// is the discriminating "one selected, one declined" scenario: a mandatory
/// (`min: 1`) lowering would force a target for the second opponent too.
#[test]
fn riptide_selects_one_opponent_target_and_declines_the_other() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, riptide_mana());
    // Give P1 a library card so the moved permanent's "third from the top"
    // placement has somewhere to sit; P2's permanent is untouched anyway.
    scenario.add_card_to_library_top(P1, "P1 Library Card");

    let p1_perm = scenario.add_creature(P1, "Opp1 Bear", 2, 2).id();
    let p2_perm = scenario.add_creature(P2, "Opp2 Bear", 2, 2).id();
    let riptide = scenario
        .add_creature_to_hand_from_oracle(P0, "Riptide Gearhulk", 4, 4, RIPTIDE_ORACLE)
        .id();

    let mut runner = scenario.build();

    // Choose P1's permanent for P1's slot; declare no target for P2 → P2's
    // optional per-opponent slot is declined.
    let outcome = runner
        .cast(riptide)
        .target_players(&[P1, P2])
        .target_objects(&[p1_perm])
        .resolve();

    outcome.assert_zone(&[riptide], Zone::Battlefield);
    // The chosen permanent left the battlefield for its owner's library.
    outcome.assert_zone(&[p1_perm], Zone::Library);
    // The declined opponent's permanent is untouched.
    outcome.assert_zone(&[p2_perm], Zone::Battlefield);
}

/// Issue #6565: "for each opponent, put up to one target nonland permanent THAT
/// PLAYER controls ..." scopes each per-opponent target to the iterated
/// opponent's permanents — never the caster's own. Offering the caster's own
/// permanent as the sole target intent must leave it untouched (it is not a legal
/// target for the opponent's slot), and the ability still resolves.
#[test]
fn riptide_per_opponent_slot_excludes_the_casters_own_permanents() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, riptide_mana());

    let my_perm = scenario.add_creature(P0, "My Own Bear", 2, 2).id();
    let opp_perm = scenario.add_creature(P1, "Opp Bear", 2, 2).id();
    let riptide = scenario
        .add_creature_to_hand_from_oracle(P0, "Riptide Gearhulk", 4, 4, RIPTIDE_ORACLE)
        .id();

    let mut runner = scenario.build();

    // Offer ONLY the caster's own permanent as target intent. It is not a legal
    // target for the opponent's per-opponent slot, so that slot is declined.
    let outcome = runner
        .cast(riptide)
        .target_players(&[P1])
        .target_objects(&[my_perm])
        .resolve();

    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "the ETB must resolve to priority, got {:?}",
        outcome.final_waiting_for()
    );
    // The caster's own permanent is not controlled by the opponent, so it can
    // never be chosen for the opponent's slot — it stays on the battlefield.
    outcome.assert_zone(&[my_perm], Zone::Battlefield);
    outcome.assert_zone(&[opp_perm], Zone::Battlefield);
}

/// Issue #6565: each opponent's own permanent is legal only for that opponent's
/// per-opponent slot. Selecting both opponents independently moves both targets
/// to their owners' libraries while the caster's own permanent remains untouched.
#[test]
fn riptide_per_opponent_slots_target_each_opponents_permanent() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, riptide_mana());
    scenario.add_card_to_library_top(P1, "P1 Library Card");
    scenario.add_card_to_library_top(P2, "P2 Library Card");

    let my_perm = scenario.add_creature(P0, "My Own Bear", 2, 2).id();
    let p1_perm = scenario.add_creature(P1, "Opp1 Bear", 2, 2).id();
    let p2_perm = scenario.add_creature(P2, "Opp2 Bear", 2, 2).id();
    let riptide = scenario
        .add_creature_to_hand_from_oracle(P0, "Riptide Gearhulk", 4, 4, RIPTIDE_ORACLE)
        .id();

    let mut runner = scenario.build();

    let outcome = runner
        .cast(riptide)
        .target_players(&[P1, P2])
        .target_objects(&[p1_perm, p2_perm])
        .resolve();

    outcome.assert_zone(&[p1_perm, p2_perm], Zone::Library);
    outcome.assert_zone(&[my_perm], Zone::Battlefield);
    outcome.assert_zone(&[riptide], Zone::Battlefield);
}
