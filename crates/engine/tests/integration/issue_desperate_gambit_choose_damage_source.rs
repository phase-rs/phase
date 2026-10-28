//! Desperate Gambit — "Choose a source you control" must parse as
//! `ChooseDamageSource` with a You-controller candidate filter. Its later
//! coin-flip shields bind the exact selected source independently of that eligibility.

use engine::game::effects::deal_damage;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{
    AbilityKind, ControllerRef, DamageModification, Effect, QuantityExpr, ReplacementDefinition,
    ResolvedAbility, ShieldKind, TargetFilter, TargetRef, TypedFilter,
};
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::GameRng;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;

fn desperate_gambit_mana() -> ManaCost {
    ManaCost::Cost {
        shards: vec![ManaCostShard::Red],
        generic: 0,
    }
}

fn source_damage_to_player(source: ObjectId, amount: i32) -> ResolvedAbility {
    ResolvedAbility::new(
        Effect::DealDamage {
            amount: QuantityExpr::Fixed { value: amount },
            target: TargetFilter::Player,
            damage_source: None,
            excess: None,
        },
        vec![TargetRef::Player(P1)],
        source,
        P0,
    )
}

const DESPERATE_GAMBIT: &str = "Choose a source you control and flip a coin. If you win the flip, \
    the next time that source would deal damage this turn, it deals double that damage instead. \
    If you lose the flip, the next time it would deal damage this turn, prevent that damage.";

fn typed_you_control(filter: &TargetFilter) -> Option<&TypedFilter> {
    match filter {
        TargetFilter::Typed(tf) if tf.controller == Some(ControllerRef::You) => Some(tf),
        _ => None,
    }
}

fn add_mana(runner: &mut engine::game::scenario::GameRunner, mana: &[ManaType]) {
    let dummy = ObjectId(0);
    let pool = &mut runner
        .state_mut()
        .players
        .iter_mut()
        .find(|p| p.id == P0)
        .unwrap()
        .mana_pool;
    for m in mana {
        pool.add(ManaUnit::new(*m, dummy, false, vec![]));
    }
}

fn drive_desperate_gambit_resolution(
    runner: &mut engine::game::scenario::GameRunner,
    chosen_source: ObjectId,
) {
    for _ in 0..96 {
        match &runner.state().waiting_for {
            WaitingFor::DamageSourceChoice { options, .. } => {
                assert!(
                    options
                        .iter()
                        .any(|option| option.identity.reference.object_id == chosen_source),
                    "chosen source must be offered, got options {options:?}"
                );
                runner
                    .act(GameAction::ChooseDamageSource {
                        source: engine::types::identifiers::ObjectIncarnationRef::of(
                            chosen_source,
                            runner.state().objects[&chosen_source].incarnation,
                        ),
                    })
                    .expect("choose damage source");
            }
            WaitingFor::CoinFlipKeepChoice { .. } => {
                runner
                    .act(GameAction::SelectCoinFlips {
                        keep_indices: vec![0],
                    })
                    .expect("resolve coin flip keep choice");
            }
            WaitingFor::ManaPayment { .. } => {
                runner.act(GameAction::PassPriority).expect("pay mana");
            }
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() {
                    break;
                }
                runner
                    .act(GameAction::PassPriority)
                    .expect("pass priority to resolve Desperate Gambit");
            }
            other => panic!("unexpected prompt resolving Desperate Gambit: {other:?}"),
        }
    }
    runner.advance_until_stack_empty();
}

fn resolve_desperate_gambit_through_source_choice(
    runner: &mut engine::game::scenario::GameRunner,
    gambit: ObjectId,
    chosen_source: ObjectId,
) {
    runner
        .act(GameAction::CastSpell {
            object_id: gambit,
            card_id: runner.state().objects[&gambit].card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast Desperate Gambit");

    drive_desperate_gambit_resolution(runner, chosen_source);
}

#[test]
fn desperate_gambit_oracle_parses_choose_damage_source_you_control() {
    let parsed = parse_oracle_text(
        DESPERATE_GAMBIT,
        "Desperate Gambit",
        &[],
        &["Instant".to_string()],
        &[],
    );
    let spell = parsed
        .abilities
        .iter()
        .find(|a| matches!(a.kind, AbilityKind::Spell))
        .expect("Desperate Gambit must have a spell ability");
    match spell.effect.as_ref() {
        Effect::ChooseDamageSource { source_filter } => {
            assert!(
                typed_you_control(source_filter).is_some(),
                "expected You-controller source filter, got {source_filter:?}"
            );
        }
        other => panic!("expected ChooseDamageSource head, got {other:?}"),
    }
}

#[test]
fn desperate_gambit_damage_source_choice_excludes_opponent_sources() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let gambit = scenario
        .add_spell_to_hand_from_oracle(P0, "Desperate Gambit", true, DESPERATE_GAMBIT)
        .with_mana_cost(desperate_gambit_mana())
        .id();
    let p0_source = scenario.add_creature(P0, "Your Shaman", 2, 2).id();
    let p1_source = scenario.add_creature(P1, "Their Bear", 2, 2).id();

    let mut runner = scenario.build();
    add_mana(&mut runner, &[ManaType::Red]);

    runner
        .act(GameAction::CastSpell {
            object_id: gambit,
            card_id: runner.state().objects[&gambit].card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast Desperate Gambit");

    for _ in 0..32 {
        match &runner.state().waiting_for {
            WaitingFor::DamageSourceChoice { options, .. } => {
                assert!(
                    options
                        .iter()
                        .any(|option| option.identity.reference.object_id == p0_source),
                    "P0's source must be offered"
                );
                assert!(
                    !options
                        .iter()
                        .any(|option| option.identity.reference.object_id == p1_source),
                    "P1's source must not be offered under 'you control'"
                );
                runner
                    .act(GameAction::ChooseDamageSource {
                        source: engine::types::identifiers::ObjectIncarnationRef::of(
                            p0_source,
                            runner.state().objects[&p0_source].incarnation,
                        ),
                    })
                    .expect("choose P0 source");
                return;
            }
            WaitingFor::ManaPayment { .. } => {
                runner.act(GameAction::PassPriority).expect("pay mana");
            }
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("pass priority toward source choice");
            }
            other => panic!("unexpected prompt: {other:?}"),
        }
    }
    panic!("Desperate Gambit never prompted for damage source choice");
}

fn shield_targets_chosen_source(shield: &ReplacementDefinition, source: ObjectId) -> bool {
    shield
        .damage_source_filter
        .as_ref()
        .is_some_and(|filter| match filter {
            TargetFilter::SpecificObject { id } => *id == source,
            TargetFilter::And { filters } => filters.iter().any(
                |filter| matches!(filter, TargetFilter::SpecificObject { id } if *id == source),
            ),
            _ => false,
        })
}

fn shields_for_chosen_source(
    runner: &engine::game::scenario::GameRunner,
    source: ObjectId,
) -> Vec<&ReplacementDefinition> {
    let mut shields = runner
        .state()
        .objects
        .get(&source)
        .map(|obj| {
            obj.replacement_definitions
                .iter_unchecked()
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    shields.extend(
        runner
            .state()
            .pending_damage_replacements
            .iter()
            .filter(|shield| shield_targets_chosen_source(shield, source)),
    );
    shields
}

fn assert_one_shot_shield_on_source(
    runner: &engine::game::scenario::GameRunner,
    source: ObjectId,
    expect_double: bool,
) {
    let shields = shields_for_chosen_source(runner, source);
    let double_shield = shields.iter().find(|s| {
        matches!(s.shield_kind, ShieldKind::DamageReplacementOneShot)
            && s.damage_modification == Some(DamageModification::Double)
    });
    let prevent_shield = shields
        .iter()
        .find(|s| matches!(s.shield_kind, ShieldKind::PreventionOneShot));
    if expect_double {
        assert!(
            double_shield.is_some(),
            "win branch must install double shield on chosen source, got {:?}",
            shields,
        );
        assert!(
            prevent_shield.is_none(),
            "win branch must not install prevention shield"
        );
    } else {
        assert!(
            prevent_shield.is_some(),
            "lose branch must install prevention shield on chosen source, got {:?}",
            shields,
        );
        assert!(
            double_shield.is_none(),
            "lose branch must not install double shield"
        );
    }
}

fn assert_damage_outcome(
    runner: &mut engine::game::scenario::GameRunner,
    source: ObjectId,
    expect_double: bool,
) {
    let mut events = Vec::<GameEvent>::new();
    deal_damage::resolve(
        runner.state_mut(),
        &source_damage_to_player(source, 3),
        &mut events,
    )
    .expect("damage resolves");

    if expect_double {
        assert_eq!(
            runner.state().players[P1.0 as usize].life,
            14,
            "win branch must double 3 → 6",
        );
    } else {
        assert_eq!(
            runner.state().players[P1.0 as usize].life,
            20,
            "lose branch must prevent damage",
        );
    }
}

#[test]
fn desperate_gambit_win_branch_installs_double_shield_on_chosen_source() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let gambit = scenario
        .add_spell_to_hand_from_oracle(P0, "Desperate Gambit", true, DESPERATE_GAMBIT)
        .with_mana_cost(desperate_gambit_mana())
        .id();
    let source = scenario.add_creature(P0, "Damage Source", 2, 2).id();

    let mut runner = scenario.build();
    runner.state_mut().rng = GameRng::seed_from_u64(0);
    add_mana(&mut runner, &[ManaType::Red]);
    resolve_desperate_gambit_through_source_choice(&mut runner, gambit, source);

    assert_one_shot_shield_on_source(&runner, source, true);
    assert_damage_outcome(&mut runner, source, true);
}

#[test]
fn desperate_gambit_lose_branch_installs_prevention_shield_on_chosen_source() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let gambit = scenario
        .add_spell_to_hand_from_oracle(P0, "Desperate Gambit", true, DESPERATE_GAMBIT)
        .with_mana_cost(desperate_gambit_mana())
        .id();
    let source = scenario.add_creature(P0, "Damage Source", 2, 2).id();

    let mut runner = scenario.build();
    runner.state_mut().rng = GameRng::seed_from_u64(1);
    add_mana(&mut runner, &[ManaType::Red]);
    resolve_desperate_gambit_through_source_choice(&mut runner, gambit, source);

    assert_one_shot_shield_on_source(&runner, source, false);
    assert_damage_outcome(&mut runner, source, false);
}

fn assert_source_program_complete(runner: &engine::game::scenario::GameRunner) {
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    assert!(runner.state().stack.is_empty());
    assert!(runner.state().resolution_stack.is_empty());
    assert!(runner.state().deferred_triggers.is_empty());
    assert!(runner.state().owed_spell_final_part().is_none());
    assert!(runner.state().pending_replacement.is_none());
    assert!(runner.state().pending_combat_lifelink.is_none());
}

#[test]
fn full_paid_gambit_public_choice_both_outcomes_survive_donate_and_current_restore() {
    use engine::types::identifiers::ObjectIncarnationRef;
    for (seed, won, expected_life) in [(0, true, 18), (1, false, 20)] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let gambit = scenario
            .add_spell_to_hand_from_oracle(P0, "Desperate Gambit", true, DESPERATE_GAMBIT)
            .with_mana_cost(desperate_gambit_mana())
            .id();
        let source = scenario
            .add_creature_from_oracle(
                P0,
                "Vulshok Sorcerer",
                1,
                1,
                "Haste\n{T}: This creature deals 1 damage to any target.",
            )
            .id();
        let unowned = scenario.add_creature(P1, "Their Bear", 2, 2).id();
        let donate = scenario
            .add_spell_to_hand_from_oracle(
                P0,
                "Donate",
                false,
                "Target player gains control of target permanent you control.",
            )
            .with_mana_cost(ManaCost::Cost {
                shards: vec![ManaCostShard::Blue],
                generic: 2,
            })
            .id();
        let mut runner = scenario.build();
        runner.state_mut().rng_seed = seed;
        runner.state_mut().rng_word_pos = 0;
        runner.state_mut().rng = GameRng::seed_from_u64(seed);
        add_mana(
            &mut runner,
            &[
                ManaType::Red,
                ManaType::Blue,
                ManaType::Colorless,
                ManaType::Colorless,
            ],
        );
        runner.cast(gambit).resolve();
        let WaitingFor::DamageSourceChoice {
            player, options, ..
        } = &runner.state().waiting_for
        else {
            panic!("complete printed DG must unconditionally choose its source before flipping");
        };
        assert_eq!(*player, P0);
        assert!(options
            .iter()
            .any(|option| option.identity.reference.object_id == source));
        assert!(!options
            .iter()
            .any(|option| option.identity.reference.object_id == unowned));
        let reference = options
            .iter()
            .find(|option| option.identity.reference.object_id == source)
            .unwrap()
            .identity
            .reference;
        let offered = options.clone();
        let ai = engine::ai_support::candidate_actions_exact(runner.state());
        let expected_actions: Vec<_> = offered
            .iter()
            .map(|option| GameAction::ChooseDamageSource {
                source: option.identity.reference,
            })
            .collect();
        assert_eq!(
            ai.into_iter()
                .map(|candidate| candidate.action)
                .collect::<Vec<_>>(),
            expected_actions
        );
        assert_chosen_capture_private_in_both_envelopes(runner.state(), &offered);
        let saved = serde_json::to_value(runner.state()).unwrap();
        *runner.state_mut() = serde_json::from_value(saved.clone()).unwrap();
        let before = serde_json::to_value(runner.state()).unwrap();
        assert!(engine::game::engine::apply(
            runner.state_mut(),
            P1,
            GameAction::ChooseDamageSource { source: reference }
        )
        .is_err());
        assert_eq!(serde_json::to_value(runner.state()).unwrap(), before);
        let forged = ObjectIncarnationRef::of(source, reference.incarnation.wrapping_add(1));
        assert!(runner
            .act(GameAction::ChooseDamageSource { source: forged })
            .is_err());
        assert_eq!(serde_json::to_value(runner.state()).unwrap(), before);
        let answer = runner
            .act(GameAction::ChooseDamageSource { source: reference })
            .unwrap();
        let flips: Vec<_> = answer
            .events
            .iter()
            .filter_map(|event| match event {
                GameEvent::CoinFlipped { player_id, won } => Some((*player_id, *won)),
                _ => None,
            })
            .collect();
        assert_eq!(
            flips,
            vec![(P0, won)],
            "the printed conjunction performs ONE real coin flip"
        );
        runner.advance_until_stack_empty();
        assert_source_program_complete(&runner);
        assert_one_shot_shield_on_source(&runner, source, won);
        let installed = serde_json::to_value(runner.state()).unwrap();
        *runner.state_mut() = serde_json::from_value(installed).unwrap();
        runner
            .cast(donate)
            .target_player(P1)
            .target_object(source)
            .resolve();
        assert_eq!(
            runner.state().objects[&source].controller,
            P1,
            "actual paid Donate changes the selected source's controller"
        );
        assert_source_program_complete(&runner);
        for _ in 0..4 {
            if matches!(runner.state().waiting_for, WaitingFor::Priority { player } if player == P1)
            {
                break;
            }
            runner.act(GameAction::PassPriority).unwrap();
        }
        runner.activate(source, 0).target_player(P0).resolve();
        assert_eq!(
            runner.state().players[0].life,
            expected_life,
            "initial You eligibility must not become a continuing DG source predicate"
        );
        assert_source_program_complete(&runner);
    }
}

fn assert_chosen_capture_private_in_both_envelopes(
    state: &engine::types::game_state::GameState,
    options: &[engine::types::game_state::PromptSourceBinding],
) {
    use engine::game::derived_views::ClientGameStateRef;
    for viewer in [P0, P1] {
        let filtered = engine::game::visibility::filter_state_for_viewer(state, viewer);
        for envelope in [
            serde_json::to_value(ClientGameStateRef::wrap(state, Some(viewer))).unwrap(),
            serde_json::to_value(ClientGameStateRef::wrap_filtered(
                state,
                &filtered,
                Some(viewer),
            ))
            .unwrap(),
        ] {
            let wire = serde_json::to_string(&envelope).unwrap();
            for private in [
                "damage_source_candidates",
                "damage_source_choice",
                "selected_damage_source",
                "damage_opportunity",
            ] {
                assert!(
                    !wire.contains(private),
                    "{private} remains private in BOTH public envelope routes"
                );
            }
            assert_eq!(
                envelope["state"]["waiting_for"]["options"],
                serde_json::to_value(options).unwrap(),
                "the complete lawful exact public bindings remain visible"
            );
        }
    }
}

#[test]
fn paid_public_source_domain_includes_permanent_spells_referred_departures_and_face_up_command() {
    use engine::game::zones::create_object;
    use engine::types::game_state::StackEntryKind;
    use engine::types::identifiers::{CardId, ObjectIncarnationRef};
    use engine::types::zones::Zone;
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let permanent_spell = scenario
        .add_artifact_to_hand_from_oracle(
            P0,
            "Darksteel Relic",
            "Indestructible (Effects that say \"destroy\" don't destroy this artifact.)",
        )
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let nondamaging = scenario
        .add_artifact_from_oracle(
            P0,
            "Darksteel Relic",
            "Indestructible (Effects that say \"destroy\" don't destroy this artifact.)",
        )
        .id();
    let mogg = scenario
        .add_creature_from_oracle(
            P0,
            "Mogg Fanatic",
            1,
            1,
            "Sacrifice this creature: It deals 1 damage to any target.",
        )
        .id();
    let reverse = scenario.add_spell_to_hand_from_oracle(P0, "Reverse Damage", true,
        "The next time a source of your choice would deal damage to you this turn, prevent that damage. You gain life equal to the damage prevented this way.")
        .with_mana_cost(ManaCost::Cost { shards: vec![ManaCostShard::White, ManaCostShard::White], generic: 1 }).id();
    let mut runner = scenario.build();
    add_mana(
        &mut runner,
        &[ManaType::White, ManaType::White, ManaType::Colorless],
    );
    let face_up = create_object(
        runner.state_mut(),
        CardId(800),
        P0,
        "Face-up command source".into(),
        Zone::Command,
    );
    let face_down = create_object(
        runner.state_mut(),
        CardId(801),
        P0,
        "Hidden command card".into(),
        Zone::Command,
    );
    runner
        .state_mut()
        .objects
        .get_mut(&face_down)
        .unwrap()
        .face_down = true;
    let unrelated = create_object(
        runner.state_mut(),
        CardId(802),
        P0,
        "Unrelated graveyard".into(),
        Zone::Graveyard,
    );
    let library = create_object(
        runner.state_mut(),
        CardId(803),
        P0,
        "Unrelated library".into(),
        Zone::Library,
    );
    let unnamed = create_object(
        runner.state_mut(),
        CardId(804),
        P0,
        "Hidden printed identity".into(),
        Zone::Battlefield,
    );
    runner
        .state_mut()
        .objects
        .get_mut(&unnamed)
        .unwrap()
        .face_down = true;
    runner.cast(permanent_spell).commit();
    let departure = ObjectIncarnationRef::of(mogg, runner.state().objects[&mogg].incarnation);
    runner
        .act(GameAction::ActivateAbility {
            source_id: mogg,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        })
        .unwrap();
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::TargetSelection { .. }
    ));
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Player(P0)),
        })
        .unwrap();
    assert_eq!(
        runner.state().objects[&mogg].zone,
        Zone::Graveyard,
        "the source actually departed while paying its printed cost"
    );
    let ability_object = runner.state().stack.iter().find(|entry| matches!(entry.kind, StackEntryKind::ActivatedAbility { source_id, .. } if source_id == mogg)).unwrap().id;
    runner.cast(reverse).resolve();
    let WaitingFor::DamageSourceChoice { options, .. } = &runner.state().waiting_for else {
        panic!("complete paid Reverse Damage reaches Source enumeration");
    };
    for id in [permanent_spell, nondamaging, face_up, unnamed] {
        assert!(
            options
                .iter()
                .any(|option| option.identity.reference.object_id == id),
            "lawful Source {id:?} is offered"
        );
    }
    assert!(
        options
            .iter()
            .any(|option| option.identity.reference == departure),
        "the departed exact source referred to by the actual activated ability remains a Source"
    );
    for id in [face_down, unrelated, library, ability_object] {
        assert!(
            !options
                .iter()
                .any(|option| option.identity.reference.object_id == id),
            "non-source/private/unreferred row {id:?} must be excluded"
        );
    }
    assert_eq!(
        options
            .iter()
            .find(|option| option.identity.reference.object_id == unnamed)
            .unwrap()
            .display_name,
        "",
        "a lawful face-down battlefield source never publishes its printed identity"
    );
    assert_chosen_capture_private_in_both_envelopes(runner.state(), options);
    let prompt = runner.state().clone();
    for legacy in [false, true] {
        for wire in [false, true] {
            let mut saved = if wire {
                serde_json::to_value(
                    engine::types::resolution::ResolutionStateWire::from_game_state(prompt.clone()),
                )
                .unwrap()
            } else {
                serde_json::to_value(&prompt).unwrap()
            };
            if legacy {
                saved["waiting_for"]["options"] = serde_json::json!(options
                    .iter()
                    .map(|option| option.identity.reference.object_id)
                    .collect::<Vec<_>>());
                let carrier = if wire {
                    "resolution_frames"
                } else {
                    "resolution_stack"
                };
                for frame in saved[carrier]["frames"].as_array_mut().unwrap() {
                    if frame["type"] == "AbilityContinuation" {
                        frame["data"]["pending"]
                            .as_object_mut()
                            .unwrap()
                            .remove("damage_source_candidates");
                    }
                }
                saved
                    .as_object_mut()
                    .unwrap()
                    .remove("resolved_rules_journal");
            }
            let state = if wire {
                serde_json::from_value::<engine::types::resolution::ResolutionStateWire>(saved)
                    .unwrap()
                    .into_game_state()
            } else {
                serde_json::from_value(saved).unwrap()
            };
            let mut resumed = engine::game::scenario::GameRunner::from_state(state);
            let WaitingFor::DamageSourceChoice {
                options: restored, ..
            } = &resumed.state().waiting_for
            else {
                panic!("restored actual source prompt");
            };
            assert!(restored.iter().any(|option| option.identity.reference == departure), "authentic scalar options recover the exact departed typed referent rather than its graveyard birth");
            resumed
                .act(GameAction::ChooseDamageSource { source: departure })
                .unwrap();
            resumed.advance_until_stack_empty();
            assert_eq!(
                resumed.state().players[0].life,
                21,
                "the selected departed source's actual damage is prevented and gains one"
            );
            assert_eq!(
                resumed.state().objects[&permanent_spell].zone,
                Zone::Battlefield
            );
            assert_source_program_complete(&resumed);
        }
    }
}

