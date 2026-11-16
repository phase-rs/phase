//! CR 607.1 + CR 607.2a + CR 406.6 + CR 603.3: Tidehollow Sculler exile and return.
//!
//! "When this creature enters, target opponent reveals their hand and you choose a
//! nonland card from it. Exile that card.
//! When this creature leaves the battlefield, return the exiled card to its
//! owner's hand."
//!
//! Two linked triggered abilities (CR 607.2a), not a CR 610.3 "until" duration: the
//! return is its own triggered ability that goes on the stack when Sculler leaves
//! the battlefield, so it can be responded to and the card stays exiled until it
//! resolves.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::{ExileLinkKind, StackEntryKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const TIDEHOLLOW_SCULLER: &str = "When this creature enters, target opponent reveals their hand and you choose a nonland card from it. Exile that card.\n\
When this creature leaves the battlefield, return the exiled card to its owner's hand.";

const MESMERIC_FIEND: &str = "When this creature enters, target opponent reveals their hand and you choose a nonland card from it. Exile that card.\n\
When this creature leaves the battlefield, return the exiled card to its owner's hand.";

const MURDER: &str = "Destroy target creature.";
const STRIONIC_RESONATOR: &str = "{2}, {T}: Copy target triggered ability you control. You may \
choose new targets for the copy. (A triggered ability uses the words \"when,\" \"whenever,\" or \
\"at.\")";

fn zone_of(runner: &GameRunner, id: ObjectId) -> Zone {
    runner.state().objects.get(&id).unwrap().zone
}

#[test]
fn tidehollow_sculler_returns_exiled_card_to_hand_when_it_leaves_play() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let sculler = scenario
        .add_creature_to_hand_from_oracle(P0, "Tidehollow Sculler", 2, 2, TIDEHOLLOW_SCULLER)
        .id();

    let target_card = scenario.add_card_to_hand(P1, "Doom Blade");
    let land_card = scenario.add_land_to_hand(P1, "Swamp").id();
    let destroy = scenario
        .add_spell_to_hand_from_oracle(P0, "Murder", true, MURDER)
        .id();

    let mut runner = scenario.build();

    let outcome = runner.cast(sculler).target_player(P1).resolve();

    let WaitingFor::RevealChoice { cards, .. } = outcome.final_waiting_for() else {
        panic!(
            "expected RevealChoice prompt after ETB resolves, got {:?}",
            outcome.final_waiting_for()
        );
    };
    assert!(cards.contains(&target_card));
    assert!(!cards.contains(&land_card));

    runner
        .act(GameAction::SelectCards {
            cards: vec![target_card],
        })
        .expect("SelectCards succeeds");

    assert_eq!(zone_of(&runner, target_card), Zone::Exile);
    assert_eq!(zone_of(&runner, land_card), Zone::Hand);
    assert!(runner.state().exile_links.iter().any(|link| {
        link.exiled_id == target_card
            && link.source_id == sculler
            && link.kind == ExileLinkKind::TrackedBySource
    }));

    // Destroy Tidehollow Sculler so it leaves the battlefield.
    runner.cast(destroy).target_object(sculler).resolve();
    runner.advance_until_stack_empty();

    assert_eq!(zone_of(&runner, sculler), Zone::Graveyard);
    assert_eq!(
        zone_of(&runner, target_card),
        Zone::Hand,
        "Target card must return to hand when Tidehollow Sculler leaves the battlefield"
    );
    assert!(runner.state().players[P1.0 as usize]
        .hand
        .contains(&target_card));
    assert!(!runner.state().exile.contains(&target_card));
    assert!(
        runner.state().exile_links.is_empty(),
        "ExileLink must be consumed"
    );
}

