//! CR 601.2b + CR 118.9d: an alternative cost is offered when it is affordable
//! only after a matching Defiler's optional reduction.
//!
//! A Defiler ("As an additional cost to cast [color] permanent spells, you may
//! pay 2 life. Those spells cost {C} less to cast if you paid life this way.")
//! reduces the alternative cost being paid (CR 118.9d). The offer for an
//! alternative cost must count that reduction, as ordinary castability already
//! does; otherwise an alternative cost the player can afford is never offered.
//! Each test gives exactly enough mana for the REDUCED alternative cost, which
//! the printed cost can't be paid with even after the reduction.

use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::parser::oracle::parse_oracle_text;
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const DEFILER_OF_DREAMS: &str = "Flying\nAs an additional cost to cast blue permanent spells, you may pay 2 life. Those spells cost {U} less to cast if you paid life this way. This effect reduces only the amount of blue mana you pay.\nWhenever you cast a blue permanent spell, draw a card.";

const DEFILER_OF_INSTINCT: &str = "First strike\nAs an additional cost to cast red permanent spells, you may pay 2 life. Those spells cost {R} less to cast if you paid life this way. This effect reduces only the amount of red mana you pay.\nWhenever you cast a red permanent spell, this creature deals 1 damage to any target.";

const MULLDRIFTER: &str = "Flying\nWhen this creature enters, draw two cards.\nEvoke {2}{U} (You may cast this spell for its evoke cost. If you do, it's sacrificed when it enters.)";

const GOBLIN_HEELCUTTER: &str = "Whenever this creature attacks, target creature can't block this turn.\nDash {2}{R} (You may cast this spell for its dash cost. If you do, it gains haste, and it's returned from the battlefield to its owner's hand at the beginning of the next end step.)";

/// Put a Defiler on P0's battlefield with the statics its Oracle text parses to
/// (not its cast trigger, which would add an unrelated prompt).
fn add_defiler(scenario: &mut GameScenario, name: &str, oracle: &str, keyword: &str) {
    let parsed = parse_oracle_text(oracle, name, &[keyword.into()], &["Creature".into()], &[]);
    assert!(
        parsed
            .statics
            .iter()
            .any(|s| format!("{s:?}").contains("DefilerCostReduction")),
        "{name} must parse to a Defiler cost reduction, got {:?}",
        parsed.statics
    );
    let mut defiler = scenario.add_creature(P0, name, 3, 3);
    for s in parsed.statics {
        defiler.with_static_definition(s);
    }
}

fn add_colorless_mana(runner: &mut GameRunner, amount: usize) {
    for _ in 0..amount {
        runner.state_mut().players[0].mana_pool.add(ManaUnit::new(
            ManaType::Colorless,
            ObjectId(0),
            false,
            vec![],
        ));
    }
}

/// Cast `spell` from hand, accept the Defiler, and return the life paid.
fn cast_accepting_the_defiler(runner: &mut GameRunner, spell: ObjectId) -> i32 {
    let life_before = runner.state().players[0].life;
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("the alternative cost is affordable with the Defiler, so the cast is legal");
    // Positive reach guard: the Defiler choice is reached.
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::DefilerPayment { .. }
        ),
        "the matching Defiler must be offered, got {:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::DecideOptionalCost { pay: true })
        .expect("accepting the Defiler must complete the cast");
    life_before - runner.state().players[0].life
}

/// CR 702.74a + CR 118.9d: Mulldrifter's evoke {2}{U} less the Defiler's {U} is
/// {2}. With 2 colorless mana, evoke is affordable only with the Defiler, and
/// the printed {4}{U} is not affordable even with it.
#[test]
fn evoke_affordable_only_with_a_defiler_is_offered() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    add_defiler(
        &mut scenario,
        "Defiler of Dreams",
        DEFILER_OF_DREAMS,
        "Flying",
    );
    let mulldrifter = scenario
        .add_creature_to_hand_from_oracle(P0, "Mulldrifter", 2, 2, MULLDRIFTER)
        .with_mana_cost(ManaCost::Cost {
            generic: 4,
            shards: vec![ManaCostShard::Blue],
        })
        .with_color(vec![ManaColor::Blue])
        .id();
    let mut runner = scenario.build();
    add_colorless_mana(&mut runner, 2);

    let life_paid = cast_accepting_the_defiler(&mut runner, mulldrifter);

    assert_eq!(runner.state().objects[&mulldrifter].zone, Zone::Stack);
    assert_eq!(life_paid, 2, "the Defiler's 2 life must be paid");
    assert_eq!(
        runner.state().players[0].mana_pool.total(),
        0,
        "evoke {{2}}{{U}} less {{U}} costs exactly the 2 mana available"
    );
}

