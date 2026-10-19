//! U7: a population reads one prior announcement, then freezes its recipients.
use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{ContinuousModification, TargetFilter, TargetReadOrigin};
use engine::types::actions::GameAction;
use engine::types::card::CardFace;
use engine::types::events::GameEvent;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::identifiers::{ObjectId, ObjectIncarnationRef};
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const P2: PlayerId = PlayerId(2);
const CRATEROUS: &str = "Craterous Stomp deals 3 damage to target creature an opponent controls. Each other creature that player controls becomes a Coward in addition to its other types and gains \"This creature can't block Giants or Warriors.\"";
const POPULATION: &str = "Each other creature that player controls becomes a Coward in addition to its other types and gains \"This creature can't block Giants or Warriors.\"";
const WAYSTONE: &str = "Attacking tokens you control get +1/+0.\nWhenever you cast your first creature spell each turn, that spell gains mobilize 1.";
const JUMP: &str = "Target creature gains flying until end of turn.";
const WISPS: &str = "Target creature becomes red and gains haste until end of turn. (It can attack and {T} this turn.)\nDraw a card.";

fn supported(text: &str, name: &str, types: &[&str]) {
    let parsed = engine::parser::parse_oracle_text(
        text,
        name,
        &[],
        &types.iter().map(|s| (*s).into()).collect::<Vec<_>>(),
        &[],
    );
    assert!(parsed.parse_warnings.is_empty(), "{name}: {parsed:#?}");
    let face = CardFace {
        abilities: parsed.abilities,
        triggers: parsed.triggers,
        static_abilities: parsed.statics,
        replacements: parsed.replacements,
        ..Default::default()
    };
    assert!(
        !engine::game::coverage::card_face_has_unimplemented_parts(&face),
        "{name}: {face:#?}"
    );
    assert!(
        !face.abilities.is_empty()
            || !face.triggers.is_empty()
            || !face.static_abilities.is_empty()
    );
}

struct Board {
    runner: GameRunner,
    spell: ObjectId,
    second: ObjectId,
    b: ObjectId,
    a: ObjectId,
    d: ObjectId,
    c: ObjectId,
    f: ObjectId,
    other: ObjectId,
    attackers: [ObjectId; 4],
    steal: ObjectId,
    steal_to_caster: ObjectId,
    bounce: ObjectId,
    blink: ObjectId,
    prevention: ObjectId,
    arrival: ObjectId,
    type_change: ObjectId,
}

fn board(text: &str, toughness: i32) -> Board {
    supported(
        text,
        if text == CRATEROUS {
            "Craterous Stomp"
        } else {
            "Synthetic"
        },
        &["Instant"],
    );
    let mut scenario = GameScenario::new_n_player(3, 91);
    scenario.at_phase(Phase::PreCombatMain);
    for player in [P0, P1, P2] {
        scenario.with_library_top(player, &["Plains", "Plains", "Plains", "Plains"]);
        scenario.with_mana_pool(
            player,
            (0..16)
                .map(|_| ManaUnit::new(ManaType::Red, ObjectId(0), false, vec![]))
                .collect(),
        );
    }
    let b = scenario
        .add_creature(P1, "Selected B", 2, toughness)
        .with_subtypes(vec!["Soldier"])
        .id();
    let a = scenario
        .add_creature(P1, "Other A", 2, 8)
        .with_subtypes(vec!["Elf"])
        .id();
    let d = scenario
        .add_creature(P1, "Other D", 2, 8)
        .with_subtypes(vec!["Goblin"])
        .id();
    let c = scenario
        .add_creature(P2, "Later C", 2, 8)
        .with_subtypes(vec!["Wizard"])
        .id();
    let f = scenario.add_creature(P2, "P2 other F", 2, 8).id();
    let other = scenario.add_creature(P0, "Caster creature", 2, 8).id();
    let attackers = [
        scenario
            .add_creature(P0, "Giant only", 1, 8)
            .with_subtypes(vec!["Giant"])
            .id(),
        scenario
            .add_creature(P0, "Warrior only", 1, 8)
            .with_subtypes(vec!["Warrior"])
            .id(),
        scenario
            .add_creature(P0, "Both", 1, 8)
            .with_subtypes(vec!["Giant", "Warrior"])
            .id(),
        scenario
            .add_creature(P0, "Neither", 1, 8)
            .with_subtypes(vec!["Bear"])
            .id(),
    ];
    let spell = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            if text == CRATEROUS {
                "Craterous Stomp"
            } else {
                "Synthetic"
            },
            true,
            text,
        )
        .with_mana_cost(ManaCost::Cost {
            generic: 1,
            shards: vec![ManaCostShard::Red],
        })
        .id();
    let second = scenario
        .add_spell_to_hand_from_oracle(P0, "Craterous Stomp", true, CRATEROUS)
        .id();
    let steal = scenario
        .add_spell_to_hand_from_oracle(
            P2,
            "Synthetic control response",
            true,
            "Gain control of target creature.",
        )
        .id();
    let steal_to_caster = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Synthetic caster control",
            true,
            "Gain control of target creature.",
        )
        .id();
    let bounce = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Synthetic bounce response",
            true,
            "Return target creature to its owner's hand.",
        )
        .id();
    let blink = scenario.add_spell_to_hand_from_oracle(P0, "Synthetic blink", true, "Exile target creature, then return that card to the battlefield under its owner's control.").id();
    let prevention = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Synthetic prevention",
            true,
            "Prevent all damage that would be dealt to target creature this turn.",
        )
        .id();
    let arrival = scenario
        .add_creature_to_hand_from_oracle(P1, "Later arrival", 2, 8, "")
        .id();
    let type_change = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Synthetic attacker type",
            true,
            "Target creature becomes a Giant until end of turn.",
        )
        .id();
    let mut runner = scenario.build();
    // Initial stolen-object fixture: owner, caster and selected controller differ.
    runner.state_mut().objects.get_mut(&b).unwrap().owner = P0;
    Board {
        runner,
        spell,
        second,
        b,
        a,
        d,
        c,
        f,
        other,
        attackers,
        steal,
        steal_to_caster,
        bounce,
        blink,
        prevention,
        arrival,
        type_change,
    }
}

