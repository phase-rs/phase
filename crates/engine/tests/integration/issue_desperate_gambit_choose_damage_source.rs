//! Desperate Gambit — "Choose a source you control" must parse as
//! `ChooseDamageSource` with a You-controller candidate filter. Its later
//! coin-flip shields bind the exact selected source independently of that eligibility.

use engine::game::effects::deal_damage;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{
    AbilityKind, ControllerRef, DamageModification, Effect, PreventionAmount, QuantityExpr,
    ReplacementDefinition, ResolvedAbility, ShieldKind, TargetFilter, TargetRef, TypedFilter,
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
        let ai = engine::ai_support::candidates::candidate_actions_exact(runner.state());
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
            target: TargetRef::Player(P0),
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
                .iter_all()
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
        runner.activate(source, 0).resolve();
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
