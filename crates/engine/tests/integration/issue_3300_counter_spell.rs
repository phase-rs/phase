//! GitHub issue #3300 — Countering an opponent's spell must remove it from the stack.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::ability::Effect;
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::game_state::{CastingVariant, StackEntry, StackEntryKind};
use engine::types::identifiers::CardId;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const COUNTERSPELL_ORACLE: &str = "Counter target spell.";

fn put_instant_on_stack(
    runner: &mut engine::game::scenario::GameRunner,
    controller: engine::types::player::PlayerId,
) -> engine::types::identifiers::ObjectId {
    let spell = engine::game::zones::create_object(
        runner.state_mut(),
        CardId(501),
        controller,
        "Shock".to_string(),
        Zone::Stack,
    );
    if let Some(obj) = runner.state_mut().objects.get_mut(&spell) {
        obj.card_types.core_types = vec![CoreType::Instant];
    }
    runner.state_mut().stack.push_back(StackEntry {
        id: spell,
        source_id: spell,
        controller,
        kind: StackEntryKind::Spell {
            card_id: CardId(501),
            ability: None,
            casting_variant: CastingVariant::Normal,
            actual_mana_spent: 0,
        },
    });
    spell
}

#[test]
fn counterspell_parses_counter_target_spell() {
    let mut scenario = GameScenario::new();
    let counterspell = scenario
        .add_spell_to_hand_from_oracle(P0, "Counterspell", true, COUNTERSPELL_ORACLE)
        .id();
    let runner = scenario.build();
    let ability = &runner.state().objects[&counterspell].abilities[0];
    assert!(
        matches!(ability.effect.as_ref(), Effect::Counter { .. }),
        "Counterspell must parse to Counter"
    );
}

#[test]
fn counterspell_counters_opponent_spell_on_stack() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mut cs =
        scenario.add_spell_to_hand_from_oracle(P0, "Counterspell", true, COUNTERSPELL_ORACLE);
    cs.with_mana_cost(ManaCost::Cost {
        generic: 0,
        shards: vec![ManaCostShard::Blue, ManaCostShard::Blue],
    });
    let counterspell = cs.id();
    scenario.add_basic_land(P0, ManaColor::Blue);
    scenario.add_basic_land(P0, ManaColor::Blue);

    let mut runner = scenario.build();
    let opponent_spell = put_instant_on_stack(&mut runner, P1);

    runner
        .cast(counterspell)
        .target_objects(&[opponent_spell])
        .resolve();

    assert!(
        runner.state().stack.is_empty(),
        "countering must remove both spells from the stack, got {:?}",
        runner.state().stack
    );
    assert_eq!(
        runner.state().objects.get(&opponent_spell).map(|o| o.zone),
        Some(Zone::Graveyard),
        "countered opponent spell must move to graveyard"
    );
}

#[test]
fn mana_leak_counters_when_opponent_declines_to_pay() {
    const MANA_LEAK: &str = "Counter target spell unless its controller pays {3}.";
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mut leak = scenario.add_spell_to_hand_from_oracle(P0, "Mana Leak", true, MANA_LEAK);
    leak.with_mana_cost(ManaCost::Cost {
        generic: 1,
        shards: vec![ManaCostShard::Blue],
    });
    let mana_leak = leak.id();
    scenario.add_basic_land(P0, ManaColor::Blue);
    scenario.add_basic_land(P0, ManaColor::Blue);

    let mut runner = scenario.build();
    let opponent_spell = put_instant_on_stack(&mut runner, P1);

    runner
        .cast(mana_leak)
        .target_objects(&[opponent_spell])
        .resolve();

    assert!(
        matches!(
            runner.state().waiting_for,
            engine::types::game_state::WaitingFor::UnlessPayment { player: P1, .. }
        ),
        "Mana Leak must prompt opponent to pay {{3}}, got {:?}",
        runner.state().waiting_for
    );

    runner
        .act(engine::types::actions::GameAction::PayUnlessCost { pay: false })
        .expect("P1 declines to pay");

    assert!(
        runner.state().stack.is_empty(),
        "declining unless cost must counter the spell"
    );
    assert_eq!(
        runner.state().objects.get(&opponent_spell).map(|o| o.zone),
        Some(Zone::Graveyard)
    );
}

