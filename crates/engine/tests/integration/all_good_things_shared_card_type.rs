//! All Good Things... — chapter IV's "If all cards exiled with this Saga share
//! a card type, you gain 10 life" reads the Saga's linked-exile pool built by
//! chapters I–III (CR 607.2a) and gates the life gain on one card type
//! (CR 205.2a) being shared by every card in it.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::triggers::drain_order_triggers_with_identity;
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

/// All Good Things..., verbatim (Scryfall Oracle text).
const ALL_GOOD_THINGS: &str = "(As this Saga enters and after your draw step, add a lore \
counter. Sacrifice after IV.)\n\
I, II, III — Draw two cards, then exile a card from your hand.\n\
IV — If all cards exiled with this Saga share a card type, you gain 10 life.";

/// Pass priority until the stack is empty or a non-priority prompt opens,
/// answering trigger-order prompts.
fn settle(runner: &mut GameRunner) {
    for _ in 0..64 {
        match &runner.state().waiting_for {
            WaitingFor::OrderTriggers { .. } => {
                drain_order_triggers_with_identity(runner.state_mut());
            }
            WaitingFor::Priority { .. } if !runner.state().stack.is_empty() => {
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            _ => return,
        }
    }
    panic!("stack did not settle: {:?}", runner.state().waiting_for);
}

/// Park P0 at the end of an earlier turn so the next advance reaches P0's next
/// precombat main phase, where the Saga gets its lore counter (CR 714.3b).
fn park_for_next_p0_turn(runner: &mut GameRunner) {
    let state = runner.state_mut();
    state.turn_number = 1;
    state.active_player = P0;
    state.phase = Phase::End;
    state.priority_player = P0;
    state.waiting_for = WaitingFor::Priority { player: P0 };
}

/// Advance to `phase`, answering a cleanup-step hand-size discard (CR 514.1)
/// with drawn Forests so the staged cards stay in hand.
fn advance_discarding_forests(runner: &mut GameRunner, phase: Phase) {
    for _ in 0..4 {
        runner.advance_to_phase(phase);
        let WaitingFor::DiscardToHandSize { count, cards, .. } = runner.state().waiting_for.clone()
        else {
            return;
        };
        let forests: Vec<ObjectId> = cards
            .into_iter()
            .filter(|id| runner.state().objects[id].name == "Forest")
            .take(count)
            .collect();
        runner
            .act(GameAction::SelectCards { cards: forests })
            .expect("discard Forests to hand size");
    }
    panic!("did not reach {phase:?}: {:?}", runner.state().waiting_for);
}

/// Advance to P0's next precombat main, so the Saga's next chapter triggers.
fn next_chapter(runner: &mut GameRunner) {
    park_for_next_p0_turn(runner);
    advance_discarding_forests(runner, Phase::PreCombatMain);
    runner.pass_both_players();
    advance_discarding_forests(runner, Phase::PreCombatMain);
    settle(runner);
}

/// Answer a chapter's "exile a card from your hand" with `card`.
fn exile_from_hand(runner: &mut GameRunner, card: ObjectId) {
    match runner.state().waiting_for.clone() {
        WaitingFor::EffectZoneChoice {
            player,
            cards,
            count: 1,
            zone: Zone::Hand,
            destination: Some(Zone::Exile),
            ..
        } => {
            assert_eq!(player, P0, "the Saga's controller exiles");
            assert!(cards.contains(&card), "{card:?} is offered: {cards:?}");
            runner
                .act(GameAction::SelectCards { cards: vec![card] })
                .expect("exile the chosen card");
        }
        other => panic!("expected the chapter's exile-from-hand choice, got {other:?}"),
    }
    settle(runner);
    assert_eq!(runner.state().objects[&card].zone, Zone::Exile);
}

/// Runs chapters I–III exiling `picks` (one per chapter), then chapter IV.
/// Returns P0's life gained by chapter IV.
fn run_saga(build: impl FnOnce(&mut GameScenario) -> [ObjectId; 3]) -> i32 {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    for player in [P0, P1] {
        scenario.with_library_top(player, &["Forest"; 16]);
    }
    scenario
        .add_creature(P0, "All Good Things...", 0, 0)
        .as_enchantment()
        .with_subtypes(vec!["Saga"])
        .from_oracle_text(ALL_GOOD_THINGS);
    let picks = build(&mut scenario);
    let mut runner = scenario.build();

    for pick in picks {
        next_chapter(&mut runner);
        exile_from_hand(&mut runner, pick);
    }
    let lore = runner
        .state()
        .objects
        .values()
        .find(|obj| obj.name == "All Good Things...")
        .map(|obj| obj.counters.get(&CounterType::Lore).copied().unwrap_or(0));
    assert_eq!(lore, Some(3), "chapters I-III resolved");

    let before = runner.life(P0);
    next_chapter(&mut runner);
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
            && runner.state().stack.is_empty(),
        "chapter IV resolved: {:?}",
        runner.state().waiting_for
    );
    // CR 714.4: the Saga is sacrificed once chapter IV has left the stack —
    // proof the chapter resolved on both the gain and no-gain paths.
    let saga_zone = runner
        .state()
        .objects
        .values()
        .find(|obj| obj.name == "All Good Things...")
        .map(|obj| obj.zone);
    assert_eq!(saga_zone, Some(Zone::Graveyard), "chapter IV resolved");
    runner.life(P0) - before
}

/// CR 205.2a + CR 607.2a: three creature cards exiled with the Saga share the
/// creature card type, so chapter IV gains 10 life.
#[test]
fn all_good_things_gains_life_when_every_exiled_card_shares_a_card_type() {
    let gained = run_saga(|scenario| {
        [
            scenario.add_creature_to_hand(P0, "Bear A", 2, 2).id(),
            scenario.add_creature_to_hand(P0, "Bear B", 2, 2).id(),
            scenario.add_creature_to_hand(P0, "Bear C", 2, 2).id(),
        ]
    });
    assert_eq!(gained, 10);
}

/// CR 205.2a: two creature cards and a land card share no single card type, so
/// chapter IV gains nothing.
#[test]
fn all_good_things_gains_no_life_when_exiled_card_types_are_mixed() {
    let gained = run_saga(|scenario| {
        [
            scenario.add_creature_to_hand(P0, "Bear A", 2, 2).id(),
            scenario.add_creature_to_hand(P0, "Bear B", 2, 2).id(),
            scenario.add_land_to_hand(P0, "Plains").id(),
        ]
    });
    assert_eq!(gained, 0);
}

/// CR 205.2a: an artifact creature, a creature and an artifact creature all
/// share the creature type even though artifact is not shared by all three.
#[test]
fn all_good_things_counts_a_type_shared_by_every_card_among_multiple_types() {
    let gained = run_saga(|scenario| {
        [
            scenario
                .add_creature_to_hand(P0, "Construct", 1, 1)
                .as_artifact_creature()
                .id(),
            scenario.add_creature_to_hand(P0, "Bear", 2, 2).id(),
            scenario
                .add_creature_to_hand(P0, "Golem", 3, 3)
                .as_artifact_creature()
                .id(),
        ]
    });
    assert_eq!(gained, 10);
}
