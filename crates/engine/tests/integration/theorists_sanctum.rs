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
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ReplacementChoice { .. }
    ));

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
