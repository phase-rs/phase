//! Aggregate player-scoped sacrifice-unless token creation.
//!
//! The Oracle grammar is the reusable clause printed by Acererak the Archlich:
//! each opponent decides in APNAP order, and only after every answer does the
//! source's controller create one Zombie per decline, while the chosen
//! creatures are sacrificed at the same time (CR 101.4).

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
const DAUTHI_VOIDWALKER: &str = "Shadow (This creature can block or be blocked by only creatures with shadow.)\nIf a card would be put into an opponent's graveyard from anywhere, instead exile it with a void counter on it.\n{T}, Sacrifice this creature: Choose an exiled card an opponent owns with a void counter on it. You may play it this turn without paying its mana cost.";
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
    let _p1_creature = all_decline.add_creature(P1, "P1 Sacrifice", 1, 1).id();
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
    let _p1_creature = scenario.add_creature(P1, "P1 Sacrifice", 1, 1).id();
    let _p2_creature = scenario.add_creature(P2, "P2 Sacrifice", 1, 1).id();
    let mut runner = scenario.build();
    give_p0_sorcery_window(&mut runner);
    runner.cast(spell).resolve();
    expect_unless(&runner, P1);

    eliminate_player(
        runner.state_mut(),
        P1,
        engine::types::events::LossReason::Conceded,
        &mut Vec::new(),
    );
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
    let _p2_creature = scenario.add_creature(P2, "P2 Sacrifice", 1, 1).id();
    let mut runner = scenario.build();
    give_p0_sorcery_window(&mut runner);
    runner.cast(spell).resolve();
    expect_unless(&runner, P1);
    runner
        .act(GameAction::PayUnlessCost { pay: false })
        .expect("P1 declines before P2 leaves");
    expect_unless(&runner, P2);

    eliminate_player(
        runner.state_mut(),
        P2,
        engine::types::events::LossReason::Conceded,
        &mut Vec::new(),
    );

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

    eliminate_player(
        runner.state_mut(),
        P1,
        engine::types::events::LossReason::Conceded,
        &mut Vec::new(),
    );
    expect_unless(&runner, P2);
    pay_sacrifice(&mut runner, p2_creature);

    assert_eq!(
        zombie_count(&runner),
        1,
        "P1's completed decline remains owed after P1 leaves"
    );
    expect_aggregate_payment_finished(&runner);
}

/// CR 608.2m + CR 800.4a: the source's controller leaves while P1 is asked.
/// The instruction had already started to resolve and continues fully: P1 and
/// P2 still answer and P1's chosen creature is sacrificed, but the Zombie owed
/// for P2's decline is not created for the departed controller (CR 800.4b).
#[test]
fn scoped_unless_sacrifice_continues_when_its_original_controller_leaves() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Scoped Zombie Test", false, SCOPED_UNLESS_ZOMBIE)
        .with_mana_cost(ManaCost::zero())
        .id();
    let p1_creature = scenario.add_creature(P1, "P1 Sacrifice", 1, 1).id();
    let p2_creature = scenario.add_creature(P2, "P2 Sacrifice", 1, 1).id();
    let mut runner = scenario.build();
    give_p0_sorcery_window(&mut runner);
    runner.cast(spell).resolve();
    expect_unless(&runner, P1);

    eliminate_player(
        runner.state_mut(),
        P0,
        engine::types::events::LossReason::Conceded,
        &mut Vec::new(),
    );

    expect_unless(&runner, P1);
    assert!(
        runner.state().pending_player_scope_unless_payment.is_some(),
        "CR 608.2m: the poll outlives its controller"
    );
    pay_sacrifice(&mut runner, p1_creature);
    expect_unless(&runner, P2);
    assert_eq!(
        runner.state().objects[&p1_creature].zone,
        Zone::Battlefield,
        "CR 101.4: nothing moves before the last opponent has chosen"
    );
    runner
        .act(GameAction::PayUnlessCost { pay: false })
        .expect("P2 declines");

    assert_eq!(
        runner.state().objects[&p1_creature].zone,
        Zone::Graveyard,
        "P1's chosen creature is sacrificed"
    );
    assert_eq!(runner.state().objects[&p2_creature].zone, Zone::Battlefield);
    assert_eq!(
        runner
            .state()
            .battlefield
            .iter()
            .filter(|id| runner.state().objects[id].name == "Zombie")
            .count(),
        0,
        "CR 800.4b: no token is created for the departed controller"
    );
    expect_aggregate_payment_finished(&runner);
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
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
    let _p2_creature = scenario.add_creature(P2, "P2 Sacrifice", 1, 1).id();
    let mut runner = scenario.build();
    give_p0_sorcery_window(&mut runner);
    runner.cast(spell).resolve();
    expect_unless(&runner, P1);
    pay_sacrifice(&mut runner, colossus);

    // CR 101.4: P1's chosen Colossus is not sacrificed while P2 chooses.
    expect_unless(&runner, P2);
    assert_eq!(runner.state().objects[&colossus].zone, Zone::Battlefield);
    runner
        .act(GameAction::PayUnlessCost { pay: false })
        .unwrap();

    // CR 616.1: the settled sacrifice asks P1 to order the replacements, and
    // the token for P2's decline waits for that answer.
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ReplacementChoice { player: P1, .. }
    ));
    assert_eq!(zombie_count(&runner), 0);
    runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("replacement choice resumes the aggregate settlement");
    assert_ne!(
        runner.state().objects.get(&colossus).map(|o| o.zone),
        Some(Zone::Battlefield)
    );
    assert_eq!(zombie_count(&runner), 1);
    expect_aggregate_payment_finished(&runner);
}

