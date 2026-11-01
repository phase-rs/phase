//! "Reveal X <color> cards from your hand" costs reveal exactly the announced
//! number of matching cards, and the effect reads that same X.
//!
//! CR 107.3a: X in a cost the text does not define is announced by the
//! controller while casting or activating, and every X takes that value.
//! CR 602.2b applies the casting steps to an activated ability: X is announced
//! (CR 601.2b) before targets are chosen (CR 601.2c) and before the total cost
//! is paid (CR 601.2h). CR 701.20a: revealing shows the cards to every player;
//! CR 701.20b: a revealed card stays in its zone.
//!
//! The class covers activation costs (the whole Martyr cycle — Sands, Spores,
//! Bones, Ashes and Frost — is driven here from its verbatim Oracle text) and
//! the same cost printed as a spell's additional cost. No printed spell carries
//! that additional cost, so the spell rows use a synthetic card ("Ember
//! Tithe"). CR 601.2a: the spell being cast is on the stack, so it is never a
//! card in hand that could pay its own reveal.

use std::collections::HashSet;

use engine::ai_support::legal_actions;
use engine::game::casting::can_activate_ability_now;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{AbilityCost, TargetRef};
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::{CastPaymentMode, PayCostKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaCost, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const MARTYR_OF_SANDS: &str = "{1}, Reveal X white cards from your hand, Sacrifice this creature: You gain three times X life.";
const MARTYR_OF_SPORES: &str = "{1}, Reveal X green cards from your hand, Sacrifice this creature: Target creature gets +X/+X until end of turn.";
const MARTYR_OF_BONES: &str = "{1}, Reveal X black cards from your hand, Sacrifice this creature: Exile up to X target cards from a single graveyard.";
const MARTYR_OF_ASHES: &str = "{2}, Reveal X red cards from your hand, Sacrifice this creature: This creature deals X damage to each creature without flying.";
const MARTYR_OF_FROST: &str = "{2}, Reveal X blue cards from your hand, Sacrifice this creature: Counter target spell unless its controller pays {X}.";
const QUICK_TONIC: &str = "You gain 1 life.";
const EMBER_TITHE: &str = "As an additional cost to cast this spell, reveal X red cards from your hand.\nYou gain X life.";

fn pool(kinds: &[ManaType]) -> Vec<ManaUnit> {
    kinds
        .iter()
        .map(|&kind| ManaUnit::new(kind, ObjectId(0), false, vec![]))
        .collect()
}

fn colored_hand_card(
    scenario: &mut GameScenario,
    player: PlayerId,
    name: &str,
    color: ManaColor,
) -> ObjectId {
    scenario
        .add_creature_to_hand(player, name, 1, 1)
        .with_color(vec![color])
        .id()
}

fn activate(runner: &mut GameRunner, source: ObjectId) -> Vec<GameEvent> {
    runner
        .act(GameAction::ActivateAbility {
            source_id: source,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("the activation must be accepted")
        .events
}

/// Returns the announced-X bounds the engine offers, panicking unless the
/// current prompt is the X announcement.
fn offered_x(runner: &GameRunner) -> (u32, u32) {
    let WaitingFor::ChooseXValue {
        player, min, max, ..
    } = &runner.state().waiting_for
    else {
        panic!(
            "the cost must announce X, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(*player, P0, "the activator / caster announces X");
    (*min, *max)
}

/// Answers any mana-payment prompt (auto payment) and returns the events.
fn pay_mana(runner: &mut GameRunner, events: &mut Vec<GameEvent>) {
    for _ in 0..4 {
        if !matches!(runner.state().waiting_for, WaitingFor::ManaPayment { .. }) {
            return;
        }
        events.extend(
            runner
                .act(GameAction::PassPriority)
                .expect("auto-paying the mana must succeed")
                .events,
        );
    }
}

/// Asserts the current prompt is a reveal of exactly `count` cards and
/// returns its eligible choices.
fn reveal_prompt(runner: &GameRunner, count: usize) -> HashSet<ObjectId> {
    let WaitingFor::PayCost {
        player,
        kind: PayCostKind::Reveal { .. },
        choices,
        count: prompt_count,
        ..
    } = &runner.state().waiting_for
    else {
        panic!(
            "the reveal cost must prompt, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(*player, P0, "the payer reveals from their own hand");
    assert_eq!(*prompt_count, count, "exactly X cards must be revealed");
    choices.iter().copied().collect()
}

fn assert_still_revealing(runner: &GameRunner) {
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::PayCost {
                kind: PayCostKind::Reveal { .. },
                ..
            }
        ),
        "a rejected reveal must leave the reveal prompt open, got {:?}",
        runner.state().waiting_for
    );
}

fn revealed_cards(events: &[GameEvent]) -> Vec<Vec<ObjectId>> {
    events
        .iter()
        .filter_map(|event| match event {
            GameEvent::CardsRevealed { card_ids, .. } => Some(card_ids.clone()),
            _ => None,
        })
        .collect()
}

fn resolve(runner: &mut GameRunner) {
    runner.advance_until_stack_empty();
    assert!(runner.state().stack.is_empty(), "the stack must resolve");
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "resolution must return to priority, got {:?}",
        runner.state().waiting_for
    );
}

/// R3 + R4 + R5a: Martyr of Sands announces X up to the white cards in its
/// activator's hand, reveals exactly X of them, and gains three times X life.
#[test]
fn martyr_of_sands_announces_x_bounded_by_white_cards_in_your_hand() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let sands = scenario
        .add_creature_from_oracle(P0, "Martyr of Sands", 1, 1, MARTYR_OF_SANDS)
        .id();
    let w1 = colored_hand_card(&mut scenario, P0, "White One", ManaColor::White);
    let w2 = colored_hand_card(&mut scenario, P0, "White Two", ManaColor::White);
    let w3 = colored_hand_card(&mut scenario, P0, "White Three", ManaColor::White);
    let r1 = colored_hand_card(&mut scenario, P0, "Red One", ManaColor::Red);
    colored_hand_card(&mut scenario, P1, "Opponent White One", ManaColor::White);
    colored_hand_card(&mut scenario, P1, "Opponent White Two", ManaColor::White);
    scenario.with_mana_pool(P0, pool(&[ManaType::Colorless]));
    let mut runner = scenario.build();
    let life_before = runner.life(P0);

    let mut events = activate(&mut runner, sands);

    // CR 107.3a + CR 118.3: X is capped at the white cards the activator can
    // reveal — not the off-color card, not the opponent's white cards.
    assert_eq!(offered_x(&runner), (0, 3));
    assert!(
        runner.act(GameAction::ChooseX { value: 4 }).is_err(),
        "X above the white cards in hand must be rejected"
    );
    assert_eq!(offered_x(&runner), (0, 3));

    events.extend(
        runner
            .act(GameAction::ChooseX { value: 2 })
            .expect("announce X = 2")
            .events,
    );
    pay_mana(&mut runner, &mut events);

    let choices = reveal_prompt(&runner, 2);
    assert_eq!(choices, HashSet::from([w1, w2, w3]));

    assert!(
        runner
            .act(GameAction::SelectCards { cards: vec![w1] })
            .is_err(),
        "revealing fewer than X cards must be rejected"
    );
    assert_still_revealing(&runner);
    assert!(
        runner
            .act(GameAction::SelectCards {
                cards: vec![w1, r1]
            })
            .is_err(),
        "revealing an off-color card must be rejected"
    );
    assert_still_revealing(&runner);

    events.extend(
        runner
            .act(GameAction::SelectCards {
                cards: vec![w1, w2],
            })
            .expect("revealing two white cards pays the cost")
            .events,
    );

    // CR 701.20b: revealed cards stay in hand.
    assert_eq!(runner.state().objects[&w1].zone, Zone::Hand);
    assert_eq!(runner.state().objects[&w2].zone, Zone::Hand);
    assert!(
        revealed_cards(&events).contains(&vec![w1, w2]),
        "the two chosen white cards are revealed, got {:?}",
        revealed_cards(&events)
    );
    assert_eq!(
        runner.state().objects[&sands].zone,
        Zone::Graveyard,
        "the self-sacrifice cost is paid"
    );
    assert_eq!(runner.state().stack.len(), 1);

    resolve(&mut runner);
    assert_eq!(
        runner.life(P0) - life_before,
        6,
        "the effect's X equals the two cards revealed (three times X)"
    );
}

/// R6: X = 0 is a legal announcement and reveals nothing (CR 107.3a +
/// CR 601.2h).
#[test]
fn martyr_of_sands_x_zero_reveals_nothing() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let sands = scenario
        .add_creature_from_oracle(P0, "Martyr of Sands", 1, 1, MARTYR_OF_SANDS)
        .id();
    let w1 = colored_hand_card(&mut scenario, P0, "White One", ManaColor::White);
    scenario.with_mana_pool(P0, pool(&[ManaType::Colorless]));
    let mut runner = scenario.build();
    let life_before = runner.life(P0);

    let mut events = activate(&mut runner, sands);
    assert_eq!(offered_x(&runner), (0, 1));
    events.extend(
        runner
            .act(GameAction::ChooseX { value: 0 })
            .expect("announce X = 0")
            .events,
    );
    pay_mana(&mut runner, &mut events);

    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "a zero reveal surfaces no prompt, got {:?}",
        runner.state().waiting_for
    );
    assert_eq!(runner.state().stack.len(), 1);
    assert_eq!(
        runner.state().objects[&sands].zone,
        Zone::Graveyard,
        "the sacrifice leg after the reveal leg was paid"
    );
    assert_eq!(runner.state().objects[&w1].zone, Zone::Hand);
    assert!(
        !revealed_cards(&events).iter().any(|ids| ids.contains(&w1)),
        "no card is revealed for X = 0"
    );

    resolve(&mut runner);
    assert_eq!(runner.life(P0), life_before);
}

/// R5b: Martyr of Spores chooses its target after X and pumps it by X.
#[test]
fn martyr_of_spores_target_gets_plus_x_plus_x() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spores = scenario
        .add_creature_from_oracle(P0, "Martyr of Spores", 1, 1, MARTYR_OF_SPORES)
        .id();
    let g1 = colored_hand_card(&mut scenario, P0, "Green One", ManaColor::Green);
    let g2 = colored_hand_card(&mut scenario, P0, "Green Two", ManaColor::Green);
    let bear = scenario.add_creature(P1, "Target Bear", 2, 2).id();
    scenario.with_mana_pool(P0, pool(&[ManaType::Colorless]));
    let mut runner = scenario.build();

    let mut events = activate(&mut runner, spores);
    assert_eq!(offered_x(&runner), (0, 2));
    events.extend(
        runner
            .act(GameAction::ChooseX { value: 2 })
            .expect("announce X = 2")
            .events,
    );

    // CR 601.2c: the target is chosen after X is announced.
    let WaitingFor::TargetSelection { target_slots, .. } = &runner.state().waiting_for else {
        panic!(
            "the activation must choose its target after X, got {:?}",
            runner.state().waiting_for
        );
    };
    assert!(target_slots[0]
        .legal_targets
        .contains(&TargetRef::Object(bear)));
    events.extend(
        runner
            .act(GameAction::ChooseTarget {
                target: Some(TargetRef::Object(bear)),
            })
            .expect("target the bear")
            .events,
    );
    pay_mana(&mut runner, &mut events);

    let choices = reveal_prompt(&runner, 2);
    assert_eq!(choices, HashSet::from([g1, g2]));
    events.extend(
        runner
            .act(GameAction::SelectCards {
                cards: vec![g1, g2],
            })
            .expect("reveal both green cards")
            .events,
    );
    assert!(revealed_cards(&events).contains(&vec![g1, g2]));

    resolve(&mut runner);
    let bear_obj = &runner.state().objects[&bear];
    assert_eq!(
        (bear_obj.power, bear_obj.toughness),
        (Some(4), Some(4)),
        "the target gets +X/+X with X = 2"
    );
}

