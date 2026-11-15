//! Eighth-round regression tests for pre-spend mana-echo settlement at the
//! three continuation roots that were still rootless, plus the choice-free
//! preview's state deduplication.
//!
//! Card premises (verified against `client/public/card-data.json`):
//! * Mirari's Wake — "Whenever you tap a land for mana, add one mana of any
//!   type that land produced." Each multi-type land tap owes one
//!   `ChooseManaColor` firing (CR 605.1b + CR 608.2d).
//! * Simic Growth Chamber — "{T}: Add {G}{U}." Both base types are produced at
//!   once, so the owed bonus is genuinely choice-owing.
//! * Coal Golem — "{3}, Sacrifice ~: Add {R}{R}{R}." Its mana ability's mana
//!   component is nested inside a composite cost, so paying it exercises the
//!   parent cursor's echo settlement.
//! * Digsite Conservator — "When this creature dies, you may pay {4}. If you
//!   do, discover 4." A resolution-time `Effect::PayCost` whose rider is gated
//!   on `EffectOutcome::OptionalEffectPerformed`.

use std::time::{Duration, Instant};

use engine::game::planechase::{can_roll_planar_die, planar_die_roll_cost};
use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::game::triggers::process_triggers;
use engine::game::zones::{create_object, move_to_zone};
use engine::types::ability::{
    AbilityCost, AdditionalCost, Effect, QuantityExpr, SacrificeCost, TargetFilter, TypeFilter,
    TypedFilter,
};
use engine::types::actions::{GameAction, ResolutionOptionalPaymentChoice};
use engine::types::card_type::CoreType;
use engine::types::events::GameEvent;
use engine::types::format::GameFormat;
use engine::types::game_state::{
    CastPaymentMode, ManaChoice, ManaChoicePrompt, PayCostKind, StackEntryKind, WaitingFor,
};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const MIRARI_ECHO_ORACLE: &str = "Creatures you control get +1/+1.\nWhenever you tap a land for mana, add one mana of any type that land produced.";
const CHAMBER_ORACLE: &str = "This land enters tapped.\nWhen this land enters, return a land you control to its owner's hand.\n{T}: Add {G}{U}.";
const COAL_GOLEM_ORACLE: &str = "{3}, Sacrifice ~: Add {R}{R}{R}.";
const MANABARBS_ORACLE: &str =
    "Whenever a player taps a land for mana, this enchantment deals 1 damage to that player.";
const DIGSITE_ORACLE: &str = "Sacrifice this creature: Exile up to four target cards from a single graveyard. Activate only as a sorcery.\nWhen this creature dies, you may pay {4}. If you do, discover 4.";
const MIARA_ORACLE: &str = "Whenever Miara or another Elf you control dies, you may pay {1} and 1 life. If you do, draw a card.\nPartner (You can have two commanders if both have partner.)";

fn floating_colorless(count: usize) -> Vec<ManaUnit> {
    (0..count)
        .map(|_| ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]))
        .collect()
}

/// CR 901.9: a minimal active plane so the planar-die special action is legal;
/// the plane itself has no abilities, so any rolled face is side-effect free.
fn install_active_plane(runner: &mut GameRunner) {
    let state = runner.state_mut();
    state.format_config.format = GameFormat::Planechase;
    let plane = create_object(
        state,
        CardId(9_791_001),
        P0,
        "Molten Tide Test Plane".to_string(),
        Zone::Command,
    );
    {
        let object = state.objects.get_mut(&plane).unwrap();
        object.card_types.core_types.push(CoreType::Plane);
        object.base_card_types = object.card_types.clone();
    }
    // `create_object` already placed the object in the command zone; only the
    // characteristic fix-up and the planar controller remain.
    debug_assert!(state.command_zone.contains(&plane));
    state.planar_controller = Some(P0);
}

fn assert_single_color_echo_choice(runner: &GameRunner, expected: &[ManaType]) {
    match &runner.state().waiting_for {
        WaitingFor::ChooseManaColor {
            player,
            choice: ManaChoicePrompt::SingleColor { options },
            ..
        } => {
            assert_eq!(*player, P0, "the payer chooses the echo bonus");
            for mana_type in expected {
                assert!(
                    options.contains(mana_type),
                    "expected {mana_type:?} among the produced types, got {options:?}"
                );
            }
        }
        other => panic!("expected a SingleColor echo choice, got {other:?}"),
    }
}

