use engine::ai_support::{candidate_actions, legal_actions};
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::{
    AbilityCost, AbilityDefinition, AbilityKind, CardPlayMode, CastFromZoneDriver,
    CastingPermission, ControllerRef, Effect, FilterProp, ManaContribution, ManaProduction,
    SacrificeCost, StaticCondition, StaticDefinition, TargetFilter, TargetRef, TypeFilter,
    TypedFilter,
};
use engine::types::actions::{DebugAction, GameAction};
use engine::types::game_state::{
    CastPaymentMode, CastingPermissionIndex, CastingVariant, ConvokeMode, ShardOptions, WaitingFor,
};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::{Keyword, KeywordKind};
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::statics::{CostModifyMode, StaticMode};
use engine::types::zones::Zone;

use crate::support::shared_card_db as load_db;

#[test]
fn offer_side_auto_payment_card_data_prerequisite_is_available() {
    assert!(
        load_db().is_some(),
        "offer-side auto-payment integration tests require the committed card fixture or full export"
    );
}

fn interactive_graveyard_mana_ability(color: ManaColor) -> AbilityDefinition {
    AbilityDefinition::new(
        AbilityKind::Activated,
        Effect::Mana {
            produced: ManaProduction::Fixed {
                colors: vec![color],
                contribution: ManaContribution::Base,
            },
            restrictions: vec![],
            grants: vec![],
            expiry: None,
            target: None,
        },
    )
    .cost(AbilityCost::Exile {
        count: 1,
        zone: Some(Zone::Graveyard),
        filter: Some(TargetFilter::Typed(
            TypedFilter::card()
                .controller(ControllerRef::You)
                .properties(vec![FilterProp::InZone {
                    zone: Zone::Graveyard,
                }]),
        )),
    })
}

fn self_sacrificing_mana_ability(color: ManaColor) -> AbilityDefinition {
    AbilityDefinition::new(
        AbilityKind::Activated,
        Effect::Mana {
            produced: ManaProduction::Fixed {
                colors: vec![color],
                contribution: ManaContribution::Base,
            },
            restrictions: vec![],
            grants: vec![],
            expiry: None,
            target: None,
        },
    )
    .cost(AbilityCost::Sacrifice(SacrificeCost::count(
        TargetFilter::SelfRef,
        1,
    )))
}

fn setup_sneak_with_sacrificial_mana(
    sneak_cost: u32,
) -> (GameRunner, ObjectId, ObjectId, ObjectId) {
    setup_sneak_with_sacrificial_mana_cost(ManaCost::generic(sneak_cost), false)
}

fn setup_sneak_with_sacrificial_mana_cost(
    sneak_cost: ManaCost,
    add_irrelevant_red_source: bool,
) -> (GameRunner, ObjectId, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::DeclareBlockers);
    let attacker = scenario.add_creature(P0, "Sneak Return Witness", 2, 2).id();
    let spell = scenario
        .add_spell_to_hand(P0, "Sacrificial Sneak Witness", true)
        .with_mana_cost(ManaCost::generic(7))
        .with_keyword(Keyword::Sneak(sneak_cost))
        .id();
    let mana_source = scenario
        .add_creature(P0, "Sneak Blood Pet Witness", 1, 1)
        .with_ability_definition(self_sacrificing_mana_ability(ManaColor::Black))
        .id();
    if add_irrelevant_red_source {
        scenario.add_basic_land(P0, ManaColor::Red);
    }
    let mut runner = scenario.build();
    runner.state_mut().phase = Phase::DeclareBlockers;
    runner.state_mut().active_player = P0;
    runner.state_mut().priority_player = P0;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P0 };
    runner
        .state_mut()
        .objects
        .get_mut(&attacker)
        .unwrap()
        .tapped = true;
    runner.state_mut().combat = Some(engine::game::combat::CombatState {
        attackers: vec![engine::game::combat::AttackerInfo::attacking_player(
            attacker, P1,
        )],
        ..Default::default()
    });
    (runner, spell, attacker, mana_source)
}

fn setup_web_slinging_with_sacrificial_mana(
    web_slinging_cost: u32,
) -> (GameRunner, ObjectId, ObjectId, ObjectId) {
    setup_web_slinging_with_sacrificial_mana_cost(ManaCost::generic(web_slinging_cost), false)
}

fn setup_web_slinging_with_sacrificial_mana_cost(
    web_slinging_cost: ManaCost,
    add_irrelevant_red_source: bool,
) -> (GameRunner, ObjectId, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let returned_creature = scenario
        .add_creature(P0, "Web-Slinging Return Witness", 2, 2)
        .id();
    let spell = scenario
        .add_spell_to_hand(P0, "Sacrificial Web-Slinging Witness", true)
        .with_mana_cost(ManaCost::generic(7))
        .with_keyword(Keyword::WebSlinging(web_slinging_cost))
        .id();
    let mana_source = scenario
        .add_creature(P0, "Web-Slinging Blood Pet Witness", 1, 1)
        .with_ability_definition(self_sacrificing_mana_ability(ManaColor::Black))
        .id();
    if add_irrelevant_red_source {
        scenario.add_basic_land(P0, ManaColor::Red);
    }
    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&returned_creature)
        .unwrap()
        .tapped = true;
    (runner, spell, returned_creature, mana_source)
}

fn complete_sacrificial_mana_cast(
    runner: &mut GameRunner,
    action: GameAction,
    spell: ObjectId,
    mana_source: ObjectId,
) {
    runner
        .act(action)
        .expect("the offered cast must enter its explicit mana-source choice");
    let selection = match &runner.state().waiting_for {
        WaitingFor::ManaSourceSelection { options, .. } => options
            .iter()
            .find(|selection| selection.source.object_id == mana_source)
            .cloned()
            .expect("the sacrificial source must remain available for explicit consent"),
        other => panic!("expected ManaSourceSelection, got {other:?}"),
    };
    runner
        .act(GameAction::ActivateManaSource { selection })
        .expect("the selected self-sacrificing mana ability must resolve");
    assert_eq!(runner.state().objects[&mana_source].zone, Zone::Graveyard);
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ManaPayment { .. }
    ));
    runner
        .act(GameAction::PassPriority)
        .expect("the produced mana must complete the prepared cast payment");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    assert_eq!(runner.state().objects[&spell].zone, Zone::Stack);
}