/// CR 607.3: copying Sculler's ETB trigger can exile two cards with
/// the same source. Its singular linked-return wording must process the full set.
#[test]
fn copied_tidehollow_sculler_etb_returns_both_linked_cards() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let sculler = scenario
        .add_creature_to_hand_from_oracle(P0, "Tidehollow Sculler", 2, 2, TIDEHOLLOW_SCULLER)
        .id();
    let first_card = scenario.add_card_to_hand(P1, "Doom Blade");
    let second_card = scenario.add_card_to_hand(P1, "Counterspell");
    let resonator = scenario
        .add_artifact_from_oracle(P0, "Strionic Resonator", STRIONIC_RESONATOR)
        .id();
    let destroy = scenario
        .add_spell_to_hand_from_oracle(P0, "Murder", true, MURDER)
        .id();
    scenario.with_mana_pool(
        P0,
        (0..2)
            .map(|_| ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]))
            .collect(),
    );
    let mut runner = scenario.build();

    drop(runner.cast(sculler).commit());
    for _ in 0..12 {
        if matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
            && runner.state().stack.last().is_some_and(|entry| {
                matches!(entry.kind, StackEntryKind::TriggeredAbility { source_id, .. } if source_id == sculler)
            })
        {
            break;
        }
        match runner.state().waiting_for {
            WaitingFor::TriggerTargetSelection { .. } => {
                runner
                    .act(GameAction::SelectTargets {
                        targets: vec![engine::types::ability::TargetRef::Player(P1)],
                    })
                    .expect("choose the opponent for Sculler's ETB");
            }
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("pass priority until Sculler's ETB is on the stack");
            }
            ref waiting => panic!("unexpected state before Sculler's ETB: {waiting:?}"),
        }
    }
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    let etb = runner.state().stack.last().expect("ETB on stack").id;
    let activation = runner.activate(resonator, 0).target_object(etb).resolve();
    if matches!(
        activation.final_waiting_for(),
        WaitingFor::CopyRetarget { .. }
    ) {
        runner
            .act(GameAction::KeepAllCopyTargets)
            .expect("keep the copied ETB's existing opponent target");
    }

    pass_until(&mut runner, "the copied ETB's exile choice", |r| {
        matches!(r.state().waiting_for, WaitingFor::RevealChoice { .. })
    });
    runner
        .act(GameAction::SelectCards {
            cards: vec![first_card],
        })
        .expect("choose the first linked card");
    pass_until(&mut runner, "the original ETB's exile choice", |r| {
        matches!(r.state().waiting_for, WaitingFor::RevealChoice { .. })
    });
    runner
        .act(GameAction::SelectCards {
            cards: vec![second_card],
        })
        .expect("choose the second linked card");

    for card in [first_card, second_card] {
        assert_eq!(zone_of(&runner, card), Zone::Exile);
        assert!(runner.state().exile_links.iter().any(|link| {
            link.exiled_id == card
                && link.source_id == sculler
                && link.kind == ExileLinkKind::TrackedBySource
        }));
    }

    runner.cast(destroy).target_object(sculler).resolve();
    runner.advance_until_stack_empty();

    assert_eq!(zone_of(&runner, first_card), Zone::Hand);
    assert_eq!(zone_of(&runner, second_card), Zone::Hand);
}

