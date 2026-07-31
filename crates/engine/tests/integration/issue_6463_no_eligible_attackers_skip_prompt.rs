//! Issue #6463: the declare-attackers step must not surface the interactive
//! `WaitingFor::DeclareAttackers` prompt when the active player has no legal
//! attack declaration available.
//!
//! CR 508.1a: 0 attackers is always a legal declaration, and the turn-based
//! action still runs even when nothing can be declared — only the
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
//!   is empty — e.g. the only opponent has protection from everything. The
//!   aggregate `valid_attack_targets` is empty even though
//!   `valid_attacker_ids` is not, and `has_potential_attackers` never checks
//!   target legality, so this reaches `Phase::DeclareAttackers` through the
//!   ordinary (trigger-free) path.
use engine::game::scenario::{GameScenario, P0, P1};
use engine::game::zones::create_object;
use engine::types::ability::{
    AbilityDefinition, AbilityKind, ContinuousModification, Duration, Effect, QuantityExpr,
    TargetFilter, TriggerConstraint, TriggerDefinition,
};
use engine::types::actions::{DebugAction, GameAction};
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::CardId;
use engine::types::keywords::{Keyword, ProtectionTarget};
use engine::types::phase::Phase;
use engine::types::triggers::TriggerMode;
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
/// every candidate's legal-target list is still empty — e.g. the sole
/// opponent has protection from everything. Checking only
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
    scenario.add_creature(P0, "Fyndhorn Elves", 1, 1);

    let mut runner = scenario.build();

    // CR 702.16e-style protection from everything on the only opponent
    // (Teferi's Protection), for the rest of the turn. `get_valid_attack_targets`
    // already excludes protected players (see
    // `get_valid_attack_targets_excludes_protected_player` in combat.rs), so
    // with a single opponent this empties the aggregate `valid_attack_targets`
    // even though `valid_attacker_ids` stays non-empty.
    let source = create_object(
        runner.state_mut(),
        CardId(999),
        P1,
        "Teferi's Protection".to_string(),
        Zone::Battlefield,
    );
    runner.state_mut().add_transient_continuous_effect(
        source,
        P1,
        Duration::UntilEndOfTurn,
        TargetFilter::SpecificPlayer { id: P1 },
        vec![ContinuousModification::AddKeyword {
            keyword: Keyword::Protection(ProtectionTarget::Everything),
        }],
        None,
    );

    assert_never_prompts_and_reaches_postcombat_main(&mut runner);
}
