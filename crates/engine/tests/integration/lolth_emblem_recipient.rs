use crate::support::{assert_damage_dealt, shared_card_db};
use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::{DamageReceivedGrouping, TargetRef};
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::{CastPaymentMode, StackEntryKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaColor;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const P2: PlayerId = PlayerId(2);

const LOLTH: &str = r#"Whenever a creature you control dies, put a loyalty counter on Lolth.
[0]: You draw a card and you lose 1 life.
[−3]: Create two 2/1 black Spider creature tokens with reach and menace.
[−8]: You get an emblem with "Whenever an opponent is dealt combat damage by one or more creatures you control, if that player lost less than 8 life this turn, they lose life equal to the difference.""#;

fn tap_lands(runner: &mut GameRunner, lands: &[ObjectId]) {
    for &source_id in lands {
        runner
            .act(GameAction::ActivateAbility {
                source_id,
                ability_index: 0,
                payment_mode: CastPaymentMode::Auto,
            })
            .expect("activate the basic land's mana ability");
        assert!(runner.state().objects[&source_id].tapped);
    }
}

// CR 606.3 + CR 606.4 + CR 510.2 + CR 603.4: Pay the printed ultimate's
// loyalty cost, then exercise the emblem with a single combat damage recipient.
#[test]
fn lolth_emblem_applies_only_the_damaged_opponents_shortfall() {
    let db = shared_card_db().expect("Lolth's printed card fixture");
    for (prior_loss, final_life) in [(0, 12), (8, 9)] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let setup = EmblemSetup::stage_from_oracle(&mut scenario);
        let prior_loss_spell = scenario.add_real_card(P0, "Exsanguinate", Zone::Hand, db);
        let ([attacker], forests) = stage_ram_gangs(&mut scenario, P0);
        let swamps: Vec<_> = (0..if prior_loss > 0 { 10 } else { 0 })
            .map(|_| scenario.add_basic_land(P0, ManaColor::Black))
            .collect();
        let mut runner = scenario.build();
        let emblem = setup.create_emblem(&mut runner);

        if prior_loss > 0 {
            tap_lands(&mut runner, &swamps);
            runner.cast(prior_loss_spell).x(prior_loss as u32).resolve();
        }
        assert_eq!(runner.life(P1), 20 - prior_loss);
        cast_ram_gangs(&mut runner, &[attacker], &forests);
        let events = combat_to_damage(&mut runner, &[(attacker, AttackTarget::Player(P1))]);
        assert_damage_dealt(&events, attacker, TargetRef::Player(P1), 3, true);
        // CR 603.4: The threshold prevents the ability from triggering at all.
        if prior_loss == 8 {
            assert!(!events.iter().any(|event| matches!(
                event,
                GameEvent::AbilityTriggered { source_id, .. } if *source_id == emblem
            )));
            assert!(runner
                .state()
                .stack
                .iter()
                .all(|entry| entry.source_id != emblem));
        } else {
            assert_eq!(runner.state().stack.len(), 1);
            assert_eq!(runner.state().stack[0].source_id, emblem);
        }
        runner.advance_until_stack_empty();
        assert!(runner.state().stack.is_empty());
        assert_eq!(runner.life(P0), 20 + prior_loss);
        assert_eq!(runner.life(P1), final_life);
    }
}

struct EmblemSetup {
    lolth: ObjectId,
    victims: [ObjectId; 4],
    sweep: ObjectId,
    plains: [ObjectId; 4],
}

impl EmblemSetup {
    fn stage(scenario: &mut GameScenario) -> Self {
        let db = shared_card_db().expect("printed card fixture");
        let lolth = scenario.add_real_card(P0, "Lolth, Spider Queen", Zone::Battlefield, db);
        Self::with_lolth(scenario, lolth)
    }

    fn stage_from_oracle(scenario: &mut GameScenario) -> Self {
        let lolth = scenario
            .add_planeswalker_from_oracle(P0, "Lolth, Spider Queen", "Lolth", 4, LOLTH)
            .id();
        Self::with_lolth(scenario, lolth)
    }