/// CR 702.109a + CR 118.9d: Goblin Heelcutter's dash {2}{R} less the Defiler's
/// {R} is {2}. With 2 colorless mana, dash is affordable only with the Defiler,
/// and the printed {3}{R} is not affordable even with it.
#[test]
fn dash_affordable_only_with_a_defiler_is_offered() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    add_defiler(
        &mut scenario,
        "Defiler of Instinct",
        DEFILER_OF_INSTINCT,
        "First strike",
    );
    let heelcutter = scenario
        .add_creature_to_hand_from_oracle(P0, "Goblin Heelcutter", 3, 2, GOBLIN_HEELCUTTER)
        .with_mana_cost(ManaCost::Cost {
            generic: 3,
            shards: vec![ManaCostShard::Red],
        })
        .with_color(vec![ManaColor::Red])
        .id();
    let mut runner = scenario.build();
    add_colorless_mana(&mut runner, 2);

    let life_paid = cast_accepting_the_defiler(&mut runner, heelcutter);

    assert_eq!(runner.state().objects[&heelcutter].zone, Zone::Stack);
    assert_eq!(life_paid, 2, "the Defiler's 2 life must be paid");
    assert_eq!(
        runner.state().players[0].mana_pool.total(),
        0,
        "dash {{2}}{{R}} less {{R}} costs exactly the 2 mana available"
    );
}

const DEFILER_OF_FLESH: &str = "Menace\nAs an additional cost to cast black permanent spells, you may pay 2 life. Those spells cost {B} less to cast if you paid life this way. This effect reduces only the amount of black mana you pay.\nWhenever you cast a black permanent spell, target creature you control gets +1/+1 and gains menace until end of turn.";

const TENACIOUS_UNDERDOG: &str = "Blitz\u{2014}{2}{B}{B}, Pay 2 life. (If you cast this spell for its blitz cost, it gains haste and \"When this creature dies, draw a card.\" Sacrifice it at the beginning of the next end step.)\nYou may cast this card from your graveyard using its blitz ability.";

/// Tenacious Underdog in the graveyard (blitz via its own rider), Defiler of
/// Flesh out, four black mana, and P0 at `life`.
fn underdog_with_defiler(life: i32) -> (GameRunner, ObjectId) {
    underdog_with_defiler_and_mana(life, 4)
}

/// `underdog_with_defiler` with exactly `black` black mana.
fn underdog_with_defiler_and_mana(life: i32, black: usize) -> (GameRunner, ObjectId) {
    let parsed = parse_oracle_text(
        TENACIOUS_UNDERDOG,
        "Tenacious Underdog",
        &[],
        &["Creature".into()],
        &["Human".into(), "Warrior".into()],
    );
    let blitz = parsed
        .extracted_keywords
        .iter()
        .find(|k| matches!(k, engine::types::keywords::Keyword::Blitz(_)))
        .expect("blitz keyword must be extracted")
        .clone();
    let own_rider = parsed
        .statics
        .first()
        .expect("graveyard-cast permission static must parse")
        .clone();
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    add_defiler(
        &mut scenario,
        "Defiler of Flesh",
        DEFILER_OF_FLESH,
        "Menace",
    );
    let underdog = scenario
        .add_creature_to_graveyard(P0, "Tenacious Underdog", 3, 2)
        .with_static_definition(own_rider)
        .with_mana_cost(ManaCost::Cost {
            generic: 1,
            shards: vec![ManaCostShard::Black],
        })
        .with_keyword(blitz)
        .with_color(vec![ManaColor::Black])
        .id();
    let mut runner = scenario.build();
    runner.state_mut().players[0].life = life;
    for _ in 0..black {
        runner.state_mut().players[0].mana_pool.add(ManaUnit::new(
            ManaType::Black,
            ObjectId(0),
            false,
            vec![],
        ));
    }
    (runner, underdog)
}

fn cast_underdog(runner: &mut GameRunner, underdog: ObjectId) {
    let card_id = runner.state().objects[&underdog].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: underdog,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("the graveyard blitz cast is legal");
}

