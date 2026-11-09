//! Issue #9213 — Curse of Hospitality.
//!
//! Oracle:
//!   Enchant player
//!   Creatures attacking enchanted player have trample.
//!   Whenever a creature deals combat damage to enchanted player, that player
//!   exiles the top card of their library. Until end of turn, that creature's
//!   controller may play that card and they may spend mana as though it were
//!   mana of any color to cast that spell.

use engine::ai_support::legal_actions;
use engine::analysis::resource::loop_states_equal_modulo_resources;
use engine::game::effects::attach::attach_to_player;
use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::trigger_index::reindex_object_triggers;
use engine::types::ability::Duration;
use engine::types::ability::{CastingPermission, ManaSpendPermission};
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::{CastPaymentMode, StackEntryKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

use super::rules::AttackTarget;

const P2: PlayerId = PlayerId(2);

const CURSE: &str = "Enchant player\n\
Creatures attacking enchanted player have trample.\n\
Whenever a creature deals combat damage to enchanted player, that player exiles the top card \
of their library. Until end of turn, that creature's controller may play that card and they \
may spend mana as though it were mana of any color to cast that spell.";

struct Board {
    runner: GameRunner,
    attacker: ObjectId,
    top_card: ObjectId,
}

/// 3 players: P0 controls the Curse enchanting P1; `attacker_controller`
/// attacks P1 with a 2/2. P1's library top is a known card.
fn board(curse_text: &str, attacker_controller: PlayerId) -> Board {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let curse = {
        let mut builder = scenario.add_creature(P0, "Curse of Hospitality", 0, 0);
        builder.as_enchantment();
        builder.with_subtypes(vec!["Aura", "Curse"]);
        builder.from_oracle_text(curse_text);
        builder.id()
    };
    let attacker = scenario
        .add_creature(attacker_controller, "Grizzly Bears", 2, 2)
        .id();
    // A {R} instant, so off-color mana shows the any-color rider.
    let top_card = scenario
        .add_spell_to_library_top(P1, "Red Instant", true)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Red],
            generic: 0,
        })
        .from_oracle_text("You gain 1 life.")
        .id();
    // P0's library top, to show an attack on P0 exiles nothing.
    scenario.add_card_to_library_top(P0, "Island");
    // P2's only lands are Swamps.
    for _ in 0..2 {
        scenario.add_basic_land(P2, ManaColor::Black);
    }
    let mut runner = scenario.build();
    attach_to_player(runner.state_mut(), curse, P1);
    evaluate_layers(runner.state_mut());
    reindex_object_triggers(runner.state_mut(), curse);
    Board {
        runner,
        attacker,
        top_card,
    }
}

fn attack_and_resolve(b: &mut Board, attacker_controller: PlayerId) {
    attack(&mut b.runner, b.attacker, attacker_controller, P1);
    assert_eq!(
        b.runner.state().objects[&b.top_card].zone,
        Zone::Exile,
        "the Curse exiled the enchanted player's top card"
    );
}

