//! Runtime coverage for the "Whenever you activate a loyalty ability of
//! [planeswalker]" trigger class (CR 606.2) — the GAP proofs from the
//! reviewed implementation plan.
//!
//! The discriminator design adds `ActivatedAbilityKind::{Normal,Loyalty}` to the
//! single `GameEvent::AbilityActivated` event (no new event), and a
//! `TriggerMode::LoyaltyAbilityActivated` matcher gated on `kind == Loyalty`.
//! The seam most likely to silently no-fire is the **targeted** loyalty
//! activation finalize (`game/casting_targets.rs`), where the kind is derived
//! from `pending.activation_cost`. These tests drive a *targeted* minus-loyalty
//! ability (two legal creature targets force the `TargetSelection` window and so
//! route through that finalize seam) and assert the trigger fires there.
//!
//! CARD TEXT (verified against MTGJSON AtomicCards.json):
//!   * Keral Keep Disciples — "Whenever you activate a loyalty ability of a
//!     Chandra planeswalker, this creature deals 1 damage to each opponent."
//!   * Elspeth's Talent — "... of enchanted planeswalker, creatures you control
//!     get +2/+2 and gain vigilance until end of turn."
//!
//! The loyalty ability itself is a generic targeted minus-loyalty ability built
//! from shipped building blocks (CR 606.1 loyalty cost + `Effect::DealDamage` to
//! `target creature`); the cards under test only need *some* loyalty ability of
//! the right planeswalker to be activated through the targeted finalize seam.

use std::sync::Arc;

use engine::game::game_object::AttachTarget;
use engine::game::scenario::{GameRunner, GameScenario};
use engine::game::zones::create_object;
use engine::types::ability::{
    AbilityCost, AbilityDefinition, AbilityKind, CounterCostSelection, Effect, QuantityExpr,
    TargetFilter, TargetRef, TypeFilter, TypedFilter,
};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::counter::{CounterMatch, CounterType};
use engine::types::events::{ActivatedAbilityKind, GameEvent};
use engine::types::game_state::{StackEntryKind, WaitingFor};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::mana::{parse_mtgjson_mana_cost, ManaColor};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const P0: PlayerId = PlayerId(0);

const KERAL_ORACLE: &str =
    "Whenever you activate a loyalty ability of a Chandra planeswalker, this creature deals 1 damage to each opponent.";
const ELSPETH_TALENT_ORACLE: &str =
    "Whenever you activate a loyalty ability of enchanted planeswalker, creatures you control get +2/+2 and gain vigilance until end of turn.";
const AJANI_ORACLE: &str = "Whenever you activate a loyalty ability, create a 2/2 colorless Wizard Soldier creature token named Cadet.\n[+1]: Creatures you control get +1/+0 and gain haste until end of turn.\n[−2]: Discard your hand, then draw a card for each creature you control.\n[−3]: Ajani deals 4 damage to each creature except for tokens you control.";
const WAY_OF_THE_MIND_SCULPTOR_ORACLE: &str = "When Way of the Mind Sculptor enters, empower Jace 5. (Put five loyalty counters on a Jace token you control. If you don't control one, first create a blue Jace planeswalker token with \"[−1]: Surveil 1\" and \"[−3]: Draw a card.\")\nWhenever you activate a loyalty ability, if you removed two or more loyalty counters to activate it, draw a card.";
const CHANDRA_AWAKENED_INFERNO_ORACLE: &str = "This spell can't be countered.\n[+2]: Each opponent gets an emblem with \"At the beginning of your upkeep, this emblem deals 1 damage to you.\"\n[−3]: Chandra deals 3 damage to each non-Elemental creature.\n[−X]: Chandra deals X damage to target creature or planeswalker. If a permanent dealt damage this way would die this turn, exile it instead.";
const STIFLE_ORACLE: &str =
    "Counter target activated or triggered ability. (Mana abilities can't be targeted.)";

/// A targeted minus-loyalty ability: CR 606.1 loyalty cost (`[−2]`, an
/// `AbilityCost::Loyalty`) + `Effect::DealDamage 2` to `target creature`. Two
/// legal creatures force a `TargetSelection` window, routing activation through
/// the targeted finalize seam (`casting_targets.rs`).
fn targeted_minus_loyalty_ability() -> AbilityDefinition {
    AbilityDefinition::new(
        AbilityKind::Activated,
        Effect::DealDamage {
            amount: QuantityExpr::Fixed { value: 2 },
            target: TargetFilter::Typed(TypedFilter::new(TypeFilter::Creature)),
            damage_source: None,
            excess: None,
        },
    )
    .cost(AbilityCost::Loyalty { amount: -2 })
    .sorcery_speed()
}

