//! Issue #5252: a permanent tapped for mana while casting a spell must remain
//! selectable for a non-mana additional sacrifice cost on that same spell.

use engine::game::scenario::{GameScenario, P0};
use engine::types::ability::{
    AbilityCost, AbilityDefinition, AbilityKind, AdditionalCost, Effect, FilterProp,
    ManaProduction, ParsedCondition, QuantityExpr, SacrificeCost, SpellCastingOption, TargetFilter,
    TypeFilter, TypedFilter,
};
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, PayCostKind, StackEntryKind, WaitingFor};
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard};
use engine::types::phase::Phase;
use engine::types::statics::StaticMode;
use engine::types::zones::Zone;

fn setup_artifact_mana_source_sacrifice_spell(
    payment_mode: CastPaymentMode,
) -> (
    engine::game::scenario::GameRunner,
    engine::types::identifiers::ObjectId,
    engine::types::identifiers::ObjectId,
    engine::types::identifiers::ObjectId,
) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Tinker-shaped Draw", false, "Draw a card.")
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Blue],
            generic: 2,
        })
        .with_additional_cost(AdditionalCost::Required(AbilityCost::Sacrifice(
            SacrificeCost::count(
                TargetFilter::Typed(TypedFilter::new(TypeFilter::Artifact)),
                1,
            ),
        )))
        .id();

    let island_a = scenario.add_basic_land(P0, ManaColor::Blue);
    let island_b = scenario.add_basic_land(P0, ManaColor::Blue);

    let lens = scenario
        .add_creature(P0, "Prismatic Lens-shaped Rock", 0, 1)
        .as_artifact()
        .with_ability_definition(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Mana {
                    produced: ManaProduction::Colorless {
                        count: QuantityExpr::Fixed { value: 1 },
                    },
                    restrictions: vec![],
                    grants: vec![],
                    expiry: None,
                    target: None,
                },
            )
            .cost(AbilityCost::Tap),
        )
        .id();

    let mut runner = scenario.build();
    let card_id = runner.state().objects[&spell].card_id;

    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode,
        })
        .expect("begin casting spell with artifact sacrifice additional cost");

    (runner, lens, island_a, island_b)
}

#[test]
fn tapped_artifact_mana_source_can_pay_spell_and_be_sacrificed_as_additional_cost() {
    let (mut runner, lens, _, _) =
        setup_artifact_mana_source_sacrifice_spell(CastPaymentMode::Auto);

    match runner.state().waiting_for.clone() {
        WaitingFor::PayCost {
            kind: PayCostKind::Sacrifice,
            choices,
            ..
        } => assert!(
            choices.contains(&lens),
            "artifact mana source must be eligible for the sacrifice cost"
        ),
        other => panic!("expected sacrifice choice, got {other:?}"),
    }

    runner
        .act(GameAction::SelectCards { cards: vec![lens] })
        .expect("selected artifact should pay sacrifice cost after auto-tap mana");

    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "spell should be fully cast after using the artifact for mana and sacrifice"
    );
    assert!(
        runner.state().objects[&lens].tapped,
        "artifact should have tapped for mana before being sacrificed"
    );
    assert_eq!(
        runner.state().objects[&lens].zone,
        Zone::Graveyard,
        "artifact should be sacrificed after mana payment"
    );
    let actual_mana_spent = runner
        .state()
        .stack
        .iter()
        .find_map(|entry| match &entry.kind {
            StackEntryKind::Spell {
                actual_mana_spent, ..
            } => Some(*actual_mana_spent),
            _ => None,
        })
        .expect("spell should be on the stack after casting");
    assert_eq!(
        actual_mana_spent, 3,
        "deferred sacrifice path must preserve the spell's actual mana spent"
    );
    let spell_object = runner
        .state()
        .stack
        .iter()
        .find_map(|entry| match &entry.kind {
            StackEntryKind::Spell { .. } => Some(entry.id),
            _ => None,
        })
        .and_then(|id| runner.state().objects.get(&id))
        .expect("spell object should remain tracked while on the stack");
    assert_eq!(
        spell_object.mana_spent_to_cast_amount, 3,
        "deferred sacrifice path must preserve object-level mana-spent amount"
    );
    assert!(
        spell_object.colors_spent_to_cast.blue > 0,
        "deferred sacrifice path must preserve object-level colors spent"
    );
}

