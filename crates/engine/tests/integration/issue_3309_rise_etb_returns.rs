//! Issue #3309 — Rise of the Dark Realms crash when resolving ETB return triggers.
//!
//! https://github.com/phase-rs/phase/issues/3309
//!
//! Reproduces mass reanimation (Rise of the Dark Realms) followed by simultaneous
//! ETB triggers including an optional graveyard-return (Sun Titan class) and
//! observer triggers (Soul Warden class).

use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const RISE_ORACLE: &str =
    "Put all creature cards from all graveyards onto the battlefield under your control.";

// Verbatim Sun Titan Oracle text (Scryfall, api.scryfall.com/cards/named?exact=Sun+Titan).
// The trigger is the compound `Whenever this creature enters or attacks` — parsing the real
// text yields `TriggerMode::EntersOrAttacks`, so both the ETB and the attack branch feed the
// graveyard-scoped return. A fabricated ETB-only const would silently leave the attack half
// unpinned (issue #5674).
const SUN_TITAN_ORACLE: &str =
    "Vigilance\nWhenever this creature enters or attacks, you may return target permanent card with mana value 3 or less from your graveyard to the battlefield.";

const SOUL_WARDEN_ORACLE: &str = "Whenever another creature enters, you gain 1 life.";

// Verbatim current Oracle (MTGJSON and independent Scryfall reading).
const TECHNOMANCER_ORACLE: &str = "When this creature enters, mill three cards, then return any number of artifact creature cards with total mana value 6 or less from your graveyard to the battlefield.";

fn technomancer_scenario() -> (GameScenario, engine::types::identifiers::ObjectId) {
    use engine::types::identifiers::ObjectId;
    use engine::types::mana::{ManaCostShard, ManaType, ManaUnit};
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let technomancer = scenario
        .add_creature_to_hand_from_oracle(P0, "Technomancer", 5, 1, TECHNOMANCER_ORACLE)
        .as_artifact_creature()
        .with_mana_cost(ManaCost::Cost {
            generic: 5,
            shards: vec![ManaCostShard::Black, ManaCostShard::Black],
        })
        .id();
    let mut mana = vec![ManaUnit::new(ManaType::Black, ObjectId(9999), false, vec![]); 2];
    mana.extend(vec![
        ManaUnit::new(
            ManaType::Colorless,
            ObjectId(9999),
            false,
            vec![]
        );
        5
    ]);
    scenario.with_mana_pool(P0, mana);
    (scenario, technomancer)
}