/// A non-loyalty targeted activated ability (no loyalty cost) dealing 2 damage
/// to target creature — produces `AbilityActivated { kind: Normal }`.
fn targeted_normal_ability() -> AbilityDefinition {
    AbilityDefinition::new(
        AbilityKind::Activated,
        Effect::DealDamage {
            amount: QuantityExpr::Fixed { value: 2 },
            target: TargetFilter::Typed(TypedFilter::new(TypeFilter::Creature)),
            damage_source: None,
            excess: None,
        },
    )
}

/// Imperatively place a planeswalker with the given subtype and loyalty under
/// `owner`, carrying `ability` at index 0. Returns its `ObjectId`.
fn place_planeswalker(
    runner: &mut GameRunner,
    owner: PlayerId,
    name: &str,
    subtype: &str,
    loyalty: u32,
    ability: AbilityDefinition,
) -> ObjectId {
    let state = runner.state_mut();
    let id = ObjectId(state.next_object_id);
    create_object(
        state,
        CardId(id.0),
        owner,
        name.to_string(),
        Zone::Battlefield,
    );
    let obj = state.objects.get_mut(&id).unwrap();
    obj.card_types.core_types.push(CoreType::Planeswalker);
    obj.card_types.subtypes.push(subtype.to_string());
    obj.base_card_types = obj.card_types.clone();
    // CR 306.5b: loyalty IS the loyalty-counter count; seed both in sync.
    obj.loyalty = Some(loyalty);
    obj.counters.insert(CounterType::Loyalty, loyalty);
    obj.abilities = Arc::new(vec![ability.clone()]);
    obj.base_abilities = Arc::new(vec![ability]);
    obj.entered_battlefield_turn = Some(state.turn_number.saturating_sub(1));
    obj.summoning_sick = false;
    id
}

/// Build a scenario at PreCombatMain with P0 active, two P0 creatures (so a
/// `target creature` loyalty ability surfaces a `TargetSelection` window), and a
/// trigger-source permanent parsed from `oracle` via the real parser.
fn scenario_with_trigger_source(
    source_name: &str,
    oracle: &str,
) -> (GameRunner, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let c1 = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let _c2 = scenario.add_creature(P0, "Hill Giant", 3, 3).id();
    let source = scenario
        .add_creature(P0, source_name, 2, 2)
        .from_oracle_text(oracle)
        .id();
    (scenario.build(), c1, source)
}

fn scenario_with_ajani() -> (GameRunner, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature(P0, "Ajani Ally", 2, 2);
    let ajani = scenario
        .add_creature(P0, "Ajani Unrelenting", 0, 0)
        .from_oracle_text(AJANI_ORACLE)
        .id();
    let mut runner = scenario.build();
    let object = runner.state_mut().objects.get_mut(&ajani).unwrap();
    object.card_types.core_types = vec![CoreType::Planeswalker];
    object.base_card_types = object.card_types.clone();
    object.loyalty = Some(5);
    object.counters.insert(CounterType::Loyalty, 5);
    (runner, ajani)
}

fn ajani_stack_entries(runner: &GameRunner, ajani: ObjectId) -> usize {
    runner
        .state()
        .stack
        .iter()
        .filter(|entry| entry.source_id == ajani)
        .count()
}