/// R5c: Martyr of Bones' target count depends on X, so the activation takes
/// the announce-X-before-targets route. X bounds the number of targets, the
/// targets come from a single graveyard, and they are exiled.
#[test]
fn martyr_of_bones_exiles_up_to_x_cards_from_a_single_graveyard() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let bones = scenario
        .add_creature_from_oracle(P0, "Martyr of Bones", 1, 1, MARTYR_OF_BONES)
        .id();
    let b1 = colored_hand_card(&mut scenario, P0, "Black One", ManaColor::Black);
    let b2 = colored_hand_card(&mut scenario, P0, "Black Two", ManaColor::Black);
    let n1 = colored_hand_card(&mut scenario, P0, "White One", ManaColor::White);
    let g0 = scenario.add_spell_to_graveyard(P0, "Own Grave", true).id();
    let g1a = scenario.add_spell_to_graveyard(P1, "Grave A", true).id();
    let g1b = scenario.add_spell_to_graveyard(P1, "Grave B", true).id();
    let g1c = scenario.add_spell_to_graveyard(P1, "Grave C", true).id();
    scenario.with_mana_pool(P0, pool(&[ManaType::Colorless]));
    let mut runner = scenario.build();

    // The activation is offered even though its target count needs X.
    assert!(can_activate_ability_now(runner.state(), P0, bones, 0));
    assert!(
        legal_actions(runner.state()).iter().any(|action| matches!(
            action,
            GameAction::ActivateAbility {
                source_id,
                ability_index: 0,
                ..
            } if *source_id == bones
        )),
        "the AI legal-action list offers the activation"
    );

    let mut events = activate(&mut runner, bones);
    assert_eq!(offered_x(&runner), (0, 2));
    assert!(
        runner.act(GameAction::ChooseX { value: 3 }).is_err(),
        "the white card cannot pay a black reveal"
    );
    events.extend(
        runner
            .act(GameAction::ChooseX { value: 2 })
            .expect("announce X = 2")
            .events,
    );

    // CR 601.2c: "up to X" builds X optional target slots.
    let WaitingFor::TargetSelection { target_slots, .. } = &runner.state().waiting_for else {
        panic!(
            "the activation must choose targets after X, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(target_slots.len(), 2, "X = 2 builds two target slots");
    assert!(target_slots.iter().all(|slot| slot.optional));

    events.extend(
        runner
            .act(GameAction::ChooseTarget {
                target: Some(TargetRef::Object(g1a)),
            })
            .expect("target a card in the opponent's graveyard")
            .events,
    );
    assert!(
        runner
            .act(GameAction::ChooseTarget {
                target: Some(TargetRef::Object(g0)),
            })
            .is_err(),
        "a second target from a different graveyard must be rejected"
    );
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::TargetSelection { .. }
        ),
        "the rejected target leaves target selection open"
    );
    events.extend(
        runner
            .act(GameAction::ChooseTarget {
                target: Some(TargetRef::Object(g1b)),
            })
            .expect("target a second card in the same graveyard")
            .events,
    );
    assert!(
        !matches!(
            runner.state().waiting_for,
            WaitingFor::TargetSelection { .. }
        ),
        "target selection ends after X targets"
    );
    assert!(
        runner
            .act(GameAction::ChooseTarget {
                target: Some(TargetRef::Object(g1c)),
            })
            .is_err(),
        "no target beyond X can be added"
    );
    pay_mana(&mut runner, &mut events);

    let choices = reveal_prompt(&runner, 2);
    assert_eq!(choices, HashSet::from([b1, b2]));
    events.extend(
        runner
            .act(GameAction::SelectCards {
                cards: vec![b1, b2],
            })
            .expect("reveal both black cards")
            .events,
    );
    assert!(revealed_cards(&events).contains(&vec![b1, b2]));

    resolve(&mut runner);
    let zone = |id: ObjectId| runner.state().objects[&id].zone;
    assert_eq!(zone(g1a), Zone::Exile);
    assert_eq!(zone(g1b), Zone::Exile);
    assert_eq!(zone(g1c), Zone::Graveyard);
    assert_eq!(zone(g0), Zone::Graveyard);
    assert_eq!(zone(b1), Zone::Hand);
    assert_eq!(zone(b2), Zone::Hand);
    assert_eq!(zone(n1), Zone::Hand);
}

