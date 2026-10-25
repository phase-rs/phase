//! Coin-branch announcement and execution regressions.
//! Synthetic fixtures deliberately omit Crooked Scales' repeat instruction.

use engine::ai_support::legal_actions;
use engine::game::casting::can_activate_ability_now;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::parser::parse_oracle_text;
use engine::types::ability::{CoinFlipRun, Effect, EffectKind, TargetFilter, TargetRef};
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::{CastPaymentMode, GameRng, RandomDraw, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;
use rand::Rng;

// Synthetic, targetless source-local punishment: isolates payment completion
// from the missing announcement-time targets being investigated by T.

fn pin_outcomes(runner: &mut GameRunner, outcomes: &[bool]) {
    let seed = (0..100_000)
        .find(|&seed| {
            let mut rng = GameRng::seed_from_u64(seed);
            outcomes
                .iter()
                .all(|&won| rng.draw(RandomDraw::Outcome).random_bool(0.5) == won)
        })
        .expect("bounded seed search finds the requested outcome sequence");
    let state = runner.state_mut();
    state.rng_seed = seed;
    state.rng_word_pos = 0;
    state.rng = GameRng::seed_from_u64(seed);
}

fn mana(scenario: &mut GameScenario, count: usize) {
    scenario.with_mana_pool(
        P0,
        (0..count)
            .map(|_| ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]))
            .collect(),
    );
}

fn pool(runner: &GameRunner) -> usize {
    runner.state().players[P0.0 as usize].mana_pool.total()
}

fn flips(events: &[GameEvent]) -> Vec<bool> {
    events
        .iter()
        .filter_map(|event| match event {
            GameEvent::CoinFlipped { player_id, won } => {
                assert_eq!(*player_id, P0);
                Some(*won)
            }
            _ => None,
        })
        .collect()
}

fn coin_terminals(events: &[GameEvent], source: ObjectId) -> usize {
    events
        .iter()
        .filter(|event| {
            matches!(event,
            GameEvent::EffectResolved { kind: EffectKind::FlipCoins, source_id, .. }
                if *source_id == source)
        })
        .count()
}

fn finish(runner: &mut GameRunner, events: &mut Vec<GameEvent>) {
    for _ in 0..20 {
        if runner.state().stack.is_empty() {
            return;
        }
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { .. }
        ));
        events.extend(
            runner
                .act(GameAction::PassPriority)
                .expect("pass priority to finish the announced ability")
                .events,
        );
    }
    panic!("announced ability did not finish within 20 priority passes");
}

fn assert_finished(runner: &GameRunner) {
    // CR 117.3b: the active player receives priority after resolution completes.
    assert!(runner.state().stack.is_empty());
    assert!(runner.state().resolution_stack.is_empty());
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P0 }
    ));
}

// Byte-identical to data/mtgjson/AtomicCards.json .data["Chaotic Strike"][0].text.
const CHAOTIC_STRIKE: &str = "Cast this spell only during combat after blockers are declared.\nFlip a coin. If you win the flip, target creature gets +1/+1 until end of turn.\nDraw a card.";
// Synthetic supported two-branch fixture, not the complete Crooked Scales card.
// The payment/repeat suffix is deliberately absent from this T witness.
const TWO_BRANCHES: &str = "{4}, {T}: Flip a coin. If you win the flip, destroy target creature an opponent controls. If you lose the flip, destroy target creature you control.";

fn chaotic_fixture(with_creatures: bool) -> (GameRunner, ObjectId, Vec<ObjectId>) {
    // SHAPE eligibility: the live parser must retain both the targeted win and
    // the independent draw, with exactly one coin rather than two coin nodes.
    let parsed = parse_oracle_text(
        CHAOTIC_STRIKE,
        "Chaotic Strike",
        &[],
        &["Instant".into()],
        &[],
    );
    assert_eq!(parsed.abilities.len(), 1);
    let root = &parsed.abilities[0];
    let Effect::FlipCoins {
        flips: run,
        win_effect: Some(win),
        lose_effect: None,
        ..
    } = root.effect.as_ref()
    else {
        panic!("required Chaotic Strike coin shape: {root:#?}");
    };
    assert_eq!(*run, CoinFlipRun::single());
    assert!(matches!(win.effect.as_ref(), Effect::Pump { .. }));
    assert!(win.sub_ability.is_none());
    let draw = root
        .sub_ability
        .as_ref()
        .expect("independent draw successor");
    assert!(matches!(draw.effect.as_ref(), Effect::Draw { .. }));
    assert!(draw.sub_ability.is_none());

    let mut scenario = GameScenario::new();
    // Both restrictions allow this actual post-blockers combat step.
    scenario.at_phase(Phase::EndCombat);
    let creatures = if with_creatures {
        vec![
            scenario.add_creature(P0, "Chosen Bear", 2, 2).id(),
            scenario.add_creature(P1, "Distractor Bear", 4, 4).id(),
        ]
    } else {
        vec![]
    };
    scenario.with_library_top(P0, &["Draw Witness", "Library Reserve"]);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Chaotic Strike", true, CHAOTIC_STRIKE)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Red],
            generic: 1,
        })
        .id();
    scenario.with_mana_pool(
        P0,
        (0..2)
            .map(|_| ManaUnit::new(ManaType::Red, ObjectId(0), false, vec![]))
            .collect(),
    );
    let mut runner = scenario.build();
    pin_outcomes(&mut runner, &[true]);
    (runner, spell, creatures)
}

fn castable(runner: &GameRunner, spell: ObjectId) -> bool {
    legal_actions(runner.state()).iter().any(|action| {
        matches!(action,
        GameAction::CastSpell { object_id, .. } if *object_id == spell)
    })
}

#[test]
fn chaotic_strike_win_pumps_announced_target_and_draws() {
    let (mut runner, spell, creatures) = chaotic_fixture(true);
    let outcome = runner.cast(spell).target_object(creatures[0]).resolve();
    assert_eq!(
        flips(outcome.events()),
        vec![true],
        "reach: forced win actually resolves"
    );
    outcome.assert_hand_drawn(P0, 1);
    // CR 608.2c: the won branch pumps the announced creature, not a distractor.
    assert_eq!(
        outcome.power_toughness(creatures[0]),
        (3, 3),
        "won branch must affect the announced target"
    );
    assert_eq!(outcome.power_toughness(creatures[1]), (4, 4));
    assert_eq!(outcome.zone_of(spell), Zone::Graveyard);
    assert_finished(&runner);
}

#[test]
fn chaotic_strike_announces_its_target_before_flipping() {
    let (mut runner, spell, creatures) = chaotic_fixture(true);
    let card_id = runner.state().objects[&spell].card_id;
    let mut events = runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast reaches announcement")
        .events;
    // CR 601.2c: target choice is part of announcement, before the coin flips.
    let WaitingFor::TargetSelection { target_slots, .. } = &runner.state().waiting_for else {
        panic!(
            "Chaotic Strike must announce its win-branch target before resolution, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(target_slots.len(), 1);
    for creature in &creatures {
        assert!(target_slots[0]
            .legal_targets
            .contains(&TargetRef::Object(*creature)));
    }
    assert!(flips(&events).is_empty());
    events.extend(
        runner
            .act(GameAction::ChooseTarget {
                target: Some(TargetRef::Object(creatures[0])),
            })
            .expect("announce chosen creature")
            .events,
    );
    if matches!(runner.state().waiting_for, WaitingFor::ManaPayment { .. }) {
        events.extend(
            runner
                .act(GameAction::PassPriority)
                .expect("pay announced spell")
                .events,
        );
    }
    assert_eq!(runner.state().stack.len(), 1);
    assert!(events.iter().any(|event| matches!(event,
        GameEvent::BecomesTarget { source_id, target, .. }
            if *source_id == spell && *target == TargetRef::Object(creatures[0]))));
    finish(&mut runner, &mut events);
    assert_eq!(flips(&events), vec![true]);
    assert_finished(&runner);
}

#[test]
fn chaotic_strike_cannot_be_cast_without_a_legal_target() {
    let (mut valid, spell, creatures) = chaotic_fixture(true);
    assert!(
        castable(&valid, spell),
        "reach: legal timing, mana and a target admit the spell"
    );
    let outcome = valid.cast(spell).target_object(creatures[0]).resolve();
    assert_eq!(flips(outcome.events()), vec![true]);
    outcome.assert_hand_drawn(P0, 1);
    assert_finished(&valid);

    let (mut empty, spell, _) = chaotic_fixture(false);
    let offered = castable(&empty, spell);
    let card_id = empty.state().objects[&spell].card_id;
    let result = empty.act(GameAction::CastSpell {
        object_id: spell,
        card_id,
        targets: vec![],
        payment_mode: CastPaymentMode::Auto,
    });
    // CR 601.2c: even a contingent win target must exist to cast the spell.
    assert!(
        !offered && result.is_err(),
        "missing win-branch target must refuse casting: offered={offered}, result={:?}",
        result.as_ref().map(|result| &result.waiting_for)
    );
    assert_eq!(empty.state().objects[&spell].zone, Zone::Hand);
    assert_eq!(pool(&empty), 2);
    assert_finished(&empty);
}

fn two_branch_fixture(
    own_pool: bool,
    opponent_pool: bool,
) -> (GameRunner, ObjectId, Vec<ObjectId>, Vec<ObjectId>) {
    // SHAPE eligibility: both supported Destroy leaves belong to this one coin.
    let parsed = parse_oracle_text(
        TWO_BRANCHES,
        "Synthetic Two-Branch Scales",
        &[],
        &["Artifact".into()],
        &[],
    );
    assert_eq!(parsed.abilities.len(), 1);
    let root = &parsed.abilities[0];
    let Effect::FlipCoins {
        flips: run,
        win_effect: Some(win),
        lose_effect: Some(lose),
        ..
    } = root.effect.as_ref()
    else {
        panic!("required supported two-branch fixture: {root:#?}");
    };
    assert_eq!(*run, CoinFlipRun::single());
    for branch in [win, lose] {
        assert!(matches!(branch.effect.as_ref(), Effect::Destroy { .. }));
        assert!(branch.unless_pay.is_none());
        assert!(branch.sub_ability.is_none());
    }
    assert!(root.sub_ability.is_none());

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_artifact_from_oracle(P0, "Synthetic Two-Branch Scales", TWO_BRANCHES)
        .id();
    let own = if own_pool {
        vec![
            scenario.add_creature(P0, "Chosen Own", 2, 2).id(),
            scenario.add_creature(P0, "Own Distractor", 3, 3).id(),
        ]
    } else {
        vec![]
    };
    let opposing = if opponent_pool {
        vec![
            scenario.add_creature(P1, "Chosen Opponent", 4, 4).id(),
            scenario.add_creature(P1, "Opponent Distractor", 5, 5).id(),
        ]
    } else {
        vec![]
    };
    mana(&mut scenario, 4);
    let mut runner = scenario.build();
    pin_outcomes(&mut runner, &[true]);
    (runner, source, own, opposing)
}

#[test]
fn two_branch_activation_announces_both_targets_before_flipping() {
    let (mut runner, source, own, opposing) = two_branch_fixture(true, true);
    let mut events = runner
        .act(GameAction::ActivateAbility {
            source_id: source,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("activation reaches announcement")
        .events;
    // CR 601.2c + CR 602.2b: both branches declare before commitment and RNG.
    let WaitingFor::TargetSelection { target_slots, .. } = &runner.state().waiting_for else {
        panic!(
            "both coin-branch targets must be announced on activation, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(
        target_slots.len(),
        2,
        "one independently declared occurrence per outcome"
    );
    for creature in &opposing {
        assert!(target_slots[0]
            .legal_targets
            .contains(&TargetRef::Object(*creature)));
        assert!(!target_slots[1]
            .legal_targets
            .contains(&TargetRef::Object(*creature)));
    }
    for creature in &own {
        assert!(target_slots[1]
            .legal_targets
            .contains(&TargetRef::Object(*creature)));
        assert!(!target_slots[0]
            .legal_targets
            .contains(&TargetRef::Object(*creature)));
    }
    assert!(flips(&events).is_empty());
    assert_eq!(pool(&runner), 4);
    assert!(!runner.state().objects[&source].tapped);
    for target in [opposing[0], own[0]] {
        events.extend(
            runner
                .act(GameAction::ChooseTarget {
                    target: Some(TargetRef::Object(target)),
                })
                .expect("announce branch target")
                .events,
        );
    }
    if matches!(runner.state().waiting_for, WaitingFor::ManaPayment { .. }) {
        events.extend(
            runner
                .act(GameAction::PassPriority)
                .expect("pay announced activation")
                .events,
        );
    }
    assert_eq!(runner.state().stack.len(), 1);
    assert_eq!(pool(&runner), 0);
    assert!(runner.state().objects[&source].tapped);
    for chosen in [opposing[0], own[0]] {
        assert!(
            events.iter().any(|event| matches!(event,
            GameEvent::BecomesTarget { source_id, target, .. }
                if *source_id == source && *target == TargetRef::Object(chosen))),
            "both announced occurrences emit BecomesTarget"
        );
    }
    finish(&mut runner, &mut events);
    assert_eq!(flips(&events), vec![true]);
    assert_finished(&runner);
}

fn refuses_missing_pool(own_pool: bool, opponent_pool: bool) {
    let (mut valid, source, own, opposing) = two_branch_fixture(true, true);
    assert!(
        can_activate_ability_now(valid.state(), P0, source, 0),
        "reach: both pools admit this activation"
    );
    let outcome = valid
        .activate(source, 0)
        .target_objects(&[opposing[0], own[0]])
        .resolve();
    assert_eq!(
        flips(outcome.events()),
        vec![true],
        "reach: the same supported activation resolves"
    );
    assert_eq!(pool(&valid), 0);
    assert!(valid.state().objects[&source].tapped);
    assert_finished(&valid);

    let (mut missing, source, _, _) = two_branch_fixture(own_pool, opponent_pool);
    let offered = can_activate_ability_now(missing.state(), P0, source, 0);
    let result = missing.act(GameAction::ActivateAbility {
        source_id: source,
        ability_index: 0,
        payment_mode: CastPaymentMode::Auto,
    });
    // CR 601.2c + CR 602.2b: neither mandatory target pool may be absent.
    assert!(!offered && result.is_err(), "missing branch pool must refuse before commitment: own_pool={own_pool}, opponent_pool={opponent_pool}, offered={offered}, result={:?}", result.as_ref().map(|result| &result.waiting_for));
    assert_eq!(pool(&missing), 4);
    assert!(!missing.state().objects[&source].tapped);
    assert_finished(&missing);
}

#[test]
fn two_branch_activation_requires_an_own_creature() {
    refuses_missing_pool(false, true);
}

#[test]
fn two_branch_activation_requires_an_opponents_creature() {
    refuses_missing_pool(true, false);
}

#[test]
fn two_branch_activation_requires_both_creature_pools() {
    refuses_missing_pool(false, false);
}

#[test]
fn two_branch_win_destroys_only_the_announced_opponents_creature() {
    let (mut runner, source, own, opposing) = two_branch_fixture(true, true);
    let outcome = runner
        .activate(source, 0)
        .target_objects(&[opposing[0], own[0]])
        .resolve();
    assert_eq!(
        flips(outcome.events()),
        vec![true],
        "reach: forced win actually resolves"
    );
    // CR 608.2c: only the selected outcome destroys its independently bound target.
    assert_eq!(
        outcome.zone_of(opposing[0]),
        Zone::Graveyard,
        "won branch must destroy the announced opponent target"
    );
    for untouched in [own[0], own[1], opposing[1], source] {
        assert_eq!(outcome.zone_of(untouched), Zone::Battlefield);
    }
    assert_eq!(coin_terminals(outcome.events(), source), 1);
    assert_eq!(pool(&runner), 0);
    assert!(runner.state().objects[&source].tapped);
    assert_finished(&runner);
}

#[test]
fn coin_branch_stack_persistence_is_strict_and_current_bindings_execute() {
    use engine::types::resolution::ResolutionStateWire;
    let (mut runner, source, own, opposing) = two_branch_fixture(true, true);
    runner
        .act(GameAction::ActivateAbility {
            source_id: source,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("activation reaches announcement");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::TargetSelection { .. }
    ));
    for target in [opposing[0], own[0]] {
        runner
            .act(GameAction::ChooseTarget {
                target: Some(TargetRef::Object(target)),
            })
            .expect("announce branch target");
    }
    if matches!(runner.state().waiting_for, WaitingFor::ManaPayment { .. }) {
        runner
            .act(GameAction::PassPriority)
            .expect("pay activation");
    }
    assert_eq!(
        runner.state().stack.len(),
        1,
        "reach: bound ability is on stack"
    );
    let current =
        serde_json::to_value(ResolutionStateWire::from_game_state(runner.state().clone())).unwrap();
    let before = runner.state().clone();
    for version in 1..=10 {
        let mut malformed = current.clone();
        malformed["stack"][0]["kind"]["data"]["ability"]
            .as_object_mut()
            .expect("actual activated stack ability")
            .remove("coin_branches")
            .expect("reach: bound stack carrier");
        malformed["resolution_state_version"] = serde_json::json!(version);
        if version == 1 {
            malformed
                .as_object_mut()
                .unwrap()
                .remove("resolution_frames");
        }
        let error = serde_json::from_value::<ResolutionStateWire>(malformed)
            .expect_err("missing stack binding is never reconstructed")
            .to_string();
        let expected = if version <= 9 {
            "RESTORE_INCOMPATIBLE_COIN_BRANCH_BINDINGS:"
        } else {
            "INVALID_COIN_BRANCH_BINDINGS:"
        };
        assert!(error.contains(expected), "v{version}: {error}");
        assert_eq!(
            *runner.state(),
            before,
            "failed restore leaves live state unchanged"
        );
    }
    *runner.state_mut() = serde_json::from_value::<ResolutionStateWire>(current)
        .unwrap()
        .into_game_state();
    let mut events = Vec::new();
    finish(&mut runner, &mut events);
    assert_eq!(flips(&events), vec![true]);
    assert_eq!(runner.state().objects[&opposing[0]].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&own[0]].zone, Zone::Battlefield);
    assert_eq!(runner.state().objects[&opposing[1]].zone, Zone::Battlefield);
    assert_finished(&runner);
}

#[test]
fn text_substitution_reaches_both_live_coin_branch_filters() {
    use engine::types::mana::ManaColor;
    // Synthetic supported pump grammar; neither real pump card has a color restriction.
    const COIN: &str = "Flip a coin. If you win the flip, target nonred creature gets +1/+1 until end of turn. If you lose the flip, target nonred creature gets +2/+2 until end of turn.";
    const CHANGE: &str = "Change the text of target spell or permanent by replacing all instances of one color word with another. (For example, you may change \"nonblack creature\" to \"nongreen creature\" or \"black mana\" to \"green mana.\")";
    for won in [true, false] {
        for changed in [false, true] {
            for chosen_color in [ManaColor::Blue, ManaColor::Green] {
                let mut scenario = GameScenario::new();
                scenario.at_phase(Phase::PreCombatMain);
                let chosen = scenario
                    .add_creature(P0, "Chosen", 2, 2)
                    .with_color(vec![chosen_color])
                    .id();
                let distractor = scenario
                    .add_creature(P1, "Distractor", 4, 4)
                    .with_color(vec![ManaColor::Green])
                    .id();
                let spell = scenario
                    .add_spell_to_hand_from_oracle(P0, "Synthetic Coin Pump", true, COIN)
                    .id();
                let change = scenario
                    .add_spell_to_hand_from_oracle(P1, "Synthetic Word Change", true, CHANGE)
                    .id();
                scenario.with_mana_pool(
                    P1,
                    vec![ManaUnit::new(ManaType::Blue, ObjectId(0), false, vec![])],
                );
                let mut runner = scenario.build();
                pin_outcomes(&mut runner, &[won]);
                let mut committed = runner
                    .cast(spell)
                    .target_objects(&[chosen, chosen])
                    .commit();
                let outcome = if changed {
                    committed
                        .act(GameAction::PassPriority)
                        .expect("P0 passes to responder");
                    committed
                        .cast(change)
                        .target_object(spell)
                        .choose_option("Red -> Blue")
                        .commit()
                        .resolve()
                } else {
                    committed.resolve()
                };
                let illegal = changed && chosen_color == ManaColor::Blue;
                assert_eq!(
                    flips(outcome.events()),
                    if illegal { vec![] } else { vec![won] },
                    "reach: legal branches flip exactly once; all-illegal branches do not flip"
                );
                let expected = if illegal {
                    2
                } else if won {
                    3
                } else {
                    4
                };
                let chosen_object = &runner.state().objects[&chosen];
                assert_eq!(
                    (
                        chosen_object.power.unwrap(),
                        chosen_object.toughness.unwrap()
                    ),
                    (expected, expected),
                    "won={won}, changed={changed}, color={chosen_color:?}"
                );
                let other = &runner.state().objects[&distractor];
                assert_eq!((other.power.unwrap(), other.toughness.unwrap()), (4, 4));
                assert_finished(&runner);
            }
        }
    }
}

#[test]
fn installed_coin_replacements_round_trip_and_execute_the_selected_bound_branch() {
    use engine::types::ability::{AbilityDefinition, AbilityKind, QuantityExpr};
    use engine::types::card_type::CoreType;
    use engine::types::identifiers::CardId;
    use engine::types::replacements::ReplacementEvent;
    use engine::types::resolution::ResolutionStateWire;

    for family in [ReplacementEvent::Draw, ReplacementEvent::Planeswalk] {
        // None is the non-coin substitute control; both coin outcomes are pinned.
        for outcome in [Some(true), Some(false), None] {
            let gain = |amount| Effect::GainLife {
                amount: QuantityExpr::Fixed { value: amount },
                player: TargetFilter::Controller,
            };
            let substitute = match outcome {
                Some(_) => {
                    let mut won = AbilityDefinition::new(AbilityKind::Spell, gain(3));
                    won.description = Some("Synthetic replacement: won".into());
                    let mut lost = AbilityDefinition::new(AbilityKind::Spell, gain(7));
                    lost.description = Some("Synthetic replacement: lost".into());
                    Effect::FlipCoins {
                        flips: CoinFlipRun::single(),
                        win_effect: Some(Box::new(won)),
                        lose_effect: Some(Box::new(lost)),
                        flipper: TargetFilter::Controller,
                    }
                }
                None => gain(11),
            };
            let install = match family {
                ReplacementEvent::Draw => Effect::CreateDrawReplacement {
                    replacement_effect: Box::new(substitute),
                },
                ReplacementEvent::Planeswalk => Effect::CreatePlaneswalkReplacement {
                    replacement_effect: Box::new(substitute),
                },
                _ => unreachable!(),
            };
            let mut scenario = GameScenario::new();
            scenario.at_phase(Phase::PreCombatMain);
            let top = scenario.add_card_to_library_top(P0, "Library control");
            let installer = scenario
                .add_spell_to_hand(P0, "Synthetic replacement installer", true)
                .with_ability(install)
                .id();
            let draw = scenario
                .add_spell_to_hand(P0, "Synthetic draw producer", true)
                .with_ability(Effect::Draw {
                    count: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::Controller,
                })
                .id();
            let mut runner = scenario.build();
            if family == ReplacementEvent::Planeswalk {
                let state = runner.state_mut();
                state.format_config = engine::types::format::FormatConfig::planechase();
                let plane = engine::game::zones::create_object(
                    state,
                    CardId(90_032),
                    P0,
                    "Synthetic active plane".into(),
                    Zone::Command,
                );
                state.objects.get_mut(&plane).unwrap().card_types.core_types =
                    vec![CoreType::Plane];
                state.command_zone.push_back(plane);
                state.planar_controller = Some(P0);
            }
            runner.cast(installer).resolve();
            assert_finished(&runner);
            assert_eq!(
                runner.state().pending_damage_replacements.len(),
                1,
                "reach: installed shield"
            );
            let runtime = runner.state().pending_damage_replacements[0]
                .runtime_execute
                .as_ref()
                .unwrap();
            assert_eq!(
                runtime.coin_branches.is_some(),
                outcome.is_some(),
                "reach: constructor materialized both outcomes"
            );
            let wire =
                serde_json::to_value(ResolutionStateWire::from_game_state(runner.state().clone()))
                    .unwrap();
            *runner.state_mut() = serde_json::from_value::<ResolutionStateWire>(wire)
                .unwrap()
                .into_game_state();
            let life = runner.state().players[0].life;
            let mut events = Vec::new();
            if family == ReplacementEvent::Draw {
                pin_outcomes(&mut runner, &[outcome.unwrap_or(true)]);
                events.extend_from_slice(runner.cast(draw).resolve().events());
                assert_eq!(
                    runner.state().objects[&top].zone,
                    Zone::Library,
                    "replaced draw did not draw"
                );
            } else {
                let seed = (0..100_000)
                    .find(|&seed| {
                        let mut rng = GameRng::seed_from_u64(seed);
                        rng.draw(RandomDraw::Outcome).random_range(1..=6) == 1
                            && outcome.is_none_or(|won| {
                                rng.draw(RandomDraw::Outcome).random_bool(0.5) == won
                            })
                    })
                    .expect("seed pins planar symbol followed by coin outcome");
                let state = runner.state_mut();
                state.rng_seed = seed;
                state.rng_word_pos = 0;
                state.rng = GameRng::seed_from_u64(seed);
                events.extend(
                    runner
                        .act(GameAction::RollPlanarDie)
                        .expect("roll planar die through production action")
                        .events,
                );
                assert!(
                    events.iter().any(|event| matches!(
                        event,
                        GameEvent::PlanarDieRolled {
                            face: engine::game::planechase::PlanarDieFace::Planeswalk,
                            ..
                        }
                    )),
                    "reach: actual planeswalking roll"
                );
                finish(&mut runner, &mut events);
            }
            assert_eq!(
                flips(&events),
                outcome.into_iter().collect::<Vec<_>>(),
                "exact coin event, absent for non-coin control"
            );
            assert_eq!(
                runner.state().players[0].life,
                life + match outcome {
                    Some(true) => 3,
                    Some(false) => 7,
                    None => 11,
                },
                "{family:?}: selected substitute payoff"
            );
            assert_finished(&runner);
        }
    }
}

#[test]
fn delayed_parent_referent_never_reads_an_unselected_coin_siblings_declaration() {
    use engine::types::ability::{
        AbilityDefinition, AbilityKind, DelayedTriggerCondition, QuantityExpr, TypedFilter,
    };
    use engine::types::counter::CounterType;

    for has_ancestor in [false, true] {
        let counter = |target| Effect::PutCounter {
            counter_type: CounterType::Plus1Plus1,
            count: QuantityExpr::Fixed { value: 1 },
            target,
        };
        let creature = TargetFilter::Typed(TypedFilter::creature());
        let coin = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::FlipCoins {
                flips: CoinFlipRun::single(),
                win_effect: Some(Box::new(AbilityDefinition::new(
                    AbilityKind::Spell,
                    Effect::CreateDelayedTrigger {
                        condition: DelayedTriggerCondition::AtNextPhase { phase: Phase::End },
                        effect: Box::new(AbilityDefinition::new(
                            AbilityKind::Spell,
                            counter(TargetFilter::ParentTarget),
                        )),
                        uses_tracked_set: false,
                    },
                ))),
                lose_effect: Some(Box::new(AbilityDefinition::new(
                    AbilityKind::Spell,
                    counter(creature.clone()),
                ))),
                flipper: TargetFilter::Controller,
            },
        );
        let definition = if has_ancestor {
            AbilityDefinition::new(AbilityKind::Spell, Effect::TargetOnly { target: creature })
                .sub_ability(coin)
        } else {
            coin
        };
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PostCombatMain);
        let ancestor = scenario
            .add_creature(P0, "Earlier chosen creature", 2, 2)
            .id();
        let sibling = scenario
            .add_creature(P1, "Unselected sibling target", 2, 2)
            .id();
        let spell = scenario
            .add_spell_to_hand(P0, "Synthetic lexical coin probe", true)
            .with_ability_definition(definition)
            .id();
        let mut runner = scenario.build();
        pin_outcomes(&mut runner, &[true]);
        let targets = if has_ancestor {
            vec![ancestor, sibling]
        } else {
            vec![sibling]
        };
        let outcome = runner.cast(spell).target_objects(&targets).resolve();
        assert_eq!(
            flips(outcome.events()),
            vec![true],
            "reach: win creates delayed instruction, loss target remains unselected"
        );
        assert_finished(&runner);
        runner.advance_to_end_step();
        assert_eq!(
            runner.state().phase,
            Phase::End,
            "reach: actual next end step"
        );
        finish(&mut runner, &mut Vec::new());
        // CR 608.2c: the delayed clause's anaphor reads its antecedent, not an
        // object announced solely for the other random outcome.
        assert_eq!(
            runner.state().objects[&sibling]
                .counters
                .get(&CounterType::Plus1Plus1)
                .copied()
                .unwrap_or(0),
            0
        );
        assert_eq!(
            runner.state().objects[&ancestor]
                .counters
                .get(&CounterType::Plus1Plus1)
                .copied()
                .unwrap_or(0),
            u32::from(has_ancestor),
            "positive control: genuine incoming antecedent remains bound"
        );
        assert_finished(&runner);
    }
}

// Synthetic building-block fixtures: independent B must remain the antecedent
// after validation, coin selection, detachment, and trigger placement.
#[derive(Clone, Copy)]
enum EmptyCoinRider {
    Counter,
    BecomeBlocked,
    Replacement,
}

fn empty_coin_binding_scenario(
    remove_b: bool,
    attached: bool,
    nested: bool,
    keep_restore: bool,
    rider: EmptyCoinRider,
) {
    use engine::game::combat::{AttackTarget, AttackerInfo, CombatState};
    use engine::game::game_object::AttachTarget;
    use engine::types::ability::{
        AbilityDefinition, AbilityKind, DelayedTriggerCondition, PreventionAmount, QuantityExpr,
        ReplacementDefinition, TypedFilter,
    };
    use engine::types::counter::CounterType;
    use engine::types::replacements::ReplacementEvent;
    use engine::types::resolution::ResolutionStateWire;

    let blocked = matches!(rider, EmptyCoinRider::BecomeBlocked);
    let phase = if blocked {
        Phase::EndCombat
    } else {
        Phase::End
    };
    let payoff = match rider {
        EmptyCoinRider::Counter => Effect::PutCounter {
            counter_type: CounterType::Plus1Plus1,
            count: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::ParentTarget,
        },
        EmptyCoinRider::BecomeBlocked => Effect::BecomeBlocked {
            target: TargetFilter::ParentTarget,
        },
        EmptyCoinRider::Replacement => Effect::AddTargetReplacement {
            replacement: Box::new(
                ReplacementDefinition::new(ReplacementEvent::DamageDone)
                    .prevention_shield(PreventionAmount::All)
                    .valid_card(TargetFilter::SelfRef),
            ),
            target: TargetFilter::ParentTarget,
        },
    };
    // Replacement's target is intentionally hidden from the ordinary effect
    // filter; the reference-only head makes the delayed binding explicit.
    let payoff = if matches!(rider, EmptyCoinRider::Replacement) {
        AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::TargetOnly {
                target: TargetFilter::ParentTarget,
            },
        )
        .sub_ability(AbilityDefinition::new(AbilityKind::Spell, payoff))
    } else {
        AbilityDefinition::new(AbilityKind::Spell, payoff)
    };
    let delayed = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::CreateDelayedTrigger {
            condition: DelayedTriggerCondition::AtNextPhase { phase },
            effect: Box::new(payoff),
            uses_tracked_set: false,
        },
    );
    let flip = |win| {
        AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::FlipCoins {
                flips: CoinFlipRun::single(),
                win_effect: Some(Box::new(win)),
                lose_effect: None,
                flipper: TargetFilter::Controller,
            },
        )
    };
    let creature = TargetFilter::Typed(TypedFilter::creature());
    let branch = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::TargetOnly {
            target: creature.clone(),
        },
    )
    .sub_ability(if nested { flip(delayed) } else { delayed });
    let coin = flip(branch).sub_ability(AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::GainLife {
            amount: QuantityExpr::Fixed { value: 2 },
            player: TargetFilter::Controller,
        },
    ));
    let definition = AbilityDefinition::new(
        if attached {
            AbilityKind::Activated
        } else {
            AbilityKind::Spell
        },
        Effect::TargetOnly { target: creature },
    )
    .sub_ability(coin);
    let mut scenario = GameScenario::new();
    scenario.at_phase(if blocked {
        Phase::CombatDamage
    } else {
        Phase::PostCombatMain
    });
    let a = scenario
        .add_creature(P0, "Earlier outer declaration", 2, 20)
        .id();
    let b = scenario
        .add_creature(P0, "Selected branch declaration", 2, 20)
        .id();
    let h = scenario
        .add_creature(P0, "Hostile attached host", 2, 20)
        .id();
    let source = if attached {
        scenario
            .add_artifact_from_oracle(P0, "Synthetic Equipment binding", "")
            .with_subtypes(vec!["Equipment"])
            .with_ability_definition(definition)
            .id()
    } else {
        scenario
            .add_spell_to_hand(P0, "Synthetic delayed coin binding", true)
            .with_ability_definition(definition)
            .id()
    };
    let response = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Synthetic exile response",
            true,
            "Exile target creature.",
        )
        .with_mana_cost(ManaCost::zero())
        .id();
    if keep_restore {
        scenario.add_artifact_from_oracle(
            P0,
            "Krark's Thumb",
            "If you would flip a coin, instead flip two coins and ignore one.",
        );
    }
    let mut runner = scenario.build();
    if attached {
        let state = runner.state_mut();
        state.objects.get_mut(&source).unwrap().attached_to = Some(AttachTarget::Object(h));
        state.objects.get_mut(&h).unwrap().attachments.push(source);
        state.layers_dirty.mark_full();
    }
    if blocked {
        let mut combat = CombatState::default();
        for id in [a, b, h] {
            combat
                .attackers
                .push(AttackerInfo::new(id, AttackTarget::Player(P1), P1));
        }
        runner.state_mut().combat = Some(combat);
    }
    pin_outcomes(
        &mut runner,
        if keep_restore {
            if nested {
                &[true, false, true, false]
            } else {
                &[true, false]
            }
        } else if nested {
            &[true, true]
        } else {
            &[true]
        },
    );
    let life = runner.life(P0);
    if attached {
        runner
            .act(GameAction::ActivateAbility {
                source_id: source,
                ability_index: 0,
                payment_mode: CastPaymentMode::Auto,
            })
            .unwrap();
        for target in [a, b] {
            runner
                .act(GameAction::ChooseTarget {
                    target: Some(TargetRef::Object(target)),
                })
                .unwrap();
        }
    } else {
        runner.cast(source).target_objects(&[a, b]).commit();
    }
    assert_eq!(runner.state().stack.len(), 1, "reach: A and B announced");
    if remove_b {
        runner.cast(response).target_object(b).commit();
    }
    let mut events = Vec::new();
    if keep_restore {
        pass_to_coin_keep(&mut runner, &mut events);
        if nested {
            events.extend(
                runner
                    .act(GameAction::SelectCoinFlips {
                        keep_indices: vec![0],
                    })
                    .unwrap()
                    .events,
            );
            assert_eq!(flips(&events), vec![true], "reach: outer win selected");
            pass_to_coin_keep(&mut runner, &mut events);
        }
        assert!(
            runner.state().active_coin_flip_frame().is_some(),
            "reach: parked coin"
        );
        if nested {
            let incoming = &runner
                .state()
                .active_coin_flip_frame()
                .unwrap()
                .branch_context;
            assert!(incoming.chosen_players.is_empty());
            assert!(incoming.forwarded_result_context.is_none());
            if remove_b {
                assert!(incoming.parent_occurrences.targets.is_empty());
                assert!(incoming.parent_occurrences.pins.is_empty());
                assert_eq!(
                    serde_json::to_value(incoming).unwrap()["parent_target_missing_reason"],
                    serde_json::json!({"IllegalAnnouncement": {"origin": "ParentAnnouncement"}}),
                    "reach: inner prompt carries only the unavailable incoming announcement"
                );
            } else {
                assert_eq!(
                    incoming.parent_occurrences.targets,
                    vec![TargetRef::Object(b)]
                );
                assert_eq!(incoming.parent_occurrences.pins.len(), 1);
                assert!(incoming.parent_occurrences.pins[0].is_some());
            }
        }
        runner.state_mut().capture_rng_word_pos();
        let wire =
            serde_json::to_value(ResolutionStateWire::from_game_state(runner.state().clone()))
                .unwrap();
        *runner.state_mut() = serde_json::from_value::<ResolutionStateWire>(wire)
            .unwrap()
            .into_game_state();
        if nested && remove_b {
            assert_eq!(
                serde_json::to_value(
                    &runner
                        .state()
                        .active_coin_flip_frame()
                        .unwrap()
                        .branch_context
                )
                .unwrap()["parent_target_missing_reason"],
                serde_json::json!({"IllegalAnnouncement": {"origin": "ParentAnnouncement"}}),
                "restored missing-only incoming binding must not become unassigned"
            );
        }
        events.extend(
            runner
                .act(GameAction::SelectCoinFlips {
                    keep_indices: vec![0],
                })
                .unwrap()
                .events,
        );
    }
    finish(&mut runner, &mut events);
    assert_eq!(
        flips(&events),
        if nested { vec![true, true] } else { vec![true] }
    );
    assert_eq!(runner.life(P0), life + 2, "reach: independent tail");
    assert!(
        events.iter().any(|event| matches!(
            event,
            GameEvent::EffectResolved {
                kind: EffectKind::CreateDelayedTrigger,
                ..
            }
        )),
        "reach: delayed instruction executed"
    );
    assert_eq!(
        runner.state().delayed_triggers.len(),
        1,
        "reach: delayed installation"
    );
    assert_eq!(runner.state().objects[&a].zone, Zone::Battlefield);
    assert_eq!(
        runner.state().objects[&b].zone,
        if remove_b {
            Zone::Exile
        } else {
            Zone::Battlefield
        }
    );
    if attached {
        assert_eq!(runner.state().objects[&source].zone, Zone::Battlefield);
        assert_eq!(
            runner.state().objects[&source].attached_to,
            Some(AttachTarget::Object(h)),
            "reach: distinct hostile host remains attached at installation"
        );
    }
    assert_finished(&runner);
    for _ in 0..12 {
        if !runner.state().stack.is_empty() {
            break;
        }
        runner.act(GameAction::PassPriority).unwrap();
    }
    assert_eq!(runner.state().phase, phase, "reach: firing phase");
    if attached {
        let placed = runner
            .state()
            .stack
            .back()
            .expect("reach: delayed stack entry")
            .ability()
            .unwrap();
        assert_eq!(
            placed.trigger_source.as_ref().unwrap().attached_to,
            Some(AttachTarget::Object(h)),
            "reach: engine captured attached-source context"
        );
    }
    let stack_binding = runner
        .state()
        .stack
        .back()
        .expect("reach: delayed trigger placed")
        .ability()
        .unwrap()
        .targets
        .clone();
    let expected = if remove_b {
        vec![]
    } else {
        vec![TargetRef::Object(b)]
    };
    // Kept separate from payoff assertions: seeding and effect-time fallback
    // are independent boundaries.
    assert_eq!(
        stack_binding, expected,
        "delayed stack binding; illegal B={remove_b}"
    );
    let mut payoff_events = Vec::new();
    finish(&mut runner, &mut payoff_events);
    for id in [a, b, h] {
        let selected = id == b && !remove_b;
        match rider {
            EmptyCoinRider::Counter => assert_eq!(
                runner.state().objects[&id]
                    .counters
                    .get(&CounterType::Plus1Plus1)
                    .copied()
                    .unwrap_or(0),
                u32::from(selected),
                "counter payoff object={id:?}; illegal B={remove_b}",
            ),
            EmptyCoinRider::BecomeBlocked => {
                let became_blocked = payoff_events.iter().any(|event| matches!(
                    event, GameEvent::AttackerBecameBlockedByEffect { attacker } if *attacker == id
                ));
                assert_eq!(
                    became_blocked, selected,
                    "blocked payoff object={id:?}; illegal B={remove_b}"
                );
            }
            EmptyCoinRider::Replacement => assert_eq!(
                runner.state().objects[&id].replacement_definitions.len(),
                usize::from(selected),
                "replacement payoff object={id:?}; illegal B={remove_b}",
            ),
        }
    }
    assert!(
        runner.state().delayed_triggers.is_empty(),
        "one-shot trigger consumed"
    );
    assert_finished(&runner);
}

