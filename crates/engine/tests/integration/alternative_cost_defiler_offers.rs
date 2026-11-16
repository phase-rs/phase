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

/// Put a Defiler on P0's battlefield, with its printed mana cost and colour and
/// the statics its Oracle text parses to (not its cast trigger, which would add
/// an unrelated prompt).
fn add_defiler(
    scenario: &mut GameScenario,
    name: &str,
    oracle: &str,
    keyword: &str,
    mana_cost: ManaCost,
    color: ManaColor,
) -> ObjectId {
    let parsed = parse_oracle_text(oracle, name, &[keyword.into()], &["Creature".into()], &[]);
    assert!(
        parsed
            .statics
            .iter()
            .any(|s| format!("{s:?}").contains("DefilerCostReduction")),
        "{name} must parse to a Defiler cost reduction, got {:?}",
        parsed.statics
    );
    let mut defiler = scenario.add_creature(P0, name, 4, 4);
    defiler.with_mana_cost(mana_cost).with_color(vec![color]);
    for s in parsed.statics {
        defiler.with_static_definition(s);
    }
    defiler.id()
}

/// Defiler of Dreams, {3}{U}{U}.
fn add_defiler_of_dreams(scenario: &mut GameScenario) -> ObjectId {
    add_defiler(
        scenario,
        "Defiler of Dreams",
        DEFILER_OF_DREAMS,
        "Flying",
        ManaCost::Cost {
            generic: 3,
            shards: vec![ManaCostShard::Blue, ManaCostShard::Blue],
        },
        ManaColor::Blue,
    )
}

/// Defiler of Flesh, {2}{B}{B}.
fn add_defiler_of_flesh(scenario: &mut GameScenario) -> ObjectId {
    add_defiler(
        scenario,
        "Defiler of Flesh",
        DEFILER_OF_FLESH,
        "Menace",
        ManaCost::Cost {
            generic: 2,
            shards: vec![ManaCostShard::Black, ManaCostShard::Black],
        },
        ManaColor::Black,
    )
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
        .act(GameAction::DecideOptionalCost { index: 0 })
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
    add_defiler_of_dreams(&mut scenario);

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
        ManaCost::Cost {
            generic: 2,
            shards: vec![ManaCostShard::Red, ManaCostShard::Red],
        },
        ManaColor::Red,
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
    add_defiler_of_flesh(&mut scenario);

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
        .act(GameAction::DecideOptionalCost { index: 0 })
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
        .act(GameAction::DecideOptionalCost { index: 0 })
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
            .act(GameAction::DecideOptionalCost { index: 0 })
            .expect("paying the Defiler's life");
    }
    assert_eq!(runner.state().players[0].life, 0, "all 6 life paid");
    assert_eq!(runner.state().players[0].mana_pool.total(), 0);
}

// ---------------------------------------------------------------------------
// #9781: Spectacle, Emerge, Prowl, Prototype and More Than Meets the Eye offers
// and the printed-cost side, priced as the cost lock prices them (CR 601.2f:
// reductions, a Defiler's included, then the floor).
// ---------------------------------------------------------------------------

use engine::types::actions::AlternativeCastDecision;
use engine::types::keywords::Keyword;

const SPAWN_OF_MAYHEM: &str = "Spectacle {1}{B}{B} (You may cast this spell for its spectacle cost rather than its mana cost if an opponent lost life this turn.)\nFlying, trample\nAt the beginning of your upkeep, this creature deals 1 damage to each player. Then if you have 10 or less life, put a +1/+1 counter on this creature.";

const CRESTING_MOSASAURUS: &str = "Emerge {6}{U} (You may cast this spell by sacrificing a creature and paying the emerge cost reduced by that creature's mana value.)\nWhen this creature enters, if you cast it, return each non-Dinosaur creature to its owner's hand.";

const EARWIG_SQUAD: &str = "Prowl {2}{B} (You may cast this for its prowl cost if you dealt combat damage to a player this turn with a Goblin or Rogue.)\nWhen this creature enters, if its prowl cost was paid, search target opponent's library for three cards and exile them. Then that player shuffles.";

const TRINISPHERE: &str = "As long as this artifact is untapped, each spell that would cost less than three mana to cast costs three mana to cast. (Additional mana in the cost may be paid with any color of mana or colorless mana. For example, a spell that would cost {1}{B} to cast costs {2}{B} to cast instead.)";

const PHYREXIAN_FLESHGORGER: &str = "Prototype {1}{B}{B} \u{2014} 3/3 (You may cast this spell with different mana cost, color, and size. It keeps its abilities and types.)\nMenace, lifelink\nWard\u{2014}Pay life equal to this creature's power.";

const SPIKEWHEEL_ACROBAT: &str = "Spectacle {2}{R} (You may cast this spell for its spectacle cost rather than its mana cost if an opponent lost life this turn.)";

const WORLD_WAR_HULK: &str = "(As this Saga enters and after your draw step, add a lore counter. Sacrifice after III.)\nI \u{2014} The next red or green creature spell you cast this turn can be cast without paying its mana cost.\nII \u{2014} Put three +1/+1 counters on target creature you control.\nIII \u{2014} Choose target creature you control. Until end of turn, double its power and toughness and it gains trample.";

const HEARTLESS_SUMMONING: &str =
    "Creature spells you cast cost {2} less to cast.\nCreatures you control get -1/-1.";

fn pool_of(ty: ManaType, n: usize) -> Vec<ManaUnit> {
    (0..n)
        .map(|_| ManaUnit::new(ty, ObjectId(0), false, vec![]))
        .collect()
}

fn cost(generic: u32, shards: &[ManaCostShard]) -> ManaCost {
    ManaCost::Cost {
        generic,
        shards: shards.to_vec(),
    }
}

/// How a cast of a spell from hand answers its prompts.
#[derive(Default)]
struct Answers {
    /// `Some` answers an `AlternativeCastChoice`; `None` treats one as a stop.
    alternative: Option<AlternativeCastDecision>,
    /// The permanent a `PayCost` selection (an emerge sacrifice) picks.
    sacrifice: Option<ObjectId>,
}

