//! Break Out — the revealed creature's mana value gates the battlefield put.
//!
//! Verbatim Oracle (MKM 190):
//!   Look at the top six cards of your library. You may reveal a creature card
//!   from among them. If that card has mana value 2 or less, you may put it onto
//!   the battlefield and it gains haste until end of turn. If you didn't put the
//!   revealed card onto the battlefield this way, put it into your hand. Put the
//!   rest on the bottom of your library in a random order.
//!
//! These tests drive the REAL parser (`add_spell_to_hand_from_oracle`) and the
//! REAL cast/resolution pipeline (scenario runner). The composition under test
//! is `Dig { keep 1 creature, reveal }` + an optional `ChangeZone -> Battlefield`
//! gated by "that card has mana value 2 or less" (`TargetMatchesFilter { Cmc }`
//! on the dig selection) + the `Not(OptionalEffectPerformed)` hand fallback.
//!
//! Revert map (two layers — the card needs both):
//!   * Parser: without the "that card" noun surface, the leading "If ..." guard
//!     fails to parse and the #2277 structural fallback strips it (the body
//!     starts with "you may"), so the put is offered for ANY mana value.
//!   * Engine: without the false-gate tail walk + outcome latch, a skipped
//!     may-put drops the whole tail (the hand fallback never resolves) and a
//!     stale `optional_effect_performed` misreads the skip as performed, so a
//!     MV 3 reveal strands in the library instead of going to hand.
//!   * `put_and_haste_when_revealed_mv_within_gate` — passes either way (the
//!     gate is open); pins the positive branch, not the gate.
//!   * `no_put_when_revealed_mv_exceeds_gate` — REVERT-FAILING on both layers.
//!     A MV 3 reveal must go straight to hand with no battlefield prompt.
//!   * `declined_put_goes_to_hand` — passes either way; pins the decline branch
//!     (prompt offered, hand fallback, no haste).

use engine::game::layers::{flush_layers, mark_layers_full};
use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const BREAK_OUT_ORACLE: &str = "Look at the top six cards of your library. You may reveal a creature card from among them. If that card has mana value 2 or less, you may put it onto the battlefield and it gains haste until end of turn. If you didn't put the revealed card onto the battlefield this way, put it into your hand. Put the rest on the bottom of your library in a random order.";

/// Stage Break Out in hand ({R}{G} paid from the pool) and a six-card library
/// whose only creature has the given mana value. Returns the runner, the spell,
/// and the creature.
fn setup(creature_mv: u32) -> (GameRunner, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(ManaType::Red, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Green, ObjectId(0), false, vec![]),
        ],
    );

    let break_out = scenario
        .add_spell_to_hand_from_oracle(P0, "Break Out", false, BREAK_OUT_ORACLE)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Red, ManaCostShard::Green],
            generic: 0,
        })
        .id();

    for i in 0..5 {
        scenario.add_card_to_library_top(P0, &format!("Library Padding {i}"));
    }
    let creature = scenario
        .add_spell_to_library_top(P0, "Revealed Creature", false)
        .as_creature()
        .with_mana_cost(ManaCost::generic(creature_mv))
        .id();

    (scenario.build(), break_out, creature)
}

/// Cast Break Out and resolve through the dig by revealing `creature`.
fn cast_and_reveal(runner: &mut GameRunner, break_out: ObjectId, creature: ObjectId) {
    let outcome = runner.cast(break_out).resolve();
    match outcome.final_waiting_for() {
        WaitingFor::DigChoice {
            cards,
            selectable_cards,
            ..
        } => {
            assert_eq!(cards.len(), 6, "Break Out looks at six cards");
            assert!(
                selectable_cards.contains(&creature),
                "the revealed creature must be selectable (the MV gate is enforced \
                 at put time, not selection time)"
            );
        }
        other => panic!("expected DigChoice after Break Out resolves, got {other:?}"),
    }
    runner
        .act(GameAction::SelectCards {
            cards: vec![creature],
        })
        .expect("revealing the creature should resolve the dig continuation");
}