#[test]
fn delayed_coin_branch_keeps_its_empty_illegal_target_binding() {
    for remove_b in [false, true] {
        empty_coin_binding_scenario(remove_b, false, false, false, EmptyCoinRider::Counter);
    }
}

#[test]
fn delayed_coin_branch_empty_binding_is_not_reseeded_from_attached_host() {
    for remove_b in [false, true] {
        empty_coin_binding_scenario(remove_b, true, false, false, EmptyCoinRider::Counter);
    }
}

#[test]
fn delayed_coin_branch_empty_binding_survives_keep_restore() {
    for remove_b in [false, true] {
        empty_coin_binding_scenario(remove_b, false, false, true, EmptyCoinRider::Counter);
    }
}

#[test]
fn nested_coin_inherits_an_unavailable_announced_binding() {
    for remove_b in [false, true] {
        empty_coin_binding_scenario(remove_b, false, true, false, EmptyCoinRider::Counter);
    }
}

#[test]
fn nested_coin_missing_only_context_survives_inner_keep_restore() {
    for remove_b in [true, false] {
        empty_coin_binding_scenario(remove_b, false, true, true, EmptyCoinRider::Counter);
    }
}

#[test]
fn detached_become_blocked_does_not_replace_an_illegal_announcement_with_its_host() {
    for remove_b in [false, true] {
        empty_coin_binding_scenario(remove_b, true, false, false, EmptyCoinRider::BecomeBlocked);
    }
}

#[test]
fn detached_target_replacement_does_not_replace_an_illegal_announcement_with_its_host() {
    for remove_b in [false, true] {
        empty_coin_binding_scenario(remove_b, true, false, false, EmptyCoinRider::Replacement);
    }
}

fn pass_to_coin_keep(runner: &mut GameRunner, events: &mut Vec<GameEvent>) {
    for _ in 0..20 {
        if !matches!(runner.state().waiting_for, WaitingFor::Priority { .. }) {
            break;
        }
        assert!(
            !runner.state().stack.is_empty(),
            "coin reaches its keep prompt"
        );
        events.extend(runner.act(GameAction::PassPriority).unwrap().events);
    }
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::CoinFlipKeepChoice { player: P0, .. }
        ),
        "required keep prompt, got {:?}",
        runner.state().waiting_for
    );
}

#[test]
fn thumb_restore_keeps_each_declared_branch_occurrence_and_refuses_pin_drift() {
    use engine::types::ability::{AbilityDefinition, AbilityKind};
    use engine::types::identifiers::ObjectIncarnationRef;
    use engine::types::resolution::ResolutionStateWire;

    for won in [false, true] {
        for stale in [false, true] {
            let mut scenario = GameScenario::new();
            scenario.at_phase(Phase::PreCombatMain);
            scenario.add_artifact_from_oracle(
                P0,
                "Krark's Thumb",
                "If you would flip a coin, instead flip two coins and ignore one.",
            );
            let own = scenario
                .add_creature(P0, "Own announced creature", 2, 2)
                .id();
            let opposing = scenario
                .add_creature(P1, "Opponent announced creature", 3, 3)
                .id();
            let distractor = scenario.add_creature(P1, "Never announced", 4, 4).id();
            let parsed = parse_oracle_text(
                TWO_BRANCHES,
                "Synthetic Scales",
                &[],
                &["Artifact".into()],
                &[],
            );
            let definition: AbilityDefinition = parsed.abilities[0].clone();
            assert_eq!(definition.kind, AbilityKind::Activated);
            let source = scenario
                .add_artifact_from_oracle(P0, "Synthetic Scales", TWO_BRANCHES)
                .id();
            mana(&mut scenario, 4);
            let mut runner = scenario.build();
            pin_outcomes(&mut runner, &[true, false]);
            let mut events = runner
                .act(GameAction::ActivateAbility {
                    source_id: source,
                    ability_index: 0,
                    payment_mode: CastPaymentMode::Auto,
                })
                .unwrap()
                .events;
            assert!(matches!(
                runner.state().waiting_for,
                WaitingFor::TargetSelection { .. }
            ));
            for target in [opposing, own] {
                events.extend(
                    runner
                        .act(GameAction::ChooseTarget {
                            target: Some(TargetRef::Object(target)),
                        })
                        .unwrap()
                        .events,
                );
            }
            if matches!(runner.state().waiting_for, WaitingFor::ManaPayment { .. }) {
                events.extend(runner.act(GameAction::PassPriority).unwrap().events);
            }
            pass_to_coin_keep(&mut runner, &mut events);
            assert!(flips(&events).is_empty(), "ignored flips have not happened");
            assert!(matches!(
                runner.state().waiting_for,
                WaitingFor::CoinFlipKeepChoice { player: P0, .. }
            ));
            let pending = runner.state().active_coin_flip_frame().unwrap();
            for (branch, target) in [
                (pending.win_effect.as_deref().unwrap(), opposing),
                (pending.lose_effect.as_deref().unwrap(), own),
            ] {
                assert_eq!(branch.controller, P0);
                assert_eq!(branch.targets, vec![TargetRef::Object(target)]);
                assert_eq!(
                    branch.target_pin_at(0),
                    Some(ObjectIncarnationRef::from_object(
                        &runner.state().objects[&target]
                    ))
                );
            }
            runner.state_mut().capture_rng_word_pos();
            let wire =
                serde_json::to_value(ResolutionStateWire::from_game_state(runner.state().clone()))
                    .unwrap();
            let mut malformed = wire.clone();
            let frame = malformed["resolution_frames"]["frames"]
                .as_array_mut()
                .unwrap()
                .iter_mut()
                .find(|frame| frame["type"] == "CoinFlip")
                .unwrap();
            let branch = &mut frame["data"]["bound_branches"]["won"];
            branch["target_pins"][0]["object_id"] = serde_json::to_value(distractor).unwrap();
            let error = serde_json::from_value::<ResolutionStateWire>(malformed)
                .unwrap_err()
                .to_string();
            assert!(error.contains("target pin"), "{error}");
            *runner.state_mut() = serde_json::from_value::<ResolutionStateWire>(wire)
                .unwrap()
                .into_game_state();
            let selected = if won { opposing } else { own };
            if stale {
                // Saved-board hostile fixture: same storage id, different object identity.
                runner
                    .state_mut()
                    .objects
                    .get_mut(&selected)
                    .unwrap()
                    .incarnation += 1;
            }
            events.extend(
                runner
                    .act(GameAction::SelectCoinFlips {
                        keep_indices: vec![usize::from(!won)],
                    })
                    .unwrap()
                    .events,
            );
            finish(&mut runner, &mut events);
            assert_eq!(flips(&events), vec![won]);
            assert_eq!(
                runner.state().objects[&selected].zone,
                if stale {
                    Zone::Battlefield
                } else {
                    Zone::Graveyard
                }
            );
            assert_eq!(
                runner.state().objects[&if won { own } else { opposing }].zone,
                Zone::Battlefield
            );
            assert_eq!(runner.state().objects[&distractor].zone, Zone::Battlefield);
            assert_finished(&runner);
        }
    }
}

#[test]
fn malformed_coin_carrier_refuses_before_rng_or_mutation_and_canonical_executes() {
    use engine::game::ability_utils::build_resolved_from_def;
    use engine::game::effects::flip_coin;
    use engine::types::ability::{
        AbilityDefinition, AbilityKind, EffectError, QuantityExpr, ResolvedAbility,
    };
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario.add_creature(P0, "Synthetic source", 2, 2).id();
    let mut runner = scenario.build();
    pin_outcomes(&mut runner, &[true, false, true]);
    let definition = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::FlipCoins {
            flips: CoinFlipRun::single(),
            win_effect: Some(Box::new(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::GainLife {
                    amount: QuantityExpr::Fixed { value: 3 },
                    player: TargetFilter::Controller,
                },
            ))),
            lose_effect: None,
            flipper: TargetFilter::Controller,
        },
    );
    let malformed = ResolvedAbility::new((*definition.effect).clone(), vec![], source, P0);
    let before = serde_json::to_value(runner.state()).unwrap();
    let mut expected_rng = runner.state().rng.clone();
    let mut events = Vec::new();
    assert_eq!(
        flip_coin::resolve(runner.state_mut(), &malformed, &mut events),
        Err(EffectError::MissingParam("resolved coin branches".into()))
    );
    assert!(events.is_empty());
    assert_eq!(serde_json::to_value(runner.state()).unwrap(), before);
    let mut actual_rng = runner.state().rng.clone();
    let expected: Vec<bool> = (0..3)
        .map(|_| expected_rng.draw(RandomDraw::Outcome).random_bool(0.5))
        .collect();
    let actual: Vec<bool> = (0..3)
        .map(|_| actual_rng.draw(RandomDraw::Outcome).random_bool(0.5))
        .collect();
    assert_eq!(actual, expected, "rejection consumes no outcome draw");
    let canonical = build_resolved_from_def(&definition, source, P0);
    flip_coin::resolve(runner.state_mut(), &canonical, &mut events).unwrap();
    assert_eq!(flips(&events), vec![true]);
    assert_eq!(runner.life(P0), 23);
    assert_finished(&runner);
}

