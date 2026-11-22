use crate::support::shared_card_db;
use engine::game::casting::activated_ability_definitions;
use engine::game::combat::AttackTarget;
use engine::game::effects::resolve_effect;
use engine::game::game_object::AttachTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{
    ContinuousModification, Duration, Effect, ResolvedAbility, StaticDefinition, TargetFilter,
    TargetRef,
};
use engine::types::ability_visit::{visit_ability_def, visit_trigger};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard};
use engine::types::phase::{Phase, PhaseStop, PhaseStopScope};
use engine::types::player::PlayerId;
use engine::types::statics::{CostModifyMode, StaticMode};
use engine::types::zones::Zone;
use std::ops::ControlFlow;

const PANIC: &str = r#"Cast this spell only during combat before blockers are declared.
Target creature can't block this turn.
Draw a card at the beginning of the next turn's upkeep."#;
const INCITE: &str = r#"Radiance — Until end of turn, target creature and each other creature that shares a color with it gain "This creature can't block.""#;
const ARTFUL_DODGE: &str = "Target creature can't be blocked this turn.\nFlashback {U} (You may cast this card from your graveyard for its flashback cost. Then exile it.)";
const DRESS_DOWN: &str = r#"Flash
When this enchantment enters, draw a card.
Creatures lose all abilities.
At the beginning of the end step, sacrifice this enchantment."#;

const FORBIDDING_SPIRIT_ORACLE: &str = "When this creature enters, until your next turn, creatures can't attack you or planeswalkers you control unless their controller pays {2} for each of those creatures.";
const WILL_ORACLE: &str = "Vigilance\n{T}: Spells you cast this turn that are white and/or blue cost {X} less to cast, where X is the amount of life you gained this turn. Activate only as a sorcery.";
const WAR_CADENCE_ORACLE: &str = "{X}{R}: This turn, creatures can't block unless their controller pays {X} for each blocking creature they control.";
const LEGEND_OF_ARENA_ORACLE: &str = "The Legend of Arena can be your commander.\nI, II — Create a 2/1 red Human Wizard creature token. Spells you cast this turn cost {1} less to cast for each Wizard you control.\nIII — Search your library for a planeswalker card, put it onto the battlefield, then shuffle your library. It enters with an additional loyalty counter on it for each Wizard you control.";
const TEYO_ORACLE: &str = "When Teyo enters, create a 0/4 white Wall creature token with defender and flying.\n[+1]: You and target opponent each draw a card.\n[−2]: Choose left or right. Until your next turn, each player may attack only the nearest opponent in the last chosen direction and planeswalkers controlled by that opponent.";
const MOON_ORACLE: &str = "Enchant creature, land, or planeswalker\nEnchanted permanent is a colorless land with \"{T}: Add {C}\" and loses all other card types and abilities.";

fn declare_attack_and_reach_blocks(runner: &mut GameRunner, attacker: ObjectId) {
    runner.advance_to_phase(Phase::BeginCombat);
    runner
        .declare_attackers(&[(attacker, AttackTarget::Player(P1))])
        .unwrap();
    reach_blocks(runner, P1);
}

fn reach_blocks(runner: &mut GameRunner, defender: PlayerId) {
    // Keep the declaration observable even when the defender has zero legal blocks.
    runner
        .state_mut()
        .phase_stops
        .entry(defender)
        .or_default()
        .push(PhaseStop {
            phase: Phase::DeclareBlockers,
            scope: PhaseStopScope::AllTurns,
        });
    for _ in 0..8 {
        match runner.state().waiting_for {
            WaitingFor::DeclareBlockers { player, .. } => {
                assert_eq!(player, defender);
                assert_eq!(runner.state().phase, Phase::DeclareBlockers);
                return;
            }
            WaitingFor::Priority { .. } if runner.state().phase == Phase::DeclareAttackers => {
                runner.act(GameAction::PassPriority).unwrap();
            }
            ref other => panic!("expected attacker priority or blocker declaration, got {other:?}"),
        }
    }
    panic!("did not reach blocker declaration");
}

fn reach_end_step(runner: &mut GameRunner) {
    let turn = runner.state().turn_number;
    for _ in 0..32 {
        assert_eq!(runner.state().turn_number, turn);
        if runner.state().phase == Phase::End {
            return;
        }
        match runner.state().waiting_for {
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).unwrap();
            }
            WaitingFor::DeclareAttackers { .. } => {
                runner.declare_attackers(&[]).unwrap();
            }
            ref other => panic!("unexpected prompt on the way to the end step: {other:?}"),
        }
    }
    panic!("did not reach the end step");
}

fn cant_block_board(
    name: &str,
    oracle: &str,
    phase: Phase,
    cost: u32,
    remove_abilities: bool,
    quoted: bool,
) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(phase);
    scenario.with_library_top(P0, &["Island", "Mountain", "Island"]);
    for _ in 0..3 {
        scenario.add_basic_land(P0, ManaColor::Red);
    }
    for _ in 0..2 {
        scenario.add_basic_land(P0, ManaColor::Blue);
    }
    let attacker = scenario.add_creature(P0, "Attacker", 2, 2).id();
    let target = scenario.add_creature(P1, "Colorless target", 2, 2).id();
    let control = scenario.add_creature(P1, "Unrestricted blocker", 2, 2).id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, name, phase == Phase::BeginCombat, oracle)
        .with_mana_cost(ManaCost::Cost {
            generic: cost,
            shards: vec![ManaCostShard::Red],
        })
        .id();
    let dress_down = scenario
        .add_spell_to_hand_from_oracle(P0, "Dress Down", false, DRESS_DOWN)
        .as_enchantment()
        .with_mana_cost(ManaCost::Cost {
            generic: 1,
            shards: vec![ManaCostShard::Blue],
        })
        .id();
    let mut runner = scenario.build();
    assert!(runner.state().transient_continuous_effects.is_empty());

    runner.cast(spell).target_object(target).resolve();
    assert_eq!(runner.state().objects[&spell].zone, Zone::Graveyard);
    assert!(runner.state().stack.is_empty());
    assert!(matches!(runner.state().waiting_for, WaitingFor::Priority { player } if player == P0));

    let mut registered = runner
        .state()
        .transient_continuous_effects
        .iter()
        .filter(|effect| effect.source_id == spell);
    let effect = registered
        .next()
        .expect("resolved source-attributed restriction");
    assert!(registered.next().is_none());
    assert_eq!(effect.affected, TargetFilter::SpecificObject { id: target });
    assert_eq!(effect.duration, Duration::UntilEndOfTurn);
    if quoted {
        assert!(matches!(
            effect.modifications.as_slice(),
            [ContinuousModification::GrantStaticAbility { definition }]
                if definition.affected == Some(TargetFilter::SelfRef)
                    && definition.mode == StaticMode::CantBlock
        ));
    } else {
        assert_eq!(
            effect.modifications.as_slice(),
            &[ContinuousModification::AddStaticMode {
                mode: StaticMode::CantBlock,
            }]
        );
    }

    if remove_abilities {
        runner.cast(dress_down).resolve();
        runner.advance_until_stack_empty();
        assert_eq!(runner.state().objects[&dress_down].zone, Zone::Battlefield);
        assert!(runner.state().stack.is_empty());
    }

    declare_attack_and_reach_blocks(&mut runner, attacker);
    if remove_abilities {
        runner
            .declare_blockers(&[(target, attacker)])
            .expect("later Dress Down removes the grant");
    } else {
        assert!(runner.declare_blockers(&[(target, attacker)]).is_err());
        runner
            .declare_blockers(&[(control, attacker)])
            .expect("unaffected blocker remains legal");
    }
}

#[test]
fn panic_and_incite_resolve_distinct_restriction_carriers() {
    cant_block_board("Panic", PANIC, Phase::BeginCombat, 0, false, false);
    cant_block_board(
        "Incite Hysteria",
        INCITE,
        Phase::PreCombatMain,
        2,
        false,
        true,
    );
}

#[test]
fn later_dress_down_removes_incites_grant_from_its_actual_target() {
    cant_block_board(
        "Incite Hysteria",
        INCITE,
        Phase::PreCombatMain,
        2,
        true,
        true,
    );
}

#[test]
fn quoted_spell_cost_definitions_survive_generic_effect_dispatch() {
    for (name, text, core_type, expected_mode, generic) in [
        (
            "Richlau, Headmaster",
            r#"At the beginning of your end step, you may pay {1}. When you do, target artifact card in your graveyard perpetually gains "This spell costs {1} less to cast." If it's a creature or Vehicle card, it perpetually gets +2/+2. Put it into your library second from the top."#,
            "Creature",
            CostModifyMode::Reduce,
            1,
        ),
        (
            "Scalar Scholar",
            r#"Increment
When this creature enters, each card you own with {X} in its mana cost perpetually gains "This spell costs {1} less to cast.""#,
            "Creature",
            CostModifyMode::Reduce,
            1,
        ),
        (
            "Teferi's Contingency",
            r#"Counter target spell. Each card in its controller's graveyard, hand, and library with the same name as that spell perpetually gains "This spell costs {2} more to cast.""#,
            "Instant",
            CostModifyMode::Raise,
            2,
        ),
    ] {
        let parsed = parse_oracle_text(text, name, &[], &[core_type.into()], &[]);
        let mut parsed_effect = None;
        let mut visitor = |effect: &Effect| {
            if let Effect::GenericEffect {
                static_abilities, ..
            } = effect
            {
                let has_cost = static_abilities
                    .iter()
                    .flat_map(|def| &def.modifications)
                    .any(|m| match m {
                        ContinuousModification::AddStaticMode { mode } => {
                            matches!(mode, StaticMode::ModifyCost { .. })
                        }
                        ContinuousModification::GrantStaticAbility { definition } => {
                            matches!(definition.mode, StaticMode::ModifyCost { .. })
                        }
                        _ => false,
                    });
                if has_cost {
                    parsed_effect = Some(effect.clone());
                    return ControlFlow::Break(());
                }
            }
            ControlFlow::Continue(())
        };
        for ability in &parsed.abilities {
            if visit_ability_def(ability, &mut visitor).is_break() {
                break;
            }
        }
        for trigger in &parsed.triggers {
            if visit_trigger(trigger, &mut visitor).is_break() {
                break;
            }
        }
        let effect = parsed_effect.unwrap_or_else(|| panic!("{name}: parsed cost effect"));

        let mut scenario = GameScenario::new();
        let source = scenario.add_creature(P0, name, 2, 2).id();
        let selected = scenario
            .add_creature_to_graveyard(P0, "Artifact card", 1, 1)
            .as_artifact_creature()
            .id();
        let mut runner = scenario.build();
        let targets = if name == "Richlau, Headmaster" {
            vec![TargetRef::Object(selected)]
        } else {
            vec![]
        };
        let ability = ResolvedAbility::new(effect, targets, source, P0);
        assert!(runner.state().transient_continuous_effects.is_empty());
        resolve_effect(runner.state_mut(), &ability, &mut Vec::new()).unwrap();

        let Effect::GenericEffect {
            static_abilities, ..
        } = &ability.effect
        else {
            unreachable!();
        };
        let expected = &static_abilities[0].modifications;
        let registered = runner
            .state()
            .transient_continuous_effects
            .iter()
            .find(|effect| {
                effect.source_id == source && effect.modifications.as_slice() == expected
            })
            .unwrap_or_else(|| panic!("{name}: parsed cost modifications must reach registration"));
        let [ContinuousModification::GrantStaticAbility { definition }] =
            registered.modifications.as_slice()
        else {
            panic!(
                "{name}: expected full grant, got {:?}",
                registered.modifications
            );
        };
        assert_eq!(
            definition.active_zones,
            [
                Zone::Hand,
                Zone::Stack,
                Zone::Command,
                Zone::Graveyard,
                Zone::Exile,
                Zone::Library,
            ]
        );
        assert!(matches!(
            &definition.mode,
            StaticMode::ModifyCost { mode, amount, .. }
                if *mode == expected_mode && *amount == ManaCost::generic(generic)
        ));
    }
}

#[test]
fn stilt_man_sacrifice_eligibility_reads_resolved_grant_before_layers_flush() {
    use engine::game::casting::find_eligible_sacrifice_targets;
    use engine::game::layers::flush_layers;
    use engine::types::ability::{TypeFilter, TypedFilter};

    let db = shared_card_db().expect("fixture database");
    let mut scenario = GameScenario::new();
    let source = scenario.add_real_card(P0, "Stilt-Man, Towering Terror", Zone::Battlefield, db);
    let target = scenario
        .add_creature(P0, "Stolen artifact", 1, 1)
        .as_artifact()
        .id();
    let control = scenario
        .add_creature(P0, "Sacrifice control", 1, 1)
        .as_artifact()
        .id();
    let mut runner = scenario.build();
    let face = db
        .get_face_by_name("Stilt-Man, Towering Terror")
        .expect("fixture face");
    let mut grant = None;
    for trigger in &face.triggers {
        let _ = visit_trigger(trigger, &mut |effect| {
            if let Effect::GenericEffect {
                static_abilities, ..
            } = effect
            {
                if static_abilities
                    .iter()
                    .flat_map(|outer| &outer.modifications)
                    .any(|m| {
                        let mode = match m {
                            ContinuousModification::AddStaticMode { mode } => mode,
                            ContinuousModification::GrantStaticAbility { definition } => {
                                &definition.mode
                            }
                            _ => return false,
                        };
                        matches!(mode, StaticMode::Other(name) if name == "CantBeSacrificed")
                    })
                {
                    grant = Some(effect.clone());
                    return ControlFlow::Break(());
                }
            }
            ControlFlow::Continue(())
        });
    }
    let eligible = |runner: &GameRunner| {
        find_eligible_sacrifice_targets(
            runner.state(),
            P0,
            source,
            None,
            &TargetFilter::Typed(TypedFilter::new(TypeFilter::Artifact)),
        )
    };
    let has_graft = |runner: &GameRunner| {
        let state = runner.state();
        engine::game::functioning_abilities::active_static_definitions(
            state,
            &state.objects[&target],
        )
        .any(|def| matches!(&def.mode, StaticMode::Other(name) if name == "CantBeSacrificed"))
    };
    assert!(eligible(&runner).contains(&target));
    let ability = ResolvedAbility::new(
        grant.expect("fixture-backed sacrifice grant"),
        vec![TargetRef::Object(target)],
        source,
        P0,
    );
    resolve_effect(runner.state_mut(), &ability, &mut Vec::new()).unwrap();
    assert!(runner
        .state()
        .transient_continuous_effects
        .iter()
        .any(|effect| {
            effect.source_id == source
                && effect.affected == TargetFilter::SpecificObject { id: target }
        }));
    assert!(!has_graft(&runner), "the layer flush has not run");
    let before_flush = eligible(&runner);
    assert!(!before_flush.contains(&target));
    assert!(before_flush.contains(&control));
    flush_layers(runner.state_mut());
    assert!(has_graft(&runner));
    let after_flush = eligible(&runner);
    assert!(!after_flush.contains(&target));
    assert!(after_flush.contains(&control));
}

fn assert_priority(runner: &GameRunner, player: engine::types::player::PlayerId) {
    assert!(runner.state().stack.is_empty());
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { player: actual } if actual == player),
        "expected priority for {player:?} during {:?}, got {:?}",
        runner.state().phase,
        runner.state().waiting_for,
    );
}

