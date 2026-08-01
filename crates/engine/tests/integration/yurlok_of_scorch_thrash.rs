use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{
    AbilityDefinition, AbilityKind, Effect, EffectScope, PlayerFilter, QuantityExpr,
    QuantityModification, QuantityRef, ReplacementCondition, ReplacementDefinition,
    ReplacementPlayerScope, TapStateChange, TargetFilter,
};
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaExpiry, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::replacements::ReplacementEvent;
use engine::types::statics::StaticMode;

const P2: PlayerId = PlayerId(2);
const YURLOK_ORACLE: &str = "Vigilance\nA player losing unspent mana causes that player to lose \
that much life.\n{1}, {T}: Each player adds {B}{R}{G}.";

fn mana(color: ManaType, count: usize) -> Vec<ManaUnit> {
    vec![ManaUnit::new(color, ObjectId(9_999), false, vec![]); count]
}

fn add_yurlok(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_creature_from_oracle(P0, "Yurlok of Scorch Thrash", 4, 4, YURLOK_ORACLE)
        .id()
}

fn advance_one_phase(runner: &mut GameRunner) -> Vec<GameEvent> {
    let mut events = Vec::new();
    engine::game::turns::advance_phase(runner.state_mut(), &mut events);
    events
}

fn pool_count(runner: &GameRunner, player: PlayerId, color: ManaType) -> usize {
    runner.state().players[player.0 as usize]
        .mana_pool
        .mana
        .iter()
        .filter(|unit| unit.color == color)
        .count()
}

#[test]
fn full_oracle_parses_and_activation_adds_brg_to_each_player() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, mana(ManaType::Colorless, 1));
    let yurlok = add_yurlok(&mut scenario);
    let mut runner = scenario.build();

    let object = &runner.state().objects[&yurlok];
    assert!(object
        .static_definitions
        .as_slice()
        .iter()
        .any(|def| def.mode == StaticMode::UnspentManaLossCausesLifeLoss));
    let ability_index = object
        .abilities
        .iter()
        .position(|ability| {
            matches!(*ability.effect, Effect::Mana { .. })
                && ability.player_scope == Some(PlayerFilter::All)
        })
        .expect("semantic Each-player mana ability should parse");
    assert!(object
        .abilities
        .iter()
        .all(|ability| !matches!(*ability.effect, Effect::Unimplemented { .. })));

    runner.activate(yurlok, ability_index).resolve();

    assert!(runner.state().objects[&yurlok].tapped);
    for player in [P0, P1, P2] {
        for color in [ManaType::Black, ManaType::Red, ManaType::Green] {
            assert_eq!(pool_count(&runner, player, color), 1);
        }
    }
    assert_eq!(runner.state().players[0].mana_pool.mana.len(), 3);
}

#[test]
fn phase_boundary_loses_actual_unspent_mana_once_per_player() {
    let mut scenario = GameScenario::new_n_player(3, 43);
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .with_life(P0, 20)
        .with_life(P1, 20)
        .with_life(P2, 20);
    scenario.with_mana_pool(P0, mana(ManaType::Black, 2));
    scenario.with_mana_pool(P1, mana(ManaType::Red, 1));
    scenario.with_mana_pool(P2, mana(ManaType::Green, 3));
    add_yurlok(&mut scenario);
    let mut runner = scenario.build();

    let events = advance_one_phase(&mut runner);

    assert_eq!(runner.state().phase, Phase::BeginCombat);
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    assert_eq!(runner.state().players[0].life, 18);
    assert_eq!(runner.state().players[1].life, 19);
    assert_eq!(runner.state().players[2].life, 17);
    assert!(runner
        .state()
        .players
        .iter()
        .all(|player| player.mana_pool.mana.is_empty()));
    let life_changes: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            GameEvent::LifeChanged { player_id, amount } => Some((*player_id, *amount)),
            _ => None,
        })
        .collect();
    assert_eq!(life_changes, vec![(P0, -2), (P1, -1), (P2, -3)]);
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, GameEvent::PhaseChanged { .. }))
            .count(),
        1
    );
}

