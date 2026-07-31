//! Issue #6463: the declare-attackers step must not surface the interactive
//! `WaitingFor::DeclareAttackers` prompt when the active player has zero
//! legal attackers (e.g. their only creature is tapped).
//!
//! CR 508.1a: 0 attackers is always a legal declaration, and the turn-based
//! action still runs even when nothing can be declared — only the
//! interactive prompt should be elided (mirroring how `DeclareBlockers`
//! already collapses when `valid_blocker_ids` is empty).
//!
//! The reproduction needs a begin-of-combat trigger, not just a tapped
//! creature: `Phase::BeginCombat`'s `has_potential_attackers` short-circuit
//! (which does correctly skip the whole combat phase when there's nothing to
//! do AND no begin-combat triggers) is bypassed whenever any begin-of-combat
//! trigger fires — once one exists, the engine unconditionally continues
//! into `Phase::DeclareAttackers` after the trigger resolves, regardless of
//! whether any creature can actually attack.
use engine::game::scenario::{GameScenario, P0};
use engine::types::ability::{
    AbilityDefinition, AbilityKind, Effect, QuantityExpr, TargetFilter, TriggerConstraint,
    TriggerDefinition,
};
use engine::types::actions::{DebugAction, GameAction};
use engine::types::game_state::WaitingFor;
use engine::types::phase::Phase;
use engine::types::triggers::TriggerMode;
use engine::types::zones::Zone;

#[test]
fn declare_attackers_prompt_skipped_when_no_legal_attackers_exist() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);

    // An unrelated "at the beginning of combat on your turn" trigger — this
    // is what causes the engine to enter Phase::DeclareAttackers at all
    // (see module doc). `GainLife` targets the controller, so it resolves
    // without any further player choice.
    let begin_combat_trigger = TriggerDefinition::new(TriggerMode::Phase)
        .phase(Phase::BeginCombat)
        .trigger_zones(vec![Zone::Battlefield])
        .constraint(TriggerConstraint::OnlyDuringYourTurn)
        .execute(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 1 },
                player: TargetFilter::Controller,
            },
        ));

    // The player's only creature — tapped, so it cannot legally attack.
    let elves = scenario
        .add_creature(P0, "Fyndhorn Elves", 1, 1)
        .with_trigger_definition(begin_combat_trigger)
        .id();

    let mut runner = scenario.build();
    runner.state_mut().debug_mode = true;
    runner
        .act(GameAction::Debug(DebugAction::SetTapped {
            object_id: elves,
            tapped: true,
        }))
        .expect("tapping the only creature should succeed");

    // Drive priority forward: PreCombatMain -> BeginCombat (trigger goes on
    // the stack) -> trigger resolves -> BeginCombat's empty stack advances
    // the phase. At every step, the engine must never stop on the
    // interactive DeclareAttackers prompt, since valid_attacker_ids is empty
    // throughout.
    for _ in 0..8 {
        if !matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
            || runner.state().phase == Phase::PostCombatMain
        {
            break;
        }
        let result = runner
            .act(GameAction::PassPriority)
            .expect("passing priority should always succeed here");
        assert!(
            !matches!(result.waiting_for, WaitingFor::DeclareAttackers { .. }),
            "declare-attackers prompt must be skipped when there are no legal \
             attackers, got {:?}",
            result.waiting_for
        );
    }

    assert_eq!(
        runner.state().phase,
        Phase::PostCombatMain,
        "with zero legal attackers the turn should sail through combat to \
         postcombat main without ever pausing on an attacker prompt"
    );
    assert!(
        runner.state().combat.is_none(),
        "combat should be cleared once the (empty) attacker declaration is \
         auto-submitted"
    );
    let p0_life = runner
        .state()
        .players
        .iter()
        .find(|p| p.id == P0)
        .expect("P0 exists")
        .life;
    assert_eq!(
        p0_life, 21,
        "the begin-of-combat trigger must still resolve even though the \
         prompt itself is skipped"
    );
}