fn black_mana_cost() -> ManaCost {
    ManaCost::Cost {
        shards: vec![ManaCostShard::Black],
        generic: 0,
    }
}

fn setup_black_spell_with_sacrificial_mana_and_irrelevant_red() -> (GameRunner, ObjectId, ObjectId)
{
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand(P0, "Sacrificial Black Witness", true)
        .with_mana_cost(black_mana_cost())
        .id();
    let mana_source = scenario
        .add_creature(P0, "Black Blood Pet Witness", 1, 1)
        .with_ability_definition(self_sacrificing_mana_ability(ManaColor::Black))
        .id();
    scenario.add_basic_land(P0, ManaColor::Red);
    (scenario.build(), spell, mana_source)
}

/// CR 601.2g-h: An ordinary cast retains explicit sacrificial-mana consent
/// when another activatable source cannot pay its remaining colored cost.
#[test]
fn irrelevant_non_sacrificial_mana_preserves_regular_source_choice() {
    let (mut runner, spell, mana_source) =
        setup_black_spell_with_sacrificial_mana_and_irrelevant_red();
    let action = legal_actions(runner.state())
        .into_iter()
        .find(|action| {
            matches!(
                action,
                GameAction::CastSpell {
                    object_id,
                    payment_mode: CastPaymentMode::AutoExceptSacrificialMana,
                    ..
                } if *object_id == spell
            )
        })
        .expect("an irrelevant red source must not enable automatic sacrifice");

    complete_sacrificial_mana_cast(&mut runner, action, spell, mana_source);
}

/// CR 702.190a + CR 601.2g-h: Sneak's exact alternative cost remains offered
/// when only an explicitly chosen self-sacrificing mana ability can pay it.
#[test]
fn sneak_sacrificial_only_cost_is_offered_and_completes_manually() {
    let (mut runner, spell, attacker, mana_source) = setup_sneak_with_sacrificial_mana(1);
    let action = legal_actions(runner.state())
        .into_iter()
        .find(|action| {
            matches!(
                action,
                GameAction::CastSpellAsSneak {
                    hand_object,
                    creature_to_return,
                    payment_mode: CastPaymentMode::AutoExceptSacrificialMana,
                    ..
                } if *hand_object == spell && *creature_to_return == attacker
            )
        })
        .expect("the choice-preserving Sneak cast must be offered");

    complete_sacrificial_mana_cast(&mut runner, action, spell, mana_source);
    assert_eq!(runner.state().objects[&attacker].zone, Zone::Hand);
}

/// CR 702.190a + CR 601.2g-h: Sneak uses its prepared alternative cost when
/// deciding whether a non-sacrificial source can actually replace source choice.
#[test]
fn irrelevant_non_sacrificial_mana_preserves_sneak_source_choice() {
    let (mut runner, spell, attacker, mana_source) =
        setup_sneak_with_sacrificial_mana_cost(black_mana_cost(), true);
    let action = legal_actions(runner.state())
        .into_iter()
        .find(|action| {
            matches!(
                action,
                GameAction::CastSpellAsSneak {
                    hand_object,
                    creature_to_return,
                    payment_mode: CastPaymentMode::AutoExceptSacrificialMana,
                    ..
                } if *hand_object == spell && *creature_to_return == attacker
            )
        })
        .expect("an irrelevant red source must not automate a Sneak sacrifice");

    complete_sacrificial_mana_cast(&mut runner, action, spell, mana_source);
}

/// CR 702.190a + CR 601.2h: A Sneak action whose prepared alternative cost
/// exceeds the only sacrificial source's capacity remains unoffered.
#[test]
fn sneak_truly_unpayable_sacrificial_cost_is_not_offered() {
    let (runner, spell, attacker, _) = setup_sneak_with_sacrificial_mana(2);
    assert_eq!(
        engine::game::keywords::effective_sneak_cost(runner.state(), spell),
        Some(ManaCost::generic(2)),
        "reach guard: the Sneak alternative-cost authority must resolve the tested cost"
    );
    assert!(runner.state().combat.as_ref().is_some_and(|combat| combat
        .attackers
        .iter()
        .any(|entry| entry.object_id == attacker)));
    assert!(!legal_actions(runner.state()).iter().any(|action| matches!(
        action,
        GameAction::CastSpellAsSneak { hand_object, .. } if *hand_object == spell
    )));
}

/// CR 702.188a + CR 601.2g-h: Web-slinging's exact alternative cost remains
/// offered when only an explicitly chosen self-sacrificing mana ability pays it.
#[test]
fn web_slinging_sacrificial_only_cost_is_offered_and_completes_manually() {
    let (mut runner, spell, returned_creature, mana_source) =
        setup_web_slinging_with_sacrificial_mana(1);
    let action = legal_actions(runner.state())
        .into_iter()
        .find(|action| {
            matches!(
                action,
                GameAction::CastSpellAsWebSlinging {
                    hand_object,
                    creature_to_return,
                    payment_mode: CastPaymentMode::AutoExceptSacrificialMana,
                    ..
                } if *hand_object == spell && *creature_to_return == returned_creature
            )
        })
        .expect("the choice-preserving Web-slinging cast must be offered");

    complete_sacrificial_mana_cast(&mut runner, action, spell, mana_source);
    assert_eq!(runner.state().objects[&returned_creature].zone, Zone::Hand);
}

