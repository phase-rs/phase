use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{ControllerRef, Effect, PlayerFilter, StaticCondition, TargetFilter};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaUnit};
use engine::types::phase::Phase;
use engine::types::statics::{CastFrequency, CostModifyMode, StaticMode};
use engine::types::triggers::TriggerMode;

const HOJO: &str = "The first activated ability you activate during your turn that targets a creature you control costs {2} less to activate.\nWhenever one or more creatures you control become the target of an activated ability, draw a card. This ability triggers only once each turn.";
const KOPALA: &str = "Spells your opponents cast that target a Merfolk you control cost {2} more to cast.\nAbilities your opponents activate that target a Merfolk you control cost {2} more to activate.";
const FERVENT: &str = "First strike, haste\nWhenever Fervent Champion attacks, another target attacking Knight you control gets +1/+0 until end of turn.\nEquip abilities you activate that target Fervent Champion cost {3} less to activate.";
const RAFT: &str = "{2}, {T}: Tap target creature. This ability costs {1} less to activate if it targets a creature with power 3 or less.";
const TAP_ABILITY: &str = "{2}: Tap target creature.";
const TRAINING_GROUNDS: &str =
    "Activated abilities of creatures you control cost {2} less to activate.";
const TAP_FOUR: &str = "{4}: Tap target creature.";
const GAIN_LIFE_FOUR: &str = "{4}: You gain 1 life.";

fn mana_pool(runner: &GameRunner) -> usize {
    runner
        .state()
        .players
        .iter()
        .find(|p| p.id == P0)
        .unwrap()
        .mana_pool
        .total()
}

fn add_white_mana(s: &mut GameScenario, amount: usize) {
    s.with_mana_pool(
        P0,
        (0..amount)
            .map(|_| ManaUnit::new(ManaColor::White.into(), ObjectId(0), false, Vec::new()))
            .collect(),
    );
}

fn reduce_ability_statics(
    oracle: &str,
    name: &str,
    types: &[String],
) -> Vec<engine::types::ability::StaticDefinition> {
    parse_oracle_text(oracle, name, &[], types, &[])
        .statics
        .into_iter()
        .filter(|def| matches!(def.mode, StaticMode::ReduceAbilityCost { .. }))
        .collect()
}

fn assert_targets_creature_you_control(targets: &Option<TargetFilter>) {
    let Some(TargetFilter::Typed(typed)) = targets else {
        panic!("expected typed target filter, got {targets:?}");
    };
    assert_eq!(typed.controller, Some(ControllerRef::You));
    assert!(
        !typed.type_filters.is_empty(),
        "expected creature type filter: {typed:?}"
    );
}

#[test]
fn professor_hojo_static_parses_frequency_targets_and_turn_condition() {
    let types = vec!["Creature".to_string()];
    let parsed = parse_oracle_text(HOJO, "Professor Hojo", &[], &types, &[]);
    assert!(
        parsed.parse_warnings.is_empty(),
        "warnings: {:?}",
        parsed.parse_warnings
    );
    assert!(parsed
        .abilities
        .iter()
        .all(|a| !matches!(a.effect.as_ref(), Effect::Unimplemented { .. })));
    assert!(parsed
        .triggers
        .iter()
        .any(|trigger| trigger.mode == TriggerMode::BecomesTarget));
    let def = parsed
        .statics
        .iter()
        .find(|def| matches!(def.mode, StaticMode::ReduceAbilityCost { .. }))
        .expect("cost static");
    assert_eq!(def.condition, Some(StaticCondition::DuringYourTurn));
    let StaticMode::ReduceAbilityCost {
        mode,
        keyword,
        amount,
        activator,
        targets,
        frequency,
        ..
    } = &def.mode
    else {
        unreachable!()
    };
    assert_eq!(*mode, CostModifyMode::Reduce);
    assert_eq!(keyword, "activated");
    assert_eq!(*amount, 2);
    assert_eq!(*activator, Some(PlayerFilter::Controller));
    assert_eq!(*frequency, Some(CastFrequency::OncePerTurn));
    assert_targets_creature_you_control(targets);
}

