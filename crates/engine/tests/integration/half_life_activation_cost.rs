use engine::ai_support::legal_actions_full;
use engine::game::casting::can_activate_ability_now;
use engine::game::keywords::object_has_effective_keyword_kind;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{
    AbilityCost, PlayerScope, QuantityExpr, QuantityRef, ReplacementDefinition, RoundingMode,
    TargetFilter, TargetRef,
};
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::format::FormatConfig;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::KeywordKind;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::replacements::ReplacementEvent;
use engine::types::zones::Zone;

const BETRAYAL: &str =
    "{B}{B}, Pay half your life, rounded up: Destroy target nonblack creature. It can't be regenerated.";
const LURKING_EVIL: &str =
    "Pay half your life, rounded up: This enchantment becomes a 4/4 Phyrexian Horror creature with flying.";
const CONFLUENCE: &str = "{T}, Pay 1 life: Add one mana of any color.";
const COMPOSITE_LURKING_EVIL: &str =
    "{B}{B}, Pay half your life, rounded up: This enchantment becomes a 4/4 Phyrexian Horror creature with flying.";

fn assert_half_life_cost(cost: &AbilityCost) {
    let AbilityCost::Composite { costs } = cost else {
        panic!("expected mana and half-life components: {cost:?}");
    };
    assert_eq!(costs.len(), 2);
    assert!(matches!(costs[0], AbilityCost::Mana { .. }));
    assert!(matches!(
        &costs[1],
        AbilityCost::PayLife {
            amount: QuantityExpr::DivideRounded {
                inner,
                divisor: 2,
                rounding: RoundingMode::Up,
            },
        } if matches!(inner.as_ref(), QuantityExpr::Ref {
            qty: QuantityRef::LifeTotal { player: PlayerScope::Controller }
        })
    ));
}

fn betrayal_board(
    life: i32,
    floated: bool,
) -> (GameRunner, ObjectId, [ObjectId; 2], ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain).with_life(P0, life);
    let source = scenario
        .add_enchantment_from_oracle(P0, "Murderous Betrayal", BETRAYAL)
        .id();
    let lands = [
        scenario
            .add_land_from_oracle(P0, "Mana Confluence", CONFLUENCE)
            .id(),
        scenario
            .add_land_from_oracle(P0, "Mana Confluence", CONFLUENCE)
            .id(),
    ];
    let target = scenario
        .add_creature(P1, "Grizzly Bears", 2, 2)
        .with_replacement_definition(
            ReplacementDefinition::new(ReplacementEvent::Destroy)
                .valid_card(TargetFilter::SelfRef)
                .description("Regenerate".to_string())
                .regeneration_shield(),
        )
        .id();
    let other = scenario.add_creature(P1, "Hill Giant", 3, 3).id();
    if floated {
        scenario.with_mana_pool(
            P0,
            vec![ManaUnit::new(ManaType::Black, ObjectId(9999), false, vec![]); 2],
        );
    }
    (scenario.build(), source, lands, target, other)
}

fn offered(runner: &GameRunner, source: ObjectId) -> bool {
    let (_, _, grouped) = legal_actions_full(runner.state());
    grouped.get(&source).is_some_and(|actions| {
        actions.iter().any(|action| {
            matches!(
                action,
                GameAction::ActivateAbility { source_id, ability_index: 0 } if *source_id == source
            )
        })
    })
}

