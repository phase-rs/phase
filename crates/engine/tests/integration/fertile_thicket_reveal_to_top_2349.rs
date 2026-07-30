//! Runtime regression for #2349 — Fertile Thicket reveal-to-top.
//!
//! Fertile Thicket: "When this land enters, you may look at the top five cards
//! of your library. If you do, reveal up to one basic land card from among them,
//! then put that card on top of your library and the rest on the bottom in any
//! order."
//!
//! On `main` the parser's `parse_dig_destination_tail` recognized only "onto the
//! battlefield" / "into your hand" — NOT "on top of your library". So the kept
//! destination resolved to `None`, the reveal-from-among clause became
//! Unimplemented, and the chosen basic land was NOT kept on top of the library.
//!
//! After the fix the parser recognizes the library-top kept destination
//! (CR 401.4) and promotes the Dig to `reveal: true` because the clause's verb
//! is "reveal" (CR 701.20a, public). The DigChoice resolver — unchanged — then
//! inserts the kept card at index 0 (top) and the rest at the bottom.
//!
//! Driven through a sorcery host (the plan's permitted fallback): the same
//! parsed Dig config runs through the real cast pipeline, so reverting either
//! parser step breaks the assertions below.

use engine::game::scenario::{GameScenario, P0};
use engine::types::actions::GameAction;
use engine::types::card_type::{CoreType, Supertype};
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

/// Fertile Thicket's ETB clause, driven through a sorcery host so the cast
/// pipeline exercises the same parsed Dig configuration.
const FERTILE_THICKET_CLAUSE: &str =
    "You may look at the top five cards of your library. If you do, reveal up to \
     one basic land card from among them, then put that card on top of your \
     library and the rest on the bottom in any order.";

#[test]
fn fertile_thicket_keeps_revealed_basic_land_on_top_and_rest_on_bottom() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);

    // Stage P0's top five cards. Exactly one basic land (a Forest) plus four
    // non-basic-land cards. `add_card_to_library_top` inserts at index 0, so add
    // bottom-of-the-five first and top-of-the-five last. We want the basic land
    // somewhere in the middle so a destination-blind move can't accidentally
    // leave it on top.
    let other4 = scenario.add_card_to_library_top(P0, "Other 4");
    let other3 = scenario.add_card_to_library_top(P0, "Other 3");
    let basic_land = scenario.add_card_to_library_top(P0, "Forest");
    let other2 = scenario.add_card_to_library_top(P0, "Other 2");
    let other1 = scenario.add_card_to_library_top(P0, "Other 1");

    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Fertile Thicket Probe", false, FERTILE_THICKET_CLAUSE)
        .with_mana_cost(ManaCost::zero())
        .id();

    let mut runner = scenario.build();

    // Type the staged cards: the basic land is a basic Forest; the others are
    // non-land cards so they cannot match the "basic land card" filter.
    {
        let forest = runner.state_mut().objects.get_mut(&basic_land).unwrap();
        forest.card_types.core_types.push(CoreType::Land);
        forest.card_types.supertypes.push(Supertype::Basic);
        forest.base_card_types = forest.card_types.clone();
    }
    for &id in &[other1, other2, other3, other4] {
        let obj = runner.state_mut().objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Sorcery);
        obj.base_card_types = obj.card_types.clone();
    }

    // Cast and accept the optional "you may look". The resolution driver halts at
    // the DigChoice prompt (it does not auto-answer dig selections).
    let outcome = runner.cast(spell).accept_optional().resolve();

    let WaitingFor::DigChoice {
        cards,
        selectable_cards,
        ..
    } = outcome.final_waiting_for()
    else {
        panic!(
            "expected DigChoice for the reveal-from-among clause, got {:?}",
            outcome.final_waiting_for()
        );
    };

    // The looked-at set is the top five; only the basic land is selectable.
    assert!(
        cards.contains(&basic_land),
        "the basic land must be among the looked-at cards; got {cards:?}"
    );
    assert!(
        selectable_cards.contains(&basic_land),
        "only the basic land may be kept; selectable = {selectable_cards:?}"
    );

    // Submit the keep-selection: the revealed Forest.
    runner
        .act(GameAction::SelectCards {
            cards: vec![basic_land],
        })
        .expect("selecting the revealed basic land should succeed");
    runner.advance_until_stack_empty();

    let state = runner.state();
    let library: Vec<ObjectId> = state.players[0].library.iter().copied().collect();

    // CR 401.4 (primary assertion — fails if Step 1 reverted): the chosen basic
    // land is kept on TOP of the library, not on the bottom.
    assert_eq!(
        library.first().copied(),
        Some(basic_land),
        "the chosen basic land must be on TOP of the library; library = {library:?}"
    );
    assert_eq!(
        state.objects[&basic_land].zone,
        Zone::Library,
        "the chosen basic land stays in the library"
    );
    assert_ne!(
        library.last().copied(),
        Some(basic_land),
        "the chosen basic land must NOT be on the bottom of the library"
    );

    // The other four cards are pushed to the bottom (in any order).
    let bottom_four: Vec<ObjectId> = library.iter().skip(1).copied().collect();
    for &id in &[other1, other2, other3, other4] {
        assert!(
            bottom_four.contains(&id),
            "non-basic-land card {id:?} must be on the bottom; bottom = {bottom_four:?}"
        );
    }

    // The kept land was not stolen into hand or onto the battlefield.
    assert!(
        !state.players[0].hand.contains(&basic_land),
        "the kept basic land must not be moved to hand"
    );

    // CR 701.20a (second assertion — fails if Step 2 reverted): the looked-at
    // cards were publicly revealed because the clause's verb is "reveal". If the
    // Dig were left at reveal:false (a private look), `revealed_cards` would be
    // empty for these cards.
    assert!(
        state.revealed_cards.contains(&basic_land),
        "the revealed basic land must be publicly revealed (reveal:true); \
         revealed_cards = {:?}",
        state.revealed_cards
    );
}