/// CR 608.2m + CR 800.4a: the poll has settled and the announced sacrifice is
/// paused on P1's CR 616.1 ordering when the source's controller leaves. The
/// resolution continues fully: P1's question stands and their sacrifice
/// completes, but no token is created for the departed controller.
///
/// Rest in Peace is P2's, so the departure leaves both of P1's options in the
/// game: had it left with P0, only Darksteel Colossus's replacement would
/// remain and there would be no choice to make (CR 616.1, CR 800.4a).
#[test]
fn scoped_unless_sacrifice_finishes_a_payers_replacement_choice_after_controller_leaves() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Scoped Zombie Test", false, SCOPED_UNLESS_ZOMBIE)
        .with_mana_cost(ManaCost::zero())
        .id();
    scenario.add_enchantment_from_oracle(P2, "Rest in Peace", REST_IN_PEACE);
    let colossus = scenario
        .add_creature_from_oracle(P1, "Darksteel Colossus", 11, 11, DARKSTEEL_COLOSSUS)
        .id();
    let _p2_creature = scenario.add_creature(P2, "P2 Sacrifice", 1, 1).id();
    let mut runner = scenario.build();
    give_p0_sorcery_window(&mut runner);
    runner.cast(spell).resolve();
    expect_unless(&runner, P1);
    pay_sacrifice(&mut runner, colossus);
    expect_unless(&runner, P2);
    runner
        .act(GameAction::PayUnlessCost { pay: false })
        .unwrap();
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ReplacementChoice { player: P1, .. }
    ));

    eliminate_player(
        runner.state_mut(),
        P0,
        engine::types::events::LossReason::Conceded,
        &mut Vec::new(),
    );
    assert!(runner.state().pending_player_scope_unless_payment.is_none());
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ReplacementChoice { player: P1, .. }
    ));
    runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("P1 still orders the replacements for their sacrifice");

    assert_ne!(
        runner.state().objects.get(&colossus).map(|o| o.zone),
        Some(Zone::Battlefield),
        "the announced sacrifice still happens"
    );
    assert_eq!(
        runner
            .state()
            .battlefield
            .iter()
            .filter(|id| runner.state().objects[id].name == "Zombie")
            .count(),
        0,
        "no token is created for the departed controller"
    );
    assert!(runner.state().pending_replacement.is_none());
    assert!(runner
        .state()
        .pending_player_scope_sacrifice_choice
        .is_none());
    assert!(
        !runner.state().has_active_post_replacement_drain(),
        "the settled batch must not retain a replacement continuation"
    );
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
}

/// CR 101.4 + Acererak the Archlich's ruling: "once all opponents have chosen,
/// creatures are sacrificed and tokens are created simultaneously". P1's
/// chosen creature stays on the battlefield while P2 decides.
#[test]
fn scoped_unless_sacrifice_waits_for_every_opponent_before_sacrificing() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Scoped Zombie Test", false, SCOPED_UNLESS_ZOMBIE)
        .with_mana_cost(ManaCost::zero())
        .id();
    let p1_creature = scenario.add_creature(P1, "P1 Sacrifice", 1, 1).id();
    let p2_creature = scenario.add_creature(P2, "P2 Sacrifice", 1, 1).id();
    let mut runner = scenario.build();
    give_p0_sorcery_window(&mut runner);
    runner.cast(spell).resolve();

    expect_unless(&runner, P1);
    pay_sacrifice(&mut runner, p1_creature);
    expect_unless(&runner, P2);
    assert_eq!(
        runner.state().objects[&p1_creature].zone,
        Zone::Battlefield,
        "CR 101.4: nothing is sacrificed before the last opponent has chosen"
    );
    runner
        .act(GameAction::PayUnlessCost { pay: false })
        .expect("P2 declines");

    assert_eq!(runner.state().objects[&p1_creature].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&p2_creature].zone, Zone::Battlefield);
    assert_eq!(zombie_count(&runner), 1, "P2's decline owes one Zombie");
    expect_aggregate_payment_finished(&runner);
    assert!(runner.state().stack.is_empty());
}

