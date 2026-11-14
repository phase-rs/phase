//! CR 107.14 + CR 122.1: a period's resource measurement keys energy by the seat holding it.

use std::collections::BTreeMap;

use engine::analysis::resource::{CounterClass, ResourceAxis, ResourceVector};
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::AbilityCost;
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, ManaChoice, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaType;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

use crate::support::shared_card_db;

fn settle(runner: &mut GameRunner) {
    for _ in 0..16 {
        if runner.state().stack.is_empty() {
            return;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("the stack resolves");
    }
    panic!("the stack did not empty: {:?}", runner.state().stack);
}

fn play(runner: &mut GameRunner, land: ObjectId) {
    let card_id = runner.state().objects[&land].card_id;
    runner
        .act(GameAction::PlayLand {
            object_id: land,
            card_id,
        })
        .expect("the land is playable");
    settle(runner);
}

fn to_main_of(runner: &mut GameRunner, seat: PlayerId) {
    for _ in 0..400 {
        let state = runner.state();
        if state.active_player == seat
            && state.phase == Phase::PreCombatMain
            && state.stack.is_empty()
            && matches!(state.waiting_for, WaitingFor::Priority { player } if player == seat)
        {
            return;
        }
        let action = match &state.waiting_for {
            WaitingFor::DeclareAttackers { .. } => GameAction::DeclareAttackers {
                attacks: vec![],
                bands: vec![],
            },
            WaitingFor::DeclareBlockers { .. } => GameAction::DeclareBlockers {
                assignments: vec![],
            },
            _ => GameAction::PassPriority,
        };
        runner.act(action).expect("the turn advances");
    }
    panic!(
        "no precombat main for {seat:?}: {:?}",
        runner.state().waiting_for
    );
}

/// Aether Hub: "When this land enters, you get {E} (an energy counter). / {T}: Add {C}. /
/// {T}, Pay {E}: Add one mana of any color." Each seat's Hub gives its controller one counter,
/// and P0 paying {E} spends P0's alone, though P1 is the seat iterated last.
#[test]
fn each_seats_energy_is_measured_from_its_own_reserve() {
    let db = shared_card_db().expect("card db");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Island"; 10]);
    scenario.with_library_top(P1, &["Island"; 10]);
    let hub0 = scenario.add_real_card(P0, "Aether Hub", Zone::Hand, db);
    let hub1 = scenario.add_real_card(P1, "Aether Hub", Zone::Hand, db);
    let mut runner = scenario.build();

    let f0 = runner.state().clone();
    play(&mut runner, hub0);
    to_main_of(&mut runner, P1);
    play(&mut runner, hub1);
    let f1 = runner.state().clone();

    to_main_of(&mut runner, P0);
    let ability_index = runner.state().objects[&hub0]
        .abilities
        .iter()
        .position(|a| {
            matches!(&a.cost, Some(AbilityCost::Composite { costs })
                if costs.iter().any(|c| matches!(c, AbilityCost::PayEnergy { .. })))
        })
        .expect("Aether Hub's {T}, Pay {E} ability");
    runner
        .act(GameAction::ActivateAbility {
            source_id: hub0,
            ability_index,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("P0 activates its Hub");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ChooseManaColor { .. }
    ));
    runner
        .act(GameAction::ChooseManaColor {
            choice: ManaChoice::SingleColor(ManaType::Red),
            count: 1,
        })
        .expect("P0 chooses red");
    let f2 = runner.state().clone();

    let raw = |state: &engine::types::game_state::GameState| {
        (state.players[0].energy, state.players[1].energy)
    };
    assert_eq!((raw(&f1), raw(&f2)), ((1, 1), (0, 1)), "reach guard");

    let (s0, s1, s2) = (
        ResourceVector::snapshot(&f0),
        ResourceVector::snapshot(&f1),
        ResourceVector::snapshot(&f2),
    );
    assert_eq!(s0.energy, BTreeMap::from([(P0, 0), (P1, 0)]));
    assert_eq!(s1.energy, BTreeMap::from([(P0, 1), (P1, 1)]));
    assert_eq!(s2.energy, BTreeMap::from([(P0, 0), (P1, 1)]));
    assert!(ResourceVector::delta(&s0, &s0).energy.is_empty());
    assert_eq!(
        ResourceVector::delta(&s0, &s1).energy,
        BTreeMap::from([(P0, 1), (P1, 1)])
    );
    let spent = ResourceVector::delta(&s1, &s2);
    assert_eq!(spent.energy, BTreeMap::from([(P0, -1)]));
    let axes = spent.axis_components();
    assert!(axes.contains(&(ResourceAxis::Energy(P0), -1)), "{axes:?}");
    assert!(
        !axes
            .iter()
            .any(|(axis, _)| *axis == ResourceAxis::Energy(P1)),
        "{axes:?}"
    );
    for s in [&s0, &s1, &s2] {
        assert!(
            !s.counters
                .keys()
                .any(|(class, _)| *class == CounterClass::Energy),
            "{:?}",
            s.counters
        );
    }
}
