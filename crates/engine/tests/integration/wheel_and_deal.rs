//! Wheel and Deal's targeted-opponent instruction is distinct from its final
//! controller-only draw. The multi-target fan-out must repeat only the former.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::phase::Phase;
use engine::types::PlayerId;

// Verified against Scryfall's Oracle text.
const WHEEL_AND_DEAL: &str =
    "Any number of target opponents each discard their hands, then draw seven cards.\nDraw a card.";

/// CR 601.2c + CR 608.2c: each selected opponent performs the first
/// instruction, then the spell's controller performs its final draw exactly
/// once after all selected opponents are finished.
#[test]
fn wheel_and_deal_fans_out_opponents_without_repeating_controller_draw() {
    let p2 = PlayerId(2);
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);

    let wheel_and_deal = scenario
        .add_spell_to_hand_from_oracle(P0, "Wheel and Deal", true, WHEEL_AND_DEAL)
        .id();
    scenario.with_cards_in_hand(P1, &["P1 discard A", "P1 discard B"]);
    scenario.with_cards_in_hand(p2, &["P2 discard A", "P2 discard B"]);
    scenario.with_library_top(
        P0,
        &[
            "Controller draw 1",
            "Controller draw 2",
            "Controller draw 3",
            "Controller draw 4",
            "Controller draw 5",
            "Controller draw 6",
            "Controller draw 7",
            "Controller draw 8",
        ],
    );
    scenario.with_library_top(
        P1,
        &[
            "P1 draw 1",
            "P1 draw 2",
            "P1 draw 3",
            "P1 draw 4",
            "P1 draw 5",
            "P1 draw 6",
            "P1 draw 7",
        ],
    );
    scenario.with_library_top(
        p2,
        &[
            "P2 draw 1",
            "P2 draw 2",
            "P2 draw 3",
            "P2 draw 4",
            "P2 draw 5",
            "P2 draw 6",
            "P2 draw 7",
        ],
    );

    let mut runner = scenario.build();
    let commit = runner
        .cast(wheel_and_deal)
        .target_players(&[P1, p2])
        .commit();
    let ability = commit
        .state()
        .stack
        .last()
        .and_then(|item| item.ability())
        .expect("Wheel and Deal must be on the stack");
    assert_eq!(
        ability.targets,
        vec![
            engine::types::ability::TargetRef::Player(P1),
            engine::types::ability::TargetRef::Player(p2),
        ],
        "the cast pipeline must preserve both chosen opponents"
    );
    let outcome = commit.resolve();

    outcome.assert_hand_drawn(P0, 1);
    outcome.assert_hand_drawn(P1, 5);
    outcome.assert_hand_drawn(p2, 5);
    assert_eq!(
        outcome
            .state()
            .players
            .iter()
            .find(|player| player.id == P1)
            .expect("P1 exists")
            .graveyard
            .len(),
        2,
        "P1's whole hand must be discarded before drawing seven"
    );
    assert_eq!(
        outcome
            .state()
            .players
            .iter()
            .find(|player| player.id == p2)
            .expect("P2 exists")
            .graveyard
            .len(),
        2,
        "P2's whole hand must be discarded before drawing seven"
    );
}

/// CR 601.2c + CR 608.2c: "Any number" permits no targets. That skips the
/// opponent-qualified instruction while preserving the independent final draw.
#[test]
fn wheel_and_deal_with_no_targets_draws_only_for_its_controller() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let wheel_and_deal = scenario
        .add_spell_to_hand_from_oracle(P0, "Wheel and Deal", true, WHEEL_AND_DEAL)
        .id();
    scenario.with_library_top(P0, &["Controller draw"]);

    let mut runner = scenario.build();
    let outcome = runner.cast(wheel_and_deal).target_players(&[]).resolve();

    outcome.assert_hand_drawn(P0, 1);
    outcome.assert_hand_drawn(P1, 0);
}
