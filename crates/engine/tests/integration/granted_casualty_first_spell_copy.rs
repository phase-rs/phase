//! Anhelo, the Painter: "The first instant or sorcery spell you cast each turn
//! has casualty 2."
//!
//! The grant reads "spells cast this turn is zero", which stops holding once
//! the spell is cast. The copy trigger re-read the grant when the spell was
//! cast, so the sacrifice was paid and no copy was made. CR 702.153b: each
//! casualty instance "triggers based on the payments made for it".
//!
//! CR 611.3b: a grant lasts only while its permanent is on the battlefield.
//! Silverquill, the Disputant sacrificed for the casualty it grants is gone
//! before the spell becomes cast, so that instance makes no copy.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::triggers::drain_order_triggers_with_identity;
use engine::types::ability::{
    AbilityCost, AdditionalCost, AdditionalCostOrigin, FilterProp, QuantityExpr, SacrificeCost,
    TargetFilter, TargetRef,
};
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

/// Anhelo, the Painter, verbatim.
const ANHELO: &str = "Deathtouch\nThe first instant or sorcery spell you cast each turn has \
casualty 2. (As you cast that spell, you may sacrifice a creature with power 2 or greater. When \
you do, copy the spell and you may choose new targets for the copy.)";

/// Cast a `{0}` "Draw a card." instant from hand with Anhelo and a 3/3 out,
/// answering casualty with `sacrifice`. Returns the runner and the 3/3.
fn cast_first_instant(sacrifice: bool) -> (GameRunner, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Forest", "Forest", "Forest"]);
    scenario.add_creature_from_oracle(P0, "Anhelo, the Painter", 1, 1, ANHELO);
    let fodder = scenario.add_creature(P0, "Fodder", 3, 3).id();
    let mut instant = scenario.add_spell_to_hand_from_oracle(P0, "Zero Draw", true, "Draw a card.");
    instant.with_mana_cost(ManaCost::zero());
    let instant = instant.id();
    let mut runner = scenario.build();
    let card_id = runner.state().objects[&instant].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: instant,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast the instant");
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::OptionalCostChoice { .. }
        ),
        "reach guard: casualty is offered, found {:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::DecideOptionalCost { pay: sacrifice })
        .expect("decide casualty");
    if sacrifice {
        runner
            .act(GameAction::SelectCards {
                cards: vec![fodder],
            })
            .expect("sacrifice Fodder");
    }
    if matches!(runner.state().waiting_for, WaitingFor::ManaPayment { .. }) {
        runner.act(GameAction::PassPriority).expect("pay {0}");
    }
    assert_eq!(runner.state().objects[&instant].zone, Zone::Stack);
    (runner, fodder)
}

/// Pass priority until the stack is empty; returns the cards P0 drew.
fn resolve_and_count_draws(runner: &mut GameRunner) -> usize {
    let hand_before = runner.state().players[0].hand.len();
    for _ in 0..20 {
        drain_order_triggers_with_identity(runner.state_mut());
        if matches!(runner.state().waiting_for, WaitingFor::CopyRetarget { .. }) {
            runner
                .act(GameAction::KeepAllCopyTargets)
                .expect("keep the copy's targets");
            continue;
        }
        if runner.state().stack.is_empty() {
            break;
        }
        runner.act(GameAction::PassPriority).expect("resolve");
    }
    assert!(runner.state().stack.is_empty());
    runner.state().players[0].hand.len() - hand_before
}

#[test]
fn paying_granted_casualty_on_the_first_instant_copies_it() {
    let (mut runner, fodder) = cast_first_instant(true);
    assert_eq!(runner.state().objects[&fodder].zone, Zone::Graveyard);
    assert_eq!(
        resolve_and_count_draws(&mut runner),
        2,
        "the instant and its casualty copy each draw a card"
    );
}

#[test]
fn declining_granted_casualty_makes_no_copy() {
    let (mut runner, fodder) = cast_first_instant(false);
    assert_eq!(runner.state().objects[&fodder].zone, Zone::Battlefield);
    assert_eq!(resolve_and_count_draws(&mut runner), 1);
}

/// Silverquill, the Disputant's casualty line, verbatim.
const SILVERQUILL: &str = "Flying, vigilance\nEach instant and sorcery spell you cast has \
casualty 1. (As you cast that spell, you may sacrifice a creature with power 1 or greater. When \
you do, copy the spell and you may choose new targets for the copy.)";

