//! Runtime tests for Dig rest "in any order" (`DigRestOrder::PlayerChosen`).
//!
//! Discriminating permutation is non-identity: after keep A from `[A, B, C]`,
//! `SelectCards { cards: [C, B] }` must become the library suffix. Identity
//! default cannot pass these assertions.

use engine::ai_support::legal_actions;
use engine::game::engine::EngineError;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::visibility::filter_state_for_viewer;
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const DUSKWATCH_ORACLE: &str = "\
{2}{G}: Look at the top three cards of your library. You may reveal a creature card \
from among them and put it into your hand. Put the rest on the bottom of your library \
in any order.\n\
At the beginning of each upkeep, if no spells were cast last turn, transform this creature.";

const PRESERVE_ORACLE: &str = "\
Look at the top three cards of your library. You may reveal a creature card \
from among them and put it into your hand. Put the rest on the bottom of your library.";

const LEAD_THE_STAMPEDE: &str = "\
Look at the top five cards of your library. You may reveal any number of creature \
cards from among them and put them into your hand. Put the rest on the bottom of \
your library in any order.";

fn colorless() -> ManaUnit {
    ManaUnit::new(ManaType::Colorless, ObjectId(1), false, Vec::new())
}

fn green() -> ManaUnit {
    ManaUnit::new(ManaType::Green, ObjectId(1), false, Vec::new())
}

fn mark_creature(runner: &mut GameRunner, id: ObjectId) {
    let obj = runner.state_mut().objects.get_mut(&id).unwrap();
    obj.card_types.core_types.push(CoreType::Creature);
    obj.base_card_types = obj.card_types.clone();
}

fn mark_sorcery(runner: &mut GameRunner, id: ObjectId) {
    let obj = runner.state_mut().objects.get_mut(&id).unwrap();
    obj.card_types.core_types.push(CoreType::Sorcery);
    obj.base_card_types = obj.card_types.clone();
}

fn seed_top_three(scenario: &mut GameScenario) -> (ObjectId, ObjectId, ObjectId) {
    // `add_card_to_library_top` inserts at index 0 — add bottom-of-three first.
    let c = scenario.add_card_to_library_top(P0, "SpellC");
    let b = scenario.add_card_to_library_top(P0, "SpellB");
    let a = scenario.add_card_to_library_top(P0, "CreatureA");
    (a, b, c)
}

fn activate_duskwatch(
    library_cards: usize,
) -> (GameRunner, ObjectId, ObjectId, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let recruiter = scenario
        .add_creature_from_oracle(P0, "Duskwatch Recruiter", 2, 2, DUSKWATCH_ORACLE)
        .id();
    scenario.with_mana_pool(P0, vec![colorless(), colorless(), green()]);
    let (a, b, c) = seed_top_three(&mut scenario);
    if library_cards == 2 {
        // Drop C from the look window by leaving only A,B on top of whatever
        // else exists — the look is the top three, so remove C from library.
        let _ = c;
    }
    let mut runner = scenario.build();
    mark_creature(&mut runner, a);
    mark_sorcery(&mut runner, b);
    mark_sorcery(&mut runner, c);
    if library_cards == 2 {
        runner
            .state_mut()
            .players
            .get_mut(0)
            .unwrap()
            .library
            .retain(|id| *id != c);
        runner.state_mut().objects.get_mut(&c).unwrap().zone = Zone::Exile;
    } else if library_cards == 1 {
        runner
            .state_mut()
            .players
            .get_mut(0)
            .unwrap()
            .library
            .retain(|id| *id == a);
        for id in [b, c] {
            runner.state_mut().objects.get_mut(&id).unwrap().zone = Zone::Exile;
        }
    } else if library_cards == 0 {
        runner
            .state_mut()
            .players
            .get_mut(0)
            .unwrap()
            .library
            .clear();
        for id in [a, b, c] {
            runner.state_mut().objects.get_mut(&id).unwrap().zone = Zone::Exile;
        }
    }
    runner.activate(recruiter, 0).resolve();
    (runner, recruiter, a, b, c)
}

fn assert_rest_still_in_library(state: &engine::types::game_state::GameState, rest: &[ObjectId]) {
    let lib = &state.players[0].library;
    for &id in rest {
        assert_eq!(
            state.objects[&id].zone,
            Zone::Library,
            "{id:?} must still be in Zone::Library during DigBottomOrder"
        );
        assert!(
            lib.iter().any(|&member| member == id),
            "{id:?} must still be in P0's library vec during DigBottomOrder; library = {lib:?}"
        );
    }
}

fn expect_dig_choice(runner: &GameRunner) -> (Vec<ObjectId>, Vec<ObjectId>) {
    match runner.state().waiting_for.clone() {
        WaitingFor::DigChoice {
            cards,
            selectable_cards,
            ..
        } => (cards, selectable_cards),
        other => panic!("expected DigChoice, got {other:?}"),
    }
}

fn expect_dig_bottom(runner: &GameRunner) -> Vec<ObjectId> {
    match runner.state().waiting_for.clone() {
        WaitingFor::DigBottomOrder { cards, .. } => cards,
        other => panic!("expected DigBottomOrder, got {other:?}"),
    }
}

