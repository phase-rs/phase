//! Regression: Omnath, Locus of Mana (#2887) — "gets +1/+1 for each unspent
//! green mana you have" was parsed as a flat +1/+1 (the per-mana multiplier
//! dropped), so Omnath never grew with floating green mana.
//!
//! With the fix the static parses to `AddDynamicPower`/`AddDynamicToughness`
//! over `QuantityRef::UnspentMana { Green }`, which the layer system resolves
//! against the controller's mana pool. This drives the REAL mana-production and
//! layer-flush path: mana production marks layers dirty, and the P/T read after
//! derivation is computed by the engine from the floating green mana.
//!
//! CR references (verified against docs/MagicCompRules.txt):
//!   - CR 106.4: unspent mana stays in a player's mana pool.
//!   - CR 613.4c: dynamic power/toughness-modifying continuous effects.

use engine::game::layers::flush_layers;
use engine::game::mana_payment;
use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaType;
use engine::types::phase::Phase;

const OMNATH: &str = "Omnath, Locus of Mana gets +1/+1 for each unspent green mana you have.";

fn add_green(runner: &mut GameRunner, n: usize) {
    for _ in 0..n {
        let mut events = Vec::new();
        mana_payment::produce_mana(
            runner.state_mut(),
            ObjectId(0),
            ManaType::Green,
            P0,
            false,
            &mut events,
        );
    }
}

fn derived_power(runner: &mut GameRunner, id: ObjectId) -> i32 {
    flush_layers(runner.state_mut());
    runner
        .state()
        .objects
        .get(&id)
        .expect("Omnath exists")
        .power
        .expect("creature has power")
}

fn derived_toughness(runner: &mut GameRunner, id: ObjectId) -> i32 {
    flush_layers(runner.state_mut());
    runner
        .state()
        .objects
        .get(&id)
        .unwrap()
        .toughness
        .expect("creature has toughness")
}

#[test]
fn omnath_scales_with_unspent_green_mana() {
    let mut scenario = GameScenario::new_n_player(2, 7);
    scenario.at_phase(Phase::PreCombatMain);
    let omnath = scenario
        .add_creature_from_oracle(P0, "Omnath, Locus of Mana", 1, 1, OMNATH)
        .id();
    let mut runner = scenario.build();

    // No floating green mana → base 1/1 (the bug made this scale-less but the
    // flat +1/+1 would have read 2/2 here).
    assert_eq!(derived_power(&mut runner, omnath), 1, "0 green → 1/1 power");
    assert_eq!(
        derived_toughness(&mut runner, omnath),
        1,
        "0 green → 1/1 toughness"
    );

    // Three unspent green mana → +3/+3 = 4/4.
    add_green(&mut runner, 3);
    assert_eq!(derived_power(&mut runner, omnath), 4, "3 green → 4 power");
    assert_eq!(
        derived_toughness(&mut runner, omnath),
        4,
        "3 green → 4 toughness"
    );

    // Two more (five total) → +5/+5 = 6/6, proving it re-derives live.
    add_green(&mut runner, 2);
    assert_eq!(derived_power(&mut runner, omnath), 6, "5 green → 6 power");
    assert_eq!(
        derived_toughness(&mut runner, omnath),
        6,
        "5 green → 6 toughness"
    );
}

/// CR 106.4 + CR 101.1: Omnath keeps Forest's {G} past the turn boundary that truncates the
/// resolved-rules journal, so spending it re-verifies its pip.
#[test]
fn retained_green_mana_is_reverified_after_the_journal_resets() {
    use crate::loop_shortcut_mana_engine::mana_ability_index;
    use crate::support::shared_card_db;
    use engine::game::scenario::P1;
    use engine::game::scenario_db::GameScenarioDbExt;
    use engine::types::actions::GameAction;
    use engine::types::game_state::{CastPaymentMode, WaitingFor};
    use engine::types::zones::Zone;

    let db = shared_card_db().expect("the integration card fixture loads");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let omnath = scenario.add_real_card(P0, "Omnath, Locus of Mana", Zone::Battlefield, db);
    let forest = scenario.add_real_card(P0, "Forest", Zone::Battlefield, db);
    let growth = scenario.add_real_card(P0, "Giant Growth", Zone::Hand, db);
    let mut runner = scenario.build();
    let forest_idx = mana_ability_index(runner.state(), forest).expect("Forest taps for mana");
    runner
        .act(GameAction::ActivateAbility {
            source_id: forest,
            ability_index: forest_idx,
        })
        .expect("Forest's mana ability");
    let pip = runner.state().players[0]
        .mana_pool
        .units()
        .next()
        .expect("Forest's {G} floats")
        .pip_id;
    assert!(runner.state().resolved_rules_journal.has_produced_pip(pip));

    for _ in 0..80 {
        let state = runner.state();
        if state.active_player == P1
            && matches!(state.waiting_for, WaitingFor::Priority { player } if player == P0)
        {
            break;
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
    let state = runner.state();
    assert_eq!(state.active_player, P1, "reach: the turn passed");
    assert_eq!(
        state.players[0]
            .mana_pool
            .units()
            .map(|unit| (unit.pip_id, unit.color))
            .collect::<Vec<_>>(),
        [(pip, ManaType::Green)],
        "reach: the pool holds exactly the retained {{G}}"
    );
    assert!(
        !state.resolved_rules_journal.has_produced_pip(pip),
        "reach: the journal reset dropped the {{G}}'s producer"
    );
    assert!(
        !state.players[0].mana_pool.is_verified(),
        "the journal reset leaves the retained pip unverified"
    );
    let power_before = derived_power(&mut runner, omnath);
    let toughness_before = derived_toughness(&mut runner, omnath);

    let card_id = runner.state().objects[&growth].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: growth,
            card_id,
            targets: vec![omnath],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("Giant Growth is castable with the retained {G}");
    for _ in 0..20 {
        if runner.state().stack.is_empty() {
            break;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("priority passes");
    }
    assert!(runner.state().stack.is_empty(), "Giant Growth resolves");
    assert_eq!(
        (
            derived_power(&mut runner, omnath),
            derived_toughness(&mut runner, omnath)
        ),
        (power_before - 1 + 3, toughness_before - 1 + 3),
        "Omnath loses the spent {{G}}'s +1/+1 and gains +3/+3"
    );
    let journal = &runner.state().resolved_rules_journal;
    assert!(
        journal.has_produced_pip(pip),
        "spending re-verified the pip"
    );
    assert_eq!(journal.spent_mana().len(), 1, "the {{G}} was spent once");
}
