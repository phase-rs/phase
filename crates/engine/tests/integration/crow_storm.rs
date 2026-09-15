//! Crow Storm's token name must flow from Oracle parsing through the normal
//! cast and Storm-copy pipelines.

use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::types::card_type::CoreType;
use engine::types::game_state::WaitingFor;
use engine::types::keywords::Keyword;
use engine::types::mana::ManaColor;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const CROW_STORM_ORACLE: &str =
    "Create a 1/2 blue Bird creature token with flying named Storm Crow.\n\
Storm (When you cast this spell, copy it for each spell cast before it this turn.)";

const OSGOOD_TOKEN_ORACLE: &str =
    "Create a 2/2 blue Human Alien Shapeshifter creature token named Osgood, Operation Double with flying.";

const GOBLIN_GATHERING_ORACLE: &str = "Create a number of 1/1 red Goblin creature tokens \
equal to two plus the number of cards named Goblin Gathering in your graveyard.";

const MIXED_TOKEN_KEYWORD_CLAUSE_ORACLE: &str = "Create a 1/1 red Goblin creature token \
with flying and cards named Goblin Gathering in your graveyard.";

const SANGUINE_BRUSHSTROKE_ORACLE: &str = "When Sanguine Brushstroke enters the battlefield, \
create a Blood token and conjure a card named Blood Artist onto the battlefield.\n\
Whenever you sacrifice a Blood token, each opponent loses 1 life and you gain 1 life.";

const PRIOR_SPELL_ORACLE: &str = "You gain 1 life.";

fn spells_cast_by(runner: &GameRunner, player: PlayerId) -> usize {
    runner
        .state()
        .spells_cast_this_turn_by_player
        .get(&player)
        .map_or(0, |records| records.len())
}

#[test]
fn crow_storm_creates_correctly_named_tokens_for_original_and_storm_copy() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let prior_spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Prior Spell", true, PRIOR_SPELL_ORACLE)
        .id();
    let crow_storm = scenario
        .add_spell_to_hand(P0, "Crow Storm", false)
        .from_oracle_text_with_keywords(&["Storm"], CROW_STORM_ORACLE)
        .id();
    let mut runner = scenario.build();

    runner.state_mut().turn_number = 1;
    runner.state_mut().active_player = P0;
    runner.state_mut().priority_player = P0;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P0 };

    runner.cast(prior_spell).resolve();
    assert_eq!(
        spells_cast_by(&runner, P0),
        1,
        "the prior spell must be cast through the normal pipeline"
    );

    runner.cast(crow_storm).resolve();
    assert_eq!(
        spells_cast_by(&runner, P0),
        2,
        "Crow Storm itself is the second cast spell; its copies are not cast"
    );

    // CR 702.40a: Storm copies Crow Storm once for the one other spell cast
    // before it this turn, so the original and one copy each create a token.
    let token_ids: Vec<_> = runner
        .state()
        .battlefield
        .iter()
        .copied()
        .filter(|id| runner.state().objects[id].is_token)
        .collect();
    assert_eq!(
        token_ids.len(),
        2,
        "Crow Storm must create one token for the spell and one for its Storm copy"
    );

    // CR 111.3 + CR 111.4: the creating spell defines these characteristics,
    // including Storm Crow's name independently of its Bird subtype.
    for token_id in &token_ids {
        let token = &runner.state().objects[token_id];
        assert_eq!(token.controller, P0);
        assert_eq!(token.name, "Storm Crow");
        assert_eq!((token.power, token.toughness), (Some(1), Some(2)));
        assert_eq!(token.color, vec![ManaColor::Blue]);
        assert!(token.card_types.core_types.contains(&CoreType::Creature));
        assert!(
            token
                .card_types
                .subtypes
                .iter()
                .any(|subtype| subtype == "Bird"),
            "token must retain its Bird subtype: {:?}",
            token.card_types.subtypes
        );
        assert!(token.keywords.contains(&Keyword::Flying));
    }

    assert!(
        token_ids
            .iter()
            .all(|id| runner.state().objects[id].name != "Bird"),
        "the positive two-token assertion above prevents this default-name regression check from passing vacuously"
    );
}

