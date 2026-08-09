//! Doom's Time Platform: "Whenever you attack, exile target nonland card from
//! your graveyard with two time counters on it. If it doesn't have suspend, it
//! gains suspend."
//!
//! Pre-fix, "with two time counters on it" was consumed by `parse_target` as a
//! `FilterProp::Counters { GE, 2 }` requiring the *graveyard* card to already
//! hold two time counters. Per CR 122.2 a card in a graveyard has no counters,
//! so that filter is vacuous — the trigger had NO legal target and the counter
//! *placement* was dropped entirely. The fix (`split_counterless_enter_counters`)
//! recognizes the counterless origin zone and lifts the clause onto the exile's
//! `enter_with_counters` (the CR 702.62a suspend template), where the resolver
//! stamps the counters as the card enters Exile.
//!
//! This drives the real pipeline (declare attackers → YouAttack trigger →
//! ChangeZone resolution) and discriminates the fix:
//!   (a) the graveyard card is exiled (pre-fix it is not even a legal target);
//!   (b) it carries two time counters (CR 702.62a);
//!   (c) Doom's Time Platform itself stays on the battlefield.

use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::WaitingFor;
use engine::types::zones::Zone;
use engine::types::Phase;

const DOOMS_TIME_PLATFORM: &str = "Whenever you attack, exile target nonland card \
    from your graveyard with two time counters on it. If it doesn't have suspend, \
    it gains suspend.";

/// CR 122.2 + CR 702.62a: the graveyard card selected by Doom's Time Platform's
/// attack trigger must be exiled with two time counters — not filtered out for
/// lacking counters it can never have in a graveyard.
#[test]
fn dooms_time_platform_exiles_graveyard_card_with_two_time_counters() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Doom's Time Platform on P0's battlefield (its type is irrelevant to the
    // "you attack" trigger; the fixture uses a noncreature permanent so it is
    // not itself a candidate attacker).
    let platform = scenario
        .add_enchantment_from_oracle(P0, "Doom's Time Platform", DOOMS_TIME_PLATFORM)
        .id();

    // A nonland (creature) card in P0's own graveyard — the trigger's target.
    let graveyard_card = scenario
        .add_creature_to_graveyard(P0, "Grizzly Bear", 2, 2)
        .id();

    // A separate creature to attack with, firing "Whenever you attack".
    let attacker = scenario.add_creature(P0, "Runeclaw Bear", 2, 2).id();

    let mut runner = scenario.build();

    runner.advance_to_combat();
    runner
        .declare_attackers(&[(attacker, AttackTarget::Player(P1))])
        .expect("declare attacker to fire the you-attack trigger");

    // Drive the YouAttack trigger: choose the (only) legal graveyard target and
    // let it resolve. `choose_first_legal_target` panics pre-fix because the
    // spurious `Counters GE 2` filter leaves the graveyard card illegal.
    for _ in 0..40 {
        match runner.state().waiting_for.clone() {
            WaitingFor::OrderTriggers { .. } => {
                engine::game::triggers::drain_order_triggers_with_identity(runner.state_mut());
            }
            WaitingFor::TriggerTargetSelection { .. } | WaitingFor::TargetSelection { .. } => {
                runner.choose_first_legal_target().expect(
                    "graveyard card must be a legal target once the counter clause is lifted",
                );
            }
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() {
                    break;
                }
                if runner.act(GameAction::PassPriority).is_err() {
                    break;
                }
            }
            _ => break,
        }
    }

    let card = runner
        .state()
        .objects
        .get(&graveyard_card)
        .expect("graveyard card object must still exist");

    // (a) The graveyard card was exiled.
    assert_eq!(
        card.zone,
        Zone::Exile,
        "CR 400.7: the targeted graveyard card must move to Exile, got {:?}",
        card.zone,
    );
    assert!(
        runner.state().exile.contains(&graveyard_card),
        "the exiled card must be in the exile zone",
    );

    // (b) It carries exactly two time counters (CR 702.62a). Pre-fix this is 0
    // (the placement was dropped) — and the card would not even be a legal
    // target — so this assertion flips when the fix is reverted.
    let time = card.counters.get(&CounterType::Time).copied().unwrap_or(0);
    assert_eq!(
        time, 2,
        "CR 702.62a: the exiled card must enter with two time counters, got {time}",
    );

    // (c) Doom's Time Platform itself is untouched — it stays on the battlefield.
    assert!(
        runner.state().battlefield.contains(&platform),
        "Doom's Time Platform must remain on the battlefield, not exile itself",
    );
}
