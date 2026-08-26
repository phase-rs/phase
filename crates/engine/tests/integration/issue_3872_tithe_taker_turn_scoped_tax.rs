//! Issue #3872 — Tithe Taker's "During your turn" cost tax must apply only on
//! the static controller's turn, not the caster's turn.
//!
//! Oracle: "During your turn, spells your opponents cast cost {1} more to cast
//! and abilities your opponents activate cost {1} more to activate unless
//! they're mana abilities."
//!
//! Reported bug (#3872): the tax raised the *opponent's* spells on the
//! opponent's OWN turn, because the parser dropped the leading "During your
//! turn," timing restriction (the static parsed with `condition: None`).
//!
//! CR 102.1: the active player is the player whose turn it is. Two fixes
//! combine here: the cost-modifier parser now attaches
//! `StaticCondition::DuringYourTurn`, and that condition is evaluated against
//! the source permanent's controller (not the caster) so it is correct in the
//! cost-modification resolver, which passes the caster as the scope player.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::parser::oracle_static::{parse_static_line, parse_static_line_multi};
use engine::types::ability::{PlayerFilter, StaticCondition};
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::statics::{ActivationExemption, CostModifyMode, StaticMode};

const TITHE_TAKER: &str = "During your turn, spells your opponents cast cost {1} more to cast and abilities your opponents activate cost {1} more to activate unless they're mana abilities.";

/// Begin casting P0's Lightning Bolt and return the total mana value of the
/// battlefield-modified cost the engine resolved for it. The cost is computed
/// when the spell is put on the stack (surfaced via `WaitingFor::TargetSelection`
/// before payment), so this reads the actual tax the cost resolver applied —
/// the {1} Tithe Taker increase is `mana_value() == 1`, no tax is `0`.
fn resolved_cost_mana_value(runner: &mut GameRunner, spell_id: ObjectId) -> u32 {
    let card_id = runner.state().objects[&spell_id].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell_id,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("casting the bolt should begin (cost is checked at payment, not here)");
    match &runner.state().waiting_for {
        WaitingFor::TargetSelection { pending_cast, .. } => pending_cast.cost.mana_value(),
        other => panic!("expected TargetSelection after casting the bolt, got {other:?}"),
    }
}

#[test]
fn tithe_taker_static_parses_with_during_your_turn_condition() {
    // CR 102.1: the leading "During your turn," timing restriction must lower
    // to a `StaticCondition::DuringYourTurn` gate on the cost-raise static —
    // not be silently dropped (which left `condition: None`, the root cause).
    let def = parse_static_line(TITHE_TAKER).expect("Tithe Taker static should parse");
    assert_eq!(
        def.condition,
        Some(StaticCondition::DuringYourTurn),
        "\"During your turn,\" must gate the cost-raise static, got {:?}",
        def.condition,
    );
}

#[test]
fn tithe_taker_does_not_tax_opponent_on_their_own_turn() {
    // P1 controls Tithe Taker. During P0's OWN turn, P0's Lightning Bolt must
    // NOT be taxed — the resolved cost carries no {1} increase. Before the fix
    // the dropped "During your turn" gate taxed it on every turn.
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain); // active player = P0

    scenario
        .add_creature(P1, "Tithe Taker", 1, 2)
        .with_static_definition(
            parse_static_line(TITHE_TAKER).expect("Tithe Taker static should parse"),
        );
    let spell_id = scenario.add_bolt_to_hand(P0);

    let mut runner = scenario.build();
    assert_eq!(
        resolved_cost_mana_value(&mut runner, spell_id),
        0,
        "Tithe Taker must NOT tax an opponent's spell on the opponent's own turn (CR 102.1)",
    );
}

#[test]
fn tithe_taker_taxes_opponent_during_controllers_turn() {
    // P1 controls Tithe Taker. During P1's turn, P0's Lightning Bolt is taxed by
    // {1} — the resolver applies the increase when it is the static controller's
    // turn, confirming the gate is enabled (not merely disabled everywhere).
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    scenario
        .add_creature(P1, "Tithe Taker", 1, 2)
        .with_static_definition(
            parse_static_line(TITHE_TAKER).expect("Tithe Taker static should parse"),
        );
    let spell_id = scenario.add_bolt_to_hand(P0);

    let mut runner = scenario.build();
    // Move to P1's turn and hand P0 priority to cast its instant.
    {
        let state = runner.state_mut();
        state.active_player = P1;
        state.priority_player = P0;
        state.waiting_for = WaitingFor::Priority { player: P0 };
    }
    assert_eq!(
        resolved_cost_mana_value(&mut runner, spell_id),
        1,
        "During the Tithe Taker controller's turn, the opponent's spell must be taxed by {{1}}",
    );
}

// ---------------------------------------------------------------------------
// Ability-cost half — "abilities your opponents activate cost {1} more to
// activate unless they're mana abilities" (previously dropped silently).
// ---------------------------------------------------------------------------

const POOL_UNITS: usize = 5;

