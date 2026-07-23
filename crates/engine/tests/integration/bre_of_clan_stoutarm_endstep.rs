//! Bre of Clan Stoutarm — end-step "impulse a nonland card, cast it only if it
//! is cheap enough, otherwise put it into your hand" trigger.
//!
//! Oracle (relevant ability):
//!   At the beginning of your end step, if you gained life this turn, exile
//!   cards from the top of your library until you exile a nonland card. You may
//!   cast that card without paying its mana cost if the spell's mana value is
//!   less than or equal to the amount of life you gained this turn. Otherwise,
//!   put it into your hand.
//!
//! These tests drive the REAL parser (`add_creature_from_oracle`) and the REAL
//! trigger→resolution pipeline (scenario runner). The composition under test is
//! `ExileFromTopUntil { NextMatches(nonland) }` with a `CastFromZone`
//! sub-ability gated by `condition = ObjectManaValue{Target} <=
//! LifeGainedThisTurn{Controller}` and `else_ability = ChangeZone -> Hand`.
//!
//! Revert map (the parser gate this PR adds re-homes the trailing
//! "if the spell's mana value is less than or equal to ..." condition onto the
//! cast clause, which in turn routes "Otherwise" to `else_ability`). Reverting
//! the condition combinator drops the gate: the cast becomes an unconditional
//! sub-ability whose "Otherwise" degrades to a `SequentialSibling`
//! `ChangeZone -> Hand` that always fires, so every impulse hit ends in hand
//! regardless of mana value and is never offered for a free cast.
//!   * `free_cast_when_mana_value_within_life_gained` — REVERT-FAILING. With
//!     the gate, mana value <= life gained presents an `OptionalEffectChoice`
//!     offer and accepting casts the card (it reaches the stack). Reverted,
//!     the card is swept to hand with no offer, so the "exiled before the
//!     offer" and "on the stack after accept" assertions both flip.
//!   * `else_to_hand_when_mana_value_exceeds_life_gained` — companion. Asserts
//!     the mana value > life gained branch puts the card in hand with no cast
//!     offer. End-state alone does not discriminate (the reverted unconditional
//!     path also lands the card in hand); the discrimination for the gate is
//!     carried by `free_cast_*`.
//!
//!   * `decline_eligible_cast_goes_to_hand` — CHARACTERIZATION test (not a
//!     rules-confirmed assertion): pins the current behavior where declining an
//!     eligible (mana value <= life gained) free cast puts the card into HAND.
//!
//! NOTE (Otherwise-routing): the engine fires the parsed `else_ability` on BOTH
//! the condition-false branch (mana value > life) AND the optional-decline
//! branch — the pre-existing else_ability convention (Wick/Chandra "may X,
//! otherwise Y"). So decline-while-eligible -> hand is the deliberate current
//! convention. It is NOT confirmed correct: the strict editorial reading of
//! "Otherwise" (= condition-negation only) implies a declined-but-eligible card
//! would stay exiled, and there is no published Bre ruling either way (as of
//! 2026-07-02). Splitting the two outcomes is class-wide engine debt.

use engine::game::scenario::{GameScenario, P0};
use engine::types::actions::GameAction;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaCost};
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const BRE_ENDSTEP_ORACLE: &str =
    "At the beginning of your end step, if you gained life this turn, \
exile cards from the top of your library until you exile a nonland card. You may cast that card \
without paying its mana cost if the spell's mana value is less than or equal to the amount of life \
you gained this turn. Otherwise, put it into your hand.";

fn put_on_library_top(state: &mut GameState, obj_id: ObjectId, owner: PlayerId) {
    let mut events = Vec::new();
    engine::game::zones::move_to_zone(state, obj_id, Zone::Library, &mut events);
    let player = state.players.iter_mut().find(|p| p.id == owner).unwrap();
    player.library.retain(|id| *id != obj_id);
    player.library.insert(0, obj_id);
}

/// Build Bre on the battlefield plus a P0 library topped with one land and then
/// a nonland spell with mana value `hit_mv`. `life_gained` seeds P0's
/// life-gained-this-turn tally, which both the trigger's intervening-if and the
/// cast clause's `mana value <= life gained` gate read.
fn setup(hit_mv: u32, life_gained: u32) -> (engine::game::scenario::GameRunner, ObjectId) {
    let mut scenario = GameScenario::new();
    // Start after combat so advancing to the end step does not halt at
    // DeclareAttackers (Bre is an eligible attacker).
    scenario.at_phase(engine::types::phase::Phase::PostCombatMain);

    scenario.add_creature_from_oracle(P0, "Bre of Clan Stoutarm", 3, 4, BRE_ENDSTEP_ORACLE);

    let land = scenario.add_basic_land(P0, ManaColor::Red);
    // Target-less sorcery so accepting a free cast lands it straight on the
    // stack with no intervening target prompt.
    let hit = scenario
        .add_spell_to_library_top(P0, "Impulse Hit", false)
        .from_oracle_text("Draw a card.")
        .with_mana_cost(ManaCost::generic(hit_mv))
        .id();

    let mut runner = scenario.build();
    // Library top -> bottom: land (a nonmatching exile), then the nonland stop.
    put_on_library_top(runner.state_mut(), hit, P0);
    put_on_library_top(runner.state_mut(), land, P0);
    runner.state_mut().players[P0.0 as usize].life_gained_this_turn = life_gained;

    (runner, hit)
}