#[test]
fn paid_gambit_selected_krark_branch_current_and_sufficient_legacy_restore_then_blink_excludes_new_birth(
) {
    use engine::types::game_state::{RandomDraw, TrustedGameStateEnvelope};
    use engine::types::resolution::ResolutionStateWire;
    use engine::types::zones::Zone;
    use rand::Rng;
    fn erase_chooser(node: &mut serde_json::Value) {
        match node {
            serde_json::Value::Object(object) => {
                if object.get("type").and_then(serde_json::Value::as_str)
                    == Some("ChosenDamageSource")
                {
                    object.remove("chooser");
                }
                for child in object.values_mut() {
                    erase_chooser(child);
                }
            }
            serde_json::Value::Array(array) => {
                for child in array {
                    erase_chooser(child);
                }
            }
            _ => {}
        }
    }
    let seed = (0..100_000)
        .find(|seed| {
            let mut rng = GameRng::seed_from_u64(*seed);
            rng.draw(RandomDraw::Outcome).random_bool(0.5)
                != rng.draw(RandomDraw::Outcome).random_bool(0.5)
        })
        .unwrap();
    for restore_mode in 0..3 {
        let legacy = restore_mode == 1;
        for wire in [false, true] {
            let mut scenario = GameScenario::new();
            scenario.at_phase(Phase::PreCombatMain);
            let source = scenario
                .add_creature_from_oracle(
                    P0,
                    "Vulshok Sorcerer",
                    1,
                    1,
                    "Haste\n{T}: This creature deals 1 damage to any target.",
                )
                .id();
            scenario.add_artifact_from_oracle(
                P0,
                "Krark's Thumb",
                "If you would flip a coin, instead flip two coins and ignore one.",
            );
            let unrelated = scenario
                .add_creature(P0, "Unrelated moving object", 2, 2)
                .id();
            let gambit = scenario
                .add_spell_to_hand_from_oracle(P0, "Desperate Gambit", true, DESPERATE_GAMBIT)
                .with_mana_cost(desperate_gambit_mana())
                .id();
            let blink = scenario.add_spell_to_hand_from_oracle(P0, "Cloudshift", true, "Exile target creature you control, then return that card to the battlefield under your control.")
                .with_mana_cost(ManaCost::Cost { shards: vec![ManaCostShard::White], generic: 0 }).id();
            let mut runner = scenario.build();
            add_mana(&mut runner, &[ManaType::Red, ManaType::White]);
            engine::game::zones::move_to_zone(
                runner.state_mut(),
                unrelated,
                Zone::Exile,
                &mut Vec::new(),
            );
            runner.state_mut().rng_seed = seed;
            runner.state_mut().rng_word_pos = 0;
            runner.state_mut().rng = GameRng::seed_from_u64(seed);
            runner.cast(gambit).resolve();
            let WaitingFor::DamageSourceChoice { options, .. } = &runner.state().waiting_for else {
                panic!("actual paid source prompt");
            };
            let source_ref = options
                .iter()
                .find(|option| option.identity.reference.object_id == source)
                .unwrap()
                .identity
                .reference;
            // Both canonical owners restore an actual unanswered prompt. The
            // historical writer advertised scalar options before exact actions.
            let carrier = if wire {
                "resolution_frames"
            } else {
                "resolution_stack"
            };
            let mut prompt = if wire {
                serde_json::to_value(ResolutionStateWire::from_game_state(runner.state().clone()))
                    .unwrap()
            } else {
                serde_json::to_value(runner.state()).unwrap()
            };
            if legacy {
                prompt["waiting_for"]["options"] = serde_json::json!(options
                    .iter()
                    .map(|option| option.identity.reference.object_id)
                    .collect::<Vec<_>>());
                for frame in prompt[carrier]["frames"].as_array_mut().unwrap() {
                    if frame["type"] == "AbilityContinuation" {
                        frame["data"]["pending"]
                            .as_object_mut()
                            .unwrap()
                            .remove("damage_source_candidates");
                    }
                }
                erase_chooser(&mut prompt);
            }
            *runner.state_mut() = if wire {
                serde_json::from_value::<ResolutionStateWire>(prompt)
                    .unwrap()
                    .into_game_state()
            } else {
                serde_json::from_value(prompt).unwrap()
            };
            runner
                .act(GameAction::ChooseDamageSource { source: source_ref })
                .unwrap();
            let WaitingFor::CoinFlipKeepChoice {
                results,
                keep_count,
                ..
            } = &runner.state().waiting_for
            else {
                panic!("actual Krark replacement parks before its single chosen flip");
            };
            assert_eq!(*keep_count, 1);
            assert_eq!(results.len(), 2);
            assert_ne!(results[0], results[1]);
            let keep = results.iter().position(|won| *won).unwrap();
            let choice = runner
                .state()
                .active_coin_flip_frame()
                .unwrap()
                .branch_context
                .damage_source_choice
                .clone()
                .unwrap();
            assert_eq!(
                choice.source.as_ref().unwrap().identity.reference,
                source_ref
            );
            let mut paused = if wire {
                serde_json::to_value(ResolutionStateWire::from_game_state(runner.state().clone()))
                    .unwrap()
            } else {
                serde_json::to_value(runner.state()).unwrap()
            };
            if restore_mode != 0 {
                for frame in paused[carrier]["frames"].as_array_mut().unwrap() {
                    if frame["type"] == "CoinFlip" {
                        frame["data"]["branch_context"]
                            .as_object_mut()
                            .unwrap()
                            .remove("damage_source_choice");
                    }
                }
            }
            if restore_mode == 2 {
                paused
                    .as_object_mut()
                    .unwrap()
                    .remove("zone_changes_this_turn");
                assert!(paused["resolved_rules_journal"]["entries"].as_array().is_some_and(|entries| !entries.is_empty()), "the actual retained structural journal supplies this resident branch's exact selection without a no-move proof");
            }
            if legacy {
                paused["last_chosen_damage_source"] = serde_json::json!({"source_id":source, "source_filter":choice.candidate_filter});
                paused
                    .as_object_mut()
                    .unwrap()
                    .remove("resolved_rules_journal");
                erase_chooser(&mut paused);
                let mut erased = paused.clone();
                erased
                    .as_object_mut()
                    .unwrap()
                    .remove("zone_changes_this_turn");
                let bytes = serde_json::to_vec(&erased).unwrap();
                let error = if wire {
                    serde_json::from_value::<ResolutionStateWire>(erased.clone())
                        .unwrap_err()
                        .to_string()
                } else {
                    serde_json::from_value::<engine::types::game_state::GameState>(erased.clone())
                        .unwrap_err()
                        .to_string()
                };
                assert!(error.contains("unrecoverable selected-source identity"), "a positively selected role without exact authority or complete history refuses identity loss precisely: {error}");
                assert!(!error.contains("source-origin ambiguity"));
                assert_eq!(serde_json::to_vec(&erased).unwrap(), bytes);
                assert_eq!(
                    runner
                        .state()
                        .active_coin_flip_frame()
                        .unwrap()
                        .branch_context
                        .damage_source_choice,
                    Some(choice.clone()),
                    "refusal preserves the existing game"
                );
            }
            *runner.state_mut() = if wire {
                serde_json::from_value::<ResolutionStateWire>(paused)
                    .unwrap()
                    .into_game_state()
            } else {
                serde_json::from_value(paused).unwrap()
            };
            assert_eq!(
                runner
                    .state()
                    .active_coin_flip_frame()
                    .unwrap()
                    .branch_context
                    .damage_source_choice,
                Some(choice)
            );
            runner
                .act(GameAction::SelectCoinFlips {
                    keep_indices: vec![keep],
                })
                .unwrap();
            runner.advance_until_stack_empty();
            if legacy {
                assert_eq!(
                    runner.state().pending_damage_replacements[0].damage_source_filter,
                    Some(TargetFilter::And {
                        filters: vec![
                            TargetFilter::SpecificObject { id: source },
                            TargetFilter::Typed(
                                TypedFilter::default().controller(ControllerRef::You)
                            )
                        ]
                    }),
                    "an actual old reader preserves its encoded continuing You predicate"
                );
            } else {
                assert_one_shot_shield_on_source(&runner, source, true);
            }

            let installed =
                serde_json::to_value(TrustedGameStateEnvelope::capture(runner.state().clone()))
                    .unwrap();
            *runner.state_mut() = serde_json::from_value::<TrustedGameStateEnvelope>(installed)
                .unwrap()
                .into_game_state();
            runner.cast(blink).target_object(source).resolve();
            assert_ne!(
                runner.state().objects[&source].incarnation,
                source_ref.incarnation
            );
            runner.activate(source, 0).target_player(P0).resolve();
            assert_eq!(runner.state().players[0].life, 19, "a sufficient-evidence legacy restore retains the selected old birth, not the later blink return");
            assert_source_program_complete(&runner);
        }
    }
}

#[test]
fn source_qualifier_rechecks_live_white_and_actual_white_departure_without_changing_unqualified_identity(
) {
    use engine::types::mana::ManaColor;
    use engine::types::zones::Zone;
    for departed in [false, true] {
        for qualified in [false, true] {
            let mut scenario = GameScenario::new();
            scenario.at_phase(Phase::PreCombatMain);
            let source = if departed {
                scenario
                    .add_creature_from_oracle(
                        P0,
                        "Mogg Fanatic",
                        1,
                        1,
                        "Sacrifice this creature: It deals 1 damage to any target.",
                    )
                    .with_color(vec![ManaColor::Red])
                    .id()
            } else {
                scenario
                    .add_creature_from_oracle(
                        P0,
                        "Vulshok Sorcerer",
                        1,
                        1,
                        "Haste\n{T}: This creature deals 1 damage to any target.",
                    )
                    .with_color(vec![ManaColor::Red])
                    .id()
            };
            let protection = if qualified {
                scenario.add_creature_from_oracle(P0, "Pilgrim of Justice", 1, 3, "Protection from red\n{W}, Sacrifice this creature: The next time a red source of your choice would deal damage this turn, prevent that damage.").id()
            } else {
                scenario.add_creature_from_oracle(P0, "Haazda Shield Mate", 1, 1, "At the beginning of your upkeep, sacrifice this creature unless you pay {W}{W}.\n{W}: The next time a source of your choice would deal damage to you this turn, prevent that damage.").id()
            };
            let purelace = scenario.add_spell_to_hand_from_oracle(P0, "Purelace", true, "Target spell or permanent becomes white. (Mana symbols on that permanent remain unchanged.)")
                .with_mana_cost(ManaCost::Cost { shards: vec![ManaCostShard::White], generic: 0 }).id();
            let mut runner = scenario.build();
            add_mana(&mut runner, &[ManaType::White, ManaType::White]);
            runner.activate(protection, 0).resolve();
            let WaitingFor::DamageSourceChoice { options, .. } = &runner.state().waiting_for else {
                panic!("paid source producer");
            };
            let reference = options
                .iter()
                .find(|option| option.identity.reference.object_id == source)
                .unwrap()
                .identity
                .reference;
            runner
                .act(GameAction::ChooseDamageSource { source: reference })
                .unwrap();
            runner.advance_until_stack_empty();
            runner.cast(purelace).target_object(source).resolve();
            assert_eq!(
                runner.state().objects[&source].color,
                vec![ManaColor::White],
                "real Purelace changes the property after selection"
            );
            *runner.state_mut() =
                serde_json::from_value(serde_json::to_value(runner.state()).unwrap()).unwrap();
            runner.activate(source, 0).target_player(P0).resolve();
            if departed {
                assert_eq!(runner.state().objects[&source].zone, Zone::Graveyard);
            }
            assert_eq!(runner.state().players[0].life, if qualified { 19 } else { 20 }, "the sink's red qualifier reads live/actual-exit white, while the unqualified identity still matches");
            assert_source_program_complete(&runner);
        }
    }
}