#[test]
fn targeted_half_life_locks_before_mana_abilities() {
    let (mut runner, source, lands, target, other) = betrayal_board(7, false);
    let cost = runner.state().objects[&source].abilities[0]
        .cost
        .as_ref()
        .unwrap();
    assert_half_life_cost(cost);
    let outcome = runner.activate(source, 0).target_object(target).resolve();
    assert_eq!(outcome.state().players[P0.0 as usize].life, 1);
    assert!(lands.iter().all(|id| outcome.state().objects[id].tapped));
    let added: Vec<_> = outcome
        .events()
        .iter()
        .filter_map(|event| match event {
            GameEvent::ManaAdded {
                mana_type: ManaType::Black,
                source_id,
                ..
            } => Some(*source_id),
            _ => None,
        })
        .collect();
    assert!(
        added.contains(&lands[0]) && added.contains(&lands[1]),
        "{added:?}"
    );
    let losses: Vec<_> = outcome
        .events()
        .iter()
        .filter_map(|event| match event {
            GameEvent::LifeChanged {
                player_id, amount, ..
            } if *player_id == P0 && *amount < 0 => Some(*amount),
            _ => None,
        })
        .collect();
    assert_eq!(losses, vec![-1, -1, -4]);
    // CR 701.19c: Murderous Betrayal bypasses the target's regeneration shield.
    assert_eq!(outcome.zone_of(target), Zone::Graveyard);
    assert_eq!(outcome.zone_of(other), Zone::Battlefield);
    assert!(outcome
        .events()
        .iter()
        .any(|event| matches!(event, GameEvent::StackPushed { .. })));
    assert!(outcome
        .events()
        .iter()
        .any(|event| matches!(event, GameEvent::StackResolved { .. })));
}

#[test]
fn targeted_pending_cost_is_fixed_before_first_mana_ability() {
    let (mut runner, source, lands, target, _) = betrayal_board(7, false);
    runner
        .act(GameAction::ActivateAbility {
            source_id: source,
            ability_index: 0,
        })
        .expect("activation reaches target announcement");
    let WaitingFor::TargetSelection { pending_cast, .. } = &mut runner.state_mut().waiting_for
    else {
        panic!("expected target selection before mana payment");
    };
    assert_half_life_cost(pending_cast.activation_cost.as_ref().unwrap());
    pending_cast.payment_mode = CastPaymentMode::Manual;

    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(target)),
        })
        .expect("target choice reaches manual mana window");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ManaPayment { .. }
    ));
    assert_eq!(runner.state().players[P0.0 as usize].life, 7);
    assert!(lands.iter().all(|id| !runner.state().objects[id].tapped));
    assert!(matches!(
        runner
            .state()
            .pending_cast
            .as_ref()
            .unwrap()
            .activation_cost
            .as_ref(),
        Some(AbilityCost::PayLife {
            amount: QuantityExpr::Fixed { value: 4 }
        })
    ));
}

#[test]
fn targeted_half_life_uses_same_amount_with_prefloated_mana() {
    let (mut runner, source, lands, target, other) = betrayal_board(7, true);
    let outcome = runner.activate(source, 0).target_object(target).resolve();
    assert_eq!(outcome.state().players[P0.0 as usize].life, 3);
    assert!(lands.iter().all(|id| !outcome.state().objects[id].tapped));
    assert_eq!(outcome.zone_of(target), Zone::Graveyard);
    assert_eq!(outcome.zone_of(other), Zone::Battlefield);
}

#[test]
fn offer_and_direct_activation_refuse_unpayable_locked_cost() {
    let (mut runner, source, lands, target, _) = betrayal_board(3, false);
    assert_half_life_cost(
        runner.state().objects[&source].abilities[0]
            .cost
            .as_ref()
            .unwrap(),
    );
    assert_eq!(runner.state().objects[&target].zone, Zone::Battlefield);
    assert!(!offered(&runner, source));
    assert!(!can_activate_ability_now(runner.state(), P0, source, 0));
    assert!(runner
        .act(GameAction::ActivateAbility {
            source_id: source,
            ability_index: 0
        })
        .is_err());
    assert_eq!(runner.state().players[P0.0 as usize].life, 3);
    assert!(lands.iter().all(|id| !runner.state().objects[id].tapped));
    assert!(runner.state().stack.is_empty());
    assert!(runner.state().pending_cast.is_none());
    assert_eq!(runner.state().objects[&target].zone, Zone::Battlefield);

    let (mut funded, source, lands, _target, _) = betrayal_board(3, true);
    assert!(offered(&funded, source));
    assert!(can_activate_ability_now(funded.state(), P0, source, 0));
    funded
        .act(GameAction::ActivateAbility {
            source_id: source,
            ability_index: 0,
        })
        .expect("funded activation reaches target selection");
    assert!(lands.iter().all(|id| !funded.state().objects[id].tapped));
    // The public driver resolves the same funded board and chosen target.
    let (mut funded, source, _, target, _) = betrayal_board(3, true);
    let outcome = funded.activate(source, 0).target_object(target).resolve();
    assert_eq!(outcome.state().players[P0.0 as usize].life, 1);
    assert_eq!(outcome.zone_of(target), Zone::Graveyard);
}