#[test]
fn ajani_unqualified_loyalty_trigger_uses_production_activation_scope() {
    let (mut positive, ajani) = scenario_with_ajani();
    positive
        .act(GameAction::ActivateAbility {
            source_id: ajani,
            ability_index: 0,
            payment_mode: engine::types::game_state::CastPaymentMode::Auto,
        })
        .expect("Ajani's parsed +1 loyalty ability must activate through apply");
    assert_eq!(
        ajani_stack_entries(&positive, ajani),
        2,
        "the loyalty ability and Ajani's unqualified trigger must both be stacked"
    );

    let (mut normal, ajani) = scenario_with_ajani();
    let normal_source = place_planeswalker(
        &mut normal,
        P0,
        "Normal Ability Source",
        "Jace",
        5,
        targeted_normal_ability(),
    );
    activate_and_target(&mut normal, normal_source, ajani);
    assert_eq!(
        ajani_stack_entries(&normal, ajani),
        0,
        "a Normal-kind activation must not fire Ajani's loyalty trigger"
    );

    let (mut opponent, ajani) = scenario_with_ajani();
    let opponent_walker = place_planeswalker(
        &mut opponent,
        PlayerId(1),
        "Opponent Walker",
        "Jace",
        5,
        targeted_minus_loyalty_ability(),
    );
    opponent.state_mut().active_player = PlayerId(1);
    opponent.state_mut().priority_player = PlayerId(1);
    opponent.state_mut().waiting_for = WaitingFor::Priority {
        player: PlayerId(1),
    };
    activate_and_target(&mut opponent, opponent_walker, ajani);
    assert_eq!(
        ajani_stack_entries(&opponent, ajani),
        0,
        "an opponent's loyalty activation must not satisfy Ajani's 'you' scope"
    );
}

