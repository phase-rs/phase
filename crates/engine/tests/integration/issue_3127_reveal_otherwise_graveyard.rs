//! Issue #3127: "Reveal the top card of your library. If <cond>, put it into
//! your hand. Otherwise, put it into your graveyard." — the Otherwise →
//! graveyard branch was a runtime NO-OP (the revealed card stayed on top of the
//! library).
//!
//! https://github.com/phase-rs/phase/issues/3127
//!
//! Affects Bloodline Shaman, Call of the Wild, Neurok Familiar, Skirk Drill
//! Sergeant, Zoologist. Root cause: the destination word "graveyard" in "into
//! your graveyard" was misread as a graveyard ORIGIN, so the resolver's
//! origin-mismatch guard (CR 400.7) skipped the move — the revealed card is in
//! the library (CR 701.20b), not the graveyard.
//!
//! This is the DISCRIMINATING runtime test: it drives the real reveal +
//! conditional pipeline and asserts the revealed card's final zone for both
//! branches. Case B (the previously-broken path) FAILS before the fix because
//! the card stays on top of the library instead of going to the graveyard.
//!
//! The runtime harness uses the simple "If it's a creature card" reveal
//! condition (the proven reveal-conditional path, cf. `lurking_predators`) so
//! the test isolates the origin/destination bug being fixed here. The exact
//! Bloodline Shaman "creature card of the chosen type" wording is covered by the
//! parser test `reveal_otherwise_graveyard_branch_has_no_origin`.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::CastPaymentMode;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

/// Bloodline Shaman class: "Reveal the top card of your library. If <cond>, put
/// it into your hand. Otherwise, put it into your graveyard." Modeled here as a
/// triggered ability so the reveal + conditional drive through the real stack
/// pipeline when an opponent casts a spell.
const REVEAL_OTHERWISE_GRAVEYARD: &str = "Whenever an opponent casts a spell, reveal the top card of your library. If it's a creature card, put it into your hand. Otherwise, put it into your graveyard.";

fn zone_of(runner: &GameRunner, id: ObjectId) -> Zone {
    runner.state().objects.get(&id).expect("object exists").zone
}

/// Move `id` to the very top of its owner's library (`library[0]`), regardless
/// of where it currently is, preserving its already-set `card_types`.
fn put_library_top(runner: &mut GameRunner, id: ObjectId) {
    let mut events = Vec::new();
    engine::game::zones::move_to_library_position(runner.state_mut(), id, true, &mut events);
}

/// Build a scenario with the reveal/otherwise enchantment on the battlefield
/// (P0) and a single card seated on top of P0's library, then have P1 cast a
/// spell to fire the trigger and resolve it through the real pipeline. Returns
/// the runner and the library card's id so callers can assert its final zone.
fn run_reveal_conditional(
    library_card: impl FnOnce(&mut GameScenario) -> ObjectId,
) -> (GameRunner, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    scenario
        .add_creature(P0, "Reveal Source", 0, 0)
        .as_enchantment()
        .from_oracle_text(REVEAL_OTHERWISE_GRAVEYARD);

    let revealed = library_card(&mut scenario);
    let opponent_spell = scenario
        .add_creature_to_hand(P1, "Opponent Bear", 2, 2)
        .id();

    let mut runner = scenario.build();
    put_library_top(&mut runner, revealed);

    runner.state_mut().active_player = P1;
    runner.state_mut().priority_player = P1;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P1 };

    let card_id = runner
        .state()
        .objects
        .get(&opponent_spell)
        .expect("spell")
        .card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: opponent_spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("opponent cast should succeed");

    runner.advance_until_stack_empty();
    (runner, revealed)
}

/// Case A: the revealed top card IS a creature — it ends in the HAND (the
/// working branch). Confirms the fix does not regress the hand path.
#[test]
fn reveal_matching_creature_goes_to_hand() {
    let (runner, revealed) =
        run_reveal_conditional(|scenario| scenario.add_creature(P0, "Library Bear", 2, 2).id());

    assert!(
        runner
            .state()
            .objects
            .get(&revealed)
            .unwrap()
            .card_types
            .core_types
            .contains(&CoreType::Creature),
        "precondition: the revealed top card is a creature"
    );
    assert_eq!(
        zone_of(&runner, revealed),
        Zone::Hand,
        "a revealed creature must go to the hand, not stay on the library"
    );
    assert!(
        !runner.state().players[P0.0 as usize]
            .library
            .contains(&revealed),
        "the revealed card must no longer be in the library"
    );
}

/// Case B (the previously-broken path): the revealed top card does NOT match —
/// it must end in the GRAVEYARD, not stay on top of the library.
///
/// This is the discriminating assertion for #3127: before the fix the revealed
/// noncreature stayed on top of the library (`origin: Some(Graveyard)` while the
/// card is in the library tripped the resolver's origin-mismatch guard), so this
/// FAILS on revert.
#[test]
fn reveal_nonmatching_card_goes_to_graveyard() {
    let (runner, revealed) =
        run_reveal_conditional(|scenario| scenario.add_card_to_library_top(P0, "Wastes Land"));

    assert!(
        !runner
            .state()
            .objects
            .get(&revealed)
            .unwrap()
            .card_types
            .core_types
            .contains(&CoreType::Creature),
        "precondition: the revealed top card is a noncreature"
    );
    assert_eq!(
        zone_of(&runner, revealed),
        Zone::Graveyard,
        "the Otherwise branch must put the revealed card into the graveyard \
         (issue #3127: pre-fix it stayed on top of the library)"
    );
    assert!(
        !runner.state().players[P0.0 as usize]
            .library
            .contains(&revealed),
        "the revealed card must NOT remain on top of the library"
    );
}
