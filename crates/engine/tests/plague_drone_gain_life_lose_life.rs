//! Plague Drone — "If an opponent would gain life, that player loses that much life instead." (issue #20404)
//!
//! CR 119.10 + CR 614.6: this is a lifegain-negation and conversion *replacement* effect
//! that applies only to opponents of the permanent's controller.
//!
//! Discriminating end-to-end:
//! 1. With Plague Drone on P0's battlefield, P1 (opponent) casting a "You gain 3 life" spell
//!    must LOSE 3 life instead (leaving them with -3 from before).
//! 2. P0 (controller) casting a "You gain 3 life" spell must gain 3 life normally.
//! 3. Without Plague Drone in play, the same spell gains life normally.
//!
//! A fourth test asserts the parsed AST shape directly (parser-level coverage,
//! not just runtime behavior) — see `plague_drone_parses_as_gain_life_replacement`.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{Effect, TargetFilter};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::replacements::ReplacementEvent;
use engine::types::Phase;
use engine::types::PlayerId;

const PLAGUE_DRONE: &str =
    "Flying\nRot Fly — If an opponent would gain life, that player loses that much life instead.";

fn card_id_of(runner: &GameRunner, id: ObjectId) -> CardId {
    runner.state().objects.get(&id).unwrap().card_id
}

/// Cast a spell from hand and drive the pipeline to stack-empty.
fn cast_and_resolve(runner: &mut GameRunner, caster: PlayerId, spell: ObjectId) {
    let card_id = card_id_of(runner, spell);
    if caster == P1 {
        // Pass priority from P0 to P1 so P1 can cast the instant spell
        runner
            .act(GameAction::PassPriority)
            .expect("pass priority to P1");
    }
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: Default::default(),
        })
        .expect("cast gain-life spell");
    for _ in 0..40 {
        if !matches!(runner.state().waiting_for, WaitingFor::Priority { .. }) {
            break;
        }
        if runner.state().stack.is_empty() || runner.act(GameAction::PassPriority).is_err() {
            break;
        }
    }
}

#[test]
fn plague_drone_converts_opponent_lifegain_to_life_loss() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // P0 controls Plague Drone
    scenario.add_creature_from_oracle(P0, "Plague Drone", 3, 3, PLAGUE_DRONE);
    // P1 (opponent of P0) has a gain-life instant spell in hand
    let spell = scenario
        .add_spell_to_hand_from_oracle(P1, "Test Lifegain", true, "You gain 3 life.")
        .id();

    let mut runner = scenario.build();
    let before = runner.life(P1);

    cast_and_resolve(&mut runner, P1, spell);

    assert_eq!(
        runner.life(P1),
        before - 3,
        "CR 119.10 + CR 614.6: Plague Drone must convert opponent's life gain to life loss"
    );
}

#[test]
fn plague_drone_does_not_affect_controller_lifegain() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // P0 controls Plague Drone
    scenario.add_creature_from_oracle(P0, "Plague Drone", 3, 3, PLAGUE_DRONE);
    // P0 (controller of Plague Drone) has a gain-life sorcery/instant spell in hand
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Test Lifegain", false, "You gain 3 life.")
        .id();

    let mut runner = scenario.build();
    let before = runner.life(P0);

    cast_and_resolve(&mut runner, P0, spell);

    assert_eq!(
        runner.life(P0),
        before + 3,
        "Plague Drone must not affect its controller's life gain"
    );
}

#[test]
fn without_drone_opponent_gains_life_normally() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // P1 has a gain-life instant spell in hand
    let spell = scenario
        .add_spell_to_hand_from_oracle(P1, "Test Lifegain", true, "You gain 3 life.")
        .id();

    let mut runner = scenario.build();
    let before = runner.life(P1);

    cast_and_resolve(&mut runner, P1, spell);

    assert_eq!(
        runner.life(P1),
        before + 3,
        "without Plague Drone, the opponent must gain life normally"
    );
}

/// Parser-level coverage: pin down the parsed AST shape for Plague Drone's
/// replacement rather than only exercising it through runtime behavior, so a
/// refactor that silently changes representation (e.g. regressing the
/// life-loss body to an `Unimplemented` no-op that another code path papers
/// over) is caught at the parser level first.
#[test]
fn plague_drone_parses_as_gain_life_replacement() {
    let parsed = parse_oracle_text(
        PLAGUE_DRONE,
        "Plague Drone",
        &[],
        &["Creature".to_string()],
        &["Phyrexian".to_string(), "Insect".to_string()],
    );

    assert_eq!(
        parsed.replacements.len(),
        1,
        "Plague Drone must parse into exactly one replacement definition; got: {:?}",
        parsed.replacements
    );

    assert!(
        matches!(parsed.replacements[0].event, ReplacementEvent::GainLife),
        "Plague Drone's replacement must be keyed on the GainLife event; got: {:?}",
        parsed.replacements[0].event
    );

    let execute = parsed.replacements[0]
        .execute
        .as_deref()
        .expect("Plague Drone's replacement must carry an execute body");
    assert!(
        matches!(
            &*execute.effect,
            Effect::LoseLife {
                target: Some(TargetFilter::PostReplacementDamageTarget),
                ..
            }
        ),
        "the life-loss recipient must be the explicit post-replacement \
         event-recipient filter bound at the parser seam; got: {:?}",
        execute.effect
    );

    // Scope the serialized-shape assertions to the replacement itself: the
    // keyword line ("Flying") legitimately lowers to an Unimplemented ability
    // when no MTGJSON keyword list is supplied to the parser, and this test
    // pins the replacement's shape, not keyword handling.
    let json = serde_json::to_string(&parsed.replacements).expect("replacements must serialize");

    assert!(
        json.contains("GainLife"),
        "serialized replacement must contain the GainLife event marker; json: {json}"
    );
    assert!(
        json.contains("LoseLife"),
        "the replacement's execute body must lower to a structured life-loss \
         effect (CR 614.6 conversion), not an opaque fallback; json: {json}"
    );
    assert!(
        json.contains("\"valid_player\":\"Opponent\""),
        "the replacement must be scoped to opponents of the controller; json: {json}"
    );
    assert!(
        !json.contains("Unimplemented"),
        "the replacement must not fall back to an Unimplemented effect; json: {json}"
    );
}