/// CR 603.4 + CR 606.4: drive Way's verbatim Oracle trigger through a public
/// loyalty activation. The positive [−3] event must bind `Some(3)`, put Way's
/// draw above Jace's draw, and use that same event snapshot at resolution. The
/// paired [−1] activation proves a known sub-threshold value reaches the same
/// matcher without firing Way.
#[test]
fn way_of_the_mind_sculptor_uses_actual_loyalty_cost_and_resolves_trigger_first() {
    let mut positive_scenario = GameScenario::new();
    positive_scenario.at_phase(Phase::PreCombatMain);
    positive_scenario.with_library_top(P0, &["Card A", "Card B", "Card C", "Card D"]);
    positive_scenario.add_basic_land(P0, ManaColor::White);
    positive_scenario.add_basic_land(P0, ManaColor::White);
    let disenchant = positive_scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Disenchant",
            true,
            "Destroy target artifact or enchantment.",
        )
        .with_mana_cost(parse_mtgjson_mana_cost("{1}{W}"))
        .id();
    let way = positive_scenario
        .add_enchantment_to_hand_from_oracle(
            P0,
            "Way of the Mind Sculptor",
            WAY_OF_THE_MIND_SCULPTOR_ORACLE,
        )
        .as_legendary()
        .id();
    let mut positive = positive_scenario.build();
    // Cast the exact printed legendary enchantment through the public pipeline. Its real
    // Empower Jace ETB creates the token whose printed [−3] is activated below.
    positive.cast(way).resolve();
    let jace = *positive
        .state()
        .battlefield
        .iter()
        .find(|id| {
            let object = &positive.state().objects[id];
            object.is_token
                && object.controller == P0
                && object
                    .card_types
                    .subtypes
                    .iter()
                    .any(|subtype| subtype == "Jace")
        })
        .expect("Way's Empower Jace ETB creates a controlled Jace token");
    assert_eq!(positive.state().objects[&jace].loyalty, Some(5));
    let before = positive.state().players[0].library.len();
    let activated = positive
        .act(GameAction::ActivateAbility {
            source_id: jace,
            ability_index: 1,
            payment_mode: engine::types::game_state::CastPaymentMode::Auto,
        })
        .expect("Jace's [−3] activates through the public action");
    assert!(
        activated.events.iter().any(|event| matches!(
            event,
            GameEvent::AbilityActivated {
                source_id,
                loyalty_counters_removed: Some(3),
                ..
            } if *source_id == jace
        )),
        "activation event must carry its actual three-counter payment"
    );
    assert_eq!(
        positive.state().stack.len(),
        2,
        "Way trigger sits above Jace ability"
    );
    let way_trigger = positive.state().stack.back().unwrap();
    assert!(
        matches!(way_trigger.kind, StackEntryKind::TriggeredAbility { source_id, .. } if source_id == way)
    );
    let way_trigger_id = way_trigger.id;
    let jace_activation_id = positive.state().stack.front().unwrap().id;

    // CR 113.7a: destroy the source while its triggered ability is on the stack.
    positive.cast(disenchant).target_object(way).commit();
    assert_eq!(positive.state().stack.back().unwrap().source_id, disenchant);
    positive.resolve_top();
    assert_eq!(positive.state().objects[&way].zone, Zone::Graveyard);
    assert_eq!(positive.state().stack.len(), 2);
    assert_eq!(positive.state().stack.back().unwrap().id, way_trigger_id);
    assert_eq!(positive.state().players[0].library.len(), before);

    // CR 603.4: the resolution-time recheck uses the activation event snapshot,
    // not the planeswalker's changed live loyalty total.
    positive.state_mut().objects.get_mut(&jace).unwrap().loyalty = Some(0);
    positive
        .state_mut()
        .objects
        .get_mut(&jace)
        .unwrap()
        .counters
        .insert(CounterType::Loyalty, 0);
    let hand_before = positive.state().players[0].hand.len();
    positive.resolve_top();
    assert_eq!(before - positive.state().players[0].library.len(), 1);
    assert_eq!(positive.state().players[0].hand.len(), hand_before + 1);
    assert_eq!(
        positive.state().stack.len(),
        1,
        "Jace ability remains below Way's trigger"
    );
    let remaining = positive.state().stack.front().unwrap();
    assert_eq!(remaining.id, jace_activation_id);
    assert!(
        matches!(remaining.kind, StackEntryKind::ActivatedAbility { source_id, .. } if source_id == jace)
    );
    positive.resolve_top();
    assert_eq!(before - positive.state().players[0].library.len(), 2);

    let mut negative_scenario = GameScenario::new();
    negative_scenario.at_phase(Phase::PreCombatMain);
    negative_scenario.with_library_top(P0, &["Card A", "Card B"]);
    let way = negative_scenario
        .add_enchantment_to_hand_from_oracle(
            P0,
            "Way of the Mind Sculptor",
            WAY_OF_THE_MIND_SCULPTOR_ORACLE,
        )
        .as_legendary()
        .id();
    let mut negative = negative_scenario.build();
    negative.cast(way).resolve();
    let jace = *negative
        .state()
        .last_created_token_ids
        .first()
        .expect("Way's printed Empower Jace ability created its Jace token");
    assert!(
        negative.state().objects[&jace].is_token,
        "reach: use the real Jace token"
    );
    let activated = negative
        .act(GameAction::ActivateAbility {
            source_id: jace,
            ability_index: 0,
            payment_mode: engine::types::game_state::CastPaymentMode::Auto,
        })
        .expect("the Jace token's printed [−1] activates through the public action");
    assert!(
        activated.events.iter().any(|event| matches!(
            event,
            GameEvent::AbilityActivated {
                source_id,
                loyalty_counters_removed: Some(1),
                ..
            } if *source_id == jace
        )),
        "negative reach guard: activation carries known Some(1)"
    );
    assert_eq!(
        negative.state().stack.len(),
        1,
        "known Some(1) does not fire Way"
    );
    let library_before_surveil = negative.state().players[0].library.len();
    negative.resolve_top();
    let kept = match &negative.state().waiting_for {
        WaitingFor::SurveilChoice { player: P0, cards } if cards.len() == 1 => cards[0],
        waiting => panic!("expected Jace's Surveil 1 choice, got {waiting:?}"),
    };
    negative
        .act(GameAction::SelectCards { cards: vec![kept] })
        .expect("keep the card seen by the Jace token's printed Surveil 1");
    assert_eq!(
        negative.state().players[0].library.len(),
        library_before_surveil
    );
    assert_eq!(negative.state().players[0].library[0], kept);
    assert_eq!(negative.state().players[0].hand.len(), 0);
    assert_eq!(negative.state().objects[&jace].loyalty, Some(4));
    assert!(matches!(
        negative.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    assert_eq!(negative.state().stack.len(), 0);

    // The variable loyalty-cost path uses the generic activation payment
    // continuation; real Chandra [−X] activations pin both threshold sides.
    for (x, expected_stack) in [(1, 1), (2, 2)] {
        let mut x_scenario = GameScenario::new();
        x_scenario.at_phase(Phase::PreCombatMain);
        x_scenario.with_library_top(P0, &["Card A", "Card B"]);
        x_scenario
            .add_enchantment_from_oracle(
                P0,
                "Way of the Mind Sculptor",
                WAY_OF_THE_MIND_SCULPTOR_ORACLE,
            )
            .as_legendary();
        let target = x_scenario.add_creature(P0, "Target", 2, 2).id();
        let chandra = x_scenario
            .add_planeswalker_from_oracle(
                P0,
                "Chandra, Awakened Inferno",
                "Chandra",
                6,
                CHANDRA_AWAKENED_INFERNO_ORACLE,
            )
            .id();
        let mut x_runner = x_scenario.build();
        x_runner
            .act(GameAction::ActivateAbility {
                source_id: chandra,
                ability_index: 2,
                payment_mode: engine::types::game_state::CastPaymentMode::Auto,
            })
            .expect("Chandra's [−X] announcement opens its X choice");
        x_runner
            .act(GameAction::ChooseX { value: x })
            .expect("announce X value");
        let x_committed = x_runner
            .act(GameAction::ChooseTarget {
                target: Some(TargetRef::Object(target)),
            })
            .expect("complete Chandra's target choice");
        assert!(
            x_committed.events.iter().any(|event| matches!(
                event,
                GameEvent::AbilityActivated {
                    source_id,
                    loyalty_counters_removed: Some(removed),
                    ..
                } if *source_id == chandra && *removed == x
            )),
            "generic X payment binds the actual {x}-counter event"
        );
        assert_eq!(
            x_runner.state().stack.len(),
            expected_stack,
            "Way's threshold result for X={x}"
        );
    }

    // Chandra's printed natural +2 action removes a known zero. It reaches the same
    // loyalty-event matcher but does not satisfy Way's threshold.
    let mut plus_scenario = GameScenario::new();
    plus_scenario.at_phase(Phase::PreCombatMain);
    plus_scenario
        .add_enchantment_from_oracle(
            P0,
            "Way of the Mind Sculptor",
            WAY_OF_THE_MIND_SCULPTOR_ORACLE,
        )
        .as_legendary();
    let plus_chandra = plus_scenario
        .add_planeswalker_from_oracle(
            P0,
            "Chandra, Awakened Inferno",
            "Chandra",
            6,
            CHANDRA_AWAKENED_INFERNO_ORACLE,
        )
        .id();
    let mut plus_runner = plus_scenario.build();
    let plus_result = plus_runner
        .act(GameAction::ActivateAbility {
            source_id: plus_chandra,
            ability_index: 0,
            payment_mode: engine::types::game_state::CastPaymentMode::Auto,
        })
        .expect("printed natural +2 activation completes through apply");
    assert!(
        plus_result.events.iter().any(|event| matches!(
            event,
            GameEvent::AbilityActivated {
                source_id,
                loyalty_counters_removed: Some(0),
                ..
            } if *source_id == plus_chandra
        )),
        "reach guard: natural +2 reports known Some(0)"
    );
    assert_eq!(
        plus_runner.state().stack.len(),
        1,
        "known zero does not fire Way"
    );

    // AtomicCards contains no printed loyalty ability whose activation cost
    // also removes another counter type. Use the nearest printed loyalty
    // pattern (Jace [−3]) with a small synthetic composite cost to prove that
    // only the loyalty leg contributes to Way's threshold.
    let mut mixed_scenario = GameScenario::new();
    mixed_scenario.at_phase(Phase::PreCombatMain);
    let mixed_way = mixed_scenario
        .add_enchantment_to_hand_from_oracle(
            P0,
            "Way of the Mind Sculptor",
            WAY_OF_THE_MIND_SCULPTOR_ORACLE,
        )
        .as_legendary()
        .id();
    let mut mixed = mixed_scenario.build();
    mixed.cast(mixed_way).resolve();
    let mixed_jace = *mixed
        .state()
        .last_created_token_ids
        .first()
        .expect("Way creates its printed Jace token");
    let mixed_cost = AbilityCost::Composite {
        costs: vec![
            AbilityCost::Loyalty { amount: -2 },
            AbilityCost::RemoveCounter {
                count: 1,
                counter_type: CounterMatch::OfType(CounterType::Generic("charge".into())),
                target: None,
                selection: CounterCostSelection::SingleObject,
            },
        ],
    };
    let mixed_ability = AbilityDefinition::new(
        AbilityKind::Activated,
        Effect::Draw {
            count: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::Controller,
        },
    )
    .cost(mixed_cost);
    {
        let object = mixed.state_mut().objects.get_mut(&mixed_jace).unwrap();
        object
            .counters
            .insert(CounterType::Generic("charge".into()), 1);
        object.abilities = std::sync::Arc::new(vec![mixed_ability.clone()]);
        object.base_abilities = std::sync::Arc::new(vec![mixed_ability]);
    }
    let mixed_result = mixed
        .act(GameAction::ActivateAbility {
            source_id: mixed_jace,
            ability_index: 0,
            payment_mode: engine::types::game_state::CastPaymentMode::Auto,
        })
        .expect("mixed loyalty and charge cost activates through apply");
    assert!(
        mixed_result.events.iter().any(|event| matches!(
            event,
            GameEvent::AbilityActivated {
                source_id,
                loyalty_counters_removed: Some(2),
                ..
            } if *source_id == mixed_jace
        )),
        "only the two loyalty counters satisfy the event receipt"
    );
    assert_eq!(
        mixed.state().objects[&mixed_jace]
            .counters
            .get(&CounterType::Generic("charge".into())),
        None
    );
    assert_eq!(
        mixed.state().stack.len(),
        2,
        "two loyalty counters still fire Way"
    );

    let mut normal_scenario = GameScenario::new();
    normal_scenario.at_phase(Phase::PreCombatMain);
    normal_scenario
        .add_enchantment_from_oracle(
            P0,
            "Way of the Mind Sculptor",
            WAY_OF_THE_MIND_SCULPTOR_ORACLE,
        )
        .as_legendary();
    let pyromancer = normal_scenario
        .add_creature_from_oracle(
            P0,
            "Prodigal Pyromancer",
            1,
            1,
            "{T}: This creature deals 1 damage to any target.",
        )
        .id();
    let mut normal = normal_scenario.build();
    normal
        .act(GameAction::ActivateAbility {
            source_id: pyromancer,
            ability_index: 0,
            payment_mode: engine::types::game_state::CastPaymentMode::Auto,
        })
        .expect("announce Prodigal Pyromancer's tap ability");
    let targeted = normal
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Player(PlayerId(1))),
        })
        .expect("target the opponent with the Pyromancer's ability");
    assert!(targeted.events.iter().any(|event| matches!(event,
        GameEvent::AbilityActivated { source_id, kind: ActivatedAbilityKind::Normal, .. } if *source_id == pyromancer
    )));
    assert_eq!(
        normal.state().stack.len(),
        1,
        "a Normal activation does not fire Way"
    );
    assert!(matches!(normal.state().stack.front().unwrap().kind,
        StackEntryKind::ActivatedAbility { source_id, .. } if source_id == pyromancer
    ));

    let mut opponent_scenario = GameScenario::new();
    opponent_scenario.at_phase(Phase::PreCombatMain);
    opponent_scenario
        .add_enchantment_from_oracle(
            P0,
            "Way of the Mind Sculptor",
            WAY_OF_THE_MIND_SCULPTOR_ORACLE,
        )
        .as_legendary();
    let source = opponent_scenario
        .add_planeswalker_from_oracle(
            PlayerId(1),
            "Chandra, Awakened Inferno",
            "Chandra",
            6,
            CHANDRA_AWAKENED_INFERNO_ORACLE,
        )
        .id();
    let mut opponent = opponent_scenario.build();
    opponent.state_mut().active_player = PlayerId(1);
    opponent.state_mut().priority_player = PlayerId(1);
    opponent.state_mut().waiting_for = WaitingFor::Priority {
        player: PlayerId(1),
    };
    let activated = opponent
        .act(GameAction::ActivateAbility {
            source_id: source,
            ability_index: 1,
            payment_mode: engine::types::game_state::CastPaymentMode::Auto,
        })
        .expect("opponent's loyalty ability reaches its normal event path");
    assert!(
        activated.events.iter().any(|event| matches!(
            event,
            GameEvent::AbilityActivated {
                player_id: PlayerId(1),
                loyalty_counters_removed: Some(3),
                ..
            }
        )),
        "reach guard: opponent's event has a known qualifying count"
    );
    assert_eq!(
        opponent.state().stack.len(),
        1,
        "opponent's 'you' does not satisfy Way"
    );
}