#[test]
fn manual_payment_defers_selected_artifact_sacrifice_until_mana_payment_commit() {
    let (mut runner, lens, island_a, island_b) =
        setup_artifact_mana_source_sacrifice_spell(CastPaymentMode::Manual);

    runner
        .act(GameAction::SelectCards { cards: vec![lens] })
        .expect("select artifact for sacrifice cost");

    assert!(
        matches!(runner.state().waiting_for, WaitingFor::ManaPayment { .. }),
        "manual payment should pause for mana after selecting the sacrifice"
    );
    assert_eq!(
        runner.state().objects[&lens].zone,
        Zone::Battlefield,
        "selected artifact must remain on the battlefield during the mana-ability window"
    );

    runner
        .act(GameAction::ActivateAbility {
            source_id: lens,
            ability_index: 0,
        })
        .expect("selected artifact should still be activatable for mana");
    runner
        .act(GameAction::ActivateAbility {
            source_id: island_a,
            ability_index: 0,
        })
        .expect("first Island should be activatable for mana");
    runner
        .act(GameAction::ActivateAbility {
            source_id: island_b,
            ability_index: 0,
        })
        .expect("second Island should be activatable for mana");

    runner
        .act(GameAction::PassPriority)
        .expect("manual payment commit should use the artifact for mana then sacrifice it");

    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "spell should finish casting after manual mana payment"
    );
    assert!(runner.state().objects[&lens].tapped);
    assert_eq!(runner.state().objects[&lens].zone, Zone::Graveyard);
}

#[test]
fn failed_manual_payment_does_not_consume_deferred_sacrifice() {
    let (mut runner, lens, _, _) =
        setup_artifact_mana_source_sacrifice_spell(CastPaymentMode::Manual);

    runner
        .act(GameAction::SelectCards { cards: vec![lens] })
        .expect("select artifact for sacrifice cost");

    let commit = runner.act(GameAction::PassPriority);
    assert!(
        commit.is_err(),
        "manual payment without activating the selected mana source should fail"
    );
    assert_eq!(
        runner.state().objects[&lens].zone,
        Zone::Battlefield,
        "failed payment commit must not consume the deferred sacrifice permanent"
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::ManaPayment { .. }),
        "failed payment commit should keep the cast in the mana payment window"
    );
}

#[test]
fn deferred_sacrifice_revalidates_cost_filter_at_commit() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Tinker-shaped Draw", false, "Draw a card.")
        .with_mana_cost(ManaCost::generic(1))
        .with_additional_cost(AdditionalCost::Required(AbilityCost::Sacrifice(
            SacrificeCost::count(
                TargetFilter::Typed(
                    TypedFilter::new(TypeFilter::Artifact).properties(vec![FilterProp::Untapped]),
                ),
                1,
            ),
        )))
        .id();

    let lens = scenario
        .add_creature(P0, "Prismatic Lens-shaped Rock", 0, 1)
        .as_artifact()
        .with_ability_definition(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Mana {
                    produced: ManaProduction::Colorless {
                        count: QuantityExpr::Fixed { value: 1 },
                    },
                    restrictions: vec![],
                    grants: vec![],
                    expiry: None,
                    target: None,
                },
            )
            .cost(AbilityCost::Tap),
        )
        .id();

    let mut runner = scenario.build();
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Manual,
        })
        .expect("begin casting spell with untapped-artifact sacrifice cost");

    runner
        .act(GameAction::SelectCards { cards: vec![lens] })
        .expect("select currently untapped artifact for sacrifice cost");
    runner
        .act(GameAction::ActivateAbility {
            source_id: lens,
            ability_index: 0,
        })
        .expect("selected artifact can still activate its tap mana ability");

    let commit = runner.act(GameAction::PassPriority);
    assert!(
        commit.is_err(),
        "deferred sacrifice must recheck the original cost filter before commit"
    );
    assert!(
        runner.state().objects[&lens].tapped,
        "the artifact became ineligible by tapping during the mana window"
    );
    assert_eq!(
        runner.state().objects[&lens].zone,
        Zone::Battlefield,
        "commit rejection must not sacrifice a permanent that no longer matches the cost"
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::ManaPayment { .. }),
        "failed commit should keep the cast in the mana payment window"
    );
}

