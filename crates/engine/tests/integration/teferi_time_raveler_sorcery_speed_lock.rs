//! Discriminating end-to-end guard for Teferi, Time Raveler's static
//! restriction: "Each opponent can cast spells only any time they could cast a
//! sorcery."
//!
//! This drives the REAL Teferi Oracle text through the full synthesis pipeline
//! (`from_oracle_text` → `synthesize_all`) onto a battlefield permanent, then
//! exercises the production cast action (`GameAction::CastSpell`) — the same
//! path the engine uses for a human/AI cast. The static lowers to
//! `StaticMode::CantCastDuring { who: opponents, when: NotSorcerySpeed }`, which
//! the cast handler enforces via `is_blocked_by_cant_cast_during`
//! (`game/casting.rs`, CR 101.2 + CR 307.5).
//!
//! The Teferi fix under test (`swallow_check.rs`) is DIAGNOSTIC-ONLY — it stops
//! a false-positive `Optional_YouMay` swallow on the [+1] flash grant. The
//! runtime restriction was already correct; this test proves that
//! independently, and is structured fail-first: a baseline opponent with NO
//! Teferi on the battlefield CAN cast the same instant at instant speed, so a
//! green run is attributable to the static, not the harness.

use engine::game::engine::EngineError;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::game_state::CastPaymentMode;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard};
use engine::types::phase::Phase;
use engine::types::statics::StaticMode;
use engine::types::PlayerId;

const TEFERI_ORACLE: &str = "Each opponent can cast spells only any time they could cast a sorcery.\n\
     [+1]: Until your next turn, you may cast sorcery spells as though they had flash.\n\
     [\u{2212}3]: Return up to one target artifact, creature, or enchantment to its owner's hand. Draw a card.";

fn one_blue() -> ManaCost {
    ManaCost::Cost {
        shards: vec![ManaCostShard::Blue],
        generic: 0,
    }
}

/// Give `player` priority in a `Priority` window so `apply_as_current` routes
/// the next action AS that player (CR 117 priority — non-active players hold
/// priority during the active player's turn too).
fn grant_priority(runner: &mut GameRunner, player: PlayerId) {
    let state = runner.state_mut();
    state.priority_player = player;
    state.waiting_for = WaitingFor::Priority { player };
}

fn cast_instant(runner: &mut GameRunner, spell: ObjectId) -> Result<(), EngineError> {
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],

            payment_mode: CastPaymentMode::Auto,
        })
        .map(|_| ())
}

/// CR 101.2 + CR 307.5: With Teferi on P0's battlefield, P1 (an opponent) may
/// only cast at sorcery speed. During P0's turn with P1 holding priority, P1's
/// instant is BLOCKED — the production cast action returns the prohibition
/// error from `is_blocked_by_cant_cast_during`.
#[test]
fn opponent_instant_blocked_during_controllers_turn_with_teferi() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain); // active = P0, empty stack.

    // Real Teferi on P0's battlefield. The static line parses independently of
    // the planeswalker type; pin that the CantCastDuring static actually
    // attached so a green test can't pass vacuously on a no-static permanent.
    let mut teferi = scenario.add_creature(P0, "Teferi, Time Raveler", 0, 0);
    let teferi_id = teferi.from_oracle_text(TEFERI_ORACLE).id();

    // P1's instant + the blue mana to pay for it (so affordability is never the
    // gate — only the static restriction).
    scenario.add_basic_land(P1, ManaColor::Blue);
    let instant = scenario
        .add_spell_to_hand(P1, "Opposing Instant", true)
        .with_mana_cost(one_blue())
        .id();

    let mut runner = scenario.build();

    assert!(
        runner.state().objects[&teferi_id]
            .static_definitions
            .iter_unchecked()
            .any(|d| matches!(d.mode, StaticMode::CantCastDuring { .. })),
        "Teferi must contribute a CantCastDuring static, got: {:?}",
        runner.state().objects[&teferi_id].static_definitions
    );

    // P1 holds priority during P0's turn (legal per CR 117) and tries the cast.
    grant_priority(&mut runner, P1);
    let result = cast_instant(&mut runner, instant);

    assert!(
        matches!(result, Err(EngineError::ActionNotAllowed(_))),
        "Teferi must block an opponent's instant at instant speed during the \
         controller's turn (CR 101.2 + CR 307.5); got {result:?}"
    );
}

/// Fail-first baseline: WITHOUT Teferi, the identical opponent instant in the
/// identical timing window is castable. Pins that the harness itself does not
/// reject the cast — the block above is attributable to Teferi's static.
#[test]
fn opponent_instant_castable_during_controllers_turn_without_teferi() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain); // active = P0.

    scenario.add_basic_land(P1, ManaColor::Blue);
    let instant = scenario
        .add_spell_to_hand(P1, "Opposing Instant", true)
        .with_mana_cost(one_blue())
        .id();

    let mut runner = scenario.build();

    grant_priority(&mut runner, P1);
    let result = cast_instant(&mut runner, instant);

    assert!(
        result.is_ok(),
        "Without Teferi, an opponent's instant must be castable at instant \
         speed during the controller's turn; got {result:?}"
    );
}

/// CR 307.5: Teferi restricts opponents to SORCERY speed, not no-casting. On
/// P1's OWN turn (active = P1, P1's main phase, empty stack, P1 holding
/// priority) P1 CAN cast the instant — sorcery-speed timing is satisfied — even
/// with Teferi still on P0's battlefield.
#[test]
fn opponent_can_cast_at_sorcery_speed_on_their_own_turn_with_teferi() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let mut teferi = scenario.add_creature(P0, "Teferi, Time Raveler", 0, 0);
    teferi.from_oracle_text(TEFERI_ORACLE);

    scenario.add_basic_land(P1, ManaColor::Blue);
    let instant = scenario
        .add_spell_to_hand(P1, "Opposing Instant", true)
        .with_mana_cost(one_blue())
        .id();

    let mut runner = scenario.build();

    // Make it P1's turn: active = P1, P1's main phase, empty stack. NotSorcerySpeed
    // is satisfied (active player + main phase + empty stack), so the restriction
    // does not bite.
    {
        let state = runner.state_mut();
        state.active_player = P1;
    }
    grant_priority(&mut runner, P1);

    let result = cast_instant(&mut runner, instant);
    assert!(
        result.is_ok(),
        "Teferi restricts opponents to sorcery speed, not silence: P1 must be \
         able to cast on their own main phase with an empty stack; got {result:?}"
    );
}
