//! Integration regression for GitHub issue #5978 — Roar of the Fifth People,
//! chapter II.
//!
//! Oracle: `This Saga gains "Creatures you control have '{T}: Add {R}, {G}, or
//! {W}.'"`
//!
//! Parser coverage lives in `oracle_saga.rs`; this test drives the production
//! Saga pipeline (lore-counter turn-based action → chapter-II `CounterAdded`
//! trigger → `GenericEffect` grant) and verifies a creature can activate the
//! granted tap-for-mana ability at runtime.

use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::triggers::drain_order_triggers_with_identity;
use engine::types::ability::{AbilityCost, AbilityKind, Effect};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::{ManaChoice, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaType;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const ROAR_ORACLE: &str = "(As this Saga enters and after your draw step, add a lore counter. Sacrifice after IV.)\nI — Create two 3/3 green Dinosaur creature tokens.\nII — This Saga gains \"Creatures you control have '{T}: Add {R}, {G}, or {W}.'\"\nIII — Search your library for a Dinosaur card, reveal it, put it into your hand, then shuffle.\nIV — Dinosaurs you control gain double strike and trample until end of turn.";

fn lore_count(runner: &GameRunner, saga_id: ObjectId) -> u32 {
    runner
        .state()
        .objects
        .get(&saga_id)
        .and_then(|obj| obj.counters.get(&CounterType::Lore).copied())
        .unwrap_or(0)
}

fn park_for_next_p0_precombat_main(runner: &mut GameRunner) {
    let state = runner.state_mut();
    state.turn_number = 1;
    state.active_player = P0;
    state.phase = Phase::End;
    state.priority_player = P0;
    state.waiting_for = WaitingFor::Priority { player: P0 };
}

fn trigger_chapter_two_via_saga_lore_counter(runner: &mut GameRunner, saga_id: ObjectId) {
    park_for_next_p0_precombat_main(runner);
    runner.advance_to_phase(Phase::PreCombatMain);
    runner.pass_both_players();
    runner.advance_to_phase(Phase::PreCombatMain);

    assert_eq!(
        lore_count(runner, saga_id),
        2,
        "CR 714.3c must add the Saga's second lore counter before chapter II fires"
    );
    assert!(
        !runner.state().stack.is_empty(),
        "chapter II CounterAdded trigger must be on the stack"
    );

    for _ in 0..32 {
        if matches!(runner.state().waiting_for, WaitingFor::OrderTriggers { .. }) {
            drain_order_triggers_with_identity(runner.state_mut());
        }
        if runner.state().stack.is_empty() {
            break;
        }
        if matches!(runner.state().waiting_for, WaitingFor::Priority { .. }) {
            let _ = runner.act(GameAction::PassPriority);
            let _ = runner.act(GameAction::PassPriority);
        } else {
            break;
        }
    }

    assert!(
        runner.state().stack.is_empty(),
        "chapter II must fully resolve before testing the granted creature ability, got waiting_for={:?}",
        runner.state().waiting_for
    );

    // CR 611.2c: nested `GrantStaticAbility` inside a saga chapter grant may
    // require a layer pass after the transient effect registers before the inner
    // `GrantAbility` reaches creatures you control.
    evaluate_layers(runner.state_mut());

    assert!(
        !runner.state().transient_continuous_effects.is_empty(),
        "chapter II must register at least one transient continuous effect on the saga, got {:?}",
        runner.state().transient_continuous_effects
    );
}

fn granted_tap_mana_ability_index(runner: &GameRunner, creature: ObjectId) -> usize {
    let abilities = &runner.state().objects[&creature].abilities;
    abilities
        .iter()
        .position(|def| {
            def.kind == AbilityKind::Activated
                && def.cost == Some(AbilityCost::Tap)
                && matches!(&*def.effect, Effect::Mana { .. })
        })
        .unwrap_or_else(|| {
            panic!(
                "creature must carry Roar chapter II granted tap mana ability; abilities={abilities:?}"
            )
        })
}

#[test]
fn roar_chapter_two_creature_taps_for_granted_mana_via_saga_pipeline() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let saga_id = scenario
        .add_creature(P0, "Roar of the Fifth People", 0, 0)
        .as_enchantment()
        .with_subtypes(vec!["Saga"])
        .from_oracle_text(ROAR_ORACLE)
        .id();
    let creature = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();

    let plains = ["Plains"; 10];
    scenario.with_library_top(P0, &plains);
    scenario.with_library_top(P1, &plains);

    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&saga_id)
        .unwrap()
        .counters
        .insert(CounterType::Lore, 1);

    trigger_chapter_two_via_saga_lore_counter(&mut runner, saga_id);

    let ability_index = granted_tap_mana_ability_index(&runner, creature);

    runner
        .act(GameAction::ActivateAbility {
            source_id: creature,
            ability_index,
        })
        .expect("activate Roar chapter II granted tap mana ability");

    match &runner.state().waiting_for {
        WaitingFor::ChooseManaColor { choice, .. } => {
            let color = match choice {
                engine::types::game_state::ManaChoicePrompt::SingleColor { options } => options
                    .iter()
                    .find(|c| matches!(c, ManaType::Red | ManaType::Green | ManaType::White))
                    .copied()
                    .unwrap_or(ManaType::Red),
                _ => ManaType::Red,
            };
            runner
                .act(GameAction::ChooseManaColor {
                    choice: ManaChoice::SingleColor(color),
                    count: 1,
                })
                .expect("choose one of the granted R/G/W colors");
        }
        other => panic!("granted mana ability must prompt for color choice, got {other:?}"),
    }

    let state = runner.state();
    assert!(
        state.players[P0.0 as usize]
            .mana_pool
            .count_color(ManaType::Red)
            + state.players[P0.0 as usize]
                .mana_pool
                .count_color(ManaType::Green)
            + state.players[P0.0 as usize]
                .mana_pool
                .count_color(ManaType::White)
            >= 1,
        "Roar chapter II must produce one of {{R}}/{{G}}/{{W}}, got pool {:?}",
        state.players[P0.0 as usize].mana_pool
    );
    assert!(
        state.objects[&creature].tapped,
        "activating the granted mana ability must tap the creature"
    );
    assert_eq!(
        state.objects[&creature].zone,
        Zone::Battlefield,
        "the mana source must stay on the battlefield"
    );
}