#[test]
fn untargeted_half_life_pays_rounded_amount() {
    for (life, payment) in [(20, 10), (7, 4), (1, 1), (0, 0), (-2, 0)] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain).with_life(P0, life);
        scenario
            .add_creature(P0, "Platinum Angel", 4, 4)
            .as_artifact()
            .from_oracle_text("You can't lose the game and your opponents can't win the game.");
        let source = scenario
            .add_enchantment_from_oracle(P0, "Lurking Evil", LURKING_EVIL)
            .id();
        let mut runner = scenario.build();
        assert!(matches!(
            runner.state().objects[&source].abilities[0].cost,
            Some(AbilityCost::PayLife {
                amount: QuantityExpr::DivideRounded {
                    rounding: RoundingMode::Up,
                    ..
                }
            })
        ));
        let outcome = runner.activate(source, 0).resolve();
        assert_eq!(outcome.state().players[P0.0 as usize].life, life - payment);
        // CR 613.1g: Layer 7 applies the resolved power/toughness change.
        assert_eq!(outcome.power_toughness(source), (4, 4));
        // CR 613.1f: Layer 6 applies the resolved flying grant.
        assert!(object_has_effective_keyword_kind(
            outcome.state(),
            source,
            KeywordKind::Flying,
        ));
        // CR 613.1d: Layer 4 applies the resolved creature subtypes.
        assert_eq!(
            outcome.state().objects[&source].card_types.subtypes,
            vec!["Phyrexian".to_string(), "Horror".to_string()],
        );
        assert!(outcome
            .events()
            .iter()
            .any(|event| matches!(event, GameEvent::StackPushed { .. })));
        assert!(outcome
            .events()
            .iter()
            .any(|event| matches!(event, GameEvent::StackResolved { .. })));
    }
}

#[test]
fn untargeted_composite_half_life_locks_before_two_mana_abilities() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain).with_life(P0, 7);
    let source = scenario
        .add_enchantment_from_oracle(P0, "Half-Life Enchantment", COMPOSITE_LURKING_EVIL)
        .id();
    let lands = [
        scenario
            .add_land_from_oracle(P0, "Mana Confluence", CONFLUENCE)
            .id(),
        scenario
            .add_land_from_oracle(P0, "Mana Confluence", CONFLUENCE)
            .id(),
    ];
    let mut runner = scenario.build();
    assert_half_life_cost(
        runner.state().objects[&source].abilities[0]
            .cost
            .as_ref()
            .unwrap(),
    );
    let outcome = runner.activate(source, 0).resolve();
    assert_eq!(outcome.state().players[P0.0 as usize].life, 1);
    assert!(lands.iter().all(|id| outcome.state().objects[id].tapped));
    let added: Vec<_> = outcome
        .events()
        .iter()
        .filter_map(|event| match event {
            GameEvent::ManaAdded {
                mana_type: ManaType::Black,
                source_id,
                ..
            } => Some(*source_id),
            _ => None,
        })
        .collect();
    assert!(added.contains(&lands[0]) && added.contains(&lands[1]));
    let losses: Vec<_> = outcome
        .events()
        .iter()
        .filter_map(|event| match event {
            GameEvent::LifeChanged {
                player_id, amount, ..
            } if *player_id == P0 && *amount < 0 => Some(*amount),
            _ => None,
        })
        .collect();
    assert_eq!(losses, vec![-1, -1, -4]);
    assert!(outcome
        .events()
        .iter()
        .any(|event| matches!(event, GameEvent::StackPushed { .. })));
    assert!(outcome
        .events()
        .iter()
        .any(|event| matches!(event, GameEvent::StackResolved { .. })));
}