/// CR 602.2 + 603.4: counter the real Jace token's printed `[−3]` ability
/// after Way's trigger has fired, then verify Way's trigger still resolves.
#[test]
fn way_trigger_remains_after_the_activated_loyalty_ability_is_countered() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Card A", "Card B"]);
    scenario.with_mana_pool(
        P0,
        vec![engine::types::mana::ManaUnit::new(
            engine::types::mana::ManaType::Blue,
            ObjectId(0),
            false,
            vec![],
        )],
    );
    let way = scenario
        .add_enchantment_to_hand_from_oracle(
            P0,
            "Way of the Mind Sculptor",
            WAY_OF_THE_MIND_SCULPTOR_ORACLE,
        )
        .as_legendary()
        .id();
    let stifle = scenario
        .add_spell_to_hand_from_oracle(P0, "Stifle", true, STIFLE_ORACLE)
        .id();
    let mut runner = scenario.build();
    runner.cast(way).resolve();
    let jace = *runner
        .state()
        .last_created_token_ids
        .first()
        .expect("Way's printed Empower Jace ability created its Jace token");
    assert!(
        runner.state().objects[&jace].is_token,
        "reach: use the real Jace token"
    );
    runner
        .act(GameAction::ActivateAbility {
            source_id: jace,
            ability_index: 1,
            payment_mode: engine::types::game_state::CastPaymentMode::Auto,
        })
        .expect("the qualifying loyalty activation commits");
    let loyalty_entry = runner
        .state()
        .stack
        .iter()
        .find(|entry| entry.source_id == jace)
        .expect("the activated loyalty ability remains on the stack")
        .id;
    assert_eq!(runner.state().stack.len(), 2, "reach: Way's trigger fired");
    let way_trigger = runner.state().stack.back().unwrap();
    assert_eq!(way_trigger.source_id, way);
    assert!(matches!(
        way_trigger.kind,
        StackEntryKind::TriggeredAbility { .. }
    ));
    let way_trigger_id = way_trigger.id;
    runner.cast(stifle).target_object(loyalty_entry).commit();
    assert_eq!(runner.state().stack.len(), 3);
    assert_eq!(runner.state().stack.back().unwrap().source_id, stifle);
    runner.resolve_top();
    assert_eq!(
        runner.state().stack.len(),
        1,
        "Stifle removes only Jace's ability"
    );
    assert!(runner
        .state()
        .stack
        .iter()
        .all(|entry| entry.id != loyalty_entry));
    assert_eq!(runner.state().stack.back().unwrap().source_id, way);
    assert_eq!(runner.state().stack.back().unwrap().id, way_trigger_id);
    assert!(runner.state().players[0].hand.is_empty());
    runner.resolve_top();
    assert!(
        runner.state().stack.is_empty(),
        "Way's trigger still resolves"
    );
    assert!(
        !runner
            .state()
            .stack
            .iter()
            .any(|entry| entry.id == loyalty_entry),
        "the activated Jace ability was countered"
    );
    let hand_cards = runner
        .state()
        .objects
        .values()
        .filter(|object| object.owner == P0 && object.zone == Zone::Hand)
        .count();
    assert_eq!(
        hand_cards, 1,
        "Way's trigger draws after Stifle counters Jace's ability"
    );
}

