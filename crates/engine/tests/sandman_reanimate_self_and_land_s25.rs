//! S25 P3 W1 #2 — Sandman, Shifting Scoundrel — RUNTIME coverage for the
//! self-and-target reanimation idiom, driven through the real activation
//! pipeline (`GameScenario` + `ActivateAbility` + resolution).
//!
//! Oracle (activated line):
//!   `{3}{G}{G}: Return this card and target land card from your graveyard to
//!    the battlefield tapped.`
//!
//! The parser lowers this to a BARE `ChangeZone { SelfRef, Graveyard →
//! Battlefield, tapped }` primary plus a targeted-land `ChangeZone` sub_ability
//! (mirroring the shipped Coastal Wizard / Lady Sun two-chained idiom). The
//! bare SelfRef primary is what lets `activation_zone_from_self_effect`
//! (CR 113.6m) stamp the graveyard as the activation zone, so the ability is
//! offered from the graveyard at all.
//!
//! This test is DISCRIMINATING and fails if the recognizer is reverted:
//!   * Revert → the ability parses to an inert `And[SelfRef, gy]` primary with a
//!     NULL activation_zone, so `ActivateAbility` from the graveyard is illegal
//!     and the first `.expect(..)` panics (the ability is never offered).
//!   * Even if it were offered, the `And` primary resolves to nothing and the
//!     sub is `Unimplemented`, so neither object moves — the battlefield/tapped
//!     assertions would fail.
//!
//! CR ANCHORS:
//!   * CR 602.2 — activating an activated ability (here, from the graveyard).
//!   * CR 601.2c + CR 115.1 — the land is a chosen target at activation;
//!     SelfRef (CR 400.7) is not a target.
//!   * CR 608.2c — the two chained moves resolve in written order.
//!   * CR 614.1 — "to the battlefield tapped" enters BOTH objects tapped.

use engine::game::scenario::{GameScenario, P0};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const SANDMAN_ORACLE: &str =
    "Sandman's power and toughness are each equal to the number of lands you control.\n\
Sandman can't be blocked by creatures with power 2 or less.\n\
{3}{G}{G}: Return this card and target land card from your graveyard to the battlefield tapped.";

/// Enough green mana to pay {3}{G}{G} (2 green pips + 3 generic).
fn green_pool(n: usize) -> Vec<ManaUnit> {
    (0..n)
        .map(|_| ManaUnit::new(ManaType::Green, ObjectId(0), false, vec![]))
        .collect()
}

#[test]
fn sandman_returns_self_and_land_both_tapped_from_graveyard() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Sandman + two lands (Forest, Mountain — both legal land targets) + Grizzly
    // Bears (a nonland creature, the illegal target) all in P0's graveyard. Two
    // legal lands force a real target-choice window (a unique legal target would
    // be auto-selected), so the Land filter's legal set is observable.
    let sandman = scenario
        .add_creature_to_graveyard(P0, "Sandman, Shifting Scoundrel", 0, 0)
        .from_oracle_text(SANDMAN_ORACLE)
        .id();
    let forest = scenario
        .add_creature_to_graveyard(P0, "Forest", 0, 0)
        .as_land()
        .id();
    let mountain = scenario
        .add_creature_to_graveyard(P0, "Mountain", 0, 0)
        .as_land()
        .id();
    let grizzly = scenario
        .add_creature_to_graveyard(P0, "Grizzly Bears", 2, 2)
        .id();

    // Fund the pool for {3}{G}{G} (auto-tap of mana sources is not modeled).
    scenario.with_mana_pool(P0, green_pool(5));

    let mut runner = scenario.build();

    // CR 602.2: activate the reanimation ability from the graveyard. On revert
    // (null activation_zone) this is illegal and the `.expect` panics.
    runner
        .act(GameAction::ActivateAbility {
            source_id: sandman,
            ability_index: 0,
        })
        .expect("Sandman's return ability must be activatable from the graveyard");

    // Drive announcement → target → payment → resolution. The land target is
    // chosen at activation (CR 601.2c); assert the Land filter the first time we
    // reach it, then submit the Forest.
    let mut chose_land = false;
    for _ in 0..64 {
        match &runner.state().waiting_for {
            // CR 601.2c + CR 115.1: choose the land target; verify it is a real
            // Land-filtered slot (not a hollow "return everything" sweep).
            WaitingFor::TargetSelection {
                target_slots,
                selection,
                ..
            } => {
                let slot = &target_slots[selection.current_slot];
                let legal = &slot.legal_targets;
                assert!(
                    legal.contains(&TargetRef::Object(forest))
                        && legal.contains(&TargetRef::Object(mountain)),
                    "both land cards must be legal targets; legal = {legal:?}"
                );
                assert!(
                    !legal.contains(&TargetRef::Object(grizzly)),
                    "Grizzly Bears (a nonland creature) must NOT be a legal target — the \
                     'target land card' filter (CR 601.2c) must exclude it; legal = {legal:?}"
                );
                assert!(
                    !legal.contains(&TargetRef::Object(sandman)),
                    "SelfRef is not a target (CR 400.7); Sandman must not be in the legal set"
                );
                chose_land = true;
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(TargetRef::Object(forest)),
                    })
                    .expect("choosing the Forest as the land target must succeed");
            }
            // CR 602.2b + CR 601.2h: finalize the {3}{G}{G} payment from the
            // funded pool (activation payment follows the spell-casting cost steps).
            WaitingFor::ManaPayment { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("finalizing the mana payment from the pool must succeed");
            }
            // CR 608.2c: the ability is on the stack — drain it to resolution.
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() {
                    break;
                }
                runner
                    .act(GameAction::PassPriority)
                    .expect("passing priority to resolve the ability must succeed");
            }
            other => panic!("unexpected window while resolving Sandman: {other:?}"),
        }
    }

    assert!(
        chose_land,
        "the land target-selection window must have been reached (revert reach-guard)"
    );

    let zone_of = |id: ObjectId| runner.state().objects.get(&id).map(|o| o.zone);
    let tapped_of = |id: ObjectId| runner.state().objects.get(&id).map(|o| o.tapped);

    // CR 400.7 + CR 608.2c: both the source and the chosen land are now on the
    // battlefield, and CR 614.1: both entered tapped.
    assert_eq!(
        zone_of(sandman),
        Some(Zone::Battlefield),
        "Sandman must return to the battlefield"
    );
    assert_eq!(
        zone_of(forest),
        Some(Zone::Battlefield),
        "the targeted Forest must return to the battlefield"
    );
    assert_eq!(
        tapped_of(sandman),
        Some(true),
        "CR 614.1: Sandman enters tapped"
    );
    assert_eq!(
        tapped_of(forest),
        Some(true),
        "CR 614.1: the returned Forest enters tapped"
    );

    // The unchosen land AND the untargeted nonland both stay put — proves only
    // the single CHOSEN land moved (not a "return all lands" sweep) and the slot
    // was a real Land filter.
    assert_eq!(
        zone_of(mountain),
        Some(Zone::Graveyard),
        "the unchosen Mountain must stay in the graveyard (only the chosen land moves)"
    );
    assert_eq!(
        zone_of(grizzly),
        Some(Zone::Graveyard),
        "Grizzly Bears (never targeted, nonland) must stay in the graveyard"
    );
}
