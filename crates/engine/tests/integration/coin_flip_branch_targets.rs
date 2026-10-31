//! Coin-branch announcement and execution regressions.
//! Crooked Scales fixtures use the printed text; the five tests whose flow the closing
//! "unless you pay {3} and repeat this process" changes use `CROOKED_SCALES_WITHOUT_PAYMENT`.

use engine::ai_support::legal_actions;
use engine::game::casting::can_activate_ability_now;
use engine::game::game_object::ObjectIncarnationRefExt;
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
        CROOKED_SCALES,
        "Crooked Scales",
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
        assert!(branch.sub_ability.is_none());
    }
    // Printed text: the losing outcome carries its unless-pay {3} (never reached on a win).
    assert!(lose.unless_pay.is_some());

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_artifact_from_oracle(P0, "Crooked Scales", CROOKED_SCALES)
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

// Byte-identical to the "Crooked Scales" oracle_text in the exported MTGJSON card data.
const CROOKED_SCALES: &str = "{4}, {T}: Flip a coin. If you win the flip, destroy target creature an opponent controls. If you lose the flip, destroy target creature you control unless you pay {3} and repeat this process.";

// Crooked Scales without its closing "unless you pay {3} and repeat this process": for the tests
// whose flow the printed clause changes (the losing outcome would first ask to pay {3}; the engine
// does not complete that prompt or the repeat, a tracked follow-up).
const CROOKED_SCALES_WITHOUT_PAYMENT: &str = "{4}, {T}: Flip a coin. If you win the flip, destroy target creature an opponent controls. If you lose the flip, destroy target creature you control.";

// Byte-identical to the "Goblin Bangchuckers" oracle_text in the exported MTGJSON card data.
const GOBLIN_BANGCHUCKERS: &str = "{T}: Flip a coin. If you win the flip, this creature deals 2 damage to any target. If you lose the flip, this creature deals 2 damage to itself.";

/// Real card: Crooked Scales with its printed Oracle text. CR 602.2b + CR 601.2c: both outcome
/// targets are announced on activation, before the flip; CR 705.2 + CR 608.2c: a won flip
/// destroys only the announced opponent's creature, and the lose outcome's unless-pay and
/// "repeat this process" (which the engine parses as Unimplemented) never run.
#[test]
fn crooked_scales_printed_text_announces_both_targets_and_a_won_flip_destroys_only_the_opponents_creature(
) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_artifact_from_oracle(P0, "Crooked Scales", CROOKED_SCALES)
        .id();
    let own = scenario.add_creature(P0, "Chosen Own", 2, 2).id();
    let own_distractor = scenario.add_creature(P0, "Own Distractor", 3, 3).id();
    let opposing = scenario.add_creature(P1, "Chosen Opponent", 4, 4).id();
    let opposing_distractor = scenario.add_creature(P1, "Opponent Distractor", 5, 5).id();
    mana(&mut scenario, 4);
    let mut runner = scenario.build();
    pin_outcomes(&mut runner, &[true]);
    let mut events = runner
        .act(GameAction::ActivateAbility {
            source_id: source,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("activation reaches announcement")
        .events;
    let WaitingFor::TargetSelection { target_slots, .. } = &runner.state().waiting_for else {
        panic!(
            "both coin-outcome targets must be announced on activation, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(
        target_slots.len(),
        2,
        "one declared occurrence per outcome: opponent's creature, then your creature"
    );
    for (slot, expected, excluded) in [
        (0, [opposing, opposing_distractor], [own, own_distractor]),
        (1, [own, own_distractor], [opposing, opposing_distractor]),
    ] {
        for creature in expected {
            assert!(target_slots[slot]
                .legal_targets
                .contains(&TargetRef::Object(creature)));
        }
        for creature in excluded {
            assert!(!target_slots[slot]
                .legal_targets
                .contains(&TargetRef::Object(creature)));
        }
    }
    assert!(flips(&events).is_empty(), "announced before any flip");
    for target in [opposing, own] {
        events.extend(
            runner
                .act(GameAction::ChooseTarget {
                    target: Some(TargetRef::Object(target)),
                })
                .expect("announce outcome target")
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
    finish(&mut runner, &mut events);
    assert_eq!(flips(&events), vec![true], "reach: forced win resolves");
    assert_eq!(
        runner.state().objects[&opposing].zone,
        Zone::Graveyard,
        "the won outcome destroys the announced opponent's creature"
    );
    for untouched in [own, own_distractor, opposing_distractor, source] {
        assert_eq!(runner.state().objects[&untouched].zone, Zone::Battlefield);
    }
    assert_eq!(coin_terminals(&events, source), 1);
    assert_finished(&runner);
}

#[test]
fn coin_branch_stack_persistence_is_strict_and_current_bindings_execute() {
    use engine::types::resolution::{ResolutionStateWire, RESOLUTION_STATE_WIRE_VERSION};
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
    for version in 1..=RESOLUTION_STATE_WIRE_VERSION {
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
        // CR 601.2c + CR 608.2b: every pre-current wire version (<= 10) cannot
        // prove the outcome bindings announced at cast, so a carrier-less coin
        // ability is refused as incompatible; the current version is malformed.
        let expected = if version < RESOLUTION_STATE_WIRE_VERSION {
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

// Real cards: none of the 86 printed coin-flip cards in the exported card data exhibits this shape:
// A spell whose single coin has win and lose pump branches each declaring 'target nonred creature';
// a color-word text change on the spell before resolution changes both branch filters and can make
// both illegal. Nearest printed card: Orcish Captain ("{1}: Flip a coin. If you win the flip,
// target Orc creature gets +2/+0 until end of turn. If you lose the flip, it gets -0/-2 until end
// of turn."); Chaotic Strike ("Cast this spell only during combat after blockers are declared. /
// Flip a coin. If you win the flip, target creature gets +1/+1 until end of turn. / Draw a card.")
// -- Orcish Captain has targeted win and lose pump branches (one shared target, Orc filter, no
// color word); Chaotic Strike has only a targeted win pump.
#[test]
fn text_substitution_reaches_both_live_coin_branch_filters() {
    use engine::types::mana::ManaColor;
    // Synthetic supported pump grammar; neither real pump card has a color restriction.
    const COIN: &str = "Flip a coin. If you win the flip, target nonred creature gets +1/+1 until end of turn. If you lose the flip, target nonred creature gets +2/+2 until end of turn.";
    const CHANGE: &str = "Change the text of target spell or permanent by replacing all instances of one color word with another. (For example, you may change \"target black spell\" to \"target blue spell.\" This effect lasts indefinitely.)";
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
                    .add_spell_to_hand_from_oracle(P1, "Sleight of Mind", true, CHANGE)
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

// Real cards: none of the 86 printed coin-flip cards in the exported card data exhibits this shape:
// A spell installs a one-shot draw or planeswalk replacement whose substitute is a coin with
// life-gain win/lose branches (or a non-coin control); after save/restore the replaced event runs
// exactly the selected branch. Nearest printed card: Saga of Krark Losing His Thumb ("Flip Ahead
// (If you would put a lore counter on this saga, instead flip a coin. If you lose the flip, don't
// put a counter on and repeat the current chapter.) / I — Add {U}{R}. / II — Create a token that's
// a copy of Krark, the Thumbless and a token that's a copy of Krark's Thumb. (Keep in mind they're
// both legendary!) / III — Choose target opponent. Flip a coin with them. The loser of the flip
// loses a quarter of their life, rounded up."); Desperate Gambit ("Choose a source you control and
// flip a coin. If you win the flip, the next time that source would deal damage this turn, it deals
// double that damage instead. If you lose the flip, the next time it would deal damage this turn,
// prevent that damage.") -- The Saga's Flip Ahead replaces a lore counter with a coin (static, not
// installed); Desperate Gambit's coin installs replacements instead of being one.
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
            // CR 615.5 + CR 614.6: the shield's rider lives in the state's
            // continuation arena behind a handle; read it through the single
            // read authority.
            let runtime = runner
                .state()
                .replacement_rider(&runner.state().pending_damage_replacements[0])
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

// Real cards: none of the 86 printed coin-flip cards in the exported card data exhibits this shape:
// A spell whose coin win branch makes an end-step delayed counter on 'that creature' while the lose
// branch targets a creature; the anaphor binds only an earlier ancestor target, never the lose
// target. Nearest printed card: Goblin Kites ("{R}: Target creature you control with toughness 2 or
// less gains flying until end of turn. Flip a coin at the beginning of the next end step. If you
// lose the flip, sacrifice that creature."); Frenetic Sliver ("All Slivers have "{0}: If this
// permanent is on the battlefield, flip a coin. If you win the flip, exile this permanent and
// return it to the battlefield under its owner's control at the beginning of the next end step. If
// you lose the flip, sacrifice it."") -- Goblin Kites delays a coin on its earlier target, with no
// sibling branches; Frenetic Sliver's won branch delays returning itself at end step.
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

// Synthetic building-block fixture: a typed owner scope reads B, not the
// controller of the source or the owner of its distinct attached host.
// Real cards: none of the 86 printed coin-flip cards in the exported card data exhibits this shape:
// An attached Equipment ability targets A, then a coin whose win branch targets B and flips again,
// countering creatures controlled by B's owner, immediately or at end step, even after B leaves.
// Nearest printed card: Goblin Lyre ("Sacrifice this artifact: Flip a coin. If you win the flip,
// this artifact deals damage to target opponent or planeswalker equal to the number of creatures
// you control. If you lose the flip, this artifact deals damage to you equal to the number of
// creatures that opponent or that planeswalker's controller controls."); Goblin Kites ("{R}: Target
// creature you control with toughness 2 or less gains flying until end of turn. Flip a coin at the
// beginning of the next end step. If you lose the flip, sacrifice that creature.") -- Goblin Lyre's
// lose branch counts creatures controlled by its target's controller (no nested coin); Goblin Kites
// delays a coin referring to an earlier target.
fn typed_owner_coin_binding_scenario(remove_b: bool, delayed_phase: Option<Phase>) {
    use engine::game::game_object::AttachTarget;
    use engine::types::ability::{
        AbilityDefinition, AbilityKind, ControllerRef, DelayedTriggerCondition, QuantityExpr,
        TypedFilter,
    };
    use engine::types::counter::CounterType;
    use engine::types::player::PlayerId;

    let mass = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::PutCounterAll {
            counter_type: CounterType::Plus1Plus1,
            count: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::Typed(
                TypedFilter::creature().controller(ControllerRef::ParentTargetOwner),
            ),
        },
    );
    let payload = match delayed_phase {
        Some(phase) => AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::CreateDelayedTrigger {
                condition: DelayedTriggerCondition::AtNextPhase { phase },
                effect: Box::new(mass),
                uses_tracked_set: false,
            },
        ),
        None => AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 1 },
                player: TargetFilter::Controller,
            },
        )
        .sub_ability(mass),
    };
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
    let definition = AbilityDefinition::new(
        AbilityKind::Activated,
        Effect::TargetOnly {
            target: creature.clone(),
        },
    )
    .sub_ability(
        flip(
            AbilityDefinition::new(AbilityKind::Spell, Effect::TargetOnly { target: creature })
                .sub_ability(flip(payload)),
        )
        .sub_ability(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 2 },
                player: TargetFilter::Controller,
            },
        )),
    );
    let mut scenario = GameScenario::new_n_player(3, 7);
    scenario.at_phase(Phase::PostCombatMain);
    let a = scenario.add_creature(P0, "Outer A", 2, 2).id();
    let b = scenario.add_creature(P1, "Owner referent B", 2, 2).id();
    let ally = scenario.add_creature(P1, "B owner population", 2, 2).id();
    let h = scenario
        .add_creature(PlayerId(2), "Hostile attached host H", 2, 2)
        .id();
    let source = scenario
        .add_artifact_from_oracle(P0, "Synthetic typed-owner Equipment", "")
        .with_subtypes(vec!["Equipment"])
        .with_ability_definition(definition)
        .id();
    let response = scenario
        .add_spell_to_hand_from_oracle(P0, "Final Death", true, "Exile target creature.")
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&source)
        .unwrap()
        .attached_to = Some(AttachTarget::Object(h));
    runner
        .state_mut()
        .objects
        .get_mut(&h)
        .unwrap()
        .attachments
        .push(source);
    runner.state_mut().layers_dirty.mark_full();
    pin_outcomes(&mut runner, &[true, true]);
    let life = runner.life(P0);
    runner
        .act(GameAction::ActivateAbility {
            source_id: source,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        })
        .unwrap();
    for id in [a, b] {
        runner
            .act(GameAction::ChooseTarget {
                target: Some(TargetRef::Object(id)),
            })
            .unwrap();
    }
    assert_eq!(runner.state().stack.len(), 1, "reach: A and B announced");
    if remove_b {
        runner.cast(response).target_object(b).commit();
    }
    let mut events = Vec::new();
    finish(&mut runner, &mut events);
    assert_eq!(
        flips(&events),
        vec![true, true],
        "reach: both selected wins"
    );
    assert_eq!(
        runner.life(P0),
        life + if delayed_phase.is_some() { 2 } else { 3 }
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
    assert_eq!(
        runner.state().objects[&source].attached_to,
        Some(AttachTarget::Object(h)),
        "reach: source remains attached to the hostile third-seat host"
    );
    assert_finished(&runner);
    if let Some(phase) = delayed_phase {
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
            "reach: installation"
        );
        let expected = if remove_b {
            vec![]
        } else {
            vec![TargetRef::Object(b)]
        };
        assert_eq!(
            runner.state().delayed_triggers[0].ability.targets,
            expected,
            "typed-owner creation snapshot; illegal B={remove_b}"
        );
        for _ in 0..18 {
            if !runner.state().stack.is_empty() {
                break;
            }
            runner.act(GameAction::PassPriority).unwrap();
        }
        assert_eq!(runner.state().phase, phase, "reach: firing phase");
        let placed = runner
            .state()
            .stack
            .back()
            .expect("reach: delayed trigger placed")
            .ability()
            .unwrap();
        assert_eq!(
            placed.trigger_source.as_ref().unwrap().attached_to,
            Some(AttachTarget::Object(h))
        );
        assert_eq!(placed.targets, expected, "typed-owner stack binding");
        finish(&mut runner, &mut events);
        assert!(
            runner.state().delayed_triggers.is_empty(),
            "one-shot consumed"
        );
    }
    assert!(
        events.iter().any(|event| matches!(
            event,
            GameEvent::EffectResolved {
                kind: EffectKind::PutCounterAll,
                ..
            }
        )),
        "reach: mass payoff executed"
    );
    for id in [a, b, ally, h] {
        assert_eq!(
            runner.state().objects[&id]
                .counters
                .get(&CounterType::Plus1Plus1)
                .copied()
                .unwrap_or(0),
            u32::from(!remove_b && [b, ally].contains(&id)),
            "only B owner's population receives counters; object={id:?}, illegal B={remove_b}"
        );
    }
    assert_finished(&runner);
}

// Real cards: none of the 86 printed coin-flip cards in the exported card data exhibits this shape:
// The nested-coin typed-owner fixture with an immediate payload: counters go only to creatures
// controlled by announced B's owner, and to nobody once B is exiled in response. Nearest printed
// card: Goblin Lyre ("Sacrifice this artifact: Flip a coin. If you win the flip, this artifact
// deals damage to target opponent or planeswalker equal to the number of creatures you control. If
// you lose the flip, this artifact deals damage to you equal to the number of creatures that
// opponent or that planeswalker's controller controls.") -- Goblin Lyre scopes a count by its
// declared target's controller inside a coin branch; it lacks nesting and a mass counter payoff.
#[test]
fn nested_coin_typed_owner_scope_preserves_its_announced_binding() {
    for remove_b in [true, false] {
        typed_owner_coin_binding_scenario(remove_b, None);
    }
}

// Real cards: none of the 86 printed coin-flip cards in the exported card data exhibits this shape:
// The nested-coin typed-owner fixture with an end-step delayed payload: the delayed trigger
// snapshots announced B (or nothing if exiled) and counters only B's owner's creatures when it
// fires. Nearest printed card: Goblin Kites ("{R}: Target creature you control with toughness 2 or
// less gains flying until end of turn. Flip a coin at the beginning of the next end step. If you
// lose the flip, sacrifice that creature."); Goblin Lyre ("Sacrifice this artifact: Flip a coin. If
// you win the flip, this artifact deals damage to target opponent or planeswalker equal to the
// number of creatures you control. If you lose the flip, this artifact deals damage to you equal to
// the number of creatures that opponent or that planeswalker's controller controls.") -- Goblin
// Kites delays work that refers to an earlier declared target; Goblin Lyre scopes by a target's
// controller; neither nests coins.
#[test]
fn delayed_coin_typed_owner_scope_snapshots_its_announced_binding() {
    for remove_b in [true, false] {
        typed_owner_coin_binding_scenario(remove_b, Some(Phase::End));
    }
}