fn coward(state: &GameState, id: ObjectId) -> bool {
    state.objects[&id]
        .card_types
        .subtypes
        .iter()
        .any(|s| s == "Coward")
}

fn assert_granted(state: &GameState, id: ObjectId) {
    assert!(coward(state, id), "recipient must acquire Coward");
    let grants: Vec<_> = state.transient_continuous_effects.iter().filter(|effect| effect.affected == TargetFilter::SpecificObject { id } && effect.modifications.iter().any(|m| matches!(m, ContinuousModification::AddSubtype { subtype } if subtype == "Coward"))).collect();
    assert_eq!(grants.len(), 1);
    assert_eq!(
        grants[0].affected_recipient,
        Some(ObjectIncarnationRef::from_object(&state.objects[&id]))
    );
    assert!(
        grants[0].modifications.len() >= 2,
        "quoted restriction accompanies subtype"
    );
}

fn priority(runner: &mut GameRunner, player: PlayerId) {
    for _ in 0..6 {
        if runner.state().priority_player == player {
            return;
        }
        runner.act(GameAction::PassPriority).unwrap();
    }
    panic!("requested player's priority did not arrive");
}

fn next_main(runner: &mut GameRunner, player: PlayerId) {
    let initial_turn = runner.state().turn_number;
    for _ in 0..400 {
        if runner.state().turn_number > initial_turn
            && runner.state().active_player == player
            && runner.state().phase == Phase::PreCombatMain
            && matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
        {
            return;
        }
        let action = match runner.state().waiting_for {
            WaitingFor::DeclareAttackers { .. } => GameAction::DeclareAttackers {
                attacks: vec![],
                bands: vec![],
            },
            WaitingFor::DeclareBlockers { .. } => GameAction::DeclareBlockers {
                assignments: vec![],
            },
            _ => GameAction::PassPriority,
        };
        runner.act(action).unwrap();
    }
    panic!("next main phase did not arrive");
}