/// CR 116.2i + CR 605.3b + CR 616.1 + CR 608.2d: after three earlier rolls the
/// next planar die costs {3}. Mirari's Wake plus one untapped Simic Growth
/// Chamber pays it as base {G}{U} plus one chosen bonus, so the action must
/// pause for the choice and only then roll — not refuse as unpayable.
#[test]
fn planar_die_bonus_dependent_payment_pauses_for_echo_and_rolls_once() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_enchantment_from_oracle(P0, "Mirari's Wake", MIRARI_ECHO_ORACLE);
    scenario.add_land_from_oracle(P0, "Simic Growth Chamber", CHAMBER_ORACLE);
    let mut runner = scenario.build();
    install_active_plane(&mut runner);
    runner
        .state_mut()
        .planar_die_actions_this_turn
        .insert(P0, 3);

    assert!(
        matches!(
            planar_die_roll_cost(runner.state(), P0),
            ManaCost::Cost { generic: 3, .. }
        ),
        "the fourth roll this turn costs {{3}}"
    );
    assert!(
        can_roll_planar_die(runner.state(), P0),
        "the Resumable preview must count the owed echo bonus"
    );

    let paused = runner
        .act(GameAction::RollPlanarDie)
        .expect("the roll must pause for the echo choice, not fail at affordability");
    assert!(
        !paused
            .events
            .iter()
            .any(|event| matches!(event, GameEvent::PlanarDieRolled { .. })),
        "the die must not be rolled before its cost is paid"
    );
    assert_single_color_echo_choice(&runner, &[ManaType::Green, ManaType::Blue]);
    assert_eq!(
        runner.state().planar_die_actions_this_turn.get(&P0),
        Some(&3),
        "the uncommitted action must not advance the escalating counter"
    );

    let resumed = runner
        .act(GameAction::ChooseManaColor {
            choice: ManaChoice::SingleColor(ManaType::Blue),
            count: 1,
        })
        .expect("answering the echo must finish the paid roll");
    assert!(
        resumed
            .events
            .iter()
            .any(|event| matches!(event, GameEvent::PlanarDieRolled { .. })),
        "the locked-cost roll commits exactly once after the payment settles"
    );
    assert_eq!(
        runner.state().players[P0.0 as usize].mana_pool.total(),
        0,
        "base {{G}}{{U}} plus one bonus exactly funds {{3}}"
    );
    assert_eq!(
        runner.state().planar_die_actions_this_turn.get(&P0),
        Some(&4),
        "the paid roll increments the action count exactly once"
    );
}

/// CR 605.3b + CR 616.1 + CR 608.2d: Coal Golem's {3} mana component is nested
/// under its composite `{3}, Sacrifice ~` cost. Mirari's Wake plus one untapped
/// Simic Growth Chamber is enough only when the owed bonus is counted, so the
/// parent cost must pause for the echo before paying the sacrifice — the child
/// attempt's own events are already claimed by the settled batch.
#[test]
fn nested_mana_sub_cost_settles_echo_before_spending_parent_cost() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_enchantment_from_oracle(P0, "Mirari's Wake", MIRARI_ECHO_ORACLE);
    scenario.add_land_from_oracle(P0, "Simic Growth Chamber", CHAMBER_ORACLE);
    // CR 117.5: Manabarbs is an ordinary observer of the same land tap. The
    // nested ManaAbilityManaPayment chain must converge to Priority and put
    // this trigger on the stack; without the recursive root walk it strands.
    scenario.add_enchantment_from_oracle(P0, "Manabarbs", MANABARBS_ORACLE);
    let golem = scenario
        .add_creature_from_oracle(P0, "Coal Golem", 3, 3, COAL_GOLEM_ORACLE)
        .id();
    let mut runner = scenario.build();

    runner
        .act(GameAction::ActivateAbility {
            source_id: golem,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("the activation preview counts the echo bonus and the live payment pauses");
    assert_single_color_echo_choice(&runner, &[ManaType::Green, ManaType::Blue]);
    assert!(
        runner.state().battlefield.contains(&golem),
        "the sacrifice cost must not be paid before the echo choice settles"
    );
    assert_eq!(
        runner.state().players[P0.0 as usize]
            .mana_pool
            .count_color(ManaType::Red),
        0,
        "the ability has not produced mana while its cost is paused"
    );

    runner
        .act(GameAction::ChooseManaColor {
            choice: ManaChoice::SingleColor(ManaType::Blue),
            count: 1,
        })
        .expect("answering the echo resumes the parent cost");
    assert!(
        !runner.state().battlefield.contains(&golem),
        "the resume pays the parent's sacrifice component exactly once"
    );
    assert_eq!(runner.state().objects[&golem].zone, Zone::Graveyard);
    assert_eq!(
        runner.state().players[P0.0 as usize].mana_pool.total(),
        3,
        "the completed ability adds exactly {{R}}{{R}}{{R}}"
    );
    assert_eq!(
        runner.state().players[P0.0 as usize]
            .mana_pool
            .count_color(ManaType::Red),
        3,
        "all three produced mana is red"
    );
    assert_eq!(
        runner.state().stack.len(),
        1,
        "Manabarbs' ordinary trigger from the same tap must reach the stack before priority returns"
    );
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { player: P0 }
        ),
        "the completed activation returns priority to its controller"
    );
}