// Real cards: none of the 86 printed coin-flip cards in the exported card data exhibits this shape:
// A spell targets A, then a coin whose win branch targets B and creates 'when that creature dies,
// gain 3 life'; an exiled B installs no trigger, and an unrelated death never pays. Nearest printed
// card: Creepy Doll ("Indestructible / Whenever this creature deals combat damage to a creature,
// flip a coin. If you win the flip, destroy that creature."); Frenetic Sliver ("All Slivers have
// "{0}: If this permanent is on the battlefield, flip a coin. If you win the flip, exile this
// permanent and return it to the battlefield under its owner's control at the beginning of the next
// end step. If you lose the flip, sacrifice it."") -- Creepy Doll's won branch destroys 'that
// creature' immediately, not via a dies trigger; Frenetic Sliver's won branch makes a delayed
// trigger about itself.
#[test]
fn delayed_death_condition_never_widens_an_unavailable_announcement() {
    use engine::types::ability::{
        AbilityDefinition, AbilityKind, DelayedTriggerCondition, QuantityExpr, TypedFilter,
    };

    for remove_b in [true, false] {
        let creature = TargetFilter::Typed(TypedFilter::creature());
        let delayed = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::CreateDelayedTrigger {
                condition: DelayedTriggerCondition::WhenDies {
                    filter: TargetFilter::ParentTarget,
                },
                effect: Box::new(AbilityDefinition::new(
                    AbilityKind::Spell,
                    Effect::GainLife {
                        amount: QuantityExpr::Fixed { value: 3 },
                        player: TargetFilter::Controller,
                    },
                )),
                uses_tracked_set: false,
            },
        );
        let definition = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::TargetOnly {
                target: creature.clone(),
            },
        )
        .sub_ability(
            AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::FlipCoins {
                    flips: CoinFlipRun::single(),
                    win_effect: Some(Box::new(
                        AbilityDefinition::new(
                            AbilityKind::Spell,
                            Effect::TargetOnly { target: creature },
                        )
                        .sub_ability(delayed),
                    )),
                    lose_effect: None,
                    flipper: TargetFilter::Controller,
                },
            )
            .sub_ability(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::GainLife {
                    amount: QuantityExpr::Fixed { value: 2 },
                    player: TargetFilter::Controller,
                },
            )),
        );
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PostCombatMain);
        let a = scenario.add_creature(P0, "Outer A", 2, 2).id();
        let b = scenario.add_creature(P1, "Announced B", 2, 2).id();
        let c = scenario.add_creature(P1, "Unrelated C", 2, 2).id();
        let spell = scenario
            .add_spell_to_hand(P0, "Synthetic delayed death condition", true)
            .with_ability_definition(definition)
            .id();
        let exile = scenario
            .add_spell_to_hand_from_oracle(P0, "Final Death", true, "Exile target creature.")
            .with_mana_cost(ManaCost::zero())
            .id();
        let kill_c = scenario
            .add_spell_to_hand_from_oracle(P0, "Murder", true, "Destroy target creature.")
            .with_mana_cost(ManaCost::zero())
            .id();
        let kill_b = scenario
            .add_spell_to_hand_from_oracle(
                P0,
                "Bilbo's Deadly Slice",
                true,
                "Destroy target creature.",
            )
            .with_mana_cost(ManaCost::zero())
            .id();
        let mut runner = scenario.build();
        pin_outcomes(&mut runner, &[true]);
        let life = runner.life(P0);
        runner.cast(spell).target_objects(&[a, b]).commit();
        assert_eq!(
            runner.state().stack.len(),
            1,
            "reach: public A/B announcement"
        );
        if remove_b {
            runner.cast(exile).target_object(b).commit();
        }
        let mut events = Vec::new();
        finish(&mut runner, &mut events);
        assert_eq!(flips(&events), vec![true]);
        assert_eq!(runner.life(P0), life + 2, "reach: independent tail");
        assert!(
            events.iter().any(|event| matches!(
                event,
                GameEvent::EffectResolved {
                    kind: EffectKind::CreateDelayedTrigger,
                    ..
                }
            )),
            "reach: delayed creation instruction"
        );
        assert_eq!(
            runner.state().delayed_triggers.len(),
            usize::from(!remove_b),
            "an unavailable announcement installs no wildcard death watch"
        );
        assert_finished(&runner);
        runner.cast(kill_c).target_object(c).commit();
        finish(&mut runner, &mut events);
        assert_eq!(
            runner.state().objects[&c].zone,
            Zone::Graveyard,
            "reach: unrelated creature actually died"
        );
        assert_eq!(runner.life(P0), life + 2, "unrelated death never pays");
        assert_eq!(
            runner.state().delayed_triggers.len(),
            usize::from(!remove_b)
        );
        if !remove_b {
            runner.cast(kill_b).target_object(b).commit();
            finish(&mut runner, &mut events);
            assert_eq!(runner.state().objects[&b].zone, Zone::Graveyard);
            assert_eq!(runner.life(P0), life + 5, "B death pays exactly once");
            assert!(runner.state().delayed_triggers.is_empty());
        } else {
            assert_eq!(runner.state().objects[&b].zone, Zone::Exile);
        }
        assert_eq!(runner.state().objects[&a].zone, Zone::Battlefield);
        assert!(runner.state().objects[&a].counters.is_empty());
        assert_finished(&runner);
    }
}

// Synthetic building-block fixtures: independent B must remain the antecedent
// after validation, coin selection, detachment, and trigger placement.
#[derive(Clone, Copy)]
enum EmptyCoinRider {
    Counter,
    CounterAfterGainLife,
    OptionalCounter,
    DrawController,
    DrawOwner,
    GainLifeController,
    GainLifeOwner,
    OptionalDrawController,
    OptionalDrawOwner,
    NestedCoinCounter,
    Saddle,
    Sacrifice,
    BecomeBlocked,
    Replacement,
}