#[test]
fn multiple_yurlok_markers_do_not_multiply_life_loss() {
    let mut scenario = GameScenario::new_n_player(3, 44);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P1, mana(ManaType::Red, 2));
    add_yurlok(&mut scenario);
    add_yurlok(&mut scenario);
    let mut runner = scenario.build();

    advance_one_phase(&mut runner);

    assert_eq!(runner.state().players[1].life, 18);
}

#[test]
fn retained_transformed_and_cant_lose_life_mana_have_distinct_outcomes() {
    let mut scenario = GameScenario::new_n_player(3, 45);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, {
        let mut pool = mana(ManaType::Red, 1);
        pool.extend(mana(ManaType::Black, 1));
        pool
    });
    scenario.with_mana_pool(P1, mana(ManaType::Green, 2));
    scenario.with_mana_pool(P2, mana(ManaType::Black, 2));
    add_yurlok(&mut scenario);
    scenario
        .add_creature_from_oracle(
            P0,
            "Red Mana Retainer",
            1,
            1,
            "You don't lose unspent red mana as steps and phases end.",
        )
        .id();
    scenario
        .add_creature_from_oracle(
            P1,
            "Mana Transformer",
            1,
            1,
            "If you would lose unspent mana, that mana becomes colorless instead.",
        )
        .id();
    scenario
        .add_creature_from_oracle(P2, "Life Lock", 1, 1, "Your life total can't change.")
        .id();
    let mut runner = scenario.build();

    let events = advance_one_phase(&mut runner);

    // P0 retained red but actually lost black, so only one life was lost.
    assert_eq!(runner.state().players[0].life, 19);
    assert_eq!(pool_count(&runner, P0, ManaType::Red), 1);
    // P1's units were transformed rather than lost.
    assert_eq!(runner.state().players[1].life, 20);
    assert_eq!(pool_count(&runner, P1, ManaType::Colorless), 2);
    // P2 really lost both mana, but CR 119.8 suppresses the life loss.
    assert_eq!(runner.state().players[2].life, 20);
    assert!(runner.state().players[2].mana_pool.mana.is_empty());

    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, GameEvent::ManaPoolEmptied { player_id, .. } if *player_id == P0))
            .count(),
        1
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, GameEvent::ManaRecolored { player_id, .. } if *player_id == P1))
            .count(),
        2
    );
    assert!(!events.iter().any(
        |event| matches!(event, GameEvent::LifeChanged { player_id, .. } if *player_id == P2)
    ));
}

#[test]
fn life_loss_replacement_choice_resumes_phase_drain_exactly_once() {
    let mut scenario = GameScenario::new_n_player(3, 46);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P1, mana(ManaType::Black, 1));
    add_yurlok(&mut scenario);

    let mut double = ReplacementDefinition::new(ReplacementEvent::LoseLife)
        .quantity_modification(QuantityModification::DOUBLE)
        .description("Double".to_string());
    double.valid_player = Some(ReplacementPlayerScope::Opponent);
    let mut plus_one = ReplacementDefinition::new(ReplacementEvent::LoseLife)
        .quantity_modification(QuantityModification::Plus { value: 1 })
        .description("Plus one".to_string());
    plus_one.valid_player = Some(ReplacementPlayerScope::Opponent);
    scenario
        .add_creature(P0, "Life Loss Replacements", 1, 1)
        .with_replacement_definition(double)
        .with_replacement_definition(plus_one);
    let mut runner = scenario.build();

    let mut events = advance_one_phase(&mut runner);
    let double_index = match &runner.state().waiting_for {
        WaitingFor::ReplacementChoice { candidates, .. } => candidates
            .iter()
            .position(|candidate| candidate.description == "Double")
            .expect("double replacement candidate"),
        waiting => panic!("expected life-loss replacement choice, got {waiting:?}"),
    };
    let result = runner
        .act(GameAction::ChooseReplacement {
            index: double_index,
        })
        .expect("replacement choice should resume the phase drain");
    events.extend(result.events);

    // Double applies first, then the sole remaining Plus-one replacement:
    // (1 * 2) + 1 = 3 life lost.
    assert_eq!(runner.state().players[1].life, 17);
    assert!(runner.state().players[1].mana_pool.mana.is_empty());
    assert_eq!(runner.state().phase, Phase::BeginCombat);
    assert!(runner.state().pending_phase_transition_progress.is_none());
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, GameEvent::LifeChanged { player_id, amount: -3 } if *player_id == P1))
            .count(),
        1
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(
                event,
                GameEvent::PhaseChanged {
                    phase: Phase::BeginCombat
                }
            ))
            .count(),
        1
    );
}