/// CR 607.2a + CR 707.10b + CR 607.3: a copy of Sculler's ETB made after Sculler
/// has left (in response to its leaves trigger) has the same source, so the card
/// it exiles is linked to the departed Sculler and the leaves trigger, resolving
/// after the copy, returns it. The original ETB resolves after the leaves trigger,
/// so its card stays exiled.
#[test]
fn tidehollow_sculler_returns_card_exiled_by_etb_copy_made_after_it_left() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let sculler = scenario
        .add_creature_to_hand_from_oracle(P0, "Tidehollow Sculler", 2, 2, TIDEHOLLOW_SCULLER)
        .id();
    let copied_card = scenario.add_card_to_hand(P1, "Doom Blade");
    let original_card = scenario.add_card_to_hand(P1, "Counterspell");
    let resonator = scenario
        .add_artifact_from_oracle(P0, "Strionic Resonator", STRIONIC_RESONATOR)
        .id();
    let destroy = scenario
        .add_spell_to_hand_from_oracle(P0, "Murder", true, MURDER)
        .id();
    scenario.with_mana_pool(
        P0,
        (0..2)
            .map(|_| ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]))
            .collect(),
    );
    let mut runner = scenario.build();

    drop(runner.cast(sculler).target_player(P1).commit());
    pass_until(&mut runner, "Sculler's ETB trigger on the stack", |r| {
        zone_of(r, sculler) == Zone::Battlefield && !r.state().stack.is_empty()
    });
    let etb = runner.state().stack[0].id;

    // Destroy Sculler in response, so its leaves trigger goes above the ETB.
    drop(runner.cast(destroy).target_object(sculler).commit());
    pass_until(&mut runner, "Sculler's leaves trigger above its ETB", |r| {
        zone_of(r, sculler) == Zone::Graveyard
            && r.state().stack.len() == 2
            && r.state()
                .stack
                .iter()
                .all(|entry| entry.source_id == sculler)
    });

    // In response to the leaves trigger, copy the still-pending ETB.
    let activation = runner.activate(resonator, 0).target_object(etb).resolve();
    if matches!(
        activation.final_waiting_for(),
        WaitingFor::CopyRetarget { .. }
    ) {
        runner
            .act(GameAction::KeepAllCopyTargets)
            .expect("keep the copied ETB's existing opponent target");
    }
    pass_until(&mut runner, "the copied ETB's exile choice", |r| {
        matches!(r.state().waiting_for, WaitingFor::RevealChoice { .. })
    });
    assert_eq!(
        runner.state().stack.len(),
        2,
        "the leaves trigger and the original ETB still wait below the copy"
    );
    runner
        .act(GameAction::SelectCards {
            cards: vec![copied_card],
        })
        .expect("choose the copied ETB's card");
    assert_eq!(zone_of(&runner, copied_card), Zone::Exile);

    // The leaves trigger resolves next, then the original ETB asks for its card.
    pass_until(&mut runner, "the original ETB's exile choice", |r| {
        matches!(r.state().waiting_for, WaitingFor::RevealChoice { .. })
    });
    assert_eq!(
        zone_of(&runner, copied_card),
        Zone::Hand,
        "the leaves trigger returns the card the post-departure copy exiled"
    );
    runner
        .act(GameAction::SelectCards {
            cards: vec![original_card],
        })
        .expect("choose the original ETB's card");
    runner.advance_until_stack_empty();

    assert_eq!(
        zone_of(&runner, original_card),
        Zone::Exile,
        "the original ETB resolved after the leaves trigger, so its card stays exiled"
    );
    assert_eq!(zone_of(&runner, copied_card), Zone::Hand);
}

#[test]
fn mesmeric_fiend_returns_exiled_card_to_hand_when_it_leaves_play() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let fiend = scenario
        .add_creature_to_hand_from_oracle(P0, "Mesmeric Fiend", 1, 1, MESMERIC_FIEND)
        .id();

    let target_card = scenario.add_card_to_hand(P1, "Counterspell");
    let destroy = scenario
        .add_spell_to_hand_from_oracle(P0, "Murder", true, MURDER)
        .id();

    let mut runner = scenario.build();

    let outcome = runner.cast(fiend).target_player(P1).resolve();

    let WaitingFor::RevealChoice { .. } = outcome.final_waiting_for() else {
        panic!(
            "expected RevealChoice prompt after ETB resolves, got {:?}",
            outcome.final_waiting_for()
        );
    };

    runner
        .act(GameAction::SelectCards {
            cards: vec![target_card],
        })
        .expect("SelectCards succeeds");

    assert_eq!(zone_of(&runner, target_card), Zone::Exile);

    runner.cast(destroy).target_object(fiend).resolve();
    runner.advance_until_stack_empty();

    assert_eq!(zone_of(&runner, fiend), Zone::Graveyard);
    assert_eq!(
        zone_of(&runner, target_card),
        Zone::Hand,
        "Target card must return to hand when Mesmeric Fiend leaves the battlefield"
    );
    assert!(runner.state().players[P1.0 as usize]
        .hand
        .contains(&target_card));
    assert!(!runner.state().exile.contains(&target_card));
    assert!(
        runner.state().exile_links.is_empty(),
        "ExileLink must be consumed"
    );
}