/// Martyr of Ashes announces X up to the red cards in its activator's hand,
/// reveals exactly X of them, and deals X damage to each creature without
/// flying (CR 120.3e marks the damage; CR 704.5g destroys a creature whose
/// marked damage reaches its toughness). A flier is not dealt damage.
#[test]
fn martyr_of_ashes_deals_x_damage_to_each_creature_without_flying() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let ashes = scenario
        .add_creature_from_oracle(P0, "Martyr of Ashes", 1, 1, MARTYR_OF_ASHES)
        .id();
    let r1 = colored_hand_card(&mut scenario, P0, "Red One", ManaColor::Red);
    let r2 = colored_hand_card(&mut scenario, P0, "Red Two", ManaColor::Red);
    let r3 = colored_hand_card(&mut scenario, P0, "Red Three", ManaColor::Red);
    let u1 = colored_hand_card(&mut scenario, P0, "Blue One", ManaColor::Blue);
    colored_hand_card(&mut scenario, P1, "Opponent Red One", ManaColor::Red);
    colored_hand_card(&mut scenario, P1, "Opponent Red Two", ManaColor::Red);
    // Toughness equal to X: destroyed. Toughness above X: X damage marked.
    let bear = scenario.add_creature(P1, "Ground Bear", 2, 2).id();
    let wall = scenario.add_creature(P1, "Ground Wall", 0, 3).id();
    let own = scenario.add_creature(P0, "Own Ground", 1, 4).id();
    // A flier that X damage would destroy.
    let bird = scenario.add_creature(P1, "Sky Bird", 1, 1).flying().id();
    scenario.with_mana_pool(P0, pool(&[ManaType::Colorless, ManaType::Colorless]));
    let mut runner = scenario.build();

    let mut events = activate(&mut runner, ashes);

    // CR 107.3a: X is capped at the red cards the activator can reveal — not
    // the off-color card, not the opponent's red cards.
    assert_eq!(offered_x(&runner), (0, 3));
    assert!(
        runner.act(GameAction::ChooseX { value: 4 }).is_err(),
        "X above the red cards in hand must be rejected"
    );
    assert_eq!(offered_x(&runner), (0, 3));
    events.extend(
        runner
            .act(GameAction::ChooseX { value: 2 })
            .expect("announce X = 2")
            .events,
    );
    pay_mana(&mut runner, &mut events);

    let choices = reveal_prompt(&runner, 2);
    assert_eq!(choices, HashSet::from([r1, r2, r3]));
    assert!(
        runner
            .act(GameAction::SelectCards {
                cards: vec![r1, u1]
            })
            .is_err(),
        "revealing an off-color card must be rejected"
    );
    assert_still_revealing(&runner);
    events.extend(
        runner
            .act(GameAction::SelectCards {
                cards: vec![r1, r3],
            })
            .expect("revealing two red cards pays the cost")
            .events,
    );

    assert!(
        revealed_cards(&events).contains(&vec![r1, r3]),
        "the two chosen red cards are revealed, got {:?}",
        revealed_cards(&events)
    );
    // CR 701.20b: revealed cards stay in hand.
    for card in [r1, r2, r3, u1] {
        assert_eq!(runner.state().objects[&card].zone, Zone::Hand);
    }
    assert_eq!(
        runner.state().objects[&ashes].zone,
        Zone::Graveyard,
        "the self-sacrifice cost is paid"
    );
    assert!(
        runner.state().players[0].mana_pool.is_empty(),
        "the {{2}} is paid"
    );
    assert_eq!(runner.state().stack.len(), 1);

    resolve(&mut runner);
    let obj = |id: ObjectId| &runner.state().objects[&id];
    assert_eq!(obj(bear).zone, Zone::Graveyard, "2 damage destroys the 2/2");
    assert_eq!(obj(wall).zone, Zone::Battlefield);
    assert_eq!(obj(wall).damage_marked, 2, "the 0/3 is dealt X = 2");
    assert_eq!(obj(own).zone, Zone::Battlefield);
    assert_eq!(
        obj(own).damage_marked,
        2,
        "each creature without flying, the activator's included"
    );
    assert_eq!(
        obj(bird).zone,
        Zone::Battlefield,
        "the flier is not dealt damage"
    );
    assert_eq!(obj(bird).damage_marked, 0);
}

