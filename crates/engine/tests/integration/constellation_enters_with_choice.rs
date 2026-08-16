//! Issue #830 — ETB observers must fire when a permanent enters carrying an
//! "As it enters, choose …" replacement that pauses the entry on a player
//! choice (e.g. Valgavoth's Lair — "As it enters, choose a color").
//!
//! Root cause: such an entry returns `WaitingFor::NamedChoice` instead of
//! `WaitingFor::Priority`, so the canonical priority-time trigger collection
//! (`engine_priority::run_post_action_pipeline`) is skipped and the entering
//! permanent's `ZoneChanged` event never reaches `process_triggers`. Every ETB
//! observer on the battlefield (constellation like Doomwake Giant, Soul Warden,
//! …) is silently dropped for that entry.
//!
//! These tests drive the REAL apply() pipeline (play land → resolve as-enters
//! choice → resolve triggers off the stack), not a hand-built state.

use engine::game::scenario::GameScenario;
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

const P0: PlayerId = PlayerId(0);
const P1: PlayerId = PlayerId(1);

// Doomwake Giant's constellation: every enchantment you control entering gives
// opponents' creatures -1/-1 until end of turn (CR 603.2 event-based trigger).
const DOOMWAKE: &str = "Constellation — Whenever this creature or another enchantment you control \
     enters, creatures your opponents control get -1/-1 until end of turn.";

// Valgavoth's Lair: an enchantment land with an as-enters color choice — the
// reported failing class. Its `Choose{Color, persist:true}` replacement pauses
// the entry on `WaitingFor::NamedChoice`.
const VALGAVOTHS_LAIR: &str = "~ enters tapped. As it enters, choose a color.";

// A plain enchantment with NO as-enters choice — resolves off the stack to
// `WaitingFor::Priority`, exercising the already-working trigger path.
// Deliberately has no ETB effect so no side-state is produced (drawing from an
// empty library would set `drew_from_empty_library` and kill P0 at the next
// SBA check before Doomwake's trigger resolves).
const PLAIN_ENCHANTMENT: &str = "Hexproof.";

// Soul Warden: a non-constellation ETB observer (proves the fix covers the
// general ETB-observer class, not just constellation).
const SOUL_WARDEN: &str = "Whenever another creature enters, you gain 1 life.";

/// Discriminating bug repro (#830): with Doomwake Giant on the battlefield,
/// playing an as-enters-choose-color land and answering the color MUST fire
/// Doomwake's constellation trigger. Fails before the fix — the entry's
/// `ZoneChanged` never reaches `process_triggers`, so the opponent creature
/// keeps its full toughness.
///
/// Reverting the fix flips the final assertion: `opp_creature.toughness`
/// remains 2 (trigger never collected) instead of dropping to 1.
#[test]
fn as_enters_choice_land_fires_constellation() {
    let mut scenario = GameScenario::new_n_player(2, 7);
    scenario.at_phase(Phase::PreCombatMain);

    // Doomwake Giant (the constellation observer) under P0.
    let doomwake = scenario
        .add_creature_from_oracle(P0, "Doomwake Giant", 4, 6, DOOMWAKE)
        .as_enchantment()
        .id();

    // An opponent creature whose toughness the constellation trigger reduces.
    let opp_creature = scenario.add_creature(P1, "Opponent Bear", 2, 2).id();

    // The as-enters-choice land in P0's hand.
    // Valgavoth's Lair is an Enchantment Land — the Enchantment type is what
    // Doomwake's "another enchantment you control" branch matches on.
    let lair = {
        let mut b = scenario.add_land_to_hand(P0, "Valgavoth's Lair");
        b.from_oracle_text(VALGAVOTHS_LAIR).as_enchantment();
        b.id()
    };

    let mut runner = scenario.build();
    let card_id = runner.state().objects.get(&lair).unwrap().card_id;

    let _ = doomwake;

    // Play the land — its entry pauses on the as-enters color choice.
    runner
        .act(GameAction::PlayLand {
            object_id: lair,
            card_id,
        })
        .expect("play Valgavoth's Lair");

    let WaitingFor::NamedChoice { options, .. } = runner.state().waiting_for.clone() else {
        panic!(
            "as-enters land must pause on the color choice, got {}",
            runner.waiting_for_kind()
        );
    };
    let color = options.first().expect("color options").clone();

    // Answer the color — this is where the deferred entry event must replay.
    runner
        .act(GameAction::ChooseOption { choice: color })
        .expect("choose the color");

    // Resolve the now-stacked constellation trigger.
    runner.advance_until_stack_empty();

    let toughness = runner
        .state()
        .objects
        .get(&opp_creature)
        .and_then(|o| o.toughness)
        .expect("opponent creature toughness");
    assert_eq!(
        toughness, 1,
        "Doomwake's constellation must fire when an as-enters-choice land enters \
         (#830): opponent's 2/2 should be 1/1 after -1/-1, got toughness {toughness}"
    );
}