#[test]
fn target_restricted_cost_static_parses_kopala_and_equip() {
    let creature = vec!["Creature".to_string()];
    let kopala = reduce_ability_statics(KOPALA, "Kopala, Warden of Waves", &creature);
    let kopala_def = kopala
        .iter()
        .find(|def| {
            matches!(
                def.mode,
                StaticMode::ReduceAbilityCost {
                    mode: CostModifyMode::Raise,
                    ..
                }
            )
        })
        .expect("Kopala activate tax");
    let StaticMode::ReduceAbilityCost {
        amount,
        activator,
        targets,
        ..
    } = &kopala_def.mode
    else {
        unreachable!()
    };
    assert_eq!(*amount, 2);
    assert_eq!(*activator, Some(PlayerFilter::Opponent));
    assert!(targets.is_some(), "Kopala target gate must parse");

    let fervent = reduce_ability_statics(FERVENT, "Fervent Champion", &creature);
    let StaticMode::ReduceAbilityCost {
        keyword,
        amount,
        targets,
        ..
    } = &fervent[0].mode
    else {
        unreachable!()
    };
    assert_eq!(keyword, "equip");
    assert_eq!(*amount, 3);
    assert!(targets.is_some(), "equip target gate must parse");
}

#[test]
fn raft_security_officer_discount_depends_on_committed_target_power() {
    fn run(power: i32) -> usize {
        let mut s = GameScenario::new();
        s.at_phase(Phase::PreCombatMain);
        let raft = s
            .add_creature_from_oracle(P0, "Raft Security Officer", 1, 3, RAFT)
            .id();
        let victim = s.add_vanilla(P1, power, 5);
        add_white_mana(&mut s, 3);
        let mut runner = s.build();
        runner
            .state_mut()
            .objects
            .get_mut(&raft)
            .unwrap()
            .has_summoning_sickness = false;
        runner.activate(raft, 0).target_object(victim).resolve();
        mana_pool(&runner)
    }
    assert_eq!(
        run(1),
        2,
        "positive guard: qualifying target gets {{1}} discount"
    );
    assert_eq!(run(5), 1, "nonqualifying target must pay full {{2}}");
}

#[test]
fn hojo_discount_counts_first_qualifying_activation_only() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    s.add_creature_from_oracle(P0, "Professor Hojo", 2, 2, HOJO);
    let wand = s
        .add_artifact_from_oracle(P0, "Training Wand", TAP_ABILITY)
        .id();
    let own_a = s.add_creature(P0, "Own A", 1, 1).id();
    let own_b = s.add_creature(P0, "Own B", 1, 1).id();
    s.add_card_to_library_top(P0, "Draw A");
    s.add_card_to_library_top(P0, "Draw B");
    add_white_mana(&mut s, 4);
    let mut runner = s.build();
    runner.activate(wand, 0).target_object(own_a).resolve();
    assert_eq!(
        mana_pool(&runner),
        4,
        "positive guard: first qualifying activation is free"
    );
    runner.activate(wand, 0).target_object(own_b).resolve();
    assert_eq!(
        mana_pool(&runner),
        2,
        "second qualifying activation pays {{2}}"
    );
}

#[test]
fn hojo_discount_rejects_wrong_controller_and_wrong_turn() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    s.add_creature_from_oracle(P0, "Professor Hojo", 2, 2, HOJO);
    let wand = s
        .add_artifact_from_oracle(P0, "Training Wand", TAP_ABILITY)
        .id();
    let enemy = s.add_creature(P1, "Enemy", 1, 1).id();
    add_white_mana(&mut s, 4);
    let mut runner = s.build();
    runner.activate(wand, 0).target_object(enemy).resolve();
    assert_eq!(
        mana_pool(&runner),
        2,
        "opponent-controlled target pays full {{2}}"
    );

    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    s.add_creature_from_oracle(P0, "Professor Hojo", 2, 2, HOJO);
    let wand = s
        .add_artifact_from_oracle(P0, "Training Wand", TAP_ABILITY)
        .id();
    let own = s.add_creature(P0, "Own", 1, 1).id();
    s.add_card_to_library_top(P0, "Draw A");
    add_white_mana(&mut s, 4);
    let mut runner = s.build();
    runner.state_mut().active_player = P1;
    runner.activate(wand, 0).target_object(own).resolve();
    assert_eq!(mana_pool(&runner), 2, "not your turn pays full {{2}}");
}