#[test]
fn duskwatch_keep_then_non_identity_bottom_order() {
    let (mut runner, _recruiter, a, b, c) = activate_duskwatch(3);
    let (looked, selectable) = expect_dig_choice(&runner);
    assert_eq!(looked.len(), 3, "look window is the top three");
    assert!(
        selectable.contains(&a) && !selectable.contains(&b) && !selectable.contains(&c),
        "only the creature is selectable; selectable = {selectable:?}"
    );

    runner
        .act(GameAction::SelectCards { cards: vec![a] })
        .expect("keep the creature");

    let rest = expect_dig_bottom(&runner);
    assert_eq!(
        rest.iter()
            .copied()
            .collect::<std::collections::HashSet<_>>(),
        [b, c].into_iter().collect(),
        "DigBottomOrder cards are the two unkept ids"
    );
    assert_rest_still_in_library(runner.state(), &rest);
    assert_eq!(runner.state().objects[&a].zone, Zone::Hand);

    let looker = filter_state_for_viewer(runner.state(), P0);
    assert_ne!(
        looker.objects[&b].name, "Hidden Card",
        "looker must still see rest identities during DigBottomOrder"
    );
    assert_ne!(looker.objects[&c].name, "Hidden Card");
    // The activated sentence is "Look at … You may reveal a creature card from
    // among them". The current parser promotes that whole Dig to `reveal: true`
    // (same as Lead the Stampede), so rest identities ride `revealed_cards`
    // through DigBottomOrder (CR 701.20a persist). A future split that keeps
    // only the chosen creature public is not this rest-order unit.
    let opponent = filter_state_for_viewer(runner.state(), P1);
    assert_ne!(
        opponent.objects[&b].name, "Hidden Card",
        "reveal-from-among Digs publish rest identities via revealed_cards during DigBottomOrder"
    );
    assert_ne!(opponent.objects[&c].name, "Hidden Card");

    let actions = legal_actions(runner.state());
    assert!(
        actions.iter().any(|action| {
            matches!(action, GameAction::SelectCards { cards } if cards == &rest)
        }),
        "legal_actions must contain the identity permutation; got {actions:?}"
    );

    runner
        .act(GameAction::SelectCards { cards: vec![c, b] })
        .expect("non-identity rest order");

    let library: Vec<ObjectId> = runner.state().players[0].library.iter().copied().collect();
    assert_eq!(
        &library[library.len() - 2..],
        &[c, b],
        "submitted order is the library suffix; library = {library:?}"
    );
    assert_eq!(runner.state().objects[&a].zone, Zone::Hand);
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "after submit, waiting is Priority, not a stuck DigBottomOrder: {:?}",
        runner.state().waiting_for
    );
}

#[test]
fn duskwatch_empty_keep_orders_all_three() {
    let (mut runner, _, a, b, c) = activate_duskwatch(3);
    expect_dig_choice(&runner);
    runner
        .act(GameAction::SelectCards { cards: vec![] })
        .expect("up_to empty keep is legal");

    let rest = expect_dig_bottom(&runner);
    assert_eq!(
        rest.iter()
            .copied()
            .collect::<std::collections::HashSet<_>>(),
        [a, b, c].into_iter().collect()
    );
    assert_rest_still_in_library(runner.state(), &rest);

    runner
        .act(GameAction::SelectCards {
            cards: vec![c, b, a],
        })
        .expect("order the full looked-at pile");

    let library: Vec<ObjectId> = runner.state().players[0].library.iter().copied().collect();
    assert_eq!(&library[library.len() - 3..], &[c, b, a]);
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
}

#[test]
fn duskwatch_one_rest_card_skips_bottom_order() {
    let (mut runner, _, a, b, _) = activate_duskwatch(2);
    expect_dig_choice(&runner);
    runner
        .act(GameAction::SelectCards { cards: vec![a] })
        .expect("keep the creature");
    assert!(
        !matches!(
            runner.state().waiting_for,
            WaitingFor::DigBottomOrder { .. }
        ),
        "len < 2 rest must not prompt; waiting = {:?}",
        runner.state().waiting_for
    );
    assert_eq!(runner.state().objects[&a].zone, Zone::Hand);
    assert_eq!(runner.state().objects[&b].zone, Zone::Library);
}

#[test]
fn duskwatch_one_card_library_offers_dig_choice() {
    let (mut runner, _, a, _, _) = activate_duskwatch(1);
    let (looked, selectable) = expect_dig_choice(&runner);
    assert_eq!(looked, vec![a]);
    assert_eq!(selectable, vec![a]);
    runner
        .act(GameAction::SelectCards { cards: vec![a] })
        .expect("keep the only creature");
    assert!(
        !matches!(
            runner.state().waiting_for,
            WaitingFor::DigBottomOrder { .. }
        ),
        "no rest-order prompt with one card"
    );
    assert_eq!(runner.state().objects[&a].zone, Zone::Hand);
}

