//! CR 733.1: the cast-undo record serializes only the members that can differ.
//!
//! Every admitted action of a cast that has already changed a cost (CR 601.2h)
//! records the inverse that canceling the cast replays. The record compares the
//! action's boundary with its result, serializing only the objects either
//! state does not share with the other, plus those a folded earlier inverse
//! names, and the deck pools only when they were written.
//!
//! Each equivalence test holds `CastUndoReferenceCheck`, which also runs the
//! complete comparison (`cast_undo_value` over the whole serialized state) at
//! every recorded step and panics unless both store the identical
//! `SpellCastReversal`. The casts cover a discard cost, a life cost, a library
//! reveal during mana payment, an alternative cost (CR 118.9) and convoke
//! (CR 702.51a) on a real four-player board, and each test asserts the CR 733.1
//! cancel outcome too. The counter test pins how much the record serializes.

use std::collections::BTreeSet;

use engine::game::perf_counters::{self, CastUndoRecordCounters};
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::{
    CastPaymentMode, CastingVariant, GameState, PersistedGameState, StackEntry, StackEntryKind,
    WaitingFor,
};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

/// Bitter Triumph, verbatim Oracle text (Scryfall).
const BITTER_TRIUMPH: &str = "As an additional cost to cast this spell, discard a card or pay 3 life.\nDestroy target creature or planeswalker.";

/// Force of Will, verbatim Oracle text (Scryfall).
const FORCE_OF_WILL: &str = "You may pay 1 life and exile a blue card from your hand rather than pay this spell's mana cost.\nCounter target spell.";

/// A land whose mana ability reveals the top card of its controller's library.
const LIBRARY_WITNESS: &str = "{T}: Add {B}. Reveal the top card of your library.";

/// Serialized key of the reversal descriptor (`GameState::spell_cast_reversal`).
const REVERSAL: &str = "spell_cast_reversal";

/// Chord of Calling in the witherbloom capture, and three untapped green
/// creatures its controller controls. Announced at X = 0, Chord's total cost
/// is {G}{G}{G}, with no generic mana, so each convoker pays one {G} of its
/// own color (CR 702.51a).
const CHORD: ObjectId = ObjectId(31);
const CONVOKERS: [(ObjectId, ManaColor); 3] = [
    (ObjectId(422), ManaColor::Green),
    (ObjectId(397), ManaColor::Green),
    (ObjectId(417), ManaColor::Green),
];

/// Runs `action` and returns the cast-undo record counts it accrued.
fn act_counted(runner: &mut GameRunner, action: GameAction) -> CastUndoRecordCounters {
    let start = perf_counters::cast_undo_record_snapshot();
    let label = format!("{action:?}");
    runner
        .act(action)
        .unwrap_or_else(|error| panic!("{label} must be accepted: {error}"));
    perf_counters::cast_undo_record_snapshot().since(start)
}

/// A recorded step built two changed-member values, and the reference check
/// found the stored reversal identical to the complete-state one.
fn assert_recorded(delta: CastUndoRecordCounters, step: &str) {
    assert_eq!(
        delta.values, 2,
        "{step} records the cast's inverse: {delta:?}"
    );
    assert_eq!(
        delta.reference_checks, 1,
        "{step} is compared with the complete record: {delta:?}"
    );
}

/// A step before any cost mutation, or past a library barrier, builds nothing.
fn assert_not_recorded(delta: CastUndoRecordCounters, step: &str) {
    assert_eq!(
        delta.values, 0,
        "{step} builds no cast-undo value: {delta:?}"
    );
    assert_eq!(
        delta.reference_checks, 0,
        "{step} records nothing: {delta:?}"
    );
}

/// The serialized reversal descriptor, or `None` when the state has none.
fn reversal(state: &GameState) -> Option<serde_json::Value> {
    serde_json::to_value(state)
        .expect("the game state serializes")
        .get(REVERSAL)
        .cloned()
}

/// One boolean field of the reversal descriptor.
fn reversal_flag(state: &GameState, name: &str) -> bool {
    reversal(state).expect("the cast owns a reversal")[name]
        .as_bool()
        .unwrap_or_else(|| panic!("the reversal's {name} is a boolean"))
}

/// The path of every inverse edit the reversal stores.
fn undo_paths(state: &GameState) -> Vec<Vec<String>> {
    let Some(descriptor) = reversal(state) else {
        return Vec::new();
    };
    descriptor["undo"]
        .as_array()
        .expect("the reversal stores its inverse")
        .iter()
        .map(|edit| {
            let (_, fields) = edit
                .as_object()
                .and_then(|edit| edit.iter().next())
                .expect("an inverse edit is a one-variant object");
            fields["path"]
                .as_array()
                .expect("an inverse edit has a path")
                .iter()
                .map(|part| part.as_str().expect("path parts are strings").to_owned())
                .collect()
        })
        .collect()
}