/// CR 702.188a + CR 601.2g-h: Web-slinging uses its prepared alternative cost
/// when deciding whether a non-sacrificial source can replace source choice.
#[test]
fn irrelevant_non_sacrificial_mana_preserves_web_slinging_source_choice() {
    let (mut runner, spell, returned_creature, mana_source) =
        setup_web_slinging_with_sacrificial_mana_cost(black_mana_cost(), true);
    let action = legal_actions(runner.state())
        .into_iter()
        .find(|action| {
            matches!(
                action,
                GameAction::CastSpellAsWebSlinging {
                    hand_object,
                    creature_to_return,
                    payment_mode: CastPaymentMode::AutoExceptSacrificialMana,
                    ..
                } if *hand_object == spell && *creature_to_return == returned_creature
            )
        })
        .expect("an irrelevant red source must not automate a Web-slinging sacrifice");

    complete_sacrificial_mana_cast(&mut runner, action, spell, mana_source);
}

/// CR 702.188a + CR 601.2h: A Web-slinging action whose prepared alternative
/// cost exceeds the only sacrificial source's capacity remains unoffered.
#[test]
fn web_slinging_truly_unpayable_sacrificial_cost_is_not_offered() {
    let (runner, spell, returned_creature, _) = setup_web_slinging_with_sacrificial_mana(2);
    assert_eq!(
        engine::game::keywords::effective_web_slinging_cost(runner.state(), P0, spell),
        Some(ManaCost::generic(2)),
        "reach guard: the Web-slinging alternative-cost authority must resolve the tested cost"
    );
    assert!(runner.state().objects[&returned_creature].tapped);
    assert!(!legal_actions(runner.state()).iter().any(|action| matches!(
        action,
        GameAction::CastSpellAsWebSlinging { hand_object, .. } if *hand_object == spell
    )));
}

fn setup_murder_with_static(
    plain_land_count: usize,
    target_sensitive_static: Option<StaticDefinition>,
) -> (GameRunner, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let murder = scenario
        .add_spell_to_hand_from_oracle(P0, "Murder", true, "Destroy target creature.")
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Black, ManaCostShard::Black],
            generic: 1,
        })
        .id();
    let victim = scenario.add_creature(P1, "Murder Victim", 2, 2).id();
    scenario.add_creature(P1, "Second Legal Target", 1, 1);
    scenario.add_basic_land(P0, ManaColor::Black);
    scenario.add_basic_land(P0, ManaColor::Black);
    for _ in 0..plain_land_count {
        scenario.add_basic_land(P0, ManaColor::Blue);
    }
    scenario
        .add_creature(P0, "Choice-Bearing Mana Source", 0, 3)
        .with_ability_definition(interactive_graveyard_mana_ability(ManaColor::Black));
    if let Some(definition) = target_sensitive_static {
        scenario
            .add_creature(P1, "Target-Sensitive Cost Static", 1, 4)
            .with_static_definition(definition);
    }
    scenario.add_spell_to_graveyard(P0, "Mana Source Exile Fodder", true);
    (scenario.build(), murder, victim)
}

fn setup_murder(plain_land_count: usize) -> (GameRunner, ObjectId, ObjectId) {
    setup_murder_with_static(plain_land_count, None)
}

fn cost_static_with_filter(
    caster_scope: ControllerRef,
    condition: Option<StaticCondition>,
    spell_filter: TargetFilter,
) -> StaticDefinition {
    let mut definition = StaticDefinition::new(StaticMode::ModifyCost {
        mode: CostModifyMode::Raise,
        amount: ManaCost::generic(1),
        spell_filter: Some(spell_filter),
        dynamic_count: None,
    })
    .affected(TargetFilter::Typed(
        TypedFilter::card().controller(caster_scope),
    ));
    if let Some(condition) = condition {
        definition = definition.condition(condition);
    }
    definition
}

fn target_sensitive_cost_static(
    caster_scope: ControllerRef,
    condition: Option<StaticCondition>,
    spell_filter: TypedFilter,
) -> StaticDefinition {
    cost_static_with_filter(
        caster_scope,
        condition,
        TargetFilter::Typed(spell_filter.properties(vec![FilterProp::Targets {
            filter: Box::new(TargetFilter::SelfRef),
        }])),
    )
}

fn target_dependent_card_filter() -> TargetFilter {
    TargetFilter::Typed(TypedFilter::card().properties(vec![FilterProp::Targets {
        filter: Box::new(TargetFilter::SelfRef),
    }]))
}

fn auto_cast_action(state: &engine::types::game_state::GameState, spell: ObjectId) -> GameAction {
    let card_id = state.objects[&spell].card_id;
    GameAction::CastSpell {
        object_id: spell,
        card_id,
        targets: vec![],
        payment_mode: CastPaymentMode::Auto,
    }
}

#[test]
fn ordinary_auto_cast_is_not_offered_when_only_interactive_mana_source_completes_cost() {
    let (runner, murder, _) = setup_murder(0);
    let action = auto_cast_action(runner.state(), murder);
    assert!(
        candidate_actions(runner.state())
            .iter()
            .any(|candidate| candidate.action == action),
        "reach guard: broad generation must expose the manually payable cast"
    );

    let before = runner.state().clone();
    let before_json = serde_json::to_value(&before).expect("state snapshot must serialize");
    let mut disposable = GameRunner::from_state(before.clone());
    disposable
        .act(action.clone())
        .expect("the production reducer must reach target selection before payment");
    assert!(matches!(
        disposable.state().waiting_for,
        WaitingFor::TargetSelection { .. }
    ));

    assert!(
        !legal_actions(runner.state()).contains(&action),
        "REVERT-PROOF: Auto must not be offered when its payer cannot answer the mana-source choice"
    );
    assert_eq!(
        runner.state().players[0].mana_pool,
        before.players[0].mana_pool
    );
    assert_eq!(runner.state().battlefield, before.battlefield);
    assert_eq!(
        serde_json::to_value(runner.state()).expect("live state must serialize"),
        before_json
    );
}

#[test]
fn ordinary_auto_cast_is_offered_and_completes_with_plain_land() {
    let (mut runner, murder, victim) = setup_murder(1);
    let action = auto_cast_action(runner.state(), murder);
    assert!(legal_actions(runner.state()).contains(&action));

    let outcome = runner.cast(murder).target_object(victim).resolve();
    outcome.assert_zone(&[victim], Zone::Graveyard);
    assert!(matches!(
        outcome.final_waiting_for(),
        WaitingFor::Priority { .. }
    ));
}

