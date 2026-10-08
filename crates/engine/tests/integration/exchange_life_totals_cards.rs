//! Full-card coverage for issue #3486 — player-to-player "exchange life totals"
//! (CR 701.12a). Real Oracle text from AtomicCards.json, driven through the
//! activation + resolution pipeline.
//!
//! - Soul Conduit: "{6}, {T}: Two target players exchange life totals."
//!   (ExchangeLifeTotals{Player, Player}).
//! - Mirror Universe: "{T}, Sacrifice this artifact: Exchange life totals with
//!   target opponent. Activate only during your upkeep."
//!   (ExchangeLifeTotals{Controller, Typed(Opponent)}).

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;

const SOUL_CONDUIT_ORACLE: &str = "{6}, {T}: Two target players exchange life totals.";
const MIRROR_UNIVERSE_ORACLE: &str =
    "{T}, Sacrifice this artifact: Exchange life totals with target opponent. \
     Activate only during your upkeep.";

/// CR 701.12c + CR 701.12a: Soul Conduit's activated ability swaps two target
/// players' life totals. P0=20, P1=5 → P0=5, P1=20.
#[test]
fn soul_conduit_swaps_two_target_players_life_totals() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_life(P0, 20).with_life(P1, 5);
    // {6} generic: fund the controller's pool with six colorless mana (source
    // auto-tap isn't modeled by the activation driver).
    scenario.with_mana_pool(
        P0,
        (0..6)
            .map(|_| ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]))
            .collect(),
    );
    let conduit = scenario
        .add_creature(P0, "Soul Conduit", 0, 0)
        .from_oracle_text(SOUL_CONDUIT_ORACLE)
        .as_artifact()
        .id();

    let mut runner = scenario.build();

    // Drive activation manually: the two `Player` slots must receive DISTINCT
    // players (P0 then P1, in declaration order). The fluent `AbilityActivation`
    // driver reuses the same first-matching declared player for every slot, so
    // it can't express two distinct same-filter player slots — choose each slot
    // explicitly here.
    runner
        .act(GameAction::ActivateAbility {
            source_id: conduit,
            ability_index: 0,
        })
        .expect("begin Soul Conduit activation");

    // Pay {6} from the funded pool, then choose P0 and P1 for the two slots.
    let players = [P0, P1];
    let mut next_player = 0usize;
    for _ in 0..16 {
        match &runner.state().waiting_for {
            WaitingFor::ManaPayment { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("finalize {6} payment from pool");
            }
            WaitingFor::TargetSelection { .. } => {
                let pid = players[next_player];
                next_player += 1;
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(TargetRef::Player(pid)),
                    })
                    .expect("choose target player");
            }
            WaitingFor::Priority { .. } => break,
            other => panic!("unexpected Soul Conduit activation prompt: {other:?}"),
        }
    }

    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().players[0].life,
        5,
        "P0's life should become P1's former total"
    );
    assert_eq!(
        runner.state().players[1].life,
        20,
        "P1's life should become P0's former total"
    );
}

/// CR 701.12c + CR 701.12a: Mirror Universe's {T},Sac ability (during the
/// controller's upkeep) swaps the controller's and a target opponent's life
/// totals. P0=3, P1=18 → P0=18, P1=3.
#[test]
fn mirror_universe_swaps_controller_with_target_opponent() {
    let mut scenario = GameScenario::new();
    // "Activate only during your upkeep" — P0 is the active player by default.
    scenario.at_phase(Phase::Upkeep);
    scenario.with_life(P0, 3).with_life(P1, 18);
    let mirror = scenario
        .add_creature(P0, "Mirror Universe", 0, 0)
        .from_oracle_text(MIRROR_UNIVERSE_ORACLE)
        .as_artifact()
        .id();

    let mut runner = scenario.build();
    let outcome = runner.activate(mirror, 0).target_player(P1).resolve();

    assert_eq!(
        outcome.state().players[0].life,
        18,
        "controller's life should become the opponent's former total"
    );
    assert_eq!(
        outcome.state().players[1].life,
        3,
        "opponent's life should become the controller's former total"
    );
}

// ---------------------------------------------------------------------------
// Round-6 plan — per-node target ownership for paired-subject effects.
// The `Effect::ExchangeLifeTotals` re-validation pruning delta (§5.9): all
// seven `ExchangeLifeTotals` cards are now routed through the paired
// `validate_targets_in_chain` arm and can PRUNE an eliminated declared
// target, where BASE kept every player target unconditionally via the
// terminal generic `None` arm's `TargetRef::Player(_) => true`.
// ---------------------------------------------------------------------------