#[test]
fn craterous_stomp_keeps_coward_type_after_dress_down_removes_block_restriction() {
    let db = shared_card_db().expect("fixture database");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Island", "Mountain"]);
    for color in [
        ManaColor::Red,
        ManaColor::Red,
        ManaColor::Blue,
        ManaColor::Blue,
    ] {
        scenario.add_basic_land(P0, color);
    }
    let giant = scenario
        .add_creature(P0, "Attacking Giant", 3, 3)
        .with_subtypes(vec!["Giant"])
        .id();
    let other_attacker = scenario.add_creature(P0, "Non-Giant attacker", 2, 2).id();
    let victim = scenario.add_creature(P1, "Damage target", 2, 2).id();
    let blocker = scenario.add_creature(P1, "Surviving blocker", 2, 2).id();
    let stomp = scenario.add_real_card(P0, "Craterous Stomp", Zone::Hand, db);
    let dress = scenario
        .add_spell_to_hand_from_oracle(P0, "Dress Down", false, DRESS_DOWN)
        .as_enchantment()
        .with_mana_cost(ManaCost::Cost {
            generic: 1,
            shards: vec![ManaCostShard::Blue],
        })
        .id();
    let mut runner = scenario.build();
    runner.cast(stomp).target_object(victim).resolve();
    assert_eq!(runner.state().objects[&victim].zone, Zone::Graveyard);
    assert_priority(&runner, P0);
    let is_coward = |runner: &GameRunner| {
        runner.state().objects[&blocker]
            .card_types
            .subtypes
            .iter()
            .any(|subtype| subtype == "Coward")
    };
    assert!(is_coward(&runner));
    let mut before = GameRunner::from_state(runner.state().clone());
    before.advance_to_phase(Phase::BeginCombat);
    before
        .declare_attackers(&[
            (giant, AttackTarget::Player(P1)),
            (other_attacker, AttackTarget::Player(P1)),
        ])
        .unwrap();
    reach_blocks(&mut before, P1);
    assert!(before.declare_blockers(&[(blocker, giant)]).is_err());
    before
        .declare_blockers(&[(blocker, other_attacker)])
        .unwrap();

    runner.cast(dress).resolve();
    runner.advance_until_stack_empty();
    assert_eq!(runner.state().objects[&dress].zone, Zone::Battlefield);
    assert!(is_coward(&runner));
    assert_priority(&runner, P0);
    declare_attack_and_reach_blocks(&mut runner, giant);
    runner
        .declare_blockers(&[(blocker, giant)])
        .expect("Coward can block after losing the ability");
}

#[test]
fn prison_barricade_attacks_only_after_paid_kicker() {
    let db = shared_card_db().expect("fixture database");
    for kicked in [true, false] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        scenario.with_library_top(P0, &["Plains", "Plains"]);
        scenario.with_library_top(P1, &["Plains", "Plains"]);
        for _ in 0..4 {
            scenario.add_basic_land(P0, ManaColor::White);
        }
        let barricade = scenario.add_real_card(P0, "Prison Barricade", Zone::Hand, db);
        let mut runner = scenario.build();
        if kicked {
            runner.cast(barricade).accept_optional().resolve();
        } else {
            runner.cast(barricade).decline_optional().resolve();
        }
        assert_priority(&runner, P0);
        let object = &runner.state().objects[&barricade];
        assert_eq!(object.zone, Zone::Battlefield);
        assert_eq!(
            object
                .static_definitions
                .iter_all()
                .any(|def| def.mode == StaticMode::CanAttackWithDefender),
            kicked
        );
        assert_eq!(
            object
                .counters
                .get(&engine::types::CounterType::Plus1Plus1)
                .copied()
                .unwrap_or(0),
            u32::from(kicked)
        );
        runner.advance_to_end_step();
        runner.advance_to_phase(Phase::PreCombatMain);
        runner.advance_to_end_step();
        runner.advance_to_phase(Phase::BeginCombat);
        assert_eq!(runner.state().active_player, P0);
        assert_eq!(
            runner
                .declare_attackers(&[(barricade, AttackTarget::Player(P1))])
                .is_ok(),
            kicked
        );
    }
}

fn assert_graveyard_quote(
    face: &engine::types::CardFace,
    mode: engine::types::ability::CardPlayMode,
) {
    let mut found = false;
    for ability in &face.abilities {
        let _ = visit_ability_def(ability, &mut |effect| {
            if let Effect::GenericEffect {
                static_abilities, ..
            } = effect
            {
                found |= static_abilities.iter().flat_map(|definition| &definition.modifications).any(
                    |modification| matches!(modification,
                        ContinuousModification::GrantStaticAbility { definition }
                        if definition.affected == Some(TargetFilter::SelfRef)
                            && definition.active_zones == [Zone::Graveyard]
                            && matches!(definition.mode, StaticMode::GraveyardCastPermission { play_mode, .. } if play_mode == mode)
                    )
                );
            }
            ControlFlow::<()>::Continue(())
        });
    }
    assert!(
        found,
        "full graveyard permission for {:?}",
        face.oracle_text
    );
}

#[test]
fn kethis_resolves_full_play_permission_but_cannot_play_graveyard_cards() {
    use engine::ai_support::legal_actions;
    use engine::game::casting::{can_cast_object_now, spell_objects_available_to_cast};
    use engine::types::ability::CardPlayMode;
    use engine::types::game_state::CastPaymentMode;

    let db = shared_card_db().expect("fixture database");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_basic_land(P0, ManaColor::White);
    scenario.add_basic_land(P0, ManaColor::Black);
    let source = scenario.add_real_card(P0, "Kethis, the Hidden Hand", Zone::Battlefield, db);
    let selected = [
        scenario.add_real_card(P0, "Boseiju, Who Endures", Zone::Graveyard, db),
        scenario.add_real_card(P0, "Karakas", Zone::Graveyard, db),
    ];
    let creature = scenario.add_real_card(P0, "Isamaru, Hound of Konda", Zone::Graveyard, db);
    let land = scenario.add_real_card(P0, "Kher Keep", Zone::Graveyard, db);
    let nonlegendary = scenario.add_real_card(P0, "Grizzly Bears", Zone::Graveyard, db);
    let later = scenario.add_real_card(P0, "Isamaru, Hound of Konda", Zone::Battlefield, db);
    let rites = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Village Rites",
            true,
            r#"As an additional cost to cast this spell, sacrifice a creature.
Draw two cards."#,
        )
        .with_mana_cost(ManaCost::Cost {
            generic: 0,
            shards: vec![ManaCostShard::Black],
        })
        .id();
    scenario.with_library_top(P0, &["Forest", "Forest"]);
    let mut runner = scenario.build();
    assert_graveyard_quote(
        db.get_face_by_name("Kethis, the Hidden Hand").unwrap(),
        CardPlayMode::Play,
    );
    let index = activated_ability_definitions(runner.state(), source)
        .into_iter()
        .next()
        .expect("Kethis activation")
        .0;
    runner
        .act(GameAction::ActivateAbility {
            source_id: source,
            ability_index: index,
            payment_mode: CastPaymentMode::Auto,
        })
        .unwrap();
    let WaitingFor::PayCost {
        player,
        count,
        choices,
        ..
    } = &runner.state().waiting_for
    else {
        panic!("expected Kethis's exile cost prompt");
    };
    assert_eq!(*player, P0);
    assert_eq!(*count, 2);
    assert!(selected.iter().all(|id| choices.contains(id)));
    assert!(!choices.contains(&nonlegendary));
    runner
        .act(GameAction::SelectCards {
            cards: selected.to_vec(),
        })
        .unwrap();
    for id in selected {
        assert_eq!(runner.state().objects[&id].zone, Zone::Exile);
    }
    assert!(!runner.state().stack.is_empty());
    runner.advance_until_stack_empty();
    assert_priority(&runner, P0);

    assert_eq!(runner.state().objects[&creature].zone, Zone::Graveyard);
    assert!(!spell_objects_available_to_cast(runner.state(), P0).contains(&creature));
    assert!(!can_cast_object_now(runner.state(), P0, creature));
    let card_id = runner.state().objects[&creature].card_id;
    assert!(runner
        .act(GameAction::CastSpell {
            object_id: creature,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .is_err());

    assert_eq!(runner.state().objects[&land].zone, Zone::Graveyard);
    assert!(!legal_actions(runner.state()).iter().any(|action| matches!(
        action, GameAction::PlayLand { object_id, .. } if *object_id == land
    )));
    let card_id = runner.state().objects[&land].card_id;
    assert!(runner
        .act(GameAction::PlayLand {
            object_id: land,
            card_id
        })
        .is_err());

    assert_eq!(runner.state().objects[&later].zone, Zone::Battlefield);
    runner.cast(rites).sacrifice_with(&[later]).resolve();
    assert_eq!(runner.state().objects[&later].zone, Zone::Graveyard);
    assert!(!spell_objects_available_to_cast(runner.state(), P0).contains(&later));
    assert!(runner.cast(later).try_resolve().is_err());

    let mut control = GameScenario::new();
    control.at_phase(Phase::PreCombatMain);
    control.add_basic_land(P0, ManaColor::White);
    control.add_real_card(P0, "Crucible of Worlds", Zone::Battlefield, db);
    control.add_real_card(P0, "Karador, Ghost Chieftain", Zone::Battlefield, db);
    let land = control.add_real_card(P0, "Kher Keep", Zone::Graveyard, db);
    let creature = control.add_real_card(P0, "Isamaru, Hound of Konda", Zone::Graveyard, db);
    let mut control = control.build();
    assert!(spell_objects_available_to_cast(control.state(), P0).contains(&creature));
    control.cast(creature).resolve();
    assert_eq!(control.state().objects[&creature].zone, Zone::Battlefield);
    let card_id = control.state().objects[&land].card_id;
    control
        .act(GameAction::PlayLand {
            object_id: land,
            card_id,
        })
        .unwrap();
    assert_eq!(control.state().objects[&land].zone, Zone::Battlefield);
}

#[test]
fn case_solves_and_resolves_full_cast_permission_but_graveyard_card_cannot_cast() {
    use engine::game::casting::{can_cast_object_now, spell_objects_available_to_cast};
    use engine::types::ability::CardPlayMode;

    let db = shared_card_db().expect("fixture database");
    for permission_control in [false, true] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        scenario.with_library_top(P0, &["Forest", "Forest"]);
        scenario.with_library_top(P1, &["Forest", "Forest"]);
        for _ in 0..4 {
            scenario.add_basic_land(P0, ManaColor::Green);
        }
        let source = scenario.add_real_card(P0, "Case of the Uneaten Feast", Zone::Battlefield, db);
        if permission_control {
            scenario.add_real_card(P0, "Karador, Ghost Chieftain", Zone::Battlefield, db);
        }
        let target = scenario.add_real_card(P0, "Grizzly Bears", Zone::Graveyard, db);
        let nourish = scenario
            .add_spell_to_hand_from_oracle(P0, "Nourish", true, r#"You gain 6 life."#)
            .with_mana_cost(ManaCost::Cost {
                generic: 0,
                shards: vec![ManaCostShard::Green, ManaCostShard::Green],
            })
            .id();
        let mut runner = scenario.build();
        assert_graveyard_quote(
            db.get_face_by_name("Case of the Uneaten Feast").unwrap(),
            CardPlayMode::Cast,
        );
        let solved = |runner: &GameRunner| {
            runner.state().objects[&source]
                .case_state
                .as_ref()
                .expect("Case state")
                .is_solved
        };
        assert!(!solved(&runner));
        let mut unsolved = GameRunner::from_state(runner.state().clone());
        reach_end_step(&mut unsolved);
        unsolved.advance_until_stack_empty();
        assert!(!solved(&unsolved));

        assert_eq!(runner.cast(nourish).resolve().life_delta(P0), 6);
        reach_end_step(&mut runner);
        runner.advance_until_stack_empty();
        assert!(solved(&runner));
        runner.advance_to_phase(Phase::PreCombatMain);
        reach_end_step(&mut runner);
        runner.advance_to_phase(Phase::PreCombatMain);
        assert_eq!(runner.state().phase, Phase::PreCombatMain);
        assert_priority(&runner, P0);
        let index = activated_ability_definitions(runner.state(), source)
            .into_iter()
            .next()
            .expect("solved activation")
            .0;
        runner.activate(source, index).pay_with(&[source]).resolve();
        runner.advance_until_stack_empty();
        assert_eq!(runner.state().objects[&source].zone, Zone::Graveyard);
        assert_priority(&runner, P0);

        assert_eq!(runner.state().objects[&target].zone, Zone::Graveyard);
        assert_eq!(
            spell_objects_available_to_cast(runner.state(), P0).contains(&target),
            permission_control
        );
        assert_eq!(
            can_cast_object_now(runner.state(), P0, target),
            permission_control
        );
        if permission_control {
            runner.cast(target).resolve();
            assert_eq!(runner.state().objects[&target].zone, Zone::Battlefield);
        } else {
            assert!(runner.cast(target).try_resolve().is_err());
        }
    }
}

fn activation(runner: &GameRunner, source: ObjectId) -> usize {
    activated_ability_definitions(runner.state(), source)
        .into_iter()
        .next()
        .expect("printed activation available")
        .0
}

fn token_named(runner: &GameRunner, name: &str) -> ObjectId {
    runner
        .state()
        .battlefield
        .iter()
        .copied()
        .find(|id| {
            let object = &runner.state().objects[id];
            object.is_token && object.name == name
        })
        .unwrap_or_else(|| panic!("expected created {name} token"))
}

fn assert_token_grant(
    runner: &GameRunner,
    token: ObjectId,
    matches_mode: impl Fn(&StaticMode) -> bool,
) {
    assert!(
        has_full_self_grant(
            runner.state().objects[&token].static_definitions.iter_all(),
            &matches_mode
        ),
        "expected full self grant on token {token:?}"
    );
}

fn has_full_self_grant<'a>(
    definitions: impl Iterator<Item = &'a StaticDefinition>,
    matches_mode: impl Fn(&StaticMode) -> bool,
) -> bool {
    definitions.flat_map(|outer| &outer.modifications).any(|m| matches!(m,
            ContinuousModification::GrantStaticAbility { definition }
                if definition.affected == Some(TargetFilter::SelfRef) && matches_mode(&definition.mode)
        ))
}