#[test]
fn technomancer_rejects_total_ten_atomically_then_returns_exactly_six() {
    use engine::game::engine::EngineError;
    use engine::types::events::GameEvent;
    use engine::types::mana::ManaCostShard;
    let (mut scenario, technomancer) = technomancer_scenario();
    let tomb = scenario
        .add_creature_to_graveyard(P0, "Canoptek Tomb Sentinel", 4, 3)
        .as_artifact_creature()
        .with_mana_cost(ManaCost::generic(4))
        .id();
    let hexmark = scenario
        .add_creature_to_graveyard(P0, "Hexmark Destroyer", 6, 6)
        .as_artifact_creature()
        .with_mana_cost(ManaCost::Cost {
            generic: 4,
            shards: vec![ManaCostShard::Black, ManaCostShard::Black],
        })
        .id();
    let opponent = scenario
        .add_creature_to_graveyard(P1, "Memnite", 1, 1)
        .as_artifact_creature()
        .with_mana_cost(ManaCost::zero())
        .id();
    let freshly_milled = scenario
        .add_spell_to_library_top(P0, "Myr Retriever", false)
        .as_creature()
        .as_artifact_creature()
        .with_mana_cost(ManaCost::generic(2))
        .id();
    let misses = [
        scenario.add_card_to_library_top(P0, "Plains"),
        scenario.add_card_to_library_top(P0, "Island"),
    ];
    let mut runner = scenario.build();
    {
        let retriever = runner.state_mut().objects.get_mut(&freshly_milled).unwrap();
        retriever.power = Some(1);
        retriever.base_power = Some(1);
        retriever.toughness = Some(1);
        retriever.base_toughness = Some(1);
    }
    let outcome = runner.cast(technomancer).resolve();
    outcome.assert_zone(&[technomancer], Zone::Battlefield);
    outcome.assert_zone(&[freshly_milled, misses[0], misses[1]], Zone::Graveyard);
    assert_eq!(
        outcome
            .events()
            .iter()
            .filter(|event| matches!(event, GameEvent::Milled { player_id: P0, .. }))
            .count(),
        3
    );
    let WaitingFor::ChooseFromZoneChoice { cards, .. } = outcome.final_waiting_for() else {
        panic!("must reach the real resolution choice after milling");
    };
    assert!(cards.contains(&tomb) && cards.contains(&hexmark) && cards.contains(&freshly_milled));
    assert!(!cards.contains(&opponent));
    let prompt = runner.state().waiting_for.clone();
    // CR 608.2d + CR 202.3: 4+6 exceeds the whole-set bound; no card moves.
    for selected in [vec![tomb, hexmark], vec![tomb, tomb], vec![opponent]] {
        assert!(matches!(
            runner.act(GameAction::SelectCards { cards: selected }),
            Err(EngineError::InvalidAction(_))
        ));
        assert_eq!(runner.state().waiting_for, prompt);
        for id in [tomb, hexmark, freshly_milled, opponent] {
            assert_eq!(runner.state().objects[&id].zone, Zone::Graveyard);
        }
    }
    runner
        .act(GameAction::SelectCards {
            cards: vec![tomb, freshly_milled],
        })
        .unwrap();
    assert_eq!(runner.state().objects[&tomb].zone, Zone::Battlefield);
    assert_eq!(
        runner.state().objects[&freshly_milled].zone,
        Zone::Battlefield
    );
    assert_eq!(runner.state().objects[&hexmark].zone, Zone::Graveyard);
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
}

#[test]
fn technomancer_single_six_and_empty_library_use_the_whole_graveyard() {
    let (mut scenario, technomancer) = technomancer_scenario();
    let hexmark = scenario
        .add_creature_to_graveyard(P0, "Hexmark Destroyer", 6, 6)
        .as_artifact_creature()
        .with_mana_cost(ManaCost::Cost {
            generic: 4,
            shards: vec![engine::types::mana::ManaCostShard::Black; 2],
        })
        .id();
    let mut runner = scenario.build();
    let outcome = runner.cast(technomancer).resolve();
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::ChooseFromZoneChoice { cards, .. }
        if cards.contains(&hexmark))
    );
    runner
        .act(GameAction::SelectCards {
            cards: vec![hexmark],
        })
        .unwrap();
    assert_eq!(runner.state().objects[&hexmark].zone, Zone::Battlefield);
}

#[test]
fn technomancer_zero_mana_value_is_not_a_cardinality_budget_and_seven_is_illegal() {
    let (mut scenario, technomancer) = technomancer_scenario();
    let zeros: Vec<_> = (0..7)
        .map(|_| {
            scenario
                .add_creature_to_graveyard(P0, "Memnite", 1, 1)
                .as_artifact_creature()
                .with_mana_cost(ManaCost::zero())
                .id()
        })
        .collect();
    let seven = scenario
        .add_creature_to_graveyard(P0, "Myr Enforcer", 4, 4)
        .as_artifact_creature()
        .with_mana_cost(ManaCost::generic(7))
        .id();
    let mut runner = scenario.build();
    let outcome = runner.cast(technomancer).resolve();
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::ChooseFromZoneChoice { cards, .. }
        if zeros.iter().all(|id| cards.contains(id)) && cards.contains(&seven))
    );
    assert!(runner
        .act(GameAction::SelectCards { cards: vec![seven] })
        .is_err());
    assert_eq!(runner.state().objects[&seven].zone, Zone::Graveyard);
    runner
        .act(GameAction::SelectCards {
            cards: zeros.clone(),
        })
        .unwrap();
    for id in zeros {
        assert_eq!(runner.state().objects[&id].zone, Zone::Battlefield);
    }
}