/// How a cast driven by [`drive_cast`] ended.
#[derive(Debug)]
enum CastOutcome {
    /// The completion boundary: the spell is on the stack and P0 holds
    /// priority (CR 601.2i). Carries the prompts answered on the way.
    Completed(Vec<&'static str>),
    /// The reducer refused an action, which changes nothing: `CastSpell` itself
    /// (`prompt: None`) or the answer to `prompt`.
    Refused {
        prompt: Option<&'static str>,
        error: String,
        prompts: Vec<&'static str>,
    },
    /// The cast is still live at a prompt the answers don't cover.
    Stopped {
        waiting: String,
        prompts: Vec<&'static str>,
    },
}

/// Cast `spell`, accept the Defiler, and answer the rest from `answers`.
fn drive_cast(runner: &mut GameRunner, spell: ObjectId, answers: &Answers) -> CastOutcome {
    let card_id = runner.state().objects[&spell].card_id;
    if let Err(e) = runner.act(GameAction::CastSpell {
        object_id: spell,
        card_id,
        targets: vec![],
        payment_mode: CastPaymentMode::Auto,
    }) {
        return CastOutcome::Refused {
            prompt: None,
            error: format!("{e:?}"),
            prompts: Vec::new(),
        };
    }
    let mut prompts = Vec::new();
    for _ in 0..8 {
        let stopped = |runner: &GameRunner, prompts: &Vec<&'static str>| CastOutcome::Stopped {
            waiting: format!("{:?}", runner.state().waiting_for),
            prompts: prompts.clone(),
        };
        let next = match &runner.state().waiting_for {
            WaitingFor::Priority { .. } if runner.state().objects[&spell].zone == Zone::Stack => {
                return CastOutcome::Completed(prompts);
            }
            WaitingFor::AlternativeCastChoice { .. } => match answers.alternative {
                Some(choice) => GameAction::ChooseAlternativeCast { choice },
                None => return stopped(runner, &prompts),
            },
            WaitingFor::DefilerPayment { .. } => GameAction::DecideOptionalCost { index: 0 },
            WaitingFor::PayCost { .. } => match answers.sacrifice {
                Some(id) => GameAction::SelectCards { cards: vec![id] },
                None => return stopped(runner, &prompts),
            },
            WaitingFor::ManaPayment { .. } => GameAction::PassPriority,
            _ => return stopped(runner, &prompts),
        };
        let prompt = match &runner.state().waiting_for {
            WaitingFor::AlternativeCastChoice { .. } => "AlternativeCastChoice",
            _ => runner.waiting_for_kind(),
        };
        if let Err(e) = runner.act(next) {
            return CastOutcome::Refused {
                prompt: Some(prompt),
                error: format!("{e:?}"),
                prompts,
            };
        }
        prompts.push(prompt);
    }
    let waiting = format!("{:?}", runner.state().waiting_for);
    CastOutcome::Stopped { waiting, prompts }
}

/// [`drive_cast`] for a cast expected to complete: the prompts answered, or
/// how it ended otherwise.
fn cast_with_defiler(
    runner: &mut GameRunner,
    spell: ObjectId,
    answers: &Answers,
) -> Result<Vec<&'static str>, String> {
    match drive_cast(runner, spell, answers) {
        CastOutcome::Completed(prompts) => Ok(prompts),
        CastOutcome::Stopped { waiting, prompts } => {
            Err(format!("stopped at {waiting} after {prompts:?}"))
        }
        refused => Err(format!("{refused:?}")),
    }
}

fn offered(runner: &GameRunner, spell: ObjectId) -> bool {
    engine::ai_support::legal_actions(runner.state())
        .iter()
        .any(|a| matches!(a, GameAction::CastSpell { object_id, .. } if *object_id == spell))
}

/// CR 601.2h: the cast is not a legal action, and the reducer refuses it:
/// `CastSpell` itself, or, once the matching Defiler is offered, both of its
/// answers (no total either way can be paid). Each refused action changes
/// nothing: the spell stays in hand and no life or mana is paid. A cast left
/// waiting at any other prompt fails.
fn assert_refused(runner: &mut GameRunner, spell: ObjectId) {
    assert!(
        !offered(runner, spell),
        "the cast must not be a legal action"
    );
    let life = runner.state().players[0].life;
    let pool = runner.state().players[0].mana_pool.total();
    match drive_cast(runner, spell, &Answers::default()) {
        CastOutcome::Refused { prompt: None, .. } => assert!(
            matches!(runner.state().waiting_for, WaitingFor::Priority { player } if player == P0),
            "a refused cast leaves P0 with priority, got {:?}",
            runner.state().waiting_for
        ),
        CastOutcome::Refused {
            prompt: Some("DefilerPayment"),
            error,
            prompts,
        } => {
            assert!(prompts.is_empty(), "{prompts:?}");
            assert!(
                error.contains("Cannot pay mana cost"),
                "accepting is refused as unpayable: {error}"
            );
            assert_eq!(runner.waiting_for_kind(), "DefilerPayment");
            let declined = runner.act(GameAction::DecideOptionalCost { index: 1 });
            assert!(declined.is_err(), "declining is refused too: {declined:?}");
            assert_eq!(runner.waiting_for_kind(), "DefilerPayment");
        }
        other => panic!("the reducer must refuse the cast, got {other:?}"),
    }
    assert_eq!(runner.state().objects[&spell].zone, Zone::Hand);
    assert_eq!(runner.state().players[0].life, life, "no life paid");
    assert_eq!(
        runner.state().players[0].mana_pool.total(),
        pool,
        "no mana paid"
    );
}
/// An untapped Trinisphere under P1's control, its floor parsed from Oracle text.
fn add_trinisphere(scenario: &mut GameScenario) {
    let parsed = parse_oracle_text(TRINISPHERE, "Trinisphere", &[], &["Artifact".into()], &[]);
    assert!(
        parsed
            .statics
            .iter()
            .any(|s| format!("{s:?}").contains("Minimum")),
        "reach: Trinisphere parses to a cost floor, got {:?}",
        parsed.statics
    );
    let mut trinisphere = scenario.add_creature(engine::game::scenario::P1, "Trinisphere", 0, 1);
    trinisphere.as_artifact();
    for s in parsed.statics {
        trinisphere.with_static_definition(s);
    }
}

/// Defiler of Flesh, Spawn of Mayhem in hand, `black` black mana, and an
/// opponent who lost life this turn (CR 702.137a's spectacle gate).
fn spawn_with_defiler(black: usize, trinisphere: bool) -> (GameRunner, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    add_defiler_of_flesh(&mut scenario);
    if trinisphere {
        add_trinisphere(&mut scenario);
    }
    let spawn = scenario
        .add_creature_to_hand(P0, "Spawn of Mayhem", 4, 4)
        .from_oracle_text_with_keywords(&["Spectacle", "Flying", "Trample"], SPAWN_OF_MAYHEM)
        .with_mana_cost(cost(2, &[ManaCostShard::Black, ManaCostShard::Black]))
        .with_color(vec![ManaColor::Black])
        .id();
    scenario.with_mana_pool(P0, pool_of(ManaType::Black, black));
    let mut runner = scenario.build();
    runner.state_mut().players[1].life_lost_this_turn = 1;
    assert!(
        runner.state().objects[&spawn]
            .keywords
            .iter()
            .any(|k| matches!(k, Keyword::Spectacle(_))),
        "reach: Spawn of Mayhem carries Spectacle"
    );
    (runner, spawn)
}