// Real cards: none of the 86 printed coin-flip cards in the exported card data exhibits this shape:
// Spell or attached Equipment targets A, then a coin (optionally nested) whose win branch targets B
// and delays a rider on B or B's player; exiling B leaves an empty binding with no substitute.
// Nearest printed card: Goblin Kites ("{R}: Target creature you control with toughness 2 or less
// gains flying until end of turn. Flip a coin at the beginning of the next end step. If you lose
// the flip, sacrifice that creature."); Frenetic Sliver ("All Slivers have "{0}: If this permanent
// is on the battlefield, flip a coin. If you win the flip, exile this permanent and return it to
// the battlefield under its owner's control at the beginning of the next end step. If you lose the
// flip, sacrifice it."") -- Goblin Kites delays a coin that sacrifices its earlier target, without
// branch targets; Frenetic Sliver's won branch makes an end-step delayed return of itself only.
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

    let player_rider = matches!(
        rider,
        EmptyCoinRider::DrawController
            | EmptyCoinRider::DrawOwner
            | EmptyCoinRider::GainLifeController
            | EmptyCoinRider::GainLifeOwner
            | EmptyCoinRider::OptionalDrawController
            | EmptyCoinRider::OptionalDrawOwner
    );
    let parent_player = match rider {
        EmptyCoinRider::DrawOwner
        | EmptyCoinRider::GainLifeOwner
        | EmptyCoinRider::OptionalDrawOwner => TargetFilter::ParentTargetOwner,
        _ => TargetFilter::ParentTargetController,
    };
    let blocked = matches!(rider, EmptyCoinRider::BecomeBlocked);
    let phase = if blocked {
        Phase::EndCombat
    } else {
        Phase::End
    };
    let payoff = match rider {
        EmptyCoinRider::DrawController
        | EmptyCoinRider::DrawOwner
        | EmptyCoinRider::OptionalDrawController
        | EmptyCoinRider::OptionalDrawOwner => Effect::Draw {
            count: QuantityExpr::Fixed { value: 1 },
            target: parent_player,
        },
        EmptyCoinRider::GainLifeController | EmptyCoinRider::GainLifeOwner => Effect::GainLife {
            amount: QuantityExpr::Fixed { value: 3 },
            player: parent_player,
        },
        EmptyCoinRider::Counter
        | EmptyCoinRider::CounterAfterGainLife
        | EmptyCoinRider::OptionalCounter
        | EmptyCoinRider::NestedCoinCounter => Effect::PutCounter {
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
        EmptyCoinRider::Saddle => Effect::BecomeSaddled {
            target: TargetFilter::ParentTarget,
        },
        EmptyCoinRider::Sacrifice => Effect::Sacrifice {
            target: TargetFilter::ParentTarget,
            count: QuantityExpr::Fixed { value: 1 },
            min_count: 0,
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
    let payoff = if matches!(rider, EmptyCoinRider::NestedCoinCounter) {
        AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::FlipCoins {
                flips: CoinFlipRun::single(),
                win_effect: Some(Box::new(payoff)),
                lose_effect: None,
                flipper: TargetFilter::Controller,
            },
        )
    } else {
        payoff
    };
    let payoff = if matches!(
        rider,
        EmptyCoinRider::OptionalCounter
            | EmptyCoinRider::OptionalDrawController
            | EmptyCoinRider::OptionalDrawOwner
    ) {
        payoff.optional()
    } else {
        payoff
    };
    let delayed = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::CreateDelayedTrigger {
            condition: DelayedTriggerCondition::AtNextPhase { phase },
            effect: Box::new(payoff),
            uses_tracked_set: false,
        },
    );
    let delayed = if matches!(
        rider,
        EmptyCoinRider::CounterAfterGainLife | EmptyCoinRider::NestedCoinCounter
    ) {
        AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 1 },
                player: TargetFilter::Controller,
            },
        )
        .sub_ability(delayed)
    } else {
        delayed
    };
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
    let mut scenario = if player_rider {
        GameScenario::new_n_player(3, 7)
    } else {
        GameScenario::new()
    };
    let p2 = engine::types::player::PlayerId(2);
    if player_rider {
        for player in [P0, P1, p2] {
            scenario.with_library_top(player, &["Draw payoff sentinel", "Library reserve"]);
        }
    }
    scenario.at_phase(if blocked {
        Phase::CombatDamage
    } else {
        Phase::PostCombatMain
    });
    let a = scenario
        .add_creature(P0, "Earlier outer declaration", 2, 20)
        .id();
    let b = scenario
        .add_creature(
            if player_rider { P1 } else { P0 },
            "Selected branch declaration",
            2,
            20,
        )
        .id();
    let h = scenario
        .add_creature(
            if player_rider { p2 } else { P0 },
            "Hostile attached host",
            2,
            20,
        )
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
        .add_spell_to_hand_from_oracle(P0, "Final Death", true, "Exile target creature.")
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
    let outcomes = if keep_restore {
        if nested {
            vec![true, false, true, false]
        } else {
            vec![true, false]
        }
    } else if nested {
        vec![true, true]
    } else {
        vec![true]
    };
    let mut outcomes = outcomes;
    if matches!(rider, EmptyCoinRider::NestedCoinCounter) {
        outcomes.extend(if keep_restore {
            vec![true, false]
        } else {
            vec![true]
        });
    }
    pin_outcomes(&mut runner, &outcomes);
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
    assert_eq!(
        runner.life(P0),
        life + if matches!(
            rider,
            EmptyCoinRider::CounterAfterGainLife | EmptyCoinRider::NestedCoinCounter
        ) {
            3
        } else {
            2
        },
        "reach: targetless head and independent tail execute"
    );
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
    let snapshot = &runner.state().delayed_triggers[0].ability;
    assert_eq!(
        snapshot.targets,
        if remove_b {
            vec![]
        } else {
            vec![TargetRef::Object(b)]
        },
        "delayed snapshot identity; illegal B={remove_b}"
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
    let player_before: Vec<_> = runner
        .state()
        .players
        .iter()
        .map(|player| (player.id, player.life, player.hand.clone()))
        .collect();
    if matches!(
        rider,
        EmptyCoinRider::OptionalCounter
            | EmptyCoinRider::OptionalDrawController
            | EmptyCoinRider::OptionalDrawOwner
    ) {
        for _ in 0..12 {
            if !matches!(runner.state().waiting_for, WaitingFor::Priority { .. }) {
                break;
            }
            payoff_events.extend(runner.act(GameAction::PassPriority).unwrap().events);
        }
        assert!(
            matches!(
                runner.state().waiting_for,
                WaitingFor::OptionalEffectChoice { player: P0, .. }
            ),
            "reach: optional delayed counter prompt"
        );
        if player_rider {
            let pending = runner.state().active_optional_effect_frame().unwrap();
            assert_eq!(
                pending.ability.targets, expected,
                "optional capture retains the original recipient binding"
            );
        }
        payoff_events.extend(
            runner
                .act(GameAction::DecideOptionalEffect { accept: true })
                .unwrap()
                .events,
        );
    }
    if matches!(rider, EmptyCoinRider::NestedCoinCounter) && keep_restore {
        pass_to_coin_keep(&mut runner, &mut payoff_events);
        payoff_events.extend(
            runner
                .act(GameAction::SelectCoinFlips {
                    keep_indices: vec![0],
                })
                .unwrap()
                .events,
        );
    }
    finish(&mut runner, &mut payoff_events);
    if matches!(rider, EmptyCoinRider::NestedCoinCounter) {
        assert_eq!(
            flips(&payoff_events),
            vec![true],
            "reach: delayed payload win"
        );
    }
    if player_rider {
        for (player, life, hand) in player_before {
            let selected = player == P1 && !remove_b;
            assert_eq!(
                runner.life(player),
                life + if selected
                    && matches!(
                        rider,
                        EmptyCoinRider::GainLifeController | EmptyCoinRider::GainLifeOwner
                    ) {
                    3
                } else {
                    0
                },
                "life payoff player={player:?}; illegal B={remove_b}",
            );
            let expected_draw = selected
                && !matches!(
                    rider,
                    EmptyCoinRider::GainLifeController | EmptyCoinRider::GainLifeOwner
                );
            let current = &runner.state().players[player.0 as usize].hand;
            if expected_draw {
                assert_eq!(current.len(), hand.len() + 1);
                assert_eq!(
                    runner.state().objects[current.last().unwrap()].name,
                    "Draw payoff sentinel"
                );
            } else {
                assert_eq!(*current, hand, "no substitute recipient player={player:?}");
            }
        }
    }
    for id in [a, b, h] {
        let selected = id == b && !remove_b;
        match rider {
            EmptyCoinRider::Counter
            | EmptyCoinRider::CounterAfterGainLife
            | EmptyCoinRider::OptionalCounter
            | EmptyCoinRider::NestedCoinCounter => assert_eq!(
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
            EmptyCoinRider::Saddle => assert_eq!(
                runner.state().objects[&id].is_saddled,
                selected,
                "saddle payoff object={id:?}; illegal B={remove_b}",
            ),
            EmptyCoinRider::Sacrifice => assert_eq!(
                runner.state().objects[&id].zone,
                if selected {
                    Zone::Graveyard
                } else if id == b && remove_b {
                    Zone::Exile
                } else {
                    Zone::Battlefield
                },
                "sacrifice payoff object={id:?}; illegal B={remove_b}",
            ),
            EmptyCoinRider::DrawController
            | EmptyCoinRider::DrawOwner
            | EmptyCoinRider::GainLifeController
            | EmptyCoinRider::GainLifeOwner
            | EmptyCoinRider::OptionalDrawController
            | EmptyCoinRider::OptionalDrawOwner => {}
        }
    }
    assert!(
        runner.state().delayed_triggers.is_empty(),
        "one-shot trigger consumed"
    );
    assert_finished(&runner);
}

// Real cards: none of the 86 printed coin-flip cards in the exported card data exhibits this shape:
// Spell form: the coin win branch's declared B gets an end-step delayed +1/+1 counter; B exiled in
// response leaves the delayed trigger's target list empty and nothing receives the counter. Nearest
// printed card: Goblin Kites ("{R}: Target creature you control with toughness 2 or less gains
// flying until end of turn. Flip a coin at the beginning of the next end step. If you lose the
// flip, sacrifice that creature."); Frenetic Sliver ("All Slivers have "{0}: If this permanent is
// on the battlefield, flip a coin. If you win the flip, exile this permanent and return it to the
// battlefield under its owner's control at the beginning of the next end step. If you lose the
// flip, sacrifice it."") -- Goblin Kites delays an effect on an earlier target; Frenetic Sliver
// delays from a won branch; neither declares a target inside a branch.
#[test]
fn delayed_coin_branch_keeps_its_empty_illegal_target_binding() {
    for remove_b in [false, true] {
        empty_coin_binding_scenario(remove_b, false, false, false, EmptyCoinRider::Counter);
    }
}

// Real cards: none of the 86 printed coin-flip cards in the exported card data exhibits this shape:
// Activated attached-Equipment form of the delayed-counter rider: when declared B is exiled, the
// empty binding is not refilled with the Equipment's host creature. Nearest printed card: Goblin
// Kites ("{R}: Target creature you control with toughness 2 or less gains flying until end of turn.
// Flip a coin at the beginning of the next end step. If you lose the flip, sacrifice that
// creature.") -- Goblin Kites delays an effect on its earlier declared target; it lacks an attached
// source and a branch-declared target.
#[test]
fn delayed_coin_branch_empty_binding_is_not_reseeded_from_attached_host() {
    for remove_b in [false, true] {
        empty_coin_binding_scenario(remove_b, true, false, false, EmptyCoinRider::Counter);
    }
}

// Real cards: none of the 86 printed coin-flip cards in the exported card data exhibits this shape:
// The delayed-counter rider under Krark's Thumb: at the keep prompt, a save/restore round trip
// keeps the won branch's B binding, or its empty binding if B was exiled. Nearest printed card:
// Goblin Kites ("{R}: Target creature you control with toughness 2 or less gains flying until end
// of turn. Flip a coin at the beginning of the next end step. If you lose the flip, sacrifice that
// creature."); Krark's Thumb ("If you would flip a coin, instead flip two coins and ignore one.")
// -- Goblin Kites supplies the delayed referent to an earlier target; Krark's Thumb supplies the
// keep prompt; neither has branch-declared targets.
#[test]
fn delayed_coin_branch_empty_binding_survives_keep_restore() {
    for remove_b in [false, true] {
        empty_coin_binding_scenario(remove_b, false, false, true, EmptyCoinRider::Counter);
    }
}

// Real cards: none of the 86 printed coin-flip cards in the exported card data exhibits this shape:
// The win branch's declared B feeds a second, nested coin whose win delays the counter on B; an
// exiled B stays an unavailable binding through the inner coin. Nearest printed card: Goblin Kites
// ("{R}: Target creature you control with toughness 2 or less gains flying until end of turn. Flip
// a coin at the beginning of the next end step. If you lose the flip, sacrifice that creature.") --
// Goblin Kites delays a coin acting on its earlier target; it has no coin nested inside a coin
// branch.
#[test]
fn nested_coin_inherits_an_unavailable_announced_binding() {
    for remove_b in [false, true] {
        empty_coin_binding_scenario(remove_b, false, true, false, EmptyCoinRider::Counter);
    }
}

// Real cards: none of the 86 printed coin-flip cards in the exported card data exhibits this shape:
// Nested-coin delayed-counter rider under Krark's Thumb: the inner keep prompt carries only B's
// binding, or its 'illegal announcement' missing reason, across save/restore. Nearest printed card:
// Goblin Kites ("{R}: Target creature you control with toughness 2 or less gains flying until end
// of turn. Flip a coin at the beginning of the next end step. If you lose the flip, sacrifice that
// creature."); Krark's Thumb ("If you would flip a coin, instead flip two coins and ignore one.")
// -- Goblin Kites delays a coin on its earlier target; Krark's Thumb gives keep prompts; neither
// nests coins or declares branch targets.
#[test]
fn nested_coin_missing_only_context_survives_inner_keep_restore() {
    for remove_b in [true, false] {
        empty_coin_binding_scenario(remove_b, false, true, true, EmptyCoinRider::Counter);
    }
}

// Real cards: none of the 86 printed coin-flip cards in the exported card data exhibits this shape:
// Attached-Equipment form whose delayed end-of-combat rider makes declared attacker B become
// blocked; an exiled B is not replaced by the Equipment's attacking host. Nearest printed card:
// Goblin Kites ("{R}: Target creature you control with toughness 2 or less gains flying until end
// of turn. Flip a coin at the beginning of the next end step. If you lose the flip, sacrifice that
// creature.") -- Goblin Kites delays an effect on its earlier target; no listed coin card makes a
// creature become blocked.
#[test]
fn detached_become_blocked_does_not_replace_an_illegal_announcement_with_its_host() {
    for remove_b in [false, true] {
        empty_coin_binding_scenario(remove_b, true, false, false, EmptyCoinRider::BecomeBlocked);
    }
}

// Real cards: none of the 86 printed coin-flip cards in the exported card data exhibits this shape:
// Attached-Equipment form whose delayed rider gives declared B a damage-prevention replacement; an
// exiled B gives the shield to no one, not to the host. Nearest printed card: Impulsive Maneuvers
// ("Whenever a creature attacks, flip a coin. If you win the flip, the next time that creature
// would deal combat damage this turn, it deals double that damage instead. If you lose the flip,
// the next time that creature would deal combat damage this turn, prevent that damage."); Desperate
// Gambit ("Choose a source you control and flip a coin. If you win the flip, the next time that
// source would deal damage this turn, it deals double that damage instead. If you lose the flip,
// the next time it would deal damage this turn, prevent that damage.") -- Both put next-time damage
// prevention on a coin-chosen referent, but immediately rather than delayed, and without a
// branch-declared target.
#[test]
fn detached_target_replacement_does_not_replace_an_illegal_announcement_with_its_host() {
    for remove_b in [false, true] {
        empty_coin_binding_scenario(remove_b, true, false, false, EmptyCoinRider::Replacement);
    }
}

// Real cards: none of the 86 printed coin-flip cards in the exported card data exhibits this shape:
// Nested-coin form where the delayed-counter trigger follows a targetless 'gain 1 life' head in the
// inner win branch; B's binding (or emptiness) survives that head. Nearest printed card: Goblin
// Kites ("{R}: Target creature you control with toughness 2 or less gains flying until end of turn.
// Flip a coin at the beginning of the next end step. If you lose the flip, sacrifice that
// creature.") -- Goblin Kites delays an effect on its earlier target; it has no targetless head
// before the delayed instruction and no nested coin.
#[test]
fn nested_coin_targetless_head_preserves_delayed_binding() {
    for remove_b in [true, false] {
        empty_coin_binding_scenario(
            remove_b,
            false,
            true,
            false,
            EmptyCoinRider::CounterAfterGainLife,
        );
    }
}

// Real cards: none of the 86 printed coin-flip cards in the exported card data exhibits this shape:
// The targetless-head nested-coin delayed-counter rider, also passing through Krark's Thumb keep
// prompts and a save/restore at the inner coin. Nearest printed card: Goblin Kites ("{R}: Target
// creature you control with toughness 2 or less gains flying until end of turn. Flip a coin at the
// beginning of the next end step. If you lose the flip, sacrifice that creature."); Krark's Thumb
// ("If you would flip a coin, instead flip two coins and ignore one.") -- Goblin Kites delays an
// effect on its earlier target; Krark's Thumb gives the keep prompt; neither nests coins.
#[test]
fn nested_coin_targetless_head_preserves_delayed_binding_after_inner_keep_restore() {
    for remove_b in [true, false] {
        empty_coin_binding_scenario(
            remove_b,
            false,
            true,
            true,
            EmptyCoinRider::CounterAfterGainLife,
        );
    }
}

// Real cards: none of the 86 printed coin-flip cards in the exported card data exhibits this shape:
// Three-player attached-Equipment form whose delayed rider makes B's controller or owner draw or
// gain 3 life; an exiled B makes nobody draw or gain, not the host's player. Nearest printed card:
// Goblin Lyre ("Sacrifice this artifact: Flip a coin. If you win the flip, this artifact deals
// damage to target opponent or planeswalker equal to the number of creatures you control. If you
// lose the flip, this artifact deals damage to you equal to the number of creatures that opponent
// or that planeswalker's controller controls."); Aleatory ("Cast this spell only during combat
// after blockers are declared. / Flip a coin. If you win the flip, target creature gets +1/+1 until
// end of turn. / Draw a card at the beginning of the next turn's upkeep.") -- Goblin Lyre scopes by
// its target's controller, but not delayed; Aleatory delays a draw for its caster, not for a
// target's player.
#[test]
fn delayed_parent_player_projections_do_not_substitute_other_players() {
    for rider in [
        EmptyCoinRider::DrawController,
        EmptyCoinRider::DrawOwner,
        EmptyCoinRider::GainLifeController,
        EmptyCoinRider::GainLifeOwner,
    ] {
        for remove_b in [true, false] {
            empty_coin_binding_scenario(remove_b, true, false, false, rider);
        }
    }
}

// Real cards: none of the 86 printed coin-flip cards in the exported card data exhibits this shape:
// The player-projection rider made optional ('you may'): the optional prompt holds only B's
// binding, and accepting draws only for B's controller/owner, nobody if B was exiled. Nearest
// printed card: Aleatory ("Cast this spell only during combat after blockers are declared. / Flip a
// coin. If you win the flip, target creature gets +1/+1 until end of turn. / Draw a card at the
// beginning of the next turn's upkeep."); Goblin Lyre ("Sacrifice this artifact: Flip a coin. If
// you win the flip, this artifact deals damage to target opponent or planeswalker equal to the
// number of creatures you control. If you lose the flip, this artifact deals damage to you equal to
// the number of creatures that opponent or that planeswalker's controller controls.") -- Aleatory
// delays a draw from a coin spell but to its caster, non-optional; Goblin Lyre projects a player
// from a target.
#[test]
fn optional_delayed_parent_player_projections_capture_no_substitute_recipient() {
    for rider in [
        EmptyCoinRider::OptionalDrawController,
        EmptyCoinRider::OptionalDrawOwner,
    ] {
        for remove_b in [true, false] {
            empty_coin_binding_scenario(remove_b, true, false, false, rider);
        }
    }
}

// Real cards: none of the 86 printed coin-flip cards in the exported card data exhibits this shape:
// Attached, nested form whose delayed payload is itself a coin putting a counter on B, with and
// without Krark's Thumb; the delayed snapshot keeps B, or nothing. Nearest printed card: Goblin
// Kites ("{R}: Target creature you control with toughness 2 or less gains flying until end of turn.
// Flip a coin at the beginning of the next end step. If you lose the flip, sacrifice that
// creature."); Krark's Thumb ("If you would flip a coin, instead flip two coins and ignore one.")
// -- Goblin Kites' delayed end-step coin acts on its earlier declared target; it lacks a
// branch-declared target and nested coins.
#[test]
fn nested_coin_payload_preserves_delayed_snapshot_identity() {
    for keep_restore in [false, true] {
        for remove_b in [true, false] {
            empty_coin_binding_scenario(
                remove_b,
                true,
                true,
                keep_restore,
                EmptyCoinRider::NestedCoinCounter,
            );
        }
    }
}

// Real cards: none of the 86 printed coin-flip cards in the exported card data exhibits this shape:
// Attached-Equipment form whose delayed rider makes declared B saddled; an exiled B saddles nothing
// rather than the Equipment's host. Nearest printed card: Goblin Kites ("{R}: Target creature you
// control with toughness 2 or less gains flying until end of turn. Flip a coin at the beginning of
// the next end step. If you lose the flip, sacrifice that creature.") -- Goblin Kites delays an
// effect on an earlier declared target; no listed coin card saddles anything.
#[test]
fn detached_saddle_does_not_replace_an_illegal_announcement_with_its_host() {
    for remove_b in [true, false] {
        empty_coin_binding_scenario(remove_b, true, false, false, EmptyCoinRider::Saddle);
    }
}

// Real cards: none of the 86 printed coin-flip cards in the exported card data exhibits this shape:
// Attached-Equipment form whose delayed rider sacrifices declared B; an exiled B leaves nothing
// sacrificed and the host untouched. Nearest printed card: Goblin Kites ("{R}: Target creature you
// control with toughness 2 or less gains flying until end of turn. Flip a coin at the beginning of
// the next end step. If you lose the flip, sacrifice that creature.") -- Goblin Kites delays a
// sacrifice of its earlier declared target behind a coin; it lacks a branch-declared target.
#[test]
fn detached_sacrifice_does_not_replace_an_illegal_announcement_with_its_host() {
    for remove_b in [true, false] {
        empty_coin_binding_scenario(remove_b, true, false, false, EmptyCoinRider::Sacrifice);
    }
}

// Real cards: none of the 86 printed coin-flip cards in the exported card data exhibits this shape:
// Attached-Equipment form whose optional delayed counter rider on declared B prompts the
// controller; accepting counters B only, nobody if B was exiled. Nearest printed card: Goblin Kites
// ("{R}: Target creature you control with toughness 2 or less gains flying until end of turn. Flip
// a coin at the beginning of the next end step. If you lose the flip, sacrifice that creature.") --
// Goblin Kites delays a mandatory effect on its earlier target; it has no optional rider or
// branch-declared target.
#[test]
fn optional_detached_counter_does_not_replace_an_illegal_announcement_with_its_host() {
    for remove_b in [true, false] {
        empty_coin_binding_scenario(
            remove_b,
            true,
            false,
            false,
            EmptyCoinRider::OptionalCounter,
        );
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

// Real cards: Crooked Scales (printed) has the two outcome-declared targets this test needs, but in
// this test the printed closing clause "unless you pay {3} and repeat this process" changes the
// flow (the losing outcome would first ask to pay {3}; the engine does not complete that prompt or
// the repeat, a tracked follow-up), so the test uses the card's text without that clause
// (CROOKED_SCALES_WITHOUT_PAYMENT). Shape: The two-branch Destroy coin activation under Krark's
// Thumb: at the keep prompt each branch holds its pinned target, a restore with a tampered pin is
// refused, and a stale incarnation is not destroyed.
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
                CROOKED_SCALES_WITHOUT_PAYMENT,
                "Synthetic Scales",
                &[],
                &["Artifact".into()],
                &[],
            );
            let definition: AbilityDefinition = parsed.abilities[0].clone();
            assert_eq!(definition.kind, AbilityKind::Activated);
            let source = scenario
                .add_artifact_from_oracle(P0, "Synthetic Scales", CROOKED_SCALES_WITHOUT_PAYMENT)
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
    use engine::types::ability::{EffectError, ResolvedAbility};
    // Tavern Swindler, printed text (byte-identical to the exported MTGJSON oracle_text).
    const TAVERN_SWINDLER: &str =
        "{T}, Pay 3 life: Flip a coin. If you win the flip, you gain 6 life.";
    let definition = parse_oracle_text(
        TAVERN_SWINDLER,
        "Tavern Swindler",
        &[],
        &["Creature".into()],
        &["Human".into(), "Rogue".into()],
    )
    .abilities
    .remove(0);
    assert!(
        matches!(
            definition.effect.as_ref(),
            Effect::FlipCoins {
                win_effect: Some(_),
                lose_effect: None,
                ..
            }
        ),
        "reach: the printed text parses to a win-only coin: {definition:#?}"
    );
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_creature_from_oracle(P0, "Tavern Swindler", 2, 2, TAVERN_SWINDLER)
        .id();
    let mut runner = scenario.build();
    pin_outcomes(&mut runner, &[true, false, true]);
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
    assert_eq!(runner.life(P0), 26);
    assert_finished(&runner);
}

// Real cards: the two ParentTarget rows use Goblin Bangchuckers, printed (Goblin Bangchuckers
// ("{T}: Flip a coin. If you win the flip, this creature deals 2 damage to any target. If you lose
// the flip, this creature deals 2 damage to itself.")); the engine parses its loss clause as a
// ParentTarget damage with no declaration of its own (the printed meaning is damage to itself; see
// the PR body). The self-damage row is a made-up control for that printed meaning, because the
// parser cannot yet produce it. Orcish Captain ("{1}: Flip a coin. If you win the flip, target Orc
// creature gets +2/+0 until end of turn. If you lose the flip, it gets -0/-2 until end of turn.")
// has a loss clause whose "it" refers to the win clause's target; this test does not model that and
// the PR body lists it as a known limitation.
#[test]
fn bangchuckers_shaped_loss_never_borrows_the_win_only_target() {
    use engine::types::ability::{AbilityDefinition, AbilityKind, QuantityExpr, TypedFilter};
    for (won, self_damage) in [(true, false), (false, false), (false, true)] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let source = if self_damage {
            // Control: the printed loss semantics (the creature damages itself) as an AST, because
            // the parser cannot yet produce it (the printed text parses to a ParentTarget loss).
            let damage = |target| Effect::DealDamage {
                amount: QuantityExpr::Fixed { value: 2 },
                target,
                damage_source: None,
                excess: None,
            };
            let coin = Effect::FlipCoins {
                flips: CoinFlipRun::single(),
                win_effect: Some(Box::new(AbilityDefinition::new(
                    AbilityKind::Activated,
                    damage(TargetFilter::Typed(TypedFilter::creature())),
                ))),
                lose_effect: Some(Box::new(AbilityDefinition::new(
                    AbilityKind::Activated,
                    damage(TargetFilter::SelfRef),
                ))),
                flipper: TargetFilter::Controller,
            };
            scenario
                .add_creature(P0, "Synthetic Bangchucker", 3, 5)
                .with_ability_definition(AbilityDefinition::new(AbilityKind::Activated, coin))
                .id()
        } else {
            // Goblin Bangchuckers, printed text: the engine parses its loss clause as a
            // ParentTarget damage with no declaration of its own.
            scenario
                .add_creature_from_oracle(P0, "Goblin Bangchuckers", 2, 2, GOBLIN_BANGCHUCKERS)
                .id()
        };
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

// Real cards: none of the 86 printed coin-flip cards in the exported card data exhibits this shape:
// A spell flipping three coins or until a loss, whose win branch targets one cast-declared creature
// and adds +1/+1 counters equal to that target's current count each win. Nearest printed card:
// Crazed Firecat ("When this creature enters, flip a coin until you lose a flip. Put a +1/+1
// counter on this creature for each flip you won."); Goblin Traprunner ("Whenever this creature
// attacks, flip three coins. For each flip you win, create a 1/1 red Goblin creature token that's
// tapped and attacking.") -- Firecat: until-loss run with a counter per win, but on itself with
// fixed count; Traprunner: three-coin per-win effect, untargeted.
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
    use engine::types::identifiers::ObjectIncarnationRef;
    for keyed_child in [None, Some(false), Some(true)] {
        // Chaotic Strike, printed text: the win-only branch targets a creature declared at cast.
        let (mut runner, spell, creatures) = chaotic_fixture(true);
        let (target, distractor) = (creatures[0], creatures[1]);
        let old = ObjectIncarnationRef::from_object(&runner.state().objects[&target]);
        runner
            .state_mut()
            .objects
            .get_mut(&target)
            .unwrap()
            .incarnation += 1;
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
        let pt = |id: ObjectId| {
            let object = &runner.state().objects[&id];
            (object.power.unwrap(), object.toughness.unwrap())
        };
        // Chaotic Strike's win pumps +1/+1 until end of turn; a stale pin pumps nothing.
        assert_eq!(pt(target), if stale { (2, 2) } else { (3, 3) });
        assert_eq!(
            pt(distractor),
            (4, 4),
            "the unchosen creature is never pumped"
        );
        assert_finished(&runner);
    }
}

// Real cards: none of the 86 printed coin-flip cards in the exported card data exhibits this shape
// (a chosen creature, a losing control change and an independent instruction after the coin). Risky
// Move (supported) has the choose-creature-you-control, flip, lose-control structure but is a
// trigger on gaining control of the enchantment from another player and lacks the trailing
// independent instruction this test observes (+5 life). Shape: A resolution-time choice of a
// creature you control, then a coin whose loss gives that chosen creature to an opponent, then an
// independent life gain, under Krark's Thumb's keep choice with a possibly empty choice. Printed
// neighbours: Risky Move ("At the beginning of each player's upkeep, that player gains control of
// this enchantment. / When you gain control of this enchantment from another player, choose a
// creature you control and an opponent. Flip a coin. If you lose the flip, that opponent gains
// control of that creature.").
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

// Real cards: Crooked Scales (printed) has the two outcome-declared targets this test needs, but in
// this test the printed closing clause "unless you pay {3} and repeat this process" changes the
// flow (the losing outcome would first ask to pay {3}; the engine does not complete that prompt or
// the repeat, a tracked follow-up), so the test uses the card's text without that clause
// (CROOKED_SCALES_WITHOUT_PAYMENT). Shape: A two-branch coin (win destroys an opponent's target
// creature, loss destroys your target creature) followed by an independent 'exile target artifact',
// optionally nested in an outer coin's win; declaration order Won, Lost, successor.
#[test]
fn coin_children_precede_a_targeted_successor_and_nested_children() {
    use engine::types::ability::{AbilityDefinition, AbilityKind};
    for nested in [false, true] {
        for won in [false, true] {
            for bulk in [false, true] {
                let mut root = parse_oracle_text(
                    CROOKED_SCALES_WITHOUT_PAYMENT,
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
                        "Shattering Blow",
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

// Real cards: Crooked Scales (printed) has the two outcome-declared targets this test needs, but in
// this test the printed closing clause "unless you pay {3} and repeat this process" changes the
// flow (the losing outcome would first ask to pay {3}; the engine does not complete that prompt or
// the repeat, a tracked follow-up), so the test uses the card's text without that clause
// (CROOKED_SCALES_WITHOUT_PAYMENT). Shape: A two-branch opponent/own targeted destroy coin followed
// by an independent life gain, where instant responses exile one or both announced branch targets
// before resolution; legality is per branch occurrence.
#[test]
fn illegal_coin_children_do_not_substitute_distractors_or_suppress_a_legal_tail() {
    use engine::types::ability::{AbilityDefinition, AbilityKind, QuantityExpr};
    for removed in [0, 1, 2, 3] {
        for won in [false, true] {
            let mut definition = parse_oracle_text(
                CROOKED_SCALES_WITHOUT_PAYMENT,
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
                            "Final Death",
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

// Real cards: none of the 86 printed coin-flip cards in the exported card data exhibits this shape:
// An activated coin with a separate 'target creature' in each branch over the same pool, letting
// one creature be declared for both occurrences (CR 115.3). Nearest printed card: Mogg Assassin
// ("{T}: You choose target creature an opponent controls, and that opponent chooses target
// creature. Flip a coin. If you win the flip, destroy the creature you chose. If you lose the flip,
// destroy the creature your opponent chose."); Crooked Scales ("{4}, {T}: Flip a coin. If you win
// the flip, destroy target creature an opponent controls. If you lose the flip, destroy target
// creature you control unless you pay {3} and repeat this process.") -- Mogg Assassin: two declared
// creature targets that may coincide, but declared before the coin by two choosers; Scales: branch
// targets with disjoint pools.
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
// Real cards: none of the 86 printed coin-flip cards in the exported card data exhibits this shape:
// A spell with opponent/own branch target creatures and an independent artifact-target tail,
// retargeted by a choose-new-targets spell through a positional prompt that survives save/restore,
// optionally after one branch target was exiled. Nearest printed card: Crooked Scales ("{4}, {T}:
// Flip a coin. If you win the flip, destroy target creature an opponent controls. If you lose the
// flip, destroy target creature you control unless you pay {3} and repeat this process."); Invert
// Polarity ("Choose target spell, then flip a coin. If you win the flip, gain control of that spell
// and you may choose new targets for it. If you lose the flip, counter that spell.") -- Scales:
// same two branch targets, no tail, Unimplemented suffix; Invert Polarity: lets a player choose new
// targets for a spell.
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
                        "Shattering Blow",
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
                        "Final Death",
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

// Real cards: none of the 86 printed coin-flip cards in the exported card data exhibits this shape:
// An activated coin whose win branch destroys exactly two target creatures in one occurrence,
// rejecting the same creature twice and accepting two distinct ones. Nearest printed card: Goblin
// Archaeologist ("{R}, {T}: Flip a coin. If you win the flip, destroy target artifact and untap
// this creature. If you lose the flip, sacrifice this creature."); Mogg Assassin ("{T}: You choose
// target creature an opponent controls, and that opponent chooses target creature. Flip a coin. If
// you win the flip, destroy the creature you chose. If you lose the flip, destroy the creature your
// opponent chose.") -- Archaeologist: win-only targeted destroy with one target; Mogg Assassin: two
// creature targets, but separate occurrences before the coin.
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

// Real cards: Crooked Scales (printed) has the two outcome-declared targets this test needs, but in
// this test the printed closing clause "unless you pay {3} and repeat this process" changes the
// flow (the losing outcome would first ask to pay {3}; the engine does not complete that prompt or
// the repeat, a tracked follow-up), so the test uses the card's text without that clause
// (CROOKED_SCALES_WITHOUT_PAYMENT). Shape: In three players, an opponent copies a two-branch
// opponent/own targeted coin activation and chooses new branch targets relative to the copy's
// controller, leaving the original intact.
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
            CROOKED_SCALES_WITHOUT_PAYMENT,
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

// Real cards: Crooked Scales (printed) has the two outcome-declared targets this test needs, but in
// this test the printed closing clause "unless you pay {3} and repeat this process" changes the
// flow (the losing outcome would first ask to pay {3}; the engine does not complete that prompt or
// the repeat, a tracked follow-up), so the test uses the card's text without that clause
// (CROOKED_SCALES_WITHOUT_PAYMENT). Shape: In three players, a two-branch opponent/own targeted
// coin activated from a source owned by another player, with the source destroyed, the own target
// blinked, or the own target stolen in response.
#[test]
fn three_seat_borrowed_source_and_target_keep_controller_and_incarnation_authority() {
    use engine::types::player::PlayerId;
    for alteration in ["unchanged", "source leaves", "blink", "control changes"] {
        for won in [false, true] {
            let p2 = PlayerId(2);
            let mut scenario = GameScenario::new_n_player(3, 7);
            scenario.at_phase(Phase::PreCombatMain);
            let mut definition = parse_oracle_text(
                CROOKED_SCALES_WITHOUT_PAYMENT,
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
                .add_spell_to_hand_from_oracle(P0, "Shatter", true, "Destroy target artifact.")
                .with_mana_cost(ManaCost::zero())
                .id();
            let blink = scenario.add_spell_to_hand_from_oracle(P0, "Ghostly Flicker", true,
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

// Real cards: none of the 86 printed coin-flip cards in the exported card data exhibits this shape:
// A two-branch targeted coin whose win target is made optional (up to one) with possibly no legal
// choice, then a mandatory loss target and an independent artifact-target tail. Nearest printed
// card: Crooked Scales ("{4}, {T}: Flip a coin. If you win the flip, destroy target creature an
// opponent controls. If you lose the flip, destroy target creature you control unless you pay {3}
// and repeat this process.") -- Same two branch targets; neither is optional, there is no tail, and
// the repeat suffix is Unimplemented.
#[test]
fn optional_empty_coin_slot_does_not_steal_the_later_target() {
    for empty in [false, true] {
        let mut definition = parse_oracle_text(
            CROOKED_SCALES,
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
                "Shattering Blow",
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

// Real cards: none of the 86 printed coin-flip cards in the exported card data exhibits this shape:
// An untargeted coin (win gain 3 life, loss lose 2 life) as the chosen-pile effect after an
// opponent separates creatures into piles, compared to the same coin cast alone. Nearest printed
// card: Winter Sky ("Flip a coin. If you win the flip, Winter Sky deals 1 damage to each creature
// and each player. If you lose the flip, each player draws a card."); Bottle of Suleiman ("{1},
// Sacrifice this artifact: Flip a coin. If you win the flip, create a 5/5 colorless Djinn artifact
// creature token with flying. If you lose the flip, this artifact deals 5 damage to you.") -- Both
// have untargeted win and loss outcomes affecting players; neither is nested under a pile
// separation.
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

// Real cards: none of the 86 printed coin-flip cards in the exported card data exhibits this shape:
// A Mizzix's Mastery copy of an instant whose two branch targets an elected opponent announces and
// whose land-target tail the caster announces; the announcer concedes mid-choice and is re-elected.
// Nearest printed card: Mogg Assassin ("{T}: You choose target creature an opponent controls, and
// that opponent chooses target creature. Flip a coin. If you win the flip, destroy the creature you
// chose. If you lose the flip, destroy the creature your opponent chose.") -- An opponent chooses
// one of its targets; but that target is declared before the coin, not per branch, on an activated
// ability.
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
            "Caustic Rain",
            &[],
            &["Sorcery".into()],
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
        // Crooked Scales, printed text. Eight mana pays two activations; the artifact is untapped
        // between them (as an untap effect would), so both activations go through the
        // production activation pipeline with the printed {4},{T} cost.
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let source = scenario
            .add_artifact_from_oracle(P0, "Crooked Scales", CROOKED_SCALES)
            .id();
        let own = [
            scenario.add_creature(P0, "Chosen Own", 2, 2).id(),
            scenario.add_creature(P0, "Own Distractor", 3, 3).id(),
        ];
        let opposing = [
            scenario.add_creature(P1, "Chosen Opponent", 4, 4).id(),
            scenario.add_creature(P1, "Opponent Distractor", 5, 5).id(),
        ];
        mana(&mut scenario, 8);
        let mut runner = scenario.build();
        pin_outcomes(&mut runner, &[true, true]);
        for opponent in [opposing[0], if same { opposing[0] } else { opposing[1] }] {
            runner.state_mut().objects.get_mut(&source).unwrap().tapped = false;
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
            if matches!(runner.state().waiting_for, WaitingFor::ManaPayment { .. }) {
                runner.act(GameAction::PassPriority).unwrap();
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
        CROOKED_SCALES,
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

// Real cards: none of the 86 printed coin-flip cards in the exported card data exhibits this shape:
// An activated coin ability (one creature target per won/lost branch, or one two-target won branch)
// copied on the stack by a copy-activated-ability spell that may choose new targets, keeping
// per-occurrence uniqueness. Nearest printed card: Crooked Scales ("{4}, {T}: Flip a coin. If you
// win the flip, destroy target creature an opponent controls. If you lose the flip, destroy target
// creature you control unless you pay {3} and repeat this process."); Krark, the Thumbless
// ("Whenever you cast an instant or sorcery spell, flip a coin. If you lose the flip, return that
// spell to its owner's hand. If you win the flip, copy that spell, and you may choose new targets
// for the copy. / Partner (You can have two commanders if both have partner.)") -- Crooked Scales
// has a separate target in each branch of an activated coin but no copy; Krark copies with new
// targets but copies spells.
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

// Real cards: none of the 86 printed coin-flip cards in the exported card data exhibits this shape:
// A creature target declared before the coin; the won branch opens with a target-free life gain and
// then gives control of that earlier target away, inline and after a Krark's Thumb keep with
// save/restore. Nearest printed card: Invert Polarity ("Choose target spell, then flip a coin. If
// you win the flip, gain control of that spell and you may choose new targets for it. If you lose
// the flip, counter that spell."); Goblin Kites ("{R}: Target creature you control with toughness 2
// or less gains flying until end of turn. Flip a coin at the beginning of the next end step. If you
// lose the flip, sacrifice that creature.") -- Both act on a target declared before the coin;
// neither puts a target-free instruction ahead of the rider that references it.
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

// Real cards: none of the 86 printed coin-flip cards in the exported card data exhibits this shape:
// A modal 'choose one' spell whose first mode is a coin with a won-branch life gain, saved and
// restored at the mode-choice prompt before the mode is selected. Nearest printed card: Tavern
// Swindler ("{T}, Pay 3 life: Flip a coin. If you win the flip, you gain 6 life."); Stitch in Time
// ("Flip a coin. If you win the flip, take an extra turn after this one.") -- Both have a
// target-free coin with only a won-branch reward; neither is modal.
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

// Real cards: none of the 86 printed coin-flip cards in the exported card data exhibits this shape:
// An Enchant-creature Aura's activated ability, bare or under a won coin branch, gains control of
// one target permanent per opponent and then attaches the Aura to a target host that must satisfy
// its Enchant restriction. Nearest printed card: Invert Polarity ("Choose target spell, then flip a
// coin. If you win the flip, gain control of that spell and you may choose new targets for it. If
// you lose the flip, counter that spell.") -- Its won branch gains control of a targeted object; it
// has no per-opponent fan-out, no Aura and no attach.
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
    // Stitch in Time, printed text: a target-free win-only coin (a Sorcery).
    let spell = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Stitch in Time",
            false,
            "Flip a coin. If you win the flip, take an extra turn after this one.",
        )
        .id();
    let mut runner = scenario.build();
    pin_outcomes(&mut runner, &[true, false]);
    let extra_turns_before = runner.state().extra_turns.len();
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
    // CR 500.7: the won flip queues exactly one extra turn for the caster.
    assert_eq!(
        runner.state().extra_turns.len(),
        extra_turns_before + 1,
        "the kept winning flip queues Stitch in Time's extra turn"
    );
    assert_finished(&runner);
}

// Real cards: none of the 86 printed coin-flip cards in the exported card data exhibits this shape:
// A coin spell with a separate 'target creature' in each branch: both targets are announced at
// cast, both emit BecomesTarget, and a 'whenever you cast a spell that targets this creature'
// trigger fires once. Nearest printed card: Crooked Scales ("{4}, {T}: Flip a coin. If you win the
// flip, destroy target creature an opponent controls. If you lose the flip, destroy target creature
// you control unless you pay {3} and repeat this process."); Orcish Captain ("{1}: Flip a coin. If
// you win the flip, target Orc creature gets +2/+0 until end of turn. If you lose the flip, it gets
// -0/-2 until end of turn.") -- Crooked Scales targets separately in each branch but is an
// activated ability (and unimplemented); Orcish Captain's lost branch reuses the won branch's
// target.
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
    // Spinal Embrace, printed text (byte-identical to the exported MTGJSON oracle_text): gain
    // control of a creature and sacrifice it at the beginning of the next end step.
    const SPINAL_EMBRACE: &str = "Cast this spell only during combat.\nUntap target creature you don't control and gain control of it. It gains haste until end of turn. At the beginning of the next end step, sacrifice it. If you do, you gain life equal to its toughness.";
    for blink in [false, true] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::EndCombat);
        let creature = scenario
            .add_creature(P1, "Delayed non-coin referent", 2, 2)
            .id();
        let land = scenario
            .add_land_from_oracle(P0, "Other flicker target", "")
            .id();
        let embrace = scenario
            .add_spell_to_hand_from_oracle(P0, "Spinal Embrace", true, SPINAL_EMBRACE)
            .with_mana_cost(ManaCost::zero())
            .id();
        let flicker = scenario.add_spell_to_hand_from_oracle(P0, "Ghostly Flicker", true,
            "Exile two target artifacts, creatures, and/or lands you control, then return those cards to the battlefield under your control.").id();
        let mut runner = scenario.build();
        runner.cast(embrace).target_object(creature).resolve();
        assert_finished(&runner);
        assert_eq!(
            runner.state().objects[&creature].controller,
            P0,
            "reach: Spinal Embrace gained control of the creature"
        );
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
        // CR 603.7c + CR 400.7: the delayed sacrifice affects only the original object, and
        // the life gain happens only if the sacrifice happened.
        assert_eq!(
            runner.state().objects[&creature].zone,
            if blink {
                Zone::Battlefield
            } else {
                Zone::Graveyard
            }
        );
        assert_eq!(runner.life(P0), if blink { 20 } else { 22 });
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

// Real cards: the plain half uses Goblin Bangchuckers, printed (Goblin Bangchuckers ("{T}: Flip a
// coin. If you win the flip, this creature deals 2 damage to any target. If you lose the flip, this
// creature deals 2 damage to itself.")). The replacement half (declare a creature, then a
// damage-redirect shield to a chosen target) is made up: none of the 86 printed coin-flip cards in
// the exported card data does that. Nearest printed card: Desperate Gambit ("Choose a source you
// control and flip a coin. If you win the flip, the next time that source would deal damage this
// turn, it deals double that damage instead. If you lose the flip, the next time it would deal
// damage this turn, prevent that damage.") -- a coin that chooses a source and installs a damage
// replacement, but declares no creature first.
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
        let source = if replacement {
            scenario
                .add_spell_to_hand(P0, "Synthetic damage target coin", true)
                .with_ability(Effect::FlipCoins {
                    flips: CoinFlipRun::single(),
                    win_effect: Some(Box::new(branch)),
                    lose_effect: None,
                    flipper: TargetFilter::Controller,
                })
                .id()
        } else {
            // Goblin Bangchuckers, printed text: its win branch deals 2 damage to any target.
            scenario
                .add_creature_from_oracle(P0, "Goblin Bangchuckers", 2, 2, GOBLIN_BANGCHUCKERS)
                .id()
        };
        let mut runner = scenario.build();
        pin_outcomes(&mut runner, &[true]);
        let mut events = if replacement {
            let card_id = runner.state().objects[&source].card_id;
            runner
                .act(GameAction::CastSpell {
                    object_id: source,
                    card_id,
                    targets: vec![],
                    payment_mode: CastPaymentMode::Auto,
                })
                .unwrap()
                .events
        } else {
            runner
                .act(GameAction::ActivateAbility {
                    source_id: source,
                    ability_index: 0,
                    payment_mode: CastPaymentMode::Auto,
                })
                .unwrap()
                .events
        };
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

// Real cards: none of the 86 printed coin-flip cards in the exported card data exhibits this shape:
// An outer resolution-time 'target creature you control' choice that comes up empty, followed by a
// coin whose won branch declares its own creature target and gains control of it, inline and after
// Thumb save/restore. Nearest printed card: Invert Polarity ("Choose target spell, then flip a
// coin. If you win the flip, gain control of that spell and you may choose new targets for it. If
// you lose the flip, counter that spell."); Goblin Archaeologist ("{R}, {T}: Flip a coin. If you
// win the flip, destroy target artifact and untap this creature. If you lose the flip, sacrifice
// this creature.") -- Invert Polarity's won branch gains control of a target; Archaeologist's won
// branch declares its own target; neither has an empty prior choice.
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

// Real cards: none of the 86 printed coin-flip cards in the exported card data exhibits this shape:
// A coin spell whose won and lost branches each target an Aura, then a new host for it; the lost
// branch's host pool must use its own Aura's Enchant restriction. Nearest printed card: Crooked
// Scales ("{4}, {T}: Flip a coin. If you win the flip, destroy target creature an opponent
// controls. If you lose the flip, destroy target creature you control unless you pay {3} and repeat
// this process."); Goblin Archaeologist ("{R}, {T}: Flip a coin. If you win the flip, destroy
// target artifact and untap this creature. If you lose the flip, sacrifice this creature.") --
// Crooked Scales declares a separate target in each branch, Archaeologist one in its won branch;
// neither attaches anything.
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
    use engine::types::counter::CounterType;
    // Miraculous Recovery, printed text (byte-identical to the exported MTGJSON oracle_text).
    const MIRACULOUS_RECOVERY: &str =
        "Return target creature card from your graveyard to the battlefield. Put a +1/+1 counter on it.";
    let definition = parse_oracle_text(
        MIRACULOUS_RECOVERY,
        "Miraculous Recovery",
        &[],
        &["Instant".into()],
        &[],
    )
    .abilities
    .remove(0);
    assert!(
        matches!(definition.effect.as_ref(), Effect::ChangeZone { .. })
            && definition.forward_result,
        "reach: the printed text forwards the returned card to its rider: {definition:#?}"
    );
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let target = scenario
        .add_creature_to_graveyard(P0, "Returned non-coin referent", 2, 2)
        .id();
    let other = scenario.add_creature(P0, "Unrelated creature", 3, 3).id();
    let spell = scenario
        .add_spell_to_hand(P0, "Miraculous Recovery", true)
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
        Some(&1)
    );
    assert!(!runner.state().objects[&other]
        .counters
        .contains_key(&CounterType::Plus1Plus1));
    assert_finished(&runner);
}

// Real cards: none of the 86 printed coin-flip cards in the exported card data exhibits this shape:
// A coin whose won branch targets an artifact and lost branch targets a creature, then destroys a
// permanent sharing its card type; the dependent pool must use the lost creature. Nearest printed
// card: Crooked Scales ("{4}, {T}: Flip a coin. If you win the flip, destroy target creature an
// opponent controls. If you lose the flip, destroy target creature you control unless you pay {3}
// and repeat this process."); Goblin Archaeologist ("{R}, {T}: Flip a coin. If you win the flip,
// destroy target artifact and untap this creature. If you lose the flip, sacrifice this creature.")
// -- Crooked Scales has separate per-branch targets and Archaeologist a won-branch artifact target;
// neither has a dependent shares-a-type target.
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

// Real cards: none of the 86 printed coin-flip cards in the exported card data exhibits this shape:
// A coin whose lost branch declares a creature you control and then an opponent's creature, with a
// rider naming the branch's first declaration by slot index, checked under legality changes and
// Thumb save/restore. Nearest printed card: Mogg Assassin ("{T}: You choose target creature an
// opponent controls, and that opponent chooses target creature. Flip a coin. If you win the flip,
// destroy the creature you chose. If you lose the flip, destroy the creature your opponent
// chose."); Crooked Scales ("{4}, {T}: Flip a coin. If you win the flip, destroy target creature an
// opponent controls. If you lose the flip, destroy target creature you control unless you pay {3}
// and repeat this process.") -- Mogg Assassin splits targets between you and an opponent but
// declares them before the coin; Crooked Scales targets per branch but only one per branch.
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
    use engine::types::counter::CounterType;
    use engine::types::identifiers::ObjectIncarnationRef;
    // Big Play, printed text (byte-identical to the exported MTGJSON oracle_text): a
    // creature-target root whose later "Put a +1/+1 counter on it" inherits that target.
    const BIG_PLAY: &str = "Target creature gets +2/+2 and gains reach until end of turn. Put a +1/+1 counter on it. (A creature with reach can block creatures with flying.)";
    for stale in [false, true] {
        let definition = parse_oracle_text(BIG_PLAY, "Big Play", &[], &["Instant".into()], &[])
            .abilities
            .remove(0);
        assert!(
            definition.sub_ability.is_some(),
            "reach: the counter rider is a later instruction of the same ability: {definition:#?}"
        );
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
            if stale { 0 } else { 1 }
        );
        assert_finished(&runner);
    }
}

// Real cards: none of the 86 printed coin-flip cards in the exported card data exhibits this shape:
// A coin branch (won or lost, by argument) holding one fight instruction with two targets, a
// creature and then a permanent that shares a card type with it; the static and live pools must
// agree. Nearest printed card: Mogg Assassin ("{T}: You choose target creature an opponent
// controls, and that opponent chooses target creature. Flip a coin. If you win the flip, destroy
// the creature you chose. If you lose the flip, destroy the creature your opponent chose.") --
// Declares two creature targets in one coin ability; it has no fight, no shares-a-type dependency,
// and both targets precede the coin.
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

// Real cards: none of the 86 printed coin-flip cards in the exported card data exhibits this shape:
// The same_node_coin_scope fixture with the dependent two-target fight in the won branch. Nearest
// printed card: Mogg Assassin ("{T}: You choose target creature an opponent controls, and that
// opponent chooses target creature. Flip a coin. If you win the flip, destroy the creature you
// chose. If you lose the flip, destroy the creature your opponent chose.") -- Declares two creature
// targets in one coin ability; it has no fight, no shares-a-type dependency, and both targets
// precede the coin.
#[test]
fn coin_scope_same_node_won_static_pool_matches_live_prompt() {
    same_node_coin_scope(true);
}

// Real cards: none of the 86 printed coin-flip cards in the exported card data exhibits this shape:
// The same_node_coin_scope fixture with the dependent two-target fight in the lost branch. Nearest
// printed card: Mogg Assassin ("{T}: You choose target creature an opponent controls, and that
// opponent chooses target creature. Flip a coin. If you win the flip, destroy the creature you
// chose. If you lose the flip, destroy the creature your opponent chose.") -- Declares two creature
// targets in one coin ability; it has no fight, no shares-a-type dependency, and both targets
// precede the coin.
#[test]
fn coin_scope_same_node_lost_static_pool_matches_live_prompt() {
    same_node_coin_scope(false);
}

// Real cards: none of the 86 printed coin-flip cards in the exported card data exhibits this shape:
// An outer creature target precedes a coin whose lost branch targets a creature and destroys a
// target permanent attached to declared slot 0; the slot must resolve to the branch's creature, not
// the outer target. Nearest printed card: Invert Polarity ("Choose target spell, then flip a coin.
// If you win the flip, gain control of that spell and you may choose new targets for it. If you
// lose the flip, counter that spell."); Crooked Scales ("{4}, {T}: Flip a coin. If you win the
// flip, destroy target creature an opponent controls. If you lose the flip, destroy target creature
// you control unless you pay {3} and repeat this process.") -- Invert Polarity targets before the
// coin and Crooked Scales targets inside the branch; neither has an attached-to-declared-target
// reference.
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

// Real cards: none of the 86 printed coin-flip cards in the exported card data exhibits this shape:
// A lost branch declares a creature, then a player, then a target permanent sharing a card type
// with 'it'; the antecedent must stay the creature (not the player) and fail if the creature's type
// changes. Nearest printed card: Crooked Scales ("{4}, {T}: Flip a coin. If you win the flip,
// destroy target creature an opponent controls. If you lose the flip, destroy target creature you
// control unless you pay {3} and repeat this process.") -- Its lost branch declares its own target;
// it has no interposed player target and no dependent shares-a-type target.
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

// Real cards: no printed card exhibits this shape (searched every oracle_text in the exported card
// data for "shares a card type with it/that": 14 cards; none declares an artifact, a creature and a
// player before a target that shares a card type with the first). Shape: a non-coin chain declares
// an artifact, a creature and a player, then a target permanent sharing a card type with 'it'; the
// antecedent must stay the chain's first object (the artifact). Nearest printed cards: Daring Thief
// ("Inspired — Whenever this creature becomes untapped, you may exchange control of target nonland
// permanent you control and target permanent an opponent controls that shares a card type with
// it."); Chaotic Transformation ("Exile up to one target artifact, up to one target creature, up to
// one target enchantment, up to one target planeswalker, and/or up to one target land. For each
// permanent exiled this way, its controller reveals cards from the top of their library until they
// reveal a card that shares a card type with it, puts that card onto the battlefield, then
// shuffles.") -- the first has a later target that shares a card type with an earlier one; the
// second declares several typed targets in sequence.
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

// Real cards: none of the 86 printed coin-flip cards in the exported card data exhibits this shape:
// A shared builder for 'destroy target permanent that shares a card type with it', whose target
// pool depends on an earlier declared referent. Nearest printed card: Goblin Archaeologist ("{R},
// {T}: Flip a coin. If you win the flip, destroy target artifact and untap this creature. If you
// lose the flip, sacrifice this creature.") -- Destroys a typed target inside a coin branch; it has
// no dependency on an earlier declared referent.
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

// Real cards: none of the 86 printed coin-flip cards in the exported card data exhibits this shape:
// A creature-you-control target precedes a coin whose lost branch destroys two target permanents
// sharing a card type with that creature; the first dependent instance must not become the second's
// antecedent. Nearest printed card: Invert Polarity ("Choose target spell, then flip a coin. If you
// win the flip, gain control of that spell and you may choose new targets for it. If you lose the
// flip, counter that spell."); Mogg Assassin ("{T}: You choose target creature an opponent
// controls, and that opponent chooses target creature. Flip a coin. If you win the flip, destroy
// the creature you chose. If you lose the flip, destroy the creature your opponent chose.") --
// Invert Polarity's branch refers to a target declared before the coin; Mogg Assassin has two
// targets; neither has dependent multi-target instances.
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

// Real cards: none of the 86 printed coin-flip cards in the exported card data exhibits this shape:
// A lost branch whose first fight target accepts a player or an object, then a creature, then a
// target permanent sharing a card type with the first; the dependent pool follows whichever was
// elected. Nearest printed card: Goblin Bangchuckers ("{T}: Flip a coin. If you win the flip, this
// creature deals 2 damage to any target. If you lose the flip, this creature deals 2 damage to
// itself."); Crooked Scales ("{4}, {T}: Flip a coin. If you win the flip, destroy target creature
// an opponent controls. If you lose the flip, destroy target creature you control unless you pay
// {3} and repeat this process.") -- Bangchuckers has an any-target slot in a coin branch and
// Crooked Scales targets per branch; neither has a dependent shares-a-type slot.
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

// Real cards: none of the 86 printed coin-flip cards in the exported card data exhibits this shape:
// A creature target precedes a coin whose lost branch holds an optional two-target fight (an
// artifact, then a permanent sharing its type); skipping the artifact leaves the outer creature as
// antecedent. Nearest printed card: Invert Polarity ("Choose target spell, then flip a coin. If you
// win the flip, gain control of that spell and you may choose new targets for it. If you lose the
// flip, counter that spell."); Mogg Assassin ("{T}: You choose target creature an opponent
// controls, and that opponent chooses target creature. Flip a coin. If you win the flip, destroy
// the creature you chose. If you lose the flip, destroy the creature your opponent chose.") --
// Invert Polarity's branch refers to a target declared before the coin and Mogg Assassin has two
// targets; neither has optional targeting or a dependency.
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

// Real cards: none of the 86 printed coin-flip cards in the exported card data exhibits this shape:
// A root creature target, then an intermediate shares-a-card-type target, then a coin whose losing
// destroy-shares-a-card-type outcome must bind to the root, not the intermediate; also with the
// whole chain inside the losing outcome. Nearest printed card: Invert Polarity ("Choose target
// spell, then flip a coin. If you win the flip, gain control of that spell and you may choose new
// targets for it. If you lose the flip, counter that spell."); Goblin Kites ("{R}: Target creature
// you control with toughness 2 or less gains flying until end of turn. Flip a coin at the beginning
// of the next end step. If you lose the flip, sacrifice that creature.") -- Both declare a target
// before the coin and make an outcome act on it; neither has an outcome that declares its own
// dependent new target.
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

// Real cards: none of the 86 printed coin-flip cards in the exported card data exhibits this shape:
// A coin whose losing outcome is a zero-to-two shares-a-card-type destroy with no possible referent
// (zero announced slots), followed by a target-player instruction; the empty group must not take
// the player slot. Nearest printed card: Dumb Ass ("At the beginning of your upkeep, flip a coin.
// If you lose the flip, target opponent chooses whether this creature attacks this turn."); Crooked
// Scales ("{4}, {T}: Flip a coin. If you win the flip, destroy target creature an opponent
// controls. If you lose the flip, destroy target creature you control unless you pay {3} and repeat
// this process.") -- Dumb Ass's losing outcome targets a player and Crooked Scales' outcomes
// destroy targets; neither has an empty dependent group before a player target.
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

// Real cards: none of the 86 printed coin-flip cards in the exported card data exhibits this shape:
// A root creature target, then a coin whose losing outcome is a two-operand effect (two
// shares-a-type sources deal power damage, or attach two Equipment) to a creature; checks four
// announced slots, then cancels. Nearest printed card: Goblin Bangchuckers ("{T}: Flip a coin. If
// you win the flip, this creature deals 2 damage to any target. If you lose the flip, this creature
// deals 2 damage to itself."); Invert Polarity ("Choose target spell, then flip a coin. If you win
// the flip, gain control of that spell and you may choose new targets for it. If you lose the flip,
// counter that spell.") -- Goblin Bangchuckers has a damage outcome with its own target; Invert
// Polarity targets before the coin. Neither has a two-operand outcome.
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

// Real cards: none of the 86 printed coin-flip cards in the exported card data exhibits this shape:
// A coin whose losing outcome destroys a creature with mana value or power at least (or above) a
// declared target's mana value; refused with no earlier object target, castable with a prior
// creature-you-control target. Nearest printed card: Goblin Lyre ("Sacrifice this artifact: Flip a
// coin. If you win the flip, this artifact deals damage to target opponent or planeswalker equal to
// the number of creatures you control. If you lose the flip, this artifact deals damage to you
// equal to the number of creatures that opponent or that planeswalker's controller controls.");
// Crooked Scales ("{4}, {T}: Flip a coin. If you win the flip, destroy target creature an opponent
// controls. If you lose the flip, destroy target creature you control unless you pay {3} and repeat
// this process.") -- Goblin Lyre's losing outcome reads a value from the winning outcome's declared
// target; it has no target filter comparing a declared object's mana value.
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

// Real cards: none of the 86 printed coin-flip cards in the exported card data exhibits this shape:
// Root creature-you-control target, then a coin: won targets an artifact, lost destroys a permanent
// with mana value at least the root's; if the root turns illegal, the dependent destroy fails while
// the spell resolves. Nearest printed card: Goblin Lyre ("Sacrifice this artifact: Flip a coin. If
// you win the flip, this artifact deals damage to target opponent or planeswalker equal to the
// number of creatures you control. If you lose the flip, this artifact deals damage to you equal to
// the number of creatures that opponent or that planeswalker's controller controls."); Invert
// Polarity ("Choose target spell, then flip a coin. If you win the flip, gain control of that spell
// and you may choose new targets for it. If you lose the flip, counter that spell.") -- Goblin Lyre
// reads a declared target's value in an outcome; Invert Polarity's outcomes act on an earlier
// target. Neither filters a target by it.
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

// Real cards: none of the 86 printed coin-flip cards in the exported card data exhibits this shape:
// An optional root artifact target, then a coin whose optional losing outcome destroys a creature
// with mana value above the root's; declining the root leaves no dependent prompt instead of an
// unbound pool. Nearest printed card: Goblin Lyre ("Sacrifice this artifact: Flip a coin. If you
// win the flip, this artifact deals damage to target opponent or planeswalker equal to the number
// of creatures you control. If you lose the flip, this artifact deals damage to you equal to the
// number of creatures that opponent or that planeswalker's controller controls."); Invert Polarity
// ("Choose target spell, then flip a coin. If you win the flip, gain control of that spell and you
// may choose new targets for it. If you lose the flip, counter that spell.") -- Goblin Lyre reads a
// declared target's value in an outcome; Invert Polarity's outcomes act on an earlier target.
// Neither has an optional antecedent.
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

// Real cards: none of the 86 printed coin-flip cards in the exported card data exhibits this shape:
// A choose-two modal spell (invented Oracle text): tap target noncreature artifact; target creature
// gets +1/+1, then a coin whose loss destroys a permanent sharing a card type with that creature,
// not the artifact. Nearest printed card: Goblin Kites ("{R}: Target creature you control with
// toughness 2 or less gains flying until end of turn. Flip a coin at the beginning of the next end
// step. If you lose the flip, sacrifice that creature."); Orcish Captain ("{1}: Flip a coin. If you
// win the flip, target Orc creature gets +2/+0 until end of turn. If you lose the flip, it gets
// -0/-2 until end of turn.") -- Both target a creature, then flip with an outcome acting on that
// creature; neither is modal or has an outcome with its own dependent target.
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

// Synthetic composition, not a printed card: "When this creature enters, choose
// <a creature type or a card name>. Flip a coin. If you win the flip or lose the
// flip, you gain 1 life for each creature you control that reads the choice."
// Both bound outcomes read the answer published by the earlier instruction.
struct ChooseThenFlip {
    choice_type: engine::types::ability::ChoiceType,
    persist: bool,
    counted: TargetFilter,
    /// Creatures P0 controls besides the host, as (name, subtypes).
    board: Vec<(&'static str, Vec<&'static str>)>,
    answer: &'static str,
    /// Life gained when the outcome reads the answer: the number of `board`
    /// creatures that match it.
    expected_gain: i32,
}

// Real cards: none of the 86 printed coin-flip cards in the exported card data exhibits this shape:
// An enters trigger chooses a creature type or card name, then flips a coin whose both outcomes
// gain life per matching creature you control; optionally under Krark's Thumb with a persisted
// round trip. Nearest printed card: Desperate Gambit ("Choose a source you control and flip a coin.
// If you win the flip, the next time that source would deal damage this turn, it deals double that
// damage instead. If you lose the flip, the next time it would deal damage this turn, prevent that
// damage."); Risky Move ("At the beginning of each player's upkeep, that player gains control of
// this enchantment. / When you gain control of this enchantment from another player, choose a
// creature you control and an opponent. Flip a coin. If you lose the flip, that opponent gains
// control of that creature.") -- Both choose something before the flip and have outcomes act on
// that choice; neither chooses a creature type or card name.
/// Drives the host's enters trigger through the production cast and priority
/// path, answers the `NamedChoice` through `GameAction::ChooseOption`, then runs
/// the flip inline (`thumb == false`) or after Krark's Thumb with a persisted
/// round trip at the keep prompt (`thumb == true`).
fn choose_then_flip(case: &ChooseThenFlip, thumb: bool, won: bool) {
    use engine::types::ability::{
        AbilityDefinition, AbilityKind, QuantityExpr, QuantityRef, TargetSelectionMode,
        TriggerDefinition,
    };
    use engine::types::game_state::PersistedGameState;
    use engine::types::triggers::TriggerMode;

    let label = format!("thumb={thumb} won={won}");
    let branch = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::GainLife {
            amount: QuantityExpr::Ref {
                qty: QuantityRef::ObjectCount {
                    filter: case.counted.clone(),
                },
            },
            player: TargetFilter::Controller,
        },
    );
    let body = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::Choose {
            choice_type: case.choice_type.clone(),
            persist: case.persist,
            selection: TargetSelectionMode::Chosen,
        },
    )
    .sub_ability(AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::FlipCoins {
            flips: CoinFlipRun::single(),
            win_effect: Some(Box::new(branch.clone())),
            lose_effect: Some(Box::new(branch)),
            flipper: TargetFilter::Controller,
        },
    ));
    // CR 603.6a: an enters-the-battlefield ability triggers when the host enters.
    let trigger = TriggerDefinition::new(TriggerMode::ChangesZone)
        .valid_card(TargetFilter::SelfRef)
        .destination(Zone::Battlefield)
        .execute(body);

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    if thumb {
        scenario.add_artifact_from_oracle(
            P0,
            "Krark's Thumb",
            "If you would flip a coin, instead flip two coins and ignore one.",
        );
    }
    for (name, subtypes) in &case.board {
        scenario
            .add_creature(P0, name, 1, 1)
            .with_subtypes(subtypes.clone());
    }
    // The host starts in hand so that its enters trigger comes from the real
    // cast, resolution and zone-change pipeline.
    let host = scenario
        .add_creature_to_hand(P0, "Synthetic choose-then-flip host", 1, 1)
        .with_mana_cost(ManaCost::zero())
        .with_trigger_definition(trigger)
        .id();
    let mut runner = scenario.build();
    runner.state_mut().all_card_names = case
        .board
        .iter()
        .map(|(name, _)| (*name).to_string())
        .collect::<Vec<_>>()
        .into();
    pin_outcomes(
        &mut runner,
        &if thumb { vec![won, !won] } else { vec![won] },
    );
    let life_before = runner.life(P0);

    drop(runner.cast(host).commit());
    let mut events = Vec::new();
    for _ in 0..8 {
        if matches!(runner.state().waiting_for, WaitingFor::NamedChoice { .. }) {
            break;
        }
        assert!(
            matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
            "{label}: unexpected prompt {:?}",
            runner.state().waiting_for
        );
        assert!(
            !runner.state().stack.is_empty(),
            "{label}: the host spell or its enters trigger reaches the choice"
        );
        events.extend(runner.act(GameAction::PassPriority).unwrap().events);
    }
    // Required prompt: the choice comes first, before any coin is flipped.
    let WaitingFor::NamedChoice {
        player,
        choice_type,
        options,
        ..
    } = &runner.state().waiting_for
    else {
        panic!(
            "{label}: the enters trigger must pause on its choice, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(*player, P0, "{label}");
    assert_eq!(
        std::mem::discriminant(choice_type),
        std::mem::discriminant(&case.choice_type),
        "{label}"
    );
    assert!(
        options.is_empty() || options.iter().any(|option| option == case.answer),
        "{label}: {options:?}"
    );
    assert!(
        flips(&events).is_empty(),
        "{label}: no flip before the choice"
    );
    assert_eq!(runner.life(P0), life_before, "{label}");

    events.extend(
        runner
            .act(GameAction::ChooseOption {
                choice: case.answer.to_string(),
            })
            .unwrap()
            .events,
    );
    if thumb {
        assert!(
            matches!(
                runner.state().waiting_for,
                WaitingFor::CoinFlipKeepChoice { player: P0, .. }
            ),
            "{label}: required keep prompt, got {:?}",
            runner.state().waiting_for
        );
        assert!(flips(&events).is_empty(), "{label}: ignored flips pending");
        // CR 614.1a + CR 705.1: the parked frame holds the bound outcomes that
        // already carry the published answer; restore it before resuming.
        runner.state_mut().capture_rng_word_pos();
        let wire =
            serde_json::to_value(PersistedGameState::capture(runner.state().clone())).unwrap();
        *runner.state_mut() = serde_json::from_value::<PersistedGameState>(wire)
            .unwrap()
            .into_game_state()
            .unwrap();
        assert!(
            matches!(
                runner.state().waiting_for,
                WaitingFor::CoinFlipKeepChoice { player: P0, .. }
            ),
            "{label}: the restored state is still at the keep prompt"
        );
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
        vec![won],
        "{label}: reach: selected outcome ran"
    );
    // CR 608.2c + CR 608.2d: the outcome applies the answer announced by the
    // earlier instruction of this resolution. An outcome that lost it counts
    // zero matching creatures and gains nothing.
    assert_eq!(
        runner.life(P0) - life_before,
        case.expected_gain,
        "{label}: the outcome reads the published choice"
    );
    assert_finished(&runner);
}

// Real cards: none of the 86 printed coin-flip cards in the exported card data exhibits this shape:
// Through choose_then_flip: a persisted creature-type choice made before the flip is read by both
// coin outcomes, which count creatures you control of the chosen type, with and without Krark's
// Thumb. Nearest printed card: Desperate Gambit ("Choose a source you control and flip a coin. If
// you win the flip, the next time that source would deal damage this turn, it deals double that
// damage instead. If you lose the flip, the next time it would deal damage this turn, prevent that
// damage."); Risky Move ("At the beginning of each player's upkeep, that player gains control of
// this enchantment. / When you gain control of this enchantment from another player, choose a
// creature you control and an opponent. Flip a coin. If you lose the flip, that opponent gains
// control of that creature.") -- Both choose something before the flip and have outcomes act on
// that choice; neither chooses a creature type or card name.
#[test]
fn choose_then_flip_outcome_reads_the_published_choice() {
    use engine::types::ability::{ChoiceType, ControllerRef, FilterProp, TypedFilter};
    // Synthetic: a non-card-name choice (CR 608.2c) published before the flip.
    let case = ChooseThenFlip {
        choice_type: ChoiceType::CreatureType {
            options: vec!["Dog".into(), "Wolf".into()],
        },
        persist: true,
        counted: TargetFilter::Typed(
            TypedFilter::creature()
                .controller(ControllerRef::You)
                .properties(vec![FilterProp::IsChosenCreatureType]),
        ),
        board: vec![
            ("Chosen type A", vec!["Wolf"]),
            ("Chosen type B", vec!["Wolf"]),
            ("Distractor", vec!["Dog"]),
        ],
        answer: "Wolf",
        expected_gain: 2,
    };
    for thumb in [false, true] {
        for won in [true, false] {
            choose_then_flip(&case, thumb, won);
        }
    }
}

// Real cards: none of the 86 printed coin-flip cards in the exported card data exhibits this shape:
// Through choose_then_flip: a non-persisted card-name choice answered through the continuation is
// read by both coin outcomes, which count creatures you control with that name, with and without
// Krark's Thumb. Nearest printed card: Desperate Gambit ("Choose a source you control and flip a
// coin. If you win the flip, the next time that source would deal damage this turn, it deals double
// that damage instead. If you lose the flip, the next time it would deal damage this turn, prevent
// that damage."); Risky Move ("At the beginning of each player's upkeep, that player gains control
// of this enchantment. / When you gain control of this enchantment from another player, choose a
// creature you control and an opponent. Flip a coin. If you lose the flip, that opponent gains
// control of that creature.") -- Both choose something before the flip and have outcomes act on
// that choice; neither chooses a creature type or card name.
#[test]
fn choose_then_flip_outcome_reads_the_named_card() {
    use engine::types::ability::{ChoiceType, ControllerRef, TypedFilter};
    // Synthetic: a card name answered through the interactive continuation
    // (CR 608.2d) reaches the bound coin outcome that reads it (CR 608.2c).
    let case = ChooseThenFlip {
        choice_type: ChoiceType::CardName,
        persist: false,
        counted: TargetFilter::And {
            filters: vec![
                TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::You)),
                TargetFilter::HasChosenName,
            ],
        },
        board: vec![
            ("Named Wolf", vec![]),
            ("Named Wolf", vec![]),
            ("Other Wolf", vec![]),
        ],
        answer: "Named Wolf",
        expected_gain: 2,
    };
    for thumb in [false, true] {
        for won in [true, false] {
            choose_then_flip(&case, thumb, won);
        }
    }
}

/// Answers every target prompt in order. Each submission goes through the
/// production `GameAction::ChooseTarget` path and must be accepted.
fn answer_prompts(runner: &mut GameRunner, label: &str, answers: &[ObjectId]) {
    let mut remaining = answers.iter().copied();
    while matches!(
        runner.state().waiting_for,
        WaitingFor::TargetSelection { .. }
    ) {
        let target = remaining
            .next()
            .unwrap_or_else(|| panic!("{label}: more target prompts than expected"));
        runner
            .act(GameAction::ChooseTarget {
                target: Some(TargetRef::Object(target)),
            })
            .unwrap_or_else(|err| {
                panic!("{label}: target submission {target:?} rejected: {err:?}")
            });
    }
    assert!(
        remaining.next().is_none(),
        "{label}: targeting ended before every declaration was answered"
    );
}

/// Casts the synthetic spell through the production entry point; the caller
/// answers the resulting target prompts with `answer_prompts`.
fn cast_for_prompts(runner: &mut GameRunner, spell: ObjectId) {
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("required announcement");
}

// Real cards: none of the 86 printed coin-flip cards in the exported card data exhibits this shape:
// A root exactly-one creature-you-control target (explicit multi-target spec), then a coin whose
// losing outcome destroys a permanent sharing a card type with that creature; optionally with
// Krark's Thumb. Nearest printed card: Goblin Kites ("{R}: Target creature you control with
// toughness 2 or less gains flying until end of turn. Flip a coin at the beginning of the next end
// step. If you lose the flip, sacrifice that creature."); Invert Polarity ("Choose target spell,
// then flip a coin. If you win the flip, gain control of that spell and you may choose new targets
// for it. If you lose the flip, counter that spell.") -- Both declare a target before the coin and
// make an outcome act on it; neither has an outcome that declares its own dependent new target.
/// Synthetic fixture (no real card): an exact-one ancestor declaration whose
/// continuation is a coin whose losing outcome depends on the ancestor's target.
/// Object ids: ancestor 1, victim 2, distractor 3, spell 4, Krark's Thumb 5.
fn multi_target_ancestor_runner(
    thumb: bool,
) -> (GameRunner, ObjectId, ObjectId, ObjectId, ObjectId) {
    use engine::types::ability::{
        AbilityDefinition, AbilityKind, ControllerRef, MultiTargetSpec, TypedFilter,
    };
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
            win_effect: None,
            lose_effect: Some(Box::new(shared_type_destroy())),
            flipper: TargetFilter::Controller,
        },
    ));
    definition.multi_target = Some(MultiTargetSpec::fixed(1, 1));
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let ancestor = scenario.add_creature(P0, "Creature antecedent", 2, 2).id();
    let victim = scenario.add_creature(P1, "Dependent creature", 4, 4).id();
    let distractor = scenario
        .add_artifact_from_oracle(P0, "Artifact distractor", "")
        .id();
    let spell = scenario
        .add_spell_to_hand(P0, "Synthetic multi-target ancestor coin", true)
        .with_ability_definition(definition)
        .id();
    if thumb {
        scenario.add_artifact_from_oracle(
            P0,
            "Krark's Thumb",
            "If you would flip a coin, instead flip two coins and ignore one.",
        );
    }
    (scenario.build(), ancestor, victim, distractor, spell)
}

// Real cards: none of the 86 printed coin-flip cards in the exported card data exhibits this shape:
// Through multi_target_ancestor_runner: the losing outcome's dependent target announced with the
// exactly-one root stays bound in the coin frame, including across a Krark's Thumb keep prompt and
// wire round trip. Nearest printed card: Goblin Kites ("{R}: Target creature you control with
// toughness 2 or less gains flying until end of turn. Flip a coin at the beginning of the next end
// step. If you lose the flip, sacrifice that creature."); Invert Polarity ("Choose target spell,
// then flip a coin. If you win the flip, gain control of that spell and you may choose new targets
// for it. If you lose the flip, counter that spell.") -- Both declare a target before the coin and
// make an outcome act on it; neither has an outcome that declares its own dependent new target.
#[test]
fn multi_target_ancestor_keeps_its_dependent_coin_outcome_target() {
    use engine::types::resolution::ResolutionStateWire;

    // Synthetic. CR 601.2c: the ancestor's variable-count declaration is fixed
    // at announcement, and CR 115.1 declares the coin outcome's target in the
    // same announcement. Assignment must reproduce the announced slot groups,
    // so the dependent losing outcome keeps its ancestor referent (CR 608.2c).
    let assert_bound_victim = |runner: &GameRunner, victim: ObjectId, label: &str| {
        let frame = runner
            .state()
            .active_coin_flip_frame()
            .unwrap_or_else(|| panic!("{label}: coin keep frame"));
        assert_eq!(
            frame.lose_effect.as_deref().unwrap().targets,
            vec![TargetRef::Object(victim)],
            "{label}: the dependent target was announced and bound"
        );
    };
    for thumb in [false, true] {
        for won in [false, true] {
            let label = format!(
                "{}/{}",
                if won { "win" } else { "lose" },
                if thumb { "thumb" } else { "plain" }
            );
            let (mut runner, ancestor, victim, distractor, spell) =
                multi_target_ancestor_runner(thumb);
            if thumb {
                pin_outcomes(&mut runner, &[true, false]);
            } else {
                pin_outcomes(&mut runner, &[won]);
            }
            cast_for_prompts(&mut runner, spell);
            answer_prompts(&mut runner, &label, &[ancestor, victim]);
            let mut events = Vec::new();
            if thumb {
                pass_to_coin_keep(&mut runner, &mut events);
                assert!(
                    flips(&events).is_empty(),
                    "{label}: ignored flips have not happened"
                );
                assert_bound_victim(&runner, victim, &label);
                // Wire round trip: restore only, no malformed leg.
                runner.state_mut().capture_rng_word_pos();
                let wire = serde_json::to_value(ResolutionStateWire::from_game_state(
                    runner.state().clone(),
                ))
                .unwrap();
                *runner.state_mut() = serde_json::from_value::<ResolutionStateWire>(wire)
                    .unwrap()
                    .into_game_state();
                assert_bound_victim(&runner, victim, &label);
                events.extend(
                    runner
                        .act(GameAction::SelectCoinFlips {
                            keep_indices: vec![usize::from(!won)],
                        })
                        .unwrap()
                        .events,
                );
            }
            finish(&mut runner, &mut events);
            assert_eq!(flips(&events), vec![won], "{label}");
            // CR 705.2 + CR 608.2c: only the losing outcome has an effect, so a
            // win leaves its announced (unused) target alone.
            assert_eq!(
                runner.state().objects[&victim].zone,
                if won {
                    Zone::Battlefield
                } else {
                    Zone::Graveyard
                },
                "{label}: dependent outcome target"
            );
            assert_eq!(
                runner.state().objects[&ancestor].zone,
                Zone::Battlefield,
                "{label}: ancestor"
            );
            assert_eq!(
                runner.state().objects[&distractor].zone,
                Zone::Battlefield,
                "{label}: distractor"
            );
            assert_finished(&runner);
        }
    }
}

// Real cards: none of the 86 printed coin-flip cards in the exported card data exhibits this shape:
// Root creature-you-control target, a middle up-to-one opponent's-artifact target with no
// candidates (zero slots), then a coin whose losing shares-a-card-type destroy must still see the
// root. Nearest printed card: Goblin Kites ("{R}: Target creature you control with toughness 2 or
// less gains flying until end of turn. Flip a coin at the beginning of the next end step. If you
// lose the flip, sacrifice that creature."); Invert Polarity ("Choose target spell, then flip a
// coin. If you win the flip, gain control of that spell and you may choose new targets for it. If
// you lose the flip, counter that spell.") -- Both declare a target before the coin and make an
// outcome act on it; neither has an outcome that declares its own dependent new target.
#[test]
fn empty_multi_target_node_between_ancestor_and_dependent_coin_outcome() {
    use engine::types::ability::{
        AbilityDefinition, AbilityKind, ControllerRef, MultiTargetSpec, TypeFilter, TypedFilter,
    };
    // Synthetic. The middle "up to one target artifact an opponent controls"
    // node announces zero slots (CR 601.2c: its count is fixed at announcement)
    // because the opponent controls no artifact, yet the ancestor above it must
    // stay visible to the coin outcome below it (CR 608.2c).
    let mut middle = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::TargetOnly {
            target: TargetFilter::Typed(
                TypedFilter::new(TypeFilter::Artifact).controller(ControllerRef::Opponent),
            ),
        },
    )
    .sub_ability(AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::FlipCoins {
            flips: CoinFlipRun::single(),
            win_effect: None,
            lose_effect: Some(Box::new(shared_type_destroy())),
            flipper: TargetFilter::Controller,
        },
    ));
    middle.multi_target = Some(MultiTargetSpec::fixed(0, 1));
    let definition = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::TargetOnly {
            target: TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::You)),
        },
    )
    .sub_ability(middle);

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let ancestor = scenario.add_creature(P0, "Creature antecedent", 2, 2).id();
    let victim = scenario.add_creature(P1, "Dependent creature", 4, 4).id();
    let distractor = scenario
        .add_artifact_from_oracle(P0, "Artifact distractor", "")
        .id();
    let spell = scenario
        .add_spell_to_hand(P0, "Synthetic empty middle declaration", true)
        .with_ability_definition(definition)
        .id();
    let mut runner = scenario.build();
    pin_outcomes(&mut runner, &[false]);
    cast_for_prompts(&mut runner, spell);
    // Exactly two prompts: the ancestor and the dependent coin outcome.
    answer_prompts(&mut runner, "lose", &[ancestor, victim]);
    let mut events = Vec::new();
    finish(&mut runner, &mut events);
    assert_eq!(flips(&events), vec![false]);
    // CR 705.2 + CR 608.2c: the losing outcome destroys its own selected object.
    assert_eq!(runner.state().objects[&victim].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&ancestor].zone, Zone::Battlefield);
    assert_eq!(runner.state().objects[&distractor].zone, Zone::Battlefield);
    assert_finished(&runner);
}

// Real cards: none of the 86 printed coin-flip cards in the exported card data exhibits this shape:
// A coin whose won outcome destroys up to two target artifacts (one exists) and lost destroys
// target creature; slot-by-slot, the won group must end at its one slot, not take lost's. Nearest
// printed card: Crooked Scales ("{4}, {T}: Flip a coin. If you win the flip, destroy target
// creature an opponent controls. If you lose the flip, destroy target creature you control unless
// you pay {3} and repeat this process."); Goblin Archaeologist ("{R}, {T}: Flip a coin. If you win
// the flip, destroy target artifact and untap this creature. If you lose the flip, sacrifice this
// creature.") -- Crooked Scales and Goblin Archaeologist give a coin outcome its own declared
// destroy target; neither has an up-to-two outcome group.
#[test]
fn coin_outcome_multi_target_group_keeps_to_its_own_slots() {
    use engine::types::ability::{
        AbilityDefinition, AbilityKind, MultiTargetSpec, TypeFilter, TypedFilter,
    };
    // Synthetic. Won = "destroy up to two target artifacts" with a single
    // artifact on the battlefield, so the announcement gives Won one slot and
    // Lost one slot (CR 601.2c: counts fixed at announcement; CR 115.1: both
    // outcomes' targets are declared up front). Won's group must end at its own
    // announced boundary instead of swallowing Lost's slot.
    for won in [false, true] {
        let label = if won { "win" } else { "lose" };
        let mut won_effect = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Destroy {
                target: TargetFilter::Typed(TypedFilter::new(TypeFilter::Artifact)),
                cant_regenerate: false,
            },
        );
        won_effect.multi_target = Some(MultiTargetSpec::fixed(0, 2));
        let lost_effect = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Destroy {
                target: TargetFilter::Typed(TypedFilter::creature()),
                cant_regenerate: false,
            },
        );
        let definition = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::FlipCoins {
                flips: CoinFlipRun::single(),
                win_effect: Some(Box::new(won_effect)),
                lose_effect: Some(Box::new(lost_effect)),
                flipper: TargetFilter::Controller,
            },
        );

        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let artifact = scenario
            .add_artifact_from_oracle(P1, "Artifact target", "")
            .id();
        let victim = scenario
            .add_creature(P1, "Losing outcome target", 4, 4)
            .id();
        let distractor = scenario.add_creature(P0, "Never selected", 2, 2).id();
        let spell = scenario
            .add_spell_to_hand(P0, "Synthetic coin outcome groups", true)
            .with_ability_definition(definition)
            .id();
        let mut runner = scenario.build();
        pin_outcomes(&mut runner, &[won]);
        cast_for_prompts(&mut runner, spell);
        answer_prompts(&mut runner, label, &[artifact, victim]);
        let mut events = Vec::new();
        finish(&mut runner, &mut events);
        assert_eq!(flips(&events), vec![won], "{label}");
        // CR 705.2 + CR 608.2c: each outcome affects only its own selected object.
        let (destroyed, kept) = if won {
            (artifact, victim)
        } else {
            (victim, artifact)
        };
        assert_eq!(
            runner.state().objects[&destroyed].zone,
            Zone::Graveyard,
            "{label}: the executed outcome's object"
        );
        assert_eq!(
            runner.state().objects[&kept].zone,
            Zone::Battlefield,
            "{label}: the other outcome's object"
        );
        assert_eq!(
            runner.state().objects[&distractor].zone,
            Zone::Battlefield,
            "{label}: distractor"
        );
        assert_finished(&runner);
    }
}