#[test]
fn selected_permanent_spell_tracks_its_completed_successor_through_both_materializers() {
    use engine::types::zones::Zone;
    for prevention in [false, true] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let source = scenario
            .add_creature_to_hand_from_oracle(
                P0,
                "Vulshok Sorcerer",
                1,
                1,
                "Haste\n{T}: This creature deals 1 damage to any target.",
            )
            .with_mana_cost(ManaCost::Cost {
                shards: vec![ManaCostShard::Red, ManaCostShard::Red],
                generic: 1,
            })
            .id();
        let shield = scenario.add_spell_to_hand_from_oracle(P0, if prevention { "Reverse Damage" } else { "Desperate Gambit" }, true,
            if prevention { "The next time a source of your choice would deal damage to you this turn, prevent that damage. You gain life equal to the damage prevented this way." } else { DESPERATE_GAMBIT })
            .with_mana_cost(if prevention { ManaCost::Cost { shards: vec![ManaCostShard::White, ManaCostShard::White], generic: 1 } } else { desperate_gambit_mana() }).id();
        let mut runner = scenario.build();
        add_mana(
            &mut runner,
            &[
                ManaType::Red,
                ManaType::Red,
                ManaType::Red,
                ManaType::White,
                ManaType::White,
                ManaType::Colorless,
                ManaType::Colorless,
            ],
        );
        runner.state_mut().rng_seed = 0;
        runner.state_mut().rng_word_pos = 0;
        runner.state_mut().rng = GameRng::seed_from_u64(0);
        runner.cast(source).commit();
        let stack_birth = runner.state().objects[&source].incarnation;
        runner.cast(shield).resolve();
        let WaitingFor::DamageSourceChoice { options, .. } = &runner.state().waiting_for else {
            panic!("permanent spells belong to the full Source domain");
        };
        let reference = options
            .iter()
            .find(|option| option.identity.reference.object_id == source)
            .unwrap()
            .identity
            .reference;
        assert_eq!(reference.incarnation, stack_birth);
        runner
            .act(GameAction::ChooseDamageSource { source: reference })
            .unwrap();
        runner.advance_until_stack_empty();
        assert_eq!(runner.state().objects[&source].zone, Zone::Battlefield);
        let capture = runner.state().pending_damage_replacements[0]
            .selected_damage_source
            .as_ref()
            .unwrap();
        assert_eq!(
            capture.identity.reference.incarnation,
            runner.state().objects[&source].incarnation,
            "only actual successful permanent-spell completion transfers the selected identity"
        );
        assert_ne!(capture.identity.reference.incarnation, stack_birth);
        *runner.state_mut() =
            serde_json::from_value(serde_json::to_value(runner.state()).unwrap()).unwrap();
        runner.activate(source, 0).target_player(P0).resolve();
        assert_eq!(
            runner.state().players[0].life,
            if prevention { 21 } else { 18 }
        );
        assert_source_program_complete(&runner);
    }
}

#[test]
fn paid_jade_competing_birth_histories_restore_exact_current_and_refuse_authentic_unbound_origins()
{
    use engine::game::scenario::GameRunner;
    use engine::types::resolution::ResolutionStateWire;
    fn old_definition_fields(node: &mut serde_json::Value) {
        match node {
            serde_json::Value::Object(object) => {
                if object.get("event").and_then(serde_json::Value::as_str) == Some("DamageDone") {
                    for key in [
                        "selected_damage_source",
                        "source_controller",
                        "source_object",
                        "origin",
                    ] {
                        object.remove(key);
                    }
                }
                for child in object.values_mut() {
                    old_definition_fields(child);
                }
            }
            serde_json::Value::Array(array) => {
                for child in array {
                    old_definition_fields(child);
                }
            }
            _ => {}
        }
    }
    for selected_before_blink in [false, true] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let source = scenario
            .add_creature_from_oracle(
                P0,
                "Mogg Fanatic",
                1,
                1,
                "Sacrifice this creature: It deals 1 damage to any target.",
            )
            .id();
        let recipient = scenario.add_creature(P1, "Protected recipient", 3, 3).id();
        let jade = scenario.add_artifact_from_oracle(P0, "Jade Monolith", "{1}: The next time a source of your choice would deal damage to target creature this turn, that source deals that damage to you instead.").id();
        let blink = scenario.add_spell_to_hand_from_oracle(P0, "Cloudshift", true, "Exile target creature you control, then return that card to the battlefield under your control.")
            .with_mana_cost(ManaCost::Cost { shards: vec![ManaCostShard::White], generic: 0 }).id();
        let later = scenario.add_spell_to_hand_from_oracle(P0, "Purelace", true, "Target spell or permanent becomes white. (Mana symbols on that permanent remain unchanged.)")
            .with_mana_cost(ManaCost::Cost { shards: vec![ManaCostShard::White], generic: 0 }).id();
        let mut runner = scenario.build();
        add_mana(
            &mut runner,
            &[ManaType::Colorless, ManaType::White, ManaType::White],
        );
        if !selected_before_blink {
            runner.cast(blink).target_object(source).resolve();
        }
        runner.activate(jade, 0).target_object(recipient).resolve();
        let WaitingFor::DamageSourceChoice { options, .. } = &runner.state().waiting_for else {
            panic!(
                "paid Jade activation keeps its recipient target and separately chooses a Source"
            );
        };
        let reference = options
            .iter()
            .find(|option| option.identity.reference.object_id == source)
            .unwrap()
            .identity
            .reference;
        runner
            .act(GameAction::ChooseDamageSource { source: reference })
            .unwrap();
        runner.advance_until_stack_empty();
        if selected_before_blink {
            runner.cast(blink).target_object(source).resolve();
        }
        runner.cast(later).target_object(recipient).resolve();
        assert_source_program_complete(&runner);
        assert!(runner.state().last_chosen_damage_source.is_none());
        assert_eq!(
            runner.state().objects[&recipient]
                .replacement_definitions
                .iter_unchecked()
                .filter(|definition| definition.selected_damage_source.is_some())
                .count(),
            1
        );
        let original = runner.state().clone();
        for wire in [false, true] {
            let current = if wire {
                serde_json::to_value(ResolutionStateWire::from_game_state(original.clone()))
                    .unwrap()
            } else {
                serde_json::to_value(&original).unwrap()
            };
            let mut old = current.clone();
            old.as_object_mut()
                .unwrap()
                .remove("resolved_rules_journal");
            old_definition_fields(&mut old);
            let bytes = serde_json::to_vec(&old).unwrap();
            let error = if wire {
                serde_json::from_value::<ResolutionStateWire>(old.clone())
                    .unwrap_err()
                    .to_string()
            } else {
                serde_json::from_value::<engine::types::game_state::GameState>(old.clone())
                    .unwrap_err()
                    .to_string()
            };
            assert!(error.contains("replacement_definitions") && error.contains("source-origin ambiguity"), "the actual June scalar-to-SpecificObject and native-clone outputs have no retained selected-origin owner: {error}");
            assert_eq!(serde_json::to_vec(&old).unwrap(), bytes);
            assert_eq!(
                serde_json::to_value(runner.state()).unwrap(),
                serde_json::to_value(&original).unwrap()
            );
            let restored = if wire {
                serde_json::from_value::<ResolutionStateWire>(current)
                    .unwrap()
                    .into_game_state()
            } else {
                serde_json::from_value(current).unwrap()
            };
            let mut resumed = GameRunner::from_state(restored);
            resumed
                .activate(source, 0)
                .target_object(recipient)
                .resolve();
            assert_eq!(
                resumed.state().objects[&recipient].damage_marked,
                if selected_before_blink { 1 } else { 0 }
            );
            assert_eq!(
                resumed.state().players[0].life,
                if selected_before_blink { 20 } else { 19 },
                "the exact selected A/B distinction survives both canonical current owners"
            );
            assert_source_program_complete(&resumed);
        }
    }
}

#[test]
fn two_actual_source_producers_replace_owned_selection_and_empty_completion_runs_its_remainder_once(
) {
    // Ordinary grammar witnesses; these synthetic programs do not claim to be printed cards.
    for empty in [false, true] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let own = scenario
            .add_creature_from_oracle(
                P0,
                "Vulshok Sorcerer",
                1,
                1,
                "Haste\n{T}: This creature deals 1 damage to any target.",
            )
            .id();
        let other = (!empty).then(|| {
            scenario
                .add_creature_from_oracle(
                    P1,
                    "Vulshok Sorcerer",
                    1,
                    1,
                    "Haste\n{T}: This creature deals 1 damage to any target.",
                )
                .id()
        });
        let body = if empty {
            "Choose a source an opponent controls. The next time that source would deal damage this turn, it deals double that damage instead. You gain 2 life."
        } else {
            "Choose a source you control. Choose a source an opponent controls. The next time that source would deal damage this turn, it deals double that damage instead."
        };
        let spell = scenario
            .add_spell_to_hand_from_oracle(P0, "Source ownership grammar witness", true, body)
            .with_mana_cost(desperate_gambit_mana())
            .id();
        let mut runner = scenario.build();
        add_mana(&mut runner, &[ManaType::Red]);
        runner.cast(spell).resolve();
        if empty {
            runner.advance_until_stack_empty();
            assert_eq!(
                runner.state().players[0].life,
                22,
                "completed-empty does not truncate or repeat the independent remainder"
            );
            assert!(
                runner.state().pending_damage_replacements.is_empty(),
                "empty selection cannot install an unconstrained modifier"
            );
            runner.activate(own, 0).target_player(P0).resolve();
            assert_eq!(runner.state().players[0].life, 21);
        } else {
            let WaitingFor::DamageSourceChoice { options, .. } = &runner.state().waiting_for else {
                panic!("first actual producer");
            };
            let reference = options
                .iter()
                .find(|option| option.identity.reference.object_id == own)
                .unwrap()
                .identity
                .reference;
            runner
                .act(GameAction::ChooseDamageSource { source: reference })
                .unwrap();
            let WaitingFor::DamageSourceChoice { options, .. } = &runner.state().waiting_for else {
                panic!("the second fresh producer must not consume its parent's completed choice");
            };
            let other = other.unwrap();
            assert!(!options
                .iter()
                .any(|option| option.identity.reference.object_id == own));
            let reference = options
                .iter()
                .find(|option| option.identity.reference.object_id == other)
                .unwrap()
                .identity
                .reference;
            runner
                .act(GameAction::ChooseDamageSource { source: reference })
                .unwrap();
            runner.advance_until_stack_empty();
            assert_eq!(
                runner.state().pending_damage_replacements[0]
                    .selected_damage_source
                    .as_ref()
                    .unwrap()
                    .identity
                    .reference,
                reference
            );
            runner.activate(own, 0).target_player(P0).resolve();
            assert_eq!(
                runner.state().players[0].life,
                19,
                "the first producer's source is a reached negative"
            );
            for _ in 0..4 {
                if matches!(runner.state().waiting_for, WaitingFor::Priority { player } if player == P1)
                {
                    break;
                }
                runner.act(GameAction::PassPriority).unwrap();
            }
            runner.activate(other, 0).target_player(P0).resolve();
            assert_eq!(
                runner.state().players[0].life,
                17,
                "only the second producer's exact source is doubled"
            );
        }
        assert_source_program_complete(&runner);
    }
}

#[test]
fn countered_selected_permanent_spell_never_acquires_a_later_recast_successor_in_either_sink() {
    use engine::types::zones::Zone;
    for prevention in [false, true] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let source = scenario
            .add_creature_to_hand_from_oracle(
                P0,
                "Vulshok Sorcerer",
                1,
                1,
                "Haste\n{T}: This creature deals 1 damage to any target.",
            )
            .with_mana_cost(ManaCost::Cost {
                shards: vec![ManaCostShard::Red, ManaCostShard::Red],
                generic: 1,
            })
            .id();
        let shield = scenario.add_spell_to_hand_from_oracle(P0, if prevention { "Reverse Damage" } else { "Desperate Gambit" }, true,
            if prevention { "The next time a source of your choice would deal damage to you this turn, prevent that damage. You gain life equal to the damage prevented this way." } else { DESPERATE_GAMBIT })
            .with_mana_cost(if prevention { ManaCost::Cost { shards: vec![ManaCostShard::White, ManaCostShard::White], generic: 1 } } else { desperate_gambit_mana() }).id();
        let counter = scenario
            .add_spell_to_hand_from_oracle(P0, "Counterspell", true, "Counter target spell.")
            .with_mana_cost(ManaCost::Cost {
                shards: vec![ManaCostShard::Blue, ManaCostShard::Blue],
                generic: 0,
            })
            .id();
        let disentomb = scenario
            .add_spell_to_hand_from_oracle(
                P0,
                "Disentomb",
                false,
                "Return target creature card from your graveyard to your hand.",
            )
            .with_mana_cost(ManaCost::Cost {
                shards: vec![ManaCostShard::Black],
                generic: 0,
            })
            .id();
        let mut runner = scenario.build();
        add_mana(
            &mut runner,
            &[
                ManaType::Red,
                ManaType::Red,
                ManaType::Red,
                ManaType::Red,
                ManaType::Red,
                ManaType::Blue,
                ManaType::Blue,
                ManaType::Black,
                ManaType::White,
                ManaType::White,
                ManaType::Colorless,
                ManaType::Colorless,
                ManaType::Colorless,
            ],
        );
        runner.state_mut().rng_seed = 0;
        runner.state_mut().rng_word_pos = 0;
        runner.state_mut().rng = GameRng::seed_from_u64(0);
        runner.cast(source).commit();
        runner.cast(shield).resolve();
        let WaitingFor::DamageSourceChoice { options, .. } = &runner.state().waiting_for else {
            panic!("the actual permanent spell is chosen before its future is known");
        };
        let reference = options
            .iter()
            .find(|option| option.identity.reference.object_id == source)
            .unwrap()
            .identity
            .reference;
        runner
            .act(GameAction::ChooseDamageSource { source: reference })
            .unwrap();
        assert_eq!(runner.state().objects[&source].zone, Zone::Stack);
        assert_eq!(runner.state().stack.len(), 1);
        runner.cast(counter).target_object(source).resolve();
        assert_eq!(
            runner.state().objects[&source].zone,
            Zone::Graveyard,
            "real paid Counterspell prevents permanent-spell completion"
        );
        runner.cast(disentomb).target_object(source).resolve();
        assert_eq!(runner.state().objects[&source].zone, Zone::Hand);
        runner.cast(source).resolve();
        assert_eq!(runner.state().objects[&source].zone, Zone::Battlefield);
        let capture = runner.state().pending_damage_replacements[0]
            .selected_damage_source
            .as_ref()
            .unwrap();
        assert_eq!(
            capture.identity.reference, reference,
            "the unrelated later cast cannot complete the old stack occurrence"
        );
        assert_ne!(
            reference.incarnation,
            runner.state().objects[&source].incarnation
        );
        *runner.state_mut() =
            serde_json::from_value(serde_json::to_value(runner.state()).unwrap()).unwrap();
        runner.activate(source, 0).target_player(P0).resolve();
        assert_eq!(
            runner.state().players[0].life,
            19,
            "recast damage is reached and neither doubled nor prevented/gained"
        );
        assert!(!runner.state().pending_damage_replacements[0].is_consumed);
        assert_source_program_complete(&runner);
    }
}

fn source_state_value(
    runner: &engine::game::scenario::GameRunner,
    wire: bool,
) -> serde_json::Value {
    if wire {
        serde_json::to_value(
            engine::types::resolution::ResolutionStateWire::from_game_state(runner.state().clone()),
        )
        .unwrap()
    } else {
        serde_json::to_value(runner.state()).unwrap()
    }
}

fn decode_source_state(
    value: serde_json::Value,
    wire: bool,
) -> Result<engine::types::game_state::GameState, serde_json::Error> {
    if wire {
        serde_json::from_value::<engine::types::resolution::ResolutionStateWire>(value)
            .map(|wire| wire.into_game_state())
    } else {
        serde_json::from_value(value)
    }
}

fn assert_source_origin_refusal_preserves_input(
    runner: &engine::game::scenario::GameRunner,
    old: serde_json::Value,
    wire: bool,
    index: usize,
) {
    let bytes = serde_json::to_vec(&old).unwrap();
    let before = serde_json::to_value(runner.state()).unwrap();
    let error = decode_source_state(old.clone(), wire)
        .unwrap_err()
        .to_string();
    assert!(error.contains(&format!("pending_damage_replacements/{index}"))
        && error.contains("unrecoverable damage source-origin ambiguity between native and selected-source materializers"),
        "the unbound actual common image has precise origin ambiguity: {error}");
    assert_eq!(serde_json::to_vec(&old).unwrap(), bytes);
    assert_eq!(serde_json::to_value(runner.state()).unwrap(), before);
}

fn assert_installed_cast_owner(runner: &engine::game::scenario::GameRunner) -> ResolvedAbility {
    use engine::types::game_state::StackEntryKind;
    let installed = &runner.state().pending_damage_replacements[0];
    let rider = installed
        .runtime_execute
        .as_deref()
        .expect("actual installed continuation");
    let occurrence = rider
        .cast_occurrence
        .expect("actual recursive cast stamp on THIS rider");
    assert_eq!(
        runner.state().spells_cast_this_turn_by_player[&occurrence.caster]
            [occurrence.turn_journal_index as usize]
            .spell_object_id,
        Some(rider.source_id)
    );
    let raw = serde_json::to_value(runner.state()).unwrap();
    let root = raw["resolved_rules_journal"]["entries"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|entry| entry.pointer("/command/StackRemoval/entry"))
        .filter_map(|entry| {
            serde_json::from_value::<engine::types::game_state::StackEntry>(entry.clone()).ok()
        })
        .find_map(|entry| match entry.kind {
            StackEntryKind::Spell {
                ability: Some(root),
                ..
            } if root.source_id == rider.source_id
                && root.controller == rider.controller
                && root.cast_occurrence == Some(occurrence) =>
            {
                Some(*root)
            }
            _ => None,
        })
        .expect("the actual finalized Spell root, not a similar activation");
    if let (Some(left), Some(right)) = (rider.source_incarnation, root.source_incarnation) {
        assert_eq!(left, right);
        assert_ne!(left, engine::types::identifiers::LEGACY_INCARNATION);
    }
    root
}