/// CR 117.7 + CR 701.6a: Counterspell cast in response must counter the
/// targeted spell when it resolves — the opponent's spell must not resolve
/// afterward.
#[test]
fn counterspell_in_response_counters_before_target_resolves() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_basic_land(P0, ManaColor::Green);
    scenario.add_basic_land(P0, ManaColor::Green);
    scenario.add_basic_land(P1, ManaColor::Blue);
    scenario.add_basic_land(P1, ManaColor::Blue);

    let bear = scenario
        .add_creature_to_hand(P0, "Grizzly Bears", 2, 2)
        .id();
    let mut cs =
        scenario.add_spell_to_hand_from_oracle(P1, "Counterspell", true, COUNTERSPELL_ORACLE);
    cs.with_mana_cost(ManaCost::Cost {
        generic: 0,
        shards: vec![ManaCostShard::Blue, ManaCostShard::Blue],
    });
    let counterspell = cs.id();

    let mut runner = scenario.build();

    // P0 casts a creature spell.
    let bear_card = runner.state().objects[&bear].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: bear,
            card_id: bear_card,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast bear");
    assert_eq!(runner.state().stack.len(), 1, "bear should be on stack");

    // P0 passes priority to P1.
    runner.act(GameAction::PassPriority).expect("P0 pass");

    // P1 casts Counterspell — target selection follows announcement.
    let cs_card = runner.state().objects[&counterspell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: counterspell,
            card_id: cs_card,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast counterspell");

    // With only one legal stack target (the bear), target selection auto-completes.
    assert_eq!(
        runner.state().stack.len(),
        2,
        "counterspell and bear should both be on stack"
    );
    let counter_entry = runner
        .state()
        .stack
        .iter()
        .find(|e| e.id == counterspell)
        .expect("counterspell on stack");
    let counter_ability = counter_entry.ability().expect("counterspell ability");
    assert!(
        counter_ability
            .targets
            .iter()
            .any(|t| matches!(t, engine::types::ability::TargetRef::Object(id) if *id == bear)),
        "counterspell must target the bear, got {:?}",
        counter_ability.targets
    );

    // Both players pass until the stack empties.
    while !runner.state().stack.is_empty() {
        if matches!(runner.state().waiting_for, WaitingFor::Priority { .. }) {
            runner.act(GameAction::PassPriority).expect("pass priority");
        } else if matches!(runner.state().waiting_for, WaitingFor::UnlessPayment { .. }) {
            runner
                .act(GameAction::PayUnlessCost { pay: false })
                .expect("decline unless");
        } else {
            panic!(
                "unexpected waiting state during stack resolution: {:?}",
                runner.state().waiting_for
            );
        }
    }

    assert_eq!(
        runner.state().objects.get(&bear).map(|o| o.zone),
        Some(Zone::Graveyard),
        "countered bear must not enter the battlefield"
    );
    assert!(
        !runner.state().battlefield.contains(&bear),
        "countered spell must not resolve to battlefield"
    );
}

/// Mana Leak cast in response must offer the targeted spell's controller the
/// unless-pay choice before countering.
#[test]
fn mana_leak_in_response_prompts_target_controller_then_counters() {
    const MANA_LEAK: &str = "Counter target spell unless its controller pays {3}.";
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_basic_land(P0, ManaColor::Green);
    scenario.add_basic_land(P0, ManaColor::Green);
    scenario.add_basic_land(P1, ManaColor::Blue);
    scenario.add_basic_land(P1, ManaColor::Blue);

    let bear = scenario
        .add_creature_to_hand(P0, "Grizzly Bears", 2, 2)
        .id();
    let mut leak = scenario.add_spell_to_hand_from_oracle(P1, "Mana Leak", true, MANA_LEAK);
    leak.with_mana_cost(ManaCost::Cost {
        generic: 1,
        shards: vec![ManaCostShard::Blue],
    });
    let mana_leak = leak.id();

    let mut runner = scenario.build();

    let bear_card = runner.state().objects[&bear].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: bear,
            card_id: bear_card,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast bear");
    runner.act(GameAction::PassPriority).expect("P0 pass");

    let leak_card = runner.state().objects[&mana_leak].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: mana_leak,
            card_id: leak_card,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast mana leak");

    // Only the bear is a legal target once self-targeting is excluded.
    let leak_entry = runner
        .state()
        .stack
        .iter()
        .find(|e| e.id == mana_leak)
        .expect("mana leak on stack");
    let leak_ability = leak_entry.ability().expect("mana leak ability");
    assert!(
        leak_ability
            .targets
            .iter()
            .any(|t| matches!(t, engine::types::ability::TargetRef::Object(id) if *id == bear)),
        "Mana Leak must auto-target the bear, got {:?}",
        leak_ability.targets
    );

    while !runner.state().stack.is_empty() {
        match &runner.state().waiting_for {
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).expect("pass");
            }
            WaitingFor::UnlessPayment { player, .. } => {
                assert_eq!(*player, P0, "bear controller must receive unless prompt");
                runner
                    .act(GameAction::PayUnlessCost { pay: false })
                    .expect("decline to pay");
            }
            other => panic!("unexpected waiting state: {other:?}"),
        }
    }

    assert_eq!(
        runner.state().objects.get(&bear).map(|o| o.zone),
        Some(Zone::Graveyard)
    );
}

