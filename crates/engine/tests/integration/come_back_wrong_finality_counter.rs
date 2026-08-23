//! CR 122.1h + CR 608.2c: a death-contingent theft against a finality counter.
//!
//! Come Back Wrong — "Destroy target creature. If a creature card is put into
//! a graveyard this way, return it to the battlefield under your control.
//! Sacrifice it at the beginning of your next end step."
//!
//! Reported from a real game: Balustrade Wurm had returned from the graveyard
//! via its Delirium ability, so it carried a finality counter (CR 122.1h: "If
//! this permanent would be put into a graveyard from the battlefield, exile it
//! instead."). The opponent's Come Back Wrong stole it anyway. The printed
//! return is contingent on the creature card actually being PUT INTO A
//! GRAVEYARD by the destruction — the finality replacement sends it to exile
//! instead, so the contingency never happened and the caster must get nothing.
//!
//! Oracle text verified against `client/public/card-data.json`.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::WaitingFor;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const COME_BACK_WRONG: &str = "Destroy target creature. If a creature card is put into a graveyard this way, return it to the battlefield under your control. Sacrifice it at the beginning of your next end step.";

struct Theft {
    prompts: Vec<String>,
    zone: Option<Zone>,
    controller: Option<engine::types::player::PlayerId>,
}

/// P0 casts Come Back Wrong at P1's bear and the resolution runs to an empty
/// stack. `with_finality` is the whole variable: the counter decides whether
/// the destruction's graveyard trip is replaced by exile (CR 122.1h).
fn resolve_theft(with_finality: bool) -> Theft {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let bear = scenario
        .add_creature_from_oracle(P1, "Doomed Bear", 2, 2, "")
        .id();
    if with_finality {
        scenario.with_counter(bear, CounterType::Finality, 1);
    }
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Come Back Wrong", false, COME_BACK_WRONG)
        .id();

    let mut runner = scenario.build();
    runner.cast(spell).commit();

    let mut prompts = Vec::new();
    let mut settled = false;
    for _ in 0..40 {
        let action = match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => {
                settled = true;
                break;
            }
            WaitingFor::Priority { .. } => GameAction::PassPriority,
            // The sorcery's single target slot: the bear is the only creature.
            WaitingFor::TargetSelection { target_slots, .. } => GameAction::SelectTargets {
                targets: target_slots
                    .iter()
                    .filter_map(|slot| slot.legal_targets.first().cloned())
                    .collect(),
            },
            other => {
                prompts.push(format!("PROMPT: {}", other.variant_name()));
                break;
            }
        };
        if runner.act(action).is_err() {
            break;
        }
    }
    assert!(
        settled,
        "the resolution must reach an empty stack — prompts seen: {prompts:?}"
    );

    let obj = runner
        .state()
        .objects
        .values()
        .find(|o| o.name == "Doomed Bear");
    Theft {
        prompts,
        zone: obj.map(|o| o.zone),
        controller: obj.map(|o| o.controller),
    }
}

/// The report's case: with a finality counter the graveyard trip becomes
/// exile (CR 122.1h), so "put into a graveyard this way" never happened —
/// the caster gets nothing and the creature stays in exile.
#[test]
fn a_finality_counter_denies_the_death_contingent_theft() {
    let theft = resolve_theft(true);
    assert_eq!(
        theft.zone,
        Some(Zone::Exile),
        "CR 122.1h: the destroyed creature must be exiled instead of dying, \
         and the graveyard-contingent return must not resolve — prompts seen: {:?}",
        theft.prompts
    );
}

/// The positive twin: without the counter the creature dies into the
/// graveyard, the contingency holds, and the caster steals it.
#[test]
fn without_a_finality_counter_the_theft_resolves() {
    let theft = resolve_theft(false);
    assert_eq!(
        (theft.zone, theft.controller),
        (Some(Zone::Battlefield), Some(P0)),
        "the plain destruction dies into the graveyard and the return puts the \
         creature onto the battlefield under the caster's control — prompts \
         seen: {:?}",
        theft.prompts
    );
}
