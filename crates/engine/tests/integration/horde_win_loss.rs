//! Horde Magic PR3 — no-life-total → mill redirect and Horde-variant win/lose.
//!
//! Horde Magic is a casual cooperative variant. The Horde seat (seat 0) has NO
//! life total:
//!   - damage / direct life loss it would suffer instead MILLS that many cards
//!     from the top of its own library (CR 120.3a damage→life-loss and CR 119.3
//!     direct life loss are redirected);
//!   - life gain does nothing to it;
//!   - it is exempt from the CR 704.5a "0 or less life loses" state-based action;
//!   - it is defeated (survivors win) when its library is empty AND it controls
//!     no creature, routed through the existing archenemy game-over path
//!     (CR 104.2a).
//!
//! Every test names an assertion that flips if the corresponding PR3 wiring is
//! reverted (see the per-test doc comments). Negative assertions are paired with
//! a positive reach-guard proving the input reached the seam under test.

use engine::game::effects::life::{apply_damage_life_loss, apply_life_gain, resolve_lose};
use engine::game::effects::phase_out::resolve as resolve_phase_out;
use engine::game::sba::check_state_based_actions;
use engine::game::scenario::GameRunner;
use engine::game::zones::create_object;
use engine::types::ability::{Effect, QuantityExpr, ResolvedAbility, TargetFilter, TargetRef};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::events::GameEvent;
use engine::types::format::{ChallengeDeck, FormatConfig};
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const HORDE: PlayerId = PlayerId(0);
const SURVIVOR: PlayerId = PlayerId(1);

/// A minimal Horde game (seat 0 = Horde, seat 1 = survivor).
fn horde_state() -> GameState {
    GameState::new(
        FormatConfig::horde(ChallengeDeck::CybermanHorde.default_ruleset()),
        2,
        42,
    )
}

/// Put `n` distinct cards on top of `owner`'s library.
fn stock_library(state: &mut GameState, owner: PlayerId, n: usize) {
    for i in 0..n {
        let card_id = CardId(state.next_object_id);
        create_object(state, card_id, owner, format!("Lib {i}"), Zone::Library);
    }
}

/// Create a creature on the battlefield under `controller`'s control.
fn add_creature(state: &mut GameState, controller: PlayerId) -> ObjectId {
    let card_id = CardId(state.next_object_id);
    let id = create_object(
        state,
        card_id,
        controller,
        "Zombie".to_string(),
        Zone::Battlefield,
    );
    let obj = state.objects.get_mut(&id).unwrap();
    obj.card_types.core_types.push(CoreType::Creature);
    obj.base_card_types = obj.card_types.clone();
    obj.power = Some(2);
    obj.toughness = Some(2);
    obj.base_power = Some(2);
    obj.base_toughness = Some(2);
    id
}

/// Damage (routed through the real `apply_damage_life_loss` seam, the single
/// damage→life-loss mutation point per CR 120.3a) that the Horde would suffer
/// MILLS that many cards from the top of the Horde's own library; the Horde's
/// life is untouched.
///
/// Revert guard: dropping the `player_has_no_life_total` redirect in
/// `apply_life_loss_after_replacement` decrements `life` and leaves the library
/// intact — the two library/graveyard assertions flip and `life` changes.
#[test]
fn damage_to_horde_mills_instead_of_losing_life() {
    let mut state = horde_state();
    stock_library(&mut state, HORDE, 10);
    let life_before = state.players[HORDE.0 as usize].life;
    let library_before = state.players[HORDE.0 as usize].library.len();

    let mut events = Vec::new();
    let lost = apply_damage_life_loss(&mut state, HORDE, 3, &mut events).unwrap();

    assert_eq!(
        lost, 3,
        "the redirect must report the loss amount as milled"
    );
    assert_eq!(
        state.players[HORDE.0 as usize].library.len(),
        library_before - 3,
        "3 damage must mill exactly 3 cards from the Horde's library"
    );
    assert_eq!(
        state.players[HORDE.0 as usize].graveyard.len(),
        3,
        "the milled cards land in the Horde's graveyard"
    );
    assert_eq!(
        state.players[HORDE.0 as usize].life, life_before,
        "the Horde has no life total — its life must not change"
    );
    assert_eq!(
        state.players[HORDE.0 as usize].life_lost_this_turn, 0,
        "no life is lost, so life_lost_this_turn must not increment"
    );
}