#[test]
fn technomancer_decline_and_empty_pool_finish_without_stale_parent_targets() {
    for with_candidate in [false, true] {
        let (mut scenario, technomancer) = technomancer_scenario();
        let candidate = with_candidate.then(|| {
            scenario
                .add_creature_to_graveyard(P0, "Memnite", 1, 1)
                .as_artifact_creature()
                .with_mana_cost(ManaCost::zero())
                .id()
        });
        let milled: Vec<_> = ["Plains", "Island", "Swamp"]
            .iter()
            .map(|name| scenario.add_card_to_library_top(P0, name))
            .collect();
        let mut runner = scenario.build();
        let outcome = runner.cast(technomancer).resolve();
        outcome.assert_zone(&milled, Zone::Graveyard);
        if let Some(id) = candidate {
            assert!(
                matches!(outcome.final_waiting_for(), WaitingFor::ChooseFromZoneChoice { cards, .. }
                if cards.contains(&id))
            );
            runner
                .act(GameAction::SelectCards { cards: vec![] })
                .unwrap();
            assert_eq!(runner.state().objects[&id].zone, Zone::Graveyard);
        }
        assert_eq!(
            runner.state().objects[&technomancer].zone,
            Zone::Battlefield
        );
        for id in milled {
            assert_eq!(runner.state().objects[&id].zone, Zone::Graveyard);
        }
        assert!(runner.state().stack.is_empty());
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { .. }
        ));
    }
}

/// Explicitly synthetic continuation-boundary fixture, not a named card's
/// Oracle. CR 608.2c/d delivers the chosen set; CR 608.2k preserves the
/// entering creature as the independent final instruction's referent.
#[test]
fn zone_choice_delivery_returns_selected_card_then_exiles_entering_source() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_creature_to_hand_from_oracle(
            P0,
            "Zone Choice Boundary Fixture",
            1,
            1,
            "When this creature enters, return any number of artifact creature cards with total mana value 6 or less from your graveyard to the battlefield. Then exile that creature.",
        )
        .with_mana_cost(ManaCost::zero())
        .id();
    let selected = scenario
        .add_creature_to_graveyard(P0, "Memnite", 1, 1)
        .as_artifact_creature()
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();
    let outcome = runner.cast(source).resolve();
    outcome.assert_zone(&[source], Zone::Battlefield);
    outcome.assert_zone(&[selected], Zone::Graveyard);
    assert!(matches!(
        outcome.final_waiting_for(),
        WaitingFor::ChooseFromZoneChoice { cards, .. } if cards.contains(&selected)
    ));
    runner
        .act(GameAction::SelectCards {
            cards: vec![selected],
        })
        .unwrap();
    assert_eq!(runner.state().objects[&selected].zone, Zone::Battlefield);
    assert_eq!(runner.state().objects[&source].zone, Zone::Exile);
    assert!(runner.state().stack.is_empty());
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
}

const KARMIC_GUIDE_ORACLE: &str =
    "Flying, echo {3}{W}{W}\nWhen Karmic Guide enters, return target creature card with mana value 3 or less from your graveyard to the battlefield.";