#[test]
fn prevented_life_loss_choice_resumes_phase_drain() {
    let mut scenario = GameScenario::new_n_player(3, 47);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P1, mana(ManaType::Black, 1));
    add_yurlok(&mut scenario);

    let mut double = ReplacementDefinition::new(ReplacementEvent::LoseLife)
        .quantity_modification(QuantityModification::DOUBLE)
        .description("Double".to_string());
    double.valid_player = Some(ReplacementPlayerScope::Opponent);
    let mut prevent = ReplacementDefinition::new(ReplacementEvent::LoseLife)
        .quantity_modification(QuantityModification::Prevent)
        .description("Prevent".to_string());
    prevent.valid_player = Some(ReplacementPlayerScope::Opponent);
    scenario
        .add_creature(P0, "Life Loss Prevention", 1, 1)
        .with_replacement_definition(double)
        .with_replacement_definition(prevent);
    let mut runner = scenario.build();

    let mut events = advance_one_phase(&mut runner);
    let prevent_index = match &runner.state().waiting_for {
        WaitingFor::ReplacementChoice { candidates, .. } => candidates
            .iter()
            .position(|candidate| candidate.description == "Prevent")
            .expect("prevent replacement candidate"),
        waiting => panic!("expected life-loss replacement choice, got {waiting:?}"),
    };
    let result = runner
        .act(GameAction::ChooseReplacement {
            index: prevent_index,
        })
        .expect("prevented replacement should resume phase drain");
    events.extend(result.events);

    assert_eq!(runner.state().players[1].life, 20);
    assert!(runner.state().players[1].mana_pool.mana.is_empty());
    assert_eq!(runner.state().phase, Phase::BeginCombat);
    assert!(runner.state().pending_phase_transition_progress.is_none());
    assert!(!events.iter().any(
        |event| matches!(event, GameEvent::LifeChanged { player_id, .. } if *player_id == P1)
    ));
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(
                event,
                GameEvent::PhaseChanged {
                    phase: Phase::BeginCombat
                }
            ))
            .count(),
        1
    );
}

#[test]
fn cross_event_life_loss_substitution_resumes_phase_drain_after_substitute() {
    let mut scenario = GameScenario::new_n_player(3, 48);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P1, mana(ManaType::Black, 1));
    add_yurlok(&mut scenario);

    let mut double = ReplacementDefinition::new(ReplacementEvent::LoseLife)
        .quantity_modification(QuantityModification::DOUBLE)
        .description("Double".to_string());
    double.valid_player = Some(ReplacementPlayerScope::Opponent);
    let mut substitute = ReplacementDefinition::new(ReplacementEvent::LoseLife)
        .execute(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::GainLife {
                amount: QuantityExpr::Ref {
                    qty: QuantityRef::EventContextAmount,
                },
                player: TargetFilter::Controller,
            },
        ))
        .description("Gain instead".to_string());
    substitute.valid_player = Some(ReplacementPlayerScope::Opponent);
    scenario
        .add_creature(P0, "Life Loss Substitution", 1, 1)
        .with_replacement_definition(double)
        .with_replacement_definition(substitute);
    let mut runner = scenario.build();

    let mut events = advance_one_phase(&mut runner);
    let substitute_index = match &runner.state().waiting_for {
        WaitingFor::ReplacementChoice { candidates, .. } => candidates
            .iter()
            .position(|candidate| candidate.description == "Gain instead")
            .expect("cross-event substitution candidate"),
        waiting => panic!("expected life-loss replacement choice, got {waiting:?}"),
    };
    let result = runner
        .act(GameAction::ChooseReplacement {
            index: substitute_index,
        })
        .expect("substitution should finish before phase drain resumes");
    events.extend(result.events);

    assert_eq!(runner.state().players[1].life, 20);
    assert_eq!(runner.state().players[0].life, 21);
    assert!(runner.state().players[1].mana_pool.mana.is_empty());
    assert_eq!(runner.state().phase, Phase::BeginCombat);
    assert!(runner.state().pending_phase_transition_progress.is_none());
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, GameEvent::LifeChanged { player_id, amount: 1 } if *player_id == P0))
            .count(),
        1
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(
                event,
                GameEvent::PhaseChanged {
                    phase: Phase::BeginCombat
                }
            ))
            .count(),
        1
    );
}