#[test]
fn only_applicable_target_sensitive_cost_statics_defer_strict_auto_payment() {
    let irrelevant_statics = [
        (
            "wrong caster scope",
            target_sensitive_cost_static(ControllerRef::You, None, TypedFilter::card()),
        ),
        (
            "failed condition",
            target_sensitive_cost_static(
                ControllerRef::Opponent,
                Some(StaticCondition::DuringYourTurn),
                TypedFilter::card(),
            ),
        ),
        (
            "nonmatching spell filter",
            target_sensitive_cost_static(ControllerRef::Opponent, None, TypedFilter::creature()),
        ),
    ];

    for (reason, definition) in irrelevant_statics {
        let (runner, murder, _) = setup_murder_with_static(0, Some(definition));
        let action = auto_cast_action(runner.state(), murder);
        assert!(
            candidate_actions(runner.state())
                .iter()
                .any(|candidate| candidate.action == action),
            "reach guard: {reason} fixture must reach exact offer filtering"
        );
        assert!(
            !legal_actions(runner.state()).contains(&action),
            "an irrelevant target-sensitive static with {reason} must not rescue an uncompletable Auto cast"
        );
    }

    let relevant = target_sensitive_cost_static(ControllerRef::Opponent, None, TypedFilter::card());
    let (runner, murder, _) = setup_murder_with_static(0, Some(relevant));
    let action = auto_cast_action(runner.state(), murder);
    assert!(
        candidate_actions(runner.state())
            .iter()
            .any(|candidate| candidate.action == action),
        "reach guard: relevant target-sensitive static fixture must generate the cast"
    );
    assert!(
        legal_actions(runner.state()).contains(&action),
        "a relevant target-sensitive static must defer strict payment until its target is chosen"
    );
}

fn assert_target_independent_composed_filter_offer_pair(label: &str, spell_filter: TargetFilter) {
    let definition = cost_static_with_filter(ControllerRef::Opponent, None, spell_filter.clone());
    let (runner, murder, _) = setup_murder_with_static(1, Some(definition));
    let action = auto_cast_action(runner.state(), murder);
    assert!(
        candidate_actions(runner.state())
            .iter()
            .any(|candidate| candidate.action == action),
        "reach guard: {label} must remain broadly payable through the choice-bearing source"
    );

    let mut disposable = GameRunner::from_state(runner.state().clone());
    disposable
        .act(action.clone())
        .expect("the composed-filter cast must reach target selection");
    let WaitingFor::TargetSelection { pending_cast, .. } = &disposable.state().waiting_for else {
        panic!("{label} must reach target selection")
    };
    assert_eq!(
        pending_cast.cost,
        ManaCost::Cost {
            shards: vec![ManaCostShard::Black, ManaCostShard::Black],
            generic: 2,
        },
        "{label} is fixed-relevant before targets and must apply its tax in the pre-target pass"
    );
    assert!(
        !legal_actions(runner.state()).contains(&action),
        "REVERT-PROOF: {label} must not defer the strict verdict and offer an uncompletable Auto cast"
    );

    let payable_definition = cost_static_with_filter(ControllerRef::Opponent, None, spell_filter);
    let (mut payable, payable_murder, victim) =
        setup_murder_with_static(2, Some(payable_definition));
    let payable_action = auto_cast_action(payable.state(), payable_murder);
    assert!(
        legal_actions(payable.state()).contains(&payable_action),
        "{label} must keep the genuinely Auto-payable sibling offered"
    );
    payable
        .cast(payable_murder)
        .target_object(victim)
        .resolve()
        .assert_zone(&[victim], Zone::Graveyard);
}

#[test]
fn target_independent_or_filter_does_not_rescue_uncompletable_auto_cast() {
    assert_target_independent_composed_filter_offer_pair(
        "fixed-true Or filter",
        TargetFilter::Or {
            filters: vec![
                TargetFilter::Typed(TypedFilter::card()),
                target_dependent_card_filter(),
            ],
        },
    );
}

#[test]
fn target_independent_not_filter_does_not_rescue_uncompletable_auto_cast() {
    assert_target_independent_composed_filter_offer_pair(
        "fixed-true Not filter",
        TargetFilter::Not {
            filter: Box::new(TargetFilter::And {
                filters: vec![
                    TargetFilter::Typed(TypedFilter::creature()),
                    target_dependent_card_filter(),
                ],
            }),
        },
    );
}

fn setup_prepared_copy(add_plain_land: bool) -> Option<(GameRunner, ObjectId, ObjectId)> {
    let db = load_db()?;
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let emeritus = scenario.add_real_card(P0, "Emeritus of Truce", Zone::Battlefield, db);
    let victim = scenario.add_creature(P1, "Prepared Copy Victim", 4, 4).id();
    scenario.add_creature(P1, "Second Prepared Target", 1, 1);
    scenario
        .add_creature(P0, "Prepared Choice-Bearing Source", 0, 3)
        .with_ability_definition(interactive_graveyard_mana_ability(ManaColor::White));
    scenario.add_spell_to_graveyard(P0, "Prepared Mana Fodder", true);
    if add_plain_land {
        scenario.add_basic_land(P0, ManaColor::White);
    }
    let mut runner = scenario.build();
    runner.state_mut().debug_mode = true;
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    runner
        .act(GameAction::Debug(DebugAction::SetPrepared {
            object_id: emeritus,
            prepared: true,
        }))
        .expect("fixture must mark the real Prepare source prepared");
    Some((runner, emeritus, victim))
}