#[test]
fn bangchuckers_shaped_loss_never_borrows_the_win_only_target() {
    use engine::types::ability::{
        AbilityDefinition, AbilityKind, DamageSource, QuantityExpr, TypedFilter,
    };
    for (won, self_damage) in [(true, false), (false, false), (false, true)] {
        let damage = |target, damage_source| Effect::DealDamage {
            amount: QuantityExpr::Fixed { value: 2 },
            target,
            damage_source,
            excess: None,
        };
        let coin = Effect::FlipCoins {
            flips: CoinFlipRun::single(),
            win_effect: Some(Box::new(AbilityDefinition::new(
                AbilityKind::Activated,
                damage(TargetFilter::Typed(TypedFilter::creature()), None),
            ))),
            lose_effect: Some(Box::new(AbilityDefinition::new(
                AbilityKind::Activated,
                damage(
                    if self_damage {
                        TargetFilter::SelfRef
                    } else {
                        TargetFilter::ParentTarget
                    },
                    if self_damage {
                        None
                    } else {
                        Some(DamageSource::Target)
                    },
                ),
            ))),
            flipper: TargetFilter::Controller,
        };
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let source = scenario
            .add_creature(P0, "Synthetic Bangchucker", 3, 5)
            .with_ability_definition(AbilityDefinition::new(AbilityKind::Activated, coin))
            .id();
        let target = scenario.add_creature(P1, "Win-only target", 4, 5).id();
        let distractor = scenario.add_creature(P1, "Distractor", 4, 5).id();
        let mut runner = scenario.build();
        pin_outcomes(&mut runner, &[won]);
        let outcome = runner.activate(source, 0).target_object(target).resolve();
        assert_eq!(flips(outcome.events()), vec![won]);
        let dealt: Vec<_> = outcome
            .events()
            .iter()
            .filter_map(|event| match event {
                GameEvent::DamageDealt {
                    source_id,
                    target,
                    amount,
                    ..
                } => Some((*source_id, target.clone(), *amount)),
                _ => None,
            })
            .collect();
        assert_eq!(
            dealt,
            if won {
                vec![(source, TargetRef::Object(target), 2)]
            } else if self_damage {
                vec![(source, TargetRef::Object(source), 2)]
            } else {
                vec![]
            }
        );
        assert_eq!(runner.state().objects[&distractor].zone, Zone::Battlefield);
        assert_finished(&runner);
    }
}

#[test]
fn repeated_bound_branch_recomputes_counter_quantity_from_a_pristine_template() {
    use engine::types::ability::{
        AbilityDefinition, AbilityKind, ObjectScope, QuantityExpr, QuantityRef, TypedFilter,
    };
    use engine::types::counter::CounterType;
    for until_loss in [false, true] {
        let coin = Effect::FlipCoins {
            flips: if until_loss {
                CoinFlipRun::UntilLoss
            } else {
                CoinFlipRun::Exactly {
                    count: QuantityExpr::Fixed { value: 3 },
                }
            },
            win_effect: Some(Box::new(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::PutCounter {
                    counter_type: CounterType::Plus1Plus1,
                    count: QuantityExpr::Ref {
                        qty: QuantityRef::CountersOn {
                            scope: ObjectScope::Target,
                            counter_type: Some(CounterType::Plus1Plus1),
                        },
                    },
                    target: TargetFilter::Typed(TypedFilter::creature()),
                },
            ))),
            lose_effect: None,
            flipper: TargetFilter::Controller,
        };
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let target = scenario.add_creature(P0, "Repeated recipient", 2, 2).id();
        scenario.with_counter(target, CounterType::Plus1Plus1, 2);
        let distractor = scenario.add_creature(P1, "Unchanged recipient", 7, 7).id();
        let spell = scenario
            .add_spell_to_hand(P0, "Synthetic repeated growing reward", true)
            .with_ability(coin)
            .id();
        let mut runner = scenario.build();
        let expected_flips = if until_loss {
            vec![true, true, true, false]
        } else {
            vec![true, true, true]
        };
        pin_outcomes(&mut runner, &expected_flips);
        let outcome = runner.cast(spell).target_object(target).resolve();
        assert_eq!(flips(outcome.events()), expected_flips);
        let counters: Vec<_> = outcome
            .events()
            .iter()
            .filter_map(|event| match event {
                GameEvent::CounterAdded {
                    object_id,
                    counter_type: CounterType::Plus1Plus1,
                    count,
                    ..
                } => {
                    assert_eq!(*object_id, target);
                    Some(*count)
                }
                _ => None,
            })
            .collect();
        assert_eq!(counters, vec![2, 4, 8]);
        #[derive(Debug, PartialEq, Eq)]
        enum RunEvent {
            Flip(bool),
            Counter(u32),
            Complete,
        }
        use RunEvent::{Complete, Counter, Flip};
        let stream: Vec<_> = outcome
            .events()
            .iter()
            .filter_map(|event| match event {
                GameEvent::CoinFlipped { won, .. } => Some(Flip(*won)),
                GameEvent::CounterAdded {
                    object_id, count, ..
                } if *object_id == target => Some(Counter(*count)),
                GameEvent::EffectResolved {
                    kind: EffectKind::FlipCoins,
                    source_id,
                    ..
                } if *source_id == spell => Some(Complete),
                _ => None,
            })
            .collect();
        assert_eq!(
            stream,
            if until_loss {
                vec![
                    Flip(true),
                    Flip(true),
                    Flip(true),
                    Flip(false),
                    Counter(2),
                    Counter(4),
                    Counter(8),
                    Complete,
                ]
            } else {
                vec![
                    Flip(true),
                    Counter(2),
                    Flip(true),
                    Counter(4),
                    Flip(true),
                    Counter(8),
                    Complete,
                ]
            }
        );
        assert_eq!(
            runner.state().objects[&target].counters[&CounterType::Plus1Plus1],
            16
        );
        assert_eq!(runner.state().objects[&distractor].power, Some(7));
        assert_eq!(coin_terminals(outcome.events(), spell), 1);
        assert_finished(&runner);
    }
}

#[test]
fn independent_coin_declaration_owns_its_pins_even_when_incoming_id_is_reused() {
    use engine::types::ability::{AbilityDefinition, AbilityKind, QuantityExpr, TypedFilter};
    use engine::types::counter::CounterType;
    use engine::types::identifiers::ObjectIncarnationRef;
    for keyed_child in [None, Some(false), Some(true)] {
        let coin = Effect::FlipCoins {
            flips: CoinFlipRun::single(),
            win_effect: Some(Box::new(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::PutCounter {
                    counter_type: CounterType::Plus1Plus1,
                    count: QuantityExpr::Fixed { value: 3 },
                    target: TargetFilter::Typed(TypedFilter::creature()),
                },
            ))),
            lose_effect: None,
            flipper: TargetFilter::Controller,
        };
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let target = scenario.add_creature(P0, "New incarnation", 2, 2).id();
        let distractor = scenario.add_creature(P1, "Unchosen", 4, 4).id();
        let spell = scenario
            .add_spell_to_hand(P0, "Synthetic independently targeted coin", true)
            .with_ability(coin)
            .id();
        let mut runner = scenario.build();
        let old = ObjectIncarnationRef::from_object(&runner.state().objects[&target]);
        runner
            .state_mut()
            .objects
            .get_mut(&target)
            .unwrap()
            .incarnation += 1;
        pin_outcomes(&mut runner, &[true]);
        drop(runner.cast(spell).target_object(target).commit());
        let current = ObjectIncarnationRef::from_object(&runner.state().objects[&target]);
        let root = runner
            .state_mut()
            .stack
            .back_mut()
            .unwrap()
            .ability_mut()
            .unwrap();
        root.set_target_incarnations_recursive(vec![old]);
        let child = root.coin_branches.as_mut().unwrap().won.as_mut().unwrap();
        assert!(
            child.target_incarnations.is_empty(),
            "incoming delayed-reference pins never stamp an independent declaration"
        );
        assert_eq!(child.target_pin_at(0), Some(current));
        if let Some(stale) = keyed_child {
            // CR 400.7 + CR 603.7c: a child's own keyed pin is still authoritative.
            child.target_incarnations = vec![if stale { old } else { current }];
        }
        let mut events = Vec::new();
        finish(&mut runner, &mut events);
        let stale = keyed_child == Some(true);
        assert_eq!(flips(&events), if stale { vec![] } else { vec![true] });
        assert_eq!(
            runner.state().objects[&target]
                .counters
                .get(&CounterType::Plus1Plus1)
                .copied()
                .unwrap_or(0),
            if stale { 0 } else { 3 }
        );
        assert!(!runner.state().objects[&distractor]
            .counters
            .contains_key(&CounterType::Plus1Plus1));
        assert_finished(&runner);
    }
}

#[test]
fn thumb_completed_empty_choice_restores_and_runs_only_independent_instructions() {
    use engine::types::ability::{
        AbilityDefinition, AbilityKind, ControllerRef, QuantityExpr, SubAbilityLink,
        TargetChoiceTiming, TypedFilter,
    };
    use engine::types::resolution::ResolutionStateWire;
    for populated in [false, true] {
        let mut life = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 5 },
                player: TargetFilter::Controller,
            },
        );
        life.sub_link = SubAbilityLink::SequentialSibling;
        let loss = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::GiveControl {
                target: TargetFilter::ParentTarget,
                recipient: TargetFilter::SpecificPlayer { id: P1 },
            },
        )
        .sub_ability(life);
        let coin = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::FlipCoins {
                flips: CoinFlipRun::single(),
                win_effect: None,
                lose_effect: Some(Box::new(loss)),
                flipper: TargetFilter::Controller,
            },
        );
        let mut choice = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::TargetOnly {
                target: TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::You)),
            },
        )
        .sub_ability(coin);
        choice.target_choice_timing = TargetChoiceTiming::Resolution;
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        scenario.add_artifact_from_oracle(
            P0,
            "Krark's Thumb",
            "If you would flip a coin, instead flip two coins and ignore one.",
        );
        let picked =
            populated.then(|| scenario.add_creature(P0, "Only eligible choice", 2, 2).id());
        let distractor = scenario.add_creature(P1, "Ineligible choice", 4, 4).id();
        let spell = scenario
            .add_spell_to_hand(P0, "Synthetic completed choice", true)
            .with_ability_definition(choice)
            .id();
        let mut runner = scenario.build();
        pin_outcomes(&mut runner, &[false, true]);
        drop(runner.cast(spell).commit());
        let mut events = Vec::new();
        pass_to_coin_keep(&mut runner, &mut events);
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::CoinFlipKeepChoice { player: P0, .. }
        ));
        let context = runner
            .state()
            .active_coin_flip_frame()
            .unwrap()
            .branch_context
            .clone();
        if populated {
            assert_eq!(
                context.parent_occurrences.targets,
                vec![TargetRef::Object(picked.unwrap())]
            );
            assert!(context.parent_occurrences.pins[0].is_some());
            assert!(context.forwarded_result_context.is_none());
        } else {
            assert!(context.parent_occurrences.targets.is_empty());
            assert!(context
                .forwarded_result_context
                .as_deref()
                .unwrap()
                .targets
                .is_empty());
        }
        runner.state_mut().capture_rng_word_pos();
        let wire =
            serde_json::to_value(ResolutionStateWire::from_game_state(runner.state().clone()))
                .unwrap();
        *runner.state_mut() = serde_json::from_value::<ResolutionStateWire>(wire)
            .unwrap()
            .into_game_state();
        assert_eq!(
            runner
                .state()
                .active_coin_flip_frame()
                .unwrap()
                .branch_context,
            context
        );
        events.extend(
            runner
                .act(GameAction::SelectCoinFlips {
                    keep_indices: vec![0],
                })
                .unwrap()
                .events,
        );
        finish(&mut runner, &mut events);
        assert_eq!(flips(&events), vec![false]);
        assert_eq!(
            runner.life(P0),
            25,
            "independent instruction runs after the reference"
        );
        if let Some(picked) = picked {
            assert_eq!(runner.state().objects[&picked].controller, P1);
            assert!(events.iter().any(|event| matches!(event,
                GameEvent::ControllerChanged { object_id, .. } if *object_id == picked)));
        } else {
            assert!(!events
                .iter()
                .any(|event| matches!(event, GameEvent::ControllerChanged { .. })));
        }
        assert_eq!(runner.state().objects[&distractor].controller, P1);
        assert_finished(&runner);
    }
}

// T2: synthetic clauses with disjoint target pools expose traversal order.
#[test]
fn coin_children_precede_a_targeted_successor_and_nested_children() {
    use engine::types::ability::{AbilityDefinition, AbilityKind};
    for nested in [false, true] {
        for won in [false, true] {
            for bulk in [false, true] {
                let mut root = parse_oracle_text(
                    TWO_BRANCHES,
                    "Synthetic ordered branches",
                    &[],
                    &["Artifact".into()],
                    &[],
                )
                .abilities
                .remove(0);
                root.cost = None;
                root.sub_ability = Some(Box::new(
                    parse_oracle_text(
                        "Exile target artifact.",
                        "Synthetic independent successor",
                        &[],
                        &["Instant".into()],
                        &[],
                    )
                    .abilities
                    .remove(0),
                ));
                if nested {
                    root = AbilityDefinition::new(
                        AbilityKind::Activated,
                        Effect::FlipCoins {
                            flips: CoinFlipRun::single(),
                            win_effect: Some(Box::new(root)),
                            lose_effect: None,
                            flipper: TargetFilter::Controller,
                        },
                    );
                }
                let mut scenario = GameScenario::new();
                scenario.at_phase(Phase::PreCombatMain);
                let source = scenario
                    .add_artifact_from_oracle(P0, "Ordered source", "")
                    .with_ability_definition(root)
                    .id();
                let own = scenario.add_creature(P0, "Own occurrence", 2, 2).id();
                let opponent = scenario.add_creature(P1, "Opponent occurrence", 3, 3).id();
                let distractor = scenario.add_creature(P1, "Creature distractor", 4, 4).id();
                let later = scenario
                    .add_artifact_from_oracle(P1, "Later artifact", "")
                    .id();
                let spare = scenario
                    .add_artifact_from_oracle(P1, "Artifact distractor", "")
                    .id();
                let mut runner = scenario.build();
                let expected = if nested { vec![true, won] } else { vec![won] };
                pin_outcomes(&mut runner, &expected);
                let events = if bulk {
                    runner
                        .act(GameAction::ActivateAbility {
                            source_id: source,
                            ability_index: 0,
                            payment_mode: CastPaymentMode::Auto,
                        })
                        .unwrap();
                    let mut events = runner
                        .act(GameAction::SelectTargets {
                            targets: [opponent, own, later].map(TargetRef::Object).to_vec(),
                        })
                        .expect("compact declaration follows Won, Lost, successor order")
                        .events;
                    finish(&mut runner, &mut events);
                    events
                } else {
                    runner
                        .activate(source, 0)
                        .target_objects(&[opponent, own, later])
                        .resolve()
                        .events()
                        .to_vec()
                };
                assert_eq!(flips(&events), expected);
                assert_eq!(
                    runner.state().objects[&opponent].zone,
                    if won {
                        Zone::Graveyard
                    } else {
                        Zone::Battlefield
                    }
                );
                assert_eq!(
                    runner.state().objects[&own].zone,
                    if won {
                        Zone::Battlefield
                    } else {
                        Zone::Graveyard
                    }
                );
                assert_eq!(runner.state().objects[&later].zone, Zone::Exile);
                assert_eq!(runner.state().objects[&spare].zone, Zone::Battlefield);
                assert_eq!(runner.state().objects[&distractor].zone, Zone::Battlefield);
                assert_eq!(
                    events
                        .iter()
                        .filter(|event| matches!(
                            event,
                            GameEvent::EffectResolved {
                                kind: EffectKind::ChangeZone,
                                ..
                            }
                        ))
                        .count(),
                    1,
                    "the independent successor executes once"
                );
                assert_finished(&runner);
            }
        }
    }
}

#[test]
fn chaotic_strike_loss_still_draws_without_pumping_either_creature() {
    let (mut runner, spell, creatures) = chaotic_fixture(true);
    pin_outcomes(&mut runner, &[false]);
    let outcome = runner.cast(spell).target_object(creatures[0]).resolve();
    assert_eq!(flips(outcome.events()), vec![false]);
    outcome.assert_hand_drawn(P0, 1);
    assert_eq!(outcome.power_toughness(creatures[0]), (2, 2));
    assert_eq!(outcome.power_toughness(creatures[1]), (4, 4));
    assert_finished(&runner);
}

// T4: remove announced targets with actual response spells, not fixture edits.
#[test]
fn illegal_coin_children_do_not_substitute_distractors_or_suppress_a_legal_tail() {
    use engine::types::ability::{AbilityDefinition, AbilityKind, QuantityExpr};
    for removed in [0, 1, 2, 3] {
        for won in [false, true] {
            let mut definition = parse_oracle_text(
                TWO_BRANCHES,
                "Synthetic legality coin",
                &[],
                &["Artifact".into()],
                &[],
            )
            .abilities
            .remove(0);
            definition.cost = None;
            definition.sub_ability = Some(Box::new(AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::GainLife {
                    amount: QuantityExpr::Fixed { value: 2 },
                    player: TargetFilter::Controller,
                },
            )));
            let mut scenario = GameScenario::new();
            scenario.at_phase(Phase::PreCombatMain);
            let source = scenario
                .add_artifact_from_oracle(P0, "Legality source", "")
                .with_ability_definition(definition)
                .id();
            let own = scenario.add_creature(P0, "Own target", 2, 2).id();
            let opponent = scenario.add_creature(P1, "Opponent target", 3, 3).id();
            let own_spare = scenario.add_creature(P0, "Own distractor", 4, 4).id();
            let opponent_spare = scenario.add_creature(P1, "Opponent distractor", 5, 5).id();
            let responses: Vec<_> = (0..2)
                .map(|_| {
                    scenario
                        .add_spell_to_hand_from_oracle(
                            P0,
                            "Synthetic removal response",
                            true,
                            "Exile target creature.",
                        )
                        .with_mana_cost(ManaCost::zero())
                        .id()
                })
                .collect();
            let mut runner = scenario.build();
            pin_outcomes(&mut runner, &[won]);
            let mut events = runner
                .act(GameAction::ActivateAbility {
                    source_id: source,
                    ability_index: 0,
                    payment_mode: CastPaymentMode::Auto,
                })
                .unwrap()
                .events;
            for target in [opponent, own] {
                events.extend(
                    runner
                        .act(GameAction::ChooseTarget {
                            target: Some(TargetRef::Object(target)),
                        })
                        .unwrap()
                        .events,
                );
            }
            assert_eq!(
                runner.state().stack.len(),
                1,
                "reach: both targets announced"
            );
            for (bit, response, target) in [(1, responses[0], opponent), (2, responses[1], own)] {
                if removed & bit != 0 {
                    runner.cast(response).target_object(target).commit();
                    for _ in 0..3 {
                        if !runner
                            .state()
                            .stack
                            .iter()
                            .any(|entry| entry.id == response)
                        {
                            break;
                        }
                        events.extend(runner.act(GameAction::PassPriority).unwrap().events);
                    }
                    assert_eq!(
                        runner.state().objects[&target].zone,
                        Zone::Exile,
                        "reach: response removed its target"
                    );
                }
            }
            // CR 608.2b: legality is local to each occurrence, not the root.
            let ability = runner.state().stack.front().unwrap().ability().unwrap();
            let validated =
                engine::game::ability_utils::validate_targets_in_chain(runner.state(), ability);
            let children = validated.coin_branches.as_ref().unwrap();
            assert_eq!(
                children.won.as_ref().unwrap().illegal_local_target_slots,
                if removed & 1 != 0 { vec![0] } else { vec![] }
            );
            assert_eq!(
                children.lost.as_ref().unwrap().illegal_local_target_slots,
                if removed & 2 != 0 { vec![0] } else { vec![] }
            );
            finish(&mut runner, &mut events);
            assert_eq!(
                flips(&events),
                if removed == 3 { vec![] } else { vec![won] }
            );
            assert_eq!(runner.life(P0), if removed == 3 { 20 } else { 22 });
            for (target, bit, selected) in [(opponent, 1, won), (own, 2, !won)] {
                assert_eq!(
                    runner.state().objects[&target].zone,
                    if removed & bit != 0 {
                        Zone::Exile
                    } else if selected {
                        Zone::Graveyard
                    } else {
                        Zone::Battlefield
                    }
                );
            }
            assert_eq!(runner.state().objects[&own_spare].zone, Zone::Battlefield);
            assert_eq!(
                runner.state().objects[&opponent_spare].zone,
                Zone::Battlefield
            );
            assert_finished(&runner);
        }
    }
}

#[test]
fn separate_coin_target_occurrences_allow_the_same_creature() {
    // CR 115.3: each branch contains a separate occurrence of "target".
    for won in [false, true] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let source = scenario.add_artifact_from_oracle(P0, "Synthetic repeated target",
            "{T}: Flip a coin. If you win the flip, destroy target creature. If you lose the flip, destroy target creature.").id();
        let target = scenario.add_creature(P1, "Shared target", 2, 2).id();
        let spare = scenario.add_creature(P1, "Unselected target", 3, 3).id();
        let mut runner = scenario.build();
        pin_outcomes(&mut runner, &[won]);
        let outcome = runner
            .activate(source, 0)
            .target_objects(&[target, target])
            .resolve();
        assert_eq!(flips(outcome.events()), vec![won]);
        assert_eq!(outcome.zone_of(target), Zone::Graveyard);
        assert_eq!(outcome.zone_of(spare), Zone::Battlefield);
        assert_finished(&runner);
    }
}