/// CR 601.2g + CR 601.2h + CR 605.4a + CR 608.2d: a spell whose non-mana
/// additional cost is deferred past the mana window pays that mana through the
/// same pre-spend echo authority. Mirari's Wake plus one two-type land funds a
/// {1}{G}{G} spell only with the owed bonus, so the deferred route must pause
/// for the type choice before spending mana or committing the sacrifice, then
/// finish the cast once the choice is answered.
#[test]
fn deferred_sacrifice_spell_settles_echo_before_spending_mana() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_enchantment_from_oracle(P0, "Mirari's Wake", MIRARI_ECHO_ORACLE);
    scenario.add_land_from_oracle(P0, "Simic Growth Chamber", CHAMBER_ORACLE);
    let fodder = scenario.add_creature(P0, "Echo Fodder", 1, 1).id();
    let spell = scenario
        .add_spell_to_hand(P0, "Deferred Echo Spell", true)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Green, ManaCostShard::Green],
            generic: 1,
        })
        .with_additional_cost(AdditionalCost::Required(AbilityCost::Sacrifice(
            SacrificeCost::count(
                TargetFilter::Typed(TypedFilter::new(TypeFilter::Creature)),
                1,
            ),
        )))
        .with_ability(Effect::GainLife {
            amount: QuantityExpr::Fixed { value: 1 },
            player: TargetFilter::Controller,
        })
        .id();
    let mut runner = scenario.build();
    let spell_card = runner.state().objects[&spell].card_id;

    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id: spell_card,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("the bonus-dependent deferred cast must be announced");
    match runner.state().waiting_for.clone() {
        WaitingFor::PayCost {
            kind: PayCostKind::Sacrifice,
            choices,
            ..
        } => assert!(
            choices.contains(&fodder),
            "the fodder creature must be eligible for the deferred sacrifice"
        ),
        other => panic!("expected the deferred sacrifice choice, got {other:?}"),
    }

    runner
        .act(GameAction::SelectCards {
            cards: vec![fodder],
        })
        .expect("the deferred sacrifice selection must reach the mana payment");
    assert_single_color_echo_choice(&runner, &[ManaType::Green, ManaType::Blue]);
    assert!(
        runner.state().battlefield.contains(&fodder),
        "the sacrifice must not be committed before the echo choice settles"
    );
    assert!(
        runner
            .state()
            .stack
            .iter()
            .any(|entry| matches!(entry.kind, StackEntryKind::Spell { .. })),
        "the spell being cast remains on the stack while its echo choice is pending"
    );

    runner
        .act(GameAction::ChooseManaColor {
            choice: ManaChoice::SingleColor(ManaType::Green),
            count: 1,
        })
        .expect("answering the echo must finish the deferred payment");
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "the cast completes to priority after the echo choice"
    );
    assert_eq!(
        runner.state().objects[&fodder].zone,
        Zone::Graveyard,
        "the deferred sacrifice commits once the mana is paid"
    );
    assert_eq!(
        runner.state().objects[&spell].zone,
        Zone::Stack,
        "the paid spell reaches the stack"
    );
}

