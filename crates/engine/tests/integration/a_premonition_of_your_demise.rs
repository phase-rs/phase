//! A Premonition of Your Demise: reveal two, reflexive damage for their total MV.
//!
//! Oracle: "When you set this scheme in motion, reveal the top two cards of
//! your library and put them into your hand. When you reveal one or more
//! nonland cards this way, this scheme deals damage equal to their total mana
//! value to any target."
//!
//! CR 701.32a + CR 904.9: the set-in-motion trigger fires on turn 1.
//! CR 603.12 + CR 701.20a: the reflexive fires only when the reveal includes
//! a nonland — two lands mean no trigger at all (no target prompt, no
//! damage). CR 202.3a: lands have MV 0, so a mixed reveal deals exactly the
//! spells' total.
//!
//! Revert-to-red: drop the reveal-top count gate — the document gaps
//! (scheme_face asserts). Drop the aggregate re-anchor — the damage is 0
//! (delta assertions fail). Drop the gate's nonland filter — the two-land leg
//! prompts (prompt-count fails).

use engine::database::synthesis::synthesize_archenemy;
use engine::game::deck_loading::create_scheme_deck_card;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::triggers::drain_order_triggers_with_identity;
use engine::parser::parse_oracle_text;
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::card::CardFace;
use engine::types::card_type::CoreType;
use engine::types::format::FormatConfig;
use engine::types::game_state::WaitingFor;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const A_PREMONITION_OF_YOUR_DEMISE: &str = "When you set this scheme in motion, reveal the top two cards of your library and put them into your hand. When you reveal one or more nonland cards this way, this scheme deals damage equal to their total mana value to any target.";

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
    for _ in 0..64 {
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

fn hand_size(runner: &GameRunner, player: PlayerId) -> usize {
    runner
        .state()
        .objects
        .values()
        .filter(|obj| obj.zone == Zone::Hand && obj.owner == player)
        .count()
}

fn life(runner: &GameRunner, player: PlayerId) -> i32 {
    runner.state().players[player.0 as usize].life
}

/// A top-of-library card: `mv` mana value, land-typed when `land`.
struct TopCard {
    mv: u32,
    land: bool,
}

/// Build the 3-player game with P0's library topped (after one draw-step
/// filler) by `top` then `second`: eight filler, the second card, the top
/// card, then the drawn filler — each push lands on top. Returns the runner
/// parked at precombat main with the scheme face-up.
fn setup_premonition(top: TopCard, second: TopCard) -> GameRunner {
    let mut format = FormatConfig::archenemy();
    format.archenemy_player = Some(P0);
    let mut scenario = GameScenario::new_with_format(format, 3, 42);
    scenario.at_phase(Phase::Draw);
    for i in 0..10 {
        scenario.add_card_to_library_top(P1, &format!("P1 Library {i}"));
        scenario.add_card_to_library_top(P2, &format!("P2 Library {i}"));
    }
    for i in 0..8 {
        scenario.add_card_to_library_top(P0, &format!("P0 Filler {i}"));
    }
    let second_id = scenario.add_card_to_library_top(P0, "P0 Second");
    let top_id = scenario.add_card_to_library_top(P0, "P0 Top");
    let _drawn_id = scenario.add_card_to_library_top(P0, "P0 Drawn Filler");
    let mut runner = scenario.build();

    for (id, spec) in [(top_id, &top), (second_id, &second)] {
        let obj = runner.state_mut().objects.get_mut(&id).unwrap();
        obj.mana_cost = ManaCost::generic(spec.mv);
        if spec.land {
            obj.card_types.core_types = vec![CoreType::Land];
        }
    }

    let face = scheme_face("A Premonition of Your Demise", A_PREMONITION_OF_YOUR_DEMISE);
    let scheme = create_scheme_deck_card(runner.state_mut(), &face, P0);
    runner.state_mut().archenemy = Some(P0);
    runner.state_mut().active_player = P0;

    drive_to_phase(&mut runner, Phase::PreCombatMain);
    assert!(
        runner.state().command_zone.contains(&scheme) && !runner.state().objects[&scheme].face_down,
        "reach guard: turn 1 must set the scheme in motion"
    );
    assert_eq!(
        hand_size(&runner, P0),
        1,
        "reach guard: P0 must have drawn only the filler"
    );
    runner
}

/// Resolve the scheme, sending the reflexive's damage at P1 when it fires.
/// Returns the number of target-selection prompts seen.
fn resolve_aiming_p1(runner: &mut GameRunner) -> usize {
    let mut target_prompts = 0;
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
            WaitingFor::TriggerTargetSelection {
                target_slots,
                selection,
                ..
            } => {
                let slot = &target_slots[selection.current_slot];
                assert!(
                    slot.legal_targets.contains(&TargetRef::Player(P1)),
                    "any target must offer the opponent, got {:?}",
                    slot.legal_targets
                );
                target_prompts += 1;
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(TargetRef::Player(P1)),
                    })
                    .expect("choose target must be accepted");
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
    target_prompts
}

#[test]
fn premonition_two_spells_deals_their_total() {
    let mut runner = setup_premonition(
        TopCard { mv: 2, land: false },
        TopCard { mv: 3, land: false },
    );
    let p1_before = life(&runner, P1);

    let target_prompts = resolve_aiming_p1(&mut runner);
    assert_eq!(
        target_prompts, 1,
        "the reflexive must prompt for any target"
    );
    runner.advance_until_stack_empty();

    assert_eq!(
        p1_before - life(&runner, P1),
        5,
        "two spells of MV 2 + 3 must deal 5"
    );
    assert_eq!(
        hand_size(&runner, P0),
        3,
        "the filler plus both revealed cards must be in hand"
    );
    assert!(
        runner.state().stack.is_empty()
            && matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "the chain must resolve back to priority with an empty stack"
    );
}

#[test]
fn premonition_land_plus_spell_deals_the_spell_only() {
    let mut runner = setup_premonition(
        TopCard { mv: 0, land: true },
        TopCard { mv: 4, land: false },
    );
    let p1_before = life(&runner, P1);

    let target_prompts = resolve_aiming_p1(&mut runner);
    assert_eq!(target_prompts, 1, "one nonland revealed must still fire");
    runner.advance_until_stack_empty();

    assert_eq!(
        p1_before - life(&runner, P1),
        4,
        "a land plus an MV-4 spell must deal exactly 4"
    );
    assert_eq!(
        hand_size(&runner, P0),
        3,
        "the filler plus both revealed cards must be in hand"
    );
    assert!(
        runner.state().stack.is_empty()
            && matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "the chain must resolve back to priority with an empty stack"
    );
}

#[test]
fn premonition_two_lands_fires_no_trigger() {
    let mut runner =
        setup_premonition(TopCard { mv: 0, land: true }, TopCard { mv: 0, land: true });
    let p1_before = life(&runner, P1);

    let target_prompts = resolve_aiming_p1(&mut runner);
    assert_eq!(
        target_prompts, 0,
        "no nonland revealed must mean no reflexive and no prompt"
    );
    runner.advance_until_stack_empty();

    assert_eq!(
        life(&runner, P1),
        p1_before,
        "no trigger must mean no damage"
    );
    assert_eq!(
        hand_size(&runner, P0),
        3,
        "the reveal still happens: filler plus both lands in hand"
    );
    assert!(
        runner.state().stack.is_empty()
            && matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "the chain must resolve back to priority with an empty stack"
    );
}