#[test]
fn fervent_champion_equip_discount_depends_on_target() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let fervent = s
        .add_creature_from_oracle(P0, "Fervent Champion", 1, 1, FERVENT)
        .id();
    let other = s.add_creature(P0, "Other", 1, 1).id();
    let equipment = s
        .add_artifact_from_oracle(P0, "Practice Sword", "Equip {3}")
        .id();
    add_white_mana(&mut s, 6);
    let mut runner = s.build();
    runner
        .activate(equipment, 0)
        .target_object(fervent)
        .resolve();
    assert_eq!(
        mana_pool(&runner),
        6,
        "positive guard: Fervent equip is free"
    );
    runner.activate(equipment, 0).target_object(other).resolve();
    assert_eq!(mana_pool(&runner), 3, "other creature pays equip {{3}}");
}

/// CR 601.2f + CR 602.2b: the post-target (`Committed`) cost pass exists only to
/// fold in riders the announcement pass had to defer. A target-INDEPENDENT
/// static (Training Grounds) is already folded in at announcement, so it must
/// NOT be applied a second time when targets settle.
///
/// Regression guard for a measured defect: with the `Committed` pass applying
/// every static, a TARGETED `{4}` ability under Training Grounds paid `{0}`
/// instead of `{2}` while the untargeted path stayed correct — a silent
/// misprice with no marker and no crash. The untargeted leg is the control that
/// proves the guard is discriminating rather than blanket.
#[test]
fn target_independent_static_is_not_double_applied_after_targets_settle() {
    fn paid(ability_oracle: &str, targeted: bool) -> usize {
        let mut s = GameScenario::new();
        s.at_phase(Phase::PreCombatMain);
        s.add_artifact_from_oracle(P0, "Training Grounds", TRAINING_GROUNDS);
        let src = s
            .add_creature_from_oracle(P0, "Tapper", 2, 2, ability_oracle)
            .id();
        let victim = s.add_vanilla(P1, 1, 1);
        add_white_mana(&mut s, 6);
        let mut runner = s.build();
        runner
            .state_mut()
            .objects
            .get_mut(&src)
            .unwrap()
            .has_summoning_sickness = false;
        let before = mana_pool(&runner);
        if targeted {
            runner.activate(src, 0).target_object(victim).resolve();
        } else {
            runner.activate(src, 0).resolve();
        }
        before - mana_pool(&runner)
    }

    assert_eq!(
        paid(TAP_FOUR, true),
        2,
        "targeted {{4}} under Training Grounds pays {{2}} - a second application would make it free"
    );
    assert_eq!(
        paid(GAIN_LIFE_FOUR, false),
        2,
        "control: the untargeted path was already correct and must stay {{2}}"
    );
}