/// CR 601.2h + CR 119.4: at 3 life, blitz's "Pay 2 life" and the Defiler's 2
/// life together (4) can't both be paid, and partial payments aren't allowed,
/// so the Defiler must not be offered. The cast still completes, paying the
/// blitz life on its own.
#[test]
fn defiler_is_not_offered_when_its_life_and_the_residuals_exceed_the_life_total() {
    let (mut runner, underdog) = underdog_with_defiler(3);
    cast_underdog(&mut runner, underdog);

    assert!(
        !matches!(
            runner.state().waiting_for,
            WaitingFor::DefilerPayment { .. }
        ),
        "4 life can't be paid from 3, so the Defiler must not be offered"
    );
    assert_eq!(runner.state().objects[&underdog].zone, Zone::Stack);
    assert_eq!(
        runner.state().players[0].life,
        1,
        "only blitz's 2 life is paid"
    );
}

/// Positive control: at 4 life the combined 4 life is payable, so the Defiler
/// is offered, and accepting it completes the cast: both life payments are
/// made and the Defiler removes one {B} of the blitz mana.
#[test]
fn defiler_is_offered_when_its_life_and_the_residuals_are_payable() {
    let (mut runner, underdog) = underdog_with_defiler(4);
    cast_underdog(&mut runner, underdog);

    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::DefilerPayment { .. }
        ),
        "4 life is payable from 4, so the Defiler must be offered, got {:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::DecideOptionalCost { pay: true })
        .expect("paying the Defiler's life must be legal");

    // The cast completed: both life payments were made (4 - 2 - 2) and the
    // Defiler removed one {B}, so 3 of the 4 mana was spent. Paying down to 0
    // life is legal (CR 119.4); CR 704.5a then ends the game at the next
    // state-based-action check, which is how the completion is observed.
    assert_eq!(
        runner.state().players[0].life,
        0,
        "the Defiler's 2 life and blitz's 2 life are both paid"
    );
    assert_eq!(
        runner.state().players[0].mana_pool.total(),
        1,
        "{{2}}{{B}}{{B}} less the Defiler's {{B}} is 3 of the 4 mana"
    );
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::GameOver {
                winner: Some(engine::types::player::PlayerId(1))
            }
        ),
        "the completed cast leaves P0 at 0 life, got {:?}",
        runner.state().waiting_for
    );
}

fn underdog_offered(runner: &GameRunner, underdog: ObjectId) -> bool {
    engine::ai_support::legal_actions(runner.state())
        .iter()
        .any(|action| matches!(action, GameAction::CastSpell { object_id, .. } if *object_id == underdog))
}

/// CR 601.2h + CR 119.4: with three black mana, blitz ({2}{B}{B}, Pay 2 life) is
/// affordable only with Defiler of Flesh's {B} reduction, which costs 2 more
/// life. At 3 life the combined 4 life can't be paid, so the reduction can't be
/// taken, the unreduced four mana can't be paid, and the cast must not be
/// offered or accepted. The offer reads the same combined-life eligibility the
/// Defiler payment prompt applies.
#[test]
fn blitz_affordable_only_with_a_defiler_whose_life_is_unpayable_is_not_offered() {
    let (mut runner, underdog) = underdog_with_defiler_and_mana(3, 3);
    assert!(
        !underdog_offered(&runner, underdog),
        "3 life can't pay the Defiler's 2 and blitz's 2 together"
    );
    let card_id = runner.state().objects[&underdog].card_id;
    assert!(
        runner
            .act(GameAction::CastSpell {
                object_id: underdog,
                card_id,
                targets: vec![],
                payment_mode: CastPaymentMode::Auto,
            })
            .is_err(),
        "the cast handler must refuse the same cast"
    );
    assert_eq!(runner.state().objects[&underdog].zone, Zone::Graveyard);
    assert_eq!(runner.state().players[0].life, 3);
}

/// Control: at 4 life the combined 4 life is payable, so the same three-mana
/// blitz is offered, and the offered action completes through the Defiler: all
/// three mana spent and both life payments made.
#[test]
fn blitz_affordable_only_with_a_payable_defiler_is_offered_and_completes() {
    let (mut runner, underdog) = underdog_with_defiler_and_mana(4, 3);
    let action = engine::ai_support::legal_actions(runner.state())
        .into_iter()
        .find(|action| matches!(action, GameAction::CastSpell { object_id, .. } if *object_id == underdog))
        .expect("4 life pays the Defiler's 2 and blitz's 2, so the cast is offered");
    runner.act(action).expect("the offered cast is accepted");
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::DefilerPayment { .. }
        ),
        "the Defiler must be offered, got {:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::DecideOptionalCost { pay: true })
        .expect("paying the Defiler's life must be legal");
    assert_eq!(runner.state().players[0].life, 0, "both life payments made");
    assert_eq!(
        runner.state().players[0].mana_pool.total(),
        0,
        "the reduced {{2}}{{B}} took all three mana"
    );
}