/// Sun Titan's real trigger is the compound `Whenever this creature enters or attacks`
/// (CR 603.6 — the parser unifies both events under `TriggerMode::EntersOrAttacks`), so the
/// graveyard-return effect is shared by the ETB *and* the attack branch. Parsing the verbatim
/// Scryfall text through `parse_oracle_text` (the entry point `database::synthesis` uses to
/// build card-data.json) is what keeps this test unable to go green while the shipped card is
/// broken: a fabricated ETB-only const would parse to `TriggerMode::ChangesZone` and pass.
#[test]
fn sun_titan_parses_enters_or_attacks_graveyard_return() {
    use engine::parser::oracle::parse_oracle_text;
    use engine::types::ability::Effect;
    use engine::types::triggers::TriggerMode;

    let parsed = parse_oracle_text(
        SUN_TITAN_ORACLE,
        "Sun Titan",
        &[],
        &["Creature".to_string()],
        &[],
    );
    let trigger = parsed
        .triggers
        .iter()
        .find(|t| t.mode == TriggerMode::EntersOrAttacks)
        .expect("Sun Titan must parse as an EntersOrAttacks trigger, not a plain ETB");
    let execute = trigger
        .execute
        .as_ref()
        .expect("EntersOrAttacks trigger must have an execute ability");
    match execute.effect.as_ref() {
        Effect::ChangeZone {
            origin: Some(Zone::Graveyard),
            destination: Zone::Battlefield,
            ..
        } => {}
        other => panic!("Sun Titan return must be graveyard→battlefield ChangeZone, got {other:?}"),
    }
    assert!(
        execute.optional,
        "\"you may return\" makes the return optional"
    );
    // No stray plain-ETB trigger should exist — the compound must not have been split.
    assert!(
        !parsed
            .triggers
            .iter()
            .any(|t| t.mode == TriggerMode::ChangesZone),
        "compound enters-or-attacks trigger must not split into a separate ETB trigger"
    );
}

#[test]
fn rise_of_dark_realms_reanimates_opponent_owned_creature() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let p0_creature = scenario
        .add_creature_to_graveyard(P0, "P0 Zombie", 2, 2)
        .id();
    let p1_creature = scenario
        .add_creature_to_graveyard(P1, "P1 Zombie", 2, 2)
        .id();

    let rise = scenario
        .add_spell_to_hand_from_oracle(P0, "Rise of the Dark Realms", false, RISE_ORACLE)
        .with_mana_cost(ManaCost::zero())
        .id();

    let mut runner = scenario.build();
    runner.cast(rise).resolve();

    assert_eq!(runner.state().objects[&p0_creature].zone, Zone::Battlefield);
    assert_eq!(
        runner.state().objects[&p1_creature].zone,
        Zone::Battlefield,
        "opponent-owned creature must not be exiled by mass reanimation"
    );
}

#[test]
fn rise_of_dark_realms_mandatory_etb_skips_when_graveyard_has_no_creature_targets() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let karmic = scenario
        .add_creature_to_graveyard(P0, "Karmic Guide", 2, 2)
        .from_oracle_text(KARMIC_GUIDE_ORACLE)
        .id();

    // Only other creature in graveyard — Rise reanimates it before Karmic's ETB
    // can target it, leaving zero legal creature cards in graveyard.
    let _other = scenario
        .add_creature_to_graveyard(P0, "Grizzly Bears", 2, 2)
        .id();

    let rise = scenario
        .add_spell_to_hand_from_oracle(P0, "Rise of the Dark Realms", false, RISE_ORACLE)
        .with_mana_cost(ManaCost::zero())
        .id();

    let mut runner = scenario.build();
    runner.cast(rise).resolve();

    let mut guard = 0;
    while guard < 64 {
        guard += 1;
        match &runner.state().waiting_for {
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).expect("pass");
            }
            WaitingFor::OrderTriggers { .. } => {
                engine::game::triggers::drain_order_triggers_with_identity(runner.state_mut());
            }
            other => panic!("unexpected prompt: {other:?}"),
        }
    }

    assert_eq!(runner.state().objects[&karmic].zone, Zone::Battlefield);
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
}