/// `attacker_controller` attacks `defender` with `attacker`; play through
/// combat to the postcombat main phase, answering trigger order with
/// `order_triggers`. At each priority, `respond` may act instead of passing
/// (it returns whether it did).
fn attack_with(
    runner: &mut GameRunner,
    attacker: ObjectId,
    attacker_controller: PlayerId,
    defender: PlayerId,
    mut order_triggers: impl FnMut(&GameRunner) -> Vec<usize>,
    mut respond: impl FnMut(&mut GameRunner) -> bool,
) {
    runner.state_mut().active_player = attacker_controller;
    runner.state_mut().priority_player = attacker_controller;
    runner.state_mut().waiting_for = WaitingFor::Priority {
        player: attacker_controller,
    };
    for _ in 0..16 {
        if runner.waiting_for_kind() == "DeclareAttackers" {
            break;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("pass to combat");
    }
    runner
        .declare_attackers(&[(attacker, AttackTarget::Player(defender))])
        .expect("declare the attack");
    for _ in 0..64 {
        if runner.state().phase == Phase::PostCombatMain && runner.state().stack.is_empty() {
            return;
        }
        if matches!(runner.state().waiting_for, WaitingFor::Priority { .. }) && respond(runner) {
            continue;
        }
        let action = match &runner.state().waiting_for {
            WaitingFor::Priority { .. } => GameAction::PassPriority,
            WaitingFor::DeclareBlockers { .. } => GameAction::DeclareBlockers {
                assignments: vec![],
            },
            WaitingFor::OrderTriggers { .. } => GameAction::OrderTriggers {
                order: order_triggers(runner),
            },
            other => panic!("unexpected prompt {other:?}"),
        };
        runner.act(action).expect("combat step");
    }
    panic!("combat never reached the postcombat main phase");
}

fn attack(
    runner: &mut GameRunner,
    attacker: ObjectId,
    attacker_controller: PlayerId,
    defender: PlayerId,
) {
    attack_with(
        runner,
        attacker,
        attacker_controller,
        defender,
        |_| panic!("no trigger order expected"),
        |_| false,
    );
}

fn grants(
    runner: &GameRunner,
    card: ObjectId,
) -> Vec<(PlayerId, Duration, Option<ManaSpendPermission>)> {
    runner.state().objects[&card]
        .casting_permissions
        .iter()
        .filter_map(|p| match p {
            CastingPermission::PlayFromExile {
                granted_to,
                duration,
                mana_spend_permission,
                ..
            } => Some((*granted_to, duration.clone(), *mana_spend_permission)),
            _ => None,
        })
        .collect()
}

/// CR 603.2 + CR 109.4: "that creature's controller" is the controller of the
/// creature that dealt the damage — here P2, neither the Curse's controller
/// (P0) nor the enchanted player (P1). The grant lasts until end of turn and
/// carries the any-color rider (CR 609.4b).
#[test]
fn the_attacking_creatures_controller_may_play_the_card_with_any_color() {
    let mut b = board(CURSE, P2);
    attack_and_resolve(&mut b, P2);
    assert_eq!(
        grants(&b.runner, b.top_card),
        vec![(
            P2,
            Duration::UntilEndOfTurn,
            Some(ManaSpendPermission::AnyColor)
        )]
    );
}

/// The same trigger with the Curse's controller attacking grants that player.
#[test]
fn the_curse_controller_attacking_gets_the_grant() {
    let mut b = board(CURSE, P0);
    attack_and_resolve(&mut b, P0);
    assert_eq!(
        grants(&b.runner, b.top_card),
        vec![(
            P0,
            Duration::UntilEndOfTurn,
            Some(ManaSpendPermission::AnyColor)
        )]
    );
}

/// End to end: P2, holding only Swamps, casts the exiled {R} instant through
/// the grant after combat — the grant reaches the real cast path and the
/// any-color rider pays the red symbol (CR 609.4b).
#[test]
fn the_grantee_casts_the_exiled_card_with_off_color_mana() {
    let mut b = board(CURSE, P2);
    attack_and_resolve(&mut b, P2);
    let runner = &mut b.runner;
    runner.advance_to_phase(Phase::PostCombatMain);
    runner.state_mut().priority_player = P2;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P2 };
    assert!(
        legal_actions(runner.state()).iter().any(
            |a| matches!(a, GameAction::CastSpell { object_id, .. } if *object_id == b.top_card)
        ),
        "P2 is offered the exiled card"
    );
    let life = runner.state().players[2].life;
    let card_id = runner.state().objects[&b.top_card].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: b.top_card,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("P2 casts the exiled card with a Swamp");
    runner.advance_until_stack_empty();
    assert_eq!(runner.state().players[2].life, life + 1);
    assert_eq!(runner.state().objects[&b.top_card].zone, Zone::Graveyard);
}

/// CR 303.4 + CR 603.2: the trigger watches damage to the ENCHANTED player
/// only. P2 hits P0, who is not enchanted: nothing is exiled and nobody gets a
/// grant. (The tests above are the positive partner: the same board, P1 hit.)
#[test]
fn combat_damage_to_another_player_does_not_trigger() {
    let mut b = board(CURSE, P2);
    let p0_top = b.runner.state().players[0].library.front().copied();
    assert!(p0_top.is_some());
    attack(&mut b.runner, b.attacker, P2, P0);
    assert_eq!(
        b.runner.state().players[0].life,
        20 - 2,
        "the attack connected"
    );
    assert_eq!(b.runner.state().objects[&b.top_card].zone, Zone::Library);
    assert_eq!(b.runner.state().players[0].library.front().copied(), p0_top);
    assert!(b
        .runner
        .state()
        .objects
        .values()
        .all(|o| o.casting_permissions.is_empty()));
}