#[test]
fn comma_bearing_token_name_survives_the_cast_pipeline_with_its_keyword_suffix() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Comma Token Spell", false, OSGOOD_TOKEN_ORACLE)
        .id();
    let mut runner = scenario.build();

    runner.state_mut().turn_number = 1;
    runner.state_mut().active_player = P0;
    runner.state_mut().priority_player = P0;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P0 };
    runner.cast(spell).resolve();

    let token_ids: Vec<_> = runner
        .state()
        .battlefield
        .iter()
        .copied()
        .filter(|id| runner.state().objects[id].is_token)
        .collect();
    assert_eq!(token_ids.len(), 1, "the spell must create its token");

    // CR 111.3 + CR 111.4: a comma is part of this token's name, while the
    // following `with flying` remains a separate defining characteristic.
    let token = &runner.state().objects[&token_ids[0]];
    assert_eq!(token.name, "Osgood, Operation Double");
    assert_eq!(token.color, vec![ManaColor::Blue]);
    assert_eq!((token.power, token.toughness), (Some(2), Some(2)));
    assert!(token.card_types.core_types.contains(&CoreType::Creature));
    assert!(
        ["Human", "Alien", "Shapeshifter"]
            .into_iter()
            .all(|subtype| token
                .card_types
                .subtypes
                .iter()
                .any(|actual| actual == subtype)),
        "token must retain each subtype: {:?}",
        token.card_types.subtypes
    );
    assert!(token.keywords.contains(&Keyword::Flying));
}

#[test]
fn named_count_operand_does_not_override_goblin_token_names_when_cast() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Goblin Gathering", false, GOBLIN_GATHERING_ORACLE)
        .id();
    let mut runner = scenario.build();

    runner.state_mut().turn_number = 1;
    runner.state_mut().active_player = P0;
    runner.state_mut().priority_player = P0;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P0 };
    let outcome = runner.cast(spell).resolve();

    // CR 111.4: `named Goblin Gathering` identifies cards in the count, not
    // the tokens' defining name, which remains the Goblin subtype.
    let token_names: Vec<_> = outcome
        .state()
        .battlefield
        .iter()
        .map(|id| &outcome.state().objects[id])
        .filter(|object| object.is_token)
        .map(|object| object.name.as_str())
        .collect();
    assert_eq!(token_names, ["Goblin", "Goblin"]);
}

#[test]
fn mixed_token_keyword_clause_does_not_rebind_the_name_when_cast() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Mixed Keyword Clause Spell",
            false,
            MIXED_TOKEN_KEYWORD_CLAUSE_ORACLE,
        )
        .id();
    let mut runner = scenario.build();

    runner.state_mut().turn_number = 1;
    runner.state_mut().active_player = P0;
    runner.state_mut().priority_player = P0;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P0 };
    let outcome = runner.cast(spell).resolve();

    let token_ids: Vec<_> = outcome
        .state()
        .battlefield
        .iter()
        .copied()
        .filter(|id| outcome.state().objects[id].is_token)
        .collect();
    assert_eq!(
        token_ids.len(),
        1,
        "the mixed grammar must still create its token through the cast pipeline"
    );

    // `cards named Goblin Gathering` is not a complete token keyword clause
    // and therefore cannot override the descriptor-derived Goblin name. This
    // assertion fails if late-name parsing returns to its former
    // nonempty-keyword-list predicate.
    let token = &outcome.state().objects[&token_ids[0]];
    assert_eq!(token.name, "Goblin");
    assert!(token.keywords.contains(&Keyword::Flying));
}

#[test]
fn named_conjure_operand_does_not_override_blood_token_name_when_cast() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let brushstroke = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Sanguine Brushstroke",
            false,
            SANGUINE_BRUSHSTROKE_ORACLE,
        )
        .as_enchantment()
        .id();
    let mut runner = scenario.build();

    runner.state_mut().turn_number = 1;
    runner.state_mut().active_player = P0;
    runner.state_mut().priority_player = P0;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P0 };
    let outcome = runner.cast(brushstroke).resolve();

    outcome.assert_zone(&[brushstroke], Zone::Battlefield);
    // CR 111.4: `named Blood Artist` belongs to the separate conjure action;
    // it must not become the name of the Blood token from the prior action.
    let token_names: Vec<_> = outcome
        .state()
        .battlefield
        .iter()
        .map(|id| &outcome.state().objects[id])
        .filter(|object| object.is_token)
        .map(|object| object.name.as_str())
        .collect();
    assert_eq!(token_names, ["Blood"]);
}