fn activate_and_target(runner: &mut GameRunner, pw: ObjectId, target: ObjectId) {
    runner
        .act(GameAction::ActivateAbility {
            source_id: pw,
            ability_index: 0,
            payment_mode: engine::types::game_state::CastPaymentMode::Auto,
        })
        .expect("activation announced");

    // CR 602.2b / CR 601.2c: answer the announcement-through-target windows in
    // order — `ChooseXValue` (only for the `[−X]` loyalty cost), then the
    // `target creature` slot — stopping once the ability commits to the stack.
    for _ in 0..16 {
        match runner.state().waiting_for {
            WaitingFor::ChooseXValue { .. } => {
                runner
                    .act(GameAction::ChooseX { value: 2 })
                    .expect("announce loyalty [-X]");
            }
            WaitingFor::TargetSelection { .. } => {
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(TargetRef::Object(target)),
                    })
                    .expect("choose ability target");
            }
            _ => break,
        }
    }
}

/// GAP-1 PROOF: a TARGETED loyalty ability of a Chandra planeswalker, activated
/// through the `TargetSelection` finalize seam, fires Keral Keep Disciples'
/// `LoyaltyAbilityActivated` trigger. Asserted by inspecting the stack right
/// after the activation commits (the trigger's source is on the stack above the
/// loyalty ability). If the kind at the targeted finalize seam were hardcoded
/// `Normal`, the matcher would reject the event and the trigger source would NOT
/// appear on the stack — the revert assertion this test guards.
#[test]
fn gap1_targeted_loyalty_fires_keral_keep_trigger() {
    let (mut runner, c1, _keral) =
        scenario_with_trigger_source("Keral Keep Disciples", KERAL_ORACLE);
    let chandra = place_planeswalker(
        &mut runner,
        P0,
        "Chandra, Acolyte of Flame",
        "Chandra",
        5,
        targeted_minus_loyalty_ability(),
    );

    activate_and_target(&mut runner, chandra, c1);

    let names = runner.stack_names();
    assert!(
        names.iter().any(|n| n == "Keral Keep Disciples"),
        "expected Keral Keep Disciples' loyalty trigger on the stack, got {names:?}"
    );
}