/// CR 608.2h + CR 109.4: a stolen creature that dies before the Curse's trigger
/// resolves still names its last controller, not its owner. P2's Curse on P1;
/// P2 attacks P1 with P1's creature (stolen with a control Aura). P2 orders the
/// creature's own "sacrifice it" trigger to resolve first, so the creature is in
/// P1's graveyard — its row's controller reset to P1 — when the Curse grants.
/// P2 is both the Curse's controller and the creature's last controller, so
/// this test separates the last-known read from the owner, not the grantee
/// from the ability's controller; `the_attacking_creatures_controller_may_play_the_card_with_any_color` does.
#[test]
fn a_stolen_creature_that_died_names_its_last_controller() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let curse = {
        let mut builder = scenario.add_creature(P2, "Curse of Hospitality", 0, 0);
        builder.as_enchantment();
        builder.with_subtypes(vec!["Aura", "Curse"]);
        builder.from_oracle_text(CURSE);
        builder.id()
    };
    let raider = scenario
        .add_creature_from_oracle(
            P1,
            "Doomed Raider",
            2,
            2,
            "Haste\nWhenever this creature deals combat damage to a player, sacrifice it.",
        )
        .id();
    let control = scenario
        .add_enchantment_from_oracle(
            P2,
            "Control Magic",
            "Enchant creature\nYou control enchanted creature.",
        )
        .with_subtypes(vec!["Aura"])
        .id();
    let top_card = scenario.add_card_to_library_top(P1, "Hill Giant");
    let mut runner = scenario.build();
    attach_to_player(runner.state_mut(), curse, P1);
    engine::game::effects::attach::attach_to(runner.state_mut(), control, raider);
    evaluate_layers(runner.state_mut());
    reindex_object_triggers(runner.state_mut(), curse);
    assert_eq!(runner.state().objects[&raider].controller, P2);

    attack_with(
        &mut runner,
        raider,
        P2,
        P1,
        |runner| {
            // Index 0 is placed first (bottom): the Curse resolves after the
            // sacrifice.
            let WaitingFor::OrderTriggers { triggers, .. } = &runner.state().waiting_for else {
                unreachable!()
            };
            let curse_at = triggers
                .iter()
                .position(|t| t.source_id == curse)
                .expect("the Curse triggered");
            let mut order: Vec<usize> = (0..triggers.len()).filter(|&i| i != curse_at).collect();
            order.insert(0, curse_at);
            order
        },
        |_| false,
    );

    assert_eq!(runner.state().objects[&raider].zone, Zone::Graveyard);
    assert_eq!(
        runner.state().objects[&raider].controller,
        P1,
        "off the battlefield the row names the owner"
    );
    assert_eq!(runner.state().objects[&top_card].zone, Zone::Exile);
    assert_eq!(
        grants(&runner, top_card),
        vec![(
            P2,
            Duration::UntilEndOfTurn,
            Some(ManaSpendPermission::AnyColor)
        )]
    );
}

/// Three players: P0's Curse on P1; P1's creature, stolen by P2's control Aura,
/// attacks P1. Returns the runner, the creature, the Aura, P1's top card and
/// response instants in P2's hand, with Plains available to pay for them.
fn stolen_attacker_board(
    response_name: &str,
    response_text: &str,
    response_cost: ManaCost,
    response_copies: usize,
) -> (GameRunner, ObjectId, ObjectId, ObjectId, Vec<ObjectId>) {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let curse = {
        let mut builder = scenario.add_creature(P0, "Curse of Hospitality", 0, 0);
        builder.as_enchantment();
        builder.with_subtypes(vec!["Aura", "Curse"]);
        builder.from_oracle_text(CURSE);
        builder.id()
    };
    let raider = scenario
        .add_creature_from_oracle(P1, "Stolen Raider", 2, 2, "Haste")
        .id();
    let control = scenario
        .add_enchantment_from_oracle(
            P2,
            "Control Magic",
            "Enchant creature\nYou control enchanted creature.",
        )
        .with_subtypes(vec!["Aura"])
        .id();
    let responses = (0..response_copies)
        .map(|_| {
            scenario
                .add_spell_to_hand_from_oracle(P2, response_name, true, response_text)
                .with_mana_cost(response_cost.clone())
                .id()
        })
        .collect();
    for _ in 0..2 * response_copies {
        scenario.add_basic_land(P2, ManaColor::White);
    }
    let top_card = scenario.add_card_to_library_top(P1, "Hill Giant");
    let mut runner = scenario.build();
    attach_to_player(runner.state_mut(), curse, P1);
    engine::game::effects::attach::attach_to(runner.state_mut(), control, raider);
    evaluate_layers(runner.state_mut());
    reindex_object_triggers(runner.state_mut(), curse);
    assert_eq!(runner.state().objects[&raider].controller, P2);
    (runner, raider, control, top_card, responses)
}