/// Djinn Illuminatus's replicate line, verbatim.
const DJINN_ILLUMINATUS: &str = "Flying\nEach instant and sorcery spell you cast has replicate. \
The replicate cost is equal to its mana cost. (When you cast it, copy it for each time you paid \
its replicate cost. You may choose new targets for the copies.)";

/// Cast a `{0}` "Draw a card." instant from hand with Silverquill's casualty 1
/// and Djinn Illuminatus's replicate, paying replicate once and answering
/// casualty with `sacrifice`; returns the cards drawn.
fn cast_replicated_instant_with_silverquill(sacrifice: bool) -> usize {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Forest", "Forest", "Forest", "Forest"]);
    scenario.add_creature_from_oracle(P0, "Silverquill, the Disputant", 4, 4, SILVERQUILL);
    scenario.add_creature_from_oracle(P0, "Djinn Illuminatus", 3, 5, DJINN_ILLUMINATUS);
    let fodder = scenario.add_creature(P0, "Fodder", 1, 1).id();
    let mut instant = scenario.add_spell_to_hand_from_oracle(P0, "Zero Draw", true, "Draw a card.");
    instant.with_mana_cost(ManaCost::zero());
    let instant = instant.id();
    let mut runner = scenario.build();
    let card_id = runner.state().objects[&instant].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: instant,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast the instant");
    let mut replicated = false;
    for _ in 0..12 {
        match runner.state().waiting_for.clone() {
            WaitingFor::OptionalCostChoice {
                origin,
                times_kicked,
                ..
            } => {
                let pay = if origin == AdditionalCostOrigin::Replicate {
                    replicated |= times_kicked == 0;
                    times_kicked == 0
                } else {
                    sacrifice
                };
                runner
                    .act(GameAction::DecideOptionalCost { pay })
                    .expect("decide a cost");
            }
            WaitingFor::PayCost { .. } => {
                runner
                    .act(GameAction::SelectCards {
                        cards: vec![fodder],
                    })
                    .expect("sacrifice Fodder");
            }
            WaitingFor::ManaPayment { .. } => {
                runner.act(GameAction::PassPriority).expect("pay {0}");
            }
            _ => break,
        }
    }
    assert!(
        replicated,
        "reach guard: replicate was offered and paid once"
    );
    assert_eq!(runner.state().objects[&instant].zone, Zone::Stack);
    let fodder_zone = if sacrifice {
        Zone::Graveyard
    } else {
        Zone::Battlefield
    };
    assert_eq!(runner.state().objects[&fodder].zone, fodder_zone);
    resolve_and_count_draws(&mut runner)
}

/// CR 702.153b: a paid replicate is not a casualty payment. With casualty
/// declined only replicate's copy is made; with it paid casualty's copy
/// draws a third card.
#[test]
fn a_paid_replicate_makes_no_granted_casualty_copy() {
    assert_eq!(cast_replicated_instant_with_silverquill(false), 2);
    assert_eq!(cast_replicated_instant_with_silverquill(true), 3);
}

/// Cast a `{0}` "Target player draws a card." instant targeting P0 with
/// Silverquill's casualty 1, answering casualty with `sacrifice`; returns the
/// cards P0 drew.
fn cast_targeted_instant_with_silverquill(sacrifice: bool) -> usize {
    cast_targeted_instant(Casualty::Granted, sacrifice)
}

/// Where the targeted instant's casualty 1 comes from.
#[derive(Clone, Copy)]
enum Casualty {
    Granted,
    Printed,
    /// Silverquill's casualty plus Djinn Illuminatus's replicate.
    GrantedWithReplicate,
}

