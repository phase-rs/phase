//! Regression: Ashnod's Cylix's "then exiles the rest" must exile the cards the
//! player did NOT put back on top.
//!
//! Verified Oracle text: "{3}, {T}: Target player looks at the top three cards
//! of their library, puts one of them back on top of their library, then exiles
//! the rest."
//!
//! CR 608.2c + CR 701.20e: the instructions run in the order written, so "the
//! rest" is the looked-at cards minus the card put back. The ability resolves
//! as one look-and-keep: the kept card goes back on top and exactly the other
//! looked-at cards are exiled; nothing below them moves.

use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const ASHNODS_CYLIX: &str = "{3}, {T}: Target player looks at the top three cards of their library, puts one of them back on top of their library, then exiles the rest.";

struct Setup {
    runner: GameRunner,
    /// Top three, topmost first: `looked_at[0]` is on top of the library.
    looked_at: [ObjectId; 3],
    deep_card: ObjectId,
}

/// Two-player board with an untapped Cylix, `{3}` floating, and four library
/// cards: the looked-at top three plus one deeper card that must never move.
fn setup() -> Setup {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // Library is built bottom-first: each add lands on top, so "Deep Card" sits
    // below the three cards the ability looks at.
    let deep_card = scenario.add_card_to_library_top(P0, "Deep Card");
    let mut looked_at = [
        scenario.add_card_to_library_top(P0, "Third From Top"),
        scenario.add_card_to_library_top(P0, "Second From Top"),
        scenario.add_card_to_library_top(P0, "Kept On Top"),
    ];
    // Built bottom-first, so the array above is deepest-first; reorder it to
    // topmost-first to match its documented meaning.
    looked_at.reverse();
    let cylix = scenario
        .add_artifact_from_oracle(P0, "Ashnod's Cylix", ASHNODS_CYLIX)
        .id();
    let mut runner = scenario.build();
    runner.state_mut().players[P0.0 as usize].mana_pool.clear();
    let pool = &mut runner.state_mut().players[P0.0 as usize].mana_pool;
    for _ in 0..3 {
        pool.add(ManaUnit::new(
            ManaType::Colorless,
            ObjectId(0),
            false,
            vec![],
        ));
    }
    let outcome = runner.activate(cylix, 0).target_players(&[P0]).resolve();
    assert!(
        runner.state().players[P0.0 as usize].mana_pool.is_empty(),
        "the {{3}} activation cost was paid from the pool"
    );
    // Reach guard: the ability stops for the put-back choice before anything
    // is exiled.
    match outcome.final_waiting_for() {
        WaitingFor::DigChoice {
            player,
            library_owner,
            cards,
            keep_count,
            up_to,
            kept_destination,
            rest_destination,
            ..
        } => {
            assert_eq!(*player, P0);
            assert_eq!(*library_owner, P0, "the target player's library");
            assert_eq!(*keep_count, 1, "puts ONE of them back on top");
            assert!(!*up_to, "putting one back is mandatory");
            assert_eq!(*kept_destination, Some(Zone::Library));
            assert_eq!(*rest_destination, Some(Zone::Exile));
            assert_eq!(
                cards.as_slice(),
                looked_at.as_slice(),
                "the choice offers exactly the looked-at cards"
            );
        }
        other => panic!("expected the put-back DigChoice, got {other:?}"),
    }
    for card in looked_at.iter().chain([&deep_card]) {
        assert_eq!(
            runner.state().objects[card].zone,
            Zone::Library,
            "nothing moves before the choice"
        );
    }
    Setup {
        runner,
        looked_at,
        deep_card,
    }
}

/// Keep `looked_at[keep]` on top; exactly the other two looked-at cards are
/// exiled, the deeper card is untouched, and resolution finishes.
fn assert_kept_card_routes_the_rest(keep: usize) {
    let Setup {
        mut runner,
        looked_at,
        deep_card,
    } = setup();
    let kept = looked_at[keep];
    let rest: Vec<ObjectId> = looked_at.iter().copied().filter(|id| *id != kept).collect();

    runner
        .act(GameAction::SelectCards { cards: vec![kept] })
        .expect("submit the card put back on top");

    assert_eq!(
        runner.state().objects[&kept].zone,
        Zone::Library,
        "the kept card stays in the library"
    );
    for card in &rest {
        assert_eq!(
            runner.state().objects[card].zone,
            Zone::Exile,
            "the rest is exiled"
        );
    }
    assert_eq!(
        runner.state().objects[&deep_card].zone,
        Zone::Library,
        "a card below the looked-at population never moves"
    );
    assert_eq!(
        runner.state().players[P0.0 as usize]
            .library
            .iter()
            .copied()
            .collect::<Vec<_>>(),
        vec![kept, deep_card],
        "the kept card is back on top, above the deeper card"
    );
    assert!(runner.state().stack.is_empty(), "resolution must finish");
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "resolution must return priority, got {:?}",
        runner.state().waiting_for
    );
}

/// The reviewed bug: "the rest" named the kept card and exiled it, leaving the
/// other two in the library.
#[test]
fn kept_top_card_stays_on_top_and_the_rest_is_exiled() {
    assert_kept_card_routes_the_rest(0);
}

/// The routed card must follow the player's choice, not a fixed position.
#[test]
fn keeping_a_different_card_routes_that_card_to_the_top() {
    assert_kept_card_routes_the_rest(1);
    assert_kept_card_routes_the_rest(2);
}
