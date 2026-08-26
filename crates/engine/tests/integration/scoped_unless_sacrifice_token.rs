//! Aggregate player-scoped sacrifice-unless token creation.
//!
//! The Oracle grammar is the reusable clause printed by Acererak the Archlich:
//! each opponent decides in APNAP order, and only after every answer does the
//! source's controller create one Zombie per decline.

use engine::game::elimination::eliminate_player;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const P2: PlayerId = PlayerId(2);
const SCOPED_UNLESS_ZOMBIE: &str =
    "For each opponent, you create a 2/2 black Zombie creature token unless they sacrifice a creature.";
const REST_IN_PEACE: &str =
    "When Rest in Peace enters, exile all graveyards.\nIf a card or token would be put into a graveyard from anywhere, exile it instead.";
const DARKSTEEL_COLOSSUS: &str = "Trample\nIndestructible\nIf Darksteel Colossus would be put into a graveyard from anywhere, reveal Darksteel Colossus and shuffle it into its owner's library instead.";

fn zombie_count(runner: &GameRunner) -> usize {
    runner
        .state()
        .battlefield
        .iter()
        .filter(|id| {
            let object = &runner.state().objects[id];
            object.controller == P0 && object.is_token && object.name == "Zombie"
        })
        .count()
}

fn expect_unless(runner: &GameRunner, player: PlayerId) {
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::UnlessPayment { player: actual, .. } if actual == player
    ));
}

fn expect_aggregate_payment_finished(runner: &GameRunner) {
    assert!(runner.state().pending_player_scope_unless_payment.is_none());
    assert!(
        !matches!(runner.state().waiting_for, WaitingFor::UnlessPayment { .. }),
        "terminal aggregate settlement must not retain a payer prompt"
    );
}

fn pay_sacrifice(runner: &mut GameRunner, creature: ObjectId) {
    runner
        .act(GameAction::PayUnlessCost { pay: true })
        .expect("payer may choose to sacrifice");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::WardSacrificeChoice { .. }
    ));
    runner
        .act(GameAction::SelectCards {
            cards: vec![creature],
        })
        .expect("selected creature pays the unless cost");
}

fn give_p0_sorcery_window(runner: &mut GameRunner) {
    let state = runner.state_mut();
    state.phase = Phase::PreCombatMain;
    state.turn_number = 2;
    state.active_player = P0;
    state.priority_player = P0;
    state.waiting_for = WaitingFor::Priority { player: P0 };
}

#[test]
fn scoped_unless_sacrifice_aggregates_mixed_three_player_answers() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Scoped Zombie Test", false, SCOPED_UNLESS_ZOMBIE)
        .with_mana_cost(ManaCost::zero())
        .id();
    let p2_creature = scenario.add_creature(P2, "P2 Sacrifice", 1, 1).id();
    let mut runner = scenario.build();
    give_p0_sorcery_window(&mut runner);
    runner.cast(spell).resolve();

    expect_unless(&runner, P1);
    runner
        .act(GameAction::PayUnlessCost { pay: false })
        .expect("P1 declines");
    expect_unless(&runner, P2);
    pay_sacrifice(&mut runner, p2_creature);

    assert_eq!(
        zombie_count(&runner),
        1,
        "one decline creates one aggregate Zombie"
    );
    assert_eq!(runner.state().objects[&p2_creature].zone, Zone::Graveyard);
    expect_aggregate_payment_finished(&runner);
}

#[test]
fn scoped_unless_sacrifice_creates_one_owned_batch_for_all_declines_and_none_for_all_pay() {
    let mut all_decline = GameScenario::new_n_player(3, 42);
    let decline_spell = all_decline
        .add_spell_to_hand_from_oracle(P0, "Scoped Zombie Test", false, SCOPED_UNLESS_ZOMBIE)
        .with_mana_cost(ManaCost::zero())
        .id();
    let _p2_creature = all_decline.add_creature(P2, "P2 Sacrifice", 1, 1).id();
    let mut decline_runner = all_decline.build();
    give_p0_sorcery_window(&mut decline_runner);
    decline_runner.cast(decline_spell).resolve();
    decline_runner
        .act(GameAction::PayUnlessCost { pay: false })
        .unwrap();
    decline_runner
        .act(GameAction::PayUnlessCost { pay: false })
        .unwrap();
    assert_eq!(zombie_count(&decline_runner), 2);
    expect_aggregate_payment_finished(&decline_runner);

    let mut all_pay = GameScenario::new_n_player(3, 42);
    let pay_spell = all_pay
        .add_spell_to_hand_from_oracle(P0, "Scoped Zombie Test", false, SCOPED_UNLESS_ZOMBIE)
        .with_mana_cost(ManaCost::zero())
        .id();
    let p1_creature = all_pay.add_creature(P1, "P1 Sacrifice", 1, 1).id();
    let p2_creature = all_pay.add_creature(P2, "P2 Sacrifice", 1, 1).id();
    let mut pay_runner = all_pay.build();
    give_p0_sorcery_window(&mut pay_runner);
    pay_runner.cast(pay_spell).resolve();
    pay_sacrifice(&mut pay_runner, p1_creature);
    pay_sacrifice(&mut pay_runner, p2_creature);
    assert_eq!(zombie_count(&pay_runner), 0);
}

