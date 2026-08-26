//! Bilbo, Fellow Conspirator — subtype-gated additional-token replacement.
//!
//! CR references verified against `docs/MagicCompRules.txt`:
//! - CR 614.1a: "instead" identifies a replacement effect.
//! - CR 614.5-6: a replacement gets one opportunity for an event, and the
//!   modified event happens instead of the original event.
//! - CR 109.5: "you" on Bilbo means Bilbo's controller.
//! - CR 111.1-2: token creation and ownership/controller semantics.
//! - CR 111.10a-b: predefined Treasure and Food tokens.

use std::collections::HashSet;

use engine::game::effects::token::apply_create_token_after_replacement;
use engine::game::replacement::{replace_event, ReplacementResult};
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::parser::parse_oracle_text;
use engine::types::card_type::CoreType;
use engine::types::events::GameEvent;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::proposed_event::{
    EtbTapState, ProposedEvent, TokenCharacteristics, TokenHostRequest, TokenSpec,
};
use engine::types::replacements::ReplacementEvent;

const BILBO: &str =
    "If you would create a Food token, instead create a Food token and a Treasure token.";

fn scenario_with_parsed_bilbo() -> GameRunner {
    let parsed = parse_oracle_text(BILBO, "Bilbo, Fellow Conspirator", &[], &[], &[]);
    assert!(
        parsed.abilities.is_empty(),
        "positive reach guard: Bilbo must not fall back to an effect ability"
    );
    assert!(
        parsed
            .replacements
            .iter()
            .any(|replacement| replacement.event == ReplacementEvent::CreateToken),
        "positive reach guard: verbatim Bilbo Oracle must produce CreateToken replacement"
    );

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "Bilbo, Fellow Conspirator", 2, 3, BILBO);
    scenario.build()
}

fn artifact_token_spec(subtype: &str, controller: PlayerId) -> TokenSpec {
    TokenSpec {
        characteristics: TokenCharacteristics {
            display_name: subtype.to_string(),
            power: None,
            toughness: None,
            core_types: vec![CoreType::Artifact],
            subtypes: vec![subtype.to_string()],
            supertypes: Vec::new(),
            colors: Vec::new(),
            keywords: Vec::new(),
        },
        script_name: subtype.to_string(),
        static_abilities: Vec::new(),
        enter_with_counters: Vec::new(),
        tapped: false,
        enters_attacking: false,
        sacrifice_at: None,
        source_id: engine::types::identifiers::ObjectId(0),
        controller,
        attach_to: TokenHostRequest::NotRequested,
    }
}

fn create_token_batch(runner: &mut GameRunner, owner: PlayerId, subtype: &str, count: u32) {
    let proposed = ProposedEvent::CreateToken {
        owner,
        spec: Box::new(artifact_token_spec(subtype, owner)),
        copy: None,
        enter_tapped: EtbTapState::Unspecified,
        count,
        applied: HashSet::new(),
    };
    let mut events: Vec<GameEvent> = Vec::new();
    match replace_event(runner.state_mut(), proposed, &mut events) {
        ReplacementResult::Execute(event) => {
            apply_create_token_after_replacement(runner.state_mut(), event, &mut events);
        }
        other => panic!("token creation should execute, got {other:?}"),
    }
}

fn token_count(runner: &GameRunner, owner: PlayerId, subtype: &str) -> usize {
    runner
        .state()
        .objects
        .values()
        .filter(|object| {
            object.is_token
                && object.owner == owner
                && object
                    .card_types
                    .subtypes
                    .iter()
                    .any(|actual| actual.eq_ignore_ascii_case(subtype))
        })
        .count()
}

#[test]
fn controllers_two_food_batch_creates_two_food_and_two_treasure() {
    let mut runner = scenario_with_parsed_bilbo();

    create_token_batch(&mut runner, P0, "Food", 2);

    assert_eq!(token_count(&runner, P0, "Food"), 2);
    assert_eq!(token_count(&runner, P0, "Treasure"), 2);
}

#[test]
fn controllers_non_food_batch_is_unchanged() {
    let mut runner = scenario_with_parsed_bilbo();

    create_token_batch(&mut runner, P0, "Clue", 2);

    assert_eq!(token_count(&runner, P0, "Clue"), 2);
    assert_eq!(token_count(&runner, P0, "Treasure"), 0);
}

#[test]
fn opponents_food_batch_is_unchanged() {
    let mut runner = scenario_with_parsed_bilbo();

    create_token_batch(&mut runner, P1, "Food", 2);

    assert_eq!(token_count(&runner, P1, "Food"), 2);
    assert_eq!(token_count(&runner, P1, "Treasure"), 0);
}