/// CR 118.7 + CR 115.9b + CR 601.2c: the `Raise` direction of a target-gated
/// activation-cost static, at RUNTIME rather than in the parsed shape.
///
/// Kopala, Warden of Waves taxes an OPPONENT's activated ability that targets a
/// Merfolk its controller controls. Two legs, because they exercise different
/// cost carriers:
///   * a fixed `{2}` cost, which rides `PendingCast::activation_cost`;
///   * an `{X}` cost, whose mana leg is extracted into `PendingCast::cost` and
///     has X concretized BEFORE targets settle — still unpaid at that point, so
///     it is repriceable.
///
/// The `{X}` leg is a regression guard for a fail-OPEN defect: an earlier
/// revision skipped every `{X}` activation in the post-target pass, on the
/// premise that its mana was already paid. The premise was false, and the skip
/// was direction-blind, so the opponent dodged the tax outright (CR 118.7
/// underpayment) instead of merely forgoing a discount.
///
/// The non-Merfolk leg is the discriminating control: it proves the tax is
/// gated on the target clause and not applied blanket.
#[test]
fn kopala_taxes_opponent_activation_that_targets_a_protected_merfolk() {
    const KOPALA_ACTIVATE_HALF: &str = "Abilities your opponents activate that target a Merfolk you control cost {2} more to activate.";
    const FIXED_TAP: &str = "{2}: Tap target creature.";
    const X_TAP: &str = "{X}: Tap target creature.";
    // Both carriers populated at once: the mana leg is extracted into
    // `PendingCast::cost` while the `{T}` residual stays in `activation_cost`.
    const X_TAP_AND_TAP: &str = "{X}, {T}: Tap target creature.";

    /// Mana P0 (the taxed opponent) actually spent.
    fn paid(ability: &str, x: Option<u32>, target_is_merfolk: bool) -> usize {
        let mut s = GameScenario::new();
        s.at_phase(Phase::PreCombatMain);
        // P1 controls both Kopala and the Merfolk the tax protects.
        s.add_creature_from_oracle(P1, "Kopala, Warden of Waves", 2, 2, KOPALA_ACTIVATE_HALF);
        let mut victim = s.add_creature(P1, "Merfolk Trickster", 2, 2);
        if target_is_merfolk {
            victim.with_subtypes(vec!["Merfolk"]);
        }
        let victim = victim.id();
        // P0 is the opponent whose activation is taxed.
        let src = s.add_artifact_from_oracle(P0, "Tapper", ability).id();
        add_white_mana(&mut s, 8);
        let mut runner = s.build();
        let before = mana_pool(&runner);
        let activation = runner.activate(src, 0);
        let activation = match x {
            Some(value) => activation.x(value),
            None => activation,
        };
        activation.target_object(victim).resolve();
        before - mana_pool(&runner)
    }

    assert_eq!(
        paid(FIXED_TAP, None, true),
        4,
        "positive guard: a fixed {{2}} activation targeting the protected Merfolk pays {{2}} + the {{2}} tax"
    );
    assert_eq!(
        paid(X_TAP, Some(2), true),
        4,
        "an {{X=2}} activation targeting the protected Merfolk pays X + the {{2}} tax - \
         skipping {{X}} here let the opponent dodge the tax entirely"
    );
    assert_eq!(
        paid(FIXED_TAP, None, false),
        2,
        "control: the same activation targeting a NON-Merfolk is untaxed, so the gate discriminates"
    );
    // CR 115.9b: the `{X}` carrier needs its OWN negative control. Without it a
    // regression that made the `pending.cost` path tax blanket - ignoring the
    // target clause entirely - would still satisfy every assertion above, because
    // the only other control rides the `activation_cost` carrier. The repaired
    // carrier is exactly the one whose predecessor failed silently through a full
    // green suite, so it does not get to be the unpinned one.
    assert_eq!(
        paid(X_TAP, Some(2), false),
        2,
        "control: an {{X}} activation targeting a NON-Merfolk is untaxed on the pending.cost carrier too"
    );
    assert_eq!(
        paid(X_TAP_AND_TAP, Some(2), true),
        4,
        "both carriers populated ({{X}} mana leg + {{T}} residual): the protected Merfolk is still taxed"
    );
    assert_eq!(
        paid(X_TAP_AND_TAP, Some(2), false),
        2,
        "control: both carriers populated, NON-Merfolk target, so no tax"
    );
}

/// Hojo's trigger as parsed, isolated from his cost static.
const HOJO_TRIGGER: &str = "Whenever one or more creatures you control become the target of an activated ability, draw a card. This ability triggers only once each turn.";

/// Number of cards in P0's library.
fn library_size(runner: &GameRunner) -> usize {
    runner
        .state()
        .objects
        .values()
        .filter(|o| o.zone == engine::types::zones::Zone::Library && o.owner == P0)
        .count()
}

