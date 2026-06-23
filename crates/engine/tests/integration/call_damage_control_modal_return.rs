//! Pipeline regression for **Call Damage Control** (MSH, green sorcery).
//!
//! Oracle:
//!   Choose up to two. Return those cards from your graveyard to your hand.
//!   • Target artifact card.
//!   • Target creature card.
//!   • Target enchantment card.
//!   • Target land card.
//!
//! The four bullet modes are bare targets; the shared return-to-hand effect is
//! phrased once in the header ("Return those cards from your graveyard to your
//! hand."). Before the fix, each mode lowered to
//! `Effect::Unimplemented { name: "target", description: "Target <type> card" }`
//! and the shared effect was dropped entirely — the spell did nothing.
//!
//! The fix distributes the header's shared `those <noun>` effect across every
//! bare-target mode (parser: `distribute_shared_mode_effect`), so each chosen
//! mode resolves an independent `Effect::Bounce` of a card of its card-type from
//! the controller's graveyard to hand, parameterized solely by card-type.
//!
//! CR 700.2 / CR 700.2a: modal spell; the controller chooses the mode(s) and an
//! illegal mode (no legal target) can't be chosen.
//! CR 700.2c / CR 601.2c: each chosen mode supplies its own single target.
//! CR 404.1: the graveyard is the source zone for the returned cards.

use engine::game::scenario::{GameScenario, P0};
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const CALL_DAMAGE_CONTROL: &str = "Choose up to two. \
    Return those cards from your graveyard to your hand.\n\
    \u{2022} Target artifact card.\n\
    \u{2022} Target creature card.\n\
    \u{2022} Target enchantment card.\n\
    \u{2022} Target land card.";

/// Stage a graveyard with one card of each of the four targetable card-types
/// plus the spell in hand. Returns `(artifact, creature, enchantment, land,
/// spell)` object ids.
fn staged_scenario() -> (
    GameScenario,
    engine::types::identifiers::ObjectId,
    engine::types::identifiers::ObjectId,
    engine::types::identifiers::ObjectId,
    engine::types::identifiers::ObjectId,
    engine::types::identifiers::ObjectId,
) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // `add_creature_to_graveyard` + `as_*` converts the core type while keeping
    // the card in the graveyard zone (CR 404.1).
    let artifact = scenario
        .add_creature_to_graveyard(P0, "Gy Artifact", 0, 0)
        .as_artifact()
        .id();
    let creature = scenario
        .add_creature_to_graveyard(P0, "Gy Creature", 2, 2)
        .id();
    let enchantment = scenario
        .add_creature_to_graveyard(P0, "Gy Enchantment", 0, 0)
        .as_enchantment()
        .id();
    let land = scenario
        .add_creature_to_graveyard(P0, "Gy Land", 0, 0)
        .as_land()
        .id();

    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Call Damage Control", false, CALL_DAMAGE_CONTROL)
        .with_mana_cost(ManaCost::generic(0))
        .id();

    (scenario, artifact, creature, enchantment, land, spell)
}

/// Revert-probe: the spell must parse to zero `Effect::Unimplemented`. With the
/// shared-effect distribution reverted, every mode is
/// `Effect::Unimplemented { name: "target" }` and this assertion fails.
#[test]
fn call_damage_control_modes_have_no_unimplemented() {
    use engine::types::ability::Effect;

    let (scenario, _artifact, _creature, _enchantment, _land, spell) = staged_scenario();
    let runner = scenario.build();
    let abilities = &runner.state().objects[&spell].abilities;
    assert_eq!(abilities.len(), 4, "four modes expected");
    for (i, ability) in abilities.iter().enumerate() {
        assert!(
            matches!(ability.effect.as_ref(), Effect::Bounce { .. }),
            "mode {i} must be a Bounce (return to hand), got {:?}",
            ability.effect
        );
    }
}

/// DISCRIMINATOR: choosing two modes returns exactly those two card-type
/// matching cards from the graveyard to hand; the unchosen types stay in the
/// graveyard. With the fix reverted the modes are no-ops and nothing moves.
#[test]
fn call_damage_control_two_modes_return_their_two_cards() {
    let (scenario, artifact, creature, enchantment, land, spell) = staged_scenario();
    let mut runner = scenario.build();

    let outcome = runner
        .cast(spell)
        // Modes 0 (artifact) and 1 (creature); per-mode targets in printed order.
        .modes(&[0, 1])
        .target_objects(&[artifact, creature])
        .resolve();

    assert_eq!(
        outcome.zone_of(artifact),
        Zone::Hand,
        "chosen artifact card must return to hand"
    );
    assert_eq!(
        outcome.zone_of(creature),
        Zone::Hand,
        "chosen creature card must return to hand"
    );
    // Negative control: unchosen card-types stay in the graveyard.
    assert_eq!(
        outcome.zone_of(enchantment),
        Zone::Graveyard,
        "unchosen enchantment card must stay in graveyard"
    );
    assert_eq!(
        outcome.zone_of(land),
        Zone::Graveyard,
        "unchosen land card must stay in graveyard"
    );
}

/// "Up to two" permits choosing a single mode (CR 700.2a / "choose up to N").
/// Exactly one card returns; the others stay put.
#[test]
fn call_damage_control_single_mode_is_legal() {
    let (scenario, artifact, creature, enchantment, land, spell) = staged_scenario();
    let mut runner = scenario.build();

    let outcome = runner
        .cast(spell)
        .modes(&[2]) // enchantment mode only
        .target_objects(&[enchantment])
        .resolve();

    assert_eq!(
        outcome.zone_of(enchantment),
        Zone::Hand,
        "the single chosen enchantment card must return to hand"
    );
    assert_eq!(outcome.zone_of(artifact), Zone::Graveyard);
    assert_eq!(outcome.zone_of(creature), Zone::Graveyard);
    assert_eq!(outcome.zone_of(land), Zone::Graveyard);
}

/// CR 700.2a: a mode with no legal target can't be chosen. With no land card in
/// any graveyard, the land mode (index 3) is unavailable and selecting it is
/// rejected.
#[test]
fn call_damage_control_land_mode_unavailable_without_land_card() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // Graveyard has an artifact and a creature, but NO land card.
    scenario
        .add_creature_to_graveyard(P0, "Gy Artifact", 0, 0)
        .as_artifact();
    scenario.add_creature_to_graveyard(P0, "Gy Creature", 2, 2);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Call Damage Control", false, CALL_DAMAGE_CONTROL)
        .with_mana_cost(ManaCost::generic(0))
        .id();

    let mut runner = scenario.build();
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("CastSpell must be accepted");

    for _ in 0..16 {
        match runner.state().waiting_for.clone() {
            WaitingFor::AbilityModeChoice {
                unavailable_modes, ..
            }
            | WaitingFor::ModeChoice {
                unavailable_modes, ..
            } => {
                // CR 700.2a: land mode (index 3) has no legal target -> unavailable.
                assert!(
                    unavailable_modes.contains(&3),
                    "land mode must be unavailable without a land card in the graveyard, \
                     got unavailable_modes={unavailable_modes:?}"
                );
                assert!(
                    runner
                        .act(GameAction::SelectModes { indices: vec![3] })
                        .is_err(),
                    "selecting the unavailable land mode must be rejected"
                );
                return;
            }
            WaitingFor::ManaPayment { .. } | WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("advance to mode choice");
            }
            other => panic!("unexpected waiting state before mode choice: {other:?}"),
        }
    }
    panic!("cast pipeline never reached the modal mode choice");
}
