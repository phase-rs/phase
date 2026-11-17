use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const THEORISTS_SANCTUM: &str = "({T}: Add {U}.)\n\
As this land enters, you may behold a Jace. If you don't, this land enters tapped. \
(To behold a Jace, choose a Jace you control or reveal a Jace card from your hand.)\n\
{2}{U}, {T}: Empower Jace 2.";

fn setup(jace_count: usize) -> (GameRunner, ObjectId, Vec<ObjectId>) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let land = scenario
        .add_land_to_hand(P0, "Theorist's Sanctum")
        .from_oracle_text(THEORISTS_SANCTUM)
        .id();
    let jaces = (0..jace_count)
        .map(|index| {
            scenario
                .add_creature_to_hand(P0, &format!("Jace fixture {index}"), 0, 0)
                .as_planeswalker_with_loyalty("Jace", 3)
                .id()
        })
        .collect();
    (scenario.build(), land, jaces)
}

#[test]
fn theorists_sanctum_beholding_a_jace_enters_untapped() {
    let (mut runner, land, jaces) = setup(1);
    let card_id = runner.state().objects[&land].card_id;

    runner
        .act(GameAction::PlayLand {
            object_id: land,
            card_id,
        })
        .expect("playing Theorist's Sanctum must be accepted");
    let WaitingFor::ReplacementChoice { candidates, .. } = &runner.state().waiting_for else {
        panic!(
            "expected the optional Behold replacement choice, got {:?}",
            runner.state().waiting_for
        );
    };
    // CR 701.4a: the accept option names the behold action and its quality.
    assert_eq!(candidates[0].description, "Behold a Jace");

    let result = runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("beholding the only Jace must be accepted");
    assert!(
        result.events.iter().any(|event| matches!(
            event,
            GameEvent::CardsRevealed { card_ids, .. } if card_ids.contains(&jaces[0])
        )),
        "CR 701.4a: the sole Jace card in hand must be revealed"
    );
    assert_eq!(runner.state().objects[&land].zone, Zone::Battlefield);
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    assert!(runner.state().stack.is_empty());
    assert!(
        !runner.state().objects[&land].tapped,
        "CR 614.12a: paying the Behold cost means the land enters untapped"
    );
}

#[test]
fn theorists_sanctum_interactive_behold_resumes_entry_untapped() {
    let (mut runner, land, jaces) = setup(2);
    let card_id = runner.state().objects[&land].card_id;

    runner
        .act(GameAction::PlayLand {
            object_id: land,
            card_id,
        })
        .expect("playing Theorist's Sanctum must be accepted");
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ),
        "expected the optional Behold replacement choice, got {:?}",
        runner.state().waiting_for
    );

    runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("accepting the Behold cost must be accepted");
    let WaitingFor::BeholdChoice { choices, .. } = &runner.state().waiting_for else {
        panic!(
            "two Jaces in hand should require a Behold choice, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(choices.len(), 2, "both Jaces should be legal choices");

    let result = runner
        .act(GameAction::SelectCards {
            cards: vec![jaces[0]],
        })
        .expect("choosing a Jace must resume the entry replacement");
    assert!(
        result.events.iter().any(|event| matches!(
            event,
            GameEvent::CardsRevealed { card_ids, .. } if card_ids.contains(&jaces[0])
        )),
        "CR 701.4a: selecting a Jace card from hand must reveal it"
    );
    assert!(
        result.events.iter().all(|event| !matches!(
            event,
            GameEvent::CardsRevealed { card_ids, .. } if card_ids.contains(&jaces[1])
        )),
        "CR 701.4a: non-selected Jace cards remain unrevealed"
    );
    assert_eq!(runner.state().objects[&land].zone, Zone::Battlefield);
    assert!(
        !runner.state().objects[&land].tapped,
        "CR 614.12a: paying the Behold cost means the land enters untapped"
    );
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    assert!(runner.state().stack.is_empty());
}

#[test]
fn theorists_sanctum_declining_behold_enters_tapped() {
    let (mut runner, land, _) = setup(1);
    let card_id = runner.state().objects[&land].card_id;

    runner
        .act(GameAction::PlayLand {
            object_id: land,
            card_id,
        })
        .expect("playing Theorist's Sanctum must be accepted");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ReplacementChoice { .. }
    ));

    runner
        .act(GameAction::ChooseReplacement { index: 1 })
        .expect("declining the Behold cost must be accepted");
    assert_eq!(runner.state().objects[&land].zone, Zone::Battlefield);
    assert!(
        runner.state().objects[&land].tapped,
        "CR 614.12a: declining the Behold cost taps the entering land"
    );
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    assert!(runner.state().stack.is_empty());
}