#[test]
fn auto_deferred_sacrifice_does_not_tap_source_if_filter_would_be_invalidated() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Tinker-shaped Draw", false, "Draw a card.")
        .with_mana_cost(ManaCost::generic(1))
        .with_additional_cost(AdditionalCost::Required(AbilityCost::Sacrifice(
            SacrificeCost::count(
                TargetFilter::Typed(
                    TypedFilter::new(TypeFilter::Artifact).properties(vec![FilterProp::Untapped]),
                ),
                1,
            ),
        )))
        .id();

    let lens = scenario
        .add_creature(P0, "Prismatic Lens-shaped Rock", 0, 1)
        .as_artifact()
        .with_ability_definition(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Mana {
                    produced: ManaProduction::Colorless {
                        count: QuantityExpr::Fixed { value: 1 },
                    },
                    restrictions: vec![],
                    grants: vec![],
                    expiry: None,
                    target: None,
                },
            )
            .cost(AbilityCost::Tap),
        )
        .id();

    let mut runner = scenario.build();
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("begin auto cast with untapped-artifact sacrifice cost");

    let selection = runner.act(GameAction::SelectCards { cards: vec![lens] });
    assert!(
        selection.is_err(),
        "auto payment must not use a selected artifact for mana if tapping invalidates the sacrifice cost"
    );
    assert!(
        !runner.state().objects[&lens].tapped,
        "rejected auto payment must not commit the invalidating tap"
    );
    assert_eq!(
        runner.state().objects[&lens].zone,
        Zone::Battlefield,
        "rejected auto payment must leave the deferred sacrifice permanent in place"
    );
    let pool_total = runner
        .state()
        .players
        .iter()
        .find(|player| player.id == P0)
        .map(|player| player.mana_pool.total())
        .unwrap_or(0);
    assert_eq!(
        pool_total, 0,
        "rejected auto payment must not commit floated mana"
    );
}

#[test]
fn x_cost_spell_defers_artifact_sacrifice_through_x_choice_and_mana_payment() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Tinker-shaped X Draw", false, "Draw X cards.")
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::X, ManaCostShard::Blue],
            generic: 0,
        })
        .with_additional_cost(AdditionalCost::Required(AbilityCost::Sacrifice(
            SacrificeCost::count(
                TargetFilter::Typed(TypedFilter::new(TypeFilter::Artifact)),
                1,
            ),
        )))
        .id();

    let island = scenario.add_basic_land(P0, ManaColor::Blue);
    let lens = scenario
        .add_creature(P0, "Prismatic Lens-shaped Rock", 0, 1)
        .as_artifact()
        .with_ability_definition(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Mana {
                    produced: ManaProduction::Colorless {
                        count: QuantityExpr::Fixed { value: 1 },
                    },
                    restrictions: vec![],
                    grants: vec![],
                    expiry: None,
                    target: None,
                },
            )
            .cost(AbilityCost::Tap),
        )
        .id();

    let mut runner = scenario.build();
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Manual,
        })
        .expect("begin casting X spell with artifact sacrifice additional cost");

    runner
        .act(GameAction::SelectCards { cards: vec![lens] })
        .expect("select artifact for sacrifice before choosing X");
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::ChooseXValue { .. }),
        "X must be chosen while the selected sacrifice artifact is still reserved"
    );
    assert_eq!(
        runner.state().objects[&lens].zone,
        Zone::Battlefield,
        "selected artifact must not be sacrificed before X is chosen"
    );

    runner
        .act(GameAction::ChooseX { value: 1 })
        .expect("choose X for the spell");
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::ManaPayment { .. }),
        "manual X cast should proceed to mana payment after choosing X"
    );
    runner
        .act(GameAction::ActivateAbility {
            source_id: lens,
            ability_index: 0,
        })
        .expect("selected artifact should remain activatable for X mana");
    runner
        .act(GameAction::ActivateAbility {
            source_id: island,
            ability_index: 0,
        })
        .expect("Island should pay the blue shard");
    runner
        .act(GameAction::PassPriority)
        .expect("manual payment should spend mana then sacrifice the selected artifact");

    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "X spell should finish casting after deferred sacrifice commit"
    );
    assert!(runner.state().objects[&lens].tapped);
    assert_eq!(runner.state().objects[&lens].zone, Zone::Graveyard);
}