/// CR 702.137a + CR 118.9d: spectacle {1}{B}{B} less the Defiler's {B} is {1}{B}.
/// With {B}{B} the printed {2}{B}{B} (even reduced to {2}{B}) is unaffordable, so
/// the spectacle cast is the only one, taken for {B}{B} and 2 life.
#[test]
fn spectacle_affordable_only_with_a_defiler_is_offered_and_completes() {
    let (mut runner, spawn) = spawn_with_defiler(2, false);
    let life_before = runner.state().players[0].life;
    assert!(
        offered(&runner, spawn),
        "the spectacle cast is a legal action"
    );

    let result = cast_with_defiler(&mut runner, spawn, &Answers::default());

    assert_eq!(result, Ok(vec!["DefilerPayment"]));
    assert_eq!(runner.state().players[0].life, life_before - 2);
    assert_eq!(
        runner.state().players[0].mana_pool.total(),
        0,
        "{{1}}{{B}}{{B}} less {{B}} takes both black mana"
    );
}

/// CR 118.9b: alternative costs are optional. With {B}{B}{B} the printed
/// {2}{B}{B} is affordable only with the Defiler ({2}{B}) and spectacle
/// {1}{B}{B} is affordable outright, so the player chooses; choosing the printed
/// cost completes through the Defiler.
#[test]
fn printed_cost_affordable_only_with_a_defiler_keeps_the_spectacle_choice() {
    let (mut runner, spawn) = spawn_with_defiler(3, false);
    let life_before = runner.state().players[0].life;

    let result = cast_with_defiler(
        &mut runner,
        spawn,
        &Answers {
            alternative: Some(AlternativeCastDecision::Normal),
            ..Answers::default()
        },
    );

    assert_eq!(
        result,
        Ok(vec!["AlternativeCastChoice", "DefilerPayment"]),
        "both casts are payable, so the player must be asked"
    );
    assert_eq!(runner.state().players[0].life, life_before - 2);
    assert_eq!(
        runner.state().players[0].mana_pool.total(),
        0,
        "{{2}}{{B}}{{B}} less {{B}} takes all three black mana"
    );
}

/// CR 601.2f: Trinisphere's floor applies after every reduction. Spectacle
/// {1}{B}{B} less the Defiler's {B} is {1}{B}, floored to {2}{B}; the printed
/// {2}{B}{B} less {B} is {2}{B}. Neither can be paid with {B}{B}.
#[test]
fn spectacle_with_a_defiler_under_trinisphere_is_not_offered_for_two_mana() {
    let (mut runner, spawn) = spawn_with_defiler(2, true);
    assert_refused(&mut runner, spawn);
}

/// Control: with {B}{B}{B} both floored totals are payable; the spectacle cast
/// with the Defiler takes all three mana.
#[test]
fn spectacle_with_a_defiler_under_trinisphere_pays_the_floor() {
    let (mut runner, spawn) = spawn_with_defiler(3, true);
    let life_before = runner.state().players[0].life;

    let result = cast_with_defiler(
        &mut runner,
        spawn,
        &Answers {
            alternative: Some(AlternativeCastDecision::Alternative),
            ..Answers::default()
        },
    );

    assert_eq!(result, Ok(vec!["AlternativeCastChoice", "DefilerPayment"]));
    assert_eq!(runner.state().players[0].life, life_before - 2);
    assert_eq!(runner.state().players[0].mana_pool.total(), 0);
}

/// Cresting Mosasaurus in hand with Defiler of Dreams, `fodder` (name, mana
/// cost) on P0's battlefield, `colorless` colourless mana and optionally an
/// untapped Trinisphere.
fn mosasaurus_board(
    fodder: Option<(&str, ManaCost)>,
    colorless: usize,
    trinisphere: bool,
) -> (GameRunner, ObjectId, ObjectId, Option<ObjectId>) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let defiler = add_defiler_of_dreams(&mut scenario);
    if trinisphere {
        add_trinisphere(&mut scenario);
    }
    let fodder = fodder.map(|(name, mana_cost)| {
        scenario
            .add_creature(P0, name, 2, 2)
            .with_mana_cost(mana_cost)
            .id()
    });
    let mosasaurus = scenario
        .add_creature_to_hand_from_oracle(P0, "Cresting Mosasaurus", 4, 8, CRESTING_MOSASAURUS)
        .with_subtypes(vec!["Dinosaur"])
        .with_mana_cost(cost(6, &[ManaCostShard::Blue, ManaCostShard::Blue]))
        .with_color(vec![ManaColor::Blue])
        .id();
    scenario.with_mana_pool(P0, pool_of(ManaType::Colorless, colorless));
    let runner = scenario.build();
    assert!(
        runner.state().objects[&mosasaurus]
            .keywords
            .iter()
            .any(|k| matches!(k, Keyword::Emerge(_))),
        "reach: Cresting Mosasaurus carries Emerge"
    );
    (runner, mosasaurus, defiler, fodder)
}

/// CR 702.119a + CR 118.9d: emerge {6}{U} less the sacrificed mana value 2 is
/// {4}{U}; less the Defiler's {U} it is {4}. With four colourless mana the emerge
/// cast is the only one, taken for the sacrifice, {4} and 2 life.
#[test]
fn emerge_affordable_only_with_a_defiler_is_offered_and_completes() {
    let (mut runner, mosasaurus, _, bears) = mosasaurus_board(
        Some(("Grizzly Bears", cost(1, &[ManaCostShard::Green]))),
        4,
        false,
    );
    let bears = bears.unwrap();
    assert!(
        offered(&runner, mosasaurus),
        "the emerge cast is a legal action"
    );
    let life_before = runner.state().players[0].life;

    let result = cast_with_defiler(
        &mut runner,
        mosasaurus,
        &Answers {
            sacrifice: Some(bears),
            ..Answers::default()
        },
    );

    assert!(result.is_ok(), "the emerge cast completes: {result:?}");
    assert_eq!(runner.state().objects[&bears].zone, Zone::Graveyard);
    assert_eq!(runner.state().players[0].life, life_before - 2);
    assert_eq!(runner.state().players[0].mana_pool.total(), 0, "{{4}} paid");
}