/// Martyr of Frost above a real opponent spell.
struct FrostBoard {
    runner: GameRunner,
    frost: ObjectId,
    /// The opponent's creature spell on the stack.
    spell: ObjectId,
    /// P0's own spell above it: a second legal target with another controller.
    own_spell: ObjectId,
    blue: Vec<ObjectId>,
    off_color: ObjectId,
    opponent_lands: Vec<ObjectId>,
}

/// P1 (the active player, four Forests) casts a {2} Bear and passes; P0 casts
/// a {0} instant above it and keeps priority, holding Martyr of Frost, three
/// blue cards and a red card, with {2} floating. P1's hand also holds two blue
/// cards, which cannot pay P0's reveal.
fn frost_above_opponent_spell() -> FrostBoard {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let frost = scenario
        .add_creature_from_oracle(P0, "Martyr of Frost", 1, 1, MARTYR_OF_FROST)
        .id();
    let blue = vec![
        colored_hand_card(&mut scenario, P0, "Blue One", ManaColor::Blue),
        colored_hand_card(&mut scenario, P0, "Blue Two", ManaColor::Blue),
        colored_hand_card(&mut scenario, P0, "Blue Three", ManaColor::Blue),
    ];
    let off_color = colored_hand_card(&mut scenario, P0, "Red One", ManaColor::Red);
    colored_hand_card(&mut scenario, P1, "Opponent Blue One", ManaColor::Blue);
    colored_hand_card(&mut scenario, P1, "Opponent Blue Two", ManaColor::Blue);
    let spell = scenario
        .add_creature_to_hand(P1, "Opponent Bear", 2, 2)
        .with_mana_cost(ManaCost::generic(2))
        .id();
    let own_spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Quick Tonic", true, QUICK_TONIC)
        .with_mana_cost(ManaCost::zero())
        .id();
    let opponent_lands = (0..4)
        .map(|_| scenario.add_basic_land(P1, ManaColor::Green))
        .collect();
    scenario.with_mana_pool(P0, pool(&[ManaType::Colorless, ManaType::Colorless]));
    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.active_player = P1;
        state.priority_player = P1;
        state.waiting_for = WaitingFor::Priority { player: P1 };
    }
    runner.cast(spell).commit();
    assert_eq!(
        runner.state().stack.len(),
        1,
        "the opponent's spell is cast"
    );
    assert_eq!(runner.state().objects[&spell].zone, Zone::Stack);
    runner.act(GameAction::PassPriority).expect("P1 passes");
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { player: P0 }
        ),
        "P0 receives priority above the opponent's spell, got {:?}",
        runner.state().waiting_for
    );
    runner.cast(own_spell).commit();
    assert_eq!(runner.state().stack.len(), 2, "P0's own spell is cast");
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { player: P0 }
        ),
        "P0 keeps priority after casting, got {:?}",
        runner.state().waiting_for
    );
    FrostBoard {
        runner,
        frost,
        spell,
        own_spell,
        blue,
        off_color,
        opponent_lands,
    }
}

