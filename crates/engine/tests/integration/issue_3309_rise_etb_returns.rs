//! Issue #3309 — Rise of the Dark Realms crash when resolving ETB return triggers.
//!
//! https://github.com/phase-rs/phase/issues/3309
//!
//! Reproduces mass reanimation (Rise of the Dark Realms) followed by simultaneous
//! ETB triggers including an optional graveyard-return (Sun Titan class) and
//! observer triggers (Soul Warden class).

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const RISE_ORACLE: &str =
    "Put all creature cards from all graveyards onto the battlefield under your control.";

const SUN_TITAN_ORACLE: &str =
    "Vigilance\nWhen Sun Titan enters, you may return target permanent card with mana value 3 or less from your graveyard to the battlefield.";

const SOUL_WARDEN_ORACLE: &str = "Whenever another creature enters, you gain 1 life.";

const KARMIC_GUIDE_ORACLE: &str =
    "Flying, echo {3}{W}{W}\nWhen Karmic Guide enters, return target creature card with mana value 3 or less from your graveyard to the battlefield.";

#[test]
fn sun_titan_etb_parses_graveyard_return_not_exile() {
    use engine::parser::oracle::parse_oracle_text;
    use engine::types::ability::Effect;

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
        .find(|t| t.mode == engine::types::triggers::TriggerMode::ChangesZone)
        .expect("Sun Titan must have an ETB trigger");
    let execute = trigger
        .execute
        .as_ref()
        .expect("ETB must have execute ability");
    match execute.effect.as_ref() {
        Effect::ChangeZone {
            origin: Some(Zone::Graveyard),
            destination: Zone::Battlefield,
            ..
        } => {}
        other => panic!("Sun Titan ETB must be graveyard→battlefield ChangeZone, got {other:?}"),
    }
    assert!(execute.optional, "Sun Titan return must be optional");
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