/// With the Curse's trigger on the stack, P2 casts `response` targeting
/// `target`, once.
fn respond_once(
    responder: PlayerId,
    response: ObjectId,
    target: ObjectId,
    curse_pending: impl Fn(&GameRunner) -> bool,
) -> impl FnMut(&mut GameRunner) -> bool {
    let mut done = false;
    move |runner: &mut GameRunner| {
        if done || !curse_pending(runner) {
            return false;
        }
        if !matches!(runner.state().waiting_for, WaitingFor::Priority { player } if player == responder)
        {
            return false;
        }
        done = true;
        let card_id = runner.state().objects[&response].card_id;
        runner
            .act(GameAction::CastSpell {
                object_id: response,
                card_id,
                targets: vec![],
                payment_mode: CastPaymentMode::Auto,
            })
            .expect("P2 casts the response");
        if let WaitingFor::TargetSelection { player, .. } = runner.state().waiting_for {
            assert_eq!(player, responder);
            runner
                .act(GameAction::ChooseTarget {
                    target: Some(engine::types::ability::TargetRef::Object(target)),
                })
                .expect("select the response target");
        } else {
            // A sole legal target is selected by the casting pipeline itself.
            assert!(matches!(
                runner.state().waiting_for,
                WaitingFor::Priority { .. }
            ));
            let spell = runner
                .state()
                .stack
                .iter()
                .find(|entry| entry.source_id == response)
                .expect("the auto-targeted response is on the stack");
            assert_eq!(
                spell
                    .ability()
                    .expect("the response has an ability")
                    .targets,
                vec![engine::types::ability::TargetRef::Object(target)],
                "automatic selection must choose the same required target"
            );
        }
        true
    }
}

fn curse_trigger_pending(curse_source: ObjectId) -> impl Fn(&GameRunner) -> bool {
    move |runner: &GameRunner| {
        runner
            .state()
            .stack
            .iter()
            .any(|e| e.source_id == curse_source)
    }
}