#[test]
fn tidehollow_sculler_with_no_nonland_cards_in_opponents_hand() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let sculler = scenario
        .add_creature_to_hand_from_oracle(P0, "Tidehollow Sculler", 2, 2, TIDEHOLLOW_SCULLER)
        .id();

    let land_card = scenario.add_land_to_hand(P1, "Plains").id();
    let destroy = scenario
        .add_spell_to_hand_from_oracle(P0, "Murder", true, MURDER)
        .id();

    let mut runner = scenario.build();

    let outcome = runner.cast(sculler).target_player(P1).resolve();

    // The ETB resolved (the reveal ran and no card was offered) rather than
    // never executing: Sculler is on the battlefield and the stack is empty.
    assert!(
        !matches!(outcome.final_waiting_for(), WaitingFor::RevealChoice { .. }),
        "no nonland card to choose, so no choice may be offered"
    );
    assert!(
        outcome.events().iter().any(|e| matches!(
            e,
            GameEvent::EffectResolved { source_id, .. }
                if *source_id == sculler
        )),
        "Sculler's ETB must have resolved"
    );
    assert_eq!(zone_of(&runner, land_card), Zone::Hand);
    assert!(runner.state().exile_links.is_empty());

    runner.cast(destroy).target_object(sculler).resolve();
    runner.advance_until_stack_empty();

    assert_eq!(zone_of(&runner, sculler), Zone::Graveyard);
    assert_eq!(zone_of(&runner, land_card), Zone::Hand);
    assert!(runner.state().exile_links.is_empty());
}

const ASHIOKS_ERASURE: &str = "Flash\n\
When this enchantment enters, exile target spell.\n\
Your opponents can't cast spells with the same name as the exiled card.\n\
When this enchantment leaves the battlefield, return the exiled card to its owner's hand.";

/// Pass priority until `done` holds, failing loudly instead of looping forever.
fn pass_until(runner: &mut GameRunner, what: &str, done: impl Fn(&GameRunner) -> bool) {
    for _ in 0..12 {
        if done(runner) {
            return;
        }
        runner
            .act(GameAction::PassPriority)
            .unwrap_or_else(|e| panic!("priority pass failed while waiting for {what}: {e:?}"));
    }
    assert!(done(runner), "never reached: {what}");
}

/// CR 603.3 + CR 607.2a: the printed leaves-the-battlefield ability is a real
/// triggered ability. While it waits on the stack (where it can be responded to
/// or countered), the card stays exiled; it returns only when the ability resolves.
#[test]
fn tidehollow_sculler_card_stays_exiled_until_leaves_trigger_resolves() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let sculler = scenario
        .add_creature_to_hand_from_oracle(P0, "Tidehollow Sculler", 2, 2, TIDEHOLLOW_SCULLER)
        .id();
    let target_card = scenario.add_card_to_hand(P1, "Doom Blade");
    let destroy = scenario
        .add_spell_to_hand_from_oracle(P0, "Murder", true, MURDER)
        .id();
    let mut runner = scenario.build();

    runner.cast(sculler).target_player(P1).resolve();
    runner
        .act(GameAction::SelectCards {
            cards: vec![target_card],
        })
        .expect("SelectCards succeeds");
    assert_eq!(zone_of(&runner, target_card), Zone::Exile);

    drop(runner.cast(destroy).target_object(sculler).commit());
    pass_until(
        &mut runner,
        "Sculler's leaves-the-battlefield trigger on the stack",
        |r| zone_of(r, sculler) == Zone::Graveyard,
    );

    assert_eq!(
        runner.state().stack.len(),
        1,
        "the leaves-the-battlefield trigger waits on the stack"
    );
    assert_eq!(
        zone_of(&runner, target_card),
        Zone::Exile,
        "the card must stay exiled while the LTB trigger is on the stack"
    );

    runner.resolve_top();
    assert_eq!(zone_of(&runner, target_card), Zone::Hand);
    assert!(runner.state().players[P1.0 as usize]
        .hand
        .contains(&target_card));
}