fn cast_targeted_instant(casualty: Casualty, sacrifice: bool) -> usize {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Forest", "Forest", "Forest"]);
    let text = match casualty {
        Casualty::Granted => {
            scenario.add_creature_from_oracle(P0, "Silverquill, the Disputant", 4, 4, SILVERQUILL);
            "Target player draws a card."
        }
        Casualty::Printed => "Casualty 1\nTarget player draws a card.",
        Casualty::GrantedWithReplicate => {
            scenario.add_creature_from_oracle(P0, "Silverquill, the Disputant", 4, 4, SILVERQUILL);
            scenario.add_creature_from_oracle(P0, "Djinn Illuminatus", 3, 5, DJINN_ILLUMINATUS);
            "Target player draws a card."
        }
    };
    let fodder = scenario.add_creature(P0, "Fodder", 1, 1).id();
    let mut instant = scenario.add_spell_to_hand_from_oracle(P0, "Targeted Draw", true, text);
    instant.with_mana_cost(ManaCost::zero());
    let instant = instant.id();
    let mut runner = scenario.build();
    let card_id = runner.state().objects[&instant].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: instant,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast the instant");
    let mut prompts = Vec::new();
    for _ in 0..12 {
        match runner.state().waiting_for.clone() {
            WaitingFor::OptionalCostChoice { origin, .. } => {
                prompts.push(origin);
                let pay = origin == AdditionalCostOrigin::Casualty && sacrifice;
                runner
                    .act(GameAction::DecideOptionalCost { pay })
                    .expect("decide a cost");
            }
            WaitingFor::PayCost { .. } => {
                runner
                    .act(GameAction::SelectCards {
                        cards: vec![fodder],
                    })
                    .expect("sacrifice Fodder");
            }
            WaitingFor::TargetSelection { .. } => {
                runner
                    .act(GameAction::SelectTargets {
                        targets: vec![TargetRef::Player(P0)],
                    })
                    .expect("target P0");
            }
            WaitingFor::ManaPayment { .. } => {
                runner.act(GameAction::PassPriority).expect("pay {0}");
            }
            _ => break,
        }
    }
    let mut expected = vec![AdditionalCostOrigin::Casualty];
    if matches!(casualty, Casualty::GrantedWithReplicate) {
        expected.push(AdditionalCostOrigin::Replicate);
    }
    assert_eq!(
        prompts, expected,
        "casualty, then the rest of the queue, before targets"
    );
    assert_eq!(runner.state().objects[&instant].zone, Zone::Stack);
    let fodder_zone = if sacrifice {
        Zone::Graveyard
    } else {
        Zone::Battlefield
    };
    assert_eq!(runner.state().objects[&fodder].zone, fodder_zone);
    resolve_and_count_draws(&mut runner)
}

/// CR 601.2b + CR 702.153b: a targeted spell declares casualty before its
/// targets; the paid casualty still copies it, a declined one does not.
#[test]
fn granted_casualty_copies_a_targeted_spell() {
    assert_eq!(cast_targeted_instant_with_silverquill(true), 2);
    assert_eq!(cast_targeted_instant_with_silverquill(false), 1);
}

/// CR 702.153a: printed casualty on a targeted spell, declared before its
/// targets, copies it when paid ("When you cast this spell, if a casualty
/// cost was paid for it, copy it").
#[test]
fn printed_casualty_copies_a_targeted_spell() {
    assert_eq!(cast_targeted_instant(Casualty::Printed, true), 2);
    assert_eq!(cast_targeted_instant(Casualty::Printed, false), 1);
}

/// CR 601.2b: casualty before targets does not drop the other queued
/// additional costs: Djinn Illuminatus's replicate is offered after it.
#[test]
fn granted_casualty_before_targets_keeps_replicate() {
    assert_eq!(
        cast_targeted_instant(Casualty::GrantedWithReplicate, true),
        2
    );
}

/// Which card a casualty instance comes from, read from its sacrifice
/// threshold: Silverquill grants casualty 1, Anhelo casualty 2, and the
/// printed instance of [`cast_with_casualty_sources`] is casualty 3.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Instance {
    Silverquill,
    Anhelo,
    Printed,
}

impl Instance {
    fn of(cost: &AdditionalCost) -> Self {
        let AdditionalCost::Optional {
            cost:
                AbilityCost::Sacrifice(SacrificeCost {
                    target: TargetFilter::Typed(filter),
                    ..
                }),
            ..
        } = cost
        else {
            panic!("casualty is an optional sacrifice, found {cost:?}");
        };
        let power = filter.properties.iter().find_map(|prop| match prop {
            FilterProp::PtComparison {
                value: QuantityExpr::Fixed { value },
                ..
            } => Some(*value),
            _ => None,
        });
        match power {
            Some(1) => Instance::Silverquill,
            Some(2) => Instance::Anhelo,
            Some(3) => Instance::Printed,
            other => panic!("unexpected casualty threshold {other:?}"),
        }
    }
}

