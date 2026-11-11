//! CR 701.50b + CR 701.50f: Raffine's Informant ("When this creature enters, it
//! connives.") leaves the battlefield before its own trigger resolves. The
//! connive still happens, with the departed permanent's last known information
//! naming the object that connived and the player who controlled it.
//!
//! The Informant is owned by P1 and entered under P0's control (Reanimate), so
//! the graveyard card's controller (its owner, P1) and its last controller on
//! the battlefield (P0) differ. P0's Leader, Super-Genius replacement reads "a
//! creature you control" against that last known controller.

use engine::game::game_object::ObjectIncarnationRefExt;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::EffectKind;
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::{ObjectId, ObjectIncarnationRef};
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const RAFFINES_INFORMANT: &str = "When this creature enters, it connives. (Draw a card, then discard a card. If you discarded a nonland card, put a +1/+1 counter on this creature.)";
const LEADER_SUPER_GENIUS: &str = "If a creature you control would connive, instead you draw a card, then that creature connives.\nAt the beginning of combat on your turn, target creature you control connives. (Draw a card, then discard a card. If you discarded a nonland card, put a +1/+1 counter on that creature.)";
const REANIMATE: &str = "Put target creature card from a graveyard onto the battlefield under your control. You lose life equal to that card's mana value.";
const MURDER: &str = "Destroy target creature.";

fn library_len(runner: &GameRunner, player: PlayerId) -> usize {
    runner.state().players[player.0 as usize].library.len()
}

/// Pass priority until `spell` has resolved, leaving anything it put on the
/// stack (its triggers) unresolved.
fn resolve_spell(runner: &mut GameRunner, spell: ObjectId) {
    for _ in 0..8 {
        if !runner
            .state()
            .stack
            .iter()
            .any(|entry| entry.source_id == spell)
        {
            return;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("priority passes");
    }
    panic!("{spell:?} did not resolve");
}

fn hand(runner: &GameRunner, player: PlayerId) -> Vec<ObjectId> {
    runner.state().players[player.0 as usize]
        .hand
        .iter()
        .copied()
        .collect()
}

/// CR 701.50b + CR 701.50f: the departed Informant's last controller draws for
/// Leader's replacement and for the connive itself, then discards, and the
/// completed connive names the battlefield incarnation that left, not the card
/// now in P1's graveyard.
#[test]
fn raffine_informant_connives_with_last_known_controller_after_leaving() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Draw one", "Draw two", "Draw three"]);
    scenario.with_library_top(P1, &["P1 card"]);
    scenario
        .add_creature_from_oracle(P0, "Leader, Super-Genius", 2, 3, LEADER_SUPER_GENIUS)
        .as_legendary();
    let informant = scenario
        .add_creature_to_graveyard(P1, "Raffine's Informant", 2, 1)
        .from_oracle_text(RAFFINES_INFORMANT)
        .id();
    let reanimate = scenario
        .add_spell_to_hand_from_oracle(P0, "Reanimate", false, REANIMATE)
        .with_mana_cost(ManaCost::zero())
        .id();
    let murder = scenario
        .add_spell_to_hand_from_oracle(P0, "Murder", true, MURDER)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();

    runner.cast(reanimate).target_object(informant).commit();
    resolve_spell(&mut runner, reanimate);
    assert_eq!(runner.state().objects[&informant].zone, Zone::Battlefield);
    assert_eq!(runner.state().objects[&informant].controller, P0);
    assert_eq!(runner.state().objects[&informant].owner, P1);
    assert!(
        runner
            .state()
            .stack
            .iter()
            .any(|entry| entry.source_id == informant),
        "the Informant's enter trigger waits on the stack"
    );
    let departed = ObjectIncarnationRef::from_object(&runner.state().objects[&informant]);

    // In response, the Informant is destroyed: it is now P1's graveyard card.
    runner.cast(murder).target_object(informant).commit();
    resolve_spell(&mut runner, murder);
    assert_eq!(runner.state().objects[&informant].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&informant].controller, P1);
    assert_ne!(
        ObjectIncarnationRef::from_object(&runner.state().objects[&informant]),
        departed,
        "the graveyard card is a new object (CR 400.7)"
    );

    let p0_library = library_len(&runner, P0);
    let p1_library = library_len(&runner, P1);
    runner.resolve_top();
    let WaitingFor::ConniveDiscard {
        player,
        conniver,
        cards,
        count,
        ..
    } = runner.state().waiting_for.clone()
    else {
        panic!(
            "the connive reaches its discard: {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(player, P0, "CR 701.50a + CR 701.50b: P0 discards");
    assert_eq!(count, 1);
    assert_eq!(conniver.identity(), departed);
    assert_eq!(
        p0_library - library_len(&runner, P0),
        2,
        "Leader's replacement draws for P0, then the connive draws once"
    );
    assert_eq!(library_len(&runner, P1), p1_library, "P1 never draws");
    assert_eq!(cards, hand(&runner, P0));
    let discarded = cards[0];
    let events = runner
        .act(GameAction::SelectCards {
            cards: vec![discarded],
        })
        .expect("P0 discards one card")
        .events;

    assert_eq!(runner.state().objects[&discarded].zone, Zone::Graveyard);
    let completed: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            GameEvent::EffectResolved {
                kind: EffectKind::Connive,
                source_id,
                subject,
            } => Some((*source_id, subject.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(completed.len(), 1, "the connive completes exactly once");
    let (source_id, subject) = &completed[0];
    assert_eq!(*source_id, informant);
    let subject = subject
        .as_ref()
        .expect("the completed connive names its subject");
    assert_eq!(
        subject.identity, departed,
        "CR 701.50b: the object that connived is the incarnation that left"
    );
    assert_eq!(
        subject.controller, P0,
        "CR 701.50b: the player who controlled it is its last controller"
    );
    assert!(
        runner.state().objects[&informant].counters.is_empty(),
        "CR 701.50a: the card in the graveyard is not the conniving permanent"
    );
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
}