    fn with_lolth(scenario: &mut GameScenario, lolth: ObjectId) -> Self {
        let db = shared_card_db().expect("printed card fixture");
        let victims = std::array::from_fn(|_| scenario.add_vanilla(P0, 1, 1));
        let sweep = scenario.add_real_card(P0, "Wrath of God", Zone::Hand, db);
        let plains = std::array::from_fn(|_| scenario.add_basic_land(P0, ManaColor::White));
        Self {
            lolth,
            victims,
            sweep,
            plains,
        }
    }

    // CR 606.3 + CR 606.4: build up to eight loyalty through deaths, then
    // activate the ultimate in the main phase with an empty stack.
    fn create_emblem(self, runner: &mut GameRunner) -> ObjectId {
        let Self {
            lolth,
            victims,
            sweep,
            plains,
        } = self;
        assert_eq!(runner.state().objects[&lolth].loyalty, Some(4));
        tap_lands(runner, &plains);
        runner.cast(sweep).resolve();
        for victim in victims {
            assert_eq!(runner.state().objects[&victim].zone, Zone::Graveyard);
        }
        assert_eq!(runner.state().objects[&lolth].loyalty, Some(8));
        assert_eq!(runner.state().phase, Phase::PreCombatMain);
        assert!(runner.state().stack.is_empty());
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { player: P0 }
        ));
        runner.activate(lolth, 2).resolve();
        assert_eq!(runner.state().objects[&lolth].zone, Zone::Graveyard);
        assert!(runner.state().stack.is_empty());

        runner
            .state()
            .objects
            .values()
            .find(|object| object.zone == Zone::Command && object.name == "Emblem")
            .expect("Lolth's ultimate created the emblem")
            .id
    }
}

fn stage_ram_gangs<const N: usize>(
    scenario: &mut GameScenario,
    player: PlayerId,
) -> ([ObjectId; N], [[ObjectId; 3]; N]) {
    let db = shared_card_db().expect("printed card fixture");
    let creatures =
        std::array::from_fn(|_| scenario.add_real_card(player, "Boggart Ram-Gang", Zone::Hand, db));
    let lands = std::array::from_fn(|_| {
        std::array::from_fn(|_| scenario.add_basic_land(player, ManaColor::Green))
    });
    (creatures, lands)
}

fn cast_ram_gangs(runner: &mut GameRunner, creatures: &[ObjectId], lands: &[[ObjectId; 3]]) {
    for (&creature, payment) in creatures.iter().zip(lands) {
        tap_lands(runner, payment);
        runner.cast(creature).resolve();
        assert_eq!(runner.state().objects[&creature].zone, Zone::Battlefield);
    }
}

fn combat_to_damage(
    runner: &mut GameRunner,
    attacks: &[(ObjectId, AttackTarget)],
) -> Vec<GameEvent> {
    runner.advance_to_combat();
    let mut events = runner
        .declare_attackers(attacks)
        .expect("legal attacks")
        .events;
    for _ in 0..32 {
        if matches!(runner.state().phase, Phase::CombatDamage | Phase::EndCombat) {
            return events;
        }
        let action = match runner.state().waiting_for {
            WaitingFor::Priority { .. } => GameAction::PassPriority,
            WaitingFor::DeclareBlockers { .. } => GameAction::DeclareBlockers {
                assignments: vec![],
            },
            ref waiting => panic!("unexpected combat prompt: {waiting:?}"),
        };
        events.extend(runner.act(action).expect("advance to combat damage").events);
    }
    panic!("combat damage did not happen");
}