fn block(state: &GameState, blocker: ObjectId, attacker: ObjectId, legal: bool) {
    let defender = state.objects[&blocker].controller;
    let mut runner = GameRunner::from_state(state.clone());
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(attacker, AttackTarget::Player(defender))])
        .unwrap();
    for _ in 0..12 {
        if matches!(
            runner.state().waiting_for,
            WaitingFor::DeclareBlockers { .. }
        ) {
            break;
        }
        runner.act(GameAction::PassPriority).unwrap();
    }
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::DeclareBlockers { .. }
    ));
    let before = runner
        .state()
        .combat
        .as_ref()
        .unwrap()
        .blocker_to_attacker
        .clone();
    let result = runner.act(GameAction::DeclareBlockers {
        assignments: vec![(blocker, attacker)],
    });
    assert_eq!(
        result.is_ok(),
        legal,
        "blocker={blocker:?}, attacker={attacker:?}: {result:?}"
    );
    if legal {
        assert_eq!(
            runner
                .state()
                .combat
                .as_ref()
                .unwrap()
                .blocker_to_attacker
                .get(&blocker),
            Some(&vec![attacker])
        );
    } else {
        assert_eq!(
            runner.state().combat.as_ref().unwrap().blocker_to_attacker,
            before
        );
    }
}

#[test]
fn craterous_stomp_other_creatures_of_target_controller() {
    let mut board = board(CRATEROUS, 8);
    let outcome = board
        .runner
        .cast(board.spell)
        .target_object(board.b)
        .resolve();
    assert_eq!(outcome.state().objects[&board.b].damage_marked, 3);
    for id in [board.a, board.d] {
        assert_granted(outcome.state(), id);
    }
    assert!(outcome.state().objects[&board.a]
        .card_types
        .subtypes
        .iter()
        .any(|s| s == "Elf"));
    assert!(outcome.state().objects[&board.d]
        .card_types
        .subtypes
        .iter()
        .any(|s| s == "Goblin"));
    for id in [board.b, board.other, board.c] {
        assert!(!coward(outcome.state(), id));
    }
}

#[test]
fn craterous_stomp_lethal_and_prevented_damage_keep_grant() {
    let mut lethal = board(CRATEROUS, 2);
    let outcome = lethal
        .runner
        .cast(lethal.spell)
        .target_object(lethal.b)
        .resolve();
    outcome.assert_zone(&[lethal.b], Zone::Graveyard);
    assert!(outcome
        .events()
        .iter()
        .any(|event| matches!(event, GameEvent::DamageDealt { amount: 3, .. })));
    assert_granted(outcome.state(), lethal.a);
    let mut prevented = board(CRATEROUS, 8);
    prevented
        .runner
        .cast(prevented.prevention)
        .target_object(prevented.b)
        .resolve();
    let outcome = prevented
        .runner
        .cast(prevented.spell)
        .target_object(prevented.b)
        .resolve();
    assert_eq!(outcome.state().objects[&prevented.b].damage_marked, 0);
    assert!(outcome
        .events()
        .iter()
        .any(|event| matches!(event, GameEvent::DamagePrevented { amount: 3, .. })));
    assert_granted(outcome.state(), prevented.a);
}

#[test]
fn craterous_restriction_is_giant_or_warrior_blocker_side() {
    let mut board = board(CRATEROUS, 8);
    board
        .runner
        .cast(board.spell)
        .target_object(board.b)
        .resolve();
    // CR 509.1b: either attacker subtype prohibits a block; neither permits it.
    for attacker in board.attackers[..3].iter().copied() {
        block(board.runner.state(), board.a, attacker, false);
    }
    block(board.runner.state(), board.a, board.attackers[3], true);
    block(board.runner.state(), board.b, board.attackers[0], true);
    board
        .runner
        .cast(board.type_change)
        .target_object(board.attackers[3])
        .resolve();
    assert!(board.runner.state().objects[&board.attackers[3]]
        .card_types
        .subtypes
        .iter()
        .any(|subtype| subtype == "Giant"));
    block(board.runner.state(), board.a, board.attackers[3], false);
}

#[test]
fn craterous_controller_is_read_when_instruction_applies() {
    let mut board = board(CRATEROUS, 8);
    let mut committed = board
        .runner
        .cast(board.spell)
        .target_object(board.b)
        .commit();
    for _ in 0..6 {
        if committed.state().priority_player == P2 {
            break;
        }
        committed.act(GameAction::PassPriority).unwrap();
    }
    committed.cast(board.steal).target_object(board.b).commit();
    let outcome = committed.resolve();
    assert_eq!(outcome.state().objects[&board.b].controller, P2);
    assert_eq!(outcome.state().objects[&board.b].damage_marked, 3);
    assert_granted(outcome.state(), board.c);
    assert!(!coward(outcome.state(), board.a));
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let b = scenario.add_creature(P1, "Illegal B", 2, 8).id();
    let a = scenario.add_creature(P1, "Unchanged A", 2, 8).id();
    let stomp = scenario
        .add_spell_to_hand_from_oracle(P0, "Craterous Stomp", true, CRATEROUS)
        .id();
    let response = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Synthetic caster response",
            true,
            "Gain control of target creature.",
        )
        .id();
    let mut illegal = scenario.build();
    let mut committed = illegal.cast(stomp).target_object(b).commit();
    committed.cast(response).target_object(b).commit();
    let outcome = committed.resolve();
    assert_eq!(outcome.state().objects[&b].controller, P0);
    assert_eq!(outcome.state().objects[&b].damage_marked, 0);
    assert!(!coward(outcome.state(), a));
}

