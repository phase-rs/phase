//! The Jolly Balloon Man: "{1}, {T}: Create a token that's a copy of another
//! target creature you control, except it's a 1/1 red Balloon creature in
//! addition to its other colors and types and it has flying and haste.
//! Sacrifice it at the beginning of the next end step. Activate only as a
//! sorcery."
//!
//! Issue #714 reported the token as a copy of the original creature instead of
//! a plain 1/1 Balloon. A copy is what the card makes (CR 707.2), with the
//! exceptions becoming copiable values (CR 707.9b). These rows pin the copy,
//! its exceptions, the delayed sacrifice (CR 603.7 + CR 701.21a), the
//! "another target creature you control" filter and the sorcery-speed
//! restriction (CR 602.5d).

use crate::support::{advance_until_stack_empty_and_collect_events, shared_card_db};
use engine::ai_support::{legal_actions, legal_actions_full};
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::events::GameEvent;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::{parse_mtgjson_mana_cost, ManaColor};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const ORACLE: &str = "Haste\n\
{1}, {T}: Create a token that's a copy of another target creature you control, except it's a 1/1 red Balloon creature in addition to its other colors and types and it has flying and haste. Sacrifice it at the beginning of the next end step. Activate only as a sorcery.";

struct Fixture {
    runner: GameRunner,
    balloon_man: ObjectId,
    bears: ObjectId,
    second_bears: ObjectId,
    opposing_bears: ObjectId,
    opt: ObjectId,
    island: ObjectId,
}

impl Fixture {
    fn new(phase: Phase) -> Self {
        let db = shared_card_db().expect("integration card database");
        let mut scenario = GameScenario::new();
        scenario.at_phase(phase);
        let balloon_man = scenario
            .add_creature(P0, "The Jolly Balloon Man", 1, 4)
            .as_legendary()
            .with_subtypes(vec!["Human", "Clown"])
            .with_mana_cost(parse_mtgjson_mana_cost("{1}{R}{W}"))
            .with_color(vec![ManaColor::Red, ManaColor::White])
            .from_oracle_text_with_keywords(&["Haste"], ORACLE)
            .id();
        let bears = scenario.add_real_card(P0, "Grizzly Bears", Zone::Battlefield, db);
        let second_bears = scenario.add_real_card(P0, "Grizzly Bears", Zone::Battlefield, db);
        let opposing_bears = scenario.add_real_card(P1, "Grizzly Bears", Zone::Battlefield, db);
        let forest = scenario.add_real_card(P0, "Forest", Zone::Battlefield, db);
        let island = scenario.add_real_card(P0, "Island", Zone::Battlefield, db);
        let opt = scenario.add_real_card(P0, "Opt", Zone::Hand, db);
        let mut runner = scenario.build();
        tap_land(&mut runner, forest);
        Self {
            runner,
            balloon_man,
            bears,
            second_bears,
            opposing_bears,
            opt,
            island,
        }
    }

    fn activation(&self) -> GameAction {
        GameAction::ActivateAbility {
            source_id: self.balloon_man,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        }
    }

    fn copy_bears(&mut self) -> ObjectId {
        let outcome = self
            .runner
            .activate(self.balloon_man, 0)
            .target_object(self.bears)
            .resolve();
        assert!(matches!(
            outcome.final_waiting_for(),
            WaitingFor::Priority { player: P0 }
        ));
        assert!(outcome.state().stack.is_empty());
        // CR 602.2b: Activation pays both the mana and tap costs.
        assert!(outcome.state().objects[&self.balloon_man].tapped);
        assert_eq!(outcome.mana_pool_total(P0), 0);
        let tokens: Vec<_> = outcome
            .state()
            .objects
            .values()
            .filter(|obj| obj.zone == Zone::Battlefield && obj.controller == P0 && obj.is_token)
            .map(|obj| obj.id)
            .collect();
        let [token] = tokens.as_slice() else {
            panic!("expected one token on P0's battlefield, got {tokens:?}");
        };
        *token
    }

    fn activation_is_offered(&self) -> bool {
        let state = self.runner.state();
        assert!(matches!(
            state.waiting_for,
            WaitingFor::Priority { player: P0 }
        ));
        assert!(!state.objects[&self.balloon_man].tapped);
        let bears = &state.objects[&self.bears];
        assert_eq!(bears.zone, Zone::Battlefield);
        assert_eq!(bears.controller, P0);
        let player = state.players.iter().find(|player| player.id == P0).unwrap();
        assert_eq!(player.mana_pool.total(), 1);
        legal_actions(state).iter().any(|action| {
            matches!(
                action,
                GameAction::ActivateAbility { source_id, ability_index: 0, .. }
                    if *source_id == self.balloon_man
            )
        })
    }
}

fn tap_land(runner: &mut GameRunner, land: ObjectId) {
    let (_, _, grouped) = legal_actions_full(runner.state());
    let action = grouped
        .get(&land)
        .into_iter()
        .flatten()
        .find(|action| matches!(action, GameAction::TapLandForMana { .. }))
        .cloned()
        .expect("land can tap for mana");
    runner.act(action).expect("tap land for mana");
}