/// The creatures of [`cast_with_casualty_sources`].
struct Board {
    silverquill: ObjectId,
    fodder: [ObjectId; 2],
}

/// Cast a `{0}` "Draw a card." instant with Silverquill's casualty 1, Anhelo's
/// casualty 2 when `anhelo`, and a printed casualty 3 when `printed`. Each
/// casualty prompt is answered by `sacrifice`: the creature to sacrifice for
/// that instance, or `None` to decline. Returns the cards drawn.
fn cast_with_casualty_sources(
    printed: bool,
    anhelo: bool,
    sacrifice: impl Fn(&Board, Instance) -> Option<ObjectId>,
) -> usize {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Forest", "Forest", "Forest", "Forest"]);
    let silverquill = scenario
        .add_creature_from_oracle(P0, "Silverquill, the Disputant", 4, 4, SILVERQUILL)
        .id();
    let anhelo = anhelo.then(|| {
        scenario
            .add_creature_from_oracle(P0, "Anhelo, the Painter", 1, 1, ANHELO)
            .id()
    });
    let fodder = [
        scenario.add_creature(P0, "Fodder A", 3, 3).id(),
        scenario.add_creature(P0, "Fodder B", 3, 3).id(),
    ];
    let text = if printed {
        "Casualty 3\nDraw a card."
    } else {
        "Draw a card."
    };
    let mut instant = scenario.add_spell_to_hand_from_oracle(P0, "Zero Draw", true, text);
    instant.with_mana_cost(ManaCost::zero());
    let instant = instant.id();
    let board = Board {
        silverquill,
        fodder,
    };
    let mut runner = scenario.build();
    let card_id = runner.state().objects[&instant].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: instant,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast the instant");
    let mut offered = Vec::new();
    let mut chosen = None;
    for _ in 0..16 {
        match runner.state().waiting_for.clone() {
            WaitingFor::OptionalCostChoice {
                origin,
                cost,
                pending_cast,
                ..
            } => {
                assert_eq!(origin, AdditionalCostOrigin::Casualty);
                let instance = Instance::of(&cost);
                // The instance names the permanent that grants it.
                let grant_source = match instance {
                    Instance::Silverquill => Some(silverquill),
                    Instance::Anhelo => anhelo,
                    Instance::Printed => None,
                };
                assert_eq!(
                    pending_cast.additional_cost_queue[0].grant_source, grant_source,
                    "{instance:?} names its granting permanent"
                );
                offered.push(instance);
                chosen = sacrifice(&board, instance);
                runner
                    .act(GameAction::DecideOptionalCost {
                        pay: chosen.is_some(),
                    })
                    .expect("decide casualty");
            }
            WaitingFor::PayCost { .. } => {
                let creature = chosen.take().expect("a paid casualty names its creature");
                runner
                    .act(GameAction::SelectCards {
                        cards: vec![creature],
                    })
                    .expect("sacrifice the chosen creature");
            }
            WaitingFor::ManaPayment { .. } => {
                runner.act(GameAction::PassPriority).expect("pay {0}");
            }
            _ => break,
        }
    }
    let mut expected = vec![Instance::Silverquill];
    if anhelo.is_some() {
        expected.push(Instance::Anhelo);
    }
    if printed {
        expected.push(Instance::Printed);
    }
    offered.sort();
    assert_eq!(
        offered, expected,
        "reach guard: each casualty instance is offered"
    );
    assert_eq!(runner.state().objects[&instant].zone, Zone::Stack);
    resolve_and_count_draws(&mut runner)
}

/// CR 611.3b + CR 601.2i: Silverquill sacrificed for its own casualty has left
/// before the spell becomes cast, so the spell no longer has that casualty and
/// no copy is made. Sacrificing another creature for it copies the spell.
#[test]
fn silverquill_sacrificed_for_its_own_casualty_makes_no_copy() {
    let draws = cast_with_casualty_sources(false, false, |b, _| Some(b.silverquill));
    assert_eq!(draws, 1, "no casualty copy");
    let draws = cast_with_casualty_sources(false, false, |b, _| Some(b.fodder[0]));
    assert_eq!(draws, 2, "another creature paid it: the copy draws too");
}