// T2/T8: the engine supplies opaque addresses; the public answer is positional.
#[test]
fn restored_retarget_changes_only_the_addressed_coin_or_later_occurrence() {
    use engine::types::ability::{AbilityDefinition, AbilityKind, CoinFlipResult};
    use engine::types::game_state::{ChainStep, RetargetScope, RetargetSlotAddress};
    use engine::types::resolution::ResolutionStateWire;
    for changed in 0..3 {
        for won in [false, true] {
            for stale_current in [false, true] {
                let mut definition = parse_oracle_text(
                "Flip a coin. If you win the flip, destroy target creature an opponent controls. If you lose the flip, destroy target creature you control.",
                "Synthetic retarget coin", &[], &["Instant".into()], &[],
            ).abilities.remove(0);
                definition.sub_ability = Some(Box::new(
                    parse_oracle_text(
                        "Exile target artifact.",
                        "Synthetic artifact tail",
                        &[],
                        &["Instant".into()],
                        &[],
                    )
                    .abilities
                    .remove(0),
                ));
                let mut scenario = GameScenario::new();
                scenario.at_phase(Phase::PreCombatMain);
                let old = [
                    scenario.add_creature(P1, "Old won", 2, 2).id(),
                    scenario.add_creature(P0, "Old lost", 3, 3).id(),
                    scenario.add_artifact_from_oracle(P1, "Old tail", "").id(),
                ];
                let new = [
                    scenario.add_creature(P1, "New won", 4, 4).id(),
                    scenario.add_creature(P0, "New lost", 5, 5).id(),
                    scenario.add_artifact_from_oracle(P1, "New tail", "").id(),
                ];
                let spell = scenario
                    .add_spell_to_hand(P0, "Synthetic retarget coin", true)
                    .with_ability_definition(definition)
                    .with_mana_cost(ManaCost::zero())
                    .id();
                let redirect = scenario
                    .add_spell_to_hand(P0, "Synthetic redirect", true)
                    .with_ability_definition(AbilityDefinition::new(
                        AbilityKind::Spell,
                        Effect::ChangeTargets {
                            target: TargetFilter::StackSpell,
                            scope: RetargetScope::All,
                            forced_to: None,
                        },
                    ))
                    .with_mana_cost(ManaCost::zero())
                    .id();
                let removal = scenario
                    .add_spell_to_hand_from_oracle(
                        P0,
                        "Synthetic stale-target response",
                        true,
                        "Exile target creature.",
                    )
                    .with_mana_cost(ManaCost::zero())
                    .id();
                let mut runner = scenario.build();
                pin_outcomes(&mut runner, &[won]);
                runner.cast(spell).target_objects(&old).commit();
                if stale_current {
                    runner.cast(removal).target_object(old[0]).commit();
                    for _ in 0..4 {
                        if !runner.state().stack.iter().any(|entry| entry.id == removal) {
                            break;
                        }
                        runner.act(GameAction::PassPriority).unwrap();
                    }
                    assert_eq!(runner.state().objects[&old[0]].zone, Zone::Exile);
                }
                runner.cast(redirect).target_object(spell).commit();
                for _ in 0..4 {
                    if !matches!(runner.state().waiting_for, WaitingFor::Priority { .. }) {
                        break;
                    }
                    runner.act(GameAction::PassPriority).unwrap();
                }
                let WaitingFor::RetargetChoice {
                    slots,
                    slot_pools,
                    current_targets,
                    ..
                } = &runner.state().waiting_for
                else {
                    panic!(
                        "required public retarget choice: {:?}",
                        runner.state().waiting_for
                    );
                };
                assert_eq!(*current_targets, old.map(TargetRef::Object));
                assert_eq!(
                    *slots,
                    vec![
                        RetargetSlotAddress {
                            path: vec![ChainStep::CoinFlipBranch {
                                result: CoinFlipResult::Won
                            }],
                            slot: 0
                        },
                        RetargetSlotAddress {
                            path: vec![ChainStep::CoinFlipBranch {
                                result: CoinFlipResult::Lost
                            }],
                            slot: 0
                        },
                        RetargetSlotAddress {
                            path: vec![ChainStep::SubAbility],
                            slot: 0
                        },
                    ]
                );
                for (position, replacement) in new.iter().enumerate() {
                    assert!(slot_pools[position].contains(&TargetRef::Object(*replacement)));
                }
                let before = runner.state().waiting_for.clone();
                runner.state_mut().capture_rng_word_pos();
                let wire = serde_json::to_value(ResolutionStateWire::from_game_state(
                    runner.state().clone(),
                ))
                .unwrap();
                *runner.state_mut() =
                    serde_json::from_value::<engine::types::game_state::PersistedGameState>(wire)
                        .unwrap()
                        .into_game_state()
                        .expect("production restore validates the parked positional retarget");
                assert_eq!(runner.state().waiting_for, before);
                let mut illegal = vec![None; 3];
                illegal[0] = Some(TargetRef::Object(new[1]));
                assert!(
                    runner
                        .act(GameAction::RetargetSpell {
                            new_targets: illegal
                        })
                        .is_err(),
                    "an own creature is illegal for the Won occurrence after restore"
                );
                assert_eq!(
                    runner.state().waiting_for,
                    before,
                    "rejection leaves the positional prompt intact"
                );
                let mut picks = vec![None; 3];
                picks[changed] = Some(TargetRef::Object(new[changed]));
                let mut events = runner
                    .act(GameAction::RetargetSpell { new_targets: picks })
                    .unwrap()
                    .events;
                finish(&mut runner, &mut events);
                assert_eq!(flips(&events), vec![won]);
                for position in 0..3 {
                    let executed =
                        position == 2 || (position == 0 && won) || (position == 1 && !won);
                    let affected = if position == 2 {
                        Zone::Exile
                    } else {
                        Zone::Graveyard
                    };
                    assert_eq!(
                        runner.state().objects[&old[position]].zone,
                        if stale_current && position == 0 {
                            Zone::Exile
                        } else if executed && changed != position {
                            affected
                        } else {
                            Zone::Battlefield
                        }
                    );
                    assert_eq!(
                        runner.state().objects[&new[position]].zone,
                        if executed && changed == position {
                            affected
                        } else {
                            Zone::Battlefield
                        }
                    );
                }
                assert_finished(&runner);
            }
        }
    }
}

#[test]
fn ai_required_coin_branch_slots_generate_only_the_current_legal_pool() {
    let (mut runner, source, own, opposing) = two_branch_fixture(true, true);
    runner
        .act(GameAction::ActivateAbility {
            source_id: source,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        })
        .unwrap();
    for (required, forbidden) in [(&opposing, &own), (&own, &opposing)] {
        let actions = legal_actions(runner.state());
        for target in required {
            assert!(actions.contains(&GameAction::ChooseTarget {
                target: Some(TargetRef::Object(*target))
            }));
        }
        for target in forbidden {
            assert!(!actions.contains(&GameAction::ChooseTarget {
                target: Some(TargetRef::Object(*target))
            }));
        }
        assert!(!actions.contains(&GameAction::ChooseTarget { target: None }));
        runner
            .act(GameAction::ChooseTarget {
                target: Some(TargetRef::Object(required[0])),
            })
            .unwrap();
    }
    if matches!(runner.state().waiting_for, WaitingFor::ManaPayment { .. }) {
        runner.act(GameAction::PassPriority).unwrap();
    }
    let mut events = Vec::new();
    finish(&mut runner, &mut events);
    assert_eq!(flips(&events), vec![true]);
    assert_eq!(runner.state().objects[&opposing[0]].zone, Zone::Graveyard);
    assert_finished(&runner);
}

#[test]
fn aleatory_draws_once_at_the_next_upkeep_on_either_coin_outcome() {
    // Verbatim authoritative MTGJSON Oracle, checked before building this fixture.
    const ALEATORY: &str = "Cast this spell only during combat after blockers are declared.\nFlip a coin. If you win the flip, target creature gets +1/+1 until end of turn.\nDraw a card at the beginning of the next turn's upkeep.";
    for won in [false, true] {
        let parsed = parse_oracle_text(ALEATORY, "Aleatory", &[], &["Instant".into()], &[]);
        assert_eq!(parsed.abilities.len(), 1);
        let root = &parsed.abilities[0];
        let Effect::FlipCoins {
            flips: run,
            win_effect: Some(win),
            lose_effect: None,
            ..
        } = root.effect.as_ref()
        else {
            panic!("Aleatory must retain exactly one targeted Won branch: {root:#?}");
        };
        assert_eq!(*run, CoinFlipRun::single());
        assert!(matches!(win.effect.as_ref(), Effect::Pump { .. }));
        assert!(win.sub_ability.is_none());
        let delayed = root
            .sub_ability
            .as_ref()
            .expect("independent next-upkeep draw");
        let Effect::CreateDelayedTrigger { effect, .. } = delayed.effect.as_ref() else {
            panic!("Aleatory must create the independent delayed draw: {delayed:#?}");
        };
        assert!(matches!(effect.effect.as_ref(), Effect::Draw { .. }));
        assert!(effect.sub_ability.is_none());
        assert!(delayed.sub_ability.is_none());
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::EndCombat);
        scenario.with_library_top(
            P0,
            &["Aleatory draw witness", "P0 reserve A", "P0 reserve B"],
        );
        scenario.with_library_top(P1, &["P1 reserve A", "P1 reserve B"]);
        let target = scenario.add_creature(P0, "Aleatory target", 2, 2).id();
        let spare = scenario.add_creature(P1, "Aleatory distractor", 4, 4).id();
        let spell = scenario
            .add_spell_to_hand_from_oracle(P0, "Aleatory", true, ALEATORY)
            .with_mana_cost(ManaCost::zero())
            .id();
        let mut runner = scenario.build();
        pin_outcomes(&mut runner, &[won]);
        let outcome = runner.cast(spell).target_object(target).resolve();
        assert_eq!(flips(outcome.events()), vec![won]);
        outcome.assert_hand_drawn(P0, 0);
        assert_eq!(
            outcome.power_toughness(target),
            if won { (3, 3) } else { (2, 2) }
        );
        assert_eq!(outcome.power_toughness(spare), (4, 4));
        assert_finished(&runner);
        let mut events = Vec::new();
        let mut upkeeps = 0;
        let mut last_upkeep_turn = runner.state().turn_number;
        for _ in 0..100 {
            if runner.state().phase == Phase::Upkeep
                && runner.state().turn_number > last_upkeep_turn
            {
                upkeeps += 1;
                last_upkeep_turn = runner.state().turn_number;
                finish(&mut runner, &mut events);
                let draws = events
                    .iter()
                    .filter(|event| {
                        matches!(event,
                            GameEvent::CardDrawn { player_id, .. } if *player_id == P0
                        )
                    })
                    .count();
                assert_eq!(
                    draws, 1,
                    "Aleatory draws at the first upkeep, never again at the second"
                );
                assert!(runner.state().stack.is_empty());
                assert!(runner.state().resolution_stack.is_empty());
                assert!(matches!(
                    runner.state().waiting_for,
                    WaitingFor::Priority { .. }
                ));
                if upkeeps == 2 {
                    break;
                }
            }
            let action = match runner.state().waiting_for {
                WaitingFor::Priority { .. } => GameAction::PassPriority,
                WaitingFor::DeclareAttackers { .. } => GameAction::DeclareAttackers {
                    attacks: vec![],
                    bands: vec![],
                },
                WaitingFor::DeclareBlockers { .. } => GameAction::DeclareBlockers {
                    assignments: vec![],
                },
                ref other => panic!("unexpected turn progress prompt: {other:?}"),
            };
            events.extend(runner.act(action).unwrap().events);
        }
        assert_eq!(upkeeps, 2, "reach: exercised both following upkeeps");
        assert_finished(&runner);
    }
}

#[test]
fn one_coin_occurrence_rejects_duplicate_targets_but_accepts_two_distinct_creatures() {
    use engine::types::ability::{AbilityDefinition, AbilityKind, MultiTargetSpec, TypedFilter};
    let mut child = AbilityDefinition::new(
        AbilityKind::Activated,
        Effect::Destroy {
            target: TargetFilter::Typed(TypedFilter::creature()),
            cant_regenerate: false,
        },
    );
    child.multi_target = Some(MultiTargetSpec::fixed(2, 2));
    let root = AbilityDefinition::new(
        AbilityKind::Activated,
        Effect::FlipCoins {
            flips: CoinFlipRun::single(),
            win_effect: Some(Box::new(child)),
            lose_effect: None,
            flipper: TargetFilter::Controller,
        },
    );
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_artifact_from_oracle(P0, "Synthetic two-target occurrence", "")
        .with_ability_definition(root)
        .id();
    let first = scenario
        .add_creature(P1, "First distinct target", 2, 2)
        .id();
    let second = scenario
        .add_creature(P1, "Second distinct target", 3, 3)
        .id();
    let mut runner = scenario.build();
    pin_outcomes(&mut runner, &[true]);
    runner
        .act(GameAction::ActivateAbility {
            source_id: source,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        })
        .unwrap();
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(first)),
        })
        .unwrap();
    let before = runner.state().clone();
    // CR 115.3: two selections within this one occurrence must be distinct.
    assert!(runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(first))
        })
        .is_err());
    assert_eq!(*runner.state(), before);
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(second)),
        })
        .unwrap();
    let mut events = Vec::new();
    finish(&mut runner, &mut events);
    assert_eq!(
        flips(&events),
        vec![true],
        "reach: the distinct assignment resolves"
    );
    assert_eq!(runner.state().objects[&first].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&second].zone, Zone::Graveyard);
    assert_finished(&runner);
}

#[test]
fn cross_controller_activated_copy_rebinds_each_branch_without_mutating_original() {
    use engine::types::ability::{
        AbilityDefinition, AbilityKind, CopyRetargetPermission, StackAbilityKind,
    };
    use engine::types::player::PlayerId;
    for won in [false, true] {
        let p2 = PlayerId(2);
        let mut scenario = GameScenario::new_n_player(3, 7);
        scenario.at_phase(Phase::PreCombatMain);
        let mut definition = parse_oracle_text(
            TWO_BRANCHES,
            "Synthetic copied activation",
            &[],
            &["Artifact".into()],
            &[],
        )
        .abilities
        .remove(0);
        definition.cost = None;
        let source = scenario
            .add_artifact_from_oracle(P0, "Original ability source", "")
            .with_ability_definition(definition)
            .id();
        let original_own = scenario.add_creature(P0, "Original own target", 2, 2).id();
        let original_opponent = scenario
            .add_creature(P1, "Original opponent target", 3, 3)
            .id();
        let copy_own = scenario.add_creature(P1, "Copy own target", 4, 4).id();
        let copy_opponent = scenario.add_creature(p2, "Copy opponent target", 5, 5).id();
        let spare = scenario.add_creature(p2, "Copy distractor", 6, 6).id();
        let copy_spell = scenario
            .add_spell_to_hand(P1, "Synthetic ability copier", true)
            .with_ability_definition(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::CopySpell {
                    target: TargetFilter::StackAbility {
                        controller: None,
                        tag: None,
                        kind: Some(StackAbilityKind::Activated),
                    },
                    retarget: CopyRetargetPermission::MayChooseNewTargets,
                    copier: None,
                    additional_modifications: vec![],
                    starting_loyalty_from_casualty_sacrifice: false,
                },
            ))
            .with_mana_cost(ManaCost::zero())
            .id();
        let mut runner = scenario.build();
        pin_outcomes(&mut runner, &[won, won]);
        runner
            .act(GameAction::ActivateAbility {
                source_id: source,
                ability_index: 0,
                payment_mode: CastPaymentMode::Auto,
            })
            .unwrap();
        for target in [original_opponent, original_own] {
            runner
                .act(GameAction::ChooseTarget {
                    target: Some(TargetRef::Object(target)),
                })
                .unwrap();
        }
        let original = runner.state().stack.back().unwrap().clone();
        runner.act(GameAction::PassPriority).unwrap();
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { player: P1 }
        ));
        runner.cast(copy_spell).target_object(original.id).commit();
        for _ in 0..6 {
            if !matches!(runner.state().waiting_for, WaitingFor::Priority { .. }) {
                break;
            }
            runner.act(GameAction::PassPriority).unwrap();
        }
        let WaitingFor::CopyRetarget {
            player,
            copy_id,
            target_slots,
            ..
        } = &runner.state().waiting_for
        else {
            panic!(
                "required activated copy retarget: {:?}",
                runner.state().waiting_for
            );
        };
        assert_eq!(*player, P1);
        let copy_id = *copy_id;
        assert_eq!(target_slots.len(), 2);
        assert!(target_slots[0]
            .legal_alternatives
            .contains(&TargetRef::Object(copy_opponent)));
        assert!(!target_slots[0]
            .legal_alternatives
            .contains(&TargetRef::Object(copy_own)));
        assert!(
            runner
                .act(GameAction::ChooseTarget {
                    target: Some(TargetRef::Object(copy_own))
                })
                .is_err(),
            "copy's won branch cannot choose its controller's creature"
        );
        runner
            .act(GameAction::ChooseTarget {
                target: Some(TargetRef::Object(copy_opponent)),
            })
            .unwrap();
        runner
            .act(GameAction::ChooseTarget {
                target: Some(TargetRef::Object(copy_own)),
            })
            .unwrap();
        let copy = runner
            .state()
            .stack
            .iter()
            .find(|entry| entry.id == copy_id)
            .unwrap();
        assert_eq!(copy.controller, P1);
        assert_eq!(copy.source_id, source);
        assert_eq!(
            copy.ability()
                .unwrap()
                .coin_branches
                .as_ref()
                .unwrap()
                .won
                .as_ref()
                .unwrap()
                .controller,
            P1
        );
        assert_eq!(
            runner
                .state()
                .stack
                .iter()
                .find(|entry| entry.id == original.id)
                .unwrap(),
            &original
        );
        let mut events = Vec::new();
        finish(&mut runner, &mut events);
        let actual: Vec<_> = events
            .iter()
            .filter_map(|event| match event {
                GameEvent::CoinFlipped { player_id, won } => Some((*player_id, *won)),
                _ => None,
            })
            .collect();
        assert_eq!(actual, vec![(P1, won), (P0, won)]);
        for (target, selected) in [
            (original_opponent, won),
            (original_own, !won),
            (copy_opponent, won),
            (copy_own, !won),
        ] {
            assert_eq!(
                runner.state().objects[&target].zone,
                if selected {
                    Zone::Graveyard
                } else {
                    Zone::Battlefield
                }
            );
        }
        assert_eq!(runner.state().objects[&spare].zone, Zone::Battlefield);
        assert_finished(&runner);
    }
}

#[test]
fn three_seat_borrowed_source_and_target_keep_controller_and_incarnation_authority() {
    use engine::types::player::PlayerId;
    for alteration in ["unchanged", "source leaves", "blink", "control changes"] {
        for won in [false, true] {
            let p2 = PlayerId(2);
            let mut scenario = GameScenario::new_n_player(3, 7);
            scenario.at_phase(Phase::PreCombatMain);
            let mut definition = parse_oracle_text(
                TWO_BRANCHES,
                "Synthetic borrowed source",
                &[],
                &["Artifact".into()],
                &[],
            )
            .abilities
            .remove(0);
            definition.cost = None;
            let source = scenario
                .add_artifact_from_oracle(P0, "Borrowed source", "")
                .with_ability_definition(definition)
                .id();
            let own = scenario.add_creature(P0, "Borrowed own target", 2, 2).id();
            let opponent = scenario
                .add_creature(p2, "Third-seat opponent target", 3, 3)
                .id();
            let spare = scenario
                .add_creature(P1, "Owner-seat distractor", 4, 4)
                .id();
            let land = scenario
                .add_land_from_oracle(P0, "Blink companion", "")
                .id();
            let remove = scenario
                .add_spell_to_hand_from_oracle(
                    P0,
                    "Synthetic source removal",
                    true,
                    "Destroy target artifact.",
                )
                .with_mana_cost(ManaCost::zero())
                .id();
            let blink = scenario.add_spell_to_hand_from_oracle(P0, "Synthetic two-object blink", true,
                "Exile two target artifacts, creatures, and/or lands you control, then return those cards to the battlefield under your control.")
                .with_mana_cost(ManaCost::zero()).id();
            let steal = scenario
                .add_spell_to_hand_from_oracle(
                    P1,
                    "Synthetic control response",
                    true,
                    "Gain control of target creature.",
                )
                .with_mana_cost(ManaCost::zero())
                .id();
            let mut runner = scenario.build();
            // Fixture ownership differs from live control before announcement.
            runner.state_mut().objects.get_mut(&source).unwrap().owner = p2;
            runner.state_mut().objects.get_mut(&own).unwrap().owner = P1;
            pin_outcomes(&mut runner, &[won]);
            runner
                .act(GameAction::ActivateAbility {
                    source_id: source,
                    ability_index: 0,
                    payment_mode: CastPaymentMode::Auto,
                })
                .unwrap();
            for target in [opponent, own] {
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(TargetRef::Object(target)),
                    })
                    .unwrap();
            }
            let old_pin = runner
                .state()
                .stack
                .back()
                .unwrap()
                .ability()
                .unwrap()
                .coin_branches
                .as_ref()
                .unwrap()
                .lost
                .as_ref()
                .unwrap()
                .target_pin_at(0)
                .unwrap();
            let response = match alteration {
                "unchanged" => None,
                "source leaves" => {
                    runner.cast(remove).target_object(source).commit();
                    Some(remove)
                }
                "blink" => {
                    runner.cast(blink).target_objects(&[own, land]).commit();
                    Some(blink)
                }
                "control changes" => {
                    runner.act(GameAction::PassPriority).unwrap();
                    runner.cast(steal).target_object(own).commit();
                    Some(steal)
                }
                _ => unreachable!(),
            };
            let mut events = Vec::new();
            if let Some(response) = response {
                for _ in 0..8 {
                    if !runner
                        .state()
                        .stack
                        .iter()
                        .any(|entry| entry.id == response)
                    {
                        break;
                    }
                    events.extend(runner.act(GameAction::PassPriority).unwrap().events);
                }
            }
            match alteration {
                "source leaves" => {
                    assert_eq!(runner.state().objects[&source].zone, Zone::Graveyard)
                }
                "blink" => assert_ne!(
                    runner.state().objects[&own].incarnation,
                    old_pin.incarnation
                ),
                "control changes" => assert_eq!(runner.state().objects[&own].controller, P1),
                "unchanged" => assert_eq!(runner.state().objects[&own].controller, P0),
                _ => unreachable!(),
            }
            finish(&mut runner, &mut events);
            assert_eq!(
                flips(&events),
                vec![won],
                "another legal occurrence keeps the coin live"
            );
            assert_eq!(
                runner.state().objects[&opponent].zone,
                if won {
                    Zone::Graveyard
                } else {
                    Zone::Battlefield
                }
            );
            let own_dies = !won && matches!(alteration, "unchanged" | "source leaves");
            assert_eq!(
                runner.state().objects[&own].zone,
                if own_dies {
                    Zone::Graveyard
                } else {
                    Zone::Battlefield
                }
            );
            assert_eq!(runner.state().objects[&spare].zone, Zone::Battlefield);
            assert_finished(&runner);
        }
    }
}

#[test]
fn optional_empty_coin_slot_does_not_steal_the_later_target() {
    for empty in [false, true] {
        let mut definition = parse_oracle_text(
            TWO_BRANCHES,
            "Synthetic optional branch",
            &[],
            &["Artifact".into()],
            &[],
        )
        .abilities
        .remove(0);
        definition.cost = None;
        let Effect::FlipCoins { win_effect, .. } = definition.effect.as_mut() else {
            unreachable!()
        };
        win_effect.as_mut().unwrap().optional_targeting = true;
        definition.sub_ability = Some(Box::new(
            parse_oracle_text(
                "Exile target artifact.",
                "Synthetic optional tail",
                &[],
                &["Instant".into()],
                &[],
            )
            .abilities
            .remove(0),
        ));
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let source = scenario
            .add_artifact_from_oracle(P0, "Optional source", "")
            .with_ability_definition(definition)
            .id();
        let own = scenario
            .add_creature(P0, "Mandatory lost target", 2, 2)
            .id();
        let opponent =
            (!empty).then(|| scenario.add_creature(P1, "Optional won target", 3, 3).id());
        let later = scenario
            .add_artifact_from_oracle(P1, "Mandatory tail target", "")
            .id();
        let mut runner = scenario.build();
        pin_outcomes(&mut runner, &[true]);
        runner
            .act(GameAction::ActivateAbility {
                source_id: source,
                ability_index: 0,
                payment_mode: CastPaymentMode::Auto,
            })
            .unwrap();
        let WaitingFor::TargetSelection {
            target_slots,
            selection,
            ..
        } = &runner.state().waiting_for
        else {
            panic!("required target prompt")
        };
        assert_eq!(target_slots.len(), 3);
        if let Some(opponent) = opponent {
            assert_eq!(selection.current_slot, 0);
            runner
                .act(GameAction::ChooseTarget {
                    target: Some(TargetRef::Object(opponent)),
                })
                .unwrap();
        } else {
            assert_eq!(
                selection.current_slot, 1,
                "empty optional pool is already completed"
            );
            assert_eq!(selection.selected_slots, vec![None]);
        }
        for target in [own, later] {
            runner
                .act(GameAction::ChooseTarget {
                    target: Some(TargetRef::Object(target)),
                })
                .unwrap();
        }
        let mut events = Vec::new();
        finish(&mut runner, &mut events);
        assert_eq!(flips(&events), vec![true]);
        if let Some(opponent) = opponent {
            assert_eq!(runner.state().objects[&opponent].zone, Zone::Graveyard);
        }
        assert_eq!(runner.state().objects[&own].zone, Zone::Battlefield);
        assert_eq!(runner.state().objects[&later].zone, Zone::Exile);
        assert_finished(&runner);
    }
}

#[test]
fn separate_piles_materializes_targetless_coin_children_before_execution() {
    use engine::types::ability::{
        AbilityDefinition, AbilityKind, PileSource, PlayerScope, QuantityExpr, TypedFilter,
        VoterScope,
    };
    use engine::types::game_state::PileSide;
    for won in [false, true] {
        let reward = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 3 },
                player: TargetFilter::Controller,
            },
        );
        let punishment = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::LoseLife {
                amount: QuantityExpr::Fixed { value: 2 },
                target: Some(TargetFilter::Controller),
            },
        );
        let coin = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::FlipCoins {
                flips: CoinFlipRun::single(),
                win_effect: Some(Box::new(reward)),
                lose_effect: Some(Box::new(punishment)),
                flipper: TargetFilter::Controller,
            },
        );
        let mut canonical = GameScenario::new();
        canonical.at_phase(Phase::PreCombatMain);
        let ordinary = canonical
            .add_spell_to_hand(P0, "Synthetic canonical coin control", true)
            .with_ability_definition(coin.clone())
            .with_mana_cost(ManaCost::zero())
            .id();
        let mut canonical = canonical.build();
        pin_outcomes(&mut canonical, &[won]);
        let control = canonical.cast(ordinary).resolve();
        assert_eq!(flips(control.events()), vec![won]);
        assert_eq!(canonical.life(P0), if won { 23 } else { 18 });
        assert_finished(&canonical);
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let a = scenario.add_creature(P1, "Pile A member", 2, 2).id();
        let b = scenario.add_creature(P1, "Pile B member", 3, 3).id();
        let spell = scenario
            .add_spell_to_hand(P0, "Synthetic pile coin", false)
            .with_ability(Effect::SeparateIntoPiles {
                partition_subject: VoterScope::EachOpponent,
                object_filter: TargetFilter::Typed(TypedFilter::creature()),
                chooser: PlayerScope::Controller,
                chosen_pile_effect: Box::new(coin),
                pile_source: PileSource::Battlefield,
                unchosen_pile_effect: None,
            })
            .with_mana_cost(ManaCost::zero())
            .id();
        let mut runner = scenario.build();
        pin_outcomes(&mut runner, &[won]);
        let outcome = runner.cast(spell).resolve();
        assert!(matches!(
            outcome.final_waiting_for(),
            WaitingFor::SeparatePilesPartition { player: P1, .. }
        ));
        let mut events = outcome.events().to_vec();
        events.extend(
            runner
                .act(GameAction::SubmitPilePartition { pile_a: vec![a] })
                .unwrap()
                .events,
        );
        let WaitingFor::SeparatePilesChoice { player, .. } = runner.state().waiting_for else {
            panic!("required pile choice")
        };
        assert_eq!(player, P0);
        events.extend(
            runner
                .act(GameAction::ChoosePile { pile: PileSide::A })
                .unwrap()
                .events,
        );
        finish(&mut runner, &mut events);
        assert_eq!(flips(&events), vec![won]);
        assert_eq!(runner.life(P0), if won { 23 } else { 18 });
        assert_eq!(
            runner.life(P1),
            20,
            "pile-local subject is not substituted for the coin controller"
        );
        assert_eq!(runner.state().objects[&a].zone, Zone::Battlefield);
        assert_eq!(runner.state().objects[&b].zone, Zone::Battlefield);
        assert_finished(&runner);
    }
}

