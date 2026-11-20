//! Issue #3861. Exercise Rikku's resolved restriction through the combat prompt,
//! precomputed pair legality, and public blocker declaration.

use engine::ai_support::filter::{FilterPipeline, SimulationFilter};
use engine::ai_support::{ActionMetadata, CandidateAction, TacticalClass};
use engine::game::combat::{
    can_block_pair_with_precomputed, collect_block_restriction_statics,
    collect_blocker_allowed_statics, collect_blocker_restriction_statics, AttackTarget,
};
use engine::game::functioning_abilities::static_kind_present;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{ChoiceType, TargetFilter, TargetRef};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;
use engine::types::format::FormatConfig;
use engine::types::game_state::{StackEntryKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::{parse_mtgjson_mana_cost, ManaColor, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::statics::StaticModeKind;
use engine::types::zones::Zone;

const P2: PlayerId = PlayerId(2);
const RIKKU: &str = "Whenever you put one or more counters on a creature, until end of turn, that creature can't be blocked by creatures your opponents control.\nSteal — {1}, {T}: Move a counter from target creature an opponent controls onto target creature you control. Activate only as a sorcery.";
const BATTLEGROWTH: &str = "Put a +1/+1 counter on target creature.";
const DRESS_DOWN: &str = "Flash\nWhen this enchantment enters, draw a card.\nCreatures lose all abilities.\nAt the beginning of the end step, sacrifice this enchantment.";
const ELVES: &str = "{T}: Add {G}.";
const CLOUDSHIFT: &str = "Exile target creature you control, then return that card to the battlefield under your control.";
const DRAWBRIDGE: &str = "Defender\n{T}: Creatures you control gain haste until end of turn.";
const VINES: &str = "Kicker {G} (You may pay an additional {G} as you cast this spell.)\nTarget creature can't be the target of spells or abilities your opponents control this turn. If this spell was kicked, that creature gets +4/+4 until end of turn.";
const BOLT: &str = "Lightning Bolt deals 3 damage to any target.";
const RAY: &str = "Untap target creature an opponent controls and gain control of it until end of turn. That creature gains haste until end of turn. When you lose control of the creature, tap it.";
const UNSUMMON: &str = "Return target creature to its owner's hand.";
const BLACK_GATE: &str = "As The Black Gate enters, you may pay 3 life. If you don't, it enters tapped.\n{T}: Add {B}.\n{1}{B}, {T}: Choose a player with the most life or tied for most life. Target creature can't be blocked by creatures that player controls this turn.";

struct Board {
    runner: GameRunner,
    rikku: Option<ObjectId>,
    featured: ObjectId,
    alternative: ObjectId,
    blockers: [ObjectId; 3],
    growths: [ObjectId; 3],
}

fn instant(
    scenario: &mut GameScenario,
    player: PlayerId,
    name: &str,
    oracle: &str,
    cost: &str,
) -> ObjectId {
    scenario
        .add_spell_to_hand_from_oracle(player, name, true, oracle)
        .with_mana_cost(parse_mtgjson_mana_cost(cost))
        .id()
}

fn bears(scenario: &mut GameScenario, player: PlayerId) -> ObjectId {
    scenario
        .add_creature(player, "Grizzly Bears", 2, 2)
        .with_mana_cost(parse_mtgjson_mana_cost("{1}{G}"))
        .with_subtypes(vec!["Bear"])
        .id()
}

fn elves(scenario: &mut GameScenario, player: PlayerId) -> ObjectId {
    scenario
        .add_creature(player, "Llanowar Elves", 1, 1)
        .with_mana_cost(parse_mtgjson_mana_cost("{G}"))
        .with_subtypes(vec!["Elf", "Druid"])
        .from_oracle_text(ELVES)
        .id()
}

fn board<T>(
    mut scenario: GameScenario,
    active: PlayerId,
    attacker: PlayerId,
    with_rikku: bool,
    setup: impl FnOnce(&mut GameScenario, ObjectId) -> T,
) -> (Board, T) {
    scenario.at_phase(Phase::PreCombatMain);
    let rikku = with_rikku.then(|| {
        scenario
            .add_creature(P0, "Rikku, Resourceful Guardian", 2, 3)
            .with_mana_cost(parse_mtgjson_mana_cost("{2}{U}"))
            .as_legendary()
            .with_subtypes(vec!["Human", "Artificer"])
            .from_oracle_text_with_keywords(&["Steal"], RIKKU)
            .id()
    });
    let featured = bears(&mut scenario, attacker);
    let alternative = bears(&mut scenario, attacker);
    let blockers = [P0, P1, P2].map(|player| bears(&mut scenario, player));
    let growths = [P0, P1, P2]
        .map(|player| instant(&mut scenario, player, "Battlegrowth", BATTLEGROWTH, "{G}"));
    for player in [P0, P1, P2] {
        for _ in 0..4 {
            scenario
                .add_land_to_library_top(player, "Forest")
                .with_subtypes(vec!["Forest"]);
        }
        scenario.with_mana_pool(
            player,
            [
                ManaType::Green,
                ManaType::Green,
                ManaType::Blue,
                ManaType::Red,
                ManaType::White,
                ManaType::Colorless,
                ManaType::Colorless,
                ManaType::Colorless,
            ]
            .map(|color| ManaUnit::new(color, ObjectId(0), false, vec![]))
            .to_vec(),
        );
    }
    let extra = setup(&mut scenario, featured);
    let mut runner = scenario.build();
    let state = runner.state_mut();
    state.active_player = active;
    state.priority_player = active;
    state.waiting_for = WaitingFor::Priority { player: active };
    (
        Board {
            runner,
            rikku,
            featured,
            alternative,
            blockers,
            growths,
        },
        extra,
    )
}

fn multiplayer(with_rikku: bool) -> Board {
    board(
        GameScenario::new_n_player(3, 42),
        P1,
        P1,
        with_rikku,
        |_, featured| featured,
    )
    .0
}

fn pass(runner: &mut GameRunner) -> Vec<GameEvent> {
    runner
        .act(GameAction::PassPriority)
        .expect("priority pass")
        .events
}

fn priority_of(runner: &mut GameRunner, player: PlayerId) {
    for _ in 0..4 {
        if matches!(runner.state().waiting_for, WaitingFor::Priority { player: who } if who == player)
        {
            return;
        }
        pass(runner);
    }
    panic!("never reached priority for {player:?}");
}

fn complete(runner: &mut GameRunner) {
    runner.advance_until_stack_empty();
    assert!(runner.state().stack.is_empty());
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
}

fn counters(runner: &GameRunner, creature: ObjectId) -> u32 {
    runner.state().objects[&creature]
        .counters
        .get(&CounterType::Plus1Plus1)
        .copied()
        .unwrap_or_default()
}

fn assert_trigger(board: &Board) {
    let trigger = board
        .runner
        .state()
        .stack
        .last()
        .expect("pending Rikku trigger");
    assert_eq!(Some(trigger.source_id), board.rikku);
    assert_eq!(trigger.controller, P0);
    assert!(matches!(
        trigger.kind,
        StackEntryKind::TriggeredAbility { .. }
    ));
    assert_eq!(board.runner.state().stack.len(), 1);
}

fn place_counter(board: &mut Board, caster: PlayerId, recipient: ObjectId, expect_trigger: bool) {
    let runner = &mut board.runner;
    let spell = board.growths[caster.0 as usize];
    priority_of(runner, caster);
    assert_eq!(counters(runner, recipient), 0);
    runner.cast(spell).target_object(recipient).commit();
    // Resolving the spell can replace it with a trigger at the same stack depth.
    for _ in 0..4 {
        if runner.state().objects[&spell].zone == Zone::Graveyard {
            break;
        }
        pass(runner);
    }
    assert_eq!(runner.state().objects[&spell].zone, Zone::Graveyard);
    assert_eq!(counters(runner, recipient), 1);
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    if expect_trigger {
        assert_trigger(board);
    } else {
        assert!(board.runner.state().stack.is_empty());
    }
}

fn grow_featured(board: &mut Board, caster: PlayerId, expect_trigger: bool) {
    place_counter(board, caster, board.featured, expect_trigger);
    complete(&mut board.runner);
}

fn public_block(
    runner: &mut GameRunner,
    blocker: ObjectId,
    attacker: ObjectId,
) -> Result<(), String> {
    runner
        .act(GameAction::DeclareBlockers {
            assignments: vec![(blocker, attacker)],
        })
        .map(|_| ())
        .map_err(|error| error.to_string())
}

fn committed(runner: &GameRunner, blocker: ObjectId, attacker: ObjectId) -> bool {
    runner
        .state()
        .combat
        .as_ref()
        .unwrap()
        .blocker_to_attacker
        .get(&blocker)
        .is_some_and(|attackers| attackers.contains(&attacker))
}

fn at_attack_declaration(runner: &mut GameRunner) {
    for _ in 0..16 {
        if matches!(
            runner.state().waiting_for,
            WaitingFor::DeclareAttackers { .. }
        ) {
            return;
        }
        pass(runner);
    }
    panic!("no attacker prompt: {:?}", runner.state().waiting_for);
}

fn combat_fork(
    board: &Board,
    featured: ObjectId,
    defender: PlayerId,
    accepts: bool,
    at_prompt: impl FnOnce(&GameRunner),
) -> GameRunner {
    let mut runner = GameRunner::from_state(board.runner.state().clone());
    at_attack_declaration(&mut runner);
    let target = AttackTarget::Player(defender);
    runner
        .declare_attackers(&[(featured, target), (board.alternative, target)])
        .unwrap();
    check_block_declaration(runner, board, featured, defender, accepts, at_prompt)
}

fn check_block_declaration(
    mut runner: GameRunner,
    board: &Board,
    featured: ObjectId,
    defender: PlayerId,
    accepts: bool,
    at_prompt: impl FnOnce(&GameRunner),
) -> GameRunner {
    for _ in 0..12 {
        match runner.state().waiting_for {
            WaitingFor::DeclareBlockers { player, .. } if player == defender => break,
            WaitingFor::DeclareBlockers { .. } => {
                runner.declare_blockers(&[]).unwrap();
            }
            WaitingFor::Priority { .. } => {
                pass(&mut runner);
            }
            ref other => panic!("expected blocker prompt, got {other:?}"),
        }
    }
    let blocker = board.blockers[defender.0 as usize];
    let WaitingFor::DeclareBlockers {
        player,
        valid_blocker_ids,
        valid_block_targets,
        ..
    } = &runner.state().waiting_for
    else {
        panic!(
            "expected blocker prompt, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(*player, defender);
    assert!(valid_blocker_ids.contains(&blocker));
    let targets = &valid_block_targets[&blocker];
    assert!(targets.contains(&board.alternative));
    at_prompt(&runner);

    let listed = targets.contains(&featured);
    let state = runner.state();
    let restrictions = collect_block_restriction_statics(state);
    let blocker_restrictions = collect_blocker_restriction_statics(state);
    let allowances = collect_blocker_allowed_statics(state);
    let can_block = |attacker| {
        can_block_pair_with_precomputed(
            state,
            blocker,
            attacker,
            &blocker_restrictions,
            &restrictions,
            &allowances,
            static_kind_present(state, StaticModeKind::CanBlockShadow),
        )
    };
    assert!(can_block(board.alternative));
    let queried = can_block(featured);
    let declared = public_block(&mut runner, blocker, featured);
    assert_eq!(
        (listed, queried, declared.is_ok()),
        (accepts, accepts, accepts),
        "{defender:?}: {declared:?}"
    );

    let blocked_attacker = if accepts {
        featured
    } else {
        let error = declared.unwrap_err();
        assert!(
            error.contains("cannot block") || error.contains("can’t block"),
            "{error}"
        );
        assert!(!committed(&runner, blocker, featured));
        assert!(
            matches!(runner.state().waiting_for, WaitingFor::DeclareBlockers { player, .. } if player == defender)
        );
        public_block(&mut runner, blocker, board.alternative).unwrap();
        board.alternative
    };
    assert!(committed(&runner, blocker, blocked_attacker));
    while matches!(
        runner.state().waiting_for,
        WaitingFor::DeclareBlockers { .. }
    ) {
        runner.declare_blockers(&[]).unwrap();
    }
    assert!(committed(&runner, blocker, blocked_attacker));
    runner
}

fn check(board: &Board, defender: PlayerId, accepts: bool) -> GameRunner {
    combat_fork(board, board.featured, defender, accepts, |_| {})
}

fn split(candidate: &Board, control: &Board) {
    for defender in [P0, P2] {
        check(control, defender, true);
        check(candidate, defender, defender == P0);
    }
}

#[test]
fn black_gate_choice_preserves_the_activators_blocker() {
    let (board, (gate, swamps)) = board(
        GameScenario::new_n_player(3, 42),
        P1,
        P1,
        false,
        |scenario, _| {
            for player in [P0, P1, P2] {
                scenario.with_life(player, 20);
            }
            let gate = scenario
                .add_land_from_oracle(P0, "The Black Gate", BLACK_GATE)
                .as_legendary()
                .with_subtypes(vec!["Gate"])
                .id();
            let swamps = [(); 2].map(|_| scenario.add_basic_land(P0, ManaColor::Black));
            (gate, swamps)
        },
    );
    let mut runner = GameRunner::from_state(board.runner.state().clone());
    at_attack_declaration(&mut runner);
    let target = AttackTarget::Player(P0);
    runner
        .declare_attackers(&[(board.featured, target), (board.alternative, target)])
        .unwrap();
    priority_of(&mut runner, P0);
    for swamp in swamps {
        runner.activate(swamp, 0).resolve();
    }

    // The entry replacement is separate from the two activated abilities.
    runner
        .activate(gate, 1)
        .target_object(board.featured)
        .resolve();
    assert!(runner.state().objects[&gate].tapped);
    let WaitingFor::NamedChoice {
        player,
        choice_type,
        options,
        ..
    } = &runner.state().waiting_for
    else {
        panic!(
            "expected The Black Gate's player choice, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(*player, P0);
    assert_eq!(*choice_type, ChoiceType::player());
    assert_eq!(options, &["0", "1", "2"]);
    runner
        .act(GameAction::ChooseOption {
            choice: P2.0.to_string(),
        })
        .unwrap();
    complete(&mut runner);
    assert!(runner
        .state()
        .transient_continuous_effects
        .iter()
        .any(|effect| {
            effect.source_id == gate
                && effect.affected == (TargetFilter::SpecificObject { id: board.featured })
        }));

    check_block_declaration(runner, &board, board.featured, P0, true, |_| {});
}

#[test]
fn v1_creator_relative_blockers() {
    let mut candidate = multiplayer(true);
    let mut control = multiplayer(false);
    grow_featured(&mut candidate, P0, true);
    grow_featured(&mut control, P0, false);
    split(&candidate, &control);
}

#[test]
fn v2_counter_actor() {
    let mut candidate = multiplayer(true);
    let mut control = multiplayer(true);
    grow_featured(&mut candidate, P0, true);
    grow_featured(&mut control, P1, false);
    split(&candidate, &control);
}

#[test]
fn rikku_distinguishes_equally_grown_attackers_in_blocker_simulation() {
    let mut board = multiplayer(true);
    grow_featured(&mut board, P0, true);
    let alternative = board.alternative;
    place_counter(&mut board, P1, alternative, false);
    complete(&mut board.runner);

    combat_fork(&board, board.featured, P2, false, |runner| {
        let state = runner.state();
        let blocker = board.blockers[P2.0 as usize];
        let candidate = |attacker| CandidateAction {
            action: GameAction::DeclareBlockers {
                assignments: vec![(blocker, attacker)],
            },
            metadata: ActionMetadata::for_actor(Some(P2), TacticalClass::Block),
        };
        let restricted = candidate(board.featured);
        let legal = candidate(alternative);
        let pipeline = FilterPipeline::new(vec![Box::new(SimulationFilter)]);

        // CR 109.5 + CR 509.1b: P2 is an opponent of the trigger's
        // controller, so only the Bears grown by P1 can be blocked.
        assert!(!pipeline.accepts(state, &restricted));
        assert!(pipeline.accepts(state, &legal));

        for candidates in [
            [restricted.clone(), legal.clone()],
            [legal.clone(), restricted],
        ] {
            let expected: Vec<_> = candidates
                .iter()
                .filter(|candidate| pipeline.accepts(state, candidate))
                .map(|candidate| candidate.action.clone())
                .collect();
            let actual: Vec<_> = pipeline
                .apply(state, candidates)
                .into_iter()
                .map(|candidate| candidate.action)
                .collect();
            assert_eq!(actual, expected);
        }
    });
}

#[test]
fn v3_dress_down() {
    let prepare = |caster, trigger| {
        let (mut board, (elves, dress_down)) = board(
            GameScenario::new_n_player(3, 42),
            P1,
            P1,
            true,
            |scenario, _| {
                let elves = elves(scenario, P1);
                let dress_down = scenario
                    .add_enchantment_to_hand_from_oracle(P0, "Dress Down", DRESS_DOWN)
                    .with_mana_cost(parse_mtgjson_mana_cost("{1}{U}"))
                    .id();
                (elves, dress_down)
            },
        );
        board.featured = elves;
        grow_featured(&mut board, caster, trigger);
        let runner = &mut board.runner;
        let drawn = runner.state().players[0].library[0];
        let rikku = board.rikku.unwrap();
        assert!(!runner.state().objects[&rikku].abilities.is_empty());
        assert!(!runner.state().objects[&board.featured].abilities.is_empty());
        priority_of(runner, P0);
        runner.cast(dress_down).commit();
        complete(runner);
        assert_eq!(runner.state().objects[&dress_down].zone, Zone::Battlefield);
        assert_eq!(runner.state().objects[&drawn].zone, Zone::Hand);
        let rikku = &runner.state().objects[&rikku];
        assert!(rikku.abilities.is_empty());
        assert!(rikku.trigger_definitions.is_empty());
        assert!(runner.state().objects[&board.featured].abilities.is_empty());
        board
    };
    split(&prepare(P0, true), &prepare(P1, false));
}

// CR 514.2: Watch the cleanup turn transition before testing the next combat.
fn next_p1_combat(board: &mut Board) {
    let runner = &mut board.runner;
    let turn = runner.state().turn_number;
    let mut cleanup = false;
    for _ in 0..160 {
        let state = runner.state();
        if state.active_player == P1
            && state.turn_number > turn
            && matches!(
                state.waiting_for,
                WaitingFor::DeclareAttackers { player: P1, .. }
            )
        {
            assert!(cleanup);
            return;
        }
        let events = match state.waiting_for {
            WaitingFor::DeclareAttackers { .. } => runner.declare_attackers(&[]).unwrap().events,
            WaitingFor::Priority { .. } => pass(runner),
            ref other => panic!("unexpected turn progression prompt: {other:?}"),
        };
        cleanup |= events.iter().any(|event| {
            matches!(
                event,
                GameEvent::PhaseChanged {
                    phase: Phase::Cleanup
                }
            )
        });
    }
    panic!("did not reach P1's next combat");
}

#[test]
fn v4_cleanup_ends_the_restriction() {
    let mut candidate = multiplayer(true);
    let mut control = multiplayer(true);
    grow_featured(&mut candidate, P0, true);
    grow_featured(&mut control, P1, false);
    check(&control, P2, true);
    check(&candidate, P2, false);
    for board in [&mut candidate, &mut control] {
        let pin = board.runner.state().objects[&board.featured].incarnation;
        next_p1_combat(board);
        assert_eq!(
            board.runner.state().objects[&board.featured].incarnation,
            pin
        );
        assert_eq!(counters(&board.runner, board.featured), 1);
        for defender in [P0, P2] {
            check(board, defender, true);
        }
    }
}

#[test]
fn v5_existing_blocks_persist() {
    let prepare = || {
        board(
            GameScenario::new_n_player(3, 42),
            P1,
            P1,
            true,
            |scenario, _| scenario.add_basic_land(P0, ManaColor::Green),
        )
    };
    let (board, forest) = prepare();
    let (mut early, _) = prepare();
    grow_featured(&mut early, P0, true);
    for defender in [P0, P2] {
        let (mut late, _) = prepare();
        late.runner = check(&board, defender, true);
        priority_of(&mut late.runner, P0);
        let (_, _, by_source) = engine::ai_support::legal_actions_full(late.runner.state());
        let tap = by_source[&forest]
            .iter()
            .find(|action| matches!(action, GameAction::TapLandForMana { .. }))
            .expect("Forest can produce green mana after blockers")
            .clone();
        late.runner.act(tap).unwrap();
        assert!(late.runner.state().objects[&forest].tapped);
        grow_featured(&mut late, P0, true);
        // CR 509.1h: The committed pair and the attacker's blocked status persist.
        assert!(committed(
            &late.runner,
            late.blockers[defender.0 as usize],
            late.featured
        ));
        assert!(late
            .runner
            .state()
            .combat
            .as_ref()
            .unwrap()
            .attackers
            .iter()
            .any(|attacker| attacker.object_id == late.featured && attacker.blocked));
        check(&early, defender, defender == P0);
    }
}

#[test]
fn v6_steal_places_the_counter() {
    let prepare = || {
        board(
            GameScenario::new_n_player(3, 42),
            P0,
            P0,
            true,
            |scenario, _| {
                let donor = bears(scenario, P1);
                scenario.with_counter(donor, CounterType::Plus1Plus1, 1);
                donor
            },
        )
    };
    let (mut candidate, donor) = prepare();
    let (control, _) = prepare();
    let rikku = candidate.rikku.unwrap();
    let featured = candidate.featured;
    let pin = candidate.runner.state().objects[&featured].incarnation;
    let runner = &mut candidate.runner;
    assert_eq!(counters(runner, donor), 1);
    assert_eq!(counters(runner, featured), 0);
    let mana_before = runner.state().players[0].mana_pool.total();
    runner
        .act(GameAction::ActivateAbility {
            source_id: rikku,
            ability_index: 0,
            payment_mode: Default::default(),
        })
        .unwrap();
    for target in [donor, featured] {
        let WaitingFor::TargetSelection {
            player: P0,
            target_slots,
            selection,
            ..
        } = &runner.state().waiting_for
        else {
            panic!("expected Steal's target selection");
        };
        assert!(target_slots[selection.current_slot]
            .legal_targets
            .contains(&TargetRef::Object(target)));
        runner
            .act(GameAction::ChooseTarget {
                target: Some(TargetRef::Object(target)),
            })
            .unwrap();
    }
    if matches!(runner.state().waiting_for, WaitingFor::ManaPayment { .. }) {
        pass(runner);
    }
    assert!(runner.state().objects[&rikku].tapped);
    assert_eq!(runner.state().players[0].mana_pool.total(), mana_before - 1);
    assert!(matches!(
        runner.state().stack.last().unwrap().kind,
        StackEntryKind::ActivatedAbility { .. }
    ));
    for _ in 0..4 {
        if counters(runner, featured) == 1 {
            break;
        }
        pass(runner);
    }
    // CR 122.5: Steal removes and places the counter in a single move.
    assert_eq!(counters(runner, donor), 0);
    assert_eq!(counters(runner, featured), 1);
    assert_eq!(runner.state().objects[&featured].incarnation, pin);
    assert_trigger(&candidate);
    complete(&mut candidate.runner);
    for defender in [P1, P2] {
        check(&control, defender, true);
        check(&candidate, defender, false);
    }
}

#[test]
fn v7_blink_creates_a_new_recipient() {
    let prepare = |caster, trigger| {
        let (mut board, (cloudshift, elves, bridge)) = board(
            GameScenario::new_n_player(3, 42),
            P1,
            P1,
            true,
            |scenario, _| {
                let elves = elves(scenario, P1);
                let bridge = scenario
                    .add_creature(P1, "Crashing Drawbridge", 0, 4)
                    .as_artifact_creature()
                    .with_subtypes(vec!["Wall"])
                    .with_mana_cost(parse_mtgjson_mana_cost("{2}"))
                    .from_oracle_text(DRAWBRIDGE)
                    .id();
                let cloudshift = instant(scenario, P1, "Cloudshift", CLOUDSHIFT, "{W}");
                (cloudshift, elves, bridge)
            },
        );
        grow_featured(&mut board, caster, trigger);
        (board, cloudshift, elves, bridge)
    };
    let (mut candidate, cloudshift, elves, bridge) = prepare(P0, true);
    let (mut control, control_cloudshift, control_elves, control_bridge) = prepare(P1, false);
    check(&control, P2, true);
    check(&candidate, P2, false);
    for (board, spell, elves, bridge) in [
        (&mut candidate, cloudshift, elves, bridge),
        (
            &mut control,
            control_cloudshift,
            control_elves,
            control_bridge,
        ),
    ] {
        let creature = board.featured;
        let runner = &mut board.runner;
        let turn = runner.state().turn_number;
        let pin = runner.state().objects[&creature].incarnation;
        priority_of(runner, P1);
        let outcome = runner.cast(spell).target_object(creature).resolve();
        // CR 400.7: The return creates a new battlefield incarnation.
        for (from, to) in [
            (Zone::Battlefield, Zone::Exile),
            (Zone::Exile, Zone::Battlefield),
        ] {
            assert!(outcome.events().iter().any(|event| matches!(event,
                GameEvent::ZoneChanged { object_id, from: Some(origin), to: destination, .. }
                    if *object_id == creature && *origin == from && *destination == to
            )));
        }
        assert_eq!(runner.state().objects[&creature].zone, Zone::Battlefield);
        assert_ne!(runner.state().objects[&creature].incarnation, pin);
        assert_eq!(runner.state().turn_number, turn);
        assert_eq!(counters(runner, creature), 0);
        complete(runner);
        for defender in [P0, P2] {
            combat_fork(board, elves, defender, true, |_| {});
        }
        let runner = &mut board.runner;
        priority_of(runner, P1);
        runner.activate(bridge, 0).resolve();
        complete(runner);
        assert!(runner.state().objects[&bridge].tapped);
        assert!(runner.state().objects[&creature].has_keyword(&Keyword::Haste));
        assert_eq!(runner.state().turn_number, turn);
        for defender in [P0, P2] {
            check(board, defender, true);
        }
    }
}

#[test]
fn v8_targeting_and_blocking_are_independent() {
    let prepare = |caster, trigger, cast_vines| {
        let (mut board, (vines, bolts)) = board(
            GameScenario::new_n_player(3, 42),
            P1,
            P1,
            true,
            |scenario, _| {
                let bolts =
                    [P0, P2].map(|player| instant(scenario, player, "Lightning Bolt", BOLT, "{R}"));
                let vines = instant(scenario, P0, "Vines of Vastwood", VINES, "{G}");
                (vines, bolts)
            },
        );
        grow_featured(&mut board, caster, trigger);
        if cast_vines {
            priority_of(&mut board.runner, P0);
            let green_before = board.runner.state().players[0]
                .mana_pool
                .count_color(ManaType::Green);
            board
                .runner
                .cast(vines)
                .target_object(board.featured)
                .commit();
            complete(&mut board.runner);
            assert_eq!(board.runner.state().objects[&vines].zone, Zone::Graveyard);
            assert_eq!(
                board.runner.state().players[0]
                    .mana_pool
                    .count_color(ManaType::Green),
                green_before - 1
            );
        }
        (board, bolts)
    };

    let (candidate, bolts) = prepare(P0, true, true);
    let (no_vines, no_vines_bolts) = prepare(P0, true, false);
    let (control, _) = prepare(P1, false, true);
    for (board, bolts, vines_active) in [
        (&candidate, bolts, true),
        (&no_vines, no_vines_bolts, false),
    ] {
        for (caster, bolt) in [P0, P2].into_iter().zip(bolts) {
            let mut runner = GameRunner::from_state(board.runner.state().clone());
            priority_of(&mut runner, caster);
            runner
                .act(GameAction::CastSpell {
                    object_id: bolt,
                    card_id: runner.state().objects[&bolt].card_id,
                    targets: vec![],
                    payment_mode: Default::default(),
                })
                .unwrap();
            let WaitingFor::TargetSelection {
                player,
                target_slots,
                ..
            } = &runner.state().waiting_for
            else {
                panic!("expected Bolt's target selection");
            };
            assert_eq!(*player, caster);
            assert!(target_slots[0]
                .legal_targets
                .contains(&TargetRef::Object(board.alternative)));
            assert_eq!(
                target_slots[0]
                    .legal_targets
                    .contains(&TargetRef::Object(board.featured)),
                caster == P0 || !vines_active
            );
        }
    }
    split(&candidate, &control);
}

#[test]
fn v9_two_headed_giant_opponent_topology() {
    for (active, attacker, defender, accepts) in [(P2, P2, P1, true), (P0, P1, P2, false)] {
        let prepare = || {
            board(
                GameScenario::new_with_format(FormatConfig::two_headed_giant(), 4, 42),
                active,
                attacker,
                true,
                |_, featured| featured,
            )
            .0
        };
        let mut candidate = prepare();
        let mut control = prepare();
        grow_featured(&mut candidate, P0, true);
        grow_featured(&mut control, P2, false);
        check(&control, defender, true);
        check(&candidate, defender, accepts);
    }
}

#[test]
fn v10_trigger_retains_its_controller() {
    let prepare = |caster, trigger| {
        let (mut board, ray) = board(
            GameScenario::new_n_player(3, 42),
            P1,
            P1,
            true,
            |scenario, _| instant(scenario, P2, "Ray of Command", RAY, "{3}{U}"),
        );
        let rikku = board.rikku.unwrap();
        let pin = board.runner.state().objects[&rikku].incarnation;
        let featured = board.featured;
        place_counter(&mut board, caster, featured, trigger);
        priority_of(&mut board.runner, P2);
        board.runner.cast(ray).target_object(rikku).commit();
        board.runner.resolve_top();
        assert_eq!(board.runner.state().objects[&ray].zone, Zone::Graveyard);
        let source = &board.runner.state().objects[&rikku];
        assert_eq!(
            (source.zone, source.controller, source.incarnation),
            (Zone::Battlefield, P2, pin)
        );
        if trigger {
            assert_trigger(&board);
        }
        complete(&mut board.runner);
        board
    };
    let candidate = prepare(P0, true);
    let control = prepare(P1, false);
    for defender in [P0, P2] {
        for (board, accepts) in [(&control, true), (&candidate, defender == P0)] {
            combat_fork(board, board.featured, defender, accepts, |runner| {
                assert_eq!(runner.state().objects[&board.rikku.unwrap()].controller, P2);
            });
        }
    }
}

#[test]
fn v11_resolved_restriction_survives_source_departure() {
    let prepare = |caster, trigger| {
        let (mut board, unsummon) = board(
            GameScenario::new_n_player(3, 42),
            P1,
            P1,
            true,
            |scenario, _| instant(scenario, P0, "Unsummon", UNSUMMON, "{U}"),
        );
        let rikku = board.rikku.unwrap();
        let pin = board.runner.state().objects[&board.featured].incarnation;
        grow_featured(&mut board, caster, trigger);
        priority_of(&mut board.runner, P0);
        board.runner.cast(unsummon).target_object(rikku).commit();
        complete(&mut board.runner);
        assert_eq!(board.runner.state().objects[&rikku].zone, Zone::Hand);
        assert!(board.runner.state().players[0].hand.contains(&rikku));
        assert!(!board.runner.state().battlefield.contains(&rikku));
        assert_eq!(
            board.runner.state().objects[&board.featured].incarnation,
            pin
        );
        assert_eq!(counters(&board.runner, board.featured), 1);
        board
    };
    split(&prepare(P0, true), &prepare(P1, false));
}