#[test]
fn prepared_copy_auto_cast_is_not_offered_when_unpayable() {
    let Some((runner, emeritus, victim)) = setup_prepared_copy(false) else {
        return;
    };
    let action = GameAction::CastPreparedCopy { source: emeritus };
    assert!(
        candidate_actions(runner.state())
            .iter()
            .any(|candidate| candidate.action == action),
        "reach guard: prepared-copy candidate must reach strict simulation"
    );
    let mut disposable = GameRunner::from_state(runner.state().clone());
    disposable
        .act(action.clone())
        .expect("prepared production reducer must synthesize the exact copy");
    let WaitingFor::TargetSelection { pending_cast, .. } = &disposable.state().waiting_for else {
        panic!("prepared copy must reach target selection")
    };
    assert_eq!(
        pending_cast.cost,
        ManaCost::Cost {
            shards: vec![ManaCostShard::White],
            generic: 0,
        }
    );
    assert!(pending_cast.casting_permission_index.is_some());
    assert_eq!(pending_cast.ability.controller, P0);
    assert!(pending_cast
        .ability
        .targets
        .iter()
        .all(|target| *target != TargetRef::Object(victim)));

    let before = runner.state().clone();
    let before_json = serde_json::to_value(&before).expect("state snapshot must serialize");
    assert!(
        !legal_actions(runner.state()).contains(&action),
        "REVERT-PROOF: implicit-Auto prepared copy must be filtered when the real payer cannot complete"
    );
    assert!(runner.state().objects[&emeritus].prepared.is_some());
    assert_eq!(
        serde_json::to_value(runner.state()).expect("live state must serialize"),
        before_json
    );
}

#[test]
fn prepared_copy_auto_cast_is_offered_and_completes_when_payable() {
    let Some((mut runner, emeritus, victim)) = setup_prepared_copy(true) else {
        return;
    };
    let action = GameAction::CastPreparedCopy { source: emeritus };
    assert!(legal_actions(runner.state()).contains(&action));
    runner
        .act(action)
        .expect("payable prepared copy must start its real cast");
    let WaitingFor::TargetSelection { pending_cast, .. } = &runner.state().waiting_for else {
        panic!("payable prepared copy must reach target selection")
    };
    assert_eq!(
        pending_cast.cost,
        ManaCost::Cost {
            shards: vec![ManaCostShard::White],
            generic: 0,
        }
    );
    assert!(pending_cast.casting_permission_index.is_some());
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(victim)),
        })
        .expect("targeting and automatic payment must commit the prepared spell");
    runner.advance_until_stack_empty();
    assert_eq!(runner.state().objects[&victim].zone, Zone::Exile);
    assert!(runner.state().objects[&emeritus].prepared.is_none());
}

fn setup_face_of_boe(add_plain_red_source: bool) -> Option<(GameRunner, ObjectId)> {
    let db = load_db()?;
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let face = scenario
        .add_creature(P0, "The Face of Boe Fixture", 3, 5)
        .with_ability_definition(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::CastFromZone {
                    target: TargetFilter::Typed(
                        TypedFilter::default()
                            .with_type(TypeFilter::Card)
                            .controller(ControllerRef::You)
                            .properties(vec![
                                FilterProp::WithKeyword {
                                    value: Keyword::Suspend {
                                        count: 0,
                                        cost: ManaCost::zero(),
                                    },
                                },
                                FilterProp::InZone { zone: Zone::Hand },
                            ]),
                    ),
                    without_paying_mana_cost: false,
                    mode: CardPlayMode::Cast,
                    cast_transformed: false,
                    alt_ability_cost: Some(AbilityCost::KeywordCostOfCastSpell {
                        keyword: KeywordKind::Suspend,
                    }),
                    constraint: None,
                    duration: None,
                    driver: CastFromZoneDriver::DuringResolution,
                    mana_spend_permission: None,
                },
            )
            .cost(AbilityCost::Tap)
            .optional(),
        )
        .id();
    let rift_bolt = scenario.add_real_card(P0, "Rift Bolt", Zone::Hand, db);
    scenario
        .add_creature(P0, "Alternative-Mana Choice Source", 0, 3)
        .with_ability_definition(interactive_graveyard_mana_ability(ManaColor::Red));
    scenario.add_spell_to_graveyard(P0, "Alternative-Mana Fodder", true);
    if add_plain_red_source {
        scenario.add_basic_land(P0, ManaColor::Red);
    }
    let mut runner = scenario.build();
    let suspend = Keyword::Suspend {
        count: 1,
        cost: ManaCost::Cost {
            shards: vec![ManaCostShard::Red],
            generic: 0,
        },
    };
    let rift_bolt_object = runner
        .state_mut()
        .objects
        .get_mut(&rift_bolt)
        .expect("the real Rift Bolt fixture must exist");
    rift_bolt_object.keywords = vec![suspend.clone()];
    rift_bolt_object.base_keywords = vec![suspend.clone()];
    assert_eq!(
        runner.state().objects[&rift_bolt].base_keywords,
        vec![suspend]
    );
    assert_eq!(
        engine::game::keywords::effective_suspend_cost(runner.state(), rift_bolt),
        Some(ManaCost::Cost {
            shards: vec![ManaCostShard::Red],
            generic: 0,
        })
    );
    let activate = legal_actions(runner.state())
        .into_iter()
        .find(|action| {
            matches!(
                action,
                GameAction::ActivateAbility { source_id, .. } if *source_id == face
            )
        })
        .expect("The Face of Boe's production activation must be legal at sorcery speed");
    runner
        .act(activate)
        .expect("The Face of Boe activation must enter the stack");
    runner.resolve_top();
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::OptionalEffectChoice { .. }
    ));
    runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .expect("the production 'you may cast' choice must be accepted");
    let WaitingFor::EffectZoneChoice { cards, .. } = &runner.state().waiting_for else {
        panic!(
            "The Face of Boe must resolve to EffectZoneChoice, got {:?}",
            runner.state().waiting_for
        )
    };
    assert_eq!(cards, &vec![rift_bolt]);
    assert!(matches!(
        runner
            .state()
            .active_ability_continuation()
            .map(|continuation| &continuation.chain.effect),
        Some(Effect::CastFromZone {
            alt_ability_cost: Some(AbilityCost::KeywordCostOfCastSpell {
                keyword: KeywordKind::Suspend
            }),
            driver: CastFromZoneDriver::DuringResolution,
            ..
        })
    ));
    Some((runner, rift_bolt))
}

