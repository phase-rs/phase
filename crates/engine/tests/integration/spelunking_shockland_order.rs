//! CR 616.1e/f: a shock land played while a "lands enter untapped" source is on
//! the battlefield must offer the ordering choice, so the player can have the
//! untap effect apply LAST and win.
//!
//! The shock-land class ("As this land enters, you may pay 2 life. If you don't,
//! it enters tapped.") parses as `execute: None` with the enters-tapped write
//! living in `ReplacementMode::MayCost`'s `decline` branch. `candidate_materiality`
//! only walked `execute`, so the candidate classified `Disjoint`, no
//! `enter_tapped` collision with Spelunking was detected, and declining the
//! payment applied the tap unopposed — the land entered tapped with no ordering
//! prompt, contradicting Spelunking's ruling that the player chooses.
//!
//! Goes RED if the decline-branch arm in `candidate_materiality` is reverted.

use engine::game::scenario::{GameScenario, P0};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const SHOCK_LAND: &str =
    "({T}: Add {R} or {G}.)\nAs this land enters, you may pay 2 life. If you don't, it enters tapped.";

/// Plays a shock land under Spelunking, declining the life payment. `untap_last`
/// picks the ordering: when the untap is applied last it must win (CR 616.1f).
/// Returns `(entered_tapped, prompt_rounds, life_paid)`.
fn play_shockland_declining(untap_last: bool) -> (bool, usize, i32) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_enchantment_from_oracle(P0, "Spelunking", "Lands you control enter untapped.");
    let mut builder = scenario.add_land_to_hand(P0, "Stomping Ground");
    builder.from_oracle_text(SHOCK_LAND);
    let land_id = builder.id();

    let mut runner = scenario.build();
    let starting_life = runner.state().players[0].life;
    let card_id = runner.state().objects[&land_id].card_id;
    runner
        .act(GameAction::PlayLand {
            object_id: land_id,
            card_id,
        })
        .expect("play land should succeed");

    let mut rounds = 0;
    while let WaitingFor::ReplacementChoice { candidates, .. } = &runner.state().waiting_for {
        // Decline the "Pay 2 life" branch; in an ordering round, put the shock
        // land's tap first (so Spelunking's untap applies last) or vice versa.
        let labels: Vec<String> = candidates.iter().map(|c| c.description.clone()).collect();
        // Identify the ordering candidates by SOURCE, not by label text: the
        // shock land's ordering label is its full Oracle text, not "Enters
        // tapped" (its tap lives in the decline branch, so
        // `replacement_choice_label` falls back to the description).
        let untap_idx = candidates
            .iter()
            .position(|c| c.source_name == "Spelunking");
        let tap_idx = candidates
            .iter()
            .position(|c| c.source_name == "Stomping Ground");
        let pick = if let Some(i) = labels.iter().position(|d| d == "Decline") {
            i
        } else {
            // CR 616.1f: the effect applied LAST wins, so to make the untap win
            // we select the TAP first.
            let (first, other) = if untap_last {
                (tap_idx, untap_idx)
            } else {
                (untap_idx, tap_idx)
            };
            first.or(other).unwrap_or(0)
        };
        runner
            .act(GameAction::ChooseReplacement { index: pick })
            .expect("replacement choice should succeed");
        rounds += 1;
        assert!(rounds <= 6, "replacement prompt failed to terminate");
    }

    let obj = &runner.state().objects[&land_id];
    assert_eq!(obj.zone, Zone::Battlefield, "the land entered play");
    (
        obj.tapped,
        rounds,
        starting_life - runner.state().players[0].life,
    )
}

#[test]
fn declining_shockland_under_spelunking_can_enter_untapped() {
    let (tapped, rounds, life_paid) = play_shockland_declining(true);

    // CR 616.1: declining the payment must NOT end the decision — the shock
    // land's tap and Spelunking's untap both write `enter_tapped`, so the
    // player is owed the ordering choice.
    assert!(
        rounds >= 2,
        "CR 616.1e: expected a second prompt to order the shock land's tap \
         against Spelunking's untap, got {rounds} round(s)"
    );

    // CR 616.1f: the untap was applied last, so it wins.
    assert!(
        !tapped,
        "CR 616.1f: with Spelunking's untap applied last the land must enter untapped"
    );

    // The payment was declined, so no life was paid.
    assert_eq!(life_paid, 0, "declining the shock payment costs no life");
}

#[test]
fn ordering_the_shockland_tap_last_still_enters_tapped() {
    // The mirror ordering stays reachable — CR 616.1e genuinely offers both
    // outcomes, so this must NOT be "Spelunking always wins".
    let (tapped, _rounds, _life_paid) = play_shockland_declining(false);
    assert!(
        tapped,
        "CR 616.1f: with the shock land's tap applied last the land enters tapped"
    );
}