#[test]
fn duplicate_deferred_sacrifice_selection_is_rejected_before_payment() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Double Tinker-shaped Draw", false, "Draw a card.")
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Blue],
            generic: 1,
        })
        .with_additional_cost(AdditionalCost::Required(AbilityCost::Sacrifice(
            SacrificeCost::count(
                TargetFilter::Typed(TypedFilter::new(TypeFilter::Artifact)),
                2,
            ),
        )))
        .id();

    scenario.add_basic_land(P0, ManaColor::Blue);
    scenario.add_basic_land(P0, ManaColor::Blue);

    let artifact = scenario
        .add_creature(P0, "Single Artifact", 0, 1)
        .as_artifact()
        .id();
    scenario
        .add_creature(P0, "Second Artifact", 0, 1)
        .as_artifact();

    let mut runner = scenario.build();
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Manual,
        })
        .expect("begin casting spell with artifact sacrifice additional cost");

    let selection = runner.act(GameAction::SelectCards {
        cards: vec![artifact, artifact],
    });
    assert!(
        selection.is_err(),
        "duplicate sacrifice selections must be rejected before deferred payment"
    );
    assert_eq!(runner.state().objects[&artifact].zone, Zone::Battlefield);
    assert!(
        !runner.state().objects[&artifact].tapped,
        "duplicate rejection must not pay any mana source cost"
    );
}

#[test]
fn deferred_sacrifice_artifact_cannot_be_consumed_by_mana_ability_cost() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Tinker-shaped Draw", false, "Draw a card.")
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Blue],
            generic: 2,
        })
        .with_additional_cost(AdditionalCost::Required(AbilityCost::Sacrifice(
            SacrificeCost::count(
                TargetFilter::Typed(TypedFilter::new(TypeFilter::Artifact)),
                1,
            ),
        )))
        .id();

    scenario.add_basic_land(P0, ManaColor::Blue);
    scenario.add_basic_land(P0, ManaColor::Blue);

    let treasure = scenario
        .add_creature(P0, "Treasure-shaped Rock", 0, 1)
        .as_artifact()
        .with_ability_definition(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Mana {
                    produced: ManaProduction::Colorless {
                        count: QuantityExpr::Fixed { value: 1 },
                    },
                    restrictions: vec![],
                    grants: vec![],
                    expiry: None,
                    target: None,
                },
            )
            .cost(AbilityCost::Sacrifice(SacrificeCost::count(
                TargetFilter::SelfRef,
                1,
            ))),
        )
        .id();

    let mut runner = scenario.build();
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Manual,
        })
        .expect("begin casting spell with artifact sacrifice additional cost");

    runner
        .act(GameAction::SelectCards {
            cards: vec![treasure],
        })
        .expect("select Treasure-shaped artifact for the spell sacrifice cost");

    let activation = runner.act(GameAction::ActivateAbility {
        source_id: treasure,
        ability_index: 0,
    });
    assert!(
        activation.is_err(),
        "a permanent already committed to the spell sacrifice cost must not be sacrificed for mana"
    );
    assert_eq!(
        runner.state().objects[&treasure].zone,
        Zone::Battlefield,
        "failed mana activation must leave the deferred sacrifice permanent in place"
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::ManaPayment { .. }),
        "failed mana activation should keep the cast in the mana payment window"
    );
}