fn tapped_opponent_lands(board: &FrostBoard) -> usize {
    board
        .opponent_lands
        .iter()
        .filter(|land| board.runner.state().objects[*land].tapped)
        .count()
}

/// Activates Martyr of Frost with X = `x` targeting the opponent's spell,
/// reveals exactly X blue cards, and resolves the ability up to the
/// unless-payment prompt. CR 118.12a: the spell's controller (P1) is asked to
/// pay {X} with the announced X (CR 107.3a); CR 118.5: a {0} is asked too.
fn frost_up_to_unless_payment(board: &mut FrostBoard, x: u32) {
    let runner = &mut board.runner;
    let mut events = activate(runner, board.frost);
    assert_eq!(offered_x(runner), (0, 3));
    events.extend(
        runner
            .act(GameAction::ChooseX { value: x })
            .expect("announce X")
            .events,
    );

    // CR 601.2c: the target is chosen after X is announced.
    let WaitingFor::TargetSelection { target_slots, .. } = &runner.state().waiting_for else {
        panic!(
            "the activation must choose its target after X, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(target_slots.len(), 1);
    assert_eq!(
        target_slots[0]
            .legal_targets
            .iter()
            .cloned()
            .collect::<HashSet<_>>(),
        HashSet::from([
            TargetRef::Object(board.spell),
            TargetRef::Object(board.own_spell)
        ]),
        "both spells on the stack are legal targets"
    );
    events.extend(
        runner
            .act(GameAction::ChooseTarget {
                target: Some(TargetRef::Object(board.spell)),
            })
            .expect("target the opponent's spell")
            .events,
    );
    pay_mana(runner, &mut events);

    let reveal = &board.blue[..x as usize];
    if x > 0 {
        let choices = reveal_prompt(runner, x as usize);
        assert_eq!(choices, board.blue.iter().copied().collect());
        let mut off = reveal[1..].to_vec();
        off.push(board.off_color);
        assert!(
            runner.act(GameAction::SelectCards { cards: off }).is_err(),
            "revealing an off-color card must be rejected"
        );
        assert_still_revealing(runner);
        events.extend(
            runner
                .act(GameAction::SelectCards {
                    cards: reveal.to_vec(),
                })
                .expect("revealing X blue cards pays the cost")
                .events,
        );
    }
    let revealed: Vec<ObjectId> = revealed_cards(&events).into_iter().flatten().collect();
    assert_eq!(
        revealed,
        reveal.to_vec(),
        "exactly the X chosen cards are revealed"
    );

    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { player: P0 }
        ),
        "the activation completes, got {:?}",
        runner.state().waiting_for
    );
    assert_eq!(runner.state().objects[&board.frost].zone, Zone::Graveyard);
    assert!(
        runner.state().players[0].mana_pool.is_empty(),
        "the {{2}} is paid"
    );
    assert_eq!(runner.state().stack.len(), 3);
    let ability = runner
        .state()
        .stack
        .back()
        .and_then(|entry| entry.ability())
        .expect("Frost's ability is on top of the stack");
    assert_eq!(ability.targets, vec![TargetRef::Object(board.spell)]);
    assert_eq!(tapped_opponent_lands(board), 2, "the Bear's {{2}} only");

    board.runner.resolve_top();
    let WaitingFor::UnlessPayment { player, cost, .. } = &board.runner.state().waiting_for else {
        panic!(
            "Frost must ask the spell's controller to pay, got {:?}",
            board.runner.state().waiting_for
        );
    };
    assert_eq!(*player, P1, "the targeted spell's controller is asked");
    assert_eq!(
        *cost,
        AbilityCost::Mana {
            cost: ManaCost::generic(x)
        },
        "the unless cost is the announced X"
    );
    assert_eq!(board.runner.state().objects[&board.spell].zone, Zone::Stack);
    assert_eq!(
        board.runner.state().stack.len(),
        2,
        "only Frost's ability has left the stack"
    );
}