/// The objects the stored inverse addresses.
fn undo_object_ids(state: &GameState) -> BTreeSet<ObjectId> {
    undo_paths(state)
        .iter()
        .filter_map(|path| match path.as_slice() {
            [member, id, ..] if member == "objects" => {
                Some(ObjectId(id.parse().expect("object keys are ids")))
            }
            _ => None,
        })
        .collect()
}

/// The hostile step: `id` was written by an earlier step of this cast and is
/// the same allocation before and after this one, so only the folded inverse
/// names it. Its edit must survive into the new inverse.
fn assert_folded_shared_object_survives(pre: &GameState, post: &GameState, id: ObjectId) {
    assert!(
        pre.objects.shares_object_with(&post.objects, id),
        "{id:?} is shared by the step's boundary and result"
    );
    assert!(
        undo_object_ids(pre).contains(&id),
        "the folded inverse names {id:?}"
    );
    assert!(
        undo_object_ids(post).contains(&id),
        "the recorded inverse still restores {id:?}"
    );
}

fn tap_land(runner: &mut GameRunner, land: ObjectId) -> CastUndoRecordCounters {
    let selection =
        engine::game::mana_sources::activatable_mana_source_selections(runner.state(), P0)
            .into_iter()
            .find(|selection| selection.source.object_id == land)
            .expect("the land offers its mana");
    act_counted(runner, GameAction::TapLandForMana { selection })
}

struct TriumphBoard {
    runner: GameRunner,
    spell: ObjectId,
    spare_a: ObjectId,
    spare_b: ObjectId,
    swamp: ObjectId,
    second_land: ObjectId,
    foe: ObjectId,
}

/// Bitter Triumph and two spare cards in hand, a Swamp plus either a second
/// Swamp or the library witness, six creatures a side, eight library cards
/// each.
fn triumph_board(witness: Option<&str>) -> TriumphBoard {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Bitter Triumph", true, BITTER_TRIUMPH)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Black],
            generic: 1,
        })
        .id();
    let spare_a = scenario.add_card_to_hand(P0, "Spare Card A");
    let spare_b = scenario.add_card_to_hand(P0, "Spare Card B");
    let swamp = scenario.add_basic_land(P0, ManaColor::Black);
    let second_land = match witness {
        Some(text) => scenario
            .add_land_from_oracle(P0, "Library witness", text)
            .id(),
        None => scenario.add_basic_land(P0, ManaColor::Black),
    };
    let mut foes = Vec::new();
    for index in 0..6 {
        scenario.add_creature(P0, &format!("Bystander {index}"), 2, 2);
        foes.push(
            scenario
                .add_creature(P1, &format!("Foe {index}"), 2, 2)
                .id(),
        );
    }
    scenario.with_library_top(P0, &["L1", "L2", "L3", "L4", "L5", "L6", "L7", "L8"]);
    scenario.with_library_top(P1, &["M1", "M2", "M3", "M4", "M5", "M6", "M7", "M8"]);
    TriumphBoard {
        runner: scenario.build(),
        spell,
        spare_a,
        spare_b,
        swamp,
        second_land,
        foe: foes[0],
    }
}

/// Casts Bitter Triumph at a foe and answers its additional-cost choice,
/// returning that answer's record counts. Announcing and targeting change no
/// cost, so neither records.
fn announce_triumph(board: &mut TriumphBoard, pay_discard: bool) -> CastUndoRecordCounters {
    let card_id = board.runner.state().objects[&board.spell].card_id;
    let delta = act_counted(
        &mut board.runner,
        GameAction::CastSpell {
            object_id: board.spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Manual,
        },
    );
    assert!(matches!(
        board.runner.state().waiting_for,
        WaitingFor::TargetSelection { .. }
    ));
    assert_not_recorded(delta, "CastSpell");
    assert!(!reversal_flag(board.runner.state(), "cost_mutations"));

    let delta = act_counted(
        &mut board.runner,
        GameAction::ChooseTarget {
            target: Some(TargetRef::Object(board.foe)),
        },
    );
    assert!(matches!(
        board.runner.state().waiting_for,
        WaitingFor::OptionalCostChoice { .. }
    ));
    assert_not_recorded(delta, "ChooseTarget");

    act_counted(
        &mut board.runner,
        GameAction::DecideOptionalCost { pay: pay_discard },
    )
}

