//! Issue #6924 — Moldervine Reclamation must trigger when a creature dies as a
//! COST, including a token sacrificed to Phyrexian Altar's mana ability.
//!
//! > Moldervine Reclamation: Whenever a creature you control dies, you gain 1
//! > life and draw a card.
//! > Phyrexian Altar: Sacrifice a creature: Add one mana of any color.
//!
//! CR 700.4 — "dies" means "is put into a graveyard from the battlefield".
//! CR 111.7 — a token in another zone ceases to exist as a state-based action,
//! and explicitly: "if a token changes zones, applicable triggered abilities
//! will trigger before the token ceases to exist". So the reporter's expectation
//! is rules-correct, not arguable.
//! CR 603.10a — abilities that trigger when a player sacrifices a permanent are
//! look-back triggers.
//! CR 605.3a — an activated mana ability may be activated when a mana payment
//! is required, including during casting or resolution. CR 605.3b — it does not
//! use the stack and resolves immediately after activation.
//!
//! This file is a DISCRIMINATOR, not a single repro. The report blames the
//! token, but that is only one of three candidate axes. The 2x2 below separates
//! them: if only the mana-ability rows fail, the defect is the mana-ability cost
//! path (not tokens); if only the token rows fail, it is tokens; if all
//! cost-sacrifice rows fail, it is cost-sacrifices generally.

use engine::game::scenario::{GameScenario, P0};
use engine::types::actions::GameAction;
use engine::types::game_state::{ManaChoice, PayCostKind, WaitingFor};
use engine::types::mana::ManaType;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

// Verbatim Oracle text (Scryfall, 2026-09-17).
const MOLDERVINE: &str = "Whenever a creature you control dies, you gain 1 life and draw a card.";
const PHYREXIAN_ALTAR: &str = "Sacrifice a creature: Add one mana of any color.";
const VISCERA_SEER: &str = "Sacrifice a creature: Scry 1.";

/// Which sacrifice outlet pays the cost. `ManaAbility` is the reported route
/// (Phyrexian Altar); `Ordinary` is the control that tells us whether the defect
/// is specific to mana abilities at all.
enum Outlet {
    ManaAbility,
    Ordinary,
}

/// Whether the sacrificed creature is a token. CR 111.7 makes a token cease to
/// exist after it dies, so the two arms need different "it really died" guards.
enum Victim {
    Token,
    Nontoken,
}

struct Outcome {
    life_delta: i32,
    hand_delta: i64,
    prompts: Vec<String>,
}

/// Whether Moldervine Reclamation is on the battlefield. `Absent` drives the
/// negative control: with no observer, both deltas must stay at zero, which is
/// what makes a passing positive arm mean anything at all.
enum Observer {
    Moldervine,
    Absent,
}

fn sacrifice_a_creature(outlet: Outlet, victim_kind: Victim) -> Outcome {
    sacrifice_a_creature_with(outlet, victim_kind, Observer::Moldervine)
}

fn sacrifice_a_creature_without_moldervine(outlet: Outlet, victim_kind: Victim) -> Outcome {
    sacrifice_a_creature_with(outlet, victim_kind, Observer::Absent)
}