/// Drive priority passes until the stack is empty, answering the may-put prompt
/// with `accept` when it appears. Returns whether the prompt appeared.
fn drive_to_empty_stack(runner: &mut GameRunner, accept_put: bool) -> bool {
    let mut put_offered = false;
    for _ in 0..20 {
        match runner.state().waiting_for.clone() {
            WaitingFor::OptionalEffectChoice { .. } => {
                put_offered = true;
                runner
                    .act(GameAction::DecideOptionalEffect { accept: accept_put })
                    .expect("answering the may-put prompt must succeed");
            }
            // "Put the rest on the bottom ... in a random order": the engine
            // offers the rest pile (any selection lands on the bottom).
            WaitingFor::EffectZoneChoice { cards, count, .. } => {
                let take = count.min(cards.len());
                runner
                    .act(GameAction::SelectCards {
                        cards: cards[..take].to_vec(),
                    })
                    .expect("placing the rest on the bottom must succeed");
            }
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() {
                    break;
                }
                if runner.act(GameAction::PassPriority).is_err() {
                    break;
                }
            }
            other => panic!("unexpected waiting state during Break Out: {other:?}"),
        }
    }
    assert!(
        runner.state().stack.is_empty(),
        "Break Out must be fully resolved"
    );
    put_offered
}

fn has_haste(runner: &mut GameRunner, id: ObjectId) -> bool {
    mark_layers_full(runner.state_mut());
    flush_layers(runner.state_mut());
    runner.state().objects[&id].has_keyword(&Keyword::Haste)
}

/// MV 2 ("2 or less" boundary, inclusive): the put is offered and taken — the
/// creature enters with haste and the other five cards go to the bottom.
#[test]
fn put_and_haste_when_revealed_mv_within_gate() {
    let (mut runner, break_out, creature) = setup(2);
    cast_and_reveal(&mut runner, break_out, creature);
    assert!(
        drive_to_empty_stack(&mut runner, true),
        "the may-put prompt must be offered when MV 2 <= 2"
    );

    assert_eq!(
        runner.state().objects[&creature].zone,
        Zone::Battlefield,
        "the revealed MV 2 creature must be put onto the battlefield"
    );
    assert!(
        has_haste(&mut runner, creature),
        "the put creature must gain haste until end of turn"
    );
    assert_eq!(
        runner.state().players[P0.0 as usize].library.len(),
        5,
        "the other five cards must go to the bottom of the library"
    );
    assert_eq!(
        runner.state().objects[&break_out].zone,
        Zone::Graveyard,
        "Break Out must be in the graveyard after resolving"
    );
}

/// MV 3: the gate fails, so no battlefield prompt may appear — the creature
/// goes straight to hand and the rest to the bottom.
#[test]
fn no_put_when_revealed_mv_exceeds_gate() {
    let (mut runner, break_out, creature) = setup(3);
    cast_and_reveal(&mut runner, break_out, creature);
    assert!(
        !drive_to_empty_stack(&mut runner, true),
        "no may-put prompt may be offered when MV 3 exceeds the gate"
    );

    assert_eq!(
        runner.state().objects[&creature].zone,
        Zone::Hand,
        "the revealed MV 3 creature must be put into hand, not onto the battlefield"
    );
    assert!(
        !has_haste(&mut runner, creature),
        "a creature that never entered must not gain haste"
    );
    assert_eq!(
        runner.state().players[P0.0 as usize].library.len(),
        5,
        "the other five cards must go to the bottom of the library"
    );
    assert_eq!(
        runner.state().objects[&break_out].zone,
        Zone::Graveyard,
        "Break Out must be in the graveyard after resolving"
    );
}

/// MV 2 with the may-put declined: the prompt is offered (the gate passes) but
/// declining sends the creature to hand with no haste.
#[test]
fn declined_put_goes_to_hand() {
    let (mut runner, break_out, creature) = setup(2);
    cast_and_reveal(&mut runner, break_out, creature);
    assert!(
        drive_to_empty_stack(&mut runner, false),
        "the may-put prompt must be offered when MV 2 <= 2"
    );

    assert_eq!(
        runner.state().objects[&creature].zone,
        Zone::Hand,
        "a declined put must send the creature to hand"
    );
    assert!(
        !has_haste(&mut runner, creature),
        "a declined put must not grant haste"
    );
    assert_eq!(
        runner.state().players[P0.0 as usize].library.len(),
        5,
        "the other five cards must go to the bottom of the library"
    );
    assert_eq!(
        runner.state().objects[&break_out].zone,
        Zone::Graveyard,
        "Break Out must be in the graveyard after resolving"
    );
}