#[test]
fn lolth_emblem_fires_once_for_each_damaged_opponent() {
    let db = shared_card_db().expect("printed card fixture");
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let setup = EmblemSetup::stage(&mut scenario);
    let ([first, second, third], forests) = stage_ram_gangs(&mut scenario, P0);
    let shock = scenario.add_real_card(P0, "Shock", Zone::Hand, db);
    let mountain = scenario.add_basic_land(P0, ManaColor::Red);
    let mut runner = scenario.build();
    let emblem = setup.create_emblem(&mut runner);
    let triggers = &runner.state().objects[&emblem].trigger_definitions;
    assert_eq!(triggers.len(), 1);
    let definition = triggers.first().expect("emblem trigger").definition();
    assert_eq!(
        definition.damage_received_grouping,
        DamageReceivedGrouping::PerPlayerRecipient
    );
    assert!(!definition.batched);

    tap_lands(&mut runner, &[mountain]);
    runner
        .cast(shock)
        .target_player(P2)
        .resolve()
        .assert_life_delta(P2, -2);
    assert_eq!((runner.life(P1), runner.life(P2)), (20, 18));
    cast_ram_gangs(&mut runner, &[first, second, third], &forests);

    let events = combat_to_damage(
        &mut runner,
        &[
            (first, AttackTarget::Player(P1)),
            (second, AttackTarget::Player(P1)),
            (third, AttackTarget::Player(P2)),
        ],
    );
    assert_damage_dealt(&events, first, TargetRef::Player(P1), 3, true);
    assert_damage_dealt(&events, second, TargetRef::Player(P1), 3, true);
    assert_damage_dealt(&events, third, TargetRef::Player(P2), 3, true);
    assert_eq!((runner.life(P1), runner.life(P2)), (14, 15));

    // CR 510.2 + CR 603.2c: simultaneous damage yields one firing per recipient.
    if let WaitingFor::OrderTriggers { triggers, .. } = &runner.state().waiting_for {
        runner
            .act(GameAction::OrderTriggers {
                order: (0..triggers.len()).collect(),
            })
            .expect("choose emblem trigger order");
    }
    assert_eq!(
        runner.state().stack.len(),
        2,
        "one emblem trigger per damaged player"
    );
    let mut recipients = Vec::new();
    for entry in &runner.state().stack {
        assert_eq!(entry.source_id, emblem);
        let StackEntryKind::TriggeredAbility {
            trigger_event:
                Some(GameEvent::DamageDealt {
                    source_id,
                    target: TargetRef::Player(recipient),
                    is_combat: true,
                    ..
                }),
            ..
        } = &entry.kind
        else {
            panic!("expected an emblem trigger with its original combat damage event");
        };
        match *recipient {
            P1 => assert!([first, second].contains(source_id)),
            P2 => assert_eq!(*source_id, third),
            other => panic!("unexpected recipient: {other:?}"),
        }
        recipients.push(*recipient);
    }
    assert!(recipients.contains(&P1) && recipients.contains(&P2));

    runner.advance_until_stack_empty();
    assert!(runner.state().stack.is_empty());
    assert_eq!(
        (runner.life(P0), runner.life(P1), runner.life(P2)),
        (20, 12, 12)
    );
}

#[test]
fn lolth_emblem_rechecks_life_loss_on_resolution() {
    let db = shared_card_db().expect("printed card fixture");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let setup = EmblemSetup::stage(&mut scenario);
    let ([first, second], forests) = stage_ram_gangs(&mut scenario, P0);
    let bolt = scenario.add_real_card(P0, "Lightning Bolt", Zone::Hand, db);
    let mountain = scenario.add_basic_land(P0, ManaColor::Red);
    let mut runner = scenario.build();
    let emblem = setup.create_emblem(&mut runner);
    cast_ram_gangs(&mut runner, &[first, second], &forests);
    combat_to_damage(
        &mut runner,
        &[
            (first, AttackTarget::Player(P1)),
            (second, AttackTarget::Player(P1)),
        ],
    );
    assert_eq!(runner.life(P1), 14);
    assert_eq!(runner.state().stack.len(), 1);
    assert_eq!(runner.state().stack[0].source_id, emblem);

    tap_lands(&mut runner, &[mountain]);
    {
        let response = runner.cast(bolt).target_player(P1).commit();
        assert_eq!(response.state().stack.len(), 2);
    }
    runner.resolve_top();
    assert_eq!(runner.life(P1), 11);
    assert_eq!(runner.state().stack.len(), 1);
    assert_eq!(runner.state().stack[0].source_id, emblem);

    // CR 608.2a: the trigger does nothing once the player has lost at least eight life.
    runner.resolve_top();
    assert!(runner.state().stack.is_empty());
    assert_eq!(runner.life(P1), 11);
}