#[derive(Clone, Copy, Debug)]
enum BulkEntry {
    Spell,
    Trigger,
}

#[derive(Clone, Copy, Debug)]
enum LostShape {
    Required,
    UpToOne,
}

// Real cards: none of the 86 printed coin-flip cards in the exported card data exhibits this shape:
// A coin, as a spell or an enters trigger, whose won outcome destroys up to two artifacts and lost
// destroys a required or up-to-one creature; one bulk SelectTargets list must split into each
// outcome's group. Nearest printed card: Crooked Scales ("{4}, {T}: Flip a coin. If you win the
// flip, destroy target creature an opponent controls. If you lose the flip, destroy target creature
// you control unless you pay {3} and repeat this process."); Goblin Archaeologist ("{R}, {T}: Flip
// a coin. If you win the flip, destroy target artifact and untap this creature. If you lose the
// flip, sacrifice this creature.") -- Crooked Scales and Goblin Archaeologist give a coin outcome
// its own declared destroy target; neither has an up-to-two outcome group.
/// Synthetic. One coin chain whose Won outcome is a multi-target node. The
/// bulk `GameAction::SelectTargets` declaration is driven through the
/// production entry for `entry`, and the losing outcome is either required or
/// "up to one" so that a lost flip may find its slot optional.
fn bulk_coin_groups(entry: BulkEntry, lost: LostShape, won: bool) {
    use engine::types::ability::TriggerDefinition;
    use engine::types::ability::{
        AbilityDefinition, AbilityKind, MultiTargetSpec, TypeFilter, TypedFilter,
    };
    use engine::types::triggers::TriggerMode;

    let label = format!("{entry:?}/{lost:?}/{}", if won { "win" } else { "lose" });

    let mut won_effect = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::Destroy {
            target: TargetFilter::Typed(TypedFilter::new(TypeFilter::Artifact)),
            cant_regenerate: false,
        },
    );
    won_effect.multi_target = Some(MultiTargetSpec::fixed(0, 2));

    let mut lost_effect = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::Destroy {
            target: TargetFilter::Typed(TypedFilter::creature()),
            cant_regenerate: false,
        },
    );
    if matches!(lost, LostShape::UpToOne) {
        lost_effect.multi_target = Some(MultiTargetSpec::fixed(0, 1));
    }

    let body = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::FlipCoins {
            flips: CoinFlipRun::single(),
            win_effect: Some(Box::new(won_effect)),
            lose_effect: Some(Box::new(lost_effect)),
            flipper: TargetFilter::Controller,
        },
    );

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let artifact = scenario
        .add_artifact_from_oracle(P1, "Artifact target", "")
        .id();
    let victim = scenario
        .add_creature(P1, "Losing outcome target", 4, 4)
        .id();
    let distractor = scenario.add_creature(P0, "Never selected", 2, 2).id();
    let source = match entry {
        BulkEntry::Spell => scenario
            .add_spell_to_hand(P0, "Synthetic bulk coin groups", true)
            .with_ability_definition(body)
            .id(),
        BulkEntry::Trigger => {
            let trigger = TriggerDefinition::new(TriggerMode::ChangesZone)
                .valid_card(TargetFilter::SelfRef)
                .destination(Zone::Battlefield)
                .execute(body);
            scenario
                .add_creature_to_hand(P0, "Synthetic bulk coin host", 1, 1)
                .with_mana_cost(ManaCost::zero())
                .with_trigger_definition(trigger)
                .id()
        }
    };
    let mut runner = scenario.build();
    pin_outcomes(&mut runner, &[won]);

    let mut events = Vec::new();
    match entry {
        BulkEntry::Spell => {
            cast_for_prompts(&mut runner, source);
            assert!(
                matches!(
                    runner.state().waiting_for,
                    WaitingFor::TargetSelection { .. }
                ),
                "{label}: the spell must pause on its target declaration"
            );
        }
        BulkEntry::Trigger => {
            drop(runner.cast(source).commit());
            let mut reached = false;
            for _ in 0..8 {
                let prompt = &runner.state().waiting_for;
                if matches!(prompt, WaitingFor::TriggerTargetSelection { .. }) {
                    reached = true;
                    break;
                }
                assert!(
                    matches!(prompt, WaitingFor::Priority { .. }),
                    "{label}: unexpected prompt {:?}",
                    prompt
                );
                events.extend(runner.act(GameAction::PassPriority).unwrap().events);
            }
            assert!(
                reached,
                "{label}: the enters trigger must pause on its targets"
            );
        }
    }

    let targets = vec![TargetRef::Object(artifact), TargetRef::Object(victim)];
    events.extend(
        runner
            .act(GameAction::SelectTargets {
                targets: targets.clone(),
            })
            .unwrap_or_else(|err| panic!("{label}: bulk declaration {targets:?} rejected: {err:?}"))
            .events,
    );
    {
        // CR 601.2c + CR 115.1: the announcement fixes each outcome's own
        // target group; the bulk list is only a positional prefix of it.
        let ability = runner.state().stack.back().unwrap().ability().unwrap();
        let branches = ability.coin_branches.as_ref().unwrap();
        assert_eq!(
            branches.won.as_ref().unwrap().targets,
            vec![TargetRef::Object(artifact)],
            "{label}: Won keeps to its own announced group"
        );
        assert_eq!(
            branches.lost.as_ref().unwrap().targets,
            vec![TargetRef::Object(victim)],
            "{label}: Lost keeps its own announced target"
        );
    }
    finish(&mut runner, &mut events);
    assert_eq!(flips(&events), vec![won], "{label}");
    // CR 705.2 + CR 608.2c: the winning and losing instructions are followed in
    // order, so each outcome affects only the object it announced.
    let (destroyed, kept) = if won {
        (artifact, victim)
    } else {
        (victim, artifact)
    };
    assert_eq!(
        runner.state().objects[&destroyed].zone,
        Zone::Graveyard,
        "{label}: the executed outcome's object"
    );
    assert_eq!(
        runner.state().objects[&kept].zone,
        Zone::Battlefield,
        "{label}: the other outcome's object"
    );
    assert_eq!(
        runner.state().objects[&distractor].zone,
        Zone::Battlefield,
        "{label}: distractor"
    );
    if matches!(entry, BulkEntry::Trigger) {
        assert_eq!(
            runner.state().objects[&source].zone,
            Zone::Battlefield,
            "{label}: the trigger host stays on the battlefield"
        );
    }
    assert_finished(&runner);
}