#[test]
fn interactive_cross_event_substitution_resumes_remaining_apnap_drain_once() {
    let mut scenario = GameScenario::new_n_player(3, 49);
    scenario.at_phase(Phase::EndCombat);
    scenario.with_mana_pool(P1, mana(ManaType::Black, 1));
    scenario.with_mana_pool(P2, {
        let mut pool = mana(ManaType::Green, 2);
        for unit in &mut pool {
            unit.expiry = Some(ManaExpiry::EndOfCombat);
        }
        pool
    });
    add_yurlok(&mut scenario);
    scenario
        .add_creature_from_oracle(
            P2,
            "Independent Green Retainer",
            1,
            1,
            "You don't lose unspent green mana as steps and phases end.",
        )
        .id();

    let mut double = ReplacementDefinition::new(ReplacementEvent::LoseLife)
        .quantity_modification(QuantityModification::DOUBLE)
        .description("Double".to_string());
    double.valid_player = Some(ReplacementPlayerScope::Opponent);
    let gain_branch = |amount| {
        AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: amount },
                player: TargetFilter::Controller,
            },
        )
    };
    let mut substitute = ReplacementDefinition::new(ReplacementEvent::LoseLife)
        .execute(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::ChooseOneOf {
                chooser: PlayerFilter::Controller,
                branches: vec![gain_branch(1), gain_branch(2)],
            },
        ))
        .description("Choose gain instead".to_string());
    substitute.valid_player = Some(ReplacementPlayerScope::Opponent);
    scenario
        .add_creature(P0, "Interactive Life Loss Substitution", 1, 1)
        .with_replacement_definition(double)
        .with_replacement_definition(substitute);
    let mut runner = scenario.build();

    let mut events = advance_one_phase(&mut runner);
    let substitute_index = match &runner.state().waiting_for {
        WaitingFor::ReplacementChoice { candidates, .. } => candidates
            .iter()
            .position(|candidate| candidate.description == "Choose gain instead")
            .expect("interactive substitution candidate"),
        waiting => panic!("expected life-loss replacement choice, got {waiting:?}"),
    };
    let replacement_result = runner
        .act(GameAction::ChooseReplacement {
            index: substitute_index,
        })
        .expect("selected substitute should surface its own choice");
    events.extend(replacement_result.events);
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ChooseOneOfBranch { .. }
    ));
    assert!(
        runner.state().pending_phase_transition_progress.is_some(),
        "the phase cursor must remain owned while the substitute waits"
    );
    assert_eq!(
        runner.state().players[2].mana_pool.mana.len(),
        2,
        "the next APNAP player must not drain before the substitute completes"
    );

    let branch_result = runner
        .act(GameAction::ChooseBranch { index: 1 })
        .expect("answering the substitute should resume and finish phase entry");
    events.extend(branch_result.events);

    assert_eq!(
        runner.state().players[0].life,
        22,
        "the selected two-life substitute runs exactly once"
    );
    assert_eq!(
        runner.state().players[1].life,
        20,
        "the original Yurlok life-loss event remains replaced"
    );
    assert_eq!(
        runner.state().players[2].life,
        20,
        "the independent active retention prevents actual mana loss"
    );
    assert!(runner.state().players[1].mana_pool.mana.is_empty());
    assert_eq!(runner.state().players[2].mana_pool.mana.len(), 2);
    assert!(runner.state().players[2]
        .mana_pool
        .mana
        .iter()
        .all(|unit| unit.expiry.is_none()));
    assert_eq!(runner.state().phase, Phase::PostCombatMain);
    assert!(runner.state().pending_phase_transition_progress.is_none());
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, GameEvent::LifeChanged { player_id, amount: 2 } if *player_id == P0))
            .count(),
        1
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, GameEvent::LifeChanged { player_id, .. } if *player_id == P2))
            .count(),
        0
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(
                event,
                GameEvent::PhaseChanged {
                    phase: Phase::PostCombatMain
                }
            ))
            .count(),
        1
    );
}