/// CR 702.119c + CR 601.2f: the permanent to sacrifice is chosen as the emerge
/// cost is announced and sacrificed as the total is paid, after the total is
/// locked, so a sacrificed Defiler's own reduction still applies. Emerge {6}{U}
/// less Defiler of Dreams' mana value 5, less its {U}, is {1}.
#[test]
fn emerge_sacrificing_the_defiler_keeps_its_reduction() {
    let (mut runner, mosasaurus, defiler, _) = mosasaurus_board(None, 1, false);
    assert_eq!(
        runner.state().objects[&defiler].effective_mana_value(),
        5,
        "reach: Defiler of Dreams has mana value 5"
    );
    assert!(
        offered(&runner, mosasaurus),
        "the emerge cast is a legal action"
    );
    let life_before = runner.state().players[0].life;

    let result = cast_with_defiler(
        &mut runner,
        mosasaurus,
        &Answers {
            sacrifice: Some(defiler),
            ..Answers::default()
        },
    );

    assert!(result.is_ok(), "the emerge cast completes: {result:?}");
    assert_eq!(runner.state().objects[&defiler].zone, Zone::Graveyard);
    assert_eq!(runner.state().players[0].life, life_before - 2);
    assert_eq!(runner.state().players[0].mana_pool.total(), 0, "{{1}} paid");
}

/// CR 601.2f: emerge {6}{U} less a mana value 4 sacrifice, less the Defiler's
/// {U}, is {2}; Trinisphere's floor, applied last, makes it {3}. Two mana can't
/// pay it.
#[test]
fn emerge_with_a_defiler_under_trinisphere_is_not_offered_for_two_mana() {
    let (mut runner, mosasaurus, _, _) = mosasaurus_board(
        Some(("Hill Giant", cost(3, &[ManaCostShard::Red]))),
        2,
        true,
    );
    assert_refused(&mut runner, mosasaurus);
}

/// Control: with three mana the floored {3} is paid exactly.
#[test]
fn emerge_with_a_defiler_under_trinisphere_pays_exactly_the_floor() {
    let (mut runner, mosasaurus, _, giant) = mosasaurus_board(
        Some(("Hill Giant", cost(3, &[ManaCostShard::Red]))),
        3,
        true,
    );
    let giant = giant.unwrap();
    assert!(
        offered(&runner, mosasaurus),
        "the emerge cast is a legal action"
    );
    let life_before = runner.state().players[0].life;

    let result = cast_with_defiler(
        &mut runner,
        mosasaurus,
        &Answers {
            sacrifice: Some(giant),
            ..Answers::default()
        },
    );

    assert!(result.is_ok(), "the emerge cast completes: {result:?}");
    assert_eq!(runner.state().objects[&giant].zone, Zone::Graveyard);
    assert_eq!(runner.state().players[0].life, life_before - 2);
    assert_eq!(
        runner.state().players[0].mana_pool.total(),
        0,
        "the floored {{3}} takes all three mana"
    );
}

const CRABOMINATION: &str = "Emerge from artifact {5}{B}{B} (You may cast this spell by sacrificing an artifact and paying the emerge cost reduced by that artifact's mana value.)\nWhen this creature enters, target opponent exiles the top card of their library, a card at random from their graveyard, and a card at random from their hand. You may cast a spell from among cards exiled this way without paying its mana cost.";

const LOTUS_PETAL: &str = "{T}, Sacrifice this artifact: Add one mana of any color.";

/// The artifact besides Lotus Petal that P0 controls.
#[derive(Clone, Copy, PartialEq, Eq)]
enum SpareArtifact {
    Absent,
    Ornithopter,
}

/// Crabomination with Defiler of Flesh, five Plains, Lotus Petal and `spare`;
/// an empty pool.
fn crabomination_board(spare: SpareArtifact) -> (GameRunner, ObjectId, ObjectId, Option<ObjectId>) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    add_defiler_of_flesh(&mut scenario);
    for _ in 0..5 {
        scenario.add_basic_land(P0, ManaColor::White);
    }
    let petal = scenario
        .add_artifact_from_oracle(P0, "Lotus Petal", LOTUS_PETAL)
        .id();
    let ornithopter = (spare == SpareArtifact::Ornithopter).then(|| {
        scenario
            .add_creature_from_oracle(P0, "Ornithopter", 0, 2, "Flying")
            .as_artifact_creature()
            .id()
    });
    let crab = scenario
        .add_creature_to_hand_from_oracle(P0, "Crabomination", 5, 5, CRABOMINATION)
        .with_mana_cost(cost(4, &[ManaCostShard::Black, ManaCostShard::Black]))
        .with_color(vec![ManaColor::Black])
        .id();
    let runner = scenario.build();
    assert!(
        runner.state().objects[&crab]
            .keywords
            .iter()
            .any(|k| matches!(k, Keyword::Emerge(_))),
        "reach: Crabomination carries Emerge"
    );
    (runner, crab, petal, ornithopter)
}