fn assert_fresh_token_grant(
    face: &engine::types::CardFace,
    matches_mode: impl Fn(&StaticMode) -> bool,
) {
    let types: Vec<_> = face
        .card_type
        .core_types
        .iter()
        .map(ToString::to_string)
        .collect();
    let parsed = parse_oracle_text(
        face.oracle_text.as_deref().expect("fixture Oracle text"),
        &face.name,
        &[],
        &types,
        &face.card_type.subtypes,
    );
    let mut visit = |effect: &Effect| {
        let found = match effect {
            Effect::Token {
                static_abilities, ..
            }
            | Effect::GenericEffect {
                static_abilities,
                target: Some(TargetFilter::LastCreated),
                ..
            } => has_full_self_grant(static_abilities.iter(), &matches_mode),
            _ => false,
        };
        if found {
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    };
    assert!(
        parsed
            .abilities
            .iter()
            .any(|ability| visit_ability_def(ability, &mut visit).is_break())
            || parsed
                .triggers
                .iter()
                .any(|trigger| visit_trigger(trigger, &mut visit).is_break()),
        "fresh Oracle parse of {} must produce a full self grant for the token",
        face.name
    );
}

#[test]
fn cloudseeder_token_blocks_flying_but_not_ground() {
    use engine::types::game_state::CastPaymentMode;
    use engine::types::keywords::Keyword;

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P1, &["Island"]);
    scenario.add_basic_land(P0, ManaColor::Blue);
    let source = scenario.add_creature_from_oracle(P0, "Cloudseeder", 1, 1, r#"Flying
{U}, {T}, Discard a card: Create a 1/1 blue Faerie creature token named Cloud Sprite. It has flying and "This token can block only creatures with flying.""#).id();
    let discarded = scenario.add_card_to_hand(P0, "Island");
    let unrestricted = scenario.add_vanilla(P0, 2, 2);
    let air = scenario
        .add_creature(P1, "Flying attacker", 2, 2)
        .with_keyword(Keyword::Flying)
        .id();
    let ground = scenario.add_vanilla(P1, 2, 2);
    let mut runner = scenario.build();
    let index = activation(&runner, source);
    runner
        .act(GameAction::ActivateAbility {
            source_id: source,
            ability_index: index,
            payment_mode: CastPaymentMode::Auto,
        })
        .unwrap();
    let WaitingFor::PayCost {
        player,
        choices,
        count,
        ..
    } = &runner.state().waiting_for
    else {
        panic!(
            "expected discard prompt, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(*player, P0);
    assert_eq!(*count, 1);
    assert!(choices.contains(&discarded));
    runner
        .act(GameAction::SelectCards {
            cards: vec![discarded],
        })
        .unwrap();
    runner.advance_until_stack_empty();
    assert_priority(&runner, P0);
    assert_eq!(runner.state().objects[&discarded].zone, Zone::Graveyard);
    assert!(runner.state().objects[&source].tapped);
    let token = token_named(&runner, "Cloud Sprite");
    assert_token_grant(&runner, token, |mode| {
        matches!(mode, StaticMode::BlockRestriction { .. })
    });

    reach_end_step(&mut runner);
    runner.advance_to_phase(Phase::BeginCombat);
    assert_eq!(runner.state().active_player, P1);
    runner
        .declare_attackers(&[
            (air, AttackTarget::Player(P0)),
            (ground, AttackTarget::Player(P0)),
        ])
        .unwrap();
    reach_blocks(&mut runner, P0);
    let mut legal = GameRunner::from_state(runner.state().clone());
    assert!(runner.declare_blockers(&[(token, ground)]).is_err());
    legal
        .declare_blockers(&[(token, air), (unrestricted, ground)])
        .unwrap();
}

#[test]
fn cloudspire_pilot_can_pay_crew_and_saddle_three() {
    let db = shared_card_db().expect("fixture database");
    assert_fresh_token_grant(
        db.get_face_by_name("Cloudspire Coordinator")
            .expect("fixture face"),
        |mode| matches!(mode, StaticMode::CrewContribution { .. }),
    );
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario.add_real_card(P0, "Cloudspire Coordinator", Zone::Battlefield, db);
    let ordinary = scenario.add_vanilla(P0, 1, 1);
    for _ in 0..6 {
        scenario.add_basic_land(P0, ManaColor::White);
    }
    let vehicle = scenario.add_artifact_to_hand_from_oracle(P0, "Cultivator's Caravan", r#"{T}: Add one mana of any color.
Crew 3 (Tap any number of creatures you control with total power 3 or more: This Vehicle becomes an artifact creature until end of turn.)"#)
        .with_subtypes(vec!["Vehicle"]).with_mana_cost(ManaCost::generic(3)).id();
    let mount = scenario.add_creature_to_hand_from_oracle(P0, "Brightfield Glider", 1, 1, r#"Vigilance
Whenever this creature attacks while saddled, it gets +1/+2 and gains flying until end of turn.
Saddle 3 (Tap any number of other creatures you control with total power 3 or more: This Mount becomes saddled until end of turn. Saddle only as a sorcery.)"#)
        .with_subtypes(vec!["Mount"]).with_mana_cost(ManaCost::generic(1)).id();
    let mut runner = scenario.build();
    runner.cast(vehicle).resolve();
    runner.cast(mount).resolve();
    let index = activation(&runner, source);
    runner.activate(source, index).resolve();
    assert_priority(&runner, P0);
    assert!(runner.state().objects[&source].tapped);
    let pilot = token_named(&runner, "Pilot");
    assert_eq!(
        runner
            .state()
            .battlefield
            .iter()
            .filter(|id| runner.state().objects[*id].name == "Pilot")
            .count(),
        2
    );
    assert_token_grant(&runner, pilot, |mode| {
        matches!(mode, StaticMode::CrewContribution { .. })
    });
    for crew in [true, false] {
        let action = |creature_ids| {
            if crew {
                GameAction::CrewVehicle {
                    vehicle_id: vehicle,
                    creature_ids,
                }
            } else {
                GameAction::SaddleMount {
                    mount_id: mount,
                    creature_ids,
                }
            }
        };
        let mut branch = GameRunner::from_state(runner.state().clone());
        branch.act(action(vec![])).unwrap();
        let (player, threshold, eligible, contributions) = match &branch.state().waiting_for {
            WaitingFor::CrewVehicle {
                player,
                vehicle_id,
                crew_power,
                eligible_creatures,
                contributions,
            } if crew && *vehicle_id == vehicle => {
                (*player, *crew_power, eligible_creatures, contributions)
            }
            WaitingFor::SaddleMount {
                player,
                mount_id,
                saddle_power,
                eligible_creatures,
                contributions,
            } if !crew && *mount_id == mount => {
                (*player, *saddle_power, eligible_creatures, contributions)
            }
            other => panic!("expected payer prompt, got {other:?}"),
        };
        assert_eq!((player, threshold), (P0, 3));
        for (id, expected) in [(ordinary, 1), (pilot, 3)] {
            let index = eligible
                .iter()
                .position(|candidate| *candidate == id)
                .expect("payer offered");
            assert_eq!(contributions[index], expected);
        }
        let mut control = GameRunner::from_state(branch.state().clone());
        assert!(control.act(action(vec![ordinary])).is_err());
        assert!(!control.state().objects[&ordinary].tapped);
        branch.act(action(vec![pilot])).unwrap();
        assert!(branch.state().objects[&pilot].tapped);
        branch.advance_until_stack_empty();
        assert_priority(&branch, P0);
        if crew {
            use engine::types::card_type::CoreType;
            assert!(branch.state().objects[&vehicle]
                .card_types
                .core_types
                .contains(&CoreType::Creature));
        } else {
            assert_eq!(branch.state().objects[&mount].saddled_by, [pilot]);
        }
    }
}

#[test]
fn tetravite_refuses_aura_movement() {
    use engine::types::game_state::CounterRemoveChoice;
    use engine::types::CounterType;

    let db = shared_card_db().expect("fixture database");
    assert_fresh_token_grant(
        db.get_face_by_name("Tetravus").expect("fixture face"),
        |mode| matches!(mode, StaticMode::Other(name) if name == "CantBeEnchanted"),
    );
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::Untap);
    let source = scenario.add_real_card(P0, "Tetravus", Zone::Battlefield, db);
    scenario.with_library_top(P0, &["Plains"]);
    for _ in 0..6 {
        scenario.add_basic_land(P0, ManaColor::White);
    }
    let host = scenario.add_vanilla(P0, 2, 2);
    let ordinary = scenario
        .add_creature(P0, "Ordinary artifact creature", 2, 2)
        .as_artifact_creature()
        .id();
    let aura = scenario.add_real_card(P0, "Pacifism", Zone::Hand, db);
    let crown = scenario
        .add_artifact_from_oracle(
            P0,
            "Crown of the Ages",
            "{4}, {T}: Attach target Aura attached to a creature to another creature.",
        )
        .id();
    let mut runner = scenario.build();
    runner.advance_to_phase(Phase::Upkeep);
    let mut removed = false;
    for _ in 0..40 {
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } if runner.state().phase == Phase::PreCombatMain => break,
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).unwrap();
            }
            WaitingFor::OrderTriggers { player, triggers } => {
                assert_eq!(player, P0);
                assert_eq!(triggers.len(), 2);
                assert!(triggers.iter().all(|trigger| trigger.source_id == source));
                runner
                    .act(GameAction::OrderTriggers {
                        order: (0..triggers.len()).collect(),
                    })
                    .unwrap();
            }
            WaitingFor::OptionalEffectChoice {
                player,
                description,
                ..
            } => {
                assert_eq!(player, P0);
                let accept = description
                    .as_deref()
                    .is_some_and(|text| text.to_lowercase().contains("remove"));
                runner
                    .act(GameAction::DecideOptionalEffect { accept })
                    .unwrap();
            }
            WaitingFor::RemoveCountersChoice {
                player,
                source_id,
                available,
                ..
            } => {
                assert_eq!((player, source_id), (P0, source));
                assert!(available.contains(&(CounterType::Plus1Plus1, 3)));
                runner
                    .act(GameAction::ChooseCountersToRemove {
                        selections: vec![CounterRemoveChoice {
                            counter_type: CounterType::Plus1Plus1,
                            count: 1,
                        }],
                    })
                    .unwrap();
                removed = true;
            }
            other => panic!("unexpected upkeep prompt {other:?}"),
        }
    }
    assert!(removed);
    assert_eq!(runner.state().phase, Phase::PreCombatMain);
    assert_priority(&runner, P0);
    assert_eq!(
        runner.state().objects[&source].counters[&CounterType::Plus1Plus1],
        2
    );
    let token = token_named(&runner, "Tetravite");
    assert_resolved_quote(
        &runner,
        source,
        token,
        StaticMode::Other("CantBeEnchanted".to_string()),
        Duration::Permanent,
    );
    runner.cast(aura).target_object(host).resolve();
    let initial_host = Some(AttachTarget::Object(host));
    assert_eq!(runner.state().objects[&aura].attached_to, initial_host);
    let index = activation(&runner, crown);
    let mut positive = GameRunner::from_state(runner.state().clone());
    positive
        .activate(crown, index)
        .target_objects(&[aura, ordinary])
        .resolve();
    assert_priority(&positive, P0);
    assert_eq!(
        positive.state().objects[&aura].attached_to,
        Some(AttachTarget::Object(ordinary))
    );
    runner
        .activate(crown, index)
        .target_objects(&[aura, token])
        .resolve();
    assert_priority(&runner, P0);
    assert_eq!(runner.state().objects[&aura].attached_to, initial_host);
    assert!(runner.state().objects[&crown].tapped);
}

#[test]
fn firion_copy_pays_two_less_for_equip() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let bearer = scenario.add_creature_from_oracle(P0, "Firion, Wild Rose Warrior", 3, 3, r#"Equipped creatures you control have haste.
Whenever a nontoken Equipment you control enters, create a token that's a copy of it, except it has "This Equipment's equip abilities cost {2} less to activate." Sacrifice that token at the beginning of the next upkeep."#).id();
    for _ in 0..2 {
        scenario.add_basic_land(P0, ManaColor::Red);
    }
    let original = scenario
        .add_artifact_to_hand_from_oracle(
            P0,
            "Vulshok Morningstar",
            r#"Equipped creature gets +2/+2.
Equip {2} ({2}: Attach to target creature you control. Equip only as a sorcery.)"#,
        )
        .with_subtypes(vec!["Equipment"])
        .with_mana_cost(ManaCost::generic(2))
        .id();
    let mut runner = scenario.build();
    runner.cast(original).resolve();
    runner.advance_until_stack_empty();
    assert_priority(&runner, P0);
    let copy = token_named(&runner, "Vulshok Morningstar");
    assert_ne!(copy, original);
    assert!(runner.state().objects[&copy].static_definitions.iter_all().any(|definition| {
        definition.affected == Some(TargetFilter::SelfRef)
            && matches!(&definition.mode, StaticMode::ReduceAbilityCost { keyword, amount: 2, .. }
                if keyword == "equip")
    }));
    let mut unpaid = GameRunner::from_state(runner.state().clone());
    let index = activation(&unpaid, original);
    let rejection = unpaid.act(GameAction::ActivateAbility {
        source_id: original,
        ability_index: index,
        payment_mode: engine::types::game_state::CastPaymentMode::Auto,
    });
    assert!(matches!(
        rejection,
        Err(engine::game::engine::EngineError::ActionNotAllowed(message))
            if message == "Cannot pay mana cost"
    ));
    let index = activation(&runner, copy);
    runner.activate(copy, index).target_object(bearer).resolve();
    assert_priority(&runner, P0);
    let attachment = Some(AttachTarget::Object(bearer));
    assert_eq!(runner.state().objects[&copy].attached_to, attachment);
    assert!(runner.state().objects[&original].attached_to.is_none());

    use engine::types::mana::{ManaType, ManaUnit};
    for _ in 0..2 {
        runner.state_mut().players[usize::from(P0.0)]
            .mana_pool
            .add(ManaUnit::new(
                ManaType::Colorless,
                ObjectId(0),
                false,
                vec![],
            ));
    }
    let index = activation(&runner, original);
    runner
        .activate(original, index)
        .target_object(bearer)
        .resolve();
    assert_eq!(runner.state().objects[&original].attached_to, attachment);
    let player = runner.state().players.iter().find(|p| p.id == P0).unwrap();
    assert_eq!(player.mana_pool.total(), 0);
    assert_priority(&runner, P0);
}

#[test]
fn urban_burgeoning_untaps_only_its_current_land() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P1, &["Forest"]);
    let land = scenario.add_basic_land(P0, ManaColor::Green);
    let other = scenario.add_basic_land(P0, ManaColor::Green);
    scenario.add_basic_land(P0, ManaColor::Green);
    let aura = scenario
        .add_enchantment_to_hand_from_oracle(P0, "Urban Burgeoning", "")
        .with_subtypes(vec!["Aura"])
        .from_oracle_text(
            r#"Enchant land
Enchanted land has "Untap this land during each other player's untap step.""#,
        )
        .with_keyword(engine::types::keywords::Keyword::Enchant(
            TargetFilter::Typed(engine::types::ability::TypedFilter::land()),
        ))
        .with_mana_cost(ManaCost::Cost {
            generic: 0,
            shards: vec![ManaCostShard::Green],
        })
        .id();
    let removal = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Disenchant",
            true,
            "Destroy target artifact or enchantment.",
        )
        .with_mana_cost(ManaCost::generic(1))
        .id();
    let mut runner = scenario.build();
    runner.cast(aura).target_object(land).resolve();
    assert_eq!(
        runner.state().objects[&aura].attached_to,
        Some(AttachTarget::Object(land))
    );
    assert!(runner.state().objects[&aura]
        .base_static_definitions
        .iter()
        .flat_map(|definition| &definition.modifications)
        .any(|modification| matches!(modification,
            ContinuousModification::GrantStaticAbility { definition }
                if definition.affected == Some(TargetFilter::SelfRef)
                    && definition.mode == StaticMode::UntapsDuringEachOtherPlayersUntapStep
        )));
    for remove in [false, true] {
        let mut branch = GameRunner::from_state(runner.state().clone());
        if remove {
            branch.cast(removal).target_object(aura).resolve();
            assert_eq!(branch.state().objects[&aura].zone, Zone::Graveyard);
        }
        for id in [land, other] {
            branch.state_mut().objects.get_mut(&id).unwrap().tapped = true;
        }
        reach_end_step(&mut branch);
        branch.advance_to_phase(Phase::Upkeep);
        assert_eq!(branch.state().active_player, P1);
        assert_eq!(branch.state().objects[&land].tapped, remove);
        assert!(branch.state().objects[&other].tapped);
    }
}

fn select_trigger_target(runner: &mut GameRunner, source: ObjectId, target: ObjectId) {
    // The trigger's companion slot binds "that player controls" to the damaged player.
    let damaged_player = TargetRef::Player(P1);
    if matches!(
        &runner.state().waiting_for,
        WaitingFor::TriggerTargetSelection {
            player: P0,
            source_id: Some(id),
            selection,
            ..
        } if *id == source && selection.current_legal_targets.as_slice() == std::slice::from_ref(&damaged_player)
    ) {
        runner
            .act(GameAction::ChooseTarget {
                target: Some(damaged_player),
            })
            .unwrap();
    }
    let WaitingFor::TriggerTargetSelection {
        player,
        source_id,
        selection,
        ..
    } = &runner.state().waiting_for
    else {
        panic!(
            "expected target selection for {source:?}, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!((*player, *source_id), (P0, Some(source)));
    assert!(selection
        .current_legal_targets
        .contains(&TargetRef::Object(target)));
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(target)),
        })
        .unwrap();
    runner.advance_until_stack_empty();
    assert_priority(runner, P0);
}