/// Free-cast path: mana value (3) <= life gained (5). The gate passes, so the
/// "you may cast" offer is presented and accepting casts the card for free —
/// it reaches the stack during resolution (Bring to Light shape).
#[test]
fn free_cast_when_mana_value_within_life_gained() {
    let (mut runner, hit) = setup(3, 5);

    runner.advance_to_end_step();
    runner.advance_until_stack_empty();

    // Exile-until ran: the nonland hit was exiled from the top of the library.
    assert_eq!(
        runner.state().objects[&hit].zone,
        Zone::Exile,
        "the nonland hit must be exiled by ExileFromTopUntil before the cast offer"
    );
    // The gate passed, so the optional free-cast offer is presented to P0.
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::OptionalEffectChoice { player, .. } if player == P0
        ),
        "expected a 'you may cast' offer for P0 when mana value <= life gained; got {:?}",
        runner.state().waiting_for
    );

    runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .expect("accept the free-cast offer");

    assert_eq!(
        runner.state().objects[&hit].zone,
        Zone::Stack,
        "accepting the free cast must put the card on the stack; zone = {:?}, waiting_for = {:?}",
        runner.state().objects[&hit].zone,
        runner.state().waiting_for,
    );
}

/// Decline-while-eligible (CHARACTERIZATION test, not rules-confirmed): mana
/// value (3) <= life gained (5), so the offer is presented — but the player
/// declines. Under the current pre-existing else_ability convention the
/// declined card goes to HAND (same branch as the too-expensive case). Revisit
/// if the condition-false vs optional-decline split lands as engine work, or if
/// an official Bre ruling appears — the strict "Otherwise" reading would leave
/// a declined-but-eligible card exiled instead.
#[test]
fn decline_eligible_cast_goes_to_hand() {
    let (mut runner, hit) = setup(3, 5);

    runner.advance_to_end_step();
    runner.advance_until_stack_empty();

    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::OptionalEffectChoice { player, .. } if player == P0
        ),
        "expected a 'you may cast' offer for P0 when mana value <= life gained; got {:?}",
        runner.state().waiting_for
    );

    runner
        .act(GameAction::DecideOptionalEffect { accept: false })
        .expect("decline the free-cast offer");

    assert_eq!(
        runner.state().objects[&hit].zone,
        Zone::Hand,
        "declining an eligible free cast must put the card into hand (the non-cast outcome)"
    );
    assert!(
        runner.state().players[P0.0 as usize].hand.contains(&hit),
        "the declined card must be in P0's hand"
    );
}

/// Companion (mana value > life gained): the gate fails, so the card is never
/// offered for a cast and instead goes to hand via the `else_ability`. End-state
/// alone is not revert-discriminating here (the reverted unconditional path also
/// lands the card in hand); `free_cast_when_mana_value_within_life_gained`
/// carries the revert-failing discrimination for the gate.
#[test]
fn else_to_hand_when_mana_value_exceeds_life_gained() {
    let (mut runner, hit) = setup(5, 2);

    runner.advance_to_end_step();
    runner.advance_until_stack_empty();

    // No free-cast offer is pending — the gate short-circuited to the else branch.
    assert!(
        !matches!(
            runner.state().waiting_for,
            WaitingFor::OptionalEffectChoice { .. }
        ),
        "no cast offer may be presented when mana value > life gained; got {:?}",
        runner.state().waiting_for
    );
    // The exiled card is moved to hand, not cast and not left in exile.
    assert_eq!(
        runner.state().objects[&hit].zone,
        Zone::Hand,
        "when mana value > life gained the card must be put into hand, not cast/left in exile"
    );
    assert!(
        runner.state().players[P0.0 as usize].hand.contains(&hit),
        "the card must be in P0's hand"
    );
}

/// Intervening-if false: no life gained this turn. The trigger's "if you gained
/// life this turn" gate (CR 603.4) fails, so the trigger never resolves and the
/// library is untouched.
#[test]
fn no_trigger_when_no_life_gained() {
    let (mut runner, hit) = setup(3, 0);

    runner.advance_to_end_step();
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().objects[&hit].zone,
        Zone::Library,
        "with no life gained this turn the end-step trigger must not exile anything"
    );
    assert!(
        !matches!(
            runner.state().waiting_for,
            WaitingFor::OptionalEffectChoice { .. }
        ),
        "no cast offer may occur when the intervening-if fails"
    );
}