/// After the unless payment is answered, P0's untargeted spell is untouched
/// and resolves; the stack empties back to priority.
fn resolve_rest(board: &mut FrostBoard) {
    assert_eq!(
        board.runner.state().objects[&board.own_spell].zone,
        Zone::Stack,
        "the untargeted spell is not countered"
    );
    let life_before = board.runner.life(P0);
    resolve(&mut board.runner);
    assert_eq!(
        board.runner.state().objects[&board.own_spell].zone,
        Zone::Graveyard
    );
    assert_eq!(
        board.runner.life(P0) - life_before,
        1,
        "P0's spell resolved"
    );
}

/// X = 2, the opponent pays {2}: the spell is not countered and resolves.
#[test]
fn martyr_of_frost_spell_resolves_when_its_controller_pays_x() {
    let mut board = frost_above_opponent_spell();
    frost_up_to_unless_payment(&mut board, 2);
    board
        .runner
        .act(GameAction::PayUnlessCost { pay: true })
        .expect("P1 pays {2}");
    assert_eq!(tapped_opponent_lands(&board), 4, "P1 paid X = 2 more");
    assert_eq!(board.runner.state().objects[&board.spell].zone, Zone::Stack);
    resolve_rest(&mut board);
    assert_eq!(
        board.runner.state().objects[&board.spell].zone,
        Zone::Battlefield,
        "the paid-for spell resolves"
    );
}