#[test]
fn lolth_emblem_ignores_combat_damage_from_an_opponents_creature() {
    let db = shared_card_db().expect("printed card fixture");
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let setup = EmblemSetup::stage(&mut scenario);
    let ([attacker], forests) = stage_ram_gangs(&mut scenario, P1);
    scenario.add_real_card(P1, "Forest", Zone::Library, db);
    let mut runner = scenario.build();
    setup.create_emblem(&mut runner);
    for _ in 0..128 {
        if runner.state().active_player == P1 && runner.state().phase == Phase::PreCombatMain {
            break;
        }
        match runner.state().waiting_for {
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            WaitingFor::DeclareAttackers { player: P0, .. } => {
                runner
                    .declare_attackers(&[])
                    .expect("P0 attacks with no creatures");
            }
            ref waiting => panic!("unexpected prompt on the way to P1's turn: {waiting:?}"),
        }
    }
    assert_eq!(runner.state().active_player, P1);
    assert_eq!(runner.state().phase, Phase::PreCombatMain);
    cast_ram_gangs(&mut runner, &[attacker], &forests);
    let events = combat_to_damage(&mut runner, &[(attacker, AttackTarget::Player(P2))]);
    assert_damage_dealt(&events, attacker, TargetRef::Player(P2), 3, true);
    assert_eq!(runner.life(P2), 17);
    assert!(runner.state().stack.is_empty());
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
}

#[test]
fn lolth_emblem_ignores_noncombat_damage_from_your_creature() {
    let db = shared_card_db().expect("printed card fixture");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let setup = EmblemSetup::stage(&mut scenario);
    let whirler = scenario.add_real_card(P0, "Goblin Chainwhirler", Zone::Hand, db);
    let mountains = std::array::from_fn::<_, 3, _>(|_| scenario.add_basic_land(P0, ManaColor::Red));
    let mut runner = scenario.build();
    let emblem = setup.create_emblem(&mut runner);
    tap_lands(&mut runner, &mountains);

    let outcome = runner.cast(whirler).resolve();
    assert_damage_dealt(outcome.events(), whirler, TargetRef::Player(P1), 1, false);
    outcome.assert_life_delta(P1, -1);
    assert_eq!(runner.life(P1), 19);
    assert!(!outcome.events().iter().any(|event| matches!(
        event,
        GameEvent::AbilityTriggered { source_id, .. } if *source_id == emblem
    )));
    assert!(outcome.state().stack.is_empty());
}

// CR 510.2 + CR 603.2c: A collective recipient still fires once when the
// source qualifier independently uses "one or more". Master of Barbs and
// Fblthp, Impossibly Lost use collective recipients without a source qualifier.
// "Collective Damage Watcher" is synthetic: no printed card is known to combine
// a collective recipient with a quantified source. The nearest printed cards
// are Master of Barbs and Fblthp, Impossibly Lost (collective recipients) and
// Lolth, Spider Queen's emblem (quantified source). The test checks precedence
// between those two existing grammar axes.
#[test]
fn qualified_collective_damage_recipients_fire_once_for_the_batch() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let watcher = scenario
        .add_enchantment_from_oracle(
            P0,
            "Collective Damage Watcher",
            "Whenever one or more opponents are dealt combat damage by one or more creatures you control, you gain 1 life.",
        )
        .id();
    let ([first, second], forests) = stage_ram_gangs(&mut scenario, P0);
    let mut runner = scenario.build();
    cast_ram_gangs(&mut runner, &[first, second], &forests);

    let events = combat_to_damage(
        &mut runner,
        &[
            (first, AttackTarget::Player(P1)),
            (second, AttackTarget::Player(P2)),
        ],
    );
    assert_damage_dealt(&events, first, TargetRef::Player(P1), 3, true);
    assert_damage_dealt(&events, second, TargetRef::Player(P2), 3, true);
    assert_eq!((runner.life(P1), runner.life(P2)), (17, 17));
    assert_eq!(
        runner.state().stack.len(),
        1,
        "the qualified collective trigger must fire once"
    );
    assert_eq!(runner.state().stack[0].source_id, watcher);
    assert!(matches!(
        runner.state().stack[0].kind,
        StackEntryKind::TriggeredAbility { .. }
    ));
    runner.resolve_top();
    assert!(runner.state().stack.is_empty());
    assert_eq!(runner.life(P0), 21);
}