/// V6a's HOSTILE sub-case — Mirror Universe's `(Controller, Typed{Opponent})`
/// shape: with the target opponent eliminated, the node's declared slot is
/// PRUNED to empty (CR 608.2b), instead of surviving unconditionally.
///
/// REVERT-FAILING (G2): at BASE, `ExchangeLifeTotals` falls through to
/// `validate_targets_in_chain`'s terminal generic arm, whose
/// `TargetRef::Player(_) => true` keeps every player target with no filter
/// consulted — including an eliminated one.
#[test]
fn mirror_universe_hostile_target_opponent_eliminated_prunes_the_declared_slot() {
    use engine::game::ability_utils::{build_resolved_from_def, validate_targets_in_chain};
    use engine::parser::oracle::parse_oracle_text;

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::Upkeep);
    let mirror = scenario.add_creature(P0, "Mirror Universe", 0, 0).id();
    let mut runner = scenario.build();

    let parsed = parse_oracle_text(MIRROR_UNIVERSE_ORACLE, "Mirror Universe", &[], &[], &[]);
    let def = parsed
        .abilities
        .first()
        .expect("Mirror Universe has an activated ability")
        .clone();
    let mut resolved = build_resolved_from_def(&def, mirror, P0);
    resolved.targets = vec![TargetRef::Player(P1)];

    // REACH GUARD (paired positive): un-eliminated, the declared slot
    // survives validation untouched — matching
    // `mirror_universe_swaps_controller_with_target_opponent` staying green.
    assert_eq!(
        validate_targets_in_chain(runner.state(), &resolved).targets,
        vec![TargetRef::Player(P1)],
        "reach guard: an un-eliminated target opponent must survive validation"
    );

    runner.state_mut().players[P1.0 as usize].is_eliminated = true;
    let validated = validate_targets_in_chain(runner.state(), &resolved);
    assert!(
        validated.targets.is_empty(),
        "with the target opponent eliminated, the node's declared slot must be pruned \
         to empty, got {:?}",
        validated.targets
    );
}

/// V6b — Soul Conduit's `(Player, Player)` shape: ONE eliminated target
/// player is PRUNED, and CR 701.12a's all-or-nothing rule is what turns a
/// short declared-players list into a total no-op at resolution (the
/// resolver's own `emit_noop` dry-slot exit, unchanged by this row).
///
/// REVERT-FAILING (MEASURED, G2): `[Player(0), Player(1)]` before this
/// change, `[Player(0)]` after.
#[test]
fn soul_conduit_eliminated_target_player_is_pruned_and_the_exchange_no_ops() {
    use engine::game::ability_utils::{build_resolved_from_def, validate_targets_in_chain};
    use engine::parser::oracle::parse_oracle_text;

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let conduit = scenario.add_creature(P0, "Soul Conduit", 0, 0).id();
    let mut runner = scenario.build();

    let parsed = parse_oracle_text(SOUL_CONDUIT_ORACLE, "Soul Conduit", &[], &[], &[]);
    let def = parsed
        .abilities
        .first()
        .expect("Soul Conduit has an activated ability")
        .clone();
    let mut resolved = build_resolved_from_def(&def, conduit, P0);
    resolved.targets = vec![TargetRef::Player(P0), TargetRef::Player(P1)];

    // REACH GUARD (paired positive): the existing
    // `soul_conduit_swaps_two_target_players_life_totals` row above proves
    // the un-eliminated pipeline swaps both life totals end to end.
    assert_eq!(
        validate_targets_in_chain(runner.state(), &resolved).targets,
        vec![TargetRef::Player(P0), TargetRef::Player(P1)],
        "reach guard: with neither player eliminated, both declared targets survive"
    );

    runner.state_mut().players[P1.0 as usize].is_eliminated = true;
    let validated = validate_targets_in_chain(runner.state(), &resolved);
    assert_eq!(
        validated.targets,
        vec![TargetRef::Player(P0)],
        "BASE (G2): [Player(0), Player(1)] unconditionally kept via the terminal \
         generic None arm's TargetRef::Player(_) => true — no filter consulted. \
         AFTER: the eliminated player is pruned, leaving only the survivor"
    );
}

/// V6c-ii — Cliffside Market's `(Controller, Player)` shape: with the
/// declared player eliminated, the node's SINGLE declared slot (the
/// context-ref `Controller` half claims nothing — V6c-i pins that at the
/// unit level) is pruned to empty, which is what makes the exchange a total
/// no-op via CR 701.12a. Driven directly against the pub re-validation seam
/// (`validate_targets_in_chain`) with real parsed Oracle text.
#[test]
fn cliffside_market_controller_and_target_player_prunes_its_single_declared_slot() {
    use engine::game::ability_utils::{build_resolved_from_def, validate_targets_in_chain};
    use engine::parser::oracle::parse_oracle_text;
    use engine::types::ability::Effect;

    const CLIFFSIDE_MARKET_ORACLE: &str = "When you planeswalk to Cliffside Market and at the \
        beginning of your upkeep, you may exchange life totals with target player.\nWhenever \
        chaos ensues, exchange control of two target permanents that share a card type.";

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let market = scenario
        .add_creature(P0, "Cliffside Market Host", 0, 0)
        .id();
    let mut runner = scenario.build();

    let parsed = parse_oracle_text(CLIFFSIDE_MARKET_ORACLE, "Cliffside Market", &[], &[], &[]);
    let elt_def = parsed
        .triggers
        .iter()
        .find_map(|t| {
            let def = t.execute.clone()?;
            matches!(*def.effect, Effect::ExchangeLifeTotals { .. }).then_some(def)
        })
        .expect("Cliffside Market has an ExchangeLifeTotals trigger");
    let mut resolved = build_resolved_from_def(&elt_def, market, P0);
    resolved.targets = vec![TargetRef::Player(P1)];

    // REACH GUARD (paired positive): un-eliminated, the sole declared slot
    // survives validation untouched.
    assert_eq!(
        validate_targets_in_chain(runner.state(), &resolved).targets,
        vec![TargetRef::Player(P1)],
        "reach guard: an un-eliminated target player must survive validation"
    );

    runner.state_mut().players[P1.0 as usize].is_eliminated = true;
    let validated = validate_targets_in_chain(runner.state(), &resolved);
    assert!(
        validated.targets.is_empty(),
        "the single declared (Player) slot must be pruned when its target is \
         eliminated, got {:?}",
        validated.targets
    );
}