/// Announces Bitter Triumph electing the discard. Electing it pays nothing
/// yet, so it records nothing either.
fn announce_triumph_discard(board: &mut TriumphBoard) {
    let delta = announce_triumph(board, true);
    let WaitingFor::PayCost { choices, .. } = &board.runner.state().waiting_for else {
        panic!(
            "the discard branch asks for the card, got {:?}",
            board.runner.state().waiting_for
        );
    };
    assert_eq!(
        choices.iter().copied().collect::<BTreeSet<_>>(),
        BTreeSet::from([board.spare_a, board.spare_b])
    );
    assert_not_recorded(delta, "electing the discard");
    assert!(!reversal_flag(board.runner.state(), "cost_mutations"));
}

/// The capture loaded through the production restore chokepoint.
fn load_witherbloom() -> GameState {
    use std::io::Read;
    let mut json = String::new();
    flate2::read::GzDecoder::new(
        &include_bytes!("../fixtures/witherbloom_altar_sprout_swarm_4p.json.gz")[..],
    )
    .read_to_string(&mut json)
    .expect("fixture .json.gz must inflate to UTF-8 JSON");
    // Committed pre-onset capture; onset-independent test (see record_captured_onsets).
    let json = engine::types::game_state::record_captured_onsets_json(&json);
    let envelope: serde_json::Value =
        serde_json::from_str(&json).expect("capture envelope parses as JSON");
    serde_json::from_value::<PersistedGameState>(envelope["gameState"].clone())
        .expect("the capture's gameState deserializes through the production decoder")
        .into_game_state()
        .expect("the capture satisfies the checked restore contract")
}

/// The witherbloom board with Chord of Calling announced for X = 0 and its
/// convokers checked.
fn chord_announced() -> GameRunner {
    let mut runner = GameRunner::from_state(load_witherbloom());
    let state = runner.state();
    let chord = &state.objects[&CHORD];
    assert_eq!(chord.name, "Chord of Calling");
    assert_eq!(chord.zone, Zone::Hand);
    assert_eq!(
        chord.mana_cost,
        ManaCost::Cost {
            shards: vec![
                ManaCostShard::X,
                ManaCostShard::Green,
                ManaCostShard::Green,
                ManaCostShard::Green,
            ],
            generic: 0,
        },
        "at X = 0 the total cost is three green pips"
    );
    for (id, color) in CONVOKERS {
        let creature = &state.objects[&id];
        assert_eq!(creature.zone, Zone::Battlefield, "{id:?}");
        assert!(!creature.tapped, "{id:?}");
        assert_eq!(creature.controller, chord.owner, "{id:?}");
        assert!(
            creature.card_types.core_types.contains(&CoreType::Creature),
            "{id:?}"
        );
        assert!(
            creature.color.contains(&color),
            "{id:?} must be {color:?} to convoke the {color:?} mana it pays"
        );
    }
    assert_eq!(state.deck_pools.len(), 4);
    let card_id = chord.card_id;
    let delta = act_counted(
        &mut runner,
        GameAction::CastSpell {
            object_id: CHORD,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Manual,
        },
    );
    assert_not_recorded(delta, "CastSpell");
    let delta = act_counted(&mut runner, GameAction::ChooseX { value: 0 });
    assert_not_recorded(delta, "ChooseX");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ManaPayment { .. }
    ));
    runner
}

/// CR 733.1 + CR 601.2h: a discard cost, then mana taps, pins and an untap.
/// Every recorded step matches the complete record, including the steps where
/// the discarded card and the spell are shared yet named by the folded
/// inverse. Canceling returns the card and the spell to hand.
#[test]
fn discard_cost_records_match_the_complete_record_through_payment_and_cancel() {
    let _reference = engine::game::CastUndoReferenceCheck::enable();
    let mut board = triumph_board(None);
    announce_triumph_discard(&mut board);
    let TriumphBoard {
        mut runner,
        spell,
        spare_a,
        spare_b,
        swamp,
        second_land,
        ..
    } = board;

    let delta = act_counted(
        &mut runner,
        GameAction::SelectCards {
            cards: vec![spare_a],
        },
    );
    assert_recorded(delta, "the discard");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ManaPayment { .. }
    ));
    assert_eq!(runner.state().objects[&spare_a].zone, Zone::Graveyard);
    assert!(reversal_flag(runner.state(), "cost_mutations"));

    assert_recorded(tap_land(&mut runner, swamp), "TapLandForMana");
    assert!(runner.state().objects[&swamp].tapped);
    let pip_id = runner.state().players[0]
        .mana_pool
        .units()
        .next()
        .expect("the Swamp's mana floats")
        .pip_id;

    for (step, action) in [
        ("SpendPoolMana", GameAction::SpendPoolMana { pip_id }),
        ("UnspendPoolMana", GameAction::UnspendPoolMana { pip_id }),
        (
            "UntapLandForMana",
            GameAction::UntapLandForMana { object_id: swamp },
        ),
    ] {
        let pre = runner.state().clone();
        assert_recorded(act_counted(&mut runner, action), step);
        assert_folded_shared_object_survives(&pre, runner.state(), spare_a);
        assert_folded_shared_object_survives(&pre, runner.state(), spell);
    }

    runner
        .act(GameAction::CancelCast)
        .expect("the cast can be canceled");
    let state = runner.state();
    assert_eq!(state.objects[&spare_a].zone, Zone::Hand);
    assert_eq!(state.objects[&spare_b].zone, Zone::Hand);
    assert_eq!(state.objects[&spell].zone, Zone::Hand);
    assert!(!state.objects[&swamp].tapped);
    assert!(!state.objects[&second_land].tapped);
    assert_eq!(state.players[0].mana_pool.total(), 0);
    assert!(state.pending_cast.is_none());
    assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
}