fn restore_installed_legacy_with_retained_original(
    runner: &mut engine::game::scenario::GameRunner,
    wire: bool,
) {
    use engine::types::resolution::ResolutionStateWire;
    let expected = runner.state().pending_damage_replacements[0].clone();
    let mut installed = if wire {
        serde_json::to_value(ResolutionStateWire::from_game_state(runner.state().clone())).unwrap()
    } else {
        serde_json::to_value(runner.state()).unwrap()
    };
    let original = assert_installed_cast_owner(runner);
    let Effect::PreventDamage {
        damage_source_filter: Some(source),
        ..
    } = &original.effect
    else {
        panic!("the definition-owned cast must retain its actual original source operand");
    };
    assert!(
        matches!(source, TargetFilter::ChosenDamageSource { .. })
            || matches!(source, TargetFilter::And { filters } if filters.iter().any(|filter|
            matches!(filter, TargetFilter::ParentTargetSlot { .. }))),
        "full-equality recovery needs THIS cast's chosen role or declared-source target pin"
    );
    if !matches!(source, TargetFilter::ChosenDamageSource { .. }) {
        assert!(!original.targets.is_empty());
        assert!(
            original
                .target_occurrences()
                .iter()
                .any(|(_, pin)| pin.is_some()),
            "the actual announced source target has an occurrence pin"
        );
        assert_eq!(
            expected.runtime_execute.as_ref().unwrap().cast_occurrence,
            original.cast_occurrence
        );
    }
    installed["pending_damage_replacements"][0]
        .as_object_mut()
        .unwrap()
        .remove("selected_damage_source");
    assert!(
        installed["pending_damage_replacements"][0]
            .get("selected_damage_source")
            .is_none(),
        "this restore reaches installed old-definition admission after real materialization"
    );
    *runner.state_mut() = if wire {
        serde_json::from_value::<ResolutionStateWire>(installed)
            .unwrap()
            .into_game_state()
    } else {
        serde_json::from_value(installed).unwrap()
    };
    assert_eq!(runner.state().pending_damage_replacements[0], expected,
        "the retained exact original/declared-target owner preserves the complete installed typed program");
}

#[test]
fn authentic_direct_typed_program_restores_continuous_native_id_predicate_without_birth_repinning()
{
    use engine::types::ability::*;
    use engine::types::resolution::ResolutionStateWire;
    for wire in [false, true] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let source = scenario
            .add_creature_from_oracle(
                P0,
                "Thrashing Wumpus",
                3,
                3,
                "{B}: This creature deals 1 damage to each creature and each player.",
            )
            .with_mana_cost(ManaCost::Cost {
                shards: vec![ManaCostShard::Black, ManaCostShard::Black],
                generic: 3,
            })
            .id();
        let direct = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::PreventDamage {
                lifetime: RedirectionLifetime::Continuous,
                amount: PreventionAmount::All,
                amount_dynamic: None,
                target: TargetFilter::Controller,
                recipient_scope: EffectScope::Single,
                scope: PreventionScope::AllDamage,
                damage_source_filter: Some(TargetFilter::SpecificObject { id: source }),
                prevention_duration: None,
            },
        )
        .cost(AbilityCost::Mana {
            cost: ManaCost::generic(1),
        });
        let installer = scenario
            .add_artifact_from_oracle(P0, "Native typed prevention witness", "")
            .with_ability_definition(direct)
            .id();
        let blink = scenario.add_spell_to_hand_from_oracle(P0, "Cloudshift", true, "Exile target creature you control, then return that card to the battlefield under your control.")
            .with_mana_cost(ManaCost::Cost { shards: vec![ManaCostShard::White], generic: 0 }).id();
        let mut runner = scenario.build();
        add_mana(
            &mut runner,
            &[
                ManaType::Colorless,
                ManaType::White,
                ManaType::Black,
                ManaType::Black,
            ],
        );
        let mut old = if wire {
            serde_json::to_value(ResolutionStateWire::from_game_state(runner.state().clone()))
                .unwrap()
        } else {
            serde_json::to_value(runner.state()).unwrap()
        };
        for field in ["abilities", "base_abilities"] {
            let body = &mut old["objects"][installer.0.to_string()][field][0]["effect"];
            if let Some(body) = body.as_object_mut() {
                body.remove("lifetime");
            }
        }
        old.as_object_mut()
            .unwrap()
            .remove("resolved_rules_journal");
        *runner.state_mut() = if wire {
            serde_json::from_value::<ResolutionStateWire>(old)
                .unwrap()
                .into_game_state()
        } else {
            serde_json::from_value(old).unwrap()
        };
        let Effect::PreventDamage {
            lifetime,
            damage_source_filter,
            ..
        } = runner.state().objects[&installer].abilities[0]
            .effect
            .as_ref()
        else {
            panic!("the stored native program remains complete");
        };
        assert_eq!(*lifetime, RedirectionLifetime::Continuous);
        assert_eq!(
            *damage_source_filter,
            Some(TargetFilter::SpecificObject { id: source })
        );
        runner.activate(installer, 0).resolve();
        assert!(runner.state().pending_damage_replacements[0]
            .selected_damage_source
            .is_none());
        let current = source_state_value(&runner, wire);
        let expected = runner.state().pending_damage_replacements.clone();
        let restored = decode_source_state(current.clone(), wire).unwrap();
        assert_eq!(restored.pending_damage_replacements, expected);
        let mut old_installed = current;
        old_installed["pending_damage_replacements"][0]
            .as_object_mut()
            .unwrap()
            .remove("selected_damage_source");
        assert_source_origin_refusal_preserves_input(&runner, old_installed, wire, 0);
        *runner.state_mut() = restored;
        let original_birth = runner.state().objects[&source].incarnation;
        runner.cast(blink).target_object(source).resolve();
        assert_ne!(runner.state().objects[&source].incarnation, original_birth);
        *runner.state_mut() =
            serde_json::from_value(serde_json::to_value(runner.state()).unwrap()).unwrap();
        runner.activate(source, 0).resolve();
        runner.activate(source, 0).resolve();
        assert_eq!((runner.state().players[0].life, runner.state().players[1].life), (20, 18), "determinate native direct ID and genuine old continuous semantics remain intact after movement and two events");
        assert_source_program_complete(&runner);
    }
}

#[test]
fn authentic_static_base_and_genuinely_encoded_one_opportunity_restore_and_apply_their_typed_lifetimes(
) {
    use engine::types::ability::*;
    use engine::types::replacements::ReplacementEvent;
    use engine::types::resolution::ResolutionStateWire;
    fn erase_capture(node: &mut serde_json::Value) {
        match node {
            serde_json::Value::Object(object) => {
                if object.get("event").is_some() {
                    object.remove("selected_damage_source");
                }
                for child in object.values_mut() {
                    erase_capture(child);
                }
            }
            serde_json::Value::Array(array) => {
                for child in array {
                    erase_capture(child);
                }
            }
            _ => {}
        }
    }
    for oneshot in [false, true] {
        for wire in [false, true] {
            let mut scenario = GameScenario::new();
            scenario.at_phase(Phase::PreCombatMain);
            let source = scenario
                .add_creature_from_oracle(
                    P0,
                    "Thrashing Wumpus",
                    3,
                    3,
                    "{B}: This creature deals 1 damage to each creature and each player.",
                )
                .with_mana_cost(ManaCost::Cost {
                    shards: vec![ManaCostShard::Black, ManaCostShard::Black],
                    generic: 3,
                })
                .id();
            let mut native = ReplacementDefinition::new(ReplacementEvent::DamageDone)
                .damage_source_filter(TargetFilter::SpecificObject { id: source });
            if oneshot {
                native = native.prevention_oneshot_shield();
                native.consume_on_apply = true;
            } else {
                native.damage_modification = Some(DamageModification::Double);
            }
            let expected = native.clone();
            let host = scenario
                .add_artifact_from_oracle(P0, "Native typed static witness", "")
                .with_replacement_definition(native)
                .id();
            let mut runner = scenario.build();
            add_mana(&mut runner, &[ManaType::Black, ManaType::Black]);
            let mut old = if wire {
                serde_json::to_value(ResolutionStateWire::from_game_state(runner.state().clone()))
                    .unwrap()
            } else {
                serde_json::to_value(runner.state()).unwrap()
            };
            erase_capture(&mut old);
            old.as_object_mut()
                .unwrap()
                .remove("resolved_rules_journal");
            *runner.state_mut() = if wire {
                serde_json::from_value::<ResolutionStateWire>(old)
                    .unwrap()
                    .into_game_state()
            } else {
                serde_json::from_value(old).unwrap()
            };
            assert_eq!(
                runner.state().objects[&host]
                    .base_replacement_definitions
                    .as_ref(),
                &vec![expected]
            );
            runner.activate(source, 0).resolve();
            assert_eq!(
                runner.state().players[0].life,
                if oneshot { 20 } else { 18 }
            );
            runner.activate(source, 0).resolve();
            assert_eq!((runner.state().players[0].life, runner.state().players[1].life), if oneshot { (19, 19) } else { (16, 16) }, "full base ownership preserves native origin and actual encoded lifetime without Oracle reparsing");
            assert_source_program_complete(&runner);
        }
    }
}

#[test]
fn authentic_targeted_awe_program_keeps_its_source_pin_rider_and_genuine_one_opportunity() {
    use engine::types::resolution::ResolutionStateWire;
    for wire in [false, true] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let source = scenario
            .add_creature_from_oracle(
                P0,
                "Thrashing Wumpus",
                3,
                3,
                "{B}: This creature deals 1 damage to each creature and each player.",
            )
            .with_mana_cost(ManaCost::Cost {
                shards: vec![ManaCostShard::Black, ManaCostShard::Black],
                generic: 3,
            })
            .id();
        let awe = scenario.add_spell_to_hand_from_oracle(P0, "Awe Strike", true, "The next time target creature would deal damage this turn, prevent that damage. You gain life equal to the damage prevented this way.")
            .with_mana_cost(ManaCost::Cost { shards: vec![ManaCostShard::White], generic: 0 }).id();
        let mut runner = scenario.build();
        add_mana(
            &mut runner,
            &[ManaType::White, ManaType::Black, ManaType::Black],
        );
        let mut old = if wire {
            serde_json::to_value(ResolutionStateWire::from_game_state(runner.state().clone()))
                .unwrap()
        } else {
            serde_json::to_value(runner.state()).unwrap()
        };
        for field in ["abilities", "base_abilities"] {
            if let Some(body) =
                old["objects"][awe.0.to_string()][field][0]["effect"].as_object_mut()
            {
                body.remove("lifetime");
            }
        }
        old.as_object_mut()
            .unwrap()
            .remove("resolved_rules_journal");
        *runner.state_mut() = if wire {
            serde_json::from_value::<ResolutionStateWire>(old)
                .unwrap()
                .into_game_state()
        } else {
            serde_json::from_value(old).unwrap()
        };
        runner.cast(awe).target_object(source).resolve();
        assert!(matches!(
            runner.state().pending_damage_replacements[0].shield_kind,
            ShieldKind::PreventionOneShot
        ));
        assert!(
            runner.state().pending_damage_replacements[0]
                .selected_damage_source
                .is_none(),
            "a declared source target retains its determinate native target role"
        );
        restore_installed_legacy_with_retained_original(&mut runner, wire);
        let first = runner.activate(source, 0).resolve();
        let gains: Vec<_> = first
            .events()
            .iter()
            .filter_map(|event| match event {
                GameEvent::LifeChanged {
                    player_id, amount, ..
                } if *player_id == P0 && *amount > 0 => Some(*amount),
                _ => None,
            })
            .collect();
        assert_eq!(gains, vec![3], "the full paid Wumpus instance produces one aggregate Awe gain, not three one-life events");
        assert_eq!(
            (
                runner.state().players[0].life,
                runner.state().players[1].life
            ),
            (23, 20)
        );
        runner.activate(source, 0).resolve();
        assert_eq!(
            (
                runner.state().players[0].life,
                runner.state().players[1].life
            ),
            (22, 19)
        );
        assert_source_program_complete(&runner);
    }
}

#[test]
fn offered_departed_birth_survives_prompt_restore_and_never_authorizes_the_returned_occurrence() {
    use engine::types::game_state::{GameState, TrustedGameStateEnvelope};
    use engine::types::zones::Zone;
    for trusted in [false, true] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let source = scenario
            .add_creature_from_oracle(
                P0,
                "Vulshok Sorcerer",
                1,
                1,
                "Haste\n{T}: This creature deals 1 damage to any target.",
            )
            .id();
        let gambit = scenario
            .add_spell_to_hand_from_oracle(P0, "Desperate Gambit", true, DESPERATE_GAMBIT)
            .with_mana_cost(desperate_gambit_mana())
            .id();
        let mut runner = scenario.build();
        add_mana(&mut runner, &[ManaType::Red]);
        runner.state_mut().rng_seed = 0;
        runner.state_mut().rng_word_pos = 0;
        runner.state_mut().rng = GameRng::seed_from_u64(0);
        runner.cast(gambit).resolve();
        let WaitingFor::DamageSourceChoice { options, .. } = &runner.state().waiting_for else {
            panic!("paid printed producer reaches its public prompt");
        };
        let options = options.clone();
        let offered = options
            .iter()
            .find(|option| option.identity.reference.object_id == source)
            .unwrap()
            .identity
            .reference;
        // Exercise the actual zone pipeline while a hostile external boundary
        // retains the already published prompt; an offered capture cannot be
        // replaced by a newly enumerated live object at restore or submission.
        engine::game::zones::move_to_zone(runner.state_mut(), source, Zone::Exile, &mut Vec::new());
        engine::game::zones::move_to_zone(
            runner.state_mut(),
            source,
            Zone::Battlefield,
            &mut Vec::new(),
        );
        let returned = engine::types::identifiers::ObjectIncarnationRef::of(
            source,
            runner.state().objects[&source].incarnation,
        );
        assert_ne!(offered, returned);
        let restored: GameState = if trusted {
            serde_json::from_value::<TrustedGameStateEnvelope>(
                serde_json::to_value(TrustedGameStateEnvelope::capture(runner.state().clone()))
                    .unwrap(),
            )
            .unwrap()
            .into_game_state()
        } else {
            serde_json::from_value(serde_json::to_value(runner.state()).unwrap()).unwrap()
        };
        *runner.state_mut() = restored;
        assert_chosen_capture_private_in_both_envelopes(runner.state(), &options);
        let before = serde_json::to_value(runner.state()).unwrap();
        assert!(runner
            .act(GameAction::ChooseDamageSource { source: returned })
            .is_err());
        assert_eq!(serde_json::to_value(runner.state()).unwrap(), before);
        let answer = runner
            .act(GameAction::ChooseDamageSource { source: offered })
            .unwrap();
        assert_eq!(
            answer
                .events
                .iter()
                .filter(|event| matches!(event, GameEvent::CoinFlipped { .. }))
                .count(),
            1
        );
        runner.advance_until_stack_empty();
        assert_eq!(
            runner.state().pending_damage_replacements[0]
                .selected_damage_source
                .as_ref()
                .unwrap()
                .identity
                .reference,
            offered
        );
        runner.activate(source, 0).target_player(P0).resolve();
        assert_eq!(
            runner.state().players[0].life,
            19,
            "the new birth deals ordinary damage after accepting only the offered old occurrence"
        );
        assert_source_program_complete(&runner);
    }
}