/// CR 118.12 + CR 605.3b + CR 616.1: Digsite Conservator's dies trigger offers
/// "you may pay {4}". With one floating colorless, Mirari's Wake, and one
/// untapped Simic Growth Chamber the live payment is affordable only because
/// the owed echo bonus is counted, so it must pause for the choice and resume
/// through its typed `EffectPayCost` root instead of failing at affordability.
#[test]
fn resolution_pay_cost_bonus_dependent_payment_pauses_for_echo() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_enchantment_from_oracle(P0, "Mirari's Wake", MIRARI_ECHO_ORACLE);
    scenario.add_land_from_oracle(P0, "Simic Growth Chamber", CHAMBER_ORACLE);
    let conservator = scenario
        .add_creature_from_oracle(P0, "Digsite Conservator", 2, 2, DIGSITE_ORACLE)
        .id();
    scenario.with_mana_pool(P0, floating_colorless(1));
    let mut runner = scenario.build();

    // Kill the Conservator through the engine's zone pipeline, then run the
    // ordinary trigger scan exactly as a post-action boundary would.
    let mut events = Vec::new();
    move_to_zone(
        runner.state_mut(),
        conservator,
        Zone::Graveyard,
        &mut events,
    );
    process_triggers(runner.state_mut(), &events);

    let mut offered = false;
    for _ in 0..20 {
        let waiting = runner.state().waiting_for.clone();
        match waiting {
            WaitingFor::ResolutionOptionalPaymentChoice { .. }
            | WaitingFor::OptionalEffectChoice { .. } => {
                offered = true;
                break;
            }
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("priority passes resolve the dies trigger");
            }
            other => panic!("unexpected state while resolving the dies trigger: {other:?}"),
        }
    }
    assert!(
        offered,
        "the dies trigger must reach its optional \"you may pay {{4}}\" prompt"
    );

    match runner.state().waiting_for.clone() {
        WaitingFor::ResolutionOptionalPaymentChoice { .. } => {
            runner
                .act(GameAction::ChooseResolutionOptionalPaymentBranch {
                    choice: ResolutionOptionalPaymentChoice::Pay { index: 0 },
                })
                .expect("the pay branch must resolve into the echo pause");
        }
        WaitingFor::OptionalEffectChoice { .. } => {
            runner
                .act(GameAction::DecideOptionalEffect { accept: true })
                .expect("accepting the optional payment must resolve into the echo pause");
        }
        other => unreachable!("loop established a payment prompt, got {other:?}"),
    }
    assert_single_color_echo_choice(&runner, &[ManaType::Green, ManaType::Blue]);

    let resumed = runner
        .act(GameAction::ChooseManaColor {
            choice: ManaChoice::SingleColor(ManaType::Blue),
            count: 1,
        })
        .expect("answering the echo resumes the effect-cost payment");
    assert_eq!(
        runner.state().players[P0.0 as usize].mana_pool.total(),
        0,
        "one colorless plus base {{G}}{{U}} plus one bonus exactly funds {{4}}"
    );
    assert!(
        !runner.state().cost_payment_failed_flag,
        "the resumed effect-cost payment must complete, not fall through to the failure flag"
    );
    assert!(
        resumed.events.iter().any(|event| matches!(
            event,
            GameEvent::EffectResolved {
                kind: engine::types::ability::EffectKind::Discover,
                ..
            }
        )),
        "the paid {{4}} must resolve the discover 4 rider, not only settle the echo"
    );
    assert_eq!(
        runner.state().objects[&conservator].zone,
        Zone::Graveyard,
        "the dies trigger's source stays in the graveyard through the payment"
    );
}