/// A direct `Effect::LoseLife` targeting the Horde also mills (proving
/// `resolve_lose` delegates to the shared `apply_life_loss_after_replacement`
/// redirect rather than inlining the life mutation — the review's M3 finding).
///
/// Revert guard: re-inlining `resolve_lose`'s Execute arm (`player.life -= ..`)
/// bypasses the redirect — the library stays full and `life` drops.
#[test]
fn direct_lose_life_on_horde_mills() {
    let mut state = horde_state();
    stock_library(&mut state, HORDE, 10);
    let life_before = state.players[HORDE.0 as usize].life;

    let ability = ResolvedAbility::new(
        Effect::LoseLife {
            amount: QuantityExpr::Fixed { value: 4 },
            target: None,
        },
        vec![TargetRef::Player(HORDE)],
        ObjectId(100),
        SURVIVOR,
    );
    let mut events = Vec::new();
    resolve_lose(&mut state, &ability, &mut events).unwrap();

    assert_eq!(
        state.players[HORDE.0 as usize].library.len(),
        6,
        "Effect::LoseLife 4 on the Horde must mill 4 (10 - 4)"
    );
    assert_eq!(
        state.players[HORDE.0 as usize].graveyard.len(),
        4,
        "the 4 milled cards land in the Horde's graveyard"
    );
    assert_eq!(
        state.players[HORDE.0 as usize].life, life_before,
        "the Horde's life must not change from LoseLife"
    );
}

/// Lifelink hostile: when a survivor's source deals damage to the Horde, the
/// Horde mills (loss path redirected) BUT the survivor's lifelink life GAIN
/// still applies — life gain is redirected to a no-op only for the Horde, never
/// for survivors.
///
/// Revert guard (gain not over-redirected): if the life-gain no-op were applied
/// to all players (not scoped to the Horde), the survivor's `life` would stay
/// flat and this assertion flips. Reach-guard: the Horde library actually shrank,
/// proving the loss side reached the redirect in the same scenario.
#[test]
fn survivor_lifelink_gains_while_horde_mills() {
    let mut state = horde_state();
    stock_library(&mut state, HORDE, 10);
    let survivor_life_before = state.players[SURVIVOR.0 as usize].life;
    let horde_library_before = state.players[HORDE.0 as usize].library.len();

    let mut events = Vec::new();
    // Lifelink: 5 damage to the Horde and 5 life to the survivor dealer.
    apply_damage_life_loss(&mut state, HORDE, 5, &mut events).unwrap();
    let gained = apply_life_gain(&mut state, SURVIVOR, 5, &mut events).unwrap();

    // Reach-guard: the loss side genuinely hit the Horde mill redirect.
    assert_eq!(
        state.players[HORDE.0 as usize].library.len(),
        horde_library_before - 5,
        "reach-guard: the Horde must have milled 5 (loss side reached the redirect)"
    );
    // Load-bearing: the survivor's life gain is NOT redirected.
    assert_eq!(gained, 5, "the survivor's lifelink gain must apply in full");
    assert_eq!(
        state.players[SURVIVOR.0 as usize].life,
        survivor_life_before + 5,
        "the survivor gains 5 life from lifelink — gain no-op is Horde-scoped only"
    );
}

/// The Horde is exempt from the CR 704.5a "0 or less life" state-based action:
/// even with its `life` forced to 0, the SBA loop must not eliminate it and the
/// game must not end.
///
/// Revert guard: removing the `player_has_no_life_total` filter in
/// `collect_life_losers` collects the Horde as a loser and eliminates it — the
/// `!is_eliminated` assertion flips. Reach-guard: the Horde's life is asserted
/// <= 0 so the loser-collection threshold is genuinely crossed.
#[test]
fn horde_exempt_from_zero_life_sba() {
    let mut state = horde_state();
    // Give the Horde a creature + library so it is unambiguously "alive/undefeated".
    add_creature(&mut state, HORDE);
    stock_library(&mut state, HORDE, 3);
    // Survivor is healthy so it is not a competing loser.
    state.players[SURVIVOR.0 as usize].life = 20;
    // Force the Horde's underlying life field to 0.
    state.players[HORDE.0 as usize].life = 0;
    state.phase = Phase::PreCombatMain;
    state.active_player = SURVIVOR;
    state.priority_player = SURVIVOR;
    state.waiting_for = WaitingFor::Priority { player: SURVIVOR };

    // Reach-guard: the Horde really is at 0-or-less life.
    assert!(
        state.players[HORDE.0 as usize].life <= 0,
        "reach-guard: the Horde must be at 0-or-less life so the loser threshold is crossed"
    );

    let mut events = Vec::new();
    check_state_based_actions(&mut state, &mut events);

    assert!(
        !state.players[HORDE.0 as usize].is_eliminated,
        "the Horde has no life total — a 0 life value must NOT eliminate it"
    );
    assert!(
        !matches!(state.waiting_for, WaitingFor::GameOver { .. }),
        "the game must not end from the Horde's 0 life; waiting_for={:?}",
        state.waiting_for
    );
}