#[test]
fn paid_awe_whole_noncombat_gain_survives_first_later_and_final_recipient_and_gain_pauses() {
    use engine::game::scenario::GameRunner;
    use engine::types::ability::*;
    use engine::types::game_state::GameState;
    use engine::types::proposed_event::ProposedEvent;
    use engine::types::replacements::ReplacementEvent;
    use engine::types::resolution::ResolutionStateWire;
    fn restored(state: &GameState, wire: bool) -> GameState {
        if wire {
            serde_json::from_value::<ResolutionStateWire>(
                serde_json::to_value(ResolutionStateWire::from_game_state(state.clone())).unwrap(),
            )
            .unwrap()
            .into_game_state()
        } else {
            serde_json::from_value(serde_json::to_value(state).unwrap()).unwrap()
        }
    }
    for recipient_index in 0..3 {
        for wire in [false, true] {
            let mut scenario = GameScenario::new();
            scenario.at_phase(Phase::PreCombatMain);
            let source = scenario
                .add_creature_from_oracle(
                    P0,
                    "Thrashing Wumpus",
                    3,
                    3,
                    "{B}: This creature deals 1 damage to each creature and each player.",
                )
                .with_mana_cost(ManaCost::Cost {
                    shards: vec![ManaCostShard::Black, ManaCostShard::Black],
                    generic: 3,
                })
                .id();
            let awe = scenario.add_spell_to_hand_from_oracle(P0, "Awe Strike", true,
                "The next time target creature would deal damage this turn, prevent that damage. You gain life equal to the damage prevented this way.")
                .with_mana_cost(ManaCost::Cost { shards: vec![ManaCostShard::White], generic: 0 }).id();
            let mut runner = scenario.build();
            add_mana(
                &mut runner,
                &[ManaType::White, ManaType::Black, ManaType::Black],
            );
            runner.cast(awe).target_object(source).resolve();
            assert!(runner.state().pending_damage_replacements[0]
                .runtime_execute
                .is_some());
            let mut optional = ReplacementDefinition::new(ReplacementEvent::DamageDone)
                .damage_modification(DamageModification::Double)
                .mode(ReplacementMode::Optional { decline: None });
            if recipient_index == 0 {
                optional.valid_card = Some(TargetFilter::SpecificObject { id: source });
            } else {
                optional.damage_target_filter = Some(DamageTargetFilter::Player {
                    player: DamageTargetPlayerScope::Specific(if recipient_index == 1 {
                        P0
                    } else {
                        P1
                    }),
                });
            }
            optional.source_controller = Some(P0);
            runner
                .state_mut()
                .pending_damage_replacements
                .push(optional);
            runner.state_mut().pending_damage_replacements.push(
                ReplacementDefinition::new(ReplacementEvent::GainLife)
                    .mode(ReplacementMode::Optional { decline: None }),
            );
            runner.activate(source, 0).resolve();
            let pending = runner
                .state()
                .pending_replacement
                .as_ref()
                .expect("actual recipient ordering pause");
            assert!(matches!(pending.proposed, ProposedEvent::Damage { .. }));
            assert_eq!(
                runner.life(P0),
                20,
                "no partial gain may run before all recipients settle"
            );
            let total: i32 = pending
                .damage_opportunity
                .prevention_tally
                .as_ref()
                .unwrap()
                .iter()
                .map(|(_, total)| *total)
                .sum();
            assert_eq!(
                total, recipient_index,
                "the actual pause retains exactly the earlier prevented legs"
            );
            if recipient_index == 2 {
                assert!(runner.state().active_ability_continuation().is_none(),
                    "the final recipient retains its tally on the actual event, with no dummy remainder");
            }
            let ordering = restored(runner.state(), wire);
            runner = GameRunner::from_state(ordering);
            let index = runner
                .state()
                .pending_replacement
                .as_ref()
                .unwrap()
                .candidates
                .iter()
                .position(|id| id.source == ObjectId(0) && id.index == 1)
                .unwrap();
            assert!(
                index > 0,
                "choose the nondefault modifier before the shield"
            );
            runner.act(GameAction::ChooseReplacement { index }).unwrap();
            assert!(
                runner
                    .state()
                    .pending_replacement
                    .as_ref()
                    .unwrap()
                    .is_optional
            );
            *runner.state_mut() = restored(runner.state(), wire);
            runner
                .act(GameAction::ChooseReplacement { index: 0 })
                .unwrap();
            let gain = runner
                .state()
                .pending_replacement
                .as_ref()
                .expect("one complete gain is itself interactive");
            assert!(
                matches!(gain.proposed, ProposedEvent::LifeGain { amount: 4, .. }),
                "the aggregate four-life event is offered only after the complete source instance"
            );
            assert!(
                gain.damage_opportunity.is_empty(),
                "gain replacement is an independent nested event"
            );
            assert_eq!(runner.life(P0), 20);
            *runner.state_mut() = restored(runner.state(), wire);
            let answer = runner
                .act(GameAction::ChooseReplacement { index: 1 })
                .unwrap();
            let gains: Vec<_> = answer
                .events
                .iter()
                .filter_map(|event| match event {
                    GameEvent::LifeChanged {
                        player_id, amount, ..
                    } if *player_id == P0 && *amount > 0 => Some(*amount),
                    _ => None,
                })
                .collect();
            assert_eq!(gains, vec![4]);
            runner.advance_until_stack_empty();
            assert_eq!((runner.life(P0), runner.life(P1)), (24, 20));
            assert_eq!(runner.state().objects[&source].damage_marked, 0);
            assert_source_program_complete(&runner);
            runner.activate(source, 0).resolve();
            assert!(
                runner
                    .state()
                    .pending_replacement
                    .as_ref()
                    .unwrap()
                    .is_optional
            );
            runner
                .act(GameAction::ChooseReplacement { index: 1 })
                .unwrap();
            runner.advance_until_stack_empty();
            assert_eq!((runner.life(P0), runner.life(P1)), (23, 19));
            assert_eq!(runner.state().objects[&source].damage_marked, 1);
            assert_source_program_complete(&runner);
        }
    }
}

#[test]
fn installed_legacy_selected_shield_recovers_only_from_its_original_lexical_flip_owner() {
    use engine::game::scenario::GameRunner;
    use engine::types::ability::*;
    use engine::types::game_state::{GameState, RandomDraw};
    use engine::types::resolution::ResolutionStateWire;
    use engine::types::zones::Zone;
    use rand::Rng;
    for wire in [false, true] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let source = scenario
            .add_creature_from_oracle(
                P0,
                "Thrashing Wumpus",
                3,
                3,
                "{B}: This creature deals 1 damage to each creature and each player.",
            )
            .with_mana_cost(ManaCost::Cost {
                shards: vec![ManaCostShard::Black, ManaCostShard::Black],
                generic: 3,
            })
            .id();
        scenario.add_artifact_from_oracle(
            P0,
            "Krark's Thumb",
            "If you would flip a coin, instead flip two coins and ignore one.",
        );
        // A typed activated composition exercises the existing installer anchor.
        // The complete actual DG program supplies the lexical producer/branches;
        // this is a building-block fixture, not a claim about a printed artifact.
        let mut program = parse_oracle_text(
            DESPERATE_GAMBIT,
            "Desperate Gambit",
            &[],
            &["Instant".into()],
            &[],
        )
        .abilities[0]
            .clone();
        program.kind = AbilityKind::Activated;
        program.cost = Some(AbilityCost::Mana {
            cost: ManaCost::generic(1),
        });
        let Effect::FlipCoins {
            lose_effect: Some(losing),
            ..
        } = program.sub_ability.as_ref().unwrap().effect.as_ref()
        else {
            panic!("the complete DG producer has its real losing prevention body");
        };
        let mut native = losing.as_ref().clone();
        native.kind = program.kind;
        native.cost = program.cost.clone();
        let Effect::PreventDamage {
            damage_source_filter,
            ..
        } = native.effect.as_mut()
        else {
            panic!("clone the whole actual losing body");
        };
        *damage_source_filter = Some(TargetFilter::SpecificObject { id: source });
        let installer = scenario
            .add_artifact_from_oracle(P0, "Typed chosen-source branch witness", "")
            .with_ability_definition(program)
            .with_ability_definition(native)
            .id();
        let blink = scenario.add_spell_to_hand_from_oracle(P0, "Cloudshift", true,
            "Exile target creature you control, then return that card to the battlefield under your control.")
            .with_mana_cost(ManaCost::Cost { shards: vec![ManaCostShard::White], generic: 0 }).id();
        let mut runner = scenario.build();
        add_mana(
            &mut runner,
            &[
                ManaType::Colorless,
                ManaType::Colorless,
                ManaType::White,
                ManaType::Black,
            ],
        );
        let seed = (0..256)
            .find(|seed| {
                let mut rng = GameRng::seed_from_u64(*seed);
                let first = rng.draw(RandomDraw::Outcome).random_bool(0.5);
                let second = rng.draw(RandomDraw::Outcome).random_bool(0.5);
                first != second
            })
            .unwrap();
        runner.state_mut().rng_seed = seed;
        runner.state_mut().rng_word_pos = 0;
        runner.state_mut().rng = GameRng::seed_from_u64(seed);
        runner.activate(installer, 0).resolve();
        let WaitingFor::DamageSourceChoice { options, .. } = &runner.state().waiting_for else {
            panic!("the real activated producer asks once");
        };
        let reference = options
            .iter()
            .find(|option| option.identity.reference.object_id == source)
            .unwrap()
            .identity
            .reference;
        runner
            .act(GameAction::ChooseDamageSource { source: reference })
            .unwrap();
        let WaitingFor::CoinFlipKeepChoice { results, .. } = &runner.state().waiting_for else {
            panic!("the actual Thumb replacement retains the selected branch owner");
        };
        // Pick a seed-independent outcome: both branches materialize a source
        // replacement, but only a loss installs the floating prevention below.
        let keep = results
            .iter()
            .position(|won| !won)
            .expect("fixture seed has a losing coin");
        runner
            .act(GameAction::SelectCoinFlips {
                keep_indices: vec![keep],
            })
            .unwrap();
        runner.advance_until_stack_empty();
        assert_eq!(runner.state().pending_damage_replacements.len(), 1);
        let expected = runner.state().pending_damage_replacements[0].clone();
        assert_eq!(expected.source_object, Some(installer));
        assert_eq!(
            expected
                .selected_damage_source
                .as_ref()
                .unwrap()
                .identity
                .reference,
            reference
        );
        let current = if wire {
            serde_json::to_value(ResolutionStateWire::from_game_state(runner.state().clone()))
                .unwrap()
        } else {
            serde_json::to_value(runner.state()).unwrap()
        };
        assert!(
            current["resolved_rules_journal"]["entries"]
                .as_array()
                .unwrap()
                .iter()
                .any(|entry| entry
                    .pointer("/command/FrameTransition/transition/Push/frame/type")
                    .and_then(serde_json::Value::as_str)
                    == Some("CoinFlip")),
            "the exact actual flip frame, not an arbitrary selected payload, is retained"
        );
        let coherent: GameState = if wire {
            serde_json::from_value::<ResolutionStateWire>(current.clone())
                .unwrap()
                .into_game_state()
        } else {
            serde_json::from_value(current.clone()).unwrap()
        };
        assert_eq!(coherent.pending_damage_replacements[0], expected);
        // This actual no-rider original proves lexical choice, but does not
        // identify which activation installed an erased definition.
        let mut old = current;
        old["pending_damage_replacements"][0]
            .as_object_mut()
            .unwrap()
            .remove("selected_damage_source");
        assert_source_origin_refusal_preserves_input(&runner, old, wire, 0);
        *runner.state_mut() = coherent;
        let mut selected_only = GameRunner::from_state(runner.state().clone());
        selected_only.activate(source, 0).resolve();
        assert_eq!((selected_only.life(P0), selected_only.life(P1)), (20, 20));
        assert_source_program_complete(&selected_only);

        runner.activate(installer, 1).resolve();
        let definitions = runner.state().pending_damage_replacements.clone();
        assert_eq!(
            definitions.len(),
            2,
            "two paid activations on the SAME unmoved installer"
        );
        assert_eq!(definitions[0], expected);
        assert!(definitions[0].runtime_execute.is_none());
        assert!(definitions[1].runtime_execute.is_none());
        assert!(definitions[1].selected_damage_source.is_none());
        let mut selected_without_capture = definitions[0].clone();
        selected_without_capture.selected_damage_source = None;
        assert_eq!(
            selected_without_capture, definitions[1],
            "ENTIRE definitions differ ONLY by selected Some/native None"
        );
        let current = source_state_value(&runner, wire);
        let restored = decode_source_state(current, wire).unwrap();
        assert_eq!(
            restored.pending_damage_replacements, definitions,
            "BOTH canonical owners preserve the DEEP entire two-definition vector"
        );
        *runner.state_mut() = restored;
        let mut both_matching = GameRunner::from_state(runner.state().clone());
        both_matching.activate(source, 0).resolve();
        let mut ordered = 0;
        while matches!(
            both_matching.state().waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ) {
            let pending = both_matching.state().pending_replacement.as_ref().unwrap();
            let index = pending
                .candidates
                .iter()
                .position(|candidate| candidate.index == 1)
                .expect("the actual native definition is offered for this matching source");
            both_matching
                .act(GameAction::ChooseReplacement { index })
                .unwrap();
            ordered += 1;
            assert!(ordered <= 8);
        }
        both_matching.advance_until_stack_empty();
        assert!(
            ordered > 0,
            "two real matching shields require actual ordering"
        );
        assert_eq!((both_matching.life(P0), both_matching.life(P1)), (20, 20));
        assert!(!both_matching.state().pending_damage_replacements[0].is_consumed);
        assert!(both_matching.state().pending_damage_replacements[1].is_consumed);
        assert_source_program_complete(&both_matching);
        let mut matching = GameRunner::from_state(runner.state().clone());
        matching.state_mut().pending_damage_replacements.truncate(1);
        matching.activate(source, 0).resolve();
        assert_eq!((matching.life(P0), matching.life(P1)), (20, 20));
        assert_eq!(matching.state().objects[&source].damage_marked, 0);
        assert_source_program_complete(&matching);
        let mut selected_blink = GameRunner::from_state(runner.state().clone());
        selected_blink
            .state_mut()
            .pending_damage_replacements
            .truncate(1);
        // CR 400.7 + CR 609.7a: the returned permanent is a different
        // chosen source; the native predicate keeps its stored ID meaning.
        selected_blink.cast(blink).target_object(source).resolve();
        assert_ne!(
            selected_blink.state().objects[&source].incarnation,
            reference.incarnation
        );
        selected_blink.activate(source, 0).resolve();
        assert_eq!((selected_blink.life(P0), selected_blink.life(P1)), (19, 19));
        assert!(!selected_blink.state().pending_damage_replacements[0].is_consumed);
        assert_source_program_complete(&selected_blink);
        runner.cast(blink).target_object(source).resolve();
        assert_eq!(runner.state().objects[&source].zone, Zone::Battlefield);
        assert_ne!(
            runner.state().objects[&source].incarnation,
            reference.incarnation
        );
        runner.activate(source, 0).resolve();
        assert_eq!(
            (runner.life(P0), runner.life(P1)),
            (20, 20),
            "native ID predicate prevents the returned birth"
        );
        assert_eq!(runner.state().objects[&source].damage_marked, 0);
        assert!(
            !runner.state().pending_damage_replacements[0].is_consumed,
            "selected old birth excludes the new source and remains unspent"
        );
        assert!(runner.state().pending_damage_replacements[1].is_consumed);
        assert_source_program_complete(&runner);
    }
}