/// CR 400.7 + CR 608.2h: blinked with the trigger pending, the creature returns
/// under its owner as a NEW object; "that creature's controller" is still the
/// controller of the object that dealt the damage — P2, not P1.
#[test]
fn a_blinked_damage_dealer_names_the_controller_of_the_object_that_dealt_it() {
    let (mut runner, raider, control, top_card, responses) = stolen_attacker_board(
        "Flicker of Fate",
        "Exile target creature or enchantment, then return it to the battlefield under its owner's control.",
        ManaCost::Cost {
            shards: vec![ManaCostShard::White],
            generic: 1,
        },
        2,
    );
    let curse = curse_source(&runner);
    let mut original_stamp = None;
    let mut pending_after_blink = None;
    let mut blink = respond_once(P2, responses[0], raider, curse_trigger_pending(curse));
    let mut blink_again = respond_once(P2, responses[1], raider, curse_trigger_pending(curse));
    attack_with(
        &mut runner,
        raider,
        P2,
        P1,
        |_| panic!("no trigger order expected"),
        |runner| {
            let Some(entry) = runner.state().stack.iter().find(|e| e.source_id == curse) else {
                return false;
            };
            // CR 603.3a: the real pending Curse trigger belongs to P0, not the
            // damage dealer's controller; its event selects the original object.
            assert_eq!(entry.controller, P0);
            let StackEntryKind::TriggeredAbility {
                trigger_event:
                    Some(GameEvent::DamageDealt {
                        source_id,
                        source_incarnation: Some(stamp),
                        target,
                        amount: 2,
                        is_combat: true,
                        ..
                    }),
                ..
            } = &entry.kind
            else {
                panic!("the Curse must carry its stamped combat-damage event");
            };
            assert_eq!(*source_id, raider);
            assert_eq!(*target, engine::types::ability::TargetRef::Player(P1));
            let stamp = *stamp;
            if let Some(original) = original_stamp {
                assert_eq!(stamp, original, "the pending event keeps its original pin");
            } else {
                assert_eq!(runner.state().objects[&raider].incarnation, stamp);
                original_stamp = Some(stamp);
            }
            if blink(runner) {
                return true;
            }
            // A second real blink supersedes Flicker's tracked-set origin. Otherwise
            // that independent origin retains the first snapshot and masks the bug.
            if runner.state().objects[&responses[0]].zone == Zone::Graveyard && blink_again(runner)
            {
                return true;
            }
            if pending_after_blink.is_none()
                && runner.state().objects[&responses[1]].zone == Zone::Graveyard
            {
                let state = runner.state();
                assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
                assert_eq!(state.stack.len(), 1, "only the real Curse remains pending");
                assert_eq!(state.players[1].life, 18, "the attack connected");
                assert_eq!(state.objects[&top_card].zone, Zone::Library);
                assert!(grants(runner, top_card).is_empty());
                // CR 400.7: Flicker returned a new object under its owner.
                assert_eq!(state.objects[&raider].zone, Zone::Battlefield);
                assert_eq!(state.objects[&raider].controller, P1);
                assert_ne!(state.objects[&raider].incarnation, stamp);
                // CR 608.2h: the original exit snapshot, not Flicker's later
                // exile-exit snapshot or the returned permanent, answers.
                assert_eq!(state.lki_cache[&raider].controller, P1);
                assert_eq!(state.lki_by_incarnation[&raider][&stamp].controller, P2);
                let attachment = state.lki_by_incarnation[&raider][&stamp]
                    .attachments
                    .iter()
                    .find(|attachment| attachment.object_id == control)
                    .expect("the first departure snapshots the attached Control Magic");
                assert_eq!(
                    attachment
                        .identity
                        .expect("the departed Control Magic attachment has an exact pin")
                        .object_id,
                    control
                );
                assert!(
                    state.lki_cache[&raider].attachments.is_empty(),
                    "the later bare-id snapshot has no attachment to supply the nested pin"
                );
                let active_set = state
                    .chain_tracked_set_id
                    .expect("the second Flicker's set");
                let latest_nonempty_set = state
                    .tracked_object_sets
                    .iter()
                    .filter(|(_, objects)| !objects.is_empty())
                    .max_by_key(|(id, _)| id.0)
                    .map(|(&id, _)| id);
                assert_eq!(
                    latest_nonempty_set,
                    Some(active_set),
                    "normalization's fallback selects the second Flicker's active set"
                );
                assert_ne!(
                    state.tracked_set_member_origins[&active_set][&raider].departure_incarnation,
                    stamp,
                    "the active tracked set no longer independently retains the damage snapshot"
                );
                pending_after_blink = Some(runner.state().clone());
            }
            false
        },
    );
    assert_eq!(runner.state().objects[&raider].zone, Zone::Battlefield);
    assert_eq!(
        runner.state().objects[&raider].controller,
        P1,
        "the returned creature is a new object under its owner"
    );
    assert_eq!(runner.state().objects[&top_card].zone, Zone::Exile);
    assert_eq!(
        grants(&runner, top_card),
        vec![(
            P2,
            Duration::UntilEndOfTurn,
            Some(ManaSpendPermission::AnyColor)
        )]
    );
    assert!(runner.state().stack.is_empty());
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));

    let pending =
        pending_after_blink.expect("capture Priority after Flicker, before Curse resolves");
    let stamp = original_stamp.expect("combat emitted a stamped damage event");
    let mut same_controller = pending.clone();
    same_controller
        .lki_by_incarnation
        .get_mut(&raider)
        .unwrap()
        .get_mut(&stamp)
        .unwrap()
        .controller = P2;
    assert!(
        loop_states_equal_modulo_resources(&pending, &same_controller),
        "control: an identical selected snapshot is the same position"
    );

    // Controlled mutation of ONE selected historical field, not a claim of an
    // independently played history. Resolve this unnormalized clone through
    // public actions to demonstrate the different future grantee it selects.
    let mut alternate_controller = pending.clone();
    alternate_controller
        .lki_by_incarnation
        .get_mut(&raider)
        .unwrap()
        .get_mut(&stamp)
        .unwrap()
        .controller = P0;
    let mut alternate = GameRunner::from_state(alternate_controller.clone());
    alternate.advance_until_stack_empty();
    assert_eq!(alternate.state().objects[&top_card].zone, Zone::Exile);
    // CR 608.2h + CR 611.2a + CR 609.4b: only the selected historical
    // controller changes; the printed grant keeps its duration and rider.
    assert_eq!(
        grants(&alternate, top_card),
        vec![(
            P0,
            Duration::UntilEndOfTurn,
            Some(ManaSpendPermission::AnyColor)
        )]
    );
    assert!(alternate.state().stack.is_empty());
    assert!(matches!(
        alternate.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    // CR 104.4b: positions granting different players permission cannot be
    // identified merely because normalization pruned the selected snapshot.
    assert!(
        !loop_states_equal_modulo_resources(&pending, &alternate_controller),
        "different selected damage-source controllers have different futures"
    );
}

/// CR 605.4a + CR 400.7: Kinnan's immediate mana receipt names the
/// intermediate Kinnan, not the Elves or either of Kinnan's other incarnations.
/// That projected-away history must not change the retained Curse binding.
#[test]
fn loop_relational_kinnan_mana_receipt_does_not_rerank_a_pending_curse_damage_source() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let curse = scenario
        .add_enchantment_from_oracle(P0, "Curse of Hospitality", CURSE)
        .with_subtypes(vec!["Aura", "Curse"])
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Red],
            generic: 2,
        })
        .id();
    let kinnan = scenario
        .add_creature_from_oracle(
            P1,
            "Kinnan, Bonder Prodigy",
            2,
            2,
            "Whenever you tap a nonland permanent for mana, add one mana of any type that permanent produced.\n\
{5}{G}{U}: Look at the top five cards of your library. You may put a non-Human creature card from among them onto the battlefield. Put the rest on the bottom of your library in a random order.",
        )
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Green, ManaCostShard::Blue],
            generic: 0,
        })
        .id();
    let elves = scenario
        .add_creature_from_oracle(P1, "Llanowar Elves", 1, 1, "{T}: Add {G}.")
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Green],
            generic: 0,
        })
        .id();
    let control = scenario
        .add_enchantment_from_oracle(
            P2,
            "Control Magic",
            "Enchant creature\nYou control enchanted creature.",
        )
        .with_subtypes(vec!["Aura"])
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Blue, ManaCostShard::Blue],
            generic: 2,
        })
        .id();
    let responses: Vec<_> = (0..2)
        .map(|_| {
            scenario
                .add_spell_to_hand_from_oracle(
                    P2,
                    "Flicker of Fate",
                    true,
                    "Exile target creature or enchantment, then return it to the battlefield under its owner's control.",
                )
                .with_mana_cost(ManaCost::Cost {
                    shards: vec![ManaCostShard::White],
                    generic: 1,
                })
                .id()
        })
        .collect();
    let plains: Vec<_> = (0..4)
        .map(|_| scenario.add_basic_land(P2, ManaColor::White))
        .collect();
    for player in [P0, P1, P2] {
        scenario.add_land_to_library_top(player, "Forest");
    }
    let top_card = scenario.add_card_to_library_top(P1, "Hill Giant");
    let mut runner = scenario.build();
    attach_to_player(runner.state_mut(), curse, P1);
    engine::game::effects::attach::attach_to(runner.state_mut(), control, kinnan);
    evaluate_layers(runner.state_mut());
    reindex_object_triggers(runner.state_mut(), curse);
    // CR 302.6: initial-board setup represents Control Magic already having
    // controlled Kinnan since before P2's turn, not a fresh control change.
    runner
        .state_mut()
        .objects
        .get_mut(&kinnan)
        .unwrap()
        .summoning_sick = false;
    assert_eq!(runner.state().objects[&kinnan].controller, P2);
    assert_eq!(runner.state().objects[&elves].controller, P1);
    assert!(!runner.state().objects[&elves].tapped);
    assert!(!runner.state().objects[&kinnan].summoning_sick);

    let mut original_stamp = None;
    let mut mana_receipt = None;
    let mut pending_after_blinks = None;
    let mut blink = respond_once(P2, responses[0], kinnan, curse_trigger_pending(curse));
    let mut blink_again = respond_once(P2, responses[1], kinnan, curse_trigger_pending(curse));
    attack_with(
        &mut runner,
        kinnan,
        P2,
        P1,
        |_| panic!("Kinnan and Curse require no trigger ordering"),
        |runner| {
            eprintln!(
                "pr10613 fixture priority={:?} blink zones={:?}/{:?} receipt={:?} captured={} stack={:?}",
                runner.state().waiting_for,
                runner.state().objects[&responses[0]].zone,
                runner.state().objects[&responses[1]].zone,
                mana_receipt,
                pending_after_blinks.is_some(),
                runner.state().stack.iter().map(|entry| entry.source_id).collect::<Vec<_>>()
            );
            let Some(entry) = runner.state().stack.iter().find(|e| e.source_id == curse) else {
                return false;
            };
            assert_eq!(entry.controller, P0);
            let StackEntryKind::TriggeredAbility {
                trigger_event:
                    Some(GameEvent::DamageDealt {
                        source_id,
                        source_incarnation: Some(stamp),
                        target,
                        amount: 2,
                        is_combat: true,
                        ..
                    }),
                ..
            } = &entry.kind
            else {
                panic!("combat must publish the real stamped Curse trigger");
            };
            assert_eq!(*source_id, kinnan);
            assert_eq!(*target, engine::types::ability::TargetRef::Player(P1));
            let stamp = *stamp;
            if let Some(original) = original_stamp {
                assert_eq!(stamp, original, "Curse keeps the old damage-dealer pin");
            } else {
                assert_eq!(runner.state().objects[&kinnan].incarnation, stamp);
                original_stamp = Some(stamp);
            }
            if blink(runner) {
                return true;
            }
            if runner.state().objects[&responses[0]].zone != Zone::Graveyard {
                return false;
            }
            if mana_receipt.is_none() {
                // P2 cast the first Flicker, but P1 now controls Kinnan and
                // Elves. Wait for P1's real priority rather than changing seats.
                if !matches!(runner.state().waiting_for, WaitingFor::Priority { player } if player == P1)
                {
                    return false;
                }
                let state = runner.state();
                assert_eq!(state.stack.len(), 1, "only Curse is pending");
                let returned = &state.objects[&kinnan];
                assert_eq!(returned.zone, Zone::Battlefield);
                assert_eq!(returned.controller, P1);
                assert!(stamp < returned.incarnation);
                assert_eq!(returned.trigger_definitions.len(), 1);
                let definition = returned.trigger_definition_ref(&returned.trigger_definitions[0]);
                assert_eq!(definition.source.object_id, kinnan);
                assert_eq!(definition.source.incarnation, returned.incarnation);
                assert!(state.triggered_abilities_added_mana_this_turn.is_empty());
                assert_eq!(state.players[1].mana_pool.total(), 0);
                runner
                    .act(GameAction::ActivateAbility {
                        source_id: elves,
                        ability_index: 0,
                        payment_mode: CastPaymentMode::Auto,
                    })
                    .expect("P1 activates the Elves while Curse remains pending");
                let state = runner.state();
                // CR 605.4a: single-type Elves mana requires no choice or
                // stack entry; Kinnan's additional green resolves immediately.
                assert!(
                    matches!(state.waiting_for, WaitingFor::Priority { player } if player == P1)
                );
                assert_eq!(state.stack.len(), 1);
                assert_eq!(state.stack[0].source_id, curse);
                assert!(state.objects[&elves].tapped);
                let pool = &state.players[1].mana_pool;
                assert_eq!(pool.total(), 2);
                assert_eq!(pool.count_color(ManaType::Green), 2);
                assert_eq!(pool.count_where(|mana| mana.source_id == elves), 1);
                assert_eq!(pool.count_where(|mana| mana.source_id == kinnan), 1);
                assert_eq!(state.triggered_abilities_added_mana_this_turn.len(), 1);
                assert!(state
                    .triggered_abilities_added_mana_this_turn
                    .contains(&(definition.clone(), P1)));
                mana_receipt = Some(definition);
                return true;
            }
            // Only after the intermediate Kinnan has produced its receipt may
            // P2 cast the second paid Flicker at P2's next actual priority.
            if blink_again(runner) {
                return true;
            }
            if pending_after_blinks.is_none()
                && runner.state().objects[&responses[1]].zone == Zone::Graveyard
            {
                let state = runner.state();
                let receipt = mana_receipt.as_ref().expect("the real Kinnan mana receipt");
                assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
                assert_eq!(state.stack.len(), 1);
                assert_eq!(state.players[1].life, 18, "the 2/2 Kinnan connected");
                assert_eq!(state.objects[&top_card].zone, Zone::Library);
                assert!(grants(runner, top_card).is_empty());
                assert_eq!(state.objects[&kinnan].zone, Zone::Battlefield);
                assert_eq!(state.objects[&kinnan].controller, P1);
                assert!(stamp < receipt.source.incarnation);
                assert!(receipt.source.incarnation < state.objects[&kinnan].incarnation);
                assert_eq!(receipt.source.object_id, kinnan, "not the Elf's epoch");
                assert_eq!(state.triggered_abilities_added_mana_this_turn.len(), 1);
                assert!(state
                    .triggered_abilities_added_mana_this_turn
                    .contains(&(receipt.clone(), P1)));
                assert_eq!(state.players[1].mana_pool.count_color(ManaType::Green), 2);
                assert!(plains.iter().all(|land| state.objects[land].tapped));
                assert_eq!(state.players[2].mana_pool.total(), 0);
                // CR 608.2h: only the old Kinnan snapshot supplies P2.
                assert_eq!(state.lki_by_incarnation[&kinnan][&stamp].controller, P2);
                assert_eq!(state.lki_cache[&kinnan].controller, P1);
                let active_set = state.chain_tracked_set_id.expect("second Flicker's set");
                let latest_nonempty_set = state
                    .tracked_object_sets
                    .iter()
                    .filter(|(_, members)| !members.is_empty())
                    .max_by_key(|(id, _)| id.0)
                    .map(|(&id, _)| id);
                assert_eq!(latest_nonempty_set, Some(active_set));
                assert_ne!(
                    state.tracked_set_member_origins[&active_set][&kinnan].departure_incarnation,
                    stamp,
                    "the latest Flicker origin does not independently retain old Kinnan"
                );
                pending_after_blinks = Some(state.clone());
            }
            false
        },
    );
    assert_eq!(runner.state().objects[&top_card].zone, Zone::Exile);
    // CR 608.2h + CR 611.2a: old Kinnan's controller gets the printed
    // until-end-of-turn permission and its AnyColor rider.
    let expected_grant = vec![(
        P2,
        Duration::UntilEndOfTurn,
        Some(ManaSpendPermission::AnyColor),
    )];
    assert_eq!(grants(&runner, top_card), expected_grant);
    assert!(runner.state().stack.is_empty());
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));

    let pending = pending_after_blinks.expect("capture actual Priority before Curse resolves");
    // Controlled comparator twin, NOT an independently played history:
    // change only this receipt ledger, keeping mana and the selected LKI.
    let mut receipt_free = pending.clone();
    receipt_free
        .triggered_abilities_added_mana_this_turn
        .clear();
    let mut receipt_free_future = GameRunner::from_state(receipt_free.clone());
    receipt_free_future.advance_until_stack_empty();
    assert_eq!(
        receipt_free_future.state().objects[&top_card].zone,
        Zone::Exile
    );
    assert_eq!(grants(&receipt_free_future, top_card), expected_grant);
    assert!(receipt_free_future.state().stack.is_empty());
    assert!(matches!(
        receipt_free_future.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    // Strict equality intentionally retains meaningful mana history; only
    // resource projection may erase it. Never resume a normalized comparand.
    assert_ne!(pending, receipt_free);
    assert!(loop_states_equal_modulo_resources(
        &pending,
        &pending.clone()
    ));
    assert!(loop_states_equal_modulo_resources(
        &receipt_free,
        &receipt_free.clone()
    ));
    assert!(
        loop_states_equal_modulo_resources(&pending, &receipt_free),
        "erasing the intermediate Kinnan mana receipt must not rerank the retained Curse pin"
    );
    assert!(
        loop_states_equal_modulo_resources(&receipt_free, &pending),
        "projected equivalence must also accept the reversed raw pair"
    );
}

/// CR 608.2h: while the SAME object stays on the battlefield, its current
/// controller answers. P2 destroys its own control Aura with the trigger
/// pending; the creature, never having left, is controlled by P1 again.
#[test]
fn a_damage_dealer_that_stayed_names_its_current_controller() {
    let (mut runner, raider, control, top_card, responses) = stolen_attacker_board(
        "Response",
        "Destroy target enchantment.",
        ManaCost::zero(),
        1,
    );
    let curse = curse_source(&runner);
    attack_with(
        &mut runner,
        raider,
        P2,
        P1,
        |_| panic!("no trigger order expected"),
        respond_once(P2, responses[0], control, curse_trigger_pending(curse)),
    );
    assert_eq!(runner.state().objects[&control].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&raider].zone, Zone::Battlefield);
    assert_eq!(runner.state().objects[&raider].controller, P1);
    assert_eq!(
        grants(&runner, top_card),
        vec![(
            P1,
            Duration::UntilEndOfTurn,
            Some(ManaSpendPermission::AnyColor)
        )]
    );
}