/// A SURVIVOR losing life decrements life normally — the mill redirect is scoped
/// to the Horde seat only.
///
/// Revert guard: if the redirect were not seat-scoped, the survivor's library
/// would shrink and its `life` would stay flat — both assertions flip.
#[test]
fn survivor_loses_life_normally() {
    let mut state = horde_state();
    stock_library(&mut state, SURVIVOR, 10);
    let life_before = state.players[SURVIVOR.0 as usize].life;
    let library_before = state.players[SURVIVOR.0 as usize].library.len();

    let mut events = Vec::new();
    apply_damage_life_loss(&mut state, SURVIVOR, 3, &mut events).unwrap();

    assert_eq!(
        state.players[SURVIVOR.0 as usize].life,
        life_before - 3,
        "a survivor loses life normally — no redirect"
    );
    assert_eq!(
        state.players[SURVIVOR.0 as usize].library.len(),
        library_before,
        "a survivor's library must be untouched by life loss"
    );
}

/// WIN: the Horde is defeated (survivors win) when its library is empty AND it
/// controls no creature. Driven through the real engine action path
/// (`apply_as_current` → `reconcile_terminal_result` → `check_game_over`), which
/// fires on every action.
///
/// Revert guard: without the Horde branch in `check_game_over`, the Horde is
/// still "living" (it can't be eliminated), so `archenemy_alive` stays true and
/// the game does NOT end — `GameOver { winner: Some(SURVIVOR) }` never appears.
#[test]
fn horde_defeated_when_library_empty_and_no_creatures() {
    let mut state = horde_state();
    // Horde: empty library (default), no creatures (default) → defeated.
    // Survivor: healthy with a library so it neither decks nor loses on life.
    stock_library(&mut state, SURVIVOR, 5);
    state.players[SURVIVOR.0 as usize].life = 20;
    state.turn_number = 4;
    state.phase = Phase::Upkeep;
    state.active_player = SURVIVOR;
    state.priority_player = SURVIVOR;
    state.waiting_for = WaitingFor::Priority { player: SURVIVOR };

    // Precondition: the game must not already be over.
    assert!(
        !matches!(state.waiting_for, WaitingFor::GameOver { .. }),
        "precondition: game must not already be over"
    );

    let mut runner = GameRunner::from_state(state);
    runner.act(GameAction::PassPriority).expect("pass priority");

    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::GameOver {
                winner: Some(SURVIVOR)
            }
        ),
        "the Horde (empty library, no creatures) is defeated — the survivor wins. \
         waiting_for={:?}",
        runner.state().waiting_for
    );
}

/// Positive reach-guard for the WIN's AND: with the library still empty but a
/// Horde creature on the battlefield, the Horde is NOT defeated and the game
/// does NOT end. Proves `horde_is_defeated` requires BOTH conditions (a creature
/// present blocks the win), so the WIN above is not a spurious library-only win.
#[test]
fn horde_not_defeated_while_it_controls_a_creature() {
    let mut state = horde_state();
    // Empty library, but the Horde still controls a creature.
    add_creature(&mut state, HORDE);
    stock_library(&mut state, SURVIVOR, 5);
    state.players[SURVIVOR.0 as usize].life = 20;
    state.turn_number = 4;
    state.phase = Phase::Upkeep;
    state.active_player = SURVIVOR;
    state.priority_player = SURVIVOR;
    state.waiting_for = WaitingFor::Priority { player: SURVIVOR };

    let mut runner = GameRunner::from_state(state);
    runner.act(GameAction::PassPriority).expect("pass priority");

    assert!(
        !matches!(runner.state().waiting_for, WaitingFor::GameOver { .. }),
        "the Horde still controls a creature — it is NOT defeated (library-empty alone \
         is insufficient). waiting_for={:?}",
        runner.state().waiting_for
    );
}