fn select_cards_action(card: ObjectId) -> GameAction {
    GameAction::SelectCards { cards: vec![card] }
}

#[test]
fn effect_zone_alternative_mana_targeted_cast_is_not_offered_when_auto_cannot_pay() {
    let Some((runner, rift_bolt)) = setup_face_of_boe(false) else {
        return;
    };
    let action = select_cards_action(rift_bolt);
    assert!(
        candidate_actions(runner.state())
            .iter()
            .any(|candidate| candidate.action == action),
        "reach guard: the production EffectZoneChoice must generate Rift Bolt's SelectCards action"
    );
    let before_json = serde_json::to_value(runner.state()).expect("state must serialize");
    let mut disposable = GameRunner::from_state(runner.state().clone());
    disposable
        .act(action.clone())
        .expect("selection must bind the Suspend alternative-mana authority");
    let WaitingFor::TargetSelection { pending_cast, .. } = &disposable.state().waiting_for else {
        panic!("Rift Bolt must reach its targeted pending cast")
    };
    assert_eq!(pending_cast.object_id, rift_bolt);
    assert_eq!(
        pending_cast.cost,
        ManaCost::Cost {
            shards: vec![ManaCostShard::Red],
            generic: 0,
        }
    );
    assert!(pending_cast.casting_permission_index.is_some());
    assert!(
        !legal_actions(runner.state()).contains(&action),
        "REVERT-PROOF: the exact EffectZoneChoice source action must be filtered when Suspend {{R}} is not Auto-payable"
    );
    assert_eq!(
        serde_json::to_value(runner.state()).expect("live state must serialize"),
        before_json
    );
}

#[test]
fn effect_zone_alternative_mana_targeted_cast_is_offered_and_completes_when_payable() {
    let Some((mut runner, rift_bolt)) = setup_face_of_boe(true) else {
        return;
    };
    let action = select_cards_action(rift_bolt);
    assert!(legal_actions(runner.state()).contains(&action));
    runner
        .act(action)
        .expect("payable Suspend-cost selection must begin the exact cast");
    let WaitingFor::TargetSelection { pending_cast, .. } = &runner.state().waiting_for else {
        panic!("Rift Bolt must reach target selection")
    };
    assert_eq!(pending_cast.object_id, rift_bolt);
    assert_eq!(
        pending_cast.cost,
        ManaCost::Cost {
            shards: vec![ManaCostShard::Red],
            generic: 0,
        }
    );
    assert!(pending_cast.casting_permission_index.is_some());
    let life_before = runner.state().players[1].life;
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Player(P1)),
        })
        .expect("target selection must auto-pay the exact Suspend cost");
    runner.advance_until_stack_empty();
    assert_eq!(runner.state().players[1].life, life_before - 3);
    assert_eq!(runner.state().objects[&rift_bolt].zone, Zone::Graveyard);
}

fn setup_free_hand_pick() -> (GameRunner, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_creature(P0, "Free Hand-Pick Source", 2, 2)
        .with_ability_definition(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::CastFromZone {
                    target: TargetFilter::Typed(
                        TypedFilter::card()
                            .controller(ControllerRef::You)
                            .properties(vec![FilterProp::InZone { zone: Zone::Hand }]),
                    ),
                    without_paying_mana_cost: true,
                    mode: CardPlayMode::Cast,
                    cast_transformed: false,
                    alt_ability_cost: None,
                    constraint: None,
                    duration: None,
                    driver: CastFromZoneDriver::DuringResolution,
                    mana_spend_permission: None,
                },
            )
            .cost(AbilityCost::Tap)
            .optional(),
        )
        .id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Free Hand-Pick Removal",
            true,
            "Destroy target creature.",
        )
        .with_mana_cost(ManaCost::generic(9))
        .id();
    let victim = scenario
        .add_creature(P1, "Free Hand-Pick Victim", 2, 2)
        .id();
    let mut runner = scenario.build();
    let activate = legal_actions(runner.state())
        .into_iter()
        .find(|action| {
            matches!(action, GameAction::ActivateAbility { source_id, .. } if *source_id == source)
        })
        .expect("the free hand-pick source must be activatable");
    runner
        .act(activate)
        .expect("activation must enter the stack");
    runner.resolve_top();
    runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .expect("the free cast offer must be accepted");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::EffectZoneChoice { .. }
    ));
    (runner, spell, victim)
}

#[test]
fn effect_zone_free_targeted_cast_selection_remains_offered() {
    let (mut runner, spell, victim) = setup_free_hand_pick();
    let action = select_cards_action(spell);
    assert!(candidate_actions(runner.state())
        .iter()
        .any(|candidate| candidate.action == action));
    assert!(
        legal_actions(runner.state()).contains(&action),
        "a Free-to-Auto hand-pick source action must never be filtered"
    );
    runner
        .act(action)
        .expect("free selection must start its cast");
    let WaitingFor::TargetSelection { pending_cast, .. } = &runner.state().waiting_for else {
        panic!("free targeted spell must reach target selection")
    };
    assert_eq!(pending_cast.object_id, spell);
    assert_eq!(pending_cast.cost, ManaCost::NoCost);
    assert!(pending_cast.casting_permission_index.is_some());
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(victim)),
        })
        .expect("free target must commit");
    runner.advance_until_stack_empty();
    assert_eq!(runner.state().objects[&victim].zone, Zone::Graveyard);
}