#[test]
fn copy_keeps_the_creature_but_is_a_1_1_red_balloon_with_flying_and_haste() {
    let mut fixture = Fixture::new(Phase::PreCombatMain);
    let token = fixture.copy_bears();
    let state = fixture.runner.state();
    let copy = &state.objects[&token];
    let bears = &state.objects[&fixture.bears];

    // CR 707.2: The copy retains the original's name and mana cost.
    assert_eq!(copy.name, "Grizzly Bears");
    assert_eq!(copy.mana_cost, parse_mtgjson_mana_cost("{1}{G}"));
    assert!(
        bears.printed_ref.is_some(),
        "the original has a printing for display"
    );
    assert_eq!(copy.printed_ref, bears.printed_ref);

    // CR 707.9b: The exceptions modify the copy's characteristics.
    assert_eq!(
        (copy.power, copy.toughness),
        (Some(1), Some(1)),
        "copy power and toughness"
    );
    assert_eq!(copy.color.len(), 2);
    assert!(copy.color.contains(&ManaColor::Green));
    assert!(copy.color.contains(&ManaColor::Red));
    assert!(copy.card_types.core_types.contains(&CoreType::Creature));
    assert!(copy
        .card_types
        .subtypes
        .iter()
        .any(|subtype| subtype == "Bear"));
    assert!(copy
        .card_types
        .subtypes
        .iter()
        .any(|subtype| subtype == "Balloon"));
    assert!(copy.keywords.contains(&Keyword::Flying));
    assert!(copy.keywords.contains(&Keyword::Haste));
    assert_eq!(bears.zone, Zone::Battlefield);
    assert_eq!((bears.power, bears.toughness), (Some(2), Some(2)));
    assert_eq!(bears.color, [ManaColor::Green]);
    assert!(!bears
        .card_types
        .subtypes
        .iter()
        .any(|subtype| subtype == "Balloon"));
}

#[test]
fn the_copy_is_sacrificed_at_the_next_end_step() {
    let mut fixture = Fixture::new(Phase::PostCombatMain);
    let token = fixture.copy_bears();
    let runner = &mut fixture.runner;
    assert_eq!(runner.state().phase, Phase::PostCombatMain);
    assert_eq!(runner.state().objects[&token].zone, Zone::Battlefield);

    // CR 603.7b: The delayed trigger goes on the stack at the next end step.
    runner.advance_to_end_step();
    assert_eq!(runner.state().phase, Phase::End);
    assert_eq!(
        runner.state().stack.len(),
        1,
        "the delayed sacrifice trigger is on the stack"
    );
    assert_eq!(runner.state().objects[&token].zone, Zone::Battlefield);
    let events = advance_until_stack_empty_and_collect_events(runner);

    // CR 701.21a and CR 111.7: P0 sacrifices the token, which then ceases to exist.
    assert!(events.iter().any(|event| matches!(
        event, GameEvent::PermanentSacrificed { object_id, player_id: P0 } if *object_id == token
    )), "P0 sacrificed the copy");
    assert!(!runner.state().objects.contains_key(&token));
    assert_eq!(
        runner.state().objects[&fixture.bears].zone,
        Zone::Battlefield
    );
    assert_eq!(
        runner.state().objects[&fixture.balloon_man].zone,
        Zone::Battlefield
    );
    assert!(runner.state().stack.is_empty());
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P0 }
    ));
}

#[test]
fn the_balloon_man_cannot_copy_itself_or_an_opponents_creature() {
    let mut fixture = Fixture::new(Phase::PreCombatMain);
    let activation = fixture.activation();
    fixture
        .runner
        .act(activation)
        .expect("announce the copy ability");
    // CR 115.1c: The live prompt offers targets matching the ability’s description.
    let WaitingFor::TargetSelection {
        player,
        target_slots,
        ..
    } = &fixture.runner.state().waiting_for
    else {
        panic!(
            "expected the live copy target prompt, got {:?}",
            fixture.runner.state().waiting_for
        );
    };
    assert_eq!(*player, P0);
    let [slot] = target_slots.as_slice() else {
        panic!("expected one copy target slot, got {target_slots:?}");
    };
    assert!(slot
        .legal_targets
        .contains(&TargetRef::Object(fixture.bears)));
    // A second legal target keeps the engine from automatically selecting the only option.
    assert!(slot
        .legal_targets
        .contains(&TargetRef::Object(fixture.second_bears)));
    assert!(
        !slot
            .legal_targets
            .contains(&TargetRef::Object(fixture.balloon_man)),
        "another creature excludes the Balloon Man"
    );
    assert!(
        !slot
            .legal_targets
            .contains(&TargetRef::Object(fixture.opposing_bears)),
        "you control excludes the opponent's Bears"
    );

    fixture
        .runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(fixture.bears)),
        })
        .expect("choose P0's Bears");
    assert_eq!(fixture.runner.state().stack.len(), 1);
    fixture.runner.advance_until_stack_empty();
    assert!(fixture.runner.state().stack.is_empty());
    assert!(matches!(
        fixture.runner.state().waiting_for,
        WaitingFor::Priority { player: P0 }
    ));
}

#[test]
fn the_balloon_man_can_only_activate_as_a_sorcery() {
    // CR 602.5d and CR 307.1: Own main phase, priority, and an empty stack.
    let mut main = Fixture::new(Phase::PreCombatMain);
    assert!(
        main.activation_is_offered(),
        "activation is legal in P0's empty main phase"
    );
    tap_land(&mut main.runner, main.island);
    let committed = main.runner.cast(main.opt).commit();
    assert_eq!(committed.state().stack.len(), 1);
    assert_eq!(main.runner.state().priority_player, P0);
    assert!(
        !main.activation_is_offered(),
        "activation is illegal with Opt on the stack"
    );

    let combat = Fixture::new(Phase::BeginCombat);
    assert!(combat.runner.state().stack.is_empty());
    assert!(
        !combat.activation_is_offered(),
        "activation is illegal in beginning of combat"
    );

    let mut opponent_turn = Fixture::new(Phase::PreCombatMain);
    opponent_turn.runner.state_mut().active_player = P1;
    assert!(opponent_turn.runner.state().stack.is_empty());
    assert_eq!(opponent_turn.runner.state().priority_player, P0);
    assert!(
        !opponent_turn.activation_is_offered(),
        "activation is illegal on the opponent's turn"
    );
}
