//! CR 107.1c + CR 608.2c + CR 701.20a: Kamahl's Summons lets each player
//! reveal any number of creature cards from their own hand, then creates one
//! Bear for each card that player revealed.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::visibility::filter_state_for_viewer;
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, GameState, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

const ORACLE: &str = "Each player may reveal any number of creature cards from their hand. Then each player creates a 2/2 green Bear creature token for each card they revealed this way.";
const MYSTIC_REFLECTION: &str = "Choose target nonlegendary creature. The next time one or more creatures or planeswalkers enter this turn, they enter as copies of the chosen creature.";

fn setup() -> (
    GameRunner,
    ObjectId,
    ObjectId,
    Vec<ObjectId>,
    Vec<ObjectId>,
    ObjectId,
) {
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
    let mystic = scenario
        .add_spell_to_hand_from_oracle(P0, "Mystic Reflection", false, MYSTIC_REFLECTION)
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
    let copy_target = scenario.add_creature(P0, "Reflection Target", 3, 3).id();
    (
        scenario.build(),
        spell,
        mystic,
        p0_creatures,
        p1_creatures,
        copy_target,
    )
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

fn token_count(runner: &GameRunner, player: PlayerId) -> usize {
    runner
        .state()
        .battlefield
        .iter()
        .filter(|id| {
            runner
                .state()
                .objects
                .get(id)
                .is_some_and(|object| object.controller == player && object.is_token)
        })
        .count()
}

fn restore_paused_runner(runner: &GameRunner) -> GameRunner {
    let wire = serde_json::to_value(runner.state()).expect("game state should serialize");
    assert!(wire.get("pending_player_scope_reveal").is_some());
    let restored: GameState = serde_json::from_value(wire.clone())
        .expect("game state with a paused reveal should deserialize");
    let restored_wire = serde_json::to_value(restored).expect("restored state should serialize");
    assert_eq!(
        wire.get("pending_player_scope_reveal"),
        restored_wire.get("pending_player_scope_reveal")
    );
    GameRunner::from_state(serde_json::from_value(restored_wire).expect("restore state"))
}

#[test]
fn kamahls_summons_prompts_each_hand_owner_and_counts_each_reveal_set() {
    let (mut runner, spell, _mystic, p0_creatures, p1_creatures, _copy_target) = setup();
    cast(&mut runner, spell);

    assert_reveal_choice(&runner, P0, &p0_creatures);
    runner
        .act(GameAction::SelectCards {
            cards: vec![p0_creatures[0]],
        })
        .expect("P0 may reveal one creature");

    // CR 400.2 + CR 701.20a: P0's selection remains hidden while P1 still
    // has the APNAP decision; the pending carrier is not public information.
    let p1_view = filter_state_for_viewer(runner.state(), P1);
    assert!(!p1_view.objects[&p0_creatures[0]].display_visible_to_viewer);

    let mut runner = restore_paused_runner(&runner);

    runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .expect("P1 may choose to reveal");
    assert_reveal_choice(&runner, P1, &p1_creatures);
    runner
        .act(GameAction::SelectCards {
            cards: p1_creatures.clone(),
        })
        .expect("P1 may reveal both creatures");

    assert!(runner.state().pending_player_scope_reveal.is_none());
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    assert!(runner.state().stack.is_empty());
    let p0_view = filter_state_for_viewer(runner.state(), P0);
    assert!(p0_view.objects[&p0_creatures[0]].display_visible_to_viewer);
    assert!(p0_view.objects[&p1_creatures[0]].display_visible_to_viewer);
    assert_eq!(bear_count(&runner, P0), 1);
    assert_eq!(bear_count(&runner, P1), 2);
}

#[test]
fn kamahls_summons_declined_player_creates_no_bears() {
    let (mut runner, spell, _mystic, p0_creatures, _p1_creatures, _copy_target) = setup();
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

#[test]
fn kamahls_summons_dynamic_batches_share_one_mystic_reflection_replacement() {
    let (mut runner, spell, mystic, p0_creatures, p1_creatures, copy_target) = setup();

    runner.cast(mystic).target_object(copy_target).resolve();
    assert!(runner.state().stack.is_empty());

    cast(&mut runner, spell);
    assert_reveal_choice(&runner, P0, &p0_creatures);
    runner
        .act(GameAction::SelectCards {
            cards: vec![p0_creatures[0]],
        })
        .expect("P0 may reveal one creature");
    runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .expect("P1 may choose to reveal");
    assert_reveal_choice(&runner, P1, &p1_creatures);
    runner
        .act(GameAction::SelectCards {
            cards: p1_creatures.clone(),
        })
        .expect("P1 may reveal both creatures");

    // CR 614.1a + CR 101.4: Mystic Reflection is consumed by the single
    // compound creation event, so both unequal per-seat dynamic batches copy
    // the chosen creature.
    assert_eq!(token_count(&runner, P0), 1);
    assert_eq!(token_count(&runner, P1), 2);
    for player in [P0, P1] {
        assert!(runner
            .state()
            .battlefield
            .iter()
            .filter_map(|id| runner.state().objects.get(id))
            .filter(|object| object.controller == player && object.is_token)
            .all(|object| object.name == "Reflection Target"));
    }
}
