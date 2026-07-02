//! GROUP A2 (S01) — resolution-time current-phase condition, driven through the
//! real parse + cast/resolve pipeline.
//!
//! Dose of Dawnglow: "Return target creature card from your graveyard to the
//! battlefield. Then if it isn't your main phase, blight 2." The "if it isn't
//! your main phase" rider is a resolution-time gate on the *current* phase
//! (CR 608.2c reads the whole text at resolution), distinct from the
//! casting-time `CastDuringPhase` snapshot. It decomposes into
//! `Not(And([CurrentPhaseIs{[PreCombatMain, PostCombatMain]}, IsYourTurn]))`:
//!   - CR 505.1 / CR 505.1a: "main phase" is BOTH the precombat and postcombat
//!     main phases.
//!   - CR 102.1: "your" phase = a phase of your turn (active player == controller).
//!   - "isn't" = `Not`.
//!
//! Before this change the rider was dropped (the blight sub-ability parsed with a
//! `null` condition — blight fired unconditionally). The runtime discriminator is
//! the negative case: cast during YOUR main phase, the gate is false and blight
//! must NOT fire. Reverting the recognizer makes the gate `null`, blight fires
//! unconditionally, an `EffectZoneChoice` surfaces, and the negative assertion
//! flips.

use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{AbilityCondition, AbilityDefinition, Effect};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::WaitingFor;
use engine::types::phase::Phase;

// Verified identical to the engine's authoritative card data (MTGJSON AtomicCards,
// confirmed against data/card-data.json oracle_text).
const DOSE: &str = "Return target creature card from your graveyard to the battlefield. \
     Then if it isn't your main phase, blight 2. (Put two -1/-1 counters on a creature you control.)";

/// Depth-first walk of an ability + its `sub_ability`/`else_ability` chain,
/// returning the first node whose top-level effect discriminant matches `pred`.
fn find_node<'a>(
    def: &'a AbilityDefinition,
    pred: &dyn Fn(&Effect) -> bool,
) -> Option<&'a AbilityDefinition> {
    if pred(&def.effect) {
        return Some(def);
    }
    def.sub_ability
        .as_deref()
        .and_then(|s| find_node(s, pred))
        .or_else(|| def.else_ability.as_deref().and_then(|s| find_node(s, pred)))
}

/// The Dose-of-Dawnglow gate as it must parse and as the resolver consults it.
fn your_main_phase_gate() -> AbilityCondition {
    AbilityCondition::Not {
        condition: Box::new(AbilityCondition::And {
            conditions: vec![
                AbilityCondition::CurrentPhaseIs {
                    phases: vec![Phase::PreCombatMain, Phase::PostCombatMain],
                },
                AbilityCondition::IsYourTurn,
            ],
        }),
    }
}

// ===========================================================================
// PARSER (production `parse_oracle_text`) — structural gate. Reverting the
// recognizer drops this condition back to `None`.
// ===========================================================================

/// The blight sub-ability must be gated on `Not(And([CurrentPhaseIs{both mains},
/// IsYourTurn]))`. Before the fix this condition was `None` (blight ungated).
#[test]
fn dose_blight_sub_ability_gated_on_current_phase() {
    let p = parse_oracle_text(DOSE, "Dose of Dawnglow", &[], &["Instant".into()], &[]);
    let spell = p
        .abilities
        .first()
        .expect("Dose must parse a spell ability (the graveyard return)");
    let blight = find_node(spell, &|e| matches!(e, Effect::BlightEffect { .. }))
        .expect("Dose must chain a BlightEffect sub-ability");
    assert_eq!(
        blight.condition,
        Some(your_main_phase_gate()),
        "blight must be gated on Not(And([CurrentPhaseIs{{both mains}}, IsYourTurn])); \
         a None condition means the recognizer is reverted and blight is ungated"
    );
}

// ===========================================================================
// RUNTIME (production cast/resolve pipeline) — behavioral discriminators.
// ===========================================================================

/// Build P0's turn at `phase`: a pre-existing creature P0 controls (blight's only
/// candidate sink besides the returned card), a creature card in P0's graveyard
/// for Dose to return, and Dose in hand; cast Dose at the graveyard creature and
/// drive resolution. Returns (blight-sink id, live runner). The runner is handed
/// back live so the positive (gate-true) test can complete the `EffectZoneChoice`
/// that blight surfaces.
fn dose_scenario(phase: Phase) -> (engine::types::identifiers::ObjectId, GameRunner) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(phase); // P0's turn; P0 has priority (can cast the instant)
    let sink = scenario.add_creature(P0, "Blight Sink", 3, 3).id();
    let grave_bear = scenario
        .add_creature_to_graveyard(P0, "Returned Bear", 2, 2)
        .id();
    let dose = scenario
        .add_spell_to_hand_from_oracle(P0, "Dose of Dawnglow", true, DOSE)
        .id();
    let mut runner = scenario.build();
    // Cast Dose targeting the graveyard creature; the shared driver resolves the
    // spell and halts at the blight `EffectZoneChoice` if (and only if) the gate
    // is true. The post-resolution waiting_for lives on `runner.state`.
    runner.cast(dose).target_object(grave_bear).resolve();
    (sink, runner)
}

/// Negative discriminator — cast during YOUR precombat main phase. The gate is
/// false (`And` true → `Not` false), so the blight sub-ability is skipped: no
/// `EffectZoneChoice` surfaces and the sink gains no counters. Reverting the
/// recognizer makes the gate `null`; blight then fires unconditionally and the
/// pipeline halts at an `EffectZoneChoice` — the `!EffectZoneChoice` assertion
/// flips and the test fails.
#[test]
fn dose_blight_suppressed_during_your_main_phase() {
    let (sink, runner) = dose_scenario(Phase::PreCombatMain);
    assert!(
        !matches!(
            runner.state().waiting_for,
            WaitingFor::EffectZoneChoice { .. }
        ),
        "during YOUR main phase the gate is false — blight must NOT fire and must \
         not prompt a creature choice. Got: {:?}",
        runner.state().waiting_for
    );
    assert_eq!(
        runner
            .state()
            .objects
            .get(&sink)
            .and_then(|o| o.counters.get(&CounterType::Minus1Minus1).copied())
            .unwrap_or(0),
        0,
        "blight suppressed during your main phase → no -1/-1 counters land"
    );
}

/// Positive — cast during YOUR end step. The gate is true (`CurrentPhaseIs{both
/// mains}` false → `And` false → `Not` true), so blight fires and prompts the
/// controller to choose a creature; choosing the sink places two -1/-1 counters.
/// CR 505.1a is exercised by the negative `PostCombatMain` resolver test; here the
/// non-main End step proves the gate flips the other way at runtime.
#[test]
fn dose_blight_fires_outside_your_main_phase() {
    let (sink, mut runner) = dose_scenario(Phase::End);
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::EffectZoneChoice { .. }
        ),
        "outside your main phase the gate is true — blight must fire and prompt a \
         creature choice. Got: {:?}",
        runner.state().waiting_for
    );
    // Complete the blight choice through the real resolution handler.
    runner
        .act(GameAction::SelectCards { cards: vec![sink] })
        .expect("blight EffectZoneChoice must accept the chosen creature");
    assert_eq!(
        runner
            .state()
            .objects
            .get(&sink)
            .and_then(|o| o.counters.get(&CounterType::Minus1Minus1).copied())
            .unwrap_or(0),
        2,
        "blight 2 places two -1/-1 counters on the chosen creature"
    );
}
