//! Regression test for issue #4271: Birthing Ritual must offer creatures up to
//! mana value (sacrificed creature's MV + 1), not always mana value 1.
//!
//! Previously, the chain ordered the dig choice BEFORE the sacrifice, so when
//! `selectable_cards` was computed, `effect_context_object` (the sacrificed
//! creature snapshot) was None. `ObjectScope::CostPaidObject` fell back to
//! `unwrap_or(0)`, yielding X = 0 + 1 = 1.
//!
//! Fixed in #4289: the sacrifice now precedes the PriorLook Dig in the chain.
//! When the filter is evaluated, `effect_context_object` holds the sacrificed
//! creature's snapshot, so X = sacrificed_mv + 1 and the player can choose any
//! creature with mana value ≤ X from among the looked-at cards.
//!
//! https://github.com/phase-rs/phase/issues/4271

use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::types::ability::EffectKind;
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;

const BIRTHING_RITUAL_ORACLE: &str = "At the beginning of your end step, if you control a creature, look at the top seven cards of your library. Then you may sacrifice a creature. If you do, you may put a creature card with mana value X or less from among those cards onto the battlefield, where X is 1 plus the sacrificed creature's mana value. Put the rest on the bottom of your library in a random order.";

/// Drive the game to the End step phase, waiting for Birthing Ritual's
/// beginning-of-end-step trigger to appear on the stack.
fn reach_end_step_with_trigger(runner: &mut GameRunner) {
    runner.advance_to_end_step();
    for _ in 0..32 {
        match runner.state().waiting_for.clone() {
            WaitingFor::DeclareAttackers { .. } => {
                runner
                    .act(GameAction::DeclareAttackers {
                        attacks: vec![],
                        bands: vec![],
                    })
                    .expect("empty attack");
            }
            WaitingFor::Priority { .. } if runner.state().phase == Phase::End => return,
            WaitingFor::Priority { .. } => runner.pass_both_players(),
            WaitingFor::OrderTriggers { .. } => {
                runner
                    .act(GameAction::OrderTriggers { order: vec![0] })
                    .ok();
            }
            _ if runner.state().phase == Phase::End => return,
            _ => runner.pass_both_players(),
        }
    }
}

/// Drive the game from End-step priority (with Birthing Ritual's trigger on
/// the stack) through accepting the optional sacrifice and the sacrifice
/// resolution, stopping just before or at `WaitingFor::DigChoice`.
///
/// Returns the `WaitingFor` state at the point when `DigChoice` is reached
/// (or panics if it isn't reached within the iteration budget).
fn drive_to_dig_choice(runner: &mut GameRunner) -> WaitingFor {
    for _ in 0..50 {
        match runner.state().waiting_for.clone() {
            // Already at DigChoice — done.
            WaitingFor::DigChoice { .. } => return runner.state().waiting_for.clone(),
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            WaitingFor::DeclareAttackers { .. } => {
                runner
                    .act(GameAction::DeclareAttackers {
                        attacks: vec![],
                        bands: vec![],
                    })
                    .expect("declare empty attackers");
            }
            WaitingFor::OrderTriggers { .. } => {
                runner
                    .act(GameAction::OrderTriggers { order: vec![0] })
                    .ok();
            }
            // "you may sacrifice a creature" — accept.
            WaitingFor::OptionalEffectChoice { .. } => {
                runner
                    .act(GameAction::DecideOptionalEffect { accept: true })
                    .expect("accept sacrifice");
            }
            // "choose a creature to sacrifice" — pick the first eligible.
            // With only one eligible creature this prompt may be skipped.
            WaitingFor::EffectZoneChoice {
                effect_kind: EffectKind::Sacrifice,
                cards,
                ..
            } => {
                let victim = cards
                    .first()
                    .copied()
                    .expect("at least one eligible creature");
                runner
                    .act(GameAction::SelectCards {
                        cards: vec![victim],
                    })
                    .expect("sacrifice creature");
            }
            _ => {
                runner.act(GameAction::PassPriority).ok();
            }
        }
    }
    panic!(
        "never reached WaitingFor::DigChoice; last state = {:?}",
        runner.state().waiting_for
    );
}

#[test]
fn birthing_ritual_cmc_filter_uses_sacrificed_creature_mana_value() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Birthing Ritual as a permanent enchantment.
    let _ritual = scenario
        .add_creature(P0, "Birthing Ritual", 0, 0)
        .as_enchantment()
        .from_oracle_text(BIRTHING_RITUAL_ORACLE)
        .id();

    // The creature P0 will sacrifice: mana value 3.
    // X = 3 + 1 = 4, so the filter must be CmcLE(4).
    let _victim = scenario
        .add_creature(P0, "Victim Creature", 3, 3)
        .with_mana_cost(ManaCost::generic(3))
        .id();

    // Top-7 library cards (added last = ends up on top):
    //   position 0 (top): MV5 Dragon   — creature, MV 5 > 4 → NOT selectable
    //   position 1:       MV4 Angel    — creature, MV 4 ≤ 4 → selectable
    //   positions 2-6:    non-creature fillers (not selectable regardless)
    for i in 0..5 {
        scenario.add_card_to_library_top(P0, &format!("Library Filler {i}"));
    }
    let mv4_angel = scenario
        .add_spell_to_library_top(P0, "MV4 Angel", false)
        .as_creature()
        .with_mana_cost(ManaCost::generic(4))
        .id();
    let mv5_dragon = scenario
        .add_spell_to_library_top(P0, "MV5 Dragon", false)
        .as_creature()
        .with_mana_cost(ManaCost::generic(5))
        .id();

    let mut runner = scenario.build();

    reach_end_step_with_trigger(&mut runner);
    assert_eq!(runner.state().phase, Phase::End, "must reach the end step");

    let dig_choice = drive_to_dig_choice(&mut runner);

    // The DigChoice must offer only the MV4 Angel; the MV5 Dragon exceeds
    // the bound X = (sacrificed MV 3) + 1 = 4.
    let WaitingFor::DigChoice {
        selectable_cards,
        cards,
        ..
    } = dig_choice
    else {
        panic!("expected DigChoice, got {dig_choice:?}");
    };

    assert_eq!(
        cards.len(),
        7,
        "all 7 looked-at cards must appear in the DigChoice"
    );
    assert!(
        selectable_cards.contains(&mv4_angel),
        "MV4 Angel (MV 4 ≤ X=4) must be selectable; selectable = {selectable_cards:?}"
    );
    assert!(
        !selectable_cards.contains(&mv5_dragon),
        "MV5 Dragon (MV 5 > X=4) must NOT be selectable; selectable = {selectable_cards:?}"
    );
    assert_eq!(
        selectable_cards.len(),
        1,
        "exactly one card (MV4 Angel) must be selectable; selectable = {selectable_cards:?}"
    );
}