fn setup_direct_graveyard_free_cast(optional: bool) -> (GameRunner, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_graveyard(P0, "Direct Free Removal", true)
        .from_oracle_text("Destroy target creature.")
        .with_mana_cost(ManaCost::generic(8))
        .id();
    let victim = scenario.add_creature(P1, "Direct Free Victim", 2, 2).id();
    let source = scenario.add_creature(P0, "Direct Free Source", 2, 2).id();
    let mut runner = scenario.build();
    let mut ability = engine::types::ability::ResolvedAbility::new(
        Effect::CastFromZone {
            target: TargetFilter::ParentTarget,
            without_paying_mana_cost: true,
            mode: CardPlayMode::Cast,
            cast_transformed: false,
            alt_ability_cost: None,
            constraint: None,
            duration: None,
            driver: CastFromZoneDriver::DuringResolution,
            mana_spend_permission: None,
        },
        vec![TargetRef::Object(spell)],
        source,
        P0,
    );
    ability.optional = optional;
    let stack_id = ObjectId(runner.state().next_object_id);
    runner.state_mut().next_object_id += 1;
    runner
        .state_mut()
        .stack
        .push_back(engine::types::game_state::StackEntry {
            id: stack_id,
            source_id: source,
            controller: P0,
            kind: engine::types::game_state::StackEntryKind::ActivatedAbility {
                source_id: source,
                ability: Box::new(ability),
            },
        });
    (runner, spell, victim)
}

fn pass_priority_and_assert_exact(runner: &mut GameRunner) {
    for _ in 0..2 {
        assert!(candidate_actions(runner.state())
            .iter()
            .any(|candidate| candidate.action == GameAction::PassPriority));
        assert!(legal_actions(runner.state()).contains(&GameAction::PassPriority));
        runner
            .act(GameAction::PassPriority)
            .expect("priority pass must be accepted");
    }
}

#[test]
fn priority_pass_resolving_targeted_free_cast_remains_offered_and_reaches_target_selection() {
    let (mut runner, spell, victim) = setup_direct_graveyard_free_cast(false);
    pass_priority_and_assert_exact(&mut runner);
    let WaitingFor::TargetSelection { pending_cast, .. } = &runner.state().waiting_for else {
        panic!("resolving the direct free cast must reach spell target selection")
    };
    assert_eq!(pending_cast.object_id, spell);
    assert_eq!(pending_cast.cost, ManaCost::NoCost);
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(victim)),
        })
        .expect("free removal target must commit");
}

#[test]
fn optional_effect_continuation_resuming_targeted_free_cast_remains_offered() {
    let (mut runner, spell, victim) = setup_direct_graveyard_free_cast(true);
    pass_priority_and_assert_exact(&mut runner);
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::OptionalEffectChoice { .. }
    ));
    let action = GameAction::DecideOptionalEffect { accept: true };
    assert!(candidate_actions(runner.state())
        .iter()
        .any(|candidate| candidate.action == action));
    assert!(legal_actions(runner.state()).contains(&action));
    runner
        .act(action)
        .expect("optional continuation response must resume the free cast");
    let WaitingFor::TargetSelection { pending_cast, .. } = &runner.state().waiting_for else {
        panic!("accepted continuation must reach the targeted spell root")
    };
    assert_eq!(pending_cast.object_id, spell);
    assert_eq!(pending_cast.cost, ManaCost::NoCost);
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(victim)),
        })
        .expect("free removal target must commit");
}

#[test]
fn convoke_targeting_cast_remains_offered_and_reaches_convoke_payment() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand(P0, "Convoke Offer Spell", true)
        .with_mana_cost(ManaCost::generic(1))
        .with_keyword(Keyword::Convoke)
        .with_ability(Effect::Destroy {
            target: TargetFilter::Typed(TypedFilter::creature()),
            cant_regenerate: false,
        })
        .id();
    let helper = scenario.add_creature(P0, "Convoke Helper", 1, 1).id();
    let target = scenario.add_creature(P1, "Convoke Target A", 2, 2).id();
    scenario.add_creature(P1, "Convoke Target B", 2, 2);
    let mut runner = scenario.build();
    let action = auto_cast_action(runner.state(), spell);
    assert!(candidate_actions(runner.state())
        .iter()
        .any(|candidate| candidate.action == action));
    assert!(legal_actions(runner.state()).contains(&action));
    runner.act(action).expect("Convoke cast must start");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::TargetSelection { .. }
    ));
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(target)),
        })
        .expect("Convoke spell target must be accepted");
    // CR 702.51a: The caster chooses which controlled creature pays the generic mana.
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ManaPayment {
            convoke_mode: Some(ConvokeMode::Convoke),
            ..
        }
    ));
    let (_, _, grouped) = engine::ai_support::legal_actions_full(runner.state());
    assert!(grouped.get(&helper).is_some_and(|actions| actions.iter().any(
        |action| matches!(action, GameAction::TapForConvoke { object_id, .. } if *object_id == helper)
    )));
}

#[test]
fn phyrexian_targeting_cast_remains_offered_and_reaches_phyrexian_choice() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand(P0, "Phyrexian Offer Spell", true)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::PhyrexianBlue],
            generic: 0,
        })
        .with_ability(Effect::Destroy {
            target: TargetFilter::Typed(TypedFilter::creature()),
            cant_regenerate: false,
        })
        .id();
    let target = scenario.add_creature(P1, "Phyrexian Target A", 2, 2).id();
    scenario.add_creature(P1, "Phyrexian Target B", 2, 2);
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(ManaType::Blue, spell, false, vec![])],
    );
    let mut runner = scenario.build();
    let action = auto_cast_action(runner.state(), spell);
    assert!(candidate_actions(runner.state())
        .iter()
        .any(|candidate| candidate.action == action));
    assert!(legal_actions(runner.state()).contains(&action));
    runner.act(action).expect("Phyrexian cast must start");
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(target)),
        })
        .expect("Phyrexian spell target must be accepted");
    // CR 107.4f: A Phyrexian blue symbol may be paid with blue mana or 2 life.
    let WaitingFor::PhyrexianPayment { shards, .. } = &runner.state().waiting_for else {
        panic!("ambiguous Phyrexian payment must remain interactive")
    };
    assert_eq!(shards.len(), 1);
    assert!(matches!(shards[0].options, ShardOptions::ManaOrLife));
}