#[test]
fn prior_controller_after_same_resolution_move_uses_lki() {
    let text = format!("Exile target creature. {POPULATION}");
    let mut board = board(&text, 8);
    let pin = ObjectIncarnationRef::from_object(&board.runner.state().objects[&board.b]);
    let outcome = board
        .runner
        .cast(board.spell)
        .target_object(board.b)
        .resolve();
    outcome.assert_zone(&[board.b], Zone::Exile);
    assert_ne!(
        outcome.state().objects[&board.b].incarnation,
        pin.incarnation
    );
    assert_granted(outcome.state(), board.a);
    assert!(!coward(outcome.state(), board.other));
}

#[test]
fn prior_controller_selected_illegal_does_not_rebind_to_later_target() {
    let text =
        format!("Tap target creature. {POPULATION} Target creature gets +1/+1 until end of turn.");
    for illegal in [false, true] {
        let mut board = board(&text, 8);
        let mut committed = board
            .runner
            .cast(board.spell)
            .target_objects(&[board.b, board.c])
            .commit();
        if illegal {
            committed.cast(board.bounce).target_object(board.b).commit();
        }
        let outcome = committed.resolve();
        assert_eq!(
            outcome.state().objects[&board.c].power,
            Some(3),
            "later legal target proves resolution"
        );
        if illegal {
            outcome.assert_zone(&[board.b], Zone::Hand);
            assert!(!coward(outcome.state(), board.a));
            assert!(!coward(outcome.state(), board.c));
        } else {
            assert_granted(outcome.state(), board.a);
            assert!(!coward(outcome.state(), board.c));
        }
    }
}

#[test]
fn prior_controller_departed_returned_target_keeps_original_occurrence() {
    // Supported public same-resolution move; forced returned-ID comparison is
    // the local selected-incarnation reader test, not an asserted public return.
    let text = format!("Exile target creature. {POPULATION}");
    let mut board = board(&text, 8);
    let outcome = board
        .runner
        .cast(board.spell)
        .target_object(board.b)
        .resolve();
    outcome.assert_zone(&[board.b], Zone::Exile);
    assert_granted(outcome.state(), board.a);
    assert_eq!(outcome.state().objects[&board.b].owner, P0);
}

#[test]
fn craterous_frozen_recipients_survive_control_changes() {
    let mut board = board(CRATEROUS, 8);
    board
        .runner
        .cast(board.spell)
        .target_object(board.b)
        .resolve();
    priority(&mut board.runner, P2);
    board
        .runner
        .cast(board.steal)
        .target_object(board.a)
        .resolve();
    assert_eq!(board.runner.state().objects[&board.a].controller, P2);
    assert_granted(board.runner.state(), board.a);
    priority(&mut board.runner, P0);
    board
        .runner
        .cast(board.steal_to_caster)
        .target_object(board.b)
        .resolve();
    assert_granted(board.runner.state(), board.d);
    // The arrival is cast during its controller's main phase through public actions.
    next_main(&mut board.runner, P1);
    priority(&mut board.runner, P1);
    board
        .runner
        .cast(board.arrival)
        .resolve()
        .assert_zone(&[board.arrival], Zone::Battlefield);
    assert!(!coward(board.runner.state(), board.arrival));
    assert_granted(board.runner.state(), board.a);
    next_main(&mut board.runner, P0);
    block(board.runner.state(), board.a, board.attackers[0], false);
    block(board.runner.state(), board.a, board.attackers[3], true);
    block(
        board.runner.state(),
        board.arrival,
        board.attackers[0],
        true,
    );
}

