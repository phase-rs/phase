use engine::game::companion::can_activate_companion;
use engine::game::deck_loading::DeckEntry;
use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::types::ability::{
    AbilityCost, AbilityDefinition, AbilityKind, Effect, ManaContribution, ManaProduction,
    ReplacementDefinition, TargetFilter,
};
use engine::types::actions::GameAction;
use engine::types::card::CardFace;
use engine::types::events::GameEvent;
use engine::types::game_state::{GameState, ManaAbilityResume, PendingCostMoveResume, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{
    ManaColor, ManaCost, ManaRestriction, ManaType, ManaUnit, PaymentContext, SpecialAction,
    SpellMeta,
};
use engine::types::phase::Phase;
use engine::types::player::CompanionInfo;
use engine::types::replacements::ReplacementEvent;
use engine::types::statics::{CostModifyMode, StaticMode};
use engine::types::zones::{EtbTapState, Zone};

fn set_declared_companion(runner: &mut GameRunner) {
    runner.state_mut().players[P0.0 as usize].companion = Some(CompanionInfo {
        card: DeckEntry {
            card: CardFace {
                name: "Companion Payment Witness".to_string(),
                ..Default::default()
            },
            count: 1,
        },
        used: false,
    });
}

fn companion_scenario() -> GameScenario {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
}

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

/// CR 116.2g + CR 702.139a: The real action offer and action dispatcher use
/// automatic mana-source activation for companion's {3}, then commit exactly
/// once after payment.
#[test]
fn companion_action_auto_taps_three_sources_and_moves_to_hand() {
    let mut scenario = companion_scenario();
    let lands = [
        scenario.add_basic_land(P0, ManaColor::Green),
        scenario.add_basic_land(P0, ManaColor::Blue),
        scenario.add_basic_land(P0, ManaColor::Red),
    ];
    let mut runner = scenario.build();
    set_declared_companion(&mut runner);

    assert!(
        can_activate_companion(runner.state(), P0),
        "the action offer must account for untapped mana sources, not only the pool"
    );
    let result = runner
        .act(GameAction::CompanionToHand)
        .expect("three untapped mana sources must pay the companion action");

    assert!(runner.state().players[P0.0 as usize]
        .companion
        .as_ref()
        .is_some_and(|companion| companion.used));
    assert!(runner.state().objects.values().any(|object| {
        object.owner == P0
            && object.zone == Zone::Hand
            && object.name == "Companion Payment Witness"
    }));
    assert!(lands.iter().all(|id| runner.state().objects[id].tapped));
    assert!(!runner.state().lands_tapped_for_mana.contains_key(&P0));
    assert!(result.events.iter().any(|event| matches!(
        event,
        GameEvent::CompanionMovedToHand { player, card_name }
            if *player == P0 && card_name == "Companion Payment Witness"
    )));
}

/// CR 116.2g: A forged direct action with insufficient sources is rejected by
/// the dry-run payment preflight. It may not consume pool mana, tap sources,
/// alter the waiting state, mark the companion used, create a hand object, or
/// clear undoable mana-tap tracking.
#[test]
fn forged_unaffordable_companion_action_has_no_payment_mutation() {
    let mut scenario = companion_scenario();
    let first = scenario.add_basic_land(P0, ManaColor::Green);
    let second = scenario.add_basic_land(P0, ManaColor::Blue);
    let mut runner = scenario.build();
    set_declared_companion(&mut runner);
    runner
        .state_mut()
        .lands_tapped_for_mana
        .insert(P0, vec![first]);

    let result = runner.act(GameAction::CompanionToHand);
    assert!(
        result.is_err(),
        "two sources cannot pay the {{3}} companion cost"
    );
    let state = runner.state();
    assert!(!state.objects[&first].tapped && !state.objects[&second].tapped);
    assert_eq!(state.players[P0.0 as usize].mana_pool.total(), 0);
    assert!(matches!(state.waiting_for, WaitingFor::Priority { player } if player == P0));
    assert!(state.players[P0.0 as usize]
        .companion
        .as_ref()
        .is_some_and(|companion| !companion.used));
    assert!(state
        .objects
        .values()
        .all(|object| { object.zone != Zone::Hand || object.name != "Companion Payment Witness" }));
    assert_eq!(state.lands_tapped_for_mana.get(&P0), Some(&vec![first]));
}

/// CR 106.6 + CR 116.2g: Only mana restricted to CompanionToHand can pay this
/// special action. The exact same restriction rejects every other special
/// action plus spell, activation, and effect payment contexts.
#[test]
fn companion_restricted_mana_routes_only_to_the_matching_special_action() {
    let restriction = ManaRestriction::OnlyForSpecialAction(SpecialAction::CompanionToHand);
    let scenario = companion_scenario();
    let mut runner = scenario.build();
    set_declared_companion(&mut runner);
    for _ in 0..3 {
        runner.state_mut().players[P0.0 as usize]
            .mana_pool
            .add(ManaUnit::new(
                ManaType::Green,
                ObjectId(700),
                false,
                vec![restriction.clone()],
            ));
    }

    assert!(can_activate_companion(runner.state(), P0));
    runner
        .act(GameAction::CompanionToHand)
        .expect("matching companion-restricted mana must be spendable");
    assert_eq!(runner.state().players[P0.0 as usize].mana_pool.total(), 0);

    let spell = SpellMeta::default();
    assert!(!restriction.allows(&PaymentContext::SpecialAction(SpecialAction::UnlockDoor)));
    assert!(!restriction.allows(&PaymentContext::Spell(&spell)));
    assert!(!restriction.allows(&PaymentContext::Activation {
        source_types: &[],
        source_subtypes: &[],
        ability_tag: None,
    }));
    assert!(!restriction.allows(&PaymentContext::Effect));
}

/// CR 118.7a + CR 116.2g: The same reduced cost drives both availability and
/// actual payment, so a {2} companion reduction makes one source sufficient.
#[test]
fn companion_cost_reduction_applies_to_offer_and_payment() {
    let mut scenario = companion_scenario();
    let land = scenario.add_basic_land(P0, ManaColor::Green);
    scenario
        .add_creature(P0, "Companion Cost Reducer", 1, 1)
        .with_static(StaticMode::ReduceActionCost {
            action: SpecialAction::CompanionToHand,
            mode: CostModifyMode::Reduce,
            amount: 2,
        });
    let mut runner = scenario.build();
    set_declared_companion(&mut runner);

    assert!(can_activate_companion(runner.state(), P0));
    runner
        .act(GameAction::CompanionToHand)
        .expect("the offered reduced companion cost must also be payable");
    assert!(runner.state().objects[&land].tapped);
    assert!(runner.state().players[P0.0 as usize]
        .companion
        .as_ref()
        .is_some_and(|companion| companion.used));
}

/// CR 605.3b + CR 616.1: When an auto-tapped mana source pauses for a
/// replacement choice, the serialized companion root carries the locked reduced
/// cost. Removing the reducer before the response cannot make the resumed
/// payment recalculate to the original {3}; it completes the committed {2}.
#[test]
fn paused_companion_payment_resumes_the_locked_cost_and_commits_once() {
    let mut scenario = companion_scenario();
    let basic = scenario.add_basic_land(P0, ManaColor::Green);
    let reducer = scenario
        .add_creature(P0, "Paused Companion Cost Reducer", 1, 1)
        .with_static(StaticMode::ReduceActionCost {
            action: SpecialAction::CompanionToHand,
            mode: CostModifyMode::Reduce,
            amount: 1,
        })
        .id();
    let sources = ["First", "Second"].map(|ordinal| {
        scenario
            .add_creature(P0, &format!("{ordinal} Paused Companion Mana Source"), 1, 1)
            .with_ability_definition(
                AbilityDefinition::new(
                    AbilityKind::Activated,
                    Effect::Mana {
                        produced: ManaProduction::Fixed {
                            colors: vec![ManaColor::Green],
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
            .id()
    });
    for name in ["First Pause Replacement", "Second Pause Replacement"] {
        scenario
            .add_creature(P0, name, 0, 0)
            .as_enchantment()
            .with_replacement_definition(redirect_exile_to_graveyard());
    }

    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&basic)
        .expect("the recorded mana source exists")
        .tapped = true;
    set_declared_companion(&mut runner);
    runner
        .state_mut()
        .lands_tapped_for_mana
        .insert(P0, vec![basic]);

    let paused = runner
        .act(GameAction::CompanionToHand)
        .expect("the source cost replacement must pause the companion payment");
    assert!(matches!(
        paused.waiting_for,
        WaitingFor::ReplacementChoice { .. }
    ));
    assert!(matches!(
        runner.state().pending_cost_move_resume.as_ref(),
        Some(PendingCostMoveResume::ManaAbilityPayment { pending, .. })
            if matches!(
                &pending.resume,
                ManaAbilityResume::CompanionToHand { player, cost }
                    if *player == P0 && cost == &ManaCost::generic(2)
            )
    ));
    assert!(runner.state().players[P0.0 as usize]
        .companion
        .as_ref()
        .is_some_and(|companion| !companion.used));
    assert_eq!(
        runner.state().lands_tapped_for_mana.get(&P0),
        Some(&vec![basic])
    );

    let serialized = serde_json::to_string(runner.state())
        .expect("the paused companion continuation must serialize");
    let restored: GameState = serde_json::from_str(&serialized)
        .expect("the paused companion continuation must deserialize");
    let mut runner = GameRunner::from_state(restored);
    runner
        .state_mut()
        .objects
        .get_mut(&reducer)
        .unwrap()
        .static_definitions
        .clear();

    let paused_again = runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("the first replacement response must resume into the next paused source");
    assert!(matches!(
        paused_again.waiting_for,
        WaitingFor::ReplacementChoice { .. }
    ));
    assert!(matches!(
        runner.state().pending_cost_move_resume.as_ref(),
        Some(PendingCostMoveResume::ManaAbilityPayment { pending, .. })
            if matches!(
                &pending.resume,
                ManaAbilityResume::CompanionToHand { player, cost }
                    if *player == P0 && cost == &ManaCost::generic(2)
            )
    ));

    let resumed = runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("the second replacement response must complete the locked payment");
    assert!(matches!(resumed.waiting_for, WaitingFor::Priority { player } if player == P0));
    assert!(sources
        .iter()
        .all(|source| runner.state().objects[source].zone == Zone::Graveyard));
    assert!(runner.state().players[P0.0 as usize]
        .companion
        .as_ref()
        .is_some_and(|companion| companion.used));
    assert_eq!(
        runner
            .state()
            .objects
            .values()
            .filter(|object| {
                object.owner == P0
                    && object.zone == Zone::Hand
                    && object.name == "Companion Payment Witness"
            })
            .count(),
        1,
        "resumption must commit the companion move exactly once"
    );
    assert!(!runner.state().lands_tapped_for_mana.contains_key(&P0));
    assert_eq!(
        resumed
            .events
            .iter()
            .filter(|event| matches!(event, GameEvent::CompanionMovedToHand { player, .. } if *player == P0))
            .count(),
        1,
        "the resumed action emits exactly one companion move event"
    );
}
