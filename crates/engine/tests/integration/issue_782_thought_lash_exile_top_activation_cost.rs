//! Issue #782 — Thought Lash: "Exile the top card of your library: Prevent the
//! next 1 damage that would be dealt to you this turn."
//!
//! Reported as the activated ability resolving without its exile-the-top-card
//! cost being paid. CR 602.2b + CR 601.2h: activating an ability pays its total
//! cost, and an ability whose cost can't be paid can't be activated.

use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::types::ability::AbilityCost;
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

// Verbatim Oracle text (Scryfall, 2026-09-15).
const THOUGHT_LASH: &str = "Cumulative upkeep—Exile the top card of your library. (At the beginning of your upkeep, put an age counter on this permanent, then sacrifice it unless you pay its upkeep cost for each age counter on it.)\nWhen a player doesn't pay this enchantment's cumulative upkeep, that player exiles all cards from their library.\nExile the top card of your library: Prevent the next 1 damage that would be dealt to you this turn.";

fn thought_lash_with_library(library_top_first: &[&str]) -> (GameRunner, ObjectId, usize) {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    if !library_top_first.is_empty() {
        scenario.with_library_top(P0, library_top_first);
    }
    let lash = scenario
        .add_creature(P0, "Thought Lash", 0, 0)
        .as_enchantment()
        .from_oracle_text(THOUGHT_LASH)
        .id();
    let runner = scenario.build();
    // Reach-guard: the activated ability exists and its cost is the
    // top-of-library exile, so every later assertion is about paying it.
    let index = runner.state().objects[&lash]
        .abilities
        .iter()
        .position(|ability| {
            matches!(
                &ability.cost,
                Some(AbilityCost::Exile {
                    zone: Some(Zone::Library),
                    ..
                })
            )
        })
        .unwrap_or_else(|| {
            panic!(
                "Thought Lash must carry an exile-the-top-card activated ability; got {:?}",
                runner.state().objects[&lash]
                    .abilities
                    .iter()
                    .map(|a| (&a.cost, &a.effect))
                    .collect::<Vec<_>>()
            )
        });
    (runner, lash, index)
}

fn library_ids(runner: &GameRunner) -> Vec<ObjectId> {
    runner.state().players[0].library.iter().copied().collect()
}

#[test]
fn thought_lash_activation_exiles_the_top_card_of_your_library() {
    let (mut runner, lash, index) =
        thought_lash_with_library(&["Top Card", "Second Card", "Third Card"]);
    let before = library_ids(&runner);
    let top = *before.first().expect("reach-guard: library seeded");

    runner
        .act(GameAction::ActivateAbility {
            source_id: lash,
            ability_index: index,
        })
        .expect("activation with a nonempty library must be accepted");
    runner.advance_until_stack_empty();

    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "the activation settles at priority; got {:?}",
        runner.state().waiting_for
    );
    assert_eq!(
        runner.state().objects[&top].zone,
        Zone::Exile,
        "paying the cost exiles the top card of the library"
    );
    assert_eq!(
        library_ids(&runner),
        before[1..].to_vec(),
        "exactly the top card leaves the library"
    );
}

#[test]
fn thought_lash_cannot_be_activated_with_an_empty_library() {
    let (mut runner, lash, index) = thought_lash_with_library(&[]);
    assert!(
        library_ids(&runner).is_empty(),
        "reach-guard: the library is empty"
    );
    let stack_before = runner.state().stack.len();

    let result = runner.act(GameAction::ActivateAbility {
        source_id: lash,
        ability_index: index,
    });

    assert!(
        result.is_err() || runner.state().stack.len() == stack_before,
        "an unpayable exile-the-top-card cost must not put the ability on the stack; \
         result = {result:?}, stack = {:?}",
        runner.state().stack
    );
}