#[test]
fn rise_of_dark_realms_mandatory_etb_return_resolves_without_crash() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let karmic = scenario
        .add_creature_to_graveyard(P0, "Karmic Guide", 2, 2)
        .from_oracle_text(KARMIC_GUIDE_ORACLE)
        .id();

    let returnee = scenario
        .add_creature_to_graveyard(P0, "Grizzly Bears", 2, 2)
        .id();

    let rise = scenario
        .add_spell_to_hand_from_oracle(P0, "Rise of the Dark Realms", false, RISE_ORACLE)
        .with_mana_cost(ManaCost::zero())
        .id();

    let mut runner = scenario.build();

    let outcome = runner.cast(rise).target_objects(&[returnee]).resolve();

    // Drive any remaining trigger prompts the cast driver stopped at.
    let mut guard = 0;
    while guard < 64 {
        guard += 1;
        match &runner.state().waiting_for {
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).expect("pass");
            }
            WaitingFor::TriggerTargetSelection { .. } => {
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(TargetRef::Object(returnee)),
                    })
                    .expect("choose return target");
            }
            WaitingFor::OrderTriggers { .. } => {
                engine::game::triggers::drain_order_triggers_with_identity(runner.state_mut());
            }
            other => panic!("unexpected prompt: {other:?}"),
        }
    }

    assert_eq!(runner.state().objects[&karmic].zone, Zone::Battlefield);
    assert_eq!(runner.state().objects[&returnee].zone, Zone::Battlefield);
    assert!(matches!(
        outcome.final_waiting_for(),
        WaitingFor::Priority { .. }
    ));
}

#[test]
fn issue_3309_rise_karmic_guide_etb_chain_advances_to_priority() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let karmic = scenario
        .add_creature_to_graveyard(P0, "Karmic Guide", 2, 2)
        .from_oracle_text(KARMIC_GUIDE_ORACLE)
        .id();
    let returnee = scenario
        .add_creature_to_graveyard(P0, "Grizzly Bears", 2, 2)
        .id();

    let rise = scenario
        .add_spell_to_hand_from_oracle(P0, "Rise of the Dark Realms", false, RISE_ORACLE)
        .with_mana_cost(ManaCost::zero())
        .id();

    let mut runner = scenario.build();
    runner.cast(rise).target_objects(&[returnee]).resolve();

    let mut guard = 0;
    while guard < 64 {
        guard += 1;
        match &runner.state().waiting_for {
            WaitingFor::TriggerTargetSelection { .. } => {
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(TargetRef::Object(returnee)),
                    })
                    .expect("choose return target");
            }
            WaitingFor::OrderTriggers { .. } => {
                engine::game::triggers::drain_order_triggers_with_identity(runner.state_mut());
            }
            _ => break,
        }
    }
    runner.advance_until_stack_empty();

    assert_eq!(runner.state().objects[&karmic].zone, Zone::Battlefield);
    assert_eq!(runner.state().objects[&returnee].zone, Zone::Battlefield);
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
}

#[test]
fn rise_of_dark_realms_optional_etb_return_with_observers_resolves() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_life(P0, 20);

    let sun_titan = scenario
        .add_creature_to_graveyard(P0, "Sun Titan", 6, 6)
        .from_oracle_text(SUN_TITAN_ORACLE)
        .id();

    // Artifact stays in graveyard — Rise only reanimates creatures.
    let sol_ring = scenario
        .add_creature_to_graveyard(P0, "Sol Ring", 0, 0)
        .with_mana_cost(ManaCost::generic(1))
        .id();

    let soul_warden = scenario
        .add_creature_to_graveyard(P1, "Soul Warden", 1, 1)
        .from_oracle_text(SOUL_WARDEN_ORACLE)
        .id();

    let rise = scenario
        .add_spell_to_hand_from_oracle(P0, "Rise of the Dark Realms", false, RISE_ORACLE)
        .with_mana_cost(ManaCost::zero())
        .id();

    let mut runner = scenario.build();
    {
        use engine::types::card_type::CoreType;
        let obj = runner.state_mut().objects.get_mut(&sol_ring).unwrap();
        obj.card_types.core_types = vec![CoreType::Artifact];
        obj.base_card_types = obj.card_types.clone();
        obj.power = None;
        obj.toughness = None;
        obj.base_power = None;
        obj.base_toughness = None;
    }
    let life_before = runner.state().players[P0.0 as usize].life;

    runner
        .cast(rise)
        .accept_optional()
        .target_objects(&[sol_ring])
        .resolve();

    let mut guard = 0;
    while guard < 64 {
        guard += 1;
        match &runner.state().waiting_for {
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).expect("pass");
            }
            WaitingFor::OptionalEffectChoice { .. } => {
                runner
                    .act(GameAction::DecideOptionalEffect { accept: true })
                    .expect("accept optional return");
            }
            WaitingFor::TriggerTargetSelection { .. } => {
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(TargetRef::Object(sol_ring)),
                    })
                    .expect("choose graveyard return target");
            }
            WaitingFor::OrderTriggers { .. } => {
                engine::game::triggers::drain_order_triggers_with_identity(runner.state_mut());
            }
            other => panic!("unexpected prompt: {other:?}"),
        }
    }

    assert_eq!(runner.state().objects[&sun_titan].zone, Zone::Battlefield);
    assert_eq!(
        runner.state().objects[&soul_warden].zone,
        Zone::Battlefield,
        "Soul Warden must be reanimated, not exiled"
    );
    assert_eq!(
        runner.state().objects[&sol_ring].zone,
        Zone::Battlefield,
        "Sun Titan ETB must return Sol Ring from graveyard"
    );
    assert!(
        runner.state().players[P0.0 as usize].life > life_before,
        "Soul Warden must grant life when co-reanimated creatures enter"
    );
}

