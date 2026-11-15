//! Surface parser near-miss: a SELF-reflexive library tuck — "you may put it on
//! the top or bottom of its owner's library" (Arashin Sovereign's
//! dies trigger) — was not routed to the existing `Effect::PutOnTopOrBottom`.
//!
//! The owner-framing patterns in `try_parse_put_on_top_or_bottom`
//! (`oracle_effect/mod.rs`) recognize "its owner puts it on their choice of the
//! top or bottom of their library" (Aether Gust, Subtlety, Aetherspouts). The
//! controller-framing self form ("you may put it on the top or
//! bottom of its owner's library") had no arm, so the effect body lowered to
//! `Effect::Unimplemented` and nothing happened at resolution — Arashin stayed
//! in the graveyard, and the whole card rendered unsupported.
//!
//! The self form carries `chooser: Controller`; owner-framed forms use
//! `ParentTargetOwner`. A non-self "it" (S.N.E.A.K. Dispatcher) or a "that card"
//! subject (Hinder) remains excluded, so it cannot be assigned the wrong chooser.
//!
//! This test drives the REAL dies-trigger -> resolve -> ChooseTopOrBottom
//! pipeline and FAILS on `main`: the clause parses to `Unimplemented`, so no
//! `TopOrBottomChoice` is ever offered and Arashin never leaves the graveyard.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::game::triggers::process_triggers;
use engine::types::actions::GameAction;
use engine::types::zones::Zone;

// Arashin Sovereign's real Oracle text (verified against card-data.json).
const ARASHIN_SOVEREIGN: &str =
    "Flying\nWhen this creature dies, you may put it on the top or bottom of its owner's library.";

/// After Arashin Sovereign dies and its optional dies trigger resolves with the
/// controller choosing "top", the creature card must be on TOP of its owner's
/// library. This setup separates the controller (P0) from owner (P1), proving
/// both that P0 receives the choice and that P1 receives the card.
#[test]
fn stolen_arashin_sovereign_controller_chooses_owner_library_position() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(engine::types::phase::Phase::PreCombatMain);

    // Two filler cards already on top, so "top" placement is observable: after the
    // tuck Arashin must sit at library[0], ABOVE the fillers (proves the chosen
    // position, not merely that the card landed somewhere in the library).
    scenario.with_library_top(P1, &["Filler A", "Filler B"]);

    let arashin = scenario
        .add_creature_from_oracle(P1, "Arashin Sovereign", 5, 7, ARASHIN_SOVEREIGN)
        .id();

    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&arashin)
        .unwrap()
        .controller = P0;

    // Kill Arashin: move it to the graveyard as a game event so the dies trigger
    // observes the death (mirrors issue_1332_bronzehide_lion).
    let mut events = Vec::new();
    engine::game::zones::move_to_zone(runner.state_mut(), arashin, Zone::Graveyard, &mut events);
    process_triggers(runner.state_mut(), &events);

    assert_eq!(
        runner.state().stack.len(),
        1,
        "Arashin's dies trigger must be on the stack"
    );

    // Drive resolution: pass priority to resolve the trigger, accept the optional
    // "you may", then choose the TOP of the library.
    let mut guard = 0;
    let reached_choice = loop {
        guard += 1;
        assert!(
            guard < 64,
            "resolution exceeded safety bound; waiting_for = {} stack = {}",
            runner.waiting_for_kind(),
            runner.state().stack.len()
        );
        match runner.waiting_for_kind() {
            "OptionalEffectChoice" => {
                runner
                    .act(GameAction::DecideOptionalEffect { accept: true })
                    .expect("accept the optional dies trigger");
            }
            "TopOrBottomChoice" => {
                assert!(matches!(
                    &runner.state().waiting_for,
                    engine::types::game_state::WaitingFor::TopOrBottomChoice { player, .. }
                        if *player == P0
                ));
                runner
                    .act(GameAction::ChooseTopOrBottom { top: true })
                    .expect("choose top of library");
                break true;
            }
            "Priority" => {
                if runner.state().stack.is_empty() {
                    break false;
                }
                if runner.act(GameAction::PassPriority).is_err() {
                    break false;
                }
            }
            _ => break false,
        }
    };

    assert!(
        reached_choice,
        "the self-tuck must reach a TopOrBottomChoice; on `main` the clause is \
         Unimplemented so no choice is offered and Arashin never leaves the graveyard"
    );

    // End-to-end runtime delta: P0 made the choice, while Arashin is now the TOP card of P1's
    // library, not in the graveyard. On `main` it is still in the graveyard.
    let arashin_obj = &runner.state().objects[&arashin];
    assert_eq!(
        arashin_obj.zone,
        Zone::Library,
        "Arashin must be in its owner's library after the self-tuck resolves; got {:?}",
        arashin_obj.zone
    );
    assert_eq!(
        runner.state().players[P1.0 as usize]
            .library
            .iter()
            .next()
            .copied(),
        Some(arashin),
        "Arashin must be on TOP of P1's library (the chosen position)"
    );
}
