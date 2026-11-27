//! Power Without Equal: draw three, no max hand, conditional free casts.
//!
//! Oracle: "When you set this scheme in motion, draw three cards. You have
//! no maximum hand size until your next turn. If you control six or more
//! lands, you may cast up to three spells from your hand without paying
//! their mana costs."
//!
//! CR 608.2g + CR 601.2: the bare-noun "up to three spells" opens a
//! free-cast window over any castable face in hand — creatures are
//! offered, the land is not (a land is no spell under the Cast policy),
//! and the window closes after the third cast. CR 608.2c: with fewer
//! than six lands the window never opens, but the draw and the emblem
//! still resolve. CR 514.1: with the emblem out, cleanup discards nothing.
//!
//! Revert-to-red: drop the bare-noun filter arm — the document gaps
//! (scheme_face asserts). Restrict the filter to instants/sorceries —
//! no creature is ever offered (offer assertions fail). Drop the land
//! gate — the short-lands game opens a window it must not.

use engine::database::synthesis::synthesize_archenemy;
use engine::game::deck_loading::create_scheme_deck_card;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::triggers::drain_order_triggers_with_identity;
use engine::parser::parse_oracle_text;
use engine::types::actions::GameAction;
use engine::types::card::CardFace;
use engine::types::card_type::CoreType;
use engine::types::format::FormatConfig;
use engine::types::game_state::{CastOfferKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaColor;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const POWER_WITHOUT_EQUAL: &str = "When you set this scheme in motion, draw three cards. You have no maximum hand size until your next turn. If you control six or more lands, you may cast up to three spells from your hand without paying their mana costs.";

const P2: PlayerId = PlayerId(2);

fn scheme_face(name: &str, oracle: &str) -> CardFace {
    let parsed = parse_oracle_text(oracle, name, &[], &["Scheme".to_string()], &[]);
    assert!(
        !format!("{parsed:?}").contains("Unimplemented"),
        "{name}: document is gapped"
    );
    let mut face = CardFace {
        name: name.to_string(),
        triggers: parsed.triggers,
        static_abilities: parsed.statics,
        ..Default::default()
    };
    face.card_type.core_types.push(CoreType::Scheme);
    synthesize_archenemy(&mut face);
    let gaps = engine::game::coverage::card_face_gaps(&face);
    assert!(gaps.is_empty(), "{name}: gaps: {gaps:?}");
    face
}

/// Drive to `target`, declaring no attackers/blockers through combat and
/// passing priority. Panics on any other prompt.
fn drive_to_phase(runner: &mut GameRunner, target: Phase) {
    for _ in 0..128 {
        runner.advance_to_phase(target);
        if runner.state().phase == target {
            return;
        }
        match runner.state().waiting_for.clone() {
            WaitingFor::DeclareAttackers { .. } => {
                runner.declare_attackers(&[]).expect("declare no attackers");
            }
            WaitingFor::DeclareBlockers { .. } => {
                runner.declare_blockers(&[]).expect("declare no blockers");
            }
            WaitingFor::Priority { .. } => {
                runner
                    .pass_holding_team()
                    .expect("holding team passes together");
            }
            WaitingFor::OrderTriggers { .. } => {
                drain_order_triggers_with_identity(runner.state_mut());
            }
            other => panic!("unexpected prompt driving to {target:?}: {other:?}"),
        }
    }
    panic!("never reached {target:?}");
}

fn zone_of(runner: &GameRunner, id: ObjectId) -> Zone {
    runner.state().objects[&id].zone
}

fn hand_of(runner: &GameRunner, player: PlayerId) -> Vec<ObjectId> {
    runner.state().players[player.0 as usize]
        .hand
        .iter()
        .cloned()
        .collect()
}

struct PowerBoard {
    creatures: Vec<ObjectId>,
    land: ObjectId,
    scheme: ObjectId,
}

/// Build the 3-player game. P0 holds four 2/2 creatures plus a land, draws
/// from a 10-deep library, and controls `lands` Forests. Returns the runner
/// parked at precombat main with the scheme fired.
fn setup_power(lands: u32) -> (GameRunner, PowerBoard) {
    let mut format = FormatConfig::archenemy();
    format.archenemy_player = Some(P0);
    let mut scenario = GameScenario::new_with_format(format, 3, 42);
    scenario.at_phase(Phase::Draw);
    for i in 0..10 {
        scenario.add_card_to_library_top(P0, &format!("P0 Library {i}"));
        scenario.add_card_to_library_top(P1, &format!("P1 Library {i}"));
        scenario.add_card_to_library_top(P2, &format!("P2 Library {i}"));
    }
    let mut creatures = Vec::new();
    for i in 0..4 {
        creatures.push(
            scenario
                .add_creature_to_hand(P0, &format!("P0 Grizzly {i}"), 2, 2)
                .id(),
        );
    }
    let land = scenario.add_land_to_hand(P0, "P0 Forest").id();
    for _ in 0..lands {
        scenario.add_basic_land(P0, ManaColor::Green);
    }
    let mut runner = scenario.build();

    let face = scheme_face("Power Without Equal", POWER_WITHOUT_EQUAL);
    let scheme = create_scheme_deck_card(runner.state_mut(), &face, P0);
    runner.state_mut().archenemy = Some(P0);
    runner.state_mut().active_player = P0;

    drive_to_phase(&mut runner, Phase::PreCombatMain);
    assert!(
        runner.state().command_zone.contains(&scheme) && !runner.state().objects[&scheme].face_down,
        "reach guard: turn 1 must set the scheme in motion"
    );
    (
        runner,
        PowerBoard {
            creatures,
            land,
            scheme,
        },
    )
}

/// The free-cast window prompt, destructured.
fn window_offer(waiting: &WaitingFor) -> (Vec<ObjectId>, Option<u8>) {
    match waiting {
        WaitingFor::CastOffer {
            kind:
                CastOfferKind::FreeCastWindow {
                    candidates,
                    remaining_casts,
                    ..
                },
            ..
        } => (candidates.clone(), *remaining_casts),
        other => panic!("expected a FreeCastWindow offer, got {other:?}"),
    }
}

/// Resolve the scheme, accepting the first `accepts` window offers (each
/// for the next scripted creature still in hand) and declining the rest.
/// Returns the number of window offers seen. Panics on any other prompt.
fn resolve_power(runner: &mut GameRunner, board: &PowerBoard, accepts: usize) -> u32 {
    let mut offers = 0;
    let mut accepted = 0;
    for _ in 0..64 {
        if runner.state().stack.is_empty()
            && matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
        {
            break;
        }
        match runner.state().waiting_for.clone() {
            WaitingFor::OrderTriggers { .. } => {
                drain_order_triggers_with_identity(runner.state_mut());
            }
            WaitingFor::CastOffer { .. } => {
                let (candidates, _remaining) = window_offer(&runner.state().waiting_for);
                if offers == 0 {
                    // Reach guard on the FIRST offer only: every creature is
                    // offered and the land never is (a land is no spell under
                    // the Cast policy). The drawn filler IS offered — the
                    // typeless fixture card reads castable, an artifact of
                    // the scenario builder, never a printed shape — so the
                    // tight bound is all-but-the-land.
                    for creature in &board.creatures {
                        assert!(
                            candidates.contains(creature),
                            "the window must offer every castable creature, missing {creature:?} in {candidates:?}"
                        );
                    }
                    assert!(
                        !candidates.contains(&board.land),
                        "the window must not offer the land: {candidates:?}"
                    );
                    assert_eq!(
                        candidates.len(),
                        hand_of(runner, P0).len() - 1,
                        "everything but the land is offered: {candidates:?}"
                    );
                }
                offers += 1;
                let pick = if accepted < accepts {
                    board
                        .creatures
                        .iter()
                        .find(|id| candidates.contains(id) && zone_of(runner, **id) == Zone::Hand)
                        .copied()
                } else {
                    None
                };
                if pick.is_some() {
                    accepted += 1;
                }
                runner
                    .act(GameAction::FreeCastWindowChoice { selection: pick })
                    .expect("window choice must be accepted");
            }
            WaitingFor::Priority { .. } => {
                runner
                    .pass_holding_team()
                    .expect("holding team passes together");
            }
            other => panic!("unexpected prompt resolving the scheme: {other:?}"),
        }
    }
    assert!(
        runner.state().stack.is_empty(),
        "all triggers must have resolved"
    );
    offers
}

/// The no-max-hand emblem the scheme created (any command-zone object but
/// the scheme itself carrying the mode).
fn emblem_present(runner: &GameRunner, board: &PowerBoard) -> bool {
    runner
        .state()
        .command_zone
        .iter()
        .filter(|id| **id != board.scheme)
        .filter_map(|id| runner.state().objects.get(id))
        .flat_map(|obj| obj.static_definitions.iter_all())
        .any(|s| format!("{:?}", s.mode).contains("NoMaximumHandSize"))
}

#[test]
fn power_casts_two_stops_early_and_lifts_hand_size() {
    let (mut runner, board) = setup_power(6);
    let hand_before = hand_of(&runner, P0).len();

    let offers = resolve_power(&mut runner, &board, 2);
    runner.advance_until_stack_empty();

    // Draw three, cast two.
    assert_eq!(
        hand_of(&runner, P0).len(),
        hand_before + 3 - 2,
        "draw three, cast two"
    );
    assert_eq!(offers, 3, "two accepts then the declined third offer");
    assert_eq!(zone_of(&runner, board.creatures[0]), Zone::Battlefield);
    assert_eq!(zone_of(&runner, board.creatures[1]), Zone::Battlefield);
    assert_eq!(zone_of(&runner, board.creatures[2]), Zone::Hand);
    assert_eq!(zone_of(&runner, board.creatures[3]), Zone::Hand);
    assert_eq!(zone_of(&runner, board.land), Zone::Hand);
    assert!(emblem_present(&runner, &board), "the hand-size emblem");
    assert!(
        runner.state().stack.is_empty()
            && matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "the chain must resolve back to priority with an empty stack"
    );
}

#[test]
fn power_window_closes_after_three_casts() {
    let (mut runner, board) = setup_power(7);

    let offers = resolve_power(&mut runner, &board, 99);
    runner.advance_until_stack_empty();

    assert_eq!(offers, 3, "the printed cap closes the window");
    assert_eq!(zone_of(&runner, board.creatures[0]), Zone::Battlefield);
    assert_eq!(zone_of(&runner, board.creatures[1]), Zone::Battlefield);
    assert_eq!(zone_of(&runner, board.creatures[2]), Zone::Battlefield);
    assert_eq!(
        zone_of(&runner, board.creatures[3]),
        Zone::Hand,
        "the fourth creature is never offered"
    );
    assert!(emblem_present(&runner, &board), "the hand-size emblem");
    assert!(
        runner.state().stack.is_empty()
            && matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "the chain must resolve back to priority with an empty stack"
    );
}

#[test]
fn power_short_on_lands_draws_and_lifts_but_never_opens_a_window() {
    let (mut runner, board) = setup_power(5);
    let hand_before = hand_of(&runner, P0).len();

    let offers = resolve_power(&mut runner, &board, 99);
    runner.advance_until_stack_empty();

    assert_eq!(offers, 0, "no window below six lands");
    assert_eq!(
        hand_of(&runner, P0).len(),
        hand_before + 3,
        "the draw still resolves"
    );
    for creature in &board.creatures {
        assert_eq!(zone_of(&runner, *creature), Zone::Hand);
    }
    assert!(emblem_present(&runner, &board), "the emblem still resolves");
    assert!(
        runner.state().stack.is_empty()
            && matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "the chain must resolve back to priority with an empty stack"
    );
}

#[test]
fn power_keeps_full_hand_through_cleanup() {
    let (mut runner, board) = setup_power(6);

    let hand_before = hand_of(&runner, P0).len();
    let offers = resolve_power(&mut runner, &board, 0);
    runner.advance_until_stack_empty();
    assert_eq!(offers, 1, "the window opens and is declined outright");
    assert_eq!(
        hand_of(&runner, P0).len(),
        hand_before + 3,
        "draw three, cast none"
    );

    // Cleanup auto-passes with no stable stop, so observe its aftermath at
    // turn-2 upkeep — before the turn-2 draw and the recycled scheme. A
    // discard prompt anywhere in the drive panics-fails, which is the
    // assertion: with the emblem out there must be none.
    drive_to_phase(&mut runner, Phase::Upkeep);
    assert_eq!(
        hand_of(&runner, P0).len(),
        hand_before + 3,
        "no maximum hand size: cleanup discards nothing"
    );
    assert!(
        hand_of(&runner, P0).contains(&board.land),
        "the land is still there"
    );
}