#[test]
fn craterous_blinked_recipient_does_not_regain_grant() {
    let mut board = board(CRATEROUS, 8);
    board
        .runner
        .cast(board.spell)
        .target_object(board.b)
        .resolve();
    let old = ObjectIncarnationRef::from_object(&board.runner.state().objects[&board.a]);
    let outcome = board
        .runner
        .cast(board.blink)
        .target_object(board.a)
        .resolve();
    outcome.assert_zone(&[board.a], Zone::Battlefield);
    assert_ne!(
        outcome.state().objects[&board.a].incarnation,
        old.incarnation
    );
    assert!(!coward(outcome.state(), board.a));
    assert_granted(outcome.state(), board.d);
    block(outcome.state(), board.a, board.attackers[0], true);
    block(outcome.state(), board.d, board.attackers[0], false);
}

#[test]
fn craterous_permanent_duration_reload_and_second_source() {
    let mut board = board(CRATEROUS, 8);
    board
        .runner
        .cast(board.spell)
        .target_object(board.b)
        .resolve();
    next_main(&mut board.runner, P0);
    assert_granted(board.runner.state(), board.a);
    let saved = serde_json::to_vec(board.runner.state()).unwrap();
    let loaded: GameState = serde_json::from_slice(&saved).unwrap();
    assert_granted(&loaded, board.a);
    block(&loaded, board.a, board.attackers[0], false);
    block(&loaded, board.a, board.attackers[3], true);
    block(&loaded, board.b, board.attackers[0], true);
    board.runner = GameRunner::from_state(loaded);
    priority(&mut board.runner, P0);
    board
        .runner
        .cast(board.second)
        .target_object(board.c)
        .resolve();
    assert_granted(board.runner.state(), board.f);
    assert_granted(board.runner.state(), board.a);
    assert_granted(board.runner.state(), board.d);
    assert!(!coward(board.runner.state(), board.b));
}

#[test]
fn prior_controller_legacy_and_missing_authority() {
    let mut board = board(CRATEROUS, 8);
    board
        .runner
        .cast(board.spell)
        .target_object(board.b)
        .resolve();
    let current: GameState =
        serde_json::from_value(serde_json::to_value(board.runner.state()).unwrap()).unwrap();
    assert_granted(&current, board.a);
    let parsed = engine::parser::parse_oracle_text(
        CRATEROUS,
        "Craterous Stomp",
        &[],
        &["Instant".into()],
        &[],
    );
    let mut legacy_ability =
        serde_json::to_value(parsed.abilities[0].sub_ability.as_ref().unwrap()).unwrap();
    assert_eq!(
        legacy_ability
            .as_object_mut()
            .unwrap()
            .remove("target_reads"),
        Some(serde_json::json!("ParentAnnouncement"))
    );
    let legacy: engine::types::ability::AbilityDefinition =
        serde_json::from_value(legacy_ability).unwrap();
    assert_eq!(legacy.target_reads, TargetReadOrigin::OwnAnnouncement);
    let mut encoded = serde_json::to_value(&current.transient_continuous_effects[0]).unwrap();
    assert!(encoded
        .as_object_mut()
        .unwrap()
        .remove("affected_recipient")
        .is_some());
    let legacy: engine::types::game_state::TransientContinuousEffect =
        serde_json::from_value(encoded).unwrap();
    assert!(legacy.affected_recipient.is_none());
    // Missing/illegal modern occurrences are discriminated in the local reader
    // and filter tests; legacy absence does not manufacture a historical pin.
    priority(&mut board.runner, P0);
    board
        .runner
        .cast(board.second)
        .target_object(board.c)
        .resolve();
    assert_granted(board.runner.state(), board.a);

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let creature = scenario
        .add_creature(P0, "Legacy unrelated recipient", 2, 8)
        .id();
    let jump = scenario
        .add_spell_to_hand_from_oracle(P0, "Jump", true, JUMP)
        .id();
    let ordinary = scenario.build();
    let loaded: GameState =
        serde_json::from_value(serde_json::to_value(ordinary.state()).unwrap()).unwrap();
    let mut loaded = GameRunner::from_state(loaded);
    loaded.cast(jump).target_object(creature).resolve();
    assert!(engine::game::keywords::has_keyword(
        &loaded.state().objects[&creature],
        &Keyword::Flying
    ));
}

