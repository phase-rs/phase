//! GitHub issue #403 — Wedding Ring (defect 1: ETB token-copy controller).
//!
//! Oracle (line 1): "When this artifact enters, if it was cast, target
//! opponent creates a token that's a copy of it."
//!
//! `Effect::CopyTokenOf` originally had no controller/owner channel, so the
//! copy token was always created under the trigger controller's control. The
//! fix adds an `owner: TargetFilter` field (mirroring `Effect::Token.owner`)
//! and — critically — surfaces it as a stack-push *target slot* via
//! `Effect::target_filter()` so the player is actually prompted to (or
//! auto-)choose the opponent. A `token_copy::resolve`-level test cannot
//! exercise slot collection; this test drives the real cast -> stack -> ETB
//! trigger -> target selection -> resolve pipeline through `apply`.
//!
//! It also exercises the "if it was cast" intervening-if: the parser now emits
//! `TriggerCondition::WasCast`, and `cast_from_zone` survives onto the
//! battlefield permanent so the CR 603.4 resolution re-check passes — while
//! the created copy *token* (never cast) does NOT re-trigger, closing the
//! reported "stack kept looping" loop.
//!
//! CR references (verified against docs/MagicCompRules.txt):
//!   - CR 109.4: only objects on the stack or battlefield have a controller;
//!     the player an effect instructs to create a token controls it.
//!   - CR 111.2: the player who creates a token is its owner.
//!   - CR 707.2: a token that's a copy of an object copies its copiable values.
//!   - CR 603.4: an intervening-if condition is re-checked when the triggered
//!     ability resolves.

use engine::game::scenario::{CastOutcome, GameScenario, P0, P1};
use engine::types::card_type::CoreType;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const WEDDING_RING_ETB: &str =
    "When this artifact enters, if it was cast, target opponent creates a token \
     that's a copy of it.";

/// Build a pure-Artifact card in P0's hand carrying the ETB Oracle line.
///
/// `add_creature_to_hand_from_oracle` stamps the `Creature` core type;
/// `as_artifact()` strips `Creature` and pushes `Artifact`, yielding a pure
/// Artifact. An Artifact spell resolves onto the battlefield (so the ETB
/// trigger fires) — a sorcery would go to the graveyard instead.
fn artifact_in_hand(scenario: &mut GameScenario, name: &str, oracle: &str) -> ObjectId {
    scenario
        .add_creature_to_hand_from_oracle(P0, name, 0, 0, oracle)
        .as_artifact()
        .id()
}

/// Cast the artifact (P0's hand) through the real pipeline: the spell resolves,
/// the ETB trigger fires, the opponent (P1) is chosen as the token creator, and
/// the trigger resolves. Returns the cast outcome.
fn cast_and_resolve(scenario: GameScenario, artifact: ObjectId) -> CastOutcome {
    let mut runner = scenario.build();
    // The "target opponent creates a token" ETB surfaces a
    // TriggerTargetSelection slot during resolution; declaring P1 routes the
    // copy token under the opponent (CR 109.4).
    runner.cast(artifact).target_player(P1).resolve()
}

/// Core defect-1 fix: casting Wedding Ring fires the ETB trigger and the copy
/// token is created under the OPPONENT's control — not P0's (the trigger
/// controller's) — and the stack does not loop.
#[test]
fn wedding_ring_etb_token_copy_is_opponent_controlled() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let wedding_ring = artifact_in_hand(&mut scenario, "Wedding Ring", WEDDING_RING_ETB);

    let outcome = cast_and_resolve(scenario, wedding_ring);

    // (a) Wedding Ring itself resolved onto the battlefield under P0.
    let ring = outcome
        .state()
        .objects
        .get(&wedding_ring)
        .expect("ring present");
    assert_eq!(ring.zone, Zone::Battlefield);
    assert_eq!(ring.controller, P0);

    // (b)+(c) Exactly one copy token, a copy of Wedding Ring, opponent-controlled.
    let copy_tokens: Vec<_> = outcome
        .state()
        .objects
        .values()
        .filter(|o| o.is_token && o.id != wedding_ring)
        .collect();
    assert_eq!(
        copy_tokens.len(),
        1,
        "exactly one copy token must be created, found {}",
        copy_tokens.len()
    );
    let token = copy_tokens[0];
    assert_eq!(
        token.name, "Wedding Ring",
        "the token must be a copy of Wedding Ring (CR 707.2), not a player"
    );
    assert!(token.card_types.core_types.contains(&CoreType::Artifact));
    assert_eq!(
        token.controller, P1,
        "CR 109.4: the copy token must be controlled by the chosen opponent (P1), \
         not by Wedding Ring's controller (P0)"
    );
    assert_eq!(
        token.owner, P1,
        "CR 111.2: the creating player owns the token"
    );

    // (d) The stack is empty afterward — regression guard for the reported
    //     "stack kept looping repeatedly" symptom. The copy token is not cast,
    //     so its own `WasCast`-gated ETB trigger does not fire.
    outcome.assert_stack_size(0);
}

/// Generalization guard for defect 1: any "target opponent creates a token
/// that's a copy of ~" card routes the copy under the chosen opponent. Driven
/// with a synthetic non-Wedding-Ring card so the fix is verified as a class
/// fix, not a Wedding-Ring special case.
#[test]
fn target_opponent_copy_token_generalizes_beyond_wedding_ring() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    const SYNTH_ETB: &str = "When this artifact enters, if it was cast, target opponent creates a \
         token that's a copy of it.";
    let synth = artifact_in_hand(&mut scenario, "Synthetic Copier", SYNTH_ETB);

    let outcome = cast_and_resolve(scenario, synth);

    let copy_tokens: Vec<_> = outcome
        .state()
        .objects
        .values()
        .filter(|o| o.is_token && o.id != synth)
        .collect();
    assert_eq!(copy_tokens.len(), 1, "exactly one copy token expected");
    assert_eq!(
        copy_tokens[0].controller,
        PlayerId(1),
        "the synthetic card's copy token must also be opponent-controlled — \
         the fix is a class fix, not a Wedding-Ring special case"
    );
    outcome.assert_stack_size(0);
}
