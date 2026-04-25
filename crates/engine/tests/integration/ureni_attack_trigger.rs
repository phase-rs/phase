//! Regression tests for the "Whenever ~ enters or attacks" trigger pattern
//! (L4-11 — Ureni of the Unwritten attack-trigger intermittent failure).
//!
//! Ureni Oracle: "Whenever Ureni enters or attacks, look at the top eight cards
//! of your library. You may put a Dragon creature card from among them onto the
//! battlefield. ..."
//!
//! The user reported that the attack side "sometimes" fails to fire. These tests
//! pin the invariants that must hold for `TriggerMode::EntersOrAttacks` with
//! `valid_card: SelfRef`:
//!   1. Solo attack puts the trigger on the stack.
//!   2. Batched attacker declaration (Ureni alongside other creatures, at various
//!      positions within `attacker_ids`) still fires the trigger.
//!   3. Attacking in a second combat phase (CR 506.4) fires a fresh trigger.
//!   4. A `SuppressTriggers` static scoped to ETB events does NOT suppress the
//!      attack branch (CR 603.2g only filters ETB/Dies events).
//!
//! The trigger effect is `GainLife { amount: 1, player: Controller }` so each
//! fire is observable as a 1-life delta on P0. We use `GainLife` rather than
//! `Dig` to keep the test scoped to trigger-registration behavior and avoid
//! mid-resolution `WaitingFor::ChooseFromZone` noise.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::ability::{
    AbilityDefinition, AbilityKind, Effect, GainLifePlayer, QuantityExpr, StaticDefinition,
    TargetFilter, TriggerDefinition, TypedFilter,
};
use engine::types::actions::GameAction;
use engine::types::game_state::ExtraPhase;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::statics::{StaticMode, SuppressedTriggerEvent};
use engine::types::triggers::TriggerMode;
use engine::types::zones::Zone;

use super::rules::{run_combat, AttackTarget, WaitingFor};

/// Build the Ureni-style trigger: `TriggerMode::EntersOrAttacks` with
/// `valid_card: SelfRef` and a `GainLife(1)` effect (one life per fire).
fn ureni_style_trigger() -> TriggerDefinition {
    TriggerDefinition::new(TriggerMode::EntersOrAttacks)
        .execute(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 1 },
                player: GainLifePlayer::Controller,
            },
        ))
        .valid_card(TargetFilter::SelfRef)
        .trigger_zones(vec![Zone::Battlefield])
}

/// Add a creature with the Ureni attack trigger. `haste` is granted via
/// `entered_battlefield_turn = None` is NOT needed because scenarios start on
/// turn 1 and `add_creature` sets entered_turn = turn_number.saturating_sub(1),
/// i.e. turn 0 — so summoning sickness is NOT active and the creature may
/// attack on turn 1. The `vigilance` flag controls whether the creature can
/// attack in a later combat without being manually untapped.
fn add_ureni(scenario: &mut GameScenario, vigilance: bool) -> ObjectId {
    let mut b = scenario.add_creature(P0, "Ureni", 7, 6);
    b.with_subtypes(vec!["Elemental"]);
    b.with_trigger_definition(ureni_style_trigger());
    if vigilance {
        b.vigilance();
    }
    b.id()
}

/// CR 508.1a + CR 603.2: Solo attack — Ureni is the only attacker, the trigger
/// must fire exactly once.
#[test]
fn ureni_solo_attack_fires_trigger() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let ureni = add_ureni(&mut scenario, false);
    let mut runner = scenario.build();

    let life_before = runner.life(P0);
    run_combat(&mut runner, vec![ureni], vec![]);
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.life(P0),
        life_before + 1,
        "Ureni solo attack must place exactly one trigger on the stack (GainLife 1)"
    );
}