#[test]
fn copied_coin_child_re_elects_a_departed_announcer_without_rebinding_other_groups() {
    use engine::types::game_state::CopyChoiceMode;
    use engine::types::player::PlayerId;
    let p2 = PlayerId(2);
    let mut definition = parse_oracle_text(
        "Flip a coin. If you win the flip, destroy target creature an opponent controls. If you lose the flip, destroy target artifact.",
        "Synthetic opponent-announced coin", &[], &["Instant".into()], &[],
    ).abilities.remove(0);
    let Effect::FlipCoins {
        win_effect,
        lose_effect,
        ..
    } = definition.effect.as_mut()
    else {
        unreachable!()
    };
    win_effect.as_mut().unwrap().target_chooser = Some(TargetFilter::Opponent);
    lose_effect.as_mut().unwrap().target_chooser = Some(TargetFilter::Opponent);
    definition.sub_ability = Some(Box::new(
        parse_oracle_text(
            "Exile target land.",
            "Synthetic caster-announced tail",
            &[],
            &["Instant".into()],
            &[],
        )
        .abilities
        .remove(0),
    ));
    let mut scenario = GameScenario::new_n_player(3, 7);
    scenario.at_phase(Phase::PreCombatMain);
    let creature = scenario.add_creature(P1, "Chosen coin creature", 2, 2).id();
    let spare = scenario.add_creature(P1, "Creature distractor", 3, 3).id();
    let artifact = scenario
        .add_artifact_from_oracle(P1, "Lost branch artifact", "")
        .id();
    let land = scenario
        .add_land_from_oracle(P0, "Caster's later land", "")
        .id();
    let source = scenario
        .add_spell_to_graveyard(P0, "Synthetic opponent-announced coin", true)
        .with_ability_definition(definition)
        .id();
    let mastery = scenario.add_spell_to_hand(P0, "Mizzix's Mastery", false)
        .from_oracle_text_with_keywords(&["Overload"],
            "Exile target card that's an instant or sorcery from your graveyard. For each card exiled this way, copy it, and you may cast the copy without paying its mana cost. Exile Mizzix's Mastery.\nOverload {5}{R}{R}{R} (You may cast this spell for its overload cost. If you do, change \"target\" in its text to \"each.\")")
        .with_mana_cost(ManaCost::zero()).id();
    let mut runner = scenario.build();
    pin_outcomes(&mut runner, &[true]);
    runner.cast(mastery).target_object(source).commit();
    for _ in 0..6 {
        if !matches!(runner.state().waiting_for, WaitingFor::Priority { .. }) {
            break;
        }
        runner.act(GameAction::PassPriority).unwrap();
    }
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ChooseFromZoneChoice { .. }
    ));
    runner
        .act(GameAction::SelectCards {
            cards: vec![source],
        })
        .unwrap();
    let WaitingFor::CopyRetarget {
        copy_id,
        mode: Some(CopyChoiceMode::Announce),
        announcer_election: Some(_),
        ..
    } = runner.state().waiting_for
    else {
        panic!(
            "required copy announcement election: {:?}",
            runner.state().waiting_for
        )
    };
    for opponent in [p2, P1] {
        runner
            .act(GameAction::ChooseAnnouncingOpponent { opponent })
            .unwrap();
    }
    let WaitingFor::CopyRetarget {
        player: asked,
        current_slot,
        ..
    } = &runner.state().waiting_for
    else {
        panic!("required coin child target choice")
    };
    assert_eq!(*asked, p2);
    assert_eq!(*current_slot, 0, "the first declaration belongs to Won");
    assert_eq!(
        runner
            .state()
            .stack
            .iter()
            .find(|entry| entry.id == copy_id)
            .unwrap()
            .ability()
            .unwrap()
            .coin_branches
            .as_ref()
            .unwrap()
            .won
            .as_ref()
            .unwrap()
            .context
            .announcing_opponent,
        Some(p2)
    );
    runner.act(GameAction::Concede { player_id: p2 }).unwrap();
    let WaitingFor::CopyRetarget {
        player: asked,
        current_slot,
        ..
    } = &runner.state().waiting_for
    else {
        panic!("required re-elected child choice")
    };
    assert_eq!(
        *asked, P1,
        "remaining opponent replaces the departed child announcer"
    );
    assert_eq!(
        *current_slot, 0,
        "replacement resumes the same child occurrence"
    );
    assert_eq!(
        runner
            .state()
            .stack
            .iter()
            .find(|entry| entry.id == copy_id)
            .unwrap()
            .ability()
            .unwrap()
            .coin_branches
            .as_ref()
            .unwrap()
            .won
            .as_ref()
            .unwrap()
            .context
            .announcing_opponent,
        Some(P1)
    );
    for (target, expected_chooser) in [(creature, P1), (artifact, P1), (land, P0)] {
        let WaitingFor::CopyRetarget { player, .. } = runner.state().waiting_for else {
            panic!("required remaining announcement")
        };
        assert_eq!(player, expected_chooser);
        runner
            .act(GameAction::ChooseTarget {
                target: Some(TargetRef::Object(target)),
            })
            .unwrap();
    }
    let ability = runner
        .state()
        .stack
        .iter()
        .find(|entry| entry.id == copy_id)
        .unwrap()
        .ability()
        .unwrap();
    let branches = ability.coin_branches.as_ref().unwrap();
    assert_eq!(
        branches.won.as_ref().unwrap().context.announcing_opponent,
        Some(P1)
    );
    assert_eq!(
        branches.lost.as_ref().unwrap().context.announcing_opponent,
        Some(P1)
    );
    assert_eq!(
        ability
            .sub_ability
            .as_ref()
            .unwrap()
            .context
            .announcing_opponent,
        None
    );
    let mut events = Vec::new();
    finish(&mut runner, &mut events);
    assert_eq!(flips(&events), vec![true]);
    assert_eq!(runner.state().objects[&creature].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&spare].zone, Zone::Battlefield);
    assert_eq!(runner.state().objects[&artifact].zone, Zone::Battlefield);
    assert_eq!(runner.state().objects[&land].zone, Zone::Exile);
    assert_finished(&runner);
}

#[test]
fn differently_bound_coin_children_do_not_coalesce_and_each_resolves_its_own_target() {
    use engine::game::stack::stack_display_groups;
    for same in [false, true] {
        let (mut runner, source, own, opposing) = two_branch_fixture(true, true);
        // Synthetic no-cost repeated activation; all declaration and resolution
        // still go through the production activation pipeline.
        let mut abilities = runner.state().objects[&source].abilities.as_ref().clone();
        abilities[0].cost = None;
        let object = runner.state_mut().objects.get_mut(&source).unwrap();
        object.abilities = std::sync::Arc::new(abilities.clone());
        object.base_abilities = std::sync::Arc::new(abilities);
        pin_outcomes(&mut runner, &[true, true]);
        for opponent in [opposing[0], if same { opposing[0] } else { opposing[1] }] {
            runner
                .act(GameAction::ActivateAbility {
                    source_id: source,
                    ability_index: 0,
                    payment_mode: CastPaymentMode::Auto,
                })
                .unwrap();
            for target in [opponent, own[0]] {
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(TargetRef::Object(target)),
                    })
                    .unwrap();
            }
        }
        assert_eq!(runner.state().stack.len(), 2);
        let groups = stack_display_groups(runner.state());
        assert_eq!(
            groups.iter().map(|group| group.count).collect::<Vec<_>>(),
            if same { vec![2] } else { vec![1, 1] },
            "different child declarations are not presented as identical stack entries"
        );
        let mut events = Vec::new();
        finish(&mut runner, &mut events);
        assert_eq!(flips(&events), vec![true, true]);
        assert_eq!(runner.state().objects[&opposing[0]].zone, Zone::Graveyard);
        assert_eq!(
            runner.state().objects[&opposing[1]].zone,
            if same {
                Zone::Battlefield
            } else {
                Zone::Graveyard
            }
        );
        assert_eq!(runner.state().objects[&own[0]].zone, Zone::Battlefield);
        assert_finished(&runner);
    }
}

#[test]
fn viewer_projection_redacts_hidden_coin_child_context_but_keeps_public_sibling_context() {
    use engine::game::visibility::filter_state_for_viewer;
    use engine::types::ability::CostPaidObjectSnapshot;
    use engine::types::game_state::ManaSpentSourceSnapshot;
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mut definition = parse_oracle_text(
        TWO_BRANCHES,
        "Synthetic redacted coin",
        &[],
        &["Artifact".into()],
        &[],
    )
    .abilities
    .remove(0);
    definition.cost = None;
    let source = scenario
        .add_artifact_from_oracle(P0, "Public coin source", "")
        .with_ability_definition(definition)
        .id();
    let hidden = scenario
        .add_spell_to_hand(P0, "Hidden payment sentinel", true)
        .id();
    let own = scenario.add_creature(P0, "Public own target", 2, 2).id();
    let opponent = scenario
        .add_creature(P1, "Public opponent target", 3, 3)
        .id();
    scenario.add_creature(P0, "Public own distractor", 4, 4);
    scenario.add_creature(P1, "Public opponent distractor", 5, 5);
    scenario.with_library_top(P0, &["Hidden library sentinel", "Library reserve"]);
    let mut runner = scenario.build();
    pin_outcomes(&mut runner, &[true]);
    runner
        .act(GameAction::ActivateAbility {
            source_id: source,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        })
        .unwrap();
    for target in [opponent, own] {
        runner
            .act(GameAction::ChooseTarget {
                target: Some(TargetRef::Object(target)),
            })
            .unwrap();
    }
    let library = runner.state().players[0].library[0];
    let snapshot = |id| {
        CostPaidObjectSnapshot::capture(
            &runner.state().objects[&id],
            runner.state().objects[&id].snapshot_public_characteristics(),
        )
    };
    let hidden_library = snapshot(library);
    let visible_object = snapshot(own);
    let mut context = runner.state().objects[&source]
        .snapshot_for_zone_change(source, Some(Zone::Battlefield), Zone::Battlefield)
        .trigger_source_context
        .unwrap();
    context.mana_spent_source_snapshots = vec![
        ManaSpentSourceSnapshot {
            source_id: hidden,
            lki: runner.state().objects[&hidden].snapshot_for_mana_spent(),
        },
        ManaSpentSourceSnapshot {
            source_id: own,
            lki: runner.state().objects[&own].snapshot_for_mana_spent(),
        },
    ];
    // Hostile metadata fixture on an actually announced stack ability. These
    // snapshots exercise projection, not a fabricated effect-resolution path.
    let branches = runner
        .state_mut()
        .stack
        .back_mut()
        .unwrap()
        .ability_mut()
        .unwrap()
        .coin_branches
        .as_mut()
        .unwrap();
    branches.won.as_mut().unwrap().trigger_source = Some(context.clone());
    branches.won.as_mut().unwrap().effect_context_object = Some(hidden_library);
    branches.lost.as_mut().unwrap().trigger_source = Some(context.clone());
    branches.lost.as_mut().unwrap().effect_context_object = Some(visible_object.clone());
    let authoritative = runner.state().clone();
    let view = filter_state_for_viewer(runner.state(), P1);
    let projected = view
        .stack
        .back()
        .unwrap()
        .ability()
        .unwrap()
        .coin_branches
        .as_ref()
        .unwrap();
    assert_eq!(
        authoritative.objects[&hidden].name,
        "Hidden payment sentinel"
    );
    for child in [
        projected.won.as_ref().unwrap(),
        projected.lost.as_ref().unwrap(),
    ] {
        let payment = &child
            .trigger_source
            .as_ref()
            .unwrap()
            .mana_spent_source_snapshots;
        assert_eq!(payment[0].source_id, hidden);
        assert_eq!(payment[0].lki.name, "Hidden Card");
        assert_eq!(payment[1], context.mana_spent_source_snapshots[1]);
    }
    assert!(projected
        .won
        .as_ref()
        .unwrap()
        .effect_context_object
        .is_none());
    assert_eq!(
        projected.lost.as_ref().unwrap().effect_context_object,
        Some(visible_object)
    );
    assert_eq!(
        *runner.state(),
        authoritative,
        "projection never mutates execution authority"
    );
    let mut events = Vec::new();
    finish(&mut runner, &mut events);
    assert_eq!(flips(&events), vec![true]);
    assert_eq!(runner.state().objects[&opponent].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&own].zone, Zone::Battlefield);
    assert_finished(&runner);
}

#[test]
fn copied_coin_targets_preserve_single_and_separate_occurrence_uniqueness() {
    use engine::types::ability::{
        AbilityDefinition, AbilityKind, CopyRetargetPermission, MultiTargetSpec, StackAbilityKind,
        TypedFilter,
    };
    for single_occurrence in [false, true] {
        let destroy = || {
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Destroy {
                    target: TargetFilter::Typed(TypedFilter::creature()),
                    cant_regenerate: false,
                },
            )
        };
        let mut win = destroy();
        if single_occurrence {
            win.multi_target = Some(MultiTargetSpec::fixed(2, 2));
        }
        let coin = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::FlipCoins {
                flips: CoinFlipRun::single(),
                win_effect: Some(Box::new(win)),
                lose_effect: (!single_occurrence).then(|| Box::new(destroy())),
                flipper: TargetFilter::Controller,
            },
        );
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let source = scenario
            .add_artifact_from_oracle(P0, "Synthetic occurrence copier source", "")
            .with_ability_definition(coin)
            .id();
        let targets: Vec<_> = ["Original A", "Original B", "Copy C", "Copy D"]
            .iter()
            .map(|name| scenario.add_creature(P1, name, 2, 2).id())
            .collect();
        let copier = scenario
            .add_spell_to_hand(P0, "Synthetic occurrence copier", true)
            .with_ability(Effect::CopySpell {
                target: TargetFilter::StackAbility {
                    controller: None,
                    tag: None,
                    kind: Some(StackAbilityKind::Activated),
                },
                retarget: CopyRetargetPermission::MayChooseNewTargets,
                copier: None,
                additional_modifications: vec![],
                starting_loyalty_from_casualty_sacrifice: false,
            })
            .with_mana_cost(ManaCost::zero())
            .id();
        let mut runner = scenario.build();
        pin_outcomes(&mut runner, &[true, true]);
        runner
            .act(GameAction::ActivateAbility {
                source_id: source,
                ability_index: 0,
                payment_mode: CastPaymentMode::Auto,
            })
            .unwrap();
        for target in [targets[0], targets[usize::from(single_occurrence)]] {
            runner
                .act(GameAction::ChooseTarget {
                    target: Some(TargetRef::Object(target)),
                })
                .unwrap();
        }
        let original = runner.state().stack.back().unwrap().clone();
        runner.cast(copier).target_object(original.id).commit();
        for _ in 0..4 {
            if !matches!(runner.state().waiting_for, WaitingFor::Priority { .. }) {
                break;
            }
            runner.act(GameAction::PassPriority).unwrap();
        }
        let WaitingFor::CopyRetarget { target_slots, .. } = &runner.state().waiting_for else {
            panic!("required occurrence-aware copy retarget");
        };
        assert_eq!(target_slots.len(), 2);
        runner
            .act(GameAction::ChooseTarget {
                target: Some(TargetRef::Object(targets[2])),
            })
            .unwrap();
        if single_occurrence {
            // CR 115.3: changing targets cannot duplicate one occurrence's target.
            assert!(runner
                .act(GameAction::ChooseTarget {
                    target: Some(TargetRef::Object(targets[2]))
                })
                .is_err());
        }
        runner
            .act(GameAction::ChooseTarget {
                target: Some(TargetRef::Object(
                    targets[if single_occurrence { 3 } else { 2 }],
                )),
            })
            .unwrap();
        assert_eq!(
            runner
                .state()
                .stack
                .iter()
                .find(|entry| entry.id == original.id)
                .unwrap(),
            &original
        );
        let mut events = Vec::new();
        finish(&mut runner, &mut events);
        assert_eq!(flips(&events), vec![true, true]);
        for (index, target) in targets.iter().enumerate() {
            let dies = single_occurrence || index % 2 == 0;
            assert_eq!(
                runner.state().objects[target].zone,
                if dies {
                    Zone::Graveyard
                } else {
                    Zone::Battlefield
                }
            );
        }
        assert_finished(&runner);
    }
}

#[test]
fn independent_branch_head_preserves_incoming_referent_inline_and_after_thumb_restore() {
    use engine::types::ability::{AbilityDefinition, AbilityKind, QuantityExpr, TypedFilter};
    use engine::types::game_state::PersistedGameState;
    for thumb in [false, true] {
        let branch = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 5 },
                player: TargetFilter::Controller,
            },
        )
        .sub_ability(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::GiveControl {
                target: TargetFilter::ParentTarget,
                recipient: TargetFilter::SpecificPlayer { id: P1 },
            },
        ));
        let definition = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::TargetOnly {
                target: TargetFilter::Typed(TypedFilter::creature()),
            },
        )
        .sub_ability(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::FlipCoins {
                flips: CoinFlipRun::single(),
                win_effect: Some(Box::new(branch)),
                lose_effect: None,
                flipper: TargetFilter::Controller,
            },
        ));
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        if thumb {
            scenario.add_artifact_from_oracle(
                P0,
                "Krark's Thumb",
                "If you would flip a coin, instead flip two coins and ignore one.",
            );
        }
        let picked = scenario.add_creature(P0, "Incoming referent", 2, 2).id();
        let distractor = scenario.add_creature(P0, "Unchosen referent", 3, 3).id();
        let spell = scenario
            .add_spell_to_hand(P0, "Synthetic independent branch head", true)
            .with_ability_definition(definition)
            .id();
        let mut runner = scenario.build();
        pin_outcomes(&mut runner, &[true, false]);
        drop(runner.cast(spell).target_object(picked).commit());
        let mut events = Vec::new();
        if thumb {
            pass_to_coin_keep(&mut runner, &mut events);
            assert!(matches!(
                runner.state().waiting_for,
                WaitingFor::CoinFlipKeepChoice { player: P0, .. }
            ));
            runner.state_mut().capture_rng_word_pos();
            let wire =
                serde_json::to_value(PersistedGameState::capture(runner.state().clone())).unwrap();
            *runner.state_mut() = serde_json::from_value::<PersistedGameState>(wire)
                .unwrap()
                .into_game_state()
                .unwrap();
            events.extend(
                runner
                    .act(GameAction::SelectCoinFlips {
                        keep_indices: vec![0],
                    })
                    .unwrap()
                    .events,
            );
        }
        finish(&mut runner, &mut events);
        assert_eq!(
            flips(&events),
            vec![true],
            "reach: selected branch executed"
        );
        assert_eq!(runner.life(P0), 25, "reach: independent head executed");
        assert_eq!(
            runner.state().objects[&picked].controller,
            P1,
            "tail retains incoming object"
        );
        assert_eq!(runner.state().objects[&distractor].controller, P0);
        assert_finished(&runner);
    }
}

#[test]
fn modal_coin_first_mode_round_trips_at_mode_choice_and_executes_its_branch() {
    use engine::types::game_state::PersistedGameState;
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario.add_spell_to_hand_from_oracle(P0, "Synthetic modal coin", true,
        "Choose one —\n• Flip a coin. If you win the flip, you gain 3 life.\n• You gain 7 life.")
        .with_mana_cost(ManaCost::zero()).id();
    let mut runner = scenario.build();
    pin_outcomes(&mut runner, &[true]);
    let card_id = runner.state().objects[&spell].card_id;
    let mut events = runner
        .act(GameAction::CastSpell {
            card_id,
            object_id: spell,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .unwrap()
        .events;
    let WaitingFor::ModeChoice { pending_cast, .. } = &runner.state().waiting_for else {
        panic!(
            "required modal announcement: {:?}",
            runner.state().waiting_for
        );
    };
    assert!(
        matches!(
            pending_cast.ability.effect,
            Effect::FlipCoins {
                win_effect: Some(_),
                ..
            }
        ),
        "reach: placeholder carries the first mode's branch-bearing coin"
    );
    runner.state_mut().capture_rng_word_pos();
    let wire = serde_json::to_value(PersistedGameState::capture(runner.state().clone())).unwrap();
    *runner.state_mut() = serde_json::from_value::<PersistedGameState>(wire)
        .expect("live modal coin prompt must restore")
        .into_game_state()
        .unwrap();
    events.extend(
        runner
            .act(GameAction::SelectModes { indices: vec![0] })
            .unwrap()
            .events,
    );
    finish(&mut runner, &mut events);
    assert_eq!(flips(&events), vec![true]);
    assert_eq!(
        runner.life(P0),
        23,
        "only selected coin reward, not other mode"
    );
    assert_finished(&runner);
}

#[test]
fn fanout_attachment_tail_keeps_enchant_restriction_with_and_without_coin_parent() {
    use engine::game::effects::attach::attach_to;
    use engine::types::ability::{
        AbilityDefinition, AbilityKind, AttachSelection, ControllerRef, MultiTargetSpec,
        PlayerFilter, QuantityExpr, QuantityRef, TypedFilter,
    };
    use engine::types::keywords::Keyword;
    for coin_parent in [false, true] {
        let mut fanout = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::GainControl {
                target: TargetFilter::Typed(
                    TypedFilter::permanent().controller(ControllerRef::TargetPlayer),
                ),
            },
        )
        .sub_ability(AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Attach {
                attachment: TargetFilter::SelfRef,
                target: TargetFilter::Typed(TypedFilter::permanent()),
                selection: AttachSelection::Targeted,
            },
        ));
        fanout.multi_target = Some(MultiTargetSpec::bounded(
            1,
            QuantityExpr::Ref {
                qty: QuantityRef::PlayerCount {
                    filter: PlayerFilter::Opponent,
                },
            },
        ));
        let root = if coin_parent {
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::FlipCoins {
                    flips: CoinFlipRun::single(),
                    win_effect: Some(Box::new(fanout)),
                    lose_effect: None,
                    flipper: TargetFilter::Controller,
                },
            )
        } else {
            fanout
        };
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let original = scenario.add_creature(P0, "Original host", 2, 2).id();
        let legal = scenario.add_creature(P0, "New creature host", 3, 3).id();
        let incompatible = scenario
            .add_artifact_from_oracle(P0, "Not a creature host", "")
            .id();
        let opponent = scenario
            .add_artifact_from_oracle(P1, "Fanout subject", "")
            .id();
        let aura = scenario
            .add_creature(P0, "Synthetic moving Aura", 0, 0)
            .as_enchantment()
            .with_subtypes(vec!["Aura"])
            .with_keyword(Keyword::Enchant(TargetFilter::Typed(
                TypedFilter::creature(),
            )))
            .with_ability_definition(root)
            .id();
        let mut runner = scenario.build();
        attach_to(runner.state_mut(), aura, original);
        pin_outcomes(&mut runner, &[true]);
        let mut events = runner
            .act(GameAction::ActivateAbility {
                source_id: aura,
                ability_index: 0,
                payment_mode: CastPaymentMode::Auto,
            })
            .unwrap()
            .events;
        // Only a fanout at the targeting root auto-selects its fixed binder.
        // Below a coin child, answer the exposed singleton player prompt first.
        if coin_parent {
            let WaitingFor::TargetSelection { selection, .. } = &runner.state().waiting_for else {
                panic!(
                    "required nested fanout binder: {:?}",
                    runner.state().waiting_for
                );
            };
            assert_eq!(selection.current_legal_targets, vec![TargetRef::Player(P1)]);
            events.extend(
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(TargetRef::Player(P1)),
                    })
                    .unwrap()
                    .events,
            );
        }
        let WaitingFor::TargetSelection { selection, .. } = &runner.state().waiting_for else {
            panic!(
                "required fanout object prompt: {:?}",
                runner.state().waiting_for
            );
        };
        assert!(
            selection
                .current_legal_targets
                .contains(&TargetRef::Object(opponent)),
            "fanout subject must be offered before the host pool (coin_parent={coin_parent}): {:?}",
            selection.current_legal_targets
        );
        events.extend(
            runner
                .act(GameAction::ChooseTarget {
                    target: Some(TargetRef::Object(opponent)),
                })
                .unwrap()
                .events,
        );
        let WaitingFor::TargetSelection { selection, .. } = &runner.state().waiting_for else {
            panic!(
                "required attachment host prompt: {:?}",
                runner.state().waiting_for
            );
        };
        assert!(
            selection
                .current_legal_targets
                .contains(&TargetRef::Object(legal)),
            "reach: legal host offered"
        );
        assert!(
            !selection
                .current_legal_targets
                .contains(&TargetRef::Object(incompatible)),
            "Aura Enchant restriction survives the fanout's SubAbility path"
        );
        // Only the host pool is under test. Continuation assignment after a
        // fanout root is unchanged base behavior (ability_utils.rs:2179-2183,
        // 2206-2210), so attachment payoff is deliberately not asserted here.
        events.extend(
            runner
                .act(GameAction::ChooseTarget {
                    target: Some(TargetRef::Object(legal)),
                })
                .unwrap()
                .events,
        );
        finish(&mut runner, &mut events);
        assert_finished(&runner);
    }
}