/// Official ruling (2021-03-19): "If Tidehollow Sculler leaves the battlefield
/// before its first ability has resolved, its second ability triggers. This
/// ability does nothing when it resolves. Then its first ability resolves and
/// exiles the chosen card indefinitely." (CR 607.2a: the return only refers to
/// cards already exiled; it is not a CR 610.3 duration, so CR 610.3b does not
/// stop the exile.)
#[test]
fn tidehollow_sculler_leaving_before_etb_resolves_exiles_indefinitely() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let sculler = scenario
        .add_creature_to_hand_from_oracle(P0, "Tidehollow Sculler", 2, 2, TIDEHOLLOW_SCULLER)
        .id();
    let target_card = scenario.add_card_to_hand(P1, "Doom Blade");
    let destroy = scenario
        .add_spell_to_hand_from_oracle(P0, "Murder", true, MURDER)
        .id();
    let mut runner = scenario.build();

    drop(runner.cast(sculler).target_player(P1).commit());
    pass_until(&mut runner, "Sculler's ETB trigger on the stack", |r| {
        zone_of(r, sculler) == Zone::Battlefield && !r.state().stack.is_empty()
    });

    // Respond to the ETB trigger by destroying Sculler.
    drop(runner.cast(destroy).target_object(sculler).commit());
    pass_until(
        &mut runner,
        "both Sculler triggers with its leaves trigger above its ETB trigger",
        |r| {
            zone_of(r, sculler) == Zone::Graveyard
                && r.state().stack.len() == 2
                && r.state().stack[0].source_id == sculler
                && r.state().stack[1].source_id == sculler
                && r.state().stack[1].ability().is_some_and(|ability| {
                    matches!(
                        ability.effect,
                        engine::types::ability::Effect::ChangeZoneAll { .. }
                    )
                })
        },
    );
    // Reach guard: the leaves trigger really fired and sits above the ETB trigger.
    assert_eq!(
        runner
            .state()
            .stack
            .iter()
            .filter(|entry| entry.source_id == sculler)
            .count(),
        2,
        "both Sculler triggers are on the stack"
    );
    // Drive priority by hand so the leaves trigger's resolution is observed.
    let mut events = Vec::new();
    for _ in 0..12 {
        if matches!(runner.state().waiting_for, WaitingFor::RevealChoice { .. }) {
            break;
        }
        events.extend(
            runner
                .act(GameAction::PassPriority)
                .expect("pass priority until the exile choice")
                .events,
        );
    }
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::RevealChoice { .. }),
        "never reached the exile choice"
    );
    assert!(
        events.iter().any(|e| matches!(
            e,
            GameEvent::EffectResolved {
                kind: engine::types::ability::EffectKind::ChangeZoneAll,
                source_id,
                ..
            } if *source_id == sculler
        )),
        "Sculler's leaves trigger must have resolved before the ETB asks for its card"
    );
    assert_eq!(zone_of(&runner, sculler), Zone::Graveyard);

    runner
        .act(GameAction::SelectCards {
            cards: vec![target_card],
        })
        .expect("SelectCards succeeds");
    runner.advance_until_stack_empty();

    assert_eq!(
        zone_of(&runner, target_card),
        Zone::Exile,
        "the chosen card is exiled indefinitely; the earlier leaves trigger did nothing"
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "stack resolution must complete back to priority"
    );
}

/// CR 607.2a + CR 603.3: Ashiok's Erasure exiles a SPELL (from the stack) and its
/// printed leaves-the-battlefield ability returns that card to its owner's hand —
/// not to the zone it was exiled from.
#[test]
fn ashioks_erasure_returns_exiled_spell_to_owners_hand() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let erasure = {
        let mut builder =
            scenario.add_spell_to_hand_from_oracle(P0, "Ashiok's Erasure", false, ASHIOKS_ERASURE);
        builder.as_enchantment();
        builder.id()
    };
    let victim = scenario
        .add_spell_to_hand_from_oracle(P0, "Victim Spell", false, "You gain 3 life.")
        .id();
    let destroy = scenario
        .add_spell_to_hand_from_oracle(P0, "Disenchant", true, "Destroy target enchantment.")
        .id();
    let mut runner = scenario.build();

    let mut victim_cast = runner.cast(victim).commit();
    victim_cast.cast(erasure).target_object(victim).resolve();
    drop(victim_cast);
    assert_eq!(zone_of(&runner, erasure), Zone::Battlefield);
    assert_eq!(zone_of(&runner, victim), Zone::Exile);

    runner.cast(destroy).target_object(erasure).resolve();
    runner.advance_until_stack_empty();

    assert_eq!(zone_of(&runner, erasure), Zone::Graveyard);
    assert_eq!(
        zone_of(&runner, victim),
        Zone::Hand,
        "the exiled spell returns to its owner's hand, not to the stack"
    );
}