/// CR 508.1a + CR 603.2: Batched attackers — Ureni is declared alongside other
/// creatures. The `matching_attack_events` loop must pick Ureni out of
/// `attacker_ids` regardless of position. We test first, middle, and last
/// positions to guard against any ordering-dependent matcher bug.
#[test]
fn ureni_batched_with_others_fires_once_per_position() {
    for position in 0..3 {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let ureni = add_ureni(&mut scenario, false);
        let other1 = scenario.add_creature(P0, "Grizzly Bear 1", 2, 2).id();
        let other2 = scenario.add_creature(P0, "Grizzly Bear 2", 2, 2).id();
        let mut runner = scenario.build();

        let attackers: Vec<ObjectId> = match position {
            0 => vec![ureni, other1, other2],
            1 => vec![other1, ureni, other2],
            2 => vec![other1, other2, ureni],
            _ => unreachable!(),
        };

        let life_before = runner.life(P0);
        run_combat(&mut runner, attackers, vec![]);
        runner.advance_until_stack_empty();

        assert_eq!(
            runner.life(P0),
            life_before + 1,
            "Ureni at attacker position {position} must still fire exactly once \
             (batched AttackersDeclared event must match SelfRef on any index)"
        );
    }
}

/// CR 506.4 + CR 500.8: Each declaration of attackers is its own event. When a
/// creature attacks in a second combat phase (e.g., via Moraug / Aggravated
/// Assault pushing an extra `BeginCombat`), a fresh `AttackersDeclared` event
/// must fire the trigger again.
#[test]
fn ureni_attacks_in_second_combat_fires_again() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // Vigilance so Ureni is still untapped for the second declaration.
    let ureni = add_ureni(&mut scenario, true);
    let mut runner = scenario.build();

    let life_before = runner.life(P0);

    // First combat.
    run_combat(&mut runner, vec![ureni], vec![]);
    runner.advance_until_stack_empty();
    assert_eq!(
        runner.life(P0),
        life_before + 1,
        "first combat attack must fire the trigger"
    );

    // CR 500.8: Schedule an extra combat phase (simulates Aggravated Assault /
    // Moraug). Anchor to the current phase so the very next `advance_phase`
    // consumes it — this is the test-harness equivalent of the in-game flow
    // where the trigger resolver pushes with anchor = `EndCombat` and the
    // engine then advances out of `EndCombat` into the extra `BeginCombat`.
    let current_phase = runner.state().phase;
    runner.state_mut().extra_phases.push(ExtraPhase {
        anchor: current_phase,
        phase: Phase::BeginCombat,
    });

    // Advance out of the current step (post-combat / end phase) into the
    // extra BeginCombat. Passing priority repeatedly drives the phase machine.
    for _ in 0..40 {
        if runner.state().phase == Phase::DeclareAttackers
            && matches!(
                runner.state().waiting_for,
                WaitingFor::DeclareAttackers { .. }
            )
        {
            break;
        }
        if runner.act(GameAction::PassPriority).is_err() {
            break;
        }
    }

    assert_eq!(
        runner.state().phase,
        Phase::DeclareAttackers,
        "extra combat phase must have reached DeclareAttackers"
    );
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::DeclareAttackers { .. }
        ),
        "engine must be waiting for attacker declaration in the extra combat"
    );

    // Declare Ureni as attacker again.
    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![(ureni, AttackTarget::Player(P1))],
        })
        .expect("second DeclareAttackers should succeed");
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.life(P0),
        life_before + 2,
        "CR 506.4: attacking in a second combat must fire the EntersOrAttacks \
         trigger a second time (total +2 life: one per combat)"
    );
}

/// CR 603.2g: `SuppressTriggers` scoped to `EntersBattlefield` events must NOT
/// suppress the attack branch of an `EntersOrAttacks` trigger. The
/// `event_is_suppressed_by_static_triggers` gate only filters ETB/Dies
/// `ZoneChanged` events; `AttackersDeclared` flows through untouched.
#[test]
fn etb_suppression_does_not_block_ureni_attack_trigger() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Torpor-Orb-style static on P1's side: suppress ETB triggers from any creature.
    let suppressor_def = StaticDefinition::new(StaticMode::SuppressTriggers {
        source_filter: TargetFilter::Typed(TypedFilter::creature()),
        events: vec![SuppressedTriggerEvent::EntersBattlefield],
    });
    let mut suppressor_builder = scenario.add_creature(P1, "Torpor Orb Stand-In", 1, 1);
    suppressor_builder.with_static_definition(suppressor_def);

    let ureni = add_ureni(&mut scenario, false);
    let mut runner = scenario.build();

    let life_before = runner.life(P0);
    run_combat(&mut runner, vec![ureni], vec![]);
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.life(P0),
        life_before + 1,
        "SuppressTriggers(ETB) must not block AttackersDeclared — the attack \
         branch of EntersOrAttacks must still fire"
    );
}
