//! When Will You Learn?: exile tops, then cast any number free.
//!
//! Oracle: "When you set this scheme in motion, each opponent exiles the top
//! card of their library. You may cast any number of spells from among cards
//! exiled this way without paying their mana costs."
//!
//! CR 701.32a + CR 904.9: the set-in-motion trigger exiles the tops.
//! CR 608.2c + CR 608.2g: the tail is a resolution-scoped free-cast window
//! over the exiled set — the offer's own decline IS the "may" (no separate
//! OptionalEffectChoice), and each cast spell leaves the window.
//!
//! Revert-to-red: drop the "when" short-name guard — the document gaps
//! (scheme_face asserts; the trigger keyword eats itself). Drop the pool
//! binding — the window offers nothing (candidate assertion fails).

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
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const WHEN_WILL_YOU_LEARN: &str = "When you set this scheme in motion, each opponent exiles the top card of their library. You may cast any number of spells from among cards exiled this way without paying their mana costs.";
const DIVINATION: &str = "Draw two cards.";

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

/// Build the game with Divination on top of P1's library, fire the scheme,
/// and park at precombat main. Returns the runner plus Divination's id.
fn setup_learn() -> (GameRunner, ObjectId) {
    let mut format = FormatConfig::archenemy();
    format.archenemy_player = Some(P0);
    let mut scenario = GameScenario::new_with_format(format, 2, 42);
    scenario.at_phase(Phase::Draw);
    for i in 0..10 {
        scenario.add_card_to_library_top(P0, &format!("P0 Library {i}"));
        scenario.add_card_to_library_top(P1, &format!("P1 Library {i}"));
    }
    let divination = scenario
        .add_spell_to_library_top(P1, "Divination", false)
        .from_oracle_text(DIVINATION)
        .id();
    let mut runner = scenario.build();

    let face = scheme_face("When Will You Learn?", WHEN_WILL_YOU_LEARN);
    let scheme = create_scheme_deck_card(runner.state_mut(), &face, P0);
    runner.state_mut().archenemy = Some(P0);
    runner.state_mut().active_player = P0;

    drive_to_phase(&mut runner, Phase::PreCombatMain);
    assert!(
        runner.state().command_zone.contains(&scheme) && !runner.state().objects[&scheme].face_down,
        "reach guard: turn 1 must set the scheme in motion"
    );
    (runner, divination)
}

/// Drain the trigger, answering every free-cast offer with `choose` (true =
/// take the single candidate, false = decline). Returns offers seen.
fn resolve_with_offers(runner: &mut GameRunner, choose: bool) -> usize {
    let mut offers = 0;
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
            WaitingFor::CastOffer {
                player,
                kind: CastOfferKind::FreeCastWindow { candidates, .. },
                ..
            } => {
                assert_eq!(player, P0, "the archenemy works the window");
                offers += 1;
                let selection = if choose {
                    candidates.first().copied()
                } else {
                    None
                };
                runner
                    .act(GameAction::FreeCastWindowChoice { selection })
                    .expect("window choice");
            }
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            other => panic!("unexpected prompt resolving the lesson: {other:?}"),
        }
    }
    assert!(
        runner.state().stack.is_empty(),
        "all triggers must have resolved"
    );
    offers
}

#[test]
fn when_will_you_learn_casts_exiled_spell_free() {
    let (mut runner, divination) = setup_learn();
    let p0_before = hand_size(&runner, P0);

    let offers = resolve_with_offers(&mut runner, true);
    assert_eq!(offers, 1, "the exiled Divination must be offered once");
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().objects[&divination].zone,
        Zone::Graveyard,
        "the free cast must resolve to the graveyard"
    );
    assert_eq!(
        hand_size(&runner, P0),
        p0_before + 2,
        "the free Divination must draw two"
    );
    assert!(
        runner.state().stack.is_empty()
            && matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "the chain must resolve back to priority with an empty stack"
    );
}

#[test]
fn when_will_you_learn_decline_leaves_exile() {
    let (mut runner, divination) = setup_learn();
    let p0_before = hand_size(&runner, P0);

    let offers = resolve_with_offers(&mut runner, false);
    assert_eq!(offers, 1, "the window must still offer once");

    assert_eq!(
        runner.state().objects[&divination].zone,
        Zone::Exile,
        "declining must leave Divination exiled"
    );
    assert_eq!(
        hand_size(&runner, P0),
        p0_before,
        "declining must draw nothing"
    );
    assert!(
        runner.state().stack.is_empty()
            && matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "the chain must resolve back to priority with an empty stack"
    );
}