/// LOSE: a survivor at 0 or less life is eliminated by the ordinary CR 704.5a
/// SBA; with the survivor gone and the Horde undefeated (it controls a creature),
/// the Horde wins via the archenemy game-over path.
///
/// Revert guard: without the Horde branch, `archenemy_alive = living.contains`
/// still yields `Some(HORDE)` here (the Horde is living), so this particular
/// assertion is robust; the discriminating value is that the winner is the HORDE
/// seat, exercising the archenemy branch with the Horde as the surviving side.
#[test]
fn survivor_at_zero_life_loses_horde_wins() {
    let mut state = horde_state();
    add_creature(&mut state, HORDE); // Horde undefeated.
    stock_library(&mut state, HORDE, 3);
    state.players[SURVIVOR.0 as usize].life = 0;
    state.phase = Phase::PreCombatMain;
    state.active_player = HORDE;
    state.priority_player = HORDE;
    state.waiting_for = WaitingFor::Priority { player: HORDE };

    let mut events: Vec<GameEvent> = Vec::new();
    check_state_based_actions(&mut state, &mut events);

    assert!(
        state.players[SURVIVOR.0 as usize].is_eliminated,
        "a survivor at 0 life must be eliminated by the CR 704.5a SBA"
    );
    assert!(
        matches!(
            state.waiting_for,
            WaitingFor::GameOver {
                winner: Some(HORDE)
            }
        ),
        "with the survivor eliminated and the Horde undefeated, the Horde wins. \
         waiting_for={:?}",
        state.waiting_for
    );
}

/// CR 702.26b: a phased-out creature is treated as though it doesn't exist, so
/// only PHASED-IN creatures keep the Horde alive. Empty library + a Horde
/// creature phased out by a Reality Ripple-shaped `Effect::PhaseOut` (driven
/// through the real phase-out resolver) → the Horde is defeated and the
/// survivor wins — even though the creature is still physically on the
/// battlefield and will phase back in on the Horde's next untap.
///
/// Paired control: `horde_not_defeated_while_it_controls_a_creature` runs the
/// identical setup with the creature phased IN and the game does NOT end.
///
/// Revert guard: without the phased-in filter in `horde_is_defeated`, the
/// phased-out creature holds the creature clause open and no `GameOver`
/// appears — the winner assertion flips.
#[test]
fn phased_out_creature_does_not_keep_horde_alive() {
    let mut state = horde_state();
    // Empty library (default), but the Horde controls a creature.
    let zombie = add_creature(&mut state, HORDE);
    stock_library(&mut state, SURVIVOR, 5);
    state.players[SURVIVOR.0 as usize].life = 20;

    // Reality Ripple: "Target artifact, creature, or land phases out." Drive
    // the REAL phase-out resolver with the creature as the resolved target.
    let ripple_card = CardId(state.next_object_id);
    let ripple = create_object(
        &mut state,
        ripple_card,
        SURVIVOR,
        "Reality Ripple".to_string(),
        Zone::Battlefield,
    );
    let ability = ResolvedAbility::new(
        Effect::PhaseOut {
            target: TargetFilter::SpecificObject { id: zombie },
        },
        vec![TargetRef::Object(zombie)],
        ripple,
        SURVIVOR,
    );
    let mut events = Vec::new();
    resolve_phase_out(&mut state, &ability, &mut events).expect("phase out resolves");

    // Reach-guard: the resolver genuinely phased the creature out (still on
    // the battlefield — phasing out is not leaving).
    assert!(
        state.objects[&zombie].is_phased_out(),
        "reach-guard: Reality Ripple must have phased the Horde creature out"
    );
    assert!(
        state.battlefield.contains(&zombie),
        "reach-guard: a phased-out permanent stays on the battlefield"
    );

    state.turn_number = 4;
    state.phase = Phase::Upkeep;
    state.active_player = SURVIVOR;
    state.priority_player = SURVIVOR;
    state.waiting_for = WaitingFor::Priority { player: SURVIVOR };

    let mut runner = GameRunner::from_state(state);
    runner.act(GameAction::PassPriority).expect("pass priority");

    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::GameOver {
                winner: Some(SURVIVOR)
            }
        ),
        "the Horde's only creature is phased out (treated as though it doesn't \
         exist) with an empty library — the Horde is defeated and the survivor \
         wins. waiting_for={:?}",
        runner.state().waiting_for
    );
}