#[test]
fn nested_life_loss_choice_cannot_bypass_outer_substitute_phase_owner() {
    let mut scenario = GameScenario::new_n_player(3, 50);
    scenario.at_phase(Phase::EndCombat);
    scenario.with_mana_pool(P1, mana(ManaType::Black, 1));
    scenario.with_mana_pool(P2, {
        let mut pool = mana(ManaType::Green, 2);
        for unit in &mut pool {
            unit.expiry = Some(ManaExpiry::EndOfCombat);
        }
        pool
    });
    add_yurlok(&mut scenario);
    scenario
        .add_creature_from_oracle(
            P2,
            "Independent Green Retainer",
            1,
            1,
            "You don't lose unspent green mana as steps and phases end.",
        )
        .id();

    let mut outer_double = ReplacementDefinition::new(ReplacementEvent::LoseLife)
        .quantity_modification(QuantityModification::DOUBLE)
        .condition(ReplacementCondition::SourceTappedState { tapped: false })
        .description("Outer double".to_string());
    outer_double.valid_player = Some(ReplacementPlayerScope::Opponent);

    let terminal_rider = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::GainLife {
            amount: QuantityExpr::Fixed { value: 5 },
            player: TargetFilter::Controller,
        },
    );
    let nested_life_loss = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::LoseLife {
            amount: QuantityExpr::Fixed { value: 2 },
            target: Some(TargetFilter::Player),
        },
    )
    .sub_ability(terminal_rider);
    let tap_then_nested_life_loss = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::SetTapState {
            target: TargetFilter::SelfRef,
            scope: EffectScope::Single,
            state: TapStateChange::Tap,
        },
    )
    .sub_ability(nested_life_loss);
    let harmless_branch = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::GainLife {
            amount: QuantityExpr::Fixed { value: 1 },
            player: TargetFilter::Controller,
        },
    );
    let mut outer_substitute = ReplacementDefinition::new(ReplacementEvent::LoseLife)
        .execute(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::ChooseOneOf {
                chooser: PlayerFilter::Opponent,
                branches: vec![tap_then_nested_life_loss, harmless_branch],
            },
        ))
        .condition(ReplacementCondition::SourceTappedState { tapped: false })
        .description("Nested choice instead".to_string());
    outer_substitute.valid_player = Some(ReplacementPlayerScope::Opponent);

    let mut nested_double = ReplacementDefinition::new(ReplacementEvent::LoseLife)
        .quantity_modification(QuantityModification::DOUBLE)
        .condition(ReplacementCondition::SourceTappedState { tapped: true })
        .description("Nested double".to_string());
    nested_double.valid_player = Some(ReplacementPlayerScope::Opponent);
    let mut nested_prevent = ReplacementDefinition::new(ReplacementEvent::LoseLife)
        .quantity_modification(QuantityModification::Prevent)
        .condition(ReplacementCondition::SourceTappedState { tapped: true })
        .description("Nested prevent".to_string());
    nested_prevent.valid_player = Some(ReplacementPlayerScope::Opponent);

    scenario
        .add_creature(P0, "Nested Replacement Harness", 1, 1)
        .with_replacement_definition(outer_double)
        .with_replacement_definition(outer_substitute)
        .with_replacement_definition(nested_double)
        .with_replacement_definition(nested_prevent);
    let mut runner = scenario.build();

    let mut events = advance_one_phase(&mut runner);
    let substitute_index = match &runner.state().waiting_for {
        WaitingFor::ReplacementChoice { candidates, .. } => candidates
            .iter()
            .position(|candidate| candidate.description == "Nested choice instead")
            .expect("outer interactive substitute candidate"),
        waiting => panic!("expected outer replacement choice, got {waiting:?}"),
    };
    let substitute_result = runner
        .act(GameAction::ChooseReplacement {
            index: substitute_index,
        })
        .expect("outer substitute should surface its branch choice");
    events.extend(substitute_result.events);
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ChooseOneOfBranch { .. }
    ));

    let branch_result = runner
        .act(GameAction::ChooseBranch { index: 0 })
        .expect("selected branch should pause on its nested life-loss ordering");
    events.extend(branch_result.events);
    let nested_prevent_index = match &runner.state().waiting_for {
        WaitingFor::ReplacementChoice { candidates, .. } => candidates
            .iter()
            .position(|candidate| candidate.description == "Nested prevent")
            .expect("nested life-loss replacement candidate"),
        waiting => panic!("expected nested life-loss replacement choice, got {waiting:?}"),
    };
    assert_eq!(
        runner.state().players[2].mana_pool.mana.len(),
        2,
        "remaining APNAP mana must wait while the outer substitute frame is paused"
    );
    assert!(!events
        .iter()
        .any(|event| matches!(event, GameEvent::PhaseChanged { .. })));

    let nested_result = runner
        .act(GameAction::ChooseReplacement {
            index: nested_prevent_index,
        })
        .expect("nested choice should resume, but not bypass, the outer substitute");
    events.extend(nested_result.events);

    assert_eq!(
        runner.state().players[0].life,
        25,
        "the outer frame's five-life terminal rider resolves after nested prevention"
    );
    assert_eq!(
        runner.state().players[1].life,
        20,
        "both the original Yurlok loss and the nested two-life loss are replaced"
    );
    assert_eq!(
        runner.state().players[2].life,
        20,
        "the remaining APNAP player is still protected by its independent retention"
    );
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ChooseOneOfBranch { player: P2, .. }
    ));
    assert_eq!(
        runner.state().players[2].mana_pool.mana[0].expiry,
        Some(ManaExpiry::EndOfCombat),
        "the phase queue must remain untouched until every outer chooser finishes"
    );
    assert!(!events
        .iter()
        .any(|event| matches!(event, GameEvent::PhaseChanged { .. })));

    let final_branch_result = runner
        .act(GameAction::ChooseBranch { index: 1 })
        .expect("the final outer chooser should terminally complete the substitute");
    events.extend(final_branch_result.events);

    assert_eq!(runner.state().players[0].life, 26);
    assert!(runner.state().pending_phase_transition_progress.is_none());
    assert_eq!(runner.state().phase, Phase::PostCombatMain);
    assert_eq!(runner.state().players[2].mana_pool.mana.len(), 2);
    assert!(runner.state().players[2]
        .mana_pool
        .mana
        .iter()
        .all(|unit| unit.expiry.is_none()));

    let life_changes: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            GameEvent::LifeChanged { player_id, amount } => Some((*player_id, *amount)),
            _ => None,
        })
        .collect();
    assert_eq!(
        life_changes,
        vec![(P0, 5), (P0, 1)],
        "nested prevention and every outer chooser finish before phase completion"
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(
                event,
                GameEvent::PhaseChanged {
                    phase: Phase::PostCombatMain
                }
            ))
            .count(),
        1,
        "the typed phase owner completes exactly once"
    );
}

