//! Integration regression for GitHub issue #5977 — The Hunger Tide Rises,
//! chapter IV.
//!
//! Oracle: "Sacrifice any number of creatures. Search your library and/or
//! graveyard for a creature card with mana value less than or equal to the
//! number of creatures sacrificed this way and put it onto the battlefield.
//! If you search your library this way, shuffle."
//!
//! Reported bug: the search/pick step misbehaved — a bare-`and` split left a
//! stray `ChangeZone { target: ParentTarget }` sibling that could duplicate the
//! battlefield put during resolution.
//!
//! Parser regressions in `oracle_effect` assert the lowered AST shape; this
//! module drives the production Saga pipeline: lore-counter turn-based action →
//! chapter-IV `CounterAdded` trigger on the stack → sacrifice choice →
//! multi-zone `SearchChoice` → single battlefield arrival for the picked card.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::triggers::drain_order_triggers_with_identity;
use engine::game::zones::create_object;
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::counter::CounterType;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const HUNGER_TIDE_ORACLE: &str = "(As this Saga enters and after your draw step, add a lore counter. Sacrifice after IV.)\nI, II, III — Create a 1/1 black and green Insect creature token.\nIV — Sacrifice any number of creatures. Search your library and/or graveyard for a creature card with mana value less than or equal to the number of creatures sacrificed this way and put it onto the battlefield. If you search your library this way, shuffle.";

fn add_library_creature(
    state: &mut GameState,
    card_id: u64,
    player: PlayerId,
    name: &str,
    mana_cost: ManaCost,
) -> ObjectId {
    let id = create_object(
        state,
        CardId(card_id),
        player,
        name.to_string(),
        Zone::Library,
    );
    let obj = state.objects.get_mut(&id).unwrap();
    obj.card_types.core_types.push(CoreType::Creature);
    obj.base_card_types = obj.card_types.clone();
    obj.mana_cost = mana_cost.clone();
    obj.base_mana_cost = mana_cost;
    id
}

fn battlefield_creature_count(state: &GameState) -> usize {
    state
        .objects
        .values()
        .filter(|obj| {
            obj.zone == Zone::Battlefield && obj.card_types.core_types.contains(&CoreType::Creature)
        })
        .count()
}

fn lore_count(runner: &GameRunner, saga_id: ObjectId) -> u32 {
    runner
        .state()
        .objects
        .get(&saga_id)
        .and_then(|obj| obj.counters.get(&CounterType::Lore).copied())
        .unwrap_or(0)
}

/// Park the game so the next precombat main phase belongs to P0 again and CR
/// 714.3c can add the Saga's fourth lore counter (mirrors `three_blind_mice`).
fn park_for_next_p0_precombat_main(runner: &mut GameRunner) {
    let state = runner.state_mut();
    state.turn_number = 1;
    state.active_player = P0;
    state.phase = Phase::End;
    state.priority_player = P0;
    state.waiting_for = WaitingFor::Priority { player: P0 };
}

fn trigger_chapter_iv_via_saga_lore_counter(runner: &mut GameRunner, saga_id: ObjectId) {
    park_for_next_p0_precombat_main(runner);
    runner.advance_to_phase(Phase::PreCombatMain); // P1 precombat main (turn 2)
    runner.pass_both_players();
    runner.advance_to_phase(Phase::PreCombatMain); // P0 precombat main (turn 3)

    assert_eq!(
        lore_count(runner, saga_id),
        4,
        "CR 714.3c must add the Saga's fourth lore counter before chapter IV fires"
    );
    assert!(
        !runner.state().stack.is_empty(),
        "chapter IV CounterAdded trigger must be on the stack"
    );
}

fn pass_priority_while_legal(runner: &mut GameRunner) {
    if matches!(runner.state().waiting_for, WaitingFor::Priority { .. }) {
        let _ = runner.act(GameAction::PassPriority);
        let _ = runner.act(GameAction::PassPriority);
    }
}

fn drain_order_triggers(runner: &mut GameRunner) {
    if matches!(runner.state().waiting_for, WaitingFor::OrderTriggers { .. }) {
        drain_order_triggers_with_identity(runner.state_mut());
    }
}