/// CR 101.4 + CR 614.1: the paying opponents' creatures are sacrificed as one
/// batch, so P1's Dauthi Voidwalker is still on the battlefield when P2's
/// creature would go to P2's graveyard, and exiles it with a void counter.
#[test]
fn scoped_unless_sacrifice_settles_the_sacrifices_simultaneously() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Scoped Zombie Test", false, SCOPED_UNLESS_ZOMBIE)
        .with_mana_cost(ManaCost::zero())
        .id();
    let walker = scenario
        .add_creature_from_oracle(P1, "Dauthi Voidwalker", 3, 2, DAUTHI_VOIDWALKER)
        .id();
    let p2_creature = scenario.add_creature(P2, "P2 Sacrifice", 1, 1).id();
    let mut runner = scenario.build();
    give_p0_sorcery_window(&mut runner);
    runner.cast(spell).resolve();

    expect_unless(&runner, P1);
    pay_sacrifice(&mut runner, walker);
    expect_unless(&runner, P2);
    pay_sacrifice(&mut runner, p2_creature);

    assert_eq!(runner.state().objects[&walker].zone, Zone::Graveyard);
    assert_eq!(
        runner.state().objects[&p2_creature].zone,
        Zone::Exile,
        "Dauthi Voidwalker still applies to the co-sacrificed creature"
    );
    assert_eq!(
        runner.state().objects[&p2_creature]
            .counters
            .get(&engine::types::counter::CounterType::Generic(
                "void".to_string()
            ))
            .copied(),
        Some(1)
    );
    assert_eq!(zombie_count(&runner), 0, "both opponents paid");
    expect_aggregate_payment_finished(&runner);
}

/// CR 101.4 + CR 616.1: the paying opponents' sacrifices are all consulted
/// before any moves. P2 controls Rest in Peace. P1 orders the replacements for
/// P1's Voidwalker and picks Rest in Peace; that answer moves nothing yet, so
/// P1's Voidwalker still replaces P2's Voidwalker's move when P2 orders theirs.
#[test]
fn scoped_unless_sacrifice_replacement_answer_waits_for_the_whole_batch() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Scoped Zombie Test", false, SCOPED_UNLESS_ZOMBIE)
        .with_mana_cost(ManaCost::zero())
        .id();
    let p1_walker = scenario
        .add_creature_from_oracle(P1, "Dauthi Voidwalker", 3, 2, DAUTHI_VOIDWALKER)
        .id();
    let p2_walker = scenario
        .add_creature_from_oracle(P2, "Dauthi Voidwalker", 3, 2, DAUTHI_VOIDWALKER)
        .id();
    let rest_in_peace = scenario
        .add_enchantment_from_oracle(P2, "Rest in Peace", REST_IN_PEACE)
        .id();
    let mut runner = scenario.build();
    give_p0_sorcery_window(&mut runner);
    runner.cast(spell).resolve();

    expect_unless(&runner, P1);
    pay_sacrifice(&mut runner, p1_walker);
    expect_unless(&runner, P2);
    pay_sacrifice(&mut runner, p2_walker);

    choose_replacement_from(&mut runner, P1, rest_in_peace);
    assert_eq!(
        runner.state().objects[&p1_walker].zone,
        Zone::Battlefield,
        "CR 101.4: an answered replacement does not move its permanent ahead of the batch"
    );
    choose_replacement_from(&mut runner, P2, p1_walker);

    assert_eq!(runner.state().objects[&p1_walker].zone, Zone::Exile);
    assert_eq!(runner.state().objects[&p2_walker].zone, Zone::Exile);
    assert_eq!(
        runner.state().objects[&p2_walker]
            .counters
            .get(&engine::types::counter::CounterType::Generic(
                "void".to_string()
            ))
            .copied(),
        Some(1),
        "P1's co-sacrificed Voidwalker replaced P2's Voidwalker's move"
    );
    assert_eq!(zombie_count(&runner), 0, "both opponents paid");
    expect_aggregate_payment_finished(&runner);
    assert!(runner.state().stack.is_empty());
}

fn choose_replacement_from(runner: &mut GameRunner, chooser: PlayerId, source: ObjectId) {
    let index = match &runner.state().waiting_for {
        WaitingFor::ReplacementChoice {
            player, candidates, ..
        } => {
            assert_eq!(
                *player, chooser,
                "CR 616.1: the affected permanent's controller orders the replacements"
            );
            candidates
                .iter()
                .position(|candidate| candidate.source_id == source)
                .unwrap_or_else(|| {
                    panic!("{source:?}'s replacement must apply to {chooser:?}'s sacrifice")
                })
        }
        other => panic!("expected {chooser:?}'s replacement choice, got {other:?}"),
    };
    runner
        .act(GameAction::ChooseReplacement { index })
        .expect("the offered replacement is a legal answer");
}
