//! CR 702.37b: megamorph's paid-turn-up counter rider, end to end.
//!
//! "Megamorph [cost]" means … "As this permanent is turned face up, put a
//! +1/+1 counter on it if its megamorph cost was paid to turn it face up."
//! Reported live: megamorph creatures came up counter-less. The rider is a
//! keyword-synthesized `TurnFaceUp` replacement gated on the payment fact the
//! PAID special action publishes — so it orders with any other
//! as-turned-face-up replacement (CR 616.1) and never fires for a free,
//! effect-driven turn-up.
//!
//! The printed costs are deliberately unpayable ({9}) so the face-down {3}
//! alternative (CR 708.4) is the only castable route.

use engine::game::scenario::{GameScenario, P0};
use engine::types::ability::{
    AbilityCost, AbilityDefinition, AbilityKind, Effect, ManaContribution, ManaProduction,
    ReplacementDefinition, TargetFilter, TurnUpCostSource,
};
use engine::types::actions::{AlternativeCastDecision, GameAction};
use engine::types::counter::CounterType;
use engine::types::game_state::WaitingFor;
use engine::types::game_state::{ManaAbilityResume, PendingCostMoveResume};
use engine::types::mana::ManaColor;
use engine::types::phase::Phase;
use engine::types::replacements::ReplacementEvent;
use engine::types::zones::EtbTapState;
use engine::types::zones::Zone;

struct FlipOutcome {
    prompts: Vec<String>,
    counters: Option<u32>,
    face_down: Option<bool>,
}

/// Cast the card face down (the only affordable route), settle, then take the
/// paid `TurnFaceUp` special action and settle again.
fn cast_face_down_and_flip(oracle_text: &str, mana: u32) -> FlipOutcome {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Hidden Test Creature", false, oracle_text)
        .as_creature()
        .id();
    scenario.with_mana_pool(
        P0,
        (0..mana)
            .map(|_| {
                engine::types::mana::ManaUnit::new(
                    engine::types::mana::ManaType::Green,
                    engine::types::identifiers::ObjectId(0),
                    false,
                    vec![],
                )
            })
            .collect(),
    );

    let mut runner = scenario.build();
    runner
        .cast(spell)
        .alternative_cast(AlternativeCastDecision::Alternative)
        .commit();

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
    assert!(settled, "the face-down cast must settle — {prompts:?}");

    let object_id = runner
        .state()
        .objects
        .values()
        .find(|o| o.zone == Zone::Battlefield && o.face_down)
        .map(|o| o.id)
        .expect("a face-down 2/2 must be on the battlefield");

    let flip = runner.act(GameAction::TurnFaceUp { object_id, x: 0 });
    assert!(flip.is_ok(), "the paid turn-up must be legal: {flip:?}");

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
    assert!(settled, "the flip must settle — {prompts:?}");

    let obj = runner.state().objects.get(&object_id);
    FlipOutcome {
        prompts,
        counters: obj.map(|o| {
            o.counters
                .get(&CounterType::Plus1Plus1)
                .copied()
                .unwrap_or(0)
        }),
        face_down: obj.map(|o| o.face_down),
    }
}

/// CR 702.37b: paying the megamorph cost to turn face up puts the counter.
#[test]
fn a_paid_megamorph_turn_up_puts_its_counter() {
    let outcome = cast_face_down_and_flip("Megamorph {2}", 5);
    assert_eq!(outcome.face_down, Some(false), "{:?}", outcome.prompts);
    assert_eq!(
        outcome.counters,
        Some(1),
        "CR 702.37b: the paid megamorph turn-up places exactly one +1/+1 \
         counter — prompts seen: {:?}",
        outcome.prompts
    );
}

/// CR 702.37: a plain morph turn-up has no counter rider.
#[test]
fn a_paid_morph_turn_up_puts_no_counter() {
    let outcome = cast_face_down_and_flip("Morph {2}", 5);
    assert_eq!(outcome.face_down, Some(false), "{:?}", outcome.prompts);
    assert_eq!(
        outcome.counters,
        Some(0),
        "CR 702.37: a plain morph turn-up places nothing — prompts seen: {:?}",
        outcome.prompts
    );
}

/// CR 702.37b + CR 616.1: a megamorph creature with an ADDITIONAL printed
/// "As … is turned face up" counter replacement — the synthesized rider rides
/// the same pipeline, so the two order together and BOTH apply. The paid flip
/// pauses on the ordering prompt; answering it completes the flip with
/// 1 (rider) + 5 (printed) counters.
#[test]
fn the_rider_orders_with_a_printed_turn_up_replacement() {
    let outcome = cast_face_down_and_flip(
        "Megamorph {2}\nAs this creature is turned face up, put five +1/+1 counters on it.",
        5,
    );
    assert_eq!(outcome.face_down, Some(false), "{:?}", outcome.prompts);
    assert_eq!(
        outcome.counters,
        Some(6),
        "CR 702.37b + CR 616.1: both as-turned-face-up replacements apply \
         (1 + 5) — prompts seen: {:?}",
        outcome.prompts
    );
}