#[test]
fn x_targeting_cast_remains_offered_and_reaches_choose_x() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand(P0, "X Offer Spell", true)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::X],
            generic: 0,
        })
        .with_ability(Effect::Destroy {
            target: TargetFilter::Typed(TypedFilter::creature()),
            cant_regenerate: false,
        })
        .id();
    let target = scenario.add_creature(P1, "X Target A", 2, 2).id();
    scenario.add_creature(P1, "X Target B", 2, 2);
    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(ManaType::Colorless, spell, false, vec![]),
            ManaUnit::new(ManaType::Colorless, spell, false, vec![]),
        ],
    );
    let mut runner = scenario.build();
    let action = auto_cast_action(runner.state(), spell);
    assert!(candidate_actions(runner.state())
        .iter()
        .any(|candidate| candidate.action == action));
    assert!(legal_actions(runner.state()).contains(&action));
    runner.act(action).expect("X cast must start");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::TargetSelection { .. }
    ));
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(target)),
        })
        .expect("X spell target must be accepted");
    // CR 601.2f: The variable mana component remains unresolved until X is chosen.
    let WaitingFor::ChooseXValue { pending_cast, .. } = &runner.state().waiting_for else {
        panic!("CR 601.2f: X must remain an announcement-time choice")
    };
    assert_eq!(pending_cast.object_id, spell);
}

#[test]
fn payable_morph_exact_cast_binds_face_down_three_and_completes() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let morph = scenario
        .add_creature_to_hand(P0, "Offer-Probe Morph", 4, 4)
        .with_mana_cost(ManaCost::NoCost)
        .with_keyword(Keyword::Morph(ManaCost::generic(5)))
        .id();
    scenario.with_mana_pool(
        P0,
        (0..3)
            .map(|_| ManaUnit::new(ManaType::Colorless, morph, false, vec![]))
            .collect(),
    );
    let mut runner = scenario.build();
    let action = auto_cast_action(runner.state(), morph);
    assert!(candidate_actions(runner.state())
        .iter()
        .any(|candidate| candidate.action == action));
    assert!(legal_actions(runner.state()).contains(&action));

    let commit = runner.cast(morph).commit();
    let stack_object = &commit.state().objects[&morph];
    assert_eq!(stack_object.zone, Zone::Stack);
    assert!(stack_object.face_down);
    assert_eq!(
        commit.state().stack_paid_facts.get(&morph),
        Some(&engine::types::game_state::StackPaidSnapshot {
            actual_mana_spent: 3,
            casting_variant: CastingVariant::FaceDown,
            ..Default::default()
        }),
        "CR 601.2h + CR 702.37c: the exact face-down authority must pay the bound {{3}} cost"
    );

    let outcome = commit.resolve();
    outcome.assert_zone(&[morph], Zone::Battlefield);
    assert!(outcome.state().objects[&morph].face_down);
}

fn exile_alt_cost_permission(
    granted_to: engine::types::PlayerId,
    cost: ManaCost,
) -> CastingPermission {
    CastingPermission::ExileWithAltCost {
        cost,
        cast_transformed: false,
        constraint: None,
        granted_to: Some(granted_to),
        resolution_cleanup: None,
        duration: None,
        graveyard_replacement: None,
        enters_with_counter: None,
        enters_with_modifications: Vec::new(),
        mana_spend_permission: None,
    }
}

#[test]
fn same_state_cast_origins_bind_distinct_pending_objects_and_permissions() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let add_targeted_spell = |scenario: &mut GameScenario, name: &str| {
        scenario
            .add_spell_to_hand_from_oracle(P0, name, true, "Destroy target creature.")
            .with_mana_cost(ManaCost::Cost {
                shards: vec![ManaCostShard::Blue],
                generic: 0,
            })
            .id()
    };
    let hand = add_targeted_spell(&mut scenario, "Hand Authority");
    let exile_a = add_targeted_spell(&mut scenario, "Exile Authority Zero");
    let exile_b = add_targeted_spell(&mut scenario, "Exile Authority One");
    scenario.add_creature(P1, "Authority Target A", 2, 2);
    scenario.add_creature(P1, "Authority Target B", 2, 2);
    scenario.add_basic_land(P0, ManaColor::Blue);
    scenario
        .add_creature(P0, "Authority Red Choice Source", 0, 3)
        .with_ability_definition(interactive_graveyard_mana_ability(ManaColor::Red));
    scenario.add_spell_to_graveyard(P0, "Authority Red Fodder", true);
    let mut runner = scenario.build();
    engine::game::zones::move_to_zone(runner.state_mut(), exile_a, Zone::Exile, &mut Vec::new());
    engine::game::zones::move_to_zone(runner.state_mut(), exile_b, Zone::Exile, &mut Vec::new());
    runner
        .state_mut()
        .objects
        .get_mut(&exile_a)
        .unwrap()
        .casting_permissions = vec![exile_alt_cost_permission(
        P0,
        ManaCost::Cost {
            shards: vec![ManaCostShard::Red],
            generic: 0,
        },
    )];
    runner
        .state_mut()
        .objects
        .get_mut(&exile_b)
        .unwrap()
        .casting_permissions = vec![
        exile_alt_cost_permission(
            P1,
            ManaCost::Cost {
                shards: vec![ManaCostShard::Red],
                generic: 0,
            },
        ),
        exile_alt_cost_permission(
            P0,
            ManaCost::Cost {
                shards: vec![ManaCostShard::Blue],
                generic: 0,
            },
        ),
    ];

    let cases = [
        (hand, None, true),
        (exile_a, Some(CastingPermissionIndex(0)), false),
        (exile_b, Some(CastingPermissionIndex(1)), true),
    ];
    let raw = candidate_actions(runner.state());
    let exact = legal_actions(runner.state());
    for (spell, permission, offered) in cases {
        let action = auto_cast_action(runner.state(), spell);
        assert!(raw.iter().any(|candidate| candidate.action == action));

        let mut disposable = GameRunner::from_state(runner.state().clone());
        disposable
            .act(action.clone())
            .expect("each casting authority must reach its exact pending root");
        let WaitingFor::TargetSelection { pending_cast, .. } = &disposable.state().waiting_for
        else {
            panic!("each authority must reach target selection")
        };
        assert_eq!(pending_cast.object_id, spell);
        assert_eq!(pending_cast.casting_permission_index, permission);
        assert_eq!(exact.contains(&action), offered);
    }
}