// Real cards: none of the 86 printed coin-flip cards in the exported card data exhibits this shape:
// Through bulk_coin_groups, spell entry, required lost: a bulk list for won up-to-two artifacts
// plus lost creature keeps each target in its own outcome group. Nearest printed card: Crooked
// Scales ("{4}, {T}: Flip a coin. If you win the flip, destroy target creature an opponent
// controls. If you lose the flip, destroy target creature you control unless you pay {3} and repeat
// this process."); Goblin Archaeologist ("{R}, {T}: Flip a coin. If you win the flip, destroy
// target artifact and untap this creature. If you lose the flip, sacrifice this creature.") --
// Crooked Scales and Goblin Archaeologist give a coin outcome its own declared destroy target;
// neither has an up-to-two outcome group.
#[test]
fn bulk_spell_declaration_keeps_coin_outcome_groups_to_their_own_slots() {
    // Synthetic. CR 601.2c + CR 115.1: a spell's bulk declaration is a
    // positional prefix of the announced slot vector, so a Won multi-target
    // node must end at its own group instead of swallowing Lost's target.
    for won in [false, true] {
        bulk_coin_groups(BulkEntry::Spell, LostShape::Required, won);
    }
}

// Real cards: none of the 86 printed coin-flip cards in the exported card data exhibits this shape:
// Through bulk_coin_groups, enters-trigger entry, required lost: the trigger's bulk declaration
// reproduces the same won/lost group partition as the spell entry. Nearest printed card: Crooked
// Scales ("{4}, {T}: Flip a coin. If you win the flip, destroy target creature an opponent
// controls. If you lose the flip, destroy target creature you control unless you pay {3} and repeat
// this process."); Goblin Archaeologist ("{R}, {T}: Flip a coin. If you win the flip, destroy
// target artifact and untap this creature. If you lose the flip, sacrifice this creature.") --
// Crooked Scales and Goblin Archaeologist give a coin outcome its own declared destroy target;
// neither has an up-to-two outcome group.
#[test]
fn bulk_trigger_declaration_keeps_coin_outcome_groups_to_their_own_slots() {
    // Synthetic. CR 601.2c + CR 115.1: the enters-trigger entry must reproduce
    // the same announced partition as the spell entry.
    for won in [false, true] {
        bulk_coin_groups(BulkEntry::Trigger, LostShape::Required, won);
    }
}