#[test]
fn cast_owned_reverse_installed_legacy_capture_recovers_and_null_contradiction_refuses() {
    use engine::game::combat::AttackTarget;
    use engine::game::scenario::GameRunner;
    use engine::types::ability::*;
    const REVERSE: &str = "The next time a source of your choice would deal damage to you this turn, prevent that damage. You gain life equal to the damage prevented this way.";
    for wire in [false, true] {
        for old_typed_continuous in [false, true] {
            let mut scenario = GameScenario::new();
            scenario.at_phase(Phase::PreCombatMain);
            let source = scenario
                .add_creature_from_oracle(
                    P1,
                    "Thrashing Wumpus",
                    3,
                    3,
                    "{B}: This creature deals 1 damage to each creature and each player.",
                )
                .with_mana_cost(ManaCost::Cost {
                    shards: vec![ManaCostShard::Black, ManaCostShard::Black],
                    generic: 3,
                })
                .id();
            let other_source = scenario
                .add_creature_from_oracle(
                    P1,
                    "Thrashing Wumpus",
                    3,
                    3,
                    "{B}: This creature deals 1 damage to each creature and each player.",
                )
                .id();
            let reverse = if old_typed_continuous {
                // Authentic old TYPED Continuous/rider custody. Rider cloning
                // and recursive cast stamping predate the new capture field;
                // this does not claim the old full Reverse parser attached it.
                let mut program =
                    parse_oracle_text(REVERSE, "Reverse Damage", &[], &["Instant".into()], &[])
                        .abilities[0]
                        .clone();
                let Effect::PreventDamage { lifetime, .. } = program.effect.as_mut() else {
                    panic!("complete typed prevention program");
                };
                *lifetime = RedirectionLifetime::Continuous;
                scenario
                    .add_spell_to_hand(P0, "Old typed continuous source rider", true)
                    .with_ability_definition(program)
                    .with_mana_cost(ManaCost::Cost {
                        shards: vec![ManaCostShard::White, ManaCostShard::White],
                        generic: 1,
                    })
                    .id()
            } else {
                scenario
                    .add_spell_to_hand_from_oracle(P0, "Reverse Damage", true, REVERSE)
                    .with_mana_cost(ManaCost::Cost {
                        shards: vec![ManaCostShard::White, ManaCostShard::White],
                        generic: 1,
                    })
                    .id()
            };
            let blink = scenario.add_spell_to_hand_from_oracle(P1, "Cloudshift", true,
                "Exile target creature you control, then return that card to the battlefield under your control.")
                .with_mana_cost(ManaCost::Cost { shards: vec![ManaCostShard::White], generic: 0 }).id();
            scenario.with_mana_pool(
                P1,
                [ManaType::White, ManaType::Black]
                    .into_iter()
                    .map(|mana| ManaUnit::new(mana, ObjectId(0), false, vec![]))
                    .collect(),
            );
            let mut runner = scenario.build();
            // Lawful fixture setup before announcement: P1 attacks this turn,
            // P0 pays for its instant while P1's permanent is the chosen source.
            runner.state_mut().active_player = P1;
            runner.state_mut().priority_player = P1;
            runner.state_mut().waiting_for = WaitingFor::Priority { player: P1 };
            add_mana(
                &mut runner,
                &[ManaType::White, ManaType::White, ManaType::Colorless],
            );
            if old_typed_continuous {
                let mut old_program = source_state_value(&runner, wire);
                for field in ["abilities", "base_abilities"] {
                    old_program["objects"][reverse.0.to_string()][field][0]["effect"]
                        .as_object_mut()
                        .unwrap()
                        .remove("lifetime");
                }
                *runner.state_mut() = decode_source_state(old_program, wire).unwrap();
            }
            runner.act(GameAction::PassPriority).unwrap();
            assert!(matches!(
                runner.state().waiting_for,
                WaitingFor::Priority { player: P0 }
            ));
            runner.cast(reverse).resolve();
            let WaitingFor::DamageSourceChoice {
                player, options, ..
            } = &runner.state().waiting_for
            else {
                panic!("full paid source prevention reaches its public prompt");
            };
            assert_eq!(*player, P0);
            let reference = options
                .iter()
                .find(|option| option.identity.reference.object_id == source)
                .unwrap()
                .identity
                .reference;
            let other_reference = options
                .iter()
                .find(|option| option.identity.reference.object_id == other_source)
                .expect("a second lawful source is genuinely offered")
                .identity
                .reference;
            let other_capture = runner
                .state()
                .resolution_stack
                .active_ability_continuation()
                .expect("the actual source prompt owns its captures")
                .pending
                .damage_source_candidates
                .iter()
                .find(|capture| capture.identity.reference == other_reference)
                .expect("use the complete real offered capture, not synthetic LKI")
                .clone();
            assert_ne!(other_reference.object_id, reference.object_id);
            assert_ne!(
                other_reference.incarnation,
                engine::types::identifiers::LEGACY_INCARNATION
            );
            runner
                .act(GameAction::ChooseDamageSource { source: reference })
                .unwrap();
            runner.advance_until_stack_empty();
            assert_source_program_complete(&runner);
            assert_eq!(runner.state().pending_damage_replacements.len(), 1);
            let expected = runner.state().pending_damage_replacements.clone();
            assert_eq!(
                expected[0]
                    .selected_damage_source
                    .as_ref()
                    .unwrap()
                    .identity
                    .reference,
                reference
            );
            let original = assert_installed_cast_owner(&runner);
            assert!(matches!(
                original.effect,
                Effect::PreventDamage {
                    damage_source_filter: Some(TargetFilter::ChosenDamageSource {
                        chooser: Some(_),
                        ..
                    }),
                    ..
                }
            ));
            assert_eq!(
                original.cast_occurrence,
                expected[0]
                    .runtime_execute
                    .as_ref()
                    .unwrap()
                    .cast_occurrence
            );
            assert!(runner.state().zone_changes_this_turn.iter().enumerate().all(|(index, record)|
                record.recorded_turn_number == runner.state().turn_number
                    && record.turn_zone_change_index == index && record.object_id != source),
                "complete actual current-turn ledger supplies birth only AFTER original chosen role");
            let current = source_state_value(&runner, wire);
            assert_eq!(
                decode_source_state(current.clone(), wire)
                    .unwrap()
                    .pending_damage_replacements,
                expected
            );
            restore_installed_legacy_with_retained_original(&mut runner, wire);
            assert_eq!(
                runner.state().pending_damage_replacements,
                expected,
                "BOTH owners recover the FULL definition, including its real cast-owned rider"
            );

            for invalid_owned_capture in [false, true] {
                let mut hostile = current.clone();
                hostile["pending_damage_replacements"][0]["selected_damage_source"] =
                    serde_json::Value::Null;
                if invalid_owned_capture {
                    // Hostile corruption of the actual CAST-BOUND original,
                    // not a similarly shaped unrelated flip frame. The actual
                    // positive original has no selection at announcement.
                    let mut corrupted = false;
                    for entry in hostile["resolved_rules_journal"]["entries"]
                        .as_array_mut()
                        .unwrap()
                    {
                        for command in ["StackPush", "StackRemoval"] {
                            if let Some(root) = entry
                                .pointer_mut(&format!("/command/{command}/entry/kind/data/ability"))
                            {
                                if root["source_id"] == serde_json::json!(reverse)
                                    && root["cast_occurrence"]
                                        == serde_json::json!(original.cast_occurrence)
                                {
                                    let mut capture = serde_json::to_value(
                                        expected[0].selected_damage_source.as_ref().unwrap(),
                                    )
                                    .unwrap();
                                    capture["identity"]["reference"]["incarnation"] =
                                        serde_json::json!(0);
                                    root["context"]["damage_source_choice"] = serde_json::json!({
                                        "source": capture, "candidate_filter": {"type": "Any"}
                                    });
                                    corrupted = true;
                                }
                            }
                        }
                    }
                    assert!(corrupted, "the actual stamped original was corrupted");
                }
                let bytes = serde_json::to_vec(&hostile).unwrap();
                let before = serde_json::to_value(runner.state()).unwrap();
                let error = decode_source_state(hostile.clone(), wire)
                    .unwrap_err()
                    .to_string();
                assert!(
                    error.contains("pending_damage_replacements/0")
                        && error
                            .contains("incoherent explicit null selected damage-source capture")
                        && error.contains("retained selected-source owner"),
                    "precise owned contradiction: {error}"
                );
                if invalid_owned_capture {
                    assert!(error.contains(
                        "incoherent retained selected-source owner and installed predicate"
                    ));
                }
                assert!(!error.contains("source-origin ambiguity"));
                assert_eq!(serde_json::to_vec(&hostile).unwrap(), bytes);
                assert_eq!(serde_json::to_value(runner.state()).unwrap(), before);
            }

            for explicit_null in [true, false] {
                let mut hostile = current.clone();
                if explicit_null {
                    hostile["pending_damage_replacements"][0]["selected_damage_source"] =
                        serde_json::Value::Null;
                } else {
                    hostile["pending_damage_replacements"][0]
                        .as_object_mut()
                        .unwrap()
                        .remove("selected_damage_source");
                }
                // Corrupt every admitted copy of THIS cast's original with
                // the coherent capture of the other actually offered source.
                // Keep the installed source predicate, cast-owned rider and
                // current-turn spell record intact so the ID contradiction
                // reaches installed_owner rather than generic schema checks.
                let mut original_paths = vec!["/resolving_stack_entry/kind/data/ability".into()];
                for index in 0..hostile["resolved_rules_journal"]["entries"]
                    .as_array()
                    .unwrap()
                    .len()
                {
                    for command in ["StackPush", "StackRemoval"] {
                        original_paths.push(format!(
                            "/resolved_rules_journal/entries/{index}/command/{command}/entry/kind/data/ability"
                        ));
                    }
                }
                let mut corrupted = 0;
                for path in &original_paths {
                    if let Some(root) = hostile.pointer_mut(path) {
                        if root["source_id"] == serde_json::json!(reverse)
                            && root["cast_occurrence"]
                                == serde_json::json!(original.cast_occurrence)
                        {
                            root["context"]["damage_source_choice"] = serde_json::json!({
                                "source": other_capture, "candidate_filter": {"type": "Any"}
                            });
                            corrupted += 1;
                        }
                    }
                }
                assert!(
                    corrupted > 0,
                    "the actual cast-bound original was corrupted"
                );
                for path in &original_paths {
                    if let Some(root) = hostile.pointer(path) {
                        if root["source_id"] == serde_json::json!(reverse)
                            && root["cast_occurrence"]
                                == serde_json::json!(original.cast_occurrence)
                        {
                            assert_eq!(
                                root["context"]["damage_source_choice"]["source"],
                                serde_json::to_value(&other_capture).unwrap(),
                                "every admitted retained original agrees on the wrong source"
                            );
                        }
                    }
                }
                assert_eq!(
                    hostile["pending_damage_replacements"][0]["damage_source_filter"],
                    current["pending_damage_replacements"][0]["damage_source_filter"]
                );
                assert_eq!(
                    hostile["pending_damage_replacements"][0]["runtime_execute"],
                    current["pending_damage_replacements"][0]["runtime_execute"]
                );
                assert_eq!(
                    hostile["spells_cast_this_turn_by_player"],
                    current["spells_cast_this_turn_by_player"]
                );
                let bytes = serde_json::to_vec(&hostile).unwrap();
                let before = serde_json::to_value(runner.state()).unwrap();
                let error = decode_source_state(hostile.clone(), wire)
                    .unwrap_err()
                    .to_string();
                assert!(
                    error.contains("pending_damage_replacements/0")
                        && error.contains(
                            "incoherent retained selected-source owner and installed predicate"
                        ),
                    "the owned wrong-ID capture has precise incoherence: {error}"
                );
                if explicit_null {
                    assert!(
                        error.contains("incoherent explicit null selected damage-source capture")
                    );
                }
                assert!(!error.contains("source-origin ambiguity"));
                assert_eq!(serde_json::to_vec(&hostile).unwrap(), bytes);
                assert_eq!(serde_json::to_value(runner.state()).unwrap(), before);
            }

            let mut moved = GameRunner::from_state(runner.state().clone());
            moved.cast(blink).target_object(source).resolve();
            assert_ne!(
                moved.state().objects[&source].incarnation,
                reference.incarnation
            );
            moved.activate(source, 0).resolve();
            assert_eq!((moved.life(P0), moved.life(P1)), (19, 19));
            assert!(!moved.state().pending_damage_replacements[0].is_consumed);
            assert_source_program_complete(&moved);

            // CR 615.5 + CR 615.8: prevent one instance and gain its actual
            // prevented amount immediately afterward, once for full Reverse.
            // Wumpus deals ONE to each recipient when activated. Use its real
            // 3/3 combat for the required single three-to-protected-player event.
            runner.advance_to_combat();
            assert!(matches!(
                runner.state().waiting_for,
                WaitingFor::DeclareAttackers { player: P1, .. }
            ));
            runner
                .declare_attackers(&[(source, AttackTarget::Player(P0))])
                .unwrap();
            runner.advance_to_phase(Phase::DeclareBlockers);
            assert!(matches!(
                runner.state().waiting_for,
                WaitingFor::DeclareBlockers { player: P0, .. }
            ));
            runner.declare_blockers(&[]).unwrap();
            let combat = runner.combat_damage();
            assert!(matches!(
                runner.state().phase,
                Phase::EndCombat | Phase::PostCombatMain
            ));
            let gains: Vec<_> = combat
                .events()
                .iter()
                .filter_map(|event| match event {
                    GameEvent::LifeChanged {
                        player_id, amount, ..
                    } if *player_id == P0 && *amount > 0 => Some(*amount),
                    _ => None,
                })
                .collect();
            assert_eq!(
                gains,
                vec![3],
                "one real three-damage event is prevented and gains three immediately"
            );
            assert_eq!((runner.life(P0), runner.life(P1)), (23, 20));
            assert_source_program_complete(&runner);
            // Mana pools empty when phases end, so fund the independent paid activation now.
            runner
                .state_mut()
                .players
                .iter_mut()
                .find(|player| player.id == P1)
                .unwrap()
                .mana_pool
                .add(ManaUnit::new(ManaType::Black, ObjectId(0), false, vec![]));
            runner.activate(source, 0).resolve();
            assert_eq!((runner.life(P0), runner.life(P1)), if old_typed_continuous { (24, 19) } else { (22, 19) },
                "stored Continuous keeps its actual typed meaning; full current Reverse spends once");
            assert_source_program_complete(&runner);
        }
    }
}

#[test]
fn incompatible_same_cast_chosen_leaf_cannot_turn_its_wrong_capture_into_installed_incoherence() {
    use engine::types::ability::*;
    for wire in [false, true] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let mut sources = Vec::new();
        for _ in 0..2 {
            sources.push(
                scenario
                    .add_creature_from_oracle(
                        P0,
                        "Thrashing Wumpus",
                        3,
                        3,
                        "{B}: This creature deals 1 damage to each creature and each player.",
                    )
                    .id(),
            );
        }
        // Supported typed alternatives, not a printed-card claim. Both are
        // chosen-source materializers in one actual recursively stamped cast;
        // their independently different lifetimes cannot install the same leaf.
        let mut chosen = parse_oracle_text("The next time a source of your choice would deal damage to you this turn, prevent that damage. You gain life equal to the damage prevented this way.", "Reverse Damage", &[], &["Instant".into()], &[])
            .abilities[0].clone();
        let Effect::PreventDamage {
            damage_source_filter,
            ..
        } = chosen.effect.as_mut()
        else {
            panic!("complete chosen-source materializer");
        };
        *damage_source_filter = Some(TargetFilter::ChosenDamageSource {
            chooser: None,
            filter: None,
        });
        let mut incompatible = chosen.clone();
        let Effect::PreventDamage { lifetime, .. } = incompatible.effect.as_mut() else {
            unreachable!()
        };
        *lifetime = RedirectionLifetime::Continuous;
        chosen.condition = Some(AbilityCondition::QuantityCheck {
            lhs: QuantityExpr::Ref {
                qty: QuantityRef::LifeTotal {
                    player: PlayerScope::Controller,
                },
            },
            comparator: Comparator::GE,
            rhs: QuantityExpr::Fixed { value: 20 },
        });
        chosen.else_ability = Some(Box::new(incompatible));
        let mut program = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::ChooseDamageSource {
                source_filter: TargetFilter::Any,
            },
        );
        program.sub_ability = Some(Box::new(chosen));
        let spell = scenario
            .add_spell_to_hand(P0, "Typed incompatible chosen alternatives", true)
            .with_ability_definition(program)
            .with_mana_cost(ManaCost::generic(1))
            .id();
        let mut runner = scenario.build();
        add_mana(
            &mut runner,
            &[ManaType::Colorless, ManaType::Black, ManaType::Black],
        );
        runner.cast(spell).resolve();
        let WaitingFor::DamageSourceChoice { options, .. } = &runner.state().waiting_for else {
            panic!("the paid composition reaches its actual source prompt");
        };
        let reference = options
            .iter()
            .find(|option| option.identity.reference.object_id == sources[0])
            .unwrap()
            .identity
            .reference;
        let other_reference = options
            .iter()
            .find(|option| option.identity.reference.object_id == sources[1])
            .unwrap()
            .identity
            .reference;
        let other_capture = runner
            .state()
            .resolution_stack
            .active_ability_continuation()
            .unwrap()
            .pending
            .damage_source_candidates
            .iter()
            .find(|capture| capture.identity.reference == other_reference)
            .unwrap()
            .clone();
        assert_ne!(reference.object_id, other_reference.object_id);
        runner
            .act(GameAction::ChooseDamageSource { source: reference })
            .unwrap();
        runner.advance_until_stack_empty();
        assert_source_program_complete(&runner);
        assert_eq!(runner.state().pending_damage_replacements.len(), 1);
        let expected = runner.state().pending_damage_replacements.clone();
        assert_eq!(expected[0].shield_kind, ShieldKind::PreventionOneShot);
        let original = assert_installed_cast_owner(&runner);
        let selected = original.sub_ability.as_ref().unwrap();
        let alternative = selected.else_ability.as_ref().unwrap();
        assert_eq!(selected.cast_occurrence, original.cast_occurrence);
        assert_eq!(alternative.cast_occurrence, original.cast_occurrence);
        assert!(matches!(
            alternative.effect,
            Effect::PreventDamage {
                lifetime: RedirectionLifetime::Continuous,
                ..
            }
        ));
        assert_eq!(
            expected[0]
                .selected_damage_source
                .as_ref()
                .unwrap()
                .identity
                .reference,
            reference
        );
        let mut legacy = source_state_value(&runner, wire);
        legacy["pending_damage_replacements"][0]
            .as_object_mut()
            .unwrap()
            .remove("selected_damage_source");
        let mut original_paths = vec!["/resolving_stack_entry/kind/data/ability".into()];
        for index in 0..legacy["resolved_rules_journal"]["entries"]
            .as_array()
            .unwrap()
            .len()
        {
            for command in ["StackPush", "StackRemoval"] {
                original_paths.push(format!("/resolved_rules_journal/entries/{index}/command/{command}/entry/kind/data/ability"));
            }
        }
        let mut corrupted = 0;
        for path in original_paths {
            if let Some(root) = legacy.pointer_mut(&path) {
                if root["source_id"] == serde_json::json!(spell)
                    && root["cast_occurrence"] == serde_json::json!(original.cast_occurrence)
                {
                    let alternative = &mut root["sub_ability"]["else_ability"];
                    assert_eq!(
                        alternative["cast_occurrence"],
                        serde_json::json!(original.cast_occurrence)
                    );
                    alternative["context"]["damage_source_choice"] = serde_json::json!({
                        "source": other_capture, "candidate_filter": {"type": "Any"}
                    });
                    corrupted += 1;
                }
            }
        }
        assert!(
            corrupted > 0,
            "the genuine incompatible resolved alternative was reached"
        );
        let bytes = serde_json::to_vec(&legacy).unwrap();
        let before = serde_json::to_value(runner.state()).unwrap();
        let restored = decode_source_state(legacy.clone(), wire).unwrap();
        assert_eq!(restored.pending_damage_replacements, expected,
            "the incompatible chosen leaf cannot erase the sufficient matching chosen meaning or manufacture an error");
        assert_eq!(serde_json::to_vec(&legacy).unwrap(), bytes);
        assert_eq!(serde_json::to_value(runner.state()).unwrap(), before);
        *runner.state_mut() = restored;
        runner.activate(sources[0], 0).resolve();
        assert_eq!((runner.life(P0), runner.life(P1)), (21, 19));
        assert_source_program_complete(&runner);
        runner.activate(sources[0], 0).resolve();
        assert_eq!((runner.life(P0), runner.life(P1)), (20, 18));
        assert_source_program_complete(&runner);
    }
}

