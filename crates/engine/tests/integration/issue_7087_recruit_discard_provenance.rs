//! GitHub issue #7087 — Recruit's contingent token must read the card chosen
//! for its interactive discard from the directly adjacent discard frame.

use std::io::Read;

use engine::game::engine::apply;
use engine::game::scenario::{GameRunner, P0, P1};
use engine::game::zones::move_to_zone;
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::{GameState, PersistedGameState, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::resolution::ResolutionFrame;
use engine::types::zones::Zone;

const BIFUR: ObjectId = ObjectId(80);
const PLAINS: ObjectId = ObjectId(45);
const MOUNTAIN_KINGS_RETURN: ObjectId = ObjectId(53);
const DRAWN_CARD: ObjectId = ObjectId(57);

fn gunzip(gz: &[u8]) -> String {
    let mut json = String::new();
    flate2::read::GzDecoder::new(gz)
        .read_to_string(&mut json)
        .expect("fixture .json.gz must inflate to UTF-8 JSON");
    json
}

fn load_state() -> GameState {
    let json = gunzip(include_bytes!(
        "fixtures/issue_7087_recruit_discard_provenance.json.gz"
    ));
    let envelope: serde_json::Value =
        serde_json::from_str(&json).expect("game-state envelope parses as JSON");
    serde_json::from_value::<PersistedGameState>(envelope["gameState"].clone())
        .expect("gameState deserializes through the production decoder")
        .into_game_state()
        .expect("persisted test snapshot satisfies the checked restore contract")
}

fn token_count(state: &GameState) -> usize {
    state
        .objects
        .values()
        .filter(|object| object.zone == Zone::Battlefield && object.is_token)
        .count()
}

fn resolve_recruit_to_discard_choice(runner: &mut GameRunner) -> Vec<ObjectId> {
    apply(runner.state_mut(), P0, GameAction::PassPriority)
        .expect("P0 may pass priority on the loaded trigger");

    let WaitingFor::DiscardChoice {
        player,
        count,
        cards,
        ..
    } = &runner.state().waiting_for
    else {
        panic!(
            "Recruit must reach P1's interactive discard choice, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(*player, P1, "the Recruit controller chooses the discard");
    assert_eq!(*count, 1, "Recruit discards exactly one card");
    assert_eq!(
        runner.state().resolution_stack.len(),
        2,
        "Recruit's suspended frames are exactly [Discard, AbilityContinuation]"
    );
    assert!(
        matches!(
            runner.state().resolution_stack.last(),
            Some(ResolutionFrame::AbilityContinuation(_))
        ),
        "the direct continuation is the stack top"
    );
    assert!(
        runner
            .state()
            .resolution_stack
            .active_ability_continuation_discard_parent_id()
            .is_some(),
        "the active continuation has a direct discard parent"
    );
    cards.clone()
}

#[test]
fn recruit_from_the_reported_state_creates_a_token_after_discarding_bifur() {
    let mut runner = GameRunner::from_state(load_state());
    let tokens_before = token_count(runner.state());

    let offered = resolve_recruit_to_discard_choice(&mut runner);
    assert!(
        offered.contains(&BIFUR),
        "Bifur is in P1's hand and must be offered for Recruit's discard"
    );

    runner
        .act(GameAction::SelectCards { cards: vec![BIFUR] })
        .expect("discarding the offered nonland Bifur must resume Recruit");

    assert_eq!(
        runner.state().objects[&BIFUR].zone,
        Zone::Graveyard,
        "the selected nonland reaches P1's graveyard"
    );
    assert_eq!(
        token_count(runner.state()),
        tokens_before + 1,
        "Recruit creates its Human Soldier token after discarding a nonland"
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "the resolved Recruit chain returns to a clean priority window"
    );
}

#[test]
fn recruit_from_the_reported_state_does_not_create_a_token_after_discarding_a_land() {
    let mut runner = GameRunner::from_state(load_state());
    let mut setup_events = Vec::new();
    move_to_zone(runner.state_mut(), PLAINS, Zone::Hand, &mut setup_events);
    assert_eq!(
        runner.state().objects[&PLAINS].zone,
        Zone::Hand,
        "the real P1 Plains is moved into hand without changing the library top"
    );

    let tokens_before = token_count(runner.state());
    let offered = resolve_recruit_to_discard_choice(&mut runner);
    assert_eq!(
        runner.state().objects[&DRAWN_CARD].zone,
        Zone::Hand,
        "P1 draws the original library-top Patient Instructor"
    );
    assert!(
        offered.contains(&PLAINS),
        "the moved P1 Plains is eligible for Recruit's discard choice"
    );

    runner
        .act(GameAction::SelectCards {
            cards: vec![PLAINS],
        })
        .expect("discarding the offered land must resume Recruit");

    assert_eq!(
        runner.state().objects[&PLAINS].zone,
        Zone::Graveyard,
        "the selected land reaches P1's graveyard"
    );
    assert_eq!(
        token_count(runner.state()),
        tokens_before,
        "Recruit does not create a token after discarding a land"
    );
    let untouched = &runner.state().objects[&MOUNTAIN_KINGS_RETURN];
    assert_eq!(
        untouched.zone,
        Zone::Hand,
        "the known mistaken land premise must not discard object 53"
    );
    assert!(
        untouched
            .card_types
            .core_types
            .contains(&CoreType::Enchantment),
        "object 53 is The Mountain-king's Return, an enchantment rather than a land"
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "the resolved Recruit chain returns to a clean priority window"
    );
}