// Real cards: none of the 86 printed coin-flip cards in the exported card data exhibits this shape:
// Through bulk_coin_groups, spell entry, up-to-one lost: the won up-to-two artifact group must not
// claim lost's optional slot. Nearest printed card: Crooked Scales ("{4}, {T}: Flip a coin. If you
// win the flip, destroy target creature an opponent controls. If you lose the flip, destroy target
// creature you control unless you pay {3} and repeat this process."); Goblin Archaeologist ("{R},
// {T}: Flip a coin. If you win the flip, destroy target artifact and untap this creature. If you
// lose the flip, sacrifice this creature.") -- Crooked Scales and Goblin Archaeologist give a coin
// outcome its own declared destroy target; neither has an up-to-two outcome group.
#[test]
fn bulk_declaration_keeps_an_optional_outcome_target_in_its_own_group() {
    // Synthetic. CR 601.2c: "up to one" Lost announces one optional slot; the
    // Won node must not claim it, or a lost flip silently destroys the Won
    // target's object (CR 705.2 + CR 608.2c).
    for won in [false, true] {
        bulk_coin_groups(BulkEntry::Spell, LostShape::UpToOne, won);
    }
}

/// Answers every target prompt in order, allowing an explicit decline (`None`)
/// for an optional slot. Mirrors `answer_prompts`, whose entries are always
/// targets; the decline is the `ChooseTarget { target: None }` control.
fn answer_prompt_choices(runner: &mut GameRunner, label: &str, answers: &[Option<ObjectId>]) {
    let mut remaining = answers.iter().copied();
    while matches!(
        runner.state().waiting_for,
        WaitingFor::TargetSelection { .. }
    ) {
        let choice = remaining
            .next()
            .unwrap_or_else(|| panic!("{label}: more target prompts than expected"));
        let target = choice.map(TargetRef::Object);
        runner
            .act(GameAction::ChooseTarget { target })
            .unwrap_or_else(|err| {
                panic!("{label}: target submission {choice:?} rejected: {err:?}")
            });
    }
    assert!(
        remaining.next().is_none(),
        "{label}: targeting ended before every declaration was answered"
    );
}

