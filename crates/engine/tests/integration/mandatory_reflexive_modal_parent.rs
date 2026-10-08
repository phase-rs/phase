//! CR 603.12: a mandatory instruction printed before a reflexive
//! "When you do" whose body is a mode list.
//!
//! Cemetery Desecrator — "When this creature enters or dies, exile another card
//! from a graveyard. When you do, choose one — • Remove X counters from target
//! permanent, where X is the mana value of the exiled card. • Target creature an
//! opponent controls gets -X/-X until end of turn, where X is the mana value of
//! the exiled card."
//!
//! Reported from a real game with every graveyard empty: the enters trigger
//! asked for a mode although there was no card anywhere to exile. Reading the
//! parse showed the cause is upstream of that symptom — the exile instruction
//! was not merely ungated, it was absent. The triggered-modal dispatch keyed the
//! whole reflexive decision on the printed `"you may "` marker, so a mandatory
//! parent classified as "no reflexive here" and the mode list replaced the
//! instruction outright. The card never exiled anything, in any game state.
//!
//! Oracle text verified against `client/public/card-data.json`.
//!
//! CR references (verified against docs/MagicCompRules.txt):
//! - CR 603.12: a reflexive triggered ability triggers "based on whether the
//!   trigger event or events occurred earlier during the resolution of the
//!   spell or ability that created them".
//! - CR 700.2b: a modal triggered ability chooses its mode(s) as it is put on
//!   the stack.
//!
//! The third test measures the CR 603.12 gate itself: with every graveyard
//! empty the mandatory instruction runs, moves nothing, and the reflexive is
//! never created — issue #7511's remaining half, answered from the parent's
//! own event slice at the sub-walk site in `resolve_ability_chain`
//! (`when_you_do_mandatory_parent_did_nothing`).
//!
//! The `RemoveCounter` pair at the end walks the same gate over the
//! event-witness branch (`GameEvent::CounterRemoved`) through the production
//! resolver — review round 1 asked for exactly this runtime pin, in both
//! directions.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

/// Copied verbatim from `client/public/card-data.json`, newlines included —
/// the mode list is line-separated there, and joining it onto one line is not
/// the printed text the parser is given at runtime.
const DESECRATOR: &str = "When this creature enters or dies, exile another card from a graveyard. When you do, choose one —\n• Remove X counters from target permanent, where X is the mana value of the exiled card.\n• Target creature an opponent controls gets -X/-X until end of turn, where X is the mana value of the exiled card.";

struct Resolution {
    prompts: Vec<String>,
    exiled: usize,
    graveyard: usize,
}

