//! Regression test for Abigale, Eloquent First-Year (BLC).
//!
//! Oracle text:
//!   Flying, first strike, lifelink
//!   When Abigale enters, up to one other target creature loses all
//!   abilities. Put a flying counter, a first strike counter, and a
//!   lifelink counter on that creature.
//!
//! Bug symptom: counters were landing on Abigale itself, not on the
//! targeted other creature. The second sentence's "that creature" is
//! an anaphor to the first sentence's target, lowered by the parser
//! as a `PutCounter` chain with `TargetFilter::ParentTarget`.

#![allow(unused_imports)]

use crate::rules::{GameAction, GameScenario, WaitingFor, Zone, P0, P1};
use engine::types::ability::{
    AbilityCost, AbilityDefinition, AbilityKind, Effect, ManaContribution, ManaProduction,
    TargetRef,
};
use engine::types::counter::CounterType;
use engine::types::keywords::{Keyword, KeywordKind};
use engine::types::mana::ManaColor;
use engine::types::phase::Phase;

fn flying() -> CounterType {
    CounterType::Keyword(KeywordKind::Flying)
}
fn first_strike() -> CounterType {
    CounterType::Keyword(KeywordKind::FirstStrike)
}
fn lifelink() -> CounterType {
    CounterType::Keyword(KeywordKind::Lifelink)
}

#[test]
fn abigale_enters_puts_counters_on_target_creature_not_self() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let abigale_oracle = "Flying, first strike, lifelink\n\
        When Abigale enters, up to one other target creature loses all abilities. \
        Put a flying counter, a first strike counter, and a lifelink counter on that creature.";
    let abigale_builder =
        scenario.add_creature_to_hand_from_oracle(P0, "Abigale", 1, 1, abigale_oracle);
    let abigale_id = abigale_builder.id();

    let bear_id = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();

    let mut runner = scenario.build();
    let abigale_card_id = runner.state().objects[&abigale_id].card_id;

    runner
        .act(GameAction::CastSpell {
            object_id: abigale_id,
            card_id: abigale_card_id,
            targets: vec![],
        })
        .expect("cast Abigale");

    for _ in 0..30 {
        match runner.state().waiting_for.clone() {
            WaitingFor::TargetSelection { .. } | WaitingFor::TriggerTargetSelection { .. } => {
                runner
                    .act(GameAction::SelectTargets {
                        targets: vec![TargetRef::Object(bear_id)],
                    })
                    .expect("select bear as target");
            }
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() {
                    // Pass once more to drain pending triggers into stack, then exit if truly idle.
                    if runner.act(GameAction::PassPriority).is_err() {
                        break;
                    }
                    if runner.state().stack.is_empty()
                        && matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
                    {
                        break;
                    }
                } else if runner.act(GameAction::PassPriority).is_err() {
                    break;
                }
            }
            _ => break,
        }
    }

    let bear = runner.state().objects.get(&bear_id).expect("bear present");
    assert!(
        bear.counters.get(&flying()).copied().unwrap_or(0) >= 1,
        "Bear should have a flying counter; counters = {:?}",
        bear.counters,
    );
    assert!(
        bear.counters.get(&first_strike()).copied().unwrap_or(0) >= 1,
        "Bear should have a first strike counter; counters = {:?}",
        bear.counters,
    );
    assert!(
        bear.counters.get(&lifelink()).copied().unwrap_or(0) >= 1,
        "Bear should have a lifelink counter; counters = {:?}",
        bear.counters,
    );

    // CR 122.1b: Keyword counters must grant the corresponding keyword at
    // layer 6. Counters without abilities is the exact bug being regressed.
    assert!(
        bear.has_keyword(&Keyword::Flying),
        "Bear should have Flying granted by its flying counter; keywords = {:?}",
        bear.keywords,
    );
    assert!(
        bear.has_keyword(&Keyword::FirstStrike),
        "Bear should have FirstStrike granted by its first strike counter; keywords = {:?}",
        bear.keywords,
    );
    assert!(
        bear.has_keyword(&Keyword::Lifelink),
        "Bear should have Lifelink granted by its lifelink counter; keywords = {:?}",
        bear.keywords,
    );

    let abigale = runner
        .state()
        .objects
        .get(&abigale_id)
        .expect("abigale present");
    assert_eq!(
        abigale.counters.get(&flying()).copied().unwrap_or(0),
        0,
        "Abigale must not have a flying counter; counters = {:?}",
        abigale.counters,
    );
    assert_eq!(
        abigale.counters.get(&first_strike()).copied().unwrap_or(0),
        0,
        "Abigale must not have a first strike counter; counters = {:?}",
        abigale.counters,
    );
    assert_eq!(
        abigale.counters.get(&lifelink()).copied().unwrap_or(0),
        0,
        "Abigale must not have a lifelink counter; counters = {:?}",
        abigale.counters,
    );
}