const UGIN_THE_INEFFABLE: &str = "Colorless spells you cast cost {2} less to cast.\n\
+1: Exile the top card of your library face down and look at it. Create a 2/2 colorless Spirit creature token. When that token leaves the battlefield, put the exiled card into your hand.\n\
\u{2212}3: Destroy target permanent that's one or more colors.";

/// CR 607.1 + CR 607.2a + CR 603.7: Ugin, the Ineffable's +1 ability binds each
/// exiled card to its own created Spirit token via a tracked-set delayed trigger.
/// When activated multiple times, removing one Spirit returns only that Spirit's
/// exiled card to hand while the other remains exiled.
#[test]
fn ugin_the_ineffable_delayed_return_returns_only_matching_spirits_exiled_card() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let ugin = scenario
        .add_planeswalker_from_oracle(P0, "Ugin, the Ineffable", "Ugin", 4, UGIN_THE_INEFFABLE)
        .as_legendary()
        .id();

    // Top of library: card_b is below card_a (so card_a is on top).
    let card_b = scenario.add_card_to_library_top(P0, "Exiled Card B");
    let card_a = scenario.add_card_to_library_top(P0, "Exiled Card A");

    let murder1 = scenario
        .add_spell_to_hand_from_oracle(P0, "Murder 1", true, MURDER)
        .id();
    let murder2 = scenario
        .add_spell_to_hand_from_oracle(P0, "Murder 2", true, MURDER)
        .id();

    let mut runner = scenario.build();

    // Grant an extra loyalty activation so we can activate Ugin's +1 twice in the same turn.
    runner
        .state_mut()
        .extra_loyalty_activations_this_turn
        .insert(P0, 1);

    // First activation: exiles card_a and creates the first Spirit token.
    runner.activate(ugin, 0).resolve();
    runner.advance_until_stack_empty();

    assert_eq!(zone_of(&runner, card_a), Zone::Exile);
    assert_eq!(zone_of(&runner, card_b), Zone::Library);

    let spirits_first: Vec<ObjectId> = runner
        .state()
        .objects
        .values()
        .filter(|obj| {
            obj.zone == Zone::Battlefield
                && obj.controller == P0
                && obj.card_types.subtypes.iter().any(|s| s == "Spirit")
        })
        .map(|obj| obj.id)
        .collect();
    assert_eq!(
        spirits_first.len(),
        1,
        "first activation creates one Spirit"
    );
    let spirit_a = spirits_first[0];

    // Second activation: exiles card_b and creates the second Spirit token.
    runner.activate(ugin, 0).resolve();
    runner.advance_until_stack_empty();

    assert_eq!(zone_of(&runner, card_a), Zone::Exile);
    assert_eq!(zone_of(&runner, card_b), Zone::Exile);

    let spirits_second: Vec<ObjectId> = runner
        .state()
        .objects
        .values()
        .filter(|obj| {
            obj.zone == Zone::Battlefield
                && obj.controller == P0
                && obj.card_types.subtypes.iter().any(|s| s == "Spirit")
        })
        .map(|obj| obj.id)
        .collect();
    assert_eq!(
        spirits_second.len(),
        2,
        "second activation creates a second Spirit"
    );
    let spirit_b = spirits_second
        .into_iter()
        .find(|&id| id != spirit_a)
        .expect("second spirit found");

    // Both cards are currently in exile.
    assert_eq!(zone_of(&runner, card_a), Zone::Exile);
    assert_eq!(zone_of(&runner, card_b), Zone::Exile);

    // Destroy the SECOND Spirit first: a FIFO association would wrongly return card_a.
    runner.cast(murder1).target_object(spirit_b).resolve();
    runner.advance_until_stack_empty();

    // CR 607.1: Only spirit_b's exiled card (card_b) returns to hand.
    assert_eq!(
        zone_of(&runner, card_b),
        Zone::Hand,
        "second Spirit's death returns its linked exiled card to hand"
    );
    assert_eq!(
        zone_of(&runner, card_a),
        Zone::Exile,
        "first Spirit's exiled card must remain in exile"
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "resolution completes back to priority"
    );

    runner.cast(murder2).target_object(spirit_a).resolve();
    runner.advance_until_stack_empty();

    assert_eq!(
        zone_of(&runner, card_a),
        Zone::Hand,
        "first Spirit's death returns its linked exiled card to hand"
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "resolution completes back to priority"
    );
}