/// CR 118.12 + CR 605.3b + CR 616.1 + CR 608.2d: Miara's dies trigger is a
/// resolution-time `PayCost` whose cost is the composite `{1}` and 1 life. The
/// mana component is funded by a two-type land whose Mirari's Wake echo is
/// choice-owing, so payment pauses inside the composite. The typed
/// `EffectPayCost` root owns the complete unpaid suffix; the resumed composite
/// must pay the printed one life exactly once, never a generic continuation on
/// top of it (regression: that double charge cost 2 life for the printed 1).
#[test]
fn miara_composite_echo_pays_life_once_through_typed_root() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_enchantment_from_oracle(P0, "Mirari's Wake", MIRARI_ECHO_ORACLE);
    scenario.add_land_from_oracle(P0, "Simic Growth Chamber", CHAMBER_ORACLE);
    scenario.with_library_top(P0, &["Miara Draw"]);
    let miara = scenario
        .add_creature_from_oracle(P0, "Miara, Thorn of the Glade", 1, 2, MIARA_ORACLE)
        .id();
    let mut runner = scenario.build();
    let life_before = runner.state().players[P0.0 as usize].life;
    let library_before = runner.state().players[P0.0 as usize].library.len();

    // Miara's own death matches the trigger's `SelfRef` disjunct, so the
    // ordinary zone pipeline and trigger scan are the whole production path.
    let mut events = Vec::new();
    move_to_zone(runner.state_mut(), miara, Zone::Graveyard, &mut events);
    process_triggers(runner.state_mut(), &events);

    let mut offered = false;
    for _ in 0..20 {
        match runner.state().waiting_for.clone() {
            WaitingFor::ResolutionOptionalPaymentChoice { .. }
            | WaitingFor::OptionalEffectChoice { .. } => {
                offered = true;
                break;
            }
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("priority passes resolve Miara's dies trigger");
            }
            other => panic!("unexpected state while resolving Miara's trigger: {other:?}"),
        }
    }
    assert!(
        offered,
        "Miara's trigger must offer its optional \"{{1}} and 1 life\" payment"
    );

    match runner.state().waiting_for.clone() {
        WaitingFor::ResolutionOptionalPaymentChoice { .. } => {
            runner
                .act(GameAction::ChooseResolutionOptionalPaymentBranch {
                    choice: ResolutionOptionalPaymentChoice::Pay { index: 0 },
                })
                .expect("the pay branch must resolve into the echo pause");
        }
        WaitingFor::OptionalEffectChoice { .. } => {
            runner
                .act(GameAction::DecideOptionalEffect { accept: true })
                .expect("accepting the optional payment must resolve into the echo pause");
        }
        other => unreachable!("the loop established a payment prompt, got {other:?}"),
    }
    assert_single_color_echo_choice(&runner, &[ManaType::Green, ManaType::Blue]);
    assert_eq!(
        runner.state().players[P0.0 as usize].life,
        life_before,
        "the composite's life component waits for the echo choice"
    );

    let resumed = runner
        .act(GameAction::ChooseManaColor {
            choice: ManaChoice::SingleColor(ManaType::Blue),
            count: 1,
        })
        .expect("answering the echo resumes the composite payment");

    assert_eq!(
        runner.state().players[P0.0 as usize].life,
        life_before - 1,
        "the printed 1 life is paid exactly once by the resumed composite"
    );
    assert_eq!(
        runner.state().players[P0.0 as usize].library.len(),
        library_before - 1,
        "the conditional rider consumes exactly one library card"
    );
    assert_eq!(
        resumed
            .events
            .iter()
            .filter(|event| matches!(
                event,
                GameEvent::CardDrawn { player_id, .. } if *player_id == P0
            ))
            .count(),
        1,
        "the \"If you do\" rider draws exactly once"
    );
    assert_eq!(
        runner.state().players[P0.0 as usize].mana_pool.total(),
        2,
        "base {{G}}{{U}} plus the chosen bonus is three mana; the printed {{1}} spends one"
    );
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { player: P0 }
        ),
        "the completed trigger restores priority to its controller"
    );
    assert!(
        runner.state().stack.is_empty(),
        "the trigger leaves the stack after its payment completes"
    );
    assert!(
        !runner.state().withholds_priority(),
        "no payment or continuation carrier survives the completed payment"
    );
    assert!(
        !runner.state().cost_payment_failed_flag,
        "the resumed composite payment must not fall through to the failure flag"
    );
    assert_eq!(
        runner.state().objects[&miara].zone,
        Zone::Graveyard,
        "Miara remains in the graveyard through her own trigger"
    );
}

/// CR 605.4a + CR 608.2d: the echo-bonus preview is a search over aggregate
/// count vectors, not ordered assignments. Twenty-four identical two-type
/// firings must stay at 25 reachable states; the previous subset enumeration
/// explored 2^24 pool rebuilds and never finished.
#[test]
fn echo_bonus_preview_deduplicates_identical_firings() {
    const WAKE_COUNT: usize = 24;
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    for _ in 0..WAKE_COUNT {
        scenario.add_enchantment_from_oracle(P0, "Mirari's Wake", MIRARI_ECHO_ORACLE);
    }
    scenario.add_land_from_oracle(P0, "Simic Growth Chamber", CHAMBER_ORACLE);
    let mut runner = scenario.build();
    install_active_plane(&mut runner);
    // Base {G}{U} plus every one of the 24 owed bonuses is exactly {26}.
    runner
        .state_mut()
        .planar_die_actions_this_turn
        .insert(P0, 26);

    let started = Instant::now();
    assert!(
        can_roll_planar_die(runner.state(), P0),
        "{WAKE_COUNT} firings plus the land's base {{G}}{{U}} fund {{26}}"
    );
    runner
        .state_mut()
        .planar_die_actions_this_turn
        .insert(P0, 27);
    assert!(
        !can_roll_planar_die(runner.state(), P0),
        "the same board cannot fund {{27}}"
    );
    let elapsed = started.elapsed();
    assert!(
        elapsed < Duration::from_secs(5),
        "the preview must deduplicate aggregate bonus vectors; took {elapsed:?} \
         for {WAKE_COUNT} identical firings"
    );
}
