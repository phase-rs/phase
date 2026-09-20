//! Carrot Cake (BLB) — a permanent's OWN "when you sacrifice it" trigger must
//! fire on its own sacrifice.
//!
//! > When this artifact enters and when you sacrifice it, create a 1/1 white
//! > Rabbit creature token and scry 1.
//! > {2}, {T}, Sacrifice this artifact: You gain 3 life.
//!
//! CR 603.10a — abilities that trigger when a player sacrifices a permanent
//! look back in time, so the sacrificed permanent's own trigger fires even
//! though the permanent is no longer on the battlefield once it resolves.
//! CR 400.7 — the card in the graveyard is a new object, so "it" has to be
//! answered from the permanent's last battlefield existence, never from the
//! graveyard card. CR 111.7 — a sacrificed token still triggers before it
//! ceases to exist.
//!
//! Field report: the Rabbit only ever came from the ETB half. Observer
//! triggers ("whenever you sacrifice an artifact") were already correct — only
//! the source's own trigger was lost. The matrix below separates the axes:
//!
//!   route \ victim                    card    token
//!   its own "Sacrifice this" cost     pass    pass
//!   another permanent's cost          pass    pass
//!   a resolving sacrifice effect      pass    pass
//!
//! plus a negative control: sacrificing a DIFFERENT artifact must not fire
//! the cake's own trigger.

use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::types::actions::GameAction;
use engine::types::game_state::{PayCostKind, WaitingFor};
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;
use engine::types::ObjectId;

// Verbatim Oracle text (Scryfall, 2026-09-19).
const CARROT_CAKE: &str = "When this artifact enters and when you sacrifice it, create a 1/1 white \
Rabbit creature token and scry 1. (Look at the top card of your library. You may put that card on the bottom.)\n\
{2}, {T}, Sacrifice this artifact: You gain 3 life.";
/// A plain artifact sacrifice outlet (ordinary activated ability, no mana).
const OUTLET: &str = "Sacrifice an artifact: You gain 1 life.";
/// An outlet whose RESOLUTION sacrifices (the Ordeal cycle's route: "sacrifice
/// Ordeal of Thassa" is an effect, not a cost).
const EFFECT_OUTLET: &str = "{T}: Sacrifice an artifact.";

enum Route {
    /// Carrot Cake's own "{2}, {T}, Sacrifice this artifact" cost.
    OwnCost,
    /// Another permanent's "Sacrifice an artifact" cost, choosing the cake.
    OtherOutlet,
    /// A resolving effect sacrifices the cake (CR 701.21a).
    Effect,
}

enum Victim {
    Card,
    /// CR 111.7: a token ceases to exist after it changes zones.
    Token,
}

struct Outcome {
    rabbits: usize,
    life_delta: i32,
    cake_zone: Option<Zone>,
    prompts: Vec<String>,
}

fn rabbits(runner: &GameRunner) -> usize {
    let state = runner.state();
    state
        .battlefield
        .iter()
        .filter(|id| {
            let obj = &state.objects[id];
            obj.name == "Rabbit" && obj.controller == P0
        })
        .count()
}