#[test]
fn mixed_incoming_pins_round_trip_through_a_production_thumb_keep() {
    use engine::types::game_state::PersistedGameState;
    use engine::types::identifiers::ObjectIncarnationRef;
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_artifact_from_oracle(
        P0,
        "Krark's Thumb",
        "If you would flip a coin, instead flip two coins and ignore one.",
    );
    let a = scenario
        .add_creature(P0, "Earlier pinned referent", 2, 2)
        .id();
    let b = scenario
        .add_creature(P0, "Resolution-time referent", 3, 3)
        .id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Synthetic mixed incoming coin",
            true,
            "Flip a coin. If you win the flip, you gain 3 life.",
        )
        .id();
    let mut runner = scenario.build();
    pin_outcomes(&mut runner, &[true, false]);
    drop(runner.cast(spell).commit());
    let a_pin = ObjectIncarnationRef::from_object(&runner.state().objects[&a]);
    let b_pin = ObjectIncarnationRef::from_object(&runner.state().objects[&b]);
    // Stage the live incoming channel produced by an earlier instruction:
    // one announced occurrence plus an object appended during resolution.
    runner
        .state_mut()
        .stack
        .back_mut()
        .unwrap()
        .ability_mut()
        .unwrap()
        .replace_target_occurrences(vec![
            (TargetRef::Object(a), Some(a_pin)),
            (TargetRef::Object(b), None),
        ]);
    let mut events = Vec::new();
    pass_to_coin_keep(&mut runner, &mut events);
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::CoinFlipKeepChoice { .. }
    ));
    let incoming = &runner
        .state()
        .active_coin_flip_frame()
        .unwrap()
        .branch_context
        .parent_occurrences;
    assert_eq!(
        incoming.targets,
        vec![TargetRef::Object(a), TargetRef::Object(b)]
    );
    runner.state_mut().capture_rng_word_pos();
    let wire = serde_json::to_value(PersistedGameState::capture(runner.state().clone())).unwrap();
    *runner.state_mut() = serde_json::from_value::<PersistedGameState>(wire)
        .expect("each incoming object must have a deterministic captured pin")
        .into_game_state()
        .unwrap();
    assert_eq!(
        runner
            .state()
            .active_coin_flip_frame()
            .unwrap()
            .branch_context
            .parent_occurrences
            .pins,
        vec![Some(a_pin), Some(b_pin)]
    );
    events.extend(
        runner
            .act(GameAction::SelectCoinFlips {
                keep_indices: vec![0],
            })
            .unwrap()
            .events,
    );
    finish(&mut runner, &mut events);
    assert_eq!(flips(&events), vec![true]);
    assert_eq!(runner.life(P0), 23);
    assert_finished(&runner);
}

#[test]
fn both_coin_branch_targets_emit_events_and_the_heroic_target_triggers_once() {
    use engine::types::game_state::StackEntryKind;
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let hero = scenario
        .add_creature_from_oracle(
            P0,
            "Synthetic heroic life reward",
            2,
            2,
            "Whenever you cast a spell that targets this creature, you gain 2 life.",
        )
        .id();
    let other = scenario.add_creature(P0, "Other branch target", 3, 3).id();
    let spell = scenario.add_spell_to_hand_from_oracle(P0, "Synthetic heroic coin", true,
        "Flip a coin. If you win the flip, target creature gets +1/+1 until end of turn. If you lose the flip, target creature gets +2/+2 until end of turn.").id();
    let mut runner = scenario.build();
    pin_outcomes(&mut runner, &[true]);
    let card_id = runner.state().objects[&spell].card_id;
    let mut events = runner
        .act(GameAction::CastSpell {
            card_id,
            object_id: spell,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .unwrap()
        .events;
    for target in [hero, other] {
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::TargetSelection { .. }
        ));
        events.extend(
            runner
                .act(GameAction::ChooseTarget {
                    target: Some(TargetRef::Object(target)),
                })
                .unwrap()
                .events,
        );
    }
    for target in [hero, other] {
        assert!(events.iter().any(
            |event| matches!(event, GameEvent::BecomesTarget { source_id, target: chosen, .. }
            if *source_id == spell && *chosen == TargetRef::Object(target))
        ));
    }
    let triggers = runner
        .state()
        .stack
        .iter()
        .filter(|entry| {
            entry.source_id == hero && matches!(entry.kind, StackEntryKind::TriggeredAbility { .. })
        })
        .count();
    assert_eq!(
        triggers, 1,
        "reach: heroic observes the announced coin branch target"
    );
    finish(&mut runner, &mut events);
    assert_eq!(runner.life(P0), 22, "one heroic reward");
    assert_eq!(flips(&events), vec![true]);
    assert_eq!(runner.state().objects[&hero].power, Some(3));
    assert_eq!(runner.state().objects[&other].power, Some(3));
    assert_finished(&runner);
}

#[test]
fn non_coin_delayed_reference_retains_its_creation_pin_across_a_blink() {
    use engine::types::ability::{
        AbilityDefinition, AbilityKind, DelayedTriggerCondition, QuantityExpr, TypedFilter,
    };
    use engine::types::counter::CounterType;
    for blink in [false, true] {
        let definition = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::TargetOnly {
                target: TargetFilter::Typed(TypedFilter::creature()),
            },
        )
        .sub_ability(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::CreateDelayedTrigger {
                condition: DelayedTriggerCondition::AtNextPhase { phase: Phase::End },
                effect: Box::new(AbilityDefinition::new(
                    AbilityKind::Spell,
                    Effect::PutCounter {
                        counter_type: CounterType::Plus1Plus1,
                        count: QuantityExpr::Fixed { value: 1 },
                        target: TargetFilter::ParentTarget,
                    },
                )),
                uses_tracked_set: false,
            },
        ));
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PostCombatMain);
        let creature = scenario
            .add_creature(P0, "Delayed non-coin referent", 2, 2)
            .id();
        let land = scenario
            .add_land_from_oracle(P0, "Other flicker target", "")
            .id();
        let spell = scenario
            .add_spell_to_hand(P0, "Synthetic non-coin delayed counter", true)
            .with_ability_definition(definition)
            .id();
        let flicker = scenario.add_spell_to_hand_from_oracle(P0, "Synthetic flicker", true,
            "Exile two target artifacts, creatures, and/or lands you control, then return those cards to the battlefield under your control.").id();
        let mut runner = scenario.build();
        runner.cast(spell).target_object(creature).resolve();
        assert_finished(&runner);
        let incarnation = runner.state().objects[&creature].incarnation;
        if blink {
            runner
                .cast(flicker)
                .target_objects(&[creature, land])
                .resolve();
            assert_eq!(
                runner.state().objects[&creature].zone,
                Zone::Battlefield,
                "reach: the blink must return the delayed referent"
            );
            assert_ne!(
                runner.state().objects[&creature].incarnation,
                incarnation,
                "reach: new object"
            );
        }
        runner.advance_to_end_step();
        finish(&mut runner, &mut Vec::new());
        assert_eq!(runner.state().phase, Phase::End);
        // CR 603.7c + CR 400.7: the delayed reference affects only the original object.
        assert_eq!(
            runner.state().objects[&creature]
                .counters
                .get(&CounterType::Plus1Plus1)
                .copied()
                .unwrap_or(0),
            u32::from(!blink)
        );
        assert_finished(&runner);
    }
}

#[test]
fn non_coin_blink_and_exile_life_riders_follow_the_moved_object() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let creature = scenario
        .add_creature(P0, "Non-coin rider referent", 4, 4)
        .id();
    let blink = scenario.add_spell_to_hand_from_oracle(P0, "Cloudshift", true,
        "Exile target creature you control, then return that card to the battlefield under your control.").id();
    let exile = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Swords to Plowshares",
            true,
            "Exile target creature. Its controller gains life equal to its power.",
        )
        .id();
    let mut runner = scenario.build();
    let incarnation = runner.state().objects[&creature].incarnation;
    runner.cast(blink).target_object(creature).resolve();
    assert_ne!(
        runner.state().objects[&creature].incarnation,
        incarnation,
        "reach: exile happened"
    );
    assert_eq!(runner.state().objects[&creature].zone, Zone::Battlefield);
    assert_finished(&runner);
    runner.cast(exile).target_object(creature).resolve();
    assert_eq!(
        runner.state().objects[&creature].zone,
        Zone::Exile,
        "reach: exile happened"
    );
    assert_eq!(
        runner.life(P0),
        24,
        "the rider still reads the exiled creature's power"
    );
    assert_finished(&runner);
}

#[test]
fn coin_damage_any_target_legality_uses_the_declaring_instruction() {
    use engine::types::ability::{AbilityDefinition, AbilityKind, QuantityExpr, TypedFilter};
    for replacement in [false, true] {
        let damage = if replacement {
            serde_json::from_value(serde_json::json!({
                "type": "CreateDamageReplacement",
                "redirect_to": { "type": "ChosenTarget" },
                "redirect_object_filter": { "type": "Any" }
            }))
            .unwrap()
        } else {
            Effect::DealDamage {
                target: TargetFilter::Any,
                amount: QuantityExpr::Fixed { value: 2 },
                damage_source: None,
                excess: None,
            }
        };
        let mut branch = AbilityDefinition::new(AbilityKind::Spell, damage);
        if replacement {
            branch = AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::TargetOnly {
                    target: TargetFilter::Typed(TypedFilter::creature()),
                },
            )
            .sub_ability(branch);
        }
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let creature = scenario.add_creature(P1, "Damage candidate", 4, 4).id();
        let land = scenario
            .add_land_from_oracle(P1, "Not a damage target", "")
            .id();
        let artifact = scenario
            .add_artifact_from_oracle(P1, "Not a creature", "")
            .id();
        let spell = scenario
            .add_spell_to_hand(P0, "Synthetic damage target coin", true)
            .with_ability(Effect::FlipCoins {
                flips: CoinFlipRun::single(),
                win_effect: Some(Box::new(branch)),
                lose_effect: None,
                flipper: TargetFilter::Controller,
            })
            .id();
        let mut runner = scenario.build();
        pin_outcomes(&mut runner, &[true]);
        let card_id = runner.state().objects[&spell].card_id;
        let mut events = runner
            .act(GameAction::CastSpell {
                object_id: spell,
                card_id,
                targets: vec![],
                payment_mode: CastPaymentMode::Auto,
            })
            .unwrap()
            .events;
        if replacement {
            runner
                .act(GameAction::ChooseTarget {
                    target: Some(TargetRef::Object(creature)),
                })
                .unwrap();
        }
        let WaitingFor::TargetSelection { selection, .. } = &runner.state().waiting_for else {
            panic!("required any-target prompt");
        };
        let offered = selection.current_legal_targets.clone();
        for legal in [
            TargetRef::Player(P0),
            TargetRef::Player(P1),
            TargetRef::Object(creature),
        ] {
            assert!(offered.contains(&legal), "reach: {legal:?} offered");
        }
        for illegal in [land, artifact] {
            assert!(
                !offered.contains(&TargetRef::Object(illegal)),
                "non-damage permanent offered (replacement={replacement})"
            );
            assert!(runner
                .act(GameAction::ChooseTarget {
                    target: Some(TargetRef::Object(illegal))
                })
                .is_err());
        }
        events.extend(
            runner
                .act(GameAction::ChooseTarget {
                    target: Some(TargetRef::Player(P1)),
                })
                .unwrap()
                .events,
        );
        finish(&mut runner, &mut events);
        assert_eq!(flips(&events), vec![true]);
        if replacement {
            assert_eq!(
                runner.state().pending_damage_replacements.len(),
                1,
                "reach: shield installed"
            );
        } else {
            assert_eq!(runner.life(P1), 18, "reach: selected player damaged");
        }
        assert_finished(&runner);
    }
}

#[test]
fn coin_own_declaration_shadows_an_empty_incoming_result_inline_and_after_restore() {
    use engine::types::ability::{
        AbilityDefinition, AbilityKind, ControllerRef, TargetChoiceTiming, TypedFilter,
    };
    use engine::types::game_state::PersistedGameState;
    for thumb in [false, true] {
        let branch = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::TargetOnly {
                target: TargetFilter::Typed(TypedFilter::creature()),
            },
        )
        .sub_ability(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::GiveControl {
                target: TargetFilter::ParentTarget,
                recipient: TargetFilter::SpecificPlayer { id: P0 },
            },
        ));
        let mut definition = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::TargetOnly {
                target: TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::You)),
            },
        )
        .sub_ability(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::FlipCoins {
                flips: CoinFlipRun::single(),
                win_effect: Some(Box::new(branch)),
                lose_effect: None,
                flipper: TargetFilter::Controller,
            },
        ));
        definition.target_choice_timing = TargetChoiceTiming::Resolution;
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        if thumb {
            scenario.add_artifact_from_oracle(
                P0,
                "Krark's Thumb",
                "If you would flip a coin, instead flip two coins and ignore one.",
            );
        }
        let chosen = scenario
            .add_creature(P1, "Branch's declared referent", 2, 2)
            .id();
        let other = scenario.add_creature(P1, "Unchosen referent", 3, 3).id();
        let spell = scenario
            .add_spell_to_hand(P0, "Synthetic empty then declared coin", true)
            .with_ability_definition(definition)
            .id();
        let mut runner = scenario.build();
        pin_outcomes(&mut runner, &[true, false]);
        drop(runner.cast(spell).target_object(chosen).commit());
        let mut events = Vec::new();
        if thumb {
            pass_to_coin_keep(&mut runner, &mut events);
            assert!(
                runner
                    .state()
                    .active_coin_flip_frame()
                    .unwrap()
                    .branch_context
                    .forwarded_result_context
                    .as_ref()
                    .unwrap()
                    .targets
                    .is_empty(),
                "reach: completed empty choice"
            );
            runner.state_mut().capture_rng_word_pos();
            let wire =
                serde_json::to_value(PersistedGameState::capture(runner.state().clone())).unwrap();
            *runner.state_mut() = serde_json::from_value::<PersistedGameState>(wire)
                .unwrap()
                .into_game_state()
                .unwrap();
            events.extend(
                runner
                    .act(GameAction::SelectCoinFlips {
                        keep_indices: vec![0],
                    })
                    .unwrap()
                    .events,
            );
        }
        finish(&mut runner, &mut events);
        assert_eq!(flips(&events), vec![true], "reach: selected branch ran");
        assert_eq!(
            runner.state().objects[&chosen].controller,
            P0,
            "own target must reach its rider"
        );
        assert_eq!(runner.state().objects[&other].controller, P1);
        assert_finished(&runner);
    }
}

#[test]
fn coin_attachment_host_uses_its_own_branch_aura() {
    use engine::game::effects::attach::attach_to;
    use engine::types::ability::{AbilityDefinition, AbilityKind, AttachSelection, TypedFilter};
    use engine::types::keywords::Keyword;
    let branch = || {
        AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::TargetOnly {
                target: TargetFilter::Typed(TypedFilter::permanent()),
            },
        )
        .sub_ability(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Attach {
                attachment: TargetFilter::ParentTarget,
                target: TargetFilter::Typed(TypedFilter::permanent()),
                selection: AttachSelection::Targeted,
            },
        ))
    };
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let original_creature = scenario
        .add_creature(P0, "Original creature host", 2, 2)
        .id();
    let new_creature = scenario.add_creature(P0, "New creature host", 3, 3).id();
    let original_land = scenario
        .add_land_from_oracle(P0, "Original land host", "")
        .id();
    let new_land = scenario.add_land_from_oracle(P0, "New land host", "").id();
    let aura_a = scenario
        .add_creature(P0, "Enchant land Aura", 0, 0)
        .as_enchantment()
        .with_subtypes(vec!["Aura"])
        .with_keyword(Keyword::Enchant(TargetFilter::Typed(TypedFilter::land())))
        .id();
    let aura_b = scenario
        .add_creature(P0, "Enchant creature Aura", 0, 0)
        .as_enchantment()
        .with_subtypes(vec!["Aura"])
        .with_keyword(Keyword::Enchant(TargetFilter::Typed(
            TypedFilter::creature(),
        )))
        .id();
    let spell = scenario
        .add_spell_to_hand(P0, "Synthetic two Aura branches", true)
        .with_ability(Effect::FlipCoins {
            flips: CoinFlipRun::single(),
            win_effect: Some(Box::new(branch())),
            lose_effect: Some(Box::new(branch())),
            flipper: TargetFilter::Controller,
        })
        .id();
    let mut runner = scenario.build();
    attach_to(runner.state_mut(), aura_a, original_land);
    attach_to(runner.state_mut(), aura_b, original_creature);
    pin_outcomes(&mut runner, &[false]);
    let card_id = runner.state().objects[&spell].card_id;
    let mut events = runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .unwrap()
        .events;
    for target in [aura_a, new_land, aura_b] {
        events.extend(
            runner
                .act(GameAction::ChooseTarget {
                    target: Some(TargetRef::Object(target)),
                })
                .unwrap()
                .events,
        );
    }
    let WaitingFor::TargetSelection { selection, .. } = &runner.state().waiting_for else {
        panic!("required lose-branch host prompt");
    };
    assert!(
        selection
            .current_legal_targets
            .contains(&TargetRef::Object(new_creature)),
        "reach: B's creature host offered"
    );
    assert!(
        !selection
            .current_legal_targets
            .contains(&TargetRef::Object(new_land)),
        "A's land restriction must not leak"
    );
    assert!(runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(new_land))
        })
        .is_err());
    events.extend(
        runner
            .act(GameAction::ChooseTarget {
                target: Some(TargetRef::Object(new_creature)),
            })
            .unwrap()
            .events,
    );
    finish(&mut runner, &mut events);
    assert_eq!(flips(&events), vec![false]);
    // Announcement-pool/validation coverage only. The existing Attach chain
    // binding loss documented in effects/attach.rs is not repaired here.
    assert_finished(&runner);
}

#[test]
fn non_coin_forwarded_parent_target_resolution_rider_keeps_its_bound_object() {
    use engine::types::ability::{
        AbilityDefinition, AbilityKind, QuantityExpr, TargetChoiceTiming,
    };
    use engine::types::counter::CounterType;
    let mut definition = parse_oracle_text(
        "Return target creature card from your graveyard to the battlefield.",
        "Synthetic non-coin forwarding",
        &[],
        &["Instant".into()],
        &[],
    )
    .abilities
    .remove(0);
    assert!(matches!(
        definition.effect.as_ref(),
        Effect::ChangeZone { .. }
    ));
    definition.forward_result = true;
    let mut rider = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::PutCounter {
            counter_type: CounterType::Plus1Plus1,
            count: QuantityExpr::Fixed { value: 2 },
            target: TargetFilter::ParentTarget,
        },
    );
    rider.target_choice_timing = TargetChoiceTiming::Resolution;
    definition.sub_ability = Some(Box::new(rider));
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let target = scenario
        .add_creature_to_graveyard(P0, "Returned non-coin referent", 2, 2)
        .id();
    let other = scenario.add_creature(P0, "Unrelated creature", 3, 3).id();
    let spell = scenario
        .add_spell_to_hand(P0, "Synthetic non-coin forwarding", true)
        .with_ability_definition(definition)
        .id();
    let mut runner = scenario.build();
    runner.cast(spell).target_object(target).resolve();
    assert_eq!(
        runner.state().objects[&target].zone,
        Zone::Battlefield,
        "reach: producer moved the object"
    );
    assert_eq!(
        runner.state().objects[&target]
            .counters
            .get(&CounterType::Plus1Plus1),
        Some(&2)
    );
    assert!(!runner.state().objects[&other]
        .counters
        .contains_key(&CounterType::Plus1Plus1));
    assert_finished(&runner);
}

#[test]
fn coin_lost_dependent_target_uses_its_lexical_creature_not_won_artifact() {
    use engine::game::ability_utils::{build_resolved_from_def, build_target_slots};
    use engine::types::ability::{
        AbilityDefinition, AbilityKind, FilterProp, SharedQuality, SharedQualityRelation,
        TypedFilter,
    };
    let declare =
        |target| AbilityDefinition::new(AbilityKind::Spell, Effect::TargetOnly { target });
    let lost =
        declare(TargetFilter::Typed(TypedFilter::creature())).sub_ability(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Destroy {
                target: TargetFilter::Typed(TypedFilter::permanent().properties(vec![
                    FilterProp::SharesQuality {
                        quality: SharedQuality::CardType,
                        reference: Some(Box::new(TargetFilter::ParentTarget)),
                        relation: SharedQualityRelation::Shares,
                    },
                ])),
                cant_regenerate: false,
            },
        ));
    let definition = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::FlipCoins {
            flips: CoinFlipRun::single(),
            win_effect: Some(Box::new(declare(TargetFilter::Typed(TypedFilter::new(
                engine::types::ability::TypeFilter::Artifact,
            ))))),
            lose_effect: Some(Box::new(lost)),
            flipper: TargetFilter::Controller,
        },
    );
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let a = scenario
        .add_artifact_from_oracle(P0, "Won artifact", "")
        .id();
    let b = scenario.add_creature(P0, "Lost antecedent", 2, 2).id();
    let c = scenario.add_creature(P1, "Shared creature type", 3, 3).id();
    let spell = scenario
        .add_spell_to_hand(P0, "Synthetic lexical target coin", true)
        .with_ability_definition(definition.clone())
        .id();
    let mut runner = scenario.build();
    let resolved = build_resolved_from_def(&definition, spell, P0);
    let slots = build_target_slots(runner.state(), &resolved).unwrap();
    assert!(slots[2].legal_targets.contains(&TargetRef::Object(c)));
    assert!(!slots[2].legal_targets.contains(&TargetRef::Object(a)));
    pin_outcomes(&mut runner, &[false]);
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .unwrap();
    for target in [a, b] {
        runner
            .act(GameAction::ChooseTarget {
                target: Some(TargetRef::Object(target)),
            })
            .unwrap();
    }
    let WaitingFor::TargetSelection { selection, .. } = &runner.state().waiting_for else {
        panic!("required dependent lost-branch target prompt");
    };
    assert!(selection
        .current_legal_targets
        .contains(&TargetRef::Object(c)));
    assert!(!selection
        .current_legal_targets
        .contains(&TargetRef::Object(a)));
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(c)),
        })
        .unwrap();
    let mut events = Vec::new();
    finish(&mut runner, &mut events);
    assert_eq!(flips(&events), vec![false]);
    assert_eq!(runner.state().objects[&c].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&a].zone, Zone::Battlefield);
    assert_eq!(runner.state().objects[&b].zone, Zone::Battlefield);
    assert_finished(&runner);
}

