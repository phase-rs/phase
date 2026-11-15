//! CR 613.1f + CR 602.2: an Elf card in your graveyard grants Thranduil its
//! activated ability. The legal action, cost, and effect all use Thranduil as
//! the activation source.

use engine::ai_support::{legal_actions, legal_actions_full};
use engine::game::layers::{flush_layers, mark_layers_full};
use engine::game::scenario::{GameScenario, P0};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::{
    AbilityCost, AbilityDefinition, AbilityKind, ActivationRestriction, Effect, EffectKind,
    EffectScope, QuantityExpr, TapStateChange, TargetFilter, TargetRef, TypedFilter,
};
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::mana::{ManaCost, ManaType};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

use crate::support::shared_card_db;

/// Complete Oracle text from `data/mtgjson/AtomicCards.json` (2026-09-22).
const THRANDUIL: &str =
    "Thranduil has all activated abilities of all Elf cards in your graveyard.\n\
Whenever another legendary Elf you control enters, draw two cards, then discard a card.";

#[test]
fn thranduil_activates_graveyard_elf_ability_as_its_own() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let thranduil = scenario
        .add_creature(P0, "Thranduil, the Elvenking", 5, 6)
        .with_subtypes(vec!["Elf", "Noble"])
        .from_oracle_text(THRANDUIL)
        .id();

    // Synthetic Elf donor isolates the grant: its typed fixture ability is
    // {T}: You gain 2 life. The donor remains in the graveyard throughout.
    let donor_ability = AbilityDefinition::new(
        AbilityKind::Activated,
        Effect::GainLife {
            amount: QuantityExpr::Fixed { value: 2 },
            player: TargetFilter::Controller,
        },
    )
    .cost(AbilityCost::Tap);
    let donor = scenario
        .add_creature_to_graveyard(P0, "Test Elf Donor", 1, 1)
        .with_subtypes(vec!["Elf"])
        .with_ability_definition(donor_ability)
        .id();
    let mut runner = scenario.build();

    mark_layers_full(runner.state_mut());
    flush_layers(runner.state_mut());
    let offered = legal_actions(runner.state());
    let ability_index = offered
        .iter()
        .find_map(|action| match action {
            GameAction::ActivateAbility {
                source_id,
                ability_index,
                ..
            } if *source_id == thranduil => Some(*ability_index),
            _ => None,
        })
        .expect("Thranduil's granted ability must be offered as its own legal action");

    let outcome = runner.activate(thranduil, ability_index).resolve();
    assert!(outcome.state().objects[&thranduil].tapped);
    assert!(!outcome.state().objects[&donor].tapped);
    assert_eq!(outcome.state().objects[&donor].zone, Zone::Graveyard);
    assert_eq!(outcome.life_delta(P0), 2);
    assert!(outcome.events().iter().any(|event| matches!(
        event,
        GameEvent::EffectResolved {
            kind: EffectKind::GainLife,
            source_id,
            ..
        } if *source_id == thranduil
    )));
}