/// Sacrifices `sacrificed` through `route` and drives every prompt until the
/// stack is empty. `sacrificed` is the cake itself except in the negative
/// control, where the outlet sacrifices a different artifact.
fn run(route: Route, victim: Victim, sacrifice_other_artifact: bool) -> Outcome {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // Scry 1 needs a card to look at.
    scenario.with_library_top(P0, &["Top One", "Top Two"]);
    scenario.with_mana_pool(
        P0,
        (0..2)
            .map(|_| ManaUnit::new(ManaType::White, ObjectId(0), false, vec![]))
            .collect(),
    );
    // Pre-existing permanent: its ETB half is not in play here.
    let cake = scenario
        .add_artifact_from_oracle(P0, "Carrot Cake", CARROT_CAKE)
        .id();
    let other = scenario
        .add_artifact_from_oracle(P0, "Spare Artifact", "")
        .id();
    let outlet = match route {
        Route::OwnCost => None,
        Route::OtherOutlet => Some(
            scenario
                .add_creature_from_oracle(P0, "Outlet", 1, 1, OUTLET)
                .id(),
        ),
        Route::Effect => Some(
            scenario
                .add_creature_from_oracle(P0, "Effect Outlet", 1, 1, EFFECT_OUTLET)
                .id(),
        ),
    };

    let mut runner = scenario.build();
    if matches!(victim, Victim::Token) {
        // No scenario builder exists for tokens; direct mutation is the house
        // idiom across the integration suite.
        runner
            .state_mut()
            .objects
            .get_mut(&cake)
            .expect("the cake exists")
            .is_token = true;
    }
    let chosen = if sacrifice_other_artifact {
        other
    } else {
        cake
    };
    let life_before = runner.state().players[P0.0 as usize].life;

    let (source_id, ability_index) = match outlet {
        None => {
            let index = runner.state().objects[&cake]
                .abilities
                .iter()
                .position(|a| a.cost.is_some())
                .expect("reach-guard: the cake's sacrifice ability must be parsed");
            (cake, index)
        }
        Some(outlet) => (outlet, 0),
    };
    runner
        .act(GameAction::ActivateAbility {
            source_id,
            ability_index,
        })
        .expect("reach-guard: the sacrifice ability must be activatable");

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
                    choices.contains(&chosen),
                    "reach-guard: the chosen artifact must be offered; got {choices:?}"
                );
                runner
                    .act(GameAction::SelectCards {
                        cards: vec![chosen],
                    })
                    .expect("sacrificing the chosen artifact must succeed");
            }
            // The resolving "Sacrifice an artifact." effect's choice.
            WaitingFor::EffectZoneChoice { cards, .. } => {
                assert!(
                    cards.contains(&chosen),
                    "reach-guard: the chosen artifact must be offered; got {cards:?}"
                );
                runner
                    .act(GameAction::SelectCards {
                        cards: vec![chosen],
                    })
                    .expect("sacrificing the chosen artifact must succeed");
            }
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
            other => panic!("unexpected prompt: {other:?}; trail so far: {prompts:?}"),
        }
    }
    runner.advance_until_stack_empty();

    Outcome {
        rabbits: rabbits(&runner),
        life_delta: runner.state().players[P0.0 as usize].life - life_before,
        cake_zone: runner.state().objects.get(&cake).map(|obj| obj.zone),
        prompts,
    }
}

fn assert_cake_triggered(outcome: &Outcome, case: &str) {
    assert_ne!(
        outcome.cake_zone,
        Some(Zone::Battlefield),
        "reach-guard: {case} — the cake must have been sacrificed"
    );
    assert_eq!(
        outcome.rabbits, 1,
        "CR 603.10a: {case} — sacrificing Carrot Cake must create exactly one Rabbit; \
         prompts: {:?}",
        outcome.prompts
    );
}

#[test]
fn own_cost_card() {
    let outcome = run(Route::OwnCost, Victim::Card, false);
    assert_cake_triggered(&outcome, "its own cost, card");
    assert_eq!(outcome.cake_zone, Some(Zone::Graveyard));
    // The activated ability itself still resolves.
    assert_eq!(
        outcome.life_delta, 3,
        "the {{2}}, {{T}} ability gains 3 life"
    );
}

#[test]
fn own_cost_token() {
    let outcome = run(Route::OwnCost, Victim::Token, false);
    assert_cake_triggered(&outcome, "its own cost, token (CR 111.7)");
    assert_eq!(
        outcome.life_delta, 3,
        "the {{2}}, {{T}} ability gains 3 life"
    );
}

#[test]
fn other_outlet_card() {
    let outcome = run(Route::OtherOutlet, Victim::Card, false);
    assert_cake_triggered(&outcome, "another permanent's cost, card");
    assert_eq!(outcome.life_delta, 1, "the outlet gains 1 life");
}

#[test]
fn other_outlet_token() {
    let outcome = run(Route::OtherOutlet, Victim::Token, false);
    assert_cake_triggered(&outcome, "another permanent's cost, token (CR 111.7)");
}

/// Negative control: "when you sacrifice IT" is about the cake only.
#[test]
fn sacrificing_a_different_artifact_does_not_fire_the_cake() {
    let outcome = run(Route::OtherOutlet, Victim::Card, true);
    assert_eq!(
        outcome.cake_zone,
        Some(Zone::Battlefield),
        "reach-guard: the cake stays"
    );
    assert_eq!(
        outcome.rabbits, 0,
        "sacrificing another artifact must not fire the cake's own trigger; prompts: {:?}",
        outcome.prompts
    );
    assert_eq!(outcome.life_delta, 1, "reach-guard: the outlet resolved");
}

#[test]
fn effect_card() {
    let outcome = run(Route::Effect, Victim::Card, false);
    assert_cake_triggered(&outcome, "a resolving sacrifice effect, card");
}

#[test]
fn effect_token() {
    let outcome = run(Route::Effect, Victim::Token, false);
    assert_cake_triggered(&outcome, "a resolving sacrifice effect, token (CR 111.7)");
}