const RUNE_SNAG: &str = "Counter target spell unless its controller pays {2} plus an additional {2} for each card named Rune Snag in each graveyard.";

fn put_card_in_graveyard(
    runner: &mut engine::game::scenario::GameRunner,
    owner: engine::types::player::PlayerId,
    card_id: u64,
    name: &str,
) {
    engine::game::zones::create_object(
        runner.state_mut(),
        CardId(card_id),
        owner,
        name.to_string(),
        Zone::Graveyard,
    );
}

/// Casts Rune Snag (P0) at a Shock on the stack (P1, holding `lands` Islands),
/// after seeding the graveyards with `snags_p0` / `snags_p1` copies of Rune Snag
/// and one differently named decoy. Returns the runner paused on P1's prompt
/// together with P1's lands.
fn cast_rune_snag(
    snags_p0: u64,
    snags_p1: u64,
    lands: usize,
) -> (
    engine::game::scenario::GameRunner,
    Vec<engine::types::identifiers::ObjectId>,
) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mut snag = scenario.add_spell_to_hand_from_oracle(P0, "Rune Snag", true, RUNE_SNAG);
    snag.with_mana_cost(ManaCost::Cost {
        generic: 0,
        shards: vec![ManaCostShard::Blue],
    });
    let rune_snag = snag.id();
    scenario.add_basic_land(P0, ManaColor::Blue);
    let p1_lands: Vec<_> = (0..lands)
        .map(|_| scenario.add_basic_land(P1, ManaColor::Blue))
        .collect();

    let mut runner = scenario.build();
    for n in 0..snags_p0 {
        put_card_in_graveyard(&mut runner, P0, 700 + n, "Rune Snag");
    }
    for n in 0..snags_p1 {
        put_card_in_graveyard(&mut runner, P1, 800 + n, "Rune Snag");
    }
    // Decoy: a differently named card must not be counted.
    put_card_in_graveyard(&mut runner, P1, 900, "Rune Stag");

    let opponent_spell = put_instant_on_stack(&mut runner, P1);
    runner
        .cast(rune_snag)
        .target_objects(&[opponent_spell])
        .resolve();
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::UnlessPayment { player: P1, .. }
        ),
        "Rune Snag must prompt the countered spell's controller, got {:?}",
        runner.state().waiting_for
    );
    (runner, p1_lands)
}

fn tapped_count(
    runner: &engine::game::scenario::GameRunner,
    lands: &[engine::types::identifiers::ObjectId],
) -> usize {
    lands
        .iter()
        .filter(|id| runner.state().objects[id].tapped)
        .count()
}

/// Rune Snag parses to a real Counter (it used to fail closed and counter
/// nothing). With no copy in a graveyard the unless cost is the flat {2}; one
/// copy in EACH graveyard raises it to {2} + 2 × 2 = {6} (CR 201.2 + CR 400.1).
#[test]
fn rune_snag_counts_copies_in_every_graveyard() {
    // Zero copies: {2} is payable with two lands, and exactly two are spent.
    let (mut runner, lands) = cast_rune_snag(0, 0, 2);
    runner
        .act(GameAction::PayUnlessCost { pay: true })
        .expect("{2} is payable with two Islands");
    assert_eq!(tapped_count(&runner, &lands), 2, "paid exactly {{2}}");

    // One copy in each player's graveyard: cost {6}; five lands cannot pay it, so nothing is tapped.
    let (mut runner, lands) = cast_rune_snag(1, 1, 5);
    // The engine resolves an unaffordable payment as a failed payment.
    runner
        .act(GameAction::PayUnlessCost { pay: true })
        .expect("attempt to pay");
    assert_eq!(tapped_count(&runner, &lands), 0, "nothing was paid");

    // Same graveyards, six lands: {6} is payable and all six are spent.
    let (mut runner, lands) = cast_rune_snag(1, 1, 6);
    runner
        .act(GameAction::PayUnlessCost { pay: true })
        .expect("{6} is payable with six Islands");
    assert_eq!(tapped_count(&runner, &lands), 6, "paid exactly {{6}}");
}

/// Declining the Rune Snag payment counters the spell.
#[test]
fn rune_snag_counters_spell_when_payment_declined() {
    let (mut runner, _) = cast_rune_snag(2, 0, 1);
    runner
        .act(GameAction::PayUnlessCost { pay: false })
        .expect("decline");
    assert!(runner.state().stack.is_empty());
    assert!(runner
        .state()
        .objects
        .values()
        .any(|o| o.name == "Shock" && o.zone == Zone::Graveyard));
}