#[test]
fn positive_half_life_cost_is_refused_when_players_cannot_pay_life() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain).with_life(P0, 7);
    let source = scenario
        .add_enchantment_from_oracle(P0, "Lurking Evil", LURKING_EVIL)
        .id();
    scenario.add_creature_from_oracle(
        P0,
        "Yasharn, Implacable Earth",
        4,
        4,
        "When Yasharn enters, search your library for a basic Forest card and a basic Plains card, reveal those cards, put them into your hand, then shuffle.\nPlayers can't pay life or sacrifice nonland permanents to cast spells or activate abilities.",
    );
    let mut runner = scenario.build();
    assert!(matches!(
        runner.state().objects[&source].abilities[0].cost,
        Some(AbilityCost::PayLife {
            amount: QuantityExpr::DivideRounded { .. }
        })
    ));
    assert!(!offered(&runner, source));
    assert!(!can_activate_ability_now(runner.state(), P0, source, 0));
    assert!(runner
        .act(GameAction::ActivateAbility {
            source_id: source,
            ability_index: 0,
        })
        .is_err());
    assert_eq!(runner.state().players[P0.0 as usize].life, 7);
    assert!(runner.state().stack.is_empty());
    assert!(runner.state().pending_cast.is_none());
}

#[test]
fn positive_half_life_cost_is_refused_when_life_cannot_change() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain).with_life(P0, 7);
    let source = scenario
        .add_enchantment_from_oracle(P0, "Lurking Evil", LURKING_EVIL)
        .id();
    scenario
        .add_creature_from_oracle(
            P0,
            "Platinum Emperion",
            8,
            8,
            "Your life total can't change.",
        )
        .as_artifact();
    let mut runner = scenario.build();
    assert!(matches!(
        runner.state().objects[&source].abilities[0].cost,
        Some(AbilityCost::PayLife {
            amount: QuantityExpr::DivideRounded { .. }
        })
    ));
    assert!(!offered(&runner, source));
    assert!(!can_activate_ability_now(runner.state(), P0, source, 0));
    assert!(runner
        .act(GameAction::ActivateAbility {
            source_id: source,
            ability_index: 0,
        })
        .is_err());
    assert_eq!(runner.state().players[P0.0 as usize].life, 7);
    assert!(runner.state().stack.is_empty());
}

#[test]
fn half_life_activation_reads_two_headed_giant_team_total() {
    // CR 810.9a: A cost reading a player's life total uses the team's total.
    let mut scenario = GameScenario::new_with_format(FormatConfig::two_headed_giant(), 4, 42);
    scenario
        .at_phase(Phase::PreCombatMain)
        .with_life(P0, 6)
        .with_life(P1, 5);
    let source = scenario
        .add_enchantment_from_oracle(P0, "Lurking Evil", LURKING_EVIL)
        .id();
    let mut runner = scenario.build();
    assert_eq!(
        engine::game::players::team_life_total(runner.state(), P0),
        11
    );
    let outcome = runner.activate(source, 0).resolve();
    assert_eq!(
        engine::game::players::team_life_total(outcome.state(), P0),
        5
    );
    assert_eq!(outcome.state().players[P0.0 as usize].life, 0);
    assert_eq!(outcome.state().players[P1.0 as usize].life, 5);
    assert!(outcome
        .events()
        .iter()
        .any(|event| matches!(event, GameEvent::StackResolved { .. })));
}