/// CR 113.3b + CR 113.3c + CR 115.1a: Professor Hojo's trigger reads "become the
/// target of an ACTIVATED ability", so a TRIGGERED ability targeting a creature
/// Hojo's controller controls must not fire it.
///
/// Parse shape first, then runtime: the parse pins that the printed kind survives
/// into `valid_source`, and the runtime legs prove the kind gates at match time.
/// That includes the activated leg, whose ability is not yet physically on the
/// stack when its targets are declared: the engine matches it through the virtual
/// targeting-source projection.
#[test]
fn professor_hojo_trigger_fires_only_for_activated_abilities() {
    use engine::types::ability::StackAbilityKind;

    // Parse: the kind is carried, not dropped to "any ability".
    let parsed = parse_oracle_text(
        HOJO_TRIGGER,
        "Professor Hojo",
        &[],
        &["Creature".to_string()],
        &[],
    );
    let trigger = parsed
        .triggers
        .iter()
        .find(|t| t.mode == TriggerMode::BecomesTarget)
        .expect("Hojo's trigger parses to BecomesTarget, not Unknown");
    assert!(
        matches!(
            trigger.valid_source,
            Some(TargetFilter::StackAbility {
                kind: Some(StackAbilityKind::Activated),
                ..
            })
        ),
        "the printed 'activated' qualifier must narrow the source, got {:?}",
        trigger.valid_source
    );

    /// P0 controls Hojo plus `own`; the probe card targets `own`.
    /// Returns the number of cards P0 drew.
    fn drawn_after(targeting_card_is_triggered: bool) -> usize {
        let mut s = GameScenario::new();
        s.at_phase(Phase::PreCombatMain);
        s.add_creature_from_oracle(P0, "Professor Hojo", 2, 2, HOJO);
        let own = s.add_creature(P0, "Own", 1, 1).id();
        for name in ["L1", "L2", "L3", "L4"] {
            s.add_card_to_library_top(P0, name);
        }
        add_white_mana(&mut s, 8);
        let source = if targeting_card_is_triggered {
            s.add_creature_to_hand_from_oracle(
                P0,
                "Tap Herald",
                1,
                1,
                "When this creature enters, tap target creature.",
            )
            .id()
        } else {
            s.add_artifact_from_oracle(P0, "Tapper", "{4}: Tap target creature.")
                .id()
        };
        let mut runner = s.build();
        let before = library_size(&runner);
        if targeting_card_is_triggered {
            runner.cast(source).target_object(own).resolve();
        } else {
            runner.activate(source, 0).target_object(own).resolve();
        }
        before - library_size(&runner)
    }

    // Positive reach guard: the activated case DOES draw, so the negative below
    // cannot pass because the trigger never fires at all.
    assert_eq!(
        drawn_after(false),
        1,
        "an activated ability targeting a creature you control draws a card"
    );
    assert_eq!(
        drawn_after(true),
        0,
        "a TRIGGERED ability targeting a creature you control must not fire Hojo"
    );
}

/// M3, CR 107.4d + CR 601.2f: a concrete `{0}` mana leg is a real cost, not an
/// absent one. An opponent's `{X}` activation with X=0, targeting a Merfolk
/// Kopala protects, still pays Kopala's `{2}` tax.
///
/// The `{X}` route extracts its mana leg into `PendingCast::cost` before targets
/// settle. At X=0 that leg is `{0}`. The settlement write-back used to test it with
/// `is_without_paying_mana()`, which is true for a concrete `{0}`, so it read the
/// leg as absent, returned early, and skipped every target-gated modifier.
#[test]
fn x_zero_activation_still_pays_a_target_gated_tax() {
    const KOPALA_ACTIVATE_HALF: &str = "Abilities your opponents activate that target a Merfolk you control cost {2} more to activate.";
    const X_TAP: &str = "{X}: Tap target creature.";

    fn paid_at_x_zero(target_is_merfolk: bool) -> usize {
        let mut s = GameScenario::new();
        s.at_phase(Phase::PreCombatMain);
        s.add_creature_from_oracle(P1, "Kopala, Warden of Waves", 2, 2, KOPALA_ACTIVATE_HALF);
        let mut victim = s.add_creature(P1, "Merfolk Trickster", 2, 2);
        if target_is_merfolk {
            victim.with_subtypes(vec!["Merfolk"]);
        }
        let victim = victim.id();
        let src = s.add_artifact_from_oracle(P0, "Tapper", X_TAP).id();
        add_white_mana(&mut s, 8);
        let mut runner = s.build();
        let before = mana_pool(&runner);
        runner.activate(src, 0).x(0).target_object(victim).resolve();
        before - mana_pool(&runner)
    }

    assert_eq!(
        paid_at_x_zero(true),
        2,
        "X=0 targeting the protected Merfolk pays the {{2}} tax: {{0}} is a real mana leg"
    );
    assert_eq!(
        paid_at_x_zero(false),
        0,
        "control: X=0 targeting a non-Merfolk is untaxed and costs nothing"
    );
}