/// Tenacious Underdog with Defiler of Flesh, `black` black mana, P0 at `life`,
/// and an opponent's unconditional "pay 2 life" casting tax. No printed card
/// imposes one (the only life-tax imposer, Terror of the Peaks, is
/// target-gated), so the tax is Terror's own parsed static with its target
/// condition cleared.
fn underdog_with_defiler_and_a_life_tax(life: i32, black: usize) -> (GameRunner, ObjectId) {
    let (mut runner, underdog) = underdog_with_defiler_and_mana(life, black);
    let mut parsed = parse_oracle_text(
        "Spells your opponents cast that target Tax Source cost an additional 3 life to cast.",
        "Tax Source",
        &[],
        &["Creature".into()],
        &[],
    );
    for s in parsed.statics.iter_mut() {
        if let engine::types::statics::StaticMode::ImposeAdditionalCost {
            cost, spell_filter, ..
        } = &mut s.mode
        {
            *spell_filter = None;
            *cost = engine::types::ability::AbilityCost::PayLife {
                amount: engine::types::ability::QuantityExpr::Fixed { value: 2 },
            };
        }
    }
    assert!(
        parsed
            .statics
            .iter()
            .any(|s| format!("{s:?}").contains("ImposeAdditionalCost")),
        "reach guard: the tax static parses"
    );
    let card_id = engine::types::identifiers::CardId(runner.state().next_object_id);
    let tax = engine::game::zones::create_object(
        runner.state_mut(),
        card_id,
        engine::game::scenario::P1,
        "Tax Source".to_string(),
        Zone::Battlefield,
    );
    {
        let obj = runner.state_mut().objects.get_mut(&tax).unwrap();
        obj.card_types
            .core_types
            .push(engine::types::card_type::CoreType::Creature);
        obj.base_card_types = obj.card_types.clone();
        for s in parsed.statics {
            obj.static_definitions.push(s.clone());
            std::sync::Arc::make_mut(&mut obj.base_static_definitions).push(s);
        }
    }
    engine::game::layers::flush_layers(runner.state_mut());
    (runner, underdog)
}

/// CR 601.2h + CR 119.4: the offer prices every imposed cost the payment does.
/// At 4 life with three black mana, the reduced blitz needs the Defiler's 2,
/// blitz's 2 and the tax's 2 (6 life): not payable, so not offered, and the
/// handler refuses the same cast. Before, the offer left the tax out.
#[test]
fn blitz_with_a_defiler_and_an_imposed_life_tax_is_not_offered_when_unpayable() {
    let (mut runner, underdog) = underdog_with_defiler_and_a_life_tax(4, 3);
    assert!(!underdog_offered(&runner, underdog));
    assert!(
        engine::game::casting::current_casting_variant_choice_options(runner.state(), P0, underdog)
            .is_empty(),
        "the engine's casting options must not include the unpayable blitz"
    );
    let card_id = runner.state().objects[&underdog].card_id;
    assert!(runner
        .act(GameAction::CastSpell {
            object_id: underdog,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .is_err());
    assert_eq!(runner.state().objects[&underdog].zone, Zone::Graveyard);
}

/// Control: at 6 life the 6 life is payable, so the cast is offered and the
/// offered action completes through the Defiler (three mana, all 6 life).
#[test]
fn blitz_with_a_defiler_and_an_imposed_life_tax_completes_when_payable() {
    let (mut runner, underdog) = underdog_with_defiler_and_a_life_tax(6, 3);
    let action = engine::ai_support::legal_actions(runner.state())
        .into_iter()
        .find(|action| matches!(action, GameAction::CastSpell { object_id, .. } if *object_id == underdog))
        .expect("6 life pays the Defiler, blitz and the tax");
    runner.act(action).expect("the offered cast is accepted");
    if let WaitingFor::DefilerPayment { .. } = runner.state().waiting_for {
        runner
            .act(GameAction::DecideOptionalCost { pay: true })
            .expect("paying the Defiler's life");
    }
    assert_eq!(runner.state().players[0].life, 0, "all 6 life paid");
    assert_eq!(runner.state().players[0].mana_pool.total(), 0);
}