fn curse_source(runner: &GameRunner) -> ObjectId {
    runner
        .state()
        .objects
        .values()
        .find(|o| o.name == "Curse of Hospitality")
        .map(|o| o.id)
        .expect("the Curse")
}

/// CR 400.7 + CR 608.2h: Contested Game Ball shares the grantee's arm
/// (`TargetFilter::TriggeringSourceController`), but reads the per-event combat
/// `DamageDealt` rather than the aggregate. P1 attacks P0 with P0's creature,
/// stolen by P1's control Aura, and blinks it with the trigger pending; it
/// returns under P0 as a new object, and "the attacking player" is still P1.
#[test]
fn contested_game_ball_names_the_attacking_player_after_a_blink() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let ball = scenario
        .add_artifact_from_oracle(
            P0,
            "Contested Game Ball",
            "Whenever you're dealt combat damage, the attacking player gains control of this artifact and untaps it.",
        )
        .id();
    let raider = scenario
        .add_creature_from_oracle(P0, "Stolen Raider", 2, 2, "Haste")
        .id();
    let control = scenario
        .add_enchantment_from_oracle(
            P1,
            "Control Magic",
            "Enchant creature\nYou control enchanted creature.",
        )
        .with_subtypes(vec!["Aura"])
        .id();
    let blink = scenario
        .add_spell_to_hand_from_oracle(
            P1,
            "Blink",
            true,
            "Exile target creature you control, then return it to the battlefield under its owner's control.",
        )
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();
    engine::game::effects::attach::attach_to(runner.state_mut(), control, raider);
    evaluate_layers(runner.state_mut());
    reindex_object_triggers(runner.state_mut(), ball);
    assert_eq!(runner.state().objects[&raider].controller, P1);

    let ball_pending =
        move |runner: &GameRunner| runner.state().stack.iter().any(|e| e.source_id == ball);
    attack_with(
        &mut runner,
        raider,
        P1,
        P0,
        |_| panic!("no trigger order expected"),
        respond_once(P1, blink, raider, ball_pending),
    );
    assert_eq!(
        runner.state().objects[&raider].controller,
        P0,
        "the blinked creature returned under its owner"
    );
    assert_eq!(runner.state().objects[&ball].controller, P1);
}
