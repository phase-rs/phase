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
use engine::game::effects::attach::attach_to_player;
use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::trigger_index::reindex_object_triggers;
use engine::types::ability::Duration;
use engine::types::ability::{CastingPermission, ManaSpendPermission};
use engine::types::actions::GameAction;
use engine::types::game_state::CastPaymentMode;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard};
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
/// attacks P1. Returns the runner, the creature, the Aura, P1's top card and a
/// zero-cost instant in P2's hand with `response_text`.
fn stolen_attacker_board(
    response_text: &str,
) -> (GameRunner, ObjectId, ObjectId, ObjectId, ObjectId) {
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
    let response = scenario
        .add_spell_to_hand_from_oracle(P2, "Response", true, response_text)
        .with_mana_cost(ManaCost::zero())
        .id();
    let top_card = scenario.add_card_to_library_top(P1, "Hill Giant");
    let mut runner = scenario.build();
    attach_to_player(runner.state_mut(), curse, P1);
    engine::game::effects::attach::attach_to(runner.state_mut(), control, raider);
    evaluate_layers(runner.state_mut());
    reindex_object_triggers(runner.state_mut(), curse);
    assert_eq!(runner.state().objects[&raider].controller, P2);
    (runner, raider, control, top_card, response)
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
        if matches!(
            runner.state().waiting_for,
            WaitingFor::TargetSelection { .. }
        ) {
            runner
                .act(GameAction::ChooseTarget {
                    target: Some(engine::types::ability::TargetRef::Object(target)),
                })
                .expect("target the creature");
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
    let (mut runner, raider, _control, top_card, response) = stolen_attacker_board(
        "Exile target creature you control, then return it to the battlefield under its owner's control.",
    );
    let curse = curse_source(&runner);
    attack_with(
        &mut runner,
        raider,
        P2,
        P1,
        |_| panic!("no trigger order expected"),
        respond_once(P2, response, raider, curse_trigger_pending(curse)),
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
}

/// CR 608.2h: while the SAME object stays on the battlefield, its current
/// controller answers. P2 destroys its own control Aura with the trigger
/// pending; the creature, never having left, is controlled by P1 again.
#[test]
fn a_damage_dealer_that_stayed_names_its_current_controller() {
    let (mut runner, raider, control, top_card, response) =
        stolen_attacker_board("Destroy target enchantment.");
    let curse = curse_source(&runner);
    attack_with(
        &mut runner,
        raider,
        P2,
        P1,
        |_| panic!("no trigger order expected"),
        respond_once(P2, response, control, curse_trigger_pending(curse)),
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