/// Casts `crab`, accepting the Defiler, answering `alternative` at an
/// `AlternativeCastChoice`, sacrificing `sacrifice`, and paying {B} with Lotus
/// Petal. Returns the prompts answered; the cast must reach `SpellCast`.
fn cast_crabomination(
    runner: &mut GameRunner,
    crab: ObjectId,
    petal: ObjectId,
    alternative: Option<AlternativeCastDecision>,
    sacrifice: Option<ObjectId>,
) -> Vec<&'static str> {
    let card_id = runner.state().objects[&crab].card_id;
    let mut events = runner
        .act(GameAction::CastSpell {
            object_id: crab,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast Crabomination")
        .events;
    let mut prompts = Vec::new();
    for _ in 0..10 {
        let action = match &runner.state().waiting_for {
            WaitingFor::Priority { .. } if runner.state().objects[&crab].zone == Zone::Stack => {
                break;
            }
            WaitingFor::AlternativeCastChoice { keyword, .. } => {
                assert_eq!(
                    *keyword,
                    engine::types::game_state::AlternativeCastKeyword::Emerge,
                    "the alternative offered is Emerge"
                );
                GameAction::ChooseAlternativeCast {
                    choice: alternative.expect("no AlternativeCastChoice expected"),
                }
            }
            WaitingFor::DefilerPayment { .. } => GameAction::DecideOptionalCost { index: 0 },
            WaitingFor::PayCost { .. } => GameAction::SelectCards {
                cards: vec![sacrifice.expect("no sacrifice expected")],
            },
            WaitingFor::ManaPayment { .. }
                if runner.state().objects[&petal].zone == Zone::Battlefield =>
            {
                GameAction::ActivateAbility {
                    source_id: petal,
                    ability_index: 0,
                    payment_mode: CastPaymentMode::Auto,
                }
            }
            WaitingFor::ChooseManaColor { .. } => GameAction::ChooseManaColor {
                choice: engine::types::game_state::ManaChoice::SingleColor(ManaType::Black),
                count: 1,
            },
            WaitingFor::ManaPayment { .. } => GameAction::PassPriority,
            other => panic!("unexpected {other:?} after {prompts:?}"),
        };
        prompts.push(match &runner.state().waiting_for {
            WaitingFor::AlternativeCastChoice { .. } => "AlternativeCastChoice",
            _ => runner.waiting_for_kind(),
        });
        events.extend(runner.act(action).expect("answer the prompt").events);
    }
    assert!(
        events
            .iter()
            .any(|e| matches!(e, engine::types::events::GameEvent::SpellCast { object_id, .. } if *object_id == crab)),
        "the cast completes: {prompts:?}"
    );
    prompts
}

/// CR 702.119a + CR 601.2g: Lotus Petal is the only artifact, so Emerge would
/// both sacrifice it and need its {B} ({5}{B}{B} less Petal's mana value 0,
/// less the Defiler's {B}). Only the printed route is offered; it completes
/// for {4}{B} through the Defiler with Petal's {B}.
#[test]
fn emerge_candidate_cant_also_pay_for_the_emerge_cost() {
    let (mut runner, crab, petal, _) = crabomination_board(SpareArtifact::Absent);
    assert!(offered(&runner, crab), "the printed cast is a legal action");

    let prompts = cast_crabomination(&mut runner, crab, petal, None, None);

    assert!(
        !prompts.contains(&"AlternativeCastChoice"),
        "Emerge isn't offered: {prompts:?}"
    );
    assert_eq!(runner.state().objects[&petal].zone, Zone::Graveyard);
    assert_eq!(runner.state().players[0].life, 18);
}

/// Control: an Ornithopter is the Emerge sacrifice, so Petal's {B} and five
/// Plains pay {5}{B}.
#[test]
fn emerge_sacrificing_another_artifact_pays_with_the_petal() {
    let (mut runner, crab, petal, ornithopter) = crabomination_board(SpareArtifact::Ornithopter);
    let ornithopter = ornithopter.unwrap();

    let prompts = cast_crabomination(
        &mut runner,
        crab,
        petal,
        Some(AlternativeCastDecision::Alternative),
        Some(ornithopter),
    );

    assert_eq!(
        prompts.first(),
        Some(&"AlternativeCastChoice"),
        "{prompts:?}"
    );
    assert!(prompts.contains(&"PayCost"), "{prompts:?}");
    assert_eq!(runner.state().objects[&ornithopter].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&petal].zone, Zone::Graveyard);
    assert_eq!(runner.state().players[0].life, 18);
    assert_eq!(runner.state().players[0].mana_pool.total(), 0);
}

/// CR 702.76a + CR 118.9d: prowl {2}{B} less the Defiler's {B} is {2}. With two
/// colourless mana the printed {3}{B}{B} (reduced {3}{B}) and the raw prowl cost
/// are unaffordable, so the prowl cast is the only one, taken for {2} and 2 life.
#[test]
fn prowl_affordable_only_with_a_defiler_is_offered_and_completes() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    add_defiler_of_flesh(&mut scenario);
    let squad = scenario
        .add_creature_to_hand_from_oracle(P0, "Earwig Squad", 5, 3, EARWIG_SQUAD)
        .with_subtypes(vec!["Goblin", "Rogue"])
        .with_mana_cost(cost(3, &[ManaCostShard::Black, ManaCostShard::Black]))
        .with_color(vec![ManaColor::Black])
        .id();
    scenario.with_mana_pool(P0, pool_of(ManaType::Colorless, 2));
    let mut runner = scenario.build();
    // CR 702.76a: a Goblin the caster controlled dealt combat damage to a player.
    runner
        .state_mut()
        .creature_types_dealt_combat_damage_this_turn
        .insert((P0, "Goblin".to_string()));
    assert!(
        runner.state().objects[&squad]
            .keywords
            .iter()
            .any(|k| matches!(k, Keyword::Prowl(_))),
        "reach: Earwig Squad carries Prowl"
    );
    let life_before = runner.state().players[0].life;

    let result = cast_with_defiler(&mut runner, squad, &Answers::default());

    assert_eq!(result, Ok(vec!["DefilerPayment"]));
    assert_eq!(runner.state().players[0].life, life_before - 2);
    assert_eq!(runner.state().players[0].mana_pool.total(), 0, "{{2}} paid");
}

/// CR 118.9b + CR 118.9d: Mulldrifter's printed {4}{U} less the Defiler's {U}
/// is {4} and evoke {2}{U} less {U} is {2}, so with four colourless mana both
/// are payable and the player chooses; the printed cast completes through the
/// Defiler.
#[test]
fn evoke_with_a_printed_cost_payable_only_with_a_defiler_keeps_the_choice() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    add_defiler_of_dreams(&mut scenario);
    let mulldrifter = scenario
        .add_creature_to_hand_from_oracle(P0, "Mulldrifter", 2, 2, MULLDRIFTER)
        .with_mana_cost(cost(4, &[ManaCostShard::Blue]))
        .with_color(vec![ManaColor::Blue])
        .id();
    scenario.with_mana_pool(P0, pool_of(ManaType::Colorless, 4));
    let mut runner = scenario.build();
    let life_before = runner.state().players[0].life;

    let result = cast_with_defiler(
        &mut runner,
        mulldrifter,
        &Answers {
            alternative: Some(AlternativeCastDecision::Normal),
            ..Answers::default()
        },
    );

    assert_eq!(result, Ok(vec!["AlternativeCastChoice", "DefilerPayment"]));
    assert_eq!(runner.state().players[0].life, life_before - 2);
    assert_eq!(runner.state().players[0].mana_pool.total(), 0, "{{4}} paid");
}

