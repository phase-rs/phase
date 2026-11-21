//! Indoraptor, the Perfect Hybrid — Enrage: "choose an opponent at random.
//! Indoraptor deals damage equal to its power to that player unless they
//! sacrifice a nontoken creature of their choice." The chosen opponent — not
//! Indoraptor's controller — receives the sacrifice-or-damage decision and the
//! damage (CR 608.2c + CR 118.12a).

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const P2: PlayerId = PlayerId(2);
const INDORAPTOR: &str = "Menace\nEnrage — Whenever Indoraptor is dealt damage, choose an opponent at random. Indoraptor deals damage equal to its power to that player unless they sacrifice a nontoken creature of their choice.";
const GUT_SHOT: &str =
    "({R/P} can be paid with either {R} or 2 life.)\nGut Shot deals 1 damage to any target.";

fn setup() -> (
    engine::game::scenario::GameRunner,
    ObjectId,
    ObjectId,
    ObjectId,
) {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let indoraptor = scenario
        .add_creature_from_oracle(P0, "Indoraptor, the Perfect Hybrid", 3, 3, INDORAPTOR)
        .id();
    // Test-only cost override: cast printed Gut Shot for free to trigger Enrage.
    let gut_shot = scenario
        .add_spell_to_hand_from_oracle(P0, "Gut Shot", true, GUT_SHOT)
        .with_mana_cost(ManaCost::zero())
        .id();
    // Both opponents control a nontoken creature so each can answer.
    let p1_creature = scenario.add_creature(P1, "P1 Fodder", 1, 1).id();
    let p2_creature = scenario.add_creature(P2, "P2 Fodder", 1, 1).id();
    let mut runner = scenario.build();
    runner.cast(gut_shot).target_object(indoraptor).resolve();
    (runner, indoraptor, p1_creature, p2_creature)
}

fn chosen_payer(runner: &engine::game::scenario::GameRunner) -> PlayerId {
    let WaitingFor::UnlessPayment { player, .. } = runner.state().waiting_for else {
        panic!(
            "expected the chosen opponent to be prompted, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_ne!(player, P0, "the controller must never be the payer");
    player
}

#[test]
fn enrage_prompts_chosen_opponent_and_damages_them_on_decline() {
    let (mut runner, indoraptor, _, _) = setup();
    let payer = chosen_payer(&runner);
    let other = if payer == P1 { P2 } else { P1 };
    let (payer_life, other_life, p0_life) =
        (runner.life(payer), runner.life(other), runner.life(P0));

    runner
        .act(GameAction::PayUnlessCost { pay: false })
        .expect("chosen opponent declines");

    // Indoraptor survived (3 toughness, 1 damage) and dealt damage equal to its power.
    assert_eq!(runner.state().objects[&indoraptor].zone, Zone::Battlefield);
    assert_eq!(runner.life(payer), payer_life - 3);
    assert_eq!(runner.life(other), other_life);
    assert_eq!(runner.life(P0), p0_life);
}

#[test]
fn enrage_chosen_opponent_may_sacrifice_to_prevent_damage() {
    let (mut runner, _, p1_creature, p2_creature) = setup();
    let payer = chosen_payer(&runner);
    let fodder = if payer == P1 {
        p1_creature
    } else {
        p2_creature
    };
    let payer_life = runner.life(payer);

    runner
        .act(GameAction::PayUnlessCost { pay: true })
        .expect("chosen opponent elects to sacrifice");
    runner
        .act(GameAction::SelectCards {
            cards: vec![fodder],
        })
        .expect("sacrifice the creature");

    assert_eq!(runner.state().objects[&fodder].zone, Zone::Graveyard);
    assert_eq!(runner.life(payer), payer_life);
}