fn sacrifice_a_creature_with(outlet: Outlet, victim_kind: Victim, observer: Observer) -> Outcome {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // The trigger draws a card, so the library must not be empty.
    scenario.with_library_top(P0, &["Draw One", "Draw Two"]);
    if matches!(observer, Observer::Moldervine) {
        scenario.add_enchantment_from_oracle(P0, "Moldervine Reclamation", MOLDERVINE);
    }

    let outlet_id = match outlet {
        Outlet::ManaAbility => scenario
            .add_artifact_from_oracle(P0, "Phyrexian Altar", PHYREXIAN_ALTAR)
            .id(),
        Outlet::Ordinary => scenario
            .add_creature_from_oracle(P0, "Viscera Seer", 1, 1, VISCERA_SEER)
            .id(),
    };
    let victim = scenario.add_creature(P0, "Victim Bear", 2, 2).id();

    let mut runner = scenario.build();
    if matches!(victim_kind, Victim::Token) {
        // No scenario builder exists for tokens; direct mutation is the house
        // idiom across the integration suite.
        runner
            .state_mut()
            .objects
            .get_mut(&victim)
            .expect("the victim exists")
            .is_token = true;
    }

    let life_before = runner.state().players[P0.0 as usize].life;
    let hand_before = runner.state().players[P0.0 as usize].hand.len() as i64;

    runner
        .act(GameAction::ActivateAbility {
            source_id: outlet_id,
            ability_index: 0,
        })
        .expect("reach-guard: the outlet's sacrifice ability must be activatable");

    let mut prompts = Vec::new();
    for _ in 0..24 {
        let waiting = runner.state().waiting_for.clone();
        prompts.push(format!("{waiting:?}").chars().take(60).collect::<String>());
        match waiting {
            WaitingFor::PayCost {
                kind: PayCostKind::Sacrifice,
                choices,
                ..
            } => {
                assert!(
                    choices.contains(&victim),
                    "reach-guard: the victim must be offered to the sacrifice cost; got {choices:?}"
                );
                runner
                    .act(GameAction::SelectCards {
                        cards: vec![victim],
                    })
                    .expect("sacrificing the chosen creature must succeed");
            }
            // Phyrexian Altar's Oracle text adds "one mana of any color". The
            // prompt was measured from the first run rather than assumed.
            WaitingFor::ChooseManaColor { .. } => {
                runner
                    .act(GameAction::ChooseManaColor {
                        choice: ManaChoice::SingleColor(ManaType::Black),
                        count: 1,
                    })
                    .expect("choosing the Altar's mana color must succeed");
            }
            // Viscera Seer's "Scry 1". Keeping the card on top leaves the library
            // untouched, so the scry cannot perturb the draw that Moldervine's
            // trigger performs — which is one of this test's two observables.
            WaitingFor::ScryChoice { cards, .. } => {
                runner
                    .act(GameAction::SelectCards { cards })
                    .expect("keeping the scryed card on top must succeed");
            }
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("passing priority must succeed");
            }
            // Deliberately loud: an unexpected state dumps the whole trail so a
            // run reports the real flow instead of silently doing nothing.
            other => panic!("unexpected prompt: {other:?}; trail so far: {prompts:?}"),
        }
    }

    runner.advance_until_stack_empty();

    // The sacrifice really happened. A token ceases to exist (CR 111.7), so the
    // nontoken arm checks the graveyard and the token arm checks only that it
    // left the battlefield.
    match victim_kind {
        Victim::Nontoken => assert_eq!(
            runner.state().objects[&victim].zone,
            Zone::Graveyard,
            "reach-guard: the sacrificed creature must be in the graveyard"
        ),
        Victim::Token => assert_ne!(
            runner.state().objects.get(&victim).map(|obj| obj.zone),
            Some(Zone::Battlefield),
            "reach-guard: the sacrificed token must have left the battlefield"
        ),
    }

    Outcome {
        life_delta: runner.state().players[P0.0 as usize].life - life_before,
        hand_delta: runner.state().players[P0.0 as usize].hand.len() as i64 - hand_before,
        prompts,
    }
}

fn assert_moldervine_triggered(outcome: &Outcome, case: &str) {
    assert_eq!(
        (outcome.life_delta, outcome.hand_delta),
        (1, 1),
        "CR 700.4 + CR 111.7: {case} — Moldervine Reclamation must gain 1 life AND \
         draw a card when a creature you control dies as a cost; prompts: {:?}",
        outcome.prompts
    );
}

/// The reported case: a TOKEN sacrificed to Phyrexian Altar's MANA ability.
#[test]
fn a_token_sacrificed_to_a_mana_ability_triggers_moldervine() {
    let outcome = sacrifice_a_creature(Outlet::ManaAbility, Victim::Token);
    assert_moldervine_triggered(&outcome, "token + mana ability (the reported case)");
}

/// Isolates the token axis: same mana ability, ordinary creature.
#[test]
fn a_nontoken_sacrificed_to_a_mana_ability_triggers_moldervine() {
    let outcome = sacrifice_a_creature(Outlet::ManaAbility, Victim::Nontoken);
    assert_moldervine_triggered(&outcome, "nontoken + mana ability");
}

/// Isolates the mana-ability axis: same token, ordinary activated ability.
#[test]
fn a_token_sacrificed_to_an_ordinary_ability_triggers_moldervine() {
    let outcome = sacrifice_a_creature(Outlet::Ordinary, Victim::Token);
    assert_moldervine_triggered(&outcome, "token + ordinary activated ability");
}

/// Control. If this fails, the harness is wrong rather than the engine.
#[test]
fn a_nontoken_sacrificed_to_an_ordinary_ability_triggers_moldervine() {
    let outcome = sacrifice_a_creature(Outlet::Ordinary, Victim::Nontoken);
    assert_moldervine_triggered(&outcome, "nontoken + ordinary activated ability (control)");
}

/// NEGATIVE control — the guard that makes the four positives mean anything.
///
/// The positive arms assert `(life, hand)` both moved by exactly 1. If ANYTHING
/// else in the fixture gained a life and drew a card, those arms would pass
/// without Moldervine Reclamation doing any work at all. Removing the enchantment
/// must take both deltas to zero; if it does not, the positives are vacuous and
/// no conclusion — least of all a closure recommendation — may be drawn from them.
#[test]
fn without_moldervine_a_cost_sacrifice_gains_no_life_and_draws_nothing() {
    let outcome = sacrifice_a_creature_without_moldervine(Outlet::ManaAbility, Victim::Token);
    assert_eq!(
        (outcome.life_delta, outcome.hand_delta),
        (0, 0),
        "negative control: with no Moldervine on the battlefield, sacrificing a token \
         to a mana ability must change neither life nor hand — otherwise the positive \
         arms prove nothing; prompts: {:?}",
        outcome.prompts
    );
}