/// M3 route separation. Deciding WHERE a target-first activation's price is
/// written (the settlement write-back) and deciding WHETHER payment is skipped
/// (the mana-leg finalizer's zero-leg check) are two different sites. M3 changed
/// only the first. This pins that each kind of activation takes its own site and
/// never the other's, so a later change to one can't silently move into the
/// other.
///
/// Each row is the other's reach guard: the targeted row proves the write-back
/// counter is live, and the untargeted row proves the skip counter is live. So
/// neither row's zero is vacuous.
#[test]
fn settlement_writeback_and_zero_leg_skip_are_distinct_sites() {
    use engine::game::perf_counters;

    // Target-first, folds to {0}: an unfloored -2 plus Hojo (-2) on {3},
    // targeting your own creature, with no mana. Priced at settlement. Both
    // reductions are unfloored, so every order locks {0} and no CR 601.2f
    // election is raised.
    {
        let mut s = GameScenario::new();
        s.at_phase(Phase::PreCombatMain);
        s.add_artifact_from_oracle(P0, "Training Grounds", TRAINING_GROUNDS);
        s.add_creature_from_oracle(P0, "Professor Hojo", 2, 2, HOJO);
        let own = s.add_creature(P0, "Own", 1, 1).id();
        // Hojo's trigger draws a card when your creature is targeted, so the
        // library must be non-empty or the draw ends the game before the
        // activation resolves.
        for name in ["L1", "L2"] {
            s.add_card_to_library_top(P0, name);
        }
        let src = s
            .add_creature_from_oracle(P0, "Tapper", 2, 2, "{3}: Tap target creature.")
            .id();
        let mut runner = s.build();
        runner
            .state_mut()
            .objects
            .get_mut(&src)
            .unwrap()
            .has_summoning_sickness = false;
        perf_counters::reset();
        runner.activate(src, 0).target_object(own).resolve();
        let routes = perf_counters::activation_cost_route_snapshot();
        assert!(
            routes.settlement_writebacks >= 1,
            "a target-first activation is priced at the settlement write-back: {routes:?}"
        );
        assert_eq!(
            routes.zero_mana_leg_skips, 0,
            "and never through the payment-skip check: {routes:?}"
        );
        assert!(
            runner.state().objects[&own].tapped,
            "positive guard: the {{0}} activation resolved and tapped its target"
        );
    }

    // Untargeted, a concrete {0}: nothing to settle, payment skipped.
    {
        let mut s = GameScenario::new();
        s.at_phase(Phase::PreCombatMain);
        let src = s
            .add_artifact_from_oracle(P0, "Free Relic", "{0}: You gain 1 life.")
            .id();
        let mut runner = s.build();
        perf_counters::reset();
        runner.activate(src, 0).resolve();
        let routes = perf_counters::activation_cost_route_snapshot();
        assert_eq!(
            routes.zero_mana_leg_skips, 1,
            "an untargeted {{0}} activation takes the payment-skip check: {routes:?}"
        );
        assert_eq!(
            routes.settlement_writebacks, 0,
            "and never the settlement write-back (it has no targets): {routes:?}"
        );
    }
}

