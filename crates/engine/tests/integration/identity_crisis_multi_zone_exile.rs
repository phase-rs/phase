//! Identity Crisis — "Exile all cards from target player's hand and graveyard."
//!
//! CR 400.3 + CR 404.1 + CR 406.2 + CR 108.2: a single mass exile whose origin is a *union* of
//! the targeted player's hand and graveyard. The parser previously captured only
//! the first origin ("hand") via `infer_origin_zone` and orphaned "and
//! graveyard" as an unsupported child clause, so the graveyard cards were never
//! exiled.
//!
//! This drives the full cast -> resolve pipeline and asserts BOTH the targeted
//! player's hand AND graveyard cards land in exile, while a wrong-player card
//! (the caster's own hand) and a wrong-zone card (the target's battlefield
//! permanent) are untouched. Reverting the parser fix (which drops the graveyard
//! origin, exiling only the hand) makes the graveyard assertion fail — so this
//! test discriminates the fix rather than merely asserting parsed AST shape.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const IDENTITY_CRISIS: &str = "Exile all cards from target player's hand and graveyard.";

#[test]
fn identity_crisis_exiles_target_hand_and_graveyard() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // The targeted player's hand + graveyard — every card here must leave for exile.
    let p1_hand = scenario.add_card_to_hand(P1, "Ancestral Vision");
    let p1_grave_creature = scenario
        .add_creature_to_graveyard(P1, "Grizzly Bears", 2, 2)
        .id();
    let p1_grave_spell = scenario
        .add_spell_to_graveyard(P1, "Lightning Bolt", true)
        .id();

    // Control cards that MUST survive: the caster's own hand (wrong player) and a
    // battlefield permanent controlled by the target (wrong zone).
    let p0_hand = scenario.add_card_to_hand(P0, "Island");
    let p1_battlefield = scenario.add_creature(P1, "Llanowar Elves", 1, 1).id();

    let identity_crisis = scenario
        .add_spell_to_hand_from_oracle(P0, "Identity Crisis", false, IDENTITY_CRISIS)
        .id();

    let mut runner = scenario.build();
    let card_id = runner.state().objects[&identity_crisis].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: identity_crisis,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast Identity Crisis");

    // Identity Crisis targets a player — answer the target-selection prompt with P1.
    match runner.state().waiting_for.clone() {
        WaitingFor::TargetSelection { .. } => {
            runner
                .act(GameAction::SelectTargets {
                    targets: vec![TargetRef::Player(P1)],
                })
                .expect("targeting the opponent (P1) must succeed");
        }
        other => {
            panic!("expected a TargetSelection prompt after casting Identity Crisis, got {other:?}")
        }
    }
    runner.advance_until_stack_empty();

    // Both origin zones of the targeted player are emptied into exile.
    assert_eq!(
        runner.state().objects[&p1_hand].zone,
        Zone::Exile,
        "target player's hand card must be exiled",
    );
    assert_eq!(
        runner.state().objects[&p1_grave_creature].zone,
        Zone::Exile,
        "target player's graveyard creature must be exiled — the multi-zone union; \
         this fails if the parser drops the graveyard origin",
    );
    assert_eq!(
        runner.state().objects[&p1_grave_spell].zone,
        Zone::Exile,
        "target player's graveyard spell must be exiled",
    );

    // Discriminators: wrong player / wrong zone are untouched.
    assert_eq!(
        runner.state().objects[&p0_hand].zone,
        Zone::Hand,
        "the caster's own hand must be untouched (the effect is scoped to the target player)",
    );
    assert_eq!(
        runner.state().objects[&p1_battlefield].zone,
        Zone::Battlefield,
        "the target player's battlefield permanent must be untouched (hand + graveyard only)",
    );
}