/// CR 113.2c: Thranduil + two identically-worded Quirion Rangers
/// ("Return a Forest you control to its owner's hand: Untap target creature.
/// Activate only once each turn.") in the graveyard yields TWO independent
/// Untap instances — each activates separately for its own Forest cost. Real
/// printed cards throughout via the committed fixture. Reverting the provider
/// head stamp collapses the pair to one (RED: the reach-guard finds one).
#[test]
fn thranduil_grants_two_independent_quirion_ranger_activations() {
    let Some(db) = shared_card_db() else { return };
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let thranduil = scenario.add_real_card(P0, "Thranduil, the Elvenking", Zone::Battlefield, db);
    scenario.add_real_card(P0, "Quirion Ranger", Zone::Graveyard, db);
    scenario.add_real_card(P0, "Quirion Ranger", Zone::Graveyard, db);
    let forest1 = scenario.add_real_card(P0, "Forest", Zone::Battlefield, db);
    let forest2 = scenario.add_real_card(P0, "Forest", Zone::Battlefield, db);
    // A third Forest keeps the second activation's bounce cost a real choice:
    // with a single Forest left the engine auto-pays it without prompting.
    scenario.add_real_card(P0, "Forest", Zone::Battlefield, db);
    let bear_a = scenario.add_real_card(P0, "Grizzly Bears", Zone::Battlefield, db);
    let bear_b = scenario.add_real_card(P0, "Grizzly Bears", Zone::Battlefield, db);
    let mut runner = scenario.build();
    mark_layers_full(runner.state_mut());
    flush_layers(runner.state_mut());
    for bear in [bear_a, bear_b] {
        runner
            .state_mut()
            .objects
            .get_mut(&bear)
            .expect("untap target on the battlefield")
            .tapped = true;
    }

    // Reach-guard: exactly two granted Untap instances on Thranduil.
    let indices: Vec<usize> = runner.state().objects[&thranduil]
        .abilities
        .iter()
        .enumerate()
        .filter(|(_, ability)| {
            ability.kind == AbilityKind::Activated
                && ability
                    .description
                    .as_deref()
                    .is_some_and(|text| text.contains("Untap"))
        })
        .map(|(index, _)| index)
        .collect();
    assert_eq!(
        indices.len(),
        2,
        "CR 113.2c: Thranduil must hold one Untap instance per Quirion Ranger; got {:?}",
        runner.state().objects[&thranduil].abilities
    );

    // First instance through the driver: bounce Forest 1, untap bear A.
    let outcome = runner
        .activate(thranduil, indices[0])
        .target_object(bear_a)
        .pay_with(&[forest1])
        .resolve();
    assert!(
        !outcome.state().objects[&bear_a].tapped,
        "bear A must be untapped by the first instance"
    );
    assert_eq!(
        outcome.zone_of(forest1),
        Zone::Hand,
        "Forest 1 must pay the first instance's bounce cost"
    );
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "first activation must resolve to Priority, got {:?}",
        outcome.final_waiting_for()
    );
    assert!(
        outcome.state().stack.is_empty(),
        "first activation must leave an empty stack"
    );

    // Second instance driven manually so both prompts are asserted
    // unconditionally, in announcement order (CR 601.2c targets, then CR
    // 601.2h costs): the creature-target prompt, then the Forest cost prompt.
    runner
        .act(GameAction::ActivateAbility {
            source_id: thranduil,
            ability_index: indices[1],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("the second Untap instance must announce");
    match &runner.state().waiting_for {
        WaitingFor::TargetSelection { selection, .. } => {
            assert!(
                selection
                    .current_legal_targets
                    .contains(&TargetRef::Object(bear_b)),
                "bear B must be a legal untap target, got {:?}",
                selection.current_legal_targets
            );
        }
        other => panic!("the second activation must prompt for its creature target, got {other:?}"),
    }
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(bear_b)),
        })
        .expect("bear B must be choosable");
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::PayCost { .. }),
        "the second activation must prompt for its Forest cost, got {:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::SelectCards {
            cards: vec![forest2],
        })
        .expect("Forest 2 must pay the bounce cost");
    while !runner.state().stack.is_empty() {
        runner
            .act(GameAction::PassPriority)
            .expect("resolution must drain to Priority");
    }
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "second activation must resolve to Priority, got {:?}",
        runner.state().waiting_for
    );
    assert!(
        !runner.state().objects[&bear_b].tapped,
        "bear B must be untapped by the second instance"
    );
    assert_eq!(
        runner.state().objects[&forest2].zone,
        Zone::Hand,
        "Forest 2 must pay the second instance's bounce cost"
    );
}