fn resolve_chapter_iv_choices(
    runner: &mut GameRunner,
    sacrifice: &[ObjectId],
    search_pick: ObjectId,
) {
    for _ in 0..64 {
        drain_order_triggers(runner);

        match &runner.state().waiting_for {
            WaitingFor::EffectZoneChoice {
                player,
                count,
                min_count,
                up_to,
                cards,
                ..
            } => {
                assert_eq!(*player, P0);
                assert_eq!(*min_count, 0, "any number includes zero (CR 107.1c)");
                assert!(*up_to, "Sacrifice any number uses variable selection");
                assert!(
                    *count >= sacrifice.len(),
                    "eligible sacrifice pool must cover chosen permanents"
                );
                for id in sacrifice {
                    assert!(
                        cards.contains(id),
                        "sacrifice candidate {id:?} must be legal, got {cards:?}"
                    );
                }
                runner
                    .act(GameAction::SelectCards {
                        cards: sacrifice.to_vec(),
                    })
                    .expect("sacrifice selection accepted");
                assert_eq!(
                    runner.state().last_effect_count,
                    Some(sacrifice.len() as i32),
                    "sacrificed-this-way count must stamp for the CMC cap"
                );
                for id in sacrifice {
                    assert!(
                        runner.state().players[0].graveyard.contains(id),
                        "sacrificed creature {id:?} must be in graveyard"
                    );
                }
            }
            WaitingFor::SearchChoice {
                player,
                cards,
                count,
                ..
            } => {
                assert_eq!(*player, P0);
                assert_eq!(*count, 1, "chapter IV finds exactly one creature card");
                assert!(
                    cards.contains(&search_pick),
                    "picked card must be a legal search candidate, got {cards:?}"
                );
                runner
                    .act(GameAction::SelectCards {
                        cards: vec![search_pick],
                    })
                    .expect("search pick resolves the put-step continuation");
            }
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
            WaitingFor::Priority { .. } => pass_priority_while_legal(runner),
            _ => pass_priority_while_legal(runner),
        }
    }

    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "chapter IV must finish after the search put (and shuffle when applicable), got {:?}",
        runner.state().waiting_for
    );
}

#[test]
fn hunger_tide_chapter_iv_library_search_puts_creature_once() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let fodder_a = scenario
        .add_creature(P0, "Fodder A", 2, 2)
        .with_mana_cost(ManaCost::generic(3))
        .id();
    let fodder_b = scenario
        .add_creature(P0, "Fodder B", 2, 2)
        .with_mana_cost(ManaCost::generic(3))
        .id();
    let saga_id = scenario
        .add_creature(P0, "The Hunger Tide Rises", 0, 0)
        .as_enchantment()
        .with_subtypes(vec!["Saga"])
        .from_oracle_text(HUNGER_TIDE_ORACLE)
        .id();
    let plains = ["Plains"; 10];
    scenario.with_library_top(P0, &plains);
    scenario.with_library_top(P1, &plains);
    let mut runner = scenario.build();
    let state = runner.state_mut();
    state
        .objects
        .get_mut(&saga_id)
        .unwrap()
        .counters
        .insert(CounterType::Lore, 3);
    let finder = add_library_creature(state, 20, P0, "Library Finder", ManaCost::generic(2));
    let too_expensive = add_library_creature(state, 21, P0, "Too Expensive", ManaCost::generic(5));

    trigger_chapter_iv_via_saga_lore_counter(&mut runner, saga_id);
    resolve_chapter_iv_choices(&mut runner, &[fodder_a, fodder_b], finder);

    let state = runner.state();
    assert_eq!(
        state.objects[&finder].zone,
        Zone::Battlefield,
        "library pick must enter the battlefield"
    );
    assert_eq!(
        battlefield_creature_count(state),
        1,
        "only the searched creature may remain on the battlefield"
    );
    assert_eq!(
        state.objects[&too_expensive].zone,
        Zone::Library,
        "over-CMC library card must stay in the library"
    );
    assert!(
        !state.players[0].library.contains(&finder),
        "found card must leave the library"
    );
}

#[test]
fn hunger_tide_chapter_iv_graveyard_search_puts_creature_once() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let fodder = scenario
        .add_creature(P0, "Fodder", 3, 3)
        .with_mana_cost(ManaCost::generic(3))
        .id();
    let graveyard_finder = scenario
        .add_creature_to_graveyard(P0, "Graveyard Finder", 1, 1)
        .with_mana_cost(ManaCost::generic(1))
        .id();
    let saga_id = scenario
        .add_creature(P0, "The Hunger Tide Rises", 0, 0)
        .as_enchantment()
        .with_subtypes(vec!["Saga"])
        .from_oracle_text(HUNGER_TIDE_ORACLE)
        .id();
    let plains = ["Plains"; 10];
    scenario.with_library_top(P0, &plains);
    scenario.with_library_top(P1, &plains);
    let mut runner = scenario.build();
    let state = runner.state_mut();
    state
        .objects
        .get_mut(&saga_id)
        .unwrap()
        .counters
        .insert(CounterType::Lore, 3);
    let _library_also = add_library_creature(state, 30, P0, "Library Also", ManaCost::generic(1));

    trigger_chapter_iv_via_saga_lore_counter(&mut runner, saga_id);
    resolve_chapter_iv_choices(&mut runner, &[fodder], graveyard_finder);

    let state = runner.state();
    assert_eq!(
        state.objects[&graveyard_finder].zone,
        Zone::Battlefield,
        "graveyard pick must enter the battlefield"
    );
    assert_eq!(
        battlefield_creature_count(state),
        1,
        "only the searched creature may remain on the battlefield"
    );
    assert!(
        state.players[0].graveyard.contains(&fodder),
        "sacrificed fodder stays in the graveyard"
    );
    assert!(
        !state.players[0].graveyard.contains(&graveyard_finder),
        "found graveyard card must leave the graveyard"
    );
}