#[test]
fn continuous_grant_duration_and_static_siblings() {
    supported(JUMP, "Jump", &["Instant"]);
    supported(WISPS, "Crimson Wisps", &["Instant"]);
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Plains", "Plains", "Plains"]);
    scenario.with_library_top(P1, &["Plains", "Plains", "Plains"]);
    let recipient = scenario.add_creature(P0, "Temporary recipient", 2, 8).id();
    let jump = scenario
        .add_spell_to_hand_from_oracle(P0, "Jump", true, JUMP)
        .id();
    let wisps = scenario
        .add_spell_to_hand_from_oracle(P0, "Crimson Wisps", true, WISPS)
        .id();
    let mut runner = scenario.build();
    runner.cast(jump).target_object(recipient).resolve();
    assert!(engine::game::keywords::has_keyword(
        &runner.state().objects[&recipient],
        &Keyword::Flying
    ));
    runner
        .cast(wisps)
        .target_object(recipient)
        .resolve()
        .assert_hand_drawn(P0, 1);
    assert!(engine::game::keywords::has_keyword(
        &runner.state().objects[&recipient],
        &Keyword::Haste
    ));
    assert!(runner.state().objects[&recipient]
        .color
        .contains(&ManaColor::Red));
    next_main(&mut runner, P0);
    assert!(!engine::game::keywords::has_keyword(
        &runner.state().objects[&recipient],
        &Keyword::Flying
    ));
    assert!(!engine::game::keywords::has_keyword(
        &runner.state().objects[&recipient],
        &Keyword::Haste
    ));
    assert!(!runner.state().objects[&recipient]
        .color
        .contains(&ManaColor::Red));
}

#[test]
fn prior_controller_recipient_pin_policy_preserves_waystone_carry() {
    supported(WAYSTONE, "Waystone's Guidance", &["Enchantment"]);
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_enchantment_from_oracle(P0, "Waystone's Guidance", WAYSTONE);
    let creature = scenario
        .add_creature_to_hand_from_oracle(P0, "First creature", 2, 2, "")
        .id();
    let mut runner = scenario.build();
    runner.cast(creature).commit();
    assert_eq!(
        runner.state().stack.len(),
        2,
        "actual cast trigger above creature spell"
    );
    runner.resolve_top();
    assert_eq!(runner.state().objects[&creature].zone, Zone::Stack);
    let mobilize = Keyword::Mobilize(engine::types::ability::QuantityExpr::Fixed { value: 1 });
    assert!(runner.state().objects[&creature]
        .keywords
        .contains(&mobilize));
    let grants: Vec<_> = runner
        .state()
        .transient_continuous_effects
        .iter()
        .filter(|grant| grant.affected == TargetFilter::SpecificObject { id: creature })
        .collect();
    assert!(!grants.is_empty());
    assert!(grants
        .iter()
        .all(|grant| grant.affected_recipient.is_none()));
    runner.advance_until_stack_empty();
    assert_eq!(runner.state().objects[&creature].zone, Zone::Battlefield);
    // CR 400.7a: the stack characteristic grant carries onto the permanent.
    assert!(runner.state().objects[&creature]
        .keywords
        .contains(&mobilize));
    assert!(runner
        .state()
        .transient_continuous_effects
        .iter()
        .filter(|grant| grant.affected == TargetFilter::SpecificObject { id: creature })
        .all(|grant| grant.affected_recipient.is_none()));
}

#[test]
fn prior_controller_gornog_static_direction_control() {
    let oracle = "Haste\nCowards can't block Warriors.\nWhenever one or more Warriors you control attack a player, target creature that player controls becomes a Coward.\nAttacking Warriors you control get +X/+0, where X is the number of Cowards your opponents control.";
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P1, "Gornog, the Red Reaper", 2, 3, oracle);
    let coward = scenario
        .add_creature(P1, "Coward blocker", 2, 8)
        .with_subtypes(vec!["Coward"])
        .id();
    let soldier = scenario
        .add_creature(P1, "Soldier blocker", 2, 8)
        .with_subtypes(vec!["Soldier"])
        .id();
    let warrior = scenario
        .add_creature(P0, "Warrior attacker", 1, 8)
        .with_subtypes(vec!["Warrior"])
        .id();
    let bear = scenario
        .add_creature(P0, "Bear attacker", 1, 8)
        .with_subtypes(vec!["Bear"])
        .id();
    let runner = scenario.build();
    // Only the second printed Gornog ability is the derived reference here.
    block(runner.state(), coward, warrior, false);
    block(runner.state(), soldier, warrior, true);
    block(runner.state(), coward, bear, true);
}
