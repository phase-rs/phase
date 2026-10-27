//! Integration tests for curse cards with spell-cast triggers.
//!
//! Covers 4 curses that trigger when enchanted player casts a spell:
//!   - Curse of Vengeance (put a spite counter on it)
//!   - Curse of Silence (chosen name costs {2} more; draw when cast)
//!   - Curse of Echoes (each other player may copy instant/sorcery)
//!   - Maddening Hex (roll d6, deal damage, reattach)
//!
//! Each test verifies at minimum that the spell-cast trigger fires (stack count
//! assertion). For Curse of Vengeance, counter placement is also verified.
//!
//! CR references:
//!   - CR 303.4b: An Aura that enchants a player is attached to that player.
//!   - CR 601.2a: A player casts a spell by announcing it and moving it to the
//!     stack.

use engine::game::effects::attach::attach_to_player;
use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::trigger_index::reindex_object_triggers;
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;

// ---------------------------------------------------------------------------
// Oracle texts
// ---------------------------------------------------------------------------

const CURSE_OF_VENGEANCE: &str =
    "Enchant player\nWhenever enchanted player casts a spell, put a spite counter on this Aura.\n\
     When enchanted player loses the game, you gain X life and draw X cards, where X is the number of spite counters on this Aura.";

const CURSE_OF_ECHOES: &str =
    "Enchant player\nWhenever enchanted player casts an instant or sorcery spell, each other player may copy that spell and may choose new targets for the copy they control.";

const MADDENING_HEX: &str =
    "Enchant player\nWhenever enchanted player casts a noncreature spell, roll a d6. This Aura deals damage to that player equal to the result. Then attach this Aura to another one of your opponents chosen at random.";

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn mana(color: ManaType) -> ManaUnit {
    ManaUnit::new(color, ObjectId(0), false, vec![])
}

/// Build a curse on the battlefield under P0's control, attached to P1.
/// P1 has a spell in hand and mana to cast it.
/// Returns (runner, curse_id, spell_id, target_id).
fn setup_spell_cast_curse(oracle: &str, name: &str) -> (GameRunner, ObjectId, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let curse_id = {
        let mut builder = scenario.add_creature_from_oracle(P0, name, 0, 0, oracle);
        builder.as_enchantment();
        builder.with_subtypes(vec!["Aura", "Curse"]);
        builder.id()
    };

    // Give P1 a cheap spell to cast (Lightning Bolt equivalent — instant, {R}).
    let spell_id = scenario.add_bolt_to_hand(P1);

    // P1 needs a target for the bolt.
    let dummy = scenario.add_creature(P0, "Memnite", 1, 1).id();

    // Mana for P1 to cast the spell.
    scenario.with_mana_pool(P1, vec![mana(ManaType::Red)]);

    // Library padding.
    for _ in 0..10 {
        scenario.add_card_to_library_top(P0, "Plains");
        scenario.add_card_to_library_top(P1, "Plains");
    }

    let mut runner = scenario.build();

    // P1 is the active player (their main phase).
    runner.state_mut().active_player = P1;
    runner.state_mut().priority_player = P1;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P1 };

    // Attach the curse to P1.
    attach_to_player(runner.state_mut(), curse_id, P1);
    evaluate_layers(runner.state_mut());
    reindex_object_triggers(runner.state_mut(), curse_id);

    (runner, curse_id, spell_id, dummy)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// Curse of Vengeance: when enchanted player casts a spell, put a spite counter
/// on the curse.
#[test]
fn curse_of_vengeance_fires_on_spell_cast() {
    let (mut runner, curse_id, spell_id, target) =
        setup_spell_cast_curse(CURSE_OF_VENGEANCE, "Curse of Vengeance");

    // Cast the bolt — target the dummy creature.
    runner.cast(spell_id).target_object(target).resolve();

    // After resolution, the trigger should have fired and resolved (putting a
    // spite counter on the curse). Check the counter.
    let counters = runner
        .state()
        .objects
        .get(&curse_id)
        .map(|obj| {
            obj.counters
                .get(&engine::types::counter::CounterType::Generic(
                    "spite".to_string(),
                ))
                .copied()
                .unwrap_or(0)
        })
        .unwrap_or(0);

    assert!(
        counters >= 1,
        "Curse of Vengeance must have at least 1 spite counter after enchanted player casts a spell (got {counters})"
    );
}

/// Curse of Echoes: trigger fires when enchanted player casts an instant/sorcery.
/// We use commit() to inspect the stack before resolution and verify the curse
/// placed a trigger from its source_id.
#[test]
fn curse_of_echoes_fires_on_instant_cast() {
    let (mut runner, curse_id, spell_id, target) =
        setup_spell_cast_curse(CURSE_OF_ECHOES, "Curse of Echoes");

    // Cast the bolt and inspect the stack before resolution.
    let commit = runner.cast(spell_id).target_object(target).commit();

    // The commit driver passes priority until the spell is committed; at that
    // point the cast-trigger from Curse of Echoes should already be on the
    // stack (CR 601.2a: triggers fire on cast announcement).
    let has_curse_trigger = commit
        .state()
        .stack
        .iter()
        .any(|entry| entry.source_id == curse_id);

    assert!(
        has_curse_trigger,
        "Curse of Echoes must put a trigger from the curse source on the stack \
         when enchanted player casts an instant or sorcery"
    );
}