#[test]
fn nested_nonpreventing_life_loss_execute_terminally_resumes_phase_owner() {
    let mut scenario = GameScenario::new_n_player(2, 54);
    scenario.at_phase(Phase::EndCombat);
    scenario.with_mana_pool(P1, mana(ManaType::Black, 1));
    add_yurlok(&mut scenario);

    let mut outer_double = ReplacementDefinition::new(ReplacementEvent::LoseLife)
        .quantity_modification(QuantityModification::DOUBLE)
        .condition(ReplacementCondition::SourceTappedState { tapped: false })
        .description("Outer double".to_string());
    outer_double.valid_player = Some(ReplacementPlayerScope::Opponent);

    let terminal_rider = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::GainLife {
            amount: QuantityExpr::Fixed { value: 5 },
            player: TargetFilter::Controller,
        },
    );
    let nested_life_loss = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::LoseLife {
            amount: QuantityExpr::Fixed { value: 2 },
            target: Some(TargetFilter::Player),
        },
    )
    .sub_ability(terminal_rider);
    let tap_then_nested_life_loss = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::SetTapState {
            target: TargetFilter::SelfRef,
            scope: EffectScope::Single,
            state: TapStateChange::Tap,
        },
    )
    .sub_ability(nested_life_loss);
    let harmless_branch = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::GainLife {
            amount: QuantityExpr::Fixed { value: 1 },
            player: TargetFilter::Controller,
        },
    );
    let mut outer_substitute = ReplacementDefinition::new(ReplacementEvent::LoseLife)
        .execute(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::ChooseOneOf {
                chooser: PlayerFilter::Opponent,
                branches: vec![tap_then_nested_life_loss, harmless_branch],
            },
        ))
        .condition(ReplacementCondition::SourceTappedState { tapped: false })
        .description("Nested choice instead".to_string());
    outer_substitute.valid_player = Some(ReplacementPlayerScope::Opponent);

    let mut nested_double = ReplacementDefinition::new(ReplacementEvent::LoseLife)
        .quantity_modification(QuantityModification::DOUBLE)
        .condition(ReplacementCondition::SourceTappedState { tapped: true })
        .description("Nested double".to_string());
    nested_double.valid_player = Some(ReplacementPlayerScope::Opponent);
    let mut nested_plus = ReplacementDefinition::new(ReplacementEvent::LoseLife)
        .quantity_modification(QuantityModification::Plus { value: 1 })
        .condition(ReplacementCondition::SourceTappedState { tapped: true })
        .description("Nested plus one".to_string());
    nested_plus.valid_player = Some(ReplacementPlayerScope::Opponent);

    scenario
        .add_creature(P0, "Nested Execute Harness", 1, 1)
        .with_replacement_definition(outer_double)
        .with_replacement_definition(outer_substitute)
        .with_replacement_definition(nested_double)
        .with_replacement_definition(nested_plus);
    let mut runner = scenario.build();

    let mut events = advance_one_phase(&mut runner);
    let substitute_index = match &runner.state().waiting_for {
        WaitingFor::ReplacementChoice { candidates, .. } => candidates
            .iter()
            .position(|candidate| candidate.description == "Nested choice instead")
            .expect("outer interactive substitute candidate"),
        waiting => panic!("expected outer replacement choice, got {waiting:?}"),
    };
    let substitute_result = runner
        .act(GameAction::ChooseReplacement {
            index: substitute_index,
        })
        .expect("outer substitute should surface its branch choice");
    events.extend(substitute_result.events);
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ChooseOneOfBranch { player: P1, .. }
    ));

    let branch_result = runner
        .act(GameAction::ChooseBranch { index: 0 })
        .expect("selected branch should pause on nested life-loss ordering");
    events.extend(branch_result.events);
    let nested_double_index = match &runner.state().waiting_for {
        WaitingFor::ReplacementChoice { candidates, .. } => candidates
            .iter()
            .position(|candidate| candidate.description == "Nested double")
            .expect("nested Double candidate"),
        waiting => panic!("expected nested life-loss ordering choice, got {waiting:?}"),
    };

    let nested_result = runner
        .act(GameAction::ChooseReplacement {
            index: nested_double_index,
        })
        .expect("nonpreventing Execute must terminally resume the outer substitute");
    events.extend(nested_result.events);

    assert_eq!(
        runner.state().players[1].life,
        15,
        "the original Yurlok loss is replaced and nested (2 × 2) + 1 applies once"
    );
    assert_eq!(
        runner.state().players[0].life,
        25,
        "the outer terminal rider resolves exactly once"
    );
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    assert!(
        runner.state().pending_phase_transition_progress.is_none(),
        "no phase owner may remain parked after terminal Execute"
    );
    assert_eq!(runner.state().phase, Phase::PostCombatMain);
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(
                event,
                GameEvent::LifeChanged {
                    player_id: P1,
                    amount: -5
                }
            ))
            .count(),
        1,
        "the nested finalized life-loss event is delivered exactly once"
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(
                event,
                GameEvent::LifeChanged {
                    player_id: P0,
                    amount: 5
                }
            ))
            .count(),
        1,
        "the outer continuation resolves exactly once"
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(
                event,
                GameEvent::PhaseChanged {
                    phase: Phase::PostCombatMain
                }
            ))
            .count(),
        1,
        "the phase transition completes exactly once without a trailing prompt"
    );
}

