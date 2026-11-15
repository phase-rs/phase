//! CR 601.2h: a save taken in Acceptable Losses' mana window still owes the
//! random discard; a save that names it with the retired key is refused.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::game_state::{
    CastPaymentMode, GameState, PersistedGameState, PersistedRestoreFinalization, WaitingFor,
};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard};
use engine::types::phase::Phase;
use engine::types::zones::Zone;
use engine::types::TargetRef;

const LEGACY_ERROR: &str = "legacy deferred_random_discard_cost snapshot";

const ACCEPTABLE_LOSSES: &str = "As an additional cost to cast this spell, discard a card at random.\nAcceptable Losses deals 5 damage to target creature.";

struct Window {
    runner: GameRunner,
    hand: [ObjectId; 2],
    target: ObjectId,
    mountains: Vec<ObjectId>,
}

fn mana_window() -> Window {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Acceptable Losses", true, ACCEPTABLE_LOSSES)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Red],
            generic: 3,
        })
        .id();
    let hand = [
        scenario.add_card_to_hand(P0, "H1"),
        scenario.add_card_to_hand(P0, "H2"),
    ];
    let target = scenario.add_vanilla(P1, 2, 2);
    let mountains = (0..4)
        .map(|_| scenario.add_basic_land(P0, ManaColor::Red))
        .collect();
    let mut runner = scenario.build();
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Manual,
        })
        .expect("the cast is offered");
    if matches!(
        runner.state().waiting_for,
        WaitingFor::TargetSelection { .. }
    ) {
        runner
            .act(GameAction::ChooseTarget {
                target: Some(TargetRef::Object(target)),
            })
            .expect("the creature is a legal target");
    }
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::ManaPayment { .. }),
        "{:?}",
        runner.state().waiting_for
    );
    Window {
        runner,
        hand,
        target,
        mountains,
    }
}

/// Rewrites every current random-discard reservation to the retired
/// `deferred_random_discard_cost: {"count": N}` shape; returns how many it rewrote.
fn retire_random_discard_key(value: &mut serde_json::Value) -> usize {
    match value {
        serde_json::Value::Object(object) => {
            let mut rewritten = 0;
            if let Some(cost) = object.remove("deferred_hand_discard_cost") {
                assert_eq!(cost["type"], "Random", "{cost}");
                object.insert(
                    "deferred_random_discard_cost".into(),
                    serde_json::json!({ "count": cost["count"] }),
                );
                rewritten += 1;
            }
            rewritten
                + object
                    .values_mut()
                    .map(retire_random_discard_key)
                    .sum::<usize>()
        }
        serde_json::Value::Array(values) => values.iter_mut().map(retire_random_discard_key).sum(),
        _ => 0,
    }
}

fn restore(json: &str) -> Result<GameState, String> {
    serde_json::from_str::<PersistedGameState>(json)
        .map_err(|error| error.to_string())?
        .prepare_for_restore(PersistedRestoreFinalization::DeferUntilRehydrated)
        .map_err(|error| error.to_string())?
        .finalize_after_rehydration(|_| Ok(()))
        .map_err(|error| error.to_string())
}

fn save(runner: &GameRunner, trusted: bool) -> serde_json::Value {
    let persisted = if trusted {
        PersistedGameState::capture(runner.state().clone())
    } else {
        PersistedGameState::Raw(Box::new(runner.state().clone()))
    };
    serde_json::to_value(&persisted).expect("the window serializes")
}

#[test]
fn acceptable_losses_restored_in_its_mana_window_still_discards_at_random() {
    for trusted in [false, true] {
        let window = mana_window();
        let json = save(&window.runner, trusted).to_string();
        let mut runner = GameRunner::from_state(restore(&json).expect("a current save restores"));
        for &mountain in &window.mountains {
            runner
                .act(GameAction::ActivateAbility {
                    source_id: mountain,
                    ability_index: 0,
                    payment_mode: CastPaymentMode::Auto,
                })
                .expect("the Mountain taps for {R}");
        }
        runner
            .act(GameAction::PassPriority)
            .expect("the payment finishes");
        runner.advance_until_stack_empty();
        let discarded = window
            .hand
            .iter()
            .filter(|card| runner.state().objects[card].zone == Zone::Graveyard)
            .count();
        assert_eq!(
            discarded, 1,
            "trusted {trusted}: one card is discarded at random"
        );
        assert_eq!(
            runner.state().objects[&window.target].zone,
            Zone::Graveyard,
            "trusted {trusted}"
        );
    }
}

#[test]
fn acceptable_losses_saved_with_the_retired_random_discard_key_is_refused() {
    for trusted in [false, true] {
        let window = mana_window();
        let mut value = save(&window.runner, trusted);
        assert!(
            retire_random_discard_key(&mut value) > 0,
            "the save owes the discard"
        );
        let error = restore(&value.to_string()).expect_err("the retired key is refused");
        assert!(error.contains(LEGACY_ERROR), "trusted {trusted}: {error}");
    }
    let window = mana_window();
    let mut bare = serde_json::to_value(window.runner.state()).expect("the state serializes");
    serde_json::from_value::<GameState>(bare.clone()).expect("a current bare state decodes");
    assert!(
        retire_random_discard_key(&mut bare) > 0,
        "the bare state owes the discard"
    );
    let error = serde_json::from_value::<GameState>(bare)
        .expect_err("the bare-state decode refuses the retired key too")
        .to_string();
    assert!(error.contains(LEGACY_ERROR), "{error}");
}
