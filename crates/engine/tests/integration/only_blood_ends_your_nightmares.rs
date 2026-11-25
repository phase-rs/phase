//! Only Blood Ends Your Nightmares: sacrifice edict with a discard decline-tail.
//!
//! Oracle: "When you set this scheme in motion, each opponent sacrifices a
//! creature of their choice. Then each opponent who didn't sacrifice a
//! creature discards two cards."
//!
//! CR 701.32a + CR 904.9: the set-in-motion trigger runs the edict.
//! CR 701.21a + CR 608.2c + CR 109.5: the second sentence is the Kroxa-class
//! decline tail with the sacrifice verb — gated per opponent on
//! `Not(ZoneChangedThisWay)`, so an opponent who sacrificed nothing (no
//! creatures at all) still discards.
//!
//! Revert-to-red: narrow the verb back to discard-only — the document gaps
//! (scheme_face asserts). Drop the decline gate — both opponents discard.
//! Read the gate as "sacrificed at all" instead of per-iteration — the wrong
//! seat discards (prompt-player assertions fail).

use engine::database::synthesis::synthesize_archenemy;
use engine::game::deck_loading::create_scheme_deck_card;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::triggers::drain_order_triggers_with_identity;
use engine::parser::parse_oracle_text;
use engine::types::actions::GameAction;
use engine::types::card::CardFace;
use engine::types::card_type::CoreType;
use engine::types::format::FormatConfig;
use engine::types::game_state::WaitingFor;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const ONLY_BLOOD_ENDS_YOUR_NIGHTMARES: &str = "When you set this scheme in motion, each opponent sacrifices a creature of their choice. Then each opponent who didn't sacrifice a creature discards two cards.";

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

#[test]
fn only_blood_ends_your_nightmares_sacrificers_spared_discard() {
    let mut format = FormatConfig::archenemy();
    format.archenemy_player = Some(P0);
    let mut scenario = GameScenario::new_with_format(format, 3, 42);
    scenario.at_phase(Phase::Draw);
    for i in 0..10 {
        scenario.add_card_to_library_top(P0, &format!("P0 Library {i}"));
        scenario.add_card_to_library_top(P1, &format!("P1 Library {i}"));
        scenario.add_card_to_library_top(P2, &format!("P2 Library {i}"));
    }
    // P1 can sacrifice (two creatures force the interactive choice); P2 has
    // no creatures and must discard instead.
    let tribute_a = scenario.add_creature(P1, "Tribute A", 2, 2).id();
    let tribute_b = scenario.add_creature(P1, "Tribute B", 2, 2).id();
    for i in 0..3 {
        scenario.add_card_to_hand(P1, &format!("P1 Hand {i}"));
        scenario.add_card_to_hand(P2, &format!("P2 Hand {i}"));
    }
    let mut runner = scenario.build();

    let face = scheme_face(
        "Only Blood Ends Your Nightmares",
        ONLY_BLOOD_ENDS_YOUR_NIGHTMARES,
    );
    let scheme = create_scheme_deck_card(runner.state_mut(), &face, P0);
    runner.state_mut().archenemy = Some(P0);
    runner.state_mut().active_player = P0;

    drive_to_phase(&mut runner, Phase::PreCombatMain);
    assert!(
        runner.state().command_zone.contains(&scheme) && !runner.state().objects[&scheme].face_down,
        "reach guard: turn 1 must set the scheme in motion"
    );

    // Resolve the trigger: P1's sacrifice choice, then P2's discard choice.
    // Exactly one prompt of each kind, on the right seats.
    let mut sacrifice_prompts = 0;
    let mut discard_prompts = 0;
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
            WaitingFor::EffectZoneChoice { player, .. } => {
                assert_eq!(player, P1, "only the stocked opponent sacrifices");
                sacrifice_prompts += 1;
                runner
                    .act(GameAction::SelectCards {
                        cards: vec![tribute_a],
                    })
                    .expect("sacrifice choice");
            }
            WaitingFor::DiscardChoice {
                player,
                cards,
                count,
                ..
            } => {
                assert_eq!(player, P2, "only the creatureless opponent discards");
                assert_eq!(count, 2, "the tail discards two cards");
                discard_prompts += 1;
                runner
                    .act(GameAction::SelectCards {
                        cards: cards.into_iter().take(2).collect(),
                    })
                    .expect("discard choice");
            }
            WaitingFor::Priority { .. } => {
                runner
                    .pass_holding_team()
                    .expect("holding team passes together");
            }
            other => panic!("unexpected prompt resolving the edict: {other:?}"),
        }
    }
    assert!(
        runner.state().stack.is_empty(),
        "all triggers must have resolved"
    );
    assert_eq!(sacrifice_prompts, 1, "P1 must be prompted exactly once");
    assert_eq!(discard_prompts, 1, "P2 must be prompted exactly once");

    assert_eq!(
        runner.state().objects[&tribute_a].zone,
        Zone::Graveyard,
        "the chosen tribute must be sacrificed"
    );
    assert_eq!(
        runner.state().objects[&tribute_b].zone,
        Zone::Battlefield,
        "the unchosen creature must remain"
    );
    assert_eq!(hand_size(&runner, P1), 3, "the sacrificer must not discard");
    assert_eq!(
        hand_size(&runner, P2),
        1,
        "the creatureless opponent must discard two"
    );
    assert!(
        runner.state().stack.is_empty()
            && matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "the chain must resolve back to priority with an empty stack"
    );
}