#[test]
fn same_cast_resolved_native_and_chosen_alternatives_cannot_lend_each_other_source_role() {
    use engine::types::ability::*;
    for wire in [false, true] {
        for native_run in [true, false] {
            let mut scenario = GameScenario::new();
            scenario.at_phase(Phase::PreCombatMain);
            scenario.with_life(P0, if native_run { 20 } else { 19 });
            let source = scenario
                .add_creature_from_oracle(
                    P0,
                    "Thrashing Wumpus",
                    3,
                    3,
                    "{B}: This creature deals 1 damage to each creature and each player.",
                )
                .id();
            // Supported typed conditional composition, not printed Oracle.
            let mut chosen = parse_oracle_text("The next time a source of your choice would deal damage to you this turn, prevent that damage. You gain life equal to the damage prevented this way.", "Reverse Damage", &[], &["Instant".into()], &[])
                .abilities[0].clone();
            let Effect::PreventDamage {
                damage_source_filter,
                ..
            } = chosen.effect.as_mut()
            else {
                panic!("complete selected materializer and GainLife rider");
            };
            *damage_source_filter = Some(TargetFilter::ChosenDamageSource {
                chooser: None,
                filter: None,
            });
            let mut native = chosen.clone();
            let Effect::PreventDamage {
                damage_source_filter,
                ..
            } = native.effect.as_mut()
            else {
                unreachable!()
            };
            *damage_source_filter = Some(TargetFilter::SpecificObject { id: source });
            native.condition = Some(AbilityCondition::QuantityCheck {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::LifeTotal {
                        player: PlayerScope::Controller,
                    },
                },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 20 },
            });
            native.else_ability = Some(Box::new(chosen));
            let mut program = AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::ChooseDamageSource {
                    source_filter: TargetFilter::Any,
                },
            );
            program.sub_ability = Some(Box::new(native));
            let spell = scenario
                .add_spell_to_hand(P0, "Typed same-cast source alternatives", true)
                .with_ability_definition(program)
                .with_mana_cost(ManaCost::generic(1))
                .id();
            let mut runner = scenario.build();
            add_mana(&mut runner, &[ManaType::Colorless, ManaType::Black]);
            runner.cast(spell).resolve();
            let WaitingFor::DamageSourceChoice { options, .. } = &runner.state().waiting_for else {
                panic!("the paid composition publishes its actual source choice");
            };
            let reference = options
                .iter()
                .find(|option| option.identity.reference.object_id == source)
                .unwrap()
                .identity
                .reference;
            runner
                .act(GameAction::ChooseDamageSource { source: reference })
                .unwrap();
            runner.advance_until_stack_empty();
            assert_source_program_complete(&runner);
            let definition = runner.state().pending_damage_replacements[0].clone();
            assert_eq!(
                definition.selected_damage_source.is_none(),
                native_run,
                "separate real runs actually execute native and chosen branches"
            );
            let original = assert_installed_cast_owner(&runner);
            let native = original.sub_ability.as_ref().unwrap();
            let chosen = native.else_ability.as_ref().unwrap();
            let occurrence = original.cast_occurrence.expect("real root cast");
            assert_eq!(native.cast_occurrence, Some(occurrence));
            assert_eq!(chosen.cast_occurrence, Some(occurrence));
            assert_eq!(
                native.sub_ability.as_ref().unwrap().cast_occurrence,
                Some(occurrence)
            );
            assert_eq!(
                chosen.sub_ability.as_ref().unwrap().cast_occurrence,
                Some(occurrence)
            );
            assert_eq!(
                definition.runtime_execute.as_ref().unwrap().cast_occurrence,
                Some(occurrence)
            );
            let current = source_state_value(&runner, wire);
            assert_eq!(
                decode_source_state(current.clone(), wire)
                    .unwrap()
                    .pending_damage_replacements[0],
                definition
            );
            let mut absent = current;
            absent["pending_damage_replacements"][0]
                .as_object_mut()
                .unwrap()
                .remove("selected_damage_source");
            assert_source_origin_refusal_preserves_input(&runner, absent, wire, 0);
            runner.activate(source, 0).resolve();
            assert_eq!(
                (runner.life(P0), runner.life(P1)),
                (if native_run { 21 } else { 20 }, 19)
            );
            assert_source_program_complete(&runner);
        }
    }
}

#[test]
fn own_resolution_nested_choice_keeps_its_actual_missing_cast_stamp_and_current_meaning() {
    use engine::types::ability::*;
    use engine::types::game_state::RandomDraw;
    use rand::Rng;
    for wire in [false, true] {
        for flip in [false, true] {
            for branch in [0, 1] {
                let mut scenario = GameScenario::new();
                scenario.at_phase(Phase::PreCombatMain);
                if flip {
                    scenario.add_artifact_from_oracle(
                        P0,
                        "Krark's Thumb",
                        "If you would flip a coin, instead flip two coins and ignore one.",
                    );
                }
                let source = scenario
                    .add_creature_from_oracle(
                        P0,
                        "Thrashing Wumpus",
                        3,
                        3,
                        "{B}: This creature deals 1 damage to each creature and each player.",
                    )
                    .id();
                let mut chosen = parse_oracle_text("The next time a source of your choice would deal damage to you this turn, prevent that damage. You gain life equal to the damage prevented this way.", "Reverse Damage", &[], &["Instant".into()], &[])
                .abilities[0].clone();
                let Effect::PreventDamage {
                    damage_source_filter,
                    ..
                } = chosen.effect.as_mut()
                else {
                    unreachable!()
                };
                *damage_source_filter = Some(TargetFilter::ChosenDamageSource {
                    chooser: None,
                    filter: None,
                });
                let mut native = chosen.clone();
                let Effect::PreventDamage {
                    damage_source_filter,
                    ..
                } = native.effect.as_mut()
                else {
                    unreachable!()
                };
                *damage_source_filter = Some(TargetFilter::SpecificObject { id: source });
                let nested = AbilityDefinition::new(
                    AbilityKind::Spell,
                    if flip {
                        Effect::FlipCoins {
                            flips: CoinFlipRun::single(),
                            flipper: TargetFilter::Controller,
                            win_effect: Some(Box::new(native)),
                            lose_effect: Some(Box::new(chosen)),
                        }
                    } else {
                        Effect::ChooseOneOf {
                            chooser: PlayerFilter::Controller,
                            branches: vec![native, chosen],
                            kind: ChooseOneOfKind::Ordinary,
                        }
                    },
                );
                let mut program = AbilityDefinition::new(
                    AbilityKind::Spell,
                    Effect::ChooseDamageSource {
                        source_filter: TargetFilter::Any,
                    },
                );
                program.sub_ability = Some(Box::new(nested));
                let spell = scenario
                    .add_spell_to_hand(P0, "Typed own-resolution source branches", true)
                    .with_ability_definition(program)
                    .with_mana_cost(ManaCost::generic(1))
                    .id();
                let mut runner = scenario.build();
                add_mana(&mut runner, &[ManaType::Colorless, ManaType::Black]);
                if flip {
                    let seed = (0..256)
                        .find(|seed| {
                            let mut rng = GameRng::seed_from_u64(*seed);
                            rng.draw(RandomDraw::Outcome).random_bool(0.5)
                                != rng.draw(RandomDraw::Outcome).random_bool(0.5)
                        })
                        .unwrap();
                    runner.state_mut().rng_seed = seed;
                    runner.state_mut().rng_word_pos = 0;
                    runner.state_mut().rng = GameRng::seed_from_u64(seed);
                }
                runner.cast(spell).resolve();
                let WaitingFor::DamageSourceChoice { options, .. } = &runner.state().waiting_for
                else {
                    panic!("source prompt");
                };
                let reference = options
                    .iter()
                    .find(|option| option.identity.reference.object_id == source)
                    .unwrap()
                    .identity
                    .reference;
                runner
                    .act(GameAction::ChooseDamageSource { source: reference })
                    .unwrap();
                if flip {
                    let WaitingFor::CoinFlipKeepChoice { results, .. } =
                        &runner.state().waiting_for
                    else {
                        panic!("actual Thumb replacement pauses the own-resolution flip");
                    };
                    let index = results
                        .iter()
                        .position(|won| *won == (branch == 0))
                        .unwrap();
                    runner
                        .act(GameAction::SelectCoinFlips {
                            keep_indices: vec![index],
                        })
                        .unwrap();
                } else {
                    let WaitingFor::ChooseOneOfBranch {
                        player, branches, ..
                    } = &runner.state().waiting_for
                    else {
                        panic!("actual nested own-resolution branch prompt");
                    };
                    assert_eq!(*player, P0);
                    assert_eq!(branches.len(), 2);
                    runner
                        .act(GameAction::ChooseBranch { index: branch })
                        .unwrap();
                }
                runner.advance_until_stack_empty();
                let definition = runner.state().pending_damage_replacements[0].clone();
                assert_eq!(definition.selected_damage_source.is_none(), branch == 0);
                assert!(definition.runtime_execute.as_ref().unwrap().cast_occurrence.is_none(),
                "actual newly built nested branch is NOT recursively stamped by the cast producer");
                let raw = source_state_value(&runner, wire);
                assert!(
                    raw["resolved_rules_journal"]["entries"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|entry| entry
                            .pointer(
                                "/command/StackRemoval/entry/kind/data/ability/cast_occurrence"
                            )
                            .is_some_and(|occurrence| !occurrence.is_null())),
                    "the paid outer Spell really WAS stamped"
                );
                assert_eq!(
                    decode_source_state(raw.clone(), wire)
                        .unwrap()
                        .pending_damage_replacements[0],
                    definition
                );
                let mut absent = raw;
                absent["pending_damage_replacements"][0]
                    .as_object_mut()
                    .unwrap()
                    .remove("selected_damage_source");
                assert_source_origin_refusal_preserves_input(&runner, absent, wire, 0);
                runner.activate(source, 0).resolve();
                assert_eq!((runner.life(P0), runner.life(P1)), (21, 19));
                assert_source_program_complete(&runner);
            }
        }
    }
}

#[test]
fn actual_noncast_copy_retains_cloned_coordinate_but_cannot_borrow_the_original_cast_record() {
    use engine::types::ability::*;
    for wire in [false, true] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let source = scenario
            .add_creature_from_oracle(
                P0,
                "Thrashing Wumpus",
                3,
                3,
                "{B}: This creature deals 1 damage to each creature and each player.",
            )
            .id();
        let reverse = scenario.add_spell_to_hand_from_oracle(P0, "Reverse Damage", true,
            "The next time a source of your choice would deal damage to you this turn, prevent that damage. You gain life equal to the damage prevented this way.")
            .with_mana_cost(ManaCost::Cost { shards: vec![ManaCostShard::White, ManaCostShard::White], generic: 1 }).id();
        let copier = scenario
            .add_artifact_from_oracle(P0, "Typed paid spell-copy witness", "")
            .with_ability_definition(
                AbilityDefinition::new(
                    AbilityKind::Activated,
                    Effect::CopySpell {
                        target: TargetFilter::StackSpell,
                        retarget: CopyRetargetPermission::KeepOriginalTargets,
                        copier: None,
                        additional_modifications: vec![],
                        starting_loyalty_from_casualty_sacrifice: false,
                    },
                )
                .cost(AbilityCost::Mana {
                    cost: ManaCost::generic(1),
                }),
            )
            .id();
        let mut runner = scenario.build();
        add_mana(
            &mut runner,
            &[
                ManaType::White,
                ManaType::White,
                ManaType::Colorless,
                ManaType::Colorless,
            ],
        );
        runner.cast(reverse).commit();
        let original_occurrence = runner
            .state()
            .stack
            .iter()
            .find(|entry| entry.id == reverse)
            .unwrap()
            .ability()
            .unwrap()
            .cast_occurrence
            .expect("actual original cast");
        runner.activate(copier, 0).target_object(reverse).resolve();
        for _ in 0..2 {
            let WaitingFor::DamageSourceChoice { options, .. } = &runner.state().waiting_for else {
                panic!("both actual copy and original independently ask for a source");
            };
            let reference = options
                .iter()
                .find(|option| option.identity.reference.object_id == source)
                .unwrap()
                .identity
                .reference;
            runner
                .act(GameAction::ChooseDamageSource { source: reference })
                .unwrap();
            runner.advance_until_stack_empty();
        }
        assert_source_program_complete(&runner);
        let definitions = runner.state().pending_damage_replacements.clone();
        assert_eq!(definitions.len(), 2);
        let copy_rider = definitions[0].runtime_execute.as_ref().unwrap();
        assert_ne!(copy_rider.source_id, reverse);
        assert_eq!(
            copy_rider.cast_occurrence,
            Some(original_occurrence),
            "assert actual copied metadata; do NOT assume this existing writer clears it"
        );
        assert_eq!(
            runner.state().spells_cast_this_turn_by_player[&original_occurrence.caster]
                [original_occurrence.turn_journal_index as usize]
                .spell_object_id,
            Some(reverse)
        );
        assert!(definitions
            .iter()
            .all(|definition| definition.selected_damage_source.is_some()));
        let current = source_state_value(&runner, wire);
        assert_eq!(
            decode_source_state(current.clone(), wire)
                .unwrap()
                .pending_damage_replacements,
            definitions
        );
        let mut absent = current.clone();
        absent["pending_damage_replacements"][0]
            .as_object_mut()
            .unwrap()
            .remove("selected_damage_source");
        assert_source_origin_refusal_preserves_input(&runner, absent, wire, 0);
        // Supplemental stored-native control on this ACTUAL noncast custody:
        // a coordinate naming a different source cannot contradict native null.
        let mut native = current;
        native["pending_damage_replacements"][0]["selected_damage_source"] =
            serde_json::Value::Null;
        let restored = decode_source_state(native, wire).unwrap();
        assert!(restored.pending_damage_replacements[0]
            .selected_damage_source
            .is_none());
        assert_eq!(restored.pending_damage_replacements[1], definitions[1]);
    }
}

#[test]
fn cast_owned_source_rider_does_not_borrow_a_registered_later_boon_body() {
    use engine::types::ability::*;
    use engine::types::triggers::TriggerMode;
    for wire in [false, true] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let source = scenario
            .add_creature_from_oracle(
                P0,
                "Thrashing Wumpus",
                3,
                3,
                "{B}: This creature deals 1 damage to each creature and each player.",
            )
            .id();
        let mut program = parse_oracle_text("The next time a source of your choice would deal damage to you this turn, prevent that damage. You gain life equal to the damage prevented this way.", "Reverse Damage", &[], &["Instant".into()], &[])
            .abilities[0].clone();
        let mut later = program.clone();
        let Effect::PreventDamage {
            damage_source_filter,
            ..
        } = later.effect.as_mut()
        else {
            unreachable!()
        };
        *damage_source_filter = Some(TargetFilter::SpecificObject { id: source });
        let mut register = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::CreateBoon {
                recipient: TargetFilter::Controller,
                trigger: Box::new(
                    TriggerDefinition::new(TriggerMode::Phase)
                        .phase(Phase::Upkeep)
                        .execute(later),
                ),
            },
        );
        register.sub_link = SubAbilityLink::SequentialSibling;
        program.sub_ability.as_mut().unwrap().sub_ability = Some(Box::new(register));
        let spell = scenario
            .add_spell_to_hand(P0, "Typed source rider with registered later body", true)
            .with_ability_definition(program)
            .with_mana_cost(ManaCost::generic(1))
            .id();
        let mut runner = scenario.build();
        add_mana(&mut runner, &[ManaType::Colorless, ManaType::Black]);
        runner.cast(spell).resolve();
        let WaitingFor::DamageSourceChoice { options, .. } = &runner.state().waiting_for else {
            panic!("actual paid source prompt");
        };
        let reference = options
            .iter()
            .find(|option| option.identity.reference.object_id == source)
            .unwrap()
            .identity
            .reference;
        runner
            .act(GameAction::ChooseDamageSource { source: reference })
            .unwrap();
        runner.advance_until_stack_empty();
        let original = assert_installed_cast_owner(&runner);
        let register = original
            .sub_ability
            .as_ref()
            .unwrap()
            .sub_ability
            .as_ref()
            .unwrap();
        assert_eq!(register.cast_occurrence, original.cast_occurrence);
        let Effect::CreateBoon { trigger, .. } = &register.effect else {
            panic!("retained actual registered boundary");
        };
        let later = engine::game::ability_utils::build_resolved_from_def(
            trigger.execute.as_ref().unwrap(),
            original.source_id,
            original.controller,
        );
        assert!(later.cast_occurrence.is_none());
        assert!(
            later.context.damage_source_choice.is_none(),
            "registered future body has no invocation-owned selected context"
        );
        restore_installed_legacy_with_retained_original(&mut runner, wire);
        runner.activate(source, 0).resolve();
        assert_eq!((runner.life(P0), runner.life(P1)), (21, 19));
        assert_eq!(
            runner.state().delayed_triggers.len(),
            1,
            "actual rider registers its separate future boon once"
        );
        assert_source_program_complete(&runner);
    }
}