#[test]
fn stilt_man_prohibits_the_public_sacrifice_payment_for_both_controllers() {
    use engine::types::game_state::CastPaymentMode;
    const ATOG: &str = "Sacrifice an artifact: This creature gets +2/+2 until end of turn.";
    let db = shared_card_db().expect("fixture database");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P1, &["Island"]);
    for _ in 0..4 {
        scenario.add_basic_land(P1, ManaColor::Blue);
    }
    let source = scenario.add_real_card(P0, "Stilt-Man, Towering Terror", Zone::Battlefield, db);
    let target = scenario
        .add_artifact_from_oracle(P1, "Stolen artifact", "")
        .id();
    let p0_control = scenario
        .add_artifact_from_oracle(P0, "P0 sacrifice control", "")
        .id();
    let p1_control = scenario
        .add_artifact_from_oracle(P1, "P1 sacrifice control", "")
        .id();
    let p0_atog = scenario
        .add_creature_from_oracle(P0, "Atog", 1, 2, ATOG)
        .id();
    let p1_atog = scenario
        .add_creature_from_oracle(P1, "Atog", 1, 2, ATOG)
        .id();
    let reclaim = scenario.add_real_card(P1, "Steal Artifact", Zone::Hand, db);
    let mut runner = scenario.build();
    runner.advance_to_phase(Phase::BeginCombat);
    runner
        .declare_attackers(&[(source, AttackTarget::Player(P1))])
        .unwrap();
    reach_blocks(&mut runner, P1);
    runner.declare_blockers(&[]).unwrap();
    for _ in 0..16 {
        if matches!(
            runner.state().waiting_for,
            WaitingFor::TriggerTargetSelection { .. }
        ) {
            break;
        }
        runner.act(GameAction::PassPriority).unwrap();
    }
    select_trigger_target(&mut runner, source, target);
    assert_eq!(runner.state().objects[&target].controller, P0);

    let pay = |runner: &mut GameRunner, player, atog, control| {
        let index = activation(runner, atog);
        runner
            .act(GameAction::ActivateAbility {
                source_id: atog,
                ability_index: index,
                payment_mode: CastPaymentMode::Auto,
            })
            .unwrap();
        let WaitingFor::PayCost {
            player: payer,
            choices,
            count,
            ..
        } = &runner.state().waiting_for
        else {
            panic!(
                "expected sacrifice prompt, got {:?}",
                runner.state().waiting_for
            );
        };
        assert_eq!((*payer, *count), (player, 1));
        assert!(choices.contains(&control));
        assert!(!choices.contains(&target));
        runner
            .act(GameAction::SelectCards {
                cards: vec![control],
            })
            .unwrap();
        runner.advance_until_stack_empty();
        assert_priority(runner, player);
        assert_eq!(runner.state().objects[&control].zone, Zone::Graveyard);
        assert_eq!(runner.state().objects[&target].zone, Zone::Battlefield);
    };
    pay(&mut runner, P0, p0_atog, p0_control);
    reach_end_step(&mut runner);
    runner.advance_to_phase(Phase::PreCombatMain);
    assert_priority(&runner, P1);
    runner.cast(reclaim).target_object(target).resolve();
    assert_eq!(
        runner.state().objects[&reclaim].attached_to,
        Some(AttachTarget::Object(target))
    );
    assert_eq!(runner.state().objects[&target].controller, P1);
    pay(&mut runner, P1, p1_atog, p1_control);
}

#[test]
fn ogre_marauder_attributes_unblockability_only_when_payment_is_declined() {
    use engine::game::combat::validate_blockers_for_player;
    use engine::game::derived_views::derive_views;

    let db = shared_card_db().expect("fixture database");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P1, &["Island"]);
    let source = scenario.add_real_card(P0, "Ogre Marauder", Zone::Battlefield, db);
    let blocker = scenario.add_vanilla(P1, 2, 2);
    let payment = scenario.add_vanilla(P1, 1, 1);
    let mut runner = scenario.build();
    runner.advance_to_phase(Phase::BeginCombat);
    runner
        .declare_attackers(&[(source, AttackTarget::Player(P1))])
        .unwrap();
    runner.advance_until_stack_empty();
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::UnlessPayment { player: P1, .. }
    ));
    let mut paid = GameRunner::from_state(runner.state().clone());
    paid.act(GameAction::PayUnlessCost { pay: true }).unwrap();
    let WaitingFor::WardSacrificeChoice {
        player, permanents, ..
    } = &paid.state().waiting_for
    else {
        panic!("expected sacrifice choice");
    };
    assert_eq!(*player, P1);
    assert!(permanents.contains(&payment));
    paid.act(GameAction::SelectCards {
        cards: vec![payment],
    })
    .unwrap();
    paid.advance_until_stack_empty();
    assert_priority(&paid, P0);
    assert_eq!(paid.state().objects[&payment].zone, Zone::Graveyard);
    assert!(!derive_views(paid.state(), None)
        .temporary_cant_be_blocked
        .contains_key(&source));
    assert!(validate_blockers_for_player(paid.state(), P1, &[(blocker, source)]).is_ok());

    runner
        .act(GameAction::PayUnlessCost { pay: false })
        .unwrap();
    runner.advance_until_stack_empty();
    assert_priority(&runner, P0);
    assert_eq!(runner.state().objects[&payment].zone, Zone::Battlefield);
    assert_eq!(
        derive_views(runner.state(), None)
            .temporary_cant_be_blocked
            .get(&source),
        Some(&Some(source))
    );
    assert!(validate_blockers_for_player(runner.state(), P1, &[(blocker, source)]).is_err());
    reach_blocks(&mut runner, P1);
    runner.declare_blockers(&[]).unwrap();
    reach_end_step(&mut runner);
    runner.advance_to_phase(Phase::PreCombatMain);
    assert_priority(&runner, P1);
    assert!(!derive_views(runner.state(), None)
        .temporary_cant_be_blocked
        .contains_key(&source));
}

const SURGE_ENGINE: &str = r#"Defender
{U}: This creature loses defender and gains "This creature can't be blocked."
{2}{U}: This creature becomes blue and has base power and toughness 5/4. Activate only if this creature doesn't have defender.
{4}{U}{U}: Draw three cards. Activate only if this creature is blue and only once."#;

const VRONOS: &str = r#"[+1]: Up to two other target planeswalkers you control phase out at the beginning of the next end step. (Treat them and anything attached to them as though they don't exist until your next turn.)
[−2]: For each opponent, return up to one target nonland permanent that player controls to its owner's hand.
[−7]: Target artifact you control becomes a 9/9 Construct artifact creature and gains vigilance, indestructible, and "This creature can't be blocked.""#;

const WILSON: &str = r#"Reach, trample
Ward {2}
Wilson, Subtle Bear can't be blocked.
{1}{G}{U}, Exile Wilson from your graveyard: Target creature you control perpetually gains "This creature can't be blocked." Activate only as a sorcery."#;

const CARNAGE: &str = r#"Trample
When Carnage enters, return target creature card with mana value 3 or less from your graveyard to the battlefield. It gains "This creature attacks each combat if able" and "When this creature deals combat damage to a player, sacrifice it."
Mayhem {B}{R}"#;

const FULL_STEAM_AHEAD: &str = r#"Until end of turn, each creature you control gets +2/+2 and gains trample and "This creature can't be blocked by more than one creature.""#;

const ETHEREAL_GRASP: &str = r#"Tap target creature. That creature perpetually gains "This creature doesn't untap during your untap step" and "{8}: Untap this creature.""#;

fn assert_resolved_quote(
    runner: &GameRunner,
    source: ObjectId,
    recipient: ObjectId,
    mode: StaticMode,
    duration: Duration,
) {
    assert!(
        runner
            .state()
            .transient_continuous_effects
            .iter()
            .any(|effect| {
                effect.source_id == source && effect.duration == duration
                    && effect.affected == TargetFilter::SpecificObject { id: recipient }
            && effect.modifications.iter().any(|modification| matches!(
                modification,
                ContinuousModification::GrantStaticAbility { definition }
                    if definition.affected == Some(TargetFilter::SelfRef) && definition.mode == mode
            ))
            }),
        "resolved {mode:?} quote from {source:?}"
    );
}

#[test]
fn full_steam_ahead_limits_the_number_of_blockers_until_cleanup() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Forest"]);
    scenario.with_library_top(P1, &["Forest"]);
    for _ in 0..5 {
        scenario.add_basic_land(P0, ManaColor::Green);
    }
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Full Steam Ahead", false, FULL_STEAM_AHEAD)
        .with_mana_cost(ManaCost::Cost {
            generic: 3,
            shards: vec![ManaCostShard::Green, ManaCostShard::Green],
        })
        .id();
    let attacker = scenario.add_creature(P0, "Attacker", 2, 2).id();
    let first = scenario.add_creature(P1, "First blocker", 2, 2).id();
    let second = scenario.add_creature(P1, "Second blocker", 2, 2).id();
    let mut runner = scenario.build();
    let mut control = GameRunner::from_state(runner.state().clone());
    runner.cast(spell).resolve();
    assert_eq!(runner.state().objects[&spell].zone, Zone::Graveyard);
    assert_resolved_quote(
        &runner,
        spell,
        attacker,
        StaticMode::CantBeBlockedByMoreThan { max: 1 },
        Duration::UntilEndOfTurn,
    );
    let mut expired = GameRunner::from_state(runner.state().clone());
    reach_end_step(&mut expired);
    expired.advance_to_phase(Phase::PreCombatMain);
    reach_end_step(&mut expired);
    expired.advance_to_phase(Phase::PreCombatMain);
    assert_priority(&expired, P0);
    declare_attack_and_reach_blocks(&mut expired, attacker);
    expired
        .declare_blockers(&[(first, attacker), (second, attacker)])
        .unwrap();

    declare_attack_and_reach_blocks(&mut runner, attacker);
    declare_attack_and_reach_blocks(&mut control, attacker);
    let double = [(first, attacker), (second, attacker)];
    assert!(runner.declare_blockers(&double).is_err());
    runner.declare_blockers(&[(first, attacker)]).unwrap();
    control.declare_blockers(&double).unwrap();
}

#[test]
fn surge_engine_grants_same_turn_evasion_and_removes_defender() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_basic_land(P0, ManaColor::Blue);
    let surge = scenario
        .add_creature_from_oracle(P0, "Surge Engine", 3, 2, SURGE_ENGINE)
        .as_artifact_creature()
        .id();
    let other = scenario.add_creature(P0, "Other attacker", 2, 2).id();
    let blocker = scenario.add_creature(P1, "Blocker", 4, 4).id();
    let mut runner = scenario.build();
    assert!(runner.state().objects[&surge]
        .keywords
        .contains(&engine::types::keywords::Keyword::Defender));
    let index = activation(&runner, surge);
    runner.activate(surge, index).resolve();
    assert_resolved_quote(
        &runner,
        surge,
        surge,
        StaticMode::CantBeBlocked,
        Duration::UntilEndOfTurn,
    );
    assert!(!runner.state().objects[&surge]
        .keywords
        .contains(&engine::types::keywords::Keyword::Defender));
    runner.advance_to_phase(Phase::BeginCombat);
    runner
        .declare_attackers(&[
            (surge, AttackTarget::Player(P1)),
            (other, AttackTarget::Player(P1)),
        ])
        .unwrap();
    reach_blocks(&mut runner, P1);
    assert!(runner.declare_blockers(&[(blocker, surge)]).is_err());
    runner.declare_blockers(&[(blocker, other)]).unwrap();
}

#[test]
fn vronos_keeps_the_animation_and_evasion_on_the_chosen_artifact() {
    use engine::types::{card_type::CoreType, keywords::Keyword};
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let vronos = scenario
        .add_planeswalker_from_oracle(P0, "Vronos, Masked Inquisitor", "Vronos", 7, VRONOS)
        .id();
    let chosen = scenario
        .add_artifact_from_oracle(P0, "Chosen artifact", "")
        .id();
    let other = scenario
        .add_creature(P0, "Other artifact", 2, 2)
        .as_artifact_creature()
        .id();
    let blocker = scenario.add_creature(P1, "Blocker", 4, 4).id();
    let mut runner = scenario.build();
    let (index, _) = activated_ability_definitions(runner.state(), vronos)
        .into_iter()
        .last()
        .expect("Vronos ultimate");
    runner
        .activate(vronos, index)
        .target_object(chosen)
        .resolve();
    assert_resolved_quote(
        &runner,
        vronos,
        chosen,
        StaticMode::CantBeBlocked,
        Duration::Permanent,
    );
    let recipient = &runner.state().objects[&chosen];
    assert_eq!((recipient.power, recipient.toughness), (Some(9), Some(9)));
    assert!(recipient
        .card_types
        .core_types
        .contains(&CoreType::Creature));
    assert!(recipient
        .card_types
        .core_types
        .contains(&CoreType::Artifact));
    assert!(recipient
        .card_types
        .subtypes
        .iter()
        .any(|subtype| subtype == "Construct"));
    assert!(recipient.keywords.contains(&Keyword::Vigilance));
    assert!(recipient.keywords.contains(&Keyword::Indestructible));
    runner.advance_to_phase(Phase::BeginCombat);
    runner
        .declare_attackers(&[
            (chosen, AttackTarget::Player(P1)),
            (other, AttackTarget::Player(P1)),
        ])
        .unwrap();
    assert!(!runner.state().objects[&chosen].tapped);
    reach_blocks(&mut runner, P1);
    assert!(runner.declare_blockers(&[(blocker, chosen)]).is_err());
    runner.declare_blockers(&[(blocker, other)]).unwrap();
}

#[test]
fn wilson_grants_evasion_after_mana_and_graveyard_exile_payment() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    for color in [ManaColor::Green, ManaColor::Blue, ManaColor::Green] {
        scenario.add_basic_land(P0, color);
    }
    scenario.with_library_top(P0, &["Island", "Island"]);
    scenario.with_library_top(P1, &["Island", "Island"]);
    let wilson = scenario
        .add_creature_to_graveyard(P0, "Wilson, Subtle Bear", 4, 4)
        .as_legendary()
        .with_subtypes(vec!["Bear", "Warrior"])
        .from_oracle_text(WILSON)
        .id();
    let chosen = scenario.add_creature(P0, "Chosen creature", 2, 2).id();
    let other = scenario.add_creature(P0, "Other creature", 2, 2).id();
    let blocker = scenario.add_creature(P1, "Blocker", 3, 3).id();
    let mut runner = scenario.build();
    let index = activation(&runner, wilson);
    runner
        .activate(wilson, index)
        .target_object(chosen)
        .pay_with(&[wilson])
        .resolve();
    assert_eq!(runner.state().objects[&wilson].zone, Zone::Exile);
    assert_resolved_quote(
        &runner,
        wilson,
        chosen,
        StaticMode::CantBeBlocked,
        Duration::UntilEndOfTurn,
    );
    let mut later = GameRunner::from_state(runner.state().clone());
    for _ in 0..2 {
        reach_end_step(&mut later);
        later.advance_to_phase(Phase::PreCombatMain);
    }
    assert_priority(&later, P0);
    declare_attack_and_reach_blocks(&mut later, chosen);
    later.declare_blockers(&[(blocker, chosen)]).unwrap();

    runner.advance_to_phase(Phase::BeginCombat);
    runner
        .declare_attackers(&[
            (chosen, AttackTarget::Player(P1)),
            (other, AttackTarget::Player(P1)),
        ])
        .unwrap();
    reach_blocks(&mut runner, P1);
    assert!(runner.declare_blockers(&[(blocker, chosen)]).is_err());
    runner.declare_blockers(&[(blocker, other)]).unwrap();
}

#[test]
fn ethereal_grasp_installs_both_perpetual_grants_through_later_untap() {
    use engine::types::ability::AbilityCost;
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    for _ in 0..9 {
        scenario.add_basic_land(P0, ManaColor::Blue);
    }
    scenario.with_library_top(P0, &["Island", "Island"]);
    scenario.with_library_top(P1, &["Island", "Island"]);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Ethereal Grasp", true, ETHEREAL_GRASP)
        .with_mana_cost(ManaCost::Cost {
            generic: 2,
            shards: vec![ManaCostShard::Blue],
        })
        .id();
    let target = scenario.add_creature(P0, "Grasped creature", 2, 2).id();
    let mut runner = scenario.build();
    runner.cast(spell).target_object(target).resolve();
    assert_eq!(runner.state().objects[&spell].zone, Zone::Graveyard);
    assert!(runner.state().objects[&target].tapped);
    for _ in 0..2 {
        runner.advance_to_end_step();
        runner.advance_to_phase(Phase::PreCombatMain);
    }
    assert_priority(&runner, P0);
    assert!(runner.state().objects[&target].tapped);
    let (index, _) = activated_ability_definitions(runner.state(), target).into_iter()
        .find(|(_, ability)| matches!(&ability.cost, Some(AbilityCost::Mana { cost }) if *cost == ManaCost::generic(8)))
        .expect("the perpetual untap activation survived into the next turn");
    runner.activate(target, index).resolve();
    assert!(!runner.state().objects[&target].tapped);
}