#[test]
fn abigale_enters_with_no_target_skipped_puts_no_counters_on_self() {
    // When the optional "up to one other target creature" is skipped, the
    // anaphor "that creature" has no referent. No counters should land on
    // Abigale (CR 115.1d: "up to N" targets is independent of the second
    // sentence's placement clause — skipping means nothing gets counters).
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let abigale_oracle = "Flying, first strike, lifelink\n\
        When Abigale enters, up to one other target creature loses all abilities. \
        Put a flying counter, a first strike counter, and a lifelink counter on that creature.";
    let abigale_builder =
        scenario.add_creature_to_hand_from_oracle(P0, "Abigale", 1, 1, abigale_oracle);
    let abigale_id = abigale_builder.id();

    // No other creatures: "up to one" can only be zero targets.
    let mut runner = scenario.build();
    let abigale_card_id = runner.state().objects[&abigale_id].card_id;

    runner
        .act(GameAction::CastSpell {
            object_id: abigale_id,
            card_id: abigale_card_id,
            targets: vec![],
        })
        .expect("cast Abigale");

    for _ in 0..30 {
        match runner.state().waiting_for.clone() {
            WaitingFor::TargetSelection { .. } | WaitingFor::TriggerTargetSelection { .. } => {
                runner
                    .act(GameAction::SelectTargets { targets: vec![] })
                    .expect("skip optional target");
            }
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() {
                    if runner.act(GameAction::PassPriority).is_err() {
                        break;
                    }
                    if runner.state().stack.is_empty()
                        && matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
                    {
                        break;
                    }
                } else if runner.act(GameAction::PassPriority).is_err() {
                    break;
                }
            }
            _ => break,
        }
    }

    let abigale = runner
        .state()
        .objects
        .get(&abigale_id)
        .expect("abigale present");
    assert!(
        abigale.counters.is_empty(),
        "Abigale must not receive counters when the optional target was skipped; \
         counters = {:?}",
        abigale.counters,
    );
}