#[test]
fn coin_lost_numbered_reference_keeps_branch_base_and_outer_legality_across_restore() {
    use engine::types::ability::{
        AbilityDefinition, AbilityKind, ControllerRef, QuantityExpr, TypedFilter,
    };
    use engine::types::counter::CounterType;
    use engine::types::game_state::PersistedGameState;
    for thumb in [false, true] {
        for illegal in [None, Some(0), Some(1)] {
            let declare = |controller| {
                AbilityDefinition::new(
                    AbilityKind::Spell,
                    Effect::TargetOnly {
                        target: TargetFilter::Typed(TypedFilter::creature().controller(controller)),
                    },
                )
            };
            let lost = declare(ControllerRef::You).sub_ability(
                declare(ControllerRef::Opponent).sub_ability(AbilityDefinition::new(
                    AbilityKind::Spell,
                    Effect::PutCounter {
                        counter_type: CounterType::Plus1Plus1,
                        count: QuantityExpr::Fixed { value: 2 },
                        target: TargetFilter::ParentTargetSlot { index: 0 },
                    },
                )),
            );
            let mut scenario = GameScenario::new();
            scenario.at_phase(Phase::PreCombatMain);
            if thumb {
                scenario.add_artifact_from_oracle(
                    P0,
                    "Krark's Thumb",
                    "If you would flip a coin, instead flip two coins and ignore one.",
                );
            }
            let a = scenario.add_creature(P0, "Won creature A", 2, 4).id();
            let b = scenario.add_creature(P0, "Lost creature B", 3, 4).id();
            let c = scenario.add_creature(P1, "Lost opponent C", 4, 4).id();
            let spell = scenario
                .add_spell_to_hand(P0, "Synthetic numbered coin", true)
                .with_ability(Effect::FlipCoins {
                    flips: CoinFlipRun::single(),
                    win_effect: Some(Box::new(declare(ControllerRef::You))),
                    lose_effect: Some(Box::new(lost)),
                    flipper: TargetFilter::Controller,
                })
                .id();
            let mut runner = scenario.build();
            pin_outcomes(&mut runner, &[false, true]);
            drop(runner.cast(spell).target_objects(&[a, b, c]).commit());
            if let Some(index) = illegal {
                // CR 608.2b: an opponent-controlled creature no longer satisfies "you control".
                runner
                    .state_mut()
                    .objects
                    .get_mut(&[a, b][index])
                    .unwrap()
                    .controller = P1;
            }
            let mut events = Vec::new();
            if thumb {
                pass_to_coin_keep(&mut runner, &mut events);
                runner.state_mut().capture_rng_word_pos();
                let wire =
                    serde_json::to_value(PersistedGameState::capture(runner.state().clone()))
                        .unwrap();
                *runner.state_mut() = serde_json::from_value::<PersistedGameState>(wire)
                    .unwrap()
                    .into_game_state()
                    .unwrap();
                events.extend(
                    runner
                        .act(GameAction::SelectCoinFlips {
                            keep_indices: vec![0],
                        })
                        .unwrap()
                        .events,
                );
            }
            finish(&mut runner, &mut events);
            assert_eq!(
                flips(&events),
                vec![false],
                "thumb={thumb}, illegal={illegal:?}"
            );
            for (target, expected) in [(a, 0), (b, if illegal == Some(1) { 0 } else { 2 }), (c, 0)]
            {
                assert_eq!(
                    runner.state().objects[&target]
                        .counters
                        .get(&CounterType::Plus1Plus1)
                        .copied()
                        .unwrap_or(0),
                    expected,
                    "target={target:?}, thumb={thumb}, illegal={illegal:?}"
                );
            }
            assert_finished(&runner);
        }
    }
}

#[test]
fn declaring_root_delayed_pin_still_guards_its_inherited_counter_rider() {
    use engine::game::ability_utils::build_resolved_from_def;
    use engine::game::effects;
    use engine::types::ability::{AbilityDefinition, AbilityKind, QuantityExpr, TypedFilter};
    use engine::types::counter::CounterType;
    use engine::types::identifiers::ObjectIncarnationRef;
    for stale in [false, true] {
        let definition = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::TargetOnly {
                target: TargetFilter::Typed(TypedFilter::creature()),
            },
        )
        .sub_ability(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::PutCounter {
                counter_type: CounterType::Plus1Plus1,
                count: QuantityExpr::Fixed { value: 3 },
                target: TargetFilter::ParentTarget,
            },
        ));
        let mut scenario = GameScenario::new();
        let target = scenario
            .add_creature(P0, "Captured delayed referent", 2, 2)
            .id();
        let mut runner = scenario.build();
        let pin = ObjectIncarnationRef::from_object(&runner.state().objects[&target]);
        let mut root = build_resolved_from_def(&definition, ObjectId(100), P0);
        root.set_unpinned_targets(vec![TargetRef::Object(target)]);
        root.sub_ability
            .as_mut()
            .unwrap()
            .set_unpinned_targets(vec![TargetRef::Object(target)]);
        root.set_target_incarnations_recursive(vec![pin]);
        if stale {
            runner
                .state_mut()
                .objects
                .get_mut(&target)
                .unwrap()
                .incarnation += 1;
        }
        let mut events = Vec::new();
        effects::resolve_ability_chain(
            runner.state_mut(),
            root.sub_ability.as_ref().unwrap(),
            &mut events,
            0,
        )
        .unwrap();
        assert_eq!(
            runner.state().objects[&target]
                .counters
                .get(&CounterType::Plus1Plus1)
                .copied()
                .unwrap_or(0),
            if stale { 0 } else { 3 }
        );
        assert_finished(&runner);
    }
}

fn same_node_coin_scope(won: bool) {
    use engine::types::ability::{
        AbilityDefinition, AbilityKind, FilterProp, SharedQuality, SharedQualityRelation,
        TypedFilter,
    };
    let branch = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::Fight {
            subject: TargetFilter::Typed(TypedFilter::creature()),
            target: TargetFilter::Typed(TypedFilter::permanent().properties(vec![
                FilterProp::SharesQuality {
                    quality: SharedQuality::CardType,
                    reference: Some(Box::new(TargetFilter::ParentTarget)),
                    relation: SharedQualityRelation::Shares,
                },
            ])),
        },
    );
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let first = scenario.add_creature(P0, "First fighter", 2, 2).id();
    let second = scenario.add_creature(P1, "Second fighter", 3, 3).id();
    let artifact = scenario
        .add_artifact_from_oracle(P0, "Unrelated artifact", "")
        .id();
    let machine = scenario
        .add_creature(P0, "Artifact creature fighter", 4, 4)
        .as_artifact_creature()
        .id();
    let spell = scenario
        .add_spell_to_hand(P0, "Synthetic same-node coin targets", true)
        .with_ability(Effect::FlipCoins {
            flips: CoinFlipRun::single(),
            win_effect: won.then(|| Box::new(branch.clone())),
            lose_effect: (!won).then(|| Box::new(branch)),
            flipper: TargetFilter::Controller,
        })
        .id();
    let mut runner = scenario.build();
    pin_outcomes(&mut runner, &[won]);
    let card_id = runner.state().objects[&spell].card_id;
    let mut events = runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("same-node dependency admits the spell")
        .events;
    let WaitingFor::TargetSelection { target_slots, .. } = &runner.state().waiting_for else {
        panic!("required first fighter prompt");
    };
    // CR 601.2c: the later pool ranges over this instruction's earlier object slot.
    let static_pool = target_slots[1].legal_targets.clone();
    assert!(static_pool.contains(&TargetRef::Object(first)));
    assert!(static_pool.contains(&TargetRef::Object(second)));
    assert!(static_pool.contains(&TargetRef::Object(machine)));
    assert!(static_pool.contains(&TargetRef::Object(artifact)));
    events.extend(
        runner
            .act(GameAction::ChooseTarget {
                target: Some(TargetRef::Object(first)),
            })
            .unwrap()
            .events,
    );
    let WaitingFor::TargetSelection { selection, .. } = &runner.state().waiting_for else {
        panic!("required second fighter prompt");
    };
    let narrowed_pool: Vec<_> = static_pool
        .into_iter()
        .filter(|target| *target != TargetRef::Object(artifact))
        .collect();
    assert_eq!(selection.current_legal_targets, narrowed_pool);
    events.extend(
        runner
            .act(GameAction::ChooseTarget {
                target: Some(TargetRef::Object(second)),
            })
            .unwrap()
            .events,
    );
    finish(&mut runner, &mut events);
    assert_eq!(
        flips(&events),
        vec![won],
        "reach: the requested outcome executes"
    );
    assert_eq!(runner.state().objects[&first].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&second].zone, Zone::Battlefield);
    assert_eq!(runner.state().objects[&artifact].zone, Zone::Battlefield);
    assert_eq!(runner.state().objects[&machine].zone, Zone::Battlefield);
    assert_finished(&runner);
}

#[test]
fn coin_scope_same_node_won_static_pool_matches_live_prompt() {
    same_node_coin_scope(true);
}

#[test]
fn coin_scope_same_node_lost_static_pool_matches_live_prompt() {
    same_node_coin_scope(false);
}

#[test]
fn coin_scope_numbered_declared_view_uses_lost_branch_base() {
    use engine::game::effects::attach::attach_to;
    use engine::types::ability::{
        AbilityDefinition, AbilityKind, AttachmentReferent, FilterProp, TypedFilter,
    };
    let declare = || {
        AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::TargetOnly {
                target: TargetFilter::Typed(TypedFilter::creature()),
            },
        )
    };
    let lost = declare().sub_ability(AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::Destroy {
            target: TargetFilter::Typed(TypedFilter::permanent().properties(vec![
                FilterProp::AttachedTo {
                    to: AttachmentReferent::DeclaredTarget { slot: 0 },
                },
            ])),
            cant_regenerate: false,
        },
    ));
    let definition = declare().sub_ability(AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::FlipCoins {
            flips: CoinFlipRun::single(),
            win_effect: Some(Box::new(declare())),
            lose_effect: Some(Box::new(lost)),
            flipper: TargetFilter::Controller,
        },
    ));
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let outer = scenario.add_creature(P0, "Outer target", 2, 2).id();
    let won = scenario.add_creature(P0, "Won target", 2, 2).id();
    let lost = scenario.add_creature(P1, "Lost target", 2, 2).id();
    let wrong = scenario
        .add_artifact_from_oracle(P0, "Outer attachment", "")
        .with_subtypes(vec!["Equipment"])
        .id();
    let right = scenario
        .add_artifact_from_oracle(P0, "Lost attachment", "")
        .with_subtypes(vec!["Equipment"])
        .id();
    let spell = scenario
        .add_spell_to_hand(P0, "Synthetic numbered attachment coin", true)
        .with_ability_definition(definition)
        .id();
    let mut runner = scenario.build();
    attach_to(runner.state_mut(), wrong, outer);
    attach_to(runner.state_mut(), right, lost);
    pin_outcomes(&mut runner, &[false]);
    let card_id = runner.state().objects[&spell].card_id;
    let mut events = runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .unwrap()
        .events;
    for target in [outer, won, lost] {
        events.extend(
            runner
                .act(GameAction::ChooseTarget {
                    target: Some(TargetRef::Object(target)),
                })
                .unwrap()
                .events,
        );
    }
    let WaitingFor::TargetSelection { selection, .. } = &runner.state().waiting_for else {
        panic!("required branch-local attachment prompt");
    };
    // CR 601.2c: the numbered referent is the lost branch's first declaration.
    assert_eq!(
        selection.current_legal_targets,
        vec![TargetRef::Object(right)]
    );
    events.extend(
        runner
            .act(GameAction::ChooseTarget {
                target: Some(TargetRef::Object(right)),
            })
            .unwrap()
            .events,
    );
    // CR 704.5p: legal Equipment must remain attached through announcement;
    // a plain artifact would be detached before the resolution check.
    assert_eq!(
        runner.state().objects[&right]
            .attached_to
            .and_then(|host| host.as_object()),
        Some(lost)
    );
    assert_eq!(
        runner.state().objects[&wrong]
            .attached_to
            .and_then(|host| host.as_object()),
        Some(outer)
    );
    finish(&mut runner, &mut events);
    assert_eq!(flips(&events), vec![false]);
    // CR 608.2b: stack resolution re-validates the chain through the branch-local
    // view (`coin_view_base`), so the numbered referent still names the lost
    // branch's first declaration; the outer attachment is never a legal target.
    assert_eq!(runner.state().objects[&right].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&wrong].zone, Zone::Battlefield);
    assert_finished(&runner);
}

#[test]
fn coin_scope_player_declaration_does_not_shadow_object_antecedent() {
    use engine::types::ability::{
        AbilityDefinition, AbilityKind, FilterProp, SharedQuality, SharedQualityRelation,
        TypedFilter,
    };
    for changed in [false, true] {
        let branch = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::TargetOnly {
                target: TargetFilter::Typed(TypedFilter::creature()),
            },
        )
        .sub_ability(
            AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::TargetOnly {
                    target: TargetFilter::Player,
                },
            )
            .sub_ability(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Destroy {
                    target: TargetFilter::Typed(TypedFilter::permanent().properties(vec![
                        FilterProp::SharesQuality {
                            quality: SharedQuality::CardType,
                            reference: Some(Box::new(TargetFilter::ParentTarget)),
                            relation: SharedQualityRelation::Shares,
                        },
                    ])),
                    cant_regenerate: false,
                },
            )),
        );
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let antecedent = scenario.add_creature(P0, "Object antecedent", 2, 2).id();
        let victim = scenario.add_creature(P1, "Dependent victim", 3, 3).id();
        let artifact = scenario
            .add_artifact_from_oracle(P0, "Unrelated artifact", "")
            .id();
        let spell = scenario
            .add_spell_to_hand(P0, "Synthetic player-interposed coin", true)
            .with_ability(Effect::FlipCoins {
                flips: CoinFlipRun::single(),
                win_effect: None,
                lose_effect: Some(Box::new(branch)),
                flipper: TargetFilter::Controller,
            })
            .id();
        let mut runner = scenario.build();
        pin_outcomes(&mut runner, &[false]);
        runner
            .cast(spell)
            .target_objects(&[antecedent, victim])
            .target_player(P1)
            .commit();
        if changed {
            // CR 608.2b: a now-illegal antecedent cannot supply information.
            runner
                .state_mut()
                .objects
                .get_mut(&antecedent)
                .unwrap()
                .card_types
                .core_types = vec![engine::types::card_type::CoreType::Artifact];
        }
        let mut events = Vec::new();
        finish(&mut runner, &mut events);
        assert_eq!(
            flips(&events),
            vec![false],
            "reach: player target keeps the coin resolving"
        );
        assert_eq!(
            runner.state().objects[&victim].zone,
            if changed {
                Zone::Battlefield
            } else {
                Zone::Graveyard
            }
        );
        assert_eq!(runner.state().objects[&antecedent].zone, Zone::Battlefield);
        assert_eq!(runner.state().objects[&artifact].zone, Zone::Battlefield);
        assert_finished(&runner);
    }
}

#[test]
fn non_coin_scope_keeps_first_prefix_object_across_later_declarations() {
    use engine::types::ability::{
        AbilityDefinition, AbilityKind, FilterProp, SharedQuality, SharedQualityRelation,
        TypeFilter, TypedFilter,
    };
    let declare =
        |target| AbilityDefinition::new(AbilityKind::Spell, Effect::TargetOnly { target });
    let definition = declare(TargetFilter::Typed(TypedFilter::new(TypeFilter::Artifact)))
        .sub_ability(
            declare(TargetFilter::Typed(TypedFilter::creature())).sub_ability(
                declare(TargetFilter::Player).sub_ability(AbilityDefinition::new(
                    AbilityKind::Spell,
                    Effect::Destroy {
                        target: TargetFilter::Typed(TypedFilter::permanent().properties(vec![
                            FilterProp::SharesQuality {
                                quality: SharedQuality::CardType,
                                reference: Some(Box::new(TargetFilter::ParentTarget)),
                                relation: SharedQualityRelation::Shares,
                            },
                        ])),
                        cant_regenerate: false,
                    },
                )),
            ),
        );
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let first = scenario
        .add_artifact_from_oracle(P0, "First prefix artifact", "")
        .id();
    let later = scenario
        .add_creature(P0, "Later prefix creature", 2, 2)
        .id();
    let victim = scenario
        .add_artifact_from_oracle(P1, "Matching artifact", "")
        .id();
    let spell = scenario
        .add_spell_to_hand(P0, "Synthetic non-coin prefix targets", true)
        .with_ability_definition(definition)
        .id();
    let mut runner = scenario.build();
    let card_id = runner.state().objects[&spell].card_id;
    let mut events = runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .unwrap()
        .events;
    let WaitingFor::TargetSelection { target_slots, .. } = &runner.state().waiting_for else {
        panic!("required first declaration prompt");
    };
    let expected = vec![TargetRef::Object(first), TargetRef::Object(victim)];
    // Non-coin preservation: the full prefix's first object, not the nearest
    // declaring node, remains the established announcement antecedent.
    assert_eq!(target_slots[3].legal_targets, expected);
    for target in [
        TargetRef::Object(first),
        TargetRef::Object(later),
        TargetRef::Player(P1),
    ] {
        events.extend(
            runner
                .act(GameAction::ChooseTarget {
                    target: Some(target),
                })
                .unwrap()
                .events,
        );
    }
    let WaitingFor::TargetSelection { selection, .. } = &runner.state().waiting_for else {
        panic!("required non-coin dependent target prompt");
    };
    assert_eq!(selection.current_legal_targets, expected);
    events.extend(
        runner
            .act(GameAction::ChooseTarget {
                target: Some(TargetRef::Object(victim)),
            })
            .unwrap()
            .events,
    );
    finish(&mut runner, &mut events);
    assert_eq!(runner.state().objects[&victim].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&first].zone, Zone::Battlefield);
    assert_eq!(runner.state().objects[&later].zone, Zone::Battlefield);
    assert!(flips(&events).is_empty());
    assert_finished(&runner);
}

fn shared_type_destroy() -> engine::types::ability::AbilityDefinition {
    use engine::types::ability::{
        AbilityDefinition, AbilityKind, FilterProp, SharedQuality, SharedQualityRelation,
        TypedFilter,
    };
    AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::Destroy {
            target: TargetFilter::Typed(TypedFilter::permanent().properties(vec![
                FilterProp::SharesQuality {
                    quality: SharedQuality::CardType,
                    reference: Some(Box::new(TargetFilter::ParentTarget)),
                    relation: SharedQualityRelation::Shares,
                },
            ])),
            cant_regenerate: false,
        },
    )
}

#[test]
fn coin_dependent_multitarget_instances_keep_the_ancestor_referent() {
    use engine::types::ability::{
        AbilityDefinition, AbilityKind, ControllerRef, MultiTargetSpec, TypedFilter,
    };
    let mut destroy = shared_type_destroy();
    destroy.multi_target = Some(MultiTargetSpec::fixed(2, 2));
    let definition = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::TargetOnly {
            target: TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::You)),
        },
    )
    .sub_ability(AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::FlipCoins {
            flips: CoinFlipRun::single(),
            win_effect: None,
            lose_effect: Some(Box::new(destroy)),
            flipper: TargetFilter::Controller,
        },
    ));
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let ancestor = scenario.add_creature(P0, "Creature antecedent", 2, 2).id();
    let machine = scenario
        .add_creature(P1, "First dependent artifact creature", 3, 3)
        .as_artifact_creature()
        .id();
    let victim = scenario
        .add_creature(P1, "Second dependent creature", 4, 4)
        .id();
    let artifact = scenario
        .add_artifact_from_oracle(P0, "Artifact distractor", "")
        .id();
    let spell = scenario
        .add_spell_to_hand(P0, "Synthetic dependent multi-target coin", true)
        .with_ability_definition(definition)
        .id();
    let mut runner = scenario.build();
    pin_outcomes(&mut runner, &[false]);
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("required announcement");
    let WaitingFor::TargetSelection { target_slots, .. } = &runner.state().waiting_for else {
        panic!("required ancestor prompt");
    };
    let static_pool = target_slots[2].legal_targets.clone();
    assert_eq!(static_pool, target_slots[1].legal_targets);
    assert!(static_pool.contains(&TargetRef::Object(victim)));
    assert!(!static_pool.contains(&TargetRef::Object(artifact)));
    for target in [ancestor, machine] {
        runner
            .act(GameAction::ChooseTarget {
                target: Some(TargetRef::Object(target)),
            })
            .unwrap();
    }
    let WaitingFor::TargetSelection { selection, .. } = &runner.state().waiting_for else {
        panic!("required second instance prompt");
    };
    let live_pool = &selection.current_legal_targets;
    let expected: Vec<_> = static_pool
        .iter()
        .filter(|target| **target != TargetRef::Object(machine))
        .cloned()
        .collect();
    assert_eq!(
        *live_pool, expected,
        "static and live pools agree except same-declaration distinctness"
    );
    assert!(live_pool.contains(&TargetRef::Object(victim)));
    assert!(live_pool.contains(&TargetRef::Object(ancestor)));
    assert!(
        !live_pool.contains(&TargetRef::Object(artifact)),
        "dependent instance one must not become instance two's antecedent"
    );
    // CR 115.3: same-declaration uniqueness is enforced on submission.
    let before = runner.state().clone();
    assert!(runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(machine)),
        })
        .is_err());
    assert_eq!(*runner.state(), before);
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(victim)),
        })
        .unwrap();
    let mut events = Vec::new();
    finish(&mut runner, &mut events);
    assert_eq!(flips(&events), vec![false]);
    assert_eq!(runner.state().objects[&machine].zone, Zone::Graveyard);
    assert_eq!(
        runner.state().objects[&victim].zone,
        Zone::Graveyard,
        "resolution retains both announced dependent instances"
    );
    assert_eq!(runner.state().objects[&ancestor].zone, Zone::Battlefield);
    assert_eq!(runner.state().objects[&artifact].zone, Zone::Battlefield);
    assert_finished(&runner);
}

#[test]
fn coin_mixed_slot_static_pool_covers_player_and_object_elections() {
    use engine::types::ability::{AbilityDefinition, AbilityKind, TypedFilter};
    for elect_object in [false, true] {
        let branch = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Fight {
                subject: TargetFilter::Any,
                target: TargetFilter::Typed(TypedFilter::creature()),
            },
        )
        .sub_ability(shared_type_destroy());
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let creature = scenario.add_creature(P0, "Object-only election", 2, 2).id();
        let creature_victim = scenario.add_creature(P1, "Creature victim", 4, 4).id();
        let artifact = scenario
            .add_artifact_from_oracle(P0, "Mixed-slot artifact", "")
            .id();
        let artifact_victim = scenario
            .add_artifact_from_oracle(P1, "Artifact victim", "")
            .id();
        let spell = scenario
            .add_spell_to_hand(P0, "Synthetic mixed-slot coin", true)
            .with_ability(Effect::FlipCoins {
                flips: CoinFlipRun::single(),
                win_effect: None,
                lose_effect: Some(Box::new(branch)),
                flipper: TargetFilter::Controller,
            })
            .id();
        let mut runner = scenario.build();
        pin_outcomes(&mut runner, &[false]);
        assert!(
            castable(&runner, spell),
            "mixed candidates must not hide the object-only slot"
        );
        let card_id = runner.state().objects[&spell].card_id;
        runner
            .act(GameAction::CastSpell {
                object_id: spell,
                card_id,
                targets: vec![],
                payment_mode: CastPaymentMode::Auto,
            })
            .unwrap();
        let WaitingFor::TargetSelection { target_slots, .. } = &runner.state().waiting_for else {
            panic!("required mixed-slot announcement");
        };
        let static_pool = target_slots[2].legal_targets.clone();
        assert!(static_pool.contains(&TargetRef::Object(creature_victim)));
        assert!(static_pool.contains(&TargetRef::Object(artifact_victim)));
        let first = if elect_object {
            TargetRef::Object(artifact)
        } else {
            TargetRef::Player(P1)
        };
        runner
            .act(GameAction::ChooseTarget {
                target: Some(first),
            })
            .unwrap();
        runner
            .act(GameAction::ChooseTarget {
                target: Some(TargetRef::Object(creature)),
            })
            .unwrap();
        let WaitingFor::TargetSelection { selection, .. } = &runner.state().waiting_for else {
            panic!("required dependent announcement");
        };
        let live_pool = &selection.current_legal_targets;
        let (victim, distractor) = if elect_object {
            (artifact_victim, creature_victim)
        } else {
            (creature_victim, artifact_victim)
        };
        assert!(live_pool.contains(&TargetRef::Object(victim)));
        assert!(!live_pool.contains(&TargetRef::Object(distractor)));
        assert!(live_pool.iter().all(|target| static_pool.contains(target)));
        runner
            .act(GameAction::ChooseTarget {
                target: Some(TargetRef::Object(victim)),
            })
            .unwrap();
        let mut events = Vec::new();
        finish(&mut runner, &mut events);
        assert_eq!(flips(&events), vec![false]);
        assert_eq!(runner.state().objects[&victim].zone, Zone::Graveyard);
        assert_eq!(runner.state().objects[&distractor].zone, Zone::Battlefield);
        assert_finished(&runner);
    }
}