#[test]
fn theorists_sanctum_without_a_jace_enters_tapped_without_prompt() {
    let (mut runner, land, _) = setup(0);
    let card_id = runner.state().objects[&land].card_id;

    runner
        .act(GameAction::PlayLand {
            object_id: land,
            card_id,
        })
        .expect("playing Theorist's Sanctum must be accepted");

    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "an unpayable Behold cost must auto-decline without a prompt, got {:?}",
        runner.state().waiting_for
    );
    assert!(runner.state().stack.is_empty());
    assert_eq!(runner.state().objects[&land].zone, Zone::Battlefield);
    assert!(
        runner.state().objects[&land].tapped,
        "CR 118.3 + CR 614.12a: without a legal Jace the cost cannot be paid"
    );
}

const JACE_WIELDER_OF_MYSTERIES: &str =
    "If you would draw a card while your library has no cards in it, you win the game instead.\n\
+1: Target player mills two cards. Draw a card.\n\
\u{2212}8: Draw seven cards. Then if your library has no cards in it, you win the game.";

const JACE_BELEREN: &str = "+2: Each player draws a card.\n\
\u{2212}1: Target player draws a card.\n\
\u{2212}10: Target player mills twenty cards.";

/// Theorist's Sanctum in hand, Jace, Wielder of Mysteries on the battlefield and,
/// optionally, Jace Beleren in hand.
fn setup_with_battlefield_jace(
    hand_jace: bool,
) -> (GameRunner, ObjectId, ObjectId, Option<ObjectId>) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let land = scenario
        .add_land_to_hand(P0, "Theorist's Sanctum")
        .from_oracle_text(THEORISTS_SANCTUM)
        .id();
    let battlefield_jace = scenario
        .add_planeswalker_from_oracle(
            P0,
            "Jace, Wielder of Mysteries",
            "Jace",
            4,
            JACE_WIELDER_OF_MYSTERIES,
        )
        .id();
    let hand_jace = hand_jace.then(|| {
        scenario
            .add_creature_to_hand_from_oracle(P0, "Jace Beleren", 0, 0, JACE_BELEREN)
            .as_planeswalker_with_loyalty("Jace", 3)
            .id()
    });
    (scenario.build(), land, battlefield_jace, hand_jace)
}

fn reveals(events: &[GameEvent]) -> bool {
    events
        .iter()
        .any(|event| matches!(event, GameEvent::CardsRevealed { .. }))
}

/// CR 701.4a: a Jace you control is the sole candidate, so it is auto-chosen; a
/// battlefield permanent is already public, so nothing is revealed.
#[test]
fn theorists_sanctum_sole_battlefield_jace_auto_pays_without_reveal() {
    let (mut runner, land, _, _) = setup_with_battlefield_jace(false);
    let card_id = runner.state().objects[&land].card_id;

    let played = runner
        .act(GameAction::PlayLand {
            object_id: land,
            card_id,
        })
        .expect("playing Theorist's Sanctum must be accepted");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ReplacementChoice { .. }
    ));
    let accepted = runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("beholding the battlefield Jace must be accepted");

    assert!(
        !reveals(&played.events) && !reveals(&accepted.events),
        "CR 701.4a: choosing a Jace you control reveals nothing"
    );
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    assert_eq!(runner.state().objects[&land].zone, Zone::Battlefield);
    assert!(
        !runner.state().objects[&land].tapped,
        "CR 614.12a: paying the Behold cost means the land enters untapped"
    );
}

/// CR 701.4a + CR 608.2d: a Jace on the battlefield plus one in hand is a real
/// choice; picking the battlefield Jace reveals nothing.
#[test]
fn theorists_sanctum_battlefield_and_hand_jace_choose_battlefield_reveals_nothing() {
    let (mut runner, land, battlefield_jace, hand_jace) = setup_with_battlefield_jace(true);
    let hand_jace = hand_jace.expect("hand Jace is staged");
    let card_id = runner.state().objects[&land].card_id;

    runner
        .act(GameAction::PlayLand {
            object_id: land,
            card_id,
        })
        .expect("playing Theorist's Sanctum must be accepted");
    let accepted = runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("accepting the Behold cost must be accepted");
    let WaitingFor::BeholdChoice { choices, .. } = &runner.state().waiting_for else {
        panic!(
            "a battlefield Jace and a hand Jace should require a Behold choice, got {:?}",
            runner.state().waiting_for
        );
    };
    assert!(
        choices.contains(&battlefield_jace) && choices.contains(&hand_jace),
        "both Jaces are legal choices"
    );
    assert!(
        !reveals(&accepted.events),
        "nothing is revealed before the choice"
    );

    let chosen = runner
        .act(GameAction::SelectCards {
            cards: vec![battlefield_jace],
        })
        .expect("choosing the battlefield Jace must resume the entry replacement");
    assert!(
        !reveals(&chosen.events),
        "CR 701.4a: choosing a Jace you control reveals nothing"
    );
    assert_eq!(runner.state().objects[&land].zone, Zone::Battlefield);
    assert!(
        !runner.state().objects[&land].tapped,
        "CR 614.12a: paying the Behold cost means the land enters untapped"
    );
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
}
