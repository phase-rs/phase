//! Dance, Pathetic Marionette: reveal-until, steal one, sweep the rest.
//!
//! Oracle: "When you set this scheme in motion, each opponent reveals cards
//! from the top of their library until they reveal a creature card. Choose
//! one of the revealed creature cards and put it onto the battlefield under
//! your control. Put all other cards revealed this way into their owners'
//! graveyards."
//!
//! CR 701.20a + CR 608.2c: the reveal leaves every card in place (`RevealOnly`)
//! — the non-creatures are not shuffled back (no phantom shuffle: both
//! libraries keep their exact order) and the matches are not parked in hand.
//! CR 608.2d: the controller chooses one revealed CREATURE (the revealed
//! non-creature is never offered). CR 110.2a: the chosen card enters under
//! the controller's control, not its owner's. CR 608.2c: the sweep moves
//! every other revealed card — non-matches AND unchosen matches — to its
//! owner's graveyard.
//!
//! Revert-to-red: drop the reveal-pool choose arm — the document gaps
//! (scheme_face asserts). Drop the lookback guard — the sweep folds into the
//! reveal's rest pile and the unchosen creature strands (graveyard
//! assertions fail). Drop the RevealOnly flip — the libraries shuffle
//! (order assertions fail).

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
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const DANCE_PATHETIC_MARIONETTE: &str = "When you set this scheme in motion, each opponent reveals cards from the top of their library until they reveal a creature card. Choose one of the revealed creature cards and put it onto the battlefield under your control. Put all other cards revealed this way into their owners' graveyards.";

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
                runner.act(GameAction::PassPriority).expect("pass priority");
                runner.act(GameAction::PassPriority).expect("pass priority");
                runner.act(GameAction::PassPriority).expect("pass priority");
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

fn controller_of(runner: &GameRunner, id: ObjectId) -> PlayerId {
    runner.state().objects[&id].controller
}

fn library_of(runner: &GameRunner, player: PlayerId) -> Vec<ObjectId> {
    runner.state().players[player.0 as usize]
        .library
        .iter()
        .cloned()
        .collect()
}

/// The three revealed cards: P1's non-creature, P1's creature, P2's
/// creature.
struct Revealed {
    p1_spell: ObjectId,
    p1_creature: ObjectId,
    p2_creature: ObjectId,
}

/// Build the 3-player game. P1's top two are a spell over a creature;
/// P2's top card is a creature; everything else is filler. Returns the
/// runner parked at precombat main plus the revealed ids.
fn setup_marionette() -> (GameRunner, Revealed) {
    let mut format = FormatConfig::archenemy();
    format.archenemy_player = Some(P0);
    let mut scenario = GameScenario::new_with_format(format, 3, 42);
    scenario.at_phase(Phase::Draw);
    for i in 0..10 {
        scenario.add_card_to_library_top(P0, &format!("P0 Library {i}"));
    }
    for i in 0..5 {
        scenario.add_card_to_library_top(P1, &format!("P1 Filler {i}"));
        scenario.add_card_to_library_top(P2, &format!("P2 Filler {i}"));
    }
    // Each push lands on top: P1's top is the spell, then the creature.
    let p1_creature = scenario.add_card_to_library_top(P1, "P1 Creature");
    let p1_spell = scenario.add_card_to_library_top(P1, "P1 Spell");
    let p2_creature = scenario.add_card_to_library_top(P2, "P2 Creature");
    let mut runner = scenario.build();

    for id in [p1_creature, p2_creature] {
        let obj = runner.state_mut().objects.get_mut(&id).unwrap();
        obj.card_types.core_types = vec![CoreType::Creature];
    }
    // P1's spell stays typeless: not a creature, so the reveal skips it.

    let face = scheme_face("Dance, Pathetic Marionette", DANCE_PATHETIC_MARIONETTE);
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
        Revealed {
            p1_spell,
            p1_creature,
            p2_creature,
        },
    )
}