/// X = 2, the opponent declines: the spell is countered (CR 701.6a).
#[test]
fn martyr_of_frost_counters_the_spell_when_its_controller_declines() {
    let mut board = frost_above_opponent_spell();
    frost_up_to_unless_payment(&mut board, 2);
    board
        .runner
        .act(GameAction::PayUnlessCost { pay: false })
        .expect("P1 declines");
    assert_eq!(tapped_opponent_lands(&board), 2, "nothing is paid");
    assert_eq!(
        board.runner.state().objects[&board.spell].zone,
        Zone::Graveyard,
        "the declined spell is countered"
    );
    resolve_rest(&mut board);
}

/// X = 0 reveals nothing and asks for {0}. CR 118.5: a {0} cost is paid only
/// by acknowledging it, so acknowledging lets the spell resolve and declining
/// counters it.
#[test]
fn martyr_of_frost_x_zero_asks_for_zero() {
    for pay in [true, false] {
        let mut board = frost_above_opponent_spell();
        frost_up_to_unless_payment(&mut board, 0);
        for card in board.blue.iter().chain([&board.off_color]) {
            assert_eq!(board.runner.state().objects[card].zone, Zone::Hand);
        }
        board
            .runner
            .act(GameAction::PayUnlessCost { pay })
            .expect("P1 answers the {0}");
        assert_eq!(tapped_opponent_lands(&board), 2, "{{0}} taps nothing");
        resolve_rest(&mut board);
        assert_eq!(
            board.runner.state().objects[&board.spell].zone,
            if pay {
                Zone::Battlefield
            } else {
                Zone::Graveyard
            },
            "pay = {pay}"
        );
    }
}

fn ember_tithe(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_spell_to_hand_from_oracle(P0, "Ember Tithe", false, EMBER_TITHE)
        .with_mana_cost(ManaCost::zero())
        .with_color(vec![ManaColor::Red])
        .id()
}

fn cast(runner: &mut GameRunner, spell: ObjectId) -> Vec<GameEvent> {
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("the cast must be accepted")
        .events
}

/// R12: the same cost as a spell's additional cost announces X, reveals
/// exactly X red cards from the caster's hand, and the effect reads X.
#[test]
fn spell_reveal_x_additional_cost_reveals_the_announced_number() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = ember_tithe(&mut scenario);
    let r1 = colored_hand_card(&mut scenario, P0, "Red One", ManaColor::Red);
    let r2 = colored_hand_card(&mut scenario, P0, "Red Two", ManaColor::Red);
    let g1 = colored_hand_card(&mut scenario, P0, "Green One", ManaColor::Green);
    colored_hand_card(&mut scenario, P1, "Opponent Red", ManaColor::Red);
    let mut runner = scenario.build();
    let life_before = runner.life(P0);

    let mut events = cast(&mut runner, spell);
    // CR 601.2a: the red spell itself is on the stack, not a card in hand.
    assert_eq!(offered_x(&runner), (0, 2));
    assert!(runner.act(GameAction::ChooseX { value: 3 }).is_err());
    events.extend(
        runner
            .act(GameAction::ChooseX { value: 2 })
            .expect("announce X = 2")
            .events,
    );
    pay_mana(&mut runner, &mut events);

    let choices = reveal_prompt(&runner, 2);
    assert_eq!(choices, HashSet::from([r1, r2]));
    assert!(runner
        .act(GameAction::SelectCards { cards: vec![r1] })
        .is_err());
    assert_still_revealing(&runner);
    assert!(runner
        .act(GameAction::SelectCards {
            cards: vec![r1, g1]
        })
        .is_err());
    assert_still_revealing(&runner);
    events.extend(
        runner
            .act(GameAction::SelectCards {
                cards: vec![r1, r2],
            })
            .expect("reveal both red cards")
            .events,
    );
    assert!(revealed_cards(&events).contains(&vec![r1, r2]));

    resolve(&mut runner);
    assert_eq!(runner.life(P0) - life_before, 2, "the effect reads X = 2");
    assert_eq!(runner.state().objects[&r1].zone, Zone::Hand);
    assert_eq!(runner.state().objects[&r2].zone, Zone::Hand);
    assert_eq!(runner.state().objects[&spell].zone, Zone::Graveyard);
}

/// R12 (X = 0): announcing zero reveals nothing and the spell goes straight
/// to the stack (CR 107.3a + CR 601.2h).
#[test]
fn spell_reveal_x_additional_cost_zero_reveals_nothing() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = ember_tithe(&mut scenario);
    let r1 = colored_hand_card(&mut scenario, P0, "Red One", ManaColor::Red);
    let mut runner = scenario.build();
    let life_before = runner.life(P0);

    let mut events = cast(&mut runner, spell);
    assert_eq!(offered_x(&runner), (0, 1));
    events.extend(
        runner
            .act(GameAction::ChooseX { value: 0 })
            .expect("announce X = 0")
            .events,
    );
    pay_mana(&mut runner, &mut events);

    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "a zero reveal surfaces no prompt, got {:?}",
        runner.state().waiting_for
    );
    assert_eq!(runner.state().stack.len(), 1);
    assert_eq!(runner.state().stack[0].source_id, spell);
    assert!(
        !revealed_cards(&events).iter().any(|ids| ids.contains(&r1)),
        "no card is revealed for X = 0"
    );

    resolve(&mut runner);
    assert_eq!(runner.life(P0), life_before);
    assert_eq!(runner.state().objects[&r1].zone, Zone::Hand);
}