/// CR 733.1 + CR 601.2h: paying 3 life writes no object, so its record
/// serializes none; canceling after a mana tap restores the life.
#[test]
fn life_cost_records_match_the_complete_record_and_cancel_restores_life() {
    let _reference = engine::game::CastUndoReferenceCheck::enable();
    let mut board = triumph_board(None);
    let life_step = announce_triumph(&mut board, false);
    let TriumphBoard {
        mut runner,
        spell,
        swamp,
        ..
    } = board;
    assert_recorded(life_step, "the life payment");
    assert_eq!(life_step.objects, 0, "paying life writes no object");
    assert_eq!(runner.life(P0), 17);
    assert!(undo_paths(runner.state()).contains(&vec![
        "players".to_owned(),
        "0".to_owned(),
        "life".to_owned()
    ]));

    assert_recorded(tap_land(&mut runner, swamp), "TapLandForMana");
    assert!(runner.state().objects[&swamp].tapped);

    runner
        .act(GameAction::CancelCast)
        .expect("the cast can be canceled");
    assert_eq!(runner.life(P0), 20);
    assert!(!runner.state().objects[&swamp].tapped);
    assert_eq!(runner.state().objects[&spell].zone, Zone::Hand);
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
}

/// CR 733.1: once a mana ability reveals a library card, the cast can no
/// longer be reversed. The steps before match the complete record, the
/// barrier step builds no value, and the cancel is refused.
#[test]
fn a_library_reveal_bars_undo_after_matching_records() {
    let _reference = engine::game::CastUndoReferenceCheck::enable();
    let mut board = triumph_board(Some(LIBRARY_WITNESS));
    announce_triumph_discard(&mut board);
    let TriumphBoard {
        mut runner,
        spare_a,
        swamp,
        second_land: witness,
        ..
    } = board;
    assert_recorded(
        act_counted(
            &mut runner,
            GameAction::SelectCards {
                cards: vec![spare_a],
            },
        ),
        "the discard",
    );
    assert_recorded(tap_land(&mut runner, swamp), "TapLandForMana");
    assert!(!reversal_flag(runner.state(), "library_undo_forbidden"));
    let top = runner.state().players[0].library[0];

    assert_not_recorded(tap_land(&mut runner, witness), "the revealing mana ability");
    assert!(runner.state().public_revealed_cards.contains(&top));
    assert!(reversal_flag(runner.state(), "library_undo_forbidden"));

    let before_cancel = serde_json::to_value(runner.state()).expect("the state serializes");
    let error = runner
        .act(GameAction::CancelCast)
        .expect_err("a library reveal cannot be undone");
    assert!(
        error
            .to_string()
            .contains("Cannot undo a library disclosure"),
        "{error}"
    );
    assert_eq!(
        serde_json::to_value(runner.state()).expect("the state serializes"),
        before_cancel
    );
}