/// Maddening Hex: trigger fires when enchanted player casts a noncreature spell.
/// The trigger involves "roll a d6" and "attach to another opponent" which
/// surfaces a TriggerTargetSelection prompt the cast driver can't auto-resolve.
/// We manually drive the cast and verify the trigger appears on the stack.
#[test]
fn maddening_hex_fires_on_noncreature_spell_cast() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let curse_id = {
        let mut builder =
            scenario.add_creature_from_oracle(P0, "Maddening Hex", 0, 0, MADDENING_HEX);
        builder.as_enchantment();
        builder.with_subtypes(vec!["Aura", "Curse"]);
        builder.id()
    };

    // Use a no-target sorcery.
    let spell_id = scenario.add_spell_to_hand(P1, "Explore", false).id();

    scenario.with_mana_pool(P1, vec![mana(ManaType::Green), mana(ManaType::Colorless)]);

    for _ in 0..10 {
        scenario.add_card_to_library_top(P0, "Plains");
        scenario.add_card_to_library_top(P1, "Island");
    }

    let mut runner = scenario.build();
    runner.state_mut().active_player = P1;
    runner.state_mut().priority_player = P1;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P1 };

    attach_to_player(runner.state_mut(), curse_id, P1);
    evaluate_layers(runner.state_mut());
    reindex_object_triggers(runner.state_mut(), curse_id);

    // Manually cast the spell.
    let card_id = runner.state().objects[&spell_id].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell_id,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("CastSpell must succeed");

    // Drive until we hit a priority window or TriggerTargetSelection.
    for _ in 0..30 {
        match &runner.state().waiting_for {
            WaitingFor::Priority { .. } => {
                // Spell is on the stack; pass priority to let triggers fire.
                let _ = runner.act(GameAction::PassPriority);
            }
            _ => break,
        }
    }

    // The trigger should have fired — check that the stack has a trigger from the curse,
    // or we've hit TriggerTargetSelection (which means the trigger is being processed).
    let trigger_on_stack = runner.state().stack.iter().any(|e| e.source_id == curse_id);
    let at_trigger_target = matches!(
        &runner.state().waiting_for,
        WaitingFor::TriggerTargetSelection { .. }
    );

    assert!(
        trigger_on_stack || at_trigger_target,
        "Maddening Hex must trigger when enchanted player casts a noncreature spell"
    );
}

/// Maddening Hex must not trigger for a matching spell cast by anyone other
/// than the enchanted player.
#[test]
fn maddening_hex_does_not_fire_for_non_enchanted_player_spell_cast() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let curse_id = {
        let mut builder =
            scenario.add_creature_from_oracle(P0, "Maddening Hex", 0, 0, MADDENING_HEX);
        builder.as_enchantment();
        builder.with_subtypes(vec!["Aura", "Curse"]);
        builder.id()
    };

    let spell_id = scenario.add_spell_to_hand(P0, "Explore", false).id();
    scenario.with_mana_pool(P0, vec![mana(ManaType::Green), mana(ManaType::Colorless)]);

    for _ in 0..10 {
        scenario.add_card_to_library_top(P0, "Plains");
        scenario.add_card_to_library_top(P1, "Island");
    }

    let mut runner = scenario.build();
    runner.state_mut().active_player = P0;
    runner.state_mut().priority_player = P0;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P0 };

    attach_to_player(runner.state_mut(), curse_id, P1);
    evaluate_layers(runner.state_mut());
    reindex_object_triggers(runner.state_mut(), curse_id);

    let card_id = runner.state().objects[&spell_id].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell_id,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("CastSpell must succeed");

    assert!(
        runner
            .state()
            .stack
            .iter()
            .all(|entry| entry.source_id != curse_id),
        "Maddening Hex must not trigger for a spell cast by the non-enchanted player"
    );
}

const HEX_P2: engine::types::player::PlayerId = engine::types::player::PlayerId(2);
const HEX_P3: engine::types::player::PlayerId = engine::types::player::PlayerId(3);

// CR 608.2c keeps die, damage and attachment in printed order; CR 109.5 and
// CR 113.8 bind each triggered ability's controller independently of later
// host/control changes. CR 701.3a-b requires a legal player host and completes
// same-host attachment without movement. CR 702.16c applies live protection,
// and CR 303.4c retires an Aura whose enchanted player leaves the game.

struct HexGame {
    runner: GameRunner,
    aura: ObjectId,
    spells: Vec<ObjectId>,
    other_aura: ObjectId,
    foreign_spell: ObjectId,
    creature_spell: ObjectId,
    protection: ObjectId,
}