// Real cards: none of the 86 printed coin-flip cards in the exported card data exhibits this shape:
// A root up-to-two creature group, then a coin whose losing outcome destroys its own target
// creature; bulk lists are positional, so mid-list or whole-group declines are refused, while
// per-slot declining works. Nearest printed card: Mogg Assassin ("{T}: You choose target creature
// an opponent controls, and that opponent chooses target creature. Flip a coin. If you win the
// flip, destroy the creature you chose. If you lose the flip, destroy the creature your opponent
// chose."); Crooked Scales ("{4}, {T}: Flip a coin. If you win the flip, destroy target creature an
// opponent controls. If you lose the flip, destroy target creature you control unless you pay {3}
// and repeat this process.") -- Mogg Assassin declares two creature targets before a coin whose
// outcomes destroy them; it has no variable-count group or outcome-own target.
/// Synthetic. CR 601.2c + CR 115.1: the announcement fixes the ancestor's
/// variable-count group and the coin outcome's own target. On a coin chain with
/// a multi-target node the bulk list is a positional prefix of the announced
/// slot vector, so a list that declines a slot mid-list cannot be re-packed.
/// Declining the whole group in bulk is refused for the same reason, and the
/// same choice stays expressible one slot at a time (CR 608.2c).
#[test]
fn bulk_declaration_reads_an_ancestor_multi_target_group_positionally() {
    use engine::types::ability::{AbilityDefinition, AbilityKind, MultiTargetSpec, TypedFilter};

    // Real cards: none of the 86 printed coin-flip cards in the exported card data exhibits this
    // shape: AST ability for the positional test: up-to-two target creatures (TargetOnly), then a
    // coin whose losing outcome destroys target creature. Nearest printed card: Mogg Assassin
    // ("{T}: You choose target creature an opponent controls, and that opponent chooses target
    // creature. Flip a coin. If you win the flip, destroy the creature you chose. If you lose the
    // flip, destroy the creature your opponent chose."); Crooked Scales ("{4}, {T}: Flip a coin. If
    // you win the flip, destroy target creature an opponent controls. If you lose the flip, destroy
    // target creature you control unless you pay {3} and repeat this process.") -- Mogg Assassin
    // declares two creature targets before a coin whose outcomes destroy them; it has no
    // variable-count group or outcome-own target.
    fn definition() -> AbilityDefinition {
        let mut definition = AbilityDefinition::new(
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
                lose_effect: Some(Box::new(AbilityDefinition::new(
                    AbilityKind::Spell,
                    Effect::Destroy {
                        target: TargetFilter::Typed(TypedFilter::creature()),
                        cant_regenerate: false,
                    },
                ))),
                flipper: TargetFilter::Controller,
            },
        ));
        definition.multi_target = Some(MultiTargetSpec::fixed(0, 2));
        definition
    }

    // Real cards: none of the 86 printed coin-flip cards in the exported card data exhibits this
    // shape: Builds the positional test board: the synthetic spell from definition() plus three
    // textless filler creatures (two group candidates and the losing-outcome target). Nearest
    // printed card: Mogg Assassin ("{T}: You choose target creature an opponent controls, and that
    // opponent chooses target creature. Flip a coin. If you win the flip, destroy the creature you
    // chose. If you lose the flip, destroy the creature your opponent chose."); Crooked Scales
    // ("{4}, {T}: Flip a coin. If you win the flip, destroy target creature an opponent controls.
    // If you lose the flip, destroy target creature you control unless you pay {3} and repeat this
    // process.") -- Mogg Assassin declares two creature targets before a coin whose outcomes
    // destroy them; it has no variable-count group or outcome-own target.
    /// Object ids: c1 1, c2 2, v 3, spell 4.
    fn scenario() -> (GameRunner, ObjectId, ObjectId, ObjectId, ObjectId) {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let c1 = scenario.add_creature(P0, "Own first", 2, 2).id();
        let c2 = scenario.add_creature(P1, "Opposing second", 3, 3).id();
        let v = scenario
            .add_creature(P1, "Losing outcome target", 4, 4)
            .id();
        let spell = scenario
            .add_spell_to_hand(P0, "Synthetic positional ancestor coin", true)
            .with_ability_definition(definition())
            .id();
        (scenario.build(), c1, c2, v, spell)
    }

    for won in [false, true] {
        let label = if won { "win" } else { "lose" };
        let (mut runner, c1, c2, v, spell) = scenario();
        pin_outcomes(&mut runner, &[won]);
        cast_for_prompts(&mut runner, spell);

        // 1. Positional refusal: slot 1 is the ancestor's second slot, so the
        // Lost outcome's required slot is left undeclared.
        match runner.act(GameAction::SelectTargets {
            targets: vec![TargetRef::Object(c1), TargetRef::Object(v)],
        }) {
            Ok(_) => panic!(
                "{label}: positional [c1, v] must be refused because Lost's required slot is undeclared"
            ),
            Err(err) => {
                assert_eq!(
                    format!("{err:?}"),
                    r#"InvalidAction("Missing required target")"#,
                    "{label}"
                );
                assert!(
                    matches!(
                        runner.state().waiting_for,
                        WaitingFor::TargetSelection { .. }
                    ),
                    "{label}: the refusal leaves the declaration open"
                );
            }
        }

        // 1b. Whole-group decline: `[v]` positions v in the ancestor's first
        // slot, so Lost again has no declaration.
        match runner.act(GameAction::SelectTargets {
            targets: vec![TargetRef::Object(v)],
        }) {
            Ok(_) => panic!(
                "{label}: positional [v] must be refused because Lost's required slot is undeclared"
            ),
            Err(err) => {
                assert_eq!(
                    format!("{err:?}"),
                    r#"InvalidAction("Missing required target")"#,
                    "{label}"
                );
                assert!(
                    matches!(
                        runner.state().waiting_for,
                        WaitingFor::TargetSelection { .. }
                    ),
                    "{label}: the refusal leaves the declaration open"
                );
            }
        }

        // 2. A full positional list reproduces the announced partition.
        let targets = vec![
            TargetRef::Object(c1),
            TargetRef::Object(c2),
            TargetRef::Object(v),
        ];
        let mut events = runner
            .act(GameAction::SelectTargets {
                targets: targets.clone(),
            })
            .unwrap_or_else(|err| panic!("{label}: bulk declaration {targets:?} rejected: {err:?}"))
            .events;
        {
            // CR 601.2c + CR 115.1: each target stays in the group it was
            // announced and validated for.
            let ability = runner.state().stack.back().unwrap().ability().unwrap();
            assert_eq!(
                ability.targets,
                vec![TargetRef::Object(c1), TargetRef::Object(c2)],
                "{label}: the ancestor holds its two announced slots"
            );
            assert_eq!(
                ability
                    .sub_ability
                    .as_ref()
                    .unwrap()
                    .coin_branches
                    .as_ref()
                    .unwrap()
                    .lost
                    .as_ref()
                    .unwrap()
                    .targets,
                vec![TargetRef::Object(v)],
                "{label}: Lost keeps its own slot"
            );
        }
        finish(&mut runner, &mut events);
        assert_eq!(flips(&events), vec![won], "{label}");
        // CR 705.2 + CR 608.2c: only the losing outcome destroys its target.
        assert_eq!(
            runner.state().objects[&v].zone,
            if won {
                Zone::Battlefield
            } else {
                Zone::Graveyard
            },
            "{label}: the losing outcome's target"
        );
        for kept in [c1, c2] {
            assert_eq!(
                runner.state().objects[&kept].zone,
                Zone::Battlefield,
                "{label}: the ancestor keeps its targets"
            );
        }
        assert_finished(&runner);
    }

    // 3. Positive control: the same declaration one slot at a time. The single
    // `None` declines the rest of the ancestor group, so there are exactly
    // three prompts: two ancestor slots and Lost.
    let (mut runner, c1, _c2, v, spell) = scenario();
    pin_outcomes(&mut runner, &[false]);
    cast_for_prompts(&mut runner, spell);
    answer_prompt_choices(&mut runner, "lose", &[Some(c1), None, Some(v)]);
    {
        let ability = runner.state().stack.back().unwrap().ability().unwrap();
        assert_eq!(
            ability.targets,
            vec![TargetRef::Object(c1)],
            "lose: the decline ends the ancestor group"
        );
        assert_eq!(
            ability
                .sub_ability
                .as_ref()
                .unwrap()
                .coin_branches
                .as_ref()
                .unwrap()
                .lost
                .as_ref()
                .unwrap()
                .targets,
            vec![TargetRef::Object(v)],
            "lose: Lost keeps its own slot through ChooseTarget"
        );
    }
    let mut events = Vec::new();
    finish(&mut runner, &mut events);
    assert_eq!(flips(&events), vec![false]);
    assert_eq!(runner.state().objects[&v].zone, Zone::Graveyard);
    assert_finished(&runner);
}