/// CR 603.7c + CR 400.7: "if that object left that zone and then returned, it's a
/// new object and thus won't be affected." Once Ugin's exiled card has gone to
/// the graveyard and been exiled again, the Spirit's death must not retrieve it.
#[test]
fn ugin_the_ineffable_return_ignores_card_exiled_again_as_new_object() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let ugin = scenario
        .add_planeswalker_from_oracle(P0, "Ugin, the Ineffable", "Ugin", 4, UGIN_THE_INEFFABLE)
        .as_legendary()
        .id();
    let card = scenario.add_card_to_library_top(P0, "Exiled Card");
    let murder = scenario
        .add_spell_to_hand_from_oracle(P0, "Murder", true, MURDER)
        .id();
    let mut runner = scenario.build();

    runner.activate(ugin, 0).resolve();
    runner.advance_until_stack_empty();
    assert_eq!(zone_of(&runner, card), Zone::Exile);
    let spirit = runner
        .state()
        .objects
        .values()
        .find(|obj| {
            obj.zone == Zone::Battlefield
                && obj.controller == P0
                && obj.card_types.subtypes.iter().any(|s| s == "Spirit")
        })
        .map(|obj| obj.id)
        .expect("Ugin creates a Spirit");

    // The exiled card is processed into the graveyard and exiled again: a new object.
    let mut events = Vec::new();
    engine::game::zones::move_to_zone(runner.state_mut(), card, Zone::Graveyard, &mut events);
    engine::game::zones::move_to_zone(runner.state_mut(), card, Zone::Exile, &mut events);
    assert_eq!(zone_of(&runner, card), Zone::Exile);

    // Drive the stack by hand so every event is observed, including those from the
    // delayed return that fires after Murder resolves.
    drop(runner.cast(murder).target_object(spirit).commit());
    let mut events = Vec::new();
    for _ in 0..12 {
        // A token ceases to exist after leaving the battlefield (CR 111.7).
        let spirit_on_battlefield = runner
            .state()
            .objects
            .get(&spirit)
            .is_some_and(|obj| obj.zone == Zone::Battlefield);
        if !spirit_on_battlefield && runner.state().stack.is_empty() {
            break;
        }
        events.extend(
            runner
                .act(GameAction::PassPriority)
                .expect("pass priority while Murder and the delayed return resolve")
                .events,
        );
    }

    // Reach guards: the Spirit left, and its delayed return resolved its
    // ChangeZoneAll (so the empty-handed outcome is not a never-fired trigger).
    assert!(
        events.iter().any(|e| matches!(
            e,
            GameEvent::ZoneChanged { object_id, from: Some(Zone::Battlefield), .. }
                if *object_id == spirit
        )),
        "the Spirit left the battlefield, so its delayed trigger was offered the event"
    );
    assert!(
        events.iter().any(|e| matches!(
            e,
            GameEvent::EffectResolved {
                kind: engine::types::ability::EffectKind::ChangeZoneAll,
                ..
            }
        )),
        "the Spirit's delayed return must resolve its ChangeZoneAll"
    );
    assert!(runner.state().stack.is_empty());
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "resolution completes back to priority"
    );
    assert_eq!(
        zone_of(&runner, card),
        Zone::Exile,
        "a card that left exile and returned is a new object and is not retrieved"
    );
}

const UGINS_LABYRINTH: &str = "Imprint — When this land enters, you may exile a colorless card with mana value 7 or greater from your hand.\n\
{T}: Add {C}. If a card is exiled with this land, add {C}{C} instead.\n\
{T}: Return the exiled card to its owner's hand.";