#[test]
fn coin_optional_same_node_election_keeps_the_ancestor_in_static_scope() {
    use engine::types::ability::{AbilityDefinition, AbilityKind, TypedFilter};
    let mut branch = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::Fight {
            subject: TargetFilter::Typed(TypedFilter::new(
                engine::types::ability::TypeFilter::Artifact,
            )),
            target: shared_type_destroy()
                .effect
                .target_filter()
                .unwrap()
                .clone(),
        },
    );
    branch.optional_targeting = true;
    let definition = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::TargetOnly {
            target: TargetFilter::Typed(TypedFilter::creature()),
        },
    )
    .sub_ability(AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::FlipCoins {
            flips: CoinFlipRun::single(),
            win_effect: None,
            lose_effect: Some(Box::new(branch)),
            flipper: TargetFilter::Controller,
        },
    ));
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let ancestor = scenario
        .add_creature(P0, "Optional fallback ancestor", 2, 2)
        .id();
    let victim = scenario
        .add_creature(P1, "Fallback creature candidate", 3, 3)
        .id();
    let artifact = scenario
        .add_artifact_from_oracle(P0, "Skipped artifact candidate", "")
        .id();
    let spell = scenario
        .add_spell_to_hand(P0, "Synthetic optional same-node coin", true)
        .with_ability_definition(definition)
        .id();
    let mut runner = scenario.build();
    pin_outcomes(&mut runner, &[false]);
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .unwrap();
    let WaitingFor::TargetSelection { target_slots, .. } = &runner.state().waiting_for else {
        panic!("required ancestor announcement");
    };
    let static_pool = target_slots[2].legal_targets.clone();
    assert!(
        static_pool.contains(&TargetRef::Object(victim)),
        "optional slot cannot hide ancestor"
    );
    assert!(
        static_pool.contains(&TargetRef::Object(artifact)),
        "electing the optional object remains possible"
    );
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(ancestor)),
        })
        .unwrap();
    runner
        .act(GameAction::ChooseTarget { target: None })
        .unwrap();
    let WaitingFor::TargetSelection { selection, .. } = &runner.state().waiting_for else {
        panic!("required optional dependent announcement");
    };
    assert!(selection
        .current_legal_targets
        .contains(&TargetRef::Object(victim)));
    assert!(!selection
        .current_legal_targets
        .contains(&TargetRef::Object(artifact)));
    assert!(selection
        .current_legal_targets
        .iter()
        .all(|target| static_pool.contains(target)));
    runner
        .act(GameAction::ChooseTarget { target: None })
        .unwrap();
    let mut events = Vec::new();
    finish(&mut runner, &mut events);
    assert_eq!(
        flips(&events),
        vec![false],
        "reach: optional branch resolves after elections"
    );
    assert_eq!(runner.state().objects[&ancestor].zone, Zone::Battlefield);
    assert_eq!(runner.state().objects[&victim].zone, Zone::Battlefield);
    assert_eq!(runner.state().objects[&artifact].zone, Zone::Battlefield);
    assert_finished(&runner);
}

#[test]
fn coin_binds_prior_intermediate_never_becomes_the_antecedent() {
    use engine::types::ability::{AbilityDefinition, AbilityKind, TypedFilter};
    for all_inside in [false, true] {
        let dependent = shared_type_destroy()
            .effect
            .target_filter()
            .unwrap()
            .clone();
        let tail =
            AbilityDefinition::new(AbilityKind::Spell, Effect::TargetOnly { target: dependent });
        let root = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::TargetOnly {
                target: TargetFilter::Typed(TypedFilter::creature()),
            },
        );
        let definition = if all_inside {
            AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::FlipCoins {
                    flips: CoinFlipRun::single(),
                    win_effect: None,
                    lose_effect: Some(Box::new(
                        root.sub_ability(tail.sub_ability(shared_type_destroy())),
                    )),
                    flipper: TargetFilter::Controller,
                },
            )
        } else {
            root.sub_ability(tail.sub_ability(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::FlipCoins {
                    flips: CoinFlipRun::single(),
                    win_effect: None,
                    lose_effect: Some(Box::new(shared_type_destroy())),
                    flipper: TargetFilter::Controller,
                },
            )))
        };
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let x = scenario
            .add_creature(P0, "Artifact creature antecedent", 2, 2)
            .as_artifact_creature()
            .id();
        let y = scenario
            .add_creature(P0, "Dependent plain creature", 3, 3)
            .id();
        let victim = scenario
            .add_artifact_from_oracle(P1, "Noncreature artifact victim", "")
            .id();
        let spell = scenario
            .add_spell_to_hand(P0, "Synthetic dependent ancestor", true)
            .with_ability_definition(definition)
            .id();
        let mut runner = scenario.build();
        pin_outcomes(&mut runner, &[false]);
        let card_id = runner.state().objects[&spell].card_id;
        let mut events = runner
            .act(GameAction::CastSpell {
                object_id: spell,
                card_id,
                targets: vec![],
                payment_mode: CastPaymentMode::Auto,
            })
            .expect("cast reaches target announcement")
            .events;
        let WaitingFor::TargetSelection { target_slots, .. } = &runner.state().waiting_for else {
            panic!("required root prompt");
        };
        assert_eq!(target_slots.len(), 3);
        let static_pool = target_slots[2].legal_targets.clone();
        assert!(static_pool.contains(&TargetRef::Object(victim)));
        for id in [x, y] {
            events.extend(
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(TargetRef::Object(id)),
                    })
                    .expect("choose ancestor declaration")
                    .events,
            );
        }
        let WaitingFor::TargetSelection { selection, .. } = &runner.state().waiting_for else {
            panic!("required dependent destruction prompt");
        };
        assert!(selection
            .current_legal_targets
            .contains(&TargetRef::Object(victim)));
        assert!(selection
            .current_legal_targets
            .iter()
            .all(|target| static_pool.contains(target)));
        events.extend(
            runner
                .act(GameAction::ChooseTarget {
                    target: Some(TargetRef::Object(victim)),
                })
                .expect("artifact shares a type with X, not Y")
                .events,
        );
        finish(&mut runner, &mut events);
        assert_eq!(flips(&events), vec![false]);
        assert_eq!(runner.state().objects[&victim].zone, Zone::Graveyard);
        assert_eq!(runner.state().objects[&x].zone, Zone::Battlefield);
        assert_eq!(runner.state().objects[&y].zone, Zone::Battlefield);
        assert_finished(&runner);
    }
}

#[test]
fn coin_zero_slot_declaration_does_not_borrow_the_following_player_pool() {
    use engine::types::ability::{AbilityDefinition, AbilityKind, MultiTargetSpec};
    let mut empty = shared_type_destroy();
    empty.multi_target = Some(MultiTargetSpec::fixed(0, 2));
    let branch = empty.sub_ability(AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::TargetOnly {
            target: TargetFilter::Player,
        },
    ));
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand(P0, "Synthetic zero-slot coin", true)
        .with_ability(Effect::FlipCoins {
            flips: CoinFlipRun::single(),
            win_effect: None,
            lose_effect: Some(Box::new(branch)),
            flipper: TargetFilter::Controller,
        })
        .id();
    let mut runner = scenario.build();
    pin_outcomes(&mut runner, &[false]);
    let outcome = runner.cast(spell).target_player(P1).resolve();
    assert_eq!(flips(outcome.events()), vec![false]);
    assert_eq!(outcome.zone_of(spell), Zone::Graveyard);
    assert_finished(&runner);
}

#[test]
fn coin_dependent_source_and_attachment_counts_match_their_slots() {
    use engine::types::ability::{
        AbilityDefinition, AbilityKind, AttachSelection, MultiTargetSpec, TypedFilter,
    };
    for attachment in [false, true] {
        let sources = shared_type_destroy()
            .effect
            .target_filter()
            .unwrap()
            .clone();
        let effect = if attachment {
            Effect::Attach {
                attachment: sources,
                target: TargetFilter::Typed(TypedFilter::creature()),
                selection: AttachSelection::Targeted,
            }
        } else {
            Effect::EachDealsDamageEqualToPower {
                sources,
                recipient: TargetFilter::Typed(TypedFilter::creature()),
                extra_source: None,
            }
        };
        let mut branch = AbilityDefinition::new(AbilityKind::Spell, effect);
        branch.multi_target = Some(MultiTargetSpec::fixed(2, 2));
        let root = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::TargetOnly {
                target: TargetFilter::Typed(TypedFilter::creature()),
            },
        )
        .sub_ability(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::FlipCoins {
                flips: CoinFlipRun::single(),
                win_effect: None,
                lose_effect: Some(Box::new(branch)),
                flipper: TargetFilter::Controller,
            },
        ));
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let ancestor = scenario
            .add_creature(P0, "Artifact creature reference", 1, 1)
            .as_artifact_creature()
            .id();
        let first = if attachment {
            scenario
                .add_artifact_from_oracle(P0, "First Equipment", "")
                .with_subtypes(vec!["Equipment"])
                .id()
        } else {
            scenario.add_creature(P0, "First damage source", 2, 3).id()
        };
        let second = if attachment {
            scenario
                .add_artifact_from_oracle(P0, "Second Equipment", "")
                .with_subtypes(vec!["Equipment"])
                .id()
        } else {
            scenario.add_creature(P0, "Second damage source", 3, 3).id()
        };
        let recipient = scenario.add_creature(P1, "Recipient", 1, 4).id();
        let spell = scenario
            .add_spell_to_hand(P0, "Synthetic counted coin operand", true)
            .with_ability_definition(root)
            .id();
        let mut runner = scenario.build();
        pin_outcomes(&mut runner, &[false]);
        let card_id = runner.state().objects[&spell].card_id;
        let mut events = runner
            .act(GameAction::CastSpell {
                object_id: spell,
                card_id,
                targets: vec![],
                payment_mode: CastPaymentMode::Auto,
            })
            .expect("ancestor-aware counts admit the cast")
            .events;
        let WaitingFor::TargetSelection { target_slots, .. } = &runner.state().waiting_for else {
            panic!("required four-slot announcement");
        };
        assert_eq!(target_slots.len(), 4);
        for id in [ancestor, first, second] {
            events.extend(
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(TargetRef::Object(id)),
                    })
                    .expect("specs and emitted slots stay in lockstep")
                    .events,
            );
        }
        let WaitingFor::TargetSelection { selection, .. } = &runner.state().waiting_for else {
            panic!("required fourth, recipient/host slot");
        };
        assert_eq!(selection.current_slot, 3);
        assert!(selection
            .current_legal_targets
            .contains(&TargetRef::Object(recipient)));
        // This row tests announcement counts, not the separately implemented
        // multi-operand assignment/resolution paths. Cancel while the final
        // declaration remains pending, through the production rollback path.
        runner
            .act(GameAction::CancelCast)
            .expect("cancel the count probe");
        assert!(flips(&events).is_empty(), "announcement must not flip");
        assert_eq!(runner.state().objects[&spell].zone, Zone::Hand);
        assert_finished(&runner);
    }
}

#[test]
fn coin_unbound_scalar_refuses_cast_until_an_object_antecedent_is_declared() {
    use engine::types::ability::{
        AbilityDefinition, AbilityKind, Comparator, ControllerRef, FilterProp, ObjectScope, PtStat,
        PtValueScope, QuantityExpr, QuantityRef, TypedFilter,
    };
    for power in [false, true] {
        for comparator in [Comparator::GE, Comparator::GT] {
            for bound in [false, true] {
                let value = QuantityExpr::Ref {
                    qty: QuantityRef::ObjectManaValue {
                        scope: ObjectScope::Target,
                    },
                };
                let property = if power {
                    FilterProp::PtComparison {
                        stat: PtStat::Power,
                        scope: PtValueScope::Current,
                        comparator,
                        value,
                    }
                } else {
                    FilterProp::Cmc { comparator, value }
                };
                let branch = AbilityDefinition::new(
                    AbilityKind::Spell,
                    Effect::Destroy {
                        target: TargetFilter::Typed(
                            TypedFilter::creature().properties(vec![property]),
                        ),
                        cant_regenerate: false,
                    },
                );
                let coin = AbilityDefinition::new(
                    AbilityKind::Spell,
                    Effect::FlipCoins {
                        flips: CoinFlipRun::single(),
                        win_effect: None,
                        lose_effect: Some(Box::new(branch)),
                        flipper: TargetFilter::Controller,
                    },
                );
                let definition = if bound {
                    AbilityDefinition::new(
                        AbilityKind::Spell,
                        Effect::TargetOnly {
                            target: TargetFilter::Typed(
                                TypedFilter::creature().controller(ControllerRef::You),
                            ),
                        },
                    )
                    .sub_ability(coin)
                } else {
                    coin
                };
                let mut scenario = GameScenario::new();
                scenario.at_phase(Phase::PreCombatMain);
                let antecedent = scenario
                    .add_creature(P0, "Scalar antecedent", 1, 1)
                    .with_mana_cost(ManaCost::generic(1))
                    .id();
                let victim = scenario
                    .add_creature(P1, "Positive mana value victim", 2, 2)
                    .with_mana_cost(ManaCost::generic(2))
                    .id();
                let spell = scenario
                    .add_spell_to_hand(P0, "Synthetic scalar coin", true)
                    .with_ability_definition(definition)
                    .id();
                let mut runner = scenario.build();
                pin_outcomes(&mut runner, &[false]);
                if bound {
                    assert!(
                        castable(&runner, spell),
                        "reach: the bound scalar has a legal assignment"
                    );
                    let outcome = runner
                        .cast(spell)
                        .target_objects(&[antecedent, victim])
                        .resolve();
                    assert_eq!(flips(outcome.events()), vec![false]);
                    outcome.assert_zone(&[victim], Zone::Graveyard);
                } else {
                    // CR 601.2c: no referent means no appropriate dependent target,
                    // never an accepted MV-2 target that self-binds at resolution.
                    assert!(!castable(&runner, spell));
                    let card_id = runner.state().objects[&spell].card_id;
                    assert!(runner
                        .act(GameAction::CastSpell {
                            object_id: spell,
                            card_id,
                            targets: vec![],
                            payment_mode: CastPaymentMode::Auto,
                        })
                        .is_err());
                    assert_eq!(runner.state().objects[&victim].zone, Zone::Battlefield);
                    assert_eq!(runner.state().objects[&spell].zone, Zone::Hand);
                }
                assert_finished(&runner);
            }
        }
    }
}

#[test]
fn coin_dependent_value_read_does_not_happen_when_the_announced_antecedent_becomes_illegal() {
    use engine::types::ability::{
        AbilityDefinition, AbilityKind, Comparator, ControllerRef, FilterProp, ObjectScope,
        QuantityExpr, QuantityRef, TypeFilter, TypedFilter,
    };
    for antecedent_stays_legal in [true, false] {
        let won = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::TargetOnly {
                target: TargetFilter::Typed(TypedFilter::new(TypeFilter::Artifact)),
            },
        );
        let lost = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Destroy {
                target: TargetFilter::Typed(TypedFilter::permanent().properties(vec![
                    FilterProp::Cmc {
                        comparator: Comparator::GE,
                        value: QuantityExpr::Ref {
                            qty: QuantityRef::ObjectManaValue {
                                scope: ObjectScope::Target,
                            },
                        },
                    },
                ])),
                cant_regenerate: false,
            },
        );
        let definition = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::TargetOnly {
                target: TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::You)),
            },
        )
        .sub_ability(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::FlipCoins {
                flips: CoinFlipRun::single(),
                win_effect: Some(Box::new(won)),
                lose_effect: Some(Box::new(lost)),
                flipper: TargetFilter::Controller,
            },
        ));
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let antecedent = scenario
            .add_creature(P0, "Scalar antecedent", 1, 1)
            .with_mana_cost(ManaCost::generic(1))
            .id();
        let artifact = scenario
            .add_artifact_from_oracle(P0, "Independent won artifact", "")
            .id();
        let victim = scenario
            .add_creature(P1, "Dependent mana value victim", 2, 2)
            .with_mana_cost(ManaCost::generic(2))
            .id();
        let spell = scenario
            .add_spell_to_hand(P0, "Synthetic illegal scalar antecedent coin", true)
            .with_ability_definition(definition)
            .id();
        let mut runner = scenario.build();
        pin_outcomes(&mut runner, &[false]);
        let card_id = runner.state().objects[&spell].card_id;
        let mut events = runner
            .act(GameAction::CastSpell {
                object_id: spell,
                card_id,
                targets: vec![],
                payment_mode: CastPaymentMode::Auto,
            })
            .expect("bound scalar and independent artifact admit the cast")
            .events;
        // CR 601.2c: announce every target before the spell commits, including
        // the dependent victim while the root antecedent is still legal.
        for (slot, target) in [antecedent, artifact, victim].into_iter().enumerate() {
            let WaitingFor::TargetSelection {
                target_slots,
                selection,
                ..
            } = &runner.state().waiting_for
            else {
                panic!("required announcement prompt for slot {slot}");
            };
            assert_eq!(target_slots.len(), 3);
            assert_eq!(selection.current_slot, slot);
            assert!(
                selection
                    .current_legal_targets
                    .contains(&TargetRef::Object(target)),
                "reach: target {target:?} is legal for announcement slot {slot}"
            );
            events.extend(
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(TargetRef::Object(target)),
                    })
                    .expect("announce the offered target")
                    .events,
            );
        }
        assert_eq!(runner.state().stack.len(), 1);
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { .. }
        ));
        assert!(flips(&events).is_empty(), "announcement must not flip");
        if !antecedent_stays_legal {
            // CR 608.2b: changing only the controller invalidates "you control".
            runner
                .state_mut()
                .objects
                .get_mut(&antecedent)
                .unwrap()
                .controller = P1;
        }
        finish(&mut runner, &mut events);
        assert_eq!(
            flips(&events),
            vec![false],
            "reach: resolve the lost branch; antecedent_stays_legal={antecedent_stays_legal}"
        );
        // CR 608.2b: the independent legal artifact keeps the spell resolving,
        // but a part requiring information about an illegal target cannot happen.
        assert_eq!(
            runner.state().objects[&victim].zone,
            if antecedent_stays_legal {
                Zone::Graveyard
            } else {
                Zone::Battlefield
            },
            "dependent destroy; antecedent_stays_legal={antecedent_stays_legal}"
        );
        assert_eq!(runner.state().objects[&artifact].zone, Zone::Battlefield);
        assert_eq!(runner.state().objects[&antecedent].zone, Zone::Battlefield);
        assert_finished(&runner);
    }
}

#[test]
fn coin_declining_the_only_possible_antecedent_offers_no_unbound_targets() {
    use engine::types::ability::{
        AbilityDefinition, AbilityKind, Comparator, FilterProp, ObjectScope, QuantityExpr,
        QuantityRef, TypeFilter, TypedFilter,
    };
    for elect_antecedent in [false, true] {
        let mut destroy = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Destroy {
                target: TargetFilter::Typed(TypedFilter::creature().properties(vec![
                    FilterProp::Cmc {
                        comparator: Comparator::GT,
                        value: QuantityExpr::Ref {
                            qty: QuantityRef::ObjectManaValue {
                                scope: ObjectScope::Target,
                            },
                        },
                    },
                ])),
                cant_regenerate: false,
            },
        );
        destroy.optional_targeting = true;
        let mut definition = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::TargetOnly {
                target: TargetFilter::Typed(TypedFilter::new(TypeFilter::Artifact)),
            },
        )
        .sub_ability(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::FlipCoins {
                flips: CoinFlipRun::single(),
                win_effect: None,
                lose_effect: Some(Box::new(destroy)),
                flipper: TargetFilter::Controller,
            },
        ));
        definition.optional_targeting = true;
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let antecedent = scenario
            .add_artifact_from_oracle(P0, "High mana value antecedent", "")
            .with_mana_cost(ManaCost::generic(5))
            .id();
        let victim = scenario
            .add_creature(P1, "Above antecedent threshold", 3, 3)
            .with_mana_cost(ManaCost::generic(6))
            .id();
        let excluded = scenario
            .add_creature(P1, "Below antecedent threshold", 2, 2)
            .with_mana_cost(ManaCost::generic(2))
            .id();
        let spell = scenario
            .add_spell_to_hand(P0, "Synthetic optional antecedent coin", true)
            .with_ability_definition(definition)
            .id();
        let mut runner = scenario.build();
        pin_outcomes(&mut runner, &[false]);
        let card_id = runner.state().objects[&spell].card_id;
        let mut events = runner
            .act(GameAction::CastSpell {
                object_id: spell,
                card_id,
                targets: vec![],
                payment_mode: CastPaymentMode::Auto,
            })
            .unwrap()
            .events;
        let WaitingFor::TargetSelection { target_slots, .. } = &runner.state().waiting_for else {
            panic!("required optional antecedent prompt");
        };
        let static_pool = target_slots[1].legal_targets.clone();
        assert!(static_pool.contains(&TargetRef::Object(victim)));
        assert!(!static_pool.contains(&TargetRef::Object(excluded)));
        events.extend(
            runner
                .act(GameAction::ChooseTarget {
                    target: elect_antecedent.then_some(TargetRef::Object(antecedent)),
                })
                .unwrap()
                .events,
        );
        if elect_antecedent {
            let WaitingFor::TargetSelection { selection, .. } = &runner.state().waiting_for else {
                panic!("required bound dependent prompt");
            };
            assert!(selection
                .current_legal_targets
                .iter()
                .all(|target| static_pool.contains(target)));
            assert_eq!(
                selection.current_legal_targets,
                vec![TargetRef::Object(victim)]
            );
            events.extend(
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(TargetRef::Object(victim)),
                    })
                    .unwrap()
                    .events,
            );
        } else {
            // An optional slot with no live candidates is skipped automatically.
            // The old unbound fallback instead offered the MV-2 creature here.
            assert!(matches!(
                runner.state().waiting_for,
                WaitingFor::Priority { .. }
            ));
            assert_eq!(runner.state().objects[&spell].zone, Zone::Stack);
        }
        finish(&mut runner, &mut events);
        assert_eq!(
            flips(&events),
            vec![false],
            "reach: both optional-election rows resolve the coin"
        );
        assert_eq!(
            runner.state().objects[&victim].zone,
            if elect_antecedent {
                Zone::Graveyard
            } else {
                Zone::Battlefield
            }
        );
        assert_eq!(runner.state().objects[&excluded].zone, Zone::Battlefield);
        assert_finished(&runner);
    }
}

#[test]
fn modal_coin_static_scope_matches_the_combined_mode_chain() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let artifact = scenario
        .add_artifact_from_oracle(P0, "First mode artifact", "")
        .id();
    let antecedent = scenario
        .add_creature(P0, "Second mode antecedent", 2, 2)
        .id();
    let victim = scenario
        .add_creature(P1, "Plain creature victim", 3, 3)
        .id();
    let spell = scenario.add_spell_to_hand_from_oracle(P0, "Synthetic modal coin antecedent", true,
        "Choose two —\n• Tap target noncreature artifact.\n• Target creature gets +1/+1 until end of turn. Flip a coin. If you lose the flip, destroy target permanent that shares a card type with that creature.")
        .with_mana_cost(ManaCost::zero()).id();
    let mut runner = scenario.build();
    pin_outcomes(&mut runner, &[false]);
    let card_id = runner.state().objects[&spell].card_id;
    let mut events = runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .unwrap()
        .events;
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ModeChoice { .. }
    ));
    events.extend(
        runner
            .act(GameAction::SelectModes {
                indices: vec![0, 1],
            })
            .unwrap()
            .events,
    );
    let WaitingFor::TargetSelection { target_slots, .. } = &runner.state().waiting_for else {
        panic!("required combined mode target announcement");
    };
    let static_pool = target_slots.last().unwrap().legal_targets.clone();
    assert!(
        static_pool.contains(&TargetRef::Object(victim)),
        "mode-one artifact must not shadow mode-two creature"
    );
    for target in [artifact, antecedent] {
        events.extend(
            runner
                .act(GameAction::ChooseTarget {
                    target: Some(TargetRef::Object(target)),
                })
                .unwrap()
                .events,
        );
    }
    let WaitingFor::TargetSelection { selection, .. } = &runner.state().waiting_for else {
        panic!("required coin consumer prompt");
    };
    assert!(selection
        .current_legal_targets
        .contains(&TargetRef::Object(victim)));
    assert!(selection
        .current_legal_targets
        .iter()
        .all(|target| static_pool.contains(target)));
    events.extend(
        runner
            .act(GameAction::ChooseTarget {
                target: Some(TargetRef::Object(victim)),
            })
            .unwrap()
            .events,
    );
    finish(&mut runner, &mut events);
    assert_eq!(flips(&events), vec![false]);
    assert!(runner.state().objects[&artifact].tapped);
    assert_eq!(runner.state().objects[&victim].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&antecedent].zone, Zone::Battlefield);
    assert_finished(&runner);
}