/// Cast Cemetery Desecrator and record what its enters trigger did.
///
/// `graveyard_fodder` is the whole variable: one card sitting in the opponent's
/// graveyard is the only thing "exile another card from a graveyard" can reach.
fn resolve_enters(graveyard_fodder: bool) -> Resolution {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // The opponent's creature is the legal target for the second mode. Without
    // it the mode could be skipped for want of a target, which would hide the
    // behavior rather than measure it.
    scenario.add_creature_from_oracle(P1, "Warded Bear", 2, 2, "Ward {2}");
    if graveyard_fodder {
        scenario.with_graveyard(P1, &["Fodder Card"]);
    }
    let desecrator = scenario
        .add_spell_to_hand_from_oracle(P0, "Cemetery Desecrator", false, DESECRATOR)
        .as_creature()
        .id();
    for _ in 0..10 {
        scenario.add_card_to_library_top(P0, "Plains");
    }

    let mut runner = scenario.build();
    runner.cast(desecrator).commit();

    let mut prompts = Vec::new();
    let mut settled = false;
    for _ in 0..40 {
        let (label, action) = match runner.state().waiting_for.clone() {
            // Priority with an empty stack is the resting state: the spell, its
            // enters trigger and the reflexive it created have all finished.
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => {
                settled = true;
                break;
            }
            WaitingFor::Priority { .. } => (None, GameAction::PassPriority),
            WaitingFor::AbilityModeChoice { .. } => (
                Some("AbilityModeChoice".to_string()),
                GameAction::SelectModes { indices: vec![1] },
            ),
            // Answer every slot with its first legal target so the chosen mode
            // actually resolves. Recording the prompt and stopping here would
            // leave the reflexive half-resolved and prove only that a question
            // was asked.
            WaitingFor::TriggerTargetSelection { target_slots, .. }
            | WaitingFor::TargetSelection { target_slots, .. } => {
                let targets: Vec<_> = target_slots
                    .iter()
                    .filter_map(|slot| slot.legal_targets.first().cloned())
                    .collect();
                (
                    Some("TargetSelection".to_string()),
                    GameAction::SelectTargets { targets },
                )
            }
            // CR 702.21a: the chosen mode targets the opponent's warded
            // creature. Declining the ward cost counters that mode and lets the
            // resolution finish, which is what makes the settled state
            // reachable without turning this file into a ward test.
            WaitingFor::UnlessPayment { .. } => (
                Some("UnlessPayment".to_string()),
                GameAction::PayUnlessCost { pay: false },
            ),
            other => {
                prompts.push(format!("PROMPT: {}", other.variant_name()));
                break;
            }
        };
        if let Some(label) = label {
            prompts.push(label);
        }
        if runner.act(action).is_err() {
            break;
        }
    }
    assert!(
        settled,
        "the resolution must reach an empty stack — prompts seen: {prompts:?}"
    );

    // Count only the fodder card: the Desecrator itself is on the battlefield
    // and the library cards never moved, so a zone census over the named card
    // cannot be satisfied by anything else the resolution touched.
    let count_in = |zone: Zone| {
        runner
            .state()
            .objects
            .values()
            .filter(|o| o.name == "Fodder Card" && o.zone == zone)
            .count()
    };
    Resolution {
        prompts,
        exiled: count_in(Zone::Exile),
        graveyard: count_in(Zone::Graveyard),
    }
}

/// CR 603.12: the printed instruction before the reflexive connector is a real
/// instruction and must be performed.
///
/// Counter-probe: with the `Mandatory` arm removed from
/// `classify_reflexive_modal_parent` this line reads `left: (0, 1)` against
/// `right: (1, 0)` — the card exiles nothing and the fodder stays in the
/// graveyard, which is exactly the shipped behavior this fixes.
#[test]
fn the_mandatory_instruction_before_a_mode_list_is_performed() {
    let resolved = resolve_enters(true);
    assert_eq!(
        (resolved.exiled, resolved.graveyard),
        (1, 0),
        "\"exile another card from a graveyard\" must move the one reachable card \
         out of the graveyard and into exile — prompts seen: {:?}",
        resolved.prompts
    );
}

/// The mode list must still be offered once the instruction has run — the fix
/// re-parents the modal, it does not remove it.
///
/// This is the positive counter-direction: a fix that suppressed the modal
/// altogether would satisfy the test above and break the card a second way.
#[test]
fn the_mode_list_still_resolves_after_the_instruction() {
    let resolved = resolve_enters(true);
    assert!(
        resolved.prompts.iter().any(|p| p == "AbilityModeChoice"),
        "the reflexive mode choice must still be offered — {:?}",
        resolved.prompts
    );
}

/// With nothing to exile, the instruction runs, moves no card — and the
/// reflexive is never created.
///
/// CR 603.12: a reflexive triggered ability triggers "based on whether the
/// trigger event or events occurred earlier during the resolution". With every
/// graveyard empty the mandatory "exile another card from a graveyard" exiles
/// nothing, so "when you do" never happened: no mode choice may be offered.
/// This closes issue #7511's remaining half (the optional-parent side landed
/// in #7414).
#[test]
fn an_impossible_exile_creates_no_reflexive() {
    let resolved = resolve_enters(false);
    assert!(
        !resolved
            .prompts
            .iter()
            .any(|p| p == "AbilityModeChoice" || p == "TargetSelection"),
        "CR 603.12: the mandatory exile did nothing, so the reflexive mode \
         choice must never be offered — prompts seen: {:?}",
        resolved.prompts
    );
    assert_eq!(
        (resolved.exiled, resolved.graveyard),
        (0, 0),
        "no graveyard held a card, so nothing may be exiled — prompts seen: {:?}",
        resolved.prompts
    );
}