#[test]
fn end_of_combat_retention_expiry_counts_as_actual_mana_loss() {
    let mut scenario = GameScenario::new_n_player(3, 51);
    scenario.at_phase(Phase::EndCombat);
    scenario.with_mana_pool(P1, {
        let mut pool = mana(ManaType::Red, 2);
        for unit in &mut pool {
            unit.expiry = Some(ManaExpiry::EndOfCombat);
        }
        pool
    });
    add_yurlok(&mut scenario);
    let mut runner = scenario.build();

    let events = advance_one_phase(&mut runner);

    assert_eq!(runner.state().phase, Phase::PostCombatMain);
    assert_eq!(runner.state().players[1].life, 18);
    assert!(runner.state().players[1].mana_pool.mana.is_empty());
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, GameEvent::LifeChanged { player_id, amount: -2 } if *player_id == P1))
            .count(),
        1
    );
}

#[test]
fn end_of_turn_retention_survives_cleanup_entry_then_counts_loss_at_cleanup_exit() {
    let mut scenario = GameScenario::new_n_player(3, 52);
    scenario.at_phase(Phase::End);
    scenario.with_mana_pool(P1, {
        let mut pool = mana(ManaType::Green, 2);
        for unit in &mut pool {
            unit.expiry = Some(ManaExpiry::EndOfTurn);
        }
        pool
    });
    add_yurlok(&mut scenario);
    let mut runner = scenario.build();

    let entry_events = advance_one_phase(&mut runner);

    assert_eq!(runner.state().phase, Phase::Cleanup);
    assert_eq!(runner.state().players[1].life, 20);
    assert_eq!(runner.state().players[1].mana_pool.mana.len(), 2);
    assert!(runner.state().players[1]
        .mana_pool
        .mana
        .iter()
        .all(|unit| unit.expiry == Some(ManaExpiry::EndOfTurn)));
    assert!(!entry_events
        .iter()
        .any(|event| matches!(event, GameEvent::LifeChanged { player_id, amount: -2 } if *player_id == P1)));

    let mut cleanup_events = Vec::new();
    assert!(
        engine::game::turns::execute_cleanup(runner.state_mut(), &mut cleanup_events).is_none()
    );
    assert!(runner.state().players[1]
        .mana_pool
        .mana
        .iter()
        .all(|unit| unit.expiry.is_none()));
    cleanup_events.extend(advance_one_phase(&mut runner));

    assert_eq!(runner.state().phase, Phase::Untap);
    assert_eq!(runner.state().players[1].life, 18);
    assert!(runner.state().players[1].mana_pool.mana.is_empty());
    assert_eq!(
        cleanup_events
            .iter()
            .filter(|event| matches!(event, GameEvent::LifeChanged { player_id, amount: -2 } if *player_id == P1))
            .count(),
        1
    );
}

#[test]
fn expired_retention_still_composes_with_another_active_retention() {
    let mut scenario = GameScenario::new_n_player(3, 53);
    scenario.at_phase(Phase::EndCombat);
    scenario.with_mana_pool(P1, {
        let mut pool = mana(ManaType::Red, 1);
        pool[0].expiry = Some(ManaExpiry::EndOfCombat);
        pool
    });
    add_yurlok(&mut scenario);
    scenario
        .add_creature_from_oracle(
            P1,
            "Independent Red Retainer",
            1,
            1,
            "You don't lose unspent red mana as steps and phases end.",
        )
        .id();
    let mut runner = scenario.build();

    let events = advance_one_phase(&mut runner);

    assert_eq!(runner.state().phase, Phase::PostCombatMain);
    assert_eq!(runner.state().players[1].life, 20);
    assert_eq!(runner.state().players[1].mana_pool.mana.len(), 1);
    assert_eq!(runner.state().players[1].mana_pool.mana[0].expiry, None);
    assert!(!events
        .iter()
        .any(|event| matches!(event, GameEvent::ManaPoolEmptied { player_id: P1, .. })));
}
