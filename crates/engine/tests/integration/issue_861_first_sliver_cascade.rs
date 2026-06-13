//! Regression for issue #861: The First Sliver must grant cascade to Sliver
//! spells you cast while it is on the battlefield.
//!
//! https://github.com/phase-rs/phase/issues/861

use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::types::ability::Effect;
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, StackEntryKind};
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::statics::StaticMode;
use engine::types::zones::Zone;

const FIRST_SLIVER_ORACLE: &str = "Cascade (When you cast this spell, exile cards from the top of your library until you exile a nonland card that costs less. You may cast it without paying its mana cost. Put the exiled cards on the bottom in a random order.)\nSliver spells you cast have cascade.";

fn add_mana(runner: &mut GameRunner, mana: &[ManaType]) {
    let dummy = engine::types::identifiers::ObjectId(0);
    let pool = &mut runner
        .state_mut()
        .players
        .iter_mut()
        .find(|p| p.id == P0)
        .unwrap()
        .mana_pool;
    for m in mana {
        pool.add(ManaUnit::new(*m, dummy, false, vec![]));
    }
}

fn exile_count(
    state: &engine::types::game_state::GameState,
    player: engine::types::player::PlayerId,
) -> usize {
    state
        .objects
        .values()
        .filter(|obj| obj.controller == player && obj.zone == Zone::Exile)
        .count()
}

#[test]
fn first_sliver_oracle_parses_sliver_spell_cascade_grant() {
    let mut scenario = GameScenario::new();
    let first_sliver = scenario
        .add_creature_from_oracle(P0, "The First Sliver", 7, 7, FIRST_SLIVER_ORACLE)
        .id();
    let runner = scenario.build();
    let obj = &runner.state().objects[&first_sliver];
    assert!(
        obj.static_definitions.iter_unchecked().any(|def| {
            matches!(
                def.mode,
                StaticMode::CastWithKeyword {
                    keyword: engine::types::keywords::Keyword::Cascade
                }
            )
        }),
        "The First Sliver must grant cascade to Sliver spells, statics={:?}",
        obj.static_definitions
    );
}

#[test]
fn first_sliver_grants_cascade_to_sliver_spells_cast_from_hand() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Forest", "Forest", "Island"]);
    let first_sliver = scenario
        .add_creature_from_oracle(P0, "The First Sliver", 7, 7, FIRST_SLIVER_ORACLE)
        .id();

    let sliver_spell = scenario
        .add_creature_to_hand(P0, "Cheap Sliver", 1, 1)
        .with_subtypes(vec!["Sliver"])
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Red],
            generic: 1,
        })
        .id();

    let mut runner = scenario.build();
    assert_eq!(
        runner.state().objects[&first_sliver].zone,
        Zone::Battlefield
    );
    assert!(runner.state().objects[&sliver_spell]
        .card_types
        .subtypes
        .iter()
        .any(|s| s == "Sliver"));

    let exile_before = exile_count(runner.state(), P0);
    add_mana(&mut runner, &[ManaType::Colorless, ManaType::Red]);

    runner
        .act(GameAction::CastSpell {
            object_id: sliver_spell,
            card_id: runner.state().objects[&sliver_spell].card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast Cheap Sliver");

    for _ in 0..24 {
        match &runner.state().waiting_for {
            engine::types::game_state::WaitingFor::ManaPayment { .. } => {
                runner.act(GameAction::PassPriority).expect("pay mana");
            }
            engine::types::game_state::WaitingFor::Priority { .. } => break,
            other => panic!("unexpected cast prompt: {other:?}"),
        }
    }

    assert!(
        runner.state().stack.iter().any(|entry| {
            matches!(
                &entry.kind,
                StackEntryKind::TriggeredAbility { ability, .. }
                    if matches!(ability.effect, Effect::Cascade)
            )
        }),
        "granted cascade must be on the stack before resolution (stack={:?})",
        runner.state().stack
    );

    runner.advance_until_stack_empty();

    let exile_after = exile_count(runner.state(), P0);
    assert!(
        exile_after > exile_before,
        "cascade must exile at least one library card (before={exile_before}, after={exile_after})"
    );
}
