//! CR 107.1c + CR 608.2c + CR 701.20a: Kamahl's Summons lets each player
//! reveal any number of creature cards from their own hand, then creates one
//! Bear for each card that player revealed.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, GameState, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

const ORACLE: &str = "Each player may reveal any number of creature cards from their hand. Then each player creates a 2/2 green Bear creature token for each card they revealed this way.";

fn setup() -> (GameRunner, ObjectId, Vec<ObjectId>, Vec<ObjectId>) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(
        P0,
        (0..4)
            .map(|_| ManaUnit::new(ManaType::Green, ObjectId(0), false, vec![]))
            .collect(),
    );
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Kamahl's Summons", false, ORACLE)
        .id();
    let p0_creatures = (0..2)
        .map(|index| {
            scenario
                .add_creature_to_hand(P0, &format!("P0 Creature {index}"), 2, 2)
                .id()
        })
        .collect();
    let p1_creatures = (0..2)
        .map(|index| {
            scenario
                .add_creature_to_hand(P1, &format!("P1 Creature {index}"), 2, 2)
                .id()
        })
        .collect();
    (scenario.build(), spell, p0_creatures, p1_creatures)
}

fn cast(runner: &mut GameRunner, spell: ObjectId) {
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("Kamahl's Summons should cast");
    runner.resolve_top();
    runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .expect("P0 may choose to reveal");
}

fn assert_reveal_choice(runner: &GameRunner, player: PlayerId, cards: &[ObjectId]) {
    match &runner.state().waiting_for {
        WaitingFor::RevealChoice {
            player: chooser,
            cards: eligible,
            cardinality,
            ..
        } => {
            assert_eq!(*chooser, player);
            assert_eq!(eligible, cards);
            assert_eq!(
                *cardinality,
                engine::types::ability::ObjectSelectionCardinality::AnyNumber
            );
        }
        other => panic!("expected {player:?}'s reveal choice, got {other:?}"),
    }
}

fn bear_count(runner: &GameRunner, player: PlayerId) -> usize {
    runner
        .state()
        .battlefield
        .iter()
        .filter(|id| {
            runner.state().objects.get(id).is_some_and(|object| {
                object.controller == player
                    && object.is_token
                    && object.name == "Bear"
                    && object.power == Some(2)
                    && object.toughness == Some(2)
            })
        })
        .count()
}

fn assert_reveal_ledger_survives_round_trip(runner: &GameRunner) {
    let wire = serde_json::to_value(runner.state()).expect("game state should serialize");
    assert!(wire.get("scoped_reveal_sets").is_some());
    let restored: GameState = serde_json::from_value(wire.clone())
        .expect("game state with a paused reveal should deserialize");
    let restored_wire = serde_json::to_value(restored).expect("restored state should serialize");
    assert_eq!(
        wire.get("scoped_reveal_sets"),
        restored_wire.get("scoped_reveal_sets")
    );
    assert_eq!(
        wire.get("tracked_set_publication_generation"),
        restored_wire.get("tracked_set_publication_generation")
    );
}

#[test]
fn kamahls_summons_prompts_each_hand_owner_and_counts_each_reveal_set() {
    let (mut runner, spell, p0_creatures, p1_creatures) = setup();
    cast(&mut runner, spell);

    assert_reveal_choice(&runner, P0, &p0_creatures);
    runner
        .act(GameAction::SelectCards {
            cards: vec![p0_creatures[0]],
        })
        .expect("P0 may reveal one creature");
    assert_reveal_ledger_survives_round_trip(&runner);

    runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .expect("P1 may choose to reveal");
    assert_reveal_choice(&runner, P1, &p1_creatures);
    runner
        .act(GameAction::SelectCards {
            cards: p1_creatures.clone(),
        })
        .expect("P1 may reveal both creatures");

    assert_eq!(bear_count(&runner, P0), 1);
    assert_eq!(bear_count(&runner, P1), 2);
}

#[test]
fn kamahls_summons_declined_player_creates_no_bears() {
    let (mut runner, spell, p0_creatures, _p1_creatures) = setup();
    cast(&mut runner, spell);

    assert_reveal_choice(&runner, P0, &p0_creatures);
    runner
        .act(GameAction::SelectCards {
            cards: vec![p0_creatures[0]],
        })
        .expect("P0 may reveal one creature");

    runner
        .act(GameAction::DecideOptionalEffect { accept: false })
        .expect("P1 may decline to reveal");

    assert_eq!(bear_count(&runner, P0), 1);
    assert_eq!(bear_count(&runner, P1), 0);
}