fn hex_game(players: u8) -> HexGame {
    use engine::types::mana::{ManaCost, ManaCostShard};
    let mut scenario = GameScenario::new_n_player(players, 44);
    scenario.at_phase(Phase::PreCombatMain);
    let aura = scenario
        .add_spell_to_hand_from_oracle(P0, "Maddening Hex", false, MADDENING_HEX)
        .as_enchantment()
        .with_subtypes(vec!["Aura", "Curse"])
        .with_mana_cost(ManaCost::Cost {
            generic: 1,
            shards: vec![ManaCostShard::Red, ManaCostShard::Red],
        })
        .id();
    // This is an explicitly constructed, inert enchant-player Aura, not a
    // second printed Hex. It must never borrow the resolving Aura's identity.
    let other_aura = scenario
        .add_enchantment_from_oracle(P0, "Inert Aura witness", "Enchant player")
        .with_subtypes(vec!["Aura"])
        .id();
    let spells = (0..3)
        .map(|_| {
            scenario
                .add_spell_to_hand_from_oracle(
                    P1,
                    "Noncreature cast witness",
                    true,
                    "You gain 0 life.",
                )
                .with_mana_cost(ManaCost::generic(1))
                .id()
        })
        .collect();
    let foreign_spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Foreign caster witness", true, "You gain 0 life.")
        .with_mana_cost(ManaCost::generic(1))
        .id();
    let creature_spell = scenario
        .add_creature_to_hand(P1, "Creature cast witness", 2, 2)
        .with_mana_cost(ManaCost::generic(1))
        .id();
    let protection = scenario
        .add_enchantment_from_oracle(
            if players > 2 { HEX_P2 } else { P1 },
            "Synthetic protection witness",
            "You have protection from red.",
        )
        .id();
    scenario.with_mana_pool(
        P0,
        vec![
            mana(ManaType::Colorless),
            mana(ManaType::Red),
            mana(ManaType::Red),
        ],
    );
    scenario.with_mana_pool(P1, vec![mana(ManaType::Colorless); 3]);
    let mut runner = scenario.build();
    engine::game::zones::move_to_zone(
        runner.state_mut(),
        protection,
        engine::types::zones::Zone::Exile,
        &mut vec![],
    );
    attach_to_player(runner.state_mut(), other_aura, P1);
    let outcome = runner.cast(aura).target_player(P1).resolve();
    outcome.assert_zone(&[aura], engine::types::zones::Zone::Battlefield);
    assert_eq!(
        runner.state().objects[&aura].attached_to,
        Some(engine::game::game_object::AttachTarget::Player(P1))
    );
    assert_eq!(
        runner.state().players[0].mana_pool.total(),
        0,
        "actual 1RR paid"
    );
    assert!(runner.state().stack.is_empty());
    assert!(runner.state().resolution_stack.is_empty());
    HexGame {
        runner,
        aura,
        spells,
        other_aura,
        foreign_spell,
        creature_spell,
        protection,
    }
}

fn hex_priority(game: &mut HexGame, player: engine::types::player::PlayerId) {
    game.runner.state_mut().active_player = player;
    game.runner.state_mut().priority_player = player;
    game.runner.state_mut().waiting_for = WaitingFor::Priority { player };
}

fn hex_cast(game: &mut HexGame, index: usize) {
    hex_priority(game, P1);
    game.runner.cast(game.spells[index]).commit();
    assert!(
        matches!(game.runner.state().waiting_for, WaitingFor::Priority { .. }),
        "host choice is at resolution, never target announcement"
    );
    assert!(
        game.runner
            .state()
            .stack
            .iter()
            .any(|entry| entry.source_id == game.aura),
        "actual spell emits Hex trigger"
    );
}

fn hex_pin(game: &mut HexGame, seed: u64) {
    game.runner.state_mut().rng_seed = seed;
    game.runner.state_mut().rng_word_pos = 0;
    game.runner.state_mut().rng = engine::types::game_state::GameRng::seed_from_u64(seed);
}

fn hex_one_trigger(game: &mut HexGame) -> Vec<engine::types::events::GameEvent> {
    let before = game
        .runner
        .state()
        .stack
        .iter()
        .filter(|entry| entry.source_id == game.aura)
        .count();
    assert!(before > 0);
    let mut events = vec![];
    for _ in 0..32 {
        if game
            .runner
            .state()
            .stack
            .iter()
            .filter(|entry| entry.source_id == game.aura)
            .count()
            < before
        {
            break;
        }
        assert!(matches!(
            game.runner.state().waiting_for,
            WaitingFor::Priority { .. }
        ));
        events.extend(
            game.runner
                .act(GameAction::PassPriority)
                .expect("resolve one actual Hex trigger")
                .events,
        );
    }
    assert_eq!(
        game.runner
            .state()
            .stack
            .iter()
            .filter(|entry| entry.source_id == game.aura)
            .count(),
        before - 1
    );
    events
}

fn hex_damage(
    events: &[engine::types::events::GameEvent],
    aura: ObjectId,
    caster: engine::types::player::PlayerId,
) -> u8 {
    use engine::types::ability::TargetRef;
    use engine::types::events::GameEvent;
    let rolls: Vec<_> = events
        .iter()
        .enumerate()
        .filter_map(|(index, event)| match event {
            GameEvent::DieRolled {
                sides: 6,
                result: Some(result),
                ..
            } => Some((index, *result)),
            _ => None,
        })
        .collect();
    assert_eq!(rolls.len(), 1, "exactly one actual d6");
    let (roll_index, result) = rolls[0];
    assert!((1..=6).contains(&result));
    let damage: Vec<_> = events
        .iter()
        .enumerate()
        .filter_map(|(index, event)| match event {
            GameEvent::DamageDealt {
                source_id,
                target,
                amount,
                ..
            } if *source_id == aura => Some((index, target, *amount)),
            _ => None,
        })
        .collect();
    assert_eq!(damage.len(), 1);
    assert_eq!(damage[0].1, &TargetRef::Player(caster));
    assert_eq!(damage[0].2, u32::from(result));
    assert!(roll_index < damage[0].0, "written die then damage order");
    result
}