#[test]
fn carnage_grants_both_the_attack_requirement_and_the_damage_trigger() {
    use engine::types::keywords::Keyword;
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P1, &["Mountain"]);
    for color in [
        ManaColor::Black,
        ManaColor::Red,
        ManaColor::Black,
        ManaColor::Red,
    ] {
        scenario.add_basic_land(P0, color);
    }
    let carnage = scenario
        .add_creature_to_hand_from_oracle(P0, "Carnage, Crimson Chaos", 4, 3, CARNAGE)
        .with_mana_cost(ManaCost::Cost {
            generic: 2,
            shards: vec![ManaCostShard::Black, ManaCostShard::Red],
        })
        .id();
    let returned = scenario.add_creature_to_graveyard(P0, "Goblin Guide", 2, 2)
        .with_mana_cost(ManaCost::Cost { generic: 0, shards: vec![ManaCostShard::Red] })
        .from_oracle_text(r#"Haste
Whenever this creature attacks, defending player reveals the top card of their library. If it's a land card, that player puts it into their hand."#)
        .with_keyword(Keyword::Haste)
        .id();
    let other = scenario.add_creature(P0, "Other attacker", 1, 1).id();
    let mut runner = scenario.build();
    runner.cast(carnage).target_object(returned).resolve();
    assert_eq!(runner.state().objects[&carnage].zone, Zone::Battlefield);
    assert_eq!(runner.state().objects[&returned].zone, Zone::Battlefield);
    assert_priority(&runner, P0);
    assert_resolved_quote(
        &runner,
        returned,
        returned,
        StaticMode::MustAttack,
        Duration::UntilEndOfTurn,
    );
    assert!(runner
        .state()
        .transient_continuous_effects
        .iter()
        .any(|effect| {
            effect.source_id == returned
                && effect.affected == TargetFilter::SpecificObject { id: returned }
                && effect.modifications.iter().any(|modification| {
                    matches!(modification, ContinuousModification::GrantTrigger { .. })
                })
        }));
    runner.advance_to_combat();
    assert!(runner
        .declare_attackers(&[(other, AttackTarget::Player(P1))])
        .is_err());
    runner
        .declare_attackers(&[(returned, AttackTarget::Player(P1))])
        .unwrap();
    runner.advance_until_stack_empty();
    reach_blocks(&mut runner, P1);
    runner.declare_blockers(&[]).unwrap();
    assert_priority(&runner, P0);
    assert_eq!(runner.combat_damage().life_delta(P1), -2);
    assert_eq!(runner.state().objects[&returned].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&other].zone, Zone::Battlefield);
}

#[test]
fn call_for_aid_keeps_its_actor_bound_sacrifice_prohibition() {
    use engine::game::casting::find_eligible_sacrifice_targets;
    use engine::types::ability::TypedFilter;
    let db = shared_card_db().expect("fixture database");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    for _ in 0..5 {
        scenario.add_basic_land(P0, ManaColor::Red);
    }
    let aid = scenario.add_real_card(P0, "Call for Aid", Zone::Hand, db);
    let stolen = scenario.add_creature(P1, "Stolen creature", 2, 2).id();
    let control = scenario
        .add_creature(P0, "Unrestricted creature", 2, 2)
        .id();
    let mut runner = scenario.build();
    runner.cast(aid).target_player(P1).resolve();
    assert_eq!(runner.state().objects[&aid].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&stolen].controller, P0);
    assert!(runner.state().transient_continuous_effects.iter().any(|effect| {
        effect.source_id == aid
            && effect.affected == TargetFilter::SpecificObject { id: stolen }
            && effect.bound_actor == Some(P0)
            && effect.modifications.iter().any(|modification| matches!(
                modification,
                ContinuousModification::AddStaticMode { mode: StaticMode::Other(name) } if name == "CantBeSacrificed"
            ))
    }));
    let eligible = find_eligible_sacrifice_targets(
        runner.state(),
        P0,
        aid,
        None,
        &TargetFilter::Typed(TypedFilter::creature()),
    );
    assert!(eligible.contains(&control));
    assert!(!eligible.contains(&stolen));
}

const CRATEROUS_STOMP: &str = r#"Craterous Stomp deals 3 damage to target creature an opponent controls. Each other creature that player controls becomes a Coward in addition to its other types and gains "This creature can't block Giants or Warriors.""#;

fn craterous_stomp_from_oracle(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_spell_to_hand_from_oracle(P0, "Craterous Stomp", false, CRATEROUS_STOMP)
        .with_mana_cost(ManaCost::Cost {
            generic: 1,
            shards: vec![ManaCostShard::Red],
        })
        .id()
}

fn add_lands(scenario: &mut GameScenario, player: PlayerId, colors: &[ManaColor]) {
    for &color in colors {
        scenario.add_basic_land(player, color);
    }
}

fn resolve_dress_down(runner: &mut GameRunner, spell: ObjectId) {
    let active = runner.state().active_player;
    runner.cast(spell).resolve();
    assert_priority(runner, active);
    assert_eq!(runner.state().objects[&spell].zone, Zone::Battlefield);
}

fn live_statics(
    runner: &GameRunner,
    recipient: ObjectId,
    mode: StaticMode,
) -> impl Iterator<Item = &StaticDefinition> {
    runner.state().objects[&recipient]
        .static_definitions
        .iter_all()
        .filter(move |definition| definition.mode == mode)
}

#[test]
fn panic_still_prevents_blocking_after_dress_down() {
    use ManaColor::{Blue, Red};

    let db = shared_card_db().expect("fixture database");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Island"]);
    add_lands(&mut scenario, P0, &[Red, Blue, Blue]);
    let panic = scenario.add_real_card(P0, "Panic", Zone::Hand, db);
    let dress = scenario.add_real_card(P0, "Dress Down", Zone::Hand, db);
    let attacker = scenario.add_vanilla(P0, 2, 2);
    let blocked = scenario.add_vanilla(P1, 2, 2);
    let other = scenario.add_vanilla(P1, 2, 2);
    let mut runner = scenario.build();
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(attacker, AttackTarget::Player(P1))])
        .unwrap();
    assert_priority(&runner, P0);

    let mut control = GameRunner::from_state(runner.state().clone());
    reach_blocks(&mut control, P1);
    control.declare_blockers(&[(blocked, attacker)]).unwrap();

    runner
        .cast(panic)
        .target_object(blocked)
        .resolve()
        .assert_zone(&[panic], Zone::Graveyard);
    assert_priority(&runner, P0);
    resolve_dress_down(&mut runner, dress);
    reach_blocks(&mut runner, P1);
    // CR 509.1b + CR 611.2c: Panic's restriction still applies to the first blocker.
    assert!(runner.declare_blockers(&[(blocked, attacker)]).is_err());
    runner.declare_blockers(&[(other, attacker)]).unwrap();
}

#[test]
fn venser_still_prevents_blocks_on_both_attackers_after_dress_down() {
    use engine::types::CounterType;
    use ManaColor::Blue;

    const VENSER_ORACLE: &str = r#"[+2]: Exile target permanent you own. Return it to the battlefield under your control at the beginning of the next end step.
[−1]: Creatures can't be blocked this turn.
[−8]: You get an emblem with "Whenever you cast a spell, exile target permanent.""#;

    let db = shared_card_db().expect("fixture database");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Island"]);
    add_lands(&mut scenario, P0, &[Blue, Blue]);
    let venser = scenario
        .add_planeswalker_from_oracle(P0, "Venser, the Sojourner", "Venser", 3, VENSER_ORACLE)
        .id();
    let dress = scenario.add_real_card(P0, "Dress Down", Zone::Hand, db);
    let first = scenario.add_vanilla(P0, 2, 2);
    let second = scenario.add_vanilla(P0, 2, 2);
    let blocker = scenario.add_vanilla(P1, 2, 2);
    let attacks = [
        (first, AttackTarget::Player(P1)),
        (second, AttackTarget::Player(P1)),
    ];
    let mut runner = scenario.build();

    let mut control = GameRunner::from_state(runner.state().clone());
    resolve_dress_down(&mut control, dress);
    control.advance_to_combat();
    control.declare_attackers(&attacks).unwrap();
    reach_blocks(&mut control, P1);
    control.declare_blockers(&[(blocker, first)]).unwrap();

    runner.activate(venser, 1).resolve();
    assert_priority(&runner, P0);
    assert_eq!(
        runner.state().objects[&venser].counters[&CounterType::Loyalty],
        2
    );
    resolve_dress_down(&mut runner, dress);

    // CR 611.2 + CR 611.2c + CR 613.1f: Venser's resolved rule effect survives
    // Dress Down's layer 6 ability removal on each creature.
    for attacker in [first, second] {
        let mut restrictions = live_statics(&runner, attacker, StaticMode::CantBeBlocked);
        assert!(restrictions
            .next()
            .is_some_and(StaticDefinition::is_resolution_installed));
        assert!(restrictions.next().is_none());
    }
    runner.advance_to_combat();
    runner.declare_attackers(&attacks).unwrap();
    reach_blocks(&mut runner, P1);
    assert!(runner.declare_blockers(&[(blocker, first)]).is_err());
    assert!(runner.declare_blockers(&[(blocker, second)]).is_err());
    runner.declare_blockers(&[]).unwrap();
}

#[test]
fn artful_dodge_still_prevents_blocks_after_turn_to_frog() {
    use engine::types::keywords::Keyword;
    use ManaColor::Blue;

    let db = shared_card_db().expect("fixture database");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    add_lands(&mut scenario, P0, &[Blue, Blue, Blue]);
    let dodge = scenario
        .add_spell_to_hand_from_oracle(P0, "Artful Dodge", false, ARTFUL_DODGE)
        .with_mana_cost(ManaCost::Cost {
            generic: 0,
            shards: vec![ManaCostShard::Blue],
        })
        .id();
    let frog = scenario.add_real_card(P0, "Turn to Frog", Zone::Hand, db);
    let attacker = scenario
        .add_creature(P0, "Attacker", 3, 3)
        .with_keyword(Keyword::Haste)
        .id();
    let blocker = scenario.add_vanilla(P1, 2, 2);
    let mut runner = scenario.build();
    let mut control = GameRunner::from_state(runner.state().clone());
    control.cast(frog).target_object(attacker).resolve();
    assert_priority(&control, P0);
    declare_attack_and_reach_blocks(&mut control, attacker);
    control.declare_blockers(&[(blocker, attacker)]).unwrap();

    runner.cast(dodge).target_object(attacker).resolve();
    assert_priority(&runner, P0);
    assert_eq!(
        live_statics(&runner, attacker, StaticMode::CantBeBlocked).count(),
        1
    );
    runner
        .cast(frog)
        .target_object(attacker)
        .resolve()
        .assert_zone(&[frog], Zone::Graveyard);
    assert_priority(&runner, P0);
    let creature = &runner.state().objects[&attacker];
    // CR 613.4b: Turn to Frog sets the creature's base P/T to 1/1.
    assert_eq!((creature.power, creature.toughness), (Some(1), Some(1)));
    assert_eq!(creature.color, [Blue]);
    assert_eq!(creature.card_types.subtypes, ["Frog"]);
    // CR 613.1f + CR 611.2c: The creature loses its abilities and keeps the rule effect.
    assert!(creature.keywords.is_empty());
    assert!(creature.abilities.is_empty());
    assert!(creature.trigger_definitions.is_empty());
    assert!(creature.replacement_definitions.is_empty());
    assert_eq!(
        live_statics(&runner, attacker, StaticMode::CantBeBlocked).count(),
        1
    );
    assert!(creature
        .static_definitions
        .iter_all()
        .all(StaticDefinition::is_resolution_installed));
    declare_attack_and_reach_blocks(&mut runner, attacker);
    assert!(runner.declare_blockers(&[(blocker, attacker)]).is_err());
    runner.declare_blockers(&[]).unwrap();
}

#[test]
fn goblin_tunneler_restriction_survives_humility() {
    use ManaColor::White;

    let db = shared_card_db().expect("fixture database");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    add_lands(&mut scenario, P0, &[White; 4]);
    let humility = scenario.add_real_card(P0, "Humility", Zone::Hand, db);
    let tunneler = scenario
        .add_creature_from_oracle(
            P0,
            "Goblin Tunneler",
            1,
            1,
            "{T}: Target creature with power 2 or less can't be blocked this turn.",
        )
        .id();
    let attacker = scenario.add_vanilla(P0, 2, 2);
    let blocker = scenario.add_vanilla(P1, 2, 2);
    let mut runner = scenario.build();
    let mut control = GameRunner::from_state(runner.state().clone());
    control
        .cast(humility)
        .resolve()
        .assert_zone(&[humility], Zone::Battlefield);
    assert_priority(&control, P0);
    declare_attack_and_reach_blocks(&mut control, attacker);
    control.declare_blockers(&[(blocker, attacker)]).unwrap();

    runner
        .activate(tunneler, activation(&runner, tunneler))
        .target_object(attacker)
        .resolve();
    assert_priority(&runner, P0);
    assert!(runner.state().objects[&tunneler].tapped);
    assert_eq!(
        live_statics(&runner, attacker, StaticMode::CantBeBlocked).count(),
        1
    );
    runner
        .cast(humility)
        .resolve()
        .assert_zone(&[humility], Zone::Battlefield);
    assert_priority(&runner, P0);
    declare_attack_and_reach_blocks(&mut runner, attacker);
    // CR 611.2 + CR 611.2c: An activated ability's resolution can also install a rule effect.
    assert!(runner.declare_blockers(&[(blocker, attacker)]).is_err());
    runner.declare_blockers(&[]).unwrap();
}

#[test]
fn parsed_craterous_stomp_quote_is_removed_by_dress_down() {
    use ManaColor::{Blue, Red};

    let db = shared_card_db().expect("fixture database");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Island"]);
    add_lands(&mut scenario, P0, &[Red, Red, Blue, Blue]);
    let stomp = craterous_stomp_from_oracle(&mut scenario);
    let dress = scenario.add_real_card(P0, "Dress Down", Zone::Hand, db);
    let giant = scenario
        .add_creature(P0, "Giant attacker", 3, 3)
        .with_subtypes(vec!["Giant"])
        .id();
    let other = scenario.add_vanilla(P0, 2, 2);
    let victim = scenario.add_vanilla(P1, 2, 2);
    let coward = scenario.add_vanilla(P1, 2, 2);
    let mut runner = scenario.build();
    runner
        .cast(stomp)
        .target_object(victim)
        .resolve()
        .assert_zone(&[victim], Zone::Graveyard);
    assert_priority(&runner, P0);

    let mut before = GameRunner::from_state(runner.state().clone());
    before.advance_to_combat();
    before
        .declare_attackers(&[
            (giant, AttackTarget::Player(P1)),
            (other, AttackTarget::Player(P1)),
        ])
        .unwrap();
    reach_blocks(&mut before, P1);
    assert!(before.declare_blockers(&[(coward, giant)]).is_err());
    before.declare_blockers(&[(coward, other)]).unwrap();

    resolve_dress_down(&mut runner, dress);
    assert!(runner.state().objects[&coward]
        .card_types
        .subtypes
        .iter()
        .any(|subtype| subtype == "Coward"));
    declare_attack_and_reach_blocks(&mut runner, giant);
    // CR 113.10 + CR 613.1f: The earlier quote is an ability and is removed.
    runner.declare_blockers(&[(coward, giant)]).unwrap();
}