/// Resolve the scheme, choosing `pick` from the offered pool. Asserts the
/// pool holds exactly the two revealed creatures — never the spell.
fn resolve_picking(runner: &mut GameRunner, revealed: &Revealed, pick: ObjectId) {
    let mut choose_prompts = 0;
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
            WaitingFor::ChooseFromZoneChoice {
                player,
                cards,
                count,
                ..
            } => {
                assert_eq!(player, P0, "the controller chooses");
                assert_eq!(count, 1, "choose exactly one");
                let mut offered = cards.clone();
                offered.sort();
                let mut expected = vec![revealed.p1_creature, revealed.p2_creature];
                expected.sort();
                assert_eq!(
                    offered, expected,
                    "the pool must be exactly the revealed creatures"
                );
                choose_prompts += 1;
                runner
                    .act(GameAction::SelectCards { cards: vec![pick] })
                    .expect("pick must be accepted");
            }
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            other => panic!("unexpected prompt resolving the scheme: {other:?}"),
        }
    }
    assert_eq!(choose_prompts, 1, "the choose must prompt exactly once");
    assert!(
        runner.state().stack.is_empty(),
        "all triggers must have resolved"
    );
}

#[test]
fn marionette_steals_p1_creature_and_sweeps_the_rest() {
    let (mut runner, revealed) = setup_marionette();
    let p1_library_before = library_of(&runner, P1);
    let p2_library_before = library_of(&runner, P2);

    resolve_picking(&mut runner, &revealed, revealed.p1_creature);
    runner.advance_until_stack_empty();

    // The steal: P1's creature enters under P0's control.
    assert_eq!(
        zone_of(&runner, revealed.p1_creature),
        Zone::Battlefield,
        "the chosen creature must be reanimated"
    );
    assert_eq!(
        controller_of(&runner, revealed.p1_creature),
        P0,
        "the chosen creature must enter under your control, not its owner's"
    );
    // The sweep: the spell and the unchosen creature reach owners' yards.
    assert_eq!(
        zone_of(&runner, revealed.p1_spell),
        Zone::Graveyard,
        "the revealed non-creature must be swept"
    );
    assert_eq!(
        zone_of(&runner, revealed.p2_creature),
        Zone::Graveyard,
        "the unchosen revealed creature must be swept, not stranded"
    );
    // No phantom shuffle: both libraries keep their exact filler order.
    let p1_expected: Vec<ObjectId> = p1_library_before
        .into_iter()
        .filter(|id| *id != revealed.p1_spell && *id != revealed.p1_creature)
        .collect();
    assert_eq!(
        library_of(&runner, P1),
        p1_expected,
        "P1's library must keep its order (no shuffle)"
    );
    let p2_expected: Vec<ObjectId> = p2_library_before
        .into_iter()
        .filter(|id| *id != revealed.p2_creature)
        .collect();
    assert_eq!(
        library_of(&runner, P2),
        p2_expected,
        "P2's library must keep its order (no shuffle)"
    );
    assert!(
        runner.state().stack.is_empty()
            && matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "the chain must resolve back to priority with an empty stack"
    );
}

#[test]
fn marionette_steals_p2_creature_and_sweeps_the_rest() {
    let (mut runner, revealed) = setup_marionette();

    resolve_picking(&mut runner, &revealed, revealed.p2_creature);
    runner.advance_until_stack_empty();

    assert_eq!(
        zone_of(&runner, revealed.p2_creature),
        Zone::Battlefield,
        "the chosen creature must be reanimated"
    );
    assert_eq!(
        controller_of(&runner, revealed.p2_creature),
        P0,
        "the chosen creature must enter under your control, not its owner's"
    );
    assert_eq!(
        zone_of(&runner, revealed.p1_creature),
        Zone::Graveyard,
        "the other opponent's creature must be swept when unchosen"
    );
    assert_eq!(
        zone_of(&runner, revealed.p1_spell),
        Zone::Graveyard,
        "the revealed non-creature must be swept"
    );
    assert!(
        runner.state().stack.is_empty()
            && matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "the chain must resolve back to priority with an empty stack"
    );
}