/// Phyrexian Fleshgorger ({7} colourless artifact creature; prototype
/// {1}{B}{B}, 3/3) in hand with Defiler of Flesh and `black` black mana.
fn fleshgorger_with_defiler(black: usize) -> (GameRunner, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    add_defiler_of_flesh(&mut scenario);
    let fleshgorger = scenario
        .add_creature_to_hand(P0, "Phyrexian Fleshgorger", 7, 5)
        .as_artifact_creature()
        .from_oracle_text_with_keywords(
            &["Prototype", "Menace", "Lifelink", "Ward"],
            PHYREXIAN_FLESHGORGER,
        )
        .with_mana_cost(ManaCost::generic(7))
        .id();
    scenario.with_mana_pool(P0, pool_of(ManaType::Black, black));
    let runner = scenario.build();
    assert!(
        runner.state().objects[&fleshgorger]
            .keywords
            .iter()
            .any(|k| matches!(
                k,
                Keyword::Prototype {
                    power: Some(3),
                    toughness: Some(3),
                    ..
                }
            )),
        "reach: Phyrexian Fleshgorger carries its prototype"
    );
    assert!(
        runner.state().objects[&fleshgorger].color.is_empty(),
        "reach: the printed Fleshgorger is colourless"
    );
    (runner, fleshgorger)
}

/// CR 702.160a + CR 718.3b: the prototyped spell has only the prototype mana
/// cost and its colour (black), so Defiler of Flesh reduces {1}{B}{B} to {1}{B}:
/// payable with {B}{B}. The printed {7} is not.
#[test]
fn prototype_is_priced_as_the_black_prototyped_spell() {
    let (mut runner, fleshgorger) = fleshgorger_with_defiler(2);
    assert!(
        offered(&runner, fleshgorger),
        "the prototyped cast is a legal action"
    );
    let life_before = runner.state().players[0].life;

    let result = cast_with_defiler(&mut runner, fleshgorger, &Answers::default());

    assert_eq!(result, Ok(vec!["DefilerPayment"]));
    assert!(
        runner.state().objects[&fleshgorger]
            .color
            .contains(&ManaColor::Black),
        "the spell on the stack is the prototyped, black spell"
    );
    assert_eq!(runner.state().players[0].life, life_before - 2);
    assert_eq!(runner.state().players[0].mana_pool.total(), 0);
}

/// Control: with {B}{B}{B} the prototype cost is payable without the Defiler,
/// which is still offered; accepting it leaves one black mana.
#[test]
fn prototype_offers_the_defiler_when_payable_without_it() {
    let (mut runner, fleshgorger) = fleshgorger_with_defiler(3);

    let result = cast_with_defiler(&mut runner, fleshgorger, &Answers::default());

    assert_eq!(result, Ok(vec!["DefilerPayment"]));
    assert_eq!(runner.state().players[0].mana_pool.total(), 1);
}

/// CR 118.9 + CR 118.9b: "the next red or green creature spell you cast this
/// turn can be cast without paying its mana cost" (World War Hulk, chapter I)
/// is an alternative cost to the printed cost, and spectacle is another. Only
/// one applies (CR 118.9a), so with spectacle {2}{R} affordable the player
/// still chooses, and the free printed cast completes with no mana spent.
#[test]
fn a_pending_free_cast_keeps_the_spectacle_choice() {
    let parsed = parse_oracle_text(
        WORLD_WAR_HULK,
        "World War Hulk",
        &[],
        &["Enchantment".into()],
        &["Saga".into()],
    );
    let spell_filter = parsed
        .triggers
        .iter()
        .find_map(|trigger| {
            trigger
                .execute
                .as_ref()
                .and_then(|ability| match &*ability.effect {
                    engine::types::ability::Effect::GrantNextSpellAbility {
                        modifier:
                            engine::types::game_state::NextSpellModifier::WithoutPayingManaCost,
                        spell_filter,
                        ..
                    } => Some(spell_filter.clone()),
                    _ => None,
                })
        })
        .expect("reach: World War Hulk's chapter I grants a free next spell");

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let acrobat = scenario
        .add_creature_to_hand(P0, "Spikewheel Acrobat", 5, 2)
        .from_oracle_text_with_keywords(&["Spectacle"], SPIKEWHEEL_ACROBAT)
        .with_mana_cost(cost(3, &[ManaCostShard::Red]))
        .with_color(vec![ManaColor::Red])
        .id();
    scenario.with_mana_pool(P0, {
        let mut pool = pool_of(ManaType::Colorless, 2);
        pool.extend(pool_of(ManaType::Red, 1));
        pool
    });
    let mut runner = scenario.build();
    runner.state_mut().players[1].life_lost_this_turn = 1;
    runner.state_mut().pending_next_spell_modifiers.push(
        engine::types::game_state::PendingNextSpellModifier {
            player: P0,
            modifier: engine::types::game_state::NextSpellModifier::WithoutPayingManaCost,
            spell_filter,
            source_id: None,
        },
    );

    let result = cast_with_defiler(
        &mut runner,
        acrobat,
        &Answers {
            alternative: Some(AlternativeCastDecision::Normal),
            ..Answers::default()
        },
    );

    assert_eq!(result, Ok(vec!["AlternativeCastChoice"]));
    assert_eq!(
        runner.state().players[0].mana_pool.total(),
        3,
        "the free printed cast spends no mana"
    );
}

/// Optimus Prime, Hero // Optimus Prime, Autobot Leader from a raw-MTGJSON
/// fixture, so both faces are the printed ones.
fn optimus_db() -> &'static engine::database::card_db::CardDatabase {
    static DB: std::sync::OnceLock<engine::database::card_db::CardDatabase> =
        std::sync::OnceLock::new();
    DB.get_or_init(|| {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/optimus_prime_hero_mtmte_atomic.json");
        engine::database::card_db::CardDatabase::from_mtgjson(&path)
            .expect("parse both printed Optimus Prime faces")
    })
}

/// Optimus Prime, Hero in hand with Heartless Summoning and Defiler of Dreams.
fn optimus_board(pool: Vec<ManaUnit>) -> (GameRunner, ObjectId) {
    use engine::game::scenario_db::GameScenarioDbExt;
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    add_defiler_of_dreams(&mut scenario);
    scenario.add_enchantment_from_oracle(P0, "Heartless Summoning", HEARTLESS_SUMMONING);
    let optimus = scenario.add_real_card(P0, "Optimus Prime, Hero", Zone::Hand, optimus_db());
    scenario.with_mana_pool(P0, pool);
    let runner = scenario.build();
    assert!(
        runner.state().objects[&optimus]
            .keywords
            .iter()
            .any(|k| matches!(k, Keyword::MoreThanMeetsTheEye(_))),
        "reach: Optimus Prime, Hero carries More Than Meets the Eye"
    );
    (runner, optimus)
}