#[test]
fn incite_hysteria_grants_its_quote_after_dress_down_until_cleanup() {
    use ManaColor::{Blue, Red};

    let db = shared_card_db().expect("fixture database");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Island", "Island"]);
    scenario.with_library_top(P1, &["Island"]);
    add_lands(&mut scenario, P0, &[Red, Red, Red, Blue, Blue]);
    let dress = scenario.add_real_card(P0, "Dress Down", Zone::Hand, db);
    let incite = scenario
        .add_spell_to_hand_from_oracle(P0, "Incite Hysteria", false, INCITE)
        .with_mana_cost(ManaCost::Cost {
            generic: 2,
            shards: vec![ManaCostShard::Red],
        })
        .id();
    let attacker = scenario.add_vanilla(P0, 1, 1);
    let blocker = scenario
        .add_creature(P1, "Red blocker", 2, 2)
        .with_color(vec![Red])
        .id();
    let mut runner = scenario.build();
    resolve_dress_down(&mut runner, dress);

    let mut control = GameRunner::from_state(runner.state().clone());
    declare_attack_and_reach_blocks(&mut control, attacker);
    control.declare_blockers(&[(blocker, attacker)]).unwrap();

    runner
        .cast(incite)
        .target_object(blocker)
        .resolve()
        .assert_zone(&[incite], Zone::Graveyard);
    assert_priority(&runner, P0);
    declare_attack_and_reach_blocks(&mut runner, attacker);
    // CR 613.7: The more recent effect grants the ability after removal applies.
    assert!(runner.declare_blockers(&[(blocker, attacker)]).is_err());
    runner.declare_blockers(&[]).unwrap();
    for _ in 0..2 {
        reach_end_step(&mut runner);
        runner.advance_to_phase(Phase::PreCombatMain);
    }
    assert_priority(&runner, P0);
    assert_eq!(runner.state().objects[&dress].zone, Zone::Graveyard);
    declare_attack_and_reach_blocks(&mut runner, attacker);
    runner.declare_blockers(&[(blocker, attacker)]).unwrap();
}

#[test]
fn petroglyphs_counts_panic_target_but_excludes_a_quoted_ability() {
    use engine::game::layers::evaluate_layers;
    use ManaColor::Red;

    let db = shared_card_db().expect("fixture database");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    add_lands(&mut scenario, P0, &[Red]);
    scenario.add_real_card(P0, "Muraganda Petroglyphs", Zone::Battlefield, db);
    let panic = scenario.add_real_card(P0, "Panic", Zone::Hand, db);
    let giant = scenario
        .add_creature(P0, "Giant attacker", 3, 3)
        .with_subtypes(vec!["Giant"])
        .id();
    let bears = scenario.add_real_card(P1, "Grizzly Bears", Zone::Battlefield, db);
    let control = scenario.add_vanilla(P1, 2, 2);
    let mut runner = scenario.build();
    evaluate_layers(runner.state_mut());
    let size = |runner: &GameRunner, id: ObjectId| {
        let creature = &runner.state().objects[&id];
        (creature.power, creature.toughness)
    };
    assert_eq!(size(&runner, control), (Some(4), Some(4)));
    assert_eq!(size(&runner, bears), (Some(4), Some(4)));
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(giant, AttackTarget::Player(P1))])
        .unwrap();
    assert_priority(&runner, P0);

    let mut baseline = GameRunner::from_state(runner.state().clone());
    reach_blocks(&mut baseline, P1);
    baseline.declare_blockers(&[(bears, giant)]).unwrap();

    runner
        .cast(panic)
        .target_object(bears)
        .resolve()
        .assert_zone(&[panic], Zone::Graveyard);
    assert_priority(&runner, P0);
    // CR 109.3 + CR 611.2c: A rule effect does not disqualify the Bears.
    assert_eq!(size(&runner, bears), (Some(4), Some(4)));
    reach_blocks(&mut runner, P1);
    assert!(runner.declare_blockers(&[(bears, giant)]).is_err());
    runner.declare_blockers(&[(control, giant)]).unwrap();

    // Set up a separate main-phase branch for the sorcery-speed quoted grant.
    let mut quote = GameScenario::new();
    quote.at_phase(Phase::PreCombatMain);
    add_lands(&mut quote, P0, &[Red; 2]);
    quote.add_real_card(P0, "Muraganda Petroglyphs", Zone::Battlefield, db);
    let stomp = craterous_stomp_from_oracle(&mut quote);
    let giant = quote
        .add_creature(P0, "Giant attacker", 3, 3)
        .with_subtypes(vec!["Giant"])
        .id();
    let victim = quote.add_vanilla(P1, 1, 1);
    let survivor = quote.add_real_card(P1, "Grizzly Bears", Zone::Battlefield, db);
    let mut quote = quote.build();
    evaluate_layers(quote.state_mut());
    assert_eq!(size(&quote, survivor), (Some(4), Some(4)));
    quote
        .cast(stomp)
        .target_object(victim)
        .resolve()
        .assert_zone(&[victim], Zone::Graveyard);
    assert_priority(&quote, P0);
    // CR 113.10: The quoted ability disqualifies the surviving creature.
    assert_eq!(size(&quote, survivor), (Some(2), Some(2)));
    declare_attack_and_reach_blocks(&mut quote, giant);
    assert!(quote.declare_blockers(&[(survivor, giant)]).is_err());
    quote.declare_blockers(&[]).unwrap();
}

#[test]
fn artful_dodge_graft_does_not_deduplicate_against_psychic_paper() {
    use engine::types::ability::{ChoiceType, DefinitionOrigin};
    use ManaColor::Blue;

    let db = shared_card_db().expect("fixture database");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Island"]);
    add_lands(&mut scenario, P0, &[Blue; 5]);
    let paper = scenario.add_real_card(P0, "Psychic Paper", Zone::Battlefield, db);
    let dodge = scenario
        .add_spell_to_hand_from_oracle(P0, "Artful Dodge", false, ARTFUL_DODGE)
        .with_mana_cost(ManaCost::Cost {
            generic: 0,
            shards: vec![ManaCostShard::Blue],
        })
        .id();
    let dress = scenario.add_real_card(P0, "Dress Down", Zone::Hand, db);
    let attacker = scenario.add_vanilla(P0, 2, 2);
    let blocker = scenario.add_vanilla(P1, 2, 2);
    let mut runner = scenario.build();
    let mut control = GameRunner::from_state(runner.state().clone());
    declare_attack_and_reach_blocks(&mut control, attacker);
    control.declare_blockers(&[(blocker, attacker)]).unwrap();

    runner.state_mut().all_card_names = vec!["Grizzly Bears".to_string()].into();
    runner
        .activate(paper, activation(&runner, paper))
        .target_object(attacker)
        .resolve();
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::NamedChoice {
            player: P0,
            choice_type: ChoiceType::CardName { .. },
            ..
        }
    ));
    runner
        .act(GameAction::ChooseOption {
            choice: "Grizzly Bears".to_string(),
        })
        .unwrap();
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::NamedChoice {
            player: P0,
            choice_type: ChoiceType::CreatureType { .. },
            ..
        }
    ));
    runner
        .act(GameAction::ChooseOption {
            choice: "Human".to_string(),
        })
        .unwrap();
    runner.advance_until_stack_empty();
    assert_priority(&runner, P0);
    assert_eq!(
        runner.state().objects[&paper].attached_to,
        Some(AttachTarget::Object(attacker))
    );
    let equip_graft: Vec<_> = live_statics(&runner, attacker, StaticMode::CantBeBlocked).collect();
    assert_eq!(equip_graft.len(), 1);
    assert_eq!(equip_graft[0].origin, DefinitionOrigin::Characteristic);

    runner.cast(dodge).target_object(attacker).resolve();
    assert_priority(&runner, P0);
    let grafts: Vec<_> = live_statics(&runner, attacker, StaticMode::CantBeBlocked).collect();
    // CR 611.2c: Identical rules text can represent effects with different origins.
    assert_eq!(grafts.len(), 2);
    let printed = grafts
        .iter()
        .copied()
        .find(|d| d.origin == DefinitionOrigin::Characteristic)
        .unwrap();
    let resolved = grafts
        .iter()
        .copied()
        .find(|d| d.origin == DefinitionOrigin::Resolution)
        .unwrap();
    let mut normalized = (*resolved).clone();
    normalized.origin = DefinitionOrigin::Characteristic;
    assert_eq!(printed, &normalized);

    resolve_dress_down(&mut runner, dress);
    declare_attack_and_reach_blocks(&mut runner, attacker);
    assert!(runner.declare_blockers(&[(blocker, attacker)]).is_err());
    runner.declare_blockers(&[]).unwrap();
}

#[test]
fn wind_drake_gains_petroglyphs_pump_and_keeps_panic_restriction_after_dress_down() {
    use engine::game::layers::evaluate_layers;
    use engine::types::keywords::Keyword;
    use ManaColor::{Blue, Red};

    let db = shared_card_db().expect("fixture database");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Island"]);
    add_lands(&mut scenario, P0, &[Red, Blue, Blue]);
    scenario.add_real_card(P0, "Muraganda Petroglyphs", Zone::Battlefield, db);
    let panic = scenario.add_real_card(P0, "Panic", Zone::Hand, db);
    let dress = scenario.add_real_card(P0, "Dress Down", Zone::Hand, db);
    let drake = scenario.add_real_card(P1, "Wind Drake", Zone::Battlefield, db);
    let attacker = scenario.add_vanilla(P0, 2, 2);
    let other = scenario.add_vanilla(P1, 2, 2);
    let mut runner = scenario.build();
    evaluate_layers(runner.state_mut());
    let creature = &runner.state().objects[&drake];
    assert!(creature.keywords.contains(&Keyword::Flying));
    assert_eq!((creature.power, creature.toughness), (Some(2), Some(2)));
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(attacker, AttackTarget::Player(P1))])
        .unwrap();
    assert_priority(&runner, P0);

    let mut control = GameRunner::from_state(runner.state().clone());
    resolve_dress_down(&mut control, dress);
    reach_blocks(&mut control, P1);
    control.declare_blockers(&[(drake, attacker)]).unwrap();

    runner
        .cast(panic)
        .target_object(drake)
        .resolve()
        .assert_zone(&[panic], Zone::Graveyard);
    assert_priority(&runner, P0);
    resolve_dress_down(&mut runner, dress);
    let creature = &runner.state().objects[&drake];
    // CR 109.3 + CR 611.2c + CR 613.1f: With flying removed, the Drake has no abilities.
    assert!(!creature.keywords.contains(&Keyword::Flying));
    assert_eq!((creature.power, creature.toughness), (Some(4), Some(4)));
    reach_blocks(&mut runner, P1);
    assert!(runner.declare_blockers(&[(drake, attacker)]).is_err());
    runner.declare_blockers(&[(other, attacker)]).unwrap();
}

#[test]
fn break_through_the_line_loses_haste_but_keeps_unblockability() {
    use engine::types::keywords::Keyword;
    use ManaColor::{Blue, Red};

    let db = shared_card_db().expect("fixture database");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Island"]);
    add_lands(&mut scenario, P0, &[Red, Blue, Blue]);
    let line = scenario.add_enchantment_from_oracle(
        P0, "Break Through the Line",
        "{R}: Target creature with power 2 or less gains haste until end of turn and can't be blocked this turn.",
    ).id();
    let dress = scenario.add_real_card(P0, "Dress Down", Zone::Hand, db);
    let attacker = scenario.add_vanilla(P0, 2, 2);
    let blocker = scenario.add_vanilla(P1, 2, 2);
    let mut runner = scenario.build();
    let mut control = GameRunner::from_state(runner.state().clone());
    declare_attack_and_reach_blocks(&mut control, attacker);
    control.declare_blockers(&[(blocker, attacker)]).unwrap();

    runner
        .activate(line, activation(&runner, line))
        .target_object(attacker)
        .resolve();
    assert_priority(&runner, P0);
    assert!(runner.state().objects[&attacker]
        .keywords
        .contains(&Keyword::Haste));
    let mut before_removal = GameRunner::from_state(runner.state().clone());
    declare_attack_and_reach_blocks(&mut before_removal, attacker);
    assert!(before_removal
        .declare_blockers(&[(blocker, attacker)])
        .is_err());
    before_removal.declare_blockers(&[]).unwrap();

    resolve_dress_down(&mut runner, dress);
    // CR 702.10a + CR 613.1f: Haste is an ability that Dress Down removes.
    assert!(!runner.state().objects[&attacker]
        .keywords
        .contains(&Keyword::Haste));
    declare_attack_and_reach_blocks(&mut runner, attacker);
    assert!(runner.declare_blockers(&[(blocker, attacker)]).is_err());
    runner.declare_blockers(&[]).unwrap();
}

#[test]
fn provoke_spell_still_requires_a_block_after_dress_down() {
    use ManaColor::{Blue, Green};

    let db = shared_card_db().expect("fixture database");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Forest", "Island"]);
    add_lands(&mut scenario, P0, &[Green, Green, Blue, Blue]);
    let provoke = scenario.add_real_card(P0, "Provoke", Zone::Hand, db);
    let dress = scenario.add_real_card(P0, "Dress Down", Zone::Hand, db);
    let attacker = scenario.add_vanilla(P0, 2, 2);
    let blocker = scenario.add_vanilla(P1, 2, 2);
    let mut runner = scenario.build();
    let mut control = GameRunner::from_state(runner.state().clone());
    resolve_dress_down(&mut control, dress);
    declare_attack_and_reach_blocks(&mut control, attacker);
    control.declare_blockers(&[]).unwrap();

    runner
        .cast(provoke)
        .target_object(blocker)
        .resolve()
        .assert_zone(&[provoke], Zone::Graveyard);
    assert_priority(&runner, P0);
    resolve_dress_down(&mut runner, dress);
    declare_attack_and_reach_blocks(&mut runner, attacker);
    // CR 509.1c: The blocker must fulfill the resolved requirement if able.
    assert!(runner.declare_blockers(&[]).is_err());
    runner.declare_blockers(&[(blocker, attacker)]).unwrap();
}

#[test]
fn deftblade_elite_provoke_still_requires_blocking_the_elite_after_dress_down() {
    use ManaColor::Blue;

    let db = shared_card_db().expect("fixture database");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Island"]);
    add_lands(&mut scenario, P0, &[Blue, Blue]);
    let dress = scenario.add_real_card(P0, "Dress Down", Zone::Hand, db);
    let elite = scenario.add_creature(P0, "Deftblade Elite", 1, 1)
        .with_color(vec![ManaColor::White])
        .with_subtypes(vec!["Human", "Soldier"])
        .from_oracle_text_with_keywords(
            &["Provoke"],
            "Provoke (Whenever this creature attacks, you may have target creature defending player controls untap and block it if able.)\n{1}{W}: Prevent all combat damage that would be dealt to and dealt by this creature this turn.",
        ).id();
    let other = scenario.add_vanilla(P0, 2, 2);
    let blocker = scenario.add_vanilla(P1, 2, 2);
    let alternative = scenario.add_vanilla(P1, 2, 2);
    let mut runner = scenario.build();
    runner.advance_to_combat();
    runner
        .declare_attackers(&[
            (elite, AttackTarget::Player(P1)),
            (other, AttackTarget::Player(P1)),
        ])
        .unwrap();
    let WaitingFor::TriggerTargetSelection {
        player: P0,
        source_id,
        target_slots,
        ..
    } = &runner.state().waiting_for
    else {
        panic!(
            "expected the Elite's provoke target selection, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(*source_id, Some(elite));
    assert_eq!(target_slots.len(), 1);
    let target = TargetRef::Object(blocker);
    assert!(target_slots[0].legal_targets.contains(&target));
    assert!(target_slots[0]
        .legal_targets
        .contains(&TargetRef::Object(alternative)));
    runner
        .act(GameAction::ChooseTarget {
            target: Some(target),
        })
        .unwrap();
    runner.advance_until_stack_empty();
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::OptionalEffectChoice { player: P0, .. }
    ));

    let mut control = GameRunner::from_state(runner.state().clone());
    control
        .act(GameAction::DecideOptionalEffect { accept: false })
        .unwrap();
    control.advance_until_stack_empty();
    assert_priority(&control, P0);
    resolve_dress_down(&mut control, dress);
    reach_blocks(&mut control, P1);
    control.declare_blockers(&[(blocker, other)]).unwrap();

    runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .unwrap();
    runner.advance_until_stack_empty();
    assert_priority(&runner, P0);
    resolve_dress_down(&mut runner, dress);
    reach_blocks(&mut runner, P1);
    // CR 702.39a + CR 509.1c: Only blocking the provoking attacker satisfies the requirement.
    assert!(runner.declare_blockers(&[(blocker, other)]).is_err());
    runner.declare_blockers(&[(blocker, elite)]).unwrap();
}