/// CR 702.153b: with Silverquill's and Anhelo's grants on the first instant,
/// each instance triggers from its own payment and only while its granting
/// permanent is still there when the spell becomes cast.
#[test]
fn each_granted_casualty_copies_only_while_its_source_remains() {
    // Silverquill pays its own instance; Anhelo's is declined.
    let draws = cast_with_casualty_sources(false, true, |b, instance| {
        (instance == Instance::Silverquill).then_some(b.silverquill)
    });
    assert_eq!(draws, 1, "Silverquill's grant left with it");
    // Silverquill pays Anhelo's instance; Silverquill's is declined.
    let draws = cast_with_casualty_sources(false, true, |b, instance| {
        (instance == Instance::Anhelo).then_some(b.silverquill)
    });
    assert_eq!(draws, 2, "Anhelo's grant remains, so its payment copies");
    // Fodder pays Silverquill's instance, Silverquill pays Anhelo's.
    let draws = cast_with_casualty_sources(false, true, |b, instance| {
        Some(match instance {
            Instance::Silverquill => b.fodder[0],
            _ => b.silverquill,
        })
    });
    assert_eq!(draws, 2, "only Anhelo's instance copies");
    // Separate fodder for both: both grants remain.
    let draws = cast_with_casualty_sources(false, true, |b, instance| {
        Some(match instance {
            Instance::Silverquill => b.fodder[0],
            _ => b.fodder[1],
        })
    });
    assert_eq!(draws, 3, "both instances copy");
}

/// CR 702.153a: printed casualty is the spell's own and stays when Silverquill
/// is sacrificed for it; Silverquill's queued instance, paid with another
/// creature, no longer applies.
#[test]
fn printed_casualty_copies_when_silverquill_is_sacrificed_for_it() {
    let draws = cast_with_casualty_sources(true, false, |b, instance| {
        Some(match instance {
            Instance::Printed => b.silverquill,
            _ => b.fodder[0],
        })
    });
    assert_eq!(draws, 2, "only the printed instance copies");
    let draws = cast_with_casualty_sources(true, false, |b, instance| {
        Some(match instance {
            Instance::Printed => b.fodder[0],
            _ => b.fodder[1],
        })
    });
    assert_eq!(draws, 3, "printed and granted instances each copy");
}

/// Fling, verbatim.
const FLING: &str = "As an additional cost to cast this spell, sacrifice a creature.\n\
Fling deals damage equal to the sacrificed creature's power to any target.";

/// CR 601.2f + CR 601.2h: cast a `{0}` Fling from hand at P1 with Anhelo, a
/// 3/3 and a 2/2 out, answering casualty with `sacrifice`. A sacrifice prompt
/// offering Anhelo (power 1) is Fling's and gets the 2/2; casualty's gets the
/// 3/3. Returns how many prompts were Fling's, the creatures' zones (3/3,
/// 2/2) and P1's life lost.
fn cast_fling_from_hand(sacrifice: bool) -> (usize, Zone, Zone, i32) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let anhelo = scenario
        .add_creature_from_oracle(P0, "Anhelo, the Painter", 1, 1, ANHELO)
        .id();
    let big = scenario.add_creature(P0, "Big Fodder", 3, 3).id();
    let small = scenario.add_creature(P0, "Small Fodder", 2, 2).id();
    let mut fling = scenario.add_spell_to_hand_from_oracle(P0, "Fling", true, FLING);
    fling.with_mana_cost(ManaCost::zero());
    let fling = fling.id();
    let mut runner = scenario.build();
    let card_id = runner.state().objects[&fling].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: fling,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast Fling");
    let mut fling_prompts = 0;
    for _ in 0..12 {
        match runner.state().waiting_for.clone() {
            WaitingFor::OptionalCostChoice { .. } => {
                runner
                    .act(GameAction::DecideOptionalCost { pay: sacrifice })
                    .expect("decide casualty");
            }
            WaitingFor::PayCost { choices, .. } => {
                let fling_cost = choices.contains(&anhelo);
                fling_prompts += usize::from(fling_cost);
                let pick = if fling_cost { small } else { big };
                runner
                    .act(GameAction::SelectCards { cards: vec![pick] })
                    .expect("sacrifice a creature");
            }
            WaitingFor::TargetSelection { .. } => {
                runner
                    .act(GameAction::SelectTargets {
                        targets: vec![TargetRef::Player(P1)],
                    })
                    .expect("target P1");
            }
            WaitingFor::ManaPayment { .. } => {
                runner.act(GameAction::PassPriority).expect("pay {0}");
            }
            _ => break,
        }
    }
    assert_eq!(runner.state().objects[&fling].zone, Zone::Stack);
    let zones = (
        runner.state().objects[&big].zone,
        runner.state().objects[&small].zone,
    );
    let life_before = runner.state().players[1].life;
    resolve_and_count_draws(&mut runner);
    let lost = life_before - runner.state().players[1].life;
    (fling_prompts, zones.0, zones.1, lost)
}