/// CR 702.162a: the spell cast converted has the characteristics of Optimus
/// Prime, Autobot Leader, a Legendary Artifact — Vehicle, so Heartless
/// Summoning's "Creature spells you cast cost {2} less" doesn't reduce it; the
/// Defiler does (it's blue). The printed {3}{U}{R}{W} creature spell is {1}{R}{W}
/// with both reductions, so both casts are payable with {2}{R}{W}: the choice
/// shows the converted cost {2}{U}{R}{W}, and the converted cast takes all four.
#[test]
fn converted_spell_is_priced_as_the_converted_face() {
    let (mut runner, optimus) = optimus_board({
        let mut pool = pool_of(ManaType::Colorless, 2);
        pool.extend(pool_of(ManaType::Red, 1));
        pool.extend(pool_of(ManaType::White, 1));
        pool
    });
    let life_before = runner.state().players[0].life;
    let card_id = runner.state().objects[&optimus].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: optimus,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("the cast starts");
    let WaitingFor::AlternativeCastChoice {
        alternative_cost, ..
    } = &runner.state().waiting_for
    else {
        panic!(
            "both casts are payable, so the player chooses, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(
        alternative_cost.as_ref(),
        Some(&cost(
            2,
            &[
                ManaCostShard::Blue,
                ManaCostShard::Red,
                ManaCostShard::White
            ]
        )),
        "Heartless Summoning doesn't reduce the converted Vehicle spell"
    );
    runner
        .act(GameAction::ChooseAlternativeCast {
            choice: AlternativeCastDecision::Alternative,
        })
        .expect("choose the converted cast");
    assert_eq!(runner.waiting_for_kind(), "DefilerPayment");
    runner
        .act(GameAction::DecideOptionalCost { index: 0 })
        .expect("accept the Defiler");

    assert_eq!(runner.state().objects[&optimus].zone, Zone::Stack);
    assert_eq!(
        runner.state().objects[&optimus].name,
        "Optimus Prime, Autobot Leader"
    );
    assert_eq!(runner.state().players[0].life, life_before - 2);
    assert_eq!(runner.state().players[0].mana_pool.total(), 0);
}

/// With only {R}{W} neither the printed {1}{R}{W} nor the converted {2}{R}{W}
/// (both with the Defiler) can be paid.
#[test]
fn converted_spell_is_not_offered_when_neither_cost_is_payable() {
    let (mut runner, optimus) = optimus_board({
        let mut pool = pool_of(ManaType::Red, 1);
        pool.extend(pool_of(ManaType::White, 1));
        pool
    });
    assert_refused(&mut runner, optimus);
}

// ---------------------------------------------------------------------------
// Fixed payments are reserved before life is budgeted for mana (CR 119.4 +
// CR 601.2h): a Defiler's 2 life can't also pay a {B} through K'rrik.
// ---------------------------------------------------------------------------

const KRRIK_LIFE_FOR_BLACK: &str =
    "For each {B} in a cost, you may pay 2 life rather than pay that mana.";

/// K'rrik, Son of Yawgmoth's "For each {B} in a cost, you may pay 2 life rather
/// than pay that mana." on P0's battlefield.
fn add_krrik(scenario: &mut GameScenario) {
    let parsed = parse_oracle_text(
        KRRIK_LIFE_FOR_BLACK,
        "K'rrik, Son of Yawgmoth",
        &[],
        &["Creature".into()],
        &[],
    );
    assert!(
        parsed
            .statics
            .iter()
            .any(|s| format!("{s:?}").contains("PayLifeAsColoredMana")),
        "reach: K'rrik's grant parses, got {:?}",
        parsed.statics
    );
    let mut krrik = scenario.add_creature(P0, "K'rrik, Son of Yawgmoth", 2, 2);
    krrik.with_color(vec![ManaColor::Black]);
    for s in parsed.statics {
        krrik.with_static_definition(s);
    }
}

/// Earwig Squad ({3}{B}{B}, prowl {2}{B}) in hand with Defiler of Flesh and
/// K'rrik, three colourless mana and P0 at `life`. With `prowl`, Changeling
/// Outcast ("can't be blocked"; every creature type) first deals combat damage
/// to P1, enabling prowl (CR 702.76a), and the cast is in the postcombat main.
fn earwig_with_krrik(life: i32, prowl: bool) -> (GameRunner, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain).with_life(P0, life);
    add_defiler_of_flesh(&mut scenario);
    add_krrik(&mut scenario);
    let outcast = scenario
        .add_creature(P0, "Changeling Outcast", 1, 1)
        .from_oracle_text_with_keywords(
            &["Changeling"],
            "Changeling\nThis creature can't block and can't be blocked.",
        )
        .with_color(vec![ManaColor::Black])
        .id();
    let squad = scenario
        .add_creature_to_hand_from_oracle(P0, "Earwig Squad", 5, 3, EARWIG_SQUAD)
        .with_subtypes(vec!["Goblin", "Rogue"])
        .with_mana_cost(cost(3, &[ManaCostShard::Black, ManaCostShard::Black]))
        .with_color(vec![ManaColor::Black])
        .id();
    let mut runner = scenario.build();
    if prowl {
        // CR 702.73a: Changeling is every creature type; the scenario's type
        // list stands in for the card database's.
        runner.state_mut().all_creature_types = vec!["Goblin".into(), "Rogue".into()];
        runner.state_mut().layers_dirty.mark_full();
        runner.advance_to_combat();
        runner
            .declare_attackers(&[(
                outcast,
                engine::game::combat::AttackTarget::Player(engine::game::scenario::P1),
            )])
            .expect("the Outcast attacks");
        while runner.state().phase != Phase::PostCombatMain {
            let next = match runner.state().waiting_for {
                WaitingFor::DeclareBlockers { .. } => GameAction::DeclareBlockers {
                    assignments: vec![],
                },
                _ => GameAction::PassPriority,
            };
            runner.act(next).expect("combat proceeds");
        }
        assert_eq!(
            runner.state().players[1].life,
            19,
            "reach: combat damage dealt"
        );
    }
    add_colorless_mana(&mut runner, 3);
    (runner, squad)
}

/// Cast `spell` with automatic payment and return the first prompt.
fn start_cast(runner: &mut GameRunner, spell: ObjectId) -> &'static str {
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("the cast is legal");
    match runner.state().waiting_for {
        WaitingFor::AlternativeCastChoice { .. } => "AlternativeCastChoice",
        _ => runner.waiting_for_kind(),
    }
}

/// The completion boundary for a cast that leaves P0 at 0 life: the spell was
/// cast (CR 601.2i) before state-based actions removed P0 from the game.
fn assert_spell_cast(events: &[engine::types::events::GameEvent], spell: ObjectId) {
    assert!(
        events.iter().any(|event| matches!(
            event,
            engine::types::events::GameEvent::SpellCast { object_id, .. } if *object_id == spell
        )),
        "the spell is cast, got {events:?}"
    );
}

