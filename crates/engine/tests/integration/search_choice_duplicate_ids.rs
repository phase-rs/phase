//! Ordinary search admission: exact Oracle, paid casting, public choices, and
//! completed delivery. Identical card names deliberately have distinct object IDs.

use engine::game::engine::EngineError;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::SearchSelectionConstraint;
use engine::types::actions::GameAction;
use engine::types::events::{GameEvent, PlayerActionKind};
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const ORACLE: &str = "Discard your hand. Search your library for three cards, put them into your hand, then shuffle.";

fn behold_scenario(
    library_size: usize,
) -> (GameRunner, ObjectId, ObjectId, Vec<ObjectId>, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Behold the Beyond", false, ORACLE)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Black, ManaCostShard::Black],
            generic: 5,
        })
        .id();
    let discarded = scenario
        .add_spell_to_hand(P0, "Discard witness", false)
        .id();
    let cards = (0..library_size)
        .map(|_| {
            scenario
                .add_spell_to_library_top(P0, "Same name", false)
                .id()
        })
        .collect();
    let foreign = scenario
        .add_spell_to_library_top(P1, "Same name", false)
        .id();
    scenario.with_mana_pool(
        P0,
        [
            ManaType::Black,
            ManaType::Black,
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Colorless,
        ]
        .into_iter()
        .map(|color| ManaUnit::new(color, ObjectId(0), false, vec![]))
        .collect(),
    );
    (scenario.build(), spell, discarded, cards, foreign)
}

fn assert_prompt(runner: &GameRunner, spell: ObjectId, discarded: ObjectId, offered: &[ObjectId]) {
    let state = runner.state();
    // CR 608.2c: the discard prefix resolves before the search choice.
    assert_eq!(state.objects[&discarded].zone, Zone::Graveyard);
    assert!(state.pending_scoped_library_search.is_none());
    let WaitingFor::SearchChoice {
        player,
        cards,
        count,
        up_to,
        allows_partial_find,
        constraint,
        reveal,
        ..
    } = &state.waiting_for
    else {
        panic!(
            "Behold must reach ordinary SearchChoice, got {:?}",
            state.waiting_for
        );
    };
    assert_eq!(*player, P0);
    assert_eq!(cards.len(), offered.len());
    assert!(offered.iter().all(|id| cards.contains(id)));
    // CR 701.23d: quantity-only searches find three cards or as many as possible.
    assert_eq!(*count, offered.len().min(3));
    assert!(!up_to && !allows_partial_find && !reveal);
    assert_eq!(*constraint, SearchSelectionConstraint::None);
    assert_eq!(
        state
            .active_ability_continuation()
            .expect("delivery is parked")
            .chain
            .source_id,
        spell
    );
}

fn assert_rejected_unchanged(runner: &mut GameRunner, cards: Vec<ObjectId>) {
    let before = serde_json::to_value(runner.state()).unwrap();
    let result = runner.act(GameAction::SelectCards { cards });
    assert!(
        matches!(result, Err(EngineError::InvalidAction(_))),
        "malformed search selection must be rejected, got {result:?}"
    );
    assert_eq!(
        serde_json::to_value(runner.state()).unwrap(),
        before,
        "rejection must precede publication, replacement, delivery, and continuation mutation"
    );
}

fn assert_complete(
    runner: &GameRunner,
    spell: ObjectId,
    selected: &[ObjectId],
    unchosen: &[ObjectId],
    events: &[GameEvent],
) {
    let state = runner.state();
    for id in selected {
        assert_eq!(state.objects[id].zone, Zone::Hand);
        assert_eq!(
            state.players[0]
                .hand
                .iter()
                .filter(|card| *card == id)
                .count(),
            1
        );
        assert_eq!(
            state
                .zone_changes_this_turn
                .iter()
                .filter(|change| change.object_id == *id
                    && change.from_zone == Some(Zone::Library)
                    && change.to_zone == Zone::Hand)
                .count(),
            1
        );
    }
    for id in unchosen {
        assert_eq!(state.objects[id].zone, Zone::Library);
    }
    // CR 701.24e: the printed shuffle occurs even for zero/one-card libraries.
    assert!(events.iter().any(|event| matches!(
        event,
        GameEvent::PlayerPerformedAction {
            player_id: P0,
            action: PlayerActionKind::ShuffledLibrary,
            ..
        }
    )));
    assert_eq!(state.objects[&spell].zone, Zone::Graveyard);
    assert!(state.stack.is_empty());
    assert!(state.active_ability_continuation().is_none());
    assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
}