// Real cards: Kopala, Warden of Waves is printed and used with its full Oracle text. The coin chain
// itself is synthetic: none of the 86 printed coin-flip cards in the exported card data declares a
// variable-count target group before a coin with an outcome-owned target. Nearest printed card:
// Mogg Assassin ("{T}: You choose target creature an opponent controls, and that opponent chooses
// target creature. Flip a coin. If you win the flip, destroy the creature you chose. If you lose
// the flip, destroy the creature your opponent chose.") declares two creature targets before a coin
// whose outcomes act on them; it has no variable-count group.
/// Synthetic coin chain (Kopala is printed). CR 601.2c + CR 115.1: the activation
/// offer's cost search must price the same announced groups the declaration does.
/// Kopala's {2} tax applies only when the ability targets a Merfolk, so every
/// affordable completion of this coin chain declines at least one ancestor slot
/// before the losing outcome's slot (the one this test plays declines the whole
/// group) and targets the untaxed creature with the losing outcome (CR 705.2 +
/// CR 608.2c).
#[test]
fn activation_offer_prices_a_declined_ancestor_group_in_a_coin_chain() {
    use engine::types::ability::{
        AbilityCost, AbilityDefinition, AbilityKind, MultiTargetSpec, TypedFilter,
    };

    // Kopala, Warden of Waves: the printed Oracle text, verbatim.
    const KOPALA: &str =
        "Spells your opponents cast that target a Merfolk you control cost {2} more to cast.\nAbilities your opponents activate that target a Merfolk you control cost {2} more to activate.";

    // Real cards: none of the 86 printed coin-flip cards in the exported card data exhibits this
    // shape: Builds the activation board: Kopala (tax on abilities targeting a Merfolk), an untaxed
    // filler creature, and the synthetic artifact with the {1} up-to-two-then-coin activated
    // ability, plus one mana. Nearest printed card: Mogg Assassin ("{T}: You choose target creature
    // an opponent controls, and that opponent chooses target creature. Flip a coin. If you win the
    // flip, destroy the creature you chose. If you lose the flip, destroy the creature your
    // opponent chose."); Crooked Scales ("{4}, {T}: Flip a coin. If you win the flip, destroy
    // target creature an opponent controls. If you lose the flip, destroy target creature you
    // control unless you pay {3} and repeat this process.") -- Mogg Assassin is an activated
    // ability declaring creature targets before coin destroy outcomes; it has no variable-count
    // group or targeting tax.
    /// Object ids: Kopala 1, untaxed creature 2, source 3.
    fn scenario() -> (GameRunner, ObjectId, ObjectId, ObjectId) {
        let mut definition = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::TargetOnly {
                target: TargetFilter::Typed(TypedFilter::creature()),
            },
        )
        .cost(AbilityCost::Mana {
            cost: ManaCost::generic(1),
        })
        .sub_ability(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::FlipCoins {
                flips: CoinFlipRun::single(),
                win_effect: None,
                lose_effect: Some(Box::new(AbilityDefinition::new(
                    AbilityKind::Spell,
                    Effect::Destroy {
                        target: TargetFilter::Typed(TypedFilter::creature()),
                        cant_regenerate: false,
                    },
                ))),
                flipper: TargetFilter::Controller,
            },
        ));
        definition.multi_target = Some(MultiTargetSpec::fixed(0, 2));

        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let kopala = scenario
            .add_creature_from_oracle(P1, "Kopala, Warden of Waves", 2, 2, KOPALA)
            .with_subtypes(vec!["Merfolk", "Wizard"])
            .id();
        let untaxed = scenario.add_creature(P0, "Untaxed creature", 2, 2).id();
        let source = scenario
            .add_artifact_from_oracle(P0, "Taxed coin source", "")
            .with_ability_definition(definition)
            .id();
        mana(&mut scenario, 1);
        (scenario.build(), kopala, untaxed, source)
    }

    for won in [false, true] {
        let label = if won { "win" } else { "lose" };
        let (mut runner, kopala, untaxed, source) = scenario();
        pin_outcomes(&mut runner, &[won]);
        assert!(
            can_activate_ability_now(runner.state(), P0, source, 0),
            "{label}: every untaxed completion declines an ancestor slot mid-vector, so the activation must be offered"
        );
        assert!(
            legal_actions(runner.state()).iter().any(|action| matches!(
                action,
                GameAction::ActivateAbility {
                    source_id,
                    ability_index: 0,
                    ..
                } if *source_id == source
            )),
            "{label}: the AI legal-action list offers it too"
        );
        let mut events = runner
            .act(GameAction::ActivateAbility {
                source_id: source,
                ability_index: 0,
                payment_mode: CastPaymentMode::Auto,
            })
            .unwrap_or_else(|err| panic!("{label}: activation refused: {err:?}"))
            .events;
        answer_prompt_choices(&mut runner, label, &[None, Some(untaxed)]);
        if matches!(runner.state().waiting_for, WaitingFor::ManaPayment { .. }) {
            events.extend(
                runner
                    .act(GameAction::PassPriority)
                    .expect("pay announced activation")
                    .events,
            );
        }
        assert_eq!(pool(&runner), 0, "{label}: paid {{1}}, untaxed");
        assert_eq!(
            runner.state().stack.len(),
            1,
            "{label}: the activated ability is on the stack"
        );
        finish(&mut runner, &mut events);
        assert_eq!(flips(&events), vec![won], "{label}");
        // CR 705.2 + CR 608.2c: only the losing outcome destroys its target.
        assert_eq!(
            runner.state().objects[&untaxed].zone,
            if won {
                Zone::Battlefield
            } else {
                Zone::Graveyard
            },
            "{label}: the losing outcome destroys the untaxed creature"
        );
        assert_eq!(
            runner.state().objects[&kopala].zone,
            Zone::Battlefield,
            "{label}: Kopala is untouched"
        );
        assert_finished(&runner);
    }
}

// Real cards: none of the 86 printed coin-flip cards in the exported card data exhibits this shape:
// A coin whose only (won) outcome destroys up to two target artifacts, followed by an unconditional
// 'destroy target creature' continuation; the bulk list must leave the continuation its own slot.
// Nearest printed card: Goblin Archaeologist ("{R}, {T}: Flip a coin. If you win the flip, destroy
// target artifact and untap this creature. If you lose the flip, sacrifice this creature.");
// Chaotic Strike ("Cast this spell only during combat after blockers are declared. / Flip a coin.
// If you win the flip, target creature gets +1/+1 until end of turn. / Draw a card.") -- Goblin
// Archaeologist has a won destroy-artifact target; Chaotic Strike has an unconditional continuation
// after the coin. Neither has a targeted continuation.
/// Synthetic. CR 601.2c + CR 115.1: the coin node's continuation is announced
/// up front as its own required target, so a multi-target node nested in the
/// Won outcome must not claim it. The continuation runs whichever way the flip
/// goes; only the winning outcome destroys its own target (CR 705.2 + CR 608.2c).
#[test]
fn bulk_declaration_keeps_a_coin_outcome_group_off_the_continuation_target() {
    use engine::types::ability::{
        AbilityDefinition, AbilityKind, MultiTargetSpec, TypeFilter, TypedFilter,
    };

    for won in [false, true] {
        let label = if won { "win" } else { "lose" };
        let mut won_effect = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Destroy {
                target: TargetFilter::Typed(TypedFilter::new(TypeFilter::Artifact)),
                cant_regenerate: false,
            },
        );
        won_effect.multi_target = Some(MultiTargetSpec::fixed(0, 2));
        let definition = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::FlipCoins {
                flips: CoinFlipRun::single(),
                win_effect: Some(Box::new(won_effect)),
                lose_effect: None,
                flipper: TargetFilter::Controller,
            },
        )
        .sub_ability(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Destroy {
                target: TargetFilter::Typed(TypedFilter::creature()),
                cant_regenerate: false,
            },
        ));

        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let artifact = scenario
            .add_artifact_from_oracle(P1, "Won outcome target", "")
            .id();
        let victim = scenario.add_creature(P1, "Continuation target", 4, 4).id();
        let distractor = scenario.add_creature(P0, "Never selected", 2, 2).id();
        let spell = scenario
            .add_spell_to_hand(P0, "Synthetic coin continuation", true)
            .with_ability_definition(definition)
            .id();
        let mut runner = scenario.build();
        pin_outcomes(&mut runner, &[won]);
        cast_for_prompts(&mut runner, spell);

        let targets = vec![TargetRef::Object(artifact), TargetRef::Object(victim)];
        let mut events = runner
            .act(GameAction::SelectTargets {
                targets: targets.clone(),
            })
            .unwrap_or_else(|err| panic!("{label}: bulk declaration {targets:?} rejected: {err:?}"))
            .events;
        {
            // CR 601.2c + CR 115.1: Won's group ends at its own announced
            // boundary, so the continuation keeps its own slot.
            let ability = runner.state().stack.back().unwrap().ability().unwrap();
            assert_eq!(
                ability
                    .coin_branches
                    .as_ref()
                    .unwrap()
                    .won
                    .as_ref()
                    .unwrap()
                    .targets,
                vec![TargetRef::Object(artifact)],
                "{label}: Won keeps to its own announced group"
            );
            assert_eq!(
                ability.sub_ability.as_ref().unwrap().targets,
                vec![TargetRef::Object(victim)],
                "{label}: the continuation keeps its own slot"
            );
        }
        finish(&mut runner, &mut events);
        assert_eq!(flips(&events), vec![won], "{label}");
        // CR 608.2c: the continuation runs after the coin outcome whatever the
        // flip; CR 705.2: only the winning outcome destroys its own target.
        assert_eq!(
            runner.state().objects[&victim].zone,
            Zone::Graveyard,
            "{label}: the continuation destroys its own target whatever the flip"
        );
        assert_eq!(
            runner.state().objects[&artifact].zone,
            if won {
                Zone::Graveyard
            } else {
                Zone::Battlefield
            },
            "{label}: only the winning outcome destroys its own target"
        );
        assert_eq!(
            runner.state().objects[&distractor].zone,
            Zone::Battlefield,
            "{label}: distractor"
        );
        assert_finished(&runner);
    }
}

// Real cards: none of the 86 printed coin-flip cards in the exported card data divides damage
// before a coin. Nearest printed card: Goblin Bangchuckers ("{T}: Flip a coin. If you win the flip,
// this creature deals 2 damage to any target. If you lose the flip, this creature deals 2 damage to
// itself.") deals damage in a coin outcome; Crooked Scales ("{4}, {T}: Flip a coin. If you win the
// flip, destroy target creature an opponent controls. If you lose the flip, destroy target creature
// you control unless you pay {3} and repeat this process.") declares a destroy target in a coin
// outcome. Neither divides damage before the coin.
/// Synthetic; no printed card has this shape. Pins that a divided-damage coin
/// chain is refused, not mis-assigned, when the pool caps the prompt.
/// CR 601.2d: the root divides a fixed pool of one damage, so
/// `cap_distribution_target_slots` trims the "up to two target creatures"
/// prompt to a single slot. That trim is positional: it also drops the coin
/// outcome's trailing required slot (CR 705.2, CR 608.2c). The announced
/// targets (CR 601.2c) can then no longer fill every required slot, and the
/// engine fails closed on both the slot-by-slot (CR 115.1,
/// `GameAction::ChooseTarget`) and the bulk (`GameAction::SelectTargets`)
/// entries, instead of silently putting the one chosen target on the losing
/// outcome's slot and leaving the divided node with none. The refusal is
/// deliberate and tracked as a follow-up; it must not regress to a silent
/// mis-assignment, and a refused cast must leave the game state untouched.
#[test]
fn divided_damage_coin_chain_is_refused_not_mis_assigned_when_the_pool_caps_the_prompt() {
    use engine::types::ability::{
        AbilityDefinition, AbilityKind, MultiTargetSpec, QuantityExpr, TypedFilter,
    };
    use engine::types::game_state::DistributionUnit;

    // Real cards: none of the 86 printed coin-flip cards in the exported card data exhibits this
    // shape: AST ability: deal 1 damage divided among up to two target creatures, then a coin whose
    // losing outcome destroys target creature. Nearest printed card: Goblin Bangchuckers ("{T}:
    // Flip a coin. If you win the flip, this creature deals 2 damage to any target. If you lose the
    // flip, this creature deals 2 damage to itself."); Crooked Scales ("{4}, {T}: Flip a coin. If
    // you win the flip, destroy target creature an opponent controls. If you lose the flip, destroy
    // target creature you control unless you pay {3} and repeat this process.") -- Goblin
    // Bangchuckers has damage in a coin outcome; Crooked Scales has destroy-target outcomes.
    // Neither divides damage before the coin.
    fn definition() -> AbilityDefinition {
        AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::DealDamage {
                amount: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Typed(TypedFilter::creature()),
                damage_source: None,
                excess: None,
            },
        )
        .sub_ability(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::FlipCoins {
                flips: CoinFlipRun::single(),
                win_effect: None,
                lose_effect: Some(Box::new(AbilityDefinition::new(
                    AbilityKind::Spell,
                    Effect::Destroy {
                        target: TargetFilter::Typed(TypedFilter::creature()),
                        cant_regenerate: false,
                    },
                ))),
                flipper: TargetFilter::Controller,
            },
        ))
        // CR 601.2c: "up to two targets" is announced with the spell; the pool
        // of one (CR 601.2d) caps that prompt to a single damage slot.
        .multi_target(MultiTargetSpec::fixed(0, 2))
        .distribute(DistributionUnit::Damage)
    }

    // Real cards: none of the 86 printed coin-flip cards in the exported card data exhibits this
    // shape: Builds the divided-damage board: the synthetic spell from definition() plus two
    // textless filler creatures (damage target and an unselected distractor). Nearest printed card:
    // Goblin Bangchuckers ("{T}: Flip a coin. If you win the flip, this creature deals 2 damage to
    // any target. If you lose the flip, this creature deals 2 damage to itself."); Crooked Scales
    // ("{4}, {T}: Flip a coin. If you win the flip, destroy target creature an opponent controls.
    // If you lose the flip, destroy target creature you control unless you pay {3} and repeat this
    // process.") -- Goblin Bangchuckers has damage in a coin outcome; Crooked Scales has
    // destroy-target outcomes. Neither divides damage before the coin.
    /// Object ids: damage target 1, distractor 2, spell 3.
    fn scenario() -> (GameRunner, ObjectId, ObjectId, ObjectId) {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let damage_target = scenario
            .add_creature(P1, "Divided damage target", 4, 4)
            .id();
        let distractor = scenario.add_creature(P0, "Never selected", 2, 2).id();
        let spell = scenario
            .add_spell_to_hand(P0, "Synthetic divided damage coin chain", true)
            .with_ability_definition(definition())
            .id();
        (scenario.build(), damage_target, distractor, spell)
    }

    /// CR 601.2c: a refused target choice changes nothing. The target prompt is
    /// still open and neither creature was touched. (The spell itself is
    /// already on the stack: CR 601.2a moves it there when the cast begins,
    /// before its targets are chosen.)
    fn assert_state_did_not_advance(
        runner: &GameRunner,
        damage_target: ObjectId,
        distractor: ObjectId,
        label: &str,
    ) {
        assert!(
            matches!(
                runner.state().waiting_for,
                WaitingFor::TargetSelection { .. }
            ),
            "{label}: the refusal leaves the declaration open, got {:?}",
            runner.state().waiting_for
        );
        for (id, name) in [(damage_target, "damage target"), (distractor, "distractor")] {
            assert_eq!(
                runner.state().objects[&id].damage_marked,
                0,
                "{label}: {name} was not damaged"
            );
            assert_eq!(
                runner.state().objects[&id].zone,
                Zone::Battlefield,
                "{label}: {name} is still on the battlefield"
            );
        }
    }

    for won in [false, true] {
        let outcome = if won { "win" } else { "lose" };

        // Leg 1: the production slot-by-slot entry (`GameAction::ChooseTarget`).
        // The capped prompt has a single slot, so the damage target is the final
        // submission and the partition check refuses it.
        let (mut runner, damage_target, distractor, spell) = scenario();
        let label = format!("{outcome}/choose-target");
        pin_outcomes(&mut runner, &[won]);
        cast_for_prompts(&mut runner, spell);
        let err = runner
            .act(GameAction::ChooseTarget {
                target: Some(TargetRef::Object(damage_target)),
            })
            .err()
            .unwrap_or_else(|| {
                panic!("{label}: the capped single-slot announcement must be refused")
            });
        assert_eq!(
            format!("{err:?}"),
            r#"InvalidAction("Target selection does not match the announced target slots")"#,
            "{label}"
        );
        assert_state_did_not_advance(&runner, damage_target, distractor, &label);

        // Leg 2: the bulk entry (`GameAction::SelectTargets`) on a fresh runner
        // is refused for the same reason: the coin outcome's required slot is
        // left undeclared.
        let (mut runner, damage_target, distractor, spell) = scenario();
        let label = format!("{outcome}/bulk");
        pin_outcomes(&mut runner, &[won]);
        cast_for_prompts(&mut runner, spell);
        let err = runner
            .act(GameAction::SelectTargets {
                targets: vec![TargetRef::Object(damage_target)],
            })
            .err()
            .unwrap_or_else(|| {
                panic!("{label}: the capped single-target submission must be refused")
            });
        assert!(
            format!("{err:?}").contains("Missing required target"),
            "{label}: unexpected refusal {err:?}"
        );
        assert_state_did_not_advance(&runner, damage_target, distractor, &label);
    }
}
