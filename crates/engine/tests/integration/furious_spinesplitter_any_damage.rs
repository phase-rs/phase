//! Furious Spinesplitter — end-step trigger: "put a +1/+1 counter on this
//! creature for each opponent who was dealt damage this turn."
//!
//! Discriminating runtime coverage for the `PlayerFilter::OpponentDealtDamage`
//! parameterization (gap Swallow:DynamicQty). The quantity is "each opponent
//! who was dealt **damage** this turn" — ANY damage, combat OR noncombat
//! (CR 120.2a/120.2b, `DamageKindFilter::Any`).
//!
//! REVERT-FAILING / non-vacuous: three players (P0 controls Spinesplitter, P1 +
//! P2 opponents). P0 deals NONCOMBAT damage (Shock) to BOTH opponents, then the
//! end-step trigger resolves. Correct count is 2 counters (one per any-damaged
//! opponent). A pre-fix swallow (`Fixed 1`) yields 1 counter → the `== 2`
//! assertion fails; a `CombatOnly` regression yields 0 (Shock is noncombat) →
//! fails. Only `Any` yields 2.
//!
//! A positive shape reach-guard confirms the trigger parsed to
//! `PlayerCount { OpponentDealtDamage { kind: Any } }` with no `Unimplemented`.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::parser::parse_oracle_text;
use engine::types::counter::CounterType;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::ObjectId;

const P2: PlayerId = PlayerId(2);

const SPINESPLITTER_ORACLE: &str = "Trample\nAt the beginning of your end step, put a +1/+1 counter on this creature for each opponent who was dealt damage this turn.";

const SHOCK_ORACLE: &str = "Shock deals 2 damage to any target.";

fn plus1_counters(state: &engine::types::game_state::GameState, id: ObjectId) -> u32 {
    state
        .objects
        .get(&id)
        .and_then(|o| o.counters.get(&CounterType::Plus1Plus1).copied())
        .unwrap_or(0)
}

#[test]
fn furious_spinesplitter_counters_per_any_damaged_opponent() {
    // Shape reach-guard (positive): the end-step trigger must parse to the
    // ANY-damage player count, no swallowed/unimplemented clause.
    // Trample is supplied as an MTGJSON keyword (as in production), so the
    // keyword line is extracted rather than left as a bare-word effect.
    let parsed = parse_oracle_text(
        SPINESPLITTER_ORACLE,
        "Furious Spinesplitter",
        &["Trample".to_string()],
        &[],
        &[],
    );
    let dbg = format!("{parsed:#?}");
    assert!(
        dbg.contains("OpponentDealtDamage") && dbg.contains("Any"),
        "trigger must parse to PlayerCount {{ OpponentDealtDamage {{ kind: Any }} }}; got {dbg}"
    );
    assert!(
        !dbg.contains("Unimplemented"),
        "no clause of Furious Spinesplitter may fall through to Unimplemented; got {dbg}"
    );

    let mut scenario = GameScenario::new_n_player(3, 42);
    // Post-combat main so advancing to the end step does not cross the combat
    // declaration steps (which would stall the phase driver at DeclareAttackers).
    scenario.at_phase(Phase::PostCombatMain);
    scenario.with_mana_pool(
        P0,
        (0..8)
            .map(|_| ManaUnit::new(ManaType::Red, ObjectId(0), false, vec![]))
            .collect(),
    );
    // Verbatim Oracle (keyword Trample extracted from the keyword line).
    let spinesplitter = scenario
        .add_creature_from_oracle(P0, "Furious Spinesplitter", 3, 3, SPINESPLITTER_ORACLE)
        .id();
    let shock1 = scenario
        .add_spell_to_hand_from_oracle(P0, "Shock", true, SHOCK_ORACLE)
        .id();
    let shock2 = scenario
        .add_spell_to_hand_from_oracle(P0, "Shock", true, SHOCK_ORACLE)
        .id();

    let mut runner = scenario.build();

    // NONCOMBAT damage to BOTH opponents (CR 120.2b) through the real pipeline.
    runner.cast(shock1).target_player(P1).resolve();
    runner.cast(shock2).target_player(P2).resolve();

    // Advance to P0's end step and resolve the triggered ability.
    runner.advance_to_end_step();
    runner.advance_until_stack_empty();

    // CR 120.1 + CR 120.2a/120.2b: both opponents were dealt (noncombat) damage
    // this turn → 2 +1/+1 counters. Fixed-1 revert → 1; CombatOnly revert → 0.
    assert_eq!(
        plus1_counters(runner.state(), spinesplitter),
        2,
        "Spinesplitter must gain one +1/+1 counter per any-damaged opponent (2)"
    );
}
