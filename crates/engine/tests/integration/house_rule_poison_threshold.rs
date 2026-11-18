//! Host-adjustable individual poison-counter loss threshold
//! (`FormatConfig.poison_counter_threshold`).
//!
//! CR 104.3d / CR 704.5c: a player with ten or more poison counters loses — the
//! magnitude is a house-rule dial, read by the poison SBA and by the HUD's
//! `DerivedViews.poison_loss` through the same `players::poison_loss_measure`.
//! CR 704.6b / CR 810.8d: the Two-Headed Giant family ignores CR 704.5c; its
//! team loses at fifteen shared counters regardless of the dial.
//!
//! Every fixture drives the real cast pipeline with Prologue to Phyresis.
//! Its "Draw a card." is a draw by P0, and `GameScenario` builds every library
//! empty, so CR 704.5b ("attempted to draw a card from a library with no cards
//! in it ... loses the game") would eliminate P0 in the same SBA pass the
//! poison rows measure. Each fixture therefore seeds P0's library with one
//! card per cast, and every reach guard checks that P0 drew it and survived,
//! so a row's elimination or survival can only come from the poison SBA.

use engine::game::derived_views::{derive_views, PoisonLossProximity, PoisonLossView};
use engine::game::players::PoisonLossScope;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::format::FormatConfig;
use engine::types::game_state::GameState;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

/// Verbatim Oracle text (Scryfall), `{1}{U}` Instant.
const PROLOGUE_TO_PHYRESIS_ORACLE: &str = "Each opponent gets a poison counter.\nDraw a card.";

const P2: PlayerId = PlayerId(2);
const P3: PlayerId = PlayerId(3);

/// Fixture contract: P0's library holds one card per planned cast and P0's
/// hand holds that many Prologues; `poison` seeds each listed seat after the
/// build.
fn setup(
    config: FormatConfig,
    player_count: u8,
    casts: usize,
    poison: &[(PlayerId, u32)],
) -> (GameRunner, Vec<ObjectId>) {
    let mut scenario = GameScenario::new_with_format(config, player_count, 7);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &vec!["Plains"; casts]);
    let prologues = (0..casts)
        .map(|_| {
            scenario
                .add_spell_to_hand_from_oracle(
                    P0,
                    "Prologue to Phyresis",
                    true,
                    PROLOGUE_TO_PHYRESIS_ORACLE,
                )
                .id()
        })
        .collect();
    let mut runner = scenario.build();
    for &(player, count) in poison {
        runner.state_mut().players[player.0 as usize].poison_counters = count;
    }
    (runner, prologues)
}

fn commander_with_threshold(threshold: u8) -> FormatConfig {
    FormatConfig {
        poison_counter_threshold: Some(threshold),
        ..FormatConfig::commander()
    }
}

fn poison(state: &GameState, player: PlayerId) -> u32 {
    state.players[player.0 as usize].poison_counters
}

fn eliminated(state: &GameState, player: PlayerId) -> bool {
    state.players[player.0 as usize].is_eliminated
}

/// V1: a Commander table playing to fifteen keeps a player at ten poison in the
/// game, and still eliminates them at fifteen.
#[test]
fn configured_individual_threshold_governs_the_poison_sba() {
    let (mut runner, prologues) = setup(commander_with_threshold(15), 2, 2, &[(P1, 9)]);

    let first = runner.cast(prologues[0]).resolve();
    // Reach guard: the poison counter landed and P0's draw came from a card.
    assert_eq!(poison(first.state(), P1), 10);
    first.assert_hand_drawn(P0, 1);
    assert!(!eliminated(first.state(), P0));
    // CR 704.5c at the house threshold: ten is no longer a loss.
    assert!(
        !eliminated(first.state(), P1),
        "a player at 10 poison must survive a 15-poison house threshold"
    );

    runner.state_mut().players[P1.0 as usize].poison_counters = 14;
    let second = runner.cast(prologues[1]).resolve();
    assert_eq!(poison(second.state(), P1), 15);
    second.assert_hand_drawn(P0, 1);
    assert!(!eliminated(second.state(), P0));
    assert!(
        eliminated(second.state(), P1),
        "a player reaching the house threshold loses (CR 704.5c)"
    );
}

/// V2: the default is CR 704.5c's ten.
#[test]
fn default_individual_threshold_is_ten() {
    assert_eq!(FormatConfig::commander().poison_counter_threshold, Some(10));
    let (mut runner, prologues) = setup(FormatConfig::commander(), 2, 1, &[(P1, 9)]);

    let outcome = runner.cast(prologues[0]).resolve();
    assert_eq!(poison(outcome.state(), P1), 10);
    outcome.assert_hand_drawn(P0, 1);
    assert!(!eliminated(outcome.state(), P0));
    assert!(
        eliminated(outcome.state(), P1),
        "ten poison counters is a loss at the CR default (CR 704.5c)"
    );
}

