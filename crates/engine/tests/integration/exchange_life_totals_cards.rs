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
