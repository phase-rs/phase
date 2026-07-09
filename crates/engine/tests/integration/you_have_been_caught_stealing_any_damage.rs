//! You've Been Caught Stealing — Bribe mode: "You create a Treasure token for
//! each opponent who was dealt damage this turn."
//!
//! Discriminating runtime coverage for the `PlayerFilter::OpponentDealtDamage`
//! parameterization (gap Swallow:DynamicQty). The Bribe quantity is
//! "each opponent who was dealt **damage** this turn" — ANY damage, combat OR
//! noncombat (CR 120.2a/120.2b, `DamageKindFilter::Any`).
//!
//! REVERT-FAILING / non-vacuous: three players (P0 caster, P1 + P2 opponents).
//! P0 deals NONCOMBAT damage (Shock) to BOTH opponents, then casts YBCS choosing
//! Bribe. The correct count is 2 Treasures (one per any-damaged opponent). A
//! pre-fix swallow (`Fixed 1`) yields 1 Treasure → the `== 2` assertion fails; a
//! `CombatOnly` regression yields 0 (Shock is noncombat) → fails. Only `Any`
//! yields 2.
//!
//! A positive shape reach-guard confirms the Bribe mode parsed to
//! `PlayerCount { OpponentDealtDamage { kind: Any } }` with no `Unimplemented`,
//! so the runtime count is not passing for the wrong reason.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::parser::parse_oracle_text;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::ObjectId;

const P2: PlayerId = PlayerId(2);

const YBCS_ORACLE: &str = "Choose one —\n\u{2022} Threaten the Merchant — Each creature blocks this turn if able.\n\u{2022} Bribe the Guards — You create a Treasure token for each opponent who was dealt damage this turn. (It's an artifact with \"{T}, Sacrifice this token: Add one mana of any color.\")";

const SHOCK_ORACLE: &str = "Shock deals 2 damage to any target.";

/// Count battlefield Treasure tokens owned by `owner`.
fn treasure_count(state: &engine::types::game_state::GameState, owner: PlayerId) -> usize {
    state
        .battlefield
        .iter()
        .filter_map(|id| state.objects.get(id))
        .filter(|o| o.is_token && o.name == "Treasure" && o.owner == owner)
        .count()
}

#[test]
fn ybcs_bribe_creates_one_treasure_per_any_damaged_opponent() {
    // Shape reach-guard (positive): the Bribe mode must parse to the ANY-damage
    // player count, with no swallowed/unimplemented clause — otherwise the
    // runtime `== 2` below could pass for the wrong reason.
    let parsed = parse_oracle_text(YBCS_ORACLE, "You've Been Caught Stealing", &[], &[], &[]);
    let dbg = format!("{parsed:#?}");
    assert!(
        dbg.contains("OpponentDealtDamage") && dbg.contains("Any"),
        "Bribe mode must parse to PlayerCount {{ OpponentDealtDamage {{ kind: Any }} }}; got {dbg}"
    );
    assert!(
        !dbg.contains("Unimplemented"),
        "no clause of YBCS may fall through to Unimplemented; got {dbg}"
    );

    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    // Generous pool; oracle-parsed spells carry no mana cost, so this is simply
    // unused if a cast needs no payment.
    scenario.with_mana_pool(
        P0,
        (0..12)
            .map(|_| ManaUnit::new(ManaType::Red, ObjectId(0), false, vec![]))
            .collect(),
    );
    let shock1 = scenario
        .add_spell_to_hand_from_oracle(P0, "Shock", true, SHOCK_ORACLE)
        .id();
    let shock2 = scenario
        .add_spell_to_hand_from_oracle(P0, "Shock", true, SHOCK_ORACLE)
        .id();
    let ybcs = scenario
        .add_spell_to_hand_from_oracle(P0, "You've Been Caught Stealing", true, YBCS_ORACLE)
        .id();

    let mut runner = scenario.build();

    // NONCOMBAT damage to BOTH opponents (CR 120.2b), through the real pipeline
    // so each records a `damage_dealt_this_turn` entry with target = Player.
    runner.cast(shock1).target_player(P1).resolve();
    runner.cast(shock2).target_player(P2).resolve();

    // Bribe the Guards is the second mode (index 1).
    let outcome = runner.cast(ybcs).modes(&[1]).resolve();
    let state = outcome.state();

    // CR 120.1 + CR 120.2a/120.2b: both opponents were dealt (noncombat) damage
    // this turn → 2 Treasure tokens. Fixed-1 revert → 1; CombatOnly revert → 0.
    assert_eq!(
        treasure_count(state, P0),
        2,
        "Bribe must create one Treasure per any-damaged opponent (2)"
    );
}