#[test]
fn duskwatch_empty_library_resolves_without_prompt() {
    let (runner, _, _, _, _) = activate_duskwatch(0);
    assert!(
        !matches!(
            runner.state().waiting_for,
            WaitingFor::DigChoice { .. } | WaitingFor::DigBottomOrder { .. }
        ),
        "empty library does as much as possible without a prompt; waiting = {:?}",
        runner.state().waiting_for
    );
}

#[test]
fn preserve_oracle_does_not_raise_dig_bottom_order() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let (a, b, c) = seed_top_three(&mut scenario);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Preserve Dig", true, PRESERVE_ORACLE)
        .with_mana_cost(engine::types::mana::ManaCost::zero())
        .id();
    let mut runner = scenario.build();
    mark_creature(&mut runner, a);
    mark_sorcery(&mut runner, b);
    mark_sorcery(&mut runner, c);

    let outcome = runner.cast(spell).resolve();
    match outcome.final_waiting_for() {
        WaitingFor::DigChoice { .. } => {}
        other => panic!("expected DigChoice, got {other:?}"),
    }
    runner
        .act(GameAction::SelectCards { cards: vec![a] })
        .expect("keep the creature");
    assert!(
        !matches!(
            runner.state().waiting_for,
            WaitingFor::DigBottomOrder { .. }
        ),
        "Preserve must not raise DigBottomOrder; waiting = {:?}",
        runner.state().waiting_for
    );
    assert_eq!(runner.state().objects[&a].zone, Zone::Hand);
}

#[test]
fn dig_bottom_order_rejects_non_permutation() {
    let (mut runner, _, a, b, c) = activate_duskwatch(3);
    runner
        .act(GameAction::SelectCards { cards: vec![a] })
        .unwrap();
    let rest = expect_dig_bottom(&runner);
    assert_eq!(
        rest.iter()
            .copied()
            .collect::<std::collections::HashSet<_>>(),
        [b, c].into_iter().collect()
    );

    assert!(
        matches!(
            runner.act(GameAction::SelectCards { cards: vec![b, b] }),
            Err(EngineError::InvalidAction(_))
        ),
        "duplicate ids are illegal"
    );
    assert!(
        matches!(
            runner.act(GameAction::SelectCards { cards: vec![b] }),
            Err(EngineError::InvalidAction(_))
        ),
        "missing id is illegal"
    );
    assert!(
        matches!(
            runner.act(GameAction::SelectCards {
                cards: vec![b, c, a],
            }),
            Err(EngineError::InvalidAction(_))
        ),
        "foreign/extra id is illegal"
    );
    runner
        .act(GameAction::SelectCards {
            cards: rest.clone(),
        })
        .expect("identity permutation of the offered cards succeeds");
}

#[test]
fn duskwatch_keep_noncreature_is_rejected() {
    let (mut runner, _, a, b, _) = activate_duskwatch(3);
    expect_dig_choice(&runner);
    assert!(
        matches!(
            runner.act(GameAction::SelectCards { cards: vec![b] }),
            Err(EngineError::InvalidAction(_))
        ),
        "keeping a noncreature must fail"
    );
    assert_eq!(
        runner.state().objects[&a].zone,
        Zone::Library,
        "reach-guard: the legal creature keep is still available"
    );
}

#[test]
fn lead_the_stampede_reveal_dig_opponent_sees_rest_identities() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let e = scenario.add_card_to_library_top(P0, "SpellE");
    let d = scenario.add_card_to_library_top(P0, "SpellD");
    let c = scenario.add_card_to_library_top(P0, "SpellC");
    let b = scenario.add_card_to_library_top(P0, "SpellB");
    let a = scenario.add_card_to_library_top(P0, "CreatureA");
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Lead the Stampede", false, LEAD_THE_STAMPEDE)
        .with_mana_cost(engine::types::mana::ManaCost::zero())
        .id();
    let mut runner = scenario.build();
    mark_creature(&mut runner, a);
    for id in [b, c, d, e] {
        mark_sorcery(&mut runner, id);
    }

    runner.cast(spell).resolve();
    let (looked, _) = expect_dig_choice(&runner);
    assert_eq!(looked.len(), 5);
    assert!(
        !runner.state().revealed_cards.is_empty(),
        "reveal-dig reach-guard: looked-at cards are public during DigChoice"
    );

    runner
        .act(GameAction::SelectCards { cards: vec![a] })
        .expect("keep the creature");
    let rest = expect_dig_bottom(&runner);
    assert_rest_still_in_library(runner.state(), &rest);

    let opponent = filter_state_for_viewer(runner.state(), P1);
    for &id in &rest {
        assert_ne!(
            opponent.objects[&id].name, "Hidden Card",
            "reveal-dig opponent must see rest identities during DigBottomOrder"
        );
    }

    runner
        .act(GameAction::SelectCards {
            cards: vec![e, d, c, b],
        })
        .expect("order the four rest cards");
    for &id in &[b, c, d, e] {
        assert!(
            !runner.state().revealed_cards.contains(&id),
            "CR 701.20d: rest ids leave revealed_cards after placement"
        );
    }
}