#[test]
fn abigale_loses_all_abilities_scopes_to_chosen_target_only() {
    // CR 113.3 + CR 611.2: "Up to one other target creature loses all abilities"
    // is a continuous effect from a resolving triggered ability that applies to
    // the *chosen target* — not to every other creature on the battlefield.
    //
    // Regression: the parser builds a `GenericEffect` whose embedded static
    // carries `affected: Typed { Creature, Another }`, which (without
    // target-aware scoping) would broadcast the lose-all-abilities effect to
    // every other creature in play. This test guarantees the runtime restricts
    // the effect to exactly the targeted creature, leaving bystanders' printed
    // abilities intact.
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let abigale_oracle = "Flying, first strike, lifelink\n\
        When Abigale enters, up to one other target creature loses all abilities. \
        Put a flying counter, a first strike counter, and a lifelink counter on that creature.";
    let abigale_builder =
        scenario.add_creature_to_hand_from_oracle(P0, "Abigale", 1, 1, abigale_oracle);
    let abigale_id = abigale_builder.id();

    // Probe with Trample — a printed keyword that Abigale's trigger does NOT
    // grant via counters. (Using Flying/FirstStrike/Lifelink would be ambiguous:
    // CR 122.1b re-grants those at layer 6 from the placed counters, masking
    // whether RemoveAllAbilities ever ran.)
    let mut target_builder = scenario.add_creature(P1, "Trample Beast", 1, 2);
    target_builder.with_keyword(Keyword::Trample);
    let target_id = target_builder.id();

    // Bystander on the opponent's side — must NOT lose Trample and must NOT
    // receive any of Abigale's keyword counters.
    let mut bystander_builder = scenario.add_creature(P1, "Bystander Beast", 1, 2);
    bystander_builder.with_keyword(Keyword::Trample);
    let bystander_id = bystander_builder.id();

    // Ally bystander on Abigale's own side — also "another creature", must
    // remain intact (proves the affected scope isn't broadcast across "Another").
    let mut ally_builder = scenario.add_creature(P0, "Ally Beast", 1, 2);
    ally_builder.with_keyword(Keyword::Trample);
    let ally_id = ally_builder.id();

    let mut runner = scenario.build();
    let abigale_card_id = runner.state().objects[&abigale_id].card_id;

    runner
        .act(GameAction::CastSpell {
            object_id: abigale_id,
            card_id: abigale_card_id,
            targets: vec![],
        })
        .expect("cast Abigale");

    for _ in 0..30 {
        match runner.state().waiting_for.clone() {
            WaitingFor::TargetSelection { .. } | WaitingFor::TriggerTargetSelection { .. } => {
                runner
                    .act(GameAction::SelectTargets {
                        targets: vec![TargetRef::Object(target_id)],
                    })
                    .expect("select target storm crow");
            }
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() {
                    if runner.act(GameAction::PassPriority).is_err() {
                        break;
                    }
                    if runner.state().stack.is_empty()
                        && matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
                    {
                        break;
                    }
                } else if runner.act(GameAction::PassPriority).is_err() {
                    break;
                }
            }
            _ => break,
        }
    }

    let target = runner.state().objects.get(&target_id).expect("target");
    let bystander = runner
        .state()
        .objects
        .get(&bystander_id)
        .expect("bystander");
    let ally = runner.state().objects.get(&ally_id).expect("ally");

    // Targeted creature: lost its printed Trample, gained the keyword counters.
    assert!(
        !target.has_keyword(&Keyword::Trample),
        "Target should have lost its printed Trample ability; keywords = {:?}",
        target.keywords,
    );
    assert!(
        target.counters.get(&flying()).copied().unwrap_or(0) >= 1,
        "Target should have received a flying counter; counters = {:?}",
        target.counters,
    );

    // Bystander on the opponent's side: keeps Trample, receives no counters.
    assert!(
        bystander.has_keyword(&Keyword::Trample),
        "Bystander must retain its printed Trample — the lose-all-abilities effect \
         is scoped to the chosen target, not broadcast to every other creature; \
         bystander keywords = {:?}",
        bystander.keywords,
    );
    assert_eq!(
        bystander.counters.get(&flying()).copied().unwrap_or(0),
        0,
        "Bystander must not receive a flying counter; counters = {:?}",
        bystander.counters,
    );
    assert_eq!(
        bystander
            .counters
            .get(&first_strike())
            .copied()
            .unwrap_or(0),
        0,
        "Bystander must not receive a first strike counter; counters = {:?}",
        bystander.counters,
    );
    assert_eq!(
        bystander.counters.get(&lifelink()).copied().unwrap_or(0),
        0,
        "Bystander must not receive a lifelink counter; counters = {:?}",
        bystander.counters,
    );

    // Ally on Abigale's own side (also "another creature"): same expectations.
    assert!(
        ally.has_keyword(&Keyword::Trample),
        "Ally bystander on controller's side must retain Trample; keywords = {:?}",
        ally.keywords,
    );
    assert_eq!(
        ally.counters.get(&flying()).copied().unwrap_or(0),
        0,
        "Ally bystander must not receive any keyword counters; counters = {:?}",
        ally.counters,
    );
}