fn hex_complete(game: &mut HexGame) {
    game.runner.advance_until_stack_empty();
    assert!(game.runner.state().stack.is_empty());
    assert!(game.runner.state().resolution_stack.is_empty());
    assert!(matches!(
        game.runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    assert_eq!(
        game.runner.state().objects[&game.other_aura].attached_to,
        Some(engine::game::game_object::AttachTarget::Player(P1))
    );
}

#[test]
fn maddening_paid_aura_die_damage_then_typed_player_move_and_exact_journal_replay() {
    use engine::game::game_object::AttachTarget;
    use engine::types::game_state::RandomDraw;
    use engine::types::resolved_commands::ResolvedRulesCommand;
    use rand::{seq::IndexedRandom, Rng};
    for players in [2, 3, 4] {
        let mut game = hex_game(players);
        hex_cast(&mut game, 0);
        hex_pin(&mut game, 73);
        let prefix = game.runner.state().clone();
        let mut expected_rng = prefix.rng.clone();
        let expected_die: u8 = expected_rng.draw(RandomDraw::Outcome).random_range(1..=6);
        let eligible = if players == 4 {
            vec![HEX_P2, HEX_P3]
        } else if players == 3 {
            vec![HEX_P2]
        } else {
            vec![]
        };
        let expected_host = if eligible.is_empty() {
            P1
        } else {
            *eligible
                .choose(expected_rng.draw(RandomDraw::Outcome))
                .unwrap()
        };
        let journal_start = prefix.resolved_rules_journal.entries().len();
        let events = hex_one_trigger(&mut game);
        let result = hex_damage(&events, game.aura, P1);
        assert_eq!(result, expected_die);
        assert_eq!(game.runner.state().players[1].life, 20 - i32::from(result));
        let host = game.runner.state().objects[&game.aura].attached_to;
        assert_eq!(host, Some(AttachTarget::Player(expected_host)));
        let mut actual_rng = game.runner.state().rng.clone();
        assert_eq!(
            actual_rng.draw(RandomDraw::Outcome).random::<u64>(),
            expected_rng.draw(RandomDraw::Outcome).random::<u64>(),
            "one die draw and exactly one nonempty host selection"
        );
        if players == 2 {
            assert_eq!(
                host,
                Some(AttachTarget::Player(P1)),
                "no other opponent completes without moving"
            );
            assert!(events.iter().all(|event| !matches!(event, engine::types::events::GameEvent::Unattached { attachment_id, .. } if *attachment_id == game.aura)));
        } else {
            let Some(AttachTarget::Player(host)) = host else {
                panic!("ordinary typed player host");
            };
            assert!(if players == 3 {
                host == HEX_P2
            } else {
                [HEX_P2, HEX_P3].contains(&host)
            });
            let unattached: Vec<_> = events
                .iter()
                .enumerate()
                .filter_map(|(index, event)| match event {
                    engine::types::events::GameEvent::Unattached {
                        attachment_id,
                        old_target,
                    } if *attachment_id == game.aura => Some((index, old_target)),
                    _ => None,
                })
                .collect();
            assert_eq!(unattached.len(), 1);
            assert_eq!(
                unattached[0].1,
                &engine::types::ability::TargetRef::Player(P1)
            );
            let damage_index = events.iter().position(|event| matches!(event, engine::types::events::GameEvent::DamageDealt { source_id, .. } if *source_id == game.aura)).unwrap();
            assert!(damage_index < unattached[0].0);
            assert_eq!(events.iter().filter(|event| matches!(event, engine::types::events::GameEvent::EffectResolved { kind: engine::types::ability::EffectKind::Attach, source_id, subject: Some(_), .. } if *source_id == game.aura)).count(), 1);
            let edits: Vec<_> = game
                .runner
                .state()
                .resolved_rules_journal
                .entries()
                .iter()
                .skip(journal_start)
                .filter_map(|entry| match entry.command.as_ref()? {
                    ResolvedRulesCommand::Attachment(command)
                        if command.attachment.object_id == game.aura =>
                    {
                        Some(command.clone())
                    }
                    _ => None,
                })
                .collect();
            assert_eq!(edits.len(), 1);
            assert_eq!(edits[0].expected_old_host, Some(AttachTarget::Player(P1)));
            assert_eq!(edits[0].resulting_host, Some(AttachTarget::Player(host)));
            let mut replay = prefix;
            let journal = replay.resolved_rules_journal.clone();
            let mut before_rng = replay.rng.clone();
            engine::game::effects::attach::apply_resolved_attachment(&mut replay, &edits[0])
                .expect("exact ordinary movement replay");
            assert_eq!(
                replay.objects[&game.aura].attached_to,
                Some(AttachTarget::Player(host))
            );
            assert_eq!(
                replay.objects[&game.aura].timestamp,
                game.runner.state().objects[&game.aura].timestamp
            );
            assert_eq!(replay.resolved_rules_journal, journal);
            assert_eq!(
                replay
                    .rng
                    .draw(engine::types::game_state::RandomDraw::Outcome)
                    .random::<u64>(),
                before_rng
                    .draw(engine::types::game_state::RandomDraw::Outcome)
                    .random::<u64>()
            );
        }
        hex_complete(&mut game);
    }
}

#[test]
fn maddening_two_pending_triggers_exclude_event_caster_even_after_newer_move() {
    use engine::game::game_object::AttachTarget;
    use engine::types::game_state::{GameRng, RandomDraw};
    use rand::{seq::IndexedRandom, Rng};
    for players in [3, 4] {
        // Enumerating seeds here is a deterministic test fixture calculation,
        // not an engine probe: the two draws model the specified d6 and host.
        for second_host in if players == 4 {
            vec![HEX_P2, HEX_P3]
        } else {
            vec![HEX_P2]
        } {
            let mut game = hex_game(players);
            hex_cast(&mut game, 0);
            hex_cast(&mut game, 1);
            assert_eq!(
                game.runner
                    .state()
                    .stack
                    .iter()
                    .filter(|entry| entry.source_id == game.aura)
                    .count(),
                2
            );
            let eligible = if players == 4 {
                vec![HEX_P2, HEX_P3]
            } else {
                vec![HEX_P2]
            };
            let seed = (0..100_000)
                .find(|seed| {
                    let mut rng = GameRng::seed_from_u64(*seed);
                    let _: u8 = rng.draw(RandomDraw::Outcome).random_range(1..=6);
                    let first = eligible.choose(rng.draw(RandomDraw::Outcome)).copied();
                    let _: u8 = rng.draw(RandomDraw::Outcome).random_range(1..=6);
                    let second = eligible.choose(rng.draw(RandomDraw::Outcome)).copied();
                    first == Some(HEX_P2) && second == Some(second_host)
                })
                .expect("pin nondegenerate prescribed host sequence");
            hex_pin(&mut game, seed);
            let newer = hex_one_trigger(&mut game);
            let first_damage = hex_damage(&newer, game.aura, P1);
            assert_eq!(
                game.runner.state().objects[&game.aura].attached_to,
                Some(AttachTarget::Player(HEX_P2))
            );
            let older = hex_one_trigger(&mut game);
            let second_damage = hex_damage(&older, game.aura, P1);
            assert_eq!(
                game.runner.state().objects[&game.aura].attached_to,
                Some(AttachTarget::Player(second_host))
            );
            assert_eq!(
                game.runner.state().players[1].life,
                20 - i32::from(first_damage) - i32::from(second_damage)
            );
            assert_eq!(
                game.runner.state().players[2].life,
                20,
                "later host never replaces captured damage recipient"
            );
            hex_complete(&mut game);
        }
    }
}

#[test]
fn maddening_captured_ability_controller_and_aura_birth_do_not_follow_later_source() {
    use engine::game::game_object::AttachTarget;
    use engine::game::zones::move_to_zone;
    use engine::types::zones::Zone;
    for variant in [0, 1, 2] {
        let mut game = hex_game(3);
        hex_cast(&mut game, 0);
        if variant != 0 {
            move_to_zone(
                game.runner.state_mut(),
                game.aura,
                Zone::Graveyard,
                &mut vec![],
            );
            if variant == 1 {
                move_to_zone(
                    game.runner.state_mut(),
                    game.aura,
                    Zone::Battlefield,
                    &mut vec![],
                );
                attach_to_player(game.runner.state_mut(), game.aura, P1);
            }
        } else {
            game.runner
                .state_mut()
                .objects
                .get_mut(&game.aura)
                .unwrap()
                .controller = HEX_P2;
            game.runner
                .state_mut()
                .objects
                .get_mut(&game.aura)
                .unwrap()
                .base_controller = Some(HEX_P2);
            evaluate_layers(game.runner.state_mut());
        }
        let events = hex_one_trigger(&mut game);
        let damage = hex_damage(&events, game.aura, P1);
        assert_eq!(game.runner.state().players[1].life, 20 - i32::from(damage));
        // CR 109.5, CR 113.8, CR 400.7: old ability keeps controller A;
        // a new Aura birth cannot be moved by the old source reference.
        assert_eq!(
            game.runner.state().objects[&game.aura].attached_to,
            match variant {
                0 => Some(AttachTarget::Player(HEX_P2)),
                1 => Some(AttachTarget::Player(P1)),
                _ => None,
            }
        );
        hex_complete(&mut game);
    }
}

#[test]
fn maddening_creature_and_nonenchanted_cast_negatives_have_real_triggering_positive() {
    let mut game = hex_game(3);
    // Two real spells that should not fire Hex, followed by one that must.
    game.runner.state_mut().players[0]
        .mana_pool
        .add(mana(ManaType::Colorless));
    hex_priority(&mut game, P0);
    let outcome = game.runner.cast(game.foreign_spell).resolve();
    assert!(outcome
        .events()
        .iter()
        .all(|event| !matches!(event, engine::types::events::GameEvent::DieRolled { .. })));
    assert_eq!(game.runner.state().players[1].life, 20);
    hex_priority(&mut game, P1);
    let outcome = game.runner.cast(game.creature_spell).resolve();
    outcome.assert_zone(
        &[game.creature_spell],
        engine::types::zones::Zone::Battlefield,
    );
    assert!(outcome
        .events()
        .iter()
        .all(|event| !matches!(event, engine::types::events::GameEvent::DieRolled { .. })));
    assert_eq!(game.runner.state().players[1].life, 20);
    hex_cast(&mut game, 0);
    let events = hex_one_trigger(&mut game);
    let result = hex_damage(&events, game.aura, P1);
    assert_eq!(game.runner.state().players[1].life, 20 - i32::from(result));
    hex_complete(&mut game);
}

fn hex_attach_replacement(game: &mut HexGame, optional: bool, second: bool) {
    use engine::types::ability::ReplacementMode;
    // Synthetic Aura/player-host composition of the established Psychic Paper
    // as-attached choice class. The printed Hex fixture above stays complete.
    let text = if second {
        "As this Aura becomes attached to a player, choose a color."
    } else {
        "As this Aura becomes attached to a player, choose a creature card name and a creature type."
    };
    let parsed = engine::parser::oracle::parse_oracle_text(
        text,
        "Synthetic attached-choice rider",
        &[],
        &["Enchantment".into()],
        &["Aura".into()],
    );
    assert_eq!(
        parsed.replacements.len(),
        1,
        "positive complete replacement fixture reach"
    );
    let mut definition = parsed.replacements[0].clone();
    if optional {
        definition.mode = ReplacementMode::Optional { decline: None };
    }
    let object = game.runner.state_mut().objects.get_mut(&game.aura).unwrap();
    object.replacement_definitions.push(definition.clone());
    std::sync::Arc::make_mut(&mut object.base_replacement_definitions).push(definition);
    game.runner.state_mut().all_card_names =
        vec!["Alpha witness".into(), "Nondefault witness".into()].into();
}

fn hex_restore(game: &mut HexGame, persisted: bool) {
    use engine::types::game_state::PersistedGameState;
    use engine::types::resolution::ResolutionStateWire;
    game.runner.state_mut().capture_rng_word_pos();
    let state = if persisted {
        let value =
            serde_json::to_value(PersistedGameState::capture(game.runner.state().clone())).unwrap();
        serde_json::from_value::<PersistedGameState>(value)
            .expect("authoritative persisted pause")
            .into_game_state()
            .expect("checked persisted game")
    } else {
        let value = serde_json::to_value(ResolutionStateWire::from_game_state(
            game.runner.state().clone(),
        ))
        .unwrap();
        serde_json::from_value::<ResolutionStateWire>(value)
            .expect("canonical resolution pause")
            .into_game_state()
    };
    game.runner = GameRunner::from_state(state);
}

fn hex_pass_to_pause(game: &mut HexGame) -> Vec<engine::types::events::GameEvent> {
    let mut events = vec![];
    for _ in 0..32 {
        if !matches!(game.runner.state().waiting_for, WaitingFor::Priority { .. }) {
            return events;
        }
        assert!(
            !game.runner.state().stack.is_empty(),
            "required replacement pause must occur before completion"
        );
        events.extend(
            game.runner
                .act(GameAction::PassPriority)
                .expect("ordinary trigger priority")
                .events,
        );
    }
    panic!("Hex trigger did not reach required replacement pause");
}

fn hex_named(game: &mut HexGame, card_name: bool) {
    use engine::types::ability::ChoiceType;
    let WaitingFor::NamedChoice {
        player,
        choice_type,
        source,
        ..
    } = &game.runner.state().waiting_for
    else {
        panic!(
            "required earned attached execute choice: {:?}",
            game.runner.state().waiting_for
        );
    };
    assert_eq!(*player, P0);
    if card_name {
        assert_eq!(*choice_type, ChoiceType::CardName);
    } else {
        assert!(matches!(choice_type, ChoiceType::CreatureType { .. }));
    }
    assert_eq!(
        source
            .as_ref()
            .expect("actual attached choice source")
            .prompt
            .identity
            .reference
            .object_id,
        game.aura
    );
    game.runner
        .act(GameAction::ChooseOption {
            choice: if card_name {
                "Nondefault witness"
            } else {
                "Zombie"
            }
            .into(),
        })
        .expect("nondefault public replacement execute answer");
}

#[test]
fn maddening_selected_host_survives_nested_attached_choices_and_both_codecs_without_redraw() {
    use engine::game::game_object::AttachTarget;
    use engine::types::ability::ChosenAttribute;
    for persisted in [false, true] {
        let mut game = hex_game(4);
        hex_attach_replacement(&mut game, false, false);
        hex_cast(&mut game, 0);
        hex_pin(&mut game, 73);
        let events = hex_pass_to_pause(&mut game);
        let damage = hex_damage(&events, game.aura, P1);
        assert_eq!(game.runner.state().players[1].life, 20 - i32::from(damage));
        let host = game.runner.state().objects[&game.aura].attached_to;
        assert!(
            matches!(host, Some(AttachTarget::Player(player)) if [HEX_P2, HEX_P3].contains(&player))
        );
        assert!(matches!(
            game.runner.state().waiting_for,
            WaitingFor::NamedChoice { .. }
        ));
        game.runner.state_mut().capture_rng_word_pos();
        let consumed = game.runner.state().rng_word_pos;
        hex_restore(&mut game, persisted);
        assert_eq!(game.runner.state().objects[&game.aura].attached_to, host);
        hex_named(&mut game, true);
        // The second actual nested choice must survive independently too.
        assert!(matches!(
            game.runner.state().waiting_for,
            WaitingFor::NamedChoice { .. }
        ));
        hex_restore(&mut game, persisted);
        hex_named(&mut game, false);
        hex_complete(&mut game);
        assert_eq!(game.runner.state().objects[&game.aura].attached_to, host);
        assert!(game.runner.state().objects[&game.aura]
            .chosen_attributes
            .contains(&ChosenAttribute::CreatureType("Zombie".into())));
        game.runner.state_mut().capture_rng_word_pos();
        assert_eq!(
            game.runner.state().rng_word_pos,
            consumed,
            "no die reroll or host redraw after either restored child"
        );
    }
}

#[test]
fn maddening_optional_and_two_replacement_ordering_preserve_selected_player_and_damage() {
    use engine::game::game_object::AttachTarget;
    use engine::types::game_state::ReplacementChoiceKind;
    for (two, accept, persisted) in [
        (false, false, false),
        (false, true, false),
        (true, false, false),
        (true, true, false),
        (false, false, true),
        (false, true, true),
        (true, false, true),
        (true, true, true),
    ] {
        let mut game = hex_game(4);
        hex_attach_replacement(&mut game, true, false);
        if two {
            hex_attach_replacement(&mut game, false, true);
        }
        hex_cast(&mut game, 0);
        hex_pin(&mut game, 73);
        let events = hex_pass_to_pause(&mut game);
        let damage = hex_damage(&events, game.aura, P1);
        assert_eq!(game.runner.state().players[1].life, 20 - i32::from(damage));
        assert_eq!(
            game.runner.state().objects[&game.aura].attached_to,
            Some(AttachTarget::Player(P1)),
            "ordering/optional pause precedes delivery"
        );
        game.runner.state_mut().capture_rng_word_pos();
        let consumed = game.runner.state().rng_word_pos;
        if two {
            let WaitingFor::ReplacementChoice {
                player,
                candidate_count,
                kind,
                candidates,
                ..
            } = &game.runner.state().waiting_for
            else {
                panic!("two actual attached replacements must order");
            };
            assert_eq!(*player, P0);
            assert_eq!(*candidate_count, 2);
            assert_eq!(*kind, ReplacementChoiceKind::Order);
            assert_eq!(candidates.len(), 2);
            assert!(
                candidates[1].description.ends_with("choose a color."),
                "nondefault candidate is the independently named color rider"
            );
            hex_restore(&mut game, persisted);
            game.runner
                .act(GameAction::ChooseReplacement { index: 1 })
                .expect("nondefault ordering choice");
        }
        let WaitingFor::ReplacementChoice {
            player,
            kind,
            candidates,
            ..
        } = &game.runner.state().waiting_for
        else {
            panic!("actual optional Attached offer");
        };
        assert_eq!(*player, P0);
        assert_eq!(*kind, ReplacementChoiceKind::OptionalBranch);
        assert_eq!(candidates[1].description, "Decline");
        hex_restore(&mut game, persisted);
        game.runner
            .act(GameAction::ChooseReplacement {
                index: if accept { 0 } else { 1 },
            })
            .unwrap();
        if two {
            let WaitingFor::NamedChoice { options, .. } = &game.runner.state().waiting_for else {
                panic!("ordered color execute must remain");
            };
            assert!(options
                .iter()
                .any(|option| option.eq_ignore_ascii_case("green")));
            game.runner
                .act(GameAction::ChooseOption {
                    choice: "Green".into(),
                })
                .expect("nondefault color");
        }
        if accept {
            hex_named(&mut game, true);
            hex_named(&mut game, false);
        }
        hex_complete(&mut game);
        assert!(
            matches!(game.runner.state().objects[&game.aura].attached_to, Some(AttachTarget::Player(player)) if [HEX_P2, HEX_P3].contains(&player))
        );
        game.runner.state_mut().capture_rng_word_pos();
        assert_eq!(game.runner.state().rng_word_pos, consumed);
    }
}

#[test]
fn maddening_two_abilities_keep_their_separate_captured_controllers() {
    use engine::game::game_object::AttachTarget;
    let mut game = hex_game(3);
    hex_cast(&mut game, 0);
    {
        let aura = game.runner.state_mut().objects.get_mut(&game.aura).unwrap();
        aura.controller = HEX_P2;
        aura.base_controller = Some(HEX_P2);
    }
    evaluate_layers(game.runner.state_mut());
    reindex_object_triggers(game.runner.state_mut(), game.aura);
    hex_cast(&mut game, 1);
    let newer = hex_one_trigger(&mut game);
    let new_damage = hex_damage(&newer, game.aura, P1);
    assert_eq!(
        game.runner.state().objects[&game.aura].attached_to,
        Some(AttachTarget::Player(P0)),
        "C's captured ability has A as its only other opponent"
    );
    let older = hex_one_trigger(&mut game);
    let old_damage = hex_damage(&older, game.aura, P1);
    assert_eq!(
        game.runner.state().objects[&game.aura].attached_to,
        Some(AttachTarget::Player(HEX_P2)),
        "A's older captured ability still permits C"
    );
    assert_eq!(
        game.runner.state().players[1].life,
        20 - i32::from(new_damage) - i32::from(old_damage)
    );
    hex_complete(&mut game);
}

#[test]
fn maddening_live_protection_elimination_and_enchant_filter_constrain_host_without_losing_damage() {
    use engine::game::game_object::AttachTarget;
    use engine::game::zones::move_to_zone;
    use engine::types::ability::TargetFilter;
    use engine::types::game_state::{GameRng, RandomDraw};
    use engine::types::keywords::Keyword;
    use engine::types::zones::Zone;
    use rand::{seq::IndexedRandom, Rng};
    for guard in [0, 1, 2] {
        let mut game = hex_game(4);
        hex_cast(&mut game, 0);
        let seed = (0..100_000)
            .find(|seed| {
                let mut rng = GameRng::seed_from_u64(*seed);
                let _: u8 = rng.draw(RandomDraw::Outcome).random_range(1..=6);
                [HEX_P2, HEX_P3].choose(rng.draw(RandomDraw::Outcome)) == Some(&HEX_P2)
            })
            .expect("without the live guard this exact seed would select C");
        hex_pin(&mut game, seed);
        match guard {
            0 => {
                move_to_zone(
                    game.runner.state_mut(),
                    game.protection,
                    Zone::Battlefield,
                    &mut vec![],
                );
                evaluate_layers(game.runner.state_mut());
            }
            1 => engine::game::elimination::eliminate_player(
                game.runner.state_mut(),
                HEX_P2,
                &mut vec![],
            ),
            _ => {
                // Explicit synthetic CURRENT restriction: printed Hex supplied
                // the real trigger; this hostile fixture narrows its live
                // enchant permission while still allowing its existing host B.
                let filter = TargetFilter::Or {
                    filters: vec![
                        TargetFilter::SpecificPlayer { id: P1 },
                        TargetFilter::SpecificPlayer { id: HEX_P3 },
                    ],
                };
                let aura = game.runner.state_mut().objects.get_mut(&game.aura).unwrap();
                aura.keywords
                    .retain(|keyword| !matches!(keyword, Keyword::Enchant(_)));
                aura.keywords.push(Keyword::Enchant(filter.clone()));
                let base = &mut aura.base_keywords;
                base.retain(|keyword| !matches!(keyword, Keyword::Enchant(_)));
                base.push(Keyword::Enchant(filter));
                evaluate_layers(game.runner.state_mut());
            }
        }
        let events = hex_one_trigger(&mut game);
        let damage = hex_damage(&events, game.aura, P1);
        assert_eq!(game.runner.state().players[1].life, 20 - i32::from(damage));
        assert_eq!(
            game.runner.state().objects[&game.aura].attached_to,
            Some(AttachTarget::Player(HEX_P3))
        );
        hex_complete(&mut game);
    }
}

#[test]
fn maddening_late_illegal_selected_host_does_not_redraw_or_move_but_earned_rider_finishes() {
    use engine::game::game_object::AttachTarget;
    use engine::types::zones::Zone;
    for (eliminated, persisted) in [(false, false), (false, true), (true, false), (true, true)] {
        let mut game = hex_game(3);
        hex_attach_replacement(&mut game, true, false);
        hex_cast(&mut game, 0);
        let events = hex_pass_to_pause(&mut game);
        let damage = hex_damage(&events, game.aura, P1);
        assert_eq!(game.runner.state().players[1].life, 20 - i32::from(damage));
        let WaitingFor::ReplacementChoice { kind, .. } = &game.runner.state().waiting_for else {
            panic!("earned pre-delivery optional replacement");
        };
        assert_eq!(
            *kind,
            engine::types::game_state::ReplacementChoiceKind::OptionalBranch
        );
        game.runner.state_mut().capture_rng_word_pos();
        let consumed = game.runner.state().rng_word_pos;
        let journal_start = game.runner.state().resolved_rules_journal.entries().len();
        if eliminated {
            engine::game::elimination::eliminate_player(
                game.runner.state_mut(),
                HEX_P2,
                &mut vec![],
            );
        } else {
            engine::game::zones::move_to_zone(
                game.runner.state_mut(),
                game.protection,
                Zone::Battlefield,
                &mut vec![],
            );
        }
        evaluate_layers(game.runner.state_mut());
        hex_restore(&mut game, persisted);
        game.runner
            .act(GameAction::ChooseReplacement { index: 0 })
            .expect("accept already earned independent rider");
        // CR 614.6, CR 609.3, CR 701.3b: impossible host delivery does as much
        // as possible; the already selected independent execute is not cancelled.
        hex_named(&mut game, true);
        hex_named(&mut game, false);
        hex_complete(&mut game);
        assert_eq!(
            game.runner.state().objects[&game.aura].attached_to,
            Some(AttachTarget::Player(P1))
        );
        assert!(game.runner.state().resolved_rules_journal.entries().iter().skip(journal_start).all(|entry| !matches!(entry.command.as_ref(), Some(engine::types::resolved_commands::ResolvedRulesCommand::Attachment(command)) if command.attachment.object_id == game.aura)));
        game.runner.state_mut().capture_rng_word_pos();
        assert_eq!(game.runner.state().rng_word_pos, consumed);
    }
}

#[test]
fn maddening_completed_same_host_does_not_offer_new_attached_replacement_and_retirement_detaches() {
    use engine::game::game_object::AttachTarget;
    let mut game = hex_game(3);
    hex_cast(&mut game, 0);
    hex_cast(&mut game, 1);
    let newer = hex_one_trigger(&mut game);
    let first = hex_damage(&newer, game.aura, P1);
    assert_eq!(
        game.runner.state().objects[&game.aura].attached_to,
        Some(AttachTarget::Player(HEX_P2))
    );
    hex_attach_replacement(&mut game, true, false);
    let older = hex_one_trigger(&mut game);
    let second = hex_damage(&older, game.aura, P1);
    assert_eq!(
        game.runner.state().players[1].life,
        20 - i32::from(first) - i32::from(second)
    );
    assert_eq!(
        game.runner.state().objects[&game.aura].attached_to,
        Some(AttachTarget::Player(HEX_P2))
    );
    assert!(
        matches!(game.runner.state().waiting_for, WaitingFor::Priority { .. }),
        "same host is completed without a new Attached proposal"
    );
    hex_complete(&mut game);
    engine::game::elimination::eliminate_player(game.runner.state_mut(), HEX_P2, &mut vec![]);
    game.runner
        .act(GameAction::PassPriority)
        .expect("ordinary finalization retires illegal player Aura");
    assert_eq!(game.runner.state().objects[&game.aura].attached_to, None);
    assert_eq!(
        game.runner.state().objects[&game.aura].zone,
        engine::types::zones::Zone::Graveyard
    );
    assert_eq!(
        game.runner.state().objects[&game.other_aura].attached_to,
        Some(AttachTarget::Player(P1))
    );
}