/// CR 702.37b: the rider fires only "if its megamorph cost was paid to turn
/// it face up" — an EFFECT that turns the creature face up publishes no
/// payment fact, so no counter.
#[test]
fn an_effect_driven_turn_up_puts_no_counter() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let creature = scenario
        .add_spell_to_hand_from_oracle(P0, "Hidden Test Creature", false, "Megamorph {2}")
        .as_creature()
        .id();
    let opener = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Test Opener",
            false,
            "Turn target face-down creature face up.",
        )
        .id();
    scenario.with_mana_pool(
        P0,
        (0..6)
            .map(|_| {
                engine::types::mana::ManaUnit::new(
                    engine::types::mana::ManaType::Green,
                    engine::types::identifiers::ObjectId(0),
                    false,
                    vec![],
                )
            })
            .collect(),
    );

    let mut runner = scenario.build();
    runner
        .cast(creature)
        .alternative_cast(AlternativeCastDecision::Alternative)
        .commit();
    let mut prompts = Vec::new();
    for _ in 0..40 {
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
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
    let object_id = runner
        .state()
        .objects
        .values()
        .find(|o| o.zone == Zone::Battlefield && o.face_down)
        .map(|o| o.id)
        .expect("a face-down 2/2 must be on the battlefield");

    runner.cast(opener).commit();
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
            WaitingFor::TargetSelection { target_slots, .. } => {
                let targets: Vec<_> = target_slots
                    .iter()
                    .filter_map(|slot| slot.legal_targets.first().cloned())
                    .collect();
                if runner.act(GameAction::SelectTargets { targets }).is_err() {
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
        "the opener must resolve — prompts seen: {prompts:?}"
    );

    let obj = &runner.state().objects[&object_id];
    assert!(!obj.face_down, "the effect turned it face up");
    assert_eq!(
        obj.counters
            .get(&CounterType::Plus1Plus1)
            .copied()
            .unwrap_or(0),
        0,
        "CR 702.37b: no megamorph cost was paid, so no counter — prompts: {prompts:?}"
    );
}

/// CR 702.37b + CR 605.3b: the SELECTED cost source survives a PAUSED
/// payment. A mana source whose own cost self-exiles, under two competing
/// exile→graveyard replacements, pauses the auto-tap (CR 616.1); the typed
/// continuation carries `cost_source: Megamorph` exactly like the locked
/// cost, and the resumed flip still places the counter.
#[test]
fn a_paused_megamorph_payment_still_places_the_counter() {
    fn redirect_exile_to_graveyard() -> ReplacementDefinition {
        ReplacementDefinition::new(ReplacementEvent::Moved)
            .destination_zone(Zone::Exile)
            .execute(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::ChangeZone {
                    destination: Zone::Graveyard,
                    origin: None,
                    target: TargetFilter::SelfRef,
                    owner_library: false,
                    enter_transformed: false,
                    enters_under: None,
                    enter_tapped: EtbTapState::Unspecified,
                    enters_attacking: false,
                    up_to: false,
                    enter_with_counters: vec![],
                    conditional_enter_with_counters: vec![],
                    face_down_profile: None,
                    enters_modified_if: None,
                },
            ))
    }

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let id = scenario
        .add_creature_to_hand_from_oracle(P0, "Hidden Test Creature", 3, 3, "Megamorph {R}")
        .id();
    let _source = scenario
        .add_creature(P0, "Self-Exiling Mana Source", 1, 1)
        .with_ability_definition(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Mana {
                    produced: ManaProduction::Fixed {
                        colors: vec![ManaColor::Red],
                        contribution: ManaContribution::Base,
                    },
                    restrictions: vec![],
                    grants: vec![],
                    expiry: None,
                    target: None,
                },
            )
            .cost(AbilityCost::Composite {
                costs: vec![
                    AbilityCost::Tap,
                    AbilityCost::Exile {
                        count: 1,
                        zone: None,
                        filter: Some(TargetFilter::SelfRef),
                    },
                ],
            }),
        )
        .id();
    for name in ["First Pause Replacement", "Second Pause Replacement"] {
        scenario
            .add_creature(P0, name, 0, 0)
            .as_enchantment()
            .with_replacement_definition(redirect_exile_to_graveyard());
    }
    let mut runner = scenario.build();

    let mut events = Vec::new();
    engine::game::morph::play_face_down(runner.state_mut(), P0, id, &mut events)
        .expect("the card is played face down");

    let paused = runner
        .act(GameAction::TurnFaceUp {
            object_id: id,
            x: 0,
        })
        .expect("the source's own cost pauses the payment rather than failing it");
    assert!(
        matches!(paused.waiting_for, WaitingFor::ReplacementChoice { .. }),
        "the mana source's exile replacement owns the window, got {:?}",
        paused.waiting_for
    );
    // The typed continuation carries the SELECTED source, locked at initiation.
    assert!(
        matches!(
            runner.state().pending_cost_move_resume.as_ref(),
            Some(PendingCostMoveResume::ManaAbilityPayment { pending, .. })
                if matches!(
                    &pending.resume,
                    ManaAbilityResume::TurnFaceUp {
                        cost_source: TurnUpCostSource::Megamorph,
                        ..
                    }
                )
        ),
        "the paused continuation must carry the megamorph classification"
    );
    assert!(runner.state().objects[&id].face_down);

    runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("the replacement choice is answered");
    let mut prompts = Vec::new();
    for _ in 0..40 {
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
            WaitingFor::Priority { .. } => {
                if runner.act(GameAction::PassPriority).is_err() {
                    break;
                }
            }
            WaitingFor::ReplacementChoice { .. } => {
                if runner
                    .act(GameAction::ChooseReplacement { index: 0 })
                    .is_err()
                {
                    break;
                }
            }
            other => {
                prompts.push(format!("PROMPT: {}", other.variant_name()));
                break;
            }
        }
    }

    let obj = &runner.state().objects[&id];
    assert!(
        !obj.face_down,
        "the resumed payment committed the flip — {prompts:?}"
    );
    assert_eq!(
        obj.counters
            .get(&CounterType::Plus1Plus1)
            .copied()
            .unwrap_or(0),
        1,
        "CR 702.37b: the carried megamorph classification places the counter \
         after the paused payment — prompts: {prompts:?}"
    );
}