#[test]
fn abigale_strips_non_keyword_abilities_from_target() {
    // CR 113.3: "[Target] loses all abilities" must remove EVERY kind of ability
    // — keyword (Defender, Menace), activated (mana), static, triggered. The
    // implementation in `layers.rs::apply` clears `keywords`, `abilities`,
    // `trigger_definitions`, `static_definitions`, and `replacement_definitions`.
    // This test verifies the full sweep on a single targeted creature, since
    // CR-correctness here is independent of keyword type.
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let abigale_oracle = "Flying, first strike, lifelink\n\
        When Abigale enters, up to one other target creature loses all abilities. \
        Put a flying counter, a first strike counter, and a lifelink counter on that creature.";
    let abigale_builder =
        scenario.add_creature_to_hand_from_oracle(P0, "Abigale", 1, 1, abigale_oracle);
    let abigale_id = abigale_builder.id();

    // Targeted creature has Defender (keyword), Menace (keyword), and an
    // activated mana ability (`{T}: Add {G}.`). All three must be stripped
    // when Abigale's trigger resolves.
    let mana_ability = AbilityDefinition::new(
        AbilityKind::Activated,
        Effect::Mana {
            produced: ManaProduction::Fixed {
                colors: vec![ManaColor::Green],
                contribution: ManaContribution::Base,
            },
            restrictions: vec![],
            grants: vec![],
            expiry: None,
            target: None,
        },
    )
    .cost(AbilityCost::Tap);

    let mut target_builder = scenario.add_creature(P1, "Llanowar Wall", 0, 4);
    target_builder
        .with_keyword(Keyword::Defender)
        .with_keyword(Keyword::Menace)
        .with_ability_definition(mana_ability);
    let target_id = target_builder.id();

    let mut runner = scenario.build();

    // Sanity: confirm starting state has all three abilities BEFORE Abigale resolves.
    {
        let pre = runner.state().objects.get(&target_id).expect("target");
        assert!(
            pre.has_keyword(&Keyword::Defender),
            "precondition: Defender"
        );
        assert!(pre.has_keyword(&Keyword::Menace), "precondition: Menace");
        assert_eq!(
            pre.abilities.len(),
            1,
            "precondition: one activated mana ability"
        );
    }

    let abigale_card_id = runner.state().objects[&abigale_id].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: abigale_id,
            card_id: abigale_card_id,
            targets: vec![],
        })
        .expect("cast Abigale");

    for _ in 0..30 {
        match runner.state().waiting_for.clone() {
            WaitingFor::TargetSelection { .. } | WaitingFor::TriggerTargetSelection { .. } => {
                runner
                    .act(GameAction::SelectTargets {
                        targets: vec![TargetRef::Object(target_id)],
                    })
                    .expect("select target");
            }
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() {
                    if runner.act(GameAction::PassPriority).is_err() {
                        break;
                    }
                    if runner.state().stack.is_empty()
                        && matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
                    {
                        break;
                    }
                } else if runner.act(GameAction::PassPriority).is_err() {
                    break;
                }
            }
            _ => break,
        }
    }

    let target = runner.state().objects.get(&target_id).expect("target");

    // All printed abilities are stripped. CR 113.3 + CR 613.1f layer 6.
    assert!(
        !target.has_keyword(&Keyword::Defender),
        "Target should have lost Defender; keywords = {:?}",
        target.keywords,
    );
    assert!(
        !target.has_keyword(&Keyword::Menace),
        "Target should have lost Menace; keywords = {:?}",
        target.keywords,
    );
    assert!(
        target.abilities.is_empty(),
        "Target should have lost its activated mana ability; abilities = {:?}",
        target.abilities,
    );

    // The granted keyword counters still apply (CR 122.1b grants at layer 6
    // from the placed counters, which carry later timestamps than the
    // RemoveAllAbilities effect).
    assert!(
        target.has_keyword(&Keyword::Flying),
        "Target should gain Flying from its flying counter; keywords = {:?}",
        target.keywords,
    );
    assert!(
        target.has_keyword(&Keyword::FirstStrike),
        "Target should gain FirstStrike from its counter; keywords = {:?}",
        target.keywords,
    );
    assert!(
        target.has_keyword(&Keyword::Lifelink),
        "Target should gain Lifelink from its counter; keywords = {:?}",
        target.keywords,
    );
}