/// P0 controls Tithe Taker (wired from the production multi-parse, so it carries
/// BOTH cost statics). P1 (an opponent) controls a permanent with a `{1}`
/// non-mana activated ability and a funded mana pool. Announce the activation as
/// P1 during `active`'s turn, finalize the mana payment from the pool, and
/// return how much generic mana the activation consumed (the taxed cost).
fn mana_consumed_activating_opponent_ability(active: PlayerId) -> usize {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // BOTH statics from the compound line (the ability half is the fix under test).
    {
        let mut b = scenario.add_creature(P0, "Tithe Taker", 1, 2);
        for def in parse_static_line_multi(TITHE_TAKER) {
            b.with_static_definition(def);
        }
    }

    // P1's permanent with a plain `{1}` activated ability (NOT a mana ability).
    let pinger = scenario
        .add_creature_from_oracle(P1, "Test Pinger", 1, 1, "{1}: You gain 1 life.")
        .id();

    // Source auto-tap is not modeled; fund the pool so payment finalizes from it.
    let pool: Vec<ManaUnit> = (0..POOL_UNITS)
        .map(|_| ManaUnit::new(ManaType::White, ObjectId(0), false, vec![]))
        .collect();
    scenario.with_mana_pool(P1, pool);

    let mut runner = scenario.build();
    // Hand P1 priority to activate; `active` decides whose turn it is.
    {
        let state = runner.state_mut();
        state.active_player = active;
        state.priority_player = P1;
        state.waiting_for = WaitingFor::Priority { player: P1 };
    }

    runner
        .act(GameAction::ActivateAbility {
            source_id: pinger,
            ability_index: 0,
        })
        .expect("P1 announces its {1} activated ability");
    // Finalize the mana payment from the funded pool (mirrors AbilityActivation:
    // PassPriority pays from the pool since source auto-tap is not modeled).
    for _ in 0..8 {
        if matches!(runner.state().waiting_for, WaitingFor::ManaPayment { .. }) {
            runner
                .act(GameAction::PassPriority)
                .expect("finalize the ability's mana payment from the pool");
        } else {
            break;
        }
    }

    let remaining = runner
        .state()
        .players
        .iter()
        .find(|p| p.id == P1)
        .expect("P1 exists")
        .mana_pool
        .total();
    POOL_UNITS - remaining
}

#[test]
fn tithe_taker_compound_line_yields_cast_and_ability_cost_statics() {
    // The production multi-parse of the full compound line must emit BOTH the
    // spell-cast tax and the activated-ability tax — the ability half was
    // previously dropped silently (the single-return pipeline kept only the cast
    // half). CR 602.2: opponent-activator-scoped; CR 605.1a: mana abilities are
    // exempt; CR 604.1: it inherits the leading "During your turn" gate.
    let defs = parse_static_line_multi(TITHE_TAKER);
    assert_eq!(
        defs.len(),
        2,
        "the compound line must yield the cast + ability statics, got {defs:?}",
    );
    assert!(
        defs.iter()
            .any(|d| matches!(d.mode, StaticMode::ModifyCost { .. })),
        "the spell-cast tax half must be present",
    );
    let ability = defs
        .iter()
        .find(|d| matches!(d.mode, StaticMode::ReduceAbilityCost { .. }))
        .expect("the activated-ability tax half must be present (was silently dropped)");
    assert_eq!(
        ability.condition,
        Some(StaticCondition::DuringYourTurn),
        "the ability half inherits the shared \"During your turn\" gate",
    );
    let StaticMode::ReduceAbilityCost {
        mode,
        amount,
        activator,
        exemption,
        ..
    } = &ability.mode
    else {
        unreachable!("filtered to ReduceAbilityCost above")
    };
    assert_eq!(*mode, CostModifyMode::Raise);
    assert_eq!(*amount, 1);
    assert_eq!(
        *activator,
        Some(PlayerFilter::Opponent),
        "the tax keys off an OPPONENT activating the ability (CR 602.2)",
    );
    assert_eq!(
        *exemption,
        ActivationExemption::ManaAbilities,
        "\"unless they're mana abilities\" exempts mana abilities (CR 605.1a)",
    );
}

#[test]
fn tithe_taker_taxes_opponent_activated_ability_during_controllers_turn() {
    // On the Tithe Taker controller's (P0's) turn, an opponent's {1} activated
    // ability is taxed to {2} — the opponent-activator ability-cost static
    // applies end-to-end (parse -> runtime). Reverting the parser fix drops the
    // ability static, so the activation would consume only {1} and this fails.
    assert_eq!(
        mana_consumed_activating_opponent_ability(P0),
        2,
        "on the controller's turn, an opponent's {{1}} ability must cost {{2}}",
    );
}

#[test]
fn tithe_taker_does_not_tax_opponent_activated_ability_on_their_own_turn() {
    // On the opponent's (P1's) OWN turn, the "During your turn" gate (P0's turn)
    // is not satisfied, so the same {1} ability costs {1} — no tax. This is the
    // differential that proves the DuringYourTurn gate is honored on the ability
    // half (CR 604.1), not merely that some tax exists.
    assert_eq!(
        mana_consumed_activating_opponent_ability(P1),
        1,
        "on the opponent's own turn the DuringYourTurn gate is off; the ability costs {{1}}",
    );
}