#[test]
fn composite_mana_ability_cost_cannot_partially_pay_with_deferred_sacrifice_artifact() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Tinker-shaped Draw", false, "Draw a card.")
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Blue],
            generic: 2,
        })
        .with_additional_cost(AdditionalCost::Required(AbilityCost::Sacrifice(
            SacrificeCost::count(
                TargetFilter::Typed(TypedFilter::new(TypeFilter::Artifact)),
                1,
            ),
        )))
        .id();

    scenario.add_basic_land(P0, ManaColor::Blue);
    scenario.add_basic_land(P0, ManaColor::Blue);

    let artifact = scenario
        .add_creature(P0, "Composite Treasure-shaped Rock", 0, 1)
        .as_artifact()
        .with_ability_definition(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Mana {
                    produced: ManaProduction::Colorless {
                        count: QuantityExpr::Fixed { value: 1 },
                    },
                    restrictions: vec![],
                    grants: vec![],
                    expiry: None,
                    target: None,
                },
            )
            .cost(AbilityCost::Composite {
                costs: vec![
                    AbilityCost::Tap,
                    AbilityCost::Sacrifice(SacrificeCost::count(TargetFilter::SelfRef, 1)),
                ],
            }),
        )
        .id();

    let mut runner = scenario.build();
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Manual,
        })
        .expect("begin casting spell with artifact sacrifice additional cost");

    runner
        .act(GameAction::SelectCards {
            cards: vec![artifact],
        })
        .expect("select artifact for the spell sacrifice cost");

    let activation = runner.act(GameAction::ActivateAbility {
        source_id: artifact,
        ability_index: 0,
    });
    assert!(
        activation.is_err(),
        "reserved artifact must not pay a composite tap-and-sacrifice mana ability cost"
    );
    assert!(
        !runner.state().objects[&artifact].tapped,
        "rejected composite mana ability must not partially tap the reserved artifact"
    );
    assert_eq!(
        runner.state().objects[&artifact].zone,
        Zone::Battlefield,
        "rejected composite mana ability must leave the deferred sacrifice permanent in place"
    );
}

#[test]
fn deferred_sacrifice_artifact_granting_spend_permission_is_paid_before_it_leaves() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Tinker-shaped Draw", false, "Draw a card.")
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Blue],
            generic: 0,
        })
        .with_additional_cost(AdditionalCost::Required(AbilityCost::Sacrifice(
            SacrificeCost::count(
                TargetFilter::Typed(TypedFilter::new(TypeFilter::Artifact)),
                1,
            ),
        )))
        .id();

    let artifact = scenario
        .add_creature(P0, "Chromatic Lens-shaped Rock", 0, 1)
        .as_artifact()
        .with_static(StaticMode::SpendManaAsAnyColor {
            spell_filter: None,
            activation_source_filter: None,
        })
        .with_ability_definition(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Mana {
                    produced: ManaProduction::Colorless {
                        count: QuantityExpr::Fixed { value: 1 },
                    },
                    restrictions: vec![],
                    grants: vec![],
                    expiry: None,
                    target: None,
                },
            )
            .cost(AbilityCost::Tap),
        )
        .id();

    let mut runner = scenario.build();
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("begin casting spell with artifact sacrifice additional cost");

    runner
        .act(GameAction::SelectCards {
            cards: vec![artifact],
        })
        .expect("colorless mana should be spent with the selected artifact's permission before sacrifice");

    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "spell should finish casting after spending mana before the permission source leaves"
    );
    assert!(runner.state().objects[&artifact].tapped);
    assert_eq!(runner.state().objects[&artifact].zone, Zone::Graveyard);
}