#[test]
fn pure_quantity_search_rejects_repeated_ids_before_delivery() {
    let (mut runner, spell, discarded, cards, foreign) = behold_scenario(4);
    runner.cast(spell).resolve();
    assert_prompt(&runner, spell, discarded, &cards);
    // CR 701.23a + CR 608.2d: three selections cannot name one found card twice.
    assert_rejected_unchanged(&mut runner, vec![cards[0], cards[0], cards[1]]);
    assert_rejected_unchanged(&mut runner, vec![cards[0], cards[1], foreign]);
    let result = runner
        .act(GameAction::SelectCards {
            cards: cards[..3].to_vec(),
        })
        .unwrap();
    assert_complete(&runner, spell, &cards[..3], &cards[3..], &result.events);
}

#[test]
fn pure_quantity_search_valid_distinct_completion_control() {
    let (mut runner, spell, discarded, cards, _) = behold_scenario(4);
    runner.cast(spell).resolve();
    assert_prompt(&runner, spell, discarded, &cards);
    let validated = engine::ai_support::validated_candidate_actions(runner.state());
    let candidate = validated.iter().find(|candidate| matches!(&candidate.action,
        GameAction::SelectCards { cards: chosen } if chosen.len() == 3 && chosen.iter().all(|id| cards.contains(id))))
        .expect("AI retains a concrete legal three-card choice");
    let GameAction::SelectCards { cards: selected } = &candidate.action else {
        unreachable!()
    };
    let selected = selected.clone();
    let unchosen: Vec<_> = cards
        .iter()
        .copied()
        .filter(|id| !selected.contains(id))
        .collect();
    let result = runner.act(candidate.action.clone()).unwrap();
    assert_complete(&runner, spell, &selected, &unchosen, &result.events);
}

#[test]
fn pure_quantity_search_preserves_required_count() {
    let (mut runner, spell, discarded, cards, _) = behold_scenario(4);
    runner.cast(spell).resolve();
    assert_prompt(&runner, spell, discarded, &cards);
    // CR 701.23d: neither short/empty nor oversized pure-quantity picks are legal.
    for chosen in [vec![], cards[..2].to_vec(), cards.clone()] {
        assert_rejected_unchanged(&mut runner, chosen);
    }
    let result = runner
        .act(GameAction::SelectCards {
            cards: cards[..3].to_vec(),
        })
        .unwrap();
    assert_complete(&runner, spell, &cards[..3], &cards[3..], &result.events);
}

#[test]
fn short_library_search_requires_each_available_card() {
    let (mut runner, spell, discarded, cards, _) = behold_scenario(2);
    runner.cast(spell).resolve();
    assert_prompt(&runner, spell, discarded, &cards);
    // CR 701.23d: both available physical cards must be found, not one twice.
    assert_rejected_unchanged(&mut runner, vec![cards[0], cards[0]]);
    let result = runner
        .act(GameAction::SelectCards {
            cards: cards.clone(),
        })
        .unwrap();
    assert_complete(&runner, spell, &cards, &[], &result.events);
}

#[test]
fn empty_library_search_still_shuffles_and_finishes() {
    let (mut runner, spell, discarded, _, _) = behold_scenario(0);
    let outcome = runner.cast(spell).resolve();
    assert_eq!(runner.state().objects[&discarded].zone, Zone::Graveyard);
    assert_complete(&runner, spell, &[], &[], outcome.events());
}