/// CR 733.1 + CR 118.9: electing Force of Will's alternative cost pays its
/// life at once. That step records like the complete record, and canceling
/// refunds the life.
#[test]
fn an_alternative_cost_election_records_like_the_complete_record_and_cancel_refunds_it() {
    let _reference = engine::game::CastUndoReferenceCheck::enable();
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let force = scenario
        .add_spell_to_hand_from_oracle(P0, "Force of Will", true, FORCE_OF_WILL)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Blue, ManaCostShard::Blue],
            generic: 3,
        })
        .id();
    let pitch = scenario
        .add_creature_to_hand(P0, "Blue Pitch Card", 1, 1)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Blue],
            generic: 0,
        })
        .id();
    let mut runner = scenario.build();
    let target = engine::game::zones::create_object(
        runner.state_mut(),
        CardId(900_196),
        P1,
        "Target Spell".to_string(),
        Zone::Stack,
    );
    runner
        .state_mut()
        .objects
        .get_mut(&target)
        .expect("the target spell exists")
        .card_types
        .core_types = vec![CoreType::Instant];
    runner.state_mut().stack.push_back(StackEntry {
        id: target,
        source_id: target,
        controller: P1,
        kind: StackEntryKind::Spell {
            card_id: CardId(900_196),
            ability: None,
            casting_variant: CastingVariant::Normal,
            actual_mana_spent: 0,
        },
    });

    let card_id = runner.state().objects[&force].card_id;
    let delta = act_counted(
        &mut runner,
        GameAction::CastSpell {
            object_id: force,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Manual,
        },
    );
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::OptionalCostChoice { .. }
    ));
    assert_not_recorded(delta, "CastSpell");

    let delta = act_counted(&mut runner, GameAction::DecideOptionalCost { pay: true });
    assert_recorded(delta, "the alternative-cost election");
    assert_eq!(runner.life(P0), 19);
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::PayCost { .. }
    ));

    runner
        .act(GameAction::CancelCast)
        .expect("the cast can be canceled");
    assert_eq!(runner.life(P0), 20);
    assert_eq!(runner.state().objects[&force].zone, Zone::Hand);
    assert_eq!(runner.state().objects[&pitch].zone, Zone::Hand);
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
}

/// CR 733.1 + CR 702.51a: convoking Chord's three green pips with three green
/// creatures on a real four-player board records like the complete record at
/// every tap, with the shared deck pools left out, and canceling untaps them.
#[test]
fn convoke_on_a_real_four_player_board_records_like_the_complete_record() {
    let _reference = engine::game::CastUndoReferenceCheck::enable();
    let mut runner = chord_announced();
    for (object_id, color) in CONVOKERS {
        let delta = act_counted(
            &mut runner,
            GameAction::TapForConvoke {
                object_id,
                mana_type: ManaType::from(color),
            },
        );
        assert_recorded(delta, &format!("TapForConvoke {object_id:?}"));
        assert_eq!(delta.deck_pool_entries, 0, "the deck pools stay shared");
        assert!(runner.state().objects[&object_id].tapped);
    }
    assert_eq!(runner.state().deck_pools.len(), 4);

    runner
        .act(GameAction::CancelCast)
        .expect("the cast can be canceled");
    let state = runner.state();
    for (id, _) in CONVOKERS {
        assert!(!state.objects[&id].tapped, "{id:?} untaps");
    }
    assert_eq!(state.objects[&CHORD].zone, Zone::Hand);
    assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
}

/// The record's cost: a convoke step serializes exactly the objects the step
/// unshared plus those the folded inverse names, fewer than one complete
/// value holds, and none of the four shared deck pools.
#[test]
fn a_convoke_step_serializes_only_unshared_objects_and_no_shared_deck_pool() {
    let mut runner = chord_announced();
    let (first, first_color) = CONVOKERS[0];
    let delta = act_counted(
        &mut runner,
        GameAction::TapForConvoke {
            object_id: first,
            mana_type: ManaType::from(first_color),
        },
    );
    assert_eq!(delta.values, 2, "the first convoke records");

    let pre = runner.state().clone();
    let folded = undo_object_ids(&pre);
    assert!(!undo_paths(&pre).is_empty(), "an earlier inverse is folded");
    let (object_id, color) = CONVOKERS[1];
    let delta = act_counted(
        &mut runner,
        GameAction::TapForConvoke {
            object_id,
            mana_type: ManaType::from(color),
        },
    );
    let post = runner.state();

    let ids: BTreeSet<ObjectId> = pre
        .objects
        .keys()
        .chain(post.objects.keys())
        .filter(|id| !pre.objects.shares_object_with(&post.objects, **id))
        .copied()
        .chain(folded)
        .collect();
    let expected = ids.iter().filter(|id| pre.objects.contains_key(id)).count()
        + ids
            .iter()
            .filter(|id| post.objects.contains_key(id))
            .count();
    assert_eq!(delta.values, 2, "the step records");
    assert_eq!(post.deck_pools.len(), 4);
    assert_eq!(delta.objects, expected as u64);
    assert!(
        delta.objects < post.objects.len() as u64,
        "{} objects serialized, a complete value alone holds {}",
        delta.objects,
        post.objects.len()
    );
    assert_eq!(delta.deck_pool_entries, 0);
}