/// CR 607.2a + CR 406.6: an ACTIVATED linked ability ("{T}: Return the exiled card to
/// its owner's hand") returns the card exiled with its source.
#[test]
fn ugins_labyrinth_activated_return_puts_imprinted_card_in_owners_hand() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let labyrinth = scenario
        .add_land_from_oracle(P0, "Ugin's Labyrinth", UGINS_LABYRINTH)
        .id();
    let imprinted = scenario.add_card_to_hand(P0, "Colorless Seven-Drop");
    let mut runner = scenario.build();

    let mut events = Vec::new();
    engine::game::zones::move_to_zone(runner.state_mut(), imprinted, Zone::Exile, &mut events);
    runner
        .state_mut()
        .exile_links
        .push(engine::types::game_state::ExileLink {
            exiled_id: imprinted,
            source_id: labyrinth,
            kind: ExileLinkKind::TrackedBySource,
        });
    assert_eq!(zone_of(&runner, imprinted), Zone::Exile);

    runner.activate(labyrinth, 1).resolve();
    runner.advance_until_stack_empty();

    assert_eq!(
        zone_of(&runner, imprinted),
        Zone::Hand,
        "the exiled card returns to its owner's hand"
    );
}

const CHAMPIONS_OF_THE_SHOAL: &str = "As an additional cost to cast this spell, behold a Merfolk and exile it. (Exile a Merfolk you control or a Merfolk card from your hand.)\n\
Whenever this creature enters or becomes tapped, tap up to one target creature and put a stun counter on it.\n\
When this creature leaves the battlefield, return the exiled card to its owner's hand.";

/// CR 118.8 + CR 607.2q + CR 603.3: the card exiled as part of Champions of the Shoal's
/// additional casting cost is linked to it and returns when it leaves the battlefield.
#[test]
fn champions_of_the_shoal_returns_cost_exiled_merfolk_when_it_leaves() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let champions = scenario
        .add_creature_to_hand_from_oracle(
            P0,
            "Champions of the Shoal",
            4,
            4,
            CHAMPIONS_OF_THE_SHOAL,
        )
        .id();
    let merfolk = {
        let mut builder = scenario.add_creature_to_hand(P0, "Merfolk Fixture", 1, 1);
        builder.with_subtypes(vec!["Merfolk"]);
        builder.id()
    };
    let destroy = scenario
        .add_spell_to_hand_from_oracle(P0, "Murder", true, MURDER)
        .id();
    scenario.with_mana_pool(
        P0,
        (0..8)
            .map(|_| ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]))
            .collect(),
    );
    let mut runner = scenario.build();

    let card_id = runner.state().objects[&champions].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: champions,
            card_id,
            targets: vec![],
            payment_mode: engine::types::game_state::CastPaymentMode::Auto,
        })
        .expect("casting Champions of the Shoal is accepted");
    for _ in 0..16 {
        match runner.state().waiting_for.clone() {
            WaitingFor::PayCost { .. } => {
                runner
                    .act(GameAction::SelectCards {
                        cards: vec![merfolk],
                    })
                    .expect("behold the Merfolk card");
            }
            WaitingFor::TriggerTargetSelection { .. } => {
                runner
                    .act(GameAction::SelectTargets { targets: vec![] })
                    .expect("decline the optional tap target");
            }
            WaitingFor::Priority { .. }
                if zone_of(&runner, champions) == Zone::Battlefield
                    && runner.state().stack.is_empty() =>
            {
                break;
            }
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            other => panic!("unexpected prompt while casting Champions: {other:?}"),
        }
    }
    assert_eq!(zone_of(&runner, champions), Zone::Battlefield);
    assert_eq!(zone_of(&runner, merfolk), Zone::Exile, "cost exile");
    assert!(runner.state().exile_links.iter().any(|link| {
        link.exiled_id == merfolk
            && link.source_id == champions
            && link.kind == ExileLinkKind::CastCost
    }));

    runner.cast(destroy).target_object(champions).resolve();
    runner.advance_until_stack_empty();

    assert_eq!(zone_of(&runner, champions), Zone::Graveyard);
    assert_eq!(
        zone_of(&runner, merfolk),
        Zone::Hand,
        "the cost-exiled Merfolk returns to its owner's hand"
    );
}