/// Regression guard for the already-working path: a PLAIN enchantment (no
/// as-enters choice) resolving off the stack to `WaitingFor::Priority` must
/// STILL fire Doomwake's constellation. Proves the fix does not regress the
/// canonical priority-time trigger collection nor double-fire the entry.
#[test]
fn plain_enchantment_still_fires_constellation() {
    let mut scenario = GameScenario::new_n_player(2, 7);
    scenario.at_phase(Phase::PreCombatMain);

    let doomwake = scenario
        .add_creature_from_oracle(P0, "Doomwake Giant", 4, 6, DOOMWAKE)
        .as_enchantment()
        .id();
    let opp_creature = scenario.add_creature(P1, "Opponent Bear", 2, 2).id();

    // A plain enchantment cast from hand — no as-enters choice.
    let aura = {
        let mut b = scenario.add_creature_to_hand(P0, "Plain Sigil", 0, 0);
        b.from_oracle_text(PLAIN_ENCHANTMENT).as_enchantment();
        b.id()
    };

    let mut runner = scenario.build();
    let card_id = runner.state().objects.get(&aura).unwrap().card_id;
    let _ = doomwake;

    runner
        .act(GameAction::CastSpell {
            object_id: aura,
            card_id,
            targets: vec![],
            payment_mode: engine::types::game_state::CastPaymentMode::Auto,
        })
        .expect("cast the plain enchantment");
    runner.advance_until_stack_empty();

    let toughness = runner
        .state()
        .objects
        .get(&opp_creature)
        .and_then(|o| o.toughness)
        .expect("opponent creature toughness");
    assert_eq!(
        toughness, 1,
        "plain enchantment (Priority-result path) must still fire constellation; \
         got toughness {toughness}"
    );
}

/// Class-generality guard: a NON-constellation ETB observer (Soul Warden, "gain
/// 1 life whenever another creature enters") must also fire off an as-enters-
/// choice CREATURE entry. Proves the fix covers the whole ETB-observer class,
/// not just constellation/enchantment cards.
#[test]
fn as_enters_choice_creature_fires_soul_warden() {
    let mut scenario = GameScenario::new_n_player(2, 7);
    scenario.at_phase(Phase::PreCombatMain);

    let soul_warden = scenario
        .add_creature_from_oracle(P0, "Soul Warden", 1, 1, SOUL_WARDEN)
        .id();
    let _ = soul_warden;

    // An as-enters-choose-color creature in P0's hand (Voice of All shape).
    let voice = {
        let mut b = scenario.add_creature_to_hand(P0, "Voice of All", 2, 2);
        b.from_oracle_text("As this creature enters, choose a color.");
        b.id()
    };

    let mut runner = scenario.build();
    let card_id = runner.state().objects.get(&voice).unwrap().card_id;
    let life_before = runner.life(P0);

    runner
        .act(GameAction::CastSpell {
            object_id: voice,
            card_id,
            targets: vec![],
            payment_mode: engine::types::game_state::CastPaymentMode::Auto,
        })
        .expect("cast the as-enters-choice creature");
    runner.advance_until_stack_empty();

    let WaitingFor::NamedChoice { options, .. } = runner.state().waiting_for.clone() else {
        panic!(
            "as-enters creature must pause on the color choice, got {}",
            runner.waiting_for_kind()
        );
    };
    let color = options.first().expect("color options").clone();
    runner
        .act(GameAction::ChooseOption { choice: color })
        .expect("choose the color");
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.life(P0),
        life_before + 1,
        "Soul Warden must gain 1 life when an as-enters-choice creature enters (#830)"
    );
}