/// CR 113.2c: Thranduil + two Llanowar Elves ("{T}: Add {G}.") in the
/// graveyard yields TWO "{T}: Add {G}" entries in the grouped legal-action
/// map; activating the first produces {G} and taps Thranduil while both
/// donors stay untapped in the graveyard. Real printed cards throughout via
/// the committed fixture. Reverting the provider head stamp leaves one entry
/// (RED).
#[test]
fn thranduil_grants_two_independent_llanowar_elves_mana_abilities() {
    let Some(db) = shared_card_db() else { return };
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let thranduil = scenario.add_real_card(P0, "Thranduil, the Elvenking", Zone::Battlefield, db);
    let elf1 = scenario.add_real_card(P0, "Llanowar Elves", Zone::Graveyard, db);
    let elf2 = scenario.add_real_card(P0, "Llanowar Elves", Zone::Graveyard, db);
    let mut runner = scenario.build();
    mark_layers_full(runner.state_mut());
    flush_layers(runner.state_mut());

    // Reach-guard through the grouped map: two ActivateAbility entries for
    // Thranduil (the grouped map carries mana abilities, unlike the flat list).
    let (_, _, grouped) = legal_actions_full(runner.state());
    let activations: Vec<usize> = grouped
        .get(&thranduil)
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|action| match action {
            GameAction::ActivateAbility {
                source_id,
                ability_index,
                ..
            } if *source_id == thranduil => Some(*ability_index),
            _ => None,
        })
        .collect();
    assert_eq!(
        activations.len(),
        2,
        "CR 113.2c: Thranduil must offer one {{T}}: Add {{G}} per Llanowar Elves; got {activations:?}"
    );
    assert_ne!(
        activations[0], activations[1],
        "the two instances must be distinct ability indices"
    );

    // Activate the first through the public action path.
    runner
        .act(GameAction::ActivateAbility {
            source_id: thranduil,
            ability_index: activations[0],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("the first mana instance must activate");
    assert!(
        runner.state().objects[&thranduil].tapped,
        "Thranduil must tap for its donated mana ability"
    );
    for elf in [elf1, elf2] {
        assert!(
            !runner.state().objects[&elf].tapped,
            "the graveyard donor must stay untapped"
        );
        assert_eq!(
            runner.state().objects[&elf].zone,
            Zone::Graveyard,
            "the donor must stay in the graveyard"
        );
    }
    assert_eq!(
        runner.state().players[P0.0 as usize]
            .mana_pool
            .count_color(ManaType::Green),
        1,
        "the first instance must produce exactly one green mana"
    );
}

/// CR 602.2a + CR 117.1b + CR 701.59a: the pending-activation guard is keyed
/// by the announcement-bound usage key, not the slot index.
///
/// Thranduil holds two grants: a synthetic collect-evidence Untap (slot 0)
/// and a real Quirion Ranger Untap (slot 1) — same description, different
/// bodies and providers. Activating slot 0 exiles its own provider (the
/// synthetic Elf) as the evidence; the layer recompute withdraws that grant
/// and the Ranger grant shifts 1 -> 0, into the recorded slot. This test
/// covers the shift mechanics end to end (pending records the
/// announcement-bound key pre-pass; the grant withdraws; the survivor shifts
/// and stays offered post-resolution). Guard discrimination — a keyed entry
/// not suppressing the shifted survivor — is pinned by the phase-ai unit test
/// `pending_key_guard_still_offers_ability_shifted_into_recorded_slot`, since
/// the passes below clear pending before the final offer assertion.
///
/// Quirion Ranger Oracle (verified vs Scryfall 2026-11-08): "Return a Forest
/// you control to its owner's hand: Untap target creature. Activate only
/// once each turn."
///
/// Synthetic twin: no printed card exhibits a collect-evidence Untap on an
/// Elf grantable by Thranduil, so the test says so and names the nearest
/// printed cards — Quirion Ranger (the gate + untap shape) and Cryptex
/// (`{T}, Collect evidence 3`, the activated collect-evidence cost shape).
#[test]
fn pending_key_does_not_suppress_shifted_survivor_after_provider_exile() {
    let Some(db) = shared_card_db() else { return };
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let thranduil = scenario.add_real_card(P0, "Thranduil, the Elvenking", Zone::Battlefield, db);
    // The synthetic donor goes to the graveyard FIRST so its grant lands at
    // the lower slot.
    let mut evidence_ability = AbilityDefinition::new(
        AbilityKind::Activated,
        Effect::SetTapState {
            target: TargetFilter::Typed(TypedFilter::creature()),
            scope: EffectScope::Single,
            state: TapStateChange::Untap,
        },
    );
    evidence_ability.cost = Some(AbilityCost::CollectEvidence { amount: 2 });
    evidence_ability.activation_restrictions = vec![ActivationRestriction::OnlyOnceEachTurn];
    evidence_ability.description = Some(
        "Return a Forest you control to its owner's hand: Untap target creature. Activate only once each turn."
            .to_string(),
    );
    let donor = scenario
        .add_creature_to_graveyard(P0, "Test Evidence Elf", 1, 1)
        .with_subtypes(vec!["Elf"])
        .with_mana_cost(ManaCost::generic(2))
        .with_ability_definition(evidence_ability)
        .id();
    let ranger = scenario.add_real_card(P0, "Quirion Ranger", Zone::Graveyard, db);
    let bear = scenario.add_real_card(P0, "Grizzly Bears", Zone::Battlefield, db);
    // A Forest keeps the surviving Ranger grant payable (its bounce cost), so
    // the final offer assertion is not confounded by payability.
    scenario.add_real_card(P0, "Forest", Zone::Battlefield, db);
    let mut runner = scenario.build();
    mark_layers_full(runner.state_mut());
    flush_layers(runner.state_mut());
    runner
        .state_mut()
        .objects
        .get_mut(&bear)
        .expect("untap target on the battlefield")
        .tapped = true;

    // Reach-guard: two grants in insertion order — collect-evidence at 0, the
    // Ranger at 1 — from distinct providers.
    let abilities = &runner.state().objects[&thranduil].abilities;
    assert_eq!(
        abilities.len(),
        2,
        "Thranduil must hold both grants, got {:?}",
        runner.state().objects[&thranduil].abilities
    );
    assert!(
        matches!(abilities[0].cost, Some(AbilityCost::CollectEvidence { .. })),
        "slot 0 must be the evidence grant, got {:?}",
        abilities[0].cost
    );
    assert!(
        matches!(abilities[1].cost, Some(AbilityCost::ReturnToHand { .. })),
        "slot 1 must be the Ranger grant, got {:?}",
        abilities[1].cost
    );
    assert_ne!(
        abilities[0].granting_object, abilities[1].granting_object,
        "the twins must have distinct providers"
    );

    // The twins share the description; only the fingerprint tells them apart.
    let ce_key = engine::game::restrictions::ability_usage_key(runner.state(), thranduil, 0)
        .expect("the evidence grant resolves before its cost is paid");
    let ranger_key = engine::game::restrictions::ability_usage_key(runner.state(), thranduil, 1)
        .expect("the Ranger grant resolves");
    assert_eq!(ce_key.description, ranger_key.description);
    assert_ne!(
        ce_key.fingerprint, ranger_key.fingerprint,
        "the twins must differ in body for the fingerprint to discriminate"
    );

    // Paired reach-guard: the evidence grant is offered before its cost.
    let offered: Vec<usize> = legal_actions(runner.state())
        .into_iter()
        .filter_map(|action| match action {
            GameAction::ActivateAbility {
                source_id,
                ability_index,
                ..
            } if source_id == thranduil => Some(ability_index),
            _ => None,
        })
        .collect();
    assert!(
        offered.contains(&0),
        "the evidence grant must be offered before its cost is paid, got {offered:?}"
    );

    // Announce through the public action path, answering each prompt
    // unconditionally as the pipeline presents it (CR 602.2b routes the
    // remainder through the spell-casting steps): the evidence-cost prompt
    // and the creature-target prompt, in pipeline order.
    runner
        .act(GameAction::ActivateAbility {
            source_id: thranduil,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("the evidence grant must announce");
    let mut saw_evidence = false;
    let mut saw_target = false;
    for _ in 0..4 {
        match &runner.state().waiting_for {
            WaitingFor::CollectEvidenceChoice {
                cards,
                minimum_mana_value,
                ..
            } => {
                assert!(
                    cards.contains(&donor),
                    "its own provider must be eligible evidence, got {cards:?}"
                );
                assert_eq!(*minimum_mana_value, 2, "the cost collects evidence 2");
                runner
                    .act(GameAction::SelectCards { cards: vec![donor] })
                    .expect("the provider must pay as the evidence");
                saw_evidence = true;
            }
            WaitingFor::TargetSelection { selection, .. } => {
                assert!(
                    selection
                        .current_legal_targets
                        .contains(&TargetRef::Object(bear)),
                    "bear must be a legal untap target, got {:?}",
                    selection.current_legal_targets
                );
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(TargetRef::Object(bear)),
                    })
                    .expect("bear must be choosable");
                saw_target = true;
            }
            WaitingFor::Priority { .. } => break,
            other => panic!("unexpected prompt during the evidence activation, got {other:?}"),
        }
    }
    assert!(
        saw_evidence && saw_target,
        "the activation must prompt for both evidence and target (evidence={saw_evidence}, target={saw_target})"
    );

    // Placement: the stack holds the activation and pending records exactly
    // the announcement-bound key — captured before the cost exiled the
    // provider, never re-derived from the (now stale) slot.
    assert_eq!(
        runner.state().stack.len(),
        1,
        "the ability must be placed after its cost is paid"
    );
    assert_eq!(
        runner.state().pending_activations,
        vec![ce_key.clone()],
        "pending must record the announcement-bound evidence key"
    );

    // Drain to Priority (passes clear the per-window guard), then recompute:
    // the exiled provider's grant is withdrawn and the Ranger shifts into
    // the recorded slot.
    while !runner.state().stack.is_empty() {
        runner
            .act(GameAction::PassPriority)
            .expect("resolution must drain to Priority");
    }
    mark_layers_full(runner.state_mut());
    flush_layers(runner.state_mut());

    assert_eq!(
        runner.state().objects[&donor].zone,
        Zone::Exile,
        "the evidence cost must exile the provider"
    );
    let abilities = &runner.state().objects[&thranduil].abilities;
    assert_eq!(
        abilities.len(),
        1,
        "the exiled provider's grant must be withdrawn, got {:?}",
        runner.state().objects[&thranduil].abilities
    );
    assert!(
        matches!(abilities[0].cost, Some(AbilityCost::ReturnToHand { .. })),
        "the surviving Ranger grant must shift into slot 0, got {:?}",
        abilities[0].cost
    );
    assert!(
        !runner.state().objects[&bear].tapped,
        "bear must be untapped by the resolved evidence grant"
    );
    assert!(
        runner.state().stack.is_empty(),
        "the activation must leave an empty stack"
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "the activation must resolve to Priority, got {:?}",
        runner.state().waiting_for
    );
    assert!(
        runner.state().pending_activations.is_empty(),
        "passes clear the per-window guard"
    );

    // Post-resolution availability: the shifted survivor stays offered.
    // Pending is empty here (asserted above), so this does not discriminate
    // the guard — that is the phase-ai unit test's job (see doc comment).
    let offered: Vec<usize> = legal_actions(runner.state())
        .into_iter()
        .filter_map(|action| match action {
            GameAction::ActivateAbility {
                source_id,
                ability_index,
                ..
            } if source_id == thranduil => Some(ability_index),
            _ => None,
        })
        .collect();
    assert!(
        offered.contains(&0),
        "the shifted Ranger grant must still be offered, got {offered:?}"
    );
    assert_eq!(
        runner.state().objects[&ranger].zone,
        Zone::Graveyard,
        "the survivor's provider must stay in the graveyard"
    );
}