/// Tezzeret, Betrayer of Flesh's full Oracle text (Scryfall `cards/named`).
const TEZZERET_FULL: &str = "The first activated ability of an artifact you activate each turn costs {2} less to activate.\n+1: Draw two cards. Then discard two cards unless you discard an artifact card.\n\u{2212}2: Target artifact becomes an artifact creature. If it isn't a Vehicle, it has base power and toughness 4/4.\n\u{2212}6: You get an emblem with \"Whenever an artifact you control becomes tapped, draw a card.\"";

fn card_face(name: &str, oracle: &str, types: &[&str]) -> engine::types::card::CardFace {
    let types: Vec<String> = types.iter().map(|t| t.to_string()).collect();
    let parsed = parse_oracle_text(oracle, name, &[], &types, &[]);
    engine::types::card::CardFace {
        name: name.to_string(),
        oracle_text: Some(oracle.to_string()),
        abilities: parsed.abilities,
        triggers: parsed.triggers,
        static_abilities: parsed.statics,
        replacements: parsed.replacements,
        ..Default::default()
    }
}

/// CR 605.1a: a once-per-turn activation discount WITHOUT a target restriction
/// stays a strict parse failure, so its card is reported unsupported rather than
/// silently mispriced.
///
/// Tezzeret's discount applies to "the first activated ability of an artifact you
/// activate each turn", and its ruling says that "does not exclude mana abilities".
/// The mana-ability path applies no `ReduceAbilityCost`, so if this line were
/// supported, a mana ability that was the turn's first activation would go
/// undiscounted. Declining it keeps the card honestly red until that path
/// prices activations.
///
/// The paired control is Professor Hojo: the SAME grammar branch, with a target
/// restriction. A target-restricted ability can never be a mana ability (CR
/// 605.1a: a mana ability "doesn't require a target"), so Hojo stays supported.
/// Without that control this test could pass because the branch broke outright,
/// not because the target clause is what it requires.
#[test]
fn once_per_turn_discount_without_a_target_restriction_stays_unsupported() {
    let tezzeret = card_face(
        "Tezzeret, Betrayer of Flesh",
        TEZZERET_FULL,
        &["Planeswalker"],
    );
    // Strict failure: no discount static is emitted...
    assert!(
        !tezzeret
            .static_abilities
            .iter()
            .any(|def| matches!(def.mode, StaticMode::ReduceAbilityCost { .. })),
        "Tezzeret's discount must not parse to a ReduceAbilityCost static: {:?}",
        tezzeret.static_abilities
    );
    // ...and the line surfaces as a recorded gap rather than vanishing.
    assert!(
        tezzeret.abilities.iter().any(|def| matches!(
            &*def.effect,
            Effect::Unimplemented { description: Some(d), .. }
                if d.contains("The first activated ability of an artifact")
        )),
        "Tezzeret's discount line must stay an Unimplemented gap"
    );
    // Coverage red: the card is reported unsupported.
    let gaps = engine::game::coverage::card_face_gaps(&tezzeret);
    assert!(
        !gaps.is_empty(),
        "Tezzeret must be reported unsupported (a coverage gap), got no gaps"
    );
    // Reach guard: the rest of the card still parses, so the gap above is the
    // discount line and not a collapsed card.
    assert!(
        tezzeret.abilities.len() >= 3,
        "Tezzeret's three loyalty abilities must still parse: {:?}",
        tezzeret.abilities
    );

    // Paired control: the same branch WITH a target restriction still parses,
    // carries its target gate and the once-per-turn frequency, and leaves Hojo's
    // card with no coverage gap.
    let hojo = card_face("Professor Hojo", HOJO, &["Creature"]);
    let discount = hojo
        .static_abilities
        .iter()
        .find(|def| matches!(def.mode, StaticMode::ReduceAbilityCost { .. }))
        .expect("control: Hojo's target-restricted discount still parses");
    let StaticMode::ReduceAbilityCost {
        frequency, targets, ..
    } = &discount.mode
    else {
        unreachable!()
    };
    assert_eq!(*frequency, Some(CastFrequency::OncePerTurn));
    assert!(targets.is_some(), "control: Hojo carries its target gate");
    assert_eq!(
        engine::game::coverage::card_face_gaps(&hojo),
        Vec::<String>::new(),
        "control: Hojo stays fully supported"
    );
}