/// CR 508.1a + CR 603.6: the attack branch of Sun Titan's `EntersOrAttacks` trigger, feeding a
/// graveyard-scoped target slot end-to-end. The ETB branch is exercised above via reanimation;
/// this pins the *attack* half, which parsing the real Oracle text (issue #5674) first makes
/// possible — an ETB-only const would never produce an attack trigger to fire here. Reverting
/// the const to the fabricated ETB-only text makes this test fail: no attack trigger fires, so
/// the graveyard creature is never returned.
#[test]
fn sun_titan_attack_branch_returns_graveyard_permanent() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Sun Titan already on the battlefield (not summoning-sick) so it can attack this turn.
    let sun_titan = scenario
        .add_creature(P0, "Sun Titan", 6, 6)
        .from_oracle_text(SUN_TITAN_ORACLE)
        .id();

    // Permanent card with mana value 3 or less in Sun Titan's controller's graveyard.
    let returnee = scenario
        .add_creature_to_graveyard(P0, "Grizzly Bears", 2, 2)
        .with_mana_cost(ManaCost::generic(2))
        .id();

    let mut runner = scenario.build();
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(sun_titan, AttackTarget::Player(P1))])
        .expect("declare Sun Titan as attacker");

    // Drive the attack trigger: it targets a graveyard permanent as it goes on the stack
    // (TriggerTargetSelection) and offers the optional "you may" on resolution
    // (OptionalEffectChoice), then returns the card graveyard→battlefield.
    let mut guard = 0;
    while guard < 64 {
        guard += 1;
        if runner.state().objects[&returnee].zone == Zone::Battlefield {
            break;
        }
        match &runner.state().waiting_for {
            WaitingFor::TriggerTargetSelection { .. } => {
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(TargetRef::Object(returnee)),
                    })
                    .expect("choose graveyard return target");
            }
            WaitingFor::OptionalEffectChoice { .. } => {
                runner
                    .act(GameAction::DecideOptionalEffect { accept: true })
                    .expect("accept optional attack-triggered return");
            }
            WaitingFor::OrderTriggers { .. } => {
                engine::game::triggers::drain_order_triggers_with_identity(runner.state_mut());
            }
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() {
                    break;
                }
                runner.act(GameAction::PassPriority).expect("pass");
            }
            other => panic!("unexpected prompt while resolving attack trigger: {other:?}"),
        }
    }

    assert_eq!(
        runner.state().objects[&returnee].zone,
        Zone::Battlefield,
        "Sun Titan's attack trigger must return the graveyard permanent to the battlefield"
    );
}
