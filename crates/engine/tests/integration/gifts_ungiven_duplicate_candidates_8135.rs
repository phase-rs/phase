//! Issue #8135 — Gifts Ungiven's chooser prompt lists every revealed card twice.
//!
//! Oracle: "Search your library for up to four cards with different names and
//! reveal them. Target opponent chooses two of those cards. Put the chosen cards
//! into your graveyard and the rest into your hand. Then shuffle."
//!
//! Reported symptoms (Discord, plus a corroborating report): the opponent's
//! chooser interface shows each revealed card twice; clicking one copy visually
//! selects both; the submitted selection and the spell's resolution are correct.
//!
//! Those three together localize the defect precisely. `CardChoiceModal`'s
//! `ChooseFromZoneModal` renders `data.cards.map(...)` with `key={id}` and holds
//! selection in a `Set<ObjectId>`, so a `cards` payload containing the same
//! ObjectId twice produces exactly this: two tiles, one shared selection
//! identity, and a deduplicated `Array.from(set)` on confirm. The frontend is
//! faithful to its input — so the duplication must be in the engine's
//! `WaitingFor::ChooseFromZoneChoice { cards }`.
//!
//! This test asserts on that payload directly.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;

/// Verbatim Scryfall Oracle text (verified 2026-09-09).
const GIFTS_UNGIVEN: &str = "Search your library for up to four cards with \
different names and reveal them. Target opponent chooses two of those cards. \
Put the chosen cards into your graveyard and the rest into your hand. Then \
shuffle.";

/// Four distinct names, so the "with different names" selection constraint is
/// satisfiable. Built from Oracle text rather than the shared card database:
/// Gifts Ungiven is absent from the curated test fixture, and requiring the
/// full export would silently skip this regression in ordinary CI.
const LIBRARY: [&str; 4] = [
    "Library card A",
    "Library card B",
    "Library card C",
    "Library card D",
];

#[test]
fn gifts_ungiven_offers_each_revealed_card_exactly_once() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &LIBRARY);
    let gifts = scenario
        .add_spell_to_hand_from_oracle(P0, "Gifts Ungiven", false, GIFTS_UNGIVEN)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();

    runner.cast(gifts).target_player(P1).resolve();

    // P0's search picks the four distinct-named cards Gifts reveals.
    let WaitingFor::SearchChoice { cards, count, .. } = runner.state().waiting_for.clone() else {
        panic!(
            "reach guard: Gifts must first park P0's library search, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(count, 4, "Gifts searches for up to four cards");
    let found: Vec<_> = cards.into_iter().take(count).collect();
    runner
        .act(GameAction::SelectCards {
            cards: found.clone(),
        })
        .expect("submitting the four searched cards must resume Gifts");

    // Now the targeted opponent chooses two of those revealed cards.
    let WaitingFor::ChooseFromZoneChoice {
        cards: offered,
        player,
        count: pick,
        ..
    } = runner.state().waiting_for.clone()
    else {
        panic!(
            "reach guard: the targeted opponent must be offered the revealed cards, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(player, P1, "the TARGETED opponent chooses (CR 601.2c)");
    assert_eq!(pick, 2, "Gifts has the opponent choose two");

    let mut deduped = offered.clone();
    deduped.sort();
    deduped.dedup();
    assert_eq!(
        offered.len(),
        deduped.len(),
        "issue #8135: the chooser must offer each revealed card EXACTLY ONCE. A \
         duplicated id renders two tiles that share one selection identity in \
         ChooseFromZoneModal (`key={{id}}` + `Set<ObjectId>`), which is why one \
         click appears to select both while the submitted pick stays correct. \
         offered={offered:?}"
    );
    // Identity, not cardinality. A payload of four DIFFERENT unique ids — the
    // resolving Gifts Ungiven card itself, say, swapped in for a revealed one —
    // has four unique members and would satisfy a count-only check while
    // offering the opponent a card that was never revealed.
    let mut expected = found.clone();
    expected.sort();
    assert_eq!(
        deduped, expected,
        "exactly the searched-and-revealed cards may be offered, and nothing \
         else; offered={offered:?} found={found:?}"
    );
}