/// A NON-Chandra planeswalker's loyalty activation does NOT fire the
/// "a Chandra planeswalker" trigger.
#[test]
fn non_chandra_loyalty_does_not_fire() {
    let (mut runner, c1, _keral) =
        scenario_with_trigger_source("Keral Keep Disciples", KERAL_ORACLE);
    let jace = place_planeswalker(
        &mut runner,
        P0,
        "Jace, the Mind Sculptor",
        "Jace",
        5,
        targeted_minus_loyalty_ability(),
    );

    activate_and_target(&mut runner, jace, c1);

    let names = runner.stack_names();
    assert!(
        !names.iter().any(|n| n == "Keral Keep Disciples"),
        "non-Chandra loyalty activation must NOT fire the trigger, got {names:?}"
    );
}

/// A NON-loyalty activated ability (kind == Normal) does NOT fire the loyalty
/// trigger, even on a Chandra planeswalker.
#[test]
fn non_loyalty_ability_does_not_fire() {
    let (mut runner, c1, _keral) =
        scenario_with_trigger_source("Keral Keep Disciples", KERAL_ORACLE);
    let chandra = place_planeswalker(
        &mut runner,
        P0,
        "Chandra, Acolyte of Flame",
        "Chandra",
        5,
        targeted_normal_ability(),
    );

    activate_and_target(&mut runner, chandra, c1);

    let names = runner.stack_names();
    assert!(
        !names.iter().any(|n| n == "Keral Keep Disciples"),
        "a Normal-kind activated ability must NOT fire the loyalty trigger, got {names:?}"
    );
}