/// V3: the Two-Headed Giant family has no individual threshold (`None`), and
/// its team rule (CR 704.6b / CR 810.8d) still eliminates at fifteen shared.
#[test]
fn two_headed_giant_team_rule_survives_the_absent_individual_threshold() {
    assert_eq!(
        FormatConfig::two_headed_giant().poison_counter_threshold,
        None
    );

    // P0+P1 vs P2+P3: Prologue gives P2 and P3 one counter each → 7 + 8 = 15.
    let (mut runner, prologues) =
        setup(FormatConfig::two_headed_giant(), 4, 1, &[(P2, 6), (P3, 7)]);
    let outcome = runner.cast(prologues[0]).resolve();
    assert_eq!(poison(outcome.state(), P2), 7);
    assert_eq!(poison(outcome.state(), P3), 8);
    outcome.assert_hand_drawn(P0, 1);
    assert!(!eliminated(outcome.state(), P0));
    assert!(!eliminated(outcome.state(), P1));
    assert!(
        eliminated(outcome.state(), P2) && eliminated(outcome.state(), P3),
        "a team with fifteen shared poison counters loses (CR 704.6b)"
    );

    // Sibling: 7 + 7 = 14 shared → nobody loses.
    let (mut runner, prologues) =
        setup(FormatConfig::two_headed_giant(), 4, 1, &[(P2, 6), (P3, 6)]);
    let outcome = runner.cast(prologues[0]).resolve();
    assert_eq!(poison(outcome.state(), P2), 7);
    assert_eq!(poison(outcome.state(), P3), 7);
    outcome.assert_hand_drawn(P0, 1);
    for seat in [P0, P1, P2, P3] {
        assert!(
            !eliminated(outcome.state(), seat),
            "P{} must not be eliminated at 14 shared poison",
            seat.0
        );
    }
}

/// V8: the derived view states the rule the SBA measured — scope, threshold,
/// and the near-lethal display classification.
#[test]
fn derived_poison_loss_view_states_the_measured_rule() {
    // House threshold 15, P1 at 10: not near lethal.
    let (mut runner, prologues) = setup(commander_with_threshold(15), 2, 1, &[(P1, 9)]);
    let outcome = runner.cast(prologues[0]).resolve();
    outcome.assert_hand_drawn(P0, 1);
    assert!(!eliminated(outcome.state(), P0));
    let views = derive_views(outcome.state(), None);
    assert_eq!(
        views.poison_loss.get(&P1),
        Some(&PoisonLossView {
            scope: PoisonLossScope::Player,
            threshold: 15,
            proximity: PoisonLossProximity::Normal,
        })
    );
    // P0 has no poison: no entry (reach guard: P1's entry is present above).
    assert_eq!(views.poison_loss.get(&P0), None);

    // House threshold 15, P1 at 13: near lethal.
    let (mut runner, prologues) = setup(commander_with_threshold(15), 2, 1, &[(P1, 12)]);
    let outcome = runner.cast(prologues[0]).resolve();
    outcome.assert_hand_drawn(P0, 1);
    assert!(!eliminated(outcome.state(), P0));
    assert_eq!(
        derive_views(outcome.state(), None).poison_loss.get(&P1),
        Some(&PoisonLossView {
            scope: PoisonLossScope::Player,
            threshold: 15,
            proximity: PoisonLossProximity::NearLethal,
        })
    );

    // Default threshold, P1 at 8: near lethal (the badge's pre-dial `>= 8`).
    let (mut runner, prologues) = setup(FormatConfig::commander(), 2, 1, &[(P1, 7)]);
    let outcome = runner.cast(prologues[0]).resolve();
    outcome.assert_hand_drawn(P0, 1);
    assert!(!eliminated(outcome.state(), P0));
    assert_eq!(
        derive_views(outcome.state(), None).poison_loss.get(&P1),
        Some(&PoisonLossView {
            scope: PoisonLossScope::Player,
            threshold: 10,
            proximity: PoisonLossProximity::NearLethal,
        })
    );

    // Two-Headed Giant, 7 + 7 = 14 shared: both opposing seats are measured as
    // a team against fifteen.
    let (mut runner, prologues) =
        setup(FormatConfig::two_headed_giant(), 4, 1, &[(P2, 6), (P3, 6)]);
    let outcome = runner.cast(prologues[0]).resolve();
    outcome.assert_hand_drawn(P0, 1);
    assert!(!eliminated(outcome.state(), P0));
    assert!(!eliminated(outcome.state(), P1));
    let views = derive_views(outcome.state(), None);
    let team_view = Some(&PoisonLossView {
        scope: PoisonLossScope::Team,
        threshold: 15,
        proximity: PoisonLossProximity::NearLethal,
    });
    assert_eq!(views.poison_loss.get(&P2), team_view);
    assert_eq!(views.poison_loss.get(&P3), team_view);
}
