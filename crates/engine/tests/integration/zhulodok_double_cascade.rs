//! Regression for issue #6005: Zhulodok, Void Gorger grants "Cascade, cascade"
//! to the colorless MV7+ spells you cast from hand, so such a spell must trigger
//! cascade TWICE (CR 702.85c: each instance of cascade triggers separately).
//!
//! Before the fix the quoted, doubled keyword grant parsed to `Unimplemented`
//! (the card did nothing); after the parser fix it emitted two
//! `CastWithKeyword { Cascade }` statics, but the casting-time keyword merge
//! folded the two granted Cascades back to one by kind, so only one cascade
//! fired. This test pins the end-to-end behavior: two cascade triggers on the
//! stack.
//!
//! https://github.com/phase-rs/phase/issues/6005

use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::types::ability::Effect;
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, StackEntryKind};
use engine::types::mana::{ManaCost, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const ZHULODOK_ORACLE: &str = "Colorless spells you cast from your hand with mana value 7 or greater have \"Cascade, cascade.\"";

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

fn cascade_triggers_on_stack(state: &engine::types::game_state::GameState) -> usize {
    state
        .stack
        .iter()
        .filter(|entry| {
            matches!(
                &entry.kind,
                StackEntryKind::TriggeredAbility { ability, .. }
                    if matches!(ability.effect, Effect::Cascade)
            )
        })
        .count()
}

#[test]
fn zhulodok_grants_two_cascades_to_colorless_seven_drop_from_hand() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // A deep, mostly-land library so each of the two cascades can dig.
    scenario.with_library_top(
        P0,
        &[
            "Forest", "Forest", "Forest", "Forest", "Forest", "Forest", "Forest", "Forest",
        ],
    );
    let zhulodok = scenario
        .add_creature_from_oracle(P0, "Zhulodok, Void Gorger", 7, 3, ZHULODOK_ORACLE)
        .id();

    // A colorless (no colored mana), mana-value-7 creature spell in hand — the
    // exact class Zhulodok's grant targets.
    let seven_drop = scenario
        .add_creature_to_hand(P0, "Colorless Behemoth", 5, 5)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![],
            generic: 7,
        })
        .id();

    let mut runner = scenario.build();
    assert_eq!(runner.state().objects[&zhulodok].zone, Zone::Battlefield);

    add_mana(&mut runner, &[ManaType::Colorless; 7]);
    runner
        .act(GameAction::CastSpell {
            object_id: seven_drop,
            card_id: runner.state().objects[&seven_drop].card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast the colorless seven-drop");

    for _ in 0..24 {
        match &runner.state().waiting_for {
            engine::types::game_state::WaitingFor::ManaPayment { .. } => {
                runner.act(GameAction::PassPriority).expect("pay mana");
            }
            engine::types::game_state::WaitingFor::Priority { .. } => break,
            other => panic!("unexpected cast prompt: {other:?}"),
        }
    }

    // CR 702.85c: "Cascade, cascade" is two instances, each triggers separately.
    assert_eq!(
        cascade_triggers_on_stack(runner.state()),
        2,
        "Zhulodok's granted \"Cascade, cascade\" must put TWO cascade triggers on \
         the stack, not one (stack={:?})",
        runner.state().stack
    );
}