#[test]
fn deferred_sacrifice_permanent_is_not_exile_cost_choice_for_mana_ability() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Tinker-shaped Draw", false, "Draw a card.")
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Blue],
            generic: 2,
        })
        .with_additional_cost(AdditionalCost::Required(AbilityCost::Sacrifice(
            SacrificeCost::count(
                TargetFilter::Typed(TypedFilter::new(TypeFilter::Artifact)),
                1,
            ),
        )))
        .id();

    scenario.add_basic_land(P0, ManaColor::Blue);
    scenario.add_basic_land(P0, ManaColor::Blue);
    scenario.add_basic_land(P0, ManaColor::Blue);

    let reserved_artifact = scenario
        .add_creature(P0, "Artifact Creature to Sacrifice", 1, 1)
        .as_artifact()
        .id();
    let unreserved_creature = scenario.add_creature(P0, "Unreserved Creature", 1, 1).id();

    let altar = scenario
        .add_creature(P0, "Food Chain-shaped Altar", 0, 1)
        .as_artifact()
        .with_ability_definition(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Mana {
                    produced: ManaProduction::Colorless {
                        count: QuantityExpr::Fixed { value: 1 },
                    },
                    restrictions: vec![],
                    grants: vec![],
                    expiry: None,
                    target: None,
                },
            )
            .cost(AbilityCost::Exile {
                count: 1,
                zone: None,
                filter: Some(TargetFilter::Typed(TypedFilter::new(TypeFilter::Creature))),
            }),
        )
        .id();

    let mut runner = scenario.build();
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Manual,
        })
        .expect("begin casting spell with artifact sacrifice additional cost");

    runner
        .act(GameAction::SelectCards {
            cards: vec![reserved_artifact],
        })
        .expect("select artifact creature for the spell sacrifice cost");

    runner
        .act(GameAction::ActivateAbility {
            source_id: altar,
            ability_index: 0,
        })
        .expect("activate exile-for-mana ability");

    match runner.state().waiting_for.clone() {
        WaitingFor::PayCost {
            kind: PayCostKind::ExileFromManaZone { .. },
            choices,
            ..
        } => {
            assert!(
                !choices.contains(&reserved_artifact),
                "reserved spell sacrifice permanent must not be offered for exile mana costs"
            );
            assert!(
                choices.contains(&unreserved_creature),
                "unreserved eligible creature should remain available"
            );
        }
        other => panic!("expected exile cost choice, got {other:?}"),
    }
    assert_eq!(
        runner.state().objects[&reserved_artifact].zone,
        Zone::Battlefield
    );
}