/// CR 601.2f + CR 118.8: a casualty decision does not discharge Fling's
/// "sacrifice a creature". Declining casualty, Fling still takes the 2/2 and
/// deals 2; accepting, casualty takes the 3/3, Fling the 2/2, and Fling and
/// its copy each deal 2.
#[test]
fn fling_from_hand_pays_its_sacrifice_beside_granted_casualty() {
    assert_eq!(
        cast_fling_from_hand(false),
        (1, Zone::Battlefield, Zone::Graveyard, 2)
    );
    assert_eq!(
        cast_fling_from_hand(true),
        (1, Zone::Graveyard, Zone::Graveyard, 4)
    );
}

/// A cost prompt the casts below answered, in order.
#[derive(Debug, PartialEq)]
enum Prompt {
    Casualty,
    Kicker,
    /// A printed "A or B" additional cost.
    Choice,
    Sacrifice,
    Targets,
}

/// Cast `spell` from hand with Anhelo out, declining casualty, answering a
/// kicker or printed choice with `pay`, sacrificing `fodder` and targeting
/// `target`. Returns the prompts in order.
fn cast_beside_declined_casualty(
    runner: &mut GameRunner,
    spell: ObjectId,
    fodder: ObjectId,
    target: TargetRef,
    pay: bool,
) -> Vec<Prompt> {
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast the spell");
    let mut prompts = Vec::new();
    for _ in 0..16 {
        match runner.state().waiting_for.clone() {
            WaitingFor::OptionalCostChoice { cost, origin, .. } => {
                let (prompt, answer) = match (cost, origin) {
                    (_, AdditionalCostOrigin::Casualty) => (Prompt::Casualty, false),
                    (AdditionalCost::Kicker { .. }, _) => (Prompt::Kicker, pay),
                    (AdditionalCost::Choice(..), _) => (Prompt::Choice, pay),
                    (cost, origin) => panic!("unexpected prompt {cost:?} ({origin:?})"),
                };
                prompts.push(prompt);
                runner
                    .act(GameAction::DecideOptionalCost { pay: answer })
                    .expect("decide a cost");
            }
            WaitingFor::PayCost { .. } => {
                prompts.push(Prompt::Sacrifice);
                runner
                    .act(GameAction::SelectCards {
                        cards: vec![fodder],
                    })
                    .expect("sacrifice");
            }
            WaitingFor::TargetSelection { .. } => {
                prompts.push(Prompt::Targets);
                runner
                    .act(GameAction::SelectTargets {
                        targets: vec![target.clone()],
                    })
                    .expect("choose the target");
            }
            WaitingFor::ManaPayment { .. } => {
                runner.act(GameAction::PassPriority).expect("pay the mana");
            }
            _ => break,
        }
    }
    assert_eq!(runner.state().objects[&spell].zone, Zone::Stack);
    prompts
}

/// CR 601.2f: with no creature of power 2 or greater, Anhelo's casualty 2
/// can't be offered, and Fling's "sacrifice a creature" is asked for once.
#[test]
fn fling_beside_unpayable_casualty_asks_for_its_sacrifice_once() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let anhelo = scenario
        .add_creature_from_oracle(P0, "Anhelo, the Painter", 1, 1, ANHELO)
        .id();
    let weak = scenario.add_creature(P0, "Weak Fodder", 1, 1).id();
    let mut fling = scenario.add_spell_to_hand_from_oracle(P0, "Fling", true, FLING);
    fling.with_mana_cost(ManaCost::zero());
    let fling = fling.id();
    let mut runner = scenario.build();
    let prompts =
        cast_beside_declined_casualty(&mut runner, fling, weak, TargetRef::Player(P1), false);
    assert_eq!(prompts, vec![Prompt::Targets, Prompt::Sacrifice]);
    assert_eq!(runner.state().objects[&weak].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&anhelo].zone, Zone::Battlefield);
}

