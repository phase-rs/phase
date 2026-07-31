//! Issue #6463: the declare-attackers step must not surface the interactive
//! `WaitingFor::DeclareAttackers` prompt when the active player has no legal
//! attack declaration available.
//!
//! CR 508.1a/d: When no nonempty declaration is legal, declaring zero attackers
//! is legal and the turn-based action still runs — only the
//! interactive prompt should be elided (mirroring how `DeclareBlockers`
//! already collapses when `valid_blocker_ids` is empty).
//!
//! There are two independent ways to end up with no legal declaration, and
//! both are covered here:
//!
//! - `valid_attacker_ids` (the creature-level candidate set) is empty — e.g.
//!   the only creature is tapped. Reaching `Phase::DeclareAttackers` at all
//!   in that case needs a begin-of-combat trigger: `Phase::BeginCombat`'s
//!   `has_potential_attackers` short-circuit (which does correctly skip the
//!   whole combat phase when there's nothing to do AND no begin-combat
//!   triggers) is bypassed whenever any begin-of-combat trigger fires — once
//!   one exists, the engine unconditionally continues into
//!   `Phase::DeclareAttackers` after the trigger resolves, regardless of
//!   whether any creature can actually attack.
//! - `valid_attacker_ids` is non-empty (an untapped, non-sick creature is a
//!   valid *candidate*) but every candidate's per-attacker legal-target list
//!   is empty — e.g. a CR 508.1c temporary attack prohibition bars attacking
//!   the only opponent. The aggregate `valid_attack_targets` is empty even
//!   though `valid_attacker_ids` is not, and `has_potential_attackers` never
//!   checks target legality, so this reaches `Phase::DeclareAttackers`
//!   through the ordinary (trigger-free) path.
use engine::game::combat::build_declare_attackers_waiting_for;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::ability::{
    AbilityDefinition, AbilityKind, Effect, GameRestriction, ProhibitedActivity, QuantityExpr,
    RestrictionExpiry, RestrictionPlayerScope, TargetFilter, TriggerConstraint, TriggerDefinition,
};
use engine::types::actions::{DebugAction, GameAction};
use engine::types::game_state::WaitingFor;
use engine::types::phase::Phase;
use engine::types::triggers::{AttackTargetFilter, TriggerMode};
use engine::types::zones::Zone;

/// Drive priority forward from precombat main until either the interactive
/// declare-attackers prompt would appear (asserted against on every step) or
/// the game reaches postcombat main. Shared by both reproductions below.
fn assert_never_prompts_and_reaches_postcombat_main(
    runner: &mut engine::game::scenario::GameRunner,
) {
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
            "declare-attackers prompt must be skipped when there is no legal \
             attack declaration, got {:?}",
            result.waiting_for
        );
    }

    assert_eq!(
        runner.state().phase,
        Phase::PostCombatMain,
        "with no legal attack declaration the turn should sail through combat \
         to postcombat main without ever pausing on an attacker prompt"
    );
    assert!(
        runner.state().combat.is_none(),
        "combat should be cleared once the (empty) attacker declaration is \
         auto-submitted"
    );
}

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
    assert_never_prompts_and_reaches_postcombat_main(&mut runner);

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

/// The candidate set can be non-empty (an untapped, non-sick creature) while
/// every candidate's legal-target list is still empty — e.g. a temporary
/// attack prohibition bars attacking the sole opponent. Checking only
/// `valid_attacker_ids` misses this: it stays non-empty even though no
/// non-empty attack declaration is actually possible. The engine must check
/// the aggregate `valid_attack_targets` instead.
#[test]
fn declare_attackers_prompt_skipped_when_every_candidate_has_no_legal_target() {
    let mut scenario = GameScenario::new_n_player(2, 7);
    scenario.at_phase(Phase::PreCombatMain);

    // Untapped, non-summoning-sick creature: a normal candidate attacker.
    // `has_potential_attackers` (the coarse BeginCombat gate) never checks
    // per-target legality, so this reaches Phase::DeclareAttackers through
    // the ordinary trigger-free path — no begin-of-combat trigger needed.
    let source = scenario.add_creature(P0, "Fyndhorn Elves", 1, 1).id();

    let mut runner = scenario.build();

    // CR 508.1c + CR 109.5: a temporary attack prohibition ("players can't
    // attack P1 this turn", a `ProhibitActivity::Attack` restriction) bars
    // declaring an attack against the only opponent. This is a per-target
    // hard restriction consulted inside `attacker_can_attack_target`
    // (`attack_passes_temporary_prohibition`), NOT a candidate-level gate —
    // `creature_cant_attack_gated` (which builds `valid_attacker_ids`) never
    // consults `state.restrictions` — so the creature remains a valid
    // candidate while its legal-target list becomes empty. With only one
    // opponent, that empties the aggregate `valid_attack_targets` too. See
    // `temporary_attack_prohibition_bars_only_the_protected_player` in
    // rules/combat.rs for the same restriction exercised directly against
    // `DeclareAttackers` validation.
    runner
        .state_mut()
        .restrictions
        .push(GameRestriction::ProhibitActivity {
            source,
            affected_players: RestrictionPlayerScope::AllPlayers,
            expiry: RestrictionExpiry::EndOfTurn,
            activity: ProhibitedActivity::Attack {
                defended: AttackTargetFilter::PlayerOrPlaneswalker,
                protected_player: Some(P1),
            },
        });

    // Precondition: the candidate set is non-empty but the aggregate legal-
    // target set is empty — the exact split the fix must detect.
    match build_declare_attackers_waiting_for(runner.state()) {
        WaitingFor::DeclareAttackers {
            valid_attacker_ids,
            valid_attack_targets,
            ..
        } => {
            assert!(
                !valid_attacker_ids.is_empty(),
                "precondition: the creature must remain a valid candidate \
                 despite the attack prohibition"
            );
            assert!(
                valid_attack_targets.is_empty(),
                "precondition: the only opponent is barred, so no legal \
                 attack target remains"
            );
        }
        other => panic!("expected DeclareAttackers, got {other:?}"),
    }

    assert_never_prompts_and_reaches_postcombat_main(&mut runner);
}