#[test]
fn finalize_rejection_happens_before_deferred_sacrifice_costs_are_paid() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::End);

    let spell = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Conditional Ward-shaped Sorcery",
            false,
            "Destroy target creature.",
        )
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Blue],
            generic: 0,
        })
        .with_additional_cost(AdditionalCost::Required(AbilityCost::Sacrifice(
            SacrificeCost::count(
                TargetFilter::Typed(TypedFilter::new(TypeFilter::Artifact)),
                1,
            ),
        )))
        .id();

    let commander = scenario.add_creature(P0, "Commander Creature", 2, 2).id();
    let plain = scenario.add_creature(P0, "Plain Creature", 2, 2).id();
    let artifact = scenario
        .add_creature(P0, "Chromatic Lens-shaped Rock", 0, 1)
        .as_artifact()
        .with_static(StaticMode::SpendManaAsAnyColor {
            spell_filter: None,
            activation_source_filter: None,
        })
        .with_ability_definition(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Mana {
                    produced: ManaProduction::Colorless {
                        count: QuantityExpr::Fixed { value: 1 },
                    },
                    restrictions: vec![],
                    grants: vec![],
                    expiry: None,
                    target: None,
                },
            )
            .cost(AbilityCost::Tap),
        )
        .id();

    let mut runner = scenario.build();
    runner.state_mut().active_player = engine::game::scenario::P1;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P0 };
    runner
        .state_mut()
        .objects
        .get_mut(&commander)
        .unwrap()
        .is_commander = true;
    runner
        .state_mut()
        .objects
        .get_mut(&spell)
        .unwrap()
        .casting_options
        .push(
            SpellCastingOption::as_though_had_flash().condition(
                ParsedCondition::SpellTargetsFilter {
                    filter: TargetFilter::Typed(
                        TypedFilter::new(TypeFilter::Creature)
                            .properties(vec![FilterProp::IsCommander]),
                    ),
                },
            ),
        );

    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("conditional flash should allow cast announcement before target choice");
    let result = runner.act(GameAction::SelectTargets {
        targets: vec![engine::types::ability::TargetRef::Object(plain)],
    });
    assert!(
        result.is_err(),
        "non-commander target must reject before deferred sacrifice costs are paid"
    );
    assert!(
        !runner.state().objects[&artifact].tapped,
        "rejected cast must not tap the deferred sacrifice artifact for mana"
    );
    assert_eq!(
        runner.state().objects[&artifact].zone,
        Zone::Battlefield,
        "rejected cast must not sacrifice the deferred artifact"
    );
    let pool_total = runner
        .state()
        .players
        .iter()
        .find(|player| player.id == P0)
        .map(|player| player.mana_pool.total())
        .unwrap_or(0);
    assert_eq!(pool_total, 0, "rejected cast must not leave floated mana");
    assert!(
        runner.state().pending_cast.is_none(),
        "terminal cast rejection must not restore a stale pending cast"
    );
    assert!(
        runner.state().stack.is_empty(),
        "terminal cast rejection must remove the announcement stack entry"
    );
}

#[test]
fn auto_payment_does_not_consume_deferred_sacrifice_artifact_for_mana() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Tinker-shaped Draw", false, "Draw a card.")
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Blue],
            generic: 2,
        })
        .with_additional_cost(AdditionalCost::Required(AbilityCost::Sacrifice(
            SacrificeCost::count(
                TargetFilter::Typed(TypedFilter::new(TypeFilter::Artifact)),
                1,
            ),
        )))
        .id();

    scenario.add_basic_land(P0, ManaColor::Blue);
    scenario.add_basic_land(P0, ManaColor::Blue);

    let treasure = scenario
        .add_creature(P0, "Treasure-shaped Rock", 0, 1)
        .as_artifact()
        .with_ability_definition(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Mana {
                    produced: ManaProduction::Colorless {
                        count: QuantityExpr::Fixed { value: 1 },
                    },
                    restrictions: vec![],
                    grants: vec![],
                    expiry: None,
                    target: None,
                },
            )
            .cost(AbilityCost::Sacrifice(SacrificeCost::count(
                TargetFilter::SelfRef,
                1,
            ))),
        )
        .id();

    let mut runner = scenario.build();
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("begin casting spell with artifact sacrifice additional cost");

    let selection = runner.act(GameAction::SelectCards {
        cards: vec![treasure],
    });
    assert!(
        selection.is_ok(),
        "auto payment should pause instead of consuming the selected artifact for mana"
    );
    assert_eq!(
        runner.state().objects[&treasure].zone,
        Zone::Battlefield,
        "auto payment must not sacrifice the selected artifact for mana before the spell cost"
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::ManaPayment { .. }),
        "unpayable auto payment should leave the cast in the mana payment window"
    );

    let retry = runner.act(GameAction::PassPriority);
    assert!(
        retry.is_err(),
        "retrying the unpayable auto fallback must fail without partial payment"
    );
    let tapped_sources = runner
        .state()
        .battlefield
        .iter()
        .filter(|id| runner.state().objects[id].tapped)
        .count();
    assert_eq!(
        tapped_sources, 0,
        "failed auto retry must not partially tap other mana sources"
    );
    let pool_total = runner
        .state()
        .players
        .iter()
        .find(|player| player.id == P0)
        .map(|player| player.mana_pool.total())
        .unwrap_or(0);
    assert_eq!(pool_total, 0, "failed auto retry must not float mana");
}