/// CR 601.2f + CR 118.8: Bitter Triumph's "discard a card or pay 3 life" is
/// still part of the total cost after casualty is declined before targets.
/// With a card in hand both branches are payable, so P0 chooses; the life
/// branch costs 3 life and keeps the card; the target is destroyed.
#[test]
fn a_printed_choice_cost_follows_declined_casualty() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "Anhelo, the Painter", 1, 1, ANHELO);
    let fodder = scenario.add_creature(P0, "Fodder", 3, 3).id();
    let victim = scenario.add_creature(P1, "Victim", 2, 2).id();
    // A card to discard, so both branches are payable and P0 is asked.
    let spare = scenario
        .add_spell_to_hand_from_oracle(P0, "Spare", true, "Draw a card.")
        .id();
    let mut triumph = scenario.add_spell_to_hand_from_oracle(
        P0,
        "Bitter Triumph",
        true,
        "As an additional cost to cast this spell, discard a card or pay 3 life.\n\
         Destroy target creature or planeswalker.",
    );
    triumph.with_mana_cost(ManaCost::zero());
    let triumph = triumph.id();
    let mut runner = scenario.build();
    let prompts = cast_beside_declined_casualty(
        &mut runner,
        triumph,
        fodder,
        TargetRef::Object(victim),
        false,
    );
    assert_eq!(
        prompts,
        vec![Prompt::Casualty, Prompt::Targets, Prompt::Choice]
    );
    assert_eq!(
        runner.state().players[0].life,
        17,
        "the life branch is paid"
    );
    assert_eq!(runner.state().objects[&spare].zone, Zone::Hand);
    resolve_and_count_draws(&mut runner);
    assert_eq!(runner.state().objects[&victim].zone, Zone::Graveyard);
}

/// CR 601.2f + CR 118.8: a cost another effect imposes for the chosen target
/// (Terror of the Peaks: "Spells your opponents cast that target this
/// creature cost an additional 3 life to cast.") is collected once targets
/// are known, after casualty was declined before them.
#[test]
fn a_target_imposed_cost_follows_declined_casualty() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "Anhelo, the Painter", 1, 1, ANHELO);
    let fodder = scenario.add_creature(P0, "Fodder", 3, 3).id();
    let terror = scenario
        .add_creature_from_oracle(
            P1,
            "Terror of the Peaks",
            5,
            4,
            "Flying\nSpells your opponents cast that target this creature cost an additional \
             3 life to cast.\nWhenever another creature you control enters, this creature deals \
             damage equal to that creature's power to any target.",
        )
        .id();
    let mut fling = scenario.add_spell_to_hand_from_oracle(P0, "Fling", true, FLING);
    fling.with_mana_cost(ManaCost::zero());
    let fling = fling.id();
    let mut runner = scenario.build();
    let prompts =
        cast_beside_declined_casualty(&mut runner, fling, fodder, TargetRef::Object(terror), false);
    assert_eq!(
        prompts,
        vec![Prompt::Casualty, Prompt::Targets, Prompt::Sacrifice]
    );
    assert_eq!(
        runner.state().players[0].life,
        17,
        "Terror's 3 life is paid"
    );
}

/// CR 601.2b + CR 702.33a: Burst Lightning's printed kicker is still offered
/// after casualty is declined before targets. Kicked, it deals 4.
#[test]
fn a_printed_kicker_follows_declined_casualty() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "Anhelo, the Painter", 1, 1, ANHELO);
    let fodder = scenario.add_creature(P0, "Fodder", 3, 3).id();
    for _ in 0..5 {
        scenario.add_basic_land(P0, engine::types::mana::ManaColor::Red);
    }
    let lightning = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Burst Lightning",
            true,
            "Kicker {4} (You may pay an additional {4} as you cast this spell.)\n\
             Burst Lightning deals 2 damage to any target. If this spell was kicked, it deals \
             4 damage instead.",
        )
        .id();
    let mut runner = scenario.build();
    let prompts =
        cast_beside_declined_casualty(&mut runner, lightning, fodder, TargetRef::Player(P1), true);
    assert_eq!(
        prompts,
        vec![Prompt::Casualty, Prompt::Targets, Prompt::Kicker]
    );
    let life_before = runner.state().players[1].life;
    resolve_and_count_draws(&mut runner);
    assert_eq!(life_before - runner.state().players[1].life, 4);
}