#[test]
fn actual_recast_and_other_caster_coordinates_preserve_each_definition_owned_spell() {
    use engine::types::game_state::CastOccurrence;
    for wire in [false, true] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let source = scenario
            .add_creature_from_oracle(
                P0,
                "Thrashing Wumpus",
                3,
                3,
                "{B}: This creature deals 1 damage to each creature and each player.",
            )
            .id();
        let text = "The next time a source of your choice would deal damage to you this turn, prevent that damage. You gain life equal to the damage prevented this way.";
        let reverse = scenario
            .add_spell_to_hand_from_oracle(P0, "Reverse Damage", true, text)
            .with_mana_cost(ManaCost::Cost {
                shards: vec![ManaCostShard::White, ManaCostShard::White],
                generic: 1,
            })
            .id();
        let other = scenario
            .add_spell_to_hand_from_oracle(P1, "Reverse Damage", true, text)
            .with_mana_cost(ManaCost::Cost {
                shards: vec![ManaCostShard::White, ManaCostShard::White],
                generic: 1,
            })
            .id();
        let recall = scenario
            .add_spell_to_hand_from_oracle(
                P0,
                "Call to Mind",
                false,
                "Return target instant or sorcery card from your graveyard to your hand.",
            )
            .with_mana_cost(ManaCost::Cost {
                shards: vec![ManaCostShard::Blue],
                generic: 2,
            })
            .id();
        scenario.with_mana_pool(
            P1,
            [ManaType::White, ManaType::White, ManaType::Colorless]
                .into_iter()
                .map(|mana| ManaUnit::new(mana, ObjectId(0), false, vec![]))
                .collect(),
        );
        let mut runner = scenario.build();
        add_mana(
            &mut runner,
            &[
                ManaType::White,
                ManaType::White,
                ManaType::White,
                ManaType::White,
                ManaType::Colorless,
                ManaType::Colorless,
                ManaType::Colorless,
                ManaType::Colorless,
                ManaType::Blue,
            ],
        );
        let select = |runner: &mut engine::game::scenario::GameRunner| {
            let WaitingFor::DamageSourceChoice { options, .. } = &runner.state().waiting_for else {
                panic!("each real paid cast reaches its own prompt");
            };
            let reference = options
                .iter()
                .find(|option| option.identity.reference.object_id == source)
                .unwrap()
                .identity
                .reference;
            runner
                .act(GameAction::ChooseDamageSource { source: reference })
                .unwrap();
            runner.advance_until_stack_empty();
            assert_source_program_complete(runner);
        };
        runner.cast(reverse).resolve();
        select(&mut runner);
        runner.cast(recall).target_object(reverse).resolve();
        assert_eq!(
            runner.state().objects[&reverse].zone,
            engine::types::zones::Zone::Hand
        );
        runner.cast(reverse).resolve();
        select(&mut runner);
        runner.act(GameAction::PassPriority).unwrap();
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { player: P1 }
        ));
        runner.cast(other).resolve();
        select(&mut runner);
        let definitions = runner.state().pending_damage_replacements.clone();
        assert_eq!(definitions.len(), 3);
        let coordinates: Vec<_> = definitions
            .iter()
            .map(|definition| {
                definition
                    .runtime_execute
                    .as_ref()
                    .unwrap()
                    .cast_occurrence
                    .unwrap()
            })
            .collect();
        assert_eq!(
            coordinates,
            vec![
                CastOccurrence {
                    caster: P0,
                    turn_journal_index: 0
                },
                CastOccurrence {
                    caster: P0,
                    turn_journal_index: 2
                },
                CastOccurrence {
                    caster: P1,
                    turn_journal_index: 0
                }
            ]
        );
        assert_eq!(
            runner.state().spells_cast_this_turn_by_player[&P0][0].spell_object_id,
            Some(reverse)
        );
        assert_eq!(
            runner.state().spells_cast_this_turn_by_player[&P0][2].spell_object_id,
            Some(reverse)
        );
        assert_eq!(
            runner.state().spells_cast_this_turn_by_player[&P1][0].spell_object_id,
            Some(other)
        );
        let current = source_state_value(&runner, wire);
        assert_eq!(
            decode_source_state(current.clone(), wire)
                .unwrap()
                .pending_damage_replacements,
            definitions
        );
        for index in 0..definitions.len() {
            let mut absent = current.clone();
            absent["pending_damage_replacements"][index]
                .as_object_mut()
                .unwrap()
                .remove("selected_damage_source");
            assert_eq!(decode_source_state(absent, wire).unwrap().pending_damage_replacements, definitions,
                "another cast of the same storage ID or another caster's index0 cannot supply this leaf's meaning");
        }
        // Supplemental field-loss control on a reached real three-cast state.
        // Missing a coordinate alone does not change coherent Some or native null.
        let mut missing = current;
        missing["pending_damage_replacements"][0]["runtime_execute"]
            .as_object_mut()
            .unwrap()
            .remove("cast_occurrence");
        let retained = decode_source_state(missing.clone(), wire).unwrap();
        assert_eq!(
            retained.pending_damage_replacements[0].selected_damage_source,
            definitions[0].selected_damage_source
        );
        missing["pending_damage_replacements"][0]["selected_damage_source"] =
            serde_json::Value::Null;
        let native = decode_source_state(missing.clone(), wire).unwrap();
        assert!(native.pending_damage_replacements[0]
            .selected_damage_source
            .is_none());
        missing["pending_damage_replacements"][0]
            .as_object_mut()
            .unwrap()
            .remove("selected_damage_source");
        assert_source_origin_refusal_preserves_input(&runner, missing, wire, 0);
    }
}

// Native typed compositions test the supported actor boundary. These are reusable
// shield building blocks, not claims that the unsupported actor text is printed.
fn inline_actor_effect(
    prevention: bool,
    chooser: Option<engine::types::ability::Chooser>,
    filter: TargetFilter,
) -> Effect {
    use engine::types::ability::*;
    let chosen = Some(TargetFilter::ChosenDamageSource {
        chooser,
        filter: Some(Box::new(filter)),
    });
    if prevention {
        Effect::PreventDamage {
            lifetime: RedirectionLifetime::OneOpportunity,
            amount: PreventionAmount::All,
            amount_dynamic: None,
            target: TargetFilter::Any,
            recipient_scope: EffectScope::All,
            scope: PreventionScope::AllDamage,
            damage_source_filter: chosen,
            prevention_duration: Some(Duration::UntilEndOfTurn),
        }
    } else {
        Effect::CreateDamageReplacement {
            source_filter: chosen,
            combat_scope: None,
            target_filter: None,
            modification: Some(DamageModification::Double),
            redirect_to: None,
            redirect_amount: None,
            redirect_object_filter: None,
            recipient_object_filter: None,
            redirect_lifetime: RedirectionLifetime::OneOpportunity,
        }
    }
}

fn assert_inline_actor_shield(
    runner: &engine::game::scenario::GameRunner,
    source: ObjectId,
    expect_double: bool,
) {
    let shields: Vec<_> = runner
        .state()
        .objects
        .values()
        .flat_map(|object| object.replacement_definitions.iter_unchecked())
        .chain(runner.state().pending_damage_replacements.iter())
        .filter(|shield| shield_targets_chosen_source(shield, source))
        .collect();
    assert_eq!(
        shields.len(),
        1,
        "the actual inline activation installs exactly one shield"
    );
    let shield = shields[0];
    if expect_double {
        assert_eq!(shield.shield_kind, ShieldKind::DamageReplacementOneShot);
        assert_eq!(shield.damage_modification, Some(DamageModification::Double));
    } else {
        assert_eq!(shield.shield_kind, ShieldKind::PreventionOneShot);
        assert_eq!(shield.damage_modification, None);
    }
}

fn inline_actor_fixture(
    prevention: bool,
    chooser: Option<engine::types::ability::Chooser>,
    empty: bool,
) -> (engine::game::scenario::GameRunner, ObjectId, ObjectId) {
    use engine::types::ability::*;
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_creature(P0, "Typed damage-source witness", 2, 2)
        .id();
    let predicate = if empty {
        TargetFilter::None
    } else {
        TargetFilter::SpecificObject { id: source }
    };
    let installer = scenario
        .add_artifact_from_oracle(P0, "Typed inline actor witness", "")
        .with_ability_definition(
            AbilityDefinition::new(
                AbilityKind::Activated,
                inline_actor_effect(prevention, chooser, predicate),
            )
            .cost(AbilityCost::Mana {
                cost: ManaCost::generic(1),
            }),
        )
        .id();
    let mut runner = scenario.build();
    add_mana(&mut runner, &[ManaType::Colorless]);
    runner
        .act(GameAction::ActivateAbility {
            source_id: installer,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("native actor fixture must actually activate");
    assert_eq!(
        runner.state().stack.len(),
        1,
        "paid public activation reaches the stack"
    );
    assert_eq!(
        runner.state().players[P0.0 as usize].mana_pool.total(),
        0,
        "public activation pays its cost"
    );
    (runner, installer, source)
}

fn advance_inline_actor(runner: &mut engine::game::scenario::GameRunner) -> Result<(), String> {
    for _ in 0..32 {
        match &runner.state().waiting_for {
            WaitingFor::DamageSourceChoice { .. } => return Ok(()),
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => return Ok(()),
            WaitingFor::Priority { .. } | WaitingFor::ManaPayment { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .map_err(|error| error.to_string())?;
            }
            other => panic!("unexpected inline actor prompt: {other:?}"),
        }
    }
    panic!("inline actor activation did not reach a terminal prompt");
}

#[test]
fn inline_source_controller_public_activation_choice_restore_and_shield_application() {
    use engine::types::ability::Chooser;
    use engine::types::resolution::ResolutionStateWire;
    for prevention in [false, true] {
        for wire in [false, true] {
            let (mut runner, _, source) =
                inline_actor_fixture(prevention, Some(Chooser::Controller), false);
            advance_inline_actor(&mut runner).expect("supported actor reaches the choice");
            let WaitingFor::DamageSourceChoice {
                player, options, ..
            } = &runner.state().waiting_for
            else {
                panic!("Controller must open its real public prompt");
            };
            assert_eq!(*player, P0);
            assert_eq!(
                options.len(),
                1,
                "the qualifier retains its candidate domain"
            );
            let reference = options[0].identity.reference;
            assert_eq!(reference.object_id, source);
            *runner.state_mut() = if wire {
                serde_json::from_value::<ResolutionStateWire>(
                    serde_json::to_value(ResolutionStateWire::from_game_state(
                        runner.state().clone(),
                    ))
                    .unwrap(),
                )
                .unwrap()
                .into_game_state()
            } else {
                serde_json::from_value(serde_json::to_value(runner.state()).unwrap()).unwrap()
            };
            runner
                .act(GameAction::ChooseDamageSource { source: reference })
                .expect("public answer resumes the shield");
            runner.advance_until_stack_empty();
            assert_source_program_complete(&runner);
            assert_inline_actor_shield(&runner, source, !prevention);
            assert_damage_outcome(&mut runner, source, !prevention);
        }
    }
}

#[test]
fn inline_source_unsupported_actors_refuse_before_prompt_or_shield_even_when_bound() {
    use engine::types::ability::Chooser;
    use engine::types::game_state::ChosenDamageSource;
    for prevention in [false, true] {
        let (mut positive, _, source) =
            inline_actor_fixture(prevention, Some(Chooser::Controller), false);
        advance_inline_actor(&mut positive).unwrap();
        let WaitingFor::DamageSourceChoice { options, .. } = &positive.state().waiting_for else {
            panic!("paired Controller activation must reach the same choice owner");
        };
        let reference = options
            .iter()
            .find(|option| option.identity.reference.object_id == source)
            .unwrap()
            .identity
            .reference;
        positive
            .act(GameAction::ChooseDamageSource { source: reference })
            .unwrap();
        positive.advance_until_stack_empty();
        assert_source_program_complete(&positive);
        assert_inline_actor_shield(&positive, source, !prevention);
        assert_damage_outcome(&mut positive, source, !prevention);

        for chooser in [Chooser::Opponent, Chooser::OwningPlayer] {
            for bound in [false, true] {
                let (mut runner, _, source) =
                    inline_actor_fixture(prevention, Some(chooser), false);
                if bound {
                    let selected = engine::game::triggers::trigger_source_context_for_latch(
                        runner.state(),
                        &runner.state().objects[&source],
                    );
                    runner
                        .state_mut()
                        .stack
                        .back_mut()
                        .unwrap()
                        .ability_mut()
                        .unwrap()
                        .context
                        .damage_source_choice = Some(Box::new(ChosenDamageSource {
                        source: Some(selected),
                        candidate_filter: TargetFilter::SpecificObject { id: source },
                    }));
                }
                // PassPriority completes the stack step even when its effect
                // refuses an unsupported instruction. Refusal is observable as
                // no prompt, continuation, or shield, not as an action error.
                advance_inline_actor(&mut runner)
                    .expect("the legal public priority action must complete");
                assert_source_program_complete(&runner);
                assert!(runner.state().return_result_frames.is_empty());
                assert!(!matches!(
                    runner.state().waiting_for,
                    WaitingFor::DamageSourceChoice { .. }
                ));
                assert!(
                    runner.state().active_ability_continuation().is_none(),
                    "refusal must not park a source-choice continuation"
                );
                assert!(
                    shields_for_chosen_source(&runner, source).is_empty(),
                    "refusal must not publish a shield"
                );
                assert!(runner.state().pending_damage_replacements.is_empty());
                assert!(
                    runner.state().objects.values().all(|object| object
                        .replacement_definitions
                        .iter_unchecked()
                        .next()
                        .is_none()),
                    "unsupported actors must not publish a differently scoped shield either"
                );
            }
        }
    }
}

#[test]
fn inline_source_lexical_none_and_empty_controller_keep_their_existing_meanings() {
    use engine::types::ability::Chooser;
    use engine::types::game_state::ChosenDamageSource;
    for prevention in [false, true] {
        for bound in [false, true] {
            let (mut runner, _, source) = inline_actor_fixture(prevention, None, false);
            if bound {
                let selected = engine::game::triggers::trigger_source_context_for_latch(
                    runner.state(),
                    &runner.state().objects[&source],
                );
                runner
                    .state_mut()
                    .stack
                    .back_mut()
                    .unwrap()
                    .ability_mut()
                    .unwrap()
                    .context
                    .damage_source_choice = Some(Box::new(ChosenDamageSource {
                    source: Some(selected),
                    candidate_filter: TargetFilter::SpecificObject { id: source },
                }));
            }
            advance_inline_actor(&mut runner).expect("a lexical reader retains its meaning");
            assert!(
                !matches!(
                    runner.state().waiting_for,
                    WaitingFor::DamageSourceChoice { .. }
                ),
                "None must never reselect"
            );
            assert_source_program_complete(&runner);
            if bound {
                assert_inline_actor_shield(&runner, source, !prevention);
                assert_damage_outcome(&mut runner, source, !prevention);
            } else {
                assert!(shields_for_chosen_source(&runner, source).is_empty());
            }
        }
        let (mut runner, _, source) =
            inline_actor_fixture(prevention, Some(Chooser::Controller), true);
        advance_inline_actor(&mut runner).expect("empty supported domain completes");
        assert!(!matches!(
            runner.state().waiting_for,
            WaitingFor::DamageSourceChoice { .. }
        ));
        assert_source_program_complete(&runner);
        assert!(shields_for_chosen_source(&runner, source).is_empty());
    }
}

#[test]
fn inline_source_active_prompt_restore_refuses_unsupported_actor_without_mutating_input() {
    use engine::types::ability::Chooser;
    use engine::types::game_state::GameState;
    use engine::types::resolution::ResolutionStateWire;
    for prevention in [false, true] {
        let (mut runner, _, _) = inline_actor_fixture(prevention, Some(Chooser::Controller), false);
        advance_inline_actor(&mut runner).unwrap();
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::DamageSourceChoice { .. }
        ));
        let current = serde_json::to_value(runner.state()).unwrap();
        assert!(
            serde_json::from_value::<GameState>(current).is_ok(),
            "paired supported prompt restores"
        );
        for chooser in [Chooser::Opponent, Chooser::OwningPlayer] {
            let mut hostile = runner.state().clone();
            let effect = &mut hostile
                .active_ability_continuation_frame_mut()
                .unwrap()
                .pending
                .chain
                .effect;
            match effect {
                Effect::PreventDamage {
                    damage_source_filter:
                        Some(TargetFilter::ChosenDamageSource { chooser: slot, .. }),
                    ..
                }
                | Effect::CreateDamageReplacement {
                    source_filter: Some(TargetFilter::ChosenDamageSource { chooser: slot, .. }),
                    ..
                } => *slot = Some(chooser),
                other => panic!("expected inline owning producer, got {other:?}"),
            }
            for wire in [false, true] {
                let input = if wire {
                    serde_json::to_value(ResolutionStateWire::from_game_state(hostile.clone()))
                        .unwrap()
                } else {
                    serde_json::to_value(&hostile).unwrap()
                };
                let before = serde_json::to_vec(&input).unwrap();
                let error = if wire {
                    serde_json::from_value::<ResolutionStateWire>(input.clone())
                        .unwrap_err()
                        .to_string()
                } else {
                    serde_json::from_value::<GameState>(input.clone())
                        .unwrap_err()
                        .to_string()
                };
                assert!(
                    error.contains("unsupported inline damage-source chooser"),
                    "{error}"
                );
                assert_eq!(serde_json::to_vec(&input).unwrap(), before);
            }
        }
    }
}