#[test]
fn scoped_unless_sacrifice_skips_an_eliminated_pending_payer() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Scoped Zombie Test", false, SCOPED_UNLESS_ZOMBIE)
        .with_mana_cost(ManaCost::zero())
        .id();
    let _p2_creature = scenario.add_creature(P2, "P2 Sacrifice", 1, 1).id();
    let mut runner = scenario.build();
    give_p0_sorcery_window(&mut runner);
    runner.cast(spell).resolve();
    expect_unless(&runner, P1);

    eliminate_player(runner.state_mut(), P1, &mut Vec::new());
    expect_unless(&runner, P2);
    runner
        .act(GameAction::PayUnlessCost { pay: false })
        .unwrap();
    assert_eq!(
        zombie_count(&runner),
        1,
        "only the living decliner contributes"
    );
    expect_aggregate_payment_finished(&runner);
}

#[test]
fn scoped_unless_sacrifice_settles_earlier_declines_when_final_payer_is_eliminated() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Scoped Zombie Test", false, SCOPED_UNLESS_ZOMBIE)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();
    give_p0_sorcery_window(&mut runner);
    runner.cast(spell).resolve();
    expect_unless(&runner, P1);
    runner
        .act(GameAction::PayUnlessCost { pay: false })
        .expect("P1 declines before P2 leaves");
    expect_unless(&runner, P2);

    eliminate_player(runner.state_mut(), P2, &mut Vec::new());

    assert_eq!(
        zombie_count(&runner),
        1,
        "P1's earlier decline settles once"
    );
    expect_aggregate_payment_finished(&runner);
}

#[test]
fn scoped_unless_sacrifice_keeps_a_departed_decliners_tokens_owed() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Scoped Zombie Test", false, SCOPED_UNLESS_ZOMBIE)
        .with_mana_cost(ManaCost::zero())
        .id();
    let p2_creature = scenario.add_creature(P2, "P2 Sacrifice", 1, 1).id();
    let mut runner = scenario.build();
    give_p0_sorcery_window(&mut runner);
    runner.cast(spell).resolve();
    expect_unless(&runner, P1);
    runner
        .act(GameAction::PayUnlessCost { pay: false })
        .expect("P1 declines before leaving");
    expect_unless(&runner, P2);

    eliminate_player(runner.state_mut(), P1, &mut Vec::new());
    expect_unless(&runner, P2);
    pay_sacrifice(&mut runner, p2_creature);

    assert_eq!(
        zombie_count(&runner),
        1,
        "P1's completed decline remains owed after P1 leaves"
    );
    expect_aggregate_payment_finished(&runner);
}

#[test]
fn scoped_unless_sacrifice_abandons_when_its_original_controller_leaves() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Scoped Zombie Test", false, SCOPED_UNLESS_ZOMBIE)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();
    give_p0_sorcery_window(&mut runner);
    runner.cast(spell).resolve();
    expect_unless(&runner, P1);

    eliminate_player(runner.state_mut(), P0, &mut Vec::new());

    assert!(runner.state().pending_player_scope_unless_payment.is_none());
    assert_eq!(zombie_count(&runner), 0);
    assert!(
        !matches!(runner.state().waiting_for, WaitingFor::UnlessPayment { .. }),
        "the departed source controller cannot leave an aggregate prompt behind"
    );
}

#[test]
fn scoped_unless_sacrifice_resumes_after_a_graveyard_replacement_choice() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Scoped Zombie Test", false, SCOPED_UNLESS_ZOMBIE)
        .with_mana_cost(ManaCost::zero())
        .id();
    scenario.add_enchantment_from_oracle(P0, "Rest in Peace", REST_IN_PEACE);
    let colossus = scenario
        .add_creature_from_oracle(P1, "Darksteel Colossus", 11, 11, DARKSTEEL_COLOSSUS)
        .id();
    let mut runner = scenario.build();
    give_p0_sorcery_window(&mut runner);
    runner.cast(spell).resolve();
    expect_unless(&runner, P1);
    pay_sacrifice(&mut runner, colossus);

    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ReplacementChoice { .. }
    ));
    runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("replacement choice resumes the aggregate payment");
    expect_unless(&runner, P2);
    runner
        .act(GameAction::PayUnlessCost { pay: false })
        .unwrap();
    assert_eq!(zombie_count(&runner), 1);
    expect_aggregate_payment_finished(&runner);
}

#[test]
fn scoped_unless_sacrifice_abandons_a_payers_replacement_choice_when_controller_leaves() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Scoped Zombie Test", false, SCOPED_UNLESS_ZOMBIE)
        .with_mana_cost(ManaCost::zero())
        .id();
    scenario.add_enchantment_from_oracle(P0, "Rest in Peace", REST_IN_PEACE);
    let colossus = scenario
        .add_creature_from_oracle(P1, "Darksteel Colossus", 11, 11, DARKSTEEL_COLOSSUS)
        .id();
    let mut runner = scenario.build();
    give_p0_sorcery_window(&mut runner);
    runner.cast(spell).resolve();
    expect_unless(&runner, P1);
    pay_sacrifice(&mut runner, colossus);
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ReplacementChoice { player: P1, .. }
    ));

    eliminate_player(runner.state_mut(), P0, &mut Vec::new());

    assert!(runner.state().pending_player_scope_unless_payment.is_none());
    assert!(runner.state().pending_replacement.is_none());
    assert!(!runner.state().replacement_may_cost_paused);
    assert!(
        !runner.state().has_active_post_replacement_drain(),
        "the abandoned aggregate must not retain a replacement continuation"
    );
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
}