fn advance_to_player_main(runner: &mut GameRunner, player: PlayerId) {
    for _ in 0..64 {
        if runner.state().active_player == player && runner.state().phase == Phase::PreCombatMain {
            assert_priority(runner, player);
            return;
        }
        match runner.state().waiting_for {
            WaitingFor::Priority { .. } => runner.act(GameAction::PassPriority).unwrap(),
            WaitingFor::DeclareAttackers { .. } => runner.declare_attackers(&[]).unwrap(),
            ref other => panic!("unexpected prompt while advancing to main phase: {other:?}"),
        };
    }
    panic!("did not reach main phase for {player:?}");
}

fn assert_declare_attackers(runner: &GameRunner, attacker: PlayerId) {
    assert!(runner.state().stack.is_empty());
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::DeclareAttackers { player, .. } if player == attacker
    ));
}

#[test]
fn boiling_blood_still_requires_attacking_after_dress_down() {
    use ManaColor::{Blue, Red};

    let db = shared_card_db().expect("fixture database");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P1, &["Island", "Mountain", "Island"]);
    add_lands(&mut scenario, P1, &[Red, Red, Red, Blue, Blue]);
    let blood = scenario.add_real_card(P1, "Boiling Blood", Zone::Hand, db);
    let dress = scenario.add_real_card(P1, "Dress Down", Zone::Hand, db);
    let attacker = scenario.add_vanilla(P1, 2, 2);
    let mut runner = scenario.build();
    advance_to_player_main(&mut runner, P1);

    let mut control = GameRunner::from_state(runner.state().clone());
    resolve_dress_down(&mut control, dress);
    control.advance_to_combat();
    assert_declare_attackers(&control, P1);
    control.declare_attackers(&[]).unwrap();

    runner
        .cast(blood)
        .target_object(attacker)
        .resolve()
        .assert_zone(&[blood], Zone::Graveyard);
    assert_priority(&runner, P1);
    resolve_dress_down(&mut runner, dress);
    runner.advance_to_combat();
    assert_declare_attackers(&runner, P1);
    // CR 508.1d: The declaration must satisfy the attack requirement.
    assert!(runner.declare_attackers(&[]).is_err());
    runner
        .declare_attackers(&[(attacker, AttackTarget::Player(P0))])
        .unwrap();
}

#[test]
fn alluring_siren_still_requires_attacking_its_controller_after_dress_down() {
    let p2 = PlayerId(2);
    let db = shared_card_db().expect("fixture database");
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P1, &["Island", "Island"]);
    add_lands(&mut scenario, P1, &[ManaColor::Blue; 2]);
    let dress = scenario.add_real_card(P1, "Dress Down", Zone::Hand, db);
    let siren = scenario
        .add_creature_from_oracle(
            P0,
            "Alluring Siren",
            1,
            1,
            "{T}: Target creature an opponent controls attacks you this turn if able.",
        )
        .id();
    let attacker = scenario.add_vanilla(P1, 2, 2);
    let mut runner = scenario.build();
    advance_to_player_main(&mut runner, P1);

    let mut control = GameRunner::from_state(runner.state().clone());
    resolve_dress_down(&mut control, dress);
    control.advance_to_combat();
    assert_declare_attackers(&control, P1);
    control
        .declare_attackers(&[(attacker, AttackTarget::Player(p2))])
        .unwrap();

    runner.act(GameAction::PassPriority).unwrap();
    runner.act(GameAction::PassPriority).unwrap();
    assert_priority(&runner, P0);
    runner
        .activate(siren, activation(&runner, siren))
        .target_object(attacker)
        .resolve();
    assert_priority(&runner, P1);
    assert!(runner.state().objects[&siren].tapped);
    resolve_dress_down(&mut runner, dress);
    runner.advance_to_combat();
    assert_declare_attackers(&runner, P1);
    // CR 508.1d: Attacking another player leaves a satisfiable requirement unmet.
    assert!(runner
        .declare_attackers(&[(attacker, AttackTarget::Player(p2))])
        .is_err());
    runner
        .declare_attackers(&[(attacker, AttackTarget::Player(P0))])
        .unwrap();
}

#[test]
fn exerted_champion_keeps_its_untap_restriction_after_dress_down() {
    use engine::types::ability::DefinitionOrigin;
    use ManaColor::Blue;

    let db = shared_card_db().expect("fixture database");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Island", "Island"]);
    scenario.with_library_top(P1, &["Island"]);
    add_lands(&mut scenario, P0, &[Blue]);
    let dress = scenario.add_real_card(P0, "Dress Down", Zone::Hand, db);
    let champion = scenario.add_real_card(P0, "Ahn-Crop Champion", Zone::Battlefield, db);
    let control = scenario
        .add_creature_from_oracle(P0, "Llanowar Elves", 1, 1, "{T}: Add {G}.")
        .id();
    let mut runner = scenario.build();
    runner.state_mut().objects.get_mut(&control).unwrap().tapped = true;
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(champion, AttackTarget::Player(P1))])
        .unwrap();
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ExertChoice { player: P0, attacker, .. } if attacker == champion
    ));
    runner.act(GameAction::ChooseExert { exert: true }).unwrap();
    runner.advance_until_stack_empty();
    assert_priority(&runner, P0);
    assert!(!runner.state().objects[&control].tapped);
    // CR 701.43a: The choice to exert installs an untap prohibition.
    let origins = |runner: &GameRunner| {
        live_statics(runner, champion, StaticMode::CantUntap)
            .map(|d| d.origin)
            .collect::<Vec<_>>()
    };
    assert_eq!(origins(&runner), [DefinitionOrigin::Resolution]);

    // The Elves supply the generic mana and start the next turn tapped.
    resolve_dress_down(&mut runner, dress);
    assert_eq!(origins(&runner), [DefinitionOrigin::Resolution]);
    reach_blocks(&mut runner, P1);
    runner.declare_blockers(&[]).unwrap();
    advance_to_player_main(&mut runner, P1);
    assert!(runner.state().objects[&champion].tapped);
    assert!(runner.state().objects[&control].tapped);
    advance_to_player_main(&mut runner, P0);
    assert!(runner.state().objects[&champion].tapped);
    assert!(!runner.state().objects[&control].tapped);
}

#[test]
fn disrupt_decorum_goad_survives_dress_down_in_three_player_combat() {
    use ManaColor::{Blue, Red};

    let p2 = PlayerId(2);
    let db = shared_card_db().expect("fixture database");
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P1, &["Island", "Island"]);
    add_lands(&mut scenario, P0, &[Red; 4]);
    add_lands(&mut scenario, P1, &[Blue; 2]);
    let decorum = scenario.add_real_card(P0, "Disrupt Decorum", Zone::Hand, db);
    let dress = scenario.add_real_card(P1, "Dress Down", Zone::Hand, db);
    let attacker = scenario.add_vanilla(P1, 2, 2);
    let mut runner = scenario.build();
    let mut control = GameRunner::from_state(runner.state().clone());
    advance_to_player_main(&mut control, P1);
    resolve_dress_down(&mut control, dress);
    control.advance_to_combat();
    assert_declare_attackers(&control, P1);
    let mut no_attack = GameRunner::from_state(control.state().clone());
    no_attack.declare_attackers(&[]).unwrap();
    control
        .declare_attackers(&[(attacker, AttackTarget::Player(P0))])
        .unwrap();

    runner
        .cast(decorum)
        .resolve()
        .assert_zone(&[decorum], Zone::Graveyard);
    assert_priority(&runner, P0);
    advance_to_player_main(&mut runner, P1);
    resolve_dress_down(&mut runner, dress);
    runner.advance_to_combat();
    assert_declare_attackers(&runner, P1);
    // CR 701.15a + CR 701.15b: Goad is a designation and still directs the attack.
    assert!(runner.declare_attackers(&[]).is_err());
    assert!(runner
        .declare_attackers(&[(attacker, AttackTarget::Player(P0))])
        .is_err());
    runner
        .declare_attackers(&[(attacker, AttackTarget::Player(p2))])
        .unwrap();
}

fn float_mana(runner: &mut GameRunner, player: PlayerId, mana: &[engine::types::mana::ManaType]) {
    use engine::types::mana::ManaUnit;

    let pool = &mut runner.state_mut().players[player.0 as usize].mana_pool;
    for &mana_type in mana {
        pool.add(ManaUnit::new(mana_type, ObjectId(0), false, Vec::new()));
    }
}

fn assert_tax(
    runner: &GameRunner,
    payer: PlayerId,
    expected_context: engine::types::game_state::CombatTaxContext,
    creatures: &[ObjectId],
) {
    use engine::types::mana::parse_mtgjson_mana_cost;

    let WaitingFor::CombatTaxPayment {
        player,
        context,
        total_cost,
        per_creature,
        ..
    } = &runner.state().waiting_for
    else {
        panic!(
            "expected combat tax payment, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(*player, payer);
    assert_eq!(*context, expected_context);
    assert_eq!(
        *total_cost,
        parse_mtgjson_mana_cost(&format!("{{{}}}", creatures.len() * 2))
    );
    assert_eq!(per_creature.len(), creatures.len());
    for creature in creatures {
        assert!(per_creature.contains(&(*creature, parse_mtgjson_mana_cost("{2}"))));
    }
}

#[test]
fn forbidding_spirit_attack_tax_survives_humility() {
    use engine::types::game_state::CombatTaxContext;
    use engine::types::mana::{parse_mtgjson_mana_cost, ManaType};

    let p2 = PlayerId(2);
    let db = shared_card_db().expect("fixture database");
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P1, &["Island"; 2]);
    add_lands(&mut scenario, P0, &[ManaColor::White; 7]);
    let spirit = scenario
        .add_creature_to_hand_from_oracle(P0, "Forbidding Spirit", 3, 3, FORBIDDING_SPIRIT_ORACLE)
        .with_mana_cost(parse_mtgjson_mana_cost("{1}{W}{W}"))
        .id();
    let humility = scenario.add_real_card(P0, "Humility", Zone::Hand, db);
    let first = scenario.add_vanilla(P1, 2, 2);
    let second = scenario.add_vanilla(P1, 2, 2);
    let mut runner = scenario.build();
    // CR 611.2: Entering and resolving the trigger installs the attack tax.
    runner
        .cast(spirit)
        .resolve()
        .assert_zone(&[spirit], Zone::Battlefield);
    assert_priority(&runner, P0);
    runner
        .cast(humility)
        .resolve()
        .assert_zone(&[humility], Zone::Battlefield);
    assert_priority(&runner, P0);
    assert_eq!(runner.state().objects[&spirit].power, Some(1));

    advance_to_player_main(&mut runner, P1);
    runner.advance_to_combat();
    assert_declare_attackers(&runner, P1);
    let mut untaxed = GameRunner::from_state(runner.state().clone());
    untaxed
        .declare_attackers(&[
            (first, AttackTarget::Player(p2)),
            (second, AttackTarget::Player(p2)),
        ])
        .unwrap();
    assert_priority(&untaxed, P1);
    assert_eq!(untaxed.state().combat.as_ref().unwrap().attackers.len(), 2);

    float_mana(&mut runner, P1, &[ManaType::Colorless; 4]);
    runner
        .declare_attackers(&[
            (first, AttackTarget::Player(P0)),
            (second, AttackTarget::Player(P0)),
        ])
        .unwrap();
    // CR 508.1h–j: The protected defender costs two mana per proposed attacker.
    assert_tax(&runner, P1, CombatTaxContext::Attacking, &[first, second]);
    let mut declined = GameRunner::from_state(runner.state().clone());
    declined
        .act(GameAction::PayCombatTax { accept: false })
        .unwrap();
    assert_declare_attackers(&declined, P1);
    declined.declare_attackers(&[]).unwrap();
    assert!(declined
        .state()
        .combat
        .as_ref()
        .is_none_or(|combat| combat.attackers.is_empty()));
    assert!(!matches!(
        declined.state().waiting_for,
        WaitingFor::DeclareAttackers { .. } | WaitingFor::CombatTaxPayment { .. }
    ));

    runner
        .act(GameAction::PayCombatTax { accept: true })
        .unwrap();
    assert_priority(&runner, P1);
    assert_eq!(runner.state().combat.as_ref().unwrap().attackers.len(), 2);
    assert_eq!(runner.state().players[1].mana_pool.total(), 0);
}

#[test]
fn ood_sphere_tap_restriction_survives_dress_down() {
    use engine::database::synthesis::synthesize_planechase;
    use engine::game::planechase::{active_plane, PlanarDieFace};
    use engine::game::printed_cards::apply_card_face_to_object;
    use engine::types::card_type::CoreType;
    use engine::types::events::GameEvent;
    use engine::types::format::FormatConfig;
    use engine::types::game_state::GameRng;

    let db = shared_card_db().expect("fixture database");
    let mut scenario = GameScenario::new_with_format(FormatConfig::planechase(), 2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P1, &["Island"; 3]);
    add_lands(&mut scenario, P1, &[ManaColor::Blue; 2]);
    let plane = scenario.add_real_card(P0, "Ood Sphere", Zone::Command, db);
    let mut face = db
        .get_face_by_name("Ood Sphere")
        .expect("plane face")
        .clone();
    let types: Vec<_> = face
        .card_type
        .core_types
        .iter()
        .map(ToString::to_string)
        .collect();
    face.triggers = parse_oracle_text(
        face.oracle_text.as_deref().expect("Ood Sphere Oracle text"),
        &face.name,
        &[],
        &types,
        &face.card_type.subtypes,
    )
    .triggers;
    // The card export runs this after parsing so plane triggers function from command.
    synthesize_planechase(&mut face);
    let dress = scenario.add_real_card(P1, "Dress Down", Zone::Hand, db);
    let copter = scenario.add_real_card(P1, "Smuggler's Copter", Zone::Battlefield, db);
    let goaded = scenario.add_real_card(P1, "Wind Drake", Zone::Battlefield, db);
    let pilot = scenario.add_vanilla(P1, 2, 2);
    let mut runner = scenario.build();
    apply_card_face_to_object(runner.state_mut().objects.get_mut(&plane).unwrap(), &face);
    runner.state_mut().planar_controller = Some(P0);
    assert_eq!(active_plane(runner.state()), Some(plane));

    let chaos = |event: &GameEvent| {
        matches!(
            event,
            GameEvent::PlanarDieRolled {
                player_id: P0,
                face: PlanarDieFace::Chaos
            }
        )
    };
    let seed = (0..512)
        .find(|&seed| {
            let mut trial = GameRunner::from_state(runner.state().clone());
            trial.state_mut().rng = GameRng::seed_from_u64(seed);
            trial
                .act(GameAction::RollPlanarDie)
                .is_ok_and(|result| result.events.iter().any(chaos))
        })
        .expect("a deterministic seed rolls Chaos");
    // CR 901.9b: Roll publicly so the active plane produces its chaos trigger.
    runner.state_mut().rng = GameRng::seed_from_u64(seed);
    let rolled = runner.act(GameAction::RollPlanarDie).unwrap();
    assert!(rolled.events.iter().any(chaos));
    assert!(matches!(
        &runner.state().waiting_for,
        WaitingFor::TriggerTargetSelection {
            player: P0,
            source_id: Some(source),
            selection,
            ..
        } if *source == plane && selection.current_legal_targets.contains(&TargetRef::Object(goaded))
    ));
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(goaded)),
        })
        .unwrap();
    assert_eq!(runner.state().stack.back().unwrap().source_id, plane);
    runner.resolve_top();
    assert_priority(&runner, P0);
    assert!(runner.state().objects[&goaded].goaded_by.contains(&P0));

    advance_to_player_main(&mut runner, P1);
    resolve_dress_down(&mut runner, dress);
    assert!(!engine::game::keywords::has_keyword(
        &runner.state().objects[&goaded],
        &engine::types::keywords::Keyword::Flying
    ));
    assert!(!runner.state().objects[&goaded].tapped);
    assert!(!runner.state().objects[&copter]
        .card_types
        .core_types
        .contains(&CoreType::Creature));
    let mut attack = GameRunner::from_state(runner.state().clone());

    // CR 702.122a: The Crewing prompt excludes the prohibited creature and
    // accepts the other untapped creature.
    runner
        .act(GameAction::CrewVehicle {
            vehicle_id: copter,
            creature_ids: vec![],
        })
        .unwrap();
    assert!(
        matches!(&runner.state().waiting_for, WaitingFor::CrewVehicle {
        player: P1, vehicle_id, eligible_creatures, ..
    } if *vehicle_id == copter && !eligible_creatures.contains(&goaded) && eligible_creatures.contains(&pilot))
    );
    let mut prohibited = GameRunner::from_state(runner.state().clone());
    assert!(prohibited
        .act(GameAction::CrewVehicle {
            vehicle_id: copter,
            creature_ids: vec![goaded],
        })
        .is_err());
    runner
        .act(GameAction::CrewVehicle {
            vehicle_id: copter,
            creature_ids: vec![pilot],
        })
        .unwrap();
    runner.advance_until_stack_empty();
    assert_priority(&runner, P1);
    assert!(runner.state().objects[&pilot].tapped);
    assert!(!runner.state().objects[&goaded].tapped);
    assert!(runner.state().objects[&copter]
        .card_types
        .core_types
        .contains(&CoreType::Creature));

    attack.advance_to_combat();
    assert_declare_attackers(&attack, P1);
    // CR 508.1f: Red-Eye allows this tap as part of declaring attackers.
    attack
        .declare_attackers(&[(goaded, AttackTarget::Player(P0))])
        .unwrap();
    assert_priority(&attack, P1);
    assert!(attack.state().objects[&goaded].tapped);
}