/// Oracle text for the `RemoveCounter` pair below: a MANDATORY
/// self-referential counter removal ahead of a reflexive draw — Vhal, Scholar
/// of Mortality's "remove all study counters from it. When you do, …" shape
/// reduced to its building blocks. No mode list and no targets, so the whole
/// chain resolves without a single player choice and the only observable is
/// the outcome itself.
const COUNTER_SCHOLAR: &str =
    "When this creature enters, remove a +1/+1 counter from it. When you do, draw a card.";

/// As above, plus the counter the mandatory removal needs — the one-variable
/// positive twin.
const COUNTER_SCHOLAR_STOCKED: &str = "This creature enters with a +1/+1 counter on it.\nWhen this creature enters, remove a +1/+1 counter from it. When you do, draw a card.";

struct CounterOutcome {
    prompts: Vec<String>,
    plains_in_hand: usize,
    counters_on_scholar: Option<u32>,
}

/// Cast the scholar and let its enters chain resolve with no interaction.
///
/// The census reads only what the chain can move: Plains stay in the library
/// unless the reflexive draws one, and the +1/+1 counter count on the scholar
/// is the mandatory instruction's own footprint.
fn resolve_counter_scholar(oracle_text: &str) -> CounterOutcome {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let scholar = scenario
        .add_spell_to_hand_from_oracle(P0, "Counter Scholar", false, oracle_text)
        .as_creature()
        .id();
    for _ in 0..10 {
        scenario.add_card_to_library_top(P0, "Plains");
    }

    let mut runner = scenario.build();
    runner.cast(scholar).commit();

    let mut prompts = Vec::new();
    let mut settled = false;
    for _ in 0..40 {
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => {
                settled = true;
                break;
            }
            WaitingFor::Priority { .. } => {
                if runner.act(GameAction::PassPriority).is_err() {
                    break;
                }
            }
            other => {
                prompts.push(format!("PROMPT: {}", other.variant_name()));
                break;
            }
        }
    }
    assert!(
        settled,
        "the chain must resolve without interaction — prompts seen: {prompts:?}"
    );

    let state = runner.state();
    CounterOutcome {
        prompts,
        plains_in_hand: state
            .objects
            .values()
            .filter(|o| o.name == "Plains" && o.zone == Zone::Hand)
            .count(),
        counters_on_scholar: state
            .objects
            .values()
            .find(|o| o.name == "Counter Scholar" && o.zone == Zone::Battlefield)
            .map(|o| {
                o.counters
                    .get(&engine::types::counter::CounterType::Plus1Plus1)
                    .copied()
                    .unwrap_or(0)
            }),
    }
}

/// The witness side of the runtime pair from review round 1: the counter is
/// there, the mandatory removal happens, and the reflexive must survive the
/// event-witness gate.
///
/// This half fails if the `GameEvent::CounterRemoved` witness is not threaded
/// into the parent's event slice at the sub-walk site — silencing the working
/// chain is the failure mode the gate must not have.
#[test]
fn a_performed_counter_removal_keeps_its_reflexive() {
    let outcome = resolve_counter_scholar(COUNTER_SCHOLAR_STOCKED);
    assert_eq!(
        (outcome.plains_in_hand, outcome.counters_on_scholar),
        (1, Some(0)),
        "the counter came off, so the reflexive must draw — prompts seen: {:?}",
        outcome.prompts
    );
}

/// The suppression side: no counter to remove, the mandatory instruction runs
/// and moves nothing — CR 603.12: "when you do" never happened, so the
/// reflexive draw must not resolve.
///
/// Counter-probe: with the sub-walk gate
/// (`when_you_do_mandatory_parent_did_nothing`) reverted, this line reads
/// `left: (1, Some(0))` against `right: (0, Some(0))` — exactly the shipped
/// behavior this PR fixes, now pinned through the production resolver.
#[test]
fn an_impossible_counter_removal_creates_no_reflexive() {
    let outcome = resolve_counter_scholar(COUNTER_SCHOLAR);
    assert_eq!(
        (outcome.plains_in_hand, outcome.counters_on_scholar),
        (0, Some(0)),
        "nothing was removed, so nothing may be drawn (CR 603.12) — prompts \
         seen: {:?}",
        outcome.prompts
    );
}
