//! CR 701.24c + CR 400.3: Green Sun's Zenith — search your library for a green
//! creature with mana value X or less, put it onto the battlefield, then shuffle.
//! Shuffle ~ into its owner's library.
//!
//! Building-block regression: the terminal "Shuffle ~ into its owner's library"
//! clause must parse to a `ChangeZone` + `Shuffle` chain that routes ~ back to
//! its owner's library (CR 400.3), not lower to `Effect::Unimplemented`.

use engine::game::scenario::{GameScenario, P0};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

use crate::support::shared_card_db as load_db;

fn add_mana(runner: &mut engine::game::scenario::GameRunner, ty: ManaType, count: usize) {
    for _ in 0..count {
        runner.state_mut().players[0]
            .mana_pool
            .add(ManaUnit::new(ty, ObjectId(0), false, vec![]));
    }
}

#[test]
fn green_suns_zenith_search_then_shuffle_self_into_library() {
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let zenith = scenario.add_real_card(P0, "Green Sun's Zenith", Zone::Hand, db);
    // A legal target: 2-mana green creature. Llanowar Elves is mana-value 1.
    let llanowar = scenario.add_real_card(P0, "Llanowar Elves", Zone::Library, db);
    scenario.add_real_card(P0, "Forest", Zone::Library, db);

    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    // X=2 → cost is {X}{G}, so we need {2}{G} = 3 green sources here.
    add_mana(&mut runner, ManaType::Green, 3);

    let card_id = runner.state().objects[&zenith].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: zenith,
            card_id,
            targets: vec![],
        })
        .expect("Green Sun's Zenith cast should succeed");

    // CR 107.1b + CR 601.2f: X is announced before mana is paid. The cast flow
    // pauses in `ChooseXValue` until the caster commits a value.
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::ChooseXValue { .. }),
        "expected ChooseXValue immediately after casting Green Sun's Zenith"
    );
    runner
        .act(GameAction::ChooseX { value: 2 })
        .expect("X=2 commit should succeed");

    runner.advance_until_stack_empty();

    // Once the spell resolves we should hit the SearchChoice waiting state.
    // Pick Llanowar Elves.
    match &runner.state().waiting_for {
        WaitingFor::SearchChoice { cards, .. } => {
            assert!(
                cards.contains(&llanowar),
                "Llanowar Elves should be a legal Green Sun's Zenith search choice (CMC 1 ≤ X)"
            );
        }
        other => panic!("expected SearchChoice, got {other:?}"),
    }

    runner
        .act(GameAction::SelectCards {
            cards: vec![llanowar],
        })
        .expect("selecting Llanowar Elves should continue the resolution");

    runner.advance_until_stack_empty();

    // Llanowar Elves should be on the battlefield (search + ChangeZone).
    assert_eq!(
        runner.state().objects[&llanowar].zone,
        Zone::Battlefield,
        "Llanowar Elves should be on the battlefield after Green Sun's Zenith resolves"
    );

    // CR 400.3: Green Sun's Zenith itself must be shuffled into its owner's
    // library (P0's), NOT moved to the graveyard.
    assert_eq!(
        runner.state().objects[&zenith].zone,
        Zone::Library,
        "Green Sun's Zenith must be in its owner's library after the terminal shuffle clause (CR 400.3)"
    );
    assert!(
        runner.state().players[0].library.contains(&zenith),
        "Green Sun's Zenith must be inside P0's library list, not just zone-tagged"
    );
    assert!(
        !runner.state().players[0].graveyard.contains(&zenith),
        "Green Sun's Zenith must NOT be in the graveyard (this is the regression)"
    );
}