#[test]
fn will_cost_reduction_survives_humility() {
    use engine::game::keywords::has_keyword;
    use engine::types::keywords::Keyword;
    use engine::types::mana::ManaType::{Colorless, Green, White};

    let db = shared_card_db().expect("fixture database");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let will = scenario
        .add_creature(P0, "Will, Scion of Peace", 2, 4)
        .with_color(vec![ManaColor::White, ManaColor::Blue])
        .with_subtypes(vec!["Human", "Wizard"])
        .as_legendary()
        .from_oracle_text(WILL_ORACLE)
        .id();
    let salve = scenario.add_real_card(P0, "Healing Salve", Zone::Hand, db);
    let second_salve = scenario.add_real_card(P0, "Healing Salve", Zone::Hand, db);
    let humility = scenario.add_real_card(P0, "Humility", Zone::Hand, db);
    let legionnaire = scenario.add_real_card(P0, "Akron Legionnaire", Zone::Hand, db);
    let bears = scenario.add_real_card(P0, "Grizzly Bears", Zone::Hand, db);
    let mut runner = scenario.build();
    float_mana(&mut runner, P0, &[White]);
    runner
        .cast(salve)
        .modes(&[0])
        .target_player(P0)
        .resolve()
        .assert_life_delta(P0, 3);
    assert_priority(&runner, P0);
    let ability = activation(&runner, will);
    runner.activate(will, ability).resolve();
    assert_priority(&runner, P0);
    assert!(runner.state().objects[&will].tapped);
    assert!(has_keyword(
        &runner.state().objects[&will],
        &Keyword::Vigilance
    ));
    float_mana(&mut runner, P0, &[White]);
    runner
        .cast(second_salve)
        .modes(&[0])
        .target_player(P0)
        .resolve()
        .assert_life_delta(P0, 3);
    assert_priority(&runner, P0);
    assert_eq!(runner.state().players[0].life_gained_this_turn, 6);
    float_mana(&mut runner, P0, &[White, White, Colorless, Colorless]);
    runner
        .cast(humility)
        .resolve()
        .assert_zone(&[humility], Zone::Battlefield);
    assert_priority(&runner, P0);
    // CR 613.1f: Will loses its printed abilities.
    assert!(!has_keyword(
        &runner.state().objects[&will],
        &Keyword::Vigilance
    ));
    assert!(activated_ability_definitions(runner.state(), will).is_empty());

    runner.state_mut().players[0].mana_pool.clear();
    float_mana(
        &mut runner,
        P0,
        &[White, White, Colorless, Colorless, Colorless, Green],
    );
    let mut nonmatching = GameRunner::from_state(runner.state().clone());
    // CR 611.2d + CR 601.2f: The reduction was locked at {3} when Will's ability
    // resolved, so {6}{W}{W} costs {3}{W}{W}; a live reading of the six life
    // gained this turn would make it cost {W}{W}.
    let outcome = runner.cast(legionnaire).resolve();
    outcome.assert_zone(&[legionnaire], Zone::Battlefield);
    assert_eq!(outcome.mana_pool_total(P0), 1);
    assert_eq!(outcome.mana_pool_color(P0, White), 0);
    assert_priority(&runner, P0);

    let outcome = nonmatching.cast(bears).resolve();
    outcome.assert_zone(&[bears], Zone::Battlefield);
    assert_eq!(outcome.mana_pool_total(P0), 4);
    assert_eq!(outcome.mana_pool_color(P0, Green), 0);
    assert_priority(&nonmatching, P0);
}

fn war_cadence_under_humility() -> (GameRunner, ObjectId) {
    use engine::types::card_type::CoreType;
    use engine::types::mana::{parse_mtgjson_mana_cost, ManaType};

    let db = shared_card_db().expect("fixture database");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P1, &["Island"; 2]);
    let cadence = scenario
        .add_enchantment_from_oracle(P1, "War Cadence", WAR_CADENCE_ORACLE)
        .with_mana_cost(parse_mtgjson_mana_cost("{2}{R}"))
        .id();
    let opalescence = scenario.add_real_card(P1, "Opalescence", Zone::Hand, db);
    let humility = scenario.add_real_card(P1, "Humility", Zone::Hand, db);
    let mut runner = scenario.build();
    advance_to_player_main(&mut runner, P1);
    float_mana(
        &mut runner,
        P1,
        &[ManaType::Red, ManaType::Colorless, ManaType::Colorless],
    );
    let ability = activation(&runner, cadence);
    runner.activate(cadence, ability).x(2).resolve();
    assert_priority(&runner, P1);
    assert_eq!(runner.state().players[1].mana_pool.total(), 0);
    assert!(
        live_statics(&runner, cadence, StaticMode::CantBlock).any(|definition| {
            matches!(
                definition.condition,
                Some(engine::types::ability::StaticCondition::UnlessPay { .. })
            )
        })
    );
    float_mana(&mut runner, P1, &[ManaType::White; 8]);
    runner
        .cast(opalescence)
        .resolve()
        .assert_zone(&[opalescence], Zone::Battlefield);
    assert_priority(&runner, P1);
    assert!(runner.state().objects[&cadence]
        .card_types
        .core_types
        .contains(&CoreType::Creature));
    assert!(!activated_ability_definitions(runner.state(), cadence).is_empty());
    runner
        .cast(humility)
        .resolve()
        .assert_zone(&[humility], Zone::Battlefield);
    assert_priority(&runner, P1);
    // CR 613.1f: The animated source lost the ability that created this tax.
    assert!(activated_ability_definitions(runner.state(), cadence).is_empty());

    (runner, cadence)
}

/// The block-tax payment itself never prompts at base (a pre-existing gap tracked
/// separately), so this row checks the retained grant at layer level only.
#[test]
fn war_cadence_block_tax_grant_survives_humility() {
    use engine::types::ability::{DefinitionOrigin, StaticCondition};

    let (runner, cadence) = war_cadence_under_humility();
    // CR 611.2 + CR 613.1f: Humility leaves the resolution-created block tax.
    assert!(
        live_statics(&runner, cadence, StaticMode::CantBlock).any(|definition| {
            definition.origin == DefinitionOrigin::Resolution
                && matches!(
                    definition.condition,
                    Some(StaticCondition::UnlessPay { .. })
                )
        })
    );
}

#[test]
fn legend_of_arena_wizard_keeps_cost_reduction_under_humility_until_cleanup() {
    use engine::types::mana::{
        parse_mtgjson_mana_cost,
        ManaType::{Blue, Red, White},
    };

    let db = shared_card_db().expect("fixture database");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    for player in [P0, P1] {
        scenario.with_library_top(player, &["Island"; 4]);
    }
    let saga = scenario
        .add_enchantment_to_hand_from_oracle(P0, "The Legend of Arena", LEGEND_OF_ARENA_ORACLE)
        .with_subtypes(vec!["Saga"])
        .as_legendary()
        .with_mana_cost(parse_mtgjson_mana_cost("{1}{U}{R}{W}"))
        .from_oracle_text(LEGEND_OF_ARENA_ORACLE)
        .id();
    let humility = scenario.add_real_card(P0, "Humility", Zone::Hand, db);
    let probe = scenario.add_real_card(P0, "Think Twice", Zone::Hand, db);
    let mut runner = scenario.build();
    float_mana(&mut runner, P0, &[Blue, Red, White, White]);
    runner
        .cast(saga)
        .resolve()
        .assert_zone(&[saga], Zone::Battlefield);
    assert_priority(&runner, P0);
    // CR 714.3a: The first lore counter triggered chapter I and created its Wizard.
    let wizard = runner
        .state()
        .battlefield
        .iter()
        .copied()
        .find(|id| {
            runner.state().objects[id]
                .card_types
                .subtypes
                .iter()
                .any(|subtype| subtype == "Wizard")
        })
        .expect("chapter I creates a Wizard");
    assert_eq!(runner.state().objects[&wizard].power, Some(2));

    float_mana(&mut runner, P0, &[White; 4]);
    runner
        .cast(humility)
        .resolve()
        .assert_zone(&[humility], Zone::Battlefield);
    assert_priority(&runner, P0);
    assert_eq!(runner.state().objects[&wizard].power, Some(1));
    assert!(runner.state().objects[&wizard]
        .card_types
        .subtypes
        .iter()
        .any(|subtype| subtype == "Wizard"));
    runner.state_mut().players[0].mana_pool.clear();
    let mut expired = GameRunner::from_state(runner.state().clone());
    float_mana(&mut runner, P0, &[Blue, Blue]);
    // CR 601.2f: The Wizard still reduces Think Twice from {1}{U} to {U}.
    let outcome = runner.cast(probe).resolve();
    outcome.assert_zone(&[probe], Zone::Graveyard);
    outcome.assert_hand_drawn(P0, 1);
    assert_eq!(outcome.mana_pool_total(P0), 1);
    assert_priority(&runner, P0);

    // CR 514.2: The reduction expires at cleanup with the same Wizard in play.
    advance_to_player_main(&mut expired, P1);
    expired.act(GameAction::PassPriority).unwrap();
    assert_priority(&expired, P0);
    assert_eq!(expired.state().objects[&wizard].zone, Zone::Battlefield);
    float_mana(&mut expired, P0, &[Blue, Blue]);
    let outcome = expired.cast(probe).resolve();
    outcome.assert_zone(&[probe], Zone::Graveyard);
    outcome.assert_hand_drawn(P0, 1);
    assert_eq!(outcome.mana_pool_total(P0), 0);
    assert_priority(&expired, P1);
}

#[test]
fn teyo_attack_direction_survives_imprisoned_in_the_moon() {
    use engine::types::ability::{AbilityCost, SeatDirection, TypeFilter, TypedFilter};
    use engine::types::card_type::CoreType;
    use engine::types::events::GameEvent;
    use engine::types::game_state::StackEntryKind;
    use engine::types::keywords::Keyword;
    use engine::types::mana::parse_mtgjson_mana_cost;

    let p2 = PlayerId(2);
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P1, &["Island"; 2]);
    add_lands(&mut scenario, P0, &[ManaColor::Blue; 3]);
    let teyo = scenario
        .add_planeswalker_from_oracle(P0, "Teyo, Geometric Tactician", "Teyo", 3, TEYO_ORACLE)
        .id();
    let moon = scenario
        .add_enchantment_to_hand_from_oracle(P0, "Imprisoned in the Moon", MOON_ORACLE)
        .with_subtypes(vec!["Aura"])
        .with_mana_cost(parse_mtgjson_mana_cost("{2}{U}"))
        .from_oracle_text_with_keywords(&["enchant"], MOON_ORACLE)
        .id();
    let attacker = scenario.add_vanilla(P1, 2, 2);
    let mut runner = scenario.build();
    // SHAPE: Match the exported Aura's complete enchant restriction.
    assert_eq!(
        runner.state().objects[&moon].keywords,
        [Keyword::Enchant(TargetFilter::Or {
            filters: [
                TypeFilter::Creature,
                TypeFilter::Land,
                TypeFilter::Planeswalker
            ]
            .into_iter()
            .map(|kind| TargetFilter::Typed(TypedFilter::new(kind)))
            .collect(),
        })]
    );
    let (minus_two, _) = activated_ability_definitions(runner.state(), teyo)
        .into_iter()
        .find(|(_, ability)| {
            matches!(
                ability.cost.as_ref(),
                Some(AbilityCost::Loyalty { amount: -2 })
            )
        })
        .expect("Teyo's −2 loyalty ability");
    runner.activate(teyo, minus_two).resolve();
    // CR 608.2d: The ability pauses for its left/right choice.
    assert!(
        matches!(&runner.state().waiting_for, WaitingFor::NamedChoice {
        player: P0, options, ..
    } if options == &["Left", "Right"])
    );
    runner
        .act(GameAction::ChooseOption {
            choice: "Left".to_string(),
        })
        .unwrap();
    runner.advance_until_stack_empty();
    assert_priority(&runner, P0);
    assert_eq!(runner.state().objects[&teyo].loyalty, Some(1));
    assert_eq!(
        runner.state().objects[&teyo].chosen_direction(),
        Some(SeatDirection::Left)
    );

    let cast = runner.cast(moon).target_object(teyo).commit();
    assert!(
        cast.state().stack.iter().any(|entry| {
            entry.id == moon
                && matches!(&entry.kind, StackEntryKind::Spell { ability: Some(ability), .. }
                if ability.targets == [TargetRef::Object(teyo)])
        }),
        "Imprisoned in the Moon must have Teyo as its target on the stack"
    );
    let outcome = cast.resolve();
    assert_eq!(
        outcome.state().objects[&moon].attached_to,
        Some(AttachTarget::Object(teyo)),
        "Imprisoned in the Moon must stay attached to Teyo. Aura events: {:?}",
        outcome
            .events()
            .iter()
            .filter(|event| matches!(event,
                GameEvent::ZoneChanged { object_id, .. } if *object_id == moon
            ) || matches!(event,
                GameEvent::Attached { attachment_id, .. }
                    | GameEvent::Unattached { attachment_id, .. } if *attachment_id == moon
            ))
            .collect::<Vec<_>>()
    );
    outcome.assert_zone(&[moon], Zone::Battlefield);
    assert_priority(&runner, P0);
    // CR 613.1d + CR 613.1f: The Aura makes Teyo a land in layer 4 and removes
    // its loyalty abilities in layer 6.
    assert_eq!(
        runner.state().objects[&teyo].card_types.core_types,
        [CoreType::Land]
    );
    assert!(activated_ability_definitions(runner.state(), teyo)
        .iter()
        .all(|(_, ability)| {
            !matches!(ability.cost.as_ref(), Some(AbilityCost::Loyalty { .. }))
        }));

    advance_to_player_main(&mut runner, P1);
    runner.advance_to_combat();
    assert_declare_attackers(&runner, P1);
    // P2 is on P1's left in this fixture, P0 on the right. CR 508.1c requires
    // the declaration to obey Teyo's resolved attack restriction.
    runner
        .declare_attackers(&[(attacker, AttackTarget::Player(P0))])
        .expect_err("Teyo rules out the other direction");
    runner
        .declare_attackers(&[(attacker, AttackTarget::Player(p2))])
        .unwrap();
    assert_priority(&runner, P1);
    assert_eq!(
        runner.state().combat.as_ref().unwrap().attackers[0].defending_player,
        p2
    );
}