/// CR 119.4 + CR 601.2h: at 2 life the printed {3}{B}{B} needs 4 life (the
/// Defiler's 2 and K'rrik's 2 for the remaining {B}, or 4 for both {B}), so
/// only prowl {2}{B} is offered: the Defiler makes it {2}, taken for the 2
/// life.
#[test]
fn printed_cost_needing_the_defilers_life_twice_is_not_offered() {
    let (mut runner, squad) = earwig_with_krrik(2, true);
    assert!(offered(&runner, squad), "the prowl cast is a legal action");

    assert_eq!(
        start_cast(&mut runner, squad),
        "DefilerPayment",
        "only prowl is offered, so there is no choice"
    );
    let paid = runner
        .act(GameAction::DecideOptionalCost { index: 0 })
        .expect("accept the Defiler");

    assert_spell_cast(&paid.events, squad);
    assert_eq!(runner.state().players[0].life, 0, "the Defiler's 2 life");
    assert_eq!(runner.state().players[0].mana_pool.total(), 1, "{{2}} paid");
}

/// Control: at 4 life both methods are offered, and the printed cast with the
/// Defiler pays {3}, 2 life for the Defiler and 2 for K'rrik's {B}.
#[test]
fn printed_cost_payable_with_the_defiler_and_krrik_keeps_the_prowl_choice() {
    let (mut runner, squad) = earwig_with_krrik(4, true);

    assert_eq!(
        start_cast(&mut runner, squad),
        "AlternativeCastChoice",
        "both methods are payable, so the player chooses"
    );
    runner
        .act(GameAction::ChooseAlternativeCast {
            choice: AlternativeCastDecision::Normal,
        })
        .expect("choose the printed cost");
    assert_eq!(runner.waiting_for_kind(), "DefilerPayment");
    runner
        .act(GameAction::DecideOptionalCost { index: 0 })
        .expect("accept the Defiler");
    // CR 107.4f + CR 601.2g: K'rrik lets the remaining {B} be paid with 2 life.
    let WaitingFor::PhyrexianPayment { shards, .. } = &runner.state().waiting_for else {
        panic!(
            "the {{B}} is offered as life, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(shards.len(), 1);
    let paid = runner
        .act(GameAction::SubmitPhyrexianChoices {
            choices: vec![engine::types::game_state::ShardChoice::PayLife],
        })
        .expect("pay the {B} with 2 life");

    assert_spell_cast(&paid.events, squad);
    assert_eq!(runner.state().players[0].life, 0, "2 + 2 life paid");
    assert_eq!(runner.state().players[0].mana_pool.total(), 0, "{{3}} paid");
}

/// CR 119.4 + CR 601.2h: without prowl, the printed cast at 2 life can't be
/// paid either way, so it isn't a legal action.
#[test]
fn printed_cost_needing_the_defilers_life_twice_is_not_castable() {
    let (runner, squad) = earwig_with_krrik(2, false);
    assert!(!offered(&runner, squad), "the printed cast is not legal");
}

const OMNATH: &str = "If you would lose unspent mana, that mana becomes black instead.";

/// CR 107.4f + CR 119.4: Omnath, Locus of All ({W}{U}{B/P}{R}{G}) with Defiler
/// of Dreams and {W}{R}{G}: the Defiler pays the {U} with 2 life and the {B/P}
/// takes 2 more, 4 in all. Refused at 3 life; at 5 it is cast, ending at 1.
#[test]
fn native_phyrexian_shard_and_defiler_life_are_paid_together() {
    for life in [3, 5] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain).with_life(P0, life);
        add_defiler_of_dreams(&mut scenario);
        let omnath = scenario
            .add_creature_to_hand_from_oracle(P0, "Omnath, Locus of All", 4, 4, OMNATH)
            .with_mana_cost(cost(
                0,
                &[
                    ManaCostShard::White,
                    ManaCostShard::Blue,
                    ManaCostShard::PhyrexianBlack,
                    ManaCostShard::Red,
                    ManaCostShard::Green,
                ],
            ))
            .with_color(ManaColor::ALL.to_vec())
            .id();
        let mut pool = pool_of(ManaType::White, 1);
        pool.extend(pool_of(ManaType::Red, 1));
        pool.extend(pool_of(ManaType::Green, 1));
        scenario.with_mana_pool(P0, pool);
        let mut runner = scenario.build();
        if life == 3 {
            assert_refused(&mut runner, omnath);
            continue;
        }
        assert_eq!(start_cast(&mut runner, omnath), "DefilerPayment");
        runner
            .act(GameAction::DecideOptionalCost { index: 0 })
            .expect("accept the Defiler");
        let paid = runner
            .act(GameAction::SubmitPhyrexianChoices {
                choices: vec![engine::types::game_state::ShardChoice::PayLife],
            })
            .expect("pay the {B/P} with 2 life");
        assert_spell_cast(&paid.events, omnath);
        assert_eq!(runner.state().players[0].life, 1);
        assert_eq!(runner.state().players[0].mana_pool.total(), 0);
    }
}

const SOLITUDE: &str = "Flash\nLifelink\nWhen this creature enters, exile up to one other target creature. That creature's controller gains life equal to its power.\nEvoke\u{2014}Exile a white card from your hand.";

/// CR 601.2f + CR 702.74a: Solitude's evoke has no mana part; Trinisphere
/// makes it {3}, which three mana pay with a white card exiled.
#[test]
fn non_mana_evoke_pays_the_floor() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    add_trinisphere(&mut scenario);
    let solitude = scenario
        .add_creature_to_hand_from_oracle(P0, "Solitude", 3, 2, SOLITUDE)
        .with_mana_cost(cost(3, &[ManaCostShard::White, ManaCostShard::White]))
        .with_color(vec![ManaColor::White])
        .id();
    let pitched = scenario
        .add_creature_to_hand(P0, "White Card", 1, 1)
        .with_color(vec![ManaColor::White])
        .id();
    scenario.with_mana_pool(P0, pool_of(ManaType::Colorless, 3));
    let mut runner = scenario.build();
    assert!(
        offered(&runner, solitude),
        "the evoke cast is a legal action"
    );

    let result = cast_with_defiler(
        &mut runner,
        solitude,
        &Answers {
            alternative: Some(AlternativeCastDecision::Alternative),
            sacrifice: Some(pitched),
        },
    );

    assert!(result.is_ok(), "{result:?}");
    assert_eq!(runner.state().objects[&pitched].zone, Zone::Exile);
    assert_eq!(runner.state().players[0].mana_pool.total(), 0, "{{3}} paid");
}