/// Enchanted-host (Elspeth's / Rowan's Talent class): the loyalty ability of the
/// ENCHANTED planeswalker fires; a different planeswalker does not. The Talent's
/// `valid_card == AttachedTo` resolves against the aura's host.
#[test]
fn enchanted_host_loyalty_fires_other_does_not() {
    let (mut runner, c1, talent) =
        scenario_with_trigger_source("Elspeth's Talent", ELSPETH_TALENT_ORACLE);

    let host = place_planeswalker(
        &mut runner,
        P0,
        "Host Walker",
        "Elspeth",
        5,
        targeted_minus_loyalty_ability(),
    );
    let other = place_planeswalker(
        &mut runner,
        P0,
        "Other Walker",
        "Jace",
        5,
        targeted_minus_loyalty_ability(),
    );

    // Make the Talent an Aura attached to the host planeswalker.
    {
        let state = runner.state_mut();
        let obj = state.objects.get_mut(&talent).unwrap();
        obj.card_types.subtypes.push("Aura".to_string());
        obj.base_card_types = obj.card_types.clone();
        obj.attached_to = Some(AttachTarget::Object(host));
    }

    // Activating the host's loyalty ability fires the Talent trigger.
    activate_and_target(&mut runner, host, c1);
    assert!(
        runner.stack_names().iter().any(|n| n == "Elspeth's Talent"),
        "enchanted-host loyalty activation should fire the Talent trigger, got {:?}",
        runner.stack_names()
    );

    // Drain the stack so the next activation starts from a clean Priority window.
    for _ in 0..32 {
        if runner.state().stack.is_empty()
            && matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
        {
            break;
        }
        if runner.act(GameAction::PassPriority).is_err() {
            break;
        }
    }

    // Activating the OTHER (non-host) planeswalker's loyalty ability does not.
    activate_and_target(&mut runner, other, c1);
    let after = runner.stack_names();
    assert!(
        !after.iter().any(|n| n == "Elspeth's Talent"),
        "non-host loyalty activation must NOT fire the Talent trigger, got {after:?}"
    );
}