/// Submits `[card, card]` against an open reveal prompt and asserts the
/// rejection leaves the prompt, life and stack exactly as they were.
fn assert_duplicate_reveal_rejected(runner: &mut GameRunner, card: ObjectId) {
    let prompt_before = runner.state().waiting_for.clone();
    let life_before = runner.life(P0);
    let stack_before = runner.state().stack.len();
    assert!(
        runner
            .act(GameAction::SelectCards {
                cards: vec![card, card],
            })
            .is_err(),
        "revealing one card twice must not pay a reveal of two cards"
    );
    assert_eq!(
        runner.state().waiting_for,
        prompt_before,
        "the rejected duplicate leaves the same reveal prompt open"
    );
    assert_eq!(runner.life(P0), life_before, "no partial payment");
    assert_eq!(
        runner.state().stack.len(),
        stack_before,
        "no partial payment"
    );
}

/// CR 601.2h + CR 701.20a: a reveal of X = 2 cards needs two different
/// cards; a raw `SelectCards` naming one card twice is rejected before any
/// payment, and the distinct pair then pays the activation cost.
#[test]
fn martyr_of_sands_rejects_revealing_one_card_twice() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let sands = scenario
        .add_creature_from_oracle(P0, "Martyr of Sands", 1, 1, MARTYR_OF_SANDS)
        .id();
    let w1 = colored_hand_card(&mut scenario, P0, "White One", ManaColor::White);
    let w2 = colored_hand_card(&mut scenario, P0, "White Two", ManaColor::White);
    scenario.with_mana_pool(P0, pool(&[ManaType::Colorless]));
    let mut runner = scenario.build();
    let life_before = runner.life(P0);

    let mut events = activate(&mut runner, sands);
    events.extend(
        runner
            .act(GameAction::ChooseX { value: 2 })
            .expect("announce X = 2")
            .events,
    );
    pay_mana(&mut runner, &mut events);
    assert_eq!(reveal_prompt(&runner, 2), HashSet::from([w1, w2]));

    assert_duplicate_reveal_rejected(&mut runner, w1);
    assert_eq!(reveal_prompt(&runner, 2), HashSet::from([w1, w2]));
    assert_eq!(
        runner.state().objects[&sands].zone,
        Zone::Battlefield,
        "the sacrifice leg is not paid by a rejected reveal"
    );
    assert!(
        revealed_cards(&events).is_empty(),
        "nothing has been revealed yet, got {:?}",
        revealed_cards(&events)
    );

    events.extend(
        runner
            .act(GameAction::SelectCards {
                cards: vec![w1, w2],
            })
            .expect("revealing two different white cards pays the cost")
            .events,
    );
    assert_eq!(revealed_cards(&events), vec![vec![w1, w2]]);
    assert_eq!(runner.state().objects[&sands].zone, Zone::Graveyard);
    resolve(&mut runner);
    assert_eq!(runner.life(P0) - life_before, 6);
}

/// CR 601.2h + CR 701.20a: the spell additional-cost leg rejects the same
/// duplicate reveal, and the distinct pair then completes the cast.
#[test]
fn spell_reveal_x_additional_cost_rejects_revealing_one_card_twice() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = ember_tithe(&mut scenario);
    let r1 = colored_hand_card(&mut scenario, P0, "Red One", ManaColor::Red);
    let r2 = colored_hand_card(&mut scenario, P0, "Red Two", ManaColor::Red);
    let mut runner = scenario.build();
    let life_before = runner.life(P0);

    let mut events = cast(&mut runner, spell);
    events.extend(
        runner
            .act(GameAction::ChooseX { value: 2 })
            .expect("announce X = 2")
            .events,
    );
    pay_mana(&mut runner, &mut events);
    assert_eq!(reveal_prompt(&runner, 2), HashSet::from([r1, r2]));

    assert_duplicate_reveal_rejected(&mut runner, r1);
    assert_eq!(reveal_prompt(&runner, 2), HashSet::from([r1, r2]));
    assert!(
        revealed_cards(&events).is_empty(),
        "nothing has been revealed yet, got {:?}",
        revealed_cards(&events)
    );

    events.extend(
        runner
            .act(GameAction::SelectCards {
                cards: vec![r1, r2],
            })
            .expect("revealing two different red cards pays the cost")
            .events,
    );
    assert_eq!(revealed_cards(&events), vec![vec![r1, r2]]);
    resolve(&mut runner);
    assert_eq!(runner.life(P0) - life_before, 2);
    assert_eq!(runner.state().objects[&spell].zone, Zone::Graveyard);
}
